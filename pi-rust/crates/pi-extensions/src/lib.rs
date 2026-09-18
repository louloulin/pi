//! `pi-extensions` — extension host crate.
//!
//! Stage 0 declares the loader trait + extension descriptor. Stage 3 wires
//! the JS shim and the `wasmtime` embedder so that existing pi extensions
//! (written against the TypeScript `ExtensionAPI`) can run unmodified.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod api;
mod loader;
mod registry;

pub use api::*;
pub use loader::*;
pub use registry::*;
