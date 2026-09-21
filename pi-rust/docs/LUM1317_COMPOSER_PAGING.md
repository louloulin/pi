# LUM-1317 — composer 草稿内翻页 + 平滑滚动窗口

结论先行：`PageUp` / `PageDown` 现在**按草稿是否溢出 composer 窗口决定归属**——溢出时由
composer 自己翻页（上游 `Editor.pageScroll` 语义），装得下时仍然是聊天记录翻页；
composer 窗口从「按光标所在页整页对齐」换成**逐行跟随光标**（上游 `render` 的
`scrollOffset` 规则），并像上游的 `createScrollBorder` 一样报出被藏起来的行数（`↑N` / `↓N`）。
真二进制 + 真 PTY 的 A/B：同一份场景在改动前是 **14 PASS / 1 FAIL / 9 XFAIL**，改动后
**24 PASS / 0 FAIL / 0 XFAIL**，那 1 个 FAIL 正是本轮要修的缺陷本身（见 §1）。

## 1. 真实审计：改动前 → 改动后

证据是**同一份**场景 `scripts/pty_scenarios/lum1317-composer-paging.json`（9 面板，76×26，
`--model faux/faux-model`）跑在**两个真二进制**上：`4ad27d5f8`（改动前基线，`feature/pi.rs`
当时的树）与 `cc53297c4`（本轮）。

| 行为 | 改动前（`4ad27d5f8`） | 改动后（`cc53297c4`） |
|---|---|---|
| 12 行草稿 + `PgUp` | `tui.altScreen.pageUp` 先截走 → **聊天记录翻页**：视口脱钩，底部出现 `↓ Jump to latest message`（面板 4 的那条 FAIL 就是它） | 草稿溢出窗口 → `tui.editor.pageUp`：光标 11 → 3，窗口 4 → 3；**pill 不出现**，transcript 一个像素没动（面板 4） |
| 草稿滚动窗口 | `scroll_window_start()`：`page = (cursor_row / show_rows) * show_rows`，光标在一个页内移动时窗口不动，跨页时**整页跳** | `follow_cursor()`：`cursor_row < start → start = cursor_row`；`cursor_row >= start + show_rows → start = cursor_row + 1 - show_rows`，**逐行**跟随（面板 5/6/7：`↑2` → `↑1` → `> `） |
| 被藏起来的行 | 没有任何提示；`↑N` / `↓N` 探针在基线全部 XFAIL（9 条） | label 栏画 `↑N` / `↓N`（面板 3/6/7/8），单行窗口画 `↕N` |
| `PgUp` 翻的"一页"多长 | 聊天记录一屏（`viewport_height`） | 草稿溢出时是 composer 窗口高度（8 行，= `composer_max_rows`；面板 4 的 11 → 3） |
| 草稿装得下时 `PgUp` | 聊天记录翻页 | 聊天记录翻页（面板 9 → 10：清空草稿后同一按键让 pill 出现） |
| `Ctrl+PageUp` / `Ctrl+PageDown` | 进编辑器后**无 consumer**（模块注释里明确写"编辑器永远不滚动"） | 进编辑器的翻页（`Ctrl+*` 变体从来不是 `tui.altScreen.pageUp` 的 chord） |

改动前的基线数字（同一份场景、同一个 harness）：

```
$ python3 scripts/pty_capture.py --bin /tmp/pi-baseline-4ad27d5f8 \
    --steps /tmp/lum1317-baseline-scenario.json --out /tmp/pty-baseline-1317.png
assertions: 24 checks over 9 panels — 14 PASS, 1 FAIL, 9 XFAIL, 0 XPASS
  FAIL   reject 'Jump to latest message'  — the load-bearing one: PgUp paged the
         composer, so the chat log never detached
  XFAIL  probe  '↑4row 05' / '↑3row 04▍' / '↑2row 03▍' / '↑1row 02▍' /
         '↓3row 09' / '> row 01▍' / '↓4row 08' / …
```

改动后（本轮二进制）：

```
$ python3 scripts/pty_capture.py --bin target/debug/pi \
    --steps scripts/pty_scenarios/lum1317-composer-paging.json \
    --out docs/screenshots/lum1317-composer-paging.png
assertions: 24 checks over 9 panels — 24 PASS, 0 FAIL, 0 XFAIL, 0 XPASS
```

截图 `docs/screenshots/lum1317-composer-paging.png`（9 面板）+ 同名 `.txt` 字符网格
（pyte 真解析，`↑` / `↓` 逐字可核对）是这一节的证据本体。

## 2. 实现

### 2.1 `crates/pi-tui/src/app.rs` —— 归属判断

* `App::composer_overflows()`（`app.rs:4639`）：`editor.visual_row_count() >
  composer_window_rows()`，即"草稿需要的视觉行数 > 帧真正给 composer 的行数"。
  行数用上一帧的换行宽度度量，所以它回答的正是渲染器刚刚回答过的问题。
* `App::composer_window_rows()`（`app.rs:4620`）：上一帧 `paint_prompt` 实际用的高度
  （已经过 `composer_max_rows` 夹取）；尚未渲染时回落到配置值，让"还没画第一帧就按键"
  走和下一帧相同的分支。
* `step_key_at` 的两条 `tui.altScreen.pageUp/pageDown` 分支各加了
  `&& !self.composer_overflows()`（`app.rs:2790-2820`）：条件不成立时**不消费**，
  事件自然落到下面的 `prompt.handle_key`，由编辑器的 `tui.editor.pageUp/pageDown` 接住。
  于是"聊天记录翻页"的能力在草稿装得下时一字不变，溢出时才让位。
* 每帧把窗口高度交给编辑器（`app.rs:2844-2852`：`set_page_rows`），让"一页"和"看得见的
  行数"是同一个数。
* `composer_scroll` / `composer_window` 两个原子字段（`app.rs:1100-1114`）：滚动偏移必须
  跨帧存活，而 `render_lines` / `render_snapshot` 都是 `&self`，所以沿用本 crate 既有的
  "渲染层记录、按键层读取" 模式（和 `composer_body_width` 同一手法）。公开
  `App::composer_scroll()` / `App::composer_window_rows()` 供测试与审计读取。

### 2.2 `crates/pi-tui/src/editor.rs` —— 草稿内翻页

* `Editor::page_up()` / `page_down()`（`editor.rs:987,993`）+ `page_scroll()`（`editor.rs:1008`）：
  上游 `Editor.pageScroll`（`packages/tui/src/components/editor.ts:1949`）的移植——按
  `page_rows` 移动**光标**，目标行夹在 `[0, 最后一行]`，沿用
  `preferred_col` 粘性列（和 `move_vertical` 同一决策表）。窗口由渲染层跟随，与上游
  "pageScroll 移动光标 + render 调整 scrollOffset" 的职责切分一致。
* `set_page_rows()` / `page_rows()`（`editor.rs:510,515`）：`0`（没人交过一帧）→ 一页一行，
  避免直接驱动 `Editor` 的调用方拿到一个死键。
* `handle_key` 在 `cursorLeft/Right` 之后分派这两个 id（与上游 `handleInput` 的检查顺序一致），
  模块文档里原本"`tui.editor.pageUp/pageDown` 没有 consumer"的那条**删除**，改成归属规则说明。

### 2.3 `crates/pi-tui/src/prompt.rs` —— 窗口与标记

* `Prompt::render_lines(width, max_rows, scroll) -> (Vec<String>, usize)`
  （`prompt.rs:259`）：接收上一帧的窗口起始行，返回本帧画的那一行。渲染不再是"无状态"的，
  这正是平滑滚动的前提。
* `follow_cursor()`（`prompt.rs:442`）：上游 `render` 的
  `packages/tui/src/components/editor.ts:532-540` 规则（**逐行**跟随 + 尾部夹取），
  替换掉原来的 `scroll_window_start()`（整页对齐）。
* `window_prefix()` + `scroll_marker()`（`prompt.rs:340,463`）：label 所在的 2 列是 Rust
  composer 唯一的稳定 chrome（上游这里是一整条边框，`createScrollBorder`，
  `packages/tui/src/components/editor.ts:266`），所以标记画在那里：首可见行 `↑N`、
  末可见行 `↓N`、只有一个可见行且两侧都有内容时 `↕N`；数字放不下时只留箭头（箭头是
  affordance，数字是细节）。草稿自己的第 0 行**永远保留 `> ` 标签**（不然滚动一下用户就
  认不出当前模式了）。
* 草稿装得下时 `follow_cursor` 恒返回 `0`，所以偏移不会从上一份草稿泄漏到下一份。

### 2.4 `crates/pi-coding-agent/src/commands/slash.rs` —— 广告面

`/help` 图例原来写 `PgUp/PgDn  scroll the chat log one page`，现在两种归属都写：
`page the chat log, or the draft when it does not fit the box`（≤74 列，不被边框裁掉）。
`help_text_documents_scroll_keys` 增了一条断言钉住这半句——LUM-1312 的教训是
"广告了却没人消费"和"消费了却广告错"一样糟。

## 3. 验证实况（1.85.0，`--locked`）

| 门禁 | 结果 |
|---|---|
| `cargo fmt --all -- --check` | PASS（无 diff） |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | PASS（exit 0，无 warning/error） |
| `cargo test --workspace --locked` | **2575 passed / 0 failed / 2 ignored（165 suites）**（本轮前基线 2563/164） |
| 真 PTY 断言 `lum1317-composer-paging` | **24/24 PASS**（9 面板，76×26，见 §1 与附件截图） |
| 同一场景跑基线 `4ad27d5f8` | 14 PASS / 1 FAIL / 9 XFAIL（BEFORE 证据） |
| 既有场景回归（基线 vs 本轮，同一二进制对同一份 json） | `lum1267` **47 PASS / 11 FAIL / 2 XFAIL 与基线逐条一致**；`lum1271` 39/40 + 1 FAIL 与基线逐条一致；`lum1312-chatinput-multiline` 19/19、`lum1312-full-tui` 7/7、`lum1282` 7/7、`lum1308` 5/5、`lum1298` 12/13（那 1 条 XFAIL 与基线相同）；`lum1260` / `lum1257` / `lum1259` / `input-surface` 为截图场景（无断言）exit 0 |
| `scripts/app_action_coverage.py` | `wired 43/44 (97.7%)`、`advertised 0/44`（本轮不动 `app.*` 轴） |
| `tui.editor.*` + `tui.input.*` id | 27 个 id，直接消费 **23/27 → 25/27**（新增的正是 `tui.editor.pageUp` / `pageDown`；剩 2 个是默认不绑定的 `historyPrevious` / `historyNext`，`Up/Down` 走到 history 由上游规则覆盖） |

`lum1267` / `lum1271` 那 12 条 FAIL **不是本轮引入的**：它们是场景陈旧——`/copy` 的补全
过滤（LUM-1263 记录的偏差）、`/model` 首屏（现在是 `ant-ling`/`anthropic`）、以及
LUM-1312 故意改掉的 `/help` 文案（`Ctrl+U clear the prompt buffer`、`· keys:`、
`submit prompt`、`/thinking`、`/trust`）。基线二进制与本轮二进制的**失败集合逐条 diff 为空**，
本轮的 PTY 回归因此是可证伪的"零回归"，而不是"没跑"。

新增测试：

* `crates/pi-tui/tests/composer_paging.rs`（新，8 条）：溢出时 `PgUp` 翻 composer 且
  transcript 不动、装得下时翻 transcript、`composer_max_rows` 边界（7/8/9 行）、
  窗口逐行跟随（`[6,6,6,6,5,4,3,2,1]`）、一页 = 窗口高度、`↑`/`↓` 标记与计数、
  `Ctrl+PageUp` 不进 transcript、清空草稿后偏移不残留。
* `crates/pi-tui/src/editor.rs`（4 条单测）：翻页步长与两端夹取、跨页保持显示列、
  没有页高时的单行回退、`PageUp`/`PageDown` 的 id 分派。

## 4. 与上游 / codex 的对照（不粉饰）

已对齐：`PageUp`/`PageDown` 在草稿内翻页且移动的是光标（窗口跟随）、窗口逐行跟随
（不是整页对齐）、溢出时有"上方/下方还有内容"的指示、`Ctrl+PageUp` 变体归编辑器、
草稿装得下时不抢聊天记录的翻页键。

仍然不同的地方与理由：

1. **"一页"的数值口径**：上游 `pageSize = max(5, floor(terminalRows * 0.3))`
   （`editor.ts:1951`），Rust 用 composer 窗口高度（`composer_max_rows`，默认 8）。
   上游那个数同时也是它的 `maxVisibleLines`（`editor.ts:524`），所以两边"一页 == 一屏"
   的语义相同，只是窗口大小的来源不同（上游按终端高度百分比，Rust 按配置上限）。
2. **指示器位置**：上游画在文本框的上下边框里（`─── ↑ 3 more ───`），Rust composer
   没有边框，只能借用 label 那 2 列，于是长数字放不下时退化成纯箭头；单行窗口用
   `↕N`，因为一个可见行没有"上下两条边框"可画。
3. **鼠标滚轮**：上游 `Editor.handleMouse` 里有 `scrollOffset` 分支（在文本框内滚），
   Rust composer 仍不处理滚轮事件（`App` 把它当成 transcript 滚动 / 选择拖动）。
   这是 composer 侧唯一还没接的窗口输入，已列进后续。
4. **粘性列**：跨页保持显示列已对齐，但上游 `moveToVisualLine` 还有"吸附到 segment 起点"
   （paste marker / 组合字素）那一步，Rust 侧仍是 char 级（与 LUM-1312 §4 的宽度口径
   问题同源）。
5. **极窄终端下的归属**：判断用的是"帧真正给 composer 的行数"，所以当
   `plan_chrome` 把编辑器区域压到 1 行时，2 行草稿也会夺走 `PgUp`。这与"草稿真的溢出
   可见窗口"这条规则一致，但在极端尺寸下与"只有超过 `composer_max_rows` 才接管"的
   直觉不同；正常终端两者等价（编辑器区域 == `min(草稿行数, composer_max_rows)`）。

## 5. 完成度与后续

| 轴 | 自评完成度 |
|---|---|
| composer 多行**渲染** | 90%（本轮不变） |
| composer **编辑语义** | 约 80% → **约 90%**（滚动/翻页这一块补齐，剩粘贴折叠、history 持久化） |
| `tui.editor.*` + `tui.input.*` id 消费 | 23/27 → **25/27** |
| 整个 TUI 的 codex/pi-ts 交互面 | 约 75% → 约 76%（composer 剩余项 + provider/后端面仍是主要缺口） |
| Rust↔TS 整体移植 | 与本轮前一致，见 `docs/FEATURE_PI_RS_STATUS.md` |

LUM-1312 §5 派出的三个子任务里，本 issue 是 1/3；2/3（粘贴折叠 `[paste #N +M lines]`）与
3/3（history 跨会话 + `Ctrl+R` + 附件召回）仍未做。本轮之后 composer 侧剩下的差距：

1. 鼠标滚轮在 composer 内滚动（本轮 §4.3），以及"滚轮滚动后光标被拉回可见区"的规则。
2. 上游把窗口跟随放在 `render` 里、把 `scrollOffset` 放在 `Editor` 里；Rust 现在把它放在
   `App`（`composer_scroll`）。如果将来 `Prompt` 要在别的宿主里渲染，这个偏移应当跟着
   `Prompt` 走 —— 属于"再多一个宿主时"的重构，本轮不预支。
