#[path = "support/accounts.rs"]
#[allow(dead_code)]
mod account_support;
#[path = "support/database.rs"]
mod database_support;
#[path = "support/serving_process.rs"]
#[allow(dead_code)]
mod serving_process;

use std::time::Duration;

use axum::http::{Method, StatusCode};
use ferl_server::db::coordination::{
    ACCOUNT_AUTHORITY_LOCK_KEY, acquire_account_authority_exclusive, begin_read_committed,
};
use sqlx::PgPool;
use tokio::time::{Instant, sleep, timeout};

use account_support::{
    call, json, migrated_database, production_router_with, request, seed_account,
};
use serving_process::{ServingProcess, send};

const CURSOR_KEY: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
const DEADLINE: Duration = Duration::from_secs(5);
const HELD_BODY: &str = r#"{"username":"held_user"}"#;

async fn wait_for_authority_waiter(pool: &PgPool) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let (_, waiting): (i64, i64) = sqlx::query_as(
            "SELECT count(*) FILTER (WHERE granted), count(*) FILTER (WHERE NOT granted) \
             FROM pg_catalog.pg_locks WHERE locktype = 'advisory' \
               AND database = (SELECT oid FROM pg_database WHERE datname = current_database()) \
               AND classid::bigint = $1 AND objid::bigint = $2 AND objsubid = 2",
        )
        .bind(i64::from(ACCOUNT_AUTHORITY_LOCK_KEY.0))
        .bind(i64::from(ACCOUNT_AUTHORITY_LOCK_KEY.1))
        .fetch_one(pool)
        .await
        .expect("test observes the account-authority lock state");
        if waiting == 1 {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "held business request did not reach the database lock; observed {waiting} waiter(s)"
        );
        sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn in_process_liveness_stays_prompt_while_database_held_business_work_saturates_admission() {
    let database = migrated_database().await;
    let app = production_router_with(
        &database,
        &[
            ("FERL_HTTP_MAX_IN_FLIGHT", "1"),
            ("FERL_HTTP_MAX_WAITING", "0"),
        ],
    )
    .await;
    let (_, admin_key) = seed_account(database.pool(), "saturation_admin", "admin", true).await;

    let mut authority = begin_read_committed(database.pool())
        .await
        .expect("authority transaction begins");
    acquire_account_authority_exclusive(&mut authority)
        .await
        .expect("test holds the account-authority gate");
    let held_app = app.clone();
    let held_key = admin_key.clone();
    let held = tokio::spawn(async move {
        call(
            held_app,
            request(
                Method::POST,
                "/api/v1/admin/accounts",
                Some(&held_key),
                HELD_BODY,
            ),
        )
        .await
    });
    wait_for_authority_waiter(database.pool()).await;

    let live = call(
        app.clone(),
        request(Method::GET, "/api/health/live", None, ""),
    )
    .await;
    assert_eq!(live.status(), StatusCode::OK);
    assert_eq!(json(live).await, serde_json::json!({"status": "ok"}));

    let excess = call(
        app.clone(),
        request(Method::GET, "/api/v1/me", Some(&admin_key), ""),
    )
    .await;
    assert_eq!(excess.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(json(excess).await["error"]["code"], "service_unavailable");

    let ready = call(
        app.clone(),
        request(Method::GET, "/api/health/ready", None, ""),
    )
    .await;
    assert_eq!(ready.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(json(ready).await["error"]["code"], "service_unavailable");

    authority
        .commit()
        .await
        .expect("test releases the account-authority gate");
    let held = timeout(DEADLINE, held)
        .await
        .expect("held business request completes after release")
        .expect("held request task joins");
    assert_eq!(held.status(), StatusCode::CREATED);

    let recovered = call(
        app.clone(),
        request(Method::GET, "/api/v1/me", Some(&admin_key), ""),
    )
    .await;
    assert_eq!(recovered.status(), StatusCode::OK);
    let live = call(app, request(Method::GET, "/api/health/live", None, "")).await;
    assert_eq!(live.status(), StatusCode::OK);
    database.close().await;
}

#[tokio::test]
async fn tcp_process_keeps_liveness_separate_from_held_database_work_and_exits_within_budget() {
    let database = migrated_database().await;
    let database_url = database.schema_database_url().to_owned();
    let (_, admin_key) =
        seed_account(database.pool(), "process_saturation_admin", "admin", true).await;

    let mut authority = begin_read_committed(database.pool())
        .await
        .expect("authority transaction begins");
    acquire_account_authority_exclusive(&mut authority)
        .await
        .expect("test holds the account-authority gate");
    let mut process = ServingProcess::start_with(
        &database_url,
        CURSOR_KEY,
        &[
            ("FERL_HTTP_MAX_IN_FLIGHT", "1"),
            ("FERL_HTTP_MAX_WAITING", "0"),
            ("FERL_READINESS_WITHDRAWAL_SECS", "3"),
            ("FERL_HTTP_DRAIN_TIMEOUT_SECS", "5"),
            ("FERL_SHUTDOWN_TIMEOUT_SECS", "10"),
        ],
    )
    .await;
    let address = process.address;
    let held_key = admin_key.clone();
    let held = tokio::spawn(async move {
        send(
            address,
            "POST",
            "/api/v1/admin/accounts",
            Some(&held_key),
            HELD_BODY,
        )
        .await
    });
    wait_for_authority_waiter(database.pool()).await;

    let live = send(address, "GET", "/api/health/live", None, "")
        .await
        .expect("liveness connects on its own TCP connection");
    assert_eq!(live.status, 200);
    assert_eq!(live.body, br#"{"status":"ok"}"#);

    let excess = send(address, "GET", "/api/v1/me", Some(&admin_key), "")
        .await
        .expect("excess business request connects");
    assert_eq!(excess.status, 503);
    assert_eq!(
        serving_process::json(&excess)["error"]["code"],
        "service_unavailable"
    );

    process.signal_terminate().await;
    let withdrawing_live = send(address, "GET", "/api/health/live", None, "")
        .await
        .expect("liveness remains reachable during readiness withdrawal");
    assert_eq!(withdrawing_live.status, 200);
    let withdrawing_ready = send(address, "GET", "/api/health/ready", None, "")
        .await
        .expect("readiness connects during withdrawal");
    assert_eq!(withdrawing_ready.status, 503);
    assert_eq!(
        serving_process::json(&withdrawing_ready)["error"]["code"],
        "service_unavailable"
    );

    authority
        .commit()
        .await
        .expect("test releases the account-authority gate");
    let held = timeout(DEADLINE, held)
        .await
        .expect("held TCP request completes after release")
        .expect("held request task joins")
        .expect("held request receives a response");
    assert_eq!(held.status, 201);

    let output = process.stop_and_capture().await;
    assert!(output.exit_success);
    assert!(output.shutdown_elapsed < Duration::from_secs(10));
    database.close().await;
}
