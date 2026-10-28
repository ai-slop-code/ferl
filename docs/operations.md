# Operations: Collector and graceful shutdown

## Local Collector (Podman)

The reproducible capture harness uses the fully qualified, multi-architecture
Collector image
`docker.io/otel/opentelemetry-collector-contrib@sha256:340c9ca6a36617084c6661bdc4f0fe92b7cda5a805d61673ec8db375fddc522b`
(Collector 0.164.0, linux/arm64 manifest verified in the authorized Podman VM).
The checked-in configuration is [`deploy/collector/otelcol.yaml`](../deploy/collector/otelcol.yaml):
OTLP HTTP binds `0.0.0.0:4318` inside the container, traces/metrics/logs all
use a memory-limited and batched pipeline, and `file/ferl` writes machine-readable
OTLP JSON with 8 MiB rotation and at most two backups (about 24 MiB including the
active file). The health-check extension is internal to the container.

Verify Podman before use; on macOS, the existing machine must be running:

```sh
podman info
podman machine list
podman machine start podman-machine-default  # only if it is stopped
```

Start and inspect the fixture using one stable run ID and loopback port:

```sh
export FERL_COLLECTOR_RUN_ID=manual
export FERL_COLLECTOR_PORT=4318
sh scripts/collector.sh pull
sh scripts/collector.sh start
sh scripts/collector.sh ready
sh scripts/collector.sh capture
```

On macOS, a host-run `ferl-server` reaches that container through the published
host loopback port, for example:

```sh
FERL_OTEL_ENDPOINT=http://127.0.0.1:4318 \
FERL_OTEL_HEADERS= \
cargo run -- serve
```

`ready` is not a container-running check: it waits up to 30 seconds for an OTLP
HTTP trace request to be accepted and for the file exporter to capture it. The
one synthetic `ferl-collector-probe` resource is intentionally separate from
ferl process resources. Process tests filter it by `service.name`.

Stop/restart preserves the capture file. `capture` prints at most the first 1 MiB
per file; tests read the bounded files directly and decode every complete
newline-delimited OTLP JSON record. Cleanup removes only the label-verified
container; it intentionally preserves capture files for inspection:

```sh
sh scripts/collector.sh stop
sh scripts/collector.sh restart
sh scripts/collector.sh ready
sh scripts/collector.sh cleanup
```

`FERL_COLLECTOR_NAME`, `FERL_COLLECTOR_PORT`, `FERL_COLLECTOR_RUN_ID`, and
`FERL_COLLECTOR_CAPTURE_DIR` are configurable. Each reproducible gate run uses a
unique run ID, container name, ephemeral loopback host port, and capture
directory; concurrent runs do not share capture files. The no-endpoint process
test separately watches the SDK default `localhost:4318` port with a loopback
sentinel, or checks the real Collector capture directly when the Collector itself
is bound there, to detect accidental default-exporter connections. If a different
service occupies 4318, free that port before the full gate. Podman pulls in this environment use `--tls-verify=false` only for the
explicitly authorized pinned image pull; ferl HTTP/HTTPS and PostgreSQL TLS
verification are unchanged.

Run the mandatory PostgreSQL + Collector + production-process verification with:

```sh
sh scripts/check.sh
```

The gate starts the existing locked PostgreSQL 17.6 fixture and the Collector,
waits for useful readiness, exports `FERL_TEST_OTEL_ENDPOINT` and the isolated
capture path, runs all Rust gates including `tests/telemetry_signals.rs`, and
stops only its Collector container on exit. It leaves capture files for audit.
The process suite exercises real `ferl-server` TCP processes, not the in-process
router as export evidence.

## Shutdown and health

On SIGTERM/SIGINT and unexpected server exit, ferl marks readiness false first.
With a configured nonzero `FERL_READINESS_WITHDRAWAL_SECS` (default 1, maximum
10), it continues polling the listener during that interval so liveness and the
exact readiness response remain observable. It then signals Axum graceful
shutdown, which stops acceptance and gives active requests the remaining part of
`FERL_HTTP_DRAIN_TIMEOUT_SECS` (default 30). On deadline, ferl drops the Axum
accept future and cancels remaining application handler futures through the
admission token; their envelope emits sanitized `service_unavailable` when the
transport still permits it and releases active-count guards. Already-started
connection response writes may remain until Tokio runtime teardown. Health routes
may no longer be reachable after acceptance stops.

After HTTP handling ends or reaches its bound, analytics producers close and
workers drain the bounded in-memory visit queue up to
`FERL_ANALYTICS_DRAIN_TIMEOUT_SECS` (default 5), limited by the same remaining
`FERL_SHUTDOWN_TIMEOUT_SECS` total deadline (default 45, maximum 300). The
schema monitor and pool close are also limited by that deadline. Metric export is
cancelled, one final collection is attempted when time remains, then trace/log
providers flush within their precomputed queue/batch/export bounds. If the total
budget cannot cover a signal provider's calculated bound, that best-effort flush
is skipped; the process does not invoke an SDK Drop default that can extend the
deadline.

When HTTP exceeds budget, outstanding handler futures are cancelled before
analytics producers close; already-started connection response writes may remain
until runtime teardown. A late enqueue after producer closure is dropped and
counted where possible. Worker work beyond its bound is aborted; the queue depth
gauge is reset and remaining visits are counted as shutdown drops. Exporters use
finite one-at-a-time HTTP requests and zero per-batch retries; slow Collector
requests are cancelled at their timeout, queue-full data is discarded by the SDK,
and later periodic batches may recover. Crash/kill/power loss can lose buffered
analytics and telemetry before any drop can be recorded. Graceful flush is best
effort, not crash durability.

Readiness requires successful initialization/schema verification, a bounded
PostgreSQL probe, and accepting state. It is independent of Collector health and
analytics queue depth. Liveness has no database or Collector dependency. A failed
completed readiness probe returns exactly `{"status":"unavailable"}`; global
admission overload remains the structured `service_unavailable` error.

Executable lifecycle evidence is in
`tests/telemetry_signals.rs::production_otlp_privacy_sampling_outage_drain_and_replacement_are_verified`.
The test observes readiness 503 and liveness 200 over TCP before listener
removal, stops/restarts the real Collector, holds OTLP responses in a controlled
loopback receiver while flooding HTTP and stalling an HTTP body, observes a real
PostgreSQL outage/recovery, and measures SIGTERM process exit under the configured
total deadline.

One local macOS/Podman 6.1.4 observation (2026-10-29, not an SLO) measured
SIGTERM-to-exit at 1.007–1.019s for ordinary, no-endpoint, and PostgreSQL-outage
processes; 4.020s for the held-body readiness-withdrawal case; and 15.780s with a
held HTTP body plus stalled OTLP receiver under a configured 60s total deadline.
The test observed readiness withdrawal at 4ms and listener removal at 2.022s.
These are fixture measurements, not production capacity or latency guarantees.
