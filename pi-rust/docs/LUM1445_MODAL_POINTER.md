# LUM-1445 — 模态列表的指针面：picker / `ctx.ui.select` 认领滚轮与点选，`/settings` 补点选

> 基线：`feature/pi.rs` = `40417a2d2`（= LUM-1436 合并 tip，本轮从它起）
> 环境：Windows 10 x86_64 / cargo 1.97.1 / `--offline`
> 本轮只改 `pi-tui`（+ 驱动侧 3 处事件放行），不碰 `pi-protocol` / `pi-agent-core` / `pi-extensions`。

## 1. 为什么是这块

LUM-1436 §7 的起手清单有 4 条，前 3 条（扩展事件、`#` provider 面、CLI flag 面）都不在 `pi-tui` 内：

| 顺位 | 项 | 本轮处置 |
|---|---|---|
| 1 | 扩展生命周期事件 20/36 → 36/36 | 已单子化 LUM-1432（`pi-protocol`+`pi-agent-core`+`pi-extensions`），不与之并行改文件 |
| 2 | `#` 技能/工具引用面 | 需 provider 注入点，跨 `pi-coding-agent` 与 LUM-1432 无重叠但与 §3 同文件面，本轮不动 |
| 3 | CLI flag 面 | 同上 |
| 4 | **modal 打开时滚轮归属** | **本轮做**：LUM-1436 只写了"滚轮不落到视口"，没做"给 overlay" |

第 4 条做下去时发现它是**半边**：模态列表不仅不认领滚轮，连**点击行都不认领**——
`step_modal_mouse_gesture` 只把 press/release 当作"点到了模态"（用于清掉聊天日志选区），
从不把行映射回条目。上游不是这样：`dispatchMouseToOverlay` 把事件翻译成 overlay 局部坐标后
交给组件，`SelectList::handleMouse`（`packages/tui/src/components/select-list.ts:110-148`）
滚轮一档一格、press 高亮、click 激活，`SettingsList::handleMouse`
（`packages/tui/src/components/settings-list.ts:179-210`）同构且要跳过搜索行。
`pi-tui` 没有组件树（模态是 `App` 直接画进 cell buffer 的），所以这半边一直缺着。

## 2. 语义（逐条对齐上游）

| 上游 | 本轮实现 |
|---|---|
| `SelectList::handleMouse` wheel：`delta = wheelDelta < 0 ? -1 : 1`，**钳制不回绕** | `App::step_modal_list_wheel`：一档一格，`clamp`；末行再往下仍是列表的（返回 `Idle`），视口不动 |
| wheel 只判 `event.y`，不判列 | `App::modal_list_hit(y)` 只读行 |
| wheel 命中 overlay 矩形但没命中条目行（标题/分割线/`(n/m)`/脚注）→ `result` 为空→ `shouldDeferViewportInputToOverlay` → 返回未消费 | 非条目行同样归模态（`Idle`），**聊天天志不滚** |
| press：高亮按下行、不激活 | `App::modal_list_press` |
| click（同一格 press+release）：激活该行（`onSelect`） | `App::modal_list_click`；picker 走 `take_selector_commit`，dialog 走自身应答通道，settings 走 `activate()` |
| `SettingsList` 的 `rowOffset = searchEnabled ? 2 : 0` | `App` 记录 geometry 时按 `is_searchable()` 补 2 行头 |

三处"激活"都**复用键盘已有的路径**，不新造语义：

- `ctx.ui.select`：`App::step_dialog(Key(Enter))` —— 对话框归 `App` 所有（应答走 oneshot）。
- `/settings`：`App::step_settings(Key(Enter))` → `pending_setting_change/activation`，驱动侧
  `drain_settings_changes` 本来就在轮询。
- picker（`/model`、`/session`、`/tree`…）：picker 的**含义**在驱动侧（`apply_selector_choice`），
  键盘 `Enter` 也由驱动自己处理（`interactive.rs:948-962`）。所以 `App` 只记录意图
  （`take_selector_commit()`），驱动在 `app.step(event)` 之后立刻消费并 `close_selector()` ——
  与 `Enter` 完全同一条落点。

## 3. 改动清单

| 位置 | 内容 |
|---|---|
| `pi-rust/crates/pi-tui/src/selector.rs:771` | `visible_range()` 由私有转 `pub`（指针行映射需要窗口起点） |
| `pi-rust/crates/pi-tui/src/settings.rs:358` | `set_cursor()` 转 `pub`（press 把选中行搬到点击行） |
| `pi-rust/crates/pi-tui/src/dialog.rs:170,177` | 新增 `select_window()` / `set_select_cursor()`（`Confirm`/`Input`/`Notify` 返回 `None`，即不认领指针） |
| `pi-rust/crates/pi-tui/src/app.rs:1201,1213` | 新增 4 个 geometry 原子量（`modal_list_kind/first_row/first_item/rows`）+ `pending_selector_commit` |
| `pi-rust/crates/pi-tui/src/app.rs:4744-4932` | `record_modal_list` / `modal_list_hit` / `step_modal_list_wheel` / `modal_list_press` / `modal_list_click` / `take_selector_commit` |
| `pi-rust/crates/pi-tui/src/app.rs:4370` | `step_modal_mouse_gesture`：press 高亮行、click 激活行（原来两者都只吞不处理） |
| `pi-rust/crates/pi-tui/src/app.rs:5706,5746,5780` | 三处绘制块按各自布局记录条目窗口（selector 标题+分割线 2 行；settings 搜索行 2 行；dialog 仅 `Select` 有列表），非 `Select` 对话框把记录清空 |
| `pi-rust/crates/pi-coding-agent/src/interactive.rs:905,920` | selector / 改名对话框分支**只截键**：滚轮与指针放行到 `app.step`（此前非键事件在这里被 `return Ok(None)` 吃掉，指针永远到不了 App） |
| `pi-rust/crates/pi-coding-agent/src/interactive.rs:1175` | `app.step` 之后消费 `take_selector_commit()`，与 `Enter` 共用 `apply_selector_choice` |
| `pi-rust/crates/pi-tui/tests/modal_pointer.rs`（新，11 条） | 行为断言：一档一格 / 钳制 / 非条目行不认领 / 日志不滚 / press 只高亮 / 同格 click 才提交 / 关掉的 picker 不再可点 / dialog wheel+click / `Confirm` 无列表 / settings 点选激活 / 搜索行与脚注不可点 |
| `pi-rust/crates/pi-tui/tests/lum1445_modal_pointer_frames.rs`（新，2 帧） | 76×18 帧 dump：点选前（`❯ faux-a`）与点选后（`❯ faux-c` + 已提交值、列表下方的 transcript/输入框/状态栏逐字未变） |
| `pi-rust/crates/pi-coding-agent/src/interactive.rs`（测试） | `the_picker_takes_the_pointer_while_it_owns_the_keyboard`：真驱动路径上滚轮移高亮、click 换模型 |

## 4. 验证（本机 Windows / cargo 1.97.1 / `--offline`）

| 门禁 | 结果 |
|---|---|
| `cargo test -p pi-tui` | **1020 passed / 0 failed**（基线 1007，+13 = 11 行为 + 2 帧） |
| `cargo test -p pi-coding-agent --lib` | **571 passed / 8 failed**（基线 `stash` 实测 570/8，**同一批 8 个** Windows 环境类失败：`absolute_paths_stay_absolute`、`missing_files_report_the_upstream_message`、`load_extensions_loads_an_esm_extension_from_disk`、`prompt_contains_builtin_tools_context_and_skills`、`absolute_paths_are_rejected`、`has_no_trust_requiring_resources_in_an_empty_project`、`detects_trust_requiring_project_resources`、`cli_export_propagates_the_upstream_error_text`；+1 = 本轮新驱动用例） |
| `cargo fmt --all -- --check` | 干净（exit 0） |
| `cargo clippy -p pi-tui --all-targets -- -D warnings` | 干净 |
| clippy `-p pi-coding-agent --all-targets -D warnings` | **失败于既存告警**：`pi-extensions/src/host.rs:3144 fn signal_name` 未使用（`dead_code`）。该文件本轮未改、基线 `cargo check` 同样报；本轮新增/修改的 `interactive.rs` 在 `cargo clippy -p pi-coding-agent --all-targets`（不加 `-D`）下零告警 |
| 反向验证 1 | 把 `record_modal_list` 存成 `MODAL_LIST_NONE`（即回到旧行为）：`modal_pointer` 11 条里 **5 条立刻红** |
| 反向验证 2 | 把 selector 分支的 `matches!(event, InputEvent::Key(_))` 去掉（回到旧的"非键直接 return"）：驱动新用例**立刻红** |

## 5. 帧截图

`docs/screenshots/lum1445-modal-pointer-before-76x18.png` / `...-after-76x18.png`
（附 `.txt` 原始 cell grid）。**说明**：本机无 PTY，走 frame-buffer 通道——它证明**几何、高亮与"谁动了"**，
不证明点击时序；时序证据是 §4 那些驱动真实 `step` 的行为断言。

## 6. Rust ↔ TS 差距复测（本轮亲自跑）

| 量 | 本轮实测 | LUM-1436 | 命令 |
|---|---|---|---|
| 纯代码规模（src↔src） | **89.1%**（136,469 / 153,106） | 88.8%（136,016） | `python pi-rust/scripts/measure_loc.py` |
| 测试规模 | **49.9%**（2,651 / 5,309） | 49.7%（2,637） | `grep -rhoE '#\[(tokio::)?test\]' pi-rust/crates --include=*.rs \| wc -l` ↔ `find packages -name '*.test.ts' \| xargs grep -hoE '\bit\(|\btest\(' \| wc -l` |
| TUI 模块面 | 35 / 42 = 83.3% | 同 | `pi-tui/src`（36 − `lib.rs`）↔ `packages/tui/src` + `components` |
| `app.*` 接线 | 43 / 44 = 97.7% | 同 | `python pi-rust/scripts/app_action_coverage.py --check-consumed` → `43 entries; 43 wired; in sync` |
| 扩展生命周期事件 | **20/36 生产构造点 = 55.6%**（声明 21/36） | 同 | `python pi-rust/scripts/extension_event_coverage.py`（15 个缺失逐条列出，LUM-1432 在办） |
| **TUI 指针面** | **6 / 6 = 100%** | 3/3（仅 composer） | composer 3（点击定位 / 下拉框点选 / 下拉框滚轮）+ **模态列表 3（picker 滚轮+点选 / dialog select 滚轮+点选 / settings 点选；settings 滚轮在 LUM-1367 已有）** |

**加权完成度**（权重表见 `RUST_TS_PARITY_METRICS.md` §4.1，公式公开）：

```text
5×1.00 + 13×0.90 + 8×1.00 + 6×0.70 + 14×0.905 + 8×0.90 + 7×0.783 + 7×0.70
  + 8×0.95 + 7×0.556 + 9×0.85 + 5×0.499 + 3×0.95 = 83.6%
```

只有测试轴从 0.497 走到 0.499（+0.01pt），总分仍 **83.6%**（一位小数不变）。
「TUI 指针面」不单独进公式（公式里的 TUI 轴是模块率与接线率的均值），它是轴 6 视觉/交互保真的前提。

## 7. chatinput 与 codex / Martty 的第一手对照（本轮亲自核对）

本轮 checkout 了 `https://github.com/louloulin/Martty`（tip `93e9231 release: v0.2.17`），
不再转述上一轮结论：

| 面 | pi-rust | Martty（`src/input/editor.rs` 394 行、`src/app.rs:2284`） |
|---|---|---|
| 多行 / 软换行移动 | 有 | 有（`move_vertical` 带 display-column 记忆） |
| 行域编辑（kill-to-end/start、word motion） | 有 + kill-ring | 有，**无 kill-ring** |
| 撤销栈 | 有（`undo_stack.rs`） | **无** |
| 历史反查 `Ctrl+R` / 跨会话 history | 有（LUM-1415） | **无**（只有 `HistoryPrev/Next`，`keymap.rs:29-30`） |
| 大段粘贴折叠 `[paste #N]` | 有（LUM-1318） | **无**（只识别 bracketed paste，不折叠） |
| composer 指针（点击定位光标） | 有（LUM-1426） | **无**：`handle_mouse` 只覆盖聊天面板选择/滚轮/工具块/slot，输入框不是鼠标目标 |
| composer 下拉框（补全）点选 + 滚轮 | 有（LUM-1431/1436） | 无补全下拉框 |
| 模态列表指针（本轮的 picker/dialog/settings） | 有 | 部分：`elicitation`/`view_overlay` 有滚轮，行点选无 |

结论（与 LUM-1436 一致，但现在是本轮自己核对的）：**issue 正文里"chatinput 与 codex/Martty 差距很大"
这句话在 composer 面上已经不成立**——编辑语义、反查、指针三面 pi-rust 是**更强**的一方；
Martty 领先的地方不在 composer，而在宠物（`pet.rs`）、主题、插件 UI 宿主（`slots.rs`）。

## 8. 剩余缺口（供下一轮起手）

| 顺位 | 项 | 范围 | 说明 |
|---|---|---|---|
| 1 | 扩展生命周期事件 +15 | `pi-protocol`/`pi-agent-core`/`pi-extensions` | LUM-1432 在办（20/36 → 36/36） |
| 2 | `#` 技能/工具引用 provider | `pi-coding-agent` autocomplete wrapper | 上游由 provider 注入（`editor.ts:251` 只是触发符） |
| 3 | CLI flag 面 | `pi-coding-agent/src/cli` | 与上游启动参数对齐 |
| 4 | modal 内拖拽/悬停 | `pi-tui` | 上游 `SelectList` 明确"hover 不改选中"（窗口以选中行居中），Rust 同构；若要 hover 预览需先改窗口策略 |
| 5 | `settings` 滚轮按矩形认领 | `pi-tui` | 现状是全局认领（`step_settings_wheel`，LUM-1367 起），与上游 `dispatchMouseToOverlay` 的矩形前提不同；改动会碰既有 3 条测试，留作独立一轮 |

两条已知遗留（记录备查，本轮未动）：`message.rs:63` 的 `tool_fold_hint` 仍硬编码 `Ctrl+O`；
`app.editor.external` 仍缺（Stage 59）。

## 9. 「最多 3 个任务」的处置 = 1 派发 + 1 自做 + 1 复核

- **自做**：模态指针面（全在 `pi-tui` + 驱动 3 处放行）。
- **派发**：LUM-1432（扩展事件 20/36 → 36/36）此前已指派 `编程助手-devbox1`，本轮核实其状态与文件面
  （`pi-protocol`/`pi-agent-core`/`pi-extensions`）与本轮**零重叠**，不重复派发。
- **不派第三条**：§8 的 2、3 条与 LUM-1432 共享 `pi-coding-agent`，并行会重演"同一缺陷两条线各修一次"
  （LUM-1431 §3、LUM-1436 §8 各清过一次）。

## 10. 范围之外

未碰 `pi-protocol` / `pi-ai` / `pi-agent-core` / `pi-session` / `pi-server` / `pi-client` / `pi-extensions`；
未碰上游 TS（`packages/**`，只读取证）；Martty 只读（`workdir/Martty`）。
