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

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::Context;
use pi_protocol::{Content, Role, SessionEntry, StopReason};
use pi_session::{SessionReader, SessionTreeNode, SessionWriter};
use pi_tui::tree::{flatten_tree_folded, tree_selector_items, TreeItem, TreeLabelEditor};
use pi_tui::Selector;

use crate::commands::session::new_session_id;

/// The `custom_type` a `/tree` label entry is written under, and the marker
/// the reader looks for when it resolves labels.
///
/// Upstream stores a `label` entry type natively; the Rust session model has
/// no such variant (`pi-protocol`), so a label travels as
/// [`SessionEntry::Extension`] and `pi-session`'s writer maps it to a
/// `custom` entry with this `custom_type`.
pub const LABEL_ENTRY_KIND: &str = "label";

/// The labels a session currently carries: `entry_id -> (label, timestamp)`,
/// the label's *own* entry timestamp (upstream `labelTimestampsById`).
type TreeLabels = HashMap<String, (String, i64)>;

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

/// Which entries the `/tree` overlay shows — upstream `tree-selector.ts`
/// `FilterMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TreeFilter {
    /// Hide settings / bookkeeping entries (upstream's default view).
    #[default]
    Default,
    /// Default minus tool results.
    NoTools,
    /// Only user messages.
    UserOnly,
    /// Only entries that carry a label (upstream `labeled-only`:
    /// `flatNode.node.label !== undefined`, `tree-selector.ts:374-376`).
    LabeledOnly,
    /// Everything, including settings / bookkeeping entries.
    All,
}

impl TreeFilter {
    /// Every mode, in upstream's cycle order
    /// (`tree-selector.ts:1064` `cycleForward`).
    pub const CYCLE: [TreeFilter; 5] = [
        TreeFilter::Default,
        TreeFilter::NoTools,
        TreeFilter::UserOnly,
        TreeFilter::LabeledOnly,
        TreeFilter::All,
    ];

    /// The next mode in the cycle (`app.tree.filter.cycleForward`).
    pub fn next(self) -> Self {
        let index = Self::CYCLE
            .iter()
            .position(|mode| *mode == self)
            .unwrap_or(0);
        Self::CYCLE[(index + 1) % Self::CYCLE.len()]
    }

    /// The previous mode in the cycle (`app.tree.filter.cycleBackward`).
    pub fn prev(self) -> Self {
        let index = Self::CYCLE
            .iter()
            .position(|mode| *mode == self)
            .unwrap_or(0);
        Self::CYCLE[(index + Self::CYCLE.len() - 1) % Self::CYCLE.len()]
    }

    /// Upstream's mode name, as shown in the picker footer.
    pub fn name(self) -> &'static str {
        match self {
            TreeFilter::Default => "default",
            TreeFilter::NoTools => "no-tools",
            TreeFilter::UserOnly => "user-only",
            TreeFilter::LabeledOnly => "labeled-only",
            TreeFilter::All => "all",
        }
    }

    /// Whether `entry` survives this mode, given the session's current
    /// leaf and whether the node carries a user label.
    ///
    /// `is_current_leaf` keeps the active position visible: upstream
    /// never hides the leaf even when its assistant message carries no
    /// text (`tree-selector.ts:341-353`). `has_label` is read from the
    /// session's label entries for `labeled-only`
    /// (`tree-selector.ts:374-376`); label entries themselves are
    /// settings/bookkeeping rows and stay hidden in `default` /
    /// `no-tools`.
    pub fn passes(self, entry: &SessionEntry, is_current_leaf: bool, has_label: bool) -> bool {
        // Upstream hides assistant turns that produced only tool calls
        // unless they errored/aborted or are the active leaf.
        if let SessionEntry::AssistantMessage(message) = entry {
            if !is_current_leaf {
                let has_text = !content_text(&message.content).trim().is_empty();
                let is_error_or_aborted = message.error_message.is_some()
                    || !matches!(message.stop_reason, StopReason::Stop | StopReason::ToolUse);
                if !has_text && !is_error_or_aborted {
                    return false;
                }
            }
        }
        let is_settings_entry = matches!(
            entry,
            SessionEntry::Extension { .. }
                | SessionEntry::Compaction { .. }
                | SessionEntry::Header { .. }
        );
        match self {
            TreeFilter::UserOnly => {
                matches!(entry, SessionEntry::UserMessage(message) if message.role == Role::User)
            }
            TreeFilter::NoTools => {
                !is_settings_entry && !matches!(entry, SessionEntry::ToolResult(_))
            }
            TreeFilter::LabeledOnly => has_label,
            TreeFilter::All => true,
            TreeFilter::Default => !is_settings_entry,
        }
    }
}

/// How the `/tree` overlay is currently viewed: the filter, the folded
/// branch ids and whether labels are drawn as timestamps.
#[derive(Debug, Clone, Default)]
pub struct TreeView {
    /// Active filter mode.
    pub filter: TreeFilter,
    /// Entry ids whose subtree is collapsed.
    pub folded: HashSet<String>,
    /// Draw each labeled row's label timestamp in the description column
    /// (`app.tree.toggleLabelTimestamp`).
    ///
    /// Upstream shows the *label's own* timestamp and only when the row
    /// carries a label (`tree-selector.ts:741-744`). Deviation: an
    /// unlabeled row keeps showing its entry timestamp, so the toggle
    /// stays observable on a tree that has no labels yet.
    pub show_label_timestamps: bool,
    /// The open label editor, if any (`app.tree.editLabel`).
    ///
    /// While this is `Some`, the driver routes every key to it instead of
    /// the picker chords, and the overlay renders its three rows in place
    /// of the tree list — upstream `TreeSelectorComponent.labelInput`
    /// (`tree-selector.ts:1332`).
    pub label_editor: Option<TreeLabelEditor>,
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
    tree_selector_with(reader, session_id, active_leaf, &TreeView::default())
}

/// [`tree_selector`] honouring a [`TreeView`]: filter, folds and the
/// label/timestamp column.
///
/// The fold set is applied while flattening (a folded node keeps its own
/// row and loses its subtree), matching upstream's `applyFilter`, where
/// `foldedNodes` is consulted after the filter mode has pruned the tree.
/// Filtering therefore happens *before* folding, so a node's foldability
/// is computed from its visible children (`isFoldable` in
/// `tree-selector.ts:1106`).
pub fn tree_selector_with(
    reader: &SessionReader,
    session_id: &str,
    active_leaf: Option<&str>,
    view: &TreeView,
) -> anyhow::Result<Selector> {
    let labels = read_labels(reader, session_id)?;
    let rows = tree_rows_with(reader, session_id, active_leaf, view, &labels)?;
    let selector_items = tree_selector_items(&rows)
        .into_iter()
        .map(|mut item| {
            item.value = format!("tree:{}", item.value);
            item
        })
        .collect::<Vec<_>>();
    let selector = Selector::new("Session tree", selector_items)
        .searchable(true)
        .with_max_visible(15)
        .with_footer(tree_footer(view));
    // Upstream swaps the tree container for the label input
    // (`tree-selector.ts:1364-1382`); the shared list draws the input's rows
    // in place of the item list, keeping the title and footer.
    Ok(match &view.label_editor {
        Some(editor) => selector.with_body(editor.render_lines()),
        None => selector,
    })
}

/// The visible rows of the current `/tree` view, in display order.
///
/// [`tree_selector_with`] renders exactly these rows; the driver reads
/// them back to answer the fold chords, which need the rows' `indent` /
/// `foldable` metadata rather than the flattened selector items.
/// Upstream keeps the equivalent maps (`visibleParentMap` /
/// `visibleChildrenMap`) inside `tree-selector.ts`.
pub fn tree_rows(
    reader: &SessionReader,
    session_id: &str,
    active_leaf: Option<&str>,
    view: &TreeView,
) -> anyhow::Result<Vec<pi_tui::tree::TreeRow>> {
    let labels = read_labels(reader, session_id)?;
    tree_rows_with(reader, session_id, active_leaf, view, &labels)
}

/// [`tree_rows`] with the session's labels already resolved.
fn tree_rows_with(
    reader: &SessionReader,
    session_id: &str,
    active_leaf: Option<&str>,
    view: &TreeView,
    labels: &TreeLabels,
) -> anyhow::Result<Vec<pi_tui::tree::TreeRow>> {
    let roots = reader
        .session_tree(session_id)
        .with_context(|| format!("building the entry tree for session {session_id:?}"))?;
    let items = roots
        .iter()
        .filter_map(|node| to_tree_item(node, active_leaf, view, labels))
        .collect::<Vec<_>>();
    Ok(flatten_tree_folded(&items, active_leaf, &view.folded))
}

/// Resolve the session's active labels — upstream `_buildIndex`'s
/// `labelsById` / `labelTimestampsById` pass (`session-manager.ts:974-990`).
///
/// Entries are read in order, so a later label entry overrides an earlier
/// one and a `null` / empty label removes the target's label. The payload
/// is accepted in both shapes the port can meet: its own writes
/// (`{targetId, label}`, mapped back from a `custom` entry) and an
/// upstream-written `label` entry, whose `pi-session` reader passes the
/// whole payload through.
fn read_labels(reader: &SessionReader, session_id: &str) -> anyhow::Result<TreeLabels> {
    let entries = reader
        .iter_entries(session_id)
        .with_context(|| format!("reading entries for session {session_id:?}"))?;
    let mut labels = TreeLabels::new();
    for entry in &entries {
        let SessionEntry::Extension { kind, payload, .. } = &entry.entry else {
            continue;
        };
        if kind != LABEL_ENTRY_KIND {
            continue;
        }
        let Some(target) = payload
            .get("targetId")
            .or_else(|| payload.get("target_id"))
            .and_then(|value| value.as_str())
        else {
            continue;
        };
        match payload.get("label").and_then(|value| value.as_str()) {
            Some(label) if !label.is_empty() => {
                labels.insert(target.to_string(), (label.to_string(), entry.timestamp));
            }
            _ => {
                labels.remove(target);
            }
        }
    }
    Ok(labels)
}

/// Append a `/tree` label entry for `target_id`; `None` removes the label.
///
/// Mirrors upstream `SessionManager.appendLabelChange`
/// (`session-manager.ts:1246-1267`): the entry is appended under the
/// session's current leaf (the writer's `last_entry_id`, which `/tree`
/// navigation persists in the session metadata), and [`read_labels`]
/// resolves the latest entry per target. The Rust session model has no
/// `label` variant, so it travels as a `custom` entry with
/// `custom_type = "label"` holding `{targetId, label}`.
pub fn append_label_change(
    database: &Path,
    session_id: &str,
    target_id: &str,
    label: Option<&str>,
) -> anyhow::Result<()> {
    let writer = SessionWriter::open(database)
        .with_context(|| format!("opening session file {}", database.display()))?;
    writer
        .resume(session_id)
        .with_context(|| format!("resuming session {session_id:?}"))?;
    writer
        .append(SessionEntry::Extension {
            extension: LABEL_ENTRY_KIND.to_string(),
            kind: LABEL_ENTRY_KIND.to_string(),
            payload: serde_json::json!({ "targetId": target_id, "label": label }),
        })
        .with_context(|| format!("appending the label for {target_id:?}"))?;
    writer.checkpoint().context("flushing the label entry")?;
    Ok(())
}

/// The current label string stored for `target_id`, if any.
///
/// Wraps [`read_labels`] so the label editor can pre-fill its buffer
/// with the node's most recent non-empty label (upstream `Input.setValue`
/// in `tree-selector.ts:1291-1293`).
pub fn current_label_for_entry(
    database: &Path,
    session_id: &str,
    target_id: &str,
) -> anyhow::Result<Option<String>> {
    let reader = SessionReader::open(database)
        .with_context(|| format!("opening session file {}", database.display()))?;
    let labels = read_labels(&reader, session_id)?;
    Ok(labels
        .get(target_id)
        .map(|(label, _timestamp)| label.clone()))
}

/// The `/tree` help footer — upstream `TREE_HELP_ITEMS`
/// (`tree-selector.ts:1219-1236`), with the current filter folded in so
/// the active view is visible without opening `/hotkeys`.
fn tree_footer(view: &TreeView) -> Vec<String> {
    vec![
        "  ↑/↓ move · ←/→ page · ctrl+← branch · shift+l label · shift+t label time".to_string(),
        "  ctrl+d default · ctrl+t no-tools · ctrl+u user-only · ctrl+l labeled-only · ctrl+a all"
            .to_string(),
        format!(
            "  ctrl+o cycle forward · shift+ctrl+o back · filter: {}{}",
            view.filter.name(),
            if view.folded.is_empty() {
                String::new()
            } else {
                format!(" · {} folded", view.folded.len())
            }
        ),
    ]
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
            } else if kind == LABEL_ENTRY_KIND {
                // Upstream `tree-selector.ts:839-840`.
                let label = payload
                    .get("label")
                    .and_then(|value| value.as_str())
                    .unwrap_or("(cleared)");
                format!("[label: {label}]")
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
/// flattener consumes, dropping the subtree rooted at a node the filter
/// rejects.
///
/// `None` means the node is filtered out at this level; its children are
/// dropped with it, exactly like upstream's `flatNodes` — a filtered-out
/// parent takes its subtree out of the visible list.
fn to_tree_item(
    node: &SessionTreeNode,
    active_leaf: Option<&str>,
    view: &TreeView,
    labels: &TreeLabels,
) -> Option<TreeItem> {
    let entry_id = node.entry.entry_id.clone();
    let is_current_leaf = entry_id.as_deref() == active_leaf;
    let active_label = entry_id.as_deref().and_then(|id| labels.get(id));
    if !view
        .filter
        .passes(&node.entry.entry, is_current_leaf, active_label.is_some())
    {
        return None;
    }
    let children = node
        .children
        .iter()
        .filter_map(|child| to_tree_item(child, active_leaf, view, labels))
        .collect::<Vec<_>>();
    let value = entry_id.unwrap_or_else(|| format!("seq:{}", node.entry.seq));
    let item = TreeItem::new(value, entry_display_text(&node.entry.entry))
        .with_user_label(active_label.map(|(label, _)| label.clone()));
    let item = match view.show_label_timestamps {
        // Upstream uses the label's own timestamp, and only when the row is
        // labeled (`tree-selector.ts:741-744`). An unlabeled row keeps the
        // entry timestamp here so the toggle stays observable on a tree
        // with no labels yet.
        true => {
            let timestamp = active_label
                .map(|(_, timestamp)| *timestamp)
                .unwrap_or(node.entry.timestamp);
            item.with_description(format_timestamp(timestamp))
        }
        false => item,
    };
    Some(item.with_children(children))
}

/// `HH:MM:SS` for an entry timestamp (milliseconds since the epoch).
fn format_timestamp(ms: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp(ms, 0)
        .map(|t| t.format("%H:%M:%S").to_string())
        .unwrap_or_else(|| format!("@{ms}"))
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
