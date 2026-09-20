//! `/export` command handler and the `--export` CLI entry point.
//!
//! Upstream splits this over three places: `interactive-mode.ts`
//! `handleExportCommand` (`.jsonl` → JSONL branch, everything else → HTML),
//! `agent-session.ts` `exportToHtml` / `exportToJsonl`, and `main.ts`'s
//! `--export` block. This module is the command layer that decides *which*
//! export to run and *which* path to write; the format work itself lives in
//! [`crate::export`].

use std::path::{Path, PathBuf};

use pi_protocol::{Message, ToolDefinition};

use crate::export::{
    export_active_session_html, export_active_session_jsonl, export_from_file,
    session_data_from_messages, ExportError,
};

/// Everything the `/export` command needs from the live session.
///
/// Borrowed on purpose: the caller already holds the agent lock, so the
/// exporter copies what it needs into the [`SessionData`](crate::export::SessionData)
/// payload instead of cloning the message log first.
pub struct ActiveSession<'a> {
    /// Session id written into the JSONL / HTML header.
    pub session_id: &'a str,
    /// Working directory recorded in the header.
    pub cwd: &'a Path,
    /// Conversation so far, in the agent's flat message shape.
    pub messages: &'a [Message],
    /// System prompt shown in the HTML header.
    pub system_prompt: &'a str,
    /// Tool definitions handed to the model.
    pub tools: &'a [ToolDefinition],
}

/// `pi --export <session.jsonl> [output.html]` (upstream `main.ts`).
///
/// Errors keep the upstream wording so the CLI can print `Error: …`
/// verbatim.
pub fn run_cli_export(input: &Path, output: Option<&Path>) -> Result<PathBuf, ExportError> {
    export_from_file(input, output)
}

/// `/export [path]`: JSONL when `path` ends in `.jsonl`, HTML otherwise
/// (upstream `handleExportCommand`).
///
/// An empty conversation is rejected with [`ExportError::NothingToExport`]
/// rather than writing an empty HTML file, mirroring the upstream
/// `Nothing to export yet - start a conversation first` guard.
pub fn run_slash_export(
    session: ActiveSession<'_>,
    path: Option<&str>,
    theme_name: Option<&str>,
) -> Result<PathBuf, ExportError> {
    let data = session_data_from_messages(
        session.session_id,
        &session.cwd.display().to_string(),
        session.messages,
        Some(session.system_prompt),
        session.tools,
    );
    if data.entries.is_empty() {
        return Err(ExportError::NothingToExport);
    }

    let output = path.map(Path::new);
    if path.is_some_and(|path| path.ends_with(".jsonl")) {
        export_active_session_jsonl(&data, output)
    } else {
        export_active_session_html(&data, output, theme_name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_protocol::Content;

    fn session<'a>(messages: &'a [Message], tools: &'a [ToolDefinition]) -> ActiveSession<'a> {
        ActiveSession {
            session_id: "test-session",
            cwd: Path::new("/tmp/project"),
            messages,
            system_prompt: "be helpful",
            tools,
        }
    }

    #[test]
    fn slash_export_refuses_an_empty_conversation() {
        let err = run_slash_export(session(&[], &[]), None, None).unwrap_err();
        assert!(matches!(err, ExportError::NothingToExport), "{err}");
        assert_eq!(
            err.to_string(),
            "Nothing to export yet - start a conversation first"
        );
    }

    #[test]
    fn slash_export_selects_the_format_from_the_extension() {
        let dir = tempfile::tempdir().expect("tempdir");
        let messages = vec![Message {
            role: pi_protocol::Role::User,
            content: vec![Content::text("hi")],
            model: None,
        }];

        let html_path = dir.path().join("out.html");
        run_slash_export(session(&messages, &[]), html_path.to_str(), Some("dark")).expect("html");
        let html = std::fs::read_to_string(&html_path).expect("read html");
        assert!(html.starts_with("<!DOCTYPE html"));

        let jsonl_path = dir.path().join("out.jsonl");
        run_slash_export(session(&messages, &[]), jsonl_path.to_str(), Some("dark"))
            .expect("jsonl");
        let jsonl = std::fs::read_to_string(&jsonl_path).expect("read jsonl");
        assert!(jsonl.ends_with('\n'));
        let header: serde_json::Value =
            serde_json::from_str(jsonl.lines().next().expect("header line")).expect("valid json");
        assert_eq!(header["type"], "session");
    }

    #[test]
    fn cli_export_propagates_the_upstream_error_text() {
        let err = run_cli_export(Path::new("/definitely/missing.jsonl"), None).unwrap_err();
        assert_eq!(err.to_string(), "File not found: /definitely/missing.jsonl");
    }
}
