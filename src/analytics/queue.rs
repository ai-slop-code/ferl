use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use sqlx::PgPool;
use tokio::{sync::mpsc, task::JoinSet};

use crate::{config::AnalyticsConfig, telemetry::metrics};

use super::{event::VisitEvent, worker::AnalyticsWorkers};

pub struct VisitQueue {
    sender: mpsc::Sender<VisitEvent>,
    receiver: Arc<std::sync::Mutex<Option<mpsc::Receiver<VisitEvent>>>>,
    accepting: Arc<AtomicBool>,
}

impl Clone for VisitQueue {
    fn clone(&self) -> Self {
        Self {
            sender: self.sender.clone(),
            receiver: Arc::clone(&self.receiver),
            accepting: Arc::clone(&self.accepting),
        }
    }
}

impl VisitQueue {
    pub(crate) fn new(capacity: usize) -> Self {
        let (sender, receiver) = mpsc::channel(capacity);
        Self {
            sender,
            receiver: Arc::new(std::sync::Mutex::new(Some(receiver))),
            accepting: Arc::new(AtomicBool::new(true)),
        }
    }

    pub(crate) fn try_enqueue(&self, event: VisitEvent) {
        let outcome = if !self.accepting.load(Ordering::Acquire) {
            metrics::AnalyticsOutcome::Disconnected
        } else {
            match self.sender.try_send(event) {
                Ok(()) => metrics::AnalyticsOutcome::Enqueued,
                Err(mpsc::error::TrySendError::Full(_)) => metrics::AnalyticsOutcome::QueueFull,
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    metrics::AnalyticsOutcome::Disconnected
                }
            }
        };
        metrics::record_analytics(outcome, Some(self.queue_depth()));
    }

    pub(crate) fn stop_accepting(&self) {
        self.accepting.store(false, Ordering::Release);
    }

    fn queue_depth(&self) -> usize {
        self.sender
            .max_capacity()
            .saturating_sub(self.sender.capacity())
    }

    pub(crate) fn start_workers(
        &self,
        pool: PgPool,
        config: AnalyticsConfig,
    ) -> Result<AnalyticsWorkers, super::worker::WorkerStartError> {
        let receiver = self
            .receiver
            .lock()
            .expect("analytics receiver mutex is not poisoned")
            .take()
            .ok_or(super::worker::WorkerStartError::AlreadyStarted)?;
        let mut tasks = JoinSet::new();
        let shared_receiver = Arc::new(tokio::sync::Mutex::new(receiver));
        let cancellation = tokio_util::sync::CancellationToken::new();
        for _ in 0..config.worker_concurrency {
            tasks.spawn(super::worker::worker_loop(
                pool.clone(),
                Arc::clone(&shared_receiver),
                cancellation.clone(),
                config,
            ));
        }
        Ok(AnalyticsWorkers::new(tasks, cancellation, self.clone()))
    }
}
