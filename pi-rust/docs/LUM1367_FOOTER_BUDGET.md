# LUM-1367 — 窄视口 footer 预算重写 + TUI / chatinput 真实审计

> scope: `pi-rust/`；base = `origin/feature/pi.rs` tip `3f1197543`（LUM-1366 之后）
> branch: `work/LUM-1367`（→ `feature/pi.rs`）
> 本轮**改代码**（不是一个只写审计的 round）：`crates/pi-tui/src/status.rs` 的 footer 布局
> 证据：真 PTY（pyte）+ 真 release 二进制 + `scripts/pty_scenarios/lum1367-narrow-footer.json` 的 A/B

## 1. TL;DR

- 用户口径里「现在的 tui chatinput 实现和 codex / Martty 差距很大」这句，我按**可复核的方式**查了一遍：
  **chatinput 的 chord 矩阵已经对齐**（§5 的 11 行表逐条复核，含 LUM-1360 修掉的 `alt+f` 遮蔽）；
  真正还差得远的是**输入框旁边的 chrome 在窄视口下的截断**，本轮按优先级把最扎眼的那一个修掉了。
- 缺陷（实测、可复现）：footer 在宽度不足时按**出现顺序硬截**，把右侧分片的**前缀**留在行尾。
  真二进制 44×14 的一帧画的是
  `Faux test model  session-18d79d8eb888c4fc  i` —— `in 0 out 0` 的**第一个字符**孤零零挂在右边缘。
- 修复：footer 改为**双布局**。整行放得下 → 原样（`docs/screenshots` 里 60/72/80 三列逐字节不变）；
  放不下 → 按 `NARROW_SACRIFICE_ORDER` **整片丢弃**（hint → cache → session → usage → gauge → model），
  并把「这是一部分」用行尾 `…` 说出来。任何分片都不会再被从中间切开。
- 验证：44×14 新场景 **7/7 PASS**，同一场景在**修复前的二进制**上 **3 条硬 FAIL**（真 A/B）；
  单元测试 22/22（含 1..=120 全宽度的「不得以残缺分片结尾」性质测试 + 全宽度 themed/plain 一致）；
  门禁见 §7。
- 完成度：见 §6。规模口径自测 **代码 88.6% / 测试 46.4%**；功能面加权 **81.4%**（继承 LUM-1260 的 13 轴公式，
  本轮不改口径），TUI 交互+视觉轴 **83.7%**。本轮**没有**改动这些口径，只把「TUI 交互」里的一格证据补实。

## 2. 缺陷：实测（修复前，真 PTY + 真 release 二进制）

复现命令（`--model faux/faux-model`，14 行终端，footer 是最后一行）：

```bash
cd pi-rust
python3 - <<'EOF' > /tmp/n.json
import json
print(json.dumps({"cols":44,"rows":14,"args":["--model","faux/faux-model"],
  "panels":[{"label":"idle","send":"","wait":1.2,"wait_for":"type a prompt"}]}))
EOF
python3 scripts/pty_capture.py --bin <binary> --steps /tmp/n.json --out /tmp/n.png --text-out /tmp/n.txt
tail -1 /tmp/n.txt
```

修复前（`3f1197543` 的 release 二进制，md5 `7491a83910b0dee06b2f8080d55ac44a`）：

| cols | footer 实测（修复前） | 问题 |
|---|---|---|
| 40 | `Faux test model  session-18d79d8c5bca3a9` | session id 被切掉最后一位（`8c5bca3a9` 而不是 16 位 hex） |
| 44 | `Faux test model  session-18d79d8eb888c4fc  i` | 行尾留下 `in 0 out 0` 的第一个字符 |
| 46 | `Faux test model  session-18d79d910402623f  in` | 行尾留下 `in` |
| 52 | `Faux test model  session-18d79d9351d01bd0  in 0 out` | 行尾留下 `in 0 out` |
| 60 | `Faux test model  session-18d79d959ae2ac22  in 0 out 0 ?/8.2k` | 恰好放得下：hint 被无声丢掉，无任何提示 |
| 70 | `Faux test model  session-18d79d97e37c723f  in 0 out 0 ?/8.2k  ? for he` | hint 只剩 `? for he` |
| 72 | `…  ? for help` | 整行刚好 72 列：正确 |
| 80 | `…          in 0 out 0 ?/8.2k  ? for help` | 正确（右对齐补白） |

根因：`status.rs::render_styled_line` 把 `left / session / right` 按**视觉顺序**逐个 `clip` 到剩余预算，
预算耗尽就把后面的字节丢掉。信息层面「丢」是对的，但它**丢在分片中间**，于是产生了没有意义的残片——
`i`、`in`、`in 0 out` 既不是 token 也不是提示，只是噪声，而且用户无从知道右边还有内容。

上游 TS 的 footer（`packages/coding-agent/src/modes/interactive/components/footer.ts:160-210`）不是这么做的：
`statsLeft` 整体用 `truncateToWidth(statsLeft, width, "...")` 截断，右侧 model 单独截断，
并且 footer 是**两行**（pwd 行 + stats 行）。Rust 侧是有意的单行简化，本轮不改成两行（会动
`plan_chrome` 的高度分配，牵连 `docs/PARITY_AND_TUI_AUDIT_LUM1260.md` §3 的矮终端修复），
只把「截断怎么截」按上游的语义对齐：**整段语义单元 + 明确的截断标记**。

## 3. 修复：footer 的双布局

`crates/pi-tui/src/status.rs`：

- `render_styled_line` 先算整行宽度。`left + session + right <= width` → **fitted 布局**，就是原来那段
  代码（右对齐补白），所以**宽终端逐字节不变**（§4 的 60/72/80 列对照）。
- 否则 → `narrow_layout`：footer 被拆成 7 个 `Zone`（`Busy / Model / Session / Usage / Cache / Gauge / Hint`），
  按 `NARROW_SACRIFICE_ORDER` **整片丢弃**，直到剩下的放得下为止；丢掉过内容就补行尾 `…`。
- 每个 `Zone` 自带 `lead`（与前一片之间的分隔），它们的**累加恰好等于** fitted 布局的分隔字符串，
  于是「刚丢了一片」的 bar 不会把留下的分片重新排版；`narrow_width()` 与 `emit_zones()` 对
  任意子集算出的宽度严格一致（这就是为什么丢弃循环能精确收敛）。
- 唯一还会被切开的情况是「最后只剩一片、而它自己就比终端还宽」（模型名比终端宽）：那时裁到
  `width-1` 再补 `…`，bar 永远不会越过自己的列预算。
- 标记用 `…`（1 列）而不是上游的 `"..."`（3 列）：footer 只有一行，44 列时 3 列就是 7% 的预算；
  本 crate 在 transcript 里已经用 `…` 表示截断（`* … (+6 lines, Ctrl+O to expand)`）。这是**有意的偏离**，
  写在 `ELLIPSIS` 常量的文档注释里。

丢弃顺序（`NARROW_SACRIFICE_ORDER`）与理由，都写在代码注释里；一句话版：

| 顺序 | 分片 | 为什么先丢 |
|---|---|---|
| 1 | `Hint`（`? for help`） | 天生是临时的；启动头已经把同一批键位列过一遍 |
| 2 | `Cache`（`R… W…`） | 是旁边 usage 总数的细节，不是独立总量 |
| 3 | `Session`（id/name） | 最宽的一片（26 列），且调用方通常自己知道是哪个 session |
| 4 | `Usage`（`in … out …`） | 累计成本，比身份更值得留 |
| 5 | `Gauge`（`42.0%/128k`） | 唯一会改变决策的数字，留到最后 |
| 6 | `Model` | 最短的一片，且它说明回答来自哪个模型 |
| — | `Busy`（spinner + 秒数） | **不丢**：它是「turn 还在跑」的唯一信号，codex / Martty 在窄视口下也都保留活动指示 |

## 4. 修复后实测

同一命令、同一场景、同一数据（新 release 二进制）：

| cols | 修复前 | 修复后 | 结论 |
|---|---|---|---|
| 40 | `…session-18d79d8c5bca3a9` | `Faux test model  in 0 out 0 ?/8.2k…` | 丢掉最长且最可省的一片，换回**完整**的三组数字 + 截断标记 |
| 44 | `…  i` | `Faux test model  in 0 out 0 ?/8.2k…` | 残片消失 |
| 46 | `…  in` | `Faux test model  in 0 out 0 ?/8.2k…` | 同上 |
| 52 | `…  in 0 out` | `Faux test model  in 0 out 0 ?/8.2k…` | 同上 |
| 60 | `… ?/8.2k` | `… ?/8.2k`（逐字节相同） | 恰好放得下时**行为不变** |
| 70 | `… ?/8.2k  ? for he` | `… ?/8.2k…` | 丢 hint（它只剩 2 个字符，信息量为 0）并标记 |
| 72 | `… ? for help` | `… ? for help`（逐字节相同） | 刚好放得下 |
| 80 | 右对齐补白 | 逐字节相同 | 宽终端不受影响 |

真 PTY 场景 A/B：

- `scripts/pty_scenarios/lum1367-narrow-footer.json`（44×14，2 panel，4 expect + 3 reject）
  - 修复后二进制：**7/7 PASS**（`PASS expect` ×4、`PASS reject` ×3）
  - 修复前二进制（`7491a839…`）：**3 条硬 FAIL**（
    `expect 'Faux test model  in 0 out 0 ?/8.2k…'`、
    `reject '  (i|in|in 0 out)\s*$'`、
    `reject 'session-[0-9a-f]{16}'`）——即这个场景是**能抓住旧缺陷的回归网**，不是事后补的断言。
- 截图：`docs/screenshots/lum1367-narrow-footer-44x14.png`（修复后）与
  `…-before.png`（修复前），各自的 `.png.txt` 是可 grep 的同帧文本。

单元测试（`cargo test -p pi-tui --lib status::`，**22/22**），新增 6 条：

1. `a_fitting_bar_is_unchanged_and_exactly_width_wide` —— 72/80/120 三档黄金字符串（含中段右对齐补白）。
2. `a_narrow_bar_drops_whole_parts_instead_of_cutting_them` —— 60/61/70/52/46/40/34/30/22/21/15 十一档黄金字符串。
3. `a_narrow_bar_never_ends_with_a_partial_token_at_any_width` —— **1..=120 全宽度**性质测试：
   每档必须填满预算，且不得以 `i` / `in` / `in 0 out` / `? for he` / `? for h` / `? for` 结尾
   （这 6 个就是 §2 表格里实测到的残片）。
4. `the_busy_spinner_outlives_the_model_when_columns_run_out` —— turn 在跑时先丢模型、不丢 spinner+秒数。
5. `a_model_wider_than_the_bar_is_clipped_and_marked` —— 唯一还会被切开的情形。
6. `a_budgeted_bar_is_laid_out_by_the_same_span_rules_as_a_fitting_one` —— 1..=120 全宽度
   `render_themed` 去 ANSI 后必须等于 `render`（themed 路径不能和 plain 路径分叉）。

## 5. chatinput：和 codex / Martty / 上游 TS 到底差多少（真实复核）

LUM-1360 把 chatinput 的 chord 矩阵钉住之后，我在 `3f1197543` 上**重新逐条复核**了一遍
（`pi-tui/src/keybindings.rs` 的 `TUI_KEYBINDINGS` vs `packages/tui/src/keybindings.ts` 的 `TUI_KEYBINDINGS`，
逐 id 逐 chord 对齐；`app.*` 默认 chord 不得无声吞掉 `tui.editor.*` / `tui.input.*` 的表格级不变量
由 `crates/pi-tui/tests/chatinput_chord_conflicts.rs` 守护）：

| 轴 | 上游 pi (keybindings.ts) | codex (keymap.rs) | Martty (input/keymap.rs) | pi-rust | 结论 |
|---|---|---|---|---|---|
| `cursorLeft/Right` | `left`/`right` + `ctrl+b`/`ctrl+f` | `left`/`right` | `Left`/`Right` `^b`/`^f` | 同上游 | ✅ |
| `cursorWordLeft/Right` | `alt+left`,`ctrl+left`,`alt+b` / `+alt+f` | `alt+b`/`alt+f` | `WordLeft`/`WordRight`（`⌥←/→`、`esc-b/f`） | 同上游 | ✅ |
| `cursorLineStart/End` | `home`,`ctrl+home`,`ctrl+a` / `end`,`ctrl+end`,`ctrl+e` | `home`/`end` | `Home`/`End` + `^a`/`^e` | 同上游 | ✅ |
| `historyPrevious/Next` | `[]`（空，由 App 层按上下文接） | — | `↑/↓`（**空 draft 时**才是 history） | 同上游（空 draft 时 `↑/↓`） | ✅ 语义一致 |
| `pageUp/Down` | `pageUp`,`ctrl+pageUp` / `pageDown`,`ctrl+pageDown` | `pageup`/`pagedown` | `PageUp`/`PageDown` | 同上游 | ✅ |
| `deleteCharForward` | `delete`,`ctrl+d` | `delete` | `Delete`/`^d`（dual-use） | 同上游 | ✅ |
| `deleteWordBackward` | `ctrl+w`,`alt+backspace` | `ctrl+w` | `DeleteWordBack` + `⌥⌫` | 同上游 | ✅ |
| `deleteWordForward` | `alt+d`,`alt+delete` | `alt+d` | — | 同上游 | ✅ |
| `deleteToLineStart/End` | `ctrl+u` / `ctrl+k` | `ctrl+u` / `ctrl+k` | `KillToStart`/`KillToEnd`（空 draft 时 `^u/^d` 变滚动） | 同上游 + kill ring | ✅ |
| `yank` / `yankPop` | `ctrl+y` / `alt+y` | 同 | `Yank`/`YankPop` | 同 | ✅ |
| `undo` | `ctrl+-` | — | — | `ctrl+-`（fish 式按词合并） | ✅ |
| `input.submit` / `newLine` | `enter` / `shift+enter`,`ctrl+j` | `enter` / `shift+enter`,`ctrl+j` | `Enter` / `shift+Enter` | 同上游 | ✅ |
| `input.copy`（Ctrl+C） | `ctrl+c`（空 buffer=`app.exit`，有 draft=`app.clear`） | `ctrl+c` | `CtrlC`(cancel) | 同上游 dual-use | ✅ |

结论：**chatinput 的键位面没有「差距很大」**——11 个轴里 11 个一致，`app.*` 与 editor chord 的抢占
由测试守护。用户感觉到的「差距」更可能来自视觉面（footer / header / 消息区的窄视口截断），
而这一轮我在同一层里实测到了确凿的一例（§2）并修掉了。

**仍然没对齐、已排队（按性价比）**：

| 顺位 | 项 | 范围 | 证据 / 影响 | 风险 |
|---|---|---|---|---|
| 1 | **chrome 的截断没有 affordance** | `app.rs::paint_extension_lines`（→ `styled.rs::write_styled_line` 在 `max_width` 处**直接停止写入**） | 44×14 实测 header 行画成 `hints hidden on a short terminal — Alt+H sho`（应 50 列，被切成 44）。同一函数还画 header / above / below / footer widgets，所以所有扩展 widget 都有这个毛病 | 中：函数是共享写入器，`crates/pi-tui/tests/*` 里有 20/24/30/48/60 等多档窄宽度快照断言，需要逐条复核 |
| 2 | markdown / message 按列宽折行（LUM1336 §9 第 1 条） | `message.rs` `markdown.rs` `latex.rs` `terminal_image.rs` 的 wrap 路径 | 短视口下 markdown/代码块/引用块溢出；信息密度 | 大（跨 4 模块） |
| 3 | 第二批扩展事件（LUM1336 §9 第 3 条） | `pi-protocol/src/events.rs` 的 14 个缺失变体 + 各自生产构造点 | 实测 20/36 可订阅事件真有构造点（55.56%），缺的恰好是插件最常用的 `before_agent_start` / `context` / `session_before_*` | 中 |
| 4 | footer 的第二行（pwd / git branch / session name，上游 `footer.ts` 的两行布局） | `status.rs` + `extension_ui.rs::plan_chrome` 的高度分配 | Rust 侧从来**没有过** cwd 行，这是相对上游/`codex` 的真实内容缺口 | 大：动 `plan_chrome` 会牵连矮终端修复（LUM-1260 §3）的回归网 |

## 6. 完成度（真实百分比）

口径分两类，**分别标注来源**；本轮不改口径，只补证据。

### 6.1 规模口径（本轮自测，命令可复现）

| 口径 | 数值 | 命令 |
|---|---|---|
| 纯代码规模 | **88.6%** | Rust `crates/*/src/**/*.rs` 135,664 行 ÷ TS `packages/*/src/**/*.ts` 153,106 行 |
| 测试规模 | **46.4%** | Rust `#[test]`+`#[tokio::test]` 2,524 ÷ TS `*.test.ts` 里 `it(`/`test(` 调用点 5,443 |
| 测试代码量 | — | Rust `crates/*/tests/**/*.rs` 53,892 行（TS 侧未按同口径计） |

（对照：`docs/RUST_TS_PARITY_METRICS.md` §0.1 在 tip `d90555fb6` 上量到「代码 82.1% / 测试 43.4%」，
本 tip 代码量涨到 135,664 行、TS 基数未变，所以代码口径升到 88.6%。测试口径两轮都在 43–47% 区间，
差别来自计数命令，**不当作进度**。）

### 6.2 功能面口径（继承，本轮未重算）

| 口径 | 数值 | 来源 |
|---|---|---|
| 功能面加权（13 轴，主口径） | **81.4%** | `docs/PARITY_AND_TUI_AUDIT_LUM1260.md` §5 的 13 轴公式（`app.*` 接线轴用实测 79.5% 重算） |
| TUI 交互+视觉轴 | **83.7%** | `(14×0.795 + 8×0.91) / 22` |
| `app.*` 接线率 | **43/44 = 97.7%** | 本轮自跑 `python3 scripts/app_action_coverage.py --json .` |
| 扩展事件生产覆盖率 | **20/36 = 55.6%** | 本轮自跑 `python3 scripts/extension_event_coverage.py --json .` |

一句话：**规模上 Rust 是 TS 的近九成，测试覆盖只有不到一半，功能加权约八成**；
剩下的差距集中在「扩展生态的生命周期事件」和「TUI 视觉面的窄视口处理」，而不在 agent 核心。

## 7. 门禁（Rust 1.85.0，离线）

```
PATH 上的 cargo 是坏 wrapper（~/.local/bin/cargo → 不存在的 /tmp/cargo-home/bin），
沿用 pi-rust/scripts/toolchain.sh 的策略：把
/home/devbox/.rustup/toolchains/1.85.0-x86_64-unknown-linux-gnu/bin 放 PATH 最前，
并设 CARGO_INCREMENTAL=0（共享卷，incremental 目录会吃掉 GB 级空间）。
```

| 命令 | 结果 |
|---|---|
| `cargo fmt --all -- --check` | clean |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | **0 warning / 0 error**（`CLIPPY_EXIT=0`，日志里 `^warning`/`^error` 计数为 0） |
| `cargo build --release -p pi-coding-agent --locked` | 成功（186 crates）；最终二进制 md5 `ae408f51b9b472713ede9fa16133963a` |
| `cargo test -p pi-tui --lib status::` | **22 passed / 0 failed** |
| `cargo test -p pi-tui --locked` | **895 passed / 0 failed**，51 个 target（lib + 45 个集成测试，含 `tests/` 里 20/24/30/48/60 列的窄宽度快照断言——它们**没有**被本轮的 footer 改动打破） |
| `cargo test -p pi-coding-agent --locked --test keybindings --test startup_header --test help_text_layout --test print_mode` | **51 passed / 0 failed**（3 + 20 + 17 + 11） |
| `cargo test --workspace --locked --no-fail-fast` | **未完成（环境）**，见 §7.1 |
| 真 PTY `lum1360-chatinput-audit.json`（最终二进制） | **16/16 PASS**——chatinput 面未被本轮影响 |
| 真 PTY `lum1367-narrow-footer.json`（最终二进制） | **7/7 PASS**；同一场景在修复前二进制上 3 FAIL（§4） |

### 7.1 环境限制（如实记录）

共享 50G 卷在本轮被打满多次（`No space left on device (os error 28)`），样子是：

- `cargo test --workspace` 在 `pi-ai` / `pi-client` / `pi-server` / `pi-coding-agent` 多个 target 上
  `couldn't create a temp dir ... rmeta...` 而编译失败；
- `pi-coding-agent/tests/keybindings.rs` 有 8 条用例 panic，但 panic 载荷是
  `temp dir: Custom { kind: StorageFull, ... }`——**不是断言失败，是磁盘**；空间恢复后同一命令
  `CA_EXIT=0`、全绿（§7 表格里那行 51 passed 就是这次）。

清理动作：删掉自己 worktree 的 `target/debug/incremental`（1.8G）与 `target/debug`（~10G，可重建，
release 目录保留），以及 `lum-1360` / `lum-1366` 两个**已完成轮次**的陈旧 `target`（738M + 5.9G）。

`docs/RUST_TS_PARITY_METRICS.md` §0.3 记录过完全相同的现象（当时 4–5 条并行 run 同时在构建），
根因相同：这套 workspace 上的并行 run 数 > 卷容量能承受的并行度。
**这条限制不影响本轮结论**：本轮只改了一个 crate，该 crate 的 51 个 target 全绿，
外加真实二进制上的 16/16 + 7/7 PTY 断言。

## 8. 范围之外 / 给下一个 round 的话

- 本轮**只改** `crates/pi-tui/src/status.rs`（+ 新场景 JSON + 截图 + 本文）。
  没碰 `Cargo.toml` / `Cargo.lock` / `packages/`（TS 侧）/ CI。
- 本轮**没有**新开子 issue。原因写在 issue 评论里：`LUM-1366`（同一 autopilot 提示词，09:30，
  已 in_review）与 `LUM-1369`（同一提示词，10:00，todo）正在同一条 `feature/pi.rs` 上，
  再开 3 条只会把合并冲突乘 3。§5 的表已经按性价比排好，下一个 round 直接从顺位 1 开工即可。
- 顺位 1（chrome 截断 affordance）的具体位置已经定位到函数级：`app.rs::paint_extension_lines`
  → `styled.rs::write_styled_line`（在 `max_width` 处 `return`，既不换行也不标记）。
  需要一并处理的是 `crates/pi-tui/tests/` 里 20/24/30/48/60 列的窄宽度快照断言。
