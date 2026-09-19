//! Embedded QuickJS host for JS extensions.
//!
//! [`JsExtensionHost`] owns one [`AsyncRuntime`] / [`AsyncContext`]
//! pair, installs the host import functions the JS shim calls into,
//! and evaluates extension source via [`JsExtensionHost::load`].
//! `ExtensionEvent`s are dispatched via [`JsExtensionHost::emit_event`]
//! which calls `_pi_dispatch` on the shim.
//!
//! Tool registrations are accumulated in [`JsExtensionHost::registry`];
//! when the agent calls a tool, the host invokes the JS-side execute
//! function via [`JsExtensionHost::execute_tool`].

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use parking_lot::Mutex;
use pi_protocol::{ExtensionEvent, ToolDefinition, UiLevel, UiRequest, UiResponse};
use rquickjs_core::function::{Async, Func};
use rquickjs_core::prelude::CatchResultExt;
use rquickjs_core::promise::MaybePromise;
use rquickjs_core::{async_with, Ctx, Function};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};

use crate::api::{ExtensionCapabilities, ExtensionEntry};
use crate::error::ExtensionError;
use crate::registry::ExtensionRegistry;
use crate::shim::SHIM_SOURCE;

/// Default timeout applied to every host import and event dispatch.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

/// User-supplied UI handler. The host calls these when a JS extension
/// requests user interaction via `ctx.ui.{confirm,input,select}` or
/// fires a `notify`.
///
/// The methods are `async` because an interactive handler has to
/// *suspend* until the user answers — a key press can arrive seconds
/// (or minutes) after the extension asks. The alternative (a sync trait
/// plus `block_in_place` + a oneshot) would park a tokio worker thread
/// per prompt and panic outright on a `current_thread` runtime, which
/// is what every `pi-extensions` test uses. Awaiting keeps the
/// `ui_worker` task cooperative and leaves the runtime free to drive
/// the QuickJS promise the JS side is awaiting.
///
/// Every method has a non-interactive default (`deny` / `cancel` /
/// no-op) so the bundled non-interactive handler only has to override
/// [`UiHandler::notify`].
#[async_trait]
pub trait UiHandler: Send + Sync + 'static {
    /// Return `true` to accept, `false` to deny.
    async fn confirm(&self, title: &str, body: &str) -> bool {
        let _ = (title, body);
        false
    }
    /// Return `Some(value)` when the user supplies text, `None` to cancel.
    async fn input(&self, title: &str, placeholder: Option<&str>) -> Option<String> {
        let _ = (title, placeholder);
        None
    }
    /// Return `Some(value)` when the user picks an option, `None` to cancel.
    async fn select(&self, title: &str, options: &[String]) -> Option<String> {
        let _ = (title, options);
        None
    }
    /// Surface a notification. Default impl is a no-op.
    async fn notify(&self, message: &str, level: UiLevel) {
        let _ = (message, level);
    }
}

/// No-op UI handler — used when tests don't care about prompts.
#[derive(Debug, Default)]
pub struct NullUiHandler;

impl UiHandler for NullUiHandler {}

/// A `UiHandler` that returns canned answers from a fixed
/// [`ScriptedUiAnswers`]. Useful in tests and the e2e fixture.
#[derive(Debug, Clone, Default)]
pub struct ScriptedUiHandler {
    answers: Arc<Mutex<ScriptedUiAnswers>>,
}

impl ScriptedUiHandler {
    /// Wrap a set of canned answers.
    pub fn new(answers: ScriptedUiAnswers) -> Self {
        Self {
            answers: Arc::new(Mutex::new(answers)),
        }
    }
}

#[async_trait]
impl UiHandler for ScriptedUiHandler {
    async fn confirm(&self, title: &str, _body: &str) -> bool {
        self.answers
            .lock()
            .confirms
            .get(title)
            .copied()
            .unwrap_or(false)
    }
    async fn input(&self, title: &str, _placeholder: Option<&str>) -> Option<String> {
        self.answers.lock().inputs.get(title).cloned()
    }
    async fn select(&self, title: &str, _options: &[String]) -> Option<String> {
        self.answers.lock().selects.get(title).cloned()
    }
}

/// Canned UI answers keyed by title.
#[derive(Debug, Clone, Default)]
pub struct ScriptedUiAnswers {
    /// `confirm(title) → accepted`
    pub confirms: std::collections::HashMap<String, bool>,
    /// `input(title) → text`
    pub inputs: std::collections::HashMap<String, String>,
    /// `select(title) → picked option`
    pub selects: std::collections::HashMap<String, String>,
}

/// Envelope for a UI request flowing from a JS extension to the host.
struct UiRequestEnvelope {
    /// What the extension wants.
    request: UiRequest,
    /// Channel the host writes the answer into.
    reply: oneshot::Sender<Option<UiResponse>>,
}

/// Collected registrations emitted by a JS extension via host imports.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RegistrationLog {
    /// Tools registered in load order.
    pub tools: Vec<ToolDefinition>,
    /// Commands registered in load order.
    pub commands: Vec<RegisteredCommand>,
    /// Custom entries appended via `pi.appendEntry` and host-import
    /// call traces (`ui_notify`, `ui_confirm`, …).
    pub entries: Vec<AppendedEntry>,
    /// Custom messages sent via `pi.sendMessage`.
    pub messages: Vec<serde_json::Value>,
    /// User messages sent via `pi.sendUserMessage`.
    pub user_messages: Vec<String>,
    /// Session name set via `pi.setSessionName`.
    pub session_name: Option<String>,
}

/// One `/`-prefixed command registered via `pi.registerCommand`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisteredCommand {
    /// Command name (the argument the user types after `/`).
    pub name: String,
    /// Short description surfaced in command palette / docs.
    #[serde(default)]
    pub description: String,
}

/// System-prompt contribution declared by a tool registered via
/// `pi.registerTool`.
///
/// `promptSnippet` becomes the tool's line in the prompt's
/// `Available tools` list; `promptGuidelines` are appended to the
/// `Guidelines` section. Both are optional, so a tool that declares
/// neither simply stays out of the prompt while remaining callable
/// through the tool registry.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisteredToolPrompt {
    /// Tool name (`pi.registerTool` `name`).
    pub name: String,
    /// One-line summary, or `None` when the extension declared none.
    #[serde(default)]
    pub snippet: Option<String>,
    /// Extra guideline bullets contributed by the tool.
    #[serde(default)]
    pub guidelines: Vec<String>,
}

/// One entry written to the host's log, either via `pi.appendEntry`
/// or as a side-effect trace from a UI call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppendedEntry {
    /// Custom type tag (`ui_notify`, `ui_confirm`, …) the host can
    /// filter on.
    pub custom_type: String,
    /// JSON payload carried alongside the type tag.
    pub data: serde_json::Value,
}

/// Outcome of invoking an extension-registered slash command through
/// [`JsExtensionHost::execute_command`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandExecutionOutcome {
    /// `true` when a command with that name existed and its handler
    /// ran. `false` means the name is not an extension command.
    pub handled: bool,
    /// `true` when the handler threw or rejected.
    pub is_error: bool,
    /// The handler's return value normalised to JSON (`null` for
    /// `undefined` / non-serialisable values).
    pub result: serde_json::Value,
    /// Error text when `is_error` is `true`.
    pub error: Option<String>,
}

/// Everything the host accumulated since the previous
/// [`JsExtensionHost::drain_side_effects`] call.
///
/// Tool / command registrations are deliberately *not* part of this
/// snapshot: they describe the loaded extension set and are read via
/// [`JsExtensionHost::registered_tools`] /
/// [`JsExtensionHost::registered_commands`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExtensionSideEffects {
    /// Entries appended via `pi.appendEntry`. UI call traces
    /// (`ui_confirm` / `ui_notify` / …) are diagnostics only and are
    /// dropped by [`JsExtensionHost::drain_side_effects`].
    pub entries: Vec<AppendedEntry>,
    /// Custom messages sent via `pi.sendMessage`.
    pub messages: Vec<serde_json::Value>,
    /// User messages sent via `pi.sendUserMessage`.
    pub user_messages: Vec<String>,
    /// Session name set via `pi.setSessionName` since the last drain.
    pub session_name: Option<String>,
}

/// Configuration for [`JsExtensionHost::with_options`].
#[derive(Clone, Default)]
pub struct HostOptions {
    /// Per-call timeout. `None` falls back to [`DEFAULT_TIMEOUT`].
    pub timeout: Option<Duration>,
    /// Optional UI handler. Defaults to [`NullUiHandler`].
    pub ui_handler: Option<Arc<dyn UiHandler>>,
    /// Context the shim hands to a tool's `execute(args, ctx)`.
    pub tool_context: ToolContext,
}

/// The session context an extension tool sees as its second argument.
///
/// Upstream calls `execute(args, ctx)` with the same `ExtensionContext`
/// events and commands receive, so `ctx.hasUI` / `ctx.mode` decide
/// whether `ctx.ui.confirm` can prompt. The Rust host stores it once per
/// host (the mode never changes mid-session) instead of threading it
/// through every [`JsExtensionHost::execute_tool`] call site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolContext {
    /// `ctx.mode` (`tui` / `print` / `rpc`).
    pub mode: String,
    /// `ctx.hasUI` — `true` only when a real interactive handler exists.
    pub has_ui: bool,
    /// `ctx.cwd`.
    pub cwd: String,
}

impl Default for ToolContext {
    fn default() -> Self {
        Self {
            mode: "print".to_string(),
            has_ui: false,
            cwd: String::new(),
        }
    }
}

impl std::fmt::Debug for HostOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostOptions")
            .field("timeout", &self.timeout)
            .field(
                "ui_handler",
                &self.ui_handler.as_ref().map(|_| "<dyn UiHandler>"),
            )
            .field("tool_context", &self.tool_context)
            .finish()
    }
}

impl HostOptions {
    /// Override the per-call timeout.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }
    /// Install a UI handler.
    pub fn with_ui_handler(mut self, handler: Arc<dyn UiHandler>) -> Self {
        self.ui_handler = Some(handler);
        self
    }
    /// Set the context tool executions see.
    pub fn with_tool_context(mut self, context: ToolContext) -> Self {
        self.tool_context = context;
        self
    }
}

/// Embedded QuickJS host. Cloning shares the underlying runtime +
/// context; both must be driven from a tokio runtime.
#[derive(Clone)]
pub struct JsExtensionHost {
    inner: Arc<Inner>,
}

struct Inner {
    /// Kept alive so the [`AsyncContext`](rquickjs_core::AsyncContext)
    /// below remains valid; the context holds a clone of the runtime
    /// but dropping the original would invalidate it.
    #[allow(dead_code)]
    runtime: rquickjs_core::AsyncRuntime,
    context: rquickjs_core::AsyncContext,
    ui_tx: mpsc::UnboundedSender<UiRequestEnvelope>,
    state: Arc<Mutex<HostState>>,
    timeout: Duration,
    /// Context handed to tool executions (see [`ToolContext`]).
    tool_context: ToolContext,
    /// Wall-clock nanos deadline the JS interrupt handler checks on
    /// every iteration. `u64::MAX` means "no deadline active".
    deadline_nanos: Arc<AtomicU64>,
}

#[derive(Default)]
struct HostState {
    registry: ExtensionRegistry,
    log: RegistrationLog,
    /// Index of [`HostState::log::tools`] into [`HostState::registry`]
    /// so `host_register_tool` can attribute tools to the extension
    /// that registered them without widening the host-import ABI.
    pending_extension: Option<String>,
}

impl JsExtensionHost {
    /// Build a new host with default options.
    pub async fn new() -> Result<Self, ExtensionError> {
        Self::with_options(HostOptions::default()).await
    }

    /// Build a new host with a UI handler attached.
    pub async fn with_handler(ui_handler: Arc<dyn UiHandler>) -> Result<Self, ExtensionError> {
        Self::with_options(HostOptions {
            ui_handler: Some(ui_handler),
            ..HostOptions::default()
        })
        .await
    }

    /// Build a new host with the given options.
    pub async fn with_options(opts: HostOptions) -> Result<Self, ExtensionError> {
        let timeout = opts.timeout.unwrap_or(DEFAULT_TIMEOUT);
        let runtime = rquickjs_core::AsyncRuntime::new().map_err(ExtensionError::from)?;
        let context = rquickjs_core::AsyncContext::full(&runtime)
            .await
            .map_err(ExtensionError::from)?;
        let (ui_tx, ui_rx) = mpsc::unbounded_channel::<UiRequestEnvelope>();
        let state = Arc::new(Mutex::new(HostState::default()));
        let deadline_nanos = Arc::new(AtomicU64::new(u64::MAX));
        let inner = Arc::new(Inner {
            runtime: runtime.clone(),
            context: context.clone(),
            ui_tx,
            state,
            timeout,
            tool_context: opts.tool_context.clone(),
            deadline_nanos: deadline_nanos.clone(),
        });

        // Install host imports + shim.
        let install_inner = inner.clone();
        async_with!(context => |ctx| {
            install_imports(&ctx, &install_inner).map_err(ExtensionError::from)?;
            // Load the shim. Failures here are fatal — the host cannot
            // serve any extension without the shim.
            ctx.eval::<(), _>(SHIM_SOURCE)
                .catch(&ctx)
                .map_err(ExtensionError::from)?;
            Ok::<_, ExtensionError>(())
        })
        .await?;

        // Spawn the runtime driver so JS promises + Async host imports
        // are pumped even when the host is otherwise idle.
        tokio::spawn(runtime.drive());

        // Install an interrupt handler that aborts the JS loop when
        // the deadline is exceeded. QuickJS calls the handler on every
        // bytecode iteration, so an infinite `while(true)` is caught
        // within a few thousand opcodes rather than blocking the host
        // forever.
        let interrupt_deadline = deadline_nanos.clone();
        runtime
            .set_interrupt_handler(Some(Box::new(move || {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_nanos() as u64)
                    .unwrap_or(0);
                let deadline = interrupt_deadline.load(Ordering::Relaxed);
                deadline != u64::MAX && now >= deadline
            })))
            .await;

        // Spawn the UI worker.
        let worker_state = inner.state.clone();
        let worker_handler = opts.ui_handler.clone();
        tokio::spawn(ui_worker(ui_rx, worker_handler, worker_state));

        Ok(Self { inner })
    }

    /// Per-call timeout.
    pub fn timeout(&self) -> Duration {
        self.inner.timeout
    }

    /// Borrow the accumulated registration log (tools / commands / entries).
    pub fn log(&self) -> RegistrationLog {
        self.inner.state.lock().log.clone()
    }

    /// Borrow the registered tools across every loaded extension.
    pub fn registered_tools(&self) -> Vec<ToolDefinition> {
        self.inner.state.lock().registry.tools().cloned().collect()
    }

    /// List every slash command registered via `pi.registerCommand`,
    /// across all loaded extensions, in registration order. The JS-side
    /// `_pi.commands` map is the source of truth so a command is listed
    /// exactly once even when several extensions register.
    pub async fn registered_commands(&self) -> Vec<RegisteredCommand> {
        let context = self.inner.context.clone();
        async_with!(context => |ctx| {
            let func: Function = ctx
                .globals()
                .get("_pi_registered_commands")
                .map_err(ExtensionError::from)?;
            let raw: String = func.call::<_, String>(()).map_err(ExtensionError::from)?;
            let parsed: serde_json::Value =
                serde_json::from_str(&raw).map_err(ExtensionError::from)?;
            let commands = parsed
                .get("commands")
                .cloned()
                .map(serde_json::from_value::<Vec<RegisteredCommand>>)
                .transpose()
                .map_err(ExtensionError::from)?
                .unwrap_or_default();
            Ok::<_, ExtensionError>(commands)
        })
        .await
        .unwrap_or_default()
    }

    /// Invoke a command registered via `pi.registerCommand`.
    ///
    /// `args` is the raw text the user typed after the command name —
    /// the upstream handler signature is `handler(args: string, ctx)`,
    /// so it is forwarded verbatim. `mode` / `has_ui` / `cwd` populate
    /// the `ctx` fields the handler reads.
    pub async fn execute_command(
        &self,
        name: &str,
        args: &str,
        mode: &str,
        has_ui: bool,
        cwd: &str,
    ) -> Result<CommandExecutionOutcome, ExtensionError> {
        let context = self.inner.context.clone();
        let timeout = self.inner.timeout;
        let name = name.to_string();
        let args = args.to_string();
        let ctx_json = serde_json::json!({
            "mode": mode,
            "hasUI": has_ui,
            "cwd": cwd,
        })
        .to_string();
        // Arm interrupt deadline.
        let deadline = Instant::now() + timeout;
        self.inner
            .deadline_nanos
            .store(system_time_nanos(deadline), Ordering::Relaxed);
        let result = tokio::time::timeout(
            timeout,
            async_with!(context => |ctx| {
                let exec: Function = ctx
                    .globals()
                    .get("_pi_execute_command")
                    .map_err(ExtensionError::from)?;
                let raw_promise: MaybePromise = exec
                    .call::<_, MaybePromise>((name, args, ctx_json))
                    .catch(&ctx)
                    .map_err(|e| e.throw(&ctx))?;
                let raw: String = raw_promise
                    .into_future()
                    .await
                    .map_err(ExtensionError::from)?;
                let outcome: CommandExecutionOutcome =
                    serde_json::from_str(&raw).map_err(ExtensionError::from)?;
                Ok::<_, ExtensionError>(outcome)
            }),
        )
        .await;
        // Disarm.
        self.inner.deadline_nanos.store(u64::MAX, Ordering::Relaxed);
        match result {
            Ok(Ok(outcome)) => Ok(outcome),
            Ok(Err(e)) => Err(ExtensionError::Runtime(e.to_string())),
            Err(_) => Err(ExtensionError::Timeout(timeout)),
        }
    }

    /// Take (and clear) the side effects accumulated since the last
    /// drain: appended entries, custom / user messages and the session
    /// name. Tool and command registrations are left intact.
    ///
    /// UI call traces (`ui_confirm`, `ui_input`, `ui_select`,
    /// `ui_notify`, `ui_answer_*`) are drained from the host log too but
    /// are not returned: they are diagnostics, already surfaced through
    /// the `UiHandler` / `pi_extension` tracing target, and persisting
    /// them into a session would add noise the upstream format does not
    /// have.
    pub fn drain_side_effects(&self) -> ExtensionSideEffects {
        let mut s = self.inner.state.lock();
        let entries: Vec<AppendedEntry> = std::mem::take(&mut s.log.entries)
            .into_iter()
            .filter(|entry| !entry.custom_type.starts_with("ui_"))
            .collect();
        ExtensionSideEffects {
            entries,
            messages: std::mem::take(&mut s.log.messages),
            user_messages: std::mem::take(&mut s.log.user_messages),
            session_name: s.log.session_name.take(),
        }
    }

    /// Borrow the registered extensions as a snapshot.
    pub fn registered_extensions(&self) -> Vec<ExtensionEntry> {
        self.inner
            .state
            .lock()
            .registry
            .extensions()
            .cloned()
            .collect()
    }

    /// Evaluate an extension source string and call its default
    /// export with the host-installed `pi` global.
    ///
    /// Two module contracts are accepted, matching upstream's
    /// `jiti.import(extensionPath, { default: true })`:
    ///
    /// * CommonJS — `module.exports = function (pi) { … }`, the shape
    ///   the upstream TypeScript source compiles to; and
    /// * ESM — `import { join } from "node:path"; export default
    ///   function (pi) { … }`, the upstream source form (rewritten in
    ///   `runtime/pi-ext-shim.mjs`, which maps the `node:path` /
    ///   `node:url` virtual modules and `import.meta`).
    ///
    /// `entry.source` is forwarded to the shim so `import.meta.url`
    /// resolves to the extension's own path and load errors name the
    /// file.
    ///
    /// `entry` is registered with the [`ExtensionRegistry`] under
    /// `entry.id` so [`JsExtensionHost::registered_tools`] can be
    /// cross-checked.
    pub async fn load(&self, entry: ExtensionEntry, source: &str) -> Result<(), ExtensionError> {
        let context = self.inner.context.clone();
        let state = self.inner.state.clone();
        let entry_clone = entry.clone();
        let source = source.to_string();
        let source_path = entry_clone.source.to_string_lossy().to_string();
        let timeout = self.inner.timeout;

        // Mark this extension as the one any host imports inside the
        // load call should be attributed to. Tools / commands that
        // arrive during the load are folded back into the registry
        // entry for `entry.id` once evaluation returns.
        {
            let mut s = state.lock();
            s.pending_extension = Some(entry.id.clone());
            s.registry
                .register(entry.clone(), ExtensionCapabilities::default());
        }

        // Arm the JS interrupt handler with the call deadline.
        let deadline = Instant::now() + timeout;
        let deadline_nanos_value = system_time_nanos(deadline);
        self.inner
            .deadline_nanos
            .store(deadline_nanos_value, Ordering::Relaxed);

        let result = tokio::time::timeout(
            timeout,
            async_with!(context => |ctx| {
                let load: Function = ctx
                    .globals()
                    .get("_pi_load_extension")
                    .map_err(ExtensionError::from)?;
                load.call::<_, ()>((source, source_path))
                    .catch(&ctx)
                    // Preserve the JS-side message (e.g. "ESM extension must
                    // `export default` …"): `CaughtError::throw` collapses it
                    // into rquickjs's generic "Exception generated by QuickJS".
                    .map_err(|e| ExtensionError::Runtime(e.to_string()))?;
                Ok::<_, ExtensionError>(())
            }),
        )
        .await;

        // Disarm the interrupt deadline regardless of outcome.
        self.inner.deadline_nanos.store(u64::MAX, Ordering::Relaxed);

        match result {
            Ok(Ok(())) => {
                let mut s = state.lock();
                s.pending_extension = None;
                // Fold the tools that landed during this load into this
                // extension's registry entry. Every other entry keeps
                // its tools: loading a second extension (a user-level
                // one next to a project-local one, say) must not wipe
                // the first extension's registrations.
                let tools = std::mem::take(&mut s.log.tools);
                s.registry.set_tools(&entry_clone.id, tools);
                Ok(())
            }
            Ok(Err(e)) => Err(ExtensionError::Load(format!("{}: {}", entry_clone.id, e))),
            Err(_) => Err(ExtensionError::Timeout(timeout)),
        }
    }

    /// Dispatch an [`ExtensionEvent`] to every subscribed handler in
    /// every loaded extension. Returns the dispatch summary the shim
    /// produced (number of handlers invoked, async marker, etc.).
    pub async fn emit_event(
        &self,
        event: &ExtensionEvent,
    ) -> Result<DispatchOutcome, ExtensionError> {
        self.emit_event_with(event, None, false, "").await
    }

    /// Dispatch an event with explicit context fields (mode, hasUI,
    /// cwd) supplied by the agent runtime.
    pub async fn emit_event_with(
        &self,
        event: &ExtensionEvent,
        mode: Option<&str>,
        has_ui: bool,
        cwd: &str,
    ) -> Result<DispatchOutcome, ExtensionError> {
        let mut envelope = serde_json::to_value(event).map_err(ExtensionError::from)?;
        if let Some(obj) = envelope.as_object_mut() {
            if let Some(m) = mode {
                obj.insert("_ctx_mode".into(), serde_json::Value::String(m.into()));
            }
            obj.insert("_ctx_hasUI".into(), serde_json::Value::Bool(has_ui));
            obj.insert("_ctx_cwd".into(), serde_json::Value::String(cwd.into()));
        }
        let event_json = serde_json::to_string(&envelope).map_err(ExtensionError::from)?;
        let context = self.inner.context.clone();
        let timeout = self.inner.timeout;
        // Arm interrupt deadline.
        let deadline = Instant::now() + timeout;
        self.inner
            .deadline_nanos
            .store(system_time_nanos(deadline), Ordering::Relaxed);
        let result = tokio::time::timeout(
            timeout,
            async_with!(context => |ctx| {
                let dispatch: Function = ctx
                    .globals()
                    .get("_pi_dispatch")
                    .map_err(ExtensionError::from)?;
                let raw_promise: MaybePromise = dispatch
                    .call::<_, MaybePromise>((event_json,))
                    .catch(&ctx)
                    .map_err(|e| e.throw(&ctx))?;
                let raw: String = raw_promise
                    .into_future()
                    .await
                    .map_err(ExtensionError::from)?;
                let outcome: DispatchOutcome = serde_json::from_str(&raw).map_err(ExtensionError::from)?;
                Ok::<_, ExtensionError>(outcome)
            }),
        )
        .await;
        // Disarm.
        self.inner.deadline_nanos.store(u64::MAX, Ordering::Relaxed);
        match result {
            Ok(Ok(outcome)) => Ok(outcome),
            Ok(Err(e)) => Err(ExtensionError::Runtime(e.to_string())),
            Err(_) => Err(ExtensionError::Timeout(timeout)),
        }
    }

    /// Execute a registered tool by name with the given arguments
    /// (as JSON). Returns the [`ToolExecutionOutcome`] the JS-side
    /// `execute` produced.
    pub async fn execute_tool(
        &self,
        name: &str,
        args_json: &str,
    ) -> Result<ToolExecutionOutcome, ExtensionError> {
        let context = self.inner.context.clone();
        let timeout = self.inner.timeout;
        let name = name.to_string();
        let args = args_json.to_string();
        // Arm interrupt deadline.
        let deadline = Instant::now() + timeout;
        self.inner
            .deadline_nanos
            .store(system_time_nanos(deadline), Ordering::Relaxed);
        let result = tokio::time::timeout(
            timeout,
            async_with!(context => |ctx| {
                let exec: Function = ctx
                    .globals()
                    .get("_pi_execute_tool")
                    .map_err(ExtensionError::from)?;
                let raw_promise: MaybePromise = exec
                    .call::<_, MaybePromise>((name, args))
                    .catch(&ctx)
                    .map_err(|e| e.throw(&ctx))?;
                let raw: String = raw_promise
                    .into_future()
                    .await
                    .map_err(ExtensionError::from)?;
                let outcome: ToolExecutionOutcome =
                    serde_json::from_str(&raw).map_err(ExtensionError::from)?;
                Ok::<_, ExtensionError>(outcome)
            }),
        )
        .await;
        // Disarm.
        self.inner.deadline_nanos.store(u64::MAX, Ordering::Relaxed);
        match result {
            Ok(Ok(outcome)) => Ok(outcome),
            Ok(Err(e)) => Err(ExtensionError::Runtime(e.to_string())),
            Err(_) => Err(ExtensionError::Timeout(timeout)),
        }
    }

    /// List the names of every tool registered so far.
    pub async fn registered_tool_names(&self) -> Vec<String> {
        let context = self.inner.context.clone();
        async_with!(context => |ctx| {
            let func: Function = ctx
                .globals()
                .get("_pi_registered_tools")
                .map_err(ExtensionError::from)?;
            let raw: String = func.call::<_, String>(()).map_err(ExtensionError::from)?;
            let parsed: serde_json::Value = serde_json::from_str(&raw).map_err(ExtensionError::from)?;
            let names = parsed
                .get("tools")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(str::to_owned))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            Ok::<_, ExtensionError>(names)
        })
        .await
        .unwrap_or_default()
    }

    /// List the prompt contributions of every tool registered via
    /// `pi.registerTool`, in registration order. Used to describe
    /// extension tools in the system prompt.
    pub async fn registered_tool_prompts(&self) -> Vec<RegisteredToolPrompt> {
        let context = self.inner.context.clone();
        async_with!(context => |ctx| {
            let func: Function = ctx
                .globals()
                .get("_pi_registered_tool_prompts")
                .map_err(ExtensionError::from)?;
            let raw: String = func.call::<_, String>(()).map_err(ExtensionError::from)?;
            let parsed: serde_json::Value =
                serde_json::from_str(&raw).map_err(ExtensionError::from)?;
            let tools = parsed
                .get("tools")
                .cloned()
                .map(serde_json::from_value::<Vec<RegisteredToolPrompt>>)
                .transpose()
                .map_err(ExtensionError::from)?
                .unwrap_or_default();
            Ok::<_, ExtensionError>(tools)
        })
        .await
        .unwrap_or_default()
    }

    /// List the event names with at least one registered handler.
    pub async fn known_event_names(&self) -> Vec<String> {
        let context = self.inner.context.clone();
        async_with!(context => |ctx| {
            let func: Function = ctx
                .globals()
                .get("_pi_known_event_names")
                .map_err(ExtensionError::from)?;
            let raw: String = func.call::<_, String>(()).map_err(ExtensionError::from)?;
            let parsed: serde_json::Value = serde_json::from_str(&raw).map_err(ExtensionError::from)?;
            let names = parsed
                .get("events")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(str::to_owned))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            Ok::<_, ExtensionError>(names)
        })
        .await
        .unwrap_or_default()
    }
}

/// Summary of a `_pi_dispatch` call as returned by the shim.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DispatchOutcome {
    /// Whether the extension had any handler for this event.
    pub handled: bool,
    /// Number of handlers that ran (sync portion).
    #[serde(default)]
    pub subscribers: usize,
    /// Synchronous results the handlers returned, in registration
    /// order. Handlers that returned a thenable show up as
    /// `{"async": true}`.
    #[serde(default)]
    pub results: Vec<serde_json::Value>,
    /// First handler-level error the shim caught, if any.
    #[serde(default)]
    pub errored: Option<DispatchError>,
}

/// Failure surfaced by a JS-side handler.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DispatchError {
    /// Error message raised by the JS handler (stringified).
    #[serde(default)]
    pub message: String,
}

/// Extra resource paths an extension advertised from a
/// `resources_discover` handler.
///
/// Rust port of the `ResourcesDiscoverResult` the upstream
/// `ExtensionRunner.emitResourcesDiscover` collects. Relative paths are
/// resolved against the session `cwd` by the loader that consumes them,
/// matching upstream's `resolveResourcePath`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiscoveredResources {
    /// Skill files / directories to load in addition to the defaults.
    pub skill_paths: Vec<PathBuf>,
    /// Prompt template files / directories to load in addition.
    pub prompt_paths: Vec<PathBuf>,
    /// Theme files. Collected for parity with upstream; the Rust TUI has
    /// no theme system yet, so nothing consumes them.
    pub theme_paths: Vec<PathBuf>,
}

impl DiscoveredResources {
    /// True when no extension contributed anything: the caller can skip
    /// re-deriving its resource bundle entirely.
    pub fn is_empty(&self) -> bool {
        self.skill_paths.is_empty() && self.prompt_paths.is_empty() && self.theme_paths.is_empty()
    }

    /// Fold one extension's JSON result into the accumulator.
    ///
    /// A handler returns `{ skillPaths?, promptPaths?, themePaths? }`;
    /// anything else (a bare string, `null`, a number) is ignored
    /// instead of failing the whole discovery pass, because a single
    /// misbehaving extension must not take away the paths its peers
    /// returned.
    pub fn absorb_value(&mut self, value: &serde_json::Value) {
        let Some(object) = value.as_object() else {
            return;
        };
        collect_paths(object.get("skillPaths"), &mut self.skill_paths);
        collect_paths(object.get("promptPaths"), &mut self.prompt_paths);
        collect_paths(object.get("themePaths"), &mut self.theme_paths);
    }

    /// Build the aggregate from a `_pi_dispatch` summary, dropping the
    /// duplicate paths two handlers may both advertise.
    pub fn from_dispatch(outcome: &DispatchOutcome) -> Self {
        let mut discovered = Self::default();
        for result in &outcome.results {
            discovered.absorb_value(result);
        }
        discovered.dedup();
        discovered
    }

    /// Remove duplicate paths while keeping first-seen order (upstream
    /// `mergePaths`).
    pub fn dedup(&mut self) {
        dedup_paths(&mut self.skill_paths);
        dedup_paths(&mut self.prompt_paths);
        dedup_paths(&mut self.theme_paths);
    }
}

fn collect_paths(value: Option<&serde_json::Value>, out: &mut Vec<PathBuf>) {
    let Some(entries) = value.and_then(serde_json::Value::as_array) else {
        return;
    };
    for entry in entries {
        // Upstream entries are plain strings. Accept `{ "path": "..." }`
        // too: that is the shape the runner wraps them into internally, so
        // an extension that echoes it back still works.
        let raw = match entry {
            serde_json::Value::String(path) => Some(path.as_str()),
            serde_json::Value::Object(object) => object.get("path").and_then(|p| p.as_str()),
            _ => None,
        };
        if let Some(path) = raw {
            let trimmed = path.trim();
            if !trimmed.is_empty() {
                out.push(PathBuf::from(trimmed));
            }
        }
    }
}

fn dedup_paths(paths: &mut Vec<PathBuf>) {
    let mut seen: Vec<PathBuf> = Vec::with_capacity(paths.len());
    paths.retain(|path| {
        if seen.contains(path) {
            return false;
        }
        seen.push(path.clone());
        true
    });
}

/// Result of invoking a registered tool's JS execute function.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ToolExecutionOutcome {
    /// Whether the tool surfaced an error result.
    #[serde(default)]
    pub is_error: bool,
    /// Content blocks produced by the tool. Each entry must round-trip
    /// through [`pi_protocol::Content`].
    #[serde(default)]
    pub content: Vec<serde_json::Value>,
    /// Optional structured details surfaced by the tool.
    #[serde(default)]
    pub details: Option<serde_json::Value>,
}

// ---------------------------------------------------------------------------
// Host import bodies.
//
// These are written as plain async fns so the closure wrapping in
// `install_imports` doesn't have to spell out the Future return type
// each time — `Func::from(|a, b| async_fn(a, b, ...))` works because the
// closure's return type is the Future returned by `async_fn`, satisfying
// `Async<F>: IntoJsFunc`.
// ---------------------------------------------------------------------------

async fn host_confirm_impl(
    title: String,
    body: String,
    ui_tx: mpsc::UnboundedSender<UiRequestEnvelope>,
    state: Arc<Mutex<HostState>>,
) -> rquickjs_core::Result<bool> {
    let (tx, rx) = oneshot::channel();
    let _ = ui_tx.send(UiRequestEnvelope {
        request: UiRequest::Confirm {
            title: title.clone(),
            body,
        },
        reply: tx,
    });
    tracing::debug!(target: "pi_extension", "waiting on confirm UI reply");
    let answer = rx
        .await
        .ok()
        .flatten()
        .and_then(|resp| match resp {
            UiResponse::Confirm { accepted } => Some(accepted),
            _ => None,
        })
        .unwrap_or(false);
    tracing::debug!(target: "pi_extension", "got confirm UI reply: {}", answer);
    state.lock().log.entries.push(AppendedEntry {
        custom_type: "ui_confirm".into(),
        data: serde_json::json!({"title": title, "accepted": answer}),
    });
    Ok(answer)
}

async fn host_input_impl(
    title: String,
    placeholder: String,
    ui_tx: mpsc::UnboundedSender<UiRequestEnvelope>,
    state: Arc<Mutex<HostState>>,
) -> rquickjs_core::Result<Option<String>> {
    let placeholder_opt = if placeholder.is_empty() {
        None
    } else {
        Some(placeholder)
    };
    let (tx, rx) = oneshot::channel();
    let _ = ui_tx.send(UiRequestEnvelope {
        request: UiRequest::Input {
            title: title.clone(),
            placeholder: placeholder_opt,
        },
        reply: tx,
    });
    let value: Option<String> = rx.await.ok().flatten().and_then(|resp| match resp {
        UiResponse::Input { value } => Some(value),
        _ => None,
    });
    state.lock().log.entries.push(AppendedEntry {
        custom_type: "ui_input".into(),
        data: serde_json::json!({"title": title, "value": value}),
    });
    Ok(value)
}

async fn host_select_impl(
    title: String,
    options_json: String,
    ui_tx: mpsc::UnboundedSender<UiRequestEnvelope>,
    state: Arc<Mutex<HostState>>,
) -> rquickjs_core::Result<Option<String>> {
    let options: Vec<String> = serde_json::from_str(&options_json).unwrap_or_default();
    let (tx, rx) = oneshot::channel();
    let _ = ui_tx.send(UiRequestEnvelope {
        request: UiRequest::Select {
            title: title.clone(),
            options: options.clone(),
        },
        reply: tx,
    });
    let value: Option<String> = rx.await.ok().flatten().and_then(|resp| match resp {
        UiResponse::Select { value } => Some(value),
        _ => None,
    });
    state.lock().log.entries.push(AppendedEntry {
        custom_type: "ui_select".into(),
        data: serde_json::json!({"title": title, "options": options, "value": value}),
    });
    Ok(value)
}

/// Install the host imports the shim expects. Each function marshals
/// its arguments as JSON, hands them to the host state, and returns a
/// QuickJS-friendly value (string / Promise).
fn install_imports(ctx: &Ctx<'_>, inner: &Arc<Inner>) -> rquickjs_core::Result<()> {
    let globals = ctx.globals();

    // Context tool executions see. Set once here because the host's
    // mode / hasUI / cwd never change mid-session; the shim reads it in
    // `_pi_execute_tool` instead of the host threading it through every
    // `execute_tool` call.
    let tool_ctx_json = serde_json::json!({
        "mode": inner.tool_context.mode,
        "hasUI": inner.tool_context.has_ui,
        "cwd": inner.tool_context.cwd,
    })
    .to_string();
    globals.set("_pi_tool_ctx", tool_ctx_json)?;

    // host_register_tool(json)
    let state_for_tool = inner.state.clone();
    let tool_fn = Func::from(move |json: String| -> rquickjs_core::Result<()> {
        let def: ToolDefinition = serde_json::from_str(&json).map_err(|e| {
            rquickjs_core::Error::new_from_js_message("register_tool", "protocol", e.to_string())
        })?;
        state_for_tool.lock().log.tools.push(def);
        Ok(())
    });
    globals.set("host_register_tool", tool_fn)?;

    // host_register_command(json)
    let state_for_cmd = inner.state.clone();
    let cmd_fn = Func::from(move |json: String| -> rquickjs_core::Result<()> {
        let cmd: RegisteredCommand = serde_json::from_str(&json).map_err(|e| {
            rquickjs_core::Error::new_from_js_message("register_command", "protocol", e.to_string())
        })?;
        state_for_cmd.lock().log.commands.push(cmd);
        Ok(())
    });
    globals.set("host_register_command", cmd_fn)?;

    // host_append_entry(customType, dataJson)
    let state_for_entry = inner.state.clone();
    let entry_fn = Func::from(
        move |custom_type: String, data_json: String| -> rquickjs_core::Result<()> {
            let data: serde_json::Value =
                serde_json::from_str(&data_json).unwrap_or(serde_json::Value::Null);
            state_for_entry
                .lock()
                .log
                .entries
                .push(AppendedEntry { custom_type, data });
            Ok(())
        },
    );
    globals.set("host_append_entry", entry_fn)?;

    // host_send_message(json)
    let state_for_msg = inner.state.clone();
    let msg_fn = Func::from(move |json: String| -> rquickjs_core::Result<()> {
        let value: serde_json::Value =
            serde_json::from_str(&json).unwrap_or(serde_json::Value::Null);
        state_for_msg.lock().log.messages.push(value);
        Ok(())
    });
    globals.set("host_send_message", msg_fn)?;

    // host_send_user_message(json)
    let state_for_user = inner.state.clone();
    let user_msg_fn = Func::from(move |json: String| -> rquickjs_core::Result<()> {
        let value: serde_json::Value =
            serde_json::from_str(&json).unwrap_or(serde_json::Value::Null);
        let text = match value {
            serde_json::Value::String(s) => s,
            other => other.to_string(),
        };
        state_for_user.lock().log.user_messages.push(text);
        Ok(())
    });
    globals.set("host_send_user_message", user_msg_fn)?;

    // host_set_session_name(name)
    let state_for_name = inner.state.clone();
    let name_fn = Func::from(move |name: String| -> rquickjs_core::Result<()> {
        state_for_name.lock().log.session_name = Some(name);
        Ok(())
    });
    globals.set("host_set_session_name", name_fn)?;

    // host_ui_notify(message, level) — sync, fire-and-forget.
    let state_for_notify = inner.state.clone();
    let ui_tx_for_notify = inner.ui_tx.clone();
    let notify_fn = Func::from(
        move |message: String, level: String| -> rquickjs_core::Result<()> {
            let ui_level = match level.as_str() {
                "success" => UiLevel::Success,
                "warning" => UiLevel::Warning,
                "error" => UiLevel::Error,
                _ => UiLevel::Info,
            };
            let (tx, _rx) = oneshot::channel();
            let _ = ui_tx_for_notify.send(UiRequestEnvelope {
                request: UiRequest::Notify {
                    message: message.clone(),
                    level: ui_level,
                },
                reply: tx,
            });
            state_for_notify.lock().log.entries.push(AppendedEntry {
                custom_type: "ui_notify".into(),
                data: serde_json::json!({"message": message, "level": level}),
            });
            Ok(())
        },
    );
    globals.set("host_ui_notify", notify_fn)?;

    // host_log(level, message)
    let log_fn = Func::from(
        move |level: String, message: String| -> rquickjs_core::Result<()> {
            match level.as_str() {
                "error" => tracing::error!(target: "pi_extension", "{}", message),
                "warn" => tracing::warn!(target: "pi_extension", "{}", message),
                "debug" => tracing::debug!(target: "pi_extension", "{}", message),
                "trace" => tracing::trace!(target: "pi_extension", "{}", message),
                _ => tracing::info!(target: "pi_extension", "{}", message),
            }
            Ok(())
        },
    );
    globals.set("host_log", log_fn)?;

    // Async host imports below return a JS Promise. Each one sends a
    // UiRequest on the channel and awaits the response oneshot. The
    // helper `async fn`s defined above take the channel + state by
    // value; the closures here bind them once at install time so each
    // JS call constructs a fresh Future.
    let ui_tx = inner.ui_tx.clone();
    let state_for_async = inner.state.clone();
    let confirm_fn = move |title: String, body: String| {
        host_confirm_impl(title, body, ui_tx.clone(), state_for_async.clone())
    };
    let confirm_function = Function::new(ctx.clone(), Async(confirm_fn))?;
    globals.set("host_ui_confirm", confirm_function)?;

    let ui_tx = inner.ui_tx.clone();
    let state_for_async = inner.state.clone();
    let input_fn = move |title: String, placeholder: String| {
        host_input_impl(title, placeholder, ui_tx.clone(), state_for_async.clone())
    };
    let input_function = Function::new(ctx.clone(), Async(input_fn))?;
    globals.set("host_ui_input", input_function)?;

    let ui_tx = inner.ui_tx.clone();
    let state_for_async = inner.state.clone();
    let select_fn = move |title: String, options_json: String| {
        host_select_impl(title, options_json, ui_tx.clone(), state_for_async.clone())
    };
    let select_function = Function::new(ctx.clone(), Async(select_fn))?;
    globals.set("host_ui_select", select_function)?;

    Ok(())
}

/// Convert an [`Instant`] to a wall-clock nanos-since-epoch value
/// matching what [`SystemTime`] would produce. Used by the JS
/// interrupt handler so a deadline set from `Instant::now() + Duration`
/// can be compared against `SystemTime::now()`.
fn system_time_nanos(target: Instant) -> u64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let delta = target.saturating_duration_since(Instant::now()).as_nanos() as u64;
    now.saturating_add(delta)
}

/// Drain UI requests from JS extensions, dispatch them through the
/// host-supplied [`UiHandler`], and reply on the oneshot the JS side
/// is awaiting.
async fn ui_worker(
    mut rx: mpsc::UnboundedReceiver<UiRequestEnvelope>,
    handler: Option<Arc<dyn UiHandler>>,
    state: Arc<Mutex<HostState>>,
) {
    while let Some(env) = rx.recv().await {
        let request = env.request.clone();
        let Some(handler) = handler.clone() else {
            // No handler installed — Notify acks, others resolve None.
            let response = match request {
                UiRequest::Notify { .. } => Some(UiResponse::NotifyAck),
                _ => None,
            };
            let _ = env.reply.send(response);
            continue;
        };
        let response = match &request {
            UiRequest::Notify { message, level } => {
                handler.notify(message, *level).await;
                Some(UiResponse::NotifyAck)
            }
            UiRequest::Confirm { title, body } => {
                let accepted = handler.confirm(title, body).await;
                Some(UiResponse::Confirm { accepted })
            }
            UiRequest::Input { title, placeholder } => {
                let value = handler.input(title, placeholder.as_deref()).await;
                value.map(|v| UiResponse::Input { value: v })
            }
            UiRequest::Select { title, options } => {
                let value = handler.select(title, options).await;
                value.map(|v| UiResponse::Select { value: v })
            }
        };
        // Record the UI answer in the log so tests can introspect.
        if let Some(ref resp) = response {
            let entry = match (request, resp.clone()) {
                (UiRequest::Confirm { title, .. }, UiResponse::Confirm { accepted }) => {
                    AppendedEntry {
                        custom_type: "ui_answer_confirm".into(),
                        data: serde_json::json!({"title": title, "accepted": accepted}),
                    }
                }
                (UiRequest::Input { title, .. }, UiResponse::Input { value }) => AppendedEntry {
                    custom_type: "ui_answer_input".into(),
                    data: serde_json::json!({"title": title, "value": value}),
                },
                (UiRequest::Select { title, .. }, UiResponse::Select { value }) => AppendedEntry {
                    custom_type: "ui_answer_select".into(),
                    data: serde_json::json!({"title": title, "value": value}),
                },
                _ => AppendedEntry {
                    custom_type: "ui_answer_notify".into(),
                    data: serde_json::Value::Null,
                },
            };
            state.lock().log.entries.push(entry);
        }
        let _ = env.reply.send(response);
    }
}
