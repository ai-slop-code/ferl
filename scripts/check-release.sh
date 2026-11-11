#!/bin/sh
set -eu

. scripts/postgres-fixtures.sh
sh scripts/postgres.sh start
sh scripts/postgres.sh ready
if ! podman image exists docker.io/library/nginx:1.30.6-alpine@sha256:47928bf67eb62a1863cb08d9ecb52d3e704304ba614eaee17884204677702951; then
    sh scripts/proxy.sh pull
fi
if ! podman image exists docker.io/otel/opentelemetry-collector-contrib@sha256:340c9ca6a36617084c6661bdc4f0fe92b7cda5a805d61673ec8db375fddc522b; then
    sh scripts/collector.sh pull
fi
FERL_REQUIRE_CLEAN_RELEASE_SOURCE=true
export FERL_REQUIRE_CLEAN_RELEASE_SOURCE
python3 scripts/test_release_verification.py
. scripts/prepare-release-package.sh
CARGO_TARGET_DIR=${FERL_RELEASE_TEST_TARGET_DIR:-"$PWD/target/release-smoke-cargo"}
export CARGO_TARGET_DIR
cargo test --locked --test openapi_release --test proxy_routing --test release_smoke --test release_tls -- --nocapture
