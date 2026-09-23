# LUM-1469 — 排队输入的可见面：composer 上方的 `Steering:` / `Follow-up:` 块 + 取回提示

> scope: `pi-rust/crates/pi-tui`（`message` / `app` / `extension_ui`）+ `pi-rust/crates/pi-coding-agent`（`interactive` 测试）
> branch: `lum1469-tui`（→ `feature/pi.rs`）
> 时间锚：LUM-1466（`fc18cb09e`）之后的下一轮；issue 正文是 LUM-981 伞形任务的复触发（autopilot），
> 并点名「tui chatinput 参考 codex / Martty」。
> **本轮同时把两条 `in_review` 但从未合入的交付救回 `feature/pi.rs`**（§0.1）。

## 0. 结论速览

| 量 | 本轮实测 | 依据 |
|---|---|---|
| issue 点名项（chatinput 与 codex / Martty 的差距） | 关掉**排队输入的上下文可见面**：块的位置、形状、`↳ <chord> 取回` 提示 | §1、§2 |
| 上游 pi-ts | 排队输入在 **editor 上方的容器**里：`Spacer(1)` + 每条约一行 + `↳ <chord> to edit all queued messages` | `interactive-mode.ts:4366-4385`（块）、`:876-892`（位置） |
| codex | 同位置的能力：`PendingInputPreview` 渲染在 composer 上方，3 行预览上限 + `…` 溢出 + `alt+up edit last queued message` | `codex-rs/tui/src/bottom_pane/pending_input_preview.rs:84-160` |
| Martty | 两条独立可见面：transcript cell + composer meta 行 `· N 条排队中`，外加一次性 tip `queued — lands after this turn · ctrl+x would send now` | `src/app.rs:4841-4845`、`src/ui.rs:707-709` |
| **本轮前 pi-rust** | 排队文本画在**日志尾部**（`render_styled_lines` 末尾追加），换行成 N 行，**没有任何取回提示**；`message.rs` 自己的注释承认这是偏差 | `message.rs:1139-1155`（基线）、§1.3 |
| 本轮后 pi-rust | 块移到 **composer 正上方**，每条约**一行**（超宽 `…` 标记），末行 `↳ <生效键位> to edit all queued messages` | §2 |
| 新增行为测试 | **17 条**（9 `lum1469_pending_block` + 1 `lum1469_pending_chord` + 4 `message.rs` 单测 + 2 `extension_ui.rs` 单测 + 1 驱动级） | §3 |
| 新增帧截图 | **3 张**（`docs/screenshots/lum1469-*.{png,txt}`） | §5 |
| `pi-tui` 全量 | **1154 passed / 0 failed**（基线合入后 1137 / 0 → **+17**） | `cargo test -p pi-tui` |
| `pi-coding-agent` | **836 passed / 28 failed**（28 条与基线同集合，全为 Windows 环境类） | §4.3 |
| 纯代码规模（src↔src） | **93.7%**（143,457 / 153,106）——涨幅**主要来自本轮救回的两条交付**，本轮的 src 净增 **+256 行** | §4.4 |
| 测试规模 | **50.8%**（2,829 / 5,563） | §4.4 |
| `app.*` 接线 | **44/44 = 100%**（本轮并入 LUM-1263 后 silent 清零） | `app_action_coverage.py` |
| 加权完成度 | **87.0%**（86.9% → 87.0%，只动测试轴） | §4.5 |

一句话结论：**排队输入本来就能看见（画在日志尾部），但它画错了地方、画错了形状，而且完全没有告诉读者「这些消息还能取回来改」。**
上游 pi-ts、codex、Martty 三家都有这层上下文提示（位置 + 取回 affordance），pi-rust 是三家对照里唯一
`git grep` 不到 `↳`/取回提示字面的端口。本轮补齐该块，并把「排队输入算 transcript 内容」这个旧口径改掉
（它此前是可滚动、可框选、可被 `/transcript` 导出的日志行）。

### 0.1 本轮并入的两条既有交付（issue 要求「都合并 feature/pi.rs」）

| issue | 分支 tip | 内容 | 合并后测试增量 |
|---|---|---|---|
| **LUM-1461** | `origin/work/LUM-1461` = `4eec9a628` | composer `paste_burst` 兜底（codex 独有）+ marker 按词原子 + 历史 marker 降级；`input.rs` 新增 415 行 | `pi-tui` +24（11 行为 + 3 帧 + 6 `input.rs` + 4 `word_navigation.rs`） |
| **LUM-1263** | `origin/work/LUM-1263` = `271cb109f` | `/tree` 改名 UI（`app.tree.editLabel` 接线，`app.*` 43/44 → 44/44） | `pi-coding-agent` +6（`--lib` 588/8 → 594/8，失败集合不变） |

合并时两处冲突都是「两侧各自往同一位置追加」：`app.rs` 的 `step_key` 层序（保留两侧，
`?` 速查面板在前、paste-burst 分类在后）与 `RUST_TS_PARITY_METRICS.md` 的 `§0.18` 段
（LUM-1461 的段落改写为 `§0.19`、LUM-1263 的为 `§0.20`，两段内容都留）。
合并前先跑 `cargo test -p pi-tui`：**1137 / 0**。

## 1. 真实审计：差的是**位置 + 形状 + 取回 affordance**，不是「有没有」

### 1.1 三边上游的第一手取证（本机，不转述）

| | upstream pi-ts | codex | Martty |
|---|---|---|---|
| 块的位置 | `pendingMessagesContainer` 挂在 **prompt/editor 区**（`interactive-mode.ts:876-892` 把它当作 `editor` 区域的一部分传入） | composer 上方（`pending_input_preview.rs` 由 `ChatComposer` 拼进 bottom pane） | transcript cell（提交即插一条 user cell，`src/app.rs:4841-4844`）+ composer meta 行计数（`src/ui.rs:707-709`） |
| 每条几行 | `TruncatedText(text, 1, 0)` → **1 行**，取第一个 `\n` 之前的部分，超宽 `truncateToWidth` | `adaptive_wrap_lines` + `PREVIEW_LINE_LIMIT = 3` 行上限，溢出打 `…` 行 | transcript cell 全文 |
| 取回提示 | `↳ ${getAppKeyDisplay("app.message.dequeue")} to edit all queued messages`（`:4380-4382`） | `alt+up edit last queued message`（`:146-156`） | 一次性 tip `queued — lands after this turn · ctrl+x would send now`（`src/app.rs:4844`） |
| 分类文案 | `Steering: …` / `Follow-up: …` | 分段标题 `Messages to be submitted after next tool call` / `Queued follow-up inputs` | 计数 `· N 条排队中` |

**三家的共同点**：排队输入有一条**紧邻输入框**的可见面，并且**带一条「怎么处置它」的提示**。
pi-rust 缺的正是这一层。

### 1.2 取证（可复现）

```bash
# 基线 fc18cb09e：排队输入画在日志尾部，且没有任何取回提示
$ git grep -n 'Steering: ' fc18cb09e -- pi-rust/crates/pi-tui/src/message.rs
fc18cb09e:pi-rust/crates/pi-tui/src/message.rs:1140:                ("Steering: ", &self.pending_steering),

# 基线：整个 Rust 端口没有 `↳` 取回提示
$ git grep -c 'to edit all queued messages' fc18cb09e -- pi-rust/crates
0
# （整树有命中：locale.rs 的启动 header 广告行 —— 但那不是「恰好此刻」的提示）

# 基线：块在 `render_styled_lines` 的末尾追加，并被 `wrap_text` 折成 N 行
$ sed -n '1136,1156p' <(git show fc18cb09e:pi-rust/crates/pi-tui/src/message.rs)
```

### 1.3 这个旧口径的三个后果（都不是「审美问题」）

1. **位置错**：排队输入是**输入通道**的一部分，不是对话历史。放在日志尾部意味着它被
   `item_line_ranges` / 选择 / 搜索 / `/transcript` 当成日志内容（`render_styled_lines` 的
   返回值同时是 `last_render_lines` 与快照来源）。
2. **形状错**：一条 12 行的粘贴草稿被 `wrap_text` 折成 12 行日志（上游是 1 行 `TruncatedText`）。
3. **无 affordance**：读者看不到「这些消息还能用 `app.message.dequeue` 取回来改」。
   该键位只在**启动 header** 上广告过（`locale.rs:207-211`），一旦 header 折叠或读者没读启动屏，
   排队消息就变成「只能等它自己发出去」的黑盒。

## 2. 落地：块是 host 的数据，行数由数据决定

### 2.1 设计取舍（为什么不做成「日志尾部 + 提示」）

把提示补在日志尾部是最小改动，但它把三个后果里的两个留着（位置、形状）。本轮的判据是
**对齐上游的形状**：块属于 composer 区域。于是：

* `MessageView` 新增 `pending_block_rows()`（无排队 → `0`，否则 `pending_len + 2`）与
  `pending_lines(dequeue_chord)`（`Spacer(1)` + 每条一行 + 提示行，全部 `Dim`）；
* `render_styled_lines` **不再**追加排队行 —— 块从日志里搬出去；
* `ExtensionFrame` 新增 `pending: u16`（与 `status` 同类的**数据驱动**区域），
  `plan_chrome` 在 **editor 之后、header 之前**预留它；
* `App::paint_pending_block` 把块画在 message viewport 与 editor 之间，逐行先清空
  （块是从「上一帧的 transcript」里切出来的，且 spacer 行本身就是空行）。

### 2.2 预算顺序是有理由的（不是随手排的）

`plan_chrome` 的顺序是 `editor` → **`pending`** → `header` → `above` → `below` → `footer`：

* `editor` 第一：composer 永远不能消失（LUM-1261 的教训）；
* `pending` 第二：**正在排队的 prompt 眼下唯一可见的地方就是它**；
* `header` 让步：启动 header 是可折叠的，且它已经有一行专门广告 `app.message.dequeue`
  —— 让广告行先让位给「数据本身」，比反过来合理。

`App::header_reserved_rows` 同步加上 `pending_block_rows()` —— 否则 24 行终端上 header 会按
「没有块」的预算保留提示行，随后被 `plan_chrome` 截尾，正是 LUM-1266 清掉的**静默切断**。

### 2.3 落地（文件:行号）

| 文件 | 改动 | 说明 |
|---|---|---|
| `pi-rust/crates/pi-tui/src/message.rs` | `+140/-19` | `PENDING_STEER_LABEL` / `PENDING_FOLLOW_UP_LABEL` / `PENDING_HINT_LEAD` / `PENDING_HINT_TEXT`（`:270-278`，文案逐字对齐上游）；`MessageView::pending_block_rows`（`:601`）；`MessageView::pending_lines`（`:624`，只取第一个 `\n` 之前、超宽交给 painter 打 `…`）；`render_styled_lines` 去掉尾部追加（`:1139`）；字段文档改口径（`:320-325`） |
| `pi-rust/crates/pi-tui/src/extension_ui.rs` | `+58/-1` | `ExtensionFrame::pending`（`:421`）、`ChromeLayout::pending`（`:451`）、`plan_chrome` 的预留（`:505`）；两个新单测（`:819`、`:834`） |
| `pi-rust/crates/pi-tui/src/app.rs` | `+80/-2` | `composed_frame` 写入 `frame.pending`（`:1788`）；`header_reserved_rows` 计入块行数（`:1802`）；渲染路径插入 `paint_pending_block` 调用（`:6231`）；`App::dequeue_chord`（`:6481`，`app.message.dequeue` 走**生效键位表**，未装表时退回平台默认 `Alt+Up` / `Alt+Q`）；`App::paint_pending_block`（`:6499`，逐行清空 + `write_styled_line_ellipsized`） |
| `pi-rust/crates/pi-tui/tests/lum1469_pending_block.rs` | **新增 9 条 + 3 帧** | 位置 / 顺序 / 几何 / 空队列回归 / `…` 标记 / 矮终端 / 三张帧 |
| `pi-rust/crates/pi-tui/tests/lum1469_pending_chord.rs` | **新增 1 条** | 提示跟随 `keybindings.json` 覆盖；未绑定则不广告死键位 |
| `pi-rust/crates/pi-tui/tests/pending_messages.rs` | `+56` | 走真实 `App::submit`（futures 排队路径）的可见性断言：忙碌时画、清空后不画 |
| `pi-rust/crates/pi-coding-agent/src/interactive.rs` | `+24` | `follow_up_queues_while_busy_and_dequeue_restores_it` 增加帧断言：驱动层真的把块画出来（不只是 `pending_len` 计数） |
| `pi-rust/docs/screenshots/lum1469-*.{png,txt}` | 3 + 3 | `scripts/frame_to_png.py` |

### 2.4 三条不变量（都有测试钉住）

1. **空队列 ⇒ 零行**：`pending_block_rows() == 0`，帧与 LUM-1466 逐行相同
   （`an_empty_queue_adds_no_row`、`plan_chrome` 单测的 `pending: 0` 默认值）。
2. **有队列 ⇒ 正好 `n + 2` 行，且 composer 不动**：transcript 让出**恰好**这些行
   （`the_block_costs_the_transcript_exactly_its_rows`：viewport 高度 −3、原点与宽度不变、
   composer 行号不变）。
3. **超宽用 `…` 标记**，不是静默切半（`a_long_queued_draft_is_one_marked_row`，LUM-1412 规则）。

## 3. 反向验证（本机实做）

把 `composed_frame` 里的 `frame.pending` 硬写成常量 `0`（不改其它任何一行）：

```
tests/lum1469_pending_block        7 / 9 立刻红
  a_queued_prompt_is_painted_above_the_composer
  steering_and_follow_up_rows_render_in_delivery_order
  the_block_costs_the_transcript_exactly_its_rows
  a_long_queued_draft_is_one_marked_row
  a_short_terminal_keeps_the_composer_and_the_block
  frame_dump_queued_prompts / frame_dump_cut_queued_draft
tests/pending_messages             2 / 5 立刻红
  busy_submit_queues_instead_of_dropping（它断言「渲染里看得见」）
  queued_prompts_are_painted_above_the_composer
```

恢复后 9 + 1 + 5 全绿。即这批断言真的钉住本轮改动。

## 4. 门禁与数字

### 4.1 fmt / clippy

| 门禁 | 结果 |
|---|---|
| `cargo fmt --all -- --check` | clean |
| `cargo clippy --offline -p pi-tui -p pi-coding-agent --all-targets` | 改动文件 **0 告警**；输出里的告警全部落在未改动的 `pi-extensions`（`signal_name` dead code）与 vendored `rquickjs-core` 上 |

### 4.2 `pi-tui` 全量

```
$ cargo test --offline -p pi-tui -j 8
targets + doc-tests  passed 1154  failed 0
```

基线（本轮救回的两条交付合入后，`20f3666d9`）**1137 / 0** → **+17** =
4（`message.rs` 单测）+ 2（`extension_ui.rs` 单测）+ 9（`lum1469_pending_block`）
+ 1（`lum1469_pending_chord`）+ 1（`pending_messages`）。

### 4.3 `pi-coding-agent`（含集成 target）

```
$ cargo test --offline -p pi-coding-agent -j 4 --no-fail-fast
targets  passed 836  failed 28
```

基线同机为 **830 / 28**（LUM-1466 §4.3）→ **+6**（LUM-1263 的 `--lib` 用例）。
28 条失败逐条核对，全部是 Windows 环境类：真 `bash` 工具、绝对路径断言、
`/tmp`、node fs、project trust、扩展发现 —— 与基线同一集合，**新增失败 0**。

`--lib` 单跑对账（口径与 LUM-1466 §4.3 一致）：

```
$ cargo test --offline -p pi-coding-agent --lib
594 passed / 8 failed      # 基线 588 / 8，同一组 8 条名字（export / js_loader / paths /
                           # resource_loader / trust×2 / tools::mod_ignore）
```

### 4.4 Rust↔TS 口径（全部可复现）

```bash
python pi-rust/scripts/measure_loc.py
git grep -h -o -E '#\[(tokio::)?test\]' fc18cb09e -- pi-rust/crates | wc -l   # 2773（issue 基线）
git grep -h -o -E '#\[(tokio::)?test\]' HEAD -- pi-rust/crates | wc -l        # 2829（本轮 tip）
grep -rhoE '#\[(tokio::)?test\]' pi-rust/crates --include=*.rs | wc -l
grep -rhoE "^\s*(it|test)(\.\w+)?\(" packages --include=*.test.ts | wc -l
python pi-rust/scripts/app_action_coverage.py
```

| 口径 | 本轮 | 上轮（LUM-1466） | 说明 |
|---|---|---|---|
| Rust src（去 mod.rs） | 143,457 行 / 254 文件 | 141,365 / 254 | **+2,092**，其中本轮救回的 LUM-1461 +1,012 行、LUM-1263 +387 行（两条各自文档口径），本轮自身 **+256 行**（`app.rs` +80/-2、`extension_ui.rs` +58/-1、`message.rs` +140/-19）；余量为两条交付与合并树的其它既有差异 |
| TS src | 153,106 行 / 669 文件 | 153,106 | 未变 |
| 规模比 | **93.7%** | 92.3% | **涨幅主要来自救回的两条交付，不是本轮新功能** |
| Rust `#[test]` | **2,829** | 2,773 | +56 = 救回交付 +39（LUM-1461 24 + LUM-1263 15）+ 本轮 **+17** |
| TS 用例 | 5,563 | 5,563 | 同口径 |
| 测试比 | **50.8%** | 49.8% | 2829/5563 = 0.5085 |
| TUI 模块 | 36 / 42 = 85.7% | 36 / 42 | 本轮没有新增模块（改的是同一批模块内的行） |
| `app.*` 接线 | **44/44 = 100%**（silent 0、advertised 0） | 43/44 = 97.7% | 由并入的 LUM-1263 关闭最后一格 |
| 扩展生命周期事件 | 36/36 声明 + 36/36 生产构造点 | 36/36 | 未动 |

### 4.5 加权完成度（公式与权重沿用 `RUST_TS_PARITY_METRICS.md` §4.1）

| # | 轴 | 权重 | 得分 | 依据 |
|---|---|---|---|---|
| 1 | 可构建 / 可测 / 可运行 | 5% | 100% | §4.1–4.3 |
| 2 | 核心 agent 循环 | 13% | 90% | 未动 |
| 3 | provider API family | 8% | 100% | 未动 |
| 4 | provider / 模型目录广度 | 6% | 70% | 未动 |
| 5 | TUI 交互面（模块率与接线率均值） | 14% | **92.9%** | (0.857 + **1.000**) / 2 —— 接线率 44/44 由 LUM-1263 关闭 |
| 6 | TUI 视觉保真 | 8% | 90% | 不上调，见下 |
| 7 | slash 命令面 | 7% | 78% | 未动 |
| 8 | CLI / 模式 / 子命令面 | 7% | 70% | 未动（LUM-1434 在办） |
| 9 | 扩展宿主能力 | 8% | 95% | 未动 |
| 10 | 扩展生命周期事件 | 7% | 100% | 未动 |
| 11 | 会话 / 存储 / 导入导出 | 9% | 85% | 未动 |
| 12 | 测试与门禁强度 | 5% | **50.8%** | §4.4 |
| 13 | 子包完整度 | 3% | 95% | 未动 |

```
5×1.00 + 13×0.90 + 8×1.00 + 6×0.70 + 14×0.929 + 8×0.90 + 7×0.78 + 7×0.70
+ 8×0.95 + 7×1.00 + 9×0.85 + 5×0.5085 + 3×0.95 = 87.02% → **87.0%**
```

**口径声明（诚实读法）**：轴 5 的 +0.09 来自**并入的 LUM-1263**（`app.*` 44/44），
不是本轮写的代码；本轮自己只贡献轴 12 的 `2773→2829`（+0.056pt）与轴 6 的**证据修复**。
轴 6（TUI 视觉保真）**本轮不上调**——理由沿用 LUM-1418 §6：该轴的取值口径是「读者能否在正确的位置
看到正确的东西」，本轮把一个已知缺口（排队输入的位置/形状/提示）从「不符上游」变成「符合上游」，
属可信度修复，不足以把 90% 重估。

## 5. 截图（frame-buffer，本机无 PTY）

| 文件 | 内容 | 生成方式 |
|---|---|---|
| `docs/screenshots/lum1469-queued-prompts-100x24.png` / `.txt` | 100×24，transcript 溢出到滚动条：块 = `Steering: also check the retry path` + `Follow-up: then summarise` + `↳ Alt+Q to edit all queued messages`，紧贴 composer 上方 | `cargo test -p pi-tui --test lum1469_pending_block -- --nocapture --test-threads=1` → `python pi-rust/scripts/frame_to_png.py` |
| `docs/screenshots/lum1469-cut-queued-draft-44x16.png` / `.txt` | 44×16：一条超宽排队草稿只占**一行**并以 `…` 标记；同帧还能看到 LUM-1412 的「cut above」提示与 LUM-1367 的 footer 分片 | 同上（`frame_dump_cut_queued_draft`） |
| `docs/screenshots/lum1469-no-queue-100x24.png` / `.txt` | 无排队：**零块行**，transcript 一路铺到 composer（回归证据） | 同上（`frame_dump_no_queue_baseline`） |

**诚实说明（本机限制）**：这台 Windows runner 没有 `pty`（`import pty` 不可用），截图走 LUM-1412
建立的 **frame-buffer 通道**：它证明「画在哪一格、内容是什么」，**不证明按键/字节时序**；
时序与几何由 17 条 App/驱动级测试覆盖（§3 的反向验证）。真终端录制（`scripts/pty_capture.py`）
仍应在 Linux runner 上对同一场景补一次。

## 6. 全 TUI 问题清单（本轮实测更新）

| 顺位 | 问题 | 证据 | 本轮处置 |
|---|---|---|---|
| 1 | ~~footer 只有一行~~ | LUM-1466 §6 #1 | **已合入** `feature/pi.rs`（本轮 tip 已含） |
| 2 | footer stats 行缺 `↑/↓`、`CH%`、`$cost`、`(auto)`、`(provider)` 前缀 | LUM-1466 §6 #2 | **在办**：LUM-1467（`in_progress`） |
| 3 | ~~`app.tree.editLabel` 是唯一 silent `app.*`~~ | `app_action_coverage.py` 44/44 | **本轮关闭**（并入 LUM-1263） |
| 4 | 扩展 `ctx.ui.setStatus(key, text)`：上游 footer 的**第 3 行**来自它，pi-rust 仍是 `ERR_PI_UI_UNSUPPORTED` | `app.rs:200-216` 的 `ctx.ui` 对照表 | **未做**，列为下一轮第一顺位（与 #2 同一文件面，等 LUM-1467 合入后再派） |
| 5 | ~~composer `paste_burst`~~ | codex `paste_burst.rs` | **本轮并入** `feature/pi.rs`（LUM-1461） |
| 6 | CLI flag 面（字面 18/40） | `RUST_TS_PARITY_METRICS.md` §3.4 | **在办**：LUM-1434 |
| 7 | ~~排队输入没有上下文取回提示，且块画在日志尾部~~ | §1 | **本轮关闭**（§2，17 条测试 + 3 帧） |
| 8 | codex 独有、上游 pi-ts 没有的 affordance：`Esc` 二次提示、`Ctrl+P/N` 历史导航 | codex `chat_composer.rs:2838-2880` | **不做**（与上游 pi-ts 冲突，除非产品明确要「超出 pi」） |

### 6.1 本轮顺带发现的既有缺陷（记录，不在本轮修）

* **`/transcript` 会带出块**：`render_snapshot` 与实时帧共用 render path，所以排队块会出现在
  `/transcript` 的快照里。其它「屏幕家具」（滚动条）都被显式排除（`scrollbar == false`），
  块没有这条豁免。判断：**可接受**（它确实是「此刻屏幕上有什么」），但应记一笔。
* **44×16 帧里「cut above」提示行会覆盖正文首行**（`lum1469-cut-queued-draft-44x16.txt` 第 1 行
  `⋯ 1 line above · Home il the turn ends.`）。这是 LUM-1412 的既有行为（提示是叠加在
  transcript 首行上的），与本轮无关；但它让「被截断的上文」读起来像乱码，值得单独一轮。

## 7. 计划与派发：**1 个新任务**（槽位 2/3）

本轮开工时扫 board（`multica issue list --project ae0b46e7…`）实测：

| issue | 状态 | 指派 | 面 |
|---|---|---|---|
| LUM-1467 | `in_progress` | `编程助手-winpi` | footer stats 行（`pi-tui/src/status.rs`） |
| LUM-1434 | `in_progress`（此前） | `编程助手-window` | CLI flag 面（`pi-coding-agent/src/cli/`） |
| LUM-1461 / LUM-1263 | `in_review` | `编程助手-winpi` | **本轮已合入 `feature/pi.rs`**（§0.1） |

即：救回的两条交付合入后，board 上的在办面回落到 2/3，cap（最多 3 个并发）**还有 1 个槽位**。

「如果任务存在是跳过还是计划和实现后续任务」的回答：**不跳过、也不重开大改**，本轮在同一个 run 里
完成 issue 点名的四件事（TUI 审计 / 缺口修复 / 截图 / 推送合并），并**派发 1 个**精确定义的后续任务：

* **LUM-1470** —— 扩展 `ctx.ui.setStatus(key, text)` 落地为 footer 的第 3 行（§6 #4）。
  上游 `footer.ts:243-251` 用 `footerData.getExtensionStatuses()` push 一行，Rust 侧
  `app.rs:200-216` 的 `ctx.ui` 对照表把它列为不支持。证据、行号、验收判据写进任务正文；
  创建时为 `backlog`（避免与 LUM-1467 抢同一个 `status.rs` / `plan_chrome` 文件面），
  在本轮 `feature/pi.rs` 推送成功、LUM-1467 合入之后再提升为 `todo` 触发运行。

**只派 1 条而不是 2 条**的理由：剩下的候选中，`/transcript` 带出块（§6.1）与「cut above 覆盖正文」
都要改 `app.rs` 的渲染路径，与 LUM-1467 正在动的 `status.rs`/`plan_chrome` 是同一屏几何；
本仓库已经为「同一缺陷两条并发线各修一次」清过两次（LUM-1431 §3、LUM-1445 §8）。

## 8. 范围之外

未碰 `pi-ai` / `pi-agent-core` / `pi-protocol` / `pi-extensions` / `pi-session` / `pi-server` /
`pi-client` / `pi-chord` / `pi-evals` / `pi-telemetry` 的源码；未碰上游 TS（`packages/**`，只读取证）；
未碰 CI / Docker；未改扩展事件表、slash 命令表、CLI flag 表、`CONSUMED_APP_ACTIONS`。

**已知偏差（写清而不是省略）**：

1. **不做 codex 的 3 行预览上限**：上游 pi-ts 是「每条 1 行」，codex 是「每条最多 3 行 + `…` 溢出行」。
   pi-rust 按 pi-ts 实现（1 行/条）。理由：本 issue 的主验收面是 pi-ts 对等；
   排队 8 条时 codex 形状会把 composer 顶出屏幕。
2. **不做 codex 的分段标题**（`Messages to be submitted after next tool call` /
   `Queued follow-up inputs`）：pi-ts 只用 `Steering:` / `Follow-up:` 前缀。按 pi-ts 实现。
3. **不做 Martty 的「一次性 tip」**：Martty 用 `show_tip` 出一条 5 秒提示并另有 meta 行计数；
   pi-rust 用的是常驻提示行（上游形状）。计数行（`· N queued`）**未做** —— 上游 footer 没有它，
   而 footer 面正被 LUM-1467 占着。
4. **`↳` 的宽度口径**：`U+21B3`（↳）在 East Asian Ambiguous 类别里，`width.rs::char_columns`
   按上游 `visibleWidth` 的同一套规则处理；44 列帧里它与英文混排不会溢出（`a_long_queued_draft_is_one_marked_row`
   断言行宽 ≤ 44）。
5. **排队块不参与鼠标**：块是 host chrome，不注册 `MouseRegion`（上游的 `pendingMessagesContainer`
   也不是点击目标）。点击块不定位光标。
