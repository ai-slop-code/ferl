use std::{fmt, time::Duration};

use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    auth::{GuardFailure, Principal, begin_account_write},
    db::{
        error::{DatabaseFailure, OperationStage, RecognizedConflict, classify},
        records::AccountRecord,
    },
    domain::{ids::AccountId, secret::AccountApiKey, time::Timestamp},
    http::{
        dto::{
            AccountCreationDto, AccountDto, AccountRole, ApiKeyReplacementDto, CollectionDto,
            PatchAccountRequest, Presence, RecordConversionError,
        },
        query::AccountListQuery,
    },
    pagination::{CursorBoundary, CursorCodec, CursorContext, CursorError},
};

use super::repository;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AccountFailure {
    AuthenticationRequired,
    InvalidCredentials,
    Forbidden,
    NotFound,
    InvalidRequest,
    UsernameUnavailable,
    LastAdminRequired,
    StorageUnavailable,
    Internal,
}

pub async fn current_account(pool: &PgPool, id: AccountId) -> Result<AccountDto, AccountFailure> {
    let record = repository::find_by_id(pool, id)
        .await
        .map_err(map_sql)?
        .ok_or(AccountFailure::InvalidCredentials)?;
    account_dto(&record)
}

pub async fn get_account(pool: &PgPool, id: AccountId) -> Result<AccountDto, AccountFailure> {
    let record = repository::find_by_id(pool, id)
        .await
        .map_err(map_sql)?
        .ok_or(AccountFailure::NotFound)?;
    account_dto(&record)
}

pub async fn list_accounts(
    pool: &PgPool,
    cursor_codec: &CursorCodec,
    actor: AccountId,
    query: &AccountListQuery,
) -> Result<CollectionDto<AccountDto>, AccountFailure> {
    let context = CursorContext::accounts(actor, query);
    let boundary = match query.page().cursor() {
        None => None,
        Some(token) => match cursor_codec.decode(&context, token) {
            Ok(CursorBoundary::AccountCreatedAt { at, id }) => Some((at.as_utc(), id.as_uuid())),
            Ok(_) | Err(CursorError::InvalidCursor) => return Err(AccountFailure::InvalidRequest),
            Err(CursorError::RandomnessUnavailable | CursorError::CapacityExceeded) => {
                return Err(AccountFailure::Internal);
            }
        },
    };
    let limit = i64::from(query.page().limit());
    let mut connection = pool.acquire().await.map_err(map_sql)?;
    let mut records = repository::list_after(&mut connection, boundary, limit + 1)
        .await
        .map_err(map_sql)?;
    let has_more = records.len() > limit as usize;
    records.truncate(limit as usize);
    let next_cursor = if has_more {
        let last = records.last().ok_or(AccountFailure::Internal)?;
        let at = Timestamp::from_database(last.created_at).map_err(|_| AccountFailure::Internal)?;
        Some(
            cursor_codec
                .encode(
                    &context,
                    &CursorBoundary::AccountCreatedAt {
                        at,
                        id: AccountId::new(last.id),
                    },
                )
                .map_err(|_| AccountFailure::Internal)?,
        )
    } else {
        None
    };
    let items = records
        .iter()
        .map(account_dto)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(CollectionDto::new(items, next_cursor))
}

pub async fn create_account(
    pool: &PgPool,
    principal: &Principal,
    lock_timeout: Duration,
    username: &str,
    role: AccountRole,
) -> Result<AccountCreationDto, AccountFailure> {
    let secret = AccountApiKey::generate().map_err(|_| AccountFailure::Internal)?;
    let digest = secret.digest();
    let mut guard = begin_account_write(pool, principal, lock_timeout)
        .await
        .map_err(map_guard)?;
    guard.require_admin().map_err(map_guard)?;
    let record = match repository::insert(
        guard.connection(),
        Uuid::new_v4(),
        username,
        role.as_database_str(),
        digest.as_bytes(),
    )
    .await
    {
        Ok(record) => record,
        Err(error) => {
            let failure = map_sql(error);
            rollback(guard).await;
            return Err(failure);
        }
    };
    let account = match account_dto(&record) {
        Ok(account) => account,
        Err(failure) => {
            rollback(guard).await;
            return Err(failure);
        }
    };
    guard.commit().await.map_err(map_guard)?;
    Ok(AccountCreationDto::new(account, secret))
}

pub async fn update_account(
    pool: &PgPool,
    principal: &Principal,
    lock_timeout: Duration,
    id: AccountId,
    patch: PatchAccountRequest,
) -> Result<AccountDto, AccountFailure> {
    let mut guard = begin_account_write(pool, principal, lock_timeout)
        .await
        .map_err(map_guard)?;
    guard.require_admin().map_err(map_guard)?;
    let current = match repository::find_by_id_for_update(guard.connection(), id).await {
        Ok(Some(record)) => record,
        Ok(None) => {
            rollback(guard).await;
            return Err(AccountFailure::NotFound);
        }
        Err(error) => {
            let failure = map_sql(error);
            rollback(guard).await;
            return Err(failure);
        }
    };
    let desired_role = match patch.role() {
        Presence::Omitted => current.role.as_str(),
        Presence::Value(role) => role.as_database_str(),
        Presence::Null => return rollback_error(guard, AccountFailure::InvalidRequest).await,
    };
    let desired_enabled = match patch.enabled() {
        Presence::Omitted => current.enabled,
        Presence::Value(enabled) => *enabled,
        Presence::Null => return rollback_error(guard, AccountFailure::InvalidRequest).await,
    };
    let changes = desired_role != current.role || desired_enabled != current.enabled;
    if changes
        && current.role == "admin"
        && current.enabled
        && (desired_role != "admin" || !desired_enabled)
    {
        match repository::enabled_admin_count(guard.connection()).await {
            Ok(1) => return rollback_error(guard, AccountFailure::LastAdminRequired).await,
            Ok(_) => {}
            Err(error) => {
                return rollback_error(guard, map_sql(error)).await;
            }
        }
    }
    let record = if changes {
        match repository::update_state(
            guard.connection(),
            id.as_uuid(),
            desired_role,
            desired_enabled,
        )
        .await
        {
            Ok(record) => record,
            Err(error) => return rollback_error(guard, map_sql(error)).await,
        }
    } else {
        current
    };
    let account = match account_dto(&record) {
        Ok(account) => account,
        Err(failure) => return rollback_error(guard, failure).await,
    };
    guard.commit().await.map_err(map_guard)?;
    Ok(account)
}

pub async fn delete_account(
    pool: &PgPool,
    principal: &Principal,
    lock_timeout: Duration,
    id: AccountId,
) -> Result<(), AccountFailure> {
    let mut guard = begin_account_write(pool, principal, lock_timeout)
        .await
        .map_err(map_guard)?;
    guard.require_admin().map_err(map_guard)?;
    let current = match repository::find_by_id_for_update(guard.connection(), id).await {
        Ok(Some(record)) => record,
        Ok(None) => return rollback_error(guard, AccountFailure::NotFound).await,
        Err(error) => return rollback_error(guard, map_sql(error)).await,
    };
    if current.role == "admin" && current.enabled {
        match repository::enabled_admin_count(guard.connection()).await {
            Ok(1) => return rollback_error(guard, AccountFailure::LastAdminRequired).await,
            Ok(_) => {}
            Err(error) => return rollback_error(guard, map_sql(error)).await,
        }
    }
    match repository::delete(guard.connection(), id.as_uuid()).await {
        Ok(1) => {}
        Ok(_) => return rollback_error(guard, AccountFailure::NotFound).await,
        Err(error) => return rollback_error(guard, map_sql(error)).await,
    }
    guard.commit().await.map_err(map_guard)
}

pub async fn rotate_current_key(
    pool: &PgPool,
    principal: &Principal,
    lock_timeout: Duration,
) -> Result<ApiKeyReplacementDto, AccountFailure> {
    let secret = AccountApiKey::generate().map_err(|_| AccountFailure::Internal)?;
    let mut guard = begin_account_write(pool, principal, lock_timeout)
        .await
        .map_err(map_guard)?;
    let id = guard.caller().id();
    match repository::replace_digest(guard.connection(), id.as_uuid(), secret.digest().as_bytes())
        .await
    {
        Ok(_) => {}
        Err(error) => return rollback_error(guard, map_sql(error)).await,
    }
    guard.commit().await.map_err(map_guard)?;
    Ok(ApiKeyReplacementDto::new(secret))
}

pub async fn reset_account_key(
    pool: &PgPool,
    principal: &Principal,
    lock_timeout: Duration,
    target: AccountId,
) -> Result<ApiKeyReplacementDto, AccountFailure> {
    let secret = AccountApiKey::generate().map_err(|_| AccountFailure::Internal)?;
    let mut guard = begin_account_write(pool, principal, lock_timeout)
        .await
        .map_err(map_guard)?;
    guard.require_admin().map_err(map_guard)?;
    let ids = [guard.caller().id().as_uuid(), target.as_uuid()];
    let locked = match repository::lock_accounts_in_uuid_order(guard.connection(), &ids).await {
        Ok(locked) => locked,
        Err(error) => return rollback_error(guard, map_sql(error)).await,
    };
    if !locked.contains(&target.as_uuid()) {
        return rollback_error(guard, AccountFailure::NotFound).await;
    }
    match repository::replace_digest(
        guard.connection(),
        target.as_uuid(),
        secret.digest().as_bytes(),
    )
    .await
    {
        Ok(_) => {}
        Err(error) => return rollback_error(guard, map_sql(error)).await,
    }
    guard.commit().await.map_err(map_guard)?;
    Ok(ApiKeyReplacementDto::new(secret))
}

fn account_dto(record: &AccountRecord) -> Result<AccountDto, AccountFailure> {
    AccountDto::from_record(record).map_err(|_: RecordConversionError| AccountFailure::Internal)
}

async fn rollback(guard: crate::auth::AccountMutationGuard) {
    if guard.rollback().await.is_err() {
        tracing::warn!(
            category = "account_transaction_rollback_failed",
            "account transaction rollback failed"
        );
    }
}

async fn rollback_error<T>(
    guard: crate::auth::AccountMutationGuard,
    error: AccountFailure,
) -> Result<T, AccountFailure> {
    rollback(guard).await;
    Err(error)
}

fn map_guard(error: GuardFailure) -> AccountFailure {
    match error {
        GuardFailure::InvalidCredentials => AccountFailure::InvalidCredentials,
        GuardFailure::Forbidden => AccountFailure::Forbidden,
        GuardFailure::NotFound => AccountFailure::NotFound,
        GuardFailure::DeadlineExceeded => AccountFailure::StorageUnavailable,
        GuardFailure::Internal => AccountFailure::Internal,
        GuardFailure::Database(failure) => map_database(failure),
    }
}

fn map_sql(error: sqlx::Error) -> AccountFailure {
    map_database(classify(&error, OperationStage::BeforeCommit))
}

fn map_database(failure: DatabaseFailure) -> AccountFailure {
    match failure {
        DatabaseFailure::Conflict(RecognizedConflict::Username) => {
            AccountFailure::UsernameUnavailable
        }
        DatabaseFailure::Unavailable(_) | DatabaseFailure::CommitOutcomeUnknown => {
            AccountFailure::StorageUnavailable
        }
        DatabaseFailure::Conflict(_) | DatabaseFailure::NotFound | DatabaseFailure::Internal => {
            AccountFailure::Internal
        }
    }
}

impl AccountFailure {
    pub const fn safe_category(self) -> &'static str {
        match self {
            Self::AuthenticationRequired => "authentication_required",
            Self::InvalidCredentials => "invalid_credentials",
            Self::Forbidden => "forbidden",
            Self::NotFound => "record_absent",
            Self::InvalidRequest => "invalid_request",
            Self::UsernameUnavailable => "username_conflict",
            Self::LastAdminRequired => "last_enabled_admin_required",
            Self::StorageUnavailable => "database_unavailable_or_commit_uncertain",
            Self::Internal => "account_persistence_failure",
        }
    }
}

impl fmt::Debug for AccountFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AccountFailure")
    }
}
