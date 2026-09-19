//! `pi` binary entry point.

use std::process::ExitCode;
use std::sync::Arc;

use pi_ai::models::Models;
use pi_ai::stream::SharedStreamFn;
use pi_coding_agent::cli::{Cli, Command};
use pi_coding_agent::extensions::ui_bridge::TuiUi;
use pi_coding_agent::extensions::wiring::{self, ExtensionLoadOptions};
use pi_coding_agent::file_processor::expand_prompt;
use pi_coding_agent::interactive::{run_interactive, InteractiveOptions};
use pi_coding_agent::packages::{commands as package_commands, PackageCommand};
use pi_coding_agent::print_mode::{run_print_mode, PrintModeOptions};
use pi_coding_agent::provider::ProviderRouter;
use pi_coding_agent::resource_loader::{
    build_cli_prompt_templates, build_cli_system_prompt_with_extension_tools,
    resolve_cli_project_trust,
};
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

    let cli = match Cli::try_parse_with_aliases() {
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

    // Prompt templates are expanded at submission time, not baked into
    // the system prompt, so they are loaded alongside it for every agent
    // mode. `--no-prompt-templates` / `-np` skips default discovery while
    // still honouring explicit `--prompt-template` paths.
    let prompt_templates = match target_mode {
        ModeTarget::Interactive | ModeTarget::Print | ModeTarget::Rpc => {
            let loaded = build_cli_prompt_templates(&cli);
            for diagnostic in &loaded.diagnostics {
                match diagnostic.path.as_deref() {
                    Some(path) => eprintln!(
                        "pi: prompt template {} ({})",
                        diagnostic.message,
                        path.display()
                    ),
                    None => eprintln!("pi: prompt template {}", diagnostic.message),
                }
            }
            loaded.templates
        }
        ModeTarget::Session | ModeTarget::Packages => Vec::new(),
    };

    let initial_prompt = cli.command.as_ref().and_then(|cmd| match cmd {
        Command::Print { prompt } => Some(prompt.join(" ")),
        _ => None,
    });

    match target_mode {
        ModeTarget::Interactive => {
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(err) => {
                    eprintln!("pi: failed to build tokio runtime: {err}");
                    return ExitCode::from(70);
                }
            };
            // Extensions load on the same runtime that drives the agent:
            // the QuickJS host spawns its promise driver + UI worker there.
            // Extensions load on the same runtime that drives the agent:
            // the QuickJS host spawns its promise driver + UI worker there.
            //
            // The dialog bridge only exists when *both* stdin and stdout
            // are real terminals: a piped run (`pi | tee`, a test harness)
            // can never answer a modal, so `ctx.hasUI` has to stay false
            // there instead of promising a UI that cannot render.
            let mut extension_ui = interactive_ui_available().then(TuiUi::new);
            let ui_bridge = extension_ui.as_ref().map(|ui| ui.bridge().clone());
            let has_ui = ui_bridge.is_some();
            let loaded_extensions = load_extensions(&runtime, &cli, "tui", has_ui, ui_bridge);
            let system_prompt =
                build_system_prompt_for(&cli, loaded_extensions.runtime.tool_prompts());
            let tool_executor = loaded_extensions.executor.clone();
            let extension_runtime = Arc::new(loaded_extensions.runtime.clone());
            // Interactive mode is the only path that still uses the
            // legacy JSONL writer (for the `/resume` directory hint).
            // Print mode owns its SQLite session through `pi-session`,
            // so opening the JSONL log eagerly would leave empty
            // `<id>.jsonl` files behind in `--session-dir`.
            let session_log = SessionLog::open(&session_dir, &session_id).ok();
            let options = InteractiveOptions {
                system_prompt,
                // `--append-system-prompt`, `~/.pi/agent/SYSTEM.md` and
                // `APPEND_SYSTEM.md` are already baked into the prompt by
                // `build_cli_system_prompt`, in the position the TS port
                // uses (before the project context).
                append_system_prompt: Vec::new(),
                model: model_override.clone(),
                models,
                session_log,
                session_id: session_id.clone(),
                initial_prompt,
                prompt_templates: prompt_templates.clone(),
                stream_fn: stream_fn.clone(),
                tool_executor,
                extensions: Some(extension_runtime),
                extension_ui: extension_ui.take(),
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
            // Expand `@file` tokens and pipe stdin in, then expand a
            // matching `/name` prompt template into the final prompt.
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
            let prompt_text = pi_coding_agent::expand_prompt_template(
                &expanded.text,
                &prompt_templates,
            );
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
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(err) => {
                    eprintln!("pi: failed to build tokio runtime: {err}");
                    return ExitCode::from(70);
                }
            };
            let loaded_extensions = load_extensions(&runtime, &cli, "print", false, None);
            let system_prompt =
                build_system_prompt_for(&cli, loaded_extensions.runtime.tool_prompts());
            let tool_executor = loaded_extensions.executor.clone();
            let options = PrintModeOptions {
                prompt: prompt_text,
                model: resolved_model,
                stream_fn,
                system_prompt,
                session: session_target,
                session_dir,
                max_turns: cli.max_turns,
                output_format: cli.output_format,
                tool_executor,
                extensions: Arc::new(loaded_extensions.runtime.clone()),
            };
            match runtime.block_on(run_print_mode(options)) {
                Ok(_) => ExitCode::SUCCESS,
                Err(err) => ExitCode::from(err.exit_code()),
            }
        }
        ModeTarget::Rpc => {
            // Headless JSON-RPC 2.0 over stdio. No TUI / crossterm here:
            // stdin and stdout are the transport.
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(err) => {
                    eprintln!("pi: failed to build tokio runtime: {err}");
                    return ExitCode::from(70);
                }
            };
            let loaded_extensions = load_extensions(&runtime, &cli, "rpc", false, None);
            let system_prompt =
                build_system_prompt_for(&cli, loaded_extensions.runtime.tool_prompts());
            let tool_executor = loaded_extensions.executor.clone();
            let options = pi_coding_agent::rpc::RpcServerOptions {
                model: resolved_model,
                models,
                stream_fn,
                system_prompt,
                session_id,
                prompt_templates: prompt_templates.clone(),
                tool_executor,
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

/// Build the system prompt for one agent mode, folding in the prompt
/// contributions declared by the mode's extension tools and reporting
/// any skill diagnostics on stderr.
fn build_system_prompt_for(
    cli: &Cli,
    extension_tools: &[pi_extensions::RegisteredToolPrompt],
) -> String {
    let (trusted, has_trust_requiring_resources) = resolve_cli_project_trust(cli);
    if has_trust_requiring_resources && !trusted {
        let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        eprintln!(
            "pi: {} is not trusted — project .pi resources are ignored.\n    Use `pi --approve` for this run, or /trust inside an interactive session.",
            cwd.display()
        );
    }
    let (prompt, diagnostics) = build_cli_system_prompt_with_extension_tools(cli, extension_tools);
    for diagnostic in &diagnostics {
        match diagnostic.path.as_deref() {
            Some(path) => eprintln!("pi: skill {} ({})", diagnostic.message, path.display()),
            None => eprintln!("pi: skill {}", diagnostic.message),
        }
    }
    prompt
}

fn default_session_dir() -> std::path::PathBuf {
    home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".pi")
        .join("sessions")
}

fn home_dir() -> Option<std::path::PathBuf> {
    pi_coding_agent::paths::home_dir()
}

/// Load JS / TypeScript extensions for one mode and return the whole
/// load outcome: the executor the agent loop should use plus the
/// [`ExtensionRuntime`](pi_coding_agent::extensions::wiring::ExtensionRuntime)
/// the mode uses to dispatch extension commands and persist their
/// session side effects. Failures are reported on stderr and never
/// abort the process: a broken extension leaves the built-in bundle
/// intact.
/// Whether this process can render an interactive dialog: both ends of
/// the terminal have to be a TTY (stdin to receive keys, stdout to draw).
fn interactive_ui_available() -> bool {
    use std::io::IsTerminal;
    std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

fn load_extensions(
    runtime: &tokio::runtime::Runtime,
    cli: &Cli,
    mode: &str,
    has_ui: bool,
    ui: Option<pi_coding_agent::extensions::ui_bridge::TuiUiBridge>,
) -> wiring::ExtensionLoadOutcome {
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let options = ExtensionLoadOptions {
        home: home_dir(),
        cwd,
        explicit: wiring::explicit_paths(&cli.extension, &cli.extensions_dir),
        mode: mode.to_string(),
        has_ui,
        ui,
        disabled: cli.no_extensions,
    };
    let outcome = wiring::load(runtime, &options);
    for (path, reason) in &outcome.errors {
        eprintln!("pi: extension load failed for {}: {reason}", path.display());
    }
    for name in &outcome.shadowed {
        eprintln!("pi: extension tool `{name}` ignored: a built-in tool already uses that name");
    }
    if !outcome.loaded.is_empty() {
        eprintln!(
            "pi: loaded {} extension(s) [{}]",
            outcome.loaded.len(),
            outcome
                .loaded
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if !outcome.runtime.commands().is_empty() {
        eprintln!(
            "pi: extension commands: {}",
            outcome
                .runtime
                .commands()
                .iter()
                .map(|c| format!("/{}", c.name))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    outcome
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
