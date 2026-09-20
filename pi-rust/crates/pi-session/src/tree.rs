//! Session tree assembly from the `entries` parent links.
//!
//! The stored rows carry the branch structure the cursor sits on:
//! `entries.parent_id` (read back as [`DecodedEntry::parent_entry_id`])
//! links every entry to the one it was appended after. [`SessionEntry`]
//! itself has no `id`/`parentId` field, so the tree is assembled at the
//! [`DecodedEntry`] layer — the same layer upstream's `SessionManager`
//! uses (`session-manager.ts` `buildTree` / `entries.parent_id`).
//!
//! Two helpers are provided:
//!
//! * [`SessionReader::session_tree`] — the whole tree, roots first, each
//!   node's children in stored order (`seq` ascending, `id` ascending).
//!   This is what the `/tree` overlay renders.
//! * [`SessionReader::entry_ancestry`] — the root-to-entry path for one
//!   entry, inclusive. This is what `/fork` copies into the new session.
//!
//! [`DecodedEntry::parent_entry_id`]: crate::DecodedEntry::parent_entry_id
//! [`SessionEntry`]: pi_protocol::SessionEntry

use std::collections::HashMap;

use crate::error::{Result, SessionError};
use crate::reader::{DecodedEntry, SessionReader};

/// One node of a session's entry tree.
#[derive(Debug, Clone)]
pub struct SessionTreeNode {
    /// The stored entry this node wraps.
    pub entry: DecodedEntry,
    /// Children in stored order (the order they were appended).
    pub children: Vec<SessionTreeNode>,
}

impl SessionTreeNode {
    /// Depth-first, pre-order iteration over this subtree (self first,
    /// then each child subtree in order).
    pub fn preorder(&self) -> Vec<&SessionTreeNode> {
        let mut out = Vec::new();
        let mut stack = vec![self];
        while let Some(node) = stack.pop() {
            out.push(node);
            for child in node.children.iter().rev() {
                stack.push(child);
            }
        }
        out
    }
}

impl SessionReader {
    /// Assemble the session's entry tree.
    ///
    /// Entries whose `parent_entry_id` is `None` — or points at an entry
    /// that is not part of this session (a cross-session parent) — become
    /// roots. Children keep the [`iter_entries`](SessionReader::iter_entries)
    /// order, so a branch list is stable and the chronological branch is
    /// first.
    pub fn session_tree(&self, session_id: &str) -> Result<Vec<SessionTreeNode>> {
        let entries = self.iter_entries(session_id)?;
        Ok(build_tree(entries))
    }

    /// The root-to-`entry_id` path, inclusive, following
    /// `parent_entry_id` links.
    ///
    /// Errors with [`SessionError::Other`] when `entry_id` is not part of
    /// the session. A cyclic parent chain (which a valid writer cannot
    /// produce, but a hand-edited file could) terminates as soon as a
    /// node repeats instead of looping forever.
    pub fn entry_ancestry(&self, session_id: &str, entry_id: &str) -> Result<Vec<DecodedEntry>> {
        let entries = self.iter_entries(session_id)?;
        let by_id: HashMap<&str, &DecodedEntry> = entries
            .iter()
            .filter_map(|entry| entry.entry_id.as_deref().map(|id| (id, entry)))
            .collect();
        let mut chain = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut current = by_id.get(entry_id).copied();
        if current.is_none() {
            return Err(SessionError::Other(format!(
                "entry {entry_id:?} not found in session {session_id:?}"
            )));
        }
        while let Some(entry) = current {
            let Some(id) = entry.entry_id.clone() else {
                break;
            };
            if !seen.insert(id) {
                break;
            }
            chain.push(entry.clone());
            current = entry
                .parent_entry_id
                .as_deref()
                .and_then(|parent| by_id.get(parent).copied());
        }
        chain.reverse();
        Ok(chain)
    }
}

/// Build the tree from `seq`-ordered entries.
///
/// Parent links point at `parent_entry_id`; the parent is always created
/// before its child, so inserting children in stored order reconstructs
/// the append history. An explicit stack keeps the walk iterative — a
/// long single-child transcript must not recurse once per entry.
fn build_tree(entries: Vec<DecodedEntry>) -> Vec<SessionTreeNode> {
    let index_by_id: HashMap<&str, usize> = entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| entry.entry_id.as_deref().map(|id| (id, index)))
        .collect();

    let mut child_indices: HashMap<usize, Vec<usize>> = HashMap::new();
    let mut roots: Vec<usize> = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        match entry
            .parent_entry_id
            .as_deref()
            .and_then(|id| index_by_id.get(id).copied())
        {
            Some(parent) if parent != index => child_indices.entry(parent).or_default().push(index),
            _ => roots.push(index),
        }
    }

    // Post-order build: children are finished before their parent, so the
    // parent can take them out of the arena.
    enum Step {
        Enter(usize),
        Exit(usize),
    }
    let mut built: Vec<Option<SessionTreeNode>> = (0..entries.len()).map(|_| None).collect();
    let mut stack: Vec<Step> = roots
        .iter()
        .rev()
        .map(|&index| Step::Enter(index))
        .collect();
    while let Some(step) = stack.pop() {
        match step {
            Step::Enter(index) => {
                stack.push(Step::Exit(index));
                if let Some(children) = child_indices.get(&index) {
                    for &child in children.iter().rev() {
                        stack.push(Step::Enter(child));
                    }
                }
            }
            Step::Exit(index) => {
                let children = child_indices
                    .remove(&index)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|child| {
                        built[child]
                            .take()
                            .expect("children are built before their parent")
                    })
                    .collect();
                built[index] = Some(SessionTreeNode {
                    entry: entries[index].clone(),
                    children,
                });
            }
        }
    }

    roots
        .into_iter()
        .map(|index| built[index].take().expect("every root is built"))
        .collect()
}
