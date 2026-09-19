//! End-to-end smoke test that drives `AnthropicProvider` against the
//! real Anthropic Messages endpoint.
//!
//! Reads the API key from `ANTHROPIC_API_KEY`, streams a single
//! assistant turn, and prints the emitted `AssistantMessageEvent`s.
//!
//! The example is **not** part of `cargo test` — it makes a real
//! network call. Run it explicitly with:
//!
//! ```text
//! ANTHROPIC_API_KEY=sk-ant-... cargo run -p pi-ai --example anthropic_stream
//! ```

use futures::StreamExt;
use pi_ai::providers::anthropic::AnthropicProvider;
use pi_ai::StreamFn;
use pi_protocol::{Content, Context, Message, Model, ProviderId, Role};
use std::env;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = env::var("ANTHROPIC_API_KEY").map_err(|_| {
        "ANTHROPIC_API_KEY is not set; this example requires a real Anthropic API key"
    })?;
    let model_id =
        env::var("ANTHROPIC_MODEL").unwrap_or_else(|_| "claude-3-5-sonnet-latest".to_string());

    let model = Model {
        provider: ProviderId::new("anthropic"),
        id: model_id,
        api: pi_protocol::Api::AnthropicMessages,
        label: None,
        context_window: 200_000,
        max_output_tokens: 8_192,
    };

    let mut ctx = Context::new("You are a concise assistant. Answer in one sentence.");
    ctx.messages.push(Message {
        role: Role::User,
        content: vec![Content::text("Say hello in exactly two words.")],
        model: None,
    });

    let provider = AnthropicProvider::new(api_key);
    let mut stream = provider
        .stream_simple(&model, &ctx, &Default::default())
        .await?;

    while let Some(event) = stream.next().await {
        match event? {
            pi_protocol::AssistantMessageEvent::Start { model } => {
                println!("[start] {model}");
            }
            pi_protocol::AssistantMessageEvent::TextDelta { delta } => {
                print!("{delta}");
            }
            pi_protocol::AssistantMessageEvent::ToolCallDelta { index, name, .. } => {
                println!("[tool-call {index}] {name:?}");
            }
            pi_protocol::AssistantMessageEvent::Done {
                content,
                stop_reason,
                usage,
            } => {
                println!();
                println!("[done] stop_reason={stop_reason:?} usage={usage:?}");
                let text: String = content
                    .iter()
                    .filter_map(|c| match c {
                        Content::Text(t) => Some(t.text.clone()),
                        _ => None,
                    })
                    .collect();
                println!("[done] final_text={text:?}");
            }
            pi_protocol::AssistantMessageEvent::ThinkingDelta { delta } => {
                eprintln!("[think] {delta}");
            }
            pi_protocol::AssistantMessageEvent::Aborted => println!("[aborted]"),
            pi_protocol::AssistantMessageEvent::Error { message } => {
                eprintln!("[error] {message}");
            }
        }
    }
    Ok(())
}
