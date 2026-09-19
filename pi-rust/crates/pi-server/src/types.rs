//! Host, routing and server configuration types — Rust port of
//! `packages/server/src/types.ts`.

use std::sync::Arc;

use futures::future::{BoxFuture, Shared};
use pi_chord::context::Context;
use pi_chord::services::{ServiceCall, ServiceProviderUpdate};
use pi_protocol::rpc::{RpcTarget, SessionTarget};
use serde_json::Value;

use crate::errors::ServerError;
use crate::listener::ServerListener;

/// A future that resolves when a hosted session terminates.
///
/// `None` means an expected close; `Some(error)` means unexpected termination.
pub type TerminationFuture = Shared<BoxFuture<'static, Option<ServerError>>>;

/// Publishes one subscription update for one request's view of a connection.
///
/// This is the synchronous Rust form of upstream's `publish` callback: the
/// current subscription state encoder is applied by the server before the
/// update reaches the wire.
pub type ServicePublish = Arc<
    dyn Fn(&str, &ServiceProviderUpdate, &Context) -> Result<(), ServerError> + Send + Sync,
>;

/// The durable identity of one hosted session.
///
/// Upstream's `SessionMetadata` lives in `pi-agent-core`; the Rust workspace
/// keeps session metadata inside the session crate, so the server only needs
/// the id (used for routing and the `attachment` envelope) and, optionally,
/// the parent id.
pub trait SessionMetadata: Send + Sync + Clone + 'static {
    /// The durable session id.
    fn id(&self) -> &str;

    /// The parent session id, when this is a child session.
    fn parent_session_id(&self) -> Option<&str> {
        None
    }
}

/// The minimal metadata a router needs when the host uses strings directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionId {
    id: String,
    parent_session_id: Option<String>,
}

impl SessionId {
    /// Creates metadata for `id`.
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            parent_session_id: None,
        }
    }

    /// Creates metadata for `id` with a parent.
    pub fn with_parent(id: impl Into<String>, parent_session_id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            parent_session_id: Some(parent_session_id.into()),
        }
    }
}

impl SessionMetadata for SessionId {
    fn id(&self) -> &str {
        &self.id
    }

    fn parent_session_id(&self) -> Option<&str> {
        self.parent_session_id.as_deref()
    }
}

/// One presentation connection's live capability for a hosted session.
pub trait RoutedSessionAttachment: Send + Sync {
    /// Routes one contract-agnostic service operation to the attached session.
    fn invoke_service(
        &self,
        call: &ServiceCall,
        publish: &ServicePublish,
        context: &Context,
    ) -> Result<Option<Value>, ServerError>;

    /// Releases the attachment. Idempotent.
    fn release(&self, context: &Context) -> Result<(), ServerError>;
}

/// Presentation-scoped routing capabilities available to server services.
pub trait RoutedServerPresentation: Send + Sync {
    /// Attaches this connection to `session_id`, publishing the attachment.
    fn attach_session(&self, session_id: &str, context: &Context) -> Result<(), ServerError>;

    /// Detaches this connection from whatever session it is attached to.
    fn detach_session(&self, context: &Context) -> Result<(), ServerError>;

    /// Releases routed attachments and handles before durable metadata is deleted.
    fn prepare_session_removal(&self, session_id: &str, context: &Context) -> Result<(), ServerError>;
}

/// One connection's server-scoped service endpoint.
pub trait RoutedServerServiceAttachment: Send + Sync {
    /// Invokes one server-scoped service call.
    fn invoke_service(
        &self,
        call: &ServiceCall,
        publish: &ServicePublish,
        context: &Context,
    ) -> Result<Option<Value>, ServerError>;

    /// Releases the server service attachment. Idempotent.
    fn release(&self, context: &Context) -> Result<(), ServerError>;
}

/// Creates a server-scoped service attachment for one connection.
pub trait RoutedServerServiceHost: Send + Sync {
    /// Attaches `presentation` and returns the connection's service endpoint.
    fn attach_client(
        &self,
        presentation: Arc<dyn RoutedServerPresentation>,
        context: &Context,
    ) -> Result<Arc<dyn RoutedServerServiceAttachment>, ServerError>;
}

/// A process-safe handle that acquires presentation-scoped session capabilities.
pub trait RoutedSessionHandle: Send + Sync {
    /// Acquires a lease for one presentation connection.
    fn attach_client(&self, context: &Context) -> Result<Arc<dyn RoutedSessionAttachment>, ServerError>;

    /// Resolves when the session terminates.
    fn terminated(&self) -> Option<TerminationFuture> {
        None
    }

    /// Closes the hosted session.
    fn close(&self, context: &Context) -> Result<(), ServerError>;
}

/// Application capabilities used by server-wide management and session routing.
pub trait ServerHost<TMetadata: SessionMetadata>: Send + Sync {
    /// The server-scoped service host.
    fn server_services(&self) -> Arc<dyn RoutedServerServiceHost>;

    /// Resolves one durable session id, or returns a bounded routing error.
    fn resolve_session(&self, session_id: &str, context: &Context) -> Result<TMetadata, ServerError>;

    /// Opens one resolved session.
    fn open_session(
        &self,
        metadata: TMetadata,
        context: &Context,
    ) -> Result<Arc<dyn RoutedSessionHandle>, ServerError>;
}

/// Publishing hook the router calls when a connection's attachment changes.
pub trait AttachmentSink: Send + Sync {
    /// Publishes the current attachment, or `None` when the client detached.
    fn publish(&self, attachment: Option<SessionTarget>);
}

/// Notifies a connection that its session attachment changed.
///
/// Internal to the crate; the router stores one per attached client.
pub(crate) struct SinkFn(pub Arc<dyn Fn(Option<SessionTarget>) + Send + Sync>);

impl AttachmentSink for SinkFn {
    fn publish(&self, attachment: Option<SessionTarget>) {
        (self.0)(attachment)
    }
}

/// How the server reports an unexpected failure that has no request to answer.
pub type ErrorObserver = Arc<dyn Fn(ServerError) + Send + Sync>;

/// How the server reports its live connection count.
pub type ConnectionCountObserver = Arc<dyn Fn(usize) + Send + Sync>;

/// Options passed to [`Server::new`](crate::server::Server::new).
pub struct ServerOptions {
    /// The listeners that supply authorized byte connections.
    pub listeners: Vec<Arc<dyn ServerListener>>,
    /// Stable logical server identity supplied by the installation or profile.
    pub server_id: String,
    /// Maximum framed byte length. Defaults to
    /// [`DEFAULT_MAX_FRAME_LENGTH`](pi_protocol::rpc::DEFAULT_MAX_FRAME_LENGTH).
    pub max_frame_length: Option<usize>,
    /// Handshake deadline in milliseconds. Defaults to 5_000.
    pub handshake_timeout_ms: Option<u64>,
    /// Called whenever the live connection count changes.
    pub on_connection_count_changed: Option<ConnectionCountObserver>,
    /// Called for every failure the server cannot report on the wire.
    pub on_error: Option<ErrorObserver>,
}

impl ServerOptions {
    /// Creates options for `server_id` with `listeners`.
    pub fn new(server_id: impl Into<String>, listeners: Vec<Arc<dyn ServerListener>>) -> Self {
        Self {
            listeners,
            server_id: server_id.into(),
            max_frame_length: None,
            handshake_timeout_ms: None,
            on_connection_count_changed: None,
            on_error: None,
        }
    }

    /// Overrides the maximum frame length.
    pub fn with_max_frame_length(mut self, max_frame_length: usize) -> Self {
        self.max_frame_length = Some(max_frame_length);
        self
    }

    /// Overrides the handshake timeout.
    pub fn with_handshake_timeout_ms(mut self, handshake_timeout_ms: u64) -> Self {
        self.handshake_timeout_ms = Some(handshake_timeout_ms);
        self
    }

    /// Installs a connection-count observer.
    pub fn with_connection_count_observer(mut self, observer: ConnectionCountObserver) -> Self {
        self.on_connection_count_changed = Some(observer);
        self
    }

    /// Installs an error observer.
    pub fn with_error_observer(mut self, observer: ErrorObserver) -> Self {
        self.on_error = Some(observer);
        self
    }
}

/// The address of one routed request target.
pub fn target_session(target: &RpcTarget) -> Option<&SessionTarget> {
    match target {
        RpcTarget::Session(session) => Some(session),
        RpcTarget::Server(_) => None,
    }
}
