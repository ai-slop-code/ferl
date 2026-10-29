#!/bin/sh
set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)
RUN_ID=${FERL_PROXY_RUN_ID:-manual}
case "$RUN_ID" in
    ''|*[!A-Za-z0-9_.-]*)
        echo "FERL_PROXY_RUN_ID must contain only ASCII letters, digits, dot, underscore, or hyphen" >&2
        exit 2
        ;;
esac

NAME=${FERL_PROXY_NAME:-ferl-part11-proxy-$RUN_ID}
PORT=${FERL_PROXY_PORT:-8081}
BACKEND_PORT=${FERL_PROXY_BACKEND_PORT:-8080}
BACKEND_HOST=${FERL_PROXY_BACKEND_HOST:-ferl-backend}
NETWORK=${FERL_PROXY_NETWORK:-}
IMAGE=docker.io/library/nginx:1.30.6-alpine@sha256:47928bf67eb62a1863cb08d9ecb52d3e704304ba614eaee17884204677702951
LABEL_KEY=io.ferl.harness
RUNTIME_DIR=${FERL_PROXY_RUNTIME_DIR:-"$ROOT/target/proxy/$RUN_ID"}

for value in "$PORT" "$BACKEND_PORT"; do
    case "$value" in ''|*[!0-9]*) echo "proxy ports must be numeric" >&2; exit 2 ;; esac
    if [ "$value" -lt 1024 ] || [ "$value" -gt 65535 ]; then
        echo "proxy ports must be from 1024 through 65535" >&2
        exit 2
    fi
done

container_exists() { podman container exists "$NAME"; }

assert_owned() {
    label=$(podman inspect --format "{{ index .Config.Labels \"$LABEL_KEY\" }}" "$NAME")
    [ "$label" = part11-proxy ] || {
        echo "refusing to operate on a container not owned by the Part 11 proxy harness" >&2
        exit 1
    }
}

assert_configuration() {
    expected_digest=${IMAGE##*@}
    actual_digest=$(podman inspect --format '{{.ImageDigest}}' "$NAME")
    [ "$actual_digest" = "$expected_digest" ] || {
        echo "refusing to reuse proxy with an unexpected image digest" >&2
        exit 1
    }
    published=$(podman port "$NAME" 8080/tcp | tail -n 1)
    [ "$published" = "127.0.0.1:$PORT" ] || {
        echo "proxy publishes $published, not 127.0.0.1:$PORT" >&2
        exit 1
    }
    mounts=$(podman inspect --format '{{range .Mounts}}{{.Destination}} {{end}}' "$NAME")
    case "$mounts" in
        *"/etc/nginx/conf.d"*"/etc/nginx/ferl-proxy-headers.conf"*"/usr/share/nginx/html"*) ;;
        *) echo "proxy is missing the checked-in routing config/static fixture mounts" >&2; exit 1 ;;
    esac
}

render_config() {
    mkdir -p "$RUNTIME_DIR/conf.d"
    case "$BACKEND_HOST" in *[!A-Za-z0-9_.-]*) echo "invalid proxy backend hostname" >&2; exit 2 ;; esac
    sed "s/ferl-backend:8080/$BACKEND_HOST:$BACKEND_PORT/" \
        "$ROOT/deploy/proxy/nginx.conf" > "$RUNTIME_DIR/conf.d/default.conf"
    cp "$ROOT/deploy/proxy/ferl-proxy-headers.conf" "$RUNTIME_DIR/ferl-proxy-headers.conf"
}

wait_ready() {
    attempts=0
    while [ "$attempts" -lt 20 ]; do
        if curl --silent --show-error --fail --max-time 1 "http://127.0.0.1:$PORT/" \
            | grep -q 'Separate static fixture'; then
            echo "proxy is serving the separate static fixture over TCP"
            return 0
        fi
        attempts=$((attempts + 1))
        sleep 1
    done
    echo "proxy did not become ready within 20 seconds" >&2
    return 1
}

case "${1:-}" in
    pull)
        podman info >/dev/null
        podman image exists "$IMAGE" || podman pull "$IMAGE"
        ;;
    start)
        podman info >/dev/null
        render_config
        if container_exists; then
            assert_owned
            assert_configuration
            running=$(podman inspect --format '{{.State.Running}}' "$NAME")
            if [ "$running" != true ]; then podman start "$NAME" >/dev/null; fi
        else
            podman image exists "$IMAGE" || {
                echo "pinned NGINX image is absent; run 'sh scripts/proxy.sh pull' first" >&2
                exit 1
            }
            if [ -n "$NETWORK" ]; then
                podman run --pull=never --detach \
                    --name "$NAME" \
                    --label "$LABEL_KEY=part11-proxy" \
                    --network "$NETWORK" \
                    --publish "127.0.0.1:$PORT:8080" \
                    --volume "$RUNTIME_DIR/conf.d:/etc/nginx/conf.d:ro" \
                    --volume "$RUNTIME_DIR/ferl-proxy-headers.conf:/etc/nginx/ferl-proxy-headers.conf:ro" \
                    --volume "$ROOT/deploy/proxy/static:/usr/share/nginx/html:ro" \
                    "$IMAGE" >/dev/null
            else
                podman run --pull=never --detach \
                    --name "$NAME" \
                    --label "$LABEL_KEY=part11-proxy" \
                    --publish "127.0.0.1:$PORT:8080" \
                    --add-host host.containers.internal:host-gateway \
                    --volume "$RUNTIME_DIR/conf.d:/etc/nginx/conf.d:ro" \
                    --volume "$RUNTIME_DIR/ferl-proxy-headers.conf:/etc/nginx/ferl-proxy-headers.conf:ro" \
                    --volume "$ROOT/deploy/proxy/static:/usr/share/nginx/html:ro" \
                    "$IMAGE" >/dev/null
            fi
        fi
        ;;
    ready)
        container_exists || { echo "proxy container is absent; run 'sh scripts/proxy.sh start'" >&2; exit 1; }
        assert_owned
        assert_configuration
        wait_ready
        ;;
    reload)
        container_exists || { echo "proxy container is absent; run 'sh scripts/proxy.sh start'" >&2; exit 1; }
        render_config
        assert_owned
        assert_configuration
        podman exec "$NAME" nginx -t
        podman exec "$NAME" nginx -s reload
        wait_ready
        ;;
    stop)
        if container_exists; then
            assert_owned
            podman stop --time 5 "$NAME" >/dev/null
        fi
        echo "Part 11 proxy container stopped; runtime config and evidence are preserved."
        ;;
    cleanup)
        if container_exists; then
            assert_owned
            podman rm --force "$NAME" >/dev/null
        fi
        echo "Removed only the Part 11 proxy container; test captures/config remain under $RUNTIME_DIR."
        ;;
    *)
        echo "usage: $0 {pull|start|ready|reload|stop|cleanup}" >&2
        exit 2
        ;;
esac
