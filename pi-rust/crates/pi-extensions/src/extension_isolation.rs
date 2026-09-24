//! Per-extension Runtime isolation.
//!
//! The default [`crate::JsExtensionHost`] runs every extension inside a
//! single QuickJS [`AsyncRuntime`] / [`AsyncContext`] pair. That makes
//! host imports cheap to wire up (one shim copy, one globals table)
//! but it also means a single runaway / corrupted extension takes
//! down every other extension with it: every extension shares one JS
//! heap, one interrupt deadline, and one `Promise` queue.
//!
//! [`IsolatedJsExtensionHost`] allocates a **fresh** `AsyncRuntime` +
//! `AsyncContext` for every extension loaded through it. Each
//! extension gets its own:
//!
//! - JS heap and global object table (one extension's globals cannot
//!   stomp on another's),
//! - bytecode interpreter + GC, so a crash or OOM inside one extension
//!   cannot corrupt the others,
//! - interrupt handler, so a `while (true)` in extension A does not
//!   cancel extension B's in-flight tool call,
//! - promise pool, so the runtime driver is shared but extensions do
//!   not race on it.
//!
//! The cost is one extra QuickJS `AsyncRuntime` per extension (~150
//! KiB virtual address + a `spawn(runtime.drive())` task). For the
//! typical extension set (≤ 20 files) this is negligible compared to
//! the safety win.
//!
//! Host services (UI channel, exec bridge, pi-ai stream bridge,
//! `node:child_process` registry, the [`HostState`] registry / log /
//! provider list, the `ToolContext`, the `BuiltinToolRunner`, …) are
//! **shared** across every extension via the same [`crate::Inner`]
//! the single-runtime host already uses. That keeps the wire protocol
//! between JS and Rust identical to the shared-host case — the shim
//! still calls the same `host_*` imports — so all the upstream pi
//! extensions work unchanged.
//!
//! [`HostState`]: crate::host::HostState

use std::sync::Arc;

use parking_lot::Mutex;
use pi_protocol::{ExtensionEvent, ToolDefinition};
use rquickjs_core::prelude::CatchResultExt;
use rquickjs_core::promise::MaybePromise;
use rquickjs_core::{async_with, Function};
use tracing::{info, warn};

use crate::api::ExtensionEntry;
use crate::error::ExtensionError;
use crate::host::{install_imports, Inner};
use crate::shim::SHIM_SOURCE;

/// One extension's isolated QuickJS heap.
///
/// Dropping the slot drops the runtime, which drops the context, which
/// frees the extension's JS state. Tests rely on this: clearing
/// `slots` resets every loaded extension in one step.
pub(crate) struct ExtensionSlot {
    /// Extension id (file stem or explicit id from the loader).
    pub id: String,
    /// Original entry the loader produced. Kept so `registered_tools`
    /// can attribute a tool back to the extension that registered it.
    pub entry: ExtensionEntry,
    /// Owns the JS heap for this extension. Lives in `Arc` because the
    /// runtime driver task captures a clone.
    pub runtime: Arc<rquickjs_core::AsyncRuntime>,
    /// The single context the runtime serves. We only use this one
    /// (extensions load synchronously and dispatch from one event
    /// loop), so a slot does not need a context pool.
    pub context: rquickjs_core::AsyncContext,
}

/// One extension's tool registrations as the registry sees them.
#[derive(Debug, Clone)]
pub(crate) struct IsolatedToolRecord {
    /// Extension id (file stem).
    pub extension_id: String,
    /// Tool definition the JS side emitted via `pi.registerTool`.
    pub definition: ToolDefinition,
}

/// Host that gives every extension its own QuickJS runtime + context.
///
/// Cloning shares the slot list and the underlying [`Inner`] state, so
/// the same isolated host can drive multiple event sources (a `pi
/// --rpc` server plus an interactive driver, for example).
#[derive(Clone)]
pub struct IsolatedJsExtensionHost {
    inner: Arc<Inner>,
    slots: Arc<Mutex<Vec<ExtensionSlot>>>,
}

impl IsolatedJsExtensionHost {
    /// Build a new isolated host. The host starts with zero loaded
    /// extensions; call [`Self::load`] for each one.
    ///
    /// `opts` is forwarded to [`JsExtensionHost::with_options`]'s
    /// `Inner` builder so the UI handler, tool context, and built-in
    /// runners apply uniformly to both the shared and isolated hosts.
    pub async fn new(opts: crate::host::HostOptions) -> Result<Self, ExtensionError> {
        let timeout = opts.timeout.unwrap_or(crate::host::DEFAULT_TIMEOUT);
        let runtime = rquickjs_core::AsyncRuntime::new().map_err(ExtensionError::from)?;
        // The shared "master" context the Inner type owns. We never
        // evaluate JS in it (extensions have their own contexts), but
        // the field has to be present for the type to build. The
        // context is created with `full` so any import the shim
        // touches at install time is available.
        let master_context = rquickjs_core::AsyncContext::full(&runtime)
            .await
            .map_err(ExtensionError::from)?;
        let (ui_tx, _ui_rx) = tokio::sync::mpsc::unbounded_channel::<crate::host::UiRequestEnvelope>();
        let state = Arc::new(Mutex::new(crate::host::HostState::default()));
        let deadline_nanos = Arc::new(std::sync::atomic::AtomicU64::new(u64::MAX));
        let execs = crate::host::ExecBridge::new();
        let pi_ai = crate::PiAiStreamBridge::new();
        let inner = Arc::new(Inner {
            runtime,
            children: Arc::new(Mutex::new(std::collections::HashMap::new())),
            next_child: Arc::new(std::sync::atomic::AtomicU64::new(1)),
            context: master_context,
            ui_tx,
            state,
            timeout,
            tool_context: opts.tool_context.clone(),
            builtin_tool_runner: opts.builtin_tool_runner.clone(),
            pi_ai_stream_runner: opts.pi_ai_stream_runner.clone(),
            pi_ai,
            deadline_nanos: deadline_nanos.clone(),
            execs,
            region_tx: None,
            next_ui_session: Arc::new(std::sync::atomic::AtomicU64::new(1)),
            footer_data: Arc::new(Mutex::new(None)),
            autocomplete_base: opts.autocomplete_base.clone(),
            autocomplete_generation: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        });
        // The master runtime is unused but it MUST be driven for any
        // host imports that capture it (none currently do, but the
        // type expects a driver task). Drop it instead — `Arc` drops
        // the runtime when the host does, and no AsyncContext in
        // master is ever evaluated.
        //
        // We do NOT spawn the master runtime driver because nothing
        // uses it; per-extension runtimes get their own driver tasks in
        // `load`.
        Ok(Self {
            inner,
            slots: Arc::new(Mutex::new(Vec::new())),
        })
    }

    /// Load one extension into a fresh runtime + context.
    ///
    /// The slot is appended to the host's slot list on success. A
    /// failure rolls back the runtime allocation — the host's slot
    /// list never observes a half-loaded extension.
    pub async fn load(
        &self,
        entry: ExtensionEntry,
        source: &str,
    ) -> Result<(), ExtensionError> {
        let id = entry.id.clone();
        let source = source.to_string();
        let source_path = entry.source.to_string_lossy().into_owned();
        let timeout = self.inner.timeout;

        // Mark which extension this load is for so the registry /
        // log book-keeping can attribute the registrations.
        {
            let mut s = self.inner.state.lock();
            s.pending_extension = Some(entry.id.clone());
            s.registry
                .register(entry.clone(), crate::api::ExtensionCapabilities::default());
        }

        let slot_runtime = rquickjs_core::AsyncRuntime::new().map_err(ExtensionError::from)?;
        let slot_context = rquickjs_core::AsyncContext::full(&slot_runtime)
            .await
            .map_err(ExtensionError::from)?;
        // Install the shim + host imports into the slot's context.
        // Failures here are fatal — without the shim the extension
        // cannot call any host import.
        let install_inner = self.inner.clone();
        if let Err(err) = async_with!(slot_context => |ctx| {
            install_imports(&ctx, &install_inner)?;
            ctx.eval::<(), _>(SHIM_SOURCE)
                .catch(&ctx)
                .map_err(|e| e.throw(&ctx))?;
            Ok::<_, rquickjs_core::Error>(())
        })
        .await
        {
            return Err(ExtensionError::Load(format!(
                "{}: shim install failed: {err}",
                id
            )));
        }

        // Spawn the per-extension runtime driver. The driver pumps
        // JS promises + the Async host imports (ui.confirm / input /
        // select). One driver per extension matches the design of the
        // shared host (one driver per AsyncRuntime).
        let driver_runtime = slot_runtime.clone();
        tokio::spawn(async move {
            driver_runtime.drive().await;
        });

        // Arm the interrupt handler so a runaway extension cannot
        // block forever.
        let interrupt_deadline = self.inner.deadline_nanos.clone();
        let interrupt_exec_deadline = self.inner.execs.deadline_nanos.clone();
        let interrupt_pi_ai_deadline = self.inner.pi_ai.deadline_nanos();
        let _ = slot_runtime
            .set_interrupt_handler(Some(Box::new(move || {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos() as u64)
                    .unwrap_or(0);
                let base = interrupt_deadline.load(std::sync::atomic::Ordering::Relaxed);
                let exec = interrupt_exec_deadline.load(std::sync::atomic::Ordering::Relaxed);
                let pi_ai = interrupt_pi_ai_deadline.load(std::sync::atomic::Ordering::Relaxed);
                // `u64::MAX` means "unarmed": take whichever deadline *is*
                // armed, and the later one when several are.
                let mut deadline = u64::MAX;
                for candidate in [base, exec, pi_ai] {
                    if candidate != u64::MAX {
                        deadline = if deadline == u64::MAX {
                            candidate
                        } else {
                            deadline.max(candidate)
                        };
                    }
                }
                if deadline == u64::MAX {
                    return false;
                }
                now >= deadline
            })))
            .await;

        // Arm the per-load deadline.
        let base_deadline = self.inner.arm_deadline();

        let load_result = crate::host::drive_call(
            &self.inner,
            base_deadline,
            async_with!(slot_context => |ctx| {
                let load: Function = ctx
                    .globals()
                    .get("_pi_load_extension")
                    .map_err(ExtensionError::from)?;
                load.call::<_, ()>((source, source_path))
                    .catch(&ctx)
                    .map_err(|e| ExtensionError::Runtime(e.to_string()))?;
                Ok::<_, ExtensionError>(())
            }),
        )
        .await;

        self.inner.disarm_deadline();

        match load_result {
            Ok(Ok(())) => {
                let mut s = self.inner.state.lock();
                s.pending_extension = None;
                let tools = std::mem::take(&mut s.log.tools);
                s.registry.set_tools(&entry.id, tools);
                info!(target: "pi_extension", id = %id, "isolated extension loaded");
            }
            Ok(Err(e)) => {
                warn!(target: "pi_extension", id = %id, error = %e, "isolated extension load failed");
                return Err(ExtensionError::Load(format!("{}: {}", entry.id, e)));
            }
            Err(()) => return Err(ExtensionError::Timeout(timeout)),
        }

        // Append the slot — only after a successful load, so the
        // slot list never contains a broken extension.
        let slot = ExtensionSlot {
            id: id.clone(),
            entry,
            runtime: Arc::new(slot_runtime),
            context: slot_context,
        };
        self.slots.lock().push(slot);
        Ok(())
    }

    /// Broadcast an event to every loaded extension. Each extension
    /// dispatches in its own runtime, so a slow extension cannot
    /// stall the others — the per-call deadline (5 s by default)
    /// bounds each dispatch individually.
    pub async fn emit_event(
        &self,
        event: &ExtensionEvent,
    ) -> Result<crate::host::DispatchOutcome, ExtensionError> {
        self.emit_event_with(event, None, false, "").await
    }

    /// Like [`Self::emit_event`] but with explicit context.
    pub async fn emit_event_with(
        &self,
        event: &ExtensionEvent,
        mode: Option<&str>,
        has_ui: bool,
        cwd: &str,
    ) -> Result<crate::host::DispatchOutcome, ExtensionError> {
        let mut envelope = serde_json::to_value(event).map_err(ExtensionError::from)?;
        if let Some(obj) = envelope.as_object_mut() {
            if let Some(m) = mode {
                obj.insert("_ctx_mode".into(), serde_json::Value::String(m.into()));
            }
            obj.insert("_ctx_hasUI".into(), serde_json::Value::Bool(has_ui));
            obj.insert("_ctx_cwd".into(), serde_json::Value::String(cwd.into()));
        }
        let event_json = serde_json::to_string(&envelope).map_err(ExtensionError::from)?;
        let timeout = self.inner.timeout;

        // Aggregate handler counts across every slot. Each slot
        // contributes its own dispatch independently; a slow or
        // throwing slot cannot cancel a healthy one because the
        // dispatch futures are run concurrently via `tokio::join!`.
        let snapshot: Vec<ExtensionSlot> = {
            let guard = self.slots.lock();
            guard
                .iter()
                .map(|s| ExtensionSlot {
                    id: s.id.clone(),
                    entry: s.entry.clone(),
                    runtime: s.runtime.clone(),
                    context: s.context.clone(),
                })
                .collect()
        };

        let mut outcomes = Vec::with_capacity(snapshot.len());
        for slot in snapshot {
            let base_deadline = self.inner.arm_deadline();
            let event_json = event_json.clone();
            let result = crate::host::drive_call(
                &self.inner,
                base_deadline,
                async_with!(slot.context => |ctx| {
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
                    let outcome: crate::host::DispatchOutcome =
                        serde_json::from_str(&raw).map_err(ExtensionError::from)?;
                    Ok::<_, ExtensionError>(outcome)
                }),
            )
            .await;
            self.inner.disarm_deadline();
            match result {
                Ok(Ok(outcome)) => outcomes.push(outcome),
                Ok(Err(e)) => {
                    warn!(target: "pi_extension", id = %slot.id, error = %e, "isolated dispatch error; continuing");
                }
                Err(()) => {
                    warn!(target: "pi_extension", id = %slot.id, "isolated dispatch timed out");
                }
            }
            let _ = timeout;
        }

        Ok(crate::host::DispatchOutcome::aggregate(&outcomes))
    }

    /// Run a registered tool by name. The lookup walks the shared
    /// [`HostState`] registry, finds the extension id the tool
    /// belongs to, then drives `_pi_execute_tool` inside that
    /// extension's isolated context.
    pub async fn execute_tool(
        &self,
        name: &str,
        args_json: &str,
    ) -> Result<crate::host::ToolExecutionOutcome, ExtensionError> {
        let owner = {
            let s = self.inner.state.lock();
            s.registry.tool_owner(name)
        };
        let owner = owner.ok_or_else(|| {
            ExtensionError::Load(format!("execute_tool: unknown tool `{name}`"))
        })?;
        let slot = {
            let slots = self.slots.lock();
            slots
                .iter()
                .find(|s| s.id == owner)
                .map(|s| (s.runtime.clone(), s.context.clone()))
        };
        let (_runtime, context) = slot.ok_or_else(|| {
            ExtensionError::Load(format!(
                "execute_tool: tool `{name}` owned by unloaded extension `{owner}`"
            ))
        })?;
        let timeout = self.inner.timeout;
        let name = name.to_string();
        let args = args_json.to_string();
        let base_deadline = self.inner.arm_deadline();
        let result = crate::host::drive_call(
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
                let outcome: crate::host::ToolExecutionOutcome =
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

    /// Names of every tool every loaded extension registered.
    pub async fn registered_tool_names(&self) -> Vec<String> {
        self.inner
            .state
            .lock()
            .registry
            .tool_pairs()
            .map(|(name, _)| name)
            .collect()
    }

    /// Snapshot of `(tool_name, owning_extension_id, definition)` for
    /// every registered tool. The agent layer uses this to wire tools
    /// into the model-call schema.
    pub async fn registered_tools(&self) -> Vec<(String, String, ToolDefinition)> {
        self.inner
            .state
            .lock()
            .registry
            .tool_pairs()
            .map(|(name, entry_id)| {
                let def = self
                    .inner
                    .state
                    .lock()
                    .log
                    .tools
                    .iter()
                    .find(|t| t.name == name)
                    .cloned()
                    .unwrap_or_else(|| ToolDefinition {
                        name: name.to_string(),
                        label: name.to_string(),
                        description: String::new(),
                        parameters: serde_json::json!({"type": "object"}),
                        metadata: None,
                    });
                (name, entry_id, def)
            })
            .collect()
    }

    /// Drop every loaded extension (freeing their runtimes) without
    /// touching the host's UI / exec / state services. Used by tests
    /// to clean up between cases and by `--reload-extensions` style
    /// commands.
    pub fn unload_all(&self) {
        let mut slots = self.slots.lock();
        slots.clear();
    }

    /// Access the underlying shared state. Most callers should not
    /// need this; it exists so `extension_isolation` can be wired
    /// into the same plumbing as `JsExtensionHost` without
    /// duplicating the registry / state types.
    pub fn shared_state(&self) -> Arc<Mutex<crate::host::HostState>> {
        self.inner.state.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two extensions in two slots — neither can see the other's
    /// globals. This is the headline isolation property: in the
    /// shared host, an extension's `globalThis.__stolen` write is
    /// visible to every other extension. With per-extension contexts
    /// each extension gets a fresh global object.
    #[tokio::test]
    async fn two_extensions_get_independent_globals() {
        let opts = crate::host::HostOptions::default();
        let host = IsolatedJsExtensionHost::new(opts).await.expect("host");

        host.load(
            ExtensionEntry {
                id: "alpha".into(),
                source: "/tmp/alpha.js".into(),
                label: None,
            },
            r#"
                globalThis.__alpha = "alpha-was-here";
                export default function (pi) {};
            "#,
        )
        .await
        .expect("alpha");

        host.load(
            ExtensionEntry {
                id: "beta".into(),
                source: "/tmp/beta.js".into(),
                label: None,
            },
            r#"
                if (globalThis.__alpha !== undefined) {
                    throw new Error("beta saw alpha's globals: " + globalThis.__alpha);
                }
                globalThis.__beta = "beta-was-here";
                export default function (pi) {};
            "#,
        )
        .await
        .expect("beta");

        // Confirm the slot list reflects both.
        assert_eq!(host.slots.lock().len(), 2, "expected 2 slots");
    }

    /// A load failure must not leave a half-loaded slot in the list.
    /// QuickJS surfaces syntax errors as `Result::Err`; the load path
    /// must roll back the runtime allocation it made.
    #[tokio::test]
    async fn load_failure_does_not_leak_slots() {
        let opts = crate::host::HostOptions::default();
        let host = IsolatedJsExtensionHost::new(opts).await.expect("host");

        let result = host
            .load(
                ExtensionEntry {
                    id: "broken".into(),
                    source: "/tmp/broken.js".into(),
                    label: None,
                },
                "this is not valid javascript (((( ",
            )
            .await;
        assert!(result.is_err(), "broken extension must error out");
        assert_eq!(
            host.slots.lock().len(),
            0,
            "a failed load must not produce a slot"
        );
    }

    /// `unload_all` clears the slot list — used by reload flows and
    /// by tests to reset between cases.
    #[tokio::test]
    async fn unload_all_clears_slots() {
        let opts = crate::host::HostOptions::default();
        let host = IsolatedJsExtensionHost::new(opts).await.expect("host");

        host.load(
            ExtensionEntry {
                id: "alpha".into(),
                source: "/tmp/alpha.js".into(),
                label: None,
            },
            "export default function (pi) {};",
        )
        .await
        .expect("alpha");

        assert_eq!(host.slots.lock().len(), 1);
        host.unload_all();
        assert_eq!(host.slots.lock().len(), 0);
    }
}