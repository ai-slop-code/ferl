#[path = "support/accounts.rs"]
#[allow(dead_code)]
mod account_support;
#[path = "support/database.rs"]
#[allow(dead_code)]
mod database_support;
#[path = "support/redirects.rs"]
#[allow(dead_code)]
mod redirect_support;

use std::{
    collections::BTreeMap,
    io::Write,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};

use account_support::{call, json, migrated_database, request, seed_account};
use axum::http::{Method, StatusCode, header};
use ferl_server::{
    app::{self, AppState},
    config::Config,
    telemetry::logging,
};
use http::HeaderValue;
use sqlx::PgPool;
use tokio::{
    task::JoinSet,
    time::{sleep, timeout},
};
use tower::ServiceExt;
use uuid::Uuid;

#[derive(Clone)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn production_logs() -> Capture {
    let capture = Capture(Arc::new(Mutex::new(Vec::new())));
    let writer = capture.clone();
    logging::init_with_writer(move || writer.clone());
    capture
}

fn log_text(capture: &Capture) -> String {
    String::from_utf8(capture.0.lock().unwrap().clone()).expect("production logs are UTF-8 JSON")
}

fn build_state(database_url: &str, extra: &[(&str, &str)]) -> AppState {
    let mut values = BTreeMap::from([
        ("FERL_DATABASE_URL".to_owned(), database_url.to_owned()),
        (
            "FERL_PUBLIC_BASE_URL".to_owned(),
            "https://failure-reports.example.test".to_owned(),
        ),
        (
            "FERL_CURSOR_KEY".to_owned(),
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".to_owned(),
        ),
        ("FERL_DB_POOL_MAX".to_owned(), "1".to_owned()),
        ("FERL_DB_ACQUIRE_TIMEOUT_SECS".to_owned(), "5".to_owned()),
        ("FERL_DB_STATEMENT_TIMEOUT_SECS".to_owned(), "5".to_owned()),
        ("FERL_DB_LOCK_TIMEOUT_SECS".to_owned(), "1".to_owned()),
        (
            "FERL_REPORT_OPERATION_TIMEOUT_SECS".to_owned(),
            "10".to_owned(),
        ),
    ]);
    for (name, value) in extra {
        values.insert((*name).to_owned(), (*value).to_owned());
    }
    let config = Config::from_map(values).expect("report failure profile is valid");
    AppState::from_config(&config).expect("production report state composes")
}

async fn wait_for_visits(pool: &PgPool, code: &str, expected: i64) {
    timeout(Duration::from_secs(8), async {
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
            assert!(count < expected, "worker does not duplicate report fixtures");
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("worker persistence is observed before the bounded deadline");
}

async fn wait_for_report_lock_wait(pool: &PgPool) {
    timeout(Duration::from_secs(5), async {
        loop {
            let waiting: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_stat_activity \
                 WHERE datname=current_database() AND wait_event_type='Lock' \
                   AND query LIKE '%FROM visits WHERE link_id%')",
            )
            .fetch_one(pool)
            .await
            .unwrap();
            if waiting {
                return;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("report SQL lock wait is observed before releasing the test barrier");
}

#[derive(Clone, Copy)]
enum Corruption<'a> {
    Timestamp(&'a str),
    Method(&'a str),
    Device(&'a str),
    Browser(&'a str),
    Os(&'a str),
    ClientIp(&'a str),
    Referrer(Option<&'a str>),
}

async fn corrupt_visit(pool: &PgPool, id: Uuid, fault: Corruption<'_>, constraint: Option<&str>) {
    let mut transaction = pool.begin().await.unwrap();
    sqlx::query("ALTER TABLE visits DISABLE TRIGGER visits_immutable_trigger")
        .execute(&mut *transaction)
        .await
        .unwrap();
    if let Some(constraint) = constraint {
        let statement = match constraint {
            "visits_method_check" => "ALTER TABLE visits DROP CONSTRAINT visits_method_check",
            "visits_device_check" => "ALTER TABLE visits DROP CONSTRAINT visits_device_check",
            "visits_browser_nonempty_check" => {
                "ALTER TABLE visits DROP CONSTRAINT visits_browser_nonempty_check"
            }
            "visits_os_nonempty_check" => {
                "ALTER TABLE visits DROP CONSTRAINT visits_os_nonempty_check"
            }
            "visits_client_ip_host_check" => {
                "ALTER TABLE visits DROP CONSTRAINT visits_client_ip_host_check"
            }
            "visits_referrer_length_check" => {
                "ALTER TABLE visits DROP CONSTRAINT visits_referrer_length_check"
            }
            _ => panic!("fault fixture uses only named report data constraints"),
        };
        sqlx::query(statement)
            .execute(&mut *transaction)
            .await
            .unwrap();
    }
    match fault {
        Corruption::Timestamp(value) => {
            sqlx::query("UPDATE visits SET occurred_at=$1::timestamptz WHERE id=$2")
                .bind(value)
                .bind(id)
                .execute(&mut *transaction)
                .await
                .unwrap();
        }
        Corruption::Method(value) => {
            sqlx::query("UPDATE visits SET method=$1 WHERE id=$2")
                .bind(value)
                .bind(id)
                .execute(&mut *transaction)
                .await
                .unwrap();
        }
        Corruption::Device(value) => {
            sqlx::query("UPDATE visits SET device=$1 WHERE id=$2")
                .bind(value)
                .bind(id)
                .execute(&mut *transaction)
                .await
                .unwrap();
        }
        Corruption::Browser(value) => {
            sqlx::query("UPDATE visits SET browser=$1 WHERE id=$2")
                .bind(value)
                .bind(id)
                .execute(&mut *transaction)
                .await
                .unwrap();
        }
        Corruption::Os(value) => {
            sqlx::query("UPDATE visits SET os=$1 WHERE id=$2")
                .bind(value)
                .bind(id)
                .execute(&mut *transaction)
                .await
                .unwrap();
        }
        Corruption::ClientIp(value) => {
            sqlx::query("UPDATE visits SET client_ip=$1::inet WHERE id=$2")
                .bind(value)
                .bind(id)
                .execute(&mut *transaction)
                .await
                .unwrap();
        }
        Corruption::Referrer(value) => {
            sqlx::query("UPDATE visits SET referrer=$1 WHERE id=$2")
                .bind(value)
                .bind(id)
                .execute(&mut *transaction)
                .await
                .unwrap();
        }
    }
    sqlx::query("ALTER TABLE visits ENABLE TRIGGER visits_immutable_trigger")
        .execute(&mut *transaction)
        .await
        .unwrap();
    transaction.commit().await.unwrap();
}

async fn assert_internal(app: &axum::Router, key: &str, path: &str) {
    let response = call(app.clone(), request(Method::GET, path, Some(key), "")).await;
    assert_eq!(
        response.status(),
        StatusCode::INTERNAL_SERVER_ERROR,
        "{path}"
    );
    assert!(!response.headers().contains_key("location"));
    assert_eq!(json(response).await["error"]["code"], "internal_error");
}

#[tokio::test]
async fn report_failures_corruption_cancellation_and_diagnostics_are_safe_and_recoverable() {
    let capture = production_logs();
    let database = migrated_database().await;
    database
        .verify(|database| {
            Box::pin(async move {
                let (owner_id, key) = seed_account(database.pool(), "report_failure_owner", "user", true).await;
                let state = build_state(database.schema_database_url(), &[]);
                state.verify_schema().await.unwrap();
                let app = app::router(state.clone());
                let code = "report-fault-canary";
                let created = call(
                    app.clone(),
                    request(
                        Method::POST,
                        "/api/v1/links",
                        Some(&key),
                        r#"{"destination_url":"https://destination.report-private-canary.invalid/A?q=private#fragment","short_code":"report-fault-canary","title":"TITLE_REPORT_PRIVATE_CANARY","tags":["TAG_REPORT_PRIVATE_CANARY"]}"#,
                    ),
                )
                .await;
                assert_eq!(created.status(), StatusCode::CREATED);
                let link = json(created).await["link"].clone();
                let link_id = Uuid::parse_str(link["id"].as_str().unwrap()).unwrap();

                let workers = state.start_analytics_workers().unwrap();
                let peer = SocketAddr::from(([198, 51, 100, 91], 45091));
                for _ in 0..2 {
                    let mut redirect_request =
                        redirect_support::request(Method::GET, "/report-fault-canary", peer);
                    redirect_request.headers_mut().insert(
                        header::REFERER,
                        HeaderValue::from_static("https://referrer.report-private-canary.invalid/?q=hidden"),
                    );
                    redirect_request.headers_mut().insert(
                        header::USER_AGENT,
                        HeaderValue::from_static("REPORT_USER_AGENT_PRIVATE_CANARY"),
                    );
                    let response = redirect_support::call(
                        app.clone(),
                        redirect_request,
                    )
                    .await;
                    assert_eq!(response.status(), StatusCode::FOUND);
                }
                wait_for_visits(database.pool(), code, 2).await;

                let success = call(
                    app.clone(),
                    request(
                        Method::GET,
                        "/api/v1/links/report-fault-canary/visits?limit=1",
                        Some(&key),
                        "",
                    ),
                )
                .await;
                assert_eq!(success.status(), StatusCode::OK);
                let success_body = json(success).await;
                let cursor_canary = success_body["next_cursor"].as_str().unwrap().to_owned();
                assert_eq!(success_body["items"].as_array().unwrap().len(), 1);

                let invalid_auth = call(
                    app.clone(),
                    request(
                        Method::GET,
                        "/api/v1/links/report-fault-canary/stats",
                        Some("invalid-credential-canary"),
                        "",
                    ),
                )
                .await;
                assert_eq!(invalid_auth.status(), StatusCode::UNAUTHORIZED);
                assert_eq!(invalid_auth.headers()["www-authenticate"], "Bearer");
                let _ = json(invalid_auth).await;

                let invalid_input = call(
                    app.clone(),
                    request(
                        Method::GET,
                        "/api/v1/links/report-fault-canary/stats?from=REPORT_QUERY_PRIVATE_CANARY",
                        Some(&key),
                        "",
                    ),
                )
                .await;
                assert_eq!(invalid_input.status(), StatusCode::BAD_REQUEST);
                let _ = json(invalid_input).await;

                let before_visits: i64 = sqlx::query_scalar("SELECT count(*) FROM visits")
                    .fetch_one(database.pool())
                    .await
                    .unwrap();
                let before_charges: i64 = sqlx::query_scalar("SELECT count(*) FROM rate_charges")
                    .fetch_one(database.pool())
                    .await
                    .unwrap();

                // An observed ACCESS EXCLUSIVE lock blocks the actual report SELECT.
                let mut blocker = database.pool().begin().await.unwrap();
                sqlx::query("LOCK TABLE visits IN ACCESS EXCLUSIVE MODE")
                    .execute(&mut *blocker)
                    .await
                    .unwrap();
                let started = tokio::time::Instant::now();
                let unavailable = call(
                    app.clone(),
                    request(
                        Method::GET,
                        &format!("/api/v1/links/report-fault-canary/visits?limit=1&cursor={cursor_canary}"),
                        Some(&key),
                        "",
                    ),
                )
                .await;
                assert_eq!(unavailable.status(), StatusCode::SERVICE_UNAVAILABLE);
                assert!(started.elapsed() < Duration::from_secs(4));
                assert_eq!(json(unavailable).await["error"]["code"], "storage_unavailable");
                blocker.rollback().await.unwrap();

                // Cancellation occurs only after PostgreSQL reports the real query
                // blocked on a lock. Dropping the request cannot leak the sole pool
                // connection or fabricate an empty page.
                let mut cancellation_blocker = database.pool().begin().await.unwrap();
                sqlx::query("LOCK TABLE visits IN ACCESS EXCLUSIVE MODE")
                    .execute(&mut *cancellation_blocker)
                    .await
                    .unwrap();
                let cancel_app = app.clone();
                let cancel_key = key.clone();
                let mut tasks = JoinSet::new();
                tasks.spawn(async move {
                    cancel_app
                        .oneshot(request(
                            Method::GET,
                            "/api/v1/links/report-fault-canary/visits?limit=1",
                            Some(&cancel_key),
                            "",
                        ))
                        .await
                });
                wait_for_report_lock_wait(database.pool()).await;
                tasks.abort_all();
                assert!(tasks.join_next().await.unwrap().unwrap_err().is_cancelled());
                cancellation_blocker.rollback().await.unwrap();
                let recovered = call(
                    app.clone(),
                    request(
                        Method::GET,
                        "/api/v1/links/report-fault-canary/stats",
                        Some(&key),
                        "",
                    ),
                )
                .await;
                assert_eq!(recovered.status(), StatusCode::OK);
                assert_eq!(json(recovered).await["lifetime_visits"], 2);

                let deadline_state = build_state(
                    database.schema_database_url(),
                    &[
                        ("FERL_DB_POOL_MAX", "1"),
                        ("FERL_DB_ACQUIRE_TIMEOUT_SECS", "5"),
                        ("FERL_REPORT_OPERATION_TIMEOUT_SECS", "1"),
                    ],
                );
                deadline_state.verify_schema().await.unwrap();
                let deadline_app = app::router(deadline_state.clone());
                let held_connection = deadline_state.pool.acquire().await.unwrap();
                let started = tokio::time::Instant::now();
                let deadline = call(
                    deadline_app,
                    request(
                        Method::GET,
                        "/api/v1/links/report-fault-canary/stats",
                        Some(&key),
                        "",
                    ),
                )
                .await;
                assert_eq!(deadline.status(), StatusCode::SERVICE_UNAVAILABLE);
                assert!(started.elapsed() < Duration::from_secs(3));
                assert_eq!(json(deadline).await["error"]["code"], "storage_unavailable");
                drop(held_connection);

                let visit_id: Uuid = sqlx::query_scalar(
                    "SELECT id FROM visits WHERE link_id=$1 ORDER BY id LIMIT 1",
                )
                .bind(link_id)
                .fetch_one(database.pool())
                .await
                .unwrap();
                let original_time: String = sqlx::query_scalar(
                    "SELECT occurred_at::text FROM visits WHERE id=$1",
                )
                .bind(visit_id)
                .fetch_one(database.pool())
                .await
                .unwrap();

                corrupt_visit(database.pool(), visit_id, Corruption::Timestamp("infinity"), None).await;
                assert_internal(&app, &key, "/api/v1/links/report-fault-canary/visits?limit=10").await;
                assert_internal(&app, &key, "/api/v1/links/report-fault-canary/stats/breakdown?group_by=day").await;
                corrupt_visit(database.pool(), visit_id, Corruption::Timestamp(&original_time), None).await;

                corrupt_visit(
                    database.pool(),
                    visit_id,
                    Corruption::Timestamp("10000-01-01 00:00:00+00"),
                    None,
                )
                .await;
                assert_internal(&app, &key, "/api/v1/links/report-fault-canary/visits?limit=10").await;
                assert_internal(&app, &key, "/api/v1/links/report-fault-canary/stats/breakdown?group_by=day").await;
                corrupt_visit(database.pool(), visit_id, Corruption::Timestamp(&original_time), None).await;

                corrupt_visit(
                    database.pool(),
                    visit_id,
                    Corruption::Method("corrupt-method"),
                    Some("visits_method_check"),
                )
                .await;
                assert_internal(&app, &key, "/api/v1/links/report-fault-canary/visits?limit=10").await;
                assert_internal(&app, &key, "/api/v1/links/report-fault-canary/stats/breakdown?group_by=method").await;
                corrupt_visit(database.pool(), visit_id, Corruption::Method("GET"), None).await;

                corrupt_visit(
                    database.pool(),
                    visit_id,
                    Corruption::Device("corrupt-device"),
                    Some("visits_device_check"),
                )
                .await;
                assert_internal(&app, &key, "/api/v1/links/report-fault-canary/visits?limit=10").await;
                assert_internal(&app, &key, "/api/v1/links/report-fault-canary/stats/breakdown?group_by=device").await;
                corrupt_visit(database.pool(), visit_id, Corruption::Device("unknown"), None).await;

                let overlong_family = "B".repeat(8_193);
                corrupt_visit(
                    database.pool(),
                    visit_id,
                    Corruption::Browser(&overlong_family),
                    None,
                )
                .await;
                assert_internal(&app, &key, "/api/v1/links/report-fault-canary/visits?limit=10").await;
                assert_internal(&app, &key, "/api/v1/links/report-fault-canary/stats/breakdown?group_by=browser").await;
                corrupt_visit(database.pool(), visit_id, Corruption::Browser("unknown"), None).await;

                corrupt_visit(
                    database.pool(),
                    visit_id,
                    Corruption::Os(""),
                    Some("visits_os_nonempty_check"),
                )
                .await;
                assert_internal(&app, &key, "/api/v1/links/report-fault-canary/visits?limit=10").await;
                assert_internal(&app, &key, "/api/v1/links/report-fault-canary/stats/breakdown?group_by=os").await;
                corrupt_visit(database.pool(), visit_id, Corruption::Os("unknown"), None).await;

                corrupt_visit(
                    database.pool(),
                    visit_id,
                    Corruption::ClientIp("192.0.2.0/24"),
                    Some("visits_client_ip_host_check"),
                )
                .await;
                assert_internal(&app, &key, "/api/v1/links/report-fault-canary/visits?limit=10").await;
                assert_internal(&app, &key, "/api/v1/links/report-fault-canary/stats/breakdown?group_by=client_ip").await;
                corrupt_visit(database.pool(), visit_id, Corruption::ClientIp("192.0.2.91/32"), None).await;

                let long_referrer = "R".repeat(8_193);
                corrupt_visit(
                    database.pool(),
                    visit_id,
                    Corruption::Referrer(Some(&long_referrer)),
                    Some("visits_referrer_length_check"),
                )
                .await;
                assert_internal(&app, &key, "/api/v1/links/report-fault-canary/visits?limit=10").await;
                assert_internal(&app, &key, "/api/v1/links/report-fault-canary/stats/breakdown?group_by=referrer").await;
                corrupt_visit(database.pool(), visit_id, Corruption::Referrer(None), None).await;

                let mut counter_transaction = database.pool().begin().await.unwrap();
                sqlx::query("ALTER TABLE links DROP CONSTRAINT links_visit_count_nonnegative_check")
                    .execute(&mut *counter_transaction)
                    .await
                    .unwrap();
                sqlx::query("UPDATE links SET visit_count=-1 WHERE id=$1")
                    .bind(link_id)
                    .execute(&mut *counter_transaction)
                    .await
                    .unwrap();
                counter_transaction.commit().await.unwrap();
                assert_internal(&app, &key, "/api/v1/links/report-fault-canary/stats").await;

                let mut counter_restore = database.pool().begin().await.unwrap();
                sqlx::query("UPDATE links SET visit_count=2 WHERE id=$1")
                    .bind(link_id)
                    .execute(&mut *counter_restore)
                    .await
                    .unwrap();
                counter_restore.commit().await.unwrap();

                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM visits")
                        .fetch_one(database.pool())
                        .await
                        .unwrap(),
                    before_visits
                );
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM rate_charges")
                        .fetch_one(database.pool())
                        .await
                        .unwrap(),
                    before_charges,
                    "success, errors, fault handling, and cancellation do not charge or create visits"
                );

                tracing::info!(target: "sqlx::query", dependency_private_canary = "DEPENDENCY_PRIVATE_CANARY", "dependency event is suppressed by the production filter");
                let logs = log_text(&capture);
                // Assert required nonempty diagnostics before testing exclusion so
                // a silent/empty capture cannot pass the privacy assertion.
                for required in [
                    "request completed",
                    "invalid_credentials",
                    "report_storage_unavailable",
                    "report_persistence_failure",
                    "report_operation_deadline",
                ] {
                    assert!(logs.contains(required), "production diagnostics include {required}");
                }
                let owner_id_canary = owner_id.as_uuid().to_string();
                for canary in [
                    database.schema_database_url(),
                    key.as_str(),
                    owner_id_canary.as_str(),
                    link["id"].as_str().unwrap(),
                    code,
                    "REPORT_QUERY_PRIVATE_CANARY",
                    "LOCK_CURSOR_PRIVATE_CANARY",
                    cursor_canary.as_str(),
                    "198.51.100.91",
                    "https://destination.report-private-canary.invalid/A?q=private#fragment",
                    "https://referrer.report-private-canary.invalid/?q=hidden",
                    "REPORT_USER_AGENT_PRIVATE_CANARY",
                    "TITLE_REPORT_PRIVATE_CANARY",
                    "TAG_REPORT_PRIVATE_CANARY",
                    "DEPENDENCY_PRIVATE_CANARY",
                ] {
                    assert!(!logs.contains(canary), "production logs exclude {canary}");
                }
                assert_eq!(workers.shutdown(Duration::from_secs(8)).await, ferl_server::analytics::WorkerStopOutcome::Drained);
            })
        })
        .await;
}
