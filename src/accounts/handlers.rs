use std::time::Duration;

use axum::{body::Body, extract::Request, response::Response};
use http::{Method, StatusCode};
use uuid::Uuid;

use crate::{
    app::AppState,
    auth::{AccountRole, AuthFailure, Principal, authenticate},
    domain::ids::AccountId,
    error::{self, ErrorCode},
    http::{
        body,
        dto::{CreateAccountRequest, PatchAccountRequest},
        query::{AccountListQuery, QueryError},
        rejection,
        request_id::{self, RequestId, success_json},
    },
};

use super::service::{self, AccountFailure};

const OPERATION_DEADLINE: Duration = Duration::from_secs(45);

pub async fn dispatch(state: AppState, request: Request<Body>) -> Response {
    let request_id = request
        .extensions()
        .get::<RequestId>()
        .map_or_else(Uuid::new_v4, |request_id| request_id.0);
    match tokio::time::timeout(OPERATION_DEADLINE, dispatch_inner(state, request)).await {
        Ok(response) => response,
        Err(_) => {
            tracing::error!(
                request_id = %request_id,
                category = "account_operation_deadline",
                "account operation exceeded its deadline"
            );
            error::response(
                request_id,
                ErrorCode::StorageUnavailable,
                "Account storage is temporarily unavailable.",
                None,
            )
        }
    }
}

async fn dispatch_inner(state: AppState, request: Request<Body>) -> Response {
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

    let (parts, body) = request.into_parts();
    let principal = match authenticate(&state.pool, &parts.headers, state.acquire_timeout).await {
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

    match route {
        Route::Current => {
            let Some(account) = principal.account() else {
                return forbidden(request_id);
            };
            if let Some(response) = reject_query(request_id, query.as_deref()) {
                return response;
            }
            match service::current_account(&state.pool, account.id()).await {
                Ok(dto) => json_response(StatusCode::OK, &dto, request_id),
                Err(error) => service_error(request_id, error),
            }
        }
        Route::RotateCurrentKey => {
            if principal.account().is_none() {
                return forbidden(request_id);
            }
            if let Some(response) = reject_query(request_id, query.as_deref()) {
                return response;
            }
            if body::require_bodyless(body).await.is_err() {
                return rejection::bodyless_error(request_id);
            }
            match service::rotate_current_key(&state.pool, &principal, state.lock_timeout).await {
                Ok(dto) => json_response(StatusCode::OK, &dto, request_id),
                Err(error) => service_error(request_id, error),
            }
        }
        Route::ListAccounts => {
            if !require_admin(&principal) {
                return forbidden(request_id);
            }
            let query = match AccountListQuery::parse(query.as_deref()) {
                Ok(query) => query,
                Err(error) => return rejection::query_error(request_id, error),
            };
            let actor = principal.account().expect("admin is an account").id();
            match service::list_accounts(&state.pool, &state.cursor_codec, actor, &query).await {
                Ok(dto) => json_response(StatusCode::OK, &dto, request_id),
                Err(error) => service_error(request_id, error),
            }
        }
        Route::CreateAccount => {
            if !require_admin(&principal) {
                return forbidden(request_id);
            }
            if let Some(response) = reject_query(request_id, query.as_deref()) {
                return response;
            }
            let object = match body::read_json_object(&parts.headers, body).await {
                Ok(object) => object,
                Err(error) => return rejection::body_error(request_id, error),
            };
            let input = match CreateAccountRequest::parse(object) {
                Ok(input) => input,
                Err(error) => return rejection::request_error(request_id, error),
            };
            match service::create_account(
                &state.pool,
                &principal,
                state.lock_timeout,
                input.username().as_str(),
                input.role(),
            )
            .await
            {
                Ok(dto) => json_response(StatusCode::CREATED, &dto, request_id),
                Err(error) => service_error(request_id, error),
            }
        }
        Route::GetAccount(raw_id) => {
            if !require_admin(&principal) {
                return forbidden(request_id);
            }
            if let Some(response) = reject_query(request_id, query.as_deref()) {
                return response;
            }
            let id = match parse_account_id(raw_id) {
                Ok(id) => id,
                Err(()) => return invalid_account_id(request_id),
            };
            match service::get_account(&state.pool, id).await {
                Ok(dto) => json_response(StatusCode::OK, &dto, request_id),
                Err(error) => service_error(request_id, error),
            }
        }
        Route::UpdateAccount(raw_id) => {
            if !require_admin(&principal) {
                return forbidden(request_id);
            }
            if let Some(response) = reject_query(request_id, query.as_deref()) {
                return response;
            }
            let id = match parse_account_id(raw_id) {
                Ok(id) => id,
                Err(()) => return invalid_account_id(request_id),
            };
            let object = match body::read_json_object(&parts.headers, body).await {
                Ok(object) => object,
                Err(error) => return rejection::body_error(request_id, error),
            };
            let patch = match PatchAccountRequest::parse(object) {
                Ok(patch) => patch,
                Err(error) => return rejection::request_error(request_id, error),
            };
            match service::update_account(&state.pool, &principal, state.lock_timeout, id, patch)
                .await
            {
                Ok(dto) => json_response(StatusCode::OK, &dto, request_id),
                Err(error) => service_error(request_id, error),
            }
        }
        Route::DeleteAccount(raw_id) => {
            if !require_admin(&principal) {
                return forbidden(request_id);
            }
            if let Some(response) = reject_query(request_id, query.as_deref()) {
                return response;
            }
            let id = match parse_account_id(raw_id) {
                Ok(id) => id,
                Err(()) => return invalid_account_id(request_id),
            };
            if body::require_bodyless(body).await.is_err() {
                return rejection::bodyless_error(request_id);
            }
            match service::delete_account(&state.pool, &principal, state.lock_timeout, id).await {
                Ok(()) => empty_response(StatusCode::NO_CONTENT),
                Err(error) => service_error(request_id, error),
            }
        }
        Route::ResetAccountKey(raw_id) => {
            if !require_admin(&principal) {
                return forbidden(request_id);
            }
            if let Some(response) = reject_query(request_id, query.as_deref()) {
                return response;
            }
            let id = match parse_account_id(raw_id) {
                Ok(id) => id,
                Err(()) => return invalid_account_id(request_id),
            };
            if body::require_bodyless(body).await.is_err() {
                return rejection::bodyless_error(request_id);
            }
            match service::reset_account_key(&state.pool, &principal, state.lock_timeout, id).await
            {
                Ok(dto) => json_response(StatusCode::OK, &dto, request_id),
                Err(error) => service_error(request_id, error),
            }
        }
        Route::MethodNotAllowed(_) => unreachable!("method errors returned before dispatch"),
    }
}

#[derive(Clone, Copy)]
enum Route<'a> {
    Current,
    RotateCurrentKey,
    ListAccounts,
    CreateAccount,
    GetAccount(&'a str),
    UpdateAccount(&'a str),
    DeleteAccount(&'a str),
    ResetAccountKey(&'a str),
    MethodNotAllowed(&'static str),
}

fn select_route<'a>(path: &'a str, method: &Method) -> Option<Route<'a>> {
    match path {
        "/api/v1/me" => Some(if *method == Method::GET {
            Route::Current
        } else {
            Route::MethodNotAllowed("GET")
        }),
        "/api/v1/me/api-key" => Some(if *method == Method::POST {
            Route::RotateCurrentKey
        } else {
            Route::MethodNotAllowed("POST")
        }),
        "/api/v1/admin/accounts" => Some(if *method == Method::GET {
            Route::ListAccounts
        } else if *method == Method::POST {
            Route::CreateAccount
        } else {
            Route::MethodNotAllowed("GET, POST")
        }),
        _ => {
            let raw = path.strip_prefix("/api/v1/admin/accounts/")?;
            if let Some(id) = raw.strip_suffix("/api-key") {
                if !id.contains('/') && !id.is_empty() {
                    return Some(if *method == Method::POST {
                        Route::ResetAccountKey(id)
                    } else {
                        Route::MethodNotAllowed("POST")
                    });
                }
                return None;
            }
            if raw.is_empty() || raw.contains('/') {
                return None;
            }
            Some(match *method {
                Method::GET => Route::GetAccount(raw),
                Method::PATCH => Route::UpdateAccount(raw),
                Method::DELETE => Route::DeleteAccount(raw),
                _ => Route::MethodNotAllowed("GET, PATCH, DELETE"),
            })
        }
    }
}

fn require_admin(principal: &Principal) -> bool {
    principal
        .account()
        .is_some_and(|account| account.role() == AccountRole::Admin)
}

fn parse_account_id(value: &str) -> Result<AccountId, ()> {
    Uuid::parse_str(value)
        .ok()
        .filter(|id| id.to_string().eq_ignore_ascii_case(value))
        .map(AccountId::new)
        .ok_or(())
}

fn invalid_account_id(request_id: Uuid) -> Response {
    error::response(
        request_id,
        ErrorCode::InvalidRequest,
        "The account identifier is invalid.",
        None,
    )
}

fn reject_query(request_id: Uuid, raw: Option<&str>) -> Option<Response> {
    if raw.is_some_and(|value| !value.is_empty()) {
        Some(rejection::query_error(
            request_id,
            QueryError::InvalidRequest,
        ))
    } else {
        None
    }
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
            tracing::warn!(request_id = %request_id, category = "invalid_credentials", "credential authentication rejected");
            error::response(
                request_id,
                ErrorCode::InvalidCredentials,
                "The supplied credentials are invalid.",
                None,
            )
        }
        AuthFailure::StorageUnavailable => {
            tracing::error!(request_id = %request_id, category = "authentication_storage_unavailable", "credential lookup failed safely");
            error::response(
                request_id,
                ErrorCode::StorageUnavailable,
                "Account storage is temporarily unavailable.",
                None,
            )
        }
        AuthFailure::Internal => {
            tracing::error!(request_id = %request_id, category = "authentication_persistence_failure", "credential data could not be decoded");
            error::response(
                request_id,
                ErrorCode::InternalError,
                "An internal error occurred.",
                None,
            )
        }
    }
}

fn service_error(request_id: Uuid, failure: AccountFailure) -> Response {
    let (code, message) = match failure {
        AccountFailure::AuthenticationRequired => (
            ErrorCode::AuthenticationRequired,
            "Authentication is required.",
        ),
        AccountFailure::InvalidCredentials => (
            ErrorCode::InvalidCredentials,
            "The supplied credentials are invalid.",
        ),
        AccountFailure::Forbidden => (ErrorCode::Forbidden, "The operation is not permitted."),
        AccountFailure::NotFound => (ErrorCode::NotFound, "The requested resource was not found."),
        AccountFailure::InvalidRequest => (
            ErrorCode::InvalidRequest,
            "One or more request values are invalid.",
        ),
        AccountFailure::UsernameUnavailable => (
            ErrorCode::UsernameUnavailable,
            "The requested username is unavailable.",
        ),
        AccountFailure::LastAdminRequired => (
            ErrorCode::LastAdminRequired,
            "At least one enabled administrator must remain.",
        ),
        AccountFailure::StorageUnavailable => (
            ErrorCode::StorageUnavailable,
            "Account storage is temporarily unavailable.",
        ),
        AccountFailure::Internal => (ErrorCode::InternalError, "An internal error occurred."),
    };
    if matches!(
        failure,
        AccountFailure::StorageUnavailable | AccountFailure::Internal
    ) {
        tracing::error!(
            request_id = %request_id,
            category = failure.safe_category(),
            "account operation failed with a sanitized category"
        );
    }
    error::response(request_id, code, message, None)
}

fn forbidden(request_id: Uuid) -> Response {
    error::response(
        request_id,
        ErrorCode::Forbidden,
        "The operation is not permitted.",
        None,
    )
}

fn not_found(request_id: Uuid) -> Response {
    error::response(
        request_id,
        ErrorCode::NotFound,
        "The requested resource was not found.",
        None,
    )
}

fn json_response(status: StatusCode, value: &impl serde::Serialize, request_id: Uuid) -> Response {
    match serde_json::to_vec(value) {
        Ok(bytes) => success_json(status, bytes),
        Err(_) => {
            tracing::error!(request_id = %request_id, category = "account_response_serialization_failure", "account response could not be serialized");
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
