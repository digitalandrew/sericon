#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
rustup target add --toolchain 1.98.1 aarch64-unknown-linux-musl arm-unknown-linux-musleabihf
CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=rust-lld cargo build --release --locked --target aarch64-unknown-linux-musl
CARGO_TARGET_ARM_UNKNOWN_LINUX_MUSLEABIHF_LINKER=rust-lld cargo build --release --locked --target arm-unknown-linux-musleabihf
