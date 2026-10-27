use axum::{
    Router,
    extract::{DefaultBodyLimit, Request, State},
    middleware,
    routing::any,
};
use http::Method;
use tower::limit::ConcurrencyLimitLayer;
use uuid::Uuid;

use crate::{
    app::AppState,
    db::{self, ProbeResult},
    error::{self, ErrorCode},
};

const MAX_API_REQUEST_BODY_BYTES: usize = 64 * 1024;

use super::{request_id, response};

pub fn router(state: AppState) -> Router {
    let max_in_flight = state.http_max_in_flight;
    Router::new()
        .route("/api/health/live", any(liveness))
        .route("/api/health/ready", any(readiness))
        .fallback(not_found)
        .with_state(state)
        .layer(ConcurrencyLimitLayer::new(max_in_flight))
        .layer(DefaultBodyLimit::max(MAX_API_REQUEST_BODY_BYTES))
        .layer(middleware::from_fn(request_id::envelope))
}

async fn liveness(request: Request) -> axum::response::Response {
    let request_id = request
        .extensions()
        .get::<request_id::RequestId>()
        .map_or_else(Uuid::new_v4, |request_id| request_id.0);
    if request.method() != Method::GET {
        return method_not_allowed(request_id, "GET");
    }
    response::health(request_id, true)
}

async fn readiness(State(state): State<AppState>, request: Request) -> axum::response::Response {
    let request_id = request
        .extensions()
        .get::<request_id::RequestId>()
        .map_or_else(Uuid::new_v4, |request_id| request_id.0);
    if request.method() != Method::GET {
        return method_not_allowed(request_id, "GET");
    }

    let ready = matches!(
        db::check_readiness(&state.pool, &state.readiness, state.acquire_timeout).await,
        ProbeResult::Ready
    );
    if !ready {
        tracing::warn!(
            request_id = %request_id,
            category = "readiness_not_ready",
            "readiness probe did not pass"
        );
    }
    response::health(request_id, ready)
}

async fn not_found(request: Request) -> axum::response::Response {
    let request_id = request
        .extensions()
        .get::<request_id::RequestId>()
        .map_or_else(Uuid::new_v4, |request_id| request_id.0);
    error::response(
        request_id,
        ErrorCode::NotFound,
        "The requested resource was not found.",
        None,
    )
}

fn method_not_allowed(request_id: Uuid, allow: &'static str) -> axum::response::Response {
    request_id::method_error(request_id, allow)
}
