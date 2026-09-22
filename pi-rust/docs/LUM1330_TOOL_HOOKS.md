# LUM-1330 — `tool_call` / `tool_result` 成为真正的钩子（可拦、可改、可替换）

> scope: `pi-rust/crates/pi-protocol/src/{events.rs,tests/wire_types.rs}`,
> `pi-rust/crates/pi-extensions/src/{hook.rs,host.rs,lib.rs}`,
> `pi-rust/crates/pi-extensions/runtime/pi-ext-shim.mjs`,
> `pi-rust/crates/pi-extensions/tests/{tool_hooks.rs,host.rs}`,
> `pi-rust/crates/pi-agent-core/src/{hooks.rs,agent_loop.rs}`,
> `pi-rust/crates/pi-agent-core/tests/{tool_execution.rs,tool_parallel.rs}`,
> `pi-rust/crates/pi-coding-agent/src/extensions/{hook.rs,events.rs,wiring.rs,mod.rs}`,
> `pi-rust/crates/pi-coding-agent/src/{interactive.rs,print_mode.rs}`,
> `pi-rust/crates/pi-coding-agent/tests/tool_hooks.rs`,
> `pi-rust/crates/pi-extensions/docs/EXTENSIONS.md`,
> `pi-rust/scripts/{fake_model_server.py,pty_scenarios/lum1330-tool-hooks*.json}`
> branch: `agent/devbox1/c042dc77838e` → `feature/pi.rs`
> baseline: `feature/pi.rs` = `6a5a2faf2`（本轮所有"之前"数字都在该 tip 上实测）
> reference: codex/pi-ts `packages/coding-agent/src/core/extensions/runner.ts`
> （`emitToolCall` / `emitToolResult`）+ `types.ts`
> （`ToolCallEvent` / `ToolCallEventResult` / `ToolResultEventResult`）

一句话结论：上游最热的两个插件钩子在本端口**只是"能被听到"，不能被"听进去"**——`tool_call`
是 agent fan-out 在工具已经开始之后发的只读通知（载荷还是 Rust 原生的 `{ call }`），
`tool_result` 同理；handler 的返回值（`block` / `reason` / `terminate` / 内容替换）被
`deliver_event() -> bool` 直接丢掉。本轮把两者接进 agent 循环里**本来就有**的
`BeforeToolCall` / `AfterToolCall` 钩子：插件现在能拦住一个工具、改写它的参数、替换模型看到的
结果，载荷字段也对齐上游（`toolCallId` / `toolName` / `input` / `content` / `isError`）。
证据是真二进制：3 条 print 模式端到端用例 + 一对真 PTY 的 A/B 帧 + 6 条 QuickJS 往返用例。

---

## 1. 真实审计：本轮之前发生了什么

| 行为 | 本轮之前（`6a5a2faf2`，实测） | 本轮之后（实测） |
|---|---|---|
| 载荷字段 | `tool_call` = `{ type, call: { id, name, arguments } }`、`tool_result` = `{ type, result: {...} }`。上游插件读 `event.toolName` / `event.input` / `event.isError` 全部是 `undefined` | `{ type:"tool_call", toolCallId, toolName, input }`、`{ type:"tool_result", toolCallId, toolName, input, content, isError, details? }`；PTY 探针证明 handler 真的读到 `event.toolName` |
| 触发时机 | `ExtensionEventMapper::map` 从 `AgentEvent::ToolExecutionStart` 派生（`extensions/events.rs:87`、`:118` 两条映射），即 agent fan-out 的**只读**通道；`tool_call` 时工具已进入执行窗口 | `ExtensionToolHooks::before_tool_call` 在 executor 之前派发（`extensions/hook.rs:80`），`after_tool_call` 在结果定稿时派发（`extensions/hook.rs:104`）；fan-out 里两条映射**已删除**，不会重复投递 |
| 能否拦住 | 不能。生产代码里 `impl BeforeToolCall` / `impl AfterToolCall` 的数量 = **0**（`grep -rn "impl BeforeToolCall\|impl AfterToolCall" crates --include=*.rs` 只命中 `pi-agent-core/tests/*`），循环里的 `block` 语义没有任何生产调用方 | 能。端到端用例 `blocked_bash_never_runs_and_the_model_sees_the_reason` 断言被拦的命令**没有落到磁盘**，模型收到 `tool call blocked: blocked-by-extension` |
| 能否改参数 | 不能。`BeforeToolCallDecision` 只有 `block` / `reason` / `terminate`，`before_tool_call(&self, call: &ToolCall)` 拿的是不可变引用 | 能。新增 `BeforeToolCallDecision::input`，`prepare_call` 用补丁后的 `ToolCall` 交给 executor（`agent_loop.rs:950`）；用例 `patched_arguments_are_the_ones_the_executor_runs` 证明"模型说要 `original.txt`，真正执行的是 `patched.txt`" |
| 能否替换结果 | 不能。`ExtensionRuntime::deliver_event` 只返回 `outcome.handled`，`DispatchOutcome.results` 在 fan-out 路径上无人消费 | 能。`dispatch_tool_result` 折叠 `{ content, details, isError }` 并写回 `ToolResult`；用例 `tool_result_handler_replaces_what_the_model_sees` 证明 `SUPER_SECRET_VALUE` **没有**出现在第二次请求里，只有 `REDACTED-BY-EXTENSION` |
| 计数口径 | `scripts/extension_event_coverage.py` 把 `tool_call` / `tool_result` 记成"有生产发射点"——发射点确实有，但那个发射点只广播不回收 | 口径不变（仍是 21/36 tag、20/36 发射点）；变的是**这两个事件的语义**：从"通知"变成"钩子" |

也就是说：**事件覆盖率的数字本轮没有涨，涨的是这两个事件的能力**。这是有意的判断——
`tool_call` / `tool_result` 是上游插件最常挂的两个钩子（`runner.ts` 里唯一带"返回值改变
执行路径"语义的工具事件），而端口把它们做成了只读广播；补齐这一点的收益不在计数表上。

## 2. 上游语义与折叠规则（逐条对照）

| 规则 | 上游出处 | 端口实现 |
|---|---|---|
| `tool_call` handler 按注册顺序跑，返回 `{ block, reason, terminate }` | `runner.ts:982-1006` `emitToolCall` | `_pi_dispatch` 依次调用并把 `results` 按注册顺序带回；`ToolCallHookOutcome::from_dispatch` 折叠 |
| **第一个** `block: true` 立刻短路返回 | `runner.ts:994-997` `if (result.block) return result` | 命中即 `return`，后面的结果不再参与折叠（单测 `first_block_wins_and_stops_the_fold`） |
| 不 block 时**最后一个**非空结果决定 `terminate` | `runner.ts:991-994` `result = handlerResult` | 循环里最后一个结果覆盖 `terminate`（单测 `last_non_blocking_result_owns_terminate`） |
| 改参数 = 就地改 `event.input`，后续 handler 看得到，不再重新校验 | `types.ts:1126` "To modify arguments, mutate `event.input` in place" | shim 把 handler 收到的**同一个对象**回传（`DispatchOutcome.event`），宿主读回 `input`；只有与原始值**真的不同**才算补丁（单测 `unchanged_event_is_not_a_patch`） |
| `tool_result` 每条结果就地 patch `currentEvent`（`content`/`details`/`isError`/`usage`），后者看到前者的改动 | `runner.ts:927-980` `emitToolResult` | `ToolResultHookOutcome::from_dispatch` 从事件对象出发逐条 patch（单测 `patches_are_applied_in_order_and_later_wins`） |
| 一条都没改时返回 `undefined` | `runner.ts:970-972` `if (!modified) return undefined` | 返回 `None`，调用方保持原结果（单测 + QuickJS 用例 `a_result_nobody_touched_folds_to_none`） |
| 坏结果（字符串/数字/null）不拖垮同类结果 | 上游 try/catch 记 error 后继续 | `as_object()` / `serde_json::from_value` 失败即跳过该条（单测 `unparsable_result_is_skipped_not_fatal`） |

## 3. 改动清单

| 文件 | 内容 |
|---|---|
| `pi-protocol/src/events.rs:145`、`:159` | `ExtensionEvent::ToolCall` / `ToolResult` 改成上游载荷（`toolCallId`/`toolName`/`input`；`toolCallId`/`toolName`/`input`/`content`/`isError`/`details`），字段名用显式 `rename` 钉死 |
| `pi-extensions/src/hook.rs`（新增，`ToolCallHookOutcome` 在 :66、`ToolResultHookOutcome` 在 :131） | `ToolCallHookOutcome` / `ToolResultHookOutcome` / `ToolCallEventResult` / `ToolResultEventResult` + 折叠规则 + 16 条单测 |
| `pi-extensions/src/host.rs:2219` | `DispatchOutcome.event`：shim 回传 handler 看到的（可能已改的）事件对象 |
| `pi-extensions/runtime/pi-ext-shim.mjs:1130` | `_pi_dispatch` 成功路径回传 `event: parsed`（就地改参数的唯一通道） |
| `pi-coding-agent/src/extensions/wiring.rs:431`、`:460` | `ExtensionRuntime::dispatch_tool_call` / `dispatch_tool_result`：无订阅者时**不进 JS**（热路径短路） |
| `pi-coding-agent/src/extensions/hook.rs`（新增，`install_tool_hooks` :56、`before_tool_call` :80、`after_tool_call` :104） | `ExtensionToolHooks` 实现 `pi_agent_core::BeforeToolCall` / `AfterToolCall`；`install_tool_hooks()` 只在有订阅者时挂载；`calls` 表把 `toolName`/`input` 从 before 带到 after |
| `pi-coding-agent/src/interactive.rs:344` | TUI 在 agent 构造后立刻挂钩子（第一次工具调用之前） |
| `pi-coding-agent/src/print_mode.rs:617` | print 模式同样挂载（它也加载扩展，不能只有 TUI 生效） |
| `pi-agent-core/src/hooks.rs:70` | `BeforeToolCallDecision.input`（+ `with_input`）；`block()` 构造器补 `input: None` |
| `pi-agent-core/src/agent_loop.rs:920`、`:950`、`:989`、`:1039` | `CallPreparation::Execute(ToolCall)`：把补丁后的调用交给 executor（串行与并行两条路径） |
| `pi-coding-agent/src/extensions/events.rs:86`、`:122` | 从 fan-out 里**删掉** `tool_call` / `tool_result` 映射（否则会投递两次，且第二次在决策之后） |
| `pi-extensions/docs/EXTENSIONS.md` | 事件表修正 + 新增「Tool hooks」一节（含已知差异） |

## 4. 证据（真二进制，不是设计意图）

### 4.1 print 模式端到端：`crates/pi-coding-agent/tests/tool_hooks.rs`

真 `pi` 二进制 + 本地 loopback OpenAI 兼容服务 + 项目级扩展（`--approve`），3 条用例：

| 用例 | 断言 | 结果 |
|---|---|---|
| `blocked_bash_never_runs_and_the_model_sees_the_reason` | ① 被拦命令的落盘文件不存在 ② 第二次请求里出现 handler 的 `reason` ③ 出现循环合成的 `tool call blocked` | PASS |
| `patched_arguments_are_the_ones_the_executor_runs` | `patched.txt` 存在且内容为 `PATCHED`；`original.txt` 不存在（模型原命令没执行） | PASS |
| `tool_result_handler_replaces_what_the_model_sees` | 第二次请求含 `REDACTED-BY-EXTENSION`，**不含** `SUPER_SECRET_VALUE` | PASS |

### 4.2 QuickJS 往返：`crates/pi-extensions/tests/tool_hooks.rs`

真 shim + 真扩展源码，6 条用例，覆盖单测覆盖不到的部分——**事件对象回传**。
其中 `a_result_nobody_touched_folds_to_none` 第一次跑是红的：宿主在派发前会往信封里插
`_ctx_mode` / `_ctx_hasUI` / `_ctx_cwd`，shim 把整个信封回传，于是"事件对象变了"被误判成
"handler 改了结果"。修法是只按被跟踪字段比较（`hook.rs` 的 `changed()`），并补了一条回归单测
`injected_context_fields_are_not_a_patch`。这是本轮**由测试逼出来的真实缺陷**，不是设计推演。

### 4.3 真 PTY A/B（TUI，截图 + 字符网格）

`scripts/fake_model_server.py` 是本地 loopback 模型：先回一个工具调用，之后回文本——
`faux/faux-model` 只会回文本，拿不到工具调用，所以证据得靠它。命令
`printf %s%s RUNAWAY _MARKER` 的标记是**运行时拼出来的**，所以网格里出现连写的
`RUNAWAY_MARKER` 只可能是 shell 真跑了。

```bash
# 基线（扩展里没有 tool_call handler）：工具会跑
python3 pi-rust/scripts/fake_model_server.py --port 8137 --tool bash \
  --arguments '{"command": "printf %s%s RUNAWAY _MARKER"}' &
python3 pi-rust/scripts/pty_capture.py --bin <pi> \
  --steps pi-rust/scripts/pty_scenarios/lum1330-tool-hooks-baseline.json \
  --out pi-rust/docs/screenshots/lum1330-tool-hooks-baseline.png \
  --text-out pi-rust/docs/screenshots/lum1330-tool-hooks-baseline.png.txt   # 2/2 PASS

# 本轮（同一个扩展文件 + tool_call handler 返回 block）：
python3 pi-rust/scripts/pty_capture.py --bin <pi> \
  --steps pi-rust/scripts/pty_scenarios/lum1330-tool-hooks.json \
  --out pi-rust/docs/screenshots/lum1330-tool-hooks.png \
  --text-out pi-rust/docs/screenshots/lum1330-tool-hooks.png.txt           # 5/5 PASS
```

| 帧 | 网格里的事实 | 断言 |
|---|---|---|
| `lum1330-tool-hooks-baseline.png.txt` | `* bash printf %s%s RUNAWAY _MARKER` → `* RUNAWAY_MARKER` → `* Took 0.1s` | `expect: RUNAWAY_MARKER`（2/2 PASS） |
| `lum1330-tool-hooks.png.txt` | `* bash printf %s%s RUNAWAY _MARKER` → `* tool call blocked: blocked-by-extension`，**没有** `RUNAWAY_MARKER`；下一轮照常回答 `finished` | `expect: blocked-by-extension / tool call blocked`，`reject: RUNAWAY_MARKER`（5/5 PASS） |

两帧用同一个二进制、同一个扩展文件、同一个模型回复，唯一差别是扩展里那个
`pi.on("tool_call", …)` handler。

## 5. 已知差异与限制（诚实条目）

1. **`tool_execution_start` 仍在钩子之前发射，且带模型原始参数。** 循环是先
   `emit_tool_start`（为了让 TUI 把一次调用渲染成"整个生命周期都 running"，含钩子耗时），
   再 `prepare_call`。因此被拦的调用**仍会**出现 start/end 对，而补过参数的调用在 TUI 里显示的
   是原参数。上游相反：钩子先跑，被拦的调用不发执行事件。改它要动 TUI 的渲染语义与既有断言，
   属于单独一轮。
2. **被拦的调用不触发 `tool_result`**（循环里 block 走 `Immediate`，不经 `run_call`）。
   上游对被拦调用是否派发 `tool_result` 本轮未逐字核对，这里按端口既有路径保留。
3. **`content` 只能带一块。** 上游 `ToolResultEventResult.content` 是数组，
   `ToolResult.content` 在端口里是单块 `Box<Content>`。handler 返回多块时**不**截断，
   保留原内容并打 `tracing::warn`；要真支持得先把 `ToolResult` 改成 `Vec<Content>`（影响面大）。
4. **`usage` 补丁解析但不落地**（`ToolResult` 没有 per-tool usage 字段），与上游有差异。
5. **不做**：15 个尚未实现的扩展事件（`before_agent_start` / `agent_settled` /
   `ui_prompt_*` / `session_before_*` / provider 三件套等）、`tool_call` 的**逐工具类型**
   判别（上游把 `bash` / `read` / `edit` … 拆成联合类型，端口统一用 `toolName: String` +
   `input: Value`，TS 插件按字段读，行为等价，类型收窄不等价）。
6. **顺带观察（未定位根因）**：loopback 模型把工具参数写成含转义反斜杠（`"\\n"`）时，
   TUI 提交后停在 `in 0 out 0`，既不报错也不落工具结果。本轮未追（属 OpenAI 流式参数拼接
   面），记为待查项，不当作本轮的结论。

## 6. 门禁（本轮实测）

> 环境：Linux x86_64 / 32 核 / rustup `1.85.0`（workspace `rust-version = "1.75"`）/ 独立
> target 目录（`pi-rust/target`，避开与其他在飞任务的共享 target）/ `CARGO_PROFILE_DEV_DEBUG=0
> CARGO_PROFILE_TEST_DEBUG=0` / `TMPDIR=/tmp`。

| 门禁 | 命令 | 结果 |
|---|---|---|
| 格式 | `cargo fmt --all -- --check` | ✅ 退出 0（`FMT CLEAN`） |
| 静态检查 | `cargo clippy --offline -p pi-protocol -p pi-agent-core -p pi-extensions -p pi-coding-agent --all-targets -- -D warnings` | ✅ 无 error / warning（首轮抓出一条 `question_mark`，已按建议改掉） |
| 工作区编译 | `cargo check --offline --workspace --all-targets` | ✅ 退出 0，无 error / warning |
| 测试 | `cargo test --offline -p pi-protocol -p pi-agent-core -p pi-extensions -p pi-coding-agent --no-fail-fast` | **1109 passed / 1 failed**，64 个 target |

唯一一条红：`reload_config.rs:192` 的 `reload_falls_back_to_defaults_and_reports_a_missing_file`
（断言 `transcript(&app).contains("has no file")`，本环境里转写被换行成 `has no` / `file`）。
**不是本轮引入**：把本轮 `src/` 改动 `git stash` 后同一条测试照样红（已实测），且该测试
只驱动 `reload()` + `App`，不经过 agent、扩展与工具路径。属于既有缺陷，见 §8 第 5 条。

过程中的两次 ENOSPC（本机同时有 4 个在飞 autopilot 任务共用 50 G 卷）让 8 个 target 的
临时目录创建失败（`StorageFull`），腾出空间后原样重跑：8/8 target 全绿（`tools_render` 29、
`tools_navigation` 14、`pi_exec` 7、`pi_ai_provider` 6、`node_module_readline` 10、
`node_builtins` 8、`child_process` 1、`builtin_tool_factories` 4）。**那 58 条红是磁盘满，
不是代码**。

## 7. 完成度（真实百分比，含公式）

| 口径 | 数值 | 怎么量的 |
|---|---|---|
| 纯代码规模 | **88.2%** | `find pi-rust/crates -path '*/src/*' -name '*.rs' ! -name 'mod.rs' \| xargs wc -l` = 135,080 / `find packages -path '*/src/*' -name '*.ts' \| xargs wc -l` = 153,106 |
| 测试规模 | **48.6%** | `grep -rho '#\[test\]\|#\[tokio::test\]' pi-rust/crates --include=*.rs \| wc -l` = 2,579 / `find packages -name '*.test.ts' \| xargs grep -ho '\bit(\|\btest(' \| wc -l` = 5,309 |
| `app.*` 接线 | **43/44 = 97.7%** | `python3 pi-rust/scripts/app_action_coverage.py`（`--check-consumed` 同 tip 通过：`CONSUMED_APP_ACTIONS: 43 entries; measured wired: 43`） |
| 扩展事件 | **21/36 tag（58.3%）、20/36 生产发射点（55.6%）** | `python3 pi-rust/scripts/extension_event_coverage.py`（本轮**未**新增事件，只改了其中两个的语义） |
| 功能面加权（§4 权重表） | **≈84.6%** | 5×1.00 + 13×0.90 + 8×1.00 + 6×0.70 + 14×**0.977** + 8×0.91 + 7×0.78 + 7×0.70 + 8×0.95 + 7×**0.556** + 9×0.85 + 5×**0.486** + 3×0.95 = 84.64；若按"模块 80.5% 与接线 97.7% 取均值"给 TUI 轴（0.891）则为 83.4% |
| 本轮的增量在哪 | 不在计数表上 | 见 §1 末段：覆盖率数字不变，变的是最高频两个工具事件从"只读广播"变成"能拦能改" |

## 8. 后续（按性价比）

1. **`tool_execution_start` 顺序对齐**（钩子先跑；被拦的调用不发执行事件）——闭合 §5.1，
   影响 TUI 渲染语义，要连带改 `tool_parallel.rs` / `tool_execution.rs` 的既有断言。
2. **补 15 个缺失事件**里最高频的三个：`agent_settled`（一次 run 真正落定）、
   `before_agent_start`（提交到循环之前）、`ui_prompt_start/end`（扩展阻塞式 UI 等待）。
   这三个在端口里都有明确发射点，是覆盖率轴上最便宜的一段。
3. **`ToolResult.content` 改成多块**，才能让 `tool_result` 的 `content` 补丁完全等价上游（§5.3）。
4. **`@` mention / paste 标记随草稿进入 history**、**slash 命令进 history**（LUM-1319 §5.2 遗留）。
5. **`reload_config.rs:192` 的既有红**：断言依赖转写是否被换行（`has no file` 被折成两行即失败），
   与终端宽度/环境有关。与 `tool_execution_start` 顺序无关，单开一条即可修（§6）。
6. **工具参数里的转义反斜杠**（§5.6）：TUI 停在 `in 0 out 0` 的现象值得单独复现一次。
