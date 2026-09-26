//! Local `!` / `!!` shell command runner.
//!
//! Migrated from `interactive.rs:L1116-L1286` on 2026-09-26 (PR1 of the
//! M1 single-file split; see `docs/API_STABILITY.md` §"interactive
//! module" and `scripts/architecture_no_regression.sh`).
//!
//! The `!` and `!!` commands run on a background task so the render
//! loop keeps reading keys while the command executes; `Esc` sets the
//! shared abort flag and the tool kills the child. One command at a
//! time, mirroring upstream's `session.isBashRunning`. The finished
//! outcome lands in the transcript through the same `InteractiveToolRenderer`
//! path the model's tool calls use, so collapsed preview, `Ctrl+O` and
//! click-to-expand all apply for free.

use std::env;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use pi_protocol::{Content, ToolCall, ToolResult};
use pi_tui::app::App;
use pi_tui::ToolBlockRenderer;
use tokio::sync::mpsc;

use crate::tools::{
    get_text_output, AbortLike, AgentTool, BashTool, InteractiveToolRenderer, ToolError, ToolOutput,
};

/// State for the local `!` / `!!` command that is currently running.
#[derive(Default)]
pub(super) struct BashRunner {
    run: Option<BashRun>,
}

struct BashRun {
    /// Set to `true` by `Esc`; the tool polls it and kills the child.
    abort: Arc<AtomicBool>,
    /// Delivers the finished command's outcome back to the render loop.
    rx: mpsc::UnboundedReceiver<BashOutcome>,
    /// The command text, echoed in the transcript header.
    command: String,
    /// `!!` — the result is kept out of the agent message log.
    excluded: bool,
}

struct BashOutcome {
    result: Result<ToolOutput, ToolError>,
    elapsed_ms: u64,
}

impl BashRunner {
    /// Whether a command is still in flight (finished-but-unpolled counts as
    /// running, so a second submission is refused until the block is shown).
    pub(super) fn is_running(&self) -> bool {
        self.run.is_some()
    }

    /// Start `command` on a background task. Returns immediately; the outcome
    /// arrives on [`BashRunner::poll`] / [`BashRunner::wait`].
    pub(super) fn start(&mut self, command: String, excluded: bool) {
        let abort = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::unbounded_channel();
        let abort_for_task = abort.clone();
        let command_for_task = command.clone();
        tokio::spawn(async move {
            let started = std::time::Instant::now();
            let args = serde_json::json!({ "command": command_for_task });
            let result = BashTool
                .execute(args, AbortLike::from_flag(abort_for_task))
                .await;
            let _ = tx.send(BashOutcome {
                result,
                elapsed_ms: started.elapsed().as_millis() as u64,
            });
        });
        self.run = Some(BashRun {
            abort,
            rx,
            command,
            excluded,
        });
    }

    /// Request cancellation of the running command (upstream
    /// `session.abortBash()`, `interactive-mode.ts:2858`).
    pub(super) fn cancel(&mut self) {
        if let Some(run) = &self.run {
            run.abort.store(true, Ordering::SeqCst);
        }
    }

    /// Fold a finished command into the transcript. Returns `true` once the run
    /// was consumed, so the render loop knows there is nothing left to poll.
    pub(super) fn poll(&mut self, app: &mut App, width: u16) -> bool {
        let Some(run) = self.run.as_mut() else {
            return true;
        };
        match run.rx.try_recv() {
            Ok(outcome) => {
                let run = self.run.take().expect("checked above");
                push_bash_block(app, &run.command, run.excluded, &outcome, width);
                true
            }
            Err(mpsc::error::TryRecvError::Empty) => false,
            Err(mpsc::error::TryRecvError::Disconnected) => {
                self.run = None;
                true
            }
        }
    }

    /// Await the running command and fold it into the transcript. Used by
    /// tests and any one-shot caller that has nothing else to poll.
    #[cfg(test)]
    pub(super) async fn wait(&mut self, app: &mut App, width: u16) {
        let Some(run) = self.run.as_mut() else {
            return;
        };
        let outcome = run.rx.recv().await;
        let Some(run) = self.run.take() else {
            return;
        };
        if let Some(outcome) = outcome {
            push_bash_block(app, &run.command, run.excluded, &outcome, width);
        }
    }
}

/// Render a finished `!` / `!!` command into the transcript.
///
/// The block goes through the same rich renderer the model's tool calls use
/// ([`crate::tools::InteractiveToolRenderer`]) and the same App-side
/// finalizer (`MessageView::finish_tool_execution_with_lines`), so the
/// collapsed preview, `Ctrl+O` and click-to-expand all apply for free.
///
/// Nothing is written to the agent's message log: the command is local, and
/// `!!` must never reach the model. `excluded` is recorded on the block's
/// details so the two forms stay distinguishable without changing the
/// transcript text.
pub(super) fn push_bash_block(
    app: &mut App,
    command: &str,
    excluded: bool,
    outcome: &BashOutcome,
    width: u16,
) {
    let (text, is_error, details) = match &outcome.result {
        Ok(output) => (
            get_text_output(output, false),
            false,
            output.details.clone(),
        ),
        Err(ToolError::Aborted) => ("command cancelled".to_string(), true, None),
        Err(err) => (err.to_string(), true, None),
    };

    let exclude_flag = serde_json::json!(excluded);
    let details = Some(match details {
        Some(mut details) => {
            if let Some(map) = details.as_object_mut() {
                map.insert("exclude_from_context".into(), exclude_flag);
            }
            details
        }
        None => serde_json::json!({ "exclude_from_context": exclude_flag }),
    });

    let call = ToolCall {
        id: next_bash_call_id(),
        name: "bash".to_string(),
        arguments: serde_json::json!({ "command": command }),
    };
    let result = ToolResult {
        tool_call_id: call.id.clone(),
        content: Box::new(Content::text(text.clone())),
        is_error,
        details,
        added_tool_names: None,
        images: Vec::new(),
    };

    let cwd = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut renderer = InteractiveToolRenderer::new(cwd);
    renderer.begin_tool(&call);
    let block = renderer.finish_tool(&result, width);

    let messages = app.messages_mut();
    messages.start_tool_execution(&call.id, "bash", command);
    messages.finish_tool_execution_with_lines(&call.id, outcome.elapsed_ms, &text, is_error, block);
}

fn next_bash_call_id() -> String {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    format!("local-bash-{}", SEQ.fetch_add(1, Ordering::Relaxed))
}