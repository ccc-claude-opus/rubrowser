//! Language runtimes compiled to wasm (see `doc/plan2.md`).

#[cfg(feature = "js-runtime")]
pub mod js;
#[cfg(feature = "js-runtime-rujse")]
pub mod js_rujse;
