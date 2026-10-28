use axum::{body::Body, extract::Request, middleware::Next, response::Response};
use http::{HeaderName, HeaderValue, Method, StatusCode};
use uuid::Uuid;

#[derive(Clone, Copy)]
pub struct RequestId(pub Uuid);

pub async fn envelope(mut request: Request, next: Next) -> Response {
    let request_id = Uuid::new_v4();
    let is_head = request.method() == Method::HEAD;
    let method = safe_method_name(request.method());
    let route = safe_route_template(request.uri().path());
    request.extensions_mut().insert(RequestId(request_id));

    let response = next.run(request).await;
    let mut response = sanitize_framework_error(response, request_id);
    if response.status() == StatusCode::UNAUTHORIZED {
        response.headers_mut().insert(
            http::header::WWW_AUTHENTICATE,
            HeaderValue::from_static("Bearer"),
        );
    }
    response.headers_mut().insert(
        HeaderName::from_static("x-request-id"),
        HeaderValue::from_str(&request_id.to_string()).expect("UUID is a valid header value"),
    );
    response.headers_mut().insert(
        http::header::CACHE_CONTROL,
        HeaderValue::from_static("no-store"),
    );

    if is_head {
        *response.body_mut() = Body::empty();
    }

    let outcome = if response.status().is_server_error() {
        crate::telemetry::metrics::Outcome::Failure.as_str()
    } else if response.status().is_client_error() {
        crate::telemetry::metrics::Outcome::Rejected.as_str()
    } else {
        crate::telemetry::metrics::Outcome::Success.as_str()
    };
    tracing::info!(
        request_id = %request_id,
        operation = crate::telemetry::metrics::Operation::HttpRequest.as_str(),
        method,
        route,
        status_code = response.status().as_u16(),
        outcome,
        "request completed"
    );
    response
}

fn sanitize_framework_error(response: Response, request_id: Uuid) -> Response {
    if response
        .extensions()
        .get::<crate::error::ContractResponse>()
        .is_some()
    {
        return response;
    }

    let status = response.status();
    if !status.is_client_error() && !status.is_server_error() {
        return response;
    }

    let (code, message) = match status {
        StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY => (
            crate::error::ErrorCode::InvalidRequest,
            "The request is invalid.",
        ),
        StatusCode::UNAUTHORIZED => (
            crate::error::ErrorCode::AuthenticationRequired,
            "Authentication is required.",
        ),
        StatusCode::FORBIDDEN => (
            crate::error::ErrorCode::Forbidden,
            "The operation is not permitted.",
        ),
        StatusCode::NOT_FOUND => (
            crate::error::ErrorCode::NotFound,
            "The requested resource was not found.",
        ),
        StatusCode::METHOD_NOT_ALLOWED => (
            crate::error::ErrorCode::MethodNotAllowed,
            "This HTTP method is not supported for this endpoint.",
        ),
        StatusCode::PAYLOAD_TOO_LARGE => (
            crate::error::ErrorCode::RequestTooLarge,
            "The request body exceeds the allowed size.",
        ),
        StatusCode::UNSUPPORTED_MEDIA_TYPE => (
            crate::error::ErrorCode::UnsupportedMediaType,
            "The request content type is not supported.",
        ),
        StatusCode::TOO_MANY_REQUESTS => (
            crate::error::ErrorCode::RateLimited,
            "The request allowance has been exceeded.",
        ),
        StatusCode::SERVICE_UNAVAILABLE => (
            crate::error::ErrorCode::ServiceUnavailable,
            "The server is temporarily unable to accept this request.",
        ),
        _ => (
            crate::error::ErrorCode::InternalError,
            "An internal error occurred.",
        ),
    };

    let preserved_header = match code {
        crate::error::ErrorCode::MethodNotAllowed => response
            .headers()
            .get(http::header::ALLOW)
            .cloned()
            .map(|value| (http::header::ALLOW, value)),
        crate::error::ErrorCode::RateLimited => response
            .headers()
            .get(http::header::RETRY_AFTER)
            .cloned()
            .map(|value| (http::header::RETRY_AFTER, value)),
        _ => None,
    };
    crate::error::response(request_id, code, message, preserved_header)
}

fn safe_method_name(method: &Method) -> &'static str {
    match *method {
        Method::GET => "GET",
        Method::HEAD => "HEAD",
        Method::POST => "POST",
        Method::PUT => "PUT",
        Method::PATCH => "PATCH",
        Method::DELETE => "DELETE",
        Method::OPTIONS => "OPTIONS",
        _ => "OTHER",
    }
}

fn safe_route_template(path: &str) -> &'static str {
    match path {
        "/api/health/live" => "/api/health/live",
        "/api/health/ready" => "/api/health/ready",
        "/api/v1/me" => "/api/v1/me",
        "/api/v1/me/api-key" => "/api/v1/me/api-key",
        "/api/v1/admin/accounts" => "/api/v1/admin/accounts",
        "/api" | "/api/" => "/api",
        _ if account_item_path(path) => "/api/v1/admin/accounts/{id}",
        _ if account_key_path(path) => "/api/v1/admin/accounts/{id}/api-key",
        _ if path.starts_with("/api/") => "/api/{unmatched}",
        _ => "/{unmatched}",
    }
}

fn account_item_path(path: &str) -> bool {
    path.strip_prefix("/api/v1/admin/accounts/")
        .is_some_and(|tail| !tail.is_empty() && !tail.contains('/'))
}

fn account_key_path(path: &str) -> bool {
    path.strip_prefix("/api/v1/admin/accounts/")
        .and_then(|tail| tail.strip_suffix("/api-key"))
        .is_some_and(|id| !id.is_empty() && !id.contains('/'))
}

pub fn method_error(request_id: Uuid, allow: &'static str) -> Response {
    let allow_header = HeaderValue::from_static(allow);
    crate::error::response(
        request_id,
        crate::error::ErrorCode::MethodNotAllowed,
        "This HTTP method is not supported for this endpoint.",
        Some((http::header::ALLOW, allow_header)),
    )
}

pub fn success_json(status: StatusCode, bytes: Vec<u8>) -> Response {
    let mut response = Response::new(Body::from(bytes));
    *response.status_mut() = status;
    response.headers_mut().insert(
        http::header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    response
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use http_body_util::BodyExt;

    use super::*;

    #[tokio::test]
    async fn framework_validation_rejection_becomes_sanitized_400_json() {
        let request_id = Uuid::new_v4();
        let mut framework_response = Response::new(Body::from("raw validation detail"));
        *framework_response.status_mut() = StatusCode::UNPROCESSABLE_ENTITY;
        framework_response.headers_mut().insert(
            http::header::CONTENT_TYPE,
            HeaderValue::from_static("text/plain; charset=utf-8"),
        );

        let response = sanitize_framework_error(framework_response, request_id);
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            response.headers()[http::header::CONTENT_TYPE],
            "application/json"
        );
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["error"]["code"], "invalid_request");
        assert_eq!(body["request_id"], request_id.to_string());
        assert!(!String::from_utf8_lossy(&bytes).contains("raw validation detail"));
    }

    #[tokio::test]
    async fn framework_method_rejection_keeps_allow_and_uses_error_envelope() {
        let request_id = Uuid::new_v4();
        let mut framework_response = Response::new(Body::from("unsupported method text"));
        *framework_response.status_mut() = StatusCode::METHOD_NOT_ALLOWED;
        framework_response
            .headers_mut()
            .insert(http::header::ALLOW, HeaderValue::from_static("GET, HEAD"));

        let response = sanitize_framework_error(framework_response, request_id);
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(response.headers()[http::header::ALLOW], "GET, HEAD");
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["error"]["code"], "method_not_allowed");
        assert!(!String::from_utf8_lossy(&bytes).contains("unsupported method text"));
    }

    #[tokio::test]
    async fn framework_json_rejection_is_replaced_with_request_id_envelope() {
        let request_id = Uuid::new_v4();
        let mut framework_response =
            Response::new(Body::from(r#"{"detail":"secret framework diagnostic"}"#));
        *framework_response.status_mut() = StatusCode::BAD_REQUEST;
        framework_response.headers_mut().insert(
            http::header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );

        let response = sanitize_framework_error(framework_response, request_id);
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            response.headers()[http::header::CONTENT_TYPE],
            "application/json"
        );
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["error"]["code"], "invalid_request");
        assert_eq!(body["request_id"], request_id.to_string());
        assert!(!String::from_utf8_lossy(&bytes).contains("secret framework diagnostic"));
    }
}
