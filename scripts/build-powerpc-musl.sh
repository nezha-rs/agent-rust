#!/usr/bin/env bash
set -euo pipefail

repo_dir="$(cd "$(dirname "$0")/.." && pwd)"
export ZIG="${ZIG:-zig}"
export CC_powerpc_unknown_linux_musl="$repo_dir/scripts/powerpc-musl-cc.sh"
export AR_powerpc_unknown_linux_musl="$repo_dir/scripts/powerpc-musl-ar.sh"
export CARGO_TARGET_POWERPC_UNKNOWN_LINUX_MUSL_LINKER="$CC_powerpc_unknown_linux_musl"
export RUSTFLAGS="${RUSTFLAGS:-} -C target-feature=+crt-static"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$repo_dir/target}"

cd "$repo_dir"
cargo +nightly -Z build-std=std,panic_abort build --locked --release --target powerpc-unknown-linux-musl
