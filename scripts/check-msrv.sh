#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$ROOT"

if [ -n "${FERL_MSRV_TOOLCHAIN_DIR:-}" ]; then
    TOOLCHAIN_BIN="$FERL_MSRV_TOOLCHAIN_DIR/bin"
elif command -v rustup >/dev/null 2>&1; then
    TOOLCHAIN_BIN=$(dirname "$(rustup which rustc --toolchain 1.94.0)")
else
    echo "install Rust 1.94.0 with rustup or set FERL_MSRV_TOOLCHAIN_DIR to its isolated prefix" >&2
    exit 2
fi

if [ ! -x "$TOOLCHAIN_BIN/rustc" ] || [ ! -x "$TOOLCHAIN_BIN/cargo" ] \
    || [ ! -x "$TOOLCHAIN_BIN/rustdoc" ]; then
    echo "Rust/Cargo/Rustdoc 1.94.0 binaries are missing from $TOOLCHAIN_BIN" >&2
    exit 2
fi

export PATH="$TOOLCHAIN_BIN:$PATH"
export RUSTC="$TOOLCHAIN_BIN/rustc"
export RUSTDOC="$TOOLCHAIN_BIN/rustdoc"
RUSTC_VERSION=$("$TOOLCHAIN_BIN/rustc" --version)
case "$RUSTC_VERSION" in
    "rustc 1.94.0 "*) ;;
    *) echo "MSRV check requires rustc 1.94.0; found $RUSTC_VERSION" >&2; exit 1 ;;
esac
"$TOOLCHAIN_BIN/rustc" --version
"$TOOLCHAIN_BIN/rustc" -vV
RUSTDOC_VERSION=$("$TOOLCHAIN_BIN/rustdoc" --version)
case "$RUSTDOC_VERSION" in
    "rustdoc 1.94.0 "*) ;;
    *) echo "MSRV check requires rustdoc 1.94.0; found $RUSTDOC_VERSION" >&2; exit 1 ;;
esac
"$TOOLCHAIN_BIN/rustdoc" --version
CARGO_VERSION=$("$TOOLCHAIN_BIN/cargo" --version)
case "$CARGO_VERSION" in
    "cargo 1.94.0 "*) ;;
    *) echo "MSRV check requires cargo 1.94.0; found $CARGO_VERSION" >&2; exit 1 ;;
esac
"$TOOLCHAIN_BIN/cargo" --version

export CARGO_TARGET_DIR="$ROOT/target/msrv-1.94/workspace-check"
"$TOOLCHAIN_BIN/cargo" check --locked --workspace --all-targets --all-features

export CARGO_TARGET_DIR="$ROOT/target/msrv-1.94/release-build"
"$TOOLCHAIN_BIN/cargo" build --locked --release --bin ferl-server
