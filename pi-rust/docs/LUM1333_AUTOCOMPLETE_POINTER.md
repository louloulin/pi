# LUM-1333 — 补全下拉的指针路由（`Editor.handleMouse` 下拉分支 / `SelectList.handleMouse`）：真 PTY A/B、门禁、已知限制

> 范围：`pi-rust/crates/pi-tui/src/{app,editor,input}.rs`、
> `pi-rust/crates/pi-tui/tests/autocomplete_pointer.rs`（新增）、
> `pi-rust/crates/pi-tui/tests/{autocomplete,mouse_scroll,mouse_selection,settings_list}.rs`（跟随签名/构造器）、
> `pi-rust/scripts/pty_capture.py`（新增滚轮 token）、
> `pi-rust/scripts/pty_scenarios/lum1333-autocomplete-pointer{,-baseline}.json`、
> `pi-rust/docs/screenshots/lum1333-autocomplete-pointer{,-baseline}.png(.txt)`
> 参照实现：上游 pi `packages/tui/src/components/editor.ts:620-640`（`handleMouse` 的下拉分支）、
> `packages/tui/src/components/select-list.ts:109-140`（`SelectList.handleMouse`）
> 基线：`origin/feature/pi.rs` = `4f9817cd1`（LUM-1328 的 tip，已含 LUM-1327）；
> 本分支 `work/LUM-1333` 从 `3e7af2761` 起，合并 `4f9817cd1`（LUM-1328）与 `e0ba60d5c`（LUM-1330）
> 后在最终合并树 `9a9fff9db` 上复测并重跑真 PTY。
> 修前/修后的两个二进制：修前 = `4f9817cd1`（`git checkout origin/feature/pi.rs -- pi-rust/crates` 后构建，
> 不含本轮改动），修后 = 最终合并树 `9a9fff9db`。

一句话结论：**补全下拉的指针路径原本没人接**——下拉由 App 画在消息视口底部（composer 之上），
于是「点一行下拉」就是「聊天视口内的一次点击」：既不选中候选、也不接受候选，滚轮滚的是聊天。
本轮按上游语义把它接通（press 选中该行 / release 同格接受 / 列表内滚轮 ±1 步进并吞掉 / 列表外一律
fall through），新增 **14 条 App 级集成测试**（全部读渲染帧），真 PTY A/B 在修前二进制上
**18/18 断言「指针什么都没做」**、修后 **20/20 断言「press 选中、release 接受、滚轮步进」**，
全量门禁 `2695 passed / 0 failed / 2 ignored`（172 suites，最终合并树）。

---

## 1. 缺陷：下拉的指针是死的（真 PTY A/B，不是设计意图）

### 1.1 缺陷本身

- 下拉的**画**归 App：`App::paint_autocomplete`（`crates/pi-tui/src/app.rs:5770`）把
  `Editor` 的行画在消息视口的**底部**、composer 顶边之上（向上生长），矩形落在消息视口之内。
- 指针的**路由**却没有一层认领它：`App::step_mouse_gesture`（`app.rs:4069`）依次试
  modal → 搜索条 → composer（LUM-1327）→ jump-to-latest pill → 截断提示 → 滚动条 → 聊天选择，
  最后落到 `step_selection_mouse_gesture`：下拉行在视口内 ⇒ 被当成一次**聊天文字选择**。
- 滚轮更直接：`InputEvent::Mouse` 当时只有 `up`/`alt`（**没有坐标**），每格滚轮无条件交给聊天视口，
  所以「在列表上滚」= 滚聊天。

结论：**点一行下拉会去选聊天文字，滚轮会滚聊天**。两个参照实现都有这一层——
上游 `Editor.handleMouse` 先算 `autocompleteStartRow`，命中就把事件折算成列表局部坐标交给
`SelectList.handleMouse`（选择 / 滚动）并返回 `{...result, focus: true}`；codex 的
`chat_composer` 也把弹层命中放在编辑器自己的行之前。

### 1.2 证据：同一组手势打给两个二进制

同一份场景（`/` 开下拉 → `press` 列表第 3 行 → 同格 `release` → 清空重开 → 列表内滚轮）打给
两个二进制，真 PTY、100×20、pyte 仿真、断言在字符网格上：

| 手势 | 修前（`4f9817cd1`，18 断言 PASS） | 修后（本分支，20 断言 PASS） |
| --- | --- | --- |
| `<MPress:6,14>` 按 `new` 那一行 | 高亮仍是 `help`，窗口指示仍是 `(1/21)`；这次点击属于聊天视口 | `❯ new`，指示 `(3/21)` |
| `<MRelease:6,14>` 同格松手 | 草稿仍是 `> /▍`，下拉还在（没有候选被接受） | 草稿 `> /new ▍`，下拉与 `(k/21)` 一起消失 |
| `<WheelDown:6,15>` 列表内滚轮 | 高亮不动（`(1/21)`）——每格滚轮都被当成聊天视口的滚动 | 高亮步进到 `clear`（`(2/21)`），草稿不动 |
| `<WheelUp:6,15>` | 不动 | 步进回 `help`（`(1/21)`） |
| `<Click:6,3>` 列表外面的聊天行 | 什么都没发生（这条路径本来就正常） | 下拉保持原状，草稿仍是 `> /▍`（回归） |

```
assertions: 20 checks over 7 panels — 20 PASS, 0 FAIL, 0 XFAIL, 0 XPASS   (修后)
assertions: 18 checks over 7 panels — 18 PASS, 0 FAIL, 0 XFAIL, 0 XPASS   (修前，断言的是缺陷本身)
```

修前/修后各 7 面板合成图与字符网格 dump：
`docs/screenshots/lum1333-autocomplete-pointer{,-baseline}.png(.txt)`；
修后那一次是在**最终合并树** `9a9fff9db` 上重跑的（20/20 PASS 与上表一致）。

---

## 2. 实现：逐条对照上游

本 port 与上游的几何不同（上游把列表画在**编辑器的可见行之下**，本 port 画在 **composer 之上**），
所以命中测试留在 App 侧，而「窗口 + 行 → 候选」的映射交给 `Editor`：

| 关注点 | 上游 | 本 port | 位置 |
| --- | --- | --- | --- |
| 命中区域 | `autocompleteStartRow .. + renderedAutocompleteHeight` | 记录的下拉矩形（`autocomplete_area`）+ 当前状态 | `app.rs:1219`（字段）/ `app.rs:5770`（写入）/ `app.rs:4487`（未画则清零） |
| 行 → 候选 | `SelectList.getVisibleRange` + `event.y` | `Editor::autocomplete_visible_rows` | `editor.rs:2214` |
| 画的与点的同源 | 同一个 `SelectList` | 同一个 `autocomplete_window_rows` | `editor.rs:2224`（`autocomplete_render_styled_lines` 也走它，`editor.rs:2261`） |
| press | `selectedIndex = itemIndex` | `Editor::select_autocomplete_index` | `editor.rs:2193` |
| click（press+release 同格） | `onSelect(selectedItem)` | release 命中同格 → `accept_autocomplete()` | `app.rs:4340` |
| wheel | `delta = wheelDelta < 0 ? -1 : 1`，**clamp** 不 wrap | `Editor::scroll_autocomplete` | `editor.rs:2170` |
| hover | 不改选中 | `Move` 不改选中（press 待定时吞掉） | `app.rs:4340` |

**四件本轮特意做对的事**

1. **「画的」与「点的」同源**：`autocomplete_visible_rows(max_rows)` 与
   `autocomplete_render_styled_lines(width, max_rows)` 都由同一个私有窗口函数生成，并用同一条
   裁剪规则（装不下时丢掉**顶部**行，保留最靠近 prompt 的候选），所以命中测试不可能与渲染漂移。
   行内容也一并返回（`Some(候选下标)` / `None` 表示 `(n/m)` 指示行），因此指示行既不会被当成候选，
   也不会 fall through 到聊天选择（`AutocompleteHit::Absorbed`）。
2. **底边由当前草稿反推**：下拉挂在 composer 顶边，而 composer 会随草稿长高。命中测试用
   `composer_rows_now`（`app.rs:4311`）按**当前**草稿行数反推 composer 顶边，与 LUM-1327 给
   composer 定的规则一致——同批到达的「草稿长高 + 点击」不会把点击丢到错误的一行
   （集成测试 `a_draft_that_grew_after_the_frame_carries_the_list_with_it` 钉住了它）。
3. **矩形是「曾经画出来」的闸门**：命中测试要求 `autocomplete_area` 非零（上一帧真的画过）**且**
   当前 `is_showing_autocomplete()`。前者保证点击永远不会「选中」读者没见过的候选；后者保证任何
   让下拉关掉的编辑（以及 `Ctrl+R`）都不会靠陈旧矩形继续吃指针。
4. **指针优先级与绘制顺序一致**：下拉 → composer → pill / 截断提示 → 滚动条 → 聊天选择
   （下拉画在 pill 之上，所以它先拿指针，`app.rs:4069`）。

**滚轮为什么需要坐标**：`InputEvent::Mouse` 现在带 `x`/`y`（`crates/pi-tui/src/input.rs:248`），
SGR 与 X10 两条解码路径都带上真实格子（`decode_mouse_report`），`App::translate_event` 也把
crossterm 的 `column/row` 传下去。滚轮分支在 modal 守卫之后、聊天视口滚动之前
（`app.rs:2756`）：列表内的每一格都被吞掉——包括已经到端点、动不了高亮的那一格——因为
「列表没有页可翻」不等于「聊天该滚」。

**`Ctrl+R` 的优先级（issue §4 要求写明）**：`Editor::begin_history_search` 会
`cancel_autocomplete()`，而命中测试每次都重新问一次当前状态，所以搜索打开后：
- 键盘归搜索（本来如此）；
- 指针**不会**再落到下拉上（下拉已经不存在，陈旧矩形被 `is_showing_autocomplete()` 否掉）；
- composer 的指针在整个搜索期间仍被整体吞掉（`ComposerHit::Absorbed`，LUM-1327 的结论不变），
  聊天视口在其下方的行照旧可选。
  集成测试 `reverse_search_takes_the_pointer_from_the_dropdown` 钉住了这三条。

---

## 3. 改动清单

| 文件 | 内容 |
| --- | --- |
| `crates/pi-tui/src/app.rs` | `AutocompleteHit` / `AutocompletePress`（`app.rs:948`、`app.rs:965`）；字段 `autocomplete_area` / `autocomplete_press`（`app.rs:1219`、`app.rs:1225`）；`composer_rows_now`（`app.rs:4311`，同时给 `composer_hit` 复用）；`step_autocomplete_mouse_gesture`（`app.rs:4340`）；`step_autocomplete_wheel`（`app.rs:4411`）；`autocomplete_hit`（`app.rs:4442`）；`autocomplete_area()`（`app.rs:4475`）；`clear_autocomplete_area`（`app.rs:4487`）；`step_mouse_gesture` 里插入下拉分支（`app.rs:4069`）；滚轮分支（`app.rs:2756`）；`paint_autocomplete` 记录矩形并按 `available` 交给 editor 裁剪（`app.rs:5770`） |
| `crates/pi-tui/src/editor.rs` | `scroll_autocomplete`（`editor.rs:2170`）；`select_autocomplete_index`（`editor.rs:2193`）；`autocomplete_visible_rows`（`editor.rs:2214`）+ 私有 `autocomplete_window_rows`（`editor.rs:2224`）；`autocomplete_render_styled_lines(width, max_rows)` / `autocomplete_render_lines(width, max_rows)`（`editor.rs:2261`、`editor.rs:2296`） |
| `crates/pi-tui/src/input.rs` | `InputEvent::Mouse` 增加 `x`/`y`（`input.rs:248`）；`wheel_at`（`input.rs:336`）；`wheel` 保持 (0,0)（`input.rs:345`）；SGR / X10 解码带上坐标 |
| `crates/pi-tui/tests/autocomplete_pointer.rs` | 新增 14 条集成测试 |
| `crates/pi-tui/tests/autocomplete.rs` 等 4 个既有测试 | 跟随 `autocomplete_render_lines(w, usize::MAX)` 与 `InputEvent::wheel` 的签名变化 |
| `scripts/pty_capture.py` | 新增 `<WheelUp:x,y>` / `<WheelDown:x,y>` token（SGR 64/65） |
| `scripts/pty_scenarios/lum1333-autocomplete-pointer{,-baseline}.json` | 同一组手势的修后 / 修前场景 |
| `docs/screenshots/lum1333-autocomplete-pointer{,-baseline}.png(.txt)` | 真 PTY 截图与字符网格 dump |

---

## 4. 验证

### 4.1 集成测试（新增 14 条，全部读渲染帧）

`cargo test -p pi-tui --test autocomplete_pointer` → **14 passed**：

| 断言 | 测试 |
| --- | --- |
| press 某行 → `autocomplete_selected` = 该行候选；press 不是编辑 | `a_press_on_a_dropdown_row_highlights_that_candidate` |
| release 同格 → 补全完成，草稿逐字为 `/render `，下拉关闭，光标在词尾 | `a_click_on_a_dropdown_row_completes_that_candidate` |
| release 落在别的行 → 不完成、下拉仍开、高亮不动 | `a_release_on_another_row_does_not_complete` |
| press 下拉行后的 drag/release 不产生聊天选择、不误完成 | `a_press_on_a_dropdown_row_never_starts_a_chat_selection` |
| `(n/m)` 指示行不是候选：press/release 都不动作 | `the_indicator_row_owns_no_candidate` |
| 列表内滚轮改选中，聊天视口顶行不动 | `the_wheel_inside_the_list_steers_the_highlight_and_not_the_log` |
| 滚到端点（clamp）仍吞掉，不滚聊天；反向那格照常步进 | `a_wheel_notch_that_cannot_move_the_highlight_still_does_not_scroll_the_log` |
| 列表外的滚轮照旧滚聊天 | `the_wheel_outside_the_list_still_scrolls_the_log` |
| **回归**：列表外的点击仍走聊天选择，下拉不受影响 | `a_click_outside_the_dropdown_still_selects_transcript_text` |
| composer 点击（LUM-1327）在下拉打开时仍生效，且下拉跟随新光标位置 | `the_composer_click_still_works_with_the_dropdown_open` |
| 光标移出 token 时下拉按新位置刷新并关闭（上游点击分支结尾的 `updateAutocomplete()`） | `placing_the_caret_outside_the_token_closes_the_dropdown` |
| `Ctrl+R` 打开后下拉已被取消，指针不再落到它上面 | `reverse_search_takes_the_pointer_from_the_dropdown` |
| 帧没画过下拉时，点击不会被吞（落到聊天选择） | `a_click_cannot_consume_a_dropdown_that_was_never_painted` |
| 帧之后草稿长高：整张列表跟着上移，命中跟着走 | `a_draft_that_grew_after_the_frame_carries_the_list_with_it` |

### 4.2 真 PTY A/B（不是设计意图）

```bash
# 修后（本分支，最终合并树 9a9fff9db：含 LUM-1328 / LUM-1330）
python3 pi-rust/scripts/pty_capture.py --bin target/debug/pi \
  --steps pi-rust/scripts/pty_scenarios/lum1333-autocomplete-pointer.json \
  --out pi-rust/docs/screenshots/lum1333-autocomplete-pointer.png \
  --text-out pi-rust/docs/screenshots/lum1333-autocomplete-pointer.png.txt
# => assertions: 20 checks over 7 panels — 20 PASS, 0 FAIL

# 修前：git checkout 4f9817cd1 -- pi-rust/crates && cargo build -p pi-coding-agent --bin pi
python3 pi-rust/scripts/pty_capture.py --bin target/debug/pi \
  --steps pi-rust/scripts/pty_scenarios/lum1333-autocomplete-pointer-baseline.json \
  --out pi-rust/docs/screenshots/lum1333-autocomplete-pointer-baseline.png \
  --text-out pi-rust/docs/screenshots/lum1333-autocomplete-pointer-baseline.png.txt
# => assertions: 18 checks over 7 panels — 18 PASS, 0 FAIL（断言的是缺陷本身）
```

真终端 100×20、真二进制、pyte 仿真；下拉占 12..17 行（五个候选 + `(k/21)` 指示行），
composer 在第 18 行，状态栏第 19 行。`(k/21)` 是「谁被选中」的机器可读证据，`❯` 是视觉证据。

### 4.3 门禁（Rust 1.85.0，`--locked`）

```
cargo fmt --all -- --check                                     干净
cargo clippy --workspace --all-targets --locked -- -D warnings  No issues found（0 warning / 0 error）
cargo test --workspace --locked --no-fail-fast                  2695 passed / 0 failed / 2 ignored（172 suites）
cargo test -p pi-tui -p pi-coding-agent --locked --no-fail-fast  1813 passed / 0 failed（86 suites，合并前连跑 4 次 1810/85 一致）
```

- 本机必须 `. pi-rust/scripts/toolchain.sh` 钉 1.85.0（`~/.local/bin/cargo` 是没默认 toolchain 的 shim）。
- **共享卷依旧紧张**：本轮多次撞上 `No space left on device`（同机另有 4 条 run 在构建同一个 workspace，
  其中一条的 `target/` 一度到 20G）。为此全量门禁用
  `CARGO_INCREMENTAL=0 CARGO_PROFILE_{DEV,TEST}_DEBUG=0` 跑（关调试信息不改测试语义），
  并在可用空间不足时重试。
- **一次未复现的 flake**：早期有一次 `-p pi-tui -p pi-coding-agent` 的全量运行出现单个 suite `6 passed / 3 failed`，
  随后连续 4 次同样范围的全量运行都是 `1810 passed / 0 failed`；当时捕获的输出里没有该 suite 名，不能断言与本轮改动有关。

---

## 5. 已知限制与后续

1. **同一次终端读里到达的手势**：帧矩形只描述上一次 paint。若「按下 `/` 的那几个字节」和「滚轮/点击」
   在**同一次读**里到达（合成事件，例如把 `"/<WheelDown:x,y>"` 一次写进 PTY），那时下拉还没被画过，
   矩形是零，于是这一格不被下拉吞掉、落到聊天视口。这是**有意**保留的闸门（否则点击会「选中」
   读者没见过的候选），与 LUM-1327 第 4 条同类；真实键盘/鼠标交互里两条事件不会同批到达，下一帧即修正。
2. composer 内**拖选 / 选中即复制**仍未做（LUM-1332 的范围）；粘贴折叠与 bracketed paste 已由
   LUM-1328 落地，本分支已合并其 tip 后重测。
3. 真 PTY 只覆盖 100×20 一种几何；多行草稿、`(n/m)` 指示行、窗口边界等由集成测试覆盖（40×14）。
4. 滚轮是**步进选中**而不是「滚到指针所在那一行」（上游 `SelectList.handleMouse` 就是 `selectedIndex ± 1`）；
   本 port 没有独立于选中的窗口偏移量，所以这也是等价的「滚列表」。
5. 下拉的命中区域是**整行**（composer 宽度），与上游一致（列表行是整行可点）。

---

## 6. 复现命令

```bash
cd pi-rust && . scripts/toolchain.sh

# 1. 本轮新增的集成测试
cargo test -p pi-tui --test autocomplete_pointer            # 14 passed
# 受影响的既有面
cargo test -p pi-tui --test autocomplete --test mouse_scroll --test mouse_selection \
  --test settings_list --test input_surface --test composer_click

# 2. 真 PTY A/B（见 §4.2）
cargo build -p pi-coding-agent --bin pi

# 3. 全量门禁
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked --no-fail-fast
```
