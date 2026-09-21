//! `/reload` — re-read the on-disk configuration the *running* TUI consumes.
//!
//! Upstream `handleReloadCommand`
//! (`packages/coding-agent/src/modes/interactive/interactive-mode.ts:5972`)
//! rebuilds the session, keybindings, extensions, skills, prompts, themes and
//! context files in place. This port re-reads the two slices the running
//! [`App`] actually owns:
//!
//! | slice | source | effect |
//! |---|---|---|
//! | keybindings | `<agent dir>/keybindings.json` | the merged table is re-resolved and re-installed into the process-wide `pi-tui` registry every component resolves chords against |
//! | UI settings | `settings.json` (`theme`, `fullscreenCopyOnSelect`) | live palette / copy-on-select |
//!
//! Extensions, skills, prompts and context files are **not** re-read here:
//! the extension host owns a QuickJS runtime built once for the session, and
//! the resource layer feeds the system prompt of the next turn. Restarting
//! those means starting a new session. [`ReloadReport`] carries exactly what
//! was re-read and [`reload`] prints that, so the transcript never claims an
//! upstream-equivalent reload this build does not perform.
//!
//! The wiring gap this closes is named in `interactive::run_interactive`:
//! "the render loop has no reload trigger yet", while
//! [`reload_keybindings`](crate::keybindings::reload_keybindings) already
//! existed as the pair of calls the reload has to make.

use std::path::{Path, PathBuf};

use pi_tui::app::App;

use crate::config::{self, ConfigSources};
use crate::keybindings::{reload_keybindings, KeybindingsManager};

/// What [`reload`] re-read, and what it deliberately left alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReloadReport {
    /// The `keybindings.json` the manager resolves against. Always set:
    /// the config layer always knows the agent-dir path it *would* read,
    /// whether or not the file exists.
    pub keybindings_path: PathBuf,
    /// Whether that file exists — an absent file keeps the defaults.
    pub keybindings_file_exists: bool,
    /// Override **entries** read from the file, in declaration order.
    /// Entries naming no known keybinding id are counted here and dropped
    /// by the manager's rebuild, exactly as at startup.
    pub binding_overrides: usize,
    /// Theme applied from `settings.json`, when that file names one.
    pub theme: Option<String>,
    /// Theme named by `settings.json` that could not be resolved, with the
    /// resolver's message.
    pub theme_error: Option<String>,
    /// Copy-on-select applied from `settings.json`.
    pub copy_on_select: bool,
}

/// Re-read `keybindings.json` and `settings.json` and apply them to the live
/// [`App`], printing one honest summary line (plus a scope note) into the
/// transcript.
///
/// `agent_dir` is the directory holding `keybindings.json`
/// ([`crate::paths::agent_dir_or_default`] in the TUI path — passed in so
/// tests can point at a temp dir); `sources` is the settings pair
/// (`interactive::settings_sources`).
pub fn reload(app: &mut App, agent_dir: &Path, sources: &ConfigSources) -> ReloadReport {
    // Keybindings: rebuild the merged table from the same agent dir the
    // startup path used, then re-install it. `reload_keybindings` is the
    // pair (`reload()` + `install`) that comment points at: the registry
    // holds a *clone* of the table, so re-reading the file without
    // re-installing would be invisible to the components.
    let mut manager = KeybindingsManager::create(agent_dir);
    let keybindings_path = manager
        .config_path()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| agent_dir.join(crate::keybindings::KEYBINDINGS_FILE_NAME));
    let keybindings_file_exists = keybindings_path.exists();
    reload_keybindings(&mut manager);

    // UI settings: the two keys `/settings` writes and the running App
    // consumes. Both are applied unconditionally — a file that omits a key
    // means "the default", which is exactly what a reload should land on.
    let ui = config::load_ui_settings(sources);
    let mut theme = None;
    let mut theme_error = None;
    if let Some(name) = ui.theme {
        match app.set_theme_by_name(&name) {
            Ok(()) => theme = Some(name),
            Err(err) => theme_error = Some(err.to_string()),
        }
    }
    app.set_copy_on_select(ui.fullscreen_copy_on_select);

    let report = ReloadReport {
        keybindings_path,
        keybindings_file_exists,
        binding_overrides: manager.get_user_bindings().len(),
        theme,
        theme_error,
        copy_on_select: ui.fullscreen_copy_on_select,
    };
    for line in summary_lines(&report) {
        app.info(line);
    }
    report
}

/// The transcript lines `/reload` prints.
///
/// Split out from [`reload`] so the wording can be asserted without an App:
/// the first line states what changed, the second states what this build does
/// *not* re-read. Advertising a reload the port does not perform would be the
/// same class of defect as advertising a keybinding with no consumer.
pub fn summary_lines(report: &ReloadReport) -> Vec<String> {
    let bindings = if report.keybindings_file_exists {
        format!(
            "keybindings → {} override(s) from {}",
            report.binding_overrides,
            report.keybindings_path.display()
        )
    } else {
        format!(
            "keybindings → defaults ({} has no file)",
            report.keybindings_path.display()
        )
    };
    let theme = match (&report.theme, &report.theme_error) {
        (Some(name), _) => format!("theme → {name}"),
        (None, Some(err)) => format!("theme → not applied: {err}"),
        (None, None) => "theme → unchanged (settings.json names none)".to_string(),
    };
    vec![
        format!("/reload: {bindings}; {theme}"),
        "/reload: extensions, skills, prompts and context files are read at startup and are not re-read here (use /new for a fresh session)".to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report() -> ReloadReport {
        ReloadReport {
            keybindings_path: PathBuf::from("/tmp/agent/keybindings.json"),
            keybindings_file_exists: true,
            binding_overrides: 2,
            theme: Some("light".into()),
            theme_error: None,
            copy_on_select: false,
        }
    }

    #[test]
    fn summary_names_the_file_and_the_override_count() {
        let lines = summary_lines(&report());
        assert!(lines[0].starts_with("/reload: "), "{}", lines[0]);
        assert!(lines[0].contains("2 override(s)"), "{}", lines[0]);
        assert!(
            lines[0].contains("/tmp/agent/keybindings.json"),
            "{}",
            lines[0]
        );
        assert!(lines[0].contains("theme → light"), "{}", lines[0]);
    }

    #[test]
    fn summary_says_so_when_there_is_no_keybindings_file() {
        let mut report = report();
        report.keybindings_file_exists = false;
        report.binding_overrides = 0;
        let lines = summary_lines(&report);
        assert!(lines[0].contains("defaults"), "{}", lines[0]);
        assert!(!lines[0].contains("override(s)"), "{}", lines[0]);
    }

    #[test]
    fn summary_reports_a_theme_that_failed_to_resolve() {
        let mut report = report();
        report.theme = None;
        report.theme_error = Some("no theme named \"neon\"".into());
        let lines = summary_lines(&report);
        assert!(lines[0].contains("not applied"), "{}", lines[0]);
        assert!(lines[0].contains("neon"), "{}", lines[0]);
    }

    #[test]
    fn summary_stays_honest_about_what_is_not_reloaded() {
        // The scope note is the contract: `/reload` must not read as a full
        // upstream reload (session + extensions + skills + prompts).
        let lines = summary_lines(&report());
        let note = &lines[1];
        for scope in ["extensions", "skills", "prompts", "context files"] {
            assert!(note.contains(scope), "{scope} missing from {note:?}");
        }
        assert!(note.contains("not re-read"), "{note:?}");
    }
}
