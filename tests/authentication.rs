#[path = "support/accounts.rs"]
#[allow(dead_code)]
mod account_support;
#[path = "support/database.rs"]
mod database_support;

use std::{
    convert::Infallible,
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    body::Body,
    http::{HeaderValue, Method, Request, StatusCode, header},
};
use ferl_server::{
    app::{self, AppState},
    config::Config,
    domain::secret::AccountApiKey,
};
use http_body_util::BodyExt;
use tokio::{net::TcpListener, sync::oneshot, time::timeout};
use tower::ServiceExt;
use tracing::{
    Event, Subscriber,
    field::{Field, Visit},
};
use tracing_subscriber::{Layer, layer::Context, prelude::*};

use account_support::{migrated_database, production_router, seed_account, seed_anonymous_link};
use database_support::{CONTAINER_NAME, command_output, wait_until_postgres_is_ready};

struct Captured(Arc<Mutex<String>>);

struct Fields(String);

impl Visit for Fields {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        use std::fmt::Write as _;
        let _ = write!(self.0, "{}={value:?};", field.name());
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        use std::fmt::Write as _;
        let _ = write!(self.0, "{}={value};", field.name());
    }
}

impl<S: Subscriber> Layer<S> for Captured {
    fn on_event(&self, event: &Event<'_>, _context: Context<'_, S>) {
        // Use the production JSON logger's explicit target allowlist; SQLx's
        // query text/parameters are filtered before reaching application logs.
        if !ferl_server::telemetry::logging::is_allowlisted_target(event.metadata().target()) {
            return;
        }
        let mut fields = Fields(String::new());
        event.record(&mut fields);
        self.0.lock().unwrap().push_str(&fields.0);
    }
}

async fn json_body(response: axum::response::Response) -> serde_json::Value {
    let request_id = response.headers()["x-request-id"]
        .to_str()
        .expect("request ID header is text")
        .to_owned();
    let bytes = timeout(Duration::from_secs(5), response.into_body().collect())
        .await
        .expect("HTTP response body completes before deadline")
        .expect("HTTP body is readable")
        .to_bytes();
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).expect("application response is JSON");
    if let Some(error_id) = value.get("request_id") {
        assert_eq!(error_id, &request_id);
    }
    value
}

fn assert_headers(response: &axum::response::Response) {
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert_eq!(response.headers()["content-type"], "application/json");
    assert_ne!(response.headers()["x-request-id"], "client-value");
}

#[tokio::test]
async fn failing_credential_and_persistence_paths_emit_only_safe_diagnostics() {
    let output = Arc::new(Mutex::new(String::new()));
    tracing::subscriber::set_global_default(
        tracing_subscriber::registry().with(Captured(Arc::clone(&output))),
    )
    .expect("this integration-test binary installs one capture subscriber");

    let database = migrated_database().await;
    let database_url = database.schema_database_url().to_owned();
    let (_admin, admin_key) = seed_account(database.pool(), "auth_admin", "admin", true).await;
    let (_user, _user_key) = seed_account(database.pool(), "auth_user", "user", true).await;
    let (link_id, management_token) = seed_anonymous_link(database.pool(), "auth-management").await;
    let app = production_router(&database).await;

    let malformed = "ferl_ak_v1_CANARY_MALFORMED_SECRET";
    let malformed_response =
        call_with_auth(app.clone(), Method::GET, "/api/v1/me", Some(malformed), "").await;
    assert_eq!(malformed_response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        malformed_response.headers()[header::WWW_AUTHENTICATE],
        "Bearer"
    );
    assert_headers(&malformed_response);
    assert_eq!(
        json_body(malformed_response).await["error"]["code"],
        "invalid_credentials"
    );

    let unmatched = AccountApiKey::generate().unwrap();
    let unmatched_secret = unmatched.as_str().to_owned();
    let unmatched_response = call_with_auth(
        app.clone(),
        Method::GET,
        "/api/v1/me",
        Some(&unmatched_secret),
        "",
    )
    .await;
    assert_eq!(unmatched_response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        json_body(unmatched_response).await["error"]["code"],
        "invalid_credentials"
    );

    let token_response = call_with_auth(
        app.clone(),
        Method::GET,
        "/api/v1/me",
        Some(&management_token),
        "",
    )
    .await;
    assert_eq!(token_response.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        json_body(token_response).await["error"]["code"],
        "forbidden"
    );
    // Fixture token itself is live and resolved on every protected request.
    let token_still_live: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM links WHERE id = $1 AND management_token_digest IS NOT NULL)",
    )
    .bind(link_id.as_uuid())
    .fetch_one(database.pool())
    .await
    .unwrap();
    assert!(token_still_live);

    let canary_path = format!("/api/v1/admin/accounts/{}", uuid::Uuid::new_v4());
    let invalid_body = call_with_auth(
        app.clone(),
        Method::POST,
        "/api/v1/admin/accounts",
        Some(&admin_key),
        r#"{"username":"CANARY_USERNAME","unexpected":"CANARY_BODY"}"#,
    )
    .await;
    assert_eq!(invalid_body.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        json_body(invalid_body).await["error"]["code"],
        "invalid_request"
    );

    let invalid_query = call_with_auth(
        app.clone(),
        Method::GET,
        "/api/v1/admin/accounts?cursor=CANARY_CURSOR",
        Some(&admin_key),
        "",
    )
    .await;
    assert_eq!(invalid_query.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        json_body(invalid_query).await["error"]["code"],
        "invalid_request"
    );

    let detail = call_with_auth(app.clone(), Method::GET, &canary_path, Some(&admin_key), "").await;
    assert_eq!(detail.status(), StatusCode::NOT_FOUND);
    assert_eq!(json_body(detail).await["error"]["code"], "not_found");

    let malformed_stored_key = AccountApiKey::generate().unwrap();
    let bad_digest = malformed_stored_key.digest();
    let bad_id = uuid::Uuid::new_v4();
    sqlx::query("ALTER TABLE accounts DROP CONSTRAINT accounts_role_check")
        .execute(database.pool())
        .await
        .expect("only this isolated schema loses its role check for a decode failure fixture");
    sqlx::query(
        "INSERT INTO accounts (id, username, role, enabled, api_key_digest) \
         VALUES ($1, 'stored_corruption', 'corrupt-role', true, $2)",
    )
    .bind(bad_id)
    .bind(bad_digest.as_bytes().as_slice())
    .execute(database.pool())
    .await
    .unwrap();
    let corrupt_secret = malformed_stored_key.as_str().to_owned();
    let decode_failure = call_with_auth(
        app.clone(),
        Method::GET,
        "/api/v1/me",
        Some(&corrupt_secret),
        "",
    )
    .await;
    assert_eq!(decode_failure.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        json_body(decode_failure).await["error"]["code"],
        "internal_error"
    );

    database.close().await;
    let outage_secret = exercise_database_outages().await;

    let diagnostics = output.lock().unwrap().clone();
    assert!(
        diagnostics.contains("invalid_credentials"),
        "invalid-credential diagnostic was emitted"
    );
    assert!(
        diagnostics.contains("authentication_persistence_failure"),
        "stored-data decode diagnostic was emitted"
    );
    assert!(
        diagnostics.contains("authentication_storage_unavailable"),
        "database authentication diagnostic was emitted"
    );
    assert!(
        diagnostics.contains("database_unavailable_or_commit_uncertain"),
        "database mutation diagnostic was emitted"
    );
    assert!(
        diagnostics.contains("request completed"),
        "HTTP completion diagnostics were emitted"
    );
    for canary in [
        malformed,
        unmatched_secret.as_str(),
        outage_secret.as_str(),
        corrupt_secret.as_str(),
        management_token.as_str(),
        admin_key.as_str(),
        "CANARY_USERNAME",
        "CANARY_BODY",
        "CANARY_CURSOR",
        "outage_canary",
        canary_path.as_str(),
    ] {
        assert!(
            !diagnostics.contains(canary),
            "diagnostic leaked canary {canary}: {diagnostics}"
        );
    }
    assert!(!diagnostics.contains(&database_url));
}

async fn exercise_database_outages() -> String {
    let key = AccountApiKey::generate().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let unavailable_port = listener.local_addr().unwrap().port();
    drop(listener);
    let config = Config::from_map(std::collections::BTreeMap::from([
        (
            "FERL_DATABASE_URL".into(),
            format!("postgres://ferl_test@127.0.0.1:{unavailable_port}/ferl_test"),
        ),
        (
            "FERL_PUBLIC_BASE_URL".into(),
            "https://short.example.test".into(),
        ),
        (
            "FERL_CURSOR_KEY".into(),
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".into(),
        ),
        ("FERL_DB_ACQUIRE_TIMEOUT_SECS".into(), "1".into()),
    ]))
    .unwrap();
    let app = app::router(AppState::from_config(&config).unwrap());
    let response = call_with_auth(app, Method::GET, "/api/v1/me", Some(key.as_str()), "").await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_headers(&response);
    assert_eq!(
        json_body(response).await["error"]["code"],
        "storage_unavailable"
    );

    let database = migrated_database().await;
    let (_admin, admin_key) = seed_account(database.pool(), "outage_admin", "admin", true).await;
    let config = Config::from_map(std::collections::BTreeMap::from([
        (
            "FERL_DATABASE_URL".to_owned(),
            database.schema_database_url().to_owned(),
        ),
        (
            "FERL_PUBLIC_BASE_URL".to_owned(),
            "https://short.example.test".to_owned(),
        ),
        (
            "FERL_CURSOR_KEY".to_owned(),
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".to_owned(),
        ),
    ]))
    .unwrap();
    let state = AppState::from_config(&config).unwrap();
    state.verify_schema().await.unwrap();
    let production_pool = state.pool.clone();
    let router = app::router(state);
    let (started_tx, started_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let stream = futures_util::stream::once(async move {
        let _ = started_tx.send(());
        let _ = release_rx.await;
        Ok::<_, Infallible>(br#"{"username":"outage_canary"}"#.to_vec())
    });
    let request = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/admin/accounts")
        .header(header::AUTHORIZATION, format!("Bearer {admin_key}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from_stream(stream))
        .unwrap();
    let task = tokio::spawn(router.oneshot(request));
    timeout(Duration::from_secs(5), started_rx)
        .await
        .expect("body decoder starts only after fresh authentication and admin eligibility")
        .expect("streamed body reports its start");

    // The test owns the shared harness lock through IsolatedTestDatabase. Use
    // direct Podman commands rather than recursively invoking the locking script.
    let stopped = command_output("podman", &["stop", "--time", "10", CONTAINER_NAME]).await;
    let _ = release_tx.send(());
    let result = timeout(Duration::from_secs(12), task).await;
    let restarted = command_output("podman", &["start", CONTAINER_NAME]).await;
    wait_until_postgres_is_ready().await;
    assert!(
        stopped.status.success(),
        "the owned harness container stopped"
    );
    assert!(
        restarted.status.success(),
        "the owned container restarted with its volume"
    );
    let response = result
        .expect("database-failed mutation request completes before its deadline")
        .expect("mutation request task joins")
        .expect("production router remains infallible");
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_headers(&response);
    assert_eq!(
        json_body(response).await["error"]["code"],
        "storage_unavailable"
    );
    let created: i64 =
        sqlx::query_scalar("SELECT count(*) FROM accounts WHERE username = 'outage_canary'")
            .fetch_one(&production_pool)
            .await
            .unwrap();
    assert_eq!(created, 0, "failed mutation left no partial account");
    let secret = key.as_str().to_owned();
    database.close().await;
    secret
}

async fn call_with_auth(
    app: axum::Router,
    method: Method,
    uri: &str,
    credential: Option<&str>,
    body: &str,
) -> axum::response::Response {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("x-request-id", "client-value");
    if let Some(credential) = credential {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {credential}"));
    }
    if !body.is_empty() {
        builder = builder.header(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
    }
    let request = builder.body(Body::from(body.to_owned())).unwrap();
    timeout(Duration::from_secs(5), app.oneshot(request))
        .await
        .expect("production account request completes")
        .expect("router service is infallible")
}
