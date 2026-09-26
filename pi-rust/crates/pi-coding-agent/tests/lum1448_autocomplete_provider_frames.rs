//! LUM-1448 — extension-injected autocomplete, rendered end to end.
//!
//! The fixture is the *real* extension file
//! `crates/pi-extensions/examples/issue-autocomplete.mjs` (a port of upstream's
//! `github-issue-autocomplete.ts` example; see its header for the two
//! documented deltas), loaded into a real [`JsExtensionHost`], and driven
//! through a real [`App`] over the real composer. Nothing here is a stub of the
//! pipeline under test except the terminal: this runner has no PTY (see
//! `docs/LUM1426_POINTER_COLUMNS.md` §9), so the evidence is the *frame buffer*
//! — the same grid the driver hands to `ratatui` — plus behavioural assertions
//! on the draft, exactly like `pi-tui/tests/lum1436_autocomplete_wheel_frames.rs`.
//! The frames printed below are what `scripts/frame_to_png.py` paints into
//! `docs/screenshots/lum1448-*`:
//!
//! 1. `#29` typed → the dropdown shows the extension's `#2983` candidate with
//!    the description it built from its own issue table;
//! 2. `Tab` on that row → the draft becomes `#2983`, dropdown closed;
//! 3. `Enter` on a row walked to with `Down` (`#14` → the second candidate)
//!    → the draft becomes `#1436` and nothing is submitted.
//!
//! The base provider is the same `CombinedAutocompleteProvider` the binary
//! installs (`commands::slash::autocomplete_commands()`), so `/`-completion has
//! to keep working through the composed chain — that is asserted too.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_coding_agent::commands::slash::autocomplete_commands;
use pi_coding_agent::extensions::autocomplete::{compose_provider, SessionAutocompleteBase};
use pi_extensions::{AutocompleteBaseProvider, ExtensionEntry, HostOptions, JsExtensionHost};
use pi_protocol::{Api, ExtensionEvent, Model, ProviderId};
use pi_tui::app::{App, AppConfig};
use pi_tui::autocomplete::{AutocompleteProvider, CombinedAutocompleteProvider};
use pi_tui::input::{InputEvent, Key};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

const COLS: u16 = 78;
const ROWS: u16 = 14;

/// Two worker threads: the composer's synchronous provider bridges into the
/// extension host with `block_in_place`, which needs a multi-thread runtime
/// (the binary's own flavour — see `src/main.rs`).
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("build tokio runtime")
}

fn fixture() -> (ExtensionEntry, String) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../pi-extensions/examples/issue-autocomplete.mjs");
    let source = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    (
        ExtensionEntry {
            source: path,
            id: "issue-autocomplete".into(),
            label: None,
        },
        source,
    )
}

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

/// Build the App the binary would build, with the fixture loaded and its
/// provider chain installed on the composer.
async fn app_with_fixture() -> App {
    let cwd = std::env::temp_dir();
    let base_slot = Arc::new(SessionAutocompleteBase::new());
    let base_host: Arc<dyn AutocompleteBaseProvider> = base_slot.clone();
    let host = JsExtensionHost::with_options(
        HostOptions::default()
            .with_timeout(Duration::from_secs(30))
            .with_autocomplete_base(base_host),
    )
    .await
    .expect("extension host");
    let (entry, source) = fixture();
    host.load(entry, &source).await.expect("load fixture");
    // The loader emits this once per process, after every extension was
    // evaluated — that is when the fixture registers its provider.
    host.emit_event_with(
        &ExtensionEvent::SessionStart,
        Some("tui"),
        true,
        &cwd.display().to_string(),
    )
    .await
    .expect("session_start");

    // The chain's `current` delegate: the same provider the binary installs.
    let base: Arc<dyn AutocompleteProvider> = Arc::new(CombinedAutocompleteProvider::new(
        autocomplete_commands(),
        cwd.clone(),
    ));
    base_slot.set(base.clone());
    let trigger_characters = host.autocomplete_rebuild().await.expect("rebuild chain");
    assert_eq!(
        trigger_characters,
        vec!['#'],
        "the fixture declares exactly one trigger character"
    );
    let provider = compose_provider(host, base, trigger_characters);

    let agent = Agent::new(AgentOptions::new(
        faux_model(),
        Arc::new(FauxProvider::default()),
        "you are pi",
    ));
    let mut app = App::new(
        &agent,
        AppConfig {
            session_id: "lum1448-autocomplete-provider".into(),
            ..AppConfig::default()
        },
    );
    app.prompt_mut()
        .editor_mut()
        .set_autocomplete_provider(provider);
    app
}

/// Type one string through the App, the way a terminal delivers keystrokes.
fn type_into(app: &mut App, text: &str) {
    for ch in text.chars() {
        app.step(InputEvent::Key(Key::char(ch)));
    }
}

fn frame(app: &mut App) -> Buffer {
    let area = Rect::new(0, 0, COLS, ROWS);
    let mut buf = Buffer::empty(area);
    app.render_to_buffer(area, &mut buf);
    buf
}

/// One buffer row as plain text, wide glyphs collapsed to one cell.
fn row(buf: &Buffer, y: u16) -> String {
    let mut text = String::new();
    let mut skip = 0usize;
    for x in 0..COLS {
        let Some(cell) = buf.cell((x, y)) else {
            break;
        };
        if skip > 0 {
            skip -= 1;
            continue;
        }
        skip = pi_tui::width::columns(cell.symbol()).saturating_sub(1);
        text.push_str(cell.symbol());
    }
    text
}

fn row_with(buf: &Buffer, needle: &str) -> u16 {
    (0..ROWS)
        .find(|y| row(buf, *y).contains(needle))
        .unwrap_or_else(|| panic!("no rendered row contains {needle:?}"))
}

fn dump(app: &mut App, caption: &str) {
    let buf = frame(app);
    println!("PANEL {caption}");
    println!("FRAME DUMP cols={COLS} rows={ROWS}");
    for y in 0..ROWS {
        println!("|{}|", row(&buf, y));
    }
    println!("END FRAME DUMP");
    println!(
        "  draft={:?} dropdown_open={}",
        app.editor_text(),
        app.prompt().editor().is_showing_autocomplete()
    );
}

/// Panel 1: the `#` trigger really reaches the extension and its candidate is
/// painted with the description the extension built.
#[test]
fn frame_dump_hash_candidates_from_the_real_extension() {
    let rt = runtime();
    rt.block_on(async {
        let mut app = app_with_fixture().await;
        type_into(&mut app, "#29");

        assert!(
            app.prompt().editor().is_showing_autocomplete(),
            "typing `#29` must open the dropdown"
        );
        assert_eq!(app.prompt().editor().autocomplete_prefix(), "#29");
        assert_eq!(app.prompt().editor().autocomplete_items().len(), 1);

        let buf = frame(&mut app);
        let candidate = row(&buf, row_with(&buf, "→ #2983"));
        assert!(
            candidate.contains("[open] Extension API for autocomplete"),
            "{candidate:?}"
        );
        // The draft row still shows the typed prefix.
        let draft_row = row(&buf, ROWS - 2);
        assert!(draft_row.contains("#29"), "{draft_row:?}");

        dump(
            &mut app,
            "`#29` typed: the extension's candidate is in the dropdown",
        );
    });
}

/// Panel 2: `Tab` lands the highlighted extension candidate in the draft.
#[test]
fn tab_lands_the_extension_candidate() {
    let rt = runtime();
    rt.block_on(async {
        let mut app = app_with_fixture().await;
        type_into(&mut app, "#29");
        app.step(InputEvent::Key(Key::new(
            pi_tui::input::KeyCode::Tab,
            pi_tui::input::KeyModifiers::NONE,
        )));

        assert_eq!(app.editor_text(), "#2983");
        assert!(!app.prompt().editor().is_showing_autocomplete());

        dump(
            &mut app,
            "`Tab`: the extension candidate landed in the draft",
        );
    });
}

/// Panel 3: `Enter` on the dropdown applies the candidate instead of
/// submitting the draft.
#[test]
fn enter_lands_the_extension_candidate_without_submitting() {
    let rt = runtime();
    rt.block_on(async {
        let mut app = app_with_fixture().await;
        type_into(&mut app, "#14");
        // The numeric branch matches two issues; the first is highlighted.
        assert_eq!(app.prompt().editor().autocomplete_items().len(), 2);
        assert_eq!(app.prompt().editor().autocomplete_items()[0].value, "#1448");
        assert_eq!(app.prompt().editor().autocomplete_items()[1].value, "#1436");
        // Walk to `#1436` (`tui.select.down`) and land it with `Enter`.
        app.step(InputEvent::Key(Key::new(
            pi_tui::input::KeyCode::Down,
            pi_tui::input::KeyModifiers::NONE,
        )));
        assert_eq!(app.prompt().editor().autocomplete_selected(), 1);
        let before = app.messages().len();

        app.step(InputEvent::Key(Key::new(
            pi_tui::input::KeyCode::Enter,
            pi_tui::input::KeyModifiers::NONE,
        )));

        assert_eq!(app.editor_text(), "#1436");
        assert_eq!(
            app.messages().len(),
            before,
            "applying a candidate must not submit the prompt"
        );

        dump(
            &mut app,
            "`Enter`: the extension candidate landed, nothing submitted",
        );
    });
}

/// The composed chain must not shadow the built-in provider: `/`-command
/// completion still works with the fixture loaded.
#[test]
fn slash_commands_still_complete_through_the_composed_chain() {
    let rt = runtime();
    rt.block_on(async {
        let mut app = app_with_fixture().await;
        type_into(&mut app, "/com");
        assert!(app.prompt().editor().is_showing_autocomplete());
        let buf = frame(&mut app);
        let rendered = (0..ROWS)
            .map(|y| row(&buf, y))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("compact"), "{rendered}");
    });
}

/// A `#` token the extension has nothing for falls back to the base provider,
/// which also has no `#` branch — so the dropdown stays closed and the draft
/// is untouched.
#[test]
fn unknown_hash_token_shows_nothing() {
    let rt = runtime();
    rt.block_on(async {
        let mut app = app_with_fixture().await;
        type_into(&mut app, "#zzz");
        assert!(!app.prompt().editor().is_showing_autocomplete());
        assert_eq!(app.editor_text(), "#zzz");
    });
}
