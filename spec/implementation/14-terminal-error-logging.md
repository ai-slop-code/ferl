# Part 14 — Terminal server errors before diagnostic shutdown

**Dependencies:** parts 12 and 13 verified and merged into local `main`. Read the
shared agent contract, architecture, Git workflow, progress record and acceptance
map before implementing this bounded follow-up.

**Normative references:** business sections 14.1–14.3 and acceptance scenarios
29–31; the existing command exit/output contracts; parts 12 and 13's approved
diagnostic-loss, shutdown and health behavior.

**Part branch:** `implementation/14-terminal-error-logging`.

## Outcome

A terminal serving failure emits its existing sanitized error diagnostic before
telemetry and stdout are shut down. With a healthy consumer and sufficient
remaining drain budget, the process reports the cause and exits unsuccessfully.
The bounded, nonblocking stdout path and existing lifecycle deadlines remain
effective when the consumer is stalled.

## Finding and scope

Review baseline: `d1abbc6f309ac3e1eae28805aeb419064f677c9b` on 2026-11-08.

`serve()` calls `Telemetry::shutdown()` on its terminal paths. That method calls
`logging::finish_stdout()`, which permanently marks the diagnostic sink closing.
Only after `serve()` returns does `serve_command()` emit the final
`server terminated with a sanitized failure` event. `DiagnosticSink::submit()`
discards it because the sink is already closed. A second flush cannot recover a
record that was never accepted.

The review reproduced the following with the actual executable and a healthy,
actively drained stdout/stderr consumer:

| Condition | Observed exit | Observed diagnostics | Required result |
|---|---:|---|---|
| Configured listener port is already held by another local listener | 1 | Empty stdout and stderr | Existing sanitized `listener_bind_failed` error on the serving diagnostic stream, then unsuccessful exit |
| PostgreSQL URL has an invalid SQLx option, such as `sslmode=not-a-mode`, while ordinary startup settings are valid | 1 | Empty stdout and stderr | Existing sanitized `database_configuration_invalid` error, then unsuccessful exit |
| Control: invalid cursor configuration rejected before telemetry shutdown | 2 | `configuration_invalid` is delivered | Preserve that existing behavior |

This task repairs diagnostic ordering, not the underlying reason for a startup
failure. The owner has scoped production-readiness work to code and existing local
verification. Backups, restore exercises, staging/deployment acceptance and
operations are outside scope. Existing architecture testing is accepted; no new
architecture or hosted-CI acceptance work is required. Preserve the existing
backend features, API contract, configuration defaults and diagnostic categories.

## Files and responsibilities

- `src/main.rs`: terminal error emission, serving result propagation and cleanup
  ownership across configuration, initialization, bind and server-exit paths.
- `src/telemetry/providers.rs`: provider shutdown and final stdout-drain ordering,
  if a narrow composition change is needed.
- `src/telemetry/logging.rs`: inspect closing/abandonment semantics; retain the
  existing finite, nonblocking sink and approved overflow policy.
- `src/shutdown.rs`: inspect the existing total-deadline integration.
- `tests/foundation_process.rs` and existing process helpers: real executable
  failure-path regressions with bounded capture and child cleanup.
- `tests/{stdout_backpressure,liveness_admission,telemetry_signals,bootstrap}.rs`:
  preserve the existing backpressure, health, privacy and command regressions.
- `docs/{observability,operations}.md`: clarify terminal-diagnostic ordering only
  where the implementation requires it.
- `spec/implementation/{progress,acceptance-map}.md`: actual evidence and handoff.

## Tasks

### 1. Establish controlled failing cases

Add focused process tests for both demonstrated failures before changing the
production ordering. Start the real `ferl-server` executable with deterministic
test configuration and normal logging enabled. Consume stdout and stderr while
the process runs, with finite capture and child-exit bounds, so this test isolates
ordering from consumer backpressure.

For the bind failure, hold an ephemeral loopback listener open until the child
has exited. For the connection-options failure, use a syntactically valid
PostgreSQL URL whose SQLx option is invalid, such as
`postgres://probe@127.0.0.1:1/probe?sslmode=not-a-mode`. Supply valid cursor/origin
settings so the child reaches the intended failure rather than an earlier
configuration check. Neither case needs a live external database or Collector.

Assert the unsuccessful exit status and the expected category in parsed structured
stdout. Include private canaries where appropriate and reject leaked credentials,
connection URLs or raw driver/bind errors. Retain the earlier-configuration-error
control. Missing diagnostics must fail the test even when the child exits with 1.

### 2. Correct terminal-event and cleanup ordering

Arrange for the terminal failure event to be emitted while the existing subscriber
and bounded diagnostic sink still accept it, before provider shutdown and the final
stdout drain. Choose the smallest change consistent with the current composition;
inspect all `ServerError` paths rather than patching only the two test branches.
Preserve the existing distinctions between initialization/configuration rejection,
serving failure and successful signal-driven shutdown.

Keep sanitized categories and private-data boundaries. Do not repair the missing
event through synchronous stdout/stderr fallback, a reopened sink, disabled logging
or an additional unbounded writer. A terminal event should participate in normal
telemetry processing before provider shutdown; this task does not promise OTLP
delivery when its exporter is unavailable or its budget is exhausted.

Preserve cleanup of workers, connections and providers on early failures and
unexpected server exit. Preserve existing exit codes and bootstrap's separately
acknowledged one-time credential result. Ask about genuinely unresolved requirements
or a necessary dependency/public-setting/scope expansion rather than inventing new
behavior. No contract amendment or new configuration surface is needed for the
demonstrated ordering correction.

### 3. Preserve the existing shutdown budget and loss policy

Use the existing absolute deadline for the serving cleanup path. Moving the final
log must not grant a fresh full timeout or append the non-serving one-second flush
budget to an already consumed serving deadline. Check worker joins, sink closure,
provider shutdown and the final command-level flush together.

With a healthy consumer, the terminal event is eligible for the bounded final drain.
With a full/stalled sink or an exhausted deadline, part 12's approved best-effort
loss remains valid. Do not turn diagnostics into a durability guarantee or wait
indefinitely to deliver the final message. Retain part 13's exact liveness-only
admission isolation and readiness behavior.

## Verification gate

1. Run the new failure-path tests against the uncorrected baseline and record their
   missing-diagnostic failures. After the fix, both must deliver the expected safe
   category and exit with 1. Keep the existing exit-2 configuration control passing.
2. Verify delivered diagnostic JSON and privacy, plus successful normal startup and
   signal-driven shutdown. Audit all terminal cleanup paths for emission before
   provider/sink closure and resource release within the existing deadline.
3. Run the existing stdout backpressure and liveness-admission regressions, including
   natural exit with stdout withheld through SIGTERM. Keep the existing Collector
   outage/privacy/lifecycle and bootstrap-output tests passing. Forced child
   termination is cleanup of a failed test, never evidence of bounded shutdown.
4. Run the existing local `sh scripts/check.sh` gate using `docs/development.md`.
   It includes formatting, locked check/Clippy/tests/doctests and local packaged
   regressions; it does not authorize or require deployment operations. Use focused
   tests during development, then clean committed part-branch inputs for packaging.
   Preserve unrelated user files and use a clean worktree if needed. Record missing
   prerequisites or failures rather than silently skipping required checks.

**Handoff:** update progress.md with the ordering decision, changed files, controlled
old/fixed results, exact commands and observed exit/diagnostic evidence. Add concrete
test references to acceptance-map.md. Commit this fix and its tests/docs on
`implementation/14-terminal-error-logging`, then follow git-workflow.md's explicit
no-ff/no-commit merge procedure into local `main`. Mark part 14 Complete only in
that verified integration. Report the verified implementation commit, merge commit
and final Git state; retain the branch and stop after this task.
