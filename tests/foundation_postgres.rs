#[path = "support/database.rs"]
mod database_support;
#[path = "support/http.rs"]
mod http_support;

use std::{collections::BTreeMap, time::Duration};

use axum::http::{Method, StatusCode};
use ferl_server::{app::AppState, config::Config, http::router};
use serde_json::Value;

use database_support::{IsolatedTestDatabase, command_output, wait_until_postgres_is_ready};
use http_support::{call, response_body};

#[tokio::test]
async fn readiness_tracks_schema_and_recovers_after_postgres_restart() {
    let database = IsolatedTestDatabase::connect_from_harness().await;
    let config = Config::from_map(BTreeMap::from([
        (
            "FERL_DATABASE_URL".to_owned(),
            std::env::var("FERL_TEST_DATABASE_URL").expect("database helper validated URL"),
        ),
        (
            "FERL_PUBLIC_BASE_URL".to_owned(),
            "https://links.example.test".to_owned(),
        ),
        (
            "FERL_CURSOR_KEY".to_owned(),
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".to_owned(),
        ),
    ]))
    .expect("production serving configuration is valid");
    let state = AppState::from_config(&config).expect("production pool configuration is valid");
    let serving_pool = state.pool.clone();
    let app = router(state.clone());
    let incompatible = call(app.clone(), Method::GET, "/api/health/ready").await;
    assert_eq!(incompatible.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        response_body(incompatible).await,
        br#"{"status":"unavailable"}"#
    );
    state.readiness.mark_schema_compatible();
    assert_eq!(
        health_status(app.clone(), Method::GET).await,
        StatusCode::OK
    );
    state.readiness.mark_schema_incompatible();
    assert_eq!(
        health_status(app.clone(), Method::GET).await,
        StatusCode::SERVICE_UNAVAILABLE
    );
    state.readiness.mark_schema_compatible();
    assert_eq!(
        health_status(app.clone(), Method::GET).await,
        StatusCode::OK
    );

    let stopped =
        command_output("podman", &["stop", "--time", "10", "ferl-stage01-postgres"]).await;
    assert!(
        stopped.status.success(),
        "test-owned PostgreSQL container stops cleanly"
    );
    let liveness = call(app.clone(), Method::GET, "/api/health/live").await;
    assert_eq!(liveness.status(), StatusCode::OK);
    assert_eq!(response_body(liveness).await, br#"{"status":"ok"}"#);

    let during_outage = call(app.clone(), Method::GET, "/api/health/ready").await;
    assert_eq!(during_outage.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        response_body(during_outage).await,
        br#"{"status":"unavailable"}"#
    );

    let started = command_output("podman", &["start", "ferl-stage01-postgres"]).await;
    assert!(
        started.status.success(),
        "test-owned PostgreSQL container restarts"
    );
    wait_until_postgres_is_ready().await;
    tokio::time::timeout(
        Duration::from_secs(3),
        sqlx::query_scalar::<_, i32>("SELECT 1").fetch_one(&serving_pool),
    )
    .await
    .expect("host test connection probe completes before its deadline")
    .expect("host Rust test process reconnects to PostgreSQL after restart");
    wait_until_http_ready(app.clone()).await;

    assert_eq!(
        health_status(app.clone(), Method::GET).await,
        StatusCode::OK
    );
    state.readiness.begin_draining();
    assert_eq!(
        health_status(app.clone(), Method::GET).await,
        StatusCode::SERVICE_UNAVAILABLE
    );
    let head = call(app.clone(), Method::HEAD, "/api/health/ready").await;
    assert_eq!(head.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(head.headers()["allow"], "GET");
    assert!(response_body(head).await.is_empty());

    drop(app);
    drop(state);
    tokio::time::timeout(Duration::from_secs(5), serving_pool.close())
        .await
        .expect("production serving pool closes before its deadline");
    database.close().await;
}

async fn health_status(app: axum::Router, method: Method) -> StatusCode {
    let response = call(app, method, "/api/health/ready").await;
    let status = response.status();
    let expected = if status == StatusCode::OK {
        "ok"
    } else {
        "unavailable"
    };
    let value: Value =
        serde_json::from_slice(&response_body(response).await).expect("health body is JSON");
    assert_eq!(value["status"], expected);
    status
}

async fn wait_until_http_ready(app: axum::Router) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        if health_status(app.clone(), Method::GET).await == StatusCode::OK {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "readiness did not recover before the bounded deadline"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
