# Part 13 — Liveness independent of business-request admission

**Dependencies:** part 12 verified and merged into local `main`. Start this task
from that integrated baseline in a separate agent session.

**Normative references:** business sections 11–12 and 14.3; OpenAPI
`getLiveness`, `getReadiness` and common method/error/header rules; acceptance
scenarios 31, 36, 38 and 51.

**Part branch:** `implementation/13-liveness-admission`.

## Outcome

The exact liveness route remains promptly responsive while the existing business
in-flight and waiting capacities are exhausted. Readiness continues through its
existing admission path and retains its schema/database/accepting-state checks.
Business admission bounds and overload responses remain effective.

## Finding and approved contract change

At review baseline `0d4cdcc499f02edaedef70ffb332012935a8af9e`, the router applies
`admit_requests` to both health endpoints and business routes. A local production
process configured with `FERL_HTTP_MAX_IN_FLIGHT=1` and
`FERL_HTTP_MAX_WAITING=0` returned liveness 200 before a database-waiting request,
503 while that request held the permit, and 200 after it finished.

This behavior is expressly permitted by the current business section 14.3 and
the OpenAPI liveness description. It is not a pre-existing contract violation.
On 2026-11-06 the owner approved the narrow change: **isolate liveness only** and
update the corresponding normative text. The owner did not select readiness
isolation. Make that approved contract change explicit in the implementation
commit; do not silently implement behavior inconsistent with the documents.

The permit values above are a deterministic review configuration, not proposed
new defaults. Part 12 owns stdout backpressure. Additional architecture testing,
hosted CI, restore exercises and load/capacity redesign are outside this task.

## Files and responsibilities

- `src/http/router.rs`: exact route composition and admission placement.
- `src/app.rs`: existing admission state/cancellation where integration requires it.
- `src/http/request_id.rs`: preserve the common envelope and instrumentation.
- `src/main.rs`, `src/shutdown.rs`, `src/db/mod.rs`: inspect existing health and
  lifecycle integration; change only what the selected admission fix needs.
- `tests/foundation_http.rs`, `tests/foundation_process.rs`,
  `tests/telemetry_signals.rs` and existing router/process support: saturation,
  health contracts, recovery and withdrawal evidence.
- `tests/openapi_{contracts,release}.rs`: actual response and contract consistency.
- `spec/server-business-logic.md`, `spec/openapi.yaml`,
  `docs/{operations,configuration,observability}.md`: the precise approved change.
- `spec/implementation/{progress,acceptance-map}.md`: evidence and Git handoff.

## Tasks

### 1. Reproduce admission-coupled liveness

Add a controlled regression that holds ordinary application work in the existing
in-flight capacity and proves liveness currently waits or receives the admission
503. Cover both exhausted admission with zero waiting capacity and occupied active
slots with admitted waiters. Establish that the holders/waiters have actually
entered before probing; do not rely on an arbitrary sleep or a burst that might
never fill the intended capacity.

Use existing semaphore/barrier, observed database-lock and bounded child-process
patterns. Keep logging consumed and healthy in this task's isolated saturation
test. The stdout regression remains part 12's separate proof obligation.

### 2. Isolate only the exact liveness route

Adjust the existing router/admission composition so liveness does not acquire or
wait for business in-flight/waiting permits. Retain the normal liveness handler;
do not manufacture unconditional 200 responses ahead of its method/query rules.

Preserve:

- `GET /api/health/live` returning exactly `{"status":"ok"}` while the listener
  serves requests, regardless of database/Collector availability or business
  admission saturation.
- Credential ignoring, query rejection, method/Allow behavior and zero HEAD body
  bytes under the existing wire contract.
- The common fresh `X-Request-ID`, `Cache-Control: no-store`, error sanitization
  and safe request instrumentation on liveness responses, including invalid calls.
- Exact raw route ownership. Similar prefixes, extra path segments and encoded
  aliases must not create new admission bypasses or successful liveness aliases.

Readiness must remain on the existing admission path. When admission is exhausted,
it may still return structured 503 `service_unavailable`. When its handler runs,
its current readiness conditions and exact healthy/unready representations apply.
Do not add overload state to those conditions or remove any existing database,
schema or draining check.

Keep the existing configured business capacities, excess-work rejection and
cancellation behavior. Use the smallest change consistent with the router's
patterns; no new health listener, endpoint, configuration knob or background queue
is authorized. Ask before expanding that scope.

### 3. Preserve bounded lifecycle and align contracts

Liveness must remain observable during the configured readiness-withdrawal phase
even with business work held. Readiness remains unready during drain when its
handler executes. Once the listener is removed, continued probe reachability is
not promised. Existing forced cancellation, analytics drain, telemetry flush and
total process-exit bounds must continue to work after the admission change.

Amend business section 14.3 and the `getLiveness` OpenAPI description to remove the
permission for a business-admission-exhaustion 503 on liveness. Review its response
map for consistency with actual remaining error paths. Preserve shared response
components, common method/framework errors, operation IDs and readiness's overload
allowance. Update the operator passages that currently describe global overload
as applying to either health route. Record this as the owner-approved contract
resolution in progress.md; retain the historical decision as history.

## Verification gate

1. Demonstrate the focused saturation regression failing on the pre-fix part 12
   baseline and passing after this change.
2. Under controlled exhausted business admission, assert liveness returns promptly
   with 200 and exact body/headers while an excess business request and readiness
   retain their existing structured overload 503. With admitted waiters, prove
   liveness does not join that queue. Release the holders and verify recovery and
   permit reuse without leaked capacity.
3. Test liveness method/query rejection and exact-path boundaries under saturation;
   retain HEAD/Allow/request-ID/no-store rules. Ensure a broad prefix bypass or a
   hardcoded unconditional success could not pass these tests.
4. Add or extend a real `ferl-server` TCP-process test using an isolated database
   or a controlled stalled database peer as appropriate. Observe a held dependent
   request and responsive liveness on separate connections. An unavailable-database
   control must not be presented as evidence of database-dependent API success.
5. Exercise schema/database unready and recovery, readiness withdrawal while work
   is held, and measured natural process exit within the existing total budget.
   Retain part 12's blocked-stdout regression and existing Collector/privacy tests.
6. From clean committed part-branch inputs, run `sh scripts/check-openapi.sh` and
   `sh scripts/check.sh` using `docs/development.md`. The latter includes the common
   locked Rust/PostgreSQL/process/packaged local regression gate. No additional
   architecture or hosted-CI gate is required for this task. Focused development
   checks may precede the closing gate; record exact results and avoid repeating
   unchanged passing suites. Preserve unrelated user files and use a clean worktree
   for clean-source packaging if needed. Record a failed prerequisite as blocked
   rather than skipping coverage or weakening fixture protections.

**Handoff:** record the narrow approved contract amendment, admission placement,
controlled old/fixed evidence, lifecycle results and exact commands. Update the
acceptance map with concrete tests and progress.md with this task's base/verified
commits. Commit only this fix and its tests/contracts/docs on its own branch, then
perform the explicit no-ff/no-commit merge into local `main` from git-workflow.md.
Mark part 13 Complete only in that verified integration. Report the merge hash,
ancestry and final Git state, retain the branch, and stop after this task.
