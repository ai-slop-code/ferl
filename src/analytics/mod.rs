//! Best-effort, bounded visit ingestion. Visit rows and counters remain
//! authoritative only in PostgreSQL; this module owns no durable local queue.

mod event;
mod queue;
mod repository;
mod user_agent;
mod worker;

pub use event::{VisitEvent, VisitMethod};
pub use queue::VisitQueue;
pub use worker::{AnalyticsWorkers, WorkerStartError, WorkerStopOutcome};
