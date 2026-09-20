# 等待反馈与启动可发现性（Stage 66 = LUM-1228）

第六轮审计（`docs/TUI_UX_AUDIT.md` 第八节）把剩余空白收敛成两块：**流式等待没有动效**
（`grep -rni spinner pi-rust/crates` 命中 0）与**首次进入没有任何 key hints**（仓库里没有启动头）。
本文记录这一片的实现、与上游的对照点、以及刻意保留的差异。

对照物：上游 `packages/tui/src/components/loader.ts`、`packages/coding-agent/src/modes/interactive/`
的 `interactive-mode.ts`、`components/status-indicator.ts`，以及 [Martty](https://github.com/louloulin/Martty)
的 `src/app.rs`（`SPINNER` / `spinner_idx` / `spinner()`）与 `src/locale.rs`（`tr(en, zh)`）。

## 一、交付内容

| 文件 | 内容 |
| --- | --- |
| `crates/pi-tui/src/loader.rs`（新增） | `SPINNER_FRAMES`（10 个盲文字形，逐字抄上游 `DEFAULT_FRAMES`）、`SPINNER_INTERVAL_MS = 80`、`Spinner`、`format_elapsed`、`indicator_line` |
| `crates/pi-tui/src/locale.rs`（新增） | `Locale`（`En` / `Zh`，`tr(en, zh)`）、`HeaderKey`（Chord / ChordTwice / ChordPair / Literal）、`STARTUP_HINTS`（20 行常量表）、`format_chord` |
| `crates/pi-tui/src/status.rs` | `BusyIndicator { frame, elapsed }` + `StatusData::busy`；footer 左簇变成 `⠋ 12s  gpt-4o  session …` |
| `crates/pi-tui/src/app.rs` | `App::tick_busy_feedback(now)`、`Spinner` 状态、`app.header` 键位、内置启动头（`builtin_header_lines` / `composed_frame`） |
| `crates/pi-coding-agent/src/interactive.rs` | `InteractiveOptions::quiet_startup`、`interactive_app_config`（启动面收敛成纯函数）、`PI_LANG` → `Locale` |
| `crates/pi-coding-agent/src/cli.rs` + `main.rs` | `--no-header` |
| `crates/pi-coding-agent/src/keybindings.rs` | `APP_KEYBINDING_IDS` 43 → **44**（新增 Rust 专有 `app.header`，默认 `alt+h`） |
| `crates/pi-coding-agent/src/commands/slash.rs` | `/hotkeys` 的 app 分组补 `app.header` 一行 |

## 二、spinner + 轮耗时

**一个时钟，不是两个。** `loader.rs` 里没有 `Instant`、没有 `thread`、没有 `tokio::time`：
`Spinner` 只是一个下标。推进由 `App::tick_busy_feedback(now)` 负责，而它的唯一调用点是
`render_to_buffer` —— 也就是渲染循环**已有的** 50 ms 节奏（`AppConfig::event_poll_interval`）。
`now` 是参数而不是 `Instant::now()`，所以测试能确定性地推进动画。

```
submit()            → turn_started = Some(now)，spinner.reset()，status.set_busy(frame, 0s)
render_to_buffer()  → tick_busy_feedback(Instant::now())
                       忙 → 距上次推进 ≥ 80ms 则 advance()；刷新 busy(frame, elapsed)
                       闲 → 清掉 busy，spinner.reset()，忘掉轮起点
```

- **帧表逐字对齐上游**：`⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏`、`80 ms`，与 Martty `src/app.rs:31` 的 `SPINNER`
  完全相同，因此「跟得上上游」是可验证的常量关系，而不是主观感受。
- **耗时是 Rust 侧新增**（上游 `status-indicator.ts` 只有 `{ state, label }`，没有计时器）。
  这不是偏离用户需求，而是这一片的目的之一：慢模型与卡死进程必须能区分。
  `format_elapsed`：`12s` / `1m05s` / `1h02m`（与 Martty 的 `0:12` 风格不同，用更紧凑的英文单位）。
- **位置**：与 Stage 64 的 usage 数字同一行、同一个左簇，顺序 `⠋ 12s  <model>  <session>  <stats>`。
  空闲时 `busy = None`，该段不产生任何 span，footer 与改前逐字节相同 —— 这是 92 个
  `render_snapshot` 快照测试不需要改的原因。
- **回传语义**：`tick_busy_feedback` 返回「footer 这一帧是否有可见变化」（帧变了，或整秒变了）。
  驱动每 50 ms 无条件重绘，所以它只用于测试断言与将来的按需重绘，不承担正确性。

## 三、启动头 + `app.header`

上游的启动头由 `customHeader ?? builtInHeader` 决定（`interactive-mode.ts:958`），展开状态来自
`getStartupExpansionState()`（`--verbose || toolOutputExpanded`），显示与否由
`options.verbose || !settingsManager.getQuietStartup()` 决定（`:910`）。

Rust 侧的映射：

- **显示与否** = `AppConfig::startup_header`。默认 `false`（保住既有快照测试的逐字节契约），
  驱动在 `interactive_app_config` 里传 `!quiet_startup`，CLI 拼写作 `--no-header`。
- **展开与否** = `App::header_expanded`（`AppConfig::startup_header_expanded` 是初值，默认 `true`）。
- **折叠后占 0 行**：`builtin_header_lines` 直接返回空 `Vec`，而不是留一行 `…`；行数还给转录。
  这是与上游 `ExpandableText` 的差异（上游折叠仍留标题行），理由是「首屏提示可关掉」比
  「标题常驻」更贴合这一片的可发现性目标，且 `app.header` 的 flash 提示里有恢复键位。
- **键位是 Rust 专有**：上游没有 `app.header`，它把启动头的 `setExpanded` 和工具块一起挂在
  `app.tools.expand` 上。这里把两者拆开（`Ctrl+O` 只折叠工具输出），并给启动头一个**真实注册**
  的 id `app.header`（默认 `alt+h`），从而可被 `keybindings.json` 覆盖、可被 `/hotkeys` 列出 ——
  与 Stage 60 给 `app.session.new` 补 `alt+n` 同一先例。`matches_with_fallback` 的双分支都有测试：
  有注册时以注册为准（`keybinding_consumer.rs` 把 `alt+h` 移到 `alt+j`），未注册时回落内置 chord。
- **文案表**：`STARTUP_HINTS` 20 行（`en` / `zh` 两列），键位列**不写死**，渲染时按
  `get_keybindings().get_keys(id)` 解析；未绑定的动作整行不显示（与 `/hotkeys` 同一规则）。
  20 行 = 上游 `expandedInstructions` 的键位 + Rust 专有的 `app.header` 一行。
- **两种语言**：`Locale` + `tr(en, zh)` 的常量表，不引 i18n 框架（对照 Martty `src/locale.rs`）。
  `PI_LANG=zh`（容忍 `zh-CN` / `en-US` 这类区域后缀）选择表格，默认英文。
- **预算**：启动头走**已有的**扩展头通道（`ExtensionFrame::header`），在 `composed_frame` 里
  「扩展没设 header 才填内置行」，然后交给 `plan_chrome`。顺序很重要 —— 行预算必须先把 header
  的行扣掉再算消息视口，否则内置头会被画进 0 高的区域。扩展 `ctx.ui.setHeader` 仍然优先。

## 四、验证

新增测试（全部为真实断言，非快照比对）：

- `crates/pi-tui/tests/busy_feedback.rs`（10 个）：footer 出现 `⠋ 0s`；80 ms 才推进一帧；
  轮结束后 segment 消失且光标归零；`render_to_buffer` 确实在推进动画；启动头标题与行内容；
  默认配置无启动头；折叠后行归还转录 + flash 文案；`--no-header` 语义；`format_elapsed` 三种量级。
- `crates/pi-tui/tests/startup_header.rs`（3 个）：对着**装好的**键位表解析真实 chord
  （`Ctrl+C twice to exit`、`Ctrl+P/Shift+Ctrl+P to cycle models` 等 19 行）；覆盖后键位跟随覆盖；
  未绑定的动作整行消失。
- `crates/pi-tui/tests/keybinding_consumer.rs`（追加）：`app.header` 被覆盖后 `alt+h` 失效、
  `alt+j` 生效。
- `crates/pi-coding-agent/tests/startup_header.rs`（3 个）：驱动侧默认显示启动头且 chord 来自
  `merged_definitions`；`quiet_startup` 时不显示；`--no-header` 能解析。
- `crates/pi-tui/src/{loader,locale,status}.rs`、`interactive.rs`、`slash.rs`、`keybindings.rs`
  的单测：帧表/推进/回绕、`format_elapsed`、`indicator_line`、locale 解析与 `tr`、
  `HeaderKey::label` 的四种形态、footer busy 段的出现/消失/裁切、`--no-header` → option →
  `AppConfig` 的映射、`/hotkeys` 新行。
- `crates/pi-coding-agent/tests/keybindings.rs`：43 → 44 的计数与顺序断言同步更新。

跑过的门（本轮实跑，`-j 4`）：

```console
$ cargo fmt --all -- --check
$ cargo clippy -p pi-tui -p pi-coding-agent --all-targets -- -D warnings
$ cargo test --workspace
   … workspace: 2238 passed / 0 failed（其中 pi-tui 728、pi-coding-agent 664）
$ cargo test -p pi-evals
   8 passed（含 docs-code-fences-balanced：本文件也在审计范围内）
```

## 五、刻意保留的差异与后续

| 项 | 现状 | 原因 / 后续 |
| --- | --- | --- |
| 折叠后不留标题行 | 折叠 = 0 行 | 见第三节；如需上游的「留 logo 行」，改 `builtin_header_lines` 一个分支 |
| 启动头不做窄屏换行 | 超宽行被 `write_styled_line` 裁掉 | 最长的提示 42 字符，80 列下不触发；真要换行再引 `textwrap` |
| `PI_LANG` 不是 `settings.json` 项 | 只读环境变量 | 上游用 `settings` + `/config`；Rust 端 `Locale` 已可注入，接设置项是后续小改 |
| 耗时不是上游行为 | Rust 新增 | 见第二节，这是本片的需求本身 |
| `app.editor.external` / `app.session.fork` 仍未消费 | 不在本片 | 第六轮审计第八节的清单 |
