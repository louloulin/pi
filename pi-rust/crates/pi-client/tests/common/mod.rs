//! Shared helpers for the integration tests.
//!
//! Everything here is built on [`pi_client::testing`], so a test reads as a
//! protocol transcript instead of a pile of plumbing.

use std::sync::Arc;

use pi_client::testing::{memory_transport, ScriptedServer};
use pi_client::{ByteTransportFactory, Client, ClientError, ClientOptions};
use pi_protocol::rpc::{ClientMessage, RpcTarget, ServerTarget};

/// The server id every scripted test uses unless it is testing a mismatch.
pub const SERVER_ID: &str = "00000000-0000-4000-8000-000000000001";
/// A second valid server id, used for mismatch and stale-target tests.
pub const OTHER_SERVER_ID: &str = "00000000-0000-4000-8000-000000000002";

/// An in-memory transport factory and the scripted server behind it.
pub struct Pairing {
    /// The factory handed to [`ClientOptions`].
    pub factory: Arc<dyn ByteTransportFactory>,
    /// The scripted server peer.
    pub server: ScriptedServer,
}

/// A fresh in-memory pairing.
pub fn pairing() -> Pairing {
    let (factory, server) = memory_transport();
    Pairing { factory, server }
}

impl Pairing {
    /// An unconnected client for this pairing.
    pub fn client(&self) -> Client {
        Client::new(ClientOptions::new(Arc::clone(&self.factory), SERVER_ID)).expect("client")
    }

    /// Runs the handshake against `server_id` and returns the client.
    pub async fn handshake(&self, server_id: &str) -> Result<Client, ClientError> {
        let factory = Arc::clone(&self.factory);
        let connecting = tokio::spawn(async move {
            Client::connect_with(ClientOptions::new(factory, SERVER_ID)).await
        });
        self.server.accept_handshake(server_id).await;
        connecting.await.expect("join")
    }

    /// Connects a client and answers its handshake.
    pub async fn connect(&self) -> Client {
        self.handshake(SERVER_ID).await.expect("handshake")
    }
}

/// Connects a client to a fresh pairing, returning both ends.
pub async fn connected() -> (Client, ScriptedServer) {
    let pairing = pairing();
    let client = pairing.connect().await;
    (client, pairing.server)
}

/// The routed server target of the connected server.
pub fn server_target() -> RpcTarget {
    RpcTarget::Server(ServerTarget {
        server_id: SERVER_ID.to_owned(),
    })
}

/// Awaits the next client message and returns it.
pub async fn next_message(server: &ScriptedServer) -> ClientMessage {
    server.recv().await.expect("client message")
}

/// Awaits the next request and fails it with a bounded protocol error.
pub async fn fail_next_request(server: &ScriptedServer, code: &str, message: &str) {
    match next_message(server).await {
        ClientMessage::Request { id, .. } => server.respond_error(&id, code, message),
        other => panic!("expected a request, received {other:?}"),
    }
}

/// Awaits the next cancel frame and returns its request id.
pub async fn next_cancel(server: &ScriptedServer) -> String {
    match next_message(server).await {
        ClientMessage::Cancel { id, .. } => id,
        other => panic!("expected a cancel, received {other:?}"),
    }
}
