//! The real `/help` legend must reach the screen as the rows its author wrote.
//!
//! `help_text()` (`commands::slash.rs`) lays out ~20 command rows and ~9
//! keybinding rows with two-space indentation and an aligned description
//! column. The interactive App prints it through `App::info_block` →
//! `MessageView::push_info_block` (role `Role::Info`, `· ` gutter), which until
//! LUM-1261 sent the block through a paragraph re-flow and collapsed every
//! newline.
//!
//! `pi-tui`'s `tests/lum1259_info_block_lines.rs` pins the renderer against a
//! six-row fixture. This test is the other half of §2.5's completion standard
//! in `docs/TUI_INPUT_AND_LAYOUT_VERIFICATION.md`: the **real** legend, from the
//! crate that owns it, rendered at the two widths the TUI is actually used at.

use pi_coding_agent::commands::slash::{help_text, hotkeys_text};
use pi_tui::message::MessageView;

/// Every source row of `text` gets its own rendered row at `width`, provided
/// no source row is wider than the body column at that width.
fn assert_rows_survive(text: &str, width: u16) {
    let mut view = MessageView::new();
    view.push_info_block(text);
    let lines = view.render_lines(width);

    let source_rows: Vec<&str> = text.split('\n').collect();
    let body_width = width as usize - 2;
    let over_long = source_rows
        .iter()
        .filter(|row| row.chars().count() > body_width)
        .count();
    if over_long == 0 {
        assert_eq!(
            lines.len(),
            source_rows.len(),
            "at {width} columns a {}-row block rendered {} row(s):\n{lines:#?}",
            source_rows.len(),
            lines.len()
        );
    }
    // A row wider than the body column wraps instead of being dropped, so the
    // block may only grow.
    assert!(
        lines.len() >= source_rows.len(),
        "at {width} columns {} of {} source rows reached the screen:\n{lines:#?}",
        lines.len(),
        source_rows.len()
    );
}

#[test]
fn the_real_help_legend_keeps_one_row_per_source_row() {
    let help = help_text();
    // Sanity-check the fixture is the block this test is about: a heading, an
    // indented command row, the key section.
    assert!(help.starts_with("slash commands:\n"));
    assert!(help.contains("\n\nkeys:\n"));

    // 120 and 100 columns fit every row (the longest is 85 columns); 80 does
    // not, and there only "not lost" can be asserted row-for-row.
    for width in [120, 100, 80] {
        assert_rows_survive(&help, width);
    }
}

#[test]
fn the_real_hotkey_legend_keeps_one_row_per_source_row() {
    for width in [120, 100, 80] {
        assert_rows_survive(&hotkeys_text(), width);
    }
}

/// A narrow terminal wraps instead of losing rows or overflowing the viewport.
///
/// Three rows are wider than the 78-column body at 80 columns: `/thinking …`
/// (85) in the command section, plus two legend rows — `Up / Down` (81) and
/// `PgUp / PgDn` (79). The count moved from 2 to 3 in LUM-1450: the `keys:`
/// legend now pads its chord column to the widest *effective* chord
/// (`Ctrl+A / Ctrl+E`, 15 columns) instead of a hand-tuned 10-column literal,
/// so the two legend rows that were already near the limit crossed it. The
/// old third row (`Ctrl+C abort the current turn…`) disappeared because the
/// legend now names `app.interrupt` (`Esc`) and `app.clear` (`Ctrl+C`) as the
/// two separate actions they are. Wrapping is the intended behaviour — a
/// wrapped row costs a line, an overflowing one costs the row.
///
/// Both therefore go through the word-wrap, which collapses whitespace runs to
/// one space — and that is why the block grows by **fewer** rows than the
/// over-long count: after collapsing, some rows fit again. Growth is bounded by
/// the number of over-long rows, and the aliased rows keep their columns.
#[test]
fn an_eighty_column_terminal_wraps_instead_of_overflowing() {
    let help = help_text();
    let source_rows: Vec<&str> = help.split('\n').collect();
    let over_long = source_rows
        .iter()
        .filter(|row| row.chars().count() > 78)
        .count();
    assert_eq!(over_long, 3, "the fixture changed: {source_rows:#?}");

    let mut view = MessageView::new();
    view.push_info_block(&help);
    let lines = view.render_lines(80);

    assert!(
        lines.len() >= source_rows.len(),
        "rows were dropped:\n{lines:#?}"
    );
    assert!(
        lines.len() <= source_rows.len() + over_long,
        "the block grew by more than the over-long rows:\n{lines:#?}"
    );
    assert!(
        lines.iter().all(|line| line.chars().count() <= 80),
        "a rendered row overflowed 80 columns:\n{lines:#?}"
    );

    for heading in ["slash commands:", "keys:"] {
        let row = lines
            .iter()
            .position(|line| line.trim_end().ends_with(heading))
            .unwrap_or_else(|| panic!("the {heading} heading lost its own row:\n{lines:#?}"));
        assert!(!lines[row].contains('/'), "{lines:#?}");
    }

    // The indentation of a wrapped row survives on its first rendered row, and
    // its continuation follows it instead of being appended to the row after.
    let thinking = lines
        .iter()
        .position(|line| line.contains("/thinking"))
        .expect("the /thinking row is rendered");
    assert!(
        lines[thinking].starts_with("·   /thinking"),
        "the /thinking row lost its indent:\n{lines:#?}"
    );
    assert!(
        !lines[thinking].contains("xhigh"),
        "the /thinking row did not wrap:\n{lines:#?}"
    );
}
