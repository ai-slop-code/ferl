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
| `FERL_TRUSTED_PROXIES` | Empty | Comma-separated IP addresses/CIDRs. Empty entries and malformed ranges are rejected; private ranges are not trusted automatically. |
| `FERL_CURSOR_KEY` | Required | Canonical unpadded base64url encoding of exactly 32 bytes. Keep stable across restarts and instances; do not generate an instance-local fallback. Part 03 cursor tokens use the authenticated `fc1<resource>.` envelope and up to 87,387 encoded bytes. |
| `FERL_DB_POOL_MIN` | `0` | Integer from 0 through `FERL_DB_POOL_MAX`. |
| `FERL_DB_POOL_MAX` | `10` | Integer from 1 through 128 connections per instance. |
| `FERL_DB_CONNECT_TIMEOUT_SECS` | `5` | Integer from 1 through 300 seconds. |
| `FERL_DB_ACQUIRE_TIMEOUT_SECS` | `2` | Integer from 1 through 300 seconds; bounds readiness acquisition and pool waits. SQLx 0.9 exposes one pool acquisition deadline, so the shorter of connect/acquire timeouts bounds both waiting for a connection and creating one. |
| `FERL_DB_STATEMENT_TIMEOUT_SECS` | `5` | Integer from 1 through 300 seconds, applied to each new PostgreSQL connection. |
| `FERL_DB_LOCK_TIMEOUT_SECS` | `2` | Integer from 1 through 300 seconds, applied to each new PostgreSQL connection. |
| `FERL_ANALYTICS_QUEUE_CAPACITY` | `2048` | Integer from 1 through 65,536; reserved for the bounded worker added in Part 07. |
| `FERL_ANALYTICS_WORKERS` | `4` | Integer from 1 through 64; reserved for Part 07. |
| `FERL_ANALYTICS_RETRY_BUDGET` | `3` | Integer from 1 through 10; reserved for Part 07. |
| `FERL_ANALYTICS_DRAIN_TIMEOUT_SECS` | `5` | Integer from 1 through 300 seconds; reserved for Part 07. |
| `FERL_HTTP_DRAIN_TIMEOUT_SECS` | `30` | Integer from 1 through 300 seconds. |
| `FERL_SHUTDOWN_TIMEOUT_SECS` | `45` | Integer from 1 through 300 seconds and at least the HTTP drain timeout. |
| `FERL_OTEL_ENDPOINT` | Unset | Optional HTTP(S) OTLP endpoint. Its absence disables export; collector reachability is never required at startup. |
| `FERL_OTEL_HEADERS` | Empty | Optional comma-separated `name=value` pairs. Header names/values are validated; commas are not supported inside values. Values are secret and are never logged. |
| `FERL_OTEL_TIMEOUT_SECS` | `5` | Integer from 1 through 300 seconds. |
| `FERL_OTEL_TRACE_SAMPLE_RATIO` | `1.0` | Finite number from 0 through 1; parent-based sampling is wired in Part 10. |
| `FERL_OTEL_RESOURCE_ATTRIBUTES` | Empty | Optional comma-separated `key=value` pairs; keys use ASCII alphanumeric, `.`, `_`, and `-`. Values must be non-sensitive, low-cardinality deployment metadata. |
| `FERL_ENVIRONMENT` | `local` | Non-empty, at most 128 UTF-8 bytes, with no control characters. |
| `FERL_INSTANCE_ID` | Random UUID per process | Optional UUID override. Use a distinct value per instance. |

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

`FERL_OTEL_ENDPOINT` and the OTLP resource settings are parsed now so malformed
configuration fails before serving. Part 01 installs safe structured stdout
logging and a typed, low-cardinality operation/outcome facade. Export providers
and actual traces, metrics, and OTLP logs are delivered in Part 10. Analytics
settings are similarly validated now and consumed by the analytics worker in
Part 07.

API request bodies have a fixed 65,536-byte maximum; this limit is not an
environment override.

The serving process starts with liveness available and readiness false until
the Part 02 background verifier confirms the embedded migration history and
required schema objects. It rechecks on a bounded interval so database/schema
recovery becomes ready without restart; readiness also requires a bounded
database probe and acceptance of work. A successful `SELECT 1` alone does not
imply schema readiness. Run `ferl-server migrate` explicitly before rollout;
the server never runs migrations or bootstrap implicitly at startup.
