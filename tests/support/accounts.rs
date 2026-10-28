use std::{collections::BTreeMap, time::Duration};

use axum::{
    Router,
    body::Body,
    http::{Method, Request, StatusCode},
    response::Response,
};
use ferl_server::{
    app::{self, AppState},
    config::{Config, MigrationConfig},
    db::migrate,
    domain::{
        ids::{AccountId, LinkId},
        secret::{AccountApiKey, ManagementTokenSecret},
        text::ComparisonText,
    },
};
use http_body_util::BodyExt;
use sqlx::PgPool;
use tokio::time::timeout;
use uuid::Uuid;

use crate::database_support::IsolatedTestDatabase;

pub async fn migrated_database() -> IsolatedTestDatabase {
    let database = IsolatedTestDatabase::connect_from_harness().await;
    let config = MigrationConfig::from_map(BTreeMap::from([(
        "FERL_DATABASE_URL".to_owned(),
        database.schema_database_url().to_owned(),
    )]))
    .expect("isolated schema URL forms a valid migration profile");
    migrate::run_migrations(&config)
        .await
        .expect("fixture schema migrates without touching another schema");
    database
}

pub async fn production_router(database: &IsolatedTestDatabase) -> Router {
    production_router_with(database, &[]).await
}

pub async fn production_router_with(
    database: &IsolatedTestDatabase,
    extra: &[(&str, &str)],
) -> Router {
    let mut values = BTreeMap::from([
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
        ("FERL_DB_LOCK_TIMEOUT_SECS".to_owned(), "10".to_owned()),
    ]);
    for (name, value) in extra {
        values.insert((*name).to_owned(), (*value).to_owned());
    }
    let config = Config::from_map(values)
        .expect("test production profile has a configured deployment cursor key");
    let state = AppState::from_config(&config).expect("production pool configuration is valid");
    state
        .verify_schema()
        .await
        .expect("production state verifies the fixture schema");
    app::router(state)
}

pub async fn seed_account(
    pool: &PgPool,
    username: &str,
    role: &str,
    enabled: bool,
) -> (AccountId, String) {
    let key = AccountApiKey::generate().expect("OS randomness is available");
    let id = AccountId::new(Uuid::new_v4());
    sqlx::query(
        "INSERT INTO accounts (id, username, role, enabled, api_key_digest) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(id.as_uuid())
    .bind(username)
    .bind(role)
    .bind(enabled)
    .bind(key.digest().as_bytes().as_slice())
    .execute(pool)
    .await
    .expect("fixture account is persisted");
    (id, key.as_str().to_owned())
}

pub async fn seed_anonymous_link(pool: &PgPool, code: &str) -> (LinkId, String) {
    let token = ManagementTokenSecret::generate().expect("OS randomness is available");
    let id = LinkId::new(Uuid::new_v4());
    let destination = "https://destination.example.test/path";
    let search = ComparisonText::from_persisted(destination);
    sqlx::query("INSERT INTO code_reservations (short_code) VALUES ($1)")
        .bind(code)
        .execute(pool)
        .await
        .expect("fixture reservation is inserted");
    sqlx::query(
        "INSERT INTO links \
         (id, short_code, owner_id, destination_url, destination_search, enabled, visit_count, management_token_digest) \
         VALUES ($1, $2, NULL, $3, $4, true, 0, $5)",
    )
    .bind(id.as_uuid())
    .bind(code)
    .bind(destination)
    .bind(search.as_str())
    .bind(token.digest().as_bytes().as_slice())
    .execute(pool)
    .await
    .expect("fixture anonymous link is persisted");
    (id, token.as_str().to_owned())
}

pub fn request(method: Method, uri: &str, credential: Option<&str>, body: &str) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("x-request-id", "client-value");
    if let Some(credential) = credential {
        builder = builder.header("authorization", format!("Bearer {credential}"));
    }
    if !body.is_empty() {
        builder = builder.header("content-type", "application/json");
    }
    builder
        .body(Body::from(body.to_owned()))
        .expect("test request is valid")
}

pub async fn call(app: Router, request: Request<Body>) -> Response {
    timeout(
        Duration::from_secs(5),
        tower::ServiceExt::oneshot(app, request),
    )
    .await
    .expect("HTTP response arrives before the deadline")
    .expect("production router is infallible")
}

pub async fn json(response: Response) -> serde_json::Value {
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert_eq!(
        response.headers()["content-type"],
        "application/json",
        "JSON response has the contract content type"
    );
    let request_id = response.headers()["x-request-id"]
        .to_str()
        .expect("request ID header is text")
        .to_owned();
    assert_ne!(request_id, "client-value");
    Uuid::parse_str(&request_id).expect("server generated a UUID request ID");
    let bytes = timeout(Duration::from_secs(5), response.into_body().collect())
        .await
        .expect("response body finishes before deadline")
        .expect("response body can be read")
        .to_bytes();
    let value: serde_json::Value = serde_json::from_slice(&bytes).expect("response is valid JSON");
    if let Some(error_request_id) = value.get("request_id") {
        assert_eq!(error_request_id, &request_id);
    }
    value
}

pub async fn bytes(response: Response) -> Vec<u8> {
    assert_eq!(response.headers()["cache-control"], "no-store");
    let request_id = response.headers()["x-request-id"].to_str().unwrap();
    assert_ne!(request_id, "client-value");
    timeout(Duration::from_secs(5), response.into_body().collect())
        .await
        .expect("response body finishes before deadline")
        .expect("response body can be read")
        .to_bytes()
        .to_vec()
}

pub fn assert_contract_headers(response: &Response, error_request_id: Option<&str>) {
    assert_eq!(response.headers()["cache-control"], "no-store");
    let request_id = response.headers()["x-request-id"].to_str().unwrap();
    assert_ne!(request_id, "client-value");
    Uuid::parse_str(request_id).expect("request ID is a server-generated UUID");
    if let Some(body_request_id) = error_request_id {
        assert_eq!(request_id, body_request_id);
    }
}

pub fn assert_status(response: &Response, expected: StatusCode) {
    assert_eq!(response.status(), expected);
    assert_contract_headers(response, None);
}
