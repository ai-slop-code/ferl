# Part 01 — Rust foundation, configuration and HTTP envelope

**Dependencies:** none. Read the shared agent contract and architecture first.

**Normative references:** business sections 1–3, 8.1, 11.2, 12, 13, 14.2–14.3; OpenAPI health paths, error and health schemas, common response headers.

## Outcome

A buildable `ferl-server` with validated configuration, structured safe stdout logs, bounded PostgreSQL pooling, a reusable HTTP envelope, health endpoints and reproducible test tooling. It is the composition root for later increments.

## Files and responsibilities

- `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml`: compatible, pinned application build.
- `src/{main,lib,app,config,error,shutdown}.rs`: entry point, composition, startup state and typed errors.
- `src/http/{mod,router,response,request_id}.rs`: routing boundary and common headers/errors.
- `src/db/{mod,pool,migrate}.rs`: bounded pool and schema-readiness interface.
- `src/telemetry/{mod,logging,metrics}.rs`: safe structured logging and typed instrumentation facade.
- `tests/support/`, `tests/foundation.rs`, local prerequisite/check scripts.
- `docs/configuration.md`, `docs/development.md`, `.gitignore`, a secret-free configuration example.

## Tasks

### 1. Build and command surface

First apply [git-workflow.md](git-workflow.md): initialize local `main` if needed, commit the reviewed specification baseline, and create or resume `implementation/01-foundation`. Implement this part on that branch and integrate it only after its checks pass.

Create a library-driven binary so HTTP construction can be tested independently of CLI parsing. Use a parser such as Clap for `serve` (also the default invocation), `migrate`, and `bootstrap-admin --username ...`. Implement serving now; complete migrate/bootstrap in their assigned parts. Incomplete operator commands must fail clearly, not appear to succeed.

Pin the stable toolchain and dependency family versions. Ensure the selected OpenTelemetry crates can support all three signals together; record the version choices. Produce build-derived version metadata without requiring Git to exist at build time.

### 2. Configuration contract

Define typed configuration and document exact variable names, encodings, units, defaults and bounds. Suggested names:

| Setting | Requirement/default |
| --- | --- |
| `FERL_DATABASE_URL` | Required PostgreSQL primary/read-write connection string; secret |
| `FERL_PUBLIC_BASE_URL` | Required HTTP(S) origin; no user-info/path prefix/query/fragment; normalize optional trailing slash |
| `FERL_LISTEN_ADDR` | Document a concrete socket-address default, such as `0.0.0.0:8080` |
| `FERL_ANONYMOUS_CREATION` | Boolean, false |
| `FERL_ANONYMOUS_CREATE_LIMIT` | Positive integer, 5 per rolling 60 seconds |
| `FERL_ACCOUNT_CREATE_LIMIT` | Positive integer, 60 per rolling 60 seconds, including admins |
| `FERL_TRUSTED_PROXIES` | Explicit IP/CIDR list, empty by default |
| `FERL_CURSOR_KEY` | Required stable base64url-encoded 32-byte deployment key for serving; never log it |
| Pool and database timeouts | Bounded pool size, acquisition/connect/query/lock timeouts; document units and concrete defaults |
| Analytics configuration | Positive bounded queue capacity, worker concurrency, retry budget and drain timeout |
| Shutdown configuration | Bounded request drain and total shutdown deadline |
| OTLP/resource/sampling settings | Standard endpoint/headers/timeouts/resource settings; service `ferl`; parent-based root ratio 1.0 |

Distinguish command-specific requirements: migration/bootstrap need the database and safe logging, not a public origin or cursor key unrelated to their work. Validate applicable static settings before serving or mutating data. Reject zero/negative limits, malformed proxy ranges, out-of-range sampling, bad URLs, invalid key length and inconsistent bounds with sanitized messages.

The public-origin validator will share the URL infrastructure completed in part 03; its configuration-specific restrictions must already be correct. Do not trust Host or forwarding headers to generate short URLs. Do not require collector reachability during startup.

### 3. HTTP envelope and routing boundary

Implement an outer response layer that runs for successes, handler rejections, unmatched routes and method errors. Generate a new request UUID independently of incoming `X-Request-ID`. Return the same ID in the error body and header. Add `no-store` to API and short-link responses; applying it to all backend responses is a simple consistent baseline.

Define the complete stable error-code enum from OpenAPI. Centralize sanitized JSON error serialization and 401/405/429 headers. Distinguish known missing data from database errors. Framework-generated plaintext errors or 422 validation responses must be converted to the specified contract.

Implement the unauthenticated health GETs with stable operation IDs: `getLiveness` at `/api/health/live` and `getReadiness` at `/api/health/ready`. Liveness is 200 with `{ "status": "ok" }` without probing dependencies. Readiness requires initialization/schema compatibility, a bounded DB probe, and accepting-work state; failure is 503 with `{ "status": "unavailable" }`. Invalid credentials do not affect either endpoint.

Preserve whole-segment `/api` precedence. At this stage unimplemented API operations, root and unsupported paths return JSON 404. Do not enable a framework's automatic HEAD alias for API GETs: only the documented methods are operations, while all HEAD errors must still have no body. Build the dispatch/envelope so part 07 can add an explicit public HEAD handler without double-counting.

### 4. Runtime and initial telemetry

Use bounded Tokio tasks/pools and cancellation/shutdown state. Implement accepting/draining flags and bounded HTTP shutdown hooks; analytics/provider draining will attach later. Emit structured lifecycle, request-completion and sanitized failure logs. Define safe operation/outcome attributes now; never emit raw URI paths, query text, addresses, headers, bodies or database statements/parameters.

### 5. Test and check harness

Provide a documented Podman-based way to start a supported PostgreSQL version locally, following the shared agent contract's container guidance. Pin the image version, publish a configurable loopback port, and supply start/readiness/stop/cleanup commands. Use `podman exec <container> pg_isready` plus a successful connection from the host test process to establish readiness; `psql` can also run inside the container. Preserve the test-owned volume when testing restart persistence.

Build isolated-database/schema helpers and deadline-bound HTTP helpers. Define which commands run pure, database and eventual process suites. Missing required prerequisites must fail with a useful message. Set up formatting, Clippy and tests; contract validation is added in part 11.

## Verification gate

- Clean build and common Rust checks succeed.
- Valid configuration starts; malformed settings fail before traffic with no secret-bearing diagnostics.
- Liveness remains successful during DB unavailability; readiness fails with the exact status body and returns after DB recovery/initialization.
- Invalid supplied credentials do not affect health. Health creates no rate charge or visit.
- Root, unknown API paths and unsupported paths return 404; unsupported health methods return 405 with the appropriate Allow value.
- Response headers cover success, fallback and method-error paths. Incoming request IDs are replaced. Every HEAD response has zero body bytes.
- Stdout fixture checks find no configured secret or raw request URL in logs.

**Handoff:** record dependency versions, configuration defaults, local test commands and the readiness/schema interface for part 02. Acceptance coverage begins for 27, 31, 37–38 and 51; later parts complete their broader scenarios.
