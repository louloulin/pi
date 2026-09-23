//! Offline conformance tests for the routed session server.
//!
//! Everything runs over the in-memory transport: no sockets, no subprocesses,
//! no timers beyond short handshake deadlines.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use pi_chord::services::ServiceCall;
use pi_protocol::rpc::{ClientMessage, RpcTarget, ServerMessage, ServerTarget};
use pi_server::testing::{ProtocolTestClient, TestServerHost};
use pi_server::transports::MemoryListener;
use pi_server::{
    ConnectionCountObserver, Server, ServerError, ServerHost, ServerListener, ServerOptions,
    SessionId,
};
use serde_json::json;
use serde_json::Value;

const SERVER_ID: &str = "3f1d2c9a-7b4e-4a1f-8c2d-9e5b6a7c8d90";
const OTHER_SERVER_ID: &str = "11111111-1111-4111-8111-111111111111";

struct Fixture {
    host: Arc<TestServerHost>,
    listener: Arc<MemoryListener>,
    server: Arc<Server<SessionId>>,
    connection_counts: Arc<Vec<AtomicUsize>>,
}

impl Fixture {
    async fn start(handshake_timeout_ms: Option<u64>) -> Self {
        let listener = MemoryListener::new();
        let host = Arc::new(TestServerHost::new());
        let connection_counts: Arc<Vec<AtomicUsize>> =
            Arc::new((0..8).map(|_| AtomicUsize::new(0)).collect());

        let observer: ConnectionCountObserver = {
            let connection_counts = Arc::clone(&connection_counts);
            Arc::new(move |count: usize| {
                let index = count.min(connection_counts.len() - 1);
                connection_counts[index].fetch_add(1, Ordering::SeqCst);
            })
        };
        let error_observer: pi_server::ErrorObserver = Arc::new({
            let errors = Arc::new(Mutex::new(Vec::<String>::new()));
            move |error: ServerError| errors.lock().push(error.to_string())
        });

        let mut options = ServerOptions::new(
            SERVER_ID,
            vec![Arc::clone(&listener) as Arc<dyn ServerListener>],
        )
        .with_connection_count_observer(observer)
        .with_error_observer(error_observer);
        if let Some(timeout) = handshake_timeout_ms {
            options = options.with_handshake_timeout_ms(timeout);
        }
        let server = Server::new(Arc::clone(&host) as Arc<dyn ServerHost<SessionId>>, options)
            .expect("server");
        server.start().await.expect("start");
        Self {
            host,
            listener,
            server,
            connection_counts,
        }
    }

    fn connect(&self) -> Arc<ProtocolTestClient> {
        let channel = self.listener.connect().expect("authorized connection");
        Arc::new(ProtocolTestClient::new(channel))
    }

    async fn close(&self) {
        self.server.close().await.expect("close");
    }
}

async fn wait_until(mut condition: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !condition() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "condition was not met before the deadline"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

fn echo_call() -> ServiceCall {
    ServiceCall::new("pi.test", "echo", vec![json!({ "n": 1 })])
}

#[tokio::test]
async fn handshake_accepts_the_supported_version() {
    let fixture = Fixture::start(None).await;
    let client = fixture.connect();
    let hello = client.hello(8).await.expect("hello");
    assert_eq!(hello, ServerMessage::hello(SERVER_ID));
    assert!(!client.closed());
    fixture.close().await;
}

#[tokio::test]
async fn handshake_rejects_a_version_mismatch() {
    let fixture = Fixture::start(None).await;
    let client = fixture.connect();
    let answer = client.hello(7).await.expect("answer");
    match answer {
        ServerMessage::HelloError { error } => {
            assert_eq!(error.code, "version");
            assert_eq!(error.message, "Unsupported protocol version 7; expected 8");
        }
        other => panic!("unexpected answer: {other:?}"),
    }
    client.wait_for_close().await;
    fixture.close().await;
}

#[tokio::test]
async fn first_message_must_be_hello() {
    let fixture = Fixture::start(None).await;
    let client = fixture.connect();
    client
        .send_message(&ClientMessage::request(
            "req-1",
            RpcTarget::Server(ServerTarget {
                server_id: SERVER_ID.to_owned(),
            }),
            echo_call().to_json(),
        ))
        .expect("send");
    let answer = client
        .next_from(0, |message| {
            matches!(message, ServerMessage::HelloError { .. })
        })
        .await
        .expect("answer");
    match answer {
        ServerMessage::HelloError { error } => {
            assert_eq!(error.code, "invalid_request");
            assert_eq!(error.message, "The first client message must be hello");
        }
        other => panic!("unexpected answer: {other:?}"),
    }
    fixture.close().await;
}

#[tokio::test]
async fn hello_may_only_be_sent_once() {
    let fixture = Fixture::start(None).await;
    let client = fixture.connect();
    client.hello(8).await.expect("hello");
    let index = client.messages().len();
    client.send_message(&ClientMessage::hello(8)).expect("send");
    let answer = client
        .next_from(index, |message| {
            matches!(message, ServerMessage::HelloError { .. })
        })
        .await
        .expect("answer");
    match answer {
        ServerMessage::HelloError { error } => {
            assert_eq!(error.code, "invalid_request");
            assert_eq!(error.message, "hello may only be sent as the first message");
        }
        other => panic!("unexpected answer: {other:?}"),
    }
    fixture.close().await;
}

#[tokio::test]
async fn handshake_times_out() {
    let fixture = Fixture::start(Some(30)).await;
    let client = fixture.connect();
    let answer = client
        .next_from(0, |message| {
            matches!(message, ServerMessage::HelloError { .. })
        })
        .await
        .expect("answer");
    match answer {
        ServerMessage::HelloError { error } => {
            assert_eq!(error.code, "invalid_request");
            assert_eq!(error.message, "Handshake timeout");
        }
        other => panic!("unexpected answer: {other:?}"),
    }
    client.wait_for_close().await;
    fixture.close().await;
}

#[tokio::test]
async fn session_request_round_trip() {
    let fixture = Fixture::start(None).await;
    fixture.host.seed("session-1");
    let client = fixture.connect();
    client.hello(8).await.expect("hello");

    let attach = client.attach(SERVER_ID, "session-1").await.expect("attach");
    assert!(attach.ok, "attach failed: {:?}", attach.error);
    client
        .next_from(0, |message| {
            matches!(
                message,
                ServerMessage::Attachment {
                    attachment: Some(_)
                }
            )
        })
        .await
        .expect("attachment");
    let attachment = client.attachment().expect("attachment");
    assert_eq!(attachment.session_id, "session-1");
    assert_eq!(attachment.server_id, SERVER_ID);

    let call = echo_call();
    let response = client
        .request_service(RpcTarget::Session(attachment), call.clone(), None)
        .await
        .expect("response");
    assert!(response.ok, "response failed: {:?}", response.error);
    assert_eq!(response.result, Some(Value::Bool(true)));

    let harness = fixture.host.latest_harness("session-1");
    assert_eq!(harness.attached_clients(), 1);
    assert_eq!(harness.service_calls(), vec![call]);
    fixture.close().await;
}

#[tokio::test]
async fn missing_session_maps_to_session_not_found() {
    let fixture = Fixture::start(None).await;
    let client = fixture.connect();
    client.hello(8).await.expect("hello");
    let response = client.attach(SERVER_ID, "missing").await.expect("response");
    assert!(!response.ok);
    let error = response.error.expect("error");
    assert_eq!(error.code, "session_not_found");
    assert!(error.message.contains("Unknown session: missing"));
    fixture.close().await;
}

#[tokio::test]
async fn wrong_server_maps_to_wrong_server() {
    let fixture = Fixture::start(None).await;
    let client = fixture.connect();
    client.hello(8).await.expect("hello");
    let response = client
        .request_service(
            RpcTarget::Server(ServerTarget {
                server_id: OTHER_SERVER_ID.to_owned(),
            }),
            echo_call(),
            None,
        )
        .await
        .expect("response");
    assert!(!response.ok);
    assert_eq!(response.error.expect("error").code, "wrong_server");
    fixture.close().await;
}

#[tokio::test]
async fn invalid_service_call_maps_to_invalid_request() {
    let fixture = Fixture::start(None).await;
    let client = fixture.connect();
    client.hello(8).await.expect("hello");
    client
        .send_message(&ClientMessage::request(
            "req-1",
            RpcTarget::Server(ServerTarget {
                server_id: SERVER_ID.to_owned(),
            }),
            json!({ "not": "a service call" }),
        ))
        .expect("send");
    let response = client
        .next_from(
            0,
            |message| matches!(message, ServerMessage::Response { id, .. } if id == "req-1"),
        )
        .await
        .expect("response");
    match response {
        ServerMessage::Response { ok, error, .. } => {
            assert!(!ok);
            assert_eq!(error.expect("error").code, "invalid_request");
        }
        other => panic!("unexpected message: {other:?}"),
    }
    fixture.close().await;
}

#[tokio::test]
async fn unknown_server_service_maps_to_internal_error() {
    let fixture = Fixture::start(None).await;
    let client = fixture.connect();
    client.hello(8).await.expect("hello");
    let response = client
        .request_service(
            RpcTarget::Server(ServerTarget {
                server_id: SERVER_ID.to_owned(),
            }),
            ServiceCall::new("pi.unknown", "explode", Vec::new()),
            None,
        )
        .await
        .expect("response");
    assert!(!response.ok);
    assert_eq!(response.error.expect("error").code, "internal_error");
    fixture.close().await;
}

#[tokio::test]
async fn duplicate_request_id_is_rejected() {
    let fixture = Fixture::start(None).await;
    fixture.host.seed("session-1");
    let client = fixture.connect();
    client.hello(8).await.expect("hello");
    client.attach(SERVER_ID, "session-1").await.expect("attach");
    let attachment = client.attachment().expect("attachment");
    let harness = fixture.host.latest_harness("session-1");
    let mut gate = harness.gate_next_service_call();

    let requester = Arc::clone(&client);
    let target = RpcTarget::Session(attachment);
    requester
        .send_message(&ClientMessage::request(
            "req-dup",
            target,
            echo_call().to_json(),
        ))
        .expect("send");
    gate.wait_entered().await;

    client
        .send_message(&ClientMessage::request(
            "req-dup",
            RpcTarget::Server(ServerTarget {
                server_id: SERVER_ID.to_owned(),
            }),
            echo_call().to_json(),
        ))
        .expect("send");
    let rejection = client
        .next_from(0, |message| {
            matches!(message, ServerMessage::Response { id, ok, .. } if id == "req-dup" && !*ok)
        })
        .await
        .expect("duplicate response");
    match rejection {
        ServerMessage::Response { error, .. } => {
            let error = error.expect("error");
            assert_eq!(error.code, "invalid_request");
            assert_eq!(error.message, "Request ID is already active");
        }
        other => panic!("unexpected message: {other:?}"),
    }

    gate.release();
    let accepted = client
        .next_from(0, |message| {
            matches!(message, ServerMessage::Response { id, ok, .. } if id == "req-dup" && *ok)
        })
        .await
        .expect("first response");
    assert!(matches!(accepted, ServerMessage::Response { ok: true, .. }));
    fixture.close().await;
}

#[tokio::test]
async fn cancel_answers_with_cancelled() {
    let fixture = Fixture::start(None).await;
    fixture.host.seed("session-1");
    let client = fixture.connect();
    client.hello(8).await.expect("hello");
    client.attach(SERVER_ID, "session-1").await.expect("attach");
    let attachment = client.attachment().expect("attachment");
    let harness = fixture.host.latest_harness("session-1");
    let mut gate = harness.gate_next_service_call();

    let requester = Arc::clone(&client);
    let target = RpcTarget::Session(attachment.clone());
    let request = tokio::spawn(async move {
        requester
            .request_service(target, echo_call(), Some("req-cancel".to_owned()))
            .await
    });
    gate.wait_entered().await;
    client
        .send_message(&ClientMessage::cancel(
            "req-cancel",
            RpcTarget::Session(attachment),
        ))
        .expect("cancel");
    gate.release();

    let response = request.await.expect("join").expect("response");
    assert!(!response.ok);
    assert_eq!(response.error.expect("error").code, "cancelled");
    fixture.close().await;
}

#[tokio::test]
async fn detaching_only_affects_the_detaching_client() {
    let fixture = Fixture::start(None).await;
    fixture.host.seed("session-1");
    let first = fixture.connect();
    let second = fixture.connect();
    first.hello(8).await.expect("hello");
    second.hello(8).await.expect("hello");
    first.attach(SERVER_ID, "session-1").await.expect("attach");
    second.attach(SERVER_ID, "session-1").await.expect("attach");

    assert_eq!(fixture.host.open_session_count(), 1);
    let harness = fixture.host.latest_harness("session-1");
    wait_until(|| harness.attached_clients() == 2).await;

    let detach = first.detach(SERVER_ID).await.expect("detach");
    assert!(detach.ok, "detach failed: {:?}", detach.error);
    wait_until(|| harness.attached_clients() == 1).await;

    let detached = first
        .next_from(0, |message| {
            matches!(message, ServerMessage::Attachment { attachment: None })
        })
        .await
        .expect("detach envelope");
    assert_eq!(detached, ServerMessage::attachment(None));
    assert!(second.attachment().is_some());
    fixture.close().await;
}

#[tokio::test]
async fn disconnecting_releases_the_attachment() {
    let fixture = Fixture::start(None).await;
    fixture.host.seed("session-1");
    let client = fixture.connect();
    client.hello(8).await.expect("hello");
    client.attach(SERVER_ID, "session-1").await.expect("attach");
    let harness = fixture.host.latest_harness("session-1");
    wait_until(|| harness.attached_clients() == 1).await;

    client.close();
    wait_until(|| harness.attachment_release_count() == 1).await;
    assert_eq!(harness.attached_clients(), 0);
    fixture.close().await;
}

#[tokio::test]
async fn session_termination_invalidates_and_publishes_detach() {
    let fixture = Fixture::start(None).await;
    fixture.host.seed("session-1");
    let client = fixture.connect();
    client.hello(8).await.expect("hello");
    client.attach(SERVER_ID, "session-1").await.expect("attach");
    let harness = fixture.host.latest_harness("session-1");
    wait_until(|| harness.attached_clients() == 1).await;

    harness.terminate(Some(ServerError::internal("session crashed")));

    let detached = client
        .next_from(0, |message| {
            matches!(message, ServerMessage::Attachment { attachment: None })
        })
        .await
        .expect("detach envelope");
    assert_eq!(detached, ServerMessage::attachment(None));
    assert!(client.attachment().is_none());
    fixture.close().await;
}

#[tokio::test]
async fn connection_count_observer_sees_connect_and_disconnect() {
    let fixture = Fixture::start(None).await;
    let client = fixture.connect();
    client.hello(8).await.expect("hello");
    wait_until(|| fixture.connection_counts[1].load(Ordering::SeqCst) > 0).await;

    client.close();
    wait_until(|| fixture.connection_counts[0].load(Ordering::SeqCst) > 0).await;
    assert_eq!(fixture.server.connection_count(), 0);
    fixture.close().await;
}

#[tokio::test]
async fn invalid_server_id_is_rejected() {
    let listener = MemoryListener::new();
    let host = Arc::new(TestServerHost::new());
    let options = ServerOptions::new(
        "not-a-uuid",
        vec![Arc::clone(&listener) as Arc<dyn ServerListener>],
    );
    match Server::new(Arc::clone(&host) as Arc<dyn ServerHost<SessionId>>, options) {
        Ok(_) => panic!("expected an invalid-server-id failure"),
        Err(error) => assert!(error.to_string().contains("canonical lowercase UUIDv4")),
    }
}

#[tokio::test]
async fn fragmented_frames_are_reassembled() {
    let fixture = Fixture::start(None).await;
    let client = fixture.connect();
    client.hello(8).await.expect("hello");
    let index = client.messages().len();
    client
        .send_fragmented_message(
            &ClientMessage::request(
                "req-frag",
                RpcTarget::Server(ServerTarget {
                    server_id: SERVER_ID.to_owned(),
                }),
                echo_call().to_json(),
            ),
            3,
        )
        .expect("send");
    let response = client
        .next_from(
            index,
            |message| matches!(message, ServerMessage::Response { id, .. } if id == "req-frag"),
        )
        .await
        .expect("response");
    match response {
        ServerMessage::Response { ok, error, .. } => {
            assert!(!ok);
            assert_eq!(error.expect("error").code, "internal_error");
        }
        other => panic!("unexpected message: {other:?}"),
    }
    fixture.close().await;
}
