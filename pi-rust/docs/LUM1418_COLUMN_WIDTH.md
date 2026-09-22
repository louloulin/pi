# LUM-1418 — 终端列宽根治：一个汉字 = 两列，全 TUI 重新按列排版

> scope: `pi-rust/`（基线 `origin/feature/pi.rs` = `14474dcf0`）
> branch: `work/LUM-1418`（→ `feature/pi.rs`）
> 时间锚：LUM-1412 之后，LUM-981 已 `done`，本轮是 LUM-1366 §4「下一轮第一顺位」第 1 条的落地
> 交付物：`crates/pi-tui/src/width.rs`（新）、`crates/pi-tui/tests/column_width.rs`（新）、真帧截图、本文

## 1. TL;DR

- **根因**：整个 Rust TUI 用**字符数**当终端列宽（`message::display_width` / `markdown::styled_width` /
  `visual_text::display_width` / `selector` / `settings` / `status` / `latex` / `dialog` / `hyperlink`）。
  ASCII 下两者相等，所以 900+ 个测试、20+ 张截图都没暴露它；中文下差 2 倍 —— 按列宽排出来的行宽度是终端的两倍，
  终端再**硬折一次**，折线以下每一行都比排版以为的位置低一行。
- **本轮**：把列宽收敛成 `crate::width` 一个模块（`unicode-width 0.2.2` + 上游
  `visibleWidth`/`graphemeWidth` 的规则），把 **13 个渲染模块里的 71 处**测量/写入引用切过去，
  并把写入 buffer 的循环改成**按列推进**（宽字形的第二列被 `reset()`，旧帧的残留字符不会透出来）。
- **顺手补上的语义缺口**：CJK 断行规则（上游 `wordWrapLine` 的第二条规则，之前注释里写着「无法生效」）、
  table 的 `natural`/`longest_word` 宽度、markdown `wrap_line` 的**溢出前判定**（之前是 `col >= width` 事后判定，
  双列字形会把行撑出去一列）。
- **实测**：`pi-tui` **926 passed / 0 failed（52 个 target）**，基线 **907 / 0** → +19；
  `--workspace` 的**失败集合与基线逐条相同**（40 条，全是 Windows 环境问题：无 `/tmp`、bash 工具、绝对路径断言、node fs、trust）。
  `cargo fmt --check` 干净，`cargo clippy -p pi-tui --all-targets -- -D warnings` 干净。
- **截图**：`docs/screenshots/lum1418-column-width.png`（+ 可 grep 的 `.txt`），100×30 真 `App` 帧，
  中文会话 + 中文草稿 + 中文表格 + 引用块，`scripts/frame_to_png.py` 渲染（本机无 `pty`）。
- **仍然存在的同族缺陷**（本轮**未**做，见 §6）：鼠标选择 / 搜索高亮的**列 → 字符**映射还是按字符数算的
  （`app.rs:764/3259/4273/4445/4587`、`search.rs:207-225`）。排版对了，指针命中还差一步。

## 2. 为什么这是「真实 P0」而不是吹毛求疵

一段中文 assistant 回复，终端 80 列：

```
修复前：wrap_line 认为 "这是一段很长的中文回复……"(30 个汉字) 占 30 列 → 一行放完
        终端实际要 60 列 → 放不下 → 终端自己折成 2 行
        排版返回 1 行，终端画了 2 行 → 该行之后所有内容整体下移 1 行
        滚动条位置、PgUp 回滚行数、鼠标选中行、搜索高亮列 全部错位
```

同一根因在输入框上更严重：composer 的行几何是**光标位置**的唯一依据
（`visual_text` 的 `caret`/`cursor_at`/`preferred_col`），行宽算错 → 光标行号错 → `▍` 画在别的行上。
并且 `app::paint_prompt` 当年是 `for (col, ch) in line.chars().enumerate()`，一次写一格，所以
**一个 44 列的纯中文草稿只画出了前 22 个字**（本轮修复前的实测：`> 中`，见 §5 的 before 帧）。

## 3. 改了什么（文件:行）

| # | 位置 | 改动 |
|---|---|---|
| 1 | `crates/pi-tui/src/width.rs`（新，含 `char_columns`/`columns`/`prefix_columns`/`truncate_columns`/`is_cjk_break`） | 列宽的唯一实现：`\t`=3、控制符=0、East Asian Wide/Fullwidth=2、Ambiguous=1、emoji=2、组合符=0；CJK 断行脚本表对齐上游 `cjkBreakRegex` |
| 2 | `Cargo.toml:92` / `crates/pi-tui/Cargo.toml` | 新增 `unicode-width = "=0.2.2"`（锁版本；已在依赖图中，`--offline` 可用） |
| 3 | `crates/pi-tui/src/visual_text.rs` | composer 换行改按列；补上游 CJK 断行规则；`display_width_of` 改测列 |
| 4 | `crates/pi-tui/src/prompt.rs` | `render_line` / `empty_row` / `blank_row` / `build_prompt_row` / `window_prefix` / `char_truncate` 全部按列；`body_width` 用列宽算 label |
| 5 | `crates/pi-tui/src/message.rs` | `display_width`→列；`hard_wrap` 按列；`wrap_words` 增加 CJK token 切分（上游 `splitIntoTokensWithAnsi` 的 flush 语义）；补 3 处 `write_plain_row` |
| 6 | `crates/pi-tui/src/markdown.rs` | `wrap_line` 按列 + **溢出前判定**；`styled_width`/`longest_word_width`/list marker 按列 |
| 7 | `crates/pi-tui/src/styled.rs` | `line_width` 按列；写入循环按列推进 + 宽字形第二列 `reset()`；`clip_keep` 返回「列数 + 字符数」两个口径；新增 `write_plain_row` / `buffer_row_text` |
| 8 | `crates/pi-tui/src/app.rs` | `paint_prompt` / 对话框 overlay 改按列写 + `reset()` 覆盖列；`render_snapshot` 用 `buffer_row_text`（跳过宽字形覆盖格，否则 dump 读到 `你 好`） |
| 9 | `crates/pi-tui/src/status.rs` | `line_width` / `clip` / `clip_line` / `zone_lead` 预算按列 |
| 10 | `crates/pi-tui/src/selector.rs` `settings.rs` | `display_width` / `truncate_to_width` / 硬折循环按列 |
| 11 | `crates/pi-tui/src/dialog.rs` `latex.rs` `hyperlink.rs` `image.rs` | 换行 / 补白 / `visible_width` / 省略号目标宽度按列 |
| 12 | `crates/pi-tui/tests/column_width.rs`（新） | 7 条列宽回归：9 个面 × 多宽度 × 混合语料，断言**没有任何一行超过其区域列数**，外加光标与「宽字形整字换行」两条 |
| 13 | `crates/pi-tui/tests/markdown.rs` | 3 条旧断言按列重写（原来断言的是字符口径） |
| 14 | `scripts/frame_to_png.py` | dump 里的行列宽按 `wcwidth` 补齐；含 CJK 时自动换用中文字体（`msyh`/`simhei`/`NotoSansCJK`…） |

## 4. 门禁（本机实测，`--offline`）

| 命令 | 结果 |
|---|---|
| `cargo fmt --all -- --check` | 干净 |
| `cargo clippy --offline -p pi-tui --all-targets -- -D warnings` | 干净 |
| `cargo check --offline --locked --workspace` | OK（191 crates，仅 `pi-extensions` 一条既有 dead_code 警告） |
| `cargo test --offline --locked -p pi-tui` | **926 passed / 0 failed**，52 个 target（基线 907 / 0 → +19） |
| `cargo test --offline --workspace --no-fail-fast` | passed 2547 / failed 40 / ignored 2；**失败集合与基线逐条相同**（见 §5.2） |

### 4.1 基线对照（同一台机、同一工具链）

```text
baseline (origin/feature/pi.rs, 无本轮改动)  pi-tui 907 passed / 0 failed
after    (work/LUM-1418)                     pi-tui 926 passed / 0 failed   (+19)
workspace 失败项：before 53 行 FAILED、after 53 行 FAILED，逐条名字相同（只有耗时不同）
```

> `--locked` 能过：`Cargo.lock` 只多了 `unicode-width 0.2.2` 一个包，以及两处同名不同版本的消歧
> （`unicode-width 0.1.14` / `unicode-width 0.2.2`）。本机离线缓存没有 `rustix 1.1.5`，cargo 曾顺手把它降到
> 1.1.4 —— 手工还原成 1.1.5（与基线一致），还原后 `--locked` 全绿。

### 4.2 既有失败（**早于本轮**，不要误判为本轮回归）

40 条 workspace 失败全部与列宽无关，都是这台 Windows 机器的环境问题：
`paths/trust/mod_ignore/js_loader/resource_loader/export/session_file` 的绝对路径与错误文案断言、
`tools_render`/`print_mode`/`ls`/`find` 的真 `bash` 工具（本机无 `/tmp`）、
`pi-extensions` 的 node fs / SDK 模块、`chatinput_chord_conflicts::the_allowlist_names_ids_that_exist_and_actually_overlap`
的 `app.models.save/ctrl+s` 允许项失效。基线逐条复现，见 §4.1。

## 5. 截图与证据

- `docs/screenshots/lum1418-column-width.png` — 100×30 **真 `App` 帧**（`render_snapshot`，与驱动喂给 ratatui
  的同一份 cell 网格），内容：启动头 / 中文提问 / 中文 markdown（标题、列表、行内 code、引用块、GFM 表格）/
  24 行中文草稿 + `▍` 光标 / 状态栏。
- `docs/screenshots/lum1418-column-width.txt` — 同帧字符网格 dump，可直接 grep 断言。
- 生成方式：`crates/pi-tui/tests/column_width.rs::frame_dump_for_the_screenshot`
  （`cargo test --nocapture` 打印 `FRAME DUMP`）+ `scripts/frame_to_png.py`。
- **诚实说明**：本机（Windows）没有 `pty` / `termios`，`scripts/pty_capture.py` **跑不了**，
  所以这是**冻结帧**渲染（frame-buffer 路径，见 `docs/LUM1412_CHROME_CLIP.md` §5），
  能证明**排版结果**，不能证明按键交互时序。Linux runner 上同帧可用
  `scripts/pty_capture.py` 端到端复现。

### 5.1 修复前后（44 列 composer）

```text
before  | > 中                                   ← paint_prompt 一次只写 1 格，中文草稿被截掉 3/4
after   | > 中文草稿：按列宽换行，光标不能跑出屏幕。▍
```

### 5.2 关键不变量（新测试网）

1. **没有一行超过它的区域列数**：`pi-tui/tests/column_width.rs` 用 9 条混合语料（ASCII / CJK / 全角 /
   emoji / 组合符 / 表格 / 列表 / 引用）在 12–80 列下逐个面断言（transcript 两套换行器、selector、status、
   composer、App 帧）。
2. **宽字形不被劈成两半**：`a_wide_glyph_on_the_last_column_moves_to_the_next_row_whole`
   （`ab你好世界` @ 7 列 → `["  ab你", "  好世", "  界"]`，无丢字）。
3. **光标在草稿里**：`composer_rows_fit_and_the_caret_stays_on_the_draft`（`▍` 恰好出现一次且落在 label 行）。
4. **CJK 断行**：`宽度 6 的 "你好世界"` → `["你好世", "界"]`（先填满列预算再断，不是一到 CJK 边界就断）。
5. **零宽 / 制表符**：`width::tests`（`a\tb`=5、`a\rb`=2、`e\u{301}`=1、emoji=2、Ambiguous=1）。

## 6. 仍然存在的同族缺陷（下一轮第一顺位，本轮**不**领）

排版已经按列，但**指针 / 高亮**的「屏幕列 ↔ 字符下标」换算还是字符口径：

| 位置 | 问题 | 影响 |
|---|---|---|
| `crates/pi-tui/src/app.rs:764`（`word_segments`）、`3259`、`4273`、`4445`、`4587` | 选择列以字符下标计 | 点在汉字右半边会选中/起选到错位的字；双击选词以字符跳 |
| `crates/pi-tui/src/search.rs:207-225`（`segment.start_col/end_col`） | 搜索高亮列以 grapheme 字符数计 | 中文行的高亮块横向错位约 2 倍 |
| `crates/pi-tui/src/app.rs:43` 的模块注释 | 明文写着「列是字符不是显示格」 | 文档需要跟着改 |

这是一次**选择模型**（`mouse_region` / `selection_granularity` / `alt_screen_search` 三套测试）的改动，
比本轮的布局改动更靠近输入栈，单独一轮更安全。**本轮不改**，理由：本轮已经动了 10 个渲染面，
再叠一次命中测试模型会让「哪一层坏了」无法二分。

其余 LUM-1366 §4 的顺位（footer 分片预算、第二批扩展事件）与 LUM-1367/LUM-1412 已完成，不再重复。

## 7. 与 TS / codex / Martty 的对齐结论

| 轴 | 上游 pi (`packages/tui/src/utils.ts`) | codex / Martty | 本轮前 pi-rust | 本轮后 |
|---|---|---|---|---|
| 字符串宽度 | `visibleWidth` = `get-east-asian-width` + string-width 规则 + emoji 2 列 + 组合符 0 | 同（`unicode-width` / `UnicodeWidthStr`） | **字符数** ❌ | `crate::width::columns` ✅ |
| 换行断点 | 空白后 + **CJK 相邻可断** | 同 | 只有空白后 ❌（注释承认 CJK 规则「无法生效」） | 两条规则都实现 ✅ |
| 溢出判定 | 加之前判（`current + w > max`） | 同 | 加之后判（`col >= width`）❌ | 加之前判 ✅ |
| 写 buffer | 每 grapheme 一次 `set_stringn`，宽字形覆盖格 reset | ratatui `set_stringn` 同样 reset | 一次一格 ❌ | 按列推进 + `reset()` ✅ |
| 截断 | `truncateToWidth` 按列 + 省略号 | 同 | 按字符 ❌ | `truncate_columns` ✅ |
| 表格列宽协商 | `natural` / `longestWord` 按列 | 同 | 按字符 ❌ | 按列 ✅ |

一句话：**「TUI chatinput 和 codex/Martty 差距很大」这件事，在宽度这一层已经不存在了**；
剩下的差距在 §6 的指针映射，和 LUM-1366 §4 已列的两条非宽度项。

## 8. 范围之外

- **未碰** `pi-coding-agent` 的任何 `.rs`（本轮只动 `pi-tui`、crates 清单、一个脚本、文档与截图）。
- **未碰**扩展事件轴、slash 命令、CLI flag 面（LUM-1366 §4 的另外两条）。
- **未碰** CI / Docker；本机 Windows 无法跑真 PTY，截图路径沿用 LUM-1412 已建立的 frame-buffer 通道。
- **未碰** `pi-agent-core` / `pi-ai` / `pi-session` 等非 TUI crate。
