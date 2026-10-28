use std::fmt;

use sqlx::PgConnection;

use crate::domain::short_code::{RandomnessError, ShortCode};

use super::repository;

const MAX_GENERATED_CANDIDATES: usize = 10;

pub(crate) trait CandidateSource: Send + Sync {
    fn next_candidate(&self) -> Result<ShortCode, RandomnessError>;
}

pub(crate) struct OsCandidates;

impl CandidateSource for OsCandidates {
    fn next_candidate(&self) -> Result<ShortCode, RandomnessError> {
        ShortCode::generate()
    }
}

pub(crate) enum AllocationFailure {
    CodeGenerationUnavailable,
    Database(sqlx::Error),
}

pub(crate) async fn reserve_generated<C: CandidateSource + ?Sized>(
    connection: &mut PgConnection,
    candidates: &C,
) -> Result<ShortCode, AllocationFailure> {
    for _ in 0..MAX_GENERATED_CANDIDATES {
        let candidate = candidates
            .next_candidate()
            .map_err(|_| AllocationFailure::CodeGenerationUnavailable)?;
        if repository::reserve_code(connection, candidate.as_str())
            .await
            .map_err(AllocationFailure::Database)?
        {
            return Ok(candidate);
        }
    }
    Err(AllocationFailure::CodeGenerationUnavailable)
}

pub(crate) async fn reserve_custom(
    connection: &mut PgConnection,
    code: &ShortCode,
) -> Result<bool, sqlx::Error> {
    repository::reserve_code(connection, code.as_str()).await
}

impl fmt::Debug for AllocationFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::CodeGenerationUnavailable => "AllocationFailure::CodeGenerationUnavailable",
            Self::Database(_) => "AllocationFailure::Database([REDACTED])",
        })
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        sync::{
            Mutex,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use crate::domain::short_code::{RandomBytes, RandomnessError, ShortCode};

    use super::{CandidateSource, MAX_GENERATED_CANDIDATES};

    struct Codes {
        values: Mutex<VecDeque<Result<String, ()>>>,
        calls: AtomicUsize,
    }

    impl CandidateSource for Codes {
        fn next_candidate(&self) -> Result<ShortCode, RandomnessError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            self.values
                .lock()
                .expect("candidate fixture mutex is not poisoned")
                .pop_front()
                .unwrap_or_else(|| Ok("fallback01".to_owned()))
                .map_err(|_| RandomnessError)
                .and_then(|code| ShortCode::parse_custom(&code).map_err(|_| RandomnessError))
        }
    }

    struct Fails;
    impl RandomBytes for Fails {
        fn fill(&mut self, _: &mut [u8]) -> Result<(), RandomnessError> {
            Err(RandomnessError)
        }
    }

    #[test]
    fn candidate_source_is_private_and_sequence_failures_are_bounded() {
        let sequence = Codes {
            values: Mutex::new([Ok("first-candidate".into()), Err(())].into()),
            calls: AtomicUsize::new(0),
        };
        assert_eq!(
            sequence.next_candidate().unwrap().as_str(),
            "first-candidate"
        );
        assert!(sequence.next_candidate().is_err());
        assert_eq!(sequence.calls.load(Ordering::Relaxed), 2);

        let mut failing_random = Fails;
        assert!(ShortCode::generate_with(&mut failing_random).is_err());
        assert_eq!(MAX_GENERATED_CANDIDATES, 10);
    }
}
