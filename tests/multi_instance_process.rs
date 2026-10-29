#[path = "support/accounts.rs"]
#[allow(dead_code)]
mod account_support;
#[path = "support/database.rs"]
#[allow(dead_code)]
mod database_support;
#[path = "support/serving_process.rs"]
#[allow(dead_code)]
mod serving_process;

use std::{
    fs,
    net::TcpListener,
    panic::AssertUnwindSafe,
    path::{Path, PathBuf},
    process::Output,
    time::Duration,
};

use account_support::{migrated_database, seed_account};
use ferl_server::db::coordination::{
    ACCOUNT_AUTHORITY_LOCK_KEY, acquire_account_authority_exclusive, begin_read_committed,
};
use futures_util::FutureExt;
use serde_json::Value;
use serving_process::{ServingProcess, assert_headers, json, send, send_with_headers};
use sqlx::{PgPool, Postgres, Transaction};
use tokio::{
    process::Command,
    task::JoinSet,
    time::{sleep, timeout},
};
use uuid::Uuid;

const CURSOR_KEY: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
const PROCESS_SETTINGS: &[(&str, &str)] = &[
    ("FERL_ANONYMOUS_CREATION", "true"),
    ("FERL_ANONYMOUS_CREATE_LIMIT", "4"),
    ("FERL_ACCOUNT_CREATE_LIMIT", "4"),
    ("FERL_TRUSTED_PROXIES", ""),
    ("FERL_DB_POOL_MAX", "4"),
    ("FERL_DB_LOCK_TIMEOUT_SECS", "10"),
];
const VISIT_LOCK_KEY: i64 = 79_022_311_711;

#[tokio::test]
async fn production_http_allowances_are_one_durable_account_and_peer_budget_across_a_b_c() {
    let database = migrated_database().await;
    database
        .verify(|database| {
            Box::pin(async move {
                let (_admin_id, admin_key) =
                    seed_account(database.pool(), "fleet_allowance_admin", "admin", true).await;
                let (account_id, account_key) =
                    seed_account(database.pool(), "fleet_allowance_user", "user", true).await;
                let url = database.schema_database_url().to_owned();
                let a = ServingProcess::start_with(&url, CURSOR_KEY, PROCESS_SETTINGS).await;
                let b = ServingProcess::start_with(&url, CURSOR_KEY, PROCESS_SETTINGS).await;

                // Reserve a code through one production HTTP process using a
                // different account. The caller below pays its own HTTP charge
                // before discovering this later creation conflict.
                let occupied = send(
                    a.address,
                    "POST",
                    "/api/v1/links",
                    Some(&admin_key),
                    r#"{"destination_url":"https://fleet-conflict.example/","short_code":"fleet-conflict"}"#,
                )
                .await
                .unwrap();
                assert_headers(&occupied);
                assert_eq!(occupied.status, 201);

                let malformed = send(
                    b.address,
                    "POST",
                    "/api/v1/links",
                    Some(&account_key),
                    "{malformed",
                )
                .await
                .unwrap();
                assert_headers(&malformed);
                assert_eq!(malformed.status, 400);
                assert_eq!(json(&malformed)["error"]["code"], "invalid_request");

                let conflict = send(
                    a.address,
                    "POST",
                    "/api/v1/links",
                    Some(&account_key),
                    r#"{"destination_url":"https://later-conflict.example/","short_code":"fleet-conflict"}"#,
                )
                .await
                .unwrap();
                assert_headers(&conflict);
                assert_eq!(conflict.status, 409);
                assert_eq!(json(&conflict)["error"]["code"], "short_code_unavailable");

                let account_race = concurrent_creates(
                    [a.address, b.address],
                    Some(account_key.clone()),
                    "fleet-account-attempt",
                )
                .await;
                assert_eq!(
                    account_race.iter().filter(|response| response.status == 201).count(),
                    2,
                    "only two slots remained after the invalid and colliding HTTP attempts"
                );
                assert_eq!(
                    account_race.iter().filter(|response| response.status == 429).count(),
                    2,
                    "A and B jointly observe the same account allowance"
                );
                for response in &account_race {
                    assert_headers(response);
                    if response.status == 429 {
                        assert_eq!(json(response)["error"]["code"], "rate_limited");
                        assert!(response.headers["retry-after"].parse::<u64>().unwrap() > 0);
                    }
                }
                assert_eq!(
                    active_account_charges(database.pool(), account_id.as_uuid()).await,
                    4,
                    "malformed, conflict, and admitted HTTP requests are durable charges"
                );
                assert_eq!(
                    sqlx::query_scalar::<_, i64>(
                        "SELECT count(*) FROM links WHERE short_code LIKE 'fleet-account-attempt-%'",
                    )
                    .fetch_one(database.pool())
                    .await
                    .unwrap(),
                    2,
                    "rate-rejected HTTP requests create no link or reservation"
                );

                // Add C while A/B are live and prove it does not mint another
                // allowance. Rotate the key, then prove the UUID-backed bucket
                // survives credential replacement and an A process restart.
                let c = ServingProcess::start_with(&url, CURSOR_KEY, PROCESS_SETTINGS).await;
                assert_rate_rejected(
                    c.address,
                    Some(&account_key),
                    "fleet-account-added-process",
                )
                .await;
                let rotation = send(
                    a.address,
                    "POST",
                    "/api/v1/me/api-key",
                    Some(&account_key),
                    "",
                )
                .await
                .unwrap();
                assert_headers(&rotation);
                assert_eq!(rotation.status, 200);
                let replacement_key = json(&rotation)["api_key"].as_str().unwrap().to_owned();
                let old_key = send(b.address, "GET", "/api/v1/me", Some(&account_key), "")
                    .await
                    .unwrap();
                assert_eq!(old_key.status, 401);
                let new_key = send(b.address, "GET", "/api/v1/me", Some(&replacement_key), "")
                    .await
                    .unwrap();
                assert_eq!(new_key.status, 200);
                assert_eq!(json(&new_key)["id"], account_id.as_uuid().to_string());
                assert_rate_rejected(
                    b.address,
                    Some(&replacement_key),
                    "fleet-account-after-rotation",
                )
                .await;
                let a_output = a.stop_and_capture().await;
                assert!(a_output.exit_success, "A exits naturally after SIGTERM");
                let replacement =
                    ServingProcess::start_with(&url, CURSOR_KEY, PROCESS_SETTINGS).await;
                assert_rate_rejected(
                    replacement.address,
                    Some(&replacement_key),
                    "fleet-account-after-restart",
                )
                .await;
                assert_eq!(
                    active_account_charges(database.pool(), account_id.as_uuid()).await,
                    4,
                    "adding/restarting an executable and rotating its API key never resets charges"
                );

                // With no trusted proxy configured, every host-to-server TCP
                // request has the observed loopback peer identity. A forged
                // X-Forwarded-For value cannot move it to another bucket.
                let forged = send_with_headers(
                    b.address,
                    "POST",
                    "/api/v1/links",
                    None,
                    "{malformed",
                    &[("X-Forwarded-For", "198.51.100.220")],
                )
                .await
                .unwrap();
                assert_eq!(forged.status, 400);
                let peer =
                    sqlx::query_scalar::<_, String>("SELECT host(client_ip) FROM rate_buckets WHERE bucket_kind = 'anonymous_ip'")
                        .fetch_one(database.pool())
                        .await
                        .unwrap();
                assert_eq!(peer, "127.0.0.1", "effective IP is the accepted TCP peer");
                let ip_race = concurrent_creates(
                    [b.address, replacement.address],
                    None,
                    "fleet-ip-attempt",
                )
                .await;
                assert_eq!(ip_race.iter().filter(|response| response.status == 201).count(), 3);
                assert_eq!(ip_race.iter().filter(|response| response.status == 429).count(), 1);
                for response in &ip_race {
                    assert_headers(response);
                    if response.status == 429 {
                        assert!(response.headers["retry-after"].parse::<u64>().unwrap() > 0);
                    }
                }
                assert_eq!(active_ip_charges(database.pool(), &peer).await, 4);
                assert_rate_rejected(
                    c.address,
                    None,
                    "fleet-ip-added-process",
                )
                .await;
                assert_eq!(active_ip_charges(database.pool(), &peer).await, 4);

                let b_output = b.stop_and_capture().await;
                let c_output = c.stop_and_capture().await;
                let replacement_output = replacement.stop_and_capture().await;
                assert!(b_output.exit_success && c_output.exit_success && replacement_output.exit_success);
                for output in [&a_output, &b_output, &c_output, &replacement_output] {
                    assert!(output.shutdown_elapsed < Duration::from_secs(25));
                    for captured in [&output.stdout, &output.stderr] {
                        assert!(!captured.contains(&account_key));
                        assert!(!captured.contains(&replacement_key));
                    }
                }
            })
        })
        .await;
}

#[tokio::test]
async fn independent_a_b_mutation_waiting_on_authority_rejects_a_key_rotated_first() {
    let database = migrated_database().await;
    database
        .verify(|database| {
            Box::pin(async move {
                let (_id, old_key) =
                    seed_account(database.pool(), "fleet_guard_user", "user", true).await;
                let url = database.schema_database_url().to_owned();
                let a = ServingProcess::start_with(&url, CURSOR_KEY, PROCESS_SETTINGS).await;
                let b = ServingProcess::start_with(&url, CURSOR_KEY, PROCESS_SETTINGS).await;
                let created = send(
                    a.address,
                    "POST",
                    "/api/v1/links",
                    Some(&old_key),
                    r#"{"destination_url":"https://guard-race.example/","short_code":"guard-race","title":"before"}"#,
                )
                .await
                .unwrap();
                assert_headers(&created);
                assert_eq!(created.status, 201);

                // Hold the deployment-wide authority gate. Queue the key
                // rotation first, then A's already-authenticated management
                // request. PostgreSQL lock wait observation establishes the
                // ordering; the released writer invalidates the key before the
                // waiting mutation's fresh guarded revalidation.
                let mut holder = begin_read_committed(database.pool()).await.unwrap();
                acquire_account_authority_exclusive(&mut holder)
                    .await
                    .unwrap();
                let rotation_address = b.address;
                let rotation_key = old_key.clone();
                let rotation = tokio::spawn(async move {
                    send(
                        rotation_address,
                        "POST",
                        "/api/v1/me/api-key",
                        Some(&rotation_key),
                        "",
                    )
                    .await
                    .expect("B rotation request reaches the production process")
                });
                wait_for_authority_waiters(database.pool(), 1).await;

                let mutation_address = a.address;
                let mutation_key = old_key.clone();
                let mutation = tokio::spawn(async move {
                    send(
                        mutation_address,
                        "PATCH",
                        "/api/v1/links/guard-race",
                        Some(&mutation_key),
                        r#"{"title":"must-not-commit"}"#,
                    )
                    .await
                    .expect("A mutation request reaches the production process")
                });
                wait_for_authority_waiters(database.pool(), 2).await;
                holder.commit().await.unwrap();

                let rotated = timeout(Duration::from_secs(8), rotation)
                    .await
                    .expect("B's queued key rotation completes")
                    .unwrap();
                assert_headers(&rotated);
                assert_eq!(rotated.status, 200);
                let new_key = json(&rotated)["api_key"].as_str().unwrap().to_owned();
                let stale = timeout(Duration::from_secs(8), mutation)
                    .await
                    .expect("A's waiting protected mutation completes")
                    .unwrap();
                assert_headers(&stale);
                assert_eq!(stale.status, 401);
                assert_eq!(json(&stale)["error"]["code"], "invalid_credentials");
                let current = send(
                    b.address,
                    "GET",
                    "/api/v1/links/guard-race",
                    Some(&new_key),
                    "",
                )
                .await
                .unwrap();
                assert_eq!(current.status, 200);
                assert_eq!(json(&current)["title"], "before");
                let revoked = send(a.address, "GET", "/api/v1/me", Some(&old_key), "")
                    .await
                    .unwrap();
                assert_eq!(revoked.status, 401);

                let a_output = a.stop_and_capture().await;
                let b_output = b.stop_and_capture().await;
                assert!(a_output.exit_success && b_output.exit_success);
                assert!(a_output.shutdown_elapsed < Duration::from_secs(25));
                assert!(b_output.shutdown_elapsed < Duration::from_secs(25));
                for output in [&a_output, &b_output] {
                    assert!(!output.stdout.contains(&old_key));
                    assert!(!output.stdout.contains(&new_key));
                }
            })
        })
        .await;
}

#[tokio::test]
async fn independent_a_b_workers_keep_counters_atomic_and_delete_b_queued_events() {
    let database = migrated_database().await;
    let run_id = format!("part11-{}", Uuid::new_v4().simple());
    let capture_dir = std::env::current_dir()
        .unwrap()
        .join("target/collector-captures")
        .join(&run_id);
    let capture_for_scenario = capture_dir.clone();
    let port = reserve_port().await;
    let result = AssertUnwindSafe(async {
        let endpoint = start_collector(&run_id, port, &capture_dir).await;
        database
            .verify(|database| {
                Box::pin(async move {
                    let url = database.schema_database_url().to_owned();
                    let b_settings = [
                        ("FERL_ANALYTICS_WORKERS", "1"),
                        ("FERL_ANALYTICS_QUEUE_CAPACITY", "8"),
                        ("FERL_OTEL_ENDPOINT", endpoint.as_str()),
                        ("FERL_OTEL_TIMEOUT_SECS", "1"),
                        ("FERL_OTEL_QUEUE_CAPACITY", "64"),
                        ("FERL_OTEL_BATCH_SIZE", "16"),
                        ("FERL_OTEL_TRACE_SAMPLE_RATIO", "0.0"),
                        ("FERL_HTTP_DRAIN_TIMEOUT_SECS", "5"),
                        ("FERL_SHUTDOWN_TIMEOUT_SECS", "20"),
                        ("FERL_READINESS_WITHDRAWAL_SECS", "1"),
                    ];
                    let mut a = ServingProcess::start_with(&url, CURSOR_KEY, PROCESS_SETTINGS).await;
                    let b = ServingProcess::start_with(&url, CURSOR_KEY, &b_settings).await;
                    let created = send(
                        a.address,
                        "POST",
                        "/api/v1/links",
                        None,
                        r#"{"destination_url":"https://fleet-analytics.example/"}"#,
                    )
                    .await
                    .unwrap();
                    assert_headers(&created);
                    assert_eq!(created.status, 201);
                    let created_body = json(&created);
                    let code = created_body["link"]["short_code"]
                        .as_str()
                        .unwrap()
                        .to_owned();
                    let token = created_body["management_token"].as_str().unwrap().to_owned();
                    let link_id = Uuid::parse_str(created_body["link"]["id"].as_str().unwrap()).unwrap();

                    let mut redirects = JoinSet::new();
                    for index in 0..16_u8 {
                        let address = if index % 2 == 0 { a.address } else { b.address };
                        let code = code.clone();
                        redirects.spawn(async move {
                            send(address, "GET", &format!("/{code}"), None, "")
                                .await
                                .expect("A/B production redirect completes")
                        });
                    }
                    for _ in 0..16 {
                        let response = timeout(Duration::from_secs(8), redirects.join_next())
                            .await
                            .expect("A/B redirect worker request completes")
                            .unwrap()
                            .unwrap();
                        assert_headers(&response);
                        assert_eq!(response.status, 302);
                    }
                    wait_for_visits(database.pool(), &code, 16).await;
                    let (rows, unique, counter): (i64, i64, i64) = sqlx::query_as(
                        "SELECT count(*), count(DISTINCT v.id), max(l.visit_count) FROM visits v \
                         JOIN links l ON l.id = v.link_id WHERE l.short_code = $1",
                    )
                    .bind(&code)
                    .fetch_one(database.pool())
                    .await
                    .unwrap();
                    assert_eq!((rows, unique, counter), (16, 16, 16));

                    // Hold a real PostgreSQL advisory lock used by a trigger
                    // after the B worker has locked the live link row. One more
                    // B redirect must then remain in its private queue. We
                    // observe both the database wait and B's exported queue-depth
                    // gauge before issuing A's DELETE.
                    sqlx::query(
                        "CREATE FUNCTION part11_hold_visit() RETURNS trigger LANGUAGE plpgsql AS $$ \
                         BEGIN PERFORM pg_advisory_xact_lock(79022311711); RETURN NEW; END; $$",
                    )
                    .execute(database.pool())
                    .await
                    .unwrap();
                    sqlx::query(
                        "CREATE TRIGGER part11_hold_visit BEFORE INSERT ON visits \
                         FOR EACH ROW EXECUTE FUNCTION part11_hold_visit()",
                    )
                    .execute(database.pool())
                    .await
                    .unwrap();
                    let held = advisory_transaction(database.pool()).await;
                    let blocked_redirect = send(b.address, "GET", &format!("/{code}"), None, "")
                        .await
                        .unwrap();
                    assert_eq!(blocked_redirect.status, 302);
                    wait_for_waiting_query(database.pool(), "INSERT INTO visits", 1).await;
                    let queued_redirect = send(b.address, "GET", &format!("/{code}"), None, "")
                        .await
                        .unwrap();
                    assert_eq!(queued_redirect.status, 302);
                    let b_instance = instance_id(&b.stdout_snapshot());
                    wait_for_queue_depth(&capture_for_scenario, &b_instance, true).await;

                    let delete_address = a.address;
                    let delete_token = token.clone();
                    let delete_code = code.clone();
                    let delete = tokio::spawn(async move {
                        send(
                            delete_address,
                            "DELETE",
                            &format!("/api/v1/links/{delete_code}"),
                            Some(&delete_token),
                            "",
                        )
                        .await
                        .expect("A deletion request returns after the observed lock release")
                    });
                    wait_for_waiting_query(database.pool(), "FOR UPDATE", 1).await;
                    held.commit().await.expect("test releases B's blocked worker");
                    let deleted = timeout(Duration::from_secs(8), delete)
                        .await
                        .expect("A deletion completes before its deadline")
                        .unwrap();
                    assert_headers(&deleted);
                    assert_eq!(deleted.status, 204);
                    wait_for_queue_depth(&capture_for_scenario, &b_instance, false).await;
                    assert_eq!(
                        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM links WHERE id=$1")
                            .bind(link_id)
                            .fetch_one(database.pool())
                            .await
                            .unwrap(),
                        0,
                        "B's in-flight and queued work cannot recreate A's deleted link"
                    );
                    assert_eq!(
                        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM visits WHERE link_id=$1")
                            .bind(link_id)
                            .fetch_one(database.pool())
                            .await
                            .unwrap(),
                        0,
                        "deletion cascades both committed and late visit rows"
                    );
                    assert_eq!(
                        sqlx::query_scalar::<_, i64>(
                            "SELECT count(*) FROM code_reservations WHERE short_code=$1",
                        )
                        .bind(&code)
                        .fetch_one(database.pool())
                        .await
                        .unwrap(),
                        1,
                        "the permanent reservation survives cross-process deletion"
                    );
                    let deleted_redirect = send(b.address, "GET", &format!("/{code}"), None, "")
                        .await
                        .unwrap();
                    assert_eq!(deleted_redirect.status, 404);

                    // A withdraws readiness while B is still ready. A fresh C
                    // attaches to the same primary database and sees no resurrected
                    // link or local-session dependency.
                    a.signal_terminate().await;
                    timeout(Duration::from_secs(5), async {
                        loop {
                            if let Ok(response) =
                                send(a.address, "GET", "/api/health/ready", None, "").await
                                && response.status == 503
                            {
                                assert_eq!(response.body, br#"{"status":"unavailable"}"#);
                                break;
                            }
                            sleep(Duration::from_millis(25)).await;
                        }
                    })
                    .await
                    .expect("A readiness withdrawal is observed over TCP");
                    assert_eq!(
                        send(b.address, "GET", "/api/health/ready", None, "")
                            .await
                            .unwrap()
                            .status,
                        200,
                        "B remains usable while A drains"
                    );
                    let a_output = a.stop_and_capture().await;
                    assert!(a_output.exit_success, "A completes a natural bounded shutdown");
                    let c = ServingProcess::start_with(&url, CURSOR_KEY, &b_settings).await;
                    assert_eq!(
                        send(c.address, "GET", &format!("/{code}"), None, "")
                            .await
                            .unwrap()
                            .status,
                        404,
                        "replacement C observes authoritative deletion without fixture reseeding"
                    );
                    let b_output = b.stop_and_capture().await;
                    let c_output = c.stop_and_capture().await;
                    assert!(b_output.exit_success && c_output.exit_success);
                    for output in [&a_output, &b_output, &c_output] {
                        assert!(output.shutdown_elapsed < Duration::from_secs(20));
                    }
                })
            })
            .await;
    })
    .catch_unwind()
    .await;
    let cleanup = collector_action(&run_id, port, &capture_dir, "cleanup").await;
    assert!(
        cleanup.status.success(),
        "only the test-owned Collector is cleaned up"
    );
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

async fn concurrent_creates(
    addresses: [std::net::SocketAddr; 2],
    credential: Option<String>,
    code_prefix: &str,
) -> Vec<serving_process::HttpResult> {
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(5));
    let mut tasks = JoinSet::new();
    for index in 0..4 {
        let address = addresses[index % 2];
        let credential = credential.clone();
        let barrier = barrier.clone();
        let body = if credential.is_some() {
            format!(
                r#"{{"destination_url":"https://{code_prefix}-{index}.example/","short_code":"{code_prefix}-{index}"}}"#
            )
        } else {
            format!(r#"{{"destination_url":"https://{code_prefix}-{index}.example/"}}"#)
        };
        tasks.spawn(async move {
            timeout(Duration::from_secs(5), barrier.wait())
                .await
                .expect("all HTTP creators reach the launch barrier");
            send(
                address,
                "POST",
                "/api/v1/links",
                credential.as_deref(),
                &body,
            )
            .await
            .expect("production HTTP creation request finishes")
        });
    }
    timeout(Duration::from_secs(5), barrier.wait())
        .await
        .expect("release both-process HTTP allowance requests together");
    let mut responses = Vec::new();
    while let Some(task) = timeout(Duration::from_secs(8), tasks.join_next())
        .await
        .expect("all A/B create requests finish before deadline")
    {
        responses.push(task.unwrap());
    }
    responses
}

async fn assert_rate_rejected(address: std::net::SocketAddr, credential: Option<&str>, code: &str) {
    let response = send(
        address,
        "POST",
        "/api/v1/links",
        credential,
        &format!(r#"{{"destination_url":"https://{code}.example/","short_code":"{code}"}}"#),
    )
    .await
    .unwrap();
    assert_headers(&response);
    assert_eq!(response.status, 429);
    assert_eq!(json(&response)["error"]["code"], "rate_limited");
    assert!(response.headers["retry-after"].parse::<u64>().unwrap() > 0);
}

async fn active_account_charges(pool: &PgPool, id: Uuid) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM rate_charges c JOIN rate_buckets b ON b.id=c.bucket_id \
         WHERE b.account_id=$1 AND c.charged_at > clock_timestamp()-interval '60 seconds'",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn active_ip_charges(pool: &PgPool, ip: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM rate_charges c JOIN rate_buckets b ON b.id=c.bucket_id \
         WHERE b.bucket_kind='anonymous_ip' AND host(b.client_ip)=$1 \
           AND c.charged_at > clock_timestamp()-interval '60 seconds'",
    )
    .bind(ip)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn wait_for_visits(pool: &PgPool, code: &str, expected: i64) {
    timeout(Duration::from_secs(12), async {
        loop {
            let actual: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM visits v JOIN links l ON l.id=v.link_id WHERE l.short_code=$1",
            )
            .bind(code)
            .fetch_one(pool)
            .await
            .unwrap();
            if actual == expected {
                break;
            }
            assert!(actual < expected, "worker event UUIDs are unique and not duplicated");
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("A/B analytics workers persist before the deadline");
}

async fn advisory_transaction(pool: &PgPool) -> Transaction<'static, Postgres> {
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(VISIT_LOCK_KEY)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx
}

async fn wait_for_waiting_query(pool: &PgPool, needle: &str, minimum: i64) {
    timeout(Duration::from_secs(8), async {
        loop {
            let waiting: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM pg_stat_activity WHERE datname=current_database() \
                 AND pid<>pg_backend_pid() AND wait_event_type='Lock' AND query LIKE $1",
            )
            .bind(format!("%{needle}%"))
            .fetch_one(pool)
            .await
            .unwrap();
            if waiting >= minimum {
                break;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("PostgreSQL did not observe {minimum} lock waiter(s) for {needle}"));
}

async fn wait_for_authority_waiters(pool: &PgPool, expected: i64) {
    timeout(Duration::from_secs(8), async {
        loop {
            let waiting: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM pg_catalog.pg_locks WHERE locktype='advisory' \
                 AND database=(SELECT oid FROM pg_database WHERE datname=current_database()) \
                 AND classid::bigint=$1 AND objid::bigint=$2 AND objsubid=2 AND NOT granted",
            )
            .bind(i64::from(ACCOUNT_AUTHORITY_LOCK_KEY.0))
            .bind(i64::from(ACCOUNT_AUTHORITY_LOCK_KEY.1))
            .fetch_one(pool)
            .await
            .unwrap();
            if waiting >= expected {
                break;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("PostgreSQL did not observe {expected} authority-gate waiter(s)"));
}

async fn reserve_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}

async fn start_collector(run_id: &str, port: u16, capture: &Path) -> String {
    fs::create_dir_all(capture).unwrap();
    let port_text = port.to_string();
    for action in ["start", "ready"] {
        let output = collector_action(run_id, port, capture, action).await;
        assert!(
            output.status.success(),
            "run-isolated pinned Collector {action} succeeds: {}",
            safe_command_output(&output)
        );
    }
    format!("http://127.0.0.1:{port_text}")
}

async fn collector_action(run_id: &str, port: u16, capture: &Path, action: &str) -> Output {
    let capture = capture.to_string_lossy().into_owned();
    let mut command = Command::new("sh");
    command
        .args(["scripts/collector.sh", action])
        .env("FERL_COLLECTOR_RUN_ID", run_id)
        .env("FERL_COLLECTOR_PORT", port.to_string())
        .env("FERL_COLLECTOR_CAPTURE_DIR", capture)
        .kill_on_drop(true);
    timeout(
        Duration::from_secs(if action == "ready" { 40 } else { 20 }),
        command.output(),
    )
    .await
    .unwrap_or_else(|_| panic!("Collector {action} command exceeded its bound"))
    .expect("Collector harness command runs")
}

fn safe_command_output(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn instance_id(stdout: &str) -> String {
    stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find_map(|value| {
            (value["fields"]["category"] == "server_started")
                .then(|| value["fields"]["instance_id"].as_str().map(str::to_owned))
                .flatten()
        })
        .expect("production startup logs its generated instance UUID")
}

async fn wait_for_queue_depth(capture: &Path, instance: &str, nonzero: bool) {
    timeout(Duration::from_secs(20), async {
        loop {
            if latest_queue_depth(capture, instance)
                .is_some_and(|depth| if nonzero { depth > 0 } else { depth == 0 })
            {
                break;
            }
            sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "Collector did not capture B's analytics queue depth nonzero={nonzero} before deadline"
        )
    });
}

fn latest_queue_depth(capture: &Path, instance: &str) -> Option<i64> {
    let mut files = fs::read_dir(capture)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .collect::<Vec<PathBuf>>();
    files.sort();
    let mut latest = None;
    for file in files {
        let contents = fs::read(file).ok()?;
        for line in contents.split(|byte| *byte == b'\n') {
            let Ok(document) = serde_json::from_slice::<Value>(line) else {
                continue;
            };
            for resource_metrics in document
                .get("resourceMetrics")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let service_instance = resource_metrics
                    .get("resource")
                    .and_then(|resource| resource.get("attributes"))
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .find(|attribute| attribute["key"] == "service.instance.id")
                    .and_then(|attribute| attribute["value"]["stringValue"].as_str());
                if service_instance != Some(instance) {
                    continue;
                }
                for metric in resource_metrics
                    .get("scopeMetrics")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .flat_map(|scope| scope["metrics"].as_array().into_iter().flatten())
                    .filter(|metric| metric["name"] == "ferl.analytics.queue.depth")
                {
                    for point in metric["gauge"]["dataPoints"]
                        .as_array()
                        .into_iter()
                        .flatten()
                    {
                        if let Some(depth) = point["asInt"]
                            .as_str()
                            .and_then(|depth| depth.parse().ok())
                            .or_else(|| point["asInt"].as_i64())
                        {
                            latest = Some(depth);
                        }
                    }
                }
            }
        }
    }
    latest
}
