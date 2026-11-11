# CI/CD and container delivery

This guide describes the GitHub Actions pipeline, the Podman-compatible
runtime image, and the version-tag release procedure for ferl. The pipeline has
run on hosted runners: verification passed on pull requests and `main`, and the
`v0.1.0` release published the image described in [Current state](#current-state).
The sections below describe how each path works and how to run it again.

## Current state

| Area | State |
| --- | --- |
| Repository | `https://github.com/ai-slop-code/ferl`, public, MIT licensed. |
| Actions | Enabled. Verification runs on pull requests and `main` pushes. |
| Build gates | `FERL_CI_ENABLED=true` and `FERL_RELEASE_ENABLED=true`. |
| Tags | `v0.1.0` is published from `main`. |
| Image | `ghcr.io/ai-slop-code/ferl@sha256:f75c9e9f7bc06e2007246b85691b8f6605a21655900a26921addb03e91ffeac9` (tags `v0.1.0`, `sha-c6ddec2…`). Public, with provenance and SBOM attestations. |
| Protection | `main` blocks force-pushes and deletion. Tags matching `v*` are protected by the `release-version-tags` ruleset. |

## Activation

Both repository variables are read with `vars.<NAME> == 'true'`. A missing
variable disables the jobs.

| Variable | Enables |
| --- | --- |
| `FERL_CI_ENABLED` | Verification and candidate-image jobs on pull requests and `main` pushes. Also required for any release. |
| `FERL_RELEASE_ENABLED` | Release preflight and the `vX.Y.Z` publication path. |

Activate in this order, and do not skip steps:

1. Enable repository Actions in **Settings → Actions**, or with the API
   (`gh api --method PUT repos/ai-slop-code/ferl/actions/permissions -F enabled=true`).
2. Set `FERL_CI_ENABLED=true` (`gh variable set FERL_CI_ENABLED --body true --repo ai-slop-code/ferl`).
   Leave `FERL_RELEASE_ENABLED` unset.
3. Trigger verification with a supported event, such as a pull request or a push
   to `main`. Enabling a variable does not replay skipped runs.
4. Record the run URLs, the source revision, and the candidate artifact name.
   Only a fully successful run counts as acceptance.
5. Only after step 4 is accepted and the README has been validated against a
   fresh external PostgreSQL 17 or 18 database, set `FERL_RELEASE_ENABLED=true`.
6. Push the first version tag (see [Release](#release)).

## Events and permissions

| Event | Verification and candidate | Publication |
| --- | --- | --- |
| Pull request | `ci.yml` → `verify.yml`, `contents: read` only. | None. |
| Push to `main` | Same as pull request. | None. |
| `vX.Y.Z` tag push | `release.yml`: preflight, then `verify.yml`, then `publish`. | Only `publish` has `packages`, `id-token`, and `attestations` write permissions. |
| Variables unset | No jobs run. | None. |

Pull requests use the ordinary `pull_request` event, so contributed code runs
without repository secrets and with read-only permissions. The workflows never
use `pull_request_target`. Verification runs on GitHub-hosted native
`ubuntu-24.04` x86_64 runners, not on a persistent host.

Every third-party action is pinned to a full commit SHA. The Syft and Grype
binaries are pinned to a release version, and their archive SHA-256 values are
checked before use. Trivy is deliberately not used: its binary and
`aquasecurity/trivy-action` tags were compromised in March 2026.

## Verification pipeline

`verify.yml` is a reusable workflow. It keeps the existing gates and adds the
candidate image:

- **`msrv`**: Rust 1.94.0 MSRV check and production release build.
- **`release-gate`**: pinned Rust 1.99.1, Podman, OpenAPI, the TCP proxy check,
  the full Rust/PostgreSQL/process/Collector gate, and the repeated packaged
  release smoke test. Images are pulled by digest with normal TLS verification.
- **`candidate-image`**: builds the release archive from the committed tree,
  builds the runtime image, runs the acceptance checks, creates an SBOM, applies
  the vulnerability policy, and uploads the candidate.

The candidate artifact is named `ferl-candidate-<run id>`, is retained for 14
days, and contains the OCI archive, its checksum, the image metadata, the SBOM,
and the acceptance summary. A re-run of the verification jobs overwrites it, so
publication always uses the newest candidate from that run.

### Runtime image

- `Dockerfile` uses `gcr.io/distroless/cc-debian12:nonroot`, pinned to the
  linux/amd64 manifest digest `sha256:777e96cf…ebbb`. Debian 12 matches the
  Bookworm builder. The base supplies glibc, the required runtime libraries, and
  CA certificates.
- The only application file is `/usr/local/bin/ferl-server`, copied from the
  verified release archive. Migrations are embedded, so no SQL files are shipped.
- The process runs as `65532:65532` with exec-form `ENTRYPOINT` and `CMD serve`,
  listens on `0.0.0.0:8080`, and handles SIGTERM.
- Labels record the source URL, version, and full revision.
- `.dockerignore` denies everything except `Dockerfile`, `.dockerignore`, and
  `bin/ferl-server`. The build stages its own context, so the repository root
  never becomes a build context.

### Acceptance checks (`scripts/check-image.sh`)

The checks run against the same OCI archive that will be published, after it is
loaded and its image ID is compared with the metadata:

1. The OCI archive matches its checksum and image metadata.
2. The image is `linux/amd64`, has the expected labels, and runs as `65532:65532`.
3. The packaged executable is an x86_64 ELF file whose digest equals the release
   executable.
4. `--version` prints `ferl-server <version>`.
5. A disposable PostgreSQL 17 fixture with TLS (a synthetic CA and server
   certificate) accepts `migrate`, and a repeated `migrate` succeeds.
6. Negative cases fail: an untrusted CA, and a hostname not in the certificate.
7. Diagnostic logs do not contain the database password.
8. `bootstrap-admin` returns one JSON document with a `ferl_ak_v1_` key, written
   to a private file. A repeated bootstrap must fail.
9. The server becomes ready on `/api/health/ready` under the read-only,
   capability-dropped, non-root configuration.
10. Creating a link returns `201`. Its redirect returns `302` with the exact
    destination.
11. `podman stop` produces exit code `0` within the shutdown budget.
12. A restarted server still serves the link, without reseeding.

Credentials are generated per run and never printed. The fixture containers,
network, and working files are removed on exit.

### Vulnerability and content policy

- **Vulnerabilities:** `grype --fail-on high --only-fixed` fails the candidate
  for any fixed High or Critical vulnerability. Unfixed findings are reported but
  do not fail the run.
- **Exceptions:** `.grype.yaml` at the repository root holds five scoped entries
  for CVEs in `libssl3` 3.0.20 in the distroless base image. Each entry names
  one vulnerability and one package version. `ferl-server` does not link OpenSSL,
  so these are base-image findings, not application code paths. Entries expire
  on **2026-12-02**: re-evaluate or remove them by then, and remove each entry
  as soon as the base image ships a fixed `libssl3`. Blanket ignores are not
  allowed.
- **Secrets:** `build-image.sh` scans the exported image filesystem for private
  key headers and common token formats. This is pattern matching, not a complete
  secret scanner, and it does not replace the repository scan before a release.

## Release

### Prerequisites

- `Cargo.toml` and the root `ferl-server` entry in `Cargo.lock` have the version
  you intend to release. For `v0.1.0`, that is the current `0.1.0`. A later
  version needs a committed version update first.
- The release commit is reachable from `main`.
- The worktree is clean.
- `FERL_CI_ENABLED` and `FERL_RELEASE_ENABLED` are both `true`.

### Procedure

```sh
git tag -a v0.1.0 -m "ferl v0.1.0" <commit>
git push origin v0.1.0
```

Pushing the tag starts `release.yml`:

1. **Preflight** rejects anything but a stable `vMAJOR.MINOR.PATCH` tag without
   leading zeroes, a checkout that does not match the annotated tag's commit, a
   commit that is not reachable from `main`, a dirty tree, and a tag that
   disagrees with `Cargo.toml` or `Cargo.lock`. Prerelease tags and mutable
   aliases such as `latest` are not published by this workflow.
2. **Verify** runs the complete verification pipeline on the tagged commit.
3. **Publish** downloads the candidate from this run, checks it, loads the
   tested image without rebuilding, and refuses to overwrite an existing version.
   It then pushes `ghcr.io/ai-slop-code/ferl:v<version>` and
   `ghcr.io/ai-slop-code/ferl:sha-<commit>`, confirms that the registry digest
   equals the candidate manifest digest, and attaches build provenance and the
   SBOM to that digest.

The immutable reference is `ghcr.io/ai-slop-code/ferl@sha256:<digest>`. The
digest is shown in the run summary and in `publication.json`. Local image IDs
are not registry digests.

Publication authenticates with the run's `GITHUB_TOKEN`, using a registry
auth file under `$RUNNER_TEMP`. The file is removed in a cleanup step. Passwords
are passed on standard input, not in arguments.

### After the first publication

Packages are created private, even though the repository is public. Do this
once, after the first successful publication:

1. In **Packages → ferl → Package settings**, change visibility to **Public**.
2. Verify anonymous access from a host with no registry credentials. The
   `--authfile` path must not exist or must be empty:

   ```sh
   printf '{}\n' > "$RUNNER_TEMP/anonymous-auth.json"   # or any private temp path
   podman --authfile "$RUNNER_TEMP/anonymous-auth.json" pull \
     --platform linux/amd64 ghcr.io/ai-slop-code/ferl@sha256:<digest>
   ```

3. Do not declare distribution complete until this pull succeeds.

Later approved version tags publish automatically. A manually published GitHub
Release is not required.

## Failed publication and recovery

- **Verification failed.** Fix the cause on a new commit, then release a new
  version. Do not move a published tag or rebuild an existing version for
  publication.
- **Publication failed before any push.** Re-run the failed publication job.
  Publication downloads the newest candidate from the run and checks its checksum
  and metadata again. Re-running only this job is not yet validated in hosted CI.
- **Publication failed after the version tag was pushed.** Re-run publication.
  If the registry already names the same manifest digest, publication resumes.
  If it names a different digest, the workflow stops. Do not force the tag.
  Release a new patch version instead.
- **Candidate expired.** After 14 days the artifact is gone. Publish a new
  version from a new tag. Do not rebuild the old version and publish it.

## Protection to configure

These settings are applied:

- `main` has no force-push or deletion permission.
- Tags matching `v*` are protected by the ruleset `release-version-tags`. It
  restricts creation, update, deletion, and non-fast-forward changes. Only the
  repository owner can bypass it, so a maintainer cannot move or delete a
  published version tag without the owner's account.

Not yet configured:

- Required status checks. Check names such as `verify / msrv` appear only after
  the first successful run. Add them to `main` at that point.

## Related documents

- [Release and self-hosting](release.md): packaging, first startup, proxy
  deployment, upgrades, and backups.
- [Release verification](release-verification.md): historical verification
  evidence and its limits.
- [Development](development.md): local fixtures and verification commands.
