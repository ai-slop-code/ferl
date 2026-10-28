use std::time::Duration;

use axum::{body::Body, extract::Request, response::Response};
use http::{Method, StatusCode};
use uuid::Uuid;

use crate::{
    app::AppState,
    auth::{Principal, authenticate},
    domain::short_code::ShortCode,
    error::{self, ErrorCode},
    http::{
        query::{BreakdownQuery, QueryError, VisitListQuery, VisitStatsQuery},
        rejection,
        request_id::{self, RequestId, success_json},
    },
    pagination::{
        CursorActor, CursorContext, breakdown_boundary::BreakdownBoundary,
        visit_boundary::VisitBoundary,
    },
};

use super::reports::ReportFailure;

const AUTHENTICATION_DEADLINE: Duration = Duration::from_secs(5);

#[derive(Clone, Copy)]
enum Route<'a> {
    Visits(&'a str),
    Stats(&'a str),
    Breakdown(&'a str),
    MethodNotAllowed,
}

pub(crate) fn handles_path(path: &str) -> bool {
    select_route(path, &Method::GET).is_some()
}

pub(crate) async fn dispatch(state: AppState, request: Request<Body>) -> Response {
    let request_id = request
        .extensions()
        .get::<RequestId>()
        .map_or_else(Uuid::new_v4, |request_id| request_id.0);
    match tokio::time::timeout(
        state.report_operation_timeout,
        dispatch_inner(state, request, request_id),
    )
    .await
    {
        Ok(response) => response,
        Err(_) => {
            tracing::error!(
                request_id = %request_id,
                category = "report_operation_deadline",
                "analytics report exceeded its configured operation deadline"
            );
            report_error(request_id, ReportFailure::StorageUnavailable)
        }
    }
}

async fn dispatch_inner(state: AppState, request: Request<Body>, request_id: Uuid) -> Response {
    let path = request.uri().path().to_owned();
    let query = request.uri().query().map(str::to_owned);
    let method = request.method().clone();
    let Some(route) = select_route(&path, &method) else {
        return not_found(request_id);
    };
    if matches!(route, Route::MethodNotAllowed) {
        return request_id::method_error(request_id, "GET");
    }

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

    let code = match route {
        Route::Visits(code) | Route::Stats(code) | Route::Breakdown(code) => code,
        Route::MethodNotAllowed => unreachable!("method errors are returned before authentication"),
    };
    if ShortCode::parse_custom(code).is_err() {
        return rejection::request_error(
            request_id,
            crate::http::dto::RequestValidationError::InvalidRequest,
        );
    }

    // This small read intentionally precedes every operation-specific query and
    // cursor operation. It returns no analytics data; each report repeats the
    // check inside its own coherent read-only transaction.
    let link_id = match state.reports.authorize_link(&principal, code).await {
        Ok(link_id) => link_id,
        Err(failure) => return report_error(request_id, failure),
    };

    match route {
        Route::Visits(_) => {
            let query = match VisitListQuery::parse(query.as_deref()) {
                Ok(query) => query,
                Err(failure) => return rejection::query_error(request_id, failure),
            };
            let context = CursorContext::visits(cursor_actor(&principal), link_id, &query);
            let boundary = match query.page().cursor() {
                Some(cursor) => match state.cursor_codec.decode(&context, cursor) {
                    Ok(boundary) => match VisitBoundary::from_cursor(boundary) {
                        Ok(boundary) => Some(boundary),
                        Err(()) => return invalid_cursor(request_id),
                    },
                    Err(_) => return invalid_cursor(request_id),
                },
                None => None,
            };
            let limit = usize::from(query.page().limit());
            let mut rows = match state
                .reports
                .visits(
                    &principal,
                    code,
                    query.interval(),
                    boundary.as_ref(),
                    (limit as i64) + 1,
                )
                .await
            {
                Ok(rows) => rows,
                Err(failure) => return report_error(request_id, failure),
            };
            let has_more = rows.len() > limit;
            rows.truncate(limit);
            let page_len = rows.len();
            let mut items = Vec::with_capacity(page_len);
            let mut next_cursor = None;
            for (index, row) in rows.into_iter().enumerate() {
                if has_more && index + 1 == page_len {
                    let boundary = match VisitBoundary::from_record(&row) {
                        Ok(boundary) => boundary,
                        Err(()) => return report_error(request_id, ReportFailure::Internal),
                    };
                    next_cursor = match state.cursor_codec.encode(&context, &boundary.into_cursor())
                    {
                        Ok(cursor) => Some(cursor),
                        Err(_) => return report_error(request_id, ReportFailure::Internal),
                    };
                }
                match crate::http::dto::VisitDto::from_record(&row, code) {
                    Ok(item) => items.push(item),
                    Err(_) => return report_error(request_id, ReportFailure::Internal),
                }
            }
            json_response(
                &crate::http::dto::CollectionDto::new(items, next_cursor),
                request_id,
            )
        }
        Route::Stats(_) => {
            let query = match VisitStatsQuery::parse(query.as_deref()) {
                Ok(query) => query,
                Err(failure) => return rejection::query_error(request_id, failure),
            };
            match state
                .reports
                .summary(&principal, code, query.interval())
                .await
            {
                Ok(record) => match crate::http::dto::VisitStatsDto::new(
                    record.lifetime_visits,
                    record.matching_visits,
                ) {
                    Ok(dto) => json_response(&dto, request_id),
                    Err(_) => report_error(request_id, ReportFailure::Internal),
                },
                Err(failure) => report_error(request_id, failure),
            }
        }
        Route::Breakdown(_) => {
            let query = match BreakdownQuery::parse(query.as_deref()) {
                Ok(query) => query,
                Err(failure) => return rejection::query_error(request_id, failure),
            };
            let context = CursorContext::breakdown(cursor_actor(&principal), link_id, &query);
            let boundary = match query.page().cursor() {
                Some(cursor) => match state.cursor_codec.decode(&context, cursor) {
                    Ok(boundary) => match BreakdownBoundary::from_cursor(boundary) {
                        Ok(boundary) => Some(boundary),
                        Err(()) => return invalid_cursor(request_id),
                    },
                    Err(_) => return invalid_cursor(request_id),
                },
                None => None,
            };
            let limit = usize::from(query.page().limit());
            let mut rows = match state
                .reports
                .breakdown(
                    &principal,
                    code,
                    query.interval(),
                    query.group_by(),
                    boundary.as_ref(),
                    (limit as i64) + 1,
                )
                .await
            {
                Ok(rows) => rows,
                Err(failure) => return report_error(request_id, failure),
            };
            if rows
                .iter()
                .any(|row| !valid_breakdown_value(query.group_by(), row.value.as_deref()))
            {
                return report_error(request_id, ReportFailure::Internal);
            }
            let has_more = rows.len() > limit;
            rows.truncate(limit);
            let page_len = rows.len();
            let mut items = Vec::with_capacity(page_len);
            let mut next_cursor = None;
            for (index, row) in rows.into_iter().enumerate() {
                if has_more && index + 1 == page_len {
                    let boundary = BreakdownBoundary {
                        value: row.value.clone(),
                    };
                    next_cursor = match state.cursor_codec.encode(&context, &boundary.into_cursor())
                    {
                        Ok(cursor) => Some(cursor),
                        Err(_) => return report_error(request_id, ReportFailure::Internal),
                    };
                }
                match crate::http::dto::BreakdownItemDto::new(row.value, row.visits) {
                    Ok(item) => items.push(item),
                    Err(_) => return report_error(request_id, ReportFailure::Internal),
                }
            }
            json_response(
                &crate::http::dto::CollectionDto::new(items, next_cursor),
                request_id,
            )
        }
        Route::MethodNotAllowed => unreachable!("method errors are returned before authentication"),
    }
}

fn select_route<'a>(path: &'a str, method: &Method) -> Option<Route<'a>> {
    let tail = path.strip_prefix("/api/v1/links/")?;
    let (code, operation) = tail.split_once('/')?;
    if code.is_empty() || code.contains('/') {
        return None;
    }
    let route = match operation {
        "visits" => Route::Visits(code),
        "stats" => Route::Stats(code),
        "stats/breakdown" => Route::Breakdown(code),
        _ => return None,
    };
    Some(if *method == Method::GET {
        route
    } else {
        Route::MethodNotAllowed
    })
}

fn cursor_actor(principal: &Principal) -> CursorActor {
    match principal {
        Principal::Account(account) => CursorActor::Account(account.id()),
        Principal::ManagementToken(token) => CursorActor::AnonymousLink(token.link_id()),
    }
}

fn valid_breakdown_value(group_by: crate::http::query::GroupBy, value: Option<&str>) -> bool {
    use crate::http::query::GroupBy;
    match (group_by, value) {
        (GroupBy::Day, Some(value)) => chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d")
            .is_ok_and(|date| date.format("%Y-%m-%d").to_string() == value),
        (GroupBy::Method, Some("GET" | "HEAD")) => true,
        (GroupBy::Device, Some("desktop" | "mobile" | "tablet" | "other" | "unknown")) => true,
        (GroupBy::Referrer | GroupBy::UserAgent, None) => true,
        (GroupBy::Referrer | GroupBy::UserAgent, Some(value)) => value.len() <= 8_192,
        (GroupBy::ClientIp, Some(value)) => value
            .parse::<std::net::IpAddr>()
            .ok()
            .map(|address| match address {
                std::net::IpAddr::V6(address) => address
                    .to_ipv4_mapped()
                    .map_or(address.into(), |v4| v4.into()),
                other => other,
            })
            .is_some_and(|address| address.to_string() == value),
        (GroupBy::Browser | GroupBy::Os, Some(value)) => !value.is_empty() && value.len() <= 8_192,
        _ => false,
    }
}

fn invalid_cursor(request_id: Uuid) -> Response {
    rejection::query_error(request_id, QueryError::InvalidRequest)
}

fn json_response(value: &impl serde::Serialize, request_id: Uuid) -> Response {
    match serde_json::to_vec(value) {
        Ok(bytes) => success_json(StatusCode::OK, bytes),
        Err(_) => report_error(request_id, ReportFailure::Internal),
    }
}

fn report_error(request_id: Uuid, failure: ReportFailure) -> Response {
    if matches!(failure, ReportFailure::InvalidCredentials) {
        crate::telemetry::metrics::record_auth_failure(
            crate::telemetry::metrics::AuthFailureCategory::InvalidCredential,
        );
    }
    let (code, message, category) = match failure {
        ReportFailure::InvalidCredentials => (
            ErrorCode::InvalidCredentials,
            "The supplied credentials are invalid.",
            "invalid_credentials",
        ),
        ReportFailure::NotFound => (
            ErrorCode::NotFound,
            "The requested resource was not found.",
            "not_found",
        ),
        ReportFailure::StorageUnavailable => (
            ErrorCode::StorageUnavailable,
            "Analytics storage is temporarily unavailable.",
            "report_storage_unavailable",
        ),
        ReportFailure::Internal => (
            ErrorCode::InternalError,
            "An internal error occurred.",
            "report_persistence_failure",
        ),
    };
    if matches!(
        failure,
        ReportFailure::StorageUnavailable | ReportFailure::Internal
    ) {
        tracing::error!(request_id = %request_id, category, "analytics report failed safely");
    }
    error::response(request_id, code, message, None)
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
    fn report_route_ownership_and_methods_are_exact() {
        for path in [
            "/api/v1/links/code/visits",
            "/api/v1/links/code/stats",
            "/api/v1/links/code/stats/breakdown",
        ] {
            assert!(handles_path(path));
            assert!(matches!(
                select_route(path, &Method::GET),
                Some(Route::Visits(_) | Route::Stats(_) | Route::Breakdown(_))
            ));
            assert!(matches!(
                select_route(path, &Method::HEAD),
                Some(Route::MethodNotAllowed)
            ));
        }
        for path in [
            "/api/v1/links/code/stats/unknown",
            "/api/v1/links/code/visits/extra",
            "/api/v1/links//stats",
            "/api/v1/links-search/code/stats",
        ] {
            assert!(!handles_path(path), "{path}");
        }
    }
}
