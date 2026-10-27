use std::time::Duration;

use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
};
use ferl_server::{app::AppState, http::router};
use http_body_util::BodyExt;
use sqlx::{
    PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use tower::ServiceExt;
use uuid::Uuid;

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
    router(AppState::new(pool, Duration::from_millis(100), 16))
}

fn request(method: Method, uri: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("x-request-id", "client-controlled-value")
        .header("authorization", "Bearer deliberately-invalid")
        .body(Body::empty())
        .expect("test request is valid")
}

async fn response_body(response: axum::response::Response) -> Vec<u8> {
    response
        .into_body()
        .collect()
        .await
        .expect("response body can be read")
        .to_bytes()
        .to_vec()
}

#[tokio::test]
async fn health_ignores_credentials_replaces_request_id_and_sets_common_headers() {
    let response = app_with_unreachable_database()
        .oneshot(request(Method::GET, "/api/health/live"))
        .await
        .expect("router responds");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let request_id = response.headers()["x-request-id"]
        .to_str()
        .expect("request id is text");
    Uuid::parse_str(request_id).expect("server request ID is a UUID");
    assert_ne!(request_id, "client-controlled-value");
    assert_eq!(response_body(response).await, br#"{"status":"ok"}"#);
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
        let response = app
            .clone()
            .oneshot(request(Method::GET, path))
            .await
            .expect("router responds");
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

    let response = app
        .clone()
        .oneshot(request(Method::POST, "/api/health/live"))
        .await
        .expect("router responds");
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(response.headers()["allow"], "GET");
    let body: serde_json::Value =
        serde_json::from_slice(&response_body(response).await).expect("method error body is JSON");
    assert_eq!(body["error"]["code"], "method_not_allowed");

    let response = app
        .oneshot(request(Method::HEAD, "/api/health/ready"))
        .await
        .expect("router responds");
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(response.headers()["allow"], "GET");
    assert!(response_body(response).await.is_empty());

    let response = app_with_unreachable_database()
        .oneshot(request(Method::HEAD, "/"))
        .await
        .expect("router responds");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert!(response_body(response).await.is_empty());
}
