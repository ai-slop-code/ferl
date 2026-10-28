//! Exact public short-link GET/HEAD redirect dispatch.

use axum::{body::Body, extract::Request, response::Response};
use http::{Method, StatusCode, header};
use uuid::Uuid;

use crate::{
    analytics::{VisitEvent, VisitMethod},
    app::AppState,
    domain::short_code::ShortCode,
    error::{self, ErrorCode},
    http::request_id::{self, RequestId},
    links::{LinkFailure, resolve_active_link},
    telemetry::metrics::{self, AnalyticsOutcome, Operation, Outcome},
};

const ALLOW_FOLLOW: &str = "GET, HEAD";

pub async fn dispatch(state: AppState, request: Request<Body>) -> Response {
    let request_id = request
        .extensions()
        .get::<RequestId>()
        .map_or_else(Uuid::new_v4, |request_id| request_id.0);
    let raw_path = request.uri().path();
    let Some(code) = exact_code(raw_path).map(str::to_owned) else {
        return not_found(request_id);
    };

    let visit_method = match *request.method() {
        Method::GET => VisitMethod::Get,
        Method::HEAD => VisitMethod::Head,
        _ => return request_id::method_error(request_id, ALLOW_FOLLOW),
    };

    match tokio::time::timeout(
        state.link_operation_timeout,
        follow(&state, request, request_id, &code, visit_method),
    )
    .await
    {
        Ok(response) => response,
        Err(_) => {
            tracing::warn!(
                request_id = %request_id,
                category = "redirect_operation_deadline",
                "short-link resolution exceeded its operation deadline"
            );
            metrics::record_operation(Operation::Redirect, Outcome::Unavailable);
            storage_unavailable(request_id)
        }
    }
}

fn exact_code(path: &str) -> Option<&str> {
    if path == "/" || path == "/api" || path.starts_with("/api/") || path.starts_with("/assets/") {
        return None;
    }
    let code = path.strip_prefix('/')?;
    if code.is_empty() || code.contains('/') || ShortCode::parse_custom(code).is_err() {
        return None;
    }
    Some(code)
}

async fn follow(
    state: &AppState,
    request: Request<Body>,
    request_id: Uuid,
    code: &str,
    method: VisitMethod,
) -> Response {
    let snapshot = match resolve_active_link(&state.pool, code, state.link_operation_timeout).await
    {
        Ok(Some(snapshot)) => snapshot,
        Ok(None) => {
            metrics::record_operation(Operation::Redirect, Outcome::Rejected);
            return not_found(request_id);
        }
        Err(LinkFailure::StorageUnavailable) => {
            tracing::warn!(
                request_id = %request_id,
                category = "redirect_storage_unavailable",
                "short-link resolution storage is unavailable"
            );
            metrics::record_operation(Operation::Redirect, Outcome::Unavailable);
            return storage_unavailable(request_id);
        }
        Err(_) => {
            tracing::error!(
                request_id = %request_id,
                category = "redirect_resolution_internal",
                "short-link resolution failed an integrity check"
            );
            metrics::record_operation(Operation::Redirect, Outcome::Failure);
            return error::response(
                request_id,
                ErrorCode::InternalError,
                "An internal error occurred.",
                None,
            );
        }
    };

    let location = snapshot.destination_url().location_header();
    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::FOUND;
    response.headers_mut().insert(header::LOCATION, location);

    // The response and its Location are fully constructed before a visit is
    // minted. Transport identity/optional-field issues only drop analytics;
    // they cannot replace or delay the successful response.
    let (parts, _body) = request.into_parts();
    match state.client_ip.resolve_request(&parts) {
        Ok(client_ip) => {
            let event = VisitEvent::for_redirect(&snapshot, method, client_ip, &parts.headers);
            state.analytics.try_enqueue(event);
        }
        Err(_) => {
            tracing::warn!(
                request_id = %request_id,
                category = "analytics_transport_peer_missing",
                "redirect issued without an analytics event because transport peer identity was unavailable"
            );
            metrics::record_analytics(AnalyticsOutcome::MissingTransportPeer, Some(0));
        }
    }
    metrics::record_operation(Operation::Redirect, Outcome::Success);
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

fn storage_unavailable(request_id: Uuid) -> Response {
    error::response(
        request_id,
        ErrorCode::StorageUnavailable,
        "Link storage is temporarily unavailable.",
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::exact_code;

    #[test]
    fn public_path_matching_uses_the_unmodified_single_segment() {
        for path in ["/assets", "/health", "/api-sale", "/valid-code"] {
            assert!(exact_code(path).is_some(), "{path}");
        }
        for path in [
            "/",
            "/api",
            "/api/unknown",
            "/assets/example.js",
            "/assets/",
            "/valid-code/",
            "/valid-code/extra",
            "/%76alid-code",
            "/VALID-CODE",
            "//valid-code",
        ] {
            assert_eq!(exact_code(path), None, "{path}");
        }
    }
}
