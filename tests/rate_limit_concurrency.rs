#[path = "support/accounts.rs"]
#[allow(dead_code)]
mod account_support;
#[path = "support/database.rs"]
mod database_support;
#[path = "support/rate_limits.rs"]
#[allow(dead_code)]
mod rate_support;

use account_support::{migrated_database, seed_account};
use ferl_server::{
    auth,
    db::coordination::{acquire_account_authority_exclusive, begin_read_committed},
    rate_limit::CreationFailure,
};
use rate_support::{close, count, parts, prepare, state, wait_for_query};
use std::{sync::Arc, time::Duration};
use tokio::{sync::Barrier, task::JoinSet, time::timeout};
use uuid::Uuid;

#[tokio::test]
async fn concurrent_first_use_shared_guards_and_unique_buckets_admit_exact_limit() {
    let db = migrated_database().await;
    db.verify(|db| Box::pin(async move {
    let (id, key) = seed_account(db.pool(), "concurrent_user", "user", true).await;
    for account in [false, true] {
        let bucket = Uuid::new_v4();
        let mut holder = begin_read_committed(db.pool()).await.unwrap();
        if account {
            sqlx::query(
                "INSERT INTO rate_buckets (id, bucket_kind, account_id) VALUES ($1, 'account', $2)",
            )
            .bind(bucket)
            .bind(id.as_uuid())
            .execute(&mut *holder)
            .await
            .unwrap();
        } else {
            sqlx::query("INSERT INTO rate_buckets (id, bucket_kind, client_ip) VALUES ($1, 'anonymous_ip', '192.0.2.20')").bind(bucket).execute(&mut *holder).await.unwrap();
        }
        // Each contender has an independent one-connection production pool.
        // The winner is invisible to their INSERT snapshots until holder commits.
        let barrier = Arc::new(Barrier::new(11));
        let mut tasks = JoinSet::new();
        for _ in 0..10 {
            let state = state(
                db.schema_database_url(),
                &[
                    ("FERL_DB_POOL_MAX", "1"),
                    ("FERL_ACCOUNT_CREATE_LIMIT", "5"),
                ],
            );
            let barrier = barrier.clone();
            let key = key.clone();
            tasks.spawn(async move {
                timeout(Duration::from_secs(5), barrier.wait())
                    .await
                    .unwrap();
                let result = prepare(
                    &state,
                    &parts("192.0.2.20", account.then_some(key.as_str())),
                )
                .await;
                close(&state.pool).await;
                result
            });
        }
        timeout(Duration::from_secs(5), barrier.wait())
            .await
            .unwrap();
        wait_for_query(db.pool(), "INSERT INTO rate_buckets%", 10).await;
        holder.commit().await.unwrap();
        let mut admitted = 0;
        while let Some(result) = timeout(Duration::from_secs(15), tasks.join_next())
            .await
            .unwrap()
        {
            match result.unwrap() {
                Ok(_) => admitted += 1,
                Err(CreationFailure::RateLimited(_)) => {}
                other => panic!("unexpected race result: {other:?}"),
            }
        }
        assert_eq!(admitted, 5);
        let rows: i64 =
            sqlx::query_scalar("SELECT count(*) FROM rate_charges WHERE bucket_id = $1")
                .bind(bucket)
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(rows, 5);
    }
    assert_eq!(count(db.pool()).await, 10);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM rate_buckets")
            .fetch_one(db.pool())
            .await
            .unwrap(),
        2
    );
    })).await;
}

#[tokio::test]
async fn committed_suspension_or_deletion_before_shared_revalidation_prevents_charge() {
    let db = migrated_database().await;
    db.verify(|db| {
        Box::pin(async move {
            let state = state(db.schema_database_url(), &[]);
            for deletion in [false, true] {
                let (id, key) = seed_account(
                    db.pool(),
                    if deletion {
                        "deleted_creator"
                    } else {
                        "suspended_creator"
                    },
                    "user",
                    true,
                )
                .await;
                let mut invalidation = begin_read_committed(db.pool()).await.unwrap();
                acquire_account_authority_exclusive(&mut invalidation)
                    .await
                    .unwrap();
                let service = state.creation.clone();
                let mut tasks = JoinSet::new();
                tasks.spawn(async move { service.prepare(&parts("192.0.2.21", Some(&key))).await });
                wait_for_query(db.pool(), "SELECT pg_advisory_xact_lock_shared%", 1).await;
                if deletion {
                    sqlx::query("DELETE FROM accounts WHERE id = $1")
                        .bind(id.as_uuid())
                        .execute(&mut *invalidation)
                        .await
                        .unwrap();
                } else {
                    sqlx::query("UPDATE accounts SET enabled = false WHERE id = $1")
                        .bind(id.as_uuid())
                        .execute(&mut *invalidation)
                        .await
                        .unwrap();
                }
                invalidation.commit().await.unwrap();
                assert_eq!(
                    timeout(Duration::from_secs(5), tasks.join_next())
                        .await
                        .unwrap()
                        .unwrap()
                        .unwrap()
                        .unwrap_err(),
                    CreationFailure::InvalidCredentials
                );
                assert_eq!(count(db.pool()).await, 0);
            }
            close(&state.pool).await;
        })
    })
    .await;
}

#[tokio::test]
async fn charge_first_holds_shared_authority_and_samples_time_after_bucket_wait() {
    let db = migrated_database().await;
    db.verify(|db| {
        Box::pin(async move {
            let state = state(db.schema_database_url(), &[]);
            let (id, key) = seed_account(db.pool(), "charge_first", "user", true).await;
            let initial = prepare(&state, &parts("192.0.2.22", Some(&key)))
                .await
                .unwrap();
            let principal = initial.creator().principal().unwrap().clone();
            let mut holder = begin_read_committed(db.pool()).await.unwrap();
            let bucket: Uuid =
                sqlx::query_scalar("SELECT id FROM rate_buckets WHERE account_id = $1 FOR UPDATE")
                    .bind(id.as_uuid())
                    .fetch_one(&mut *holder)
                    .await
                    .unwrap();
            let service = state.creation.clone();
            let mut charges = JoinSet::new();
            charges.spawn(async move { service.prepare(&parts("192.0.2.22", Some(&key))).await });
            wait_for_query(db.pool(), "SELECT id FROM rate_buckets%FOR UPDATE", 1).await;
            let pool = state.pool.clone();
            let mut invalidators = JoinSet::new();
            invalidators.spawn(async move {
                let mut guard =
                    auth::begin_account_write(&pool, &principal, Duration::from_secs(10))
                        .await
                        .unwrap();
                let count: i64 =
                    sqlx::query_scalar("SELECT count(*) FROM rate_charges WHERE bucket_id = $1")
                        .bind(bucket)
                        .fetch_one(guard.connection())
                        .await
                        .unwrap();
                assert_eq!(
                    count, 2,
                    "exclusive invalidation only proceeds after acknowledged rate transaction"
                );
                sqlx::query("UPDATE accounts SET enabled = false WHERE id = $1")
                    .bind(id.as_uuid())
                    .execute(guard.connection())
                    .await
                    .unwrap();
                guard.commit().await.unwrap();
            });
            wait_for_query(db.pool(), "SELECT pg_advisory_xact_lock(%", 1).await;
            // Database instant sampled while the contender is observably blocked.
            let release_time: String = sqlx::query_scalar("SELECT clock_timestamp()::text")
                .fetch_one(&mut *holder)
                .await
                .unwrap();
            holder.commit().await.unwrap();
            timeout(Duration::from_secs(5), charges.join_next())
                .await
                .unwrap()
                .unwrap()
                .unwrap()
                .unwrap();
            timeout(Duration::from_secs(5), invalidators.join_next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert!(
        sqlx::query_scalar::<_, bool>(
            "SELECT max(charged_at) >= $2::text::timestamptz FROM rate_charges WHERE bucket_id = $1"
        )
        .bind(bucket)
        .bind(release_time)
        .fetch_one(db.pool())
        .await
        .unwrap()
    );
            close(&state.pool).await;
        })
    })
    .await;
}

#[tokio::test]
async fn timeout_and_cancellation_after_insert_rollback_and_release_bucket() {
    let db = migrated_database().await;
    db.verify(|db| Box::pin(async move {
    sqlx::query("CREATE FUNCTION rate_test_hold() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_advisory_xact_lock(715051); RETURN NEW; END $$").execute(db.pool()).await.unwrap();
    sqlx::query("CREATE TRIGGER rate_test_hold AFTER INSERT ON rate_charges FOR EACH ROW EXECUTE FUNCTION rate_test_hold()").execute(db.pool()).await.unwrap();
    let state = state(
        db.schema_database_url(),
        &[
            ("FERL_DB_POOL_MAX", "1"),
            ("FERL_DB_STATEMENT_TIMEOUT_SECS", "2"),
            ("FERL_DB_LOCK_TIMEOUT_SECS", "2"),
            ("FERL_CREATION_TIMEOUT_SECS", "1"),
        ],
    );
    for cancellation in [false, true] {
        let mut holder = begin_read_committed(db.pool()).await.unwrap();
        sqlx::query("SELECT pg_advisory_xact_lock(715051)")
            .execute(&mut *holder)
            .await
            .unwrap();
        let service = state.creation.clone();
        let mut tasks = JoinSet::new();
        tasks.spawn(async move { service.prepare(&parts("192.0.2.23", None)).await });
        wait_for_query(db.pool(), "INSERT INTO rate_charges%", 1).await;
        if cancellation {
            tasks.abort_all();
        }
        let result = timeout(Duration::from_secs(3), tasks.join_next())
            .await
            .unwrap()
            .unwrap();
        if cancellation {
            assert!(result.unwrap_err().is_cancelled());
        } else {
            assert_eq!(
                result.unwrap().unwrap_err(),
                CreationFailure::StorageUnavailable
            );
        }
        holder.rollback().await.unwrap();
        // Reuse the SAME one connection after SQLx's queued rollback. This is
        // stronger than observing zero rows from a separate snapshot alone.
        timeout(Duration::from_secs(5), async {
            loop {
                let idle: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_stat_activity WHERE datname = current_database() AND pid <> pg_backend_pid() AND query LIKE 'INSERT INTO rate_charges%' AND state <> 'idle'").fetch_one(db.pool()).await.unwrap();
                if idle == 0 { break; } tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.unwrap();
        assert_eq!(count(db.pool()).await, 0);
    }
    sqlx::query("DROP TRIGGER rate_test_hold ON rate_charges")
        .execute(db.pool())
        .await
        .unwrap();
    prepare(&state, &parts("192.0.2.23", None)).await.unwrap();
    assert_eq!(count(db.pool()).await, 1);
    close(&state.pool).await;
    })).await;
}
