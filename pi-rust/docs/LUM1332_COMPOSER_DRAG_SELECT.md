# LUM-1332 — composer 拖选 + 选中即复制（codex / 上游 parity）：真 PTY A/B、渲染层高亮修复、实测数字

> 范围：`pi-rust/crates/pi-tui/src/{app,visual_text}.rs`、
> `pi-rust/crates/pi-tui/tests/{composer_drag_select,composer_click}.rs`、
> `pi-rust/scripts/pty_capture.py`、
> `pi-rust/scripts/pty_scenarios/lum1332-composer-drag-select{,-baseline}.json`、
> `pi-rust/docs/screenshots/lum1332-composer-drag-select{,-baseline}.png(.txt)`
> 基线：`origin/feature/pi.rs` = `3e7af2761`（LUM-1327 的 tip）
> 参照实现：codex `codex-rs/tui/src/bottom_pane/chat_composer/mouse.rs`
> （`prepare_mouse` → textarea `handle_mouse` → `copy_selection`）、
> 上游 pi `packages/tui/src/components/editor.ts:634-638`（故意不处理 press/drag/release）
> 与 `packages/tui/src/tui-alt-screen.ts:1310-1462`（屏幕级选择：拖选、release 同日格才合成 click、`copyOnSelect`）

一句话结论：**拖拽原本被 composer 吞掉且什么都没做**——LUM-1327 只接通了点击定位，
`step_composer_mouse_gesture` 的 drag / release 分支一律 `Some(StepOutcome::Idle)`；
本轮把拖拽变成 composer 内的字符选择（REVERSED 高亮，与聊天选择同一套语义），
release 走同一条剪贴板通道（`App::pending_clipboard` → OSC 52），
拖出 composer 时**夹紧到 composer 边界**并在文档写明；LUM-1327 的两条语义（单击定位、点击不是编辑）逐条复测保留。
真 PTY A/B：同一组手势打给修前 / 修后二进制，修前 19/19 断言“什么都没发生”，
修后 30/30 断言“高亮 + 复制 + 光标跟随”（7 面板、真终端、pyte）；
全量门禁在 rustc 1.85.0 下 `2648 passed / 0 failed / 2 ignored`（169 suites）。

---

## 1. 结论速览

| 口径 | 本轮实测 | 上一轮（LUM-1327 文，同口径） | 说明 |
| --- | --- | --- | --- |
| `cargo test --workspace --locked --no-fail-fast` | **2648 passed / 0 failed / 2 ignored**（169 suites） | 2632 passed / 0 failed / 2 ignored（168 suites） | 本轮 +16 条测试、+1 suite，全部真跑完 |
| 新增测试 | **14 条帧级集成**（`composer_drag_select.rs`）+ **2 条 `VisualLayout` 单测** | +10（8 集成 + 2 单测） | 计数一致：2632 + 16 = 2648 |
| 真 PTY 断言 | **30 checks / 7 panels，30 PASS**（修后） | 13 checks / 6 panels | 本轮的断言含高亮区间与 OSC 52 内容 |
| 真 PTY A/B（修前） | **19 checks / 7 panels，19 PASS**（断言“没发生”） | 13 checks / 6 panels | 修前基线把缺陷正面断言下来 |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | **No issues found** | 通过 | 见 §6 的 profile 说明 |
| `cargo fmt --all -- --check` | **clean** | 通过 | — |
| 渲染层高亮（截图） | **修前不可见 → 修后可见**（实测像素，见 §2.3） | 未覆盖 | 本轮修掉 `pty_capture.py` 的 reverse 交换 bug |
| composer 指针行为（4 项） | **2/4**：单击定位 ✓（LUM-1327）、拖选+复制 ✓（本轮）；双击选词 ✗、三击选行 ✗ | 1/4 | 见 §5 |
| 改动规模 | `app.rs` +511/-62、`visual_text.rs` +53/-7、`composer_click.rs` +16/-5、`pty_capture.py` +131/-14、新增 `composer_drag_select.rs` 562 行、scenario 185+153 行 | — | `git diff --numstat` 实测 |

**不要把“composer 指针行为 2/4”读成“composer 完成度 50%”**：它只数本轮对照 codex 明列的 4 个指针手势，
不含 composer 的其它面（多行、分页、chips、历史、autocomplete 等，分别由 LUM-1282/1317/1224/1319/1305 覆盖）。

---

## 2. 真实审计：修复前的缺陷（真 PTY A/B，不是设计意图）

### 2.1 缺陷本身

LUM-1327 把 composer 的指针路径接通到「点击定位」，但拖拽被明确吞掉：

```rust
// 修前（3e7af2761）app.rs —— drag / release 都只是“吞掉”
MouseGestureKind::Release(MouseButton::Left)
    if self.composer_press.take().is_some() =>
{
    Some(StepOutcome::Idle)
}
MouseGestureKind::Drag(MouseButton::Left) | MouseGestureKind::Move
    if self.composer_press.is_some() =>
{
    Some(StepOutcome::Idle)
}
```

两个参照实现都不这样：

* 上游 pi-ts 把 press/drag/release **交给渲染层的屏幕级选择**
  （`editor.ts:634-638` 直接 `return undefined`），屏幕级选择负责拖选、release 落在同一格时**合成一个 click**
  再派回编辑器定位光标，`copyOnSelect ?? true` 时 release 即复制
  （`tui-alt-screen.ts:1310-1348,1443-1462`）；
* codex 把同一套语义留在 composer 内（`chat_composer/mouse.rs` → textarea `handle_mouse` → `copy_selection`）。

本 port 没有屏幕级选择，且聊天选择的矩形**明确排除 composer 行**
（`App::selection_point` 在视口外返回 `None`），所以拖拽在两层之间掉空了：既不选字，也不复制。

### 2.2 证据：同一组手势打给两个二进制

```bash
# 修前：从 origin/feature/pi.rs (3e7af2761) 构建的二进制
python3 pi-rust/scripts/pty_capture.py --bin <pre-fix pi> \
  --steps pi-rust/scripts/pty_scenarios/lum1332-composer-drag-select-baseline.json \
  --out pi-rust/docs/screenshots/lum1332-composer-drag-select-baseline.png

# 修后：本轮的二进制
python3 pi-rust/scripts/pty_capture.py --bin pi-rust/target/debug/pi \
  --steps pi-rust/scripts/pty_scenarios/lum1332-composer-drag-select.json \
  --out pi-rust/docs/screenshots/lum1332-composer-drag-select.png
```

（100×20 真终端、真二进制、pyte 仿真；`<MPress:x,y> <MDrag:x,y> <MRelease:x,y>` 是 LUM-1327 加进 harness 的 SGR 编码 token。）

| 手势（草稿 `hello world`，composer 行 18，body 列 N = 格 2+N） | 修前（19/19 断言“没发生”） | 修后（30/30 断言） |
| --- | --- | --- |
| 从 `llo` 的 `l`（格 4,18）拖到 `world` 的 `o`（格 9,18） | `> he▍llo world`（只有 press 的定位；**拖拽没有移动光标**，无高亮，无复制） | `> hello w▍orld` + `# reverse y=18 x=4-10 'llo w▍o'`（高亮正好覆盖拖动区间，光标跟到拖拽终点） |
| 松开（格 9,18） | 无 `# clipboard` | `# clipboard: 'llo wo'`（OSC 52 base64 解码后的载荷） |
| 单击（格 4,18） | `> he▍llo world`（LUM-1327，本来就是修好的） | `> he▍llo world`，无高亮、无复制（语义不变） |
| 两行草稿，`first line` 的 `i`（3,17）拖到 `second line` 的 `o`（5,18） | `> f▍irst line` + `  second line`（拖拽无效果） | `  sec▍ond line` + `y=17 x=3-11 'irst line'`、`y=18 x=2-6 'sec▍o'` |
| 上一步松开 | 无 `# clipboard` | `# clipboard: 'irst line\nseco'`（跨行选择带换行） |
| 从（8,17）拖到（0,0）：指针离开 composer | 无高亮（聊天区也没有） | `> ▍first line` + `y=17 x=3-9 'first l'`（夹紧到 composer 左边界），**聊天区 0 条 `# reverse`** |
| `MRelease:0,19` + 点状态栏（20,19） | 无复制 | `# clipboard: 'line\ns'`（release 夹紧到 composer 底行后复制），状态栏点击不产生复制 |

截图：`docs/screenshots/lum1332-composer-drag-select.png`（修后，7 面板）、
`...-baseline.png`（修前，同一组手势）。文本 dump（`.png.txt`）里每个面板带
`frame <hash>`（冻结网格哈希）、`px <hash>`（渲染像素哈希）以及断言行，可逐条核对。

修后 7 个面板的网格 / 像素哈希（`.png.txt` 实测）：

```
frame  2129654e8fb4  px 52d55b1e021b   panel 1 拖选
frame  d78e972c3962  px 52d55b1e021b   panel 2 松开并复制（像素与 1 相同：复制不是编辑）
frame  eadf8e5a7b54  px ca735382c67a   panel 3 单击（无选择）
frame  7829139f219c  px a55cb16987d8   panel 4 跨行拖选
frame  6687859a6ed2  px a55cb16987d8   panel 5 松开并复制
frame  5e58214aae3a  px 5c7148682668   panel 6 拖出 composer（夹紧）
frame  bc32ac484365  px 517a7d7b4cb8   panel 7 release 夹紧后复制 + 状态栏点击
```

### 2.3 审计中发现的第二个缺陷：截图根本画不出高亮

真 PTY 的第一轮跑出 `# reverse y=18 x=4-10 'llo w▍o'`（网格证据成立），
但**同一张 PNG 里那 7 格没有任何视觉差异**：`pty_capture.py` 的 reverse 交换写成了

```python
if getattr(cell, "reverse", False):
    fg, bg = (bg if bg != DEFAULT_BG else DEFAULT_FG), (fg if fg != DEFAULT_FG else DEFAULT_BG)
```

在 `fg/bg` 都是默认色时（选择高亮正是“默认前景 + 默认背景”的格子）两边都回落成默认值，
交换成了恒等变换。实测（修前渲染器，同一条修后帧）：panel 1 row 18 的 x=2..15 采样点背景**全部**是
`(10,10,12)`；改成真正的交换 `fg, bg = bg, fg` 后，同一行出现 `(212,212,212)` 的连续区间，
且区间与 `# reverse` 行逐格一致：

| 面板 | 网格证据（`# reverse`） | PNG 实测反色区间 |
| --- | --- | --- |
| 1 拖选 / 2 松开 | `y=18 x=4-10` | row 18: `(4,10)` |
| 3 单击 | 无 | 无 |
| 4 跨行 5 松开 | `y=17 x=3-11`、`y=18 x=2-6` | row 17: `(3,11)`，row 18: `(2,6)` |
| 6 夹紧 | `y=17 x=3-9` | row 17: `(3,9)` |
| 7 release 夹紧 | `y=17 x=8-11`、`y=18 x=2-3` | row 17: `(8,11)`，row 18: `(2,3)` |

复核用的采样脚本（面板高 20 行、caption 26px、cell 18×36、面板间距 10）：

```python
from PIL import Image
im = Image.open("pi-rust/docs/screenshots/lum1332-composer-drag-select.png").convert("RGB")
top = lambda i: i * 748 + i * 10 + 27          # 面板 i 的网格首行
bg = lambda x, y, i=0: im.getpixel((1 + x * 18 + 15, top(i) + y * 36 + 31))
print([x for x in range(100) if bg(x, 18)[0] > 100])   # -> [4, 5, 6, 7, 8, 9, 10]
```

顺带说明：这条分支从 harness 落地（`809648412`）起就没被任何文档的截图依赖过——
历史轮次的高亮证据都走文本 dump，所以这个 bug 一直没暴露；本轮的高亮必须“看得见”，才被 A/B 抓出来。

---

## 3. 实现

### 3.1 手势状态机（`App::step_composer_mouse_gesture`）

| 事件 | 行为 | 参照 |
| --- | --- | --- |
| 左键 press，格子映射到草稿字符 | 记录 `composer_press`、`composer_dragged = false`，**重锚**选择（旧选择清空），按 `click_offset` 定位光标 | 上游 press 分支重锚 + `Editor.handleMouse` 的 click 定位 |
| 左键 press，格子被 composer 吸收（短草稿的 padding 行、`Ctrl+R` 搜索行） | 吞掉，不建选择、不动光标 | 上游 `{ handled: true }` |
| 左键 press，**在 composer 之外** | 清掉 composer 选择并把事件放行给聊天选择 | 上游每次 press 重锚唯一的选择 |
| drag / move（press 尚未 release） | 指针格**夹紧**进 composer → 选择 focus 前移；focus 变化才算“拖过”，光标跟到 focus | codex textarea 拖拽移动光标；上游 `selectionDragged` |
| 左键 release（press 属于 composer，且拖过） | 用 release 自己的格子收尾（终端会合并 motion 事件），复制选择 | 上游 release 先 `updateSelectionFocus` 再 `copySelectionToClipboard` |
| 左键 release（未拖过、同一格） | **click**：清空选择、不复制（光标已在 press 时落位） | 上游 `isClick` → 派发 click 并 `clearTextSelection()` |
| 其它按钮 / 无 press 的 move | 不认领（返回 `None`，落到聊天选择或 hover） | 上游右键仅 Windows 粘贴 |

夹紧规则（本 port 的明确选择，写进代码注释与本文件）：**指针离开 composer 时夹紧到 composer 矩形**
（y 夹到 composer 的首/末行，x 夹到首/末列），而不是“放弃选择”或“夹紧到整屏”。
理由：上游夹紧到整屏是因为它的选择层覆盖整屏；本 port 夹紧到整屏等于让 composer 手势去选聊天区文字，
与 LUM-1327 的“composer 手势不得延伸聊天选择”冲突。

### 3.2 选择区间：`ComposerPoint` / `ComposerSelection`

两端都是**草稿字符偏移**（`Prompt::text()`，chip 已展开成 `[Image #N]` 标签），不是屏幕格：

```rust
struct ComposerPoint { offset: usize, on_char: bool }
struct ComposerSelection { anchor: ComposerPoint, focus: ComposerPoint }

fn bounds(&self) -> Option<(usize, usize)> {   // start..end，end 独占
    if self.anchor.offset == self.focus.offset { return None; }        // 空选择
    let (first, last) = /* 按 offset 读序 */;
    let end = if last.on_char { last.offset + 1 } else { last.offset };
    Some((first.offset, end))
}
```

`on_char` 对应上游 `getGraphemeCellRange` 的“指针下面到底有没有字符”：
拖动终点落在字符上 → 该字符**属于**选择（`+1`）；落在行尾之后（wrap 行会 snap 回最后一个字符，
硬换行行则给出该行边界 / `\n` 的偏移）→ 只取边界。这样正拖、反拖、拖到行尾、拖到两行草稿的
padding 行都是同一套规则，`VisualLayout` 新增 `click_char()` 专门回答这个问题
（`click_offset()` 保持 LUM-1327 的光标语义不变，并由 `click_char().unwrap_or(cursor_at)` 组合而成）。

### 3.3 高亮（REVERSED，只覆盖 composer 行）

`App::apply_composer_selection_highlight` 与聊天选择的 `apply_selection_highlight` 用同一套
`Modifier::REVERSED`，但格子由**本帧的 `VisualLayout`** 反推：composer 行 = gutter（`> ` / 缩进 / `↑`计数）
+ 草稿字符，而**光标 `▍` 是插入的**（`build_prompt_row`），所以每个格对应哪个字符取决于 marker 在不在它左边。
实现遍历 layout 的逐字符 `source` 偏移，跳过 marker 格：
光标严格落在选择区间内部时 marker 也一起反色（否则高亮中间会被 marker 打出一个洞）。

### 3.4 复制通道

release 时若 `AppConfig::copy_on_select`（`/settings` 里的 `copyOnSelect`，默认 true）为真，
`App::composer_selection_text()` 的文本进入 `App::pending_clipboard` —— **与聊天选择同一条通道**，
driver 在渲染循环里取走并写 OSC 52（`interactive.rs` 的 `take_clipboard_request` + `clipboard::osc52_sequence`）。
App 仍然不碰终端剪贴板。文本是草稿的**逐字切片**，不做行尾 trim：
聊天选择 trim 是因为它读的是空格填充的屏幕行，而 composer 选择的区间本来就量在草稿上。

### 3.5 选择何时失效

两端都是草稿偏移，草稿一变偏移就指向别的文字，所以这些入口显式清空：
`step_prompt`（按键后草稿或光标真的变了才清）、`paste_text`、`paste_image`、`set_editor_text`、
`clear_composer`、`restore_pending_to_editor`、`follow_up_from_editor`、`close_custom` 的草稿恢复、
`Ctrl+C` 的清稿路径，以及 §3.1 的“press 在 composer 之外”。

---

## 4. 测试

### 4.1 帧级集成测试（`crates/pi-tui/tests/composer_drag_select.rs`，14 条）

全部读**渲染帧**（`render_to_buffer` 的 `Buffer`）与 App 的公开 API，不碰内部状态：

1. 拖选：高亮正好覆盖拖动区间（反色格逐格比对）、`composer_selection_text` 等于草稿片段、松开后发出剪贴板请求；
2. 反拖（从右往左）同样成立；
3. 单击：光标落位、**不复制**、无聊天选择、草稿逐字不变、`undo_len()` 不变；
4. 拖拽跨行：两行各自高亮，区间文本带 `\n`；
5. 拖出 composer：夹紧到左边界，聊天选择仍为 `None`；
6. 拖到草稿下方的 padding 行：夹紧到草稿末尾；
7. `copy_on_select = false`：保留高亮但不排队剪贴板；
8. 拖回起点：空选择、不复制（对应上游 `anchor !== focus`）；
9. 拖选后敲键：高亮消失；
10. 草稿里混入图片 chip：选中文本与高亮格**逐字一致**（display 偏移口径不串位）；
11. 光标在选区左侧时高亮整体右移一格（marker 插入规则）；
12. composer 拖拽永不产生聊天选择（LUM-1327 语义）；
13. release 落在与最后一次 drag 不同的格子：区间与光标都按 release 自己的格子收尾；
14. press 在 composer 之外：清掉 composer 高亮。

### 4.2 单元测试（`visual_text.rs`，+2 条）

`click_char()` 的两条边界：wrap 行尾之后 snap 回最后一个字符（`Some`）、硬换行行尾之后只给边界（`None`）、
空行给 `None`；`click_offset()` 的既有两条测试原样保留（光标语义未变）。

### 4.3 `composer_click.rs` 的更新

LUM-1327 的 `a_click_on_the_composer_never_starts_a_selection` 用“拖拽返回 `Idle`”表达
“composer 没有自己的拖选”——本轮该前提已不成立，测试改名为
`a_drag_on_the_composer_never_starts_a_chat_log_selection` 并断言真正要保的东西：
拖拽返回 `Redraw`（composer 选择）、`selection_text() == None`、`composer_selection_text() == Some("world")`。
其余 7 条断言未动。

---

## 5. 与参照实现的差距（本轮之后还差什么）

| 能力 | 上游 pi-ts | codex | 本 port 本轮后 |
| --- | --- | --- | --- |
| 单击定位 | `Editor.handleMouse` click 分支 | `prepare_mouse` → textarea | ✓（LUM-1327） |
| 拖选 + 选中即复制 | 屏幕级选择 | `copy_selection` | ✓（本轮） |
| 双击选词 / 三击选行 | `getWordSelection` / `getLineSelection` | textarea 双击选词 | ✗ 未做：composer 里没有 click-count 状态（聊天选择有，见 `ClickTarget`） |
| 右键复制 | 右键仅 Windows 粘贴 | 右键 `copy_selection` | ✗ 未做 |
| 拖拽时的边界自动滚动 | `autoScrollSelection`（50ms interval） | 无（composer 内不滚动） | ✗ 未做：composer 窗口跟随光标，拖到 padding 即夹紧 |
| 选择跨出 composer | 屏幕级（含聊天区） | 只在 composer 内 | ✓ 夹紧到 composer（与上游不同且已写明） |

---

## 6. 门禁与复现

```bash
cd pi-rust
. scripts/toolchain.sh
cargo fmt --all -- --check                       # clean
cargo clippy --workspace --all-targets --locked -- -D warnings   # No issues found
cargo test --workspace --locked --no-fail-fast   # 2648 passed / 0 failed / 2 ignored (169 suites)

# 真 PTY（修后）
python3 scripts/pty_capture.py --bin target/debug/pi \
  --steps scripts/pty_scenarios/lum1332-composer-drag-select.json \
  --out docs/screenshots/lum1332-composer-drag-select.png        # 30 checks, 30 PASS
```

**门禁的 profile 说明（必须写明）**：本机 overlay 盘被多个并行 run 的 target 目录占满，
默认 profile（`debug = 2` + incremental）在 `cargo test --workspace` 阶段触发过两次
`No space left on device`。最终这次门禁跑在
`CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0` 下，
命令与 flag 与约定门禁完全一致，只有这两项构建缓存 / 调试信息开关不同：
它不影响任何测试的判定，但**产物没有 DWARF、也没有增量缓存**，与 `cargo test` 的默认产物不同；
需要默认产物的读者请在有足够磁盘（约 20 GB）的机器上原样重跑。

修前基线二进制的复现：

```bash
git stash push -u -- pi-rust/crates          # 回到 3e7af2761 的源码
cargo build -p pi-coding-agent --bin pi
cp target/debug/pi /tmp/pi-pre && git stash pop
python3 scripts/pty_capture.py --bin /tmp/pi-pre \
  --steps scripts/pty_scenarios/lum1332-composer-drag-select-baseline.json \
  --out docs/screenshots/lum1332-composer-drag-select-baseline.png   # 19 checks, 19 PASS
```

---

## 7. 已知限制与后续

1. **双击选词 / 三击选行未做**：composer 里没有 click-count 状态。补的时候建议复用聊天选择的
   `ClickTarget` / `word_segments` / `SelectionGranularity`，因为它们已经处理了 UAX #29 分段与
   `Click preserves the draft` 的语义（codex 的 `copy_selection` 也只复制草稿）。
2. **右键复制未做**：本 port 的右键目前不认领（上游 pi-ts 也只在 Windows 上给粘贴）。
3. **拖到 padding 行不自动滚动**：composer 窗口跟随光标（LUM-1317），拖到窗口外只是夹紧；
   上游的屏幕级选择有 50ms 自动滚动。若要做，应挂到 `App::composer_scroll` 上而不是聊天区的
   `advance_selection_autoscroll`。
4. **夹紧语义与上游不同**（已在本文件与代码注释写明）：本 port 夹紧到 composer 边界，
   上游夹紧到整屏。改动它需要先有屏幕级选择层。
5. **两处高亮可同时存在**：点击聊天区会清掉 composer 高亮，但点 composer **不会**清掉聊天高亮
   （LUM-1327 的既有语义：任何一侧的选择都不是另一侧拖动的一部分）。
6. **harness 的 reverse 交换修复会影响其它轮次的截图**：修后任何含反色格的旧场景 PNG 像素会变
   （文本 dump 与断言不变）。历史文档里记录的 `px` 哈希因此会失效，属预期。
7. 本轮**未**重算整 TUI 的跨轮进度轴（代码规模、`app.*` 接线、slash 命令交集、`/settings` 行数）：
   它们与 composer 拖选无关，重算只会把上一轮的数字抄一遍；需要的读者请看
   `LUM1327_COMPOSER_POINTER.md` §1 与 `RUST_TS_PARITY_METRICS.md`。
