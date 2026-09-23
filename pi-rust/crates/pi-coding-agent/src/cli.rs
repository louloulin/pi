//! CLI flag parser — mirrors the `pi` CLI surface from
//! `packages/coding-agent/src/cli.ts`.

use clap::{Parser, Subcommand};

use crate::print_mode::OutputFormat;

/// Top-level CLI parser.
#[derive(Debug, Parser)]
#[command(name = "pi", about = "Rust port of the Pi coding agent")]
pub struct Cli {
    /// Subcommand to run. Defaults to interactive mode when omitted.
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Path to a single extension file (or directory) to load for this
    /// run only (matches `pi -e <path>` in the TS CLI). Repeatable.
    #[arg(short = 'e', long = "extension", value_name = "PATH")]
    pub extension: Vec<std::path::PathBuf>,

    /// Extra directory to scan for JS / TypeScript extensions. The
    /// default search paths (`~/.pi/agent/extensions/` and
    /// `.pi/extensions/`) are always searched unless `--no-extensions`
    /// is set. Repeatable.
    #[arg(long = "extensions-dir", value_name = "DIR")]
    pub extensions_dir: Vec<std::path::PathBuf>,

    /// Disable extension loading entirely: the default search paths,
    /// `-e` and `--extensions-dir` are all ignored, so the agent ships
    /// only the built-in tool bundle.
    #[arg(long = "no-extensions", conflicts_with_all = ["extension", "extensions_dir"])]
    pub no_extensions: bool,

    /// Print events to stdout as JSON instead of rendering the TUI.
    ///
    /// Accepts an optional inline prompt: `pi --print "hello"` runs a
    /// single turn with the given prompt; `pi --print` (no argument)
    /// reads the prompt from stdin (when piped) or fails with
    /// `EX_USAGE` (64).
    #[arg(long, value_name = "PROMPT", num_args = 0..=1, default_missing_value = "")]
    pub print: Option<String>,

    /// RPC mode — stream JSON events on stdout and read JSON-RPC requests
    /// on stdin (matches `pi --rpc` in the TS CLI).
    #[arg(long)]
    pub rpc: bool,

    /// Delete the cross-session prompt history (`~/.pi/agent/history.jsonl`)
    /// and exit.
    ///
    /// The explicit cleanup entry point for the composer history: `/clear`
    /// leaves the file alone (upstream parity) and the TUI never deletes it as
    /// a side effect of a keystroke, so clearing it is a deliberate command.
    /// Prints the path it cleared.
    #[arg(long)]
    pub clear_history: bool,

    /// Default model — accepts `<provider>/<model-id>` (e.g.
    /// `faux/faux-model`). Mirrors `pi --model` in the TS CLI.
    #[arg(long, value_name = "PROVIDER/MODEL")]
    pub model: Option<String>,

    /// Append text to the system prompt. Mirrors `pi --append-system-prompt`.
    #[arg(long, value_name = "TEXT")]
    pub append_system_prompt: Vec<String>,

    /// Load a skill file or directory for this run only. The default
    /// locations (`~/.pi/agent/skills` and `.pi/skills`) are always
    /// searched unless `--no-skills` is set. Repeatable. Mirrors
    /// `pi --skill`.
    #[arg(long = "skill", value_name = "PATH")]
    pub skill: Vec<std::path::PathBuf>,

    /// Disable skill discovery and loading (`--no-skills`, `-ns` in the
    /// TS CLI).
    #[arg(long = "no-skills")]
    pub no_skills: bool,

    /// Disable `AGENTS.md` / `CLAUDE.md` discovery and loading
    /// (`--no-context-files`, `-nc` in the TS CLI).
    #[arg(long = "no-context-files")]
    pub no_context_files: bool,

    /// Suppress the built-in startup header (the key-hint screen printed
    /// above the transcript on entry). The header is also the `app.header`
    /// toggle at runtime, and is only shown by interactive mode. The TS CLI
    /// spells this as the `quietStartup` setting instead.
    #[arg(long = "no-header")]
    pub no_header: bool,

    /// Export a session file to HTML and exit.
    ///
    /// Usage: `pi --export <session.jsonl> [output.html]`. Without an
    /// output path the HTML is written in the current working directory as
    /// `pi-session-<input-basename>.html`. Mirrors `pi --export` in the TS
    /// CLI. `--export` runs before any provider credentials are resolved.
    #[arg(long = "export", value_names = ["SESSION", "OUTPUT"], num_args = 1..=2)]
    pub export: Option<Vec<std::path::PathBuf>>,

    /// Load a prompt template file or directory for this run only. The
    /// default locations (`~/.pi/agent/prompts` and `.pi/prompts`) are
    /// always searched unless `--no-prompt-templates` is set.
    /// Repeatable. Mirrors `pi --prompt-template`.
    #[arg(long = "prompt-template", value_name = "PATH")]
    pub prompt_template: Vec<std::path::PathBuf>,

    /// Disable prompt template discovery and loading
    /// (`--no-prompt-templates`, `-np` in the TS CLI).
    #[arg(long = "no-prompt-templates")]
    pub no_prompt_templates: bool,

    /// Trust the current project's `.pi` resources for this run
    /// (`--approve`, `-a` in the TS CLI). Overrides any saved decision.
    #[arg(long = "approve", short = 'a', conflicts_with = "no_approve")]
    pub approve: bool,

    /// Ignore the current project's `.pi` resources for this run, even if
    /// a saved decision trusts them (`--no-approve`, `-na` in the TS CLI).
    #[arg(long = "no-approve")]
    pub no_approve: bool,

    /// Path to a session directory. Defaults to `~/.pi/sessions/`.
    #[arg(long, value_name = "PATH")]
    pub session_dir: Option<std::path::PathBuf>,

    /// Resume a previous session by id or path.
    #[arg(long, value_name = "SESSION")]
    pub resume: Option<String>,

    /// Continue the most recent session (print mode only). Shorthand
    /// for `--session` with the most recently modified database.
    #[arg(long, value_name = "SESSION", conflicts_with_all = ["resume"])]
    pub r#continue_: Option<Option<String>>,

    /// Attach a specific session by id or path (print mode only).
    /// When omitted in `--print` mode the agent starts a fresh
    /// session.
    #[arg(long, value_name = "SESSION")]
    pub session: Option<String>,

    /// Output format for print mode. `text` streams the final reply on
    /// stdout (default); `json` emits a single JSON object at the end
    /// of the turn; `json-events` streams NDJSON event objects as they
    /// arrive (matches the RPC-mode wire shape).
    #[arg(long, value_name = "FORMAT", default_value = "text")]
    pub output_format: OutputFormat,

    /// Hard cap on the number of agent turns the print mode will run.
    /// Exceeding the cap forces the turn to end and exits with code 1.
    /// `0` disables the cap.
    #[arg(long, value_name = "N", default_value_t = 0u32)]
    pub max_turns: u32,
}

impl Cli {
    /// `Some(true)` for `--approve`, `Some(false)` for `--no-approve`,
    /// `None` when neither flag was passed.
    pub fn trust_override(&self) -> Option<bool> {
        if self.approve {
            Some(true)
        } else if self.no_approve {
            Some(false)
        } else {
            None
        }
    }

    /// Parse `std::env::args_os()`, normalising the multi-character short
    /// flags the TS CLI accepts but clap cannot express on its own
    /// (`-np` → `--no-prompt-templates`, `-na` → `--no-approve`).
    pub fn parse_with_aliases() -> Self {
        Self::parse_from(std::env::args_os().map(normalize_arg))
    }

    /// Same as [`Self::parse_with_aliases`] but returns clap's error
    /// instead of exiting, so the caller can control the exit code.
    pub fn try_parse_with_aliases() -> Result<Self, clap::Error> {
        Self::try_parse_from_args(std::env::args_os())
    }

    /// Parse an explicit argument list, applying the same TS short-flag
    /// normalisation as [`Self::parse_with_aliases`]. Used by tests (and
    /// embedders) that build their own argv.
    pub fn try_parse_from_args<I, T>(args: I) -> Result<Self, clap::Error>
    where
        I: IntoIterator<Item = T>,
        T: Into<std::ffi::OsString> + Clone,
    {
        Self::try_parse_from(args.into_iter().map(|arg| normalize_arg(arg.into())))
    }
}

/// Rewrite TS-only short flags into their long form.
fn normalize_arg(arg: std::ffi::OsString) -> std::ffi::OsString {
    if arg == "-np" {
        std::ffi::OsString::from("--no-prompt-templates")
    } else if arg == "-na" {
        std::ffi::OsString::from("--no-approve")
    } else {
        arg
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::path::PathBuf;

    #[test]
    fn ts_short_prompt_template_flags_are_normalised() {
        assert_eq!(
            normalize_arg(OsString::from("-np")),
            OsString::from("--no-prompt-templates")
        );
        assert_eq!(
            normalize_arg(OsString::from("-na")),
            OsString::from("--no-approve")
        );
        assert_eq!(
            normalize_arg(OsString::from("--print")),
            OsString::from("--print")
        );
    }

    #[test]
    fn parses_the_export_flag() {
        let cli = Cli::try_parse_from(["pi", "--export", "session.jsonl"]).expect("parses");
        assert_eq!(cli.export, Some(vec![PathBuf::from("session.jsonl")]));

        let cli =
            Cli::try_parse_from(["pi", "--export", "session.jsonl", "out.html"]).expect("parses");
        assert_eq!(
            cli.export,
            Some(vec![
                PathBuf::from("session.jsonl"),
                PathBuf::from("out.html")
            ])
        );

        // The optional second value must not swallow the next flag.
        let cli = Cli::try_parse_from(["pi", "--export", "session.jsonl", "--print", "hi"])
            .expect("parses");
        assert_eq!(cli.print.as_deref(), Some("hi"));
        assert_eq!(cli.export, Some(vec![PathBuf::from("session.jsonl")]));
    }

    #[test]
    fn trust_flags_resolve_to_an_override() {
        let plain = Cli::try_parse_from(["pi", "--print", "hi"]).expect("parses");
        assert_eq!(plain.trust_override(), None);

        let approve = Cli::try_parse_from(["pi", "--approve", "--print", "hi"]).expect("parses");
        assert_eq!(approve.trust_override(), Some(true));
        let short = Cli::try_parse_from(["pi", "-a", "--print", "hi"]).expect("parses");
        assert_eq!(short.trust_override(), Some(true));

        let deny = Cli::try_parse_from(["pi", "--no-approve", "--print", "hi"]).expect("parses");
        assert_eq!(deny.trust_override(), Some(false));

        // The two flags contradict each other.
        assert!(Cli::try_parse_from(["pi", "--approve", "--no-approve", "--print", "hi"]).is_err());
    }
}

/// Subcommands — mirrors `pi <subcommand>` in the TS CLI.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Print the resolved version of pi + active providers.
    Version {
        /// Output format (`text` or `json`).
        #[arg(long, value_name = "FORMAT", default_value = "text")]
        output: OutputFormat,
    },
    /// Install a pi package (`npm:`, `git:`, local path, or HTTPS URL).
    Install {
        /// Package spec (see packages.md in the docs).
        spec: String,
        /// Override the pi root used for the registry and install dir
        /// (defaults to `$PI_HOME` or `~/.pi`).
        #[arg(long, value_name = "PATH")]
        dir: Option<std::path::PathBuf>,
    },
    /// Remove a pi package.
    Remove {
        /// Package spec.
        spec: String,
        /// Override the pi root (defaults to `$PI_HOME` or `~/.pi`).
        #[arg(long, value_name = "PATH")]
        dir: Option<std::path::PathBuf>,
    },
    /// List installed pi packages.
    List {
        /// Override the pi root (defaults to `$PI_HOME` or `~/.pi`).
        #[arg(long, value_name = "PATH")]
        dir: Option<std::path::PathBuf>,
        /// Output format (`text` or `json`).
        #[arg(long, value_name = "FORMAT", default_value = "text")]
        output: OutputFormat,
    },
    /// Refresh the model catalog without changing installed packages.
    UpdateModels,
    /// Start an interactive session (the default when no subcommand
    /// is given).
    Interactive,
    /// Print mode — send a prompt, output result, exit (alias for
    /// `--print`).
    Print {
        /// Initial prompt.
        prompt: Vec<String>,
    },
    /// RPC mode — stream JSON events on stdout, accept JSON-RPC
    /// requests on stdin (alias for `--rpc`).
    Rpc,
    /// List built-in models — used by the `/model` slash command and
    /// the `--list-models` flag.
    ListModels {
        /// Output format (`text` or `json`).
        #[arg(long, value_name = "FORMAT", default_value = "text")]
        output: OutputFormat,
    },
    /// Session management subcommands (`list`, `show`, `export`,
    /// `migrate`). See [`SessionCommand`].
    Session {
        /// Session sub-action.
        #[command(subcommand)]
        action: SessionCommand,
    },
}

/// `pi session <action>` subcommands.
#[derive(Debug, Subcommand)]
pub enum SessionCommand {
    /// List every session in the session directory.
    List {
        /// Optional path to a specific SQLite database; defaults to
        /// every `*.sqlite` in the session directory.
        #[arg(long, value_name = "PATH")]
        database: Option<std::path::PathBuf>,
    },
    /// Print one session's message count and aggregated usage.
    ///
    /// The cached `sessions` projection is returned, but it is recomputed
    /// from `entries` + `usage_ledger` first; a disagreement is reported
    /// on stderr and as `"consistent": false` in the output.
    Stats {
        /// Session id to report on (matches the `id` field of the header).
        session_id: String,
        /// Path to the SQLite database containing the session.
        #[arg(long, value_name = "PATH")]
        database: std::path::PathBuf,
    },
    /// Show the entries of a single session as JSON Lines on stdout.
    Show {
        /// Session id to show (matches the `id` field of the header).
        session_id: String,
        /// Path to the SQLite database containing the session.
        #[arg(long, value_name = "PATH")]
        database: std::path::PathBuf,
    },
    /// Export a session as JSON Lines.
    ///
    /// The output starts with the session header line, followed by one
    /// line per stored entry, and ends with a trailing newline — i.e. it
    /// is a valid input for `pi session migrate` again.
    ///
    /// Without `--output` the JSONL is printed to stdout. With
    /// `--output PATH` it is written to that file (its parent directory
    /// is created when missing) and a JSON summary is printed instead.
    Export {
        /// Session id to export.
        session_id: String,
        /// Path to the SQLite database containing the session.
        #[arg(long, value_name = "PATH")]
        database: std::path::PathBuf,
        /// Write the JSONL to this file instead of stdout.
        #[arg(long, value_name = "PATH")]
        output: Option<std::path::PathBuf>,
    },
    /// Migrate a Stage 4 JSONL session file into the SQLite backend.
    /// The original JSONL is preserved on disk; the migration is
    /// non-blocking on failure.
    Migrate {
        /// Path to the JSONL session file.
        jsonl_path: std::path::PathBuf,
        /// Optional destination path; defaults to `<jsonl-stem>.sqlite`
        /// alongside the source.
        #[arg(long, value_name = "PATH")]
        to: Option<std::path::PathBuf>,
    },
}
