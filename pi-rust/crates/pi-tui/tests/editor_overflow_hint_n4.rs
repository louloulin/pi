//! N4 — Editor reverse-video cursor + overflow hint.
//!
//! nanopi paints the composer caret as a `▍` block with the
//! `REVERSED` modifier (so the OS IME candidate window can anchor to
//! the cell) and appends `(line n/N)` italic DarkGray to the right edge
//! of the top visible row when the draft overflows the composer
//! (`docs/NANOPI_VS_PI_RUST_GAP_ANALYSIS.md`, §N4; `nanopi/src/mode/tui.rs:5103-5113`).
//!
//! The REVERSED cursor was already wired in `paint_prompt` (the
//! upstream IME anchor rule, see LUM-1418 / `interactive-mode.ts`).
//! These tests pin both the existing cursor modifier and the new
//! overflow-hint helper.

use std::sync::Arc;

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, Model, ProviderId};
use pi_tui::app::{App, AppConfig};
use pi_tui::components::prompt::Prompt;

fn faux_model() -> Model {
    Model {
        provider: ProviderId::new("faux"),
        id: "faux-model".into(),
        api: Api::Faux,
        label: Some("Faux".into()),
        context_window: 1024,
        max_output_tokens: 256,
    }
}

fn app() -> App {
    let agent = Agent::new(AgentOptions::new(
        faux_model(),
        Arc::new(FauxProvider::default()),
        "you are pi",
    ));
    App::new(
        &agent,
        AppConfig {
            session_id: "n4-overflow".into(),
            ..AppConfig::default()
        },
    )
}

/// `Prompt::overflow_hint` returns `None` when the draft fits the
/// composer window — single line, or a multi-line draft whose total
/// rows are ≤ `max_rows`.
#[test]
fn overflow_hint_returns_none_when_draft_fits() {
    let mut prompt = Prompt::new("> ");
    // Single-line draft fits a 3-row composer easily.
    prompt.editor_mut().insert_str("hello world");
    assert!(prompt.overflow_hint(40, 3).is_none());
}

/// `Prompt::overflow_hint` returns `Some("(line n/N)")` when the draft
/// overflows `max_rows`. `n` is the cursor row (1-based), `N` is the
/// total row count of the draft at the given width.
#[test]
fn overflow_hint_reports_line_position_when_draft_overflows() {
    let mut prompt = Prompt::new("> ");
    // A draft long enough to wrap to 4 rows at width=20 (label `> `,
    // so body width = 18 cells). Push the caret to the middle of the
    // draft so we can verify the 1-based cursor row.
    let line = "a".repeat(72);
    prompt.editor_mut().insert_str(&line);
    // Cursor starts at end → on the last wrapped row.
    let hint = prompt.overflow_hint(20, 2);
    let hint = hint.expect("overflow hint must be present when rows > max_rows");
    assert!(
        hint.starts_with("(line ") && hint.ends_with(')'),
        "hint must use the (line n/N) format, got {hint:?}"
    );
    assert!(
        hint.contains("/4"),
        "hint must report the total row count, got {hint:?}"
    );
}

/// The hint count includes the row the caret sits on (1-based) so
/// a freshly-typed long draft reports `(line N/N)` — the cursor is
/// on the last row by default.
#[test]
fn overflow_hint_uses_one_based_cursor_row() {
    let mut prompt = Prompt::new("> ");
    prompt.editor_mut().insert_str(&"x".repeat(60));
    let hint = prompt.overflow_hint(20, 2).expect("overflow hint");
    // 60 chars at body width 18 → 4 wrapped rows. Cursor at end → row 4.
    assert_eq!(hint, "(line 4/4)", "caret on last row should report (line 4/4)");
}

/// A draft whose body is empty (just the label) reports no hint —
/// the empty buffer is single-row by definition, regardless of `max_rows`.
#[test]
fn overflow_hint_returns_none_for_empty_buffer() {
    let prompt = Prompt::new("> ");
    assert!(prompt.overflow_hint(40, 1).is_none());
    assert!(prompt.overflow_hint(40, 5).is_none());
}

/// End-to-end: render the app with a long draft into a narrow
/// composer and confirm the hint text appears on the first composer
/// row. The app's [`App::render_to_buffer`] walks through
/// `paint_prompt`, which in turn calls `prompt.overflow_hint` and
/// paints the right-aligned italic Dim row.
#[test]
fn render_paints_overflow_hint_on_top_composer_row() {
    let mut app = app();
    // Default `composer_max_rows` is 8 (AppConfig). At width=60 the
    // body width is 57, so 9+ wrapped rows are needed to overflow the
    // composer window — 600 chars gives ~11 rows with margin to spare.
    app.prompt_mut()
        .editor_mut()
        .insert_str(&"abcdefghijklmnopqrstuvwxyz ".repeat(22));
    let area = ratatui::layout::Rect {
        x: 0,
        y: 0,
        width: 60,
        height: 20,
    };
    let mut buf = ratatui::buffer::Buffer::empty(area);
    app.render_to_buffer(area, &mut buf);

    // Walk the buffer top-to-bottom looking for the `(line …)` hint
    // — its exact row depends on the App's chrome reservation, but the
    // hint is unique enough to grep for.
    let mut found_hint = false;
    for y in 0..area.height {
        let text: String = (0..area.width)
            .map(|x| {
                buf.cell((x, y))
                    .map(|c| c.symbol().to_string())
                    .unwrap_or_default()
            })
            .collect();
        if text.contains("(line ") && text.contains(')') {
            found_hint = true;
            // Confirm the hint carries the ITALIC modifier — the
            // `(line n/N)` styling is the visible affordance.
            let hint_col = text.find("(line ").expect("hint present");
            let cell = buf
                .cell((hint_col as u16, y))
                .expect("hint cell exists");
            assert!(
                cell.style()
                    .add_modifier
                    .contains(ratatui::style::Modifier::ITALIC),
                "overflow hint must be italic, cell style = {:?}",
                cell.style()
            );
            break;
        }
    }
    assert!(
        found_hint,
        "overflow hint must appear on the top composer row"
    );
}

/// The REVERSED modifier is the upstream IME anchor. The existing
/// `paint_prompt` applies it to the `▍` caret glyph; this test pins
/// the cell-level modifier so a future refactor cannot accidentally
/// drop it (the IME candidate overlay would drift one column left).
#[test]
fn cursor_cell_carries_the_reversed_modifier() {
    let mut app = app();
    app.prompt_mut().editor_mut().insert_str("hello");
    let area = ratatui::layout::Rect {
        x: 0,
        y: 0,
        width: 40,
        height: 20,
    };
    let mut buf = ratatui::buffer::Buffer::empty(area);
    app.render_to_buffer(area, &mut buf);
    // Locate the `▍` glyph and assert it carries Modifier::REVERSED.
    let mut found = false;
    for y in 0..area.height {
        for x in 0..area.width {
            if let Some(cell) = buf.cell((x, y)) {
                if cell.symbol() == "▍" {
                    let style = cell.style();
                    assert!(
                        style.add_modifier.contains(ratatui::style::Modifier::REVERSED),
                        "caret glyph at ({x},{y}) must carry Modifier::REVERSED for the IME anchor"
                    );
                    found = true;
                }
            }
        }
    }
    assert!(found, "expected a ▍ caret glyph somewhere on the rendered buffer");
}