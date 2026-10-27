use http::{HeaderName, HeaderValue, StatusCode};
use serde::Serialize;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorCode {
    InvalidRequest,
    InvalidCredentials,
    AuthenticationRequired,
    Forbidden,
    AnonymousCreationDisabled,
    NotFound,
    MethodNotAllowed,
    ShortCodeUnavailable,
    UsernameUnavailable,
    LastAdminRequired,
    NotAnonymousLink,
    RequestTooLarge,
    UnsupportedMediaType,
    RateLimited,
    StorageUnavailable,
    ServiceUnavailable,
    CodeGenerationUnavailable,
    InternalError,
}

impl ErrorCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid_request",
            Self::InvalidCredentials => "invalid_credentials",
            Self::AuthenticationRequired => "authentication_required",
            Self::Forbidden => "forbidden",
            Self::AnonymousCreationDisabled => "anonymous_creation_disabled",
            Self::NotFound => "not_found",
            Self::MethodNotAllowed => "method_not_allowed",
            Self::ShortCodeUnavailable => "short_code_unavailable",
            Self::UsernameUnavailable => "username_unavailable",
            Self::LastAdminRequired => "last_admin_required",
            Self::NotAnonymousLink => "not_anonymous_link",
            Self::RequestTooLarge => "request_too_large",
            Self::UnsupportedMediaType => "unsupported_media_type",
            Self::RateLimited => "rate_limited",
            Self::StorageUnavailable => "storage_unavailable",
            Self::ServiceUnavailable => "service_unavailable",
            Self::CodeGenerationUnavailable => "code_generation_unavailable",
            Self::InternalError => "internal_error",
        }
    }

    const fn status(self) -> StatusCode {
        match self {
            Self::InvalidRequest => StatusCode::BAD_REQUEST,
            Self::InvalidCredentials | Self::AuthenticationRequired => StatusCode::UNAUTHORIZED,
            Self::Forbidden | Self::AnonymousCreationDisabled => StatusCode::FORBIDDEN,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::MethodNotAllowed => StatusCode::METHOD_NOT_ALLOWED,
            Self::ShortCodeUnavailable
            | Self::UsernameUnavailable
            | Self::LastAdminRequired
            | Self::NotAnonymousLink => StatusCode::CONFLICT,
            Self::RequestTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Self::UnsupportedMediaType => StatusCode::UNSUPPORTED_MEDIA_TYPE,
            Self::RateLimited => StatusCode::TOO_MANY_REQUESTS,
            Self::StorageUnavailable
            | Self::ServiceUnavailable
            | Self::CodeGenerationUnavailable => StatusCode::SERVICE_UNAVAILABLE,
            Self::InternalError => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

#[derive(Serialize)]
struct ErrorEnvelope {
    error: ErrorBody,
    request_id: String,
}

#[derive(Serialize)]
struct ErrorBody {
    code: &'static str,
    message: &'static str,
}

#[derive(Clone)]
pub(crate) struct ContractResponse;

pub fn response(
    request_id: Uuid,
    code: ErrorCode,
    message: &'static str,
    extra_header: Option<(HeaderName, HeaderValue)>,
) -> axum::response::Response {
    let body = ErrorEnvelope {
        error: ErrorBody {
            code: code.as_str(),
            message,
        },
        request_id: request_id.to_string(),
    };
    let body = serde_json::to_vec(&body).expect("static error envelope is serializable");
    let mut response = axum::response::Response::new(axum::body::Body::from(body));
    *response.status_mut() = code.status();
    response.headers_mut().insert(
        http::header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    if matches!(
        code,
        ErrorCode::InvalidCredentials | ErrorCode::AuthenticationRequired
    ) {
        response.headers_mut().insert(
            http::header::WWW_AUTHENTICATE,
            HeaderValue::from_static("Bearer"),
        );
    }
    if let Some((name, value)) = extra_header {
        response.headers_mut().insert(name, value);
    }
    response.extensions_mut().insert(ContractResponse);
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_status_table_matches_the_wire_contract() {
        let cases = [
            (ErrorCode::InvalidRequest, StatusCode::BAD_REQUEST),
            (ErrorCode::InvalidCredentials, StatusCode::UNAUTHORIZED),
            (ErrorCode::AuthenticationRequired, StatusCode::UNAUTHORIZED),
            (ErrorCode::Forbidden, StatusCode::FORBIDDEN),
            (ErrorCode::AnonymousCreationDisabled, StatusCode::FORBIDDEN),
            (ErrorCode::NotFound, StatusCode::NOT_FOUND),
            (ErrorCode::MethodNotAllowed, StatusCode::METHOD_NOT_ALLOWED),
            (ErrorCode::ShortCodeUnavailable, StatusCode::CONFLICT),
            (ErrorCode::UsernameUnavailable, StatusCode::CONFLICT),
            (ErrorCode::LastAdminRequired, StatusCode::CONFLICT),
            (ErrorCode::NotAnonymousLink, StatusCode::CONFLICT),
            (ErrorCode::RequestTooLarge, StatusCode::PAYLOAD_TOO_LARGE),
            (
                ErrorCode::UnsupportedMediaType,
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
            ),
            (ErrorCode::RateLimited, StatusCode::TOO_MANY_REQUESTS),
            (
                ErrorCode::StorageUnavailable,
                StatusCode::SERVICE_UNAVAILABLE,
            ),
            (
                ErrorCode::ServiceUnavailable,
                StatusCode::SERVICE_UNAVAILABLE,
            ),
            (
                ErrorCode::CodeGenerationUnavailable,
                StatusCode::SERVICE_UNAVAILABLE,
            ),
            (ErrorCode::InternalError, StatusCode::INTERNAL_SERVER_ERROR),
        ];
        for (code, expected) in cases {
            assert_eq!(code.status(), expected, "{}", code.as_str());
        }
    }

    #[test]
    fn authentication_errors_set_the_bearer_challenge_and_safe_envelope() {
        let request_id = Uuid::new_v4();
        let response = response(
            request_id,
            ErrorCode::AuthenticationRequired,
            "Authentication is required.",
            None,
        );
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(response.headers()[http::header::WWW_AUTHENTICATE], "Bearer");
        assert_eq!(
            response.headers()[http::header::CONTENT_TYPE],
            "application/json"
        );
    }
}
