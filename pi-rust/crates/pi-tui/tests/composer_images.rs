//! `app.clipboard.pasteImage` (`Alt+V`, LUM-1224): the composer image-chip
//! contract.
//!
//! The App owns no clipboard handle — the driver reads it and calls back into
//! `App::paste_image` / `App::paste_text`. These tests cover both paste paths,
//! the chip editing semantics, the capacity cap, and the interleaved
//! text+image order that reaches the agent.

use std::sync::Arc;
use std::time::Duration;

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, Content, ImageContent, Model, ProviderId};
use pi_tui::app::{App, AppConfig, StepOutcome, Submission};
use pi_tui::input::{InputEvent, Key, KeyCode, KeyModifiers};
use pi_tui::keybindings::reset_keybindings;
use tokio::sync::Mutex as AsyncMutex;

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

fn model() -> Agent {
    Agent::new(AgentOptions::new(
        faux_model(),
        Arc::new(FauxProvider::default()),
        "you are pi",
    ))
}

fn app() -> App {
    let agent = model();
    App::new(
        &agent,
        AppConfig {
            session_id: "images".into(),
            ..AppConfig::default()
        },
    )
}

fn image(data: &str) -> ImageContent {
    ImageContent {
        mime_type: "image/png".into(),
        data: data.into(),
    }
}

fn alt(code: KeyCode) -> Key {
    Key::new(
        code,
        KeyModifiers {
            alt: true,
            ..Default::default()
        },
    )
}

#[tokio::test]
async fn alt_v_records_a_clipboard_read_request() {
    reset_keybindings();
    let mut app = app();

    assert_eq!(app.step_key(alt(KeyCode::Char('v'))), StepOutcome::Redraw);
    // The driver sees the request exactly once.
    assert!(app.take_image_paste_request());
    assert!(!app.take_image_paste_request());
    // A plain character is not a paste request.
    app.step_key(Key::new(KeyCode::Char('v'), KeyModifiers::NONE));
    assert!(!app.take_image_paste_request());
}

#[tokio::test]
async fn paste_image_attaches_a_chip_and_paste_text_inserts_text() {
    let mut app = app();

    // The image path: a chip, not truncated bytes.
    app.set_editor_text("look at ");
    app.paste_image(image("AAAA"));
    assert_eq!(app.image_count(), 1);
    assert_eq!(app.editor_text(), "look at [Image #1]");

    // The no-image path: plain text insertion at the cursor.
    app.paste_text(" this");
    assert_eq!(app.editor_text(), "look at [Image #1] this");
    assert_eq!(app.image_count(), 1);
}

#[tokio::test]
async fn paste_image_stops_at_the_capacity_with_a_hint() {
    let mut app = app();
    for index in 0..8 {
        app.paste_image(image(&format!("img-{index}")));
    }
    assert_eq!(app.image_count(), 8);

    app.paste_image(image("overflow"));
    assert_eq!(app.image_count(), 8, "the ninth image is refused");
    let hint = app.status_flash().unwrap_or_default();
    assert!(
        hint.contains("At most 8"),
        "expected a capacity hint, got {hint:?}"
    );
}

#[tokio::test]
async fn clear_composer_drops_the_draft_and_its_chips() {
    let mut app = app();
    app.set_editor_text("draft");
    app.paste_image(image("AAAA"));
    app.clear_composer();
    assert_eq!(app.editor_text(), "");
    assert_eq!(app.image_count(), 0);
}

// Single-threaded runtime: `submit` marks the App busy synchronously and the
// spawned turn cannot run until this task awaits, so the refusal path is
// deterministic.
#[tokio::test]
async fn busy_enter_with_chips_keeps_the_whole_draft() {
    let agent = Arc::new(AsyncMutex::new(model()));
    let mut app = App::new(
        &*agent.lock().await,
        AppConfig {
            session_id: "images".into(),
            ..AppConfig::default()
        },
    );

    // `submit` marks the App busy synchronously, so the refusal path is
    // deterministic without waiting on the provider.
    app.submit(agent.clone(), "first turn");
    assert!(app.is_busy());

    app.set_editor_text("describe ");
    app.paste_image(image("one"));
    let before = app.editor_text();

    assert_eq!(
        app.step(InputEvent::Key(Key::new(
            KeyCode::Enter,
            KeyModifiers::NONE
        ))),
        StepOutcome::Redraw
    );
    // Nothing was consumed: the text and the chip are still in the composer.
    assert_eq!(app.editor_text(), before);
    assert_eq!(app.image_count(), 1);
    assert!(app
        .status_flash()
        .unwrap_or_default()
        .contains("Cannot attach images"));

    for _ in 0..300 {
        tokio::time::sleep(Duration::from_millis(5)).await;
        app.drain_agent_events();
        if !app.is_busy() {
            break;
        }
    }
}

#[test]
fn submission_orders_text_before_images() {
    let submission = Submission {
        text: "describe this".into(),
        images: vec![image("one"), image("two")],
        draft: None,
    };
    let blocks = submission.content_blocks();
    assert_eq!(blocks.len(), 3);
    assert!(matches!(&blocks[0], Content::Text(text) if text.text == "describe this"));
    assert!(matches!(&blocks[1], Content::Image(img) if img.data == "one"));
    assert!(matches!(&blocks[2], Content::Image(img) if img.data == "two"));
    // A text-only submission must not gain an empty image block.
    assert_eq!(Submission::from("hi").content_blocks().len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interleaved_draft_reaches_the_agent_in_order() {
    let agent = Arc::new(AsyncMutex::new(model()));
    let mut app = App::new(
        &*agent.lock().await,
        AppConfig {
            session_id: "images".into(),
            ..AppConfig::default()
        },
    );

    app.set_editor_text("before ");
    app.paste_image(image("one"));
    // Type between the chips the way the editor would — `set_editor_text`
    // deliberately drops attachments, so `insert_str` is the real path.
    app.prompt_mut().editor_mut().insert_str(" middle ");
    app.paste_image(image("two"));
    app.prompt_mut().editor_mut().insert_str(" after");
    assert_eq!(app.image_count(), 2);

    let submitted = Submission {
        text: app.editor_text(),
        images: app.prompt().images().to_vec(),
        draft: None,
    };
    app.clear_composer();
    app.submit(agent.clone(), submitted);

    for _ in 0..300 {
        tokio::time::sleep(Duration::from_millis(5)).await;
        app.drain_agent_events();
        if !app.is_busy() {
            break;
        }
    }

    let state = agent.lock().await;
    let first = state.state().messages.first().expect("user message");
    assert_eq!(
        first.content,
        vec![
            Content::text("before [Image #1] middle [Image #2] after"),
            Content::Image(image("one")),
            Content::Image(image("two")),
        ]
    );
}
