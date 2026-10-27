#[path = "support/database.rs"]
mod database_support;

use std::{collections::BTreeMap, process::Output, sync::Arc, time::Duration};

use ferl_server::{
    config::MigrationConfig,
    db::migrate::{self, MIGRATION_LOCK_KEY, SchemaIssue, VerificationError},
};
use sqlx::Row;
use tokio::{process::Command, sync::Barrier, time::timeout};

use database_support::IsolatedTestDatabase;

#[tokio::test]
async fn migration_cli_migrates_fresh_isolated_schema_and_is_idempotent() {
    let database = IsolatedTestDatabase::connect_from_harness().await;
    assert_eq!(
        migrate::verify_compatibility(database.pool()).await,
        Err(VerificationError::Incompatible(SchemaIssue::MissingHistory))
    );

    let first = run_migrate(database.schema_database_url(), &[]).await;
    assert!(first.status.success(), "first migration command succeeds");

    let second = run_migrate(database.schema_database_url(), &[]).await;
    assert!(
        second.status.success(),
        "repeated migration command succeeds"
    );
    migrate::verify_compatibility(database.pool())
        .await
        .expect("embedded migration history and schema are compatible");

    let history = sqlx::query("SELECT version, success FROM _sqlx_migrations ORDER BY version")
        .fetch_all(database.pool())
        .await
        .expect("migration history is queryable");
    assert_eq!(history.len(), 1, "repeated migrate records one version row");
    assert_eq!(history[0].get::<i64, _>("version"), 1);
    assert!(history[0].get::<bool, _>("success"));

    let table_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM information_schema.tables \
         WHERE table_schema = current_schema() AND table_type = 'BASE TABLE' \
           AND table_name <> '_sqlx_migrations'",
    )
    .fetch_one(database.pool())
    .await
    .expect("the expected schema's table inventory is readable");
    assert_eq!(table_count, 8);

    let state: (bool, bool) =
        sqlx::query_as("SELECT singleton, bootstrap_completed FROM system_state")
            .fetch_one(database.pool())
            .await
            .expect("exactly one bootstrap-state row is seeded");
    assert_eq!(state, (true, false));
    let reservations: Vec<String> =
        sqlx::query_scalar("SELECT short_code FROM code_reservations ORDER BY short_code")
            .fetch_all(database.pool())
            .await
            .expect("seed reservations can be inspected");
    assert_eq!(reservations, ["api"]);

    let reservation_columns: Vec<String> = sqlx::query_scalar(
        "SELECT column_name FROM information_schema.columns \
         WHERE table_schema = current_schema() AND table_name = 'code_reservations' \
         ORDER BY ordinal_position",
    )
    .fetch_all(database.pool())
    .await
    .expect("reservation shape is inspectable");
    assert_eq!(reservation_columns, ["short_code"]);
    database.close().await;
}

#[tokio::test]
async fn concurrent_migration_processes_create_one_consistent_history() {
    let database = IsolatedTestDatabase::connect_from_harness().await;
    let barrier = Arc::new(Barrier::new(3));
    let mut tasks = Vec::new();
    for _ in 0..2 {
        let url = database.schema_database_url().to_owned();
        let barrier = barrier.clone();
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            run_migrate(&url, &[]).await
        }));
    }
    barrier.wait().await;
    for task in tasks {
        let output = timeout(Duration::from_secs(45), task)
            .await
            .expect("concurrent migration process finishes before its deadline")
            .expect("migration task completes");
        assert!(
            output.status.success(),
            "both concurrent migration commands succeed"
        );
    }

    migrate::verify_compatibility(database.pool())
        .await
        .expect("concurrent migration commands leave a compatible schema");
    let versions: i64 = sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations")
        .fetch_one(database.pool())
        .await
        .expect("migration history is readable");
    assert_eq!(versions, 1, "the migration was applied exactly once");
    database.close().await;
}

#[tokio::test]
async fn migration_rejects_dirty_mismatched_and_newer_history_without_repairing_it() {
    let database = IsolatedTestDatabase::connect_from_harness().await;
    let url = database.schema_database_url().to_owned();
    let config = MigrationConfig::from_map(BTreeMap::from([(
        "FERL_DATABASE_URL".to_owned(),
        url.clone(),
    )]))
    .expect("migration profile is valid without serving settings");
    migrate::run_migrations(&config)
        .await
        .expect("initial migration succeeds");
    let checksum: Vec<u8> =
        sqlx::query_scalar("SELECT checksum FROM _sqlx_migrations WHERE version = 1")
            .fetch_one(database.pool())
            .await
            .expect("installed checksum is recorded");

    sqlx::query("UPDATE _sqlx_migrations SET success = false WHERE version = 1")
        .execute(database.pool())
        .await
        .expect("test marks its owned migration dirty");
    assert_eq!(
        migrate::verify_compatibility(database.pool()).await,
        Err(VerificationError::Incompatible(SchemaIssue::DirtyMigration))
    );
    let dirty = run_migrate(&url, &[]).await;
    assert_eq!(dirty.status.code(), Some(1));
    let success: bool =
        sqlx::query_scalar("SELECT success FROM _sqlx_migrations WHERE version = 1")
            .fetch_one(database.pool())
            .await
            .expect("failed migration history remains available");
    assert!(!success, "migrate does not rewrite a dirty row");

    sqlx::query("UPDATE _sqlx_migrations SET success = true WHERE version = 1")
        .execute(database.pool())
        .await
        .expect("test restores its migration row before the next case");
    sqlx::query("UPDATE _sqlx_migrations SET checksum = $1 WHERE version = 1")
        .bind(vec![0_u8; checksum.len()])
        .execute(database.pool())
        .await
        .expect("test changes only its owned history checksum");
    assert_eq!(
        migrate::verify_compatibility(database.pool()).await,
        Err(VerificationError::Incompatible(
            SchemaIssue::HistoryMismatch
        ))
    );
    let mismatch = run_migrate(&url, &[]).await;
    assert_eq!(mismatch.status.code(), Some(1));
    let changed_checksum: Vec<u8> =
        sqlx::query_scalar("SELECT checksum FROM _sqlx_migrations WHERE version = 1")
            .fetch_one(database.pool())
            .await
            .expect("changed checksum remains unchanged by migrate");
    assert_eq!(changed_checksum, vec![0_u8; checksum.len()]);

    sqlx::query("UPDATE _sqlx_migrations SET checksum = $1 WHERE version = 1")
        .bind(checksum)
        .execute(database.pool())
        .await
        .expect("test restores the exact embedded checksum");
    sqlx::query(
        "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) \
         VALUES (2, 'unexpected', true, $1, 0)",
    )
    .bind(vec![7_u8; 48])
    .execute(database.pool())
    .await
    .expect("test adds an unsupported newer migration row");
    assert_eq!(
        migrate::verify_compatibility(database.pool()).await,
        Err(VerificationError::Incompatible(SchemaIssue::NewerVersion))
    );
    let newer = run_migrate(&url, &[]).await;
    assert_eq!(newer.status.code(), Some(1));
    let history_count: i64 = sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations")
        .fetch_one(database.pool())
        .await
        .expect("unsupported history remains unchanged");
    assert_eq!(history_count, 2);
    database.close().await;
}

#[tokio::test]
async fn migration_lock_wait_is_bounded_and_missing_objects_are_not_repaired() {
    let database = IsolatedTestDatabase::connect_from_harness().await;
    let mut lock_connection = database
        .pool()
        .acquire()
        .await
        .expect("a fixture session is acquired for the migration advisory lock");
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(MIGRATION_LOCK_KEY)
        .execute(&mut *lock_connection)
        .await
        .expect("test holds the exact deployment-wide migration lock");

    let started = tokio::time::Instant::now();
    let timed_out = run_migrate(
        database.schema_database_url(),
        &[
            ("FERL_MIGRATE_CONNECT_TIMEOUT_SECS", "1"),
            ("FERL_MIGRATE_LOCK_TIMEOUT_SECS", "1"),
            ("FERL_MIGRATE_TOTAL_TIMEOUT_SECS", "4"),
        ],
    )
    .await;
    assert_eq!(timed_out.status.code(), Some(1));
    assert!(started.elapsed() < Duration::from_secs(4));
    assert!(combined_output(&timed_out).contains("migration_lock_timeout"));
    let released: bool = sqlx::query_scalar("SELECT pg_advisory_unlock($1)")
        .bind(MIGRATION_LOCK_KEY)
        .fetch_one(&mut *lock_connection)
        .await
        .expect("test releases its session-scoped migration lock");
    assert!(released);
    drop(lock_connection);

    let url = database.schema_database_url().to_owned();
    let config = MigrationConfig::from_map(BTreeMap::from([(
        "FERL_DATABASE_URL".to_owned(),
        url.clone(),
    )]))
    .expect("migration-specific profile is valid");
    migrate::run_migrations(&config)
        .await
        .expect("migration proceeds after the advisory lock is released");
    sqlx::query("DROP TABLE rate_charges")
        .execute(database.pool())
        .await
        .expect("test drops only a table in its uniquely owned schema");
    assert_eq!(
        migrate::verify_compatibility(database.pool()).await,
        Err(VerificationError::Incompatible(SchemaIssue::MissingTable))
    );
    let command = run_migrate(&url, &[]).await;
    assert_eq!(command.status.code(), Some(1));
    let table_exists: bool = sqlx::query_scalar("SELECT to_regclass('rate_charges') IS NOT NULL")
        .fetch_one(database.pool())
        .await
        .expect("test can confirm migration did not recreate missing storage");
    assert!(!table_exists);
    database.close().await;
}

#[tokio::test]
async fn schema_verifier_detects_nullability_and_minimal_reservation_drift() {
    let database = IsolatedTestDatabase::connect_from_harness().await;
    let config = MigrationConfig::from_map(BTreeMap::from([(
        "FERL_DATABASE_URL".to_owned(),
        database.schema_database_url().to_owned(),
    )]))
    .expect("migration-specific profile is valid");
    migrate::run_migrations(&config)
        .await
        .expect("isolated schema migrates");

    sqlx::query("ALTER TABLE accounts ALTER COLUMN enabled DROP NOT NULL")
        .execute(database.pool())
        .await
        .expect("test changes nullability only in its owned schema");
    assert_eq!(
        migrate::verify_compatibility(database.pool()).await,
        Err(VerificationError::Incompatible(SchemaIssue::ColumnMismatch))
    );
    sqlx::query("ALTER TABLE accounts ALTER COLUMN enabled SET NOT NULL")
        .execute(database.pool())
        .await
        .expect("test restores the authoritative nullability");

    sqlx::query("ALTER TABLE code_reservations ADD COLUMN destination_url text")
        .execute(database.pool())
        .await
        .expect("test adds metadata only to its owned reservation table");
    assert_eq!(
        migrate::verify_compatibility(database.pool()).await,
        Err(VerificationError::Incompatible(SchemaIssue::ColumnMismatch))
    );
    sqlx::query("ALTER TABLE code_reservations DROP COLUMN destination_url")
        .execute(database.pool())
        .await
        .expect("test restores the deliberately minimal reservation shape");
    migrate::verify_compatibility(database.pool())
        .await
        .expect("restored schema verifies again");
    database.close().await;
}

#[tokio::test]
async fn failed_initial_migration_rolls_back_ddl_and_success_history_together() {
    let database = IsolatedTestDatabase::connect_from_harness().await;
    let config = MigrationConfig::from_map(BTreeMap::from([(
        "FERL_DATABASE_URL".to_owned(),
        database.schema_database_url().to_owned(),
    )]))
    .expect("migration-specific profile is valid");
    sqlx::query("CREATE TABLE accounts (id integer)")
        .execute(database.pool())
        .await
        .expect("test introduces a migration conflict only in its owned schema");

    assert_eq!(
        migrate::run_migrations(&config).await,
        Err(migrate::MigrationFailure::MigrationExecutionFailed)
    );
    let system_state_exists: bool =
        sqlx::query_scalar("SELECT to_regclass('system_state') IS NOT NULL")
            .fetch_one(database.pool())
            .await
            .expect("test can inspect rolled-back schema state");
    assert!(!system_state_exists, "partial migration DDL is rolled back");
    let successful_rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations WHERE success")
            .fetch_one(database.pool())
            .await
            .expect("SQLx migration history table remains queryable");
    assert_eq!(successful_rows, 0, "no false success row is committed");

    sqlx::query("DROP TABLE accounts")
        .execute(database.pool())
        .await
        .expect("test removes only its deliberately conflicting fixture table");
    migrate::run_migrations(&config)
        .await
        .expect("migration lock was released and clean retry applies after fixture repair");
    migrate::verify_compatibility(database.pool())
        .await
        .expect("completed migration has valid history and schema");
    database.close().await;
}

#[tokio::test]
async fn migration_configuration_is_command_specific_and_failures_are_sanitized() {
    let secret_url = "postgres://migrate-user:database-secret@127.0.0.1:1/ferl_test";
    let malformed = run_migrate(
        secret_url,
        &[
            ("FERL_MIGRATE_LOCK_TIMEOUT_SECS", "not-a-duration"),
            ("FERL_PUBLIC_BASE_URL", "not an origin"),
            ("FERL_CURSOR_KEY", "not a cursor key"),
            ("FERL_LISTEN_ADDR", "not a socket"),
        ],
    )
    .await;
    assert_eq!(malformed.status.code(), Some(2));
    let malformed_output = combined_output(&malformed);
    assert!(malformed_output.contains("FERL_MIGRATE_LOCK_TIMEOUT_SECS"));
    assert!(!malformed_output.contains("database-secret"));
    assert!(!malformed_output.contains("not-a-duration"));
    assert!(!malformed_output.contains("not an origin"));
    assert!(!malformed_output.contains("postgres://"));

    let unavailable = run_migrate(secret_url, &[("FERL_MIGRATE_CONNECT_TIMEOUT_SECS", "1")]).await;
    assert_eq!(unavailable.status.code(), Some(1));
    let unavailable_output = combined_output(&unavailable);
    assert!(unavailable_output.contains("migration_connection_unavailable"));
    assert!(!unavailable_output.contains("database-secret"));
    assert!(!unavailable_output.contains("postgres://"));
    assert!(!unavailable_output.contains("127.0.0.1:1"));
}

fn combined_output(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

async fn run_migrate(database_url: &str, extra: &[(&str, &str)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ferl-server"));
    command
        .env_clear()
        .env("FERL_DATABASE_URL", database_url)
        .arg("migrate")
        .kill_on_drop(true);
    for (name, value) in extra {
        command.env(name, value);
    }
    timeout(Duration::from_secs(45), command.output())
        .await
        .expect("migration executable finishes before its test deadline")
        .expect("migration executable can be launched")
}
