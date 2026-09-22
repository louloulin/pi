//! LUM-1418 — every rendered row is measured in **terminal columns**.
//!
//! The port used to measure layout in characters: one column per `char`. For
//! ASCII that is the same number, which is why no test caught it. For CJK — the
//! content a Chinese-language session is made of — it is wrong by a factor of
//! two: `wrap_line`/`wrap_words` produced rows twice as wide as the terminal,
//! the terminal hard-wrapped the overflow *again*, and every row below the wrap
//! was drawn one line off from the row the layout believed in. The composer had
//! the same defect one level worse, because its row bookkeeping is what places
//! the caret.
//!
//! This file is the regression net for the fix. It asserts the one invariant
//! that matters on every surface — **no rendered row is wider than the region
//! it was rendered for** — over a corpus that mixes ASCII, CJK, fullwidth
//! forms, emoji and combining marks, at many widths. A second group pins the
//! caret: a wide glyph must not push the `▍` marker (or the cursor row) off the
//! draft.
//!
//! `frame_dump_for_the_screenshot` prints a real 100×30 frame with CJK content;
//! `scripts/frame_to_png.py` renders it into
//! `docs/screenshots/lum1418-column-width.png`. That path exists because this
//! runner has no `pty` module (Windows), so `scripts/pty_capture.py` cannot run
//! here — see `docs/LUM1412_CHROME_CLIP.md` §5. The same frame is reproducible
//! end-to-end from the real binary on a Linux runner.

use std::sync::{Arc, Mutex};

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, Model, ProviderId};
use pi_tui::app::{App, AppConfig};
use pi_tui::message::{MessageItem, MessageView};
use pi_tui::prompt::Prompt;
use pi_tui::selector::{Selector, SelectorItem};
use pi_tui::status::{StatusBar, StatusData};
use pi_tui::width::columns;

const COLS: u16 = 100;
const ROWS: u16 = 30;

/// `set_keybindings` is process-global and this binary's tests share a
/// process, so every test that renders an `App` takes this lock.
static REGISTRY: Mutex<()> = Mutex::new(());

/// The corpus: plain ASCII, CJK sentences, mixed runs, fullwidth forms, emoji
/// and a combining mark. Every one of these is a case where "count characters"
/// and "count columns" disagree.
const CORPUS: &[&str] = &[
    "plain ascii text that has to wrap somewhere sensible",
    "这是一段中文文本，用来验证换行不是按字符数计算的，而是按终端列宽度计算的。",
    "混合 mixed 中英文 content 在一行里 交替出现 and must still wrap",
    "全角ＡＢＣ１２３与半角ABC123混排应当分别占两列和一列",
    "emoji 😀🎉🚀 在文本里也占两列，不能按一个字符算",
    "combining e\u{301} mark and a zero width joiner \u{200d} inside a run",
    "一个非常长的没有空格的中文句子应该能够按照列宽自由断行而不是整体溢出屏幕",
    "- 列表项：中文内容 + `code` + 链接 https://example.com/路径",
    "| 名称 | 说明 |\n| --- | --- |\n| 中文表格 | 内容很长需要按列宽折行 |",
];

fn every_line_fits(lines: &[String], width: usize, what: &str) {
    for (index, line) in lines.iter().enumerate() {
        assert!(
            columns(line) <= width,
            "{what}: row {index} occupies {} columns in a {width}-column region: {line:?}",
            columns(line)
        );
    }
}

#[test]
fn transcript_rows_never_exceed_the_render_width() {
    for width in [16usize, 24, 33, 48, 80] {
        let mut view = MessageView::new().with_markdown(true);
        for text in CORPUS {
            view.push(MessageItem::assistant(*text));
            view.push(MessageItem::user("中文提问：这一个问题本身也需要折行吗？"));
            view.push(MessageItem::tool("bash echo 你好 → 你好\n世界\nこんにちは"));
        }
        every_line_fits(&view.render_lines(width as u16), width, "transcript");
    }
}

#[test]
fn markdown_rows_never_exceed_the_render_width_without_the_view() {
    // The plain path (markdown off) measures through `message::wrap_words`,
    // which is a different wrap loop from `markdown::wrap_line`.
    for width in [12usize, 20, 40] {
        let mut view = MessageView::new().with_markdown(false);
        for text in CORPUS {
            view.push(MessageItem::assistant(*text));
        }
        every_line_fits(&view.render_lines(width as u16), width, "plain transcript");
    }
}

#[test]
fn selector_rows_never_exceed_the_render_width() {
    let selector = Selector::new(
        "模型",
        vec![
            SelectorItem::new("a", "中文标签很长很长需要被截断而不是溢出终端屏幕")
                .with_description("描述同样是中文而且很长，长到必须按列宽截断才行"),
            SelectorItem::new("b", "全角ＡＢＣＤＥＦＧ"),
            SelectorItem::new("c", "emoji 😀🎉 label"),
            SelectorItem::new("d", "short"),
        ],
    );
    for width in [20u16, 40, 64] {
        every_line_fits(&selector.render_lines(width), width as usize, "selector");
    }
}

#[test]
fn status_bar_never_exceeds_its_width_with_a_cjk_session() {
    let bar = StatusBar::new();
    let data = StatusData::new("claude-sonnet-4", "会话 id 很长很长很长")
        .with_session_name("中文会话名称也要被正确裁剪")
        .with_hint("? 查看帮助")
        .with_context_window(200_000);
    for width in [24usize, 40, 60, 100] {
        let line = bar.render(&data, width as u16);
        every_line_fits(&[line], width, "status bar");
    }
}

#[test]
fn composer_rows_fit_and_the_caret_stays_on_the_draft() {
    for width in [20u16, 30, 44, 80] {
        let mut prompt = Prompt::new("> ");
        prompt.editor_mut().insert_str(
            "这是一段中文草稿，它必须按照终端列宽换行，而不是按照字符数量换行，否则光标会跑到屏幕外面去。",
        );
        let (lines, _) = prompt.render_lines(width, 6, 0);
        every_line_fits(&lines, width as usize, "composer");

        // The caret marker is drawn inside a row that fits, and the row it is
        // drawn on is one the renderer actually produced.
        let caret_rows: Vec<&String> = lines.iter().filter(|line| line.contains('▍')).collect();
        assert_eq!(
            caret_rows.len(),
            1,
            "the caret must be drawn exactly once at {width} columns: {lines:?}"
        );
        assert!(caret_rows[0].starts_with("> ") || caret_rows[0].starts_with("  "));
    }
}

#[test]
fn a_wide_glyph_on_the_last_column_moves_to_the_next_row_whole() {
    // Width 7 leaves five columns of body (the `  ` role prefix takes two).
    // `ab` + 你 is four columns; adding 好 needs six, so the row breaks
    // *before* 好 rather than overflowing by the glyph's second column — which
    // is exactly what a `col >= width` post-check would do.
    let mut view = MessageView::new().with_markdown(false);
    view.push(MessageItem::assistant("ab你好世界"));
    let lines = view.render_lines(7);
    every_line_fits(&lines, 7, "transcript");
    assert_eq!(lines, vec!["  ab你", "  好世", "  界"]);
    // No glyph is dropped or duplicated by the wrap.
    let joined: String = lines.concat();
    for glyph in ["ab", "你", "好", "世", "界"] {
        assert!(joined.contains(glyph), "{glyph:?} lost in {lines:?}");
    }
}

fn faux_model() -> Model {
    Model {
        provider: ProviderId::new("faux"),
        id: "faux-model".into(),
        api: Api::Faux,
        label: Some("Faux".into()),
        context_window: 1024,
        max_output_tokens: 256,
    }
}

fn cjk_app() -> App {
    let agent = Agent::new(AgentOptions::new(
        faux_model(),
        Arc::new(FauxProvider::default()),
        "you are pi",
    ));
    let mut app = App::new(
        &agent,
        AppConfig {
            session_id: "lum1418-column-width".into(),
            startup_header: true,
            ..AppConfig::default()
        },
    );
    app.messages_mut()
        .push(MessageItem::user("帮我按终端列宽重排这段中文，谢谢。"));
    app.messages_mut().push(MessageItem::assistant(
        "## 结论\n\n中文、全角字符与 emoji 都占 **两列**，所以换行必须按列宽计算。\n\n\
         - 你 好 世 界：四个汉字 = 8 列\n\
         - `code 中英混排`\n\n\
         > 引用块里的中文同样按列宽折行，不会再溢出终端。\n\n\
         | 名称 | 说明 |\n| --- | --- |\n| 列宽 | 两列一个汉字 |\n",
    ));
    app
}

/// The frame the screenshot is rendered from. Every row is padded to the
/// region width with **columns**, so the dump is a faithful character grid for
/// a terminal that draws wide glyphs twice (see `scripts/frame_to_png.py`).
#[test]
fn frame_dump_for_the_screenshot() {
    let _guard = REGISTRY
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut app = cjk_app();
    app.prompt_mut()
        .editor_mut()
        .insert_str("中文草稿：按列宽换行，光标不能跑出屏幕。");
    let snapshot = app.render_snapshot(COLS, ROWS);
    for (index, line) in snapshot.lines.iter().enumerate() {
        assert!(
            columns(line) <= COLS as usize,
            "frame row {index} overflows: {line:?}"
        );
    }
    println!("FRAME DUMP cols={COLS} rows={ROWS}");
    for line in &snapshot.lines {
        println!("|{}|", pad(line, COLS as usize));
    }
    println!("END FRAME DUMP");
}

/// Pad `line` to `width` **columns** (not characters) for the frame dump.
fn pad(line: &str, width: usize) -> String {
    let mut out = String::from(line);
    for _ in columns(line)..width {
        out.push(' ');
    }
    out
}
