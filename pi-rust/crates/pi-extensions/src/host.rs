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

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::io::Write as _;
use std::path::PathBuf;
use std::pin::Pin;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
use tokio::io::AsyncReadExt;
use tokio::sync::{mpsc, oneshot, watch, Notify};

use crate::api::{ExtensionCapabilities, ExtensionEntry};
use crate::deflate::{self, crc32};
use crate::error::ExtensionError;
use crate::registry::ExtensionRegistry;
use crate::shim::SHIM_SOURCE;

/// Default timeout applied to every host import and event dispatch.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

/// Extra time the host grants an explicit `pi.exec` `options.timeout`
/// before its own per-call deadline fires. The child kills itself at
/// `options.timeout`; the host deadline is raised past that so the
/// extension still receives the `killed: true` result instead of a
/// host-level `ExtensionError::Timeout`.
pub const EXEC_TIMEOUT_GRACE: Duration = Duration::from_secs(1);

/// Upper bound on a single `pi.exec` `options.timeout`. A larger value is
/// clamped so `Instant + timeout` can never overflow (the JS side may pass
/// any number).
const MAX_EXEC_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);

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
    /// Optional runner behind the JS `create*Tool` factories.
    ///
    /// `pi-extensions` cannot depend on the crate that owns the
    /// concrete built-in bundle, so the host is injected here and the
    /// shim reaches it through the `host_builtin_tool_definition` (sync)
    /// and `host_builtin_tool` (async) imports. Without it the factories
    /// still import and return objects, but their `execute` rejects with
    /// a clear error.
    pub builtin_tool_runner: Option<Arc<dyn BuiltinToolRunner>>,
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
            .field(
                "builtin_tool_runner",
                &self
                    .builtin_tool_runner
                    .as_ref()
                    .map(|_| "<dyn BuiltinToolRunner>"),
            )
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
    /// Install the runner behind the built-in `create*Tool` factories.
    pub fn with_builtin_tool_runner(mut self, runner: Arc<dyn BuiltinToolRunner>) -> Self {
        self.builtin_tool_runner = Some(runner);
        self
    }
}

/// Outcome of running one built-in tool through [`BuiltinToolRunner`].
///
/// Mirrors [`ToolExecutionOutcome`] on the wire: `content` is a list of
/// serialized [`pi_protocol::Content`] blocks, `is_error` the error flag
/// and `details` the opaque structured payload. `content` is a block list
/// rather than a single `String` so an image returned by the built-in
/// `read` tool survives the bridge instead of being flattened to text.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuiltinToolOutcome {
    /// Content blocks, each a serialized [`pi_protocol::Content`].
    #[serde(default)]
    pub content: Vec<serde_json::Value>,
    /// Whether the tool surfaced an error result.
    #[serde(default)]
    pub is_error: bool,
    /// Optional structured details surfaced by the tool.
    #[serde(default)]
    pub details: Option<serde_json::Value>,
}

/// Definition metadata for one built-in tool.
///
/// [`BuiltinToolRunner::definition`] returns it and the shim hands
/// `parameters` to the JS factory, so the schema an extension sees is
/// the exact one the host executor coerces arguments against.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuiltinToolDefinition {
    /// Tool name (`read`, `bash`, …).
    pub name: String,
    /// Human-readable label.
    pub label: String,
    /// Model-facing description.
    pub description: String,
    /// JSON Schema for the tool's argument object.
    pub parameters: serde_json::Value,
}

/// Host-side implementation of the built-in tools the JS `create*Tool`
/// factories wrap.
///
/// `pi-extensions` cannot depend on `pi-coding-agent` (that would be a
/// cycle), so the concrete built-in bundle is injected through
/// [`HostOptions::builtin_tool_runner`]. Two host imports reach it:
/// [`BuiltinToolRunner::definition`] backs the *synchronous*
/// `host_builtin_tool_definition` import, because a factory has to hand
/// out `parameters` the moment `createReadTool(cwd)` is called, and
/// [`BuiltinToolRunner::run`] backs the async `host_builtin_tool` import
/// the tool's `execute` awaits.
pub trait BuiltinToolRunner: Send + Sync + 'static {
    /// Definition for a built-in tool, or `None` when the runner does not
    /// know the name. `None` makes the JS factory fall back to a
    /// permissive `{ "type": "object" }` schema, and its `execute`
    /// reject with "not available".
    fn definition(&self, name: &str) -> Option<BuiltinToolDefinition>;

    /// Run one built-in tool.
    ///
    /// `cwd` is the directory the factory was created with
    /// (`createReadTool(cwd)`), or `None` when the extension passed no
    /// argument. An [`Err`] is a *host-level* failure — the runner is
    /// unusable or the name is unknown — and the shim turns it into a
    /// rejected promise. A tool that merely failed returns
    /// `Ok(BuiltinToolOutcome { is_error: true, .. })`, which the shim
    /// passes through as a structured tool result.
    fn run<'a>(
        &'a self,
        name: String,
        args: serde_json::Value,
        cwd: Option<String>,
    ) -> Pin<Box<dyn Future<Output = Result<BuiltinToolOutcome, String>> + Send + 'a>>;
}

/// Embedded QuickJS host. Cloning shares the underlying runtime +
/// context; both must be driven from a tokio runtime.
#[derive(Clone)]
pub struct JsExtensionHost {
    inner: Arc<Inner>,
}

/// Live `node:child_process` children, keyed by the shim-visible handle.
type Children = Arc<Mutex<HashMap<u64, Arc<ChildEntry>>>>;

struct Inner {
    /// Kept alive so the [`AsyncContext`](rquickjs_core::AsyncContext)
    /// below remains valid; the context holds a clone of the runtime
    /// but dropping the original would invalidate it.
    #[allow(dead_code)]
    runtime: rquickjs_core::AsyncRuntime,
    /// Live `node:child_process` children, keyed by the handle the shim
    /// passes back. [`Drop`] kills whatever is left so a detached child
    /// can never outlive the host that started it.
    children: Children,
    /// Allocates the `node:child_process` handles above.
    next_child: Arc<AtomicU64>,
    context: rquickjs_core::AsyncContext,
    ui_tx: mpsc::UnboundedSender<UiRequestEnvelope>,
    state: Arc<Mutex<HostState>>,
    timeout: Duration,
    /// Context handed to tool executions (see [`ToolContext`]).
    tool_context: ToolContext,
    /// Runner behind the JS `create*Tool` factories (see
    /// [`BuiltinToolRunner`]).
    builtin_tool_runner: Option<Arc<dyn BuiltinToolRunner>>,
    /// Wall-clock nanos deadline the JS interrupt handler checks on
    /// every iteration. `u64::MAX` means "no deadline active".
    deadline_nanos: Arc<AtomicU64>,
    /// In-flight `pi.exec` calls: the cancel channel and the deadline
    /// extension (see [`ExecBridge`]).
    execs: ExecBridge,
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

impl Drop for Inner {
    fn drop(&mut self) {
        // Ask every surviving child's wait task to kill it. The process
        // is also protected by `kill_on_drop`, so a runtime shutdown that
        // aborts the wait task still reaps it.
        let children: Vec<Arc<ChildEntry>> = self.children.lock().values().cloned().collect();
        for child in children {
            child.killed.store(true, Ordering::Relaxed);
            if let Some(kill) = child.kill_tx.lock().take() {
                let _ = kill.send(());
            }
        }
        // `pi.exec` children are owned by [`run_child`]'s future rather
        // than by a dedicated wait task, so ask every surviving call to
        // kill its child too.
        self.execs.cancel_all();
    }
}

impl Inner {
    /// Arm the JS interrupt deadline for one host call and return it, so
    /// the interrupt handler and [`drive_call`] agree on the base.
    fn arm_deadline(&self) -> Instant {
        let deadline = Instant::now() + self.timeout;
        self.deadline_nanos
            .store(system_time_nanos(deadline), Ordering::Relaxed);
        deadline
    }

    /// Clear the per-call deadline — both the base and any `pi.exec`
    /// extension armed during the call — once the host call returns.
    fn disarm_deadline(&self) {
        self.deadline_nanos.store(u64::MAX, Ordering::Relaxed);
        self.execs.reset_deadline();
    }
}

/// Drive one host call to completion under the host deadline.
///
/// This is the previous `tokio::time::timeout(inner.timeout, future)` with
/// one addition: while an in-flight `pi.exec` that passed an explicit
/// `options.timeout` is registered, the deadline is raised to that
/// `options.timeout + [`EXEC_TIMEOUT_GRACE`]` so the child's own timeout
/// decides the outcome — the host must not cut a call short before the
/// deadline the extension asked for.
///
/// `Err(())` means the deadline fired; the future is dropped (as
/// `tokio::time::timeout` did) and every exec still in flight is killed
/// first, because a `host_exec` future lives on in the QuickJS async pool
/// after the JS call that awaited it is dropped.
async fn drive_call<F>(inner: &Inner, base_deadline: Instant, future: F) -> Result<F::Output, ()>
where
    F: std::future::Future,
{
    tokio::pin!(future);
    loop {
        let deadline = match inner.execs.deadline() {
            Some(exec) if exec > base_deadline => exec,
            _ => base_deadline,
        };
        if Instant::now() >= deadline {
            inner.execs.cancel_all();
            return Err(());
        }
        tokio::select! {
            output = &mut future => return Ok(output),
            () = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => {}
        }
    }
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
        let execs = ExecBridge::new();
        let inner = Arc::new(Inner {
            runtime: runtime.clone(),
            children: Arc::new(Mutex::new(HashMap::new())),
            next_child: Arc::new(AtomicU64::new(1)),
            context: context.clone(),
            ui_tx,
            state,
            timeout,
            tool_context: opts.tool_context.clone(),
            builtin_tool_runner: opts.builtin_tool_runner.clone(),
            deadline_nanos: deadline_nanos.clone(),
            execs,
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
        let interrupt_exec_deadline = inner.execs.deadline_nanos.clone();
        runtime
            .set_interrupt_handler(Some(Box::new(move || {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_nanos() as u64)
                    .unwrap_or(0);
                let base = interrupt_deadline.load(Ordering::Relaxed);
                let exec = interrupt_exec_deadline.load(Ordering::Relaxed);
                // `u64::MAX` means "unarmed": take whichever deadline *is*
                // armed, and the later one when both are.
                let deadline = match (base, exec) {
                    (u64::MAX, u64::MAX) => return false,
                    (u64::MAX, exec) => exec,
                    (base, u64::MAX) => base,
                    (base, exec) => base.max(exec),
                };
                now >= deadline
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
        // Arm the interrupt deadline, then drive the whole call under it.
        // `drive_call` extends the deadline while a `pi.exec` with an
        // explicit `options.timeout` is in flight.
        let base_deadline = self.inner.arm_deadline();
        let result = drive_call(
            &self.inner,
            base_deadline,
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
        self.inner.disarm_deadline();
        match result {
            Ok(Ok(outcome)) => Ok(outcome),
            Ok(Err(e)) => Err(ExtensionError::Runtime(e.to_string())),
            Err(()) => Err(ExtensionError::Timeout(timeout)),
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

        // Arm the JS interrupt handler with the call deadline, then drive
        // the load under it (`drive_call` extends it for an in-flight
        // `pi.exec` with an explicit `options.timeout`).
        let base_deadline = self.inner.arm_deadline();

        let result = drive_call(
            &self.inner,
            base_deadline,
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

        self.inner.disarm_deadline();

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
            Err(()) => Err(ExtensionError::Timeout(timeout)),
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
        // Arm the interrupt deadline, then drive the dispatch under it
        // (`drive_call` extends it for an in-flight `pi.exec` with an
        // explicit `options.timeout`).
        let base_deadline = self.inner.arm_deadline();
        let result = drive_call(
            &self.inner,
            base_deadline,
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
        self.inner.disarm_deadline();
        match result {
            Ok(Ok(outcome)) => Ok(outcome),
            Ok(Err(e)) => Err(ExtensionError::Runtime(e.to_string())),
            Err(()) => Err(ExtensionError::Timeout(timeout)),
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
        // Arm the interrupt deadline, then drive the tool under it
        // (`drive_call` extends it for an in-flight `pi.exec` with an
        // explicit `options.timeout`).
        let base_deadline = self.inner.arm_deadline();
        let result = drive_call(
            &self.inner,
            base_deadline,
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
        self.inner.disarm_deadline();
        match result {
            Ok(Ok(outcome)) => Ok(outcome),
            Ok(Err(e)) => Err(ExtensionError::Runtime(e.to_string())),
            Err(()) => Err(ExtensionError::Timeout(timeout)),
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

/// `pi.exec(command, args, options)` — request decoded from the JS shim.
///
/// Mirrors upstream `ExecOptions` (`packages/coding-agent/src/core/exec.ts`).
/// `signal` cannot cross the host ABI (QuickJS has no `AbortSignal`, and a
/// signal is not JSON), so cancellation travels out of band: the shim
/// allocates `id`, passes it here, and calls `host_exec_cancel(id)` when
/// the extension's signal fires. `cwd` is filled in by the shim with the
/// session cwd when the extension omits it.
#[derive(Debug, Default, Deserialize)]
struct ExecRequest {
    /// Call id allocated by the shim; keys [`ExecBridge`] for cancellation
    /// and the deadline extension. `None` disables both.
    #[serde(default)]
    id: Option<u64>,
    /// Arguments passed to the child process, never through a shell.
    #[serde(default)]
    args: Vec<String>,
    /// Working directory; the session cwd when the extension omitted it.
    #[serde(default)]
    cwd: Option<String>,
    /// Kill the child after this many milliseconds (`None` = no limit).
    #[serde(default)]
    timeout: Option<u64>,
}

/// Result handed back to JS — the upstream `ExecResult` shape.
#[derive(Debug, Serialize)]
struct ExecOutcome {
    stdout: String,
    stderr: String,
    code: i32,
    killed: bool,
}

/// Registry of in-flight `pi.exec` / `fetch` calls, keyed by the id the shim
/// allocates (one monotonic counter serves both). Two behaviours ride on it:
///
/// * **cancellation** — `host_exec_cancel(id)` / `host_fetch_cancel(id)` (the
///   shim calls them when the extension's `AbortSignal` fires) marks the slot;
///   [`run_child`]'s wait loop sees the mark, `start_kill`s the child and reaps
///   it, and [`run_fetch`] sees it and drops the in-flight request; and
/// * **the timeout extension** — an explicit `options.timeout` / `timeout` is
///   a promise the host keeps, so while such a call is in flight the host
///   per-call deadline is raised to `timeout + [`EXEC_TIMEOUT_GRACE`]`, and the
///   call's own timeout decides the outcome (see [`drive_call`]).
#[derive(Clone)]
struct ExecBridge {
    state: Arc<Mutex<ExecState>>,
    /// Wall-clock nanos of the furthest explicit exec deadline, in the same
    /// encoding as [`Inner::deadline_nanos`] (`u64::MAX` = unarmed). The JS
    /// interrupt handler takes the later of the two.
    deadline_nanos: Arc<AtomicU64>,
}

#[derive(Default)]
struct ExecState {
    /// Live calls, keyed by the shim-allocated id.
    live: HashMap<u64, Arc<ExecSlot>>,
    /// Ids cancelled before their call registered. An `async` host import
    /// creates its future when JS calls it but is not polled until the JS
    /// job yields, so an extension that aborts synchronously after
    /// `pi.exec(...)` can beat the first poll; the cancel is remembered and
    /// applied on registration.
    cancelled: HashSet<u64>,
    /// Furthest explicit `options.timeout` (+ grace) armed by a live call,
    /// kept as an [`Instant`] for the host call deadline.
    deadline: Option<Instant>,
}

/// One in-flight `pi.exec` call.
struct ExecSlot {
    /// Set by [`ExecBridge::cancel`] (or by a pre-registration cancel).
    cancel: AtomicBool,
    /// Wakes [`run_child`]'s wait loop the moment a cancel lands.
    notify: Notify,
    /// Host deadline this call asks for: `spawn + options.timeout +`
    /// [`EXEC_TIMEOUT_GRACE`]. `None` when the extension passed no
    /// `options.timeout` — the host deadline then applies unchanged.
    hard_deadline: Option<Instant>,
}

impl ExecBridge {
    fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(ExecState::default())),
            deadline_nanos: Arc::new(AtomicU64::new(u64::MAX)),
        }
    }

    /// Register a live call. Returns `true` when the shim had already
    /// cancelled its id (the abort beat the host future's first poll).
    fn register(&self, id: u64, slot: &Arc<ExecSlot>) -> bool {
        let mut state = self.state.lock();
        let cancelled = state.cancelled.remove(&id);
        if cancelled {
            slot.cancel.store(true, Ordering::Relaxed);
        }
        state.live.insert(id, slot.clone());
        if let Some(deadline) = slot.hard_deadline {
            let newer = match state.deadline {
                Some(current) => deadline > current,
                None => true,
            };
            if newer {
                state.deadline = Some(deadline);
                self.deadline_nanos
                    .store(system_time_nanos(deadline), Ordering::Relaxed);
            }
        }
        cancelled
    }

    /// Unregister a finished call. The deadline high-water mark is *not*
    /// lowered here: the extension still has to resume JS after the
    /// `await` to see the result, and that resume must not be killed by
    /// the (by then elapsed) base deadline. [`Inner::disarm_deadline`]
    /// clears it once the whole host call returns.
    fn unregister(&self, id: u64) {
        self.state.lock().live.remove(&id);
    }

    /// Furthest explicit exec deadline armed so far, if any.
    fn deadline(&self) -> Option<Instant> {
        self.state.lock().deadline
    }

    /// `host_exec_cancel(id)` — remember the cancel and poke the running
    /// call, if any.
    fn cancel(&self, id: u64) {
        let mut state = self.state.lock();
        match state.live.get(&id) {
            Some(slot) => {
                slot.cancel.store(true, Ordering::Relaxed);
                slot.notify.notify_waiters();
            }
            None => {
                // Bound the set so a stray id cannot grow it without
                // limit; the shim's ids are monotonic, so a clear only
                // ever drops already-finished ids.
                if state.cancelled.len() >= 1024 {
                    state.cancelled.clear();
                }
                state.cancelled.insert(id);
            }
        }
    }

    /// Kill every call still in flight. Used when the host call deadline
    /// fires: no live call has an unexpired explicit `options.timeout`
    /// left at that point, so every survivor is a runaway.
    fn cancel_all(&self) {
        let state = self.state.lock();
        for slot in state.live.values() {
            slot.cancel.store(true, Ordering::Relaxed);
            slot.notify.notify_waiters();
        }
    }

    /// Clear the deadline high-water mark (end of a host call).
    fn reset_deadline(&self) {
        self.state.lock().deadline = None;
        self.deadline_nanos.store(u64::MAX, Ordering::Relaxed);
    }
}

/// Body of the `host_exec` import: run the requested command and return
/// the `ExecResult` as JSON. Never rejects — upstream `pi.exec` resolves
/// on every outcome and extensions branch on `code` / `killed`.
async fn host_exec_impl(
    bridge: &ExecBridge,
    command: String,
    args_json: String,
) -> rquickjs_core::Result<String> {
    let request: ExecRequest = serde_json::from_str(&args_json).unwrap_or_default();
    // `options.timeout` is clamped so the host deadline cannot overflow.
    let limit = request
        .timeout
        .filter(|ms| *ms > 0)
        .map(|ms| Duration::from_millis(ms).min(MAX_EXEC_TIMEOUT));
    let slot = Arc::new(ExecSlot {
        cancel: AtomicBool::new(false),
        notify: Notify::new(),
        hard_deadline: limit.map(|limit| Instant::now() + limit + EXEC_TIMEOUT_GRACE),
    });
    // Register before spawning so a cancel that races the first poll is
    // seen (`register` returns `true` for an already-cancelled id).
    if let Some(id) = request.id {
        bridge.register(id, &slot);
    }
    let outcome = run_child(&command, &request, &slot, limit).await;
    if let Some(id) = request.id {
        bridge.unregister(id);
    }
    Ok(serde_json::to_string(&outcome)
        .unwrap_or_else(|_| r#"{"stdout":"","stderr":"","code":1,"killed":false}"#.to_string()))
}

/// Spawn `command` with `args` (no shell, like upstream `spawn(..., {shell:
/// false})`), capture stdout / stderr and enforce `limit` (`options.timeout`)
/// plus cancellation via `slot`.
///
/// The child is polled with `try_wait` (rather than `Child::wait`) so the
/// same `&mut child` can both enforce the timeout and react to a cancel —
/// the `node:child_process` wait loop uses the same shape.
///
/// Divergences from upstream, recorded in `docs/EXTENSIONS.md`:
///
/// * a timeout or a cancel kills the child with `SIGKILL` (upstream
///   escalates `SIGTERM` → `SIGKILL` after 5 s) and reports `code = -1`,
///   where Node collapses the missing exit code to `0`; a killed run must
///   never look like a success to `if (code !== 0)` callers.
/// * a spawn failure (`ENOENT`) puts the OS error into `stderr` instead of
///   dropping it, with `code = 1` like upstream.
async fn run_child(
    command: &str,
    request: &ExecRequest,
    slot: &ExecSlot,
    limit: Option<Duration>,
) -> ExecOutcome {
    let mut cmd = tokio::process::Command::new(command);
    cmd.args(&request.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // The host runtime can drop this future (outer per-call timeout);
        // the child must never outlive the extension call.
        .kill_on_drop(true);
    if let Some(cwd) = request.cwd.as_deref().filter(|cwd| !cwd.is_empty()) {
        cmd.current_dir(cwd);
    }
    let started = Instant::now();
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(error) => {
            return ExecOutcome {
                stdout: String::new(),
                stderr: error.to_string(),
                code: 1,
                killed: false,
            }
        }
    };

    // Drain both pipes concurrently with the wait below: a child that
    // writes more than the OS pipe buffer (~64 KiB) would otherwise block
    // forever on write and the host would deadlock waiting for it to exit.
    let stdout_task = tokio::spawn(read_pipe(child.stdout.take()));
    let stderr_task = tokio::spawn(read_pipe(child.stderr.take()));

    let mut killed = false;
    let mut kill_sent = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {}
            Err(_) => break None,
        }
        let cancelled = slot.cancel.load(Ordering::Relaxed);
        let expired = limit.is_some_and(|limit| started.elapsed() >= limit);
        if !kill_sent && (cancelled || expired) {
            killed = true;
            kill_sent = true;
            let _ = child.start_kill();
        }
        // 5 ms poll keeps the loop cheap; the `Notify` makes a cancel
        // immediate instead of waiting for the next tick.
        tokio::select! {
            () = tokio::time::sleep(Duration::from_millis(5)) => {}
            () = slot.notify.notified() => {}
        }
    };

    ExecOutcome {
        stdout: stdout_task.await.unwrap_or_default(),
        stderr: stderr_task.await.unwrap_or_default(),
        // `None` when the process died from a signal (or could not be
        // reaped) — see the divergence note above.
        code: status.and_then(|status| status.code()).unwrap_or(-1),
        killed,
    }
}

/// Drain one child pipe into a string, lossy-decoding UTF-8 the way
/// Node's `data.toString()` does.
async fn read_pipe<R>(pipe: Option<R>) -> String
where
    R: tokio::io::AsyncRead + Unpin,
{
    let Some(mut pipe) = pipe else {
        return String::new();
    };
    let mut buffer = Vec::new();
    let _ = pipe.read_to_end(&mut buffer).await;
    String::from_utf8_lossy(&buffer).into_owned()
}

// ---------------------------------------------------------------------------
// `fetch` global — HTTP bridge
//
// Upstream extensions run inside Node/Bun, so they get the platform `fetch`.
// QuickJS ships none, and a JS-only polyfill cannot reach the network, so the
// shim's `fetch`/`Headers`/`Request`/`Response` are backed by this import: the
// shim serialises the request to JSON, `host_fetch` performs it with the same
// `reqwest` stack the `pi-ai` providers use (and therefore the same rustls +
// proxy-env policy), and the response comes back as
// `{status, statusText, url, redirected, headers, body(base64)}`.
//
// Cancellation and the timeout extension reuse [`ExecBridge`]: the shim
// allocates the id from the same monotonic counter as `pi.exec`, passes it
// here, and calls `host_fetch_cancel(id)` when the caller's `AbortSignal`
// fires; an explicit `timeout` raises the host per-call deadline exactly like
// `pi.exec`'s `options.timeout`.
//
// Documented divergences (see `docs/EXTENSIONS.md`): no streaming body /
// `ReadableStream`, no `FormData`/`Blob` bodies, no automatic content
// decompression (the workspace `reqwest` does not enable the `gzip` feature,
// so no `Accept-Encoding` is sent), and `redirected` is reported as
// `final_url != request_url` rather than a redirect count.
// ---------------------------------------------------------------------------

/// `fetch(url, init)` — request decoded from the JS shim.
#[derive(Debug, Default, Deserialize)]
struct FetchRequest {
    /// Call id allocated by the shim; keys [`ExecBridge`] for cancellation
    /// and the deadline extension. `None` disables both.
    #[serde(default)]
    id: Option<u64>,
    /// Absolute request URL.
    #[serde(default)]
    url: String,
    /// HTTP method; the shim upper-cases it.
    #[serde(default = "default_fetch_method")]
    method: String,
    /// Request headers, in the order the shim saw them.
    #[serde(default)]
    headers: Vec<(String, String)>,
    /// Request body as base64 (`None` for GET / HEAD).
    #[serde(default)]
    body: Option<String>,
    /// Reject after this many milliseconds (`None` = only the host deadline).
    #[serde(default)]
    timeout: Option<u64>,
}

fn default_fetch_method() -> String {
    "GET".to_string()
}

/// The process-wide `reqwest` client. One client means one connection pool and
/// one proxy/TLS configuration for every extension `fetch` — the same policy
/// the provider calls get.
fn fetch_client() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(reqwest::Client::new)
}

/// Envelope the shim turns into a rejected `fetch` promise. `name` is the JS
/// error name (`TypeError` / `AbortError` / `TimeoutError`).
fn fetch_error_envelope(name: &str, message: &str) -> String {
    serde_json::json!({ "ok": false, "name": name, "message": message }).to_string()
}

/// Sleep until `deadline`, or forever when there is none.
async fn sleep_until_opt(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => {
            tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await;
        }
        None => std::future::pending::<()>().await,
    }
}

/// Body of the `host_fetch` import: perform the request and return the JSON
/// envelope. Never rejects, so the shim always owns the JS error object.
async fn host_fetch_impl(bridge: &ExecBridge, request_json: &str) -> rquickjs_core::Result<String> {
    let request: FetchRequest = match serde_json::from_str(request_json) {
        Ok(request) => request,
        Err(error) => {
            return Ok(fetch_error_envelope(
                "TypeError",
                &format!("invalid fetch request: {error}"),
            ))
        }
    };
    let limit = request
        .timeout
        .filter(|ms| *ms > 0)
        .map(|ms| Duration::from_millis(ms).min(MAX_EXEC_TIMEOUT));
    let slot = Arc::new(ExecSlot {
        cancel: AtomicBool::new(false),
        notify: Notify::new(),
        hard_deadline: limit.map(|limit| Instant::now() + limit + EXEC_TIMEOUT_GRACE),
    });
    // Register before sending so a cancel that races the first poll is seen
    // (`register` returns `true` for an already-cancelled id).
    if let Some(id) = request.id {
        bridge.register(id, &slot);
    }
    let outcome = run_fetch(&request, &slot, limit).await;
    if let Some(id) = request.id {
        bridge.unregister(id);
    }
    Ok(outcome)
}

/// Perform one `fetch` request, honouring cancellation (`slot`) and an
/// explicit `limit`. Returns the JSON envelope.
async fn run_fetch(request: &FetchRequest, slot: &ExecSlot, limit: Option<Duration>) -> String {
    if slot.cancel.load(Ordering::Relaxed) {
        return fetch_error_envelope("AbortError", "This operation was aborted");
    }
    let method = match reqwest::Method::from_bytes(request.method.as_bytes()) {
        Ok(method) => method,
        Err(_) => {
            return fetch_error_envelope(
                "TypeError",
                &format!("invalid HTTP method: {}", request.method),
            )
        }
    };
    let mut builder = fetch_client().request(method, &request.url);
    for (name, value) in &request.headers {
        match (
            reqwest::header::HeaderName::from_bytes(name.as_bytes()),
            reqwest::header::HeaderValue::from_str(value),
        ) {
            (Ok(name), Ok(value)) => builder = builder.header(name, value),
            _ => {
                return fetch_error_envelope("TypeError", &format!("invalid header: {name}"));
            }
        }
    }
    if let Some(encoded) = &request.body {
        match base64_decode(encoded) {
            Some(bytes) => builder = builder.body(bytes),
            None => {
                return fetch_error_envelope("TypeError", "request body is not valid base64");
            }
        }
    }

    let deadline = limit.map(|limit| Instant::now() + limit);
    // `send` and the body read are raced against the cancel notify, a 50 ms
    // tick (a `notify_waiters` that lands between the flag check and the
    // `notified()` registration would otherwise be missed) and the explicit
    // timeout. The per-call host deadline still wraps the whole import.
    let send = builder.send();
    tokio::pin!(send);
    let response = loop {
        if slot.cancel.load(Ordering::Relaxed) {
            return fetch_error_envelope("AbortError", "This operation was aborted");
        }
        tokio::select! {
            result = &mut send => break result,
            () = slot.notify.notified() => {}
            () = tokio::time::sleep(Duration::from_millis(50)) => {}
            () = sleep_until_opt(deadline) => {
                return fetch_error_envelope("TimeoutError", "the operation timed out");
            }
        }
    };
    let response = match response {
        Ok(response) => response,
        Err(error) => {
            let name = if error.is_timeout() {
                "TimeoutError"
            } else {
                "TypeError"
            };
            return fetch_error_envelope(name, &error.to_string());
        }
    };

    // Capture the metadata before `bytes(self)` consumes the response.
    let status = response.status();
    let final_url = response.url().to_string();
    let redirected = final_url != request.url;
    let headers: Vec<(String, String)> = response
        .headers()
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_string(),
                value.to_str().unwrap_or("").to_string(),
            )
        })
        .collect();

    let body = response.bytes();
    tokio::pin!(body);
    let bytes = loop {
        if slot.cancel.load(Ordering::Relaxed) {
            return fetch_error_envelope("AbortError", "This operation was aborted");
        }
        tokio::select! {
            result = &mut body => break result,
            () = slot.notify.notified() => {}
            () = tokio::time::sleep(Duration::from_millis(50)) => {}
            () = sleep_until_opt(deadline) => {
                return fetch_error_envelope("TimeoutError", "the operation timed out");
            }
        }
    };
    let bytes = match bytes {
        Ok(bytes) => bytes,
        Err(error) => {
            let name = if error.is_timeout() {
                "TimeoutError"
            } else {
                "TypeError"
            };
            return fetch_error_envelope(name, &error.to_string());
        }
    };

    serde_json::to_string(&serde_json::json!({
        "ok": true,
        "status": status.as_u16(),
        "statusText": status.canonical_reason().unwrap_or(""),
        "url": final_url,
        "redirected": redirected,
        "headers": headers,
        "body": base64_encode(&bytes),
    }))
    .unwrap_or_else(|_| fetch_error_envelope("TypeError", "failed to encode the response"))
}

// ---------------------------------------------------------------------------
// `node:child_process` bridge
//
// `spawn` hands back a live handle, so unlike every other `node:*` op the
// child outlives the JS call that created it:
//
//   shim                            host (Rust)
//   spawn(cmd, args)                child_process.spawn → tokio::process::Command
//     ──────────────────────────▶     + one reader task per pipe
//     ◀──── {handle, pid}             + one wait task (deadline + kill channel)
//   await host_child_read           parks on a `watch` channel until bytes / EOF
//   await host_child_wait           parks until the process is reaped
//
// The buffered forms (`exec` / `execSync` / `spawnSync`) are a one-shot
// variant that captures both pipes and returns a Node-shaped result. They are
// the source of truth: the callback / promise wrappers in the shim run the
// sync form and schedule their callback on the microtask queue, exactly like
// `node:fs/promises` does.
// ---------------------------------------------------------------------------

/// Handles the shim passes back (`child_process.spawn` allocates them).
#[derive(Clone)]
struct ChildBridge {
    children: Children,
    next_child: Arc<AtomicU64>,
    /// Ceiling every child gets: the host per-call timeout.
    timeout: Duration,
}

/// One buffered pipe of a spawned child.
struct ChildStream {
    state: Mutex<ChildStreamState>,
    /// Bumped on every write / EOF so `host_child_read` can park without
    /// polling. A `watch` channel is used instead of `Notify` because its
    /// version counter cannot lose a wake-up between the state check and the
    /// await.
    progress: watch::Sender<u64>,
}

#[derive(Default)]
struct ChildStreamState {
    data: Vec<u8>,
    eof: bool,
}

impl ChildStream {
    fn new() -> Self {
        Self {
            state: Mutex::new(ChildStreamState::default()),
            progress: watch::channel(0u64).0,
        }
    }

    fn push(&self, chunk: &[u8]) {
        self.state.lock().data.extend_from_slice(chunk);
        self.progress
            .send_modify(|version| *version = version.wrapping_add(1));
    }

    fn finish(&self) {
        self.state.lock().eof = true;
        self.progress
            .send_modify(|version| *version = version.wrapping_add(1));
    }

    /// Take everything buffered. `None` means "nothing yet, not EOF".
    fn take(&self) -> Option<(Vec<u8>, bool)> {
        let mut state = self.state.lock();
        if state.data.is_empty() {
            return state.eof.then(|| (Vec::new(), true));
        }
        let data = std::mem::take(&mut state.data);
        Some((data, state.eof))
    }
}

/// Live state for one `node:child_process` child.
struct ChildEntry {
    stdout: Arc<ChildStream>,
    stderr: Arc<ChildStream>,
    exit: Mutex<Option<ChildExit>>,
    exit_progress: watch::Sender<u64>,
    /// Poked by `child.kill()`; the wait task owns the `Child` and performs
    /// the signal, so no external code needs `&mut Child`.
    kill_tx: Mutex<Option<mpsc::UnboundedSender<()>>>,
    killed: AtomicBool,
}

#[derive(Clone, Serialize)]
struct ChildExit {
    code: Option<i32>,
    signal: Option<String>,
    killed: bool,
    timed_out: bool,
}

impl ChildEntry {
    fn new() -> Self {
        Self {
            stdout: Arc::new(ChildStream::new()),
            stderr: Arc::new(ChildStream::new()),
            exit: Mutex::new(None),
            exit_progress: watch::channel(0u64).0,
            kill_tx: Mutex::new(None),
            killed: AtomicBool::new(false),
        }
    }
}

/// Drain one child pipe into its [`ChildStream`] until EOF.
async fn pump_child_stream<R>(mut pipe: R, stream: Arc<ChildStream>)
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut buffer = vec![0u8; 16 * 1024];
    loop {
        match pipe.read(&mut buffer).await {
            Ok(0) => break,
            Ok(read) => stream.push(&buffer[..read]),
            Err(_) => break,
        }
    }
    stream.finish();
}

/// Own the child until it exits, honouring the host deadline and any
/// `child.kill()` request. Polling `try_wait` (rather than `select!` on
/// `Child::wait`) keeps the `&mut Child` borrow in one place so the kill
/// branch can signal it.
async fn wait_child(
    mut child: tokio::process::Child,
    entry: Arc<ChildEntry>,
    mut kill_rx: mpsc::UnboundedReceiver<()>,
    deadline: Option<Instant>,
) {
    let mut killed = false;
    let mut timed_out = false;
    let mut kill_sent = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {}
            Err(_) => break None,
        }
        if !kill_sent && deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            killed = true;
            timed_out = true;
            kill_sent = true;
            let _ = child.start_kill();
        }
        let sleep = tokio::time::sleep(Duration::from_millis(5));
        tokio::select! {
            () = sleep => {}
            request = kill_rx.recv() => {
                if request.is_some() && !kill_sent {
                    killed = true;
                    kill_sent = true;
                    let _ = child.start_kill();
                }
            }
        }
    };
    if killed {
        entry.killed.store(true, Ordering::Relaxed);
    }
    let (code, signal) = match status {
        Some(status) => (status.code(), exit_signal_name(status)),
        None => (None, None),
    };
    *entry.exit.lock() = Some(ChildExit {
        code,
        signal,
        killed,
        timed_out,
    });
    entry
        .exit_progress
        .send_modify(|version| *version = version.wrapping_add(1));
}

#[cfg(unix)]
fn exit_signal_name(status: std::process::ExitStatus) -> Option<String> {
    use std::os::unix::process::ExitStatusExt;
    status.signal().map(signal_name)
}

#[cfg(not(unix))]
fn exit_signal_name(_status: std::process::ExitStatus) -> Option<String> {
    None
}

fn signal_name(signal: i32) -> String {
    let name = match signal {
        1 => "SIGHUP",
        2 => "SIGINT",
        3 => "SIGQUIT",
        6 => "SIGABRT",
        9 => "SIGKILL",
        13 => "SIGPIPE",
        14 => "SIGALRM",
        15 => "SIGTERM",
        _ => return format!("SIG{signal}"),
    };
    name.to_string()
}

fn child_lookup(children: &Children, handle: u64) -> rquickjs_core::Result<Arc<ChildEntry>> {
    children.lock().get(&handle).cloned().ok_or_else(|| {
        rquickjs_core::Error::new_from_js_message(
            "child_process",
            "handle",
            format!("unknown child process handle {handle}"),
        )
    })
}

/// `host_child_read(handle, stream)` — resolve once the stream has bytes or
/// reaches EOF (`{data, done}`).
async fn child_read_impl(
    handle: u64,
    stream: String,
    children: &Children,
) -> rquickjs_core::Result<String> {
    let entry = child_lookup(children, handle)?;
    let stream = match stream.as_str() {
        "stdout" => entry.stdout.clone(),
        "stderr" => entry.stderr.clone(),
        other => {
            return Err(rquickjs_core::Error::new_from_js_message(
                "child_process",
                "stream",
                format!("unknown child stream `{other}`"),
            ))
        }
    };
    let mut progress = stream.progress.subscribe();
    loop {
        if let Some((data, done)) = stream.take() {
            return Ok(serde_json::json!({
                "data": base64_encode(&data),
                "done": done,
            })
            .to_string());
        }
        if progress.changed().await.is_err() {
            return Ok(serde_json::json!({ "data": "", "done": true }).to_string());
        }
    }
}

/// `host_child_wait(handle)` — resolve with the exit record once the child is
/// reaped.
async fn child_wait_impl(handle: u64, children: &Children) -> rquickjs_core::Result<String> {
    let entry = child_lookup(children, handle)?;
    let mut progress = entry.exit_progress.subscribe();
    loop {
        if let Some(exit) = entry.exit.lock().clone() {
            return Ok(serde_json::to_string(&exit).unwrap_or_else(|_| "{}".to_string()));
        }
        if progress.changed().await.is_err() {
            return Err(rquickjs_core::Error::new_from_js_message(
                "child_process",
                "handle",
                "child process handle was dropped before it exited",
            ));
        }
    }
}

/// `child_process.spawn` — start the process and return `{handle, pid}`.
fn spawn_child(
    args: &serde_json::Value,
    bridge: &ChildBridge,
) -> Result<serde_json::Value, NodeError> {
    let program = node_arg_str(args, "program")?;
    let argv = node_arg_str_array(args, "args")?;
    let shell = node_arg_bool(args, "shell");
    let cwd = args
        .get("cwd")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let env = node_arg_env(args)?;
    let (program, argv) = if shell {
        shell_wrap(&program, &argv)
    } else {
        (program, argv)
    };

    let mut command = tokio::process::Command::new(&program);
    command
        .args(&argv)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // A host that disappears must not orphan the OS process.
        .kill_on_drop(true);
    if let Some(cwd) = cwd.as_deref().filter(|cwd| !cwd.is_empty()) {
        command.current_dir(cwd);
    }
    if let Some(env) = env.as_ref() {
        command.env_clear();
        command.envs(env.iter().map(|(key, value)| (key, value)));
    }
    let mut child = command
        .spawn()
        .map_err(|error| NodeError::io(error, "spawn", &program))?;
    let pid = child.id().unwrap_or(0);
    let entry = Arc::new(ChildEntry::new());
    if let Some(pipe) = child.stdout.take() {
        tokio::spawn(pump_child_stream(pipe, entry.stdout.clone()));
    } else {
        entry.stdout.finish();
    }
    if let Some(pipe) = child.stderr.take() {
        tokio::spawn(pump_child_stream(pipe, entry.stderr.clone()));
    } else {
        entry.stderr.finish();
    }
    let (kill_tx, kill_rx) = mpsc::unbounded_channel();
    *entry.kill_tx.lock() = Some(kill_tx);
    // Bounded by the host per-call timeout: a long-lived child cannot pin an
    // extension call (or the `pi` process) forever. A shorter `timeout` in the
    // options tightens the bound, never loosens it.
    let requested = args
        .get("timeoutMs")
        .and_then(serde_json::Value::as_u64)
        .filter(|ms| *ms > 0)
        .map(Duration::from_millis);
    let deadline = Instant::now() + requested.unwrap_or(bridge.timeout).min(bridge.timeout);
    tokio::spawn(wait_child(child, entry.clone(), kill_rx, Some(deadline)));
    let handle = bridge.next_child.fetch_add(1, Ordering::Relaxed);
    bridge.children.lock().insert(handle, entry);
    Ok(serde_json::json!({ "handle": handle, "pid": pid }))
}

/// `child_process.kill` — ask the wait task to signal the child.
fn kill_child(
    args: &serde_json::Value,
    bridge: &ChildBridge,
) -> Result<serde_json::Value, NodeError> {
    let handle = node_arg_u64(args, "handle")?;
    let entry = bridge.children.lock().get(&handle).cloned();
    let Some(entry) = entry else {
        return Ok(serde_json::json!(false));
    };
    if entry.exit.lock().is_some() {
        return Ok(serde_json::json!(false));
    }
    let sender = entry.kill_tx.lock().clone();
    match sender {
        Some(sender) => {
            entry.killed.store(true, Ordering::Relaxed);
            let _ = sender.send(());
            Ok(serde_json::json!(true))
        }
        None => Ok(serde_json::json!(false)),
    }
}

/// One-shot command used by `execSync` / `spawnSync` / `execFileSync` (and,
/// through the shim, by the callback / promise forms).
fn run_command_sync_op(
    args: &serde_json::Value,
    bridge: &ChildBridge,
) -> Result<serde_json::Value, NodeError> {
    let program = node_arg_str(args, "program")?;
    let argv = node_arg_str_array(args, "args")?;
    let shell = node_arg_bool(args, "shell");
    let cwd = args
        .get("cwd")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let env = node_arg_env(args)?;
    let input = match args.get("input").and_then(|value| value.as_str()) {
        Some(encoded) => Some(
            base64_decode(encoded)
                .ok_or_else(|| NodeError::invalid("`input` is not valid base64"))?,
        ),
        None => None,
    };
    let requested = args
        .get("timeoutMs")
        .and_then(|value| value.as_u64())
        .filter(|ms| *ms > 0)
        .map(Duration::from_millis);
    // The host deadline is a ceiling: an extension cannot opt into a call that
    // outlives the per-call timeout the host itself enforces.
    let timeout = Some(requested.map_or(bridge.timeout, |value| value.min(bridge.timeout)));
    let max_buffer = args
        .get("maxBuffer")
        .and_then(|value| value.as_u64())
        .map(|value| value as usize);
    let outcome = run_command_sync(RunSpec {
        program,
        args: argv,
        shell,
        cwd,
        env,
        input,
        timeout,
        max_buffer,
    })?;
    Ok(serde_json::json!({
        "pid": outcome.pid,
        "stdout": base64_encode(&outcome.stdout),
        "stderr": base64_encode(&outcome.stderr),
        "code": outcome.code,
        "signal": outcome.signal,
        "killed": outcome.killed,
        "timedOut": outcome.timed_out,
        "maxBufferExceeded": outcome.max_buffer_exceeded,
    }))
}

/// Dispatch the `child_process.*` ops. Returns the same error shape as
/// [`node_call`] so the shim's envelope handling is unchanged.
fn child_process_call(
    op: &str,
    args: &serde_json::Value,
    bridge: &ChildBridge,
) -> Result<serde_json::Value, NodeError> {
    match op {
        "child_process.spawn" => spawn_child(args, bridge),
        "child_process.kill" => kill_child(args, bridge),
        "child_process.reap" => {
            let handle = node_arg_u64(args, "handle")?;
            bridge.children.lock().remove(&handle);
            Ok(serde_json::json!(true))
        }
        "child_process.runSync" => run_command_sync_op(args, bridge),
        other => Err(NodeError::new(
            "ERR_UNSUPPORTED_OPERATION",
            format!("node bridge op `{other}` is not implemented"),
        )),
    }
}

struct RunSpec {
    program: String,
    args: Vec<String>,
    shell: bool,
    cwd: Option<String>,
    env: Option<Vec<(String, String)>>,
    input: Option<Vec<u8>>,
    timeout: Option<Duration>,
    max_buffer: Option<usize>,
}

struct RunOutcome {
    pid: u32,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    code: Option<i32>,
    signal: Option<String>,
    killed: bool,
    timed_out: bool,
    max_buffer_exceeded: bool,
}

/// Blocking (truly synchronous) runner behind the `*Sync` APIs. It uses
/// `std::process` + OS threads rather than `tokio::process` because it runs
/// *inside* a host import: blocking the caller's runtime thread while tokio
/// tasks drained the pipes would deadlock.
fn run_command_sync(spec: RunSpec) -> Result<RunOutcome, NodeError> {
    let (program, args) = if spec.shell {
        shell_wrap(&spec.program, &spec.args)
    } else {
        (spec.program.clone(), spec.args.clone())
    };
    let mut command = std::process::Command::new(&program);
    command
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(if spec.input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
    if let Some(cwd) = spec.cwd.as_deref().filter(|cwd| !cwd.is_empty()) {
        command.current_dir(cwd);
    }
    if let Some(env) = spec.env.as_ref() {
        command.env_clear();
        command.envs(env.iter().map(|(key, value)| (key, value)));
    }
    let mut child = command
        .spawn()
        .map_err(|error| NodeError::io(error, "spawn", &program))?;
    let pid = child.id();

    // A child that fills its stdin pipe before we read it would deadlock, so
    // feed it from its own thread — mirroring the pipe readers below. The
    // write is bounded too: a child that never reads stdin must not pin us.
    let (stdin_tx, stdin_rx) = std::sync::mpsc::channel();
    let stdin_thread = spec.input.map(|input| {
        let mut stdin = child.stdin.take();
        std::thread::spawn(move || {
            if let Some(stdin) = stdin.as_mut() {
                let _ = stdin.write_all(&input);
                let _ = stdin.flush();
            }
            let _ = stdin_tx.send(());
        })
    });

    let cap = spec.max_buffer;
    let stdout_buffer = Arc::new(PipeBuffer::new());
    let stderr_buffer = Arc::new(PipeBuffer::new());
    let stdout_thread = std::thread::spawn({
        let pipe = child.stdout.take();
        let buffer = stdout_buffer.clone();
        move || read_pipe_blocking(pipe, cap, buffer)
    });
    let stderr_thread = std::thread::spawn({
        let pipe = child.stderr.take();
        let buffer = stderr_buffer.clone();
        move || read_pipe_blocking(pipe, cap, buffer)
    });

    let deadline = spec.timeout.map(|timeout| Instant::now() + timeout);
    let mut killed = false;
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {}
            Err(_) => break None,
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            killed = true;
            timed_out = true;
            let _ = child.kill();
            break child.wait().ok();
        }
        std::thread::sleep(Duration::from_millis(5));
    };

    // Both pipes are drained with a bounded wait. The direct child is gone by
    // here, but a grandchild (`sh -c "sleep 30 &"`) can still hold the write
    // end open: waiting for EOF would pin the host for as long as that
    // grandchild lives. We keep whatever was captured instead — see the
    // divergence table in `docs/NODE_BUILTINS.md`.
    let grace = Duration::from_millis(PIPE_DRAIN_GRACE_MS);
    let (stdout, stdout_exceeded) = stdout_buffer.take_after(grace);
    let (stderr, stderr_exceeded) = stderr_buffer.take_after(grace);
    if let Some(thread) = stdin_thread {
        let _ = stdin_rx.recv_timeout(grace);
        if thread.is_finished() {
            let _ = thread.join();
        }
    }
    if stdout_thread.is_finished() {
        let _ = stdout_thread.join();
    }
    if stderr_thread.is_finished() {
        let _ = stderr_thread.join();
    }
    let (code, signal) = match status {
        Some(status) => (status.code(), exit_signal_name(status)),
        None => (None, None),
    };
    Ok(RunOutcome {
        pid,
        stdout,
        stderr,
        code,
        signal,
        killed,
        timed_out,
        max_buffer_exceeded: stdout_exceeded || stderr_exceeded,
    })
}

/// How long a `*Sync` call keeps draining a pipe after the direct child is
/// gone. Bounds a grandchild that inherited the pipe's write end.
const PIPE_DRAIN_GRACE_MS: u64 = 250;

/// Accumulator shared with a pipe reader thread. The reader may outlive the
/// call (a grandchild holding the pipe), so the caller reads a snapshot
/// rather than joining unconditionally.
struct PipeBuffer {
    state: std::sync::Mutex<PipeBufferState>,
    done: std::sync::Condvar,
}

#[derive(Default)]
struct PipeBufferState {
    data: Vec<u8>,
    exceeded: bool,
    finished: bool,
}

impl PipeBuffer {
    fn new() -> Self {
        Self {
            state: std::sync::Mutex::new(PipeBufferState::default()),
            done: std::sync::Condvar::new(),
        }
    }

    fn push(&self, chunk: &[u8], cap: Option<usize>) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        match cap {
            Some(cap) => {
                let remaining = cap.saturating_sub(state.data.len());
                if chunk.len() > remaining {
                    state.data.extend_from_slice(&chunk[..remaining]);
                    state.exceeded = true;
                } else {
                    state.data.extend_from_slice(chunk);
                }
            }
            None => state.data.extend_from_slice(chunk),
        }
    }

    fn finish(&self) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.finished = true;
        self.done.notify_all();
    }

    /// Wait up to `grace` for EOF, then snapshot whatever has been captured.
    fn take_after(&self, grace: Duration) -> (Vec<u8>, bool) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let deadline = Instant::now() + grace;
        while !state.finished {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            let (next, _) = self
                .done
                .wait_timeout(state, remaining)
                .unwrap_or_else(|error| error.into_inner());
            state = next;
        }
        (state.data.clone(), state.exceeded)
    }
}

/// Read a blocking pipe to EOF, keeping at most `cap` bytes but still draining
/// so the child can exit. The cap flag reports whether the cap was hit.
fn read_pipe_blocking<R: std::io::Read>(
    pipe: Option<R>,
    cap: Option<usize>,
    buffer: Arc<PipeBuffer>,
) {
    let Some(mut pipe) = pipe else {
        buffer.finish();
        return;
    };
    let mut chunk = [0u8; 16 * 1024];
    loop {
        match pipe.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => buffer.push(&chunk[..read], cap),
            Err(_) => break,
        }
    }
    buffer.finish();
}

/// Build the `(program, args)` pair that runs `program args…` through the
/// platform shell. Arguments are joined with spaces and **not** quoted, which
/// matches Node's `shell: true` for the simple commands extensions use (and is
/// documented as a divergence).
fn shell_wrap(program: &str, args: &[String]) -> (String, Vec<String>) {
    let mut command = program.to_string();
    for arg in args {
        command.push(' ');
        command.push_str(arg);
    }
    #[cfg(windows)]
    {
        ("cmd.exe".to_string(), vec!["/C".to_string(), command])
    }
    #[cfg(not(windows))]
    {
        ("/bin/sh".to_string(), vec!["-c".to_string(), command])
    }
}

fn node_arg_u64(args: &serde_json::Value, key: &str) -> Result<u64, NodeError> {
    args.get(key)
        .and_then(|value| value.as_u64())
        .ok_or_else(|| NodeError::invalid(format!("missing numeric argument `{key}`")))
}

fn node_arg_str_array(args: &serde_json::Value, key: &str) -> Result<Vec<String>, NodeError> {
    match args.get(key) {
        None | Some(serde_json::Value::Null) => Ok(Vec::new()),
        Some(serde_json::Value::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_str().map(str::to_string).ok_or_else(|| {
                    NodeError::invalid(format!("`{key}` must be an array of strings"))
                })
            })
            .collect(),
        Some(_) => Err(NodeError::invalid(format!(
            "`{key}` must be an array of strings"
        ))),
    }
}

/// Node replaces the whole environment when `options.env` is given; a missing
/// key is removed, and non-string values are coerced. `null` values are
/// skipped, like Node's `undefined`.
fn node_arg_env(args: &serde_json::Value) -> Result<Option<Vec<(String, String)>>, NodeError> {
    match args.get("env") {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::Object(map)) => {
            let mut out = Vec::with_capacity(map.len());
            for (key, value) in map {
                if value.is_null() {
                    continue;
                }
                let value = match value {
                    serde_json::Value::String(text) => text.clone(),
                    other => other.to_string(),
                };
                out.push((key.clone(), value));
            }
            Ok(Some(out))
        }
        Some(_) => Err(NodeError::invalid("`env` must be an object")),
    }
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

    // The shim falls back to this value when a `node:*` module is asked
    // for the working directory (`process.cwd()` / `path.resolve` on a
    // relative path); the host is the only side that knows it.
    globals.set("_pi_cwd", inner.tool_context.cwd.clone())?;

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

    // host_node_call(op, argsJson) — the single entry point behind the
    // `node:*` virtual modules (fs / os / process / crypto / child_process).
    // Returns a JSON *envelope* (`{"ok":true,"value":…}` /
    // `{"ok":false,…}`) instead of throwing so the shim can build the
    // `Error` object with the `code` / `syscall` / `path` fields Node
    // extensions branch on. `child_process` needs the live-child bridge, so
    // the import closes over a clone of it.
    let bridge = ChildBridge {
        children: inner.children.clone(),
        next_child: inner.next_child.clone(),
        timeout: inner.timeout,
    };
    let node_call_bridge = bridge.clone();
    let node_call_fn = Func::from(move |op: String, args_json: String| -> String {
        host_node_call_dispatch(&op, &args_json, &node_call_bridge)
    });
    globals.set("host_node_call", node_call_fn)?;

    // host_child_read(handle, stream) -> Promise<{data, done}> — drains what
    // the reader task has buffered so far. `data` is base64 so arbitrary
    // binary survives the JSON envelope.
    let read_bridge = bridge.clone();
    let child_read_fn = move |handle: u64, stream: String| {
        let children = read_bridge.children.clone();
        async move { child_read_impl(handle, stream, &children).await }
    };
    globals.set(
        "host_child_read",
        Function::new(ctx.clone(), Async(child_read_fn))?,
    )?;

    // host_child_wait(handle) -> Promise<{code, signal, killed, timedOut}>
    let wait_bridge = bridge.clone();
    let child_wait_fn = move |handle: u64| {
        let children = wait_bridge.children.clone();
        async move { child_wait_impl(handle, &children).await }
    };
    globals.set(
        "host_child_wait",
        Function::new(ctx.clone(), Async(child_wait_fn))?,
    )?;

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

    // host_exec_cancel(id) — cancel a running `pi.exec`. The shim calls
    // this when the extension's `options.signal` fires; the host kills the
    // child and `host_exec` resolves with `killed: true`.
    let cancel_bridge = inner.execs.clone();
    let cancel_fn = Func::from(move |id: u64| -> rquickjs_core::Result<()> {
        cancel_bridge.cancel(id);
        Ok(())
    });
    globals.set("host_exec_cancel", cancel_fn)?;

    // host_exec(command, argsJson) -> Promise<string> — the `pi.exec`
    // bridge. Resolves with the JSON `ExecResult`
    // (`{stdout, stderr, code, killed}`); never rejects, like upstream.
    let exec_bridge = inner.execs.clone();
    let exec_fn = move |command: String, args_json: String| {
        let bridge = exec_bridge.clone();
        async move { host_exec_impl(&bridge, command, args_json).await }
    };
    let exec_function = Function::new(ctx.clone(), Async(exec_fn))?;
    globals.set("host_exec", exec_function)?;

    // host_fetch_cancel(id) — cancel a running `fetch`. The shim calls this
    // when the request's `AbortSignal` fires; the host drops the in-flight
    // request and `host_fetch` resolves with the `AbortError` envelope.
    let fetch_cancel_bridge = inner.execs.clone();
    let fetch_cancel_fn = Func::from(move |id: u64| -> rquickjs_core::Result<()> {
        fetch_cancel_bridge.cancel(id);
        Ok(())
    });
    globals.set("host_fetch_cancel", fetch_cancel_fn)?;

    // host_fetch(requestJson) -> Promise<string> — the `fetch` global's
    // transport. `requestJson` is
    // `{id?, url, method?, headers?, body?(base64), timeout?}`; resolves with
    // the JSON `{ok, status, statusText, url, redirected, headers, body}`
    // envelope (or `{ok:false,name,message}`). Never rejects.
    let fetch_bridge = inner.execs.clone();
    let fetch_fn = move |request_json: String| {
        let bridge = fetch_bridge.clone();
        async move { host_fetch_impl(&bridge, &request_json).await }
    };
    let fetch_function = Function::new(ctx.clone(), Async(fetch_fn))?;
    globals.set("host_fetch", fetch_function)?;

    // host_builtin_tool_definition(name) -> JSON string — synchronous,
    // because a JS `create*Tool` factory hands out `parameters` the
    // moment it is called. Resolves to
    // `{"ok":true,"definition":{name,label,description,parameters}}` /
    // `{"ok":false,"error":"…"}`. The shim never rejects on this; it
    // falls back to a permissive schema when the runner is missing.
    let definition_runner = inner.builtin_tool_runner.clone();
    let builtin_tool_definition_fn = Func::from(move |name: String| -> String {
        match definition_runner
            .as_ref()
            .and_then(|runner| runner.definition(&name))
        {
            Some(definition) => {
                serde_json::json!({"ok": true, "definition": definition}).to_string()
            }
            None => serde_json::json!({
                "ok": false,
                "error": format!(
                    "built-in tool `{name}` is not available (no runner installed or unknown name)"
                ),
            })
            .to_string(),
        }
    });
    globals.set("host_builtin_tool_definition", builtin_tool_definition_fn)?;

    // host_builtin_tool(name, argsJson, cwd) -> Promise<string> — the
    // bridge behind the JS `create*Tool` factories' `execute`. Resolves
    // with a JSON envelope (`{"ok":true,"content":[…],"isError":…,
    // "details":…}` / `{"ok":false,"error":"…"}`); never rejects, like
    // `host_exec`, so the shim owns the JS `Error` shape.
    let builtin_runner = inner.builtin_tool_runner.clone();
    let builtin_tool_fn = move |name: String, args_json: String, cwd: Option<String>| {
        let runner = builtin_runner.clone();
        async move { host_builtin_tool_impl(runner, name, args_json, cwd).await }
    };
    let builtin_tool_function = Function::new(ctx.clone(), Async(builtin_tool_fn))?;
    globals.set("host_builtin_tool", builtin_tool_function)?;

    Ok(())
}

/// Body of the `host_builtin_tool` import.
///
/// Returns a JSON envelope and never rejects: `{"ok":false,"error":…}`
/// covers a missing runner, malformed arguments and a runner-level
/// failure alike, so the shim can construct the JS `Error` with a stable
/// code. A tool that ran but failed is still `ok:true` with `isError`
/// set, matching how [`ToolExecutionOutcome`] reports tool errors.
async fn host_builtin_tool_impl(
    runner: Option<Arc<dyn BuiltinToolRunner>>,
    name: String,
    args_json: String,
    cwd: Option<String>,
) -> rquickjs_core::Result<String> {
    let Some(runner) = runner else {
        return Ok(serde_json::json!({
            "ok": false,
            "error": "the extension host has no built-in tool runner; `create*Tool` factories cannot execute here",
        })
        .to_string());
    };
    let args: serde_json::Value = match serde_json::from_str(&args_json) {
        Ok(args) => args,
        Err(err) => {
            return Ok(serde_json::json!({
                "ok": false,
                "error": format!("invalid tool arguments: {err}"),
            })
            .to_string())
        }
    };
    let cwd = cwd.filter(|value| !value.is_empty());
    match runner.run(name, args, cwd).await {
        Ok(outcome) => Ok(serde_json::json!({
            "ok": true,
            "content": outcome.content,
            "isError": outcome.is_error,
            "details": outcome.details,
        })
        .to_string()),
        Err(err) => Ok(serde_json::json!({"ok": false, "error": err}).to_string()),
    }
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

// ---------------------------------------------------------------------------
// `node:*` builtin bridge
//
// Upstream extension hosts run on Node/Bun, so the extension ecosystem
// imports `node:fs`, `node:fs/promises`, `node:os`, `node:buffer` and
// `node:crypto` directly (`packages/coding-agent/examples/extensions/*`
// does across ~15 of its examples). The embedded QuickJS runtime has no
// filesystem of its own, so the shim's virtual modules are backed by the
// `host_node_call` import installed above: one JSON-in / JSON-out
// function keeps the Rust surface small and makes every op unit
// testable.
//
// The protocol is deliberately boring:
//
//   host_node_call("fs.readFile", "{\"path\":\"/x\"}")
//     => {"ok":true,"value":{"base64":"aGk="}}
//   host_node_call("fs.readFile", "{\"path\":\"/missing\"}")
//     => {"ok":false,"code":"ENOENT","message":"…","syscall":"open","path":"/missing"}
//
// Errors are *values*, not QuickJS exceptions: Node extensions branch on
// `err.code` (`ENOENT` / `EEXIST` / …) and turning that into a stringly
// typed JS exception in Rust would lose the shape.
// ---------------------------------------------------------------------------

/// Decode the `args_json` payload, run the requested op and encode the
/// result envelope. Never panics: malformed JSON is reported as
/// `EINVAL` so a bad shim call surfaces as a normal JS error instead of
/// taking the host down.
///
/// `child_process.*` ops carry live handles and so need the bridge; every
/// other `node:*` op goes to the stateless [`node_call`] table.
fn host_node_call_dispatch(op: &str, args_json: &str, bridge: &ChildBridge) -> String {
    let args: serde_json::Value =
        serde_json::from_str(args_json).unwrap_or(serde_json::Value::Null);
    let result = if op.starts_with("child_process.") {
        child_process_call(op, &args, bridge)
    } else {
        node_call(op, &args)
    };
    match result {
        Ok(value) => serde_json::json!({ "ok": true, "value": value }).to_string(),
        Err(err) => serde_json::json!({
            "ok": false,
            "code": err.code,
            "message": err.message,
            "syscall": err.syscall,
            "path": err.path,
        })
        .to_string(),
    }
}

/// A Node-shaped filesystem / OS error (`code` + human message).
struct NodeError {
    code: String,
    message: String,
    syscall: Option<String>,
    path: Option<String>,
}

impl NodeError {
    fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
            syscall: None,
            path: None,
        }
    }

    /// Map a Rust `io::Error` onto the Node errno string extensions
    /// check (`ENOENT` for a missing file, `EEXIST` for an occupied
    /// path, …) plus Node's `CODE: message, syscall 'path'` rendering.
    fn io(err: std::io::Error, syscall: &str, path: &str) -> Self {
        let code = errno_code(&err);
        Self {
            message: format!("{}: {}, {} '{}'", code, err, syscall, path),
            code,
            syscall: Some(syscall.to_string()),
            path: Some(path.to_string()),
        }
    }

    fn invalid(message: impl Into<String>) -> Self {
        Self::new("EINVAL", message)
    }
}

/// `io::Error` → POSIX errno name. The raw OS code is preferred (it is
/// what Node reports); `ErrorKind` is the fallback for errors without
/// one (e.g. custom `io::Error`s built by `std`).
fn errno_code(err: &std::io::Error) -> String {
    if let Some(raw) = err.raw_os_error() {
        let name = match raw {
            1 => "EPERM",
            2 => "ENOENT",
            5 => "EIO",
            13 => "EACCES",
            17 => "EEXIST",
            20 => "ENOTDIR",
            21 => "EISDIR",
            22 => "EINVAL",
            28 => "ENOSPC",
            30 => "EROFS",
            36 => "ENAMETOOLONG",
            39 => "ENOTEMPTY",
            _ => "",
        };
        if !name.is_empty() {
            return name.to_string();
        }
    }
    let name = match err.kind() {
        std::io::ErrorKind::NotFound => "ENOENT",
        std::io::ErrorKind::PermissionDenied => "EACCES",
        std::io::ErrorKind::AlreadyExists => "EEXIST",
        std::io::ErrorKind::InvalidInput => "EINVAL",
        std::io::ErrorKind::InvalidData => "EINVAL",
        std::io::ErrorKind::UnexpectedEof => "EIO",
        _ => "EIO",
    };
    name.to_string()
}

/// Read a required string argument.
fn node_arg_str(args: &serde_json::Value, key: &str) -> Result<String, NodeError> {
    args.get(key)
        .and_then(|value| value.as_str())
        .map(|value| value.to_string())
        .ok_or_else(|| NodeError::invalid(format!("missing string argument `{key}`")))
}

/// Read a required base64-encoded byte argument. The shim sends binary
/// payloads as base64 so the op table stays JSON-in / JSON-out.
fn node_arg_bytes(args: &serde_json::Value, key: &str) -> Result<Vec<u8>, NodeError> {
    let encoded = node_arg_str(args, key)?;
    base64_decode(&encoded)
        .ok_or_else(|| NodeError::invalid(format!("`{key}` argument is not valid base64")))
}

fn node_arg_bool(args: &serde_json::Value, key: &str) -> bool {
    args.get(key)
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

/// System-time → milliseconds since the epoch, as Node's `Stats` reports.
fn millis(time: std::io::Result<SystemTime>) -> f64 {
    match time.and_then(|t| t.duration_since(UNIX_EPOCH).map_err(std::io::Error::other)) {
        Ok(duration) => duration.as_secs_f64() * 1000.0,
        Err(_) => 0.0,
    }
}

fn stat_json(meta: &std::fs::Metadata) -> serde_json::Value {
    // `mode` is a unix concept; other targets report the conventional
    // 0o644 so extension code can still branch on "is this executable".
    #[cfg(unix)]
    let mode = {
        use std::os::unix::fs::MetadataExt;
        meta.mode()
    };
    #[cfg(not(unix))]
    let mode = 0o100644u32;
    let birthtime = millis(meta.created());
    let modified = millis(meta.modified());
    serde_json::json!({
        "isFile": meta.is_file(),
        "isDirectory": meta.is_dir(),
        "isSymbolicLink": meta.file_type().is_symlink(),
        "size": meta.len(),
        "mode": mode,
        "mtimeMs": modified,
        "atimeMs": millis(meta.accessed()),
        // Node reports the inode change time here; creation time is the
        // closest portable stand-in (equal on filesystems that expose no
        // birth time).
        "ctimeMs": if birthtime > 0.0 { birthtime } else { modified },
        "birthtimeMs": birthtime,
    })
}

/// Dispatch one `node:*` op. Kept as a single table so the shim has one
/// import to talk to and the ops can be unit-tested directly.
fn node_call(op: &str, args: &serde_json::Value) -> Result<serde_json::Value, NodeError> {
    match op {
        // -- fs ------------------------------------------------------------
        "fs.readFile" => {
            let path = node_arg_str(args, "path")?;
            let bytes = std::fs::read(&path).map_err(|e| NodeError::io(e, "open", &path))?;
            Ok(serde_json::json!({ "base64": base64_encode(&bytes) }))
        }
        "fs.writeFile" | "fs.appendFile" => {
            let path = node_arg_str(args, "path")?;
            let payload = node_arg_str(args, "base64")?;
            let bytes = base64_decode(&payload)
                .ok_or_else(|| NodeError::invalid("`base64` argument is not valid base64"))?;
            let append = op == "fs.appendFile" || node_arg_bool(args, "append");
            if append {
                use std::io::Write;
                let mut file = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)
                    .map_err(|e| NodeError::io(e, "open", &path))?;
                file.write_all(&bytes)
                    .map_err(|e| NodeError::io(e, "write", &path))?;
            } else {
                std::fs::write(&path, &bytes).map_err(|e| NodeError::io(e, "open", &path))?;
            }
            Ok(serde_json::Value::Null)
        }
        "fs.exists" => {
            let path = node_arg_str(args, "path")?;
            // `existsSync` follows symlinks (a dangling link is "missing").
            Ok(serde_json::json!(std::path::Path::new(&path).exists()))
        }
        "fs.readdir" => {
            let path = node_arg_str(args, "path")?;
            let entries =
                std::fs::read_dir(&path).map_err(|e| NodeError::io(e, "scandir", &path))?;
            let mut out = Vec::new();
            for entry in entries {
                let entry = entry.map_err(|e| NodeError::io(e, "scandir", &path))?;
                let file_type = entry
                    .file_type()
                    .map_err(|e| NodeError::io(e, "stat", &path))?;
                out.push(serde_json::json!({
                    "name": entry.file_name().to_string_lossy(),
                    "isFile": file_type.is_file(),
                    "isDirectory": file_type.is_dir(),
                    "isSymbolicLink": file_type.is_symlink(),
                }));
            }
            // Node returns entries in readdir order, which is unspecified
            // but stable per filesystem; sort by name so JS-visible order
            // does not depend on inode layout (matches `readdirSync` on
            // ext4 in practice and keeps tests deterministic).
            out.sort_by(|a, b| {
                a["name"]
                    .as_str()
                    .unwrap_or_default()
                    .cmp(b["name"].as_str().unwrap_or_default())
            });
            Ok(serde_json::Value::Array(out))
        }
        "fs.stat" | "fs.lstat" => {
            let path = node_arg_str(args, "path")?;
            let meta = if op == "fs.lstat" {
                std::fs::symlink_metadata(&path)
            } else {
                std::fs::metadata(&path)
            }
            .map_err(|e| NodeError::io(e, "stat", &path))?;
            Ok(stat_json(&meta))
        }
        "fs.mkdir" => {
            let path = node_arg_str(args, "path")?;
            let result = if node_arg_bool(args, "recursive") {
                std::fs::create_dir_all(&path)
            } else {
                std::fs::create_dir(&path)
            };
            result.map_err(|e| NodeError::io(e, "mkdir", &path))?;
            Ok(serde_json::Value::Null)
        }
        "fs.unlink" => {
            let path = node_arg_str(args, "path")?;
            std::fs::remove_file(&path).map_err(|e| NodeError::io(e, "unlink", &path))?;
            Ok(serde_json::Value::Null)
        }
        "fs.rmdir" => {
            let path = node_arg_str(args, "path")?;
            std::fs::remove_dir(&path).map_err(|e| NodeError::io(e, "rmdir", &path))?;
            Ok(serde_json::Value::Null)
        }
        "fs.rm" => {
            let path = node_arg_str(args, "path")?;
            let force = node_arg_bool(args, "force");
            let recursive = node_arg_bool(args, "recursive");
            let meta = std::fs::symlink_metadata(&path);
            match meta {
                Ok(meta) if meta.is_dir() => {
                    if recursive {
                        std::fs::remove_dir_all(&path)
                            .map_err(|e| NodeError::io(e, "rm", &path))?;
                    } else {
                        std::fs::remove_dir(&path).map_err(|e| NodeError::io(e, "rm", &path))?;
                    }
                }
                Ok(_) => {
                    std::fs::remove_file(&path).map_err(|e| NodeError::io(e, "rm", &path))?;
                }
                Err(e) if force && e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(NodeError::io(e, "rm", &path)),
            }
            Ok(serde_json::Value::Null)
        }
        "fs.rename" => {
            let from = node_arg_str(args, "from")?;
            let to = node_arg_str(args, "to")?;
            std::fs::rename(&from, &to).map_err(|e| NodeError::io(e, "rename", &from))?;
            Ok(serde_json::Value::Null)
        }
        "fs.copyFile" => {
            let from = node_arg_str(args, "from")?;
            let to = node_arg_str(args, "to")?;
            std::fs::copy(&from, &to).map_err(|e| NodeError::io(e, "copyfile", &from))?;
            Ok(serde_json::Value::Null)
        }
        "fs.realpath" => {
            let path = node_arg_str(args, "path")?;
            let resolved =
                std::fs::canonicalize(&path).map_err(|e| NodeError::io(e, "realpath", &path))?;
            Ok(serde_json::json!(resolved.to_string_lossy()))
        }

        // -- os ------------------------------------------------------------
        "os.homedir" => Ok(serde_json::json!(home_dir())),
        "os.tmpdir" => Ok(serde_json::json!(tmp_dir())),
        "os.platform" => Ok(serde_json::json!(host_platform())),
        "os.arch" => Ok(serde_json::json!(std::env::consts::ARCH)),
        "os.type" => Ok(serde_json::json!(host_os_type())),
        "os.eol" => Ok(serde_json::json!(host_eol())),
        "os.hostname" => Ok(serde_json::json!(std::fs::read_to_string("/etc/hostname")
            .map(|name| name.trim().to_string())
            .unwrap_or_else(|_| "localhost".to_string()))),
        "os.release" => Ok(serde_json::json!(uname_release())),

        // -- process -------------------------------------------------------
        "process.env" => {
            let mut map = serde_json::Map::new();
            for (key, value) in std::env::vars() {
                map.insert(key, serde_json::Value::String(value));
            }
            Ok(serde_json::Value::Object(map))
        }
        "process.cwd" => Ok(serde_json::json!(std::env::current_dir()
            .map(|dir| dir.to_string_lossy().to_string())
            .unwrap_or_else(|_| "/".to_string()))),
        "process.platform" => Ok(serde_json::json!(host_platform())),
        "process.arch" => Ok(serde_json::json!(std::env::consts::ARCH)),
        "process.pid" => Ok(serde_json::json!(std::process::id())),

        // -- crypto --------------------------------------------------------
        "crypto.randomBytes" => {
            let count = args
                .get("count")
                .and_then(|value| value.as_u64())
                .ok_or_else(|| NodeError::invalid("missing numeric argument `count`"))?;
            let mut bytes = vec![0u8; count as usize];
            getrandom_fill(&mut bytes)?;
            Ok(serde_json::json!({ "base64": base64_encode(&bytes) }))
        }

        "crypto.digest" => {
            // `crypto.subtle.digest` / `createHash` symmetric backing: the
            // shim buffers the message and asks for one digest. See
            // `crate::digest` for why the algorithms are hand-rolled.
            let argument = node_arg_str(args, "algorithm")?;
            let algorithm = crate::digest::Algorithm::parse(&argument).ok_or_else(|| {
                NodeError::invalid(format!("unsupported digest algorithm `{argument}`"))
            })?;
            let bytes = node_arg_bytes(args, "base64")?;
            Ok(serde_json::json!({
                "base64": base64_encode(&crate::digest::digest(algorithm, &bytes))
            }))
        }

        "crypto.hmac" => {
            // `crypto.createHmac` backing: the shim buffers the message and
            // the key, the host computes one RFC 2104 HMAC on top of the
            // same hand-rolled SHA-1 / SHA-256 primitives as `crypto.digest`.
            let argument = node_arg_str(args, "algorithm")?;
            let algorithm = crate::digest::Algorithm::parse(&argument).ok_or_else(|| {
                NodeError::invalid(format!("unsupported hmac algorithm `{argument}`"))
            })?;
            let key = node_arg_bytes(args, "keyBase64")?;
            let bytes = node_arg_bytes(args, "base64")?;
            Ok(serde_json::json!({
                "base64": base64_encode(&crate::digest::hmac(algorithm, &key, &bytes))
            }))
        }

        // -- zlib ------------------------------------------------------------
        //
        // Two compression backends: the zstd family (the workspace already
        // bundles `zstd = 0.13` for `pi-session`) and the gzip/deflate
        // family (the pure-Rust RFC 1951/1950/1952 codec in
        // `crate::deflate` — the offline registry has no `flate2` /
        // `miniz_oxide`). `crc32` is shared by the shim and the gzip
        // trailer.
        "zlib.zstdCompress" => {
            let bytes = node_arg_bytes(args, "base64")?;
            // Node's default for `zstdCompressSync` is zstd's default (3).
            let level = match args.get("level") {
                Some(value) => value
                    .as_i64()
                    .and_then(|value| i32::try_from(value).ok())
                    .ok_or_else(|| NodeError::invalid("`level` argument is out of range"))?,
                None => zstd::DEFAULT_COMPRESSION_LEVEL,
            };
            let compressed = zstd::stream::encode_all(std::io::Cursor::new(bytes), level)
                .map_err(|e| zstd_error(&e))?;
            Ok(serde_json::json!({ "base64": base64_encode(&compressed) }))
        }
        "zlib.zstdDecompress" => {
            let bytes = node_arg_bytes(args, "base64")?;
            let decoded = zstd::stream::decode_all(std::io::Cursor::new(bytes))
                .map_err(|e| zstd_error(&e))?;
            Ok(serde_json::json!({ "base64": base64_encode(&decoded) }))
        }
        "zlib.deflate" => {
            let bytes = node_arg_bytes(args, "base64")?;
            let level = node_arg_level(args)?;
            let compressed = deflate::zlib_compress(&bytes, level);
            Ok(serde_json::json!({ "base64": base64_encode(&compressed) }))
        }
        "zlib.inflate" => {
            let bytes = node_arg_bytes(args, "base64")?;
            let decoded = deflate::zlib_decompress(&bytes).map_err(zlib_error)?;
            Ok(serde_json::json!({ "base64": base64_encode(&decoded) }))
        }
        "zlib.deflateRaw" => {
            let bytes = node_arg_bytes(args, "base64")?;
            let level = node_arg_level(args)?;
            let compressed = deflate::deflate_raw(&bytes, level);
            Ok(serde_json::json!({ "base64": base64_encode(&compressed) }))
        }
        "zlib.inflateRaw" => {
            let bytes = node_arg_bytes(args, "base64")?;
            let decoded = deflate::inflate_raw(&bytes).map_err(zlib_error)?;
            Ok(serde_json::json!({ "base64": base64_encode(&decoded) }))
        }
        "zlib.gzip" => {
            let bytes = node_arg_bytes(args, "base64")?;
            let level = node_arg_level(args)?;
            let compressed = deflate::gzip_compress(&bytes, level);
            Ok(serde_json::json!({ "base64": base64_encode(&compressed) }))
        }
        "zlib.gunzip" => {
            let bytes = node_arg_bytes(args, "base64")?;
            let decoded = deflate::gzip_decompress(&bytes).map_err(zlib_error)?;
            Ok(serde_json::json!({ "base64": base64_encode(&decoded) }))
        }
        "zlib.crc32" => {
            let bytes = node_arg_bytes(args, "base64")?;
            // Node's second argument is the *previous* (finalised) CRC, so a
            // chained call continues the same stream.
            let value = args
                .get("value")
                .and_then(|value| value.as_u64())
                .unwrap_or(0) as u32;
            Ok(serde_json::json!({ "value": crc32(&bytes, value) }))
        }

        other => Err(NodeError::new(
            "ERR_UNSUPPORTED_OPERATION",
            format!("node bridge op `{other}` is not implemented"),
        )),
    }
}

/// `os.platform()` / `process.platform` — Node names the platforms
/// `linux` / `darwin` / `win32`, which differs from `std`'s
/// `linux` / `macos` / `windows`.
fn host_platform() -> &'static str {
    match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    }
}

/// `os.type()` — Node's uname-style OS name.
fn host_os_type() -> &'static str {
    match std::env::consts::OS {
        "macos" => "Darwin",
        "windows" => "Windows_NT",
        "linux" => "Linux",
        other => other,
    }
}

/// `os.EOL`.
fn host_eol() -> &'static str {
    if cfg!(windows) {
        "\r\n"
    } else {
        "\n"
    }
}

/// `os.homedir()` — `$HOME`, falling back to the passwd entry via
/// `$USER` is not available here, so `/root` is the last resort.
fn home_dir() -> String {
    std::env::var("HOME")
        .ok()
        .filter(|home| !home.is_empty())
        .unwrap_or_else(|| "/root".to_string())
}

/// `os.tmpdir()` — `$TMPDIR`, then `/tmp`.
fn tmp_dir() -> String {
    std::env::var("TMPDIR")
        .ok()
        .filter(|dir| !dir.is_empty())
        .unwrap_or_else(|| "/tmp".to_string())
}

/// `os.release()` — best-effort `uname -r` value. The host is
/// Linux-only (the embedded QuickJS host is native-only), so reading
/// the paired `version` file is enough.
fn uname_release() -> String {
    std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .map(|release| release.trim().to_string())
        .unwrap_or_else(|_| "unknown".to_string())
}

/// Fill `out` from the OS entropy pool. `/dev/urandom` is the portable
/// Linux source and the only one this host needs; there is no fallback
/// PRNG on purpose — a weak `randomUUID` would be worse than an error.
fn getrandom_fill(out: &mut [u8]) -> Result<(), NodeError> {
    use std::io::Read;
    let mut file = std::fs::File::open("/dev/urandom")
        .map_err(|e| NodeError::io(e, "open", "/dev/urandom"))?;
    file.read_exact(out)
        .map_err(|e| NodeError::io(e, "read", "/dev/urandom"))
}

const BASE64_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 (RFC 4648, with padding). Hand-rolled to keep
/// `pi-extensions` dependency-free: the workspace has no base64 direct
/// dependency and the shim needs the same primitives on the JS side.
fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(BASE64_ALPHABET[((triple >> 18) & 0x3f) as usize] as char);
        out.push(BASE64_ALPHABET[((triple >> 12) & 0x3f) as usize] as char);
        if chunk.len() > 1 {
            out.push(BASE64_ALPHABET[((triple >> 6) & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(BASE64_ALPHABET[(triple & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

/// Inverse of [`base64_encode`]. Returns `None` on any malformed input
/// so the caller can raise `EINVAL` instead of silently truncating.
fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let mut buffer = 0u32;
    let mut bits = 0u32;
    let mut padding = 0usize;
    for byte in text.bytes() {
        if byte == b'=' {
            padding += 1;
            continue;
        }
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'\n' | b'\r' | b' ' | b'\t' => continue,
            _ => return None,
        };
        if padding > 0 {
            return None;
        }
        buffer = (buffer << 6) | value as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
        }
    }
    (padding <= 2).then_some(out)
}

/// Build the `NodeError` for a failed gzip/deflate call. The codec already
/// reports Node's `Z_DATA_ERROR` / `Z_BUF_ERROR` / `Z_STREAM_ERROR` names.
fn zlib_error(err: deflate::ZlibError) -> NodeError {
    NodeError::new(err.code, err.message)
}

/// Read Node's optional `options.level`. `-1` is `Z_DEFAULT_COMPRESSION`;
/// anything outside `-1..=9` is a `Z_STREAM_ERROR` (the shim raises Node's
/// `RangeError` before reaching here, this is the Rust-side backstop).
fn node_arg_level(args: &serde_json::Value) -> Result<Option<i32>, NodeError> {
    match args.get("level") {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(value) => value
            .as_i64()
            .and_then(|level| i32::try_from(level).ok())
            .filter(|level| (-1..=9).contains(level))
            .map(Some)
            .ok_or_else(|| NodeError::new("Z_STREAM_ERROR", "invalid compression level")),
    }
}

/// Build the `NodeError` for a failed zstd call. The `zstd` crate wraps
/// the raw error code in an `io::Error` whose message is
/// `ZSTD_getErrorName(code)`; the code is recovered from that prose so
/// `err.code` matches Node's `ZSTD_ErrorCode` names.
fn zstd_error(err: &std::io::Error) -> NodeError {
    let name = err.to_string();
    NodeError::new(zstd_error_code(&name), format!("ZSTD: {name}"))
}

/// `ZSTD_getErrorName()` prose → Node's `ZSTD_error_*` code. Node reports
/// the enum name (a corrupt frame is `ZSTD_error_prefix_unknown`), while
/// the `zstd` crate only exposes the C error string; the realistic decode
/// failures are mapped back here and anything else falls back to
/// `ZSTD_error_GENERIC` rather than inventing a code.
fn zstd_error_code(message: &str) -> &'static str {
    match message {
        "Unknown frame descriptor" => "ZSTD_error_prefix_unknown",
        "Data corruption detected" => "ZSTD_error_corruption_detected",
        "Restored data doesn't match checksum" => "ZSTD_error_checksum_wrong",
        "Src size is incorrect" => "ZSTD_error_srcSize_wrong",
        "Destination buffer is too small" => "ZSTD_error_dstSize_tooSmall",
        "Unsupported frame parameter" => "ZSTD_error_frameParameter_unsupported",
        "Version not supported" => "ZSTD_error_version_unsupported",
        _ => "ZSTD_error_GENERIC",
    }
}
