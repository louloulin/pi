//! Signal combination — port of `packages/ai/src/utils/abort-signals.ts`.
//!
//! Providers compose several cancellation sources into one signal they can
//! hand to a single request: the caller's `options.signal` plus a local
//! timeout controller (upstream's Azure / OpenAI-Codex paths), where the
//! request must stop as soon as *any* source fires.
//!
//! # Deliberate differences from the TypeScript implementation
//!
//! * **Native only.** Combining needs a parent signal this module can
//!   trigger, plus a way to subscribe to each child. The native
//!   [`AbortSignal`] wraps a `CancellationToken`, so a parent is cancellable
//!   and each child is observed by awaiting its `cancelled()` future in a
//!   freshly spawned task. The WASM `AbortSignal` is a bare flag that only the
//!   JS host may set and exposes no `cancelled()` await, so there is no
//!   subscription to register (and no runtime task to spawn); the module is
//!   therefore compiled out on `wasm32` rather than silently returning a
//!   parent that can never fire. WASM callers must poll
//!   [`AbortSignal::is_cancelled`] of each source themselves, the same
//!   degradation the crate already documents for its WASM cancellation model.
//! * **No cancellation reason.** Upstream forwards `signal.reason` into
//!   `controller.abort(reason)`; Rust [`AbortSignal`] carries no reason, so the
//!   parent is simply cancelled. Consumers see `StreamError::Aborted`, which
//!   is the only "aborted" shape the crate models.
//! * **`cleanup` takes `&self` and is idempotent.** Upstream returns a
//!   `cleanup: () => void` closure; here every listener is an `AbortHandle`,
//!   which can be aborted through a shared reference, so cleanup is a plain
//!   method and may be called more than once. The borrowed shape also means
//!   the combined signal stays usable until the caller drops it.
//! * **Requires a tokio runtime.** Registering each child spawns a small
//!   listener task, exactly like upstream's `addEventListener`; calling this
//!   outside a tokio runtime panics. Combined signals are built inside
//!   provider request paths, which are always async.

use tokio_util::sync::CancellationToken;

use crate::types::AbortSignal;

/// `CombinedAbortSignal` — the parent signal plus the cleanup that
/// unsubscribes every listener registered for it.
pub struct CombinedAbortSignal {
    signal: Option<AbortSignal>,
    listeners: Vec<tokio::task::AbortHandle>,
}

impl CombinedAbortSignal {
    /// The combined signal, or `None` when no input signal was active
    /// (upstream's absent `signal` property).
    pub fn signal(&self) -> Option<&AbortSignal> {
        self.signal.as_ref()
    }

    /// `cleanup()` — stop observing the child signals.
    ///
    /// Aborts the listener tasks registered when the parent was built. Idempotent
    /// and safe to call after the parent fired; a no-op for the zero- and
    /// one-signal cases, which register nothing.
    pub fn cleanup(&self) {
        for listener in &self.listeners {
            listener.abort();
        }
    }
}

impl std::fmt::Debug for CombinedAbortSignal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CombinedAbortSignal")
            .field("signal", &self.signal)
            .field("listeners", &self.listeners.len())
            .finish()
    }
}

/// `combineAbortSignals(signals)` — one signal that fires when any of
/// `signals` fires.
///
/// * no active signal → no signal, nothing to clean up;
/// * exactly one → that signal is returned as-is (upstream does the same: no
///   wrapper is worth allocating for a single source);
/// * two or more → a fresh parent `CancellationToken`, with one listener task
///   per child. An already-cancelled child cancels the parent immediately and
///   stops the registration walk, exactly like upstream's early `break`.
pub fn combine_abort_signals(signals: &[Option<AbortSignal>]) -> CombinedAbortSignal {
    let active: Vec<&AbortSignal> = signals.iter().flatten().collect();

    match active.len() {
        0 => CombinedAbortSignal {
            signal: None,
            listeners: Vec::new(),
        },
        1 => CombinedAbortSignal {
            signal: Some(active[0].clone()),
            listeners: Vec::new(),
        },
        _ => {
            let token = CancellationToken::new();
            let parent = AbortSignal::native(token.clone());
            let mut listeners = Vec::new();

            for child in active {
                if child.is_cancelled() {
                    token.cancel();
                    break;
                }
                let child = child.clone();
                let token = token.clone();
                let listener = tokio::spawn(async move {
                    child.cancelled().await;
                    token.cancel();
                });
                listeners.push(listener.abort_handle());
            }

            CombinedAbortSignal {
                signal: Some(parent),
                listeners,
            }
        }
    }
}
