# rubrowser

A browser based on wasm. (RUST)

rubrowser does not run HTML or JavaScript. Its pages are **Rust programs compiled to WebAssembly**. The browser has no native widgets: the address bar, tabs, buttons and page content are all drawn by Rust into a single pixel buffer.

> Status: early development (milestone M0). See [doc/plan.md](doc/plan.md).

## Native version (main)

```bash
cargo install rubrowser
```

```bash
rubrowser
```

## Canvas version (extra)

The same browser compiled to wasm and drawn on a single HTML `<canvas>`:

```bash
./scripts/build.sh
```

```bash
python3 -m http.server -d web 8000
```

Then open <http://localhost:8000>.

## Features

| Feature | Use |
|---|---|
| `native` (default) | Native window version (`winit` + `softbuffer`) |
| `web` | Canvas version, built with `--target wasm32-unknown-unknown` |
| `sdk` | API for writing pages; no dependencies |

## License

MIT
