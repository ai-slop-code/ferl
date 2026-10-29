# Development and local verification

## Toolchain and commands

The application pins Rust 1.99.1 in `rust-toolchain.toml`, declares MSRV 1.94,
and commits `Cargo.lock`. The build version comes from Cargo package metadata;
building does not require Git. Install the pinned stable toolchain with rustup,
including `rustfmt` and `clippy`, if it is not already available.

```sh
cargo fmt --all -- --check
cargo check --locked --workspace --all-targets --all-features
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace --all-targets
```

The all-targets test command includes the domain, decoding, cursor, OpenAPI
component/release conformance, packaged release smoke, and packaged TLS suites.
The required whole-document OpenAPI command is
`sh scripts/check-openapi.sh`: it validates
the checked-in document against the pinned OAI OpenAPI 3.1 schema, compiles all
applicable JSON Schema roots using locked Draft 2020-12 tooling, rejects invalid
controls, checks the method/path/operationId inventory, and probes the
production executable over TCP. `spec/openapi.yaml` remains authoritative.

Run the declared compiler floor with `sh scripts/check-msrv.sh` after installing
Rust 1.94.0. It uses separate `target/msrv-1.94/` directories for the locked
all-target/all-feature workspace check and production release build. If the
compiler is installed in an isolated prefix rather than through rustup, set
`FERL_MSRV_TOOLCHAIN_DIR` to the **toolchain prefix** (the directory containing
`bin/`); the script appends `/bin` and selects matching `rustc`, `rustdoc`, and
`cargo` 1.94.0. Do not pass the result of `dirname rustup which rustc` directly,
because that path already ends in `/bin`. CI resolves `rustc` and takes its
parent's parent before calling the script. This is a real compiler gate, not
`cargo metadata` evidence.

The PostgreSQL integration test is required by the ordinary test command. It
fails with an actionable message if the harness environment is missing; it is
not silently skipped. Run `sh scripts/check.sh` to start/verify the owned
PostgreSQL and run-isolated Collector fixtures, set required test variables,
execute fmt, all-target/all-feature build, warning-denied Clippy, all-target
tests, doctests, build the locked packaged release executable, and run
independent-process/proxy/release-smoke evidence. Collector/process tests are
mandatory and fail rather than skip if a fixture cannot be reached. Use
`sh scripts/check-openapi.sh`, `sh scripts/check-proxy.sh`, and
`sh scripts/check-release.sh` for independently repeatable focused release gates.
`sh scripts/check-tls.sh` builds/verifies a packaged executable from a clean
source tree, then tests PostgreSQL `verify-full` chain/hostname validation and
OTLP HTTPS signal export/rejection using disposable, run-isolated TLS fixtures.
It does not restart or remove the retained `ferl-stage01-postgres-data` volume.

## PostgreSQL with Podman

This harness owns only the container `ferl-stage01-postgres` and volume
`ferl-stage01-postgres-data`. It uses the pinned image
`docker.io/library/postgres:17.6-alpine3.22@sha256:ef257d85f76e48da1c64832459b59fcaba1a4dac97bf5d7450c77753542eee94`
and publishes loopback port 55432 by default. The named volume is persistent
across stop/start and restart tests. Use a different free port with
`FERL_POSTGRES_PORT`.

```sh
podman info
podman machine list
# If the existing default machine is stopped:
podman machine start
sh scripts/postgres.sh pull
sh scripts/postgres.sh start
sh scripts/postgres.sh ready
FERL_TEST_DATABASE_URL=postgres://ferl:ferl@127.0.0.1:55432/ferl_test \
FERL_TEST_POSTGRES_CONTAINER=ferl-stage01-postgres \
FERL_TEST_POSTGRES_PORT=55432 \
cargo test --test foundation_postgres -- --nocapture
sh scripts/postgres.sh stop
```

The image pull is pinned by digest. In environments where the corporate proxy
intercepts registry TLS, `pull` uses `podman pull --tls-verify=false`; that
exception applies only to pulling this container image. `start` uses
`--pull=never`, so container creation cannot perform an implicit second pull.
PostgreSQL application connections and other application TLS connections retain
normal certificate verification. `ready` waits up to 60 seconds for
`podman exec ... pg_isready -t 1`; the loop has an approximately 60-second
deadline. The integration test then proves a successful connection from the
host Rust process, checks readiness through the real PostgreSQL pool, and
stops/restarts only the label-verified harness container to verify recovery
while preserving its volume. `stop` preserves data. The explicitly destructive
`cleanup` command removes only the container and volume after verifying their harness labels.
Do not point test variables at a development or production database: the test
restarts the harness-owned PostgreSQL container.

Each `IsolatedTestDatabase` also uses a process-local async mutex to serialize
sibling Tokio tests that share PostgreSQL database-scoped advisory locks. Its
shared `flock` remains the cross-process coordination mechanism for test
binaries and harness scripts.

## Current stage boundaries

`ferl-server` (or no subcommand) runs the HTTP server. `migrate` applies the
embedded, versioned schema using only database and migration settings.
`bootstrap-admin --username ...` uses its own bounded configuration profile,
requires a migrated compatible schema, and emits one account-creation JSON DTO
to stdout only after commit; its sanitized diagnostics use stderr. Startup and
migrations never bootstrap. Part 04 registers account routes, Part 06 adds
single-link management routes, Part 07 adds exact public GET/HEAD redirects and
bounded best-effort visit ingestion, Part 08 adds scoped link listing and tag
discovery, and Part 09 registers the three authorized per-link analytics reports.
Part 10 adds OTLP signals, privacy boundaries, and bounded lifecycle. Part 11
adds independent-process release verification, whole-document OpenAPI
conformance, the pinned TCP proxy fixture, and a packaged-binary smoke. See
[the release/operator guide](release.md) and run `sh scripts/check-release.sh`.
Readiness is fail-closed until the production composition verifies migration
history and required schema objects, then performs a bounded database probe.
The verifier rechecks for database or schema recovery.
See [database operations](database.md) for schema versioning, lock keys,
isolation, and the migration procedure.

Structured JSON logs go to stdout. Only allowlisted method, route template,
status, operation/outcome category, and request correlation ID are emitted by
the HTTP envelope. Never add raw paths, query strings, headers, addresses,
payloads, credentials, database statements/parameters, or destination URLs to
instrumentation.

Part 05 adds production-composed effective-IP/creation-allowance services and TCP
peer plumbing. Part 06 registers create/detail/PATCH/DELETE/management-token
operations; Part 08 registers only `GET /api/v1/links` and `GET /api/v1/tags`
in addition to the retained `POST /api/v1/links`. Link collection `Allow` is
`GET, POST`; tag collection `Allow` is `GET`. Neither collection implies HEAD.
Listing authenticates freshly before parsing filters, denies management tokens,
defaults users/admins to own scope, and validates owner filter authority before
decoding its value. Candidate filters and keyset boundaries run in one PostgreSQL
statement with complete DTO/tag/owner hydration, checked cursors, and live
pagination. Tag counts group distinct visible live links over the full scope.
See [database operations](database.md) for the v2 migration, plan evidence,
index details, and required maintenance rollout order.
Create-link dispatch calls `CreationService::prepare(&Parts)` before query/body
validation, consumes its private acknowledged charge by ownership, and opens a
separate creation transaction. Account writes revalidate the retained credential
under the shared authority gate; token mutations lock/revalidate the live link
row. The shared `LinkService` uses the validated public origin and independent
pool, lock, SQL, body, and operation bounds. Its code-reservation allocator has a
private deterministic test seam; production always uses the secure generator.
`links::representation::build` is reusable by later stages, and
`links::resolve_active_link(&PgPool, code, deadline)` is the fresh one-statement
active-link snapshot used by redirects. The public router preserves raw path
spelling, builds `Location` from the exact resolved value, then nonblockingly
hands an immutable event to a bounded in-memory queue. A configured worker pool
uses the original visit table to atomically insert an event and
conditionally increment its link counter under the link-row lock. Retry and
shutdown drain are bounded; UUID idempotency covers an ambiguous visit commit,
and deletion/cascade ordering prevents resurrection. Abrupt process termination
may lose events that remain buffered.

Part 09 report routes are `GET /api/v1/links/{code}/visits`, `GET
/api/v1/links/{code}/stats`, and `GET
/api/v1/links/{code}/stats/breakdown`; all other methods return bodyless 405 with
`Allow: GET`. They authenticate and establish link authority before query/cursor
decoding. Final credential/object validation and report SQL share one
`REPEATABLE READ READ ONLY` transaction and held primary-pool connection; no row
lock or hidden checkout is taken. `FERL_REPORT_OPERATION_TIMEOUT_SECS` bounds the
whole report independently of the account API wrapper, worker timeouts, and
database bounds. See [analytics report operations](analytics-reports.md) for
snapshot/deletion semantics, cursor transport, query plans, and the v2/no-migration
decision.

`sh scripts/check.sh` runs all suites, including
`creation_order`, `link_creation`, `link_authorization`, `link_management`,
`token_rotation`, `link_concurrency`, `link_failures`, `link_resolution`,
`routing`, `redirects`, `visit_ingestion`, `analytics_failures`,
`visit_concurrency`, `link_process`, `link_listing`, `link_filters`,
`link_pagination`, `tag_discovery`, `discovery_failures`, `discovery_values`,
`discovery_process`, `visit_records`, `analytics_authorization`, `report_failures`,
`report_process`, `multi_instance_process`, `openapi_release`, `proxy_routing`,
`release_smoke`, and migrations. Part 09 also has deterministic real-PostgreSQL
worker-backed report and production-query-plan tests in `src/analytics/reports/tests.rs`.
The mandatory `telemetry_signals` suite also runs the production binary against
PostgreSQL and the pinned Collector and inspects real OTLP and process-shutdown
evidence.
Part 08's process test starts independent
production server executables, captures bounded stdout/stderr, and exercises
shared CRUD/list/discovery,
credential/token invalidation, allowance accounting, trusted TCP peers, redirects,
cross-process cursor continuation, worker counter visibility, and the owned
PostgreSQL-volume restart. No production test command, HTTP test route,
npm download, or clock override is used. See
[creation allowance](creation-allowance.md) for Part 05 charge semantics and
the ownership-consuming Part 06 interface.
