# LUM-1274 — `/scoped-models` 面板：把最后 6 个 silent 的 `app.*` 接线，并复测完成度

本轮主题：**在既有清单里挑回报最高的一格真做**（不是再写一份审计），顺手修掉一个被 PTY 抓到的真缺陷。

一句话结论：`/scoped-models`（上游 `ScopedModelsSelectorComponent`）在 Rust 侧落地，`app.models.*` 六个
chord 从「绑定了但没有任何代码路径 resolve」变成真功能 —— `app.*` 接线率 **37/44 → 43/44（97.7%）**，
只剩 `app.tree.editLabel` 一个 silent；同时把面板 footer 从「一条 124 字符的行」拆成 4 条短行，
修掉 120 列终端下 `(unsaved)` 被裁掉的缺陷。两条真 PTY 场景（9 格 + 4 格）**40/40 断言通过**。

---

## 1. 缺陷：六个 chord 是「哑的」

`pi-rust/scripts/app_action_coverage.py` 把 44 个 `app.*` 动作分成 wired / advertised / silent。
LUM-1308 收尾时是：

```text
wired: 37/44 (84.1%)   advertised: 0/44 (0.0%)   silent: 7/44 (15.9%)
  app.models.clearAll  app.models.enableAll  app.models.reorderDown  app.models.reorderUp
  app.models.save      app.models.toggleProvider      app.tree.editLabel
```

这 7 个里有 6 个在同一张上游面板上（`packages/coding-agent/src/modes/interactive/components/scoped-models-selector.ts`），
而 Rust 侧连这张面板本身都没有：`/scoped-models` 不在 parser、不在 `help_text()`、不在自动补全表里。
这跟「上游也没有」不是一回事 —— 上游 6 个 chord 全部有消费者（`scoped-models-selector.ts:296-401`），
而且 `settings.json` 的 `enabledModels` 在 TS 里会影响 `Ctrl+P` 的循环范围（`main.ts:448`
把 `enabledModels` 解析进 `session.scopedModels`）。

## 2. 实现

### 2.1 新模块 `crates/pi-coding-agent/src/scoped_models.rs`（814 行 / 25 个单测）

上游是「一个 Component 自己拥有整张列表」；本 port 沿用既有的 `/resume`、`/tree` 拆分方式
（状态机在 driver 里、渲染走共享的 `pi_tui::selector::Selector`），因此这个模块只做**状态机 + 渲染数据**：

| 上游 | Rust | 说明 |
|---|---|---|
| `EnabledIds = string[] \| null` | `Option<Vec<String>>`（`EnabledModels`） | `None` = 全部启用、无过滤 |
| `isEnabled` `:17` | `is_enabled` `:84` | |
| `normalizeEnabled` `:22` | `normalize_enabled` `:93` | 勾满时收敛回 `None`（不会存 192 个 id） |
| `toggle` `:26` | `toggle` `:104` | 从 `None` 出发 = 把该 model 摘掉 |
| `enableAll` / `clearAll` `:33,45` | `enable_all` `:129` / `clear_all` `:153` | `None` = 无搜索过滤 → 目标是整个 catalog；`Some` = 只动过滤后的行（可以是空集） |
| `move` `:55` | `move_model` `:184` | 越界/未启用 = 原样返回 |
| `getSortedIds` `:65` | `sorted_ids` `:204` | 已启用按**显式顺序**在前，其余按 catalog 顺序 |
| `isDirty` + `onPersist` | `dirty` / `mark_saved` | 改动只在 `Ctrl+S` 落盘 |
| `getFooterText` `:194` | `footer` `:339` | 见 §3 |

`updateSessionModels`（上游 `interactive-mode.ts:5042-5062`）的三条 `None` 分支逐条对到
`ScopedModelsPanel::cycle_scope()` `:305`：**没有显式 scope** / **scope 为空** /
**catalog 里每个 model 都在 scope 里** → 都返回 `None`（循环退回整个 catalog）；另外
「scope 里的 id 全都不在 catalog 里」也返回 `None`，否则 `Ctrl+P` 会变成一个按下去没反应的死键。

### 2.2 接线（`crates/pi-coding-agent/src/interactive.rs`）

| 位置 | 作用 |
|---|---|
| `open_scoped_models_selector` `:1518` | `/scoped-models` 打开面板；面板对象只建一次（上游组件整个 session 存活），每次打开刷新 catalog |
| `seed_model_scope` `:1541` | 启动时读 `settings.json#enabledModels` 播种 scope（对齐 `main.ts:448`），**不打开面板也生效** |
| `refresh_scoped_models_selector` `:1560` | 改动后原地重建：搜索词与光标跟随被操作的那一行（上游保留 `selectedIndex`） |
| `scoped_row_index` `:1590` | 在**过滤后**的视图里定位某一行：用 `pi_tui::fuzzy::fuzzy_rank` 重算同一顺序（`Selector::refilter` 用的就是它） |
| `PickerKind::ScopedModels` + `handle_picker_key` `:2130` | 6 个 chord 的消费者；`Enter` 在这里被认领成 toggle（不关面板），`Ctrl+S` 写盘后刷新 |
| `cycle_catalog` `:1638` / `cycle_model` `:1667` | `Ctrl+P` 走 scope（保留 scope 顺序 —— 这正是 reorder 编辑的东西）；只有 1 个 model 时按上游 `interactive-mode.ts:4192` 的措辞区分 `only one model in scope` |
| `PickerState::scoped_models` | 面板状态与 `/resume`、`/tree` 的视图状态同级，活在 selector 之外 |

`crates/pi-coding-agent/src/commands/slash.rs`：`SlashCommand::ScopedModels` `:31`、parser `:116`、
`/help` 行、自动补全表（测试 `autocomplete_commands_match_the_parser_exactly` 钉住两端）。

### 2.3 广告面

`crates/pi-tui/src/keybindings.rs:436` 把 6 个 id 加进 `CONSUMED_APP_ACTIONS`；相应地
`crates/pi-coding-agent/tests/startup_header.rs:97` 把它们加进 `SELECTOR_SCOPED`（只在该面板
打开时有意义，所以 `/hotkeys` 的 app 组**不**印它们 —— 上游的 hotkeys 表也没有这 6 行，
它只在面板自己的 footer 里提示）。脚本自检：

```text
$ python3 pi-rust/scripts/app_action_coverage.py --check-consumed
CONSUMED_APP_ACTIONS: 43 entries; measured wired: 43
in sync: the header's hint filter matches the code        # exit 0
```

### 2.4 `settings.json` 读写（`crates/pi-coding-agent/src/config.rs`）

| 函数 | 对齐上游 | 行为 |
|---|---|---|
| `load_enabled_models` `:262` | `getEnabledModels` `:1315` | 非字符串元素逐条告警跳过；整个值不是数组 → 告警 + 当「无 scope」；空数组 → 当「无 scope」 |
| `save_enabled_models` `:303` | `setEnabledModels` `:1324` | 写 `enabledModels`；`None`（= 全启用）**删键**而不是写 `null`（`JSON.stringify` 丢 `undefined` 的语义） |
| `remove_user_setting` `:541` / `remove_path` `:569` | 同上 | 只动 user 文件、原子替换、其它键逐字保留；文件不存在时删键是 no-op（不创建文件） |

上游的 `enabledModels` 是**模式**（`provider/*`、`*sonnet*`、可选 `:thinkingLevel` 后缀），
Rust 侧只认字面 id：不能解析的模式会当成「unavailable」行留在面板里（上游 `a3ee1d286`
也是这个做法）。**这是本轮的真实收敛，写进 §7。**

## 3. 顺带修掉的真缺陷：footer 被裁

第一版实现照上游把 footer 拼成**一条**长行：

```text
  Enter toggle · Ctrl+A all · Ctrl+X clear · Ctrl+P provider · Alt+Up / Alt+Down reorder · Ctrl+S save · 191/192 enabled (unsaved)
```

124 字符 > 120 列 → `Selector` 的 footer 不换行，尾部被裁，**`(unsaved)` 与计数一起消失**，
即「改了但用户看不到未保存提示」。PTY 第一轮就是靠断言 `(unsaved)` FAIL 抓到的。

改法：同样的信息拆成 4 条短行（`scoped_models.rs:339`），并加单测断言每行 < 80 列
（`panel_footer_reports_the_count_and_dirty_state`），保证 80×24 默认终端也不再裁：

```text
  Enter toggle · Ctrl+A enable all · Ctrl+X clear all
  Ctrl+P toggle provider · Alt+Up / Alt+Down reorder · Ctrl+S save
  Session-only. Ctrl+S to save to settings.
  2/192 enabled (unsaved)
```

## 4. 门禁实况（1.85.0，`--locked`）

```bash
. pi-rust/scripts/toolchain.sh        # 或 export PATH=…/toolchains/1.85.0-x86_64-unknown-linux-gnu/bin:$PATH
cd pi-rust
cargo fmt --all -- --check                                    # 无 diff
cargo clippy --workspace --all-targets --locked -- -D warnings # 0 findings，exit 0
cargo test --workspace --locked                                # exit 0
```

| 门禁 | 结果 |
|---|---|
| `cargo fmt --all -- --check` | **PASS**（无 diff） |
| `cargo clippy --workspace --all-targets -- -D warnings` | **PASS**（0 findings，exit 0） |
| `cargo test --workspace --locked` | **2506 passed / 0 failed / 2 ignored**（162 套件）|

上轮基线 2469 → **+37**（`scoped_models` 25 + 面板和弦/cycle/seed 8 + `config` 4 = 37）。

## 5. 证据（真 PTY，`pyte` 逐字节渲染）

### 5.1 `lum1274-scoped-models.png` — 9 格 / **28 断言全 PASS**

```bash
python3 pi-rust/scripts/pty_capture.py --bin pi-rust/target/debug/pi \
  --home /tmp/pi-pty-home-lum1274 \
  --steps pi-rust/scripts/pty_scenarios/lum1274-scoped-models.json \
  --out pi-rust/docs/screenshots/lum1274-scoped-models.png
# assertions: 28 checks over 9 panels — 28 PASS, 0 FAIL, 0 XFAIL, 0 XPASS
```

| 格 | 按键 | 断言证明的事 |
|---|---|---|
| 1 | — | 出货启动头（本轮未改） |
| 2 | `Alt+H` + `/scoped-models` | 面板标题 / 勾选行 + provider 列 / 4 行 footer 全文可见；**192 个 model** 的真实 catalog |
| 3 | `Enter` | `191/192 enabled (unsaved)` —— `tui.select.confirm` 变成 toggle，面板不关 |
| 4 | `Ctrl+X` | `0/192 enabled`（`app.models.clearAll`） |
| 5 | `Ctrl+A` | 回到 `all enabled`（`app.models.enableAll` 收敛成 `None`） |
| 6 | `Ctrl+P` | `189/192 enabled` —— 整条 ant-ling provider（3 个 model）一次关掉 |
| 7 | `Ctrl+X` + 两行 `Enter` | `2/192 enabled`，且**两行顺序**被两行文本 needle 钉住 |
| 8 | `Alt+Down` | 同一窗口里两行**换位**（needle + `(2/192)` 计数一起变）—— `app.models.reorderDown` |
| 9 | `Ctrl+S` | `Model selection saved to settings` 且 `(unsaved)` 消失 |

> 第 7-8 格是 reorder 的 A/B：`app.models.reorderUp/Down` 默认绑 `alt+up`/`alt+down`。
> harness 为此加了 `Alt` 键位支持：`<M-a>..<M-z>` 用 `ESC+字符`、`<M-Arrow>` 用 xterm 的
> CSI 修饰形式 `ESC [ 1 ; 3 A`。**`ESC ESC [ A` 这种老写法不能用** —— 第一个 `ESC` 会被
> 单独投递、先把面板关掉（本轮实测踩到，已写进 `pty_capture.py:133` 的注释）。

Ctrl+S 落到哪儿了？第 1 轮 capture 跑在保留的 HOME 上，写盘结果可以直接看：

```bash
$ cat /tmp/pi-pty-home-lum1274/.pi/agent/settings.json
{
  "enabledModels": [
    "ant-ling/Ling-2.6-flash",
    "ant-ling/Ling-2.6-1T"
  ]
}
```

顺序正是第 8 格 `Alt+Down` 之后的结果 —— 落盘的是**改动后的 scope**，不是初始值。

### 5.2 `lum1274-scoped-models-seed.png` — 4 格 / **12 断言全 PASS**（读回）

第二个 scenario 用**上一轮写出的那个 settings.json** 当子进程的 HOME（`--home` 保留目录），
于是「重启后 scope 生效」这条链路可以端到端证：

```text
[1] /scoped-models        → 2/192 enabled（无 (unsaved)、无 all enabled）+ 保存时的行序  ✔
[2] Esc                   → 面板关闭、composer 回来                                  ✔
[3] Ctrl+P                → model → ant-ling/Ling 2.6 Flash（scope 的第一项）          ✔
[4] Ctrl+P                → model → ant-ling/Ling 2.6 1T（**在 scope 内回绕**）         ✔
    reject: Ring-2.6-1T / anthropic/ —— 全 catalog 循环会走到它们，scoped 不会      ✔
```

### 5.3 与 Codex / pi TUI 的交互形态对照

同一批截图能直接对照的三点（都是本轮新拍的画面，不是描述）：

| 交互 | 本轮实测画面 | Codex / pi-ts |
|---|---|---|
| 启动头 + 键位提示块 | 第 1 格：20 行 hint（`Ctrl+Z to suspend` … `drop files to attach`）+ 一行产品说明 | pi-ts 同形态（`STARTUP_HINTS` 逐条对齐），Codex 也是「banner + hint 列表」 |
| 模态选择器（列表 + 搜索 + footer 图例） | 第 2 格：标题 / `─` 分隔 / 光标 `❯` / 勾选列 / provider 描述列 / `(1/192)` 滚动计数 / 4 行图例 | Codex 的 `/model` 是同类模态列表；pi-ts 面板更接近（有 `(unsaved)` 与 provider 列） |
| 单键状态反馈（未保存 / 计数 / 提示） | 第 3、4、6、9 格：footer 计数与 `(unsaved)` 随 chord 实时变化 | pi-ts 用 `theme.fg("warning","(unsaved)")` 同一语义 |

**没做到的**（§7 会重复）：面板在矮视口（启动头未折叠）下会被裁 —— 见 §7 第一条。

## 6. 完成度复测（每条都附可复现命令）

### 6.1 规模

```bash
find pi-rust/crates -path '*/src/*' -name '*.rs' ! -name 'mod.rs' | xargs wc -l | tail -1   # 131,410
find packages -path '*/src/*' -name '*.ts' ! -name '*.d.ts' | xargs wc -l | tail -1          # 153,066
```

**src↔src = 131,410 / 153,066 ≈ 85.9%**（上轮 84.8%，+1,608 行为本轮新增）。
含全部 `tests/` 的口径：183,951 / 318,969 ≈ **57.7%**。

### 6.2 测试规模

```bash
grep -rn "#\[test\]\|#\[tokio::test\]" pi-rust/crates --include=*.rs | wc -l         # 2,439（源内标记；实跑 2,506 个 case）
find packages -name '*.test.ts' ! -path '*/node_modules/*' | wc -l                   # 544 个测试文件
grep -rn "^\s*it(\|^\s*test(" packages --include=*.test.ts | wc -l                  # 5,299 个 case
```

**2,506 / 5,299 = 47.3%**（上轮口径 45.7%）。TUI 交互路径仍然主要靠 PTY 截图，回归网比 TS 薄。

### 6.3 `app.*` 接线

```bash
python3 pi-rust/scripts/app_action_coverage.py
# wired: 43/44 (97.7%)   advertised: 0/44 (0.0%)   silent: 1/44 (2.3%)  → app.tree.editLabel
```

### 6.4 扩展生命周期事件（仍是最大开放轴）

```bash
python3 pi-rust/scripts/extension_event_coverage.py
# wire tags matching upstream: 21/36 (58.3%)   production emit sites: 20/36 (55.6%)   missing: 15/36
```

### 6.5 slash 内置命令：**18 / 23**

新增 `/scoped-models`；仍缺上游的 `import share changelog login logout`（后两个是 OAuth/账号面）。

### 6.6 加权完成度（沿用 LUM-1293/1298 的 13 轴口径，逐格重测）

| 轴 | pi-rust | pi-ts | 本轮变化 |
|---|---|---|---|
| 核心 Agent 循环 | 100% | 100% | — |
| LLM provider（主路径） | 100% | 100% | — |
| 内置工具（read/write/edit/bash/find/grep/ls） | 7/8 | 7/8 | — |
| Slash 命令 | **18/23 = 78.3%** | 23 | **+1（/scoped-models）** |
| 扩展生命周期事件 | 20/36 = 55.6% | 36 | —（重测，未变） |
| 会话存储（SQLite / zstd） | 100% | 100% | — |
| TUI 渲染（Markdown + 高亮 + 主题 + 图片 + LaTeX） | 100%（33,090 行 `pi-tui/src`） | 100% | — |
| `app.*` 键位 | **43/44 = 97.7%** | 43/43 | **+6（本轮面板）** |
| 富工具渲染器 | 100% | 100% | — |
| TUI 鼠标 / 拖拽 / 选词 | 100% | 100% | — |
| TUI 截断提示 | 100% | 100% | — |
| TUI jump-to-latest | 100% | 100% | — |
| OAuth / 远程模型目录 | 0% | 100% | — |

**13 轴算术平均 = 86.1%**（上轮 82.0%）。两个轴的跳变全部来自本轮：
`app.*` 84.1% → 97.7%（+13.6 格）、slash 17/23 → 18/23（+4.3 格）。

> 口径说明：13 轴里 9 条是「有没有」（0/100/部分），只有 4 条是脚本测出来的比例；
> 这个 86.1% **不是**「代码写完了多少」的度量，只是同一张表的历史可比数字。
> 真实的差距看 §6.2（测试网 47.3%）与 §6.4（扩展事件 55.6%）。

## 7. Martty 对照（真实数字，`github.com/louloulin/Martty` @ `93e9231` v0.2.17）

Martty 是单体 crate 的 ratatui TUI（`deepseek-harness-tui`），34 个 `.rs` / **33,687 行**，
依赖面 `ratatui 0.30.2 + crossterm 0.29 + tui-markdown + agent-client-protocol`。

| 维度 | pi-rust `pi-tui` | Martty |
|---|---|---|
| 规模 | 33,090 行 / 33 模块（含 markdown 1,950、highlight 2,574、latex 1,856、terminal_image 1,471 全是自己写的） | 33,687 行 / 34 文件，markdown 交给 `tui-markdown` |
| 键位架构 | 分散在 `App::step_key` + driver 的 `handle_input_event` + `CONSUMED_APP_ACTIONS` 三处（本轮又验证了一遍这个成本） | `src/input/keymap.rs` 一张**纯函数** `KeyEvent → Action` 表，无 `App` 依赖，一处可测 |
| 视图扩展 | 扩展 UI 桥（`extension_ui.rs` 894 行）+ 自定义 footer/editor 组件 | `src/slots.rs` 组合器 + Cordis 客户端插件树（theme / view / command 都是插件） |
| 附件 | composer 图片 chip（LUM-1224，退格整块删） | `attachments.rs`：`[image N]` **token 就在草稿文本里**，chip 是渲染，光标/退格/kill/历史都按普通文本处理 |
| CJK 排版 | `markdown.rs` 自己按宽度 wrap（含 CJK 宽度处理） | 拉丁/中日韩**双色**正文 + 列表悬挂缩进 + 代码块边框 + `---` 画成整行 `─` |
| 主题 | `theme.rs` 1,811 行，内置 dark/light + `/settings` 切换 + 热重载 | `theme.rs` 从 Web 端 design token 1:1 映射，语义色（brand/hint/success/attention/error） |

**值得抄的三件**（不是「风格」而是能减少真缺陷的）：
1. **单一 keymap 纯函数**。本轮修 footer 之前，`Ctrl+S` 在面板里到底归谁处理要读三处代码；
   Martty 那种一张表能让「绑了没人管」这类缺陷在编译期就少一半。
2. **附件 token 化**。`[image N]` 活在草稿里，光标移动/历史召回/粘贴天然正确；pi-rust 的 chip
   是独立于文本的模型，等价行为要单独实现（LUM-1224 就写了一整轮）。
3. **`slots` 式视图扩展**。pi-rust 现在的扩展只能换 footer/editor 组件 + 弹对话框；
   theme/view/command 级插槽是上游 pi 的 extension UI 能力，Rust 侧只做到 `ctx.ui.*`。

**Martty 没有而 pi-rust 有的**：`/tree` 分支树、`/fork`、SQLite v4 会话、HTML 导出、
终端图片协议（kitty/iTerm2）、LaTeX、扩展生命周期事件、`app.*` 键位表。

## 8. 已知限制（照实说）

1. **矮视口下选择器会被裁**（本轮实测）：`App` 把 selector 裁剪到 message viewport
   （`app.rs:4846-4866`），启动头 20 行时面板只剩 ~7 行，footer 完全看不到。所以
   §5.1 第 2 格先按了 `Alt+H`。这不是本轮引入的，是已立案的 P0（LUM-1262）。
2. **`enabledModels` 只认字面 id**：上游的 glob（`provider/*`、`*sonnet*`）与 `:thinkingLevel`
   后缀没有实现（`resolveModelScopeFromModels` / `minimatch` 未移植）。面板里这类条目会显示成
   unavailable 行，且 `Ctrl+P` 不会把它当 scope。
3. **`app.models.save` 只写 user 文件**：项目级 `.pi/settings.json` 不写（上游同样只改 global）。
4. **`app.tree.editLabel` 仍是唯一 silent 的 `app.*`**（`/tree` 的 label 编辑），本轮未动。
5. **PTY 断言仍是「人看 PNG + 断言文本」**：场景文件可复现、断言自动跑，但还没有进 CI
   （`scripts/ci.sh` 目前只跑 fmt/clippy/test）。

## 9. 下一步（建议 ≤3，且不重复已立案的）

1. **P0 已立案，优先认领它**：LUM-1262「短视口布局」（≤23 行输入框被裁 + selector 被裁）——
   本轮又给它加了一条独立证据（面板 footer 在 120×34 + 展开启动头时不可见）。
2. **`app.tree.editLabel` + `/tree` label 编辑**（最后一个 silent 的 `app.*` id，回报小但闭合面干净）。
3. **扩展事件第二批**（`before_agent_start` / `context` / `ui_prompt_*` 等，55.6% → 70%+），
   已立案在 LUM-1295；把 `extension_event_coverage.py` 接进 `scripts/ci.sh` 可让这条轴不再回退。

派发判断：本轮**未新增** stage —— LUM-1262（P0）、LUM-1295/1290（扩展事件）、LUM-1263（本轮的
`/scoped-models`，已被本轮实现）都在既有清单里，再派只会重复付费。
