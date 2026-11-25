# Changelog

## Unreleased

- **Developer tools** (F12, Cmd+Opt+I, Ctrl+Shift+I), docked below the page: Elements (DOM tree, box model outline on the page, attributes, matching rules with declarations that can be turned off, computed values, edits, picking an element), Console (levels, uncaught errors, a JavaScript line with history), Network (requests with headers and bodies), Sources, Application (localStorage, sessionStorage, cookies, `blob:` URLs) and Performance (script call time and instructions, frame times). `--devtools PANEL[:SELECTOR]` for screenshots.
- **HTTP**: every method (PUT, DELETE, PATCH, HEAD… were sent as POST before) and the headers scripts set (without forbidden ones); redirects are followed by the browser, so cookies set on the way are kept; 303 (and 301/302 after POST) continue with GET.
- **Cookies**: one jar (cookie_store, MIT OR Apache-2.0) for HTTP and `document.cookie`; HttpOnly cookies are hidden from and protected against scripts; persistent cookies are saved with the profile.
- **Web Storage** kept by the browser: `localStorage` per origin, saved with the profile (5 MB quota, `QuotaExceededError`); `sessionStorage` per tab, surviving reloads and navigation; items as properties.
- **Profile**: the window keeps cookies and local storage in the user's data folder (`--user-data-dir DIR` to choose); screenshots and `--headless` keep nothing unless given one.
- **Observers**: `MutationObserver` records what scripts change (`childList`, `attributes`, `characterData`, `subtree`, filters, old values); `IntersectionObserver` (`root`, `rootMargin`, `threshold`) and `ResizeObserver` follow the layout.
- **Remote debugging** over the Chrome DevTools Protocol: `--remote-debugging-port N` (127.0.0.1 only) and `--headless`; Browser, Target (flattened sessions), Page, Runtime, DOM, CSS, Network, Input, Overlay and Log. New dependency: serde_json (MIT OR Apache-2.0).

## 0.3.0

- **Touch and an on-screen keyboard**: Touch Events and Pointer Events with `pointerType: "touch"`, compatibility mouse events, `touch-action`; a tap clicks, a drag scrolls with momentum, a long press opens the context menu. An on-screen keyboard (`--keyboard auto|always|never`) for text fields the user touched, with layouts for `type`/`inputmode` and an Enter key labelled from `enterkeyhint`.
- **Origin and Sec-Fetch-Site**: POST requests carry `Origin`; requests to potentially trustworthy URLs carry `Sec-Fetch-Site` (Fetch Metadata), so servers can refuse forms posted from other sites. Without a Public Suffix List, other origins are `cross-site`.
- **`--installer URL`**: pages that declare an app package (`<link rel="ruwebos-app">`) get an Install button.
- **WebSocket** (`ws:`/`wss:`, text and binary messages, subprotocols), `focus()`/`blur()` and `document.activeElement`, `pointer-events: none`, ruby annotations, scroll position restored on back/forward and reload, non-ASCII URLs percent-encoded as the URL Standard says.
- **Files**: `<input type=file>` with the native dialog, `Blob`/`File`/`FileReader`/`blob:` URLs, multipart uploads, downloads (`Content-Disposition: attachment`, `<a download>`).
- **Text selection**, copy and a context menu; `<img>` `load`/`error` events with `complete`/`naturalWidth`/`naturalHeight`; a `Referer` on requests (`strict-origin-when-cross-origin`).
- **Input**: keyboard shortcuts with any layout, `keyup`, `KeyboardEvent.repeat`, mouse and pointer events, `hashchange`/`popstate`.
- **Web Audio** oscillators and gain nodes with `AudioParam` automation; media `#t=` fragments, the frame after a seek, `drawImage(video)`.
- **Intl**: every IANA time zone (also `Date`'s local time), zh-TW, sv and en-CA dates; `table-layout: fixed`.
- CSSOM, link URL parts, deep recursion; the `nodeValue` setter, unitless `calc()` numbers, render-blocking stylesheets (up to 3 s).
- **Linux**: cross-build, tests on Debian 13, booting in ruvm.

## 0.2.0

The first release on crates.io.
