#!/bin/sh
# Cross-builds the Linux browser from macOS (or another Linux):
#   scripts/linux/build.sh [aarch64|x86_64]       (default: aarch64)
# Output: target/<arch>-unknown-linux-gnu/release/rubrowser
# Needs: rustup target add <arch>-unknown-linux-gnu, zig, cargo install cargo-zigbuild
# (zig provides the C compiler and linker for Linux that `ring` needs).
set -e
cd "$(dirname "$0")/../.."
arch=${1:-aarch64}
cargo zigbuild --release --target "$arch-unknown-linux-gnu"
ls -l "target/$arch-unknown-linux-gnu/release/rubrowser"
