use std::{fmt, str::FromStr, time::Duration};

use sqlx::{PgPool, Row, postgres::PgPoolOptions};
use tokio::time::timeout;
use uuid::Uuid;

use crate::{
    config::BootstrapConfig,
    db::{
        coordination::{self, CoordinationError},
        error::{DatabaseFailure, OperationStage, RecognizedConflict, classify},
        migrate::{self, VerificationError},
    },
    domain::{secret::AccountApiKey, text::Username},
    http::dto::{AccountCreationDto, AccountDto},
};

use super::repository;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BootstrapFailure {
    InvalidUsername,
    RandomnessUnavailable,
    AlreadyCompleted,
    UsernameUnavailable,
    SchemaIncompatible,
    StorageUnavailable,
    Internal,
}

/// Create exactly one initial enabled administrator. The returned DTO contains
/// the plaintext credential and must only be serialized to the operator result
/// stream after this function reports an acknowledged commit.
pub async fn run(
    config: &BootstrapConfig,
    username: &str,
) -> Result<AccountCreationDto, BootstrapFailure> {
    let username = Username::parse(username).map_err(|_| BootstrapFailure::InvalidUsername)?;
    let secret = AccountApiKey::generate().map_err(|_| BootstrapFailure::RandomnessUnavailable)?;
    timeout(config.total_timeout, run_inner(config, &username, secret))
        .await
        .unwrap_or(Err(BootstrapFailure::StorageUnavailable))
}

async fn run_inner(
    config: &BootstrapConfig,
    username: &Username,
    secret: AccountApiKey,
) -> Result<AccountCreationDto, BootstrapFailure> {
    let options =
        sqlx::postgres::PgConnectOptions::from_str(config.database_url.expose_to_driver())
            .map_err(|_| BootstrapFailure::Internal)?;
    let acquire_timeout = config.connect_timeout.min(Duration::from_secs(30));
    let statement_timeout = config.statement_timeout.as_millis().to_string();
    let lock_timeout = config.lock_timeout.as_millis().to_string();
    let pool: PgPool = PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(acquire_timeout)
        .after_connect(move |connection, _metadata| {
            let statement_timeout = statement_timeout.clone();
            let lock_timeout = lock_timeout.clone();
            Box::pin(async move {
                sqlx::query(
                    "SELECT set_config('statement_timeout', $1, false), \
                            set_config('lock_timeout', $2, false)",
                )
                .bind(statement_timeout)
                .bind(lock_timeout)
                .execute(connection)
                .await?;
                Ok(())
            })
        })
        .connect_with(options)
        .await
        .map_err(map_sql)?;

    match timeout(
        config.statement_timeout,
        migrate::verify_compatibility(&pool),
    )
    .await
    {
        Ok(Ok(())) => {}
        Ok(Err(VerificationError::DatabaseUnavailable | VerificationError::DeadlineExceeded)) => {
            pool.close().await;
            return Err(BootstrapFailure::StorageUnavailable);
        }
        Ok(Err(_)) | Err(_) => {
            pool.close().await;
            return Err(BootstrapFailure::SchemaIncompatible);
        }
    }

    let mut transaction = coordination::begin_read_committed(&pool)
        .await
        .map_err(map_coordination)?;
    if let Err(error) = coordination::acquire_account_authority_exclusive_bounded(
        &mut transaction,
        config.lock_timeout,
    )
    .await
    {
        rollback(transaction).await;
        pool.close().await;
        return Err(map_coordination(error));
    }
    let state = match sqlx::query(
        "SELECT bootstrap_completed FROM system_state WHERE singleton = true FOR UPDATE",
    )
    .fetch_optional(&mut *transaction)
    .await
    {
        Ok(Some(row)) => match row.try_get::<bool, _>("bootstrap_completed") {
            Ok(completed) => completed,
            Err(_) => {
                rollback(transaction).await;
                pool.close().await;
                return Err(BootstrapFailure::Internal);
            }
        },
        Ok(None) => {
            rollback(transaction).await;
            pool.close().await;
            return Err(BootstrapFailure::SchemaIncompatible);
        }
        Err(error) => {
            let failure = map_sql(error);
            rollback(transaction).await;
            pool.close().await;
            return Err(failure);
        }
    };
    if state {
        rollback(transaction).await;
        pool.close().await;
        return Err(BootstrapFailure::AlreadyCompleted);
    }

    let digest = secret.digest();
    let record = match repository::insert(
        &mut transaction,
        Uuid::new_v4(),
        username.as_str(),
        "admin",
        digest.as_bytes(),
    )
    .await
    {
        Ok(record) => record,
        Err(error) => {
            let failure = map_sql(error);
            rollback(transaction).await;
            pool.close().await;
            return Err(failure);
        }
    };
    if let Err(error) =
        sqlx::query("UPDATE system_state SET bootstrap_completed = true WHERE singleton = true")
            .execute(&mut *transaction)
            .await
    {
        let failure = map_sql(error);
        rollback(transaction).await;
        pool.close().await;
        return Err(failure);
    }
    let account = match AccountDto::from_record(&record) {
        Ok(account) => account,
        Err(_) => {
            rollback(transaction).await;
            pool.close().await;
            return Err(BootstrapFailure::Internal);
        }
    };
    if let Err(error) = transaction.commit().await {
        // A failed commit acknowledgement is ambiguous. Never retry and never
        // emit the generated key; operators must inspect durable state.
        let failure = map_sql_at_commit(error);
        pool.close().await;
        return Err(failure);
    }
    pool.close().await;
    Ok(AccountCreationDto::new(account, secret))
}

async fn rollback(transaction: sqlx::Transaction<'_, sqlx::Postgres>) {
    if transaction.rollback().await.is_err() {
        tracing::warn!(
            category = "bootstrap_rollback_failed",
            "bootstrap rollback failed"
        );
    }
}

fn map_coordination(error: CoordinationError) -> BootstrapFailure {
    match error {
        CoordinationError::DeadlineExceeded => BootstrapFailure::StorageUnavailable,
        CoordinationError::Database(failure) => map_database(failure),
    }
}

fn map_sql(error: sqlx::Error) -> BootstrapFailure {
    map_database(classify(&error, OperationStage::BeforeCommit))
}

fn map_sql_at_commit(error: sqlx::Error) -> BootstrapFailure {
    map_database(classify(&error, OperationStage::Commit))
}

fn map_database(failure: DatabaseFailure) -> BootstrapFailure {
    match failure {
        DatabaseFailure::Conflict(RecognizedConflict::Username) => {
            BootstrapFailure::UsernameUnavailable
        }
        DatabaseFailure::Unavailable(_) | DatabaseFailure::CommitOutcomeUnknown => {
            BootstrapFailure::StorageUnavailable
        }
        DatabaseFailure::Conflict(_) | DatabaseFailure::NotFound | DatabaseFailure::Internal => {
            BootstrapFailure::Internal
        }
    }
}

impl BootstrapFailure {
    pub const fn safe_category(self) -> &'static str {
        match self {
            Self::InvalidUsername => "bootstrap_username_invalid",
            Self::RandomnessUnavailable => "bootstrap_randomness_unavailable",
            Self::AlreadyCompleted => "bootstrap_already_completed",
            Self::UsernameUnavailable => "bootstrap_username_conflict",
            Self::SchemaIncompatible => "bootstrap_schema_incompatible",
            Self::StorageUnavailable => "bootstrap_database_unavailable_or_commit_uncertain",
            Self::Internal => "bootstrap_persistence_failure",
        }
    }
}

impl fmt::Debug for BootstrapFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("BootstrapFailure")
    }
}
