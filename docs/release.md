# ferl release and operator guide

## Supported artifact and verification scope

The release archive contains the `ferl-server` executable, the exact checked-in
`spec/openapi.yaml`, both SQL migrations, the checked-in NGINX routing/header
configuration and minimal static fixture, `Cargo.lock`, the pinned toolchain
declaration, operator documents, `SHA256SUMS`, and `PROVENANCE.json`. Migration
support is compiled into the executable **and** the SQL is shipped as an
operator-readable inventory: `0001_initial.sql` and
`0002_discovery_indexes.sql`. `ferl-server migrate` applies the embedded
checksummed migrations; do not edit `_sqlx_migrations` or use the shipped SQL as
an alternative manual migration path.

The locally verified Part 11 archive targets `aarch64-unknown-linux-gnu`, built
with Rust/Cargo 1.99.1 in the pinned Debian Bookworm builder image. Its release
smoke runs the actual packaged executable as UID 65532 in that same pinned
Linux/arm64 image. This is **not** a claim of compatibility with other Linux
distributions, libc versions, CPU architectures, or operating systems. The
archive is not a container image. No bit-for-bit reproducibility claim is made;
the packaging process normalizes archive timestamps/ownership and records
checksums and build provenance. The documented locked builder is a repeatable
procedure; it does not by itself prove independent builds are byte-identical.
Packaging can be compared deterministically when the executable and all source
inputs are identical; the hardening handoff records whether that comparison was
measured.

The release script is configured for `aarch64-unknown-linux-gnu` and
`x86_64-unknown-linux-gnu`; each uses its target-specific, digest-pinned Debian
Bookworm Rust image (arm64
`docker.io/library/rust@sha256:32f7944ed00949c44d3944a0a852b3e5ea862a610a38b43ac92a33aa87256b75`,
amd64
`docker.io/library/rust@sha256:8bf7188ae46ad73b801c8d405eedc13ce63a7852b562885743a56890723549a8`).
In particular, CI no longer builds an Ubuntu-native executable and then assumes
it runs on an older Bookworm runtime. The x86_64 builder path is configured for
the Linux CI host, but only the arm64 artifact has been executed locally; no
hosted workflow result is claimed here.

## Runtime TLS and CA trust

Rustls is used for PostgreSQL and OTLP HTTPS connections. PostgreSQL TLS options
are parsed by SQLx 0.9: use `sslmode=verify-full` and an explicit
`sslrootcert=/absolute/path/to/ca-bundle.pem` for private roots. SQLx's enabled
`tls-rustls-ring-webpki` feature supplies bundled public roots and adds the
configured PEM roots; `verify-full` checks both the certificate chain and URL
hostname.

The OTLP exporter uses the locked `opentelemetry-otlp` 0.33.1 → `reqwest` 0.13.5
Rustls client and its platform verifier. It uses the runtime's normal trust
source. On Linux, `SSL_CERT_FILE` and `SSL_CERT_DIR` are honored by the resolved
rustls native-certificate loader; provide a PEM bundle containing the required
roots (including ordinary public roots if replacing the default bundle) or
install private roots using the runtime's documented trust mechanism. The
production TLS test supplies a run-local `SSL_CERT_FILE`; it does not change a
host trust store. Collector certificate errors remain best-effort telemetry
failures and do not gate HTTP requests or readiness. Export attempts and graceful
shutdown remain bounded by the configured timeout and total shutdown deadline.

No inbound TLS listener is implemented; terminate client TLS at the reverse
proxy. Run `sh scripts/check-tls.sh` for packaged PostgreSQL and OTLP HTTPS
verification.

Build and verify the archive from a clean checkout with the pinned Rust builder
image available locally:

```sh
podman info
podman machine list
# On a new machine, run the explicitly digest-pinned image pull first.
podman pull docker.io/library/rust@sha256:32f7944ed00949c44d3944a0a852b3e5ea862a610a38b43ac92a33aa87256b75
sh scripts/package-release.sh
sh scripts/check-release.sh
```

The documented builder-image pull uses ordinary TLS verification. In the
owner-authorized local intercepting-registry environment, the fixture scripts
may use `--tls-verify=false` only for fully qualified, digest-pinned image pulls;
this does not change runtime TLS. `scripts/package-release.sh` checks clean Git
provenance before the build; the builder runs `cargo fetch --locked`, then uses
the reusable Cargo registry cache (writable so Cargo can unpack locked sources)
and an isolated Cargo target under ignored `target/`. A fresh target directory
therefore does not imply a cold registry. The script refuses to
silently substitute another builder digest. Set `FERL_RELEASE_OUTPUT_DIR` to
choose another local output directory. Verify both the archive and its internal
inventory:

```sh
cd target/release-packages
shasum -a 256 -c ferl-server-0.1.0-aarch64-unknown-linux-gnu.tar.gz.sha256
mkdir -p inspect
tar -xzf ferl-server-0.1.0-aarch64-unknown-linux-gnu.tar.gz -C inspect
cd inspect
shasum -a 256 -c SHA256SUMS
cmp spec/openapi.yaml ../../../spec/openapi.yaml
```

The extracted binary's revision, target, toolchain/image, schema and migration
inventory, binary checksum, and OpenAPI checksum are in `PROVENANCE.json`.
Keep the archive and sidecar checksum together. Checksums detect corruption;
they do not authenticate the publisher. Use the organization's separate signed
artifact-distribution process when one is established.

## First startup

`docs/configuration.md` contains the complete setting inventory (type, default,
bounds, precedence, and command-specific scope); examples below only highlight
fleet-critical values.

Provision PostgreSQL 17.x, create a database/role and (if used) a schema, then
point **every** ferl instance at the same PostgreSQL primary/read-write endpoint
and schema. The supported ferl schema is v2. Create secrets out of band; never
put them in this document, shell history, telemetry, or a shared example file.

```sh
export FERL_DATABASE_URL='postgres://ferl:REDACTED@db.example/ferl?sslmode=verify-full'
export FERL_PUBLIC_BASE_URL='https://s.example'
export FERL_CURSOR_KEY="$(python3 -c 'import base64,secrets; print(base64.urlsafe_b64encode(secrets.token_bytes(32)).rstrip(b"=").decode())')"
export FERL_LISTEN_ADDR='0.0.0.0:8080'

./bin/ferl-server migrate
umask 077
./bin/ferl-server bootstrap-admin --username ferl-admin > /secure/ferl-bootstrap.json
./bin/ferl-server serve
```

`migrate` must finish successfully before bootstrap or serving. Bootstrap is a
one-time committed database operation; its JSON stdout contains the only
initial plaintext admin key. Store the file in a private secret manager, verify
the key has been retained, and securely remove any temporary local copy.
Restarting the server never migrates, bootstraps, or rotates credentials.
Readiness remains false until the binary verifies the complete matching schema
history and required objects, then completes a bounded PostgreSQL probe.

`FERL_DATABASE_URL` should use the primary/read-write database, including for
reads. A lagging read replica can invalidate authentication, cursor,
reservation, and deletion freshness guarantees. `FERL_PUBLIC_BASE_URL` is an
origin only; it is not inferred from `Host` or forwarding headers.

## Fleet configuration and capacity

Instances serving one deployment must use the same public origin, database
primary/schema, 32-byte cursor key, anonymous-creation switch, account/IP
creation limits, Unicode/runtime version, and other business settings. Cursor
continuations are stateless and can cross instances only with the same key and
authorization. Provision `FERL_CURSOR_KEY` once from a cryptographic source and
store it in the deployment secret manager. A coordinated key change invalidates
all outstanding cursors; rotate it only as an intentional fleet-wide operation.
Do not generate an instance-local fallback or change the key on restart.

Size database capacity for all instances and other clients:

```text
maximum ferl connections = replica count × FERL_DB_POOL_MAX
total PostgreSQL budget = ferl + migration/admin/backup/monitoring clients
```

Defaults are up to 10 pooled connections per instance (configurable maximum
128), plus short-lived command connections. Keep the sum below PostgreSQL's
usable `max_connections` with operational reserve. More application instances
do not create database capacity or promise linear throughput scaling.

`FERL_ANONYMOUS_CREATION` defaults to `false`; existing anonymous tokens remain
valid when the switch is disabled. Default creation accounting is five
anonymous attempts per effective IP and 60 account attempts per account UUID
per rolling 60 seconds (admins included). Eligible attempts consume a durable
charge before body validation, so malformed requests, conflicts and later
creation failures are charged; rate-rejected attempts are not. These are
attempt limits, not successful-link quotas. Credential rotation and process or
database-container restart do not reset live charges.

All authoritative account/link/token/report operations use the PostgreSQL
primary. There is no sticky session, local durable state, destination cache, or
session transfer step.

## Reverse proxy and IP trust

Serve `/` and `/assets/*` from a separate static host. Route exact `/api` and
`/api/*` to ferl before generic short-code routing; pass other paths unchanged
to ferl. Do not configure SPA history fallback. The checked-in NGINX example and
minimal fixture are in `deploy/proxy/`; run the actual TCP verification with
`sh scripts/check-proxy.sh` (or as part of `sh scripts/check-release.sh`). It
uses the pinned NGINX 1.30.6 stable Alpine image and a run-isolated container.
Only the fixture HTML and asset are static; there is no SPA implementation.

The example's body handling streams requests to ferl, whose fixed maximum is
65,536 bytes; it does not buffer a second unbounded copy. NGINX's request line
uses one 128 KiB large-header buffer, enough for ferl's 87,387-byte encoded
cursor plus path/query syntax. There are four such large buffers per connection;
ordinary request headers remain subject to that aggregate bound. The server
also accepts up to 8,192 bytes for each supported analytics header. Choose
connection and slow-client limits with those per-connection buffers in mind.

Trust only proxy IPs/CIDRs observed by the backend and only when those proxies
sanitize/establish X-Forwarded-For. The example is an edge proxy and replaces
client-supplied forwarding data with the TCP peer NGINX actually observed. If a
load balancer precedes it, establish a controlled chain at that edge and
configure each actual trusted hop explicitly. Never trust all private ranges
or `127.0.0.1` merely because it looks local.

On the local macOS Podman host-proxy path, Part 11's direct probe observed
`127.0.0.1` as the ferl process's peer even though NGINX was in a container.
That source-NAT observation is **not safe to trust**: a direct local client can
have the same peer address. The supported proxy verification therefore places
NGINX and the packaged ferl process on one isolated user-defined Podman network,
observes the actual container peer, and trusts only that address. The separate
host-reachability probe records the host-gateway behavior but does not add it to
`FERL_TRUSTED_PROXIES`. For deployment, use a network topology where proxy and
backend peers are distinguishable and enforce network access so clients cannot
reach the trusted backend interface directly.

## Redirect and analytics semantics

GET and HEAD of an active code issue an empty 302 with the stored `Location`
and `Cache-Control: no-store`; query strings on the short URL are ignored.
Visits count redirects issued, not successful destination loads. Visit writes
are best effort through a bounded in-memory queue. Reports are eventually
consistent, counters can undercount responses, and process failure can lose
events not yet committed. Committed visit rows and counters are PostgreSQL
state. Historical destination snapshots remain with the live link indefinitely
and are returned only through authorized per-link reports; deleting a link
deletes those rows and its counter. Link deletion leaves a minimal permanent
short-code reservation. Deleting an account cascades its links/visits and keeps
reservations. Reusing a deleted username creates a new UUID and inherits no
links.

Collector availability and queue depth do not affect readiness or request
success. Readiness is withdrawn before graceful removal; SIGTERM/SIGINT triggers
bounded HTTP drain, analytics best-effort drain and telemetry flush under one
total shutdown deadline. Measure process exit, not a shutdown log line.

## Upgrade, backup and restore

Migrations are append-only and serialized by a PostgreSQL advisory lock. The v2
upgrade is not an overlapping rolling deployment: stop/drain all older
instances, run `migrate` with the new binary, verify schema readiness, then
start new instances. The v1 binary fails closed after v2 migration. Rollback to
v1 requires restoring the pre-upgrade database or an explicit forward fix; do
not delete migration history.

Back up PostgreSQL consistently, including accounts/current credential
digests, owned and anonymous live links, titles/tags, visits and counters,
permanent code reservations, bootstrap completion, migration history, and live
rate charges. Back up `FERL_CURSOR_KEY` separately in the secret manager; the
database backup cannot restore it. A restore must also restore a compatible
binary/configuration and preserve the cursor key if old cursors must continue.
Validate restores in an isolated database by checking `migrate` compatibility
and readiness before traffic. This release gate verifies populated schema
upgrade/restart preservation; it does not claim a provider-specific backup
product or an external disaster-recovery exercise.

## Verification commands

From a clean checkout, install Rust 1.99.1 with rustfmt/clippy, Rust 1.94.0,
and Podman. Start the existing Podman machine on macOS. The TLS gate creates
run-specific PostgreSQL and Collector containers and removes only those
containers/network; it does not restart or remove the retained named PostgreSQL
fixture volume.

```sh
rustup toolchain install 1.94.0 --profile minimal
sh scripts/check-msrv.sh
sh scripts/check-tls.sh
sh scripts/postgres.sh start
sh scripts/postgres.sh ready
sh scripts/check-openapi.sh
sh scripts/check-proxy.sh
sh scripts/check.sh
sh scripts/check-release.sh
```

`tests/legacy_compatibility.rs` needs retained Part 07 commit
`9bcf7c0c021c994e76d0696633fe822f0a49a5e1`; CI must fetch full Git history
(`fetch-depth: 0`). It builds the old source in a separate target directory
with a bounded cold-build deadline. A shallow checkout missing that commit is a
failed prerequisite, not a skipped compatibility test.
