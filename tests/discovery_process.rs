#[path = "support/accounts.rs"]
#[allow(dead_code)]
mod account_support;
#[path = "support/database.rs"]
mod database_support;
#[path = "support/serving_process.rs"]
mod serving_process;

use account_support::{migrated_database, seed_account};
use serving_process::{ServingProcess, assert_headers, json, send};
use std::{process::Output, time::Duration};
use tokio::{process::Command, time::timeout};

const CURSOR_KEY: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
const PROCESS_SETTINGS: &[(&str, &str)] = &[
    ("FERL_DB_POOL_MAX", "4"),
    ("FERL_DB_ACQUIRE_TIMEOUT_SECS", "2"),
    ("FERL_DB_LOCK_TIMEOUT_SECS", "5"),
];

#[tokio::test]
async fn discovery_cursors_and_fresh_state_cross_independent_servers_and_volume_restart() {
    let database = migrated_database().await;
    database
        .verify(|database| {
            Box::pin(async move {
                let (owner_id, initial_key) =
                    seed_account(database.pool(), "discovery_process_owner", "user", true).await;
                let database_url = database.schema_database_url().to_owned();
                let a = ServingProcess::start_with(&database_url, CURSOR_KEY, PROCESS_SETTINGS).await;
                let b = ServingProcess::start_with(&database_url, CURSOR_KEY, PROCESS_SETTINGS).await;
                for path in [
                    "/api/v1/links?q=FILTER_PRIVATE_CANARY",
                    "/api/v1/tags?q=FILTER_PRIVATE_CANARY",
                ] {
                    let no_match = send(a.address, "GET", path, Some(&initial_key), "")
                        .await
                        .unwrap();
                    assert_headers(&no_match);
                    assert_eq!(json(&no_match)["items"], serde_json::json!([]));
                }

                for (code, title, tag) in [
                    ("disc-alpha", "Alpha", "SharedTag"),
                    ("disc-beta", "Beta", "sharedtag"),
                    ("disc-gamma", "Gamma", "GammaTag"),
                ] {
                    let created = send(
                        a.address,
                        "POST",
                        "/api/v1/links",
                        Some(&initial_key),
                        &format!(
                            r#"{{"destination_url":"https://{code}.process.example/path","short_code":"{code}","title":"{title}","tags":["{tag}"]}}"#
                        ),
                    )
                    .await
                    .unwrap();
                    assert_headers(&created);
                    assert_eq!(created.status, 201);
                }

                let first = send(
                    a.address,
                    "GET",
                    "/api/v1/links?sort=title&order=asc&limit=1",
                    Some(&initial_key),
                    "",
                )
                .await
                .unwrap();
                assert_headers(&first);
                assert_eq!(first.status, 200);
                assert_eq!(json(&first)["items"][0]["short_code"], "disc-alpha");
                let cursor_one = json(&first)["next_cursor"].as_str().unwrap().to_owned();
                let second = send(
                    b.address,
                    "GET",
                    &format!("/api/v1/links?sort=title&order=asc&limit=1&cursor={cursor_one}"),
                    Some(&initial_key),
                    "",
                )
                .await
                .unwrap();
                assert_headers(&second);
                assert_eq!(second.status, 200);
                assert_eq!(json(&second)["items"][0]["short_code"], "disc-beta");
                let cursor_two = json(&second)["next_cursor"].as_str().unwrap().to_owned();

                let replacement = send(
                    b.address,
                    "POST",
                    "/api/v1/me/api-key",
                    Some(&initial_key),
                    "",
                )
                .await
                .unwrap();
                assert_headers(&replacement);
                assert_eq!(replacement.status, 200);
                let new_key = json(&replacement)["api_key"].as_str().unwrap().to_owned();
                let revoked = send(
                    a.address,
                    "GET",
                    "/api/v1/links",
                    Some(&initial_key),
                    "",
                )
                .await
                .unwrap();
                assert_headers(&revoked);
                assert_eq!(revoked.status, 401);
                let continued = send(
                    a.address,
                    "GET",
                    &format!("/api/v1/links?sort=title&order=asc&limit=1&cursor={cursor_two}"),
                    Some(&new_key),
                    "",
                )
                .await
                .unwrap();
                assert_headers(&continued);
                assert_eq!(continued.status, 200);
                assert_eq!(json(&continued)["items"][0]["short_code"], "disc-gamma");

                let tags = send(
                    b.address,
                    "GET",
                    "/api/v1/tags?q=SHAREDTAG",
                    Some(&new_key),
                    "",
                )
                .await
                .unwrap();
                assert_headers(&tags);
                assert_eq!(tags.status, 200);
                assert_eq!(json(&tags)["items"], serde_json::json!([{"name":"sharedtag", "link_count":2}]));

                let patched = send(
                    a.address,
                    "PATCH",
                    "/api/v1/links/disc-beta",
                    Some(&new_key),
                    r#"{"title":"Beta updated","tags":["replacement-tag"]}"#,
                )
                .await
                .unwrap();
                assert_headers(&patched);
                assert_eq!(patched.status, 200);
                assert_eq!(json(&patched)["destination_url"], "https://disc-beta.process.example/path");
                let listed_on_b = send(
                    b.address,
                    "GET",
                    "/api/v1/links?q=beta%20updated",
                    Some(&new_key),
                    "",
                )
                .await
                .unwrap();
                assert_headers(&listed_on_b);
                assert_eq!(json(&listed_on_b)["items"][0]["short_code"], "disc-beta");
                assert_eq!(json(&listed_on_b)["items"][0]["tags"], serde_json::json!(["replacement-tag"]));

                let deleted = send(
                    a.address,
                    "DELETE",
                    "/api/v1/links/disc-gamma",
                    Some(&new_key),
                    "",
                )
                .await
                .unwrap();
                assert_headers(&deleted);
                assert_eq!(deleted.status, 204);
                let after_delete = send(
                    b.address,
                    "GET",
                    "/api/v1/links?q=disc-gamma",
                    Some(&new_key),
                    "",
                )
                .await
                .unwrap();
                assert_headers(&after_delete);
                assert_eq!(json(&after_delete)["items"], serde_json::json!([]));

                let worker_link = send(
                    a.address,
                    "POST",
                    "/api/v1/links",
                    Some(&new_key),
                    r#"{"destination_url":"https://worker.process.example/path","short_code":"disc-worker","title":"Worker counter"}"#,
                )
                .await
                .unwrap();
                assert_eq!(worker_link.status, 201);
                let _ = json(&worker_link);
                let before_visits: i64 = sqlx::query_scalar(
                    "SELECT visit_count FROM links WHERE short_code = 'disc-worker'",
                )
                .fetch_one(database.pool())
                .await
                .unwrap();
                let redirect = send(b.address, "GET", "/disc-worker", None, "")
                    .await
                    .unwrap();
                assert_headers(&redirect);
                assert_eq!(redirect.status, 302);
                assert!(redirect.body.is_empty());
                timeout(Duration::from_secs(8), async {
                    loop {
                        let count: i64 = sqlx::query_scalar(
                            "SELECT visit_count FROM links WHERE short_code = 'disc-worker'",
                        )
                        .fetch_one(database.pool())
                        .await
                        .unwrap();
                        if count > before_visits {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                })
                .await
                .expect("Part 07 visit worker updates its persisted counter before deadline");
                let worker_list = send(
                    a.address,
                    "GET",
                    "/api/v1/links?q=Worker%20counter",
                    Some(&new_key),
                    "",
                )
                .await
                .unwrap();
                assert_headers(&worker_list);
                assert_eq!(json(&worker_list)["items"][0]["visit_count"], before_visits + 1);

                let restart_page = send(
                    a.address,
                    "GET",
                    "/api/v1/links?sort=created_at&order=desc&limit=1",
                    Some(&new_key),
                    "",
                )
                .await
                .unwrap();
                assert_eq!(restart_page.status, 200);
                let restart_cursor = json(&restart_page)["next_cursor"]
                    .as_str()
                    .unwrap()
                    .to_owned();
                let capture_a = a.stop_and_capture().await;
                let capture_b = b.stop_and_capture().await;
                assert!(capture_a.stdout.contains("request completed"));
                assert!(capture_b.stdout.contains("request completed"));
                let owner_id_canary = owner_id.as_uuid().to_string();
                for canary in [
                    database_url.as_str(),
                    initial_key.as_str(),
                    new_key.as_str(),
                    "FILTER_PRIVATE_CANARY",
                    "disc-alpha",
                    "disc-beta",
                    "disc-gamma",
                    "Worker counter",
                    "https://worker.process.example/path",
                    owner_id_canary.as_str(),
                ] {
                    assert!(!capture_a.stdout.contains(canary));
                    assert!(!capture_a.stderr.contains(canary));
                    assert!(!capture_b.stdout.contains(canary));
                    assert!(!capture_b.stderr.contains(canary));
                }

                // The shared fixture lock is already owned by IsolatedTestDatabase;
                // invoke Podman directly here, never a recursively locking script.
                let stopped = podman(&["stop", "--time", "10", database_support::selected_container_name()]).await;
                let started = podman(&["start", database_support::selected_container_name()]).await;
                let ready = wait_postgres_ready().await;
                if !ready {
                    let recovered = podman(&["start", database_support::selected_container_name()]).await;
                    assert!(recovered.status.success(), "a second owned-harness start recovers PostgreSQL");
                    assert!(wait_postgres_ready().await, "owned PostgreSQL becomes ready before cleanup deadline");
                }
                assert!(stopped.status.success(), "the owned database container stops");
                assert!(started.status.success(), "the named-volume container restarts");
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM links WHERE short_code LIKE 'disc-%'")
                        .fetch_one(database.pool())
                        .await
                        .unwrap(),
                    3
                );
                assert_eq!(
                    sqlx::query_scalar::<_, i64>("SELECT visit_count FROM links WHERE short_code = 'disc-worker'")
                        .fetch_one(database.pool())
                        .await
                        .unwrap(),
                    before_visits + 1
                );

                let c = ServingProcess::start_with(&database_url, CURSOR_KEY, PROCESS_SETTINGS).await;
                let after_restart = send(
                    c.address,
                    "GET",
                    &format!("/api/v1/links?sort=created_at&order=desc&limit=1&cursor={restart_cursor}"),
                    Some(&new_key),
                    "",
                )
                .await
                .unwrap();
                assert_headers(&after_restart);
                assert_eq!(after_restart.status, 200);
                assert_eq!(json(&after_restart)["items"].as_array().unwrap().len(), 1);
                let tag_after_restart = send(
                    c.address,
                    "GET",
                    "/api/v1/tags?q=replacement-tag",
                    Some(&new_key),
                    "",
                )
                .await
                .unwrap();
                assert_headers(&tag_after_restart);
                assert_eq!(json(&tag_after_restart)["items"][0]["link_count"], 1);
                c.stop().await;
            })
        })
        .await;
}

async fn podman(arguments: &[&str]) -> Output {
    let mut command = Command::new("podman");
    command.kill_on_drop(true).args(arguments);
    timeout(Duration::from_secs(20), command.output())
        .await
        .expect("owned-harness Podman operation completes before deadline")
        .expect("Podman can be executed")
}

async fn wait_postgres_ready() -> bool {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let output = podman(&[
            "exec",
            database_support::selected_container_name(),
            "pg_isready",
            "-q",
            "-t",
            "1",
            "-U",
            "ferl",
            "-d",
            "ferl_test",
        ])
        .await;
        if output.status.success() {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}
