# Implementation progress and agent handoff

## Terminal error logging follow-up — planned, 2026-11-08

Part 14 is **Planned**; parts 12 and 13 are complete in Git and in the current
part-status table below. The earlier planning and acceptance entries are retained
as historical records.

- Review baseline: `d1abbc6f309ac3e1eae28805aeb419064f677c9b`.
- Task: [14 — Terminal error logging](14-terminal-error-logging.md).
- Implementation branch: `implementation/14-terminal-error-logging`, from the
  current verified `main` containing parts 12 and 13.
- Confirmed finding: `serve()` shuts down telemetry and closes stdout before
  `serve_command()` logs its terminal failure. With a healthy reader, an occupied
  listener and invalid SQLx options each exited with 1 and empty stdout/stderr;
  invalid cursor configuration remained an exit-2 control with a diagnostic.
- Owner scope: production-readiness work covers code and the existing local
  verification gates. Backups, restore exercises and staging/deployment operations
  or acceptance are outside scope. Existing architecture coverage is accepted.
  Historical pending external-acceptance entries are not prerequisites for part 14.
- The fix must preserve bounded diagnostic loss, one existing serving shutdown
  deadline, bootstrap output and liveness-only admission isolation.

### Part 14 specification integration

Status: **Complete** for documentation preparation on
`maintenance/terminal-error-logging-spec`, based on the review baseline above.
The new task, index, separate agent prompt and this planning record are the only
intended changes. Runtime implementation and regression results belong to the
future part 14 agent. A clean documentation worktree preserves the primary
checkout's existing untracked user documents.

Documentation verification passed: `git diff --cached --check`; an inline Python 3
check of the exact four-file documentation scope, 30 local Markdown links/anchors,
balanced code fences, required task sections, the two failure categories and control,
explicit scope, Planned status and separate agent assignment; and review against
the existing numbered-task and Git integration patterns. Verified specification
commit `eb26525f5c34eef808970d85f72abfcf8e8bc917` is integrated into local `main`
by this explicit no-ff merge; only this integration handoff differs from the
verified branch tree. The merge hash is reported after creation, not embedded in
its own tree. Part 14 remains **Planned**; this documentation integration does not
implement or verify the production fix.

## Runtime hardening follow-up — owner-approved plan, 2026-11-06

Parts 12 and 13 are **Planned**. This entry records instructions and owner
decisions; it does not claim either implementation or its regression tests exist.
Historical parts 01–11 and maintenance completion records remain intact.

The review baseline was `0d4cdcc499f02edaedef70ffb332012935a8af9e`. It identified
two bounded runtime changes:

| Part | Task | Branch | Dependency |
|---|---|---|---|
| [12](12-stdout-backpressure.md) | Prevent serving stdout backpressure from stalling requests or process shutdown | `implementation/12-stdout-backpressure` | Completed part 11 |
| [13](13-liveness-admission.md) | Isolate exact liveness routing from business admission, with the approved contract amendment | `implementation/13-liveness-admission` | Part 12 verified and merged into `main` |

Owner decisions:

- Diagnostic logs may be dropped when a finite stdout buffer is full or the
  existing shutdown deadline expires. The one-time bootstrap credential result
  is separate and retains its existing acknowledged output/error behavior.
- Isolate **liveness only**. Readiness retains its existing admission and
  schema/database/accepting-state behavior. Current business section 14.3 and
  OpenAPI explicitly permit liveness overload 503; part 13 must amend those
  passages narrowly and consistently rather than call the old behavior a
  contract violation. Part 12 may clarify the approved stdout-loss policy in
  business section 14.
- Continue the numbered implementation-file pattern with one expert Rust agent
  per task, sequential execution, a separate branch/commit history for each fix,
  and verified explicit local merges into `main` following git-workflow.md.
- Existing architecture testing is accepted for these tasks. Additional platform
  testing and hosted CI are not prerequisites for either part. The review's
  restore/capacity observations do not authorize a new code feature or redesign.
- This preparatory session is documentation-only. Implementation agents must
  inspect their actual starting source and ask about unresolved requirements or
  scope expansion instead of assuming new behavior.

### Specification integration

Status: **Complete** for documentation preparation on
`maintenance/runtime-hardening-specs`, based on
`0d4cdcc499f02edaedef70ffb332012935a8af9e`. The owner authorized committing and
merging these instruction files separately from the future fixes. A clean
documentation worktree preserves the primary checkout's pre-existing untracked
`readme.md`, `docs/cli-api-handoff.md` and `docs/publication-cicd-agent-handoff.md`.
Documentation verification passed:

- `git diff --cached --check`.
- An inline Python 3 check of the exact five-file documentation scope, 28 local
  Markdown links/anchors, balanced code fences, the two task files' required
  sections, their separate agent/branch assignments, and both Planned statuses.
- Review against the existing numbered-part and local no-ff integration patterns,
  the current health contracts and the owner's explicit decisions.

Only the two task briefs, implementation README, developer prompt and progress
record changed. Runtime code, test code and normative contracts are not changed
by this preparation; the future agents own their specified implementation gates.
Verified specification commit `84146063dbf463793888e94b988af1e3ecd46263` is
integrated by this explicit no-ff merge into local `main`. Only this specification
integration handoff differs from the verified branch tree. Both implementation
parts remain **Planned**; neither fix has been implemented or verified by this
documentation session. The merge hash is reported after creation, not embedded
in its own tree. The original untracked documents are preserved separately from
the committed specification changes.

## Release-readiness acceptance — worklist recorded before implementation

Branch `maintenance/release-readiness-acceptance`; verified starting `main`
baseline `487cc04749e1d391ba04a4a4d45d2afaa0b83aab`, whose merge parents are
`960f7a422f593c42383d2b8fe719ffd8aaee9ae4` and
`ea961a30bceca87b75c5ca969656dce5791565ea`.
Starting inspection also found a clean index/tree, no pending Git operation or
remote, one worktree, configured identity `LubosBa <30705301+LubosBa@users.noreply.github.com>`, both
retained maintenance branches, and the required Part 07 compatibility commit
reachable from `main`. The pinned
Stage 01 PostgreSQL container and volume were running and label-verified; their
identities are preserved for use and neither is task-owned. Initial container ID:
`272ce0848e0d7f0e8da96139e80b31dfec0b31d785bffd690007f1e2296bd1ea`; pinned
image digest `sha256:ef257d85f76e48da1c64832459b59fcaba1a4dac97bf5d7450c77753542eee94`;
volume `ferl-stage01-postgres-data` at Podman mountpoint
`/var/home/core/.local/share/containers/storage/volumes/ferl-stage01-postgres-data/_data`.

| Location | Demonstrated defect or hypothesis; plausible faulty implementation | Focused control | Acceptance condition | External prerequisite |
|---|---|---|---|---|
| `tests/proxy_routing.rs`, `tests/support/podman_server.rs`, `scripts/proxy.sh`, proxy fixture | Retained `7309ea5` output has backend peer, NGINX IP, persisted IP, and NGINX-observed IP all `10.89.0.4`. Correct forwarding and backend peer fallback are indistinguishable; host-published traffic can make NGINX observe itself as the client. | Send a run-marked request from a separate run-owned client container over the isolated network through one retained NGINX instance; inspect client/NGINX/backend IDs and IPs; compare persisted client, backend peer, and attacker spoof values. Independently send a spoofed request directly from an untrusted network peer. | Three IP values are distinguishable; proxied visit stores the real client IP; direct visit stores its peer despite spoofed headers; existing proxy path/method/header/cursor assertions remain. | None for local fixtures. |
| `tests/support/openapi.rs`, `tests/openapi_release.rs` | Resolver checks the supplied root dialect, resolves `$ref`, then adapts every expanded schema by replacing `$schema`. A referenced target can introduce unsupported or ill-typed `$schema` and be silently accepted by adaptation. | Call the public resolver with supplied documents covering a bad direct target, invalid dialect type, second-hop bad target, restrictive supported sibling, true/false targets, and example/constant data containing schema-like keys. | Every public schema-resolution path rejects unsupported effective referenced dialects before adaptation; supported references/siblings/booleans/data semantics and current normative schemas pass. | None. |
| `.github/workflows/ci.yml`, advertised targets and release evidence | Local aarch64/macOS evidence does not establish hosted CI or native Linux/amd64 runtime support; workflow and package scripts advertise both `aarch64-unknown-linux-gnu` and `x86_64-unknown-linux-gnu`. | Only after authorization, execute the actual hosted workflow with complete history and native runners for each advertised target; retain run URL/revision/job evidence. | Native CI and packaged execution pass for every approved supported release architecture; do not infer native compatibility from QEMU. | Approved remote and push authorization; hosted workflow execution authorization; explicit supported architecture decision; authorized evidence retention. |
| Staging, backup and isolated restore | No approved staging target, private credential path, backup target, or separately recoverable cursor-key mechanism is specified in this local checkout. Local fixtures cannot establish deployment or restore acceptance. | After authorization, run the synthetic-data deployment and separate-target backup/restore exercise in the approved environment; retain private evidence only by its approved method. | Every deployment and restore step in the release acceptance request passes using the exact verified artifact; source database is never overwritten. | Approved staging/environment operations, credentials/evidence method, backup/restore target, and cursor-key recovery mechanism. |

Outcome status at worklist creation:

- Local verification corrections: **Planned**.
- Native CI acceptance: **Pending authorization** (not a pass).
- Staging and restore acceptance: **Pending authorization** (not a pass).

Current outcome status after verification:

- Local verification corrections: **Complete** on local `main`. Verified
  implementation commit `25f077f6fde044d41a665e100309e421bcdb7cb9` and
  handoff-only commit `08f782dcecee793b2bc4d6bd4ddd79a70c3e1202` are integrated
  by the explicit no-ff merge with parents
  `487cc04749e1d391ba04a4a4d45d2afaa0b83aab` and
  `08f782dcecee793b2bc4d6bd4ddd79a70c3e1202`. The merge hash is reported after
  creation, not embedded in its own tree. The maintenance branch is retained.
- Native CI acceptance: **Pending authorization** (not a pass).
- Staging and restore acceptance: **Pending authorization** (not a pass).

Implementation-input commit sequence on this branch:

- `54461e7157377ca4f0037c89a6e5a3d9f5e0f875` — focused implementation and
  regression controls.
- `4f4936a83107251b385806b2afaadd8cce3e2958` — warning-denied Clippy correction
  for the proxy trust-list assertion.
- `25f077f6fde044d41a665e100309e421bcdb7cb9` — order-independent proxy mount
  checks, asserted reload success, and final verified implementation inputs.

No normative contract, migration, dependency version, or release platform
declaration changed.

Development and failed-run evidence (all raw logs/metadata are preserved and are
not included in the consolidated test counts):

- The new referenced-dialect test was first run against the uncorrected resolver
  at starting revision `487cc04749e1d391ba04a4a4d45d2afaa0b83aab`. It failed as
  intended: unsupported string dialect, invalid-type dialect, and second-hop
  dialect all returned `Ok` schemas with `$schema` rewritten to Draft 2020-12.
  Raw log: `target/release-readiness/36effbe1b5e845dab684df60c98bf51e/before-fix-2ca84701cb424fdebdbbaa9f89e4c4a3/`.
- After the resolver correction, the same filtered test passed (1 passed); the
  whole-document example-data control passed separately (1 passed). Logs:
  `.../openapi-regression-df15ecff42ac42de869fe5c08e36da8e/` and
  `.../openapi-document-regression-adb55688d57e4a30873404056df59e77/` under
  `target/release-readiness/36effbe1b5e845dab684df60c98bf51e/`.
- The focused proxy binary was executed 8 times during development: 4 passed and
  4 failed while the harness was being corrected. The failures exposed a config
  path assertion error, an immediate readiness probe, and finally the concrete
  Podman mount-order check that rejected an otherwise valid NGINX reload. The
  reload command's failure had also been ignored by the test. The final focused
  pass with the corrected mount verification and an asserted successful reload
  is `target/release-readiness/36effbe1b5e845dab684df60c98bf51e/proxy-reload-mounted-15s-665b90ee817c4511a78b6d625605f0e0/`.
- The first committed full-gate attempt at `54461e7` failed warning-denied
  Clippy on an unnecessary test-only clone; the correction is a separate commit
  `4f4936a83107251b385806b2afaadd8cce3e2958`. The raw failed gate is
  `target/release-readiness/36effbe1b5e845dab684df60c98bf51e/gate-check-full-1c9ce03118b947bc9e5bbefc8b72929a/`.
- A later proxy-gate attempt at `4f4936a` exposed the ignored reload failure and
  mount-order issue. The focused raw failure is
  `target/release-readiness/36effbe1b5e845dab684df60c98bf51e/proxy-reload-assert-run-5c461e32a8874e7793bcaacc28fed79d/`; the consolidated failed gate
  is `target/release-readiness/36effbe1b5e845dab684df60c98bf51e/gate-final-check-proxy-8a8cdbe21ab94b0b837d242c2c15c782/`.
  The order-independent mount check and reload assertion are in final input
  commit `25f077f`. Both failed consolidated-run directories remain intact.

Final consolidated gates on `25f077f6fde044d41a665e100309e421bcdb7cb9` all
passed. The evidence root is
`target/release-readiness/36effbe1b5e845dab684df60c98bf51e/`; each invocation
directory contains `invocation.json` plus raw stdout/stderr logs.

| Exact gate | Result and separated counts | Evidence directory |
|---|---|---|
| `sh scripts/check-openapi.sh` | 4 passed; 0 failed/ignored. | `gate-accepted-check-openapi-5048d12ae75d4989b128dddfd61f496e/` |
| `sh scripts/check.sh` | 227 all-target cases: 97 library, 0 binary, 130 integration cases across 52 integration binaries; separate `cargo test --workspace --doc`: 3 doctests. 0 failed/ignored. | `gate-accepted-check-full-e8c3514747f14d2798d274492a708d48/` |
| `sh scripts/check-proxy.sh` | 1 packaged production TCP test passed. The test recorded client `10.89.0.6`, NGINX/backend peer `10.89.0.4`, spoof `198.51.100.240`; forwarded and direct spoofed visits persisted `10.89.0.6`; the post-reload NGINX peer fallback persisted `10.89.0.4`. | `gate-accepted-check-proxy-9c80b6d657a14f869198395df40803e8/` |
| `sh scripts/check-release.sh` | 8 packaged Rust tests and 8 Python release-verifier controls passed. | `gate-accepted-check-release-264f73b0e75c4acc9567e26f8aee4ca0/` |
| `sh scripts/check-tls.sh` | 2 packaged TLS/lifecycle tests passed; the Collector decoded 10 traces, 14 metrics, and 11 logs from the intended process. | `gate-accepted-check-tls-e08c20d1eda1423685fc1eb0eef1c25d/` |

The consolidated Rust suite used Rust/Cargo/Rustdoc 1.99.1 on
`aarch64-apple-darwin`; Podman 6.1.4's Linux VM reported `arm64`; the database
fixture was PostgreSQL 17.6 and the proxy fixture NGINX 1.30.6. `check.sh` reused
the existing Cargo build/registry caches; package/builder output directories
were unique for the runs. No task-owned proxy/client/server/collector containers
or networks remained after the gates. The only labeled container left was the
preserved `ferl-stage01-postgres`, with the same initial ID
`272ce0848e0d7f0e8da96139e80b31dfec0b31d785bffd690007f1e2296bd1ea`; its
`ferl-stage01-postgres-data` volume and mountpoint were unchanged. The detached
clean-checkout worktree was removed after its artifact and evidence were retained
under `target/release-readiness/`.

MSRV verification ran in the same detached clean-checkout worktree at the exact
implementation commit with initially absent MSRV target directories:
`FERL_MSRV_TOOLCHAIN_DIR=$PWD/target/toolchains/rust-1.94.0 sh scripts/check-msrv.sh`.
Actual `rustc`, Cargo, and Rustdoc were all 1.94.0 on `aarch64-apple-darwin`; the
locked all-target/all-feature check and production release build passed. Its
raw evidence is `gate-accepted-check-msrv-215ef2bb5fb74668afafd49e20852d35/`.

Clean-checkout release verification ran `sh scripts/check-release.sh` at
`25f077f6fde044d41a665e100309e421bcdb7cb9` in detached worktree
`$TMPDIR`.
The checkout was not shallow (58 commits) and retained compatibility commit
`9bcf7c0c021c994e76d0696633fe822f0a49a5e1`; separate release-test and builder
target directories were initially absent. The existing Cargo registry cache was
reused (warm, not cold). The verified 17-file package was run as UID 65532 in
the same digest-pinned Debian Bookworm `linux/arm64` image used by the build.

- Artifact:
  `target/release-readiness/36effbe1b5e845dab684df60c98bf51e/clean-checkout-final/packages/ferl-server-0.1.0-aarch64-unknown-linux-gnu.tar.gz`
- Archive SHA-256: `f968bd5babd6098b202ae505a45ecf3f59dcfedcf9ffccd42acd234384e773d7`
- Binary SHA-256: `76fb42bffd1e224c616b87ca19b22ec52201e9fe101d4a7954a247bab3c32919`
- Source revision/tree: `25f077f6fde044d41a665e100309e421bcdb7cb9` /
  `248e28e1a542bbd18d4eb7ede72e061666c4555d`; target
  `aarch64-unknown-linux-gnu`; schema v2; `source_tree_clean=true`.
- Builder: `docker.io/library/rust@sha256:32f7944ed00949c44d3944a0a852b3e5ea862a610a38b43ac92a33aa87256b75`
  (Rust/Cargo 1.99.1, Debian Bookworm, linux/arm64).

The current release scripts advertise both `aarch64-unknown-linux-gnu` and
`x86_64-unknown-linux-gnu`. The local packaged runtime result is Linux/arm64
only; native Linux/amd64 and hosted workflow acceptance are not claimed. There
is no configured remote, approved push/hosted-workflow authorization, approved
platform decision, staging target/operation authorization, private credential
and evidence-retention method, backup/restore target, or cursor-key recovery
mechanism. Native CI and staging/restore therefore remain **Pending
authorization**; this does not negate the verified local correction gate.

The post-release task does not mutate production behavior, schemas, migrations,
dependencies, or normative business/OpenAPI contracts.

## Verification-integrity closure — Complete

### Verified implementation and results — 2026-10-29

Status: **Complete**. Branch
`maintenance/verification-integrity-closure`; base `960f7a422f593c42383d2b8fe719ffd8aaee9ae4`;
verified implementation commit `7309ea5ead2279b6ea01470c8382abed6013a177`;
handoff-only follow-up commit `ea961a30bceca87b75c5ca969656dce5791565ea`.
Integrated into local `main` by an explicit no-ff merge with parents
`960f7a422f593c42383d2b8fe719ffd8aaee9ae4` and
`ea961a30bceca87b75c5ca969656dce5791565ea`. The merge commit hash is reported
after creation, not embedded here. The maintenance branch is retained.
No normative contract, migration, production feature, or dependency version
changed; this milestone closes evidence-integrity gaps for existing behavior.

Confirmed findings and fixes:

- **Historical counts:** Listing-only inventory at retained hardening commit
  `9755fb61a42fa8d328ca5723a0cec1927afe0541` is 97 library + 0 binary + 128
  integration cases in 52 integration binaries = 225 all-target cases, plus 3
  doctests. This confirms 225 + 3 = 228 distinct cases. The original hardening
  raw gate log is absent; the retained evidence directory contains artifacts
  and repackages only. Original pass/fail/ignored output therefore remains
  unverified; the previous Complete status is preserved.
- **OpenAPI:** Normative and malformed documents share one complete OAS
  3.1/JSON Schema validation path. Resolvers use supplied documents; inline,
  component, and webhook schema roots are discovered structurally; nested
  map-keyword dialects are checked before adaptation; boolean schemas and
  conjunctive `$ref` siblings are preserved. `$dynamicRef` is rejected because
  dynamic-scope/anchor semantics are not implemented. Route ownership now uses
  valid authorization and operation-specific outcomes, plus exact Allow sets,
  received headers, and body bytes.
- **Proxy:** The observed NGINX instance and peer IP are retained through
  upstream replacement. Its config is directory-mounted so reload observes the
  new backend. The TCP test checks raw/encoded/trailing/multi-segment paths,
  GET/HEAD, zero redirect bytes, exact query+fragment Location, common headers,
  unsupported-method behavior, maximum legal analytics headers, and all issued
  large-cursor pages through explicit null termination. Host-gateway loopback
  remains distinct from the safely trusted bridge peer and direct caller.
- **TLS/lifecycle:** Positive/negative CA files are parsed and readable; both
  hostname alias pairs resolve to their run-owned servers. PostgreSQL rejection
  is bracketed by successful migrations on the same fixture. OTLP rejection is
  bracketed by positive same-Collector recovery; received records are decoded,
  checked for meaningful content, and attributed to the actual process by
  `service.instance.id`. Process output and TLS-exported payloads are scanned
  for privacy canaries. Absence no longer relies on arbitrary sleep. Owned
  cleanup is synchronous on cancellation; a controlled cancellation test proves
  owned containers/networks are removed while Stage 01 container/volume
  identities survive. Shutdown timing ends at process exit before later
  inspection/log collection/removal.
- **Packaging:** Archive validation precedes extraction and checks canonical
  unique paths, links/special entries, exact directories/files, checksums,
  committed migration inventory, source blobs/tree, target ELF, and builder
  receipt. A Git-archive source snapshot is checked path/mode/byte-for-byte and
  made read-only before the locked build. Build receipt binds binary hash,
  target, source tree, invocation, command, and observed builder. Archive and
  sidecar writes and staging extraction are atomic. Packaged restart smoke now
  checks the revoked credential after restart along with code reservation and
  absence of reseeding.
- **CI/MSRV:** Workflow passes the toolchain prefix rather than a path already
  ending in `/bin`; the script pins/checks Rust, Rustdoc, and Cargo 1.94.0.
  Rust 1.99.1 remains the normal toolchain.

Post-commit verification on implementation `7309ea5ead2279b6ea01470c8382abed6013a177`:

| Command | Observed result |
|---|---|
| `sh scripts/check-openapi.sh` | Passed 3 tests, 0 failures/ignored; whole document, focused mutation controls, authorized route ownership, exact Allow sets, received headers, and bodyless behavior. |
| `sh scripts/check.sh` | Passed **226 all-target cases** (97 library, 0 binary, 129 integration cases across 52 binaries) plus **3 doctests**, 0 failures/ignored. Includes fmt, locked check, warning-denied Clippy, PostgreSQL, process, Collector, proxy, release smoke, and TLS. Log: `target/verification-integrity/7309ea5/full-gate.log`. |
| `sh scripts/check-proxy.sh` | Passed 1 packaged production TCP proxy test. Observed host-gateway peer `127.0.0.1`, NGINX/backend peer `10.89.0.4`, direct untrusted peer `10.89.0.3`. Log: `target/verification-integrity/7309ea5/check-proxy.log`. |
| `sh scripts/check-release.sh` | Passed 7 packaged Rust tests (3 OpenAPI, proxy, release smoke, 2 TLS) and 8 Python release-verifier controls; 0 failures/ignored. Log: `target/verification-integrity/7309ea5/check-release.log`. |
| `sh scripts/check-tls.sh` | Passed 2 packaged TLS/lifecycle tests. PostgreSQL `verify-full` migration/bootstrap/recovery and TLS rejection controls passed; Collector decoded 10 traces, 14 metrics, 11 logs from the intended process. Log: `target/verification-integrity/7309ea5/check-tls.log`. |
| MSRV/CI invocation shape | `TOOLCHAIN_BIN="$PWD/target/toolchains/rust-1.94.0/bin"; TOOLCHAIN_PREFIX="$(dirname "$TOOLCHAIN_BIN")"; FERL_MSRV_TOOLCHAIN_DIR="$TOOLCHAIN_PREFIX" sh scripts/check-msrv.sh` passed locked all-target/all-feature check and production release build using `rustc/rustdoc 1.94.0 (4a4ef493e 2026-03-02)` and `cargo 1.94.0 (85eff7c80 2026-01-15)` on `aarch64-apple-darwin`. Local installed prefix used because rustup is unavailable. Log: `target/verification-integrity/7309ea5/check-msrv-ci-shape.log`. |

The consolidated count above contains one all-target invocation and one separate
doctest invocation. Focused OpenAPI/TLS/cleanup tests and historical `--list`
inventory controls were run separately and are not added to the 226 + 3.

Clean-checkout release proof ran `sh scripts/check-release.sh` in detached
worktree `$TMPDIR`
at the exact implementation commit, with complete history and legacy commit
`9bcf7c0c021c994e76d0696633fe822f0a49a5e1` reachable. Cargo test and builder
target directories were initially fresh; the registry cache was reused (warm,
not cold). The package verified 17 files, exact OpenAPI and both migrations,
schema v2, `source_tree_clean=true`, and ran as UID 65532 in the same pinned
Debian Bookworm arm64 image used for its build. Packaged migrate/bootstrap/
create/list/GET/HEAD/reports/rotate/delete/restart-without-reseeding passed.

- Artifact: `target/verification-integrity/7309ea5/clean-checkout/packages/ferl-server-0.1.0-aarch64-unknown-linux-gnu.tar.gz`
- Archive SHA-256: `16ca864fe57e62400edd141837cc1cbd0cd569f1cc5edf110a5d36606598b9a5`
- Binary SHA-256: `76fb42bffd1e224c616b87ca19b22ec52201e9fe101d4a7954a247bab3c32919`
- OpenAPI SHA-256: `45677d0d0c0afda4687fada791ce492682ad546f2fbc612113890cffecbb6b5e`
- Source tree: `2f5d020db52daf1404d4c1014ec779e48a7a50ca`; build invocation:
  `0c232a81f0ee42b2870cebb8a0784562`; build-receipt SHA-256:
  `096eac7f93d6cabeb37ea2c2fefe0044c4bb35bfd36c5347fddbbf3edb80b65b`; builder image:
  `docker.io/library/rust@sha256:32f7944ed00949c44d3944a0a852b3e5ea862a610a38b43ac92a33aa87256b75`.

Two repackages of identical binary/source/build-receipt inputs produced the
same archive checksum as the clean-checkout package. Separate fresh-target
builds on this same host and pinned builder also produced the same binary hash;
the warm registry was shared, so this is not a cold-cache or cross-machine
reproducibility claim. Hosted CI was not run. The available host/Podman VM is
macOS/aarch64 and Linux/arm64; native Linux/amd64 runtime verification is not
claimed. No blocker remains; awaiting local Git integration.

### Correction to the preceding verification-integrity evidence — 2026-10-29

The preceding Complete status and original evidence are preserved, but two
assertions described there exceeded what their controls established:

- The proxy log `target/verification-integrity/7309ea5/check-proxy.log` records
  the backend peer, NGINX address, persisted address, and NGINX-observed address
  all as `10.89.0.4`. Because the observed client and NGINX peer were identical,
  that run did **not** distinguish selecting the forwarded client from ignoring
  forwarding and recording the backend peer. Its spoof comparison alone did not
  prove a different real client address was forwarded.
- The OpenAPI negative dialect controls covered supplied roots and inline
  schema positions, but did not call a public resolver with a referenced target
  that introduced `$schema`. The public helper checked the supplied schema,
  resolved references, then adapted the expanded schema; adaptation could
  overwrite an unchecked referenced dialect. Whole-document validation did not
  protect those public functions when called independently.

These are verification-integrity gaps in those two assertions, not unfinished
business features or a change to the historical milestone record. The current
release-readiness acceptance work adds direct public-resolver controls and a
run-owned network client with distinct observed identities; its final committed
gate results are recorded in the release-readiness section above.

Branch `maintenance/verification-integrity-closure`; base `main` commit
`960f7a422f593c42383d2b8fe719ffd8aaee9ae4`. Initial inspection found a clean
index/tree, no pending Git operation or remote, configured identity
`LubosBa <30705301+LubosBa@users.noreply.github.com>`, no `AGENTS.md`, the expected hardening merge
parents, and the retained legacy compatibility commit. The pre-existing
`ferl-stage01-postgres` container and `ferl-stage01-postgres-data` volume are
label-verified and running; they must remain untouched except for authorized
fixture use that preserves the volume.

### Evidence-based worklist (before implementation)

| Location | Demonstrated defect or hypothesis; plausible false green | Focused control | Acceptance condition |
|---|---|---|---|
| Historical gate records / `progress.md`, `acceptance-map.md`, `docs/release-verification.md` | The reported 228 all-target count may include the 3-doctest result that is then listed again. The retained hardening artifact directory has packages/repackages but no raw gate log, so original per-binary arithmetic cannot currently be independently reconstructed. | Search retained evidence; preserve the limitation. Capture fresh `cargo test --workspace --all-targets` and `cargo test --workspace --doc` output separately and count each binary/result without repeated filtered runs. | New counts are disjoint and exact; historical correction is explicit and does not invent missing original output. |
| `tests/support/openapi.rs`, `tests/openapi_release.rs` | Schema-root helpers can overwrite/check dialects inconsistently, `$dynamicRef` pointer existence is mistaken for semantics, map-valued schema keywords may hide nested dialects, and malformed documents do not all use one document validator. | Mutate supplied documents at operation responses, nested `properties`/`$defs`, boolean schemas, `$ref` siblings, and `$dynamicRef`; call the same complete validation entry point. | Normative document passes; each plausible malformed variant fails before adaptation, and unsupported dynamic-reference semantics are accurately rejected. |
| `tests/openapi_release.rs` route probes | Unauthenticated 401 or 403 can establish that some protected route claimed a path without proving the intended handler owns it. | Use existing authorized database fixtures and operation-specific responses/effects for route ownership; retain exact response/header/body assertions. | All reviewed operations have distinct executable evidence; `Allow`, received headers, and body bytes match actual behavior. |
| `tests/proxy_routing.rs`, `scripts/proxy.sh` | The peer-observation NGINX is removed before a different instance forwards trusted assertions; the test does not bind later identity to the observed peer. Several claimed GET/HEAD, redirect-byte, exact Location, method-error, and final cursor-page assertions are incomplete. | Keep one observed NGINX instance for forwarding, verify its network identity across upstream reconfiguration, and traverse all actual proxy pages including termination. | Trusted address belongs to the proxy used for assertions; paths, GET/HEAD, headers, unsupported method, maximum analytics headers, and every cursor bucket are checked over TCP. |
| `tests/release_tls.rs`, `tests/support/podman_server.rs` | TLS negatives can pass for non-TLS causes; the CA/preflight does not prove parsing/use; telemetry identity/privacy and absence-of-export use incomplete evidence; cleanup fallback is detached and shutdown timing includes later collection/removal. | Add same-fixture positive recovery, explicit CA/hostname checks, run-scoped decoded signal/privacy checks, bounded export-settling evidence, synchronous owned-resource cleanup regression, and signal-to-exit timing. | Positive and negative cases isolate TLS validation; telemetry belongs to this process/run; cleanup is complete before test exit and leaves unrelated fixtures intact. |
| `scripts/prepare-release-package.sh`, `scripts/verify-release.py`, `scripts/package_release.py`, `scripts/build-linux.sh` | `tar` extraction precedes verification; normalized duplicate paths and special members need adversarial controls; caller metadata can outlive/bypass its build invocation; provenance migration inventory must be tied to the committed source. | Pre-validate every archive member/checksum before writing files; reject path aliases/links/special/unexpected entries; bind an observed build receipt and committed source tree; test plausible substitution/corruption/interruption cases. | No unverified archive is extracted or accepted; source, target, binary, migrations, and observed builder evidence agree; partial output cannot verify as complete. |
| `.github/workflows/ci.yml`, `scripts/check-msrv.sh`, docs | CI passes a `.../bin` path to an interface that appends `/bin`, so the hosted MSRV command shape likely resolves `bin/bin`. | Execute the documented prefix form and the exact CI invocation shape locally with actual Rust/Cargo 1.94.0; compare target/build prerequisites. | Both intended interfaces select Rust/Cargo 1.94.0 and complete the locked check/release build; no hosted CI success is claimed. |

Handoff summary: Parts 01–11 are complete and merged into local `main`. Part 11 — Release verification was verified on `implementation/11-release-verification` from baseline `71d77b3f3838081add13d46cdefe0a91cdd70efd` and integrated by the explicit no-ff merge recorded below.

## Post-release hardening milestone — 2026-10-29

Status: **Complete** on `maintenance/post-release-hardening`, based on the
verified, clean Part 11 merge `6038cbe6703ed81e387b7d9fa5998e686bedcd21`.
That commit has the expected parents `71d77b3f3838081add13d46cdefe0a91cdd70efd`
and `4226cd63298a7599e5866ae545845a590e32616f`; the retained Part 11 branch is
at `4226cd63298a7599e5866ae545845a590e32616f`, with implementation
`95d053d70c47c7199cfbd45545fa8c08b8d945d1`. The required legacy compatibility
commit `9bcf7c0c021c994e76d0696633fe822f0a49a5e1` is reachable from this base.
The repository root is `$PWD`; no remote is
configured, only this worktree exists, the index/tree were clean, no Git
operation was pending, and configured identity is
`LubosBa <30705301+LubosBa@users.noreply.github.com>`. No `AGENTS.md` was found. The existing
`ferl-stage01-postgres` container and `ferl-stage01-postgres-data` volume are
pre-existing and must be preserved.

The milestone is bounded to independent verification hardening, demonstrated
test/package/CI/documentation corrections, an actual Rust 1.94 build, packaged
PostgreSQL TLS and OTLP HTTPS exercises, clean-checkout packaging/smoke evidence,
and local Git integration. It does not reopen Part 11 or add business behavior.

### Evidence-based worklist

| Finding/location | Plausible false green or failure | Planned verification | Acceptance condition |
| --- | --- | --- | --- |
| `tests/openapi_release.rs` and `tests/support/openapi.rs`: response-map helper is only applied to a negative control; runtime inventory accepts any declared status, including 404 for an unregistered protected route; the 405 test synthesizes `Cache-Control`; schema helpers overwrite `$schema`. | Missing operation responses, misrouted routes, absent received headers, or an unsupported dialect can pass the documented gate. | Add negative controls that exercise the same validation path, assert the normative document's response maps, require exact unauthenticated route ownership outcomes, validate received headers, and reject unsupported effective dialects. Run `sh scripts/check-openapi.sh` plus focused controls. | Each plausible defect fails its control; all actual operation/route/header and schema checks accept the normative document and fail malformed variants. |
| `tests/proxy_routing.rs`: the host-gateway probe, untrusted-peer probe, and subsequent trusted-proxy container reused one published host port; after separating ports, the first static-host probes still targeted the replacement port before it was started. | Replacement startup could race with another local listener, or an endpoint probe could connect to no listener and fail before reaching its intended fixture. | Assign distinct independently reserved ports and route each probe phase through its corresponding run-specific NGINX container; run the clean-source proxy gate. | Each probe reaches the intended container and the replacement topology does not rely on immediate address reuse. |
| `tests/release_tls.rs`: PostgreSQL readiness polling used `pg_isready` over the container's Unix socket before the packaged TLS client connected over TCP. | Local database readiness could pass before the TCP listener accepts the verified TLS connection, creating an intermittent migration failure under the full-suite load. | Poll PostgreSQL's loopback TCP listener before the packaged migration and include only the CLI's sanitized failure category in diagnostics; rerun standalone and consolidated TLS gates. | A passing readiness precondition covers the same TCP transport used by the packaged verifier; TLS remains verified by the real `verify-full` migration. |
| `scripts/build-linux.sh`, `scripts/package_release.py`, `scripts/verify-release.py`, and `tests/release_smoke.rs`. | A native Ubuntu build may not run on the documented Bookworm runtime; a multiarch image tag can select the wrong local architecture; metadata can describe source inputs without proving the clean revision used; archive inventory/extraction checks may miss discrepancies. | Pin target-specific Bookworm builder manifests, assert compiler/OS/target in the builder, bind metadata to the clean checkout, and test archive members/checksums/OpenAPI/migrations in a complete clean checkout with a fresh Cargo target. | The clean checkout's exact committed source, target-specific builder/runtime, package members and smoke behavior agree; make no independent bit-for-bit build claim unless measured. |
| `Cargo.toml` declares Rust 1.94; prior evidence used only 1.99.1. | A locked dependency or source feature requires a newer compiler despite metadata. | Obtain and execute actual isolated Rust/Cargo 1.94.0; run locked workspace all-target/all-feature check and locked production release build. | Both requested commands pass on the recorded target, or a specific blocker is documented without claiming MSRV compatibility. |
| `src/db/pool.rs`, migration/bootstrap connection options, `src/telemetry/providers.rs`, and release smoke currently have no end-to-end TLS evidence. | PostgreSQL server verification may skip chain/hostname validation, or OTLP HTTPS export may fail despite exporter configuration. | Use run-isolated short-lived CA/server fixtures and the packaged binary; test valid chain+hostname, untrusted CA, hostname mismatch, actual decoded OTLP signals, and exporter failure/readiness/shutdown controls. | Valid TLS handshakes and actual telemetry receipt are observed; both certificate controls reject; request/readiness stay independent and shutdown remains within its total deadline. |
| `.github/workflows/ci.yml` and release build scripts. | CI builds a native Ubuntu executable but exercises it in Debian Bookworm, while local macOS uses the pinned Bookworm builder; hosted success is not available locally. | Compare build/runtime prerequisites; pin or align the Linux builder/runtime where justified, run local equivalents, and state hosted CI is unverified. | The local Linux artifact is built with the same pinned runtime-generation baseline used by smoke; no hosted result is claimed. |

### Hardening handoff — verified implementation

Verified implementation revision: `9755fb61a42fa8d328ca5723a0cec1927afe0541`
(`fix: gate TLS migration on PostgreSQL TCP readiness`), descended from base
`6038cbe6703ed81e387b7d9fa5998e686bedcd21`. Handoff-only follow-up:
`8453dc6cea92ae4f6e842e5fe17e9bd18e233142`.

Integration: explicit no-ff merge into local `main`, with parents
`6038cbe6703ed81e387b7d9fa5998e686bedcd21` and
`8453dc6cea92ae4f6e842e5fe17e9bd18e233142`. The staged implementation matched
the retained branch tip; only completion handoff metadata in this file differs
in the merge commit. The resulting merge hash is reported after creation, not
embedded here. The `maintenance/post-release-hardening` branch is retained.

Final local gates on the committed implementation:

- `sh scripts/check-openapi.sh` — 3 tests passed; the normative OpenAPI document
  and actual production method/path/response/header probes passed.
- `sh scripts/check-proxy.sh` — 1 packaged TCP proxy test passed. The host
  gateway, probe proxy, and trusted replacement use distinct reserved ports.
- `sh scripts/check-release.sh` — 6 packaged Rust tests and 3 Python verifier
  regression controls passed; no failures or ignored tests.
- `sh scripts/check-tls.sh` — 1 packaged TLS test passed. The Collector decoded
  10 traces, 14 metrics, and 11 log records; invalid CA and hostname controls
  rejected export while API/readiness stayed healthy and shutdown stayed under
  the configured 20-second total bound.
- `sh scripts/check.sh` — the handoff's **historical count correction** is an
  inventory of 225 all-target test cases plus 3 doctests (228 distinct cases),
  not 228 all-target cases plus 3 doctests. Listing-only controls at retained
  implementation `9755fb61a42fa8d328ca5723a0cec1927afe0541` ran
  `cargo test --locked --workspace --all-targets -- --list` (97 library, 0
  binary, 128 integration cases across 52 integration binaries) and
  `cargo test --locked --workspace --doc -- --list` (3 doctests): 225 + 3.
  These enumerate test cases and do not execute tests. The original hardening
  raw gate log was not retained: `target/hardening-evidence/9755fb6-clean-checkout/`
  contains packages/repackages but no gate output, so original pass/fail and
  ignored status cannot be independently re-established. The historical
  handoff recorded no failures/ignored tests. Its gate included formatting, locked check,
  warning-denied Clippy, PostgreSQL, independent-process, Collector, proxy,
  packaged release, and TLS suites.
- `FERL_MSRV_TOOLCHAIN_DIR=target/toolchains/rust-1.94.0 sh scripts/check-msrv.sh`
  — actual Rust/Cargo 1.94.0 passed the locked workspace check
  (`--all-targets --all-features`) and locked production release build on
  `aarch64-apple-darwin`. The standard pinned project toolchain remains 1.99.1.

Clean-checkout release verification passed in a disposable detached worktree at
this exact implementation revision, with complete Git history, the required
legacy commit reachable, and fresh Cargo target directories. The existing
Cargo registry cache was reused (not a cold dependency-cache build). The
packaged `aarch64-unknown-linux-gnu` executable ran as UID 65532 in the same
digest-pinned Debian Bookworm arm64 image used for its build. The archive has 17
verified files, exact `spec/openapi.yaml` bytes, both migrations, matching
provenance and checksums, and `source_tree_clean=true`.

Artifact from the clean-checkout run:

- Path: `target/hardening-evidence/9755fb6-clean-checkout/packages/ferl-server-0.1.0-aarch64-unknown-linux-gnu.tar.gz`
- Archive SHA-256: `8f170dcb3d4f7a0cb88dc86554694ec9280c26755ea2622843d66e3ed1ea5915`
- Binary SHA-256: `76fb42bffd1e224c616b87ca19b22ec52201e9fe101d4a7954a247bab3c32919`
- OpenAPI SHA-256: `45677d0d0c0afda4687fada791ce492682ad546f2fbc612113890cffecbb6b5e`

Independent builds from this commit used separate fresh Cargo target directories
and the same pinned arm64 Bookworm builder; their binary and archive checksums
matched. Repackaging identical inputs was byte-identical. The Cargo registry
cache was shared, so this is not a cold-cache or cross-machine reproducibility
claim. Hosted CI was not run. The pinned amd64 image was pulled, but local
execution under the arm64 Podman QEMU emulator segfaulted; no x86_64 runtime
result is claimed. Other runtime distributions remain unverified. No remote,
push, published tag, deployment, migration, or business-contract change was made.

Historical Part 11 evidence below remains a separate record and is not new
evidence for this milestone.

Status lifecycle: Planned → In progress → Verified (awaiting merge) → Complete. Use Blocked when a required implementation, verification or Git integration step cannot finish. Complete requires a committed merge into local `main`, as defined in [git-workflow.md](git-workflow.md).

## Part status

| Part | Status | Evidence / blocker |
| --- | --- | --- |
| 01 Foundation | Complete | Full Part 01 gate and real PostgreSQL readiness/outage/recovery passed; verified branch `implementation/01-foundation` was integrated by merge commit `5bd6127d36560e51ded4f46de68efc10ea78bf5a`. |
| 02 Persistence | Complete | Verified implementation `1a6b7bbbebc35c815a37dca478287004683cb1a1` and handoff branch tip `31c9a1e184052eca3e55ffb16160aee0c14b0f06` from `implementation/02-persistence` are integrated in the explicit local no-ff merge into `main`. |
| 03 Domain and wire contracts | Complete | Verified implementation `9f143478c657a505082911d015c8961f14df28dd` and handoff-only branch tip `5d5da6142397fecfe4ae7c7a67187da00f3a2e5f` from `implementation/03-domain-and-wire-contracts` are integrated by this explicit no-ff merge into `main`. |
| 04 Authentication and accounts | Complete | Verified implementation `b740bb2d80c1bac590df52b72005efcc6bebb758` and integrated part tip `1aacfbca49b94c000cd7ab17b20a3609b4c740dd` from `implementation/04-authentication-and-accounts` are integrated by this explicit no-ff merge into `main` from base `15d9ddebdc0b15d06a0bea97883a6beaf1b1437a`. The tested implementation tree is unchanged; only this completion handoff is added in the merge commit. |
| 05 Client IP and rate limits | Complete | Verified implementation `cda4b77866e96b498be144d6f819cd4a113a6896` and integrated part tip `6c21d8ab8a4d05f7c41166da98d8fb3d42ffd7d3` from `implementation/05-client-ip-and-rate-limits`, base `14ec982566c348f6b59e1f7a715f7eefc2ab9b19`, are integrated by this explicit local no-ff merge. Closing gate: 144 tests and 3 compile-fail doctests passed. |
| 06 Link management | Complete | Verified implementation `264f3f30af32d4e1b8460504f7d0abed62590d6e` and integrated part tip `968b4eaaafae7f21723763c7fc76d2e4890ffa70` from `implementation/06-link-management`, based on `bbe53b3b439d961b88e0a56b877ad3e409411f2e`, are integrated by this explicit no-ff merge. |
| 07 Redirects and visit ingestion | Complete | Verified implementation `e7d0bb8d39fa22748642a9f9488e21cb505a8598` and integrated part tip `5f9fe68d8d8c171808fb4e3db9c1dddb6306d1c7` are integrated into local `main` by the explicit no-ff Part 07 merge recorded below. |
| 08 Link and tag discovery | Complete | Verified implementation `1526f60527ac7fd9206fbbdef2b51a11b87d5af8`, integrated branch tip `fd9f4d35bda58b82a81834891d95f318fa3cc913`, based on Part 07 merge `9bcf7c0c021c994e76d0696633fe822f0a49a5e1`, is integrated by the explicit no-ff merge recorded in the Part 08 handoff below. The closing gate passed; only this completion handoff differs in the merge commit. |
| 09 Analytics reports | Complete | Verified implementation `178e297` and handoff-only part tip `f08b805` from `implementation/09-analytics-reports`, based on Part 08 merge `12c10c234c492dcbdf554a3cc6e1321d83474361`, are integrated by the explicit no-ff merge recorded below. |
| 10 Observability and lifecycle | Complete | Verified implementation `1d84f0a` and handoff-only branch tip `3f21bb4` are integrated from `implementation/10-observability-and-lifecycle` by the explicit no-ff merge from base `6153b768`; see the Part 10 handoff below. |
| 11 Release verification | Complete | Verified implementation `95d053d70c47c7199cfbd45545fa8c08b8d945d1` and handoff-only branch tip `4226cd63298a7599e5866ae545845a590e32616f` from `implementation/11-release-verification`, based on `71d77b3f3838081add13d46cdefe0a91cdd70efd`, are integrated by the explicit local no-ff merge recorded below. The full local gate, OpenAPI, clean-source proxy/package gates, and all 51 scenario evidence mappings are recorded in the Part 11 handoff. |
| 12 Stdout backpressure | Complete | Verified implementation `77f11a86c78fe06706460689e4772c2ac6a3fc9b` and evidence tip `01ebaf8c7cfb3914d72f57727650ba433a778ef4` from `implementation/12-stdout-backpressure`, base `ea1691b7bb5cc375956bcc6a7cdd3b17dd435f98`, integrated by the explicit no-ff merge into `main`; clean-source `sh scripts/check.sh` passed. See the Part 12 handoff below. |
| 13 Liveness admission | Complete | Verified implementation `f52af28` and evidence tip `352b844` from `implementation/13-liveness-admission`, base `91295dd` (part 12 merge), integrated into `main` by the explicit no-ff merge recorded in the Part 13 handoff. Clean-source `sh scripts/check.sh` (244 passed, 0 failed, 0 ignored) and `sh scripts/check-openapi.sh` passed. See the Part 13 handoff below. |
| 14 Terminal error logging | Planned | Emit terminal serving errors before provider/stdout closure; separate branch `implementation/14-terminal-error-logging`, controlled failure-path regressions and existing local verification required. See the part 14 spec. |

## Part 12 — Bounded stdout diagnostics under backpressure

Status: **Complete** (integrated into local `main` by the explicit no-ff merge; the merge commit hash is reported after creation, not embedded here). Branch `implementation/12-stdout-backpressure`;
base `main` commit `ea1691b7bb5cc375956bcc6a7cdd3b17dd435f98`; verified
implementation commit `77f11a86c78fe06706460689e4772c2ac6a3fc9b`. The implementation
commit contains source, tests, and the owner-approved policy/operator docs; this
evidence commit adds only progress and acceptance-map records. Merge into `main` is
pending the explicit no-ff/no-commit handoff.

### Failure and sink design

- Root cause: the production subscriber wrote each event synchronously to
  `std::io::stdout`, and exporter diagnostics used `println!`. A stalled reader
  blocked the request that emitted an event.
- Serving stdout now uses `DiagnosticWriter`: each tracing event is staged in a
  per-event buffer and enqueued as one complete record on a fixed
  `sync_channel` of **256 records**. Each record is capped at **8 KiB**
  (about 2 MiB retained). One detached `ferl-stdout-diagnostics` OS thread owns
  stdout and is the only thread that can block on it. No new dependency, setting,
  or async-looking blocking queue is used.
- Overflow, oversized records and write errors discard the record and increment a
  counter. When the consumer resumes, one sanitized record
  (`fields.category = "diagnostic_records_dropped"`, `fields.dropped_records = N`)
  precedes the next complete record. A failed summary write restores its count.
- Exporter diagnostics (`otel_export_failed`, `otel_export_queue_full`) use the
  same queue through `emit_diagnostic_line`, bypassing tracing, so they cannot
  recurse into OTLP.
- Shutdown: `Telemetry::shutdown` drains last with the same absolute total
  deadline. At the deadline the writer is abandoned and pending records are
  discarded; the drain never extends `FERL_SHUTDOWN_TIMEOUT_SECS`. `main` also
  calls `finish_stdout` with a fixed 1-second budget for `migrate` and config
  rejection; serving calls it again and it returns immediately.
- Bootstrap is unchanged: its one-time result remains a direct stdout write with
  its separate stderr diagnostics (`init_stderr`), and its failure exit behavior
  is unchanged.
- Audit notes: the writer thread is detached (no join/Drop block); the runtime
  never performs a stdout write; a partial line from an OS-reported failed write
  remains and is not retried. Stdout is a pipe the process does not own, so
  `EPIPE` is counted as loss and does not stop later attempts.

### Files changed

- `src/telemetry/logging.rs`: sink, writer, drain, bounds, unit tests (5).
- `src/telemetry/providers.rs`: writer composition, nonrecursive exporter line,
  drain at end of `Telemetry::shutdown`.
- `src/main.rs`: bounded drain for non-serving commands.
- `tests/stdout_backpressure.rs` (new, 3 process tests);
  `tests/support/serving_process.rs`: a withheld-stdout gate (`start_with_withheld_stdout`,
  `release_stdout`); `tests/rate_limit_process.rs`: worker drains before exit
  (its parent asserts a log record).
- `spec/server-business-logic.md` (sections 14.1, 14.3 narrow approved policy),
  `docs/observability.md` (new "Stdout diagnostic bounds"), `docs/operations.md`,
  `docs/configuration.md`.
- No migration, dependency, HTTP contract or configuration default changed.

### Controlled failing and passing evidence

Old implementation (base commit, before the fix):

- `target/stdout-backpressure/old-baseline.log`:
  `cargo test --locked --test stdout_backpressure -- --test-threads=1`: 1 passed
  (healthy control), 2 failed. Both withheld-consumer tests stalled at
  `liveness request 173 ... HTTP test deadline exceeded`.
- Out-of-tree production-process driver `target/stdout-backpressure/measure_exit.py`
  (ad-hoc; stdout never read; SIGTERM; 3 s/1 s/0 s budgets). Runs 1 and 2
  (`old-unread-stdout*.json`): 174 health requests completed, then timeouts;
  process still alive 15 s after SIGTERM and killed as failure cleanup. This
  matches the review's 174-request observation.
- `RUST_LOG=off` control, 800 requests (`old-logs-off-control.json`): exit 0 after
  0.002 s.

Fixed implementation (commit `77f11a8`):

- `target/stdout-backpressure/fixed-run1.log`: 3 passed, 0 failed.
- Same driver (`fixed-unread-stdout.json`, 600 requests, stdout never read):
  exit status 0 after SIGTERM in **3.005 s, 3.006 s, 3.004 s** against the 3 s total
  budget; the drain consumed the budget and the writer was abandoned.
- `RUST_LOG=off` control (`fixed-logs-off-control.json`): 800 requests, exit 0 in
  0.014 s.
- Process test assertion tolerance: exit ≤ budget + 1.5 s (scheduler/spawn
  allowance). Observed exits were within the budget.
- Unit tests (`cargo test --locked --lib telemetry::logging`, 5 passed): exact
  overflow accounting with a blocked writer (producers finish under a 5 s bound),
  oversized discard with later delivery, failed-write report then recovery,
  abandon at drain deadline, exporter line queueing.

### Commands and results (clean-source gate)

Harness: `ferl-stage01-postgres` was Exited (its volume intact). `sh scripts/postgres.sh start`
failed because the harness port check cannot read a stopped container's published
port; `podman start ferl-stage01-postgres` started the same owned container (same
image digest, mount and `127.0.0.1:55432`), then `sh scripts/postgres.sh ready`
passed. The script was not changed; this harness restart limitation is recorded for
separate attention.

Focused checks in the primary checkout before the commit: `cargo fmt --all -- --check`
clean; `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings`
clean; focused `cargo test --locked --lib --test bootstrap --test foundation_process
--test rate_limit_process --test link_failures --test analytics_failures
--test report_failures --test rate_limit_failures --test stdout_backpressure`
passed (102 lib; 5 bootstrap; 2 foundation_process; 2 link_failures; 1 analytics;
1 report_failures; 1 rate_limit_failures; 1 rate_limit_process; 3 stdout_backpressure).

Packaged gate: the primary checkout cannot run `package_release.py`, whose
clean-source check fails on the user's pre-existing untracked files (`readme.md`,
`docs/cli-api-handoff.md`, `docs/publication-cicd-agent-handoff.md`, preserved
unchanged). Therefore a detached clean worktree was created at the exact
implementation commit (`/private/var/folders/.../T/opencode/ferl-part12-77f11a8`) and
ran `sh scripts/check.sh` there, logged at
`target/stdout-backpressure/gate-check-sh-77f11a8.log`, **exit 0**:

- Totals across all result blocks: **238 passed, 0 failed, 0 ignored** (includes 102
  library tests, the process/fixture suites, and 3 doctests).
- Includes `telemetry_signals` (1 passed, Collector decoded signals),
  `release_tls` (2), `release_smoke` (1), `proxy_routing` (1), `rate_limit_process`
  (1), `legacy_compatibility` (1), `multi_instance_process` (3), and `stdout_backpressure` (3).
- Packaged artifact: `ferl-server-0.1.0-aarch64-unknown-linux-gnu.tar.gz`, archive
  sha256 `00b247384b97d2bc6bb236ed382782bc7d8ab6ca03c47a213514fc74eb033565`,
  binary sha256 `9495b88dd903f8507ec3a06b525d172d91d948535a88bd88a2d497819f7778ef`,
  OpenAPI sha256 `45677d0d0c0afda4687fada791ce492682ad546f2fbc612113890cffecbb6b5e`,
  `source_tree_clean=true`, built in the pinned Bookworm builder
  `docker.io/library/rust@sha256:32f7944ed00949c44d3944a0a852b3e5ea862a610a38b43ac92a33aa87256b75`.

Not run: hosted CI, native Linux/amd64 runtime, and the MSRV script (not part of
this part's gate; `check.sh` does not include it). Only Linux/arm64 packaged execution
through the existing gate is claimed.

### Acceptance scenarios

- Scenario 31 (collector outage/backpressure, bounded shutdown) gains stdout
  evidence: request continuity and exit-budget under a withheld consumer
  (`tests/stdout_backpressure.rs::withheld_stdout_consumer_keeps_requests_business_success_and_shutdown_bounded`).
- Scenario 30 privacy and the all-signal test continue to pass unchanged.
- Scenario 29, 36, 50 are unaffected; see `acceptance-map.md`.

### Residual items and next stage

- Diagnostic loss is the owner-approved policy above; a crash can lose records already
  queued. A stalled consumer costs the process its total shutdown budget (bounded).
- Harness: restart of a stopped `ferl-stage01-postgres` through `scripts/postgres.sh start`
  fails its port check; needs a separate harness fix. Not changed here.
- Part 13 (liveness admission) is next, starting from `main` after this merge.

## Part 13 — Liveness isolated from business admission

Status: **Complete** upon the no-ff merge into local `main` that contains this
record (the merge hash is reported after creation, not embedded here). Earlier
text below describes the verified state.

Status (at verification): **Verified (awaiting merge)**. Branch `implementation/13-liveness-admission`;
base `main` commit `91295ddddf79235b607f79444f68b9f6c805f90a` (the part 12 merge).
Prerequisite confirmed before
work: part 12 verified implementation `77f11a86c78fe06706460689e4772c2ac6a3fc9b`
and evidence tip `01ebaf8c7cfb3914d72f57727650ba433a778ef4` are ancestors of
`main` at that merge. Verified
implementation commit: `f52af280284d22fa2d1ba37eee54d05632d39156`
(`fix(part-13): isolate liveness from business admission`). Integration merge
commit hash is reported after creation, not embedded here.

### Approved contract amendment (owner, 2026-11-06, liveness only)

- Business section 14.3 no longer permits a business-admission 503 on
  `GET /api/health/live`. It states that liveness is isolated from bounded
  business HTTP admission and keeps its normal 200 or method/query rejection.
  Readiness keeps the overload allowance.
- OpenAPI `getLiveness`: description now says liveness is isolated from bounded
  HTTP admission; its response map is `200`, `400`, `500` (the `503` entry was
  removed). `500` is retained as the common unexpected-failure response that
  readiness also lists; `405` remains covered by the common method rule, as for
  every other operation. `getReadiness`, shared components and operation IDs are
  unchanged.
- Operator docs (`docs/operations.md`, `docs/configuration.md`,
  `docs/observability.md`) describe the isolation. The historical part 01 and
  part 02 decisions remain as history.

### Implemented behavior and files

- `src/http/router.rs`: `/api/health/live` is merged outside the business
  admission layer. Readiness, `/api`, `/api/{*path}`, public short-code and the
  fallback remain behind `admit_requests` with the unchanged body limit. The
  request-ID/no-store envelope still wraps every route, including liveness.
  Liveness keeps its normal handler (method, query and envelope rules).
- No `src/app.rs`, `src/shutdown.rs`, `src/db/`, configuration, dependency,
  migration or listener change was needed. Business capacities, excess-work
  rejection and force-cancel behavior are unchanged.
- Changed files: `src/http/router.rs` (composition plus unit tests),
  `tests/liveness_admission.rs` (new), `tests/openapi_release.rs` (response-map
  test), `spec/openapi.yaml`, `spec/server-business-logic.md`,
  `docs/{operations,configuration,observability}.md`, and this record plus
  `acceptance-map.md`.

### Controlled evidence (old versus fixed)

Old composition: a detached worktree at `91295dd` was given the new tests with
its original router composition. Commands:
`cargo test --locked --lib http::router` and
`cargo test --locked --test liveness_admission -- --test-threads=1`.

- Unit: 3 failed (`liveness_stays_prompt_while_saturated_business_capacity_sheds_work`,
  `liveness_does_not_join_admitted_business_waiters`,
  `exact_liveness_rules_hold_and_aliases_stay_behind_business_admission`) with
  liveness `503` instead of `200` or `400`; the pre-existing admission bounds
  test passed.
- Integration: 0 passed, 2 failed. Both failed at the liveness assertion with
  `left: 503, right: 200` while the held request owned the only permit. The
  held request was confirmed waiting in `pg_locks` first. The worktree was then
  removed.

Fixed (`f52af28`, primary checkout), focused commands with
`FERL_TEST_DATABASE_URL=postgres://ferl:ferl@127.0.0.1:55432/ferl_test`,
`FERL_TEST_POSTGRES_CONTAINER=ferl-stage01-postgres`,
`FERL_TEST_POSTGRES_PORT=55432`:

- `cargo fmt --all -- --check` clean after `cargo fmt --all`.
- `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` clean.
- `cargo test --locked --lib http::router`: 4 passed.
- `cargo test --locked --test liveness_admission -- --test-threads=1`: 2 passed.
- `cargo test --locked --test openapi_release health_response`: 1 passed.
- `cargo test --locked --test foundation_http --test foundation_postgres`
  (run together): 2 and 1 passed.
- `cargo test --locked --test routing --test rate_limits --test foundation_process --test foundation_http --test stdout_backpressure --test liveness_admission -- --test-threads=1`:
  routing 2, rate_limits 5, foundation_process 2, foundation_http 2,
  stdout_backpressure 3 (part 12 regressions retained), liveness_admission 2 — all passed.
- `sh scripts/check-openapi.sh`: 4 passed (before the response-map test was
  added); after it, the clean-worktree run passed 5 (see below).

### Closing gate (clean committed input)

Detached worktree
`/private/var/folders/rs/gbl8dx2d0072fmd6fdyr2qs40000gp/T/opencode/ferl-part13-f52af28`
at `f52af28` (the primary checkout's untracked user docs were excluded):

- `sh scripts/check-openapi.sh` → exit 0; `openapi_release` 5 passed, 0 failed.
  Log: `target/part13-evidence/check-openapi-f52af28.log`.
- `sh scripts/check.sh` → exit 0. 244 total passed, 0 failed, 0 ignored: 105
  library, 0 binary, 136 integration cases across all integration binaries, 3
  doctests. Includes fmt, `cargo check`, warning-denied clippy, PostgreSQL,
  process, Collector, proxy, release smoke/TLS and packaging. Log:
  `target/part13-evidence/check-f52af28.log`.
- Clean-source packaging: `source_tree_clean=true`, verified archive
  `ferl-server-0.1.0-aarch64-unknown-linux-gnu.tar.gz`, binary SHA-256
  `5f0ea659cfdd4e0b81b2c8f077a1a37aba4cd813fe00e0b1e41056ae2b51d1a3`,
  OpenAPI SHA-256 `566e467b0e8fbc51e4492b276c8b44d2a03cfa1a0d854833b9d3380961a78860`.
  The OpenAPI digest intentionally differs from part 12 (`45677d0d…`) because the
  normative contract changed. Pinned builder:
  `docker.io/library/rust@sha256:32f7944ed00949c44d3944a0a852b3e5ea862a610a38b43ac92a33aa87256b75`.

Copies of both logs are in `target/part13-evidence/` of the primary checkout.
Only Linux/arm64 packaged execution was run; no hosted CI or x86_64 runtime is
claimed.

### Acceptance scenarios

- 31: liveness observable while stdout/PostgreSQL states vary; process test
  `tests/liveness_admission.rs::tcp_process_keeps_liveness_separate_from_held_database_work_and_exits_within_budget`
  adds saturated-admission and withdrawal observations with bounded exit.
- 36: unchanged drain/replacement evidence; the withdrawal leg is covered above.
- 38: exact health isolation and aliases are covered by
  `src/http/router.rs::tests::exact_liveness_rules_hold_and_aliases_stay_behind_business_admission`.
- 51: `tests/openapi_release.rs::health_response_maps_isolate_liveness_from_admission_overload`.

### Residual items

- Untracked user documentation was not edited. `docs/cli-api-handoff.md`
  (around its health table and the paragraph beginning "Admission overload can
  instead return a structured `service_unavailable`") and the untracked
  `readme.md` describe pre-amendment overload behavior for health routes. The
  owner should decide whether those files change.
- The Atlassian MCP containers visible in Podman are unrelated and were not
  touched. The Stage 01 PostgreSQL container and volume were used read/write
  only through the harness.
- Part 13 is the last planned runtime-hardening part; no next implementation
  stage is defined by these briefs.

## Recorded decisions

The baseline implementation choices are in [architecture.md](architecture.md). Record selected versions, operational bounds, deviations and their rationale here during implementation.

- Owner-provided environment note: Podman is set up and may be used to start PostgreSQL and OpenTelemetry Collector containers when needed. Local dependency instructions should use Podman; runtime availability and actual container/test results are verified during implementation.
- Owner-required Git workflow: initialize the local repository on `main` if needed; implement each numbered part on a dedicated branch; after verification, commit it and merge into local `main`. Include the handoff in the committed integration.
- Part 01 selected Axum 0.8.9, SQLx 0.9.0, Tokio 1.54.1, and the OpenTelemetry 0.33.1 family (OTLP, SDK, API, log appender) with `tracing-opentelemetry` 0.34.0. `Cargo.lock` resolves one OpenTelemetry API/SDK/OTLP version; the all-signal feature set compiled. Rust 1.99.1 is pinned; package MSRV is 1.94.
- The serving pool is lazy so liveness can start during a database outage. Its size, in-flight HTTP concurrency, acquire/connect cap, statement/lock limits, analytics bounds, OTLP settings, and shutdown limits are validated and documented. SQLx 0.9 provides one pool acquisition deadline, so the shorter configured connect/acquire timeout is applied to acquisitions.
- HTTP admission now bounds both in-flight work and admitted waiters (`FERL_HTTP_MAX_IN_FLIGHT` and `FERL_HTTP_MAX_WAITING`). Excess work is rejected immediately with structured 503 `service_unavailable`; this overload code is documented in both normative contracts because their original 503 lists did not define request-capacity exhaustion. The in-flight and waiting caps are independently validated.
- Public-origin validation checks the unnormalized input syntax before applying URL-parser normalization, so dot segments, encoded dot segments, backslashes, whitespace/control characters, user-info, paths, queries, and fragments cannot be normalized into an accepted origin. A single optional trailing slash remains accepted and normalized.
- Framework error responses are distinguished from already-conforming application responses by an internal response extension. This lets the outer envelope sanitize JSON and plaintext framework rejections uniformly without rewriting health 503 bodies, and keeps error body/header request IDs consistent.
- Historical Part 01 readiness decision: readiness failed closed until schema compatibility was verified, and `SELECT 1` was not considered schema proof. Part 02 subsequently installed the real migration verifier documented in its handoff below.
- Historical Part 01 note: its PostgreSQL readiness test manually marked the test state schema-compatible to isolate the live database probe/restart recovery; it did not prove a schema version. Part 02 later added the real verifier and migration tests, which are the current schema evidence.
- The Podman harness uses `docker.io/library/postgres:17.6-alpine3.22@sha256:ef257d85f76e48da1c64832459b59fcaba1a4dac97bf5d7450c77753542eee94`, loopback port `55432` by default, configurable via `FERL_POSTGRES_PORT`, label `io.ferl.harness=stage01`, and persistent volume `ferl-stage01-postgres-data`. The harness verifies the image digest, volume, loopback port, and ownership before reuse; creation uses `--pull=never` after explicit pull.
- Historical registry blocker: a local Podman image pull failed TLS certificate verification. Pulls use normal TLS verification; application PostgreSQL and HTTP TLS verification settings were not changed, and no trust-store modification is part of this project.
- Historical Part 01 command decision: `migrate` and `bootstrap-admin` were then intentionally unavailable. Part 02 implemented the explicit migration command; bootstrap was assigned to Part 04 and had not been implemented at the Part 01 checkpoint. `serve` validates the database URL, public origin, cursor key, runtime bounds, and telemetry settings.
- Telemetry endpoint, headers, timeouts, resources, and sampling settings are parsed/redacted now. Part 01 emits safe structured stdout and typed operation/outcome events; actual OTLP providers/export and collector-outage verification remain Part 10.
- Part 10 keeps the resolved OpenTelemetry 0.33.1 API/SDK/OTLP family and tracing-opentelemetry 0.34.0. It uses async Tokio SDK batch processors (with the pinned SDK's explicit experimental features) and a manual-reader metric export task because the OTLP reqwest exporters are async; an empty-resource builder prevents SDK detectors. The unrestricted tracing appender is deliberately not used: a field-allowlisted LogRecord bridge and separate fixed-category SDK diagnostic layer preserve the privacy boundary.
- Part 10 configuration precedence is standard OTEL over FERL compatibility values; signal-specific OTEL exporter variables and arbitrary resource detectors/attributes are rejected. `FERL_INSTANCE_ID` now fails validation because the resource instance ID must be generated per process run. No business prose/OpenAPI conflict was found; no normative business or OpenAPI passage was changed.

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
- Exact image command `podman pull docker.io/library/postgres:17.6-alpine3.22@sha256:ef257d85f76e48da1c64832459b59fcaba1a4dac97bf5d7450c77753542eee94` — passed; image ID `sha256:29e645e20571728f22df398b9954bb6c1cefec390d99ed4e26308fc2f52d6a37`. The script path `sh scripts/postgres.sh pull` also passed with the same digest.
- `sh scripts/postgres.sh start` and `sh scripts/postgres.sh ready` — passed. `ready` observed `podman exec ferl-stage01-postgres pg_isready -q -t 1 -U ferl -d ferl_test` accepting connections. Container inspection confirmed image digest, `io.ferl.harness=stage01`, `127.0.0.1:55432`, and persistent volume `ferl-stage01-postgres-data`; the restart test retained that volume.
- `sh -n scripts/postgres.sh && sh -n scripts/check.sh && git diff --check` — passed after final implementation changes; `git diff --cached --check` and `git diff --cached main --check` also passed during staged review.

Acceptance evidence: Part 01 subsets of scenarios 27, 31, 37, and 38 are mapped to concrete tests in `acceptance-map.md`. Other business-operation database outages, reverse-proxy/static-host behavior, collector outage/export, and scenario 51 OpenAPI release validation remain later-stage checks. The acceptance map does not claim those broader scenarios complete.

No Part 01 implementation or verification blocker remains. At the Part 01 checkpoint, migrations and the real schema verifier were still required before deployment readiness; Part 02 now supplies that persistence gate. Administrator bootstrap remains Part 04 and OTLP providers remain Part 10. The merge commit hash is intentionally not embedded in its own tree; obtain it from local Git history after the merge commit is created.

Historical next-part note: Part 02 — Persistence followed Part 01 and is now complete/merged as recorded below. Part 03 then followed and is also complete; Part 04 is next.

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

At the Part 02 handoff, remaining responsibilities were Parts 03–11. Part 03 has since completed; Parts 04–11 remain future work. Schema v1 is still the only supported ferl migration state in this binary. No known Part 02 blocker remains.

## Part 03 handoff — 2026-10-28

Status: **Complete**. Verified implementation commit: `9f143478c657a505082911d015c8961f14df28dd` (`feat(part-03): add domain and wire contracts`). Part branch: `implementation/03-domain-and-wire-contracts`. Base: verified `main` `2ca9988f85818afd4f9cecb21df6be53d0a978f6`. Handoff-only branch tip integrated: `5d5da6142397fecfe4ae7c7a67187da00f3a2e5f` (`docs(part-03): record verified implementation commit`). Integration merge commit: `48ed2a003bf3401e5a2f359db2641baa0620e09e`, with parents `2ca9988f85818afd4f9cecb21df6be53d0a978f6` and `5d5da6142397fecfe4ae7c7a67187da00f3a2e5f`. The merge tree contained the tested implementation unchanged; its only additional content was the Part 03 completion metadata. This post-merge handoff-only update records the actual merge hash after its creation.

Sources reviewed before implementation: implementation README, agent contract, architecture, Git workflow, progress and acceptance map, complete Part 03 brief; normative business sections 3–4, 5.1, 6.2–6.7, 7, 9.1–9.3, 10, 11.2–11.5, 12, 13–13.1 and 14.2–14.3; relevant OpenAPI paths, parameters, schemas, nullability, defaults, errors, headers and security; source, dependencies, schema, harness, tests and database/configuration/development docs. No normative contract conflict was found or normative contract edited.

### Delivered behavior and decisions

- Added validated types in `src/domain/{ids,count,url,short_code,text,time,secret}.rs`. Username/custom-code syntax is exact; generated codes use OS randomness with 252/256 rejection sampling and a 128-draw per-character ceiling. URL validation checks raw and serialized byte limits, explicit scheme/authority/port, empty user-info, parser-repair cases, and `Location` safety. Public-origin validation now shares this pre-normalization logic.
- Narrow prerequisite correction: the former public-origin validator relied on `url::Url` for port parsing, and the selected URL release repairs `https://internal:` to a host without a port. Configuration now delegates to `domain::url::PublicOrigin::parse`, which checks raw authority/port syntax first and rejects both empty and out-of-range supplied ports. `src/domain/url.rs::tests::validates_authority_before_url_parser_repair` demonstrates the parser behavior; configuration and URL boundary tests cover rejection. No broader Part 01/02 rewrite was needed.
- Full case folding, NFC, Unicode White_Space, and General_Category Control use pinned ICU4X 2.4.0 compiled Unicode 18.0.0 data. The actual `icu_casemap_data`, `icu_normalizer_data`, and `icu_properties_data` crate sources identify Unicode 18.0.0. Persisted comparison keys must use this same version across instances; future Unicode data changes require an explicit compatibility/data-migration plan.
- Titles/tags/search are separate validated types; tags enforce 256 raw scalars and 20 inputs before deduplication, canonicalize by full fold/NFC, and sort bytewise. Persisted comparison transform does not apply request query trimming/limits.
- Credentials are exactly `ferl_ak_v1_` / `ferl_mt_v1_` followed by 32 OS-random bytes encoded as canonical 43-character unpadded base64url. SHA-256 covers the complete presented secret including prefix/version. The fixed 32-byte digest, parsed redacted credential/class, and explicitly one-time serializable output are distinct types. No startup or migration code issues credentials or creates accounts; bootstrap had not yet been implemented at the Part 03 checkpoint and is the current Part 04 work.
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

Unexecuted required checks/blockers at the Part 03 checkpoint: **none**. PostgreSQL regressions were not skipped. At that handoff, authentication/accounts (Part 04), client-IP/rate services (Part 05), link services (Part 06), redirects/analytics (Part 07), discovery/report SQL (Parts 08–09), and OTLP/release verification (Parts 10–11) remained unimplemented. Part 04 began after this historical handoff; its present status is recorded below.

## Part 04 handoff — 2026-10-28

Status: **Complete**. Verified implementation commit `b740bb2d80c1bac590df52b72005efcc6bebb758` and handoff-only part-tip commit `1aacfbca49b94c000cd7ab17b20a3609b4c740dd` from `implementation/04-authentication-and-accounts` are integrated in this explicit no-ff merge into local `main`. Base: `15d9ddebdc0b15d06a0bea97883a6beaf1b1437a`. The merge commit records completion; its hash is intentionally reported after creation, not embedded in its own tree.

Part branch: `implementation/04-authentication-and-accounts`.

Base commit: `15d9ddebdc0b15d06a0bea97883a6beaf1b1437a` on verified `main`, including the Part 03 handoff-only follow-up. The branch was created from this exact commit with a clean index/tree. Verified implementation: `b740bb2d80c1bac590df52b72005efcc6bebb758`; integrated part tip: `1aacfbca49b94c000cd7ab17b20a3609b4c740dd`. The completed integration is merge `14ec982566c348f6b59e1f7a715f7eefc2ab9b19`, with parents `15d9ddebdc0b15d06a0bea97883a6beaf1b1437a` and `1aacfbca49b94c000cd7ab17b20a3609b4c740dd`.

### Delivered interfaces and behavior

- `auth::authenticate(&PgPool, &HeaderMap, deadline)` parses a single bearer field and performs a fresh primary-database digest lookup. Public `Principal::{Account, ManagementToken}` values have private identity/role/digest fields and no caller-accessible UUID/secret constructor. Disabled, deleted, revoked, unmatched, malformed, duplicated, combined, unsupported, padded, or noncanonical credentials remain 401; missing is `authentication_required`; a valid wrong-scope account/token is 403; database unavailability is 503; corrupt stored values are sanitized 500s.
- `begin_account_mutation` (shared gate) and `begin_account_write` (exclusive gate) own a READ COMMITTED transaction and fresh post-wait caller role/state. Their `AccountMutationGuard` retains the lock through explicit `commit`/`rollback`; cancellation/drop rolls back. `begin_anonymous_link_mutation` locks and revalidates the live token-bearing row through transaction completion. `authorize_link_access` checks only live ownership/token authority and returns concealed 404 for inaccessible objects; explicit owner-state validation fails closed on missing/corrupt owners. No public link business routes were registered.
- `accounts::repository` functions receive the supplied connection/transaction. `accounts::service::{current_account,get_account,list_accounts,create_account,update_account,delete_account,rotate_current_key,reset_account_key}` implement the HTTP behavior and revalidate every write after the exclusive account gate. Last-enabled-admin checks cover the proposed combined state. No-op PATCH preserves `updated_at`; actual state/key changes update it. Deletion cascades links/tags/visits and retains code reservations.
- `AppState::from_config` now retains the deployment-configured `CursorCodec`; no production fallback key exists. Account listing uses a typed `AccountListQuery`, configured key, actor-bound `accounts` cursor, exact `(created_at DESC, id DESC)` keyset predicate, `limit + 1`, and the last returned item as boundary. Page size changes and cross-instance continuation work; disabled accounts remain listed.
- All account routes preserve their OpenAPI operation IDs/method selection. Route/method selection precedes authentication; API GET routes do not acquire automatic HEAD success. JSON/body/query rejection uses shared strict decoders. 401 includes `WWW-Authenticate: Bearer`; error request IDs match headers; all responses are no-store; HEAD and 204 have zero body bytes. Allowlisted access-log templates hide account UUIDs.
- `ferl-server bootstrap-admin --username <username>` uses `BootstrapConfig` with only `FERL_DATABASE_URL` and `FERL_BOOTSTRAP_{CONNECT,LOCK,STATEMENT,TOTAL}_TIMEOUT_SECS` (defaults 10/5/10/30 seconds, each 1–300; total covers connection/lock). It requires a previously migrated compatible schema and never migrates. It locks authority then `system_state`, atomically inserts one enabled admin and marks completion, and emits the `AccountCreation` JSON DTO as exactly one stdout line only after acknowledged commit. Diagnostics use stderr. Exit 0 means success, 2 invalid config/username, 1 storage/schema/already-completed/output failure. Lost/failed output never reopens bootstrap or prints a replacement; recovery is an operator concern. Serve and migrate never bootstrap.
- `AccountApiKey` and `ManagementTokenSecret` are distinct one-time secret types; response DTO constructors cannot exchange account keys and management tokens. Database-byte digest conversion is checked explicitly.
- No schema change or migration was needed; published `migrations/0001_initial.sql` and `Cargo.lock` are unchanged. Serving code remains scoped to Part 04; Parts 05–11 endpoints/services were not started.

### Changed files

Added `src/auth/{mod,bearer,principal,guard}.rs`, `src/accounts/{mod,bootstrap,handlers,repository,service}.rs`, `tests/{authentication,accounts,bootstrap,account_concurrency,account_process}.rs`, `tests/support/{accounts,openapi,serving_process}.rs`. Updated `src/{app,config,main,lib}.rs`, `src/domain/secret.rs`, `src/http/{dto,mod,router,request_id}.rs`, `src/pagination/codec.rs`, `src/telemetry/logging.rs`, `tests/{domain_contracts,foundation_http,foundation_process,openapi_contracts}.rs`, `docs/{configuration,database,development}.md`, this record, and `acceptance-map.md`. No normative business/OpenAPI contract was changed.

### Verification evidence

- PostgreSQL prerequisite: `podman info`, `podman machine list`, `sh scripts/postgres.sh start`, and `sh scripts/postgres.sh ready` passed. Tests used `IsolatedTestDatabase` and its fixture-owned `schema_database_url()` for migrations, production pools, CLI commands, and independent serving processes. The pinned PostgreSQL 17.6 `ferl-stage01-postgres` container/`ferl-stage01-postgres-data` volume was restarted only under the shared harness lock and preserved.
- `sh scripts/check.sh` — final full gate passed after the commit-uncertainty test and all implementation changes. It ran `cargo fmt --all -- --check`, `cargo check --workspace --all-targets --all-features`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, and `cargo test --workspace --all-targets`. Results: 69 library tests, 7 account-concurrency, 1 account-process, 4 accounts, 1 authentication, 5 bootstrap, 2 coordination, 6 cursor-codec, 5 domain-contract, 2 foundation HTTP, 1 PostgreSQL restart/readiness, 2 process, 5 HTTP-decoding, 7 migration, 5 OpenAPI, and 4 persistence tests passed (126 total; binary unit target 0; none ignored/skipped).
- `cargo test --doc` — passed; 0 doctests are defined.
- Concrete Part 04 evidence includes: `tests/accounts.rs::{fresh_authentication_role_matrix_and_secret_rotation_are_exact,account_admin_changes_preserve_noops_disabled_resets_and_last_admin,account_deletion_cascades_children_keeps_reservation_and_allows_clean_username_reuse,account_pagination_is_keyset_bound_and_continues_after_boundary_deletion}`; `tests/authentication.rs::failing_credential_and_persistence_paths_emit_only_safe_diagnostics` (including real auth and post-auth mutation database outages, captured with the production log-target allowlist); `tests/bootstrap.rs::{bootstrap_result_is_one_time_private_and_survives_postgres_restart,bootstrap_conflict_rolls_back_completion_and_broken_result_stream_never_reopens,bootstrap_rejects_an_unmigrated_schema_without_applying_ddl,concurrent_bootstrap_processes_have_exactly_one_winner,bootstrap_failure_after_admin_insert_rolls_back_admin_and_completion_together}`; `tests/account_concurrency.rs::{account_authority_revalidates_after_wait_and_holds_shared_guard_through_commit,authority_guard_timeout_and_cancellation_roll_back_and_release_waiters,waiting_admin_self_action_observes_the_committed_demotion,anonymous_token_row_guard_revalidates_and_conceals_other_links,link_authority_helpers_check_live_fixture_ownership_without_leaking_details,controlled_last_admin_removal_races_never_commit_zero_enabled_admins,lost_commit_acknowledgement_is_not_replayed_as_a_second_account_creation}`; `tests/account_process.rs::independent_servers_share_fresh_credentials_account_cursors_and_last_admin_gate`; and `tests/openapi_contracts.rs::{actual_bootstrap_dto_and_account_http_responses_match_openapi_components,json_schema_ref_sibling_assertions_are_conjunctive_and_formats_are_asserted}`.
- `tests/authentication.rs` captures actual failing-path diagnostics with malformed/unmatched credentials, invalid bodies/queries, stored-data decode failure and database outages, asserts the intended diagnostic categories were emitted, and checks the canaries are absent. The mutation outage stops/restarts only the named harness container while holding its interprocess lock and proves no partial account remains.
- Toolchain evidence: `rustc 1.99.1 (58cf8f9c3 2026-10-13)` and `cargo 1.99.1 (5f94df478 2026-08-27)`; `cargo metadata --format-version 1` resolved 345 packages, none declares `rust_version` above 1.94.0. Rust 1.94 was not run and is not claimed.
- `sh -n scripts/check.sh scripts/postgres.sh`, `git diff --check`, staged `git diff --cached --check`, and the post-test `sh scripts/postgres.sh ready` passed. Full staged inventory was reviewed; normative business/OpenAPI files, `migrations/0001_initial.sql`, `Cargo.toml`, and `Cargo.lock` are unchanged. No generated/secret files are included.

Acceptance contributions: scenario 2 credential/error distinctions; scenarios 5–9 bootstrap, account key lifecycle, suspension, last-admin races, deletion; scenario 20 bootstrap persistence/restart subset; scenario 27 real authentication and mutation outages; scenario 28 guard order/revalidation; scenario 32 independent-server account auth/cursor continuation; scenario 34 independent-process bootstrap/admin races. Fixture-level object authority evidence contributes to scenarios 3–4 but their public link services remain Parts 06/09. Bootstrap/account response components and actual account HTTP outputs contribute to scenario 51 but complete release conformance remains Part 11. Exact test mappings and later responsibilities are in `acceptance-map.md`.

Part 04 is complete and merged. At that checkpoint Part 05 had not begun. Part 05 followed on its own branch and is complete as recorded below; Parts 06–09 link/redirect/analytics services and Parts 10–11 OTLP/full release verification remain later responsibilities. The historical Part 04 test named `lost_commit_acknowledgement_is_not_replayed_as_a_second_account_creation` terminates a backend inside a deferred COMMIT trigger and proves an aborted transaction, not durable commit with lost acknowledgement. Part 05 adds distinct wire-proxy evidence for the latter.

## Part 05 handoff — 2026-10-28

Status: **Complete**. The closing full verification gate passed and the verified
part is integrated into local `main` by this explicit no-ff merge commit.

Part branch: `implementation/05-client-ip-and-rate-limits`.

Base: `14ec982566c348f6b59e1f7a715f7eefc2ab9b19`, the verified Part 04 merge.
The initial inspection confirmed clean `main`/index/tree, both expected merge
parents and relevant ancestry back to baseline
`40b3ae713f0cd9bd2389f872d2953d29ef38f38d`, retained Part 04 tip
`1aacfbca49b94c000cd7ab17b20a3609b4c740dd`, no existing Part 05 branch and no
pending pseudo-ref or merge/rebase/sequencer operation directory. Work uses the
configured repository-local `LubosBa <30705301+LubosBa@users.noreply.github.com>` identity. No branch
rewrite, remote configuration or push occurred.

Verified implementation: `cda4b77866e96b498be144d6f819cd4a113a6896`
(`feat(part-05): add effective IP and durable creation allowance`).
Integrated part tip: `6c21d8ab8a4d05f7c41166da98d8fb3d42ffd7d3`
(`docs(part-05): record verified implementation commit`).
Integration is recorded by this merge commit, whose parents are
`14ec982566c348f6b59e1f7a715f7eefc2ab9b19` and
`6c21d8ab8a4d05f7c41166da98d8fb3d42ffd7d3`. Before completion metadata,
`git diff --cached --exit-code implementation/05-client-ip-and-rate-limits` and
`git diff --exit-code` confirmed exact equality with the reviewed part tree.
Only this progress completion metadata differs from the integrated part tip;
implementation/build inputs remain identical to the closing full gate.
The actual Part 05 integration merge is
`bbe53b3b439d961b88e0a56b877ad3e409411f2e`.

### Delivered behavior and integration contract

- `client_ip::{CanonicalIp, ClientIpResolver}` centralizes canonical mapped-IP
  identity, host-mask persistence and redacted diagnostics. Full trusted-chain
  validation precedes right-to-left selection; invalid/missing/duplicate/33-hop
  chains fall back to canonical peer. Only X-Forwarded-For has conditional
  authority. Original IPv6 proxy prefixes are retained; canonical IPv4 is compared
  using its mapped representation against IPv6 ranges, preserving exact /128,
  /120→IPv4 /24, /96 and broader-prefix membership. Parsing stores at most 32
  addresses, relies on transport header bounds, and imposes no extra whitespace
  byte limit. The existing visit DTO converter now uses the same canonical type.
- `main.rs` uses `app::transport_service` to install accepted TCP
  `ConnectInfo<SocketAddr>`. Missing context is an internal integration failure,
  never an invented anonymous identity. Test-owned TCP composition invokes the
  same production wrapper and resolver.
- `AppState::from_config` composes actual anonymous/proxy/limit/timeout settings
  into `CreationService`. Its old partial constructor was removed; the two
  existing tests using it now use validated production configuration. New startup
  `FERL_CREATION_TIMEOUT_SECS` defaults to 15s, bounded 1–300. Existing 5 anonymous
  and 60 user/admin defaults and overrides are preserved.
- `CreationService::prepare(&Parts)` performs fresh authentication → eligibility
  → independently acknowledged charge, without access to body frames. Valid
  management tokens are forbidden, missing credentials obey the anonymous switch,
  supplied-invalid credentials never fall back, availability is 503, corruption
  is sanitized 500. Account charges use the shared account guard, fresh separate
  post-wait credential/status/role lookup, and retain that guard through COMMIT.
- `ChargedAttempt` has private fields, no Clone/Copy/serialization or public
  identity constructor, and owns its creator/evidence for this attempt.
  **Part 06 sequence:** select route/method; split request; call `prepare`; retain
  the acknowledged charge; strictly decode/validate body and feature permissions;
  consume the charge by ownership; open a new creation transaction and call the
  shared account guard again using `creator.principal()` when authenticated.
  `subject()` is identity, not authorization. Later rotation/validation/creation
  failure leaves the charge committed. No link endpoint, allocation/CRUD/token
  business service, redirect/worker, SPA or exporter is delivered here.
- READ COMMITTED protocol: shared authority gate if account → stable subject
  bucket FOR UPDATE → bucket charge rows. Unique partial indexes coordinate first
  use; INSERT ON CONFLICT DO NOTHING is followed by a fresh-statement lookup even
  when the winner was invisible to the INSERT snapshot. Account buckets have no
  ownership cascade. Every helper uses the passed active connection; independent
  one-connection production pools work without hidden checkout.
- One database `clock_timestamp()` sample follows the bucket wait. Private checked
  `DatabaseInstant` preserves PostgreSQL binary microseconds, including valid
  ranges beyond Chrono, without application time/float arithmetic or request-time
  truncation. Count `(sample - 60s, sample]`, excluding future charges; insert one
  explicitly sampled charge iff `n < L`. Rejection changes no charge ID/time.
  Retry-After is `max(1, ceil(t[n-L] + 60s - sample))`, using exact PostgreSQL
  NUMERIC interval extraction. Seven charges at reduced limit three select the
  fifth oldest, established by controlled database-time tests.
- The shared-gate helper's local statement setting is explicitly restored to the
  configured SQL bound before bucket queries; lock, SQL, auth and total operation
  deadlines stay distinct. Error rollback has a 2s bound within the total bound.
  Cancelling the owning service future drops the transaction/queues SQLx rollback;
  server-side statement/lock bounds remain active. Private borrowed-connection
  futures require rollback/drop on cancellation; no mutation continues afterward.
  Existing bounded server drain/schema-monitor join/pool shutdown remain composed.
- No internal retries. I/O failure or timeout during COMMIT is uncertain and
  returns sanitized storage 503, without capability, automatic replay or refund.
  The failure suite distinguishes an aborted in-flight COMMIT from a durable
  COMMIT whose acknowledgement a controlled loopback test proxy withholds after
  independent durable-row observation. Exactly one durable charge remains in the
  latter case; no second charge is attempted.
- Opportunistic cleanup only on admission removes at most 128 expired charge rows
  at/before the sampled cutoff under the same bucket lock/transaction. Stable
  buckets remain; idle expired history is ignored until later use. A cleanup
  failure aborts admission. Multiple processes safely prune the same expired
  history. No worker/task/interval or global unbounded delete is introduced.
- `http::rejection::creation_error` supplies reusable 401/403/429/503/500 mapping,
  positive Retry-After and the production request-ID/no-store/JSON/bodyless-HEAD
  envelope. Actual HTTP errors are schema/header validated with the locked
  OpenAPI tooling, including ref-sibling conjunction, formats and invalid controls.
  Health/accounts and unimplemented link/unknown fallbacks consume no allowance.

Narrow prerequisite correction: the actual production logging capture initially
failed because nested per-layer filters suppressed required application events
after SQLx activity (including acknowledged `charged`). Composing EnvFilter and
the dependency-target allowlist globally restored those events while retaining
dependency suppression. `tests/rate_limit_failures.rs` reproduced the defect and
now captures the actual JSON logger with diagnostic-presence and canary-exclusion
assertions. Historical Part 04 verification records remain preserved; its stale
future-tense hash wording and distinction between its aborted-COMMIT test and true
lost acknowledgement are corrected above. No normative contract conflict or
normative business/OpenAPI edit was needed.

### Changed files and compatibility

Added `src/client_ip.rs`, `src/rate_limit/{mod,bucket,clock,repository,service,maintenance,tests}.rs`,
`tests/{client_ip,rate_limits,rate_limit_concurrency,rate_limit_failures,rate_limit_process}.rs`,
`tests/support/{rate_limits,commit_proxy}.rs`, and `docs/creation-allowance.md`.
Updated `src/{app,config,lib,main}.rs`, `src/http/{dto,mod,rejection}.rs`,
`src/telemetry/logging.rs`, `tests/{authentication,foundation_http}.rs`,
`tests/support/{database,openapi}.rs`, `scripts/check.sh`, `.env.example`,
`docs/{configuration,database,development,domain-contracts}.md`, this record and
`acceptance-map.md`. Test scenarios now have a bounded cleanup-on-assertion wrapper;
child executables/captures are part of the test harness, never production commands.

**No migration or dependency change:** `migrations/0001_initial.sql`, `Cargo.toml`,
`Cargo.lock`, schema version 1, PostgreSQL 17.x compatibility, and pinned Rust
1.99.1/package MSRV 1.94 remain intact. SQLx 0.9.0's actual timestamp codec was
inspected before choosing the accounting representation. Complete decisions and
consumption details are in `docs/creation-allowance.md`.

### Verification evidence

- `podman info`; `podman machine list`; `sh scripts/postgres.sh start`;
  `sh scripts/postgres.sh ready` — passed. Existing machine was running; existing
  label/digest/port/volume-verified PostgreSQL 17.6 harness was reused. No image pull
  or TLS change was needed. Every database/child pool used `IsolatedTestDatabase` and its
  schema URL. Container restart retained `ferl-stage01-postgres-data` under the
  existing cross-process lock without invoking a recursively locking script.
- Full `sh scripts/check.sh` passed: `cargo fmt --all -- --check`,
  `cargo check --workspace --all-targets --all-features`,
  `cargo clippy --workspace --all-targets --all-features -- -D warnings`,
  `cargo test --workspace --all-targets`, and explicit `cargo test --doc`.
  Observed **144 tests + 3 compile-fail doctests**, none ignored/skipped:
  72 library; 7 account concurrency; 1 account process; 4 accounts;
  1 authentication; 5 bootstrap; 4 client IP; 2 coordination; 6 cursor;
  5 domain; 2 foundation HTTP; 1 PostgreSQL readiness/restart; 2 foundation process;
  5 HTTP decoding; 7 migrations; 5 OpenAPI; 4 persistence; 4 rate concurrency;
  1 rate failure/privacy; 1 rate process; 5 rate allowance. Binary unit target 0.
  The final mapped exact-address startup fixture is included in the closing gate.
- Exact new test names are mapped in `acceptance-map.md` and
  `docs/creation-allowance.md`. Important evidence includes
  `concurrent_first_use_shared_guards_and_unique_buckets_admit_exact_limit`,
  `exact_microsecond_window_rounding_equal_times_and_reduced_limits`,
  `eligibility_and_body_order_preserve_independently_committed_charges`,
  `real_failures_commit_uncertainty_http_contract_and_production_diagnostics_are_private`,
  and `independent_processes_share_allowance_and_restarts_preserve_durable_charges`.
  The latter re-executes independently running test workers for both account/IP
  categories, verifies ten database waiters and exactly five combined committed
  admissions, process replacement, bounded concurrent cleanup and named-volume
  restart. Production stdout/stderr is captured rather than discarded.
- `rustc --version`: `rustc 1.99.1 (58cf8f9c3 2026-10-13) (Homebrew)`;
  `cargo --version`: `cargo 1.99.1 (5f94df478 2026-08-27) (Homebrew)`.
  `cargo metadata --format-version 1 --locked`: 345 packages, no advertised
  dependency MSRV above 1.94. This is metadata evidence; Rust 1.94 was not run.
- Intermediate compilation exposed the test harness needing the shared envelope
  module's public library access, and Clippy requested eager `unwrap_or`; causes
  were corrected. The real logging regression above was fixed without weakening
  diagnostic-presence/privacy assertions. All affected suites then passed.
- `sh -n scripts/check.sh scripts/postgres.sh`, `git diff --check`, and
  `git diff --cached --check` passed. The complete 36-file staged diff against
  `main`, including every new file, was reviewed. `git diff --exit-code` confirmed
  the working tree matches the staged content; explicit contract/migration/build
  manifest/lockfile comparison found no changes. No generated files or secrets
  are included. Post-gate `sh scripts/postgres.sh ready` passed. Only handoff
  metadata changes follow the tested implementation.

Acceptance contributions: scenarios **16–19, 27–28, 33**, and the accounting,
coordination, stdout privacy and reusable-error subsets of **20/30/34/51**.
These are production-service/fixture/independent-worker contributions; public
createLink composition remains Part 06, resolver visit capture Part 07, complete
OTLP privacy Part 10, and broad multi-service/release conformance Part 11. None
of those broad scenarios is marked release-complete here.

Required verification checks unexecuted: **none**. Part 05 blockers: **none**.
The verified part branch is retained for traceability. The merge's resulting hash
is reported from Git after commit rather than embedded in its own tree.
Historical next-part note: Part 06 — Link management followed the Part 05
checkpoint. It is now active on `implementation/06-link-management` from this
verified Part 05 merge; its completed handoff will be recorded below. Public
creation consumes the acknowledged charge before body validation and revalidates
account authority in a separate protected creation transaction.

## Part 06 handoff — 2026-10-28

Status: **Complete**. The Part 06 implementation and closing verification gate
passed on `implementation/06-link-management` and are integrated into local
`main` by this explicit no-ff merge.

### Git checkpoint and traceability

- Repository root: `$PWD`.
- Base/main: `bbe53b3b439d961b88e0a56b877ad3e409411f2e`, the verified Part 05
  merge with parents `14ec982566c348f6b59e1f7a715f7eefc2ab9b19` and
  `6c21d8ab8a4d05f7c41166da98d8fb3d42ffd7d3`. The retained Part 05 branch and
  tip remain `implementation/05-client-ip-and-rate-limits` and
  `6c21d8ab8a4d05f7c41166da98d8fb3d42ffd7d3`; verified Part 05 implementation
  is `cda4b77866e96b498be144d6f819cd4a113a6896`.
- `implementation/06-link-management` already existed at the expected base when
  resumed. Its working tree contained the in-progress Part 06 source/docs/tests
  listed below; the index was initially clean. The main and branch refs, merge
  ancestry, retained Part 05 branch, and absence of merge/rebase/cherry-pick/
  revert/sequencer operation state were inspected. No existing branch or commit
  was reset, replaced, amended, or force-updated.
- Local identity: `LubosBa <30705301+LubosBa@users.noreply.github.com>` from repository configuration.
  No global identity, remote, or push was configured.
- Base commit: `bbe53b3b439d961b88e0a56b877ad3e409411f2e`.
- Verified implementation commit: `264f3f30af32d4e1b8460504f7d0abed62590d6e`
  (`feat(part-06): implement link management`). Integrated part tip:
  `968b4eaaafae7f21723763c7fc76d2e4890ffa70` from the handoff-only follow-up
  `docs(part-06): finalize acceptance traceability`.
- Integration state: Complete by this explicit no-ff merge commit, with
  parents `bbe53b3b439d961b88e0a56b877ad3e409411f2e` and
  `968b4eaaafae7f21723763c7fc76d2e4890ffa70`. The merge's resulting hash is
  obtained from Git after commit and is intentionally not embedded in its own
  tree.

### Implemented Part 06 behavior

- `AppState::from_config` composes `LinkService` with the actual validated
  `PublicOrigin`, production primary pool, configured link-operation/statement/
  lock/acquire bounds, and the Part 05 `CreationService`. No handler default or
  request `Host`/forwarded origin is used. Added bounded
  `FERL_LINK_OPERATION_TIMEOUT_SECS` (default 30s, range 1–300) to validated
  serving configuration, `.env.example`, and `docs/configuration.md`.
- Registered exactly `POST /api/v1/links`, `GET /api/v1/links/{code}`,
  `PATCH /api/v1/links/{code}`, `DELETE /api/v1/links/{code}`, and
  `POST /api/v1/links/{code}/management-token`. Dispatch selects paths/methods
  before authentication, advertises only implemented methods, does not install
  future collection behavior, and does not provide implicit API HEAD success.
  Safe request-log templates hide resolved codes.
- Actual createLink pipeline: route/method selection → split request into parts
  and body → `state.creation.prepare(&parts)` → charged creator eligibility and
  independently acknowledged allowance commit → query/media/streamed-body/raw
  field/domain/feature validation → consume `ChargedAttempt` by ownership in
  `LinkService::create` → new creation transaction → acknowledged commit → 201.
  The handler has no Axum JSON extractor, eager body middleware, content-length
  shortcut, or pre-charge query validation. Anonymous `short_code` raw-member
  presence wins before ordinary field validation, including null/wrong type.
  Eligible malformed, oversized, unsupported-media, invalid-field, conflict,
  allocation, and post-charge persistence failures retain their earlier charge.
- `ChargedAttempt` remains private-field, non-Clone/Copy/non-serializable. The
  link service consumes it; account creation retains the presented `Principal`
  and revalidates it in a *new* READ COMMITTED transaction after taking the
  shared authority gate. `owner_id` comes only from the fresh guarded caller.
  Anonymous creation uses a separate transaction, stores null owner plus one
  fresh management-token digest, and emits plaintext only in the committed
  one-time response. No allowance transaction/bucket lock spans body reads,
  allocation, or link writes; charges are never refunded.
- Generated codes use the existing secure uniform eight-character generator,
  share the permanent reservation namespace with custom codes, and try at most
  ten `INSERT ... ON CONFLICT DO NOTHING RETURNING` candidates. Collisions do
  not abort the transaction; unrelated constraint/decode/storage errors are not
  reclassified as collisions. A private internal candidate-source seam is
  replaceable only by in-crate tests; HTTP and environment values cannot control
  it, and production composition always uses OS randomness. Custom `api` and
  occupied/deleted codes return 409; `assets`, `health`, and `api-sale` remain
  valid custom codes. Reservation/link/search/tag writes are atomic; rollback
   leaves no new reservation or child rows, while committed deletion retains only
   the permanent reservation.
- Narrow prerequisite safety corrections: Part 06 link rows use a checked finite
  PostgreSQL-microsecond `DatabaseTimestamp` decoder rather than SQLx 0.9's
  unchecked Chrono binary addition; the `infinity` stored-value HTTP test proves
  a sanitized 500 rather than a driver panic. Inspection of locked `uuid`
  1.29.0 also showed `Uuid::new_v4()` panics on `getrandom` failure. New link-ID
  generation uses fallible `getrandom::fill` plus
  `Builder::from_random_bytes`; `links::service::tests::link_identifier_randomness_failure_is_typed_instead_of_panicking`
  verifies deterministic failure maps to the internal typed error without
  panic. These fixes are limited to new link row/ID paths; no completed-stage
  rewrite was made.
- The reusable `links::representation::build` delegates to checked
  `LinkDto::from_record(record, canonical_tags, configured_origin, owner_state)`.
  Representations contain exactly the required 12 fields, explicit null title
  and owner, sorted/deduplicated canonical tags, checked counts/timestamps,
  configured short URL, and computed current `active`; no digest or plaintext
  token is serialized. A single-statement link/owner/tag snapshot prevents mixed
  metadata versions. No-op canonical PATCH commits without changing `updated_at`;
  actual destination/search, title/search, enabled and complete tag-set changes
  update atomically with database time. IDs/code/owner/created time/counter are
  immutable. DELETE cascades only its live link children and preserves its code.
- Protected management checks current authority before reading mutation bodies,
  release those preflight guards before body I/O, and repeat complete checks in
  the actual mutation transaction. Account operations take the shared
  account-authority gate, re-read key/existence/enabled/current role after the
  wait, lock the target live link, and retain locks through commit. Token
  operations lock/revalidate the live token-bearing row; token rotation and
  token-authorized edits/deletion use that same row protocol. Admin token
  replacement additionally revalidates current admin role and rejects owned
  links with `not_anonymous_link`. Lock order is authority gate → account rows →
  live link rows → child rows. Bounded gate helpers leave lock-deadline
  `statement_timeout`; link services explicitly restore configured statement
  and lock bounds before subsequent work. No hidden pool checkout occurs inside
  a held transaction.
- `GET` and `PATCH`/`DELETE` conceal unrelated objects as 404, missing auth is
  401 `authentication_required`, invalid/revoked/disabled credentials are 401
  `invalid_credentials` with Bearer challenge, and valid wrong operation scope
  is 403. Enabled owners, admins, and a matching management token manage their
  permitted links; admins may inspect/manage disabled links and suspended-owner
  links. Old anonymous tokens stop authenticating after rotation/deletion.
- Commit errors/deadlines return sanitized 503 and no DTO/plaintext secret. No
  mutation transaction is replayed, no reservation is released after unknown
  commit, no charge is refunded, and no create idempotency is added. A test-only
  PostgreSQL wire proxy targets a specified successful COMMIT ordinal so the
  actual allowance COMMIT is acknowledged before the create COMMIT response is
  withheld. It similarly targets post-preflight PATCH, DELETE, and token
  replacement commits. Independent durable-state reads prove the committed
  outcomes and no success DTO/204/token plaintext is returned. Separate
  trigger/backend-termination evidence proves an aborted create COMMIT leaves its
  charge but no link/reservation/tags. No uncertain request is automatically
  replayed or compensated.
- Added `links::resolve_active_link(&PgPool, code, deadline)` as the Part 07
  handoff. Its fresh single SQL statement joins link/owner state on the primary,
  verifies anonymous/owned invariants and stored values, applies enabled/live
  owner state, and returns a minimal typed immutable destination snapshot. It
  distinguishes unavailable code from storage/conversion failure, has no cache
  or credential bypass, and adds no redirect, visit event, queue, or worker.

### Files, compatibility, and contract review

New production modules: `src/links/{mod,allocation,repository,service,handlers,representation}.rs`.
Updated production seams: `src/{app,config,lib}.rs`,
`src/accounts/handlers.rs`, `src/auth/guard.rs`, `src/db/records.rs`,
`src/domain/url.rs`, `src/http/{dto,request_id}.rs`, and `scripts/check.sh`.
Executable evidence adds `src/links/service/tests.rs`,
`tests/{creation_order,link_creation,link_authorization,link_management,token_rotation,link_concurrency,link_resolution,link_failures,link_process}.rs`,
and narrowly extends `tests/support/{accounts,commit_proxy,database,openapi,serving_process}.rs`.
Operator/test documentation updates `docs/{configuration,database,development,domain-contracts}.md`,
`.env.example`, and this record plus `acceptance-map.md`.

No migration or Cargo dependency/lockfile change was required. Published
`migrations/0001_initial.sql` is unchanged. Supported ferl schema remains v1 and
PostgreSQL compatibility remains 17.x. Normative `server-business-logic.md` and
`openapi.yaml` were not edited; no contract conflict was found. Prior Part 01
container-pull TLS history and Part 05 verification/integration records remain
unchanged. No Part 07/08 service, public redirect, visit ingestion, list/search,
tag discovery, report, analytics worker, SPA, OTLP exporter, or release-wide
deployment feature was added.

### Closing verification evidence

- Environment: `podman info`, `podman machine list`, and
  `sh scripts/postgres.sh ready` passed. Podman 6.1.4 and its existing machine
  were available; pinned PostgreSQL 17.6 `ferl-stage01-postgres` was ready.
  The link-process suite restarted only that verified container directly while
  holding `IsolatedTestDatabase`'s common interprocess harness lock, preserved
  `ferl-stage01-postgres-data`, then verified links/tags/digests/reservations/
  charges through a replacement production server. No image pull or TLS change
  was needed.
- `sh scripts/check.sh` passed after final code/test changes. It ran
  `cargo fmt --all -- --check`,
  `cargo check --workspace --all-targets --all-features`,
  `cargo clippy --workspace --all-targets --all-features -- -D warnings`,
  `cargo test --workspace --all-targets`, and `cargo test --doc`. Results:
  **168 tests plus 3 compile-fail doctests passed**, with none ignored or skipped:
  77 library, 7 account concurrency, 1 account process, 4 accounts,
  1 authentication, 5 bootstrap, 4 client IP, 2 coordination,
  4 creation-order, 6 cursor, 5 domain-contract, 2 foundation HTTP,
  1 foundation PostgreSQL restart/readiness, 2 foundation process,
  5 HTTP-decoding, 1 link-authorization, 4 link-concurrency, 3 link-creation,
  2 link-failures, 2 link-management, 1 link-process, 1 link-resolution,
  7 migrations, 5 OpenAPI, 4 persistence, 4 rate-concurrency,
  1 rate-failure/privacy, 1 rate-process, 5 rate-limit allowance, and
  1 token-rotation test. Binary unit target: 0 tests.
- `cargo metadata --format-version 1 --locked` resolved 345 packages; none
  advertises an MSRV above 1.94. Actual toolchain was
  `rustc 1.99.1 (58cf8f9c3 2026-10-13) (Homebrew)` and
  `cargo 1.99.1 (5f94df478 2026-08-27) (Homebrew)`. Rust 1.94 was not run and
  is not claimed. `sh -n scripts/check.sh scripts/postgres.sh` and
  `git diff --check` passed.
- Concrete Part 06 test evidence: `tests/creation_order.rs::{route_and_eligibility_rejections_precede_body_reads_and_allowance_charges,every_eligible_invalid_attempt_is_charged_after_prepare,account_rotation_after_charge_and_before_body_completion_blocks_creation,default_public_create_limits_charge_five_anonymous_and_sixty_account_attempts}`;
  `tests/link_creation.rs::{production_create_routes_return_complete_owned_and_anonymous_representations,one_connection_pool_composes_allowance_and_new_creation_transaction,permanent_code_namespace_reserves_api_but_not_asset_or_health_root_names}`;
  `tests/link_management.rs::{patch_detail_noops_and_delete_are_atomic_and_keep_permanent_reservations,concurrent_tag_replacements_commit_complete_sets_without_union}`;
  `tests/link_authorization.rs::public_link_operations_apply_fresh_object_authority_and_conceal_other_objects`;
  `tests/token_rotation.rs::token_replacement_is_scoped_bodyless_one_time_and_survives_creation_disable`;
  `tests/link_concurrency.rs::{concurrent_custom_code_requests_have_one_winner_and_one_durable_reservation,guarded_account_mutations_revalidate_after_wait_and_hold_shared_gate_to_commit,link_row_lock_timeout_and_request_cancellation_leave_no_partial_mutation,token_patch_serializes_with_rotation_and_deletion_on_the_live_link_row}`;
  `tests/link_failures.rs::{actual_link_http_contract_storage_failures_and_production_diagnostics_are_private,creation_and_token_commit_acknowledgement_uncertainty_is_not_replayed}`;
  `tests/link_process.rs::independent_production_servers_share_link_state_allowance_and_restart_persistence`;
  `tests/link_resolution.rs::fresh_active_link_resolver_tracks_http_edits_disable_suspension_and_deletion`; and
  `links::service::tests::{generated_allocation_retries_exactly_ten_without_orphan_rows_or_extra_charge,link_identifier_randomness_failure_is_typed_instead_of_panicking}`.
- Acceptance contributions are scoped to scenarios **1–4, 9–13, 16–18, 20,
  26–28, 32–34, 39, 41, and 43–45**, plus relevant privacy/OpenAPI subsets of
  **30/51**, as mapped in `acceptance-map.md`. Later discovery, redirect/visit,
  analytics, OTLP, and full release conformance are explicitly not claimed.

No required Part 06 check remains unexecuted and no blocker remains. Part 07
followed on its own branch and is complete as recorded below. **Next: Part 08 —
Link and tag discovery.**

## Part 07 handoff — 2026-10-28

Status: **Complete**. The Part 07 implementation and full verification gate
passed on `implementation/07-redirects-and-visit-ingestion` and are integrated
into local `main` by this explicit no-ff merge.

### Git checkpoint and traceability

- Repository root: `$PWD`.
- Initial checkpoint: clean `main`/index/tree at the expected Part 06 merge
  `6db4ede67927a24381eddb3889d4479e1b7de17a`, with parents
  `bbe53b3b439d961b88e0a56b877ad3e409411f2e` and
  `968b4eaaafae7f21723763c7fc76d2e4890ffa70`. The retained Part 06 branch was
  `implementation/06-link-management` at `968b4eaaafae7f21723763c7fc76d2e4890ffa70`;
  its verified implementation was `264f3f30af32d4e1b8460504f7d0abed62590d6e`.
  Original baseline `40b3ae713f0cd9bd2389f872d2953d29ef38f38d` is an ancestor.
- `implementation/07-redirects-and-visit-ingestion` did not exist and was created
  from that exact main checkpoint. There were no pending merge/rebase/cherry-pick/
  revert/sequencer operations. Local identity is
  `LubosBa <30705301+LubosBa@users.noreply.github.com>`; no global identity, remote, or push was set.
- Base commit: `6db4ede67927a24381eddb3889d4479e1b7de17a`.
- Verified implementation commit: `e7d0bb8d39fa22748642a9f9488e21cb505a8598`
  (`feat(part-07): add redirects and visit ingestion`).
- Integrated part-tip commit: `5f9fe68d8d8c171808fb4e3db9c1dddb6306d1c7`
  (`docs(part-07): record verified implementation commit`), a handoff-only
  follow-up that records the verified implementation hash.
- Integration: explicit no-ff merge parents are
  `6db4ede67927a24381eddb3889d4479e1b7de17a` and
  `5f9fe68d8d8c171808fb4e3db9c1dddb6306d1c7`. The tested source/build tree is
  unchanged; the merge added only this completion metadata. Its resulting hash
  is reported after creation and is intentionally not embedded in its own tree.

### Delivered Part 07 behavior

- `http::router` reserves `/api` and `/api/*` before short codes, sends direct
  `/assets/*`, root, extra segments and invalid spellings to 404, and registers
  only explicit GET and HEAD handlers on the single-segment path. The method
  router has a custom fallback that returns 405/`Allow: GET, HEAD`; it does not
  install Axum's GET-to-HEAD behavior. Dispatch validates the original URI path
  and never percent-decodes, case-folds, trims, or slash-normalizes a code.
  `/assets`, `/health`, and `/api-sale` remain candidate codes; `/api` is
  reserved as a whole segment.
- Public follows ignore Authorization and the incoming query. They call the
  Part 06 `links::resolve_active_link` once with the configured operation
  deadline, obtaining a fresh one-statement primary-pool link/owner snapshot.
  A successfully built response is an empty 302 with the exact checked stored
  serialization in `Location`; no query merge, URL rewriting, network access,
  DNS, destination fetch, or reachability check occurs. Unknown/disabled/
  suspended/deleted are the same 404 with no Location/visit; storage unavailability
  is 503 and persisted conversion corruption is 500, not a fabricated 404.
- Only after 302 construction does the route create an event with the same
  immutable destination snapshot, UUID, server UTC microsecond occurrence time,
  original GET/HEAD method, and `ClientIpResolver`'s canonical transport-owned
  peer/proxy address. A missing `ConnectInfo` is a safe analytics integration
  drop, never an invented IP, and cannot replace the redirect. Referer and
  User-Agent are independently captured only as one complete valid UTF-8 value
  no larger than 8,192 bytes; invalid, missing, duplicated, and oversized values
  are null without truncation or redirect rejection.
- `VisitQueue::try_enqueue` uses the configured bounded Tokio channel without
  awaiting capacity. The configured 1–64 worker pool parses with local bundled
  Woothee 0.13.0 rules and persists into schema-v1 PostgreSQL; no report/list/
  discovery endpoint was registered. Missing or unrecognized UA data yields
  unknown device/browser/OS while valid raw UA remains stored. Events have no
  Debug/serialization surface.
- Each persistence transaction uses one pool connection and the established
  lock order's live link row → visit child row: verify immutable ID/code under
  `FOR UPDATE`, insert the stable UUID with `ON CONFLICT DO NOTHING`, and
  increment the nonnegative counter only for a newly inserted event, in the same
  transaction. Overflow, insert/counter errors, cancellation, and rollback
  cannot leave a partial event/count; visit writes never change `updated_at`.
  There is no account-authority gate because analytics changes neither account
  authority nor link metadata. Deletion-first leaves no link and drops the event;
  worker-first commits while holding the row lock, then deletion waits and
  cascades its event/counter. Disabled state after issuance does not invalidate
  an already-issued event.
- Retries are bounded by `FERL_ANALYTICS_RETRY_BUDGET` after the initial attempt,
  per-attempt `FERL_ANALYTICS_OPERATION_TIMEOUT_SECS`, independent DB acquire/
  statement/lock limits, 25ms exponential backoff capped at 1s, and
  `FERL_ANALYTICS_ROLLBACK_TIMEOUT_SECS`. Transient failures and even unknown
  COMMIT acknowledgement retry only with the same event UUID/time/snapshot;
  the unique insert makes a durable prior commit a no-op on retry. Unlike create
  operations, this narrow internal visit retry is idempotent. No HTTP request
  waits for persistence. The bounded in-memory queue is best effort; abrupt
  process termination can lose unflushed events.
- `AppState::start_analytics_workers` is the one-time composition seam.
  `main::serve` starts the bounded pool; `shutdown::stop_accepting_and_drain`
  first completes bounded HTTP drain, then `shutdown::drain_analytics` closes
  enqueue admission and drains only until the independent analytics/total
  shutdown bound before closing the shared pool. Timeouts cancel workers; queue
  backlog does not control readiness. Only safe bounded outcomes and queue depth
  are emitted through the telemetry facade/logger, not codes, URLs, IDs, IPs,
  credentials, query strings, or captured headers.

### Files, configuration, migration, and contract review

- Added `src/analytics/{mod,event,queue,repository,user_agent,worker}.rs`,
  `src/redirects.rs`, `tests/{routing,redirects,visit_ingestion,analytics_failures,visit_concurrency}.rs`,
  and `tests/support/{redirects,analytics_commit_proxy}.rs`.
- Updated `src/{app,config,lib,main,shutdown}.rs`,
  `src/domain/{short_code,url}.rs`, `src/http/{request_id,router}.rs`,
  `src/telemetry/metrics.rs`, `Cargo.toml`, `Cargo.lock`, `.env.example`,
  `scripts/check.sh`, `tests/{foundation_http,foundation_process,link_creation,link_process,openapi_contracts}.rs`,
  `tests/support/serving_process.rs`, `docs/{configuration,database,development,domain-contracts}.md`,
  this progress record, and `acceptance-map.md`.
- New serving configuration: `FERL_ANALYTICS_OPERATION_TIMEOUT_SECS` defaults
  to 30s and `FERL_ANALYTICS_ROLLBACK_TIMEOUT_SECS` to 2s; rollback must be less
  than the operation bound. Existing validated analytics settings now consumed
  by production are capacity 2,048 (1–65,536), workers 4 (1–64), retry budget 3
  (1–10), and drain 5s (1–300). Backoff is fixed/bounded in code. The event
  queue is process-local; there is no durable queue or new command/test route.
- No schema change/migration: schema v1, PostgreSQL 17.x/17.6 support, and
  `migrations/0001_initial.sql` are unchanged. Normative
  `spec/server-business-logic.md` and `spec/openapi.yaml` are unchanged. No
  contract conflict was found. Scenario 51 evidence is only the implemented
  redirect/HEAD/error subset; it does not claim release-wide conformance.

### Closing verification evidence

- `podman info`, `podman machine list`, `sh scripts/postgres.sh start`, and
  `sh scripts/postgres.sh ready` passed before the gate. The existing pinned
  PostgreSQL 17.6 `ferl-stage01-postgres` / persistent
  `ferl-stage01-postgres-data` harness was reused; `ready` was rechecked after
  the full gate. Independent process tests restart only this owned container
  while holding the fixture's shared interprocess lock.
- `sh scripts/check.sh` passed all five prescribed commands:
  `cargo fmt --all -- --check`,
  `cargo check --workspace --all-targets --all-features`,
  `cargo clippy --workspace --all-targets --all-features -- -D warnings`,
  `cargo test --workspace --all-targets`, and `cargo test --doc`. Results:
  **183 tests and 3 compile-fail doctests passed**, with none ignored/skipped:
  84 library tests, binary target 0, and 99 integration tests across the existing
  suites plus the five new Part 07 suites. Exact Part 07 process/route/worker
  evidence is listed in `acceptance-map.md`.
- The gate used `rustc 1.99.1 (58cf8f9c3 2026-10-13) (Homebrew)` and
  `cargo 1.99.1 (5f94df478 2026-08-27) (Homebrew)`. Locked metadata resolved
  346 packages with no advertised dependency MSRV above package MSRV 1.94.
  Rust 1.94 was not run and is not claimed. `git diff --check` and shell syntax
  validation are also part of final review.
- Acceptance contributions: scenarios **7, 13–15, 19, 21–22, 24–28, 35, 37–40,
  and 50**, plus actual route/header/error/schema portions of **51**. Part 11
  still owns complete cross-process pending-queue deletion/release audit and
  release-wide OpenAPI/proxy conformance. Part 09 owns authorized reports;
  Part 10 owns OTLP exporters/full signal privacy and exporter drain.

No required Part 07 gate check remains unexecuted and no blocker remains.
**Next: Part 08 — Link and tag discovery.** Part 09 reports, Part 10 OTLP/lifecycle
export, and Part 11 release verification remain later work.

## Part 08 handoff — 2026-10-28

Status: **Complete**. The Part 08 implementation and its full verification gate
passed on `implementation/08-link-and-tag-discovery`, then were integrated into
local `main` by the explicit no-ff merge recorded after the reviewed
implementation and handoff-only commits.

### Git checkpoint and traceability

- Repository root: `$PWD`.
- Starting state: clean `main`/index/tree at `9bcf7c0c021c994e76d0696633fe822f0a49a5e1`,
  the Part 07 merge with parents `6db4ede67927a24381eddb3889d4479e1b7de17a`
  and `5f9fe68d8d8c171808fb4e3db9c1dddb6306d1c7`. The retained Part 07 branch
  is `implementation/07-redirects-and-visit-ingestion`; its implementation is
  `e7d0bb8d39fa22748642a9f9488e21cb505a8598`. The Part 07 merge is the current
  main/base commit. Original baseline `40b3ae713f0cd9bd2389f872d2953d29ef38f38d`
  is an ancestor.
- The Part 08 branch did not exist and was created from that exact main tip.
  No pending merge, rebase, cherry-pick, revert or sequencer state was present.
  Local identity is `LubosBa <30705301+LubosBa@users.noreply.github.com>`; no remote or global Git
  configuration was changed and nothing was pushed.
- The supplied Part 06 implementation SHA `264f3f35bcdab2e2f563d779fae6728e50b0c1a8`
  was not an object in this repository. `git log --all` and `git show` verified
  the actual reachable implementation as
  `264f3f30af32d4e1b8460504f7d0abed62590d6e` (parent
  `bbe53b3b439d961b88e0a56b877ad3e409411f2e`), integrated by the expected Part
  06 merge `6db4ede67927a24381eddb3889d4479e1b7de17a`. Historical Part 06/07
  handoff references in this progress record and acceptance map are corrected to
  that real hash; no commit history was rewritten.
- Base commit: `9bcf7c0c021c994e76d0696633fe822f0a49a5e1`.
- Verified implementation commit: `1526f60527ac7fd9206fbbdef2b51a11b87d5af8`
  (`feat(part-08): add link and tag discovery`).
- Integrated part tip: `fd9f4d35bda58b82a81834891d95f318fa3cc913`
  (`docs(part-08): record verified implementation commit`), the handoff-only
  follow-up that records the verified implementation hash.
- Integration: explicit no-ff merge parents are
  `9bcf7c0c021c994e76d0696633fe822f0a49a5e1` and
  `fd9f4d35bda58b82a81834891d95f318fa3cc913`. The final progress-only completion
  update is the only content added during that integration merge; tested source,
  migration, lockfile, and test tree match the integrated Part 08 tip. The merge
  commit hash is reported after commit and is intentionally not embedded in its
  own tree.

### Delivered Part 08 behavior

- Registered exactly `GET /api/v1/links` and `GET /api/v1/tags` while retaining
  `POST /api/v1/links` and all Part 06/07 operations. Link collection advertises
  `Allow: GET, POST`; tags advertises `Allow: GET`. Neither has implicit HEAD;
  HEAD is bodyless 405 through the existing request-ID/no-store envelope.
  `accounts::handlers` dispatches both discovery modules through the production
  router; no test-only route, CLI, projection, tag mutation, or reports endpoint
  was added.
- Both handlers authenticate a fresh enabled account before operation-query
  work. Missing credentials return 401 `authentication_required`; supplied
  malformed/revoked/invalid credentials return 401 `invalid_credentials` with
  `WWW-Authenticate: Bearer`; management tokens are 403. Scope defaults to own
  for users/admins. Admin all-scope includes anonymous links and disabled-owner
  links. The shared `RawQuery` decoder now retains encoded values until
  `parse_authorized` applies scope/owner-presence authority: non-admin owner
  presence is 403 even for malformed/repeated owner values; admin owner presence
  outside all-scope is 400 before decoding; permitted malformed values are 400.
  This narrowly fixes the pre-existing parser-before-authority precedence defect
  without adding a second query decoder.
- Link listing filters the authorized PostgreSQL candidate relation before
  keyset selection. q searches normalized destination/code/title using literal
  substring patterns with bound values and explicit `!` escaping; `%`, `_`, `!`
  are escaped and backslash remains literal. `enabled` is the link flag, dates
  are inclusive-lower/exclusive-upper UTC microseconds, tag any/all uses
  non-fan-out association predicates, and filter categories combine with AND.
  Unknown/mixed tags behave as specified. Unknown parameters, repeated scalars,
  invalid enums/limits/intervals and tag-mode-without-tags remain sanitized 400s.
- A single PostgreSQL listing statement uses one snapshot for the bounded
  candidate page, owner-enabled state and canonical ordered tags; it returns at
  most `limit + 1` rows and converts every item with
  `links::representation::build`. All 12 Link fields, explicit nulls, configured
  short URL, persisted count, timestamps and owner-derived active are checked.
  There is no per-item checkout or N+1 hydration. Database, conversion, timeout
  and cancellation failures cannot fabricate an empty collection.
- All six `(created_at|title|visit_count) × (asc|desc)` orders use UUID in the
  same direction. Title predicates/orderings use explicit C collation and NULLS
  LAST in both directions, with null encoded explicitly and no sentinel.
  Cursors use the existing sealed codec/context/typed boundaries, include
  normalized filters and actor/account/scope/owner, exclude limit, and use the
  last returned row. Each page reauthenticates; key rotation preserves a cursor,
  revoked credentials fail 401, demoted all-scope admins fail 403, recreated
  usernames cannot inherit a cursor, and boundary deletion does not require the
  old row. Pagination is live for mutable title/count values; no snapshot store
  or exact traversal under movement is promised.
- Tag discovery groups live visible associations, counts distinct link IDs over
  the complete scope (including individually disabled/suspended-owner links),
  filters q only on canonical tag names, returns positive counts in C/UTF-8
  order, and paginates grouped names after complete counts. Clearing tags,
  deleting links/accounts, and scope-specific final-association removal appear
  immediately after commit.
- Safe failures reuse the existing envelope and configured production pool,
  operation/acquire/statement/lock deadlines. Real lock and pool failures return
  sanitized 503 rather than partial/empty results. No listing/tag operation
  charges creation allowance or emits visits. Actual production logger/process
  tests scan query, cursor, title, tag, destination, credentials, IDs and DB URL
  canaries after proving safe diagnostics are present.

### Migration, indexes, plans and compatibility

- Added append-only `migrations/0002_discovery_indexes.sql`; `0001_initial.sql`
  and its checksum/history are unchanged. Latest ferl schema is v2; supported
  PostgreSQL remains 17.x and the harness is the pinned 17.6 image. Production
  readiness/compatibility verifies the new migration history and each new index
  key definition, collation, validity/readiness and nonunique semantics.
- Eight transactional B-trees cover global/scoped created_at, visit_count and
  title order, with distinct ASC/DESC title indexes to keep NULLS LAST in both
  directions. Existing `(link_id,name)` and `(name,link_id)` indexes serve tag
  association/grouping paths. The two visit_count keys are explicitly accepted
  write amplification on every successful Part 07 worker counter increment.
  No full-width URL B-tree, `pg_trgm`, extension, or extension lifecycle was
  added. Maximum 8,192-byte destination, 200-scalar title, 64-scalar tag and
  256-scalar trimmed raw tag insertion are tested.
- `EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON)` runs on the production SQL builders
  over 20,000 links/22,000 associations. Selective owner and global first and
  continuation pages use their corresponding ordering indexes for all sorts and
  directions. Representative warm-buffer plans reported roughly 0.23–0.35 ms
  for keyset pages, with 51 rows at the limit+1 boundary and no buffer reads.
  Title continuation scans preserve the explicit null boundaries. Literal
  substring q uses the owner candidate index/bitmap then scans 4,000 authorized
  candidates (4,000 rows filtered in the no-match fixture); no trigram/full-text
  index is claimed. Tag-any uses the `(link_id,name)` primary key in a semi-join;
  tag-all uses `(name,link_id)` for association counts (4,000 loops/20,000
  buffer hits in that fixture). Broad/owner tag discovery plans scan and group
  the complete association relation: the measured broad 20k-link fixture used
  sequential scans, hash joins and in-memory sorts (about 42.5 ms/630 buffer
  hits); owner scope processed the 4k-link candidate set (about 19.1 ms/635
  hits). Broad q remains a scan. These are representative plan observations, not
  throughput targets or a demand for index scans on tiny/broad fixtures.
- Migration evidence includes fresh v2, repeat and concurrent CLI migration;
  populated authentic v1→v2 preservation of account/key digests, links/titles,
  tags, visits/counters, reservations, bootstrap and rate charges; missing/wrong
  index rejection; and a held `links` table lock causing bounded version-2 DDL
  timeout, full transaction rollback/no v2 history, then successful retry.
  Normal `CREATE INDEX` was chosen so DDL and SQLx history are transactional;
  it holds a write-blocking table lock during build. The new
  `tests/legacy_compatibility.rs::v1_server_runs_on_v1_then_fails_closed_after_v2_upgrade_and_v2_runs_ready`
  builds the retained v1 source: v1 serves v1, fails closed/unready after v2,
  and v2 is ready after migration. Required rollout is stop/drain v1, migrate,
  verify, then start v2; mixed-version rolling deployment is not supported.
- `tests/discovery_process.rs::discovery_cursors_and_fresh_state_cross_independent_servers_and_volume_restart`
  uses production TCP servers A/B and replacement C against the same isolated
  schema. It exercises create/update/delete, cross-server cursors, key rotation,
  asynchronous worker counter visibility, canaries and a named-volume restart
  while the fixture's shared harness lock is held, without repopulating state.

### Prerequisite correction and changed files

- The full gate initially exposed an intermittent existing Part 04 lock-test
  failure: sibling Tokio test tasks could observe database-scoped advisory locks
  from other isolated schemas because `flock` alone only supplied the
  interprocess boundary. `IsolatedTestDatabase` now owns a bounded-acquisition
  process-local async fixture guard in addition to the existing shared `flock`;
  no production lock/coordination protocol or assertion was changed. The full
  account-concurrency suite and closing gate pass with that regression fix.
- Production: `migrations/0002_discovery_indexes.sql`, `src/{accounts/handlers,db/migrate,http/query,http/request_id,lib,links/handlers,links/list_handler,links/list_repository,links/mod,links/service,pagination/context,tags/*}.rs`, `Cargo.toml` (SQLx JSON decode feature for plan evidence; no version/dependency addition), and `tests/support/{database,serving_process}.rs`.
- Executable suites: `tests/{discovery_failures,discovery_process,discovery_values,link_filters,link_listing,link_pagination,tag_discovery,legacy_compatibility}.rs`; migration, OpenAPI and existing method-boundary tests were extended. Operator/developer docs: `docs/{configuration,creation-allowance,database,development,domain-contracts}.md`, `scripts/check.sh`, this handoff and `acceptance-map.md`.
- `spec/server-business-logic.md`, `spec/openapi.yaml`, `migrations/0001_initial.sql`, and `Cargo.lock` are unchanged; no source/contract conflict was found. Part 09 analytics reports, Part 10 OTLP exporters, and Part 11 release/deployment machinery are not implemented or claimed.

### Closing verification evidence

- `podman info`, `podman machine list`, `sh scripts/postgres.sh start`, and
  `sh scripts/postgres.sh ready` passed. Tests used `IsolatedTestDatabase` and
  its schema URL for migrations, production pools and child processes; only the
  label/digest-verified owned `ferl-stage01-postgres` container was restarted,
  with `ferl-stage01-postgres-data` preserved.
- Exact closing command: `sh scripts/check.sh`. It passed
  `cargo fmt --all -- --check`,
  `cargo check --workspace --all-targets --all-features`,
  `cargo clippy --workspace --all-targets --all-features -- -D warnings`,
  `cargo test --workspace --all-targets`, and `cargo test --doc`.
  Observed **200 all-target tests**: 87 library, 113 integration, binary unit
  target 0; plus **3 compile-fail doctests**. **203 total passed, none ignored
  or skipped.** The 113 integration tests are across 42 binaries, including all
  new discovery suites and the retained-v1 compatibility test.
- Toolchain: `rustc 1.99.1 (58cf8f9c3 2026-10-13) (Homebrew)`;
  `cargo 1.99.1 (5f94df478 2026-08-27) (Homebrew)`. `cargo metadata
  --format-version 1 --locked` resolved 346 packages; no resolved package
  advertises `rust_version` above 1.94.0. This is metadata only; Rust 1.94 was
  not run and is not claimed. PostgreSQL 17.6 is the observed harness version.
- Acceptance contributions: scenarios **3, 20, 27, 30 subset, 32, 34, 41–49,
  and 51 actual discovery response subset**. Reports, OTLP/full-signal privacy,
  and release-wide acceptance remain Parts 09–11.
- No required Part 08 check remains unexecuted and no blocker remains. Part 08 is
  complete and merged. **Part 09 — Analytics reports** is next; no report query,
  exporter, or release/deployment machinery is claimed here.

## Part 09 execution checklist — 2026-10-28

Status: **Complete**. The verified implementation and full gate passed on
`implementation/09-analytics-reports`, then were integrated into local `main` by
the explicit no-ff merge recorded below. The branch was created from clean base
`12c10c234c492dcbdf554a3cc6e1321d83474361`. The base has the
expected Part 08 merge parents `9bcf7c0c021c994e76d0696633fe822f0a49a5e1`
and `fd9f4d35bda58b82a81834891d95f318fa3cc913`; retained Part 08 and earlier
branches remain present. The configured local identity is
`LubosBa <30705301+LubosBa@users.noreply.github.com>`. No other worktree or pending Git operation was
found. Podman is available; the owned PostgreSQL 17.6 harness is accepting
connections, and `scripts/postgres.sh start` / `ready` both pass. On 2026-10-28,
`sh scripts/check.sh` completed successfully with Rust/Cargo 1.99.1: 95 library,
118 integration, and 0 binary tests plus 3 doctests (216 total, none ignored).
The closing output is
`$TMPDIR`.
The complete tracked and untracked diff was reviewed against the Part 08 base;
`git diff --check main` passed. Rust 1.94 was not run.

Endpoint scope is exactly `GET /api/v1/links/{code}/visits`, `GET
/api/v1/links/{code}/stats`, and `GET /api/v1/links/{code}/stats/breakdown`;
unsupported methods, including HEAD, are explicit bodyless 405 responses with
`Allow: GET`. No Part 10 or Part 11 work is in scope.

Concrete prerequisites observed before implementation:

- `VisitRecord.occurred_at` currently decodes directly as `DateTime<Utc>`, unlike
  `LinkRecord` timestamps, which use `DatabaseTimestamp`. The report path must
  use checked PostgreSQL-microsecond decoding and separately enforce the narrower
  RFC 3339 wire range; infinity, out-of-range finite values, and formatting need
  regression evidence.
- The locked transport stack is Axum 0.8.9 / Hyper 1.12.0. Hyper's HTTP/1
  `DEFAULT_MAX_BUFFER_SIZE` is `8192 + 4096 * 100` (417,792 bytes), including
  the request line and headers, with 100 headers by default. The existing cursor
  codec caps tokens at 87,387 bytes. No transport change is proposed unless an
  actual production TCP request-target test disproves interoperability.
- `/api` dispatch currently wraps account/API dispatch in a fixed 45-second
  timeout; discovery has a separate link-operation timeout, and analytics workers
  have their own persistence timeout. Reports need an explicit configured total
  deadline and must be dispatched outside the unrelated 45-second wrapper so a
  report timeout cannot be overridden or mislabeled.
- Existing `Principal` credentials have private fields, but a UUID alone is not
  authority. Report service entry points will accept `&Principal`, perform a
  precedence-preserving read-only authority preflight, then revalidate the
  authenticated credential and object authority in the report snapshot.
- The current schema is v2. It already has
  `visits_link_occurred_id_idx (link_id, occurred_at DESC, id DESC)`, and its
  links/code/credential indexes are verified by the production schema checker.
  No new index or migration is assumed; 8,192-byte header grouping values will
  not receive B-tree keys.

Proposed per-response policy: use the primary pool and one explicitly held
`REPEATABLE READ READ ONLY` transaction/connection for final credential/object
revalidation and report SQL. A small preflight read establishes object-authority
precedence before query decoding; it returns no analytics data and is repeated
inside the final snapshot. No link row lock is taken. A deletion committed before
the final snapshot yields concealed 404; a deletion after that snapshot may let
the in-flight response finish from its old consistent snapshot, while every later
request reauthenticates and reauthorizes. Lifetime counter and matching visit rows
are read from the same snapshot. Connection ownership and cancellation remain
bounded; composition must pass with `FERL_DB_POOL_MAX=1`.

Initial query/transport direction: raw pages use the existing visit ordering index
and `(occurred_at, id) < boundary`; summary counts use the persisted counter plus
interval count within the same snapshot; all eight breakdowns use static SQL
expressions, complete aggregation before null-aware C-collated keyset pagination,
and canonical rendered IP strings. The legal encoded cursor fits below the
observed Hyper buffer, but valid maximum-size header capture, escaped/UTF-8 cursor
continuation over production TCP, and A/B/replacement-C use remain mandatory
evidence.

Execution checklist:

1. [x] Fresh authentication, object authority, and strict query precedence.
2. [x] Consistent per-response snapshot, timeout composition, bounded single
   connection, and `FERL_DB_POOL_MAX=1` composition.
3. [x] Checked raw visit DTOs, historical destinations, and exact keyset pages.
4. [x] Lifetime/interval summaries and worker/counter snapshot correctness.
5. [x] All eight breakdown dimensions, null/string boundaries, and complete
   group counts independent of page size.
6. [x] Maximum legal cursor over actual TCP, across instances and replacement.
7. [x] Production query-plan evidence; v2 migration/compatibility decision.
8. [x] Real HTTP/PostgreSQL/concurrency/process/privacy/fault/OpenAPI evidence.
9. [x] Full gate and complete tracked/untracked diff review; whitespace/error
   checks pass.
10. [x] Explicit local no-ff merge and clean final Git/handoff state.

### Part 09 Git checkpoint and traceability

- Repository root: `$PWD`. The implementation branch
  `implementation/09-analytics-reports` is retained. Its base is Part 08 merge
  `12c10c234c492dcbdf554a3cc6e1321d83474361`, with parents
  `9bcf7c0c021c994e76d0696633fe822f0a49a5e1` and
  `fd9f4d35bda58b82a81834891d95f318fa3cc913`.
- Verified implementation commit: `178e297` (`feat(part-09): add analytics
  reports`). Integrated part tip: `f08b805` (`docs(part-09): record verified
  implementation handoff`), the handoff-only follow-up recording the implementation
  hash.
- Integration: explicit no-ff merge parents are
  `12c10c234c492dcbdf554a3cc6e1321d83474361` and
  `f08b805fa33f13b14bc2ca5257b669e34732c6c9`. The only integration content
  beyond that verified part tip is this Part 09 completion metadata; implementation,
  tests, docs, and gate evidence are otherwise unchanged. The merge commit hash is
  intentionally not embedded in its own tree and is reported after commit.
- `main` was an ancestor of the implementation commit before integration. No
  history was rewritten, no remote was configured, and nothing was pushed. Local
  Git identity remains `LubosBa <30705301+LubosBa@users.noreply.github.com>`.

## Part 10 execution checklist — 2026-10-29

Status: **Complete**. The production implementation and closing gate were
verified on `implementation/10-observability-and-lifecycle` and are integrated
into local `main` by the explicit no-ff merge recorded below.
The branch is based on the verified Part 09 merge
`6153b7684383d066697f473731755c103c870484`. The repository-local identity is
`LubosBa <30705301+LubosBa@users.noreply.github.com>`. The work began from clean `main`/index/tree;
there are no remotes, and no history rewrite or push is part of this workflow.
Verified implementation commit: `1d84f0aacf21bfcf82465463e3c1c623a23cb7f2`
(`feat(part-10): add OTLP observability and bounded lifecycle`). This full hash
is recorded by the handoff-only follow-up commit. Integrated part tip:
`3f21bb4118bbf334a5741ebd9a8f2326a53d72fe` (`docs(part-10): record verified
implementation handoff`). The explicit no-ff merge has parents
`6153b7684383d066697f473731755c103c870484` and
`3f21bb4118bbf334a5741ebd9a8f2326a53d72fe`. Before adding completion metadata,
the staged merge tree exactly matched the reviewed part tip; only this Part 10
completion handoff differs in the merge commit. The merge hash is intentionally
not embedded in its own tree and is reported from local history after commit.
The verified base includes schema v2. Part 10 adds no migration and leaves
`migrations/0001_initial.sql`, `migrations/0002_discovery_indexes.sql`, the
normative business specification, OpenAPI, and `Cargo.lock` unchanged.

### Delivered behavior and decisions

- `src/telemetry/providers.rs` owns actual OTLP/HTTP-protobuf trace, metric, and
  LogRecord providers. It uses the resolved OpenTelemetry 0.33.1 API/SDK/OTLP
  family and `tracing-opentelemetry` 0.34.0, async Tokio trace/log batch
  processors, and a manual-reader metric task. Providers are process-owned; no
  global SDK providers or detector-derived resource attributes are installed.
- Resource attributes use one explicit allowlist and generated per-run
  `service.instance.id`; standard OTEL settings take precedence over FERL
  compatibility settings. Signal-specific exporter settings, arbitrary resource
  attributes, and `FERL_INSTANCE_ID` overrides are rejected. No-endpoint mode
  remains stdout-only and makes no implicit localhost export attempt.
- `src/telemetry/logging.rs` retains structured stdout and adds an explicit
  field-allowlisted OTLP LogRecord bridge. Trace/span attributes, log fields,
  resource keys, metric dimensions, and exporter diagnostics are bounded and
  sanitized. A separate six-slot, rate-limited diagnostic path writes fixed
  categories to stdout only; raw exporter errors/configuration are not visited.
  W3C traceparent is validated for parent-based sampling/correlation; accepted
  tracestate is discarded and baggage is ignored.
- `src/telemetry/metrics.rs` defines bounded HTTP, redirect, creation, auth,
  database, analytics queue/event, and telemetry-log metrics, including
  cancellation-safe request/DB timers. Analytics queue accounting is updated
  under serialized ownership and reset/counts pending drops at shutdown.
- Shutdown uses one absolute total deadline across readiness withdrawal, HTTP
  drain and outstanding-handler cancellation, schema monitor, analytics drain,
  pool closure, and telemetry export. During withdrawal the listener continues
  polling health routes. Export attempts are serial, bounded, and have retries
  disabled; provider flushing is skipped when its precomputed budget cannot fit.
- The pinned Collector harness in `deploy/collector/otelcol.yaml` and
  `scripts/collector.sh` captures all three OTLP signals to bounded JSON files.
  `tests/telemetry_signals.rs` exercises the production executable over TCP with
  the real PostgreSQL and Collector fixtures, including privacy canaries,
  sampling/context, outage/recovery, queue pressure, replacement, readiness
  withdrawal, and stalled-receiver shutdown behavior.

Changed files: `.env.example`, `Cargo.toml`, `docs/{configuration,development,
observability,operations}.md`, `scripts/{check,collector}.sh`,
`deploy/collector/otelcol.yaml`, `spec/implementation/{acceptance-map,progress}.md`,
`src/{accounts/handlers,accounts/service,analytics/handlers,analytics/queue,
analytics/report_repository,analytics/repository,analytics/worker,app,
auth/bearer,auth/guard,config,db/mod,http/request_id,http/router,links/handlers,
links/service,main,rate_limit/service,redirects,shutdown,telemetry/logging,
telemetry/metrics,telemetry/mod,telemetry/providers}.rs`,
`tests/support/serving_process.rs`, and `tests/telemetry_signals.rs`.

### Verification evidence

- Tooling: `rustc 1.99.1 (58cf8f9c3 2026-10-13) (Homebrew)`,
  `cargo 1.99.1 (5f94df478 2026-08-27) (Homebrew)`, and Podman 6.1.4. The real
  database fixture reports PostgreSQL 17.6. The Collector is
  `docker.io/otel/opentelemetry-collector-contrib@sha256:340c9ca6a36617084c6661bdc4f0fe92b7cda5a805d61673ec8db375fddc522b`;
  its image reports version 0.164.0.
- `sh scripts/check.sh` — passed on the final implementation. It ran
  `cargo fmt --all -- --check`,
  `cargo check --workspace --all-targets --all-features`,
  `cargo clippy --workspace --all-targets --all-features -- -D warnings`,
  `cargo test --workspace --all-targets`, and `cargo test --doc`. The full gate
  reported **218 all-target tests** (including the binary target with 0 tests)
  and **3 doctests** passed; none failed or were ignored. It used the real pinned
  PostgreSQL and Collector fixtures.
- Fresh lifecycle rerun:
  `FERL_TEST_DATABASE_URL=postgres://ferl:ferl@127.0.0.1:55432/ferl_test FERL_TEST_POSTGRES_CONTAINER=ferl-stage01-postgres FERL_TEST_POSTGRES_PORT=55432 FERL_TEST_OTEL_ENDPOINT=http://127.0.0.1:43180 FERL_TEST_OTEL_CAPTURE_DIR=$PWD/target/collector-captures/default FERL_COLLECTOR_RUN_ID=default FERL_COLLECTOR_PORT=43180 cargo test --test telemetry_signals -- --nocapture`
  — passed (1 production-process test; 40.28s). Measured graceful exits in
  milliseconds: A 1,019; B 1,012; replacement C 1,013; root-sampling-zero D
  1,015; no-endpoint 1,007; database-outage 1,014; readiness-withdrawal E 4,020;
  stalled HTTP + stalled exporter 15,780. Readiness withdrawal was observed at
  4ms and listener removal at 2,022ms. The stalled case exited below its
  configured 60s total deadline; other measured exits were below 10s.
- The production test verifies actual captured traces, metrics, and LogRecords;
  W3C sampled/unsampled/invalid context and root-ratio-zero behavior; resource,
  span, log and metric allowlists; absence of injected secrets/URLs/IPs/query,
  cursor, title/tag, user-agent, baggage, and trace-state canaries; collector
  refusal/restart; a real PostgreSQL stop/recovery; readiness 503 while liveness
  remains available; and request cancellation/metric cleanup under slow export.
- `git diff --check main` passed. Full tracked/untracked inventory was reviewed;
  `git diff --exit-code main -- spec/server-business-logic.md spec/openapi.yaml
  migrations/0001_initial.sql Cargo.lock` passed. No Part 10 implementation
  change alters schema v2 or the business/OpenAPI contracts.
- Acceptance contributions are mapped to concrete evidence for scenarios
  **25, 29–31, 36, and 50** in `acceptance-map.md`. The independent release audit,
  rollout/proxy checks, packaging, and release-wide OpenAPI verification remain
  Part 11 and are not claimed here.

No required Part 10 check remained unexecuted at this handoff. Part 10 is
complete and merged. Part 11 was the next part at that time; its independent
release audit is now recorded in the Part 11 handoff below.

## Part 11 handoff — 2026-10-29

Status: **Complete**. The Part 11 implementation and required local verification
passed on `implementation/11-release-verification`, based on the verified Part
10 merge `71d77b3f3838081add13d46cdefe0a91cdd70efd`. The verified implementation
tip is `95d053d70c47c7199cfbd45545fa8c08b8d945d1`
(`fix(proxy): align documented proxy fixture version`); its preceding feature
commit is `c548e47ff50175a5111f1c4195b4ef24a0980799`. Handoff-only branch tip
`4226cd63298a7599e5866ae545845a590e32616f` (`docs(part-11): record verified
release handoff`) was integrated by the explicit local no-ff merge into `main`.
The merge parents are `71d77b3f3838081add13d46cdefe0a91cdd70efd` and
`4226cd63298a7599e5866ae545845a590e32616f`. The merge tree differs from the
reviewed part tip only in this Part 11 completion metadata. Its merge hash is
reported from Git after creation rather than embedded in its own tree.

### Delivered verification and operator material

- Added real A/B/C production HTTP coverage for shared account/IP create
  allowances, durable charges, stale-key mutation revalidation, A/B analytics
  workers, queued-event deletion, graceful readiness withdrawal, and replacement
  against the same isolated PostgreSQL schema.
- Added pinned whole-document OpenAPI 3.1 validation, all local references and
  JSON Schema 2020-12 roots, a reviewed **22-entry** method/path/operationId
  inventory, and production executable TCP probes. Existing business and
  `spec/openapi.yaml` contracts remain normative and unchanged.
- Added a pinned NGINX 1.30.6 TCP fixture with a separate static root, verified
  raw-path/body/header behavior, observed proxy trust, and real maximum-header
  cursor continuation. The macOS host-gateway probe observed `127.0.0.1`, which
  is explicitly not trusted; the isolated network observed NGINX/backend peer
  `10.89.0.4`, direct untrusted peer `10.89.0.3`, and NGINX client `10.89.0.6`.
- Added a locked Linux/arm64 Bookworm release build, normalized archive
  packaging, internal checksums/provenance, and packaged-binary migrate,
  bootstrap, HTTP, rotation, deletion, and no-reseed restart smoke. Operator and
  proxy setup, limits, trust, migration, backup, TLS/CA, and verification scope
  are documented in `docs/release.md`.
- `spec/server-business-logic.md`, `spec/openapi.yaml`, migrations, and
  `Cargo.lock` were not changed. No remote, push, external publication, or
  deployment was performed.

### Verification evidence

- Environment: macOS host, Podman 6.1.4, PostgreSQL 17.6, Rust/Cargo 1.99.1,
  NGINX 1.30.6, and the digest-pinned Debian Bookworm Rust 1.99.1 builder. The
  locally verified artifact target is `aarch64-unknown-linux-gnu`; no other
  target or host platform is claimed.
- `sh scripts/check.sh` — passed on the exact feature implementation that was
  committed as `c548e47ff50175a5111f1c4195b4ef24a0980799`, before the subsequent
  one-line proxy-version comment correction. The log reports **224 all-target
  tests plus 3 doctests passed**, with zero failures and zero ignored tests; it
  ran fmt, locked check, warning-denied Clippy, all-target tests, doctests, and
  the packaged release smoke. The correction changes no executable code or
  test behavior.
- `sh scripts/check-openapi.sh` — passed on verified implementation
  `95d053d70c47c7199cfbd45545fa8c08b8d945d1` (3 tests; 0 failed/ignored).
- `sh scripts/check-proxy.sh` — passed on clean source at that same revision
  (1 production proxy test; 0 failed/ignored). It rebuilt and verified the
  17-file Linux/arm64 archive with `source_tree_clean=true`.
- `sh scripts/check-release.sh` — passed on clean source at that revision
  (5 tests: 3 OpenAPI, 1 proxy, 1 packaged release smoke; 0 failed/ignored).
  The artifact is
  `target/release-packages/ferl-server-0.1.0-aarch64-unknown-linux-gnu.tar.gz`,
  SHA-256 `07d7641e88e852d6891b459c7dbd18f7d0a1db7f0b23850418d7b22718ab5d17`.
  Its verified provenance records source `95d053d70c47c7199cfbd45545fa8c08b8d945d1`,
  `source_tree_clean=true`, target `aarch64-unknown-linux-gnu`, 17 inventoried
  files, OpenAPI SHA-256
  `45677d0d0c0afda4687fada791ce492682ad546f2fbc612113890cffecbb6b5e`, and
  binary SHA-256
  `76fb42bffd1e224c616b87ca19b22ec52201e9fe101d4a7954a247bab3c32919`.
- All 51 scenarios have exact executable evidence in `acceptance-map.md`; the
  corresponding full-suite and dedicated-gate results are recorded in
  `docs/release-verification.md`. Harness-owned proxy/server/network resources
  were removed; the pre-existing PostgreSQL fixture remains running.
- Hosted CI was not run. Rust 1.94 compilation and an end-to-end TLS handshake
  were not run and are not claimed. Neither is a blocker to the locally
  specified Part 11 gate; no platform or hosted result is inferred.

No required local Part 11 check remains unexecuted. The explicit local no-ff
merge is complete; the implementation branch is retained, no remote is
configured, and no push or external publication occurred. The merge commit hash
is reported from Git after creation rather than embedded in its own tree.

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
