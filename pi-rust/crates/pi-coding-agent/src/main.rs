//! `pi` binary entry point.

use std::process::ExitCode;
use std::sync::Arc;

use clap::Parser;
use pi_ai::models::Models;
use pi_ai::stream::SharedStreamFn;
use pi_coding_agent::cli::{Cli, Command};
use pi_coding_agent::file_processor::expand_prompt;
use pi_coding_agent::interactive::{run_interactive, InteractiveOptions};
use pi_coding_agent::packages::{commands as package_commands, PackageCommand};
use pi_coding_agent::print_mode::{run_print_mode, PrintModeOptions};
use pi_coding_agent::provider::ProviderRouter;
use pi_coding_agent::session_log::SessionLog;
use pi_protocol::{Api, Model, ProviderId};

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_target(false)
        // Logs must never land on stdout: print mode's `json-events`
        // feed and RPC mode both promise that stdout carries nothing
        // but their own payloads.
        .with_writer(std::io::stderr)
        .init();

    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(err) => {
            // `clap`'s error type already prints to stdout/stderr as
            // appropriate; just propagate the suggested exit code.
            err.exit();
        }
    };

    let models = build_default_models();
    let model_override = cli
        .model
        .as_deref()
        .and_then(|raw| resolve_model(&models, raw));
    let resolved_model = model_override
        .clone()
        .unwrap_or_else(|| default_model(&models));

    // One router per process: it dispatches on `model.provider` at stream
    // time, so print / RPC / interactive mode all reach the real OpenAI /
    // Anthropic / Google adapters and `/model` + `setModel` can switch
    // providers mid-session. Fail fast when the selected model's provider
    // has no credential instead of silently streaming from the faux one.
    let router = ProviderRouter::from_env();
    if let Err(err) = router.require(&resolved_model) {
        eprintln!("pi: {err}");
        return ExitCode::from(err.exit_code());
    }
    let stream_fn: SharedStreamFn = Arc::new(router);

    let session_dir = cli.session_dir.clone().unwrap_or_else(default_session_dir);
    let session_id = cli.resume.clone().unwrap_or_else(new_session_id);

    let system_prompt = default_system_prompt();

    // The built-in tool bundle (read / write / edit / bash / find / grep /
    // ls) is shared by all three modes. Building it here — the composition
    // root — keeps a single executor instance per process and guarantees
    // interactive, print and RPC modes execute tool calls for real.
    let tool_executor: Arc<dyn pi_agent_core::tools::ToolExecutor> =
        pi_coding_agent::tool_executor::default_executor();

    let initial_prompt = cli.command.as_ref().and_then(|cmd| match cmd {
        Command::Print { prompt } => Some(prompt.join(" ")),
        _ => None,
    });

    let target_mode = match cli.command.as_ref() {
        Some(Command::Print { .. }) => ModeTarget::Print,
        Some(Command::Rpc) => ModeTarget::Rpc,
        Some(Command::Session { .. }) => ModeTarget::Session,
        Some(Command::Version { .. })
        | Some(Command::Install { .. })
        | Some(Command::Remove { .. })
        | Some(Command::List { .. })
        | Some(Command::UpdateModels)
        | Some(Command::ListModels { .. }) => ModeTarget::Packages,
        _ => {
            if cli.print.is_some() {
                ModeTarget::Print
            } else if cli.rpc {
                ModeTarget::Rpc
            } else {
                ModeTarget::Interactive
            }
        }
    };

    match target_mode {
        ModeTarget::Interactive => {
            // Interactive mode is the only path that still uses the
            // legacy JSONL writer (for the `/resume` directory hint).
            // Print mode owns its SQLite session through `pi-session`,
            // so opening the JSONL log eagerly would leave empty
            // `<id>.jsonl` files behind in `--session-dir`.
            let session_log = SessionLog::open(&session_dir, &session_id).ok();
            let options = InteractiveOptions {
                system_prompt,
                append_system_prompt: cli.append_system_prompt.clone(),
                model: model_override.clone(),
                models,
                session_log,
                session_id: session_id.clone(),
                initial_prompt,
                stream_fn: stream_fn.clone(),
                tool_executor: tool_executor.clone(),
            };
            let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
                Ok(rt) => rt,
                Err(err) => {
                    eprintln!("pi: failed to build tokio runtime: {err}");
                    return ExitCode::from(70);
                }
            };
            match runtime.block_on(run_interactive(options)) {
                Ok(_) => ExitCode::SUCCESS,
                Err(err) => {
                    eprintln!("pi: interactive session error: {err:?}");
                    ExitCode::FAILURE
                }
            }
        }
        ModeTarget::Print => {
            // Priority: `pi print "..."` subcommand → `--print "..."` →
            // empty (read from stdin).
            let prompt_raw = initial_prompt
                .or_else(|| cli.print.clone())
                .unwrap_or_default();
            // Expand `@file` tokens and pipe stdin in.
            let stdin = match pi_coding_agent::read_stdin_if_piped() {
                Ok(value) => value,
                Err(err) => {
                    eprintln!("pi: failed to read stdin: {err}");
                    return ExitCode::from(74);
                }
            };
            let expanded = match expand_prompt(&prompt_raw, stdin.as_deref()) {
                Ok(out) => out,
                Err(err) => {
                    eprintln!("pi: {err}");
                    return ExitCode::from(err.exit_code());
                }
            };
            // `--continue` / `--session` interact: a bare `--continue`
            // (`None` payload) attaches the most recent session, while
            // `--continue=<id>` and `--session <id>` attach that exact
            // session. All three resolve against the same `pi-session`
            // SQLite store the TUI's `/resume` reads.
            let session_target = if let Some(id) = cli.session.clone() {
                Some(Some(id))
            } else {
                // `Some(None)` for bare `--continue`, `Some(Some(id))`
                // for `--continue=<id>`.
                cli.continue_.clone()
            };
            let options = PrintModeOptions {
                prompt: expanded.text,
                model: resolved_model,
                stream_fn,
                system_prompt,
                session: session_target,
                session_dir,
                max_turns: cli.max_turns,
                output_format: cli.output_format,
                tool_executor: tool_executor.clone(),
            };
            let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
                Ok(rt) => rt,
                Err(err) => {
                    eprintln!("pi: failed to build tokio runtime: {err}");
                    return ExitCode::from(70);
                }
            };
            match runtime.block_on(run_print_mode(options)) {
                Ok(_) => ExitCode::SUCCESS,
                Err(err) => ExitCode::from(err.exit_code()),
            }
        }
        ModeTarget::Rpc => {
            // Headless JSON-RPC 2.0 over stdio. No TUI / crossterm here:
            // stdin and stdout are the transport.
            let options = pi_coding_agent::rpc::RpcServerOptions {
                model: resolved_model,
                models,
                stream_fn,
                system_prompt,
                session_id,
                tool_executor,
            };
            let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
                Ok(rt) => rt,
                Err(err) => {
                    eprintln!("pi: failed to build tokio runtime: {err}");
                    return ExitCode::from(70);
                }
            };
            match runtime.block_on(pi_coding_agent::rpc::run_rpc_server(options)) {
                Ok(_) => ExitCode::SUCCESS,
                Err(err) => {
                    eprintln!("pi: rpc error: {err}");
                    ExitCode::from(70)
                }
            }
        }
        ModeTarget::Session => {
            let Some(Command::Session { action }) = cli.command else {
                unreachable!("ModeTarget::Session only set when command is Session");
            };
            match pi_coding_agent::run_session_command(action) {
                Ok(_) => ExitCode::SUCCESS,
                Err(err) => {
                    eprintln!("pi: session command failed: {err:?}");
                    ExitCode::FAILURE
                }
            }
        }
        ModeTarget::Packages => {
            let package_command = match cli.command.as_ref() {
                Some(Command::Version { output }) => PackageCommand::Version { output: *output },
                Some(Command::Install { spec, dir }) => PackageCommand::Install {
                    spec: spec.clone(),
                    dir: dir.clone(),
                },
                Some(Command::Remove { spec, dir }) => PackageCommand::Remove {
                    spec: spec.clone(),
                    dir: dir.clone(),
                },
                Some(Command::List { dir, output }) => PackageCommand::List {
                    dir: dir.clone(),
                    output: *output,
                },
                Some(Command::UpdateModels) => PackageCommand::UpdateModels,
                Some(Command::ListModels { output }) => {
                    PackageCommand::ListModels { output: *output }
                }
                _ => unreachable!("ModeTarget::Packages only set for package subcommands"),
            };
            match package_commands::run(package_command, &models) {
                Ok(()) => ExitCode::SUCCESS,
                Err(err) => {
                    eprintln!("pi: {err}");
                    ExitCode::from(err.exit_code())
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModeTarget {
    Interactive,
    Print,
    Rpc,
    Session,
    Packages,
}

fn default_system_prompt() -> String {
    "You are pi, an interactive coding agent running on the Rust port.\n\
     Answer the user's request, call tools when useful, and keep your\n\
     replies concise."
        .to_string()
}

fn default_session_dir() -> std::path::PathBuf {
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    home.join(".pi").join("sessions")
}

fn new_session_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("session-{nanos:x}")
}

fn build_default_models() -> Models {
    // The catalog is data, not code: every entry comes from
    // `pi_ai::providers::registry::BUILTIN_PROVIDERS`, the same table the
    // `ProviderRouter` uses to build streaming adapters. Adding a provider
    // therefore means one registry entry, not an edit here.
    let mut models = Models::new();
    for spec in pi_ai::providers::registry::BUILTIN_PROVIDERS {
        let entries: Vec<Model> = spec
            .models
            .iter()
            .map(|m| Model {
                provider: ProviderId::new(spec.id),
                id: m.id.to_string(),
                api: spec.api,
                label: Some(m.label.to_string()),
                context_window: m.context_window,
                max_output_tokens: m.max_output_tokens,
            })
            .collect();
        if !entries.is_empty() {
            models.set_provider(ProviderId::new(spec.id), entries);
        }
    }
    models
}

fn resolve_model(models: &Models, raw: &str) -> Option<Model> {
    // Accept `<provider>/<id>` or just `<id>` (the latter matches the
    // first provider that exposes an entry with that id).
    if let Some((provider, id)) = raw.split_once('/') {
        let provider_id = ProviderId::new(provider);
        return models.get_model(&provider_id, id).cloned();
    }
    models
        .iter()
        .find(|(_, m)| m.id == raw)
        .map(|(_, m)| m.clone())
}

fn default_model(models: &Models) -> Model {
    // Prefer the faux provider when present so print mode + the TUI
    // boot deterministically without an API key. Falls back to the
    // first registered model otherwise (hash-map order is not stable).
    if let Some(model) = models.get_model(&ProviderId::new("faux"), "faux-model") {
        return model.clone();
    }
    models
        .iter()
        .next()
        .map(|(_, m)| m.clone())
        .unwrap_or_else(|| Model {
            provider: ProviderId::new("faux"),
            id: "faux-model".into(),
            api: Api::Faux,
            label: Some("Faux test model".into()),
            context_window: 8192,
            max_output_tokens: 1024,
        })
}
