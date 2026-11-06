# ferl server: staged implementation instructions

This directory is an execution plan for an AI Rust developer implementing the first ferl backend release and its bounded follow-up fixes. Implement the numbered parts in order. Each part defines a bounded increment, target files, implementation steps, and a verification gate.

The repository contained only the business specification and OpenAPI document when these instructions were written. **These files describe future work; no server implementation or passing test results are implied.**

## Start here

For every implementation session, read:

1. [Shared agent contract](00-agent-contract.md).
2. [Architecture and database coordination](architecture.md).
3. The selected part below and its referenced sections of [server-business-logic.md](../server-business-logic.md).
4. The relevant operations and schemas in [openapi.yaml](../openapi.yaml).
5. [Progress record](progress.md), plus the code and tests produced by earlier parts.
6. [Local Git workflow](git-workflow.md): initialize `main`, branch per part, verify, commit and merge.

The business specification and OpenAPI are normative. This directory supplies implementation choices and work sequencing; it does not override either contract. Resolve an actual contract conflict explicitly before implementing the affected behavior.

## Ordered parts

| Part | Implemented increment | Depends on | Completion evidence |
| --- | --- | --- | --- |
| [01 — Foundation](01-foundation.md) | Buildable Rust server, validated configuration, HTTP envelope, health, test harness | None | Server starts; foundational checks and probes pass |
| [02 — Persistence](02-persistence.md) | Versioned PostgreSQL schema, permanent reservations, constraints and transaction helpers | 01 | Real-database schema, cascade, and migration checks pass |
| [03 — Domain and wire contracts](03-domain-and-wire-contracts.md) | Validation, secret formats, strict decoding, sealed cursor codec | 02 | Boundary and serialization tests pass |
| [04 — Authentication and accounts](04-authentication-and-accounts.md) | Bootstrap, account API, authorization and credential mutation coordination | 03 | Account flows and revocation/admin races pass |
| [05 — Client IP and creation allowance](05-client-ip-and-rate-limits.md) | Shared effective IP resolution and durable rolling rate accounting | 04 | Proxy cases and real-database limiter races pass |
| [06 — Link management](06-link-management.md) | Creation, reads, atomic edits/deletion, titles/tags and anonymous tokens | 05 | Create/manage/delete flows pass with correct charging |
| [07 — Redirects and visit ingestion](07-redirects-and-visit-ingestion.md) | Exact public routing, 302 responses, bounded analytics worker and atomic counters | 06 | Redirect, snapshot, retry, and deletion-race checks pass |
| [08 — Link and tag discovery](08-link-and-tag-discovery.md) | Scoped lists, filters, search, ordering, keyset pagination and tag counts | 07 | All sort/filter/scope combinations and cursor boundaries pass |
| [09 — Analytics reports](09-analytics-reports.md) | Raw visit pages, interval summaries and grouped breakdowns | 08 | Authorized reports match controlled persisted fixtures |
| [10 — Observability and lifecycle](10-observability-and-lifecycle.md) | OTLP traces/metrics/logs, privacy verification, bounded drain and export | 09 | Signal capture, outage, redaction and shutdown checks pass |
| [11 — Release verification](11-release-verification.md) | Independent-process correctness, OpenAPI conformance, packaging and operator docs | 10 | All 51 acceptance scenarios have executable evidence |
| [12 — Stdout backpressure](12-stdout-backpressure.md) | Bounded nonblocking serving diagnostics and bounded writer shutdown | 11 | A blocked stdout consumer cannot stall requests or natural process exit; bootstrap output remains separate |
| [13 — Liveness admission](13-liveness-admission.md) | Owner-approved liveness-only admission isolation and matching contract amendment | 12 | Liveness stays responsive under business saturation; readiness and business admission retain their existing behavior |

The sequence is deliberately linear so a fresh agent can take one file at a time. Keep every completed increment buildable. Part 06 yields a usable link-management API; part 07 adds shortening/redirect behavior; part 09 completes the business endpoints; part 11 establishes first-release readiness. Intermediate increments are development milestones, not claims of a conforming release.

Parts 12–13 are owner-approved runtime follow-ups to the historically completed
parts 01–11. Assign each to a separate agent. Implement, verify, commit and merge
part 12 before starting part 13 from the updated `main`; use each file's own
`implementation/NN-<slug>` branch. The owner accepts existing architecture testing
for these tasks. These assignments do not require new architecture coverage,
hosted workflows, staging/restore exercises or a capacity redesign. Their status
and approved decisions are recorded in [progress.md](progress.md).

## Prompt to give the Rust developer agent

For the full reusable execution prompt, use [developer-prompt.md](developer-prompt.md). The shorter assignment below also incorporates the mandatory Git workflow.

The same file includes [separate part 12 and part 13 assignments](developer-prompt.md#runtime-hardening-assignments).

```text
Implement part NN from spec/implementation/README.md.

Read spec/implementation/00-agent-contract.md, architecture.md, git-workflow.md, progress.md,
the selected part, and its normative business/OpenAPI references first.
Initialize the local repository on main if needed and commit its reviewed baseline.
Start or resume implementation/NN-<part-slug> from the current verified main.
Inspect the repository and existing work. Implement the selected part completely,
including its required checks, while preserving completed behavior.
Do not silently change the API or business rules. Record an actual contract
conflict and resolve it before implementing the affected behavior.

Update spec/implementation/progress.md with changed files, exact checks and results,
implementation decisions, and any blockers. After its checks pass, commit the part
and merge it into local main with an explicit merge commit, following git-workflow.md.
Finish with a concise handoff stating whether the part's completion gate passed,
the branch/implementation/merge commits, final Git status, and which part is next.
```

Replace `NN` with one numbered part. The developer may adjust internal filenames to match existing code, but must document the mapping and preserve the specified responsibilities.

## Traceability and completion

- [Acceptance map](acceptance-map.md) assigns every business acceptance scenario to implementation and final verification work.
- [Progress record](progress.md) is the persistent handoff between agent sessions.
- A part is complete only when its functionality and verification gate are complete and its committed branch is merged into local `main`. A skipped integration check is recorded as blocked, not passed.
- Mark release readiness only after part 11. Shared-database tests using two pools are useful during development, but do not replace the required two-independent-process tests.
