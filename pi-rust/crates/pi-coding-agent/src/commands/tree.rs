//! `/tree`, `/fork` and `/clone` — session tree navigation and branching.
//!
//! The read side lives in `pi-session` ([`SessionReader::session_tree`],
//! [`SessionReader::entry_ancestry`]) and the verbatim copy lives in
//! [`SessionWriter::copy_entries_from`]; this module is the coding-agent
//! layer that turns them into what the TUI needs:
//!
//! * [`tree_selector`] — the `/tree` overlay, built from the session's
//!   `entry_id`/`parent_entry_id` links and flattened by
//!   [`pi_tui::flatten_tree`] (pre-order, active branch first, gutter
//!   aligned). Values are `tree:<entry_id>`.
//! * [`fork_selector`] — the `/fork` user-message picker. Values are
//!   `fork:<entry_id>`.
//! * [`clone_session`] / [`fork_session`] — create the new upstream-v4
//!   session file. `/clone` copies the whole session entry-for-entry;
//!   `/fork` copies the root-to-selected-message path (inclusive, so the
//!   forked transcript ends at the message the user picked). Neither
//!   touches the source file — it is opened read-only.
//!
//! [`SessionReader::session_tree`]: pi_session::SessionReader::session_tree
//! [`SessionReader::entry_ancestry`]: pi_session::SessionReader::entry_ancestry
//! [`SessionWriter::copy_entries_from`]: pi_session::SessionWriter::copy_entries_from

use std::path::{Path, PathBuf};

use anyhow::Context;
use pi_protocol::{Content, Role, SessionEntry, StopReason};
use pi_session::{SessionReader, SessionTreeNode, SessionWriter};
use pi_tui::tree::{flatten_tree, tree_selector_items, TreeItem};
use pi_tui::Selector;

use crate::commands::session::new_session_id;

/// A freshly created session file produced by [`clone_session`] /
/// [`fork_session`].
#[derive(Debug, Clone)]
pub struct CreatedSession {
    /// Path of the new upstream-v4 SQLite file.
    pub path: PathBuf,
    /// Identifier stamped on its header.
    pub session_id: String,
    /// Number of entries copied into it (excluding the header, which
    /// lives in the `sessions` row).
    pub entries: usize,
}

/// The `/tree` overlay for a stored session.
///
/// `active_leaf` marks the branch the cursor currently sits on so it is
/// ordered first and gets the `•` marker; pass the session's tip (see
/// [`session_tip`]) when the cursor is at the end of the transcript.
pub fn tree_selector(
    reader: &SessionReader,
    session_id: &str,
    active_leaf: Option<&str>,
) -> anyhow::Result<Selector> {
    let roots = reader
        .session_tree(session_id)
        .with_context(|| format!("building the entry tree for session {session_id:?}"))?;
    let items = roots.iter().map(to_tree_item).collect::<Vec<_>>();
    let rows = flatten_tree(&items, active_leaf);
    let selector_items = tree_selector_items(&rows)
        .into_iter()
        .map(|mut item| {
            item.value = format!("tree:{}", item.value);
            item
        })
        .collect::<Vec<_>>();
    Ok(Selector::new("Session tree", selector_items)
        .searchable(true)
        .with_max_visible(15))
}

/// The `/fork` user-message picker. Values are `fork:<entry_id>`.
///
/// Mirrors `getUserMessagesForForking` (`agent-session.ts:3337`): only
/// user messages with non-empty text are offered, in entry order.
pub fn fork_selector(reader: &SessionReader, session_id: &str) -> anyhow::Result<Selector> {
    let entries = reader
        .iter_entries(session_id)
        .with_context(|| format!("reading entries for session {session_id:?}"))?;
    let items = entries
        .iter()
        .filter_map(|entry| {
            let SessionEntry::UserMessage(message) = &entry.entry else {
                return None;
            };
            if message.role != Role::User {
                return None;
            }
            let text = content_text(&message.content);
            let text = normalize(&text);
            if text.is_empty() {
                return None;
            }
            let entry_id = entry.entry_id.clone()?;
            Some(
                pi_tui::SelectorItem::new(format!("fork:{entry_id}"), text)
                    .with_description(format!("seq {}", entry.seq)),
            )
        })
        .collect::<Vec<_>>();
    Ok(Selector::new("Fork from a user message", items)
        .searchable(true)
        .with_max_visible(10))
}

/// The id of the session's current tip (highest `seq`, then highest
/// `id`), i.e. the default active leaf for [`tree_selector`].
pub fn session_tip(reader: &SessionReader, session_id: &str) -> anyhow::Result<Option<String>> {
    let entries = reader.iter_entries(session_id)?;
    Ok(entries.into_iter().rev().find_map(|entry| entry.entry_id))
}

/// Human-readable, single-line text for one entry, mirroring upstream
/// `tree-selector.ts` `getEntryDisplayText` (without theme colours).
pub fn entry_display_text(entry: &SessionEntry) -> String {
    match entry {
        SessionEntry::Header { id, .. } => format!("[header: {id}]"),
        SessionEntry::UserMessage(message) => {
            format!("user: {}", normalize(&content_text(&message.content)))
        }
        SessionEntry::AssistantMessage(message) => {
            let text = normalize(&content_text(&message.content));
            if !text.is_empty() {
                format!("assistant: {text}")
            } else if matches!(message.stop_reason, StopReason::Aborted) {
                "assistant: (aborted)".to_string()
            } else if let Some(error) = &message.error_message {
                let error: String = normalize(error).chars().take(80).collect();
                format!("assistant: {error}")
            } else {
                "assistant: (no content)".to_string()
            }
        }
        SessionEntry::ToolResult(result) => {
            if result.is_error {
                "[tool error]".to_string()
            } else {
                "[tool result]".to_string()
            }
        }
        SessionEntry::ToolCall(call) => format!("[tool: {}]", call.name),
        SessionEntry::Extension {
            extension,
            kind,
            payload,
        } => {
            if extension == "branch_summary" {
                let summary = payload
                    .get("summary")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default();
                format!("[branch summary]: {}", normalize(summary))
            } else {
                format!("[custom: {kind}]")
            }
        }
        SessionEntry::Compaction { tokens_before, .. } => {
            format!("[compaction: {}k tokens]", tokens_before / 1000)
        }
    }
}

/// Copy the whole session into a new file. The source is only read.
pub fn clone_session(
    directory: &Path,
    source: &SessionReader,
    source_session_id: &str,
) -> anyhow::Result<CreatedSession> {
    create_copy(directory, source, source_session_id, None)
}

/// Copy the root-to-`entry_id` path (inclusive) into a new file. The
/// source is only read.
pub fn fork_session(
    directory: &Path,
    source: &SessionReader,
    source_session_id: &str,
    entry_id: &str,
) -> anyhow::Result<CreatedSession> {
    let entry_ids = source
        .entry_ancestry(source_session_id, entry_id)
        .with_context(|| format!("resolving the fork path for {entry_id:?}"))?
        .into_iter()
        .filter_map(|entry| entry.entry_id)
        .collect::<Vec<_>>();
    if entry_ids.is_empty() {
        anyhow::bail!("entry {entry_id:?} has no entries to fork");
    }
    create_copy(directory, source, source_session_id, Some(entry_ids))
}

fn create_copy(
    directory: &Path,
    source: &SessionReader,
    source_session_id: &str,
    entry_ids: Option<Vec<String>>,
) -> anyhow::Result<CreatedSession> {
    std::fs::create_dir_all(directory)
        .with_context(|| format!("creating session directory {}", directory.display()))?;
    let session_id = new_session_id();
    let path = directory.join(format!("{session_id}.sqlite"));
    let writer = SessionWriter::open(&path)
        .with_context(|| format!("creating session file {}", path.display()))?;
    let result = writer
        .write_header(SessionEntry::Header {
            id: session_id.clone(),
            created_at: chrono::Utc::now(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        })
        .and_then(|()| writer.copy_entries_from(source, source_session_id, entry_ids.as_deref()))
        .and_then(|entries| {
            writer.checkpoint()?;
            Ok(entries)
        });
    match result {
        Ok(entries) => Ok(CreatedSession {
            path,
            session_id,
            entries,
        }),
        Err(err) => {
            // Never leave a half-written session behind: the header row
            // is already committed, so drop the whole file.
            let _ = std::fs::remove_file(&path);
            Err(err)
                .with_context(|| format!("copying session {source_session_id:?} into {session_id}"))
        }
    }
}

/// Turn one tree node into the generic [`TreeItem`] the `pi-tui`
/// flattener consumes.
fn to_tree_item(node: &SessionTreeNode) -> TreeItem {
    let value = node
        .entry
        .entry_id
        .clone()
        .unwrap_or_else(|| format!("seq:{}", node.entry.seq));
    TreeItem::new(value, entry_display_text(&node.entry.entry))
        .with_children(node.children.iter().map(to_tree_item).collect())
}

/// Concatenate the text blocks of a message, mirroring upstream's
/// `extractContent`.
pub fn content_text(blocks: &[Content]) -> String {
    let mut parts = Vec::new();
    for block in blocks {
        match block {
            Content::Text(text) => parts.push(text.text.clone()),
            Content::ToolResult(result) => {
                if let Content::Text(text) = &*result.content {
                    parts.push(text.text.clone());
                }
            }
            Content::Image(_) | Content::ToolCall(_) => {}
        }
    }
    parts.join("\n")
}

/// Collapse newline / tab runs into single spaces, like upstream's
/// `normalize`.
fn normalize(value: &str) -> String {
    value
        .chars()
        .map(|ch| match ch {
            '\n' | '\r' | '\t' => ' ',
            other => other,
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_protocol::{Message, TextContent};

    fn user_entry(text: &str) -> SessionEntry {
        SessionEntry::UserMessage(Message {
            role: Role::User,
            content: vec![Content::Text(TextContent {
                text: text.to_string(),
            })],
            model: None,
        })
    }

    #[test]
    fn user_message_display_is_a_single_line() {
        assert_eq!(
            entry_display_text(&user_entry("hello\nworld\t!")),
            "user: hello world !"
        );
    }

    #[test]
    fn content_text_joins_text_blocks() {
        let blocks = vec![
            Content::Text(TextContent { text: "a".into() }),
            Content::Text(TextContent { text: "b".into() }),
        ];
        assert_eq!(content_text(&blocks), "a\nb");
    }
}
