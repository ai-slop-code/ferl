#[path = "support/accounts.rs"]
#[allow(dead_code)]
mod account_support;
#[path = "support/commit_proxy.rs"]
#[allow(dead_code)]
mod commit_proxy;
#[path = "support/database.rs"]
mod database_support;
#[path = "support/openapi.rs"]
mod openapi;

use account_support::{
    bytes, call, json, migrated_database, production_router_with, request, seed_account,
};
use axum::{
    Router,
    body::Body,
    extract::ConnectInfo,
    http::{Method, Request, StatusCode},
};
use ferl_server::{app, app::AppState, config::Config};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    convert::Infallible,
    io::Write,
    net::SocketAddr,
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};
use tokio::{task::JoinSet, time::timeout};
use tower::ServiceExt;
use uuid::Uuid;

#[derive(Clone)]
struct Capture(Arc<Mutex<Vec<u8>>>);

static PRODUCTION_LOGS: OnceLock<Capture> = OnceLock::new();

fn production_capture() -> Capture {
    PRODUCTION_LOGS
        .get_or_init(|| {
            let capture = Capture(Arc::new(Mutex::new(Vec::new())));
            let writer = capture.clone();
            ferl_server::telemetry::logging::init_with_writer(move || writer.clone());
            capture
        })
        .clone()
}

fn digest_hex(secret: &str) -> String {
    ferl_server::domain::secret::SecretDigest::from_presented_secret(secret)
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn bytes_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

impl Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn anonymous_request(body: Body, content_type: &str) -> Request<Body> {
    let mut request = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/links")
        .header("content-type", content_type)
        .header("x-request-id", "client-value")
        .body(body)
        .expect("test request is valid");
    request
        .extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([192, 0, 2, 93], 4567))));
    request
}

fn router_for_url(database_url: &str, extra: &[(&str, &str)]) -> Router {
    let mut values = BTreeMap::from([
        ("FERL_DATABASE_URL".to_owned(), database_url.to_owned()),
        (
            "FERL_PUBLIC_BASE_URL".to_owned(),
            "https://short.privacy-canary.invalid".to_owned(),
        ),
        (
            "FERL_CURSOR_KEY".to_owned(),
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".to_owned(),
        ),
        ("FERL_DB_POOL_MAX".to_owned(), "1".to_owned()),
        ("FERL_DB_LOCK_TIMEOUT_SECS".to_owned(), "5".to_owned()),
    ]);
    for (name, value) in extra {
        values.insert((*name).to_owned(), (*value).to_owned());
    }
    let config = Config::from_map(values).expect("test production configuration is valid");
    app::router(AppState::from_config(&config).expect("production state composes"))
}

async fn wait_for_activity_query(pool: &sqlx::PgPool, pattern: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
    loop {
        let waiting: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE datname = current_database() \
             AND wait_event_type = 'Lock' AND query LIKE $1)",
        )
        .bind(pattern)
        .fetch_one(pool)
        .await
        .expect("test observes the intended PostgreSQL lock wait");
        if waiting {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the intended query did not enter a lock wait: {pattern}"
        );
        tokio::task::yield_now().await;
    }
}

async fn assert_error(
    response: axum::response::Response,
    status: StatusCode,
    code: &str,
    component: &str,
) -> Value {
    assert_eq!(response.status(), status);
    let headers = response.headers().clone();
    assert_eq!(headers["cache-control"], "no-store");
    assert_eq!(headers["content-type"], "application/json");
    let request_id = headers["x-request-id"].to_str().unwrap();
    assert_ne!(request_id, "client-value");
    Uuid::parse_str(request_id).unwrap();
    if status == StatusCode::UNAUTHORIZED {
        assert_eq!(headers["www-authenticate"], "Bearer");
    }
    if status == StatusCode::TOO_MANY_REQUESTS {
        assert!(
            headers["retry-after"]
                .to_str()
                .unwrap()
                .parse::<u32>()
                .unwrap()
                > 0
        );
    }
    let response: Value = json(response).await;
    assert_eq!(response["request_id"], request_id);
    assert_eq!(response["error"]["code"], code);
    assert!(openapi::validates_response(
        component,
        &headers,
        Some(&response),
        false
    ));
    response
}

#[tokio::test]
async fn actual_link_http_contract_storage_failures_and_production_diagnostics_are_private() {
    let capture = production_capture();

    let database = migrated_database().await;
    database
        .verify(|database| {
            Box::pin(async move {
                let (_admin_id, admin_key) =
                    seed_account(database.pool(), "privacy_admin_canary", "admin", true).await;
                let (owner_id, owner_key) =
                    seed_account(database.pool(), "privacy_owner_canary", "user", true).await;
                let (_, unrelated_key) =
                    seed_account(database.pool(), "privacy_unrelated_canary", "user", true).await;
                let application = production_router_with(
                    database,
                    &[
                        ("FERL_ANONYMOUS_CREATION", "true"),
                        ("FERL_ANONYMOUS_CREATE_LIMIT", "1"),
                    ],
                )
                .await;
                let mut canaries = vec![
                    database.schema_database_url().to_owned(),
                    owner_key.clone(),
                    digest_hex(&owner_key),
                    admin_key.clone(),
                    unrelated_key.clone(),
                    "privacy_admin_canary".to_owned(),
                    "privacy_owner_canary".to_owned(),
                    "privacy_unrelated_canary".to_owned(),
                    "code-privacy-canary".to_owned(),
                    "destination_privacy_canary.invalid".to_owned(),
                    "tag_privacy_canary".to_owned(),
                    "TITLE_PRIVACY_CANARY".to_owned(),
                    "TAG_PRIVACY_CANARY".to_owned(),
                    "DESTINATION_PRIVACY_CANARY".to_owned(),
                    "STORED_URL_PRIVACY_CANARY".to_owned(),
                    "BODY_PRIVACY_CANARY".to_owned(),
                    "SQL_DEPENDENCY_PRIVACY_CANARY".to_owned(),
                    "192.0.2.93".to_owned(),
                    "privacy-token".to_owned(),
                ];

                let owned = call(
                    application.clone(),
                    request(
                        Method::POST,
                        "/api/v1/links",
                        Some(&owner_key),
                        r#"{"destination_url":"https://DESTINATION_PRIVACY_CANARY.invalid/path","short_code":"code-privacy-canary","title":"TITLE_PRIVACY_CANARY","tags":["TAG_PRIVACY_CANARY"]}"#,
                    ),
                )
                .await;
                assert_eq!(owned.status(), StatusCode::CREATED);
                let owned_headers = owned.headers().clone();
                let owned = json(owned).await;
                canaries.push(owned["link"]["id"].as_str().unwrap().to_owned());
                assert!(openapi::validates("LinkCreation", &owned));
                assert!(openapi::validates_response(
                    "LinkCreated",
                    &owned_headers,
                    Some(&owned),
                    false
                ));
                assert!(owned.get("management_token").is_none());

                let anonymous = call(
                    application.clone(),
                    anonymous_request(
                        Body::from(
                            r#"{"destination_url":"https://anonymous-privacy.invalid/","title":"TITLE_PRIVACY_CANARY","tags":["TAG_PRIVACY_CANARY"]}"#,
                        ),
                        "application/json",
                    ),
                )
                .await;
                assert_eq!(anonymous.status(), StatusCode::CREATED);
                let anonymous_headers = anonymous.headers().clone();
                let anonymous = json(anonymous).await;
                canaries.push(anonymous["link"]["id"].as_str().unwrap().to_owned());
                assert!(openapi::validates("LinkCreation", &anonymous));
                assert!(openapi::validates_response(
                    "LinkCreated",
                    &anonymous_headers,
                    Some(&anonymous),
                    false
                ));
                let management_token = anonymous["management_token"]
                    .as_str()
                    .unwrap()
                    .to_owned();
                canaries.push(management_token.clone());
                canaries.push(digest_hex(&management_token));
                canaries.push(owner_id.as_uuid().to_string());
                for id in sqlx::query_scalar::<_, String>(
                    "SELECT id::text FROM rate_buckets UNION ALL SELECT id::text FROM rate_charges",
                )
                .fetch_all(database.pool())
                .await
                .unwrap()
                {
                    canaries.push(id);
                }
                let code = owned["link"]["short_code"].as_str().unwrap();
                let detail = call(
                    application.clone(),
                    request(
                        Method::GET,
                        &format!("/api/v1/links/{code}"),
                        Some(&owner_key),
                        "",
                    ),
                )
                .await;
                assert_eq!(detail.status(), StatusCode::OK);
                let detail_headers = detail.headers().clone();
                let detail = json(detail).await;
                assert!(openapi::validates_response(
                    "Link",
                    &detail_headers,
                    Some(&detail),
                    false
                ));
                assert!(detail.get("management_token").is_none());
                let mut bad_headers = detail_headers.clone();
                bad_headers.remove("cache-control");
                assert!(!openapi::validates_response(
                    "Link",
                    &bad_headers,
                    Some(&detail),
                    false
                ));
                let mut malformed_link = detail.clone();
                malformed_link["title"] = Value::Null;
                malformed_link.as_object_mut().unwrap().remove("owner_id");
                assert!(!openapi::validates("Link", &malformed_link));

                let forbidden = call(
                    application.clone(),
                    request(
                        Method::POST,
                        "/api/v1/links",
                        Some(&management_token),
                        "BODY_PRIVACY_CANARY",
                    ),
                )
                .await;
                assert_error(
                    forbidden,
                    StatusCode::FORBIDDEN,
                    "forbidden",
                    "Forbidden",
                )
                .await;
                let invalid = call(
                    application.clone(),
                    request(
                        Method::GET,
                        "/api/v1/links/code-privacy-canary",
                        Some("invalid-privacy-credential"),
                        "",
                    ),
                )
                .await;
                assert_error(
                    invalid,
                    StatusCode::UNAUTHORIZED,
                    "invalid_credentials",
                    "Unauthorized",
                )
                .await;
                canaries.push("invalid-privacy-credential".to_owned());
                let concealed = call(
                    application.clone(),
                    request(
                        Method::GET,
                        "/api/v1/links/code-privacy-canary",
                        Some(&unrelated_key),
                        "",
                    ),
                )
                .await;
                assert_error(concealed, StatusCode::NOT_FOUND, "not_found", "NotFound").await;

                let invalid_body = call(
                    application.clone(),
                    request(
                        Method::POST,
                        "/api/v1/links",
                        Some(&owner_key),
                        r#"{"destination_url":"https://invalid-body.invalid/","unknown":"BODY_PRIVACY_CANARY"}"#,
                    ),
                )
                .await;
                assert_error(
                    invalid_body,
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    "BadRequest",
                )
                .await;

                let conflict = call(
                    application.clone(),
                    request(
                        Method::POST,
                        "/api/v1/links",
                        Some(&owner_key),
                        r#"{"destination_url":"https://conflict.invalid/","short_code":"code-privacy-canary"}"#,
                    ),
                )
                .await;
                assert_error(
                    conflict,
                    StatusCode::CONFLICT,
                    "short_code_unavailable",
                    "Conflict",
                )
                .await;

                let rate_limited = call(
                    application.clone(),
                    anonymous_request(
                        Body::from(r#"{"destination_url":"https://rate-limited.invalid/"}"#),
                        "application/json",
                    ),
                )
                .await;
                assert_error(
                    rate_limited,
                    StatusCode::TOO_MANY_REQUESTS,
                    "rate_limited",
                    "RateLimited",
                )
                .await;

                let invalid_query = call(
                    application.clone(),
                    request(
                        Method::POST,
                        "/api/v1/links?unknown=BODY_PRIVACY_CANARY",
                        Some(&owner_key),
                        r#"{"destination_url":"https://query.invalid/"}"#,
                    ),
                )
                .await;
                assert_error(
                    invalid_query,
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    "BadRequest",
                )
                .await;

                let method = call(
                    application.clone(),
                    request(Method::HEAD, "/api/v1/links", None, ""),
                )
                .await;
                assert_eq!(method.status(), StatusCode::METHOD_NOT_ALLOWED);
                assert_eq!(method.headers()["allow"], "POST");
                assert_eq!(method.headers()["cache-control"], "no-store");
                assert!(openapi::validates_response(
                    "MethodNotAllowed",
                    method.headers(),
                    None,
                    true
                ));
                assert!(bytes(method).await.is_empty());
                let detail_head = call(
                    application.clone(),
                    request(Method::HEAD, "/api/v1/links/code-privacy-canary", None, ""),
                )
                .await;
                assert_eq!(detail_head.status(), StatusCode::METHOD_NOT_ALLOWED);
                assert_eq!(detail_head.headers()["allow"], "GET, PATCH, DELETE");
                assert!(openapi::validates_response(
                    "MethodNotAllowed",
                    detail_head.headers(),
                    None,
                    true
                ));
                assert!(bytes(detail_head).await.is_empty());
                let unknown = call(
                    application.clone(),
                    request(Method::GET, "/api/v1/links/a/visits", None, ""),
                )
                .await;
                assert_error(unknown, StatusCode::NOT_FOUND, "not_found", "NotFound").await;

                // A failed new creation transaction leaves its independently
                // acknowledged allowance but none of its reservation/link/tags.
                sqlx::query(
                    "CREATE FUNCTION link_test_insert_fault() RETURNS trigger LANGUAGE plpgsql AS $$ \
                     BEGIN RAISE EXCEPTION 'DB_BODY_PRIVACY_CANARY'; END $$",
                )
                .execute(database.pool())
                .await
                .unwrap();
                sqlx::query(
                    "CREATE TRIGGER link_test_insert_fault BEFORE INSERT ON links FOR EACH ROW EXECUTE FUNCTION link_test_insert_fault()",
                )
                .execute(database.pool())
                .await
                .unwrap();
                let charge_count: i64 = sqlx::query_scalar("SELECT count(*) FROM rate_charges")
                    .fetch_one(database.pool())
                    .await
                    .unwrap();
                let before_links: i64 = sqlx::query_scalar("SELECT count(*) FROM links")
                    .fetch_one(database.pool())
                    .await
                    .unwrap();
                let failed = call(
                    application.clone(),
                    request(
                        Method::POST,
                        "/api/v1/links",
                        Some(&owner_key),
                        r#"{"destination_url":"https://persistence-fault.invalid/","short_code":"fault-code","tags":["fault-tag"]}"#,
                    ),
                )
                .await;
                assert_error(
                    failed,
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal_error",
                    "InternalError",
                )
                .await;
                sqlx::query("DROP TRIGGER link_test_insert_fault ON links")
                    .execute(database.pool())
                    .await
                    .unwrap();
                sqlx::query("DROP FUNCTION link_test_insert_fault()")
                    .execute(database.pool())
                    .await
                    .unwrap();
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM rate_charges")
                        .fetch_one(database.pool())
                        .await
                        .unwrap(),
                    charge_count + 1
                );
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM links")
                        .fetch_one(database.pool())
                        .await
                        .unwrap(),
                    before_links
                );
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM code_reservations WHERE short_code = 'fault-code'")
                        .fetch_one(database.pool())
                        .await
                        .unwrap(),
                    0
                );
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM link_tags WHERE name = 'fault-tag'")
                        .fetch_one(database.pool())
                        .await
                        .unwrap(),
                    0
                );

                // Decode infinity through the exact link-row decoder. The
                // request fails safely with 500 rather than panicking in Chrono.
                sqlx::query("INSERT INTO code_reservations (short_code) VALUES ('timestamp-corrupt')")
                    .execute(database.pool())
                    .await
                    .unwrap();
                let link_id = Uuid::new_v4();
                sqlx::query(
                    "INSERT INTO links (id, short_code, owner_id, destination_url, destination_search, enabled, visit_count, created_at, updated_at) \
                     VALUES ($1, 'timestamp-corrupt', (SELECT id FROM accounts WHERE api_key_digest = $2), \
                     'https://time-corrupt.invalid/', 'https://time-corrupt.invalid/', true, 0, 'infinity', clock_timestamp())",
                )
                .bind(link_id)
                .bind(
                    ferl_server::domain::secret::SecretDigest::from_presented_secret(&owner_key)
                        .as_bytes()
                        .as_slice(),
                )
                .execute(database.pool())
                .await
                .unwrap();
                let corrupt = call(
                    application.clone(),
                    request(
                        Method::GET,
                        "/api/v1/links/timestamp-corrupt",
                        Some(&owner_key),
                        "",
                    ),
                )
                .await;
                assert_error(
                    corrupt,
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal_error",
                    "InternalError",
                )
                .await;
                canaries.push("DB_BODY_PRIVACY_CANARY".to_owned());
                canaries.push("https://time-corrupt.invalid/".to_owned());
                canaries.push(link_id.to_string());

                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let closed_port = listener.local_addr().unwrap().port();
                drop(listener);
                let mut unavailable_url = url::Url::parse(database.schema_database_url()).unwrap();
                unavailable_url.set_port(Some(closed_port)).unwrap();
                unavailable_url
                    .set_username("db-url-user-canary")
                    .unwrap();
                unavailable_url
                    .set_password(Some("db-url-password-canary"))
                    .unwrap();
                canaries.push(unavailable_url.to_string());
                canaries.push("db-url-user-canary".to_owned());
                canaries.push("db-url-password-canary".to_owned());
                let unavailable = router_for_url(
                    unavailable_url.as_str(),
                    &[("FERL_DB_ACQUIRE_TIMEOUT_SECS", "1")],
                );
                let outage = call(
                    unavailable,
                    request(
                        Method::GET,
                        "/api/v1/links/code-privacy-canary",
                        Some(&owner_key),
                        "",
                    ),
                )
                .await;
                assert_error(
                    outage,
                    StatusCode::SERVICE_UNAVAILABLE,
                    "storage_unavailable",
                    "Unavailable",
                )
                .await;

                // Dependency-target suppression is part of production logging,
                // not a test-capture filter.
                tracing::info!(
                    target: "sqlx::query",
                    sql = "SELECT SQL_DEPENDENCY_PRIVACY_CANARY",
                    parameters = "BODY_PRIVACY_CANARY",
                    "deliberately verbose dependency event"
                );

                drop(application);
                let diagnostics =
                    String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
                for category in [
                    "request completed",
                    "invalid_credentials",
                    "link_persistence_failure",
                    "short_code_conflict",
                    "link_authentication_storage_unavailable",
                ] {
                    assert!(
                        diagnostics.contains(category),
                        "safe diagnostic was not emitted before privacy scan: {category}"
                    );
                }
                for canary in canaries {
                    assert!(
                        !diagnostics.contains(&canary),
                        "private canary leaked in actual production diagnostics"
                    );
                }
                for line in diagnostics.lines() {
                    serde_json::from_str::<Value>(line).expect("production output is JSON");
                }
            })
        })
        .await;
}

#[tokio::test]
async fn creation_and_token_commit_acknowledgement_uncertainty_is_not_replayed() {
    let capture = production_capture();
    let database = migrated_database().await;
    database
        .verify(|database| {
            Box::pin(async move {
                let (_, owner_key) =
                    seed_account(database.pool(), "uncertain_link_owner", "user", true).await;
                let mut canaries = vec![
                    owner_key.clone(),
                    digest_hex(&owner_key),
                    database.schema_database_url().to_owned(),
                    "https://aborted-commit.invalid/".to_owned(),
                    "https://durable-lost-ack.invalid/".to_owned(),
                    "https://rotation-uncertain.invalid/".to_owned(),
                ];

                // Abort a creation COMMIT while its deferred PostgreSQL trigger
                // is blocked. This is not a durable lost acknowledgement.
                sqlx::query(
                    "CREATE FUNCTION link_commit_hold() RETURNS trigger LANGUAGE plpgsql AS $$ \
                     BEGIN PERFORM pg_advisory_xact_lock(715306); RETURN NEW; END $$",
                )
                .execute(database.pool())
                .await
                .unwrap();
                sqlx::query(
                    "CREATE CONSTRAINT TRIGGER link_commit_hold AFTER INSERT ON links DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION link_commit_hold()",
                )
                .execute(database.pool())
                .await
                .unwrap();
                let mut blocker = database.pool().begin().await.unwrap();
                sqlx::query("SELECT pg_advisory_xact_lock(715306)")
                    .execute(&mut *blocker)
                    .await
                    .unwrap();
                let app = production_router_with(database, &[]).await;
                let mut requests = JoinSet::new();
                requests.spawn(async move {
                    call(
                        app,
                        request(
                            Method::POST,
                            "/api/v1/links",
                            Some(&owner_key),
                            r#"{"destination_url":"https://aborted-commit.invalid/","short_code":"aborted-commit"}"#,
                        ),
                    )
                    .await
                });
                wait_for_activity_query(database.pool(), "COMMIT%")
                    .await;
                let holder_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
                    .fetch_one(&mut *blocker)
                    .await
                    .unwrap();
                let backend: i32 = sqlx::query_scalar(
                    "SELECT pid FROM pg_stat_activity WHERE datname = current_database() \
                     AND pid <> pg_backend_pid() AND query LIKE 'COMMIT%' \
                     AND $1 = ANY(pg_blocking_pids(pid)) LIMIT 1",
                )
                .bind(holder_pid)
                .fetch_one(database.pool())
                .await
                .unwrap();
                assert!(sqlx::query_scalar::<_, bool>("SELECT pg_terminate_backend($1)")
                    .bind(backend)
                    .fetch_one(database.pool())
                    .await
                    .unwrap());
                let aborted = timeout(Duration::from_secs(8), requests.join_next())
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap();
                assert_error(
                    aborted,
                    StatusCode::SERVICE_UNAVAILABLE,
                    "storage_unavailable",
                    "Unavailable",
                )
                .await;
                blocker.rollback().await.unwrap();
                sqlx::query("DROP TRIGGER link_commit_hold ON links")
                    .execute(database.pool())
                    .await
                    .unwrap();
                sqlx::query("DROP FUNCTION link_commit_hold()")
                    .execute(database.pool())
                    .await
                    .unwrap();
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM links")
                        .fetch_one(database.pool())
                        .await
                        .unwrap(),
                    0,
                    "backend termination aborted the create transaction"
                );
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM code_reservations WHERE short_code = 'aborted-commit'")
                        .fetch_one(database.pool())
                        .await
                        .unwrap(),
                    0
                );
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM rate_charges")
                        .fetch_one(database.pool())
                        .await
                        .unwrap(),
                    1,
                    "the earlier allowance COMMIT is durable independently"
                );

                // In the real HTTP pipeline COMMIT 1 (allowance) is acknowledged
                // before body reads; target COMMIT 2 is the creation transaction.
                let mut proxy = commit_proxy::CommitProxy::start_target(
                    database.schema_database_url(),
                    2,
                )
                .await;
                let proxied = router_for_url(
                    &proxy.url,
                    &[
                        ("FERL_ANONYMOUS_CREATION", "true"),
                        ("FERL_DB_POOL_MAX", "1"),
                    ],
                );
                let (body_seen_tx, body_seen_rx) = tokio::sync::oneshot::channel();
                let (body_release_tx, body_release_rx) = tokio::sync::oneshot::channel();
                let stream = futures_util::stream::once(async move {
                    body_seen_tx.send(()).unwrap();
                    let _ = body_release_rx.await;
                    Ok::<_, Infallible>(
                        r#"{"destination_url":"https://durable-lost-ack.invalid/","title":"Lost token"}"#
                            .as_bytes()
                            .to_vec(),
                    )
                });
                let mut create_requests = JoinSet::new();
                create_requests.spawn(async move {
                    ServiceExt::oneshot(
                        proxied,
                        anonymous_request(Body::from_stream(stream), "application/json"),
                    )
                    .await
                    .unwrap()
                });
                timeout(Duration::from_secs(8), body_seen_rx)
                    .await
                    .unwrap()
                    .unwrap();
                let charges_before_creation: i64 =
                    sqlx::query_scalar("SELECT count(*) FROM rate_charges")
                        .fetch_one(database.pool())
                        .await
                        .unwrap();
                assert_eq!(charges_before_creation, 2);
                body_release_tx.send(()).unwrap();
                proxy.committed().await;
                let durable: (i64, i64, i64, i64) = sqlx::query_as(
                    "SELECT (SELECT count(*) FROM links), (SELECT count(*) FROM code_reservations WHERE short_code <> 'api'), \
                     (SELECT count(*) FROM link_tags), (SELECT count(*) FROM rate_charges)",
                )
                .fetch_one(database.pool())
                .await
                .unwrap();
                assert_eq!(durable, (1, 1, 0, 2));
                let lost_creation: (Uuid, String, Vec<u8>) = sqlx::query_as(
                    "SELECT id, short_code, management_token_digest FROM links",
                )
                .fetch_one(database.pool())
                .await
                .unwrap();
                canaries.push(lost_creation.0.to_string());
                canaries.push(lost_creation.1);
                canaries.push(bytes_hex(&lost_creation.2));
                proxy.disconnect().await;
                let uncertain = timeout(Duration::from_secs(8), create_requests.join_next())
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap();
                let uncertain = assert_error(
                    uncertain,
                    StatusCode::SERVICE_UNAVAILABLE,
                    "storage_unavailable",
                    "Unavailable",
                )
                .await;
                assert!(uncertain.get("management_token").is_none());
                assert!(uncertain.get("link").is_none());
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM links")
                        .fetch_one(database.pool())
                        .await
                        .unwrap(),
                    1,
                    "an unknown COMMIT is not replayed as a second link"
                );
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM rate_charges")
                        .fetch_one(database.pool())
                        .await
                        .unwrap(),
                    2,
                    "no automatic second allowance charge is attempted"
                );

                // A token-replacement acknowledgement can be lost after the
                // digest has durably changed. No plaintext is returned and the
                // old token remains revoked.
                let anon = call(
                    production_router_with(database, &[("FERL_ANONYMOUS_CREATION", "true")]).await,
                    anonymous_request(
                        Body::from(r#"{"destination_url":"https://rotation-uncertain.invalid/"}"#),
                        "application/json",
                    ),
                )
                .await;
                assert_eq!(anon.status(), StatusCode::CREATED);
                let anon = json(anon).await;
                let rotation_code = anon["link"]["short_code"].as_str().unwrap().to_owned();
                let old_token = anon["management_token"].as_str().unwrap().to_owned();
                canaries.push(old_token.clone());
                canaries.push(digest_hex(&old_token));
                let old_digest: Vec<u8> = sqlx::query_scalar(
                    "SELECT management_token_digest FROM links WHERE short_code = $1",
                )
                .bind(&rotation_code)
                .fetch_one(database.pool())
                .await
                .unwrap();
                // COMMIT 1 releases the pre-body fresh-authority check; COMMIT 2
                // is the digest replacement whose acknowledgement is withheld.
                let mut rotation_proxy = commit_proxy::CommitProxy::start_target(
                    database.schema_database_url(),
                    2,
                )
                .await;
                let rotation_app = router_for_url(&rotation_proxy.url, &[("FERL_DB_POOL_MAX", "1")]);
                let rotation_uri = format!("/api/v1/links/{rotation_code}/management-token");
                let mut rotation_requests = JoinSet::new();
                rotation_requests.spawn(async move {
                    ServiceExt::oneshot(
                        rotation_app,
                        request(Method::POST, &rotation_uri, Some(&old_token), ""),
                    )
                    .await
                    .unwrap()
                });
                rotation_proxy.committed().await;
                let new_digest: Vec<u8> = sqlx::query_scalar(
                    "SELECT management_token_digest FROM links WHERE short_code = $1",
                )
                .bind(&rotation_code)
                .fetch_one(database.pool())
                .await
                .unwrap();
                assert_ne!(old_digest, new_digest);
                canaries.push(bytes_hex(&new_digest));
                rotation_proxy.disconnect().await;
                let failed_rotation = timeout(Duration::from_secs(8), rotation_requests.join_next())
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap();
                let failed_rotation = assert_error(
                    failed_rotation,
                    StatusCode::SERVICE_UNAVAILABLE,
                    "storage_unavailable",
                    "Unavailable",
                )
                .await;
                assert!(failed_rotation.get("management_token").is_none());
                let old_token_response = call(
                    production_router_with(database, &[]).await,
                    request(
                        Method::GET,
                        &format!("/api/v1/links/{rotation_code}"),
                        Some(anon["management_token"].as_str().unwrap()),
                        "",
                    ),
                )
                .await;
                assert_eq!(old_token_response.status(), StatusCode::UNAUTHORIZED);
                assert_eq!(json(old_token_response).await["error"]["code"], "invalid_credentials");

                // PATCH preflight COMMIT is acknowledged; the following data
                // mutation COMMIT is durable but its acknowledgement is lost.
                let management_app = production_router_with(
                    database,
                    &[("FERL_ANONYMOUS_CREATION", "true")],
                )
                .await;
                let patch_link = call(
                    management_app.clone(),
                    anonymous_request(
                        Body::from(r#"{"destination_url":"https://patch-uncertain.invalid/"}"#),
                        "application/json",
                    ),
                )
                .await;
                assert_eq!(patch_link.status(), StatusCode::CREATED);
                let patch_link = json(patch_link).await;
                let patch_code = patch_link["link"]["short_code"]
                    .as_str()
                    .unwrap()
                    .to_owned();
                let patch_token = patch_link["management_token"]
                    .as_str()
                    .unwrap()
                    .to_owned();
                canaries.push(patch_token.clone());
                canaries.push(digest_hex(&patch_token));
                canaries.push(patch_code.clone());
                canaries.push("durable but not acknowledged".to_owned());
                canaries.push("https://patch-uncertain.invalid/".to_owned());
                let mut patch_proxy = commit_proxy::CommitProxy::start_target(
                    database.schema_database_url(),
                    2,
                )
                .await;
                let patch_app = router_for_url(&patch_proxy.url, &[("FERL_DB_POOL_MAX", "1")]);
                let patch_uri = format!("/api/v1/links/{patch_code}");
                let mut patch_requests = JoinSet::new();
                let patch_credential = patch_token.clone();
                patch_requests.spawn(async move {
                    ServiceExt::oneshot(
                        patch_app,
                        request(
                            Method::PATCH,
                            &patch_uri,
                            Some(&patch_credential),
                            r#"{"title":"durable but not acknowledged"}"#,
                        ),
                    )
                    .await
                    .unwrap()
                });
                patch_proxy.committed().await;
                assert_eq!(
                    sqlx::query_scalar::<_, Option<String>>(
                        "SELECT title FROM links WHERE short_code = $1",
                    )
                    .bind(&patch_code)
                    .fetch_one(database.pool())
                    .await
                    .unwrap()
                    .as_deref(),
                    Some("durable but not acknowledged")
                );
                patch_proxy.disconnect().await;
                let patch_failure = timeout(Duration::from_secs(8), patch_requests.join_next())
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap();
                let patch_failure = assert_error(
                    patch_failure,
                    StatusCode::SERVICE_UNAVAILABLE,
                    "storage_unavailable",
                    "Unavailable",
                )
                .await;
                assert!(patch_failure.get("link").is_none());
                let patch_durable = call(
                    management_app.clone(),
                    request(
                        Method::GET,
                        &format!("/api/v1/links/{patch_code}"),
                        Some(&patch_token),
                        "",
                    ),
                )
                .await;
                assert_eq!(patch_durable.status(), StatusCode::OK);
                assert_eq!(json(patch_durable).await["title"], "durable but not acknowledged");

                // DELETE likewise leaves its committed cascade/reservation in
                // place and returns no 204 success capability after the lost ack.
                let delete_link = call(
                    management_app.clone(),
                    anonymous_request(
                        Body::from(r#"{"destination_url":"https://delete-uncertain.invalid/","tags":["gone"]}"#),
                        "application/json",
                    ),
                )
                .await;
                assert_eq!(delete_link.status(), StatusCode::CREATED);
                let delete_link = json(delete_link).await;
                let delete_code = delete_link["link"]["short_code"]
                    .as_str()
                    .unwrap()
                    .to_owned();
                let delete_token = delete_link["management_token"]
                    .as_str()
                    .unwrap()
                    .to_owned();
                canaries.push(delete_token.clone());
                canaries.push(digest_hex(&delete_token));
                canaries.push(delete_code.clone());
                canaries.push("https://delete-uncertain.invalid/".to_owned());
                canaries.push("gone".to_owned());
                let mut delete_proxy = commit_proxy::CommitProxy::start_target(
                    database.schema_database_url(),
                    2,
                )
                .await;
                let delete_app = router_for_url(&delete_proxy.url, &[("FERL_DB_POOL_MAX", "1")]);
                let delete_uri = format!("/api/v1/links/{delete_code}");
                let mut delete_requests = JoinSet::new();
                delete_requests.spawn(async move {
                    ServiceExt::oneshot(
                        delete_app,
                        request(Method::DELETE, &delete_uri, Some(&delete_token), ""),
                    )
                    .await
                    .unwrap()
                });
                delete_proxy.committed().await;
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM links WHERE short_code = $1")
                        .bind(&delete_code)
                        .fetch_one(database.pool())
                        .await
                        .unwrap(),
                    0
                );
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM code_reservations WHERE short_code = $1")
                        .bind(&delete_code)
                        .fetch_one(database.pool())
                        .await
                        .unwrap(),
                    1
                );
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM link_tags WHERE name = 'gone'")
                        .fetch_one(database.pool())
                        .await
                        .unwrap(),
                    0
                );
                delete_proxy.disconnect().await;
                let delete_failure = timeout(Duration::from_secs(8), delete_requests.join_next())
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap();
                assert_eq!(delete_failure.status(), StatusCode::SERVICE_UNAVAILABLE);
                assert_eq!(json(delete_failure).await["error"]["code"], "storage_unavailable");
                let deleted_token = call(
                    management_app,
                    request(
                        Method::GET,
                        &format!("/api/v1/links/{delete_code}"),
                        Some(delete_link["management_token"].as_str().unwrap()),
                        "",
                    ),
                )
                .await;
                assert_eq!(deleted_token.status(), StatusCode::UNAUTHORIZED);
                assert_eq!(json(deleted_token).await["error"]["code"], "invalid_credentials");

                let diagnostics = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
                assert!(
                    diagnostics.contains("link_commit_outcome_unknown"),
                    "commit uncertainty was emitted through production diagnostics"
                );
                assert!(diagnostics.contains("request completed"));
                for canary in canaries {
                    assert!(
                        !diagnostics.contains(&canary),
                        "commit-failure canary leaked in production diagnostics"
                    );
                }

            })
        })
        .await;
}
