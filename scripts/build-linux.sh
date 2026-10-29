#!/bin/sh
set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)
TARGET=${1:-aarch64-unknown-linux-gnu}
OUT=${FERL_LINUX_BUILD_DIR:-"$ROOT/target/linux-release"}
SOURCE_DIR=${FERL_BUILD_SOURCE_DIR:-$ROOT}
SOURCE_DIR=$(CDPATH= cd -- "$SOURCE_DIR" && pwd)
CARGO_REGISTRY=${CARGO_HOME:-"$HOME/.cargo"}/registry
HOST=$(rustc -vV | sed -n 's/^host: //p')
SOURCE_REVISION=${FERL_BUILD_SOURCE_REVISION:-$(git -C "$ROOT" rev-parse HEAD)}
SOURCE_TREE=${FERL_BUILD_SOURCE_TREE:-$(git -C "$ROOT" rev-parse 'HEAD^{tree}')}
SOURCE_SNAPSHOT_VERIFIED=false

case "$TARGET" in
    aarch64-unknown-linux-gnu)
        RUSTUP_TOOLCHAIN=1.99.1-aarch64-unknown-linux-gnu
        IMAGE=docker.io/library/rust@sha256:32f7944ed00949c44d3944a0a852b3e5ea862a610a38b43ac92a33aa87256b75
        PLATFORM=linux/arm64
        ;;
    x86_64-unknown-linux-gnu)
        RUSTUP_TOOLCHAIN=1.99.1-x86_64-unknown-linux-gnu
        IMAGE=docker.io/library/rust@sha256:8bf7188ae46ad73b801c8d405eedc13ce63a7852b562885743a56890723549a8
        PLATFORM=linux/amd64
        ;;
    *) echo "unsupported Linux release target '$TARGET'" >&2; exit 2 ;;
esac

case "$HOST/$TARGET" in
    aarch64-apple-darwin/aarch64-unknown-linux-gnu \
    | aarch64-unknown-linux-gnu/aarch64-unknown-linux-gnu \
    | x86_64-unknown-linux-gnu/x86_64-unknown-linux-gnu)
        ;;
    *)
        echo "building Linux target $TARGET from host $HOST is not supported" >&2
        exit 2
        ;;
esac

if [ ! -d "$CARGO_REGISTRY" ]; then
    mkdir -p "$CARGO_REGISTRY"
fi
cargo fetch --locked
podman info >/dev/null
podman image exists "$IMAGE" || podman pull "$IMAGE"
mkdir -p "$OUT"
OUT=$(CDPATH= cd -- "$OUT" && pwd)
test -f "$SOURCE_DIR/Cargo.toml"
test -f "$SOURCE_DIR/Cargo.lock"
if [ "$SOURCE_DIR" = "$ROOT" ]; then
    [ "$(git -C "$ROOT" rev-parse HEAD)" = "$SOURCE_REVISION" ] \
        && [ "$(git -C "$ROOT" rev-parse 'HEAD^{tree}')" = "$SOURCE_TREE" ] \
        && [ -z "$(git -C "$ROOT" status --porcelain --untracked-files=normal)" ] || {
        echo "release build from the worktree requires its exact clean committed source" >&2
        exit 1
    }
else
    python3 "$SCRIPT_DIR/verify_source_snapshot.py" \
        --repository "$ROOT" \
        --revision "$SOURCE_REVISION" \
        --snapshot "$SOURCE_DIR" \
        --require-read-only
    SOURCE_SNAPSHOT_VERIFIED=true
fi

podman run --rm --pull=never --platform "$PLATFORM" \
    --memory 1536m --cpus 3 --pids-limit 512 \
    --volume "$SOURCE_DIR:/workspace:ro" \
    --volume "$CARGO_REGISTRY:/usr/local/cargo/registry:rw" \
    --volume "$OUT:/out:rw" \
    --workdir /workspace \
    --env CARGO_HOME=/usr/local/cargo \
    --env CARGO_TARGET_DIR=/out/target \
    --env CARGO_BUILD_JOBS=2 \
    --env RUSTFLAGS="-C debuginfo=0" \
    --env TARGET="$TARGET" \
    --env TARGET_PLATFORM="$PLATFORM" \
    --env BUILDER_IMAGE="$IMAGE" \
    --env RUSTUP_TOOLCHAIN="$RUSTUP_TOOLCHAIN" \
    --entrypoint /bin/sh \
    "$IMAGE" -ec '
        . /etc/os-release
        [ "${ID:-}" = debian ] && [ "${VERSION_CODENAME:-}" = bookworm ] || {
            echo "release builder is not Debian Bookworm" >&2
            exit 1
        }
        case "$(rustc --version)" in
            "rustc 1.99.1 "*) ;;
            *) echo "builder rustc is not the pinned 1.99.1 toolchain" >&2; exit 1 ;;
        esac
        case "$(cargo --version)" in
            "cargo 1.99.1 "*) ;;
            *) echo "builder cargo is not the pinned 1.99.1 toolchain" >&2; exit 1 ;;
        esac
        [ "$(rustc -vV | sed -n "s/^host: //p")" = "$TARGET" ] || {
            echo "builder architecture does not match the requested target" >&2
            exit 1
        }
        cargo build --locked --release --target "$TARGET" --bin ferl-server
        {
            rustc --version
            cargo --version
            rustc -vV
            printf "builder_os=%s/%s\n" "$ID" "$VERSION_CODENAME"
        } > /out/toolchain.txt
        BINARY="/out/target/$TARGET/release/ferl-server"
        BINARY_SHA256=$(sha256sum "$BINARY" | awk "{print \$1}")
        printf "{\"target\":\"%s\",\"builder_image\":\"%s\",\"builder_platform\":\"%s\",\"build_command\":\"cargo build --locked --release --target %s --bin ferl-server\",\"binary_sha256\":\"%s\"}\n" \
            "$TARGET" "$BUILDER_IMAGE" "$TARGET_PLATFORM" "$TARGET" "$BINARY_SHA256" \
            > /out/builder-observation.json
    '

test -x "$OUT/target/$TARGET/release/ferl-server"
python3 - "$OUT" "$TARGET" "$IMAGE" "$PLATFORM" "$SOURCE_REVISION" "$SOURCE_TREE" "$SOURCE_SNAPSHOT_VERIFIED" <<'PY'
import hashlib
import json
from pathlib import Path
import sys
import uuid

out = Path(sys.argv[1]).resolve()
target, image, platform, revision, tree, snapshot_verified = sys.argv[2:]
relative_binary = Path("target") / target / "release" / "ferl-server"
binary = out / relative_binary
toolchain = (out / "toolchain.txt").read_text()
observation = json.loads((out / "builder-observation.json").read_text())
build_command = f"cargo build --locked --release --target {target} --bin ferl-server"
binary_sha256 = hashlib.sha256(binary.read_bytes()).hexdigest()
if (
    observation.get("target") != target
    or observation.get("builder_image") != image
    or observation.get("builder_platform") != platform
    or observation.get("build_command") != build_command
    or observation.get("binary_sha256") != binary_sha256
):
    raise SystemExit("builder-observed output does not match the requested release build")
receipt = {
    "build_invocation": uuid.uuid4().hex,
    "source_revision": revision,
    "source_tree": tree,
    "source_snapshot_verified": snapshot_verified == "true",
    "target": target,
    "builder_image": image,
    "builder_platform": platform,
    "build_command": build_command,
    "builder_observation": observation,
    "binary_relative_path": relative_binary.as_posix(),
    "binary_sha256": binary_sha256,
    "toolchain_observed": toolchain,
}
temporary = out / f".build-receipt.{uuid.uuid4().hex}.tmp"
temporary.write_text(json.dumps(receipt, indent=2, sort_keys=True) + "\n")
temporary.replace(out / "build-receipt.json")
PY
echo "built $OUT/target/$TARGET/release/ferl-server in pinned Bookworm builder $IMAGE"
