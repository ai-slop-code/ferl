//! Account SQL. Every transaction-taking helper receives the active connection
//! explicitly; no helper checks out a hidden pool connection.

use chrono::{DateTime, Utc};
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::{db::records::AccountRecord, domain::ids::AccountId};

pub async fn find_by_id(
    pool: &PgPool,
    id: AccountId,
) -> Result<Option<AccountRecord>, sqlx::Error> {
    sqlx::query_as::<_, AccountRecord>(
        "SELECT id, username, role, enabled, api_key_digest, created_at, updated_at \
         FROM accounts WHERE id = $1",
    )
    .bind(id.as_uuid())
    .fetch_optional(pool)
    .await
}

pub async fn find_by_id_for_update(
    connection: &mut PgConnection,
    id: AccountId,
) -> Result<Option<AccountRecord>, sqlx::Error> {
    sqlx::query_as::<_, AccountRecord>(
        "SELECT id, username, role, enabled, api_key_digest, created_at, updated_at \
         FROM accounts WHERE id = $1 FOR UPDATE",
    )
    .bind(id.as_uuid())
    .fetch_optional(&mut *connection)
    .await
}

pub async fn insert(
    connection: &mut PgConnection,
    id: Uuid,
    username: &str,
    role: &str,
    digest: &[u8; 32],
) -> Result<AccountRecord, sqlx::Error> {
    sqlx::query_as::<_, AccountRecord>(
        "INSERT INTO accounts (id, username, role, enabled, api_key_digest) \
         VALUES ($1, $2, $3, true, $4) \
         RETURNING id, username, role, enabled, api_key_digest, created_at, updated_at",
    )
    .bind(id)
    .bind(username)
    .bind(role)
    .bind(digest.as_slice())
    .fetch_one(&mut *connection)
    .await
}

pub async fn lock_accounts_in_uuid_order(
    connection: &mut PgConnection,
    ids: &[Uuid],
) -> Result<Vec<Uuid>, sqlx::Error> {
    sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM accounts WHERE id = ANY($1) ORDER BY id FOR UPDATE",
    )
    .bind(ids.to_vec())
    .fetch_all(&mut *connection)
    .await
}

pub async fn enabled_admin_count(connection: &mut PgConnection) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT count(*) FROM accounts WHERE role = 'admin' AND enabled")
        .fetch_one(&mut *connection)
        .await
}

pub async fn update_state(
    connection: &mut PgConnection,
    id: Uuid,
    role: &str,
    enabled: bool,
) -> Result<AccountRecord, sqlx::Error> {
    sqlx::query_as::<_, AccountRecord>(
        "UPDATE accounts SET role = $2, enabled = $3, updated_at = clock_timestamp() \
         WHERE id = $1 \
         RETURNING id, username, role, enabled, api_key_digest, created_at, updated_at",
    )
    .bind(id)
    .bind(role)
    .bind(enabled)
    .fetch_one(&mut *connection)
    .await
}

pub async fn replace_digest(
    connection: &mut PgConnection,
    id: Uuid,
    digest: &[u8; 32],
) -> Result<AccountRecord, sqlx::Error> {
    sqlx::query_as::<_, AccountRecord>(
        "UPDATE accounts SET api_key_digest = $2, updated_at = clock_timestamp() \
         WHERE id = $1 \
         RETURNING id, username, role, enabled, api_key_digest, created_at, updated_at",
    )
    .bind(id)
    .bind(digest.as_slice())
    .fetch_one(&mut *connection)
    .await
}

pub async fn delete(connection: &mut PgConnection, id: Uuid) -> Result<u64, sqlx::Error> {
    Ok(sqlx::query("DELETE FROM accounts WHERE id = $1")
        .bind(id)
        .execute(&mut *connection)
        .await?
        .rows_affected())
}

pub async fn list_after(
    connection: &mut PgConnection,
    boundary: Option<(DateTime<Utc>, Uuid)>,
    limit_plus_one: i64,
) -> Result<Vec<AccountRecord>, sqlx::Error> {
    match boundary {
        Some((created_at, id)) => {
            sqlx::query_as::<_, AccountRecord>(
                "SELECT id, username, role, enabled, api_key_digest, created_at, updated_at \
                 FROM accounts WHERE (created_at, id) < ($1, $2) \
                 ORDER BY created_at DESC, id DESC LIMIT $3",
            )
            .bind(created_at)
            .bind(id)
            .bind(limit_plus_one)
            .fetch_all(&mut *connection)
            .await
        }
        None => {
            sqlx::query_as::<_, AccountRecord>(
                "SELECT id, username, role, enabled, api_key_digest, created_at, updated_at \
                 FROM accounts ORDER BY created_at DESC, id DESC LIMIT $1",
            )
            .bind(limit_plus_one)
            .fetch_all(&mut *connection)
            .await
        }
    }
}
