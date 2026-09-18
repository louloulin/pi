//! JavaScript shim embedded into the host binary.
//!
//! The shim mirrors the TypeScript `ExtensionAPI` shape and is loaded
//! once per [`JsExtensionHost`](crate::JsExtensionHost) before any
//! extension source is evaluated. Extension source sees a `pi` global
//! plus the host import functions installed by [`host::install_imports`].

/// Source of `runtime/pi-ext-shim.mjs` at compile time.
pub const SHIM_SOURCE: &str = include_str!("../runtime/pi-ext-shim.mjs");