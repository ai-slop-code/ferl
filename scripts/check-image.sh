#!/bin/sh
set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)
METADATA=${1:?usage: check-image.sh <image-metadata.json>}
METADATA=$(CDPATH= cd -- "$(dirname "$METADATA")" && pwd)/$(basename -- "$METADATA")
OUT_DIR=$(dirname -- "$METADATA")
. "$SCRIPT_DIR/postgres-fixtures.sh"
PG_IMAGE=$FERL_PG_IMAGE
SUMMARY="check-summary-postgresql-$FERL_TEST_POSTGRES_MAJOR.json"

field() {
    python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))[sys.argv[2]])' "$METADATA" "$1"
}

VERSION=$(field version)
REVISION=$(field source_revision)
PLATFORM=$(field platform)
IMAGE_ID=$(field image_id)
BINARY_SHA256=$(field binary_sha256)
ARCHIVE="$OUT_DIR/$(field image_archive)"
RUN_ID=$(python3 -c 'import uuid; print(uuid.uuid4().hex)')
LABEL="io.ferl.check-run=$RUN_ID"
WORK="$OUT_DIR/check-$RUN_ID"
IMAGE="ferl-check:$RUN_ID"
NET="ferl-check-net-$RUN_ID"
DB_ALIAS="ferl-check-db"
DB_WRONG_ALIAS="ferl-check-wrong"
DB_NAME="ferl-check-postgres-$RUN_ID"
APP="ferl-check-app-$RUN_ID"
APP_RESTART="ferl-check-app-restart-$RUN_ID"
ADMIN="ferl-check-admin"
CHECKS=""

umask 077
mkdir -p "$WORK/ca" "$WORK/untrusted-ca" "$WORK/db-tls"
chmod 0755 "$WORK/ca" "$WORK/untrusted-ca" "$WORK/db-tls"

cleanup() {
    podman ps -aq --filter "label=$LABEL" | xargs -r podman rm -f -t 0 >/dev/null 2>&1 || true
    podman network rm -f "$NET" >/dev/null 2>&1 || true
    podman rmi -f "$IMAGE" >/dev/null 2>&1 || true
    rm -rf "$WORK"
}
trap cleanup EXIT

pass() {
    CHECKS="$CHECKS $1"
    echo "check passed: $1"
}

fail() {
    echo "check failed: $1" >&2
    exit 1
}

show_log() {
    python3 -c 'import sys
password = open(sys.argv[2]).read().strip()
text = open(sys.argv[1], errors="replace").read().replace(password, "<redacted>")
print(text[-3000:], file=sys.stderr)' "$1" "$PASSWORD_FILE" || true
}

podman info >/dev/null
podman pull --platform linux/amd64 "$PG_IMAGE" >/dev/null

python3 - "$ARCHIVE" "$ARCHIVE.sha256" "$(field image_archive_sha256)" <<'PY'
import hashlib
from pathlib import Path
import sys

archive, sidecar, expected = sys.argv[1:]
if hashlib.sha256(Path(archive).read_bytes()).hexdigest() != expected:
    raise SystemExit("OCI archive does not match image metadata")
if Path(sidecar).read_text().split()[0] != expected:
    raise SystemExit("OCI archive sidecar does not match image metadata")
PY
pass "oci-archive-checksum"

podman load --quiet -i "$ARCHIVE" >/dev/null
podman tag "$IMAGE_ID" "$IMAGE"
[ "$(podman image inspect --format '{{.Id}}' "$IMAGE")" = "$IMAGE_ID" ] || fail "loaded image ID differs from metadata"
pass "loaded-candidate-identity"

[ "$(podman image inspect --format '{{.Os}}/{{.Architecture}}' "$IMAGE")" = "$PLATFORM" ] || fail "image platform"
[ "$(podman image inspect --format '{{index .Config.Labels "org.opencontainers.image.revision"}}' "$IMAGE")" = "$REVISION" ] || fail "revision label"
[ "$(podman image inspect --format '{{index .Config.Labels "org.opencontainers.image.version"}}' "$IMAGE")" = "$VERSION" ] || fail "version label"
[ "$(podman image inspect --format '{{index .Config.Labels "org.opencontainers.image.source"}}' "$IMAGE")" = "https://github.com/ai-slop-code/ferl" ] || fail "source label"
[ "$(podman image inspect --format '{{.Config.User}}' "$IMAGE")" = "65532:65532" ] || fail "image user"
pass "image-metadata"

CID=$(podman create --label "$LABEL" "$IMAGE")
podman cp "$CID:/usr/local/bin/ferl-server" "$WORK/ferl-server" >/dev/null
podman rm -f "$CID" >/dev/null
python3 - "$WORK/ferl-server" "$BINARY_SHA256" <<'PY'
import hashlib
from pathlib import Path
import struct
import sys

path, expected = sys.argv[1:]
data = Path(path).read_bytes()
if data[:4] != b"\x7fELF" or data[4] != 2:
    raise SystemExit("packaged executable is not a 64-bit ELF file")
machine = struct.unpack_from("<H", data, 18)[0]
if machine != 62:
    raise SystemExit("packaged executable is not x86_64")
if hashlib.sha256(data).hexdigest() != expected:
    raise SystemExit("image executable differs from the verified release executable")
PY
pass "x86_64-elf-and-binary-digest"

RUN_FLAGS="--pull=never --platform linux/amd64 --label $LABEL --user 65532:65532 --read-only --cap-drop=all --security-opt=no-new-privileges --tmpfs /tmp:rw,noexec,nosuid,nodev,size=16m"

VERSION_OUTPUT=$(podman run --rm $RUN_FLAGS "$IMAGE" --version)
[ "$VERSION_OUTPUT" = "ferl-server $VERSION" ] || fail "version output"
pass "version-output"

openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes \
    -keyout "$WORK/ca/root.key" -out "$WORK/ca/root-ca.pem" -days 1 -sha256 \
    -subj "/CN=ferl-check-root" \
    -addext "basicConstraints=critical,CA:TRUE" -addext "keyUsage=critical,keyCertSign,cRLSign" >/dev/null 2>&1
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes \
    -keyout "$WORK/untrusted-ca/root.key" -out "$WORK/untrusted-ca/root-ca.pem" -days 1 -sha256 \
    -subj "/CN=ferl-check-untrusted" \
    -addext "basicConstraints=critical,CA:TRUE" -addext "keyUsage=critical,keyCertSign,cRLSign" >/dev/null 2>&1
openssl req -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes \
    -keyout "$WORK/db-tls/server.key" -out "$WORK/db-tls/server.csr" \
    -subj "/CN=$DB_ALIAS" >/dev/null 2>&1
printf 'subjectAltName=DNS:%s\nbasicConstraints=CA:FALSE\nkeyUsage=digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\n' "$DB_ALIAS" > "$WORK/db-tls/server.ext"
openssl x509 -req -in "$WORK/db-tls/server.csr" -CA "$WORK/ca/root-ca.pem" -CAkey "$WORK/ca/root.key" \
    -CAcreateserial -out "$WORK/db-tls/server.crt" -days 1 -sha256 -extfile "$WORK/db-tls/server.ext" >/dev/null 2>&1
cp "$WORK/ca/root-ca.pem" "$WORK/db-tls/root-ca.pem"
chmod 0644 "$WORK/ca/root-ca.pem" "$WORK/untrusted-ca/root-ca.pem" "$WORK/db-tls/server.crt" "$WORK/db-tls/root-ca.pem"
chmod 0600 "$WORK/db-tls/server.key"
pass "synthetic-tls-fixture"

PASSWORD_FILE="$WORK/db-password"
openssl rand -hex 24 > "$PASSWORD_FILE"
chmod 0600 "$PASSWORD_FILE"
DB_PASSWORD=$(cat "$PASSWORD_FILE")

podman network create --label "$LABEL" "$NET" >/dev/null
podman run --detach --name "$DB_NAME" --label "$LABEL" --platform linux/amd64 \
    --network "$NET" --network-alias "$DB_ALIAS" --network-alias "$DB_WRONG_ALIAS" \
    --env POSTGRES_USER=ferl --env POSTGRES_DB=ferl \
    --env POSTGRES_PASSWORD_FILE=/run/secrets/db-password \
    --volume "$PASSWORD_FILE:/run/secrets/db-password:ro" \
    --volume "$WORK/db-tls:/tls-src:ro" \
    --entrypoint /bin/sh "$PG_IMAGE" -ec '
        mkdir -p /tls-local
        cp /tls-src/server.crt /tls-src/server.key /tls-local/
        chown postgres:postgres /tls-local/server.crt /tls-local/server.key
        chmod 0600 /tls-local/server.key
        exec docker-entrypoint.sh postgres -c ssl=on -c ssl_cert_file=/tls-local/server.crt -c ssl_key_file=/tls-local/server.key
    ' >/dev/null

READY=0
for _ in $(seq 1 90); do
    if podman exec "$DB_NAME" pg_isready -h 127.0.0.1 -U ferl -d ferl >/dev/null 2>&1; then
        READY=1
        break
    fi
    sleep 1
done
[ "$READY" = 1 ] || fail "PostgreSQL fixture did not become ready"
[ "$(podman exec "$DB_NAME" psql -h 127.0.0.1 -U ferl -d ferl -X -tAc 'SHOW server_version_num' | cut -c1-2)" = "$FERL_TEST_POSTGRES_MAJOR" ] || fail "fixture is not PostgreSQL $FERL_TEST_POSTGRES_MAJOR"
[ "$(podman exec "$DB_NAME" psql -h 127.0.0.1 -U ferl -d ferl -X -tAc 'SHOW ssl')" = "on" ] || fail "fixture TLS is off"
DB_SERVER_VERSION=$(podman exec "$DB_NAME" psql -h 127.0.0.1 -U ferl -d ferl -X -tAc 'SHOW server_version')
pass "postgresql-$FERL_TEST_POSTGRES_MAJOR-tls-fixture"

CURSOR_KEY=$(python3 -c 'import base64,secrets; print(base64.urlsafe_b64encode(secrets.token_bytes(32)).rstrip(b"=").decode())')
write_env() {
    host=$1
    {
        printf 'FERL_DATABASE_URL=postgres://ferl:%s@%s:5432/ferl?sslmode=verify-full&sslrootcert=/tls/root-ca.pem\n' "$DB_PASSWORD" "$host"
        printf 'FERL_PUBLIC_BASE_URL=https://links.example.test\n'
        printf 'FERL_CURSOR_KEY=%s\n' "$CURSOR_KEY"
        printf 'FERL_ANONYMOUS_CREATION=false\n'
        printf 'FERL_ENVIRONMENT=production\n'
        printf 'FERL_SHUTDOWN_TIMEOUT_SECS=45\n'
    } > "$2"
    chmod 0600 "$2"
}
write_env "$DB_ALIAS" "$WORK/ferl.env"
write_env "$DB_WRONG_ALIAS" "$WORK/ferl-wrong-host.env"
unset DB_PASSWORD CURSOR_KEY

ferl_once() {
    env_file=$1
    ca_dir=$2
    shift 2
    podman run --rm $RUN_FLAGS --log-driver=none --network "$NET" \
        --env-file "$env_file" --volume "$ca_dir:/tls:ro" "$IMAGE" "$@"
}

ferl_once "$WORK/ferl.env" "$WORK/ca" migrate >"$WORK/migrate.log" 2>&1 || { show_log "$WORK/migrate.log"; fail "migrate over verified TLS"; }
ferl_once "$WORK/ferl.env" "$WORK/ca" migrate >"$WORK/migrate-again.log" 2>&1 || fail "repeated migrate is idempotent"
pass "migrate-fresh-database"

if ferl_once "$WORK/ferl.env" "$WORK/untrusted-ca" migrate >"$WORK/negative-untrusted.log" 2>&1; then
    fail "untrusted PostgreSQL CA was accepted"
fi
pass "negative-untrusted-ca-rejected"

if ferl_once "$WORK/ferl-wrong-host.env" "$WORK/ca" migrate >"$WORK/negative-hostname.log" 2>&1; then
    fail "mismatched PostgreSQL hostname was accepted"
fi
pass "negative-hostname-rejected"

for log in "$WORK"/*.log; do
    if grep -q -F "$(cat "$PASSWORD_FILE")" "$log"; then
        fail "a diagnostic log contains the database password"
    fi
done
pass "logs-free-of-database-password"

ferl_once "$WORK/ferl.env" "$WORK/ca" bootstrap-admin --username "$ADMIN" >"$WORK/bootstrap.json" 2>"$WORK/bootstrap.err" || fail "bootstrap-admin"
python3 - "$WORK/bootstrap.json" "$WORK/auth-header" <<'PY'
import json
from pathlib import Path
import sys

source, header = sys.argv[1:]
text = Path(source).read_text()
if text.count("\n") != 1:
    raise SystemExit("bootstrap output is not one JSON line")
result = json.loads(text)
key = result["api_key"]
if not key.startswith("ferl_ak_v1_"):
    raise SystemExit("bootstrap returned an unexpected key format")
Path(header).write_text(f"Authorization: Bearer {key}\n")
PY
chmod 0600 "$WORK/auth-header"
pass "bootstrap-admin-once"

if ferl_once "$WORK/ferl.env" "$WORK/ca" bootstrap-admin --username "$ADMIN" >/dev/null 2>&1; then
    fail "repeated bootstrap-admin succeeded"
fi
pass "repeated-bootstrap-refused"

start_server() {
    name=$1
    podman run --detach --name "$name" --label "$LABEL" $RUN_FLAGS --log-driver=none \
        --network "$NET" --publish 127.0.0.1::8080 --stop-timeout=60 \
        --env-file "$WORK/ferl.env" --volume "$WORK/ca:/tls:ro" "$IMAGE" serve >/dev/null
}

wait_ready() {
    name=$1
    port=$(podman port "$name" 8080/tcp | head -n 1 | sed 's/.*://')
    for _ in $(seq 1 60); do
        if curl --silent --fail --max-time 2 "http://127.0.0.1:$port/api/health/ready" | grep -q '"status":"ok"'; then
            echo "$port"
            return 0
        fi
        sleep 1
    done
    return 1
}

start_server "$APP"
PORT=$(wait_ready "$APP") || fail "server did not become ready"
pass "server-ready-as-nonroot"

SLUG="check-$(openssl rand -hex 4)"
DESTINATION="https://example.com/ferl-check-destination"
STATUS=$(curl --silent --output "$WORK/create.json" --write-out '%{http_code}' \
    --header @"$WORK/auth-header" --header 'Content-Type: application/json' \
    --data "{\"destination_url\":\"$DESTINATION\",\"short_code\":\"$SLUG\"}" \
    "http://127.0.0.1:$PORT/api/v1/links")
[ "$STATUS" = "201" ] || fail "link creation returned $STATUS"
pass "link-created"

REDIRECT=$(curl --silent --output /dev/null --dump-header - "http://127.0.0.1:$PORT/$SLUG" | tr -d '\r')
printf '%s\n' "$REDIRECT" | head -n 1 | grep -q ' 302 ' || fail "redirect status"
printf '%s\n' "$REDIRECT" | grep -q -i "^location: $DESTINATION\$" || fail "redirect destination"
pass "redirect-302-exact-location"

START=$(date +%s)
podman stop --time 60 "$APP" >/dev/null
ELAPSED=$(( $(date +%s) - START ))
[ "$(podman inspect --format '{{.State.ExitCode}}' "$APP")" = "0" ] || fail "SIGTERM did not produce a clean exit"
[ "$ELAPSED" -le 50 ] || fail "graceful shutdown exceeded the configured budget"
pass "graceful-sigterm-exit"

start_server "$APP_RESTART"
PORT=$(wait_ready "$APP_RESTART") || fail "restarted server did not become ready"
REDIRECT=$(curl --silent --output /dev/null --dump-header - "http://127.0.0.1:$PORT/$SLUG" | tr -d '\r')
printf '%s\n' "$REDIRECT" | grep -q -i "^location: $DESTINATION\$" || fail "link did not persist across restart"
pass "restart-persistence-without-reseed"

podman stop --time 60 "$APP_RESTART" >/dev/null
podman rm -f "$APP" "$APP_RESTART" >/dev/null 2>&1 || true

python3 - "$METADATA" "$OUT_DIR/$SUMMARY" "$CHECKS" "$FERL_TEST_POSTGRES_MAJOR" "$DB_SERVER_VERSION" "$PG_IMAGE" <<'PY'
import json
from pathlib import Path
import sys

metadata_path, summary_path, checks, major, server_version, pg_image = sys.argv[1:]
metadata = json.loads(Path(metadata_path).read_text())
summary = {
    "result": "passed",
    "image_id": metadata["image_id"],
    "manifest_digest": metadata["manifest_digest"],
    "source_revision": metadata["source_revision"],
    "version": metadata["version"],
    "platform": metadata["platform"],
    "postgresql_major": int(major),
    "postgresql_server_version": server_version,
    "postgresql_image": pg_image,
    "checks": checks.split(),
}
Path(summary_path).write_text(json.dumps(summary, indent=2, sort_keys=True) + "\n")
PY
echo "candidate image acceptance passed for PostgreSQL $FERL_TEST_POSTGRES_MAJOR: $OUT_DIR/$SUMMARY"
