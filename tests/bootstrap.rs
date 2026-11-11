#[path = "support/accounts.rs"]
#[allow(dead_code)]
mod account_support;
#[path = "support/database.rs"]
mod database_support;
#[path = "support/openapi.rs"]
#[allow(dead_code)]
mod openapi_validation;

use std::{process::Stdio, time::Duration};

use account_support::{migrated_database, seed_account};
use database_support::{command_output, wait_until_postgres_is_ready};
use serde_json::Value;
use sqlx::Row;
use tokio::{process::Command, sync::Barrier, time::timeout};

fn bootstrap_command(database_url: &str, username: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ferl-server"));
    command
        .env_clear()
        .env("FERL_DATABASE_URL", database_url)
        // These are deliberately irrelevant to the bootstrap profile.
        .env("FERL_PUBLIC_BASE_URL", "not-a-url")
        .env("FERL_CURSOR_KEY", "not-a-cursor-key")
        .arg("bootstrap-admin")
        .arg("--username")
        .arg(username)
        .kill_on_drop(true);
    command
}

async fn run_bootstrap(database_url: &str, username: &str) -> std::process::Output {
    timeout(
        Duration::from_secs(40),
        bootstrap_command(database_url, username).output(),
    )
    .await
    .expect("bootstrap child exits before its process deadline")
    .expect("ferl-server bootstrap executable starts")
}

fn one_line_json(bytes: &[u8]) -> Value {
    let text = std::str::from_utf8(bytes).expect("operator result is UTF-8");
    assert_eq!(
        text.lines().count(),
        1,
        "exactly one result line is emitted"
    );
    serde_json::from_str(text).expect("operator result is machine-readable JSON")
}

#[tokio::test]
async fn bootstrap_result_is_one_time_private_and_survives_postgres_restart() {
    let database = migrated_database().await;
    let url = database.schema_database_url().to_owned();
    let output = run_bootstrap(&url, "initial_admin").await;
    assert_eq!(output.status.code(), Some(0));
    let result = one_line_json(&output.stdout);
    assert!(openapi_validation::validates("AccountCreation", &result));
    assert_eq!(
        result
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>(),
        ["account", "api_key"].into_iter().collect()
    );
    assert_eq!(
        result["account"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>(),
        [
            "id",
            "username",
            "role",
            "enabled",
            "created_at",
            "updated_at"
        ]
        .into_iter()
        .collect()
    );
    assert_eq!(result["account"]["username"], "initial_admin");
    assert_eq!(result["account"]["role"], "admin");
    assert_eq!(result["account"]["enabled"], true);
    let key = result["api_key"].as_str().unwrap();
    assert!(key.starts_with("ferl_ak_v1_"));
    let stderr = String::from_utf8(output.stderr).expect("diagnostics use UTF-8 stderr");
    assert!(stderr.contains("bootstrap-admin committed"));
    assert!(!stderr.contains(key));
    assert!(!stderr.contains(&url));
    assert!(!stderr.contains("initial_admin"));

    let state: bool = sqlx::query_scalar("SELECT bootstrap_completed FROM system_state")
        .fetch_one(database.pool())
        .await
        .expect("bootstrap state is persisted");
    assert!(state);
    let digest: Vec<u8> = sqlx::query_scalar("SELECT api_key_digest FROM accounts WHERE id = $1")
        .bind(
            result["account"]["id"]
                .as_str()
                .unwrap()
                .parse::<uuid::Uuid>()
                .unwrap(),
        )
        .fetch_one(database.pool())
        .await
        .expect("the created account digest is persisted");
    assert_eq!(
        digest,
        ferl_server::domain::secret::SecretDigest::from_presented_secret(key)
            .as_bytes()
            .to_vec()
    );

    let repeated = run_bootstrap(&url, "replacement_admin").await;
    assert_eq!(repeated.status.code(), Some(1));
    assert!(
        repeated.stdout.is_empty(),
        "repeat never emits a replacement secret"
    );
    let stderr = String::from_utf8(repeated.stderr).unwrap();
    assert!(stderr.contains("bootstrap_already_completed"));
    assert!(!stderr.contains(key));
    assert!(!stderr.contains(&url));

    let stopped = command_output(
        "podman",
        &[
            "stop",
            "--time",
            "10",
            database_support::selected_container_name(),
        ],
    )
    .await;
    assert!(
        stopped.status.success(),
        "only the existing harness container is stopped"
    );
    let started = command_output(
        "podman",
        &["start", database_support::selected_container_name()],
    )
    .await;
    assert!(
        started.status.success(),
        "the harness container restarts with its named volume"
    );
    wait_until_postgres_is_ready().await;
    let state_after_restart: bool =
        sqlx::query_scalar("SELECT bootstrap_completed FROM system_state")
            .fetch_one(database.pool())
            .await
            .expect("database pool reconnects after restart");
    assert!(state_after_restart);
    let after_restart = run_bootstrap(&url, "replacement_admin").await;
    assert_eq!(after_restart.status.code(), Some(1));
    assert!(after_restart.stdout.is_empty());
    assert!(
        !String::from_utf8(after_restart.stderr)
            .unwrap()
            .contains(key)
    );

    database.close().await;
}

#[tokio::test]
async fn concurrent_bootstrap_processes_have_exactly_one_winner() {
    let database = migrated_database().await;
    let url = database.schema_database_url().to_owned();
    let barrier = std::sync::Arc::new(Barrier::new(3));
    let mut tasks = Vec::new();
    for _ in 0..2 {
        let url = url.clone();
        let barrier = barrier.clone();
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            run_bootstrap(&url, "race_admin").await
        }));
    }
    barrier.wait().await;
    let mut outputs = Vec::new();
    for task in tasks {
        outputs.push(
            timeout(Duration::from_secs(45), task)
                .await
                .expect("bootstrap process task completes before deadline")
                .expect("bootstrap process task does not panic"),
        );
    }
    assert_eq!(
        outputs
            .iter()
            .filter(|output| output.status.success())
            .count(),
        1
    );
    let winner = outputs
        .iter()
        .find(|output| output.status.success())
        .unwrap();
    assert!(
        outputs
            .iter()
            .filter(|output| !output.status.success())
            .all(|output| output.stdout.is_empty())
    );
    let result = one_line_json(&winner.stdout);
    assert!(openapi_validation::validates("AccountCreation", &result));
    assert_eq!(result["account"]["username"], "race_admin");
    assert!(
        result["api_key"]
            .as_str()
            .unwrap()
            .starts_with("ferl_ak_v1_")
    );
    let accounts: i64 = sqlx::query_scalar("SELECT count(*) FROM accounts")
        .fetch_one(database.pool())
        .await
        .unwrap();
    assert_eq!(accounts, 1);
    let completed: bool = sqlx::query_scalar("SELECT bootstrap_completed FROM system_state")
        .fetch_one(database.pool())
        .await
        .unwrap();
    assert!(completed);
    for output in outputs {
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(!stderr.contains(result["api_key"].as_str().unwrap()));
        assert!(!stderr.contains(&url));
        assert!(!stderr.contains("race_admin"));
    }
    database.close().await;
}

#[tokio::test]
async fn bootstrap_conflict_rolls_back_completion_and_broken_result_stream_never_reopens() {
    let database = migrated_database().await;
    let url = database.schema_database_url().to_owned();
    let invalid_username = run_bootstrap(&url, "Invalid name").await;
    assert_eq!(invalid_username.status.code(), Some(2));
    assert!(invalid_username.stdout.is_empty());
    let invalid_diagnostic = String::from_utf8(invalid_username.stderr).unwrap();
    assert!(invalid_diagnostic.contains("bootstrap_username_invalid"));
    assert!(!invalid_diagnostic.contains("Invalid name"));
    assert!(!invalid_diagnostic.contains("ferl_ak_v1_"));
    let _existing = seed_account(database.pool(), "existing_user", "user", true).await;
    let conflict = run_bootstrap(&url, "existing_user").await;
    assert_eq!(conflict.status.code(), Some(1));
    assert!(conflict.stdout.is_empty());
    let state: bool = sqlx::query_scalar("SELECT bootstrap_completed FROM system_state")
        .fetch_one(database.pool())
        .await
        .unwrap();
    assert!(!state, "username conflict rolls back bootstrap completion");
    let admin_count: i64 = sqlx::query_scalar("SELECT count(*) FROM accounts WHERE role = 'admin'")
        .fetch_one(database.pool())
        .await
        .unwrap();
    assert_eq!(admin_count, 0);
    let error = String::from_utf8(conflict.stderr).unwrap();
    assert!(error.contains("bootstrap_username_conflict"));
    assert!(!error.contains(&url));
    assert!(!error.contains("ferl_ak_v1_"));

    let mut command = bootstrap_command(&url, "lost_output_admin");
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn().expect("bootstrap process starts");
    drop(child.stdout.take());
    let output = timeout(Duration::from_secs(40), child.wait_with_output())
        .await
        .expect("broken-output bootstrap exits before deadline")
        .expect("bootstrap process is reaped");
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("bootstrap_result_output_failed"));
    assert!(!stderr.contains(&url));
    assert!(!stderr.contains("lost_output_admin"));
    assert!(!stderr.contains("ferl_ak_v1_"));

    let completed: bool = sqlx::query_scalar("SELECT bootstrap_completed FROM system_state")
        .fetch_one(database.pool())
        .await
        .unwrap();
    assert!(
        completed,
        "printing failure never reopens committed bootstrap"
    );
    let repeated = run_bootstrap(&url, "replacement_admin").await;
    assert_eq!(repeated.status.code(), Some(1));
    assert!(repeated.stdout.is_empty());
    assert!(
        String::from_utf8(repeated.stderr)
            .unwrap()
            .contains("bootstrap_already_completed")
    );
    let admins: i64 = sqlx::query_scalar("SELECT count(*) FROM accounts WHERE role = 'admin'")
        .fetch_one(database.pool())
        .await
        .unwrap();
    assert_eq!(admins, 1);
    let _row = sqlx::query("SELECT id FROM accounts WHERE username = 'lost_output_admin'")
        .fetch_one(database.pool())
        .await
        .expect("the committed result remains in storage despite output failure");
    let _ = _row.get::<uuid::Uuid, _>("id");
    let digest: Vec<u8> = sqlx::query_scalar(
        "SELECT api_key_digest FROM accounts WHERE username = 'lost_output_admin'",
    )
    .fetch_one(database.pool())
    .await
    .unwrap();
    let digest_hex = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    assert!(
        !stderr.contains(&digest_hex),
        "digest is not written to diagnostics"
    );
    database.close().await;
}

#[tokio::test]
async fn bootstrap_rejects_an_unmigrated_schema_without_applying_ddl() {
    let database = database_support::IsolatedTestDatabase::connect_from_harness().await;
    let output = run_bootstrap(database.schema_database_url(), "no_schema_admin").await;
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("bootstrap_schema_incompatible"));
    assert!(!stderr.contains(database.schema_database_url()));
    assert!(!stderr.contains("ferl_ak_v1_"));
    let migrated: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM information_schema.tables \
         WHERE table_schema = current_schema() AND table_name = 'accounts')",
    )
    .fetch_one(database.pool())
    .await
    .unwrap();
    assert!(!migrated, "bootstrap does not silently install the schema");
    database.close().await;
}

#[tokio::test]
async fn bootstrap_failure_after_admin_insert_rolls_back_admin_and_completion_together() {
    let database = migrated_database().await;
    sqlx::query(
        "CREATE FUNCTION ferl_test_reject_bootstrap_completion() RETURNS trigger \
         LANGUAGE plpgsql AS $$ BEGIN \
             IF NEW.bootstrap_completed THEN \
                 RAISE EXCEPTION 'controlled bootstrap failure' USING ERRCODE = 'P0001'; \
             END IF; \
             RETURN NEW; \
         END $$",
    )
    .execute(database.pool())
    .await
    .expect("only the isolated schema receives the controlled failure fixture");
    sqlx::query(
        "CREATE TRIGGER ferl_test_reject_bootstrap_completion \
         BEFORE UPDATE ON system_state FOR EACH ROW \
         EXECUTE FUNCTION ferl_test_reject_bootstrap_completion()",
    )
    .execute(database.pool())
    .await
    .unwrap();
    let output = run_bootstrap(database.schema_database_url(), "rollback_admin").await;
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("bootstrap_persistence_failure"));
    assert!(!stderr.contains("rollback_admin"));
    assert!(!stderr.contains("ferl_ak_v1_"));

    let completed: bool = sqlx::query_scalar("SELECT bootstrap_completed FROM system_state")
        .fetch_one(database.pool())
        .await
        .unwrap();
    let admins: i64 = sqlx::query_scalar("SELECT count(*) FROM accounts WHERE role = 'admin'")
        .fetch_one(database.pool())
        .await
        .unwrap();
    assert_eq!(
        (completed, admins),
        (false, 0),
        "insert and state change roll back together"
    );
    sqlx::query("DROP TRIGGER ferl_test_reject_bootstrap_completion ON system_state")
        .execute(database.pool())
        .await
        .unwrap();
    sqlx::query("DROP FUNCTION ferl_test_reject_bootstrap_completion()")
        .execute(database.pool())
        .await
        .unwrap();
    database.close().await;
}
