use std::time::Duration;

use axum::{
    Router,
    body::Body,
    http::{Method, Request},
    response::Response,
};
use http_body_util::BodyExt;
use tokio::time::timeout;
use tower::ServiceExt;

const HTTP_DEADLINE: Duration = Duration::from_secs(3);

pub fn request(method: Method, uri: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("x-request-id", "client-controlled-value")
        .header("authorization", "Bearer deliberately-invalid")
        .body(Body::empty())
        .expect("test request is valid")
}

pub async fn call(app: Router, method: Method, uri: &str) -> Response {
    timeout(HTTP_DEADLINE, app.oneshot(request(method, uri)))
        .await
        .expect("HTTP response arrives before the test deadline")
        .expect("router service is infallible")
}

pub async fn response_body(response: Response) -> Vec<u8> {
    timeout(HTTP_DEADLINE, response.into_body().collect())
        .await
        .expect("HTTP response body completes before the test deadline")
        .expect("HTTP response body can be read")
        .to_bytes()
        .to_vec()
}
