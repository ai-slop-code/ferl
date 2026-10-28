use std::time::Duration;

use axum::{body::Body, extract::Request, response::Response};
use uuid::Uuid;

use crate::{
    app::AppState,
    auth::{AccountRole, authenticate},
    error::{self, ErrorCode},
    http::{query::LinkListQuery, rejection},
};

use super::handlers::{authentication_error, json_response, link_error};

const AUTHENTICATION_DEADLINE: Duration = Duration::from_secs(5);

pub(crate) async fn dispatch(
    state: AppState,
    request: Request<Body>,
    request_id: Uuid,
) -> Response {
    let raw_query = request.uri().query().map(str::to_owned);
    let (parts, _body) = request.into_parts();
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
    let Some(account) = principal.account() else {
        return error::response(
            request_id,
            ErrorCode::Forbidden,
            "The operation is not permitted.",
            None,
        );
    };
    let query = match LinkListQuery::parse_authorized(
        raw_query.as_deref(),
        account.role() == AccountRole::Admin,
    ) {
        Ok(query) => query,
        Err(failure) => return rejection::query_error(request_id, failure),
    };
    match state
        .links
        .list(&state.cursor_codec, account.id(), &query)
        .await
    {
        Ok(collection) => json_response(http::StatusCode::OK, &collection, request_id),
        Err(failure) => link_error(request_id, failure),
    }
}
