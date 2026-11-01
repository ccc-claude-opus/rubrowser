//! rubrowser: a browser whose pages are Rust programs compiled to WebAssembly.
//!
//! The browser has no native widgets. Everything (address bar, tabs, buttons,
//! page content) is drawn into a single pixel buffer, which a platform layer
//! puts on screen: a native window (`native` feature) or an HTML `<canvas>`
//! (`web` feature).
//!
//! Page authors only need the `sdk` feature, which has no dependencies.

#[cfg(feature = "core")]
pub mod browser;
#[cfg(feature = "core")]
pub mod gfx;
#[cfg(any(feature = "native", feature = "web"))]
pub mod platform;
#[cfg(feature = "sdk")]
pub mod sdk;
