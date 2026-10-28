use std::{fmt, time::Duration};

use http::request::Parts;
use sqlx::{PgConnection, PgPool, Postgres, Transaction};

use crate::{
    auth::{self, AccountMutationGuard, AuthFailure, GuardFailure, Principal},
    client_ip::ClientIpResolver,
    config::Config,
    db::{
        coordination::{self, CoordinationError},
        error::{DatabaseFailure, OperationStage, classify},
    },
    telemetry::metrics,
    telemetry::metrics::{DbOperation, DbTimer},
};

use super::{RateSubject, maintenance, repository};

const ROLLBACK_DEADLINE: Duration = Duration::from_secs(2);

#[derive(Clone)]
pub struct CreationService {
    pool: PgPool,
    resolver: ClientIpResolver,
    anonymous_enabled: bool,
    anonymous_limit: u32,
    account_limit: u32,
    authentication_timeout: Duration,
    statement_timeout: Duration,
    lock_timeout: Duration,
    total_timeout: Duration,
}

/// A successful independently committed charge belonging to this one attempt.
/// Part 06 must consume it; there is no Clone, serialization, or ID constructor.
///
/// ```compile_fail
/// use ferl_server::rate_limit::ChargedAttempt;
/// fn duplicate(charge: ChargedAttempt) { let _ = charge.clone(); }
/// ```
/// ```compile_fail
/// use ferl_server::rate_limit::ChargedAttempt;
/// let charge = ChargedAttempt { creator: () };
/// ```
/// ```compile_fail
/// use ferl_server::rate_limit::ChargedAttempt;
/// fn expose(charge: ChargedAttempt) { let _ = serde_json::to_string(&charge); }
/// ```
pub struct ChargedAttempt {
    creator: Creator,
}

/// Retains the presented authentication evidence for a NEW Part 06 transaction.
/// UUID identity alone must never replace its later guard revalidation.
pub struct Creator {
    subject: RateSubject,
    principal: Option<Principal>,
}

impl ChargedAttempt {
    pub fn creator(&self) -> &Creator {
        &self.creator
    }
    pub fn into_creator(self) -> Creator {
        self.creator
    }
}

impl Creator {
    pub const fn subject(&self) -> RateSubject {
        self.subject
    }
    pub fn principal(&self) -> Option<&Principal> {
        self.principal.as_ref()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetryAfter(u32);

impl RetryAfter {
    pub const fn seconds(self) -> u32 {
        self.0
    }
    pub(super) fn from_database(seconds: i64) -> Result<Self, CreationFailure> {
        if !(1..=60).contains(&seconds) {
            return Err(CreationFailure::Internal);
        }
        Ok(Self(
            u32::try_from(seconds).map_err(|_| CreationFailure::Internal)?,
        ))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CreationFailure {
    InvalidCredentials,
    AnonymousCreationDisabled,
    Forbidden,
    RateLimited(RetryAfter),
    StorageUnavailable,
    CommitOutcomeUnknown,
    Internal,
}

impl CreationService {
    pub(crate) fn from_config(pool: PgPool, config: &Config) -> Self {
        Self {
            pool,
            resolver: ClientIpResolver::new(&config.trusted_proxies),
            anonymous_enabled: config.anonymous_creation,
            anonymous_limit: config.anonymous_create_limit,
            account_limit: config.account_create_limit,
            authentication_timeout: config.database.acquire_timeout,
            statement_timeout: config.database.statement_timeout,
            lock_timeout: config.database.lock_timeout,
            total_timeout: config.creation_timeout,
        }
    }

    /// Call only after route/method selection, BEFORE reading any body frames.
    /// This future owns its transaction: cancellation drops it (SQLx queues a
    /// rollback); server statement/lock bounds remain active until it releases.
    /// No automatic retries, especially after an uncertain commit.
    pub async fn prepare(&self, parts: &Parts) -> Result<ChargedAttempt, CreationFailure> {
        let mut observation = AttemptObservation { finished: false };
        let mut committing = false;
        let result = tokio::time::timeout(
            self.total_timeout,
            self.prepare_inner(parts, &mut committing),
        )
        .await
        .unwrap_or({
            Err(if committing {
                CreationFailure::CommitOutcomeUnknown
            } else {
                CreationFailure::StorageUnavailable
            })
        });
        observation.finished = true;
        if matches!(&result, Err(CreationFailure::RateLimited(_))) {
            metrics::record_rate_rejection();
        }
        let outcome = match &result {
            Ok(_) => "charged",
            Err(error) => error.safe_category(),
        };
        tracing::info!(
            operation = "creation.allowance",
            outcome,
            "creation allowance completed"
        );
        result
    }

    async fn prepare_inner(
        &self,
        parts: &Parts,
        committing: &mut bool,
    ) -> Result<ChargedAttempt, CreationFailure> {
        let principal = auth::authenticate(&self.pool, &parts.headers, self.authentication_timeout)
            .await
            .map_err(|failure| match failure {
                AuthFailure::Invalid | AuthFailure::Missing => CreationFailure::InvalidCredentials,
                AuthFailure::StorageUnavailable => CreationFailure::StorageUnavailable,
                AuthFailure::Internal => CreationFailure::Internal,
            })?;
        let (mut transaction, creator, limit) = match principal {
            Some(principal) => {
                if principal.account().is_none() {
                    return Err(CreationFailure::Forbidden);
                }
                let guard = auth::begin_account_mutation(&self.pool, &principal, self.lock_timeout)
                    .await
                    .map_err(guard_failure)?;
                let subject = RateSubject::Account(guard.caller().id());
                let fresh = Principal::Account(guard.caller().clone());
                (
                    ChargeTransaction::Account(guard),
                    Creator {
                        subject,
                        principal: Some(fresh),
                    },
                    self.account_limit,
                )
            }
            None => {
                if !self.anonymous_enabled {
                    return Err(CreationFailure::AnonymousCreationDisabled);
                }
                let ip = self
                    .resolver
                    .resolve_request(parts)
                    .map_err(|_| CreationFailure::Internal)?;
                let transaction = coordination::begin_read_committed(&self.pool)
                    .await
                    .map_err(|error| match error {
                        CoordinationError::Database(error) => database_failure(error),
                        CoordinationError::DeadlineExceeded => CreationFailure::StorageUnavailable,
                    })?;
                (
                    ChargeTransaction::Anonymous(transaction),
                    Creator {
                        subject: RateSubject::Anonymous(ip),
                        principal: None,
                    },
                    self.anonymous_limit,
                )
            }
        };
        let result = self
            .charge(transaction.connection(), creator.subject, limit)
            .await;
        match result {
            Ok(()) => {
                // A deadline DURING commit is uncertain, never success/replay.
                *committing = true;
                transaction.commit().await?;
                Ok(ChargedAttempt { creator })
            }
            Err(error) => {
                if !matches!(
                    tokio::time::timeout(ROLLBACK_DEADLINE, transaction.rollback()).await,
                    Ok(Ok(()))
                ) {
                    tracing::warn!(
                        category = "rate_rollback_failed",
                        "allowance rollback exceeded its bound or failed"
                    );
                    return Err(CreationFailure::StorageUnavailable);
                }
                Err(error)
            }
        }
    }

    async fn charge(
        &self,
        connection: &mut PgConnection,
        subject: RateSubject,
        limit: u32,
    ) -> Result<(), CreationFailure> {
        let mut timer = DbTimer::new(DbOperation::RateCharge);
        let result = async {
            repository::configure(connection, self.statement_timeout, self.lock_timeout).await?;
            let bucket = repository::lock_bucket(connection, subject).await?;
            let now = repository::sample(connection).await?;
            if let Some(retry) = repository::allowance(connection, bucket, now, limit).await? {
                return Err(CreationFailure::RateLimited(retry));
            }
            maintenance::prune(connection, bucket, now).await?;
            repository::insert(connection, bucket, now).await
        }
        .await;
        if result.is_ok() || matches!(&result, Err(CreationFailure::RateLimited(_))) {
            timer.success();
        } else {
            timer.failure();
        }
        result
    }
}

// The variant makes authority lifetime and SHARED guard use explicit. Every
// repository function receives this connection; no nested pool acquisition.
enum ChargeTransaction {
    Account(AccountMutationGuard),
    Anonymous(Transaction<'static, Postgres>),
}
impl ChargeTransaction {
    fn connection(&mut self) -> &mut PgConnection {
        match self {
            Self::Account(guard) => guard.connection(),
            Self::Anonymous(tx) => tx,
        }
    }
    async fn commit(self) -> Result<(), CreationFailure> {
        match self {
            Self::Account(guard) => guard.commit().await.map_err(guard_failure),
            Self::Anonymous(tx) => tx
                .commit()
                .await
                .map_err(|error| database_failure(classify(&error, OperationStage::Commit))),
        }
    }
    async fn rollback(self) -> Result<(), CreationFailure> {
        match self {
            Self::Account(guard) => guard.rollback().await.map_err(guard_failure),
            Self::Anonymous(tx) => tx.rollback().await.map_err(sql_failure),
        }
    }
}

pub(super) fn sql_failure(error: sqlx::Error) -> CreationFailure {
    database_failure(classify(&error, OperationStage::BeforeCommit))
}
fn database_failure(error: DatabaseFailure) -> CreationFailure {
    match error {
        DatabaseFailure::Unavailable(_) => CreationFailure::StorageUnavailable,
        DatabaseFailure::CommitOutcomeUnknown => CreationFailure::CommitOutcomeUnknown,
        _ => CreationFailure::Internal,
    }
}
fn guard_failure(error: GuardFailure) -> CreationFailure {
    match error {
        GuardFailure::InvalidCredentials => CreationFailure::InvalidCredentials,
        GuardFailure::Forbidden => CreationFailure::Forbidden,
        GuardFailure::Database(error) => database_failure(error),
        GuardFailure::DeadlineExceeded => CreationFailure::StorageUnavailable,
        _ => CreationFailure::Internal,
    }
}
impl CreationFailure {
    pub const fn safe_category(self) -> &'static str {
        match self {
            Self::InvalidCredentials => "invalid_credentials",
            Self::AnonymousCreationDisabled => "anonymous_creation_disabled",
            Self::Forbidden => "forbidden",
            Self::RateLimited(_) => "rate_limited",
            Self::StorageUnavailable => "rate_storage_unavailable",
            Self::CommitOutcomeUnknown => "rate_commit_outcome_unknown",
            Self::Internal => "creation_internal_failure",
        }
    }
}
impl fmt::Debug for ChargedAttempt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ChargedAttempt([COMMITTED])")
    }
}
struct AttemptObservation {
    finished: bool,
}
impl Drop for AttemptObservation {
    fn drop(&mut self) {
        if !self.finished {
            tracing::info!(
                operation = "creation.allowance",
                outcome = "cancelled",
                "creation allowance cancelled; commit may be uncertain"
            );
        }
    }
}
