//! Platform layers: put the browser's canvas on screen and feed it input.

#[cfg(feature = "native")]
pub mod native;
#[cfg(all(feature = "web", target_arch = "wasm32"))]
pub mod web;
