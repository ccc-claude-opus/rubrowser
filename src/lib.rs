//! rubrowser: a browser whose pages are Rust programs compiled to WebAssembly.
//!
//! The browser has no native widgets. Everything (address bar, tabs, buttons,
//! page content) is drawn into a single pixel buffer, which a platform layer
//! puts on screen: a native window (`native` feature) or an HTML `<canvas>`
//! (`web` feature).
//!
//! Page authors only need the `sdk` feature, which has no dependencies.

#[cfg(any(feature = "core", feature = "sdk"))]
pub mod abi;
#[cfg(feature = "core")]
pub mod browser;
#[cfg(feature = "core")]
pub mod css;
#[cfg(feature = "core")]
pub mod document;
#[cfg(feature = "core")]
pub mod dom;
#[cfg(feature = "core")]
pub mod gfx;
#[cfg(feature = "core")]
pub mod input;
#[cfg(feature = "js-runtime")]
pub mod lang;
#[cfg(feature = "core")]
pub mod layout;
#[cfg(feature = "core")]
pub mod net;
#[cfg(feature = "core")]
pub mod paint;
#[cfg(any(feature = "native", feature = "web"))]
pub mod platform;
#[cfg(feature = "core")]
pub mod script;
#[cfg(feature = "sdk")]
pub mod sdk;
#[cfg(feature = "core")]
pub mod text;
#[cfg(feature = "core")]
pub mod ui;
