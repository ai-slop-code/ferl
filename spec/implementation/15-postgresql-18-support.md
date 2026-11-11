# Part 15 — PostgreSQL 18 support

**Status:** Planned. This file is an implementation assignment for a Rust AI
agent. Its findings come from source inspection; PostgreSQL 18 compatibility
has not been executed or verified during specification preparation.

**Dependencies:** the integrated backend and runtime follow-ups through part 14.
Read the [shared agent contract](00-agent-contract.md),
[architecture](architecture.md), [Git workflow](git-workflow.md), and current
[progress record](progress.md) before implementation. Use the published-history
integration entry at the top of progress.md when interpreting older handoffs.

**Review baseline:** `d1d586546eb061a0edc6ce48638ae22fdab6a46f`, reviewed on
2026-11-11. Reinspect intervening changes before applying this assignment.

**Normative references:** [business specification](../server-business-logic.md)
sections 5.2, 7, 9, 12, 13–14 and the persistence/concurrency acceptance scenarios;
[OpenAPI](../openapi.yaml) for existing HTTP and health behavior. PostgreSQL's
major version is an implementation compatibility policy, not an HTTP API version.

**Part branch:** `implementation/15-postgresql-18-support`.

## Outcome and scope

Support PostgreSQL **17.x and 18.x** with the same `ferl-server` executable,
embedded schema, commands, and API. Interpret the request's “psql version 18” as
PostgreSQL 18 server compatibility and compatible operator client tooling.
Retaining 17.x is the additive-support assumption for this assignment.

`migrate`, `bootstrap-admin`, serving, schema monitoring, and packaged execution
must work against PostgreSQL 18. Existing fail-closed behavior applies to every
other major version. `psql` is the operator/test client; production Rust database
access uses SQLx and must continue through the existing SQLx connection paths.

Keep the implementation within the current modules, concrete repository
functions, environment-driven Podman scripts, isolated-schema helpers, and
process tests. Extend those patterns directly. No database adapter framework,
alternate migration engine, new production database-version setting, or separate
PostgreSQL 18 application path is needed. PostgreSQL 18 feature adoption, such as
UUIDv7, generated search columns, OAuth, or database-side case folding, is outside
this compatibility increment.

## Findings in the current codebase

| Area | Observed implementation | Implementation consequence |
|---|---|---|
| Version policy | `src/db/migrate.rs` has `SUPPORTED_POSTGRES_MAJOR = 17`; `is_supported_postgres_version_num` accepts `[170000, 180000)`. Its unit test explicitly rejects `180000`. | Extend this single policy to admit both majors and retain a finite upper bound. |
| Migration gate | `run_migrations_inner` calls `ensure_supported_postgres_version` before acquiring the migration advisory lock or running SQLx migrations. | Preserve rejection before migration DDL/history creation. |
| Serving and bootstrap | `verify_connection` uses the same predicate; the schema monitor and `AppState::verify_schema` use compatibility verification. `src/accounts/bootstrap.rs::run_inner` verifies compatibility before opening the bootstrap transaction. | Updating only migration admission is insufficient; keep all paths on the shared policy. |
| Schema identity | `LATEST_SCHEMA_VERSION` is 2, `MINIMUM_SUPPORTED_SCHEMA_VERSION` is 1, and the verifier requires both embedded migrations/checksums. A v1-only database is not ready. | PostgreSQL major 18 does not mean ferl schema 18 or a new schema migration. |
| Driver | `Cargo.toml` and `Cargo.lock` use SQLx 0.9.0, PostgreSQL, Tokio, and Rustls. `src/db/pool.rs` establishes bounded lazy pools and session timeouts. | Verify this locked driver first; no dependency change is established by this review. |
| Shared fixture | `scripts/postgres.sh` pins PostgreSQL `17.6-alpine3.22` by digest, with a named container/volume and `/var/lib/postgresql/data` mount. | Add a separately identified 18 fixture using the same lifecycle and ownership checks. |
| Rust fixture checks | `tests/support/database.rs` checks a fixed container name, digest, volume, mount, loopback port, and URL; scenarios use a process mutex plus shared file lock. | Fixture selection must update validation as well as startup. |
| Restart/network consumers | Several tests and `tests/support/podman_server.rs` name the 17 container directly or use `database_support::CONTAINER_NAME`. | Every restart and network attachment must target the selected, verified fixture. |
| TLS fixtures | `tests/release_tls.rs` pins 17 and mounts a bounded tmpfs at `/var/lib/postgresql/data`; `scripts/check-tls.sh` separately pins its image. | Both the image and data location must agree for 18. |
| Candidate image gate | `scripts/check-image.sh` pins 17, asserts server major `17`, and records `postgresql-17-tls-fixture`. `.github/workflows/verify.yml` also pulls/names 17 explicitly. | Packaged support must be exercised on 18 as well as through native Rust tests. |
| Historical binary | `tests/legacy_compatibility.rs` builds retained commit `9bcf7c0c021c994e76d0696633fe822f0a49a5e1`. Inspection of that commit confirms its 17-only gate. | Its original v1-to-v2 scenario must run on 17; it cannot simply be pointed at 18 and expected to migrate. |
| Query-plan evidence | Discovery and report test `print_plan` helpers decode row counts with `as_i64().unwrap_or_default()`; discovery also asserts specific index names. | Check PostgreSQL 18 plan output and choices without fabricating zero-valued observations or weakening query guarantees. |

### PostgreSQL 18 changes relevant to this repository

Use the upstream references below and the selected image's actual behavior:

- The official image uses `PGDATA=/var/lib/postgresql/18/docker` and declares its
  data volume at `/var/lib/postgresql` for 18. The existing 17 mount path is not
  interchangeable with this layout. This also affects the TLS fixture's tmpfs.
- PostgreSQL 18 stores column `NOT NULL` constraints in `pg_constraint`. The
  existing verifier counts only explicitly named constraint pairs and checks
  nullability separately through `information_schema.columns`; extra catalog
  rows alone do not establish a bug. Verify the real queries before changing them.
- PostgreSQL 18 exposes constraint enforcement metadata and supports
  `NOT ENFORCED` constraints. Check its effect on the existing required-constraint
  verification and enforcement tests; presence of a named row must not be used
  as evidence that a deliberately non-enforced required constraint protects data.
- B-tree skip scans and other planner changes can select different valid access
  paths. `EXPLAIN` can report fractional row counts. Inspect actual JSON output
  when adapting plan evidence.
- PostgreSQL 18 adds protocol 3.2 and deprecates MD5 authentication. Verify the
  locked SQLx client's actual connections, SCRAM, TLS, cancellation, and fault
  tests; supporting this server does not itself require adopting a new protocol
  or authentication mechanism.
- A PostgreSQL major upgrade is a cluster operation. Reusing a PostgreSQL 17
  data directory with an 18 executable is not a cluster upgrade.

References consulted during this review:

1. [PostgreSQL 18 release notes](https://www.postgresql.org/docs/18/release-18.html)
   — compatibility, constraints, planner, protocol, and client changes.
2. [Official PostgreSQL image documentation](https://github.com/docker-library/docs/blob/master/postgres/README.md#pgdata)
   — version-specific `PGDATA` and volume destinations.
3. [PostgreSQL cluster upgrade documentation](https://www.postgresql.org/docs/18/upgrading.html)
   — operator reference for major upgrades.

## Files and responsibilities

- **Runtime policy and verification:** `src/db/migrate.rs`; inspect integration
  in `src/app.rs`, `src/db/mod.rs`, `src/db/pool.rs`, and
  `src/accounts/bootstrap.rs`. Preserve `SchemaIssue`/`VerificationError` and
  sanitized error mapping in `src/db/error.rs` and `src/telemetry/logging.rs`.
- **Schema:** inspect `migrations/0001_initial.sql` and
  `migrations/0002_discovery_indexes.sql`. Preserve their bytes and checksums.
- **Persistent fixtures:** `scripts/postgres.sh`, `tests/support/database.rs`,
  and the environment setup in `scripts/check.sh`, `scripts/check-openapi.sh`,
  `scripts/check-proxy.sh`, and `scripts/check-release.sh`.
- **Fixture consumers:** `tests/support/podman_server.rs` and
  `tests/{foundation_postgres,authentication,bootstrap,link_process,discovery_process,report_process,rate_limit_process,telemetry_signals}.rs`.
  Search all consumers of `CONTAINER_NAME`, `POSTGRES_CONTAINER`, and the literal
  `ferl-stage01-postgres`; this list is a starting inventory, not a replacement
  for reference checking.
- **Packaged TLS and image execution:** `tests/release_tls.rs`,
  `scripts/check-tls.sh`, `scripts/check-image.sh`, and the existing verification
  jobs in `.github/workflows/verify.yml`.
- **Behavioral evidence:** `tests/migrations.rs`, `tests/foundation_postgres.rs`,
  `tests/persistence_constraints.rs`, `tests/legacy_compatibility.rs`, and the
  existing bootstrap, process, concurrency, and failure suites.
- **Plan evidence:** test helpers in `src/links/list_repository.rs` and
  `src/analytics/reports/tests.rs`; inspect the existing production builders in
  `src/tags/repository.rs` and `src/analytics/report_query.rs` as needed.
- **Documentation:** `readme.md`, `docs/{database,development,release,operations,cicd}.md`,
  relevant current version wording in `docs/creation-allowance.md` and
  `src/rate_limit/repository.rs`, plus the implementation index/progress/acceptance
  records. Preserve historical verification claims as dated evidence.

## Tasks

### 1. Extend the existing version policy

Make `is_supported_postgres_version_num` accept PostgreSQL 17.x and 18.x only:
numeric server versions from `170000` inclusive to `190000` exclusive. Give the
constant declaration(s) names that accurately describe the two-major policy;
keep the predicate in its existing module and use it at both current call sites.
Continue reading numeric `server_version_num` from the connected server.

Update the existing unit test to cover `170000`, the retained `170006` fixture,
the last value below `180000`, `180000`, a representative 18 minor, and the last
value below `190000`. Reject `169999`, `190000`, a later major, zero, negative
values, and `i32` extrema. These boundaries must catch both accidental 17 removal
and accidentally accepting all future versions.

Keep unsupported-major migration failures as
`MigrationFailure::Incompatible(SchemaIssue::UnsupportedDatabaseVersion)` and
the safe category `postgres_version_unsupported`. Serving continues to be live
but unready after failed compatibility verification. Bootstrap retains its
existing `SchemaIncompatible` mapping and one-time output contract.

Keep schema version 2 and both required migration records. Retain the standalone
migration connection, advisory-lock key, deadline ordering, transactional DDL,
history verification, and readiness-generation protection. No migration is
justified merely by widening the server-version gate.

### 2. Extend the existing harness to select either supported major

Use the existing environment-variable and explicit Podman command style. Add
one **test-harness-only** selector, `FERL_TEST_POSTGRES_MAJOR`, accepting exactly
`17` or `18`, defaulting to `17`. This is a proposed addition, not an existing
setting. Reject invalid values before operating on containers. Keep it out of
production `Config`, the public API, and ordinary deployment configuration.

Use two explicit fixture configurations in the current script/helper locations:

| Selection | Persistent container | Persistent volume | Data mount | Default host port |
|---|---|---|---|---:|
| 17 | `ferl-stage01-postgres` | `ferl-stage01-postgres-data` | `/var/lib/postgresql/data` | 55432 |
| 18 | `ferl-stage01-postgres18` | `ferl-stage01-postgres18-data` | `/var/lib/postgresql` | 55433 |

Preserve the 17 image pin. Resolve a published PostgreSQL 18.x Alpine image and
record its full tag and actual registry digest before implementation is
considered verified. Check availability for the existing local fixture and
Linux/amd64 packaged-test platforms. This spec does not supply an unverified
digest. Use explicit verified pulls and the existing `--pull=never` startup.

Apply the selected port default consistently in all gate scripts; keep
`FERL_POSTGRES_PORT` as the explicit override and export the matching existing
`FERL_TEST_DATABASE_URL`, `FERL_TEST_POSTGRES_CONTAINER`, and
`FERL_TEST_POSTGRES_PORT`. Propagate the selector to Rust test processes.

Extend `IsolatedTestDatabase` to validate the exact selected name, pinned digest,
volume/mount, label, and loopback port. Also observe `server_version_num` from the
actual connection and assert its major matches the selection. The environment
variable must not turn the current allowlist into permission to operate on an
arbitrary named container or database.

Reuse the process mutex, shared file lock, generated schema prefix,
`options[search_path]`, bounded waits, and schema cleanup. Keeping the existing
shared lock for sequential major runs is sufficient. Use concrete helpers in
`tests/support/database.rs` for selected fixture identity; migrate restart and
network consumers to them rather than adding parallel helper frameworks.
Select configuration before launching tests; do not mutate process environment
from concurrent Rust tests.

The 18 fixture must initialize its own volume. `start`, `ready`, `stop`, and
`cleanup` operate only on the selected owned resources. Switching selections
must neither repurpose nor delete the retained 17 volume. Verify actual
`data_directory` is under the selected persistent mount, and exercise persistence
after restart. In an owned test fixture, also verify recreation of its container
with the same volume retains data; restart alone cannot detect an unintended
anonymous-volume dependency.

### 3. Verify the real schema, driver, and query behavior

Run existing migration and foundation tests on 18 after extending the fixture.
Establish these observations with the production paths:

1. Before migration, compatibility reports missing history rather than unsupported
   PostgreSQL. Fresh and repeated CLI migrations succeed, and history contains
   exactly versions 1 and 2 with the embedded checksums.
2. Concurrent migration processes serialize successfully. Existing lock-timeout,
   failed-DDL rollback, populated v1-schema-to-v2-schema, corrupt-history,
   missing-object, nullability, and wrong-index cases retain their outcomes.
3. Bootstrap commits once; serving becomes ready through the real verifier;
   database outage and schema drift make it unready; recovery is observed.
4. Existing constraint and transaction tests establish uniqueness, cascades,
   permanent reservations, immutability, counter consistency, and durable
   allowance behavior on both server majors.

Inspect `information_schema.columns`, the named `pg_constraint` query, and
`pg_get_indexdef` results on 18. Make only demonstrated compatibility corrections
inside the existing verifier. For the new non-enforced-constraint state, use an
isolated-schema drift control and ensure a required constraint with enforcement
disabled is rejected. Reuse the current schema-incompatibility categories. Any
18-only catalog access must be selected after version detection so its SQL also
remains valid when running against 17. Do not reduce verification to a table count
or accept every schema once the version check passes.

Run the current rate-clock, IP, discovery, pagination, analytics, and failure
tests with the locked SQLx version. In particular preserve exact microsecond
rate arithmetic, C-collated ordering, NULLS LAST keysets, IPv4/IPv6 `inet`,
application-owned Unicode normalization, prepared/bound queries, commit-ambiguity
handling, and report snapshots. Change dependencies only for a reproduced driver
incompatibility, with a documented minimal remedy and the existing compiler gates.

Capture the production query-plan tests on both majors. Update the existing
test-only plan printers to retain numeric row observations when PostgreSQL
returns fractions; an unsupported conversion must not silently report zero.
If an exact index-name assertion fails, inspect the actual plan and its bounded
ordering/filtering behavior. Adapt that assertion only with equivalent evidence;
do not disable planner features or delete indexes just to recreate a 17 plan.

### 4. Preserve meaningful retained-binary coverage

Keep the retained Part 07 commit and build procedure intact. On PostgreSQL 17,
retain the original real-binary sequence in `tests/legacy_compatibility.rs`:
old binary migrates/serves v1, current binary upgrades to v2, old binary reports
`schema_version_newer_than_binary`, current binary becomes ready.

Make this suite execute explicit version-appropriate assertions on PostgreSQL
18: the unmodified old binary's migration must fail with
`postgres_version_unsupported` before creating migration history or application
tables. Its serving process must report that category and remain live/unready.
Then the current binary must migrate the same fresh isolated schema and become
ready. Use the existing bounded command and `ServingProcess` helpers.

Update test naming to describe these paths accurately. A branch that returns
early, an ignored test, or patching the old source to accept 18 is not evidence.
Both major-specific runs are required; PostgreSQL 18's old-binary rejection does
not replace PostgreSQL 17's schema-upgrade compatibility test.

### 5. Exercise packaged TLS and candidate images on both majors

Honor the same selector in `scripts/check-tls.sh`, `tests/release_tls.rs`, and
`scripts/check-image.sh`. Keep each disposable fixture's existing run-owned
network, cleanup, certificates, resource bounds, and private configuration.
For 18, move the TLS database tmpfs to `/var/lib/postgresql` so the actual
`PGDATA` remains under its existing size bound. Inspect and clean any resources
created by the selected image according to the existing owned-resource pattern.

Retain positive `sslmode=verify-full` migration/bootstrap/readiness and negative
untrusted-CA/hostname controls. Observe the actual server major in each packaged
fixture and record it in the result. Keep application connections on SCRAM/TLS.

Adapt the TLS cancellation regression's persistent-container/volume identity
checks to the selected fixture. Retain evidence that cleanup leaves unrelated
persistent fixtures intact. Ensure the gate starts/readies the persistent fixture
that this test requires: currently the standalone `check-tls.sh` assumes the
Stage 01 fixture already exists while the cancellation test requires it.

Extend the existing `.github/workflows/verify.yml` jobs using their current
script-invocation pattern: run the release gate for 17 and 18 and check the same
built candidate image against both majors. Keep the full-history checkout and
existing build/artifact identities. Preserve both major-specific image check
summaries; the current `check-summary.json` is overwritten on each invocation.
Publication must depend on both checks succeeding. No new CI framework or
automatic release/publication operation is part of this assignment.

### 6. Align current documentation and handoff

Update current operator requirements from 17-only to 17.x/18.x after verification.
Document that the supported version is the connected server's version;
`psql --version` alone does not establish it. Use PostgreSQL 18 client tools for
the 18 setup examples, including suitable `pg_dump`/`pg_restore` versions where
backup commands are described.

Document the two local fixture selections, pins, ports, mount paths, prerequisites,
and exact gate invocations. Keep historical release reports and the historical
`docs/publication-cicd-agent-handoff.md` snapshot accurate to their original runs;
record fresh evidence with its actual major rather than rewriting old results.

In `docs/database.md` and `docs/release.md`, distinguish a ferl schema migration
from a PostgreSQL cluster upgrade. Describe installing a binary that supports
both majors before moving to 18, using the operator's PostgreSQL upgrade process,
and rechecking ferl migrations/readiness afterward. Link upstream major-upgrade
instructions. An application `migrate` command or container image swap cannot
convert a 17 cluster, and a 17-only application binary is not a rollback target
while the database remains on 18. This task requires local compatibility tests
and documentation, not a production upgrade or external restore exercise.

Add this part to the implementation index and update progress.md and
acceptance-map.md with the actual changes, test names, major-specific evidence,
and any reproduced compatibility corrections.

## Verification gate

Use focused existing tests during implementation, then run the existing local
gate from clean committed inputs as required by the packaging scripts:

1. Run `FERL_TEST_POSTGRES_MAJOR=17 sh scripts/check.sh` and
   `FERL_TEST_POSTGRES_MAJOR=18 sh scripts/check.sh` sequentially. Each invocation
   must execute formatting, locked all-target/all-feature check and Clippy,
   all-target tests, doctests, process/Collector checks, and packaged suites on
   the selected fixture. Label evidence with the observed server version/digest.
2. Verify the independently callable OpenAPI, proxy, release, and TLS scripts
   propagate the selector. Run focused entrypoints where needed to establish
   their setup behavior; do not repeat unchanged passing suites solely to
   accumulate command output.
3. Run `scripts/check-image.sh` against the same verified candidate metadata on
   both majors using the existing platform/build procedure. Record local
   platform prerequisites explicitly if this gate cannot execute on the host.
4. Check selector rejection and ownership/mount/port mismatch failure behavior,
   selected-fixture restart/recreation persistence, and cleanup isolation.
5. Confirm existing migration files and checksums are identical to the review
   baseline. Run `git diff --check`. If dependencies change for a demonstrated
   incompatibility, also run the existing `sh scripts/check-msrv.sh` gate.

Acceptance requires positive PostgreSQL 18 evidence for migration, bootstrap,
readiness/recovery, CRUD/redirects, discovery/reports, durable coordination,
packaged TLS, and persistence; passing PostgreSQL 17 regressions; finite numeric
version boundaries; and genuine retained-binary coverage on each major. Use the
existing tests for these behaviors rather than cloning every suite into an
18-specific test file. Missing fixtures, image pins, compiler prerequisites,
retained Git history, or required test execution are blockers, not skipped passes.

**Handoff:** record the selected image tag/digest and observed server/client
versions, unchanged schema identity, changed files, compatibility findings,
exact commands/results per major, and any remaining blockers. Follow the existing
part-branch verification and explicit no-ff/no-commit local merge procedure.
Mark part 15 Complete only after its implementation and required verification
are integrated. Report the verified implementation commit, merge commit, and
final Git state; retain the branch and stop after this part.
