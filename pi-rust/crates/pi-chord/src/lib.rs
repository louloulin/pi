//! Chord: application composition runtime (Rust port of `packages/chord`).
//!
//! `packages/chord` is the application composition layer of Pi: a typed service graph ("facets"),
//! replicated state that travels as JSON deltas, and a host context that threads cancellation
//! through every call. This crate ports its *core layer* — the parts that have no filesystem,
//! clock, socket or thread-pool dependency — so the same semantics are available to the Rust
//! rewrite:
//!
//! | Module | Upstream | Contents |
//! | --- | --- | --- |
//! | [`json`] | `json.ts` | JSON value validation and normalisation |
//! | [`delta`] | `delta/index.ts` | paths, ops, apply, diff, tracker, wire codec |
//! | [`context`] | `context/index.ts` | `Context`, `ContextKey`, abort signals, `awaitWithContext` |
//! | [`types`] | `types.ts` | facet/service/state vocabulary |
//! | [`state`] | `services/state.ts` | replicated state producer, live view and cold replica |
//! | [`facets`] | `facets/*` | facet lifecycle, local service directory, host kernel, loaders |
//! | [`api`] | `api.ts` | the public entry points |
//!
//! Deliberately out of scope here: `services/` remote transport (`provider`, `consumer`,
//! `service-wire`, `loopback`, `facet-loader`'s service sources) and `node/*` — Stage 18/19 own
//! those. See `crates/pi-chord/README.md` for the full mapping and the documented deviations.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod api;
pub mod context;
pub mod delta;
pub mod facets;
pub mod json;
pub mod state;
pub mod types;

pub use api::*;
