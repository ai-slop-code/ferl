use std::{sync::OnceLock, time::Instant};

use opentelemetry::{
    KeyValue,
    metrics::{Counter, Gauge, Histogram, MeterProvider, UpDownCounter},
};
use opentelemetry_sdk::metrics::SdkMeterProvider;

static INSTRUMENTS: OnceLock<Instruments> = OnceLock::new();

struct Instruments {
    http_requests: Counter<u64>,
    http_duration: Histogram<f64>,
    http_in_flight: UpDownCounter<i64>,
    redirect_outcomes: Counter<u64>,
    link_creation_outcomes: Counter<u64>,
    auth_failures: Counter<u64>,
    rate_rejections: Counter<u64>,
    db_duration: Histogram<f64>,
    db_failures: Counter<u64>,
    analytics_events: Counter<u64>,
    analytics_queue_depth: Gauge<u64>,
    telemetry_log_records: Counter<u64>,
}

impl Instruments {
    fn new(provider: &SdkMeterProvider) -> Self {
        let meter = provider.meter("ferl.server");
        Self {
            http_requests: meter
                .u64_counter("ferl.http.server.request.count")
                .with_description("HTTP requests completed or cancelled by the server")
                .with_unit("{request}")
                .build(),
            http_duration: meter
                .f64_histogram("ferl.http.server.request.duration")
                .with_description("Elapsed server handling time for one HTTP request")
                .with_unit("ms")
                .build(),
            http_in_flight: meter
                .i64_up_down_counter("ferl.http.server.active_requests")
                .with_description("Requests currently inside the bounded HTTP middleware")
                .with_unit("{request}")
                .build(),
            redirect_outcomes: meter
                .u64_counter("ferl.redirect.outcomes")
                .with_description("Redirect lookup and response outcomes")
                .with_unit("{operation}")
                .build(),
            link_creation_outcomes: meter
                .u64_counter("ferl.link.creation.outcomes")
                .with_description("Link creation attempt outcomes after service entry")
                .with_unit("{operation}")
                .build(),
            auth_failures: meter
                .u64_counter("ferl.authentication.failures")
                .with_description("Authentication and authorization rejections")
                .with_unit("{failure}")
                .build(),
            rate_rejections: meter
                .u64_counter("ferl.link.creation.rate_rejections")
                .with_description("Creation attempts rejected by the durable rolling allowance")
                .with_unit("{attempt}")
                .build(),
            db_duration: meter
                .f64_histogram("ferl.database.operation.duration")
                .with_description("Elapsed time for a sanitized database operation category")
                .with_unit("ms")
                .build(),
            db_failures: meter
                .u64_counter("ferl.database.operation.failures")
                .with_description("Database operations that ended in failure or cancellation")
                .with_unit("{operation}")
                .build(),
            analytics_events: meter
                .u64_counter("ferl.analytics.events")
                .with_description("Analytics admission, persistence, duplicate, and drop outcomes")
                .with_unit("{event}")
                .build(),
            analytics_queue_depth: meter
                .u64_gauge("ferl.analytics.queue.depth")
                .with_description("Current in-memory analytics events waiting for a worker")
                .with_unit("{event}")
                .build(),
            telemetry_log_records: meter
                .u64_counter("ferl.telemetry.log_records")
                .with_description("Sanitized application LogRecords submitted to the SDK bridge")
                .with_unit("{log_record}")
                .build(),
        }
    }
}

/// Installs this process's metric instruments against its owned SDK provider.
/// Production composition calls this exactly once; tests that need real export
/// use an isolated executable so global SDK state cannot leak across tests.
pub(crate) fn install(provider: &SdkMeterProvider) {
    let _ = INSTRUMENTS.set(Instruments::new(provider));
}

#[derive(Clone, Copy, Debug)]
pub enum Operation {
    HttpRequest,
    ReadinessProbe,
    Startup,
    Shutdown,
    Redirect,
    LinkCreate,
    Authentication,
}

impl Operation {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::HttpRequest => "http.request",
            Self::ReadinessProbe => "health.readiness",
            Self::Startup => "server.startup",
            Self::Shutdown => "server.shutdown",
            Self::Redirect => "redirect.follow",
            Self::LinkCreate => "link.create",
            Self::Authentication => "credential.authenticate",
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
    DroppedShutdown,
    RetryAttempt,
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
            Self::DroppedShutdown => "dropped_shutdown",
            Self::RetryAttempt => "retry_attempt",
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

#[derive(Clone, Copy, Debug)]
pub enum AuthFailureCategory {
    InvalidCredential,
    StorageUnavailable,
    Internal,
    MissingCredential,
    ForbiddenScope,
}

impl AuthFailureCategory {
    const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidCredential => "invalid_credential",
            Self::StorageUnavailable => "storage_unavailable",
            Self::Internal => "internal",
            Self::MissingCredential => "missing_credential",
            Self::ForbiddenScope => "forbidden_scope",
        }
    }
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

/// Cancellation-safe request ownership: cancellation still releases the active
/// count and records a bounded `cancelled` completion.
pub struct HttpRequestGuard {
    started: Instant,
    method: &'static str,
    route: &'static str,
    completed: bool,
}

impl HttpRequestGuard {
    pub fn new(method: &'static str, route: &'static str) -> Self {
        if let Some(instruments) = INSTRUMENTS.get() {
            instruments.http_in_flight.add(
                1,
                &[
                    KeyValue::new("method", method),
                    KeyValue::new("route", route),
                ],
            );
        }
        Self {
            started: Instant::now(),
            method,
            route,
            completed: false,
        }
    }

    pub fn complete(&mut self, status: http::StatusCode) {
        if self.completed {
            return;
        }
        let class = status_class(status);
        self.record(
            class,
            if status.is_server_error() {
                "failure"
            } else if status.is_client_error() {
                "rejected"
            } else {
                "success"
            },
        );
        self.completed = true;
    }

    fn record(&self, status_class: &'static str, outcome: &'static str) {
        if let Some(instruments) = INSTRUMENTS.get() {
            let attrs = [
                KeyValue::new("method", self.method),
                KeyValue::new("route", self.route),
                KeyValue::new("status_class", status_class),
                KeyValue::new("outcome", outcome),
            ];
            instruments.http_requests.add(1, &attrs);
            instruments
                .http_duration
                .record(self.started.elapsed().as_secs_f64() * 1_000.0, &attrs);
        }
    }
}

impl Drop for HttpRequestGuard {
    fn drop(&mut self) {
        if !self.completed {
            self.record("cancelled", "cancelled");
        }
        if let Some(instruments) = INSTRUMENTS.get() {
            instruments.http_in_flight.add(
                -1,
                &[
                    KeyValue::new("method", self.method),
                    KeyValue::new("route", self.route),
                ],
            );
        }
    }
}

pub fn record_operation(operation: Operation, outcome: Outcome) {
    if let Some(instruments) = INSTRUMENTS.get() {
        let attrs = [
            KeyValue::new("operation", operation.as_str()),
            KeyValue::new("outcome", outcome.as_str()),
        ];
        match operation {
            Operation::Redirect => instruments.redirect_outcomes.add(1, &attrs),
            Operation::LinkCreate => instruments.link_creation_outcomes.add(1, &attrs),
            Operation::Authentication if !matches!(outcome, Outcome::Success) => {
                instruments.auth_failures.add(1, &attrs);
            }
            _ => {}
        }
    }
    tracing::debug!(
        operation = operation.as_str(),
        outcome = outcome.as_str(),
        "operation outcome"
    );
}

pub fn record_auth_failure(category: AuthFailureCategory) {
    if let Some(instruments) = INSTRUMENTS.get() {
        instruments
            .auth_failures
            .add(1, &[KeyValue::new("category", category.as_str())]);
    }
}

pub fn record_rate_rejection() {
    if let Some(instruments) = INSTRUMENTS.get() {
        instruments.rate_rejections.add(1, &[]);
    }
}

/// A bounded database operation vocabulary; never pass SQL, table names, or
/// object identifiers to this instrumentation boundary.
#[derive(Clone, Copy, Debug)]
pub enum DbOperation {
    CredentialLookup,
    RedirectLookup,
    LinkCreate,
    RateCharge,
    AccountMutation,
    LinkMutation,
    ReportQuery,
    AnalyticsPersist,
    ReadinessProbe,
}

impl DbOperation {
    const fn as_str(self) -> &'static str {
        match self {
            Self::CredentialLookup => "credential.lookup",
            Self::RedirectLookup => "redirect.lookup",
            Self::LinkCreate => "link.create",
            Self::RateCharge => "rate_limit.charge",
            Self::AccountMutation => "account.mutation",
            Self::LinkMutation => "link.mutation",
            Self::ReportQuery => "report.query",
            Self::AnalyticsPersist => "analytics.persist",
            Self::ReadinessProbe => "health.readiness_probe",
        }
    }
}

pub struct DbTimer {
    operation: DbOperation,
    started: Instant,
    outcome: &'static str,
}

impl DbTimer {
    pub fn new(operation: DbOperation) -> Self {
        Self {
            operation,
            started: Instant::now(),
            outcome: "failure",
        }
    }

    pub fn success(&mut self) {
        self.outcome = "success";
    }

    pub fn failure(&mut self) {
        self.outcome = "failure";
    }
}

impl Drop for DbTimer {
    fn drop(&mut self) {
        if let Some(instruments) = INSTRUMENTS.get() {
            let attrs = [
                KeyValue::new("operation", self.operation.as_str()),
                KeyValue::new("outcome", self.outcome),
            ];
            instruments
                .db_duration
                .record(self.started.elapsed().as_secs_f64() * 1_000.0, &attrs);
            if self.outcome != "success" {
                instruments.db_failures.add(1, &attrs);
            }
        }
    }
}

/// Update the process-local current-depth gauge after the caller has serialized
/// its queue mutation with its queue ownership lock.
pub(crate) fn record_queue_depth(depth: usize) {
    if let Some(instruments) = INSTRUMENTS.get() {
        instruments.analytics_queue_depth.record(depth as u64, &[]);
    }
}

pub(crate) fn record_log_record(severity: &'static str) {
    if let Some(instruments) = INSTRUMENTS.get() {
        instruments
            .telemetry_log_records
            .add(1, &[KeyValue::new("severity", severity)]);
    }
}

pub fn record_analytics(outcome: AnalyticsOutcome, queue_depth: Option<usize>) {
    record_analytics_count(outcome, 1);
    tracing::info!(
        operation = "analytics.visit",
        outcome = outcome.as_str(),
        queue_depth = queue_depth,
        "analytics worker outcome"
    );
}

pub(crate) fn record_analytics_count(outcome: AnalyticsOutcome, count: usize) {
    if count == 0 {
        return;
    }
    if let Some(instruments) = INSTRUMENTS.get() {
        let (phase, value) = match outcome {
            AnalyticsOutcome::Enqueued => ("enqueue", "admitted"),
            AnalyticsOutcome::QueueFull => ("drop", "queue_full"),
            AnalyticsOutcome::Disconnected => ("drop", "producer_closed"),
            AnalyticsOutcome::MissingTransportPeer => ("drop", "missing_peer"),
            AnalyticsOutcome::Persisted => ("persist", "committed"),
            AnalyticsOutcome::Duplicate => ("persist", "duplicate"),
            AnalyticsOutcome::DroppedDeleted => ("drop", "link_deleted"),
            AnalyticsOutcome::DroppedShutdown => ("drop", "shutdown_deadline"),
            AnalyticsOutcome::RetryAttempt => ("retry", "attempt"),
            AnalyticsOutcome::PersistFailed => ("failure", "persist_failed"),
            AnalyticsOutcome::ParserUnrecognized => ("parse", "unrecognized"),
        };
        instruments.analytics_events.add(
            count as u64,
            &[
                KeyValue::new("phase", phase),
                KeyValue::new("outcome", value),
            ],
        );
    }
}

fn status_class(status: http::StatusCode) -> &'static str {
    if status.is_informational() {
        "1xx"
    } else if status.is_success() {
        "2xx"
    } else if status.is_redirection() {
        "3xx"
    } else if status.is_client_error() {
        "4xx"
    } else {
        "5xx"
    }
}
