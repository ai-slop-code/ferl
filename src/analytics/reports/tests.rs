use std::{collections::BTreeMap, net::IpAddr, time::Duration};

use axum::http::{Method, StatusCode, header};
use http::{HeaderMap, HeaderValue};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    account_support::{call, json as response_json, migrated_database, request, seed_account},
    analytics::{VisitMethod, event::VisitEvent, report_repository},
    app::{self, AppState},
    client_ip::CanonicalIp,
    config::Config,
    domain::{
        ids::VisitId,
        time::{TimeInterval, Timestamp},
    },
    http::query::GroupBy,
    links::{ActiveLinkSnapshot, resolve_active_link},
    pagination::{breakdown_boundary::BreakdownBoundary, visit_boundary::VisitBoundary},
};

fn production_state(database_url: &str, max_pool: usize) -> AppState {
    let values = BTreeMap::from([
        ("FERL_DATABASE_URL".to_owned(), database_url.to_owned()),
        (
            "FERL_PUBLIC_BASE_URL".to_owned(),
            "https://reports.example.test".to_owned(),
        ),
        (
            "FERL_CURSOR_KEY".to_owned(),
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".to_owned(),
        ),
        ("FERL_DB_POOL_MAX".to_owned(), max_pool.to_string()),
        ("FERL_DB_ACQUIRE_TIMEOUT_SECS".to_owned(), "5".to_owned()),
        ("FERL_DB_LOCK_TIMEOUT_SECS".to_owned(), "2".to_owned()),
    ]);
    let config = Config::from_map(values).expect("report production settings are valid");
    AppState::from_config(&config).expect("production state composes from report settings")
}

async fn create_link(router: &axum::Router, key: &str, code: &str) -> Value {
    let response = call(
        router.clone(),
        request(
            Method::POST,
            "/api/v1/links",
            Some(key),
            &format!(
                r#"{{"destination_url":"https://history.example.test/{code}?version=initial#section","short_code":"{code}"}}"#
            ),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    response_json(response).await
}

fn test_headers(referrer: Option<&str>, user_agent: Option<&str>) -> HeaderMap {
    let mut headers = HeaderMap::new();
    if let Some(referrer) = referrer {
        headers.insert(header::REFERER, HeaderValue::from_str(referrer).unwrap());
    }
    if let Some(user_agent) = user_agent {
        headers.insert(
            header::USER_AGENT,
            HeaderValue::from_str(user_agent).unwrap(),
        );
    }
    headers
}

fn enqueue_event(
    state: &AppState,
    snapshot: &ActiveLinkSnapshot,
    id: u128,
    at: &str,
    method: VisitMethod,
    ip: &str,
    headers: HeaderMap,
) {
    let event = VisitEvent::for_test(
        snapshot,
        Uuid::from_u128(id),
        Timestamp::parse_rfc3339(at).expect("fixture has a valid occurrence instant"),
        method,
        CanonicalIp::new(ip.parse::<IpAddr>().expect("fixture IP parses")),
        &headers,
    );
    state.analytics.try_enqueue(event);
}

async fn report(router: &axum::Router, key: &str, path: &str) -> Value {
    let response = call(router.clone(), request(Method::GET, path, Some(key), "")).await;
    assert_eq!(response.status(), StatusCode::OK, "{path}");
    response_json(response).await
}

async fn report_response(
    router: &axum::Router,
    method: Method,
    key: Option<&str>,
    path: &str,
) -> (StatusCode, Value) {
    let response = call(router.clone(), request(method, path, key, "")).await;
    let status = response.status();
    (status, response_json(response).await)
}

async fn wait_for_visits(pool: &sqlx::PgPool, code: &str, expected: i64) {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let count: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM visits v JOIN links l ON l.id=v.link_id WHERE l.short_code=$1",
            )
            .bind(code)
            .fetch_one(pool)
            .await
            .unwrap();
            if count == expected {
                return;
            }
            assert!(count < expected, "worker persistence did not duplicate the fixture");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("visit persistence is observed before its deadline");
}

async fn wait_for_worker_lock_wait(pool: &sqlx::PgPool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let waiting: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_stat_activity \
                 WHERE datname=current_database() AND wait_event_type='Lock' \
                   AND query LIKE '%SELECT short_code FROM links WHERE id = $1 FOR UPDATE%')",
            )
            .fetch_one(pool)
            .await
            .unwrap();
            if waiting {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("worker lock wait is observed before releasing the barrier");
}

#[tokio::test]
async fn deterministic_worker_fixtures_cover_raw_keysets_summaries_and_all_breakdowns() {
    let database = migrated_database().await;
    database
        .verify(|database| {
            Box::pin(async move {
                let (_, key) = seed_account(database.pool(), "report_golden_owner", "user", true).await;
                let (admin_id, admin_key) =
                    seed_account(database.pool(), "report_golden_admin", "admin", true).await;
                let (_, other_admin_key) =
                    seed_account(database.pool(), "report_golden_other_admin", "admin", true).await;
                let state = production_state(database.schema_database_url(), 1);
                state.verify_schema().await.expect("production verifies schema v2");
                let router = app::router(state.clone());
                create_link(&router, &key, "report-golden").await;
                let snapshot = resolve_active_link(
                    &state.pool,
                    "report-golden",
                    Duration::from_secs(2),
                )
                .await
                .expect("the test link resolves")
                .expect("test link is active");

                let chrome = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/120.0.0.0 Safari/537.36";
                let iphone = "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Mobile/15E148 Safari/604.1";
                let workers = state.start_analytics_workers().expect("worker set starts once");
                enqueue_event(
                    &state,
                    &snapshot,
                    1,
                    "2024-12-31T23:59:59.999999Z",
                    VisitMethod::Get,
                    "192.0.2.9",
                    test_headers(None, None),
                );
                enqueue_event(
                    &state,
                    &snapshot,
                    2,
                    "2025-01-01T00:00:00Z",
                    VisitMethod::Head,
                    "192.0.2.10",
                    test_headers(Some(""), Some(chrome)),
                );
                enqueue_event(
                    &state,
                    &snapshot,
                    3,
                    "2024-02-29T23:59:59.999999Z",
                    VisitMethod::Get,
                    "2001:db8::1",
                    test_headers(Some("https://ref.test/a"), Some(iphone)),
                );
                enqueue_event(
                    &state,
                    &snapshot,
                    4,
                    "2024-02-29T23:59:59.999999Z",
                    VisitMethod::Get,
                    "2001:db8::2",
                    test_headers(Some("https://ref.test/a"), Some(iphone)),
                );
                enqueue_event(
                    &state,
                    &snapshot,
                    5,
                    "2024-03-01T00:00:00.000001Z",
                    VisitMethod::Get,
                    "192.0.2.10",
                    test_headers(Some("https://ref.test/z"), Some(chrome)),
                );
                enqueue_event(
                    &state,
                    &snapshot,
                    6,
                    "2024-03-01T00:00:00Z",
                    VisitMethod::Get,
                    "::ffff:192.0.2.10",
                    test_headers(None, Some("")),
                );
                enqueue_event(
                    &state,
                    &snapshot,
                    7,
                    "0000-01-01T00:00:00Z",
                    VisitMethod::Get,
                    "192.0.2.9",
                    test_headers(None, None),
                );
                assert_eq!(
                    workers.shutdown(Duration::from_secs(8)).await,
                    crate::analytics::WorkerStopOutcome::Drained
                );

                let all_stats = report(
                    &router,
                    &key,
                    "/api/v1/links/report-golden/stats",
                )
                .await;
                assert_eq!(all_stats, json!({"lifetime_visits":7,"matching_visits":7}));

                let intervals = [
                    (
                        "/api/v1/links/report-golden/stats?from=2024-02-29T23%3A59%3A59.999999Z",
                        6,
                    ),
                    (
                        "/api/v1/links/report-golden/stats?to=2024-03-01T00%3A00%3A00.000001Z",
                        4,
                    ),
                    (
                        "/api/v1/links/report-golden/stats?from=2024-02-29T23%3A59%3A59.999999Z&to=2024-03-01T00%3A00%3A00Z",
                        2,
                    ),
                    (
                        "/api/v1/links/report-golden/stats?from=2030-01-01T00%3A00%3A00Z",
                        0,
                    ),
                ];
                for (path, expected) in intervals {
                    assert_eq!(
                        report(&router, &key, path).await,
                        json!({"lifetime_visits":7,"matching_visits":expected}),
                        "{path}"
                    );
                }

                let mut cursor = None;
                let mut ids = Vec::new();
                let mut occurrence_times = Vec::new();
                let mut snapshots = Vec::new();
                for new_limit in [1, 2, 1, 2, 1] {
                    let path = format!(
                        "/api/v1/links/report-golden/visits?limit={new_limit}{}",
                        cursor
                            .as_deref()
                            .map(|value| format!("&cursor={value}"))
                            .unwrap_or_default()
                    );
                    let page = report(&router, &key, &path).await;
                    for row in page["items"].as_array().unwrap() {
                        ids.push(row["id"].as_str().unwrap().to_owned());
                        occurrence_times.push(row["occurred_at"].as_str().unwrap().to_owned());
                        snapshots.push(row["redirect_url"].as_str().unwrap().to_owned());
                    }
                    cursor = page["next_cursor"].as_str().map(str::to_owned);
                    if cursor.is_none() {
                        break;
                    }
                }
                assert_eq!(
                    ids,
                    [2_u128, 1, 5, 6, 4, 3, 7]
                        .into_iter()
                        .map(|id| Uuid::from_u128(id).to_string())
                        .collect::<Vec<_>>(),
                    "occurred_at DESC then UUID DESC traverses ties once across page sizes"
                );
                assert_eq!(occurrence_times.last().unwrap(), "0000-01-01T00:00:00.000000Z");
                assert!(snapshots.iter().all(|url| url == "https://history.example.test/report-golden?version=initial#section"));

                let expected = [
                    (
                        "day",
                        json!([
                            {"value":"0000-01-01","visits":1},
                            {"value":"2024-02-29","visits":2},
                            {"value":"2024-03-01","visits":2},
                            {"value":"2024-12-31","visits":1},
                            {"value":"2025-01-01","visits":1}
                        ]),
                    ),
                    ("method", json!([{"value":"GET","visits":6},{"value":"HEAD","visits":1}])),
                    ("referrer", json!([
                        {"value":null,"visits":3},
                        {"value":"","visits":1},
                        {"value":"https://ref.test/a","visits":2},
                        {"value":"https://ref.test/z","visits":1}
                    ])),
                    ("client_ip", json!([
                        {"value":"192.0.2.10","visits":3},
                        {"value":"192.0.2.9","visits":2},
                        {"value":"2001:db8::1","visits":1},
                        {"value":"2001:db8::2","visits":1}
                    ])),
                    ("user_agent", json!([
                        {"value":null,"visits":2},
                        {"value":"","visits":1},
                        {"value":chrome,"visits":2},
                        {"value":iphone,"visits":2}
                    ])),
                    ("device", json!([
                        {"value":"desktop","visits":2},
                        {"value":"mobile","visits":2},
                        {"value":"unknown","visits":3}
                    ])),
                    ("browser", json!([
                        {"value":"Chrome","visits":2},
                        {"value":"Safari","visits":2},
                        {"value":"unknown","visits":3}
                    ])),
                    ("os", json!([
                        {"value":"Windows","visits":2},
                        {"value":"iOS","visits":2},
                        {"value":"unknown","visits":3}
                    ])),
                ];
                for (group, golden) in expected {
                    let first = report(
                        &router,
                        &key,
                        &format!("/api/v1/links/report-golden/stats/breakdown?group_by={group}&limit=2"),
                    )
                    .await;
                    let mut actual = first["items"].as_array().unwrap().clone();
                    let mut cursor = first["next_cursor"].as_str().map(str::to_owned);
                    while let Some(value) = cursor.take() {
                        let page = report(
                            &router,
                            &key,
                            &format!("/api/v1/links/report-golden/stats/breakdown?group_by={group}&limit=1&cursor={value}"),
                        )
                        .await;
                        actual.extend(page["items"].as_array().unwrap().iter().cloned());
                        cursor = page["next_cursor"].as_str().map(str::to_owned);
                    }
                    assert_eq!(Value::Array(actual), golden, "complete groups/order for {group}");
                }

                let mut timezone_transaction = database.pool().begin().await.unwrap();
                sqlx::query("SET LOCAL TIME ZONE 'Pacific/Auckland'")
                    .execute(&mut *timezone_transaction)
                    .await
                    .unwrap();
                let timezone_days = crate::analytics::report_query::breakdown(
                    snapshot.link_id().as_uuid(),
                    TimeInterval::new(None, None).unwrap(),
                    GroupBy::Day,
                    None,
                    200,
                    false,
                )
                .build_query_as::<crate::analytics::report_repository::BreakdownRecord>()
                .fetch_all(&mut *timezone_transaction)
                .await
                .unwrap();
                assert_eq!(
                    timezone_days.iter().map(|row| row.value.as_deref().unwrap()).collect::<Vec<_>>(),
                    ["0000-01-01", "2024-02-29", "2024-03-01", "2024-12-31", "2025-01-01"],
                    "UTC day grouping does not depend on the PostgreSQL session timezone"
                );
                timezone_transaction.commit().await.unwrap();

                let after_null = report(
                    &router,
                    &key,
                    "/api/v1/links/report-golden/stats/breakdown?group_by=referrer&limit=1",
                )
                .await;
                assert_eq!(after_null["items"][0]["value"], Value::Null);
                let cursor = after_null["next_cursor"].as_str().unwrap();
                let continued = report(
                    &router,
                    &key,
                    &format!("/api/v1/links/report-golden/stats/breakdown?group_by=referrer&limit=1&cursor={cursor}"),
                )
                .await;
                assert_eq!(continued["items"][0]["value"], "");

                let interval_page = report(
                    &router,
                    &key,
                    "/api/v1/links/report-golden/visits?from=2024-01-01T00%3A00%3A00Z&limit=1",
                )
                .await;
                let interval_cursor = interval_page["next_cursor"].as_str().unwrap().to_owned();
                let equivalent_offset = call(
                    router.clone(),
                    request(
                        Method::GET,
                        &format!("/api/v1/links/report-golden/visits?from=2024-01-01T02%3A00%3A00%2B02%3A00&limit=2&cursor={interval_cursor}"),
                        Some(&key),
                        "",
                    ),
                )
                .await;
                assert_eq!(equivalent_offset.status(), StatusCode::OK);
                let equivalent_offset = response_json(equivalent_offset).await;
                assert_eq!(equivalent_offset["items"].as_array().unwrap().len(), 2);

                for path in [
                    format!("/api/v1/links/report-golden/visits?from=2024-01-02T00%3A00%3A00Z&cursor={interval_cursor}"),
                    format!("/api/v1/links/report-golden/stats/breakdown?group_by=day&cursor={interval_cursor}"),
                    format!("/api/v1/links/report-golden/visits?from=2024-01-01T00%3A00%3A00Z&cursor={}x", interval_cursor),
                ] {
                    let invalid = call(
                        router.clone(),
                        request(Method::GET, &path, Some(&key), ""),
                    )
                    .await;
                    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST, "{path}");
                    assert_eq!(response_json(invalid).await["error"]["code"], "invalid_request");
                }

                let admin_cursor_page = report(
                    &router,
                    &admin_key,
                    "/api/v1/links/report-golden/visits?limit=1",
                )
                .await;
                let admin_cursor = admin_cursor_page["next_cursor"].as_str().unwrap().to_owned();
                let wrong_actor = call(
                    router.clone(),
                    request(
                        Method::GET,
                        &format!("/api/v1/links/report-golden/visits?cursor={admin_cursor}"),
                        Some(&key),
                        "",
                    ),
                )
                .await;
                assert_eq!(wrong_actor.status(), StatusCode::BAD_REQUEST);
                let _ = response_json(wrong_actor).await;

                create_link(&router, &key, "report-golden-two").await;
                let wrong_target = call(
                    router.clone(),
                    request(
                        Method::GET,
                        &format!("/api/v1/links/report-golden-two/visits?cursor={interval_cursor}"),
                        Some(&key),
                        "",
                    ),
                )
                .await;
                assert_eq!(wrong_target.status(), StatusCode::BAD_REQUEST);
                let _ = response_json(wrong_target).await;

                let method_cursor_page = report(
                    &router,
                    &key,
                    "/api/v1/links/report-golden/stats/breakdown?group_by=method&limit=1",
                )
                .await;
                let method_cursor = method_cursor_page["next_cursor"].as_str().unwrap();
                let wrong_group = call(
                    router.clone(),
                    request(
                        Method::GET,
                        &format!("/api/v1/links/report-golden/stats/breakdown?group_by=os&cursor={method_cursor}"),
                        Some(&key),
                        "",
                    ),
                )
                .await;
                assert_eq!(wrong_group.status(), StatusCode::BAD_REQUEST);
                let _ = response_json(wrong_group).await;

                // A deleted username can be recreated with a new UUID, but its new
                // admin principal cannot inherit the prior administrator's cursor.
                let deleted_admin = call(
                    router.clone(),
                    request(
                        Method::DELETE,
                        &format!("/api/v1/admin/accounts/{}", admin_id.as_uuid()),
                        Some(&other_admin_key),
                        "",
                    ),
                )
                .await;
                assert_eq!(deleted_admin.status(), StatusCode::NO_CONTENT);
                let recreated = call(
                    router.clone(),
                    request(
                        Method::POST,
                        "/api/v1/admin/accounts",
                        Some(&other_admin_key),
                        r#"{"username":"report_golden_admin","role":"admin"}"#,
                    ),
                )
                .await;
                assert_eq!(recreated.status(), StatusCode::CREATED);
                let recreated = response_json(recreated).await;
                let new_admin_id = recreated["account"]["id"].as_str().unwrap();
                let new_admin_key = recreated["api_key"].as_str().unwrap();
                assert_ne!(new_admin_id, admin_id.as_uuid().to_string());
                let inherited = call(
                    router.clone(),
                    request(
                        Method::GET,
                        &format!("/api/v1/links/report-golden/visits?cursor={admin_cursor}"),
                        Some(new_admin_key),
                        "",
                    ),
                )
                .await;
                assert_eq!(inherited.status(), StatusCode::BAD_REQUEST);
                let _ = response_json(inherited).await;

                let before = sqlx::query_scalar::<_, i64>("SELECT visit_count FROM links WHERE short_code = 'report-golden'")
                    .fetch_one(database.pool())
                    .await
                    .unwrap();
                assert_eq!(before, 7, "worker persistence increments the counter atomically");

                let boundary_page = report(
                    &router,
                    &key,
                    "/api/v1/links/report-golden/visits?limit=1",
                )
                .await;
                let deleted_boundary = boundary_page["items"][0]["id"].as_str().unwrap().to_owned();
                let boundary_cursor = boundary_page["next_cursor"].as_str().unwrap().to_owned();
                // This direct row deletion is limited to the explicit stateless
                // boundary regression: continuation retains values, not a row lookup.
                sqlx::query("DELETE FROM visits WHERE id=$1")
                    .bind(Uuid::parse_str(&deleted_boundary).unwrap())
                    .execute(database.pool())
                    .await
                    .unwrap();
                let continued = report(
                    &router,
                    &key,
                    &format!("/api/v1/links/report-golden/visits?limit=1&cursor={boundary_cursor}"),
                )
                .await;
                assert_eq!(continued["items"].as_array().unwrap().len(), 1);
                assert_ne!(continued["items"][0]["id"], deleted_boundary);
            })
        })
        .await;
}

#[tokio::test]
async fn report_pool_failure_is_a_storage_error_not_an_empty_report() {
    let key = crate::domain::secret::AccountApiKey::generate().unwrap();
    let values = BTreeMap::from([
        (
            "FERL_DATABASE_URL".to_owned(),
            "postgres://ferl:ferl@127.0.0.1:1/ferl_test".to_owned(),
        ),
        (
            "FERL_PUBLIC_BASE_URL".to_owned(),
            "https://reports.example.test".to_owned(),
        ),
        (
            "FERL_CURSOR_KEY".to_owned(),
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".to_owned(),
        ),
        ("FERL_DB_ACQUIRE_TIMEOUT_SECS".to_owned(), "1".to_owned()),
        (
            "FERL_REPORT_OPERATION_TIMEOUT_SECS".to_owned(),
            "3".to_owned(),
        ),
    ]);
    let config = Config::from_map(values).unwrap();
    let state = AppState::from_config(&config).unwrap();
    let router = app::router(state);
    let response = call(
        router,
        request(
            Method::GET,
            "/api/v1/links/no-link/stats",
            Some(key.as_str()),
            "",
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        response_json(response).await["error"]["code"],
        "storage_unavailable"
    );
}

#[tokio::test]
async fn production_report_query_builders_emit_analyze_buffer_plans_for_representative_data() {
    let database = migrated_database().await;
    database
        .verify(|database| {
            Box::pin(async move {
                let (_, owner_a_key) = seed_account(database.pool(), "report_plan_owner_a", "user", true).await;
                let (_, owner_b_key) = seed_account(database.pool(), "report_plan_owner_b", "user", true).await;
                let state = production_state(database.schema_database_url(), 6);
                state.verify_schema().await.unwrap();
                let router = app::router(state.clone());
                create_link(&router, &owner_a_key, "plan-report-a").await;
                create_link(&router, &owner_b_key, "plan-report-b").await;
                let link_a = resolve_active_link(&state.pool, "plan-report-a", Duration::from_secs(2))
                    .await.unwrap().unwrap();
                let link_b = resolve_active_link(&state.pool, "plan-report-b", Duration::from_secs(2))
                    .await.unwrap().unwrap();

                let workers = state.start_analytics_workers().unwrap();
                let chrome = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/120.0.0.0 Safari/537.36";
                let iphone = "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Mobile/15E148 Safari/604.1";
                for index in 0..1_200_u128 {
                    let agent = match index % 20 {
                        0 => None,
                        1 => Some(String::new()),
                        2 => Some(chrome.to_owned()),
                        3 => Some(iphone.to_owned()),
                        _ => Some(format!("Agent/{:04}", index % 500)),
                    };
                    let referrer = match index % 10 {
                        0 => Some(String::new()),
                        1 => Some(format!("https://ref-plan.example/{}", index % 100)),
                        _ => None,
                    };
                    let day = (index % 28) + 1;
                    let instant = format!("2024-02-{day:02}T{:02}:{:02}:{:02}Z", (index % 24), (index % 60), (index % 60));
                    let ip = if index % 7 == 0 {
                        "2001:db8::1".to_owned()
                    } else {
                        format!("192.0.2.{}", (index % 4) + 1)
                    };
                    enqueue_event(
                        &state,
                        &link_a,
                        10_000 + index,
                        &instant,
                        if index % 5 == 0 { VisitMethod::Head } else { VisitMethod::Get },
                        &ip,
                        test_headers(referrer.as_deref(), agent.as_deref()),
                    );
                }
                for index in 0..120_u128 {
                    let instant = format!("2024-03-{:02}T12:00:00Z", (index % 28) + 1);
                    enqueue_event(
                        &state,
                        &link_b,
                        100_000 + index,
                        &instant,
                        VisitMethod::Get,
                        "192.0.2.100",
                        test_headers(
                            Some("https://other-owner.example/path"),
                            Some("Owner-B-Agent/1"),
                        ),
                    );
                }
                assert_eq!(workers.shutdown(Duration::from_secs(30)).await, crate::analytics::WorkerStopOutcome::Drained);
                sqlx::query("ANALYZE visits").execute(database.pool()).await.unwrap();
                sqlx::query("ANALYZE links").execute(database.pool()).await.unwrap();

                let id_a: Uuid = sqlx::query_scalar("SELECT id FROM links WHERE short_code='plan-report-a'")
                    .fetch_one(database.pool()).await.unwrap();
                let total: i64 = sqlx::query_scalar("SELECT count(*) FROM visits WHERE link_id=$1")
                    .bind(id_a).fetch_one(database.pool()).await.unwrap();
                assert_eq!(total, 1_200);
                let broad = crate::domain::time::TimeInterval::new(None, None).unwrap();
                let narrow = crate::domain::time::TimeInterval::new(
                    Some(Timestamp::parse_rfc3339("2024-02-10T00:00:00Z").unwrap()),
                    Some(Timestamp::parse_rfc3339("2024-02-13T00:00:00Z").unwrap()),
                ).unwrap();
                let raw_boundary = VisitBoundary {
                    occurred_at: Timestamp::parse_rfc3339("2024-02-20T12:00:00Z").unwrap(),
                    id: VisitId::new(Uuid::from_u128(10_600)),
                };
                let raw_first = report_repository::explain_visits(database.pool(), id_a, broad, None, 51).await.unwrap();
                let raw_continuation = report_repository::explain_visits(database.pool(), id_a, narrow, Some(&raw_boundary), 51).await.unwrap();
                print_plan("visits broad first", &raw_first);
                print_plan("visits narrow continuation", &raw_continuation);
                let summary_lifetime = report_repository::explain_summary(database.pool(), id_a, broad).await.unwrap();
                let summary_narrow = report_repository::explain_summary(database.pool(), id_a, narrow).await.unwrap();
                print_plan("summary lifetime", &summary_lifetime);
                print_plan("summary narrow", &summary_narrow);

                for group in [
                    GroupBy::Day,
                    GroupBy::Method,
                    GroupBy::Referrer,
                    GroupBy::ClientIp,
                    GroupBy::UserAgent,
                    GroupBy::Device,
                    GroupBy::Browser,
                    GroupBy::Os,
                ] {
                    let (label, boundary) = match group {
                        GroupBy::Day => ("day", Some("2024-02-10".to_owned())),
                        GroupBy::Method => ("method", Some("GET".to_owned())),
                        GroupBy::Referrer => ("referrer", None),
                        GroupBy::ClientIp => ("client_ip", Some("192.0.2.1".to_owned())),
                        GroupBy::UserAgent => ("user_agent", None),
                        GroupBy::Device => ("device", Some("desktop".to_owned())),
                        GroupBy::Browser => ("browser", Some("Chrome".to_owned())),
                        GroupBy::Os => ("os", Some("iOS".to_owned())),
                    };
                    let first = report_repository::explain_breakdown(
                        database.pool(), id_a, broad, group, None, 21,
                    ).await.unwrap();
                    let continued = report_repository::explain_breakdown(
                        database.pool(), id_a, broad, group,
                        Some(&BreakdownBoundary { value: boundary }), 21,
                    ).await.unwrap();
                    print_plan(&format!("breakdown {label} first"), &first);
                    print_plan(&format!("breakdown {label} continuation"), &continued);
                }
                let other_owner: i64 = sqlx::query_scalar("SELECT count(*) FROM visits v JOIN links l ON l.id=v.link_id WHERE l.short_code='plan-report-b'")
                    .fetch_one(database.pool()).await.unwrap();
                assert_eq!(other_owner, 120, "representative plans include multiple links and owners");
            })
        })
        .await;
}

#[tokio::test]
async fn worker_counter_snapshots_are_coherent_and_part08_listing_observes_committed_events() {
    use std::sync::Arc;

    use tokio::{sync::Barrier, task::JoinSet};

    let database = migrated_database().await;
    database
        .verify(|database| {
            Box::pin(async move {
                let (_, key) = seed_account(database.pool(), "report_concurrent_owner", "user", true).await;
                let state = production_state(database.schema_database_url(), 4);
                state.verify_schema().await.unwrap();
                let router = app::router(state.clone());
                create_link(&router, &key, "report-concurrent").await;
                let snapshot = resolve_active_link(&state.pool, "report-concurrent", Duration::from_secs(2))
                    .await.unwrap().unwrap();
                let workers = state.start_analytics_workers().unwrap();

                // Block the real worker at its live-link row lock, observe its wait
                // in pg_stat_activity, and prove reports show the pre-commit 0/0.
                let mut blocker = database.pool().begin().await.unwrap();
                sqlx::query("SELECT id FROM links WHERE id=$1 FOR UPDATE")
                    .bind(snapshot.link_id().as_uuid())
                    .fetch_one(&mut *blocker)
                    .await
                    .unwrap();
                enqueue_event(
                    &state,
                    &snapshot,
                    1,
                    "2024-06-01T12:00:00Z",
                    VisitMethod::Get,
                    "192.0.2.1",
                    test_headers(None, None),
                );
                wait_for_worker_lock_wait(database.pool()).await;
                assert_eq!(
                    report(&router, &key, "/api/v1/links/report-concurrent/stats").await,
                    json!({"lifetime_visits":0,"matching_visits":0})
                );
                blocker.commit().await.unwrap();
                wait_for_visits(database.pool(), "report-concurrent", 1).await;
                assert_eq!(
                    report(&router, &key, "/api/v1/links/report-concurrent/stats").await,
                    json!({"lifetime_visits":1,"matching_visits":1})
                );

                let mut events = Vec::new();
                for index in 2..402_u128 {
                    let at = format!("2024-06-01T12:{:02}:{:02}Z", (index / 60) % 60, index % 60);
                    let ip = format!("192.0.2.{}", (index % 200) + 1);
                    let event = VisitEvent::for_test(
                        &snapshot,
                        Uuid::from_u128(index),
                        Timestamp::parse_rfc3339(&at).unwrap(),
                        if index % 2 == 0 { VisitMethod::Get } else { VisitMethod::Head },
                        CanonicalIp::new(ip.parse::<IpAddr>().unwrap()),
                        &HeaderMap::new(),
                    );
                    events.push(event);
                }
                let barrier = Arc::new(Barrier::new(3));
                let mut tasks = JoinSet::new();
                let writer_queue = state.analytics.clone();
                let writer_barrier = Arc::clone(&barrier);
                tasks.spawn(async move {
                    writer_barrier.wait().await;
                    for (index, event) in events.into_iter().enumerate() {
                        writer_queue.try_enqueue(event);
                        if index % 16 == 0 {
                            tokio::task::yield_now().await;
                        }
                    }
                });
                let reader_router = router.clone();
                let reader_key = key.clone();
                let reader_barrier = Arc::clone(&barrier);
                tasks.spawn(async move {
                    reader_barrier.wait().await;
                    for _ in 0..160 {
                        let body = report(
                            &reader_router,
                            &reader_key,
                            "/api/v1/links/report-concurrent/stats",
                        )
                        .await;
                        assert_eq!(
                            body["lifetime_visits"], body["matching_visits"],
                            "one response never combines a lifetime counter with a different visit snapshot"
                        );
                    }
                });
                barrier.wait().await;
                tokio::time::timeout(Duration::from_secs(30), async {
                    while let Some(joined) = tasks.join_next().await {
                        joined.expect("concurrent worker/report task completes without panic");
                    }
                })
                .await
                .expect("concurrent report and event workload stays within deadline");

                assert_eq!(workers.shutdown(Duration::from_secs(20)).await, crate::analytics::WorkerStopOutcome::Drained);
                wait_for_visits(database.pool(), "report-concurrent", 401).await;
                let summary = report(&router, &key, "/api/v1/links/report-concurrent/stats").await;
                assert_eq!(summary, json!({"lifetime_visits":401,"matching_visits":401}));
                let links = call(
                    router,
                    request(Method::GET, "/api/v1/links?sort=visit_count&order=desc", Some(&key), ""),
                )
                .await;
                assert_eq!(links.status(), StatusCode::OK);
                assert_eq!(response_json(links).await["items"][0]["visit_count"], 401);
            })
        })
        .await;
}

#[tokio::test]
async fn final_snapshot_revalidation_rejects_link_deletion_token_rotation_and_account_deletion() {
    use http::{HeaderMap, HeaderValue, header};

    let database = migrated_database().await;
    database
        .verify(|database| {
            Box::pin(async move {
                let (_, owner_key) =
                    seed_account(database.pool(), "report_race_owner", "user", true).await;
                let (_admin_id, admin_key) =
                    seed_account(database.pool(), "report_race_admin", "admin", true).await;
                let state = production_state(database.schema_database_url(), 2);
                state.verify_schema().await.unwrap();
                let router = app::router(state.clone());
                create_link(&router, &owner_key, "report-race-delete").await;

                let mut account_headers = HeaderMap::new();
                account_headers.insert(
                    header::AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {owner_key}")).unwrap(),
                );
                let account_principal = crate::auth::authenticate(
                    &state.pool,
                    &account_headers,
                    Duration::from_secs(2),
                )
                .await
                .unwrap()
                .unwrap();
                assert!(
                    state
                        .reports
                        .authorize_link(&account_principal, "report-race-delete")
                        .await
                        .is_ok()
                );
                let deleted = call(
                    router.clone(),
                    request(
                        Method::DELETE,
                        "/api/v1/links/report-race-delete",
                        Some(&owner_key),
                        "",
                    ),
                )
                .await;
                assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
                assert!(matches!(
                    state
                        .reports
                        .summary(
                            &account_principal,
                            "report-race-delete",
                            crate::http::query::VisitStatsQuery::parse(None)
                                .unwrap()
                                .interval(),
                        )
                        .await,
                    Err(super::ReportFailure::NotFound)
                ));

                let (token_link, token) = crate::account_support::seed_anonymous_link(
                    database.pool(),
                    "report-race-token",
                )
                .await;
                let mut token_headers = HeaderMap::new();
                token_headers.insert(
                    header::AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
                );
                let token_principal =
                    crate::auth::authenticate(&state.pool, &token_headers, Duration::from_secs(2))
                        .await
                        .unwrap()
                        .unwrap();
                assert_eq!(
                    state
                        .reports
                        .authorize_link(&token_principal, "report-race-token")
                        .await
                        .unwrap(),
                    token_link
                );
                let rotation = call(
                    router.clone(),
                    request(
                        Method::POST,
                        "/api/v1/links/report-race-token/management-token",
                        Some(&token),
                        "",
                    ),
                )
                .await;
                assert_eq!(rotation.status(), StatusCode::OK);
                assert!(matches!(
                    state
                        .reports
                        .summary(
                            &token_principal,
                            "report-race-token",
                            crate::http::query::VisitStatsQuery::parse(None)
                                .unwrap()
                                .interval(),
                        )
                        .await,
                    Err(super::ReportFailure::InvalidCredentials)
                ));

                let (delete_owner_id, delete_owner_key) =
                    seed_account(database.pool(), "report_race_deleted_owner", "user", true).await;
                create_link(&router, &delete_owner_key, "report-race-account-delete").await;
                let mut deleted_owner_headers = HeaderMap::new();
                deleted_owner_headers.insert(
                    header::AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {delete_owner_key}")).unwrap(),
                );
                let deleted_owner_principal = crate::auth::authenticate(
                    &state.pool,
                    &deleted_owner_headers,
                    Duration::from_secs(2),
                )
                .await
                .unwrap()
                .unwrap();
                state
                    .reports
                    .authorize_link(&deleted_owner_principal, "report-race-account-delete")
                    .await
                    .unwrap();
                let deleted_account = call(
                    router,
                    request(
                        Method::DELETE,
                        &format!("/api/v1/admin/accounts/{}", delete_owner_id.as_uuid()),
                        Some(&admin_key),
                        "",
                    ),
                )
                .await;
                assert_eq!(deleted_account.status(), StatusCode::NO_CONTENT);
                assert!(matches!(
                    state
                        .reports
                        .summary(
                            &deleted_owner_principal,
                            "report-race-account-delete",
                            crate::http::query::VisitStatsQuery::parse(None)
                                .unwrap()
                                .interval(),
                        )
                        .await,
                    Err(super::ReportFailure::InvalidCredentials)
                ));
            })
        })
        .await;
}

#[tokio::test]
async fn already_started_read_snapshots_finish_consistently_across_delete_and_token_rotation_commits()
 {
    use http::{HeaderMap, HeaderValue, header};

    let database = migrated_database().await;
    database
        .verify(|database| {
            Box::pin(async move {
                let (_, owner_key) =
                    seed_account(database.pool(), "report_snapshot_owner", "user", true).await;
                let (_, admin_key) =
                    seed_account(database.pool(), "report_snapshot_admin", "admin", true).await;
                let state = production_state(database.schema_database_url(), 3);
                state.verify_schema().await.unwrap();
                let router = app::router(state.clone());
                create_link(&router, &owner_key, "report-snapshot-delete").await;

                let mut owner_headers = HeaderMap::new();
                owner_headers.insert(
                    header::AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {owner_key}")).unwrap(),
                );
                let owner_principal =
                    crate::auth::authenticate(&state.pool, &owner_headers, Duration::from_secs(2))
                        .await
                        .unwrap()
                        .unwrap();
                let (mut delete_snapshot, delete_link_id) =
                    report_repository::begin_authorized_snapshot(
                        &state.pool,
                        &owner_principal,
                        "report-snapshot-delete",
                    )
                    .await
                    .unwrap();
                let deletion = call(
                    router.clone(),
                    request(
                        Method::DELETE,
                        "/api/v1/links/report-snapshot-delete",
                        Some(&owner_key),
                        "",
                    ),
                )
                .await;
                assert_eq!(deletion.status(), StatusCode::NO_CONTENT);
                let old_snapshot = crate::analytics::report_query::summary(
                    delete_link_id.as_uuid(),
                    TimeInterval::new(None, None).unwrap(),
                    false,
                )
                .build_query_as::<crate::analytics::report_repository::SummaryRecord>()
                .fetch_one(&mut *delete_snapshot)
                .await
                .unwrap();
                delete_snapshot.commit().await.unwrap();
                assert_eq!(
                    (old_snapshot.lifetime_visits, old_snapshot.matching_visits),
                    (0, 0)
                );
                let (deleted_status, deleted_body) = report_response(
                    &router,
                    Method::GET,
                    Some(&admin_key),
                    "/api/v1/links/report-snapshot-delete/stats",
                )
                .await;
                assert_eq!(deleted_status, StatusCode::NOT_FOUND);
                assert_eq!(deleted_body["error"]["code"], "not_found");

                let (token_link, old_token) = crate::account_support::seed_anonymous_link(
                    database.pool(),
                    "report-snapshot-token",
                )
                .await;
                let mut token_headers = HeaderMap::new();
                token_headers.insert(
                    header::AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {old_token}")).unwrap(),
                );
                let token_principal =
                    crate::auth::authenticate(&state.pool, &token_headers, Duration::from_secs(2))
                        .await
                        .unwrap()
                        .unwrap();
                let (mut token_snapshot, token_link_id) =
                    report_repository::begin_authorized_snapshot(
                        &state.pool,
                        &token_principal,
                        "report-snapshot-token",
                    )
                    .await
                    .unwrap();
                assert_eq!(token_link_id, token_link);
                let rotation = call(
                    router.clone(),
                    request(
                        Method::POST,
                        "/api/v1/links/report-snapshot-token/management-token",
                        Some(&old_token),
                        "",
                    ),
                )
                .await;
                assert_eq!(rotation.status(), StatusCode::OK);
                let replacement = response_json(rotation).await["management_token"]
                    .as_str()
                    .unwrap()
                    .to_owned();
                let old_token_snapshot = crate::analytics::report_query::summary(
                    token_link_id.as_uuid(),
                    TimeInterval::new(None, None).unwrap(),
                    false,
                )
                .build_query_as::<crate::analytics::report_repository::SummaryRecord>()
                .fetch_one(&mut *token_snapshot)
                .await
                .unwrap();
                token_snapshot.commit().await.unwrap();
                assert_eq!(
                    (
                        old_token_snapshot.lifetime_visits,
                        old_token_snapshot.matching_visits
                    ),
                    (0, 0)
                );
                let (old_status, old_body) = report_response(
                    &router,
                    Method::GET,
                    Some(&old_token),
                    "/api/v1/links/report-snapshot-token/stats",
                )
                .await;
                assert_eq!(old_status, StatusCode::UNAUTHORIZED);
                assert_eq!(old_body["error"]["code"], "invalid_credentials");
                let (new_status, new_body) = report_response(
                    &router,
                    Method::GET,
                    Some(&replacement),
                    "/api/v1/links/report-snapshot-token/stats",
                )
                .await;
                assert_eq!(new_status, StatusCode::OK);
                assert_eq!(new_body["lifetime_visits"], 0);
            })
        })
        .await;
}

fn print_plan(label: &str, plan: &Value) {
    fn walk(node: &Value, nodes: &mut Vec<String>) {
        let Some(object) = node.as_object() else {
            return;
        };
        if let Some(kind) = object.get("Node Type").and_then(Value::as_str) {
            let rows = observed_number(object, "Actual Rows");
            let loops = observed_number(object, "Actual Loops");
            let hits = observed_number(object, "Shared Hit Blocks");
            let reads = observed_number(object, "Shared Read Blocks");
            let removed = observed_number(object, "Rows Removed by Filter");
            let index = object
                .get("Index Name")
                .and_then(Value::as_str)
                .unwrap_or("-");
            let strategy = object
                .get("Strategy")
                .and_then(Value::as_str)
                .unwrap_or("-");
            let sort = object
                .get("Sort Method")
                .and_then(Value::as_str)
                .unwrap_or("-");
            let sort_space = observed_number(object, "Sort Space Used");
            let memory = observed_number(object, "Peak Memory Usage");
            let hash_batches = observed_number(object, "Hash Batches");
            nodes.push(format!(
                "{kind}[strategy={strategy},rows={},loops={},removed={},hits={},reads={},index={index},sort={sort},sort_space_kb={},peak_memory_kb={},hash_batches={}]",
                rows.as_deref().unwrap_or("-"),
                loops.as_deref().unwrap_or("-"),
                removed.as_deref().unwrap_or("-"),
                hits.as_deref().unwrap_or("-"),
                reads.as_deref().unwrap_or("-"),
                sort_space.as_deref().unwrap_or("-"),
                memory.as_deref().unwrap_or("-"),
                hash_batches.as_deref().unwrap_or("-"),
            ));
        }
        if let Some(plans) = object.get("Plans").and_then(Value::as_array) {
            for child in plans {
                walk(child, nodes);
            }
        }
    }
    let root = &plan[0];
    let mut nodes = Vec::new();
    walk(&root["Plan"], &mut nodes);
    println!(
        "REPORT_PLAN {label}: planning_ms={} execution_ms={} {}",
        root["Planning Time"]
            .as_f64()
            .expect("EXPLAIN reports a numeric planning time"),
        root["Execution Time"]
            .as_f64()
            .expect("EXPLAIN reports a numeric execution time"),
        nodes.join(" -> ")
    );
}

/// PostgreSQL 18 reports integral EXPLAIN observations as JSON floats, and
/// fractional averages remain fractional. Keep every numeric observation exact
/// enough to inspect; a value that cannot be read is a failed observation.
fn observed_number(object: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    let value = object.get(key)?;
    let number = value
        .as_f64()
        .unwrap_or_else(|| panic!("EXPLAIN field {key} is not numeric: {value}"));
    Some(number.to_string())
}
