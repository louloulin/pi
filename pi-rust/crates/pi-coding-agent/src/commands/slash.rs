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
        "compact" => SlashCommand::Compact {
            instructions: (!args.is_empty()).then(|| args.to_string()),
        },
        "exit" | "quit" => SlashCommand::Exit,
        "trust" => SlashCommand::Trust(parse_trust_decision(args)),
        "hotkeys" => SlashCommand::Hotkeys,
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
    out.push_str("  /trust    show or set project trust (/trust yes|no)\n");
    out.push_str("  /compact  summarize the conversation prefix to free context\n");
    out.push_str("  /hotkeys  list the keyboard shortcuts\n");
    out.push_str("  /exit     quit the interactive session\n");
    out.push_str("\nkeys:\n");
    out.push_str("  Enter       submit prompt\n");
    out.push_str("  Up / Down   navigate prompt history\n");
    out.push_str("  PgUp/PgDn   scroll the chat log one page\n");
    out.push_str("  Home / End  jump to the start / end of the chat log\n");
    out.push_str("  Ctrl+C      abort the current turn (or exit on idle)\n");
    out.push_str("  Ctrl+D      exit on an empty prompt\n");
    out.push_str("  Ctrl+L      clear the screen\n");
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
    ("trust", "Show or set project trust", Some("yes|no")),
    (
        "compact",
        "Manually compact the session context",
        Some("[instructions]"),
    ),
    ("hotkeys", "Show all keyboard shortcuts", None),
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

/// The `/hotkeys` overview for the process-wide (installed) keybindings.
///
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
    use super::*;

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
            "help", "clear", "new", "copy", "name", "model", "session", "export", "resume", "tree",
            "fork", "clone", "settings", "trust", "compact", "hotkeys", "exit",
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
        assert_eq!(with_hints, ["name", "model", "export", "trust", "compact"]);
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
