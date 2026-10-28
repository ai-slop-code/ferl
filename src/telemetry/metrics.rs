#[derive(Clone, Copy, Debug)]
pub enum Operation {
    HttpRequest,
    ReadinessProbe,
    Startup,
    Shutdown,
    Redirect,
}

impl Operation {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::HttpRequest => "http.request",
            Self::ReadinessProbe => "health.readiness",
            Self::Startup => "server.startup",
            Self::Shutdown => "server.shutdown",
            Self::Redirect => "redirect.follow",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum AnalyticsOutcome {
    Enqueued,
    QueueFull,
    Disconnected,
    MissingTransportPeer,
    Persisted,
    Duplicate,
    DroppedDeleted,
    PersistFailed,
    ParserUnrecognized,
}

impl AnalyticsOutcome {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Enqueued => "enqueued",
            Self::QueueFull => "queue_full",
            Self::Disconnected => "disconnected",
            Self::MissingTransportPeer => "missing_transport_peer",
            Self::Persisted => "persisted",
            Self::Duplicate => "duplicate",
            Self::DroppedDeleted => "dropped_deleted",
            Self::PersistFailed => "persist_failed",
            Self::ParserUnrecognized => "parser_unrecognized",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Outcome {
    Success,
    Rejected,
    Unavailable,
    Failure,
}

impl Outcome {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Rejected => "rejected",
            Self::Unavailable => "unavailable",
            Self::Failure => "failure",
        }
    }
}

pub fn record_operation(operation: Operation, outcome: Outcome) {
    tracing::debug!(
        operation = operation.as_str(),
        outcome = outcome.as_str(),
        "operation outcome"
    );
}

/// Safe low-cardinality analytics telemetry. Queue depth is a gauge value,
/// never an attribute derived from event contents.
pub fn record_analytics(outcome: AnalyticsOutcome, queue_depth: Option<usize>) {
    tracing::info!(
        operation = "analytics.visit",
        outcome = outcome.as_str(),
        queue_depth = queue_depth,
        "analytics worker outcome"
    );
}
