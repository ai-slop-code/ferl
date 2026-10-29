#!/bin/sh
# Source this file to build, verify, extract, and export the packaged binary.
set -eu

if [ -f "$PWD/Cargo.toml" ] && [ -d "$PWD/scripts" ]; then
    RELEASE_ROOT=$PWD
    RELEASE_SCRIPT_DIR=$PWD/scripts
else
    RELEASE_SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
    RELEASE_ROOT=$(CDPATH= cd -- "$RELEASE_SCRIPT_DIR/.." && pwd)
fi
RELEASE_HOST=$(rustc -vV | sed -n 's/^host: //p')
case "$RELEASE_HOST" in
    aarch64-apple-darwin) RELEASE_DEFAULT_TARGET=aarch64-unknown-linux-gnu ;;
    aarch64-unknown-linux-gnu|x86_64-unknown-linux-gnu) RELEASE_DEFAULT_TARGET=$RELEASE_HOST ;;
    *) echo "unsupported release build host $RELEASE_HOST" >&2; return 2 2>/dev/null || exit 2 ;;
esac
FERL_RELEASE_TARGET=${FERL_RELEASE_TARGET:-$RELEASE_DEFAULT_TARGET}
FERL_RELEASE_OUTPUT_DIR=${FERL_RELEASE_OUTPUT_DIR:-"$RELEASE_ROOT/target/release-packages"}
export FERL_RELEASE_TARGET FERL_RELEASE_OUTPUT_DIR

sh "$RELEASE_SCRIPT_DIR/package-release.sh"
RELEASE_VERSION=$(sed -n 's/^version = "\([^"]*\)"$/\1/p' "$RELEASE_ROOT/Cargo.toml" | head -n 1)
RELEASE_ARCHIVE="$FERL_RELEASE_OUTPUT_DIR/ferl-server-$RELEASE_VERSION-$FERL_RELEASE_TARGET.tar.gz"
RELEASE_RUN_ID=$(python3 -c 'import uuid; print(uuid.uuid4().hex)')
FERL_RELEASE_ROOT="$RELEASE_ROOT/target/release-smoke/$RELEASE_RUN_ID"
FERL_SOURCE_REVISION=$(git -C "$RELEASE_ROOT" rev-parse HEAD)
python3 "$RELEASE_SCRIPT_DIR/verify-release.py" \
    --extract-to "$FERL_RELEASE_ROOT" \
    --archive "$RELEASE_ARCHIVE" \
    --source-revision "$FERL_SOURCE_REVISION" \
    --target "$FERL_RELEASE_TARGET" \
    --openapi "$RELEASE_ROOT/spec/openapi.yaml" \
    ${FERL_REQUIRE_CLEAN_RELEASE_SOURCE:+--require-clean-source}
FERL_RELEASE_BINARY="$FERL_RELEASE_ROOT/bin/ferl-server"
python3 "$RELEASE_SCRIPT_DIR/verify-release.py" \
    --root "$FERL_RELEASE_ROOT" \
    --archive "$RELEASE_ARCHIVE" \
    --source-revision "$FERL_SOURCE_REVISION" \
    --target "$FERL_RELEASE_TARGET" \
    --openapi "$RELEASE_ROOT/spec/openapi.yaml" \
    ${FERL_REQUIRE_CLEAN_RELEASE_SOURCE:+--require-clean-source}
export FERL_RELEASE_ROOT FERL_RELEASE_BINARY FERL_RELEASE_ARCHIVE FERL_SOURCE_REVISION
