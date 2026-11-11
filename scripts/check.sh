#!/bin/sh
set -eu

. scripts/postgres-fixtures.sh
sh scripts/postgres.sh start
sh scripts/postgres.sh ready

# Each gate run receives its own collector container, host port, and capture
# directory. The no-endpoint test monitors the SDK's standard localhost:4318
# default separately; the Collector itself uses an ephemeral loopback port.
FERL_COLLECTOR_RUN_ID=${FERL_COLLECTOR_RUN_ID:-gate-$$}
if [ -z "${FERL_COLLECTOR_PORT:-}" ]; then
    FERL_COLLECTOR_PORT=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()')
fi
export FERL_COLLECTOR_RUN_ID FERL_COLLECTOR_PORT
trap 'sh scripts/collector.sh cleanup >/dev/null 2>&1 || true' EXIT HUP INT TERM
sh scripts/collector.sh start
sh scripts/collector.sh ready
export FERL_TEST_OTEL_ENDPOINT="http://127.0.0.1:$FERL_COLLECTOR_PORT"
export FERL_TEST_OTEL_CAPTURE_DIR="${FERL_COLLECTOR_CAPTURE_DIR:-$PWD/target/collector-captures/$FERL_COLLECTOR_RUN_ID}"
export FERL_COLLECTOR_CAPTURE_DIR="$FERL_TEST_OTEL_CAPTURE_DIR"

cargo fmt --all -- --check
cargo check --locked --workspace --all-targets --all-features
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
# Release/proxy suites run the packaged Linux executable, never a stale target/
# binary. The pinned builder and NGINX images must be present before the gate.
sh scripts/proxy.sh pull
. scripts/prepare-release-package.sh
# Child workers execute the rate_limit_process test binary, never a production
# CLI test command. Clear only worker dispatch inputs for this parent gate.
unset FERL_RATE_TEST_WORKER_URL FERL_RATE_TEST_CREDENTIAL
# Includes IP/allowance, link management/discovery/reports, migrations/query
# plans, redirect/visit ingestion, concurrency, fault/privacy, OpenAPI, and
# independent production-process suites.
cargo test --locked --workspace --all-targets
cargo test --locked --doc
