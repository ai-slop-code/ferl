# Implementation progress and agent handoff

Initial state: instructions prepared; backend implementation had not started. Part 01 is now being implemented on its dedicated branch.

Status lifecycle: Planned → In progress → Verified (awaiting merge) → Complete. Use Blocked when a required implementation, verification or Git integration step cannot finish. Complete requires a committed merge into local `main`, as defined in [git-workflow.md](git-workflow.md).

## Part status

| Part | Status | Evidence / blocker |
| --- | --- | --- |
| 01 Foundation | Blocked | Implementation and non-database checks are complete; the required real-PostgreSQL outage/recovery test cannot run because the Podman VM rejects the registry TLS chain. Work is preserved on `implementation/01-foundation`; not merged. |
| 02 Persistence | Planned | — |
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
- Readiness fails closed until a schema verifier marks compatibility; Part 01 does not claim `SELECT 1` is schema proof. Part 02 must install the migration verifier before the server can report ready against a real deployment schema.
- The Part 01 PostgreSQL readiness test manually marks its test state schema-compatible to isolate the live database probe and restart recovery. It does not prove a schema version; Part 02's real verifier and migration tests remain required.
- The Podman harness uses `docker.io/library/postgres:17.6-alpine3.22@sha256:ef257d85f76e48da1c64832459b59fcaba1a4dac97bf5d7450c77753542eee94`, a loopback-published port, and persistent volume `ferl-stage01-postgres-data`. Registry metadata confirmed the active arm64 image digest, but Podman could not pull it because its VM does not trust the environment's registry certificate chain.
- The failed `start` attempt created the labeled, harness-owned persistent volume but no container. The volume is retained for retry; no unrelated Podman resource was changed.

## Contract conflicts

No contract conflict was found for Part 01. The business specification and OpenAPI agree on the health paths/bodies, request ID, no-store headers, and structured error shape. A future entry must identify both passages, the precise incompatibility, and explicit resolution before affected behavior is implemented.

## Part 01 handoff — 2026-10-27

Status: **Blocked** (verification prerequisite unavailable; not verified and not merged).

Part branch: `implementation/01-foundation`

Base commit: `40b3ae713f0cd9bd2389f872d2953d29ef38f38`

Implementation checkpoint commit: `195376016909d356d645b4bc16657a13a25a572d` (`wip(part-01): implement foundation pending PostgreSQL gate`).
Integration state: not merged; `main` remains at the reviewed specification baseline.

Implemented behavior:

- Added the library-driven `ferl-server` binary with default/explicit `serve`, plus clearly failing `migrate` and `bootstrap-admin --username ...` commands that do not access or modify the database yet.
- Added sanitized, typed startup configuration, normalized public origin, strict 32-byte base64url cursor key validation, trusted-proxy IP/CIDR parsing, bounded pool/HTTP concurrency/timeouts, and OTLP/resource/sampling settings. Secret-bearing types have redacted `Debug`; failures report only setting/category and static reason.
- Added a bounded, lazy PostgreSQL pool, statement/lock timeouts, an explicit fail-closed schema readiness state, and bounded graceful shutdown/drain behavior.
- Added an outer HTTP envelope that replaces incoming request IDs, emits the matching error-body ID, applies `Cache-Control: no-store`, strips every HEAD body, sanitizes framework plaintext/422 rejections, and logs only safe route templates and bounded attributes. Liveness is DB-independent; readiness requires accepting-work state, schema compatibility, and a bounded PostgreSQL probe.
- Added structured JSON stdout logging with dependency-target filtering, safe operation/outcome types, build-derived version metadata, a Podman PostgreSQL harness, and configuration/development instructions.

Changed files: `.gitignore` (baseline), `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml`, `build.rs`, `.env.example`, `src/{main,lib,app,config,error,shutdown}.rs`, `src/http/{mod,request_id,response,router}.rs`, `src/db/{mod,pool,migrate}.rs`, `src/telemetry/{mod,logging,metrics}.rs`, `tests/foundation_{http,postgres,process}.rs`, `scripts/{postgres,check}.sh`, `docs/{configuration,development}.md`, and this handoff plus `acceptance-map.md`.

Checks executed:

- `cargo fmt --all -- --check` — passed.
- `cargo check --workspace --all-targets --all-features` — passed.
- `cargo clippy --workspace --all-targets --all-features -- -D warnings` — passed.
- `cargo test --lib --test foundation_http --test foundation_process` — passed: 10 library tests, 2 HTTP tests, and 2 executable-process tests. Evidence includes configuration/error tests in `src/config.rs` and `src/error.rs`, HTTP envelope tests in `src/http/request_id.rs`, `tests/foundation_http.rs::{health_ignores_credentials_replaces_request_id_and_sets_common_headers,fallback_and_method_errors_are_json_and_head_is_bodyless}`, and `tests/foundation_process.rs::{executable_starts_without_database_and_stdout_excludes_secrets_and_raw_urls,unavailable_operator_commands_and_invalid_startup_fail_safely}`.
- `cargo test --test foundation_postgres --no-run` — passed compilation only; the database test was not executed.
- `sh -n scripts/postgres.sh && sh -n scripts/check.sh && git diff --check` — passed.
- `cargo tree -i opentelemetry` (and SDK/OTLP reverse-dependency checks) — confirmed a single compatible OpenTelemetry 0.33.1 family.
- `podman info` — passed with Podman 6.1.4; `podman machine list` showed the existing machine running.
- `sh scripts/postgres.sh start` — failed (exit 125) before creating a container: registry pull failed with `x509: certificate signed by unknown authority`. The host's `curl -I --max-time 15 https://registry-1.docker.io/v2/` succeeded TLS and returned the expected registry `401`; `podman machine ssh` showed the VM rejects the chain. No TLS verification was disabled and no trust store was modified.

Unexecuted required checks and reason:

- `cargo test --workspace --all-targets` and `tests/foundation_postgres.rs::readiness_tracks_schema_and_recovers_after_postgres_restart` are blocked until Podman can pull the pinned PostgreSQL image. The test deliberately fails rather than skips when harness variables/database are absent; no real-PostgreSQL result is claimed.
- The completion gate therefore remains blocked. An approved CA path/trust-store authorization or approved registry mirror is required to proceed; after restoring image pulls, run `scripts/check.sh`, review results, commit the verified part tip, and merge explicitly into `main`.

Acceptance evidence added: scenarios 27 and 31 have Part 01 process test references, with database recovery explicitly pending; scenario 37 root 404 and scenario 38 API-prefix/health behavior reference the HTTP/process suites. OpenAPI release validation (scenario 51), collector coverage, and broader business-operation portions remain assigned to later parts.

Remaining work: unblock Podman registry trust, execute the PostgreSQL restart/recovery test and full workspace test gate, then commit and integrate Part 01. Do not start Part 02 until those steps pass.

Next part: **02 — Persistence**, only after Part 01 is verified and merged.

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
