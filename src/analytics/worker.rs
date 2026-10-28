use std::{sync::Arc, time::Duration};

use sqlx::PgPool;
use tokio::{
    sync::Mutex,
    task::JoinSet,
    time::{Instant, timeout, timeout_at},
};
use tokio_util::sync::CancellationToken;

use crate::{config::AnalyticsConfig, telemetry::metrics};

use super::{
    event::VisitEvent,
    queue::VisitQueue,
    repository::{self, PersistOutcome},
    user_agent::{self, ParseOutcome},
};

const MAX_BACKOFF: Duration = Duration::from_millis(1_000);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerStartError {
    AlreadyStarted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerStopOutcome {
    Drained,
    DeadlineExceeded,
    WorkerFailed,
}

pub struct AnalyticsWorkers {
    tasks: JoinSet<()>,
    cancellation: CancellationToken,
    queue: VisitQueue,
}

impl AnalyticsWorkers {
    pub(super) fn new(
        tasks: JoinSet<()>,
        cancellation: CancellationToken,
        queue: VisitQueue,
    ) -> Self {
        Self {
            tasks,
            cancellation,
            queue,
        }
    }

    /// Stop accepting events and let workers persist the already-buffered
    /// queue. A deadline aborts remaining work; buffered events are best effort.
    pub async fn shutdown(mut self, deadline: Duration) -> WorkerStopOutcome {
        self.queue.stop_accepting();
        self.cancellation.cancel();
        let deadline_at = Instant::now() + deadline;
        let join_all = async {
            let mut failed = false;
            while let Some(result) = self.tasks.join_next().await {
                if result.is_err() {
                    failed = true;
                }
            }
            failed
        };
        match timeout_at(deadline_at, join_all).await {
            Ok(false) => WorkerStopOutcome::Drained,
            Ok(true) => WorkerStopOutcome::WorkerFailed,
            Err(_) => {
                self.tasks.abort_all();
                let remaining = deadline_at.saturating_duration_since(Instant::now());
                if !remaining.is_zero() {
                    let _ = timeout(remaining, async {
                        while self.tasks.join_next().await.is_some() {}
                    })
                    .await;
                }
                tracing::warn!(
                    category = "analytics_drain_deadline",
                    "analytics worker drain exceeded its deadline; remaining buffered visits were dropped"
                );
                WorkerStopOutcome::DeadlineExceeded
            }
        }
    }
}

impl Drop for AnalyticsWorkers {
    fn drop(&mut self) {
        self.queue.stop_accepting();
        self.cancellation.cancel();
        self.tasks.abort_all();
    }
}

pub(super) async fn worker_loop(
    pool: PgPool,
    receiver: Arc<Mutex<tokio::sync::mpsc::Receiver<VisitEvent>>>,
    cancellation: CancellationToken,
    config: AnalyticsConfig,
) {
    loop {
        let next_event = {
            let mut receiver = receiver.lock().await;
            tokio::select! {
                biased;
                _ = cancellation.cancelled() => None,
                event = receiver.recv() => event,
            }
        };
        if let Some(event) = next_event {
            process_event(&pool, &event, config).await;
            continue;
        }
        if cancellation.is_cancelled() {
            drain_queue(&pool, &receiver, config).await;
        }
        return;
    }
}

async fn drain_queue(
    pool: &PgPool,
    receiver: &Arc<Mutex<tokio::sync::mpsc::Receiver<VisitEvent>>>,
    config: AnalyticsConfig,
) {
    loop {
        let event = receiver.lock().await.try_recv();
        match event {
            Ok(event) => process_event(pool, &event, config).await,
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
            | Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => return,
        }
    }
}

async fn process_event(pool: &PgPool, event: &VisitEvent, config: AnalyticsConfig) {
    let classification = match user_agent::classify(event.user_agent()) {
        ParseOutcome::Parsed(classification) => classification,
        ParseOutcome::Unrecognized => user_agent::unknown(),
    };
    let mut last_category = "analytics_persist_failed";
    for attempt in 0..=config.retry_budget {
        let attempt_result = timeout(
            config.operation_timeout,
            repository::persist_once(pool, event, &classification, config.rollback_timeout),
        )
        .await;
        match attempt_result {
            Ok(Ok(PersistOutcome::Inserted)) => {
                metrics::record_analytics(metrics::AnalyticsOutcome::Persisted, None);
                return;
            }
            Ok(Ok(PersistOutcome::Duplicate)) => {
                metrics::record_analytics(metrics::AnalyticsOutcome::Duplicate, None);
                return;
            }
            Ok(Ok(PersistOutcome::LinkDeleted)) => {
                metrics::record_analytics(metrics::AnalyticsOutcome::DroppedDeleted, None);
                return;
            }
            Ok(Err(failure)) => {
                last_category = failure.category;
                if !failure.retryable {
                    break;
                }
            }
            Err(_) => {
                last_category = "analytics_operation_deadline";
            }
        }
        if attempt < config.retry_budget {
            let backoff = retry_backoff(attempt);
            tracing::warn!(
                category = last_category,
                retry = attempt + 1,
                "analytics persistence attempt failed; retrying the same event"
            );
            tokio::time::sleep(backoff).await;
        }
    }
    tracing::warn!(
        category = last_category,
        "analytics event was dropped after bounded persistence attempts"
    );
    metrics::record_analytics(metrics::AnalyticsOutcome::PersistFailed, None);
}

fn retry_backoff(retry: u32) -> Duration {
    let shift = retry.min(10);
    Duration::from_millis(25_u64.saturating_mul(1_u64 << shift)).min(MAX_BACKOFF)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::retry_backoff;

    #[test]
    fn retry_backoff_is_exponential_and_capped() {
        assert_eq!(retry_backoff(0), Duration::from_millis(25));
        assert_eq!(retry_backoff(2), Duration::from_millis(100));
        assert_eq!(retry_backoff(10), Duration::from_secs(1));
        assert_eq!(retry_backoff(u32::MAX), Duration::from_secs(1));
    }
}
