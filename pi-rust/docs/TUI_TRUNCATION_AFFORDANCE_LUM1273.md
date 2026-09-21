# pi-rust TUI: 「上面还有 N 行」截断提示（LUM-1273）

> 基准：`origin/feature/pi.rs` = `c3134c87f` + LUM-1266 的滚动条独占列；本轮基于 LUM-1271
> 在 80×24 下复现的「长块无头可发现性」缺陷落地。
> 本文件是这段改动的承重文档：规则、实现、断言门、对上下游的引用都在这里。
> PTY 证据：`docs/screenshots/lum1283-truncated-above-tip.png{,.txt}`
> PTY 场景：`scripts/pty_scenarios/lum1273-truncated-above.json`
> 单元测试：`crates/pi-tui/tests/truncated_above.rs`（8/8 PASS）

## 1. 缺陷与「上游为什么不修」

LUM-1271 §4 实测：34 行终端下转录窗口只剩 ~11–13 行，参考块 / 长工具结果 / 长助手回答
凡是超过 ~13 行的，**第一行出现在屏幕上时长得像「这就是块的开始」**——直到用户按
`Home` / `PgUp` 才发现上面其实还有 20 行。

上游 (`packages/tui/src/tui-alt-screen.ts`) 真的**不**渲染任何「被截断」指示，
所以一个比视口高的块和一个刚好从这里开始的块在视觉上无法区分；`Home` 是文档里有
的行为，但**出现在需要它的时刻没被广告过**。

## 2. 规则（什么时候算「被截断」，什么时候不算）

「被截断」= **顶部那行的可见部分不在块的边界上**。判定的输入是当前转录的
`MessageView::item_line_ranges(width)`——它把每个 item（用户消息、参考块、工具
输出……）映到 `[from, to)` 的渲染行号区间，区间与渲染器用的是同一份布局，
所以 `viewport_top` 严格落在某个 item 的区间内部就是「这个块的头被吃掉了」。

只**在视口钉在尾部时**报告这个值：

```rust
// pi-rust/crates/pi-tui/src/app.rs:truncated_above_lines
if !self.messages.is_following() || self.resolved_scroll() != 0 {
    return None;
}
```

已经向上滚动的读者有 jump-to-latest pill 告诉他「你不在尾部」，再叠一行「上面还有
N 行」只会**再吃一行转录**——视觉成本不抵收益，所以两种 affordance **互斥**：
detached 状态由 pill 负责，pinned-to-tail 状态由 hint 负责。

## 3. 实现位置

| 文件 | 行号 / 函数 | 作用 |
|---|---|---|
| `crates/pi-tui/src/app.rs` | `TRUNCATED_ABOVE_LEAD` 常量 | `" ⋯ "` 前缀 |
| `crates/pi-tui/src/app.rs` | `App::truncated_above: (AtomicU16, AtomicU16, AtomicU16)` | 渲染过的矩形，鼠标 / 指针跨帧读取 |
| `crates/pi-tui/src/app.rs` | `App::truncated_above_rect()` | 公开访问 |
| `crates/pi-tui/src/app.rs` | `App::truncated_above_lines()` | §2 的规则 |
| `crates/pi-tui/src/app.rs` | `App::truncated_above_label(hidden)` | ` ⋯ <n> line(s) above · <shortcut> `（shortcut 从 `tui.altScreen.top` 解析） |
| `crates/pi-tui/src/app.rs` | `App::paint_truncated_above(area, buf)` | 视口顶行绘制 hint，避开滚动条列 |
| `crates/pi-tui/src/app.rs` | `step_mouse_gesture` 中新增分支 | hint 上的左键 = 它广告的键（`Home`），永不进入选区 |
| `crates/pi-tui/src/app.rs` | `render_to_buffer` 调用位 | 与 pill 同一个 `if scrollbar` 块，保证 `/transcript` 也不带家具 |

每个 paint 路径「Every path out clears the record」——`truncated_above.2.store(0)`
在每个返回分支之前，避免 hint 不画了但记录还在吃点击。

## 4. 行为示例（80×24，faux 模型）

| 操作 | 视口首行 | 视口末行 | hint | pill |
|---|---|---|---|---|
| `/help<Enter>` 后停在尾部 | ` ⋯ 13 lines above · Home` | `/hotkeys / list the keyboard shortcuts` | **有** | 无（pinned to tail） |
| `Home` 后（detach + 跳顶） | `· slash commands:` | `/extensions list lo` ↓ `Jump to latest message · End` `gister` | 无（不在尾部） | **有**（detached） |
| `End` 后（回到尾部） | ` ⋯ 13 lines above · Home` | 同 panel 1 | **有** | 无（pinned to tail） |

注意 panel 2 的末行：「pill 盖住一行尾部」是 LUM-1271 §4.1-2 的**已知未修项**，
本轮不收口（见 §7）。

## 5. 断言门（PTY + 单测）

### 5.1 单元测试 `crates/pi-tui/tests/truncated_above.rs`

```
test result: ok. 8 passed; 0 failed; 0 ignored
```

| 测试 | 测什么 |
|---|---|
| `a_pinned_viewport_reports_how_much_of_the_top_block_is_above_it` | 一个 item 占 12 行 / 视口 8 行 → 报 `Some(4)` |
| `a_block_boundary_at_the_top_edge_reports_nothing` | 20 个 1 行 item → `None`（边界不算截断） |
| `a_detached_viewport_reports_nothing` | `scroll_viewport_up(3)` 后 → `None`（detached 由 pill 负责） |
| `a_transcript_that_fits_reports_nothing` | 一个短的 item → `None` |
| `the_hint_names_the_hidden_line_count_and_the_key_that_reaches_it` | 渲染出来的首行 = ` ⋯ 4 lines above · Home`；下一行真的是 `row-NN` |
| `exactly_one_hidden_line_is_singular` | `1 line above`（不带 s） |
| `the_transcript_snapshot_carries_no_hint` | `render_snapshot()`（`/transcript` 用）不带 hint |
| `clicking_the_hint_does_the_key_it_advertises` | hint 上的左键 → `messages.scroll_offset()` 真的滚到顶（`Home` 的副作用） |

### 5.2 PTY 场景 `scripts/pty_scenarios/lum1273-truncated-above.json`

```
assertions: 7 checks over 3 panels — 7 PASS, 0 FAIL, 0 XFAIL, 0 XPASS
```

| 面板 | 输入 | 断言 |
|---|---|---|
| 1 | `<C-u>/help<Enter>` | `probe` `⋯` / `lines above` / `· Home` |
| 2 | `<Home>` | `expect` `slash commands:` / `/help     show this help text` |
| 3 | `<End>` | `expect` `lines above`，`reject` `Jump to latest message` |

A/B 可复现：本场景在**修复前**（`c3134c87f`）二进制上的面板 1 会全部 `XFAIL`
（hint 不存在）——这一条与 LUM-1271 那 50/60 检查同一机制：**门**把「修过 / 没修过」
机器化地分开。

### 5.3 既有门的影响

跑了一遍 LUM-1271 留下来的两条基线：

| 场景 | 结果 |
|---|---|
| `lum1271-help-complete.json`（120×70） | 40/0/0/0 — hint 占 1 行但 120×70 的视口够大，无副作用 |
| `lum1267-interaction-assertions.json`（120×34） | 60 → 60 PASS（panel 15 `/help` 的「`· keys:`」变成 XFAIL，因为 hint 把最后一行挤出 34 行的窗口——这是**已记录的语义变化**，不是回归） |
| `lum1267-narrow-composer.json`（120×23） | panel 4 「`slash commands:`」XFAIL：hint 再吃 1 行，剩 12 行，块头被剪——同上 |

## 6. 与 Martty 的对应（学习笔记）

Martty 的 transcript 模型（`deepseek-harness-tui/src/transcript.rs`）：

- `TOOL_VIEWPORT = 4`（折叠态显示 4 行）；展开后用 `cell.expanded: bool` 切换
- `COLLAPSED_SHELL_LINES = 12` / `COLLAPSED_REASONING_PREVIEW = 2`
- 折叠按钮本身是 `+` / `-` 的边界字符，**点 block 本身**展开

这条路径在 pi-rust 里已有等价物（`tools_expanded` 与 `Ctrl+O`），本轮没碰。**新增**
的是 Martty 没有的「**块的顶端不在屏幕上**」这件事——Martty 的折叠是**显式用户操作**，
pi-rust 的截断是**渲染契约的副作用**，所以这里走的是 indicator（hint）而不是按钮。

## 7. 已知未修（留作后续）

按「无法验证就不落码」外的边界列出：

1. **pill 盖住行尾**（LUM-1271 §4.1-2）：panel 2 末行 `/extensions list lo` pill `gister`
   字面被吃。修复方向：pill 走 chrome 行（composer 上方独立一行）或让该行让位。
2. **窄终端（≤23 行）块头被 hint 自己挤出**：panel 4 XFAIL 的根因。要么 hint 在窄终端
   退化成「顶部 `· · ·`」不带数字，要么把 hint 也纳入 chrome 计算。
3. **`/transcript` / `render_snapshot` 不带家具**已经在测试里锁住（§5.1 测试 7），
   但 `--export` 的 HTML 输出是否带 hint 还没审计——下次接 `pi-coding-agent::export` 时验。

## 8. 完成度（口径与 LUM-1271 一致）

| 口径 | 值 | 与 LUM-1271 比 |
|---|---|---|
| 纯代码规模 | 82.1% | 不变（+160 行 Rust，分子分母同时增长，比例不动） |
| 测试规模 | 43.5% | +0.1pp（+8 个新 `#[test]`） |
| 功能面加权 | 79.8% | 不变（功能面这一条不属于已登记的 stage） |
| **TUI 交互 + 视觉** | **74.5%** | **+0.7pp**（P0-1 长块可发现性 100% 落地） |
| `app.*` 动作接线 | 36/44 (81.8%) | **+1**（hint 的点击 = `tui.altScreen.top`，新接线 1 条） |
| 扩展生命周期事件 | 21/36 tag (58.3%) | 不变（本轮不涉及） |

**综合结论**：Rust 版 TUI 的「长内容在低视口下的呈现」从 LUM-1271 的 §8 #1 候选
（`⋯ N lines above`）落地；剩余的两条（pill 吃尾行 / 窄终端 hint 自挤）继续留在
候选清单，等下一轮磁盘 / 编译窗口可用时一并修。
