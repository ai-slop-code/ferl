# ferl

**Short links on your own domain, with data you control.**

ferl is a self-hosted URL-shortening backend written in Rust. It provides a JSON
API for managing accounts, links, and visit analytics, backed by PostgreSQL.
Public short links redirect directly to their destinations.

[Getting started](#getting-started) · [API examples](#use-the-api) ·
[Self-hosting](#running-on-your-own-domain) · [Docker image](#docker-image) ·
[Documentation](#documentation)

## What you get

- **Your own short-link domain** with generated or custom short codes.
- **Accounts and API keys** with admin and user roles, account isolation, and key rotation.
- **Link management** with editable destinations, optional titles, tags, and enable/disable controls.
- **Search and discovery** with filtering, sorting, and cursor-based pagination.
- **Per-link analytics** with visit records, counts, and breakdowns by time, referrer, client IP, user agent, and more.
- **Optional anonymous creation**, disabled by default, with a per-link management token.
- **PostgreSQL-backed state** shared across instances, including creation rate limits and permanent code reservations.
- **Operational visibility** through health endpoints, structured logs, and optional OpenTelemetry export.

This repository contains the backend. A web interface and a separate CLI client
are planned as separate projects; you can use the API today with `curl` or your
own integrations. The full interface is defined in [OpenAPI 3.1](spec/openapi.yaml).

## Getting started

The steps below run a source-built server with a dedicated local PostgreSQL
database. Run shell commands from the repository root unless noted otherwise.

### 1. Build the server

You will need:

- **Rust 1.99.1**, the toolchain pinned in `rust-toolchain.toml`.
- A native C/C++ build toolchain, **CMake**, and **pkg-config** for native dependencies.
- **PostgreSQL 17.x** and its command-line tools. No PostgreSQL extensions are required.
- **Python 3** and **curl** for the setup and API examples below.

Clone or download this repository, then enter its directory. With
[rustup](https://rustup.rs/) installed:

```sh
rustup toolchain install 1.99.1 --profile minimal --component rustfmt --component clippy
cargo build --locked --release --bin ferl-server
./target/release/ferl-server --version
```

The executable is `target/release/ferl-server`. Migrations are embedded in it;
running the server does not require Rust or Cargo to be installed.

### 2. Prepare PostgreSQL

Create a dedicated database and login role. For a local Linux PostgreSQL
installation, run these as an operator with permission to use the `postgres`
administrative account:

```sh
sudo -u postgres createuser --pwprompt --no-superuser --no-createdb --no-createrole ferl
sudo -u postgres createdb --owner=ferl ferl
```

Choose a strong password when prompted. Ensure PostgreSQL accepts password
authentication for this role from the server's host. For a managed or remote
database, create the equivalent role/database using that provider's tools and
use its **primary/read-write** endpoint.

The migration role needs permission to create tables, indexes, and functions in
the selected schema. See [database setup](docs/database.md) for custom schemas
and the requirement to use the same `search_path` for migration and serving.

### 3. Configure ferl

Generate a cursor-encryption key **once per deployment**:

```sh
python3 -c 'import base64,secrets; print(base64.urlsafe_b64encode(secrets.token_bytes(32)).rstrip(b"=").decode())'
```

Save it in your configuration and back it up separately from PostgreSQL. Reuse
the same key on every restart and instance; changing it invalidates existing
pagination cursors.

Create a private environment file:

```sh
umask 077
mkdir -p "$HOME/.config/ferl"
touch "$HOME/.config/ferl/ferl.env"
chmod 600 "$HOME/.config/ferl/ferl.env"
```

Open that file in your editor and add the following, replacing the password and
cursor-key placeholders:

```sh
FERL_DATABASE_URL='postgres://ferl:REPLACE_WITH_DATABASE_PASSWORD@127.0.0.1:5432/ferl'
FERL_PUBLIC_BASE_URL='http://127.0.0.1:8080'
FERL_CURSOR_KEY='REPLACE_WITH_GENERATED_CURSOR_KEY'
FERL_LISTEN_ADDR='127.0.0.1:8080'
FERL_ANONYMOUS_CREATION='false'
```

Percent-encode special characters in the database URL's username/password. The
local URL above is for same-host setup; for a remote database, use verified TLS
as described in [Running on your own domain](#running-on-your-own-domain).

ferl reads the process environment; it does **not** automatically load `.env`
files. Load your file into the current shell before running commands:

```sh
set -a
. "$HOME/.config/ferl/ferl.env"
set +a
```

### 4. Migrate and create your first admin

Apply migrations explicitly:

```sh
./target/release/ferl-server migrate
```

After migration succeeds, bootstrap the first administrator **once**. Its API
key is returned only in the command's JSON output, so retain that output privately:

```sh
(
    umask 077
    set -C
    ./target/release/ferl-server bootstrap-admin --username ferl-admin \
        > "$HOME/.config/ferl/bootstrap.json"
)
```

`set -C` prevents overwriting an existing bootstrap result. Store the returned
key in your secret manager. Bootstrap completion is saved in PostgreSQL; running
bootstrap again cannot retrieve the key or create a replacement administrator.

### 5. Start the server

```sh
./target/release/ferl-server serve
```

In another terminal, check readiness:

```sh
curl --fail-with-body http://127.0.0.1:8080/api/health/ready
```

A ready server returns `200` with `{"status":"ok"}`. Readiness may initially
return `503` while schema verification completes. `/api/health/live` checks
whether the HTTP process can respond. The backend's `/` returns `404`; use the
health endpoints to check that it is running.

## Use the API

In a second terminal, load the initial admin key from the private bootstrap file:

```sh
FERL_API_KEY="$(python3 -c 'import json,pathlib; print(json.loads((pathlib.Path.home()/".config/ferl/bootstrap.json").read_text())["api_key"])')"
FERL_URL='http://127.0.0.1:8080'
```

For a remote installation, set `FERL_URL` to your public HTTPS origin.
`FERL_API_KEY` and `FERL_URL` are shell conveniences for these examples, not server settings.

**Create a short link:**

```sh
curl --fail-with-body "$FERL_URL/api/v1/links" \
    -H "Authorization: Bearer $FERL_API_KEY" \
    -H 'Content-Type: application/json' \
    --data '{"destination_url":"https://example.com/docs?edition=2#intro","short_code":"my-docs","title":"Documentation","tags":["docs"]}'
```

The `201` response contains a `link` object with its `short_url`. Omit
`short_code` to generate one automatically.

**List your links and inspect a redirect:**

```sh
curl --fail-with-body "$FERL_URL/api/v1/links" \
    -H "Authorization: Bearer $FERL_API_KEY"

curl --head "$FERL_URL/my-docs"
```

The redirect returns `302`, the exact stored destination in `Location`, and
`Cache-Control: no-store`. Both GET and HEAD record a best-effort visit.

**Read visit counts:**

```sh
curl --fail-with-body "$FERL_URL/api/v1/links/my-docs/stats" \
    -H "Authorization: Bearer $FERL_API_KEY"
```

Visit persistence is asynchronous, so counts may take a moment to appear.
Visits measure redirects issued, not confirmed destination loads.

Admins provision other accounts through `POST /api/v1/admin/accounts`. Normal
link listings show the caller's own links; admins can explicitly request
`scope=all`. See the [OpenAPI contract](spec/openapi.yaml) for account creation,
link edits/deletion, credential rotation, and analytics queries.

## Running on your own domain

The usual deployment is:

```text
Client → HTTPS reverse proxy → ferl-server → PostgreSQL 17
                                  └──────→ OpenTelemetry Collector (optional)
```

- Set `FERL_PUBLIC_BASE_URL` to an origin such as `https://s.example.com`, with
  no path prefix. This controls returned short URLs; it does not configure DNS or TLS.
- Terminate HTTPS at your reverse proxy. ferl serves HTTP internally; expose its
  listener only to the intended proxy/network.
- For remote PostgreSQL, use `sslmode=verify-full` and your CA bundle, for example
  `postgres://ferl:ENCODED_PASSWORD@db.example.com/ferl?sslmode=verify-full&sslrootcert=/etc/ferl/db-ca.pem`.
- Route exact `/api` and `/api/*` to ferl before short-link routing. Preserve raw
  paths and do not add a site-wide SPA fallback. `/` and `/assets/*` are reserved
  for a separately hosted web client; the files shipped in `deploy/proxy/static/`
  are test fixtures.
- Set `FERL_TRUSTED_PROXIES` only to the actual proxy peers seen by ferl, and
  configure the edge proxy to replace untrusted `X-Forwarded-For` values. Empty
  trust uses the connection peer. A broad private-network or loopback trust rule
  can let direct callers spoof their identity.
- Preserve the request-target/header limits in the [NGINX example](deploy/proxy/nginx.conf),
  including enough space for server-issued analytics cursors. Its companion
  [forwarding configuration](deploy/proxy/ferl-proxy-headers.conf) is included by
  the routing file.

Run the executable under your service manager as a dedicated unprivileged user.
Supply the same saved environment on every start and run `serve` as the normal
startup command. Migration and bootstrap are separate operator actions.

### Useful settings

| Setting | Default | Purpose |
| --- | --- | --- |
| `FERL_ANONYMOUS_CREATION` | `false` | Allow creation without an account; returns a per-link management token. |
| `FERL_ACCOUNT_CREATE_LIMIT` | `60` | Creation attempts per account in a rolling 60-second window. |
| `FERL_ANONYMOUS_CREATE_LIMIT` | `5` | Creation attempts per effective client IP in the same window. |
| `FERL_DB_POOL_MAX` | `10` | Maximum pooled database connections per instance. |
| `FERL_TRUSTED_PROXIES` | Empty | Explicit trusted proxy IPs/CIDRs. |
| `FERL_OTEL_ENDPOINT` | Unset | Optional OTLP HTTP/protobuf Collector base URL. |
| `FERL_SHUTDOWN_TIMEOUT_SECS` | `45` | Total graceful shutdown deadline. |

Settings are read at startup. All instances of a deployment must share its
database, cursor key, public origin, and business settings. Full defaults,
bounds, TLS trust sources, and OTEL precedence are in the
[configuration reference](docs/configuration.md).

### Restart, upgrade, and back up

- **Restart:** send SIGTERM or SIGINT (Ctrl+C in a terminal). Allow the configured
  shutdown budget before forcing termination. Start `serve` again with the same
  database and environment; no migration, bootstrap, or reseeding is needed for
  an ordinary restart.
- **Upgrade:** back up first, follow the version-specific migration instructions,
  and wait for readiness before restoring traffic. The documented v1 → v2
  schema upgrade requires stopping old instances before migration; mixed-version
  rolling compatibility is not assumed.
- **Back up:** take a consistent PostgreSQL backup, including accounts, links,
  titles/tags, visits/counters, rate accounting, deleted-code reservations,
  bootstrap state, and migration history. Retain the cursor key separately and
  test restoration into an isolated database with a compatible executable.

Deleting a link permanently reserves its code. Visit ingestion is best effort:
queued events may be lost on abrupt process termination. See the
[release and operator guide](docs/release.md) for the complete operational model.

## Docker image

> **Not yet published.** The first image will be available after the `v0.1.0`
> release is published. Until then, use the source build above. The commands
> below describe the intended deployment; they become valid once the digest in
> the release summary is available.

ferl publishes a Docker-compatible image for **`linux/amd64`** to GitHub
Container Registry: `ghcr.io/ai-slop-code/ferl`. It runs with Podman on a native
x86_64 Linux host and uses an **external PostgreSQL 17.x** database. The image
contains only the `ferl-server` executable, CA certificates, and a glibc runtime,
and it runs as UID `65532`.

Release automation and the verification behind each image are described in
[CI/CD and container delivery](docs/cicd.md).

### Prepare an external PostgreSQL 17 database

You need PostgreSQL **17.x** with TLS and SCRAM authentication, reachable from the
host where ferl runs. Use the primary (read-write) endpoint. Replace the
placeholders and connect as an administrative account allowed to create a role
and a database. Use a verified CA certificate:

```sh
psql \
  "host=db.example.com port=5432 dbname=postgres user=DB_ADMIN sslmode=verify-full sslrootcert=/path/to/db-ca.pem" \
  -X -W -v ON_ERROR_STOP=1
```

Confirm the server version before continuing:

```sql
SHOW server_version;
```

For a new deployment, create a dedicated, non-superuser application role that
owns its database. It runs migrations as well as serving:

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

`\password` prompts for the password, so it is not written into the SQL. These
are initialization commands. Do not run them against an existing role or
database you do not intend to change. On a managed database, use the provider's
equivalent operations with the same ownership and search path.

Prerequisites:

- The application host can resolve and reach the database host.
- The server certificate matches the hostname in `FERL_DATABASE_URL`.
- The `pg_hba.conf` (or the provider's equivalent) allows SCRAM from the
  application host for the `ferl` role.
- The CA certificate is readable inside the container.

ferl does not need any PostgreSQL extensions. Use the same database, schema,
and search path for migration, bootstrap, and serving.

### Create private runtime configuration

```sh
umask 077
mkdir -p "$HOME/.config/ferl"
touch "$HOME/.config/ferl/ferl.env"
chmod 600 "$HOME/.config/ferl/ferl.env"
```

Write the Podman environment file. Use plain `KEY=value` lines with no quotes and
no `export`. Podman passes these values to the process. Do not `source` the file:

```dotenv
FERL_DATABASE_URL=postgres://ferl:URL_ENCODED_PASSWORD@db.example.com:5432/ferl?sslmode=verify-full&sslrootcert=/etc/ferl/db-ca.pem
FERL_PUBLIC_BASE_URL=https://s.example.com
FERL_LISTEN_ADDR=0.0.0.0:8080
FERL_CURSOR_KEY=REPLACE_WITH_GENERATED_KEY
FERL_ANONYMOUS_CREATION=false
FERL_ENVIRONMENT=production
FERL_SHUTDOWN_TIMEOUT_SECS=45
```

Percent-encode special characters in the password part of the database URL. The
`sslrootcert` path is the path inside the container, not on the host.

Generate the cursor key once per deployment and store it privately with the
environment file. Keep a separate backup of it, and reuse it across restarts and
instances. Changing it invalidates existing pagination cursors:

```sh
python3 -c 'import base64,secrets; print(base64.urlsafe_b64encode(secrets.token_bytes(32)).rstrip(b"=").decode())'
```

Place the trusted database CA at `$HOME/.config/ferl/db-ca.pem`. The container's
non-root process must be able to read it. Podman's `--env-file` supplies the
settings. ferl does not load `.env` files on its own.

### Pull and migrate

Use the digest from the release summary. Tags are convenient, but the digest is
the immutable reference:

```sh
FERL_CONFIG="$HOME/.config/ferl"
FERL_IMAGE='ghcr.io/ai-slop-code/ferl@sha256:REPLACE_WITH_PUBLISHED_DIGEST'

podman pull --platform linux/amd64 "$FERL_IMAGE"
```

Define a one-shot helper for the current shell session:

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

Migration must succeed before bootstrap or startup. Do not apply the SQL by hand
or edit migration history to force readiness.

### Bootstrap the first administrator once

Run this after migration succeeds. It writes the one-time key to a private file:

```sh
(
  umask 077
  set -C
  ferl_once bootstrap-admin --username ferl-admin \
    > "$FERL_CONFIG/bootstrap.json"
)
```

The helper disables container logging, so the returned key is not kept in a log
driver. Store the key in your secret manager, then delete the bootstrap file.
Bootstrap is recorded in PostgreSQL. Running the command again does not recover
a lost key or create a replacement administrator. If output fails after the
database commit, follow the [release and operator guide](docs/release.md) for
recovery. Do not automate retries or reset the database.

### Start the server

For a reverse proxy on the same host:

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

On SELinux hosts, the CA mount may need a relabel option such as `:ro,z`. Check
that the mount is readable by the non-root process in your environment.

```sh
curl --fail-with-body http://127.0.0.1:8080/api/health/ready
```

A ready server returns:

```json
{"status":"ok"}
```

Readiness can return `503` while schema verification completes.
`/api/health/live` checks that the process responds. `/api/health/ready` also
checks database and schema readiness. The root path `/` is not a health endpoint.

Keep these points in mind:

- `FERL_PUBLIC_BASE_URL` sets the origin used in returned short URLs. It does not
  configure DNS and does not terminate TLS.
- HTTPS terminates at your reverse proxy. ferl serves plain HTTP internally. See
  [Running on your own domain](#running-on-your-own-domain) for routing.
- Set `FERL_TRUSTED_PROXIES` to the addresses of your actual proxy. Do not trust
  an entire private network by default.
- The loopback publish address is for a proxy on the same host. Adapt it for any
  other network topology.
- For unattended hosting, use a service manager. A rootless Podman Quadlet should
  keep the same image, environment file, and CA mount, with a stop allowance
  longer than `FERL_SHUTDOWN_TIMEOUT_SECS`. If the service needs to run without a
  login session, enable lingering for that user.

### Upgrade and roll back

Record the running image digest, back up PostgreSQL, and keep the cursor key
separately. Then review the release notes for migration compatibility. Stop the
old instances if the migration requires it, run `migrate` with the new digest,
start the new image, and wait for readiness before sending traffic.

ferl's schema changes are not overlapping-compatible. Restoring the previous image
alone does not undo a migration. Restore a compatible database backup, or apply a
deliberate forward fix. The full procedure is in
[Restart, upgrade, and back up](#restart-upgrade-and-back-up) and
[the release and operator guide](docs/release.md).

## Project status

The agreed backend feature scope is complete. Local Rust, PostgreSQL, proxy,
packaging, and TLS verification are recorded in the
[release verification report](docs/release-verification.md). Packaged runtime
execution has been verified on Linux/arm64 in the pinned Debian Bookworm image;
hosted CI, native Linux/amd64 acceptance, and staging/restore acceptance remain
pending. These results do not establish production readiness or support for
additional platforms.

## Documentation

| Guide | Contents |
| --- | --- |
| [Configuration](docs/configuration.md) | Environment variables, defaults, bounds, secrets, and TLS. |
| [Release and self-hosting](docs/release.md) | Packaging, first startup, proxy deployment, upgrades, and backups. |
| [Database](docs/database.md) | PostgreSQL versions, schemas, migrations, and compatibility. |
| [Operations](docs/operations.md) | Collector setup, health, and graceful shutdown. |
| [Analytics reports](docs/analytics-reports.md) | Reports, consistency, pagination, and cursor transport. |
| [Development](docs/development.md) | Toolchain, Podman test fixtures, and verification commands. |
| [OpenAPI](spec/openapi.yaml) | Machine-readable HTTP API contract. |

For development, start with [the local verification guide](docs/development.md).
Its Podman fixtures are for isolated testing, including intentional outage and
restart exercises; use a separate database for your self-hosted deployment.
