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
//! Not ported (upstream-only, out of scope): folding (`⊞`/`⊟`), the tree
//! label editor, the three filter modes and horizontal viewport scrolling.
//! Because filtering is not re-flattened, a fuzzy hit keeps its original
//! indent/gutter instead of being re-indented as upstream's
//! `recomputeVisualStructure` would.
//!
//! [`TreeRow`] is the flattened row: `prefix` is the indent + gutter +
//! connector string, `label` is the node's own text, and
//! [`tree_selector_items`] composes them (plus the `•` active-path
//! marker) into [`SelectorItem`]s the existing [`Selector`](crate::Selector)
//! renders, filters and windows.
//!
//! [`Selector`]: crate::selector::Selector

use std::collections::HashMap;

use crate::selector::SelectorItem;

/// One node of the tree the flattening API accepts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeItem {
    /// Stable identity of the node (the session entry id for `/tree`).
    pub value: String,
    /// The node's own display text, without indent or gutter.
    pub label: String,
    /// Optional secondary column.
    pub description: Option<String>,
    /// Children in stored order.
    pub children: Vec<TreeItem>,
}

impl TreeItem {
    /// A leaf with `value` / `label` and no description or children.
    pub fn new(value: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            label: label.into(),
            description: None,
            children: Vec::new(),
        }
    }

    /// Attach the secondary description column.
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
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
    /// Optional secondary column.
    pub description: Option<String>,
    /// True when this node lies on the root-to-active-leaf path.
    pub on_active_path: bool,
}

/// Flatten a forest into pre-order rows.
///
/// The subtree containing `active_value` (typically the session's current
/// leaf) is emitted first at every level, so the branch the cursor sits on
/// is not buried under older forks. Child order is otherwise preserved.
pub fn flatten_tree(roots: &[TreeItem], active_value: Option<&str>) -> Vec<TreeRow> {
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
        });
    }

    let mut rows = Vec::new();
    while let Some(frame) = stack.pop() {
        rows.push(TreeRow {
            value: frame.node.value.clone(),
            indent: frame.indent,
            prefix: render_prefix(&frame, multiple_roots),
            label: frame.node.label.clone(),
            description: frame.node.description.clone(),
            on_active_path: is_active(frame.node),
        });

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
            });
        }
    }
    rows
}

/// Compose flattened rows into selector items, baking the indent + gutter
/// and the `•` active-path marker into the label.
pub fn tree_selector_items(rows: &[TreeRow]) -> Vec<SelectorItem> {
    rows.iter()
        .map(|row| {
            let marker = if row.on_active_path { "• " } else { "" };
            let label = format!("{}{}{}", row.prefix, marker, row.label);
            let item = SelectorItem::new(row.value.clone(), label);
            match &row.description {
                Some(description) => item.with_description(description.clone()),
                None => item,
            }
        })
        .collect()
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
fn render_prefix(frame: &Frame<'_>, multiple_roots: bool) -> String {
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
                // Upstream puts the fold indicator (`⊟`/`⊞`) here; folding
                // is not ported, so a connector always shows an unbroken
                // `─`.
                1 => prefix.push('─'),
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
        assert_eq!(by_value("y").prefix, "├─ ");
        assert_eq!(by_value("x").prefix, "└─ ");
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
}
