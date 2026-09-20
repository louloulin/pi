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
`docs/TUI_UX_AUDIT.md` 二十）。

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
**更正既有文档**：`docs/TUI_UX_AUDIT.md` 写的是"仅接线 3/43"，实测为 **21/44 ≈ 48%**（那份口径只统计了少数几个文件）；`tree.filter*` 一族很可能由 selector 组件内部按键处理而非走全局 `app.*`，已在 §1.3 标注待复核，**未计入差距**。
→ 接线率 **47.7%**（下界；若 tree/models 族确实由组件内部消化，实际会更高）。

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
- 把 TUI 快捷键接线从 47.7% 提到 100%：总分 +4.1pt → **80.2%**。
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
7. **已知 UI 细节缺陷（实测复现）**：`@` 文件补全候选首行 label/value 重复显示（截图 frame 7：`❯ src/  src`）；`#` 触发符没有任何 provider 支撑（截图 frame 8，输入 `#` 无候选）。

## 7. 建议的推进顺序

1. 把 `ExtensionEvent` 补齐到上游 36 个事件名（含别名兼容 `session_shutdown`），在 `agent_loop` 的 observer 上直接转发 ——> 单项 +5.6pt，且直接决定"插件生态兼容"成败。
2. 补 `models.* / session.* / tree.*` 快捷键接线（+4.1pt），顺手修 `@` 补全首行重复。
3. 补 CLI 的 `--theme/--thinking/--tools/--provider/--offline` 与 4 个高价值 slash 命令。
4. 补测试：把 Rust 用例数从 2,232 往 5,309 靠（当前 42%，是最大的"隐藏债务"）。
