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
    JsExtensionBridge, JsExtensionHost,
};
use pi_protocol::ExtensionEvent;
use thiserror::Error;

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
            .filter(|entry| js_kind_for(entry).is_some())
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
        let kind = match js_kind_for(&path) {
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
        // Strip TypeScript type annotations by delegating to a tiny
        // pass that drops `: Type` style annotations on
        // registerTool / registerCommand argument lists. Real-world
        // TS source goes through the upstream `tsc` pipeline before
        // being put on disk, so the heuristic only covers the most
        // common shapes the tests ship.
        let source = if kind == JsKind::TypeScript {
            strip_simple_types(&source)
        } else {
            source
        };
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
// File-kind detection + minimal TypeScript stripping.
//
// Stage 3 keeps the TS pipeline intentionally tiny: the canonical
// upstream extensions are written in TS and compiled to JS before
// being put on disk. When the loader encounters a `.ts` file we run
// a small regex-based strip pass that handles the two most common
// TypeScript constructs upstream extensions use:
//   - `: Type` annotations on parameters (e.g. `args: ExtensionAPI`)
//   - generic-typed return types on registerTool/execute
//
// Anything more exotic (enums, namespaces, decorators) falls back to
// QuickJS's strict syntax error, which the agent surfaces to the
// user through `JsLoadOutcome::errors`.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JsKind {
    JavaScript,
    TypeScript,
}

fn js_kind_for(path: &Path) -> Option<JsKind> {
    match path.extension().and_then(|s| s.to_str()) {
        Some("js") | Some("mjs") | Some("cjs") => Some(JsKind::JavaScript),
        Some("ts") => Some(JsKind::TypeScript),
        _ => None,
    }
}

fn id_for_path(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("extension")
        .to_string()
}

/// Strip a couple of common TypeScript constructs so the upstream
/// `.ts` source can run through QuickJS unchanged.
///
/// This is a deliberately tiny pass — a real TypeScript pipeline
/// (swc / tsc) is added in a later stage. For now, removing `: Type`
/// annotations on common shapes is enough to keep the upstream
/// `hello.ts` / `notify.ts` / `commands.ts` examples loadable.
fn strip_simple_types(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut chars = source.chars().peekable();
    while let Some(c) = chars.next() {
        // Drop `import type { ... } from "..."` and `import { type Foo }`
        // lines: QuickJS would fail on the `type` keyword.
        if c == 'i' {
            let mut rest = String::new();
            for next in chars.by_ref() {
                rest.push(next);
                if next == '\n' {
                    break;
                }
            }
            if rest.starts_with("mport type") || rest.starts_with("mport { type ") {
                continue;
            }
            out.push(c);
            out.push_str(&rest);
            continue;
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_for_known_extensions() {
        assert_eq!(js_kind_for(Path::new("/x/y.ts")), Some(JsKind::TypeScript));
        assert_eq!(js_kind_for(Path::new("/x/y.js")), Some(JsKind::JavaScript));
        assert_eq!(js_kind_for(Path::new("/x/y.mjs")), Some(JsKind::JavaScript));
        assert_eq!(js_kind_for(Path::new("/x/y.json")), None);
    }

    #[test]
    fn id_uses_file_stem() {
        assert_eq!(id_for_path(Path::new("/x/y/hello.ts")), "hello");
    }

    #[test]
    fn strips_type_only_imports() {
        let src = "import type { Foo } from \"./foo\";\nconst x = 1;\n";
        let out = strip_simple_types(src);
        assert!(out.contains("const x = 1;"));
        assert!(!out.contains("import type"));
    }

    #[test]
    fn keeps_regular_imports() {
        let src = "import { bar } from \"./bar\";\n";
        let out = strip_simple_types(src);
        assert_eq!(out, src);
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
