//! `providers` suite — provider authoring, dispatch and wire format.
//!
//! Upstream `providers.eval.ts` has a coding agent *write* a provider
//! registration (env var, base URL, model metadata), reloads it, and then
//! probes the runtime through a fake Acme HTTP server to prove the model is
//! reachable. The Rust port splits that into offline, deterministic checks
//! against the same wire contract:
//!
//! 1. [`providers-openai-compatible-request`] drives `OpenAiProvider`
//!    through an agent against a fixture server that validates the request
//!    exactly like the upstream Acme handler (auth, content type, model,
//!    `stream: true`, prompt) and returns the `ACME_OK` SSE answer.
//! 2. [`providers-router-dispatch`] proves `ProviderRouter` builds a working
//!    adapter from credential + base-URL env vars (the state the agent's
//!    "add a provider" edit produces), and that a missing credential fails
//!    with the env var name instead of silently streaming from faux.
//! 3. [`providers-model-metadata-divergence`] records the metadata the Rust
//!    `Model` / `ProviderSpec` carry versus the upstream `Model` type.
//!
//! Documented divergence: upstream also covers a *custom* NDJSON streaming
//! provider with its own adapter. This build has adapters only for the four
//! first-party API families, so that scenario has no Rust analogue yet; the
//! fixture server is still used, just for the API family that exists.
//!
//! The only network case is [`providers-live-openai`], skipped unless
//! `PI_EVAL_LIVE=1` **and** an `OPENAI_API_KEY` are present.

use std::collections::HashMap;
use std::sync::Arc;

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::openai::{OpenAiProvider, DEFAULT_BASE_URL};
use pi_coding_agent::{ProviderError, ProviderRouter};
use pi_protocol::Api;
use serde_json::{json, Value};

use crate::fixture::{sse_text, FixtureResponse, FixtureServer};
use crate::harness::{Case, CaseOutput, EvalError, EvalSuite};
use crate::support::{self, run_agent};

/// Model the OpenAI-compatible fixture registers.
const API_MODEL_ID: &str = "acme-chat";
/// Credential the fixture server accepts.
const API_KEY: &str = "resolved-acme-key";
/// Prompt the upstream probe sends.
const PROBE_PROMPT: &str = "Reply with ACME_OK.";
/// Answer the fixture streams back.
const PROBE_RESPONSE: &str = "ACME_OK";

/// Drive `OpenAiProvider` against a validator fixture server.
fn request_shape_case() -> Case {
    Case::builder("providers-openai-compatible-request")
        .description("OpenAI-compatible provider sends the expected request and parses the answer")
        .assertion("request-shape", |output| {
            let expected = json!({
                "path": "/v1/chat/completions",
                "method": "POST",
                "authorization": "Bearer resolved-acme-key",
                "content_type_json": true,
                "model": API_MODEL_ID,
                "stream": true,
                "user_prompt": PROBE_PROMPT,
                "system_prompt_present": true,
                "text": PROBE_RESPONSE,
                "stop_reason": "stop",
                "input_tokens": 3,
                "output_tokens": 2,
            });
            if output.output != expected {
                return Err(format!(
                    "request/response mismatch:\n expected {expected}\n      got {}",
                    output.output
                ));
            }
            Ok(())
        })
        .run(|| async {
            let server = FixtureServer::start(acme_handler)
                .map_err(|error| EvalError::Case(error.to_string()))?;
            let provider: pi_ai::SharedStreamFn =
                Arc::new(OpenAiProvider::with_base_url(API_KEY, server.base_url()));
            let model = support::model("acme", API_MODEL_ID, Api::OpenAiChatCompletions);
            let agent = Agent::new(AgentOptions::new(
                model,
                provider,
                "You are Acme's probe.\nGuidelines:\n- reply verbatim.",
            ));
            let run = run_agent(agent, PROBE_PROMPT).await?;

            let requests = server.requests();
            if requests.len() != 1 {
                return Err(EvalError::Case(format!(
                    "expected exactly one request, got {}",
                    requests.len()
                )));
            }
            let request = &requests[0];
            let body = request.json().unwrap_or(Value::Null);
            let messages = body
                .get("messages")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let system_prompt_present = messages
                .iter()
                .any(|message| message.get("role").and_then(Value::as_str) == Some("system"));
            let user_prompt = messages
                .iter()
                .find(|message| message.get("role").and_then(Value::as_str) == Some("user"))
                .and_then(|message| message.get("content"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let stop_reason = run
                .assistant_messages
                .last()
                .map(|message| format!("{:?}", message.stop_reason).to_lowercase())
                .unwrap_or_default();

            Ok(CaseOutput {
                output: json!({
                    "path": request.path,
                    "method": request.method,
                    "authorization": request.header("authorization").unwrap_or_default(),
                    "content_type_json": request
                        .header("content-type")
                        .map(|value| value.starts_with("application/json"))
                        .unwrap_or(false),
                    "model": body.get("model").and_then(Value::as_str).unwrap_or_default(),
                    "stream": body.get("stream").and_then(Value::as_bool).unwrap_or(false),
                    "user_prompt": user_prompt,
                    "system_prompt_present": system_prompt_present,
                    "text": run.output(),
                    "stop_reason": stop_reason,
                    "input_tokens": run.usage("acme", API_MODEL_ID).input,
                    "output_tokens": run.usage("acme", API_MODEL_ID).output,
                }),
                ..CaseOutput::default()
            })
        })
        .build()
}

/// `ProviderRouter` dispatches to the provider configured through env vars.
fn router_dispatch_case() -> Case {
    Case::builder("providers-router-dispatch")
        .description("ProviderRouter resolves env-configured providers and reports missing credentials")
        .assertion("router-dispatch", |output| {
            let expected = json!({
                "provider_ids": ["deepseek", "faux"],
                "dispatched": true,
                "path": "/v1/chat/completions",
                "authorization": "Bearer resolved-acme-key",
                "model": "deepseek-chat",
                "text": PROBE_RESPONSE,
                "missing_key_vars": "OPENAI_API_KEY",
                "faux_available": true,
            });
            if output.output != expected {
                return Err(format!(
                    "router behavior mismatch:\n expected {expected}\n      got {}",
                    output.output
                ));
            }
            Ok(())
        })
        .run(|| async {
            let server = FixtureServer::start(move |_request| {
                FixtureResponse::sse(sse_text("deepseek-chat", PROBE_RESPONSE, 3, 2))
            })
            .map_err(|error| EvalError::Case(error.to_string()))?;

            let mut env = HashMap::new();
            env.insert("DEEPSEEK_API_KEY".to_string(), API_KEY.to_string());
            env.insert("DEEPSEEK_BASE_URL".to_string(), server.base_url());
            let router = ProviderRouter::from_env_with(|name| env.get(name).cloned());

            let deepseek = support::model("deepseek", "deepseek-chat", Api::OpenAiChatCompletions);
            let adapter = router
                .require(&deepseek)
                .map_err(|error| EvalError::Case(error.to_string()))?;
            let agent = Agent::new(AgentOptions::new(
                deepseek,
                adapter,
                "Answer concisely.",
            ));
            let run = run_agent(agent, PROBE_PROMPT).await?;

            let requests = server.requests();
            let dispatch_ok = requests.len() == 1;
            let request = requests.first();

            let missing = router.require(&support::model(
                "openai",
                "gpt-4o-mini",
                Api::OpenAiChatCompletions,
            ));
            let missing_key_vars = match missing {
                Err(ProviderError::MissingApiKey { vars, .. }) => vars,
                Err(other) => {
                    return Err(EvalError::Case(format!(
                        "expected MissingApiKey for openai, got {other}"
                    )))
                }
                Ok(_) => {
                    return Err(EvalError::Case(
                        "expected MissingApiKey for openai, got a registered adapter".into(),
                    ))
                }
            };
            let faux_available = router.require(&support::faux_model()).is_ok();

            Ok(CaseOutput {
                output: json!({
                    "provider_ids": router.provider_ids(),
                    "dispatched": dispatch_ok,
                    "path": request.map(|r| r.path.clone()).unwrap_or_default(),
                    "authorization": request
                        .and_then(|r| r.header("authorization"))
                        .unwrap_or_default(),
                    "model": request
                        .and_then(|r| r.json())
                        .and_then(|body| body.get("model").and_then(Value::as_str).map(str::to_string))
                        .unwrap_or_default(),
                    "text": run.output(),
                    "missing_key_vars": missing_key_vars,
                    "faux_available": faux_available,
                }),
                ..CaseOutput::default()
            })
        })
        .build()
}

/// Record the metadata the Rust model / provider types carry.
fn metadata_divergence_case() -> Case {
    Case::builder("providers-model-metadata-divergence")
        .description("documents Rust Model / ProviderSpec metadata versus the upstream Model type")
        .assertion("metadata-divergence", |output| {
            let expected = json!({
                "model_fields": [
                    "api",
                    "context_window",
                    "id",
                    "label",
                    "max_output_tokens",
                    "provider"
                ],
                "upstream_only_fields": [
                    "cost",
                    "input",
                    "maxTokens",
                    "name",
                    "reasoning"
                ],
                "display_name": "DeepSeek",
                "api": "openai_chat_completions",
                "requires_api_key": true,
            });
            if output.output != expected {
                return Err(format!(
                    "metadata mismatch:\n expected {expected}\n      got {}",
                    output.output
                ));
            }
            Ok(())
        })
        .run(|| async {
            let spec = pi_ai::providers::registry::find_provider("deepseek")
                .ok_or_else(|| EvalError::Case("deepseek spec missing".into()))?;
            let mut model = support::model("deepseek", "deepseek-chat", Api::OpenAiChatCompletions);
            model.label = Some("DeepSeek Chat".into());
            let mut fields: Vec<String> = serde_json::to_value(&model)?
                .as_object()
                .map(|object| object.keys().cloned().collect())
                .unwrap_or_default();
            fields.sort();
            Ok(CaseOutput {
                output: json!({
                    "model_fields": fields,
                    // Present upstream, absent from the Rust `Model`: the
                    // port keeps only what the adapter needs on the wire.
                    "upstream_only_fields": ["cost", "input", "maxTokens", "name", "reasoning"],
                    "display_name": spec.display_name,
                    "api": match spec.api {
                        Api::OpenAiChatCompletions => "openai_chat_completions",
                        Api::AnthropicMessages => "anthropic_messages",
                        Api::OpenAiResponses => "openai_responses",
                        Api::GoogleGenerativeAi => "google_generative_ai",
                        Api::BedrockConverse => "bedrock_converse",
                        Api::CohereV2 => "cohere_v2",
                        Api::MistralConversations => "mistral_conversations",
                        Api::AzureOpenAiResponses => "azure_openai_responses",
                        Api::Faux => "faux",
                    },
                    "requires_api_key": spec.requires_api_key(),
                }),
                ..CaseOutput::default()
            })
        })
        .build()
}

/// Real-API probe, skipped unless `PI_EVAL_LIVE=1` and `OPENAI_API_KEY` exist.
fn live_openai_case() -> Case {
    let skip_reason = if !support::env_flag("PI_EVAL_LIVE") {
        Some("set PI_EVAL_LIVE=1 to run live provider evals".to_string())
    } else if support::env_string("OPENAI_API_KEY").is_none() {
        Some("set OPENAI_API_KEY to run the live OpenAI eval".to_string())
    } else {
        None
    };
    Case::builder("providers-live-openai")
        .description("live OpenAI Chat Completions probe (network; opt in via PI_EVAL_LIVE=1)")
        .skip_reason(skip_reason)
        .assertion("live-openai", |output| {
            let text = output.as_str().unwrap_or_default();
            if text.trim().is_empty() {
                return Err("live provider returned no text".into());
            }
            if output.usage.total == 0 {
                return Err("live provider reported no token usage".into());
            }
            Ok(())
        })
        .run(|| async {
            let api_key = support::env_string("OPENAI_API_KEY")
                .ok_or_else(|| EvalError::Case("OPENAI_API_KEY is not set".into()))?;
            let model_id =
                support::env_string("PI_EVAL_MODEL").unwrap_or_else(|| "gpt-4o-mini".to_string());
            let base_url = support::env_string("OPENAI_BASE_URL")
                .or_else(|| support::env_string("PI_EVAL_BASE_URL"))
                .unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
            let provider: pi_ai::SharedStreamFn =
                Arc::new(OpenAiProvider::with_base_url(api_key, base_url));
            let model = support::model("openai", &model_id, Api::OpenAiChatCompletions);
            let agent = Agent::new(AgentOptions::new(
                model,
                provider,
                "Answer in one short sentence.",
            ));
            let run = run_agent(agent, "Reply with ACME_OK.").await?;
            Ok(CaseOutput::text(run.output()).with_usage(run.usage("openai", &model_id)))
        })
        .build()
}

/// Upstream Acme handler: validate the request, then stream `ACME_OK`.
fn acme_handler(request: &crate::fixture::RecordedRequest) -> FixtureResponse {
    if request.path != "/v1/chat/completions" {
        return FixtureResponse::error(404, "Unknown endpoint");
    }
    if request.method != "POST" {
        return FixtureResponse::error(405, "Expected POST");
    }
    if !request
        .header("content-type")
        .map(|value| value.starts_with("application/json"))
        .unwrap_or(false)
    {
        return FixtureResponse::error(415, "Expected application/json");
    }
    let Some(body) = request.json() else {
        return FixtureResponse::error(400, "Invalid JSON");
    };
    if request.header("authorization") != Some(format!("Bearer {API_KEY}").as_str()) {
        return FixtureResponse::error(401, "Invalid Acme credential");
    }
    let prompt = body
        .get("messages")
        .and_then(Value::as_array)
        .and_then(|messages| {
            messages
                .iter()
                .find(|message| message.get("role").and_then(Value::as_str) == Some("user"))
        })
        .and_then(|message| message.get("content"))
        .and_then(Value::as_str);
    if body.get("model").and_then(Value::as_str) != Some(API_MODEL_ID)
        || prompt != Some(PROBE_PROMPT)
        || body.get("stream").and_then(Value::as_bool) != Some(true)
    {
        return FixtureResponse::error(422, "Invalid OpenAI-compatible request");
    }
    FixtureResponse::sse(sse_text(API_MODEL_ID, PROBE_RESPONSE, 3, 2))
}

/// Build the `providers` suite.
pub fn suite() -> EvalSuite {
    EvalSuite::new("providers")
        .with_case(request_shape_case())
        .with_case(router_dispatch_case())
        .with_case(metadata_divergence_case())
        .with_case(live_openai_case())
}
