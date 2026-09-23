//! `/reload` — the config slices a running session can re-read.
//!
//! The command's contract is a pair of claims: (1) `keybindings.json` and the
//! UI slice of `settings.json` are genuinely re-read *mid-session* (editing
//! the file and reloading changes behaviour without a restart), and (2) the
//! transcript says what was **not** re-read, instead of implying the upstream
//! `handleReloadCommand` (session + extensions + skills + prompts + context
//! files) that this port does not perform.
//!
//! Kept in its own integration binary: `set_keybindings` mutates process
//! state, and cargo gives every `tests/*.rs` file its own process, so this
//! test can replace the global table without racing the other suites.
//!
//! Within this binary the tests still share that one registry, so they take
//! [`registry_guard`] for their whole body. Without it, the *first* full
//! workspace run of LUM-1312 reported `left: ["up"] right: ["ctrl+n"]` in
//! `reload_picks_up_an_edited_file_without_a_restart`: the sibling test that
//! finished first had already called `reset_keybindings()`, so the second
//! reload's freshly installed table was gone before the assertion read it.
//! Cargo's per-file process isolation does not help — the race is *inside*
//! the file.

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_coding_agent::config::ConfigSources;
use pi_coding_agent::reload;
use pi_protocol::{Api, Model, ProviderId};
use pi_tui::app::{App, AppConfig};
use pi_tui::keybindings::{get_keybindings, reset_keybindings};

const KEYBINDINGS: &str = "keybindings.json";
const SETTINGS: &str = "settings.json";

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
    App::new(&agent, AppConfig::default())
}

/// The settings pair a CLI run would use, pointed at the temp dir so the test
/// never touches the developer's real `~/.pi/agent/settings.json`.
fn sources(dir: &Path) -> ConfigSources {
    ConfigSources {
        user: Some(dir.join(SETTINGS)),
        project: None,
    }
}

fn write(dir: &Path, name: &str, body: &str) {
    std::fs::write(dir.join(name), body).expect("write config fixture");
}

/// The App's rendered transcript, joined — the transcript is only reachable
/// through a render pass.
fn transcript(app: &App) -> String {
    app.render_snapshot(80, 24).lines.join("\n")
}

/// Serialize the tests that install a table into the process-wide keybinding
/// registry (and reset it afterwards).
///
/// The registry is one global, so two tests running at the same time cannot
/// both own it. Poisoning is recovered from instead of propagated: a test
/// that panicked still leaves the registry usable for the next one, and
/// swallowing the panic would only turn one red test into several.
fn registry_guard() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[test]
fn reload_rereads_keybindings_and_ui_settings_mid_session() {
    let _guard = registry_guard();
    let dir = tempfile::tempdir().expect("temp dir");
    write(dir.path(), KEYBINDINGS, r#"{"cursorUp":["ctrl+p"]}"#);
    write(
        dir.path(),
        SETTINGS,
        r#"{"theme":"light","fullscreenCopyOnSelect":false}"#,
    );

    let mut app = app();
    // The App's built-in default before any reload: the base palette and
    // copy-on-select on (upstream `DEFAULT_FULLSCREEN_COPY_ON_SELECT`).
    assert_eq!(app.theme().name(), Some("dark"));
    assert!(app.copy_on_select());

    let report = reload(&mut app, dir.path(), &sources(dir.path()));

    // (1) Keybindings: the merged table is re-resolved from this dir and
    // re-installed, which is what the components read every chord from.
    assert_eq!(
        get_keybindings().get_keys("tui.editor.cursorUp"),
        vec!["ctrl+p".to_string()],
        "the reloaded table must reach the process-wide registry"
    );
    assert!(report.keybindings_file_exists);
    assert_eq!(report.binding_overrides, 1);

    // (2) UI settings: theme and copy-on-select land live.
    assert_eq!(app.theme().name(), Some("light"));
    assert_eq!(report.theme.as_deref(), Some("light"));
    assert!(!app.copy_on_select());
    assert!(!report.copy_on_select);

    // (3) The transcript names what changed and what did not.
    let text = transcript(&app);
    assert!(text.contains("/reload:"), "{text}");
    assert!(text.contains("1 override(s)"), "{text}");
    assert!(text.contains(KEYBINDINGS), "{text}");
    assert!(text.contains("theme → light"), "{text}");
    assert!(
        text.contains("not re-read"),
        "the scope note must survive rendering:\n{text}"
    );

    reset_keybindings();
}

#[test]
fn reload_picks_up_an_edited_file_without_a_restart() {
    let _guard = registry_guard();
    // The defect a "reload" command exists to fix: the registry holds a clone
    // of the table, so re-reading the file without re-installing is invisible.
    // Two reloads with different fixtures is the observable proof.
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().to_path_buf();
    write(&path, KEYBINDINGS, r#"{"cursorUp":["ctrl+p"]}"#);
    write(&path, SETTINGS, "{}");

    let mut app = app();
    reload(&mut app, dir.path(), &sources(dir.path()));
    assert_eq!(
        get_keybindings().get_keys("tui.editor.cursorUp"),
        vec!["ctrl+p".to_string()]
    );

    write(
        &path,
        KEYBINDINGS,
        r#"{"cursorUp":["ctrl+n"],"pageUp":["ctrl+y"]}"#,
    );
    let report = reload(&mut app, dir.path(), &sources(dir.path()));
    assert_eq!(
        get_keybindings().get_keys("tui.editor.cursorUp"),
        vec!["ctrl+n".to_string()],
        "the second reload must republish the edited table"
    );
    assert_eq!(report.binding_overrides, 2);

    reset_keybindings();
}

#[test]
fn reload_falls_back_to_defaults_and_reports_a_missing_file() {
    let _guard = registry_guard();
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().to_path_buf();
    // Only settings.json exists: the keybinding slice has to say so rather
    // than report "0 overrides from <path>" as if the file were read.
    write(
        &path,
        SETTINGS,
        r#"{"theme":"light","fullscreenCopyOnSelect":false}"#,
    );

    let mut app = app();
    let report = reload(&mut app, dir.path(), &sources(dir.path()));
    assert!(!report.keybindings_file_exists);
    assert_eq!(report.binding_overrides, 0);
    let text = transcript(&app);
    assert!(text.contains("defaults"), "{text}");
    assert!(text.contains("has no file"), "{text}");

    // A settings file that names no theme leaves the live theme alone and
    // restores the copy-on-select default — "absent key means default" is
    // exactly what a reload should land on.
    write(&path, SETTINGS, "{}");
    let report = reload(&mut app, dir.path(), &sources(dir.path()));
    assert_eq!(app.theme().name(), Some("light"), "theme left untouched");
    assert_eq!(report.theme, None);
    assert!(app.copy_on_select());
    assert!(
        transcript(&app).contains("unchanged"),
        "{}",
        transcript(&app)
    );

    reset_keybindings();
}

#[test]
fn reload_reports_a_theme_that_does_not_resolve() {
    let _guard = registry_guard();
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().to_path_buf();
    write(&path, SETTINGS, r#"{"theme":"no-such-theme"}"#);

    let mut app = app();
    let report = reload(&mut app, dir.path(), &sources(dir.path()));
    assert!(report.theme.is_none());
    assert!(
        report.theme_error.is_some(),
        "an unresolvable theme must be reported, not swallowed"
    );
    assert_eq!(app.theme().name(), Some("dark"), "the old theme is kept");
    let text = transcript(&app);
    assert!(text.contains("not applied"), "{text}");
}
