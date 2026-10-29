#!/bin/sh
set -eu

PORT=${FERL_POSTGRES_PORT:-55432}
sh scripts/postgres.sh start
sh scripts/postgres.sh ready
export FERL_TEST_DATABASE_URL="postgres://ferl:ferl@127.0.0.1:$PORT/ferl_test"
export FERL_TEST_POSTGRES_CONTAINER=ferl-stage01-postgres
export FERL_TEST_POSTGRES_PORT="$PORT"

if ! podman image exists docker.io/library/nginx:1.30.6-alpine@sha256:47928bf67eb62a1863cb08d9ecb52d3e704304ba614eaee17884204677702951; then
    sh scripts/proxy.sh pull
fi
FERL_REQUIRE_CLEAN_RELEASE_SOURCE=true
export FERL_REQUIRE_CLEAN_RELEASE_SOURCE
. scripts/prepare-release-package.sh
cargo test --locked --test proxy_routing -- --nocapture
