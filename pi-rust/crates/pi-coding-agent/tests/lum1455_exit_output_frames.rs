//! LUM-1455 frame source — what the terminal is left with after the TUI exits.
//!
//! `cargo test -- --nocapture` prints the byte-exact exit output of
//! `pi_coding_agent::interactive::exit_screen_output` for a real session, and
//! `scripts/frame_to_png.py` paints it (this Windows runner has no PTY — see
//! `docs/LUM1455_EXIT_TRANSCRIPT.md` §4).
//!
//! Three panels, all composed from a real [`pi_tui::App`] carrying the same
//! kind of content a session does (a user turn, a Markdown answer with CJK, a
//! tool block):
//!
//! 1. `fullscreenExitOutput: "transcript"` (the default) **with** a resumable
//!    session — the transcript, then `To resume this session: …`;
//! 2. `"resume-hint"` — the previous screen is restored, so only the hint is
//!    written;
//! 3. `"transcript"` for a session that has no SQLite database yet (a fresh
//!    interactive session) — the transcript alone, because `pi --resume <id>`
//!    could not resolve it (`resume_command_for`).
//!
//! The transcript lines are the *real* `App::transcript_text` output, so the
//! dump carries the live palette's SGR codes; `frame_to_png.py` honours `7` /
//! `1` / `4` and ignores `2` (dim), which is why panel 1's hint line renders
//! plain in the PNG while the raw bytes are dim. That is stated in the caption
//! and in the `.txt` artifact, not hidden.
//!
//! What this proves: the exact bytes a user's terminal receives on quit.
//! What it does not prove: keystroke timing or a live tty — the behavioural
//! contract is pinned by `interactive::tests::exit_output_*` and
//! `format_resume_command_*`.

use std::sync::Arc;

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_coding_agent::config::FullscreenExitOutput;
use pi_coding_agent::interactive::exit_screen_output;
use pi_protocol::{Api, Model, ProviderId};
use pi_tui::app::{App, AppConfig};
use pi_tui::message::MessageItem;

/// The width the session is dumped at (the real driver uses the terminal's).
const COLS: u16 = 100;

fn faux_model() -> Model {
    Model {
        provider: ProviderId::new("faux"),
        id: "faux-model".into(),
        api: Api::Faux,
        label: Some("Faux".into()),
        context_window: 8192,
        max_output_tokens: 512,
    }
}

/// A session worth leaving on the terminal: a question, a Markdown answer with
/// CJK and a code span, and a tool block.
fn session() -> App {
    let agent = Agent::new(AgentOptions::new(
        faux_model(),
        Arc::new(FauxProvider::default()),
        "you are pi",
    ));
    let mut app = App::new(
        &agent,
        AppConfig {
            session_id: "lum1455-exit".into(),
            ..AppConfig::default()
        },
    );
    app.messages_mut()
        .push(MessageItem::user("退出后还能看到这次会话吗？"));
    app.messages_mut().push(MessageItem::assistant(
        "能。`fullscreenExitOutput` 默认 `transcript`，退出时把整段记录写回普通屏幕，\
         滚动缓冲里就留下了它，随后打印一行 dim 的 resume 提示。\n\n\
         设置为 `resume-hint` 时相反：屏幕恢复原样，只留那一行提示。",
    ));
    app.messages_mut()
        .push(MessageItem::tool("read pi-rust/crates/pi-tui/src/app.rs"));
    app.messages_mut()
        .push(MessageItem::user("那 resume 命令从哪来？"));
    app.messages_mut().push(MessageItem::assistant(
        "只有真的能 resume 的会话才打印：`--resume <id>` 只认 SQLite 会话，\
         所以全新会话（仅 JSONL）不会广告一条注定失败的命令。",
    ));
    app
}

/// Print one frame dump so `frame_to_png.py` can paint it.
fn dump(caption: &str, lines: &[String]) {
    // The leading newline keeps the marker on its own row: `cargo test` prints
    // `test <name> ... ` without a newline before captured output.
    println!("\nPANEL {caption}");
    println!("FRAME DUMP cols={COLS} rows={}", lines.len());
    for line in lines {
        println!("|{line}|");
    }
    println!("END FRAME DUMP");
}

/// Split the exit output into dump rows, keeping the SGR codes.
fn rows(text: &str) -> Vec<String> {
    text.lines().map(str::to_string).collect()
}

#[test]
fn frame_dump_transcript_mode_with_a_resumable_session() {
    let app = session();
    let transcript = app.transcript_text(COLS);
    let exit = exit_screen_output(
        FullscreenExitOutput::Transcript,
        Some(&transcript),
        Some("pi --resume lum1455-exit"),
    );
    // The transcript is in there, and so is the hint.
    assert!(exit.contains("退出后还能看到这次会话吗？"), "{exit}");
    assert!(exit.contains("To resume this session:"), "{exit}");
    assert!(exit.ends_with('\n'), "{exit:?}");
    dump(
        "LUM-1455 exit 后终端留下的内容（transcript + resume 提示；dim 在 PNG 里按普通字渲染）",
        &rows(&exit),
    );
}

#[test]
fn frame_dump_resume_hint_mode() {
    let app = session();
    let transcript = app.transcript_text(COLS);
    let exit = exit_screen_output(
        FullscreenExitOutput::ResumeHint,
        Some(&transcript),
        Some("pi --resume lum1455-exit"),
    );
    // Exactly one row: the transcript is deliberately dropped, the previous
    // screen comes back instead.
    assert_eq!(rows(&exit).len(), 1, "{exit:?}");
    dump(
        "LUM-1455 fullscreenExitOutput=resume-hint：只写提示，屏幕上恢复原内容",
        &rows(&exit),
    );
}

#[test]
fn frame_dump_transcript_mode_without_a_resumable_session() {
    let app = session();
    let transcript = app.transcript_text(COLS);
    let exit = exit_screen_output(FullscreenExitOutput::Transcript, Some(&transcript), None);
    assert!(!exit.contains("To resume this session:"), "{exit}");
    assert!(exit.contains("退出后还能看到这次会话吗？"), "{exit}");
    dump(
        "LUM-1455 全新会话（只有 JSONL，无 SQLite）：只留 transcript，不广告不可用的 resume",
        &rows(&exit),
    );
}
