#!/bin/sh
set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
. "$SCRIPT_DIR/postgres-fixtures.sh"

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

NAME=$FERL_PG_NAME
VOLUME=$FERL_PG_VOLUME
MOUNT=$FERL_PG_MOUNT
IMAGE=$FERL_PG_IMAGE
PORT=$FERL_PG_PORT
LABEL_KEY=io.ferl.harness

container_exists() {
    podman container exists "$NAME"
}

volume_exists() {
    podman volume exists "$VOLUME"
}

assert_container_owned() {
    label=$(podman inspect --format "{{ index .Config.Labels \"$LABEL_KEY\" }}" "$NAME")
    [ "$label" = "$FERL_PG_LABEL" ] || {
        echo "refusing to operate on a container not owned by the $FERL_PG_LABEL harness" >&2
        exit 1
    }
}

assert_volume_owned() {
    label=$(podman volume inspect --format "{{ index .Labels \"$LABEL_KEY\" }}" "$VOLUME")
    [ "$label" = "$FERL_PG_LABEL" ] || {
        echo "refusing to operate on a volume not owned by the $FERL_PG_LABEL harness" >&2
        exit 1
    }
}

assert_container_configuration() {
    expected_digest=${IMAGE##*@}
    actual_digest=$(podman inspect --format '{{.ImageDigest}}' "$NAME")
    [ "$actual_digest" = "$expected_digest" ] || {
        echo "refusing to reuse a $NAME container with an unexpected image digest" >&2
        exit 1
    }
    mount=$(podman inspect --format '{{range .Mounts}}{{.Name}}:{{.Destination}}{{end}}' "$NAME")
    [ "$mount" = "$VOLUME:$MOUNT" ] || {
        echo "refusing to reuse a $NAME container without its persistent volume" >&2
        exit 1
    }
    assert_volume_owned
    published=$(podman inspect --format '{{index .HostConfig.PortBindings "5432/tcp"}}' "$NAME")
    [ "$published" = "[{127.0.0.1 $PORT}]" ] || {
        echo "harness container publishes $published, not 127.0.0.1:$PORT; choose its original port or clean up and recreate it" >&2
        exit 1
    }
}

wait_ready() {
    attempts=0
    while [ "$attempts" -lt 30 ]; do
        if podman exec "$NAME" pg_isready -q -t 1 -U ferl -d ferl_test; then
            echo "PostgreSQL $FERL_TEST_POSTGRES_MAJOR container is accepting connections; host connection is verified by the Rust integration test."
            return 0
        fi
        attempts=$((attempts + 1))
        sleep 1
    done
    echo "PostgreSQL did not become ready within approximately 60 seconds" >&2
    return 1
}

pull_image() {
    podman pull "$IMAGE"
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
                podman volume create --label "$LABEL_KEY=$FERL_PG_LABEL" "$VOLUME" >/dev/null
            fi
            if ! podman image exists "$IMAGE"; then
                pull_image
            fi
            podman run --pull=never --detach \
                --name "$NAME" \
                --label "$LABEL_KEY=$FERL_PG_LABEL" \
                --publish "127.0.0.1:$PORT:5432" \
                --volume "$VOLUME:$MOUNT" \
                --env POSTGRES_USER=ferl \
                --env POSTGRES_PASSWORD=ferl \
                --env POSTGRES_DB=ferl_test \
                "$IMAGE" >/dev/null
        fi
        ;;
    ready)
        container_exists || {
            echo "PostgreSQL harness container $NAME is not present; run 'FERL_TEST_POSTGRES_MAJOR=$FERL_TEST_POSTGRES_MAJOR sh scripts/postgres.sh start'" >&2
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
        echo "PostgreSQL $FERL_TEST_POSTGRES_MAJOR harness container stopped; persistent volume preserved."
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
        echo "Removed only $FERL_PG_LABEL harness-owned PostgreSQL $FERL_TEST_POSTGRES_MAJOR resources."
        ;;
    *)
        echo "usage: $0 {pull|start|ready|stop|cleanup}" >&2
        exit 2
        ;;
esac
