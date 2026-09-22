# LUM-1432 — 扩展生命周期事件补齐 20/36 → 36/36

> 基线：`origin/feature/pi.rs` = `40417a2d2`（LUM-1436 合并后的 tip）
> 环境：Windows x86_64 / cargo 1.97.1 / `--offline`
> 结论：**上游 36 个事件名全部有 Rust 变体、且全部有生产构造点**；
> 覆盖率脚本 `production emit sites: 36/36 (100.0%)`。
> 本文只写本轮真实做了什么的证据，口径与命令都可复现。

## 1. 交付内容

### 1.1 `pi-protocol/src/events.rs`：15 个新变体 + 3 个新枚举

| 上游事件名 | Rust 变体 | 载荷要点（字段名与上游逐字对齐） |
|---|---|---|
| `project_trust` | `ProjectTrust` | `cwd` |
| `session_before_switch` | `SessionBeforeSwitch` | `reason`(`new`/`resume`)、`targetSessionFile?` |
| `session_before_fork` | `SessionBeforeFork` | `entryId`、`position`(`before`/`at`) |
| `session_before_compact` | `SessionBeforeCompact` | `reason`、`willRetry`、`customInstructions?` |
| `session_compact_failed` | `SessionCompactFailed` | `reason`、`errorMessage?`、`aborted`、`willRetry`、`fromExtension` |
| `session_before_tree` | `SessionBeforeTree` | `targetId`、`oldLeafId?`、`userWantsSummary`、`customInstructions?` |
| `session_tree` | `SessionTree` | `newLeafId`、`oldLeafId`、`fromExtension` |
| `context` | `Context` | `messages` |
| `before_provider_request` | `BeforeProviderRequest` | `payload` |
| `before_provider_headers` | `BeforeProviderHeaders` | `headers` |
| `after_provider_response` | `AfterProviderResponse` | `status`、`headers` |
| `before_agent_start` | `BeforeAgentStart` | `prompt`、`systemPrompt` |
| `agent_settled` | `AgentSettled` | — |
| `ui_prompt_start` | `UiPromptStart` | `kind`、`title?` |
| `ui_prompt_end` | `UiPromptEnd` | `kind`、`title?` |

新枚举：`SessionBeforeSwitchReason`、`ForkPosition`、`UiPromptKind`。
`ExtensionEvent::name()` 的 wire tag 与上游名逐一相等，所以插件用上游名订阅即可。

### 1.2 `pi-agent-core`：异步生命周期缝

同步 fan-out（`EventObserver = Fn(AgentEvent)`）**无法 await 插件的回答**，而
`context` / `before_agent_start` 是请求/响应式钩子。因此新增
`hooks::LifecycleHooks`（全默认空实现）并接进 `AgentHookAdapter.lifecycle`：

| 方法 | loop / facade 调用点 | 语义 |
|---|---|---|
| `before_agent_start` | `agent.rs::prompt_content`（`AgentStart` 之前） | 返回 `Some(prompt)` → 替换本次 run 的 system prompt；下一次 run 回到 base（`base_system_prompt` 字段） |
| `context` | `agent_loop.rs::run_inner`（每个 provider 调用之前） | 返回的消息列表替换 `current_context.messages` |
| `agent_settled` | `agent.rs::prompt_content`（`AgentEnd` 之后，错误路径也走） | 观察 |
| `before_provider_request` | `agent_loop.rs::stream_assistant_events` | 观察（见 §3 保真度） |
| `before_provider_headers` | 同上 | 观察（见 §3） |
| `after_provider_response` | 同上（流建立 = `true`，失败 = `false`） | 观察（见 §3） |

`pi-agent-core` 不构造 `ExtensionEvent`（JS runtime 在 `pi-coding-agent`），只提供缝；
构造与投递由 `pi-coding-agent/src/extensions/lifecycle.rs` 的
`ExtensionLifecycleHooks` 负责。

### 1.3 `pi-coding-agent`：36 个生产构造点

| 事件 | 构造点 |
|---|---|
| `agent_start/end`、`turn_*`、`message_*`、`tool_*`、`user_message`、`input` | `extensions/events.rs`（LUM-1246 的 fan-out 映射表，未改） |
| `before_agent_start`、`context`、`agent_settled`、`before_provider_request`、`before_provider_headers`、`after_provider_response` | `extensions/lifecycle.rs`（`LifecycleHooks` 实现） |
| `ui_prompt_start` / `ui_prompt_end` | `extensions/lifecycle.rs`（`UiPromptObserver`），由 `ui_bridge.rs::TuiUiHandler` 在 `ctx.ui.confirm/input/select` 前后转发 |
| `project_trust` | `extensions/wiring.rs::ask_project_trust`，在扩展加载的两趟之间（见 §1.4） |
| `session_before_switch` | `interactive.rs`：`start_new_session`（`new`）、`resume_session`（`resume`） |
| `session_before_fork` | `interactive.rs::handle_fork_selection` |
| `session_before_compact` | `interactive.rs`：`run_compact`、`maybe_auto_compact` |
| `session_compact_failed` | 同上两处的失败分支 |
| `session_before_tree` / `session_tree` | `interactive.rs::handle_tree_selection` |
| `model_select` | `interactive.rs::apply_selector_choice`（唯一的模型切换点） |
| `session_start` / `session_shutdown` / `resources_discover` | `wiring.rs` / `bridge.rs`（既有） |

### 1.4 `project_trust`：两趟加载

上游在未信任项目上先加载 global / explicit 扩展，让它们回答 `project_trust`，再决定
是否评估项目自己的 `.pi/extensions`（`core/project-trust.ts`）。Rust 之前是**一趟**、
直接跳过事件（`main.rs` 里留有「`resolve_project_trusted` 不消费扩展结果」的注释）。
本轮把 `wiring::load` 改成：

1. 第一趟：global + explicit（`search.project = None`）；
2. 若项目有需信任资源且未信任 → `emit_event_with(ProjectTrust)`，取**第一个** `yes`/`no`
   （`undecided` 落空，与 `runner.ts:212` 一致）；`remember: true` 写回 trust store；
3. 回答 `yes` → 第二趟只加载 `.pi/extensions`（同一个 host，handler 不重复注册）；
4. 之后才发 `session_start`，项目扩展与 global 扩展一起收到。

`pi.on` 的别名解析同时接进 `ExtensionRuntime::has_subscriber_for`，所以订阅与投递不会对不上。

### 1.5 `pi-extensions`：别名表 + JS 侧映射

* `crates/pi-extensions/src/events.rs`：`UPSTREAM_EVENT_NAMES`(36) / `RUST_ONLY_EVENT_NAMES`(1) /
  `EVENT_ALIASES`(`session_end` → `session_shutdown`) / `canonical_event_name` /
  `is_known_event_name`。
* `runtime/pi-ext-shim.mjs`：同样的 `UPSTREAM_EVENT_NAMES` 数组；`pi.on` 与 `_pi_dispatch`
  都走 `canonicalEventName`（既有行为，本轮补全表）。
* 两边由单测 `shim_and_rust_event_tables_agree` 逐名比对 —— 表漂移会**红**，
  与 `extension_event_coverage.py --check-doc` 同一个思路。

## 2. 验证证据

### 2.1 覆盖率脚本

```text
$ python pi-rust/scripts/extension_event_coverage.py pi-rust
upstream events: 36 (36 subscribable + 0 declared-only)
wire tags matching upstream:    36/36 (100.0%)
production emit sites:          36/36 (100.0%)
rust-only tags (not upstream):  user_message
missing variants: 0/36

$ python pi-rust/scripts/extension_event_coverage.py pi-rust --check-doc
documented: 36 events; extracted upstream: 36
in sync: the parity doc's event list matches the code      (exit 0)
```

### 2.2 端到端测试（宿主构造 → JS 回调被调 → 回调解影响后续行为）

`crates/pi-coding-agent/tests/extension_lifecycle_hooks.rs`（4 条，全绿）走真实路径
`.pi/extensions/*.js → wiring::load → ExtensionRuntime → LifecycleHooks → provider 收到的东西`，
流侧是记录型 faux provider（无网络、无 API key）：

| 用例 | 行为断言 |
|---|---|
| `before_agent_start_can_replace_the_system_prompt` | handler 返回的 `systemPrompt` 就是 provider 调用携带的 system prompt（`Context.system_prompt`） |
| `context_can_rewrite_the_messages_sent_to_the_provider` | handler 追加的消息出现在 provider 收到的 `Context.messages` |
| `project_trust_extension_can_trust_an_untrusted_project` | `undecided` → 项目扩展的 `ext_echo` **不在** tools；`yes` → **在** |
| `lifecycle_hooks_are_installed_only_for_a_subscribing_runtime` | 同一 `ExtensionLifecycleHooks` 直调也生效（排除 harness 巧合） |

`crates/pi-agent-core/tests/hooks.rs`（2 条新增，全绿）：`lifecycle_hooks_fire_at_the_run_boundaries`
断言 6 个缝全部真被调用且 `context`/`before_agent_start` 的返回值进入 provider context；
`runs_without_lifecycle_hooks_are_unchanged` 保证不装钩子时行为不变。

`crates/pi-coding-agent/src/interactive.rs` 单测（3 条新增，真 QuickJS，全绿）：
`session_before_*` 的 `{cancel:true}` 真能 veto、非 cancel 结果不 veto、
`entryId`/`position`/`reason`/`targetSessionFile` 载荷真进 JS。

`crates/pi-coding-agent/src/extensions/lifecycle.rs` 单测（6 条，全绿）覆盖
`context`/`before_agent_start`/`agent_settled`/provider 边界/`ui_prompt_*` 的真 JS 回调。

## 3. 保真度缺口（没做到的，明说）

1. **`before_provider_request` / `before_provider_headers` 的返回值目前无处可放。**
   `pi-ai` 的 `StreamFn` 收的是类型化 `Context`，各家 adapter 自己用 credential + base URL
   组装 HTTP，没有任何 wire-payload / header 缝。本轮把事件按宿主**真实拥有**的数据构造并投递
   （请求描述符、空 header map），handler 返回值被记下但在 loop 处丢弃；要真生效需要给
   `pi-ai` 的 adapter 加 `on_payload` / `transform_headers` 回调（跨 5 个 adapter，本轮未做）。
2. **`after_provider_response` 的 `status` 是推导值**：adapter 未暴露 HTTP 状态，
   流建立 → `200`，调用失败 → `0`；`headers` 恒为空。
3. **`context` 不是逐 handler 链式**：shim 一次性把事件发给全部 handler，本轮折叠规则是
   「最后一个非空 `messages` 胜出」。handler 之间互相依赖时会与上游不同。
4. **`ui_prompt_start/end` 只在真 TUI 对话框上报**（`TuiUiHandler`）；print/rpc 模式的
   `StderrUiHandler` 不阻塞人，故不发。
5. **`session_before_*` 只在交互模式的对应命令上触发**（`/new`、`/resume`、`/fork`、
   `/compact`、`/tree`）。print 模式没有这些命令，故不会触发；`session_before_compact` 的
   `willRetry` 恒为 `false`（Rust 的自动压缩不在失败后重试同一轮）。
6. **`session_before_tree` 的 `userWantsSummary` 恒为 `false`**：Rust 的树导航只做
   `set_leaf`，没有上游的 branch-summary 路径，`customInstructions` 也没有来源。

## 4. 门禁（本 tip 实测，`--offline`）

| 命令 | 结果 |
|---|---|
| `cargo test --workspace --locked --no-fail-fast` | **2646 passed / 39 failed / 2 ignored**（基线 `40417a2d2` 同机 **2620 / 39**，失败集合逐条相同） |
| `cargo fmt --all -- --check` | EXIT=0 |
| `cargo clippy -p pi-protocol -p pi-agent-core -p pi-extensions -p pi-coding-agent --all-targets --locked` | 0 条来自本仓库的告警（余下均为 `vendor/rquickjs-core` 既有告警） |
| `python pi-rust/scripts/extension_event_coverage.py pi-rust --check-doc` | EXIT=0 |

39 条失败全部是**本机 Windows 环境类**（loopback HTTP、`/dev/urandom`、bash 工具、
短路径 `ADMINI~1` 路径断言），与 LUM-1431 §5 记录同类；本轮未新增任何失败（新增 26 条全绿）。

## 5. 未达项 / 后续

1. 给 `pi-ai` adapter 加 `on_payload` / `transform_headers` / `on_response` 回调，
   让 provider 三个事件的**返回值真生效**、`status`/`headers` 有真值（§3.1、§3.2）。
2. `context` 逐 handler 链式（需要 shim 支持 per-handler 派发或返回链）。
3. `session_before_*` 补上 `willRetry` / branch-summary 语义（依赖自动压缩的重试路径与树摘要）。
4. 事件轴数字已更新进 `docs/RUST_TS_PARITY_METRICS.md` §0.12 / §3.6 / §4.1。
