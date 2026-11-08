use std::{
    collections::BTreeMap,
    io::{self, Write},
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError},
    },
    thread,
    time::{Duration, Instant, SystemTime},
};

use opentelemetry::{
    Context, Key,
    logs::{AnyValue, LogRecord, Logger, LoggerProvider, Severity},
    trace::TraceContextExt,
};
use opentelemetry_sdk::logs::SdkLoggerProvider;
use tracing::{Event, Subscriber, field::Visit};
use tracing_subscriber::{
    EnvFilter,
    filter::filter_fn,
    fmt::{self, MakeWriter},
    layer::{Context as LayerContext, Filter, Layer, SubscriberExt},
    util::SubscriberInitExt,
};

/// Complete serving diagnostic records that may wait for the stdout consumer.
/// Records beyond this bound are discarded and counted rather than staged.
const DIAGNOSTIC_QUEUE_RECORDS: usize = 256;
/// A complete record larger than this is discarded. Each queued record is
/// therefore bounded, and the queue retains at most about 2 MiB of records.
const DIAGNOSTIC_RECORD_BYTES: usize = 8 * 1024;
const WRITER_POLL: Duration = Duration::from_millis(10);
const FINISH_POLL: Duration = Duration::from_millis(10);

static STDOUT: OnceLock<DiagnosticSink> = OnceLock::new();

pub fn init_stdout() {
    init_with_writer(DiagnosticWriter::stdout());
}

/// Wait, within the caller's absolute deadline, for queued stdout diagnostics to
/// reach the consumer. Records still pending at the deadline are discarded and
/// the writer is abandoned; a stalled consumer cannot delay the caller further.
pub async fn finish_stdout(deadline: Instant) {
    if let Some(sink) = STDOUT.get() {
        sink.finish(deadline).await;
    }
}

/// Emit one already-sanitized, nonrecursive diagnostic line through the same
/// bounded stdout queue. It never enters tracing, so it cannot be exported as a
/// log record or reenter the failing exporter.
pub(crate) fn emit_diagnostic_line(line: &str) {
    stdout_sink().emit_line(line);
}

fn stdout_sink() -> &'static DiagnosticSink {
    STDOUT.get_or_init(|| DiagnosticSink::spawn(io::stdout()))
}

/// Non-blocking production writer for structured stdout. Each tracing event is
/// one record, and producers only attempt a bounded handoff.
#[derive(Clone, Copy)]
pub struct DiagnosticWriter(&'static DiagnosticSink);

impl DiagnosticWriter {
    pub fn stdout() -> Self {
        Self(stdout_sink())
    }
}

impl<'a> MakeWriter<'a> for DiagnosticWriter {
    type Writer = DiagnosticRecord;

    fn make_writer(&'a self) -> Self::Writer {
        DiagnosticRecord {
            sink: self.0,
            bytes: Vec::new(),
            oversized: false,
        }
    }
}

/// Owns the single stdout writer thread for one process. Producers enqueue
/// complete records with `try_send`; only that thread may block on the consumer.
pub(crate) struct DiagnosticSink {
    sender: SyncSender<Vec<u8>>,
    shared: Arc<DiagnosticShared>,
}

#[derive(Default)]
struct DiagnosticShared {
    dropped: AtomicU64,
    closing: AtomicBool,
    abandoned: AtomicBool,
    finished: AtomicBool,
}

impl DiagnosticSink {
    pub(crate) fn spawn<W>(writer: W) -> Self
    where
        W: Write + Send + 'static,
    {
        let (sender, receiver) = mpsc::sync_channel(DIAGNOSTIC_QUEUE_RECORDS);
        let shared = Arc::new(DiagnosticShared::default());
        let writer_shared = Arc::clone(&shared);
        thread::Builder::new()
            .name("ferl-stdout-diagnostics".to_owned())
            .spawn(move || run_writer(writer, receiver, writer_shared))
            .expect("diagnostic writer thread starts");
        Self { sender, shared }
    }

    fn submit(&self, record: Vec<u8>) {
        if self.shared.closing.load(Ordering::Acquire) || self.sender.try_send(record).is_err() {
            self.discard();
        }
    }

    fn discard(&self) {
        self.shared.dropped.fetch_add(1, Ordering::Relaxed);
    }

    fn emit_line(&self, line: &str) {
        let mut record = Vec::with_capacity(line.len() + 1);
        record.extend_from_slice(line.as_bytes());
        record.push(b'\n');
        if record.len() > DIAGNOSTIC_RECORD_BYTES {
            self.discard();
        } else {
            self.submit(record);
        }
    }

    async fn finish(&self, deadline: Instant) {
        self.shared.closing.store(true, Ordering::Release);
        loop {
            if self.shared.finished.load(Ordering::Acquire)
                || self.shared.abandoned.load(Ordering::Acquire)
            {
                return;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                self.shared.abandoned.store(true, Ordering::Release);
                return;
            }
            tokio::time::sleep(remaining.min(FINISH_POLL)).await;
        }
    }
}

/// One tracing event's staging buffer. It is submitted atomically on drop, so a
/// bounded number of complete records, not partial lines, enter the queue.
pub struct DiagnosticRecord {
    sink: &'static DiagnosticSink,
    bytes: Vec<u8>,
    oversized: bool,
}

impl Write for DiagnosticRecord {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if !self.oversized {
            if self.bytes.len() + buffer.len() <= DIAGNOSTIC_RECORD_BYTES {
                self.bytes.extend_from_slice(buffer);
            } else {
                self.oversized = true;
                self.bytes = Vec::new();
            }
        }
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for DiagnosticRecord {
    fn drop(&mut self) {
        if self.oversized {
            self.sink.discard();
        } else if !self.bytes.is_empty() {
            self.sink.submit(std::mem::take(&mut self.bytes));
        }
    }
}

fn run_writer<W: Write>(mut writer: W, receiver: Receiver<Vec<u8>>, shared: Arc<DiagnosticShared>) {
    loop {
        if shared.abandoned.load(Ordering::Acquire) {
            return;
        }
        let record = match receiver.try_recv() {
            Ok(record) => record,
            Err(TryRecvError::Disconnected) => break,
            Err(TryRecvError::Empty) => {
                if shared.closing.load(Ordering::Acquire) {
                    break;
                }
                match receiver.recv_timeout(WRITER_POLL) {
                    Ok(record) => record,
                    Err(RecvTimeoutError::Timeout) => continue,
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            }
        };
        write_record(&mut writer, &shared, &record);
    }
    let _ = writer.flush();
    shared.finished.store(true, Ordering::Release);
}

/// Report discarded records before the next complete record. The count is
/// restored if the report itself cannot be written, so it is not silently lost.
fn write_record<W: Write>(writer: &mut W, shared: &DiagnosticShared, record: &[u8]) {
    let lost = shared.dropped.swap(0, Ordering::AcqRel);
    if lost > 0 && writer.write_all(&loss_record(lost)).is_err() {
        shared.dropped.fetch_add(lost, Ordering::Relaxed);
    }
    if writer.write_all(record).is_err() {
        shared.dropped.fetch_add(1, Ordering::Relaxed);
    }
}

fn loss_record(lost: u64) -> Vec<u8> {
    let mut line = serde_json::to_vec(&serde_json::json!({
        "timestamp": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true),
        "level": "WARN",
        "fields": {
            "message": "diagnostic records were discarded under bounded stdout backpressure",
            "category": "diagnostic_records_dropped",
            "dropped_records": lost,
        },
    }))
    .unwrap_or_default();
    line.push(b'\n');
    line
}

pub fn init_stderr() {
    init_with_writer(std::io::stderr);
}

/// Install the production JSON/filter composition with an explicit output sink.
pub fn init_with_writer<W>(writer: W)
where
    W: for<'writer> MakeWriter<'writer> + Send + Sync + 'static,
{
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let layer = fmt::layer()
        .json()
        .with_writer(writer)
        .with_current_span(false)
        .with_span_list(false)
        .with_target(false);
    // Global filters compose before formatting. Nesting per-layer filters here
    // can leave application events suppressed after filtered dependency spans.
    tracing_subscriber::registry()
        .with(filter)
        .with(filter_fn(|metadata| {
            is_allowlisted_target(metadata.target())
        }))
        .with(layer)
        .init();
}

/// Install the production stdout subscriber together with the SDK's real
/// tracing-to-trace and sanitized tracing-to-LogRecord layers.
pub(crate) fn init_with_providers<W>(
    writer: W,
    logger_provider: SdkLoggerProvider,
    tracer: opentelemetry_sdk::trace::SdkTracer,
) -> Result<(), tracing_subscriber::util::TryInitError>
where
    W: for<'writer> MakeWriter<'writer> + Send + Sync + 'static,
{
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let stdout = fmt::layer()
        .json()
        .with_writer(writer)
        .with_current_span(false)
        .with_span_list(false)
        .with_target(false);
    let traces = tracing_opentelemetry::OpenTelemetryLayer::new(tracer)
        .with_location(false)
        .with_threads(false)
        .with_tracked_inactivity(false)
        .with_target(false)
        .with_filter(SpanOnlyFilter);
    let logs = SafeOtelLogLayer::new(logger_provider);
    let diagnostics = ExporterDiagnosticsLayer;

    tracing_subscriber::registry()
        .with(filter)
        .with(stdout.with_filter(filter_fn(|metadata| {
            is_allowlisted_target(metadata.target())
        })))
        .with(traces)
        .with(logs.with_filter(filter_fn(|metadata| {
            is_allowlisted_target(metadata.target())
        })))
        .with(diagnostics.with_filter(filter_fn(|metadata| {
            is_otel_internal_target(metadata.target())
        })))
        .try_init()
}

/// Span attributes form the trace allowlist. Events are exported only through
/// `SafeOtelLogLayer`, whose visitor copies an explicit set of safe fields.
struct SpanOnlyFilter;

impl<S> Filter<S> for SpanOnlyFilter {
    fn enabled(&self, metadata: &tracing::Metadata<'_>, _context: &LayerContext<'_, S>) -> bool {
        metadata.is_span() && is_allowlisted_target(metadata.target())
    }
}

struct ExporterDiagnosticsLayer;

impl<S: Subscriber> tracing_subscriber::Layer<S> for ExporterDiagnosticsLayer {
    fn on_event(&self, event: &Event<'_>, _context: LayerContext<'_, S>) {
        if !is_otel_internal_target(event.metadata().target())
            || !matches!(
                *event.metadata().level(),
                tracing::Level::WARN | tracing::Level::ERROR
            )
        {
            return;
        }
        let mut name = DiagnosticEventName::default();
        event.record(&mut name);
        if let Some(name) = name.0 {
            crate::telemetry::providers::diagnostics::sdk_event(event.metadata().target(), &name);
        }
    }
}

#[derive(Default)]
struct DiagnosticEventName(Option<String>);

impl Visit for DiagnosticEventName {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "name"
            && value.len() <= 96
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        {
            self.0 = Some(value.to_owned());
        }
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "name" {
            self.record_str(field, format!("{value:?}").trim_matches('"'));
        }
    }
}

fn is_otel_internal_target(target: &str) -> bool {
    target == "opentelemetry"
        || target.starts_with("opentelemetry::")
        || target == "opentelemetry_sdk"
        || target.starts_with("opentelemetry_sdk::")
        || target == "opentelemetry_otlp"
        || target.starts_with("opentelemetry_otlp::")
}

struct SafeOtelLogLayer {
    logger: <SdkLoggerProvider as LoggerProvider>::Logger,
}

impl SafeOtelLogLayer {
    fn new(provider: SdkLoggerProvider) -> Self {
        Self {
            logger: provider.logger("ferl.server"),
        }
    }
}

impl<S: Subscriber> tracing_subscriber::Layer<S> for SafeOtelLogLayer {
    fn on_event(&self, event: &Event<'_>, _context: LayerContext<'_, S>) {
        let metadata = event.metadata();
        if !is_allowlisted_target(metadata.target())
            || !matches!(
                *metadata.level(),
                tracing::Level::INFO | tracing::Level::WARN | tracing::Level::ERROR
            )
        {
            return;
        }

        let mut fields = SafeFields::default();
        event.record(&mut fields);

        let mut record = self.logger.create_log_record();
        record.set_event_name("ferl.application");
        record.set_timestamp(SystemTime::now());
        record.set_observed_timestamp(SystemTime::now());
        record.set_body(AnyValue::from("ferl application event"));
        let safe_severity = match *metadata.level() {
            tracing::Level::ERROR => {
                record.set_severity_text("ERROR");
                record.set_severity_number(Severity::Error);
                "error"
            }
            tracing::Level::WARN => {
                record.set_severity_text("WARN");
                record.set_severity_number(Severity::Warn);
                "warn"
            }
            _ => {
                record.set_severity_text("INFO");
                record.set_severity_number(Severity::Info);
                "info"
            }
        };

        for (name, value) in fields.0 {
            match value {
                SafeField::String(value) => record.add_attribute(Key::new(name), value),
                SafeField::Integer(value) => record.add_attribute(Key::new(name), value),
                SafeField::Boolean(value) => record.add_attribute(Key::new(name), value),
            }
        }

        let span_context = Context::current().span().span_context().clone();
        if span_context.is_valid() {
            record.set_trace_context(
                span_context.trace_id(),
                span_context.span_id(),
                Some(span_context.trace_flags()),
            );
        }
        self.logger.emit(record);
        crate::telemetry::metrics::record_log_record(safe_severity);
    }
}

#[derive(Default)]
struct SafeFields(BTreeMap<&'static str, SafeField>);

const SAFE_CATEGORIES: &[&str] = &[
    "account_operation_deadline",
    "account_persistence_failure",
    "account_response_serialization_failure",
    "account_transaction_rollback_failed",
    "account_authority_lock_deadline",
    "anonymous_creation_disabled",
    "analytics_drain_complete",
    "analytics_drain_deadline",
    "analytics_counter_overflow",
    "analytics_link_identity_mismatch",
    "analytics_operation_deadline",
    "analytics_persist_failed",
    "analytics_rollback_deadline",
    "analytics_transport_peer_missing",
    "analytics_worker_failed",
    "analytics_worker_start_failed",
    "authentication_persistence_failure",
    "authentication_required",
    "authentication_storage_unavailable",
    "authority_guard_rollback_failed",
    "bootstrap_already_completed",
    "bootstrap_committed",
    "bootstrap_database_unavailable_or_commit_uncertain",
    "bootstrap_persistence_failure",
    "bootstrap_randomness_unavailable",
    "bootstrap_result_output_failed",
    "bootstrap_result_serialization_failed",
    "bootstrap_rollback_failed",
    "bootstrap_schema_incompatible",
    "bootstrap_username_conflict",
    "bootstrap_username_invalid",
    "code_generation_unavailable",
    "code_reservation_conflict",
    "configuration_invalid",
    "credential_digest_conflict",
    "creation_internal_failure",
    "database_commit_outcome_unknown",
    "database_configuration_invalid",
    "database_connection_unavailable",
    "database_internal_failure",
    "database_pool_close_deadline_reached",
    "database_pool_unavailable",
    "database_timeout_or_lock",
    "database_transaction_aborted",
    "database_unavailable_or_commit_uncertain",
    "forbidden",
    "http_drain_deadline_reached",
    "http_handler_cancel_deadline",
    "http_server_failed",
    "http_server_stopped_unexpectedly",
    "invalid_credentials",
    "invalid_request",
    "last_enabled_admin_required",
    "link_authentication_internal_failure",
    "link_authentication_storage_unavailable",
    "link_commit_outcome_unknown",
    "link_operation_deadline",
    "link_persistence_failure",
    "link_response_serialization_failure",
    "link_storage_unavailable",
    "link_transaction_rollback_failed",
    "listener_bind_failed",
    "management_token_digest_conflict",
    "migration_connection_unavailable",
    "migration_database_unavailable",
    "migration_execution_failed",
    "migration_lock_release_failed",
    "migration_lock_timeout",
    "migration_total_deadline",
    "migrations_complete",
    "metric_export_shutdown_deadline",
    "not_anonymous_link",
    "not_found",
    "pool_close_deadline",
    "postgres_version_unsupported",
    "rate_cleanup_failed",
    "rate_commit_outcome_unknown",
    "rate_limited",
    "rate_rollback_failed",
    "rate_storage_unavailable",
    "readiness_not_ready",
    "readiness_schema_unverified",
    "record_absent",
    "redirect_operation_deadline",
    "redirect_resolution_internal",
    "redirect_storage_unavailable",
    "report_operation_deadline",
    "report_persistence_failure",
    "report_storage_unavailable",
    "schema_column_mismatch",
    "schema_constraint_missing",
    "schema_history_mismatch",
    "schema_history_missing",
    "schema_index_missing",
    "schema_index_mismatch",
    "schema_migration_dirty",
    "schema_monitor_shutdown_deadline",
    "schema_monitor_stopped_unexpectedly",
    "schema_seed_missing",
    "schema_table_missing",
    "schema_trigger_missing",
    "schema_version_newer_than_binary",
    "schema_version_unsupported",
    "schema_verification_database_unavailable",
    "schema_verification_deadline",
    "schema_verification_superseded",
    "server_draining",
    "server_shutdown_complete",
    "server_started",
    "server_stopped",
    "service_unavailable",
    "short_code_conflict",
    "shutdown_deadline_reached",
    "shutdown_signal_error",
    "tag_discovery_deadline",
    "tag_persistence_failure",
    "tag_storage_unavailable",
    "telemetry_configuration_invalid",
    "telemetry_provider_shutdown_deadline",
    "telemetry_provider_shutdown_skipped",
    "telemetry_subscriber_initialization_failed",
    "username_conflict",
];

enum SafeField {
    String(String),
    Integer(i64),
    Boolean(bool),
}

impl Visit for SafeFields {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        let name = field.name();
        if !matches!(
            name,
            "request_id"
                | "trace_id"
                | "span_id"
                | "category"
                | "operation"
                | "outcome"
                | "method"
                | "route"
                | "status_class"
                | "setting"
        ) {
            return;
        }
        self.record_safe_string(name, format!("{value:?}"));
    }

    fn record_error(
        &mut self,
        _field: &tracing::field::Field,
        _value: &(dyn std::error::Error + 'static),
    ) {
        // Dependency error text and chains are never exported.
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.record_safe_string(field.name(), value.to_owned());
    }

    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        if matches!(field.name(), "status_code" | "retry" | "queue_depth") {
            self.0.insert(field.name(), SafeField::Integer(value));
        }
    }

    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        if matches!(field.name(), "status_code" | "retry" | "queue_depth")
            && let Ok(value) = i64::try_from(value)
        {
            self.0.insert(field.name(), SafeField::Integer(value));
        }
    }

    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        if matches!(field.name(), "schema_compatible" | "sampled") {
            self.0.insert(field.name(), SafeField::Boolean(value));
        }
    }
}

impl SafeFields {
    fn record_safe_string(&mut self, name: &'static str, value: String) {
        let value = value.trim_matches('"');
        let safe = match name {
            "request_id" => uuid::Uuid::parse_str(value).is_ok(),
            "trace_id" => valid_hex_id(value, 32),
            "span_id" => valid_hex_id(value, 16),
            "category" => SAFE_CATEGORIES.contains(&value),
            "operation" => matches!(
                value,
                "http.request"
                    | "health.readiness"
                    | "server.startup"
                    | "server.shutdown"
                    | "redirect.follow"
                    | "link.create"
                    | "credential.authenticate"
                    | "analytics.visit"
            ),
            "outcome" => matches!(
                value,
                "success"
                    | "rejected"
                    | "unavailable"
                    | "failure"
                    | "enqueued"
                    | "queue_full"
                    | "disconnected"
                    | "missing_transport_peer"
                    | "persisted"
                    | "duplicate"
                    | "dropped_deleted"
                    | "dropped_shutdown"
                    | "retry_attempt"
                    | "persist_failed"
                    | "parser_unrecognized"
            ),
            "method" => matches!(
                value,
                "GET" | "HEAD" | "POST" | "PUT" | "PATCH" | "DELETE" | "OPTIONS" | "OTHER"
            ),
            "route" => safe_route(value),
            "status_class" => matches!(value, "1xx" | "2xx" | "3xx" | "4xx" | "5xx" | "cancelled"),
            "setting" => safe_setting(value),
            _ => false,
        };
        if safe {
            self.0.insert(name, SafeField::String(value.to_owned()));
        }
    }
}

fn valid_hex_id(value: &str, length: usize) -> bool {
    value.len() == length
        && value.bytes().all(|byte| byte.is_ascii_hexdigit())
        && value.bytes().any(|byte| byte != b'0')
}

fn safe_route(value: &str) -> bool {
    matches!(
        value,
        "/api/health/live"
            | "/api/health/ready"
            | "/api/v1/me"
            | "/api/v1/me/api-key"
            | "/api/v1/admin/accounts"
            | "/api/v1/admin/accounts/{id}"
            | "/api/v1/admin/accounts/{id}/api-key"
            | "/api/v1/links"
            | "/api/v1/links/{code}"
            | "/api/v1/links/{code}/management-token"
            | "/api/v1/links/{code}/visits"
            | "/api/v1/links/{code}/stats"
            | "/api/v1/links/{code}/stats/breakdown"
            | "/api/v1/tags"
            | "/api"
            | "/api/{unmatched}"
            | "/{short_code}"
            | "/{unmatched}"
    )
}

fn safe_setting(value: &str) -> bool {
    value.starts_with("FERL_") || value.starts_with("OTEL_") || matches!(value, "environment")
}

/// Keep application stdout/stderr diagnostics on the explicit internal target
/// allowlist; dependency events can contain SQL, parameters, URLs, or headers.
pub fn is_allowlisted_target(target: &str) -> bool {
    target == "ferl_server" || target.starts_with("ferl_server::")
}

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, MutexGuard};

    use serde_json::Value;
    use tokio::time::timeout;

    use super::*;

    #[derive(Default)]
    struct GateState {
        open: bool,
        entered: usize,
        fail_next: usize,
        delivered: Vec<u8>,
    }

    #[derive(Clone, Default)]
    struct Gate(Arc<(Mutex<GateState>, std::sync::Condvar)>);

    impl Gate {
        fn writer(&self) -> GatedWriter {
            GatedWriter(self.clone())
        }

        fn state(&self) -> MutexGuard<'_, GateState> {
            self.0.0.lock().expect("gate lock is not poisoned")
        }

        fn open(&self) {
            self.state().open = true;
            self.0.1.notify_all();
        }

        fn wait_entered(&self, writes: usize) {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut state = self.state();
            while state.entered < writes {
                let remaining = deadline.saturating_duration_since(Instant::now());
                assert!(!remaining.is_zero(), "consumer reached its blocking write");
                state = self
                    .0
                    .1
                    .wait_timeout(state, remaining)
                    .expect("gate wait succeeds")
                    .0;
            }
        }

        fn delivered_lines(&self) -> Vec<Value> {
            let bytes = self.state().delivered.clone();
            String::from_utf8(bytes)
                .expect("delivered records are UTF-8")
                .lines()
                .map(|line| serde_json::from_str(line).expect("delivered record is JSON"))
                .collect()
        }
    }

    struct GatedWriter(Gate);

    impl Write for GatedWriter {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            let (state, ready) = &*self.0.0;
            let mut state = state.lock().expect("gate lock is not poisoned");
            state.entered += 1;
            ready.notify_all();
            while !state.open {
                state = ready.wait(state).expect("gate wait succeeds");
            }
            if state.fail_next > 0 {
                state.fail_next -= 1;
                return Err(io::Error::other("test consumer failed"));
            }
            state.delivered.extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn leaked_sink(gate: &Gate) -> &'static DiagnosticSink {
        Box::leak(Box::new(DiagnosticSink::spawn(gate.writer())))
    }

    fn json_subscriber(sink: &'static DiagnosticSink) -> impl Subscriber + Send + Sync {
        fmt::Subscriber::builder()
            .json()
            .with_writer(DiagnosticWriter(sink))
            .with_current_span(false)
            .with_span_list(false)
            .with_target(false)
            .finish()
    }

    fn category_lines(lines: &[Value]) -> Vec<&Value> {
        lines
            .iter()
            .filter(|line| line["fields"]["category"] == "diagnostic_records_dropped")
            .collect()
    }

    #[tokio::test]
    async fn full_queue_discards_excess_records_without_blocking_producers() {
        let gate = Gate::default();
        let sink = leaked_sink(&gate);
        let produced = DIAGNOSTIC_QUEUE_RECORDS * 2;

        let (done_tx, done_rx) = mpsc::channel();
        let producer_gate = gate.clone();
        let producer = thread::spawn(move || {
            tracing::subscriber::with_default(json_subscriber(sink), || {
                tracing::info!(index = 0_u64, "first record");
                // The consumer is blocked inside the first record's write, so
                // every later record must be admitted or discarded immediately.
                producer_gate.wait_entered(1);
                for index in 1..produced {
                    tracing::info!(index = index as u64, "queued record");
                }
            });
            done_tx.send(()).expect("test receives producer completion");
        });
        done_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("producers finish while the consumer is blocked");
        producer.join().expect("producer thread completes");

        // The blocked first record is not queued; every later record beyond the
        // queue bound is discarded.
        let discarded = (produced - 1 - DIAGNOSTIC_QUEUE_RECORDS) as u64;
        assert_eq!(
            sink.shared.dropped.load(Ordering::Acquire),
            discarded,
            "exactly the records beyond the queue bound are discarded"
        );

        gate.open();
        timeout(
            Duration::from_secs(5),
            sink.finish(Instant::now() + Duration::from_secs(5)),
        )
        .await
        .expect("finish returns while the healthy consumer drains");
        assert!(sink.shared.finished.load(Ordering::Acquire));

        let lines = gate.delivered_lines();
        assert_eq!(lines.len(), DIAGNOSTIC_QUEUE_RECORDS + 2);
        assert_eq!(lines[0]["fields"]["index"], 0);
        let loss = category_lines(&lines);
        assert_eq!(loss.len(), 1, "one loss summary is delivered");
        assert_eq!(loss[0]["fields"]["dropped_records"], discarded);
        assert_eq!(lines[1]["fields"]["category"], "diagnostic_records_dropped");
        for (position, line) in lines[2..].iter().enumerate() {
            assert_eq!(line["fields"]["index"], position as u64 + 1);
            assert_eq!(line["fields"]["message"], "queued record");
            assert_eq!(line["level"], "INFO");
        }
    }

    #[tokio::test]
    async fn oversized_record_is_discarded_and_later_records_still_flow() {
        let gate = Gate::default();
        gate.state().open = true;
        let sink = leaked_sink(&gate);
        let oversized = "x".repeat(DIAGNOSTIC_RECORD_BYTES * 2);
        tracing::subscriber::with_default(json_subscriber(sink), || {
            tracing::info!(payload = %oversized, "oversized record");
            tracing::info!("small record");
        });
        timeout(
            Duration::from_secs(5),
            sink.finish(Instant::now() + Duration::from_secs(5)),
        )
        .await
        .expect("finish returns");

        let lines = gate.delivered_lines();
        assert_eq!(
            lines.len(),
            2,
            "only the summary and the small record are written"
        );
        assert_eq!(lines[0]["fields"]["dropped_records"], 1);
        assert_eq!(lines[1]["fields"]["message"], "small record");
    }

    #[tokio::test]
    async fn failed_write_is_counted_and_reported_before_the_next_record() {
        let gate = Gate::default();
        {
            let mut state = gate.state();
            state.open = true;
            state.fail_next = 1;
        }
        let sink = leaked_sink(&gate);
        tracing::subscriber::with_default(json_subscriber(sink), || {
            tracing::info!("lost record");
            tracing::info!("recovered record");
        });
        timeout(
            Duration::from_secs(5),
            sink.finish(Instant::now() + Duration::from_secs(5)),
        )
        .await
        .expect("finish returns");

        let lines = gate.delivered_lines();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["fields"]["dropped_records"], 1);
        assert_eq!(lines[1]["fields"]["message"], "recovered record");
    }

    #[tokio::test]
    async fn stalled_consumer_is_abandoned_at_the_drain_deadline() {
        let gate = Gate::default();
        let sink = leaked_sink(&gate);
        let blocked = thread::spawn(move || {
            tracing::subscriber::with_default(json_subscriber(sink), || {
                tracing::info!("stalled record");
            });
        });
        gate.wait_entered(1);
        blocked
            .join()
            .expect("producer completes without waiting for the consumer");

        timeout(
            Duration::from_secs(5),
            sink.finish(Instant::now() + Duration::from_millis(50)),
        )
        .await
        .expect("finish honors its deadline while the consumer stays blocked");
        assert!(sink.shared.abandoned.load(Ordering::Acquire));
        assert!(!sink.shared.finished.load(Ordering::Acquire));

        sink.emit_line(r#"{"category":"late"}"#);
        gate.open();
    }

    #[tokio::test]
    async fn exporter_diagnostic_lines_enter_the_bounded_queue_without_waiting() {
        let gate = Gate::default();
        let sink = leaked_sink(&gate);
        sink.emit_line(r#"{"category":"otel_export_failed"}"#);
        gate.open();
        timeout(
            Duration::from_secs(5),
            sink.finish(Instant::now() + Duration::from_secs(5)),
        )
        .await
        .expect("finish returns");

        let lines = gate.delivered_lines();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["category"], "otel_export_failed");
    }
}
