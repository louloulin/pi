//! Integration tests proving the coding agent executes *real* built-in tools.
//!
//! A scripted provider emits tool calls, [`default_executor`] runs them, and
//! the assertions check the actual filesystem / shell output landed in the
//! tool results. Everything is isolated in `tempfile` directories — no
//! network, no API keys.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::stream;
use pi_agent_core::{Agent, AgentOptions};
use pi_ai::stream::{AssistantMessageEventStream, StreamFn};
use pi_ai::types::SimpleStreamOptions;
use pi_ai::StreamError;
use pi_coding_agent::tool_executor::default_executor;
use pi_protocol::{
    Api, AssistantMessage, AssistantMessageEvent, Content, Context as AgentContext, Message, Model,
    ProviderId, Role, StopReason, TextContent, ToolCall, ToolResult, Usage,
};
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn faux_model() -> Model {
    Model {
        provider: ProviderId::new("faux"),
        id: "faux-model".into(),
        api: Api::Faux,
        label: None,
        context_window: 8192,
        max_output_tokens: 1024,
    }
}

fn user_message(text: &str) -> Message {
    Message {
        role: Role::User,
        content: vec![Content::Text(TextContent { text: text.into() })],
        model: None,
    }
}

fn tool_call_message(name: &str, id: &str, arguments: serde_json::Value) -> AssistantMessage {
    AssistantMessage {
        model: "faux-model".into(),
        content: vec![Content::ToolCall(ToolCall {
            id: id.into(),
            name: name.into(),
            arguments,
        })],
        stop_reason: StopReason::ToolUse,
        usage: Usage::default(),
        error_message: None,
    }
}

fn text_reply(text: &str) -> AssistantMessage {
    AssistantMessage {
        model: "faux-model".into(),
        content: vec![Content::text(text)],
        stop_reason: StopReason::Stop,
        usage: Usage::default(),
        error_message: None,
    }
}

/// Scripted stream that also records the tool names advertised to it.
struct ScriptedStream {
    responses: Mutex<Vec<AssistantMessage>>,
    calls: Mutex<usize>,
    advertised_tools: Mutex<Vec<String>>,
}

impl ScriptedStream {
    fn new(responses: Vec<AssistantMessage>) -> Self {
        Self {
            responses: Mutex::new(responses),
            calls: Mutex::new(0),
            advertised_tools: Mutex::new(Vec::new()),
        }
    }

    fn call_count(&self) -> usize {
        *self.calls.lock().expect("stream calls")
    }

    fn advertised_tools(&self) -> Vec<String> {
        self.advertised_tools
            .lock()
            .expect("advertised tools")
            .clone()
    }
}

#[async_trait]
impl StreamFn for ScriptedStream {
    async fn stream_simple(
        &self,
        _model: &Model,
        ctx: &AgentContext,
        _options: &SimpleStreamOptions,
    ) -> Result<AssistantMessageEventStream, StreamError> {
        self.advertised_tools
            .lock()
            .expect("advertised tools")
            .extend(ctx.tools.iter().map(|tool| tool.name.clone()));

        let mut responses = self.responses.lock().expect("scripted responses");
        if responses.is_empty() {
            return Err(StreamError::Malformed("scripted stream exhausted".into()));
        }
        let message = responses.remove(0);
        *self.calls.lock().expect("stream calls") += 1;
        let events = vec![
            Ok(AssistantMessageEvent::Start {
                model: message.model.clone(),
            }),
            Ok(AssistantMessageEvent::Done {
                content: message.content,
                stop_reason: message.stop_reason,
                usage: message.usage,
            }),
        ];
        Ok(Box::pin(stream::iter(events)))
    }
}

/// First tool result text in the agent's message log.
fn first_tool_text(agent: &Agent) -> String {
    agent
        .loop_ref()
        .state()
        .messages
        .iter()
        .flat_map(|message| message.content.iter())
        .find_map(|content| match content {
            Content::ToolResult(result) => match result.content.as_ref() {
                Content::Text(text) => Some(text.text.clone()),
                _ => None,
            },
            _ => None,
        })
        .expect("agent produced a tool result")
}

/// Every tool result in the agent's message log, in order.
fn tool_results(agent: &Agent) -> Vec<ToolResult> {
    agent
        .loop_ref()
        .state()
        .messages
        .iter()
        .flat_map(|message| message.content.iter())
        .filter_map(|content| match content {
            Content::ToolResult(result) => Some(result.clone()),
            _ => None,
        })
        .collect()
}

fn tool_result_text(result: &ToolResult) -> String {
    match result.content.as_ref() {
        Content::Text(text) => text.text.clone(),
        other => format!("{other:?}"),
    }
}

fn build_agent(stream: Arc<ScriptedStream>) -> Agent {
    Agent::new(
        AgentOptions::new(faux_model(), stream, "you are pi")
            .with_tool_executor(default_executor()),
    )
}

fn temp_dir(label: &str) -> TempDir {
    TempDir::with_prefix(format!("pi-agent-tools-{label}-{}-", std::process::id()))
        .expect("tempdir")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn read_tool_returns_real_file_contents() {
    let dir = temp_dir("read");
    let file = dir.path().join("note.txt");
    std::fs::write(&file, "hello from disk\nsecond line\n").expect("write fixture");

    let stream = Arc::new(ScriptedStream::new(vec![
        tool_call_message("read", "call-read", serde_json::json!({"path": file})),
        text_reply("done"),
    ]));
    let mut agent = build_agent(stream.clone());

    agent
        .loop_mut()
        .run(vec![user_message("read the note")], |_| {})
        .await
        .expect("loop runs");

    let text = first_tool_text(&agent);
    assert!(
        text.contains("hello from disk") && text.contains("second line"),
        "read tool must return the real file contents, got: {text:?}"
    );
    assert!(
        !text.contains("(stub) executed"),
        "the executor must not fall back to the stub"
    );
    assert_eq!(stream.call_count(), 2, "loop streams the follow-up turn");
}

#[tokio::test]
async fn definitions_are_advertised_to_the_provider() {
    let dir = temp_dir("defs");
    let file = dir.path().join("note.txt");
    std::fs::write(&file, "content\n").expect("write fixture");

    let stream = Arc::new(ScriptedStream::new(vec![
        tool_call_message("read", "call-read", serde_json::json!({"path": file})),
        text_reply("done"),
    ]));
    let mut agent = build_agent(stream.clone());

    agent
        .loop_mut()
        .run(vec![user_message("go")], |_| {})
        .await
        .expect("loop runs");

    let advertised = stream.advertised_tools();
    for expected in ["read", "write", "edit", "bash", "find", "grep", "ls"] {
        assert!(
            advertised.iter().any(|name| name == expected),
            "tool '{expected}' must be advertised to the model"
        );
    }
}

#[tokio::test]
async fn grep_tool_finds_matching_line_in_temp_dir() {
    // `grep` resolves `path` relative to the process cwd and rejects
    // absolute paths, so the fixture lives in a temp dir *inside* the cwd.
    let cwd = std::env::current_dir().expect("cwd");
    let dir = tempfile::Builder::new()
        .prefix("pi-tools-grep-")
        .tempdir_in(&cwd)
        .expect("tempdir in cwd");
    std::fs::write(dir.path().join("data.txt"), "irrelevant\nneedle here\n")
        .expect("write fixture");
    let relative = dir
        .path()
        .strip_prefix(&cwd)
        .expect("temp dir is inside cwd")
        .to_string_lossy()
        .into_owned();

    let stream = Arc::new(ScriptedStream::new(vec![
        tool_call_message(
            "grep",
            "call-grep",
            serde_json::json!({"pattern": "needle", "path": relative}),
        ),
        text_reply("done"),
    ]));
    let mut agent = build_agent(stream);

    agent
        .loop_mut()
        .run(vec![user_message("search")], |_| {})
        .await
        .expect("loop runs");

    let text = first_tool_text(&agent);
    assert!(
        text.contains("needle here"),
        "grep must report the matching line, got: {text:?}"
    );
    assert!(
        text.contains("data.txt"),
        "grep output should name the file"
    );
}

#[tokio::test]
async fn bash_tool_runs_a_real_command() {
    let stream = Arc::new(ScriptedStream::new(vec![
        tool_call_message(
            "bash",
            "call-bash",
            serde_json::json!({"command": "printf 'bash-marker'"}),
        ),
        text_reply("done"),
    ]));
    let mut agent = build_agent(stream);

    agent
        .loop_mut()
        .run(vec![user_message("run it")], |_| {})
        .await
        .expect("loop runs");

    let text = first_tool_text(&agent);
    assert!(
        text.contains("bash-marker"),
        "bash must return the command's stdout, got: {text:?}"
    );
}

#[tokio::test]
async fn unknown_tool_surfaces_as_error_without_stopping_the_loop() {
    let stream = Arc::new(ScriptedStream::new(vec![
        tool_call_message("not_a_tool", "call-unknown", serde_json::json!({})),
        text_reply("recovered"),
    ]));
    let mut agent = build_agent(stream.clone());

    agent
        .loop_mut()
        .run(vec![user_message("go")], |_| {})
        .await
        .expect("loop runs");

    let text = first_tool_text(&agent);
    assert!(
        text.contains("unknown tool"),
        "an unregistered tool name must report 'unknown tool', got: {text:?}"
    );
    assert_eq!(
        stream.call_count(),
        2,
        "the loop continues after the unknown-tool error"
    );
}

#[tokio::test]
async fn tool_errors_are_marked_is_error_in_the_message_log() {
    let stream = Arc::new(ScriptedStream::new(vec![
        tool_call_message(
            "read",
            "call-missing",
            serde_json::json!({"path": "/definitely/not/a/real/file-42.txt"}),
        ),
        text_reply("done"),
    ]));
    let mut agent = build_agent(stream);

    agent
        .loop_mut()
        .run(vec![user_message("read a missing file")], |_| {})
        .await
        .expect("loop runs");

    let is_error = agent
        .loop_ref()
        .state()
        .messages
        .iter()
        .flat_map(|message| message.content.iter())
        .find_map(|content| match content {
            Content::ToolResult(result) => Some(result.is_error),
            _ => None,
        })
        .expect("tool result present");
    assert!(is_error, "a failed read must be flagged as an error result");
}

#[tokio::test]
async fn parallel_read_batch_lands_in_source_order() {
    // `read` declares no execution-mode override, so a batch of reads takes
    // the concurrent path in `pi-agent-core`; their results must still be
    // appended in the order the model emitted them, each carrying the real
    // file contents.
    let dir = temp_dir("parallel-read");
    let first = dir.path().join("first.txt");
    let second = dir.path().join("second.txt");
    std::fs::write(&first, "alpha contents\n").expect("write fixture");
    std::fs::write(&second, "beta contents\n").expect("write fixture");

    let batch = AssistantMessage {
        model: "faux-model".into(),
        content: vec![
            Content::ToolCall(ToolCall {
                id: "call-first".into(),
                name: "read".into(),
                arguments: serde_json::json!({"path": first}),
            }),
            Content::ToolCall(ToolCall {
                id: "call-second".into(),
                name: "read".into(),
                arguments: serde_json::json!({"path": second}),
            }),
        ],
        stop_reason: StopReason::ToolUse,
        usage: Usage::default(),
        error_message: None,
    };

    let stream = Arc::new(ScriptedStream::new(vec![batch, text_reply("done")]));
    let mut agent = build_agent(stream.clone());

    agent
        .loop_mut()
        .run(vec![user_message("read both files")], |_| {})
        .await
        .expect("loop runs");

    let results = tool_results(&agent);
    assert_eq!(
        results
            .iter()
            .map(|result| result.tool_call_id.as_str())
            .collect::<Vec<_>>(),
        vec!["call-first", "call-second"],
        "parallel results are appended in source order"
    );
    assert!(tool_result_text(&results[0]).contains("alpha contents"));
    assert!(tool_result_text(&results[1]).contains("beta contents"));
    assert!(results.iter().all(|result| !result.is_error));
    assert_eq!(stream.call_count(), 2);
}
