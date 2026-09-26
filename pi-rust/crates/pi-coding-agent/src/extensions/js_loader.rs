//! JS extension loader — bridges `pi-coding-agent` to the embedded
//! QuickJS host in [`pi_extensions`].
//!
//! The agent already ships a JSON-descriptor loader (kept for backwards
//! compatibility with the original Stage 0 path); this module is the
//! newer JS path: it walks every `.js` / `.mjs` / `.ts` file under the
//! project's extension directories and asks the
//! [`JsExtensionHost`](pi_extensions::JsExtensionHost) to evaluate each.
//!
//! Both paths coexist. `--extensions-dir <dir>` accepts either file
//! shape; the file extension decides which loader runs.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use pi_extensions::{
    ExtensionBridge, ExtensionCapabilities, ExtensionEntry, ExtensionSearchPaths,
    JsExtensionBridge, JsExtensionHost, SourceKind, cjs_to_esm, json_to_esm, transpile_with_cache,
};
use pi_protocol::ExtensionEvent;
use thiserror::Error;

use crate::extensions::required_api::{validate_required_api, RequiredApiReport};

/// Failure mode surfaced by the JS loader.
#[derive(Debug, Error)]
pub enum JsLoaderError {
    /// One of the candidate files could not be read.
    #[error("failed to read {path}: {source}")]
    Read {
        /// File path that failed to load.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// The host rejected the extension source.
    #[error("failed to evaluate extension: {0}")]
    Eval(String),
    /// The host runtime itself failed.
    #[error("host error: {0}")]
    Host(#[from] pi_extensions::ExtensionError),
    /// The extension's `required_api` declared an entry this build
    /// does not implement. The loader refuses to register the
    /// extension so the author sees the gap at startup.
    #[error("extension {path} {report}")]
    RequiredApi {
        path: PathBuf,
        report: RequiredApiReport,
    },
    /// The `required_api` header comment was present but not parseable.
    #[error("extension {path} has malformed @required_api header: {detail}")]
    RequiredApiParse {
        path: PathBuf,
        detail: String,
    },
}

/// Aggregated view of a single load operation: which extensions
/// landed, which ones failed, and the bridge to drive them.
pub struct JsLoadOutcome {
    /// Bridge the agent uses to dispatch events into the loaded
    /// extensions.
    pub bridge: JsExtensionBridge,
    /// Snapshot of every extension entry the host registered.
    pub entries: Vec<ExtensionEntry>,
    /// Per-file load failures (path + reason).
    pub errors: Vec<(PathBuf, JsLoaderError)>,
}

/// Outcome of a single candidate file: success carries the source
/// string and entry, failure carries the loader error.
#[allow(dead_code)]
enum CandidateResult {
    Ok {
        entry: ExtensionEntry,
        source: String,
    },
    Err {
        path: PathBuf,
        error: JsLoaderError,
    },
}

/// One extension load pass: the default search paths plus any paths the
/// user named explicitly (`-e <file>` / `--extensions-dir <dir>`).
#[derive(Debug, Clone, Default)]
pub struct ExtensionLoadRequest {
    /// Default `~/.pi/agent/extensions` + `.pi/extensions` roots.
    pub search: ExtensionSearchPaths,
    /// Extra files or directories named on the command line. Directories
    /// are walked with the same rules as [`ExtensionSearchPaths`].
    pub explicit: Vec<PathBuf>,
}

/// Enumerate JS / TS extension files under the search paths and
/// load each one through the host. Returns the bridge + summary.
pub async fn load_extensions(
    host: JsExtensionHost,
    paths: &ExtensionSearchPaths,
    mode: &str,
    has_ui: bool,
    cwd: &str,
) -> JsLoadOutcome {
    load_candidates(host, paths.candidates(), mode, has_ui, cwd).await
}

/// Like [`load_extensions`], but also loads the files / directories the
/// user named on the command line. Explicit candidates are loaded first
/// and de-duplicated against the default search paths, so `-e` can point
/// at a file that also lives in `.pi/extensions/` without evaluating it
/// twice.
pub async fn load_configured_extensions(
    host: JsExtensionHost,
    request: &ExtensionLoadRequest,
    mode: &str,
    has_ui: bool,
    cwd: &str,
) -> JsLoadOutcome {
    let mut candidates = Vec::new();
    for path in &request.explicit {
        candidates.extend(expand_explicit(path));
    }
    candidates.extend(request.search.candidates());
    load_candidates(host, candidates, mode, has_ui, cwd).await
}

/// Walk an explicit `-e` / `--extensions-dir` path. A file is taken as
/// is; a directory is walked (depth 2) for extension-looking files.
fn expand_explicit(path: &Path) -> Vec<PathBuf> {
    if path.is_dir() {
        walkdir::WalkDir::new(path)
            .max_depth(2)
            .into_iter()
            .filter_map(Result::ok)
            .map(|entry| entry.path().to_path_buf())
            .filter(|entry| SourceKind::for_path(entry).is_some())
            .collect()
    } else {
        vec![path.to_path_buf()]
    }
}

/// Load every candidate that looks like a JS / TS extension. Candidates
/// whose extension is unknown are skipped silently; files that exist but
/// fail to read / evaluate land in [`JsLoadOutcome::errors`].
async fn load_candidates(
    host: JsExtensionHost,
    candidates: Vec<PathBuf>,
    mode: &str,
    has_ui: bool,
    cwd: &str,
) -> JsLoadOutcome {
    let bridge = JsExtensionBridge::new(host.clone(), mode.to_string(), has_ui, cwd.to_string());
    let mut entries = Vec::new();
    let mut errors = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for path in candidates {
        let kind = match SourceKind::for_path(&path) {
            Some(k) => k,
            None => continue,
        };
        if !seen.insert(std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone())) {
            continue;
        }
        let id = id_for_path(&path);
        let source = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(source) => {
                errors.push((
                    path.clone(),
                    JsLoaderError::Read {
                        path: path.clone(),
                        source,
                    },
                ));
                continue;
            }
        };
        // Enforce the `required_api` manifest contract before doing
        // any work. An extension that declares a dependency on an API
        // this build does not implement is refused at load time so the
        // gap is visible immediately, instead of crashing on the first
        // call to a missing method.
        match parse_required_api_header(&source) {
            Ok(Some(declared)) => {
                let report = validate_required_api(&declared);
                if !report.is_satisfied() {
                    errors.push((
                        path.clone(),
                        JsLoaderError::RequiredApi {
                            path: path.clone(),
                            report,
                        },
                    ));
                    continue;
                }
            }
            Ok(None) => { /* no header → no constraint */ }
            Err(detail) => {
                errors.push((
                    path.clone(),
                    JsLoaderError::RequiredApiParse {
                        path: path.clone(),
                        detail,
                    },
                ));
                continue;
            }
        }
        // Compile the source to ESM JavaScript before handing it to
        // the QuickJS host. TS / TSX goes through SWC's `strip` pass
        // (`pi_extensions::transpile`); CJS-flavored sources get a
        // lightweight rewrite of `module.exports` / `require` to
        // their ESM equivalents. JSON files become an `export
        // default <parsed>` module.
        let source = compile_source(&source, &path, kind);
        let entry = ExtensionEntry {
            source: path.clone(),
            id: id.clone(),
            label: None,
        };
        match host.load(entry.clone(), &source).await {
            Ok(()) => entries.push(entry),
            Err(err) => errors.push((path.clone(), JsLoaderError::Eval(format!("{err}")))),
        }
    }

    JsLoadOutcome {
        bridge,
        entries,
        errors,
    }
}

/// Bridge the host's already-loaded extensions into a wrapper the
/// agent's event loop can consume.
pub fn bridge_for(
    host: JsExtensionHost,
    mode: &str,
    has_ui: bool,
    cwd: &str,
) -> Arc<JsExtensionBridge> {
    Arc::new(JsExtensionBridge::new(
        host,
        mode.to_string(),
        has_ui,
        cwd.to_string(),
    ))
}

/// Build the canonical search paths for a given home / cwd.
pub fn search_paths(home: Option<&Path>, cwd: &Path) -> ExtensionSearchPaths {
    ExtensionSearchPaths::from_env(home, cwd)
}

/// Fan out a [`ExtensionEvent`] through every loaded extension via the
/// bridge. The bridge's [`deliver`](pi_extensions::ExtensionBridge)
/// returns `true` when at least one handler subscribed.
pub async fn dispatch(bridge: &JsExtensionBridge, event: &ExtensionEvent) -> bool {
    bridge.deliver(event).await
}

/// Flatten a [`JsLoadOutcome`] into the JSON-descriptor format the
/// JSON loader path already understands. Used by callers that want
/// to log the load results without depending on the JS host types.
pub fn outcome_to_json(outcome: &JsLoadOutcome) -> Vec<serde_json::Value> {
    outcome
        .entries
        .iter()
        .map(|e| {
            serde_json::json!({
                "source": e.source.display().to_string(),
                "id": e.id,
            })
        })
        .collect()
}

/// Suppress the unused-import warning when this module is included
/// without the JSON loader being wired in (e.g. in a smaller binary).
#[allow(dead_code)]
fn _caps_keepalive() -> ExtensionCapabilities {
    ExtensionCapabilities::default()
}

// ---------------------------------------------------------------------------
// Source compilation pipeline.
//
// The loader used to call a tiny `strip_simple_types` regex pass that
// only knew how to drop `import type` lines. That left every real
// upstream extension — which uses full TypeScript — un-loadable. We
// now route `.ts` / `.tsx` through SWC's `strip` transform (see
// `pi_extensions::ts_transpiler`) and `.cjs` through a CJS → ESM
// rewrite before handing the source to the QuickJS host.
// ---------------------------------------------------------------------------

fn compile_source(source: &str, path: &Path, kind: SourceKind) -> String {
    let path_display = path.to_string_lossy().into_owned();
    // Persistent transpiled-source cache. A `None` cache dir disables
    // caching (useful for tests / `--offline` first builds); a `Some`
    // turns repeated loads into a cheap disk lookup. We pass the
    // cache dir at the call site so `compile_source` itself stays
    // pure / unit-testable.
    let cache_dir = module_cache_dir();
    match kind {
        SourceKind::TypeScript | SourceKind::TypeScriptJsx => {
            match transpile_with_cache(source, &path_display, kind, cache_dir.as_deref()) {
                Ok(transpiled) => cjs_to_esm::rewrite(&transpiled),
                Err(err) => {
                    tracing::warn!(target: "pi_extension", path = %path_display, error = %err, "TS transpile failed; passing source through unchanged");
                    source.to_string()
                }
            }
        }
        SourceKind::Cjs => cjs_to_esm::rewrite(source),
        SourceKind::Json => match json_to_esm(source, &path_display) {
            Ok(esm) => esm,
            Err(err) => {
                tracing::warn!(target: "pi_extension", path = %path_display, error = %err, "JSON module conversion failed; passing source through");
                source.to_string()
            }
        },
        SourceKind::JavaScript | SourceKind::JavaScriptJsx => cjs_to_esm::rewrite(source),
    }
}

fn id_for_path(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("extension")
        .to_string()
}

/// Parse the `@required_api` header from a JS / TS extension source.
///
/// Recognised forms:
///
/// ```text
/// // @required_api ["turn_start", "ui.setWidget"]
/// //  @required_api  [ 'turn_start', 'ui.setWidget' ]
/// ```
///
/// The header is recognised only in the first 32 lines (above any
/// `import` / `module.exports` statements that are likely to appear in a
/// real extension) so a comment containing the substring `@required_api`
/// in a doc block does not get picked up.
///
/// Returns:
///
/// * `Ok(Some(vec))` — header present and parseable.
/// * `Ok(None)` — header absent, no constraint to enforce.
/// * `Err(detail)` — header present but malformed; the loader surfaces
///   the detail so the author can fix the manifest.
pub fn parse_required_api_header(source: &str) -> Result<Option<Vec<String>>, String> {
    for line in source.lines().take(32) {
        let trimmed = line.trim_start();
        let Some(rest) = trimmed.strip_prefix("//") else {
            continue;
        };
        let rest = rest.trim_start();
        let Some(rest) = rest.strip_prefix("@required_api") else {
            continue;
        };
        let rest = rest.trim_start();
        // Expect `[ ... ]` (square brackets required so a stray
        // comment like "// @required_api note: …" cannot sneak in).
        let Some(rest) = rest.strip_prefix('[') else {
            return Err("expected `[` after @required_api".to_string());
        };
        let Some(rest) = rest.strip_suffix(']') else {
            return Err("expected closing `]` for @required_api list".to_string());
        };
        let mut entries: Vec<String> = Vec::new();
        for raw in rest.split(',') {
            let token = raw
                .trim()
                .trim_start_matches('"')
                .trim_end_matches('"')
                .trim_start_matches('\'')
                .trim_end_matches('\'')
                .trim();
            if token.is_empty() {
                continue;
            }
            entries.push(token.to_string());
        }
        return Ok(Some(entries));
    }
    Ok(None)
}

/// Resolve the on-disk cache directory for transpiled module sources.
/// Reads `PI_EXTENSION_CACHE_DIR` first (lets operators redirect the
/// cache to a scratch volume), then falls back to
/// `$XDG_CACHE_HOME/pi/extensions` or `$HOME/.cache/pi/extensions`.
/// Returns `None` when no home directory is available — caching is a
/// best-effort optimisation, never a hard requirement.
fn module_cache_dir() -> Option<std::path::PathBuf> {
    if let Ok(dir) = std::env::var("PI_EXTENSION_CACHE_DIR") {
        if !dir.is_empty() {
            return Some(std::path::PathBuf::from(dir));
        }
    }
    if let Some(mut dir) = dirs::cache_dir() {
        dir.push("pi");
        dir.push("extensions");
        return Some(dir);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_for_known_extensions() {
        assert_eq!(SourceKind::for_path(Path::new("/x/y.ts")), Some(SourceKind::TypeScript));
        assert_eq!(SourceKind::for_path(Path::new("/x/y.js")), Some(SourceKind::JavaScript));
        assert_eq!(SourceKind::for_path(Path::new("/x/y.mjs")), Some(SourceKind::JavaScript));
        assert_eq!(SourceKind::for_path(Path::new("/x/y.tsx")), Some(SourceKind::TypeScriptJsx));
        assert_eq!(SourceKind::for_path(Path::new("/x/y.json")), Some(SourceKind::Json));
    }

    #[test]
    fn id_uses_file_stem() {
        assert_eq!(id_for_path(Path::new("/x/y/hello.ts")), "hello");
    }

    #[test]
    fn compiles_typescript_through_swc_strip() {
        // Real-world TS shape: type annotation + interface + import type.
        // The SWC pipeline must drop the type-only bits and keep the
        // value-only ones so the QuickJS host can compile it.
        let src = r#"
            import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
            interface Box<T> { value: T }
            const pi = {} as unknown as ExtensionAPI;
            const x: number = 1;
            export default function (pi: unknown) { return pi; };
        "#;
        let out = compile_source(src, Path::new("/tmp/sample.ts"), SourceKind::TypeScript);
        assert!(!out.contains("interface"), "interface leaked: {out}");
        assert!(!out.contains(": number"), "annotation leaked: {out}");
        assert!(!out.contains("import type"), "import type leaked: {out}");
        assert!(out.contains("export default"), "default missing: {out}");
    }

    #[test]
    fn compiles_cjs_to_esm() {
        let src = "module.exports = function (pi) { return pi; };";
        let out = compile_source(src, Path::new("/tmp/sample.cjs"), SourceKind::Cjs);
        assert!(out.contains("export default"), "default missing: {out}");
        assert!(!out.contains("module.exports ="), "module.exports leaked: {out}");
    }

    #[test]
    fn compiles_json_to_default_export() {
        let src = r#"{"name": "x"}"#;
        let out = compile_source(src, Path::new("/tmp/sample.json"), SourceKind::Json);
        assert!(out.contains("export default"), "default missing: {out}");
        assert!(out.contains("\"name\""), "value missing: {out}");
    }

    #[tokio::test]
    async fn load_extensions_finds_files_under_dir() {
        let dir = std::env::temp_dir().join("pi_extensions_loader_e2e");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(
            dir.join("alpha.js"),
            r#"
                module.exports = function (pi) {
                    pi.registerTool({
                        name: "alpha",
                        label: "Alpha",
                        description: "alpha",
                        parameters: { type: "object" },
                        execute: function () {
                            return { content: [{ type: "text", text: "alpha" }] };
                        },
                    });
                };
            "#,
        )
        .expect("write alpha");
        std::fs::write(
            dir.join("beta.ts"),
            r#"
                module.exports = function (pi) {
                    pi.registerTool({
                        name: "beta",
                        label: "Beta",
                        description: "beta",
                        parameters: { type: "object" },
                        execute: function () {
                            return { content: [{ type: "text", text: "beta" }] };
                        },
                    });
                };
            "#,
        )
        .expect("write beta");

        let paths = ExtensionSearchPaths {
            global: None,
            project: Some(dir.clone()),
        };
        let host = JsExtensionHost::new().await.expect("host");
        let outcome =
            load_extensions(host.clone(), &paths, "tui", true, dir.to_str().unwrap()).await;
        let ids: Vec<&str> = outcome.entries.iter().map(|e| e.id.as_str()).collect();
        assert!(ids.contains(&"alpha"), "alpha not loaded, got {ids:?}");
        assert!(ids.contains(&"beta"), "beta not loaded, got {ids:?}");
        assert!(outcome.errors.is_empty(), "errors: {:?}", outcome.errors);

        // The host should now expose both tools to the agent.
        let tool_names = host.registered_tool_names().await;
        assert!(tool_names.contains(&"alpha".to_string()));
        assert!(tool_names.contains(&"beta".to_string()));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Stage 25: the canonical upstream ESM extension form loads from
    /// disk through the same candidate walk as CommonJS — `import` from a
    /// virtual module, `export default function (pi)`, and
    /// `import.meta.dirname` resolving to the extension's own directory.
    #[tokio::test]
    async fn load_extensions_loads_an_esm_extension_from_disk() {
        use pi_protocol::ResourcesDiscoverReason;

        let dir = std::env::temp_dir().join("pi_extensions_esm_e2e");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(
            dir.join("dynamic.mjs"),
            r#"
                import { join } from "node:path";

                export default function (pi) {
                    pi.on("resources_discover", () => ({
                        skillPaths: [join(import.meta.dirname, "SKILL.md")],
                        promptPaths: [join(import.meta.dirname, "dynamic.md")],
                    }));
                }
            "#,
        )
        .expect("write dynamic.mjs");

        let paths = ExtensionSearchPaths {
            global: None,
            project: Some(dir.clone()),
        };
        let host = JsExtensionHost::new().await.expect("host");
        let outcome = load_extensions(host, &paths, "print", false, dir.to_str().unwrap()).await;
        assert!(outcome.errors.is_empty(), "errors: {:?}", outcome.errors);
        assert_eq!(outcome.entries.len(), 1, "{:?}", outcome.entries);

        let discovered = outcome
            .bridge
            .discover_resources(ResourcesDiscoverReason::Startup)
            .await;
        assert_eq!(discovered.skill_paths, vec![dir.join("SKILL.md")]);
        assert_eq!(discovered.prompt_paths, vec![dir.join("dynamic.md")]);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
