//! LUM-1259 tripwire: a command-reference block must keep its line structure.
//!
//! `/help` and `/hotkeys` print *pre-laid-out* text: `help_text()`
//! (`pi-coding-agent/src/commands/slash.rs`) builds one row per command and
//! one row per keybinding, with two-space indentation and aligned columns.
//! On the current tip that layout never reaches the screen — the body is
//! rendered through the plain-text path, and `wrap_text` → `split_words`
//! (`pi-tui/src/message.rs`, "join the runs with a single ASCII space")
//! re-flows the whole block into one paragraph, so every newline and the
//! indentation are gone.
//!
//! A real-PTY capture of the tip (`docs/screenshots/lum1259-tip-interaction.png`,
//! panel 7, and `lum1259-small-terminal.png`, panel 4) shows the damage: 11–16
//! consecutive rows that all read `> slash commands: /help show this help text
//! /clear clear the message view …`, indistinguishable from typed input.
//!
//! `Role::Info` + `push_info_block` (LUM-1238, unmerged as of `4b187b418`)
//! replaces the gutter with `· `, but it does **not** touch `wrap_text`, so the
//! legend still collapses. Its own test asserts the prefix against a
//! single-line fixture, which cannot see the re-flow.
//!
//! This test pins the missing half. It is `#[ignore]`d because the behaviour
//! does not exist yet — it is an acceptance test for the next round, not a
//! green gate:
//!
//! ```text
//! cargo test -p pi-tui --test lum1259_info_block_lines -- --ignored
//! ```

use pi_tui::message::MessageView;

/// The shape of the real `/help` body: a heading row, indented rows, a blank
/// row, another heading, more indented rows.
const COMMAND_REFERENCE: &str = "slash commands:\n  /help     show this help text\n  /clear    clear the message view\n\nkeys:\n  Enter       submit prompt\n  Ctrl+C      abort the current turn (or exit on idle)\n";

#[test]
#[ignore = "LUM-1259: /help-style blocks are re-flowed into one paragraph until \
            the plain-text path preserves newlines — see docs/TUI_INPUT_AND_LAYOUT_VERIFICATION.md"]
fn a_command_reference_block_keeps_its_line_structure() {
    let mut view = MessageView::new();
    view.push_info(COMMAND_REFERENCE);
    let lines = view.render_lines(100);

    // Six source rows (one of them blank) must not become one wrapped blob.
    assert!(
        lines.len() >= 5,
        "a 6-row command reference collapsed into {} row(s):\n{lines:#?}",
        lines.len()
    );

    // The two headings survive as their own rows...
    assert!(
        lines
            .iter()
            .any(|l| l.trim_end().ends_with("slash commands:")),
        "the 'slash commands:' heading lost its own row:\n{lines:#?}"
    );
    assert!(
        lines.iter().any(|l| l.trim_end().ends_with("keys:")),
        "the 'keys:' heading lost its own row:\n{lines:#?}"
    );

    // ...and a command row is not glued onto the row that follows it.
    let help_row = lines
        .iter()
        .position(|l| l.contains("/help"))
        .expect("the /help row is rendered");
    assert!(
        !lines[help_row].contains("/clear"),
        "/help and /clear were glued onto one row, so the columns are gone:\n{lines:#?}"
    );

    // The blank separator row is still a separator: the last command row and
    // the 'keys:' heading do not share a row.
    let keys_row = lines
        .iter()
        .position(|l| l.trim_end().ends_with("keys:"))
        .expect("the keys heading is rendered");
    assert!(
        !lines[keys_row].contains("/exit") && !lines[keys_row].contains("/hotkeys"),
        "the blank separator row was eaten, gluing the sections together:\n{lines:#?}"
    );
}
