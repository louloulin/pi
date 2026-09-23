# LUM-1461 — composer `paste_burst`（终端不发 bracketed paste 的突发识别，codex 对齐）+ marker 按词原子 + 历史 marker 降级

> 本轮的三个切片都来自 `docs/LUM1460_PASTE_RESCUE.md` 的收尾清单（§4.1 偏差、§5 偏差 1 / 4、§10 顺位 1–3）。
> 所有数字在本机（Windows / rustc 1.97.1 / 离线）实测，命令写在对应小节里。

## 0. 结论速览

| 量 | 本轮实测 | 基线 `c37d80742`（LUM-1460 合并后） | 依据 |
|---|---|---|---|
| `pi-tui` 全量测试 | **1099 passed / 0 failed** | 1075 / 0 | `cargo test --offline -p pi-tui`，75 个 target；+24 = 行为 11 + 帧 3 + `input.rs` 6 + `word_navigation.rs` 4 |
| Rust 测试标记数 | **2764**（基线 2740，**+24**） | 2740 | `grep -rhoE '#\[(tokio::)?test\]' pi-rust/crates --include=*.rs \| wc -l` |
| 纯代码规模（src↔src） | **92.5%**（141,557 / 153,106） | 91.8%（140,615） | `python pi-rust/scripts/measure_loc.py`；+942 行 src |
| 加权完成度（§4.1 权重表，公式见 `RUST_TS_PARITY_METRICS.md`） | **86.9%**（86.85） | 86.8%（86.83） | 只有测试轴动：2,740/5,309 → 2,764/5,309 |
| composer 粘贴能力面（LUM-1460 §4 登记轴） | **7 / 7**（不变） | 7 / 7 | 本轮把其中的 `paste_burst` 兜底单独登记为 **1 / 1** |
| `paste_burst` 兜底（本期新登记，**codex 独有、pi-ts 无对应格**） | **1 / 1** | 0 / 1 | `tests/lum1461_paste_burst.rs`（11 条）+ `tests/lum1461_burst_frames.rs`（3 条） |

一句话结论：**终端不发 bracketed paste 时，composer 不再把整段粘贴当逐字符输入**——
`input.rs` 的 `PasteBurst` 按 codex 的时间窗（8 ms 字符间隔 / 3 字符门槛 / 120 ms 回车抑制窗）
把一串按键判为一次粘贴，`App::step_key_at` 把它交给 `Editor::insert_paste`，
于是 **LUM-1460 已落地的折叠、注册表、撤销单元、原子编辑全部复用**，没有第二套粘贴实现。
`Alt+B`/`Alt+F` 现在一步跨过整个 `[paste #N …]`；跨会话历史召回回来的无内容 marker
**降级为普通文本并给一次提示**。

## 1. 与 codex `paste_burst` 的逐条对照

参考实现：`openai/codex` `codex-rs/tui/src/bottom_pane/paste_burst.rs`
（本机取 `main` @ `888e02db34bc6fe226e2131448413c051491f5b8`，590 行；逐行对照）。

| # | codex | 本轮 pi-rust | 位置 |
|---|---|---|---|
| 1 | `PASTE_BURST_CHAR_INTERVAL = 8 ms`（相邻"普通字符"的最大间隔） | **同值** `pub const PASTE_BURST_CHAR_INTERVAL` | `input.rs:546` |
| 2 | `PASTE_BURST_MIN_CHARS = 3` | **同值** `PASTE_BURST_MIN_CHARS` | `input.rs:551` |
| 3 | `PASTE_ENTER_SUPPRESS_WINDOW = 120 ms`（窗口内 `Enter` 是粘贴里的换行，不是提交） | **同值** `PASTE_ENTER_SUPPRESS_WINDOW`，`newline_should_insert` | `input.rs:560`、`:696` |
| 4 | `PASTE_BURST_ACTIVE_IDLE_TIMEOUT = 8 ms`（非 Windows）/ `60 ms`（Windows） | **同值的平台分叉** | `input.rs:568` / `:572` |
| 5 | `on_plain_char` / `on_plain_char_no_hold` 返回 `CharDecision` | `on_plain_char -> BurstDecision`（`Typed` / `BeginBurst{retro_chars}` / `Buffered`） | `input.rs:577`、`:631` |
| 6 | `note_plain_char`：`delta <= interval` 才累加 `consecutive_plain_char_burst` | 同一计数规则，字段 `consecutive` | `input.rs:632-641` |
| 7 | 第 3 个窗口内字符 → `BeginBuffer`，retro 抓取已插入前缀 | 第 3 个窗口内字符 → `BeginBurst { retro_chars: consecutive-1 }`，`Editor::cut_chars_before_cursor` 抓取 | `input.rs:646-655`、`editor.rs:1442` |
| 8 | `absorb`/`begin_with_retro_grabbed` 把前缀折进缓冲 | `absorb_retro(prefix)`（前缀 prepend，保持字节顺序） | `input.rs:662` |
| 9 | `append_control_char_if_active`：`\n` / `\t` 只在 burst 上下文里并入 | `append_control_if_active` **同语义**（只有 active 才收） | `input.rs:705` |
| 10 | `flush_if_due`：活跃缓冲按 `ACTIVE_IDLE_TIMEOUT` 超时 flush | `flush_if_due -> Option<String>`，flush 后清计数但**保留回车抑制窗** | `input.rs:720` |
| 11 | `clear_window_after_non_char` | `clear_window` / `flush_now_and_clear` | `input.rs:733`、`:745` |
| 12 | `FlushResult::Paste(String)` → `ChatComposer::handle_paste` | flush 后的字符串走 `Editor::insert_paste`（**与 bracketed paste 同一条通道**） | `app.rs:3371` |
| 13 | `Enter` 抑制窗内 `newline_should_insert_instead_of_submit` | `App::step_composer_burst` 对 `KeyCode::Enter` / `Tab` 调 `append_control_if_active` | `app.rs:3266-3325` |

### 1.1 与 codex 的两条**有意偏差**（不是遗漏）

1. **不做"首个字符暂持"（flicker suppression）**。codex 的 `pending_first_char` 会把一个
   普通字符扣住最多 `PASTE_BURST_CHAR_INTERVAL`，等 UI tick 再决定"按打字插入"还是"并入粘贴"，
   代价是**每次打字都要靠 tick 才能上屏**。codex 的 TUI 有高频 tick；本端口的
   `App` 与驱动之间**没有保证的 tick**（`step_key` 只在按键时被调用），扣住一个字符就等于
   在"用户打完一个字后停手"时把它挂住。因此本端口**每个字符都立即按普通打字插入**，
   确认突发时再用 `cut_chars_before_cursor` **回溯**折进粘贴缓冲。
   结果：粘贴的最终状态与 codex 一致（一整段走 `handle_paste`），正常打字零延迟，
   误判时退化为"同样这些字符、作为一个单元插入"，**不丢字符**。
2. **不做 retro 的 `looks_pastey` 启发式**。codex 的 `decide_begin_buffer` 在回溯前还要
   判断抓到的前缀"像不像粘贴"（含空白或 ≥16 字符），否则放弃缓冲。本端口在
   `consecutive >= PASTE_BURST_MIN_CHARS` 时直接开始缓冲：前缀最多 2 个字符，
   不存在 codex 要避免的"先画出来再收回去"的抖动，反而少一条会**放弃粘贴识别**的分支。

## 2. 本轮落地（文件:行号）

| 文件 | 改动 | 说明 |
|---|---|---|
| `pi-tui/src/input.rs` | `+~250` | `PasteBurst` 状态机（常量、`BurstDecision`、`on_plain_char` / `absorb_retro` / `abort` / `newline_should_insert` / `append_control_if_active` / `flush_if_due` / `flush_now_and_clear` / `clear_window` / `flush_deadline`），时钟全部可注入；6 条单测 |
| `pi-tui/src/app.rs` | `+~200` | `AppConfig::paste_burst`（默认 **false**）、`App::step_key_at` 拆出 `step_key_inner` 并做 burst 记账、`step_composer_burst`（分类 + retro 抓取 + 回车/制表符并入 + 非 burst 键 flush 后放行）、`tick_paste_burst` / `paste_burst_deadline`（驱动用）、`step_paste` 先 flush 再清窗、历史 marker 提示落 `flash_status` |
| `pi-tui/src/editor.rs` | `+~120` | `cut_chars_before_cursor`（回溯抓取，自带撤销快照、绝不丢字节）、`unresolved_paste_marker_ids`（无注册表 marker）、`take_stale_paste_notice`（每会话一次的提示）、`atomic_paste_spans`（registry 有效 id 的 marker span）、`set_entry_internal` 在召回时登记提示、`move_word_left` / `move_word_right` 改走 marker 感知版本 |
| `pi-tui/src/word_navigation.rs` | `+~260` | `segments_in`（把 `atoms` 合并成原子段，等价于上游 `segmentWithMarkers(..., "word")`）、`find_word_backward_with_atoms` / `find_word_forward_with_atoms`；原有两个函数变成 `atoms = &[]` 的薄封装，行为不变 |
| `pi-coding-agent/src/interactive.rs` | `+~30` | `interactive_app_config` 打开 `paste_burst: true`（App 默认关闭，真终端由驱动开启）；`run_loop` 每圈调 `app.tick_paste_burst`，并在 burst 活跃时把 `crossterm::event::poll` 的超时压到 flush 截止时刻 |
| `pi-tui/tests/lum1461_paste_burst.rs` | 新增 | 11 条行为用例（下 §4） |
| `pi-tui/tests/lum1461_burst_frames.rs` | 新增 | 3 帧 frame-buffer 截图源（§6） |

### 2.1 三个设计决定（不是照抄）

1. **默认关闭，驱动打开。** `AppConfig::paste_burst` 默认 `false`：`App` 级测试会用
   `Instant::now()` 连打多个字符（真实终端永远不会让合成事件落在同一毫秒），
   默认开启会让既有 App 测试的"输入"被当成粘贴。交互驱动
   （`interactive.rs:284`）打开它——真终端才是需要这层兜底的地方。
   `AppConfig::default` 与 `startup_header` 的"App 默认关、驱动开"是同一套路。
2. **burst 分类放在 `app.*` 和弦之前。** `step_key_at` 里 burst 判定排在模态层之后、
   和弦表之前（`app.rs:3000`）。原因有两个：① `Ctrl+R` 这类和弦必须仍然生效——
   非 burst 键只 flush 缓冲并**放行**给正常路径（用 `burst_flush_pending` 把
   原本的 `Idle` 提升为 `Redraw`，帧不会脏）；② 和弦不是粘贴内容，
   它到来时缓冲必须先落地，否则粘贴文本会跨和弦错位。
3. **flush 由驱动 beat 驱动。** 缓冲只有在"安静下来"之后才会变成草稿，
   而安静是时间事件，不是按键事件。驱动每圈调 `tick_paste_burst(now)`，
   并用 `paste_burst_deadline()` 决定 poll 超时（活跃时 1 ms 起、封顶 50 ms），
   这样粘贴结束后最多一个 beat 就上屏，不需要后台线程、不需要新依赖。

## 3. 门禁（本轮实测）

| 门禁 | 命令 | 结果 |
|---|---|---|
| 单测（主门） | `cargo test --offline -p pi-tui` | **1099 / 0**，75 target（基线 1075/0 → **+24**） |
| 粘贴行为 | `cargo test --offline -p pi-tui --test lum1461_paste_burst` | **11 / 0** |
| 帧 | `cargo test --offline -p pi-tui --test lum1461_burst_frames` | **3 / 0** |
| 全仓编译 | `cargo check --offline --workspace --all-targets` | exit 0（只剩 `rquickjs-core`(vendor) 与 `pi-extensions` 的既有告警） |
| 格式 | `cargo fmt --all -- --check` | exit 0 |
| lint | `cargo clippy --offline -p pi-tui -p pi-coding-agent --all-targets` | `pi-tui` **0 告警**；`pi-coding-agent` 只余 `rquickjs-core`(vendor) / `pi-extensions` 的既有告警，本轮文件 0 命中 |
| 反向验证 | 关掉 `paste_burst` 或删掉 `step_composer_burst` 的 `Enter` 分支 | `a_fast_run_becomes_one_paste_marker` / `a_newline_inside_a_burst_never_submits` 立刻失败 |

**既有失败（与本轮无关，未处置）**：`cargo test -p pi-coding-agent` 在本机有若干条
与粘贴通道无关的失败：`--lib` 8 条（`paths::absolute_paths_*` 的 Windows 路径分隔符、
`trust::*`、`export::*`、`js_loader::*`、`resource_loader::*`、`mod_ignore::*`），
`--test cli_extensions` 10 条（本机 mock provider 的 `transport error` 与 trust 提示）。
失败文件本轮**一行未碰**（`git status` 可证），属于本机环境既有问题；
跳过它们后 `cargo test -p pi-coding-agent -- --skip …` 的其余 558 条全绿。

## 4. 验收清单（逐条挂钩用例）

| 验收项 | 用例 | 位置 |
|---|---|---|
| 窗口内多字符判为一次粘贴（折叠生效） | `a_fast_run_becomes_one_paste_marker`（12 行按键流 → `[paste #1 +12 lines]`，展开回 12 行） | `tests/lum1461_paste_burst.rs:121` |
| 窗口外逐字符正常插入 | `a_run_outside_the_window_is_ordinary_typing`（100 ms/字符 → 无 marker、`tick` 无事可做） | `:146` |
| 误判不丢字符 | `a_fast_run_of_ordinary_characters_keeps_every_character`（`xyz` → 文本仍是 `xyz`） | `:164` |
| 含 `\n` 的 burst 不触发提交 | `a_newline_inside_a_burst_never_submits`（`abc` + `Enter` + `def` → `abc\ndef`，全程无 `Submitted`） | `:176` |
| 和弦打断 burst 不丢文本 | `a_chord_ends_the_burst_without_losing_its_text`（`Ctrl+O` 前 flush，仍红绘） | `:193` |
| `Alt+F` 一步跨过整个 marker | `alt_f_crosses_a_whole_marker_in_one_step`（0 → 3 → 24 → 29） | `:228` |
| `Alt+B` 一步跨过整个 marker | `alt_b_crosses_a_whole_marker_in_one_step`（29 → 25 → 4） | `:251` |
| marker 内不落点 | `word_steps_never_land_inside_a_marker`（正反各 8 步，落点永不落在 4..24 开区间） | `:264` |
| 历史召回：有注册表仍展开（**正向**） | `a_recalled_marker_with_a_live_registry_still_expands`，且**不**提示 | `:290` |
| 历史召回：无注册表降级为普通文本（**反向**） | `a_recalled_marker_without_a_registry_degrades_to_plain_text`（`expanded_text` 是字面量、`Backspace` 只删一个字符、提示只出一次） | `:308` |
| 提示真的上屏 | `the_app_flashes_the_stale_marker_hint_once`（真 `render_snapshot` 里出现，第二次召回不再出现） | `:333` |
| burst 判定单元级 | `three_fast_characters_become_one_burst` / `characters_outside_the_window_stay_ordinary_typing` / `a_burst_flushes_only_after_the_idle_timeout` / `enter_inside_a_burst_is_a_newline_and_the_window_outlives_the_buffer` / `abort_hands_every_buffered_character_back` / `the_flush_deadline_tracks_the_active_burst` | `src/input.rs`（`mod tests`） |
| 按词原子的纯函数级 | `one_backward_step_crosses_a_whole_marker` / `one_forward_step_crosses_a_whole_marker` / `without_atoms_a_marker_is_walked_character_by_character` / `atoms_are_only_merged_when_the_range_holds_them_whole` | `src/word_navigation.rs`（`mod tests`） |

## 5. 偏差清单（诚实条目）

1. **`app.rs` 的"双击选词粒度"没有改，因为不需要改。** 上游 composer 的
   `Editor.handleMouse`（`packages/tui/src/components/editor.ts:618-666`）只处理单击定位光标，
   **没有**双击选词；双击选词只存在于 transcript 层
   （`getWordSelection`，`tui-alt-screen.ts:1156-1197`），它作用在**已渲染的行**上，
   而 marker 在渲染时早已展开/按普通文本画过。所以"补双击选词粒度"这个"必要时"项
   在本端口与上游都无对象；`Alt+B`/`Alt+F` 才是 marker 按词原子的真入口。
2. **burst 只在 composer 层分类，不覆盖 `app.*` 模态。** burst 判定在模态层**之后**才被调用
   （`app.rs:3000`），所以对话框 / settings / selector / 自定义 overlay 打开时根本到不了它；
   `Ctrl+R` 搜索另有一道显式守卫（`app.rs:3269`）。这些层拥有键盘，其中的字符不是草稿内容。
3. **`paste_burst` 不进 §4.1 加权公式。** 它是 codex 独有、上游 pi-ts 没有对应格的能力，
   本轮按 LUM-1450 的规矩只**新登记**为 1/1，不擅自加轴进公式；
   公式里动的仍然只有测试轴（+0.02pt）。
4. **Windows 的空闲窗是 60 ms（codex 同值）。** 因此本机 `tick` 的截止时刻比 Linux 晚，
   截图/用例都用显式 `Instant` 驱动，不依赖真实 sleep。
5. **`Enter` 抑制窗的边界行为与 codex 一致但值得知道**：粘贴结束后 120 ms 内的一个
   `Enter` 会被当成粘贴里的换行而**不提交**。这是 codex 的取舍（多行粘贴的末尾换行
   必须留在粘贴里），本轮照抄；窗口外的 `Enter` 正常提交（`a_run_outside...` 与
   `composer_paste.rs` 的既有提交用例覆盖两侧）。
6. **无 PTY 验收**：本机 Windows 无 `pty`/`pyte`，截图仍是 frame-buffer 冻结帧（§6），
   与 LUM-1460 的诚实差距相同。

## 6. 截图（frame-buffer，120×24，真 `App::render_to_buffer`）

| 图 | 证明什么 |
|---|---|
| `docs/screenshots/lum1461-burst-marker-120x24.png`(+`.txt`) | 12 行粘贴**以按键流送入**（无 bracketed paste），安静后 composer 只有一行 `> [paste #1 +12 lines]▍` |
| `docs/screenshots/lum1461-burst-two-markers-120x24.png`(+`.txt`) | 两次突发之间夹着**慢速手打** ` and `，各自保留摘要：`[paste #1 +11 lines] and [paste #2 1500 chars]` |
| `docs/screenshots/lum1461-burst-stale-recall-120x24.png`(+`.txt`) | 跨会话历史召回的 marker 无内容：草稿是**字面量**，状态栏给出一次性提示 `Pasted content recalled from a previous session is no longer available` |

**诚实说明**：本机没有 PTY，这三张是**冻结帧**——它们证明"画出来的东西"，
不证明按键/字节时序（时序由 `lum1461_paste_burst.rs` 的 11 条驱动级用例覆盖，
时钟是显式注入的 `Instant`）。生成命令：

```bash
CARGO_TARGET_DIR=<dir> cargo test --offline -p pi-tui --test lum1461_burst_frames \
    <case> -- --exact --nocapture      # 取 FRAME DUMP … END FRAME DUMP 区间
python pi-rust/scripts/frame_to_png.py \
    --text pi-rust/docs/screenshots/lum1461-burst-marker-120x24.txt \
    --out  pi-rust/docs/screenshots/lum1461-burst-marker-120x24.png \
    --caption "LUM-1461 marker (frame-buffer, not a PTY capture)"
```

## 7. 剩余缺口 / 下一轮顺位

| 顺位 | 项 | 说明 |
|---|---|---|
| 1 | provider 三事件返回值真生效 | `pi-ai` + `pi-coding-agent`；已立 **LUM-1462**（backlog），`RUST_TS_PARITY_METRICS.md` §6/§7 第一顺位 |
| 2 | tree 改名 UI（最后一条 silent `app.*`） | **LUM-1263**（todo）；`pi-tui/tree.rs` + 驱动 |
| 3 | modal 内拖拽/悬停、settings 滚轮按矩形认领 | LUM-1450 §7 第 5 条，仍未做 |
| 4 | burst 的"持首字符"路径 | 若将来驱动有了稳定高频 beat，可把 codex 的 flicker suppression 补上（本轮有意不做，见 §1.1） |
| 5 | 真 PTY 场景 | `scripts/pty_scenarios/` 里补一条"关闭 bracketed paste 后的按键流"场景，在 Unix runner 上跑 |

## 8. 范围之外

未碰 `pi-ai` / `pi-agent-core` / `pi-protocol` / `pi-extensions` / `pi-session` /
`pi-server` / `pi-client` / `pi-chord` / `pi-evals` / `pi-telemetry` 的源码；
未碰上游 TS（`packages/**`，只读取证）；未引入任何新依赖
（时间窗用 `std::time::Instant` + `Duration`，时钟由调用方注入）。
