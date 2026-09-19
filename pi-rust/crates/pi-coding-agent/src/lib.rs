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
//!
//! Stage 21 wires the resource layer: [`context_files`] (project
//! `AGENTS.md` / `CLAUDE.md` discovery), [`skills`] (Agent Skills
//! discovery + prompt formatting), [`frontmatter`] (YAML frontmatter for
//! markdown resources), and [`system_prompt`], which assembles the whole
//! thing into the prompt every mode sends to the model.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod cli;
pub mod commands;
pub mod config;
pub mod context_files;
pub mod extensions;
pub mod file_processor;
pub mod frontmatter;
pub mod interactive;
pub mod packages;
pub mod paths;
pub mod print_mode;
pub mod provider;
pub mod resource_loader;
pub mod rpc;
pub mod session_log;
pub mod skills;
pub mod system_prompt;
pub mod text_fallback;
pub mod tool_executor;
pub mod tools;

pub use commands::resume::{list_resumable, resolve as resolve_resume, SessionRef};
pub use commands::session::run as run_session_command;
pub use context_files::{load_project_context_files, ContextFile};
pub use file_processor::{
    expand_prompt, read_stdin_if_piped, ExpandedPrompt, FileError, MAX_FILE_BYTES,
};
pub use print_mode::{
    run_print_mode, OutputFormat, PrintModeError, PrintModeOptions, PrintModeResult,
};
pub use provider::{api_key_env_vars, base_url_env_vars, ProviderError, ProviderRouter};
pub use resource_loader::{
    build_cli_system_prompt, load_resources, LoadedResources, ResourceLoadOptions,
};
pub use rpc::{
    run_rpc_server, JsonRpcError, RpcOutcome, RpcServerError, RpcServerOptions,
};
pub use skills::{load_skills, LoadSkillsOptions, Skill, SkillsLoadResult};
pub use system_prompt::{build_system_prompt, SystemPromptOptions};
pub use tool_executor::{default_executor, BuiltinToolExecutor, ExtensionToolExecutor};
