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
SOURCE_TREE=$(git -C "$ROOT" rev-parse 'HEAD^{tree}')
mkdir -p "$ROOT/target"
SOURCE_SNAPSHOT=$(mktemp -d "$ROOT/target/ferl-release-source.XXXXXX")
SOURCE_ARCHIVE="$SOURCE_SNAPSHOT.tar"
cleanup_source_snapshot() {
    if [ -d "$SOURCE_SNAPSHOT" ]; then
        chmod -R u+w "$SOURCE_SNAPSHOT"
    fi
    rm -rf "$SOURCE_SNAPSHOT"
    rm -f "$SOURCE_ARCHIVE"
}
trap cleanup_source_snapshot EXIT
git -C "$ROOT" archive --format=tar "$SOURCE_REVISION" > "$SOURCE_ARCHIVE"
tar -xf "$SOURCE_ARCHIVE" -C "$SOURCE_SNAPSHOT"
rm -f "$SOURCE_ARCHIVE"
python3 "$SCRIPT_DIR/verify_source_snapshot.py" \
    --repository "$ROOT" \
    --revision "$SOURCE_REVISION" \
    --snapshot "$SOURCE_SNAPSHOT"
chmod -R a-w "$SOURCE_SNAPSHOT"
export FERL_BUILD_SOURCE_DIR="$SOURCE_SNAPSHOT"
export FERL_BUILD_SOURCE_REVISION="$SOURCE_REVISION"
export FERL_BUILD_SOURCE_TREE="$SOURCE_TREE"

case "$TARGET" in
    aarch64-unknown-linux-gnu)
        export FERL_LINUX_BUILD_DIR="$LINUX_BUILD_DIR"
        sh "$SCRIPT_DIR/build-linux.sh" "$TARGET"
        ;;
    x86_64-unknown-linux-gnu)
        export FERL_LINUX_BUILD_DIR="$LINUX_BUILD_DIR"
        sh "$SCRIPT_DIR/build-linux.sh" "$TARGET"
        ;;
    *)
        echo "unsupported release target '$TARGET'; choose aarch64-unknown-linux-gnu or x86_64-unknown-linux-gnu" >&2
        exit 2
        ;;
esac

if [ "$(git -C "$ROOT" rev-parse HEAD)" != "$SOURCE_REVISION" ] \
    || [ "$(git -C "$ROOT" rev-parse 'HEAD^{tree}')" != "$SOURCE_TREE" ] \
    || [ "$(git -C "$ROOT" status --porcelain --untracked-files=normal)" != "$SOURCE_STATUS_BEFORE" ]; then
    echo "source revision or tree changed while building the release executable" >&2
    exit 1
fi
python3 "$SCRIPT_DIR/package_release.py" \
    --build-manifest "$LINUX_BUILD_DIR/build-receipt.json" \
    --source-snapshot "$SOURCE_SNAPSHOT" \
    --output-dir "$OUTPUT_DIR"
