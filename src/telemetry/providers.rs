use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock, Weak},
    time::{Duration, Instant},
};

use opentelemetry::{KeyValue, trace::TracerProvider};
use opentelemetry_otlp::{
    LogExporter as OtlpLogExporter, MetricExporter as OtlpMetricExporter, Protocol, RetryPolicy,
    SpanExporter as OtlpSpanExporter, WithExportConfig, WithHttpConfig,
};
use opentelemetry_sdk::{
    Resource,
    error::OTelSdkResult,
    logs::{
        BatchConfigBuilder as LogBatchConfigBuilder, LogBatch, LogExporter, SdkLoggerProvider,
        log_processor_with_async_runtime::BatchLogProcessor as AsyncBatchLogProcessor,
    },
    metrics::{
        InstrumentKind, ManualReader, Pipeline, SdkMeterProvider, data::ResourceMetrics,
        exporter::PushMetricExporter, reader::MetricReader,
    },
    trace::{
        BatchConfigBuilder, Sampler, SdkTracerProvider, SpanData, SpanExporter,
        span_processor_with_async_runtime::BatchSpanProcessor as AsyncBatchSpanProcessor,
    },
};
use tokio::{
    task::JoinHandle,
    time::{interval, timeout},
};
use tokio_util::sync::CancellationToken;

use crate::{
    BUILD_VERSION,
    config::TelemetryConfig,
    telemetry::{logging, metrics},
};

const BATCH_SCHEDULE: Duration = Duration::from_secs(1);
const METRIC_SCHEDULE: Duration = Duration::from_secs(1);

#[derive(Clone, Copy)]
pub(crate) enum Signal {
    Traces,
    Metrics,
    Logs,
}

impl Signal {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Traces => "traces",
            Self::Metrics => "metrics",
            Self::Logs => "logs",
        }
    }
}

/// A separate, non-tracing diagnostic route. It deliberately does not include
/// exporter error strings and can therefore neither leak dependency detail nor
/// recursively enter the OTLP log pipeline. One diagnostic per 10 seconds is
/// permitted for the entire process (fixed, bounded state).
pub(crate) mod diagnostics {
    use super::*;

    static LAST: OnceLock<Mutex<[Option<Instant>; 6]>> = OnceLock::new();

    pub(super) fn export_failure(signal: Signal) {
        report("otel_export_failed", signal, 0);
    }

    pub(super) fn queue_full(signal: Signal) {
        report("otel_export_queue_full", signal, 1);
    }

    fn report(category: &'static str, signal: Signal, offset: usize) {
        let last = LAST.get_or_init(|| Mutex::new([None; 6]));
        let Ok(mut last) = last.lock() else { return };
        let now = Instant::now();
        let index = signal_index(signal) * 2 + offset;
        if last[index].is_some_and(|at| now.duration_since(at) < Duration::from_secs(10)) {
            return;
        }
        last[index] = Some(now);
        logging::emit_diagnostic_line(&format!(
            "{{\"category\":\"{category}\",\"signal\":\"{}\",\"message\":\"OTLP diagnostics are sanitized; details suppressed\"}}",
            signal.label()
        ));
    }

    fn signal_index(signal: Signal) -> usize {
        match signal {
            Signal::Traces => 0,
            Signal::Metrics => 1,
            Signal::Logs => 2,
        }
    }

    pub(crate) fn sdk_event(target: &str, name: &str) {
        let signal = if name.contains("SpanProcessor") {
            Signal::Traces
        } else if name.contains("LogProcessor") {
            Signal::Logs
        } else if name.contains("PeriodicReader") || name.contains("MetricExporter") {
            Signal::Metrics
        } else {
            return;
        };
        if name.contains("Dropping") || name.contains("Dropped") {
            queue_full(signal);
        } else if name.contains("Export") && (name.contains("Error") || name.contains("Failed"))
            || name.contains("Shutdown.Timeout")
        {
            let _ = target; // The fixed SDK target is not included in the diagnostic.
            export_failure(signal);
        }
    }
}

#[derive(Debug)]
struct SafeSpanExporter(OtlpSpanExporter);

impl SpanExporter for SafeSpanExporter {
    async fn export(&self, batch: Vec<SpanData>) -> OTelSdkResult {
        match self.0.export(batch).await {
            Ok(()) => Ok(()),
            Err(error) => {
                diagnostics::export_failure(Signal::Traces);
                Err(error)
            }
        }
    }

    fn set_resource(&mut self, resource: &Resource) {
        self.0.set_resource(resource);
    }
}

#[derive(Debug)]
struct SafeLogExporter(OtlpLogExporter);

impl LogExporter for SafeLogExporter {
    async fn export(&self, batch: LogBatch<'_>) -> OTelSdkResult {
        match self.0.export(batch).await {
            Ok(()) => Ok(()),
            Err(error) => {
                diagnostics::export_failure(Signal::Logs);
                Err(error)
            }
        }
    }

    fn set_resource(&mut self, resource: &Resource) {
        self.0.set_resource(resource);
    }
}

#[derive(Debug)]
struct SafeMetricExporter(OtlpMetricExporter);

impl PushMetricExporter for SafeMetricExporter {
    async fn export(&self, metrics: &ResourceMetrics) -> OTelSdkResult {
        match self.0.export(metrics).await {
            Ok(()) => Ok(()),
            Err(error) => {
                diagnostics::export_failure(Signal::Metrics);
                Err(error)
            }
        }
    }

    fn force_flush(&self) -> OTelSdkResult {
        self.0.force_flush()
    }

    fn shutdown_with_timeout(&self, timeout: Duration) -> OTelSdkResult {
        self.0.shutdown_with_timeout(timeout)
    }

    fn temporality(&self) -> opentelemetry_sdk::metrics::Temporality {
        self.0.temporality()
    }
}

#[derive(Debug)]
struct SharedManualReader(Arc<ManualReader>);

impl MetricReader for SharedManualReader {
    fn register_pipeline(&self, pipeline: Weak<Pipeline>) {
        self.0.register_pipeline(pipeline);
    }

    fn collect(&self, metrics: &mut ResourceMetrics) -> OTelSdkResult {
        self.0.collect(metrics)
    }

    fn force_flush(&self) -> OTelSdkResult {
        self.0.force_flush()
    }

    fn shutdown_with_timeout(&self, timeout: Duration) -> OTelSdkResult {
        self.0.shutdown_with_timeout(timeout)
    }

    fn temporality(&self, kind: InstrumentKind) -> opentelemetry_sdk::metrics::Temporality {
        self.0.temporality(kind)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TelemetryInitError {
    #[error("telemetry exporter configuration is invalid")]
    ExporterConfiguration,
    #[error("telemetry subscriber could not be installed")]
    SubscriberAlreadyInstalled,
}

/// Owns every process provider, the bounded metric task, and the explicit
/// shutdown path. No SDK global provider is installed, so test processes cannot
/// replace one another's providers; the production binary composes once.
pub struct Telemetry {
    tracer_provider: SdkTracerProvider,
    meter_provider: SdkMeterProvider,
    logger_provider: SdkLoggerProvider,
    metric_reader: Arc<ManualReader>,
    metric_exporter: Option<Arc<SafeMetricExporter>>,
    metric_task: Option<JoinHandle<()>>,
    metric_cancel: CancellationToken,
    export_timeout: Duration,
    provider_flush_budget: Duration,
}

impl Telemetry {
    pub async fn initialize(
        config: &TelemetryConfig,
        total_shutdown_timeout: Duration,
    ) -> Result<Self, TelemetryInitError> {
        // SDK defaults would invoke EnvResourceDetector and inherit arbitrary
        // OTEL_RESOURCE_ATTRIBUTES. Start empty and add only validated values.
        let mut attributes = vec![
            KeyValue::new("service.name", config.service_name.clone()),
            KeyValue::new("service.version", BUILD_VERSION),
            KeyValue::new("service.instance.id", config.instance_id.to_string()),
        ];
        attributes.extend(
            config
                .resource_attributes
                .iter()
                .map(|(key, value)| KeyValue::new(key.clone(), value.clone())),
        );
        let resource = Resource::builder_empty()
            .with_attributes(attributes)
            .build();

        // The asynchronous SDK processors below export at most one batch at a
        // time. Reserve no more than one eighth of the total shutdown budget per
        // signal, including the worst-case number of batches in each bounded
        // queue. RetryPolicy::disabled() makes each batch one bounded attempt.
        let batches_per_provider = config
            .queue_capacity
            .div_ceil(config.batch_size)
            .saturating_add(1);
        let divisor = u32::try_from(batches_per_provider.saturating_mul(8))
            .unwrap_or(u32::MAX)
            .max(1);
        let per_batch_budget = (total_shutdown_timeout / divisor).max(Duration::from_micros(1));
        let export_timeout = config
            .export_timeout
            .min(total_shutdown_timeout / 4)
            .min(per_batch_budget)
            .max(Duration::from_micros(1));

        let trace_provider = build_trace_provider(config, resource.clone(), export_timeout)?;
        let logger_provider = build_log_provider(config, resource.clone(), export_timeout)?;

        let metric_reader = Arc::new(ManualReader::builder().build());
        let meter_provider = SdkMeterProvider::builder()
            .with_resource(resource)
            .with_reader(SharedManualReader(Arc::clone(&metric_reader)))
            .build();
        metrics::install(&meter_provider);

        let metric_exporter = config
            .endpoint
            .as_ref()
            .map(|_| build_metric_exporter(config, export_timeout).map(Arc::new))
            .transpose()?;

        if logging::init_with_providers(
            logging::DiagnosticWriter::stdout(),
            logger_provider.clone(),
            trace_provider.tracer("ferl.server"),
        )
        .is_err()
        {
            return Err(TelemetryInitError::SubscriberAlreadyInstalled);
        }
        opentelemetry::global::set_text_map_propagator(
            opentelemetry_sdk::propagation::TraceContextPropagator::new(),
        );

        let metric_cancel = CancellationToken::new();
        let metric_task = metric_exporter.as_ref().map(|exporter| {
            tokio::spawn(run_metric_exporter(
                Arc::clone(&metric_reader),
                Arc::clone(exporter),
                metric_cancel.clone(),
                export_timeout,
            ))
        });

        Ok(Self {
            tracer_provider: trace_provider,
            meter_provider,
            logger_provider,
            metric_reader,
            metric_exporter,
            metric_task,
            metric_cancel,
            export_timeout,
            provider_flush_budget: export_timeout.saturating_mul(
                u32::try_from(
                    config
                        .queue_capacity
                        .div_ceil(config.batch_size)
                        .saturating_add(1)
                        .saturating_mul(2),
                )
                .unwrap_or(u32::MAX),
            ),
        })
    }

    /// Attempt all signal drains against one caller-owned absolute deadline.
    /// SDK trace/log processors receive only the remaining duration. Metrics use
    /// one async exporter task, which can be cancelled if the deadline expires.
    pub async fn shutdown(&mut self, deadline: Instant) {
        self.metric_cancel.cancel();
        if let Some(mut task) = self.metric_task.take() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                task.abort();
            } else if timeout(remaining, &mut task).await.is_err() {
                // The metric export is async and cancellation drops its request future.
                // Abort is a final guard against a faulty HTTP implementation.
                task.abort();
                tracing::warn!(
                    category = "metric_export_shutdown_deadline",
                    "metric export task exceeded shutdown budget"
                );
            }
        }

        if let Some(exporter) = self.metric_exporter.take() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if !remaining.is_zero() {
                let mut data = ResourceMetrics::default();
                if self.metric_reader.collect(&mut data).is_ok()
                    && data.scope_metrics().next().is_some()
                    && timeout(remaining.min(self.export_timeout), exporter.export(&data))
                        .await
                        .is_err()
                {
                    diagnostics::export_failure(Signal::Metrics);
                }
            }
            let _ = exporter.shutdown_with_timeout(Duration::ZERO);
        }

        // ManualReader shutdown is synchronous and has no worker thread or
        // exporter wait; it prevents SDK provider Drop from doing default work.
        let _ = self.meter_provider.shutdown_with_timeout(Duration::ZERO);

        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining >= self.provider_flush_budget {
            let tracer_provider = self.tracer_provider.clone();
            let logger_provider = self.logger_provider.clone();
            let per_provider = self.provider_flush_budget / 2;
            let shutdown = tokio::task::spawn_blocking(move || {
                let traces = tracer_provider.shutdown_with_timeout(per_provider).is_ok();
                let logs = logger_provider.shutdown_with_timeout(per_provider).is_ok();
                (traces, logs)
            });
            match timeout(remaining, shutdown).await {
                Ok(Ok((true, true))) => {}
                Ok(Ok((traces, logs))) => {
                    if !traces {
                        tracing::warn!(
                            category = "trace_provider_shutdown_deadline",
                            "trace provider did not flush within its bounded budget"
                        );
                    }
                    if !logs {
                        tracing::warn!(
                            category = "log_provider_shutdown_deadline",
                            "log provider did not flush within its bounded budget"
                        );
                    }
                }
                Ok(Err(_)) | Err(_) => tracing::warn!(
                    category = "telemetry_provider_shutdown_deadline",
                    "telemetry providers exceeded their shared shutdown budget"
                ),
            }
        } else {
            // Avoid invoking SDK default Drop shutdown (five seconds) after the
            // total deadline no longer has the provider's calculated bound.
            // The global tracing subscriber retains provider clones until process
            // exit; unflushed data is best effort and dies with this process.
            tracing::warn!(
                category = "telemetry_provider_shutdown_skipped",
                "insufficient total shutdown budget remains for bounded provider flushes"
            );
        }

        logging::finish_stdout(deadline).await;
    }
}

fn build_trace_provider(
    config: &TelemetryConfig,
    resource: Resource,
    timeout: Duration,
) -> Result<SdkTracerProvider, TelemetryInitError> {
    let mut builder = SdkTracerProvider::builder()
        .with_sampler(Sampler::parent_based(Sampler::TraceIdRatioBased(
            config.trace_sample_ratio,
        )))
        .with_resource(resource);
    if let Some(endpoint) = &config.endpoint {
        let exporter = OtlpSpanExporter::builder()
            .with_http()
            .with_endpoint(signal_endpoint(endpoint.as_str(), "traces"))
            .with_protocol(Protocol::HttpBinary)
            .with_timeout(timeout)
            .with_headers(export_headers(config))
            .with_retry_policy(RetryPolicy::disabled())
            .build()
            .map_err(|_| TelemetryInitError::ExporterConfiguration)?;
        let batch = AsyncBatchSpanProcessor::builder(
            SafeSpanExporter(exporter),
            opentelemetry_sdk::runtime::Tokio,
        )
        .with_batch_config(
            BatchConfigBuilder::default()
                .with_max_queue_size(config.queue_capacity)
                .with_max_export_batch_size(config.batch_size)
                .with_max_concurrent_exports(1)
                .with_scheduled_delay(BATCH_SCHEDULE)
                .with_max_export_timeout(timeout)
                .build(),
        )
        .build();
        builder = builder.with_span_processor(batch);
    }
    Ok(builder.build())
}

fn build_log_provider(
    config: &TelemetryConfig,
    resource: Resource,
    timeout: Duration,
) -> Result<SdkLoggerProvider, TelemetryInitError> {
    let mut builder = SdkLoggerProvider::builder().with_resource(resource);
    if let Some(endpoint) = &config.endpoint {
        let exporter = OtlpLogExporter::builder()
            .with_http()
            .with_endpoint(signal_endpoint(endpoint.as_str(), "logs"))
            .with_protocol(Protocol::HttpBinary)
            .with_timeout(timeout)
            .with_headers(export_headers(config))
            .with_retry_policy(RetryPolicy::disabled())
            .build()
            .map_err(|_| TelemetryInitError::ExporterConfiguration)?;
        let processor = AsyncBatchLogProcessor::builder(
            SafeLogExporter(exporter),
            opentelemetry_sdk::runtime::Tokio,
        )
        .with_batch_config(
            LogBatchConfigBuilder::default()
                .with_max_queue_size(config.queue_capacity)
                .with_max_export_batch_size(config.batch_size)
                .with_scheduled_delay(BATCH_SCHEDULE)
                .with_max_export_timeout(timeout)
                .build(),
        )
        .build();
        builder = builder.with_log_processor(processor);
    }
    Ok(builder.build())
}

fn build_metric_exporter(
    config: &TelemetryConfig,
    timeout: Duration,
) -> Result<SafeMetricExporter, TelemetryInitError> {
    let endpoint = config
        .endpoint
        .as_ref()
        .expect("metric exporter is built only with a configured endpoint");
    let exporter = OtlpMetricExporter::builder()
        .with_http()
        .with_endpoint(signal_endpoint(endpoint.as_str(), "metrics"))
        .with_protocol(Protocol::HttpBinary)
        .with_timeout(timeout)
        .with_headers(export_headers(config))
        .with_retry_policy(RetryPolicy::disabled())
        .with_temporality(opentelemetry_sdk::metrics::Temporality::Cumulative)
        .build()
        .map_err(|_| TelemetryInitError::ExporterConfiguration)?;
    Ok(SafeMetricExporter(exporter))
}

fn export_headers(config: &TelemetryConfig) -> HashMap<String, String> {
    config
        .headers
        .entries_for_exporter()
        .iter()
        .map(|(name, value)| {
            // The OTLP builder decodes values using form-url-encoding. Encode
            // our already-validated values once so percent sequences remain
            // literal and FERL values retain their documented raw semantics.
            let value = url::form_urlencoded::byte_serialize(value.as_bytes()).collect::<String>();
            (name.as_str().to_owned(), value)
        })
        .collect()
}

fn signal_endpoint(base: &str, signal: &str) -> String {
    format!("{}/v1/{signal}", base.trim_end_matches('/'))
}

async fn run_metric_exporter(
    reader: Arc<ManualReader>,
    exporter: Arc<SafeMetricExporter>,
    cancellation: CancellationToken,
    export_timeout: Duration,
) {
    let mut schedule = interval(METRIC_SCHEDULE);
    schedule.tick().await;
    loop {
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => return,
            _ = schedule.tick() => {
                let mut data = ResourceMetrics::default();
                if reader.collect(&mut data).is_ok()
                    && data.scope_metrics().next().is_some()
                    && timeout(export_timeout, exporter.export(&data)).await.is_err()
                {
                    diagnostics::export_failure(Signal::Metrics);
                }
            }
        }
    }
}
