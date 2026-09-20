//! Command-reference blocks keep their line structure.
//!
//! `/help` and `/hotkeys` print *pre-laid-out* text: `help_text()`
//! (`pi-coding-agent/src/commands/slash.rs`) builds one row per command and
//! one row per keybinding, with two-space indentation and aligned columns.
//!
//! Until LUM-1261 that layout never reached the screen. The body rendered
//! through the plain-text path, and `wrap_text` → `split_words`
//! (`pi-tui/src/message.rs`, "join the runs with a single ASCII space")
//! re-flowed the whole block into one paragraph, so every newline and the
//! indentation were gone — a real-PTY capture showed 11–16 consecutive rows
//! that all read `> slash commands: /help show this help text /clear clear the
//! message view …` (`docs/screenshots/lum1259-tip-interaction.png`, panel 7).
//!
//! `Role::Info` + `push_info_block` (LUM-1238) replaced the gutter with `· `,
//! but did not touch `wrap_text`, so the legend still collapsed. Its own test
//! asserted the prefix against a single-line fixture, which cannot see the
//! re-flow.
//!
//! LUM-1261 made `wrap_text` split the body at its hard line breaks first and
//! wrap each source line on its own, returning a row that already fits
//! verbatim — the same contract as upstream `wrapTextWithAnsi` /
//! `wrapSingleLine` (`packages/tui/src/utils.ts:843-876`). This test was
//! written as the acceptance gate for that fix (it was `#[ignore]`d while the
//! behaviour was missing, per `docs/TUI_INPUT_AND_LAYOUT_VERIFICATION.md`
//! §2.5) and is now a plain gate.
//!
//! ```text
//! cargo test -p pi-tui --test lum1259_info_block_lines
//! ```

use pi_tui::message::MessageView;

/// The shape of the real `/help` body: a heading row, indented rows, a blank
/// row, another heading, more indented rows.
const COMMAND_REFERENCE: &str = "slash commands:\n  /help     show this help text\n  /clear    clear the message view\n\nkeys:\n  Enter       submit prompt\n  Ctrl+C      abort the current turn (or exit on idle)\n";

#[test]
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

/// One rendered row per source row, in order — the structural half of §2.5's
/// completion standard ("一个源行至少一个渲染行"), asserted through the `Role::Info`
/// path the App actually uses for `/help`.
#[test]
fn an_info_block_renders_one_row_per_source_row() {
    let mut view = MessageView::new();
    view.push_info_block(COMMAND_REFERENCE);
    let lines = view.render_lines(100);

    assert_eq!(
        lines.len(),
        // A trailing hard break is its own (empty) row, exactly as upstream's
        // `split(/\r\n|\r|\n/)` counts it; `str::lines()` would hide that row.
        COMMAND_REFERENCE.split('\n').count(),
        "rendered rows do not match source rows:\n{lines:#?}"
    );
    assert_eq!(lines[0], "· slash commands:");
    // Indentation and the column run between command and description survive.
    assert_eq!(lines[1], "·   /help     show this help text");
    assert_eq!(lines[3], "· ");
}
