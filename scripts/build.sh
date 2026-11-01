#!/bin/sh
# Build the canvas (web) version into web/rubrowser.wasm.
set -e
cd "$(dirname "$0")/.."

cargo rustc --lib --release --target wasm32-unknown-unknown \
  --no-default-features --features web --crate-type cdylib
cp target/wasm32-unknown-unknown/release/rubrowser.wasm web/

echo "Built web/rubrowser.wasm. Serve it with:"
echo "  python3 -m http.server -d web 8000"
