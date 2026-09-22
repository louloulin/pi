# LUM-1305 — 补全下拉框改走 `SelectList` 行布局（清掉编辑器侧第二套渲染）

一句话结论：`pi-tui` 里「命令 / 文件补全下拉框」原本由 `Editor` 自己拼字符串渲染（`label` + 两个空格 +
`description`，44 列以上才出现描述，`App` 再从行首字符 `❯` 反推样式）；本轮把它换成**上游
`SelectList` 的那一套行布局**——`selector.rs` 现在导出 `SelectorLayout` / `SelectListRow` /
`select_list_visible_range` / `select_list_row_spans` 四个共享件，模态选择器（`/model`、`/session`、
`/settings`）与 composer 下拉框都调用同一份代码，编辑器侧的第二套渲染被删除。
（收尾时已 rebase 到 `feature/pi.rs` 的 `4ad27d5f8`，全部门禁与 A/B 截图都是在合并后的树上重跑的。）

净变化（`pi-rust/crates/pi-tui/src`）：**+340 / −156 行**（新增部分主要是共享件的文档、trait 与
`SelectorLayout` 的 API 面）。真正被删除的是编辑器侧那套渲染——`autocomplete_render_lines` 的内联实现、
`autocomplete_visible_range`、只被它使用的 `fn truncate_display`、以及 `App` 里从文本反推样式的分支——
`Selector` 自己的重复实现也退化成 1–3 行的委托。

行为上的净变化：下拉框的描述列从「固定 2 个空格」变成「按 `SLASH_COMMAND_SELECT_LIST_LAYOUT`（12–32 列）
对齐的列」，选中行/描述列的配色从「按文本猜」变成 theme span。

---

## 1. 问题与真实审计

issue 的要求是「补全下拉框改走 SelectList 描述列对齐（清掉编辑器侧第二套渲染）」。先核对这件事
在本轮起点是否真的成立（起点 `cdc033e21` = LUM-1308 tip；本轮收尾时 `feature/pi.rs` 已被
LUM-1274 / LUM-1312 推到 `4ad27d5f8`，本轮已 rebase 到它并重跑全部门禁，见 §4）：

### 1.1 改动前确实存在两套渲染

| 位置 | 内容 | 问题 |
|---|---|---|
| `crates/pi-tui/src/editor.rs:1117`（旧） | `Editor::autocomplete_render_lines` | 内联实现：`marker + label`，`width > 44` 才追加 `"  " + description`，整行 `truncate_display` 截断 |
| `crates/pi-tui/src/editor.rs:1148`（旧） | `Editor::autocomplete_visible_range` | 又一份 `getVisibleRange`（与 `Selector::visible_range` 重复） |
| `crates/pi-tui/src/editor.rs:1674`（旧） | `fn truncate_display` | 只被上面那个内联渲染使用 |
| `crates/pi-tui/src/app.rs:5051`（旧） | `App::paint_autocomplete` | 取纯文本行，按 `row.starts_with('❯')` 给**整行**上色：选中行 `accent`、其余行 `muted` |
| `crates/pi-tui/src/selector.rs:575/589/812`（旧） | `Selector::primary_column_width` / `primary_column_bounds` / `render_row_spans` | 另一份完整实现：列宽 clamp、描述列对齐、宽度感知截断、`accent`+`selectedBg` 选中行 |

也就是说：**同一份上游组件（`packages/tui/src/components/select-list.ts`）在 Rust 侧被实现了两遍**，
第二遍（编辑器侧）在行为上已经落后于第一遍——它没有列对齐、没有 `selectedBg` 背景、描述列出现在
44 列而不是上游的 40 列，而且**两处的「描述何时出现」阈值不同**，用户在同一终端宽度下看到的
`/model` 弹窗与 `/` 下拉框排版不一致。

### 1.2 上游事实（这才是要对齐的东西）

上游的 composer 下拉框**不是自绘的**，它就是 `SelectList`：

```ts
// packages/tui/src/components/editor.ts:2224-2248
private createAutocompleteList(prefix, items): SelectList {
    const layout = prefix.startsWith("/") ? SLASH_COMMAND_SELECT_LIST_LAYOUT : undefined;
    const list = new SelectList(items, this.autocompleteMaxVisible, this.theme.selectList, layout);
    ...
}
// packages/tui/src/components/editor.ts:605-614
if (this.autocompleteState && this.autocompleteList) {
    const autocompleteResult = this.autocompleteList.render(contentWidth);
    ...
}
```

| 上游常量 / 行为 | 值 / 位置 |
|---|---|
| `DEFAULT_PRIMARY_COLUMN_WIDTH` | 32（`select-list.ts:5`） |
| `PRIMARY_COLUMN_GAP` | 2（`select-list.ts:6`） |
| `MIN_DESCRIPTION_WIDTH` | 10（`select-list.ts:7`） |
| 描述列出现条件 | `width > 40`（`select-list.ts:170`） |
| `SLASH_COMMAND_SELECT_LIST_LAYOUT` | `{ min: 12, max: 32 }`（`editor.ts:245-248`） |
| 列宽 | 最宽 label + gap，clamp 到 `[min, max]`（`getPrimaryColumnWidth`） |
| 选中行 | 整行 `selectedText`（`accent` over `selectedBg`，`select-list.ts:205,216`） |
| 非选中描述列 | `theme.description(spacing + truncatedDesc)`，即 gap 与描述一起 `muted`（`select-list.ts:208`） |

`Selector` 侧（LUM-1097/LUM-1112 已落地）本来就是按这张表实现的，所以本轮的活儿是**把编辑器侧对齐过来**，
而不是重写一遍 `SelectList`。

---

## 2. 实现

### 2.1 共享件（`crates/pi-tui/src/selector.rs`）

| 上游 | Rust（本轮新增/改造） | 行号 |
|---|---|---|
| `SelectItem` | `pub trait SelectListRow { value/label/description }` | `selector.rs:181` |
| `SelectListLayoutOptions` | `SelectorLayout`（`new` / `slash_command` / `bounds`） | `selector.rs:107`、`:138`、`:147` |
| `getPrimaryColumnWidth` | `SelectorLayout::primary_column_width(rows)` | `selector.rs:159` |
| `getVisibleRange` | `pub fn select_list_visible_range(len, selected, max_visible)` | `selector.rs:213` |
| `renderItem` | `pub fn select_list_row_spans(row, selected, width, column)` | `selector.rs:241` |

`Selector` 现在只是这三个共享件的调用方（`selector.rs:764` / `:776` / `:784`），
它自己的 `visible_range` / `primary_column_width` / `render_row_spans` 变成 1–3 行的委托；
`primary_column_bounds` 整个删除（改用 `SelectorLayout::bounds`）。

`AutocompleteItem` 实现 `SelectListRow`（`autocomplete.rs:115`），所以两份候选类型走同一条渲染路径。

### 2.2 下拉框（`crates/pi-tui/src/editor.rs`、`app.rs`）

* `Editor::autocomplete_render_styled_lines(width) -> Vec<StyledLine>`（`editor.rs:1139`）：
  列宽取自 `autocomplete_layout().primary_column_width(items)`，窗口取自 `select_list_visible_range`，
  每行取自 `select_list_row_spans`，滚动提示 `(n/total)` 与上游一致用 `muted`。
* `Editor::autocomplete_layout()`（`editor.rs:1192`）：`prefix.starts_with('/')` → 上游 slash 布局
  `[12, 32]`，否则默认 `[32, 32]` —— 逐字对应 `editor.ts:2228` 的三元表达式。
* `Editor::autocomplete_render_lines`（`editor.rs:1176`）保留为纯文本薄壳（`plain_text`），
  旧签名不变，原有调用/测试不受影响。
* `App::paint_autocomplete`（`app.rs:5044`）改吃 `Vec<StyledLine>`，不再从文本反推样式；
  仍按「借用转录行、紧贴 prompt、长列表保留尾部窗口」的位置策略，并且**先把借来的那行 `reset()`**，
  避免候选行短于它覆盖的转录行时右侧露出旧输出（与 selector / settings 浮层同一条规则）。
* 删除：`Editor::autocomplete_visible_range`、`fn truncate_display`、App 里的
  `selected_style`/`plain_style` 反推与手工补空格。

### 2.3 行为差异（A/B，同一行候选，真 PTY）

两边的图都由同一个 scenario（`scripts/pty_scenarios/lum1305-autocomplete.json`）驱动，两个二进制建在
**同一棵树**上：`lum1305-autocomplete-before.png` = `4ad27d5f8`（= 本轮 rebase 基，LUM-1312 tip），
`lum1305-autocomplete.png` = 本轮提交。

| 场景 | 改动前 | 改动后（= 上游） |
|---|---|---|
| `/` 菜单 | `❯ help  Show this help text`（描述紧跟 2 空格） | `❯ help           Show this help text`（主列 = 最宽命令 + 2） |
| `/mo` | `❯ model  <provider/model> — …` | `❯ model          <provider/model> — …`（列不因列表变窄而移动） |
| `@` 文件菜单 | `❯ src/  src` | `❯ src/                            src`（默认 32 列主列，描述在第 34 列） |
| 选中行配色 | `accent` 前景（无背景） | `accent` 前景 + `selectedBg` 背景，整行包裹 |
| 非选中行 | 整行 `muted`（标签也灰） | 标签默认前景，只有描述列 `muted` |

斜杠菜单的列宽**跟着命令表走**（上游 `getPrimaryColumnWidth`），这是旧渲染器从来没有的行为：
本轮开头 `/` 菜单的主列是 12（最宽命令 6–10 列），本轮的同伴任务 LUM-1274 落地 `/scoped-models`
（13 列）后同一个二进制就变成 15（描述从第 14 列移到第 17 列）——同一个 scenario 的断言从
“硬编码 14 列”改成“拒绝旧的两空格形式 + 接受当前列宽”，也正是因为这个性质。

最后一行的差别是用户最容易感知的：改动前**每个候选行都是灰的**，而模态 `/model` 列表里
标签是正常文字色、描述才是灰色。同一个 scenario 在两边跑出的断言结果本身就是可复现的 A/B 判别器：

| 二进制 | 断言 |
|---|---|
| 改动前（`4ad27d5f8`） | **10 PASS / 7 FAIL / 4 XFAIL**（5 条 `probe` 里 4 条未命中，3 条拒绝旧布局的 `reject` 直接 FAIL） |
| 改动后（本轮提交） | **21 PASS / 0 FAIL / 0 XFAIL** |

---

## 3. 测试

新增 7 条，改 3 条旧断言（旧断言钉的是被删除的那套渲染）。

| 用例 | 文件 | 钉住什么 |
|---|---|---|
| `slash_dropdown_aligns_descriptions_into_the_select_list_column` | `tests/autocomplete.rs` | 12 列主列（`help` + `render` 这一组）→ 描述从第 14 列开始；标签被补齐到列宽 |
| `slash_dropdown_primary_column_tracks_the_widest_label` | 同上 | 最宽标签 24 + gap 2 = 26 列（clamp 内），两行描述同一列 |
| `slash_dropdown_clamps_a_label_wider_than_the_primary_column` | 同上 | 超过 32 列的标签被截断到 30，描述仍从第 34 列开始 |
| `file_dropdown_uses_the_fixed_primary_column` | 同上 | `@` 菜单走默认 32 列，且与同行的模态 `Selector` 落点一致 |
| `narrow_dropdown_drops_the_description_column` | 同上 | 40/30/20 列下只画标签，不溢出 |
| `the_dropdown_and_the_modal_selector_share_one_row_layout` | 同上 | **回归门**：同一批候选，下拉框的行 == 同 bounds 的 `Selector` 行（逐字节） |
| `the_composer_dropdown_paints_the_select_list_theme_roles` | `tests/app_theme.rs` | 真 `render_to_buffer`：选中行 `accent`+`selectedBg`、非选中标签无样式、描述列 `muted`、右侧被抹平 |

改动/新增的旧断言：

* `tests/autocomplete.rs::the_app_paints_the_dropdown_directly_above_the_prompt`：期望从
  `"  hotkeys  list shortcuts"` 改为 `"  hotkeys     list shortcuts"`（列宽 12；该用例的命令表
  自己定义了 `help` / `hotkeys`，所以列宽与全局命令表无关）。
* `tests/input_surface.rs::typing_slash_paints_the_command_dropdown_above_the_prompt`：
  40 列下不再出现描述文本（上游阈值是 40，不是 44），并显式断言这一点。

---

## 4. 门禁

工具链 `1.85.0`（`scripts/toolchain.sh` 解析），全部 `--locked`，`CARGO_INCREMENTAL=0`。
**最终树** = `4ad27d5f8`（LUM-1274/LUM-1312 已合入的 `origin/feature/pi.rs` tip）+ 本轮提交。

| 门禁 | 命令 | 结果 |
|---|---|---|
| 格式 | `cargo fmt --all -- --check` | ✅ 干净（本轮新代码先被 fmt 抓到 6 处，已 `cargo fmt --all`） |
| 静态检查 | `cargo clippy --workspace --all-targets --locked -- -D warnings` | ✅ 0 findings，exit 0 |
| 全量测试 | `cargo test --workspace --locked` | ✅ exit 0，**2570 passed / 0 failed**，164 个 suite |
| 单 crate 复跑 | `cargo test -p pi-tui --locked` | ✅ 369 lib + 41 集成 target + 7 doc，0 failed |
| 交互 A/B | `scripts/pty_capture.py` × 2（同 scenario，改动前/后二进制） | ✅ 后 21/21 PASS；前 10 PASS / 7 FAIL / 4 XFAIL |

本轮所有门禁都跑在**最终树**上（用 `git checkout origin/feature/pi.rs -- pi-rust/crates` 做 BEFORE 二进制、
再 `git checkout HEAD -- pi-rust/crates` 还原，源码逐字节不变）。

---

## 5. 已落地与未落地

### 5.1 本轮落地

1. 下拉框与模态选择器共用一份 `SelectList` 行布局（描述列对齐、列宽 clamp、宽度阈值回到上游的 40）。
2. 下拉框的选中行拿到 `selectedBg` 背景，非选中行的标签不再是灰色。
3. 下拉框借用的那几行先被 `reset()`，候选行右侧不再漏出转录内容。
4. 回归门 `the_dropdown_and_the_modal_selector_share_one_row_layout`：同一批候选，下拉框的行与
   同 bounds 的 `Selector` 行逐字节相等——以后谁再写第三套渲染，这条会红。

### 5.2 未落地（不属于本 issue 口径，列出来是为了不重复算账）

1. **下拉框的鼠标交互**：上游 `Editor.handleMouse` 支持点选下拉行（`editor.ts:622-638`），
   Rust 侧仍是纯键盘。
2. **列宽按 `char` 而不是显示宽度**：宽字形（CJK / emoji）会算窄，是 `pi-tui` 的既有偏差。
3. **`#` 触发符**：`DEFAULT_AUTOCOMPLETE_TRIGGER_CHARACTERS` 里声明了 `#`，但没有 provider 会响应它
   （见 §6.2 第 3 条）。

### 5.3 与同轮同伴任务的交叉

本轮收尾时 `feature/pi.rs` 被两个同伴任务推前了两次，均已 rebase 并重跑门禁：

* **LUM-1274**（`/scoped-models` 面板）：新增 13 列的命令名，把 `/` 菜单的主列从 12 推到 15——
  旧渲染器不会因为命令表变化而移动列，这正是两种实现的可观察差异之一（scenario 的断言因此从
  “写死第 14 列”改成“拒绝旧的两空格形式 + 接受当前列宽”）。
* **LUM-1312**（composer 多行编辑语义）：改了 `editor.rs` 的行域编辑与 `app.rs` 的
  `composer_body_width`，与本轮的 `autocomplete_render_*` / `paint_autocomplete` 相邻但不重叠，
  rebase 无冲突；两边的新测试在同一次 `cargo test --workspace` 里全绿（2570 passed）。

## 6. Rust ↔ TS 真实差距（本轮复测）

口径与命令见 `docs/RUST_TS_PARITY_METRICS.md` §1.1；下面是本轮亲自跑出的数字，不是引用。

| 轴 | 测量命令 | 本轮 | 上轮（引用） |
|---|---|---|---|
| TUI 模块数 | `ls packages/tui/src/*.ts packages/tui/src/**/*.ts \| wc -l` vs `ls crates/pi-tui/src/*.rs \| wc -l` | **33 / 42 = 78.6%** | 33/41 = 80.5%（上游自己长了一个文件） |
| `app.*` 接线率 | `python3 scripts/app_action_coverage.py .` | **wired 43/44 = 97.7%，advertised 0/44，silent 1/44**（只剩 `app.tree.editLabel`） | 37/44 = 84.1%（LUM-1274 又补了 `app.models.*`） |
| 测试用例数 | `cargo test --workspace --locked` 的 passed 合计（2570）vs `grep -rhoE "^\s*(it|test)\(" packages --include=*.ts \| wc -l`（5439） | **2570 / 5439 = 47.3%** | 2,232/5,309 = 42.0% |
| 下拉框与上游的组件一致性 | 本轮的 `the_dropdown_and_the_modal_selector_share_one_row_layout` | ✅ 逐字节相等 | ❌ 两套渲染 |

测试数 2570 跑在**最终树**（`4ad27d5f8` + 本轮提交，164 suites，0 failed）；
上表中的模块数 / 接线率是同树实测。

**加权总分不能直接与上轮比，原因是一条口径不一致（这里点出来，不掩盖）**：
`RUST_TS_PARITY_METRICS.md` §3.7 写的是「模块 80.5% 与快捷键 47.7% 的加权 = 58%」，但任何常见的
加权都得不到 58（50/50 → 64.1，60/40 → 67.4），58 的取法没有公开。本轮改用**公开公式**
`轴5 = (模块率 + 接线率) / 2`：

```
轴5 = (78.6 + 97.7) / 2 = 88.2%（上轮口径下是 58%）
轴12（测试强度）= 2570 / 5439 = 47.3%
其余 11 轴沿用 §4.1 的权重与得分
```

加权和 = 5×1.00 + 13×0.90 + 8×1.00 + 6×0.70 + 14×0.882 + 8×0.90 + 7×0.74 + 7×0.70 + 8×0.95
+ 7×0.20 + 9×0.85 + 5×0.473 + 3×0.95 = **80.4%**（上轮公开值 76.05%）。

**两个数字都在这里：差的那 4.4pt 里，3.4pt 是轴 5 的取法，剩下 1pt 是本轮 + LUM-1274/LUM-1312
把接线与用例真推上去的。**总分的大头摆动项依旧没动：§4.1 里权重最大的单项仍是「扩展生命周期事件」
（7% × 20%），把它补到 100% 单项就是 +5.6pt，比本轮这类 UI 一致性修复大一个量级。

### 6.1 本轮不派活的原因（“最多 3 个任务”是上限，不是下限）

issue 允许「最多开启 3 个任务同时运行」并要我选最佳方式。本轮的判断是**零派发**：
本 issue 自身的交付（下拉框走 `SelectList`）已经在本次 run 内完成并过了全量门，
派子任务只会把同一块代码面（`pi-tui` 的行布局）拆成并发写者；而当前唯一能改变总分大盘的工作
（扩展事件 7/36 → 36/36）已经在 `RUST_TS_PARITY_METRICS.md` §6 的 P0 位置，不需要新开一轮来判断。

### 6.2 下一轮性价比序列（不派活，只列清单）

1. **P0 · 扩展生命周期事件**（+5.6pt）——插件生态兼容的头号阻塞；`tool_call` / `tool_result` /
   `tool_execution_start|end` / `turn_start` / `message_*` / `session_shutdown` 在 Rust 下是死代码变体。
2. **P1 · 下拉框鼠标点选**（上游 `editor.ts:622-638`）——本轮把渲染统一了，鼠标是同一组件剩下的另一半。
   **已关闭（LUM-1431）**：press 高亮 / click 应用 / 异格释放丢弃 / 计数行不可点 /
   关闭即清命中框；见 `docs/LUM1431_AUTOCOMPLETE_MOUSE.md`。
3. **P1 · `#` 触发符**——`DEFAULT_AUTOCOMPLETE_TRIGGER_CHARACTERS` 声明了但无 provider 响应，
   属于「广告了不干活」，与 LUM-1308 消灭的 `app.*` 假广告同类。**仍开放**（LUM-1431 §7 复核）。

## 7. 已知限制

1. **列宽按 `char` 计数**，与 `pi-tui` 其余部分（`message` / `prompt` / `Selector`）一致；
   宽字形（CJK、emoji）仍按 1 列计。上游用 `visibleWidth`（wcwidth 语义），这是**既有**的偏差，
   不在本轮范围内。
   **已在 LUM-1418 修复**：全 TUI 13 个渲染模块 71 处测量/写入切到终端列口径
   （`crate::width`），本文件下文描述的是该轮之前的状态。
2. **下拉框借用转录行时会连滚动条那列一起抹平**（`selector`/`settings` 浮层同样如此）。
   下拉框存在期间该几行的滚动条缺一列，关闭后立即恢复。
3. `@` 文件菜单首行的「label 与 description 文本相同」不是缺陷：上游
   `autocomplete.ts:801-805` 就是 `label: entryName`、`description: displayPath`，
   顶层条目两者天然相同；改动后它至少落在对齐的描述列上，而不是紧贴标签。
