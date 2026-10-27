# Shared instructions for every implementation part

## 1. Authority and scope

Read [server-business-logic.md](../server-business-logic.md) and [openapi.yaml](../openapi.yaml) as the two normative contracts. Follow the business rules even when JSON Schema cannot express them. These instructions choose a practical implementation shape, not additional product features.

Deliver the Rust backend binary `ferl-server`: JSON API, public redirects, operator bootstrap, PostgreSQL persistence and OpenTelemetry. The separate SPA and CLI have their own projects. The server's root returns 404, and the release includes the checked-in OpenAPI YAML.

Before each part:

1. Read applicable repository instructions and the existing progress record.
2. Inspect implemented modules, migrations, dependencies and tests. Extend existing working code rather than replacing it with the illustrative layout.
3. Identify the part's dependencies. If an earlier gate is incomplete, fix or explicitly record the blocking dependency before claiming this part complete.
4. Read the referenced normative sections and operation schemas. Preserve operation IDs, field names, statuses, headers, nullability and defaults.
5. Read [the local Git workflow](git-workflow.md). Initialize the repository if needed, then start or resume the selected part's branch before implementation.

## 2. Working rules

- Complete one requested part at a time, maintaining a buildable repository. Implement shared interfaces needed by that part; keep future business endpoints unregistered until their actual implementation exists.
- Keep handlers thin: strict HTTP decoding, application service call, response mapping. Keep authorization and transaction boundaries visible in application services.
- Use typed Rust values for validated data and typed errors. Keep database records, public DTOs and secret-bearing results separate.
- Use PostgreSQL for authoritative coordination from the first implementation. In-memory locks, caches and mock repositories cannot establish cross-instance correctness.
- Use the primary/read-write database endpoint. Read credentials, owner status and destinations afresh per request.
- Return a successful mutation response only after commit. Preserve ambiguity after a lost commit acknowledgement; do not automatically replay a create as though it were idempotent.
- Update `updated_at` only for actual link metadata/state changes. A no-op but syntactically valid PATCH returns the current representation; it is distinct from an empty PATCH. Analytics increments do not touch `updated_at`.
- Never log request/response bodies or derive `Debug` indiscriminately for credentials, configuration, URLs, analytics events, titles, tags or request DTOs. Use explicit safe diagnostic categories.
- Prefer a small number of concrete services and repository functions over a generic framework. Introduce narrow clock/randomness/queue interfaces only where meaningful deterministic tests need them.
- Migrations are append-only after they have been used as a completed increment. Preserve existing data and migration history across later parts.
- Each numbered part is implemented on its own branch, verified, committed and merged into local `main` using [git-workflow.md](git-workflow.md). A part is not complete until its Git integration is complete.

## 3. Mandatory cross-cutting behavior

Every implemented application response has a fresh server-generated `X-Request-ID`. Every API and public short-link response has `Cache-Control: no-store`, including errors and secret-bearing responses. Error bodies follow section 12 and never expose raw dependency errors. HEAD responses always have zero body bytes.

Public redirect and health requests ignore credentials. On business API operations, supplied malformed/invalid/revoked credentials produce 401 rather than anonymous fallback. Distinguish missing credentials, wrong operation scope, inaccessible objects, and unavailable storage.

Creation processing is fixed: authenticate → check creator eligibility → commit rate charge → decode/validate body and feature permissions → create in a separate transaction. Framework JSON/body middleware must not reorder these steps. Revalidate credentials inside protected mutation transactions as described in [architecture.md](architecture.md).

All configuration is startup configuration. Instances share business settings, public origin, cursor cryptographic configuration and PostgreSQL state. Configuration validation must not echo secrets.

## 4. Verification policy

Write tests for observable invariants and meaningful boundaries, particularly authority, persistence, races, pagination and failure behavior. Prefer tests that would fail under a plausible incorrect implementation over tests that duplicate individual assignments.

Use:

- Unit/property tests for pure normalization, parsing, cursor validation and ordering predicates.
- Real PostgreSQL integration tests for uniqueness, migration behavior, transactions, durable rate accounting, deletion and counter consistency.
- HTTP tests for request precedence, actual headers/body bytes, routing and endpoint representations.
- Independent executable-process tests in part 11 for shared-state correctness. Do not substitute two routers in one Tokio runtime.

Use isolated test databases or schemas with controlled migration setup. Never clean an arbitrary configured development/production database. Concurrency tests use barriers and bounded waits, not timing-dependent sleeps to establish the race. Async analytics tests wait with a deadline or use internal test-only worker controls; they must not assume immediate visibility.

### Local dependencies with Podman

The project owner has set up Podman and authorized using it to start PostgreSQL and OpenTelemetry Collector containers as needed. Prefer Podman for local integration prerequisites and document reproducible start, readiness, stop and cleanup commands. Check runtime availability with `podman info`; on macOS, inspect `podman machine list` and start the existing machine if necessary.

Use pinned, fully qualified image references, project/test-specific container names and configurable loopback-published ports. Native `podman` commands are sufficient; only use `podman compose` after verifying its external Compose provider is available. If a container-test library expects a Docker-compatible socket, explicitly configure and verify its Podman connection.

Wait for actual dependency readiness with a deadline before tests. Keep PostgreSQL data across restart/persistence scenarios, capture collector output for signal/privacy assertions, and clean up only resources created by the test harness. Record the commands and dependency image versions in the handoff.

Once tooling exists, the common gate is:

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets
```

Part 01 must document how integration prerequisites are started and how required tests are invoked. If additional suites require explicit commands, run them for the relevant parts and record those commands. No silent skipping when PostgreSQL or a collector fixture is absent. Keep successful checks current after meaningful changes without repeatedly running unrelated suites.

## 5. Handoff requirements

Update [progress.md](progress.md) after each part with:

- Status: planned, in progress, blocked, verified (awaiting merge), or complete. Complete includes committed integration into `main`.
- Files/modules and migrations added or changed.
- Concrete implementation decisions, library versions and deviations from the suggested internal layout.
- Exact commands, observed results and relevant acceptance scenario numbers.
- Known unfinished items and dependencies for the next part.
- Part branch, base commit and verified implementation commit. Report the resulting merge commit and clean final Git state in the final response; follow the Git workflow to include completed progress in the merge itself.

Do not mark a later part complete merely because its interface exists. Do not report tests as passing unless executed. An actual contract conflict gets a concise entry naming both sources, the conflict and its resolution; do not quietly select one source.
