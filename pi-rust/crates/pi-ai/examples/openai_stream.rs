//! End-to-end smoke test that drives `OpenAiProvider` against the real
//! OpenAI Chat Completions endpoint.
//!
//! Reads the API key from `OPENAI_API_KEY`, streams a single assistant
//! turn, and prints the emitted `AssistantMessageEvent`s.
//!
//! The example is **not** part of `cargo test` — it makes a real network
//! call. Run it explicitly with:
//!
//! ```text
//! OPENAI_API_KEY=sk-... cargo run -p pi-ai --example openai_stream
//! ```

use futures::StreamExt;
use pi_ai::providers::openai::OpenAiProvider;
use pi_ai::StreamFn;
use pi_protocol::{Content, Context, Message, Model, ProviderId, Role};
use std::env;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = env::var("OPENAI_API_KEY")
        .map_err(|_| "OPENAI_API_KEY is not set; this example requires a real OpenAI API key")?;
    let model_id = env::var("OPENAI_MODEL").unwrap_or_else(|_| "gpt-4o-mini".to_string());

    let model = Model {
        provider: ProviderId::new("openai"),
        id: model_id,
        api: pi_protocol::Api::OpenAiChatCompletions,
        label: None,
        context_window: 128_000,
        max_output_tokens: 4096,
    };

    let mut ctx = Context::new("You are a concise assistant. Answer in one sentence.");
    ctx.messages.push(Message {
        role: Role::User,
        content: vec![Content::text("Say hello in exactly two words.")],
        model: None,
    });

    let provider = OpenAiProvider::new(api_key);
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
                print!("{delta}");
            }
            pi_protocol::AssistantMessageEvent::Aborted => println!("[aborted]"),
            pi_protocol::AssistantMessageEvent::Error { message } => {
                eprintln!("[error] {message}");
            }
        }
    }
    Ok(())
}
