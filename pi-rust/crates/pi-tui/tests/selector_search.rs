//! End-to-end tests for the searchable, windowed [`Selector`].
//!
//! These drive the whole [`App`] input path — the same one the binary
//! uses — so they cover the wiring the `/model` and `/resume` pickers
//! rely on:
//!
//! 1. A searchable selector consumes printable keys as search text
//!    instead of letting them reach the prompt.
//! 2. The rendered rows and the `(n/total)` scroll indicator follow the
//!    filter and the cursor.
//! 3. `Enter` still returns the highlighted item's opaque `value`, which
//!    is what `pi-coding-agent` turns back into a model or session id.
//! 4. A plain (non-searchable) selector keeps its vim-style key map, so
//!    the extension `ctx.ui.select` dialog is unaffected.

use std::sync::Arc;

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, Model, ProviderId};
use pi_tui::app::{App, AppConfig, StepOutcome};
use pi_tui::input::{InputEvent, Key, KeyCode, KeyModifiers};
use pi_tui::selector::{Selector, SelectorAction, SelectorItem};

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
    let config = AppConfig {
        session_id: "selector-search".into(),
        ..AppConfig::default()
    };
    App::new(&agent, config)
}

fn model_items() -> Vec<SelectorItem> {
    vec![
        SelectorItem::new("model:gpt-5", "GPT-5").with_description("openai"),
        SelectorItem::new("model:claude-sonnet-4-5", "Claude Sonnet 4.5")
            .with_description("anthropic"),
        SelectorItem::new("model:gemini-2.5-pro", "Gemini 2.5 Pro").with_description("google"),
        SelectorItem::new("model:deepseek-v4-pro", "DeepSeek V4 Pro").with_description("deepseek"),
    ]
}

fn type_str(app: &mut App, text: &str) {
    for ch in text.chars() {
        app.step(InputEvent::character(ch));
    }
}

#[test]
fn typing_filters_the_list_without_reaching_the_prompt() {
    let mut app = app();
    app.open_selector(Selector::new("Pick a model", model_items()).searchable(true));

    type_str(&mut app, "gpt");

    // The prompt stays empty: the selector swallowed the keys.
    assert_eq!(app.render_snapshot(60, 12).prompt_buffer, "");
    let selector = app.selector().expect("selector is open");
    assert_eq!(selector.filter(), "gpt");
    assert_eq!(selector.filtered_len(), 1);
    assert_eq!(selector.selected_value(), Some("model:gpt-5"));

    let snapshot = app.render_snapshot(60, 12);
    let joined = snapshot.lines.join("\n");
    assert!(joined.contains("→ GPT-5"), "rows were {joined:?}");
    assert!(!joined.contains("Claude Sonnet 4.5"));
    // The snapshot mirrors the filtered rows for harnesses.
    assert_eq!(snapshot.selector_items.len(), 1);
}

#[test]
fn a_filter_without_matches_renders_the_no_match_line() {
    let mut app = app();
    app.open_selector(Selector::new("Pick a model", model_items()).searchable(true));

    type_str(&mut app, "zzz");

    let snapshot = app.render_snapshot(60, 12);
    let joined = snapshot.lines.join("\n");
    assert!(joined.contains("No matching items"), "rows were {joined:?}");
    assert!(snapshot.selector_items.is_empty());
}

#[test]
fn backspace_widens_the_filter_again() {
    let mut app = app();
    app.open_selector(Selector::new("Pick a model", model_items()).searchable(true));

    type_str(&mut app, "gpt");
    assert_eq!(app.selector().unwrap().filtered_len(), 1);
    app.step(InputEvent::key(KeyCode::Backspace, KeyModifiers::NONE));
    assert_eq!(app.selector().unwrap().filter(), "gp");
    app.step(InputEvent::key(KeyCode::Backspace, KeyModifiers::NONE));
    app.step(InputEvent::key(KeyCode::Backspace, KeyModifiers::NONE));
    assert_eq!(app.selector().unwrap().filter(), "");
    assert_eq!(app.selector().unwrap().filtered_len(), 4);
    // Backspace with an empty filter is a no-op, not an error.
    assert_eq!(
        app.step(InputEvent::key(KeyCode::Backspace, KeyModifiers::NONE)),
        StepOutcome::Idle,
    );
}

#[test]
fn arrows_navigate_the_filtered_view_and_enter_returns_its_value() {
    let mut app = app();
    let mut selector = Selector::new("Pick a model", model_items()).searchable(true);
    // `searchable` selectors treat `j` as search text, not as movement.
    selector.set_filter("model:");
    app.open_selector(selector);

    assert_eq!(
        app.step(InputEvent::key(KeyCode::Down, KeyModifiers::NONE)),
        StepOutcome::Redraw,
    );
    assert_eq!(
        app.selector().unwrap().selected_value(),
        Some("model:claude-sonnet-4-5"),
    );
    // The App reports the selection and leaves closing to the caller.
    assert_eq!(
        app.step(InputEvent::key(KeyCode::Enter, KeyModifiers::NONE)),
        StepOutcome::Redraw,
    );
    assert_eq!(
        app.selector().unwrap().selected_value(),
        Some("model:claude-sonnet-4-5"),
    );
}

#[test]
fn esc_reports_cancelled_without_dropping_the_filter() {
    let mut app = app();
    app.open_selector(Selector::new("Pick a model", model_items()).searchable(true));
    type_str(&mut app, "gpt");

    assert_eq!(
        app.step(InputEvent::key(KeyCode::Esc, KeyModifiers::NONE)),
        StepOutcome::Redraw,
    );
    // Upstream cancels on Esc even with an active query; the caller closes
    // the selector (mirrors `model-selector.ts`).
    assert_eq!(app.selector().unwrap().filter(), "gpt");
}

#[test]
fn a_windowed_selector_only_renders_max_visible_rows() {
    let items = (0..12)
        .map(|i| SelectorItem::new(format!("model:m{i}"), format!("Model {i}")))
        .collect::<Vec<_>>();
    let mut app = app();
    app.open_selector(
        Selector::new("Pick a model", items)
            .searchable(true)
            .with_max_visible(10),
    );

    let snapshot = app.render_snapshot(60, 30);
    let joined = snapshot.lines.join("\n");
    assert!(joined.contains("  (1/12)"), "rows were {joined:?}");
    assert!(joined.contains("→ Model 0"));
    assert!(!joined.contains("Model 11"));

    // Walking to the end scrolls the window.
    for _ in 0..11 {
        app.step(InputEvent::key(KeyCode::Down, KeyModifiers::NONE));
    }
    let joined = app.render_snapshot(60, 30).lines.join("\n");
    assert!(joined.contains("  (12/12)"), "rows were {joined:?}");
    assert!(joined.contains("→ Model 11"));
    assert!(!joined.contains("  Model 0"));
    assert!(!joined.contains("→ Model 0"));
}

#[test]
fn a_plain_selector_keeps_vim_navigation_and_ignores_typing() {
    let mut app = app();
    app.open_selector(Selector::new("Select: mode", model_items()));

    // `j` moves the cursor instead of filtering (upstream
    // `ExtensionSelectorComponent`).
    assert_eq!(app.step(InputEvent::character('j')), StepOutcome::Redraw,);
    assert_eq!(app.selector().unwrap().cursor(), 1);
    // Printable keys that are not navigation are swallowed.
    assert_eq!(app.step(InputEvent::character('z')), StepOutcome::Idle);
    assert_eq!(app.selector().unwrap().filter(), "");
    assert_eq!(app.selector().unwrap().filtered_len(), 4);

    let mut selector = Selector::new("Select: mode", model_items());
    assert_eq!(
        selector.handle_key(Key::new(KeyCode::Char('G'), KeyModifiers::NONE)),
        SelectorAction::Changed,
    );
    assert_eq!(selector.cursor(), 3);
}
