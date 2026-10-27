# Part 11 — Multi-instance correctness, contract conformance and release

**Dependencies:** parts 01–10 complete.

**Normative references:** all business requirements, especially sections 11.6, 13.1, 14–15; entire OpenAPI artifact.

## Outcome

Executable evidence covers all 51 acceptance scenarios, including at least two independent ferl processes sharing PostgreSQL. A clean checkout can build, migrate, bootstrap, run, verify and package the backend using documented commands.

## Files and responsibilities

- `tests/support/{process,proxy,collector,faults}.rs` and independent-process suites.
- `tests/{multi_instance,contract_conformance,proxy_routing,release_smoke}.rs` or equivalent explicitly runnable suites.
- `scripts/check-openapi.*`, schema fixture files/tool dependency locks and a consolidated check command.
- CI configuration for Rust, PostgreSQL, contract, collector/proxy and process tests.
- `Dockerfile`/release packaging as appropriate, `deploy/` examples, and `docs/{development,configuration,operations,release}.md`.
- Final updates to progress.md and acceptance-map.md evidence pointers.

## Tasks

### 1. Independent-process harness

Start two separately spawned `ferl-server` executables on distinct ports with isolated process memory, shared test database, identical public origin/business settings/cursor key and explicit trusted-proxy settings. Use the same migrations as production. Route requests deliberately to A/B rather than relying on random balancing.

Use the documented Podman fixtures for shared PostgreSQL and the OpenTelemetry Collector during local verification. The independent ferl executables may run directly on the host and connect through published ports. For containerized proxy fixtures, explicitly verify container-to-host connectivity and observed peer addresses on the local Podman setup before configuring trusted proxies. CI must provide an equivalently reproducible dependency setup; record its runtime and connection settings.

Capture structured stdout separately, bound every child wait, clean up only harness-owned processes/databases, and expose failure diagnostics without secrets. Use barriers/test-only IPC or controlled database locks for deterministic races. Fault injection controls must not become public HTTP operations or appear in the normal release configuration/binary surface.

Run the following scenario groups:

| Group | Required process-level evidence |
| --- | --- |
| Fresh visibility | Create through A, read/redirect/list/manage through B; edits, suspension, re-enable, deletion, key/token rotation become visible on the other process after commit |
| Stateless credentials/cursors | Keys/tokens survive process restart; link/account/tag/visit/breakdown cursors cross A/B with shared configuration and fresh authorization |
| Shared rate allowance | Split simultaneous account and effective-IP attempts across A/B; combined admits equal one allowance; rejection/Retry-After consistent; adding/restarting a process does not reset it |
| Global invariants | Race custom allocation, bootstrap and last-admin removal across executables; one global outcome, no orphan data or all-admin loss |
| Migration/startup | Concurrent migrate commands serialize; starting against missing/incompatible schema is never ready; successful deployment preserves previous data |
| Analytics | A/B worker inserts preserve counts and UUID idempotency; delete through A while B holds queued events and prove B cannot resurrect data |
| Removal/replacement | Observe A draining/unready, remove it, add a fresh C; B/C serve existing links, accounts, tokens, visits, reports and cursors without state transfer |

Verify stale-mutation ordering using both independent processes: invalidation committed before a waiting mutation's guarded revalidation prevents that mutation. Verify allowed already-resolved redirects can complete with an earlier snapshot.

### 2. OpenAPI validation and executable conformance

Select and pin tooling that genuinely supports OpenAPI 3.1 and its JSON Schema dialect. Validate YAML structure, every reference and schema consistency. Preserve operation IDs; don't regenerate a looser contract from implementation and treat it as authoritative.

Extract the documented `(method, path, operationId)` inventory and compare it with implemented routes. Check common unsupported-method behavior, including bodyless HEAD errors and Allow, even where a common error is described globally rather than repeated on every operation.

Maintain representative valid and invalid fixture requests/responses for every operation family. Validate response schemas, required fields/nulls, collection shapes, conditional anonymous token presence, status codes and headers. Explicitly test:

- Optional authentication only for eligible anonymous creation and public health/redirects.
- Bearer required/role and object authority elsewhere.
- Strict unknown fields, nullability and repeated scalar query handling.
- Canonical tags, nullable titles, complete Link records and immutable visit snapshots.
- Every collection's cursor/limit/filter rules and date formats.
- Exact error shape/codes with matching X-Request-ID; WWW-Authenticate, Retry-After and Allow where required.
- All 302, 204 and HEAD body rules; exact health success/unready schemas.

Schema validation alone cannot prove byte limits, canonicalization, feature permissions, request precedence or concurrency. Link those cases to their business suites rather than weakening the spec to make a schema-only validator sufficient. Add a required CI command that fails on invalid/unresolved OpenAPI or representative schema regressions.

### 3. Verify shared-origin proxy boundary

Supply a concrete reverse-proxy/static-host example and test it using a minimal separate static fixture. It must:

- Serve only the separate fixture at exact `/` and files under `/assets/*`.
- Dispatch exact `/api` and `/api/*` to ferl before generic code routing.
- Pass `/assets`, `/health` and `/api-sale` to short-code lookup without rewriting.
- Preserve raw code-path behavior; percent-encoded aliases or trailing slash must not become successful redirects through proxy normalization.
- Return 404 for missing assets, missing codes, unknown API routes and unsupported multi-segment paths; never apply a site-wide SPA history fallback.
- Establish/append X-Forwarded-For as documented and configure trusted proxy IPs explicitly. Direct untrusted spoofing must not affect effective-IP identity.

Document body/header/request-target limits. In particular, allow server-issued analytics cursors containing the maximum permitted grouping boundary to be followed through the supported proxy example. Use only fixture static files for this test, not an implementation of the future SPA.

### 4. Failure, upgrade and restart verification

Run bounded failure scenarios with separate resolution versus analytics write failures. Confirm dependent DB failures yield 503; successful resolution followed by queue/write failure still yields 302. Check collector outages and drain deadlines with both processes active.

Test restart and migration using a populated database containing accounts, current digests, owned/anonymous links, titles/tags, historical visits, nonzero counters, permanent deleted-code reservations, bootstrap completion and still-live rate charges. Preserve that data and configured cursor key. Confirm a deleted username can be reused without inheriting the old account UUID/links.

No test may satisfy restart persistence by repopulating fixtures after restart. No deployment step may silently bootstrap or rotate credentials.

### 5. Build/release and operator documentation

Produce a reproducible server build/release artifact containing the binary, migration support and `spec/openapi.yaml`. Document the release artifact's supported PostgreSQL version/extensions and runtime TLS/CA needs. A container may use multi-stage builds and a non-root runtime; it must expose the actual health paths and server command.

Document:

- Clean startup: provision PostgreSQL → configure stable secrets/origin → migrate once → bootstrap once → serve.
- Every setting's type/default/bound and which settings must match across instances; no secret values in examples.
- Provisioning and securely retaining the shared cursor key; changing it invalidates existing cursors and requires coordinated fleet configuration.
- Primary database requirement, connection-budget calculation across replicas, proxy trust and explicit route ownership.
- Rate attempts versus successful creations, anonymous-creation switch semantics and no total quota.
- Best-effort visits, indefinite retained history until deletion, immutable destination snapshots and eventual report consistency.
- Readiness, bounded drain, collector outages, schema migration rollout and backup/restore scope (including reservations/bootstrap state).
- How to run each required suite and validate OpenAPI from a clean checkout.

Avoid claiming linear scaling, exactly-once create requests, durable analytics queues or confirmed destination loads.

## Final verification gate

1. Run the common Rust checks, all database/HTTP suites, independent-process suites, collector/privacy/shutdown checks, proxy checks and OpenAPI validation through documented commands.
2. Audit [acceptance-map.md](acceptance-map.md): all 51 scenarios have concrete executable test locations and observed passing results. Fill in evidence locations, not generic assertions.
3. Perform a clean-checkout release smoke flow: migrate/bootstrap, create a titled/tagged link, list it, follow GET and HEAD, observe reports, rotate credentials, delete, and prove its code remains unavailable after restart.
4. Inspect the packaged release for the intended server binary, migrations and exact checked-in OpenAPI artifact.
5. Mark part 11 complete only when no required check is skipped or blocked. Record remaining non-required refinements separately from release behavior.

**Final handoff:** summarize implemented server capabilities, exact verification commands/results, release artifact locations, operator startup commands and any genuinely unresolved blocker. First-release readiness depends on this entire gate.
