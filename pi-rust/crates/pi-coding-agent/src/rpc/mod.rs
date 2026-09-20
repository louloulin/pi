//! RPC mode — JSON-RPC 2.0 over stdio (Stage 12 of the Rust port).
//!
//! `pi --rpc` (and `pi rpc`) turns the binary into a headless service an
//! editor, host process, or remote controller can drive:
//!
//! * **stdin** — one JSON-RPC 2.0 request or notification per line
//!   (NDJSON, LF framing; blank lines ignored).
//! * **stdout** — one JSON object per line: a [`protocol::Response`] for
//!   every request, and a notification carrying the agent events the
//!   `json-events` print mode emits.
//!
//! The wire types live in [`protocol`], the error-code mapping in
//! [`error`], the event translation in [`events`] (shared with
//! `print_mode`), and the read/dispatch/event loop in [`server`].
//!
//! # Method set
//!
//! | method                 | params                     | result |
//! |------------------------|----------------------------|--------|
//! | `prompt`               | `{ "text": "…" }`          | `{ "turn", "stopReason" }` after `turn_end` |
//! | `abort`                | –                          | `{ "aborted": bool }` |
//! | `getState` / `get_state` | –                        | `{ "model", "messages", "sessionId" }` |
//! | `setModel` / `set_model` | `{ "model": "provider/id" }` or `{ "provider", "modelId" }` | `{ "model" }` |
//!
//! Unknown methods answer `-32601`, bad params `-32602`, internal failures
//! `-32603`, malformed JSON `-32700`, and a second `prompt` while a turn
//! is in flight `-32000`.
//!
//! # Client usage
//!
//! [`client::RpcClient`] is the Rust counterpart of the upstream
//! `rpc-client.ts`: it spawns `pi --rpc`, writes requests, pairs each
//! response to its `id`, and streams the `event` notifications to
//! callbacks or the [`client::RpcClient::events`] log. The integration
//! tests in `tests/rpc.rs` are built on it, so an embedder gets the same
//! client the tests exercise.
//!
//! ```no_run
//! use std::time::Duration;
//! use pi_coding_agent::rpc::{RpcClient, RpcClientOptions};
//!
//! let options = RpcClientOptions::new("target/debug/pi");
//! let mut client = RpcClient::spawn(&options).expect("spawn pi --rpc");
//! let result = client.prompt("hello").expect("one turn"); // response paired by id
//! let state = client.get_state().expect("state");          // {"model", "messages", "sessionId"}
//! for event in client.events() {
//!     println!("{} / {}", event["type"], event["turn"]);
//! }
//! client.close_stdin();                                     // EOF → server drains and exits 0
//! let status = client.wait_for_exit(Duration::from_secs(30)).expect("exit");
//! assert_eq!(status.code(), Some(0));
//! # let _ = result;
//! ```
//!
//! Events also stream to [`client::RpcClient::on_event`] callbacks while a
//! [`client::RpcClient::call`] is waiting, which is what
//! `prompt_returns_response_and_streams_events` asserts. This client speaks
//! only this stdio protocol — the `pi-protocol` / `pi-client` framing and
//! unix-socket transport are a separate line.
//!
//! # Concurrency
//!
//! [`run_rpc_server`] allows exactly one in-flight turn. It rejects a
//! second `prompt` with `-32000 busy` instead of queueing it: the upstream
//! TypeScript RPC queue is tied to the `steer` / `follow_up` /
//! `streamingBehavior` surface, which is out of this stage's method set.
//! Rejecting keeps the single-turn invariant explicit and lets a client
//! fall back to `abort` + retry.
//!
//! stdin EOF drains the in-flight turn and exits 0; a closed stdout
//! (`EPIPE`) exits without panicking. Only JSON reaches stdout —
//! diagnostics go to stderr.

#![deny(missing_docs)]

pub mod client;
pub mod error;
pub mod events;
pub mod protocol;
pub mod server;

pub use client::{ClientMessage, RpcClient, RpcClientError, RpcClientOptions, StdinMode};
pub use error::{codes, JsonRpcError};
pub use events::agent_event_to_json;
pub use protocol::{Incoming, Notification, Request, Response, JSONRPC_VERSION};
pub use server::{run_rpc_server, RpcServerError};

use pi_agent_core::tools::ToolExecutor;
use pi_ai::models::Models;
use pi_ai::stream::SharedStreamFn;
use pi_protocol::Model;
use std::sync::Arc;

use crate::prompt_templates::PromptTemplate;

/// Inputs for [`run_rpc_server`].
pub struct RpcServerOptions {
    /// Model the agent starts with. `setModel` may switch it later.
    pub model: Model,
    /// Model catalog used to resolve `setModel` arguments.
    pub models: Models,
    /// Streaming implementation driving the agent (the faux provider in
    /// tests, a real provider adapter in production).
    pub stream_fn: SharedStreamFn,
    /// System prompt prepended to every turn.
    pub system_prompt: String,
    /// Session id reported by `getState` — opaque to this stage.
    pub session_id: String,
    /// Tool executor the agent loop dispatches model tool calls to.
    /// The `pi` binary passes
    /// [`default_executor`](crate::tool_executor::default_executor);
    /// tests inject a scripted executor.
    pub tool_executor: Arc<dyn ToolExecutor>,
    /// Prompt templates loaded at startup. A `prompt` request whose text
    /// is `/<name> [args]` and matches a template is expanded into the
    /// template body before the turn starts.
    pub prompt_templates: Vec<PromptTemplate>,
}

/// Outcome of a completed RPC session.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RpcOutcome {
    /// Number of non-empty stdin lines the server processed.
    pub requests: u64,
    /// Completed turns (including turns ended by `abort`).
    pub turns: u32,
}
