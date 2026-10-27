# Implementation progress and agent handoff

Initial state: instructions prepared; backend implementation had not started. Part 01 is now being implemented on its dedicated branch.

Status lifecycle: Planned → In progress → Verified (awaiting merge) → Complete. Use Blocked when a required implementation, verification or Git integration step cannot finish. Complete requires a committed merge into local `main`, as defined in [git-workflow.md](git-workflow.md).

## Part status

| Part | Status | Evidence / blocker |
| --- | --- | --- |
| 01 Foundation | Complete | Full Part 01 gate and real PostgreSQL readiness/outage/recovery passed; verified branch `implementation/01-foundation` was integrated by merge commit `5bd6127d36560e51ded4f46de68efc10ea78bf5a`. |
| 02 Persistence | Complete | Verified implementation `1a6b7bbbebc35c815a37dca478287004683cb1a1` and handoff branch tip `31c9a1e184052eca3e55ffb16160aee0c14b0f06` from `implementation/02-persistence` are integrated in the explicit local no-ff merge into `main`. |
| 03 Domain and wire contracts | Planned | — |
| 04 Authentication and accounts | Planned | — |
| 05 Client IP and rate limits | Planned | — |
| 06 Link management | Planned | — |
| 07 Redirects and visit ingestion | Planned | — |
| 08 Link and tag discovery | Planned | — |
| 09 Analytics reports | Planned | — |
| 10 Observability and lifecycle | Planned | — |
| 11 Release verification | Planned | — |

## Recorded decisions

The baseline implementation choices are in [architecture.md](architecture.md). Record selected versions, operational bounds, deviations and their rationale here during implementation.

- Owner-provided environment note: Podman is set up and may be used to start PostgreSQL and OpenTelemetry Collector containers when needed. Local dependency instructions should use Podman; runtime availability and actual container/test results are verified during implementation.
- Owner-required Git workflow: initialize the local repository on `main` if needed; implement each numbered part on a dedicated branch; after verification, commit it and merge into local `main`. Include the handoff in the committed integration.
- Part 01 selected Axum 0.8.9, SQLx 0.9.0, Tokio 1.54.1, and the OpenTelemetry 0.33.1 family (OTLP, SDK, API, log appender) with `tracing-opentelemetry` 0.34.0. `Cargo.lock` resolves one OpenTelemetry API/SDK/OTLP version; the all-signal feature set compiled. Rust 1.99.1 is pinned; package MSRV is 1.94.
- The serving pool is lazy so liveness can start during a database outage. Its size, in-flight HTTP concurrency, acquire/connect cap, statement/lock limits, analytics bounds, OTLP settings, and shutdown limits are validated and documented. SQLx 0.9 provides one pool acquisition deadline, so the shorter configured connect/acquire timeout is applied to acquisitions.
- HTTP admission now bounds both in-flight work and admitted waiters (`FERL_HTTP_MAX_IN_FLIGHT` and `FERL_HTTP_MAX_WAITING`). Excess work is rejected immediately with structured 503 `service_unavailable`; this overload code is documented in both normative contracts because their original 503 lists did not define request-capacity exhaustion. The in-flight and waiting caps are independently validated.
- Public-origin validation checks the unnormalized input syntax before applying URL-parser normalization, so dot segments, encoded dot segments, backslashes, whitespace/control characters, user-info, paths, queries, and fragments cannot be normalized into an accepted origin. A single optional trailing slash remains accepted and normalized.
- Framework error responses are distinguished from already-conforming application responses by an internal response extension. This lets the outer envelope sanitize JSON and plaintext framework rejections uniformly without rewriting health 503 bodies, and keeps error body/header request IDs consistent.
- Readiness fails closed until a schema verifier marks compatibility; Part 01 does not claim `SELECT 1` is schema proof. Part 02 must install the migration verifier before the server can report ready against a real deployment schema.
- The Part 01 PostgreSQL readiness test manually marks its test state schema-compatible to isolate the live database probe and restart recovery. It does not prove a schema version; Part 02's real verifier and migration tests remain required.
- The Podman harness uses `docker.io/library/postgres:17.6-alpine3.22@sha256:ef257d85f76e48da1c64832459b59fcaba1a4dac97bf5d7450c77753542eee94`, loopback port `55432` by default, configurable via `FERL_POSTGRES_PORT`, label `io.ferl.harness=stage01`, and persistent volume `ferl-stage01-postgres-data`. The harness verifies the image digest, volume, loopback port, and ownership before reuse; creation uses `--pull=never` after explicit pull.
- Historical registry blocker and resolution: the previous session's Podman VM reported `x509: certificate signed by unknown authority` because this environment's corporate proxy intercepts TLS. The owner confirmed the interception and explicitly authorized `podman pull --tls-verify=false` for image pulls only. The exact pinned pull succeeded; application PostgreSQL and HTTP TLS verification settings were not changed, and no CA/trust-store modification was made.
- Command-specific configuration remains explicit: `serve` validates the database URL, public origin, cursor key, runtime bounds, and telemetry settings. `migrate` and `bootstrap-admin` are intentionally unavailable in this stage and fail without reading configuration or changing database state; their future implementations need not require serving-only settings.
- Telemetry endpoint, headers, timeouts, resources, and sampling settings are parsed/redacted now. Part 01 emits safe structured stdout and typed operation/outcome events; actual OTLP providers/export and collector-outage verification remain Part 10.

Part 02 persistence decisions:

- Schema version 1 is the only version accepted by this binary. PostgreSQL 17.x is the supported major line; the real harness is PostgreSQL 17.6. Migration rejects another major version before DDL. SQLx 0.9 embedded migrations use `_sqlx_migrations`; no extension is required.
- `ferl-server migrate` validates only `FERL_DATABASE_URL` and `FERL_MIGRATE_*` values before connecting. Defaults are connect 10s, migration advisory lock 30s with 50ms `pg_try_advisory_lock` polling, statement 120s, total 600s, and polling interval bounds 10–1,000ms. Invalid applicable config exits 2; runtime/compatibility failures exit 1 with safe categories; success exits 0. It neither creates an administrator nor completes bootstrap.
- SQLx 0.9.0 PostgreSQL migrator behavior was inspected in the resolved crate source: its default database lock is session-scoped, uses a blocking advisory lock, and early errors can bypass its explicit unlock. ferl instead holds key `0x4645524c4d494752` (`FERLMIGR`, decimal `5063543843715630930`) with bounded nonblocking attempts on a dedicated `PgConnection`, disables SQLx's inner lock while held, and closes that session on failure/cancellation. PostgreSQL DDL and SQLx history bookkeeping share each migration transaction.
- Schema verification compares every embedded history version/checksum/success value, rejects missing/dirty/mismatched/unsupported/newer history, and checks server major, core table and column signatures/nullability, required `COLLATE "C"` fields, named constraints by table, baseline indexes, immutability triggers, singleton state and `api` seed. A successful `SELECT 1`, table count, or history row alone is not compatibility evidence.
- `serve` starts live with schema readiness false and does not migrate/bootstrap. One verifier task checks immediately, rechecks every 5s with a 30s ceiling, and is cancelled/joined within 2s on shutdown. A failed schema check or readiness DB probe clears compatibility; a verification generation prevents an older in-flight schema result from overriding that failure. Recovery becomes ready only after the monitor verifies schema again and the readiness route's bounded DB probe succeeds. The route checks accepting-work/schema state again after its probe, so drain does not become ready from a stale successful probe.
- Account-authority transaction advisory key is `(0x4645524c, 0x41434354)` (`FERL` / `ACCT`): shared for future account-authenticated link mutations, exclusive for every account write. Callers pass the transaction explicitly. Bounded variants set a transaction-local statement deadline and require rollback/drop on timeout. READ COMMITTED and the global order (authority gate → account rows in UUID order → link rows in UUID order → child rows) are documented; revalidation is a subsequent statement with a fresh snapshot.
- Migration and serving test connections both use the fixture URL's startup `options[search_path]`; helper pool, production `AppState` pool and CLI processes all target one UUID-owned schema. `IsolatedTestDatabase` exposes `pool()` and `schema_database_url()`, removes only its generated schema on `close()`, and holds a cross-process `flock`. `scripts/postgres.sh` uses the same lock for harness operations, so restart tests coordinate beyond Rust test-local mutexes.
- Normative business/OpenAPI files were not changed. No contract conflict was found. Domain Unicode normalization, public DTOs, services, bootstrap CLI, account mutation endpoints, rate-limit service, link CRUD/redirects, atomic visit-worker increments, and independent-process non-migration release races remain assigned to later parts.

## Contract conflicts

No contract conflict was found for Part 01. The business specification and OpenAPI agree on health paths/bodies, request IDs, no-store headers, and structured errors. Health routes have no declared query parameters in OpenAPI; undeclared health query values return `invalid_request` under the business specification's section 11.2 unknown-query rule. The original 503 error list did not define bounded HTTP-capacity exhaustion, so `service_unavailable` was added consistently to business section 12 and the OpenAPI error enum/503 response. A future actual conflict must name both passages, the precise incompatibility, and its explicit resolution.

## Part 01 handoff — 2026-10-27

Status: **Complete**. The full Part 01 completion gate passed and was integrated into local `main` by merge commit `5bd6127d36560e51ded4f46de68efc10ea78bf5a` (parents: `40b3ae713f0cd9bd2389f872d2953d29ef38f38d` and `aa6239a7f776d8965464863c8dae6836bbeeabac`).

Part branch: `implementation/01-foundation`

Base commit: `40b3ae713f0cd9bd2389f872d2953d29ef38f38d`

Implementation checkpoint commit: `195376016909d356d645b4bc16657a13a25a572d` (`wip(part-01): implement foundation pending PostgreSQL gate`).
Resumed handoff commit: `f6aa001109f195aa7f5da512997530e0402df724` (`docs(part-01): record blocked verification checkpoint`).
Verified implementation commit: `2706102d06171051ebf6c8bc15305786146b2b5b` (`feat(part-01): complete foundation verification`).
Part branch tip integrated: `aa6239a7f776d8965464863c8dae6836bbeeabac` (`docs(part-01): record verified implementation commit`); this handoff-only commit records the verified implementation commit above.
Integration state: merged by `5bd6127d36560e51ded4f46de68efc10ea78bf5a`; `git show` confirmed the expected two parents and the verified part branch remains retained at `aa6239a7f776d8965464863c8dae6836bbeeabac`.

Implemented behavior:

- Added the library-driven `ferl-server` binary with default/explicit `serve`, plus clearly failing `migrate` and `bootstrap-admin --username ...` commands that do not access or modify the database yet.
- Added sanitized, typed startup configuration, pre-normalization public-origin validation, strict 32-byte base64url cursor key validation, trusted-proxy IP/CIDR parsing, bounded pool/HTTP in-flight and waiting work/timeouts, and OTLP/resource/sampling settings. Secret-bearing types have redacted `Debug`; failures report only setting/category and static reason.
- Added a shared production `AppState::from_config` composition path for the executable and real-PostgreSQL readiness test, a bounded lazy pool with statement/lock timeouts, an explicit fail-closed schema readiness state, and bounded graceful HTTP drain/pool shutdown. Excess admitted work fails promptly with structured 503.
- Added an outer HTTP envelope that replaces incoming request IDs, keeps error-body/header IDs aligned, applies `Cache-Control: no-store`, strips every HEAD body, sanitizes JSON/plaintext framework rejections, rejects undeclared health query parameters, and logs only safe route templates and bounded attributes. Liveness is DB-independent; readiness requires accepting-work state, schema compatibility, and a bounded PostgreSQL probe.
- Added structured JSON stdout logging with dependency-target filtering, safe operation/outcome types, build-derived version metadata, a Podman PostgreSQL harness, and configuration/development instructions.
- Added deadline-bound HTTP helpers and an isolated, uniquely named PostgreSQL test schema helper. The database test uses the production pool/configuration composition and checks the harness label, pinned digest, loopback port, persistent volume, and successful host Rust connections; cleanup drops only its generated schema.

Changed files: `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml`, `build.rs`, `.env.example`, `src/{main,lib,app,config,error,shutdown}.rs`, `src/http/{mod,request_id,response,router}.rs`, `src/db/{mod,pool,migrate}.rs`, `src/telemetry/{mod,logging,metrics}.rs`, `tests/foundation_{http,postgres,process}.rs`, `tests/support/{database,http}.rs`, `scripts/{postgres,check}.sh`, `docs/{configuration,development}.md`, `spec/server-business-logic.md`, `spec/openapi.yaml`, this handoff, and `acceptance-map.md`.

Checks executed:

Toolchain/dependency evidence: `rustc 1.99.1 (58cf8f9c3 2026-10-13)` and `cargo 1.99.1 (5f94df478 2026-08-27)` match `rust-toolchain.toml`. `cargo metadata` found no resolved package advertising a `rust_version` above the declared 1.94 MSRV; an actual 1.94 compiler run was unavailable because `rustup` is not installed. `Cargo.lock` is committed and reproducible. `cargo tree -i opentelemetry`, SDK, and OTLP checks show one compatible 0.33.1 family and `tracing-opentelemetry` 0.34.0.

Checks executed after the final implementation changes:

- `sh scripts/check.sh` — passed after final implementation changes, including use of the shared production config/pool composition by the PostgreSQL test. It ran `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, and `cargo test --workspace --all-targets`. Results: 13 library tests, 2 HTTP tests, 1 real-PostgreSQL integration test, and 2 executable-process tests passed; the binary unit target had 0 tests. No test was ignored or skipped.
- `FERL_TEST_DATABASE_URL=postgres://ferl:ferl@127.0.0.1:55432/ferl_test FERL_TEST_POSTGRES_CONTAINER=ferl-stage01-postgres FERL_TEST_POSTGRES_PORT=55432 cargo test --test foundation_postgres -- --nocapture` — passed after the final helper changes (1 test). It proves connection from the host Rust process before and after restart, `pg_isready` after restart, liveness through outage, exact readiness unavailable/ready bodies, and readiness after restart. The test manually marks its schema state compatible only to test probing; no schema verification is claimed.
- `cargo test --lib --test foundation_http --test foundation_process` — passed during final test development (13 library, 2 HTTP, 2 process tests). Coverage includes normalized-origin rejection, JSON framework rejection sanitation, bounded active/waiting admission, overload headers/body IDs, bounded active-request drain, process startup/shutdown, secret-safe stdout, health credentials, query/method handling, structured fallback, and bodyless HEAD errors.
- `podman info` and `podman machine list` — passed; Podman 6.1.4 with `podman-machine-default` already running.
- Exact image command `podman pull --tls-verify=false docker.io/library/postgres:17.6-alpine3.22@sha256:ef257d85f76e48da1c64832459b59fcaba1a4dac97bf5d7450c77753542eee94` — passed; image ID `sha256:29e645e20571728f22df398b9954bb6c1cefec390d99ed4e26308fc2f52d6a37`. The script path `sh scripts/postgres.sh pull` also passed with the same digest.
- `sh scripts/postgres.sh start` and `sh scripts/postgres.sh ready` — passed. `ready` observed `podman exec ferl-stage01-postgres pg_isready -q -t 1 -U ferl -d ferl_test` accepting connections. Container inspection confirmed image digest, `io.ferl.harness=stage01`, `127.0.0.1:55432`, and persistent volume `ferl-stage01-postgres-data`; the restart test retained that volume.
- `sh -n scripts/postgres.sh && sh -n scripts/check.sh && git diff --check` — passed after final implementation changes; `git diff --cached --check` and `git diff --cached main --check` also passed during staged review.

Acceptance evidence: Part 01 subsets of scenarios 27, 31, 37, and 38 are mapped to concrete tests in `acceptance-map.md`. Other business-operation database outages, reverse-proxy/static-host behavior, collector outage/export, and scenario 51 OpenAPI release validation remain later-stage checks. The acceptance map does not claim those broader scenarios complete.

No Part 01 implementation or verification blocker remains. At the Part 01 checkpoint, migrations and the real schema verifier were still required before deployment readiness; Part 02 now supplies that persistence gate. Administrator bootstrap remains Part 04 and OTLP providers remain Part 10. The merge commit hash is intentionally not embedded in its own tree; obtain it from local Git history after the merge commit is created.

Next part: **02 — Persistence**, only after Part 01 is verified and merged.

## Part 02 handoff — 2026-10-27

Status: **Complete**. The Part 02 implementation and complete verification gate passed and were integrated into local `main` by an explicit no-ff merge commit.

Part branch: `implementation/02-persistence`

Base commit: `5bd6127d36560e51ded4f46de68efc10ea78bf5a`

Verified implementation commit: `1a6b7bbbebc35c815a37dca478287004683cb1a1` (`feat(part-02): add persistence schema and migrations`).
Part branch tip integrated: `31c9a1e184052eca3e55ffb16160aee0c14b0f06` (`docs(part-02): record verified implementation commit`); this handoff-only commit records the verified implementation commit above.
Integration state: integrated by the explicit no-ff merge on `main` with parents `5bd6127d36560e51ded4f46de68efc10ea78bf5a` and `31c9a1e184052eca3e55ffb16160aee0c14b0f06`. The merged implementation tree matched the verified part branch; only this progress completion metadata differs from the tested tree.

Implemented files and behavior:

- Added `migrations/0001_initial.sql` for `system_state`, accounts, permanent code reservations, links, tags, visits, typed rate buckets and charges. Named checks/FKs/unique constraints, baseline indexes, bytewise collations, digests/lengths, ownership modes, host-masked `inet`, monotonic bootstrap state, permanent reservations, immutable account/link/visit identities, and deletion direction are authoritative in PostgreSQL.
- Implemented embedded, repeatable `ferl-server migrate`, command-specific safe config, PostgreSQL 17.x check, bounded session advisory-lock coordination, transactional migration bookkeeping, and schema verification. Added `src/db/coordination.rs`, `src/db/error.rs`, and non-serializable/no-`Debug` row types and checked timestamp/counter conversions in `src/db/records.rs`.
- Connected the real verifier to production serve composition. Liveness stays DB-independent; readiness needs verified history/schema, a bounded DB probe, and accepting work. Background rechecks restore readiness only after DB/schema recovery validation; draining and failed probes clear readiness.
- Added real-PostgreSQL process/constraint/lock/restart suites and isolated search-path fixtures. `scripts/check.sh` now runs `cargo check` as well as fmt, clippy, and all tests; the external Podman harness and test suites share an interprocess lock.
- Changed files: `Cargo.toml`, `Cargo.lock`, `migrations/0001_initial.sql`, `src/app.rs`, `src/config.rs`, `src/main.rs`, `src/db/{mod,migrate,coordination,error,records}.rs`, `tests/{migrations,persistence_constraints,coordination}.rs`, `tests/foundation_postgres.rs`, `tests/foundation_process.rs`, `tests/support/database.rs`, `scripts/{check,postgres}.sh`, `docs/{database,configuration,development}.md`, this file, and `acceptance-map.md`. No normative business/OpenAPI file changed.

Verification evidence:

- Environment: `podman info`, `podman machine list`, and `sh scripts/postgres.sh ready` passed. `podman-machine-default` was running; the label-verified, pinned PostgreSQL 17.6 container accepted host connections. The real restart test stopped/restarted only that container and preserved its named volume.
- `sh scripts/check.sh` — passed after final implementation changes. It ran `cargo fmt --all -- --check`, `cargo check --workspace --all-targets --all-features`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, and `cargo test --workspace --all-targets` against the real harness. Results: 21 library tests, 2 coordination, 2 HTTP, 1 PostgreSQL readiness/restart, 2 process, 7 migration, and 4 persistence-constraint tests passed (39 total; binary unit target has 0; none ignored/skipped).
- Concrete database evidence: `tests/migrations.rs::{migration_cli_migrates_fresh_isolated_schema_and_is_idempotent,concurrent_migration_processes_create_one_consistent_history,migration_rejects_dirty_mismatched_and_newer_history_without_repairing_it,migration_lock_wait_is_bounded_and_missing_objects_are_not_repaired,schema_verifier_detects_nullability_and_minimal_reservation_drift,failed_initial_migration_rolls_back_ddl_and_success_history_together,migration_configuration_is_command_specific_and_failures_are_sanitized}`; `tests/persistence_constraints.rs::{accounts_reservations_roles_and_digest_constraints_are_authoritative,link_ownership_digest_counter_and_immutable_identity_constraints_hold,link_and_account_deletion_cascade_children_but_rollback_is_atomic,visits_tags_and_rate_accounting_have_authoritative_shapes_and_deletion_rules}`; `tests/coordination.rs::{shared_authority_locks_coexist_and_exclusive_gate_waits_for_commit_and_rollback,bounded_wait_failure_is_sanitized_and_read_committed_revalidation_is_fresh}`; and `tests/foundation_postgres.rs::readiness_tracks_schema_and_recovers_after_postgres_restart`.
- Acceptance contributions are recorded for scenarios 5, 9, 12–13, 20, 24, 26, and the migration/DB-coordination portion of 34 in `acceptance-map.md`. They distinguish database constraints/transactions from later API/bootstrap/worker behavior and independent-process release coverage.
- Toolchain: `rustc 1.99.1` and `cargo 1.99.1`. `cargo metadata --format-version 1` resolved 302 packages and found no declared dependency `rust_version` above 1.94. An actual Rust 1.94 compiler run was not available and is not claimed.

Remaining later-stage responsibilities: Parts 03–11 as above; schema v1 is the only supported ferl migration state in this binary. No known Part 02 blocker remains.

## Per-part handoff template

Copy this section for each implementation session:

```text
Part / date:
Status:
Part branch:
Base commit:
Verified implementation commit:
Integration state (pending / merged into main):

Implemented behavior:
Changed files and migrations:
Decisions / deviations:

Checks executed:
- Command:
  Result:
  Acceptance scenarios covered:

Unexecuted required checks and reason:
Remaining blockers or unfinished behavior:
Next part and prerequisite notes:
```
