//! Suite registry — the Rust analogue of `packages/evals/src/*.eval.ts`.
//!
//! Each module exports a `suite()` constructor; [`all_suites`] assembles them
//! in the order they should run. Cases that need a live model carry a skip
//! reason instead of being omitted, so a default run reports them explicitly
//! as `skipped` (and `cargo test -p pi-evals` stays green offline).

pub mod docs;
pub mod extensions;
pub mod models;
pub mod providers;
pub mod smoke;

use crate::harness::EvalSuite;

/// Every suite, in run order.
///
/// Network-backed cases inside these suites are gated per case with
/// `CaseBuilder::skip_unless_env`, so this list is safe to run offline:
/// `PI_EVAL_LIVE=1` plus a provider credential is the only way to make a
/// case reach the network.
pub fn all_suites() -> Vec<EvalSuite> {
    vec![
        smoke::suite(),
        models::suite(),
        providers::suite(),
        extensions::suite(),
        docs::suite(),
    ]
}

/// Build one suite by name (`smoke`, `models`, `providers`, `extensions`,
/// `docs`).
pub fn suite_named(name: &str) -> Option<EvalSuite> {
    match name {
        "smoke" => Some(smoke::suite()),
        "models" => Some(models::suite()),
        "providers" => Some(providers::suite()),
        "extensions" => Some(extensions::suite()),
        "docs" => Some(docs::suite()),
        _ => None,
    }
}
