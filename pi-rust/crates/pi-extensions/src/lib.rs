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
pub mod capability_manifest;
pub mod cjs_to_esm;
pub mod compatibility_scanner;
mod deflate;
pub mod exec_mediation;
mod digest;
mod error;
mod events;
pub mod extension_isolation;
mod hook;
mod host;
pub mod fs_connector;
pub mod secret_broker;
mod loader;
pub mod module_cache;
mod pi_ai;
mod registry;
pub mod runtime_risk;
mod shim;
pub mod ts_transpiler;
pub mod ui_leases;
pub mod wasm_extension;

pub use api::*;
pub use auto_repair::*;
pub use autocomplete::*;
pub use bridge::*;
pub use capability_manifest::*;
pub use cjs_to_esm::*;
pub use compatibility_scanner::*;
pub use exec_mediation::*;
pub use fs_connector::*;
pub use secret_broker::*;
pub use error::*;
pub use events::*;
pub use extension_isolation::*;
pub use hook::*;
pub use host::*;
pub use loader::*;
pub use module_cache::*;
pub use pi_ai::*;
pub use registry::*;
pub use runtime_risk::*;
pub use shim::*;
pub use ts_transpiler::*;
pub use ui_leases::*;
pub use wasm_extension::*;
