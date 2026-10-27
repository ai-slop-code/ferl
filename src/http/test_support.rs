//! Test-only route composition for exercising strict decoders through the same
//! request-ID, no-store, error-sanitizing, and HEAD envelope as production.
//! This router is never registered by `http::router` or the server executable.

use axum::{
    Router,
    body::Body,
    extract::Request,
    middleware,
    response::Response,
    routing::{any, get, post},
};
use http::Method;

use super::{
    body::{read_json_object, require_bodyless},
    dto::{CreatorKind, PatchAccountRequest, PatchLinkRequest, RawCreateLink},
    query::{
        AccountListQuery, BreakdownQuery, LinkListQuery, TagDiscoveryQuery, VisitListQuery,
        VisitStatsQuery,
    },
    rejection,
    request_id::{self, RequestId},
};

pub fn decoder_router() -> Router {
    Router::new()
        .route("/decode/create", post(create))
        .route("/decode/patch", post(patch))
        .route("/decode/account-patch", post(account_patch))
        .route("/decode/bodyless", any(bodyless))
        .route("/decode/accounts", get(accounts))
        .route("/decode/links", get(links))
        .route("/decode/tags", get(tags))
        .route("/decode/visits", get(visits))
        .route("/decode/stats", get(stats))
        .route("/decode/breakdown", get(breakdown))
        .layer(middleware::from_fn(request_id::envelope))
}

async fn create(request: Request) -> Response {
    let id = request_id(&request);
    let creator = if request.uri().query() == Some("creator=anonymous") {
        CreatorKind::Anonymous
    } else {
        CreatorKind::Account
    };
    let (parts, body) = request.into_parts();
    let raw = match read_json_object(&parts.headers, body).await {
        Ok(raw) => raw,
        Err(error) => return rejection::body_error(id, error),
    };
    match RawCreateLink::from_object(raw).validate(creator) {
        Ok(_) => rejection::decoded_ok(),
        Err(error) => rejection::request_error(id, error),
    }
}

async fn patch(request: Request) -> Response {
    let id = request_id(&request);
    let (parts, body) = request.into_parts();
    let raw = match read_json_object(&parts.headers, body).await {
        Ok(raw) => raw,
        Err(error) => return rejection::body_error(id, error),
    };
    match PatchLinkRequest::parse(raw) {
        Ok(_) => rejection::decoded_ok(),
        Err(error) => rejection::request_error(id, error),
    }
}

async fn account_patch(request: Request) -> Response {
    let id = request_id(&request);
    let (parts, body) = request.into_parts();
    let raw = match read_json_object(&parts.headers, body).await {
        Ok(raw) => raw,
        Err(error) => return rejection::body_error(id, error),
    };
    match PatchAccountRequest::parse(raw) {
        Ok(_) => rejection::decoded_ok(),
        Err(error) => rejection::request_error(id, error),
    }
}

async fn bodyless(request: Request) -> Response {
    let id = request_id(&request);
    if !matches!(*request.method(), Method::POST | Method::DELETE) {
        return crate::http::request_id::method_error(id, "POST, DELETE");
    }
    match require_bodyless(request.into_body()).await {
        Ok(()) => rejection::decoded_ok(),
        Err(_) => rejection::bodyless_error(id),
    }
}

async fn accounts(request: Request) -> Response {
    parse_query(request, AccountListQuery::parse)
}

async fn links(request: Request) -> Response {
    parse_query(request, LinkListQuery::parse)
}

async fn tags(request: Request) -> Response {
    parse_query(request, TagDiscoveryQuery::parse)
}

async fn visits(request: Request) -> Response {
    parse_query(request, VisitListQuery::parse)
}

async fn stats(request: Request) -> Response {
    parse_query(request, VisitStatsQuery::parse)
}

async fn breakdown(request: Request) -> Response {
    parse_query(request, BreakdownQuery::parse)
}

fn parse_query<T>(
    request: Request,
    parse: impl FnOnce(Option<&str>) -> Result<T, super::query::QueryError>,
) -> Response {
    let id = request_id(&request);
    match parse(request.uri().query()) {
        Ok(_) => rejection::decoded_ok(),
        Err(error) => rejection::query_error(id, error),
    }
}

fn request_id(request: &Request<Body>) -> uuid::Uuid {
    request
        .extensions()
        .get::<RequestId>()
        .map_or_else(uuid::Uuid::new_v4, |id| id.0)
}
