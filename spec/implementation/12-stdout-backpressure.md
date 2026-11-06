# Part 12 — Bounded stdout diagnostics under backpressure

**Dependencies:** parts 01–11 complete. Read the current source, shared agent
contract, architecture, Git workflow, progress record and acceptance map first.

**Normative references:** business sections 3, 14.1–14.3 and acceptance scenarios
29–31, 36 and 50; the existing bootstrap result-stream contract and OpenAPI.

**Part branch:** `implementation/12-stdout-backpressure`.

## Outcome

The serving process continues handling requests and exits within its configured
total shutdown budget when its stdout consumer stops reading. Structured stdout
diagnostics remain enabled, bounded and private. The owner permits dropping
diagnostic records when the finite buffer is full or the shutdown deadline
expires. This permission does not apply to bootstrap credential output, HTTP
responses, or authoritative PostgreSQL state.

## Finding and approved scope

The review at `0d4cdcc499f02edaedef70ffb332012935a8af9e` found that production
subscriber construction passes `std::io::stdout` directly to the formatter.
Request-completion events write through that synchronous sink before returning
the response. SDK/exporter diagnostics also have a direct `println!` path.

A local production-process probe withheld reads from the child's stdout pipe.
After 174 completed health requests, subsequent probes timed out. With
`FERL_SHUTDOWN_TIMEOUT_SECS=3`, `FERL_HTTP_DRAIN_TIMEOUT_SECS=1` and
`FERL_READINESS_WITHDRAWAL_SECS=0`, the process remained alive five seconds after
SIGTERM. Reading stdout again allowed exit. An otherwise equivalent control with
`RUST_LOG=off` completed 800 health requests and exited promptly. Those counts and
timings are review observations, not portable thresholds or a replacement for a
committed regression test. Logging must remain enabled in the fixed-path test.

The owner approved best-effort diagnostic loss on 2026-11-06. Clarify that narrow
policy in business section 14 and the operator documentation as part of this fix;
do not reinterpret the existing bootstrap result as lossy telemetry. Part 13 owns
the separate liveness-admission change. Architecture expansion, hosted CI,
staging/restore exercises and capacity redesign are outside this assignment.

## Files and responsibilities

- `src/telemetry/logging.rs`: production formatting, filtering and output sink.
- `src/telemetry/providers.rs`: sink ownership, sanitized nonrecursive exporter
  diagnostics, startup and shutdown composition.
- `src/main.rs`, `src/shutdown.rs`: lifetime and total-deadline integration where
  necessary; preserve the command-specific output streams.
- `tests/telemetry_signals.rs`, `tests/foundation_process.rs` and existing process
  support: real executable/stdout fault coverage. Add a focused test file or a
  private sink-control seam only if the existing harness needs it.
- `tests/bootstrap.rs`: regression coverage for one-time output and output errors.
- `spec/server-business-logic.md`, `docs/{observability,operations,configuration}.md`:
  the approved diagnostic-loss policy, finite bounds and lifecycle behavior.
- `spec/implementation/{progress,acceptance-map}.md`: actual evidence and handoff.

## Tasks

### 1. Establish the failure and the output boundary

Inspect all serving diagnostic paths, including ordinary JSON events, startup and
shutdown logs, exporter errors and queue-full diagnostics. Establish a focused
regression that fails against the old implementation because a blocked output
consumer stalls requests or process exit. Do not use disabled logging, a different
subscriber, or an unreachable Collector as a substitute for stdout backpressure.

Keep production-process tests isolated, with bounded output capture and cleanup.
Use explicit synchronization or observed blocked writes/backlog to establish the
fault. Do not assume a platform pipe capacity, a fixed number of requests, or a
settling sleep proves saturation. A narrow private test seam may provide the
ordering evidence; it must not become a production flag or HTTP operation.

### 2. Make serving diagnostic writes bounded and nonblocking

Use the existing telemetry architecture and dependencies where practical. Define
and document a finite queue/memory bound and a concrete overflow policy. Producer
work must not wait for sink progress or queue capacity. Bound retained records and
their storage, including any staging buffers; an unbounded channel or one task per
log entry does not satisfy the requirement.

Move potentially blocking sink I/O off HTTP/runtime execution paths. Audit the
selected writer's full-queue, write-error, flush and Drop behavior rather than
assuming an asynchronous-looking API is nonblocking. Ask the owner before adding
a dependency or public configuration surface; this task does not preauthorize new
environment settings, a logging framework replacement or durable log storage.

Route the existing sanitized exporter diagnostics through the bounded output
boundary without recursively emitting them through OTLP. A loss/error diagnostic
must not restore a synchronous write to the stalled sink or recursively log its
own failure. Preserve field allowlists, safe categories, JSON formatting and
request/trace correlation. OTLP export retains its existing independent bounded
behavior and does not become contingent on stdout success.

### 3. Own the writer through bounded shutdown

Include any writer drain/flush in the remaining existing total shutdown budget.
A sink that remains blocked must not prevent natural process exit. Inspect worker
joins, runtime teardown and destructor paths: abandoning an awaited future is
insufficient if a blocking task or Drop still keeps the process alive.

With a healthy consumer, attempt a bounded drain and retain complete structured
records. With a stalled consumer or exhausted budget, discard pending diagnostics
as approved. Use the existing lifecycle ordering and settings; do not extend the
total budget, terminate early to evade unrelated shutdown work, or claim delivery
of records that were dropped.

Audit shared helpers used by `migrate` and `bootstrap-admin`. Preserve their
stream routing, exit/error behavior, and the separately acknowledged one-time
bootstrap JSON result. Never enqueue that credential result into a lossy
diagnostic buffer or mix it with telemetry. Successful bootstrap must still report
output failure when the authoritative result cannot be written.

### 4. Document only the implemented policy

Record the actual bound, overflow choice, sink-failure behavior and shutdown
ownership. Update the narrow stdout provisions in business section 14 consistently
with the owner's approval. Keep the existing HTTP/API contract and configuration
defaults. Preserve historical verification records and add new evidence under
part 12 rather than rewriting the earlier completion claims.

## Verification gate

1. Demonstrate the focused regression failing on the old implementation and passing
   with the fix. Keep a healthy-consumer control with logging enabled.
2. With real serving stdout consumption withheld and bounded saturation established,
   verify continued health and representative successful business/redirect requests.
   Use isolated real PostgreSQL for database-dependent success assertions. Keep
   request concurrency below the existing admission limit so this test isolates
   stdout from the separate part 13 behavior.
3. Keep the consumer blocked through SIGTERM. Measure elapsed time to actual child
   exit against the configured total budget with a documented scheduler tolerance.
   Forced termination is failure cleanup, never passing shutdown evidence.
4. Exercise finite-buffer overflow, sink write failure and recovery of a temporary
   stall. Prove bounded storage/drop behavior with controlled tests, and validate
   complete JSON/correlation for records delivered to a healthy consumer. Do not
   require every record to survive a deliberately lossy condition.
5. Run the existing Collector/exporter outage, all-signal privacy and lifecycle
   tests, plus bootstrap output regressions. Confirm canary secrets remain absent
   from diagnostics and bootstrap output remains one-time and separately routed.
6. Follow `docs/development.md` and run `sh scripts/check.sh` from clean committed
   inputs. This is the existing local fmt, locked check/Clippy/test/doctest and
   packaged regression gate, not a request for additional architecture testing.
   During development run the focused suites first; record exact commands/results.
   Use checkpoint commits on this part branch before clean-source packaging. If
   unrelated user files prevent a clean checkout, preserve them and use a clean
   worktree at the intended commit or ask for resolution. Diagnose a fixture
   failure without weakening its ownership checks or skipping required suites.

**Handoff:** update progress.md with the sink design, finite bounds, loss policy,
old/fixed controls, actual exit measurements, commands/results and acceptance-map
test pointers. Record the base and verified implementation commits. Follow
git-workflow.md to commit this fix on its own branch and merge it into local
`main` using the explicit no-ff/no-commit handoff procedure. Mark part 12 Complete
only inside its verified integration; report the merge hash and final Git state.
Retain the branch, stop, and hand part 13 to a separate agent.
