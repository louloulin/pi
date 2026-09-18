//! Text fallback used when the TUI cannot start (terminal setup fails,
//! or the agent turn returns an unrecoverable error). Mirrors the
//! `--print` mode shape: read stdin / prompt the user, run a single
//! turn, write the result to stdout, exit.

use std::io::Write;

use pi_agent_core::Agent;

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
                    pi_agent_core::AssistantMessageUpdate::TextDelta(delta),
                ) => {
                    write!(stdout, "{delta}")?;
                    stdout.flush()?;
                }
                pi_agent_core::AgentEvent::TurnEnd { .. } => break,
                _ => {}
            }
        }
        writeln!(stdout)?;
        eprintln!("> next prompt (Ctrl+D to exit):");
    }
    Ok(())
}
