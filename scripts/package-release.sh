#!/bin/sh
set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)
HOST=$(rustc -vV | sed -n 's/^host: //p')
case "$HOST" in
    aarch64-apple-darwin) DEFAULT_TARGET=aarch64-unknown-linux-gnu ;;
    aarch64-unknown-linux-gnu|x86_64-unknown-linux-gnu) DEFAULT_TARGET=$HOST ;;
    *) echo "no verified Linux release target for build host $HOST" >&2; exit 2 ;;
esac
TARGET=${FERL_RELEASE_TARGET:-$DEFAULT_TARGET}
OUTPUT_DIR=${FERL_RELEASE_OUTPUT_DIR:-"$ROOT/target/release-packages"}
LINUX_BUILD_DIR=${FERL_LINUX_BUILD_DIR:-"$ROOT/target/linux-release"}

SOURCE_REVISION=$(git -C "$ROOT" rev-parse HEAD)
if [ -n "${FERL_SOURCE_REVISION:-}" ] && [ "$FERL_SOURCE_REVISION" != "$SOURCE_REVISION" ]; then
    echo "FERL_SOURCE_REVISION must match the checkout HEAD used for the build" >&2
    exit 1
fi
SOURCE_STATUS_BEFORE=$(git -C "$ROOT" status --porcelain --untracked-files=normal)
if [ -n "$SOURCE_STATUS_BEFORE" ]; then
    echo "release packaging requires a clean tracked/untracked source tree before building" >&2
    exit 1
fi
SOURCE_TREE_CLEAN=true

case "$TARGET" in
    aarch64-unknown-linux-gnu)
        export FERL_LINUX_BUILD_DIR="$LINUX_BUILD_DIR"
        sh "$SCRIPT_DIR/build-linux.sh" "$TARGET"
        BINARY="$LINUX_BUILD_DIR/target/$TARGET/release/ferl-server"
        BUILDER="Rust/Cargo 1.99.1; pinned Debian Bookworm builder image sha256:32f7944ed00949c44d3944a0a852b3e5ea862a610a38b43ac92a33aa87256b75; linux/arm64"
        ;;
    x86_64-unknown-linux-gnu)
        export FERL_LINUX_BUILD_DIR="$LINUX_BUILD_DIR"
        sh "$SCRIPT_DIR/build-linux.sh" "$TARGET"
        BINARY="$LINUX_BUILD_DIR/target/$TARGET/release/ferl-server"
        BUILDER="Rust/Cargo 1.99.1; pinned Debian Bookworm builder image sha256:8bf7188ae46ad73b801c8d405eedc13ce63a7852b562885743a56890723549a8; linux/amd64"
        ;;
    *)
        echo "unsupported release target '$TARGET'; choose aarch64-unknown-linux-gnu or x86_64-unknown-linux-gnu" >&2
        exit 2
        ;;
esac

if [ "$(git -C "$ROOT" rev-parse HEAD)" != "$SOURCE_REVISION" ] \
    || [ "$(git -C "$ROOT" status --porcelain --untracked-files=normal)" != "$SOURCE_STATUS_BEFORE" ]; then
    echo "source revision or tree changed while building the release executable" >&2
    exit 1
fi
python3 "$SCRIPT_DIR/package_release.py" \
    --binary "$BINARY" \
    --target "$TARGET" \
    --source-revision "$SOURCE_REVISION" \
    --source-tree-clean "$SOURCE_TREE_CLEAN" \
    --builder "$BUILDER" \
    --output-dir "$OUTPUT_DIR"
