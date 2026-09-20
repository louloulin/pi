//! Slash command parser + dispatcher.
//!
//! Stage 4 ships the minimum command set required by the acceptance
//! criteria: `/help`, `/clear`, `/model`, `/session`, `/exit`,
//! `/resume`, plus `/trust`, `/settings`, `/compact` and `/export`. Each is
//! parsed into a [`SlashCommand`] variant and dispatched by
//! `interactive.rs`.

/// Slash command enum — one variant per supported slash command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlashCommand {
    /// `/help` — print the slash command help text.
    Help,
    /// `/clear` — clear the message view.
    Clear,
    /// `/new` — start a fresh session (clears the agent state and opens a
    /// new session file).
    New,
    /// `/copy` — copy the last assistant message to the clipboard.
    Copy,
    /// `/name [name]` — show or set the session display name.
    Name {
        /// New display name. `None` (no argument) asks for the current one.
        name: Option<String>,
    },
    /// `/model` — open the model selector.
    Model,
    /// `/session` — print session info.
    Session,
    /// `/export [path]` — write the current session to a file.
    ///
    /// A path ending in `.jsonl` exports the session branch as JSONL;
    /// anything else (or no path at all) writes a self-contained HTML
    /// file. Mirrors upstream `handleExportCommand`.
    Export {
        /// Optional output path, with surrounding quotes removed.
        path: Option<String>,
    },
    /// `/exit` — quit the interactive session.
    Exit,
    /// `/resume` — list and pick a previous session file.
    Resume,
    /// `/tree` — open the session tree overlay (navigate branches and
    /// move the active leaf). Mirrors upstream `handleTreeCommand`.
    Tree,
    /// `/fork` — open the user-message picker and branch a new session
    /// from the selected message. Mirrors upstream
    /// `showUserMessageSelector`.
    Fork,
    /// `/clone` — copy the current session into a new session file
    /// (upstream `handleCloneCommand`).
    Clone,
    /// `/settings` — open the settings modal (upstream
    /// `SettingsSelectorComponent`).
    Settings,
    /// `/thinking [level]` — open the thinking-level selector, or set the
    /// level directly when an argument is given. Mirrors upstream
    /// `handleThinkingCommand` (`interactive-mode.ts:4789`).
    Thinking {
        /// The requested level, spelled as upstream does (`off` … `max`).
        /// `None` (no argument) opens the selector.
        level: Option<String>,
    },
    /// `/trust [yes|no]` — show or change the saved project-trust
    /// decision. `None` shows the current state; the decision only takes
    /// effect on the next start. Mirrors upstream `showTrustSelector`.
    Trust(Option<bool>),
    /// `/compact [instructions]` — replace the conversation prefix with a
    /// model-generated summary.
    Compact {
        /// Optional custom focus appended to the summarization prompt.
        instructions: Option<String>,
    },
    /// `/hotkeys` — list the effective keyboard shortcuts. Mirrors upstream
    /// `handleHotkeysCommand` (`interactive-mode.ts:6315`).
    Hotkeys,
    /// `/extensions` — list the loaded extensions and what they register
    /// (tools / commands / providers), plus any load failure and any tool
    /// a built-in shadowed.
    Extensions,
    /// Anything else, captured as the command name (without the slash).
    Unknown(String),
}

/// Parse a slash command. The input is the raw prompt text starting
/// with `/`. Returns an error if the input is not a slash command.
pub fn handle_command(text: &str) -> Result<SlashCommand, String> {
    let trimmed = text.trim();
    if !trimmed.starts_with('/') {
        return Err(format!("not a slash command: {text:?}"));
    }
    let rest = trimmed.trim_start_matches('/');
    let (name, args) = match rest.find(char::is_whitespace) {
        Some(idx) => (&rest[..idx], rest[idx..].trim()),
        None => (rest, ""),
    };
    let cmd = match name {
        "help" | "?" => SlashCommand::Help,
        "clear" => SlashCommand::Clear,
        "new" => SlashCommand::New,
        "copy" => SlashCommand::Copy,
        "name" => SlashCommand::Name {
            name: (!args.is_empty()).then(|| args.to_string()),
        },
        "model" => SlashCommand::Model,
        "session" => SlashCommand::Session,
        "export" => SlashCommand::Export {
            path: (!args.is_empty()).then(|| strip_quotes(args)),
        },
        "resume" => SlashCommand::Resume,
        "tree" => SlashCommand::Tree,
        "fork" => SlashCommand::Fork,
        "clone" => SlashCommand::Clone,
        "settings" => SlashCommand::Settings,
        "thinking" => SlashCommand::Thinking {
            level: (!args.is_empty()).then(|| args.to_string()),
        },
        "compact" => SlashCommand::Compact {
            instructions: (!args.is_empty()).then(|| args.to_string()),
        },
        "exit" | "quit" => SlashCommand::Exit,
        "trust" => SlashCommand::Trust(parse_trust_decision(args)),
        "hotkeys" => SlashCommand::Hotkeys,
        "extensions" => SlashCommand::Extensions,
        other => SlashCommand::Unknown(other.to_string()),
    };
    Ok(cmd)
}

/// Parse the optional `/trust` argument.
///
/// `yes` / `on` / `true` trust, `no` / `off` / `false` do not, and an
/// empty or unrecognised argument asks for the current state.
fn parse_trust_decision(args: &str) -> Option<bool> {
    match args.trim().to_ascii_lowercase().as_str() {
        "yes" | "y" | "on" | "true" | "trust" => Some(true),
        "no" | "n" | "off" | "false" | "untrust" => Some(false),
        _ => None,
    }
}

/// Strip one pair of surrounding single or double quotes from a `/export`
/// path (upstream `getPathCommandArgument` accepts `"/tmp/my file.html"`).
fn strip_quotes(args: &str) -> String {
    let trimmed = args.trim();
    let quoted = (trimmed.starts_with('"') && trimmed.ends_with('"') && trimmed.len() >= 2)
        || (trimmed.starts_with('\'') && trimmed.ends_with('\'') && trimmed.len() >= 2);
    if quoted {
        trimmed[1..trimmed.len() - 1].to_string()
    } else {
        trimmed.to_string()
    }
}

/// Slash-command help text rendered by `/help` and the App's status
/// bar hint.
pub fn help_text() -> String {
    let mut out = String::new();
    out.push_str("slash commands:\n");
    out.push_str("  /help     show this help text\n");
    out.push_str("  /clear    clear the message view\n");
    out.push_str("  /new      start a new session\n");
    out.push_str("  /copy     copy the last assistant message to the clipboard\n");
    out.push_str("  /name [name] show or set the session display name\n");
    out.push_str("  /model    pick a model (opens selector)\n");
    out.push_str("  /session  show the current session info\n");
    out.push_str("  /export [path] export the session (HTML, or JSONL for a .jsonl path)\n");
    out.push_str("  /resume   resume a previous session\n");
    out.push_str("  /tree     navigate the session tree and switch branches\n");
    out.push_str("  /fork     branch a new session from a user message\n");
    out.push_str("  /clone    copy the current session into a new session file\n");
    out.push_str("  /settings show or change interface settings\n");
    out.push_str(
        "  /thinking set the reasoning level (/thinking off|minimal|low|medium|high|xhigh|max)\n",
    );
    out.push_str("  /trust    show or set project trust (/trust yes|no)\n");
    out.push_str("  /compact  summarize the conversation prefix to free context\n");
    out.push_str("  /hotkeys  list the keyboard shortcuts\n");
    out.push_str("  /extensions list loaded extensions and what they register\n");
    out.push_str("  /exit     quit the interactive session\n");
    out.push_str("\nkeys:\n");
    out.push_str("  Enter       submit prompt\n");
    out.push_str("  Up / Down   navigate prompt history\n");
    out.push_str("  PgUp/PgDn   scroll the chat log one page\n");
    out.push_str("  Home / End  jump to the start / end of the chat log\n");
    out.push_str(
        "  Ctrl+C      abort the current turn (or clear the prompt on idle; twice exits)\n",
    );
    out.push_str("  Ctrl+D      exit on an empty prompt\n");
    out.push_str("  Ctrl+L      open the model selector\n");
    out.push_str("  Ctrl+U      clear the prompt buffer\n");
    out.push_str("  Esc         close selector / cancel turn\n");
    out
}

/// The composer's autocomplete index: one row per command this binary
/// actually implements, in the order [`help_text`] prints them.
///
/// The dropdown must list exactly the commands [`handle_command`] accepts —
/// advertising a command that does not run is worse than advertising
/// nothing. `autocomplete_commands_lists_every_implemented_command` pins
/// both ends of that contract.
pub const AUTOCOMPLETE_COMMANDS: &[(&str, &str, Option<&str>)] = &[
    ("help", "Show this help text", None),
    ("clear", "Clear the message view", None),
    ("new", "Start a new session", None),
    ("copy", "Copy last agent message to clipboard", None),
    ("name", "Set session display name", Some("<name>")),
    (
        "model",
        "Select model (opens selector UI)",
        Some("<provider/model>"),
    ),
    ("session", "Show session info and stats", None),
    (
        "export",
        "Export session (HTML default, or a .jsonl path)",
        Some("[path]"),
    ),
    ("resume", "Resume a different session", None),
    ("tree", "Navigate session tree (switch branches)", None),
    (
        "fork",
        "Create a new fork from a previous user message",
        None,
    ),
    (
        "clone",
        "Duplicate the current session at the current position",
        None,
    ),
    ("settings", "Open settings menu", None),
    (
        "thinking",
        "Set the reasoning level (opens the selector)",
        Some("[level]"),
    ),
    ("trust", "Show or set project trust", Some("yes|no")),
    (
        "compact",
        "Manually compact the session context",
        Some("[instructions]"),
    ),
    ("hotkeys", "Show all keyboard shortcuts", None),
    (
        "extensions",
        "List loaded extensions and what they register",
        None,
    ),
    ("exit", "Quit the interactive session", None),
];

/// Build the [`pi_tui::autocomplete`] command list for
/// [`AUTOCOMPLETE_COMMANDS`].
///
/// No argument completions are registered: the port has no argument
/// completer for any of these (upstream ships one only for `/model`'s
/// provider list, which the selector owns).
pub fn autocomplete_commands() -> Vec<pi_tui::autocomplete::SlashCommand> {
    AUTOCOMPLETE_COMMANDS
        .iter()
        .map(|(name, description, hint)| {
            let command =
                pi_tui::autocomplete::SlashCommand::new(*name).with_description(*description);
            match hint {
                Some(hint) => command.with_argument_hint(*hint),
                None => command,
            }
        })
        .collect()
}

/// The `/extensions` overview: every loaded source, what the extensions
/// registered, what a built-in shadowed, and what failed to load.
///
/// `home` / `cwd` are the prefixes shortened to `~` / `./` (see
/// [`display_path`]); the report itself is home-agnostic so it can be
/// built and asserted on any machine.
///
/// The load pass does not attribute a command or a provider to one source,
/// so those sections list the union across sources and the header says so.
pub fn extensions_text(
    report: &crate::extensions::wiring::ExtensionReport,
    home: Option<&std::path::Path>,
    cwd: &std::path::Path,
) -> String {
    let mut out = String::new();
    if report.disabled {
        out.push_str("extensions: none (--no-extensions)\n");
        out.push_str(
            "note: --no-extensions ignores the default search paths, --extensions-dir and -e.\n",
        );
        return out;
    }
    if report.is_empty() {
        out.push_str("extensions: none\n");
        out.push_str(
            "note: nothing was found in ~/.pi/agent/extensions, .pi/extensions, --extensions-dir or -e <path>.\n",
        );
        return out;
    }

    // One line per section: the transcript flows text (no line is kept
    // verbatim), so the labels — not indentation — are what keeps the
    // listing readable there.
    out.push_str(&format!("extensions: {} loaded", report.loaded.len()));
    if !report.errors.is_empty() {
        out.push_str(&format!(", {} failed", report.errors.len()));
    }
    if !report.shadowed.is_empty() {
        out.push_str(&format!(", {} tool(s) shadowed", report.shadowed.len()));
    }
    out.push('\n');

    if !report.loaded.is_empty() {
        let sources: Vec<String> = report
            .loaded
            .iter()
            .map(|path| display_path(path, home, cwd))
            .collect();
        out.push_str(&format!("sources: {}\n", sources.join(", ")));
    }
    out.push_str(&format!("tools: {}\n", join_names(&report.tools)));
    let commands: Vec<String> = report
        .commands
        .iter()
        .map(|command| match command.description.as_str() {
            "" => format!("/{}", command.name),
            description => format!("/{} ({description})", command.name),
        })
        .collect();
    out.push_str(&format!(
        "commands: {}\n",
        if commands.is_empty() {
            "(none)".to_string()
        } else {
            commands.join(", ")
        }
    ));
    out.push_str(&format!("providers: {}\n", join_names(&report.providers)));

    if !report.shadowed.is_empty() {
        out.push_str(&format!(
            "shadowed by a built-in tool (the built-in wins): {}\n",
            report.shadowed.join(", ")
        ));
    }
    for (path, reason) in &report.errors {
        out.push_str(&format!(
            "failed to load: {} — {reason}\n",
            display_path(path, home, cwd)
        ));
    }
    if report.loaded.len() > 1 {
        out.push_str(
            "note: tools / commands / providers are the union across the loaded sources.\n",
        );
    }
    out
}

/// One comma-joined row for a name list, `(none)` when it is empty.
fn join_names(names: &[String]) -> String {
    if names.is_empty() {
        "(none)".to_string()
    } else {
        names.join(", ")
    }
}

/// Render an extension source path the way the UI shows it: `~` for the
/// home prefix, `./` for the working directory, absolute otherwise.
///
/// Both prefixes are passed in rather than read from the environment, so the
/// shortening is testable. A one-component base (`/`, `.`) is skipped: it
/// would otherwise rewrite every absolute path.
pub fn display_path(
    path: &std::path::Path,
    home: Option<&std::path::Path>,
    cwd: &std::path::Path,
) -> String {
    let shorten = |base: &std::path::Path, prefix: &str| -> Option<String> {
        if base.components().count() <= 1 {
            return None;
        }
        path.strip_prefix(base)
            .ok()
            .map(|rest| format!("{prefix}{}", rest.display()))
    };
    if let Some(text) = home.and_then(|home| shorten(home, "~/")) {
        return text;
    }
    if let Some(text) = shorten(cwd, "./") {
        return text;
    }
    path.display().to_string()
}

/// The `/hotkeys` overview for the process-wide (installed) keybindings.
/// The interactive TTY path installs the merged coding-agent table
/// (`crate::keybindings::install_keybindings_from`), so this resolves the
/// same chords the components do — including any `keybindings.json`
/// override.
pub fn hotkeys_text() -> String {
    hotkeys_text_with(&pi_tui::keybindings::get_keybindings())
}

/// [`hotkeys_text`] against an explicit table (the injectable form used by
/// tests).
pub fn hotkeys_text_with(keybindings: &pi_tui::keybindings::KeybindingsManager) -> String {
    // Upstream groups the same way: navigation, editing, the transcript
    // viewport, the app actions and the selection lists
    // (`interactive-mode.ts:6315-6419`). A row whose id has no chord is
    // dropped — an unbound action is not a shortcut.
    const NAVIGATION: &[(&str, &str)] = &[
        ("tui.editor.cursorUp", "previous line / prompt history"),
        ("tui.editor.cursorDown", "next line / prompt history"),
        ("tui.editor.cursorLeft", "move cursor left"),
        ("tui.editor.cursorRight", "move cursor right"),
        ("tui.editor.cursorWordLeft", "move by word (left)"),
        ("tui.editor.cursorWordRight", "move by word (right)"),
        ("tui.editor.cursorLineStart", "start of line"),
        ("tui.editor.cursorLineEnd", "end of line"),
        ("tui.editor.jumpForward", "jump forward to character"),
        ("tui.editor.jumpBackward", "jump backward to character"),
        ("tui.editor.pageUp", "prompt page up"),
        ("tui.editor.pageDown", "prompt page down"),
    ];
    const EDITING: &[(&str, &str)] = &[
        ("tui.input.submit", "send message"),
        ("tui.input.newLine", "insert a newline"),
        (
            "tui.editor.deleteCharBackward",
            "delete character backwards",
        ),
        ("tui.editor.deleteCharForward", "delete character forwards"),
        ("tui.editor.deleteWordBackward", "delete word backwards"),
        ("tui.editor.deleteWordForward", "delete word forwards"),
        ("tui.editor.deleteToLineStart", "delete to start of line"),
        ("tui.editor.deleteToLineEnd", "delete to end of line"),
        ("tui.editor.yank", "paste the most-recently-deleted text"),
        ("tui.editor.yankPop", "cycle through pasted deleted text"),
        ("tui.editor.undo", "undo"),
    ];
    const TRANSCRIPT: &[(&str, &str)] = &[
        ("tui.altScreen.lineUp", "scroll up one line"),
        ("tui.altScreen.lineDown", "scroll down one line"),
        ("tui.altScreen.halfPageUp", "scroll up half a page"),
        ("tui.altScreen.halfPageDown", "scroll down half a page"),
        ("tui.altScreen.pageUp", "scroll up one page"),
        ("tui.altScreen.pageDown", "scroll down one page"),
        ("tui.altScreen.top", "jump to the start of the chat log"),
        ("tui.altScreen.bottom", "jump to the end of the chat log"),
        (
            "tui.altScreen.previousPrompt",
            "jump to the previous prompt",
        ),
        ("tui.altScreen.nextPrompt", "jump to the next prompt"),
        ("tui.altScreen.search", "search the chat log"),
        ("tui.altScreen.searchNext", "next search hit"),
        ("tui.altScreen.searchPrevious", "previous search hit"),
        ("tui.altScreen.searchClose", "close the search bar"),
    ];
    const APP: &[(&str, &str)] = &[
        ("app.interrupt", "cancel autocomplete / abort streaming"),
        ("app.clear", "clear the prompt (twice: exit)"),
        ("app.exit", "exit when the prompt is empty"),
        ("app.suspend", "suspend to the background"),
        // Stage 67 (LUM-1230) gave `app.thinking.cycle` a consumer; the row
        // has to come back with it, or the header advertises Shift+Tab while
        // `/hotkeys` stays silent (LUM-1242).
        ("app.thinking.cycle", "cycle the thinking level"),
        ("app.model.cycleForward", "cycle to the next model"),
        ("app.model.cycleBackward", "cycle to the previous model"),
        ("app.message.copy", "copy the last assistant message"),
        ("app.message.followUp", "queue a follow-up message"),
        (
            "app.message.dequeue",
            "restore queued messages to the editor",
        ),
        ("app.thinking.toggle", "show or hide thinking blocks"),
        (
            "app.tools.expand",
            "expand or collapse tool output (Ctrl+O by default)",
        ),
        (
            "app.header",
            "expand or collapse the startup header (Alt+H by default)",
        ),
        ("app.model.select", "open the model selector"),
        ("app.session.new", "start a new session"),
        ("app.session.tree", "open the session tree"),
        ("app.session.fork", "fork a session from a message"),
        ("app.session.resume", "resume a session"),
        (
            "app.clipboard.pasteImage",
            "attach a clipboard image (falls back to pasting text)",
        ),
    ];
    const SELECTORS: &[(&str, &str)] = &[
        ("tui.select.up", "move the selection up"),
        ("tui.select.down", "move the selection down"),
        ("tui.select.pageUp", "selection page up"),
        ("tui.select.pageDown", "selection page down"),
        ("tui.select.confirm", "confirm the selection"),
        ("tui.select.cancel", "close the selection list"),
        ("tui.input.tab", "path completion / accept autocomplete"),
    ];

    let mut out = String::from("keyboard shortcuts:\n");
    for (heading, rows) in [
        ("navigation", NAVIGATION),
        ("editing", EDITING),
        ("chat log", TRANSCRIPT),
        ("app", APP),
        ("selectors and completion", SELECTORS),
    ] {
        let mut section = String::new();
        for (id, label) in rows {
            let chords = keybindings
                .get_keys(id)
                .iter()
                .map(|chord| format_chord(chord))
                .collect::<Vec<_>>();
            if chords.is_empty() {
                continue;
            }
            // Bound is not the same as implemented. An `app.*` id no consumer
            // answers is not a shortcut, so it is not advertised (LUM-1240 /
            // LUM-1245). `tui.*` ids are consumed by the components, so the
            // filter is a no-op for them.
            if !pi_tui::keybindings::app_action_is_consumed(id) {
                continue;
            }
            section.push_str(&format!("  {:<16} {}\n", chords.join(" / "), label));
        }
        if !section.is_empty() {
            out.push_str(&format!("\n{heading}:\n{section}"));
        }
    }
    out.push_str("\ncommands:\n");
    out.push_str("  /               slash commands (/help, /model, /hotkeys, …)\n");
    out.push_str("  Esc             close the selector / overlay first\n");
    out
}

/// Title-case a chord id for display (`ctrl+p` → `Ctrl+P`, `pageUp` →
/// `PgUp`), matching the legend style in [`help_text`].
///
/// Duplicated on purpose from `pi_tui::locale::format_chord` (the header is
/// rendered by the App, which cannot reach into this crate);
/// `the_two_chord_formatters_agree` keeps the copies from drifting.
fn format_chord(chord: &str) -> String {
    chord
        .split('+')
        .map(|part| match part {
            "ctrl" => "Ctrl".to_string(),
            "alt" => "Alt".to_string(),
            "shift" => "Shift".to_string(),
            "super" | "meta" => "Cmd".to_string(),
            "escape" => "Esc".to_string(),
            "pageUp" => "PgUp".to_string(),
            "pageDown" => "PgDn".to_string(),
            "home" => "Home".to_string(),
            "end" => "End".to_string(),
            "tab" => "Tab".to_string(),
            "enter" => "Enter".to_string(),
            "space" => "Space".to_string(),
            "up" => "Up".to_string(),
            "down" => "Down".to_string(),
            "left" => "Left".to_string(),
            "right" => "Right".to_string(),
            other => {
                // Single letters are title-cased so `ctrl+p` reads `Ctrl+P`.
                if other.chars().count() == 1 {
                    other.to_uppercase()
                } else {
                    other.to_string()
                }
            }
        })
        .collect::<Vec<_>>()
        .join("+")
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;

    #[test]
    fn autocomplete_commands_match_the_slash_command_table() {
        // Every candidate the composer offers must actually parse, and every
        // command `/help` documents must be offered — the two lists are
        // written separately for layout reasons (see `autocomplete_commands`),
        // so this is the invariant that keeps them from drifting.
        let commands = autocomplete_commands();
        assert!(!commands.is_empty());
        for command in &commands {
            assert!(
                handle_command(&format!("/{}", command.name)).is_ok(),
                "autocomplete offers /{} but the parser does not know it",
                command.name
            );
        }
        let names: Vec<&str> = commands.iter().map(|c| c.name.as_str()).collect();
        let help = help_text();
        let documented: Vec<&str> = help
            .lines()
            .filter_map(|line| line.trim().strip_prefix('/'))
            .map(|rest| rest.split_whitespace().next().unwrap_or_default())
            .collect();
        assert!(!documented.is_empty());
        for name in documented {
            assert!(
                names.contains(&name),
                "/help documents /{name} but the autocomplete table omits it"
            );
        }
    }

    #[test]
    fn autocomplete_commands_carry_hints_and_descriptions() {
        let commands = autocomplete_commands();
        let compact = commands
            .iter()
            .find(|command| command.name == "compact")
            .expect("compact is offered");
        assert_eq!(compact.argument_hint.as_deref(), Some("[instructions]"));
        assert!(compact.description.is_some());
        let help = commands
            .iter()
            .find(|command| command.name == "help")
            .expect("help is offered");
        assert_eq!(help.argument_hint, None);
    }

    #[test]
    fn parses_known_commands() {
        assert_eq!(handle_command("/help").unwrap(), SlashCommand::Help);
        assert_eq!(handle_command("/hotkeys").unwrap(), SlashCommand::Hotkeys);
        assert_eq!(handle_command("/clear").unwrap(), SlashCommand::Clear);
        assert_eq!(handle_command("/model").unwrap(), SlashCommand::Model);
        assert_eq!(handle_command("/session").unwrap(), SlashCommand::Session);
        assert_eq!(
            handle_command("/export").unwrap(),
            SlashCommand::Export { path: None }
        );
        assert_eq!(
            handle_command("/export out.html").unwrap(),
            SlashCommand::Export {
                path: Some("out.html".into())
            }
        );
        assert_eq!(
            handle_command("/export \"/tmp/my session.html\"").unwrap(),
            SlashCommand::Export {
                path: Some("/tmp/my session.html".into())
            }
        );
        assert_eq!(
            handle_command("/export out.jsonl").unwrap(),
            SlashCommand::Export {
                path: Some("out.jsonl".into())
            }
        );
        assert_eq!(handle_command("/exit").unwrap(), SlashCommand::Exit);
        assert_eq!(handle_command("/quit").unwrap(), SlashCommand::Exit);
        assert_eq!(handle_command("/new").unwrap(), SlashCommand::New);
        assert_eq!(handle_command("/copy").unwrap(), SlashCommand::Copy);
        assert_eq!(
            handle_command("/name").unwrap(),
            SlashCommand::Name { name: None }
        );
        assert_eq!(
            handle_command("/name my session").unwrap(),
            SlashCommand::Name {
                name: Some("my session".into())
            }
        );
        assert_eq!(handle_command("/resume").unwrap(), SlashCommand::Resume);
        assert_eq!(handle_command("/tree").unwrap(), SlashCommand::Tree);
        assert_eq!(handle_command("/fork").unwrap(), SlashCommand::Fork);
        assert_eq!(handle_command("/clone").unwrap(), SlashCommand::Clone);
        assert_eq!(handle_command("/settings").unwrap(), SlashCommand::Settings);
        assert_eq!(handle_command("/trust").unwrap(), SlashCommand::Trust(None));
        assert_eq!(
            handle_command("/compact").unwrap(),
            SlashCommand::Compact { instructions: None }
        );
    }

    #[test]
    fn parses_the_trust_argument() {
        for yes in ["yes", "y", "on", "true", "trust"] {
            assert_eq!(
                handle_command(&format!("/trust {yes}")).unwrap(),
                SlashCommand::Trust(Some(true))
            );
        }
        for no in ["no", "n", "off", "false", "untrust"] {
            assert_eq!(
                handle_command(&format!("/trust {no}")).unwrap(),
                SlashCommand::Trust(Some(false))
            );
        }
        assert_eq!(
            handle_command("/trust maybe").unwrap(),
            SlashCommand::Trust(None)
        );
        assert_eq!(
            handle_command("/compact keep the API notes").unwrap(),
            SlashCommand::Compact {
                instructions: Some("keep the API notes".into())
            }
        );
    }

    #[test]
    fn help_text_documents_the_session_commands() {
        let text = help_text();
        for command in ["/new", "/copy", "/name", "/tree", "/fork", "/clone"] {
            assert!(text.contains(command), "{command} missing from:\n{text}");
        }
    }

    #[test]
    fn hotkeys_text_lists_the_session_branch_chords() {
        // Stage 65 wires `app.session.tree` / `fork` / `resume`; the app
        // group has to advertise them once the actions have consumers.
        let manager = pi_tui::keybindings::KeybindingsManager::new(
            crate::keybindings::merged_definitions(
                &crate::keybindings::Platform::Linux,
                &crate::keybindings::process_env(),
            ),
            pi_tui::keybindings::KeybindingsConfig::default(),
        );
        let text = hotkeys_text_with(&manager);
        for label in [
            "open the session tree",
            "fork a session from a message",
            "resume a session",
        ] {
            assert!(text.contains(label), "{label} missing from:\n{text}");
        }
        for chord in ["Alt+T", "Alt+F", "Alt+R"] {
            assert!(text.contains(chord), "{chord} missing from:\n{text}");
        }
    }

    #[test]
    fn hotkeys_text_lists_the_new_session_chord() {
        // `app.session.new` is a default chord as of Stage 60, so it is a
        // real shortcut and belongs in the app group (`alt+n` on Linux).
        let manager = pi_tui::keybindings::KeybindingsManager::new(
            crate::keybindings::merged_definitions(
                &crate::keybindings::Platform::Linux,
                &crate::keybindings::process_env(),
            ),
            pi_tui::keybindings::KeybindingsConfig::default(),
        );
        let text = hotkeys_text_with(&manager);
        assert!(text.contains("start a new session"), "{text}");
        assert!(text.contains("Alt+N"), "{text}");
    }

    #[test]
    fn hotkeys_text_lists_the_tool_fold_chord() {
        // Stage 58 wired `app.tools.expand` (LUM-1214); the row has to be
        // advertised now that the action has a consumer, otherwise
        // `/hotkeys` would omit the headline Ctrl+O affordance.
        let manager = pi_tui::keybindings::KeybindingsManager::new(
            crate::keybindings::merged_definitions(
                &crate::keybindings::Platform::Linux,
                &crate::keybindings::process_env(),
            ),
            pi_tui::keybindings::KeybindingsConfig::default(),
        );
        let text = hotkeys_text_with(&manager);
        assert!(text.contains("Ctrl+O"), "{text}");
        assert!(text.contains("expand or collapse tool output"), "{text}");
    }

    #[test]
    fn hotkeys_text_lists_the_startup_header_chord() {
        // Stage 66 (LUM-1228) gave `app.header` a consumer and a default
        // chord, so the app group has to list it.
        let manager = pi_tui::keybindings::KeybindingsManager::new(
            crate::keybindings::merged_definitions(
                &crate::keybindings::Platform::Linux,
                &crate::keybindings::process_env(),
            ),
            pi_tui::keybindings::KeybindingsConfig::default(),
        );
        let text = hotkeys_text_with(&manager);
        assert!(text.contains("Alt+H"), "{text}");
        assert!(
            text.contains("expand or collapse the startup header"),
            "{text}"
        );
    }

    #[test]
    fn the_two_chord_formatters_agree() {
        // `pi-tui` renders the startup header and `pi-coding-agent` renders
        // `/hotkeys`; both need `ctrl+p` → `Ctrl+P`, and the App cannot call
        // into this crate, so the mapping is duplicated. A drift here means
        // the same action is spelled two ways on two surfaces (LUM-1242).
        for chord in [
            "ctrl+p",
            "shift+ctrl+p",
            "alt+h",
            "escape",
            "ctrl+c",
            "pageUp",
            "alt+enter",
            "shift+tab",
            "ctrl+o",
            "alt+up",
        ] {
            assert_eq!(
                format_chord(chord),
                pi_tui::locale::format_chord(chord),
                "the two copies of the chord formatter disagree on {chord}"
            );
        }
    }

    #[test]
    fn help_text_documents_scroll_keys() {
        // The fullscreen App owns the scrollback (alternate screen), so the
        // legend has to name the scroll bindings
        // (`tui.altScreen.pageUp` / `pageDown` / `top` / `bottom`).
        let text = help_text();
        assert!(text.contains("PgUp/PgDn"), "{text}");
        assert!(text.contains("Home / End"), "{text}");
    }

    #[test]
    fn help_text_lists_hotkeys_command() {
        assert!(help_text().contains("/hotkeys"), "{}", help_text());
    }

    #[test]
    fn the_help_legend_describes_ctrl_l_as_the_model_selector() {
        // `/help` is the third advertisement surface (the startup header and
        // `/hotkeys` are the other two), and it kept saying `clear the screen`
        // long after the driver had claimed `app.model.select` for the model
        // selector — the chord the shipped header advertises.
        // `interactive.rs::ctrl_l_opens_the_model_selector` pins the behavior
        // this legend describes; this test pins the legend.
        let text = help_text();
        assert!(
            text.contains("Ctrl+L      open the model selector"),
            "the /help legend must describe app.model.select, not a screen wipe:\n{text}"
        );
        assert!(
            !text.contains("clear the screen"),
            "no legend row may still advertise the removed clear-the-transcript chord:\n{text}"
        );
    }

    #[test]
    fn hotkeys_text_lists_effective_chords() {
        // The coding-agent table binds `app.model.cycleForward` to ctrl+p and
        // `app.message.copy` to ctrl+x; both must render title-cased.
        let manager = pi_tui::keybindings::KeybindingsManager::new(
            crate::keybindings::merged_definitions(
                &crate::keybindings::Platform::Linux,
                &crate::keybindings::process_env(),
            ),
            pi_tui::keybindings::KeybindingsConfig::default(),
        );
        let text = hotkeys_text_with(&manager);
        assert!(text.contains("Ctrl+P"), "{text}");
        assert!(text.contains("Ctrl+X"), "{text}");
        assert!(text.contains("Ctrl+T"), "{text}");
        assert!(text.contains("navigation:"), "{text}");
        assert!(text.contains("chat log:"), "{text}");
        assert!(text.contains("copy the last assistant message"), "{text}");
        assert!(text.contains("show or hide thinking blocks"), "{text}");
    }

    #[test]
    fn hotkeys_text_skips_bound_but_unimplemented_actions() {
        // LUM-1240: four advertised chords were bound but answered by no
        // consumer. `/hotkeys` read "has a default chord" as "is
        // implemented". `app.suspend` (Ctrl+Z) and `app.editor.external` are
        // still unimplemented, so they must not be advertised; `app.model.select`
        // is implemented as of LUM-1245 and must be.
        let manager = pi_tui::keybindings::KeybindingsManager::new(
            crate::keybindings::merged_definitions(
                &crate::keybindings::Platform::Linux,
                &crate::keybindings::process_env(),
            ),
            pi_tui::keybindings::KeybindingsConfig::default(),
        );
        // Bound in the table, so the filter is what drops them.
        assert_eq!(
            manager.get_keys("app.suspend"),
            ["ctrl+z"],
            "app.suspend stays bound; only the advertisement is filtered"
        );
        let text = hotkeys_text_with(&manager);
        assert!(
            !text.contains("suspend to the background"),
            "unimplemented app.suspend advertised:\n{text}"
        );
        assert!(
            text.contains("open the model selector"),
            "app.model.select missing:\n{text}"
        );
        assert!(text.contains("Ctrl+L"), "{text}");
        // The exhaustive contract — the group's chord cells are exactly the
        // cells of the bound, consumed, non-selector-scoped `app.*` ids — lives
        // in `tests/startup_header.rs`, where it can build both sets. Here the
        // two rows that were lying are pinned directly, by their rendered cell
        // rather than by substring (`Shift+T` is a prefix of `Shift+Tab`, and
        // `ctrl+p` is bound to two ids, so neither `contains` nor a per-id
        // lookup can answer "is this row printed" — LUM-1242).
        let cell_is_advertised = |chords: &[&str]| {
            let rendered: Vec<String> = chords.iter().map(|c| format_chord(c)).collect();
            let prefix = format!("  {:<16} ", rendered.join(" / "));
            text.lines().any(|line| line.starts_with(&prefix))
        };
        assert!(
            !cell_is_advertised(&["ctrl+z"]),
            "app.suspend is advertised on /hotkeys but has no consumer:\n{text}"
        );
        assert!(cell_is_advertised(&["ctrl+l"]), "{text}");
    }

    #[test]
    fn hotkeys_text_skips_unbound_rows() {
        // The plain `pi-tui` table leaves `app.*` unbound: those rows must
        // not be advertised.
        let manager = pi_tui::keybindings::KeybindingsManager::tui_defaults();
        let text = hotkeys_text_with(&manager);
        assert!(!text.contains("cycle to the next model"), "{text}");
        assert!(text.contains("send message"), "{text}");
    }

    #[test]
    fn captures_unknown_command_name() {
        assert_eq!(
            handle_command("/foo").unwrap(),
            SlashCommand::Unknown("foo".into())
        );
    }

    #[test]
    fn parses_the_extensions_command() {
        assert_eq!(
            handle_command("/extensions").unwrap(),
            SlashCommand::Extensions
        );
        // Arguments are tolerated like every other no-argument command.
        assert_eq!(
            handle_command("/extensions now").unwrap(),
            SlashCommand::Extensions
        );
    }

    #[test]
    fn help_text_lists_the_extensions_command() {
        assert!(help_text().contains("/extensions"), "{}", help_text());
    }

    /// A report with one extension of each interesting shape.
    fn full_report() -> crate::extensions::wiring::ExtensionReport {
        crate::extensions::wiring::ExtensionReport {
            loaded: vec![
                PathBuf::from("/home/dev/.pi/agent/extensions/foo.mjs"),
                PathBuf::from("/work/fixture-ext.mjs"),
            ],
            tools: vec!["echo".into(), "greet".into()],
            shadowed: vec!["read".into()],
            commands: vec![pi_extensions::RegisteredCommand {
                name: "ext-echo".into(),
                description: "Echo something".into(),
            }],
            providers: vec!["acme".into()],
            errors: vec![(PathBuf::from("/work/broken.mjs"), "boom".into())],
            disabled: false,
        }
    }

    #[test]
    fn extensions_text_lists_sources_registrations_and_failures() {
        let text = extensions_text(
            &full_report(),
            Some(Path::new("/home/dev")),
            Path::new("/work"),
        );

        // Counts in the header, then one labelled line per section.
        assert!(
            text.contains("extensions: 2 loaded, 1 failed, 1 tool(s) shadowed"),
            "{text}"
        );
        assert!(
            text.contains("sources: ~/.pi/agent/extensions/foo.mjs, ./fixture-ext.mjs"),
            "{text}"
        );
        assert!(text.contains("tools: echo, greet"), "{text}");
        assert!(
            text.contains("commands: /ext-echo (Echo something)"),
            "{text}"
        );
        assert!(text.contains("providers: acme"), "{text}");
        assert!(
            text.contains("shadowed by a built-in tool (the built-in wins): read"),
            "{text}"
        );
        assert!(
            text.contains("failed to load: ./broken.mjs — boom"),
            "{text}"
        );
        assert!(
            text.contains("note: tools / commands / providers are the union"),
            "{text}"
        );
        // A single source needs no union disclaimer.
        let one = crate::extensions::wiring::ExtensionReport {
            loaded: vec![PathBuf::from("/work/only.mjs")],
            ..crate::extensions::wiring::ExtensionReport::default()
        };
        assert!(
            !extensions_text(&one, None, Path::new("/work")).contains("note:"),
            "{text}"
        );
        // Empty sections still say `(none)` instead of dropping the row.
        let quiet = crate::extensions::wiring::ExtensionReport {
            errors: vec![(PathBuf::from("/work/bad.mjs"), "boom".into())],
            ..crate::extensions::wiring::ExtensionReport::default()
        };
        let quiet = extensions_text(&quiet, None, Path::new("/work"));
        assert!(quiet.contains("tools: (none)"), "{quiet}");
        assert!(quiet.contains("commands: (none)"), "{quiet}");
        assert!(quiet.contains("providers: (none)"), "{quiet}");
        assert!(quiet.contains("extensions: 0 loaded, 1 failed"), "{quiet}");
    }

    #[test]
    fn extensions_text_without_extensions_says_none() {
        let text = extensions_text(
            &crate::extensions::wiring::ExtensionReport::default(),
            None,
            Path::new("/work"),
        );
        assert!(text.starts_with("extensions: none\n"), "{text}");
        assert!(!text.contains("sources:"), "{text}");
    }

    #[test]
    fn extensions_text_reports_the_no_extensions_flag() {
        let report = crate::extensions::wiring::ExtensionReport {
            disabled: true,
            ..crate::extensions::wiring::ExtensionReport::default()
        };
        let text = extensions_text(&report, None, Path::new("/work"));
        assert!(
            text.starts_with("extensions: none (--no-extensions)"),
            "{text}"
        );
    }

    #[test]
    fn display_path_shortens_home_then_cwd_and_keeps_the_rest_absolute() {
        let home = Path::new("/home/dev");
        let cwd = Path::new("/work");
        assert_eq!(
            display_path(Path::new("/home/dev/.pi/x.mjs"), Some(home), cwd),
            "~/.pi/x.mjs"
        );
        assert_eq!(
            display_path(Path::new("/work/ext.mjs"), Some(home), cwd),
            "./ext.mjs"
        );
        assert_eq!(
            display_path(Path::new("/opt/ext.mjs"), Some(home), cwd),
            "/opt/ext.mjs"
        );
        // A one-component base must not rewrite every absolute path.
        assert_eq!(
            display_path(
                Path::new("/opt/ext.mjs"),
                Some(Path::new("/")),
                Path::new(".")
            ),
            "/opt/ext.mjs"
        );
    }

    #[test]
    fn ignores_arguments() {
        // The parser tolerates trailing arguments without erroring.
        assert_eq!(
            handle_command("/model faux/faux-model").unwrap(),
            SlashCommand::Model
        );
    }

    #[test]
    fn rejects_non_slash_input() {
        assert!(handle_command("hello").is_err());
    }

    // -----------------------------------------------------------------------
    // Composer autocomplete index (LUM-1236)
    // -----------------------------------------------------------------------

    /// Every command the parser accepts must be offered, and nothing that
    /// does not run may be. The dropdown is a contract with
    /// [`handle_command`], not a wish list.
    #[test]
    fn autocomplete_commands_match_the_parser_exactly() {
        const IMPLEMENTED: &[&str] = &[
            "help",
            "clear",
            "new",
            "copy",
            "name",
            "model",
            "session",
            "export",
            "resume",
            "tree",
            "fork",
            "clone",
            "settings",
            "thinking",
            "trust",
            "compact",
            "hotkeys",
            "extensions",
            "exit",
        ];
        let names: Vec<String> = autocomplete_commands()
            .into_iter()
            .map(|command| command.name)
            .collect();
        assert_eq!(names, IMPLEMENTED);
        for name in IMPLEMENTED {
            let parsed = handle_command(&format!("/{name}"))
                .unwrap_or_else(|error| panic!("/{name} is offered but rejected: {error}"));
            assert!(
                !matches!(parsed, SlashCommand::Unknown(_)),
                "/{name} is offered but the parser does not know it"
            );
        }
    }

    #[test]
    fn every_offered_command_carries_a_description() {
        for command in autocomplete_commands() {
            let description = command.description.unwrap_or_default();
            assert!(
                !description.is_empty(),
                "/{} would show a blank row in the dropdown",
                command.name
            );
        }
        let with_hints: Vec<String> = autocomplete_commands()
            .into_iter()
            .filter(|command| command.argument_hint.is_some())
            .map(|command| command.name)
            .collect();
        assert_eq!(
            with_hints,
            ["name", "model", "export", "thinking", "trust", "compact"]
        );
    }

    /// The installed list has to produce candidates through the same trait
    /// the editor drives — a table nobody can complete from is still a
    /// dead feature.
    #[test]
    fn the_offered_list_completes_a_command_prefix() {
        use pi_tui::autocomplete::{AutocompleteProvider, CombinedAutocompleteProvider};

        let provider = CombinedAutocompleteProvider::new(autocomplete_commands(), ".");
        let lines = ["/com".to_string()];
        let suggestions = provider
            .get_suggestions(&lines, 0, 4, false)
            .expect("a candidate for /com");
        let values: Vec<&str> = suggestions
            .items
            .iter()
            .map(|item| item.value.as_str())
            .collect();
        // `compact` matches the name directly and wins; a description-only
        // hit (`fork`: "…a new fork from a previous user message") is still
        // surfaced, which is the port's documented superset over upstream's
        // name-only filter.
        assert_eq!(values.first(), Some(&"compact"));
        assert!(values.contains(&"copy"), "{values:?}");

        // A description-only hit still resolves to the command name that
        // gets typed into the buffer.
        let lines = ["/clip".to_string()];
        let suggestions = provider
            .get_suggestions(&lines, 0, 5, false)
            .expect("a candidate for /clip");
        let copy = suggestions
            .items
            .iter()
            .find(|item| item.value == "copy")
            .expect("the copy candidate");
        let applied = provider.apply_completion(&lines, 0, 5, copy, &suggestions.prefix);
        assert_eq!(applied.lines, ["/copy ".to_string()]);
    }
}
