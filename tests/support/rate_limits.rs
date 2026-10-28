use axum::{
    Router,
    body::Body,
    extract::{ConnectInfo, Request, State},
    middleware,
    response::Response,
    routing::any,
};
use ferl_server::{
    app::AppState,
    config::Config,
    http::{
        body, rejection,
        request_id::{self, RequestId},
    },
    rate_limit::{ChargedAttempt, CreationFailure},
};
use http::request::Parts;
use sqlx::PgPool;
use std::{collections::BTreeMap, net::SocketAddr, time::Duration};
use tokio::time::timeout;

pub fn config(url: &str, extra: &[(&str, &str)]) -> Config {
    let mut values = BTreeMap::from([
        ("FERL_DATABASE_URL".into(), url.into()),
        (
            "FERL_PUBLIC_BASE_URL".into(),
            "https://short.example.test".into(),
        ),
        (
            "FERL_CURSOR_KEY".into(),
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".into(),
        ),
        ("FERL_ANONYMOUS_CREATION".into(), "true".into()),
        ("FERL_DB_LOCK_TIMEOUT_SECS".into(), "10".into()),
        ("FERL_DB_STATEMENT_TIMEOUT_SECS".into(), "12".into()),
    ]);
    for (name, value) in extra {
        values.insert((*name).into(), (*value).into());
    }
    Config::from_map(values).expect("validated production creation configuration")
}

pub fn state(url: &str, extra: &[(&str, &str)]) -> AppState {
    AppState::from_config(&config(url, extra)).unwrap()
}

pub fn parts(ip: &str, credential: Option<&str>) -> Parts {
    request(ip, credential, Body::empty()).into_parts().0
}

pub fn request(ip: &str, credential: Option<&str>, body: Body) -> Request {
    let mut request = Request::builder()
        .method("POST")
        .uri("/attempt")
        .header("content-type", "application/json")
        .header("x-request-id", "untrusted-request-id")
        .body(body)
        .unwrap();
    let ip = ip.parse().unwrap();
    request
        .extensions_mut()
        .insert(ConnectInfo(SocketAddr::new(ip, 4567)));
    if let Some(key) = credential {
        request
            .headers_mut()
            .insert("authorization", format!("Bearer {key}").parse().unwrap());
    }
    request
}

pub async fn prepare(state: &AppState, parts: &Parts) -> Result<ChargedAttempt, CreationFailure> {
    timeout(Duration::from_secs(20), state.creation.prepare(parts))
        .await
        .expect("bounded production allowance operation")
}

pub async fn count(pool: &PgPool) -> i64 {
    timeout(
        Duration::from_secs(5),
        sqlx::query_scalar("SELECT count(*) FROM rate_charges").fetch_one(pool),
    )
    .await
    .unwrap()
    .unwrap()
}

pub async fn close(pool: &PgPool) {
    timeout(Duration::from_secs(5), pool.close())
        .await
        .expect("production pool closes")
}

/// Deliberately test-owned orchestration, never registered in the server.
/// Even HEAD traverses the service here to exercise every reusable error body.
pub fn harness(state: AppState) -> Router {
    Router::new()
        .route("/attempt", any(attempt))
        .with_state(state)
        .layer(middleware::from_fn(request_id::envelope))
}

async fn attempt(State(state): State<AppState>, request: Request) -> Response {
    let id = request.extensions().get::<RequestId>().unwrap().0;
    let (parts, body) = request.into_parts();
    let _charge = match state.creation.prepare(&parts).await {
        Ok(charge) => charge,
        Err(error) => return rejection::creation_error(id, error),
    };
    match body::read_json_object(&parts.headers, body).await {
        Ok(_) => rejection::decoded_ok(),
        Err(error) => rejection::body_error(id, error),
    }
}

pub async fn wait_for_query(pool: &PgPool, pattern: &str, expected: i64) {
    timeout(Duration::from_secs(8), async {
        loop {
            let count: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_stat_activity WHERE datname = current_database() AND pid <> pg_backend_pid() AND wait_event_type = 'Lock' AND query LIKE $1")
                .bind(pattern).fetch_one(pool).await.unwrap();
            if count >= expected { break; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.expect("controlled PostgreSQL lock wait becomes observable");
}
