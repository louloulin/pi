//! `extensions` suite — load a JS tool extension and use it end-to-end.
//!
//! Upstream `extensions.eval.ts` asks a coding agent to *write*
//! `.pi/extensions/hello.ts`, reloads it, then asks the agent to use the
//! `hello` tool so the final response is exactly `Hello, Bob!`. Judging a
//! model-written file is not reproducible offline, so the Rust port fixes the
//! extension source (the same `module.exports = function (pi) { … }` shape as
//! the `pi-extensions` e2e fixture) and judges the runtime path instead:
//!
//! 1. the QuickJS host loads the source and registers a `hello` tool;
//! 2. `execute_tool("hello", {name: "Bob"})` returns the greeting;
//! 3. an agent driven by a fixture model calls `hello` and answers with the
//!    tool's result.
//!
//! The upstream checks on the *generated source* (canonical import specifier,
//! no legacy `@mariozechner` / `@sinclair/typebox` imports) are a TypeScript
//! packaging concern with no Rust analogue; they are recorded as a documented
//! divergence in `crates/pi-evals/README.md` instead of being faked.

use std::sync::Arc;

use async_trait::async_trait;
use pi_agent_core::{Agent, AgentError, AgentOptions, ToolExecutor};
use pi_ai::providers::openai::OpenAiProvider;
use pi_extensions::{ExtensionEntry, JsExtensionHost, ToolExecutionOutcome};
use pi_protocol::{Api, Content, ToolCall, ToolDefinition, ToolResult};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::fixture::{sse_text, sse_tool_call, FixtureResponse, FixtureServer};
use crate::harness::{Case, CaseOutput, EvalError, EvalSuite};
use crate::support::{self, run_agent};

/// Fixture extension: registers the `hello` tool used by every case.
///
/// Byte-identical in shape to `pi-extensions/tests/e2e.rs`, so a regression
/// in the shim ABI surfaces here as well.
const HELLO_JS: &str = r#"
module.exports = function (pi) {
    pi.registerTool({
        name: "hello",
        label: "Hello",
        description: "A simple greeting tool",
        parameters: {
            type: "object",
            properties: {
                name: { type: "string", description: "Name to greet" }
            },
            required: ["name"]
        },
        execute: function (args, _ctx) {
            return {
                content: [{ type: "text", text: "Hello, " + String(args.name) + "!" }],
                details: { greeted: args.name },
            };
        },
    });
};
"#;

const FIXTURE_MODEL: &str = "acme-chat";

fn entry(id: &str) -> ExtensionEntry {
    ExtensionEntry {
        source: std::path::PathBuf::from(format!("<eval-fixture>/{id}.js")),
        id: id.to_string(),
        label: None,
    }
}

async fn load_host() -> Result<JsExtensionHost, EvalError> {
    let host = JsExtensionHost::new()
        .await
        .map_err(|error| EvalError::Case(error.to_string()))?;
    host.load(entry("hello"), HELLO_JS)
        .await
        .map_err(|error| EvalError::Case(error.to_string()))?;
    Ok(host)
}

/// Loading the fixture registers the `hello` tool.
fn load_case() -> Case {
    Case::builder("extensions-load-registers-tool")
        .description("extension source loads without errors and registers the hello tool")
        .assertion("load", |output| {
            let expected = json!({
                "tools": ["hello"],
                "extension_ids": ["hello"],
                "required": ["name"],
            });
            if output.output != expected {
                return Err(format!(
                    "extension registration mismatch:\n expected {expected}\n      got {}",
                    output.output
                ));
            }
            Ok(())
        })
        .run(|| async {
            let host = load_host().await?;
            let tools = host.registered_tools();
            let names: Vec<String> = tools.iter().map(|tool| tool.name.clone()).collect();
            let required: Vec<String> = tools
                .iter()
                .find(|tool| tool.name == "hello")
                .and_then(|tool| tool.parameters.get("required"))
                .and_then(|value| value.as_array())
                .map(|values| values.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
                .unwrap_or_default();
            let extension_ids: Vec<String> = host
                .registered_extensions()
                .into_iter()
                .map(|extension| extension.id)
                .collect();
            Ok(CaseOutput {
                output: json!({
                    "tools": names,
                    "extension_ids": extension_ids,
                    "required": required,
                }),
                ..CaseOutput::default()
            }
            .with_artifact("hello.js", json!(HELLO_JS)))
        })
        .build()
}

/// The registered tool executes and returns the greeting.
fn execute_case() -> Case {
    Case::builder("extensions-execute-hello-tool")
        .description("hello tool returns `Hello, Bob!` for name Bob")
        .assertion("execute", |output| {
            let expected = json!({
                "is_error": false,
                "text": "Hello, Bob!",
                "details": { "greeted": "Bob" },
            });
            if output.output != expected {
                return Err(format!(
                    "tool outcome mismatch:\n expected {expected}\n      got {}",
                    output.output
                ));
            }
            Ok(())
        })
        .run(|| async {
            let host = load_host().await?;
            let outcome = host
                .execute_tool("hello", r#"{"name":"Bob"}"#)
                .await
                .map_err(|error| EvalError::Case(error.to_string()))?;
            let text = first_text(&outcome).unwrap_or_default();
            Ok(CaseOutput {
                output: json!({
                    "is_error": outcome.is_error,
                    "text": text,
                    "details": outcome.details.unwrap_or(json!({})),
                }),
                ..CaseOutput::default()
            })
        })
        .build()
}

/// Full path: fixture model asks for `hello`, the extension answers.
fn agent_round_trip_case() -> Case {
    Case::builder("extensions-agent-hello-round-trip")
        .description("agent executes the extension tool and answers with its greeting")
        .assertion("round-trip", |output| {
            let text = output.as_str().unwrap_or_default();
            if text != "Hello, Bob!" {
                return Err(format!("expected `Hello, Bob!`, got `{text}`"));
            }
            if output.usage.tool_calls != 1 {
                return Err(format!(
                    "expected one tool call, got {}",
                    output.usage.tool_calls
                ));
            }
            let greeted = output.events.iter().any(|event| {
                matches!(
                    event,
                    crate::harness::TranscriptEvent::ToolResult { name, content, .. }
                        if name == "hello" && content.to_string().contains("Hello, Bob!")
                )
            });
            if !greeted {
                return Err("tool result did not carry the extension's greeting".into());
            }
            Ok(())
        })
        .run(|| async {
            let host = load_host().await?;
            let definitions = host.registered_tools();
            let server = FixtureServer::start(|request| {
                let has_tool_result = request
                    .json()
                    .and_then(|body| {
                        body.get("messages")
                            .and_then(|messages| messages.as_array())
                            .map(|messages| {
                                messages.iter().any(|message| {
                                    message.get("role").and_then(|role| role.as_str())
                                        == Some("tool")
                                })
                            })
                    })
                    .unwrap_or(false);
                if has_tool_result {
                    FixtureResponse::sse(sse_text(FIXTURE_MODEL, "Hello, Bob!", 12, 3))
                } else {
                    FixtureResponse::sse(sse_tool_call(
                        FIXTURE_MODEL,
                        "hello",
                        "{\"name\":\"Bob\"}",
                        6,
                        4,
                    ))
                }
            })
            .map_err(|error| EvalError::Case(error.to_string()))?;
            let provider: pi_ai::SharedStreamFn =
                Arc::new(OpenAiProvider::with_base_url("fixture-key", server.base_url()));
            let model = support::model("acme", FIXTURE_MODEL, Api::OpenAiChatCompletions);
            let options = AgentOptions::new(model, provider, "Use tools when asked.")
                .with_tool_executor(Arc::new(ExtensionToolExecutor { host, definitions }));
            let agent = Agent::new(options);
            let run = run_agent(agent, "Greet Bob with the hello tool.").await?;
            Ok(CaseOutput::text(run.output())
                .with_usage(run.usage("acme", FIXTURE_MODEL))
                .with_events(run.transcript())
                .with_artifact("turns", json!(server.request_count())))
        })
        .build()
}

/// `ToolExecutor` that forwards calls into the QuickJS extension host.
struct ExtensionToolExecutor {
    host: JsExtensionHost,
    definitions: Vec<ToolDefinition>,
}

#[async_trait]
impl ToolExecutor for ExtensionToolExecutor {
    fn definitions(&self) -> Vec<ToolDefinition> {
        self.definitions.clone()
    }

    async fn execute(
        &self,
        call: &ToolCall,
        _signal: CancellationToken,
    ) -> Result<ToolResult, AgentError> {
        let args = serde_json::to_string(&call.arguments).map_err(|error| AgentError::Tool {
            tool: call.name.clone(),
            message: error.to_string(),
        })?;
        let outcome = self
            .host
            .execute_tool(&call.name, &args)
            .await
            .map_err(|error| AgentError::Tool {
                tool: call.name.clone(),
                message: error.to_string(),
            })?;
        Ok(ToolResult {
            tool_call_id: call.id.clone(),
            content: Box::new(Content::text(first_text(&outcome).unwrap_or_default())),
            is_error: outcome.is_error,
            details: outcome.details,
        })
    }
}

/// Concatenate the text blocks of a tool outcome.
fn first_text(outcome: &ToolExecutionOutcome) -> Option<String> {
    let text = outcome
        .content
        .iter()
        .filter_map(|block| {
            let block_type = block.get("type").and_then(|value| value.as_str());
            if block_type == Some("text") {
                block.get("text").and_then(|value| value.as_str())
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join("");
    (!text.is_empty()).then_some(text)
}

/// Build the `extensions` suite.
pub fn suite() -> EvalSuite {
    EvalSuite::new("extensions")
        .with_case(load_case())
        .with_case(execute_case())
        .with_case(agent_round_trip_case())
}
