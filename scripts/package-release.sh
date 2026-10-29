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

case "$TARGET" in
    aarch64-unknown-linux-gnu)
        export FERL_LINUX_BUILD_DIR="$LINUX_BUILD_DIR"
        sh "$SCRIPT_DIR/build-linux.sh" "$TARGET"
        BINARY="$LINUX_BUILD_DIR/target/$TARGET/release/ferl-server"
        BUILDER="Rust/Cargo 1.99.1; linux/arm64 native or pinned Debian Bookworm builder image sha256:48b7f435b8fc6378ce43b44f15e3a47fd8919b2825f835c39bd4bd3da50af5b8"
        ;;
    x86_64-unknown-linux-gnu)
        export FERL_LINUX_BUILD_DIR="$LINUX_BUILD_DIR"
        sh "$SCRIPT_DIR/build-linux.sh" "$TARGET"
        BINARY="$LINUX_BUILD_DIR/target/$TARGET/release/ferl-server"
        BUILDER="Rust/Cargo 1.99.1 native linux/x86_64"
        ;;
    *)
        echo "unsupported release target '$TARGET'; only verified aarch64-unknown-linux-gnu is packaged" >&2
        exit 2
        ;;
esac

SOURCE_REVISION=${FERL_SOURCE_REVISION:-$(git -C "$ROOT" rev-parse HEAD)}
if [ -z "$(git -C "$ROOT" status --porcelain --untracked-files=normal)" ]; then
    SOURCE_TREE_CLEAN=true
else
    SOURCE_TREE_CLEAN=false
fi
python3 "$SCRIPT_DIR/package_release.py" \
    --binary "$BINARY" \
    --target "$TARGET" \
    --source-revision "$SOURCE_REVISION" \
    --source-tree-clean "$SOURCE_TREE_CLEAN" \
    --builder "$BUILDER" \
    --output-dir "$OUTPUT_DIR"
