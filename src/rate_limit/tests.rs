//! Deterministic database-clock seam, compiled ONLY into the library test target.
//! The same repository SQL is exercised with a database-produced timestamp.
use super::{RateSubject, clock::DatabaseInstant, maintenance, repository};
use crate::{
    account_support::migrated_database, client_ip::CanonicalIp,
    db::coordination::begin_read_committed,
};
use sqlx::PgConnection;
use uuid::Uuid;

const INSTANT: &str = "2026-10-28T12:34:56.123456Z";

async fn seed(connection: &mut PgConnection, bucket: Uuid, offsets: &[&str]) {
    sqlx::query("DELETE FROM rate_charges WHERE bucket_id = $1")
        .bind(bucket)
        .execute(&mut *connection)
        .await
        .unwrap();
    for offset in offsets {
        sqlx::query("INSERT INTO rate_charges (id, bucket_id, charged_at) VALUES ($1, $2, $3::text::timestamptz + $4::text::interval)")
            .bind(Uuid::new_v4()).bind(bucket).bind(INSTANT).bind(offset).execute(&mut *connection).await.unwrap();
    }
}

#[tokio::test]
async fn exact_microsecond_window_rounding_equal_times_and_reduced_limits() {
    let db = migrated_database().await;
    db.verify(|db| {
        Box::pin(async move {
            let mut tx = begin_read_committed(db.pool()).await.unwrap();
            let bucket = repository::lock_bucket(
                &mut tx,
                RateSubject::Anonymous(CanonicalIp::new("192.0.2.1".parse().unwrap())),
            )
            .await
            .unwrap();
            let now: DatabaseInstant = sqlx::query_scalar("SELECT $1::text::timestamptz")
                .bind(INSTANT)
                .fetch_one(&mut *tx)
                .await
                .unwrap();
            let round_trip: bool = sqlx::query_scalar("SELECT $1 = $2::text::timestamptz")
                .bind(now)
                .bind(INSTANT)
                .fetch_one(&mut *tx)
                .await
                .unwrap();
            assert!(
                round_trip,
                "all six fractional digits survive PostgreSQL binary round trip"
            );
            for (offsets, limit, expected) in [
                (vec!["-60 seconds"], 1, None),
                (vec!["-60000001 microseconds"], 1, None),
                (vec!["-59999999 microseconds"], 1, Some(1)),
                (vec!["-58499999 microseconds"], 1, Some(2)),
                (vec!["0 seconds", "0 seconds", "0 seconds"], 3, Some(60)),
                (vec!["1 microsecond", "60 seconds"], 1, None),
                (
                    vec![
                        "-55 seconds",
                        "-50 seconds",
                        "-45 seconds",
                        "-40 seconds",
                        "-35 seconds",
                        "-30 seconds",
                        "-25 seconds",
                    ],
                    3,
                    Some(25),
                ),
            ] {
                seed(&mut tx, bucket, &offsets).await;
                let before: Vec<(Uuid, String)> = sqlx::query_as(
            "SELECT id, charged_at::text FROM rate_charges WHERE bucket_id = $1 ORDER BY id",
        )
        .bind(bucket)
        .fetch_all(&mut *tx)
        .await
        .unwrap();
                let result = repository::allowance(&mut tx, bucket, now, limit)
                    .await
                    .unwrap()
                    .map(|r| r.seconds());
                assert_eq!(result, expected, "boundary fixture {offsets:?}");
                let after: Vec<(Uuid, String)> = sqlx::query_as(
            "SELECT id, charged_at::text FROM rate_charges WHERE bucket_id = $1 ORDER BY id",
        )
        .bind(bucket)
        .fetch_all(&mut *tx)
        .await
        .unwrap();
                assert_eq!(before, after, "checks/rejections never refresh charges");
            }
            tx.rollback().await.unwrap();
        })
    })
    .await;
}

#[tokio::test]
async fn cleanup_is_bounded_cutoff_inclusive_and_keeps_live_future_and_bucket_rows() {
    let db = migrated_database().await;
    db.verify(|db| Box::pin(async move {
    let mut tx = begin_read_committed(db.pool()).await.unwrap();
    let subject = RateSubject::Anonymous(CanonicalIp::new("192.0.2.2".parse().unwrap()));
    let bucket = repository::lock_bucket(&mut tx, subject).await.unwrap();
    let now: DatabaseInstant = sqlx::query_scalar("SELECT $1::text::timestamptz")
        .bind(INSTANT)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    let mut offsets = vec!["-60 seconds"; 300];
    offsets.extend(["-59999999 microseconds", "0 seconds", "1 microsecond"]);
    seed(&mut tx, bucket, &offsets).await;
    maintenance::prune(&mut tx, bucket, now).await.unwrap();
    let counts: (i64, i64) = sqlx::query_as("SELECT count(*), count(*) FILTER (WHERE charged_at > $2 - interval '60 seconds') FROM rate_charges WHERE bucket_id = $1").bind(bucket).bind(now).fetch_one(&mut *tx).await.unwrap();
    assert_eq!(counts, (175, 3));
    tx.commit().await.unwrap();
    let mut tx = begin_read_committed(db.pool()).await.unwrap();
    assert_eq!(
        repository::lock_bucket(&mut tx, subject).await.unwrap(),
        bucket
    );
    maintenance::prune(&mut tx, bucket, now).await.unwrap();
    maintenance::prune(&mut tx, bucket, now).await.unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM rate_charges WHERE bucket_id = $1")
        .bind(bucket)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(count, 3);
    tx.commit().await.unwrap();
    })).await;
}

#[tokio::test]
async fn accounting_clock_handles_postgres_range_without_chrono_panics() {
    let db = migrated_database().await;
    db.verify(|db| {
        Box::pin(async move {
            for value in ["infinity", "-infinity"] {
                assert!(
                    sqlx::query_scalar::<_, DatabaseInstant>("SELECT $1::text::timestamptz")
                        .bind(value)
                        .fetch_one(db.pool())
                        .await
                        .is_err()
                );
            }
            for value in [
                "0001-01-01 00:00:00.000001+00",
                "294000-01-01 00:00:00.999999+00",
                INSTANT,
            ] {
                let instant: DatabaseInstant = sqlx::query_scalar("SELECT $1::text::timestamptz")
                    .bind(value)
                    .fetch_one(db.pool())
                    .await
                    .unwrap();
                assert!(
                    sqlx::query_scalar::<_, bool>("SELECT $1 = $2::text::timestamptz")
                        .bind(instant)
                        .bind(value)
                        .fetch_one(db.pool())
                        .await
                        .unwrap()
                );
            }
        })
    })
    .await;
}
