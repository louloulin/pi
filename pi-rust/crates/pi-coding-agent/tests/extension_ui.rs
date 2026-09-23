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
use pi_coding_agent::extensions::ui_bridge::{RegionPump, TuiUi};
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
    write_extension_source(tag, EXTENSION)
}

/// Write `source` into a unique temp dir and return `(dir, file)`.
fn write_extension_source(tag: &str, source: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("pi-ext-ui-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    let file = dir.join("ui.js");
    std::fs::write(&file, source).expect("write extension");
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

// ---------------------------------------------------------------------------
// `ctx.ui` regions / overlays (LUM-1190)
//
// The dialog tests above cover the request/response half of `ctx.ui`. These
// cover the mutation half: `setWidget` / `setHeader` / `setFooter` /
// `setEditorComponent` / `custom` travel through the region bridge and land in
// the `App`, which renders them.
// ---------------------------------------------------------------------------

/// Installs every region from `session_start`, echoing the render width so the
/// test can prove the TUI's width reaches the JS `render(width)`.
const EXTENSION_REGIONS: &str = r#"
    module.exports = function (pi) {
        pi.on("session_start", function (event, ctx) {
            ctx.ui.setHeader(() => ({ render: (width) => ["HEADER@" + width] }));
            ctx.ui.setWidget("todos", ["TODO one", "TODO two"], { placement: "aboveEditor" });
            ctx.ui.setFooter(() => ({ render: () => ["FOOTER"] }));
            ctx.ui.setEditorComponent(() => ({
                render: () => ["EDITOR"],
                handleInput: (data) => { pi.appendEntry("editor-key", { data: data }); },
            }));
        });
    };
"#;

/// Opens an overlay and leaves it up (no `await`, so the session stays live).
const EXTENSION_OVERLAY: &str = r#"
    module.exports = function (pi) {
        pi.on("session_start", function (event, ctx) {
            ctx.ui.custom(() => ({ render: () => ["== OVERLAY =="] }), { overlay: true });
        });
    };
"#;

/// Opens an overlay, then needs two dialogs to observe: the first opens it,
/// the second gives the render loop a chance to see it hidden before
/// `resolve()` closes it. Driven from a command because a `session_start`
/// handler runs before the TUI pumps dialogs (see
/// `session_start_ui_request_is_denied_before_the_tui_pumps_dialogs`).
const EXTENSION_OVERLAY_LIFECYCLE: &str = r#"
    module.exports = function (pi) {
        pi.registerCommand("overlay", {
            description: "Drives the overlay lifecycle",
            handler: async function (args, ctx) {
                const handle = ctx.ui.custom(() => ({ render: () => ["== OVERLAY =="] }), {
                    overlay: true,
                });
                await ctx.ui.confirm("one", "open the overlay");
                handle.setVisible(false);
                await ctx.ui.confirm("two", "hide the overlay");
                handle.resolve("done");
                await handle;
                pi.appendEntry("overlay-closed", { result: "done" });
                return "done";
            },
        });
    };
"#;

/// `ctx.ui.setStatus(key, text)` used to be inert here: the shim answered
/// `not available in the pi extension host` and the footer stayed two rows.
/// This drives the real JS host, the region pump and the App, so the assertion
/// is on a rendered frame, not on a counter.
const EXTENSION_STATUS: &str = r#"
    module.exports = function (pi) {
        pi.on("session_start", function (event, ctx) {
            // Installed out of order, and one key cleared, so the test also
            // pins the sort and the clear contract.
            ctx.ui.setStatus("zz", "queued");
            ctx.ui.setStatus("aa", "thinking");
            ctx.ui.setStatus("gone", "never seen");
            ctx.ui.setStatus("gone", undefined);
        });
    };
"#;

/// Load an extension that drives `ctx.ui` regions and return the pieces the
/// interactive loop owns: the dialog/region halves plus the host runtime.
fn load_regions(
    rt: &tokio::runtime::Runtime,
    dir: &std::path::Path,
    file: &std::path::Path,
) -> (
    TuiUi,
    RegionPump,
    pi_coding_agent::extensions::wiring::ExtensionLoadOutcome,
) {
    let mut ui = TuiUi::new();
    let mut options = ExtensionLoadOptions::for_mode(None, dir.to_path_buf(), "tui", true);
    options.explicit = vec![file.to_path_buf()];
    options.ui = Some(ui.bridge().clone());
    options.ui_region_host = Some(ui.region_host());
    let loaded = wiring::load(rt, &options);
    assert!(loaded.errors.is_empty(), "errors: {:?}", loaded.errors);
    ui.arm();
    let (ops, tx) = ui.take_regions().expect("region receiver");
    (ui, RegionPump::new(ops, tx), loaded)
}

#[test]
fn interactive_extension_statuses_reach_the_footer_row() {
    let (dir, file) = write_extension_source("status", EXTENSION_STATUS);
    let rt = runtime();
    let (mut ui, mut pump, _loaded) = load_regions(&rt, &dir, &file);

    let mut app = app();
    app.attach_ui_dialogs(ui.take_dialogs().expect("dialog receiver"));
    // `session_start` ran during load, so the mutations are already queued.
    rt.block_on(async { pump.pump(&mut app, 60).await });

    // The host's map landed in the App's footer snapshot, sorted by key and
    // with the cleared key gone.
    let statuses = app.status_data().extension_statuses.clone();
    assert_eq!(
        statuses,
        vec![
            ("aa".to_string(), "thinking".to_string()),
            ("zz".to_string(), "queued".to_string()),
        ],
        "the cleared key must not survive"
    );

    // …and the frame grew a third footer row with the extension text on it.
    let snapshot = app.render_snapshot(60, 14);
    let last = snapshot.lines.last().expect("a rendered frame").trim_end();
    assert_eq!(last, "thinking queued", "{:?}", snapshot.lines);
    // The stats row is still the row above it — the status row is appended
    // below upstream's `[pwdLine, statsLine]`, not inserted between them.
    assert!(
        !snapshot.lines[13 - 1].contains("thinking"),
        "{:?}",
        snapshot.lines
    );

    // Clearing the last status restores the two-row footer.
    app.clear_extension_statuses();
    rt.block_on(async { pump.pump(&mut app, 60).await });
    let snapshot = app.render_snapshot(60, 14);
    assert!(!snapshot.lines.iter().any(|line| line.contains("thinking")));
    assert!(!snapshot.lines.iter().any(|line| line.contains("queued")));
}

#[test]
fn interactive_regions_render_into_the_app() {
    let (dir, file) = write_extension_source("regions", EXTENSION_REGIONS);
    let rt = runtime();
    let (mut ui, mut pump, loaded) = load_regions(&rt, &dir, &file);

    let mut app = app();
    app.attach_ui_dialogs(ui.take_dialogs().expect("dialog receiver"));

    // `session_start` ran during load, so the mutations are already queued.
    rt.block_on(async { pump.pump(&mut app, 40).await });

    let lines = app.render_snapshot(40, 12).lines;
    let row = |needle: &str| {
        lines
            .iter()
            .position(|line| line.contains(needle))
            .unwrap_or_else(|| panic!("{needle} missing from {lines:#?}"))
    };
    // The width the loop passed to `pump` reached the JS `render(width)`.
    let header = row("HEADER@40");
    let widget = row("TODO one");
    let editor = row("EDITOR");
    let footer = row("FOOTER");
    assert!(
        header < widget && widget < editor && editor < footer,
        "regions keep their order (header, widget, editor, footer): {lines:#?}"
    );
    assert_eq!(lines[widget + 1].trim(), "TODO two");

    // The editor component, not the default prompt, owns the editor region…
    assert!(
        !lines[editor].contains("> "),
        "the custom editor replaced the prompt: {lines:#?}"
    );

    // …and it receives terminal input, which is delivered to JS on the next
    // pump as the raw data string upstream's `handleInput(data)` expects.
    assert_eq!(
        app.step_key(Key::new(KeyCode::Char('x'), KeyModifiers::NONE)),
        StepOutcome::Redraw
    );
    rt.block_on(async { pump.pump(&mut app, 40).await });
    let entries = loaded.runtime.drain_side_effects().entries;
    let key = entries
        .iter()
        .find(|e| e.custom_type == "editor-key")
        .expect("editor-key entry");
    assert_eq!(key.data["data"], "x");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn interactive_custom_overlay_renders_over_the_transcript() {
    let (dir, file) = write_extension_source("overlay", EXTENSION_OVERLAY);
    let rt = runtime();
    let (mut ui, mut pump, _loaded) = load_regions(&rt, &dir, &file);

    let mut app = app();
    app.messages_mut().push(pi_tui::MessageItem::user("hello"));
    app.attach_ui_dialogs(ui.take_dialogs().expect("dialog receiver"));

    // The factory resolves on a microtask, so pump until the overlay lands.
    rt.block_on(async {
        for _ in 0..200 {
            pump.pump(&mut app, 40).await;
            if app.custom_open() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("the custom overlay never opened");
    });

    let lines = app.render_snapshot(40, 12).lines;
    assert!(
        lines.iter().any(|line| line.contains("== OVERLAY ==")),
        "the overlay renders: {lines:#?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn interactive_custom_overlay_visibility_and_close_reach_the_app() {
    let (dir, file) = write_extension_source("overlay-lifecycle", EXTENSION_OVERLAY_LIFECYCLE);
    let rt = runtime();
    let (mut ui, mut pump, loaded) = load_regions(&rt, &dir, &file);

    let mut app = app();
    app.attach_ui_dialogs(ui.take_dialogs().expect("dialog receiver"));

    let runtime = loaded.runtime.clone();
    let handle = rt.spawn(async move { runtime.execute_command("overlay", "").await });

    let mut dialogs_answered = 0;
    let mut saw_hidden = false;
    let mut saw_visible = false;
    rt.block_on(async {
        for _ in 0..600 {
            pump.pump(&mut app, 40).await;
            app.poll_ui_dialogs();
            if app.custom_open() {
                if app.custom_visible() {
                    saw_visible = true;
                } else {
                    saw_hidden = true;
                }
            }
            if app.dialog_open() {
                dialogs_answered += 1;
                app.step_key(Key::new(KeyCode::Enter, KeyModifiers::NONE));
            }
            if dialogs_answered >= 2 && !app.custom_open() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    });
    let outcome = rt.block_on(handle).expect("join").expect("execute command");
    assert_eq!(outcome.result, serde_json::json!("done"));

    assert_eq!(dialogs_answered, 2, "both extension dialogs were answered");
    assert!(
        saw_visible,
        "the overlay was open and visible after it opened"
    );
    assert!(
        saw_hidden,
        "`handle.setVisible(false)` reached the App while the session stayed open"
    );
    assert!(!app.custom_open(), "`handle.resolve()` closed the overlay");

    let entries = loaded.runtime.drain_side_effects().entries;
    assert!(
        entries.iter().any(|e| e.custom_type == "overlay-closed"),
        "the extension's `resolve()` future completed"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
