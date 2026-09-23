//! Integration tests for `pi --rpc` (Stage 12).
//!
//! These tests spawn the real binary and drive it over stdio — the same
//! way an editor / host process would — through the shared
//! [`pi_coding_agent::rpc::RpcClient`] (the Rust counterpart of the
//! upstream `rpc-client.ts`). The client owns the NDJSON framing, the
//! `id` ↔ response pairing, the per-read timeouts and the stderr capture;
//! the tests only assert wire behaviour.
//!
//! The binary always drives the faux provider (no API key needed), so a
//! `prompt` deterministically produces `"(faux) hello"`.

use std::time::Duration;

use pi_coding_agent::rpc::client::{
    ClientMessage, RpcClient, RpcClientError, RpcClientOptions, StdinMode,
};
use serde_json::{json, Value};

/// Per-read / per-wait timeout. Generous enough for a cold debug build's
/// first turn, small enough to fail fast on a real hang.
const TIMEOUT: Duration = Duration::from_secs(20);

/// Spawn `pi --rpc` (plus `extra_args`) through the shared client.
fn spawn_client(extra_args: &[&str]) -> RpcClient {
    let options = RpcClientOptions::new(env!("CARGO_BIN_EXE_pi"))
        .args(extra_args.iter().copied())
        .timeout(TIMEOUT);
    RpcClient::spawn(&options).expect("failed to spawn pi --rpc")
}

/// Close stdin (EOF) and assert the server drained and exited 0.
fn expect_clean_exit(client: &mut RpcClient) {
    client.close_stdin();
    let status = client
        .wait_for_exit(TIMEOUT)
        .expect("pi --rpc did not exit after stdin EOF");
    let stderr = client.stderr().join("\n");
    assert_eq!(
        status.code(),
        Some(0),
        "stdin EOF must exit 0; stderr: {stderr}"
    );
}

/// Position of the first event of `event_type` in the retained log.
fn event_position(events: &[Value], event_type: &str) -> Option<usize> {
    events.iter().position(|v| v["type"] == json!(event_type))
}

/// Position of the first `message_update` carrying a `text_delta`.
fn text_delta_position(events: &[Value]) -> Option<usize> {
    events.iter().position(|v| {
        v["type"] == json!("message_update")
            && v["assistantMessageEvent"]["type"] == json!("text_delta")
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn prompt_returns_response_and_streams_events() {
    let mut client = spawn_client(&[]);
    let response = client
        .call("prompt", Some(json!({"text": "hello"})))
        .expect("prompt response");

    assert_eq!(response.jsonrpc, "2.0");
    assert_eq!(response.id, Some(json!(1)));
    assert!(response.error.is_none(), "{:?}", response.error);
    let result = response.result.as_ref().expect("prompt result");
    assert_eq!(result["stopReason"], "stop");
    assert!(result["turn"].is_number(), "{result}");

    // The server emits the turn's events before it answers the request, so
    // every one of them must already be in the client's log the moment the
    // response arrives — that is the streaming guarantee.
    let events = client.events();
    let text_delta = text_delta_position(events);
    let message_end = event_position(events, "message_end");
    let turn_end = event_position(events, "turn_end");

    let text_delta = text_delta.expect("no text_delta event notification arrived");
    let message_end = message_end.expect("no message_end event notification arrived");
    let turn_end = turn_end.expect("no turn_end event notification arrived");
    assert!(
        text_delta < message_end,
        "text_delta must precede message_end: {events:?}"
    );
    assert!(
        message_end < turn_end,
        "message_end must precede turn_end: {events:?}"
    );

    // getState reflects the turn that just ran.
    let state = client.get_state().expect("getState response");
    assert!(state["model"]["id"].is_string(), "{state}");
    assert!(
        state["sessionId"].as_str().is_some_and(|id| !id.is_empty()),
        "getState must report a session id: {state}"
    );
    let messages = state["messages"].as_array().expect("messages array");
    assert!(
        messages.len() >= 2,
        "expected user + assistant messages, got {messages:?}"
    );
    assert_eq!(messages[0]["role"], "user");
    assert_eq!(messages[1]["role"], "assistant");

    expect_clean_exit(&mut client);
}

#[test]
fn prompt_template_expands_slash_invocations() {
    // `--prompt-template` loads a markdown template; a `prompt` whose
    // text is `/greet <arg>` must reach the agent as the expanded body,
    // proving RPC mode shares the prompt-template pipeline with the TUI.
    let dir = tempfile::TempDir::with_prefix("pi-rpc-prompts-").expect("tempdir");
    let template = dir.path().join("greet.md");
    std::fs::write(
        &template,
        "---\ndescription: Greet someone.\n---\nhello-template:$1",
    )
    .expect("write template");

    let mut client = spawn_client(&["--prompt-template", template.to_str().expect("utf-8 path")]);
    client
        .call("prompt", Some(json!({"text": "/greet world"})))
        .expect("prompt response");

    let state = client.get_state().expect("getState response");
    let serialized = state.to_string();
    assert!(
        serialized.contains("hello-template:world"),
        "template body was not expanded into the prompt: {serialized}"
    );
    assert!(
        !serialized.contains("/greet world"),
        "the raw slash invocation leaked into the transcript: {serialized}"
    );

    expect_clean_exit(&mut client);
}

#[test]
fn get_state_without_prompt_returns_empty_state() {
    let mut client = spawn_client(&[]);
    let response = client.call("getState", None).expect("getState response");
    assert_eq!(response.id, Some(json!(1)));
    let result = response.result.as_ref().expect("getState result");
    assert_eq!(result["messages"], json!([]));
    assert_eq!(result["model"]["id"], "faux-model");
    assert!(
        result["sessionId"]
            .as_str()
            .is_some_and(|id| !id.is_empty()),
        "getState must report a session id: {result}"
    );

    expect_clean_exit(&mut client);
}

#[test]
fn unknown_method_returns_method_not_found() {
    let mut client = spawn_client(&[]);
    let error = client
        .call_ok("definitely-not-a-method", None)
        .expect_err("unknown method must fail");
    assert_eq!(error.json_rpc_code(), Some(-32601), "{error}");

    expect_clean_exit(&mut client);
}

#[test]
fn invalid_json_line_returns_parse_error_and_keeps_running() {
    let mut client = spawn_client(&[]);
    // The low-level entry point exists for exactly this: bytes the typed
    // API cannot express.
    client
        .send_raw_line("{ this is not json")
        .expect("write raw line");

    let message = client.recv_message(TIMEOUT).expect("parse-error response");
    let ClientMessage::Response(error) = message else {
        panic!("expected a response frame, got {message:?}");
    };
    assert_eq!(error.id, None);
    assert_eq!(error.error.as_ref().expect("error object").code, -32700);

    // The server must still answer a well-formed request afterwards.
    let response = client
        .call("getState", None)
        .expect("getState after parse error");
    let result = response.result.expect("getState result");
    assert!(result["messages"].is_array(), "{result}");

    expect_clean_exit(&mut client);
}

#[test]
fn abort_without_in_flight_turn_is_idempotent() {
    let mut client = spawn_client(&[]);
    assert!(!client.abort().expect("abort response"));

    expect_clean_exit(&mut client);
}

#[test]
fn set_model_rejects_unknown_model() {
    let mut client = spawn_client(&[]);
    let error = client
        .call_ok("setModel", Some(json!({"model": "nope/nope-model"})))
        .expect_err("unknown model must fail");
    assert_eq!(error.json_rpc_code(), Some(-32602), "{error}");

    // A known model still switches.
    let response = client
        .call(
            "setModel",
            Some(json!({"model": "anthropic/claude-sonnet-4-5"})),
        )
        .expect("setModel response");
    assert_eq!(
        response.result.expect("setModel result")["model"]["id"],
        "claude-sonnet-4-5"
    );

    expect_clean_exit(&mut client);
}

#[test]
fn rpc_flag_no_longer_prints_the_stage5_stub() {
    let mut client = spawn_client(&[]);
    // stdin is already at EOF; the server should exit 0 without printing
    // anything but JSON (nothing, in this case).
    client.close_stdin();
    let status = client
        .wait_for_exit(TIMEOUT)
        .expect("pi --rpc did not exit after stdin EOF");
    let stderr = client.stderr().join("\n");

    assert_eq!(status.code(), Some(0), "stderr: {stderr}");
    assert!(
        !stderr.contains("Stage 5 deliverable"),
        "rpc mode still prints the stub banner: {stderr}"
    );
    assert!(
        !stderr.contains("rpc mode is a"),
        "rpc mode still prints the stub banner: {stderr}"
    );
}

#[test]
fn rpc_flag_without_stdin_exits_zero() {
    // `--rpc` must never fall back to an interactive TUI (which would
    // block forever without a tty). `/dev/null` gives immediate EOF.
    let options = RpcClientOptions::new(env!("CARGO_BIN_EXE_pi"))
        .stdin(StdinMode::Null)
        .timeout(TIMEOUT);
    let mut client = RpcClient::spawn(&options).expect("spawn pi --rpc");

    // stdin was never piped, so writing to it fails loudly instead of hanging.
    assert!(matches!(
        client.send_raw_line("{}"),
        Err(RpcClientError::StdinClosed)
    ));

    let status = client
        .wait_for_exit(TIMEOUT)
        .expect("pi --rpc with /dev/null stdin did not exit");
    assert_eq!(status.code(), Some(0));
}
