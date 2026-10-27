#[path = "support/http.rs"]
mod http_support;

use std::time::Duration;

use axum::http::{Method, StatusCode};
use ferl_server::{app::AppState, http::router};
use sqlx::{
    PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use uuid::Uuid;

use http_support::{call, response_body};

fn app_with_unreachable_database() -> axum::Router {
    let options = PgConnectOptions::new()
        .host("127.0.0.1")
        .port(1)
        .username("ferl_test")
        .database("ferl_test");
    let pool: PgPool = PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_millis(100))
        .connect_lazy_with(options);
    router(AppState::new(pool, Duration::from_millis(100), 16, 16))
}

#[tokio::test]
async fn health_ignores_credentials_replaces_request_id_and_sets_common_headers() {
    let app = app_with_unreachable_database();
    let response = call(app.clone(), Method::GET, "/api/health/live").await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let request_id = response.headers()["x-request-id"]
        .to_str()
        .expect("request id is text");
    Uuid::parse_str(request_id).expect("server request ID is a UUID");
    assert_ne!(request_id, "client-controlled-value");
    assert_eq!(response_body(response).await, br#"{"status":"ok"}"#);

    let response = call(app, Method::GET, "/api/health/ready").await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let request_id = response.headers()["x-request-id"]
        .to_str()
        .expect("request ID is text")
        .to_owned();
    let body: serde_json::Value =
        serde_json::from_slice(&response_body(response).await).expect("readiness JSON");
    assert_eq!(body, serde_json::json!({"status": "unavailable"}));
    assert_ne!(request_id, "client-controlled-value");
}

#[tokio::test]
async fn fallback_and_method_errors_are_json_and_head_is_bodyless() {
    let app = app_with_unreachable_database();
    for path in [
        "/",
        "/api",
        "/api/",
        "/api/v1/unknown",
        "/api-sale",
        "/api/health/live/extra",
    ] {
        let response = call(app.clone(), Method::GET, path).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "path {path}");
        assert_eq!(response.headers()["cache-control"], "no-store");
        let header_id = response.headers()["x-request-id"]
            .to_str()
            .unwrap()
            .to_owned();
        let body: serde_json::Value =
            serde_json::from_slice(&response_body(response).await).expect("error body is JSON");
        assert_eq!(body["error"]["code"], "not_found");
        assert_eq!(body["request_id"], header_id);
    }

    let response = call(app.clone(), Method::POST, "/api/health/live").await;
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(response.headers()["allow"], "GET");
    assert_eq!(response.headers()["cache-control"], "no-store");
    let header_id = response.headers()["x-request-id"]
        .to_str()
        .unwrap()
        .to_owned();
    let body: serde_json::Value =
        serde_json::from_slice(&response_body(response).await).expect("method error body is JSON");
    assert_eq!(body["error"]["code"], "method_not_allowed");
    assert_eq!(body["request_id"], header_id);

    let response = call(app.clone(), Method::HEAD, "/api/health/ready").await;
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(response.headers()["allow"], "GET");
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert!(response_body(response).await.is_empty());

    let response = call(app.clone(), Method::HEAD, "/").await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert!(response_body(response).await.is_empty());

    let response = call(app.clone(), Method::HEAD, "/api/v1/unknown").await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert!(response_body(response).await.is_empty());

    for path in ["/api/health/live?unexpected=1", "/api/health/ready?x="] {
        let response = call(app.clone(), Method::GET, path).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "path {path}");
        assert_eq!(response.headers()["cache-control"], "no-store");
        let header_id = response.headers()["x-request-id"]
            .to_str()
            .unwrap()
            .to_owned();
        assert_ne!(header_id, "client-controlled-value");
        let body: serde_json::Value =
            serde_json::from_slice(&response_body(response).await).expect("query error is JSON");
        assert_eq!(body["error"]["code"], "invalid_request");
        assert_eq!(body["request_id"], header_id);
    }
}
