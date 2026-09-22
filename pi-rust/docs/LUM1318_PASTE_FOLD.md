# LUM-1318 — 大段粘贴折叠成 `[paste #N +M lines]`（LUM-1312 后续 2/3）

> scope: `pi-rust/crates/pi-tui/src/{editor,prompt,app}.rs`,
> `pi-rust/crates/pi-coding-agent/src/interactive.rs`,
> `pi-rust/crates/pi-tui/tests/{composer_pastes,composer_paging}.rs`,
> `pi-rust/scripts/{pty_capture.py,pty_scenarios/lum1318-paste-fold.json}`
> branch: `work/LUM-1318` → `feature/pi.rs`
> reference: 上游 `packages/tui/src/components/editor.ts` 的 `handlePaste` /
> `PASTE_MARKER_REGEX` / `pastes` 表 / `higherIds` 重编号 / `expandPasteMarkers`，
> 以及 `packages/tui/src/terminal.ts:184` 的 `\x1b[?2004h`（bracketed paste 开关）

一句话结论：composer 现在按上游语义**折叠大段粘贴**——超过 10 行或 1000 字符的粘贴不再
内联进 buffer，而是进侧表、buffer 里只留一个哨兵字符，渲染成 `[paste #N +M lines]`
（按长度折叠时是 `[paste #N M chars]`）。200 行粘贴 = **1 行 composer**；`Enter` 交给模型的
是**原文**；`Backspace`/`Delete`/行域 kill 命中哨兵时整块移除并自动重编号；`Ctrl+-` 恢复整块；
与图片 chip 混用不串位。同时把驱动接上 **bracketed paste**（上游本来就开着）：粘贴由一个
`Event::Paste` 事件送达，而不是几十上百次按键重放——这正是真 PTY 里能稳定折叠的前提。

---

## 1. 真实审计：本轮之前发生了什么

| 行为 | 本轮之前（`6a5a2faf2` 树，真 PTY/单测实测） | 本轮之后（真 PTY 实测） |
|---|---|---|
| 200 行粘贴 | 整段 3091 字节内联进 buffer：composer 按 `composer_max_rows` 长到满窗，`↑` 上方还有 61+ 行 | 一个哨兵字符 → 一行 `> [paste #1 +200 lines]`（截图面板 2/6，帧 `d79f4804d0ef`/`76dc1686a82c`） |
| 终端粘贴的送达方式 | 驱动从未开启 bracketed paste：`grep -rn 'EnableBracketedPaste\|Event::Paste'` 全仓 0 命中，粘贴按字节变成一串按键事件（连 `\n` 都按 `Ctrl+J` 逐字重放） | `setup_terminal`/`resume_tui` 开 `EnableBracketedPaste`、`suspend_tui` 关；`CtEvent::Paste` 走 `App::paste_text` → 折叠（`interactive.rs:4126,4176,4195,616`） |
| 提交文本 | `EditorAction::Submit(self.display_text())`，buffer 里就是全文，模型收到全文 | `Submit(self.expanded_text())`：标记展开回原文，模型收到的仍是原文，**不会**收到 `[paste #N …]` |
| 删除 | 粘贴的文本是普通字符：删一个字符只删一个字符，`Ctrl+U` 杀掉整行几百字符 | 哨兵是单字符：`Backspace`/`Delete` 一次删整块，kill 命中后剩余标记自动重编号（面板 9：`[paste #2 +11 lines]` 删掉 `#1` 后变 `[paste #1 +11 lines]`，且**载荷跟着标记走**） |
| kill ring / yank | 无标记可复活 | 被 kill 的文本先 `strip_sentinels`，yank 不会插回一个没有载荷的标记（与图片 chip 同规则） |
| 外部编辑器 / `ctx.ui.getEditorText` | 只给得到 buffer 文本 | `App::editor_text()` 改为 `expanded_text()`，对齐上游 `getExpandedText() ?? getText()`（`interactive-mode.ts:2447`） |

`Ctrl+U`/`Ctrl+K` 的可用性也随之恢复：折叠后它们作用在**草稿的当前行**（LUM-1312 的语义），
而不是几百字符的粘贴内容。

## 2. 设计：哨兵字符 + “位置即编号”

移植上游语义时**没有**照搬上游的数据结构，而是复用了端口已有的 chip 模型
（`CHIP_CHAR` + `images` 对齐），因为哨兵模型天然满足上游用正则维持的那些不变量：

| | 上游 `packages/tui/src/components/editor.ts` | 本端口 |
|---|---|---|
| buffer 内容 | 标记**文本**本身（`[paste #1 +123 lines]`）写进 `state.lines` | 一个哨兵字符 [`PASTE_CHAR`]（U+FFFB，与 `CHIP_CHAR` U+FFFC 同族且互不相同，`insert_char` 拒绝直接输入两者） |
| 载荷表 | `private pastes: Map<number, string>` + `pasteCounter` | `pastes: Vec<Paste>`（`text` + 预先算好的 `summary`）；**没有** `paste_counter` |
| 编号来源 | 计数器分配 id，标记文本里存 id | id = 在 buffer 中的**出现序号**（第 n 个哨兵 ↔ `pastes[n-1]`，与图片 chip 同一套对齐规则） |
| 删除后重编号 | 删表项 + 递减计数器 + 正则重写所有更大 id 的标记文本（`higherIds`，`:1388-1401`） | `pastes.drain(..)`：位置即编号，**重编号是删除的副产品**，不需要二次遍历 |
| 原子性 | `segmentWithMarkers` 把标记文本合并成一个 grapheme（只对仍有效的 id） | 哨兵本来就是**一个字符**：光标移动 / 退格 / 行域 kill 天然整块处理，不需要分词器参与 |
| 渲染 vs 提交 | `getText()` = 标记文本；`getExpandedText()` 用 `expandPasteMarkers` 展开 | `display_text()` = 标记；`expanded_text()` = 原文；`Submit` 走后者 |
| 折叠判定 | `pastedLines.length > 10 \|\| totalChars > 1000` | `should_fold_paste`：`split('\n').count() > 10 \|\| chars().count() > 1000`（行数口径完全一致；字符数用 Unicode 标量而非 UTF-16 code unit，只有贴边界的星平面字符会落在不同侧） |
| 标记摘要 | 展开时按行数决定 `+N lines` / `N chars`，每次都要数 | 折叠时算一次存进 `Paste.summary`，每帧渲染不再扫载荷 |

`paste_counter` 是这些差异里唯一“上游有、端口没有”的状态：在“位置即编号”下它不再被任何
读路径使用（id 由位置推出），保留就只是死状态，因此没有引入；上游需要它，是因为它要把 id
写进 buffer 文本再靠正则找回来。

## 3. 真 PTY 证据

场景 `scripts/pty_scenarios/lum1318-paste-fold.json`（100×30，`--model faux/faux-model`，
本提交构建的 `target/debug/pi`），截图与字符网格在
`docs/screenshots/lum1318-paste-fold.png{,.txt}`：

```bash
python3 scripts/pty_capture.py --bin target/debug/pi \
  --steps scripts/pty_scenarios/lum1318-paste-fold.json \
  --out docs/screenshots/lum1318-paste-fold.png \
  --text-out docs/screenshots/lum1318-paste-fold.png.txt
```

**断言 22/22 PASS**（`assert` 行也在 `.txt` 里，逐条可查）：

| 面板 | 结论 | 关键帧 |
|---|---|---|
| 1 | idle，composer 一行 | `af8a48851164` |
| 2 | 200 行 bracketed paste → **一行** `> [paste #1 +200 lines]▍`；`reject 'pasted line 1' / 'pasted line 200' / '  pasted line'` 全过（正文没进屏幕、transcript 也没多出东西） | `d79f4804d0ef` |
| 3 | `Enter` 提交：transcript 出现 `pasted line 200`，且 `reject '[paste #'` → 模型拿到的是全文、不是标记 | `af599f364c35` |
| 4 | `Up` 召回：草稿回到全文（history 存的是展开后的 prompt，见 §5.1），仍然 `reject '[paste #'` | `54a5a87e62d7` |
| 5 | `Ctrl+C` 清空召回的大草稿，网格与面板 3 **逐字节相同**（帧哈希同为 `af599f364c35`）= 空 composer + 未动的 transcript | `af599f364c35` |
| 6 | 再粘一次：又折叠成 `#1` | `76dc1686a82c` |
| 7 | 标记就是普通草稿字符：` summary` 打在它旁边 | `43e0fb1cc7ce` |
| 8 | 第二个 11 行粘贴 → `[paste #2 +11 lines]` | `fab9c86b9eb5` |
| 9 | `Ctrl+A`+`Del` 删掉**第一个**标记：剩下的被重编号为 `#1` 且显示自己的 `+11 lines`（`reject '+200 lines'` 通过） | `e7afd30c6716` |
| 10 | 紧接着 `Enter`：transcript 出现 `second paste row 11` → 重编号后**载荷没有串位**（若串位这里会送出 200 行那份） | `f99267f80883` |
| 11 | 同一进程内的 A/B：同样的 200 行**按键重放**（无 bracketed paste 标记）→ 不折叠、草稿堆满窗口（`↑` 上方 61 行） | `c6325e9af5bd` |

实测数字（同一组 run，字符网格逐帧可复算）：

- 折叠后 composer 行数 **1**（`> [paste #1 +200 lines]▍`，面板 2/6）；同一段文本按按键送达时
  composer 窗口 **7 行 + `↑`**，且 6 秒内只重放了 200 行里的 **68 行**（面板 11 冻结帧停在
  `pasted line 6▍`，约等于 crossterm 一次 1024 字节读的量）。
- 折叠判定边界：10 行 / 1000 字符**不折叠**，11 行 / 1001 字符折叠（`fold_thresholds_match_upstream`，
  真 PTY 侧由面板 2（200 行）与面板 8（11 行）覆盖）。
- 断言/截图之外没有“人工描述的成功”：22 条断言全部由 pyte 网格判定，`frame`/`px` 哈希写在 `.txt` 里。

## 4. 单元 / 集成测试

`cargo test -p pi-tui`（全绿）新增：

- `crates/pi-tui/src/editor.rs` 单测 10 条：阈值边界、标记渲染 + 提交逐字等于原文、
  退格删整块并连续重编号、`Delete`、undo 还原整块（再 undo 连标记本身一起撤）、
  kill ring 不复活无载荷标记（含“只 kill 到一个标记时 ring 保持空、yank 是 no-op”）、
  chip+paste 交错不串位（含在草稿头部插入后两个表一起位移）、`set_text` 丢掉哨兵与载荷、
  哨兵不可输入、`clear` 清空。
- `crates/pi-tui/tests/composer_pastes.rs` 8 条（走真实 `App`）：200 行粘贴后 composer 行数 ≤2
  且 `editor_text()` 仍是全文、`Enter` 经 `StepOutcome::Submitted` 把原文交给 `Agent` 且 transcript 是全文、
  小粘贴保持字面、chip+paste 提交顺序与 content block 数量、退格整块删 + undo、
  历史召回（chip 还原、标记降级为文本）、`follow_up_from_editor` 提交展开文本、`clear_composer` 清空。

既有测试只改了一处：`composer_paging.rs:140` 用 `insert_str(&draft(12))` 造一个 12 行草稿，
12 行 > 10 行阈值，现在会被折叠——该测试要的是“高草稿”而不是“折叠粘贴”，因此改成
`set_text(draft(12))`（程序化写入路径，语义不变）。其余 400+ 条未改动、全绿。

## 5. 已知偏差与限制（真实，不粉饰）

1. **history 存的是展开后的 prompt**：`addToHistory` 在上游拿到的也是 `submitValue` 展开后的文本，
   所以召回一条折叠过的粘贴会把全文放回草稿（面板 4 就是这个行为）。端口与上游一致，
   本轮**没有**改成“history 存标记”。副作用：历史文件（LUM-1319 的跨会话 history）也会追加全文。
2. **history 条目无法还原折叠态**：会话内条目的 `raw`（含哨兵）在 `push_history_entry` 里被拒绝
   （条目没有地方存粘贴原文，还原出哨兵就成了“没有载荷的标记”），改走
   `rebuild_raw(display_text, images)`：chip 照样还原（`[Image #N]` 标签反推），标记部分按文本落地。
   图片 chip 的召回能力不受影响（`composer_pastes.rs` 的召回用例覆盖）。
3. **kill ring 丢载荷**：跨哨兵的 kill 先 `strip_sentinels` 再入 ring，所以被 kill 的粘贴原文不会被
   yank 回来（与图片 chip 已有规则一致，`editor.rs` 的 `kill_ring_never_resurrects_a_chip_without_its_payload`）。
   上游的 ring 存的是标记文本，若 id 恰好还有效会重新展开——端口**故意不模仿**：那会让一个陈旧标记
   指向另一段粘贴的载荷。
4. **`handlePaste` 的清洗没有移植**：上游在折叠前会解 CSI-u 控制字符、展开 tab、过滤不可打印字符、
   给“路径形”粘贴补前导空格；端口的折叠直接吃 `insert_str` 拿到的字符串（只做 `strip_sentinels`）。
   这些属于上游终端层的整理，本端口没有对应层。
5. **没有粘贴数量上限**：上游也没有；图片 chip 的 8 张上限不变。
6. **发现的既有缺陷（本轮未修，已如实记录）**：一次性向 stdin 写入 >1 KiB 的**按键字节**时，
   驱动的读取循环会在读完约一个 1024 字节块后停住——应用还在跑（进程状态 `R`、CPU 时间继续增长、
   仍在写帧），但之后的按键（包括 `Ctrl+C`）都不再被消费，20 秒后仍停在同一帧。
   60 行按键（890 B）可以正常走完，200 行（3091 B）停在 68 行（≈1023 B）。
   这与折叠无关：在同一台机器上用**不含本轮改动**的二进制
   （`work/LUM-1327` 的 `/tmp/pi-ab/pi-baseline`）复现出完全相同的 68 行停顿。
   本轮接上 bracketed paste 之后，真实粘贴不再走这条路径（一个 `Event::Paste`），
   但“大块按键字节”这条路径的问题仍在，建议单独立项（根因在 crossterm 读取/`poll(ZERO)` 层，
   不在编辑器）。

## 6. 门禁与交付

改动清单：

| 文件 | 改动 |
|---|---|
| `crates/pi-tui/src/editor.rs` | `PASTE_CHAR`(381) / `PASTE_FOLD_LINE_THRESHOLD`,`PASTE_FOLD_CHAR_THRESHOLD`(387,391) / `Paste`(463) / `should_fold_paste`(507) / `expanded_text`+`render`(840,846) / `paste_label_width`(913) / `paste_attachments`,`paste_count`(920,925) / `remove_pastes_in_range`,`remove_attachments_in_range`(996,1015) / `insert_folded_paste`(1497) / `strip_sentinels`(2838)；`display_text`/`display_cursor`/`kill_range`/`yank`/`yank_pop`/`undo`/`clear`/`set_text_internal`/`set_buffer_and_images`/`push_history_entry` 相应接线 |
| `crates/pi-tui/src/prompt.rs` | `Prompt::expanded_text`(90) |
| `crates/pi-tui/src/app.rs` | `editor_text` 走展开文本(2544) / `follow_up_from_editor` 提交展开文本(2161) / `CtEvent::Paste` 显式归为 `Ignored`(5446) / `paste_text` 文档(4639) |
| `crates/pi-coding-agent/src/interactive.rs` | `EnableBracketedPaste`(4126,4195) + `DisableBracketedPaste`(4177) + `Event::Paste` → `App::paste_text`(616) |
| `crates/pi-tui/tests/composer_pastes.rs` | 新增 8 条 App 级测试 |
| `scripts/pty_capture.py` | 新增 `encode_paste`(179) 与 panel 的 `paste` 字段(768)：把面板内容当成一次 bracketed paste 发送 |
| `scripts/pty_scenarios/lum1318-paste-fold.json`, `docs/screenshots/lum1318-paste-fold.png{,.txt}` | 11 面板 / 22 断言的实拍证据 |

门禁命令（本沙箱同一卷上同时跑 3 个 agent，按 LUM-1224 记录的磁盘缓解前缀执行）：

```console
$ . pi-rust/scripts/toolchain.sh
$ CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 CARGO_NET_OFFLINE=1 \
    cargo fmt --all -- --check
$ CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 CARGO_NET_OFFLINE=1 \
    cargo clippy --workspace --all-targets --locked -- -D warnings
$ CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 CARGO_NET_OFFLINE=1 \
    cargo test --workspace --locked
```

结果见下方 §6.1（本文件随最后一次门禁运行更新）。

### 6.1 门禁实测结果

<!-- GATE-RESULTS -->
