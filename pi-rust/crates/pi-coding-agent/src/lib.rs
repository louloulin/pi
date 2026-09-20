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
//!
//! Stage 23 wires [`trust`]: project-local `.pi` resources are only
//! loaded once the directory is trusted, matching the upstream
//! `core/trust-manager.ts` gate.
//!
//! Stage 24 wires [`compaction`], the `/compact` context summarizer
//! (token estimation, cut-point selection, and the summarization call).
//!
//! Stage 25 wires [`export`]: the self-contained HTML / JSONL session
//! export behind the `/export` slash command and `pi --export`.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod cli;
pub mod clipboard;
pub mod commands;
pub mod compaction;
pub mod config;
pub mod context_files;
pub mod export;
pub mod extensions;
pub mod file_processor;
pub mod frontmatter;
pub mod interactive;
pub mod keybindings;
pub mod packages;
pub mod paths;
pub mod print_mode;
pub mod prompt_templates;
pub mod provider;
pub mod resource_loader;
pub mod rpc;
pub mod session_log;
pub mod skills;
pub mod system_prompt;
pub mod text_fallback;
pub mod thinking;
pub mod tool_executor;
pub mod tool_validation;
pub mod tools;
pub mod trust;

pub use commands::resume::{list_resumable, resolve as resolve_resume, SessionRef};
pub use commands::session::run as run_session_command;
pub use compaction::{
    calculate_context_tokens, compact, compact_history, estimate_context_tokens, estimate_tokens,
    extract_summary, find_cut_point, format_file_operations, prepare_compaction,
    replace_with_compaction, serialize_conversation, should_compact, summary_message, Compaction,
    CompactionError, CompactionPreparation, CompactionSettings, CutPoint,
    DEFAULT_COMPACTION_SETTINGS,
};
pub use context_files::{load_project_context_files, ContextFile};
pub use export::{
    export_active_session_html, export_active_session_jsonl, export_from_file, generate_html,
    generate_jsonl, pre_render_custom_tools, read_session_file, session_data_from_messages,
    ExportError, RenderedToolHtml, SessionData, ToolInfo,
};
pub use file_processor::{
    expand_prompt, read_stdin_if_piped, ExpandedPrompt, FileError, MAX_FILE_BYTES,
};
pub use keybindings::{
    app_default_keybindings, install_keybindings, install_keybindings_from,
    is_legacy_keybinding_name, load_from_file, load_from_file_with_table, load_raw_config,
    merged_definitions, migrate_keybinding_name, migrate_keybindings_config,
    migrate_keybindings_config_with_table, order_keybindings_config, process_env,
    reload_keybindings, to_keybindings_config, windows_keybindings, Env, KeybindingsManager,
    Platform, RawKeybindingsConfig, APP_KEYBINDING_IDS, KEYBINDINGS_FILE_NAME,
    KEYBINDING_NAME_MIGRATIONS,
};
pub use print_mode::{
    run_print_mode, OutputFormat, PrintModeError, PrintModeOptions, PrintModeResult,
};
pub use prompt_templates::{
    expand_prompt_template, find_prompt_template, load_prompt_templates,
    LoadPromptTemplatesOptions, PromptTemplate, PromptTemplateDiagnostic, PromptTemplateSource,
    PromptTemplatesLoadResult,
};
pub use provider::{api_key_env_vars, base_url_env_vars, ProviderError, ProviderRouter};
pub use resource_loader::{
    build_cli_system_prompt, load_resources, resolve_cli_project_trust, LoadedResources,
    ResourceLoadOptions,
};
pub use rpc::{run_rpc_server, JsonRpcError, RpcOutcome, RpcServerError, RpcServerOptions};
pub use skills::{load_skills, LoadSkillsOptions, Skill, SkillsLoadResult};
pub use system_prompt::{build_system_prompt, SystemPromptOptions};
pub use tool_executor::{
    default_executor, BuiltinToolBridge, BuiltinToolExecutor, ExtensionToolExecutor,
};
pub use trust::{
    has_trust_requiring_project_resources, resolve_project_trusted, DefaultProjectTrust,
    ProjectTrustDecision, ProjectTrustStore, ProjectTrustStoreEntry, ProjectTrustUpdate,
    TrustError,
};
