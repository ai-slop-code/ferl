# Server configuration

`ferl-server` reads configuration from the process environment at startup. The
`serve` command requires `FERL_DATABASE_URL`, `FERL_PUBLIC_BASE_URL`, and
`FERL_CURSOR_KEY`. The operator commands are deliberately unavailable until
their implementation parts; they fail without reading or changing database
state. Invalid values fail startup with only the setting name and a static
reason in diagnostics. Secret values are never included in configuration debug
output or startup logs.

| Variable | Required/default | Encoding and bounds |
| --- | --- | --- |
| `FERL_DATABASE_URL` | Required | PostgreSQL primary/read-write URL. Treat as secret. |
| `FERL_PUBLIC_BASE_URL` | Required | HTTP(S) origin only; no user info, path prefix, query, or fragment. A trailing `/` is accepted and removed. |
| `FERL_LISTEN_ADDR` | `0.0.0.0:8080` | Rust socket address (`IP:port`). |
| `FERL_HTTP_MAX_IN_FLIGHT` | `256` | Maximum concurrently serviced HTTP requests per instance, integer from 1 through 4,096. |
| `FERL_ANONYMOUS_CREATION` | `false` | Case-insensitive `true` or `false`. |
| `FERL_ANONYMOUS_CREATE_LIMIT` | `5` | Positive integer, maximum 1,000,000 attempts per rolling 60 seconds. |
| `FERL_ACCOUNT_CREATE_LIMIT` | `60` | Positive integer, maximum 1,000,000 attempts per account per rolling 60 seconds (admins included). |
| `FERL_TRUSTED_PROXIES` | Empty | Comma-separated IP addresses/CIDRs. Empty entries and malformed ranges are rejected; private ranges are not trusted automatically. |
| `FERL_CURSOR_KEY` | Required | Unpadded base64url encoding of exactly 32 bytes. Keep stable across restarts and instances; do not generate an instance-local fallback. |
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

Generate a cursor key with
`python3 -c 'import base64,secrets; print(base64.urlsafe_b64encode(secrets.token_bytes(32)).rstrip(b"=").decode())'`.
Do not deploy the placeholder in `.env.example` or reuse a key across unrelated
deployments.

`FERL_OTEL_ENDPOINT` and the OTLP resource settings are parsed now so malformed
configuration fails before serving. Part 01 installs safe structured stdout
logging and a typed, low-cardinality operation/outcome facade. Export providers
and actual traces, metrics, and OTLP logs are delivered in Part 10. Analytics
settings are similarly validated now and consumed by the analytics worker in
Part 07.

API request bodies have a fixed 65,536-byte maximum; this limit is not an
environment override.

Part 01 does not install or verify the persistence schema. It starts the HTTP
process with liveness available, but readiness remains 503 until the Part 02
migration verifier marks the schema compatible. A successful `SELECT 1` alone
does not imply schema readiness. The server never runs migrations or bootstrap
implicitly at startup.
