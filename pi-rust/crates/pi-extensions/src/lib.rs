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
mod bridge;
mod deflate;
mod digest;
mod error;
mod host;
mod loader;
mod registry;
mod shim;

pub use api::*;
pub use bridge::*;
pub use error::*;
pub use host::*;
pub use loader::*;
pub use registry::*;
pub use shim::*;
