//! `pi-coding-agent` — interactive CLI binary.
//!
//! Stage 0 declares the CLI entry point and the public surface. Stage 4
//! wires the TUI + interactive mode; Stage 5 wires the session backend.
//!
//! Stage 1 (this commit) also lands the built-in tool bundle
//! ([`tools`]): `read`, `write`, `edit`, `bash`. These are the tools the
//! model sees by default; extension tools are layered on top of them via
//! [`pi_extensions`].
//!
//! Stage 3 wires the [`extensions`] module so the agent can load JS /
//! TypeScript extensions from disk through the embedded QuickJS host.
//!
//! Stage 8 wires [`print_mode`], the non-interactive single-shot entry
//! point (`pi --print "..."`). Print mode is the canonical surface for
//! CI / scripts / containerised hosts — it streams text (or NDJSON) on
//! stdout and never opens a TUI.
//!
//! Stage 11 wires [`packages`], the `pi install` / `remove` / `list` /
//! `version` / `list-models` / `update-models` ecosystem.
//!
//! Stage 12 wires [`rpc`], the headless JSON-RPC 2.0 over stdio mode
//! (`pi --rpc`) that editors and host processes drive.
//!
//! Stage 14 wires [`provider`], the [`ProviderRouter`](provider::ProviderRouter)
//! that maps a resolved model to its real streaming adapter
//! (OpenAI / Anthropic / Google) instead of the hard-coded faux
//! provider the first three modes shipped with.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod cli;
pub mod commands;
pub mod config;
pub mod extensions;
pub mod file_processor;
pub mod interactive;
pub mod packages;
pub mod print_mode;
pub mod provider;
pub mod rpc;
pub mod session_log;
pub mod text_fallback;
pub mod tool_executor;
pub mod tools;

pub use commands::resume::{list_resumable, resolve as resolve_resume, SessionRef};
pub use commands::session::run as run_session_command;
pub use file_processor::{
    expand_prompt, read_stdin_if_piped, ExpandedPrompt, FileError, MAX_FILE_BYTES,
};
pub use print_mode::{
    run_print_mode, OutputFormat, PrintModeError, PrintModeOptions, PrintModeResult,
};
pub use provider::{api_key_env_vars, base_url_env_vars, ProviderError, ProviderRouter};
pub use rpc::{
    run_rpc_server, JsonRpcError, RpcOutcome, RpcServerError, RpcServerOptions,
};
pub use tool_executor::{default_executor, BuiltinToolExecutor};
