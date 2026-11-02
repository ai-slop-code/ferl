#[path = "support/accounts.rs"]
#[allow(dead_code)]
mod account_support;
#[path = "support/database.rs"]
#[allow(dead_code)]
mod database_support;
#[path = "support/serving_process.rs"]
mod serving_process;

use std::{process::Output, time::Duration};

use account_support::{migrated_database, seed_account};
use ferl_server::pagination::MAX_CURSOR_ENCODED_BYTES;
use serde_json::{Value, json};
use serving_process::{ServingProcess, assert_headers, json, send, send_with_headers};
use sqlx::PgPool;
use tokio::{
    process::Command,
    time::{sleep, timeout},
};

const CURSOR_KEY: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
const SETTINGS: &[(&str, &str)] = &[
    ("FERL_DB_POOL_MAX", "4"),
    ("FERL_DB_ACQUIRE_TIMEOUT_SECS", "2"),
    ("FERL_DB_LOCK_TIMEOUT_SECS", "5"),
    ("FERL_TRUSTED_PROXIES", "127.0.0.1"),
    ("FERL_ANONYMOUS_CREATION", "true"),
];

#[tokio::test]
async fn report_authority_history_and_maximum_breakdown_cursors_cross_tcp_instances_and_volume_restart()
 {
    let database = migrated_database().await;
    database
        .verify(|database| {
            Box::pin(async move {
                let (owner_id, original_key) =
                    seed_account(database.pool(), "report_process_owner", "user", true).await;
                let (_, admin_key) =
                    seed_account(database.pool(), "report_process_admin", "admin", true).await;
                let database_url = database.schema_database_url().to_owned();
                let a = ServingProcess::start_with(&database_url, CURSOR_KEY, SETTINGS).await;
                let b = ServingProcess::start_with(&database_url, CURSOR_KEY, SETTINGS).await;

                let created = send(
                    a.address,
                    "POST",
                    "/api/v1/links",
                    Some(&original_key),
                    r#"{"destination_url":"https://history-A.report-canary.invalid/start?q=old#A","short_code":"process-report","title":"REPORT_TITLE_PRIVATE_CANARY","tags":["REPORT_TAG_PRIVATE_CANARY"]}"#,
                )
                .await
                .unwrap();
                assert_headers(&created);
                assert_eq!(created.status, 201);
                let created_body = json(&created);
                assert_eq!(created_body["link"]["short_code"], "process-report");
                let report_link_id = created_body["link"]["id"].as_str().unwrap().to_owned();

                let first_redirect = send_with_headers(
                    b.address,
                    "GET",
                    "/process-report",
                    None,
                    "",
                    &[
                        ("X-Forwarded-For", "198.51.100.77"),
                        ("Referer", "https://referrer.report-canary.invalid/path?q=hidden"),
                        ("User-Agent", "PROCESS_UA_PRIVATE_CANARY"),
                    ],
                )
                .await
                .unwrap();
                assert_headers(&first_redirect);
                assert_eq!(first_redirect.status, 302);
                assert_eq!(
                    first_redirect.headers.get("location").map(String::as_str),
                    Some("https://history-a.report-canary.invalid/start?q=old#A")
                );

                let edited = send(
                    a.address,
                    "PATCH",
                    "/api/v1/links/process-report",
                    Some(&original_key),
                    r#"{"destination_url":"https://history-B.report-canary.invalid/next?q=new#B"}"#,
                )
                .await
                .unwrap();
                assert_headers(&edited);
                assert_eq!(edited.status, 200);
                let _ = json(&edited);
                let second_redirect = send_with_headers(
                    a.address,
                    "GET",
                    "/process-report",
                    None,
                    "",
                    &[("X-Forwarded-For", "198.51.100.78"), ("User-Agent", "PROCESS_UA_SECOND_CANARY")],
                )
                .await
                .unwrap();
                assert_eq!(second_redirect.status, 302);
                assert_eq!(second_redirect.headers["location"], "https://history-b.report-canary.invalid/next?q=new#B");
                let third_redirect = send_with_headers(
                    b.address,
                    "HEAD",
                    "/process-report",
                    None,
                    "",
                    &[("X-Forwarded-For", "198.51.100.79"), ("User-Agent", "PROCESS_UA_THIRD_CANARY")],
                )
                .await
                .unwrap();
                assert_eq!(third_redirect.status, 302);
                assert!(third_redirect.body.is_empty());

                wait_for_visit_count(database.pool(), "process-report", 3).await;
                let stats = send(
                    b.address,
                    "GET",
                    "/api/v1/links/process-report/stats",
                    Some(&original_key),
                    "",
                )
                .await
                .unwrap();
                assert_headers(&stats);
                assert_eq!(stats.status, 200);
                assert_eq!(json(&stats), json!({"lifetime_visits":3,"matching_visits":3}));
                let query_error = send(
                    b.address,
                    "GET",
                    "/api/v1/links/process-report/stats?from=REPORT_QUERY_PRIVATE_CANARY",
                    Some(&original_key),
                    "",
                )
                .await
                .unwrap();
                assert_headers(&query_error);
                assert_eq!(query_error.status, 400);

                let first_visits = send(
                    a.address,
                    "GET",
                    "/api/v1/links/process-report/visits?limit=1",
                    Some(&original_key),
                    "",
                )
                .await
                .unwrap();
                assert_eq!(first_visits.status, 200);
                let first_visits = json(&first_visits);
                assert_eq!(first_visits["items"].as_array().unwrap().len(), 1);
                let visit_id = first_visits["items"][0]["id"].as_str().unwrap().to_owned();
                let raw_url_a = first_visits["items"][0]["redirect_url"]
                    .as_str()
                    .unwrap()
                    .to_owned();
                let visit_cursor_a = first_visits["next_cursor"].as_str().unwrap().to_owned();
                assert!(!visit_cursor_a.contains("198.51.100."));

                let rotation = send(
                    a.address,
                    "POST",
                    "/api/v1/me/api-key",
                    Some(&original_key),
                    "",
                )
                .await
                .unwrap();
                assert_eq!(rotation.status, 200);
                let current_key = json(&rotation)["api_key"].as_str().unwrap().to_owned();
                let revoked = send(
                    b.address,
                    "GET",
                    "/api/v1/links/process-report/stats",
                    Some(&original_key),
                    "",
                )
                .await
                .unwrap();
                assert_headers(&revoked);
                assert_eq!(revoked.status, 401);
                assert_eq!(revoked.headers.get("www-authenticate").map(String::as_str), Some("Bearer"));

                let second_visits = send(
                    b.address,
                    "GET",
                    &format!("/api/v1/links/process-report/visits?limit=1&cursor={visit_cursor_a}"),
                    Some(&current_key),
                    "",
                )
                .await
                .unwrap();
                assert_headers(&second_visits);
                assert_eq!(second_visits.status, 200);
                let second_visits = json(&second_visits);
                assert_eq!(second_visits["items"].as_array().unwrap().len(), 1);
                let raw_url_b = second_visits["items"][0]["redirect_url"]
                    .as_str()
                    .unwrap()
                    .to_owned();
                let visit_cursor_b = second_visits["next_cursor"].as_str().unwrap().to_owned();

                let anonymous = send(
                    a.address,
                    "POST",
                    "/api/v1/links",
                    None,
                    r#"{"destination_url":"https://anonymous.report-canary.invalid/"}"#,
                )
                .await
                .unwrap();
                assert_eq!(anonymous.status, 201);
                let anonymous = json(&anonymous);
                let anonymous_code = anonymous["link"]["short_code"].as_str().unwrap().to_owned();
                let anonymous_link_id = anonymous["link"]["id"].as_str().unwrap().to_owned();
                let original_token = anonymous["management_token"].as_str().unwrap().to_owned();
                for agent in ["A", "B", "C", "D"] {
                    let visit = send_with_headers(
                        a.address,
                        "GET",
                        &format!("/{anonymous_code}"),
                        None,
                        "",
                        &[("User-Agent", agent)],
                    )
                    .await
                    .unwrap();
                    assert_eq!(visit.status, 302);
                }
                wait_for_visit_count(database.pool(), &anonymous_code, 4).await;
                let token_first = send(
                    a.address,
                    "GET",
                    &format!("/api/v1/links/{anonymous_code}/stats/breakdown?group_by=user_agent&limit=1"),
                    Some(&original_token),
                    "",
                )
                .await
                .unwrap();
                assert_eq!(token_first.status, 200);
                let token_first = json(&token_first);
                assert_eq!(token_first["items"][0]["value"], "A");
                let anonymous_cursor_a = token_first["next_cursor"].as_str().unwrap().to_owned();
                let token_rotation = send(
                    a.address,
                    "POST",
                    &format!("/api/v1/links/{anonymous_code}/management-token"),
                    Some(&original_token),
                    "",
                )
                .await
                .unwrap();
                assert_eq!(token_rotation.status, 200);
                let current_token = json(&token_rotation)["management_token"]
                    .as_str()
                    .unwrap()
                    .to_owned();
                let old_token = send(
                    b.address,
                    "GET",
                    &format!("/api/v1/links/{anonymous_code}/stats"),
                    Some(&original_token),
                    "",
                )
                .await
                .unwrap();
                assert_eq!(old_token.status, 401);
                let token_second = send(
                    b.address,
                    "GET",
                    &format!("/api/v1/links/{anonymous_code}/stats/breakdown?group_by=user_agent&limit=1&cursor={anonymous_cursor_a}"),
                    Some(&current_token),
                    "",
                )
                .await
                .unwrap();
                assert_eq!(token_second.status, 200);
                let token_second = json(&token_second);
                assert_eq!(token_second["items"][0]["value"], "B");
                let anonymous_cursor_b = token_second["next_cursor"].as_str().unwrap().to_owned();
                let token_report = send(
                    b.address,
                    "GET",
                    &format!("/api/v1/links/{anonymous_code}/stats"),
                    Some(&current_token),
                    "",
                )
                .await
                .unwrap();
                assert_eq!(token_report.status, 200);
                assert_eq!(json(&token_report)["lifetime_visits"], 4);

                // Individual disablement is reportable; owner suspension revokes
                // the key but leaves the link reportable by a live admin.
                let state_link = send(
                    a.address,
                    "POST",
                    "/api/v1/links",
                    Some(&current_key),
                    r#"{"destination_url":"https://state.report-canary.invalid/","short_code":"process-state"}"#,
                )
                .await
                .unwrap();
                assert_eq!(state_link.status, 201);
                let state_link_id = json(&state_link)["link"]["id"].as_str().unwrap().to_owned();
                let disabled = send(
                    a.address,
                    "PATCH",
                    "/api/v1/links/process-state",
                    Some(&current_key),
                    r#"{"enabled":false}"#,
                )
                .await
                .unwrap();
                assert_eq!(disabled.status, 200);
                let disabled_report = send(
                    b.address,
                    "GET",
                    "/api/v1/links/process-state/stats",
                    Some(&current_key),
                    "",
                )
                .await
                .unwrap();
                assert_eq!(disabled_report.status, 200);
                let suspended = send(
                    a.address,
                    "PATCH",
                    &format!("/api/v1/admin/accounts/{}", owner_id.as_uuid()),
                    Some(&admin_key),
                    r#"{"enabled":false}"#,
                )
                .await
                .unwrap();
                assert_eq!(suspended.status, 200);
                let owner_rejected = send(
                    b.address,
                    "GET",
                    "/api/v1/links/process-state/stats",
                    Some(&current_key),
                    "",
                )
                .await
                .unwrap();
                assert_eq!(owner_rejected.status, 401);
                let admin_report = send(
                    b.address,
                    "GET",
                    "/api/v1/links/process-state/stats",
                    Some(&admin_key),
                    "",
                )
                .await
                .unwrap();
                assert_eq!(admin_report.status, 200);
                let reenabled = send(
                    a.address,
                    "PATCH",
                    &format!("/api/v1/admin/accounts/{}", owner_id.as_uuid()),
                    Some(&admin_key),
                    r#"{"enabled":true}"#,
                )
                .await
                .unwrap();
                assert_eq!(reenabled.status, 200);

                let deleted = send(
                    a.address,
                    "DELETE",
                    "/api/v1/links/process-state",
                    Some(&current_key),
                    "",
                )
                .await
                .unwrap();
                assert_eq!(deleted.status, 204);
                let deleted_report = send(
                    b.address,
                    "GET",
                    "/api/v1/links/process-state/stats",
                    Some(&admin_key),
                    "",
                )
                .await
                .unwrap();
                assert_eq!(deleted_report.status, 404);

                let max_quote = format!("M{}", "\"".repeat(8_191));
                let max_utf8 = format!("M{}€", "é".repeat(4_094));
                assert_eq!(max_quote.len(), 8_192);
                assert_eq!(max_utf8.len(), 8_192);
                let quote_values = ["A", max_quote.as_str(), "N", "Z"];
                let utf8_values = ["A", max_utf8.as_str(), "N", "Z"];
                let quote_code = "max-quote";
                let utf8_code = "max-utf8";
                let mut max_link_ids = Vec::new();
                for code in [quote_code, utf8_code] {
                    let created = send(
                        a.address,
                        "POST",
                        "/api/v1/links",
                        Some(&current_key),
                        &format!(r#"{{"destination_url":"https://{code}.report-canary.invalid/","short_code":"{code}"}}"#),
                    )
                    .await
                    .unwrap();
                    assert_eq!(created.status, 201);
                    max_link_ids.push(json(&created)["link"]["id"].as_str().unwrap().to_owned());
                }
                for (code, values) in [(quote_code, &quote_values), (utf8_code, &utf8_values)] {
                    for value in values {
                        let visit = send_with_headers(
                            a.address,
                            "GET",
                            &format!("/{code}"),
                            None,
                            "",
                            &[("User-Agent", value)],
                        )
                        .await
                        .unwrap();
                        assert_eq!(visit.status, 302, "transport accepts exact 8,192-byte header");
                    }
                }
                for (code, values) in [(quote_code, &quote_values), (utf8_code, &utf8_values)] {
                    for value in values {
                        wait_for_user_agent(database.pool(), code, value).await;
                    }
                }
                let stored_quote: String = sqlx::query_scalar(
                    "SELECT user_agent FROM visits v JOIN links l ON l.id = v.link_id WHERE l.short_code = $1 AND user_agent = $2",
                )
                .bind(quote_code)
                .bind(&max_quote)
                .fetch_one(database.pool())
                .await
                .expect("the quote-rich header is captured exactly from production TCP");
                assert_eq!(stored_quote, max_quote);
                let stored_utf8: String = sqlx::query_scalar(
                    "SELECT user_agent FROM visits v JOIN links l ON l.id = v.link_id WHERE l.short_code = $1 AND user_agent = $2",
                )
                .bind(utf8_code)
                .bind(&max_utf8)
                .fetch_one(database.pool())
                .await
                .expect("the UTF-8 header is captured exactly from production TCP");
                assert_eq!(stored_utf8, max_utf8);

                let quote_first = send(
                    a.address,
                    "GET",
                    &format!("/api/v1/links/{quote_code}/stats/breakdown?group_by=user_agent&limit=2"),
                    Some(&current_key),
                    "",
                )
                .await
                .unwrap();
                assert_headers(&quote_first);
                assert_eq!(quote_first.status, 200);
                let quote_first = json(&quote_first);
                assert_eq!(quote_first["items"][0]["value"], "A");
                assert_eq!(quote_first["items"][1]["value"], max_quote);
                let quote_cursor_a = quote_first["next_cursor"].as_str().unwrap().to_owned();
                assert!(quote_cursor_a.len() > 16_000, "JSON quote escaping expands the boundary before sealing");
                assert!(quote_cursor_a.len() <= MAX_CURSOR_ENCODED_BYTES);
                let quote_second = send(
                    b.address,
                    "GET",
                    &format!("/api/v1/links/{quote_code}/stats/breakdown?group_by=user_agent&limit=1&cursor={quote_cursor_a}"),
                    Some(&current_key),
                    "",
                )
                .await
                .unwrap();
                assert_eq!(quote_second.status, 200);
                let quote_second = json(&quote_second);
                assert_eq!(quote_second["items"][0]["value"], "N");
                let quote_cursor_b = quote_second["next_cursor"].as_str().unwrap().to_owned();

                let utf8_first = send(
                    a.address,
                    "GET",
                    &format!("/api/v1/links/{utf8_code}/stats/breakdown?group_by=user_agent&limit=2"),
                    Some(&current_key),
                    "",
                )
                .await
                .unwrap();
                assert_eq!(utf8_first.status, 200);
                let utf8_first = json(&utf8_first);
                assert_eq!(utf8_first["items"][1]["value"], max_utf8);
                let utf8_cursor_a = utf8_first["next_cursor"].as_str().unwrap().to_owned();
                assert!(utf8_cursor_a.len() <= MAX_CURSOR_ENCODED_BYTES);
                let utf8_second = send(
                    b.address,
                    "GET",
                    &format!("/api/v1/links/{utf8_code}/stats/breakdown?group_by=user_agent&limit=1&cursor={utf8_cursor_a}"),
                    Some(&current_key),
                    "",
                )
                .await
                .unwrap();
                assert_eq!(utf8_second.status, 200);
                let utf8_second = json(&utf8_second);
                assert_eq!(utf8_second["items"][0]["value"], "N");
                let utf8_cursor_b = utf8_second["next_cursor"].as_str().unwrap().to_owned();
                let quote_target_bytes = format!(
                    "/api/v1/links/{quote_code}/stats/breakdown?group_by=user_agent&limit=1&cursor={quote_cursor_a}"
                )
                .len();
                let utf8_target_bytes = format!(
                    "/api/v1/links/{utf8_code}/stats/breakdown?group_by=user_agent&limit=1&cursor={utf8_cursor_a}"
                )
                .len();
                println!(
                    "REPORT_TCP_CURSOR raw_header_bytes=8192 quote_cursor_bytes={} quote_request_target_bytes={} utf8_cursor_bytes={} utf8_request_target_bytes={} codec_cap_bytes={MAX_CURSOR_ENCODED_BYTES}",
                    quote_cursor_a.len(),
                    quote_target_bytes,
                    utf8_cursor_a.len(),
                    utf8_target_bytes,
                );

                let a_output = a.stop_and_capture().await;
                let b_output = b.stop_and_capture().await;
                assert!(a_output.stdout.contains("request completed"));
                assert!(b_output.stdout.contains("request completed"));
                let owner_id_canary = owner_id.as_uuid().to_string();
                let canaries = [
                    database_url.as_str(),
                    original_key.as_str(),
                    current_key.as_str(),
                    original_token.as_str(),
                    current_token.as_str(),
                    anonymous_code.as_str(),
                    "process-report",
                    "REPORT_TITLE_PRIVATE_CANARY",
                    "REPORT_TAG_PRIVATE_CANARY",
                    "198.51.100.77",
                    "198.51.100.78",
                    "198.51.100.79",
                    "PROCESS_UA_PRIVATE_CANARY",
                    "PROCESS_UA_SECOND_CANARY",
                    "PROCESS_UA_THIRD_CANARY",
                    "REPORT_QUERY_PRIVATE_CANARY",
                    owner_id_canary.as_str(),
                    report_link_id.as_str(),
                    visit_id.as_str(),
                    anonymous_code.as_str(),
                    anonymous_link_id.as_str(),
                    state_link_id.as_str(),
                    max_link_ids[0].as_str(),
                    max_link_ids[1].as_str(),
                    "process-state",
                    "max-quote",
                    "max-utf8",
                    "https://history-A.report-canary.invalid/start?q=old#A",
                    "https://history-B.report-canary.invalid/next?q=new#B",
                    "https://history-a.report-canary.invalid/start?q=old#A",
                    "https://history-b.report-canary.invalid/next?q=new#B",
                    max_quote.as_str(),
                    max_utf8.as_str(),
                    quote_cursor_a.as_str(),
                    quote_cursor_b.as_str(),
                    utf8_cursor_a.as_str(),
                    utf8_cursor_b.as_str(),
                    visit_cursor_a.as_str(),
                    visit_cursor_b.as_str(),
                    anonymous_cursor_a.as_str(),
                    anonymous_cursor_b.as_str(),
                ];
                for output in [&a_output, &b_output] {
                    for canary in canaries {
                        assert!(!output.stdout.contains(canary), "stdout excludes canary {canary}");
                        assert!(!output.stderr.contains(canary), "stderr excludes canary {canary}");
                    }
                }

                // This fixture already owns the interprocess harness lock. Restart
                // only its verified named-volume container directly, then always
                // attempt recovery before checking the stop outcome.
                let stopped = podman(&["stop", "--time", "10", database_support::CONTAINER_NAME]).await;
                let started = podman(&["start", database_support::CONTAINER_NAME]).await;
                let ready = wait_postgres_ready().await;
                if !ready {
                    let retry = podman(&["start", database_support::CONTAINER_NAME]).await;
                    assert!(retry.is_some_and(|output| output.status.success()), "owned PostgreSQL recovery start succeeds");
                    assert!(wait_postgres_ready().await, "owned PostgreSQL becomes ready before cleanup deadline");
                }
                assert!(stopped.is_some_and(|output| output.status.success()), "owned PostgreSQL container stops");
                assert!(started.is_some_and(|output| output.status.success()), "named-volume PostgreSQL container restarts");
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM visits v JOIN links l ON l.id=v.link_id WHERE l.short_code IN ('process-report','max-quote','max-utf8') OR l.short_code=$1")
                        .bind(&anonymous_code)
                        .fetch_one(database.pool())
                        .await
                        .unwrap(),
                    15,
                    "reports and immutable event snapshots survive the owned volume restart"
                );

                let c = ServingProcess::start_with(
                    &database_url,
                    CURSOR_KEY,
                    &[("FERL_ANONYMOUS_CREATION", "false"), ("FERL_TRUSTED_PROXIES", "127.0.0.1")],
                )
                .await;
                let raw_page_c = send(
                    c.address,
                    "GET",
                    &format!("/api/v1/links/process-report/visits?limit=1&cursor={visit_cursor_b}"),
                    Some(&current_key),
                    "",
                )
                .await
                .unwrap();
                assert_eq!(raw_page_c.status, 200);
                let raw_page_c = json(&raw_page_c);
                assert_eq!(raw_page_c["items"].as_array().unwrap().len(), 1);
                let raw_url_c = raw_page_c["items"][0]["redirect_url"].as_str().unwrap();
                assert!(
                    [raw_url_a.as_str(), raw_url_b.as_str(), raw_url_c]
                        .contains(&"https://history-a.report-canary.invalid/start?q=old#A")
                        && [raw_url_a.as_str(), raw_url_b.as_str(), raw_url_c]
                            .contains(&"https://history-b.report-canary.invalid/next?q=new#B"),
                    "continued TCP report pages retain both historical destinations"
                );

                for (code, cursor) in [(quote_code, quote_cursor_b), (utf8_code, utf8_cursor_b)] {
                    let continued = send(
                        c.address,
                        "GET",
                        &format!("/api/v1/links/{code}/stats/breakdown?group_by=user_agent&limit=1&cursor={cursor}"),
                        Some(&current_key),
                        "",
                    )
                    .await
                    .unwrap();
                    assert_headers(&continued);
                    assert_eq!(continued.status, 200);
                    assert_eq!(json(&continued)["items"][0]["value"], "Z");
                    assert_eq!(json(&continued)["next_cursor"], Value::Null);
                }
                let after_restart_stats = send(
                    c.address,
                    "GET",
                    "/api/v1/links/process-report/stats",
                    Some(&current_key),
                    "",
                )
                .await
                .unwrap();
                assert_eq!(after_restart_stats.status, 200);
                assert_eq!(json(&after_restart_stats), json!({"lifetime_visits":3,"matching_visits":3}));
                let anonymous_after_restart = send(
                    c.address,
                    "GET",
                    &format!("/api/v1/links/{anonymous_code}/stats"),
                    Some(&current_token),
                    "",
                )
                .await
                .unwrap();
                assert_eq!(anonymous_after_restart.status, 200);
                assert_eq!(json(&anonymous_after_restart)["lifetime_visits"], 4);
                let old_token_after_restart = send(
                    c.address,
                    "GET",
                    &format!("/api/v1/links/{anonymous_code}/stats"),
                    Some(&original_token),
                    "",
                )
                .await
                .unwrap();
                assert_eq!(old_token_after_restart.status, 401);
                let token_third = send(
                    c.address,
                    "GET",
                    &format!("/api/v1/links/{anonymous_code}/stats/breakdown?group_by=user_agent&limit=1&cursor={anonymous_cursor_b}"),
                    Some(&current_token),
                    "",
                )
                .await
                .unwrap();
                assert_eq!(token_third.status, 200);
                let token_third = json(&token_third);
                assert_eq!(token_third["items"][0]["value"], "C");
                let anonymous_cursor_c = token_third["next_cursor"].as_str().unwrap().to_owned();
                let token_fourth = send(
                    c.address,
                    "GET",
                    &format!("/api/v1/links/{anonymous_code}/stats/breakdown?group_by=user_agent&limit=1&cursor={anonymous_cursor_c}"),
                    Some(&current_token),
                    "",
                )
                .await
                .unwrap();
                assert_eq!(token_fourth.status, 200);
                assert_eq!(json(&token_fourth)["items"][0]["value"], "D");
                c.stop().await;
            })
        })
        .await;
}

async fn wait_for_visit_count(pool: &PgPool, code: &str, expected: i64) {
    timeout(Duration::from_secs(12), async {
        loop {
            let count: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM visits v JOIN links l ON l.id = v.link_id WHERE l.short_code = $1",
            )
            .bind(code)
            .fetch_one(pool)
            .await
            .unwrap();
            if count == expected {
                return;
            }
            assert!(count < expected, "worker persisted no duplicate visits");
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("bounded workers persist visits before the deadline");
}

async fn wait_for_user_agent(pool: &PgPool, code: &str, value: &str) {
    timeout(Duration::from_secs(12), async {
        loop {
            let count: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM visits v JOIN links l ON l.id = v.link_id WHERE l.short_code = $1 AND v.user_agent = $2",
            )
            .bind(code)
            .bind(value)
            .fetch_one(pool)
            .await
            .unwrap();
            if count == 1 {
                return;
            }
            assert!(count < 1, "exact grouping header persisted once");
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("exact transport-captured header persists before the deadline");
}

async fn podman(arguments: &[&str]) -> Option<Output> {
    let mut command = Command::new("podman");
    command.kill_on_drop(true).args(arguments);
    timeout(Duration::from_secs(20), command.output())
        .await
        .ok()
        .and_then(Result::ok)
}

async fn wait_postgres_ready() -> bool {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let output = podman(&[
            "exec",
            database_support::CONTAINER_NAME,
            "pg_isready",
            "-q",
            "-t",
            "1",
            "-U",
            "ferl",
            "-d",
            "ferl_test",
        ])
        .await;
        if output.is_some_and(|output| output.status.success()) {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        sleep(Duration::from_millis(200)).await;
    }
}
