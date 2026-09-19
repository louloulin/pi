//! Slash command parser + dispatcher.
//!
//! Stage 4 ships the minimum command set required by the acceptance
//! criteria: `/help`, `/clear`, `/model`, `/session`, `/exit`,
//! `/resume`, plus `/trust` and `/settings`. Each is parsed into a
//! [`SlashCommand`] variant and dispatched by `interactive.rs`.

/// Slash command enum — one variant per supported slash command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlashCommand {
    /// `/help` — print the slash command help text.
    Help,
    /// `/clear` — clear the message view.
    Clear,
    /// `/model` — open the model selector.
    Model,
    /// `/session` — print session info.
    Session,
    /// `/exit` — quit the interactive session.
    Exit,
    /// `/resume` — list and pick a previous session file.
    Resume,
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
        "model" => SlashCommand::Model,
        "session" => SlashCommand::Session,
        "resume" => SlashCommand::Resume,
        "settings" => SlashCommand::Settings,
        "compact" => SlashCommand::Compact {
            instructions: (!args.is_empty()).then(|| args.to_string()),
        },
        "exit" | "quit" => SlashCommand::Exit,
        "trust" => SlashCommand::Trust(parse_trust_decision(args)),
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

/// Slash-command help text rendered by `/help` and the App's status
/// bar hint.
pub fn help_text() -> String {
    let mut out = String::new();
    out.push_str("slash commands:\n");
    out.push_str("  /help     show this help text\n");
    out.push_str("  /clear    clear the message view\n");
    out.push_str("  /model    pick a model (opens selector)\n");
    out.push_str("  /session  show the current session info\n");
    out.push_str("  /resume   resume a previous session\n");
    out.push_str("  /settings show or change interface settings\n");
    out.push_str("  /trust    show or set project trust (/trust yes|no)\n");
    out.push_str("  /compact  summarize the conversation prefix to free context\n");
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_known_commands() {
        assert_eq!(handle_command("/help").unwrap(), SlashCommand::Help);
        assert_eq!(handle_command("/clear").unwrap(), SlashCommand::Clear);
        assert_eq!(handle_command("/model").unwrap(), SlashCommand::Model);
        assert_eq!(handle_command("/session").unwrap(), SlashCommand::Session);
        assert_eq!(handle_command("/exit").unwrap(), SlashCommand::Exit);
        assert_eq!(handle_command("/quit").unwrap(), SlashCommand::Exit);
        assert_eq!(handle_command("/resume").unwrap(), SlashCommand::Resume);
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
    fn help_text_documents_scroll_keys() {
        // The fullscreen App owns the scrollback (alternate screen), so the
        // legend has to name the scroll bindings
        // (`tui.altScreen.pageUp` / `pageDown` / `top` / `bottom`).
        let text = help_text();
        assert!(text.contains("PgUp/PgDn"), "{text}");
        assert!(text.contains("Home / End"), "{text}");
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
}
