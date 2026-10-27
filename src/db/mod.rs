pub mod coordination;
pub mod error;
pub mod migrate;
pub mod pool;
pub mod records;

use std::time::Duration;

use sqlx::{PgPool, Row};
use tokio::time::timeout;

use crate::app::ReadinessState;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProbeResult {
    Ready,
    Unavailable,
}

pub async fn check_readiness(
    pool: &PgPool,
    state: &ReadinessState,
    acquire_timeout: Duration,
) -> ProbeResult {
    if !state.is_accepting_work() || !state.is_schema_compatible() {
        return ProbeResult::Unavailable;
    }

    let result = timeout(acquire_timeout, async {
        sqlx::query("SELECT 1 AS ferl_probe")
            .fetch_one(pool)
            .await
            .map(|row| row.try_get::<i32, _>("ferl_probe").ok() == Some(1))
    })
    .await;

    match result {
        Ok(Ok(true)) => final_state_after_successful_probe(state),
        _ => final_state_after_failed_probe(state),
    }
}

fn final_state_after_successful_probe(state: &ReadinessState) -> ProbeResult {
    if state.is_accepting_work() && state.is_schema_compatible() {
        ProbeResult::Ready
    } else {
        ProbeResult::Unavailable
    }
}

fn final_state_after_failed_probe(state: &ReadinessState) -> ProbeResult {
    // A failed DB probe invalidates readiness until the background verifier
    // establishes compatibility again after database recovery.
    state.mark_schema_incompatible();
    ProbeResult::Unavailable
}

#[cfg(test)]
mod tests {
    use super::{ProbeResult, final_state_after_failed_probe, final_state_after_successful_probe};
    use crate::app::ReadinessState;

    #[test]
    fn a_completed_probe_cannot_restore_readiness_after_drain_begins() {
        let state = ReadinessState::new_accepting();
        assert!(state.mark_schema_compatible_if_current(state.schema_revision()));
        state.begin_draining();
        assert_eq!(
            final_state_after_successful_probe(&state),
            ProbeResult::Unavailable
        );
    }

    #[test]
    fn a_failed_database_probe_invalidates_the_schema_lease_until_reverification() {
        let state = ReadinessState::new_accepting();
        assert!(state.mark_schema_compatible_if_current(state.schema_revision()));
        assert_eq!(
            final_state_after_failed_probe(&state),
            ProbeResult::Unavailable
        );
        assert!(!state.is_schema_compatible());
    }
}
