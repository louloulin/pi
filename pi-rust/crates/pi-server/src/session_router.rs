//! Session routing and multi-session lifecycle — Rust port of
//! `packages/server/src/session-router.ts`.
//!
//! The router keeps at most one attachment per connection. A request whose
//! target names a different session or attachment is answered with
//! `session_not_attached` instead of silently re-routing, matching upstream.
//!
//! # Deliberate deviations
//!
//! * Upstream chains per-client promises so that two routing operations for one
//!   connection never interleave. This port is synchronous, so a single
//!   [`Mutex`](parking_lot::Mutex) over the router state provides the same
//!   guarantee.
//! * Upstream's `terminated` handling awaits a promise; here it is an optional
//!   [`TerminationFuture`](crate::types::TerminationFuture) polled by a spawned
//!   Tokio task when a runtime is available.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use parking_lot::Mutex;
use pi_chord::context::Context;
use pi_chord::services::ServiceCall;
use pi_protocol::rpc::{RpcTarget, SessionTarget};
use serde_json::Value;
use uuid::Uuid;

use crate::errors::ServerError;
use crate::types::{
    target_session, AttachmentSink, ErrorObserver, RoutedSessionAttachment, RoutedSessionHandle,
    ServerHost, ServicePublish, SessionMetadata,
};

/// The opaque connection identity used to key attachments.
pub(crate) type ClientKey = u64;

struct Attachment {
    id: String,
    client: ClientKey,
    session_id: String,
    lease: Arc<dyn RoutedSessionAttachment>,
    sink: Arc<dyn AttachmentSink>,
}

struct HostedSessionEntry {
    handle: Arc<dyn RoutedSessionHandle>,
    attachments: HashSet<ClientKey>,
}

struct RouterState {
    hosted: HashMap<String, HostedSessionEntry>,
    attachments_by_client: HashMap<ClientKey, Arc<Attachment>>,
}

/// Routes service calls to hosted sessions and tracks their lifecycle.
pub struct SessionRouter<TMetadata: SessionMetadata> {
    host: Arc<dyn ServerHost<TMetadata>>,
    server_id: String,
    is_closing: Arc<dyn Fn() -> bool + Send + Sync>,
    report_error: Option<ErrorObserver>,
    state: Mutex<RouterState>,
}

impl<TMetadata: SessionMetadata> SessionRouter<TMetadata> {
    /// Creates a router for `server_id` over `host`.
    pub fn new(
        host: Arc<dyn ServerHost<TMetadata>>,
        server_id: impl Into<String>,
        is_closing: Arc<dyn Fn() -> bool + Send + Sync>,
        report_error: Option<ErrorObserver>,
    ) -> Self {
        Self {
            host,
            server_id: server_id.into(),
            is_closing,
            report_error,
            state: Mutex::new(RouterState {
                hosted: HashMap::new(),
                attachments_by_client: HashMap::new(),
            }),
        }
    }

    /// The logical server id embedded in attachment envelopes.
    pub fn server_id(&self) -> &str {
        &self.server_id
    }

    /// Routes one call to the attachment named by `target`.
    pub fn execute_service_call(
        &self,
        call: &ServiceCall,
        target: &RpcTarget,
        client: ClientKey,
        publish: &ServicePublish,
        context: &Context,
    ) -> Result<Option<Value>, ServerError> {
        let lease = {
            let state = self.state.lock();
            if (self.is_closing)() {
                return Err(ServerError::server_draining());
            }
            let session = target_session(target).ok_or_else(ServerError::session_not_attached)?;
            state
                .attachments_by_client
                .get(&client)
                .filter(|attachment| {
                    attachment.session_id == session.session_id
                        && attachment.id == session.attachment_id
                })
                .map(|attachment| Arc::clone(&attachment.lease))
                .ok_or_else(ServerError::session_not_attached)?
        };
        lease.invoke_service(call, publish, context)
    }

    /// Attaches `client` to `session_id`, publishing the new attachment.
    pub fn attach_client(
        self: &Arc<Self>,
        client: ClientKey,
        session_id: &str,
        context: &Context,
        sink: Arc<dyn AttachmentSink>,
    ) -> Result<(), ServerError> {
        if (self.is_closing)() {
            return Err(ServerError::server_draining());
        }
        {
            let state = self.state.lock();
            if let Some(current) = state.attachments_by_client.get(&client) {
                if current.session_id == session_id {
                    return Ok(());
                }
            }
        }
        let (key, handle) = self.acquire_session(session_id, context)?;
        if (self.is_closing)() {
            return Err(ServerError::server_draining());
        }
        let current = {
            let state = self.state.lock();
            state.attachments_by_client.get(&client).cloned()
        };
        if let Some(current) = current {
            if current.session_id != key {
                self.release_attachment(&current, context, false)?;
            }
        }
        let lease = handle.attach_client(context)?;
        let attachment = Arc::new(Attachment {
            id: Uuid::new_v4().to_string(),
            client,
            session_id: key.clone(),
            lease,
            sink,
        });
        {
            let mut state = self.state.lock();
            match state.hosted.get_mut(&key) {
                Some(entry) => {
                    entry.attachments.insert(client);
                }
                None => {
                    return Err(ServerError::session_not_found(format!(
                        "Unknown session: {session_id}"
                    )));
                }
            }
            state
                .attachments_by_client
                .insert(client, Arc::clone(&attachment));
        }
        attachment.sink.publish(Some(SessionTarget {
            server_id: self.server_id.clone(),
            session_id: key,
            attachment_id: attachment.id.clone(),
        }));
        Ok(())
    }

    /// Detaches `client` from its current session, publishing the detach.
    pub fn detach_client(&self, client: ClientKey, context: &Context) -> Result<(), ServerError> {
        let current = {
            let state = self.state.lock();
            state.attachments_by_client.get(&client).cloned()
        };
        match current {
            Some(attachment) => self.release_attachment(&attachment, context, true),
            None => Ok(()),
        }
    }

    /// Releases every attachment and closes the handle for `session_id`.
    pub fn remove_session(&self, session_id: &str, context: &Context) -> Result<(), ServerError> {
        if (self.is_closing)() {
            return Err(ServerError::server_draining());
        }
        let (handle, attachments) = {
            let state = self.state.lock();
            match state.hosted.get(session_id) {
                Some(entry) => (
                    Arc::clone(&entry.handle),
                    entry
                        .attachments
                        .iter()
                        .filter_map(|client| state.attachments_by_client.get(client).cloned())
                        .collect::<Vec<_>>(),
                ),
                None => return Ok(()),
            }
        };
        let mut first_error: Option<ServerError> = None;
        for attachment in attachments {
            if let Err(error) = self.release_attachment(&attachment, context, true) {
                first_error.get_or_insert(error);
            }
        }
        if let Err(error) = handle.close(context) {
            first_error.get_or_insert(error);
        }
        {
            let mut state = self.state.lock();
            if state
                .hosted
                .get(session_id)
                .is_some_and(|entry| Arc::ptr_eq(&entry.handle, &handle))
            {
                state.hosted.remove(session_id);
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    /// Releases `client`'s attachment without publishing a detach.
    pub fn disconnect(&self, client: ClientKey, context: &Context) -> Result<(), ServerError> {
        let current = {
            let state = self.state.lock();
            state.attachments_by_client.get(&client).cloned()
        };
        match current {
            Some(attachment) => self.release_attachment(&attachment, context, false),
            None => Ok(()),
        }
    }

    /// Releases every attachment and closes every hosted session.
    pub fn close(&self, context: &Context) -> Result<(), ServerError> {
        let (attachments, handles) = {
            let state = self.state.lock();
            (
                state
                    .attachments_by_client
                    .values()
                    .cloned()
                    .collect::<Vec<_>>(),
                state
                    .hosted
                    .values()
                    .map(|entry| Arc::clone(&entry.handle))
                    .collect::<Vec<_>>(),
            )
        };
        let mut first_error: Option<ServerError> = None;
        for attachment in attachments {
            if let Err(error) = self.release_attachment(&attachment, context, false) {
                first_error.get_or_insert(error);
            }
        }
        for handle in handles {
            if let Err(error) = handle.close(context) {
                first_error.get_or_insert(error);
            }
        }
        {
            let mut state = self.state.lock();
            state.attachments_by_client.clear();
            state.hosted.clear();
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    fn acquire_session(
        self: &Arc<Self>,
        session_id: &str,
        context: &Context,
    ) -> Result<(String, Arc<dyn RoutedSessionHandle>), ServerError> {
        {
            let state = self.state.lock();
            if let Some(entry) = state.hosted.get(session_id) {
                return Ok((session_id.to_owned(), Arc::clone(&entry.handle)));
            }
        }
        let metadata = self.host.resolve_session(session_id, context)?;
        let key = metadata.id().to_owned();
        let handle = self.host.open_session(metadata, context)?;
        if (self.is_closing)() {
            handle.close(context)?;
            return Err(ServerError::server_draining());
        }
        {
            let mut state = self.state.lock();
            state.hosted.insert(
                key.clone(),
                HostedSessionEntry {
                    handle: Arc::clone(&handle),
                    attachments: HashSet::new(),
                },
            );
        }
        if let Some(terminated) = handle.terminated() {
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                let router = Arc::clone(self);
                let watched_key = key.clone();
                let watched_handle = Arc::clone(&handle);
                runtime.spawn(async move {
                    let error = terminated.await;
                    router.invalidate(&watched_key, &watched_handle, error);
                });
            }
        }
        Ok((key, handle))
    }

    fn release_attachment(
        &self,
        attachment: &Arc<Attachment>,
        context: &Context,
        publish: bool,
    ) -> Result<(), ServerError> {
        let was_current = {
            let mut state = self.state.lock();
            if let Some(entry) = state.hosted.get_mut(&attachment.session_id) {
                entry.attachments.remove(&attachment.client);
            }
            let current = state
                .attachments_by_client
                .get(&attachment.client)
                .is_some_and(|current| Arc::ptr_eq(current, attachment));
            if current {
                state.attachments_by_client.remove(&attachment.client);
            }
            current
        };
        let result = attachment.lease.release(context);
        if publish && was_current {
            attachment.sink.publish(None);
        }
        result
    }

    fn invalidate(
        &self,
        session_id: &str,
        handle: &Arc<dyn RoutedSessionHandle>,
        error: Option<ServerError>,
    ) {
        let attachments = {
            let mut state = self.state.lock();
            let matches = state
                .hosted
                .get(session_id)
                .is_some_and(|entry| Arc::ptr_eq(&entry.handle, handle));
            if !matches {
                return;
            }
            let entry = state
                .hosted
                .remove(session_id)
                .expect("entry checked above");
            entry
                .attachments
                .iter()
                .filter_map(|client| state.attachments_by_client.get(client).cloned())
                .collect::<Vec<_>>()
        };
        for attachment in attachments {
            if let Err(release_error) =
                self.release_attachment(&attachment, pi_chord::context::background_context(), true)
            {
                self.report(release_error);
            }
        }
        if let Some(error) = error {
            self.report(error);
        }
    }

    fn report(&self, error: ServerError) {
        if let Some(observer) = &self.report_error {
            observer(error);
        }
    }
}
