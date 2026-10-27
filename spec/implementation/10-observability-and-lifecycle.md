# Part 10 — OpenTelemetry, telemetry boundaries and graceful lifecycle

**Dependencies:** part 09. Earlier parts already emit safe structured stdout and typed instrumentation events.

**Normative references:** business sections 3, 9.2, 13.1, 14 and acceptance scenarios 29–31, 36, 50.

## Outcome

The server emits correlated traces, metrics and logs over OTLP HTTP/protobuf when configured, retains structured stdout logging, and remains functional under exporter/backlog failures. Readiness/draining and shutdown have bounded behavior.

## Files and responsibilities

- `src/telemetry/{providers,traces,metrics,logs,attributes,diagnostics}.rs`.
- Finish `src/shutdown.rs`, runtime/worker/provider composition and health state wiring.
- `tests/support/collector.rs` or an external collector fixture with machine-readable capture.
- `tests/{telemetry_signals,telemetry_privacy,collector_failures,shutdown}.rs`.
- `docs/{observability,operations,configuration}.md`, a local collector example.

## Tasks

### 1. Configure real providers for all three signals

Wire mutually compatible OpenTelemetry SDK trace, metric and log providers and OTLP HTTP/protobuf exporters. A tracing subscriber alone is not an OTLP log exporter; verify actual LogRecord export in addition to spans and metrics.

Run the local OpenTelemetry Collector fixture with Podman. Pin a fully qualified collector image and mount a checked-in configuration enabling OTLP HTTP on the container's `0.0.0.0:4318`, with a configurable loopback-published host port. Configure pipelines for traces, metrics and logs and a machine-readable capture exporter supported by the chosen image. Document how the host-run ferl process reaches it, how readiness is checked, and how captured signals are read. Reuse this fixture for stop/restart and outage tests; use a controlled stalled receiver/proxy where a test specifically needs backpressure rather than immediate connection refusal.

Use `service.name=ferl` by default, build-derived service.version, and one opaque service.instance.id generated per process run. Support documented standard environment/resource settings, endpoint, export headers, timeout and sampling configuration. Validate static syntax without requiring network reachability.

Parent-based trace sampling defaults to root ratio 1.0. Respect valid W3C traceparent/tracestate context; do not propagate arbitrary baggage. Tie request logs to request ID and trace/span IDs where available. Analytics worker spans may link to the original request without extending its lifetime through a long queue wait.

Without an endpoint, attempt no OTLP export and keep JSON stdout. Metrics and error logs must still be produced independently of whether a trace was sampled. Startup and bootstrap secret output must not pass through telemetry fields.

### 2. Instrument the specified work

Use explicit operation spans for HTTP handling, database work, link operations, credential operations and analytics processing. Establish a small stable attribute/outcome vocabulary and document metric names/types/units.

Required metrics include:

- HTTP count, latency histogram and in-flight count by safe route template/method/status category.
- Redirect outcomes and link creation outcomes/rate rejections.
- Authentication/authorization failure categories.
- Database error count and latency by sanitized operation category.
- Analytics enqueue/persist/drop/failure counts and queue depth.

Include lifecycle, request-completion and sanitized error logs. Use route templates for known operations, a fixed unmatched marker for unknown paths and bounded method values (map arbitrary unsupported method tokens to an `other` category). Do not label metrics with request IDs or object/actor identity. service.instance.id belongs to the required process resource, not an application-generated per-request dimension.

### 3. Explicit telemetry allowlist

Audit application instrumentation and automatic HTTP/database layers. Disable capture of URL/path/query/address/header/body data and SQL statements/parameters. Prevent library errors from formatting excluded values indirectly. Use typed safe classifications instead of raw `Display`/`Debug` error chains.

Never emit:

```text
API keys, management tokens or their digests; Authorization;
database connection strings, cursor/export secrets, export headers;
request/response bodies or full request/destination/referrer URLs;
historical redirect_url values; query/search/cursor strings;
titles, tag names, client IPs, forwarded headers or raw user agents;
actual short-code paths or account/link/visit IDs as metric labels.
```

Keep detailed visit fields exclusively in PostgreSQL and authorized responses. Resource attributes must follow the same safe-data policy; document permitted deployment metadata and reject/reserve unsafe automatic capture settings. Correlation IDs are allowed in logs/traces, not metric labels.

### 4. Bounded export and failure diagnostics

Configure finite queues/batches, timeouts/retries and shutdown flush deadlines for providers and log bridges. Slow or unreachable collectors cannot block handlers, turn successful redirects into errors, or grow memory without bound.

Exporter diagnostics go through a rate-limited sanitized stdout path that does not recursively export its own errors. Queue-full/log-export failures remain bounded even when errors themselves are frequent. Document that abrupt termination can lose buffered analytics/telemetry before loss is measured.

### 5. Drain and health behavior

Complete signal-driven shutdown:

1. Transition accepting-work state to draining; readiness becomes 503 immediately while liveness remains available during the readiness/drain transition.
2. Stop accepting new application work/connections according to the server's drain strategy and allow bounded in-flight completion.
3. Close analytics producers after in-flight responses finish, then attempt a bounded queue drain.
4. Flush/shut down providers within their remaining deadlines; close resources and exit within the configured total bound.

Provide a short, bounded readiness-withdrawal phase before closing the listener, or an equivalent testable server lifecycle mechanism, so load balancers/tests can observe unready before removal. Keep health probes dependency-specific: readiness depends on initialization/schema/DB/accepting state, not queue depth or collector status. Document the shutdown timing settings and ordering.

## Verification gate

- Capture OTLP and stdout for representative success/error account/link/redirect/report flows. Prove traces, metrics and LogRecords exist with expected correlation and safe resources.
- Send a valid remote trace context and verify parent behavior; confirm arbitrary baggage is not carried into downstream work/export attributes.
- Change root sampling to zero and verify sampled root traces disappear while metrics and error logs remain. Test parent-based behavior independently.
- Inject distinctive canary secrets, destination/referrer URLs, query/title/tag strings, IPs and raw UAs. Exercise validation/database/parser/export errors and inspect every exported signal plus stdout. No excluded canary or raw automatic HTTP/SQL field may appear.
- Saturate or black-hole the collector and verify API/redirect success, bounded buffers/retries/diagnostics and readiness independent of the collector.
- Make PostgreSQL unavailable and verify readiness 503 with exact health body, liveness 200 and dependent request failures 503.
- Observe draining readiness before removal; stall analytics/exporters and prove shutdown exits within a deadline. Verify a healthy bounded drain persists queued visits where feasible without claiming crash durability.

**Handoff:** record metric schema, exporter versions/bounds, privacy-capture results and exact shutdown timings for part 11. Main scenarios: 25, 29–31, 36 and 50.
