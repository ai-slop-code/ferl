use axum::{
    Router,
    extract::{DefaultBodyLimit, Request, State},
    middleware::{self, Next},
    routing::{MethodFilter, any, on},
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
    let business = Router::new()
        .route("/api/health/ready", any(readiness))
        .route("/api", any(account_api_dispatch))
        .route("/api/{*path}", any(account_api_dispatch))
        .route(
            "/{short_code}",
            on(MethodFilter::GET, public_follow)
                .on(MethodFilter::HEAD, public_follow)
                .fallback(public_follow),
        )
        .fallback(path_dispatch)
        .with_state(state)
        .layer(DefaultBodyLimit::max(MAX_API_REQUEST_BODY_BYTES))
        .layer(middleware::from_fn_with_state(admission, admit_requests));
    // Liveness is merged outside business admission so it never acquires or waits
    // for in-flight/waiting permits. The shared envelope still wraps every route.
    Router::new()
        .route("/api/health/live", any(liveness))
        .merge(business)
        .layer(middleware::from_fn(request_id::envelope))
}

async fn account_api_dispatch(
    State(state): State<AppState>,
    request: Request,
) -> axum::response::Response {
    crate::accounts::handlers::dispatch(state, request).await
}

async fn path_dispatch(
    State(state): State<AppState>,
    request: Request,
) -> axum::response::Response {
    let path = request.uri().path();
    if path == "/api" || path.starts_with("/api/") {
        crate::accounts::handlers::dispatch(state, request).await
    } else {
        crate::redirects::dispatch(state, request).await
    }
}

async fn public_follow(
    State(state): State<AppState>,
    request: Request,
) -> axum::response::Response {
    crate::redirects::dispatch(state, request).await
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
    let cancellation = admission.force_cancel_token();
    let _in_flight = tokio::select! {
        biased;
        _ = cancellation.cancelled() => {
            return error::response(
                request_id,
                ErrorCode::ServiceUnavailable,
                "The server is temporarily unable to accept additional work.",
                None,
            );
        }
        permit = admission.acquire_in_flight() => permit,
    };
    tokio::select! {
        biased;
        _ = cancellation.cancelled() => error::response(
            request_id,
            ErrorCode::ServiceUnavailable,
            "The server is temporarily unable to accept additional work.",
            None,
        ),
        response = next.run(request) => response,
    }
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
    use std::{collections::BTreeMap, sync::Arc, time::Duration};

    use axum::{
        body::Body,
        extract::State,
        http::{Method, Request, StatusCode},
        middleware,
        response::Response,
        routing::get,
    };
    use http_body_util::BodyExt;
    use tokio::{
        sync::{Semaphore, mpsc},
        time::timeout,
    };
    use tower::ServiceExt;

    use crate::{
        app::{AppState, HttpAdmission},
        config::Config,
        http::request_id,
    };

    use super::{admit_requests, router};

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

    fn saturated_app_state(max_waiting: &str) -> AppState {
        let config = Config::from_map(BTreeMap::from([
            (
                "FERL_DATABASE_URL".to_owned(),
                "postgres://ferl_test@127.0.0.1:1/ferl_test".to_owned(),
            ),
            (
                "FERL_PUBLIC_BASE_URL".to_owned(),
                "https://short.example.test".to_owned(),
            ),
            (
                "FERL_CURSOR_KEY".to_owned(),
                "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".to_owned(),
            ),
            ("FERL_DB_POOL_MAX".to_owned(), "1".to_owned()),
            ("FERL_HTTP_MAX_IN_FLIGHT".to_owned(), "1".to_owned()),
            ("FERL_HTTP_MAX_WAITING".to_owned(), max_waiting.to_owned()),
        ]))
        .expect("test configuration is valid");
        AppState::from_config(&config).expect("lazy test pool is configured")
    }

    fn probe(method: Method, uri: &str) -> Request<Body> {
        Request::builder()
            .method(method)
            .uri(uri)
            .header("x-request-id", "client-value")
            .header("authorization", "Bearer deliberately-invalid")
            .body(Body::empty())
            .expect("test request is valid")
    }

    async fn send(app: &axum::Router, request: Request<Body>) -> Response {
        timeout(Duration::from_secs(2), app.clone().oneshot(request))
            .await
            .expect("response arrives before the deadline")
            .expect("router service is infallible")
    }

    async fn body_bytes(response: Response) -> Vec<u8> {
        timeout(Duration::from_secs(2), response.into_body().collect())
            .await
            .expect("response body completes before the deadline")
            .expect("response body is readable")
            .to_bytes()
            .to_vec()
    }

    async fn assert_admission_overload(response: Response) {
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(response.headers()["cache-control"], "no-store");
        let request_id = response.headers()["x-request-id"]
            .to_str()
            .expect("request ID is text")
            .to_owned();
        assert_ne!(request_id, "client-value");
        let body: serde_json::Value = serde_json::from_slice(&body_bytes(response).await)
            .expect("admission overload body is JSON");
        assert_eq!(body["error"]["code"], "service_unavailable");
        assert_eq!(body["request_id"], request_id);
    }

    #[tokio::test]
    async fn liveness_stays_prompt_while_saturated_business_capacity_sheds_work() {
        let state = saturated_app_state("0");
        let admission = state.http_admission.clone();
        let app = router(state);
        let admitted = admission
            .try_admit()
            .expect("holder occupies the only admitted slot");
        let in_flight = admission.acquire_in_flight().await;
        assert_eq!(admission.available_admissions(), 0);

        let live = send(&app, probe(Method::GET, "/api/health/live")).await;
        assert_eq!(live.status(), StatusCode::OK);
        assert_eq!(live.headers()["cache-control"], "no-store");
        assert_ne!(live.headers()["x-request-id"], "client-value");
        assert_eq!(body_bytes(live).await, br#"{"status":"ok"}"#);

        for uri in ["/api/v1/unknown", "/api/health/ready"] {
            assert_admission_overload(send(&app, probe(Method::GET, uri)).await).await;
        }
        assert_eq!(admission.available_admissions(), 0);

        drop(in_flight);
        drop(admitted);
        assert_eq!(admission.available_admissions(), 1);
        let recovered = send(&app, probe(Method::GET, "/api/v1/unknown")).await;
        assert_eq!(recovered.status(), StatusCode::NOT_FOUND);
        assert_eq!(admission.available_admissions(), 1);
    }

    #[tokio::test]
    async fn liveness_does_not_join_admitted_business_waiters() {
        let state = saturated_app_state("1");
        let admission = state.http_admission.clone();
        let app = router(state);
        let admitted = admission
            .try_admit()
            .expect("holder occupies the admitted slot");
        let in_flight = admission.acquire_in_flight().await;
        let waiter = tokio::spawn(app.clone().oneshot(probe(Method::GET, "/api/v1/unknown")));
        wait_until_waiting_slot_is_taken(&admission).await;

        let live = send(&app, probe(Method::GET, "/api/health/live")).await;
        assert_eq!(live.status(), StatusCode::OK);
        assert_eq!(body_bytes(live).await, br#"{"status":"ok"}"#);
        assert_eq!(admission.available_admissions(), 0);
        assert_admission_overload(send(&app, probe(Method::GET, "/api/v1/unknown")).await).await;

        drop(in_flight);
        let waited = timeout(Duration::from_secs(2), waiter)
            .await
            .expect("admitted waiter completes after its holder releases")
            .expect("waiter task does not panic")
            .expect("router service is infallible");
        assert_eq!(waited.status(), StatusCode::NOT_FOUND);
        drop(admitted);
        assert_eq!(admission.available_admissions(), 2);
        let recovered = send(&app, probe(Method::GET, "/api/v1/unknown")).await;
        assert_eq!(recovered.status(), StatusCode::NOT_FOUND);
        assert_eq!(admission.available_admissions(), 2);
    }

    #[tokio::test]
    async fn exact_liveness_rules_hold_and_aliases_stay_behind_business_admission() {
        const ALIASES: [&str; 5] = [
            "/api/health/live/",
            "/api/health/live/extra",
            "/api/health%2Flive",
            "/api/%68ealth/live",
            "/api/health/live%2F",
        ];
        let state = saturated_app_state("0");
        let admission = state.http_admission.clone();
        let app = router(state);
        let admitted = admission
            .try_admit()
            .expect("holder occupies the only admitted slot");
        let in_flight = admission.acquire_in_flight().await;

        let query = send(&app, probe(Method::GET, "/api/health/live?unexpected=1")).await;
        assert_eq!(query.status(), StatusCode::BAD_REQUEST);
        let query_id = query.headers()["x-request-id"]
            .to_str()
            .expect("request ID is text")
            .to_owned();
        let body: serde_json::Value =
            serde_json::from_slice(&body_bytes(query).await).expect("query rejection body is JSON");
        assert_eq!(body["error"]["code"], "invalid_request");
        assert_eq!(body["request_id"], query_id);

        let post = send(&app, probe(Method::POST, "/api/health/live")).await;
        assert_eq!(post.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(post.headers()["allow"], "GET");
        let head = send(&app, probe(Method::HEAD, "/api/health/live")).await;
        assert_eq!(head.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(head.headers()["allow"], "GET");
        assert!(body_bytes(head).await.is_empty());

        for uri in ALIASES {
            assert_admission_overload(send(&app, probe(Method::GET, uri)).await).await;
        }

        drop(in_flight);
        drop(admitted);
        for uri in ALIASES {
            let response = send(&app, probe(Method::GET, uri)).await;
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "alias {uri}");
            assert_eq!(response.headers()["cache-control"], "no-store");
        }
    }
}
