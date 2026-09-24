//! `pi-extensions` — JS extension host crate.
//!
//! Stage 0 declares the loader trait + extension descriptor. Stage 3
//! wires the JS shim and the embedded QuickJS runtime so existing pi
//! extensions (written against the TypeScript `ExtensionAPI`) can run
//! unmodified inside the Rust host.
//!
//! ## Architecture
//!
//! ```text
//!  ┌──────────────────────┐     JSON strings     ┌────────────────────┐
//!  │  pi-extensions host  │ ◄──────────────────► │  QuickJS runtime   │
//!  │  (host.rs + bridge)  │     host imports      │  + pi-ext-shim.mjs │
//!  └─────────┬────────────┘                      └────────┬───────────┘
//!            │ events / tool calls / UI requests           │
//!            ▼                                             ▼
//!  ┌──────────────────────┐                      ┌────────────────────┐
//!  │  pi-coding-agent     │                      │  user extension .js │
//!  │  extensions/loader   │                      │  via _pi_load_ext  │
//!  └──────────────────────┘                      └────────────────────┘
//! ```
//!
//! The host exposes [`JsExtensionHost`]; the bridge layer implements
//! [`ExtensionBridge`](api::ExtensionBridge) for the agent.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod api;
pub mod auto_repair;
pub mod autocomplete;
pub mod bridge;
pub mod cjs_to_esm;
mod deflate;
mod digest;
mod error;
mod events;
pub mod extension_isolation;
mod hook;
mod host;
mod loader;
pub mod module_cache;
mod pi_ai;
mod registry;
mod shim;
pub mod ts_transpiler;
pub mod wasm_extension;

pub use api::*;
pub use auto_repair::*;
pub use autocomplete::*;
pub use bridge::*;
pub use cjs_to_esm::*;
pub use error::*;
pub use events::*;
pub use extension_isolation::*;
pub use hook::*;
pub use host::*;
pub use loader::*;
pub use module_cache::*;
pub use pi_ai::*;
pub use registry::*;
pub use shim::*;
pub use ts_transpiler::*;
pub use wasm_extension::*;
