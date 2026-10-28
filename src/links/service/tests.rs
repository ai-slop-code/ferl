use std::{
    collections::{BTreeMap, VecDeque},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use axum::{
    body::Body,
    extract::ConnectInfo,
    http::{Method, Request, StatusCode},
};
use tokio::time::timeout;

use crate::{
    app::AppState,
    config::{Config, MigrationConfig},
    database_support::IsolatedTestDatabase,
    db::migrate,
    domain::short_code::{RandomnessError, ShortCode},
};

use super::{super::allocation::CandidateSource, LinkFailure, new_link_id_with};

#[path = "../../../tests/support/openapi.rs"]
mod openapi;

struct Sequence {
    values: Mutex<VecDeque<Result<&'static str, ()>>>,
    calls: AtomicUsize,
}

impl CandidateSource for Sequence {
    fn next_candidate(&self) -> Result<ShortCode, RandomnessError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        let value = self
            .values
            .lock()
            .expect("candidate fixture mutex is not poisoned")
            .pop_front()
            .unwrap_or(Err(()));
        value
            .map_err(|()| RandomnessError)
            .and_then(|code| ShortCode::parse_custom(code).map_err(|_| RandomnessError))
    }
}

#[test]
fn link_identifier_randomness_failure_is_typed_instead_of_panicking() {
    assert!(matches!(
        new_link_id_with(|_| Err(())),
        Err(LinkFailure::Internal)
    ));
    let identifier = new_link_id_with(|bytes| {
        bytes.fill(0x5a);
        Ok(())
    })
    .expect("a successful OS-randomness seam creates a version-4 link ID");
    assert_eq!(
        identifier.as_uuid().get_version(),
        Some(uuid::Version::Random)
    );
}

fn config(database_url: &str) -> Config {
    Config::from_map(BTreeMap::from([
        ("FERL_DATABASE_URL".to_owned(), database_url.to_owned()),
        (
            "FERL_PUBLIC_BASE_URL".to_owned(),
            "https://links.example.test".to_owned(),
        ),
        (
            "FERL_CURSOR_KEY".to_owned(),
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".to_owned(),
        ),
        ("FERL_ANONYMOUS_CREATION".to_owned(), "true".to_owned()),
        ("FERL_DB_POOL_MAX".to_owned(), "1".to_owned()),
        ("FERL_ANONYMOUS_CREATE_LIMIT".to_owned(), "10".to_owned()),
    ]))
    .expect("the isolated production configuration is valid")
}

fn anonymous_request(body: &str) -> Request<Body> {
    let mut request = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/links")
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_owned()))
        .expect("fixture request is valid");
    request.extensions_mut().insert(ConnectInfo(
        "192.0.2.201:1234"
            .parse::<std::net::SocketAddr>()
            .expect("fixture peer is a socket address"),
    ));
    request
}

#[tokio::test]
async fn generated_allocation_retries_exactly_ten_without_orphan_rows_or_extra_charge() {
    let database = IsolatedTestDatabase::connect_from_harness().await;
    database
        .verify_owned(|fixture_pool, schema_url| {
            Box::pin(async move {
                let migration = MigrationConfig::from_map(BTreeMap::from([(
                    "FERL_DATABASE_URL".to_owned(),
                    schema_url.clone(),
                )]))
                .expect("isolated migration profile is valid");
                migrate::run_migrations(&migration)
                    .await
                    .expect("isolated schema migrates");
                let mut state = AppState::from_config(&config(&schema_url))
                    .expect("production state composes with one connection");
                state.verify_schema().await.expect("schema verifies");
                let pool = state.pool.clone();

                sqlx::query("INSERT INTO code_reservations (short_code) VALUES ('collision-one')")
                    .execute(&fixture_pool)
                    .await
                    .expect("first generated candidate is already reserved");
                let sequence = Arc::new(Sequence {
                    values: Mutex::new([Ok("collision-one"), Ok("generated-ok")].into()),
                    calls: AtomicUsize::new(0),
                });
                state.links.candidates = sequence.clone();
                let application = crate::app::router(state.clone());
                let created = crate::account_support::call(
                    application.clone(),
                    anonymous_request(
                        r#"{"destination_url":"https://allocation.example/","tags":["one","Two"]}"#,
                    ),
                )
                .await;
                assert_eq!(created.status(), StatusCode::CREATED);
                let headers = created.headers().clone();
                let created = crate::account_support::json(created).await;
                assert!(openapi::validates("LinkCreation", &created));
                assert!(openapi::validates_response(
                    "LinkCreated",
                    &headers,
                    Some(&created),
                    false
                ));
                assert_eq!(
                    created["link"]["short_code"],
                    "generated-ok"
                );
                assert_eq!(sequence.calls.load(Ordering::Relaxed), 2);
                drop(application);
                let codes: Vec<String> = sqlx::query_scalar(
                    "SELECT short_code FROM code_reservations WHERE short_code <> 'api' ORDER BY short_code",
                )
                .fetch_all(&fixture_pool)
                .await
                .expect("reserved codes are readable");
                assert_eq!(codes, ["collision-one", "generated-ok"]);
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM links")
                        .fetch_one(&fixture_pool)
                        .await
                        .unwrap(),
                    1
                );
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM link_tags")
                        .fetch_one(&fixture_pool)
                        .await
                        .unwrap(),
                    2
                );
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM rate_charges")
                        .fetch_one(&fixture_pool)
                        .await
                        .unwrap(),
                    1,
                    "internal candidate retries consume only the committed attempt charge"
                );

                let exhausted_codes = [
                    "exhaust-001",
                    "exhaust-002",
                    "exhaust-003",
                    "exhaust-004",
                    "exhaust-005",
                    "exhaust-006",
                    "exhaust-007",
                    "exhaust-008",
                    "exhaust-009",
                    "exhaust-010",
                ];
                for code in exhausted_codes {
                    sqlx::query("INSERT INTO code_reservations (short_code) VALUES ($1)")
                        .bind(code)
                        .execute(&fixture_pool)
                        .await
                        .expect("each bounded candidate is already reserved");
                }
                let sequence = Arc::new(Sequence {
                    values: Mutex::new(exhausted_codes.map(Ok).into()),
                    calls: AtomicUsize::new(0),
                });
                state.links.candidates = sequence.clone();
                let application = crate::app::router(state.clone());
                let failure = crate::account_support::call(
                    application.clone(),
                    anonymous_request(
                        r#"{"destination_url":"https://exhaustion.example/","tags":["must-rollback"]}"#,
                    ),
                )
                .await;
                assert_eq!(failure.status(), StatusCode::SERVICE_UNAVAILABLE);
                let headers = failure.headers().clone();
                let failure = crate::account_support::json(failure).await;
                assert!(openapi::validates_response(
                    "Unavailable",
                    &headers,
                    Some(&failure),
                    false
                ));
                assert_eq!(
                    failure["error"]["code"],
                    "code_generation_unavailable"
                );
                assert_eq!(sequence.calls.load(Ordering::Relaxed), 10);
                drop(application);
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM links")
                        .fetch_one(&fixture_pool)
                        .await
                        .unwrap(),
                    1
                );
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM link_tags")
                        .fetch_one(&fixture_pool)
                        .await
                        .unwrap(),
                    2,
                    "failed allocation leaves no tag rows"
                );
                assert_eq!(
                    sqlx::query_scalar::<_, i64>(
                        "SELECT count(*) FROM code_reservations WHERE short_code LIKE 'exhaust-%'",
                    )
                    .fetch_one(&fixture_pool)
                    .await
                    .unwrap(),
                    10,
                    "only reservations that existed before this failed attempt remain"
                );
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM rate_charges")
                        .fetch_one(&fixture_pool)
                        .await
                        .unwrap(),
                    2
                );

                let failing = Arc::new(Sequence {
                    values: Mutex::new([Err(())].into()),
                    calls: AtomicUsize::new(0),
                });
                state.links.candidates = failing.clone();
                let application = crate::app::router(state.clone());
                let failed = crate::account_support::call(
                    application.clone(),
                    anonymous_request(r#"{"destination_url":"https://randomness.example/"}"#),
                )
                .await;
                assert_eq!(failed.status(), StatusCode::SERVICE_UNAVAILABLE);
                let headers = failed.headers().clone();
                let failed = crate::account_support::json(failed).await;
                assert!(openapi::validates_response(
                    "Unavailable",
                    &headers,
                    Some(&failed),
                    false
                ));
                assert_eq!(
                    failed["error"]["code"],
                    "code_generation_unavailable"
                );
                assert_eq!(failing.calls.load(Ordering::Relaxed), 1);
                drop(application);
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM links")
                        .fetch_one(&fixture_pool)
                        .await
                        .unwrap(),
                    1
                );
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM rate_charges")
                        .fetch_one(&fixture_pool)
                        .await
                        .unwrap(),
                    3
                );

                drop(state);
                timeout(Duration::from_secs(5), pool.close())
                    .await
                    .expect("production pool closes before schema cleanup");
            })
        })
        .await;
}
