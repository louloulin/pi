//! `pi` binary entry point.

use clap::Parser;
use pi_ai::models::Models;
use pi_coding_agent::cli::{Cli, Command};
use pi_coding_agent::interactive::{run_interactive, InteractiveOptions};
use pi_coding_agent::session_log::SessionLog;
use pi_protocol::Model;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_target(false)
        .init();

    let cli = Cli::parse();

    let models = build_default_models();

    let model_override = cli
        .model
        .as_deref()
        .and_then(|raw| resolve_model(&models, raw));

    let session_dir = cli.session_dir.clone().unwrap_or_else(default_session_dir);
    let session_id = cli.resume.clone().unwrap_or_else(new_session_id);
    let session_log = SessionLog::open(&session_dir, &session_id).ok();

    let system_prompt = default_system_prompt();

    let initial_prompt = cli.command.as_ref().and_then(|cmd| match cmd {
        Command::Print { prompt } => prompt.join(" ").into(),
        _ => None,
    });

    let target_mode = match cli.command.as_ref() {
        Some(Command::Print { .. }) => ModeTarget::Print,
        Some(Command::Rpc) => ModeTarget::Rpc,
        _ => {
            if cli.print {
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
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;
            runtime.block_on(run_interactive(options))?;
        }
        ModeTarget::Print => {
            println!("pi (rust) {} — print mode", env!("CARGO_PKG_VERSION"));
            println!("(Stage 4 wires interactive; print mode is a stub.)");
        }
        ModeTarget::Rpc => {
            println!(
                "pi (rust) {} — rpc mode is a Stage 5 deliverable",
                env!("CARGO_PKG_VERSION")
            );
        }
    }

    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModeTarget {
    Interactive,
    Print,
    Rpc,
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
    // entries so `/model` lists something useful. Stage 1 replaces
    // these stubs with real provider catalogues once the OpenAI /
    // Anthropic adapters land.
    let faux = Model {
        provider: pi_protocol::ProviderId::new("faux"),
        id: "faux-model".into(),
        api: pi_protocol::Api::Faux,
        label: Some("Faux test model".into()),
        context_window: 8192,
        max_output_tokens: 1024,
    };
    let openai = Model {
        provider: pi_protocol::ProviderId::new("openai"),
        id: "gpt-4o-mini".into(),
        api: pi_protocol::Api::OpenAiChatCompletions,
        label: Some("GPT-4o mini (stub)".into()),
        context_window: 128_000,
        max_output_tokens: 16_384,
    };
    let anthropic = Model {
        provider: pi_protocol::ProviderId::new("anthropic"),
        id: "claude-3-5-sonnet-latest".into(),
        api: pi_protocol::Api::AnthropicMessages,
        label: Some("Claude 3.5 Sonnet (stub)".into()),
        context_window: 200_000,
        max_output_tokens: 8_192,
    };
    models.set_provider(pi_protocol::ProviderId::new("faux"), vec![faux]);
    models.set_provider(pi_protocol::ProviderId::new("openai"), vec![openai]);
    models.set_provider(pi_protocol::ProviderId::new("anthropic"), vec![anthropic]);
    models
}

fn resolve_model(models: &Models, raw: &str) -> Option<Model> {
    // Accept `<provider>/<id>` or just `<id>` (the latter matches the
    // first provider that exposes an entry with that id).
    if let Some((provider, id)) = raw.split_once('/') {
        let provider_id = pi_protocol::ProviderId::new(provider);
        return models.get_model(&provider_id, id).cloned();
    }
    models
        .iter()
        .find(|(_, m)| m.id == raw)
        .map(|(_, m)| m.clone())
}
