use std::{collections::BTreeMap, time::SystemTime};

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

pub fn init_stdout() {
    init_with_writer(std::io::stdout);
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
