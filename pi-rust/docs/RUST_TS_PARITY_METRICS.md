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

### 0.10 LUM-1431 复测：autocomplete 下拉框补上鼠标点选（composer 鼠标面 2/2）；LUM-1422 的搜索栏列宽预算并入

本轮领 LUM-1426 §8 的第一顺位，并把并行未合并线 `work/LUM-1422` 抢救合并。按同一套命令重测全部可测口径
（本机 Windows / cargo 1.97.1 / `--offline`，从仓库根跑脚本）。

| 量 | 本轮实测 | LUM-1426 | 说明 |
|---|---|---|---|
| 纯代码规模（src↔src） | **88.8%**（135,950 / 153,106） | 88.7%（135,733） | `python pi-rust/scripts/measure_loc.py`；本轮 +217 行 src（`app.rs` 指针路由 + `search.rs` 列宽预算） |
| 测试规模 | **49.5%**（2,628 / 5,309） | 49.0%（2,603） | Rust `#[test]`/`#[tokio::test]` 与 TS `*.test.ts` 的 `it(`/`test(` 重数；+25 = LUM-1422 的 4 个新测试文件（15 条）+ 本轮 2 个新测试文件（10 条：行为 7 + 帧 3） |
| TUI 模块面 | **35 / 42 = 83.3%** | 35/42 | 本轮无新 src 模块（改动落在既有 `app.rs` / `editor.rs` / `prompt.rs` / `search.rs`） |
| `app.*` 接线 | **43 / 44 = 97.7%**，silent 1（`app.tree.editLabel`） | 同 | `python pi-rust/scripts/app_action_coverage.py --check-consumed` → `43 entries; measured wired: 43 / in sync` |
| **composer 鼠标面** | **2 / 2 = 100%** | 1 / 2 = 50% | 点击定位光标（LUM-1426）+ 下拉框点选（本轮）；上游 `editor.ts:618-666` |
| 指针映射面 | **3 / 3 = 100%** | 3/3 | 本轮无回退（LUM-1422 的选择模型改动已丢弃，其 15 条行为测试在 LUM-1426 的模型上全绿） |
| slash 内置命令 | 18 / 23 = 78.3%（字面 17/23，`exit`≡`quit`） | 同 | 本轮未动 |
| 扩展生命周期事件 | **21/36 声明 = 58.3%**；**20/36 生产构造点 = 55.6%** | 同 | 本轮未动 |

**加权完成度（权重表见 §4.1，公式公开）**：

```text
5×1.00 + 13×0.90 + 8×1.00 + 6×0.70 + 14×0.905 + 8×0.90 + 7×0.783 + 7×0.70
  + 8×0.95 + 7×0.556 + 9×0.85 + 5×0.495 + 3×0.95 = 83.6%
轴 5 = (模块率 0.833 + 接线率 0.977) / 2 = 0.905
```

**口径对账**：唯一变动项是测试轴 0.490 → 0.495（5×0.005 = +0.03pt），加权仍落在 **83.6%**
（83.59 → 83.62，一位小数不变）。**composer 鼠标面不进公式**（公式里的 TUI 轴是模块率与接线率的均值，
不含该分项）——它的价值是「轴 6 视觉/交互保真的前提」，所以本轮**不重估**任何轴的取值，只把分项从
1/2 记到 2/2。这与 LUM-1418「列宽根治也不动加权」的处理一致。

### 0.11 LUM-1436 复测：滚轮带坐标、下拉框认领滚轮（composer 鼠标面 3/3）；`#` 触发符伪缺口更正

本轮领 LUM-1431 §7 的第一顺位，并把该节其余三条逐条复核。全部口径本机重测
（Windows / cargo 1.97.1 / `--offline`，从仓库根跑脚本）。

| 量 | 本轮实测 | LUM-1431 | 说明 |
|---|---|---|---|
| 纯代码规模（src↔src） | **88.8%**（136,016 / 153,106） | 88.8%（135,950） | `python pi-rust/scripts/measure_loc.py`；+66 行 src（`input.rs` 的滚轮坐标 + `app.rs` 的认领分支） |
| 测试规模 | **49.7%**（2,637 / 5,309） | 49.5%（2,628） | +9 = `autocomplete_wheel.rs`（7 条）+ `lum1436_autocomplete_wheel_frames.rs`（2 帧） |
| TUI 模块面 | **35 / 42 = 83.3%** | 35/42 | 本轮无新 src 模块 |
| `app.*` 接线 | **43 / 44 = 97.7%**，silent 1（`app.tree.editLabel`） | 同 | `python pi-rust/scripts/app_action_coverage.py --check-consumed` → `43 entries; measured wired: 43 / in sync` |
| **composer 鼠标面** | **3 / 3 = 100%** | 2 / 2 = 100% | 点击定位光标（LUM-1426）+ 下拉框点选（LUM-1431）+ **下拉框滚轮（本轮）**；口径由「点选」扩到「指针全部」 |
| 指针映射面 | **3 / 3 = 100%** | 3/3 | 本轮无回退 |
| slash 内置命令 | 18 / 23 = 78.3%（字面 17/23，`exit`≡`quit`） | 同 | 本轮未动 |
| 扩展生命周期事件 | **21/36 声明 = 58.3%**；**20/36 生产构造点 = 55.6%** | 同 | `python pi-rust/scripts/extension_event_coverage.py`；15 个缺失变体逐条列出，已作为独立 issue LUM-1432 派发 |

**加权完成度（权重表见 §4.1，公式公开）**：

```text
5×1.00 + 13×0.90 + 8×1.00 + 6×0.70 + 14×0.905 + 8×0.90 + 7×0.783 + 7×0.70
  + 8×0.95 + 7×0.556 + 9×0.85 + 5×0.497 + 3×0.95 = 83.6%
```

只有测试轴从 0.495 走到 0.497（+0.01pt），加权仍落在 **83.6%**。

**口径对账 / 更正**：

1. 「composer 鼠标面」与 LUM-1431 一样**不单独进公式**（公式里的 TUI 轴是模块率与接线率的均值），
   本轮只更新可复算的分项，不重估任何轴；
2. LUM-1431 §7 第 2 条「`#` 触发符空转」**实测为伪缺口**：上游
   `DEFAULT_AUTOCOMPLETE_TRIGGER_CHARACTERS = ["@", "#"]`（`packages/tui/src/components/editor.ts:251`）
   是扩展注入点，基础 `CombinedAutocompleteProvider` 无 `#` 分支且返回 `null` → 上游**不开**下拉框；
   Rust 逐字同构（`editor.rs:1913` 的 `None` → `cancel_autocomplete()`）。故该条从缺口清单移出，
   真正的 `#` 面属 provider 侧的技能/工具引用（见 `docs/LUM1436_AUTOCOMPLETE_WHEEL.md` §1.2）；
3. LUM-1431 §5 的「`cargo fmt --all -- --check` 干净」在基线 `b0c9f89a1` 上**复现不出来**
   （`lum1431_autocomplete_frames.rs` 有 4 处 rustfmt 差异，cargo 1.97.1）；本轮已修绿。

### 0.12 LUM-1432 复测：扩展生命周期事件 **20/36 → 36/36**；加权 **86.7%**

本轮只动一条轴：**扩展生命周期事件**（LUM-1431 §7 列为待办的 P0）。
细节、逐事件构造点与保真度缺口见 `docs/LUM1432_EXTENSION_EVENTS.md`。

| 量 | 本轮实测 | LUM-1431 | 说明 |
|---|---|---|---|
| 扩展事件声明面 | **36 / 36 = 100%** | 21/36 = 58.3% | `ExtensionEvent::name()` 的 wire tag 与上游 36 个名字逐一相等 |
| 扩展事件生产构造点 | **36 / 36 = 100%** | 20/36 = 55.6% | `python pi-rust/scripts/extension_event_coverage.py pi-rust` → `production emit sites: 36/36 (100.0%)` |
| 其中真钩子（返回值影响后续行为） | **10 / 36** | 0 | `context`、`before_agent_start`、`agent_settled`、`project_trust`、`session_before_{switch,fork,compact,tree}`、`session_tree`、`model_select` |

**加权完成度（权重表见 §4.1，公式公开）**：只有第 10 轴从 0.556 走到 1.000：

```text
5×1.00 + 13×0.90 + 8×1.00 + 6×0.70 + 14×0.905 + 8×0.90 + 7×0.783 + 7×0.70
  + 8×0.95 + 7×1.000 + 9×0.85 + 5×0.497 + 3×0.95 = 86.7%
```

比 §0.11 的 83.6% 高 **+3.1pt**，正是 `7×(1.000−0.556) = +3.108`——没有夹带任何其他轴的改动。

**本轮新增证据（可复跑）**：

| 证据 | 命令 / 文件 | 结果 |
|---|---|---|
| 覆盖率 | `python pi-rust/scripts/extension_event_coverage.py pi-rust` | 36/36（声明 + 生产构造点），0 缺失变体 |
| 文档对账 | `python pi-rust/scripts/extension_event_coverage.py pi-rust --check-doc` | EXIT=0（§3.6 的 36 名清单与代码同步） |
| 端到端（真事件 → 真回调 → 真行为） | `crates/pi-coding-agent/tests/extension_lifecycle_hooks.rs` | 4/4 绿：`before_agent_start` 换 system prompt、`context` 改消息、`project_trust` 决定项目扩展是否加载 |
| agent-core 缝 | `crates/pi-agent-core/tests/hooks.rs` | 2 条新增绿（6 个缝全触发 + 未装钩子时行为不变） |
| `session_before_*` veto | `crates/pi-coding-agent/src/interactive.rs` 单测 | 3 条新增绿（真 QuickJS：veto / 非 veto / 载荷字段） |
| 门禁 | `cargo test --workspace --locked --no-fail-fast` | **2646 passed / 39 failed**（基线同机 2620 / 39，失败集合相同） |
| 格式 / lint | `cargo fmt --all -- --check`、`cargo clippy … --all-targets` | fmt EXIT=0；本仓库 0 告警 |

**诚实说明**：provider 三个事件（`before_provider_request` / `before_provider_headers` /
`after_provider_response`）已按宿主真实拥有的数据构造并投递，但 `pi-ai` 的 adapter 没有
wire-payload / header 缝，**handler 返回值目前不生效**、`after_provider_response.status`
是推导值（流建立 200 / 失败 0）。逐条列在 `docs/LUM1432_EXTENSION_EVENTS.md` §3。

### 0.13 LUM-1445 复测：模态列表认领指针（picker / `ctx.ui.select` 滚轮+点选，`/settings` 点选）；TUI 指针面 6/6

本轮接 LUM-1436 §7 的第 4 条（modal 打开时滚轮归属），做下去发现是**半边**：模态列表不仅不认领滚轮，
**连点选行都不认领**（`step_modal_mouse_gesture` 只把 press/release 当"点到了模态"用于清日志选区）。
现状是：上游 `SelectList::handleMouse` 一档一格、press 高亮、click 激活
（`packages/tui/src/components/select-list.ts:110-148`），`SettingsList::handleMouse` 同构还要跳过搜索行
（`packages/tui/src/components/settings-list.ts:179-210`）。全部口径本机重测（Windows / cargo 1.97.1 / `--offline`）。

| 量 | 本轮实测 | LUM-1436 | 说明 |
|---|---|---|---|
| 纯代码规模（src↔src） | **89.1%**（136,469 / 153,106） | 88.8%（136,016） | `python pi-rust/scripts/measure_loc.py`；+453 行 src（geometry + 路由 + 三处激活落点 + 驱动放行） |
| 测试规模 | **49.9%**（2,651 / 5,309） | 49.7%（2,637） | +14 = `modal_pointer.rs`（11 条）+ `lum1445_modal_pointer_frames.rs`（2 帧）+ 驱动用例（1 条） |
| TUI 模块面 | **35 / 42 = 83.3%** | 35/42 | 本轮无新 src 模块 |
| `app.*` 接线 | **43 / 44 = 97.7%**，silent 1（`app.tree.editLabel`） | 同 | `python pi-rust/scripts/app_action_coverage.py --check-consumed` → `43 entries; measured wired: 43 / in sync` |
| composer 鼠标面 | **3 / 3 = 100%** | 3 / 3 | 本轮无回退 |
| **模态列表指针面** | **3 / 3 = 100%** | 0 / 3 | picker（滚轮+点选）/ `ctx.ui.select`（滚轮+点选）/ settings（点选；滚轮此前已有） |
| 扩展生命周期事件 | **21/36 声明 = 58.3%**；**20/36 生产构造点 = 55.6%** | 同 | 本轮自做面不含此轴；同一条 tip 上 §0.12 的 LUM-1432 已把它推到 36/36，合并后以 §0.12 为准 |

**加权完成度（权重表见 §4.1，公式公开）**：

本轮**自己**的增量只有测试轴（0.497 → 2,651/5,309 = 0.499）：

```text
5×1.00 + 13×0.90 + 8×1.00 + 6×0.70 + 14×0.905 + 8×0.90 + 7×0.783 + 7×0.70
  + 8×0.95 + 7×0.556 + 9×0.85 + 5×0.499 + 3×0.95 = 83.6%
```

单看本轮：**83.6%**（与 §0.11 同一位小数，+0.01pt）。但本轮同时把 §0.12 的 LUM-1432 并入了
`feature/pi.rs`，所以**分支 tip 的合计值是 86.7%**（扩展轴 0.556 → 1.000，`+7×0.444 = +3.108pt`；
测试轴再 +0.01pt）：

```text
5×1.00 + 13×0.90 + 8×1.00 + 6×0.70 + 14×0.905 + 8×0.90 + 7×0.783 + 7×0.70
  + 8×0.95 + 7×1.000 + 9×0.85 + 5×0.499 + 3×0.95 = 86.7%
```

**口径对账**：

1. 「模态列表指针面」同样**不单独进公式**（TUI 轴是模块率与接线率的均值，§0.11 已声明），
   它与「composer 鼠标面」合起来是本轮可复算的 **TUI 指针面 6 / 6 = 100%**；
2. 上游的 `shouldDeferViewportInputToOverlay`（`packages/tui/src/tui-alt-screen.ts:645-694`）语义是
   "命中 overlay 但组件没处理 → 归还未消费"；`pi-tui` 无下游传播链路，等价实现是**丢弃**，
   两条分支的观感一致（日志不动），已在代码注释里写明；
3. settings 的滚轮仍是**全局认领**（`step_settings_wheel`），与上游的矩形前提不同 —— 这是本轮
   **明确保留**的偏差（改动会碰既有 3 条测试），记入 `docs/LUM1445_MODAL_POINTER.md` §8 第 5 条。

### 0.14 LUM-1447 复测：两个死键位接线 + 转录折叠提示按生效键位渲染；`tui.*` 消费面 49/49

本轮只动 `pi-tui`（+1 个扫描脚本），不改任何轴的分母；公式里唯一变动的项是测试轴。
细节、反证与帧截图见 `docs/LUM1447_BINDING_HINTS.md`。

| 量 | 本轮实测 | LUM-1445 | 说明 |
|---|---|---|---|
| 纯代码规模（src↔src） | **90.2%**（138,153 / 153,106） | 90.1%（137,961） | `python pi-rust/scripts/measure_loc.py`；+192 行 src |
| 测试规模 | **50.3%**（2,670 / 5,309） | 50.2%（2,663） | +7 = `editor.rs` 3 条单测 + `tests/hint_bindings.rs`（1 条）+ `tests/lum1447_binding_hints_frames.rs`（3 帧） |
| TUI 模块面 | 35 / 42 = 83.3% | 同 | 本轮无新 src 模块 |
| `app.*` 接线 | 43 / 44 = 97.7%，silent 1（`app.tree.editLabel`） | 同 | 未动 |
| **TUI 键位消费面（本量）** | **`tui.*` 49 / 49 = 100%**（本轮前 **47/49**） | —— | 新脚本 `python pi-rust/scripts/keybinding_coverage.py pi-rust --check` → exit 0；死键位为 `tui.editor.historyPrevious` / `historyNext` |
| 扩展生命周期事件 | 36/36（声明 + 生产构造点） | 同 | 未动 |

**加权完成度（权重表见 §4.1，公式公开）**：只有第 12 轴从 0.499 走到 2,670/5,309 = **0.5029**：

```text
5×1.00 + 13×0.90 + 8×1.00 + 6×0.70 + 14×0.905 + 8×0.90 + 7×0.783 + 7×0.70
  + 8×0.95 + 7×1.000 + 9×0.85 + 5×0.5029 + 3×0.95 = 86.8%
```

**口径对账**：把第 12 轴换回 0.499 时同一算式为 **86.75%**（§0.13 报作 86.7%），
所以这 0.1pt 差额**全部**来自「用实测的 2,670/5,309 替掉旧的 0.499」，不是任何轴被重估
（本轮没有一条轴的能力发生变化）。

**本轮新登记的量与脚本（可反证）**：

| 证据 | 命令 | 结果 |
|---|---|---|
| 死键位扫描 | `python pi-rust/scripts/keybinding_coverage.py pi-rust --check` | exit 0：`tui.* 49/49`、`app.* 43/44`、`in sync: 1 known-unconsumed` |
| 反证 | 把 `editor.rs` 的两个分支 stash 掉再跑上面那条 | exit 1，精确报出 `UNCONSUMED tui.editor.historyPrevious` / `historyNext` |
| 行为 | `cargo test -p pi-tui --lib history_chord` | 3 passed / 0 failed；删掉两个分支后立刻 2 failed |
| 提示 | `cargo test -p pi-tui --test hint_bindings` | 1 passed：默认 / override / 双键位 / 解绑 / 未知 id 五种状态 |
| 帧 | `docs/screenshots/lum1447-fold-hint-{default_ctrl_o,rebound_ctrl_u,unbound}.{png,txt}` | 3 帧，80×20，真 `App::render_to_buffer` |
| 门禁 | `cargo test -p pi-tui` / `cargo fmt --all -- --check` / `cargo clippy -p pi-tui --all-targets` | **1027 passed / 0 failed**（基线 1020）；fmt 干净；clippy 0 告警 |

**诚实说明**：`tool_fold_hint` 在**裸 `pi-tui` 注册表**（没有 `app.*` 表）下走
`Ctrl+O` 兜底，因为 `app.tools.expand` 的 id 定义在 `pi-coding-agent` 的表里；
表里**有**该 id 而用户解绑时才去掉键位（`docs/LUM1447_BINDING_HINTS.md` §3 有两条分支的理由）。
`/help` 的 `keys:` 段与 dialog/settings 页脚仍是硬编码 chord，**本轮不算已修**，
列为下一轮第 2、3 条。

### 0.15 LUM-1448 复测：扩展注入 autocomplete provider（宿主能力面新增一行「autocomplete provider 注入」）；加权仍 **86.8%**

本轮动 `pi-tui` + `pi-extensions` + `pi-coding-agent`，不改任何轴的分母，也没有一条轴的能力被重估；
公式里唯一变动的项仍是测试轴。细节、决策（异步化三选一）、逐条上游对照与 8 条偏差见
`docs/LUM1448_AUTOCOMPLETE_PROVIDER.md`。

| 量 | 本轮实测 | LUM-1447 | 说明 |
|---|---|---|---|
| 纯代码规模（src↔src） | **91.1%**（139,505 / 153,106） | 90.2%（138,153） | `python pi-rust/scripts/measure_loc.py`（仓库根）；+1,352 行 src（新模块 2 个 + host/shim/editor/wiring/interactive） |
| 测试规模 | **50.8%**（2,670 + 26 = 2,696 / 5,309） | 50.3%（2,670） | 同一命令在改动前后各跑一次：`grep -rn --include=*.rs -E '^\s*#\[(tokio::)?test\(\(|\]\)' pi-rust/crates \| wc -l` → 基线 2,721 / 本轮 2,747（**+26**）。二者绝对数比 §0.14 的 2,670 高 51，是本行口径与上轮的差异（上轮未记录命令）；**为不重估历史轴，此处只把同命令的 +26 增量加在 §0.14 的 2,670 上** |
| TUI 模块面 | 35 / 42 = 83.3% | 同 | 本轮无新 `pi-tui` src 模块（`pi-extensions`/`pi-coding-agent` 各 +1，不计入该轴） |
| `app.*` 接线 | 43 / 44 = 97.7%，silent 1（`app.tree.editLabel`） | 同 | 未动 |
| `tui.*` 消费面 | 49 / 49 = 100% | 同 | 未动 |
| **宿主能力面（本量，§3.6）** | **新增一行：`ctx.ui.addAutocompleteProvider` = 已支持（同步子集）** | 未覆盖 | 上游 wrapper 链（`current` 透传 + `triggerCharacters` 去重）在 shim 实现；宿主侧同步 `host_ui_autocomplete` 提供内置 provider 作链尾；Rust→JS 走 `_pi_autocomplete_call`。已知偏差：只服务同步回调 / `options.signal` 恒不 abort。该轴仍取 **95%**（子项补齐不等于重估） |
| 扩展生命周期事件 | 36/36 | 同 | 未动 |

**加权完成度（权重表见 §4.1）**：只有第 12 轴从 0.5029 走到 0.5029 + 26/5,309 = **0.5078**：

```text
5×1.00 + 13×0.90 + 8×1.00 + 6×0.70 + 14×0.905 + 8×0.90 + 7×0.783 + 7×0.70
  + 8×0.95 + 7×1.000 + 9×0.85 + 5×0.5078 + 3×0.95 = 86.79% → **86.8%**
```

即“测试轴 +26 条”贡献 **+0.025pt**（86.7655 → 86.79），一位小数上仍为 **86.8%**。
本轮**没有**因新能力而抬高任何轴——宿主能力面本来就在 95%，`addAutocompleteProvider` 是把
一个空子项填上，不是新的能力层。

**本轮新登记的量与门禁（可反证）**：

| 证据 | 命令 | 结果 |
|---|---|---|
| 宿主层（真 QuickJS + 真 fixture） | `cargo test -p pi-extensions --test autocomplete` | **8 passed / 0 failed** |
| 交互层（真 `App` + frame-buffer 帧） | `cargo test -p pi-coding-agent --test lum1448_autocomplete_provider_frames` | **5 passed / 0 failed**（3 帧） |
| 接线 | `cargo test -p pi-coding-agent --lib install_extension_autocomplete` | 1 passed |
| 帧 | `docs/screenshots/lum1448-autocomplete-provider-{hash,tab,enter}-78x14.{png,txt}` | 3 帧，78×14，真 `App::render_to_buffer`；`python pi-rust/scripts/frame_to_png.py` 上色 |
| 门禁 | `cargo test -p pi-tui` / `-p pi-extensions` / `-p pi-coding-agent`；`cargo fmt --all -- --check`；`cargo clippy -p pi-tui -p pi-extensions -p pi-coding-agent --all-targets` | **1034/0**（基线 1027/0）、**131/5**（基线 120/5）、**823/28**（基线 815/28）；fmt exit 0；clippy 0 条新告警 |

**诚实说明**：三条新测试文件全是**真驱动**（真 QuickJS host + 真 `App` + 磁盘上的真 fixture），
但本机（Windows runner）**没有 PTY**，所以 Tab/Enter 走的是 `App::step(InputEvent::Key)`，
不是 crossterm 字节流；截图是 frame-buffer 通道而非 PTY 实拍。上游 TS 侧无法交叉验证
（本机无 `node_modules`），所以“与上游一致”的判据是**源码逐行核对**（见交付文档 §1）。
`pi-extensions` 的 5 条 / `pi-coding-agent` 的 28 条失败与本轮**逐字同名**（Windows 环境类：
临时路径分隔符、`ls`/`bash`、绝对路径拒绝），`diff` 两边失败名集合为空。
### 0.16 LUM-1450 复测（并入 LUM-1448 之后）：模态列表与提示面统一按生效键位；提示面硬编码 chord **19 → 0**

本轮改 `pi-tui`（selector / settings / dialog / app / prompt）+ `pi-coding-agent`
（`commands/slash.rs` / `interactive.rs` / `text_fallback.rs`），细节、反向验证与帧截图见
`docs/LUM1450_HINT_CHORDS.md`。

下表前两行是**本轮自己的分支**（基于 `c6d6df108`，即 §0.14 的 tip）上的数字；
本轮到分支时 `origin/feature/pi.rs` 已被 §0.15（LUM-1448）推进，所以合并后的口径见
本节末尾的「合并后复测」——两份数字分开写，不用本轮自测值冒充合并后的值
（LUM-1445 §10 的同一条规矩）。

| 量 | 本轮自测（`c6d6df108` + 本轮） | LUM-1447 | 说明 |
|---|---|---|---|
| 纯代码规模（src↔src） | **90.5%**（138,635 / 153,106） | 90.2%（138,153） | `python pi-rust/scripts/measure_loc.py`；+482 行 src |
| 测试规模 | **50.6%**（2,684 / 5,309） | 50.3%（2,670） | +14 = `select_list_keybindings` 9 + `lum1450_dialog_frames` 2 + `lum1450_help_legend_frames` 2 + slash 1 |
| TUI 模块面 | 35 / 42 = 83.3% | 同 | 本轮无新 src 模块 |
| `app.*` 接线 | 43 / 44 = 97.7%（silent 1：`app.tree.editLabel`） | 同 | 未动 |
| `tui.*` 消费面 | 49 / 49 = 100% | 同 | 未动。**本轮发现该口径漏报**：`Selector` 写死 `KeyCode::Enter` 时 `tui.select.confirm` 也算「已消费」，因为它只数「字面量消费者」。见 §6 与 LUM-1450 §7 第 2 条 |
| 扩展生命周期事件 | 36/36 | 同 | 未动 |
| **提示面硬编码 chord（新登记）** | **0 硬编码 / 5 知会**（基线 **19 / 5**） | ——（未测） | `python pi-rust/scripts/hint_chord_literals.py pi-rust --check`；双向 `--check`（新硬编码报错 + 知会项失效也报错） |
| **模态列表键位来源（新登记）** | **2 / 2 组件 + 3 / 3 页脚** | 0 / 2 + 0 / 3（全部写死 `KeyCode`） | `Selector`、`SettingsList` 的 up/down/confirm/cancel/pageUp/pageDown 全走 `kb.matches`；Confirm/Input/Select 三处页脚全由 `key_text_or` 生成 |

**加权完成度（权重表见 §4.1，公式公开）**：13 个轴里只有第 12 轴动了，
`2,670/5,309 = 0.5029` → `2,684/5,309 = 0.5056`：

```text
5×1.00 + 13×0.90 + 8×1.00 + 6×0.70 + 14×0.905 + 8×0.90 + 7×0.783 + 7×0.70
  + 8×0.95 + 7×1.000 + 9×0.85 + 5×0.5056 + 3×0.95 = 86.8%
```

一位小数不变。**这不是「本轮没事干」**，而是说：本轮关掉的是「广告的键位不生效」这类
缺陷，它不在 §4.1 那 13 个轴的任何一个里——轴 5 用的是「模块率与 `app.*` 接线率的均值」，
而 `app.*` 一列一动没动。所以本轮的价值落在两个**新登记、可反证**的轴上（提示面 0/19；
模态列表键位来源 2/2 + 3/3）。若把「提示面」当成一条轴（权重 5%），它会从 0/19 = 0%
走到 0 硬编码 = 100%（+5.0pt），但**本轮不这么算**：
一条新轴要先进 §4.1 的权重表、说清权重怎么来的，再进公式；
先把测量做实，下一轮再谈加权。

**TUI 交互+视觉轴**：`(14×0.905 + 8×0.90) / 22 = 90.3%`。

本轮的门禁与三条反向验证（均在 stash 掉源码后的基线上复现失败）见
`docs/LUM1450_HINT_CHORDS.md` §4。

#### 0.16.1 合并后复测（`feature/pi.rs` = LUM-1448 + LUM-1450，本轮亲自跑）

合并基：`ac9df2a1f`（LUM-1448）；冲突只有 `RUST_TS_PARITY_METRICS.md` 一处（两边各加了一节
`§0.15`）——解决方式：**两节都留**，LUM-1448 作 §0.15、本轮作 §0.16；
`interactive.rs` 两边各自新增的内容 git 自动合并，合并后逐字复核了两处（§3 表格里的
bash 忙提示与 `/settings` 描述），无丢失。

| 量 | 合并后实测 | 本轮自测 | 差额来自 |
|---|---|---|---|
| 纯代码规模（src↔src） | **91.5%**（140,032 / 153,106） | 90.5%（138,635） | LUM-1448 的 1,397 行 src |
| 测试规模 | **51.1%**（2,711 / 5,309） | 50.6%（2,684） | LUM-1448 的 +26 与 1 条计数口径差 |
| `pi-tui` 全量 | **1045 passed / 0 failed** | 1038 / 0 | LUM-1448 的 7 条 |
| `pi-coding-agent --lib` | **584 passed / 8 failed** | 581 / 8 | LUM-1448 的 3 条；8 个失败**逐字同名** |
| 其余 target 失败集 | `cli_extensions` 3/10、`cli_tools` 0/4、`reload_config` 3/1、`system_prompt_resources` 4/1、`tools` 35/1、`tools_navigation` 26/3 | 同 | 与合并前**逐条相同**，均为 Windows 环境类 |
| 提示面硬编码 chord | **0 / 5 知会** | 同 | —— |
| `keybinding_coverage.py --check` | exit 0（`tui.* 49/49`、`app.* 43/44`） | 同 | —— |
| `app_action_coverage.py --check-consumed` | exit 0（`43 wired; in sync`） | 同 | —— |

合并后加权（第 12 轴 = 2,711/5,309 = 0.5107）：

```text
5×1.00 + 13×0.90 + 8×1.00 + 6×0.70 + 14×0.905 + 8×0.90 + 7×0.783 + 7×0.70
  + 8×0.95 + 7×1.000 + 9×0.85 + 5×0.5107 + 3×0.95 = 86.80% → **86.8%**
```

**口径对账**：`grep -rhoE '#\[(tokio::)?test\]' pi-rust/crates --include=*.rs | wc -l` 在合并后 tip
上给出 **2,711**，比「§0.14 的 2,670 + LUM-1448 的 26 + 本轮的 14 = 2,710」多 1 条；
差额来自 §0.15 自己就记过的**同一命令在不同轮次给出 ±51** 的口径差（它当时只在 2,670 上加增量，
没有重估历史轴）。本轮同样**不重估** §0.14/§0.15 的旧值，只把合并后实测量登记在这里。

### 0.17 LUM-1460 复测：抢救 LUM-1328/LUM-1318 的 composer 粘贴通道（`pi-rust/crates` 里此前 **0 命中**）；加权仍 **86.8%**

本轮**不是新功能**，是把一条已写完却从未进入 `feature/pi.rs` 的交付救回来
（提交 `6631b8c77` / `9c20bd8f1` 只存在于 `origin/work/LUM-1328` 与 `origin/work/LUM-1318`，
`git merge-base --is-ancestor` 两条都是 NOT），并适配当前 tip 的
`HistoryEntry` / `Submission` 模型。根因是上一轮收编检查的**假阳性**
（`git grep 'paste #'` 不限路径 → 命中上游 `packages/tui/src/components/editor.ts`
与 pi-rust 的 md 文档，`-- pi-rust/crates` 是 **0**）。细节、逐行对照、偏差清单与
frame-buffer 截图见 `docs/LUM1460_PASTE_RESCUE.md`。

| 量 | 本轮实测 | 基线 `815b21d13` | 说明 |
|---|---|---|---|
| 纯代码规模（src↔src） | **91.8%**（140,615 / 153,106） | 91.5%（140,032） | `python pi-rust/scripts/measure_loc.py`；+583 行 src |
| 测试规模 | **51.6%**（2,740 / 5,309） | 51.1%（2,711） | `grep -rhoE '#\[(tokio::)?test\]' pi-rust/crates --include=*.rs \| wc -l`，基线在同一命令下于 `lum-1457` worktree（tip `815b21d13`）复测 |
| `pi-tui` 全量 | **1075 passed / 0 failed** | 1045 / 0 | `cargo test --offline -p pi-tui`（57 target），+30 = `composer_paste`（27）+ `lum1460_paste_frames`（3）|
| TUI 模块面 | 35 / 42 = 83.3% | 同 | 无新模块 |
| `app.*` 接线 | 43 / 44 = 97.7%（silent 1：`app.tree.editLabel`） | 同 | 未动 |
| `tui.*` 消费面 | 49 / 49 = 100% | 同 | 未动 |
| 扩展生命周期事件 | 36 / 36 | 同 | 未动 |
| **composer 粘贴能力面（本期新登记轴）** | **7 / 7 = 100%** | **0 / 7 = 0%** | 7 行逐条挂在 `tests/composer_paste.rs` 的用例上，见交付文档 §4 |

**加权完成度（权重表见 §4.1，公式公开）**：13 条轴里只有第 12 轴动了，
`2711/5309 = 0.5107` → `2740/5309 = 0.5161`：

```text
5×1.00 + 13×0.90 + 8×1.00 + 6×0.70 + 14×0.905 + 8×0.90 + 7×0.783 + 7×0.70
  + 8×0.95 + 7×1.000 + 9×0.85 + 5×(2740/5309) + 3×0.95 = 86.83% → **86.8%**
```

即一位小数不变（86.80 → 86.83，**+0.03pt**）。**粘贴通道不单独进公式**：它是轴 5
（TUI 交互面）内部的一格，而轴 5 的输入是"模块率与 `app.*` 接线率的均值"，两条都没动。
按 LUM-1450 立下的规矩（"新轴要先说清权重来源再进 §4.1"），本轮只把它作为
**新登记、可反证的量**公开（0/7 → 7/7）。

**本轮的门禁与证据（可复跑）**：

| 证据 | 命令 | 结果 |
|---|---|---|
| 粘贴行为 | `cargo test --offline -p pi-tui --test composer_paste` | **27 / 0**（含删除重编号 3 条、原子编辑 3 条、撤销/历史 3 条） |
| 帧 | `cargo test --offline -p pi-tui --test lum1460_paste_frames` | **3 / 0** |
| 全仓编译 | `cargo check --offline --workspace --all-targets` | exit 0（只剩 `rquickjs-core`(vendor) 与 `pi-extensions` 的既有告警） |
| 格式 / lint | `cargo fmt --all -- --check`；`cargo clippy --offline -p pi-tui -p pi-coding-agent --all-targets` | fmt exit 0；`pi-tui` / `pi-coding-agent` **0 告警**（老分支带来的 `question_mark` 1 条已修） |
| 帧截图 | `docs/screenshots/lum1460-paste-{marker,two-markers,submitted}-100x24.png`(+`.txt`) | 3 帧，100×24，真 `App::render_to_buffer`；`python pi-rust/scripts/frame_to_png.py` 上色 |
| 丢失证据 | `git grep -c 'paste #' origin/feature/pi.rs -- pi-rust/crates \| wc -l` | **0**（整树 12 文件全是上游 TS 与 md 文档） |

**诚实说明**：本机（Windows runner）**没有 PTY**，三张截图是 frame-buffer 冻结帧，
证明"画出来的东西"（composer 只有一行 marker；提交后 transcript 是 12 行正文），
**不证明按键/字节时序**——时序由 27 条 `App`/`Editor` 级驱动用例覆盖。
老分支的真 PTY 场景（`lum1328-paste.json` / `lum1318-paste-fold.json`）与基线截图
**没有带进本轮**（在本机跑不了）；`docs/screenshots/lum1328-paste*.png` 是那条分支的历史实拍，
不是在本轮代码上拍的。

### 0.18 LUM-1464 复测：`?` 键假广告收口（footer 广告 `? for help`，基线 `pi-rust/crates` 里 `Char('?')` **0 命中**）；加权 **86.8% → 86.9%**（只有测试轴 +0.05pt）

**量的是什么**：footer 文案里的**单字符 chord** 有没有兑现。基线取证（限定路径，不重蹈 LUM-1460 的整树假阳性）：

```bash
$ git grep -n "Char('?')" origin/feature/pi.rs -- pi-rust/crates | wc -l
0
$ git grep -n '"\? for help"' origin/feature/pi.rs -- pi-rust/crates
origin/feature/pi.rs:pi-rust/crates/pi-tui/src/app.rs:1384:  status_data.hint = Some("? for help".to_string())
```

对照 codex（`bottom_pane/chat_composer.rs:3154-3157` → `footer.rs::shortcut_overlay_lines`）与 Martty
（`src/input/keymap.rs:97` 空输入 `Ctrl+K` → `ShowKeys`）：两边都有「空输入 + 单键 = 键位速查表」，
pi-rust 本轮补齐，并保留「任何其它键先关掉它、再照常处理」（codex `reset_mode_after_activity`）。

| 口径 | 本轮 | 上一快照（LUM-1460） |
|---|---|---|
| 纯代码规模（src↔src） | **92.0%**（140,894 / 153,106） | 91.8%（140,615 / 153,106） |
| 测试规模 | **49.5%**（2,755 / 5,563） | 49.4%（2,740 / 5,309） |
| `app.*` 接线 | 43/44 = **97.7%** | 43/44 |
| 扩展生命周期事件 | **36/36** 声明 + **36/36** 生产构造点 | 同 |
| TUI 模块 | **36/42 = 85.7%** | 35/42 |
| TUI 交互+视觉轴 | 轴 5 = (0.857 + 0.977)/2 = **91.7%**；轴 6 = 90% | 同形 |
| 加权完成度 | **86.9%** | 86.8% |

> **口径声明（两条）**：① TS 用例分母由 5,309 改为 5,563 是**换口径**（旧口径只数 `it(`/`test(`，
> 新口径收 `it.each(` 等包装），不是 TS 测试变多——所以测试轴上那 0.1pt 不可当成绩读；
> ② TUI 模块 35→36 是**别的轮次并入的模块**，本轮没有新增模块，也没有关闭任何「能力有没有」的轴，
> 加权从 86.8 到 86.9 只有测试轴的 +0.05pt。**本轮交付的是可信度（广告可兑现），不是覆盖率**。

**门禁**：`cargo fmt --all -- --check` 干净；`cargo clippy --offline -p pi-tui --all-targets` 0 warning；
`cargo test --offline -p pi-tui` **1090 / 0**（基线 1075/0 → +15）；
`cargo test --offline -p pi-coding-agent -j 2 --no-fail-fast` **826 / 28**（28 条全为 Windows 环境类；
`reload_rereads_keybindings_and_ui_settings_mid_session` 已用 `git stash` 在基线复现同一条 FAILED →
新增失败 0）；**反向验证**：禁用触发分支 → 5 条测试立刻红。

**证据分级**：4 张 `docs/screenshots/lum1464-hints-*.png`(+`.txt`) 是 frame-buffer 冻结帧
（本机无 `pty`），证明几何与内容，**不证明按键时序**；时序由 `tests/shortcut_overlay.rs` 的 11 条覆盖。
本轮审计与缺口清单见 `docs/LUM1464_SHORTCUT_OVERLAY.md`。

### 0.19 LUM-1467 复测：footer stats 行对齐上游字段（`↑/↓`、`CH%`、`$cost`、`(auto)`、`(provider)` 前缀）；加权仍 **86.9%**

**量的是什么**：`pi-rust` 的第二行 footer（stats 行）与上游 `footer.ts:130-200` 的**字段差**。
基线（LUM-1466 交付后）缺四类：箭头形状（`in/out` vs `↑/↓`）、`CH%`、`$cost (+sub)`、
`(auto)`、多 provider 的 `(provider)` 前缀；且行内排布是 model 左 / stats 右，上游相反。
本轮全部补齐，并改成上游的 stats 左 / model 右。

| 口径 | 本轮 | 上一快照（LUM-1464，§0.18） |
|---|---|---|
| 纯代码规模（src↔src） | **92.7%**（141,902 / 153,106） | 92.0%（140,894 / 153,106） |
| 测试规模（新口径 / 旧口径） | **50.2%**（2,790 / 5,563） / 52.6%（2,790 / 5,309） | 49.5%（2,755 / 5,563） |
| Rust 用例数（`crates/**`） | **2,790** | 2,755 |
| 轴 5 TUI 交互面 | 轴 5 = (0.857 + 0.977)/2 = **91.7%** | 同形 |
| 轴 6 TUI 视觉保真 | **90%（不重估）** | 90% |
| 轴 12 测试与门禁强度 | 2,790/5,563 = **0.5015** | 0.4953 |
| 加权完成度 | **86.9%** | 86.9% |

**为什么总分不动**：本轮没有关闭任何“能力有没有”的轴。它是一个 **TUI 视觉保真度**的
缺口——但 §4.1 的轴 6 依据是 **§3.8**（主题 / Markdown / 高亮 / LaTeX / 图片），footer 的
stats 行不在那 5 项里，所以本轮**换的是证据，不是数字**。把轴 6 从 0.90 推到 0.95 只值
+0.4pt（`8×0.05`），不足以改变结论，本文**不动**它（与 §0.8 的处置同一条规矩；要重估先得在
§3.8 里说清哪一项抬了多少）。加权和里唯一动的是第 12 轴 **+0.015pt**（+17 条用例）：

```
5×1.000 + 13×0.90 + 8×1.000 + 6×0.70 + 14×0.917 + 8×0.90 + 7×0.783
+ 7×0.70 + 8×0.95 + 7×1.00 + 9×0.85 + 5×0.5015 + 3×0.95 = 86.93% ≈ 86.9%
```

**口径诚实声明**：TS 分母 5,563 沿用 §0.18 的新口径；用 §1.1 的旧口径
（`\bit\(|\btest\(`）在同一棵树上量到 **5,309**，对应第 12 轴 0.5255、加权 **87.05%**。
两个数都对，取决于用哪个尺子；为与上一快照逐项对照，这里沿用 §0.18 的尺子并写明差额
（+0.12pt）。

**门禁**（本机 Windows / cargo 1.97.1 / `--offline`）：`cargo fmt --all -- --check` 干净；
`cargo clippy -p pi-tui --all-targets` **0 warning**；`cargo test -p pi-tui` **1121 / 0**
（基线 1104/0 → +17）；`cargo test -p pi-coding-agent --lib interactive::` **107 / 0**；
`--test reload_config` 的 `reload_rereads_keybindings_and_ui_settings_mid_session` **既有失败**
（已 `git stash` 掉本轮 `interactive.rs` 后在基线复现同一条），**新增失败 0**。

**证据分级**：3 张 `docs/screenshots/lum1467-stats-*.png`(+`.txt`) 是 frame-buffer 冻结帧
（本机无 `pty`），证明几何与字段内容，**不证明按键时序**；交互时序由 `status.rs` 的 37 条
单测与 `tests/lum1467_stats_fields_frames.rs` 的 6 条覆盖。本轮细节、上游取证、已知偏差
见 `docs/LUM1467_FOOTER_STATS_FIELDS.md`。

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

**宿主能力面（强）**：`pi-extensions/docs/EXTENSIONS.md` 表格列出宿主 import：`registerTool/registerCommand/registerProvider/unregisterProvider/appendEntry/sendMessage/sendUserMessage/setSessionName/exec(+cancel)/fetch(+cancel)/ui.notify/ui.confirm/ui.input/ui.select/ui.custom/setWidget/setHeader/setFooter/setEditorComponent/**ui.addAutocompleteProvider**(LUM-1448)/log/child_process(zlib,crypto,fs,os,process via shim)/pi_ai_stream_start`。对照 TS `docs/extensions.md` 的 ~21 个 `pi.*`：宿主能力类**基本全覆盖**，缺 `setActiveTools、setThinkingLevel、setLabel、registerShortcut、registerFlag、registerEntryRenderer、registerMessageRenderer、registerMarkdownTransformer、getAllTools`（9 个）→ 覆盖面约 **57%–95%**，取 **95%**（能力等价入口多于 TS 的 UI 面，例如 `ctx.ui.custom` 的 overlay 生命周期在 Rust 侧有完整 host op）。

新增一行（LUM-1448）：**autocomplete provider 注入 = 已支持（同步子集）**。
`ctx.ui.addAutocompleteProvider(factory)` 的 wrapper 链（`current` 透传 + `triggerCharacters` 去重）
在 shim 里实现，宿主侧通过同步 `host_ui_autocomplete(op, json)` 提供内置
`CombinedAutocompleteProvider` 作为链尾，Rust→JS 走 `_pi_autocomplete_call(op, json)`；
布局/触发符表对齐 `interactive-mode.ts:734-745`。已知偏差：**只服务同步回调**
（Promise-returning 回调不 await，该次调用回落内置 provider 并告警一次）、`options.signal` 恒不 abort。
逐条对照与证据见 `docs/LUM1448_AUTOCOMPLETE_PROVIDER.md`。

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
>
> **已关闭（LUM-1432）**：该轴**已补满**——上游 36 个事件名全部有 Rust 变体且全部有生产构造点
> （`production emit sites: 36/36`），其中 10 个是返回值真影响后续行为的钩子。
> 逐事件构造点表、别名表、端到端证据与 6 条保真度缺口见 `docs/LUM1432_EXTENSION_EVENTS.md`；
> 本轮数字见 **§0.12**。

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
| 10 | 扩展生命周期事件 | 7% | 100% | LUM-1432：36/36 声明 + 36/36 生产构造点（`scripts/extension_event_coverage.py`）；10 个为真钩子，见 §0.12 |
| 11 | 会话 / 存储 / 导入导出兼容 | 9% | 85% | 读兼容、写待补 |
| 12 | 测试与门禁强度 | 5% | 45% | 用例 2,232 vs 5,309 |
| 13 | 子包完整度（server/client/chord/telemetry/evals/protocol） | 3% | 95% | §3.10 |

加权和 = 5×1.00 + 13×0.90 + 8×1.00 + 6×0.70 + 14×0.58 + 8×0.90 + 7×0.74 + 7×0.70 + 8×0.95 + 7×0.20 + 9×0.85 + 5×0.45 + 3×0.95 = **76.05%**

### 4.2 敏感性（诚实说明）

- 把扩展事件轴从 20% 提到 100%（即补完 36 个事件）：总分为 **81.7%**——**这是全表最大的单一摆动项**，说明"扩展生态兼容"是当前性价比最高的攻坚方向。
  > **已兑现（LUM-1432）**：该轴已补到 100%，实测总分 **86.7%**（§0.12）；本条保留为当时的敏感性估计。
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

1. **已关闭（LUM-1432）· 扩展生命周期事件**：原 P0「7/36 声明、2/36 运行期」已补到 **36/36 声明 + 36/36 生产构造点**（§0.12）；剩余是 6 条保真度缺口（provider 三个事件返回值不生效、`context` 不链式、`session_before_*` 的 `willRetry`/摘要语义缺），见 `docs/LUM1432_EXTENSION_EVENTS.md` §3。
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

1. 给 `pi-ai` adapter 加 `on_payload` / `transform_headers` / `on_response` 回调，让 provider 三个事件的返回值真生效（§6.1 的保真度缺口）——这是扩展事件轴剩下的最后一截。
2. 补 `models.* / session.* / tree.*` 快捷键接线（+2.9pt），顺手修 `@` 补全首行重复。
3. 补 CLI 的 `--theme/--thinking/--tools/--provider/--offline` 与 4 个高价值 slash 命令。
4. 补测试：把 Rust 用例数从 2,232 往 5,309 靠（当前 42%，是最大的"隐藏债务"）。
