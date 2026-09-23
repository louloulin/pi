//! `smoke` suite — minimal end-to-end agent runs.
//!
//! Upstream `smoke.eval.ts` drives a real `AgentSession` against a live model
//! and asserts the answer, the empty error list, and that tokens were spent.
//! The Rust port keeps that contract but runs it **offline**: one case uses
//! the in-process faux provider, the other two point `OpenAiProvider` at the
//! loopback fixture server in [`crate::fixture`]. Only the live case in
//! [`crate::suites::providers`] needs a real API key.

use std::sync::Arc;

use async_trait::async_trait;
use pi_agent_core::{Agent, AgentError, AgentOptions, ToolExecutor};
use pi_ai::providers::faux::FauxProvider;
use pi_ai::providers::openai::OpenAiProvider;
use pi_protocol::{Api, Content, StopReason, ToolCall, ToolDefinition, ToolResult};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::fixture::{sse_text, sse_tool_call, FixtureResponse, FixtureServer};
use crate::harness::{CaseOutput, EvalError, EvalSuite};
use crate::support::{self, run_agent};

const FIXTURE_MODEL: &str = "fixture-chat";

/// `faux` provider answers a factual prompt.
fn faux_answer_case() -> crate::harness::Case {
    crate::harness::Case::builder("smoke-faux-answer")
        .description("faux provider returns the scripted answer end-to-end")
        .assertion("faux-answer", |output| {
            let text = output.as_str().unwrap_or_default();
            if text != "Paris" {
                return Err(format!("expected `Paris`, got `{text}`"));
            }
            if output.events.is_empty() {
                return Err("expected a non-empty transcript".into());
            }
            Ok(())
        })
        .run(|| async {
            let provider: pi_ai::SharedStreamFn =
                Arc::new(FauxProvider::with_scripts(vec!["Paris".into()]));
            let model = support::faux_model();
            let agent = Agent::new(AgentOptions::new(
                model,
                provider,
                "Answer factual questions concisely.",
            ));
            let run = run_agent(
                agent,
                "What's the capital of France? Respond with only the city name.",
            )
            .await?;
            let stop = run
                .assistant_messages
                .last()
                .map(|message| message.stop_reason);
            if stop != Some(StopReason::Stop) {
                return Err(EvalError::Case(format!(
                    "expected stop reason `Stop`, got {stop:?}"
                )));
            }
            Ok(CaseOutput::text(run.output())
                .with_usage(run.usage("faux", "faux-model"))
                .with_events(run.transcript())
                .with_artifact("event_count", json!(run.events.len())))
        })
        .build()
}

/// `OpenAiProvider` against a loopback SSE fixture.
fn fixture_answer_case() -> crate::harness::Case {
    crate::harness::Case::builder("smoke-openai-fixture-answer")
        .description("OpenAI provider parses a fixture SSE answer and reports usage")
        .assertion("fixture-answer", |output| {
            let text = output.as_str().unwrap_or_default();
            if text != "Paris" {
                return Err(format!("expected `Paris`, got `{text}`"));
            }
            if output.usage.total != 5 || output.usage.input != 3 || output.usage.output != 2 {
                return Err(format!(
                    "expected 3/2/5 tokens, got {}/{}/{}",
                    output.usage.input, output.usage.output, output.usage.total
                ));
            }
            if output.usage.model.as_deref() != Some(FIXTURE_MODEL) {
                return Err(format!("unexpected model {:?}", output.usage.model));
            }
            Ok(())
        })
        .run(|| async {
            let server = FixtureServer::start(|_request| {
                FixtureResponse::sse(sse_text(FIXTURE_MODEL, "Paris", 3, 2))
            })
            .map_err(|error| EvalError::Case(error.to_string()))?;
            let provider: pi_ai::SharedStreamFn = Arc::new(OpenAiProvider::with_base_url(
                "fixture-key",
                server.base_url(),
            ));
            let model = support::model("openai", FIXTURE_MODEL, Api::OpenAiChatCompletions);
            let agent = Agent::new(AgentOptions::new(model, provider, "Answer concisely."));
            let run = run_agent(agent, "What's the capital of France?").await?;
            let requests = server.requests();
            if requests.len() != 1 {
                return Err(EvalError::Case(format!(
                    "expected exactly one fixture request, got {}",
                    requests.len()
                )));
            }
            let request = &requests[0];
            if request.path != "/v1/chat/completions" {
                return Err(EvalError::Case(format!(
                    "unexpected request path {}",
                    request.path
                )));
            }
            Ok(CaseOutput::text(run.output())
                .with_usage(run.usage("openai", FIXTURE_MODEL))
                .with_events(run.transcript())
                .with_artifact("request_body", request.json().unwrap_or_default()))
        })
        .build()
}

/// Full tool round-trip: model asks for `hello`, the loop runs it, the model
/// answers with the tool output.
fn fixture_tool_turn_case() -> crate::harness::Case {
    crate::harness::Case::builder("smoke-openai-fixture-tool-turn")
        .description("agent executes a fixture-issued tool call and answers with its result")
        .assertion("tool-turn", |output| {
            let text = output.as_str().unwrap_or_default();
            if text != "Hello, Bob!" {
                return Err(format!("expected `Hello, Bob!`, got `{text}`"));
            }
            if support::tool_calls_named(&output.events, "hello").is_empty() {
                return Err("expected a `hello` tool call in the transcript".into());
            }
            if output.usage.tool_calls != 1 {
                return Err(format!(
                    "expected one tool call, got {}",
                    output.usage.tool_calls
                ));
            }
            if output.usage.total != 22 {
                return Err(format!(
                    "expected 22 total tokens (9 on the tool turn + 13 on the answer turn), got {}",
                    output.usage.total
                ));
            }
            Ok(())
        })
        .run(|| async {
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
                    FixtureResponse::sse(sse_text(FIXTURE_MODEL, "Hello, Bob!", 10, 3))
                } else {
                    FixtureResponse::sse(sse_tool_call(
                        FIXTURE_MODEL,
                        "hello",
                        "{\"name\":\"Bob\"}",
                        5,
                        4,
                    ))
                }
            })
            .map_err(|error| EvalError::Case(error.to_string()))?;
            let provider: pi_ai::SharedStreamFn = Arc::new(OpenAiProvider::with_base_url(
                "fixture-key",
                server.base_url(),
            ));
            let model = support::model("openai", FIXTURE_MODEL, Api::OpenAiChatCompletions);
            let options = AgentOptions::new(model, provider, "Use tools when asked.")
                .with_tool_executor(Arc::new(HelloTool));
            let agent = Agent::new(options);
            let run = run_agent(agent, "Greet Bob with the hello tool.").await?;
            if server.request_count() != 2 {
                return Err(EvalError::Case(format!(
                    "expected two fixture turns, got {}",
                    server.request_count()
                )));
            }
            Ok(CaseOutput::text(run.output())
                .with_usage(run.usage("openai", FIXTURE_MODEL))
                .with_events(run.transcript())
                .with_artifact("turns", json!(server.request_count())))
        })
        .build()
}

/// Minimal `hello` tool used by the tool-turn case.
struct HelloTool;

#[async_trait]
impl ToolExecutor for HelloTool {
    fn definitions(&self) -> Vec<ToolDefinition> {
        vec![ToolDefinition {
            name: "hello".into(),
            label: "Hello".into(),
            description: "Greet someone by name".into(),
            parameters: json!({
                "type": "object",
                "properties": { "name": { "type": "string" } },
                "required": ["name"]
            }),
            metadata: None,
        }]
    }

    async fn execute(
        &self,
        call: &ToolCall,
        _signal: CancellationToken,
    ) -> Result<ToolResult, AgentError> {
        let name = call
            .arguments
            .get("name")
            .and_then(|value| value.as_str())
            .unwrap_or("world");
        Ok(ToolResult {
            tool_call_id: call.id.clone(),
            content: Box::new(Content::text(format!("Hello, {name}!"))),
            is_error: false,
            details: None,
            added_tool_names: None,
        })
    }
}

/// Build the `smoke` suite.
pub fn suite() -> EvalSuite {
    EvalSuite::new("smoke")
        .with_case(faux_answer_case())
        .with_case(fixture_answer_case())
        .with_case(fixture_tool_turn_case())
}
