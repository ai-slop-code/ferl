#!/bin/sh
set -eu

. scripts/postgres-fixtures.sh
sh scripts/postgres.sh start
sh scripts/postgres.sh ready
cargo test --locked --test openapi_release -- --nocapture
