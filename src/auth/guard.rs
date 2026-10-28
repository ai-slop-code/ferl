use std::{fmt, time::Duration};

use sqlx::{PgPool, Postgres, Row, Transaction};

use crate::{
    db::{
        coordination::{self, CoordinationError},
        error::{DatabaseFailure, OperationStage, classify},
    },
    domain::{
        ids::{AccountId, LinkId},
        secret::SecretDigest,
    },
    telemetry::metrics::{DbOperation, DbTimer},
};

use super::{AccountPrincipal, AccountRole, ManagementPrincipal, Principal};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum GuardFailure {
    InvalidCredentials,
    Forbidden,
    NotFound,
    Database(DatabaseFailure),
    DeadlineExceeded,
    Internal,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LinkAuthorityFailure {
    NotFound,
    Database(DatabaseFailure),
    Internal,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum OwnerState {
    Anonymous,
    Enabled,
    Disabled,
    Missing,
}

/// Owns the transaction and the fresh caller state established after its gate
/// was acquired. The caller must commit or roll back; dropping rolls back.
pub struct AccountMutationGuard {
    transaction: Transaction<'static, Postgres>,
    caller: AccountPrincipal,
}

/// A live anonymous link row held `FOR UPDATE` with its management digest
/// revalidated under the row lock until this guard is committed/dropped.
pub struct LinkMutationGuard {
    transaction: Transaction<'static, Postgres>,
    token: ManagementPrincipal,
}

impl AccountMutationGuard {
    pub fn caller(&self) -> &AccountPrincipal {
        &self.caller
    }

    pub fn require_admin(&self) -> Result<(), GuardFailure> {
        if self.caller.role() == AccountRole::Admin {
            Ok(())
        } else {
            Err(GuardFailure::Forbidden)
        }
    }

    pub fn connection(&mut self) -> &mut sqlx::PgConnection {
        &mut self.transaction
    }

    pub async fn commit(self) -> Result<(), GuardFailure> {
        self.transaction
            .commit()
            .await
            .map_err(|error| GuardFailure::Database(classify(&error, OperationStage::Commit)))
    }

    pub async fn rollback(self) -> Result<(), GuardFailure> {
        self.transaction
            .rollback()
            .await
            .map_err(|error| GuardFailure::Database(classify(&error, OperationStage::BeforeCommit)))
    }
}

impl LinkMutationGuard {
    pub fn link_id(&self) -> LinkId {
        self.token.link_id()
    }

    pub fn connection(&mut self) -> &mut sqlx::PgConnection {
        &mut self.transaction
    }

    pub async fn commit(self) -> Result<(), GuardFailure> {
        self.transaction
            .commit()
            .await
            .map_err(|error| GuardFailure::Database(classify(&error, OperationStage::Commit)))
    }

    pub async fn rollback(self) -> Result<(), GuardFailure> {
        self.transaction
            .rollback()
            .await
            .map_err(|error| GuardFailure::Database(classify(&error, OperationStage::BeforeCommit)))
    }
}

/// Begin an account-authenticated link mutation under the shared authority
/// gate. Fresh credential/status/role lookup occurs as a later SQL statement.
#[tracing::instrument(skip_all, name = "db.account.shared_authority_guard")]
pub async fn begin_account_mutation(
    pool: &PgPool,
    principal: &Principal,
    lock_timeout: Duration,
) -> Result<AccountMutationGuard, GuardFailure> {
    let account = principal.account().ok_or(GuardFailure::Forbidden)?;
    let mut timer = DbTimer::new(DbOperation::AccountMutation);
    let mut transaction = begin(pool).await?;
    if let Err(error) =
        coordination::acquire_account_authority_shared_bounded(&mut transaction, lock_timeout).await
    {
        return rollback_after_error(transaction, map_coordination(error)).await;
    }
    let caller = match revalidate_account(&mut transaction, account.digest()).await {
        Ok(Some(caller)) => caller,
        Ok(None) => {
            timer.success();
            return rollback_after_error(transaction, GuardFailure::InvalidCredentials).await;
        }
        Err(error) => return rollback_after_error(transaction, error).await,
    };
    timer.success();
    Ok(AccountMutationGuard {
        transaction,
        caller,
    })
}

/// Begin an account write under the exclusive gate, then revalidate the
/// presented credential and current enabled state/role in a fresh statement.
#[tracing::instrument(skip_all, name = "db.account.exclusive_authority_guard")]
pub async fn begin_account_write(
    pool: &PgPool,
    principal: &Principal,
    lock_timeout: Duration,
) -> Result<AccountMutationGuard, GuardFailure> {
    let account = principal.account().ok_or(GuardFailure::Forbidden)?;
    let mut timer = DbTimer::new(DbOperation::AccountMutation);
    let mut transaction = begin(pool).await?;
    if let Err(error) =
        coordination::acquire_account_authority_exclusive_bounded(&mut transaction, lock_timeout)
            .await
    {
        return rollback_after_error(transaction, map_coordination(error)).await;
    }
    let caller = match revalidate_account(&mut transaction, account.digest()).await {
        Ok(Some(caller)) => caller,
        Ok(None) => {
            timer.success();
            return rollback_after_error(transaction, GuardFailure::InvalidCredentials).await;
        }
        Err(error) => return rollback_after_error(transaction, error).await,
    };
    timer.success();
    Ok(AccountMutationGuard {
        transaction,
        caller,
    })
}

/// Lock and revalidate the presented anonymous credential's live link row.
/// Asking to mutate another link remains a concealed 404 once this token has
/// proved valid; the lock is held through the caller's transaction.
#[tracing::instrument(skip_all, name = "db.link.anonymous_authority_guard")]
pub async fn begin_anonymous_link_mutation(
    pool: &PgPool,
    principal: &Principal,
    requested_link: LinkId,
    lock_timeout: Duration,
) -> Result<LinkMutationGuard, GuardFailure> {
    let token = principal
        .management_token()
        .ok_or(GuardFailure::Forbidden)?;
    let mut timer = DbTimer::new(DbOperation::LinkMutation);
    let mut transaction = begin(pool).await?;
    set_transaction_lock_deadline(&mut transaction, lock_timeout).await?;
    let row = sqlx::query(
        "SELECT management_token_digest FROM links \
         WHERE id = $1 AND owner_id IS NULL FOR UPDATE",
    )
    .bind(token.link_id().as_uuid())
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|error| GuardFailure::Database(classify(&error, OperationStage::BeforeCommit)));
    let row = match row {
        Ok(Some(row)) => row,
        Ok(None) => {
            timer.success();
            return rollback_after_error(transaction, GuardFailure::InvalidCredentials).await;
        }
        Err(error) => return rollback_after_error(transaction, error).await,
    };
    let stored: Vec<u8> = match row.try_get("management_token_digest") {
        Ok(stored) => stored,
        Err(_) => return rollback_after_error(transaction, GuardFailure::Internal).await,
    };
    let Some(stored) = SecretDigest::from_database_bytes(&stored) else {
        return rollback_after_error(transaction, GuardFailure::Internal).await;
    };
    if &stored != token.digest() {
        timer.success();
        return rollback_after_error(transaction, GuardFailure::InvalidCredentials).await;
    }
    if requested_link != token.link_id() {
        timer.success();
        return rollback_after_error(transaction, GuardFailure::NotFound).await;
    }
    timer.success();
    Ok(LinkMutationGuard {
        transaction,
        token: ManagementPrincipal::from_database(token.link_id(), token.digest().clone()),
    })
}

/// Establish object-level authority without selecting or exposing destination,
/// token digest, owner details, or child/analytics data to the caller.
pub async fn authorize_link_access(
    pool: &PgPool,
    principal: &Principal,
    link_id: LinkId,
    deadline: Duration,
) -> Result<(), LinkAuthorityFailure> {
    let row = tokio::time::timeout(deadline, async {
        sqlx::query(
            "SELECT l.id, l.owner_id, l.management_token_digest, a.enabled AS owner_enabled \
             FROM links l LEFT JOIN accounts a ON a.id = l.owner_id WHERE l.id = $1",
        )
        .bind(link_id.as_uuid())
        .fetch_optional(pool)
        .await
    })
    .await
    .map_err(|_| {
        LinkAuthorityFailure::Database(DatabaseFailure::Unavailable(
            crate::db::error::TransientFailure::TimeoutOrLock,
        ))
    })?
    .map_err(|error| {
        LinkAuthorityFailure::Database(classify(&error, OperationStage::BeforeCommit))
    })?;
    let Some(row) = row else {
        return Err(LinkAuthorityFailure::NotFound);
    };
    let owner_id: Option<uuid::Uuid> = row
        .try_get("owner_id")
        .map_err(|_| LinkAuthorityFailure::Internal)?;
    let token_digest: Option<Vec<u8>> = row
        .try_get("management_token_digest")
        .map_err(|_| LinkAuthorityFailure::Internal)?;
    let owner_enabled: Option<bool> = row
        .try_get("owner_enabled")
        .map_err(|_| LinkAuthorityFailure::Internal)?;
    let owner_state = match (owner_id, token_digest.as_ref(), owner_enabled) {
        (None, Some(digest), None) => {
            if SecretDigest::from_database_bytes(digest).is_none() {
                return Err(LinkAuthorityFailure::Internal);
            }
            OwnerState::Anonymous
        }
        (Some(_), None, Some(true)) => OwnerState::Enabled,
        (Some(_), None, Some(false)) => OwnerState::Disabled,
        // A live owned link with no owner row violates the FK invariant. It is
        // persistence corruption, not an enabled owner.
        (Some(_), None, None) => return Err(LinkAuthorityFailure::Internal),
        _ => return Err(LinkAuthorityFailure::Internal),
    };
    let authorized = match principal {
        Principal::Account(account) if account.role() == AccountRole::Admin => true,
        Principal::Account(account) => {
            owner_state == OwnerState::Enabled && owner_id == Some(account.id().as_uuid())
        }
        Principal::ManagementToken(token) => {
            if owner_state != OwnerState::Anonymous || token.link_id() != link_id {
                false
            } else {
                token_digest.as_deref() == Some(token.digest().as_bytes().as_slice())
            }
        }
    };
    if authorized {
        Ok(())
    } else {
        Err(LinkAuthorityFailure::NotFound)
    }
}

pub(crate) async fn revalidate_account(
    transaction: &mut Transaction<'_, Postgres>,
    digest: &SecretDigest,
) -> Result<Option<AccountPrincipal>, GuardFailure> {
    let row = sqlx::query(
        "SELECT id, role, enabled, api_key_digest FROM accounts WHERE api_key_digest = $1",
    )
    .bind(digest.as_bytes().as_slice())
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|error| GuardFailure::Database(classify(&error, OperationStage::BeforeCommit)))?;
    let Some(row) = row else {
        return Ok(None);
    };
    let id = row.try_get("id").map_err(|_| GuardFailure::Internal)?;
    let role: String = row.try_get("role").map_err(|_| GuardFailure::Internal)?;
    let enabled: bool = row.try_get("enabled").map_err(|_| GuardFailure::Internal)?;
    let stored_digest: Vec<u8> = row
        .try_get("api_key_digest")
        .map_err(|_| GuardFailure::Internal)?;
    if SecretDigest::from_database_bytes(&stored_digest).is_none() {
        return Err(GuardFailure::Internal);
    }
    if !enabled {
        return Ok(None);
    }
    let role = AccountRole::parse_database(&role).ok_or(GuardFailure::Internal)?;
    Ok(Some(AccountPrincipal::from_database(
        AccountId::new(id),
        role,
        digest.clone(),
    )))
}

async fn begin(pool: &PgPool) -> Result<Transaction<'static, Postgres>, GuardFailure> {
    coordination::begin_read_committed(pool)
        .await
        .map_err(map_coordination)
}

async fn set_transaction_lock_deadline(
    transaction: &mut Transaction<'_, Postgres>,
    deadline: Duration,
) -> Result<(), GuardFailure> {
    let millis = deadline.as_millis().max(1).to_string();
    sqlx::query(
        "SELECT set_config('statement_timeout', $1, true), \
                set_config('lock_timeout', $1, true)",
    )
    .bind(millis)
    .execute(&mut **transaction)
    .await
    .map(|_| ())
    .map_err(|error| GuardFailure::Database(classify(&error, OperationStage::BeforeCommit)))
}

fn map_coordination(error: CoordinationError) -> GuardFailure {
    match error {
        CoordinationError::Database(failure) => GuardFailure::Database(failure),
        CoordinationError::DeadlineExceeded => GuardFailure::DeadlineExceeded,
    }
}

async fn rollback_after_error<T>(
    transaction: Transaction<'static, Postgres>,
    error: GuardFailure,
) -> Result<T, GuardFailure> {
    if transaction.rollback().await.is_err() {
        tracing::warn!(
            category = "authority_guard_rollback_failed",
            "guard rollback failed"
        );
    }
    Err(error)
}

macro_rules! safe_debug {
    ($name:ty, $text:literal) => {
        impl fmt::Debug for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str($text)
            }
        }
    };
}

safe_debug!(GuardFailure, "GuardFailure");
safe_debug!(LinkAuthorityFailure, "LinkAuthorityFailure");
safe_debug!(OwnerState, "OwnerState");
safe_debug!(AccountMutationGuard, "AccountMutationGuard([TRANSACTION])");
safe_debug!(LinkMutationGuard, "LinkMutationGuard([TRANSACTION])");
