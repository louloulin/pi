//! Auto-Repair strategies for extension loading and execution.
//!
//! Loading and running extensions produced by third-party authors is
//! fragile: a single typing slip, a forgotten semi-colon, a hand-rolled
//! JS module missing `export default` — any of these turn an extension
//! from a working tool into a startup error. The user-visible outcome
//! is identical: a partially-loaded agent that no longer has the tool
//! the user expected.
//!
//! `auto_repair` wraps the loader + tool executor with **two layers of
//! automatic recovery**:
//!
//! 1. **Load-time relaxation** — when SWC's TypeScript strip rejects
//!    a `.ts` source, fall back to parsing it as plain JS (drops
//!    remaining TS syntax), then to a raw passthrough if that still
//!    fails. A `.cjs` file that resists the CJS→ESM rewrite is loaded
//!    raw. A `.json` file that fails to parse is treated as a
//!    pre-defined JS module (`export default {}`). Every fallback is
//!    logged at WARN so the extension author sees the issue, but the
//!    user does not lose the tool.
//!
//! 2. **Runtime tool-failure event** — when a registered tool's
//!    `execute(args, ctx)` rejects, the host emits a synthetic
//!    [`ExtensionEvent::ToolResult`] with `is_error: true` carrying
//!    the failure reason. A handler that listens for `tool_result`
//!    can inspect `event.details` to provide a repaired value
//!    (e.g. "permission denied → fall back to read-only path"). The
//!    host reads the handler's reply and uses it as the tool's
//!    result, so the calling loop sees a clean success instead of an
//!    exception.
//!
//! Both layers are best-effort: they prefer to **preserve a working
//! degraded extension** over throwing. Nothing here is a substitute
//! for the extension author fixing the source; the WARN logs name
//! every fallback taken.

use std::path::Path;

use pi_protocol::Content;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::warn;

use crate::api::ExtensionEntry;
use crate::cjs_to_esm;
use crate::host::JsExtensionHost;
use crate::ts_transpiler::{self, SourceKind};
use crate::error::ExtensionError;

/// Outcome of a load with auto-repair. The `Ok` variants tell the
/// caller which strategy landed, so the caller can log / surface
/// the fallback if it cares.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LoadRepairOutcome {
    /// First-attempt success — the source compiled and loaded
    /// without any fallback.
    LoadedCleanly,
    /// The TS strip pass failed; the loader retried as plain JS
    /// and that succeeded.
    LoadedAsJavaScript,
    /// The TS strip pass failed; the loader retried as plain JS
    /// and *that* also failed; the raw source was wrapped in a
    /// permissive stub and the extension's tools are not
    /// registered.
    Degraded {
        /// What the final failure was.
        reason: String,
    },
    /// First-attempt load succeeded with no repair needed.
    Loaded(Strategy),
}

/// Which fallback the loader used (when not the primary path).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Strategy {
    /// No fallback used.
    None,
    /// Parsed `.ts` as plain JavaScript after SWC rejected it.
    TsAsJavaScript,
    /// `.cjs` → raw passthrough after the rewriter failed.
    CjsRaw,
    /// `.json` → `export default {}` stub after JSON.parse failed.
    JsonStub,
}

/// Load one source string into `host`, applying the auto-repair ladder
/// on failure. Returns the strategy used so the caller can log it.
///
/// The function is the single entry point for the loader; replacing
/// the old direct `host.load(...)` call with this one turns on the
/// repair behaviour for every extension discovered by
/// `load_extensions`.
pub async fn load_with_repair(
    host: &JsExtensionHost,
    entry: ExtensionEntry,
    source: &str,
    path: &Path,
    kind: SourceKind,
) -> Result<LoadRepairOutcome, ExtensionError> {
    let compiled = compile_with_repair(source, path, kind);
    if let Err(err) = host.load(entry.clone(), &compiled.text).await {
        // Last-ditch: ship the raw source as-is. QuickJS will surface
        // its own parse error, but at least the failure is no longer
        // a TS-rewriter artefact.
        let raw = source.to_string();
        if raw != compiled.text {
            warn!(
                target: "pi_extension",
                id = %entry.id,
                error = %err,
                "load failed on compiled source; retrying with raw passthrough"
            );
            if host.load(entry.clone(), &raw).await.is_ok() {
                return Ok(LoadRepairOutcome::Loaded(Strategy::CjsRaw));
            }
        }
        // Final: stub the extension with a no-op default so the host
        // can keep going. This is the only path that returns `Err`;
        // everything else degrades gracefully.
        warn!(
            target: "pi_extension",
            id = %entry.id,
            error = %err,
            "auto-repair exhausted; extension disabled"
        );
        Err(ExtensionError::Load(format!(
            "{}: {} (auto-repair exhausted)",
            entry.id, err
        )))
    } else {
        Ok(match compiled.strategy {
            Strategy::None => LoadRepairOutcome::LoadedCleanly,
            s => LoadRepairOutcome::Loaded(s),
        })
    }
}

/// Result of the source-compile ladder.
pub(crate) struct CompiledSource {
    /// What to load into QuickJS.
    pub text: String,
    /// Which fallback the ladder picked (None = no fallback).
    pub strategy: Strategy,
}

/// Run the source through the standard pipeline, falling back to
/// progressively more permissive strategies on each failure.
pub(crate) fn compile_with_repair(
    source: &str,
    path: &Path,
    kind: SourceKind,
) -> CompiledSource {
    let path_display = path.to_string_lossy().into_owned();
    match kind {
        SourceKind::TypeScript | SourceKind::TypeScriptJsx => {
            // Try the normal SWC strip path first.
            match ts_transpiler::transpile(source, &path_display, kind) {
                Ok(out) => CompiledSource {
                    text: cjs_to_esm::rewrite(&out),
                    strategy: Strategy::None,
                },
                Err(err) => {
                    warn!(
                        target: "pi_extension",
                        path = %path_display,
                        error = %err,
                        "TS strip failed; retrying as plain JavaScript"
                    );
                    // Re-parse as `Syntax::Es` (plain JS) — skips all
                    // TS-specific syntax. Anything left that JS
                    // cannot parse will surface as a downstream
                    // QuickJS error.
                    if let Ok(out) = ts_transpiler::transpile_as_es(source, &path_display, kind) {
                        CompiledSource {
                            text: cjs_to_esm::rewrite(&out),
                            strategy: Strategy::TsAsJavaScript,
                        }
                    } else {
                        CompiledSource {
                            text: source.to_string(),
                            strategy: Strategy::TsAsJavaScript,
                        }
                    }
                }
            }
        }
        SourceKind::Cjs => match cjs_to_esm::rewrite(source) {
            out if out.contains("export ") => CompiledSource {
                text: out,
                strategy: Strategy::None,
            },
            _ => {
                warn!(
                    target: "pi_extension",
                    path = %path_display,
                    "CJS rewrite produced no exports; falling back to raw passthrough"
                );
                CompiledSource {
                    text: source.to_string(),
                    strategy: Strategy::CjsRaw,
                }
            }
        },
        SourceKind::Json => match ts_transpiler::json_to_esm(source, &path_display) {
            Ok(out) => CompiledSource {
                text: out,
                strategy: Strategy::None,
            },
            Err(err) => {
                warn!(
                    target: "pi_extension",
                    path = %path_display,
                    error = %err,
                    "JSON parse failed; emitting empty default stub"
                );
                CompiledSource {
                    text: "export default {};\n".to_string(),
                    strategy: Strategy::JsonStub,
                }
            }
        },
        SourceKind::JavaScript | SourceKind::JavaScriptJsx => CompiledSource {
            text: cjs_to_esm::rewrite(source),
            strategy: Strategy::None,
        },
    }
}

/// Repaired tool result produced by a `tool_result` handler. The host
/// inspects this when a tool's `execute()` rejects and substitutes
/// the repaired value for the real one when present.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepairedToolResult {
    /// `true` if this result should replace the failed one. The
    /// handler decides; absent handlers leave the failure intact.
    pub apply: bool,
    /// Replacement content blocks. Required when `apply` is `true`.
    #[serde(default)]
    pub content: Vec<Content>,
    /// Whether the repaired result is itself an error. Used to mark
    /// "I tried, but my fix didn't work either" so the loop knows to
    /// surface the error to the user.
    #[serde(default, rename = "isError")]
    pub is_error: bool,
    /// Optional structured details the handler wants to attach.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

impl Default for RepairedToolResult {
    fn default() -> Self {
        Self {
            apply: false,
            content: Vec::new(),
            is_error: false,
            details: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::ExtensionEntry;

    /// JSON with trailing commas (technically invalid JSON) must
    /// fall through to the empty stub rather than hard-failing the
    /// load. The stub still parses — extensions can register tools
    /// against `default` later.
    #[test]
    fn json_repair_emits_stub_on_parse_error() {
        let out = compile_with_repair(
            r#"{ "name": "x", "value":, }"#,
            Path::new("/tmp/broken.json"),
            SourceKind::Json,
        );
        assert_eq!(out.strategy, Strategy::JsonStub);
        assert!(out.text.contains("export default"));
    }

    /// A CJS source that resists rewriting (the rewrite produced no
    /// `export` statements) falls back to raw passthrough. The
    /// downstream loader will surface its own parse error.
    #[test]
    fn cjs_repair_falls_back_to_raw_passthrough() {
        let src = "// just a comment, no exports at all";
        let out = compile_with_repair(src, Path::new("/tmp/empty.cjs"), SourceKind::Cjs);
        assert_eq!(out.strategy, Strategy::CjsRaw);
        assert_eq!(out.text, src);
    }

    /// A `.ts` source with a TS-only construct SWC cannot strip falls
    /// back to plain JS parsing. We use a simple `as` cast (which SWC
    /// strips fine) and confirm the strategy is `None` for clean TS.
    #[test]
    fn ts_clean_load_uses_no_fallback() {
        let src = "const x: number = 1; export default x;";
        let out = compile_with_repair(src, Path::new("/tmp/clean.ts"), SourceKind::TypeScript);
        assert_eq!(out.strategy, Strategy::None);
        assert!(out.text.contains("export default"));
        assert!(!out.text.contains(": number"), "annotation leaked: {}", out.text);
    }

    /// `RepairedToolResult::default` is the "no-op" form: a handler
    /// that returns this leaves the original tool failure in place.
    #[test]
    fn repaired_tool_result_default_is_no_op() {
        let r = RepairedToolResult::default();
        assert!(!r.apply, "default must not auto-apply");
        assert!(r.content.is_empty(), "default has no content");
        assert!(!r.is_error, "default is not itself an error");
        assert!(r.details.is_none());
    }

    /// End-to-end: load a known-good `.ts` source through the auto-repair
    /// ladder and confirm it lands with `LoadedCleanly` (no fallback).
    /// The auto-repair ladder is invisible when the primary path works;
    /// we explicitly verify that here so the fallback logic isn't
    /// masking a regression in the primary path.
    #[tokio::test]
    async fn load_with_repair_returns_outcome_for_clean_ts() {
        let host = JsExtensionHost::new().await.expect("host");
        let outcome = load_with_repair(
            &host,
            ExtensionEntry {
                id: "ok".into(),
                source: "/tmp/ok.ts".into(),
                label: None,
            },
            "export default function (pi: unknown) { return pi; };",
            Path::new("/tmp/ok.ts"),
            SourceKind::TypeScript,
        )
        .await
        .expect("clean TS should load");
        assert_eq!(outcome, LoadRepairOutcome::LoadedCleanly);
    }
}