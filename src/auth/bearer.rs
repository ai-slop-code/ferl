use std::{fmt, time::Duration};

use http::HeaderMap;
use sqlx::{PgPool, Row};
use tokio::time::timeout;

use crate::{
    db::error::{DatabaseFailure, OperationStage, classify},
    domain::{
        ids::{AccountId, LinkId},
        secret::{CredentialClass, CredentialHeader, SecretDigest},
    },
    telemetry::metrics::{self, AuthFailureCategory, DbOperation, DbTimer},
};

use super::{AccountPrincipal, AccountRole, ManagementPrincipal, Principal};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AuthFailure {
    Missing,
    Invalid,
    StorageUnavailable,
    Internal,
}

/// Parse the one Authorization field and resolve its complete-secret digest on
/// every call. No role, enabled state, token, or principal is cached.
#[tracing::instrument(skip_all, name = "credential.authenticate")]
pub async fn authenticate(
    pool: &PgPool,
    headers: &HeaderMap,
    deadline: Duration,
) -> Result<Option<Principal>, AuthFailure> {
    let credential = match CredentialHeader::parse(headers) {
        CredentialHeader::Missing => return Ok(None),
        CredentialHeader::SuppliedInvalid => {
            metrics::record_auth_failure(AuthFailureCategory::InvalidCredential);
            return Err(AuthFailure::Invalid);
        }
        CredentialHeader::Parsed(credential) => credential,
    };
    let digest = credential.digest().clone();
    let mut db_timer = DbTimer::new(DbOperation::CredentialLookup);
    let query = async {
        match credential.class() {
            CredentialClass::AccountKey => lookup_account(pool, &digest).await,
            CredentialClass::ManagementToken => lookup_management_token(pool, &digest).await,
        }
    };
    let result = match timeout(deadline, query).await {
        Ok(result) => result,
        Err(_) => Err(AuthFailure::StorageUnavailable),
    };
    match result {
        Ok(principal) => {
            db_timer.success();
            Ok(principal)
        }
        Err(failure) => {
            if matches!(failure, AuthFailure::Invalid) {
                // A successful lookup that found no matching credential is an
                // authentication rejection, not a database availability fault.
                db_timer.success();
            } else {
                db_timer.failure();
            }
            metrics::record_auth_failure(match failure {
                AuthFailure::Invalid => AuthFailureCategory::InvalidCredential,
                AuthFailure::StorageUnavailable => AuthFailureCategory::StorageUnavailable,
                AuthFailure::Internal => AuthFailureCategory::Internal,
                AuthFailure::Missing => AuthFailureCategory::MissingCredential,
            });
            Err(failure)
        }
    }
}

async fn lookup_account(
    pool: &PgPool,
    digest: &SecretDigest,
) -> Result<Option<Principal>, AuthFailure> {
    let row = sqlx::query(
        "SELECT id, role, enabled, api_key_digest FROM accounts WHERE api_key_digest = $1",
    )
    .bind(digest.as_bytes().as_slice())
    .fetch_optional(pool)
    .await
    .map_err(classify_read)?;
    let Some(row) = row else {
        return Err(AuthFailure::Invalid);
    };
    let id = row.try_get("id").map_err(|_| AuthFailure::Internal)?;
    let role: String = row.try_get("role").map_err(|_| AuthFailure::Internal)?;
    let enabled: bool = row.try_get("enabled").map_err(|_| AuthFailure::Internal)?;
    let stored_digest: Vec<u8> = row
        .try_get("api_key_digest")
        .map_err(|_| AuthFailure::Internal)?;
    if SecretDigest::from_database_bytes(&stored_digest).is_none() {
        return Err(AuthFailure::Internal);
    }
    if !enabled {
        return Err(AuthFailure::Invalid);
    }
    let role = AccountRole::parse_database(&role).ok_or(AuthFailure::Internal)?;
    Ok(Some(Principal::Account(AccountPrincipal::from_database(
        AccountId::new(id),
        role,
        digest.clone(),
    ))))
}

async fn lookup_management_token(
    pool: &PgPool,
    digest: &SecretDigest,
) -> Result<Option<Principal>, AuthFailure> {
    let row = sqlx::query(
        "SELECT id, management_token_digest FROM links \
         WHERE owner_id IS NULL AND management_token_digest = $1",
    )
    .bind(digest.as_bytes().as_slice())
    .fetch_optional(pool)
    .await
    .map_err(classify_read)?;
    let Some(row) = row else {
        return Err(AuthFailure::Invalid);
    };
    let id = row.try_get("id").map_err(|_| AuthFailure::Internal)?;
    let stored_digest: Vec<u8> = row
        .try_get("management_token_digest")
        .map_err(|_| AuthFailure::Internal)?;
    if SecretDigest::from_database_bytes(&stored_digest).is_none() {
        return Err(AuthFailure::Internal);
    }
    Ok(Some(Principal::ManagementToken(
        ManagementPrincipal::from_database(LinkId::new(id), digest.clone()),
    )))
}

fn classify_read(error: sqlx::Error) -> AuthFailure {
    match classify(&error, OperationStage::BeforeCommit) {
        DatabaseFailure::Unavailable(_) => AuthFailure::StorageUnavailable,
        DatabaseFailure::Internal
        | DatabaseFailure::Conflict(_)
        | DatabaseFailure::NotFound
        | DatabaseFailure::CommitOutcomeUnknown => AuthFailure::Internal,
    }
}

impl fmt::Debug for AuthFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Missing => "AuthFailure::Missing",
            Self::Invalid => "AuthFailure::Invalid",
            Self::StorageUnavailable => "AuthFailure::StorageUnavailable",
            Self::Internal => "AuthFailure::Internal",
        })
    }
}
