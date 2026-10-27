#!/bin/sh
set -eu

PORT=${FERL_POSTGRES_PORT:-55432}
sh scripts/postgres.sh start
sh scripts/postgres.sh ready

export FERL_TEST_DATABASE_URL="postgres://ferl:ferl@127.0.0.1:$PORT/ferl_test"
export FERL_TEST_POSTGRES_CONTAINER=ferl-stage01-postgres
export FERL_TEST_POSTGRES_PORT="$PORT"

cargo fmt --all -- --check
cargo check --workspace --all-targets --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
# --all-targets includes Part 03 domain, decoder, cursor, and OpenAPI suites.
cargo test --workspace --all-targets
