# ferl: repository publication and CI/CD agent handoff

> Historical planning snapshot from 2026-11-02, retained with the original
> handoff. Publication and container delivery have since been implemented in
> the GitHub history. For current workflow behavior and image instructions, use
> [CI/CD and container delivery](cicd.md) and the [README](../readme.md).
> The phase authorizations, no-build constraint and unpublished-state observations
> below describe that earlier assignment, not the current repository state.

## 1. Assignment and execution boundary

Act as the CI/CD engineer responsible for preparing ferl for public GitHub
publication and automated, versioned container delivery. Read this entire
handoff and inspect the current repository before making implementation changes.

The owner originally requested a privacy audit, cleanup, publication, container
CI, release automation, and deployment documentation. After clarification, the
owner selected **audit and plan only** and explicitly said **do not build
anything yet**. This document records that audit and the implementation plan.
Creating this handoff does not authorize implementation or remote operations.

Use the owner's next instruction to determine which implementation phases are
authorized. Until then, stay in planning mode. Even if subsequently asked to
implement files or publish the repository, keep application builds, container
builds, build-producing tests, hosted CI execution, and release-tag pushes
disabled until the owner explicitly lifts the no-build constraint.

Do not assume missing requirements. Ask about unresolved owner decisions while
continuing independent authorized work. Preserve unrelated changes and existing
documents. Treat examples in this file as future instructions, not commands to
execute while reading it.

## 2. Confirmed owner decisions

| Area | Confirmed decision |
| --- | --- |
| GitHub destination | `https://github.com/ai-slop-code/ferl` |
| Repository visibility | Public |
| Registry/image | `ghcr.io/ai-slop-code/ferl` |
| Image visibility | Public; anonymous pulls must work |
| Container platform | `linux/amd64`, the OCI platform corresponding to x86_64 |
| Rust release target | `x86_64-unknown-linux-gnu` |
| Container tooling | Docker-compatible OCI image, built and runnable with Podman |
| Release event | Push of a `vX.Y.Z` Git tag; publishing a GitHub Release is not required |
| Database instructions | External PostgreSQL 17.x |
| History treatment | Preserve and sanitize history, including the compatibility baseline |
| Local-reference treatment | Remove identifying machine details and execution diaries; retain portable localhost examples, container networking, and synthetic test fixtures |
| Public Git identity | GitHub login and ID-based no-reply email |
| Resolved public identity | `LubosBa <30705301+LubosBa@users.noreply.github.com>` |
| Current execution scope | Audit/planning and writing this handoff only |

The chosen no-reply identity keeps the GitHub account identifiable while removing
the original personal name/email from Git metadata. Do not replace it with an
invented organization identity.

The workflow structure, helper filenames, immutable versioning policy, signing,
and image-runtime design below are engineering recommendations implementing
these decisions. Validate their technical details against the current code and
upstream tool documentation.

## 3. Reference state and audit evidence

### 3.1 Reference snapshot

The audit was performed on 2026-11-02 at repository revision:

```text
0d4cdcc499f02edaedef70ffb332012935a8af9e
```

At the audit, the current branch was `main`, no Git remote was configured, and
these user documents were untracked:

```text
docs/cli-api-handoff.md
readme.md
```

This handoff is an additional document created after that audit. Recheck the
current status; neither a clean index nor an old file list proves the current
worktree is ready to publish. A clone or `git archive` alone omits untracked
documents.

Read-only GitHub checks observed:

- The `ai-slop-code/ferl` repository endpoint returned HTTP 404.
- The authenticated account had active administrator membership in
  `ai-slop-code`.
- The organization allowed members to create public repositories.

These are observations, not a guarantee that the destination or permissions are
unchanged. Recheck before any authorized creation operation. Do not overwrite an
existing repository if the destination has since been created.

### 3.2 Audit coverage and result

The audit inspected:

- **206 tracked and untracked files**.
- **60 commits and 540 unique file blobs** across all reachable Git history.
- Git author/committer metadata, configuration, dependency sources, deployment
  scripts, and the existing GitHub Actions workflow.
- Provider-token/private-key patterns, credential URLs, secret assignments,
  high-entropy candidates, personal paths, and machine-specific context.

Gitleaks **8.30.1** was downloaded temporarily from its official release and
checked against the published checksum. It scanned reachable history and a
snapshot of the tracked/untracked publishable files.

**No confirmed private token or password was found.** Gitleaks reported four
findings in the working files and four historical findings. Every finding was
an OpenAPI SHA-256 checksum, verified against the corresponding current or
historical `spec/openapi.yaml` bytes.

Current false-positive locations at that snapshot:

- `spec/implementation/progress.md:242` and `:395`.
- `docs/release-verification.md:163` and `:252`.

The inspected database-password examples and fixed cursor keys are synthetic
test fixtures. Retain those portable fixtures under the owner's chosen scope.
Do not interpret that decision as permission to allowlist arbitrary credentials.

Ignored artifacts were inventoried, not exhaustively content-scanned. Ignored
directories contained approximately **115 GB** of outputs, including compiled
files, logs, and build evidence. They are outside the publishable-source scan
and must be excluded from Git publication and container contexts.

The audit result is bounded to its reference state. Rerun scans against the
actual publication candidate after changes; do not claim the entire working
directory or future artifacts have been certified secret-free.

### 3.3 Cleanup findings

Line numbers below refer to the audit snapshot and can move.

| Finding | Location | Required treatment |
| --- | --- | --- |
| Personal repository and temporary-directory paths | `spec/implementation/progress.md`; `docs/release-verification.md:318` and `:336` | Remove identifying paths throughout publishable history. |
| Personal name and non-GitHub email | Author/committer metadata and `spec/implementation/progress.md` | Rewrite using the approved GitHub no-reply identity. |
| Workstation details, execution diaries, and proxy-specific workarounds | Progress/verification documents and portions of deployment documentation | Preserve useful technical requirements; remove identifying execution detail. |
| Disabled registry TLS verification | `scripts/postgres.sh:107`, `scripts/collector.sh:59`, `scripts/check-tls.sh:16–19` | Restore verified pulls and correct accompanying documentation. |
| Coding-harness-specific temporary-directory name | `tests/legacy_compatibility.rs:30` | Use a project-specific temporary-directory prefix. |
| Build jobs triggered on every push | `.github/workflows/ci.yml:3–5` | Disable Actions before initial publication and gate every build-producing job. |

Keep generic references such as `127.0.0.1`, portable `$HOME` configuration
paths, test-only keys, and Podman's `host.containers.internal` where needed.
These are different from recorded personal paths and workstation observations.
Do not reproduce original personal paths or private email addresses in new
public audit reports, commit messages, or this handoff.

## 4. Repository-specific facts to preserve

Read these files before implementing:

- `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml`, and `build.rs`.
- `.github/workflows/ci.yml`.
- `scripts/build-linux.sh`, `scripts/package-release.sh`,
  `scripts/package_release.py`, `scripts/prepare-release-package.sh`,
  `scripts/verify-release.py`, and `scripts/verify_source_snapshot.py`.
- `scripts/check.sh`, `scripts/check-msrv.sh`, `scripts/check-openapi.sh`,
  `scripts/check-proxy.sh`, `scripts/check-release.sh`, and
  `scripts/check-tls.sh`.
- `tests/legacy_compatibility.rs`, `tests/release_smoke.rs`,
  `tests/release_tls.rs`, and `tests/support/podman_server.rs`.
- `readme.md`, `docs/configuration.md`, `docs/database.md`,
  `docs/development.md`, `docs/release.md`, and `docs/operations.md`.
- `spec/openapi.yaml` and `spec/server-business-logic.md` for API contracts.

Reference-state facts:

- Binary/package name: `ferl-server`; package version: `0.1.0`.
- Ordinary pinned Rust toolchain: **1.99.1**; declared MSRV: **1.94**.
- CI checks the MSRV using **1.94.0**.
- PostgreSQL major **17** is required; current application schema version is
  exactly **2**.
- Migrations are embedded in the executable. `migrate`, `bootstrap-admin`, and
  `serve` are separate production commands.
- `migrate` and `bootstrap-admin` require database configuration but do not
  require serving-only origin/cursor settings.
- `serve` requires `FERL_DATABASE_URL`, `FERL_PUBLIC_BASE_URL`, and
  `FERL_CURSOR_KEY`.
- The application reads process environment variables and does not load `.env`
  files automatically.
- Health endpoints are `/api/health/live` and `/api/health/ready`; `/` returns
  404 in the backend.
- The default total shutdown deadline is 45 seconds.
- The existing release artifact is an archive, not a published container image.
- Existing packaging verifies the committed source snapshot, archive inventory,
  migration bytes, checksums, ELF target, and build receipt.
- Existing containerized release tests execute the packaged binary inside the
  pinned Rust builder image. They do not yet establish acceptance of a smaller
  production runtime image.
- Historical documents describe local ARM64 acceptance. Native Linux/amd64
  hosted CI acceptance has not been established by this audit.

Do not silently change business behavior, migrations, API contracts, dependency
versions, toolchains, or supported platforms to make a new workflow pass. Report
an actual incompatibility and propose a focused correction.

## 5. Phase A: sanitize a publication copy

### 5.1 Preserve the original and include intended work

Once implementation is authorized:

1. Inspect current status, refs, remotes, worktrees, and repository instructions.
2. Preserve the original repository privately outside the publication copy.
3. Prepare a separate publication copy, accounting for tracked and untracked
   intended documents without copying ignored build/data directories.
4. Review `readme.md`, `docs/cli-api-handoff.md`, and this handoff for inclusion.
5. Keep private audit reports, replacement rules, backup objects, and original
   history outside the public tree.

### 5.2 Clean current files

- Replace personal paths with portable variables or project-relative examples.
- Remove personal identities embedded in documentation.
- Convert execution diaries into reusable verification requirements and concise
  status summaries.
- Preserve accurate supported-version and architecture information.
- Restore normal TLS verification for all registry pulls and correct related
  text in development, release, and operations documentation.
- Replace the compatibility test's coding-harness-specific scratch-directory
  name with a project-specific prefix.
- Preserve the test harness's ownership labels, locking, bounded operations,
  and cleanup rules.
- Retain `.env.example` as clearly synthetic development configuration.
- Add container-context exclusions and construct the release-image context
  from an explicit allowlist.

Use standard container CA trust configuration when a deployment needs private
trust roots; do not retain a default TLS bypass as a workstation workaround.

### 5.3 Rewrite publishable history

Use `git-filter-repo` in the publication copy to sanitize:

- File contents in all intended reachable history.
- Author and committer names/emails.
- Identifying commit messages.
- Relevant tag annotations or other refs that will be published.

Use the approved identity:

```text
LubosBa <30705301+LubosBa@users.noreply.github.com>
```

A `.mailmap` alone does not erase original metadata. Configure the publication
copy's future commits to use the approved identity too; do not change global Git
configuration. Preserve the filter's old-to-new commit map privately.

### 5.4 Repair the historical compatibility dependency

The current test pins the authentic schema-v1 baseline:

```text
tests/legacy_compatibility.rs:18
PART_07_BASE = 7b19cfbd886efb8539b6cc2a8eed6bcc42e78922
```

History rewriting changes this ID. After rewriting:

1. Locate the replacement baseline ID in the commit map.
2. Inspect its source and verify that it remains the authentic v1 implementation.
3. Update the current `PART_07_BASE` constant.
4. Update meaningful references in operator/test documentation, including any
   retained references in this handoff and the CLI handoff's source snapshot.
5. Verify the replacement baseline is reachable from the published branch.
6. Retain full-history checkouts in CI.

Do not remove, skip, or fabricate the legacy-binary test to avoid preserving its
dependency. Do not relabel old artifact hashes or execution claims as evidence
for rewritten source. Retain original evidence privately where useful and
publish fresh acceptance evidence after builds are authorized.

### 5.5 Repeat the publication audit

Before pushing, scan the actual candidate, including newly added files:

- Gitleaks against intended reachable history and tracked/untracked publication
  content.
- Targeted personal-path, email/identity, private-endpoint, and TLS-bypass checks.
- Git metadata, commit messages, intended refs, and the exact tracked inventory.
- Reachability of the rewritten historical compatibility commit.

Inspect findings without printing secret values. If a genuine credential is
found, report its location/category, arrange revocation or rotation with the
owner, and remove it from publishable history. Do not test credentials against
external services to see whether they work.

For verified false positives, use narrowly scoped exceptions with an explanation.
Do not exclude whole documentation or test directories from secret scanning.

## 6. Phase B: publish with builds disabled

The current workflow builds on every push. Prevent an accidental first-push
build through both repository-level disabling and job-level activation gates.

Add repository-variable gates:

```text
FERL_CI_ENABLED
FERL_RELEASE_ENABLED
```

Require the exact string `true`. Missing variables mean disabled. All
build-producing CI jobs require `FERL_CI_ENABLED == 'true'`; release execution
additionally requires `FERL_RELEASE_ENABLED == 'true'`. Apply these gates to
existing jobs and reusable-workflow entry points, not only new image jobs.

Future authorized publication sequence:

1. Commit sanitized content and disabled-by-default workflow gates.
2. Recheck the destination and authenticated organization permissions.
3. Create the empty public repository without pushing.
4. Disable repository Actions before pushing any workflow-containing commit.
5. Push the intended sanitized `main` branch.
6. Configure repository protections and leave build gates disabled.

Illustrative commands; `$SANITIZED_CHECKOUT` must identify the reviewed copy:

```sh
gh repo create ai-slop-code/ferl \
  --public \
  --source "$SANITIZED_CHECKOUT" \
  --remote origin

gh api --method PUT \
  repos/ai-slop-code/ferl/actions/permissions \
  -F enabled=false

git -C "$SANITIZED_CHECKOUT" push --set-upstream origin main
```

Do not add `--push` to repository creation. Verify the disable operation
succeeded before pushing. Do not use `--mirror`, publish backup refs, or
indiscriminately push every local branch/tag.

Configure protected `main`, required checks once their names are established,
and release-tag rules restricting unauthorized creation, modification, and
deletion. Initial publication must not create a release tag or enable builds.

## 7. Phase C: construct the container delivery path

### 7.1 Reuse the verified release machinery

Extend the existing release scripts rather than introducing a second unrelated
Rust release build. The intended flow is:

```text
Committed source
    -> pinned Bookworm Rust builder
    -> verified amd64 release archive
    -> minimal OCI runtime image
    -> runtime-image acceptance tests
    -> publication of that tested image
```

On the native x86_64 Linux runner, explicitly set:

```sh
FERL_RELEASE_TARGET=x86_64-unknown-linux-gnu
```

The lower-level `scripts/build-linux.sh` defaults to ARM64 when no target is
provided. Always set the compilation target and image platform explicitly.

At implementation, verify that pinned builders/fixtures remain available and
contain required native build prerequisites. Do not silently replace a missing
digest, skip a fixture, or claim a build based solely on inspecting metadata.

### 7.2 Runtime-image contract

Add a `Dockerfile` compatible with Podman that provides:

- A digest-pinned glibc runtime compatible with the Bookworm-built executable.
- The required runtime shared libraries and CA certificates.
- Only the verified executable and explicitly approved runtime assets.
- Numeric non-root identity `65532:65532`.
- `FERL_LISTEN_ADDR=0.0.0.0:8080` and exposed port 8080.
- Exec-form process launch:

  ```dockerfile
  ENTRYPOINT ["/usr/local/bin/ferl-server"]
  CMD ["serve"]
  ```

- SIGTERM shutdown, without a wrapper that swallows signals.
- OCI source, version, and revision labels. The source label must be
  `https://github.com/ai-slop-code/ferl`.

Resolve and verify a suitable runtime digest during implementation. Inspect
actual library requirements and test the resulting image. Existing acceptance
inside the Rust builder does not prove a smaller runtime is sufficient.

Build an OCI image explicitly for `linux/amd64`. Do not infer cross-architecture
support from a successful ARM64 run or QEMU execution. Native amd64 acceptance
is required for this delivery target.

### 7.3 Build context and secrets

Add `.dockerignore` and generate a minimal image context from verified package
inputs, for example a Dockerfile plus `bin/ferl-server`. If the context is staged
under ignored `target/`, ensure the intended ignore rules apply to that context.

Do not send the repository root's ignored files to the builder or use
`COPY . .`. Exclude `.git`, environment files, private configuration, database
data, logs, local artifacts, caches, and verification captures.

Keep credentials out of image layers, build arguments, labels, receipts, and
logs. Generate binaries from the clean committed source snapshot, not a
pre-existing workstation binary. Preserve source/archive/checksum verification.

## 8. Phase D: GitHub Actions implementation

### 8.1 Proposed files

| File | Responsibility |
| --- | --- |
| `.github/workflows/ci.yml` | Pull-request/main triggers and activation gate |
| `.github/workflows/verify.yml` | Reusable verification and candidate-image construction |
| `.github/workflows/release.yml` | Tag validation and publication orchestration |
| `Dockerfile` | Runtime-image definition |
| `.dockerignore` | Context exclusions |
| `scripts/build-image.sh` | Verify the release archive and assemble the OCI image |
| `scripts/check-image.sh` | Exercise the actual runtime image |
| `docs/cicd.md` | Activation, pipeline operation, release, and recovery guide |
| `readme.md` | Installation and PostgreSQL/migration/bootstrap/startup instructions |

Keep script names/style consistent with the repository. This file is the agent
assignment; `docs/cicd.md` should become the maintained operator/developer guide.

### 8.2 Event and permission model

| Event, after activation | Verification | Candidate image | GHCR publication |
| --- | --- | --- | --- |
| Pull request | Yes | Build and test | No |
| Push to `main` | Yes | Build and test | No |
| Valid version-tag push | Yes, for tagged commit | Build and test | Yes |
| Required activation variables unset | No builds | No builds | No publication |

Use ordinary `pull_request` for untrusted contributions, with read-only
permissions. Do not execute pull-request code in a privileged
`pull_request_target` or publication job. Use native GitHub-hosted x86_64 Linux
runners; do not expose a persistent deployment host to contribution jobs.

Release-trigger skeleton, to be completed with validation and activation gates:

```yaml
on:
  push:
    tags:
      - "v*"

permissions:
  contents: read

concurrency:
  group: release-${{ github.ref }}
  cancel-in-progress: false
```

The glob is preliminary filtering, not SemVer validation. Give PR/main workflows
appropriate per-ref concurrency so obsolete verification can be cancelled;
release publication must not be cancelled midway by a newer run of that tag.

### 8.3 Release preflight, before compilation

Require all of the following:

1. The expected repository, `ai-slop-code/ferl`.
2. Both activation variables enabled.
3. A stable SemVer tag such as `v0.1.0`, with three numeric components and no
   leading zeroes other than zero itself.
4. A checkout of the exact tagged commit, correctly resolving annotated tags.
5. The tagged commit reachable from protected `main`.
6. A clean tracked/untracked source tree.
7. The rewritten compatibility baseline reachable in the checkout.
8. Agreement between the tag without `v`, `Cargo.toml`, and the root package
   entry in `Cargo.lock`.

Later verify that the executable's `--version` agrees too. The package version,
OpenAPI version, and `/api/v1` API prefix are distinct; do not change the API
version merely to release a package.

The current package version is `0.1.0`. A later version tag requires a committed
package/lockfile update first. Supporting prerelease tags or additional mutable
aliases would be a separate policy decision; ask rather than silently adding it.

Pass event values through environment variables and quote them. Do not splice
unvalidated tag or branch text into shell source. Fetch the refs needed to check
main ancestry, rather than assuming a detached checkout contains `origin/main`.

### 8.4 Verification jobs

Use native **x86_64 Ubuntu 24.04** runners and retain existing coverage:

- Actual Rust 1.94.0 MSRV check and release compilation.
- Pinned Rust 1.99.1 ordinary toolchain, rustfmt, and Clippy.
- Locked dependency resolution/builds.
- Formatting and warning-denied Clippy.
- Unit, integration, and documentation tests.
- PostgreSQL migrations and authentic legacy-binary compatibility.
- Whole-document OpenAPI checks.
- Proxy, Collector, lifecycle, release-package, and TLS verification.

The existing entry points are `scripts/check-msrv.sh`, `scripts/check.sh`,
`scripts/check-openapi.sh`, `scripts/check-proxy.sh`,
`scripts/check-release.sh`, and `scripts/check-tls.sh`. These commands build
code. Do not run them while the owner's no-build constraint remains active.

The existing tests use label-verified Podman fixtures and deliberately restart
their PostgreSQL container. Preserve this harness instead of replacing it with
a generic Actions service container. Do not point tests at the operator's
external application database.

Implementation requirements:

- Pin actions to reviewed full commit SHAs.
- Set checkout `fetch-depth: 0` and `persist-credentials: false`.
- Verify host architecture, Podman, and required native build tools.
- Pull digest-pinned fixture images with normal TLS verification.
- Keep output in ignored, run-specific directories so packaging's clean-tree
  checks remain meaningful.
- Preserve process/interprocess fixture locks and owned-resource cleanup.
- Set bounded job/process timeouts and collect sanitized failure diagnostics.
- Give verification jobs no registry-write credentials or signing permissions.
- If caching is added, key it by OS/architecture/toolchain/lockfile; do not treat
  an old cached binary as the candidate artifact or import untrusted cache state
  into a privileged publisher.

The current scripts perform repeated package builds. Preserve acceptance
coverage initially. A later reuse optimization must retain source, checksum,
build-receipt, and clean-snapshot validation and bind tests to the selected
candidate. Never publish a different executable merely because another build
of the same commit passed tests.

### 8.5 Acceptance of the actual runtime image

Add image-specific verification. The current builder-image/binary-mount tests
remain useful, but do not replace testing the image users will pull.

The candidate-image checks must:

1. Confirm `linux/amd64` image metadata and x86_64 ELF architecture.
2. Run `--version` and compare it to the release metadata.
3. Migrate a fresh isolated PostgreSQL 17 database.
4. Bootstrap an administrator and capture the returned key privately.
5. Start the server as the image's non-root user with the intended read-only
   filesystem and capability restrictions.
6. Wait with a bounded deadline for `/api/health/ready`.
7. Create a link and verify redirect behavior through the real server.
8. Restart against the same database and verify persistence without reseeding.
9. Verify database certificate-chain and hostname checks, including negatives.
10. Verify graceful SIGTERM exit within the configured shutdown budget.

Use disposable test infrastructure and synthetic credentials. Bootstrap output
must not become a public log or uploaded artifact. Generate an SBOM and scan the
candidate image for secrets and vulnerabilities before publication. Define and
document the vulnerability failure policy; exceptions must be scoped and
explained rather than blanket ignores.

### 8.6 Candidate handoff and isolated publication

The reusable verification workflow should produce an OCI archive, its checksum,
version/source metadata, and evidence identifying the accepted candidate. Use
run-specific artifact names. Download candidates only from the same trusted
workflow run, never by an ambiguous name across unrelated runs.

On a fresh publication runner, after all required jobs succeed:

1. Download that run's accepted candidate.
2. Check the archive checksum and expected source/version/architecture metadata.
3. Load the tested image without rebuilding or running repository-supplied
   application code in the privileged publication job.
4. Authenticate to GHCR using the repository's `GITHUB_TOKEN`.
5. Push the accepted image and capture the registry manifest digest.
6. Attach signed provenance and the SBOM to that registry digest.
7. Report the exact immutable pull reference and verification evidence.

Account for the distinction between a local image ID and the registry manifest
digest; do not substitute one for the other in release instructions.

Publication/signing permission example:

```yaml
permissions:
  contents: read
  packages: write
  id-token: write
```

Add `attestations: write` if using GitHub artifact attestations. Keep OIDC
permissions in the signing/publication scope. Prefer short-lived workflow
credentials over a stored personal access token.

Set Podman's registry auth-file path under `$RUNNER_TEMP`, authenticate with
`--password-stdin`, and remove authentication state in cleanup. Do not put auth
files in the checkout or upload them. Record source revision, verified builder
and runtime digests, artifact checksums, and the final image digest in provenance.

### 8.7 Version tags, retries, and GHCR visibility

Recommended image references:

```text
ghcr.io/ai-slop-code/ferl:v0.1.0
ghcr.io/ai-slop-code/ferl:sha-<full-commit-id>
ghcr.io/ai-slop-code/ferl@sha256:<manifest-digest>
```

Treat version tags as immutable through publisher policy:

- Reject overwriting an existing version with different content.
- Resume an interrupted publication only from the verified candidate and
  recorded digest, not an unverified rebuild.
- Protect Git release tags against unauthorized changes and deletion.
- Preserve sufficient candidate/evidence retention for bounded recovery and
  document behavior when that retention has expired.

GHCR initially creates packages as private independently of source-repository
visibility. Use the source label and workflow publication to associate the
package with the repository and verify Actions access to it.

After the first approved publication, explicitly change the package to public.
This is a one-time package configuration step, possible after the package
exists. Verify a digest pull using an empty registry-auth configuration so a
locally authenticated success is not mistaken for anonymous accessibility.

Do not declare first-release distribution complete until public pulls succeed.
Subsequent approved version-tag pushes should publish automatically without
requiring a manually published GitHub Release.

## 9. Phase E: README deployment instructions

Replace the existing planned-image section with accurate instructions matching
the implemented image. Keep unpublished image availability clearly marked until
the first release exists. Link to `docs/cicd.md`, existing database/configuration
references, and release operations. Update any affected README anchors.

The examples below specify the intended runtime contract and must be verified
after builds/runtime tests are authorized. They contain deployment placeholders,
not actual infrastructure settings.

### 9.1 Requirements and database model

State explicitly:

- A native x86_64 Linux host with Podman is the delivery target.
- PostgreSQL **17.x**, its client tools, Python 3, and curl are needed for these
  setup examples.
- Use the PostgreSQL primary/read-write endpoint.
- `psql` provisions the role/database; `ferl-server migrate` applies embedded
  migrations; `bootstrap-admin` is a separate one-time operation.
- The example uses one dedicated non-superuser application login that owns its
  database and can perform migrations. Do not invent an unverified split-role
  permission model.

### 9.2 Create the initial PostgreSQL role and database

Connect using an administrative account permitted to create the dedicated role
and database. Replace the hostname, administrative login, and CA path:

```sh
psql \
  "host=db.example.com port=5432 dbname=postgres user=DB_ADMIN sslmode=verify-full sslrootcert=/path/to/db-ca.pem" \
  -X -W -v ON_ERROR_STOP=1
```

First inspect the version and proceed only on PostgreSQL 17:

```sql
SHOW server_version;
```

For a new deployment, run inside `psql`:

```sql
CREATE ROLE ferl
  LOGIN
  NOSUPERUSER
  NOCREATEDB
  NOCREATEROLE
  NOREPLICATION;

SET password_encryption = 'scram-sha-256';
\password ferl

CREATE DATABASE ferl OWNER ferl;

\connect ferl

REVOKE ALL ON DATABASE ferl FROM PUBLIC;
REVOKE CREATE ON SCHEMA public FROM PUBLIC;

GRANT USAGE, CREATE ON SCHEMA public TO ferl;

ALTER ROLE ferl IN DATABASE ferl
  SET search_path = public;
```

`\password` prompts rather than embedding the password in the SQL example. These
are initialization commands, not instructions to overwrite existing roles or
databases. For a managed database, use equivalent provider-supported operations
with the same ownership/search-path requirements.

Document prerequisites:

- DNS resolution and database access from the application container.
- PostgreSQL TLS and a certificate matching the selected hostname.
- SCRAM authentication and the appropriate connection rule for the app role.
- Network access from the application host.
- A CA certificate readable inside the application container.

Use the same database/schema/search path for migration, bootstrap, and serving.
No PostgreSQL extension installation is required by the current application.

### 9.3 Create private runtime configuration

```sh
umask 077
mkdir -p "$HOME/.config/ferl"
touch "$HOME/.config/ferl/ferl.env"
chmod 600 "$HOME/.config/ferl/ferl.env"
```

The Podman environment file uses plain `KEY=value` entries without shell quotes
or `export`:

```dotenv
FERL_DATABASE_URL=postgres://ferl:URL_ENCODED_PASSWORD@db.example.com:5432/ferl?sslmode=verify-full&sslrootcert=/etc/ferl/db-ca.pem
FERL_PUBLIC_BASE_URL=https://s.example.com
FERL_LISTEN_ADDR=0.0.0.0:8080
FERL_CURSOR_KEY=REPLACE_WITH_GENERATED_KEY
FERL_ANONYMOUS_CREATION=false
FERL_ENVIRONMENT=production
FERL_SHUTDOWN_TIMEOUT_SECS=45
```

Percent-encode special characters in database URL credentials. The URL's CA path
is a path inside the container. Do not source this unquoted Podman env-file
example as a shell script; Podman supplies it to the application environment.

Generate a cursor key once per deployment:

```sh
python3 -c 'import base64,secrets; print(base64.urlsafe_b64encode(secrets.token_bytes(32)).rstrip(b"=").decode())'
```

Save it privately in the environment file, reuse it across restarts and
instances, and back it up separately from PostgreSQL. Changing it invalidates
existing pagination cursors. Do not generate it automatically on every start.

Place the trusted database CA at `$HOME/.config/ferl/db-ca.pem` and ensure the
container's non-root process can read the mounted certificate. The application
does not automatically load `.env` files; `--env-file` is Podman's mechanism.

### 9.4 Pull and migrate

Use the registry digest reported by the release workflow:

```sh
FERL_CONFIG="$HOME/.config/ferl"
FERL_IMAGE='ghcr.io/ai-slop-code/ferl@sha256:REPLACE_WITH_PUBLISHED_DIGEST'

podman pull --platform linux/amd64 "$FERL_IMAGE"
```

Define a one-shot helper for the same shell session:

```sh
ferl_once() {
  podman run --rm \
    --platform linux/amd64 \
    --pull=never \
    --log-driver=none \
    --read-only \
    --cap-drop=all \
    --security-opt=no-new-privileges \
    --env-file "$FERL_CONFIG/ferl.env" \
    --volume "$FERL_CONFIG/db-ca.pem:/etc/ferl/db-ca.pem:ro" \
    "$FERL_IMAGE" "$@"
}

ferl_once migrate
```

Migration must succeed before bootstrap or startup. Do not manually apply the
shipped SQL instead of the embedded migration command or edit SQLx migration
history to force readiness.

### 9.5 Bootstrap the initial administrator once

After migration succeeds:

```sh
(
  umask 077
  set -C
  ferl_once bootstrap-admin --username ferl-admin \
    > "$FERL_CONFIG/bootstrap.json"
)
```

The helper disables container logging so the returned API key is not also
retained by the container log driver. Do not allocate a TTY or upload the
bootstrap output. `set -C` prevents overwriting an existing output file.

Verify success and retain the returned key in the operator's secret manager.
Bootstrap completion is stored in PostgreSQL. Repeating the command does not
recover a lost key or create a replacement administrator. Follow the documented
recovery procedure if output fails after commit; do not automate retries or a
database reset.

### 9.6 Start the server and check readiness

For a reverse proxy running on the same host:

```sh
podman run --detach \
  --name ferl \
  --platform linux/amd64 \
  --pull=never \
  --read-only \
  --cap-drop=all \
  --security-opt=no-new-privileges \
  --stop-timeout=60 \
  --publish 127.0.0.1:8080:8080 \
  --env-file "$FERL_CONFIG/ferl.env" \
  --volume "$FERL_CONFIG/db-ca.pem:/etc/ferl/db-ca.pem:ro" \
  "$FERL_IMAGE" serve
```

On SELinux hosts, document the appropriate shared read-only mount labeling for
the CA file, for example `:ro,z` when it is shared across application containers.
Validate the documented mount and non-root access in the target environment.

```sh
curl --fail-with-body http://127.0.0.1:8080/api/health/ready
```

Expected ready response:

```json
{"status":"ok"}
```

Readiness can initially return 503 while schema verification completes.
`/api/health/live` checks process responsiveness; `/api/health/ready` includes
database/schema readiness. The backend's `/` is not a health endpoint.

The README must also explain:

- `FERL_PUBLIC_BASE_URL` controls returned short URLs and does not configure DNS
  or terminate TLS.
- HTTPS terminates at the reverse proxy; ferl serves HTTP internally.
- Preserve exact API and short-link routing using the existing proxy guidance.
- Set trusted proxies from the actual topology; do not copy historical observed
  container IPs or trust an entire private network by default.
- The example's loopback port publishing is for a same-host proxy. Ask for the
  intended topology before adapting it to another host/network arrangement.
- Use a service manager for unattended hosting. A rootless Podman Quadlet on
  Linux should retain the same image/env/CA settings, appropriate restart
  behavior, and a stop allowance greater than the application's shutdown
  deadline. If lingering is required, document it for the selected service user.

## 10. Upgrade, rollback, and restore instructions

Document this operator sequence:

1. Record the currently deployed image digest.
2. Back up PostgreSQL and retain the cursor key separately.
3. Review version-specific migration compatibility.
4. Drain and stop old instances where the migration requires it.
5. Run `migrate` with the new image and require successful completion.
6. Start the new image against the same intended primary/schema.
7. Wait for readiness before restoring traffic.

The documented v1-to-v2 transition is not an overlapping rolling upgrade.
Old binaries reject the newer schema and new binaries require their expected
schema. An image rollback alone is not sufficient after an incompatible
migration: restore compatible database state or apply a deliberate forward fix.

Backups must preserve application data, credentials/digests, reserved codes,
bootstrap state, rate accounting, and migration history. Preserve the cursor key
separately. Verify restoration in an isolated database with a compatible image
before claiming restore acceptance. Do not configure CI to migrate or bootstrap
the owner's production database as part of image publication.

## 11. Validation and staged activation

### 11.1 Before builds are authorized

Perform only checks that do not compile or launch the application:

- Publication secret scans and targeted privacy checks.
- Git-history/metadata inspection and baseline reachability checks.
- File-inventory and clean-source review.
- Workflow linting, script syntax checks, and documentation consistency checks.
- Read-only verification of relevant GitHub configuration when authorized.

Do not run build-producing Cargo commands or the existing `check-*.sh` gates.
Even test-listing commands may compile; they are not a way around the no-build
constraint. Do not push a release tag to test whether a trigger works.

### 11.2 When the owner authorizes builds

1. Confirm implementation is committed in the sanitized repository.
2. Enable repository Actions while leaving release publication disabled.
3. Set `FERL_CI_ENABLED=true` and trigger the agreed verification event.
4. Obtain successful native amd64 CI and actual runtime-image acceptance.
5. Validate the README against a fresh isolated external PostgreSQL 17 database.
6. Record real run URLs, source revision, architecture, and artifact evidence.
7. Set `FERL_RELEASE_ENABLED=true` when release execution is authorized.
8. Push the first agreed version tag, with matching committed package version.
9. Complete the initial GHCR public-visibility configuration and anonymous pull
   verification.

Enabling a variable does not itself replay previously skipped workflows. Use a
supported event deliberately after authorization. Report any failed or skipped
gate accurately; historical local results are not substitutes for current CI.

### 11.3 Completion criteria

Publication-only completion requires:

- Sanitized intended files and reachable history.
- No confirmed secrets in the publication-candidate scan.
- Preserved and remapped legacy compatibility baseline.
- The intended public repository populated with the reviewed branch.
- Builds and releases still disabled when the no-build constraint is active.

Full CI/CD completion additionally requires:

- Successful native Linux/amd64 verification with the actual supported compiler
  versions and PostgreSQL fixtures.
- Successful tests of the image that is published.
- Consistent package/tag/executable versions and source provenance.
- GHCR publication by short-lived workflow credentials.
- Public anonymous pulls by the recorded registry digest.
- SBOM and signed provenance bound to that digest.
- Verified fresh-database, migration, bootstrap, startup, and restart docs.
- Documented activation, failed-publication recovery, upgrade, and rollback.

## 12. Unresolved inputs and final agent report

Ask the owner when an actual deployment or additional release policy requires:

- Database hostname, administrative login, CA location, and network access.
- Public origin and reverse-proxy topology.
- Deployment host/service-manager details.
- The first release version if different from the current package version.
- Prerelease or mutable-alias policies beyond the stable version scheme above.
- A license choice if adding license files or an OCI license label; do not
  invent a license from public visibility.

Runtime/base-image digests, action SHAs, shared-library requirements, and tooling
versions must be resolved from inspected dependencies and upstream sources at
implementation time, not fabricated. Document any technical blocker precisely.

At the end of each authorized phase, report:

1. Files changed and the purpose of each change.
2. Cleanup scope and history/baseline mapping outcome, without private values.
3. Checks actually run, their results, and important coverage limits.
4. Whether any builds, tests, pushes, or remote-setting changes occurred.
5. Repository/image/run URLs and digests only when they actually exist.
6. Remaining disabled phases, unresolved inputs, and the next required action.

Keep this handoff and maintained CI/CD documentation consistent with the final
implementation. Update reference-state claims after history rewriting without
presenting old execution evidence as new acceptance.

## 13. Upstream references

- [GitHub Container registry](https://docs.github.com/en/packages/working-with-a-github-packages-registry/working-with-the-container-registry)
- [GitHub Actions workflow syntax](https://docs.github.com/en/actions/reference/workflows-and-actions/workflow-syntax)
- [Podman run reference](https://docs.podman.io/en/latest/markdown/podman-run.1.html)
- [git-filter-repo](https://github.com/newren/git-filter-repo)
- [Gitleaks](https://github.com/gitleaks/gitleaks)

Use upstream documentation to validate exact tool and API behavior before
implementation. These references do not authorize running builds or changing
the confirmed owner decisions.
