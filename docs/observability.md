# Observability

## Providers and signal boundaries

`ferl-server` uses the resolved OpenTelemetry 0.33.1 API, SDK, and OTLP crates,
`tracing-opentelemetry` 0.34.0, and the locked reqwest/rustls HTTP client. OTLP is
HTTP/protobuf. The lockfile is unchanged from the prior increment. SDK
`experimental_metrics_custom_reader`,
`experimental_trace_batch_span_processor_with_async_runtime`, and
`experimental_logs_batch_log_processor_with_async_runtime` SDK features are
enabled on that same release, along with OTLP `internal-logs`: the async-runtime
processors are required for the async reqwest exporters, the manual metric reader
lets ferl own a cancellable periodic exporter task, and internal SDK events are
needed to observe queue-full diagnostics. The `opentelemetry-appender-tracing` crate remains in the
locked dependency set but is intentionally not used: its default visitor copies
arbitrary tracing fields. `SafeOtelLogLayer` is the production LogRecord bridge.

All three providers receive the same explicit resource. Resource detection starts
from `Resource::builder_empty()`; SDK environment, host, process, and container
detectors are not invoked. No OpenTelemetry global tracer, meter, or logger
provider is installed. The application owns the providers and shutdown sequence;
the one-time production subscriber and instrument handles are process scoped.
Provider composition is verified in a child `ferl-server` process, keeping global
subscriber/provider state isolated from parallel Rust tests.

The resource contains:

| Attribute | Policy |
| --- | --- |
| `service.name` | `ferl` by default; `OTEL_SERVICE_NAME` overrides `FERL_OTEL_SERVICE_NAME`, which overrides the default. |
| `service.version` | `CARGO_PKG_VERSION`, injected at build time; never operator supplied. |
| `service.instance.id` | A new random opaque UUID for each process run. `FERL_INSTANCE_ID` and resource-attribute overrides are rejected. |
| `deployment.environment.name` | `OTEL_RESOURCE_ATTRIBUTES` then `FERL_OTEL_RESOURCE_ATTRIBUTES`; otherwise `FERL_ENVIRONMENT` (default `local`). |
| `deployment.name`, `service.namespace`, `cloud.region` | Optional allowlisted deployment metadata. Standard OTEL resource values override matching FERL values. |

Resource metadata values are at most 64 ASCII letters, digits, `.`, `_`, or `-`.
Credentials, URLs, IPs, host/process names, arbitrary resource keys, and
`service.*` identity overrides are not accepted. The standard OTEL resource
attribute parser is not delegated to an SDK environment detector.

## Configuration precedence and export

Standard OTEL settings take precedence over their FERL compatibility settings:

| Standard setting | FERL setting / behavior |
| --- | --- |
| `OTEL_SERVICE_NAME` | `FERL_OTEL_SERVICE_NAME`; default `ferl`. |
| `OTEL_RESOURCE_ATTRIBUTES` | `FERL_OTEL_RESOURCE_ATTRIBUTES`; standard keys override the same FERL keys. |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | `FERL_OTEL_ENDPOINT`; one shared HTTP(S) base URL, then `/v1/traces`, `/v1/metrics`, or `/v1/logs` is appended. |
| `OTEL_EXPORTER_OTLP_HEADERS` | `FERL_OTEL_HEADERS`; standard header setting replaces the FERL setting. Standard values use OTEL percent decoding. Header content is never logged. |
| `OTEL_EXPORTER_OTLP_TIMEOUT` | `FERL_OTEL_TIMEOUT_SECS`; standard value is milliseconds, FERL value is seconds. |
| `OTEL_TRACES_SAMPLER_ARG` | `FERL_OTEL_TRACE_SAMPLE_RATIO`; standard value wins. |
| `OTEL_TRACES_SAMPLER` | Only `parentbased_traceidratio` is accepted. Its root ratio defaults to `1.0`. |
| `OTEL_EXPORTER_OTLP_PROTOCOL` | Only `http/protobuf` is accepted. |

Signal-specific endpoint/header/timeout/protocol/compression variables and OTLP
compression settings are rejected. The exporter builders also receive
programmatic, already-validated settings so SDK environment readers cannot
silently override them. Endpoint syntax, header names/values, timeout ranges,
resource policy, and sampling settings are checked before serving. Validation
does not perform DNS lookups or contact a Collector. Exporter-construction
errors are mapped to a static category and never include the endpoint or headers.

Without an endpoint, ferl keeps JSON stdout and records in-process instruments,
but it creates no OTLP exporters, no metric export task, and no default collector
connection. It does not silently fall back to `localhost:4317/4318`.

### Trace context and correlation

The HTTP envelope assigns a fresh server request ID and starts an
`http.server.request` span with a fixed route template, bounded method, and that
request ID. It uses `Future::instrument`, not a thread-local `enter()` guard over
an `await`, so context follows Tokio task polling. Completion logs contain the
fresh request ID and active trace/span IDs; these identifiers are never metric
dimensions.

Only W3C `traceparent` and `tracestate` are read, and only when each header has one
value. The SDK W3C propagator validates both. A valid remote parent is retained
for parent-based sampling and parent IDs. Valid tracestate is parsed and then
discarded before span creation because it is opaque header data; malformed
context follows the propagator's invalid-context behavior and becomes a root
span, without logging raw values. Baggage and all other incoming headers are
ignored. Analytics events retain no request context or request span; worker spans
are independent and never keep request spans alive in the queue.

Parent-based sampling uses root ratio `1.0` by default. Sampled remote parents
remain sampled, unsampled remote parents remain unsampled, and root ratio zero
suppresses root spans. Metrics and sanitized log records are not derived from
sampled spans, so they continue at root ratio zero.

### Safe traces and LogRecords

The trace layer exports application spans only; tracing events are disabled for
that layer so event fields cannot become span events. Source file/module/thread
metadata and inactivity attributes are disabled. Service operations use
`skip_all` spans with fixed names. No Axum, SQLx, or HTTP-client automatic
instrumentation that captures URL, headers, body, SQL, or query arguments is
enabled.

The stdout formatter remains the existing structured JSON sink. The OTLP log
layer copies only the explicit field allowlist (`category`, `operation`,
`outcome`, `request_id`, `trace_id`, `span_id`, `method`, `route`, bounded status
and retry/queue integers, approved setting names, and schema status). It replaces
the body with the fixed text `ferl application event`; arbitrary messages,
exceptions, `Debug` values, and dependency error chains are discarded. The
application target allowlist excludes SQLx, reqwest, and other dependencies from
stdout, traces, and LogRecords.

OpenTelemetry SDK internal events are not exported as application logs. A
separate diagnostic layer reads only their fixed event name, maps it to one of
`otel_export_failed` or `otel_export_queue_full` plus a signal (`traces`,
`metrics`, or `logs`), and writes one structured JSON line directly to stdout.
One bounded six-slot rate limiter allows at most one diagnostic per category and
signal per 10 seconds. Error strings, URLs, headers, stack traces, and exporter
configuration are never read by the diagnostic visitor. This path does not use
tracing and cannot recursively export itself. A final privacy gate checks stdout,
stderr, OTLP spans/events/statuses, LogRecords, resources, and metric dimensions.

## Metric schema

All metrics are synchronous SDK instruments and use cumulative temporality and
sum aggregation, except the current-value queue gauge (last-value aggregation)
and the two explicit-bucket histograms. Default SDK histogram buckets for both
duration histograms are `[0, 5, 10, 25, 50, 75, 100, 250, 500, 750, 1000, 2500,
5000, 7500, 10000]` milliseconds, with min/max recorded. There are no exemplars
or trace-ID labels.

| Name | Type / unit | Dimensions | Exact counting rule |
| --- | --- | --- | --- |
| `ferl.http.server.request.count` | Counter `{request}` | `route`, `method`, `status_class`, `outcome` | Every request entering the bounded HTTP middleware, including overload responses; response completion is counted once, and cancellation is counted as `cancelled`. |
| `ferl.http.server.request.duration` | Histogram `ms` | same as request count | Wall-clock time from middleware entry to response completion/cancellation. |
| `ferl.http.server.active_requests` | UpDownCounter `{request}` | `route`, `method` | RAII count for requests in the outer HTTP pipeline, including bounded waiters. Drop releases the count on cancellation. |
| `ferl.redirect.outcomes` | Counter `{operation}` | fixed `operation=redirect.follow`, `outcome` | A valid public short-code resolution result; `success` means the 302 is constructed, not that the client followed it. |
| `ferl.link.creation.outcomes` | Counter `{operation}` | fixed `operation=link.create`, `outcome` | Link service attempts after committed rate admission and body validation. `success` means the mutation service committed; pre-service validation is excluded. |
| `ferl.link.creation.rate_rejections` | Counter `{attempt}` | none | A creation attempt denied by the durable rolling allowance; the rejected attempt itself is not a new charge. |
| `ferl.authentication.failures` | Counter `{failure}` | finite `category` | Missing/invalid credentials, DB lookup availability/integrity errors, and explicit forbidden scopes. Concealed 404 object access is not a forbidden label. |
| `ferl.database.operation.duration` | Histogram `ms` | fixed sanitized `operation`, `outcome` | Timer scope is the documented repository/guard/readiness operation; no hidden pool checkout is added. Errors/cancellation are non-success. |
| `ferl.database.operation.failures` | Counter `{operation}` | `operation`, `outcome` | Every timed DB operation that does not return success, including cancellation/timeouts. |
| `ferl.analytics.events` | Counter `{event}` | fixed `phase`, `outcome` | `enqueue/admitted` means queue admission; `persist/committed` means durable insert+counter commit; `persist/duplicate`, `drop/link_deleted`, `drop/queue_full`, `drop/producer_closed`, `retry/attempt`, persistence failure, and shutdown drops are distinct. |
| `ferl.analytics.queue.depth` | Gauge `{event}` | none | Current in-memory queued events, not active worker events. A serialized atomic ownership update and shutdown reset prevent drift. |
| `ferl.telemetry.log_records` | Counter `{log_record}` | bounded `severity` | Sanitized application LogRecords submitted to the SDK bridge, not Collector acknowledgements. |

Known routes use stable templates, including `/{short_code}` and
`/api/v1/links/{code}`; unmatched paths use a fixed marker. Unsupported method
tokens map to `OTHER`. No request ID, trace/span ID, short code, account/link/
visit identity, URL, query, IP, user-agent, title, tag, or cursor can be a metric
dimension.

Database operation vocabulary is `credential.lookup`, `redirect.lookup`,
`link.create`, `account.mutation`, `link.mutation`, `report.query`,
`analytics.persist`, and `health.readiness_probe`. A report operation timer wraps
the existing read-only repeatable-read transaction; it does not change schema v2,
authorization order, timeout `FERL_REPORT_OPERATION_TIMEOUT_SECS`, cursor
handling, or snapshot semantics.

## Buffer, retry, and shutdown bounds

The async trace and log processors each use a finite queue (`FERL_OTEL_QUEUE_CAPACITY`,
default 1,024; maximum 65,536), maximum batch (`FERL_OTEL_BATCH_SIZE`, default
128), one-second schedule, and one export at a time. The metric exporter has one
one-second periodic task and a `ManualReader`, so it has no growing export queue
and at most one metric request in flight. All signals use OTLP cumulative
temporality. OTLP retries are explicitly disabled (zero retries per batch); later
scheduled metric collections and later trace/log batches are new best-effort
attempts. A failed trace/log batch is dropped, not replayed unboundedly.

Effective per-request exporter timeout is the minimum of configured timeout,
one quarter of the total shutdown timeout, and the calculated queue-drain budget
`total_shutdown / (8 * (ceil(queue_capacity / batch_size) + 1))` (with a 1µs
floor). Export concurrency is one. SDK asynchronous processors are used because
the pinned HTTP exporters are async; they avoid synchronous `block_on` requests
without a Tokio reactor. Provider shutdown is moved to the blocking pool only
after a calculated upper bound is available. If the total deadline no longer has
that budget, provider flush is skipped rather than using an SDK five-second Drop
default. Tokio runtime teardown cancels remaining async exporter tasks. Abrupt
process failure can lose buffered traces, logs, metrics, and in-memory visits.

Readiness requires accepting state, verified schema, and a bounded PostgreSQL
probe; it does not depend on exporter health or analytics backlog. `operations.md`
documents the actual Collector fixture and process-exit measurements.
