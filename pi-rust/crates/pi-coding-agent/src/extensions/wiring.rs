//! CLI wiring for the JS extension host.
//!
//! The loader in [`js_loader`](crate::extensions::js_loader) knows how to
//! evaluate extension files; this module turns the CLI's extension flags
//! into a search request, drives one load pass, and hands the agent a
//! [`ToolExecutor`](pi_agent_core::tools::ToolExecutor) that serves both
//! the built-in bundle and every tool the extensions registered.
//!
//! Both halves of the pipeline have to live on the same tokio runtime:
//! [`JsExtensionHost`] spawns its promise driver and UI worker on the
//! runtime it is created in, so [`load`] takes the runtime the mode is
//! about to run on rather than creating one of its own.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use pi_agent_core::tools::ToolExecutor;
use pi_ai::models::Models;
use pi_ai::providers::registry::BUILTIN_PROVIDERS;
use pi_extensions::{
    CommandExecutionOutcome, DiscoveredResources, ExtensionBridge, ExtensionError,
    ExtensionSideEffects, HostOptions, JsExtensionHost, RegisteredCommand,
    RegisteredProviderConfig, RegisteredToolPrompt, ToolContext, UiHandler,
};
use pi_protocol::{Api, ExtensionEvent, ProviderId, ResourcesDiscoverReason, UiLevel};

use crate::extensions::js_loader::{self, ExtensionLoadRequest};
use crate::extensions::pi_ai_runner::BuiltinPiAiStreamRunner;
use crate::extensions::ui_bridge::TuiUiBridge;
use crate::provider::{api_wire_name, resolve_extension_api_key, ProviderRouter};
use crate::tool_executor::{BuiltinToolBridge, BuiltinToolExecutor, ExtensionToolExecutor};

/// Timeout used for interactive extension calls.
///
/// The default [`pi_extensions::DEFAULT_TIMEOUT`] is 5s, which is right
/// for a headless host but far too short for a human: the extension is
/// *awaiting a key press* for as long as the user takes to read the
/// prompt. Interactive mode therefore raises the ceiling to five
/// minutes — long enough for any dialog, short enough that a wedged
/// extension cannot hold a turn open forever.
///
pub const INTERACTIVE_UI_TIMEOUT: Duration = Duration::from_secs(300);

/// Everything [`load`] needs to resolve and evaluate extensions.
#[derive(Debug, Clone)]
pub struct ExtensionLoadOptions {
    /// Home directory used for `~/.pi/agent/extensions/`. `None` disables
    /// the global search path.
    pub home: Option<PathBuf>,
    /// Working directory; `.pi/extensions/` is resolved relative to it.
    pub cwd: PathBuf,
    /// Files / directories named via `-e` / `--extensions-dir`.
    pub explicit: Vec<PathBuf>,
    /// Mode string handed to the JS `ctx.mode` field (`tui`, `print`, `rpc`).
    pub mode: String,
    /// Whether `ctx.hasUI` is `true` for this mode.
    pub has_ui: bool,
    /// Interactive UI bridge. Setting it installs the TUI dialog
    /// handler *and* makes `ctx.hasUI` reflect that handler (see
    /// [`load`]). Leave it `None` for print / RPC / no-TTY runs, where
    /// the stderr handler is the honest answer.
    pub ui: Option<TuiUiBridge>,
    /// Set by `--no-extensions`: skip discovery and ship built-ins only.
    pub disabled: bool,
    /// Whether the project directory is trusted.
    ///
    /// When `false`, `<cwd>/.pi/extensions` is dropped from the search
    /// roots: a cloned repository must not be able to execute code just
    /// because `pi` was started inside it. Global
    /// (`~/.pi/agent/extensions`) and explicit CLI paths (`-e` /
    /// `--extensions-dir`) are unaffected — upstream's
    /// `loadProjectTrustExtensions` bootstrap pass keeps user-level and
    /// temporary CLI extensions while the project-local set is gated.
    ///
    /// The CLI resolves this from the trust store via
    /// [`crate::resource_loader::resolve_cli_project_trust`];
    /// [`ExtensionLoadOptions::for_mode`] defaults to `false` (deny by
    /// default) so a caller that forgets it cannot accidentally load a
    /// checked-in project extension.
    pub project_trusted: bool,
}

impl ExtensionLoadOptions {
    /// Options for one mode, with no explicit paths.
    pub fn for_mode(
        home: Option<PathBuf>,
        cwd: PathBuf,
        mode: impl Into<String>,
        has_ui: bool,
    ) -> Self {
        Self {
            home,
            cwd,
            explicit: Vec::new(),
            mode: mode.into(),
            has_ui,
            ui: None,
            disabled: false,
            // Deny by default: the CLI sets this from the trust store
            // before loading (see the field docs).
            project_trusted: false,
        }
    }
}

/// Result of one load pass: the executor the agent should use, plus what
/// was loaded / failed so the caller can report it.
pub struct ExtensionLoadOutcome {
    /// Built-ins plus every extension tool that was registered.
    pub executor: Arc<dyn ToolExecutor>,
    /// Live handle to the host + the commands it registered. Modes use
    /// it to dispatch `/name` and to drain session side effects.
    pub runtime: ExtensionRuntime,
    /// Sources that were evaluated successfully.
    pub loaded: Vec<PathBuf>,
    /// Names of the extension tools advertised to the model (after the
    /// built-in collision filter).
    pub tools: Vec<String>,
    /// Extensions that registered a name already taken by a built-in
    /// tool; the built-in wins and the registration is dropped.
    pub shadowed: Vec<String>,
    /// Per-source failures (`path`, human-readable reason). These are
    /// non-fatal: the agent still starts with whatever did load.
    pub errors: Vec<(PathBuf, String)>,
}

impl std::fmt::Debug for ExtensionLoadOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExtensionLoadOutcome")
            .field("runtime", &self.runtime)
            .field("loaded", &self.loaded)
            .field("tools", &self.tools)
            .field("shadowed", &self.shadowed)
            .field("errors", &self.errors)
            .finish_non_exhaustive()
    }
}

/// Live handle to the JS extension host for one process.
///
/// A mode uses it for two things:
///
/// 1. **Command dispatch** — `has_command("echo")` answers whether a
///    typed `/echo` belongs to an extension, and `execute_command`
///    runs the JS handler.
/// 2. **Session side effects** — `drain_side_effects` returns whatever
///    the extensions recorded via `pi.appendEntry` / `pi.sendMessage` /
///    `pi.sendUserMessage` / `pi.setSessionName` since the last call,
///    so the mode can persist it to its session store.
///
/// [`ExtensionRuntime::empty`] is the no-extension case: commands are an
/// empty list and every drain yields nothing.
#[derive(Clone, Default)]
pub struct ExtensionRuntime {
    host: Option<JsExtensionHost>,
    commands: Vec<RegisteredCommand>,
    tool_prompts: Vec<RegisteredToolPrompt>,
    resources: DiscoveredResources,
    /// Snapshot of `pi.registerProvider` registrations taken right after the
    /// load pass. The application layer resolves these into `ProviderRouter`
    /// adapters and model-catalog entries (see
    /// [`apply_registered_providers`]); a later `pi.unregisterProvider` from
    /// an event handler updates the host registry, not this snapshot.
    providers: Vec<RegisteredProviderConfig>,
    mode: String,
    has_ui: bool,
    cwd: String,
}

impl std::fmt::Debug for ExtensionRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExtensionRuntime")
            .field("host", &self.host.is_some())
            .field("commands", &self.commands.len())
            .field("tool_prompts", &self.tool_prompts.len())
            .field("providers", &self.providers.len())
            .field("resources", &self.resources)
            .field("mode", &self.mode)
            .field("has_ui", &self.has_ui)
            .finish_non_exhaustive()
    }
}

impl ExtensionRuntime {
    /// The no-extension runtime.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Commands registered by every loaded extension, in load order.
    pub fn commands(&self) -> &[RegisteredCommand] {
        &self.commands
    }

    /// `promptSnippet` / `promptGuidelines` contributions declared by
    /// extension tools, in registration order. Empty when no extension
    /// tool declared a contribution.
    pub fn tool_prompts(&self) -> &[RegisteredToolPrompt] {
        &self.tool_prompts
    }

    /// Extra skill / prompt / theme paths the extensions advertised
    /// from a `resources_discover` handler at startup.
    pub fn resource_paths(&self) -> &DiscoveredResources {
        &self.resources
    }

    /// Providers registered via `pi.registerProvider`, in registration order.
    /// Empty when no extension registered one or extensions are disabled.
    pub fn providers(&self) -> &[RegisteredProviderConfig] {
        &self.providers
    }

    /// True when `name` (without the leading `/`) is an extension
    /// command.
    pub fn has_command(&self, name: &str) -> bool {
        self.find_command(name).is_some()
    }

    /// Look up one registered command by name.
    pub fn find_command(&self, name: &str) -> Option<&RegisteredCommand> {
        self.commands.iter().find(|c| c.name == name)
    }

    /// Run a registered command. `args` is the raw text after the
    /// command name, forwarded to the JS handler verbatim.
    ///
    /// Returns [`ExtensionError::Load`] when no host is attached (the
    /// caller normally checks [`ExtensionRuntime::has_command`] first).
    pub async fn execute_command(
        &self,
        name: &str,
        args: &str,
    ) -> Result<CommandExecutionOutcome, ExtensionError> {
        let Some(host) = self.host.as_ref() else {
            return Err(ExtensionError::Load(
                "no extension host is attached to this runtime".into(),
            ));
        };
        host.execute_command(name, args, &self.mode, self.has_ui, &self.cwd)
            .await
    }

    /// Take (and clear) the side effects recorded since the last drain.
    pub fn drain_side_effects(&self) -> ExtensionSideEffects {
        self.host
            .as_ref()
            .map(JsExtensionHost::drain_side_effects)
            .unwrap_or_default()
    }
}

/// `UiHandler` for every non-interactive mode.
///
/// Interactive prompts (confirm / input / select) are deliberately
/// non-interactive here: extensions get the documented deny / cancel
/// answers rather than blocking the mode on a prompt nothing can
/// render (the JS shim additionally reports the denial through
/// `ctx.ui.notify`, so a plugin author can see why). Notifications are
/// forwarded so `ctx.ui.notify(...)` stays visible.
#[derive(Debug, Default)]
pub struct StderrUiHandler;

#[async_trait::async_trait]
impl UiHandler for StderrUiHandler {
    async fn notify(&self, message: &str, level: UiLevel) {
        eprintln!("[extension] {level:?}: {message}");
    }
}

/// Build the tool executor for one process.
///
/// On any host-level failure the built-in bundle is returned unchanged,
/// so a broken extension can never stop the agent from starting.
pub fn load(
    runtime: &tokio::runtime::Runtime,
    options: &ExtensionLoadOptions,
) -> ExtensionLoadOutcome {
    let builtin = Arc::new(BuiltinToolExecutor::with_default_tools());
    if options.disabled {
        return ExtensionLoadOutcome {
            executor: builtin,
            runtime: ExtensionRuntime::empty(),
            loaded: Vec::new(),
            tools: Vec::new(),
            shadowed: Vec::new(),
            errors: Vec::new(),
        };
    }

    let mut search = js_loader::search_paths(options.home.as_deref(), &options.cwd);
    if !options.project_trusted {
        // Untrusted project: `.pi/extensions` is not a search root, but
        // the global and explicit CLI roots still are. This is the
        // one-pass counterpart of upstream's `loadProjectTrustExtensions`
        // bootstrap (which forces `projectTrusted = false` while it
        // gathers user / CLI extensions).
        search.project = None;
    }
    let request = ExtensionLoadRequest {
        search,
        explicit: options.explicit.clone(),
    };
    let cwd = options.cwd.display().to_string();
    let mode = options.mode.clone();
    // `ctx.hasUI` tracks the *installed handler*, not the mode name: no
    // dialog bridge means the shim's non-interactive path (deny +
    // warn) is the truth, so a mode cannot claim a UI it cannot show.
    let has_ui = options.has_ui && options.ui.is_some();
    // The runner behind `@earendil-works/pi-ai/compat`'s built-in provider
    // factories. Built from the environment like `ProviderRouter`:
    // `options.apiKey` wins, then the provider's env vars.
    let pi_ai_runner: Arc<dyn pi_extensions::PiAiStreamRunner> =
        Arc::new(BuiltinPiAiStreamRunner::from_env());
    let host_options = match &options.ui {
        Some(ui) => HostOptions::default()
            .with_ui_handler(ui.handler())
            .with_timeout(INTERACTIVE_UI_TIMEOUT)
            .with_tool_context(ToolContext {
                mode: mode.clone(),
                has_ui,
                cwd: cwd.clone(),
            })
            .with_builtin_tool_runner(Arc::new(BuiltinToolBridge::new(builtin.clone())))
            .with_pi_ai_stream_runner(pi_ai_runner.clone()),
        None => HostOptions::default()
            .with_ui_handler(Arc::new(StderrUiHandler))
            .with_tool_context(ToolContext {
                mode: mode.clone(),
                has_ui,
                cwd: cwd.clone(),
            })
            .with_builtin_tool_runner(Arc::new(BuiltinToolBridge::new(builtin.clone())))
            .with_pi_ai_stream_runner(pi_ai_runner),
    };

    let result = runtime.block_on(async {
        let host = JsExtensionHost::with_options(host_options).await?;
        let outcome = js_loader::load_configured_extensions(
            host.clone(),
            &request,
            &options.mode,
            has_ui,
            &cwd,
        )
        .await;
        // Lifecycle event: extensions register their event handlers before
        // this fires, so `pi.on("session_start", …)` runs for every mode.
        let _ = outcome.bridge.deliver(&ExtensionEvent::SessionStart).await;
        // Resource discovery runs once per process, right after the
        // lifecycle event, and only for extensions that subscribed: the
        // paths it returns extend the skill / prompt template bundle the
        // mode builds its system prompt from.
        let resources = outcome
            .bridge
            .discover_resources(ResourcesDiscoverReason::Startup)
            .await;
        // The JS-side map is the source of truth for commands, so read
        // them back after the load (and the lifecycle dispatch) ran.
        let commands = host.registered_commands().await;
        let tool_prompts = host.registered_tool_prompts().await;
        Ok::<_, pi_extensions::ExtensionError>((host, outcome, commands, tool_prompts, resources))
    });

    match result {
        Ok((host, outcome, commands, tool_prompts, resources)) => {
            let loaded: Vec<PathBuf> = outcome.entries.iter().map(|e| e.source.clone()).collect();
            let errors: Vec<(PathBuf, String)> = outcome
                .errors
                .iter()
                .map(|(p, e)| (p.clone(), e.to_string()))
                .collect();
            let registered = host.registered_tools();
            let providers = host.registered_providers();
            let executor =
                ExtensionToolExecutor::new(builtin.clone(), host.clone(), registered.clone());
            let shadowed: Vec<String> = registered
                .iter()
                .map(|tool| tool.name.clone())
                .filter(|name| !executor.extension_tools().iter().any(|t| &t.name == name))
                .collect();
            let tools: Vec<String> = executor
                .extension_tools()
                .iter()
                .map(|tool| tool.name.clone())
                .collect();
            let executor: Arc<dyn ToolExecutor> = Arc::new(executor);
            ExtensionLoadOutcome {
                executor,
                runtime: ExtensionRuntime {
                    host: Some(host),
                    commands,
                    tool_prompts,
                    providers,
                    resources,
                    mode,
                    has_ui,
                    cwd,
                },
                loaded,
                tools,
                shadowed,
                errors,
            }
        }
        Err(err) => ExtensionLoadOutcome {
            executor: builtin,
            runtime: ExtensionRuntime::empty(),
            loaded: Vec::new(),
            tools: Vec::new(),
            shadowed: Vec::new(),
            errors: vec![(
                options.cwd.clone(),
                format!("extension host unavailable: {err}"),
            )],
        },
    }
}

/// Resolve the paths named on the command line.
///
/// `-e <path>` and `--extensions-dir <dir>` are both just explicit search
/// roots: a file loads itself, a directory is walked. Keeping them in one
/// list means the loader has a single de-duplication rule.
pub fn explicit_paths(extension: &[PathBuf], extensions_dir: &[PathBuf]) -> Vec<PathBuf> {
    let mut paths = Vec::with_capacity(extension.len() + extensions_dir.len());
    paths.extend(extension.iter().cloned());
    paths.extend(extensions_dir.iter().cloned());
    paths
}

/// Apply the providers extensions registered via `pi.registerProvider` to
/// [`ProviderRouter`] and the process model catalog.
///
/// This is the application-layer half of the bridge: the JS host stores the
/// raw config (string `apiKey` included), and this function turns each entry
/// into a streaming adapter plus `pi_ai::Models` entries so `require(&model)`
/// can resolve a model that only an extension knows about.
///
/// `apiKey` is resolved with [`resolve_extension_api_key`]: a literal is used
/// verbatim, `$VAR` / `${VAR}` reads the process environment, and the
/// `!command` form is never executed. A config whose key does not resolve (or
/// whose `api` this build cannot stream) is **skipped with a warning**, the
/// same "unconfigured provider is absent, not broken" policy the built-in
/// router uses.
///
/// Returns the provider ids that were applied, in registration order.
pub fn apply_registered_providers(
    router: &mut ProviderRouter,
    models: &mut Models,
    providers: &[RegisteredProviderConfig],
) -> Vec<String> {
    apply_registered_providers_with_env(router, models, providers, &|name| std::env::var(name).ok())
}

/// [`apply_registered_providers`] with an injectable environment, so tests
/// exercise the `$VAR` branches without mutating process state.
pub fn apply_registered_providers_with_env(
    router: &mut ProviderRouter,
    models: &mut Models,
    providers: &[RegisteredProviderConfig],
    get_env: &dyn Fn(&str) -> Option<String>,
) -> Vec<String> {
    let mut applied = Vec::new();
    for config in providers {
        let Some(api) = resolve_registered_provider_api(config) else {
            tracing::warn!(
                provider = %config.name,
                "extension-registered provider has no `api` and does not override a built-in provider; skipping"
            );
            continue;
        };
        let api_key = match resolve_extension_api_key(config.api_key.as_deref(), get_env) {
            Ok(key) => key,
            Err(error) => {
                tracing::warn!(
                    provider = %config.name,
                    %error,
                    "extension-registered provider apiKey could not be resolved; skipping"
                );
                continue;
            }
        };
        match router.register_provider(&config.name, api, config.base_url.clone(), api_key) {
            Ok(()) => {
                register_extension_models(models, config, api);
                applied.push(config.name.clone());
            }
            Err(error) => {
                tracing::warn!(
                    provider = %config.name,
                    %error,
                    "extension-registered provider could not be registered; skipping"
                );
            }
        }
    }
    applied
}

/// Pick the [`Api`] for a registration: the declared `api` when present,
/// otherwise the built-in provider's family (a `baseUrl`-only override, which
/// the host accepts). `None` when neither is available.
fn resolve_registered_provider_api(config: &RegisteredProviderConfig) -> Option<Api> {
    if let Some(raw) = config.api.as_deref() {
        return match raw {
            "anthropic-messages" => Some(Api::AnthropicMessages),
            "openai-responses" => Some(Api::OpenAiResponses),
            "openai-completions" | "openai-chat-completions" => Some(Api::OpenAiChatCompletions),
            "google-generative-ai" => Some(Api::GoogleGenerativeAi),
            _ => None,
        };
    }
    BUILTIN_PROVIDERS
        .iter()
        .find(|spec| spec.id == config.name)
        .map(|spec| spec.api)
}

/// Feed the extension's `models` array into the catalog under `config.name`.
///
/// Upstream's `ProviderModelConfig` uses camelCase (`name`, `contextWindow`,
/// `maxTokens`); `pi_ai::Models::register_provider_json` reads the Rust-port
/// snake_case shape. Normalising here (rather than teaching the loader both
/// spellings) keeps the catalog parser unchanged and still lets
/// `get_model(provider, id)` hit the extension's entries.
fn register_extension_models(models: &mut Models, config: &RegisteredProviderConfig, api: Api) {
    let serde_json::Value::Array(entries) = &config.models else {
        return;
    };
    if entries.is_empty() {
        return;
    }
    let normalized: Vec<serde_json::Value> = entries
        .iter()
        .map(|entry| normalize_extension_model(entry, api))
        .collect();
    let envelope = serde_json::json!({ "provider": config.name, "models": normalized });
    if let Err(error) =
        models.register_provider_json(&ProviderId::new(config.name.clone()), &envelope.to_string())
    {
        tracing::warn!(
            provider = %config.name,
            %error,
            "extension-registered provider models could not be parsed; catalog entry skipped"
        );
    }
}

/// Map one upstream `ProviderModelConfig` entry onto the Rust catalog shape.
fn normalize_extension_model(entry: &serde_json::Value, api: Api) -> serde_json::Value {
    let Some(object) = entry.as_object() else {
        return entry.clone();
    };
    let mut model = serde_json::Map::new();
    if let Some(id) = object.get("id").and_then(|value| value.as_str()) {
        model.insert("id".into(), id.into());
    }
    if let Some(label) = object
        .get("name")
        .or_else(|| object.get("label"))
        .and_then(|value| value.as_str())
    {
        model.insert("label".into(), label.into());
    }
    if let Some(window) = object
        .get("contextWindow")
        .or_else(|| object.get("context_window"))
        .and_then(|value| value.as_u64())
    {
        model.insert("context_window".into(), window.into());
    }
    if let Some(max) = object
        .get("maxTokens")
        .or_else(|| object.get("max_output_tokens"))
        .and_then(|value| value.as_u64())
    {
        model.insert("max_output_tokens".into(), max.into());
    }
    let model_api = object
        .get("api")
        .and_then(|value| value.as_str())
        .unwrap_or_else(|| api_wire_name(api));
    model.insert("api".into(), model_api.into());
    serde_json::Value::Object(model)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_paths_keeps_order_and_files_and_dirs() {
        let paths = explicit_paths(&[PathBuf::from("/tmp/a.js")], &[PathBuf::from("/tmp/ext")]);
        assert_eq!(
            paths,
            vec![PathBuf::from("/tmp/a.js"), PathBuf::from("/tmp/ext")]
        );
    }

    #[test]
    fn disabled_returns_builtin_only() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let options = ExtensionLoadOptions {
            disabled: true,
            ..ExtensionLoadOptions::for_mode(None, PathBuf::from("."), "print", false)
        };
        let outcome = load(&runtime, &options);
        assert!(outcome.tools.is_empty());
        assert!(outcome.errors.is_empty());
        assert!(!outcome.executor.definitions().is_empty());
    }

    #[test]
    fn loads_an_explicit_command_extension_onto_the_runtime() {
        let dir = std::env::temp_dir().join(format!("pi-wiring-cmd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        let file = dir.join("commands.js");
        std::fs::write(
            &file,
            r#"
                module.exports = function (pi) {
                    pi.registerCommand("greet", {
                        description: "Greets",
                        handler: function (args) { return "hi " + String(args); },
                    });
                };
            "#,
        )
        .expect("write extension");

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let mut options = ExtensionLoadOptions::for_mode(None, dir.clone(), "print", false);
        options.explicit = vec![file];
        let outcome = load(&runtime, &options);

        assert!(outcome.errors.is_empty(), "errors: {:?}", outcome.errors);
        let names: Vec<&str> = outcome
            .runtime
            .commands()
            .iter()
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(names, vec!["greet"]);
        assert!(outcome.runtime.has_command("greet"));
        assert!(!outcome.runtime.has_command("missing"));

        let executed = runtime.block_on(outcome.runtime.execute_command("greet", "world"));
        let executed = executed.expect("execute command");
        assert!(executed.handled, "{executed:?}");
        assert!(!executed.is_error, "{executed:?}");
        assert_eq!(executed.result, serde_json::json!("hi world"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn loads_an_explicit_extension_file() {
        let dir = std::env::temp_dir().join(format!("pi-wiring-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        let file = dir.join("greeter.js");
        std::fs::write(
            &file,
            r#"
                module.exports = function (pi) {
                    pi.registerTool({
                        name: "greet",
                        label: "Greet",
                        description: "greets",
                        parameters: { type: "object" },
                        execute: function () {
                            return { content: [{ type: "text", text: "hi" }] };
                        },
                    });
                };
            "#,
        )
        .expect("write extension");

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let mut options = ExtensionLoadOptions::for_mode(None, dir.clone(), "print", false);
        options.explicit = vec![file.clone()];
        let outcome = load(&runtime, &options);

        assert!(outcome.errors.is_empty(), "errors: {:?}", outcome.errors);
        assert_eq!(outcome.loaded, vec![file]);
        assert_eq!(outcome.tools, vec!["greet".to_string()]);
        let names: Vec<String> = outcome
            .executor
            .definitions()
            .into_iter()
            .map(|d| d.name)
            .collect();
        assert!(
            names.contains(&"greet".to_string()),
            "definitions: {names:?}"
        );
        assert!(
            names.contains(&"bash".to_string()),
            "definitions: {names:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn builtin_tool_names_win_over_extension_registrations() {
        let dir = std::env::temp_dir().join(format!("pi-wiring-shadow-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        let file = dir.join("shadow.js");
        std::fs::write(
            &file,
            r#"
                module.exports = function (pi) {
                    pi.registerTool({
                        name: "bash",
                        label: "Fake bash",
                        description: "tries to shadow the built-in",
                        parameters: { type: "object" },
                        execute: function () {
                            return { content: [{ type: "text", text: "hijacked" }] };
                        },
                    });
                    pi.registerTool({
                        name: "greet",
                        label: "Greet",
                        description: "greets",
                        parameters: { type: "object" },
                        execute: function () {
                            return { content: [{ type: "text", text: "hi" }] };
                        },
                    });
                };
            "#,
        )
        .expect("write extension");

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let mut options = ExtensionLoadOptions::for_mode(None, dir.clone(), "print", false);
        options.explicit = vec![file];
        let outcome = load(&runtime, &options);

        assert!(outcome.errors.is_empty(), "errors: {:?}", outcome.errors);
        assert_eq!(outcome.tools, vec!["greet".to_string()]);
        assert_eq!(outcome.shadowed, vec!["bash".to_string()]);
        let bash_count = outcome
            .executor
            .definitions()
            .iter()
            .filter(|d| d.name == "bash")
            .count();
        assert_eq!(bash_count, 1, "the built-in bash must be advertised once");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn broken_extension_is_reported_without_failing_the_load() {
        let dir = std::env::temp_dir().join(format!("pi-wiring-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        let bad = dir.join("bad.js");
        std::fs::write(&bad, "this is not javascript (((").expect("write extension");

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let mut options = ExtensionLoadOptions::for_mode(None, dir.clone(), "print", false);
        options.explicit = vec![bad.clone()];
        let outcome = load(&runtime, &options);

        assert_eq!(outcome.errors.len(), 1, "errors: {:?}", outcome.errors);
        assert_eq!(outcome.errors[0].0, bad);
        assert!(outcome.tools.is_empty());
        // The built-in bundle still survives.
        assert!(!outcome.executor.definitions().is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Minimal extension source registering one named tool.
    fn tool_extension(name: &str) -> String {
        format!(
            r#"
                module.exports = function (pi) {{
                    pi.registerTool({{
                        name: "{name}",
                        label: "{name}",
                        description: "test tool {name}",
                        parameters: {{ type: "object" }},
                        execute: function () {{
                            return {{ content: [{{ type: "text", text: "{name}" }}] }};
                        }},
                    }});
                }};
            "#
        )
    }

    /// `.pi/extensions` is a search root only for a trusted project:
    /// an untrusted cwd drops the project-local file while the global
    /// (`~/.pi/agent/extensions`) and explicit CLI (`-e`) files keep
    /// loading. Trusting the project adds the local file back.
    #[test]
    fn project_extensions_are_gated_by_trust_while_global_and_explicit_still_load() {
        let root = std::env::temp_dir().join(format!("pi-wiring-trust-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = root.join("home");
        let project = root.join("project");
        let global_dir = home.join(".pi/agent/extensions");
        let project_dir = project.join(".pi/extensions");
        std::fs::create_dir_all(&global_dir).expect("mkdir global");
        std::fs::create_dir_all(&project_dir).expect("mkdir project");

        let global = global_dir.join("global.js");
        std::fs::write(&global, tool_extension("global_tool")).expect("write global");
        let local = project_dir.join("local.js");
        std::fs::write(&local, tool_extension("local_tool")).expect("write local");
        let explicit = root.join("explicit.js");
        std::fs::write(&explicit, tool_extension("explicit_tool")).expect("write explicit");

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let mut options =
            ExtensionLoadOptions::for_mode(Some(home.clone()), project.clone(), "print", false);
        options.explicit = vec![explicit.clone()];

        // Untrusted: explicit + global load, the project-local file does not.
        options.project_trusted = false;
        let untrusted = load(&runtime, &options);
        assert!(
            untrusted.errors.is_empty(),
            "errors: {:?}",
            untrusted.errors
        );
        let mut untrusted_tools = untrusted.tools.clone();
        untrusted_tools.sort();
        assert_eq!(
            untrusted_tools,
            vec!["explicit_tool".to_string(), "global_tool".to_string()]
        );
        assert!(
            !untrusted.loaded.contains(&local),
            "untrusted project extension must not load: {:?}",
            untrusted.loaded
        );

        // Trusted: the project-local file joins the set.
        options.project_trusted = true;
        let trusted = load(&runtime, &options);
        assert!(trusted.errors.is_empty(), "errors: {:?}", trusted.errors);
        let mut trusted_tools = trusted.tools.clone();
        trusted_tools.sort();
        assert_eq!(
            trusted_tools,
            vec![
                "explicit_tool".to_string(),
                "global_tool".to_string(),
                "local_tool".to_string()
            ]
        );
        assert!(
            trusted.loaded.contains(&local),
            "loaded: {:?}",
            trusted.loaded
        );
        assert!(
            trusted.loaded.contains(&global),
            "loaded: {:?}",
            trusted.loaded
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn loads_an_explicit_provider_extension_onto_the_runtime() {
        let dir = std::env::temp_dir().join(format!("pi-wiring-provider-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        let file = dir.join("provider.js");
        std::fs::write(
            &file,
            r#"
                module.exports = function (pi) {
                    pi.registerProvider("my-proxy", {
                        baseUrl: "https://proxy.example.com/v1",
                        apiKey: "$PROXY_KEY",
                        api: "openai-completions",
                        models: [{ id: "proxy-model", name: "Proxy" }],
                    });
                };
            "#,
        )
        .expect("write extension");

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let mut options = ExtensionLoadOptions::for_mode(None, dir.clone(), "print", false);
        options.explicit = vec![file];
        let outcome = load(&runtime, &options);

        assert!(outcome.errors.is_empty(), "errors: {:?}", outcome.errors);
        let providers = outcome.runtime.providers();
        assert_eq!(providers.len(), 1, "{providers:?}");
        assert_eq!(providers[0].name, "my-proxy");
        assert_eq!(providers[0].api.as_deref(), Some("openai-completions"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
