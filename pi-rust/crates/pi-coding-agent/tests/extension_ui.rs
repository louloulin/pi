//! End-to-end tests for the interactive extension UI bridge (LUM-1077).
//!
//! These drive the real JS host: an extension file calls
//! `ctx.ui.confirm(...)` from a command, from a tool, and from
//! `session_start`, and the test asserts what the host answers in
//! interactive mode (TUI dialog) versus print mode (documented deny).

use std::sync::Arc;
use std::time::Duration;

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_coding_agent::extensions::ui_bridge::TuiUi;
use pi_coding_agent::extensions::wiring::{self, ExtensionLoadOptions};
use pi_protocol::{Api, Content, Model, ProviderId, ToolCall};
use pi_tui::app::{App, AppConfig, StepOutcome};
use pi_tui::dialog::DialogKind;
use pi_tui::input::{Key, KeyCode, KeyModifiers};
use tokio_util::sync::CancellationToken;

/// Extension used by every test: a command and a tool that both ask the
/// user to confirm, plus a `session_start` handler that asks *during*
/// load (when the TUI is not pumping dialogs yet).
const EXTENSION: &str = r#"
    module.exports = function (pi) {
        async function ask(ctx) {
            return await ctx.ui.confirm("Delete files?", "This cannot be undone.");
        }
        pi.on("session_start", async function (event, ctx) {
            const ok = await ask(ctx);
            pi.appendEntry("boot-result", { ok: ok });
        });
        pi.registerCommand("ask", {
            description: "Asks for confirmation",
            handler: async function (args, ctx) {
                const ok = await ask(ctx);
                pi.appendEntry("ask-result", { ok: ok });
                return ok ? "accepted" : "denied";
            },
        });
        pi.registerTool({
            name: "ask_tool",
            label: "Ask",
            description: "Confirms before acting",
            parameters: { type: "object" },
            execute: async function (args, ctx) {
                const ok = await ask(ctx);
                pi.appendEntry("tool-result", { ok: ok });
                return { content: [{ type: "text", text: ok ? "accepted" : "denied" }] };
            },
        });
    };
"#;

fn faux_model() -> Model {
    Model {
        provider: ProviderId::new("faux"),
        id: "faux-model".into(),
        api: Api::Faux,
        label: Some("Faux".into()),
        context_window: 8192,
        max_output_tokens: 1024,
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("runtime")
}

/// Write [`EXTENSION`] into a unique temp dir and return its path.
fn write_extension(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("pi-ext-ui-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    let file = dir.join("ui.js");
    std::fs::write(&file, EXTENSION).expect("write extension");
    (dir, file)
}

fn app() -> App {
    let agent = Agent::new(AgentOptions::new(
        faux_model(),
        Arc::new(FauxProvider::default()),
        "you are pi",
    ));
    let config = AppConfig {
        session_id: "extension-ui".into(),
        ..AppConfig::default()
    };
    App::new(&agent, config)
}

/// Pump the App until the extension's dialog shows up, then press Enter.
///
/// `Join` is whatever the spawned extension call returns; the helper
/// returns it so each test can assert on the outcome it cares about.
async fn answer_prompt_with_enter<T>(
    app: &mut App,
    join: impl std::future::Future<Output = T>,
) -> T {
    tokio::time::timeout(Duration::from_secs(20), async {
        while !app.dialog_open() {
            app.poll_ui_dialogs();
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(
            app.dialog().map(|d| d.kind()),
            Some(DialogKind::Confirm),
            "extension asked for a confirmation"
        );
        assert_eq!(
            app.step_key(Key::new(KeyCode::Enter, KeyModifiers::NONE)),
            StepOutcome::Redraw
        );
        join.await
    })
    .await
    .expect("the extension dialog never reached the TUI")
}

#[test]
fn interactive_command_confirm_is_accepted_by_the_tui() {
    let (dir, file) = write_extension("command");
    let rt = runtime();

    let mut ui = TuiUi::new();
    let mut options = ExtensionLoadOptions::for_mode(None, dir.clone(), "tui", true);
    options.explicit = vec![file];
    options.ui = Some(ui.bridge().clone());
    let loaded = wiring::load(&rt, &options);
    assert!(loaded.errors.is_empty(), "errors: {:?}", loaded.errors);
    assert!(loaded.runtime.has_command("ask"));
    ui.arm();

    let mut app = app();
    app.attach_ui_dialogs(ui.take_dialogs().expect("dialog receiver"));

    let runtime = loaded.runtime.clone();
    let handle = rt.spawn(async move { runtime.execute_command("ask", "").await });

    let outcome = rt
        .block_on(answer_prompt_with_enter(&mut app, handle))
        .expect("join")
        .expect("execute command");
    assert!(outcome.handled, "{outcome:?}");
    assert_eq!(outcome.result, serde_json::json!("accepted"));

    // The extension observed the accepted answer too.
    let entries = loaded.runtime.drain_side_effects().entries;
    let ask = entries
        .iter()
        .find(|e| e.custom_type == "ask-result")
        .expect("ask-result entry");
    assert_eq!(ask.data["ok"], true);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn interactive_tool_confirm_is_accepted_by_the_tui() {
    let (dir, file) = write_extension("tool");
    let rt = runtime();

    let mut ui = TuiUi::new();
    let mut options = ExtensionLoadOptions::for_mode(None, dir.clone(), "tui", true);
    options.explicit = vec![file];
    options.ui = Some(ui.bridge().clone());
    let loaded = wiring::load(&rt, &options);
    assert!(loaded.errors.is_empty(), "errors: {:?}", loaded.errors);
    ui.arm();

    let mut app = app();
    app.attach_ui_dialogs(ui.take_dialogs().expect("dialog receiver"));

    let executor = loaded.executor.clone();
    let call = ToolCall {
        id: "call-1".into(),
        name: "ask_tool".into(),
        arguments: serde_json::json!({}),
    };
    let handle = rt.spawn(async move { executor.execute(&call, CancellationToken::new()).await });

    let result = rt
        .block_on(answer_prompt_with_enter(&mut app, handle))
        .expect("join")
        .expect("execute tool");
    assert!(!result.is_error, "{result:?}");
    match result.content.as_ref() {
        Content::Text(text) => assert_eq!(text.text, "accepted"),
        other => panic!("unexpected tool content: {other:?}"),
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn print_mode_confirm_denies_instead_of_hanging() {
    let (dir, file) = write_extension("print");
    let rt = runtime();

    // No bridge: print mode has no way to render a dialog, so
    // `ctx.hasUI` is false and the request must resolve immediately.
    let mut options = ExtensionLoadOptions::for_mode(None, dir.clone(), "print", false);
    options.explicit = vec![file];
    let loaded = wiring::load(&rt, &options);
    assert!(loaded.errors.is_empty(), "errors: {:?}", loaded.errors);

    let outcome = rt
        .block_on(loaded.runtime.execute_command("ask", ""))
        .expect("execute command");
    assert!(outcome.handled, "{outcome:?}");
    assert_eq!(outcome.result, serde_json::json!("denied"));

    let entries = loaded.runtime.drain_side_effects().entries;
    let ask = entries
        .iter()
        .find(|e| e.custom_type == "ask-result")
        .expect("ask-result entry");
    assert_eq!(ask.data["ok"], false);
    // `drain_side_effects` deliberately hides the host's internal
    // `ui_notify` entries (they are not session content); the warning
    // the user sees on stderr is asserted in the pi-extensions suite.

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn session_start_ui_request_is_denied_before_the_tui_pumps_dialogs() {
    let (dir, file) = write_extension("boot");
    let rt = runtime();

    // Extension loading dispatches `session_start` before the render
    // loop exists; a modal queued there could never be answered, so the
    // bridge answers it with the deny default instead of blocking.
    let mut ui = TuiUi::new();
    let mut options = ExtensionLoadOptions::for_mode(None, dir.clone(), "tui", true);
    options.explicit = vec![file];
    options.ui = Some(ui.bridge().clone());

    let started = std::time::Instant::now();
    let loaded = wiring::load(&rt, &options);
    assert!(loaded.errors.is_empty(), "errors: {:?}", loaded.errors);
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "load blocked on a dialog: {:?}",
        started.elapsed()
    );

    let entries = loaded.runtime.drain_side_effects().entries;
    let boot = entries
        .iter()
        .find(|e| e.custom_type == "boot-result")
        .expect("boot-result entry");
    assert_eq!(boot.data["ok"], false);

    // Nothing was queued for a TUI that was not running yet.
    let mut dialogs = ui.take_dialogs().expect("dialog receiver");
    assert!(dialogs.try_recv().is_err(), "no dialog may be queued");

    let _ = std::fs::remove_dir_all(&dir);
}
