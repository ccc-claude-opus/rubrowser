# rubrowser

A browser based on wasm. (RUST)

rubrowser draws an entire web browser on a **blank canvas** — a native window's pixel buffer, or a single HTML `<canvas>`. It lays out and paints **HTML + CSS** itself, and **only executes WebAssembly**: page scripts are wasm modules, and **JavaScript runs inside a JS engine compiled to wasm** ([boa](https://crates.io/crates/boa_engine)). Even the browser's own tabs, toolbar and address bar are an HTML/CSS document rendered by the same engine.

- HTML parsed by [`html5ever`](https://crates.io/crates/html5ever) into rubrowser's own DOM
- CSS selectors and cascade with [`simplecss`](https://crates.io/crates/simplecss); block and flex layout with [`taffy`](https://crates.io/crates/taffy); inline layout written from scratch
- Text rendered with [`fontdue`](https://crates.io/crates/fontdue) and the bundled DejaVu fonts
- Page scripts are wasm modules run by [`wasmi`](https://crates.io/crates/wasmi), sandboxed with fuel and memory limits
- `<script>` JavaScript, inline `onclick` handlers and a familiar DOM API, executed by `js.wasm` (see [doc/javascript.md](doc/javascript.md))

## Install and run

```bash
cargo install rubrowser
```

```bash
rubrowser
```

Open a file or URL directly:

```bash
rubrowser page.html
```

Built-in pages: `about:home`, `about:counter`, `about:todo`, `about:stopwatch`, `about:events` (Rust/wasm) and `about:js-todo`, `about:calc` (JavaScript).

Shortcuts: <kbd>Cmd/Ctrl</kbd>+<kbd>L</kbd> address bar, +<kbd>T</kbd> new tab, +<kbd>W</kbd> close tab, +<kbd>R</kbd> reload, +<kbd>[</kbd>/<kbd>]</kbd> back/forward, <kbd>Tab</kbd> moves focus.

## JavaScript

```html
<button onclick="count()">+1</button> <span id="n">0</span>
<script>
  let n = 0;
  function count() { document.getElementById('n').textContent = ++n; }
</script>
```

The browser compiles nothing at runtime: it loads the bundled `js.wasm` (boa) and hands it the source. See [doc/javascript.md](doc/javascript.md).

## Scripts in Rust

```html
<button id="inc">+1</button> <span id="count">0</span>
<script type="application/wasm" src="counter.wasm"></script>
```

```rust
use rubrowser::sdk::*;

fn init() {
    let count = query("#count").unwrap();
    let mut n = 0;
    query("#inc").unwrap().on("click", move |_| {
        n += 1;
        count.set_text(&n.to_string());
    });
}

rubrowser::script!(init);
```

Build the script with `crate-type = ["cdylib"]` and only the `sdk` feature (no dependencies):

```toml
rubrowser = { version = "0.1", default-features = false, features = ["sdk"] }
```

```bash
cargo build --release --target wasm32-unknown-unknown
```

See [doc/scripting.md](doc/scripting.md) for the API and [doc/abi.md](doc/abi.md) for the low-level interface.

## Canvas version

The same browser compiled to wasm and drawn on one `<canvas>`:

```bash
./scripts/build.sh
```

```bash
python3 -m http.server -d web 8000
```

Then open <http://localhost:8000>. Append `#URL` to open a page, e.g. `http://localhost:8000/#about:todo`.

## Features

| Feature | Use |
|---|---|
| `native` (default) | Native window version (`winit` + `softbuffer`) |
| `web` | Canvas version, built for `wasm32-unknown-unknown` |
| `sdk` | API for writing page scripts; no dependencies |
| `core` | The engine without a platform layer |

## Supported HTML and CSS

A practical subset aimed at pages written for rubrowser: block, inline, inline-block and flex layout; margins, padding, borders, rounded corners, backgrounds; fonts, line height, text alignment, white-space, decorations, lists; `overflow` scrolling, absolute positioning, `:hover`, `:active`, `:focus`; links and text inputs. See [doc/plan.md](doc/plan.md) for details and [doc/architecture.md](doc/architecture.md) for how it works.

Not supported yet: images, tables, floats, CJK fonts and IME.

## License

MIT. The bundled DejaVu fonts are under the Bitstream Vera license.
