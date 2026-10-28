use std::time::Duration;

use sqlx::PgConnection;
use uuid::Uuid;

use super::{
    RateSubject,
    clock::DatabaseInstant,
    service::{CreationFailure, RetryAfter, sql_failure},
};

pub(super) async fn configure(
    connection: &mut PgConnection,
    statement: Duration,
    lock: Duration,
) -> Result<(), CreationFailure> {
    // The authority helper left its lock deadline in statement_timeout. Restore
    // the independently configured statement bound, never disable it.
    sqlx::query(
        "SELECT set_config('statement_timeout', $1, true), set_config('lock_timeout', $2, true)",
    )
    .bind(statement.as_millis().max(1).to_string())
    .bind(lock.as_millis().max(1).to_string())
    .execute(connection)
    .await
    .map_err(sql_failure)?;
    Ok(())
}

pub(super) async fn lock_bucket(
    connection: &mut PgConnection,
    subject: RateSubject,
) -> Result<Uuid, CreationFailure> {
    let (kind, account, ip) = match subject {
        RateSubject::Account(id) => ("account", Some(id.as_uuid()), None),
        RateSubject::Anonymous(ip) => ("anonymous_ip", None, Some(ip.host_network())),
    };
    sqlx::query(
        "INSERT INTO rate_buckets (id, bucket_kind, account_id, client_ip) \
         VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING",
    )
    .bind(Uuid::new_v4())
    .bind(kind)
    .bind(account)
    .bind(ip)
    .execute(&mut *connection)
    .await
    .map_err(sql_failure)?;
    // A conflict winner may be invisible to the INSERT snapshot. Always use a
    // separate READ COMMITTED statement; never invent an alternate bucket.
    let id = match subject {
        RateSubject::Account(id) => {
            sqlx::query_scalar(
                "SELECT id FROM rate_buckets \
             WHERE bucket_kind = 'account' AND account_id = $1 FOR UPDATE",
            )
            .bind(id.as_uuid())
            .fetch_optional(connection)
            .await
        }
        RateSubject::Anonymous(ip) => {
            sqlx::query_scalar(
                "SELECT id FROM rate_buckets \
             WHERE bucket_kind = 'anonymous_ip' AND client_ip = $1 FOR UPDATE",
            )
            .bind(ip.host_network())
            .fetch_optional(connection)
            .await
        }
    }
    .map_err(sql_failure)?
    .ok_or(CreationFailure::Internal)?;
    Ok(id)
}

pub(super) async fn sample(
    connection: &mut PgConnection,
) -> Result<DatabaseInstant, CreationFailure> {
    // A statement AFTER the row lock, not transaction now() or an app clock.
    sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(connection)
        .await
        .map_err(sql_failure)
}

pub(super) async fn allowance(
    connection: &mut PgConnection,
    bucket: Uuid,
    now: DatabaseInstant,
    limit: u32,
) -> Result<Option<RetryAfter>, CreationFailure> {
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM rate_charges WHERE bucket_id = $1 \
         AND charged_at > $2 - interval '60 seconds' AND charged_at <= $2",
    )
    .bind(bucket)
    .bind(now)
    .fetch_one(&mut *connection)
    .await
    .map_err(sql_failure)?;
    if count < 0 {
        return Err(CreationFailure::Internal);
    }
    if count < i64::from(limit) {
        return Ok(None);
    }
    let offset = count
        .checked_sub(i64::from(limit))
        .ok_or(CreationFailure::Internal)?;
    // EXTRACT returns NUMERIC on PostgreSQL 17: exact interval microseconds,
    // rounded upwards only once. OFFSET n-L handles reduced startup limits.
    let seconds: i64 = sqlx::query_scalar(
        "SELECT greatest(1, ceil(extract(epoch FROM \
             (charged_at + interval '60 seconds' - $2))))::bigint \
         FROM rate_charges WHERE bucket_id = $1 \
         AND charged_at > $2 - interval '60 seconds' AND charged_at <= $2 \
         ORDER BY charged_at OFFSET $3 LIMIT 1",
    )
    .bind(bucket)
    .bind(now)
    .bind(offset)
    .fetch_one(connection)
    .await
    .map_err(sql_failure)?;
    Ok(Some(RetryAfter::from_database(seconds)?))
}

pub(super) async fn insert(
    connection: &mut PgConnection,
    bucket: Uuid,
    now: DatabaseInstant,
) -> Result<(), CreationFailure> {
    sqlx::query("INSERT INTO rate_charges (id, bucket_id, charged_at) VALUES ($1, $2, $3)")
        .bind(Uuid::new_v4())
        .bind(bucket)
        .bind(now)
        .execute(connection)
        .await
        .map_err(sql_failure)?;
    Ok(())
}
