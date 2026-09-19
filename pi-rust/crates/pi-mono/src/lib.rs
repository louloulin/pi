//! `pi-mono` — workspace-level glue. Re-exports the public surface other
//! crates and downstream binaries consume.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub use pi_agent_core as agent_core;
pub use pi_ai as ai;
pub use pi_chord as chord;
pub use pi_extensions as extensions;
pub use pi_protocol as protocol;
pub use pi_telemetry as telemetry;
