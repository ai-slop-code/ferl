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
