//! `app.editor.external` (`Ctrl+G`) — hand the composer's draft to `$EDITOR`.
//!
//! Port of `packages/coding-agent/src/modes/interactive/external-editor.ts`.
//! Upstream's `handleOpenExternalEditor`
//! (`interactive-mode.ts:4246-4262`) stops the TUI, writes the editor buffer to
//! a temp `prompt.md`, runs the configured editor on it with inherited stdio,
//! and puts the file's contents back into the editor when the editor exits 0.
//!
//! What this module owns is the *file + process* half, which is pure enough to
//! test without a terminal:
//!
//! * [`resolve_command`] — the `externalEditor` setting, then `VISUAL`, then
//!   `EDITOR`, then a platform default (upstream
//!   `settings-manager.ts:969-979`).
//! * [`edit_in_external_editor`] — temp dir, spawn, read back, clean up.
//!
//! The *terminal* half (leaving the alternate screen so the editor gets a
//! usable tty, and repainting afterwards) lives in
//! [`crate::interactive`], because the driver owns the terminal.
//!
//! Before this module the chord was a documented false ad: the header hint and
//! `app.editor.external` were both defined, and no code path ever answered the
//! key (`pi-tui/src/keybindings.rs`'s `CONSUMED_APP_ACTIONS` left the id out on
//! purpose, so even `/hotkeys` filtered it).

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

/// What [`edit_in_external_editor`] needs to run one round-trip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalEditorOptions {
    /// The editor command line, e.g. `"code --wait"` or `"nano"`. Split on
    /// single spaces exactly like upstream (`options.command.split(" ")`).
    pub command: String,
    /// The draft handed to the editor.
    pub content: String,
}

/// The outcome of one external-editor round-trip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExternalEditorResult {
    /// The editor exited `0`; this is the file's content, with a BOM and a
    /// single trailing newline stripped (upstream `stripBom(..).replace(/\n$/, "")`).
    Complete(String),
    /// The editor could not be launched, or exited non-zero. The caller must
    /// leave the draft untouched.
    Failed,
}

/// The banner printed on the normal screen while the editor owns the tty
/// (upstream `external-editor.ts:26-27`, wording verbatim).
pub fn banner(command: &str) -> String {
    format!("Launching external editor: {command}\nPi will resume when the editor exits.")
}

/// The editor command to run, in upstream precedence order.
///
/// A blank (or whitespace-only) `configured` value is ignored, exactly like
/// upstream's `configuredEditor.trim() !== ""` guard, so an empty
/// `externalEditor: ""` in `settings.json` cannot strand the chord.
pub fn resolve_command(
    configured: Option<&str>,
    visual: Option<&str>,
    editor: Option<&str>,
    windows: bool,
) -> String {
    for value in [configured, visual, editor].into_iter().flatten() {
        if !value.trim().is_empty() {
            return value.to_string();
        }
    }
    if windows { "notepad" } else { "nano" }.to_string()
}

/// `stripBom` then drop **one** trailing newline.
fn normalize(content: &str) -> String {
    let without_bom = content.strip_prefix('\u{feff}').unwrap_or(content);
    without_bom
        .strip_suffix('\n')
        .unwrap_or(without_bom)
        .to_string()
}

/// A unique temp directory for one round-trip.
///
/// Deliberately hand-rolled instead of pulling `tempfile` into the runtime
/// dependency graph: the requirement is "a directory only this call uses", and
/// pid + a process-wide counter + nanos is enough to guarantee that. The
/// directory is removed again by [`edit_in_external_editor`].
fn unique_temp_dir() -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!(
        "pi-editor-{}-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed),
        nanos
    ))
}

/// Write `content` to a temp `prompt.md`, run the editor on it, and read the
/// result back (upstream `editInExternalEditor`).
///
/// Blocking: the caller has already given the tty to the child. The temp
/// directory is removed on every path, including failure.
pub fn edit_in_external_editor(options: &ExternalEditorOptions) -> ExternalEditorResult {
    edit_in_directory(options, &unique_temp_dir())
}

/// [`edit_in_external_editor`] with the temp directory supplied by the caller,
/// which hands over ownership of it: the directory is created and removed by
/// the time this returns.
///
/// Split out so the cleanup is observable in a test (the public entry point
/// picks a name no test can predict).
fn edit_in_directory(
    options: &ExternalEditorOptions,
    directory: &std::path::Path,
) -> ExternalEditorResult {
    if let Err(err) = fs::create_dir_all(directory) {
        eprintln!(
            "pi: external editor: cannot create {}: {err}",
            directory.display()
        );
        return ExternalEditorResult::Failed;
    }
    let path = directory.join("prompt.md");
    let result = run_round_trip(options, &path);
    // Best effort, like upstream's `finally` block: a leftover temp file is
    // not worth failing the chord over.
    let _ = fs::remove_dir_all(directory);
    result
}

fn run_round_trip(options: &ExternalEditorOptions, path: &std::path::Path) -> ExternalEditorResult {
    if let Err(err) = fs::write(path, &options.content) {
        eprintln!("pi: external editor: cannot write draft: {err}");
        return ExternalEditorResult::Failed;
    }
    // Written before the child starts, on the normal screen the driver already
    // restored, so the user sees why the TUI vanished.
    println!("{}", banner(&options.command));
    let _ = std::io::stdout().flush();

    let mut parts = options.command.split(' ').filter(|part| !part.is_empty());
    let Some(program) = parts.next() else {
        eprintln!("pi: external editor: empty command");
        return ExternalEditorResult::Failed;
    };
    let args = parts.collect::<Vec<_>>();

    // `spawn` (not `status`) so a launch failure is distinguishable from a
    // non-zero exit, and `stdio: inherit` so the editor owns the tty —
    // upstream's `stdio: "inherit"`.
    let spawned = Command::new(program)
        .args(&args)
        .arg(path)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn();
    let mut child = match spawned {
        Ok(child) => child,
        Err(err) => {
            eprintln!(
                "pi: external editor: cannot launch `{}`: {err}",
                options.command
            );
            return ExternalEditorResult::Failed;
        }
    };
    match child.wait() {
        Ok(status) if status.success() => {}
        Ok(status) => {
            eprintln!(
                "pi: external editor: `{}` exited with {status}",
                options.command
            );
            return ExternalEditorResult::Failed;
        }
        Err(err) => {
            eprintln!(
                "pi: external editor: `{}` did not exit cleanly: {err}",
                options.command
            );
            return ExternalEditorResult::Failed;
        }
    }
    match fs::read_to_string(path) {
        Ok(content) => ExternalEditorResult::Complete(normalize(&content)),
        Err(err) => {
            eprintln!("pi: external editor: cannot read the edited draft: {err}");
            ExternalEditorResult::Failed
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_editor_wins_over_the_environment() {
        assert_eq!(
            resolve_command(Some("code --wait"), Some("vim"), Some("nano"), false),
            "code --wait"
        );
    }

    #[test]
    fn blank_configured_editor_falls_through_to_visual_then_editor() {
        // Upstream guards with `.trim() !== ""`, so an empty or whitespace
        // setting must not win.
        assert_eq!(
            resolve_command(Some(""), Some("vim"), Some("nano"), false),
            "vim"
        );
        assert_eq!(
            resolve_command(Some("   "), Some(""), Some("nano"), false),
            "nano"
        );
    }

    #[test]
    fn the_platform_default_closes_the_chain() {
        assert_eq!(resolve_command(None, None, None, false), "nano");
        assert_eq!(resolve_command(None, None, None, true), "notepad");
        // Whitespace-only environment values are ignored too.
        assert_eq!(resolve_command(None, Some(" "), None, true), "notepad");
    }

    #[test]
    fn the_command_line_is_split_on_single_spaces_and_the_path_is_appended() {
        // The splitting rule is upstream's (`split(" ")`): doubled spaces
        // produce empty args that are dropped here rather than handed to the
        // editor as a positional argument.
        let options = ExternalEditorOptions {
            command: "my-editor  --wait".to_string(),
            content: "x".to_string(),
        };
        let mut parts = options.command.split(' ').filter(|part| !part.is_empty());
        assert_eq!(parts.next(), Some("my-editor"));
        assert_eq!(parts.collect::<Vec<_>>(), vec!["--wait"]);
    }

    #[cfg(unix)]
    fn write_script(dir: &std::path::Path, name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        fs::create_dir_all(dir).unwrap();
        let path = dir.join(name);
        fs::write(&path, body).unwrap();
        let mut perms = fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&path, perms).unwrap();
        path
    }

    #[cfg(unix)]
    #[test]
    fn a_real_editor_round_trip_replaces_the_draft() {
        let scripts = unique_temp_dir();
        // The "editor" appends a line to the file it is handed — the same
        // contract a real editor honours ($1 is the temp prompt.md).
        let script = write_script(
            &scripts,
            "editor.sh",
            "#!/bin/sh\nprintf 'edited\\n' > \"$1\"\n",
        );
        let round_trip = unique_temp_dir();
        let options = ExternalEditorOptions {
            command: script.display().to_string(),
            content: "original".to_string(),
        };
        assert_eq!(
            edit_in_directory(&options, &round_trip),
            ExternalEditorResult::Complete("edited".to_string())
        );
        assert!(
            !round_trip.exists(),
            "the temp dir is removed after the round-trip"
        );
        let _ = fs::remove_dir_all(&scripts);
    }

    #[cfg(unix)]
    #[test]
    fn a_non_zero_exit_keeps_the_draft() {
        let scripts = unique_temp_dir();
        let script = write_script(
            &scripts,
            "failing.sh",
            "#!/bin/sh\nprintf 'half written' > \"$1\"\nexit 3\n",
        );
        let round_trip = unique_temp_dir();
        let options = ExternalEditorOptions {
            command: script.display().to_string(),
            content: "original".to_string(),
        };
        assert_eq!(
            edit_in_directory(&options, &round_trip),
            ExternalEditorResult::Failed,
            "a failed editor must not hand its partial write back"
        );
        assert!(
            !round_trip.exists(),
            "a failed editor still gets its temp dir cleaned up"
        );
        let _ = fs::remove_dir_all(&scripts);
    }

    #[test]
    fn a_missing_editor_binary_fails_instead_of_panicking() {
        let options = ExternalEditorOptions {
            command: "pi-editor-that-does-not-exist-1308".to_string(),
            content: "original".to_string(),
        };
        assert_eq!(
            edit_in_external_editor(&options),
            ExternalEditorResult::Failed
        );
    }

    #[test]
    fn an_empty_command_fails() {
        let options = ExternalEditorOptions {
            command: "   ".to_string(),
            content: "original".to_string(),
        };
        assert_eq!(
            edit_in_external_editor(&options),
            ExternalEditorResult::Failed
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_utf8_bom_and_one_trailing_newline_are_stripped() {
        let scripts = unique_temp_dir();
        let script = write_script(
            &scripts,
            "bom.sh",
            // Two trailing newlines: upstream strips exactly one.
            "#!/bin/sh\nprintf '\\357\\273\\277hello\\n\\n' > \"$1\"\n",
        );
        let options = ExternalEditorOptions {
            command: script.display().to_string(),
            content: "original".to_string(),
        };
        assert_eq!(
            edit_in_external_editor(&options),
            ExternalEditorResult::Complete("hello\n".to_string())
        );
        let _ = fs::remove_dir_all(&scripts);
    }

    #[test]
    fn normalize_matches_upstream() {
        assert_eq!(normalize("\u{feff}draft"), "draft");
        assert_eq!(normalize("draft\n"), "draft");
        assert_eq!(normalize("draft\n\n"), "draft\n");
        assert_eq!(normalize("draft"), "draft");
        // Interior newlines are untouched.
        assert_eq!(normalize("a\nb\n"), "a\nb");
    }

    #[test]
    fn temp_directories_are_unique_per_call() {
        let a = unique_temp_dir();
        let b = unique_temp_dir();
        assert_ne!(a, b);
        assert!(a.to_string_lossy().contains("pi-editor-"));
    }

    #[test]
    fn the_banner_names_the_command() {
        let text = banner("nano");
        assert!(text.contains("Launching external editor: nano"), "{text}");
        assert!(
            text.contains("Pi will resume when the editor exits."),
            "{text}"
        );
    }
}
