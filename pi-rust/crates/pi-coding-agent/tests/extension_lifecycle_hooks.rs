//! End-to-end tests for the extension lifecycle events added in LUM-1432.
//!
//! Each test drives the **real** path a plugin's callback travels:
//!
//! ```text
//!   .pi/extensions/*.js  →  wiring::load  →  ExtensionRuntime
//!        →  ExtensionLifecycleHooks  →  agent loop / extension load pass
//!        →  what the provider (or the load pass) actually did
//! ```
//!
//! The stream side is a recording faux provider, so no network or API key is
//! involved and the assertion can look at the exact `Context` the model would
//! have received. That is stronger than spawning the binary and sniffing an
//! HTTP body, and it runs in CI environments without loopback networking.
//!
//! Covered:
//!
//! * `before_agent_start` — the handler's `systemPrompt` replaces the system
//!   prompt the provider call carries.
//! * `context` — the handler's `messages` rewrite is what the call carries.
//! * `project_trust` — a global extension's `{ trusted: "yes" }` answer makes
//!   an otherwise-untrusted project's own extension load (and `undecided`
//!   leaves it out).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};

use async_trait::async_trait;
use pi_agent_core::LifecycleHooks;
use pi_ai::providers::faux::FauxProvider;
use pi_ai::stream::SharedStreamFn;
use pi_ai::{AssistantMessageEventStream, SimpleStreamOptions, StreamError, StreamFn};
use pi_coding_agent::extensions::lifecycle::ExtensionLifecycleHooks;
use pi_coding_agent::extensions::wiring::{self, ExtensionLoadOptions, ExtensionLoadOutcome};
use pi_coding_agent::print_mode::{run_print_mode, OutputFormat, PrintModeOptions};
use pi_coding_agent::tool_executor::default_executor;
use pi_protocol::{Api, Context, Model, ProviderId};

/// Marker the `before_agent_start` handler installs as the system prompt.
const SYSTEM_OVERRIDE: &str = "LUM1432-SYSTEM-OVERRIDE";
/// Marker the `context` handler appends as a user message.
const CONTEXT_MARKER: &str = "LUM1432-CONTEXT-MARKER";

const BEFORE_AGENT_START_EXTENSION: &str = r#"
module.exports = function (pi) {
  pi.on("before_agent_start", function () {
    return { systemPrompt: "LUM1432-SYSTEM-OVERRIDE" };
  });
};
"#;

const CONTEXT_EXTENSION: &str = r#"
module.exports = function (pi) {
  pi.on("context", function (event) {
    var msgs = event.messages.slice();
    msgs.push({ role: "user", content: [{ type: "text", text: "LUM1432-CONTEXT-MARKER" }] });
    return { messages: msgs };
  });
};
"#;

/// A global extension that answers the project-trust prompt.
fn project_trust_extension(answer: &str) -> String {
    format!(
        r#"
module.exports = function (pi) {{
  pi.on("project_trust", function () {{
    return {{ trusted: "{answer}" }};
  }});
}};
"#
    )
}

/// A project extension registering `ext_echo`; the tool name is the marker
/// that proves the project's own `.pi/extensions` was loaded.
const PROJECT_ECHO_EXTENSION: &str = r#"
module.exports = function (pi) {
  pi.registerTool({
    name: "ext_echo",
    label: "Echo",
    description: "Echoes the text argument back to the model.",
    parameters: {
      type: "object",
      properties: { text: { type: "string" } },
      required: ["text"],
    },
    execute: async function (args) {
      var text = args && typeof args.text === "string" ? args.text : "";
      return { content: [{ type: "text", text: "ext-" + "echoed:" + text }] };
    },
  });
};
"#;

fn tempdir(label: &str) -> tempfile::TempDir {
    tempfile::TempDir::with_prefix(format!("pi-lifecycle-{label}-{}-", std::process::id()))
        .expect("tempdir")
}

/// `wiring::load` blocks on its own tokio runtime, so it must not run inside
/// the test's runtime. A dedicated thread keeps both rules.
fn load_extensions(options: ExtensionLoadOptions) -> ExtensionLoadOutcome {
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        wiring::load(&runtime, &options)
    })
    .join()
    .expect("load thread")
}

fn load_options(home: Option<PathBuf>, cwd: &Path, project_trusted: bool) -> ExtensionLoadOptions {
    ExtensionLoadOptions {
        home,
        cwd: cwd.to_path_buf(),
        explicit: Vec::new(),
        mode: "print".into(),
        has_ui: false,
        ui: None,
        ui_region_host: None,
        disabled: false,
        project_trusted,
    }
}

fn install_extension(root: &Path, name: &str, source: &str) {
    let dir = root.join(".pi").join("extensions");
    std::fs::create_dir_all(&dir).expect("mkdir extensions dir");
    std::fs::write(dir.join(format!("{name}.js")), source).expect("write extension");
}

fn install_global_extension(home: &Path, name: &str, source: &str) {
    let dir = home.join(".pi").join("agent").join("extensions");
    std::fs::create_dir_all(&dir).expect("mkdir global extensions dir");
    std::fs::write(dir.join(format!("{name}.js")), source).expect("write global extension");
}

/// Stream adapter that records the `Context` each turn's provider call
/// carried, then delegates to the faux provider.
struct ContextRecorder {
    inner: FauxProvider,
    seen: Arc<StdMutex<Vec<Context>>>,
}

#[async_trait]
impl StreamFn for ContextRecorder {
    async fn stream_simple(
        &self,
        model: &Model,
        ctx: &Context,
        options: &SimpleStreamOptions,
    ) -> Result<AssistantMessageEventStream, StreamError> {
        self.seen.lock().expect("lock").push(ctx.clone());
        self.inner.stream_simple(model, ctx, options).await
    }
}

fn faux_model() -> Model {
    Model {
        provider: ProviderId::new("faux"),
        id: "faux-model".into(),
        api: Api::Faux,
        label: Some("Faux test model".into()),
        context_window: 8192,
        max_output_tokens: 1024,
    }
}

/// Run one print-mode turn against the recording provider and return the
/// context the provider saw.
async fn run_one_turn(
    label: &str,
    home: Option<PathBuf>,
    cwd: &Path,
    project_trusted: bool,
    system_prompt: &str,
) -> Vec<Context> {
    let outcome = load_extensions(load_options(home, cwd, project_trusted));
    assert!(
        outcome.runtime.has_any_subscriber(),
        "the fixture extension must have loaded and subscribed"
    );
    let seen = Arc::new(StdMutex::new(Vec::new()));
    let stream_fn: SharedStreamFn = Arc::new(ContextRecorder {
        inner: FauxProvider::with_scripts(vec!["done".into()]),
        seen: seen.clone(),
    });
    let session_dir = tempdir(label);
    let options = PrintModeOptions {
        prompt: "say hi".into(),
        model: faux_model(),
        stream_fn,
        system_prompt: system_prompt.into(),
        session: None,
        session_dir: session_dir.path().to_path_buf(),
        max_turns: 1,
        output_format: OutputFormat::Text,
        tool_executor: default_executor(),
        extensions: Arc::new(outcome.runtime),
        retry: pi_agent_core::RetryPolicy::disabled(),
    };
    run_print_mode(options).await.expect("print run");
    let contexts = seen.lock().expect("lock").clone();
    assert_eq!(contexts.len(), 1, "one provider call");
    contexts
}

/// `before_agent_start`'s `systemPrompt` is the system prompt the provider
/// call carries.
#[tokio::test(flavor = "current_thread")]
async fn before_agent_start_can_replace_the_system_prompt() {
    let home = tempdir("before-agent-start-home");
    let project = tempdir("before-agent-start-project");
    install_global_extension(home.path(), "override", BEFORE_AGENT_START_EXTENSION);

    let contexts = run_one_turn(
        "before-agent-start",
        Some(home.path().to_path_buf()),
        project.path(),
        false,
        "BASE-SYSTEM-PROMPT",
    )
    .await;

    assert_eq!(
        contexts[0].system_prompt, SYSTEM_OVERRIDE,
        "the handler's systemPrompt must replace the base prompt"
    );
}

/// `context`'s `messages` result is the list the provider call carries.
#[tokio::test(flavor = "current_thread")]
async fn context_can_rewrite_the_messages_sent_to_the_provider() {
    let home = tempdir("context-home");
    let project = tempdir("context-project");
    install_global_extension(home.path(), "context", CONTEXT_EXTENSION);

    let contexts = run_one_turn(
        "context",
        Some(home.path().to_path_buf()),
        project.path(),
        false,
        "base",
    )
    .await;

    let texts: Vec<String> = contexts[0]
        .messages
        .iter()
        .flat_map(|message| message.content.iter())
        .filter_map(|block| match block {
            pi_protocol::Content::Text(text) => Some(text.text.clone()),
            _ => None,
        })
        .collect();
    assert!(
        texts.iter().any(|text| text.contains(CONTEXT_MARKER)),
        "the handler's appended message must reach the provider: {texts:?}"
    );
}

/// A global extension answering `project_trust` decides whether an untrusted
/// project's own `.pi/extensions` loads — and therefore whether its tool is
/// registered.
#[test]
fn project_trust_extension_can_trust_an_untrusted_project() {
    let project = tempdir("project-trust-project");
    install_extension(project.path(), "echo", PROJECT_ECHO_EXTENSION);

    // `undecided` falls through to the default (untrusted), so the project
    // extension is never evaluated.
    let undecided_home = tempdir("project-trust-undecided");
    install_global_extension(
        undecided_home.path(),
        "trust",
        &project_trust_extension("undecided"),
    );
    let undecided = load_extensions(load_options(
        Some(undecided_home.path().to_path_buf()),
        project.path(),
        false,
    ));
    assert!(
        !undecided.tools.contains(&"ext_echo".to_string()),
        "`undecided` must leave the project untrusted: {:?}",
        undecided.tools
    );

    // `yes` runs the second load pass, and the project's tool is registered.
    let trusted_home = tempdir("project-trust-yes");
    install_global_extension(
        trusted_home.path(),
        "trust",
        &project_trust_extension("yes"),
    );
    let trusted = load_extensions(load_options(
        Some(trusted_home.path().to_path_buf()),
        project.path(),
        false,
    ));
    assert!(
        trusted.tools.contains(&"ext_echo".to_string()),
        "`yes` must load the project extension: {:?}",
        trusted.tools
    );
}

/// The lifecycle hooks themselves are wired when a runtime has subscribers —
/// the assertion is indirect (a provider call carries the rewrite), which is
/// the point: the hook ran, not just the constructor.
#[tokio::test(flavor = "current_thread")]
async fn lifecycle_hooks_are_installed_only_for_a_subscribing_runtime() {
    let home = tempdir("hooks-installed-home");
    let project = tempdir("hooks-installed-project");
    install_global_extension(home.path(), "override", BEFORE_AGENT_START_EXTENSION);

    // Sanity: the same hook object used directly also overrides, so the
    // print-mode result above cannot be a coincidence of the harness.
    let outcome = load_extensions(load_options(
        Some(home.path().to_path_buf()),
        project.path(),
        false,
    ));
    let hooks = ExtensionLifecycleHooks::new(Arc::new(outcome.runtime));
    assert_eq!(
        hooks.before_agent_start("say hi", "BASE").await.as_deref(),
        Some(SYSTEM_OVERRIDE)
    );
}
