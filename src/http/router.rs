use axum::{
    Router,
    extract::{DefaultBodyLimit, Request, State},
    middleware::{self, Next},
    routing::any,
};
use http::Method;
use uuid::Uuid;

use crate::{
    app::{AppState, HttpAdmission},
    db::{self, ProbeResult},
    error::{self, ErrorCode},
};

const MAX_API_REQUEST_BODY_BYTES: usize = 64 * 1024;

use super::{request_id, response};

pub fn router(state: AppState) -> Router {
    let admission = state.http_admission.clone();
    Router::new()
        .route("/api/health/live", any(liveness))
        .route("/api/health/ready", any(readiness))
        .fallback(not_found)
        .with_state(state)
        .layer(DefaultBodyLimit::max(MAX_API_REQUEST_BODY_BYTES))
        .layer(middleware::from_fn_with_state(admission, admit_requests))
        .layer(middleware::from_fn(request_id::envelope))
}

async fn admit_requests(
    State(admission): State<HttpAdmission>,
    request: Request,
    next: Next,
) -> axum::response::Response {
    let request_id = request
        .extensions()
        .get::<request_id::RequestId>()
        .map_or_else(Uuid::new_v4, |request_id| request_id.0);
    let Some(_admitted) = admission.try_admit() else {
        return error::response(
            request_id,
            ErrorCode::ServiceUnavailable,
            "The server is temporarily unable to accept additional work.",
            None,
        );
    };
    let _in_flight = admission.acquire_in_flight().await;
    next.run(request).await
}

async fn liveness(request: Request) -> axum::response::Response {
    let request_id = request
        .extensions()
        .get::<request_id::RequestId>()
        .map_or_else(Uuid::new_v4, |request_id| request_id.0);
    if request.method() != Method::GET {
        return method_not_allowed(request_id, "GET");
    }
    if request.uri().query().is_some() {
        return invalid_health_query(request_id);
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
    if request.uri().query().is_some() {
        return invalid_health_query(request_id);
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

fn invalid_health_query(request_id: Uuid) -> axum::response::Response {
    error::response(
        request_id,
        ErrorCode::InvalidRequest,
        "Health endpoints do not accept query parameters.",
        None,
    )
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use axum::{
        body::Body,
        extract::State,
        http::{Method, Request, StatusCode},
        middleware,
        routing::get,
    };
    use http_body_util::BodyExt;
    use tokio::{
        sync::{Semaphore, mpsc},
        time::timeout,
    };
    use tower::ServiceExt;

    use crate::{app::HttpAdmission, http::request_id};

    use super::admit_requests;

    struct HeldHandler {
        entered: mpsc::UnboundedSender<()>,
        release: Arc<Semaphore>,
    }

    async fn held_handler(State(state): State<Arc<HeldHandler>>) -> &'static str {
        state.entered.send(()).expect("test receiver remains open");
        state
            .release
            .acquire()
            .await
            .expect("test gate remains open")
            .forget();
        "finished"
    }

    fn held_request() -> Request<Body> {
        Request::builder()
            .method(Method::GET)
            .uri("/held")
            .header("x-request-id", "client-value")
            .body(Body::empty())
            .expect("test request is valid")
    }

    async fn wait_until_waiting_slot_is_taken(admission: &HttpAdmission) {
        timeout(Duration::from_secs(2), async {
            while admission.available_admissions() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("second request is admitted to the bounded waiting capacity");
    }

    #[tokio::test]
    async fn request_admission_bounds_active_and_waiting_work_and_sheds_excess() {
        let admission = HttpAdmission::new(1, 1);
        let (entered_tx, mut entered_rx) = mpsc::unbounded_channel();
        let release = Arc::new(Semaphore::new(0));
        let handler_state = Arc::new(HeldHandler {
            entered: entered_tx,
            release: Arc::clone(&release),
        });
        let app = axum::Router::new()
            .route("/held", get(held_handler))
            .with_state(handler_state)
            .layer(middleware::from_fn_with_state(
                admission.clone(),
                admit_requests,
            ))
            .layer(middleware::from_fn(request_id::envelope));

        let first = tokio::spawn(app.clone().oneshot(held_request()));
        timeout(Duration::from_secs(2), entered_rx.recv())
            .await
            .expect("first handler starts before the deadline")
            .expect("first handler reports its start");

        let second = tokio::spawn(app.clone().oneshot(held_request()));
        wait_until_waiting_slot_is_taken(&admission).await;

        let rejected = timeout(Duration::from_secs(2), app.oneshot(held_request()))
            .await
            .expect("excess request is rejected promptly")
            .expect("router service is infallible");
        assert_eq!(rejected.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(rejected.headers()["cache-control"], "no-store");
        let request_id = rejected.headers()["x-request-id"]
            .to_str()
            .expect("request ID is text")
            .to_owned();
        assert_ne!(request_id, "client-value");
        let body = timeout(Duration::from_secs(2), rejected.into_body().collect())
            .await
            .expect("overload response body completes")
            .expect("overload response body is readable")
            .to_bytes();
        let body: serde_json::Value = serde_json::from_slice(&body).expect("JSON error response");
        assert_eq!(body["error"]["code"], "service_unavailable");
        assert_eq!(body["request_id"], request_id);

        release.add_permits(2);
        for task in [first, second] {
            let response = timeout(Duration::from_secs(2), task)
                .await
                .expect("admitted request finishes before the deadline")
                .expect("request task does not panic")
                .expect("router service is infallible");
            assert_eq!(response.status(), StatusCode::OK);
        }
        let second_started = timeout(Duration::from_secs(2), entered_rx.recv())
            .await
            .expect("queued handler starts before the deadline");
        assert!(second_started.is_some());
    }
}
