#!/bin/sh
# Builds the example page scripts (embedded in the browser as about: pages)
# and the canvas (web) version of the browser into web/rubrowser.wasm.
set -e
cd "$(dirname "$0")/.."

examples="counter todo stopwatch events"
args=""
for e in $examples; do args="$args --example $e"; done

# 1. Page scripts: Rust -> wasm with only the `sdk` feature.
cargo build --profile page --target wasm32-unknown-unknown \
  --no-default-features --features sdk $args
for e in $examples; do
  cp "target/wasm32-unknown-unknown/page/examples/$e.wasm" src/browser/pages/
done

# 2. The browser itself as wasm, for the canvas version.
cargo rustc --lib --release --target wasm32-unknown-unknown \
  --no-default-features --features web --crate-type cdylib
cp target/wasm32-unknown-unknown/release/rubrowser.wasm web/

echo "Built example scripts and web/rubrowser.wasm. Serve the web version with:"
echo "  python3 -m http.server -d web 8000"
