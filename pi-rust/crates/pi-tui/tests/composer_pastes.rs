//! Composer paste folding (LUM-1318).
//!
//! Pasting a few hundred lines used to inline every byte into the buffer:
//! the composer grew to hundreds of rows, `Ctrl+U` / `Ctrl+K` acted on a
//! wall of pasted text instead of the draft, and nothing could be typed
//! around it any more. The composer now folds a paste the way upstream
//! `handlePaste` does — past
//! [`PASTE_FOLD_LINE_THRESHOLD`](pi_tui::editor::PASTE_FOLD_LINE_THRESHOLD)
//! lines or
//! [`PASTE_FOLD_CHAR_THRESHOLD`](pi_tui::editor::PASTE_FOLD_CHAR_THRESHOLD)
//! characters the text goes into a side table and the buffer holds one sentinel
//! that renders as `[paste #N +M lines]`.
//!
//! These tests drive the real [`App`] and the real key path. The contract they
//! pin is the one that matters to a user: the composer stays short, `Enter`
//! hands the *whole* pasted text to the model (never the marker), and a folded
//! paste behaves like any other one-character unit while editing.
//!
//! The real-PTY half of the evidence (the same scenario against the built
//! binary, with screenshots) lives in
//! `docs/LUM1318_PASTE_FOLD_AND_AUDIT.md`.

use std::sync::Arc;
use std::time::Duration;

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, Content, ImageContent, Model, ProviderId};
use pi_tui::app::{App, AppConfig, StepOutcome, Submission};
use pi_tui::editor::{PASTE_CHAR, PASTE_FOLD_CHAR_THRESHOLD, PASTE_FOLD_LINE_THRESHOLD};
use pi_tui::input::{InputEvent, Key, KeyCode, KeyModifiers};
use tokio::sync::Mutex as AsyncMutex;

const WIDTH: u16 = 90;
const HEIGHT: u16 = 30;

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
    App::new(
        &model(),
        AppConfig {
            session_id: "composer-pastes".into(),
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

/// A paste of `lines` numbered rows — the shape a real paste has: short
/// lines, many of them.
fn paste(lines: usize) -> String {
    (1..=lines)
        .map(|row| format!("pasted line {row}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn enter() -> InputEvent {
    InputEvent::Key(Key::new(KeyCode::Enter, KeyModifiers::NONE))
}

/// Submit through the real key path and hand back the announced draft.
fn submit_with_enter(app: &mut App) -> Submission {
    match app.step(enter()) {
        StepOutcome::Submitted(submission) => submission,
        other => panic!("Enter must submit, got {other:?}"),
    }
}

async fn drain(app: &mut App) {
    for _ in 0..300 {
        tokio::time::sleep(Duration::from_millis(5)).await;
        app.drain_agent_events();
        if !app.is_busy() {
            return;
        }
    }
}

#[test]
fn a_200_line_paste_keeps_the_composer_short() {
    let mut app = app();
    let pasted = paste(200);
    app.paste_text(&pasted);

    // The draft: one marker on one row.
    assert_eq!(app.prompt().text(), "[paste #1 +200 lines]");
    assert_eq!(app.prompt().line_count(WIDTH, 8), 1);
    assert_eq!(app.prompt().editor().paste_count(), 1);
    assert_eq!(app.prompt().editor().text(), PASTE_CHAR.to_string());

    // The pre-fold port would have drawn 200 rows here; the composer window
    // is what the screenshot in the audit doc shows.
    let snapshot = app.render_snapshot(WIDTH, HEIGHT);
    let composer_rows: Vec<&String> = snapshot
        .lines
        .iter()
        .filter(|line| line.trim_start().starts_with("> ") || line.starts_with("  "))
        .collect();
    assert!(
        composer_rows.len() <= 2,
        "composer took {} rows for 200 pasted lines: {composer_rows:?}",
        composer_rows.len()
    );
    assert!(
        snapshot
            .lines
            .iter()
            .any(|line| line.contains("[paste #1 +200 lines]")),
        "the marker is what the pane shows: {:?}",
        snapshot.lines
    );
    // ... and the bytes are still there for whoever needs them.
    assert_eq!(app.editor_text(), pasted);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn enter_submits_the_whole_pasted_text_and_the_transcript_shows_it() {
    let agent = Arc::new(AsyncMutex::new(model()));
    let mut app = App::new(
        &*agent.lock().await,
        AppConfig {
            session_id: "composer-pastes".into(),
            ..AppConfig::default()
        },
    );
    let pasted = paste(200);
    app.paste_text("summarise this:\n");
    app.paste_text(&pasted);

    let submission = submit_with_enter(&mut app);
    assert_eq!(
        submission.text,
        format!("summarise this:\n{pasted}"),
        "the model gets the pasted bytes, not the marker"
    );
    assert!(!submission.text.contains("[paste #"));
    // `Submit` text is byte-for-byte the paste, not a re-wrapped or trimmed
    // version of it.
    assert!(submission.text.ends_with(&pasted));

    app.submit(agent.clone(), submission);
    drain(&mut app).await;

    let state = agent.lock().await;
    let first = state.state().messages.first().expect("user message");
    assert_eq!(
        first.content,
        vec![Content::text(format!("summarise this:\n{pasted}"))]
    );
    drop(state);

    // The transcript keeps the full text too — that is what the user sent.
    let rendered = app.render_snapshot(WIDTH, HEIGHT);
    assert!(
        rendered
            .lines
            .iter()
            .any(|line| line.contains("pasted line 1")),
        "the transcript shows the pasted text"
    );
}

#[test]
fn small_pastes_stay_literal() {
    let mut app = app();
    // Exactly at both thresholds: still literal, both by height and by
    // length.
    let ten_lines = paste(PASTE_FOLD_LINE_THRESHOLD);
    let thousand_chars = "x".repeat(PASTE_FOLD_CHAR_THRESHOLD);
    app.paste_text(&ten_lines);
    app.paste_text("\n");
    app.paste_text(&thousand_chars);
    assert_eq!(app.prompt().editor().paste_count(), 0);
    assert!(!app.prompt().editor().text().contains(PASTE_CHAR));
    assert_eq!(
        app.prompt().text(),
        format!("{ten_lines}\n{thousand_chars}")
    );

    // One row/length past either threshold folds.
    let mut folded = App::new(
        &model(),
        AppConfig {
            session_id: "composer-pastes".into(),
            ..AppConfig::default()
        },
    );
    folded.paste_text(&paste(PASTE_FOLD_LINE_THRESHOLD + 1));
    folded.paste_text(&"x".repeat(PASTE_FOLD_CHAR_THRESHOLD + 1));
    assert_eq!(folded.prompt().editor().paste_count(), 2);
    assert_eq!(
        folded.prompt().text(),
        "[paste #1 +11 lines][paste #2 1001 chars]"
    );
}

#[test]
fn a_folded_paste_and_image_chips_keep_their_own_payloads() {
    let mut app = app();
    let first = paste(30);
    let second = paste(40);
    app.set_editor_text("before ");
    app.paste_image(image("one"));
    app.prompt_mut().editor_mut().insert_str(" middle ");
    app.prompt_mut().editor_mut().insert_str(&first);
    app.prompt_mut().editor_mut().insert_str(" after ");
    app.paste_image(image("two"));
    app.prompt_mut().editor_mut().insert_str(&second);

    assert_eq!(app.image_count(), 2);
    assert_eq!(app.prompt().editor().paste_count(), 2);
    assert_eq!(
        app.prompt().text(),
        "before [Image #1] middle [paste #1 +30 lines] after [Image #2][paste #2 +40 lines]"
    );
    assert_eq!(
        app.prompt()
            .editor()
            .paste_attachments()
            .iter()
            .map(|paste| paste.text().to_string())
            .collect::<Vec<_>>(),
        vec![first.clone(), second.clone()]
    );

    let submission = submit_with_enter(&mut app);
    assert_eq!(
        submission.text,
        format!("before [Image #1] middle {first} after [Image #2]{second}")
    );
    assert_eq!(submission.images, vec![image("one"), image("two")]);
    assert_eq!(submission.content_blocks().len(), 3);
    assert!(matches!(
        &submission.content_blocks()[0],
        Content::Text(text) if text.text == submission.text
    ));
}

#[test]
fn backspace_takes_a_folded_paste_out_as_one_unit_and_renumbers() {
    let mut app = app();
    let first = paste(20);
    let second = paste(30);
    app.paste_text(&first);
    app.paste_text(&second);
    assert_eq!(
        app.prompt().text(),
        "[paste #1 +20 lines][paste #2 +30 lines]"
    );

    // The cursor is at the end of the draft: one Backspace removes the whole
    // second marker, not one character of its label.
    assert_eq!(app.step(backspace()), StepOutcome::Redraw);
    assert_eq!(app.prompt().text(), "[paste #1 +20 lines]");
    assert_eq!(app.prompt().editor().paste_count(), 1);
    assert_eq!(app.prompt().editor().paste_attachments()[0].text(), first);

    // ... and the marker that is gone does not come back through `Ctrl+-`
    // with no payload: undo restores the marker *and* its text.
    assert_eq!(app.step(ctrl_minus()), StepOutcome::Redraw);
    assert_eq!(
        app.prompt().text(),
        "[paste #1 +20 lines][paste #2 +30 lines]"
    );
    assert_eq!(app.prompt().editor().paste_attachments()[1].text(), second);
}

#[tokio::test]
async fn a_recalled_entry_keeps_the_chip_and_spells_the_marker_out() {
    let agent = Arc::new(AsyncMutex::new(model()));
    let mut app = App::new(
        &*agent.lock().await,
        AppConfig {
            session_id: "composer-pastes".into(),
            ..AppConfig::default()
        },
    );
    let pasted = paste(20);
    app.set_editor_text("look ");
    app.paste_image(image("one"));
    app.prompt_mut().editor_mut().insert_str(&pasted);
    let submitted = submit_with_enter(&mut app);
    assert_eq!(submitted.text, format!("look [Image #1]{pasted}"));

    app.submit(agent.clone(), submitted);

    // Recall: the chip comes back (its payload is an in-session attachment),
    // and the folded paste is back as the *text* the entry stored — history
    // keeps the expanded prompt, exactly like upstream's `addToHistory`, so a
    // recalled 20-line paste is 20 lines of draft again. What must not
    // happen is a marker with no text behind it.
    assert_eq!(app.step(InputEvent::Key(up())), StepOutcome::Redraw);
    assert_eq!(app.image_count(), 1);
    assert_eq!(app.prompt().editor().paste_count(), 0);
    assert!(!app.prompt().editor().text().contains(PASTE_CHAR));
    assert_eq!(app.prompt().text(), format!("look [Image #1]{pasted}"));
}

#[test]
fn follow_up_submits_the_expanded_text() {
    let mut app = app();
    let pasted = paste(50);
    app.paste_text(&pasted);

    match app.follow_up_from_editor() {
        pi_tui::app::FollowUpOutcome::Submitted(submission) => {
            assert_eq!(submission.text, pasted);
            assert_eq!(
                submission.raw_text.as_deref(),
                Some(PASTE_CHAR.to_string().as_str())
            );
        }
        other => panic!("an idle App submits the follow-up, got {other:?}"),
    }
}

#[test]
fn clear_composer_drops_the_folded_paste() {
    let mut app = app();
    app.paste_text(&paste(200));
    app.clear_composer();
    assert_eq!(app.prompt().editor().paste_count(), 0);
    assert_eq!(app.prompt().text(), "");
    assert_eq!(app.editor_text(), "");
}

fn backspace() -> InputEvent {
    InputEvent::Key(Key::new(KeyCode::Backspace, KeyModifiers::NONE))
}

fn up() -> Key {
    Key::new(KeyCode::Up, KeyModifiers::NONE)
}

/// `Ctrl+-` (`tui.editor.undo`) — the byte crossterm reports for it.
fn ctrl_minus() -> InputEvent {
    InputEvent::Key(Key::new(KeyCode::Char('_'), KeyModifiers::CONTROL))
}
