//! `pi` binary entry point.

use std::process::ExitCode;
use std::sync::Arc;

use pi_ai::models::Models;
use pi_ai::stream::SharedStreamFn;
use pi_coding_agent::cli::{Cli, Command};
use pi_coding_agent::commands::session::new_session_id;
use pi_coding_agent::config::{
    load_agent_retry_policy_default, load_compaction_settings_default,
    load_fullscreen_exit_output_default, load_provider_retry_policy_default,
    load_quiet_startup_default,
};
use pi_coding_agent::extensions::ui_bridge::TuiUi;
use pi_coding_agent::extensions::wiring::{self, ExtensionLoadOptions};
use pi_coding_agent::file_processor::expand_prompt;
use pi_coding_agent::interactive::{run_interactive, InteractiveOptions};
use pi_coding_agent::packages::{commands as package_commands, PackageCommand};
use pi_coding_agent::print_mode::{run_print_mode, PrintModeOptions};
use pi_coding_agent::provider::ProviderRouter;
use pi_coding_agent::resource_loader::{
    build_cli_prompt_templates_with_extensions, build_cli_system_prompt_with_extensions,
    resolve_cli_project_trust,
};
use pi_coding_agent::session_log::SessionLog;
use pi_coding_agent::startup_log;
use pi_extensions::DiscoveredResources;
use pi_protocol::{Api, Model, ProviderId};

fn detect_mode_target(cli: &Cli) -> ModeTarget {
    match cli.command.as_ref() {
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
    }
}

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

    // `pi --export <session.jsonl> [output.html]` is a pure file
    // transform: handle it before the provider router so it works with no
    // API key configured (upstream `main.ts` does the same, printing
    // `Exported to: <path>` / `Error: <message>`).
    if let Some(export) = cli.export.as_ref() {
        let input = &export[0];
        let output = export.get(1).map(std::path::PathBuf::as_path);
        return run_export_cli(input, output);
    }

    // `pi --clear-history` is the explicit cleanup entry point for the
    // cross-session prompt history (LUM-1319): it deletes the file and exits,
    // before the provider router, so it works with no credentials configured.
    if cli.clear_history {
        return clear_prompt_history_cli();
    }

    // Decide the run target *before* any startup-notice-emitting code
    // runs. The interactive target silences stderr for the
    // startup_log::log_message channel so extension-load noise does not
    // race the TUI's first alt-screen frame; non-interactive targets
    // keep the historical behaviour of mirroring each notice to stderr.
    let target_mode = detect_mode_target(&cli);
    startup_log::set_mirror_to_stderr(target_mode != ModeTarget::Interactive);

    let mut models = build_default_models();
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
    //
    // `settings.retry.provider` wraps every adapter in the provider-request
    // retry loop (429/5xx/transport failures, `Retry-After` aware). The
    // default policy retries nothing, matching upstream.
    let mut router =
        ProviderRouter::from_env().with_provider_retry(load_provider_retry_policy_default());
    if let Err(err) = router.require(&resolved_model) {
        eprintln!("pi: {err}");
        return ExitCode::from(err.exit_code());
    }

    let session_dir = cli.session_dir.clone().unwrap_or_else(default_session_dir);
    let session_id = cli.resume.clone().unwrap_or_else(new_session_id);

    // Prompt templates are expanded at submission time, not baked into
    // the system prompt. They are loaded per mode *after* the extension
    // host has run `resources_discover`, so a plugin-provided template
    // participates in the same `/name` lookup the default directories do
    // (`--no-prompt-templates` / `-np` still silences both).

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
            let ui_region_host = extension_ui.as_ref().map(|ui| ui.region_host());
            let has_ui = ui_bridge.is_some();
            // `load_extensions` consumes the host; keep a clone for the
            // interactive options so the driver can consult the
            // extension shortcut registry later (P0-3).
            let ui_region_host_for_options = ui_region_host.clone();
            let loaded_extensions =
                load_extensions(&runtime, &cli, "tui", has_ui, ui_bridge, ui_region_host);
            // The UI-facing projection of the load pass: the startup header's
            // extension row and `/extensions` read this, never stderr.
            let extension_report = loaded_extensions.report(cli.no_extensions);
            // Fold `pi.registerProvider` registrations into the router +
            // catalog before any turn streams. Extension providers are
            // additive: a bad key or unknown family warns and is skipped.
            let applied = wiring::apply_registered_providers(
                &mut router,
                &mut models,
                loaded_extensions.runtime.providers(),
            );
            if !applied.is_empty() {
                tracing::debug!(providers = ?applied, "registered extension providers");
            }
            let stream_fn: SharedStreamFn = Arc::new(router.clone());
            let extension_resources = loaded_extensions.runtime.resource_paths().clone();
            let system_prompt = build_system_prompt_for(
                &cli,
                loaded_extensions.runtime.tool_prompts(),
                &extension_resources,
            );
            let prompt_templates = load_prompt_templates_for(&cli, &extension_resources);
            let tool_executor = loaded_extensions.executor.clone();
            let extension_runtime = Arc::new(loaded_extensions.runtime.clone());
            // Interactive mode is the only path that still uses the
            // legacy JSONL writer (for the `/resume` directory hint).
            // Print mode owns its SQLite session through `pi-session`,
            // so opening the JSONL log eagerly would leave empty
            // `<id>.jsonl` files behind in `--session-dir`.
            let session_log = SessionLog::open(&session_dir, &session_id).ok();
            // `--resume <id>`: attach the stored session's name so the
            // status bar and `/session` show the identity the TUI
            // `/resume` would restore (and `/name` persists into the same
            // database).
            let (session_name, session_database) = match cli.resume.as_deref() {
                Some(id) => match pi_coding_agent::resolve_resume(&session_dir, id) {
                    Ok(reference) => (reference.name, Some(reference.database)),
                    Err(_) => (None, None),
                },
                None => (None, None),
            };
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
                session_name,
                session_database,
                session_leaf: None,
                compaction: load_compaction_settings_default(),
                // `settings.retry` drives the agent-level retry loop: a
                // transient provider failure restarts the assistant call
                // with exponential backoff instead of ending the turn.
                retry: load_agent_retry_policy_default(),
                initial_prompt,
                prompt_templates,
                stream_fn: stream_fn.clone(),
                tool_executor,
                extensions: Some(extension_runtime),
                extension_report,
                extension_ui: extension_ui.take(),
                // P0-3: the TuiRegionHost owns the extension shortcut
                // registry; share its Arc with the interactive driver so
                // `dispatch_global_chord_table` can claim chords that
                // `ctx.ui.registerShortcut` installed.
                extension_shortcut_registry: ui_region_host_for_options
                    .as_ref()
                    .map(|h| h.extension_shortcuts()),
                // `app.clipboard.pasteImage` reads the real system clipboard
                // (`wl-paste` / `xclip` / `pngpaste` / PowerShell).
                clipboard: None,
                // Picker view state (`/resume`, `/tree`) starts at its
                // defaults; the driver keeps it across selector opens.
                pickers: Default::default(),
                // Credential store: the router owns the live store so
                // `/login` writes land in the same place the resolution
                // path reads from (`ProviderRouter::resolve_provider_auth`).
                credential_store: router.credential_store(),
                // `--no-header`: the startup key-hint screen. Upstream's
                // equivalent is the `quietStartup` setting, which it reads
                // while constructing the session (`interactive-mode.ts:859`).
                // LUM-1310: the setting is honoured here too, not only by
                // `/settings` — the header is a layout decision taken before
                // the first frame, so it has to be resolved at this point.
                quiet_startup: cli.no_header || load_quiet_startup_default(),
                // `fullscreenExitOutput` (`settings.json`) decides what the
                // session leaves on the terminal when it exits; read here, at
                // construction, like the rest of the UI slice.
                exit_output: load_fullscreen_exit_output_default(),
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
            let loaded_extensions = load_extensions(&runtime, &cli, "print", false, None, None);
            let applied = wiring::apply_registered_providers(
                &mut router,
                &mut models,
                loaded_extensions.runtime.providers(),
            );
            if !applied.is_empty() {
                tracing::debug!(providers = ?applied, "registered extension providers");
            }
            let stream_fn: SharedStreamFn = Arc::new(router.clone());
            let extension_resources = loaded_extensions.runtime.resource_paths().clone();
            let system_prompt = build_system_prompt_for(
                &cli,
                loaded_extensions.runtime.tool_prompts(),
                &extension_resources,
            );
            let prompt_templates = load_prompt_templates_for(&cli, &extension_resources);
            // Template expansion runs here, not before the extension load:
            // `/name` must be able to resolve to a template a plugin
            // contributed through `resources_discover`.
            let prompt_text =
                pi_coding_agent::expand_prompt_template(&expanded.text, &prompt_templates);
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
                retry: load_agent_retry_policy_default(),
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
            let loaded_extensions = load_extensions(&runtime, &cli, "rpc", false, None, None);
            let applied = wiring::apply_registered_providers(
                &mut router,
                &mut models,
                loaded_extensions.runtime.providers(),
            );
            if !applied.is_empty() {
                tracing::debug!(providers = ?applied, "registered extension providers");
            }
            let stream_fn: SharedStreamFn = Arc::new(router.clone());
            let extension_resources = loaded_extensions.runtime.resource_paths().clone();
            let system_prompt = build_system_prompt_for(
                &cli,
                loaded_extensions.runtime.tool_prompts(),
                &extension_resources,
            );
            let prompt_templates = load_prompt_templates_for(&cli, &extension_resources);
            let tool_executor = loaded_extensions.executor.clone();
            let options = pi_coding_agent::rpc::RpcServerOptions {
                model: resolved_model,
                models,
                stream_fn,
                system_prompt,
                session_id,
                prompt_templates,
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

/// Run `pi --clear-history` and exit.
///
/// Deletes `<agent dir>/history.jsonl` — the composer's cross-session prompt
/// history — and prints the path it cleared. A missing file is a success ("no
/// history" is what the caller asked for), a permission error exits 74
/// (`EX_IOERR`) with the path on stderr.
fn clear_prompt_history_cli() -> ExitCode {
    let path = pi_coding_agent::paths::history_file_path();
    let store = pi_tui::history_store::HistoryStore::new(&path);
    match store.clear() {
        Ok(()) => {
            println!("Cleared prompt history: {}", path.display());
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("pi: could not clear {}: {err}", path.display());
            ExitCode::from(74)
        }
    }
}

/// Run `pi --export <session.jsonl> [output.html]` and exit.
///
/// Output strings match upstream `main.ts`: `Exported to: <path>` on
/// stdout with exit 0, `Error: <message>` on stderr with exit 1.
fn run_export_cli(input: &std::path::Path, output: Option<&std::path::Path>) -> ExitCode {
    match pi_coding_agent::commands::export::run_cli_export(input, output) {
        Ok(path) => {
            println!("Exported to: {}", path.display());
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("Error: {err}");
            ExitCode::from(1)
        }
    }
}

/// Build the system prompt for one agent mode, folding in the prompt
/// contributions declared by the mode's extension tools and the skills
/// its extensions advertised through `resources_discover`, and reporting
/// any skill diagnostics on stderr.
fn build_system_prompt_for(
    cli: &Cli,
    extension_tools: &[pi_extensions::RegisteredToolPrompt],
    extension_resources: &DiscoveredResources,
) -> String {
    // Resolved here rather than inside the loader so the notice can name
    // the directory; the loader takes the decision as a flag.
    let (trusted, has_trust_requiring_resources) = resolve_cli_project_trust(cli);
    if has_trust_requiring_resources && !trusted {
        let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        eprintln!(
            "pi: {} is not trusted — project .pi resources are ignored.\n    Use `pi --approve` for this run, or /trust inside an interactive session.",
            cwd.display()
        );
    }
    let (prompt, diagnostics) =
        build_cli_system_prompt_with_extensions(cli, extension_tools, extension_resources);
    for diagnostic in &diagnostics {
        match diagnostic.path.as_deref() {
            Some(path) => eprintln!("pi: skill {} ({})", diagnostic.message, path.display()),
            None => eprintln!("pi: skill {}", diagnostic.message),
        }
    }
    prompt
}

/// Load the prompt templates for one agent mode, folding in what the
/// loaded extensions advertised, and report diagnostics on stderr.
fn load_prompt_templates_for(
    cli: &Cli,
    extension_resources: &DiscoveredResources,
) -> Vec<pi_coding_agent::prompt_templates::PromptTemplate> {
    let loaded = build_cli_prompt_templates_with_extensions(cli, extension_resources);
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

/// The default session directory (`--session-dir`'s default).
///
/// Moved to [`pi_coding_agent::paths::default_session_dir`] so the interactive
/// exit hint can tell a default directory from a custom one without a second
/// copy of the rule.
fn default_session_dir() -> std::path::PathBuf {
    pi_coding_agent::paths::default_session_dir()
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
    ui_region_host: Option<std::sync::Arc<pi_coding_agent::extensions::ui_bridge::TuiRegionHost>>,
) -> wiring::ExtensionLoadOutcome {
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    // Resolve trust *before* discovery so an untrusted project's
    // `.pi/extensions` is never evaluated. This is the one-pass
    // counterpart of upstream's `loadProjectTrustExtensions` bootstrap:
    // the Rust `resolve_project_trusted` does not consume an extension
    // result, so there is nothing a pre-trust load pass would feed back.
    let (project_trusted, _) = resolve_cli_project_trust(cli);
    let options = ExtensionLoadOptions {
        home: home_dir(),
        cwd,
        explicit: wiring::explicit_paths(&cli.extension, &cli.extensions_dir),
        mode: mode.to_string(),
        has_ui,
        ui,
        ui_region_host,
        disabled: cli.no_extensions,
        project_trusted,
    };
    let outcome = wiring::load(runtime, &options);
    for (path, reason) in &outcome.errors {
        startup_log::log_message(&format!(
            "pi: extension load failed for {}: {reason}",
            path.display()
        ));
    }
    for name in &outcome.shadowed {
        startup_log::log_message(&format!(
            "pi: extension tool `{name}` ignored: a built-in tool already uses that name"
        ));
    }
    if !outcome.loaded.is_empty() {
        startup_log::log_message(&format!(
            "pi: loaded {} extension(s) [{}]",
            outcome.loaded.len(),
            outcome
                .loaded
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !outcome.runtime.commands().is_empty() {
        startup_log::log_message(&format!(
            "pi: extension commands: {}",
            outcome
                .runtime
                .commands()
                .iter()
                .map(|c| format!("/{}", c.name))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    outcome
}

fn build_default_models() -> Models {
    // The catalog is data, not code: every entry comes from
    // `pi_ai::providers::registry::BUILTIN_PROVIDERS`, the same table the
    // `ProviderRouter` uses to build streaming adapters. Adding a provider
    // therefore means one registry entry, not an edit here.
    //
    // P38: if `~/.pi/agent/models-cache.json` exists from a previous
    // `pi --update-models` (driven by `ModelsStore::refresh`), prefer it
    // — it's the catalog the user pulled last, with a validated schema.
    // The static user override `models.json` still wins when present.
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(agent_dir) = pi_coding_agent::paths::agent_dir() {
        let cache_path = agent_dir.join("models-cache.json");
        let store = pi_ai::ModelsStore::load_or_default(cache_path);
        if let Some(cached) = store.cached_models() {
            return cached;
        }
    }
    // P35: if the user has dropped a `models.json` at `~/.pi/agent/`,
    // parse + validate it through [`Models::load_models_json`] and let
    // it override (or augment) the registry. Missing file is silent (the
    // registry is the source of truth); schema errors are surfaced so a
    // typo never silently disables a provider.
    if let Some(agent_dir) = pi_coding_agent::paths::agent_dir() {
        let path = agent_dir.join("models.json");
        match Models::load_models_json(&path) {
            Ok(overrides) => return overrides,
            Err(pi_ai::LoadModelsError::Io { .. }) => {}
            Err(err) => {
                startup_log::log_message(&format!("pi: {err}"));
            }
        }
    }
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
