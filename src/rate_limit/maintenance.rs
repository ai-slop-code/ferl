use sqlx::PgConnection;
use uuid::Uuid;

use super::{
    clock::DatabaseInstant,
    service::{CreationFailure, sql_failure},
};

/// Opportunistic cleanup only inside the owning bucket transaction. Stable
/// buckets are never removed. One admission does at most 128 expired deletes.
pub(super) async fn prune(
    connection: &mut PgConnection,
    bucket: Uuid,
    now: DatabaseInstant,
) -> Result<(), CreationFailure> {
    sqlx::query(
        "DELETE FROM rate_charges WHERE id IN (\
             SELECT id FROM rate_charges WHERE bucket_id = $1 \
             AND charged_at <= $2 - interval '60 seconds' ORDER BY charged_at LIMIT 128\
         )",
    )
    .bind(bucket)
    .bind(now)
    .execute(connection)
    .await
    .map_err(|error| {
        tracing::warn!(
            category = "rate_cleanup_failed",
            "bounded allowance cleanup failed"
        );
        sql_failure(error)
    })?;
    Ok(())
}
