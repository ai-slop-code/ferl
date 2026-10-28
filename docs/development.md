# Development and local verification

## Toolchain and commands

The application pins Rust 1.99.1 in `rust-toolchain.toml`, declares MSRV 1.94,
and commits `Cargo.lock`. The build version comes from Cargo package metadata;
building does not require Git. Install the pinned stable toolchain with rustup,
including `rustfmt` and `clippy`, if it is not already available.

```sh
cargo fmt --all -- --check
cargo check --workspace --all-targets --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets
```

The all-targets test command includes the Part 03 `domain_contracts`,
`http_decoding`, `cursor_codec`, and `openapi_contracts` suites. The OpenAPI
suite reads the checked-in OpenAPI 3.1 YAML with `yaml_serde` and validates
representative fixtures against its locally resolved component schemas using
the pinned JSON Schema Draft 2020-12 `jsonschema` crate. It does not claim
endpoint/release conformance (Part 11).

The PostgreSQL integration test is required by the ordinary test command. It
fails with an actionable message if the harness environment is missing; it is
not silently skipped. Run `scripts/check.sh` to start/verify the test database,
set the required test variables, execute all four commands, and run doctests.

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

## Current stage boundaries

`ferl-server` (or no subcommand) runs the HTTP server. `migrate` applies the
embedded, versioned schema using only database and migration settings.
`bootstrap-admin --username ...` uses its own bounded configuration profile,
requires a migrated compatible schema, and emits one account-creation JSON DTO
to stdout only after commit; its sanitized diagnostics use stderr. Startup and
migrations never bootstrap. Part 04 registers account routes, and Part 06 adds
single-link management routes. Part 07 adds only exact public GET/HEAD redirects
and bounded best-effort visit ingestion. Link/tag discovery, reports and OTLP
remain later stages.
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
operations; collection listing and public redirects remain later stages.
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
uses the existing schema-v1 visit table to atomically insert an event and
conditionally increment its link counter under the link-row lock. Retry and
shutdown drain are bounded; UUID idempotency covers an ambiguous visit commit,
and deletion/cascade ordering prevents resurrection. Abrupt process termination
may lose events that remain buffered.

`sh scripts/check.sh` runs all existing suites, including
`creation_order`, `link_creation`, `link_authorization`, `link_management`,
`token_rotation`, `link_concurrency`, `link_failures`, `link_resolution`,
`routing`, `redirects`, `visit_ingestion`, `analytics_failures`,
`visit_concurrency`, and `link_process`. The process suite starts two independent production server
executables, captures bounded stdout/stderr, and exercises shared CRUD,
credential/token invalidation, allowance accounting, trusted TCP peers, redirects,
visit persistence across process/container restart, and the owned PostgreSQL-volume restart. No production test command, HTTP test route,
npm download, or clock override is used. See
[creation allowance](creation-allowance.md) for Part 05 charge semantics and
the ownership-consuming Part 06 interface.
