//! Text fallback used when the TUI cannot start (terminal setup fails,
//! or the agent turn returns an unrecoverable error). Mirrors the
//! `--print` mode shape: read stdin / prompt the user, run a single
//! turn, write the result to stdout, exit.

use std::io::Write;
use std::path::PathBuf;

use pi_agent_core::Agent;

use crate::tools::render::{render_lines_ansi, render_lines_plain, ToolRenderSession};
use pi_tui::{ColorMode, StyledLine, Theme};

/// Reason the text fallback is being invoked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FallbackReason {
    /// `enable_raw_mode` or `EnterAlternateScreen` failed.
    TerminalSetupFailed(String),
    /// An agent turn returned an unrecoverable error.
    AgentError(String),
}

impl std::fmt::Display for FallbackReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FallbackReason::TerminalSetupFailed(msg) => {
                write!(f, "terminal setup failed: {msg}")
            }
            FallbackReason::AgentError(msg) => {
                write!(f, "agent error: {msg}")
            }
        }
    }
}

/// Run a single agent turn in text mode.
pub async fn run_text_fallback(agent: &mut Agent, reason: FallbackReason) -> anyhow::Result<()> {
    let _ = reason;
    eprintln!("pi: falling back to text mode");
    let theme = fallback_theme();
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut render_session = ToolRenderSession::new(cwd);
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let mut buffer = String::new();
    eprintln!("> type a prompt (Ctrl+D to exit):");
    loop {
        buffer.clear();
        let read = stdin.read_line(&mut buffer)?;
        if read == 0 {
            break;
        }
        let text = buffer.trim();
        if text.is_empty() {
            continue;
        }
        // Subscribe BEFORE `prompt` so the receiver catches the
        // events emitted synchronously by the loop.
        let mut rx = agent.subscribe();
        agent.prompt(text).await?;
        while let Some(event) = rx.recv().await {
            match event {
                pi_agent_core::AgentEvent::MessageUpdate(
                    pi_agent_core::AssistantMessageUpdate::TextDelta { delta },
                ) => {
                    write!(stdout, "{delta}")?;
                    stdout.flush()?;
                }
                // Tool calls and results are rendered through the same
                // renderer layer the TUI will use: read/write results get
                // syntax highlighting, everything else is skipped.
                pi_agent_core::AgentEvent::ToolExecutionStart { call } => {
                    let lines = render_session.call(&call, false);
                    emit_lines(&mut stdout, &lines, theme.as_ref())?;
                }
                pi_agent_core::AgentEvent::ToolExecutionEnd { result, .. } => {
                    let lines = render_session.result(&result);
                    emit_lines(&mut stdout, &lines, theme.as_ref())?;
                }
                pi_agent_core::AgentEvent::TurnEnd { .. } => {
                    render_session.clear();
                    break;
                }
                _ => {}
            }
        }
        writeln!(stdout)?;
        eprintln!("> next prompt (Ctrl+D to exit):");
    }
    Ok(())
}

/// Theme used for the text fallback.
///
/// `NO_COLOR` (or a missing built-in theme) disables styling entirely, which
/// keeps redirected output clean. There is no user theme selection on this
/// path: the fallback exists precisely because the TUI could not start.
fn fallback_theme() -> Option<Theme> {
    if std::env::var_os("NO_COLOR").is_some() {
        return None;
    }
    pi_tui::builtin_theme("dark", ColorMode::from_true_color(true)).ok()
}

/// Write one rendered block to `stdout`, appending a newline when the block
/// does not already end with one.
fn emit_lines(
    stdout: &mut std::io::Stdout,
    lines: &[StyledLine],
    theme: Option<&Theme>,
) -> std::io::Result<()> {
    if lines.is_empty() {
        return Ok(());
    }
    let rendered = match theme {
        Some(theme) => render_lines_ansi(lines, theme),
        None => render_lines_plain(lines),
    };
    write!(stdout, "{rendered}")?;
    if !rendered.ends_with('\n') {
        writeln!(stdout)?;
    }
    stdout.flush()
}
