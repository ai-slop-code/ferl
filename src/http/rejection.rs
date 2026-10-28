use axum::response::Response;
use http::StatusCode;
use uuid::Uuid;

use crate::error::{self, ErrorCode};

use super::{body::BodyDecodeError, dto::RequestValidationError, query::QueryError};

pub fn body_error(request_id: Uuid, error: BodyDecodeError) -> Response {
    let (code, message) = match error {
        BodyDecodeError::TooLarge => (
            ErrorCode::RequestTooLarge,
            "The request body exceeds the allowed size.",
        ),
        BodyDecodeError::UnsupportedMediaType => (
            ErrorCode::UnsupportedMediaType,
            "The request content type is not supported.",
        ),
        BodyDecodeError::InvalidRequest => {
            (ErrorCode::InvalidRequest, "The request body is invalid.")
        }
    };
    error::response(request_id, code, message, None)
}

pub fn request_error(request_id: Uuid, error: RequestValidationError) -> Response {
    match error {
        RequestValidationError::InvalidRequest => error::response(
            request_id,
            ErrorCode::InvalidRequest,
            "One or more request fields are invalid.",
            None,
        ),
        RequestValidationError::ForbiddenFeature => error::response(
            request_id,
            ErrorCode::Forbidden,
            "The requested feature is not permitted.",
            None,
        ),
    }
}

pub fn query_error(request_id: Uuid, error: QueryError) -> Response {
    match error {
        QueryError::InvalidRequest => error::response(
            request_id,
            ErrorCode::InvalidRequest,
            "One or more query parameters are invalid.",
            None,
        ),
        QueryError::Forbidden => error::response(
            request_id,
            ErrorCode::Forbidden,
            "The requested query scope is not permitted.",
            None,
        ),
    }
}

pub fn bodyless_error(request_id: Uuid) -> Response {
    error::response(
        request_id,
        ErrorCode::InvalidRequest,
        "This operation does not accept a request body.",
        None,
    )
}

pub fn decoded_ok() -> Response {
    let mut response = Response::new(axum::body::Body::empty());
    *response.status_mut() = StatusCode::NO_CONTENT;
    response
}

/// Reusable Part 05 error mapping. The caller supplies the production envelope's
/// request ID; no creation endpoint is registered until Part 06.
pub fn creation_error(request_id: Uuid, failure: crate::rate_limit::CreationFailure) -> Response {
    use crate::rate_limit::CreationFailure;
    let (code, message) = match failure {
        CreationFailure::InvalidCredentials => (
            ErrorCode::InvalidCredentials,
            "The supplied credentials are invalid.",
        ),
        CreationFailure::AnonymousCreationDisabled => (
            ErrorCode::AnonymousCreationDisabled,
            "Anonymous creation is disabled.",
        ),
        CreationFailure::Forbidden => (ErrorCode::Forbidden, "The operation is not permitted."),
        CreationFailure::RateLimited(_) => (
            ErrorCode::RateLimited,
            "The creation allowance has been exceeded.",
        ),
        CreationFailure::StorageUnavailable | CreationFailure::CommitOutcomeUnknown => (
            ErrorCode::StorageUnavailable,
            "Creation allowance storage is temporarily unavailable.",
        ),
        CreationFailure::Internal => (ErrorCode::InternalError, "An internal error occurred."),
    };
    let header = if let CreationFailure::RateLimited(retry) = failure {
        Some((
            http::header::RETRY_AFTER,
            http::HeaderValue::from_str(&retry.seconds().to_string())
                .expect("positive integer header"),
        ))
    } else {
        None
    };
    tracing::info!(request_id = %request_id, category = failure.safe_category(), "creation attempt rejected");
    error::response(request_id, code, message, header)
}
