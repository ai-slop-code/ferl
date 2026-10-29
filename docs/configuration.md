# Server configuration

`ferl-server` reads configuration from the process environment at startup. The
`serve` command requires `FERL_DATABASE_URL`, `FERL_PUBLIC_BASE_URL`, and
`FERL_CURSOR_KEY`. The explicit `migrate` command has a command-specific
configuration profile: it requires only `FERL_DATABASE_URL` and applicable
`FERL_MIGRATE_*` settings, and does not parse serving-only variables.
`bootstrap-admin` has a separate profile requiring only the database URL and
the bounded `FERL_BOOTSTRAP_*` settings below. It does not parse serving-only
origin, cursor, listener, proxy, analytics, or OTLP settings. Invalid values
fail with only the setting name and a static reason in diagnostics. Secret
values are never included in configuration debug output or logs.

| Variable | Required/default | Encoding and bounds |
| --- | --- | --- |
| `FERL_DATABASE_URL` | Required | PostgreSQL primary/read-write URL. Treat as secret. |
| `FERL_PUBLIC_BASE_URL` | Required | HTTP(S) origin only; no user info, path prefix, query, or fragment. A trailing `/` is accepted and removed. |
| `FERL_LISTEN_ADDR` | `0.0.0.0:8080` | Rust socket address (`IP:port`). |
| `FERL_HTTP_MAX_IN_FLIGHT` | `256` | Maximum concurrently serviced HTTP requests per instance, integer from 1 through 4,096. |
| `FERL_HTTP_MAX_WAITING` | `256` | Maximum admitted requests waiting for an in-flight slot, integer from 0 through 4,096. Requests above the combined in-flight and waiting capacity fail fast with 503 `service_unavailable`. |
| `FERL_ANONYMOUS_CREATION` | `false` | Case-insensitive `true` or `false`. |
| `FERL_ANONYMOUS_CREATE_LIMIT` | `5` | Positive integer, maximum 1,000,000 attempts per rolling 60 seconds. |
| `FERL_ACCOUNT_CREATE_LIMIT` | `60` | Positive integer, maximum 1,000,000 attempts per account per rolling 60 seconds (admins included). |
| `FERL_CREATION_TIMEOUT_SECS` | `15` | Integer from 1 through 300 seconds; total authentication, eligibility, and independently committed allowance deadline. |
| `FERL_LINK_OPERATION_TIMEOUT_SECS` | `30` | Integer from 1 through 300 seconds; bounds each link-service operation and tag-discovery operation, independent of body, pool, lock, and SQL statement bounds. |
| `FERL_REPORT_OPERATION_TIMEOUT_SECS` | `30` | Integer from 1 through 300 seconds; bounds each complete analytics report request, including fresh authentication, authority checks, cursor handling, and report SQL. Report dispatch is outside the account API's fixed operation wrapper. |
| `FERL_TRUSTED_PROXIES` | Empty | Comma-separated IP addresses/CIDRs. Empty entries and malformed ranges are rejected; private ranges are not trusted automatically. |
| `FERL_CURSOR_KEY` | Required | Canonical unpadded base64url encoding of exactly 32 bytes. Keep stable across restarts and instances; do not generate an instance-local fallback. Part 03 cursor tokens use the authenticated `fc1<resource>.` envelope and up to 87,387 encoded bytes. |
| `FERL_DB_POOL_MIN` | `0` | Integer from 0 through `FERL_DB_POOL_MAX`. |
| `FERL_DB_POOL_MAX` | `10` | Integer from 1 through 128 connections per instance. |
| `FERL_DB_CONNECT_TIMEOUT_SECS` | `5` | Integer from 1 through 300 seconds. |
| `FERL_DB_ACQUIRE_TIMEOUT_SECS` | `2` | Integer from 1 through 300 seconds; bounds readiness acquisition and pool waits. SQLx 0.9 exposes one pool acquisition deadline, so the shorter of connect/acquire timeouts bounds both waiting for a connection and creating one. |
| `FERL_DB_STATEMENT_TIMEOUT_SECS` | `5` | Integer from 1 through 300 seconds, applied to each new PostgreSQL connection. |
| `FERL_DB_LOCK_TIMEOUT_SECS` | `2` | Integer from 1 through 300 seconds, applied to each new PostgreSQL connection. |
| `FERL_ANALYTICS_QUEUE_CAPACITY` | `2048` | Integer from 1 through 65,536; bounded in-memory visit events per instance. Queue-full events are dropped without delaying redirects. |
| `FERL_ANALYTICS_WORKERS` | `4` | Integer from 1 through 64 concurrent visit workers per instance. |
| `FERL_ANALYTICS_RETRY_BUDGET` | `3` | Integer from 1 through 10 retries after the initial persistence attempt. Transient/ambiguous visit writes reuse the same event UUID and snapshot. |
| `FERL_ANALYTICS_OPERATION_TIMEOUT_SECS` | `30` | Integer from 1 through 300 seconds; total bound for each worker persistence attempt, independent of pool, lock and statement deadlines. Must exceed the rollback timeout. |
| `FERL_ANALYTICS_ROLLBACK_TIMEOUT_SECS` | `2` | Integer from 1 through 300 seconds; bounds explicit rollback after a failed visit transaction. |
| `FERL_ANALYTICS_DRAIN_TIMEOUT_SECS` | `5` | Integer from 1 through 300 seconds; maximum visit-queue drain before worker cancellation and pool closure. |
| `FERL_READINESS_WITHDRAWAL_SECS` | `1` | Integer from 0 through 10 seconds. Readiness fails immediately; during this interval the listener continues polling health routes before acceptance is stopped. |
| `FERL_HTTP_DRAIN_TIMEOUT_SECS` | `30` | Integer from 1 through 300 seconds. |
| `FERL_SHUTDOWN_TIMEOUT_SECS` | `45` | Integer from 1 through 300 seconds and at least the HTTP drain timeout. This is one end-to-end signal/unexpected-exit deadline, not a per-phase timeout. |
| `FERL_OTEL_SERVICE_NAME` | `ferl` | Safe 1–64 character service identifier. `OTEL_SERVICE_NAME`, when present, takes precedence. |
| `FERL_OTEL_ENDPOINT` | Unset | Optional HTTP(S) OTLP/HTTP-protobuf base URL. Export is completely disabled when absent; collector reachability/DNS is not required at startup/readiness. Signal paths `/v1/{traces,metrics,logs}` are appended. `OTEL_EXPORTER_OTLP_ENDPOINT` takes precedence. |
| `FERL_OTEL_HEADERS` | Empty | Optional comma-separated `name=value` pairs. `OTEL_EXPORTER_OTLP_HEADERS`, when present, replaces this setting and is decoded using OTEL percent-encoding. Header values are secret and never logged. |
| `FERL_OTEL_TIMEOUT_SECS` | `5` | Integer from 1 through 300 seconds. `OTEL_EXPORTER_OTLP_TIMEOUT` takes precedence and is milliseconds (1–300,000). The effective request timeout is further capped by the total shutdown/batch budget. |
| `FERL_OTEL_TRACE_SAMPLE_RATIO` | `1.0` | Finite number from 0 through 1. Sampling is always parent-based; roots use this ratio. `OTEL_TRACES_SAMPLER_ARG` takes precedence, and only `parentbased_traceidratio` is accepted for `OTEL_TRACES_SAMPLER`. |
| `FERL_OTEL_QUEUE_CAPACITY` | `1024` | Integer from 1 through 65,536, shared per-process maximum for trace/log batch queues. |
| `FERL_OTEL_BATCH_SIZE` | `128` | Integer from 1 through the configured queue capacity; maximum spans/log records per batch. Trace export concurrency is 1; log export is serial. |
| `FERL_OTEL_RESOURCE_ATTRIBUTES` | Empty | Comma-separated allowlisted `key=value` deployment metadata. Only `deployment.environment.name`, `deployment.name`, `service.namespace`, and `cloud.region` are accepted; values are 1–64 ASCII letters/digits/`.`/`_`/`-`. `OTEL_RESOURCE_ATTRIBUTES` overrides duplicate keys. |
| `FERL_ENVIRONMENT` | `local` | Safe 1–64 character deployment-environment fallback for `deployment.environment.name`; an explicit allowlisted resource attribute wins. |
| `FERL_INSTANCE_ID` | Generated | Overrides are rejected. `service.instance.id` is a new opaque UUID generated for every process run and shared across all three signals. |

PostgreSQL TLS options are parsed by the pinned SQLx PostgreSQL driver from
`FERL_DATABASE_URL`. For private deployments use
`sslmode=verify-full&sslrootcert=/absolute/path/to/ca-bundle.pem`; the `verify-full`
mode verifies both the trusted certificate chain and the hostname in the URL.
The configured PEM CA is added to the driver's bundled web-PKI public roots.
Do not use `sslmode=disable` or weaken verification to work around a certificate
or hostname error. See [release TLS verification](release.md#runtime-tls-and-ca-trust)
for the OTLP HTTPS trust source as well.

## Migration command settings

`ferl-server migrate` validates its applicable static settings before opening a
database connection. These variables are ignored by `serve` and do not require
`FERL_PUBLIC_BASE_URL`, `FERL_CURSOR_KEY`, listener, proxy, or telemetry
configuration.

| Variable | Required/default | Encoding and bounds |
| --- | --- | --- |
| `FERL_DATABASE_URL` | Required | Valid PostgreSQL URL; treated as a secret. Its `search_path` must match serving. |
| `FERL_MIGRATE_CONNECT_TIMEOUT_SECS` | `10` | Integer from 1 through 3,600 seconds. |
| `FERL_MIGRATE_LOCK_TIMEOUT_SECS` | `30` | Integer from 1 through 3,600 seconds; bounds migration advisory-lock acquisition. |
| `FERL_MIGRATE_STATEMENT_TIMEOUT_SECS` | `120` | Integer from 1 through 3,600 seconds. |
| `FERL_MIGRATE_TOTAL_TIMEOUT_SECS` | `600` | Integer from 1 through 3,600 seconds and at least the connection and lock timeouts. |
| `FERL_MIGRATE_LOCK_POLL_MS` | `50` | Integer from 10 through 1,000 milliseconds. |

The supported PostgreSQL/schema versions, migration lock, and exact
incompatibility behavior are documented in [database.md](database.md).

## One-time bootstrap command settings

`ferl-server bootstrap-admin --username <username>` validates the exact
username and its command-specific settings before contacting PostgreSQL. It
requires a schema already migrated and compatible with the binary; it never
runs migrations. The transaction serializes on the account-authority gate and
the singleton `system_state` row. Only an acknowledged commit enables output.

| Variable | Default | Bound / behavior |
| --- | ---: | --- |
| `FERL_BOOTSTRAP_CONNECT_TIMEOUT_SECS` | `10` | Integer from 1 through 300 seconds. |
| `FERL_BOOTSTRAP_LOCK_TIMEOUT_SECS` | `5` | Integer from 1 through 300 seconds; bounds the exclusive authority gate. |
| `FERL_BOOTSTRAP_STATEMENT_TIMEOUT_SECS` | `10` | Integer from 1 through 300 seconds. |
| `FERL_BOOTSTRAP_TOTAL_TIMEOUT_SECS` | `30` | Integer from 1 through 300 seconds and at least the connection and lock bounds. |

On success, stdout contains exactly one JSON line using the `AccountCreation`
shape: `{"account":{...},"api_key":"ferl_ak_v1_..."}`. This is the only
plaintext account-key output. Bootstrap diagnostics go to stderr, separately
from the serving process's structured JSON stdout logs. Exit status is 0 for
success, 2 for invalid configuration/username, and 1 for schema, storage,
already-completed, or output failure. A failed output stream never reopens a
committed bootstrap; repeating the command does not print a replacement key.
If the result is lost after commit, recover through the operator's secure
database/backup procedure. There is intentionally no automatic reset or second
bootstrap path.

Generate a cursor key with
`python3 -c 'import base64,secrets; print(base64.urlsafe_b64encode(secrets.token_bytes(32)).rstrip(b"=").decode())'`.
Do not deploy the placeholder in `.env.example` or reuse a key across unrelated
deployments.

Cursor key changes invalidate previously issued cursors and must be coordinated
across instances. Part 03's exact cursor version, associated data, resource
codes, context binding, and decoded/encoded limits are documented in
[domain and wire contracts](domain-contracts.md).

Standard OTEL settings supported by `serve` are `OTEL_SERVICE_NAME`,
`OTEL_RESOURCE_ATTRIBUTES`, `OTEL_EXPORTER_OTLP_ENDPOINT`,
`OTEL_EXPORTER_OTLP_HEADERS`, `OTEL_EXPORTER_OTLP_TIMEOUT`,
`OTEL_EXPORTER_OTLP_PROTOCOL=http/protobuf`, `OTEL_TRACES_SAMPLER`, and
`OTEL_TRACES_SAMPLER_ARG`. When both setting families specify a value, standard
OTEL settings take precedence over their FERL compatibility equivalents.
Signal-specific endpoint/header/timeout/protocol/compression variables and
generic compression are rejected rather than being read implicitly by the SDK.
Unsupported resource attributes (including host/process/instance detector data)
are rejected. Resource attributes cannot override `service.name`, build-derived
`service.version`, or generated `service.instance.id`.

No DNS lookup or collector connection occurs during configuration/provider
construction. With no endpoint, the process keeps structured stdout and valid
in-process instruments but starts no exporter task and makes no OTLP attempt.
See [observability.md](observability.md) for SDK versions, metric schema, queue
bounds, sampling/correlation, and diagnostics. See [operations.md](operations.md)
for the pinned Collector and shutdown operations.

Compatibility correction: `FERL_INSTANCE_ID` was previously accepted as a UUID
override, and arbitrary `FERL_OTEL_RESOURCE_ATTRIBUTES` were accepted. Part 10
now rejects `FERL_INSTANCE_ID` and restricts resources to the explicit allowlist
above so process identity cannot be reused and environment/resource detectors
cannot introduce unreviewed host or sensitive metadata.

Analytics settings configure the bounded visit queue/workers, per-attempt and
rollback deadlines, idempotent retry budget, and graceful drain. Abrupt process
termination can lose in-memory visits and telemetry; neither is crash durable.

Part 09 adds `FERL_REPORT_OPERATION_TIMEOUT_SECS`, which bounds complete report
requests independently of the link-operation, account-wrapper, worker, pool, and
PostgreSQL statement deadlines. See [analytics report operations](analytics-reports.md)
for the snapshot boundary, v2/no-migration decision, and maximum-cursor transport
requirements.

API request bodies have a fixed 65,536-byte maximum; this limit is not an
environment override.

The serving process starts with liveness available and readiness false until
the Part 02 background verifier confirms the embedded migration history and
required schema objects. It rechecks on a bounded interval so database/schema
recovery becomes ready without restart; readiness also requires a bounded
database probe and acceptance of work. A successful `SELECT 1` alone does not
imply schema readiness. Run `ferl-server migrate` explicitly before rollout;
the server never runs migrations or bootstrap implicitly at startup.

Part 05 composes anonymous creation, both limits, proxy trust and database bounds
into production `CreationService`; changing business settings requires coordinated
fleet restart. [Creation allowance](creation-allowance.md) defines mapped IPv6
proxy-prefix comparisons, exact rolling accounting, cleanup and failure bounds.
The public creation route is delivered in Part 06.
