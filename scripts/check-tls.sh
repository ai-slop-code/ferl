#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$ROOT"

if [ -n "$(git status --porcelain --untracked-files=normal)" ]; then
    echo "TLS release verification requires a clean committed source tree" >&2
    exit 1
fi

podman info >/dev/null
. "$ROOT/scripts/postgres-fixtures.sh"
sh scripts/postgres.sh start
sh scripts/postgres.sh ready
POSTGRES_IMAGE=$FERL_PG_IMAGE
COLLECTOR_IMAGE=docker.io/otel/opentelemetry-collector-contrib@sha256:340c9ca6a36617084c6661bdc4f0fe92b7cda5a805d61673ec8db375fddc522b
if ! podman image exists "$POSTGRES_IMAGE"; then
    podman pull "$POSTGRES_IMAGE"
fi
if ! podman image exists "$COLLECTOR_IMAGE"; then
    podman pull "$COLLECTOR_IMAGE"
fi

RUN_ID=$(python3 -c 'import uuid; print(uuid.uuid4().hex)')
EVIDENCE_DIR="$ROOT/target/release-tls/$RUN_ID"
export FERL_RELEASE_OUTPUT_DIR="$EVIDENCE_DIR/packages"
export FERL_LINUX_BUILD_DIR="$EVIDENCE_DIR/linux-builder"
export FERL_RELEASE_TEST_TARGET_DIR="$EVIDENCE_DIR/rust-tests"
export FERL_REQUIRE_CLEAN_RELEASE_SOURCE=true
mkdir -p "$EVIDENCE_DIR"

. "$ROOT/scripts/prepare-release-package.sh"
CARGO_TARGET_DIR=$FERL_RELEASE_TEST_TARGET_DIR
export CARGO_TARGET_DIR
cargo test --locked --test release_tls -- --nocapture
