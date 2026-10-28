use std::time::Duration;

use axum::{body::Body, extract::Request, response::Response};
use http::{Method, StatusCode};
use uuid::Uuid;

use crate::{
    app::AppState,
    auth::{AccountRole, authenticate},
    db::error::{DatabaseFailure, OperationStage, classify},
    error::{self, ErrorCode},
    http::{rejection, request_id},
    pagination::{CursorBoundary, CursorContext},
};

use super::{query::TagDiscoveryQuery, repository};

const AUTHENTICATION_DEADLINE: Duration = Duration::from_secs(5);

pub(crate) fn handles_path(path: &str) -> bool {
    path == "/api/v1/tags"
}

pub async fn dispatch(state: AppState, request: Request<Body>) -> Response {
    let request_id = request
        .extensions()
        .get::<request_id::RequestId>()
        .map_or_else(Uuid::new_v4, |request_id| request_id.0);
    if request.method() != Method::GET {
        return request_id::method_error(request_id, "GET");
    }
    match tokio::time::timeout(
        state.link_operation_timeout,
        dispatch_inner(state, request, request_id),
    )
    .await
    {
        Ok(response) => response,
        Err(_) => {
            tracing::error!(
                request_id = %request_id,
                category = "tag_discovery_deadline",
                "tag discovery exceeded its operation deadline"
            );
            discovery_error(request_id, DiscoveryFailure::StorageUnavailable)
        }
    }
}

async fn dispatch_inner(state: AppState, request: Request<Body>, request_id: Uuid) -> Response {
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
        Err(failure) => return crate::links::handlers::authentication_error(request_id, failure),
    };
    let Some(account) = principal.account() else {
        return error::response(
            request_id,
            ErrorCode::Forbidden,
            "The operation is not permitted.",
            None,
        );
    };
    let query = match TagDiscoveryQuery::parse_authorized(
        raw_query.as_deref(),
        account.role() == AccountRole::Admin,
    ) {
        Ok(query) => query,
        Err(failure) => return rejection::query_error(request_id, failure),
    };
    let actor = account.id();
    let context = CursorContext::tags(actor, &query);
    let boundary = query
        .page()
        .cursor()
        .map(|cursor| state.cursor_codec.decode(&context, cursor))
        .transpose()
        .map_err(|_| DiscoveryFailure::InvalidRequest);
    let boundary = match boundary {
        Ok(boundary) => boundary,
        Err(failure) => return discovery_error(request_id, failure),
    };
    let mut rows =
        match repository::list(&state.pool, actor.as_uuid(), &query, boundary.as_ref()).await {
            Ok(rows) => rows,
            Err(error) => {
                return discovery_error(
                    request_id,
                    match classify(&error, OperationStage::BeforeCommit) {
                        DatabaseFailure::Unavailable(_) | DatabaseFailure::CommitOutcomeUnknown => {
                            DiscoveryFailure::StorageUnavailable
                        }
                        DatabaseFailure::Internal
                        | DatabaseFailure::Conflict(_)
                        | DatabaseFailure::NotFound => DiscoveryFailure::Internal,
                    },
                );
            }
        };
    let limit = usize::from(query.page().limit());
    let has_more = rows.len() > limit;
    rows.truncate(limit);
    let page_len = rows.len();
    let mut items = Vec::with_capacity(page_len);
    let mut next_cursor = None;
    for (index, row) in rows.into_iter().enumerate() {
        let name = row.name;
        let item = match crate::http::dto::TagInfoDto::new(name.clone(), row.link_count) {
            Ok(item) => item,
            Err(_) => return discovery_error(request_id, DiscoveryFailure::Internal),
        };
        if has_more && index + 1 == page_len {
            next_cursor = match state
                .cursor_codec
                .encode(&context, &CursorBoundary::TagName { name })
            {
                Ok(cursor) => Some(cursor),
                Err(_) => return discovery_error(request_id, DiscoveryFailure::Internal),
            };
        }
        items.push(item);
    }
    let collection = crate::http::dto::CollectionDto::new(items, next_cursor);
    match serde_json::to_vec(&collection) {
        Ok(bytes) => request_id::success_json(StatusCode::OK, bytes),
        Err(_) => discovery_error(request_id, DiscoveryFailure::Internal),
    }
}

#[derive(Clone, Copy)]
enum DiscoveryFailure {
    InvalidRequest,
    StorageUnavailable,
    Internal,
}

fn discovery_error(request_id: Uuid, failure: DiscoveryFailure) -> Response {
    match failure {
        DiscoveryFailure::InvalidRequest => error::response(
            request_id,
            ErrorCode::InvalidRequest,
            "One or more query parameters are invalid.",
            None,
        ),
        DiscoveryFailure::StorageUnavailable => {
            tracing::error!(request_id = %request_id, category = "tag_storage_unavailable", "tag discovery storage failed safely");
            error::response(
                request_id,
                ErrorCode::StorageUnavailable,
                "Tag storage is temporarily unavailable.",
                None,
            )
        }
        DiscoveryFailure::Internal => {
            tracing::error!(request_id = %request_id, category = "tag_persistence_failure", "tag discovery failed with a sanitized category");
            error::response(
                request_id,
                ErrorCode::InternalError,
                "An internal error occurred.",
                None,
            )
        }
    }
}
