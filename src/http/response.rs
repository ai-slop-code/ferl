use axum::response::Response;
use http::{HeaderValue, StatusCode};
use serde::Serialize;
use uuid::Uuid;

use super::request_id::success_json;

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
}

pub fn health(request_id: Uuid, available: bool) -> Response {
    let status = if available {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    let body = HealthResponse {
        status: if available { "ok" } else { "unavailable" },
    };
    let bytes = serde_json::to_vec(&body).expect("static health response is serializable");
    let mut response = success_json(status, bytes);
    response.headers_mut().insert(
        http::header::HeaderName::from_static("x-request-id"),
        HeaderValue::from_str(&request_id.to_string()).expect("UUID is a valid header value"),
    );
    response
}
