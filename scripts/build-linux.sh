#!/bin/sh
set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)
TARGET=${1:-aarch64-unknown-linux-gnu}
OUT=${FERL_LINUX_BUILD_DIR:-"$ROOT/target/linux-release"}
CARGO_REGISTRY=${CARGO_HOME:-"$HOME/.cargo"}/registry
HOST=$(rustc -vV | sed -n 's/^host: //p')

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

podman run --rm --pull=never --platform "$PLATFORM" \
    --memory 1536m --cpus 3 --pids-limit 512 \
    --volume "$ROOT:/workspace:ro" \
    --volume "$CARGO_REGISTRY:/usr/local/cargo/registry:rw" \
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
        . /etc/os-release
        [ "${ID:-}" = debian ] && [ "${VERSION_CODENAME:-}" = bookworm ] || {
            echo "release builder is not Debian Bookworm" >&2
            exit 1
        }
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
            printf "builder_os=%s/%s\n" "$ID" "$VERSION_CODENAME"
        } > /out/toolchain.txt
        cargo build --locked --release --target "$TARGET" --bin ferl-server
    '

test -x "$OUT/target/$TARGET/release/ferl-server"
echo "built $OUT/target/$TARGET/release/ferl-server in pinned Bookworm builder $IMAGE"
