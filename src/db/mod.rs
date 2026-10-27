pub mod migrate;
pub mod pool;

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
        Ok(Ok(true)) => ProbeResult::Ready,
        _ => ProbeResult::Unavailable,
    }
}
