//! End-to-end tests: a real `pi-server` on one end and this crate on the other.
//!
//! The in-memory listener keeps the fast path offline; the Unix test exercises
//! the real `AF_UNIX` transport on both sides.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use pi_chord::services::ServiceCall;
use pi_client::{
    ByteTransport, ByteTransportFactory, ByteTransportHandlers, Client, ClientError, ClientOptions,
    ConnectionState, RpcTarget, SessionTarget,
};
use pi_server::testing::TestServerHost;
use pi_server::transports::{MemoryClient, MemoryListener};
use pi_server::{Server, ServerHost, ServerListener, ServerOptions, SessionId};
use serde_json::json;
use tokio::sync::mpsc;

const SERVER_ID: &str = "3f1d2c9a-7b4e-4a1f-8c2d-9e5b6a7c8d90";

/// Adapts a `pi-server` in-memory client to this crate's byte-transport trait.
struct MemoryFactory {
    channel: Arc<MemoryClient>,
}

#[async_trait]
impl ByteTransportFactory for MemoryFactory {
    async fn connect(
        &self,
        handlers: Arc<dyn ByteTransportHandlers>,
    ) -> Result<Arc<dyn ByteTransport>, ClientError> {
        let channel = Arc::clone(&self.channel);
        tokio::spawn(async move {
            while let Some(chunk) = channel.recv().await {
                handlers.on_data(chunk).await;
            }
            handlers.on_close().await;
        });
        Ok(Arc::new(MemoryTransport {
            channel: Arc::clone(&self.channel),
        }))
    }
}

struct MemoryTransport {
    channel: Arc<MemoryClient>,
}

#[async_trait]
impl ByteTransport for MemoryTransport {
    async fn send(&self, chunk: Vec<u8>) -> Result<(), ClientError> {
        self.channel
            .send(chunk)
            .map_err(|error| ClientError::disconnected(error.to_string()))
    }

    fn close(&self) {
        self.channel.close();
    }
}

async fn start_server(
    listener: Arc<dyn ServerListener>,
) -> (Arc<TestServerHost>, Arc<Server<SessionId>>) {
    let host = Arc::new(TestServerHost::new());
    host.seed("session-1");
    let options = ServerOptions::new(SERVER_ID, vec![listener]);
    let server =
        Server::new(Arc::clone(&host) as Arc<dyn ServerHost<SessionId>>, options).expect("server");
    server.start().await.expect("start");
    (host, server)
}

fn server_target() -> RpcTarget {
    RpcTarget::Server(pi_protocol::rpc::ServerTarget {
        server_id: SERVER_ID.to_owned(),
    })
}

#[tokio::test]
async fn in_memory_server_round_trips_requests_and_shutdown() {
    let listener = MemoryListener::new();
    let (host, server) = start_server(Arc::clone(&listener) as Arc<dyn ServerListener>).await;

    let channel = listener.connect().expect("in-memory connection");
    let factory: Arc<dyn ByteTransportFactory> = Arc::new(MemoryFactory { channel });
    let client = Client::connect_with(ClientOptions::new(factory, SERVER_ID))
        .await
        .expect("handshake");
    assert_eq!(client.connection_state(), ConnectionState::Connected);
    assert_eq!(client.server_id(), SERVER_ID);

    // Server-target call: attach this connection to a seeded session.
    let (attachments_tx, mut attachments_rx) = mpsc::unbounded_channel::<Option<SessionTarget>>();
    let _subscription = client
        .on_attachment_change(Arc::new(move |attachment| {
            let _ = attachments_tx.send(attachment.cloned());
        }))
        .expect("listener");

    client
        .request(
            server_target(),
            ServiceCall::new("pi.session-management", "attach", vec![json!("session-1")]),
            None,
        )
        .await
        .expect("attach");

    let attachment = tokio::time::timeout(Duration::from_secs(2), attachments_rx.recv())
        .await
        .expect("attachment event")
        .expect("channel")
        .expect("attached");
    assert_eq!(attachment.session_id, "session-1");
    assert_eq!(host.latest_harness("session-1").attached_clients(), 1);

    // Session-target call: the harness answers with `true`.
    let result = client
        .request(
            RpcTarget::Session(attachment),
            ServiceCall::new("pi.test", "echo", vec![json!({ "n": 1 })]),
            None,
        )
        .await
        .expect("session request");
    assert_eq!(result, Some(json!(true)));
    assert_eq!(host.latest_harness("session-1").service_calls().len(), 1);

    // Closing the server closes the client's transport.
    server.close().await.expect("close");
    let mut waited = 0;
    while client.connection_state() != ConnectionState::Disconnected {
        tokio::time::sleep(Duration::from_millis(10)).await;
        waited += 1;
        assert!(waited < 200, "client never observed the close");
    }
    client.dispose().await;
}

#[cfg(unix)]
#[tokio::test]
async fn unix_transport_round_trips_against_a_real_server() {
    use pi_client::unix::{create_unix_transport_factory, UnixTransportOptions};
    use pi_server::transports::UnixServerListener;

    let path = std::env::temp_dir().join(format!(
        "pi-client-e2e-{}-{}.sock",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .subsec_nanos()
    ));
    let _ = std::fs::remove_file(&path);

    let listener = UnixServerListener::new(path.clone());
    let (host, server) = start_server(Arc::clone(&listener) as Arc<dyn ServerListener>).await;

    let factory = create_unix_transport_factory(UnixTransportOptions::new(&path)).expect("factory");
    let client = Client::connect_with(ClientOptions::new(factory, SERVER_ID))
        .await
        .expect("handshake");

    client
        .request(
            server_target(),
            ServiceCall::new("pi.session-management", "attach", vec![json!("session-1")]),
            None,
        )
        .await
        .expect("attach");

    let mut waited = 0;
    let attachment = loop {
        if let Some(attachment) = client.attachment() {
            break attachment;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
        waited += 1;
        assert!(waited < 200, "the attachment never arrived");
    };

    let result = client
        .request(
            RpcTarget::Session(attachment),
            ServiceCall::new("pi.test", "echo", vec![json!(true)]),
            None,
        )
        .await
        .expect("session request");
    assert_eq!(result, Some(json!(true)));
    assert_eq!(host.latest_harness("session-1").service_calls().len(), 1);

    client.dispose().await;
    server.close().await.expect("close");
    assert!(!path.exists(), "the server removed its socket");
}
