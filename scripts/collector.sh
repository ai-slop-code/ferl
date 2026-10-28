#!/bin/sh
set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)
RUN_ID=${FERL_COLLECTOR_RUN_ID:-default}
case "$RUN_ID" in
    ''|*[!A-Za-z0-9_.-]*)
        echo "FERL_COLLECTOR_RUN_ID must contain only ASCII letters, digits, dot, underscore, or hyphen" >&2
        exit 2
        ;;
esac

NAME=${FERL_COLLECTOR_NAME:-ferl-stage10-collector-$RUN_ID}
PORT=${FERL_COLLECTOR_PORT:-4318}
IMAGE=docker.io/otel/opentelemetry-collector-contrib@sha256:340c9ca6a36617084c6661bdc4f0fe92b7cda5a805d61673ec8db375fddc522b
LABEL_KEY=io.ferl.harness
CAPTURE_DIR=${FERL_COLLECTOR_CAPTURE_DIR:-"$ROOT/target/collector-captures/$RUN_ID"}
CONFIG="$ROOT/deploy/collector/otelcol.yaml"

case "$PORT" in
    ''|*[!0-9]*) echo "FERL_COLLECTOR_PORT must be an integer from 1024 through 65535" >&2; exit 2 ;;
esac
if [ "$PORT" -lt 1024 ] || [ "$PORT" -gt 65535 ]; then
    echo "FERL_COLLECTOR_PORT must be an integer from 1024 through 65535" >&2
    exit 2
fi

container_exists() { podman container exists "$NAME"; }

assert_owned() {
    label=$(podman inspect --format "{{ index .Config.Labels \"$LABEL_KEY\" }}" "$NAME")
    [ "$label" = stage10 ] || {
        echo "refusing to operate on a container not owned by the Part 10 collector harness" >&2
        exit 1
    }
}

assert_configuration() {
    expected_digest=${IMAGE##*@}
    actual_digest=$(podman inspect --format '{{.ImageDigest}}' "$NAME")
    [ "$actual_digest" = "$expected_digest" ] || {
        echo "refusing to reuse a collector with an unexpected image digest" >&2
        exit 1
    }
    published=$(podman inspect --format '{{json .HostConfig.PortBindings}}' "$NAME" | python3 -c 'import json,sys; bindings=json.load(sys.stdin).get("4318/tcp", []); print("" if not bindings else bindings[0]["HostIp"] + ":" + bindings[0]["HostPort"])')
    [ "$published" = "127.0.0.1:$PORT" ] || {
        echo "collector publishes $published, not 127.0.0.1:$PORT" >&2
        exit 1
    }
    mounts=$(podman inspect --format '{{range .Mounts}}{{.Destination}} {{end}}' "$NAME")
    case "$mounts" in *"/etc/otelcol-contrib/config.yaml"*) ;; *) echo "collector is missing its checked-in config mount" >&2; exit 1 ;; esac
    case "$mounts" in *"/capture"*) ;; *) echo "collector is missing its capture mount" >&2; exit 1 ;; esac
}

pull_image() {
    # This Podman VM is behind the owner-confirmed TLS-intercepting proxy.
    # TLS verification is disabled for this pinned image pull only.
    podman pull --tls-verify=false "$IMAGE"
}

probe() {
    curl --silent --show-error --fail --max-time 1 \
        --header 'content-type: application/json' \
        --data-binary '{"resourceSpans":[{"resource":{"attributes":[{"key":"service.name","value":{"stringValue":"ferl-collector-probe"}}]},"scopeSpans":[{"scope":{"name":"ferl.harness"},"spans":[{"traceId":"00000000000000000000000000000001","spanId":"0000000000000001","name":"collector.readiness","kind":1,"startTimeUnixNano":"1","endTimeUnixNano":"2"}]}]}]}' \
        "http://127.0.0.1:$PORT/v1/traces" >/dev/null
}

wait_ready() {
    attempts=0
    while [ "$attempts" -lt 30 ]; do
        if probe && [ -s "$CAPTURE_DIR/telemetry.json" ]; then
            echo "Collector health: OTLP/HTTP accepted a trace and the file exporter captured it."
            return 0
        fi
        attempts=$((attempts + 1))
        sleep 1
    done
    echo "Collector did not accept and capture the readiness trace within 30 seconds" >&2
    return 1
}

case "${1:-}" in
    pull)
        podman info >/dev/null
        pull_image
        ;;
    start)
        podman info >/dev/null
        mkdir -p "$CAPTURE_DIR"
        if container_exists; then
            assert_owned
            assert_configuration
            running=$(podman inspect --format '{{ .State.Running }}' "$NAME")
            if [ "$running" != true ]; then
                podman start "$NAME" >/dev/null
            fi
        else
            if ! podman image exists "$IMAGE"; then pull_image; fi
            podman run --pull=never --detach \
                --name "$NAME" \
                --label "$LABEL_KEY=stage10" \
                --memory 512m \
                --cpus 1 \
                --pids-limit 128 \
                --publish "127.0.0.1:$PORT:4318" \
                --volume "$CONFIG:/etc/otelcol-contrib/config.yaml:ro" \
                --volume "$CAPTURE_DIR:/capture" \
                "$IMAGE" --config=/etc/otelcol-contrib/config.yaml >/dev/null
        fi
        ;;
    ready)
        container_exists || { echo "collector is absent; run 'sh scripts/collector.sh start'" >&2; exit 1; }
        assert_owned
        assert_configuration
        wait_ready
        ;;
    capture)
        if [ -d "$CAPTURE_DIR" ]; then
            find "$CAPTURE_DIR" -maxdepth 1 -type f -print
            for file in "$CAPTURE_DIR"/*; do
                [ -f "$file" ] || continue
                echo "--- $file (first 1048576 bytes) ---"
                head -c 1048576 "$file"
                echo
            done
        else
            echo "capture directory does not exist: $CAPTURE_DIR" >&2
            exit 1
        fi
        ;;
    stop)
        if container_exists; then
            assert_owned
            running=$(podman inspect --format '{{ .State.Running }}' "$NAME")
            if [ "$running" = true ]; then podman stop --time 5 "$NAME" >/dev/null; fi
        fi
        echo "Part 10 collector stopped; capture files are preserved."
        ;;
    restart)
        sh "$0" stop
        sh "$0" start
        sh "$0" ready
        ;;
    cleanup)
        if container_exists; then
            assert_owned
            podman rm --force "$NAME" >/dev/null
        fi
        echo "Removed only the Part 10 collector container; capture files were preserved at $CAPTURE_DIR."
        ;;
    *)
        echo "usage: $0 {pull|start|ready|capture|stop|restart|cleanup}" >&2
        exit 2
        ;;
esac
