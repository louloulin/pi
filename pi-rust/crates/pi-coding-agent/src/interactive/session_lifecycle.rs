//! Session attach / branch / name plumbing.
//!
//! Migrated from `interactive.rs` on 2026-09-26 (PR7 of the M1 single-file
//! split; see `scripts/architecture_no_regression.sh`).
//!
//! Every function here is a verb on the driver's *current* session:
//! resume, fork, clone, /new, /name, and the helpers they all funnel
//! through (`attach_session`, `session_directory`, the entry ↔ message
//! converters). The picker / refresh code that depends on the tree and
//! fork selector helpers (still part of the pre-existing baseline
//! gaps from task #11 — `tree_selector_with`, `fork_selector`,
//! `build_session_selector`) stays in `mod.rs` for now and will move
//! when those helpers land.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use pi_agent_core::Agent;
use pi_protocol::{Content, ExtensionEvent, Message, SessionBeforeSwitchReason};
use pi_session::{SessionEntry, SessionReader, SessionWriter};
use pi_tui::app::App;
use pi_tui::message::MessageItem;
use tokio::sync::Mutex as AsyncMutex;

use crate::commands::session::new_session_id;
use crate::commands::tree::CreatedSession;
use crate::session_log::SessionLog;

use super::{extension_veto, deliver_extension_event, InteractiveOptions};

/// Attach the TUI to a stored session picked from `/resume`.
///
/// Restores the transcript (the rendered view *and* the agent's model
/// context), the session identity and its `/name`, so a named session
/// keeps its name across a resume.
pub(super) async fn resume_session(
    app: &mut App,
    agent: &Arc<AsyncMutex<Agent>>,
    options: &mut InteractiveOptions,
    session_id: &str,
) {
    let Some(directory) = session_directory(options) else {
        app.info("/resume: session directory not configured".to_string());
        return;
    };
    let reference = match crate::list_resumable(&directory) {
        Ok(refs) => refs.into_iter().find(|r| r.session_id == session_id),
        Err(err) => {
            app.info(format!("/resume: {err}"));
            return;
        }
    };
    let Some(reference) = reference else {
        app.info(format!("/resume: session {session_id} not found"));
        return;
    };
    let reader = match SessionReader::open(&reference.database) {
        Ok(reader) => reader,
        Err(err) => {
            app.info(format!("/resume: {err}"));
            return;
        }
    };
    // Upstream `session_before_switch` for a resume (`reason: "resume"`),
    // fired once the target exists so a handler sees its file.
    if extension_veto(
        options.extensions.as_ref(),
        ExtensionEvent::SessionBeforeSwitch {
            reason: SessionBeforeSwitchReason::Resume,
            target_session_file: Some(reference.database.display().to_string()),
        },
    )
    .await
    {
        app.info("/resume: cancelled by an extension".to_string());
        return;
    }
    match attach_session(app, agent, options, reference.database, session_id, &reader).await {
        Ok(Some(name)) => app.info(format!("Resumed session {session_id} ({name})")),
        Ok(None) => app.info(format!("Resumed session {session_id}")),
        Err(err) => app.info(format!("/resume: {err}")),
    }
}

/// The directory session files live in, when one is configured.
pub(super) fn session_directory(options: &InteractiveOptions) -> Option<PathBuf> {
    options
        .session_log
        .as_ref()
        .map(|log| log.directory().to_path_buf())
}

/// Point the TUI and the agent at a stored session: transcript, model
/// context, identity, `/name` and the JSONL trail. `/resume`, `/fork`
/// and `/clone` all funnel through here so a resumed and a freshly
/// branched session behave identically.
///
/// Returns the session's display name when it has one.
pub(super) async fn attach_session(
    app: &mut App,
    agent: &Arc<AsyncMutex<Agent>>,
    options: &mut InteractiveOptions,
    database: PathBuf,
    session_id: &str,
    reader: &SessionReader,
) -> anyhow::Result<Option<String>> {
    let entries = reader
        .iter_entries(session_id)?
        .into_iter()
        .map(|decoded| decoded.entry)
        .collect::<Vec<SessionEntry>>();
    let name = reader.session_name(session_id).ok().flatten();
    let messages = entries_to_messages(&entries);

    app.messages_mut().clear();
    for entry in &entries {
        if let Some(item) = entry_to_item(entry) {
            app.messages_mut().push(item);
        }
    }
    agent.lock().await.state_mut().messages = messages;

    options.session_id = session_id.to_string();
    options.session_name = name.clone();
    options.session_database = Some(database);
    options.session_leaf = None;
    // Keep the JSONL trail aimed at the session we just attached to.
    if let Some(directory) = session_directory(options) {
        if let Ok(log) = SessionLog::open(&directory, session_id) {
            options.session_log = Some(log);
        }
    }
    app.set_session_id(session_id);
    app.set_session_name(name.clone());
    Ok(name)
}

/// Open the session database the driver is currently attached to.
pub(super) fn open_current_session(
    app: &mut App,
    options: &InteractiveOptions,
    verb: &str,
) -> Option<(PathBuf, SessionReader)> {
    let Some(database) = options.session_database.clone() else {
        app.info(format!("{verb}: no session database"));
        return None;
    };
    match SessionReader::open(&database) {
        Ok(reader) => Some((database, reader)),
        Err(err) => {
            app.info(format!("{verb}: {err}"));
            None
        }
    }
}

/// Attach the TUI to a freshly created `/fork` or `/clone` session so the
/// user keeps going in the new branch (upstream replaces the runtime).
pub(super) async fn clone_into_new_session(
    app: &mut App,
    agent: &Arc<AsyncMutex<Agent>>,
    options: &mut InteractiveOptions,
    created: CreatedSession,
) {
    let reader = match SessionReader::open(&created.path) {
        Ok(reader) => reader,
        Err(err) => {
            app.info(format!("session {}: {err}", created.session_id));
            return;
        }
    };
    match attach_session(
        app,
        agent,
        options,
        created.path,
        &created.session_id,
        &reader,
    )
    .await
    {
        Ok(_) => app.info(format!(
            "Session branched into {} ({} entries)",
            created.session_id, created.entries
        )),
        Err(err) => app.info(format!("session {}: {err}", created.session_id)),
    }
}

/// Start a fresh session: open a new upstream-v4 session file, reset the
/// agent + view state and point the driver at the new identity.
///
/// This is what both `/new` and `app.session.new` run. It differs from
/// `/clear`, which only clears the message view and keeps the session.
pub(super) async fn start_new_session(
    app: &mut App,
    agent: &Arc<AsyncMutex<Agent>>,
    options: &mut InteractiveOptions,
) {
    // Upstream `session_before_switch`: fired before the current session is
    // replaced, and a handler may veto it (`session_before_switch`).
    if extension_veto(
        options.extensions.as_ref(),
        ExtensionEvent::SessionBeforeSwitch {
            reason: SessionBeforeSwitchReason::New,
            target_session_file: None,
        },
    )
    .await
    {
        app.info("/new: cancelled by an extension".to_string());
        return;
    }
    let Some(directory) = options
        .session_log
        .as_ref()
        .map(|log| log.directory().to_path_buf())
    else {
        app.info("/new: session directory not configured".to_string());
        return;
    };
    let id = new_session_id();
    // Open the JSONL log first so a failure leaves no half-created
    // session behind; the previous session's files are never touched.
    let log = match SessionLog::open(&directory, &id) {
        Ok(log) => log,
        Err(err) => {
            app.info(format!("/new: could not open session log: {err}"));
            return;
        }
    };
    let path = directory.join(format!("{id}.sqlite"));
    if let Err(err) = write_new_session_file(&path, &id) {
        app.info(format!("/new: could not create session file: {err}"));
        return;
    }

    app.messages_mut().clear();
    // A new session starts with an empty composer: the previous draft's text
    // and any image chips belong to the old session.
    app.clear_composer();
    agent.lock().await.state_mut().messages.clear();

    options.session_log = Some(log);
    options.session_id = id.clone();
    options.session_name = None;
    options.session_database = Some(path);
    app.set_session_id(id.clone());
    app.set_session_name(None);
    app.info(format!("Started new session {id}"));
}

/// Create an upstream-v4 session database and stamp the header, using
/// [`SessionWriter`] (never hand-rolled SQL).
pub(super) fn write_new_session_file(path: &Path, session_id: &str) -> anyhow::Result<()> {
    let writer = SessionWriter::open(path)?;
    writer.write_header(SessionEntry::Header {
        id: session_id.to_string(),
        created_at: chrono::Utc::now(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    })?;
    writer.checkpoint()?;
    Ok(())
}

/// Persist the display name to the session's files.
///
/// `/resume` reads the name back from the SQLite `sessions.metadata`, so
/// naming a session that has no database yet also materialises one
/// (header + name) — otherwise the name could not survive a resume.
pub(super) fn persist_session_name(options: &mut InteractiveOptions, name: &str) -> anyhow::Result<()> {
    if options.session_database.is_none() {
        if let Some(directory) = options
            .session_log
            .as_ref()
            .map(|log| log.directory().to_path_buf())
        {
            let path = directory.join(format!("{}.sqlite", options.session_id));
            write_new_session_file(&path, &options.session_id)?;
            options.session_database = Some(path);
        }
    }
    if let Some(path) = options.session_database.clone() {
        let writer = SessionWriter::open(&path)?;
        writer.resume(&options.session_id)?;
        writer.set_session_name(name)?;
        writer.checkpoint()?;
    }
    // The interactive session also has a JSONL trail; record the name
    // there (same `session_name` kind the extension path uses) so the
    // legacy file is self-describing.
    if let Some(log) = options.session_log.as_ref() {
        let _ = log.append_extension("extension", "session_name", serde_json::json!(name));
    }
    Ok(())
}

/// Set `/name <text>`: normalize, persist, then reflect the name in the
/// status bar. Mirrors upstream `handleNameCommand`
/// (`interactive-mode.ts:6193`).
pub(super) async fn set_session_name(app: &mut App, options: &mut InteractiveOptions, raw: &str) {
    let name = normalize_session_name(raw);
    if name.is_empty() {
        app.info("usage: /name <name>".to_string());
        return;
    }
    if options.session_database.is_none() && options.session_log.is_none() {
        app.info("/name: session persistence is disabled — the name was not saved".to_string());
        return;
    }
    match persist_session_name(options, &name) {
        Ok(()) => {
            if name != raw.trim() {
                app.info(format!(
                    "Session name was normalized from {raw:?} to {name:?}"
                ));
            }
            options.session_name = Some(name.clone());
            app.set_session_name(Some(name.clone()));
            // Upstream `session_info_changed`.
            deliver_extension_event(
                options.extensions.as_ref(),
                ExtensionEvent::SessionInfoChanged {
                    name: Some(name.clone()),
                },
            )
            .await;
            app.info(format!("Session name set: {name}"));
        }
        Err(err) => app.info(format!("/name: could not save the session name: {err}")),
    }
}

/// Collapse runs of CR/LF into a single space and trim, matching
/// upstream `appendSessionInfo` (`session-manager.ts:1151`).
pub(super) fn normalize_session_name(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut in_newline_run = false;
    for ch in raw.chars() {
        if ch == '\r' || ch == '\n' {
            if !in_newline_run {
                out.push(' ');
                in_newline_run = true;
            }
        } else {
            out.push(ch);
            in_newline_run = false;
        }
    }
    out.trim().to_string()
}

/// Flatten the text blocks of a content list into one string.
pub(super) fn content_text(content: &[Content]) -> String {
    content
        .iter()
        .filter_map(|block| match block {
            Content::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Render a stored entry as a message-view item, skipping entries that
/// have no place in the transcript (headers, extension bookkeeping,
/// compaction checkpoints).
pub(super) fn entry_to_item(entry: &SessionEntry) -> Option<MessageItem> {
    match entry {
        SessionEntry::UserMessage(message) => Some(MessageItem::user(
            content_text(&message.content),
        )),
        SessionEntry::AssistantMessage(message) => Some(MessageItem::assistant(
            content_text(&message.content),
        )),
        SessionEntry::ToolResult(result) => Some(MessageItem::tool(content_text(
            std::slice::from_ref(&*result.content),
        ))),
        _ => None,
    }
}

/// Rebuild the agent's model context from stored entries. A compaction
/// checkpoint resets the log to its summary plus the retained tail,
/// exactly as replaying the session would.
pub(super) fn entries_to_messages(entries: &[SessionEntry]) -> Vec<Message> {
    let mut messages = Vec::new();
    for entry in entries {
        match entry {
            SessionEntry::UserMessage(message) => messages.push(message.clone()),
            SessionEntry::AssistantMessage(message) => messages.push(Message {
                role: pi_protocol::Role::Assistant,
                content: message.content.clone(),
                model: Some(message.model.clone()).filter(|model| !model.is_empty()),
            }),
            SessionEntry::ToolCall(call) => messages.push(Message {
                role: pi_protocol::Role::Assistant,
                content: vec![Content::ToolCall(call.clone())],
                model: None,
            }),
            SessionEntry::ToolResult(result) => messages.push(Message {
                role: pi_protocol::Role::Tool,
                content: vec![Content::ToolResult(result.clone())],
                model: None,
            }),
            SessionEntry::Compaction {
                summary,
                retained_tail,
                ..
            } => {
                messages.clear();
                messages.push(Message {
                    role: pi_protocol::Role::System,
                    content: vec![Content::text(summary.clone())],
                    model: None,
                });
                messages.extend(retained_tail.iter().cloned());
            }
            SessionEntry::Header { .. } | SessionEntry::Extension { .. } => {}
        }
    }
    messages
}