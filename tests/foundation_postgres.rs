#[path = "support/database.rs"]
mod database_support;
#[path = "support/http.rs"]
mod http_support;

use std::{collections::BTreeMap, time::Duration};

use axum::http::{Method, StatusCode};
use ferl_server::{
    app::AppState,
    config::{Config, MigrationConfig},
    db::migrate::{self, SchemaIssue, VerificationError},
    db::records::VisitRecord,
    http::router,
};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use database_support::{IsolatedTestDatabase, command_output, wait_until_postgres_is_ready};
use http_support::{call, response_body};

#[tokio::test]
async fn readiness_tracks_schema_and_recovers_after_postgres_restart() {
    let database = IsolatedTestDatabase::connect_from_harness().await;
    let database_url = database.schema_database_url().to_owned();
    let config = Config::from_map(BTreeMap::from([
        ("FERL_DATABASE_URL".to_owned(), database_url.clone()),
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
    assert_eq!(
        state.verify_schema().await,
        Err(VerificationError::Incompatible(SchemaIssue::MissingHistory))
    );

    let migration_config = MigrationConfig::from_map(BTreeMap::from([(
        "FERL_DATABASE_URL".to_owned(),
        database_url,
    )]))
    .expect("migration-specific configuration does not require serving settings");
    migrate::run_migrations(&migration_config)
        .await
        .expect("embedded migration applies to the isolated fixture schema");
    state
        .verify_schema()
        .await
        .expect("production schema verifier recognizes the migrated schema");
    seed_restart_persistence_fixture(database.pool()).await;

    let cancellation = CancellationToken::new();
    let mut schema_monitor = migrate::spawn_schema_monitor(
        serving_pool.clone(),
        state.readiness.clone(),
        cancellation.clone(),
        Duration::from_millis(100),
        Duration::from_secs(3),
    );
    assert_eq!(
        health_status(app.clone(), Method::GET).await,
        StatusCode::OK
    );

    sqlx::query("UPDATE _sqlx_migrations SET success = false WHERE version = 1")
        .execute(database.pool())
        .await
        .expect("test marks only its owned migration history dirty");
    wait_until_schema_compatible(&state, false).await;
    assert_eq!(
        health_status(app.clone(), Method::GET).await,
        StatusCode::SERVICE_UNAVAILABLE
    );
    sqlx::query("UPDATE _sqlx_migrations SET success = true WHERE version = 1")
        .execute(database.pool())
        .await
        .expect("test restores its owned migration history");
    wait_until_schema_compatible(&state, true).await;
    assert_eq!(
        health_status(app.clone(), Method::GET).await,
        StatusCode::OK
    );

    let stopped =
        command_output("podman", &["stop", "--time", "10", "ferl-stage01-postgres"]).await;
    let liveness = call(app.clone(), Method::GET, "/api/health/live").await;
    let during_outage = call(app.clone(), Method::GET, "/api/health/ready").await;
    let started = command_output("podman", &["start", "ferl-stage01-postgres"]).await;
    wait_until_postgres_is_ready().await;
    tokio::time::timeout(
        Duration::from_secs(3),
        sqlx::query_scalar::<_, i32>("SELECT 1").fetch_one(&serving_pool),
    )
    .await
    .expect("host test connection probe completes before its deadline")
    .expect("host Rust test process reconnects to PostgreSQL after restart");
    wait_until_http_ready(app.clone()).await;
    assert_restart_persistence_fixture(&serving_pool).await;

    assert!(
        stopped.status.success(),
        "test-owned PostgreSQL container stops cleanly"
    );
    assert_eq!(liveness.status(), StatusCode::OK);
    assert_eq!(response_body(liveness).await, br#"{"status":"ok"}"#);
    assert_eq!(during_outage.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        response_body(during_outage).await,
        br#"{"status":"unavailable"}"#
    );
    assert!(
        started.status.success(),
        "test-owned PostgreSQL container restarts"
    );

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
    cancellation.cancel();
    tokio::time::timeout(Duration::from_secs(2), &mut schema_monitor)
        .await
        .expect("schema monitor stops before its deadline")
        .expect("schema monitor task exits cleanly");
    drop(state);
    tokio::time::timeout(Duration::from_secs(5), serving_pool.close())
        .await
        .expect("production serving pool closes before its deadline");
    database.close().await;
}

async fn wait_until_schema_compatible(state: &AppState, compatible: bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while state.readiness.is_schema_compatible() != compatible {
        assert!(
            tokio::time::Instant::now() < deadline,
            "schema monitor did not observe the expected compatibility state"
        );
        tokio::task::yield_now().await;
    }
}

async fn seed_restart_persistence_fixture(pool: &sqlx::PgPool) {
    let account_id = uuid::Uuid::from_u128(0x0202_0000_0000_0000_0000_0000_0000_0001);
    let link_id = uuid::Uuid::from_u128(0x0202_0000_0000_0000_0000_0000_0000_0002);
    let bucket_id = uuid::Uuid::from_u128(0x0202_0000_0000_0000_0000_0000_0000_0003);

    sqlx::query("INSERT INTO accounts (id, username, role, enabled, api_key_digest) VALUES ($1, 'restart_user', 'user', true, $2)")
        .bind(account_id)
        .bind(vec![0x42_u8; 32])
        .execute(pool)
        .await
        .expect("restart fixture account with digest inserts");
    sqlx::query("INSERT INTO code_reservations (short_code) VALUES ('restart-code')")
        .execute(pool)
        .await
        .expect("restart fixture reservation inserts");
    sqlx::query(
        "INSERT INTO links \
         (id, short_code, owner_id, destination_url, destination_search, title, title_search, \
          enabled, visit_count) \
         VALUES ($1, 'restart-code', $2, 'https://persist.example/path?x=1#frag', \
                 'https://persist.example/path?x=1#frag', 'Persisted title', 'persisted title', \
                 true, 1)",
    )
    .bind(link_id)
    .bind(account_id)
    .execute(pool)
    .await
    .expect("restart fixture link inserts");
    sqlx::query("INSERT INTO link_tags (link_id, name) VALUES ($1, 'restart-tag')")
        .bind(link_id)
        .execute(pool)
        .await
        .expect("restart fixture tag association inserts");
    sqlx::query(
        "INSERT INTO visits \
         (id, link_id, redirect_url, occurred_at, method, client_ip, referrer, user_agent, device, browser, os) \
         VALUES ($1, $2, 'https://persist.example/old-snapshot?q=2#old', \
                 '2026-10-27T10:00:00Z', 'HEAD', '2001:db8::44', \
                 'https://referrer.example/path', 'Restart fixture browser', \
                 'desktop', 'FixtureBrowser', 'FixtureOS')",
    )
    .bind(uuid::Uuid::from_u128(0x0202_0000_0000_0000_0000_0000_0000_0004))
    .bind(link_id)
    .execute(pool)
    .await
    .expect("restart fixture immutable visit snapshot inserts");
    sqlx::query(
        "INSERT INTO rate_buckets (id, bucket_kind, account_id) VALUES ($1, 'account', $2)",
    )
    .bind(bucket_id)
    .bind(account_id)
    .execute(pool)
    .await
    .expect("restart fixture stable account bucket inserts");
    sqlx::query("INSERT INTO rate_charges (id, bucket_id, charged_at) VALUES ($1, $2, '2026-10-27T10:01:00Z')")
        .bind(uuid::Uuid::from_u128(0x0202_0000_0000_0000_0000_0000_0000_0005))
        .bind(bucket_id)
        .execute(pool)
        .await
        .expect("restart fixture rate charge inserts");
}

async fn assert_restart_persistence_fixture(pool: &sqlx::PgPool) {
    let account_id = uuid::Uuid::from_u128(0x0202_0000_0000_0000_0000_0000_0000_0001);
    let link_id = uuid::Uuid::from_u128(0x0202_0000_0000_0000_0000_0000_0000_0002);
    let account_digest: Vec<u8> =
        sqlx::query_scalar("SELECT api_key_digest FROM accounts WHERE id = $1")
            .bind(account_id)
            .fetch_one(pool)
            .await
            .expect("account digest survives PostgreSQL restart and reconnect");
    assert_eq!(account_digest, vec![0x42_u8; 32]);

    let (destination, title, count): (String, Option<String>, i64) =
        sqlx::query_as("SELECT destination_url, title, visit_count FROM links WHERE id = $1")
            .bind(link_id)
            .fetch_one(pool)
            .await
            .expect("link metadata and counter survive restart");
    assert_eq!(destination, "https://persist.example/path?x=1#frag");
    assert_eq!(title.as_deref(), Some("Persisted title"));
    assert_eq!(count, 1);

    let tags: Vec<String> = sqlx::query_scalar("SELECT name FROM link_tags WHERE link_id = $1")
        .bind(link_id)
        .fetch_all(pool)
        .await
        .expect("tag associations survive restart");
    assert_eq!(tags, ["restart-tag"]);
    let snapshot: String = sqlx::query_scalar("SELECT redirect_url FROM visits WHERE link_id = $1")
        .bind(link_id)
        .fetch_one(pool)
        .await
        .expect("immutable visit destination snapshot survives restart");
    assert_eq!(snapshot, "https://persist.example/old-snapshot?q=2#old");
    let visit: VisitRecord = sqlx::query_as(
        "SELECT id, link_id, redirect_url, occurred_at, method, client_ip, referrer, user_agent, \
                device, browser, os \
         FROM visits WHERE link_id = $1",
    )
    .bind(link_id)
    .fetch_one(pool)
    .await
    .expect("visit attributes survive restart");
    assert_eq!(visit.method, "HEAD");
    assert_eq!(visit.client_ip.addr().to_string(), "2001:db8::44");
    assert_eq!(visit.client_ip.prefix_len(), 128);
    assert_eq!(
        visit.referrer.as_deref(),
        Some("https://referrer.example/path")
    );
    assert_eq!(visit.user_agent.as_deref(), Some("Restart fixture browser"));

    let charge_count: i64 = sqlx::query_scalar("SELECT count(*) FROM rate_charges")
        .fetch_one(pool)
        .await
        .expect("rate charge persists after restart");
    assert_eq!(charge_count, 1);
    let reservation: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM code_reservations WHERE short_code = 'restart-code')",
    )
    .fetch_one(pool)
    .await
    .expect("permanent reservation survives restart");
    assert!(reservation);
    let bootstrap_completed: bool =
        sqlx::query_scalar("SELECT bootstrap_completed FROM system_state WHERE singleton")
            .fetch_one(pool)
            .await
            .expect("bootstrap state survives restart");
    assert!(
        !bootstrap_completed,
        "migration and serving do not bootstrap an admin"
    );
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
