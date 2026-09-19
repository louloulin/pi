//! `pi` binary entry point.

use std::process::ExitCode;
use std::sync::Arc;

use clap::Parser;
use pi_ai::models::Models;
use pi_ai::providers::faux::FauxProvider;
use pi_ai::stream::SharedStreamFn;
use pi_coding_agent::cli::{Cli, Command};
use pi_coding_agent::file_processor::expand_prompt;
use pi_coding_agent::interactive::{run_interactive, InteractiveOptions};
use pi_coding_agent::print_mode::{run_print_mode, PrintModeOptions};
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

    let session_dir = cli.session_dir.clone().unwrap_or_else(default_session_dir);
    let session_id = cli.resume.clone().unwrap_or_else(new_session_id);
    let session_log = SessionLog::open(&session_dir, &session_id).ok();

    let system_prompt = default_system_prompt();

    let initial_prompt = cli.command.as_ref().and_then(|cmd| match cmd {
        Command::Print { prompt } => Some(prompt.join(" ")),
        _ => None,
    });

    let target_mode = match cli.command.as_ref() {
        Some(Command::Print { .. }) => ModeTarget::Print,
        Some(Command::Rpc) => ModeTarget::Rpc,
        Some(Command::Session { .. }) => ModeTarget::Session,
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
            let options = InteractiveOptions {
                system_prompt,
                append_system_prompt: cli.append_system_prompt.clone(),
                model: model_override.clone(),
                models,
                session_log,
                session_id: session_id.clone(),
                initial_prompt,
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
            // (`None` payload) attaches the most recent session, an
            // explicit `--session <id>` attaches that id.
            let session_target = if let Some(id) = cli.session.clone() {
                Some(Some(id))
            } else if cli.continue_.is_some() {
                Some(None)
            } else {
                None
            };
            let options = PrintModeOptions {
                prompt: expanded.text,
                model: resolved_model,
                stream_fn: SharedStreamFn::from(Arc::new(FauxProvider::default()) as Arc<dyn pi_ai::stream::StreamFn>),
                system_prompt,
                session: session_target,
                session_dir,
                max_turns: cli.max_turns,
                output_format: cli.output_format,
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
            let stream_fn = SharedStreamFn::from(
                Arc::new(FauxProvider::default()) as Arc<dyn pi_ai::stream::StreamFn>,
            );
            let options = pi_coding_agent::rpc::RpcServerOptions {
                model: resolved_model,
                models,
                stream_fn,
                system_prompt,
                session_id,
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
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModeTarget {
    Interactive,
    Print,
    Rpc,
    Session,
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
    let mut models = Models::new();
    // Stage 4 ships the faux model + a couple of pseudo provider
    // entries so `/model` lists something useful. Stage 7 replaces
    // the Anthropic stub with the real `AnthropicProvider` streaming
    // adapter; the model catalog stays inline for now (a future stage
    // will move it into `pi-ai/src/models/catalog.json` like the TS
    // upstream does).
    let faux = Model {
        provider: ProviderId::new("faux"),
        id: "faux-model".into(),
        api: Api::Faux,
        label: Some("Faux test model".into()),
        context_window: 8192,
        max_output_tokens: 1024,
    };
    let openai = Model {
        provider: ProviderId::new("openai"),
        id: "gpt-4o-mini".into(),
        api: Api::OpenAiChatCompletions,
        label: Some("GPT-4o mini".into()),
        context_window: 128_000,
        max_output_tokens: 16_384,
    };
    let anthropic_sonnet = Model {
        provider: ProviderId::new("anthropic"),
        id: "claude-sonnet-4-5".into(),
        api: Api::AnthropicMessages,
        label: Some("Claude Sonnet 4.5".into()),
        context_window: 200_000,
        max_output_tokens: 8_192,
    };
    let anthropic_opus = Model {
        provider: ProviderId::new("anthropic"),
        id: "claude-opus-4-5".into(),
        api: Api::AnthropicMessages,
        label: Some("Claude Opus 4.5".into()),
        context_window: 200_000,
        max_output_tokens: 8_192,
    };
    let anthropic_haiku = Model {
        provider: ProviderId::new("anthropic"),
        id: "claude-haiku-4-5".into(),
        api: Api::AnthropicMessages,
        label: Some("Claude Haiku 4.5".into()),
        context_window: 200_000,
        max_output_tokens: 8_192,
    };
    models.set_provider(ProviderId::new("faux"), vec![faux]);
    models.set_provider(ProviderId::new("openai"), vec![openai]);
    models.set_provider(
        ProviderId::new("anthropic"),
        vec![anthropic_sonnet, anthropic_opus, anthropic_haiku],
    );
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
