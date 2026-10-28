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
    io::Write,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};

use account_support::{migrated_database, request as api_request, seed_anonymous_link};
use axum::http::{HeaderValue, Method, StatusCode, header};
use ferl_server::{app, telemetry::logging};
use http_body_util::BodyExt;
use redirect_support::{call, request, state, wait_for_visits};
use tokio::time::{sleep, timeout};

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

fn diagnostics(capture: &Capture) -> String {
    String::from_utf8(capture.0.lock().unwrap().clone()).expect("production logs are UTF-8 JSON")
}

async fn wait_for_diagnostic(capture: &Capture, expected: &str) {
    timeout(Duration::from_secs(8), async {
        loop {
            if diagnostics(capture).contains(expected) {
                return;
            }
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("expected production diagnostic {expected} before deadline"));
}

#[tokio::test]
async fn queue_worker_parser_and_database_failures_never_fail_a_redirect() {
    let capture = production_logs();
    let database = migrated_database().await;
    database
        .verify(|database| {
            Box::pin(async move {
                let (link_id, token) =
                    seed_anonymous_link(database.pool(), "analytics-fault").await;
                let peer = SocketAddr::from(([192, 0, 2, 88], 40088));
                let destination_canary =
                    "https://destination.analytics-private-canary.invalid/path?q=secret#fragment";

                // No worker is started for this instance, so one bounded event
                // fits and the next is rejected immediately as queue-full.
                let full_state = state(
                    database.schema_database_url(),
                    &[("FERL_ANALYTICS_QUEUE_CAPACITY", "1")],
                );
                full_state.verify_schema().await.unwrap();
                let full_app = app::router(full_state.clone());
                let patched = account_support::call(
                    full_app.clone(),
                    api_request(
                        Method::PATCH,
                        "/api/v1/links/analytics-fault",
                        Some(&token),
                        &format!(r#"{{"destination_url":"{destination_canary}"}}"#),
                    ),
                )
                .await;
                assert_eq!(patched.status(), StatusCode::OK);
                let _ = account_support::json(patched).await;
                for expected in ["enqueued", "queue_full"] {
                    let response = call(
                        full_app.clone(),
                        request(Method::GET, "/analytics-fault", peer),
                    )
                    .await;
                    assert_eq!(response.status(), StatusCode::FOUND);
                    assert!(response.headers().contains_key(header::LOCATION));
                    assert!(
                        response
                            .into_body()
                            .collect()
                            .await
                            .unwrap()
                            .to_bytes()
                            .is_empty()
                    );
                    if expected == "queue_full" {
                        wait_for_diagnostic(&capture, expected).await;
                    }
                }
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM visits")
                        .fetch_one(database.pool())
                        .await
                        .unwrap(),
                    0,
                    "queue saturation is a best-effort drop, not a database write"
                );

                // A stopped/disconnected queue remains a successful redirect.
                let disconnected_state = state(database.schema_database_url(), &[]);
                disconnected_state.verify_schema().await.unwrap();
                let stopped_workers = disconnected_state.start_analytics_workers().unwrap();
                assert_eq!(
                    stopped_workers.shutdown(Duration::from_secs(2)).await,
                    ferl_server::analytics::WorkerStopOutcome::Drained
                );
                let disconnected_app = app::router(disconnected_state.clone());
                let disconnected = call(
                    disconnected_app.clone(),
                    request(Method::HEAD, "/analytics-fault", peer),
                )
                .await;
                assert_eq!(disconnected.status(), StatusCode::FOUND);
                assert!(
                    disconnected
                        .into_body()
                        .collect()
                        .await
                        .unwrap()
                        .to_bytes()
                        .is_empty()
                );
                wait_for_diagnostic(&capture, "disconnected").await;

                // Inject a transient PostgreSQL failure after successful
                // resolution. The background worker retries the same event;
                // neither the 302 nor its Location depends on the write.
                sqlx::query(
                    "CREATE FUNCTION fail_visit_insert() RETURNS trigger LANGUAGE plpgsql AS $$ \
                     BEGIN RAISE EXCEPTION 'private visit fault' USING ERRCODE = '40001'; END; $$",
                )
                .execute(database.pool())
                .await
                .unwrap();
                sqlx::query(
                    "CREATE TRIGGER fail_visit_insert BEFORE INSERT ON visits \
                     FOR EACH ROW EXECUTE FUNCTION fail_visit_insert()",
                )
                .execute(database.pool())
                .await
                .unwrap();

                let worker_state = state(
                    database.schema_database_url(),
                    &[
                        ("FERL_ANALYTICS_RETRY_BUDGET", "1"),
                        ("FERL_ANALYTICS_OPERATION_TIMEOUT_SECS", "10"),
                    ],
                );
                worker_state.verify_schema().await.unwrap();
                let worker_app = app::router(worker_state.clone());
                let workers = worker_state.start_analytics_workers().unwrap();
                let mut failed_write = request(
                    Method::GET,
                    "/analytics-fault?redirect-query-canary=private",
                    peer,
                );
                failed_write.headers_mut().insert(
                    header::REFERER,
                    HeaderValue::from_static(
                        "https://referrer.analytics-private-canary.invalid/secret?q=secret",
                    ),
                );
                failed_write.headers_mut().insert(
                    header::USER_AGENT,
                    HeaderValue::from_static("unrecognized-user-agent-private-canary"),
                );
                let response = call(worker_app.clone(), failed_write).await;
                assert_eq!(response.status(), StatusCode::FOUND);
                assert_eq!(response.headers()[header::LOCATION], destination_canary);
                assert!(
                    response
                        .into_body()
                        .collect()
                        .await
                        .unwrap()
                        .to_bytes()
                        .is_empty()
                );
                wait_for_diagnostic(&capture, "parser_unrecognized").await;
                wait_for_diagnostic(&capture, "persist_failed").await;

                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM visits WHERE link_id = $1")
                        .bind(link_id.as_uuid())
                        .fetch_one(database.pool())
                        .await
                        .unwrap(),
                    0
                );
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT visit_count FROM links WHERE id = $1")
                        .bind(link_id.as_uuid())
                        .fetch_one(database.pool())
                        .await
                        .unwrap(),
                    0
                );

                sqlx::query("DROP TRIGGER fail_visit_insert ON visits")
                    .execute(database.pool())
                    .await
                    .unwrap();
                sqlx::query("DROP FUNCTION fail_visit_insert()")
                    .execute(database.pool())
                    .await
                    .unwrap();
                let recovered = call(
                    worker_app.clone(),
                    request(Method::GET, "/analytics-fault", peer),
                )
                .await;
                assert_eq!(recovered.status(), StatusCode::FOUND);
                wait_for_visits(database.pool(), "analytics-fault", 1).await;
                assert_eq!(
                    workers.shutdown(Duration::from_secs(5)).await,
                    ferl_server::analytics::WorkerStopOutcome::Drained
                );

                let diagnostic_text = diagnostics(&capture);
                for required in [
                    "request completed",
                    "queue_full",
                    "enqueued",
                    "disconnected",
                    "parser_unrecognized",
                    "persist_failed",
                    "persisted",
                    "analytics.visit",
                    "queue_depth",
                    "database_transaction_aborted",
                ] {
                    assert!(
                        diagnostic_text.contains(required),
                        "missing safe diagnostic {required}"
                    );
                }
                tracing::info!(
                    target: "sqlx::query",
                    query = "SQL_DEPENDENCY_PRIVATE_CANARY",
                    parameters = "BODY_DEPENDENCY_PRIVATE_CANARY",
                    "dependency event must be filtered by production logger"
                );
                let diagnostic_text = diagnostics(&capture);
                assert!(!diagnostic_text.contains("SQL_DEPENDENCY_PRIVATE_CANARY"));
                assert!(!diagnostic_text.contains("BODY_DEPENDENCY_PRIVATE_CANARY"));
                for canary in [
                    "analytics-fault",
                    "redirect-query-canary",
                    destination_canary,
                    "referrer.analytics-private-canary.invalid",
                    "unrecognized-user-agent-private-canary",
                    token.as_str(),
                    "192.0.2.88",
                    database.schema_database_url(),
                ] {
                    assert!(
                        !diagnostic_text.contains(canary),
                        "private canary leaked: {canary}"
                    );
                }

                for state in [full_state, disconnected_state, worker_state] {
                    let pool = state.pool.clone();
                    drop(state);
                    timeout(Duration::from_secs(5), pool.close())
                        .await
                        .expect("production pool closes before schema cleanup");
                }
            })
        })
        .await;
}
