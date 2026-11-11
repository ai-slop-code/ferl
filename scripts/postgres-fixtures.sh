#!/bin/sh
# Test-harness-only PostgreSQL fixture selection. Source this file; it is not
# production configuration. FERL_TEST_POSTGRES_MAJOR accepts exactly 17 or 18.
# Each major owns a separate container, volume, mount, ownership label, image
# pin, and loopback port, so switching selections never repurposes the other.

FERL_TEST_POSTGRES_MAJOR=${FERL_TEST_POSTGRES_MAJOR-17}
case "$FERL_TEST_POSTGRES_MAJOR" in
    17)
        FERL_PG_NAME=ferl-stage01-postgres
        FERL_PG_VOLUME=ferl-stage01-postgres-data
        FERL_PG_MOUNT=/var/lib/postgresql/data
        FERL_PG_LABEL=stage01
        FERL_PG_DEFAULT_PORT=55432
        FERL_PG_IMAGE=docker.io/library/postgres:17.6-alpine3.22@sha256:ef257d85f76e48da1c64832459b59fcaba1a4dac97bf5d7450c77753542eee94
        ;;
    18)
        FERL_PG_NAME=ferl-stage01-postgres18
        FERL_PG_VOLUME=ferl-stage01-postgres18-data
        FERL_PG_MOUNT=/var/lib/postgresql
        FERL_PG_LABEL=stage01-postgres18
        FERL_PG_DEFAULT_PORT=55433
        FERL_PG_IMAGE=docker.io/library/postgres:18.6-alpine3.23@sha256:f1031bf693d8d60b0a0380c51a3510607c7a93c7c1d18bf7af94a3a8dbeb40b8
        ;;
    *)
        echo "FERL_TEST_POSTGRES_MAJOR must be exactly 17 or 18" >&2
        exit 2
        ;;
esac

FERL_PG_PORT=${FERL_POSTGRES_PORT:-$FERL_PG_DEFAULT_PORT}
case "$FERL_PG_PORT" in
    ''|*[!0-9]*)
        echo "FERL_POSTGRES_PORT must be an integer from 1024 through 65535" >&2
        exit 2
        ;;
esac
if [ "$FERL_PG_PORT" -lt 1024 ] || [ "$FERL_PG_PORT" -gt 65535 ]; then
    echo "FERL_POSTGRES_PORT must be an integer from 1024 through 65535" >&2
    exit 2
fi

export FERL_TEST_POSTGRES_MAJOR FERL_PG_NAME FERL_PG_VOLUME FERL_PG_MOUNT FERL_PG_LABEL FERL_PG_IMAGE FERL_PG_PORT
export FERL_TEST_DATABASE_URL="postgres://ferl:ferl@127.0.0.1:$FERL_PG_PORT/ferl_test"
export FERL_TEST_POSTGRES_CONTAINER="$FERL_PG_NAME"
export FERL_TEST_POSTGRES_PORT="$FERL_PG_PORT"
