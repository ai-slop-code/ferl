# ferl

**Short links on your own domain, with data you control.**

ferl is a self-hosted URL-shortening backend written in Rust. It provides a JSON
API for managing accounts, links, and visit analytics, backed by PostgreSQL.
Public short links redirect directly to their destinations.

[Getting started](#getting-started) · [API examples](#use-the-api) ·
[Self-hosting](#running-on-your-own-domain) · [Docker image](#docker-image-planned) ·
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

## Docker image (planned)

The intended distribution path is a prebuilt Docker-compatible image published
to **GitHub Container Registry (`ghcr.io`)**. You will pull that image and run it
on your own server with an external PostgreSQL database.

There is currently no published ferl image or ready-to-run Compose deployment
documented in this repository. The image name, version tags, supported
architectures, and Docker/Compose commands will be added here when the image is
available. For now, use the source-build instructions above; the
[release guide](docs/release.md) also documents building and verifying a Linux
release archive with Podman.

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
