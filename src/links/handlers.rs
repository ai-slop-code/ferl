use std::time::Duration;

use axum::{body::Body, extract::Request, response::Response};
use http::{Method, StatusCode};
use uuid::Uuid;

use crate::{
    app::AppState,
    auth::{AuthFailure, authenticate},
    error::{self, ErrorCode},
    http::{
        body,
        dto::{CreatorKind, RawCreateLink},
        query::QueryError,
        rejection,
        request_id::{self, RequestId, success_json},
    },
};

use super::service::{CreatedLink, LinkFailure};

const AUTHENTICATION_DEADLINE: Duration = Duration::from_secs(5);

pub(crate) fn handles_path(path: &str) -> bool {
    path == "/api/v1/links" || path.strip_prefix("/api/v1/links/").is_some()
}

pub async fn dispatch(state: AppState, request: Request<Body>) -> Response {
    let request_id = request
        .extensions()
        .get::<RequestId>()
        .map_or_else(Uuid::new_v4, |request_id| request_id.0);
    let path = request.uri().path().to_owned();
    let method = request.method().clone();
    let query = request.uri().query().map(str::to_owned);
    let Some(route) = select_route(&path, &method) else {
        return not_found(request_id);
    };
    if let Route::MethodNotAllowed(allow) = route {
        return request_id::method_error(request_id, allow);
    }

    if matches!(route, Route::Create) {
        return create(state, request, request_id, query.as_deref()).await;
    }

    let (parts, request_body) = request.into_parts();
    let principal = match authenticate(
        &state.pool,
        &parts.headers,
        AUTHENTICATION_DEADLINE.min(state.acquire_timeout),
    )
    .await
    {
        Ok(Some(principal)) => principal,
        Ok(None) => {
            return error::response(
                request_id,
                ErrorCode::AuthenticationRequired,
                "Authentication is required.",
                None,
            );
        }
        Err(failure) => return authentication_error(request_id, failure),
    };
    let raw_code = match route {
        Route::Get(code) | Route::Patch(code) | Route::Delete(code) | Route::RotateToken(code) => {
            code
        }
        Route::Create | Route::MethodNotAllowed(_) => unreachable!(),
    };
    if crate::domain::short_code::ShortCode::parse_custom(raw_code).is_err() {
        return rejection::request_error(
            request_id,
            crate::http::dto::RequestValidationError::InvalidRequest,
        );
    }

    match route {
        Route::Get(code) => {
            let dto = match state.links.get(&principal, code).await {
                Ok(dto) => dto,
                Err(failure) => return link_error(request_id, failure),
            };
            if let Some(response) = reject_query(request_id, query.as_deref()) {
                return response;
            }
            json_response(StatusCode::OK, &dto, request_id)
        }
        Route::Patch(code) => {
            if let Err(failure) = state.links.authorize_management(&principal, code).await {
                return link_error(request_id, failure);
            }
            if let Some(response) = reject_query(request_id, query.as_deref()) {
                return response;
            }
            let object = match body::read_json_object(&parts.headers, request_body).await {
                Ok(object) => object,
                Err(failure) => return rejection::body_error(request_id, failure),
            };
            let patch = match crate::http::dto::PatchLinkRequest::parse(object) {
                Ok(patch) => patch,
                Err(failure) => return rejection::request_error(request_id, failure),
            };
            match state.links.patch(&principal, code, patch).await {
                Ok(dto) => json_response(StatusCode::OK, &dto, request_id),
                Err(failure) => link_error(request_id, failure),
            }
        }
        Route::Delete(code) => {
            if let Err(failure) = state.links.authorize_management(&principal, code).await {
                return link_error(request_id, failure);
            }
            if let Some(response) = reject_query(request_id, query.as_deref()) {
                return response;
            }
            if body::require_bodyless(request_body).await.is_err() {
                return rejection::bodyless_error(request_id);
            }
            match state.links.delete(&principal, code).await {
                Ok(()) => empty_response(StatusCode::NO_CONTENT),
                Err(failure) => link_error(request_id, failure),
            }
        }
        Route::RotateToken(code) => {
            if let Err(failure) = state
                .links
                .authorize_token_replacement(&principal, code)
                .await
            {
                return link_error(request_id, failure);
            }
            if let Some(response) = reject_query(request_id, query.as_deref()) {
                return response;
            }
            if body::require_bodyless(request_body).await.is_err() {
                return rejection::bodyless_error(request_id);
            }
            match state.links.rotate_token(&principal, code).await {
                Ok(dto) => json_response(StatusCode::OK, &dto, request_id),
                Err(failure) => link_error(request_id, failure),
            }
        }
        Route::Create | Route::MethodNotAllowed(_) => unreachable!("handled before authentication"),
    }
}

async fn create(
    state: AppState,
    request: Request<Body>,
    request_id: Uuid,
    query: Option<&str>,
) -> Response {
    let (parts, request_body) = request.into_parts();
    let attempt = match state.creation.prepare(&parts).await {
        Ok(attempt) => attempt,
        Err(failure) => return rejection::creation_error(request_id, failure),
    };
    if let Some(response) = reject_query(request_id, query) {
        return response;
    }
    let kind = if attempt.creator().principal().is_some() {
        CreatorKind::Account
    } else {
        CreatorKind::Anonymous
    };
    let object = match body::read_json_object(&parts.headers, request_body).await {
        Ok(object) => object,
        Err(failure) => return rejection::body_error(request_id, failure),
    };
    let input = match RawCreateLink::from_object(object).validate(kind) {
        Ok(input) => input,
        Err(failure) => return rejection::request_error(request_id, failure),
    };
    match state.links.create(attempt, input).await {
        Ok(CreatedLink::Anonymous(dto)) => json_response(StatusCode::CREATED, &dto, request_id),
        Ok(CreatedLink::Owned(dto)) => json_response(StatusCode::CREATED, &dto, request_id),
        Err(failure) => link_error(request_id, failure),
    }
}

#[derive(Clone, Copy)]
enum Route<'a> {
    Create,
    Get(&'a str),
    Patch(&'a str),
    Delete(&'a str),
    RotateToken(&'a str),
    MethodNotAllowed(&'static str),
}

fn select_route<'a>(path: &'a str, method: &Method) -> Option<Route<'a>> {
    if path == "/api/v1/links" {
        return Some(if *method == Method::POST {
            Route::Create
        } else {
            Route::MethodNotAllowed("POST")
        });
    }
    let tail = path.strip_prefix("/api/v1/links/")?;
    if let Some(code) = tail.strip_suffix("/management-token") {
        if code.is_empty() || code.contains('/') {
            return None;
        }
        return Some(if *method == Method::POST {
            Route::RotateToken(code)
        } else {
            Route::MethodNotAllowed("POST")
        });
    }
    if tail.is_empty() || tail.contains('/') {
        return None;
    }
    Some(match *method {
        Method::GET => Route::Get(tail),
        Method::PATCH => Route::Patch(tail),
        Method::DELETE => Route::Delete(tail),
        _ => Route::MethodNotAllowed("GET, PATCH, DELETE"),
    })
}

fn reject_query(request_id: Uuid, raw: Option<&str>) -> Option<Response> {
    raw.filter(|value| !value.is_empty())
        .map(|_| rejection::query_error(request_id, QueryError::InvalidRequest))
}

fn authentication_error(request_id: Uuid, failure: AuthFailure) -> Response {
    match failure {
        AuthFailure::Missing => error::response(
            request_id,
            ErrorCode::AuthenticationRequired,
            "Authentication is required.",
            None,
        ),
        AuthFailure::Invalid => {
            tracing::warn!(request_id = %request_id, category = "invalid_credentials", "link credential authentication rejected");
            error::response(
                request_id,
                ErrorCode::InvalidCredentials,
                "The supplied credentials are invalid.",
                None,
            )
        }
        AuthFailure::StorageUnavailable => {
            tracing::error!(request_id = %request_id, category = "link_authentication_storage_unavailable", "link credential lookup failed safely");
            error::response(
                request_id,
                ErrorCode::StorageUnavailable,
                "Link storage is temporarily unavailable.",
                None,
            )
        }
        AuthFailure::Internal => {
            tracing::error!(request_id = %request_id, category = "link_authentication_internal_failure", "link credential data could not be decoded");
            error::response(
                request_id,
                ErrorCode::InternalError,
                "An internal error occurred.",
                None,
            )
        }
    }
}

fn link_error(request_id: Uuid, failure: LinkFailure) -> Response {
    let (code, message) = match failure {
        LinkFailure::AuthenticationRequired => (
            ErrorCode::AuthenticationRequired,
            "Authentication is required.",
        ),
        LinkFailure::InvalidCredentials => (
            ErrorCode::InvalidCredentials,
            "The supplied credentials are invalid.",
        ),
        LinkFailure::Forbidden => (ErrorCode::Forbidden, "The operation is not permitted."),
        LinkFailure::NotFound => (ErrorCode::NotFound, "The requested resource was not found."),
        LinkFailure::InvalidRequest => (
            ErrorCode::InvalidRequest,
            "One or more request values are invalid.",
        ),
        LinkFailure::ShortCodeUnavailable => (
            ErrorCode::ShortCodeUnavailable,
            "The requested short code is unavailable.",
        ),
        LinkFailure::CodeGenerationUnavailable => (
            ErrorCode::CodeGenerationUnavailable,
            "A short code could not be generated at this time.",
        ),
        LinkFailure::NotAnonymousLink => (
            ErrorCode::NotAnonymousLink,
            "Management tokens apply only to anonymous links.",
        ),
        LinkFailure::StorageUnavailable | LinkFailure::CommitOutcomeUnknown => (
            ErrorCode::StorageUnavailable,
            "Link storage is temporarily unavailable.",
        ),
        LinkFailure::Internal => (ErrorCode::InternalError, "An internal error occurred."),
    };
    if matches!(
        failure,
        LinkFailure::StorageUnavailable | LinkFailure::CommitOutcomeUnknown | LinkFailure::Internal
    ) {
        tracing::error!(request_id = %request_id, category = failure.safe_category(), "link operation failed with a sanitized category");
    } else {
        tracing::info!(
            request_id = %request_id,
            operation = "link.management",
            outcome = failure.safe_category(),
            "link operation rejected"
        );
    }
    error::response(request_id, code, message, None)
}

fn json_response(status: StatusCode, value: &impl serde::Serialize, request_id: Uuid) -> Response {
    match serde_json::to_vec(value) {
        Ok(bytes) => success_json(status, bytes),
        Err(_) => {
            tracing::error!(request_id = %request_id, category = "link_response_serialization_failure", "link response could not be serialized");
            error::response(
                request_id,
                ErrorCode::InternalError,
                "An internal error occurred.",
                None,
            )
        }
    }
}

fn empty_response(status: StatusCode) -> Response {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = status;
    response
}

fn not_found(request_id: Uuid) -> Response {
    error::response(
        request_id,
        ErrorCode::NotFound,
        "The requested resource was not found.",
        None,
    )
}

#[cfg(test)]
mod tests {
    use http::Method;

    use super::{Route, handles_path, select_route};

    #[test]
    fn link_dispatch_selects_only_the_part_06_methods() {
        assert!(handles_path("/api/v1/links"));
        assert!(handles_path("/api/v1/links/a"));
        assert!(!handles_path("/api/v1/links-search"));
        assert!(matches!(
            select_route("/api/v1/links", &Method::POST),
            Some(Route::Create)
        ));
        assert!(matches!(
            select_route("/api/v1/links", &Method::GET),
            Some(Route::MethodNotAllowed("POST"))
        ));
        assert!(matches!(
            select_route("/api/v1/links/example", &Method::HEAD),
            Some(Route::MethodNotAllowed("GET, PATCH, DELETE"))
        ));
        assert!(matches!(
            select_route("/api/v1/links/example/management-token", &Method::POST),
            Some(Route::RotateToken("example"))
        ));
        assert!(select_route("/api/v1/links/example/visits", &Method::GET).is_none());
    }
}
