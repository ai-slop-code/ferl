# Implementation progress and agent handoff

Historical baseline note: instructions were prepared before implementation began. Parts 01 and 02 have since completed and merged; Part 03 is the current implementation stage on its dedicated branch.

Status lifecycle: Planned → In progress → Verified (awaiting merge) → Complete. Use Blocked when a required implementation, verification or Git integration step cannot finish. Complete requires a committed merge into local `main`, as defined in [git-workflow.md](git-workflow.md).

## Part status

| Part | Status | Evidence / blocker |
| --- | --- | --- |
| 01 Foundation | Complete | Full Part 01 gate and real PostgreSQL readiness/outage/recovery passed; verified branch `implementation/01-foundation` was integrated by merge commit `5bd6127d36560e51ded4f46de68efc10ea78bf5a`. |
| 02 Persistence | Complete | Verified implementation `1a6b7bbbebc35c815a37dca478287004683cb1a1` and handoff branch tip `31c9a1e184052eca3e55ffb16160aee0c14b0f06` from `implementation/02-persistence` are integrated in the explicit local no-ff merge into `main`. |
| 03 Domain and wire contracts | Verified (awaiting merge) | Verified implementation `9f143478c657a505082911d015c8961f14df28dd` on `implementation/03-domain-and-wire-contracts`, based on verified `main` `2ca9988f85818afd4f9cecb21df6be53d0a978f6`; explicit integration remains pending. |
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
- Historical Part 01 note: its PostgreSQL readiness test manually marked the test state schema-compatible to isolate the live database probe/restart recovery; it did not prove a schema version. Part 02 later added the real verifier and migration tests, which are the current schema evidence.
- The Podman harness uses `docker.io/library/postgres:17.6-alpine3.22@sha256:ef257d85f76e48da1c64832459b59fcaba1a4dac97bf5d7450c77753542eee94`, loopback port `55432` by default, configurable via `FERL_POSTGRES_PORT`, label `io.ferl.harness=stage01`, and persistent volume `ferl-stage01-postgres-data`. The harness verifies the image digest, volume, loopback port, and ownership before reuse; creation uses `--pull=never` after explicit pull.
- Historical registry blocker and resolution: the previous session's Podman VM reported `x509: certificate signed by unknown authority` because this environment's corporate proxy intercepts TLS. The owner confirmed the interception and explicitly authorized `podman pull --tls-verify=false` for image pulls only. The exact pinned pull succeeded; application PostgreSQL and HTTP TLS verification settings were not changed, and no CA/trust-store modification was made.
- Historical Part 01 command decision: `migrate` and `bootstrap-admin` were then intentionally unavailable. Part 02 implemented the explicit migration command; `bootstrap-admin` remains clearly unavailable until Part 04. `serve` validates the database URL, public origin, cursor key, runtime bounds, and telemetry settings.
- Telemetry endpoint, headers, timeouts, resources, and sampling settings are parsed/redacted now. Part 01 emits safe structured stdout and typed operation/outcome events; actual OTLP providers/export and collector-outage verification remain Part 10.

Part 02 persistence decisions:

- Schema version 1 is the only version accepted by this binary. PostgreSQL 17.x is the supported major line; the real harness is PostgreSQL 17.6. Migration rejects another major version before DDL. SQLx 0.9 embedded migrations use `_sqlx_migrations`; no extension is required.
- `ferl-server migrate` validates only `FERL_DATABASE_URL` and `FERL_MIGRATE_*` values before connecting. Defaults are connect 10s, migration advisory lock 30s with 50ms `pg_try_advisory_lock` polling, statement 120s, total 600s, and polling interval bounds 10–1,000ms. Invalid applicable config exits 2; runtime/compatibility failures exit 1 with safe categories; success exits 0. It neither creates an administrator nor completes bootstrap.
- SQLx 0.9.0 PostgreSQL migrator behavior was inspected in the resolved crate source: its default database lock is session-scoped, uses a blocking advisory lock, and early errors can bypass its explicit unlock. ferl instead holds key `0x4645524c4d494752` (`FERLMIGR`, decimal `5063543843715630930`) with bounded nonblocking attempts on a dedicated `PgConnection`, disables SQLx's inner lock while held, and closes that session on failure/cancellation. PostgreSQL DDL and SQLx history bookkeeping share each migration transaction.
- Schema verification compares every embedded history version/checksum/success value, rejects missing/dirty/mismatched/unsupported/newer history, and checks server major, core table and column signatures/nullability, required `COLLATE "C"` fields, named constraints by table, baseline indexes, immutability triggers, singleton state and `api` seed. A successful `SELECT 1`, table count, or history row alone is not compatibility evidence.
- `serve` starts live with schema readiness false and does not migrate/bootstrap. One verifier task checks immediately, rechecks every 5s with a 30s ceiling, and is cancelled/joined within 2s on shutdown. A failed schema check or readiness DB probe clears compatibility; a verification generation prevents an older in-flight schema result from overriding that failure. Recovery becomes ready only after the monitor verifies schema again and the readiness route's bounded DB probe succeeds. The route checks accepting-work/schema state again after its probe, so drain does not become ready from a stale successful probe.
- Account-authority transaction advisory key is `(0x4645524c, 0x41434354)` (`FERL` / `ACCT`): shared for future account-authenticated link mutations, exclusive for every account write. Callers pass the transaction explicitly. Bounded variants set a transaction-local statement deadline and require rollback/drop on timeout. READ COMMITTED and the global order (authority gate → account rows in UUID order → link rows in UUID order → child rows) are documented; revalidation is a subsequent statement with a fresh snapshot.
- Migration and serving test connections both use the fixture URL's startup `options[search_path]`; helper pool, production `AppState` pool and CLI processes all target one UUID-owned schema. `IsolatedTestDatabase` exposes `pool()` and `schema_database_url()`, removes only its generated schema on `close()`, and holds a cross-process `flock`. `scripts/postgres.sh` uses the same lock for harness operations, so restart tests coordinate beyond Rust test-local mutexes.
- At the Part 02 handoff, normative business/OpenAPI files were unchanged and no contract conflict was found. Domain Unicode normalization/public DTOs were then assigned to Part 03 and have since been implemented there; services, bootstrap CLI, account mutation endpoints, rate-limit service, link CRUD/redirects, atomic visit-worker increments, and independent-process release races remain later-stage work.

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

Historical next-part note: Part 02 — Persistence followed Part 01 and is now complete/merged as recorded below. The current next part is Part 03.

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

At the Part 02 handoff, remaining responsibilities were Parts 03–11; Part 03 is now underway, and Parts 04–11 remain future work. Schema v1 is still the only supported ferl migration state in this binary. No known Part 02 blocker remains.

## Part 03 handoff — 2026-10-28

Status: **Verified (awaiting merge)**. Verified implementation commit: `9f143478c657a505082911d015c8961f14df28dd` (`feat(part-03): add domain and wire contracts`). Part branch: `implementation/03-domain-and-wire-contracts`. Base: verified `main` `2ca9988f85818afd4f9cecb21df6be53d0a978f6`. The repository-local identity is `LubosBa <30705301+LubosBa@users.noreply.github.com>`. The working tree/index are clean after the reviewed implementation commit. The following handoff-only commit records this verified implementation; the resulting branch-tip hash will be recorded in the pending merge completion metadata. No merge/rebase/cherry-pick was pending at the checkpoint.

Sources reviewed before implementation: implementation README, agent contract, architecture, Git workflow, progress and acceptance map, complete Part 03 brief; normative business sections 3–4, 5.1, 6.2–6.7, 7, 9.1–9.3, 10, 11.2–11.5, 12, 13–13.1 and 14.2–14.3; relevant OpenAPI paths, parameters, schemas, nullability, defaults, errors, headers and security; source, dependencies, schema, harness, tests and database/configuration/development docs. No normative contract conflict was found or normative contract edited.

### Delivered behavior and decisions

- Added validated types in `src/domain/{ids,count,url,short_code,text,time,secret}.rs`. Username/custom-code syntax is exact; generated codes use OS randomness with 252/256 rejection sampling and a 128-draw per-character ceiling. URL validation checks raw and serialized byte limits, explicit scheme/authority/port, empty user-info, parser-repair cases, and `Location` safety. Public-origin validation now shares this pre-normalization logic.
- Narrow prerequisite correction: the former public-origin validator relied on `url::Url` for port parsing, and the selected URL release repairs `https://internal:` to a host without a port. Configuration now delegates to `domain::url::PublicOrigin::parse`, which checks raw authority/port syntax first and rejects both empty and out-of-range supplied ports. `src/domain/url.rs::tests::validates_authority_before_url_parser_repair` demonstrates the parser behavior; configuration and URL boundary tests cover rejection. No broader Part 01/02 rewrite was needed.
- Full case folding, NFC, Unicode White_Space, and General_Category Control use pinned ICU4X 2.4.0 compiled Unicode 18.0.0 data. The actual `icu_casemap_data`, `icu_normalizer_data`, and `icu_properties_data` crate sources identify Unicode 18.0.0. Persisted comparison keys must use this same version across instances; future Unicode data changes require an explicit compatibility/data-migration plan.
- Titles/tags/search are separate validated types; tags enforce 256 raw scalars and 20 inputs before deduplication, canonicalize by full fold/NFC, and sort bytewise. Persisted comparison transform does not apply request query trimming/limits.
- Credentials are exactly `ferl_ak_v1_` / `ferl_mt_v1_` followed by 32 OS-random bytes encoded as canonical 43-character unpadded base64url. SHA-256 covers the complete presented secret including prefix/version. The fixed 32-byte digest, parsed redacted credential/class, and explicitly one-time serializable output are distinct types. No startup or migration code issues credentials or creates accounts; `bootstrap-admin` remains unavailable for Part 04.
- Strict body/query decoding lives in `src/http/{body,query,dto,rejection}.rs`. Body reads are bounded by actual streamed bytes at 65,536 and have a 30-second total deadline. JSON media type is parsed (application/json or application/*+json; optional single UTF-8 charset only). Media-type checks precede body reads; size checks precede JSON parsing. Duplicate JSON object members are rejected. Raw `short_code` presence is retained and anonymous permission rejection precedes field validation. PATCH title presence distinguishes omitted/null/value; tags allow omitted/array but reject null. Query parsers are operation-specific, preserve multiplicity, validate percent/UTF-8 strictly, and retain separate scope authorization checks. Test-only decoder routes run through the production HTTP envelope and are not registered by the production router.
- Stateless cursors use released `chacha20poly1305` 0.11.0 XChaCha20-Poly1305 with 32-byte deployment key, fresh 24-byte OS nonce, authenticated `fc1<resource>.` associated data, deterministic typed JSON context digest, and typed/sort-compatible boundaries. Page size is excluded. Maximum decoded nonce+ciphertext+tag is 65,536 bytes; encoded cursor cap is 87,387 bytes, supporting an 8,192-byte raw grouping value with worst-case JSON escaping and metadata. No credential, credential digest, or configuration secret enters cursor context/payload.
- Response DTOs and shared collection envelopes are in `http/dto.rs`; checked SQLx record conversions verify role/method/device, digest length, tags/search fields, signed counters, microsecond timestamps, host-mask `inet`, mapped IPv6 normalization, and owner state. Short URLs use the configured origin and typed immutable code; historical visits use stored snapshots. `VisitCount` now aliases the shared checked `domain::count::Count` primitive. No schema change/migration was needed; `migrations/0001_initial.sql` is unchanged.
- `tests/openapi_contracts.rs` loads the actual OpenAPI 3.1 file using `yaml_serde` 0.10.7, checks every local reference and operation ID, resolves component schema references, and validates representative requests/responses using pinned `jsonschema` 0.60.2 Draft 2020-12. This is a Part 03 schema contribution only; routes/headers/release conformance remains Parts 04–11.

### Changed files

`Cargo.toml`, `Cargo.lock`; new `src/domain/{mod,count,ids,secret,short_code,text,time,url}.rs`; `src/config.rs`; `src/db/records.rs`; new `src/http/{body,dto,query,rejection,test_support}.rs` and `src/http/mod.rs`; new `src/pagination/{mod,codec,context}.rs`; `src/lib.rs`; new `tests/{domain_contracts,http_decoding,cursor_codec,openapi_contracts}.rs`; `docs/{domain-contracts,configuration,database,development}.md`; `scripts/check.sh`; this progress record and `spec/implementation/acceptance-map.md`. No normative business/OpenAPI edit, migration, production business route, service, bootstrap execution, or later-stage feature was added.

### Verification evidence

- `podman info`; `podman machine list` — passed; `podman-machine-default` was running.
- `sh scripts/postgres.sh ready` — passed before and after the full test gate. The existing pinned PostgreSQL 17.6 harness accepted connections. `tests/foundation_postgres.rs::readiness_tracks_schema_and_recovers_after_postgres_restart` passed and restarted only the harness container while preserving its owned volume.
- `sh scripts/check.sh` — final run passed. It ran `cargo fmt --all -- --check`, `cargo check --workspace --all-targets --all-features`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, and `cargo test --workspace --all-targets` against the real PostgreSQL harness. Results: 67 library tests, 2 coordination, 6 cursor-codec, 5 domain-contract, 2 foundation HTTP, 1 PostgreSQL restart/readiness, 2 process, 5 HTTP-decoding, 7 migration, 3 OpenAPI-schema, and 4 persistence-constraint tests passed (104 total; binary target 0; none ignored/skipped).
- New suite entry points: `tests/domain_contracts.rs::{validated_domain_values_keep_their_scalar_and_canonical_invariants,url_normalization_and_persisted_snapshot_checks_are_precise,account_and_link_dtos_expose_exact_fields_and_required_nulls,visit_and_report_conversions_keep_snapshot_nulls_counts_and_bare_ip,time_intervals_are_utc_microsecond_normalized_and_checked}`; `tests/http_decoding.rs::{body_limit_is_enforced_on_actual_streamed_bytes_at_both_boundaries,content_type_json_errors_duplicates_and_unknown_fields_use_contract_statuses,anonymous_feature_permission_uses_raw_presence_and_bodylessness_uses_received_bytes,query_decoders_are_operation_scoped_and_errors_remain_sanitized,failing_real_parser_and_crypto_paths_do_not_log_canary_values}`; all six `tests/cursor_codec.rs` cases; and all three `tests/openapi_contracts.rs` cases. The new unit suites add URL repair/limits, Unicode idempotence/expansion, credential KAT/random failures/redaction, body/query boundaries, authenticated malformed cursor payloads, and DTO conversion checks.
- Existing real-database regressions also passed, including `tests/migrations.rs::concurrent_migration_processes_create_one_consistent_history`, `tests/migrations.rs::migration_rejects_dirty_mismatched_and_newer_history_without_repairing_it`, all four `tests/persistence_constraints.rs` cases, both `tests/coordination.rs` cases, both `tests/foundation_http.rs` cases, and both `tests/foundation_process.rs` cases.
- `cargo metadata --format-version 1` — 345 resolved packages; no package declares `rust_version` above 1.94.0. `rustc 1.99.1` and `cargo 1.99.1` were used. An actual Rust 1.94 compiler run is not claimed (rustup unavailable).
- `sh -n scripts/check.sh scripts/postgres.sh`, `git diff --check`, and `git diff --cached --check` — passed during review. The complete staged diff was reviewed against the base; no migration or generated/secret files are staged.
- Two intermediate `sh scripts/check.sh` runs exposed a missing unit-test `Url` import/unused `Uuid` import and then Clippy's explicit loop-counter lint. Both causes were fixed without weakening assertions; the final full gate above passed.
- `npx --yes @redocly/cli@1.34.5 lint --extends=minimal spec/openapi.yaml` was attempted but npm registry access returned HTTP 403. This optional external CLI was not a required check; reproducible locked YAML/JSON Schema validation against the checked-in schemas passed. Full route/release conformance remains Part 11.

Acceptance contributions are mapped to exact evidence for scenarios 10–12, 19 (host/mapped-IP conversion only; trusted proxy resolution remains Part 05), 23 (DTO/time/cursor primitives; report SQL remains Part 09), 41 (domain/null DTOs; authorized service flows remain Part 06/08), 45 (canonicalization/presence; persistence remains Part 06), 47–49 (query/cursor primitives; database filtering/keyset traversal and cross-instance endpoint flow remain Parts 08/11), and 51 (OpenAPI component/reference/schema fixtures only; route/release conformance remains Parts 04–11).

Unexecuted required checks/blockers: **none**. PostgreSQL regressions were not skipped. Later-stage authentication/accounts (Part 04), client-IP/rate services (Part 05), link services (Part 06), redirects/analytics (Part 07), discovery/report SQL (Parts 08–09), OTLP/release verification (Parts 10–11) remain unimplemented here.

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
