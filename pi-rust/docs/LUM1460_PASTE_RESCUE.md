# LUM-1460 — 抢救丢失的 composer 粘贴通道（bracketed paste + paste marker）+ Rust↔TS 真实复测

> 本轮的交付**不是新功能**，而是把一条**已写完却从未进入 `feature/pi.rs` 的交付**
> （LUM-1328 的粘贴通道 + LUM-1318 的删除重编号）救回来，并适配当前 tip。
> 所有数字都在本机（Windows / rustc 1.97.1 / 离线）实测，命令写在对应小节里。

## 0. 结论速览

| 量 | 本轮实测 | 依据 |
|---|---|---|
| composer 粘贴能力面（本期新登记轴） | **0 / 7 → 7 / 7** | §4 逐行对照 + `tests/composer_paste.rs`（27 条） |
| `pi-tui` 全量测试 | **1075 passed / 0 failed**（基线 `815b21d13` = 1045 / 0） | `cargo test -p pi-tui`，57 个 target |
| Rust 测试标记数 | **2740**（基线 2711，**+29**） | `grep -rhoE '#\[(tokio::)?test\]' pi-rust/crates --include=*.rs \| wc -l` |
| 纯代码规模（src↔src） | **91.8%**（140,615 / 153,106） | `python pi-rust/scripts/measure_loc.py`（本轮 +583 行 src） |
| 加权完成度（§4.1 权重表） | **86.8%**（86.80 → 86.83，一位小数不变） | 只有测试轴动：2,711/5,309 → 2,740/5,309 |
| TUI 交互+视觉轴 | **90.3%**（`(14×0.905 + 8×0.90) / 22`） | 同上，未重估 |

一句话结论：**粘贴通道在这条分支上是"零"——驱动从没请求 bracketed paste，
`CtEvent::Paste` 在 `translate_event` 里被翻成 `Ignored`，多行粘贴会被当成逐字符输入
（每个 `\n` 都是一次 `Enter`）。** LUM-1328/LUM-1318 早就写好并自测过这条通道，
但它们的提交只存在于 `origin/work/LUM-1328` 与 `origin/work/LUM-1318` 上，
从未合进 `feature/pi.rs`；上一轮（LUM-1450）的"收编检查"用
`git grep 'paste #'` 判它"已在树上"，**那是一条假阳性**（命中来自上游 TS 与
pi-rust 的 md 文档，`pi-rust/crates` 里 0 命中）。本轮把这条交付救回来，并按当前
tip 的 `HistoryEntry` / `Submission` 模型重新适配。

## 1. 丢失是怎么发生的（可复现）

### 1.1 提交谱系

| 提交 | 内容 | 是否在 `feature/pi.rs` 谱系内 |
|---|---|---|
| `6631b8c77` | LUM-1328 composer 粘贴通道（bracketed paste + marker 模型） | **否** |
| `3ad6cc2cd` | LUM-1318 大段粘贴折叠（哨兵实现，已被 marker 实现取代） | **否** |
| `9c20bd8f1` | LUM-1318 删除 marker 后重编号（上游 `higherIds`） | **否** |

```bash
$ for c in 6631b8c77 9c20bd8f1; do
    git merge-base --is-ancestor $c origin/feature/pi.rs && echo "$c ANCESTOR" || echo "$c NOT ancestor"
  done
6631b8c77 NOT ancestor
9c20bd8f1 NOT ancestor
```

两者都只在 `origin/work/LUM-1328`（tip `a083f2ac1`）与 `origin/work/LUM-1318`
（tip `4eb70815d`）上，是典型的"写完 → 分支漂走 → 无人合并"。

### 1.2 根因：上一轮收编检查的假阳性

`docs/LUM1450_HINT_CHORDS.md` §8 写的是：

> 未合并的只有 LUM-1318/1319/1327/1330/1333/1336 那批（`feature/pi.rs` 上已有
> **等价但不同谱系**的实现：`git grep 'paste #'`、… 均在）

那 6 条里有 5 条确实各有等价实现（1319→LUM-1415 历史搜索、1327→LUM-1426 指针定位、
1330→工具钩子、1333→LUM-1431 下拉框指针、1336→LUM-1418 列宽），
**但粘贴这条没有**。假阳性的来源：

```bash
# 基线 tip 上，不限路径：
$ git grep -l 'paste #' origin/feature/pi.rs | wc -l
12
$ git grep -l 'paste #' origin/feature/pi.rs
packages/coding-agent/CHANGELOG.md      # 上游 TS 的 changelog
packages/tui/CHANGELOG.md
packages/tui/README.md
packages/tui/src/components/editor.ts   # 上游实现本身
packages/tui/test/editor.test.ts
packages/tui/test/word-navigation.test.ts
pi-rust/docs/LUM1312_CHATINPUT_MULTILINE.md   # 文档里讨论过 marker
pi-rust/docs/LUM1317_COMPOSER_PAGING.md
pi-rust/docs/LUM1415_HISTORY_SEARCH.md
pi-rust/docs/LUM1436_AUTOCOMPLETE_WHEEL.md
…

# 只看 Rust 源码：
$ git grep -c 'paste #' origin/feature/pi.rs -- pi-rust/crates | wc -l
0
```

`git grep` 默认搜**整棵树**，上游 TS 的 `editor.ts` 与 pi-rust 的 md 文档都写着
`[paste #N …]`，于是"Rust 侧实现了"这个结论被自己的取证方式造了出来。
**流程修正（写进本轮）**：任何"某能力是否已在树上"的断言必须限定
`-- pi-rust/crates`，并且要有一条**行为**证据（测试名 / 帧），不能只看字面量。

## 2. 本轮落地（文件:行号）

| 文件 | 改动 | 说明 |
|---|---|---|
| `pi-rust/crates/pi-tui/src/editor.rs` | `+~460` | `pastes: BTreeMap<u32,String>` + `paste_counter`、`insert_paste`（CSI-u 解码 / `normalizeText` / 控制字节过滤 / 路径分隔 / 阈值折叠）、`expanded_text`、marker 的原子退格/前删与 `←`·`→` 跨越、删除后 `renumber_paste_markers_after`、撤销快照与历史回放带上注册表 |
| `pi-rust/crates/pi-tui/src/app.rs` | `+~90` | `App::step_paste`（模态层优先，模态开着丢弃）、`paste_text` 改走同一入口、`expanded_editor_text`、`paste_marker_count`、`Submission.draft`（见 §2.1） |
| `pi-rust/crates/pi-tui/src/prompt.rs` | `+11` | `Prompt::expanded_text`（上游 `getExpandedText`） |
| `pi-rust/crates/pi-coding-agent/src/interactive.rs` | `+~25` | `setup_terminal` / `resume_tui` 开 `EnableBracketedPaste`、`suspend_tui` 关；事件循环把 `CtEvent::Paste` 交给 `App::step_paste`；外部编辑器改用 `expanded_editor_text()` |
| `pi-rust/crates/pi-tui/tests/composer_paste.rs` | `+584` | 27 条行为用例（下 §4 逐条挂钩） |
| `pi-rust/crates/pi-tui/tests/lum1460_paste_frames.rs` | 新增 | 3 帧 frame-buffer 截图源（§6） |
| `pi-rust/crates/pi-tui/tests/composer_images.rs` | `+2` | `Submission` 新字段的两个构造点 |

### 2.1 适配当前 tip 的两个决定（不是照抄）

1. **历史模型用 HEAD 的**：老分支的 `HistoryEntry { text, raw, images }` 与当前 tip
   的 `HistoryEntry { text, images }`（LUM-1415 的搜索模型，`text` 本身就是**原始
   buffer**，sentinels/marker 在内）是两套。保留 HEAD 那套，删掉老分支的 `raw`
   字段与 `current_draft_entry` / `restore_history_entry` 辅助函数，避免同一个文件里
   出现两代模型（LUM-1318 的提交信息里已经踩过一次这个坑）。
2. **提交要带上原始草稿**：HEAD 的 `App::submit` 只拿到 `Submission { text, images }`，
   而 `text` 是**展开后**的正文——直接把 marker 丢了。新增 `Submission.draft`
   （composer 的原始 buffer，marker + sentinel 在内）与 `Submission::history_text()`，
   `App::submit` 用它写历史：**模型收到 12 行正文，`Up` 召回仍然是一行 marker**。
   这一格在本轮之前两套模型都不成立，是本轮补上的缝合点。

## 3. 门禁（本轮实测）

| 门禁 | 命令 | 结果 |
|---|---|---|
| 单测（主门） | `cargo test --offline -p pi-tui` | **1075 / 0**，57 target（基线 1045/0 → **+30** = `composer_paste` 27 + 帧 3） |
| 粘贴行为 | `cargo test --offline -p pi-tui --test composer_paste` | **27 / 0** |
| 帧 | `cargo test --offline -p pi-tui --test lum1460_paste_frames` | **3 / 0** |
| 全仓编译 | `cargo check --offline --workspace --all-targets` | exit 0（只剩 `rquickjs-core`(vendor) 与 `pi-extensions` 的**既有**告警） |
| 格式 | `cargo fmt --all -- --check` | exit 0 |
| lint | `cargo clippy --offline -p pi-tui -p pi-coding-agent --all-targets` | `pi-tui` / `pi-coding-agent` **0 告警**（修掉了老分支带来的 `question_mark` 1 条） |
| 反向验证 | 删掉 `step_paste` 分支或 `insert_paste` 的折叠判定 | `composer_paste` 立刻 `11_lines_become_a_line_count_marker` / `a_multi_line_paste_fills_the_draft_instead_of_submitting_it` 失败 |

## 4. 粘贴能力面：0/7 → 7/7（逐行可反证）

| # | 能力 | 上游 pi-ts | codex | Martty | 本轮前 | 本轮 | 钉住它的用例 |
|---|---|---|---|---|---|---|---|
| 1 | 请求 bracketed paste | `terminal.ts:184` | 同（crossterm） | 同（`main.rs:488/534`） | ❌ | ✅ `setup_terminal`/`resume_tui` 开、`suspend_tui` 关 | `a_terminal_paste_event_is_not_replayed_as_keys` |
| 2 | 粘贴事件入口 | `stdin-buffer` → `paste` 事件 | `ChatComposer::handle_paste` | `Event::Paste` → composer | ❌ `Ignored` | ✅ `CtEvent::Paste` → `App::step_paste` | 同上 + `paste_text_and_step_paste_agree` |
| 3 | 折成 marker | >10 行或 >1000 字符 → `[paste #N +L lines]` / `[paste #N C chars]` | `[Pasted Content N chars]` | 折成单行 | ❌ 全文直接插入 | ✅ 同规则同格式 | `eleven_lines_become_a_line_count_marker`、`a_long_single_line_paste_reports_its_character_count`、`ten_lines_stay_inline`、`a_thousand_characters_stay_inline` |
| 4 | 提交时还原正文 | `expandPasteMarkers` | `pending_pastes` 展开 | — | ❌ | ✅ `Editor::expanded_text()`，`Submit` 与外部编辑器都走它 | `submitting_a_marker_hands_the_model_the_content`、`enter_after_a_paste_still_submits_the_whole_draft` |
| 5 | marker 原子编辑 | `segmentWithMarkers`：退格/前删/`←`·`→` 一个单元 | `TextElement` 元素级 | — | ❌ | ✅ 退格/前删删整个 marker（连注册表内容），`←`/`→` 一步跨过 | `backspace_removes_a_whole_marker_and_forgets_its_content`、`delete_forward_removes_a_whole_marker_too`、`left_and_right_step_over_a_whole_marker` |
| 6 | 删除后重编号 | `higherIds`（`editor.ts:1389-1410`） | — | — | ❌ | ✅ 注册表下移 + 标签从后往前重写（`#10`→`#9` 会缩短 buffer） | `deleting_the_first_marker_renumbers_the_survivor_and_keeps_its_content`、`deleting_a_middle_marker_keeps_the_numbering_contiguous`、`renumbering_moves_the_caret_with_the_shorter_label` |
| 7 | 一次粘贴 = 一次撤销，注册表随撤销/历史回放 | 快照含 `pastes` | 元素级 | — | ❌ | ✅ 快照带 `pastes` + counter；历史召回后仍能展开 | `a_paste_is_one_undo_unit`、`undo_restores_a_deleted_marker_with_its_content`、`a_recalled_history_entry_can_still_expand_its_marker` |

另外四条输入清洗也在（老分支交付的一部分，逐条有单测）：
`crlf_and_tabs_are_normalized_and_control_bytes_dropped`（`\r\n`/`\r`→`\n`、tab→4 空格、
丢控制字节）、`a_csi_u_re_encoded_control_byte_is_decoded_back_inside_a_paste`
（tmux popup 的 `ESC [<cp>;5u`）、`a_pasted_absolute_path_is_separated_from_the_word_before_it`
（上游"光标前是词字符则补空格"）、`a_paste_does_not_open_the_autocomplete_dropdown`。

### 4.1 与 codex / Martty 的差异（诚实条目）

| 维度 | pi-ts | codex | Martty | 本轮 pi-rust |
|---|---|---|---|---|
| 折叠形态 | `[paste #N +L lines]`（正文进注册表） | `[Pasted Content N chars]`（`pending_pastes`） | 折成单行（`src/app.rs:710-742`） | 与 pi-ts 同格式（issue 点名"参考 codex"，但折叠格式对齐 pi-ts，因为 Rust 侧要对齐的是 pi 插件/会话语义） |
| 突发识别 | 只认 bracketed paste 事件 | 另有 `paste_burst`（时间窗内把突发字符判为粘贴，终端不发事件时兜底） | 无 | **无**（本轮未做，见 §10 第 1 条） |
| 按词移动跨 marker | `segmentWithMarkers` 也用于**按词**移动 | 元素级 | — | **只做到字符级**（`←`/`→`/退格/前删是原子的，`Alt+B`/`Alt+F` 会走进 marker 内部）——§5 偏差 1 |

## 5. 偏差清单（相对上游，逐条说明为什么不改）

1. **按词移动/词选择不认 marker**：上游 `segmentWithMarkers(..., "word")` 让 `Alt+B`
   一步跳过整个 marker；Rust 的 `word_navigation.rs` 只按词边界走。改动要同时碰
   `find_word_backward/forward` 与选择粒度，属于下一轮的单点切片（§10 第 2 条）。
2. **id 计数器不递减**：上游删除 marker 时 `pasteCounter--`（`editor.ts:1394`），
   本端口保持单调，所以删掉 `#1` 后新粘贴会拿 `#3`（草稿里编号仍连续）。
   理由：一个已经写进 buffer 的 id 不复用；代价是"下一号可能跳号"。
3. **`clear()` 不清空注册表**：上游 `submitValue` 里 `pastes.clear()`。本端口保留，
   因为历史召回要靠它把 marker 重新展开（上游召回后 `expandPasteMarkers` 找不到条目
   就把 marker 当普通文本提交）。
4. **`pastes` 不进跨会话历史文件**：落盘的是 marker 文本；重启后召回一条带 marker 的
   历史，展开只会得到 marker 字面量。与上游"持久化文件同样不存注册表"一致。
5. **未接 `paste_burst`**：见 §4.1。
6. **marker 无独立配色**：上游也只把 marker 当普通文本渲染（`isPasteMarker` 只参与
   折行分块），因此本端口不引入新样式。
7. **无 PTY 验收**：老分支的 `scripts/pty_scenarios/lum1328-paste.json` /
   `lum1318-paste-fold.json` 与它们的基线截图**没有带进本轮**（本机 Windows 无
   `pty`/`pyte`，带进来也跑不了）。截图走 frame-buffer 通道（§6），这是本轮
   与"真 PTY 实拍"之间的诚实差距。

## 6. 截图（frame-buffer，100×24，真 `App::render_to_buffer`）

| 图 | 证明什么 |
|---|---|
| `docs/screenshots/lum1460-paste-marker-100x24.png`(+`.txt`) | 12 行粘贴后 composer **只有一行** `> [paste #1 +12 lines]▍`，正文一行都没画进输入区 |
| `docs/screenshots/lum1460-paste-two-markers-100x24.png`(+`.txt`) | 两次粘贴各自保留摘要：`[paste #1 +11 lines] and [paste #2 1234 chars]` |
| `docs/screenshots/lum1460-paste-submitted-100x24.png`(+`.txt`) | 回车提交后，transcript 的 user 消息里是**展开后的 12 行**（`> log line 1 … > log line 12`） |

**诚实说明**：本机没有 PTY，这三张是**冻结帧**——它们证明"画出来的东西"，
不证明按键/字节时序（时序由 `composer_paste.rs` 的 27 条驱动级用例覆盖）。
另外 `docs/screenshots/lum1328-paste{,-baseline}.png` 是老分支的实拍截图，
本轮一并带进树作为**历史证据**，不是在本轮代码上拍的。

## 7. Rust↔TS 百分比（本轮复测）

| 口径 | 本轮 | 基线 `815b21d13` | 变化 |
|---|---|---|---|
| 纯代码规模（src↔src） | **91.8%**（140,615 / 153,106） | 91.5%（140,032） | +583 行 src |
| 测试规模（`#[test]` 标记 / TS 用例） | **51.6%**（2,740 / 5,309） | 51.1%（2,711） | +29 条 |
| TUI 模块面 | 35 / 42 = 83.3% | 同 | 无新模块（marker 逻辑落在既有 `editor.rs`） |
| `app.*` 接线 | 43 / 44 = 97.7%（silent 1：`app.tree.editLabel`） | 同 | 未动 |
| `tui.*` 消费面 | 49 / 49 = 100% | 同 | 未动 |
| 扩展生命周期事件 | 36 / 36（声明 + 生产构造点） | 同 | 未动 |
| **composer 粘贴能力面** | **7 / 7 = 100%** | **0 / 7 = 0%** | 本轮的实质变化 |
| 加权完成度（§4.1 权重表） | **86.8%** | 86.8%（86.80） | 只有测试轴 +0.03pt |

```text
5×1.00 + 13×0.90 + 8×1.00 + 6×0.70 + 14×0.905 + 8×0.90 + 7×0.783 + 7×0.70
  + 8×0.95 + 7×1.000 + 9×0.85 + 5×(2740/5309) + 3×0.95 = 86.83% → **86.8%**
```

**为什么加权只动了 0.03pt**：粘贴通道不在 §4.1 的 13 条轴里——它是轴 5（TUI 交互面）
内部的一格，而轴 5 用的是"模块率与 `app.*` 接线率的均值"，两条输入都没动。
本轮按 LUM-1450 的规矩**不擅自加轴进公式**：先把它作为**新登记的量**（0/7 → 7/7）
公开，下一轮若要进公式，必须先说清权重来源。

## 8. 「最多 3 个任务」的处置 = 1 自做 + 2 新派 + 1 入库 backlog

* **自做（1 件）**：`pi-tui/src/{editor,app,prompt}.rs` + `pi-coding-agent/interactive.rs`
  + 两个测试文件 + 文档/截图。
* **收编（0 件）**：本轮开工时复查了 `origin` 上所有 `work/*`、`agent/*` 分支：
  唯一"已写完未合并"的交付就是本轮的 LUM-1328/LUM-1318（其余 5 条各有等价实现，
  见 §1.2）。**LUM-1452**（在办、范围就是本条通道，上一轮死在 ENOSPC、零提交）
  的范围由本轮交付覆盖。
* **新派（2 件）**：
  ① `LUM-1461`（新建 → 提升为 todo）：composer `paste_burst`（终端不发 bracketed paste
  时的突发识别，codex 对齐）+ marker 按词原子 + 持久化历史 marker 降级；
  ② `LUM-1263`（既有 backlog，**口径更正后提升为 todo**）：tree 改名 UI，补上最后一个
  silent `app.*`。它的原背景（LUM-1260 的 35/44、`/scoped-models` 与 `app.models.*` 6 个）
  **已经过期**——实测 `app.models.*` 6 个全部已接线、`/scoped-models` 选择器已存在，
  只剩 `app.tree.editLabel`（`grep -rn '"app.tree.editLabel"' pi-rust/crates` = 0 命中），
  故本轮把它的正文收窄到树改名一项再启动，避免重复劳动。
  两条 run 的文件面彼此不重叠（1461 = `input.rs`/`editor.rs`/`app.rs`；
  1263 = `tree.rs`/`interactive.rs`），也与在办的 LUM-1434（`cli/`）零重叠。
* **入库 backlog（1 件）**：`LUM-1462`（`pi-ai` provider 三事件返回值真生效：
  `before_provider_request` / `before_provider_headers` / `after_provider_response`），
  它是 `RUST_TS_PARITY_METRICS.md` §6/§7 的第一顺位，但**不属于 issue 点名的 TUI 面**，
  因此只入库、不启动 run。
* **顺带发现（未处置）**：`LUM-1433`（backlog，"下拉框滚轮 + `#` 触发符补全"）的两项
  已在 LUM-1436 / LUM-1448 落地，属可关闭的过期项；`LUM-1420`（composer `--clear-history`
  + Ctrl+R 命中高亮）与上面的 1461 都在 `editor.rs` 上，不宜同期并发，留作下一顺位。
* **为什么正好 2 条新派**：本 run 结束后占用 = 新派 2 + 待办 LUM-1434 1 = 3，
  已达上限；LUM-1453 仍是 backlog。

## 9. 磁盘：上一轮 ENOSPC 的根因与本轮处置

`LUM-1452` 的 run 死在 `ENOSPC: no space left on device`（workdir 上只留下这一条系统
评论）。实测根因：`C:` 盘 600G 已用 590G（**99%**），而本 workdir 一处就躺着
**146G** 构建缓存，其中 10 个已完成轮次的 `pi-rust/target` 各占 11–23G
（1452 那轮自己留下 6.6G）。

处置：本轮把构建缓存放到另一块盘（`CARGO_TARGET_DIR=D:/cargo-target-lum1460`，
D: 余量 69G），不再挤 `C:`；同时回收**已完成轮次** worktree 下的 `target/`
（纯 cargo 产物，删掉只是重新编译，源码、提交、远端分支一律不动）。
本轮不删在办 worktree 的 `target`。

## 10. 剩余缺口 / 下一轮顺位

| 顺位 | 项 | 范围 | 说明 |
|---|---|---|---|
| 1 | `paste_burst` | `pi-tui` `input.rs`/`editor.rs` | codex 在"终端不发 bracketed paste"时的兜底；**已立 LUM-1461（提升为 todo）** |
| 2 | 按词移动/词选择认 marker | `pi-tui` `editor.rs`/`word_navigation.rs` | §5 偏差 1；上游 `segmentWithMarkers(..., "word")` |
| 3 | 持久化历史里的 marker 展开 | `pi-tui` `history_store.rs` | 偏差 4；与上游一致地"不存注册表"，但可以在召回时把 marker 降级成文本并给出提示 |
| 4 | provider 三事件返回值真生效 | `pi-ai` + `pi-coding-agent` | §6/§7 第一顺位以外唯一的大格；**已立 LUM-1462（backlog）** |
| 5 | tree 改名 UI | `pi-tui` `tree.rs` + `pi-coding-agent` | **LUM-1263（口径更正后提升为 todo）** |
| 6 | modal 内拖拽/悬停、settings 滚轮按矩形认领 | `pi-tui` | LUM-1450 §7 第 5 条，仍未做 |

## 11. 范围之外

未碰 `pi-ai` / `pi-agent-core` / `pi-protocol` / `pi-extensions` / `pi-session` /
`pi-server` / `pi-client` / `pi-chord` / `pi-evals` / `pi-telemetry` 的源码；
未碰 slash 命令表与扩展事件轴；未碰上游 TS（`packages/**`，只读取证）。
