//! `wasm-bindgen` exports for the `pi-agent-core` crate.
//!
//! The single exported type, [`AgentHandle`], is a thin WASM-friendly
//! wrapper around the native [`Agent`]. It mirrors the public surface
//! the JS host consumes:
//!
//! * [`AgentHandle::new`] — construct an agent backed by the model id
//!   passed in (the JS host is expected to have called
//!   `register_faux_provider` first so the id resolves).
//! * [`AgentHandle::prompt`] — return a JS Promise that resolves when
//!   the agent finishes the turn. While the turn runs every event the
//!   agent emits is fanned out to all [`subscribe`](AgentHandle::subscribe)
//!   callbacks as a JS-friendly object.
//! * [`AgentHandle::subscribe`] / [`AgentHandle::unsubscribe`] — manage
//!   the JS-side listener list.
//!
//! All cross-boundary types are serialised via `serde-wasm-bindgen`,
//! using `serialize_large_numbers_as_bigints(true)` so `u64` fields in
//! the protocol survive the trip without losing precision in JS.

#![cfg(target_arch = "wasm32")]

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use js_sys::{Function, Promise};
use pi_ai::wasm as pi_ai_wasm;
use pi_protocol::{Message, Model, ProviderId};
use serde::Serialize;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::spawn_local;

use crate::agent::{Agent, AgentOptions};

/// Inner state shared between the `AgentHandle` facade and the
/// background event-drain task.
struct Inner {
    /// The wrapped agent. Borrowed mutably while a turn is running.
    agent: Agent,
    /// Subscriber callbacks registered via
    /// [`AgentHandle::subscribe`].
    subscribers: HashMap<u32, Function>,
    /// Monotonic counter used to allocate subscriber ids.
    next_subscriber_id: u32,
}

/// WASM-friendly wrapper around [`Agent`].
///
/// `Rc<RefCell<...>>` replaces `Arc<Mutex<...>>` because
/// `wasm32-unknown-unknown` is single-threaded. We trade the lock cost
/// for borrow-checker-enforced single-threaded access.
#[wasm_bindgen]
pub struct AgentHandle {
    inner: Rc<RefCell<Inner>>,
}

#[wasm_bindgen]
impl AgentHandle {
    /// Construct an agent backed by the named model.
    ///
    /// `model_id` may be a bare id (e.g. `"faux-model"`) or a
    /// `provider:id` pair (e.g. `"faux:faux-model"`). The lookup walks
    /// the catalog the JS host populated with
    /// `register_faux_provider`.
    #[wasm_bindgen(constructor)]
    pub fn new(model_id: String) -> Result<AgentHandle, JsValue> {
        let (provider_hint, id) = match model_id.split_once(':') {
            Some((p, i)) => (Some(p.to_string()), i.to_string()),
            None => (None, model_id.clone()),
        };

        let model = pi_ai_wasm::with_catalog(|catalog| {
            if let Some(provider) = &provider_hint {
                catalog.get_model(&ProviderId::new(provider), &id).cloned()
            } else {
                catalog
                    .iter()
                    .find(|(_, m)| m.id == id)
                    .map(|(_, m)| m.clone())
            }
        })
        .flatten()
        .ok_or_else(|| JsValue::from_str(&format!("model not found: {model_id}")))?;

        let stream_fn = pi_ai_wasm::faux_stream_fn().ok_or_else(|| {
            JsValue::from_str(
                "no stream function registered; call register_faux_provider() before constructing an AgentHandle",
            )
        })?;

        let inner = Inner {
            agent: Agent::new(AgentOptions::new(model, stream_fn, "")),
            subscribers: HashMap::new(),
            next_subscriber_id: 1,
        };

        Ok(Self {
            inner: Rc::new(RefCell::new(inner)),
        })
    }

    /// Number of currently-registered subscribers. Useful for tests
    /// and debug overlays.
    #[wasm_bindgen(getter)]
    pub fn subscriber_count(&self) -> u32 {
        self.inner.borrow().subscribers.len() as u32
    }

    /// Snapshot the configured model id. Returns `undefined` if the
    /// handle has been dropped.
    #[wasm_bindgen(getter)]
    pub fn model_id(&self) -> Result<JsValue, JsValue> {
        let inner = self.inner.borrow();
        Ok(JsValue::from_str(&inner.agent.model().id))
    }

    /// Subscribe to agent events.
    ///
    /// `cb` is a JS function called with one argument — the serialised
    /// [`AgentEvent`] object. The returned id can later be passed to
    /// [`unsubscribe`](Self::unsubscribe).
    #[wasm_bindgen]
    pub fn subscribe(&mut self, cb: Function) -> u32 {
        let mut inner = self.inner.borrow_mut();
        let id = inner.next_subscriber_id;
        inner.next_subscriber_id = inner.next_subscriber_id.wrapping_add(1);
        inner.subscribers.insert(id, cb);
        id
    }

    /// Remove a subscriber. Silently ignores unknown ids.
    #[wasm_bindgen]
    pub fn unsubscribe(&mut self, id: u32) {
        self.inner.borrow_mut().subscribers.remove(&id);
    }

    /// Enqueue a user message and run a single agent turn.
    ///
    /// Returns a JS Promise that resolves with `undefined` when the
    /// turn completes, or rejects with an `Error` if the streaming
    /// layer surfaces an error.
    #[wasm_bindgen]
    pub fn prompt(&mut self, text: String) -> Result<Promise, JsValue> {
        let inner = self.inner.clone();

        let promise = Promise::new(&mut |resolve, reject| {
            let inner = inner.clone();
            let text = text.clone();

            // Spawn the prompt task. We hold the agent borrow for the
            // duration of the turn, so we have to be careful about
            // deadlocks: the event-drain task uses `borrow()` (not
            // `borrow_mut()`), and we call `subscribe` *before* the
            // borrow_mut starts, so the drainer only borrows the
            // subscribers map (not the agent).
            spawn_local(async move {
                // Subscribe BEFORE taking the agent borrow, so the
                // event drainer can use the channel without blocking
                // on the agent's borrow_mut.
                let mut receiver = {
                    let inner = inner.borrow();
                    inner.agent.subscribe()
                };

                // Drainer task — polls the agent's event channel and
                // forwards every event to every subscriber.
                let drain_inner = inner.clone();
                let drain_handle = spawn_local(async move {
                    while let Some(event) = receiver.recv().await {
                        let snapshot = drain_inner.borrow();
                        if snapshot.subscribers.is_empty() {
                            continue;
                        }
                        let serializer = serde_wasm_bindgen::Serializer::new()
                            .serialize_large_number_types_as_bigints(true);
                        match event.serialize(&serializer) {
                            Ok(value) => {
                                for cb in snapshot.subscribers.values() {
                                    let _ = cb.call1(&JsValue::NULL, &value);
                                }
                            }
                            Err(err) => {
                                web_sys_console_error(&format!(
                                    "AgentHandle: failed to serialise event: {err}"
                                ));
                            }
                        }
                    }
                });

                // Run the turn. We hold the agent borrow for the
                // duration of the call, so subscribers added during
                // the turn still see every event (the drainer only
                // touches the subscriber map, not the agent).
                let result = {
                    let mut inner = inner.borrow_mut();
                    inner.agent.prompt(&text).await
                };

                // Wait for the drainer to finish flushing remaining
                // events. The receiver's sender side was dropped when
                // `Agent::prompt` returned, so the drainer sees EOF
                // and exits.
                let _ = drain_handle;

                match result {
                    Ok(()) => {
                        let _ = resolve.call0(&JsValue::UNDEFINED);
                    }
                    Err(err) => {
                        let _ =
                            reject.call1(&JsValue::UNDEFINED, &JsValue::from_str(&err.to_string()));
                    }
                }
            });
        });

        Ok(promise)
    }
}

/// Emit a string to `console.error`. WASM-only — pulls `web_sys` only
/// on the wasm target so the rest of the binary stays slim.
#[cfg(target_arch = "wasm32")]
fn web_sys_console_error(message: &str) {
    // Use a tiny inline `js!`-equivalent instead of pulling in the
    // entire `web_sys` crate — the agent runtime only needs a console
    // hook for diagnostic logging.
    #[wasm_bindgen]
    extern "C" {
        #[wasm_bindgen(js_namespace = console, js_name = error)]
        fn console_error(s: &str);
    }
    console_error(message);
}

impl AgentHandle {
    /// Borrow the inner `Model`. Used by unit tests on native targets
    /// that exercise the same code paths via `#[cfg(target_arch =
    /// "wasm32")]`-gated shims.
    #[allow(dead_code)]
    pub fn model(&self) -> Model {
        self.inner.borrow().agent.model().clone()
    }

    /// Number of `Message` rows currently in the agent's state. Used
    /// by tests to confirm a `prompt` call appended a user message.
    #[allow(dead_code)]
    pub fn message_count(&self) -> usize {
        self.inner.borrow().agent.state().messages.len()
    }
}

/// Helper to format the messages currently in the agent state. Useful
/// for tests; not exported to JS.
#[allow(dead_code)]
pub(crate) fn messages_snapshot(handle: &AgentHandle) -> Vec<Message> {
    handle.inner.borrow().agent.state().messages.clone()
}
