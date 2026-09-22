# Rust ↔ TypeScript 差距量化审计（真实百分比）

> 基准快照：`origin/feature/pi.rs` = `af3aa30a4`（本文所有数字都在该 tip 的独立 worktree 上实测）
> 环境：Linux x86_64 / 32 核 / cargo+rustc 1.98.1 / 离线构建（`--offline`）
> 本文只做**测量**，不改代码；所有命令都可复现，误差来源与不可测项在 §1.3 明确列出。

## 0. 结论速览

| 口径 | 数值 | 含义 |
|---|---|---|
| 纯代码规模 | **82.5%** | Rust `src` 124,242 行 / TS `src` 150,538 行 |
| 测试规模 | **42.0%** | Rust 2,232 个 `#[test]` / TS 5,309 个用例 |
| 功能面加权（本审计主口径） | **76.1%** | 13 个功能轴按权重加权，权重为主观赋值，公式见 §4 |
| TUI 交互+视觉（本 issue 关注面） | **69.6%** | 上述权重中 TUI 相关的 22% |

一句话结论：**pi-rust 不是"差不多完成了"，它已经是一个能跑、能测、能交互的完整实现（核心循环 + 5 个 API family + 7 个内置工具 + 真 TUI 全部可用）；差距集中在两个地方——TUI 快捷键/组件的接线率，和扩展生态的生命周期事件。** 移除/补上扩展事件这一轴，加权值会从 76.1% 跳到 81.7%（见 §4.2 敏感性）。

> 本表是 `af3aa30a4` 的快照。之后三条完整交付被抢救合并（LUM-1256），**当前 tip 的复测数字见 §0.1**（加权 **79.9%**）。

### 0.1 合并后复测（LUM-1256，tip `d90555fb6`）

上一快照之后有三条**已经写完但长期未合并**的交付一直漂在各自的 `work/*` 分支上：
Stage 68 扩展生命周期事件（LUM-1246）、Stage 71 扩展可见性（LUM-1239）、Stage 70 输入面收尾（LUM-1238）。
本 round 把三条并入 `feature/pi.rs` 并重测（合并细节、合并期发现的真实缺陷见
`docs/TUI_UX_AUDIT.md` 二十一）。同时 `origin/feature/pi.rs` 上还并行落了一个 LUM-1257
（jump-to-latest 指示器），合并时它与我这边救回来的 LUM-1238 版本撞了同一个功能，保留的是
已推送到远端的 LUM-1257 版（详见 `docs/TUI_UX_AUDIT.md` 二十一.2 第 5 条）。

| 口径 | 旧快照 | 本 tip | 变化 |
|---|---|---|---|
| 纯代码规模 | 82.5% | **82.1%** | Rust `src` 125,652 行 / TS 153,106 行 |
| 测试规模 | 42.0% | **43.4%** | Rust 2,303 个 `#[test]` / TS 5,309 个用例 |
| 功能面加权（主口径） | 76.1% | **79.9%** | 三条轴变动，见下表 |
| TUI 交互+视觉 | 69.6% | **73.8%** | (14×0.64 + 8×0.91) / 22 |

变动的三条轴：

| 轴 | 旧 | 新 | 依据（可复现） |
|---|---|---|---|
| 扩展生命周期事件（权重 7%） | 20% | **57%** | 枚举 tag 对齐上游 **21/35 = 60%**（`pi-protocol/src/events.rs` 的 `ExtensionEvent::name()`）；生产代码真有构造点 **20/35 = 57.1%**（`grep -rho 'ExtensionEvent::[A-Za-z]*' crates --include=*.rs` 去掉测试后 21 个变体，其中 `UserMessage` 是 Rust 原生名）；PTY 实拍证明一次 faux turn 里 `agent_start/turn_start/message_start/message_update/message_end/turn_end/agent_end/input` 与 `user_bash` 全部真触发 |
| slash 命令（权重 7%） | 74% | **78%** | `/thinking`（Stage 67 已实现、却漏进补全表）与 `/extensions`（Stage 71 新增）补入，对上上游 23 条为 **18/23** |
| TUI 交互面（权重 14%） | 58% | **64%** | 模块面 80.5% 不变；12.3/12.4 的四条输入面缺口（补全零接线、Ctrl+C 无 500 ms 窗口、jump-to-latest 无指示、`/help` 与用户输入同前缀）本轮关闭 → 取「模块 80.5% 与接线 47.7% 的算术均值」= 64%（旧的 58% 比该均值还低：把已修缺口仍算作缺口） |

扩展事件轴剩余 14 个缺口（上游名）：
`before_agent_start, agent_settled, before_provider_request, before_provider_headers, after_provider_response,
project_trust, session_before_compact, session_before_fork, session_before_switch, session_before_tree,
session_compact_failed, session_tree, ui_prompt_start, ui_prompt_end`。
另有 `model_select` 已有变体与 wire tag、却**没有生产构造点**（`/model` 选完不广播）。

加权重算：
5×1.00 + 13×0.90 + 8×1.00 + 6×0.70 + 14×0.64 + 8×0.91 + 7×0.78 + 7×0.70 + 8×0.95 + 7×0.57 + 9×0.85 + 5×0.46 + 3×0.95 = **79.9%**

敏感性（更新）：

- 扩展事件轴补到 100%：**82.9%**（+3.0pt）——仍是最大的单一摆动项，但已经从「20% 的巨大空洞」变成「还差 14 个事件」。
- `app.*` 接线率从 47.7% 补到 100%：交互轴 64% → 90.3%，总分 **83.6%**（+3.7pt）——**现在这是性价比最高的一轴**。
  > **已作废（§0.2）**：47.7% 是扫描器低报，实测 **79.5%**（35/44），补满只有 **+2.9pt**，
  > 且剩下 9 条里 6 条是**缺组件**（无 `/scoped-models`、无 tree 改名 UI），不是补键位。


### 0.2 口径更正：`app.*` 接线率不是 47.7%，实测 **79.5%**；加权随之改为 **81.4%**

同一个量被三个独立任务测过，结论差 36 个百分点，根因已经定位清楚：

| 口径 | 值 | 为什么错 |
| --- | --- | --- |
| 本报告 §0.1（我，LUM-1256） | 21/44 = 47.7% | 扫描器「截到第一个 `#[cfg(test)]` 就停」；`interactive.rs` 的树选择器 handler 在该标记**之后**，被整段丢弃 → **少算约 17 条**（低报） |
| LUM-1259（`docs/TUI_INPUT_AND_LAYOUT_VERIFICATION.md` §4.1） | 19/44 = 43.2% | 未剥离注释/字符串中的字面量，口径不同，同样低报 |
| LUM-1260（`scripts/app_action_coverage.py`） | **35/44 = 79.5%** | 唯一做过假阳性剔除与交叉校验的口径，**采纳** |

采纳 79.5% 的三条理由（本人复跑确认，不是转述）：

1. `strip_test_items()` 是括号/字符串/注释感知的，不会过早截断；
2. 它排掉了两类假阳性：`keybindings.rs` 里的**定义**、`locale.rs` / `slash.rs` 里的**广告行**（`("app.x", "描述")`）——只认 handler 形状的精确字面量 `"app.<id>"`；
3. `python3 pi-rust/scripts/app_action_coverage.py --check-consumed` 在合并 tip 上退出 0：
   `CONSUMED_APP_ACTIONS: 35 entries; measured wired: 35`，即**代码里的分发点与手写清单两个独立来源一致**。

```text
wired:      35/44 (79.5%)
advertised:  2/44 (4.5%)   app.editor.external / app.suspend
silent:      7/44 (15.9%)  app.models.{clearAll,enableAll,reorderDown,reorderUp,save,toggleProvider} / app.tree.editLabel
```

**连带更正两条结论：**

1. **功能面加权改用 81.4%**（`docs/PARITY_AND_TUI_AUDIT_LUM1260.md` §5 的逐轴复算；我复核其算式：13 个轴权重和为 100，把 `14×0.477` 换成 `14×0.795` 后加权和 = 81.383）。比我 §0.1 的 79.9% 高，差额全部来自**口径修正**，不是功能变多（他们说得很直白，我也赞成：这是把旧报告里两条算错的轴改对）。
2. **「`app.*` 接线是性价比最高的一轴」作废**：79.5% 的起点下补满只剩 14×0.205 = **+2.9pt**，而且剩下 9 条里 **6 条是缺组件**（上游 `ScopedModelsSelectorComponent` 与 tree 改名 UI 在 Rust 侧根本不存在，不是「键位没接」）。当前真正的性价比序列：
   1. ≤23 行终端里**输入框整个不在屏上**（用户盲打；`docs/PARITY_AND_TUI_AUDIT_LUM1260.md` §3.1，120×22/23/24 三帧对照）；
   2. 扩展事件缺的那 15 个恰好是插件最常用的（`tool_call` / `tool_result` / `before_agent_start` / `context`），按使用频率加权**远差于** 58.3% 这个名义覆盖；
   3. `/help` 的预排版被 `wrap_text` 重排成一整段（第二十一.6 第 0 条）。

> 教训（写进流程）：**百分比必须附「怎么量的」与「量它的脚本」**。我 §0.1 那张表里只有这一格是自己撮合出来的扫描器，也恰好是错得最厉害的一格；LUM-1260 的工具之所以可信，是因为它能被 `--check-consumed` 反测试（伪造清单会报 `FALSE AD` 并 exit 1）。后续这类数字一律走可复核脚本。

### 0.3 LUM-1261 复测：§0.2「性价比序列」的第 1、3 条已关闭（tip `c8bc6785a` + 本轮提交）

本轮**不改任何计数口径**，所以加权总分仍是 §0.2 的 **81.4%**（`app.*` 接线 79.5%；TUI 交互+视觉轴
用同一条公式 `(14×0.795 + 8×0.91) / 22` = **83.7%**，即 §0.1 表格里那格 73.8% 在口径更正后的值）。
改的是**证据**，而且是 §0.2 自己列出的性价比序列：

| §0.2 的序列 | 本轮 | 证据 |
| --- | --- | --- |
| 1. ≤23 行终端里输入框整个不在屏上（盲打） | **关闭** | `pi-tui/src/extension_ui.rs:415` 的 `plan_chrome` 改为**先预留提示行**（启动头可折叠、输入框不可）；120×23 前后对照 `docs/screenshots/lum1260-small-terminal-23.png.txt`（草稿不可见）→ `lum1261-small-terminal-23.png.txt`（行 22 `> typed blind▍`）；回归测试 `extension_ui.rs:758`；细节见 `docs/TUI_INPUT_AND_LAYOUT_VERIFICATION.md` §7.2 |
| 2. 扩展事件缺 15 个 | 未动 | 不是本 issue 的面，仍待专门一轮 |
| 3. `/help` 预排版被 `wrap_text` 重排成一整段 | **关闭** | `pi-tui/src/message.rs:1533/1570/1591` 换行感知包装 + 原样快路径；`pi-coding-agent/tests/help_text_layout.rs`（真实 `help_text()`）；120×50 前后对照 `lum1259-tip-interaction.png.txt:205` → `lum1261-help-layout.png.txt:129`；细节见 `docs/TUI_INPUT_AND_LAYOUT_VERIFICATION.md` §7.1 |

门禁（本轮独立运行，全部 `--offline`）：

| 命令 | 结果 |
| --- | --- |
| `cargo test --offline -p pi-tui` | **798 passed / 0 failed**，45 个 target 全部有结果（含 lib 358） |
| `cargo test --offline -p pi-coding-agent --test startup_header --test help_text_layout --test keybindings --test print_mode --test tools_render --test builtin_tool_factories` | **66 passed / 0 failed** |
| `cargo test --offline --workspace` | 未完成：`error: couldn't create a temp dir: No space left on device (os error 28)` → `could not compile \`pi-coding-agent\` (test "tools_render")`。50G 共享卷同时 4–5 条 run 在构建，与代码无关 |

已知 flaky（**早于本轮**，记在这里以免下一轮误判）：`pi-coding-agent --test extension_ui` 的
`interactive_regions_render_into_the_app` 实测 20 次失败 1 次（`EDITOR missing from [...]`），
该用例自 `d02fb0ace`（LUM-1190）就在，是 `pump()` 投递与渲染之间的竞态；40×12 下新旧 `plan_chrome`
分配完全相同，**与本轮布局改动无关**。

### 0.4 LUM-1266 复测：矮终端里聊天区从 1 行回到 17 行（合并提交 `e4db5cb82`）

同样**不改计数口径**，加权仍是 **81.4%**。本轮是 TUI 布局修正，关掉的是 §0.2 序列背后的同一个根因
（`plan_chrome` 不感知内置启动头高度）剩下的另外两个受害者：

| 问题 | 本轮 | 证据 |
| --- | --- | --- |
| 120×22/23 里聊天区只剩 1 行，`/help` 与 `/hotkeys` 渲染成**完全相同的帧** | **关闭** | 内置启动头在装不下时自动折叠；`app.rs::builtin_header_lines`；A/B：`docs/screenshots/lum1266-before-short-23.png.txt`（面板 3/4 同哈希 `9b19865217b3`，harness 判 FAIL）→ `lum1266-short-23.png.txt`（5 帧全不同，`/help` 19 行可见）；细节见 `docs/TUI_SHORT_VIEWPORT_AND_SCROLLBAR_LUM1266.md` |
| 滚动条画在文字最后一列，会吃掉整宽行的最后一个字符 | **关闭** | 画滚动条时转写区宽度 `W → W-1`（`app.rs::viewport_for_render` / `viewport_reserved` / `scrollbar_geometry`）；不变量测试 `pi-tui/tests/short_viewport.rs`（3 条）。诚实说明：120 列下无行填满末列，所以缺同 tip 的 before 帧（详见该文 §2.4） |

门禁（本轮独立运行）：

| 命令 | 结果 |
| --- | --- |
| `cargo test --offline -p pi-tui` | **805 passed / 0 failed**（LUM-1261 tip 为 798，本轮 +7） |
| `cargo clippy --offline -p pi-tui --all-targets` | 无告警 |
| 真机 PTY `lum1266-{short-22,short-23,tall-34}` | 3/5/3 帧各不相同、退出码 0（120×22 / 120×23 / 120×34） |

### 0.5 LUM-1307 复测：启动即应用 `settings.json`；门禁与两条口径的更正（tip `0ed96bb8e` + 本轮）

本轮是 **TUI 设置缺口修复**，不改任何轴的分母，但顺手把几个可测口径重测了一遍。细节见
`docs/LUM1307_STARTUP_SETTINGS.md`。

| 口径 | 本轮实测（命令见该文 §8） | 本文旧值 | 说明 |
| --- | --- | --- | --- |
| 纯代码规模（src↔src） | **84.4%**（129,174 / 153,066） | 82.5%（124,242 / 150,538，`af3aa30a4`） | 口径一致，只是增长；LUM-1306 的「57.5%」是**分子用含 tests 的 `.rs`、分母用 TS src+测试**的混用口径 |
| 测试规模 | **45.0%**（2,390 标记 / 5,309） | 42.0%（2,232 / 5,309） | `cargo test --workspace` 实跑 **2457 passed / 0 failed** |
| slash 内置命令 | **17 / 23**（逐名核对上游 `BUILTIN_SLASH_COMMANDS`） | 20/23（LUM-1306） | 旧口径分子用了「上游命令 ∪ Rust 独有命令」，分母却是上游；修正后缺 6：`scoped-models import share changelog login logout` |
| `app.*` 接线 | **35 / 44 (79.5%)** | 35/44 | 未变（本轮未动键位） |
| 加权完成度（13 轴） | **不重算** | 81.4%（§0.4）/ 82.2%（LUM-1306） | 本文 §0.2/0.4 与 LUM-1306 已给出两个不同总分（权重表分项不可追溯），本轮不再叠一层小数 |
| 门禁 | 1.85.0 下 `fmt --check` **15 处**、`clippy -D warnings` **11 条**（均既有） | §0.4 记「clippy 无告警」 | 差异来自**工具链版本未钉住**（本文头部记的是 `cargo+rustc 1.98.1`，该工具链已不在本机）；仓库无 `rust-toolchain.toml`，PATH 上的 `cargo` 还是坏 wrapper |

对 TUI 轴线本身：本轮关闭 LUM-1306 §4.3 记录的「`settings.json` 的 UI 切片只有 `/settings` +
`/reload` 会读，启动帧不生效」这一项，A/B 证据（真 PTY，修前/修后各 2 张）在
`docs/screenshots/lum1307-startup-ui-{before,after}.png`，颜色维度由新脚本
`scripts/theme_palette_report.py` 对齐到 `assets/themes/*.json`（启动帧的 accent/muted/dim
从 dark 的 `#8abeb7`/`#808080`/`#666666` 变为 light 的 `#5a8080`/`#6c6c6c`/`#767676`）。
TUI 轴的其余未达项（23 个 `app.*` 未接线、`@` 补全首行重复、`#` 无 provider）本轮未动。

### 0.6 LUM-1310 复测：启动生效 3 键 + `/settings` 6 行；差距重测（tip `cdc033e21` + 本轮）

本轮是 **TUI 设置缺口修复**，不改任何轴的分母，但把几个可测口径在**同一台机、同一工具链（1.85.0）**上
重测了一遍。细节与真 PTY A/B 见 `docs/LUM1310_SETTINGS_AND_TUI_AUDIT.md`。

| 口径 | 本轮实测 | 本文旧值 | 说明 |
| --- | --- | --- | --- |
| 纯代码规模（src↔src） | **85.1%**（130,333 / 153,106） | 84.4%（129,174 / 153,066，LUM-1307 文） | 同口径；本轮 +1,159 Rust 行 |
| 测试规模 | **45.5%**（2,416 标记 / 5,309） | 45.0%（2,390 / 5,309） | `cargo test --workspace` 实跑 **2483 passed / 0 failed / 2 ignored** |
| slash 内置命令 | **16 / 23 (69.6%)**（`exit`≡`quit` 则 17/23） | 17/23（LUM-1307 文） | 本轮逐名重测：缺口 `scoped-models import share changelog login logout quit` |
| `app.*` 接线 | **37 / 44 (84.1%)**；advertised **0/44**；silent 7/44 | 37/44；advertised 0/44 | 未变（本轮未动键位） |
| `/settings` 行 | **6 / 37** row id | 3 / 37 | 本轮 +3：`hide-thinking`、`autocomplete-max-visible`、`quiet-startup` |
| 门禁（1.85.0，`--locked`） | `fmt --check` **干净**；`clippy -D warnings` **0 findings**；`test` **2483 passed** | fmt 15 处 / clippy 11 条（工具链未钉版） | `scripts/toolchain.sh` + 钉版后已可复现 |

对 TUI 轴线本身：本轮关闭「`quietStartup` / `hideThinkingBlock` / `autocompleteMaxVisible` 三键
只有手改 JSON 一条路、且手改也不在启动帧生效」这一项，A/B 证据（真 PTY，修前/修后各 3 张，
**两个二进制 md5 不同**）在 `docs/screenshots/lum1310-{startup-ui-keys,quiet-startup-on,quiet-startup-off}-{before,after}.png`。
TUI 轴的其余未达项（7 个 silent `app.*`、`/settings` 剩余的 31 行、`fullscreen*` 三项）本轮未动。

### 0.7 LUM-1305 复测：composer 下拉框与模态选择器合并为同一套 `SelectList` 行布局（tip `feature/pi.rs` + 本轮）

本轮把「下拉框」从编辑器侧的自绘渲染改成共享 `SelectList` 行布局，并亲自复测了三条轴：

| 轴 | 本轮 | 上轮公开值 |
|---|---|---|
| TUI 模块数 | 33 / 42 = **78.6%** | 33/41 = 80.5%（上游文件数长了） |
| `app.*` 接线率 | wired **43/44 = 97.7%**，advertised **0/44**，silent 1/44 | 37/44 = 84.1% |
| 测试用例数（Rust / TS） | 2570 / 5439 = **47.3%** | 2,232 / 5,309 = 42.0% |

**加权总分需更正一条口径**：§3.7 的「模块 80.5% 与快捷键 47.7% 的加权 = 58%」没有公开取法
（50/50 得 64.1），改用公开公式 `轴5 = (模块率 + 接线率) / 2` 后，本轮加权为 **80.4%**
（上轮公开值 76.05%；差额里 3.4pt 是这条口径，其余是接线率与用例数真实上表）。公式与逐项拆解见
`docs/LUM1305_AUTOCOMPLETE_SELECT_LIST.md` §6。

另外一条修正：§6 的第 7 条把 `@` 补全首行的「label 与 description 相同」当成缺陷，
但上游就是 `label: entryName` + `description: displayPath`，**这不是 port 的错**（同条里的
`#` 触发符缺口成立，保留为 P1）。

### 0.8 LUM-1418 复测：列宽口径从「字符数」改为「终端列」，全 TUI 13 个渲染模块切换（tip `feature/pi.rs` + 本轮）

本轮**不改任何轴的分子分母**，所以加权总分仍是 **81.4%**。改的是**之前没有人量过的一条度量**：

| 量 | 本轮 | 上轮公开值 | 说明 |
|---|---|---|---|
| 纯代码规模（src↔src） | **87.6%**（134,062 → 134,449 / 153,106） | 85.1%（130,333 / 153,106，LUM-1310 文） | 同口径；本轮 +387 行 src（609 增 / 222 删） |
| 测试规模 | **47.0%**（2,555 标记 / 5,439） | 47.3%（2,570 / 5,439，LUM-1305 文） | 本轮 +19 个 `#[test]`（净 -15：旧口径把 34 个标记算重了，见下） |
| **列宽口径面（新量）** | **13 个模块 / 71 处引用按列 = 100%**（本轮前 **0**） | —— | 之前没有任何一轮量过「一行的宽度是什么」 |
| **指针映射面（新量，未做）** | **0 / 3 = 0%** | —— | 鼠标选择 / 双击选词 / 搜索高亮列的「屏幕列 ↔ 字符下标」仍是字符口径 |

**为什么加权总分不动**：这条缺陷不落在任何一条轴的「有没有这个能力」上，而是落在轴 6（TUI 视觉保真，权重 8%，
取值 90%）的**前提**里 —— 90% 这个数字隐含「渲染是对的」。它在 ASCII 下确实对，所以历史审计（全部用 ASCII 样本）
没有任何机会看到它。修完之后轴 6 的取值不变，但支撑它的证据从「ASCII 帧看起来对」变成 §5.2 的四条列宽不变量。
把轴 6 重估为 95% 只能 +0.4pt，不足以改变结论，所以本文**不动**它。

**测试标记数净减 15 的诚实说明**：LUM-1305 §0.7 报的 2,570 是用
`grep -rhc '#\[test\]\|#\[tokio::test\]' … | paste -sd+ | bc`（逐文件计数再求和）量的，
与文件顺序无关但会把「一行里出现两次」计两次；本轮改用
`python3` 的 `re.findall(r'#\[test\]|#\[tokio::test\]', …)` 重数，同一棵树上得 **2,536**（基线）/ **2,555**（本轮）。
**两个口径都不是本轮引入的**，本文只把基线一并重数，避免把「换了个尺子」误读成「测试变少了」。

本轮新增/修改的回归网（`crates/pi-tui/tests/column_width.rs` + `width.rs` 单测，共 19 条）：

```
列宽不变量  transcript(markdown/plain 两套换行器) ×5 列宽(16/24/33/48/80)
            + selector ×3 + status ×4 + composer ×4 + App 真帧(100×30)
字形完整性  ab你好世界 @7 列 → ["  ab你", "  好世", "  界"]（不丢字、不劈字）
光标完整性  ▍ 恰好一次且落在草稿行内
CJK 断点    "你好世界" @6 列 → ["你好世", "界"]（两条断行规则都要）
零宽/制表  a\tb=5、a\rb=2、e+U+0301=1、emoji=2、Ambiguous=1
```

门禁与基线对照的完整命令、输出与「既有失败不是本轮引入」的逐条证明，见
`docs/LUM1418_COLUMN_WIDTH.md` §4。

### 0.9 LUM-1426 复测：指针/高亮改按终端列，composer 补上点击定位；加权 **83.6%**（主口径）/ **84.6%**（与上轮同口径）

本轮把 LUM-1418 §0.8 里「指针映射面 **0/3 = 0%**」这条新量做完了，并补了一个此前没人量过的
**composer 鼠标面**。同时按同一套命令重测了全部可测口径（本机 Windows / cargo 1.97.1 / `--offline`）。

| 量 | 本轮实测 | 上轮公开值 | 说明 |
|---|---|---|---|
| 纯代码规模（src↔src） | **88.7%**（135,733 / 153,106） | 87.6%（134,449 / 153,106，LUM-1418 文） | 口径一致；`find … ｜ xargs wc -l ｜ tail -1` 会因 xargs 分批而**只看到最后一批**，本轮改用 `python pi-rust/scripts/measure_loc.py` 逐文件求和（Rust `crates/**/src/*.rs` 去 `mod.rs`，TS `packages/**/src/*.ts`） |
| 测试规模 | **49.0%**（2,603 / 5,309） | 47.0%（2,555 / 5,439） | Rust `#[test]`/`#[tokio::test]` 重数；TS `*.test.ts` 的 `it(`/`test(`（544 文件 / 5,309） |
| TUI 模块面 | **35 / 42 = 83.3%**（Rust `pi-tui/src` 36 − `lib.rs`；TS `tui/src` 24 + `components` 18） | 33/42 = 78.6% | 本轮新增 `width.rs` 与 `pointer_columns` 相关模块 |
| `app.*` 接线 | **43 / 44 = 97.7%**，silent 1（`app.tree.editLabel`） | 43/44（LUM-1305） | `python pi-rust/scripts/app_action_coverage.py --check-consumed` → `in sync` |
| slash 内置命令 | **18 / 23 = 78.3%**（字面 17/23，`exit`≡`quit`） | 78% | 缺 `import share changelog logout` + `quit` |
| 扩展生命周期事件 | **21/36 声明 = 58.3%**；**20/36 生产构造点 = 55.6%** | 57% | 上轮的 57% 用的分母是 35，本轮按上游文档的 **36** 个事件名重算 → 这 −0.1pt 是**口径修正**，不是功能回退 |
| **指针映射面（LUM-1418 §0.8 新量）** | **3 / 3 = 100%**（鼠标选择 / 双击选词 / 搜索高亮） | **0 / 3 = 0%** | 本轮关闭，见 `docs/LUM1426_POINTER_COLUMNS.md` §2–§3 |
| **composer 鼠标面（新量）** | **1 / 2 = 50%** | —— | 点击定位光标 ✅（上游 `editor.ts:620-666`）；autocomplete 下拉框点选 ❌（下一轮第一顺位） |

**加权完成度（权重表见 §4.1，公式公开）**：

```text
5×1.00 + 13×0.90 + 8×1.00 + 6×0.70 + 14×0.905 + 8×0.90 + 7×0.783 + 7×0.70
  + 8×0.95 + 7×0.556 + 9×0.85 + 5×0.490 + 3×0.95 = 83.6%
轴 5 = (模块率 0.833 + 接线率 0.977) / 2 = 0.905
```

**口径对账（读者可自行复算）**：上轮公开的 81.4% 用的是「轴 5 = 接线率单独取值」的算式，
按它自己给的输入逐项相加实为 **82.1%**。本轮同时给出两个口径：

- **主口径（轴 5 = 模块率与接线率均值）= 83.6%**；
- **与上轮同口径（轴 5 只取接线率）= 84.6%**（上轮同输入复算 82.1% → **+2.5pt**）。

差额可逐项对账：轴 5 接线 0.795→0.977（+2.55pt）、测试轴 0.46→0.490（+0.15pt）、
事件轴 0.57→0.556（−0.10pt，纯口径修正），其余轴未动。**没有一项是「重估」出来的。**

**TUI 交互+视觉轴**在同一条公式 `(14×0.905 + 8×0.90) / 22` 下为 **90.3%**（上轮同式为 83.7%）。

## 1. 方法与口径

### 1.1 测量命令（可复现）

```bash
# 规模
git rev-parse --short HEAD                     # af3aa30a4
find pi-rust/crates -path '*/src/*' -name '*.rs' ! -name 'mod.rs' | xargs wc -l | tail -1
find packages -path '*/src/*' -name '*.ts'      | xargs wc -l | tail -1
# 测试
grep -rhc '#\[test\]\|#\[tokio::test\]' pi-rust/crates --include=*.rs | paste -sd+ | bc
find packages -name '*.test.ts' | xargs grep -ch '\bit(\|\btest(' | paste -sd+ | bc
# 工具 / 命令 / 快捷键 / provider / 扩展
sed -n '1,30p' pi-rust/crates/pi-coding-agent/src/tools/defaults.rs
grep -oE 'AUTOCOMPLETE_COMMANDS[\s\S]*' pi-rust/crates/pi-coding-agent/src/commands/slash.rs
./pi --help
sed -n '108,141p' pi-rust/crates/pi-protocol/src/events.rs   # ExtensionEvent 变体
```

### 1.2 本审计的运行时证据

`pi-rust/scripts/pty_capture.py`（本 round 新提交的 PTY 录制工具，真 PTY + `TIOCSWINSZ` + pyte 终端仿真 + PIL 渲染）：

- `pi-rust/docs/screenshots/lum1241-interaction.png`（8 帧：启动帧 / `/` 补全 / 模糊收窄 / ↓ 移动选择 / Tab 落地 / `@` 文件补全 / 收窄 / Tab 落地）
- `pi-rust/docs/screenshots/lum1241-turn-and-commands.png`（8 帧：`/model` 选择器 / Esc 关闭 / 真实 faux turn / `!` 本地 bash / `/help` / `/hotkeys` / PgUp 回滚 / `#` 缺口）
- 同名 `.txt` 是**字符网格 dump**，可直接 grep 断言，弥补 PNG 不可搜索的缺点。

### 1.3 不可测项与限制（诚实条目）

1. **TS 侧模型条目数不可测**：`packages/ai/src/providers/data/*.json` 被 `.gitignore:11` 忽略，本 checkout 里不存在（是构建产物）。因此 Rust 的 166 条模型只能给"下界对比"，不能算 TS 侧分母。
2. **无法跑 TS 侧做行为 A/B**：本环境没有 bun / `node_modules`，所有 TS 数字都是静态测量；Rust 侧才有运行时证据。
3. **LOC ≠ 质量**：Rust 侧有 TS 没有的实现（`pi-session` 的 rusqlite+zstd、`pi-protocol` 的 wire types）；TS 侧有 Rust 未移植的资源（39 个 `*.models.ts` + 其 JSON）。
4. **权重是判断，不是测量**：§4 的权重由我给出并公开公式；换权重就换结果，读者可自行替换重算。
5. 快捷键"未接线"里有一部分（`tree.filter*`、`models.*`）可能由选择器组件内部消费，`app.*` 全局表查不到——这类已单独标注"待复核"，未计入差距分。

## 2. 规模轴

| Rust crate | src 行 | 文件 | tests 行 | 用例 | | TS package | src 行 | 文件 | 测试行 | 用例 |
|---|---|---|---|---|---|---|---|---|---|---|
| pi-agent-core | 3,145 | 11 | 4,173 | 11 | | agent | 25,305 | 92 | 21,660 | — |
| pi-ai | 16,998 | 41 | 5,073 | 199 | | ai | 24,383 | 179 | 36,935 | — |
| pi-chord | 11,142 | 28 | 2,597 | 133 | | chord | 5,822 | 24 | 3,552 | — |
| pi-client | 2,772 | 10 | 770 | 7 | | client | 1,135 | 8 | 715 | — |
| pi-coding-agent | 37,094 | 66 | 10,316 | 522 | | coding-agent | 70,052 | 258 | 54,813 | — |
| pi-evals | 2,956 | 10 | 265 | 1 | | evals | 1,964 | 11 | 476 | — |
| pi-extensions | 6,825 | 11 | 10,304 | 143 | | （无对应包，宿主侧） | — | — | — | — |
| pi-mono | 16 | 1 | 0 | 0 | | （monorepo 元数据） | — | — | — | — |
| pi-protocol | 2,373 | 13 | 230 | 29 | | protocol | 869 | 8 | 567 | — |
| pi-server | 3,514 | 16 | 746 | 1 | | server | 1,966 | 16 | 1,057 | — |
| pi-session | 4,550 | 11 | 2,909 | 81 | | session-backends | — | — | 1,909 | — |
| pi-telemetry | 1,518 | 8 | 561 | 18 | | telemetry | 935 | 6 | 243 | — |
| pi-tui | 31,339 | 33 | 11,713 | 731 | | tui | 18,107 | 42 | 17,573 | — |
| **合计** | **124,242** | **259** | **49,657** | **1,876** | | **合计** | **150,538** | **644** | **140,044** | **5,309** |

- Rust 含 `tests/` 的全部 `.rs`：174,338 行。Rust 另有 1,519 个 `tests/` 目录下的 `fn` + 独立 grep 的 `#[test]`/`#[tokio::test]` 合计 2,232。
- **Rust 领先的子系统**：`pi-chord`（11,142 vs 5,822）、`pi-evals`（2,956 vs 1,964）、`pi-telemetry`（1,518 vs 935）、`pi-protocol`（2,373 vs 869）——这些不是"落后面"。
- 规模口径：124,242 / 150,538 = **82.5%**；测试用例口径：2,232 / 5,309 = **42.0%**。

## 3. 功能面逐轴实测

### 3.1 内置工具：7 / 8

`pi-rust/crates/pi-coding-agent/src/tools/defaults.rs` 的 `default_tool_bundle()` 返回 `read, write, edit, bash, find, grep, ls`（7 个，注释即"Currently seven tools"）。
TS 侧 `packages/coding-agent/src/core/tools/` 注册 8 个：`bash, edit, find, grep, ls, powershell, read, write`。
唯一缺口 `powershell` 是 Windows 专属工具（本机 Linux 不可运行，属**有意的平台差异**，不是能力缺口）→ **87.5%**。

### 3.2 provider 与模型目录

- Rust `pi-ai/src/providers/`：`anthropic, azure_openai_responses, faux, google, mistral, mod, openai, openai_responses, registry`；`registry.rs` 静态登记 **32 个 provider / 166 条 `ModelSpec::new`**，**烘焙进二进制、离线可用**（这正是 `pi --model faux/faux-model` 无网络可跑的原因）。
- TS：47 个 provider 实现文件 + 39 个 `*.models.ts`，但其 `data/*.json` 不入库（§1.3）。
- API family 覆盖：两边都是 5 族 —— `openai-completions, openai-responses, anthropic-messages, google-generative-ai, azure-openai-responses` → **5/5 = 100%**。
- 广度比：32 / 47 = **68.1%**（TS 分母含大量同族变体，实际"厂商覆盖"更接近）。

### 3.3 slash 命令：17 / 23

Rust：`help clear new copy name model session export resume tree fork clone settings compact exit trust hotkeys`（`commands/slash.rs`，`AUTOCOMPLETE_COMMANDS` 在 181 行）。
TS：上述去掉 `clear/exit` 外还多 `thinking scoped-models import share changelog login logout reload`。
**缺 8 个**：`thinking, scoped-models, import, share, changelog, login, logout, reload` → 17/23 = **73.9%**。

### 3.4 CLI 顶层 flag：18 / 40

方法：Rust 用二进制 `--help` 实测（23 个，含 `-h`）；TS 用 `packages/coding-agent/src/cli/args.ts` 声明（41 个，剔除 `--` / `---` 噪声）。
交集 18 个。TS 独有 22 个：`api-key exclude-tools fork list-models mode models name no-builtin-tools no-session no-themes no-tools offline provider session-id system-prompt theme thinking tools tui-mode use-theme verbose version`。
Rust 独有 5 个：`extensions-dir max-turns no-header output-format rpc`。
**口径注释（诚实项）**：其中 `fork → /fork`、`version → pi version`、`list-models → pi list-models`、`provider → --model <p>/<m>`、`rpc → --rpc` 在 Rust 侧另有等价入口，按字面 flag 名算是 18/40 = **45%**，按"能力入口"算约 23/40 = 57%。**本文取字面口径 45%**。

### 3.5 快捷键接线：44 定义 / 21 接线

`pi-coding-agent/src/keybindings.rs` 定义 44 个 `app.*` action。在**其它文件**里出现（即真正被消费）的 21 个：

```
clear 20  header 28  tools.expand 27  pasteImage 18  exit 15  cycleForward 13
followUp 11  thinking.toggle 11  interrupt 10  dequeue 9  cycleBackward 9  model.select 8
session.new 8  session.tree 6  message.copy 5  session.resume 5  suspend 5  thinking.cycle 4
editor.external 3  session.fork 3  thinking.save 1
```

未在全局表消费的 23 个：`models.{clearAll,enableAll,reorderDown,reorderUp,save,toggleProvider}`、`session.{delete,deleteNoninvasive,rename,toggleNamedFilter,togglePath,toggleSort}`、`tree.{editLabel,foldOrUp,unfoldOrDown,filter.all,filter.cycleBackward,filter.cycleForward,filter.default,filter.labeledOnly,filter.noTools,filter.userOnly,toggleLabelTimestamp}`。
`docs/TUI_UX_AUDIT.md` 写的是"仅接线 3/43"。
**（该段真实值已被新脚本取代：21/44 ≈ 48% 是**低报**——扫描器在 `interactive.rs` 的第一个
`#[cfg(test)]` 处截断，漏掉了树选择器的 handler。实测 **35/44 = 79.5%**，见 §0.2；
LUM-1259 另测的 19/44 = 43.2% 同样低报。）**
→ 接线率见 §0.2（本段下方的 47.7% 已作废）。

### 3.6 扩展生态（两条子轴，本审计最大发现）

**宿主能力面（强）**：`pi-extensions/docs/EXTENSIONS.md` 表格列出宿主 import：`registerTool/registerCommand/registerProvider/unregisterProvider/appendEntry/sendMessage/sendUserMessage/setSessionName/exec(+cancel)/fetch(+cancel)/ui.notify/ui.confirm/ui.input/ui.select/ui.custom/setWidget/setHeader/setFooter/setEditorComponent/log/child_process(zlib,crypto,fs,os,process via shim)/pi_ai_stream_start`。对照 TS `docs/extensions.md` 的 ~21 个 `pi.*`：宿主能力类**基本全覆盖**，缺 `setActiveTools、setThinkingLevel、setLabel、registerShortcut、registerFlag、registerEntryRenderer、registerMessageRenderer、registerMarkdownTransformer、getAllTools`（9 个）→ 覆盖面约 **57%–95%**，取 **95%**（能力等价入口多于 TS 的 UI 面，例如 `ctx.ui.custom` 的 overlay 生命周期在 Rust 侧有完整 host op）。

**生命周期事件面（弱，真正的 P0 缺口）**：
- TS（`docs/extensions.md`）扩展可监听 **36 个事件名**：`before_agent_start/agent_start/agent_end/agent_settled/turn_start/turn_end/message_start/message_update/message_end/tool_call/tool_execution_start/tool_execution_update/tool_execution_end/tool_result/input/context/session_start/session_shutdown/session_before_compact/session_compact/session_compact_failed/session_before_fork/session_before_switch/session_before_tree/session_tree/session_info_changed/model_select/thinking_level_select/project_trust/resources_discover/user_bash/ui_prompt_start/ui_prompt_end/before_provider_request/before_provider_headers/after_provider_response`。
- Rust 协议枚举 `pi-protocol/src/events.rs:108 ExtensionEvent` 只有 **7 个变体**：`SessionStart, SessionEnd, UserMessage, ToolCall, ToolResult, AgentEnd, ResourcesDiscover`。
- **运行期真正会触发**的只有 **2 个**：`wiring.rs:374` 的 `SessionStart`（每进程一次）与 `bridge.rs:71` 的 `discover_resources`（`resources_discover`）。`UserMessage/ToolCall/ToolResult/AgentEnd/SessionEnd` 除了测试代码，**没有任何生产调用点**（`grep -rn 'ExtensionEvent::(UserMessage|ToolCall|ToolResult|AgentEnd|SessionEnd)'` 仅命中 `tests/`）。
- 分发是**精确名匹配**（`pi-ext-shim.mjs:1040 _pi_dispatch` → `_pi.handlers[parsed.type]`），没有别名表：所以上游插件写 `pi.on("session_shutdown", …)`、`pi.on("tool_execution_start", …)`、`pi.on("turn_start", …)` 在 Rust 下**永远不会触发**（Rust 的对应名是 `session_end`，且 `turn_start`/`tool_execution_start` 连变体都没有）。
→ 声明面 7/36 = **19.4%**；运行期 2/36 = **5.6%**；本审计取 **20%** 作为该轴得分（偏乐观，给在途的 LUM-1239 Stage 71"扩展可见性"留空间）。

> **已修正（LUM-1256 合并后）**：上面这段是 `af3aa30a4` 的实测，LUM-1246 的 Stage 68 合并进来后该轴为
> 声明 **21/35 = 60%**、运行期构造 **20/35 = 57.1%**，本审计取 **57%**。新枚举在
> `pi-protocol/src/events.rs`（含上游 paritiy 变体，字段名带显式 `rename` 对齐 `types.ts`）。
> 复测与剩余缺口清单见 **§0.1**。

### 3.7 TUI 组件与交互面

- 模块数：Rust `pi-tui/src/` 33 个模块（`app.rs` 5,042 行；`extension_ui, highlight, hyperlink, image, locale, message, prompt, search, selector, settings, status, styled, styles, terminal_image, theme, tree, undo_stack, word_navigation, autocomplete, fuzzy, markdown, diff...`）；TS `tui/src/` 24 顶层 + 17 `components/` = 41。→ 33/41 = **80.5%**。
- 交互器：`pi-coding-agent/src/interactive.rs` 4,187 行。
- 终端图像协议：Rust 7 文件 / TS 10；语法高亮引用：Rust 17 / TS 18（两边都有 image + highlight + latex + search + kill-ring + undo + autocomplete）。
→ 综合交互轴取 **58%**（模块 80.5% 与快捷键 47.7% 的加权，且真实缺口在接线而非能力）。

### 3.8 视觉保真（主题 / Markdown / 高亮 / LaTeX / 图片）

内置主题、`markdown.rs`、`highlight.rs`、`latex`、`hyperlink.rs`、`locale.rs`、`terminal_image.rs`、`THEMING.md` 全部在位；PTY 实拍显示真彩色、粗体、反色、光标盒、多面板布局都正确渲染（见截图）→ **90%**（缺口是上游的少数视觉细节与组件，不是框架缺失）。

### 3.9 会话 / 存储 / 导入导出

`pi-session`：`branch/error/export/lib/migrate/reader/schema/stats/tree/usage/writer`，rusqlite + zstd，**能读**上游 `packages/session-backends/sqlite-node` 的 AgentHarness 存储格式 4；`pi session list|show|export|migrate` 子命令齐全；JSONL/HTML 导出在位。**写**上游布局仍是待办（双向兼容只完成一半）→ **85%**。

### 3.10 子包对应关系（Rust 不落后的部分）

`pi-server` / `pi-client` 与 TS 的 `server` / `client` 是模块级一一对应（Rust 侧还多出 `cancel.rs`、`subscription.rs`、`testing.rs`）；`pi-chord`、`pi-evals`、`pi-telemetry`、`pi-protocol` 的 src 行数**都高于**对应 TS 包（§2）→ **95%**。

## 4. 加权完成度

### 4.1 权重表（公式公开，权重为主观判断）

| # | 轴 | 权重 | 实测得分 | 依据 |
|---|---|---|---|---|
| 1 | 可构建 / 可测 / 可运行（本 tip） | 5% | 100% | §5 门禁全绿 |
| 2 | 核心 agent 循环（工具执行、截断、diff、压缩） | 13% | 90% | `pi-agent-core` + `agent_loop.rs` + `tools/` |
| 3 | provider API family 适配 | 8% | 100% | 5/5 族 |
| 4 | provider / 模型目录广度 | 6% | 70% | 32/47 模块 = 68.1% |
| 5 | TUI 交互面（模块 + 快捷键接线） | 14% | 58% | 80.5% 模块 / 47.7% 接线 |
| 6 | TUI 视觉保真 | 8% | 90% | §3.8 |
| 7 | slash 命令面 | 7% | 74% | 17/23 |
| 8 | CLI / 模式 / 子命令面 | 7% | 70% | §3.4（字面 45%、能力入口 57% 之间的取中） |
| 9 | 扩展宿主能力 | 8% | 95% | §3.6 上 |
| 10 | 扩展生命周期事件 | 7% | 20% | 7/36 声明、2/36 运行期 |
| 11 | 会话 / 存储 / 导入导出兼容 | 9% | 85% | 读兼容、写待补 |
| 12 | 测试与门禁强度 | 5% | 45% | 用例 2,232 vs 5,309 |
| 13 | 子包完整度（server/client/chord/telemetry/evals/protocol） | 3% | 95% | §3.10 |

加权和 = 5×1.00 + 13×0.90 + 8×1.00 + 6×0.70 + 14×0.58 + 8×0.90 + 7×0.74 + 7×0.70 + 8×0.95 + 7×0.20 + 9×0.85 + 5×0.45 + 3×0.95 = **76.05%**

### 4.2 敏感性（诚实说明）

- 把扩展事件轴从 20% 提到 100%（即补完 36 个事件）：总分为 **81.7%**——**这是全表最大的单一摆动项**，说明"扩展生态兼容"是当前性价比最高的攻坚方向。
- 把 TUI 快捷键接线从 47.7% 提到 100%：总分 +4.1pt → **80.2%**。（同样按 §0.2 作废：79.5% 起点下只余 +2.9pt，且其中大半是缺组件）
- 权重整体由我给定；若把"规模"口径直接当完成度则是 82.5%，若只看测试则是 42.0%。**三个数字都对，取决于你问的是"代码搬了多少""测了多少""功能能用多少"。**

## 5. 门禁结果（本 tip 实测）

| 门禁 | 结果 |
|---|---|
| 冷构建 `cargo build --offline -p pi-coding-agent --bin pi` | ✅ EXIT=0，`Finished dev profile in 2m 09s`，二进制 53,952,976 B |
| `cargo test --offline -p pi-tui` | ✅ **758 passed / 0 failed**，42 个 target（lib 344 + 40 集成 + 7 doc） |
| `cargo fmt --all -- --check` | ✅ EXIT=0 |
| `cargo test -p pi-coding-agent` | 引用 LUM-1236 §15.4 同 tip 结果：450 lib + 24 targets + 6 doc，0 failed |
| clippy `--all-targets -D warnings` | 引用 LUM-1236 §15.4：exit 0（本 round 未改任何 `.rs`，`git status` 仅新增 `docs/` 与 `scripts/`） |
| PTY 交互实测 | ✅ 16 帧真终端录制，截图 + 字符网格 dump 见 §1.2 |

## 6. 未达项清单（按性价比排序）

1. **P0 · 扩展生命周期事件**：7/36 声明、2/36 运行期。上游插件最常用的 `tool_call`、`tool_result`、`tool_execution_start/end`、`turn_start`、`message_*`、`session_shutdown` 在 Rust 下是死代码变体或不存在。**这是"兼容 pi 插件生态"这条目标的头号阻塞**。
2. **P1 · TUI 快捷键接线**：23 个 `app.*` action 未在全局表消费（`models.*` 6 个、`session.*` 6 个、`tree.*` 11 个）。用户可感知的是 `/models` 批量启停、会话重命名/删除、树过滤器走不到键盘。
3. **P1 · CLI flag 面**：`--theme/--thinking/--tools/--provider/--api-key/--verbose/--offline` 等 22 个 TS flag 无字面等价物（部分有替代入口）。
4. **P2 · slash 命令 8 个**：`thinking, scoped-models, import, share, changelog, login, logout, reload`。
5. **P2 · 会话写入侧上游兼容**：当前只保证"读"上游 sqlite/JSONL 格式。
6. **P3 · `powershell` 工具**：Windows 专属，本平台可不做。
7. **UI 细节项（LUM-1305 重新定性）**：`@` 文件补全首行的 `❯ src/  src` **不是缺陷**——上游
   `packages/tui/src/autocomplete.ts:801-805` 就是 `label: entryName` + `description: displayPath`，
   顶层条目两者天然相同，Rust 侧逐字对齐；LUM-1305 后它落在与模态选择器同一套对齐描述列上。
   同一条里的另一半——`#` 触发符没有任何 provider 支撑（`DEFAULT_AUTOCOMPLETE_TRIGGER_CHARACTERS`
   声明了 `#`）——**仍然成立**，降为 P1（与 `app.*` 假广告同类：宣传了但不干活），
   详见 `docs/LUM1305_AUTOCOMPLETE_SELECT_LIST.md` §6.2。

## 7. 建议的推进顺序

1. 把 `ExtensionEvent` 补齐到上游 36 个事件名（含别名兼容 `session_shutdown`），在 `agent_loop` 的 observer 上直接转发 ——> 单项 +5.6pt，且直接决定"插件生态兼容"成败。
2. 补 `models.* / session.* / tree.*` 快捷键接线（+4.1pt），顺手修 `@` 补全首行重复。
3. 补 CLI 的 `--theme/--thinking/--tools/--provider/--offline` 与 4 个高价值 slash 命令。
4. 补测试：把 Rust 用例数从 2,232 往 5,309 靠（当前 42%，是最大的"隐藏债务"）。
