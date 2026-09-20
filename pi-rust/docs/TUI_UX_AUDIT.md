# pi-rust TUI 用户体验审计（首轮）

审计对象：`pi-rust/crates/pi-tui` + `pi-rust/crates/pi-coding-agent` 的交互模式（alt-screen TUI）。
对照物：上游 TS `packages/coding-agent/src/modes/interactive/interactive-mode.ts`、
`packages/tui/src/tui-alt-screen.ts`，以及 [Martty](https://github.com/louloulin/Martty)（ratatui 实现的
DeepSeek Harness TUI）。

本文件是可执行的差距清单：每条都给出「证据 → 影响 → 修复方向 → 风险」，第五节是可直接派给
worker 的 stage 划分。

## 结论

1. 渲染底座已经不缺：`pi-tui` 27,895 行、30 个模块，markdown / 高亮 / 主题 / 搜索 / 鼠标选词
   / 滚动条 / 选择器 / 设置弹窗 / 图片（kitty & iTerm2）/ 扩展 UI 都有，测试面也很宽。
2. 真正的差距在「交互编排」：43 个 `app.*` 键位只消费了 3 个（`app.interrupt`、`app.clear`、
   `app.exit`），上游 23 个内置斜杠命令 Rust 只实现了 11 个（本轮补到 12）。
3. 影响最大的一条是**工具输出没有折叠/展开**：交互 TUI 把所有工具结果拍成一行
   `[tool:name] args → result`，一个 `read` 或 `bash` 就能刷满整个会话。
4. 仓库里已经有 2,269 行上游风格的工具渲染器（`crates/pi-coding-agent/src/tools/render.rs`，
   带折叠、高亮、`render_lines_plain`/`render_lines_ansi`），但只被 print 模式和 HTML 导出使用，
   交互 TUI 完全没接上 —— 这是「已有零件没装机」，不是「要从零造」。
5. 同时发现一个静默的确定性缺陷：`Models` 内部是 `HashMap`，`/model` 列表顺序、`default_model`
   的选择结果每次都不同。本轮已修（见第四节）。
6. 第二轮审计（LUM-1215）把 P1-3 从「缺功能」升级为**确定性缺陷**：流式期间在编辑器里敲
   Enter，文本会被静默丢弃 —— 编辑器先清空，`App::submit` 再因忙直接 `return`
   （`crates/pi-tui/src/app.rs:1932-1937` → `:1307-1310`），全程没有队列、没有提示、没有日志。
   这是输入丢失，严重度高于 P0-1 的「刷屏」；修复面与 P0-1 的富渲染器接线互不重叠，因此单独
   停放为 Stage 61（见第五节）。

> **落地（LUM-1220）**：Stage 61 = LUM-1216 已修并合入 `feature/pi.rs`（`c234068d1`）。忙时
> Enter 与 alt+enter 都入队（steering / follow-up 两段），alt+up 把排队消息取回编辑器，待发消息
> 以 dim `Steering:` / `Follow-up:` 行渲染在转录尾部，turn 结束后按序逐条投递。已知差异：`Agent`
> 在整段 `prompt()` 期间被 `AsyncMutex` 独占，队列只能在下个 turn 边界投递（steer 优先于
> followUp），**不是**中途插入当前 turn；真正的 mid-turn steer 需要 core 暴露共享队列并补发
> `UserMessage` 事件，留作后续切片。

> **第三轮审计（LUM-1221）**：P0-1 与 P1-3 均已落地（Stage 58 / Stage 61），本轮把注意力从
> 「会话内容的呈现」转到「用户到 agent 的输入通道」与「状态反馈」，结论是**入口缺失**还在：
> `!`/`!!` 本地 bash 通道零实现（P1-4）、`app.clipboard.pasteImage`（alt+v）零消费者、
> `TurnUsage` 不进界面。新增可执行切片见「三点五、第三轮补充」与第五节 62/63/64。

> **行号校正（LUM-1219，tip `570f6158d`）**：LUM-1215 写下的 `app.rs:1849-1852` / `:1236-1239` /
> `:1772` / `:1778` 是 `3e7566bf2` 之前的旧坐标，被 LUM-1213（thinking 渲染）的合并往下推了约 83 行。
> 现在请一律以 P1-3 表格里校正后的坐标为准；**代码结构未变**（同一处 `prompt.clear()` + 同一处忙时
> `return`），只是行号漂移。

## 一、基线

| 项 | 数值 | 证据 |
| --- | --- | --- |
| `pi-tui` 源码规模 | 27,895 行 / 30 模块 | `crates/pi-tui/src/` |
| 最大模块 | `app.rs` 4,195、`highlight.rs` 2,574、`editor.rs` 2,280、`markdown.rs` 1,950、`latex.rs` 1,856、`theme.rs` 1,801 | `wc -l crates/pi-tui/src/*.rs` |
| `app.*` 键位定义 | 43 个（Stage 66 起 **44**，含 Rust 专有 `app.header`） | `crates/pi-coding-agent/src/keybindings.rs:201` `app_default_keybindings` |
| `app.*` 消费者（改前） | 3 个 | `crates/pi-tui/src/app.rs:1772` `app.interrupt`、`:1777` `app.clear`、`crates/pi-tui/src/editor.rs:1097` `app.exit` |
| 上游内置斜杠命令 | 23 个 | `packages/coding-agent/src/core/slash-commands.ts` |
| Rust 已实现（改前） | 11 个 | `crates/pi-coding-agent/src/commands/slash.rs:60` 解析分支 |
| 富工具渲染器 | 2,269 行，富渲染只在 print / export 用 | `crates/pi-coding-agent/src/tools/render.rs` |
| 交互工具渲染（改前） | `format_tool` 单行全量输出 | `crates/pi-tui/src/message.rs:715` |

## 二、pi-tui 已经做好的（不要重复投资）

- **编辑器**：kill ring / yank pop / undo 栈 / word 导航 / 跳转（`tui.editor.jumpForward|Backward`）
  / 历史记录 / 自动补全（路径 + 命令，`autocomplete.rs` 1,172 行）。
- **会话视图**：markdown（含 LaTeX）、代码高亮、超链接（OSC 8）、主题切换、浅色/深色、
  鼠标滚轮与拖拽选词、copy-on-select、滚动条几何、按页/首尾/按 prompt 跳转、搜索
  （`search.rs` 1,100 行，含 next/previous/close）。
- **模态**：选择器（可搜索、可窗口化、模糊匹配）、设置弹窗、对话框、扩展 UI（`ctx.ui.*`，
  含 header/footer/region 注入与 `poll_ui_dialogs`）。
- **终端能力**：alt-screen、kitty/iTerm2 图片、剪贴板 OSC 52、窄屏布局。

结论：接下来的 TUI 工作应以「接线 + 编排 + 信息密度」为主，而不是继续堆渲染特性。

## 三、问题清单（按用户体验影响排序）

### P0-1 工具输出不折叠、不可展开

> **已修（Stage 58 = LUM-1214，`6d4f64e62`，LUM-1221 轮合入 `feature/pi.rs` 的 `76d1d634e`）**。
> 交互路径接上了已有的富渲染器：`pi-tui` 定义 `ToolBlock` / `ToolBlockRenderer` 接口与
> `MessageView::tools_expanded` / `tool_preview_lines`（默认 `TOOL_PREVIEW_LINES = 4`），
> 折叠时只画尾部 N 行 + `… (+M lines, Ctrl+O to expand)`；`app.tools.expand`（ctrl+o，可被
> `keybindings.json` 覆盖）一键全展开并给状态栏 flash；鼠标单击只切该块
> （同格释放才生效，拖拽选词与滚轮语义不变）；`pi-coding-agent` 侧 `InteractiveToolRenderer`
> 以 `expanded=true` 运行渲染器，避免其自身常量先裁行。下面的原始分析保留作为对照。

- 现象：一次 `read`/`bash` 的完整结果被塞进一行，既没有截断预览，也没有展开入口。
- 证据：`crates/pi-tui/src/message.rs:715` `format_tool` 拼 `[tool:name] args → result`；
  `crates/pi-tui/src/app.rs:1175-1205` 的 `ToolExecutionStart/End` 只调
  `start_tool_execution` / `finish_tool_execution`（`duration_ms` 拿到手也没进渲染）。
- 上游：`app.tools.expand`（ctrl+o）切换工具输出展开，启动头里也用同一键位提示
  （`interactive-mode.ts:4207-4227` `toggleToolOutputExpansion` / `setToolsExpanded`）。
- Martty：工具块默认只显示固定 4 行尾巴（`src/transcript.rs:1698`），点击块本身切换展开/折叠
  （`src/app.rs:2327`、`src/app.rs:2532`），滚轮仍然滚会话（`src/app.rs:4516`）。
- 修复方向：`MessageItem` 增加 `full_text`/折叠行数，`MessageView` 增加 `tools_expanded` 开关，
  渲染时截断为 N 行 + `… (+M lines, Ctrl+O to expand)`；`app.tools.expand` 切开关；鼠标点击
  工具块复用拖拽选词的命中测试（`mouse_region.rs`）。
- 风险：折叠会改变渲染行数，选词/搜索/快照类测试的坐标系会变 —— 必须给默认折叠行数
  留下可注入参数，并一次性更新 `tests/snapshot.rs`、`tests/mouse_selection.rs`。
- 建议：Stage 58（最高优先，唯一需要跨 crate 的改动）。

### P0-2 43 个 `app.*` 键位只接了 3 个

- 证据：定义在 `crates/pi-coding-agent/src/keybindings.rs:201`（43 个），消费者只有
  `app.rs:1772`、`app.rs:1777`、`editor.rs:1097`。用户按 ctrl+o / ctrl+t / ctrl+g / alt+enter
  完全没有反应，而 `/hotkeys`（本轮新增）会如实显示这些键位 —— 所以本轮的 `/hotkeys`
  只列出**已实现**的动作（见 `slash.rs:152` 的分组表）。
- 上游：43 个动作全部通过 `defaultEditor.onAction(...)` 注册（`interactive-mode.ts:2883-2903`）。
- 修复方向：在 `crates/pi-coding-agent/src/interactive.rs:395-430` 的拦截块里逐个补
  （模式已就位：`matches_with_fallback` + 覆盖层守卫 + 处理完 `return Ok(None)`）。
  待接：`app.thinking.toggle`/`cycle`、`app.editor.external`、`app.message.followUp`/`dequeue`、
  `app.session.new`/`tree`/`fork`/`resume`、`app.clipboard.pasteImage`、`app.suspend`。
- 风险：低（每个动作独立，可增量提交）。注意 `app.suspend` 要把终端交还父 shell，属于
  `run_interactive` 级别，需要先 teardown 再 restore。
- 建议：Stage 59（拆成 2 批：thinking/external-editor/session 一批，queue/steer 一批）。

### P0-3 斜杠命令缺口（23 → 12）

未实现：`/tree`、`/thinking`、`/scoped-models`、`/import`、`/share`、`/copy`、`/name`、
`/changelog`、`/fork`、`/clone`、`/login`、`/logout`、`/new`、`/reload`。

- 影响：会话管理（`/new`、`/tree`、`/fork`）和登录（`/login`）是日常入口；`/new` 缺失时
  用户只能重启进程。
- 修复方向：按「先会话再账户」排序 —— `/new`（清空 + 落新 session 文件）→ `/tree`、`/fork`
  （需要 session 分支树，依赖 `pi-session` + Stage 55 的写路径）→ `/copy`（本轮 `app.message.copy`
  已有底层通道，只差命令入口）→ `/login`/`/logout`（凭据读写需单独设计，涉及密钥，最后做）。
- 风险：`/tree`、`/fork` 与 LUM-1209（Stage 55 `pi-session` 写路径对齐 v4）强耦合，必须等
  写路径落地后再做，否则会造第二个「假兼容 fixture」。
- 建议：Stage 60，且 `/tree`、`/fork` 排在 LUM-1209 之后。

### P1-1 模型目录顺序不确定（本轮已修）

- 现象：`Models::iter()` 是 `HashMap` 迭代，`/model` 列表顺序随机；`default_model` 直接取
  `.next()`，未指定 `--model` 时启动模型随机。
- 证据：`crates/pi-ai/src/models.rs:19` `by_provider: HashMap`；改前
  `crates/pi-coding-agent/src/interactive.rs:1308` `models.iter().next()`。
- 修复：`sorted_models()`（`interactive.rs:486`）统一按 `(provider, id)` 排序，`/model`
  选择器、Ctrl+P 循环、`default_model` 共用。

### P1-2 thinking 块不可见性控制缺失

- 上游 `app.thinking.toggle`（ctrl+t）隐藏/显示思考块、`app.thinking.cycle`（shift+tab）切等级。
- Rust 的 markdown 渲染会把思考文本当普通正文；没有「是否显示」开关，也没有等级概念
  （`pi_protocol::Model` 目前没有 `reasoning` 字段，见 `crates/pi-protocol/src/model.rs:52`）。
- 影响：长思考会淹没答案；高噪音。
- 修复方向：先做「显示/隐藏」这一层（纯 UI 状态），等级要等协议模型字段补齐。

### P1-3 流式期间的输入被静默丢弃（follow-up 队列 / steer / dequeue 全缺）

首轮把这条写成「交互节奏断裂」偏轻了。第二轮审计（LUM-1215）给出的证据链说明它是**输入丢失**：

| 环节 | 位置 | 行为 |
| --- | --- | --- |
| 编辑器提交 | `crates/pi-tui/src/app.rs:1932-1937`（校正后） | `PromptAction::Submit` → **先 `prompt.clear()`**，再抛 `StepOutcome::Submitted` |
| 驱动转发 | `crates/pi-coding-agent/src/interactive.rs:461`（校正后） | 无条件 `app.submit(agent.clone(), text)` |
| 忙时丢弃 | `crates/pi-tui/src/app.rs:1307-1310`（校正后） | `if self.turn_busy.load(..) { return; }` —— 文本到此为止 |

也就是说：一个长 turn 里敲进去的每一句话，按 Enter 就永久消失，没有任何反馈。`turn_busy` 只被
`app.interrupt`（`app.rs:1847`）、`app.clear`（`app.rs:1852`）、`/compact`（`interactive.rs:950`）读取，
编辑器与提交路径都不看它，所以这不是「禁止输入」，而是「接受后扔掉」。

对照：同是忙时拒绝，`/compact` 会明确回一句 `a turn is in flight — abort or wait for it to finish`
（`interactive.rs:949-952`），所以「静默」只发生在普通 prompt 这条路上，不是全局行为。

同时 `app.message.followUp`、`app.message.dequeue`、`app.clipboard.pasteImage`、`app.session.new`
四个上游键位 id 在 Rust 全仓**没有任何消费者**（LUM-1219 在 tip `570f6158d` 上复核，
`grep -rn … --include=*.rs | grep -v keybindings.rs` 零命中；只在 `keybindings.rs` 里定义，
另有 `crates/pi-tui/src/app.rs:105-114` 的文档注释提及 —— 该注释本身也已过期：它把
`app.model.cycleForward` / `app.thinking.toggle` 仍列为「no consumer」，而这两个 LUM-1210 / LUM-1213
已接线，待 Stage 58/61 收工后一并订正，避免与在飞的 `app.rs` 改动面冲突）。

上游语义（`packages/coding-agent/src/modes/interactive/interactive-mode.ts`）：

| 输入 | 上游行为 | 位置 |
| --- | --- | --- |
| 流式中 Enter | `session.prompt(text, { streamingBehavior: "steer" })`，进当前 turn | `:3136-3143` |
| 流式中 alt+enter | `{ streamingBehavior: "followUp" }`，排队等 turn 结束 | `:4146-4149` |
| 空闲时 alt+enter | 等同 Enter（`handleFollowUp` 用 `onSubmit` 兜底） | `:4151-4155` |
| `app.message.dequeue` | 取回 steering + followUp 全部消息；状态栏 `No queued messages to restore` / `Restored N queued message(s) to editor` | `:4157-4163`、`:4336-4372` |
| 排队消息可见 | `pendingMessagesContainer` + `hasPendingMessages: () => session.pendingMessageCount > 0` | `:385`、`:550`、`:2052` |

更正首轮的一处说法：**ctrl+x 不是 steer**。上游 `app.message` 只注册 copy / followUp / dequeue
（`:2895-2899`），steer 是「流式期间的 Enter」，没有独立键位 id；ctrl+x（`app.message.copy`）不冲突。

底座并非空白：核心侧队列已存在 —— `crates/pi-agent-core/src/queue.rs:18`
（`MessageQueue::push/is_empty/drain`）、`crates/pi-agent-core/src/agent_loop.rs:221-227`
（`push_follow_up` / `follow_up_len`），agent loop 会在工具迭代之间消费它（`agent_loop.rs:305`、
`:463-478`）。缺的是三件事：

1. `MessageQueue` 补 `len()` / `take_all()`（dequeue 取回需要）与 steering / followUp 两段式
   （上游 `clearQueue()` 返回 `{ steering, followUp }`，`:4353-4364`）；
2. `App` 持有一份 pending 列表：忙时 `submit` 入队而不是 `return`，并让 `MessageView` 渲染「待发送」
   样式（`MessageItem` 目前只有 `role/text/streaming`，`message.rs:34-43`）；
3. 驱动接 `app.message.followUp` / `app.message.dequeue`，并让流式 Enter 走 steer。

影响：任何「边看输出边补话」的场景（长 turn、多轮迭代）都在丢输入。
修复方向：按上面 1/2/3 三刀切，建议先落 2+3（`App` 内队列 + 键位），1 视是否需要跨 turn 持久化再定。
已停放为 **Stage 61 = LUM-1216**（见第五节），建议优先级高于 Stage 58。

### P1-4 没有 `!cmd` 本地 shell 通道（LUM-1221 复核后升级为「入口缺失」）

- 上游 `!` 直接跑本地命令、`!!` 跑但不进上下文；这是「不问模型先看一眼」的常用动作。
  上游证据（LUM-1221 复核）：`isBashMode` 由编辑器 `onChange` 从 `text.trimStart().startsWith("!")` 推出并发起
  编辑器边框换色（`packages/coding-agent/src/modes/interactive/interactive-mode.ts:2907-2912`），提交分支在
   `:3106-3123` 拆分 `!!`（`isExcluded`）与 `!`，忙时给 `A bash command is already running. Press Esc to cancel it first.`
  而不是静默排队；启动头用 `rawKeyHint("!", …)` 提示（`:932`、`:943`）。
- Rust 侧零实现：`pi-rust/crates/pi-coding-agent/src/` 全树无 `isBashMode` / `startsWith("!")` 等价物。
- 修复方向：编辑器前缀识别（已有 `autocomplete.rs` 的前缀逻辑可参考）+ `tool_executor` 的
  修复方向：编辑器前缀识别（已有 `autocomplete.rs` 的前缀逻辑可参考）+ `tool_executor` 的
  bash 通路。与 Stage 61 已落地的 pending 队列有一处交集：bash 忙时应「拒绝并提示」而不是入队（上游语义）。
- **已落地（Stage 62 = LUM-1223，`b2f673922`）**：前缀解析放在 `editor.rs`（`parse_bash_command` /
  `is_bash_mode`），提交分支在 `interactive.rs::handle_submitted`；命令结果复用 Stage 58 的
  `InteractiveToolRenderer` + `MessageView::finish_tool_execution_with_lines`，折叠 / `Ctrl+O` /
  点击展开全部生效；忙时按上游语义**拒绝并把文本放回编辑器**（不接 Stage 61 的 pending 队列）；
  `Esc` 复用 `CancellationToken`/`AbortLike` 通道杀掉子进程。**已知差异**：`pi-protocol::Role`
  没有 `bashExecution` 变体，所以 `!` 目前也不写入 `Agent::state().messages`（上游会写入并在
  构造上下文时过滤 `!!`）—— 待协议层补 role 后即可对齐。编辑器边框色上游在 `bashMode` 时切换，
  本移植的 prompt 没有边框，改为把 `> ` 标签染成 `ThemeColor::BashMode`。

### P2 其它

- 多图粘贴 chip（≤8）：`image.rs`/`terminal_image.rs` 已有渲染，缺 composer 侧的 chip 列表与
  退格删除语义。
- token / cache 指标：`TurnUsage` 已在 `app.rs`（`take_turn_usage`），但没有落到消息脚注。
- 会话级 keybindings 热重载：`reload_keybindings` 已实现（`keybindings.rs:789`），但渲染循环
  没有触发点（`interactive.rs:222` 的 `_keybindings` 注释已写明这点）。
- 启动头可展开（上游 `ExpandableText`）：与 P0-1 用同一套折叠机制，建议合并实现。

## 三点五、第三轮补充（LUM-1221，对照 Martty 的「输入通道 / 信息密度」）

第二轮把注意力放在「会话内容的呈现」上；第三轮改看「用户到 agent 的输入通道」与「状态反馈」，
因为 Stage 61 / 60 / 58 已经把队列、会话命令与折叠补齐，剩下的缺口集中在**入口**和**反馈**两类：

| 问题 | pi-rust 现状 | 上游 / Martty 对位 | 建议 stage |
| --- | --- | --- | --- |
| `!cmd` / `!!cmd` 本地 shell | 零实现（见 P1-4） | 上游 `interactive-mode.ts:2907-2912`、`:3106-3123`；Martty 把客户端命令留在本地、不进 agent 上下文（`src/app.rs` 测试 `client_plugin_command_invocation_stays_out_of_the_agent_prompt`） | 62 |
| `app.clipboard.pasteImage`（alt+v） | 键位已定义（`keybindings.rs:165`、`:273`），**零消费者**（`grep -rn pasteImage pi-rust/crates --include=*.rs` 只剩 `tests/keybindings.rs`） | 上游 `onPasteImage`：按路径挂图片，无图片时退化为纯文本粘贴（`interactive-mode.ts:2913-2915`）；Martty 有 composer 图片 chip 全语义：`chip_at` / `delete_token_at`（退格吃掉整个 token 而不是一个字符）/ `draft_split_keeps_text_and_images_interleaved` | 63 |
| token / cache 指标不进界面 | `TurnUsage` 只在 `maybe_auto_compact` 里被读一次（`crates/pi-coding-agent/src/interactive.rs:1438`），界面无脚注 | Martty 有 footer usage 快照（`acp_resume_usage_snapshot_reaches_the_footer_once`） | 64（LUM-1225 已落地，见第七节） |
| 流式等待没有动效 | `pi-tui` 全树无 spinner（`grep -rn spinner` 零命中），忙时只有状态行文本 | Martty 有 subagent/turn spinner（`a_running_subagent_keeps_the_spinner_advancing`） | 低优先，随 62 一起评估 → **已落地，Stage 66 = LUM-1228**（见第十节） |

结论：TUI 的下一批工作按「先入口、后密度」排序，即 **62（`!cmd`）→ 63（图片 chip + `pasteImage`）→ 64（指标脚注）**。
Stage 58/61/60 均已合入，62/63 的文件面（`editor.rs` + 提交分支 + `app.rs`）与已合入的折叠面**只共享
`interactive.rs` 的拦截块**，可顺序落地。

## 三点六、第四轮验证与修复（LUM-1222，与 LUM-1221 并发的重复协调轮）

LUM-1221 与本轮是同一 autopilot 提示的两条并发轮。本轮开工时 `origin/feature/pi.rs` 已经是
`d22c5d694`（LUM-1220 的 61+60），于是本轮独立地把 Stage 58（`origin/work/LUM-1214` = `6d4f64e62`）
合进同一基线，**四处冲突独立人工解**（`interactive.rs` 的 `set_session_name` + `set_tool_block_renderer`
并留、`app.rs`/`lib.rs` 的 `use` 合并、`message.rs` 手写 `Default` 补回 `pending_steering`/`pending_follow_up`）。

结果：本轮合并树的 tree hash 与 LUM-1221 的合并提交 `76d1d634e` **逐字相同**（`a42b444d06db…`），
两条独立合并线互为交叉验证，故本轮**弃用自己的重复合并提交、以远端为基**（同 LUM-1219 的处置口径）。
本轮在自己的树上实跑全量门得到 `145 / 2159 / 0 / 2`，与 LUM-1221 自报值逐字一致。

本轮产出（两处对用户可见的修复 + 一次验收复核）：

1. **修 `feature/pi.rs` 当前是红门**：LUM-1221 的文档提交 `b20800ed3` 在
   `docs/TUI_UX_AUDIT.md` 里插第三轮验证块时，**吃掉了首轮块的起始 fence**，导致该文件
   fence 不配对；`pi-evals` 的 `docs-code-fences-balanced`（审计 `pi-rust/docs/**` + 两个 README）
   因此在 `feature/pi.rs` 上**实跑失败**（`cargo test --workspace` 退出码 101）。本轮补回
   ` ```console ` 起始 fence，全仓重新扫描已无未闭合块。教训：**文档提交也要过一遍全量门** ——
   LUM-1221 的门只跑在合并提交 `76d1d634e` 上，文档提交在其之后。
2. **`/hotkeys` 补 `app.tools.expand`（Ctrl+O）**：Stage 58 已让该动作有消费者，但
   `slash.rs` 的 app 分组表没加这一行 —— 违反本文件自己定的规则「只列出真正实现的动作」
   （未实现的不显示，实现的不漏）。用户在 `/hotkeys` 里看不到刚做好的折叠入口，这条正是
   信息密度类缺陷。新增单测 `hotkeys_text_lists_the_tool_fold_chord`。
3. **Stage 58 验收复核**（对照 LUM-1214 的 5 条验收标准逐条查）：

| 标准 | 结论 | 证据 |
| --- | --- | --- |
| 1 默认折叠 + N 可注入 | 通过 | `message.rs:48` `TOOL_PREVIEW_LINES = 4`；`with_/set_tool_preview_lines`（`:444`/`:455`）；`tool_body_lines` 取**尾部** N 行 + `tool_fold_hint`（`:1167-1175`）；短块不折叠（`:1714` 测试） |
| 2 `app.tools.expand` 可覆盖 + ctrl+o 回落 | 通过 | `app.rs:2051` `matches_app_key(..., "app.tools.expand", &["ctrl+o"])` → `toggle_tools_expanded` + 状态栏 flash；`tests/keybinding_consumer.rs:46` 用 `ctrl+g` 覆盖后原键失能、新键生效 |
| 3 单击只切该块 / 滚轮语义不变 | 通过 | `app.rs:2932` 单击命中 `tool_block_at` 记 `tool_press`，同格释放才提交；`tests/mouse_scroll.rs:289-295` 断言滚轮不改 `tools_expanded` |
| 4 `render.rs` 接入 ≥ bash/read/edit | 通过 | ``renders.rs:1576`` `InteractiveToolRenderer`（`impl pi_tui::ToolBlockRenderer`）在 `interactive.rs:276` 安装；`renderer_for` 覆盖 read/write/edit/bash/find/grep/ls；`tests/tool_blocks.rs` 10 个用例 |
| 5 全量门绿 | **修复后才绿** | 见第 1 条：tip 上 `pi-evals` 失败；本轮修 fence 后 `cargo test --workspace` 全绿 |

两条**未实现的遗留**（均不属 LUM-1214 的编号验收标准，记录备查）：

- 折叠提示硬编码 `Ctrl+O`（`message.rs:54` `tool_fold_hint`），用户用 `keybindings.json` 把
  `app.tools.expand` 改到 `ctrl+g` 后，界面仍提示 Ctrl+O；上游的提示取自**生效 chord**。
  修法需把 chord 字符串从 driver 注入 `MessageView`（不能反向依赖），建议并入 Stage 63 或其后。
- 启动头（上游 `ExpandableText`，`interactive-mode.ts:4207-4227`）的展开与工具块共用同一个
  `app.tools.expand` 开关，但 Rust 端**没有启动头**（搜 `ExpandableText`/`app.header` 零命中），
  故「两套折叠状态」的风险实际不成立，只是开关的覆盖面比上游窄。
  → **已落地，Stage 66 = LUM-1228**：Rust 端补了内置启动头，并**主动把两套折叠拆开**
  （`Ctrl+O` 只折叠工具输出，启动头归新注册的 `app.header`，默认 `alt+h`），
  因此「开关覆盖面比上游窄」换成了「比上游多一个可覆盖、可被 `/hotkeys` 列出的 id」。

## 四、本轮已交付

| 改动 | 位置 | 说明 |
| --- | --- | --- |
| Ctrl+P / Shift+Ctrl+P 循环模型 | `crates/pi-coding-agent/src/interactive.rs:406`、`:498` | 首个真正落地的 `app.*` 动作；越界（当前模型不在目录里）从头开始；单模型给出提示而不是空转 |
| Ctrl+X 复制最后一条助手消息 | `crates/pi-coding-agent/src/interactive.rs:424`、`:537` | 复用 App 已有的 clipboard 通道；新增 `App::request_clipboard`（`crates/pi-tui/src/app.rs:3176`） |
| `/hotkeys` | `crates/pi-coding-agent/src/commands/slash.rs:146`、`interactive.rs:630` | 从**生效**键位表（含 `keybindings.json` 覆盖）渲染，按 navigation / editing / chat log / app / selectors 分组；只列出真正实现的 `app.*` 动作 |
| 模型目录排序 | `crates/pi-coding-agent/src/interactive.rs:486`、`:1308` | `/model` 列表、循环顺序、默认模型统一确定化 |

拦截位置统一在 `handle_input_event`（`interactive.rs:395`）：选择器 / 对话框 / 设置 / 自定义
覆盖层 / 搜索任一打开时不抢键，插在 `app.step` 之前 —— 对应上游把动作注册在 editor 上的行为。

## 五、后续 stage 建议

| Stage | 内容 | 验收 | 依赖 / 风险 |
| --- | --- | --- | --- |
| 58（LUM-1214，已合入） | 工具输出折叠 + `app.tools.expand` + 点击工具块展开 | 已交付 `6d4f64e62`，LUM-1221 轮合入（`76d1d634e`）；折叠提示 + N 可注入 + 键位可覆盖 + 单击单块，测试 `tests/tool_blocks.rs` 与 `tools_render.rs` 快照 | 无（选词/搜索快照未受影响，因折叠只在装了渲染器的交互路径生效） |
| 59（部分完成） | 补齐 `app.*` 动作第 1 批 | 已完成：`app.thinking.toggle`（LUM-1213）、`app.message.copy`（LUM-1210）、`app.model.cycle*`（LUM-1210）、`app.message.followUp`/`dequeue`（Stage 61）、`app.session.new`（Stage 60）、`app.tools.expand`（Stage 58）。剩余：`app.editor.external`、`app.session.tree`/`fork`/`resume` | `/tree`、`/fork` 的读路径已由 Stage 56（LUM-1212）备齐；外部编辑器需要 teardown/restore 终端 |
| 60（LUM-1218，已合入） | 会话命令补齐：`/new`、`/copy`、`/name` | 已交付 `86f46dea0`，LUM-1220 轮合入 `feature/pi.rs`（3 个命令 + `app.session.new` 键位 + 6 个新测试） | `/tree`、`/fork` 仍待接线 |
| 61（LUM-1216，已合入） | 流式期间输入不丢：`App` 内 pending 队列 + steer（Enter）/ followUp（alt+enter）/ dequeue（alt+up）+ 排队消息渲染 | 已交付 `8cab3a136`，LUM-1220 轮合入 `feature/pi.rs` | 无；mid-turn steer 需 core 暴露共享队列，留作后续切片 |
| 62（LUM-1223，已交付） | `!cmd` / `!!cmd` 本地 bash 通道 + 忙时拒绝语义 | 已交付 `b2f673922`（LUM-1225 轮合入）：前缀识别 + 执行 + 结果折叠块 + `Esc` 取消；`!!` 因协议层缺 `bashExecution` role 暂以「不入 log」实现；忙时拒回编辑器而不入 Stage 61 队列 | 提交分支与 Stage 61 的队列相邻，需先判 bash 再判队列（已按此顺序实现） |
| 63（LUM-1224，停放） | `app.clipboard.pasteImage` + composer 图片 chip（≤8，退格整块删） | alt+v 挂图 / 无图退化纯文本；chip 可整块删除 | `image.rs` / `terminal_image.rs` 渲染已就绪，只缺 composer 侧；62 已落地，可开工 |
| 65（LUM-1226，已派发 `todo`） | 会话树导航：`/tree`、`/fork`、`/clone` + `app.session.tree`/`fork`/`resume` 接线 | 树覆盖层由 `DecodedEntry.entry_id`/`parent_entry_id` 拼；`/fork` 选 user message 建新会话；`/clone` 原位复制；源会话零改写（`verify_stats` 断言） | 读路径已由 Stage 56 备齐（`branch_meta`/`branch_entries`/`scan_branch`）；写路径需新增「拷前 N 条到新会话」（上游 `SessionManager.forkFrom`，`session-manager.ts:1611`）；与 Stage 62 共用 `interactive.rs` 的命令分支，按现有顺序追加 |
| 66（LUM-1226，停放 `backlog`） | 流式反馈与可发现性：spinner + 轮耗时 + 启动头 key hints + `app.header` | `spinner` 全仓命中从 0 到有；耗时与 Stage 64 同 footer 行；启动头可折叠且不占行 | 对照 Martty `src/app.rs` 的 `SPINNER`/`spinner_idx`/`spinner()` 与测试 `a_running_subagent_keeps_the_spinner_advancing`；文案对照 Martty `src/locale.rs`，用常量表不引 i18n 框架 |
| 67（LUM-1230，停放 `backlog`） | 思考级别：`/thinking [level]` + `app.thinking.cycle`（`shift+tab`）/ `app.thinking.save`（`ctrl+s`）+ 编辑器边框随级别着色 +「当前模型不支持思考」提示 | 4 个入口全部接线；`/thinking` 与键位走同一段切换代码；边框色用 `Theme::thinking_border`；不支持时给状态行而非静默 | 上游 `interactive-mode.ts:2884`（cycle）、`:2986`（`/thinking` 选择器）、`:2139`（边框色）、`:4170`（不支持提示）；`ThinkingLevel`（`pi-agent-core/src/hooks.rs:87`）与 `Theme::thinking_border`（`pi-tui/src/theme.rs:1108`）已在位，缺的是 App/session 之间的级别贯通；`slash.rs` / `interactive.rs` 与 Stage 65 同文件，须排在 65 之后 |
| 68（LUM-1231，停放 `backlog`） | 斜杠命令第二批：`/reload`、`/changelog`、`/import`、`/login`、`/logout`、`/scoped-models` | 解析分支 + 实际行为 + `/help` 文案；`/reload` 重载 keybindings/extensions/skills/prompts/themes/context 至少覆盖已实现的子集 | 上游 `slash-commands.ts:24-42`；auth 子系统（LUM-1171 / LUM-1180）与 session 导入导出（LUM-1174）已在位；`/share` 需 GitHub gist + 凭据，单独停放；与 Stage 67 同文件，串联在 67 之后 |
| 69（LUM-1232，停放 `backlog`） | Martty 风格会话级持久 shell：`!` 命令之间保留 `cd` / 环境变量，退出 TUI 时回收 | 连续 `!cd sub` + `!pwd` 看到目录延续；一个 `!export X=1` 在下一个 `!` 可见；`Esc` 仍可中断；进程随 TUI 退出而终止；`!!` 语义不变 | **非上游对齐**（上游 `bash-executor.ts:50` 每条命令新起进程），属「最佳体验」增强，需先决定默认开/关；实现对标 Martty `src/app.rs:718-835`（`PersistentShell` + 控制 fd 9 + `shell_quote`）；与 Stage 62 的 `BashRunner` 同文件 |

并发约束：LUM-1219 轮是 3 worker 在飞的重复轮（零派发）；LUM-1221 开工时在飞 2 个
（LUM-1214 Stage 58、LUM-1220 协调轮），LUM-1214 与 LUM-1220 均在本轮内收工（且 LUM-1214 的
产物未提交、由本轮抢救），故本轮把 58 合入后按「非冲突面优先」派发 Stage 56 = LUM-1212
（`backlog` → `todo`，解 `/tree`/`/fork` 的读路径阻塞），62/63 待其占用槽位释放后晋升。

> **落地状态（LUM-1225 更新）**：Stage 58（`6d4f64e62`）、Stage 60（`b91a3ae12`）、Stage 61
> （`c234068d1`）早已并入 `feature/pi.rs`；LUM-1222 修复了 `b20800ed3` 引入的未闭合 fence、使
> `pi-evals` 回绿（tip `3fefd7bd8`）。本轮（LUM-1225）在 `3fefd7bd8` 之上交付 Stage 64
> （usage 脚注，`f84e262b5`），并合入 Stage 56（LUM-1212，`c9122e3d8`，pi-session 的
> usage/stats/branch 读路径）与 Stage 62（LUM-1223，`b2f673922`，`!cmd`/`!!cmd`）。
> Stage 63（LUM-1224）先前受「必须排在本切片之后」约束，现已解除，可开工。

## 六、验证

第三轮（LUM-1221，合入 Stage 58 + Stage 61 + Stage 60 后的 `feature/pi.rs` 全量实跑；
同轮由 LUM-1222 独立复现（树 hash 相同），见「三点六」）：

```
$ CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 \
  cargo test --workspace --offline
  145 套件 / 2159 passed / 0 failed / 2 ignored    （上一基线 144 / 2149 / 0 / 2）

$ … cargo clippy --workspace --all-targets --offline -- -D warnings
  Finished `dev` profile … （仅依赖 crate `rquickjs-core` 有 12 条 warning，本仓零 warning）

$ cargo fmt --all -- --check
  干净
```

首轮（LUM-1210）：

```console
$ CARGO_HOME=/tmp/cargo-home CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0 \
  cargo test -p pi-coding-agent -p pi-tui --offline
  59 个 test target：1291 passed / 0 failed / 0 ignored

$ CARGO_HOME=/tmp/cargo-home CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0 \
  cargo clippy -p pi-coding-agent -p pi-tui --all-targets --offline -- -D warnings
  完成，0 warning

$ cargo fmt -p pi-coding-agent -p pi-tui -- --check
  干净
```

Stage 62 轮（LUM-1223，已合入本轮 `feature/pi.rs`）：

```console
$ cargo test -p pi-coding-agent -p pi-tui --offline
  63 个 test target：1343 passed / 0 failed / 0 ignored

$ cargo clippy -p pi-coding-agent -p pi-tui --all-targets --offline -- -D warnings
  exit 0

$ cargo fmt -p pi-coding-agent -p pi-tui -- --check
  干净
```

新增测试：`commands::slash::tests::hotkeys_text_lists_effective_chords`、
`hotkeys_text_skips_unbound_rows`、`help_text_lists_hotkeys_command`、
`interactive::tests::cycling_models_wraps_around_the_sorted_catalog`、
`cycling_from_a_model_outside_the_catalog_starts_at_the_top`、
`cycling_a_single_model_reports_it_instead_of_switching`、
`copying_the_last_assistant_message_queues_it_for_the_clipboard`、
`copying_an_empty_transcript_reports_nothing_to_copy`、
`default_model_is_the_first_entry_of_the_sorted_catalog`、
`sorted_models_orders_by_provider_then_id`。

Stage 62（LUM-1223）新增测试：`interactive::tests::bang_echo_renders_a_tool_block_without_a_user_message`、
`double_bang_output_is_visible_but_never_enters_the_agent_log`、
`a_busy_app_refuses_bash_and_restores_the_editor`、
`empty_bang_commands_fall_back_to_the_normal_prompt`、
`esc_cancels_a_running_bash_command`；`tools::bash::tests::abort_kills_a_running_command`；
`pi_tui` 侧 `bash_mode_ignores_leading_whitespace`、`parses_bang_and_double_bang_commands`、
`empty_bash_commands_fall_back_to_the_prompt`、`bash_mode_colours_the_prompt_label`。

已知限制：`/hotkeys` 只列已实现动作（未实现的 `app.*` 不显示，避免「文档骗人」）；
Ctrl+P 循环的是完整模型目录，上游的 `/scoped-models` 作用域还没实现；本轮未重跑全量
workspace（LUM-1209 / LUM-1211 正在各自的 worktree 里编译，避免三份全量构建抢 CPU/磁盘），
但 `pi-tui` + `pi-coding-agent` 的 `--all-targets` 门槛已单独跑过。

## 七、第五轮（LUM-1225）：usage 指标落到 footer（Stage 64）

第三轮审计把「token / cache 指标不进界面」列为低优先的 Stage 64。本轮认定它其实是「信息密度」
里最便宜、也最常被看的一格：上下文占比是用户判断「该不该 `/compact`」的唯一信号，而旧状态栏只
显示 `in X out Y` 两个累计值，既没有 cache 命中，也没有窗口占比。本轮落地如下：

| 改动 | 位置 | 说明 |
| --- | --- | --- |
| 紧凑 token 格式化 `format_tokens` | `crates/pi-tui/src/status.rs` | 对齐上游 `formatTokens`（`footer.ts:23-31`）：`1.5k` / `12k` / `1.5M` / `12M` |
| cache 累计 | `status.rs` 的 `StatusData::{cache_read,cache_write}` + `add_usage` | 从 `MessageEnd.usage` 累计 R/W，仅在 >0 时显示 |
| 上下文仪表 | `status.rs` 的 `context_gauge` | `42.0%/128k`；窗口已知但尚无 turn 时显示 `?/128k`；>70% warning、>90% error（对齐 `footer.ts:151-176`） |
| App 接线 | `crates/pi-tui/src/app.rs` | 构造时用 `agent.model().context_window` 播种窗口；`MessageEnd` 更新累计与最近 turn 的 context；`queue_model_switch` 随模型更新窗口并清空占比 |

安全边界：`context_window == 0`（无模型窗口信息）时整段隐藏，`StatusData::new` 的旧构造
（测试与驱动）行为不变；`add_tokens` 保留。新增 8 个单测、1 个 App 级测试与 1 个主题色测试。

状态：Stage 64（`f84e262b5`）与 Stage 56（LUM-1212，`c9122e3d8`）、Stage 62（LUM-1223，
`b2f673922`）已由本轮一并并入 `feature/pi.rs`。至此审计里的「入口 → 密度」三段中，
56/58/60/61/62/64 已落地；63（图片 chip）的排序约束（「必须排在 62 之后」）已解除，成为下一个槽位；
剩下的 Stage 59 尾部（`app.editor.external`、`app.session.tree`/`fork`）所需的 `branch_*` 读路径
（Stage 56）本轮已备齐，可以接线。

本轮合并树的全量门（在两个 merge commit `65676563d`、`d20361992` 上实跑）：

```console
$ CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 \
  cargo test --workspace --offline
  147 套件 / 2202 passed / 0 failed / 2 ignored    （上一基线 145 / 2159 / 0 / 2）

$ … cargo clippy --workspace --all-targets --offline -- -D warnings
  Finished `dev` profile … （仅依赖 crate `rquickjs-core` 自带 warning，本仓零 warning）

$ cargo fmt --all -- --check
  干净
```

合并细节：Stage 56 整条分支零冲突；Stage 62 仅 `docs/TUI_UX_AUDIT.md` 三处冲突
（P1-4 落地说明、第五节 stage 表、落地状态块），代码文件（`app.rs` / `lib.rs` / `editor.rs`）
均自动合并。附带修回一处文档回归：`origin/work/LUM-1223` 误把「首轮（LUM-1210）」的
测试数从 `59/1291` 改成它自己的 `63/1343`，本轮恢复历史值并为 62 单列验证块。

## 八、第六轮（LUM-1226）：并发轮在合并树上复核 —— 剩余入口只剩两类

本轮与 LUM-1225 是**同 title 的并发 autopilot 轮**。LUM-1225 已把 Stage 64 交付、并把在飞的
Stage 56（LUM-1212）与 Stage 62（LUM-1223）一并并入 `feature/pi.rs`（tip `b6f67d7f6`，全量门
147 / 2202 / 0 / 2）。因此本轮不重复它的合并动作，改为在**合并后的树**上重跑一次
「未消费键位 + 空白能力」普查，把剩余缺口收敛成两个可执行切片。

未消费的 `app.*`（`grep -rn '"<key>"' crates --include=*.rs | grep -v keybindings.rs`）：

| 键位 | 命中 | 归属 |
| --- | --- | --- |
| `app.session.tree` / `app.session.fork` / `app.session.resume` | 0 / 0 / 0 | Stage 65（`/tree`、`/fork`、`/clone` + `/resume` 键位） |
| `app.clipboard.pasteImage` | 0 | Stage 63（LUM-1224，串行约束已解除） |
| `app.header` | 0 | Stage 66（启动头折叠开关） |
| `app.thinking.cycle` | 0 | 未切片（Stage 59 尾部，需 `Model.reasoning` 语义） |
| `app.editor.external` | 0 | 未切片（需 teardown/restore 终端交接，与 Stage 66 同族） |

空白能力（全仓命中数）：

```console
$ grep -rni "spinner" pi-rust/crates --include=*.rs | wc -l
0
$ grep -rni "startup\|banner" pi-rust/crates/pi-tui/src/*.rs
crates/pi-tui/src/lib.rs:117:/// Build info that the binary prints at startup.
crates/pi-tui/src/theme.rs:789:    /// Info banner background for HTML export.
```

即：**等待反馈**（spinner / 轮耗时）与**启动可发现性**（首屏 key hints）两块仍是空白，两者都在
`pi-tui` 内、与其它 stage 无文件耦合，合并为一个 Stage 66。`pi-rust` 至今**没有 locale/i18n 模块**，
Martty 的 `src/locale.rs`（`tr(en, zh)` / `command_desc` / `ambient_tip`）是文案集中化的现成参考，
Stage 66 用常量表即可，不引入 i18n 框架。

`/resume` 其实**已有**选择器（`interactive.rs:1104` 起走 `list_resumable` + `Selector`，
`max_visible = 10` 对齐上游 `session-selector.ts`），但**键位 `app.session.resume` 没有接线**，
用户只能手打命令；Stage 65 要求它与 `/resume` 复用同一段代码，不要复制两份。

树的数据来源**不需要动协议**：`pi_protocol::SessionEntry`（`crates/pi-protocol/src/session.rs:23`）
的变体不带 `id`/`parentId`，但父链在存储层就有 —— `DecodedEntry.entry_id` / `parent_entry_id`
（`crates/pi-session/src/reader.rs:78`）、`EntryRow.parent_entry_id`（`crates/pi-session/src/schema.rs:416`），
对应上游 `entries.parent_id`（`session-manager.ts:49`、`:143`、`:230`）。写侧 `SessionWriter`
（`crates/pi-session/src/writer.rs:175-371`）有 `write_header`/`resume`/`append`/`commit`/`checkpoint`/`rollback`，
但缺「拷前 N 条到新会话」——那正是 `/fork`、`/clone` 要补的函数。

**远程分支清点**：除 56 / 62 外，`origin` 上还有 7 条 `work/*`、`agent/*` 分支不是
`feature/pi.rs` 的祖先（`142cee5d0ed9` pi-ai auth、`e3a55b14fe9d` Gemini provider、`lum-1023`
扩展宿主、`lum-1058` telemetry、`lum-1173` rustfmt、`9f0097e10886`、`lum-1020`），但逐条比对后
**全部是更旧、体量更小的前身版本**（落后 121–478 个提交，同路径文件在 tip 上更完整），无可合并内容；
本轮实测 `git diff --shortstat origin/feature/pi.rs <branch> -- <branch 改过的文件>` 只剩
「tip 更完整」的净增，因此不产生合并动作。

**本轮派发**：Stage 63（`backlog` → `todo`）与 Stage 65（新建 `todo`），Stage 66 停放 `backlog`；
LUM-1225 收工后同时在跑 2 个 stage，符合「最多 3 个并发」。


## 九、第七轮（LUM-1229）：满槽并发轮 —— 对照 Martty 源码复核「输入通道 / 思考级别 / 命令面」

本轮开工时 `multica issue runs <LUM-981> --siblings --active` 有 3 个 stage 在跑（Stage 63 =
LUM-1224、Stage 65 = LUM-1227、Stage 66 = LUM-1228，三者都有活跃 workdir），按「最多 3 个并发」
**零派发**，只做复核与停放；`feature/pi.rs` 的 tip 仍是 `7e340c841`，无待合并分支（`origin` 上
`work/LUM-1224`、`work/LUM-1227`、`work/LUM-1228` 尚不存在）。

### 9.1 在飞产物的文件面（合并冲突预判）

三个 worker 的 `git status --short`：

```console
$ git -C <LUM-1227 workdir> status --short
 M pi-rust/crates/pi-coding-agent/src/commands/mod.rs
 M pi-rust/crates/pi-coding-agent/src/commands/slash.rs
 M pi-rust/crates/pi-coding-agent/src/interactive.rs
 M pi-rust/crates/pi-coding-agent/src/keybindings.rs
 M pi-rust/crates/pi-session/src/lib.rs
 M pi-rust/crates/pi-session/src/writer.rs
 M pi-rust/crates/pi-tui/src/lib.rs
?? pi-rust/crates/pi-coding-agent/src/commands/tree.rs
?? pi-rust/crates/pi-session/src/tree.rs
?? pi-rust/crates/pi-tui/src/tree.rs
$ git -C <LUM-1228 workdir> status --short
 M pi-rust/crates/pi-tui/src/status.rs
?? pi-rust/crates/pi-tui/src/loader.rs
?? pi-rust/crates/pi-tui/src/locale.rs
$ git -C <LUM-1224 workdir> status --short
 M pi-rust/crates/pi-agent-core/src/agent.rs
 M pi-rust/crates/pi-tui/src/editor.rs
 M pi-rust/crates/pi-tui/src/prompt.rs
```

三条线目前**互不重叠**：65 在 `commands/` + `pi-session` + `pi-tui/lib.rs`，66 在
`pi-tui/status.rs` + 两个新模块，63 在 `pi-agent-core` + `editor.rs` + `prompt.rs`。
唯一要盯的是 **Stage 66 的接线**：spinner / 启动头要进 App，`app.header` 需要一个消费点，
如果它后续改 `pi-tui/src/lib.rs` 或 `interactive.rs`，就会和 65 撞同一文件（`lib.rs` 同文件、
`interactive.rs` 同文件）。下一轮收 66 时先看它的最终 diff，再决定谁先合。

### 9.2 Martty 对照复核：三条还没切的轴

前六轮对照的是「会话内容呈现」「输入通道」「信息密度」，本轮补三条轴。

**(1) `!` 本地 shell 是每条命令新起进程，Martty 是会话级持久 shell。**
Rust 侧 `interactive.rs:450` 的 `BashRunner::start` 直接调 `crate::tools::BashTool`
（`interactive.rs:458`），而 `BashTool` 每次 `Command::new("sh")`（`tools/bash.rs:125`，
Windows 走 `:478` 的 `cmd`），没有 `cd` / 环境变量延续。上游也是这个语义 ——
`executeBashWithOperations`（`packages/coding-agent/src/core/bash-executor.ts:50`）每条命令走一次
`BashOperations`。Martty 不同：`src/app.rs:718` 起维护 `PersistentShell`，用 fd 9 当控制通道
（`src/app.rs:793` 写 `eval <quoted>` + 状态标记，`:835` `shell_quote`），README 明确写
「shell 从 workspace 启动，`cd`、环境变量等状态会在后续 `!` 命令中保留，退出 TUI 后结束」。
这是**增强项而非上游对齐**，因此排在两个上游对齐切片之后（Stage 69）。

**(2) 思考级别（thinking level）零 UI —— 上游对齐里最后一块大的。**
`app.thinking.cycle` / `app.thinking.save` 在 `keybindings.rs:232-233` 只有定义、全仓零消费者，
`/thinking` 在 `slash.rs:76-95` 的 14 个解析分支里没有。上游却把这套做全了：
`app.thinking.cycle` → `cycleThinkingLevel()`（`interactive-mode.ts:2884`）、`/thinking` 打开
`ThinkingSelectorComponent`（`:2986`）、编辑器边框按级别取色（`:2139`）、当前模型不支持时给
`Current model does not support thinking` 状态（`:4170`）。Rust 侧的零件已经在位：
`ThinkingLevel`（`pi-agent-core/src/hooks.rs:87`，含 `is_reasoning()` `:106`）与
`Theme::thinking_border(level, text)`（`pi-tui/src/theme.rs:1108`），缺的是「App ↔ session 之间
把级别贯通」。因此这是 Stage 67。

**(3) 斜杠命令面 23 → 14（Stage 65 落定后 17），仍缺 6 条。**
Stage 65 补齐 `tree` / `fork` / `clone` 后，上游 23 条里还缺 `thinking`（并入 Stage 67）、
`reload`、`changelog`、`import`、`login`、`logout`、`scoped-models`、`share`
（`packages/coding-agent/src/core/slash-commands.ts:24-42`）。前 6 条依赖已经具备（auth 在
LUM-1171 / LUM-1180，导入导出在 LUM-1174），合并为 Stage 68；`/share` 要 GitHub gist 与凭据，
单独停放不动。因此 Stage 68。

### 9.3 已覆盖、不要重复投资的轴（本轮复核结论）

- **鼠标**：`pi-tui` 已有 cell / word / line 三档选词（`app.rs:472-479`）、拖拽边缘自动滚动
  （`app.rs:534-538`、`:915`），与 Martty 的「单击工具展开、双击选词、滚轮滚对话」等价，
  不需要再切。
- **流式输入**：排队 / steer / dequeue（Stage 61）、工具折叠 + 点击展开（Stage 58）、
  usage 脚注（Stage 64）均已落地，Martty README 对应行已有等价物。
- **不切**：`/liang` 像素宠物、UI Preset、ACP elicitation、`chrome.right` 右栏 —— 前两者是
  Martty 壳层的趣味/组合能力，与本仓「上游 pi 兼容」的目标无关；elicitation 与右栏依赖
  `ctx.ui` 扩展宿主（LUM-1184 已开面），等有真实消费者再排。

### 9.4 本轮动作

docs-only：新增本节 + 第五节 3 行 stage 表（67 / 68 / 69）。停放编号：Stage 67 = LUM-1230、
Stage 68 = LUM-1231、Stage 69 = LUM-1232（均 `backlog`，未启动 run，符合「最多 3 个并发」）。**未跑全量门** —— 两个 worker 正在
编译（1227 的 `target` 502M、1228 的 845M，均 12:37 仍在写入），第三方构建会争磁盘与 CPU；
tip 的门已由 LUM-1225 实跑（147 / 2202 / 0 / 2）。磁盘剩 25G，无陈旧 target 可回收
（只有这两个活跃 workdir 带 `target`，无其它任务残留）。校验：
`cargo fmt --all -- --check` 干净；本文件 fence 计数为偶数、新增段落零相对链接
（满足 `pi-evals/src/suites/docs.rs:123` 与 `:130` 两个 case）。

## 十、第八轮（LUM-1228）：Stage 66 落地 —— 等待反馈与启动可发现性

第六轮把两块空白合并成 Stage 66 停放 `backlog`；本轮把它实现、验证并合入。详细设计、与上游
逐项对照以及刻意保留的差异见新文件 **`docs/TUI_BUSY_AND_STARTUP.md`**，这里只记结论与核对。

1. **流式等待有动效了**：`crates/pi-tui/src/loader.rs`（新增）抄上游
   `packages/tui/src/components/loader.ts` 的 10 个盲文字形与 80 ms 帧率（与 Martty `SPINNER`
   逐字相同）。**没有新计时器**：`Spinner` 只有下标，推进点在 `App::tick_busy_feedback(now)`，
   唯一调用者是 `render_to_buffer` —— 渲染循环已有的 50 ms 节奏。`now` 是参数，测试可确定性推进。
2. **轮耗时落进 Stage 64 那一行**：footer 左簇变成 `⠋ 12s  <model>  <session>  <stats>`。
   空闲时 `busy = None`、该段零 span，footer 与改前逐字节相同（这是 92 个快照测试不用改的原因）。
   耗时是 Rust 侧新增（上游 `status-indicator.ts` 没有计时器）。
3. **启动头补上了**：内置头 = 标题（`pi v<version>`）+ 20 行 key hints + onboarding 文案；
   行数走已有扩展头通道（`ExtensionFrame::header`），在 `composed_frame` 里「扩展没设 header 才填
   内置行」，因此 `ctx.ui.setHeader` 仍然优先，且 `plan_chrome` 能先把 header 的行扣掉再算消息视口。
   折叠后 **0 行**（与上游「折叠留标题行」的差异已记录）。
4. **`app.header` 从 0 消费者变成真实键位**：上游把启动头的展开挂在 `app.tools.expand` 上，
   这里拆开并新注册 `app.header`（默认 `alt+h`），使 `APP_KEYBINDING_IDS` 43 → **44**。
   有注册时以注册为准、未注册时回落内置 chord，两个分支各有测试。
5. **文案集中成常量表**：`crates/pi-tui/src/locale.rs`（`Locale` / `tr(en, zh)` / `STARTUP_HINTS`
   的 `en`+`zh` 两列），键位列运行时按生效 chord 解析，未绑定的动作整行不显示（同 `/hotkeys` 规则）。
   `PI_LANG`（容忍 `zh-CN` 这类区域后缀）选择表格，默认英文 —— 没有引入 i18n 框架。
6. **开关对齐上游而非新造概念**：上游是 `quietStartup` 设置 + `--verbose`；Rust 侧是
   `InteractiveOptions::quiet_startup` + CLI `--no-header`，并在 `interactive_app_config`
   里收敛成纯函数（可无终端断言）。

核对（本轮新增/改动的测试，全部实跑）：

| 断言 | 证据 |
| --- | --- |
| 提交后 footer 立刻有 `⠋ 0s` | `crates/pi-tui/tests/busy_feedback.rs` |
| 80 ms 推进一帧、之间不成帧 | 同上（`SPINNER_INTERVAL_MS` 用合成 `Instant` 驱动） |
| 轮结束后 segment 消失、光标归零、第二次 tick 为空操作 | 同上 |
| 动画确实由 `render_to_buffer` 推进 | 同上（画进 `ratatui::Buffer` 后断言字形在屏幕上） |
| 启动头标题 / 行内容 / 默认关闭 / 折叠归还行数 + flash 文案 | 同上 |
| 对着**装好的**键位表解析真实 chord（19 行） | `crates/pi-tui/tests/startup_header.rs` |
| 覆盖 `app.header` 后 `alt+h` 失效、新键生效 | `crates/pi-tui/tests/keybinding_consumer.rs` |
| 驱动侧默认显示头、`--no-header` 不显示、flag 能解析 | `crates/pi-coding-agent/tests/startup_header.rs` |
| 43 → 44 的顺序与计数 | `crates/pi-coding-agent/tests/keybindings.rs` |
| `/hotkeys` 列出 `Alt+H` | `crates/pi-coding-agent/src/commands/slash.rs` 单测 |

未消费的 `app.*` 只剩第六轮列出的 `app.editor.external`（需终端 teardown/restore 交接）与
Stage 65 范围内的 `app.session.fork` / `app.session.resume`。

本轮实测：`cargo fmt --all -- --check` 干净；`cargo clippy -p pi-tui -p pi-coding-agent --all-targets
-- -D warnings` 退出码 0；`cargo test --workspace` **2238 passed / 0 failed**（其中 `pi-tui` 728、
`pi-coding-agent` 664）；`cargo test -p pi-evals` 8 passed（含 `docs-code-fences-balanced`，本节与
新文件都在审计范围内）。
 — 等待反馈与启动可发现性（spinner + 轮耗时 + 启动头 + app.header）(Stage 66))
