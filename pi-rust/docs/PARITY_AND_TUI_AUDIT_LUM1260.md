# LUM-1260 · Rust/TS parity 复核 + TUI 布局缺陷取证

本文件是 LUM-1260 的审计产物，**不改任何 `.rs`**（本轮无构建预算，见 §7）。它做三件事：

1. 复核别人已经写进 `docs/` 的结论，**能推翻的推翻、能精确化的精确化**，并给出可重跑的复现命令；
2. 修掉证据工具本身的**可信度缺陷**（这是本轮最严重的发现，见 §2.1）；
3. 复算 Rust ↔ TS parity，并把旧口径错在哪一条一条列出来（§5）。

| 项 | 值 |
|---|---|
| 审计基准 | `b5769b585`（本轮 `origin/feature/pi.rs` 已推进到 `8ca937893`，两者**生产 `.rs` 完全相同**：`git diff --name-only b5769b585 8ca937893` 只有 1 个测试文件 + docs + scripts + 截图） |
| 被观测二进制 | `artifacts/pi-tip-b5769b585`（54,695,016 B，19:55 构建自 `b5769b585` 的 worktree；行为证据：帧里出现 LUM-1257 的 `↓ Jump to latest message`，该指示器只在 `4b187b418` ⊆ `b5769b585` 之后存在） |
| 二进制可证伪性 | 二进制内**没有**内嵌 commit hash（`strings` 查 `b5769b585` / `64cf58e36` / `LUM-1257` 均为 0 命中），所以上面的溯源靠 worktree 状态 + 行为特征，不是构建指纹。这点如实声明。 |
| 终端尺寸 | 主场景 120×34；短视口对照 120×22 / 120×23 / 120×24 |
| 模型 | `faux/faux-model`（本地脚本化 provider，不触网） |

---

## 1. 复现命令

```bash
# 交互全景（17 帧 + 字符网格 dump）
python3 pi-rust/scripts/pty_capture.py --bin <pi 二进制> \
  --steps pi-rust/scripts/pty_scenarios/lum1260-tip-interaction.json \
  --out /tmp/tip.png --scale 2 --sheet 2

# 短视口（同样的命令换 scenario；22/23/24 三份对照）
python3 pi-rust/scripts/pty_capture.py --bin <pi 二进制> \
  --steps pi-rust/scripts/pty_scenarios/lum1260-small-terminal-23.json --out /tmp/s23.png

# `app.*` id 接线覆盖 + 与启动头提示过滤表双向对账（不需要编译器）
python3 pi-rust/scripts/app_action_coverage.py pi-rust
python3 pi-rust/scripts/app_action_coverage.py pi-rust --check-consumed
```

提交在 `pi-rust/docs/screenshots/` 下的图都带同名 `.txt`，`.txt` 是**字符网格**（含每帧 `frame`/`px` 指纹、光标坐标、`alive=`），图只是同一批网格的渲染结果——本模型没有视觉输入，所有结论都基于 `.txt`，图供人眼复核。

---

## 2. 证据链本身的缺陷（P0，已修）

### 2.1 旧 `pty_capture.py` 的每个面板都渲染的是**最后一帧**（已修）

`render_panel` 在循环结束后才调用，拿到的是**同一个仍被 pty 事件继续改写的** `pyte.Screen` 对象，所以多面板 PNG 里所有面板都是最后一帧的拷贝。实测（修复前）：三面板行程中，面板 1/2/3 的内容区 sha 全等于 `c1b0e2e27184`。

**影响面**：`pi-rust/docs/screenshots/` 里 LUM-1241/LUM-1245/LUM-1257 等**所有多面板 PNG** 都不能再当作"第 N 步是这样的"的证据——图是真的，但每格画的都是终局。`.txt` 网格当时是逐面板正确的（它走的是另一条取值路径），所以基于 `.txt` 写的结论仍然成立；**基于图片面描述步骤的说法全部作废，需要按新图重读**。

**修复**（`pi-rust/scripts/pty_capture.py`）：

- 每帧 `frame = copy.deepcopy(screen)` 冻结后再入卡（`pyte.Screen` 没有 `copy()`，必须 `deepcopy`）；
- 每帧打印 `frame <sha12>` / `px <sha12>` 双指纹（字符网格 + 渲染像素），`.txt` 头部同时记录 `cursor x,y` 与 `alive=`；
- **自检**：相邻面板若字节相同 → 默认 `WARN`；场景里写 `"distinct_panels": true` 则 **`FAIL` 并以 exit 1 拒绝报成功**（本轮两个新场景都开了它）；非相邻同帧 → `WARN` 并点名是哪两格；
- 新增 `child_alive(pid)`（`waitpid(WNOHANG)`）→ 每帧记录进程是否还活着，把"UI 还在"从断言变成机器事实；
- 新增 `--sheet N` 拼版（把 17 格竖排的 ~11k px 长图变成可读网格）。

**这个自检当场就抓到了我自己写错的 caption**：第一版场景第 6 格我写的是"Enter 打开模型选择器"，自检报 `panel [6] 与上一格字节相同` ⇒ 实况是补全列表选中项已被 `<Down>` 移到 `/copy`，`Tab` 把 `/copy` 填进输入框，`Enter` 只是执行了 `/copy`。**没有这个自检，这张图会把"选择器打开了"这个假结论带进审计报告。** 保留此段是为了说明：多面板截图工具如果没有逐帧身份校验，它产出的"证据"是负资产。

### 2.2 `app_action_coverage.py`（新增，无需构建）

度量 `app.*` action 的**消费面**（handler 是否真的按 id 分发），并排除两类假阳性：`keybindings.rs` 里的定义、`locale.rs`/`slash.rs` 里 `/hotkeys` 那种 `("app.x", "描述")` 的广告行。`strip_test_items()` 是括号/字符串/注释感知的，不能像旧脚本那样"截到第一个 `#[cfg(test)]` 就停"——那会在 `interactive.rs` 的树选择器 handler 之前截断，**少算约 17 个 action**（旧口径 21/44 = 47.7% 的死因）。

`--check-consumed` 把实测接线集合与 `pi-tui/src/keybindings.rs` 里手工维护的 `CONSUMED_APP_ACTIONS`（启动头提示行的过滤表）双向对账：

```
CONSUMED_APP_ACTIONS: 35 entries; measured wired: 35
in sync: the header's hint filter matches the code        # exit 0
```

两个独立来源（代码里的分发点 vs 手写清单）互相印证 35/44 = **79.5%**。检测器本身反向测过：伪造一份清单会报 `FALSE AD app.ghost ...` 且 exit 1。

---

## 3. TUI 布局缺陷（P0，本轮实机复现）

### 3.1 视口 ≤23 行时**输入框整个不在屏上**，用户盲打

| 尺寸 | 输入框 | 证据 |
|---|---|---|
| 120×34 | 在 | `lum1260-tip-interaction.png.txt` 第 1 格 `> type a prompt — /help for commands` |
| 120×24 | 在 | `lum1260-small-terminal-24.*`：`> typed blind▍`（含光标） |
| **120×23** | **不在** | `lum1260-small-terminal-23.*`：屏底最后一行是 footer；**第 2 格与第 1 格字节完全相同**（`frame` 同 `656d45d32258`）——即"往输入框里打字"在屏幕上**没有任何可见变化** |
| **120×22** | **不在** | `lum1260-small-terminal.*`：同上，第 2 格与第 1 格同帧 |

边界是 **rows ≥ 24 才有输入框**。旧一轮探针（同一二进制）的尺寸矩阵一致：`100x24` 在、`100x20` 不在、`80x24` 在、`80x30` 在。

**成因**：启动头（标题 + 19 行快捷键提示 + `drop files to attach`）+ onboarding 行 + footer 把 24 行以下的空间吃光，输入框与光标被挤到屏幕外（120×23 时最后一行是 footer，不是输入框，说明它是在**下方**被裁掉，不是被覆盖）。

**对照实现**：Martty（`src/ui.rs:25-33` `composer_height()`：≥15 行给 4 行，≥10 给 3 行，否则 2 行；`:36-54` `resolved_composer_height()` 让输入框随折行增长但 `maximum = (height/2).max(min).min(12)`，永不超过半屏；`:130-145` 输入框带边框卡片，上边框兼作 cap 行、下边框承载 meta 行，所以**不额外占行**，然后 `chat_h = main.height - (composer_h + ...)`——**输入框固定、transcript 让位**，与 pi-rust 现在的行为正好相反）。Martty 还**主动删掉了快捷键提示行**（`src/ui.rs:106` 只保留 `terminal too small — need ≥ 24x6` 的守卫，提示靠 tip banner + `/keys`）：pi-rust 那 20 行启动头正是 Martty 明确放弃的反模式。

**建议修法**（需构建验证，本轮不落码）：给 transcript 高度设下限，先折叠启动头（`Alt+H` 已有 `app.header` 语义，可在此基础上把"行数不足自动收起"接上），再加 `terminal too small` 守卫而不是把输入框裁掉。

### 3.2 `/help` 每行正好 120 列 → 最后一格被滚动条列覆盖

`lum1260-tip-interaction.png.txt` 第 15 格（120×34，transcript 已溢出因此滚动条可见）：

```
120|· /extensions list loaded extensions and what they register /exit quit the interactive session keys: Enter submit promp┃
120|· Up / Down navigate prompt history PgUp/PgDn scroll the chat log one page Home / End jump to the start / end of the   ┃
```

- 每一行都是**恰好 120 个字符**（终端全宽），没有为滚动条列留位；
- 滚动条列 `column = origin_x + width - 1`（`pi-rust/crates/pi-tui/src/app.rs:3373`）在文本渲染**之后**覆盖写入（`app.rs:3122-3125` 画 `┃`/`│`），于是每行**最后一个字符被吃掉**：`submit promp┃` 的 `t`、`end of the   ┃` 的尾部都消失了；
- 宽度预算处只减了角色前缀 2 格：`text_width_for(width) = width - 2`（`pi-rust/crates/pi-tui/src/message.rs:1400`），没有减滚动条。
- 折行还会在**每个续行重新加 `· `**（`message.rs:1120` 的 `Role::Info` 前缀 + `plain_lines` 逐行 prefix），所以一段连续文字看起来像一串列表项——这是同一处的视觉副作用。

上游同款风险：`packages/tui/src/layout.ts:296` 也用 `box.rect.x + box.rect.width - 1` 作滚动条列，说明这个"文字吃到最后一列"的约定可能有上游来源；但上游会**为覆盖层**按滚动条列收窄宽度（`packages/tui/src/tui-alt-screen.ts:1626` `availableWidth = scrollbarColumn - clip.x`）。因此在给 Rust 打补丁前应先确认上游 transcript 正文是否也全宽——**本条按"Rust 侧实测缺陷 + 上游待核"报告，不宣称上游一定正常**。

LUM-1259 的标题写"`/help` 只修了一半"，本帧支持这个判断：`Role::Info` 的 `· ` 前缀修好了（不再伪装成用户输入），但**行宽预算与折行前缀**没修。

---

## 4. 输入面复核（结论：一条推翻旧说法、一条精确化）

### 4.1 Ctrl+C：窗口 ≈500 ms —— LUM-1238 §12.4 item 1 已不再复现

同一二进制、同一 PTY 驱动，逐次测量（`alive` 由 `waitpid(WNOHANG)` 判定）：

| 动作 | 结果 |
|---|---|
| 有草稿时按 1 次 `<C-c>` | 草稿清空回 placeholder，**`alive=True`**；随后输入 `Z` 能落到输入框 |
| 背靠背 `<C-c><C-c>` | **`alive=False`**（进程退出） |
| 两次 `<C-c>` 间隔 | 0.05 / 0.2 / 0.35 s → 退出；**0.5 / 0.7 / 1.0 / 2.0 s → 存活** |

⇒ 语义是"**单次 = 清空草稿**，500 ms 内两次 = 退出"，启动头里那句 `Ctrl+C twice to exit` 是**准确的**。这与 LUM-1259 在新 tip 上的修正一致（其场景 JSON 的 caption 亦写"#1 clears the draft and stays alive, #2 inside 500ms exits"），本轮补上的是**窗口期 bisect**（0.35 s 退 / 0.5 s 不退）。旧的"单次 Ctrl+C 会丢草稿但并不退出/无窗口"的说法在本二进制上不成立。

顺带：LUM-1259 为此单独写了一个 `scripts/pty_probe_ctrl_c.py`（因为"`pty_capture.py` 只能渲染，不能回答进程是否退出"）。本轮已把 `alive=` 做进渲染工具，**这个独立探针不再是必需**——两个工具的结论一致，说明口径收敛。

### 4.2 斜杠补全的匹配面比上游宽：输入 `/mo` 出 8 个候选

第 3 格实况（120×34，`/mo`）：

```
❯ model  <provider/model> — Select model (opens selector UI)
  copy  Copy last agent message to clipboard
  thinking  off|minimal|low|medium|high|xhigh|max — Set the reasoning level
  export  [path] — Export session (HTML default, or a .jsonl path)
  fork  Create a new fork from a previous user message
  (1/8)
```

- 上游只按**命令名**过滤：`packages/tui/src/autocomplete.ts:330` `fuzzyFilter(commandItems, prefix, (item) => item.name)`，按名匹配 `mo` 只有 `/model` 一个候选；
- Rust 侧 `pi-rust/crates/pi-tui/src/autocomplete.rs:434-437` 自己写明"Upstream fuzzy-filters on the command name; this port matches the name plus description (a superset)"，`search_text = "{name} {description}"`（`:451-453`）——于是描述里含 m…o 子序列的 `copy`/`thinking`/`export`/`fork` 全部混进候选；
- **用户可见后果**：`mo` → 8 条候选而不是 1 条；`<Down>` 会把选中项移到与"mo"无关的 `/copy`（这正是自检抓到的那格）。这不是"实现 bug"而是**刻意 superset**，但 UX 代价可测，属于低成本收敛项（只按名匹配、描述仍照旧展示）。

---

## 5. parity 复算与口径修正

| 轴 | 权重 | 旧口径 | 本轮实测 | 说明 |
|---|---|---|---|---|
| 规模（LOC） | — | 82.5% | **83.5%**（127,908 / 153,106） | 同一算法复测 |
| 测试用例 | 5% | 45%（2,232 vs 5,309） | **44.0%**（2,338 vs 5,309） | ts 侧分母不变 |
| TUI 交互面 | 14% | 58%（= 80.5% 模块 / 47.7% 接线） | **79.5%**（35/44 id 接线，模块数不定百分比） | 旧 47.7% 是 `strip_test_items` 在 `interactive.rs` 里**过早截断**导致的少算（见 §2.2）；模块数两侧文件划分粒度不可比（Rust 33 个 `pi-tui/src/*.rs` vs TS 24 + 19 个组件），故只报原始数、不再折算百分比 |
| slash 命令面 | 7% | 74%（17/23） | **69.6%**（有效 16/23） | 旧值把别名与广告行算了进去：`AUTOCOMPLETE_COMMANDS` 只有 19 个规范名，`exit/quit` 归并为 1，与 TS 的 23 个名字交集有效为 16 |
| 扩展生命周期事件 | 7% | 20%（7/36 声明、2/36 运行期） | **58.3%**（21/36 可发射） | 口径差在"谁算生产者"：旧值只数 `pi-extensions` 里的调用点，本轮数**任意生产代码**（`extensions/events.rs`、`wiring.rs`、`interactive.rs` 等），22 个变体里 21 个有真实构造点（唯一死变体是 `ModelSelect`）。**注意**：这 21 个是"名义覆盖"，缺的 15 个恰好含插件最常用的 `tool_call`/`tool_result`/`before_agent_start`/`context` 等，**按使用频率加权仍很差** |
| 其他 8 轴 | 51% | 见 `RUST_TS_PARITY_METRICS.md` §4.1 | 沿用 | 本轮未改判 |

加权和 = 5×1.00 + 13×0.90 + 8×1.00 + 6×0.70 + **14×0.795** + 8×0.90 + **7×0.696** + 7×0.70 + 8×0.95 + **7×0.583** + 9×0.85 + 5×0.44 + 3×0.95 = **81.4%**

三点如实声明：

1. **这个数比旧的 76.05% 高，是因为旧的两条轴算错了，不是因为功能变多**（改动方向一条升、一条降，最大一项 +2.7pt 来自"扩展事件"的生产者口径）。两条轴我都标了口径变更，读者可以按旧口径重算回 76%。
2. **TUI 完成度**取哪个口径说哪个：id 接线 **79.5%**；界面缺口是**具体组件缺失**而非百分比——上游有 `ScopedModelsSelectorComponent`（`packages/coding-agent/src/modes/interactive/components/scoped-models-selector.ts`，`:352/:206/:298/:207/:139/:208` 调 `app.models.toggleProvider/reorderUp/reorderDown/save`）与 `app.tree.editLabel`（`components/tree-selector.ts:1084/:1222`，`core/keybindings.ts:38` 定义），Rust 侧只有键位定义、没有组件也没有 `/scoped-models`。要一个 TUI 交互完成度数字，我给 **≈78%（判断值，非测量值）**，测量值只有 79.5% 这一条。
3. 三个数同时成立：**代码搬了 83.5%、测了 44.0%、按功能加权能用 81.4%**。

---

## 6. 缺口优先级（连同两侧 file:line）

| 级别 | 缺口 | 证据 | 修法要点 |
|---|---|---|---|
| P0 | ≤23 行输入框不在屏上（§3.1） | 120×22/23/24 三帧对照 | transcript 高度下限 + 启动头自动收起 + `terminal too small` 守卫 |
| P0 | `/help` 每行最后一格被滚动条列吃掉（§3.2） | 第 15 格 `.txt` | 滚动条可见时 `text_width` 减 1；先核对上游 `layout.ts:296` 是否也全宽 |
| P0 | 多面板截图证据不可信（§2.1） | 修复前后 sha 对比 | 已修（冻结帧 + `alive=` + `distinct_panels` 自检），存量图需重拍 |
| P1 | `/scoped-models` + 6 个 `app.models.*` 全未接线 | `app_action_coverage.py` 报 6 个 `silent`；上游组件存在 | 一次性做"命令 + 组件 + 键位消费"，接线 35/44 → 43/44（97.7%），slash 16/23 → 17/23 |
| P1 | `app.tree.editLabel` 未接线 | 同上（silent）；上游 `tree-selector.ts:1084` | 树选择器加 label 编辑态 |
| P1 | 扩展事件缺 15 个上游名字 | §5 轴 10 | 上游最常用的 `tool_call`/`tool_result`/`before_agent_start`/`context` 优先 |
| P2 | slash 缺 `changelog/import/login/logout/reload/scoped-models/share` | `commands/slash.rs:203` 与 `core/slash-commands.ts` 对照 | 与 P1 的 `scoped-models` 合并做 |
| P2 | 斜杠补全匹配面过宽（§4.2） | 第 3 格 + `autocomplete.rs:434` | 只按 name 匹配 |

---

## 7. 本轮做不到的事（不落未验证代码）

- **没有构建预算**：`/` 使用率 95%（余 2.5–2.7 GB），同机已有 4 个并发构建进程，`target/` 目录吃掉数 GB。本轮**未运行 `cargo build` / `cargo test`**，因此 §3 的两个 P0 修法只以"补丁要点 + 复现命令"给出，**不进代码**。按仓库规则"无法验证就不落码"，它们应作为 backlog 子任务在有构建预算时落地并配 PTY 复拍。
- 本轮唯一改动的非 `.rs` 文件：`pi-rust/scripts/pty_capture.py`（工具）、新增 `pi-rust/scripts/app_action_coverage.py`、4 个 scenario JSON、截图与 `.txt`、本文件。`git status` 里没有任何 `.rs`。
- `git merge-tree --write-tree origin/feature/pi.rs 8ca937893` 类检查用的都是只读命令；本轮不改他人 worktree（LUM-1256/LUM-1258 正在编辑 `app.rs` / `keybindings.rs` / `pty_capture.py`），因此**本轮对 `pty_capture.py` 的改写与 LUM-1258 未提交的改动会冲突**——这个冲突是必要代价（他们要动的是同一函数区），已在合并说明里点出。
