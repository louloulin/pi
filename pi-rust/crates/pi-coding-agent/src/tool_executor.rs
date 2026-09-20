//! Adapts the built-in tool bundle to the agent loop's tool executor.
//!
//! `pi-agent-core` knows how to drive tools but not which ones exist; this
//! module is the bridge. [`BuiltinToolExecutor`] owns the default bundle
//! (`read`, `write`, `edit`, `bash`, `find`, `grep`, `ls`) and implements
//! `pi_agent_core::ToolExecutor`, so the loop can advertise those tools to
//! the model and execute the calls it emits.

use std::collections::HashSet;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use pi_agent_core::tools::ToolExecutor;
use pi_agent_core::AgentError;
use pi_extensions::{
    BuiltinToolDefinition, BuiltinToolOutcome, BuiltinToolRunner, JsExtensionHost,
};
use pi_protocol::{Content, ToolCall, ToolDefinition, ToolExecutionMode, ToolResult};
use tokio_util::sync::CancellationToken;

use crate::tool_validation::coerce_tool_arguments;
use crate::tools::{default_tool_bundle, AbortLike, DynAgentTool, ToolError};

/// [`ToolExecutor`] backed by the built-in [`AgentTool`] bundle.
///
/// The tool list is held by registration order; [`definitions`](ToolExecutor::definitions)
/// and execution both preserve that order.
pub struct BuiltinToolExecutor {
    tools: Vec<DynAgentTool>,
}

impl BuiltinToolExecutor {
    /// Wrap an explicit tool list.
    pub fn new(tools: Vec<DynAgentTool>) -> Self {
        Self { tools }
    }

    /// Wrap [`default_tool_bundle`].
    pub fn with_default_tools() -> Self {
        Self::new(default_tool_bundle())
    }

    /// The tools this executor owns, in registration order.
    pub fn tools(&self) -> &[DynAgentTool] {
        &self.tools
    }
}

impl std::fmt::Debug for BuiltinToolExecutor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BuiltinToolExecutor")
            .field(
                "tools",
                &self
                    .tools
                    .iter()
                    .map(|tool| tool.name())
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

/// Build the default executor as the trait object the agent loop expects.
pub fn default_executor() -> Arc<dyn ToolExecutor> {
    Arc::new(BuiltinToolExecutor::with_default_tools())
}

/// Adapts the built-in tool bundle to the extension host's
/// [`BuiltinToolRunner`] bridge.
///
/// The host has to run a built-in tool from JavaScript (the JS
/// `create*Tool` factories), but `pi-extensions` must not depend on this
/// crate, so the bundle is injected as `Arc<dyn BuiltinToolRunner>`. A
/// single [`BuiltinToolExecutor`] is shared between the agent loop and
/// this bridge, so an extension that re-registers `read` delegates to the
/// exact same implementation the model calls.
pub struct BuiltinToolBridge {
    executor: Arc<BuiltinToolExecutor>,
}

impl BuiltinToolBridge {
    /// Wrap a shared built-in executor.
    pub fn new(executor: Arc<BuiltinToolExecutor>) -> Self {
        Self { executor }
    }
}

impl std::fmt::Debug for BuiltinToolBridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BuiltinToolBridge")
            .field("executor", &self.executor)
            .finish()
    }
}

impl BuiltinToolRunner for BuiltinToolBridge {
    fn definition(&self, name: &str) -> Option<BuiltinToolDefinition> {
        let tool = self
            .executor
            .tools()
            .iter()
            .find(|tool| tool.name() == name)?;
        Some(BuiltinToolDefinition {
            name: tool.name().to_string(),
            label: tool.label().to_string(),
            description: tool.description().to_string(),
            parameters: tool.parameters(),
        })
    }

    fn run<'a>(
        &'a self,
        name: String,
        mut args: serde_json::Value,
        cwd: Option<String>,
    ) -> Pin<Box<dyn Future<Output = Result<BuiltinToolOutcome, String>> + Send + 'a>> {
        Box::pin(async move {
            if let Some(cwd) = cwd.as_deref().filter(|value| !value.is_empty()) {
                rebase_arguments(&name, &mut args, cwd);
            }
            // The extension's own tool-call id is not forwarded across the
            // bridge, so the built-in result gets a synthetic one. Only the
            // extension that re-registers the tool can observe it, and the
            // shim keeps its own result shape.
            let call = ToolCall {
                id: format!("extension:{name}"),
                name,
                arguments: args,
            };
            match ToolExecutor::execute(self.executor.as_ref(), &call, CancellationToken::new())
                .await
            {
                Ok(result) => Ok(BuiltinToolOutcome {
                    content: vec![
                        serde_json::to_value(&*result.content).unwrap_or(serde_json::Value::Null)
                    ],
                    is_error: result.is_error,
                    details: result.details,
                }),
                // `BuiltinToolExecutor` reports an unknown name — and any
                // tool-level failure it could not fold into a result — as a
                // host-level error. Surface it as a structured `is_error`
                // outcome so the extension sees a failed tool, not a broken
                // bridge (mirrors `ToolExecutionOutcome`).
                Err(err) => Ok(BuiltinToolOutcome {
                    content: vec![serde_json::json!({"type": "text", "text": err.to_string()})],
                    is_error: true,
                    details: None,
                }),
            }
        })
    }
}

/// Rebase the extension-supplied `cwd` into the argument shape a built-in
/// tool understands.
///
/// `createReadTool(cwd)` promises that relative paths resolve against
/// `cwd`, but the Rust file tools resolve them against the *process* cwd
/// and `find` / `grep` / `ls` reject absolute paths outright. This helper
/// therefore makes `path` absolute for the tools that accept it and sets
/// `bash`'s own `cwd`; the navigation tools are left untouched and stay
/// anchored at the pi process cwd (documented divergence in
/// `docs/SDK_MODULES.md`).
fn rebase_arguments(name: &str, args: &mut serde_json::Value, cwd: &str) {
    let Some(object) = args.as_object_mut() else {
        return;
    };
    if name == "bash" {
        let already_set = object
            .get("cwd")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|value| !value.is_empty());
        if !already_set {
            object.insert(
                "cwd".to_string(),
                serde_json::Value::String(cwd.to_string()),
            );
        }
        return;
    }
    if !matches!(name, "read" | "write" | "edit") {
        return;
    }
    let Some(path) = object.get("path").and_then(serde_json::Value::as_str) else {
        return;
    };
    if Path::new(path).is_relative() {
        let joined = Path::new(cwd).join(path);
        object.insert(
            "path".to_string(),
            serde_json::Value::String(joined.to_string_lossy().into_owned()),
        );
    }
}

/// Execution mode for a name resolved against the built-in bundle of an
/// [`ExtensionToolExecutor`]: a built-in keeps its declared mode, and every
/// other name is an extension tool — or a stale call — which is serialized
/// out of caution.
fn extension_execution_mode(builtin: &BuiltinToolExecutor, tool_name: &str) -> ToolExecutionMode {
    if builtin.tools().iter().any(|tool| tool.name() == tool_name) {
        return builtin.execution_mode(tool_name);
    }
    ToolExecutionMode::Sequential
}

/// [`ToolExecutor`] that layers extension-registered tools on top of the
/// built-in bundle.
///
/// The built-in tools keep their names: an extension that registers a
/// tool under an existing name (`bash`, `read`, …) is ignored for that
/// name so a `.pi/extensions/*.js` file can never silently shadow a core
/// tool. Every other registered tool is advertised to the model and
/// executed inside the embedded QuickJS host, which is what makes the pi
/// plugin ecosystem reachable from the binary instead of only from the
/// library tests.
///
/// The [`JsExtensionHost`] must outlive the executor and must be driven
/// by the same tokio runtime that runs the agent loop (the host spawns
/// its promise driver on the runtime it was created in).
pub struct ExtensionToolExecutor {
    builtin: Arc<BuiltinToolExecutor>,
    host: JsExtensionHost,
    extension_tools: Vec<ToolDefinition>,
}

impl ExtensionToolExecutor {
    /// Combine a built-in bundle with the tools an extension host has
    /// registered. Extension tools whose name collides with a built-in
    /// are dropped (see the type docs).
    ///
    /// The built-in bundle is shared (`Arc`) so the same instance backs
    /// [`BuiltinToolBridge`], i.e. a `createReadTool(cwd)` an extension
    /// obtained runs the very tool the model calls.
    pub fn new(
        builtin: Arc<BuiltinToolExecutor>,
        host: JsExtensionHost,
        extension_tools: Vec<ToolDefinition>,
    ) -> Self {
        let mut seen: HashSet<String> = builtin
            .tools()
            .iter()
            .map(|tool| tool.name().to_string())
            .collect();
        let extension_tools = extension_tools
            .into_iter()
            .filter(|tool| seen.insert(tool.name.clone()))
            .collect();
        Self {
            builtin,
            host,
            extension_tools,
        }
    }

    /// The extension host backing this executor.
    pub fn host(&self) -> &JsExtensionHost {
        &self.host
    }

    /// The extension-registered tool definitions actually advertised
    /// (i.e. after the collision filter).
    pub fn extension_tools(&self) -> &[ToolDefinition] {
        &self.extension_tools
    }
}

impl std::fmt::Debug for ExtensionToolExecutor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExtensionToolExecutor")
            .field(
                "builtin",
                &self
                    .builtin
                    .tools()
                    .iter()
                    .map(|t| t.name())
                    .collect::<Vec<_>>(),
            )
            .field(
                "extensions",
                &self
                    .extension_tools
                    .iter()
                    .map(|t| t.name.as_str())
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

#[async_trait]
impl ToolExecutor for ExtensionToolExecutor {
    fn definitions(&self) -> Vec<ToolDefinition> {
        let mut definitions = self.builtin.definitions();
        definitions.extend(self.extension_tools.iter().cloned());
        definitions
    }

    /// Built-in tools keep their own declared mode. Extension tools are
    /// deliberately reported [`ToolExecutionMode::Sequential`]: they all run
    /// through one `JsExtensionHost`, whose interrupt deadline is armed and
    /// disarmed on shared host state, so two concurrent `execute_tool` calls
    /// would clobber each other's deadline.
    fn execution_mode(&self, tool_name: &str) -> ToolExecutionMode {
        extension_execution_mode(&self.builtin, tool_name)
    }

    async fn execute(
        &self,
        call: &ToolCall,
        signal: CancellationToken,
    ) -> Result<ToolResult, AgentError> {
        if self
            .builtin
            .tools()
            .iter()
            .any(|tool| tool.name() == call.name)
        {
            return self.builtin.execute(call, signal).await;
        }
        if !self
            .extension_tools
            .iter()
            .any(|tool| tool.name == call.name)
        {
            return Err(AgentError::Tool {
                tool: call.name.clone(),
                message: "unknown tool".to_string(),
            });
        }
        if signal.is_cancelled() {
            return Err(AgentError::Tool {
                tool: call.name.clone(),
                message: "operation aborted".to_string(),
            });
        }

        // Extension tools declare their schema in the same JSON Schema dialect,
        // so they get the same coercion as the built-ins before the arguments
        // cross into the QuickJS host.
        let arguments = match self
            .extension_tools
            .iter()
            .find(|definition| definition.name == call.name)
        {
            Some(definition) => coerce_tool_arguments(&definition.parameters, &call.arguments),
            None => call.arguments.clone(),
        };
        let args = arguments.to_string();
        match self.host.execute_tool(&call.name, &args).await {
            Ok(outcome) => {
                let (content, image_text) = fold_content(content_blocks_from_json(&outcome.content));
                Ok(ToolResult {
                    tool_call_id: call.id.clone(),
                    content: Box::new(content),
                    is_error: outcome.is_error,
                    details: with_image_text(outcome.details, image_text),
                    // Extension tools cannot advertise deferred tools yet —
                    // `pi_extensions::ToolExecutionOutcome` has no
                    // `addedToolNames` field (see `deferred_tools` module docs).
                    added_tool_names: None,
                })
            }
            // A host-level failure (timeout, JS exception, missing
            // execute function) is reported as an error *result*, not a
            // fatal loop error — the model gets to react to it, exactly
            // like a built-in tool that exited non-zero.
            Err(err) => Ok(ToolResult {
                tool_call_id: call.id.clone(),
                content: Box::new(Content::text(format!(
                    "extension tool `{}` failed: {err}",
                    call.name
                ))),
                is_error: true,
                details: None,
                added_tool_names: None,
            }),
        }
    }
}

/// Decode the JSON content blocks a JS extension returned into
/// [`Content`] values. Blocks that do not match the wire shape are kept
/// as their JSON text so nothing an extension produced is silently
/// dropped.
fn content_blocks_from_json(blocks: &[serde_json::Value]) -> Vec<Content> {
    blocks
        .iter()
        .map(|block| {
            serde_json::from_value::<Content>(block.clone())
                .unwrap_or_else(|_| Content::text(block.to_string()))
        })
        .collect()
}

#[async_trait]
impl ToolExecutor for BuiltinToolExecutor {
    fn definitions(&self) -> Vec<ToolDefinition> {
        self.tools.iter().map(|tool| tool.definition()).collect()
    }

    /// Each built-in tool declares its own mode; an unset mode means
    /// [`ToolExecutionMode::Parallel`], matching the upstream
    /// `executionMode === undefined` default. Unknown names are treated as
    /// parallel so a stale call cannot serialize a whole batch.
    fn execution_mode(&self, tool_name: &str) -> ToolExecutionMode {
        self.tools
            .iter()
            .find(|tool| tool.name() == tool_name)
            .and_then(|tool| tool.execution_mode())
            .unwrap_or(ToolExecutionMode::Parallel)
    }

    async fn execute(
        &self,
        call: &ToolCall,
        signal: CancellationToken,
    ) -> Result<ToolResult, AgentError> {
        let tool = self
            .tools
            .iter()
            .find(|tool| tool.name() == call.name)
            .ok_or_else(|| AgentError::Tool {
                tool: call.name.clone(),
                message: "unknown tool".to_string(),
            })?;

        // Best-effort bridge between the loop's async cancellation token and
        // the tools' synchronous `AbortLike` flag: a token that has already
        // fired maps to a cancelled handle, and a token that fires mid-call
        // is observed the next time the tool polls the flag.
        let abort = if signal.is_cancelled() {
            AbortLike::cancelled()
        } else {
            AbortLike::none()
        };

        // Coerce the model's arguments against the tool's JSON Schema before
        // the tool parses them. Models routinely emit `"5"` for an integer or
        // `"true"` for a flag; upstream's `validateToolArguments` coerces those
        // and every tool here matches upstream's acceptance. Final strictness
        // still comes from each tool's own `serde_json::from_value`.
        let arguments = coerce_tool_arguments(&tool.parameters(), &call.arguments);
        match tool.execute(arguments, abort).await {
            Ok(output) => {
                let (content, image_text) = fold_content(output.content);
                Ok(ToolResult {
                    tool_call_id: call.id.clone(),
                    content: Box::new(content),
                    is_error: false,
                    details: with_image_text(output.details, image_text),
                    // Built-in tools return a `ToolOutput`, which carries no
                    // `addedToolNames` equivalent (upstream `AgentToolResult`);
                    // none of them load tools mid-transcript.
                    added_tool_names: None,
                })
            }
            // Cancellation keeps a dedicated error path so callers can tell
            // an abort apart from a tool that legitimately failed.
            Err(ToolError::Aborted) => Err(AgentError::Tool {
                tool: call.name.clone(),
                message: "operation aborted".to_string(),
            }),
            Err(err) => Ok(ToolResult {
                tool_call_id: call.id.clone(),
                content: Box::new(Content::text(err.to_string())),
                is_error: true,
                details: None,
                added_tool_names: None,
            }),
        }
    }
}

/// Collapse a tool's content blocks into the single block
/// [`ToolResult::content`] carries.
///
/// Every built-in tool emits exactly one text block, so the common path just
/// moves it. Multi-block output is folded differently depending on whether an
/// image is involved:
///
/// * an image block wins the single `content` slot (upstream sends
///   `[text note, image]` for `read`, but `ToolResult::content` is one block)
///   and every sibling text block is returned as the second element so the
///   caller can park it under `details.image_text`;
/// * without an image, the blocks flatten into one text block as before, so
///   nothing is silently dropped.
fn fold_content(blocks: Vec<Content>) -> (Content, Option<String>) {
    let mut iter = blocks.into_iter();
    match (iter.next(), iter.next()) {
        (None, _) => (Content::text(""), None),
        (Some(only), None) => match only {
            Content::Image(image) => (Content::Image(image), None),
            other => (other, None),
        },
        (Some(first), Some(second)) => {
            let rest: Vec<Content> = std::iter::once(second).chain(iter).collect();
            let image = match &first {
                Content::Image(_) => Some(0usize),
                _ => rest
                    .iter()
                    .position(|block| matches!(block, Content::Image(_)))
                    .map(|idx| idx + 1),
            };
            match image {
                Some(0) => (first, Some(fold_text(&rest))),
                Some(idx) => {
                    let mut blocks: Vec<Content> = std::iter::once(first).chain(rest).collect();
                    let image_block = blocks.remove(idx);
                    (image_block, Some(fold_text(&blocks)))
                }
                None => {
                    let mut text = block_text(&first);
                    text.push('\n');
                    text.push_str(&fold_text(&rest));
                    (Content::text(text), None)
                }
            }
        }
    }
}

/// Join blocks' human-readable text with newlines.
fn fold_text(blocks: &[Content]) -> String {
    blocks
        .iter()
        .map(block_text)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Render a single content block as text for [`fold_content`].
fn block_text(block: &Content) -> String {
    match block {
        Content::Text(text) => text.text.clone(),
        Content::Image(image) => pi_tui::image_fallback(
            &image.mime_type,
            pi_tui::get_image_dimensions(&image.data, &image.mime_type),
            None,
        ),
        Content::ToolCall(call) => format!("[tool call {}]", call.name),
        Content::ToolResult(result) => format!("[tool result {}]", result.tool_call_id),
    }
}

/// Park the text that accompanied an image block under `details.image_text`.
fn with_image_text(details: Option<serde_json::Value>, text: Option<String>) -> Option<serde_json::Value> {
    let Some(text) = text else {
        return details;
    };
    let mut object = match details {
        Some(serde_json::Value::Object(object)) => object,
        Some(other) => {
            let mut object = serde_json::Map::new();
            object.insert("details".to_string(), other);
            object
        }
        None => serde_json::Map::new(),
    };
    object.insert("image_text".to_string(), serde_json::Value::String(text));
    Some(serde_json::Value::Object(object))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_modes_follow_the_tool_declarations() {
        let executor = BuiltinToolExecutor::with_default_tools();

        // `bash` touches the shell and `find` / `grep` / `ls` walk the tree,
        // so the loop must not run two of them at once.
        for name in ["bash", "find", "grep", "ls"] {
            assert_eq!(
                executor.execution_mode(name),
                ToolExecutionMode::Sequential,
                "{name} must serialize a batch"
            );
        }

        // The file tools are independent and declare no override, which the
        // executor reads as the upstream default (`Parallel`).
        for name in ["read", "write", "edit"] {
            assert_eq!(
                executor.execution_mode(name),
                ToolExecutionMode::Parallel,
                "{name} must be allowed to fan out"
            );
        }
    }

    #[test]
    fn unknown_builtin_name_is_parallel() {
        let executor = BuiltinToolExecutor::with_default_tools();
        assert_eq!(
            executor.execution_mode("no_such_tool"),
            ToolExecutionMode::Parallel,
            "a stale or renamed call must not serialize the batch"
        );
    }

    #[test]
    fn extension_tools_are_serialized() {
        let builtin = BuiltinToolExecutor::with_default_tools();

        // Not a built-in name → treated as an extension tool → Sequential.
        assert_eq!(
            extension_execution_mode(&builtin, "ext_exec"),
            ToolExecutionMode::Sequential
        );
        // Built-in names still report their own declared mode.
        assert_eq!(
            extension_execution_mode(&builtin, "bash"),
            ToolExecutionMode::Sequential
        );
        assert_eq!(
            extension_execution_mode(&builtin, "read"),
            ToolExecutionMode::Parallel
        );
    }

    #[test]
    fn an_image_block_wins_the_single_content_slot() {
        let image = Content::Image(pi_protocol::ImageContent {
            mime_type: "image/png".to_string(),
            data: "iVBORw0KGgoAAAANSUhEUgAAAUAAAADw".to_string(),
        });
        let (content, image_text) = fold_content(vec![
            Content::text("Read image file [image/png]"),
            image.clone(),
        ]);
        assert_eq!(content, image, "the image must stay the content block");
        assert_eq!(
            image_text.as_deref(),
            Some("Read image file [image/png]"),
            "the caption moves to details.image_text"
        );

        // `ToolResult::content` is one block, so `details.image_text` is where
        // the caption survives to the renderers.
        let details = with_image_text(Some(serde_json::json!({ "truncation": 1 })), image_text)
            .expect("details");
        assert_eq!(details["image_text"], "Read image file [image/png]");
        assert_eq!(details["truncation"], 1);

        // Without an image the old text folding is unchanged.
        let (content, image_text) = fold_content(vec![Content::text("a"), Content::text("b")]);
        assert_eq!(content, Content::text("a\nb"));
        assert!(image_text.is_none());
    }
}
