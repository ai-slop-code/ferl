//! Best-effort, bounded visit ingestion. Visit rows and counters remain
//! authoritative only in PostgreSQL; this module owns no durable local queue.

mod event;
pub(crate) mod handlers;
mod queue;
mod report_query;
mod report_repository;
mod reports;
mod repository;
mod user_agent;
mod worker;

pub use event::{VisitEvent, VisitMethod};
pub use queue::VisitQueue;
pub(crate) use reports::ReportService;
pub use worker::{AnalyticsWorkers, WorkerStartError, WorkerStopOutcome};
