//! LUM-1447 — the transcript hint advertises the *effective* fold chord.
//!
//! `tool_fold_hint` used to hardcode `Ctrl+O`, so a `keybindings.json`
//! override moved the key that folds a tool block but not the text that
//! tells the reader which key that is (upstream resolves both through
//! `keyHint("app.tools.expand", "to expand")`,
//! `packages/coding-agent/src/modes/interactive/components/bash-execution.ts:180-184`).
//!
//! `set_keybindings` mutates process state, so every phase lives in one test
//! and this file is its own integration binary — the same rule
//! `pi-coding-agent/tests/keybinding_install.rs` documents.

use pi_tui::keybindings::{
    key_hint_or, key_text, key_text_or, reset_keybindings, set_keybindings,
    tui_default_keybindings, KeybindingDefinition, KeybindingsConfig, KeybindingsManager,
};
use pi_tui::message::{tool_fold_hint, MessageView, ToolBlock};
use pi_tui::styled::{SpanStyle, StyledSpan};
use pi_tui::theme::ThemeColor;

/// A manager whose `app.tools.expand` is bound to `keys` (empty = unbound).
fn manager(keys: Vec<&str>) -> KeybindingsManager {
    let mut definitions = tui_default_keybindings();
    definitions.push((
        "app.tools.expand".to_string(),
        KeybindingDefinition::new(keys),
    ));
    KeybindingsManager::new(definitions, KeybindingsConfig::new())
}

/// One collapsed tool block with `count` rich body lines.
fn view_with_tool_body(count: usize) -> MessageView {
    let mut view = MessageView::new();
    view.start_tool_execution("call-1", "bash", "{\"command\":\"echo hi\"}");
    let header = vec![vec![StyledSpan::new(
        "bash echo hi",
        SpanStyle::fg(ThemeColor::ToolTitle),
    )]];
    let body = (0..count)
        .map(|idx| {
            vec![StyledSpan::new(
                format!("body-{idx}"),
                SpanStyle::fg(ThemeColor::ToolOutput),
            )]
        })
        .collect();
    view.finish_tool_execution_with_lines(
        "call-1",
        3,
        "body",
        false,
        Some(ToolBlock::new(header, body)),
    );
    view
}

#[test]
fn the_fold_hint_follows_the_effective_keybindings() {
    // A bare `pi-tui` registry has no `app.tools.expand` at all: the shipped
    // default stands in, which is why the unit test in `message.rs` (which
    // never installs a manager) still reads `Ctrl+O`.
    reset_keybindings();
    assert_eq!(tool_fold_hint(6), "… (+6 lines, Ctrl+O to expand)");
    assert_eq!(key_text_or("app.tools.expand", "Ctrl+O"), "Ctrl+O");

    // Default in a table that *does* own the id: unchanged.
    set_keybindings(manager(vec!["ctrl+o"]));
    assert_eq!(tool_fold_hint(6), "… (+6 lines, Ctrl+O to expand)");
    assert_eq!(key_text("app.tools.expand").as_deref(), Some("Ctrl+O"));

    // A user override moves the key *and* the hint. Upstream's transcript
    // (the same string `MessageView` renders) has to agree.
    set_keybindings(manager(vec!["ctrl+u"]));
    assert_eq!(tool_fold_hint(6), "… (+6 lines, Ctrl+U to expand)");
    let lines = view_with_tool_body(10).render_lines(40);
    assert_eq!(lines[1], "* … (+6 lines, Ctrl+U to expand)");

    // Two chords join with `/`, the same spelling `/hotkeys` and the
    // startup header use.
    set_keybindings(manager(vec!["ctrl+u", "alt+o"]));
    assert_eq!(tool_fold_hint(6), "… (+6 lines, Ctrl+U/Alt+O to expand)");

    // Explicitly unbound: the description survives, the dead chord does not.
    set_keybindings(manager(vec![]));
    assert_eq!(tool_fold_hint(6), "… (+6 lines, to expand)");
    assert_eq!(key_text("app.tools.expand"), None);
    assert_eq!(key_text_or("app.tools.expand", "Ctrl+O"), "");
    assert_eq!(
        key_hint_or("app.tools.expand", "Ctrl+O", "to expand"),
        "to expand"
    );
    // A rebound-but-unbound pair still renders the description alone.
    assert_eq!(
        view_with_tool_body(10).render_lines(40)[1],
        "* … (+6 lines, to expand)"
    );

    // An id outside the table is never a panic and never a dead chord.
    assert_eq!(key_text("app.no.such.action"), None);
    assert_eq!(key_text_or("app.no.such.action", "Ctrl+O"), "Ctrl+O");

    reset_keybindings();
}
