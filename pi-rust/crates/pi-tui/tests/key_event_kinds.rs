//! LUM-1457 — a Windows console delivers a key **release** for every key
//! press, and the port used to act on both.
//!
//! `crossterm`'s Win32 backend turns every `KEY_EVENT` console record into a
//! `KeyEvent` with a `KeyEventKind`, and it does **not** filter the key-up
//! records: a probe that reads crossterm events inside a Windows ConPTY
//! prints, for one typed `ab`,
//!
//! ```text
//! KEY kind=Press   code=Char('a')
//! KEY kind=Release code=Char('a')
//! KEY kind=Press   code=Char('b')
//! KEY kind=Release code=Char('b')
//! ```
//!
//! `App::translate_event` mapped both kinds to `InputEvent::Key`, so on
//! Windows every keystroke ran its handler twice: typing `alpha` rendered
//! `aallpphhaa`, one `Backspace` deleted two characters, one `Enter`
//! submitted twice. Upstream pi runs on Node's `readline`, which never
//! delivers a release — one press, one action — so `Release` is now dropped
//! before translation. `Press` and `Repeat` (key auto-repeat) still map.
//!
//! This file is the regression gate. It starts at the raw crossterm layer
//! (`CtEvent::Key`) so it fails if the filter is removed, not merely if the
//! editor changes, and it ends at the editor the draft lands in, which is
//! what the user sees.
//!
//! ```text
//! cargo test -p pi-tui --test key_event_kinds
//! ```

use crossterm::event::KeyModifiers as CtModifiers;
use crossterm::event::{Event as CtEvent, KeyCode as CtKeyCode, KeyEvent, KeyEventKind};

use pi_tui::input::{InputEvent, KeyCode, KeyModifiers};
use pi_tui::{App, Editor, EditorAction};

/// One raw console key record, exactly as crossterm's Windows backend
/// reports it (including the key-up record for the same key).
fn console_key(code: CtKeyCode, kind: KeyEventKind) -> CtEvent {
    CtEvent::Key(KeyEvent::new_with_kind(code, CtModifiers::NONE, kind))
}

/// The events a real Windows console sends for one tap of `c`: press, then
/// release.
fn tap(c: char) -> Vec<CtEvent> {
    vec![
        console_key(CtKeyCode::Char(c), KeyEventKind::Press),
        console_key(CtKeyCode::Char(c), KeyEventKind::Release),
    ]
}

/// Feed a raw console stream through the translator and then into the editor,
/// i.e. exactly what `interactive.rs`'s event loop does per frame.
fn typed(stream: Vec<CtEvent>) -> Editor {
    let mut editor = Editor::new();
    for event in App::translate_events(stream) {
        editor.handle_event(event);
    }
    editor
}

#[test]
fn a_release_event_carries_no_input() {
    assert_eq!(
        App::translate_event(console_key(CtKeyCode::Char('a'), KeyEventKind::Release)),
        None,
        "a key-up record must not become an InputEvent"
    );
    assert_eq!(
        App::translate_event(console_key(CtKeyCode::Backspace, KeyEventKind::Release)),
        None
    );
}

#[test]
fn press_and_auto_repeat_still_translate() {
    assert_eq!(
        App::translate_event(console_key(CtKeyCode::Char('a'), KeyEventKind::Press)),
        Some(InputEvent::character('a'))
    );
    // Holding a key down is key auto-repeat, which is real input.
    assert_eq!(
        App::translate_event(console_key(CtKeyCode::Char('a'), KeyEventKind::Repeat)),
        Some(InputEvent::character('a'))
    );
}

#[test]
fn a_tapped_character_is_typed_once() {
    let editor = typed(tap('a'));
    assert_eq!(
        editor.text(),
        "a",
        "one physical keystroke must insert one character, not two"
    );
}

#[test]
fn a_typed_word_is_not_doubled() {
    // The exact symptom the ConPTY capture showed: `> aallpphhaa`.
    let stream: Vec<CtEvent> = "alpha".chars().flat_map(tap).collect();
    let editor = typed(stream);
    assert_eq!(editor.text(), "alpha");
}

#[test]
fn one_backspace_deletes_one_character() {
    let mut stream: Vec<CtEvent> = "ab".chars().flat_map(tap).collect();
    stream.push(console_key(CtKeyCode::Backspace, KeyEventKind::Press));
    stream.push(console_key(CtKeyCode::Backspace, KeyEventKind::Release));
    let editor = typed(stream);
    assert_eq!(
        editor.text(),
        "a",
        "the release record must not delete a second character"
    );
}

#[test]
fn one_enter_submits_once() {
    let mut stream: Vec<CtEvent> = tap('h');
    stream.push(console_key(CtKeyCode::Enter, KeyEventKind::Press));
    stream.push(console_key(CtKeyCode::Enter, KeyEventKind::Release));

    let mut editor = Editor::new();
    let mut submits = 0;
    for event in App::translate_events(stream) {
        if let EditorAction::Submit(text) = editor.handle_event(event) {
            submits += 1;
            assert_eq!(text, "h");
        }
    }
    assert_eq!(submits, 1, "one Enter press submits one draft");
}

#[test]
fn a_held_key_repeats() {
    // Press, auto-repeat, release: a held key types twice on every platform.
    let stream = vec![
        console_key(CtKeyCode::Char('x'), KeyEventKind::Press),
        console_key(CtKeyCode::Char('x'), KeyEventKind::Repeat),
        console_key(CtKeyCode::Char('x'), KeyEventKind::Release),
    ];
    assert_eq!(typed(stream).text(), "xx");
}

#[test]
fn control_chords_are_not_applied_twice() {
    // `Ctrl+W` is a kill-word chord: on Windows the key-up record used to
    // kill a second word, so `alpha beta gamma` + one Ctrl+W lost two words.
    let mut editor = Editor::new();
    editor.insert_str("alpha beta gamma");
    let ctrl_w = |kind| {
        CtEvent::Key(KeyEvent::new_with_kind(
            CtKeyCode::Char('w'),
            CtModifiers::CONTROL,
            kind,
        ))
    };
    for event in App::translate_events(vec![
        ctrl_w(KeyEventKind::Press),
        ctrl_w(KeyEventKind::Release),
    ]) {
        editor.handle_event(event);
    }
    assert_eq!(editor.text(), "alpha beta ");
}

#[test]
fn non_key_events_keep_their_translation() {
    // The filter is specific to key releases; the mouse path is untouched and
    // still yields exactly one `InputEvent` per crossterm event.
    let mouse = CtEvent::Mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::ScrollUp,
        column: 3,
        row: 4,
        modifiers: CtModifiers::NONE,
    });
    assert_eq!(
        App::translate_event(mouse),
        Some(InputEvent::wheel(true, false, 3, 4))
    );
    let resize = CtEvent::Resize(120, 40);
    assert_eq!(
        App::translate_events(vec![resize]),
        vec![InputEvent::Resize {
            width: 120,
            height: 40
        }]
    );
    // A chord built through the convenience constructor still matches, so
    // this file's editor-level cases are not accidentally testing nothing.
    assert_eq!(
        InputEvent::key(KeyCode::Char('w'), KeyModifiers::CONTROL),
        InputEvent::key(KeyCode::Char('w'), KeyModifiers::CONTROL)
    );
}
