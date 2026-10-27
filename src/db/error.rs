use sqlx::Error;

/// A stable database failure category. Raw driver errors and server text are never
/// carried into public errors or application diagnostics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DatabaseFailure {
    Conflict(RecognizedConflict),
    NotFound,
    Unavailable(TransientFailure),
    CommitOutcomeUnknown,
    Internal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecognizedConflict {
    Username,
    ShortCode,
    CodeReservation,
    AccountCredentialDigest,
    ManagementTokenDigest,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransientFailure {
    Connection,
    Pool,
    TimeoutOrLock,
    TransactionAborted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OperationStage {
    BeforeCommit,
    Commit,
}

impl DatabaseFailure {
    pub const fn safe_category(self) -> &'static str {
        match self {
            Self::Conflict(RecognizedConflict::Username) => "username_conflict",
            Self::Conflict(RecognizedConflict::ShortCode) => "short_code_conflict",
            Self::Conflict(RecognizedConflict::CodeReservation) => "code_reservation_conflict",
            Self::Conflict(RecognizedConflict::AccountCredentialDigest) => {
                "credential_digest_conflict"
            }
            Self::Conflict(RecognizedConflict::ManagementTokenDigest) => {
                "management_token_digest_conflict"
            }
            Self::NotFound => "record_absent",
            Self::Unavailable(TransientFailure::Connection) => "database_connection_unavailable",
            Self::Unavailable(TransientFailure::Pool) => "database_pool_unavailable",
            Self::Unavailable(TransientFailure::TimeoutOrLock) => "database_timeout_or_lock",
            Self::Unavailable(TransientFailure::TransactionAborted) => {
                "database_transaction_aborted"
            }
            Self::CommitOutcomeUnknown => "database_commit_outcome_unknown",
            Self::Internal => "database_internal_failure",
        }
    }
}

/// Classify PostgreSQL failures only from SQLSTATE and recognized constraint names.
/// An absent row is returned by repository code through [`not_found`], not inferred
/// from driver messages. Callers must never retry `CommitOutcomeUnknown`.
pub fn classify(error: &Error, stage: OperationStage) -> DatabaseFailure {
    if let Error::Database(database_error) = error {
        let state = database_error.code();
        if state.as_deref() == Some("23505") {
            return match database_error.constraint() {
                Some("accounts_username_unique") => {
                    DatabaseFailure::Conflict(RecognizedConflict::Username)
                }
                Some("links_short_code_unique") => {
                    DatabaseFailure::Conflict(RecognizedConflict::ShortCode)
                }
                Some("code_reservations_pkey") => {
                    DatabaseFailure::Conflict(RecognizedConflict::CodeReservation)
                }
                Some("accounts_api_key_digest_unique") => {
                    DatabaseFailure::Conflict(RecognizedConflict::AccountCredentialDigest)
                }
                Some("links_management_token_digest_unique") => {
                    DatabaseFailure::Conflict(RecognizedConflict::ManagementTokenDigest)
                }
                _ => DatabaseFailure::Internal,
            };
        }

        if let Some(state) = state.as_deref() {
            if state.starts_with("08") || matches!(state, "53300" | "57P01" | "57P03") {
                return transient(TransientFailure::Connection, stage);
            }
            if matches!(state, "55P03" | "57014") {
                return transient(TransientFailure::TimeoutOrLock, stage);
            }
            if matches!(state, "40001" | "40P01") {
                return transient(TransientFailure::TransactionAborted, stage);
            }
        }

        return DatabaseFailure::Internal;
    }

    match error {
        Error::PoolTimedOut => transient(TransientFailure::Pool, stage),
        Error::PoolClosed => transient(TransientFailure::Pool, stage),
        Error::Io(_) | Error::Tls(_) => transient(TransientFailure::Connection, stage),
        _ => DatabaseFailure::Internal,
    }
}

pub const fn not_found() -> DatabaseFailure {
    DatabaseFailure::NotFound
}

const fn transient(kind: TransientFailure, stage: OperationStage) -> DatabaseFailure {
    if matches!(stage, OperationStage::Commit) {
        DatabaseFailure::CommitOutcomeUnknown
    } else {
        DatabaseFailure::Unavailable(kind)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pool_timeouts_are_availability_failures_before_commit_and_ambiguous_at_commit() {
        assert_eq!(
            classify(&Error::PoolTimedOut, OperationStage::BeforeCommit),
            DatabaseFailure::Unavailable(TransientFailure::Pool)
        );
        assert_eq!(
            classify(&Error::PoolTimedOut, OperationStage::Commit),
            DatabaseFailure::CommitOutcomeUnknown
        );
    }

    #[test]
    fn safe_categories_do_not_include_driver_diagnostics() {
        assert_eq!(
            DatabaseFailure::Internal.safe_category(),
            "database_internal_failure"
        );
        assert_eq!(not_found(), DatabaseFailure::NotFound);
    }
}
