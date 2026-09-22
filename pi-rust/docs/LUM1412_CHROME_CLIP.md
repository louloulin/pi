# LUM-1412 — chrome 截断 affordance：扩展 widget 行不再静默切半

> scope: `pi-rust/`；base = `origin/feature/pi.rs` tip `0cb14b63d`（LUM-1367 之后）
> branch: `work/LUM-1412`（→ `feature/pi.rs`）
> 本轮**改代码**：`crates/pi-tui/src/styled.rs`（新增带标记的写入器）+
> `crates/pi-tui/src/app.rs`（`paint_extension_lines` 与两个 chrome 标签改走它）
> 证据：单元黄金串 12/12、集成 18/18 + 2/2、`pi-tui` 全量 **907/0**（基线 895/0）、
> `crates/pi-coding-agent` 四个 TUI 面测试 **51/0**、44×14 帧缓冲截图 A/B

## 1. TL;DR

- 用户口径「现在的 tui chatinput 实现和 codex / Martty 差距很大」在本轮被拆成两个可复核的问题：
  **键位面上一轮已经对齐**（§6 的 11 行矩阵，LUM-1360/LUM-1366/LUM-1367 三轮逐条复核），
  **视觉面上确实还有确凿缺陷**——而且不是 chatinput 本体，是它周围 chrome 的**截断没有 affordance**。
- 缺陷（可复现）：`app.rs::paint_extension_lines` 是所有扩展 widget（启动头、above/below widget、
  footer、`custom` overlay）唯一的写入器，它调 `styled.rs::write_styled_line`，
  而后者在 `max_width` 处**直接 return**——既不换行也不留标记。
  真 44×14 里启动头折行行（`locale::header_folded_line`，51 字）被画成
  `hints hidden on a short terminal — Alt+H sho`，读者无法区分「句子就这么长」和「终端太窄」。
- 修复：新增 `styled::write_styled_line_ellipsized`。放得下 → 与旧写入器**逐 cell 相同**；
  放不下 → **按词边界**丢掉尾部残词、在截断处那一格写 `…`（`styled::CLIP_MARK`）。
  footer 的分片预算（LUM-1367）用的是同一条「整段语义单元 + 明确标记」原则，现在是同一套。
- 验证：`pi-tui` 全量 **907 passed / 0 failed**（52 个 target；本 runner 上的基线实测 `0cb14b63d` 为 **895/51**，
  +12 恰好是本轮新增的 12 条：8 单元 + 2 集成 + 2 新文件）；
  `styled::` 单元 12/12（含 1..=120 全宽度性质测试）；`pi-coding-agent` 四个 TUI 面测试 **51/0**；
  44×14 帧缓冲 A/B 截图 + 可 grep 的 `.txt`。
- 完成度：规模口径 **代码 88.9% / 测试 47.8%**；功能面加权 **81.4%**（继承 LUM-1260 §5 的 13 轴公式，
  本轮不改口径；若把测试轴从 0.44 刷成 0.478 同一公式给 **81.6%**），TUI 交互 id 接线 **43/44 = 97.7%**。
  本轮只把「TUI 视觉面 · chrome 截断」这一格从 ✗ 变成 ✓，**没有**动任何百分比口径。
- **本轮 runner 是 Windows**（`编程助手-winpi`，`dbe772db`），没有 `pty` 模块也没有 `pyte`，
  `scripts/pty_capture.py` 跑不起来 → **没有真 PTY 截图**，只有帧缓冲截图。
  这是本轮最重要的如实声明，展开在 §5，含 Linux 侧一条命令的复现路径。

## 2. 缺陷：实测与根因

### 2.1 实测帧（44×14）

`cargo test -p pi-tui --test lum1412_chrome_clip -- --nocapture`（本轮新增的帧源）：

```text
|pi v0.1.0                                   |
|hints hidden on a short terminal — Alt+H…   |   ← 本轮修复后
|> hello                                     |
|                                            | ×9
|> type a prompt — /help for commands        |
|Faux  lum1412-chrome-clip  in 0 out 0 ?/1.0k|
```

修复前同一行（`styled.rs::tests::the_plain_writer_still_clips_without_a_mark` 逐字节钉住）：

```text
|hints hidden on a short terminal — Alt+H sho|   ← 修复前，`sho` 是 `shows` 的一半
```

### 2.2 根因

```text
app.rs::paint_extension_lines(rect, lines, buf)      // header / above / below / footer / custom overlay
  └─ styled.rs::write_styled_line(..., rect.width, line, theme)
       └─ write_styled_line_inner(...)
            for ch in span.text.chars() {
                if col >= max_width { return; }     // ← 就是这里：静默、无标记、无换行
                ...
            }
```

三个事实让这个缺陷值得修：

1. **它影响的是所有扩展 widget**，不止启动头——above/below widget、footer、`custom` overlay
   走的是同一个函数，任何比区域宽的行都会被切。
2. **它是「正常路径」不是边缘情况**：`TextComponent::render(width)` 明确忽略宽度
   （`component.rs:370-373`，`let _ = width;`），扩展作者写一行长文案就会踩到；
   启动头自身折行后也必然超宽（51 字 vs 44 列）。
3. **上游不是这么做的**：上游 TS 走 `truncateToWidth(text, width, "...")`，
   截断本身是**可见**的。Rust 侧连提示都没有。

## 3. 修复

`crates/pi-tui/src/styled.rs`：

- 抽出 `write_styled_line_inner`（`WriteMode { hyperlinks, tail }`），
  `tail` 是 `Tail::Cut`（旧行为）或 `Tail::Mark('…')`（新行为）。
  **只有真的溢出时才写标记**，所以放得下的路径与旧写入器逐 cell 相同——
  宽终端的黄金帧一个字节都不动。
- `clip_keep(line, max_width)`：给定要保留多少字符。先留一列给标记，再取
  **预算内最后一个空白边界**（丢整词而不是留残词）。若该边界会丢掉超过一半预算
  （一个很长的 token 前面只有一个短词），那就退回按列硬切——**仍然是带标记的**。
- 标记的样式取「被它替换掉的那个字符」的样式（`style_at`），所以带主题的 span
  被截断后颜色不丢。
- `CLIP_MARK = '…'`（1 列），不用上游的 `"..."`（3 列）：chrome 行是一行一个，
  44 列里 3 列是 7% 的预算；而且转写区早就用 `…` 表示截断（`* … (+6 lines, Ctrl+O to expand)`），
  status bar 的分片预算也用同一个字符。这是一条**有意的偏离**，写在常量文档注释里。

`crates/pi-tui/src/app.rs`：`paint_extension_lines` 与两处 chrome 标签
（jump-to-latest pill、`truncated above` 提示，二者在 label 比区域还宽时会硬切）改调
`write_styled_line_ellipsized`。高度方向的截断（区域比内容矮）**保持静默**：
那由调用方自己的内容概括，而且在最后一行写 `…` 会和宽度截断混淆（代码注释里写了理由）。

## 4. 验证

### 4.1 单元（`cargo test -p pi-tui --lib styled::`，**12 passed / 0 failed**，新增 8 条）

| 测试 | 钉住什么 |
|---|---|
| `a_fitting_line_is_written_exactly_like_the_plain_writer` | 放得下时**两个 Buffer 逐 cell 相等**（宽终端黄金帧不动） |
| `an_overlong_chrome_row_drops_a_whole_word_and_marks_it` | 44 列黄金串；行宽仍是 44，`…` 在第 41 格，其后全空白 |
| `a_narrow_row_gives_up_whole_words_before_it_cuts_inside_one` | 50/40/30/20/12 五档黄金串（50→`…shows…`，40→丢 session 级整词，12 才开始切词） |
| `a_marked_row_never_overruns_and_never_ends_in_a_fragment` | **1..=120 全宽度**性质：宽度精确、溢出必有标记、不得以 `sho`/`sh` 结尾 |
| `a_single_long_token_is_cut_at_the_edge_and_still_marked` | 只有一个长 token 时退回按列切，仍带标记（不会被清成空行） |
| `the_mark_inherits_the_style_of_the_text_it_replaces` | 标记的 fg 等于被替换字符所在 span 的 fg |
| `a_one_column_row_is_just_the_mark_and_zero_columns_write_nothing` | 1 列 = 只有 `…`；0 列 = 一个 cell 都不写 |
| `the_plain_writer_still_clips_without_a_mark` | **A/B 的另一半**：旧写入器仍然产出 `— Alt+H sho` |

### 4.2 集成（frame 级）

- `crates/pi-tui/tests/extension_ui.rs`（**18/18**，新增 2 条）：
  `an_overlong_extension_row_is_marked_not_silently_cut`（44×14 启动头行 = `… Alt+H…`，
  且断言**不等于**修复前那个串；放得下的行不带标记）、
  `an_overlong_overlay_row_is_marked_too`（`custom` overlay 同一规则）。
- `crates/pi-tui/tests/lum1412_chrome_clip.rs`（**2/2**，新文件）：
  44×14 真布局的断言 + 供截图管线的帧 dump。

### 4.3 全量门禁

| 命令 | 结果 |
|---|---|
| `cargo fmt --all -- --check` | clean |
| `cargo clippy -p pi-tui --all-targets --locked -- -D warnings` | **exit 0 / 0 warning**（`too_many_arguments` 触发过一次，已用 `WriteMode` 收口） |
| `cargo check --workspace --locked` | **exit 0**（唯一 warning 来自 `vendor/rquickjs-core`，第三方，本轮无关） |
| `cargo test -p pi-tui --locked` | **907 passed / 0 failed**，**52 个 target** |
| `cargo test -p pi-tui --locked` @ 基线 `0cb14b63d`（独立 worktree，本轮实测） | **895 passed / 0 failed**，51 个 target → 增量 **+12 passed / +1 target**，与新增测试数一一对应 |
| `cargo test -p pi-coding-agent --locked --test startup_header --test help_text_layout --test print_mode --test keybindings` | **51 passed / 0 failed**（3 + 20 + 17 + 11，与 LUM-1367 的门禁行一致） |
| `cargo test --workspace --locked --no-fail-fast` | **2528 passed / 40 failed / 2 ignored** —— 40 条**全部**是 Windows 平台断言（§4.4） |

### 4.4 全量里的 40 条失败：与本轮无关，且是平台差异

失败集中在 `pi-coding-agent` 的路径 / 导出 / trust / 扩展发现 / 资源加载，抽样：

| 测试 | 失败内容 |
|---|---|
| `paths::tests::absolute_paths_stay_absolute` | `left: "C:\\a\\b"` vs `right: "/a/b"` |
| `commands::export::tests::cli_export_propagates_the_upstream_error_text` | `File not found: C:\definitely\missing.jsonl` vs `/definitely/missing.jsonl` |
| `extensions::js_loader::tests::load_extensions_loads_an_esm_extension_from_disk` | 期望相对 `SKILL.md`，实际是 `C:\Users\ADMINI~1\AppData\Local\Temp\...\SKILL.md` |
| `tools::mod_ignore::tests::absolute_paths_are_rejected` | 期望 `/etc` 被拒，实际被接受 |
| `trust::tests::*`、`resource_loader::tests::*` | 家目录 / 项目资源发现基于 POSIX 路径 |
| `tests/cli_extensions.rs`、`tests/chatinput_chord_conflicts.rs` | 扩展发现与 `$HOME` 布局 |

这些测试在 LUM-1367（Linux runner）上是绿的（那一轮的 workspace 全量卡在磁盘，但同一批 `pi-coding-agent`
测试在其 Linux worktree 上 51/0）。本轮 `pi-tui`（唯一被改动的 crate）**907/0**，
且改动面只有 2 个调用点 + 1 个新函数。

> 取证方式：本轮**没有**跑 Linux 基线做双盲 A/B。要坐实「40 条是 Windows 平台差异」，
> 下一步是在 devbox1（Linux）上跑同一命令；本轮按证据等级如实标注为**「改动面 + 平台特征推断」**，
> 不是「已双盲验证」。

### 4.5 工具坑：共享 `CARGO_TARGET_DIR` 在跨 worktree 时会给假阴性

本轮为了量基线数字，用 `git worktree add` 在**同一个** `CARGO_TARGET_DIR` 上跑了基线树。
回到本轮树再跑时，`crates/pi-tui/tests/lum1412_chrome_clip.rs` 里那条断言**失败**了，
帧里是**修复前**的 `— Alt+H sho`——即测试二进制是新的、链接的 `pi_tui` rlib 是基线那次留下的。
`cargo clean -p pi-tui` 后重跑即 **907/0**。

教训（写给下一个在这台机器上量的轮次）：同一 `CARGO_TARGET_DIR` 上编译两个**同名同版本**的
path 依赖（两个 worktree 的 `pi-tui`），cargo 不会按源码目录分桶，后一次构建会覆盖前一次；
量 A/B 时必须 `cargo clean -p <crate>` 或给基线用独立 target 目录，否则拿到的是
**上一种源码的产物**。本轮的基线数字（895/51）与 LUM-1367 在 Linux 上独立量到的
「895/0、51 个 target」完全一致，两条独立路径互证，故 §1/§4.3 的基线数字可信。

## 5. 本轮 runner 限制：没有真 PTY 截图（如实声明）

`scripts/pty_capture.py` 需要真 PTY（`import pty` / `fcntl` / `termios`）与 `pyte`，
本 runner 是 Windows：`pty` 模块不存在、`pyte` 未安装、`pip` 装不上（pypi 不可达）。
因此本轮**做不到**前几轮那种「真 release 二进制 + 真 PTY + 冻结帧」的截图，
也没有 `expect`/`reject` 的真二进制复核。

本轮的替代证据，等级如实标注：

| 等级 | 内容 | 能证明什么 / 不能证明什么 |
|---|---|---|
| **强** | `styled::` 12 条单元 + `extension_ui` 18 条集成 + `lum1412_chrome_clip` 2 条，全部跑在 `render_snapshot` 上 | 能证明**渲染结果**（对同一 cell grid 精确断言）；不能证明真终端里的逐字节行为 |
| **中** | `docs/screenshots/lum1412-chrome-clip-44x14{,-before}.{txt,png}`，由 `scripts/frame_to_png.py` 从 `render_snapshot` 的 cell grid 渲染 | 能看见 TUI 长什么样；**不是 PTY 截屏**，图上 caption 已写明；无颜色（dump 是文本） |
| **待补** | `scripts/pty_scenarios/lum1412-chrome-clip.json`（44×14，`probe` 新行为 + `reject` 旧残片） | Linux 侧一条命令即可产出真 PTY 证据；本轮**未能执行**，断言用 `probe`（未满足记 XFAIL，不会毒化下一轮门禁） |

Linux 侧复现（下一轮或 devbox1 直接跑）：

```bash
cd pi-rust
cargo build --release -p pi-coding-agent --locked
python3 scripts/pty_capture.py \
  --bin target/release/pi \
  --steps scripts/pty_scenarios/lum1412-chrome-clip.json \
  --out  docs/screenshots/lum1412-chrome-clip-pty-44x14.png
```

新增工具 `scripts/frame_to_png.py`（60 行）：把 `FRAME DUMP` 文本 dump 画成 PNG，
调色板与 `pty_capture.py` 相同。它存在的理由就是本 runner 这类情况
（`docs/FEATURE_PI_RS_STATUS.md` §六 记录过 `--assignee pi` 误命中 Windows 机器）；
文件头明确写了「不是 PTY 截图的替代品」。

## 6. TUI ↔ codex ↔ Martty ↔ 上游 TS：chatinput 到底差在哪

用户口径里「chatinput 差距很大」。把它拆成两层，逐层给结论：

### 6.1 键位层：已对齐（LUM-1360 起，本轮复核未变）

| 轴 | 上游 pi | codex | Martty | pi-rust | 结论 |
|---|---|---|---|---|---|
| `cursorLeft/Right` | `left`/`right` + `ctrl+b/f` | `left`/`right` | `Left`/`Right` `^b`/`^f` | 同上游 | ✅ |
| `cursorWordLeft/Right` | `alt+left/ctrl+left/alt+b` | `alt+b`/`alt+f` | `WordLeft/Right` | 同上游 | ✅ |
| `cursorLineStart/End` | `home`/`end` + `ctrl+a/e` | `home`/`end` | `Home`/`End` | 同上游 | ✅ |
| `historyPrevious/Next` | `[]`（App 层按上下文） | — | `↑/↓`（空 draft） | 同上游 | ✅ |
| `pageUp/Down` | `pageup`/`pagedown` | 同 | `PageUp/Down` | 同上游 | ✅ |
| `deleteCharForward` | `delete`,`ctrl+d` | `delete` | `Delete`/`^d` | 同上游 | ✅ |
| `deleteWordBackward` | `ctrl+w`,`alt+backspace` | `ctrl+w` | `DeleteWordBack` | 同上游 | ✅ |
| `deleteWordForward` | `alt+d`,`alt+delete` | `alt+d` | — | 同上游 | ✅ |
| `deleteToLineStart/End` | `ctrl+u`/`ctrl+k` | 同 | `KillToStart/End` | 同上游 + kill ring | ✅ |
| `yank`/`yankPop` | `ctrl+y`/`alt+y` | 同 | `Yank/YankPop` | 同 | ✅ |
| `undo` | `ctrl+-` | — | — | `ctrl+-`（fish 式按词合并） | ✅ |
| `input.submit`/`newLine` | `enter` / `shift+enter`,`ctrl+j` | 同 | 同 | 同上游 | ✅ |
| `input.copy`（Ctrl+C） | dual-use | `ctrl+c` | `Cancel` | 同上游 dual-use | ✅ |

表格级不变量由 `pi-tui/tests/chatinput_chord_conflicts.rs`（+ `pi-coding-agent` 同名用例）守护：
任何 `app.*` 默认 chord 不得无声吞掉 `tui.editor.*`/`tui.input.*`，除非在 `ALLOWED_OVERLAPS` 里且带理由。

**结论**：键位面 13 个轴全一致。用户感觉到的差距不在键位。

### 6.2 视觉/chrome 层：本轮的贡献与仍然存在的缺口

| 顺位 | 项 | 状态 |
|---|---|---|
| 1 | **chrome 行的截断没有 affordance** | **本轮修掉**（`paint_extension_lines` 家族全量带标记 + 按词边界） |
| 2 | markdown / 消息按列宽折行（LUM1336 §9 第 1 条） | 未动：`message.rs`/`markdown.rs`/`latex.rs`/`terminal_image.rs` 的 wrap 路径，跨 4 模块，风险大 |
| 3 | 第二批扩展事件（LUM1336 §9 第 3 条） | 未动：`pi-protocol/src/events.rs` 15 个缺失变体，实测生产覆盖 20/36 = 55.6% |
| 4 | footer 第二行（cwd / git branch / session name，上游 `footer.ts` 两行布局） | 未动：Rust 侧从来没有 cwd 行，这是**内容**缺口而非视觉截断；动它要牵连 `plan_chrome` 的矮终端回归网 |
| 5 | 聊天消息区的行宽口径（宽字符按 1 列，上游按终端列） | 已知偏离，写在 `visual_text.rs` 模块文档里，是 crate 级 follow-up |

## 7. 完成度（真实百分比）

| 口径 | 数值 | 命令 |
|---|---|---|
| 纯代码规模 | **88.9%** | Rust `crates/*/src/**/*.rs` **136,042** 行 ÷ TS `packages/*/src/**/*.ts` 153,106 行 |
| 测试规模 | **47.8%** | Rust `#[test]`/`#[tokio::test]` **2,536** ÷ TS `*.test.ts` 的 `it(`/`test(` 调用点 **5,309** |
| 测试代码量 | — | Rust `crates/*/tests/**/*.rs` 54,116 行（TS 侧未按同口径计） |
| 功能面加权（13 轴，主口径） | **81.4%** | 继承 `docs/PARITY_AND_TUI_AUDIT_LUM1260.md` §5（本轮不改口径）；若把测试轴 0.44 → 0.478 同一公式给 **81.6%** |
| TUI 交互 id 接线 | **43/44 = 97.7%** | `python3 scripts/app_action_coverage.py --json .`（本轮自跑） |
| 扩展事件生产覆盖 | **20/36 = 55.6%** | `python3 scripts/extension_event_coverage.py --json .`（本轮自跑；tag 21/36 = 58.3%） |

口径声明（沿用 LUM-1367 的三点）：测试百分比对**计数命令**敏感（LUM-1367 用另一条命令得 46.4%，
本轮 47.8%，两者都在 43–48% 区间，**不当作进度**）；13 轴公式继承，未重算权重；
本轮唯一变化的格是「TUI 视觉面 · chrome 截断」从 ✗ 到 ✓，它不落在任何一条加权轴的百分比里
（TUI 轴按 id 接线率计），所以**没有**把 81.4% 改写成更好看的数。

## 8. 产出文件

| 文件 | 说明 |
|---|---|
| `crates/pi-tui/src/styled.rs` | `CLIP_MARK` / `line_width` / `write_styled_line_ellipsized` / `clip_keep` / `style_at` / `WriteMode` / `Tail` + 8 条新单测 |
| `crates/pi-tui/src/app.rs` | `paint_extension_lines` + pill + truncated-above 提示改走带标记的写入器（+ import） |
| `crates/pi-tui/tests/extension_ui.rs` | 2 条新集成用例 |
| `crates/pi-tui/tests/lum1412_chrome_clip.rs` | 新文件：44×14 断言 + 帧 dump 源 |
| `scripts/frame_to_png.py` | 新工具：frame dump → PNG（无 PTY 的 runner 用） |
| `scripts/pty_scenarios/lum1412-chrome-clip.json` | 新场景：44×14 真 PTY 的 `probe`/`reject`（本轮未执行，见 §5） |
| `docs/screenshots/lum1412-chrome-clip-44x14.{txt,png}` | 修复后帧（帧缓冲，非 PTY） |
| `docs/screenshots/lum1412-chrome-clip-44x14-before.{txt,png}` | 修复前同帧（按旧写入器契约推导，单元测试钉住） |
| `docs/LUM1412_CHROME_CLIP.md` | 本文 |

## 9. 并发与后续派活（回答「最多开 3 个任务」）

- 开工时本工作区有 **LUM-1405**（同一条 autopilot 提示词，16:07Z 触发，我 16:30Z）在跑，
  且同为 `编程助手-winpi`（Windows）。两轮同题并发是既成事实，不是本轮制造的。
- 本轮**不再新开子 issue**：同文件面（`pi-tui`）已有 1 路并发，再派只会把合并冲突乘 3；
  这也与 LUM-1366/LUM-1367 两轮的处理一致。
- 下一轮的顺位（§6.2 的表已排序）：**markdown/message 按列宽折行**（顺位 2）是下一个
  单点收益最大的，但它跨 4 个模块，建议单独一轮；顺位 3（扩展事件第二批）与它文件面不重叠，
  可以并行。
- **给下一轮 Windows runner 的提示**：本轮的 `scripts/frame_to_png.py` + `FRAME DUMP` 协议
  可以在没有 PTY 的机器上产出帧缓冲截图；但**必须**像本文 §5 一样标出证据等级，
  不能把帧缓冲截图说成真 PTY 截图。

## 10. 范围之外

- 本轮**未**改 `Cargo.toml` / `Cargo.lock` / `packages/`（TS 侧）/ CI。
- 本轮**未**动 `status.rs`（footer 自己的分片预算上一轮刚做完，本轮只让它与 chrome 用同一个标记字符）。
- 本轮**未**重拍 `docs/screenshots/*.png` 里的存量图；它们是在 Linux 上拍的，
  本轮改动对「放得下」的行是逐 cell 相同的，但**没有**逐图复核（如实标注为未复核）。
- 全量 workspace 的 40 条 Windows 平台失败（§4.4）**未修**：它们不在本轮 scope，
  且需要 POSIX 路径语义，属于平台差异而非缺陷。
