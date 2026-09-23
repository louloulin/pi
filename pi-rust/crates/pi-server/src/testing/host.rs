//! Deterministic host double used by the offline conformance tests.
//!
//! Mirrors `packages/server/src/testing/**`. The host owns scripted session
//! harnesses so a test can observe attachment counts, gate a service call
//! mid-flight, inject failures and terminate a session out of band.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use futures::future::{BoxFuture, FutureExt};
use parking_lot::Mutex;
use pi_chord::context::Context;
use pi_chord::services::ServiceCall;
use serde_json::Value;
use tokio::sync::oneshot;

use crate::errors::ServerError;
use crate::types::{
    RoutedServerPresentation, RoutedServerServiceAttachment, RoutedServerServiceHost,
    RoutedSessionAttachment, RoutedSessionHandle, ServerHost, SessionId, SessionMetadata,
    TerminationFuture,
};

/// A one-shot rendezvous between a test and a blocked host operation.
pub struct Gate {
    entered: oneshot::Receiver<()>,
    release: std::sync::mpsc::Sender<()>,
}

impl Gate {
    /// Waits until the blocked operation has been entered.
    pub async fn wait_entered(&mut self) {
        let _ = (&mut self.entered).await;
    }

    /// Releases the blocked operation.
    pub fn release(&self) {
        let _ = self.release.send(());
    }
}

struct GateWaiter {
    entered: Option<oneshot::Sender<()>>,
    release: std::sync::mpsc::Receiver<()>,
}

impl GateWaiter {
    fn wait(&mut self) {
        if let Some(entered) = self.entered.take() {
            let _ = entered.send(());
        }
        let _ = self.release.recv();
    }
}

fn gate() -> (Gate, GateWaiter) {
    let (entered_tx, entered_rx) = oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    (
        Gate {
            entered: entered_rx,
            release: release_tx,
        },
        GateWaiter {
            entered: Some(entered_tx),
            release: release_rx,
        },
    )
}

/// Scripted state for one hosted session.
pub struct TestHarness {
    session: SessionId,
    attached_clients: AtomicUsize,
    attachment_release_count: AtomicUsize,
    close_count: AtomicUsize,
    service_calls: Mutex<Vec<ServiceCall>>,
    next_attach_error: Mutex<Option<ServerError>>,
    next_service_error: Mutex<Option<ServerError>>,
    next_service_result: Mutex<Value>,
    next_release_error: Mutex<Option<ServerError>>,
    next_close_error: Mutex<Option<ServerError>>,
    next_service_gate: Mutex<Option<GateWaiter>>,
    next_close_gate: Mutex<Option<GateWaiter>>,
    termination_tx: Mutex<Option<oneshot::Sender<Option<ServerError>>>>,
    termination_rx: Mutex<Option<oneshot::Receiver<Option<ServerError>>>>,
}

impl TestHarness {
    /// Creates a harness for `session`.
    pub fn new(session: SessionId) -> Self {
        let (termination_tx, termination_rx) = oneshot::channel();
        Self {
            session,
            attached_clients: AtomicUsize::new(0),
            attachment_release_count: AtomicUsize::new(0),
            close_count: AtomicUsize::new(0),
            service_calls: Mutex::new(Vec::new()),
            next_attach_error: Mutex::new(None),
            next_service_error: Mutex::new(None),
            next_service_result: Mutex::new(Value::Bool(true)),
            next_release_error: Mutex::new(None),
            next_close_error: Mutex::new(None),
            next_service_gate: Mutex::new(None),
            next_close_gate: Mutex::new(None),
            termination_tx: Mutex::new(Some(termination_tx)),
            termination_rx: Mutex::new(Some(termination_rx)),
        }
    }

    /// The session id this harness serves.
    pub fn session_id(&self) -> &str {
        self.session.id()
    }

    /// The number of live attachments.
    pub fn attached_clients(&self) -> usize {
        self.attached_clients.load(Ordering::SeqCst)
    }

    /// The number of attachment releases observed.
    pub fn attachment_release_count(&self) -> usize {
        self.attachment_release_count.load(Ordering::SeqCst)
    }

    /// The number of close calls observed.
    pub fn close_count(&self) -> usize {
        self.close_count.load(Ordering::SeqCst)
    }

    /// The service calls observed, in delivery order.
    pub fn service_calls(&self) -> Vec<ServiceCall> {
        self.service_calls.lock().clone()
    }

    /// Injects the next attachment failure.
    pub fn fail_next_attach(&self, error: ServerError) {
        *self.next_attach_error.lock() = Some(error);
    }

    /// Injects the next service failure.
    pub fn fail_next_service(&self, error: ServerError) {
        *self.next_service_error.lock() = Some(error);
    }

    /// Sets the value the next service call returns.
    pub fn set_next_service_result(&self, value: Value) {
        *self.next_service_result.lock() = value;
    }

    /// Injects the next release failure.
    pub fn fail_next_release(&self, error: ServerError) {
        *self.next_release_error.lock() = Some(error);
    }

    /// Injects the next close failure.
    pub fn fail_next_close(&self, error: ServerError) {
        *self.next_close_error.lock() = Some(error);
    }

    /// Gates the next service call.
    pub fn gate_next_service_call(&self) -> Gate {
        let (public, waiter) = gate();
        *self.next_service_gate.lock() = Some(waiter);
        public
    }

    /// Gates the next close.
    pub fn gate_next_close(&self) -> Gate {
        let (public, waiter) = gate();
        *self.next_close_gate.lock() = Some(waiter);
        public
    }

    /// Terminates the session out of band.
    pub fn terminate(&self, error: Option<ServerError>) {
        if let Some(sender) = self.termination_tx.lock().take() {
            let _ = sender.send(error);
        }
    }

    fn invoke_service(&self, call: &ServiceCall) -> Result<Option<Value>, ServerError> {
        self.service_calls.lock().push(call.clone());
        if let Some(error) = self.next_service_error.lock().take() {
            return Err(error);
        }
        if let Some(mut waiter) = self.next_service_gate.lock().take() {
            waiter.wait();
        }
        let mut result = self.next_service_result.lock();
        Ok(Some(std::mem::replace(&mut *result, Value::Bool(true))))
    }

    fn take_termination(&self) -> Option<TerminationFuture> {
        let receiver = self.termination_rx.lock().take()?;
        let future: BoxFuture<'static, Option<ServerError>> =
            Box::pin(async move { receiver.await.ok().flatten() });
        Some(future.shared())
    }
}

struct HarnessAttachment {
    harness: Arc<TestHarness>,
    released: AtomicBool,
}

impl RoutedSessionAttachment for HarnessAttachment {
    fn invoke_service(
        &self,
        call: &ServiceCall,
        _publish: &crate::types::ServicePublish,
        _context: &Context,
    ) -> Result<Option<Value>, ServerError> {
        self.harness.invoke_service(call)
    }

    fn release(&self, _context: &Context) -> Result<(), ServerError> {
        if self.released.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        self.harness
            .attachment_release_count
            .fetch_add(1, Ordering::SeqCst);
        self.harness.attached_clients.fetch_sub(1, Ordering::SeqCst);
        match self.harness.next_release_error.lock().take() {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

struct HarnessHandle(Arc<TestHarness>);

impl RoutedSessionHandle for HarnessHandle {
    fn attach_client(
        &self,
        _context: &Context,
    ) -> Result<Arc<dyn RoutedSessionAttachment>, ServerError> {
        if let Some(error) = self.0.next_attach_error.lock().take() {
            return Err(error);
        }
        self.0.attached_clients.fetch_add(1, Ordering::SeqCst);
        Ok(Arc::new(HarnessAttachment {
            harness: Arc::clone(&self.0),
            released: AtomicBool::new(false),
        }))
    }

    fn terminated(&self) -> Option<TerminationFuture> {
        self.0.take_termination()
    }

    fn close(&self, _context: &Context) -> Result<(), ServerError> {
        self.0.close_count.fetch_add(1, Ordering::SeqCst);
        if let Some(mut waiter) = self.0.next_close_gate.lock().take() {
            waiter.wait();
        }
        if let Some(error) = self.0.next_close_error.lock().take() {
            return Err(error);
        }
        if let Some(sender) = self.0.termination_tx.lock().take() {
            let _ = sender.send(None);
        }
        Ok(())
    }
}

struct TestServerServicesAttachment {
    presentation: Arc<dyn RoutedServerPresentation>,
}

impl RoutedServerServiceAttachment for TestServerServicesAttachment {
    fn invoke_service(
        &self,
        call: &ServiceCall,
        _publish: &crate::types::ServicePublish,
        context: &Context,
    ) -> Result<Option<Value>, ServerError> {
        if call.instance.is_none()
            && call.service_id == "pi.session-management"
            && call.member == "attach"
            && call.args.len() == 1
        {
            let session_id = call.args[0]
                .as_str()
                .ok_or_else(|| ServerError::internal("attach requires a session id"))?;
            self.presentation.attach_session(session_id, context)?;
            return Ok(None);
        }
        if call.instance.is_none()
            && call.service_id == "pi.session-management"
            && call.member == "detach"
            && call.args.is_empty()
        {
            self.presentation.detach_session(context)?;
            return Ok(None);
        }
        Err(ServerError::internal(format!(
            "Unsupported test server service {}.{}",
            call.service_id, call.member
        )))
    }

    fn release(&self, _context: &Context) -> Result<(), ServerError> {
        Ok(())
    }
}

struct TestServerServices;

impl RoutedServerServiceHost for TestServerServices {
    fn attach_client(
        &self,
        presentation: Arc<dyn RoutedServerPresentation>,
        _context: &Context,
    ) -> Result<Arc<dyn RoutedServerServiceAttachment>, ServerError> {
        Ok(Arc::new(TestServerServicesAttachment { presentation }))
    }
}

/// The scripted [`ServerHost`] used by the conformance tests.
pub struct TestServerHost {
    server_services: Arc<TestServerServices>,
    sessions: Mutex<HashMap<String, SessionId>>,
    harnesses: Mutex<HashMap<String, Vec<Arc<TestHarness>>>>,
    open_session_count: AtomicUsize,
    next_open_session_error: Mutex<Option<ServerError>>,
    next_open_session_gate: Mutex<Option<GateWaiter>>,
}

impl Default for TestServerHost {
    fn default() -> Self {
        Self::new()
    }
}

impl TestServerHost {
    /// Creates an empty host.
    pub fn new() -> Self {
        Self {
            server_services: Arc::new(TestServerServices),
            sessions: Mutex::new(HashMap::new()),
            harnesses: Mutex::new(HashMap::new()),
            open_session_count: AtomicUsize::new(0),
            next_open_session_error: Mutex::new(None),
            next_open_session_gate: Mutex::new(None),
        }
    }

    /// Registers one durable session.
    pub fn seed(&self, id: &str) -> SessionId {
        let metadata = SessionId::new(id);
        self.sessions.lock().insert(id.to_owned(), metadata.clone());
        metadata
    }

    /// Registers one durable child session.
    pub fn seed_child(&self, id: &str, parent_session_id: &str) -> SessionId {
        let metadata = SessionId::with_parent(id, parent_session_id);
        self.sessions.lock().insert(id.to_owned(), metadata.clone());
        metadata
    }

    /// The number of open-session calls observed.
    pub fn open_session_count(&self) -> usize {
        self.open_session_count.load(Ordering::SeqCst)
    }

    /// Injects the next open-session failure.
    pub fn fail_next_open_session(&self, error: ServerError) {
        *self.next_open_session_error.lock() = Some(error);
    }

    /// Gates the next open-session call.
    pub fn gate_next_open_session(&self) -> Gate {
        let (public, waiter) = gate();
        *self.next_open_session_gate.lock() = Some(waiter);
        public
    }

    /// The most recently opened harness for `id`.
    pub fn latest_harness(&self, id: &str) -> Arc<TestHarness> {
        self.harnesses
            .lock()
            .get(id)
            .and_then(|harnesses| harnesses.last().cloned())
            .unwrap_or_else(|| panic!("no harness for {id}"))
    }

    /// The number of harnesses opened for `id`.
    pub fn harness_count(&self, id: &str) -> usize {
        self.harnesses
            .lock()
            .get(id)
            .map(Vec::len)
            .unwrap_or_default()
    }
}

impl ServerHost<SessionId> for TestServerHost {
    fn server_services(&self) -> Arc<dyn RoutedServerServiceHost> {
        Arc::clone(&self.server_services) as Arc<dyn RoutedServerServiceHost>
    }

    fn resolve_session(
        &self,
        session_id: &str,
        _context: &Context,
    ) -> Result<SessionId, ServerError> {
        self.sessions
            .lock()
            .get(session_id)
            .cloned()
            .ok_or_else(|| ServerError::session_not_found(format!("Unknown session: {session_id}")))
    }

    fn open_session(
        &self,
        metadata: SessionId,
        _context: &Context,
    ) -> Result<Arc<dyn RoutedSessionHandle>, ServerError> {
        self.open_session_count.fetch_add(1, Ordering::SeqCst);
        if let Some(mut waiter) = self.next_open_session_gate.lock().take() {
            waiter.wait();
        }
        if let Some(error) = self.next_open_session_error.lock().take() {
            return Err(error);
        }
        let harness = Arc::new(TestHarness::new(metadata.clone()));
        self.harnesses
            .lock()
            .entry(metadata.id().to_owned())
            .or_default()
            .push(Arc::clone(&harness));
        Ok(Arc::new(HarnessHandle(harness)))
    }
}
