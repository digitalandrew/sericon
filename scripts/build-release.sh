#!/bin/sh
# Prepare the helper cross-compilers described in docs/helper.md first.
set -eu
cd "$(dirname "$0")/.."
python3 helper/build.py
rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl arm-unknown-linux-musleabihf
for target in x86_64-unknown-linux-musl aarch64-unknown-linux-musl arm-unknown-linux-musleabihf; do
    # These Rust targets include their static C runtime; rust-lld links it.
    linker_var=$(printf 'CARGO_TARGET_%s_LINKER' "$target" | tr '[:lower:]-' '[:upper:]_')
    env "$linker_var=rust-lld" cargo build --release --locked --target "$target"
done
