//! Tree flattening for the `/tree` session overlay.
//!
//! This is the Rust subset of upstream
//! `packages/coding-agent/src/modes/interactive/components/tree-selector.ts`
//! that the port needs: a pre-order flatten with the branch containing the
//! active leaf first, drifting indentation, and the `│ ├─ └─` gutter
//! alignment. It is deliberately data-driven — [`TreeItem`] carries only
//! the value (the session entry id), the display label and the children —
//! so `pi-tui` needs no dependency on `pi-session`; the coding agent
//! assembles [`TreeItem`]s from its own entry tree.
//!
//! Ported: folding and the filter modes (Stage 68, LUM-1255 — see
//! [`flatten_tree_folded`] and [`TreeRow::foldable`]), the user-assigned
//! label (`[label] ` prefix, [`TreeItem::user_label`]) and the label
//! editor ([`TreeLabelEditor`], LUM-1263). Because filtering is not
//! re-flattened, a fuzzy hit keeps its original indent/gutter instead of
//! being re-indented as upstream's `recomputeVisualStructure` would.
//!
//! Not ported (upstream-only, out of scope): horizontal viewport
//! scrolling.
//!
//! [`TreeRow`] is the flattened row: `prefix` is the indent + gutter +
//! connector string, `label` is the node's own text, and
//! [`tree_selector_items`] composes them (plus the `•` active-path
//! marker) into [`SelectorItem`]s the existing [`Selector`](crate::Selector)
//! renders, filters and windows.
//!
//! [`Selector`]: crate::components::selector::Selector

use std::collections::{HashMap, HashSet};

use crate::components::editor::Editor;
use crate::core::input_parse::{InputEvent, Key};
use crate::components::keybindings::{get_keybindings, matches_with_fallback};
use crate::components::selector::SelectorItem;

/// One node of the tree the flattening API accepts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeItem {
    /// Stable identity of the node (the session entry id for `/tree`).
    pub value: String,
    /// The node's own display text, without indent or gutter.
    pub label: String,
    /// Optional secondary column.
    pub description: Option<String>,
    /// User-assigned label (`/tree` rename), drawn as `[label] ` before
    /// the display text. `None` means the node carries no label.
    ///
    /// Upstream calls this `node.label` (resolved from the session's
    /// `label` entries); this port names it `user_label` because
    /// [`TreeItem::label`] is already the entry's display text.
    pub user_label: Option<String>,
    /// Children in stored order.
    pub children: Vec<TreeItem>,
}

impl TreeItem {
    /// A leaf with `value` / `label` and no description, label or
    /// children.
    pub fn new(value: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            label: label.into(),
            description: None,
            user_label: None,
            children: Vec::new(),
        }
    }

    /// Attach the secondary description column.
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Attach the user-assigned label (`/tree` rename).
    pub fn with_user_label(mut self, label: Option<String>) -> Self {
        self.user_label = label;
        self
    }

    /// Replace the children (stored order is preserved).
    pub fn with_children(mut self, children: Vec<TreeItem>) -> Self {
        self.children = children;
        self
    }
}

/// A flattened tree row: what to draw, in pre-order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeRow {
    /// Stable identity, copied from the [`TreeItem`].
    pub value: String,
    /// Logical indent level (before the multiple-roots display shift).
    pub indent: usize,
    /// Indent + gutter + connector glyphs (`"│ ├─ "`, `""`, …).
    pub prefix: String,
    /// The node's own display text.
    pub label: String,
    /// The node's user-assigned label, copied from the [`TreeItem`].
    pub user_label: Option<String>,
    /// Optional secondary column.
    pub description: Option<String>,
    /// True when this node lies on the root-to-active-leaf path.
    pub on_active_path: bool,
    /// True when this node's children are hidden by a fold.
    pub folded: bool,
    /// True when upstream's `isFoldable` says this node can be folded:
    /// it has visible children and is either a root or the start of a
    /// branch segment (its visible parent has more than one visible
    /// child). A node in the middle of a single-child chain is not
    /// foldable — folding it would hide the rest of the chain.
    pub foldable: bool,
}

/// Flatten a forest into pre-order rows.
///
/// The subtree containing `active_value` (typically the session's current
/// leaf) is emitted first at every level, so the branch the cursor sits on
/// is not buried under older forks. Child order is otherwise preserved.
pub fn flatten_tree(roots: &[TreeItem], active_value: Option<&str>) -> Vec<TreeRow> {
    flatten_tree_folded(roots, active_value, &HashSet::new())
}

/// [`flatten_tree`] with folding: the children of every node whose value
/// is in `folded` are skipped.
///
/// Mirrors upstream `tree-selector.ts` `applyFilter`'s fold step — the
/// folded set is consulted while walking, so a folded node keeps its own
/// row (drawn with the `⊞` fold glyph) and loses its subtree. A value in
/// `folded` that is not currently foldable is ignored (upstream clears
/// the fold set whenever the filter changes, and `isFoldable` gates the
/// chord that adds to it).
pub fn flatten_tree_folded(
    roots: &[TreeItem],
    active_value: Option<&str>,
    folded: &HashSet<String>,
) -> Vec<TreeRow> {
    let active = compute_active_subtrees(roots, active_value);
    let is_active = |node: &TreeItem| active.get(&node.value).copied().unwrap_or(false);

    let multiple_roots = roots.len() > 1;
    let mut ordered_roots: Vec<&TreeItem> = roots.iter().collect();
    // Stable: only the active branch moves, everything else keeps its order.
    ordered_roots.sort_by_key(|a| std::cmp::Reverse(is_active(a)));

    let mut stack: Vec<Frame> = Vec::new();
    for (index, root) in ordered_roots.iter().enumerate().rev() {
        let is_last = index == ordered_roots.len() - 1;
        stack.push(Frame {
            node: root,
            indent: if multiple_roots { 1 } else { 0 },
            just_branched: multiple_roots,
            show_connector: multiple_roots,
            is_last,
            gutters: Vec::new(),
            is_virtual_root_child: multiple_roots,
            // A root has no visible parent, so it is foldable whenever it
            // has children (`isFoldable`'s `parentId === null` branch).
            foldable: !root.children.is_empty(),
        });
    }

    let mut rows = Vec::new();
    while let Some(frame) = stack.pop() {
        let is_folded = frame.foldable && folded.contains(&frame.node.value);
        rows.push(TreeRow {
            value: frame.node.value.clone(),
            indent: frame.indent,
            prefix: render_prefix(&frame, multiple_roots, is_folded),
            label: frame.node.label.clone(),
            user_label: frame.node.user_label.clone(),
            description: frame.node.description.clone(),
            on_active_path: is_active(frame.node),
            folded: is_folded,
            foldable: frame.foldable,
        });
        if is_folded {
            continue;
        }

        let multiple_children = frame.node.children.len() > 1;
        let child_indent = if multiple_children {
            frame.indent + 1
        } else if frame.just_branched && frame.indent > 0 {
            // First generation after a branch: +1 for visual grouping.
            frame.indent + 1
        } else {
            // Single-child chain: stay flat.
            frame.indent
        };

        let connector_displayed = frame.show_connector && !frame.is_virtual_root_child;
        let display_indent = display_indent(frame.indent, multiple_roots);
        let mut child_gutters: Vec<Gutter> = frame
            .gutters
            .iter()
            .map(|gutter| Gutter {
                position: gutter.position,
                show: gutter.show,
            })
            .collect();
        if connector_displayed {
            child_gutters.push(Gutter {
                position: display_indent.saturating_sub(1),
                show: !frame.is_last,
            });
        }

        let mut prioritized = Vec::new();
        let mut rest = Vec::new();
        for child in &frame.node.children {
            if is_active(child) {
                prioritized.push(child);
            } else {
                rest.push(child);
            }
        }
        let ordered: Vec<&TreeItem> = prioritized.into_iter().chain(rest).collect();
        for (index, child) in ordered.iter().enumerate().rev() {
            let child_is_last = index == ordered.len() - 1;
            stack.push(Frame {
                node: child,
                indent: child_indent,
                just_branched: multiple_children,
                show_connector: multiple_children,
                is_last: child_is_last,
                gutters: child_gutters
                    .iter()
                    .map(|gutter| Gutter {
                        position: gutter.position,
                        show: gutter.show,
                    })
                    .collect(),
                is_virtual_root_child: false,
                // `isFoldable`: a child of a branch point starts a segment
                // and may be folded; the only child of a chain may not.
                foldable: multiple_children && !child.children.is_empty(),
            });
        }
    }
    rows
}

/// Compose flattened rows into selector items, baking the indent + gutter,
/// the `•` active-path marker and the user label into the label.
///
/// Upstream's row order is `prefix + foldMarker + pathMarker + label +
/// labelTimestamp + content` (`tree-selector.ts:746-749`), with the label
/// rendered as `[<label>] ` (`:740`). The port composes the same string
/// here; the label timestamp travels in the description column instead
/// (see [`TreeRow::description`]).
pub fn tree_selector_items(rows: &[TreeRow]) -> Vec<SelectorItem> {
    rows.iter()
        .map(|row| {
            let marker = if row.on_active_path { "• " } else { "" };
            let user_label = match &row.user_label {
                Some(label) => format!("[{label}] "),
                None => String::new(),
            };
            let label = format!("{}{}{}{}", row.prefix, marker, user_label, row.label);
            let item = SelectorItem::new(row.value.clone(), label);
            match &row.description {
                Some(description) => item.with_description(description.clone()),
                None => item,
            }
        })
        .collect()
}

/// What one key did to an open [`TreeLabelEditor`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use]
pub enum TreeLabelAction {
    /// The buffer may have changed; the editor stays open.
    Edited,
    /// `tui.select.confirm` (`Enter`): commit. `None` means the buffer was
    /// empty after trimming, i.e. "remove the label" — upstream
    /// `LabelInput.onSubmit` passes `value || undefined`
    /// (`tree-selector.ts:1315-1316`).
    Commit(Option<String>),
    /// `tui.select.cancel` (`Esc`): close without writing.
    Cancel,
}

/// The `/tree` label editor — upstream `LabelInput`
/// (`tree-selector.ts:1271-1323`).
///
/// The buffer is a [`Editor`], so character insertion, backspace / delete,
/// word motions, undo and the kill ring all follow the composer's own
/// semantics instead of a second hand-rolled input box. This type only adds
/// the three things a label field needs: pre-filling the node's current
/// label, intercepting `Enter` / `Esc` before the editor sees them, and the
/// three-row upstream render.
#[derive(Debug, Clone)]
pub struct TreeLabelEditor {
    /// The entry whose label is being edited (`tree:<entry_id>` without the
    /// prefix).
    entry_id: String,
    /// The single-line text buffer.
    editor: Editor,
}

impl TreeLabelEditor {
    /// Open an editor for `entry_id`, pre-filled with `current_label`
    /// (cursor at the end, mirroring upstream `Input.setValue`).
    pub fn new(entry_id: impl Into<String>, current_label: Option<&str>) -> Self {
        let mut editor = Editor::new();
        if let Some(label) = current_label {
            editor.set_text(label);
        }
        Self {
            entry_id: entry_id.into(),
            editor,
        }
    }

    /// The entry id being edited.
    pub fn entry_id(&self) -> &str {
        &self.entry_id
    }

    /// The current buffer, raw (no cursor and no trimming).
    pub fn text(&self) -> &str {
        self.editor.text()
    }

    /// Process one key. `Enter` / `Esc` are answered before the editor can
    /// see them (`Editor` would submit / insert a newline); every other key
    /// is the editor's own.
    pub fn handle_key(&mut self, key: Key) -> TreeLabelAction {
        let kb = get_keybindings();
        let event = InputEvent::Key(key);
        if matches_with_fallback(&kb, &event, "tui.select.confirm", &["enter"]) {
            return TreeLabelAction::Commit(single_line_label(self.editor.text()));
        }
        if matches_with_fallback(&kb, &event, "tui.select.cancel", &["escape"]) {
            return TreeLabelAction::Cancel;
        }
        self.editor.handle_key(key);
        TreeLabelAction::Edited
    }

    /// The upstream `LabelInput.render` rows (`tree-selector.ts:1297-1309`):
    /// the prompt, the input line with this port's `▍` caret (the same
    /// glyph the composer uses), and the effective `save` / `cancel`
    /// chords.
    pub fn render_lines(&self) -> Vec<String> {
        let text = self.editor.display_text();
        let cursor = self.editor.display_cursor();
        let (before, after) = split_at_char(&text, cursor);
        let save = crate::components::keybindings::key_text_or("tui.select.confirm", "enter");
        let cancel = crate::components::keybindings::key_text_or("tui.select.cancel", "escape");
        vec![
            "  Label (empty to remove):".to_string(),
            format!("  {before}\u{258d}{after}"),
            format!("  {save} save    {cancel} cancel"),
        ]
    }
}

/// Trim a label buffer to the single-line value upstream `Input` can hold.
///
/// Upstream `Input` never contains a newline; this port's [`Editor`] can
/// (`Alt+Enter` inserts one), so hard line breaks are folded to spaces and
/// the result is trimmed. `None` is upstream's `value || undefined`: an
/// empty field removes the label.
fn single_line_label(text: &str) -> Option<String> {
    let single: String = text
        .chars()
        .map(|ch| match ch {
            '\n' | '\r' | '\t' => ' ',
            other => other,
        })
        .collect();
    let value = single.trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

/// Split `text` at character index `idx` (clamped).
fn split_at_char(text: &str, idx: usize) -> (&str, &str) {
    let byte = text
        .char_indices()
        .nth(idx)
        .map(|(byte, _)| byte)
        .unwrap_or(text.len());
    text.split_at(byte)
}

/// One branch level's vertical gutter: `position` is the display indent
/// whose connector it continues, `show` whether it draws `│` or a space.
#[derive(Debug, Clone)]
struct Gutter {
    position: usize,
    show: bool,
}

/// One flattening stack frame.
struct Frame<'a> {
    node: &'a TreeItem,
    indent: usize,
    just_branched: bool,
    show_connector: bool,
    is_last: bool,
    gutters: Vec<Gutter>,
    is_virtual_root_child: bool,
    /// Upstream `isFoldable(entryId)` for this node.
    foldable: bool,
}

/// Indent the row is actually drawn at: multiple roots are drawn as
/// children of a virtual root, so their display indent is one less.
fn display_indent(indent: usize, multiple_roots: bool) -> usize {
    if multiple_roots {
        indent.saturating_sub(1)
    } else {
        indent
    }
}

/// Build the indent + gutter + connector prefix for one row.
fn render_prefix(frame: &Frame<'_>, multiple_roots: bool, folded: bool) -> String {
    let display_indent = display_indent(frame.indent, multiple_roots);
    let connector_displayed = frame.show_connector && !frame.is_virtual_root_child;
    let connector_position = connector_displayed.then(|| display_indent.saturating_sub(1));
    let total_chars = display_indent * 3;
    let mut prefix = String::with_capacity(total_chars);
    for index in 0..total_chars {
        let level = index / 3;
        let position = index % 3;
        if let Some(gutter) = frame.gutters.iter().find(|gutter| gutter.position == level) {
            prefix.push(if position == 0 && gutter.show {
                '│'
            } else {
                ' '
            });
        } else if connector_position == Some(level) {
            match position {
                0 => prefix.push(if frame.is_last { '└' } else { '├' }),
                // Upstream's fold indicator: `⊞` collapsed, `⊟` expanded
                // (`tree-selector.ts` gutter rendering). A row with no
                // children keeps the unbroken `─`.
                1 => prefix.push(if !frame.foldable {
                    '─'
                } else if folded {
                    '⊞'
                } else {
                    '⊟'
                }),
                _ => prefix.push(' '),
            }
        } else {
            prefix.push(' ');
        }
    }
    prefix
}

/// `containsActive(node)`: true when `node` is the active leaf or an
/// ancestor of it. Since values are unique, a `HashMap<value, bool>` is
/// enough; the walk is iterative so a long transcript never recurses.
fn compute_active_subtrees(
    roots: &[TreeItem],
    active_value: Option<&str>,
) -> HashMap<String, bool> {
    enum Step<'a> {
        Enter(&'a TreeItem),
        Exit(&'a TreeItem),
    }
    let mut result = HashMap::new();
    let mut stack: Vec<Step> = roots.iter().rev().map(Step::Enter).collect();
    while let Some(step) = stack.pop() {
        match step {
            Step::Enter(node) => {
                stack.push(Step::Exit(node));
                for child in node.children.iter().rev() {
                    stack.push(Step::Enter(child));
                }
            }
            Step::Exit(node) => {
                let mut has_active = active_value == Some(node.value.as_str());
                for child in &node.children {
                    if result.get(&child.value).copied().unwrap_or(false) {
                        has_active = true;
                    }
                }
                result.insert(node.value.clone(), has_active);
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(value: &str, children: Vec<TreeItem>) -> TreeItem {
        TreeItem::new(value, value.to_uppercase()).with_children(children)
    }

    #[test]
    fn single_child_chains_stay_flat_with_no_prefix() {
        let roots = vec![item("r", vec![item("a", vec![item("b", vec![])])])];
        let rows = flatten_tree(&roots, None);
        assert_eq!(
            rows.iter()
                .map(|row| row.value.as_str())
                .collect::<Vec<_>>(),
            vec!["r", "a", "b"]
        );
        assert!(rows.iter().all(|row| row.indent == 0));
        assert!(rows.iter().all(|row| row.prefix.is_empty()));
        assert!(rows.iter().all(|row| !row.on_active_path));
    }

    #[test]
    fn branches_get_connectors_and_align_their_gutters() {
        let roots = vec![item(
            "r",
            vec![item("x", vec![item("x1", vec![])]), item("y", vec![])],
        )];
        let rows = flatten_tree(&roots, Some("y"));
        assert_eq!(
            rows.iter()
                .map(|row| row.value.as_str())
                .collect::<Vec<_>>(),
            vec!["r", "y", "x", "x1"],
            "the branch containing the active leaf is emitted first"
        );

        let by_value = |value: &str| rows.iter().find(|row| row.value == value).expect("row");
        assert_eq!(by_value("r").prefix, "");
        // The active branch is emitted first, so it carries the "more
        // siblings follow" connector and the inactive branch closes the
        // list (upstream `flattenTree`, unchanged by the reorder).
        // `y` is a leaf, so its connector keeps the unbroken rule; `x` has
        // a child, so the connector's second column carries the fold
        // indicator (Stage 68) — expanded, since nothing is folded.
        assert_eq!(by_value("y").prefix, "├─ ");
        assert!(by_value("y").prefix.contains('─'), "no fold box on a leaf");
        assert_eq!(by_value("x").prefix, "└⊟ ");
        assert!(by_value("x").foldable);
        assert!(!by_value("y").foldable);
        // `x` is the last child, so its connector does not continue: its
        // child is indented but draws no vertical gutter line.
        assert_eq!(by_value("x1").prefix, "      ");
        assert_eq!(by_value("x1").indent, 2);

        assert!(by_value("r").on_active_path);
        assert!(by_value("y").on_active_path);
        assert!(!by_value("x").on_active_path);
        assert!(!by_value("x1").on_active_path);
    }

    #[test]
    fn selector_items_bake_in_the_gutter_and_the_active_marker() {
        let roots = vec![item("r", vec![item("x", vec![]), item("y", vec![])])];
        let rows = flatten_tree(&roots, Some("y"));
        let items = tree_selector_items(&rows);
        let labels = items
            .iter()
            .map(|item| (item.value.as_str(), item.label.as_str()))
            .collect::<Vec<_>>();
        assert_eq!(
            labels,
            vec![("r", "• R"), ("y", "├─ • Y"), ("x", "└─ X")],
            "the active path (root + branch) gains the `•` marker"
        );
    }

    #[test]
    fn multiple_roots_are_drawn_as_virtual_root_children() {
        let roots = vec![item("r1", vec![]), item("r2", vec![])];
        let rows = flatten_tree(&roots, None);
        assert_eq!(
            rows.iter()
                .map(|row| row.value.as_str())
                .collect::<Vec<_>>(),
            vec!["r1", "r2"]
        );
        // The virtual root suppresses the connector, so roots sit at
        // column 0.
        assert!(rows.iter().all(|row| row.indent == 1));
        assert!(rows.iter().all(|row| row.prefix.is_empty()));
    }

    // -----------------------------------------------------------------------
    // Folding (Stage 68 / LUM-1255)
    // -----------------------------------------------------------------------

    /// The shape every fold test uses — a branch point with two children,
    /// each of them a single-child chain:
    ///
    /// ```text
    /// r ─ x ─ x1
    ///   └ y ─ y1
    /// ```
    fn branchy() -> Vec<TreeItem> {
        vec![item(
            "r",
            vec![
                item("x", vec![item("x1", vec![])]),
                item("y", vec![item("y1", vec![])]),
            ],
        )]
    }

    #[test]
    fn foldable_marks_the_segment_starts_only() {
        let rows = flatten_tree(&branchy(), None);
        let foldable = |value: &str| {
            rows.iter()
                .find(|row| row.value == value)
                .expect("row")
                .foldable
        };
        // The root has visible children and no visible parent
        // (`isFoldable`'s `parentId === null` branch).
        assert!(foldable("r"));
        // `x` and `y` are the children of a branch point, so each starts a
        // segment.
        assert!(foldable("x"), "{rows:#?}");
        assert!(foldable("y"));
        // `x1` / `y1` are the only child of their chain: folding them
        // would hide the rest of the chain.
        assert!(!foldable("x1"));
        assert!(!foldable("y1"));
    }

    #[test]
    fn folding_a_row_hides_its_subtree_but_keeps_the_row() {
        let folded = HashSet::from(["x".to_string()]);
        let rows = flatten_tree_folded(&branchy(), None, &folded);
        assert_eq!(
            rows.iter()
                .map(|row| row.value.as_str())
                .collect::<Vec<_>>(),
            vec!["r", "x", "y", "y1"],
            "`x1` is gone, `x` stays"
        );
        let x = rows.iter().find(|row| row.value == "x").expect("x");
        assert!(x.folded, "the row reports its own fold");
        assert!(x.prefix.contains('⊞'), "collapsed glyph: {:?}", x.prefix);
        let y = rows.iter().find(|row| row.value == "y").expect("y");
        assert!(!y.folded);
        assert!(y.prefix.contains('⊟'), "expanded glyph: {:?}", y.prefix);
        // A row in the middle of a single-child chain keeps the plain
        // indent — it has no connector of its own, so no fold box either.
        let y1 = rows.iter().find(|row| row.value == "y1").expect("y1");
        assert_eq!(y1.prefix, "      ", "{:?}", y1.prefix);
    }

    #[test]
    fn folding_the_root_hides_everything_below_it() {
        let folded = HashSet::from(["r".to_string()]);
        let rows = flatten_tree_folded(&branchy(), None, &folded);
        assert_eq!(rows.len(), 1, "{rows:#?}");
        assert_eq!(rows[0].value, "r");
        assert!(rows[0].folded);
    }

    #[test]
    fn a_fold_on_a_non_foldable_row_is_ignored() {
        // Upstream clears the fold set whenever the filter changes and
        // gates the chord on `isFoldable`; a stale value must not hide a
        // chain.
        let folded = HashSet::from(["x1".to_string()]);
        let rows = flatten_tree_folded(&branchy(), None, &folded);
        assert_eq!(rows.len(), 5, "{rows:#?}");
        assert_eq!(rows.iter().filter(|row| row.folded).count(), 0);
    }

    #[test]
    fn flatten_tree_is_folding_with_an_empty_set() {
        let roots = branchy();
        assert_eq!(
            flatten_tree(&roots, Some("y1"))
                .iter()
                .map(|row| (row.value.clone(), row.folded, row.foldable))
                .collect::<Vec<_>>(),
            flatten_tree_folded(&roots, Some("y1"), &HashSet::new())
                .iter()
                .map(|row| (row.value.clone(), row.folded, row.foldable))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn active_roots_move_ahead_of_older_roots() {
        let roots = vec![item("r1", vec![]), item("r2", vec![])];
        let rows = flatten_tree(&roots, Some("r2"));
        assert_eq!(
            rows.iter()
                .map(|row| row.value.as_str())
                .collect::<Vec<_>>(),
            vec!["r2", "r1"]
        );
        assert!(rows[0].on_active_path);
    }

    // -----------------------------------------------------------------------
    // User labels + the label editor (LUM-1263)
    // -----------------------------------------------------------------------

    #[test]
    fn a_user_label_is_drawn_before_the_display_text() {
        let roots = vec![
            TreeItem::new("a", "user: hello").with_user_label(Some("checkpoint".into())),
            TreeItem::new("b", "user: bye"),
        ];
        let rows = flatten_tree(&roots, None);
        assert_eq!(rows[0].user_label.as_deref(), Some("checkpoint"));
        assert_eq!(rows[1].user_label, None);
        let items = tree_selector_items(&rows);
        let labels = items
            .iter()
            .map(|item| item.label.as_str())
            .collect::<Vec<_>>();
        assert_eq!(labels, vec!["[checkpoint] user: hello", "user: bye"]);
    }

    fn label_editor(current: Option<&str>) -> TreeLabelEditor {
        TreeLabelEditor::new("e1", current)
    }

    fn key(code: crate::core::input_parse::KeyCode) -> Key {
        Key::new(code, crate::core::input_parse::KeyModifiers::NONE)
    }

    #[test]
    fn the_label_editor_prefills_the_current_label_and_commits_it() {
        let mut editor = label_editor(Some("old"));
        assert_eq!(editor.entry_id(), "e1");
        assert_eq!(editor.text(), "old");
        // The cursor starts at the end, so typing appends.
        assert_eq!(
            editor.handle_key(key(crate::core::input_parse::KeyCode::Char('!'))),
            TreeLabelAction::Edited
        );
        assert_eq!(editor.text(), "old!");
        assert_eq!(
            editor.handle_key(key(crate::core::input_parse::KeyCode::Enter)),
            TreeLabelAction::Commit(Some("old!".into()))
        );
    }

    #[test]
    fn an_empty_label_commits_as_remove() {
        let mut editor = label_editor(Some("old"));
        for _ in 0.."old".len() {
            assert_eq!(
                editor.handle_key(key(crate::core::input_parse::KeyCode::Backspace)),
                TreeLabelAction::Edited
            );
        }
        assert_eq!(editor.text(), "");
        assert_eq!(
            editor.handle_key(key(crate::core::input_parse::KeyCode::Enter)),
            TreeLabelAction::Commit(None),
            "an empty field removes the label, like upstream `value || undefined`"
        );

        // Whitespace-only is empty too.
        let mut blank = label_editor(None);
        for ch in "  \t ".chars() {
            if ch == '\t' {
                let _ = blank.handle_key(Key::new(
                    crate::core::input_parse::KeyCode::Tab,
                    crate::core::input_parse::KeyModifiers::NONE,
                ));
            } else {
                let _ = blank.handle_key(key(crate::core::input_parse::KeyCode::Char(ch)));
            }
        }
        assert_eq!(
            blank.handle_key(key(crate::core::input_parse::KeyCode::Enter)),
            TreeLabelAction::Commit(None)
        );
    }

    #[test]
    fn escape_cancels_without_touching_the_buffer() {
        let mut editor = label_editor(Some("keep"));
        let _ = editor.handle_key(key(crate::core::input_parse::KeyCode::Char('!')));
        assert_eq!(
            editor.handle_key(key(crate::core::input_parse::KeyCode::Esc)),
            TreeLabelAction::Cancel
        );
        assert_eq!(editor.text(), "keep!");
    }

    #[test]
    fn the_label_editor_is_the_composer_editor() {
        // The buffer is an `Editor`, so its own editing semantics apply:
        // Ctrl+W deletes the previous word, and characters land at the
        // cursor after a Home press.
        let mut editor = label_editor(Some("two words"));
        let _ = editor.handle_key(Key::new(
            crate::core::input_parse::KeyCode::Char('w'),
            crate::core::input_parse::KeyModifiers::CONTROL,
        ));
        assert_eq!(editor.text(), "two ");
        let _ = editor.handle_key(key(crate::core::input_parse::KeyCode::Home));
        let _ = editor.handle_key(key(crate::core::input_parse::KeyCode::Char('X')));
        assert_eq!(editor.text(), "Xtwo ");
    }

    #[test]
    fn a_hard_line_break_is_folded_to_a_space_on_commit() {
        let mut editor = label_editor(None);
        let _ = editor.handle_key(key(crate::core::input_parse::KeyCode::Char('a')));
        let _ = editor.handle_key(Key::new(
            crate::core::input_parse::KeyCode::Enter,
            crate::core::input_parse::KeyModifiers {
                shift: true,
                ..Default::default()
            },
        ));
        let _ = editor.handle_key(key(crate::core::input_parse::KeyCode::Char('b')));
        assert!(editor.text().contains('\n'));
        assert_eq!(
            editor.handle_key(key(crate::core::input_parse::KeyCode::Enter)),
            TreeLabelAction::Commit(Some("a b".into()))
        );
    }

    #[test]
    fn the_label_editor_renders_upstream_rows_with_a_caret() {
        let mut editor = label_editor(Some("ab"));
        let _ = editor.handle_key(key(crate::core::input_parse::KeyCode::Left));
        let lines = editor.render_lines();
        assert_eq!(lines[0], "  Label (empty to remove):");
        assert_eq!(lines[1], "  a\u{258d}b");
        assert!(lines[2].contains("save"), "{:?}", lines[2]);
        assert!(lines[2].contains("cancel"), "{:?}", lines[2]);
    }
}
