# LUM-1464 — `?` shortcut overlay（TUI chatinput 收口）+ 全 TUI 真实审计 + Rust↔TS 复测

> scope: `pi-rust/`（`pi-tui` 单模块 + 2 个测试文件）
> branch: `work/LUM-1464`（→ `feature/pi.rs`）
> 时间锚：LUM-1460（`c37d80742`）之后的下一轮；issue 正文是 LUM-981 伞形任务的复触发

## 0. 结论速览

| 量 | 本轮实测 | 依据 |
|---|---|---|
| **`?` 键（issue 点名项：chatinput 与 codex/Martty 的差距）** | **假广告 → 真功能** | §1：基线 `pi-rust/crates` 里 `Char('?')` **0 命中**，而 `app.rs:1384` 广告 `? for help` |
| 新增行为测试 | **11 条**（`tests/shortcut_overlay.rs`） | §3 |
| 新增帧测试 | **4 条 / 4 张截图** | `tests/lum1464_shortcut_overlay_frames.rs` + `docs/screenshots/lum1464-hints-*.{png,png.txt}` |
| `pi-tui` 全量 | **1090 passed / 0 failed**（74 target） | 基线 `c37d80742` 同机 **1075/0** → **+15**（= 11 + 4） |
| `pi-coding-agent` | 826 passed / **28 failed**（33 target） | 28 条全是 Windows 环境类；见 §4.3 的逐条对账 |
| 纯代码规模（src↔src） | **92.0%**（140,894 / 153,106） | `python pi-rust/scripts/measure_loc.py` |
| 测试规模 | **49.5%**（2,755 / 5,563） | §4.4 的两条命令 |
| `app.*` 接线 | **43/44 = 97.7%** | `app_action_coverage.py --check-consumed` → `43/43 in sync` |
| 扩展生命周期事件 | **36/36 声明 + 36/36 生产构造点** | `extension_event_coverage.py` |
| TUI 模块面 | **36/42 = 85.7%** | §4.4 |
| 加权完成度 | **86.9%**（与上轮 86.9 同值；只有测试轴 +0.05pt） | §4.5，公式与权重沿用 `RUST_TS_PARITY_METRICS.md` §4.1 |

一句话结论：**issue 里「TUI chatinput 和 codex / Martty 差距很大」在编辑语义面上早已不成立（前几轮逐条
对账 + 本轮复核），但本轮逮到一个真实的、可复现的差：`?`。** 这个键在 pi-rust 的 footer 上从第一版起就
广告着 `? for help`，而整个 Rust 端口**没有任何 `?` 处理点**——按下它只是往草稿里插入一个 `?`。codex 有真东西
（`ChatComposer::handle_shortcut_overlay_key` → `FooterMode::ShortcutOverlay`，`bottom_pane/footer.rs:shortcut_overlay_lines`
按列排键位表），Martty 也有等价物（`src/input/keymap.rs:97` 空草稿时 `Ctrl+K` → `ShowKeys`）。本轮把这一格补上，
并把「广告必须能兑现」这条不变量从 `app.*` 扩到 footer 文案。

## 1. 真实审计：`?` 是一条**没人量过**的假广告

### 1.1 取证（可复现，全部限定 `-- pi-rust/crates`）

```bash
# 基线 tip 上，Rust 源码里有没有 `?` 的处理点？
$ git grep -n "Char('?')" origin/feature/pi.rs -- pi-rust/crates | wc -l
0

# 但 footer 广告着它：
$ git grep -n '"for help"\|"? for help"' origin/feature/pi.rs -- pi-rust/crates
origin/feature/pi.rs:pi-rust/crates/pi-tui/src/app.rs:1384:        status_data.hint = Some("? for help".to_string());
origin/feature/pi.rs:pi-rust/crates/pi-tui/tests/thinking.rs:235:  assert_eq!(snapshot.status.hint.as_deref(), Some("? for help"));
```

`App::status_for_render` 在无 flash、无反查、无思考级别时把 `status_data.hint` 原样送到状态栏
（`app.rs::status_for_render`），所以**真实 TUI 的空闲 footer 就是 `… ?/8.2k  ? for help`**，
而 `?` 只会被 `editor.insert_char` 吃成一个字符。

### 1.2 为什么前几轮的「广告真实性」审计没抓到它

前几轮把「广告面 vs 实现面」做成了脚本（`scripts/app_action_coverage.py` /
`hint_chord_literals.py` / `keybinding_coverage.py`），但三条口径都只扫 **registry id**
（`app.*` / `tui.*`）。`?` 既不是 registry id，也不出现在 `/hotkeys` 里——它是**状态栏文案里的一个字面量**：

| 脚本 | 它扫的对象 | 为什么漏掉 `?` |
|---|---|---|
| `app_action_coverage.py` | `CONSUMED_APP_ACTIONS` 与 `"app.<id>"` 字面量 | `?` 不是 `app.*` id |
| `keybinding_coverage.py` | 定义文件 vs 消费点的 id 出现 | 同上 |
| `hint_chord_literals.py` | 提示文案里的 chord 字面量 | 它的模式是 `Ctrl+…` / `Alt+…`，`?` 不在词法里 |

**流程修正（写给下一轮）**：`hint_chord_literals.py` 的扫描面应从「修饰键 chord」扩到
「单字符 chord 的文案」——本类缺陷的判据只有一条：**文案里出现的键，代码里必须有分支**。
本轮已把该键的行为测试与帧测试一起放进 `tests/shortcut_overlay.rs`，作为这条不变量的回归门。

### 1.3 codex / Martty 怎么做的（第一手，不转述）

| | codex（本机 `lclaw/project/codex`，`2874286`） | Martty（`93e9231`，本轮 clone） | 上游 pi-ts | pi-rust 本轮前 |
|---|---|---|---|---|
| 键 | `?`（空 composer、非 paste burst） | `Ctrl+K`（空输入） | 无 | 无（但**广告了** `?`） |
| 条件 | `matches!(code, Char('?')) && !has_ctrl_or_alt && self.is_empty() && !self.is_in_paste_burst()`（`chat_composer.rs:3154-3157`） | `KeyCode::Char('k') if ctrl && ctx.input_empty`（`keymap.rs:97`） | — | — |
| 展示面 | 多行 footer 列布局（`footer.rs::shortcut_overlay_lines`） | 独立 keys overlay | — | — |
| 其它键的行为 | `reset_mode_after_activity()` 关掉 overlay 后**照常处理该键** | 关闭 overlay | — | — |

即两边都收敛到同一形状：**空输入 + 单键 = 键位速查表；任何其它键先关掉它、再照常处理。**
本轮实现与这条完全同构（§2）。

## 2. 本轮落地（文件:行号）

| 文件 | 改动 | 说明 |
|---|---|---|
| `pi-rust/crates/pi-tui/src/app.rs` | `+~40`（字段/API）、`+~60`（键路由）、`+~90`（渲染与内容） | `App::shortcut_overlay` 字段；`shortcut_overlay_open` / `toggle_shortcut_overlay` / `close_shortcut_overlay`；`step_key_at` 里的 `?` 路由（打开/关闭/落穿）；`hint_entries`（把 header 的 hint 解析抽成共享入口）；`shortcut_overlay_lines`（标题 + 分隔线 + 一/两列 + onboarding）；`paint_shortcut_overlay`（贴近 composer 的底部锚定面板） |
| `pi-rust/crates/pi-tui/src/locale.rs` | `+16` | `SHORTCUT_OVERLAY_TITLE_{EN,ZH}`、`SHORTCUT_OVERLAY_CLOSE_{EN,ZH}`（与既有 header 文案同一套 `Locale::tr` 机制） |
| `pi-rust/crates/pi-tui/src/extension_ui.rs` | `+12` | `ExtensionUi::has_overlay()`：App 绘制自己的面板前的闸门，`custom` overlay 是更外层 |
| `pi-rust/crates/pi-tui/tests/shortcut_overlay.rs` | 新增（11 条） | 触发闸门 / 关闭语义 / 4 个键盘层不抢占 |
| `pi-rust/crates/pi-tui/tests/lum1464_shortcut_overlay_frames.rs` | 新增（4 帧） | `100×30` 空闲 / `100×30` 打开 / `56×16` 单列 / `100×30` 关闭复原 |
| `pi-rust/docs/screenshots/lum1464-hints-*.{png,png.txt}` | 4 + 4 | `scripts/frame_to_png.py` |

### 2.1 路由（插在 modal / settings 之后、全局 chord 之前）

```
history-search → extension overlay → dialog → settings → ★ `?` overlay → 全局 chord → alt-screen → editor → composer
```

三条规则，逐条对应 codex：

1. **打开**：`?` 且无修饰键、无 transcript 搜索、无 `custom` overlay、无补全下拉、草稿为空、无附件 → `Redraw`（codex 的 4 个条件 + 本 port 的 3 个键盘层）。
2. **关闭**：`?` 或 `Esc` → 关闭并 `Redraw`（`Esc` 被吞掉，不会落到 `app.clear` / `app.interrupt`）。
3. **落穿**：其它任何键 → 先关 overlay，**不 return**，继续走下面所有层（codex 的 `reset_mode_after_activity`）。所以 `Ctrl+O` 既关 help 又真的展开工具输出（有测试钉住）。

### 2.2 渲染（底部锚定、借 transcript 行、关闭即复原）

- 面板贴 composer 上沿，**借用 transcript 的底部行**，不占 chrome 行 → 打开 help 不重排对话；测试
  `frame_dump_closing_restores_the_transcript` 逐行断言「关掉后帧与打开前完全相同」。
- 宽 ≥ 72 列（`cell_width = width/2 ≥ 36`）分两列；否则单列。列内 chord 左对齐到**本组最宽的 chord**
  （本轮实测 `Ctrl+P/Shift+Ctrl+P` → 22 列），描述按**终端列**补白，超出用 `…` 标记而不是静默切断
  （LUM-1412 的规则）。
- 短终端 `lines.truncate(available)`：保留标题与最靠前的键位，`/hotkeys` 仍是穷举面。

### 2.3 内容与既有面**同源**

`hint_entries()` 是 header / `/hotkeys` / overlay 三处共用的解析入口（原来只在 `header_hint_lines` 里），
两个过滤器（`is_wired()` + registry 里是否绑定）原样保留。所以：

- overlay 不会广告死键位（`is_wired` 拦掉未实现的 `app.*`）；
- 用户用 `keybindings.json` 改键后，overlay 与 header 同步移动（同一 `kb.get_keys`）。

## 3. 反向验证（本机实做）

把 `step_key_at` 里的触发分支换成 `return StepOutcome::Idle`（不改其它任何一行）：

```
tests/shortcut_overlay.rs         3 条立刻红（opens / again-closes / escape-closes）
lum1464_shortcut_overlay_frames   2 条立刻红（two_columns / single_column）
```

恢复后 10 + 4 全绿。即这批断言真的钉住了本轮改动，而不是在描述既有行为。

## 4. 门禁与数字

### 4.1 fmt / clippy / check

| 门禁 | 结果 |
|---|---|
| `cargo fmt --all -- --check` | clean |
| `cargo clippy --offline -p pi-tui --all-targets` | 0 warning |
| `cargo check --offline -p pi-tui --all-targets` | exit 0 |

### 4.2 `pi-tui` 全量

```
$ cargo test --offline -p pi-tui -j 4
targets 74  passed 1090  failed 0  ignored 0
```

基线（`c37d80742`，本轮前）为 **1075 / 0**（LUM-1460 §3 记录），差额 **+15 = 11 行为 + 4 帧**。

### 4.3 `pi-coding-agent`（含集成 target）

```
$ cargo test --offline -p pi-coding-agent -j 2 --no-fail-fast
targets 33  passed 826  failed 28  ignored 0
```

28 条**全部为 Windows 环境类**（真 `bash` 工具、绝对路径断言、`/tmp`、node fs、project trust、
扩展发现），逐条名字见下表；**本机另做了一次 stash 对账**：唯一一条名字里带 keybinding 的
`reload_rereads_keybindings_and_ui_settings_mid_session` 在**未打补丁的基线**上重跑仍是 FAILED
（`reload_config` 4 条里 3 passed / 1 failed，与本轮树一致），即**新增失败 0 条**。

与 `?` / footer / 帧相关的 5 个 target 全绿：`chatinput_chord_conflicts`、`keybindings`、
`help_text_layout`、`startup_header`、`extension_ui`（+ `lum1448`/`lum1450` 两个帧 target）。

**诚实说明（本机限制）**：Windows runner 无 `pty`（`import pty` 不可用），所以本轮的截图走的是
LUM-1412 建立的 **frame-buffer 通道**：它证明「画在哪一格、内容是什么」，**不证明按键时序**；
时序由 11 条 App 级行为测试覆盖。`scripts/pty_capture.py` 仍是真终端的证据标准，
下一台 Linux runner 上应对同一场景补一次 PTY 录制。

### 4.4 Rust↔TS 口径（全部可复现）

```bash
python pi-rust/scripts/measure_loc.py
grep -rhoE '#\[(tokio::)?test\]' pi-rust/crates --include=*.rs | wc -l
grep -rhoE "^\s*(it|test)(\.\w+)?\(" packages --include=*.test.ts | wc -l
ls pi-rust/crates/pi-tui/src/*.rs | wc -l
ls packages/tui/src/*.ts packages/tui/src/components/*.ts | wc -l
python pi-rust/scripts/app_action_coverage.py --check-consumed
python pi-rust/scripts/extension_event_coverage.py
```

| 口径 | 本轮 | 上轮（LUM-1460） | 说明 |
|---|---|---|---|
| Rust src（去 mod.rs） | 140,894 行 / 253 文件 | 140,615 | 本轮 +279（源码 +~190，其余是测试/文档） |
| TS src | 153,106 行 / 669 文件 | 153,106 | 未变 |
| 规模比 | **92.0%** | 91.8% | — |
| Rust `#[test]` | 2,755 | 2,740 | +15（= 11 + 4） |
| TS 用例 | 5,563 | 5,309（旧口径） | **口径变了**：本轮按 `it|test(` 前缀重数，含 `it.each` 等包装 |
| 测试比 | **49.5%** | 49.4% | 同口径不可比，见下面的口径声明 |
| TUI 模块 | 36 / 42 = **85.7%** | 35 / 42 | 上轮之后另有模块并入；本轮未加模块 |
| `app.*` 接线 | 43/44 = **97.7%** | 43/44 | 唯一 silent 仍是 `app.tree.editLabel`（缺 UI，LUM-1263 在办） |
| 扩展事件 | 36/36 + 36/36 | 36/36 | 未动 |

> **口径声明**：TS 用例数从 5,309 变 5,563 是**换口径**（旧口径只数 `it(` / `test(`，新口径收
> `it.each(` 等），不是 TS 测试变多；因此 49.4% → 49.5% 这个「上升」不可当成绩读。

### 4.5 加权完成度（公式与权重沿用 `RUST_TS_PARITY_METRICS.md` §4.1）

| # | 轴 | 权重 | 得分 | 依据 |
|---|---|---|---|---|
| 1 | 可构建 / 可测 / 可运行 | 5% | 100% | §4.1–4.3 |
| 2 | 核心 agent 循环 | 13% | 90% | 未动 |
| 3 | provider API family | 8% | 100% | 未动 |
| 4 | provider / 模型目录广度 | 6% | 70% | 未动 |
| 5 | TUI 交互面（模块率与接线率均值） | 14% | **91.7%** | (0.857 + 0.977) / 2 |
| 6 | TUI 视觉保真 | 8% | 90% | 未动 |
| 7 | slash 命令面 | 7% | 78% | 未动（18/23） |
| 8 | CLI / 模式 / 子命令面 | 7% | 70% | 未动（LUM-1434 在办） |
| 9 | 扩展宿主能力 | 8% | 95% | 未动 |
| 10 | 扩展生命周期事件 | 7% | 100% | §4.4 |
| 11 | 会话 / 存储 / 导入导出 | 9% | 85% | 未动 |
| 12 | 测试与门禁强度 | 5% | **49.5%** | §4.4 |
| 13 | 子包完整度 | 3% | 95% | 未动 |

```
5×1.00 + 13×0.90 + 8×1.00 + 6×0.70 + 14×0.917 + 8×0.90 + 7×0.78 + 7×0.70
+ 8×0.95 + 7×1.00 + 9×0.85 + 5×0.495 + 3×0.95 = 86.9%
```

**诚实读法**：本轮**没有让加权分变好看的位置**——`?` overlay 不落在任何一条「有没有这个能力」的轴上
（它属于轴 5 的质量面，而轴 5 的口径是模块数 + `app.*` 接线数）。它让这一格从「广告为真」变成「广告可兑现」，
是**可信度**的修复，不是覆盖率 +1。

## 5. 全 TUI 真实问题清单（本轮实测，按性价比）

| 顺位 | 问题 | 证据 | 处置 |
|---|---|---|---|
| 1 | **footer 是单行，上游 pi 是两行**：缺 `pwd (git branch) • sessionName` 整行；stats 面缺 `$cost` / `CH%` 缓存命中 / `(auto)` 自动压缩 / 多 provider 前缀 / 扩展 `setStatus` 行 | `status.rs:160-162` 自己的文档写着 "Upstream's two-line footer … a deliberate simplification"，而上游 `packages/coding-agent/.../footer.ts:230-246` 确实 `const lines = [pwdLine, dimStatsLeft + dimRemainder]` 再 push `extensionStatuses` | **未做**（跨 `status` + `plan_chrome` + 驱动接线，会动 1000+ 条既有断言的帧高）。列为下一轮第一顺位 |
| 2 | `app.tree.editLabel` 是唯一 silent `app.*`（44 条里 43 条有消费点） | `app_action_coverage.py` | **在办**：LUM-1263（`in_progress`） |
| 3 | composer `paste_burst`（终端不发 bracketed paste 时的突发识别，codex `paste_burst.rs`） | codex `handle_input_basic_with_time` 的时间窗 | **在办**：LUM-1461（`in_progress`，本 agent）——本轮刻意不碰，避免与在飞写者抢同一文件 |
| 4 | CLI flag 面（字面 18/40） | `RUST_TS_PARITY_METRICS.md` §3.4 | **在办**：LUM-1434（`in_progress`，编程助手-go） |
| 5 | codex 独有、上游 pi-ts 没有的两条 affordance：`Esc` 二次提示（backtrack 到上一条消息）、`Ctrl+P`/`Ctrl+N` 历史导航 | codex `chat_composer.rs:2838-2848`（Esc → `esc_hint_mode`）、`2861-2880`（`Char('p')|Char('n')` 历史导航）；pi-rust 的 `tui.editor.historyPrevious/Next` 是 `NO_KEYS`（与上游一致） | **不做**：与上游 pi-ts 冲突，除非产品明确要「超出 pi」 |
| 6 | 本轮修掉的这一条 | §1 | **已关闭**：`?` 广告可兑现（§2、§3） |

## 6. 「最多 3 个任务」的处置 = **零派发**（槽位已满 3/3）

本轮开工时扫 board（`multica issue list --project ae0b46e7… --status in_progress`）实测：

| issue | 状态 | 指派 | 面 |
|---|---|---|---|
| LUM-1263 | `in_progress` | `编程助手-winpi` | tree 改名 UI（`app.tree.editLabel`，`pi-tui` + `pi-coding-agent`） |
| LUM-1461 | `in_progress` | `编程助手-winpi` | composer `paste_burst`（`pi-tui` 同一文件面） |
| LUM-1434 | `in_progress` | `编程助手-go` | CLI flag 面（`pi-coding-agent/src/cli/`） |

**结论**：三个槽位都已被在办任务占据，且其中两条（LUM-1263、LUM-1461）与 `pi-tui` **同一文件面**——
本轮再派第四条只会重演「同一缺陷两条并发线各修一次」（LUM-1431 §3、LUM-1445 §8 各清过一次）。
所以本轮选择 **self-do 1 件（`?` overlay）+ 零派发**，并把「下一轮第一顺位」写成 §5 的可挑清单。

「如果任务存在是跳过还是计划和实现后续任务」的回答：**不跳过、不重开大改**——issue 要求的四件事
（TUI 审计 / chatinput 修复 / 截图 / 推送合并）本轮全部在同一 run 内完成并过门，所以按 LUM-1366 §5
的既定判断法，本轮**实现了一个真实缺口**（`?`）而不是再造一条协调轮。

## 7. 范围之外

未碰 `pi-ai` / `pi-agent-core` / `pi-protocol` / `pi-extensions` / `pi-session` / `pi-server` /
`pi-client` / `pi-chord` / `pi-evals` / `pi-telemetry` 的源码；未碰 `pi-coding-agent`（本轮改动
全在 `pi-tui`）；未碰上游 TS（`packages/**`，只读取证）；未碰 CI / Docker；未碰扩展事件表、
slash 命令表、CLI flag 表。
