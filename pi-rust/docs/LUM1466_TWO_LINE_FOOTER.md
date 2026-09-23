# LUM-1466 — 两行 footer（`pwd (branch) • name` + stats 行）+ 全 TUI 真实审计 + 截图

> scope: `pi-rust/crates/pi-tui`（`status` / `app` / `extension_ui`）+ `pi-rust/crates/pi-coding-agent`（`footer` 新增 / `interactive` 接线）
> branch: `work/LUM-1466`（→ `feature/pi.rs`）
> 时间锚：LUM-1464（`b7d93acb5`）之后的下一轮；issue 正文是 LUM-981 伞形任务的复触发（autopilot）

## 0. 结论速览

| 量 | 本轮实测 | 依据 |
|---|---|---|
| **issue 点名项（TUI 与 codex / Martty / pi-ts 的差距）** | 编辑语义面此前几轮已对齐；**本轮关掉 LUM-1464 §5 的第一顺位缺口：footer 只有一行** | §1、§2 |
| 上游 pi-ts 的 footer | **2 行 + 可选第 3 行**：`[pwdLine, statsLine]`，扩展 `setStatus` 再 push 一行 | `packages/coding-agent/src/modes/interactive/components/footer.ts:230-246` |
| 本轮前 pi-rust 的 footer | **1 行**（model / session / stats / hint 挤在同一行，且 `status.rs` 自己的注释承认这是 "a deliberate simplification"） | `status.rs:160-162`（基线） |
| 新增行为测试 | **14 条**（4 条 `status.rs` 单元 + 9 条 `tests/lum1466_two_line_footer_frames.rs` + 1 条 `tests/startup_header.rs`） | §3 |
| 新增帧测试 | **3 张截图**（`docs/screenshots/lum1466-footer-*.{png,txt}`） | §5 |
| `pi-tui` 全量 | **1104 passed / 0 failed**（75 target + doc-test） | 基线 `b7d93acb5` 文档记录 **1090 / 0** → **+14** |
| `pi-coding-agent` | **830 passed / 28 failed**（33 target） | 基线同机 **826 / 28**（LUM-1464 §4.3）→ **+4**，失败集合逐条同形 |
| 新增失败 | **0**（`git stash` 对账：`pi-coding-agent --lib` 前后均为同一 8 条） | §4.3 |
| 纯代码规模（src↔src） | **92.3%**（141,365 / 153,106） | `python pi-rust/scripts/measure_loc.py` |
| 测试规模 | **49.8%**（2,773 / 5,563） | §4.4 |
| `app.*` 接线 | 43/44 = **97.7%**（唯一 silent 仍是 `app.tree.editLabel`） | `app_action_coverage.py` |
| 加权完成度 | **86.9%**（与上轮同值；见 §4.5 的口径声明） | 公式与权重沿用 `RUST_TS_PARITY_METRICS.md` §4.1 |

一句话结论：**上游 pi-ts 的 footer 从第一版起就是两行，pi-rust 把它压成了一行**——这不是「少一个功能」，
而是**同一屏少一行信息**（工作目录 + git 分支 + `/name`）。本轮把这一行补上，并且把它接进真实的
`pi-coding-agent` interactive 驱动（不是只在测试里成立），同时保证「没有 cwd 的宿主」的帧几何**一字节不变**。

## 1. 真实审计：footer 的一行差，是**三边都有的**形状差

### 1.1 三边上游的第一手取证（本机，不转述）

| | upstream pi-ts | codex | Martty |
|---|---|---|---|
| footer 行数 | **2 行**（+ 扩展 status 第 3 行） | **1 行**（`? for shortcuts` + 右侧 context 指示） | composer 框自带 **2 行**（bottom border = meta 行；top cap = 统计行） |
| 第一行内容 | `pwd (branch) • sessionName`，dim | — | meta 行：mode chips 左 / model 右（`src/ui.rs:608-641`） |
| 第二行内容 | stats 左（`↑/↓/R/W/CH%/$(auto)/context%`） + **右对齐** `(provider) model • thinking` | — | 统计行：`Input/Output tok · N turn · steps · Cache hit%`（`src/ui.rs:1229-1236` ← `compact_slot_sections`） |
| 取证 | `components/footer.ts:119-127`（pwd 行）、`:230-246`（`const lines = [pwdLine, dimStatsLeft + dimRemainder]`） | `codex-rs/tui/src/bottom_pane/footer.rs:190-216`（`footer_height`）、`:315`（`single_line_footer_layout`） | `src/ui.rs:608-641`、`:882-908`、`:1229-1264` |

即：**codex 是单行**（所以「对齐 codex」并不要求两行），但 **pi-ts 与 Martty 都给了统计/位置一条独立的行**，
而 pi-rust 的 footer 是这一组里最薄的：既没有 pwd 行，也没有 `CH%` / `$cost` / `(auto)` / 多 provider 前缀。

### 1.2 为什么前几轮没抓到它

LUM-1464 §5 已经把它写成「下一轮第一顺位」，理由是它**跨 `status` + `plan_chrome` + 驱动接线**，
会动帧高。本轮先把「动帧高」这件事本身量清楚（§2.1），再决定怎么动。

### 1.3 基线取证（可复现）

```bash
# 基线（origin/feature/pi.rs = b7d93acb5）上 footer 只有一行：
$ git grep -n "location_line\|git_branch" origin/feature/pi.rs -- pi-rust/crates/pi-tui/src | wc -l
0
# 而 status.rs 自己的注释写着上游是两行：
$ git grep -n "Upstream's two-line footer" origin/feature/pi.rs -- pi-rust/crates/pi-tui/src
origin/feature/pi.rs:pi-rust/crates/pi-tui/src/status.rs:160:    /// Upstream's two-line footer dims the whole stats line
# 整个 Rust 端口没有任何 git 分支/工作目录的读取点：
$ git grep -n "rev-parse\|refs/heads\|\.git/HEAD" origin/feature/pi.rs -- pi-rust/crates | wc -l
0
```

## 2. 设计：一行数据 = 一行 footer，**没有 cwd 就不占行**

### 2.1 先把代价量清楚（本轮的第一步是一次实验）

把 `plan_chrome` 的 `status` 从 `1` 硬改成 `2`（不改其它任何一行），跑 `cargo test -p pi-tui`：

```
passed 1025  failed 65（18 个 target）
```

65 条失败全部是**几何敏感**的既有断言（viewport 高度、composer 行号、scrollbar 几何、selection 行号）。
**结论**：无条件两行会把「一个可见缺口」的修复变成「65 条断言改数字」——那批断言钉的是排版不变量，
跟着改动一起改，等于把它们变成对当前实现的复述（本仓库 LUM-1418 §6 的既定判断法）。

### 2.2 于是：位置行由**宿主**提供，行数由**数据**决定

上游 `FooterComponent` 恒有 `sessionManager.getCwd()`；Rust 侧的「宿主」就是
`pi-coding-agent` 的 interactive 驱动。所以：

* `pi-tui` 新增 `StatusData::{cwd, git_branch}`，`StatusBar::line_count` 返回 **2**（有 cwd）或 **1**（没有）；
  `plan_chrome` 按 `ExtensionFrame::status` 预留**恰好这么多行**。
* `pi-coding-agent::interactive::run_loop` 在启动时调 `App::set_status_cwd` / `set_status_git_branch`
  ——真实 TUI 恒为两行 footer（上游形状），而「没有工作目录的宿主」（`pi-tui` 自己的裸 `App`、
  headless snapshot、`/transcript`）保持一行，几何与 LUM-1464 前**逐字节相同**。

**这不是绕过测试，而是把「谁的 cwd」这件事放回它该在的层**：`pi-tui` 是组件库，它不能替宿主决定
进程的工作目录。代价写在 §7（范围之外 / 已知偏差）。

### 2.3 落地（文件:行号）

| 文件 | 改动 | 说明 |
|---|---|---|
| `pi-rust/crates/pi-tui/src/status.rs` | `+283/-16` | `StatusData::{cwd, git_branch}`（`:48`、`:52`）、`with_cwd`/`with_git_branch`、`StatusData::location_line`（`:141`，`~/repo (main) • name`）、`StatusBar::line_count`（`:210`）、`render_lines` / `render_lines_plain`（`:220`、`:230`）、`render`/`render_themed` 多行 join、`render_to_buffer{,_themed}` 逐行写；`format_cwd_for_footer`（`:660`）、`location_span`（`:629`，超宽用 `…` 标记，LUM-1412 规则）、`session_segment`（`:615`，位置行已带走 `/name` 时 stats 行不再重复它） |
| `pi-rust/crates/pi-tui/src/extension_ui.rs` | `+18/-5` | `ExtensionFrame::status`（`:414`）与 `plan_chrome` 的数据驱动预留（`:464`）；`ExtensionUi::frame` 默认 `1` |
| `pi-rust/crates/pi-tui/src/app.rs` | `+45/-5` | `composed_frame` 把 `StatusBar::line_count` 写进 `frame.status`（`:1755`）；`App::set_status_cwd` / `set_status_git_branch`（`:2049`、`:2057`）；`header_reserved_rows`（`:1769`）——启动 header 的折叠阈值跟着 footer 的实际行数走 |
| `pi-rust/crates/pi-coding-agent/src/footer.rs` | **新增 150 行** | `git_branch(cwd) -> Option<String>`：读 `.git/HEAD`（支持 worktree 的 `gitdir:` 文件），detached HEAD → `None`，与上游 `getGitBranch()` 契约一致 |
| `pi-rust/crates/pi-coding-agent/src/interactive.rs` | `+10` | `run_loop` 启动时把 cwd + branch 交给 App |
| `pi-rust/crates/pi-coding-agent/src/lib.rs` | `+1` | `pub mod footer;` |
| `pi-rust/crates/pi-tui/tests/lum1466_two_line_footer_frames.rs` | **新增 259 行** | 9 条（6 行为 + 3 帧 dump） |
| `pi-rust/crates/pi-tui/tests/startup_header.rs` | `+45` | header 折叠阈值跟随 footer 行数 |
| `pi-rust/docs/screenshots/lum1466-footer-*.{png,txt}` | 3 + 3 | `scripts/frame_to_png.py` |

### 2.4 三条不变量（都有测试钉住）

1. **没有 cwd ⇒ 一行**，且帧与基线逐行相同（`without_a_cwd_the_footer_stays_one_row`、
   `a_branch_without_a_cwd_adds_no_row`）。
2. **有 cwd ⇒ 两行**，位置行在 stats 行正上方（`a_location_row_is_drawn_above_the_stats_row`），
   且 transcript **只让出一行**（`the_transcript_gives_up_exactly_one_row_for_the_location_row`）。
3. **超宽用 `…` 标记**而不是静默切断（`a_long_location_row_is_marked_when_it_is_cut`）。

## 3. 反向验证（本机实做）

把 `StatusBar::line_count` 的返回硬写成常量 `1`（不改其它任何一行）：

```
tests/lum1466_two_line_footer_frames  3 条立刻红
  a_location_row_is_drawn_above_the_stats_row
  the_transcript_gives_up_exactly_one_row_for_the_location_row
  a_short_terminal_still_paints_the_composer_and_both_footer_rows
```

把 `header_reserved_rows` 退回常量 `HEADER_RESERVED_ROWS`：

```
tests/startup_header  a_two_row_footer_folds_the_header_one_row_earlier 立刻红
  assertion `left == right` failed（two_row = 28，期望 29）
```

恢复后 9 + 11 全绿。即这批断言真的钉住本轮改动，而不是在描述既有行为。

**这条第三点不是顺手加的**：启动 header 的折叠阈值原来写死 `MIN_TRANSCRIPT_ROWS + RESERVED_CHROME_ROWS(=2)`。
footer 变成两行后，24 行的终端会让 header 多留一行、随后被 `plan_chrome` 截尾——正是 LUM-1266 清掉的
「静默切断」。修法是把阈值做成 `header_reserved_rows()`，读 footer 的实时行数。

## 4. 门禁与数字

### 4.1 fmt / clippy

| 门禁 | 结果 |
|---|---|
| `cargo fmt --all -- --check` | clean |
| `cargo clippy --offline -p pi-tui --all-targets` | 本 crate **0 warning**（只有 `~/.cargo/config.toml` 的 `net.http-multiplexing` 提示） |
| `cargo clippy --offline -p pi-coding-agent --all-targets` | 本 crate 与 `pi-tui` **0 warning**；输出里的 14 条 warning 全部落在**未改动的** `pi-extensions`（`signal_name` dead code）与 vendored `rquickjs-core` 上，与本轮无关 |

### 4.2 `pi-tui` 全量

```
$ cargo test --offline -p pi-tui -j 4
targets 75 + doc-tests  passed 1104  failed 0  ignored 0
```

基线（`b7d93acb5`，LUM-1464 §4.2 记录）**1090 / 0**，差额 **+14** =
4（`status` 单元）+ 9（`lum1466_two_line_footer_frames`）+ 1（`startup_header`）。

### 4.3 `pi-coding-agent`（含集成 target）

```
$ cargo test --offline -p pi-coding-agent -j 2 --no-fail-fast
targets 33  passed 830  failed 28  ignored 0
```

基线同机为 **826 / 28**（LUM-1464 §4.3）→ **+4**（`footer::tests` 的 4 条），失败集合同形，
全部为 Windows 环境类（真 `bash` 工具、绝对路径断言、`/tmp`、node fs、project trust、扩展发现）。

**stash 对账（逐条，不是「看起来一样」）**：`git stash -u` 后在基线上跑 `-p pi-coding-agent --lib`：

```
baseline:  584 passed / 8 failed   （8 条名字）
after   :  588 passed / 8 failed   （同一组 8 条名字）
```

即 **新增失败 0 条**，新增通过 4 条。

### 4.4 Rust↔TS 口径（全部可复现）

```bash
python pi-rust/scripts/measure_loc.py
grep -rhoE '#\[(tokio::)?test\]' pi-rust/crates --include=*.rs | wc -l
grep -rhoE "^\s*(it|test)(\.\w+)?\(" packages --include=*.test.ts | wc -l
ls pi-rust/crates/pi-tui/src/*.rs | wc -l
ls packages/tui/src/*.ts packages/tui/src/components/*.ts | wc -l
python pi-rust/scripts/app_action_coverage.py
python pi-rust/scripts/extension_event_coverage.py
```

| 口径 | 本轮 | 上轮（LUM-1464） | 说明 |
|---|---|---|---|
| Rust src（去 mod.rs） | 141,365 行 / 254 文件 | 140,894 / 253 | +471 行、+1 文件（`footer.rs`） |
| TS src | 153,106 行 / 669 文件 | 153,106 | 未变 |
| 规模比 | **92.3%** | 92.0% | — |
| Rust `#[test]` | **2,773** | 2,755 | +18 = 4 + 9 + 1 + 4（两 crate 合计） |
| TS 用例 | 5,563 | 5,563 | 同口径 |
| 测试比 | **49.8%** | 49.5% | 2755/5563 = 0.4953 → 2773/5563 = 0.4985 |
| TUI 模块 | 36 / 42 = **85.7%** | 36 / 42 | 未加模块（本轮是同一模块内的行数变化） |
| `app.*` 接线 | 43/44 = **97.7%**（silent: `app.tree.editLabel`） | 43/44 | 未动；LUM-1263 仍在 `in_review`（**未合入 `feature/pi.rs`**） |
| 扩展生命周期事件 | 36/36 声明 + 36/36 生产构造点 | 36/36 | 未动 |

### 4.5 加权完成度（公式与权重沿用 `RUST_TS_PARITY_METRICS.md` §4.1）

| # | 轴 | 权重 | 得分 | 依据 |
|---|---|---|---|---|
| 1 | 可构建 / 可测 / 可运行 | 5% | 100% | §4.1–4.3 |
| 2 | 核心 agent 循环 | 13% | 90% | 未动 |
| 3 | provider API family | 8% | 100% | 未动 |
| 4 | provider / 模型目录广度 | 6% | 70% | 未动 |
| 5 | TUI 交互面（模块率与接线率均值） | 14% | 91.7% | (0.857 + 0.977) / 2 |
| 6 | TUI 视觉保真 | 8% | **90%** | **不动**，见下 |
| 7 | slash 命令面 | 7% | 78% | 未动 |
| 8 | CLI / 模式 / 子命令面 | 7% | 70% | 未动（LUM-1434 在办） |
| 9 | 扩展宿主能力 | 8% | 95% | 未动 |
| 10 | 扩展生命周期事件 | 7% | 100% | §4.4 |
| 11 | 会话 / 存储 / 导入导出 | 9% | 85% | 未动 |
| 12 | 测试与门禁强度 | 5% | **49.8%** | §4.4 |
| 13 | 子包完整度 | 3% | 95% | 未动 |

```
5×1.00 + 13×0.90 + 8×1.00 + 6×0.70 + 14×0.917 + 8×0.90 + 7×0.78 + 7×0.70
+ 8×0.95 + 7×1.00 + 9×0.85 + 5×0.498 + 3×0.95 = 86.9%
```

**口径声明（诚实读法）**：轴 12 从 0.4953 走到 0.4985，折算 **+0.017pt**，一位小数不变。
轴 6（TUI 视觉保真）**本轮不上调**——理由沿用 LUM-1418 §6：该轴的取值口径不是「上游有几行」，
把它重估为 92% 只能 +0.16pt，不足以改变结论。本轮的价值是**把该轴的最大已知缺口从「1 行 vs 2 行」
变成「2 行 vs 2 行」**，属证据/可信度修复，不是覆盖率 +1。

## 5. 截图（frame-buffer，本机无 PTY）

| 文件 | 内容 | 生成方式 |
|---|---|---|
| `docs/screenshots/lum1466-footer-two-row-100x30.png` / `.txt` | 100×30：真 `App::render_to_buffer` 帧，footer 两行 = `/srv/repo (main) • demo` + `Faux  lum1466-footer ... in 0 out 0 ?/8.2k  ? for help` | `cargo test -p pi-tui --test lum1466_two_line_footer_frames frame_dump_two_line_footer -- --nocapture` → `python pi-rust/scripts/frame_to_png.py` |
| `docs/screenshots/lum1466-footer-cut-44x14.png` / `.txt` | 44×14：位置行被切且**以 `…` 标记**，stats 行同时按 LUM-1367 的整段丢弃规则收窄 | 同上（`frame_dump_cut_location_row`） |
| `docs/screenshots/lum1466-footer-single-row-100x30.png` / `.txt` | 100×30 无 cwd：**基线的一行 footer，逐行相同**（回归证据） | 同上（`frame_dump_single_row_footer_without_a_cwd`） |

**诚实说明（本机限制）**：这台 Windows runner 没有 `pty`（`import pty` 不可用），所以截图走 LUM-1412
建立的 **frame-buffer 通道**：它证明「画在哪一格、内容是什么」，**不证明按键时序**；时序/几何由
14 条 App 级测试覆盖（§3 的反向验证）。`scripts/pty_capture.py` 仍是真终端的证据标准，Linux runner 上
应对同一场景补一次 PTY 录制。

## 6. 全 TUI 问题清单（本轮实测更新）

| 顺位 | 问题 | 证据 | 本轮处置 |
|---|---|---|---|
| 1 | ~~footer 是单行，上游 pi 是两行（缺 pwd/branch/name 整行）~~ | LUM-1464 §5 #1 | **已关闭**（§2，含 14 条测试 + 3 帧） |
| 2 | **stats 行本身仍缺上游的字段**：`↑/↓` 箭头、`CH%` 缓存命中率、`$cost`（+`(sub)`）、`(auto)` 自动压缩、多 provider 的 `(provider)` 前缀；且上游 stats 行**不含** session 段，pi-rust 仍保留 | 上游 `footer.ts:130-200`（`statsParts.push('↑…')`、`R/W`、`CH${rate}%`、`$${cost}`、`(auto)`、`getAvailableProviderCount() > 1` 前缀） | **未做**，已写成子任务（§8） |
| 3 | `app.tree.editLabel` 是唯一 silent `app.*`（44 条里 43 条有消费点） | `app_action_coverage.py` → 43/44 | **在办**：LUM-1263（`in_review`，尚未合入 `feature/pi.rs`） |
| 4 | 扩展 `ctx.ui.setStatus(key, text)`：上游 footer 的**第 3 行**来自它，pi-rust 仍是 `ERR_PI_UI_UNSUPPORTED` | `app.rs:200-216` 的 `ctx.ui` 对照表明确排除 `setStatus`；上游 `footer.ts:243-251` 用 `footerData.getExtensionStatuses()` push 一行 | **未做**，列为下一轮第一顺位（与 #2 **同一文件面**，本轮不并发派发，见 §8） |
| 5 | composer `paste_burst`（终端不发 bracketed paste 时的突发识别） | codex `chat_composer.rs` / `paste_burst.rs` | **在办**：LUM-1461（`in_review`） |
| 6 | CLI flag 面（字面 18/40） | `RUST_TS_PARITY_METRICS.md` §3.4 | **在办**：LUM-1434（`in_progress`） |
| 7 | codex 独有、上游 pi-ts 没有的 affordance：`Esc` 二次提示、`Ctrl+P/N` 历史导航 | codex `chat_composer.rs:2838-2880` | **不做**（与上游 pi-ts 冲突，除非产品明确要「超出 pi」） |
| 8 | 本轮修掉的这一条 | §1 | **已关闭** |

## 7. 计划与派发：**1 个新任务**（槽位 2/3）

本轮开工时扫 board（`multica issue list --project ae0b46e7… --status in_progress`）实测：

| issue | 状态 | 指派 | 面 |
|---|---|---|---|
| LUM-1434 | `in_progress` | `编程助手-window` | CLI flag 面（`pi-coding-agent/src/cli/`） |
| LUM-1263 | `in_review` | `编程助手-winpi` | tree 改名 UI（`pi-tui` + `pi-coding-agent`）——**已交付，待验收**，其分支尚未合入 `feature/pi.rs` |
| LUM-1461 | `in_review` | `编程助手-winpi` | composer `paste_burst`（`pi-tui` 同一文件面）——同上 |

即：上一轮的 3/3 满载已回落到 **1/3 在用**，cap（最多 3 个并发）**有余量**。

「如果任务存在是跳过还是计划和实现后续任务」的回答：**不跳过、也不重开大改**，本轮在同一个 run 里
完成 issue 点名的四件事（TUI 审计 / 缺口修复 / 截图 / 推送合并），并**派发 1 个**精确定义的后续任务：

* **LUM-1467（子任务）**——footer stats 行对齐上游（§6 #2）：`↑/↓`、`CH%`、`$cost (+sub)`、`(auto)`、
  `(provider)` 前缀。证据、行号、验收判据全部写进任务正文。

**只派 1 条而不是 2 条**的理由：§6 #2 与 #4 都要改 `pi-tui/src/status.rs`（stats 行与第 3 行是同一个
`render_lines`）；本仓库已经为「同一缺陷两条并发线各修一次」清过两次（LUM-1431 §3、LUM-1445 §8）。
所以 #4 留在清单上，等 #2 落地后再派。

## 8. 范围之外

未碰 `pi-ai` / `pi-agent-core` / `pi-protocol` / `pi-extensions` / `pi-session` / `pi-server` /
`pi-client` / `pi-chord` / `pi-evals` / `pi-telemetry` 的源码；未碰上游 TS（`packages/**`，只读取证）；
未碰 CI / Docker；未改扩展事件表、slash 命令表、CLI flag 表、`CONSUMED_APP_ACTIONS`。

**已知偏差（写清而不是省略）**：

1. git 分支在**启动时解析一次**，上游有 `HEAD` 文件监视器（`footer-data-provider.ts:139-196`）会在切分支时
   重绘 footer；Rust 端口不重绘（`pi-rust/crates/pi-coding-agent/src/footer.rs` 模块注释已写明）。
2. 分支名从 `.git/HEAD` 解析而非 `git symbolic-ref --short HEAD`（不依赖 `git` 可执行文件）；对
   `packed-refs` 与 detached HEAD 的行为与上游一致（后者 → 无 `(branch)` 后缀），但 `refs/` 下非
   `refs/heads/` 的怪 ref 上游会显示、本端口显示为空。
3. `format_cwd_for_footer` 的折叠形式统一用 `/`（上游用平台分隔符）；Windows 路径的可读性不受影响。
4. 当位置行带上 `/name` 时，stats 行**不再重复**该名字（上游 stats 行本就没有 session 段）；
   无名字时会话 id 仍留在 stats 行——这是对「一行 footer 也要能说出自己是哪个会话」的保留。
