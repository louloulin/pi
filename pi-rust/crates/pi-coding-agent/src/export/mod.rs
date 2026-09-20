//! Session export: self-contained HTML and JSONL.
//!
//! Ports the export half of upstream `packages/coding-agent`:
//!
//! | upstream | here |
//! | --- | --- |
//! | `core/export-html/index.ts` | [`html`] + [`theme`] + [`session_file`] |
//! | `core/export-html/{template.html,template.css,template.js,vendor/*}` | `assets/export-html/**` (verbatim) |
//! | `core/session-export.ts` | [`jsonl`] |
//! | `cli/args.ts` + `main.ts` `--export` | [`export_from_file`] |
//! | `agent-session.ts` `exportToHtml` / `exportToJsonl` | [`export_active_session_html`] / [`export_active_session_jsonl`] |
//!
//! See `docs/SESSION_EXPORT.md` for the known divergences
//! (`preRenderCustomTools` is not ported, and the `/export` default file name
//! follows the issue spec rather than upstream's `pi-session-<basename>`).

pub mod html;
pub mod jsonl;
pub mod session_file;
pub mod theme;

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use html::{base64_encode, generate_html};
pub use jsonl::{generate_jsonl, timestamped_session_file_name};
pub use session_file::{read_session_file, session_data_from_messages};
pub use theme::{export_colors, generate_theme_vars, resolved_theme_colors};

/// Product name used in generated file names (`APP_NAME` upstream).
pub const APP_NAME: &str = "pi";

/// Session file format version written into exported headers
/// (`CURRENT_SESSION_VERSION` upstream).
pub const CURRENT_SESSION_VERSION: u32 = 3;

/// The payload injected into the HTML template
/// (upstream `interface SessionData` in `export-html/index.ts`).
///
/// Field names and nesting match upstream byte for byte because
/// `template.js` consumes them directly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionData {
    /// Session header entry (`{"type":"session","version","id","timestamp","cwd"}`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<Value>,
    /// Session entries in upstream shape.
    #[serde(default)]
    pub entries: Vec<Value>,
    /// Id of the leaf entry the transcript was rendered from
    /// (upstream `leafId: string | null`).
    #[serde(rename = "leafId", default)]
    pub leaf_id: Option<String>,
    /// Active system prompt, when a live agent state was available.
    #[serde(
        rename = "systemPrompt",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub system_prompt: Option<String>,
    /// Tool definitions handed to the model, when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolInfo>>,
    /// Pre-rendered custom-tool HTML keyed by tool-call id.
    ///
    /// Always `None` in this port: see the `preRenderCustomTools`
    /// divergence note in the module docs.
    #[serde(
        rename = "renderedTools",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub rendered_tools: Option<Value>,
}

/// The subset of a tool definition the export payload carries
/// (upstream `Pick<ToolDefinition, "name" | "description" | "parameters">`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolInfo {
    /// Tool name.
    pub name: String,
    /// Description shown to the model.
    pub description: String,
    /// JSON Schema of the arguments object.
    pub parameters: Value,
}

/// Everything that can go wrong during an export.
///
/// The `Display` strings are the upstream user-facing messages
/// (`File not found: …`, `Session file is not a valid pi session: …`,
/// `Nothing to export yet - start a conversation first`), because `main.ts`
/// prints them verbatim behind its `Error: ` prefix.
#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    /// `--export` pointed at a path that does not exist.
    #[error("File not found: {0}")]
    FileNotFound(String),
    /// The file exists but does not start with a session header.
    #[error("Session file is not a valid pi session: {0}")]
    InvalidSession(String),
    /// `/export` was run before any message was recorded.
    #[error("Nothing to export yet - start a conversation first")]
    NothingToExport,
    /// Reading the session file failed.
    #[error("Failed to read {path}: {message}")]
    Io {
        /// Path that failed to read.
        path: String,
        /// Underlying OS error.
        message: String,
    },
    /// Writing the export failed.
    #[error("Failed to write {path}: {message}")]
    Write {
        /// Path that failed to write.
        path: String,
        /// Underlying OS error.
        message: String,
    },
}

/// Export a session file to HTML (upstream `exportFromFile`, used by
/// `pi --export <session.jsonl> [output.html]`).
///
/// `output` defaults to `pi-session-<input-basename>.html` in the current
/// working directory, matching upstream. The output path is only
/// normalised (not absolutised), so a relative request stays relative in
/// the returned path and in the CLI's `Exported to: …` line, exactly as
/// upstream's `normalizePath` behaves.
pub fn export_from_file(input: &Path, output: Option<&Path>) -> Result<PathBuf, ExportError> {
    let data = session_file::read_session_file(input)?;
    let target = match output {
        Some(path) => crate::paths::normalize_lexically(path),
        None => default_file_export_path(input),
    };
    write_export(&target, &generate_html(&data, None))
}

/// Export a live session to HTML (upstream `AgentSession::exportToHtml`).
///
/// `theme_name` comes from the running TUI theme; `None` falls back to the
/// default (`dark`) theme, which is what the CLI path uses.
pub fn export_active_session_html(
    data: &SessionData,
    output: Option<&Path>,
    theme_name: Option<&str>,
) -> Result<PathBuf, ExportError> {
    let target = match output {
        Some(path) => crate::paths::normalize_lexically(path),
        None => PathBuf::from(timestamped_session_file_name("html")),
    };
    write_export(&target, &generate_html(data, theme_name))
}

/// Export a live session to JSONL (upstream `AgentSession::exportToJsonl`).
///
/// `output` defaults to `session-<ISO>.jsonl` in the current working
/// directory.
pub fn export_active_session_jsonl(
    data: &SessionData,
    output: Option<&Path>,
) -> Result<PathBuf, ExportError> {
    let target = match output {
        Some(path) => crate::paths::normalize_lexically(path),
        None => PathBuf::from(timestamped_session_file_name("jsonl")),
    };
    write_export(&target, &generate_jsonl(data))
}

/// Default output path for [`export_from_file`]:
/// `pi-session-<input-basename>.html`.
fn default_file_export_path(input: &Path) -> PathBuf {
    let name = input
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("session");
    let stem = name.strip_suffix(".jsonl").unwrap_or(name);
    PathBuf::from(format!("{APP_NAME}-session-{stem}.html"))
}

/// Write `contents` to `path`, creating parent directories first
/// (upstream `mkdirSync(dirname, {recursive: true})` for JSONL, and the
/// same convenience for HTML).
pub fn write_export(path: &Path, contents: &str) -> Result<PathBuf, ExportError> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|error| ExportError::Write {
                path: path.display().to_string(),
                message: error.to_string(),
            })?;
        }
    }
    std::fs::write(path, contents).map_err(|error| ExportError::Write {
        path: path.display().to_string(),
        message: error.to_string(),
    })?;
    Ok(path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_file_export_path_matches_upstream() {
        assert_eq!(
            default_file_export_path(Path::new("/tmp/sessions/session-2024.jsonl")),
            PathBuf::from("pi-session-session-2024.html")
        );
        assert_eq!(
            default_file_export_path(Path::new("session.jsonl")),
            PathBuf::from("pi-session-session.html")
        );
    }

    #[test]
    fn session_data_serialises_upstream_field_names() {
        let value = serde_json::to_value(session_file::fixture_session_data()).unwrap();
        assert!(value.get("leafId").is_some());
        assert!(value.get("systemPrompt").is_some());
        assert_eq!(value["tools"][0]["name"], "bash");
        // `renderedTools` is omitted rather than null.
        assert!(value.get("renderedTools").is_none());
    }
}
