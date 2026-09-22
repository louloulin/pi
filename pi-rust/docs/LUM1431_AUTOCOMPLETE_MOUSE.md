# LUM-1431 — autocomplete 下拉框鼠标点选（chatinput 补完）+ LUM-1422 产物抢救合并

> scope: `pi-rust/`（基线 `origin/feature/pi.rs` = `c093196be` = LUM-1426）
> branch: `work/LUM-1431`（→ `feature/pi.rs`）
> 时间锚：LUM-1426 之后。本轮领的是 LUM-1426 §8 明写的「下一轮第一顺位」——**autocomplete
> 下拉框的鼠标点选**，并把并行 worker 线 `work/LUM-1422`（未合并、随时会随工作树消失）抢救合并。
> 交付物：`crates/pi-tui/src/{app,editor,prompt}.rs`、`crates/pi-tui/tests/autocomplete_mouse.rs`（新，7 条）、
> `crates/pi-tui/tests/lum1431_autocomplete_frames.rs`（新，3 帧）、三张真帧截图、本文。

## 1. TL;DR

- **chatinput 缺口（本轮修掉）**：composer 的下拉框画出来了（LUM-1236），键盘也能选（Up/Down/Tab），
  但**它不属于任何鼠标目标**。上游把下拉框矩形判在编辑器自己的 `handleMouse` 里、且判在屏幕级文本选择
  *之前*（`packages/tui/src/components/editor.ts:618-638` → `SelectList.handleMouse`，
  `packages/tui/src/components/select-list.ts:109-140`）；Rust 端口点下去会**落到列表背后的 transcript
  选择路径**上——点候选等于替背后的聊天记录起了一段选区。现在：press 高亮该行并夺走指针，click（同格
  释放）应用该候选并关掉列表，异格释放丢弃，`(n/m)` 计数行不可点，列表关闭后那一块格子立刻还给 transcript。
- **LUM-1422 抢救**：`work/LUM-1422` 是并行 worker 留下的**未合并**产物（3 个提交，`search.rs` 的列宽预算
  + 4 个测试文件 + 截图）。它与已合入的 LUM-1426 在 `app.rs` 的选择模型上正面冲突。本轮按「谁在
  `feature/pi.rs` 上、谁有测试证据」取舍：**保留 LUM-1426 的字符下标模型**（它已在树上、带
  `pointer_columns.rs` 14 条），**保留 LUM-1422 的 `search.rs` 列宽预算**（LUM-1426 没覆盖），
  **丢弃 LUM-1422 在 `app.rs` 上的选择/高亮改动**，并把它的 4 个测试文件全部留下作回归门 —— 15 条在
  合并后的树上**全绿**，等于对两套模型的可观察行为做了一次交叉验证（见 §3）。
- **实测**：`pi-tui` **998 passed / 0 failed**（基线 973 / 0 → **+25** = LUM-1422 的 15 + 本轮 10：
  `autocomplete_mouse.rs` 7 条 + 帧 dump 3 条）；
  `--workspace` **2620 passed / 39 failed / 2 ignored**，失败**全部落在 `pi-coding-agent` /
  `pi-extensions` 的已知 Windows 环境类**（真 `bash`、绝对路径断言、`/tmp`、node fs、trust），
  条数与 LUM-1426 基线逐字相同（39），`pi-tui` 零失败。
- **截图**：`docs/screenshots/lum1431-autocomplete-press-76x16.png`（press 后高亮 `❯ model`、草稿仍是 `/`）、
  `-click-`（click 后 `/model `、列表消失）、`-counter-`（六命令三行窗口的 `(1/6)`），均带可 grep 的 `.txt`。
- **Rust↔TS 差距复测**：规模 **88.8%**（135,950 / 153,106）、测试 **49.5%**（2,628 / 5,309）、
  TUI 模块 **35/42 = 83.3%**、`app.*` 接线 **43/44 = 97.7%**、composer 鼠标面 **2/2 = 100%**、
  加权 **83.6%**（公式与权重见 `RUST_TS_PARITY_METRICS.md` §0.10）。

## 2. 缺口的具体形态（可复算）

上游的指针路由（`packages/tui/src/tui-alt-screen.ts:876-935`）：

1. `dispatchMouseToLayout(event)` 先把 press 交给命中的**组件**（编辑器就是组件之一）；
2. 组件返回结果时，屏幕级做 `clearTextSelection()` 并记 `mousePressTarget`；
3. 释放时若指针没离开该格，合成一个 `click` 事件再交给**同一个** target；
4. 只有没有任何组件认领这个 press，才走 `handleSelectionMouseEvent`（transcript 文本选择）。

编辑器的 `handleMouse` 里，**列表矩形排在编辑器主体之前**（`editor.ts:618-638`）：

```ts
const autocompleteStartRow = this.renderedVisibleLineCount + 2;
if (this.autocompleteState && this.autocompleteList &&
    event.y >= autocompleteStartRow &&
    event.y < autocompleteStartRow + this.renderedAutocompleteHeight) {
    const result = this.autocompleteList.handleMouse?.({ ...event, x: event.x - paddingX,
        y: event.y - autocompleteStartRow, width: contentWidth, height: this.renderedAutocompleteHeight });
    return result ? { ...result, focus: true } : undefined;
}
```

`SelectList.handleMouse`（`select-list.ts:109-140`）定的语义：press 只改 `selectedIndex`（并记
`mousePressedIndex`），click 才 `onSelect(item)`；`itemIndex = startIndex + event.y`，越界返回 `undefined`。

Rust 端口在 LUM-1431 之前：`App::step_mouse_gesture` 依次是 modal → 搜索栏 → jump-to-latest pill →
「cut above」提示 → 滚动条 → composer → transcript 选择，**列表不在其中**。列表又恰恰画在
transcript 的格子上（`App::paint_autocomplete` 借用编辑器上方若干行），所以点候选 = 点背后那条消息。

## 3. LUM-1422：抢在与工作树一起被删之前合并

`work/LUM-1422`（`88a0552bb` / `0edbc16b8` / `c954a9fa9`）基于 `1e2977238`，三条提交：

| 提交 | 内容 | 本轮处置 | 理由 |
|---|---|---|---|
| `88a0552bb` | `search.rs::render_search_bar` 的查询/计数/占位符预算改按终端列（`width::columns` / `truncate_columns`）；`search.rs` 单测新增 | ✅ **保留** | LUM-1426 只把搜索 **corpus** 的 span 改成列，没碰搜索栏自身的宽度预算；40 列栏里 15 个汉字会排出 55 列的行，渲染器把计数与右边框裁掉 |
| `0edbc16b8` + `c954a9fa9` | `app.rs` 的 `word_segments` / `line_selection` / `apply_selection_highlight` / `selection_text` 改成「选择坐标 = 显示列」 | ❌ **丢弃**（保留其测试） | 与 LUM-1426 的「选择坐标 = 字符下标，边界处用 `width::{char_index_at_column, columns_before}` 换算」正面冲突。两种模型都能自洽，但 `feature/pi.rs` 上是 LUM-1426 那一套，且带 `pointer_columns.rs`（14 条）与 composer 点击；把两者混在一棵树上会产生**双重换算**（`columns_before(列)` 把列当字符下标再乘一次） |
| 同上的 4 个测试文件（`lum1422_{search_columns,search_frames,selection_columns,wide_glyph_frames}.rs`，15 条）+ 4 张截图 | 行为级断言（读渲染出的 cell / 复制出的字符串） | ✅ **全部保留** | 它们不依赖被丢弃的内部模型，只看可观察结果 —— 于是成了**跨模型回归门**：15 条在 LUM-1426 的字符下标模型上**全绿**，独立证明两套模型的可观察行为一致 |

手工过冲突的过程（`git merge` 后仅 `app.rs` 一处冲突，`search.rs` 自动合并）：

- `apply_search_highlight`：LUM-1426 的 `SearchSegment` 已是**终端列**（改在 `search.rs::build_corpus` 的
  grapheme 路径上），LUM-1422 还假设它是字符下标并在绘制处换算 —— 取 **HEAD**（LUM-1426）版本；
- `word_segments` / `line_selection` / `selection_text` / `apply_selection_highlight`：全部取 **HEAD**，
  并删掉 LUM-1422 带进来的 `prefix_chars` / 局部 `char_index_at_column`（`width.rs` 里已有同名公用件）；
- 合并后 `cargo test -p pi-tui --test lum1422_*` → **15 passed / 0 failed**，`app.rs` 的 diff 相对 HEAD
  只剩 `search.rs` 的列宽预算（`git checkout HEAD -- app.rs`）。

**诚实说明**：这一条不是「LUM-1422 错了」也不是「LUM-1426 错了」，而是同一缺陷被两条并发线各修一次。
本轮的价值是把它**收敛成一条**、不丢任何测试证据，并明确记下取舍依据。同样的并发重复在
`FEATURE_PI_RS_STATUS.md` 里已被记录过七轮（LUM-1213/1215/1217/1219/1220/1221/1222）。

## 4. 改了什么（文件:行）

| # | 位置 | 改动 |
|---|---|---|
| 1 | `pi-tui/src/editor.rs:1825` | 新增 `autocomplete_window()` —— 返回渲染器**同一套** `select_list_visible_range` 算出的 `(start, end)` 候选窗口，指针行号据此反查候选 |
| 2 | `pi-tui/src/editor.rs:1842` | 新增 `set_autocomplete_selected(index)` —— press 半场：只移动高亮、不应用（上游 `SelectList.handleMouse` 的 `press` 分支），越界夹到最后一项 |
| 3 | `pi-tui/src/prompt.rs:187` | 新增 `Prompt::accept_autocomplete()` —— click 半场，把 `Editor::accept_autocomplete` 的 `EditorAction` 映射成 `PromptAction`，与既有 `place_cursor` 同形 |
| 4 | `pi-tui/src/app.rs:1194-1212` | 新增四个几何/窗口字段：`autocomplete_origin` / `autocomplete_size` / `autocomplete_first_item` / `autocomplete_item_rows`（`AtomicU16`/`AtomicUsize`，因为绘制是 `&self`） |
| 5 | `pi-tui/src/app.rs:1201` | 新增 `autocomplete_mouse_press: Option<(u16, u16, usize)>` —— 按下格 + 按下时的候选下标，等价上游 `mousePressTarget` + `mousePressedIndex` |
| 6 | `pi-tui/src/app.rs:4605` | `record_autocomplete_area(rect, first_item, item_rows)` —— 记录/清除命中框；`None` 必须真正清零，否则关闭后的陈旧矩形会继续吞点击 |
| 7 | `pi-tui/src/app.rs:4631` | `autocomplete_item_at(x, y)` —— 屏幕格 → 候选下标；计数行/列表外返回 `None` |
| 8 | `pi-tui/src/app.rs:4661` | `autocomplete_mouse_gesture(&gesture)` —— press 清掉 transcript 选区并记待定格、click 用**按下时**的下标应用（窗口会因选中而重新居中）、异格释放只丢弃、拖拽/移动吞掉 |
| 9 | `pi-tui/src/app.rs:4014` | 在 `step_mouse_gesture` 里把该分支插在**滚动条与 transcript 选择之前**（对齐上游「列表矩形先于屏幕级选择」） |
| 10 | `pi-tui/src/app.rs:5585` | `paint_autocomplete` 每帧记录几何：候选窗口 + 短视口丢弃的行数（`dropped`）都要并入 `first_item`，否则矮终端上点到的会是错的候选；三个提前 return 分支与「自定义编辑器组件」分支都改为显式清除 |
| 11 | `pi-tui/tests/autocomplete_mouse.rs`（新） | **7 条**：press 高亮不应用 / click 应用并关列表 / 拖拽不选背后文本 / 异格释放不应用（且后续 click 仍可用）/ 计数行不可点 / 窗口重新居中后仍应用**按下时**的候选 / 列表关闭后同一格归还 transcript |
| 12 | `pi-tui/tests/lum1431_autocomplete_frames.rs`（新） | 3 个真帧 dump（press / counter / click），供 `scripts/frame_to_png.py` 出图 |
| 13 | `pi-tui/src/search.rs`（来自 LUM-1422） | `render_search_bar` 的候选按钮 / 结果计数 / 查询 / 占位符全部按终端列预算（`columns` / `truncate_columns`） |

**未碰**：`pi-coding-agent`（下拉框的 provider 安装已在 LUM-1236 落地，鼠标路径完全在 `pi-tui` 内）、
`pi-agent-core`、`pi-session`、扩展事件面、CLI flag 面；无新依赖。

## 5. 门禁与实测（合并后的树，本机 Windows / cargo 1.97.1 / `--offline`）

```console
$ CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 cargo fmt --all -- --check
  干净
$ … cargo clippy --offline -p pi-tui --all-targets -- -D warnings
  Finished `dev` profile …（0 warning）
$ … cargo test --offline -p pi-tui
  998 passed / 0 failed            （基线 origin/feature/pi.rs tip c093196be = 973 / 0 → +25）
$ … cargo test --offline --locked --workspace --no-fail-fast
  178 个 test-result 行；2620 passed / 39 failed / 2 ignored
$ … 失败分布（逐 target 统计）
  pi-coding-agent(8 lib) + cli_extensions(10) + cli_tools(4) + reload_config(1)
  + system_prompt_resources(1) + tools(4) + tools_navigation(3) + tools_render(2)
  + pi-extensions(node_builtins 3 + sdk_modules 1 + web_globals 1) = 39
  pi-tui：0
```

- 39 条失败**逐条落在已知的 Windows 环境类**（无 `/tmp`、无真 `bash`、绝对路径断言、node fs、trust），
  与 LUM-1426 记录的基线条数（39）相同，且**没有一个 target 属于本轮改动的 crate 之外**——
  本轮只动了 `pi-tui`，而失败集合全部在 `pi-coding-agent` / `pi-extensions`。
- `app_action_coverage.py --check-consumed` → `43 entries; measured wired: 43 / in sync`（未动键位面）。

### 5.1 真帧截图（无 PTY，frame-buffer 通道）

| 文件 | 面板 | 画面 |
|---|---|---|
| `docs/screenshots/lum1431-autocomplete-press-76x16.png`(+`.txt`) | press | 列表停在 transcript 上方，`❯ model` 已高亮，草稿仍是 `/` —— 证明 press 只移动高亮 |
| `docs/screenshots/lum1431-autocomplete-click-76x16.png`(+`.txt`) | click | 草稿变成 `/model `、列表消失、被借用的那几行回到 transcript |
| `docs/screenshots/lum1431-autocomplete-counter-76x16.png`(+`.txt`) | counter | 六命令三行窗口，尾行 `(1/6)` 画出但不可点 |

**诚实分级**：本机 Windows 无 `pty`，这三张是 `App::render_to_buffer` 的**冻结帧**，证明几何与高亮，
**不证明按键时序**；交互时序的证据是 `autocomplete_mouse.rs` 的 7 条行为断言（它们驱动真实的
`step_mouse_gesture`）。

## 6. Rust↔TS 差距复测（本轮亲自跑，不是引用）

| 量 | 本轮实测 | LUM-1426 | 命令 |
|---|---|---|---|
| 纯代码规模（src↔src） | **88.8%**（135,950 / 153,106） | 88.7%（135,733） | `python pi-rust/scripts/measure_loc.py`（从仓库根跑） |
| 测试规模 | **49.5%**（2,628 / 5,309） | 49.0%（2,603） | `#\[test\]｜#\[tokio::test\]` 与 `*.test.ts` 的 `it(｜test(` 重数 |
| TUI 模块面 | **35 / 42 = 83.3%** | 35/42 | `pi-tui/src`（36 − `lib.rs`）↔ `packages/tui/src` + `components` |
| `app.*` 接线 | **43 / 44 = 97.7%**（silent 1 = `app.tree.editLabel`） | 同 | `python pi-rust/scripts/app_action_coverage.py --check-consumed` |
| **composer 鼠标面** | **2 / 2 = 100%** | 1/2 = 50% | 点击定位光标（LUM-1426）+ 下拉框点选（本轮） |
| 指针映射面 | **3 / 3 = 100%** | 100% | 鼠标选择 / 双击选词 / 搜索高亮 |
| slash 内置命令 | 18 / 23 = 78.3% | 同 | 本轮未动 |
| 扩展生命周期事件 | 21/36 声明 = 58.3%；20/36 生产构造点 = 55.6% | 同 | 本轮未动 |

**加权完成度**（权重表见 `RUST_TS_PARITY_METRICS.md` §4.1）：

```text
5×1.00 + 13×0.90 + 8×1.00 + 6×0.70 + 14×0.905 + 8×0.90 + 7×0.783 + 7×0.70
  + 8×0.95 + 7×0.556 + 9×0.85 + 5×0.495 + 3×0.95 = 83.6%
```

只有测试轴从 0.490 走到 0.495（+0.03pt），总分仍落在 **83.6%**——本轮修的是「TUI 交互轴」里
一个此前被算作 50% 的分项，它不单独进公式（公式里的轴 7 是 7×0.783 的 slash 面）；把 0.783→0.80
才是「TUI 交互轴的可见变化」，本轮**不重估**，只更新可复算的分项。

## 7. 剩余缺口（按性价比，供下一轮起手）

| 顺位 | 项 | 范围 | 预计影响 | 证据 |
|---|---|---|---|---|
| 1 | **下拉框滚轮**（缺坐标） | `InputEvent::Mouse { up, alt }` 不带 x/y，上游 `SelectList.handleMouse` 的 wheel 分支需要坐标才能把滚轮映射到列表 | 鼠标滚轮在列表上滚动候选（当前只有键盘 Up/Down） | 上游 `select-list.ts:111-121`；Rust `input.rs:195-244` 明确写了「wheel 走 `InputEvent::Mouse`、无坐标」 |
| 2 | **`#` 触发符空转** | `DEFAULT_AUTOCOMPLETE_TRIGGER_CHARACTERS = ['@', '#']`，但 provider 只处理 `/` 与 `@` | 输入 `#` 会尝试打开下拉框却没有候选（技能/工具引用面缺失） | `editor.rs:240` vs `autocomplete.rs::get_suggestions` 的分支 |
| 3 | **扩展生命周期事件补 15 个** | `pi-protocol/src/events.rs` + `pi-agent-core` observer + `pi-extensions` 别名表 | 兼容 pi 插件生态的头号阻塞；单轴 +3.1pt | LUM-1426 §8 第 2 条，本轮复核仍为 20/36 |
| 4 | **CLI flag 面** | `pi-coding-agent/src/cli` | 与上游启动参数对齐 | LUM-1426 §8 第 3 条 |

两条已知遗留（不单独开切片，记录备查）：`message.rs:63` 的 `tool_fold_hint` 仍硬编码 `Ctrl+O`
（用户改键后提示不跟着变，LUM-1222 §5 已记）；`app.editor.external` 仍缺（Stage 59）。

## 8. 「最多 3 个任务」的处置 = 零派发

issue 的「最多开启 3 个任务同时运行」是**上限**而非配额。本轮把 LUM-1422 的抢救合并、chatinput 的下拉框
点选、门禁、截图、审计放在**同一个 run 内**完成：这三件事全部落在 `pi-tui` 的同一块代码面
（`app.rs` 的指针路由 + 列表几何）。再派并发写者只会制造**本轮刚刚亲手清理掉的那种重复**
（§3 的两套模型之争），所以本轮不派子任务，把 §7 的四条留成下一轮的起手清单。

## 9. 范围之外

- **未碰** `pi-coding-agent` / `pi-ai` / `pi-agent-core` / `pi-session` / `pi-server` / `pi-client`。
- **未碰** 扩展事件轴、slash 命令表、CLI flag 面、键位表（`app.*` 接线率不变）。
- **未碰** CI / Docker；截图沿用 LUM-1412 建立的 frame-buffer 通道。
- **未碰** 上游 TS 代码（`packages/**`）——只读来做对齐取证。
