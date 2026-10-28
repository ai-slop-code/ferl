use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use sqlx::PgPool;
use tokio::{sync::mpsc, task::JoinSet};

use crate::{config::AnalyticsConfig, telemetry::metrics};

use super::{event::VisitEvent, worker::AnalyticsWorkers};

pub struct VisitQueue {
    sender: mpsc::Sender<VisitEvent>,
    receiver: Arc<std::sync::Mutex<Option<mpsc::Receiver<VisitEvent>>>>,
    accepting: Arc<AtomicBool>,
    depth: Arc<AtomicUsize>,
    depth_lock: Arc<Mutex<()>>,
}

impl Clone for VisitQueue {
    fn clone(&self) -> Self {
        Self {
            sender: self.sender.clone(),
            receiver: Arc::clone(&self.receiver),
            accepting: Arc::clone(&self.accepting),
            depth: Arc::clone(&self.depth),
            depth_lock: Arc::clone(&self.depth_lock),
        }
    }
}

impl VisitQueue {
    pub(crate) fn new(capacity: usize) -> Self {
        let (sender, receiver) = mpsc::channel(capacity);
        metrics::record_queue_depth(0);
        Self {
            sender,
            receiver: Arc::new(std::sync::Mutex::new(Some(receiver))),
            accepting: Arc::new(AtomicBool::new(true)),
            depth: Arc::new(AtomicUsize::new(0)),
            depth_lock: Arc::new(Mutex::new(())),
        }
    }

    pub(crate) fn try_enqueue(&self, event: VisitEvent) {
        let _depth_lock = self
            .depth_lock
            .lock()
            .expect("analytics queue metric lock is not poisoned");
        let outcome = if !self.accepting.load(Ordering::Acquire) {
            metrics::AnalyticsOutcome::Disconnected
        } else {
            match self.sender.try_send(event) {
                Ok(()) => {
                    let depth = self.depth.fetch_add(1, Ordering::AcqRel) + 1;
                    metrics::record_queue_depth(depth);
                    metrics::AnalyticsOutcome::Enqueued
                }
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

    pub(crate) fn record_dequeue(&self) {
        let _depth_lock = self
            .depth_lock
            .lock()
            .expect("analytics queue metric lock is not poisoned");
        let depth = self.depth.load(Ordering::Acquire);
        if depth > 0 {
            let remaining = self.depth.fetch_sub(1, Ordering::AcqRel) - 1;
            metrics::record_queue_depth(remaining);
        }
    }

    pub(crate) fn queue_depth(&self) -> usize {
        self.depth.load(Ordering::Acquire)
    }

    pub(crate) fn finish_queue(&self) {
        let _depth_lock = self
            .depth_lock
            .lock()
            .expect("analytics queue metric lock is not poisoned");
        let pending = self.depth.swap(0, Ordering::AcqRel);
        if pending > 0 {
            metrics::record_analytics_count(metrics::AnalyticsOutcome::DroppedShutdown, pending);
        }
        metrics::record_queue_depth(0);
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
                self.clone(),
            ));
        }
        Ok(AnalyticsWorkers::new(tasks, cancellation, self.clone()))
    }
}
