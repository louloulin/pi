//! Offline-first evaluation harness and suites for the Pi Rust port.
//!
//! This crate is the Rust counterpart of the upstream TypeScript
//! `packages/evals` package. It keeps the upstream workflow — **case → run →
//! judge → summary → artifact** — while making the default path deterministic
//! and network-free:
//!
//! * [`harness`] defines the generic pieces: [`Case`], [`Judge`],
//!   [`EvalSuite`], [`run_suites`] and the [`EvalReport`] artifact writer.
//! * [`fixture`] serves the OpenAI-compatible wire protocol from a loopback
//!   `std::net` HTTP server, so provider evals exercise real request
//!   building, SSE parsing and usage accounting without a network.
//! * [`suites`] contains the ported suites: `smoke`, `models`, `providers`,
//!   `extensions` and `docs`.
//!
//! Cases that genuinely need a real API key are declared with
//! [`CaseBuilder::skip_unless_env`], so a default run records them as
//! `skipped` rather than failing. Run them with:
//!
//! ```text
//! PI_EVAL_LIVE=1 OPENAI_API_KEY=sk-... cargo run -p pi-evals --example run_evals -- --live
//! ```
//!
//! `crates/pi-evals/README.md` maps every upstream file and its assertions to
//! the Rust case that covers it, and lists the documented divergences.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod fixture;
pub mod harness;
pub mod suites;

mod support;

pub use fixture::{sse_text, sse_tool_call, FixtureResponse, FixtureServer, RecordedRequest};
pub use harness::{
    run_suites, AcceptAllJudge, AssertionJudge, Case, CaseBuilder, CaseFuture, CaseOutput,
    CaseResult, CaseStatus, EvalError, EvalReport, EvalSuite, Judge, Judgment, ReportTotals,
    RunOptions, SuiteReport, TokenUsage, TranscriptEvent,
};
pub use suites::{all_suites, suite_named};
