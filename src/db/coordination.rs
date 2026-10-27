//! Database-wide lock ordering for protected mutations.
//!
//! The authority gate uses the deployment-wide PostgreSQL advisory key
//! `(0x4645524c, 0x41434354)` (`FERL` / `ACCT`). Account-authenticated link
//! mutations take it shared; every account write, including bootstrap and
//! credential replacement, takes it exclusive. The lock order is: authority
//! gate → account rows in UUID order → link rows in UUID order → child rows.
//! Rate-bucket coordination is in a separate short transaction and is never
//! held while creating a link.
//!
//! Guarded mutations use READ COMMITTED. Acquire the potentially waiting gate
//! in one SQL statement, then re-read the credential/role/state in a separate
//! statement. Under READ COMMITTED that second statement receives a fresh
//! snapshot after any lock wait; do not combine the wait and revalidation in a
//! SELECT/CTE or use a transaction-wide snapshot.

use std::time::Duration;

use sqlx::{PgPool, Postgres, Transaction};
use tokio::time::timeout;

use super::error::{DatabaseFailure, OperationStage, TransientFailure, classify};

/// PostgreSQL's two-int advisory-lock key reserved for account authority.
pub const ACCOUNT_AUTHORITY_LOCK_KEY: (i32, i32) = (0x4645_524c, 0x4143_4354);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoordinationError {
    Database(DatabaseFailure),
    DeadlineExceeded,
}

impl CoordinationError {
    pub const fn safe_category(self) -> &'static str {
        match self {
            Self::Database(failure) => failure.safe_category(),
            Self::DeadlineExceeded => "account_authority_lock_deadline",
        }
    }
}

/// Start an explicit READ COMMITTED transaction for a guarded mutation.
pub async fn begin_read_committed(
    pool: &PgPool,
) -> Result<Transaction<'static, Postgres>, CoordinationError> {
    let mut transaction = pool.begin().await.map_err(|error| {
        CoordinationError::Database(classify(&error, OperationStage::BeforeCommit))
    })?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL READ COMMITTED")
        .execute(&mut *transaction)
        .await
        .map_err(|error| {
            CoordinationError::Database(classify(&error, OperationStage::BeforeCommit))
        })?;
    Ok(transaction)
}

/// Acquire the shared account-authority gate on the caller's transaction.
/// This function does not check out a connection or read authorization state.
pub async fn acquire_account_authority_shared(
    transaction: &mut Transaction<'_, Postgres>,
) -> Result<(), CoordinationError> {
    sqlx::query("SELECT pg_advisory_xact_lock_shared($1, $2)")
        .bind(ACCOUNT_AUTHORITY_LOCK_KEY.0)
        .bind(ACCOUNT_AUTHORITY_LOCK_KEY.1)
        .execute(&mut **transaction)
        .await
        .map(|_| ())
        .map_err(|error| {
            CoordinationError::Database(classify(&error, OperationStage::BeforeCommit))
        })
}

/// Acquire the exclusive account-authority gate on the caller's transaction.
/// Use for every account write, including bootstrap and changes to credentials,
/// roles, enabled state, or account existence.
pub async fn acquire_account_authority_exclusive(
    transaction: &mut Transaction<'_, Postgres>,
) -> Result<(), CoordinationError> {
    sqlx::query("SELECT pg_advisory_xact_lock($1, $2)")
        .bind(ACCOUNT_AUTHORITY_LOCK_KEY.0)
        .bind(ACCOUNT_AUTHORITY_LOCK_KEY.1)
        .execute(&mut **transaction)
        .await
        .map(|_| ())
        .map_err(|error| {
            CoordinationError::Database(classify(&error, OperationStage::BeforeCommit))
        })
}

/// Bound a shared-gate wait without changing its PostgreSQL transaction scope.
/// If the deadline expires, the caller must roll back/drop the transaction and
/// must not continue with any authorization-dependent mutation.
pub async fn acquire_account_authority_shared_bounded(
    transaction: &mut Transaction<'_, Postgres>,
    deadline: Duration,
) -> Result<(), CoordinationError> {
    set_local_lock_statement_deadline(transaction, deadline).await?;
    match timeout(
        deadline + Duration::from_secs(1),
        acquire_account_authority_shared(transaction),
    )
    .await
    {
        Ok(Err(CoordinationError::Database(DatabaseFailure::Unavailable(
            TransientFailure::TimeoutOrLock,
        )))) => Err(CoordinationError::DeadlineExceeded),
        Ok(result) => result,
        Err(_) => Err(CoordinationError::DeadlineExceeded),
    }
}

/// Bound an exclusive-gate wait. On timeout, roll back/drop the transaction;
/// never treat a cancelled lock query as permission to proceed.
pub async fn acquire_account_authority_exclusive_bounded(
    transaction: &mut Transaction<'_, Postgres>,
    deadline: Duration,
) -> Result<(), CoordinationError> {
    set_local_lock_statement_deadline(transaction, deadline).await?;
    match timeout(
        deadline + Duration::from_secs(1),
        acquire_account_authority_exclusive(transaction),
    )
    .await
    {
        Ok(Err(CoordinationError::Database(DatabaseFailure::Unavailable(
            TransientFailure::TimeoutOrLock,
        )))) => Err(CoordinationError::DeadlineExceeded),
        Ok(result) => result,
        Err(_) => Err(CoordinationError::DeadlineExceeded),
    }
}

async fn set_local_lock_statement_deadline(
    transaction: &mut Transaction<'_, Postgres>,
    deadline: Duration,
) -> Result<(), CoordinationError> {
    let millis = deadline.as_millis().max(1).to_string();
    sqlx::query("SELECT set_config('statement_timeout', $1, true)")
        .bind(millis)
        .execute(&mut **transaction)
        .await
        .map(|_| ())
        .map_err(|error| {
            CoordinationError::Database(classify(&error, OperationStage::BeforeCommit))
        })
}
