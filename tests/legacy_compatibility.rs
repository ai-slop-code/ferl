#[path = "support/accounts.rs"]
#[allow(dead_code)]
mod account_support;
#[path = "support/database.rs"]
#[allow(dead_code)]
mod database_support;
#[path = "support/serving_process.rs"]
mod serving_process;

use account_support::seed_account;
use database_support::IsolatedTestDatabase;
use futures_util::FutureExt;
use serving_process::{ServingProcess, assert_headers, json, send};
use std::{panic::AssertUnwindSafe, path::PathBuf, process::Output, time::Duration};
use tokio::{process::Command, time::timeout};
use uuid::Uuid;

const PART_07_BASE: &str = "9bcf7c0c021c994e76d0696633fe822f0a49a5e1";
const CURSOR_KEY: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

struct LegacyWorktree {
    repository: PathBuf,
    worktree: PathBuf,
    target: PathBuf,
}

impl LegacyWorktree {
    async fn build() -> Result<(Self, PathBuf), String> {
        let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let scratch = std::env::temp_dir().join("opencode");
        tokio::fs::create_dir_all(&scratch)
            .await
            .expect("approved compatibility scratch directory exists");
        let unique = Uuid::new_v4().simple().to_string();
        let worktree = scratch.join(format!("ferl-part07-{unique}"));
        let target = scratch.join(format!("ferl-part07-target-{unique}"));
        let guard = Self {
            repository: repository.clone(),
            worktree: worktree.clone(),
            target: target.clone(),
        };
        if let Err(message) = git_worktree_add(&repository, &worktree).await {
            guard.cleanup().await?;
            return Err(message);
        }
        let mut build = Command::new("cargo");
        build
            .current_dir(&worktree)
            .env("CARGO_TARGET_DIR", &target)
            .arg("build")
            .arg("--locked")
            .arg("--bin")
            .arg("ferl-server")
            .kill_on_drop(true);
        // This is a deliberately isolated cold build of retained Part 07
        // dependencies. Ten minutes remains bounded while allowing clean CI
        // workers to compile the historical graph without a warmed target dir.
        let result = match timeout(Duration::from_secs(600), build.output()).await {
            Ok(Ok(output)) => output,
            Ok(Err(_)) => {
                guard.cleanup().await?;
                return Err("Cargo could not build the retained Part 07 source".to_owned());
            }
            Err(_) => {
                guard.cleanup().await?;
                return Err("Part 07 server build exceeded its 600-second deadline".to_owned());
            }
        };
        if !result.status.success() {
            let message = format!("Part 07 server build failed: {}", output(&result));
            guard.cleanup().await?;
            return Err(message);
        }
        Ok((guard, target.join("debug/ferl-server")))
    }

    async fn cleanup(self) -> Result<(), String> {
        let mut command = Command::new("git");
        command
            .arg("-C")
            .arg(&self.repository)
            .args(["worktree", "remove"])
            .arg(&self.worktree)
            .kill_on_drop(true);
        let removed = timeout(Duration::from_secs(20), command.output())
            .await
            .map_err(|_| "temporary Part 07 worktree removal exceeded its deadline".to_owned())?
            .map_err(|_| "Git could not remove the temporary Part 07 worktree".to_owned())?;
        if !removed.status.success() {
            return Err(format!(
                "temporary worktree cleanup failed: {}",
                output(&removed)
            ));
        }
        timeout(
            Duration::from_secs(20),
            tokio::fs::remove_dir_all(&self.target),
        )
        .await
        .map_err(|_| "temporary Part 07 build cleanup exceeded its deadline".to_owned())?
        .map_err(|_| "temporary Part 07 build output could not be removed".to_owned())?;
        Ok(())
    }
}

#[tokio::test]
async fn v1_server_runs_on_v1_then_fails_closed_after_v2_upgrade_and_v2_runs_ready() {
    let (legacy_worktree, legacy_binary) = LegacyWorktree::build()
        .await
        .expect("retained Part 07 binary builds for compatibility evidence");
    let legacy_binary_for_test = legacy_binary.clone();
    let outcome = AssertUnwindSafe(async move {
        let database = IsolatedTestDatabase::connect_from_harness().await;
        database
            .verify_owned(move |pool, schema_url| {
                let legacy_binary = legacy_binary_for_test.clone();
                Box::pin(async move {
                    let old_migration = run_migrate(&legacy_binary, &schema_url).await;
                    assert!(
                        old_migration.status.success(),
                        "v1 binary migrates v1 schema: {}",
                        output(&old_migration)
                    );
                    let (_, key) = seed_account(&pool, "binary_compat_owner", "user", true).await;

                    let legacy = ServingProcess::start_binary_with(
                        legacy_binary.to_str().unwrap(),
                        &schema_url,
                        CURSOR_KEY,
                        &[],
                    )
                    .await;
                    let legacy_collection =
                        send(legacy.address, "GET", "/api/v1/links", Some(&key), "")
                            .await
                            .unwrap();
                    assert_headers(&legacy_collection);
                    assert_eq!(legacy_collection.status, 405);
                    assert_eq!(legacy_collection.headers["allow"], "POST");
                    let _ = legacy.stop_and_capture().await;

                    let config = ferl_server::config::MigrationConfig::from_map(
                        std::collections::BTreeMap::from([(
                            "FERL_DATABASE_URL".to_owned(),
                            schema_url.clone(),
                        )]),
                    )
                    .expect("v2 migration profile is valid");
                    ferl_server::db::migrate::run_migrations(&config)
                        .await
                        .expect("new binary upgrades the actual v1 schema to v2");

                    let incompatible = ServingProcess::start_binary_without_readiness(
                        legacy_binary.to_str().unwrap(),
                        &schema_url,
                        CURSOR_KEY,
                        &[],
                    )
                    .await;
                    let ready_deadline = tokio::time::Instant::now() + Duration::from_secs(10);
                    let response = loop {
                        match send(incompatible.address, "GET", "/api/health/ready", None, "").await
                        {
                            Ok(response) => break Some(response),
                            Err(_) if tokio::time::Instant::now() < ready_deadline => {
                                tokio::time::sleep(Duration::from_millis(50)).await;
                            }
                            Err(_) => break None,
                        }
                    };
                    let old_output = incompatible.stop_and_capture().await;
                    assert!(
                        response.is_some(),
                        "old server answers health before deadline"
                    );
                    let response = response.unwrap();
                    assert_headers(&response);
                    assert_eq!(response.status, 503);
                    assert!(
                        old_output
                            .stdout
                            .contains("schema_version_newer_than_binary")
                            || old_output
                                .stderr
                                .contains("schema_version_newer_than_binary"),
                        "old binary reports the safe incompatibility category"
                    );

                    let current = ServingProcess::start(&schema_url, CURSOR_KEY).await;
                    let listing = send(current.address, "GET", "/api/v1/links", Some(&key), "")
                        .await
                        .unwrap();
                    assert_headers(&listing);
                    assert_eq!(listing.status, 200);
                    assert_eq!(json(&listing)["items"], serde_json::json!([]));
                    current.stop().await;
                })
            })
            .await;
    })
    .catch_unwind()
    .await;
    let cleanup = legacy_worktree.cleanup().await;
    assert!(
        cleanup.is_ok(),
        "temporary compatibility worktree cleans up: {cleanup:?}"
    );
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

async fn git_worktree_add(
    repository: &std::path::Path,
    worktree: &std::path::Path,
) -> Result<(), String> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(repository)
        .args(["worktree", "add", "--detach"])
        .arg(worktree)
        .arg(PART_07_BASE)
        .kill_on_drop(true);
    let result = timeout(Duration::from_secs(20), command.output())
        .await
        .map_err(|_| "Git worktree creation exceeded its deadline".to_owned())?
        .map_err(|_| "Git could not create the retained-source worktree".to_owned())?;
    if result.status.success() {
        Ok(())
    } else {
        Err(format!(
            "Part 07 worktree creation failed: {}",
            output(&result)
        ))
    }
}

async fn run_migrate(binary: &std::path::Path, database_url: &str) -> Output {
    let mut command = Command::new(binary);
    command
        .env_clear()
        .env("FERL_DATABASE_URL", database_url)
        .arg("migrate")
        .kill_on_drop(true);
    timeout(Duration::from_secs(45), command.output())
        .await
        .expect("legacy migration process finishes before its deadline")
        .expect("legacy migration process starts")
}

fn output(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}
