# rubrowser

A browser based on wasm. (RUST)

rubrowser draws an entire web browser on a **blank canvas** — a native window's pixel buffer, or a single HTML `<canvas>`. It lays out and paints **HTML + CSS** itself, and **only executes WebAssembly**: page scripts are wasm modules, and **JavaScript runs inside a JS engine compiled to wasm** ([boa](https://crates.io/crates/boa_engine)). Even the browser's own tabs, toolbar and address bar are an HTML/CSS document rendered by the same engine.

- HTML parsed by [`html5ever`](https://crates.io/crates/html5ever) into rubrowser's own DOM
- Its own CSS engine (parser, selectors, `@media`/`@supports`/`@layer`, `var()`, `calc()`); block, flex and grid layout with [`taffy`](https://crates.io/crates/taffy); inline layout written from scratch
- Text rendered with [`fontdue`](https://crates.io/crates/fontdue): the system's Arial/Georgia when installed (so text wraps like in other browsers), the bundled DejaVu fonts otherwise, and a system CJK font for Chinese/Japanese
- SVG (`<img>`, inline `<svg>`, `mask-image` icons) rasterized with [`resvg`](https://crates.io/crates/resvg)
- Page scripts are wasm modules run by [`wasmi`](https://crates.io/crates/wasmi), sandboxed with fuel and memory limits
- `<script>` JavaScript, inline `onclick` handlers and a familiar DOM API, executed by `js.wasm` (see [doc/javascript.md](doc/javascript.md))

## Install and run

```bash
cargo install rubrowser
```

```bash
rubrowser
```

To play audio and video, build with the `media` feature, which uses FFmpeg (install FFmpeg 8 first, e.g. `brew install ffmpeg`):

```bash
cargo install rubrowser --features media
```

Open a file or URL directly:

```bash
rubrowser page.html
```

Real sites such as Wikipedia and GitHub render close to other browsers. Run without JavaScript, or render a page to a PNG without opening a window:

```bash
rubrowser --no-js https://en.wikipedia.org/wiki/Rust_(programming_language)
```

```bash
rubrowser --screenshot out.png --width 1280 --height 900 https://github.com/rust-lang/rust
```

Debugging aids for `--screenshot`: `RUBROWSER_DUMP=selector` prints the layout boxes under an element, `RUBROWSER_PROFILE=1` prints style and layout times.

Built-in pages: `about:home`, `about:counter`, `about:todo`, `about:stopwatch`, `about:events` (Rust/wasm), `about:js-todo`, `about:calc` (JavaScript) and `about:form`.

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

## Other languages

Any interpreter compiled to wasm can be plugged in by a page — the browser stays unchanged:

```html
<link rel="rubrowser-runtime" type="text/python" href="python.wasm">
<script type="text/python">print("hello")</script>
```

See [doc/languages.md](doc/languages.md) for the runtime protocol (`rb_alloc` / `rb_eval`).

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
| `media` | Audio and video playback with FFmpeg (`ffmpeg-next`) and `cpal`; AVIF images (dav1d) |
| `web` | Canvas version, built for `wasm32-unknown-unknown` |
| `sdk` | API for writing page scripts; no dependencies |
| `core` | The engine without a platform layer |

## Supported HTML and CSS

A practical subset: block, inline, inline-block, flex, grid and table layout; floats, absolute, fixed and sticky positioning, z-index and stacking contexts; margins, padding, borders (solid, dashed, dotted, double), rounded corners, outlines, opacity; background colors and images; fonts, line height, text alignment, white-space, decorations, `text-transform`, sub/superscript, lists, `::before`/`::after` content with counters; `overflow` scrolling; `:hover`, `:active`, `:focus`; links; images (PNG, JPEG, GIF, WebP, BMP, SVG, `data:` URLs; animated GIF/APNG/WebP; AVIF with the `media` feature), inline SVG (with text), `mask-image`; `#fragment` links; forms (text, password, textarea, checkbox, radio, select, buttons, labels, GET submission); `<details>`. See [doc/plan.md](doc/plan.md), [doc/plan2.md](doc/plan2.md), [doc/plan3.md](doc/plan3.md) and [doc/architecture.md](doc/architecture.md).

Also: gradients, box and text shadows, `filter`, `clip-path` shapes, 2D transforms, CSS animations and transitions, multi-column layout, container queries, vertical writing, `text-overflow`, `line-clamp`, `word-break`/`overflow-wrap`, `object-fit`, web fonts (WOFF/WOFF2), color emoji, right-to-left text with Arabic shaping, CJK line breaking, `<iframe>`, `<canvas>` 2D, `<video>`/`<audio>` (feature `media`), range/color/file inputs, `<progress>`/`<meter>`, ES modules, `Intl` (en-US), same-origin `fetch`/`XMLHttpRequest`, custom elements, form POST. See [doc/plan4.md](doc/plan4.md).

Not supported yet: IME, cross-origin requests from scripts, scripts inside iframes, WebGL, canvas shadows and filters.

## License

MIT. The bundled DejaVu fonts are under the Bitstream Vera license.
