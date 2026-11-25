# rubrowser

A browser based on wasm. (RUST)

rubrowser draws an entire web browser on a **blank canvas** — a native window's pixel buffer, or a single HTML `<canvas>`. It lays out and paints **HTML + CSS** itself, and **only executes WebAssembly**: page scripts are wasm modules, and **JavaScript runs inside a JS engine compiled to wasm** ([boa](https://crates.io/crates/boa_engine)). Even the browser's own tabs, toolbar and address bar are an HTML/CSS document rendered by the same engine.

- HTML parsed by [`html5ever`](https://crates.io/crates/html5ever) into rubrowser's own DOM
- Its own CSS engine (parser, selectors, `@media`/`@supports`/`@layer`, `var()`, `calc()`); block, flex and grid layout with [`taffy`](https://crates.io/crates/taffy); inline layout written from scratch
- Text rendered with [`fontdue`](https://crates.io/crates/fontdue): the system's Arial/Georgia when installed (so text wraps like in other browsers), the bundled DejaVu fonts otherwise, and a system CJK font for Chinese/Japanese
- SVG (`<img>`, inline `<svg>`, `mask-image` icons) rasterized with [`resvg`](https://crates.io/crates/resvg)
- Page scripts are wasm modules, compiled to machine code by [`wasmtime`](https://crates.io/crates/wasmtime) in the native build and interpreted by [`wasmi`](https://crates.io/crates/wasmi) in the web build, sandboxed with fuel and memory limits
- `<script>` JavaScript, inline `onclick` handlers and a familiar DOM API, executed by `js.wasm` (see [doc/javascript.md](doc/javascript.md))

## Install and run

```bash
cargo install rubrowser
```

```bash
rubrowser
```

It runs on macOS and Linux (X11 or Wayland; building needs a C compiler). [doc/linux.md](doc/linux.md) covers cross-building for Linux from macOS, testing in Debian 13 with Docker, and booting that Debian in the [ruvm](https://github.com/ccc-claude-opus/ruvm) emulator to run rubrowser there. [ruwebos](https://github.com/ccc-claude-opus/ruwebos) builds a phone + desktop system with rubrowser as the only interface, [rucloud](https://github.com/ccc-claude-opus/rucloud) for apps and services, and Linux in ruvm (plan moved from doc/system.md).

To play audio and video, build with the `media` feature, which uses FFmpeg (install FFmpeg 8 first, e.g. `brew install ffmpeg`):

```bash
cargo install rubrowser --features media
```

rubrowser's own code stays MIT either way; the `media` build links to the FFmpeg libraries installed on the machine, so a binary you *distribute* follows that FFmpeg's license: Homebrew's FFmpeg is built with `--enable-gpl` (x264), so such a binary must be distributed under the GPL's terms. To ship binaries under MIT, link against an LGPL build of FFmpeg (configured without `--enable-gpl` / `--enable-nonfree`; playback only needs FFmpeg's own decoders), keep it dynamically linked and include its license notice and source offer. `<video>` and `<audio>` play over HTTP with Range requests, start at a media fragment (`#t=30`, Media Fragments URI 1.0) and can be drawn into a canvas (`drawImage(video, …)`).

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

Debugging aids for `--screenshot`: `RUBROWSER_DUMP=selector` prints the layout boxes under an element, `RUBROWSER_PROFILE=1` prints style and layout times. `RUBROWSER_INPUT="down 300 400; move 360 420; up 360 420; key C cmd; wait 500"` plays mouse and key input before the screenshot (`click`, `right`, `keydown a` / `keyup a` to hold a key, `[`/`]` too; copied text is printed). In the window, `RUBROWSER_KEYS=1` prints each key as rubrowser reads it and `RUBROWSER_AUDIO=1` prints Web Audio commands.

Built-in pages: `about:home`, `about:counter`, `about:todo`, `about:stopwatch`, `about:events` (Rust/wasm), `about:js-todo`, `about:calc` (JavaScript) and `about:form`.

Shortcuts (also with a Zhuyin, Pinyin or other non-Latin keyboard layout): <kbd>Cmd/Ctrl</kbd>+<kbd>L</kbd> address bar, +<kbd>T</kbd> new tab, +<kbd>W</kbd> close tab, +<kbd>R</kbd> reload, +<kbd>[</kbd>/<kbd>]</kbd> back/forward, +<kbd>C</kbd> copy, +<kbd>A</kbd> select all, <kbd>Tab</kbd> moves focus; in text fields also +<kbd>X</kbd> cut and +<kbd>V</kbd> paste.

Select page text by dragging (double-click selects a word, Shift-click extends the selection). Right click (or Ctrl-click on macOS) opens a context menu: copy, select all, back, forward, reload; on a link open / open in a new tab / copy its address / download; on an image open in a new tab / copy its address / download; in a text field cut, copy, paste and select all. A page that cancels the `contextmenu` event shows its own menu instead.

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
rubrowser = { version = "0.2", default-features = false, features = ["sdk"] }
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

A practical subset: block, inline, inline-block, flex, grid and table layout; floats, absolute, fixed and sticky positioning, z-index and stacking contexts; margins, padding, borders (solid, dashed, dotted, double), rounded corners, outlines, opacity; background colors and images; fonts, line height, text alignment, white-space, decorations, `text-transform`, sub/superscript, lists, `::before`/`::after` content with counters; `overflow` scrolling; `:hover`, `:active`, `:focus`; links; images (PNG, JPEG, GIF, WebP, BMP, SVG, `data:` URLs; animated GIF/APNG/WebP; AVIF with the `media` feature), inline SVG (with text), `mask-image`; `#fragment` links; forms (text, password, textarea, checkbox, radio, select, buttons, labels, GET and POST submission with urlencoded, `multipart/form-data` and `text/plain` bodies); `<details>`. See [doc/plan.md](doc/plan.md), [doc/plan2.md](doc/plan2.md), [doc/plan3.md](doc/plan3.md) and [doc/architecture.md](doc/architecture.md).

Also: gradients, box and text shadows, `filter`, `clip-path` shapes, 2D transforms, CSS animations and transitions, multi-column layout, container queries, vertical writing, `text-overflow`, `line-clamp`, `word-break`/`overflow-wrap`, `object-fit`, web fonts (WOFF/WOFF2), color emoji, right-to-left text with Arabic shaping, CJK line breaking, `<iframe>`, `<canvas>` 2D, `<video>`/`<audio>` (feature `media`), range/color inputs, file inputs with the native file dialog, `Blob`/`File`/`FileReader`/`blob:` URLs, downloads (`Content-Disposition: attachment`, `<a download>`), `<progress>`/`<meter>`, ES modules, `Intl` (en-US, plus zh-TW, sv and en-CA dates; every IANA time zone from the host's database, which is also `Date`'s local time), `table-layout: fixed`, same-origin `fetch`/`XMLHttpRequest`, custom elements, form POST, `<img>` `load`/`error` events with `complete`/`naturalWidth`/`naturalHeight` (map libraries such as Leaflet show their tiles), a `Referer` on requests (the default `strict-origin-when-cross-origin` policy; OpenStreetMap's tile servers require it), `Origin` on POST requests and `Sec-Fetch-Site` (Fetch Metadata; without a Public Suffix List, other origins are `cross-site`) so servers can refuse forms posted from other sites, mouse and pointer events (`pointerdown`/`mousedown`, `pointermove`/`mousemove`, `pointerup`/`mouseup`, legacy `which`, `buttons`), `hashchange`/`popstate` and `location.hash = …`, `selectstart` and `user-select: none`, Web Audio oscillators and gain nodes with `AudioParam` automation (sound with the `media` feature), `WebSocket` (`ws:`/`wss:`, text and binary messages, subprotocols), `focus()`/`blur()` and `document.activeElement`, `CSS.supports`/`CSS.escape`, `pointer-events: none`, ruby annotations (`<ruby>`/`<rt>`/`<rp>`, `display: ruby`), scroll position restored on back/forward and reload, non-ASCII URLs percent-encoded as the URL Standard says. Touch input (Touch Events, Pointer Events with `pointerType: "touch"`, compatibility mouse events, `touch-action`): a tap clicks, a drag scrolls and keeps sliding, a long press opens the context menu. An on-screen keyboard (`--keyboard auto|always|never`) comes up for a text field the user touched, with layouts for `type`/`inputmode` (text, URL, email, numbers, phone) and the Enter key labelled from `enterkeyhint`; the page gets shorter and hears `resize`. See [doc/plan4.md](doc/plan4.md) and [doc/files.md](doc/files.md) (uploads and downloads). Developer tools (F12): Elements (with live CSS toggles), Console, Network, Sources, Application and Performance panels, and remote debugging over the Chrome DevTools Protocol (`--remote-debugging-port`, `--headless`), see [doc/devtools.md](doc/devtools.md).

Not supported yet: scripts inside iframes, WebGL, canvas shadows and filters.

What changed in each version: [CHANGELOG.md](CHANGELOG.md).

## License

MIT. The bundled DejaVu fonts are under the Bitstream Vera license.
