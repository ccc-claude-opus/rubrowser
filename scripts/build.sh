#!/bin/sh
# Builds the example page scripts and the JavaScript runtime (both embedded
# in the browser) and the canvas (web) version into web/rubrowser.wasm.
set -e
cd "$(dirname "$0")/.."

examples="counter todo stopwatch events"
args=""
for e in $examples; do args="$args --example $e"; done

# 1. Example page scripts: Rust -> wasm with only the `sdk` feature.
cargo build --profile page --target wasm32-unknown-unknown \
  --no-default-features --features sdk $args
for e in $examples; do
  cp "target/wasm32-unknown-unknown/page/examples/$e.wasm" src/browser/pages/
done

# 2. Language runtimes: JavaScript is the boa engine compiled to wasm.
#    (.cargo/config.toml selects getrandom's custom backend for wasm.)
#    A 16 MiB stack (instead of 1 MiB) for deeply recursive scripts.
cargo rustc --profile runtime --target wasm32-unknown-unknown \
  --no-default-features --features js-runtime --example js_runtime \
  -- -C link-arg=-zstack-size=16777216
cp target/wasm32-unknown-unknown/runtime/examples/js_runtime.wasm src/browser/runtimes/js.wasm

#    The same environment on the rujse engine (../rujse), which the browser
#    uses when built with the `rujse` feature. prelude.js goes in compiled
#    (rujse's bytecode cache), made by the rujse command-line tool.
(cd ../rujse && cargo build --release --bin rujse)
../rujse/target/release/rujse --cache-out src/lang/prelude.rjbc src/lang/prelude.js prelude.js
cargo rustc --profile runtime --target wasm32-unknown-unknown \
  --no-default-features --features js-runtime-rujse --example js_runtime_rujse \
  -- -C link-arg=-zstack-size=16777216
cp target/wasm32-unknown-unknown/runtime/examples/js_runtime_rujse.wasm src/browser/runtimes/js-rujse.wasm

# 3. The browser itself as wasm, for the canvas version.
cargo rustc --lib --release --target wasm32-unknown-unknown \
  --no-default-features --features web --crate-type cdylib
cp target/wasm32-unknown-unknown/release/rubrowser.wasm web/

echo "Built example scripts and web/rubrowser.wasm. Serve the web version with:"
echo "  python3 -m http.server -d web 8000"
