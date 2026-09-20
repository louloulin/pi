# LUM-1266：矮终端下的启动头自动折叠 + 聊天滚动条独占最右列

分类：TUI / pi-rust 交互。
基线：`origin/feature/pi.rs` = `b8adfe6bd`（LUM-1261 的 tip）。本轮把 `work/LUM-1266`（合并提交 `e4db5cb82`）合入该 tip 后推送回 `feature/pi.rs`。

## 0. 结论（先看这段）

本轮只做两件事，两件都在 `pi-tui` 里，都有独立测试：

| # | 改动 | 位置 | 证据 |
| --- | --- | --- | --- |
| A2 | **内置启动头在矮终端下自动折叠**：终端装不下"标题 + 提示列表 + 输入框 + 状态栏"时，提示列表折成 1 行并告诉用户如何展开 | `crates/pi-tui/src/app.rs`（`composed_frame` / `builtin_header_lines` / `header_title_lines` / `header_hint_lines`）、`crates/pi-tui/src/locale.rs`（`header_folded_line`） | 真机 PTY A/B：120×22 下聊天区 **0 行 → 17 行**；120×23 下 `/help` 与 `/hotkeys` 两帧 **从"完全相同"变成两帧各 19/21 行可见内容** |
| B | **聊天滚动条自己占一列**：绘制滚动条时转写区宽度从 `W` 变 `W-1`，文字不再被滚动条覆盖 | `crates/pi-tui/src/app.rs`（`viewport_for_render`、`record_viewport`、`viewport_reserved`、`scrollbar_geometry`、`paint_scroll_to_end`） | `tests/short_viewport.rs` 3 条不变量测试 + 帧里"文本列恒为 119、滚动条恒落在第 120 列" |

**与 LUM-1261 的关系（重要）**：LUM-1261 在平行分支上先落地了同一处根因的另一种修法——`extension_ui.rs::plan_chrome`
改为**先发输入框预算、再发 header**（提交 `b8adfe6bd`），并修了 `/help` 的逐源行渲染（`c8bc6785a`）。
本轮合并时**整段采纳 LUM-1261 的 `plan_chrome`**（它的测试已经钉住该行为，不保留我的重复实现），
只保留它**没有**做的两件事：内置头的自动折叠（正是 LUM-1261 §7.4 第 1 条自认"仍未实现"的项）与滚动条列预留。

## 1. 缺陷 A2：矮终端下启动头挤掉整个聊天区

### 1.1 现象（真机，`b8adfe6bd` 编出来的二进制）

`docs/screenshots/lum1266-before-short-22.png.txt` 面板 1（120×22）与
`docs/screenshots/lum1266-before-short-23.png.txt` 面板 3–4（120×23）：

```text
# before @120x23（面板 3 = 输入 /help 之后，面板 4 = 输入 /hotkeys 之后）
row 0  |pi v0.1.0
row 1..18|  Esc to interrupt / … / drop files to attach      ← 18 行提示把屏幕吃满
row 19 |（空）
row 20 |·                                                     ← 聊天区只剩这 1 行
row 21 |> type a prompt — /help for commands                  ← 输入框（LUM-1261 修的，可见了）
row 22 |Faux test model  session-…  in 0 out 0 ?/8.2k  ? for help
```

两个连续面板（`/help` 与 `/hotkeys`）的帧哈希**完全相同**（`9b19865217b3`），harness 直接判
`FAIL: panel(s) [4] are byte-identical to the previous panel`（该场景退出码 1）。
也就是说：**输入框可见了，但你打不开聊天记录**——`/help` 的内容根本没有地方画。

### 1.2 根因

`plan_chrome` 决定"每块区域发几行"，但它只按"渲染顺序 + 总高度"发预算，**不知道内置启动头有多少行**。
默认启动头是 21 行（`startup_header_expanded: true`，见 `pi-coding-agent/src/interactive.rs`），
于是 22/23 行的终端里 header 拿走的行数把转写区压到 1 行。
LUM-1261 把顺序改成"输入框优先"后，被牺牲的从输入框变成了聊天区——同一根因的另一个受害者。

### 1.3 修法

折叠策略只作用于**内置启动头**，且只在其**装不下**时触发：

```rust
// crates/pi-tui/src/app.rs
pub const MIN_TRANSCRIPT_ROWS: u16 = 3;      // 转写区至少留 3 行（含 1 行提示行）
pub const RESERVED_CHROME_ROWS: u16 = 2;     // 输入框 + 状态栏
pub const HEADER_RESERVED_ROWS: u16 = MIN_TRANSCRIPT_ROWS + RESERVED_CHROME_ROWS;

fn builtin_header_lines(&self, total_height: u16) -> Option<Vec<Line>> {
    let expanded_rows = self.header_title_lines().len() + self.header_hint_lines().len();
    if expanded_rows + HEADER_RESERVED_ROWS as usize > total_height as usize {
        // 折叠帧：标题行 + （还有余量时）一行"提示已折叠，按 Alt+H 展开"
    } else { /* 完整体：标题行 + 全部提示行 */ }
}
```

折叠行文案走 locale：`locale.rs::header_folded_line`（EN：`hints hidden on a short terminal — Alt+H shows them`）。

**策略分界（有意为之）**：

| 来源 | 装不下时 |
| --- | --- |
| 内置启动头（`startup_header`） | **折叠**成 1 行提示 + 标题 |
| 扩展 `ctx.ui.setHeader` 给的 header | **截尾**（保留 LUM-1261 定的策略，扩展内容不替它做取舍） |

做成折叠而不是裁掉提示列表的理由：提示列表是"教学信息"，聊天区是"工作区"。
矮终端上牺牲 1 行教学信息换回十几行工作区，同时**明确告诉用户**怎么把提示要回来（`Alt+H`），比静默截断更可预期。

### 1.4 修好之后（真机 PTY）

```text
# after @120x22  docs/screenshots/lum1266-short-22.png.txt 面板 1（frame 9a71526dc540）
row 0  |pi v0.1.0
row 1  |hints hidden on a short terminal — Alt+H shows them
row 2..19|（聊天区，17 行）
row 20 |> type a prompt — /help for commands
row 21 |Faux test model  session-…  in 0 out 0 ?/8.2k  ? for help
```

```text
# after @120x23  docs/screenshots/lum1266-short-23.png.txt 面板 3（/help，frame e1f655d7c339）
row 2  |·   /settings show or change interface settings                       │
row 3  |·   /thinking set the reasoning level (/thinking off|minimal|…       │
…
row 21 |> type a prompt — /help for commands

# 面板 4（/hotkeys，frame 5f3160203622）
row 2  |·   Alt+N            start a new session                              │
…
row 18 |·   /               slash commands (/help, /model, /hotkeys, …)        ┃
```

同场景两帧**不再相同**（`e1f655d7c339` ≠ `5f3160203622`，退出码 0），聊天区从 1 行变成 17–19 行。

**高终端不受影响**（回归）：120×34 的 before/after 两帧逐行比较，**只有状态栏里的 session id 不同**，
其余 33 行完全一致（说明折叠只在装不下时触发，`total >= 24` 的布局一行没动）。

## 2. 缺陷 B：滚动条覆盖文字最后一列

### 2.1 现象（历史证据，LUM-1260 的帧）

`docs/screenshots/lum1260-tip-interaction.png.txt:536`（旧 tip，早于 LUM-1261 的逐源行渲染）：

```text
· /extensions list loaded extensions and what they register /exit quit the interactive session keys: Enter submit promp┃
```

`promp┃`——`prompt` 的最后一个 `t` 被滚动条的 `┃` 吃掉。根因：`scrollbar_geometry()` 把滚动条画在
`origin_x + width - 1`，而文字包装的可用宽度仍是整块 `width`，两者共用最后一列。

### 2.2 修法

```rust
// crates/pi-tui/src/app.rs
pub const SCROLLBAR_COLUMNS: u16 = 1;

fn viewport_for_render(&self, area: Rect, scrollbar: bool) -> (Rect, u16) {
    let reserved = if scrollbar && area.width > SCROLLBAR_COLUMNS && area.height > 0
        && self.messages.line_count(area.width) > area.height as usize { SCROLLBAR_COLUMNS } else { 0 };
    (Rect { width: area.width - reserved, ..area }, reserved)
}
```

- 渲染前把转写区宽度收窄 `W → W-1`，并把这 1 列记进 `viewport_reserved`（`record_viewport`）。
- `scrollbar_geometry()` 的列号改为 `origin_x + width + viewport_reserved - 1`——收窄后仍落在屏幕最右列。
- `paint_scroll_to_end`（"跳到最新"药丸）改用 `viewport_for_render` 的边界，药丸跟着挪到滚动条左边。
- `max_scroll` / 高亮 / 搜索命中仍基于 `area.width`，收窄后自动跟随，不会出现"按旧宽度算的滚动位置"。

**为什么不会来回抖**：收窄只会让换行更早发生，行数单调不减 ⇒ "整宽下已经溢出 ⇒ `W-1` 下仍然溢出" ⇒
`reserved` 的取值不会因为自身的收窄而翻转（测试 `the_viewport_gives_up_one_column_only_while_the_bar_is_drawn` 钉住该不变量）。

### 2.3 修好之后（真机帧，同一 tip 内“画不画滚动条”两种形态）

**关键场景**：`docs/screenshots/lum1266-longline.png.txt` 面板 3（120×23，frame `f84b7887ec21`，退出码 0）——
先 `/help` 把转写区顶到溢出（于是**画**滚动条），再回车一条 **130 列**的消息（`0123456789`×13）：

```text
row 17 |·                                                                        ┃
row 18 |> 0123456789…0123456┃      ← `> ` + 117 列正文 = 119 列，第 120 列是滚动条
row 19 |> 7890123456789        ┃      ← 余下 13 列，一个字符都没丢（117 + 13 = 130）
row 20 |  faux-model (faux) hello                                ┃
```

即：**画滚动条时正文只能拿 119 列**（旧代码拿 120 列，第 120 列正好被 `┃` 盖掉）。
**反向对照**（同一 tip、同一场景）：`lum1266-longline` 的面板 2（转写区还没溢出 → **不画**滚动条）里，
同一条 130 列消息折成整 120 列的两行（`> ` + 118 / `> ` + 12，130 个字符全在）。
“少一列”只在真的画了滚动条时发生——这正是
`short_viewport.rs::the_viewport_gives_up_one_column_only_while_the_bar_is_drawn` 钉住的不变量。

`docs/screenshots/lum1266-short-23.png.txt` 面板 3/4/5 也一致（`/help` 溢出、滚动条在画）：

```text
row 10 len=120 …'             ┃'   ← 滑块
row 11 len=120 …'             ┃'
row 12 len=120 …'             │'
row 21 len= 36 …'p for commands'    ← 输入框行不参与（它本来就不画滚动条）
```

不变量由 `crates/pi-tui/tests/short_viewport.rs` 三条测试覆盖（40 列、滚动条在第 39 列）：
占满整宽的正文行**保住**最后一列字符、宽度只在“确实要画滚动条”时少 1 列、“跳到最新”药丸停在滚动条左侧。

### 2.4 诚实条目：B 缺“旧二进制 vs 新二进制”的成对实拍

原计划是“`b8adfe6bd` 二进制 vs 合并后二进制”两套同场景实拍。实际只完成了**旧二进制的 3 组实拍**
（就是 §1.4 用到的 before 帧）；合并后的二进制在采集之后于共享磁盘上**被并行 run 清理掉**
（`pi-rust/target` 与 `/tmp` 下的留存副本一起消失，当时只剩 1.8G，重编第二个 binary 不现实；现已重编当前二进制并补采）。
所以 B 缺一半对照：

- **有**：同一 tip 内“画 / 不画滚动条”两种形态的实拍（§2.3）；3 条不变量测试；旧代码吃掉字符的历史帧
  `lum1260-tip-interaction.png.txt:536`（`promp┃`）。
- **没有**：用 `b8adfe6bd` 二进制跑 `lum1266-longline` 的那一帧。
  另外 `before-tall-34` 与本轮 `tall-34` 逐行只差 session id —— 说明 LUM-1261 之后，120 列的 `/help` 场景里
  没有行会填满末列，滚动条盖掉的只是补白空格：旧行为要“正好填满整宽的行”才露出损失，这正是 B 要消除的巧合。

## 3. 证据与复现

| 文件 | 二进制 | 结果 |
| --- | --- | --- |
| `docs/screenshots/lum1266-before-short-22.png(.txt)` | `b8adfe6bd` | 退出 0；18 行提示 + **0 行聊天区** |
| `docs/screenshots/lum1266-before-short-23.png(.txt)` | `b8adfe6bd` | **退出 1**：面板 3/4 同哈希 `9b19865217b3`（`/help` 与 `/hotkeys` 都画不出来） |
| `docs/screenshots/lum1266-before-tall-34.png(.txt)` | `b8adfe6bd` | 退出 0；高终端完整 21 行提示 |
| `docs/screenshots/lum1266-short-22.png(.txt)` | 本轮 | 退出 0；折叠行 + 17 行聊天区（`9a71526dc540` / `67f7dd4f46e9` / `79e38ea8b355`） |
| `docs/screenshots/lum1266-short-23.png(.txt)` | 本轮 | 退出 0；5 帧全不同（`5714967d1786` / `277b08a0e577` / `e1f655d7c339` / `5f3160203622` / `f8543542969f`） |
| `docs/screenshots/lum1266-tall-34.png(.txt)` | 本轮 | 退出 0；与 before-tall 逐行只差 session id |
| `docs/screenshots/lum1266-longline.png(.txt)` | 本轮 | 退出 0；3 帧全不同（`3a1916d0e95d` / `a2ab6b43ffb1` / `f84b7887ec21`）；面板 2 = 不画滚动条时 130 列消息占满整 120 列，面板 3 = 画滚动条时折成 119 列 + 第 120 列滚动条，字符一个不少 |

场景文件：`scripts/pty_scenarios/lum1266-{short-22,short-23,tall-34,longline}.json`（120×22 / 120×23 / 120×34 / 120×23，
`distinct_panels: true`，`--model faux/faux-model`）。采命令（在 `pi-rust/` 下）：

```bash
cargo build -p pi-coding-agent --offline
python3 scripts/pty_capture.py --bin ./target/debug/pi \
  --steps scripts/pty_scenarios/lum1266-short-23.json --out docs/screenshots/lum1266-short-23.png
```

> 采集提示：本机负载高时**冷启动首帧可能超过 10 秒**（本轮实测 `load average` 到过 **118**），会出现"面板 1 空帧"
> 甚至 `exec failed` 的假失败；`e3b0c44298fc` 这个帧哈希等于空屏，见到它先怀疑启动没赶上，别怀疑渲染。
> 三个短终端场景的面板 1 等待是 **8s**，`lum1266-longline` 是 **30s**（它要在面板 1 之后再发输入；
> 输入若落在启动之前，会被终端行规回显成"满屏空白 + 回显两行"的假象）。采集前先看一眼 `uptime`。

代码测试：

```bash
cargo test -p pi-tui --offline              # 805 passed; 0 failed（LUM-1261 tip 是 798，本轮 +7）
cargo clippy -p pi-tui --all-targets --offline
```

本轮新增/改动的测试：
`crates/pi-tui/tests/short_viewport.rs`（新文件，5 条：滚动条列不变量 ×3、矮终端折叠后输入框仍在 ×2）、
`crates/pi-tui/tests/startup_header.rs`（+1：真实键位表下 100×23 折叠后输入框行 = `HEIGHT-2`、转写区 ≥3 行）、
`crates/pi-tui/tests/extension_ui.rs`（+1：`plan_chrome` 在 2..24 行全高度都不饿死输入框）。

## 4. 本轮**没有**做的（诚实条目）

1. **低于 24 行没有"终端太小"提示**：Martty 有这类守卫，这里选择"降级而不是拒绝"——转写区给到 3 行即认为可用
   （`MIN_TRANSCRIPT_ROWS`）。低于 6 行（标题+折叠行+3+2）时视觉上已经很挤，但功能不残缺，所以不加守卫。
2. **没有把折叠做成可配置**：折叠阈值是常量推导（`HEADER_RESERVED_ROWS`），没有 `settings` 开关，
   也没有"折叠后仍记得用户上次手动展开状态"的持久化。`Alt+H` 手动切换仍然有效。
3. **滚动条仍是 1 列字符**（`│`/`┃`），没有做 codex/pi 那种"细滚动条 + 拖拽手柄"的更宽滚动条样式。
4. **80 列下超宽行的列对齐**（LUM-1261 §7.4 第 2 条）仍然存在，本轮不碰。
5. **B 缺一份"同场景的旧二进制"帧**（见 §2.4）：B 目前是"同一 tip 两种形态的实拍 + 3 条不变量测试 + 历史帧"三级证据，
   不是成对 A/B。
6. **滚动条的鼠标命中区（`mouse_region`）未复核**：列预留只保证绘制不重叠；点击/拖拽滚动条的命中列是否要同步位移没有测。

## 5. 指标（本轮口径，命令可复现）

| 指标 | 值 | 命令 |
| --- | --- | --- |
| Rust `#[test]` 数 | 1,972 | `grep -rho '#\[test\]' crates --include=*.rs \| wc -l` |
| Rust `#[tokio::test]` 数 | 384 | 同上换 `#[tokio::test]`（合计 **2,356**） |
| `src` 行数 | 130,160 | `find crates -path '*/src/*' -name '*.rs' \| xargs wc -l \| tail -1` |
| `tests` 行数 | 51,617 | 同上换 `*/tests/*` |
| `app.*` id 接线 | 35/44 = 79.5% | 见 `RUST_TS_PARITY_METRICS.md` §0.2（本轮未变） |
| 加权完成度（对 TS 版） | **81.4%** | 见 §0.2；本轮只动 TUI 布局，不改口径 |

与 TS 版的差距仍然是结构性的（LOC ~85%、测试用例 ~44%、扩展生命周期事件 58.3%），
本轮是"把已经画出来的界面在矮终端上修正确"，不是补功能面。
