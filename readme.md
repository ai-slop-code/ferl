# ferl

**Self-hosted short links with your own domain and your own data.**

ferl is a URL-shortening backend. It exposes a JSON API for accounts, links,
and visit analytics, and it serves short links as HTTP redirects. State lives
in PostgreSQL 17.x or 18.x.

This guide gets a ferl server running from the published container image.
For building from source, see [Development](docs/development.md).

## Features

- Generated or custom short codes on your own domain.
- Accounts and API keys with admin/user roles, account isolation, and key rotation.
- Editable destinations, optional titles, tags, and enable/disable controls.
- Link search, filtering, sorting, and cursor-based pagination.
- Per-link visit records, counts, and breakdown reports.
- Optional anonymous creation, disabled by default, with per-link management tokens.
- Shared PostgreSQL state, creation rate limits, and permanent code reservations.
- Structured logs, health endpoints, and optional OpenTelemetry export.

The web interface and CLI client are separate projects. You can use the JSON API
with `curl` or your own integrations today.

## Contents

- [Requirements](#requirements)
- [1. Prepare PostgreSQL](#1-prepare-postgresql)
- [2. Write the configuration](#2-write-the-configuration)
- [3. Pull the image](#3-pull-the-image)
- [4. Migrate the database](#4-migrate-the-database)
- [5. Create the first administrator](#5-create-the-first-administrator)
- [6. Start the server](#6-start-the-server)
- [7. Verify the installation](#7-verify-the-installation)
- [Put ferl behind HTTPS](#put-ferl-behind-https)
- [Use the API](#use-the-api)
- [Upgrade, back up, and restore](#upgrade-back-up-and-restore)
- [Documentation](#documentation)

## Requirements

- A **Linux x86_64** host with **Podman** (or another Docker-compatible runtime
  configured for `linux/amd64`).
- **PostgreSQL 17.x or 18.x** reachable from the host, with TLS enabled and SCRAM
  authentication. ferl does not need any PostgreSQL extensions. The supported
  version is the connected server's version; `psql --version` alone does not
  establish it.
- A CA certificate that trusts the PostgreSQL server, saved as a PEM file.
- A public hostname and a reverse proxy that terminates HTTPS. The examples
  below run ferl on `127.0.0.1:8080` for a proxy on the same host.
- `python3`, `curl`, and `psql` for the setup commands in this guide.

The image runs as the non-root user `65532`, with a read-only root filesystem
and all Linux capabilities dropped. It contains only the `ferl-server`
executable and its runtime libraries.

## 1. Prepare PostgreSQL

Connect as an administrative role that may create roles and databases. Replace
the host, the administrative user, and the CA path:

```sh
psql \
  "host=db.example.com port=5432 dbname=postgres user=DB_ADMIN sslmode=verify-full sslrootcert=/path/to/db-ca.pem" \
  -X -W -v ON_ERROR_STOP=1
```

Confirm the server is PostgreSQL 17 or 18. Use a PostgreSQL 18 client
(`psql`, `pg_dump`, `pg_restore`) for PostgreSQL 18 setup and backup commands:

```sql
SHOW server_version;
```

The ferl binary supports PostgreSQL 17.x and 18.x. An existing PostgreSQL 17
cluster is not converted by installing a new ferl binary; see
[docs/database.md](docs/database.md#postgresql-major-upgrades).

Create a dedicated role and database for ferl. The role owns the database and
runs both migrations and the server:

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

`\password` prompts for the password, so it never appears in the SQL. Run these
statements only against a role and database you intend to create. On a managed
database, use the provider's equivalent steps with the same owner and search
path.

Make sure that:

- the application host can reach the database host on port 5432;
- the server certificate matches the hostname in `FERL_DATABASE_URL`;
- your `pg_hba.conf` (or the provider's equivalent) allows SCRAM from the
  application host for the `ferl` role.

## 2. Write the configuration

Create a private directory for ferl's configuration:

```sh
umask 077
mkdir -p "$HOME/.config/ferl"
touch "$HOME/.config/ferl/ferl.env"
chmod 600 "$HOME/.config/ferl/ferl.env"
cp /path/to/db-ca.pem "$HOME/.config/ferl/db-ca.pem"
chmod 644 "$HOME/.config/ferl/db-ca.pem"
```

Generate the cursor key once for this deployment. Keep it with your backups and
reuse it on every restart. Changing it invalidates existing pagination cursors:

```sh
python3 -c 'import base64,secrets; print(base64.urlsafe_b64encode(secrets.token_bytes(32)).rstrip(b"=").decode())'
```

Open `$HOME/.config/ferl/ferl.env` and add these lines, replacing the
placeholders. Write plain `KEY=value` lines with no quotes and no `export`:

```dotenv
FERL_DATABASE_URL=postgres://ferl:URL_ENCODED_PASSWORD@db.example.com:5432/ferl?sslmode=verify-full&sslrootcert=/etc/ferl/db-ca.pem
FERL_PUBLIC_BASE_URL=https://s.example.com
FERL_CURSOR_KEY=REPLACE_WITH_GENERATED_KEY
FERL_ENVIRONMENT=production
FERL_ANONYMOUS_CREATION=false
FERL_SHUTDOWN_TIMEOUT_SECS=45
```

- `FERL_DATABASE_URL`: percent-encode special characters in the password. The
  `sslrootcert` path is the path **inside the container**, which the run command
  below mounts from `db-ca.pem`.
- `FERL_PUBLIC_BASE_URL`: the public HTTPS origin that appears in short URLs.
  Use an origin only: no path, query, or trailing components.

Podman reads this file with `--env-file`. ferl does not read `.env` files by
itself, and you should not `source` this file in a shell.

The complete list of settings and their limits is in
[Configuration](docs/configuration.md).

## 3. Pull the image

Use the digest from the release summary of the version you are installing. The
digest is the immutable reference. Tags such as `v0.1.0` can move only if the
release policy is changed, so do not use them for production:

```sh
FERL_CONFIG="$HOME/.config/ferl"
FERL_IMAGE='ghcr.io/ai-slop-code/ferl@sha256:REPLACE_WITH_RELEASE_DIGEST'

podman pull --platform linux/amd64 "$FERL_IMAGE"
```

The release summary is on the Actions run for the release tag. After the first
release, the package is public and this pull needs no login. Before it is made
public, run `podman login ghcr.io` with a token that has `read:packages`.

Define a helper for one-shot commands in this shell. It runs with the same
restrictions as the server:

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

ferl_once --version
```

The version command should print `ferl-server 0.1.2`, or the version of the
image you pulled.

## 4. Migrate the database

Apply the schema embedded in the image:

```sh
ferl_once migrate
```

Migration must succeed before you bootstrap or start the server. Do not apply
the SQL by hand or edit the migration history.

## 5. Create the first administrator

Run this once. It writes the one-time API key to a private file:

```sh
(
  umask 077
  set -C
  ferl_once bootstrap-admin --username ferl-admin \
    > "$FERL_CONFIG/bootstrap.json"
)
```

- The helper disables container logging, so the key is not stored in a log
  driver.
- `set -C` refuses to overwrite an existing file.
- Copy the `api_key` value into your password manager, then delete
  `bootstrap.json`:

  ```sh
  rm -f "$FERL_CONFIG/bootstrap.json"
  ```

Bootstrap is recorded in PostgreSQL. Running it again does not return the key or
create another administrator. If the key is lost, restore a database backup taken
after bootstrap, following [Release and self-hosting](docs/release.md#upgrade-backup-and-restore).
Do not reset the database to regain access.

## 6. Start the server

For a reverse proxy on the same host, publish the port on loopback only:

```sh
podman run --detach \
  --name ferl \
  --platform linux/amd64 \
  --pull=never \
  --read-only \
  --cap-drop=all \
  --security-opt=no-new-privileges \
  --restart=always \
  --stop-timeout=60 \
  --publish 127.0.0.1:8080:8080 \
  --env-file "$FERL_CONFIG/ferl.env" \
  --volume "$FERL_CONFIG/db-ca.pem:/etc/ferl/db-ca.pem:ro" \
  "$FERL_IMAGE" serve
```

- `--stop-timeout=60` is longer than `FERL_SHUTDOWN_TIMEOUT_SECS`, so the server
  can drain before Podman sends `SIGKILL`.
- On SELinux hosts, the CA mount may need a relabel option such as `:ro,z`.
  Confirm the non-root process can read the file.
- `--restart=always` restarts ferl after a reboot. For a production host, a
  rootless Podman Quadlet unit is the more complete option; the service must keep
  the same image, environment file, and CA mount.

## 7. Verify the installation

Check readiness. It returns `503` until the database and schema are verified:

```sh
curl --fail-with-body http://127.0.0.1:8080/api/health/ready
```

A ready server returns:

```json
{"status":"ok"}
```

`/api/health/live` only confirms the process responds. The root path `/` is not a
health endpoint.

Liveness remains responsive when business-request admission is saturated.
Readiness still uses that admission limit and may return a structured
`service_unavailable` error under overload.

Then create a link with your API key and follow its redirect:

```sh
FERL_API_KEY='PASTE_THE_API_KEY_HERE'
FERL_URL='http://127.0.0.1:8080'

curl --fail-with-body "$FERL_URL/api/v1/links" \
  -H "Authorization: Bearer $FERL_API_KEY" \
  -H 'Content-Type: application/json' \
  --data '{"destination_url":"https://example.com/docs","short_code":"my-docs"}'

curl --head "$FERL_URL/my-docs"
```

The redirect returns `302` with the destination in `Location`.

## Put ferl behind HTTPS

ferl serves plain HTTP internally. Your reverse proxy terminates TLS and forwards
requests to `127.0.0.1:8080`:

- `FERL_PUBLIC_BASE_URL` sets the origin in returned short URLs. It does not
  configure DNS and does not terminate TLS.
- Forward `/api/` and your short-link paths to ferl. Keep the proxy's routing
  exact to the paths in [the OpenAPI contract](spec/openapi.yaml).
- If the proxy runs on another host or network, set `FERL_TRUSTED_PROXIES` to
  its exact addresses or CIDRs, and add it to the run command's environment
  file. ferl does not trust any private network by default.

The deployment examples are in [Release and self-hosting](docs/release.md).

## Use the API

The full interface is in [the OpenAPI contract](spec/openapi.yaml). These
commands use the admin key from step 5.

**List your links:**

```sh
curl --fail-with-body "$FERL_URL/api/v1/links" \
  -H "Authorization: Bearer $FERL_API_KEY"
```

**Read visit counts for a link:**

```sh
curl --fail-with-body "$FERL_URL/api/v1/links/my-docs/stats" \
  -H "Authorization: Bearer $FERL_API_KEY"
```

Visits are recorded asynchronously, so counts can take a moment to appear.
Admins create other accounts through `POST /api/v1/admin/accounts`.

## Upgrade, back up, and restore

- **Back up** PostgreSQL regularly. Store the cursor key and the CA file
  separately from the database backup.
- **Upgrade** by recording the current image digest, backing up the database,
  reading the release notes for migration compatibility, stopping the old
  container if the release requires it, running `migrate` with the new digest,
  and starting the new container. Wait for readiness before sending traffic.
- **Roll back** by restoring a compatible database backup. An image rollback
  alone does not undo a schema migration.

Full procedures are in [Release and self-hosting](docs/release.md).

## Building from source

If you cannot use the published image, build the server with the pinned Rust
toolchain. [Development](docs/development.md) describes the toolchain, local
PostgreSQL fixtures, and the verification commands.

From the repository root, with Rustup, a native C/C++ toolchain, CMake, and
pkg-config installed:

```sh
rustup toolchain install 1.99.1 --profile minimal --component rustfmt --component clippy
cargo build --locked --release --bin ferl-server
./target/release/ferl-server --version
```

The executable is `target/release/ferl-server`; migrations are embedded. Configure
the process environment using [Configuration](docs/configuration.md), then run
its separate `migrate`, `bootstrap-admin`, and `serve` commands as described in
[Release and self-hosting](docs/release.md#first-startup).

## License

ferl is released under the [MIT License](LICENSE).

## Documentation

| Guide | Contents |
| --- | --- |
| [Configuration](docs/configuration.md) | Every setting, its default, and its limits. |
| [Release and self-hosting](docs/release.md) | Image and archive verification, upgrades, backups, and recovery. |
| [CI/CD and container delivery](docs/cicd.md) | How the image is built, tested, and published. |
| [Database](docs/database.md) | PostgreSQL requirements, schema versions, and migrations. |
| [Operations](docs/operations.md) | Health checks, shutdown, and telemetry. |
| [Analytics reports](docs/analytics-reports.md) | Report queries, pagination, and cursors. |
| [Development](docs/development.md) | Toolchain, local fixtures, and verification commands. |
| [OpenAPI](spec/openapi.yaml) | The machine-readable HTTP API contract. |
