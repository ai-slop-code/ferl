#[path = "support/database.rs"]
mod database_support;

use std::time::Duration;

use ferl_server::db::{
    coordination::{
        ACCOUNT_AUTHORITY_LOCK_KEY, CoordinationError, acquire_account_authority_exclusive,
        acquire_account_authority_shared, acquire_account_authority_shared_bounded,
        begin_read_committed,
    },
    error::{DatabaseFailure, TransientFailure},
};
use sqlx::PgPool;
use tokio::{
    sync::oneshot,
    time::{sleep, timeout},
};
use uuid::Uuid;

use database_support::IsolatedTestDatabase;

#[tokio::test]
async fn shared_authority_locks_coexist_and_exclusive_gate_waits_for_commit_and_rollback() {
    let database = IsolatedTestDatabase::connect_from_harness().await;
    migrate(&database).await;
    let pool = database.pool();
    let mut shared_commit = begin_read_committed(pool)
        .await
        .expect("shared holder transaction begins at READ COMMITTED");
    acquire_account_authority_shared(&mut shared_commit)
        .await
        .expect("first shared authority lock is acquired");
    let mut shared_rollback = begin_read_committed(pool)
        .await
        .expect("second shared transaction begins");
    acquire_account_authority_shared(&mut shared_rollback)
        .await
        .expect("shared authority locks coexist");
    assert_lock_state(pool, 2, 0).await;

    let pool_for_waiter = pool.clone();
    let (started_tx, started_rx) = oneshot::channel();
    let (acquired_tx, acquired_rx) = oneshot::channel();
    let waiter = tokio::spawn(async move {
        let mut transaction = begin_read_committed(&pool_for_waiter)
            .await
            .expect("exclusive waiter transaction begins");
        started_tx.send(()).expect("test receiver remains open");
        let acquired = acquire_account_authority_exclusive(&mut transaction).await;
        acquired_tx
            .send(acquired)
            .expect("test receiver remains open");
        if acquired.is_ok() {
            transaction
                .commit()
                .await
                .expect("exclusive transaction commits");
        } else {
            transaction
                .rollback()
                .await
                .expect("failed lock transaction rolls back");
        }
    });
    timeout(Duration::from_secs(3), started_rx)
        .await
        .expect("exclusive waiter starts before the deadline")
        .expect("exclusive waiter starts");
    wait_for_lock_state(pool, 2, 1).await;
    shared_commit
        .commit()
        .await
        .expect("commit releases the first transaction-scoped shared lock");
    wait_for_lock_state(pool, 1, 1).await;
    shared_rollback
        .rollback()
        .await
        .expect("rollback releases the second shared lock");
    let acquired = timeout(Duration::from_secs(3), acquired_rx)
        .await
        .expect("exclusive waiter proceeds after shared locks release")
        .expect("exclusive waiter reports its result");
    assert!(acquired.is_ok());
    timeout(Duration::from_secs(3), waiter)
        .await
        .expect("exclusive task finishes before the deadline")
        .expect("exclusive task joins");
    assert_lock_state(pool, 0, 0).await;
    database.close().await;
}

#[tokio::test]
async fn bounded_wait_failure_is_sanitized_and_read_committed_revalidation_is_fresh() {
    let database = IsolatedTestDatabase::connect_from_harness().await;
    migrate(&database).await;
    let pool = database.pool();
    let mut exclusive_holder = begin_read_committed(pool)
        .await
        .expect("exclusive holder transaction begins");
    acquire_account_authority_exclusive(&mut exclusive_holder)
        .await
        .expect("exclusive authority lock is acquired");

    let mut blocked_shared = begin_read_committed(pool)
        .await
        .expect("bounded waiter transaction begins");
    let timeout_result =
        acquire_account_authority_shared_bounded(&mut blocked_shared, Duration::from_millis(150))
            .await;
    assert_eq!(timeout_result, Err(CoordinationError::DeadlineExceeded));
    blocked_shared
        .rollback()
        .await
        .expect("timed-out waiter transaction is rolled back, not reused");
    assert_lock_state(pool, 1, 0).await;
    exclusive_holder
        .rollback()
        .await
        .expect("holder rollback releases the exclusive transaction lock");
    assert_lock_state(pool, 0, 0).await;

    let account_id = Uuid::from_u128(7001);
    sqlx::query(
        "INSERT INTO accounts (id, username, role, enabled, api_key_digest) \
         VALUES ($1, 'fresh_snapshot', 'user', true, $2)",
    )
    .bind(account_id)
    .bind(vec![70_u8; 32])
    .execute(pool)
    .await
    .expect("authority fixture account inserts");

    let mut invalidation = begin_read_committed(pool)
        .await
        .expect("authority invalidation begins");
    acquire_account_authority_exclusive(&mut invalidation)
        .await
        .expect("invalidation acquires the exclusive gate first");
    sqlx::query("UPDATE accounts SET enabled = false WHERE id = $1")
        .bind(account_id)
        .execute(&mut *invalidation)
        .await
        .expect("invalidation changes the protected authority");

    let pool_for_mutation = pool.clone();
    let (pre_read_tx, pre_read_rx) = oneshot::channel();
    let (revalidated_tx, revalidated_rx) = oneshot::channel();
    let mutation = tokio::spawn(async move {
        let mut transaction = begin_read_committed(&pool_for_mutation)
            .await
            .expect("guarded mutation transaction begins");
        let old_enabled: bool = sqlx::query_scalar("SELECT enabled FROM accounts WHERE id = $1")
            .bind(account_id)
            .fetch_one(&mut *transaction)
            .await
            .expect("pre-lock observation sees the last committed authority");
        pre_read_tx
            .send(old_enabled)
            .expect("test receiver remains open");
        let lock_result = acquire_account_authority_shared(&mut transaction).await;
        if lock_result.is_err() {
            let _ = transaction.rollback().await;
            revalidated_tx
                .send(None)
                .expect("test receiver remains open");
            return;
        }
        let current_enabled =
            sqlx::query_scalar::<_, bool>("SELECT enabled FROM accounts WHERE id = $1")
                .bind(account_id)
                .fetch_one(&mut *transaction)
                .await
                .expect("post-lock authority read uses a fresh READ COMMITTED snapshot");
        revalidated_tx
            .send(Some(current_enabled))
            .expect("test receiver remains open");
        transaction
            .rollback()
            .await
            .expect("read-only guard rolls back");
    });

    assert!(
        timeout(Duration::from_secs(3), pre_read_rx)
            .await
            .expect("mutation pre-read completes")
            .expect("mutation sent pre-read"),
        "mutation's pre-lock snapshot observes enabled authority"
    );
    wait_for_lock_state(pool, 1, 1).await;
    invalidation
        .commit()
        .await
        .expect("committing invalidation releases the exclusive gate");
    let current = timeout(Duration::from_secs(3), revalidated_rx)
        .await
        .expect("mutation revalidates after the exclusive lock commits")
        .expect("mutation sends a revalidation result");
    assert_eq!(current, Some(false));
    timeout(Duration::from_secs(3), mutation)
        .await
        .expect("guarded mutation task finishes")
        .expect("guarded mutation joins");
    assert_lock_state(pool, 0, 0).await;

    let enabled: bool = sqlx::query_scalar("SELECT enabled FROM accounts WHERE id = $1")
        .bind(account_id)
        .fetch_one(pool)
        .await
        .expect("test can inspect the committed invalidation");
    assert!(!enabled, "the exclusive invalidation remains committed");
    let isolation = sqlx::query_scalar::<_, String>("SHOW transaction_isolation")
        .fetch_one(pool)
        .await
        .expect("PostgreSQL reports default transaction isolation");
    assert_eq!(isolation, "read committed");

    assert_eq!(ACCOUNT_AUTHORITY_LOCK_KEY, (0x4645_524c, 0x4143_4354));
    assert_eq!(
        CoordinationError::Database(DatabaseFailure::Unavailable(
            TransientFailure::TimeoutOrLock
        ))
        .safe_category(),
        "database_timeout_or_lock"
    );
    database.close().await;
}

async fn migrate(database: &IsolatedTestDatabase) {
    let config =
        ferl_server::config::MigrationConfig::from_map(std::collections::BTreeMap::from([(
            "FERL_DATABASE_URL".to_owned(),
            database.schema_database_url().to_owned(),
        )]))
        .expect("valid isolated migration configuration");
    ferl_server::db::migrate::run_migrations(&config)
        .await
        .expect("isolated PostgreSQL fixture migrates");
}

async fn assert_lock_state(pool: &PgPool, granted: i64, waiting: i64) {
    let actual = lock_state(pool).await;
    assert_eq!(actual, (granted, waiting));
}

async fn wait_for_lock_state(pool: &PgPool, granted: i64, waiting: i64) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if lock_state(pool).await == (granted, waiting) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "PostgreSQL advisory lock state did not reach the synchronized expectation"
        );
        sleep(Duration::from_millis(10)).await;
    }
}

async fn lock_state(pool: &PgPool) -> (i64, i64) {
    sqlx::query_as(
        "SELECT count(*) FILTER (WHERE granted), count(*) FILTER (WHERE NOT granted) \
         FROM pg_catalog.pg_locks \
         WHERE locktype = 'advisory' \
           AND database = (SELECT oid FROM pg_database WHERE datname = current_database()) \
           AND classid::bigint = $1 AND objid::bigint = $2 AND objsubid = 2",
    )
    .bind(i64::from(ACCOUNT_AUTHORITY_LOCK_KEY.0))
    .bind(i64::from(ACCOUNT_AUTHORITY_LOCK_KEY.1))
    .fetch_one(pool)
    .await
    .expect("PostgreSQL exposes the advisory lock state")
}
