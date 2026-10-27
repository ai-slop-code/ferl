use std::time::{Duration, Instant};

use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
};
use ferl_server::{app::AppState, http::router};
use http_body_util::BodyExt;
use sqlx::{PgPool, postgres::PgPoolOptions};
use tokio::{
    process::Command,
    time::{sleep, timeout},
};
use tower::ServiceExt;
use url::Url;

const CONTAINER_NAME: &str = "ferl-stage01-postgres";

#[tokio::test]
async fn readiness_tracks_schema_and_recovers_after_postgres_restart() {
    let database_url = std::env::var("FERL_TEST_DATABASE_URL").expect(
        "FERL_TEST_DATABASE_URL is required; run scripts/postgres.sh start and scripts/check.sh",
    );
    let container_name = std::env::var("FERL_TEST_POSTGRES_CONTAINER")
        .expect("FERL_TEST_POSTGRES_CONTAINER must identify the harness-owned Podman container");
    assert_eq!(
        container_name, CONTAINER_NAME,
        "only the fixed harness container may be restarted"
    );
    assert_container_is_harness_owned().await;
    assert_test_url_matches_container(&database_url);

    let pool: PgPool = PgPoolOptions::new()
        .max_connections(2)
        .acquire_timeout(Duration::from_secs(2))
        .connect(&database_url)
        .await
        .expect("host test process must establish a real PostgreSQL connection");
    sqlx::query("SELECT 1")
        .execute(&pool)
        .await
        .expect("PostgreSQL probe succeeds");

    let state = AppState::new(pool.clone(), Duration::from_millis(500), 16);
    let app = router(state.clone());
    assert_eq!(
        health_status(app.clone(), Method::GET).await,
        StatusCode::SERVICE_UNAVAILABLE
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

    let stopped = command_output("podman", &["stop", "--time", "10", CONTAINER_NAME]).await;
    assert!(
        stopped.status.success(),
        "test-owned PostgreSQL container stops cleanly"
    );
    let outage_app = app.clone();
    let during_outage = timeout(
        Duration::from_secs(3),
        tokio::spawn(async move { outage_app.oneshot(health_request(Method::GET)).await }),
    )
    .await;

    // Restart the persistent container before asserting any outage response result.
    let started = command_output("podman", &["start", CONTAINER_NAME]).await;
    assert!(
        started.status.success(),
        "test-owned PostgreSQL container restarts"
    );
    wait_until_postgres_is_ready().await;
    wait_until_http_ready(app.clone()).await;

    let during_outage = during_outage
        .expect("outage request finishes within its deadline")
        .expect("outage request task does not panic")
        .expect("router is infallible");
    assert_eq!(during_outage.status(), StatusCode::SERVICE_UNAVAILABLE);
    let outage_body = during_outage
        .into_body()
        .collect()
        .await
        .expect("outage readiness body is readable")
        .to_bytes();
    let outage_json: serde_json::Value =
        serde_json::from_slice(&outage_body).expect("outage readiness is JSON");
    assert_eq!(outage_json["status"], "unavailable");
    assert_eq!(
        health_status(app.clone(), Method::GET).await,
        StatusCode::OK
    );
    state.readiness.begin_draining();
    assert_eq!(
        health_status(app.clone(), Method::GET).await,
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(
        health_status(app, Method::HEAD).await,
        StatusCode::METHOD_NOT_ALLOWED
    );
    pool.close().await;
}

async fn health_status(app: axum::Router, method: Method) -> StatusCode {
    let response = app
        .oneshot(health_request(method))
        .await
        .expect("router responds");
    let status = response.status();
    if status != StatusCode::METHOD_NOT_ALLOWED {
        let body = response
            .into_body()
            .collect()
            .await
            .expect("health body is readable")
            .to_bytes();
        let expected = if status == StatusCode::OK {
            "ok"
        } else {
            "unavailable"
        };
        let value: serde_json::Value = serde_json::from_slice(&body).expect("health body is JSON");
        assert_eq!(value["status"], expected);
    }
    status
}

fn health_request(method: Method) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri("/api/health/ready")
        .body(Body::empty())
        .expect("test request is valid")
}

async fn assert_container_is_harness_owned() {
    let output = command_output(
        "podman",
        &[
            "inspect",
            "--format",
            "{{ index .Config.Labels \"io.ferl.harness\" }}",
            CONTAINER_NAME,
        ],
    )
    .await;
    assert!(
        output.status.success(),
        "expected the Stage 01 Podman container"
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "stage01",
        "refusing to restart a container not owned by the Stage 01 harness"
    );
}

fn assert_test_url_matches_container(database_url: &str) {
    let parsed = Url::parse(database_url).expect("FERL_TEST_DATABASE_URL is a valid URL");
    assert_eq!(parsed.scheme(), "postgres");
    assert_eq!(parsed.host_str(), Some("127.0.0.1"));
    let configured_port = std::env::var("FERL_TEST_POSTGRES_PORT")
        .expect("FERL_TEST_POSTGRES_PORT must be set by the harness")
        .parse::<u16>()
        .expect("test port is a valid port");
    assert_eq!(parsed.port(), Some(configured_port));
    assert_eq!(parsed.path(), "/ferl_test");
}

async fn wait_until_postgres_is_ready() {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let result = timeout(
            Duration::from_secs(3),
            Command::new("podman")
                .args([
                    "exec",
                    CONTAINER_NAME,
                    "pg_isready",
                    "-U",
                    "ferl",
                    "-d",
                    "ferl_test",
                ])
                .output(),
        )
        .await;
        if matches!(result, Ok(Ok(output)) if output.status.success()) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "PostgreSQL did not recover before the bounded deadline"
        );
        sleep(Duration::from_millis(200)).await;
    }
}

async fn wait_until_http_ready(app: axum::Router) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if health_status(app.clone(), Method::GET).await == StatusCode::OK {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "readiness did not recover before the bounded deadline"
        );
        sleep(Duration::from_millis(100)).await;
    }
}

async fn command_output(program: &str, arguments: &[&str]) -> std::process::Output {
    timeout(
        Duration::from_secs(20),
        Command::new(program).args(arguments).output(),
    )
    .await
    .expect("Podman command completes within its deadline")
    .expect("Podman command can be executed")
}
