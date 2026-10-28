#[path = "support/accounts.rs"]
#[allow(dead_code)]
mod account_support;
#[path = "support/database.rs"]
#[allow(dead_code)]
mod database_support;
#[path = "support/redirects.rs"]
#[allow(dead_code)]
mod redirect_support;

use std::{net::SocketAddr, time::Duration};

use account_support::{migrated_database, seed_anonymous_link};
use axum::http::{HeaderValue, Method, StatusCode, header};
use ferl_server::{analytics::WorkerStopOutcome, app};
use http_body_util::BodyExt;
use redirect_support::{call, request, state, wait_for_visits};
use sqlx::Row;
use tokio::time::timeout;

const PEER: SocketAddr = SocketAddr::new(
    std::net::IpAddr::V4(std::net::Ipv4Addr::new(203, 0, 113, 42)),
    43000,
);

#[tokio::test]
async fn optional_headers_obey_complete_utf8_duplicate_and_byte_limit_rules() {
    let database = migrated_database().await;
    database
        .verify(|database| {
            Box::pin(async move {
                let (link_id, _) = seed_anonymous_link(database.pool(), "header-code").await;
                let state = state(database.schema_database_url(), &[]);
                state.verify_schema().await.unwrap();
                let app = app::router(state.clone());

                // UTF-8 is measured in bytes, not Unicode scalar values. Both
                // values are exactly 8,192 bytes and must be retained whole.
                let exact_referer = "é".repeat(4_096);
                let exact_agent = "a".repeat(8_192);
                assert_eq!(exact_referer.len(), 8_192);
                let mut exact = request(Method::GET, "/header-code", PEER);
                exact.headers_mut().insert(
                    header::REFERER,
                    HeaderValue::from_bytes(exact_referer.as_bytes()).unwrap(),
                );
                exact.headers_mut().insert(
                    header::USER_AGENT,
                    HeaderValue::from_bytes(exact_agent.as_bytes()).unwrap(),
                );
                exact.headers_mut().insert(
                    "x-forwarded-for",
                    HeaderValue::from_static("198.51.100.99"),
                );
                assert_eq!(call(app.clone(), exact).await.status(), StatusCode::FOUND);

                let mut duplicate_and_oversized = request(Method::GET, "/header-code", PEER);
                duplicate_and_oversized.headers_mut().append(
                    header::REFERER,
                    HeaderValue::from_static("https://one.invalid/"),
                );
                duplicate_and_oversized.headers_mut().append(
                    header::REFERER,
                    HeaderValue::from_static("https://two.invalid/"),
                );
                duplicate_and_oversized.headers_mut().insert(
                    header::USER_AGENT,
                    HeaderValue::from_bytes(&vec![b'u'; 8_193]).unwrap(),
                );
                assert_eq!(
                    call(app.clone(), duplicate_and_oversized).await.status(),
                    StatusCode::FOUND
                );

                let mut invalid_utf8 = request(Method::GET, "/header-code", PEER);
                invalid_utf8.headers_mut().insert(
                    header::REFERER,
                    HeaderValue::from_bytes(&[0xff, 0xfe]).unwrap(),
                );
                assert_eq!(
                    call(app.clone(), invalid_utf8).await.status(),
                    StatusCode::FOUND
                );

                let missing = request(Method::HEAD, "/header-code", PEER);
                let response = call(app.clone(), missing).await;
                assert_eq!(response.status(), StatusCode::FOUND);
                assert!(
                    response
                        .into_body()
                        .collect()
                        .await
                        .unwrap()
                        .to_bytes()
                        .is_empty()
                );

                let workers = state.start_analytics_workers().unwrap();
                assert_eq!(
                    workers.shutdown(Duration::from_secs(5)).await,
                    WorkerStopOutcome::Drained,
                    "bounded graceful shutdown drains already-buffered events"
                );
                wait_for_visits(database.pool(), "header-code", 4).await;
                let rows = sqlx::query(
                    "SELECT method, host(client_ip) AS client_ip, referrer, user_agent, device, browser, os \
                     FROM visits WHERE link_id = $1 ORDER BY id",
                )
                .bind(link_id.as_uuid())
                .fetch_all(database.pool())
                .await
                .unwrap();
                assert_eq!(rows.len(), 4);
                let exact_row = rows
                    .iter()
                    .find(|row| {
                        row.try_get::<Option<String>, _>("referrer")
                            .unwrap()
                            .as_deref()
                            == Some(exact_referer.as_str())
                    })
                    .expect("the complete exact-byte referrer is persisted");
                assert_eq!(
                    exact_row
                        .try_get::<Option<String>, _>("user_agent")
                        .unwrap()
                        .as_deref(),
                    Some(exact_agent.as_str())
                );
                assert_eq!(
                    exact_row.try_get::<String, _>("client_ip").unwrap(),
                    "203.0.113.42",
                    "spoofed XFF is ignored when the transport peer is untrusted"
                );
                assert_eq!(exact_row.try_get::<String, _>("device").unwrap(), "unknown");
                assert_eq!(
                    exact_row.try_get::<String, _>("browser").unwrap(),
                    "unknown"
                );
                assert_eq!(exact_row.try_get::<String, _>("os").unwrap(), "unknown");

                let unusable = rows
                    .iter()
                    .filter(|row| {
                        row.try_get::<Option<String>, _>("referrer")
                            .unwrap()
                            .is_none()
                            && row
                                .try_get::<Option<String>, _>("user_agent")
                                .unwrap()
                                .is_none()
                    })
                    .count();
                assert_eq!(
                    unusable, 3,
                    "duplicate, invalid, oversized and missing values become null"
                );
                assert!(
                    rows.iter()
                        .any(|row| { row.try_get::<String, _>("method").unwrap() == "HEAD" })
                );
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT visit_count FROM links WHERE id = $1")
                        .bind(link_id.as_uuid())
                        .fetch_one(database.pool())
                        .await
                        .unwrap(),
                    4
                );

                let pool = state.pool.clone();
                drop(app);
                drop(state);
                timeout(Duration::from_secs(5), pool.close())
                    .await
                    .expect("production pool closes before schema cleanup");
            })
        })
        .await;
}
