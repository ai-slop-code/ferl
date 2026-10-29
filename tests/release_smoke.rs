#[path = "support/accounts.rs"]
#[allow(dead_code)]
mod account_support;
#[path = "support/database.rs"]
#[allow(dead_code)]
mod database_support;
#[path = "support/podman_server.rs"]
#[allow(dead_code)]
mod podman_server;
#[path = "support/serving_process.rs"]
#[allow(dead_code)]
mod serving_process;

use std::{panic::AssertUnwindSafe, path::PathBuf, time::Duration};

use futures_util::FutureExt;
use serde_json::Value;
use serving_process::{assert_headers, json, send};
use tokio::time::{sleep, timeout};
use uuid::Uuid;

const USERNAME: &str = "release_admin";

#[tokio::test]
async fn packaged_binary_migrates_bootstraps_serves_rotates_deletes_and_restarts_without_reseeding()
{
    let binary = PathBuf::from(
        std::env::var_os("FERL_RELEASE_BINARY")
            .expect("run scripts/check-release.sh; the packaged release binary is required"),
    )
    .canonicalize()
    .expect("the extracted release binary exists");
    let run_id = Uuid::new_v4().simple().to_string();
    let network = podman_server::create_network(&run_id).await;
    let network_for_test = network.clone();
    let first_name = format!("ferl-part11-smoke-a-{run_id}");
    let second_name = format!("ferl-part11-smoke-b-{run_id}");
    let first_for_cleanup = first_name.clone();
    let second_for_cleanup = second_name.clone();
    let result = AssertUnwindSafe(async {
        let database = database_support::IsolatedTestDatabase::connect_from_harness().await;
        database
            .verify(|database| {
                Box::pin(async move {
                    let database_url =
                        podman_server::container_database_url(database.schema_database_url()).await;
                    let key = fresh_cursor_key();

                    let migration = podman_server::migrate(
                        &binary,
                        &network_for_test,
                        &database_url,
                    )
                    .await;
                    assert!(
                        migration.status.success(),
                        "packaged ferl-server migrate succeeds: {}",
                        String::from_utf8_lossy(&migration.stderr)
                    );
                    let applied: Vec<i64> = sqlx::query_scalar(
                        "SELECT version FROM _sqlx_migrations WHERE success ORDER BY version",
                    )
                    .fetch_all(database.pool())
                    .await
                    .unwrap();
                    assert_eq!(applied, [1, 2], "packaged binary applies the published migrations");

                    let bootstrap = podman_server::bootstrap(
                        &binary,
                        &network_for_test,
                        &database_url,
                        USERNAME,
                    )
                    .await;
                    assert!(
                        bootstrap.status.success(),
                        "packaged bootstrap-admin succeeds: {}",
                        String::from_utf8_lossy(&bootstrap.stderr)
                    );
                    assert_eq!(bootstrap.stdout.iter().filter(|byte| **byte == b'\n').count(), 1);
                    let bootstrap_dto: Value = serde_json::from_slice(&bootstrap.stdout)
                        .expect("bootstrap result is one private JSON document");
                    assert_eq!(bootstrap_dto["account"]["username"], USERNAME);
                    let initial_key = bootstrap_dto["api_key"]
                        .as_str()
                        .expect("one-time admin key is retained in memory")
                        .to_owned();
                    assert!(initial_key.starts_with("ferl_ak_v1_"));
                    assert!(!String::from_utf8_lossy(&bootstrap.stderr).contains(&initial_key));
                    assert!(
                        sqlx::query_scalar::<_, bool>(
                            "SELECT bootstrap_completed FROM system_state WHERE singleton",
                        )
                        .fetch_one(database.pool())
                        .await
                        .unwrap()
                    );

                    let cursor_key = key.clone();
                    let first = podman_server::ContainerServer::start(
                        &binary,
                        &first_name,
                        &network_for_test,
                        &database_url,
                        &cursor_key,
                        "",
                        podman_server::host_port(),
                        &[],
                    )
                    .await;
                    let uid = podman_server::command_output(
                        "podman",
                        &["inspect", "--format", "{{.Config.User}}", &first_name],
                        Duration::from_secs(10),
                    )
                    .await;
                    assert!(uid.status.success());
                    assert_eq!(String::from_utf8_lossy(&uid.stdout).trim(), "65532:65532");

                    let created = send(
                        first.address,
                        "POST",
                        "/api/v1/links",
                        Some(&initial_key),
                        r#"{"destination_url":"https://release-smoke.example/path?q=1#frag","short_code":"release-smoke","title":"Packaged title","tags":["Release","smoke"]}"#,
                    )
                    .await
                    .unwrap();
                    assert_headers(&created);
                    assert_eq!(created.status, 201);
                    assert_eq!(json(&created)["link"]["title"], "Packaged title");
                    assert_eq!(json(&created)["link"]["tags"], serde_json::json!(["release", "smoke"]));

                    let listed = send(
                        first.address,
                        "GET",
                        "/api/v1/links?q=Packaged",
                        Some(&initial_key),
                        "",
                    )
                    .await
                    .unwrap();
                    assert_headers(&listed);
                    assert_eq!(listed.status, 200);
                    assert_eq!(json(&listed)["items"][0]["short_code"], "release-smoke");

                    for method in ["GET", "HEAD"] {
                        let redirect = send(first.address, method, "/release-smoke?ignored=1", None, "")
                            .await
                            .unwrap();
                        assert_headers(&redirect);
                        assert_eq!(redirect.status, 302);
                        assert_eq!(
                            redirect.headers["location"],
                            "https://release-smoke.example/path?q=1#frag"
                        );
                        assert!(redirect.body.is_empty());
                    }
                    timeout(Duration::from_secs(10), async {
                        loop {
                            let response = send(
                                first.address,
                                "GET",
                                "/api/v1/links/release-smoke/stats",
                                Some(&initial_key),
                                "",
                            )
                            .await
                            .unwrap();
                            if response.status == 200
                                && json(&response)["lifetime_visits"].as_i64() == Some(2)
                            {
                                break;
                            }
                            sleep(Duration::from_millis(25)).await;
                        }
                    })
                    .await
                    .expect("GET/HEAD visit reports become visible before deadline");

                    let rotation = send(
                        first.address,
                        "POST",
                        "/api/v1/me/api-key",
                        Some(&initial_key),
                        "",
                    )
                    .await
                    .unwrap();
                    assert_headers(&rotation);
                    assert_eq!(rotation.status, 200);
                    let current_key = json(&rotation)["api_key"].as_str().unwrap().to_owned();
                    let revoked = send(first.address, "GET", "/api/v1/me", Some(&initial_key), "")
                        .await
                        .unwrap();
                    assert_eq!(revoked.status, 401);
                    let current = send(first.address, "GET", "/api/v1/me", Some(&current_key), "")
                        .await
                        .unwrap();
                    assert_eq!(current.status, 200);

                    let deleted = send(
                        first.address,
                        "DELETE",
                        "/api/v1/links/release-smoke",
                        Some(&current_key),
                        "",
                    )
                    .await
                    .unwrap();
                    assert_headers(&deleted);
                    assert_eq!(deleted.status, 204);
                    assert!(deleted.body.is_empty());
                    assert_eq!(
                        sqlx::query_scalar::<_, i64>(
                            "SELECT count(*) FROM code_reservations WHERE short_code='release-smoke'",
                        )
                        .fetch_one(database.pool())
                        .await
                        .unwrap(),
                        1
                    );

                    let first_exit = first.stop_naturally().await;
                    assert_eq!(first_exit.exit_code, 0);
                    assert!(!first_exit.oom_killed);
                    assert!(first_exit.shutdown_elapsed < Duration::from_secs(20));

                    // This is a fresh server container over the same schema and
                    // cursor key. No migration/bootstrap/reseed runs on restart.
                    let second = podman_server::ContainerServer::start(
                        &binary,
                        &second_name,
                        &network_for_test,
                        &database_url,
                        &key,
                        "",
                        podman_server::host_port(),
                        &[],
                    )
                    .await;
                    let after_restart = send(
                        second.address,
                        "GET",
                        "/api/v1/me",
                        Some(&current_key),
                        "",
                    )
                    .await
                    .unwrap();
                    assert_headers(&after_restart);
                    assert_eq!(after_restart.status, 200);
                    let old_code = send(second.address, "GET", "/release-smoke", None, "")
                        .await
                        .unwrap();
                    assert_eq!(old_code.status, 404);
                    let reuse = send(
                        second.address,
                        "POST",
                        "/api/v1/links",
                        Some(&current_key),
                        r#"{"destination_url":"https://never-reuse.example/","short_code":"release-smoke"}"#,
                    )
                    .await
                    .unwrap();
                    assert_headers(&reuse);
                    assert_eq!(reuse.status, 409);
                    assert_eq!(json(&reuse)["error"]["code"], "short_code_unavailable");
                    assert_eq!(
                        sqlx::query_scalar::<_, i64>(
                            "SELECT count(*) FROM links WHERE short_code='release-smoke'",
                        )
                        .fetch_one(database.pool())
                        .await
                        .unwrap(),
                        0,
                        "restart does not reseed a deleted link"
                    );
                    let exit = second.stop_naturally().await;
                    assert_eq!(exit.exit_code, 0);
                    assert!(!exit.oom_killed);
                    assert!(exit.shutdown_elapsed < Duration::from_secs(20));
                    for output in [first_exit.logs, exit.logs] {
                        assert!(!output.contains(&initial_key));
                        assert!(!output.contains(&current_key));
                        assert!(!output.contains(&key));
                    }
                })
            })
            .await;
    })
    .catch_unwind()
    .await;

    for name in [&first_for_cleanup, &second_for_cleanup] {
        let cleanup = podman_server::cleanup_server(name).await;
        assert!(
            cleanup.status.success() || !container_exists(name).await,
            "test-owned server container cleanup is bounded"
        );
    }
    let network_cleanup = podman_server::remove_network(&network).await;
    assert!(
        network_cleanup.status.success(),
        "test-owned network is removed"
    );
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

fn fresh_cursor_key() -> String {
    let mut random = [0_u8; 32];
    getrandom::fill(&mut random).expect("OS cryptographic randomness creates a fresh cursor key");
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(random)
}

async fn container_exists(name: &str) -> bool {
    let output = podman_server::command_output(
        "podman",
        &["container", "exists", name],
        Duration::from_secs(10),
    )
    .await;
    output.status.success()
}
