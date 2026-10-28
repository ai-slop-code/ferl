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
# Child workers execute the rate_limit_process test binary, never a production
# CLI test command. Clear only worker dispatch inputs for this parent gate.
unset FERL_RATE_TEST_WORKER_URL FERL_RATE_TEST_CREDENTIAL
# Includes IP, allowance, concurrency, fault/privacy and independent-process suites.
cargo test --workspace --all-targets
cargo test --doc
