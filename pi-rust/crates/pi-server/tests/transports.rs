//! Transport-level tests: the same protocol flow over real loopback sockets.
//!
//! These use `127.0.0.1:0` and a temporary Unix socket under
//! `CARGO_TARGET_TMPDIR` — no external processes and no off-host traffic.

use std::sync::Arc;

use pi_chord::services::ServiceCall;
use pi_protocol::rpc::{
    encode_client_message, ClientMessage, RpcTarget, ServerMessage, ServerMessageDecoder,
    ServerTarget,
};
use pi_server::testing::TestServerHost;
use pi_server::transports::TcpServerListener;
use pi_server::{Server, ServerHost, ServerListener, ServerOptions, SessionId};
use serde_json::json;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

const SERVER_ID: &str = "3f1d2c9a-7b4e-4a1f-8c2d-9e5b6a7c8d90";

struct WireClient<S> {
    stream: S,
    decoder: ServerMessageDecoder,
    pending: Vec<ServerMessage>,
}

impl<S> WireClient<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    fn new(stream: S) -> Self {
        Self {
            stream,
            decoder: ServerMessageDecoder::new(None),
            pending: Vec::new(),
        }
    }

    async fn send(&mut self, message: &ClientMessage) {
        let frame = encode_client_message(message, None).expect("encode");
        self.stream.write_all(&frame).await.expect("write");
    }

    async fn read_more(&mut self) -> bool {
        let mut buffer = [0u8; 4096];
        match self.stream.read(&mut buffer).await {
            Ok(0) => false,
            Ok(read) => {
                let messages = self.decoder.push(&buffer[..read]).expect("decode");
                self.pending.extend(messages);
                true
            }
            Err(_) => false,
        }
    }

    async fn next_matching(
        &mut self,
        predicate: impl Fn(&ServerMessage) -> bool,
    ) -> ServerMessage {
        loop {
            if let Some(index) = self.pending.iter().position(&predicate) {
                return self.pending.remove(index);
            }
            assert!(
                self.read_more().await,
                "the stream closed before a matching message arrived"
            );
        }
    }

    async fn hello(&mut self) -> ServerMessage {
        self.send(&ClientMessage::hello(8)).await;
        self.next_matching(|message| {
            matches!(
                message,
                ServerMessage::Hello { .. } | ServerMessage::HelloError { .. }
            )
        })
        .await
    }

    async fn request(
        &mut self,
        id: &str,
        target: RpcTarget,
        call: ServiceCall,
    ) -> ServerMessage {
        self.send(&ClientMessage::request(id, target, call.to_json()))
            .await;
        self.next_matching(|message| {
            matches!(message, ServerMessage::Response { id: response_id, .. } if response_id == id)
        })
        .await
    }
}

async fn start_server(listener: Arc<dyn ServerListener>) -> (Arc<TestServerHost>, Arc<Server<SessionId>>) {
    let host = Arc::new(TestServerHost::new());
    host.seed("session-1");
    let options = ServerOptions::new(SERVER_ID, vec![listener]);
    let server = Server::new(Arc::clone(&host) as Arc<dyn ServerHost<SessionId>>, options)
        .expect("server");
    server.start().await.expect("start");
    (host, server)
}

fn attach_call() -> ServiceCall {
    ServiceCall::new(
        "pi.session-management",
        "attach",
        vec![json!("session-1")],
    )
}

fn server_target() -> RpcTarget {
    RpcTarget::Server(ServerTarget {
        server_id: SERVER_ID.to_owned(),
    })
}

#[tokio::test]
async fn tcp_transport_handshakes_and_routes() {
    let listener = TcpServerListener::new("127.0.0.1:0".parse().expect("addr"));
    let (host, server) = start_server(Arc::clone(&listener) as Arc<dyn ServerListener>).await;
    let address = listener.local_addr().expect("bound address");

    let stream = tokio::net::TcpStream::connect(address)
        .await
        .expect("connect");
    let mut client = WireClient::new(stream);
    assert_eq!(client.hello().await, ServerMessage::hello(SERVER_ID));

    let response = client
        .request("tcp-1", server_target(), attach_call())
        .await;
    match response {
        ServerMessage::Response { ok, error, .. } => {
            assert!(ok, "attach failed: {error:?}");
        }
        other => panic!("unexpected message: {other:?}"),
    }

    let attachment = client
        .next_matching(|message| {
            matches!(message, ServerMessage::Attachment { attachment: Some(_) })
        })
        .await;
    let attachment = match attachment {
        ServerMessage::Attachment { attachment: Some(value) } => value,
        other => panic!("unexpected message: {other:?}"),
    };

    let response = client
        .request(
            "tcp-2",
            RpcTarget::Session(attachment),
            ServiceCall::new("pi.test", "echo", vec![json!({ "n": 1 })]),
        )
        .await;
    match response {
        ServerMessage::Response { ok, result, error, .. } => {
            assert!(ok, "session request failed: {error:?}");
            assert_eq!(result, Some(json!(true)));
        }
        other => panic!("unexpected message: {other:?}"),
    }

    let harness = host.latest_harness("session-1");
    assert_eq!(harness.service_calls().len(), 1);
    server.close().await.expect("close");
}

#[cfg(unix)]
#[tokio::test]
async fn unix_transport_handshakes_and_routes() {
    use pi_server::transports::UnixServerListener;

    let path = std::env::temp_dir().join(format!(
        "pi-server-transport-{}-{}.sock",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .subsec_nanos()
    ));
    let listener = UnixServerListener::new(path.clone());
    let (host, server) = start_server(Arc::clone(&listener) as Arc<dyn ServerListener>).await;

    let stream = tokio::net::UnixStream::connect(&path)
        .await
        .expect("connect");
    let mut client = WireClient::new(stream);
    assert_eq!(client.hello().await, ServerMessage::hello(SERVER_ID));

    let response = client
        .request("unix-1", server_target(), attach_call())
        .await;
    match response {
        ServerMessage::Response { ok, error, .. } => {
            assert!(ok, "attach failed: {error:?}");
        }
        other => panic!("unexpected message: {other:?}"),
    }

    let server_id = host.latest_harness("session-1").session_id().to_owned();
    assert_eq!(server_id, "session-1");

    server.close().await.expect("close");
    assert!(!path.exists(), "close removed the socket path");
}
