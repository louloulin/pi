# pi-rust 扩展 API 稳定性与 TS 对位表

> **目标**：让任意 JS 扩展作者能判断「这个 API 在 pi-rust 里能不能用」
> **基线**：TS `pi-coding-agent` v0.85.1+ + pi-rust `feature/pi.rs` 分支
> **维护策略**：月度 diff 监控；新 API 进入稳定集需经 `tests/ts_api_parity.rs` 反射对位 + 至少 1 个真实 JS 扩展使用

---

## 1. 当前稳定子集（v0.85.x 兼容）

### 1.1 TUI 渲染层（pi-tui crate）

| TS `pi-tui` 导出 | Rust 路径 | 状态 |
|---|---|---|
| `Box` / `BoxLayout` | `pi_tui::BoxLayout` | ✅ |
| `CancellableLoader` | `pi_tui::CancellableLoader` | ✅ |
| `Markdown` | `pi_tui::Markdown` | ✅ |
| `VStack` / `HStack` | `pi_tui::VStack` / `pi_tui::HStack` | ✅ |
| `Container` | `pi_tui::Container` | ✅ |
| `Spacer` | `pi_tui::Spacer` | ✅ |
| `ExpandableText` | `pi_tui::ExpandableText` | ✅ |
| `TruncatedText` | `pi_tui::TruncatedText` | ✅ |
| `Text` | `pi_tui::Text` | ✅ |
| `SelectList` / `Selector` | `pi_tui::SelectList` / `pi_tui::Selector` | ✅ |
| `Settings` | `pi_tui::Settings` | ✅ |
| `ProcessTerminal` | `pi_tui::ProcessTerminal` | ✅ |
| `TuiAltScreen` | `pi_tui::TuiAltScreen` | ✅ |
| `DynamicBorder` | `pi_tui::DynamicBorder` | ✅ |
| `Keybindings` / `KeybindingManager` | `pi_tui::Keybindings` / `pi_tui::KeybindingManager` | ✅ |
| `Loader` | `pi_tui::Loader` | ✅ |
| `MarkdownTheme` | `pi_tui::MarkdownTheme` | ✅ |
| `Theme` / `load_theme` | `pi_tui::Theme` / `pi_tui::load_theme` | ✅ |
| `Input` / `Editor` | `pi_tui::Input` / `pi_tui::Editor` | ✅ |

**stable version**：与 `pi-tui` v0.85.x 一一对应。

### 1.2 扩展钩子（pi-coding-agent/extensions）

| TS API | Rust 路径 | 状态 | 备注 |
|---|---|---|---|
| `extension.on('agent_start', …)` | `ExtensionRuntime::dispatch_event(AgentEvent::AgentStart)` | ✅ | |
| `extension.on('agent_end', …)` | 同上 + `AgentEnd` | ✅ | |
| `extension.on('turn_start', …)` | `AgentEvent::TurnStart` | ✅ | |
| `extension.on('message_start', …)` | `AgentEvent::MessageStart { model }` | ✅ | |
| `extension.on('message_update', …)` | `AgentEvent::MessageUpdate(...)` | ✅ | |
| `extension.on('message_end', …)` | `AgentEvent::MessageEnd { message }` | ✅ | |
| `extension.on('tool_execution_start', …)` | `AgentEvent::ToolExecutionStart` | ✅ | |
| `extension.on('tool_execution_update', …)` | `AgentEvent::ToolExecutionUpdate` | ✅ | |
| `extension.on('tool_execution_end', …)` | `AgentEvent::ToolExecutionEnd` | ✅ | |
| `extension.on('turn_end', …)` | `AgentEvent::TurnEnd` | ✅ | |
| `extension.on('user_message', …)` | `AgentEvent::UserMessage` | ✅ | |
| `extension.on('context', …)` | `ExtensionLifecycleHooks::context` | ✅ | 可 mutate messages |
| `extension.on('before_agent_start', …)` | `ExtensionLifecycleHooks::before_agent_start` | ✅ | 可追加 systemPrompt |
| `extension.on('before_provider_request', …)` | `ExtensionLifecycleHooks::before_provider_request` | ⚠️ partial | 文档化 gap（见 §3.1） |
| `extension.on('before_provider_headers', …)` | `ExtensionLifecycleHooks::before_provider_headers` | ⚠️ partial | 文档化 gap（见 §3.1） |
| `extension.on('after_provider_response', …)` | `ExtensionLifecycleHooks::after_provider_response` | ⚠️ partial | 仅 200/0 占位（见 §3.1） |
| `extension.on('agent_settled', …)` | `ExtensionLifecycleHooks::agent_settled` | ✅ | |
| `extension.on('before_tool_call', …)` | `ExtensionToolHooks::before_tool_call` | ✅ | block / terminate / input patch |
| `extension.on('after_tool_call', …)` | `ExtensionToolHooks::after_tool_call` | ✅ | content / is_error / details rewrite |
| `extension.on('prompt_started', …)` | `UiPromptObserver::prompt_started` | ✅ | |
| `extension.on('prompt_finished', …)` | `UiPromptObserver::prompt_finished` | ✅ | |

### 1.3 TUI 订阅面（pi-tui/app/agent_events）

| 类型 | 路径 | 说明 |
|---|---|---|
| `AgentEventHandler` (trait) | `pi_tui::app::AgentEventHandler` | 12 个默认 no-op 回调 |
| `AgentEventRouter` | `pi_tui::app::AgentEventRouter` | 泛型 fan-out 容器 |
| `default_router()` | `pi_tui::app::default_router` | 空 router 工厂 |

**新增于 2026-09-26（M1 第二刀）**。

---

## 2. 当前已知缺口（M2-P0 待补）

### 2.1 ExtensionUIContext 方法缺口（19 个）

按规划 `tests/ts_api_parity.rs` 反射枚举目标补齐：

| 缺失方法 | 类型 | 计划任务 |
|---|---|---|
| `setWorkingMessage(msg)` | UI | P0-2 |
| `setWorkingVisible(visible)` | UI | P0-2 |
| `setWorkingIndicator(opts)` | UI | P0-2 |
| `setHiddenThinkingLabel(label)` | UI | P0-2 |
| `getEditorText()` | Query | P0-2 |
| `getEditorComponent()` | Query | P0-2 |
| `getTheme(name)` | Query | P0-2 |
| `getAllThemes()` | Query | P0-2 |
| `getToolsExpanded()` | Query | P0-2 |
| `setToolsExpanded(expanded)` | UI | P0-2 |
| `onTerminalInput(handler)` | Subscribe | P0-2 |
| `addAutocompleteProvider(factory)` | UI | P0-2 |
| `pasteToEditor(text)` | UI | P0-2 |
| `editor(title, prefill)` | Dialog | P0-2 |
| `registerMarkdownTransformer(transformer)` | Register | P0-6 |
| `registerMessageRenderer(role, renderer)` | Register | P0-5 |
| `registerEntryRenderer(customType, renderer)` | Register | P0-5 |
| `registerShortcut(chord, callback)` | Register | P0-3 |
| `registerFlag(flag)` | Register | P0-2 |

### 2.2 ExtensionEvent 家族缺口（10 个 session_*）

`SessionStart / SessionBeforeSwitch / SessionBeforeFork / SessionBeforeCompact / SessionCompact / SessionCompactFailed / SessionShutdown / SessionBeforeTree / SessionTree / SessionInfoChanged` — 计划任务 P1-2。

### 2.3 Tool 渲染 trait 缺口（3 个）

`renderCall / renderResult / promptSnippet / promptGuidelines / prepareArguments / constrainedSampling` — 计划任务 P1-1。

---

## 3. 已文档化的实现差距（historical）

### 3.1 Provider 请求 / 响应钩子

来源：`crates/pi-coding-agent/src/extensions/lifecycle.rs` 文档注释

- **`before_provider_request`**：Rust handler 返回 `Some(replacement)`，但**当前循环忽略返回值**（provider adapter 只接收 typed `Context`，不接受 generic JSON 替换）。要在所有 provider 类型上落地 mutation 语义，需要把 Context 泛型化或引入 `ProviderPayload` 中间层。
- **`before_provider_headers`**：Rust handler 返回值类型为 `()`（仅 mutate 入参 headers）。文档说"return value is ignored"——与 TS 完全一致，✅。
- **`after_provider_response`**：Rust handler 仅收到 `ok: bool` 占位（200/0），不携带完整 response body。完整化需把 provider response shape 也泛型化。

**结论**：这 3 个钩子在 Rust 端**骨架存在**，但完整 mutation 语义需要 provider 层重设计。预计 P3 解决。

### 3.2 Tool 渲染

Rust 把 per-tool `renderCall` / `renderResult` 拆到独立 `tools/render.rs::ToolBlockRenderer`，不在 `AgentTool` trait 里。扩展想自定义 tool 渲染需注册 `ToolBlockRenderer`（参见 `crates/pi-tui/src/components/tool_block_renderer.rs`）。P1-1 将把它合到 `AgentTool` trait。

---

## 4. 扩展作者使用守则

### 4.1 在 manifest 里声明 `required_api`

```json
{
  "name": "my-extension",
  "version": "0.1.0",
  "pi_version": ">=0.85.0",
  "required_api": [
    "ExtensionUIContext.setWidget",
    "ExtensionUIContext.setStatus",
    "ExtensionUIContext.registerShortcut",
    "AgentEventHandler.on_tool_call"
  ]
}
```

启动时 `pi-rust` 会校验 `required_api` 中每一项：
- ✅ 全在当前稳定子集 → 启动
- ⚠️ 任一项为缺口（P0/P1） → **fail-fast** + 输出 `docs/API_STABILITY.md` 链接
- ⚠️ 任一项为 historical gap（§3） → 启动但打印 warning

### 4.2 渐进式迁移策略

| 旧 TS-only API | Rust 推荐替代 |
|---|---|
| `ui.setWidget` | 同名（已可用） |
| `ui.onTerminalInput` | 用 `App::drain_input_events` + 自实现 router |
| `ui.registerShortcut` | 暂用 `keybindings.rs` 直接注册（缺 P0-3） |
| `ui.getTheme` | 暂用 `app.theme()` 字段直读 |
| `ui.editor()` | 暂用 `dialog.input()` 多行变体 |

### 4.3 不推荐依赖的 API

以下 API 在 TS 端是 experimental（带 `@experimental` 标签），Rust 端**未实现**，扩展不应使用：
- `ui.registerFlag`（P0-2 计划补）
- `before_provider_request.payload.replace`（provider 层未泛型化）
- `SessionEvent` 家族（10 个 variant，P1-2 计划补）

---

## 5. 验证工具

| 命令 | 用途 |
|---|---|
| `cargo test -p pi-tui --lib app::agent_events` | AgentEventHandler trait 单测 |
| `cargo test -p pi-coding-agent --lib extensions` | 钩子单测 |
| `cargo test -p pi-tui --lib ts_api_parity` | （待 P0-2 完成） |
| `cargo test -p pi-coding-agent --lib ts_event_parity` | （待 P1-2 完成） |

---

## 6. 月度 diff 流程

每月 1 号跑：

```bash
./scripts/ts_diff.sh
# 输出 TS vX.Y.Z 新增/移除/变更方法表
# 自动开 issue：pi-rust-gap-{method-name}
```

扩展作者可在 `required_api` 里引用未实现的 method，CI 会 fail-fast + 给出对应 issue 链接。