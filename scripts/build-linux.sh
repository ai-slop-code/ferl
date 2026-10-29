#!/bin/sh
set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)
TARGET=${1:-aarch64-unknown-linux-gnu}
IMAGE=docker.io/library/rust@sha256:48b7f435b8fc6378ce43b44f15e3a47fd8919b2825f835c39bd4bd3da50af5b8
OUT=${FERL_LINUX_BUILD_DIR:-"$ROOT/target/linux-release"}
CARGO_REGISTRY=${CARGO_HOME:-"$HOME/.cargo"}/registry
HOST=$(rustc -vV | sed -n 's/^host: //p')

case "$TARGET" in
    aarch64-unknown-linux-gnu) RUSTUP_TOOLCHAIN=1.99.1-aarch64-unknown-linux-gnu ;;
    x86_64-unknown-linux-gnu) RUSTUP_TOOLCHAIN=1.99.1-x86_64-unknown-linux-gnu ;;
    *) echo "unsupported Linux release target '$TARGET'" >&2; exit 2 ;;
esac

if [ "$HOST" = "$TARGET" ]; then
    mkdir -p "$OUT"
    {
        rustc --version
        cargo --version
        rustc -vV
    } > "$OUT/toolchain.txt"
    CARGO_TARGET_DIR="$OUT/target" cargo build --locked --release --target "$TARGET" --bin ferl-server
    test -x "$OUT/target/$TARGET/release/ferl-server"
    echo "built $OUT/target/$TARGET/release/ferl-server with native pinned Rust toolchain"
    exit 0
fi
if [ "$TARGET" != aarch64-unknown-linux-gnu ] || [ "$HOST" != aarch64-apple-darwin ]; then
    echo "cross-building $TARGET from host $HOST is not supported" >&2
    exit 2
fi
if [ ! -d "$CARGO_REGISTRY" ]; then
    echo "Cargo registry cache is unavailable at the standard Cargo home" >&2
    exit 1
fi
podman info >/dev/null
podman image exists "$IMAGE" || podman pull "$IMAGE"
mkdir -p "$OUT"

podman run --rm --pull=never \
    --memory 1536m --cpus 3 --pids-limit 512 \
    --volume "$ROOT:/workspace:ro" \
    --volume "$CARGO_REGISTRY:/usr/local/cargo/registry:ro" \
    --volume "$OUT:/out:rw" \
    --workdir /workspace \
    --env CARGO_HOME=/usr/local/cargo \
    --env CARGO_TARGET_DIR=/out/target \
    --env CARGO_BUILD_JOBS=2 \
    --env RUSTFLAGS="-C debuginfo=0" \
    --env TARGET="$TARGET" \
    --env RUSTUP_TOOLCHAIN="$RUSTUP_TOOLCHAIN" \
    --entrypoint /bin/sh \
    "$IMAGE" -ec '
        case "$(rustc --version)" in
            "rustc 1.99.1 "*) ;;
            *) echo "builder rustc is not the pinned 1.99.1 toolchain" >&2; exit 1 ;;
        esac
        [ "$(rustc -vV | sed -n "s/^host: //p")" = "$TARGET" ] || {
            echo "builder architecture does not match the requested target" >&2
            exit 1
        }
        {
            rustc --version
            cargo --version
            rustc -vV
        } > /out/toolchain.txt
        cargo build --locked --release --target "$TARGET" --bin ferl-server
    '

test -x "$OUT/target/$TARGET/release/ferl-server"
echo "built $OUT/target/$TARGET/release/ferl-server with pinned $IMAGE"
