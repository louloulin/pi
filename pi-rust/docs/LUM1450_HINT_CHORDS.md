# LUM-1450 — 提示面与选择键统一按生效键位渲染

> 基线：`feature/pi.rs` = `c6d6df108`（= LUM-1447 合并 tip，本轮从它起）
> 环境：Windows 10 x86_64 / cargo 1.97.1 / `--offline`
> 本轮改 `pi-tui`（selector / settings / dialog / app / prompt）+ `pi-coding-agent`
> （`commands/slash.rs` / `interactive.rs` / `text_fallback.rs`）+ 1 个扫描脚本；
> 不碰 `pi-protocol` / `pi-ai` / `pi-agent-core` / `pi-extensions` / `pi-session`。

## 1. 为什么是这块

LUM-1447 §7 把「提示面硬编码 chord」列为第 2、3 条。本轮开工前先把这个**缺陷类**量了一遍
（脚本随本轮入库，可复跑、可反证）：

```bash
python pi-rust/scripts/hint_chord_literals.py pi-rust          # 报告
python pi-rust/scripts/hint_chord_literals.py pi-rust --check   # 门禁
```

**基线实数：19 处**。它们分三种，而第 3 种是上一轮没被发现、危害最大的：

| 类别 | 基线处数 | 用户看到什么 |
|---|---|---|
| `/help` 的 `keys:` 段（整段字面量） | 10 | 改键后 header 与 `/hotkeys` 都换了，`/help` 还在广告旧 chord |
| 三处对话框页脚 + 反向搜索页脚 | 4 | 扩展模态页脚写死 `[Enter]`/`[Esc]` |
| **`/hotkeys` 的 modal 键位是死广告** | — | 见下 |
| 其它运行期提示（bash 忙、`/settings` 描述、无 TTY 回退提示符） | 4 | 文案里的 chord 不随改键 |
| `/hotkeys` 尾部的 `Esc` 与 `/scoped-models` 命令行 | 2 | 同上 |

第 3 类是本轮**顺手量出来的新缺陷**：`pi-tui` 的 `Selector` 与 `SettingsList`
把 `Enter` / `Esc` / `↑` / `↓` **写死在 `match key.code`** 里，而
`/hotkeys` 的 selectors 分组一直在广告 `tui.select.up/down/confirm/cancel`
（`keybinding_coverage.py` 报 `tui.* 49/49 consumed`，因为
`editor.rs` 的补全下拉框消费了这几个 id，于是扫描器认为它们「已被消费」）。
上游不是这样：`SelectList::handleInput` 与 `SettingsList::handleInput` 全走
registry（`packages/tui/src/components/select-list.ts:144-175`、
`settings-list.ts:231-246`）。

所以用户写

```json
{ "selectUp": ["ctrl+p"], "selectDown": ["ctrl+n"] }
```

在 codex 里能用的 `Ctrl+P/N`，在 Rust 版的 `/model`、`/session`、`/tree`、`/settings`
里**一个都不生效**——`/hotkeys` 却照着新键广告。这是「广告了但不干活」最严重的一处，
因为它覆盖每一个模态列表。

## 2. 语义（逐条对齐上游）

| 上游 | 本轮实现 |
|---|---|
| `SelectList::handleInput`：`tui.select.up` → 上一行（**回绕**）、`down` → 下一行（回绕）、`confirm` → 激活、`cancel` → 取消 | `Selector::select_list_action`，判定顺序照抄上游（up, down, confirm, cancel），`prev()`/`next()` 的回绕语义一字不改 |
| `SelectList` 的 `pageUp`/`pageDown` 无（上游纯列表不分页） | Rust 的 `Selector` 有分页，且表里有 `tui.select.pageUp/pageDown` → 一并接线（顺序放在 cancel 之后，不抢上游四键） |
| `kb.matches` 判定在「修饰键属于 App」之前 | `handle_search_key_with` 先判 5 个 chord 再判修饰键：`tui.select.cancel` 出货就是 `escape` **和** `ctrl+c`，上游列表自己应答 `ctrl+c`，不推给 App |
| `SettingsList::handleInput`：up/down/confirm/cancel + 空格激活（搜索框为空时） | `SettingsList::handle_key_with` 同构；`k`/`j`/`Home`/`End`/空格/搜索输入等 Rust 扩展保留在 chord 判定之后 |
| `ExtensionInputComponent`：`keyHint("tui.select.confirm","submit")`，匹配 `tui.select.confirm` **或** `\n`（`extension-input.ts:67,75`） | `Dialog` 的 `Input` 除了 `Prompt` 自己的 `tui.input.submit` 之外**也**接 `tui.select.confirm`；页脚按 `tui.input.submit` 渲染（真正提交它的是 `Prompt`） |
| 扩展 confirm 上游是 Yes/No `SelectList` | Rust 的 `Confirm` 不是列表，但接受/拒绝两键改用列表的 confirm/cancel id；`y`/`n` 作为 Rust 自有别名保留 |
| `keyHint(id, desc)` 读 registry | 新增 `pi_tui::keybindings::key_text_in(kb, id, fallback)` 与 `key_text_preferring(kb, id, preferred)`；`key_text_or` / `key_hint_or` 改为它们的全局包装 |

### 2.1 为什么需要 `key_text_preferring`

registry 的出货集合是**冗余**的：`tui.editor.cursorLineStart` 同时绑
`home` / `ctrl+home` / `ctrl+a`。如果 `/help` 老老实实渲染「全部生效 chord」，
那一行会变成 `Home/Ctrl+Home/Ctrl+A / End/Ctrl+End/Ctrl+E`
（46 列），把单行图例撑成一面墙。

`key_text_preferring` 的规则因此是：

* 表里**仍绑着**出货那个 chord → 渲染它（`Ctrl+A`）；
* 该 id 被改到别处 → 渲染**实际生效**的 chord（`Alt+A`）；
* 表里没有这个 id（裸 `pi-tui` 注册表解析不了 `app.*`）→ 渲染传入的出货默认值；
* 表里有但被解绑 → 空串，**整行丢弃**（一条没有 chord 的图例行什么也没广告）。

`/hotkeys` 仍是**穷举**列表（它渲染全部生效 chord），`/help` 是**单行图例**
（每个动作一个要记的键）。二者分工写在代码注释里。

### 2.2 顺手改掉的一处**错误**广告

基线图例里有一行：

```text
  Ctrl+C      abort the current turn (or clear the prompt on idle; twice exits)
```

它把**两个动作**压在**一个 id** 上。合并表里它们本来就是两个 id：

| id | 出货 chord（`pi-coding-agent/src/keybindings.rs:227-229`） | 含义 |
|---|---|---|
| `app.interrupt` | `escape` | abort |
| `app.clear` | `ctrl+c` | 清空草稿（再按一次退出） |

所以旧图例的 chord 列**本身就是错的**（`Ctrl+C` 不 abort，它先清草稿）。本轮拆成两行，
两行都从表里解析：

```text
  Esc              abort the current turn
  Ctrl+C           clear the prompt on idle (twice exits)
```

这是「按生效键位渲染」顺带修掉的语义错误，不是新功能。

## 3. 改动清单

| 位置 | 内容 |
|---|---|
| `pi-rust/crates/pi-tui/src/keybindings.rs:806` | `key_text_in(kb, id, fallback)`（可注入的 `key_text_or`） |
| `pi-rust/crates/pi-tui/src/keybindings.rs:838` | `key_text_preferring(kb, id, preferred)` + 取舍理由 |
| `pi-rust/crates/pi-tui/src/selector.rs:697-830` | `handle_key`/`handle_search_key` 拆出 `*_with`；新增 `select_list_action`（5 个 chord）与 `confirm()`；搜索路径的修饰键守卫移到 chord 判定之后 |
| `pi-rust/crates/pi-tui/src/settings.rs:320-370` | `handle_key_with`：四个 `tui.select.*` chord 先判，`k`/`j`/`Home`/`End`/空格/过滤留在后面 |
| `pi-rust/crates/pi-tui/src/prompt.rs:165-200` | `Prompt::handle_key_with`（`Dialog` 注入表时不再回落到全局注册表） |
| `pi-rust/crates/pi-tui/src/dialog.rs:225-300` | `handle_key_with`；Confirm/Input 接 `tui.select.confirm`/`cancel`；`dialog_hint()` 生成页脚 |
| `pi-rust/crates/pi-tui/src/dialog.rs:315-390` | 三处页脚改为 `dialog_hint(&[key_text_or(...)], label)` |
| `pi-rust/crates/pi-tui/src/app.rs:1886-1900` | 反向搜索页脚的两个 chord 走 `tui.input.submit` / `tui.select.cancel` |
| `pi-rust/crates/pi-coding-agent/src/commands/slash.rs:169` | `help_text()` → `help_text_with(kb)`；`key_legend()`：12 行图例按表解析、列宽按最宽生效 chord 计算 |
| `pi-rust/crates/pi-coding-agent/src/commands/slash.rs:178,224` | `/scoped-models` 命令行与 `/hotkeys` 尾部 `Esc` 改为解析 |
| `pi-rust/crates/pi-coding-agent/src/interactive.rs:1243,3578` | bash 忙提示、`/settings` 的 copy 描述改为解析 |
| `pi-rust/crates/pi-coding-agent/src/text_fallback.rs:46,88` | 无 TTY 回退提示符的 `Ctrl+D` 改为解析 |
| `pi-rust/crates/pi-tui/tests/select_list_keybindings.rs`（新，9 条） | 行为 + 页脚：出货表零回归 / `ctrl+p/n` 移动（含可搜索列表）/ `f2`+`alt+x` 确认取消 / SettingsList 同构 / 三处页脚默认与改键 / 解绑 / App 真实 `step` 路由 / 只有 `Select` 有列表 |
| `pi-rust/crates/pi-tui/tests/lum1450_dialog_frames.rs`（新，2 帧） | 80×24：确认模态页脚，出货表 vs `selectConfirm/selectCancel` 改键 |
| `pi-rust/crates/pi-coding-agent/tests/lum1450_help_legend_frames.rs`（新，2 帧） | 100×30：真合并表 + 真 `/help` 文本，出货 vs 三个 chord 改键 |
| `pi-rust/crates/pi-coding-agent/src/commands/slash.rs`（测试） | `the_help_legend_tracks_the_effective_chords`（改键 / 文案内引用 / 解绑丢行） |
| `pi-rust/scripts/hint_chord_literals.py`（新） | 提示面 chord 字面量扫描 + 双向 `--check` |
| `pi-rust/docs/screenshots/lum1450-*.{png,txt}`（新，8 个） | §5 的四帧及其 cell grid |

## 4. 验证（本机 Windows / cargo 1.97.1 / `--offline`）

| 门禁 | 结果 |
|---|---|
| `cargo test -p pi-tui` | **1038 passed / 0 failed**（基线 1027，+11 = 9 行为 + 2 帧） |
| `cargo test -p pi-coding-agent --lib` | **581 passed / 8 failed**（基线同机 A/B 实测 580/8，**同一批 8 个** Windows 环境类失败：`absolute_paths_stay_absolute`、`missing_files_report_the_upstream_message`、`load_extensions_loads_an_esm_extension_from_disk`、`prompt_contains_builtin_tools_context_and_skills`、`absolute_paths_are_rejected`、`has_no_trust_requiring_resources_in_an_empty_project`、`detects_trust_requiring_project_resources`、`cli_export_propagates_the_upstream_error_text`；+1 = slash 新用例） |
| `cargo test -p pi-coding-agent --no-fail-fast` | 失败的 target 与基线**逐条相同**：`cli_extensions` 3/10、`cli_tools` 0/4、`reload_config` 3/1、`system_prompt_resources` 4/1、`tools` 35/1、`tools_navigation` 26/3（全部为绝对路径 / 临时目录 / 扩展加载的 Windows 环境类，基线已用 `git stash` A/B 实测同一批） |
| 提示面相关 target | `help_text_layout` 3/0、`lum1450_help_legend_frames` 2/0、`startup_header` 3/0、`keybindings` 20/0、`keybinding_install` 1/0、`chatinput_chord_conflicts` 2/0、`tools_render` 3/0 全绿 |
| `cargo fmt --all -- --check` | 干净（exit 0） |
| `cargo clippy -p pi-tui --all-targets -- -D warnings` | 干净（0 告警） |
| clippy `-p pi-coding-agent --all-targets` | 仅剩**既存**告警 `pi-extensions/src/host.rs:3144 fn signal_name` 未使用（基线同样报，本轮未碰该文件）；本轮新增/修改的文件零告警 |
| `hint_chord_literals.py --check` | exit 0：**19 → 0** 硬编码提示 chord，5 条知会项 |
| `keybinding_coverage.py --check` | exit 0：`tui.* 49/49`、`app.* 43/44`、`in sync: 1 known-unconsumed` |
| `app_action_coverage.py --check-consumed` | exit 0：`43 entries; 43 wired; in sync` |

### 4.1 反向验证（三处，全部在**未改动的基线**上跑同一份断言）

本轮的新用例大多调用新增的 `handle_key_with` / `help_text_with`，在基线上**编译不过**——
那只能证明 API 是新的，不能证明**行为**变了。所以另写三份只用基线 API
（`set_keybindings` + `handle_key` / `help_text`）的临时用例，先 stash 掉本轮
`pi-rust/crates/**/src` 再跑：

| 断言 | 基线 | 本轮 |
|---|---|---|
| 装 `{"selectDown":["ctrl+n"]}` 后 App 真实 `step(ctrl+n)` 移动 picker 高亮 | **FAILED**（`Idle`，不移动） | ok |
| 装 `{"selectConfirm":["f2"]}` 后确认模态页脚含 `f2` | **FAILED**（页脚写死 `[Enter/y]`） | ok |
| 装 `{"historySearch":["ctrl+p"]}` 后 `help_text()` 含 `Ctrl+P` 且不含 `Ctrl+R` | **FAILED**（整段字面量） | ok |

反向验证脚本跑完即删（未入库），因为同一语义已由
`tests/select_list_keybindings.rs` 与 `slash.rs::the_help_legend_tracks_the_effective_chords`
以可注入表的形式固化。

`hint_chord_literals.py --check` 本身也做了双向反证：

* 把 `("slash.rs","kitty-protocol")` 从知会表里删掉 → exit 1，精确报出
  `HARDCODED …slash.rs:251 "insert a new line (Shift+Enter on kitty-protocol terminals)"`；
* 往知会表里塞一条匹配不到任何东西的 `("slash.rs","NEVER MATCHES")` → exit 1，
  报 `STALE ALLOWLIST … matches nothing`（防止「修好了但没缩小清单」）。

## 5. 帧截图

`frame-buffer` 通道（本机无 PTY，见 `docs/LUM1426_POINTER_COLUMNS.md` §9）：
真 `App::render_to_buffer` 的 cell grid → `scripts/frame_to_png.py`。

| 文件 | 内容 |
|---|---|
| `docs/screenshots/lum1450-help-legend-default-100x30.png` | 合并表：`Ctrl+R` 反查、`Ctrl+L` 选模型、`Esc` abort、`Ctrl+C` 清草稿 |
| `docs/screenshots/lum1450-help-legend-rebound-100x30.png` | 把 `historySearch`→`ctrl+p`、`interrupt`→`ctrl+g`、`modelSelect`→`ctrl+m`：同三行跟着换，其余行逐字未动 |
| `docs/screenshots/lum1450-dialog-footer-default-80x24.png` | `ctx.ui.confirm` 页脚 `[Enter/y] accept    [n/Esc/Ctrl+C] deny` |
| `docs/screenshots/lum1450-dialog-footer-rebound-80x24.png` | 同一模态在 `selectConfirm/selectCancel` 改键后：`[f2/y] accept    [n/Alt+X] deny` |

诚实说明：帧证明**画出来的字**，不证明按键时序；按键语义由 §4 里那些喂真 `Key` 的
行为断言与反向验证证明。每帧都附 `.txt` 原始格网，可直接 `diff`：

```bash
diff <(sed -n '16,30p' docs/screenshots/lum1450-help-legend-default-100x30.txt) \
     <(sed -n '16,30p' docs/screenshots/lum1450-help-legend-rebound-100x30.txt)
```

三行差异，正好是被改的三个 chord。

## 6. Rust ↔ TS 差距复测（本轮亲自跑）

| 量 | 本轮实测 | LUM-1447 | 命令 |
|---|---|---|---|
| 纯代码规模（src↔src） | **90.5%**（138,635 / 153,106） | 90.2%（138,153） | `python pi-rust/scripts/measure_loc.py` |
| 测试规模 | **50.6%**（2,684 / 5,309） | 50.3%（2,670） | `grep -rhoE '#\[(tokio::)?test\]' pi-rust/crates --include=*.rs \| wc -l` ↔ `find packages -name '*.test.ts' \| xargs grep -hoE '\bit\(|\btest\(' \| wc -l` |
| TUI 模块面 | 35 / 42 = 83.3% | 同 | 本轮无新 src 模块 |
| `app.*` 接线 | 43 / 44 = 97.7%（`app.tree.editLabel` silent） | 同 | `app_action_coverage.py --check-consumed` |
| `tui.*` 消费面 | 49 / 49 = 100% | 同 | `keybinding_coverage.py --check`；**本轮的教训**：这个口径只测「有没有字面量消费者」，`Selector` 写死 `KeyCode::Enter` 也能让 `tui.select.confirm` 算「已消费」——见 §7 |
| 扩展生命周期事件 | 36/36 | 同 | 未动 |
| **提示面硬编码 chord（本轮新登记）** | **0 / 19 → 0**（5 条知会项） | ——（未测） | `hint_chord_literals.py --check` |
| **模态列表键位来源（本轮新登记）** | **2 / 2 组件 + 3 / 3 页脚**（`Selector`、`SettingsList` 的 up/down/confirm/cancel/pageUp/pageDown 全读 registry；三处对话框页脚全解析） | 0 / 2 + 0 / 3 | 见 §4 行为断言；`selector.rs:779`、`settings.rs:329`、`dialog.rs:344-390` |

**加权完成度**（权重表见 `RUST_TS_PARITY_METRICS.md` §4.1，公式公开）：
13 个轴里只有第 12 轴（测试）动了，`2,670/5,309 = 0.5029` → `2,684/5,309 = 0.5056`：

```text
5×1.00 + 13×0.90 + 8×1.00 + 6×0.70 + 14×0.905 + 8×0.90 + 7×0.783 + 7×0.70
  + 8×0.95 + 7×1.000 + 9×0.85 + 5×0.5056 + 3×0.95 = 86.8%
```

一位小数不变。**这是诚实的**：本轮关掉的是「广告的键位不生效」这类缺陷，
它不在 §4.1 那 13 个轴的任何一个里——轴 5（TUI 交互面）用的是「模块率与
`app.*` 接线率的均值」，而 `app.*` 一列一动没动。所以本轮的价值不体现在加权分上，
体现在两个**新登记、可反证**的轴上（提示面 0/19；模态列表键位来源 2/2 + 3/3），
以及 §4.1 里那三条反向验证。

**当前进度一句话**：纯代码规模 **90.5%**、测试规模 **50.6%**、13 轴加权 **86.8%**、
TUI 交互+视觉轴 `(14×0.905 + 8×0.90)/22 = 90.3%`。

**合并后复测**：本轮到分支时 `origin/feature/pi.rs` 已被 LUM-1448（扩展 autocomplete
provider）推进，合并后的实测数字（规模 **91.5%**、测试 **51.1%**、`pi-tui` **1045/0**、
`pi-coding-agent --lib` **584/8**、加权仍 **86.8%**）与两份数字的分工见
`RUST_TS_PARITY_METRICS.md` §0.16.1。

## 7. 剩余缺口（供下一轮起手）

| 顺位 | 项 | 范围 | 说明 |
|---|---|---|---|
| 1 | `hint_chord_literals.py` 的 5 条知会项里，3 条是**真**缺口 → **已立 LUM-1453（backlog，指派 devbox1）** | `pi-coding-agent` | ① `AUTOCOMPLETE_COMMANDS` 的 `Configure which models Ctrl+P cycles`（`const` 表，需 provider 侧格式化）；② `/help` 的 `/new` 行文案里的 `Shift+Enter`（kitty 协议条件，需改成渲染 `tui.input.newLine` 的全部 chord，列宽 +3）；③ `/hotkeys` 的 `(Ctrl+O by default)` / `(Alt+H by default)`（说的是出货值，不是生效值）。三者都已写进脚本注释的理由与 LUM-1453 正文 |
| 2 | `keybinding_coverage.py` 的口径缺陷 | `pi-rust/scripts` | 它把「非定义文件里出现过这个 id 字面量」当作「已消费」，于是 `Selector` 写死 `Enter` 时 `tui.select.confirm` 仍算 100%。本轮靠人工发现；下一轮应加一条**组件级**断言：`Selector`/`SettingsList`/`Dialog` 的 `handle_key` 轨迹里必须出现 `kb.matches` 调用（可用 `--check-components` 反证） |
| 3 | `app.tree.editLabel`（唯一 silent `app.*`） | `pi-coding-agent` + `pi-tui` | 需要 tree 改名 UI（组件不存在），补齐后 `app.*` 44/44 = 100% |
| 4 | `/help` 的 `keys:` 段与 `/hotkeys` 仍是两套行表 | `pi-coding-agent` | 两处都读 registry，但行集合与分组不同（`/help` 12 行紧凑图例、`/hotkeys` 5 组穷举）。合并会让 `/help` 变长；可考虑让 `/help` 只留 `keys:` 标题 + 指向 `/hotkeys` |
| 5 | modal 内拖拽/悬停、`settings` 滚轮按矩形认领 | `pi-tui` | 沿用 LUM-1445 §8 第 4、5 条 |

## 8. 「最多 3 个任务」的处置 = 1 自做 + 0 新派运行 + 1 入库 backlog + 0 收编

* **自做（1 件）**：本轮切片集中在 `pi-tui` 的四个模态组件 + `pi-coding-agent` 的
  提示文案。与在办的 LUM-1448（扩展 autocomplete provider，`pi-extensions` +
  `pi-coding-agent/src/extensions/`）、待办的 LUM-1434（CLI flag 面，
  `pi-coding-agent/src/cli/`）**文件面零重叠**。
* **新派（1 件，入库 backlog 不启动）**：§7 第 1 条（提示面剩余 3 处硬编码 chord）
  立为 [LUM-1453](mention://issue/01a0cb0d-d7b2-7a5e-88fb-c0204eeb4b00) 并指派
  `编程助手-devbox1`（`22e8b20d`），但状态写 **backlog**
  —— 平台语义是「todo 会立刻起一轮 run，backlog 只入库待提升」。
  验收标准写在该 issue 正文里（含「`AUTOCOMPLETE_COMMANDS` 是 `const` 表，
  需要 provider 侧格式化」这条必须先决策的前提）。
* **为什么不直接派 todo**：按本项目前几轮的口径（LUM-1445 §9），**已指派但未开工**
  的 LUM-1434 同样占一个槽位。当前占用 = 自做 1 + 在办 LUM-1448 1 + 待办 LUM-1434 1 = 3，
  已达「最多 3 个同时运行」的上限，所以本轮不再起第四条 run；待 LUM-1434 或 LUM-1448
  收尾后把这条 backlog 提升为 todo 即可（一次状态翻转，验收标准已写全）。
* **不收编**：本轮开工时检查了 `origin` 上所有 `work/*` 与 `agent/*` 分支，
  未合并的只有 LUM-1318/1319/1327/1330/1333/1336 那批（`feature/pi.rs` 上已有
  **等价但不同谱系**的实现：`git grep 'paste #'`、`tool_call` 钩子、`history_search` 均在），
  没有第三条可收编的已完成交付。

## 9. 范围之外

未碰 `pi-protocol` / `pi-ai` / `pi-agent-core` / `pi-extensions` / `pi-session` /
`pi-server` / `pi-client` / `pi-chord` / `pi-evals` / `pi-telemetry` 的源码；
未碰 slash 命令表、扩展事件轴、CI / Docker；未碰上游 TS（`packages/**`，只读取证）。
`settings.rs` 的 `Enter/Space to change` 页脚**明确不改**——上游
`settings-list.ts:321` 自己也是字面量（LUM-1447 §7 第 3 条的提醒），
把它变成可改键会造出 pi-ts 没有的行为。
