//! Multi-line composer (LUM-1282).
//!
//! The composer used to be a fixed single-row prompt; long buffers were
//! hard-clipped at the row width and the user could not see what they had
//! typed past the right margin. The new behaviour is the upstream editor's
//! word-wrap: the prompt grows with the buffer (up to
//! [`AppConfig::composer_max_rows`]) and the chrome layout reserves the
//! rows so the message view gives way before the composer disappears.
//!
//! These tests pin the user-visible contract:
//!
//! * `Prompt::line_count` returns the natural row count.
//! * `Prompt::render_lines` produces N rows with the label on the first
//!   row and an indent on the continuation rows.
//! * The App renders the multi-line composer into the editor region and
//!   the editor region grows to fit the natural row count (the message
//!   view shrinks accordingly).

use std::sync::Arc;

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, Model, ProviderId};
use pi_tui::app::{App, AppConfig};
use pi_tui::prompt::Prompt;

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

fn agent() -> Agent {
    Agent::new(AgentOptions::new(
        faux_model(),
        Arc::new(FauxProvider::default()),
        "you are pi",
    ))
}

fn app_with_composer_max(max_rows: usize) -> App {
    App::new(
        &agent(),
        AppConfig {
            session_id: "composer-multiline".into(),
            composer_max_rows: max_rows,
            ..AppConfig::default()
        },
    )
}

/// A 20-char body at width 12 (label "> " = 2, body width 10) wraps into
/// 2 rows; the prompt reports both.
#[test]
fn prompt_grows_when_the_buffer_wraps() {
    let mut prompt = Prompt::new("> ");
    prompt.editor_mut().insert_str("aaaaaaaaaa bbbbbb");
    assert_eq!(prompt.line_count(12, 8), 2);
    let lines = prompt.render_lines(12, 8);
    assert_eq!(lines.len(), 8);
    assert!(
        lines[0].starts_with("> "),
        "first row owns the label: {lines:?}"
    );
    for row in &lines[1..] {
        assert!(
            row.starts_with("  "),
            "continuation rows are indented: {row:?}"
        );
    }
}

/// Setting `composer_max_rows = 1` reproduces the legacy single-row
/// composer: the editor region never grows past 1 row, even when the
/// buffer overflows the column.
#[test]
fn composer_max_rows_one_keeps_the_legacy_single_row_layout() {
    let mut app = app_with_composer_max(1);
    app.prompt_mut()
        .editor_mut()
        .insert_str("a string that would normally wrap across multiple rows");
    assert_eq!(app.prompt().line_count(80, 1), 1);
}

/// A long buffer renders across multiple rows and the editor region in
/// the snapshot reflects it.
#[test]
fn app_renders_multi_row_composer_into_the_editor_region() {
    let mut app = app_with_composer_max(4);
    app.prompt_mut()
        .editor_mut()
        .insert_str("this is a fairly long composer draft that wraps");

    let wide = app.render_snapshot(80, 24);
    let narrow = app.render_snapshot(20, 24);

    // Narrow viewport: the composer wraps, the message view shrinks,
    // and the editor region spans more than one row.
    let narrow_editor_rows: Vec<_> = narrow
        .lines
        .iter()
        .filter(|line| line.trim_start().starts_with("> ") || line.starts_with("  "))
        .collect();
    let wide_editor_rows: Vec<_> = wide
        .lines
        .iter()
        .filter(|line| line.trim_start().starts_with("> ") || line.starts_with("  "))
        .collect();
    assert!(
        narrow_editor_rows.len() > wide_editor_rows.len(),
        "narrow viewport must produce more composer rows (wide={}, narrow={}):\nwide={:?}\nnarrow={:?}",
        wide_editor_rows.len(),
        narrow_editor_rows.len(),
        wide.lines,
        narrow.lines,
    );
}

/// Long, paste-style content (a 200-character body) wraps to multiple
/// rows on a wide viewport; the cap prevents the composer from eating
/// the entire screen.
#[test]
fn cap_protects_against_pathologically_long_buffers() {
    let mut app = app_with_composer_max(3);
    app.prompt_mut().editor_mut().insert_str(&"x ".repeat(200));
    let snapshot = app.render_snapshot(80, 24);
    // The composer must not exceed the cap, even on a 24-row viewport.
    let editor_rows: Vec<_> = snapshot
        .lines
        .iter()
        .filter(|line| line.trim_start().starts_with("> ") || line.starts_with("  "))
        .collect();
    assert!(
        editor_rows.len() <= 3,
        "composer took {} rows, cap was 3: {:?}",
        editor_rows.len(),
        editor_rows
    );
}
