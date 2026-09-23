//! LUM-1450 — the modal lists and the dialog footers follow the effective
//! keybindings.
//!
//! Upstream reads every one of these chords off the registry:
//!
//! * `SelectList::handleInput` — `tui.select.up` / `down` / `confirm` /
//!   `cancel` (`packages/tui/src/components/select-list.ts:144-175`);
//! * `SettingsList::handleInput` — the same four
//!   (`packages/tui/src/components/settings-list.ts:231-246`);
//! * `ExtensionInputComponent` — `keyHint("tui.select.confirm", "submit")` /
//!   `keyHint("tui.select.cancel", "cancel")`
//!   (`components/extension-input.ts:67,75-77`), and the extension confirm is
//!   a Yes/No `SelectList`.
//!
//! The Rust port hardcoded `Enter` / `Esc` / arrows in all three, while
//! `/hotkeys` advertised the ids as rebindable — so a `keybindings.json`
//! override was a chord that did nothing inside a modal. These tests pin both
//! halves of the fix: the *behaviour* (a rebound chord moves / confirms /
//! cancels) and the *footer* (the painted affordance names the chord that
//! actually works).
//!
//! Two halves of the component always read the **process-wide** registry: the
//! `handle_key` wrappers (the ones the driver and the App call) and the
//! rendered hints (`key_text_or`, the same helper the transcript's fold hint
//! uses). The injected `*_with` halves are used for the pure behaviour cases;
//! everything that goes through the global slot installs a table inside
//! [`with_table`], which serialises on a mutex and restores the previous table
//! even when an assertion panics.

use std::sync::Arc;

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, Model, ProviderId, UiRequest, UiResponse};
use pi_tui::app::{App, AppConfig, StepOutcome};
use pi_tui::dialog::{Dialog, DialogAction};
use pi_tui::input::{InputEvent, Key, KeyCode, KeyModifiers};
use pi_tui::keybindings::{
    get_keybindings, set_keybindings, tui_default_keybindings, KeybindingsConfig,
    KeybindingsManager,
};
use pi_tui::selector::{Selector, SelectorAction, SelectorItem};
use pi_tui::settings::{SettingItem, SettingsAction, SettingsList};
use tokio::sync::oneshot;

/// `set_keybindings` mutates process state, so the cases that install a table
/// run one at a time inside this binary.
static REGISTRY: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The shipped `pi-tui` table plus `overrides`.
fn manager(overrides: &[(&str, &[&str])]) -> KeybindingsManager {
    let mut config = KeybindingsConfig::default();
    for (id, keys) in overrides {
        config.set(*id, keys.iter().copied());
    }
    KeybindingsManager::new(tui_default_keybindings(), config)
}

/// Run `body` with `overrides` installed as the process-wide table.
fn with_table<R>(overrides: &[(&str, &[&str])], body: impl FnOnce() -> R) -> R {
    let _guard = REGISTRY
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let previous = get_keybindings();
    set_keybindings(manager(overrides));
    let result = body();
    set_keybindings(previous);
    result
}

fn ctrl(c: char) -> Key {
    Key::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

fn alt(c: char) -> Key {
    Key::new(KeyCode::Char(c), KeyModifiers::ALT)
}

fn plain(c: char) -> Key {
    Key::new(KeyCode::Char(c), KeyModifiers::NONE)
}

fn bare(code: KeyCode) -> Key {
    Key::new(code, KeyModifiers::NONE)
}

fn items() -> Vec<SelectorItem> {
    vec![
        SelectorItem::new("a", "Alpha"),
        SelectorItem::new("b", "Beta"),
        SelectorItem::new("c", "Gamma"),
    ]
}

fn setting_items() -> Vec<SettingItem> {
    vec![
        SettingItem::new("theme", "Theme").with_values(["dark", "light"], "dark"),
        SettingItem::new("density", "Density").with_values(["cozy", "compact"], "cozy"),
    ]
}

fn confirm_dialog() -> (Dialog, oneshot::Receiver<Option<UiResponse>>) {
    let (tx, rx) = oneshot::channel();
    let dialog = Dialog::new(
        UiRequest::Confirm {
            title: "Deploy".into(),
            body: "Ship it?".into(),
        },
        tx,
    );
    (dialog, rx)
}

fn input_dialog() -> (Dialog, oneshot::Receiver<Option<UiResponse>>) {
    let (tx, rx) = oneshot::channel();
    let dialog = Dialog::new(
        UiRequest::Input {
            title: "Name".into(),
            placeholder: Some("session".into()),
        },
        tx,
    );
    (dialog, rx)
}

fn select_dialog() -> (Dialog, oneshot::Receiver<Option<UiResponse>>) {
    let (tx, rx) = oneshot::channel();
    let dialog = Dialog::new(
        UiRequest::Select {
            title: "Pick".into(),
            options: vec!["a".into(), "b".into()],
        },
        tx,
    );
    (dialog, rx)
}

// ---------------------------------------------------------------- behaviour

#[test]
fn the_shipped_table_is_the_old_hardcoded_behaviour() {
    // No regression: with the defaults in place every chord the component used
    // to answer still answers.
    let kb = manager(&[]);
    let mut sel = Selector::new("Pick", items());

    assert_eq!(
        sel.handle_key_with(&kb, bare(KeyCode::Down)),
        SelectorAction::Changed
    );
    assert_eq!(sel.cursor(), 1);
    assert_eq!(
        sel.handle_key_with(&kb, bare(KeyCode::Up)),
        SelectorAction::Changed
    );
    assert_eq!(sel.cursor(), 0);
    assert_eq!(
        sel.handle_key_with(&kb, bare(KeyCode::PageDown)),
        SelectorAction::Changed
    );
    assert_eq!(sel.cursor(), 2);
    assert_eq!(
        sel.handle_key_with(&kb, plain('g')),
        SelectorAction::Changed
    );
    assert_eq!(sel.cursor(), 0);
    assert_eq!(
        sel.handle_key_with(&kb, bare(KeyCode::Enter)),
        SelectorAction::Selected("a".into())
    );
    assert_eq!(
        sel.handle_key_with(&kb, bare(KeyCode::Esc)),
        SelectorAction::Cancelled
    );
    // `tui.select.cancel` ships as `escape` *and* `ctrl+c`.
    assert_eq!(
        sel.handle_key_with(&kb, ctrl('c')),
        SelectorAction::Cancelled
    );
}

#[test]
fn a_rebound_select_up_moves_the_picker() {
    // codex's `Ctrl+P` for the previous row — the exact override a user writes
    // in `keybindings.json`, and which used to be a dead chord inside a modal.
    let kb = manager(&[
        ("tui.select.up", &["ctrl+p"]),
        ("tui.select.down", &["ctrl+n"]),
    ]);
    let mut sel = Selector::new("Pick", items());

    assert_eq!(sel.handle_key_with(&kb, ctrl('n')), SelectorAction::Changed);
    assert_eq!(sel.cursor(), 1);
    assert_eq!(sel.handle_key_with(&kb, ctrl('p')), SelectorAction::Changed);
    assert_eq!(sel.cursor(), 0);
    // Ctrl+P at the top wraps, exactly like `Up` did.
    assert_eq!(sel.handle_key_with(&kb, ctrl('p')), SelectorAction::Changed);
    assert_eq!(sel.cursor(), 2);
    // The old chord is no longer a movement: the override *replaced* it.
    assert_eq!(
        sel.handle_key_with(&kb, bare(KeyCode::Down)),
        SelectorAction::None
    );
    assert_eq!(sel.cursor(), 2);

    // The searchable path answers the same chords (the model / session pickers).
    let mut searchable = Selector::new("Pick", items()).searchable(true);
    assert_eq!(
        searchable.handle_key_with(&kb, ctrl('n')),
        SelectorAction::Changed
    );
    assert_eq!(searchable.cursor(), 1);
    // ... and `ctrl+p` is no longer swallowed as "a control chord for the app".
    assert_eq!(
        searchable.handle_key_with(&kb, ctrl('p')),
        SelectorAction::Changed
    );
    assert_eq!(searchable.cursor(), 0);
    assert_eq!(
        searchable.filter(),
        "",
        "the chord must not reach the filter"
    );
}

#[test]
fn a_rebound_confirm_and_cancel_are_answered_by_the_picker() {
    let kb = manager(&[
        ("tui.select.confirm", &["f2"]),
        ("tui.select.cancel", &["alt+x"]),
    ]);
    let mut sel = Selector::new("Pick", items());
    sel.handle_key_with(&kb, bare(KeyCode::Down));

    assert_eq!(
        sel.handle_key_with(&kb, bare(KeyCode::F(2))),
        SelectorAction::Selected("b".into())
    );
    // `Enter` is not the confirm chord any more, and it is not a chord at all.
    let mut sel = Selector::new("Pick", items());
    assert_eq!(
        sel.handle_key_with(&kb, bare(KeyCode::Enter)),
        SelectorAction::None
    );
    assert_eq!(
        sel.handle_key_with(&kb, alt('x')),
        SelectorAction::Cancelled
    );
    // `Esc` was replaced along with the binding.
    assert_eq!(
        sel.handle_key_with(&kb, bare(KeyCode::Esc)),
        SelectorAction::None
    );
}

#[test]
fn the_settings_list_answers_the_same_four_chords() {
    let kb = manager(&[
        ("tui.select.up", &["ctrl+p"]),
        ("tui.select.down", &["ctrl+n"]),
        ("tui.select.confirm", &["f2"]),
        ("tui.select.cancel", &["alt+x"]),
    ]);
    let mut list = SettingsList::new(setting_items(), 10);

    assert_eq!(
        list.handle_key_with(&kb, ctrl('n')),
        SettingsAction::Changed
    );
    assert_eq!(list.cursor(), 1);
    assert_eq!(
        list.handle_key_with(&kb, ctrl('p')),
        SettingsAction::Changed
    );
    assert_eq!(list.cursor(), 0);
    assert_eq!(
        list.handle_key_with(&kb, bare(KeyCode::F(2))),
        SettingsAction::ValueChanged {
            id: "theme".into(),
            value: "light".into()
        }
    );
    assert_eq!(
        list.handle_key_with(&kb, alt('x')),
        SettingsAction::Cancelled
    );

    // Defaults are untouched under the shipped table.
    let defaults = manager(&[]);
    let mut list = SettingsList::new(setting_items(), 10);
    assert_eq!(
        list.handle_key_with(&defaults, bare(KeyCode::Enter)),
        SettingsAction::ValueChanged {
            id: "theme".into(),
            value: "light".into()
        }
    );
    assert_eq!(
        list.handle_key_with(&defaults, bare(KeyCode::Esc)),
        SettingsAction::Cancelled
    );
}

// ------------------------------------------------------------------ footers

#[test]
fn the_dialog_footers_name_the_effective_chords() {
    // Defaults: the old literals, with `tui.select.cancel`'s second shipped
    // chord (`ctrl+c`) now named — it always denied, the footer just did not
    // say so.
    with_table(&[], || {
        let (dialog, _rx) = confirm_dialog();
        assert_eq!(
            dialog.render_lines(60).last().unwrap(),
            "[Enter/y] accept    [n/Esc/Ctrl+C] deny"
        );
        let (dialog, _rx) = input_dialog();
        assert_eq!(
            dialog.render_lines(60).last().unwrap(),
            "[Enter] submit    [Esc/Ctrl+C] cancel"
        );
        let (dialog, _rx) = select_dialog();
        assert_eq!(
            dialog.render_lines(60).last().unwrap(),
            "[Enter] choose    [Up/Down] move    [Esc/Ctrl+C] cancel"
        );
    });
}

#[test]
fn a_rebound_table_moves_the_painted_footer_with_the_behaviour() {
    // `f2` is spelled the way `format_chord` spells every non-modifier chord
    // (`/hotkeys` renders a user-bound `f2` the same way); the modifier parts
    // are title-cased.
    with_table(
        &[
            ("tui.select.confirm", &["f2"]),
            ("tui.select.cancel", &["alt+x"]),
        ],
        || {
            let (dialog, _rx) = confirm_dialog();
            assert_eq!(
                dialog.render_lines(60).last().unwrap(),
                "[f2/y] accept    [n/Alt+X] deny"
            );
            let (dialog, _rx) = select_dialog();
            assert_eq!(
                dialog.render_lines(60).last().unwrap(),
                "[f2] choose    [Up/Down] move    [Alt+X] cancel"
            );
        },
    );

    // ... and the painted affordance is the one that answers.
    let local = manager(&[("tui.select.confirm", &["f2"])]);
    let (mut dialog, mut rx) = input_dialog();
    assert_eq!(
        dialog.handle_key_with(&local, bare(KeyCode::F(2))),
        DialogAction::Resolved(Some(UiResponse::Input {
            value: String::new()
        }))
    );
    assert_eq!(
        rx.try_recv(),
        Ok(Some(UiResponse::Input {
            value: String::new()
        }))
    );
    // The default confirm chord is *also* a chord of the input dialog
    // (`tui.input.submit`, the prompt's own id), so rebinding only
    // `tui.select.confirm` leaves it working — the dialog answers both ids,
    // exactly like upstream's `ExtensionInputComponent`, which matches
    // `tui.select.confirm` **or** a raw newline
    // (`components/extension-input.ts:75`).
    let (mut dialog, _rx) = input_dialog();
    assert_eq!(
        dialog.handle_key_with(&local, bare(KeyCode::Enter)),
        DialogAction::Resolved(Some(UiResponse::Input {
            value: String::new()
        }))
    );
    // With *both* accept chords moved, `Enter` is inert.
    let both = manager(&[
        ("tui.select.confirm", &["f2"]),
        ("tui.input.submit", &["f2"]),
    ]);
    let (mut dialog, _rx) = input_dialog();
    assert_eq!(
        dialog.handle_key_with(&both, bare(KeyCode::Enter)),
        DialogAction::None
    );
}

#[test]
fn an_unbound_action_keeps_the_affordance_without_the_chord() {
    with_table(&[("tui.select.cancel", &[])], || {
        let (dialog, _rx) = input_dialog();
        assert_eq!(
            dialog.render_lines(60).last().unwrap(),
            "[Enter] submit    cancel"
        );
    });
}

// ---------------------------------------------------------------- the App

fn faux_model() -> Model {
    Model {
        provider: ProviderId::new("faux"),
        id: "faux-model".into(),
        api: Api::Faux,
        label: Some("Faux".into()),
        context_window: 2048,
        max_output_tokens: 512,
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
            session_id: "lum1450-select-keys".into(),
            ..AppConfig::default()
        },
    )
}

#[test]
fn the_app_routes_the_rebound_chords_to_its_open_picker() {
    // The driver hands keys to `Selector::handle_key`, which reads the
    // process-wide registry — so the App-level case installs one.
    with_table(
        &[
            ("tui.select.up", &["ctrl+p"]),
            ("tui.select.down", &["ctrl+n"]),
            ("tui.select.confirm", &["f2"]),
        ],
        || {
            let mut app = app();
            app.open_selector(Selector::new("Pick", items()));

            assert_eq!(app.step(InputEvent::Key(ctrl('n'))), StepOutcome::Redraw);
            assert_eq!(app.selector().unwrap().cursor(), 1);
            // `Down` is no longer a movement chord in this table.
            assert_eq!(
                app.step(InputEvent::Key(bare(KeyCode::Down))),
                StepOutcome::Idle
            );
            assert_eq!(app.selector().unwrap().cursor(), 1);
            // `F2` is consumed by the list (a confirm, not a move): the App
            // reports the redraw and the *driver* closes the picker, exactly
            // like `Enter` (`interactive.rs::apply_selector_choice`).
            assert_eq!(
                app.step(InputEvent::Key(bare(KeyCode::F(2)))),
                StepOutcome::Redraw
            );
            assert_eq!(app.selector().unwrap().cursor(), 1);
            assert_eq!(app.selector().unwrap().selected_value(), Some("b"));
            // An unrelated key is not consumed by the list at all.
            assert_eq!(
                app.step(InputEvent::Key(bare(KeyCode::F(3)))),
                StepOutcome::Idle
            );
        },
    );
}

// ------------------------------------------------------------------- scope

#[test]
fn only_the_select_dialog_answers_the_list_chords() {
    // `Confirm` / `Input` have no `tui.select.up` target: the chords there are
    // the confirm/cancel pair, not a cursor move.
    with_table(&[], || {
        let kb = manager(&[]);
        let (mut dialog, _rx) = confirm_dialog();
        assert_eq!(dialog.select_window(), None);
        let before = dialog.render_lines(60);
        assert_eq!(
            dialog.handle_key_with(&kb, bare(KeyCode::Down)),
            DialogAction::None
        );
        assert_eq!(dialog.render_lines(60), before);

        let (mut dialog, _rx) = select_dialog();
        assert!(dialog.select_window().is_some());
        assert_eq!(
            dialog.handle_key_with(&kb, bare(KeyCode::Down)),
            DialogAction::Changed
        );
        assert_eq!(dialog.select_cursor(), 1);
    });
}
