#!/bin/sh
set -eu

if [ "${FERL_POSTGRES_LOCK_HELD:-0}" != 1 ]; then
    command -v python3 >/dev/null 2>&1 || {
        echo "python3 is required to coordinate PostgreSQL harness operations" >&2
        exit 1
    }
    python3 - "$0" "$@" <<'PY'
import fcntl
import os
import subprocess
import sys

lock_path = os.environ.get("FERL_TEST_HARNESS_LOCK_PATH")
if not lock_path:
    lock_path = os.path.join(os.environ.get("TMPDIR", "/tmp"), "ferl-stage01-postgres.lock")
with open(lock_path, "a+") as lock:
    fcntl.flock(lock.fileno(), fcntl.LOCK_EX)
    env = dict(os.environ)
    env["FERL_POSTGRES_LOCK_HELD"] = "1"
    result = subprocess.run(["sh", sys.argv[1], *sys.argv[2:]], env=env, check=False)
    raise SystemExit(result.returncode)
PY
    exit $?
fi

NAME=ferl-stage01-postgres
VOLUME=ferl-stage01-postgres-data
IMAGE=docker.io/library/postgres:17.6-alpine3.22@sha256:ef257d85f76e48da1c64832459b59fcaba1a4dac97bf5d7450c77753542eee94
PORT=${FERL_POSTGRES_PORT:-55432}
LABEL_KEY=io.ferl.harness

case "$PORT" in
    ''|*[!0-9]*)
        echo "FERL_POSTGRES_PORT must be an integer from 1024 through 65535" >&2
        exit 2
        ;;
esac
if [ "$PORT" -lt 1024 ] || [ "$PORT" -gt 65535 ]; then
    echo "FERL_POSTGRES_PORT must be an integer from 1024 through 65535" >&2
    exit 2
fi

container_exists() {
    podman container exists "$NAME"
}

volume_exists() {
    podman volume exists "$VOLUME"
}

assert_container_owned() {
    label=$(podman inspect --format "{{ index .Config.Labels \"$LABEL_KEY\" }}" "$NAME")
    [ "$label" = stage01 ] || {
        echo "refusing to operate on a container not owned by the Stage 01 harness" >&2
        exit 1
    }
}

assert_volume_owned() {
    label=$(podman volume inspect --format "{{ index .Labels \"$LABEL_KEY\" }}" "$VOLUME")
    [ "$label" = stage01 ] || {
        echo "refusing to operate on a volume not owned by the Stage 01 harness" >&2
        exit 1
    }
}

assert_container_configuration() {
    expected_digest=${IMAGE##*@}
    actual_digest=$(podman inspect --format '{{.ImageDigest}}' "$NAME")
    [ "$actual_digest" = "$expected_digest" ] || {
        echo "refusing to reuse a Stage 01 container with an unexpected image digest" >&2
        exit 1
    }
    mount=$(podman inspect --format '{{range .Mounts}}{{.Name}}:{{.Destination}}{{end}}' "$NAME")
    [ "$mount" = "$VOLUME:/var/lib/postgresql/data" ] || {
        echo "refusing to reuse a Stage 01 container without its persistent volume" >&2
        exit 1
    }
    assert_volume_owned
    published=$(podman port "$NAME" 5432/tcp | tail -n 1)
    [ "$published" = "127.0.0.1:$PORT" ] || {
        echo "harness container publishes $published, not 127.0.0.1:$PORT; choose its original port or clean up and recreate it" >&2
        exit 1
    }
}

wait_ready() {
    attempts=0
    while [ "$attempts" -lt 30 ]; do
        if podman exec "$NAME" pg_isready -q -t 1 -U ferl -d ferl_test; then
            echo "PostgreSQL container is accepting connections; host connection is verified by the Rust integration test."
            return 0
        fi
        attempts=$((attempts + 1))
        sleep 1
    done
    echo "PostgreSQL did not become ready within approximately 60 seconds" >&2
    return 1
}

pull_image() {
    # TLS verification is disabled only for this Podman image pull in the
    # owner-authorized corporate TLS-interception environment. Application
    # database and HTTP TLS verification settings are not changed.
    podman pull --tls-verify=false "$IMAGE"
}

case "${1:-}" in
    pull)
        podman info >/dev/null
        pull_image
        ;;
    start)
        podman info >/dev/null
        if container_exists; then
            assert_container_owned
            assert_container_configuration
            running=$(podman inspect --format '{{ .State.Running }}' "$NAME")
            if [ "$running" != true ]; then
                podman start "$NAME" >/dev/null
            fi
        else
            if volume_exists; then
                assert_volume_owned
            else
                podman volume create --label "$LABEL_KEY=stage01" "$VOLUME" >/dev/null
            fi
            if ! podman image exists "$IMAGE"; then
                pull_image
            fi
            podman run --pull=never --detach \
                --name "$NAME" \
                --label "$LABEL_KEY=stage01" \
                --publish "127.0.0.1:$PORT:5432" \
                --volume "$VOLUME:/var/lib/postgresql/data" \
                --env POSTGRES_USER=ferl \
                --env POSTGRES_PASSWORD=ferl \
                --env POSTGRES_DB=ferl_test \
                "$IMAGE" >/dev/null
        fi
        ;;
    ready)
        container_exists || {
            echo "PostgreSQL harness container is not present; run 'sh scripts/postgres.sh start'" >&2
            exit 1
        }
        assert_container_owned
        assert_container_configuration
        wait_ready
        ;;
    stop)
        if container_exists; then
            assert_container_owned
            running=$(podman inspect --format '{{ .State.Running }}' "$NAME")
            if [ "$running" = true ]; then
                podman stop "$NAME" >/dev/null
            fi
        fi
        echo "PostgreSQL harness container stopped; persistent volume preserved."
        ;;
    cleanup)
        if container_exists; then
            assert_container_owned
            podman rm --force "$NAME" >/dev/null
        fi
        if volume_exists; then
            assert_volume_owned
            podman volume rm "$VOLUME" >/dev/null
        fi
        echo "Removed only Stage 01 harness-owned PostgreSQL resources."
        ;;
    *)
        echo "usage: $0 {pull|start|ready|stop|cleanup}" >&2
        exit 2
        ;;
esac
