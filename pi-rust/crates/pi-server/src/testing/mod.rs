//! Test doubles — Rust port of `packages/server/src/testing/**`.
//!
//! The doubles are public so downstream crates can reuse them, and are the
//! backbone of this crate's offline conformance tests.

pub mod client;
pub mod host;

pub use client::ProtocolTestClient;
pub use host::{Gate, TestHarness, TestServerHost};
