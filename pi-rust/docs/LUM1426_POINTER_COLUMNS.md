# LUM-1426 — 指针/高亮按列对齐 + chatinput 鼠标定位光标

> scope: `pi-rust/`（基线 `origin/feature/pi.rs` = `1e2977238`）
> branch: `agent/winpi/050be6769ef2-impl`（→ `feature/pi.rs`）
> 时间锚：LUM-1418 之后；本轮领的是 LUM-1418 §6 明写的「下一轮第一顺位」——指针/高亮的
> 「屏幕列 ↔ 字符下标」换算，并把 issue 点名的 **chatinput 缺口**（composer 无鼠标目标）补上。
> 交付物：`crates/pi-tui/src/width.rs`（新函数）、`crates/pi-tui/tests/pointer_columns.rs`（新，14 条）、
> `crates/pi-tui/src/{app,search,editor,prompt}.rs`、`scripts/frame_to_png.py`（SGR 高亮）、
> 两张真帧截图、本文。

## 1. TL;DR

- **根因（上一轮留下的同族缺陷）**：LUM-1418 把**排版**口径从「字符数」改成「终端列」，但**指针与高亮**
  的换算还是字符口径。一条中文行是 *N* 个字符、*2N* 个格子，于是：
  1. 点击/拖拽落到**相邻**汉字上（`> 你好世界` 里 `好` 占第 4–5 格，而字符 4 是 `世`、字符 5 是 `界`）；
     双击选词也照错位选；
  2. 选择高亮按「一个字符一格」画，只覆盖每个宽字的左半格，越往右越偏；
  3. 搜索命中的 span 表按 grapheme 计数，中文行的高亮整体左移约一半、压在错误的字上。
- **本轮**：把两个方向各收敛成一个纯函数 —— `width::char_index_at_column`（格子→字形，上游
  `getGraphemeCellRange`）与 `width::columns_before`（字符区间→格子区间）—— 接到三个面上；
  搜索 corpus 的列直接改成终端列。
- **chatinput**：上游 pi-ts `editor.ts:620-666`、codex、Martty 都支持**点击 composer 定位光标**，
  Rust 端口此前**完全没有**这个目标（composer 行不在任何 mouse hit test 里）。本轮补上，
  含上游的 `isLastSegment` 修正（点在折行行尾不会跳到下一行行首）。
- **实测**：`pi-tui` **973 passed / 0 failed**（基线 953 / 0 → **+20**）；
  `--workspace` **2595 passed / 39 failed**，**失败集合与基线逐条相同（0 新增）**。
- **截图**：`docs/screenshots/lum1426-pointer-columns.png`（中文选择 + 中文搜索高亮，100×30）、
  `docs/screenshots/lum1426-composer-caret.png`（点击中文草稿定位光标，72×12），均带可 grep 的 `.txt`。
- **Rust↔TS 差距复测**：规模 **88.7%**、测试 **49.0%**、`app.*` 接线 **97.7%**、
  扩展事件 **55.6%（生产构造点）**、加权完成度 **83.6%**（主口径）/ **84.6%**（与上轮同口径）。
- 仍然存在的同族缺陷：**autocomplete 下拉框的鼠标点选**（上游先判下拉框矩形；Rust 侧点了会落到
  transcript 选择路径上），列为下一轮第一顺位，见 §8。

## 2. 三个面的具体错位（可复算）

以 `> 你好世界` 为例（`>`=第 0 格、空格第 1 格）：

| 字符下标 | 0 | 1 | 2 | 3 | 4 | 5 |
|---|---|---|---|---|---|---|
| 字符 | `>` | ` ` | 你 | 好 | 世 | 界 |
| 占据格子 | 0 | 1 | 2–3 | 4–5 | 6–7 | 8–9 |

1. **鼠标选择/双击**：`selection_point_clamped`（`app.rs`）把 `x - origin_x` 直接当字符下标。
   点在 `好` 的第 2 格（**格 5**）→ 旧代码解析成**字符 5 = 界**，双击选中的是 `界` 而不是 `好`。
2. **选择高亮**：`apply_selection_highlight` 用 `for col in from..to` 按**字符下标**写 buffer 格。
   选中 `好世`（字符 3..5）只点亮格 3、4 —— 而真正该亮的是 4..8。中文选择看起来"少了半截 + 偏左"。
3. **搜索高亮**：`search.rs` 的 `build_corpus` 用 `grapheme.chars().count()` 累加列，非 ASCII 行走
   grapheme 路径 → `你好` 记成 `0..2`（实际 `0..4`）。`apply_search_highlight` 又按格子写 buffer，
   于是高亮比真实命中窄一半、位置左移，中文行几乎对不上。

三处同一个原因：**屏幕按格子寻址，文本按字符切分，中间没有换算**。

## 3. 改了什么（文件:行）

| # | 位置 | 改动 |
|---|---|---|
| 1 | `crates/pi-tui/src/width.rs` | 新增 `columns_before(text, char_index)`（字符下标 → 起始格子）与 `char_index_at_column(text, col)`（格子 → **整字形**下标；宽字形两格都指向该字形，零宽字符不占格，越界回落字符数）。带 3 条单测（含混合行往返） |
| 2 | `crates/pi-tui/src/app.rs::selection_point_clamped` | 指针格子先经 `char_index_at_column` 吸附到字形，再进入既有的字符口径选择模型（按下 / 拖拽 / 选词 / 取文本全部复用） |
| 3 | `crates/pi-tui/src/app.rs::apply_selection_highlight` | 字符区间 `from..to` 经 `columns_before` 转成格子区间后写 buffer，宽字形两格一起点亮 |
| 4 | `crates/pi-tui/src/app.rs::apply_search_highlight` | 长度与 clamp 从 `chars().count()` 改为 `width::columns()`，与搜索 span 的列口径一致 |
| 5 | `crates/pi-tui/src/search.rs::build_corpus` | 非 ASCII 路径按 `width::columns(grapheme)` 累加列（含空白 grapheme）；`SearchSegment` / `SourceSpan` 文档改成「终端列」 |
| 6 | `crates/pi-tui/src/search.rs` 单测 | 更新 `non_ascii_lines_use_the_grapheme_path`（`你好` 的 `end_col` 2 → **4**，口径变了），新增 `non_ascii_columns_count_cells_not_characters`（`a你 世界` 起点 3 → **4**；emoji 命中 `3..5`） |
| 7 | `crates/pi-tui/src/app.rs` 模块文档 | 把「列是字符不是显示格」改成「选择模型用字符、边界用格子」，并说明两个方向的换算 |
| 8 | `crates/pi-tui/src/editor.rs` | 新增 `place_display_cursor` / `is_at_display_offset`：按显示文本字符偏移放置光标，清掉 sticky 列与历史浏览；chip 标签内的偏移夹出标签外（2 条单测） |
| 9 | `crates/pi-tui/src/prompt.rs` | 新增 `Prompt::place_cursor(display)` → `PromptAction` |
| 10 | `crates/pi-tui/src/app.rs`（composer 指针） | 记录 composer 矩形（`paint_prompt` / 自定义 editor 组件时清零）；`composer_contains` / `composer_cursor_offset` / `place_prompt_cursor` / `prompt_mouse_gesture`：左键按下记位、同格释放才算 click、映射走 `VisualLayout` + 字形吸附、含上游 `isLastSegment` 修正；`step_mouse_gesture` 在 scrollbar 之后、transcript 选择之前插入该分支 |
| 11 | `crates/pi-tui/tests/pointer_columns.rs`（新） | **14 条**：宽字形右半格点击 / 拖拽整字 / 双击选词 / 高亮覆盖全字 / 混合行 / 搜索 span 列 / 搜索高亮格子 / composer 点击三点（字形、标签列、折行行）/ 点击与拖拽不误触 transcript 选择 / 两张截图帧 |
| 12 | `scripts/frame_to_png.py` | dump 支持 SGR（`7` 反显、`1` 加粗、`4` 下划线）：按 run 绘制反显底/下划线，便于把**高亮**画进 PNG；无 SGR 的行仍走原路径 |

## 4. chatinput：composer 点击定位光标

### 4.1 三个参考实现都有，Rust 没有

| 参考 | 位置 | 行为 |
|---|---|---|
| 上游 pi-ts | `packages/tui/src/components/editor.ts:620-666` | 点击 → `visualLineIndex` → 字形吸附 → `setCursorCol`，并带 `isLastSegment` 修正 |
| codex | `codex-rs/tui` 的输入行 | 支持鼠标点击落点定位（TUI 鼠标捕获后自行换算） |
| Martty | `src/input/`（ratatui 单体 crate） | 同上 |

Rust 端口此前：`step_mouse_gesture` 只依次处理 modal → 搜索栏 → jump-to-latest pill → 「cut above」提示 →
滚动条 → transcript 文本选择。**composer 行不属于任何 hit test 目标**，点它什么都不会发生
（`selection_point` 只映射 message viewport，落在 prompt 行的手势被忽略）。

### 4.2 修法（对齐上游语义）

1. 帧渲染时记录 composer 矩形（`record_composer_area`），指针在两次渲染之间到达，靠这份几何命中。
2. **按下 + 同格释放**才算 click（上游的 `isClick` 闸门）；从 composer 起手的拖拽被吞掉，
   不会穿透到底下的 transcript 选区。
3. 行映射与渲染同源：`VisualLayout::new(prompt.text(), composer_body_width)` + `composer_scroll`
   得到点击命中的**草稿行**，列先经 `char_index_at_column` 吸附字形，再走 `VisualLayout::cursor_at`
   得到草稿字符偏移。
4. 上游的 `isLastSegment` 修正：点在某折行行的**行尾之外**时停在那一行，而不是跳到下一行行首
   （测试 `a_click_on_a_wrapped_draft_row_lands_on_that_row` 钉住这条）。

### 4.3 仍未做（诚实清单）

- **autocomplete 下拉框的鼠标点选**：上游 `editor.ts:620` 第一段先判下拉框矩形并把事件交给
  `SelectList.handleMouse`（选中 + 确认）。Rust 侧下拉框是画在 message 行上的 overlay，
  **没有 hit test**：点它会落到 transcript 选择路径，选到下拉框底下的文字。这是本轮引入之外的
  **既有缺陷**，也是下一轮第一顺位（§8）。
- **滚轮在 composer 上**：仍归 transcript（与上游一致），本轮未改。

## 5. 门禁（本机实测，`--offline`）

| 命令 | 结果 |
|---|---|
| `cargo fmt --all -- --check` | 干净 |
| `cargo clippy --offline -p pi-tui --all-targets` | 0 warning / 0 error |
| `cargo check --offline -p pi-tui --all-targets` | OK |
| `cargo test --offline -p pi-tui` | **973 passed / 0 failed**，57 个 target（基线 **953 / 0** → +20） |
| `cargo test --offline --workspace --no-fail-fast` | passed **2595** / failed **39** / 2 ignored；172 个 target |
| `python pi-rust/scripts/app_action_coverage.py --check-consumed` | `wired 43/44`，`in sync` |
| `python pi-rust/scripts/extension_event_coverage.py` | 21/36 声明、20/36 生产构造点 |

### 5.1 基线对照与「无新增回归」的证明

```text
baseline (origin/feature/pi.rs = 1e2977238)   pi-tui 953 passed / 0 failed
after    (本分支)                              pi-tui 973 passed / 0 failed   (+20)

workspace 失败项名字集合（本轮前后各 38 条具名 + 1 条 doc-test 编译）：
  comm -3 <baseline FAILED 名单> <after FAILED 名单>  →  空集
```

39 条失败**全部早于本轮**、全部是这台 Windows 机器的环境问题：`paths/trust/mod_ignore/js_loader/
resource_loader/export/session_file` 的绝对路径与错误文案断言、`tools_render`/`print_mode`/`ls`/`find`
的真 `bash` 工具（本机无 `/tmp`）、`pi-extensions` 的 node fs / SDK 模块、
`pi-client` 的 doc-test 编译。逐条与 LUM-1418 §4.3 记录的同族一致。

## 6. 截图与证据

| 文件 | 内容 | 生成方式 |
|---|---|---|
| `docs/screenshots/lum1426-pointer-columns.png` / `.txt` | 100×30 真 `App` 帧：中文会话 + 中文选择（`好世` 反显）+ 中文搜索命中（`世界` 反显加粗）+ 搜索栏 + 状态栏 | `cargo test -p pi-tui --test pointer_columns frame_dump_for_the_screenshot -- --nocapture` → `scripts/frame_to_png.py` |
| `docs/screenshots/lum1426-composer-caret.png` / `.txt` | 72×12 帧：点击中文草稿 `你好世界，按列宽定位光标` 里 `按` 的第 2 格后，`▍` 恰好落在 `按` 之前 | 同上（`frame_dump_for_the_composer_screenshot`） |

`.txt` 是高亮带 SGR 的 cell dump，可直接断言：
`|> 你[好][世][界]，这是一段中文文本|`（`[ ]` 表示反显格，见 §2 的格子表）。

**诚实说明**：本机（Windows）没有 `pty` / `termios`，`scripts/pty_capture.py` **跑不了**，
这两张是 **frame-buffer 冻结帧**（LUM-1412 建立的通道），能证明**排版与高亮落在哪一格**，
不能证明按键/鼠标的**时序**；交互时序由 `pi-tui` 的 App 级测试（含本轮 14 条）覆盖，
Linux runner 上可用同一 dump 端到端复现。

## 7. Rust↔TS 差距复测（真实百分比）

测量命令（可在本仓库复跑）：

```bash
# 规模（python 求和，不要用 xargs+wc：文件多时 xargs 会分批，tail 只看到最后一批）
python pi-rust/scripts/measure_loc.py   # rust src 135,733 / ts src 153,106
# 测试标记
python -c "…re.findall(r'#\[test\]|#\[tokio::test\]')"   # 2,603
# 接线 / 事件 / 命令 面
python pi-rust/scripts/app_action_coverage.py     # wired 43/44
python pi-rust/scripts/extension_event_coverage.py # 21/36 声明、20/36 生产
```

| 口径 | 本轮实测 | 上轮公开值 | 说明 |
|---|---|---|---|
| 纯代码规模（src↔src） | **88.7%**（135,733 / 153,106） | 87.6%（134,449 / 153,106） | 同口径；本轮 +1,284 行 src |
| 测试规模 | **49.0%**（2,603 / 5,309） | 47.0%（2,555 / 5,439） | TS 分母按 `*.test.ts` 的 `it(`/`test(` 重测 |
| TUI 模块面 | **35 / 42 = 83.3%**（不含 `lib.rs`；TS = `tui/src` 24 + `components` 18） | 33/42 = 78.6% | 本轮新增 `width.rs` 等 |
| `app.*` 接线 | **43 / 44 = 97.7%**，silent 1（`app.tree.editLabel`） | 43/44（LUM-1305） | 脚本 `--check-consumed` 自校验通过 |
| slash 内置命令 | **18 / 23 = 78.3%**（字面 17/23；`exit`≡`quit`） | 78% | 缺 `import share changelog logout` + `quit`(有 `exit`) |
| 扩展生命周期事件 | **21/36 声明 = 58.3%**；**20/36 生产构造点 = 55.6%** | 57% | 上一轮用 20/35，本轮按上游 36 个事件名重算分母 |
| **指针映射面（新量）** | **3 / 3 = 100%**（鼠标选择 / 双击选词 / 搜索高亮） | 0/3 = 0%（LUM-1418 §0.8 新量） | 本轮关闭 |
| **composer 鼠标面（新量）** | **1 / 2 = 50%** | 0/2 = 0% | 点击定位 ✅；下拉框点选 ❌（§4.3） |

### 加权完成度（权重同 §4.1，公式公开）

| # | 轴 | 权重 | 本轮 | 加权 |
|---|---|---|---|---|
| 1 | 可构建 / 可测 / 可运行 | 5% | 1.00 | 5.00 |
| 2 | 核心 agent 循环 | 13% | 0.90 | 11.70 |
| 3 | provider API family | 8% | 1.00 | 8.00 |
| 4 | provider / 模型目录广度 | 6% | 0.70 | 4.20 |
| 5 | TUI 交互面（模块率与接线率均值） | 14% | (0.833+0.977)/2 = **0.905** | 12.67 |
| 6 | TUI 视觉保真 | 8% | 0.90 | 7.20 |
| 7 | slash 命令面 | 7% | 0.783 | 5.48 |
| 8 | CLI / 模式 / 子命令面 | 7% | 0.70 | 4.90 |
| 9 | 扩展宿主能力 | 8% | 0.95 | 7.60 |
| 10 | 扩展生命周期事件 | 7% | 0.556 | 3.89 |
| 11 | 会话 / 存储 / 导入导出 | 9% | 0.85 | 7.65 |
| 12 | 测试与门禁强度 | 5% | 0.490 | 2.45 |
| 13 | 子包完整度 | 3% | 0.95 | 2.85 |
| | **合计** | 100% | | **83.6%** |

**口径说明（重要）**：上轮公开的 81.4% 用的是「轴 5 = 接线率单独取值」的算式，且逐项相加实为 **82.1%**
（我按它给的输入复算：5×1.00+13×0.90+8×1.00+6×0.70+14×0.795+8×0.91+7×0.78+7×0.70+8×0.95+7×0.57+
9×0.85+5×0.46+3×0.95 = 82.06）。为了让读者能逐项追溯，本轮同时给出两个口径：

- **主口径（模块率与接线率均值）= 83.6%**；
- **与上轮同口径（轴 5 只取接线率）= 84.6%**（上轮同样输入复算 82.1% → +2.5pt）。

差额可逐项对账：轴 5 接线 0.795→0.977（+2.55pt）、测试轴 0.46→0.490（+0.15pt）、
事件轴 0.57→0.556（−0.10pt，纯口径修正），其余轴未动。

## 8. 剩余缺口与「最多 3 个任务」的处置

真实缺口（按性价比）：

| 顺位 | 项 | 范围 | 预计影响 | 风险 |
|---|---|---|---|---|
| 1 | **autocomplete 下拉框鼠标点选** | `pi-tui` 的 `paint_autocomplete` 几何 + `Selector`/`SelectList` 行寻址（上游 `editor.ts:620-638`） | 点选候选；顺手堵住「点下拉框选到背后文字」 | 中（单模块 + 一组鼠标测试） |
| 2 | **扩展生命周期事件补 15 个**（`before_agent_start` / `context` / `tool_execution_*` / `session_before_*` …） | `pi-protocol/src/events.rs` + `pi-agent-core` observer + `pi-extensions` 别名表 | **兼容 pi 插件生态**的头号阻塞；单轴 +3.1pt | 高（跨 3 crate，事件面已规范） |
| 3 | **CLI flag 面**（`--theme/--thinking/--tools/--provider/--api-key/--offline` 等） | `pi-coding-agent/src/cli` | 与上游启动参数对齐；单轴 +0.4pt×若干 | 低 |

**本轮处置 = 零派发**。理由与 LUM-1418/1305 一致，且更明确：issue 的「最多 3 个任务同时运行」是**上限**
而非配额；本轮交付（指针 + composer 鼠标 + 文档 + 截图 + 门禁）在**同一个 run 内**完成并全量过门，
而上面三条顺位里第 1 条与第 2 条都要改 `pi-tui` / `pi-agent-core`，并发派发会把「哪一层坏了」拆散。
下一轮从第 1 条起手最省事（它是本轮 §4.3 留下的直接续集），第 2 条需要独立一轮。

## 9. 范围之外

- **未碰** `pi-coding-agent` 的任何 `.rs`（本轮只动 `pi-tui`、一个脚本、文档与截图）。
- **未碰**扩展事件轴、slash 命令、CLI flag 面（§8 的第 2、3 条）。
- **未碰** CI / Docker；本机 Windows 无 `pty`，截图沿用 frame-buffer 通道。
- **未碰** `pi-ai` / `pi-agent-core` / `pi-session` 等非 TUI crate。
