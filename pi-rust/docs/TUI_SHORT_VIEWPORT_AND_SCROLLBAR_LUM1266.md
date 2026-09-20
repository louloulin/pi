# 短视口 composer + 滚动条独立列（LUM-1266）

> 分支 `feature/pi.rs`。改动只在 `pi-tui`：`plan_chrome` 的高度预算、启动头部按帧折叠、
> 消息视口为聊天滚动条让出 1 列。证据 = 真 PTY 帧（`docs/screenshots/lum1266-*.png` +
> 同名 `.txt` 字符网格与帧哈希）+ 6 个新用例。
>
> 上一轮（LUM-1260）只做到了"定位 + 取证"，代码一行没动；本轮把两处 P0 都改掉。

## 0. 结论

| 场景 | LUM-1260 的帧 | 本轮 |
|---|---|---|
| 120×22 / 120×23，启动头部开启 | **composer 一行都没有**，光标停在状态栏行（`lum1260-small-terminal-23.png.txt` 第 5 行起 19 行提示 + 空行 + onboarding = 21 行，之后直接是状态栏） | 头部折叠成 `pi v0.1.0` + `hints hidden on a short terminal — Alt+H shows them`，composer 在第 21 行、光标可见（`lum1266-short-22/23.png.txt`） |
| 120×34 | 展开的提示表 | 不变（装得下就不折叠，`lum1266-tall-34.png.txt` 第 1 面板） |
| 120×34，`/help` + 滚动条 | 行尾 `…Enter submit promp┃`——**`prompt` 的 `t` 被滚动条覆盖** | 行为 `…Enter submit` + 次行 `prompt Up / Down …`，一个字符都没丢 |

三组场景共 11 帧，帧哈希两两不同（`distinct_panels: true` 下 harness 未报错），
说明这些画面是真实交互序列，不是同一帧复制。

## 1. 缺陷 A：`plan_chrome` 把 composer 饿死

### 1.1 现场

`docs/screenshots/lum1260-small-terminal-23.png.txt`（旧 tip `b5769b585`，120×23，光标
`(120,22)`）：19 行键位提示 + 空行 + onboarding 占满 21 行，之后只有状态栏。用户能打字，
但屏幕上看不到自己打了什么。

### 1.2 根因

旧的 `plan_chrome` 只预留 `status(1)` + `message(1)`，其余按渲染顺序 **先给 header**。
`builtin_header_lines()` 在 120 宽下展开是 21 行，于是 `editor` 分到 0 行，`paint_prompt`
拿到 `height == 0` 直接 return，composer 从未落到 buffer 上。

### 1.3 修法

`crates/pi-tui/src/extension_ui.rs:411` `plan_chrome`：在任何扩展区域之前先预留
**状态行 1 行 + 编辑器自身 1 行（`editor_want`，多行 composer 时按需）+ 消息区 1 行**，
扩展区域只能花剩下的预算，末尾区域（footer/below）先被截断。

- 3 行以上终端：composer 与消息区必定各有一行，头部先截尾。
- 恰好 2 行：只剩 `状态 + 1 行`，那一行留给消息区（终端此时不可用，策略写进模块注释）。

`ChromeLayout` 的字段与不变量不变，调用方（`composed_frame` → `plan_chrome` → 各
`*_rect`）无需感知。

### 1.4 头部按帧自动折叠（A2）

只在 `plan_chrome` 里留位还不够——头部会把 21 行全画在 transcript 的地盘上。
`crates/pi-tui/src/app.rs:1501` `builtin_header_lines(total_height)`：

- 展开行数 + `HEADER_RESERVED_ROWS`（= 3 transcript + composer + 状态行 = 5）> 终端高度 →
  本次渲染丢弃提示表，保留产品行（`header_title_lines`，`app.rs:1532`）；
- 若还剩得下 1 行，追加一行暗色提示（`locale.rs:241` `header_folded_line`，
  EN `hints hidden on a short terminal — Alt+H shows them` / ZH `终端太矮，键位提示已折叠 — Alt+H 展开`），
  键位取自 `app.header` 的实时 chord，未绑定时这一行自动消失；
- **不修改 `header_expanded`**：短终端下用户按 `Alt+H` 仍会展开（列表被 `plan_chrome` 截断，
  composer 依然在），长终端行为与之前逐字节一致。

## 2. 缺陷 B：滚动条覆盖正文最后一列

### 2.1 字符级现场对比

同一份 `/help` 文本、同样 120×34、同样滚到底（旧帧 = `lum1260-tip-interaction.png.txt` 面板 15，
新帧 = `lum1266-tall-34.png.txt` 面板 3）：

```text
旧：· /extensions list loaded extensions and what they register /exit quit the interactive session keys: Enter submit promp┃
新：· /extensions list loaded extensions and what they register /exit quit the interactive session keys: Enter submit      ┃
    · prompt Up / Down navigate prompt history PgUp/PgDn scroll the chat log one page Home / End jump to the start / end of┃
```

旧帧把整段文本按 120 列折行，然后滚动条画在 `origin_x + width - 1 = 119`（0 基）——也就是
**正文的最后一列**上，凡是正好写满最后一列的字都被吃掉（这里丢了 `prompt` 的 `t`，
后面整段因少一个字符而错位；拼接全文旧 1239 字符 vs 新 1240 字符）。

### 2.2 修法

- `app.rs:4504` `viewport_for_render(area, scrollbar)`：只有当这一帧 **确实要画滚动条**
  （`line_count(width) > height`）时，才把消息区宽度减 `SCROLLBAR_COLUMNS = 1`；
  文本折行、命中测试、选区/搜索高亮全部用同一个宽度。
- `record_viewport(message_area, reserved)`（`app.rs:4529`）把让出的列数记进
  `viewport_reserved`，**只**给滚动条用。
- `scrollbar_geometry()`（`app.rs:3417`）的列改成
  `origin_x + width + viewport_reserved - 1`：文字在自己变窄的列里收尾，滚动条回到终端
  最右列（120/120）。
- `paint_scroll_to_end()`（`app.rs:3354`）的可用宽度用 `.min(message_area.width)` 夹住，
  "↓ Jump to latest message" 药丸不会越到滚动条上。
- `render_snapshot(width, height)`（`scrollbar = false`）逐字节不变：不画滚动条就不让列，
  EOF 快照与既有断言不受影响。

### 2.3 为什么不会来回抖动

只有当**满宽**下已经溢出才会让出 1 列；折行宽度变小只可能让行数变多、不会变少，
所以"满宽溢出 ⇒ 减 1 列后仍溢出"，判定不会在两个状态间振荡。

## 3. 验证

新增/改写的用例：

| 文件 | 用例 | 断言 |
|---|---|---|
| `crates/pi-tui/src/extension_ui.rs:749` | `plan_chrome_never_starves_the_composer` | 高度 2..24 循环：composer 恒有 1 行（2 行终端除外），消息区 ≥1 行 |
| `crates/pi-tui/tests/extension_ui.rs` | `an_over_tall_region_is_truncated_and_the_message_view_survives` | 30×5：头部只剩 2 行，**composer 与消息行同时存在** |
| `crates/pi-tui/tests/startup_header.rs` | `the_header_folds_on_a_terminal_that_cannot_hold_it` | 真键位表、100×23：提示表折叠、折叠提示行出现、composer 在倒数第二行、消息区 ≥3 行 |
| `crates/pi-tui/tests/short_viewport.rs` | `a_full_width_row_keeps_its_last_column_next_to_the_bar` | 满宽行一个字符不少，末列是文字、终端最右列是滚动条 |
| 同上 | `the_viewport_gives_up_one_column_only_while_the_bar_is_drawn` | 不溢出时 `viewport() == (40, 8)`、无滚动条；溢出时 `(39, 8)`、`geometry().column == 39` |
| 同上 | `the_jump_to_latest_pill_stays_left_of_the_bar` | 游离视图下药丸右边界 ≤ 滚动条列，且真的画在屏幕上 |
| 同上 | `a_short_terminal_folds_the_header_and_still_paints_the_composer` | 40×10/12/23：composer 恒在 `height-2` 行，状态栏在最后一行 |
| 同上 | `a_tall_terminal_keeps_the_expanded_header_and_the_composer` | 40×24：提示表仍展开 |

命令（都在 `pi-rust/` 下）：

```bash
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_DEV_INCREMENTAL=false cargo test -p pi-tui --offline
# 结果：351 passed（lib）+ 32 个集成测试文件全绿，0 failed
cargo clippy -p pi-tui --all-targets --offline   # 无 warning
cargo fmt -p pi-tui -- --check                   # 无 diff

cargo build -p pi-coding-agent --offline
python3 scripts/pty_capture.py --bin ./target/debug/pi \
  --steps scripts/pty_scenarios/lum1266-short-22.json  --out docs/screenshots/lum1266-short-22.png
python3 scripts/pty_capture.py --bin ./target/debug/pi \
  --steps scripts/pty_scenarios/lum1266-short-23.json  --out docs/screenshots/lum1266-short-23.png
python3 scripts/pty_capture.py --bin ./target/debug/pi \
  --steps scripts/pty_scenarios/lum1266-tall-34.json   --out docs/screenshots/lum1266-tall-34.png
```

帧哈希（来自各截图同名 `.txt`）：

- `lum1266-short-22.png`：`0c9daefe4217` / `55b93f701458` / `25f991284b9a`
- `lum1266-short-23.png`：`fad20f1094c8` / `457da095fad6` / `087605abe335` / `7c8bc52e3860` / `c2ea11f89242`
- `lum1266-tall-34.png`：3 帧，`/help` 面板行尾列为滚动条、正文列 1..119

另外 `lum1266-short-23.png.txt` 面板 5（PgUp）是"滚动条 + 部分滚动 + 药丸"同框的画面。

## 4. 没做的事（诚实清单）

1. **`/help` 的段落重排**：`info_block` 把换行折成空格再整体折行，于是 `/help` 每个命令
   不再独占一行、续行行首还会再打一个 `· `。上一轮就记录在案的
   "`·` reflow finding"，属于 `info_block` 渲染策略，本轮没碰（改它会动 LUM-1259 的断言）。
2. **药丸压在正文上**：`↓ Jump to latest message` 是覆盖式绘制，行内其余文字仍在（见
   `lum1266-short-23.png.txt` 面板 5）。LUM-1257 的既定设计，本轮只保证它不越到滚动条列。
3. **"终端过小"守卫**：Martty 有这类硬守卫。本轮 planner 已经能在 3 行以上优雅降级
   （composer 恒在），再加守卫只增加状态与风险，故不加，改为文档化。
4. **`app.*` 静默 id**：仍 7 个（`app.models.clearAll/enableAll/reorderDown/reorderUp/save/toggleProvider`、
   `app.tree.editLabel`），LUM-1263 负责。
5. **指标**：`#[test]` 2,338 → 2,344（+6），LOC `crates/*/src/**/*.rs` 129,960 行；
   `app.*` id 接线率不变（35/44 = 79.5%）。本轮的收益在"可用性"而不在接线率上，
   加权口径（TUI 交互+视觉）不变，故不改 `RUST_TS_PARITY_METRICS.md` 的总分。
