//! Interactive extension UI bridge — `ctx.ui.*` onto real TUI dialogs.
//!
//! The `pi-extensions` host turns a JS `ctx.ui.confirm / input / select`
//! into a [`UiRequest`] and *awaits* the matching [`UiResponse`]. In
//! interactive mode the answer is a key press, so this module is the
//! join point between the two halves of the process:
//!
//! - [`TuiUiBridge`] implements [`UiHandler`]:
//!   it wraps every request in a [`Dialog`] and sends it to the TUI,
//!   then awaits the oneshot the App resolves it with.
//! - the receiver half goes to [`App::attach_ui_dialogs`]; the render
//!   loop calls [`App::poll_ui_dialogs`] once per tick, so an incoming
//!   request becomes a modal on the next frame.
//!
//! ## Why the `ready` gate
//!
//! [`wiring::load`](crate::extensions::wiring::load) dispatches
//! `session_start` *before* the TUI exists — no event loop is pumping
//! dialogs yet, and a modal sent at that point would sit in the channel
//! until the host timeout fired. The gate starts closed: until
//! [`TuiUiBridge::arm`] is called (the interactive loop is about to
//! start) every request is forwarded to the [`StderrUiHandler`]
//! fallback, which denies / cancels immediately while still surfacing
//! notifications. `disarm` puts the gate back when the loop exits, so a
//! fallback to text mode cannot leave an extension waiting.
//!
//! [`UiRequest`]: pi_protocol::UiRequest
//! [`UiResponse`]: pi_protocol::UiResponse
//! [`UiHandler`]: pi_extensions::UiHandler
//! [`App::attach_ui_dialogs`]: pi_tui::App::attach_ui_dialogs
//! [`App::poll_ui_dialogs`]: pi_tui::App::poll_ui_dialogs

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use pi_extensions::UiHandler;
use pi_protocol::{UiLevel, UiRequest, UiResponse};
use pi_tui::dialog::Dialog;
use tokio::sync::{mpsc, oneshot};

use crate::extensions::wiring::StderrUiHandler;

/// Receiver half of the bridge — the App drains it every tick.
pub type TuiUiReceiver = mpsc::UnboundedReceiver<Dialog>;

/// Sender half of the bridge.
///
/// Cheap to clone: wiring keeps one clone to install the handler and
/// the interactive loop keeps another to arm / disarm the gate.
#[derive(Clone)]
pub struct TuiUiBridge {
    tx: mpsc::UnboundedSender<Dialog>,
    ready: Arc<AtomicBool>,
    handler: Arc<dyn UiHandler>,
}

impl std::fmt::Debug for TuiUiBridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TuiUiBridge")
            .field("armed", &self.is_armed())
            .field("closed", &self.tx.is_closed())
            .finish()
    }
}

impl TuiUiBridge {
    /// Create a bridge and the receiver its dialogs travel on.
    pub fn channel() -> (Self, TuiUiReceiver) {
        let (tx, rx) = mpsc::unbounded_channel();
        let ready = Arc::new(AtomicBool::new(false));
        let handler: Arc<dyn UiHandler> = Arc::new(TuiUiHandler {
            tx: tx.clone(),
            ready: ready.clone(),
            fallback: StderrUiHandler,
        });
        (Self { tx, ready, handler }, rx)
    }

    /// The handler to install on the extension host.
    pub fn handler(&self) -> Arc<dyn UiHandler> {
        self.handler.clone()
    }

    /// Open the gate: dialogs now reach the TUI loop.
    pub fn arm(&self) {
        self.ready.store(true, Ordering::SeqCst);
    }

    /// Close the gate: requests deny via the stderr fallback again.
    pub fn disarm(&self) {
        self.ready.store(false, Ordering::SeqCst);
    }

    /// Whether the gate is open.
    pub fn is_armed(&self) -> bool {
        self.ready.load(Ordering::SeqCst)
    }
}

/// Both halves of an interactive UI bridge.
///
/// `main` builds one, hands [`TuiUi::bridge`] to the extension loader
/// and moves the value into [`InteractiveOptions`]
/// (crate::interactive::InteractiveOptions), whose loop attaches
/// [`TuiUi::take_dialogs`] to the App and arms the gate.
#[derive(Debug)]
pub struct TuiUi {
    bridge: TuiUiBridge,
    dialogs: Option<TuiUiReceiver>,
}

impl TuiUi {
    /// Create the pair.
    pub fn new() -> Self {
        let (bridge, dialogs) = TuiUiBridge::channel();
        Self {
            bridge,
            dialogs: Some(dialogs),
        }
    }

    /// The handler-bearing half.
    pub fn bridge(&self) -> &TuiUiBridge {
        &self.bridge
    }

    /// Take the receiver (once) to hand it to the App.
    pub fn take_dialogs(&mut self) -> Option<TuiUiReceiver> {
        self.dialogs.take()
    }

    /// Open the gate (see [`TuiUiBridge::arm`]).
    pub fn arm(&self) {
        self.bridge.arm();
    }

    /// Close the gate (see [`TuiUiBridge::disarm`]).
    pub fn disarm(&self) {
        self.bridge.disarm();
    }
}

impl Default for TuiUi {
    fn default() -> Self {
        Self::new()
    }
}

/// [`UiHandler`] that forwards extension UI requests to the TUI App.
struct TuiUiHandler {
    tx: mpsc::UnboundedSender<Dialog>,
    ready: Arc<AtomicBool>,
    /// Used while the TUI is not pumping dialogs (load-time
    /// `session_start`, after the loop exited).
    fallback: StderrUiHandler,
}

impl TuiUiHandler {
    /// Send one request to the App and wait for the user.
    ///
    /// Returns `None` when the App is gone or answers "cancelled" —
    /// the same value the non-interactive handler produces, so a
    /// dropped TUI can only ever deny a prompt.
    async fn ask(&self, request: UiRequest) -> Option<UiResponse> {
        let (tx, rx) = oneshot::channel();
        if self.tx.send(Dialog::new(request, tx)).is_err() {
            return None;
        }
        rx.await.unwrap_or(None)
    }
}

#[async_trait]
impl UiHandler for TuiUiHandler {
    async fn confirm(&self, title: &str, body: &str) -> bool {
        if !self.ready.load(Ordering::SeqCst) {
            return self.fallback.confirm(title, body).await;
        }
        match self
            .ask(UiRequest::Confirm {
                title: title.to_string(),
                body: body.to_string(),
            })
            .await
        {
            Some(UiResponse::Confirm { accepted }) => accepted,
            _ => false,
        }
    }

    async fn input(&self, title: &str, placeholder: Option<&str>) -> Option<String> {
        if !self.ready.load(Ordering::SeqCst) {
            return self.fallback.input(title, placeholder).await;
        }
        match self
            .ask(UiRequest::Input {
                title: title.to_string(),
                placeholder: placeholder.map(str::to_string),
            })
            .await
        {
            Some(UiResponse::Input { value }) => Some(value),
            _ => None,
        }
    }

    async fn select(&self, title: &str, options: &[String]) -> Option<String> {
        if !self.ready.load(Ordering::SeqCst) {
            return self.fallback.select(title, options).await;
        }
        match self
            .ask(UiRequest::Select {
                title: title.to_string(),
                options: options.to_vec(),
            })
            .await
        {
            Some(UiResponse::Select { value }) => Some(value),
            _ => None,
        }
    }

    async fn notify(&self, message: &str, level: UiLevel) {
        if !self.ready.load(Ordering::SeqCst) {
            return self.fallback.notify(message, level).await;
        }
        // Fire-and-forget: the App turns this into a transcript line and
        // the (unread) reply channel is dropped immediately.
        let (tx, _rx) = oneshot::channel();
        let _ = self.tx.send(Dialog::new(
            UiRequest::Notify {
                message: message.to_string(),
                level,
            },
            tx,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unarmed_bridge_denies_without_touching_the_channel() {
        let (bridge, mut rx) = TuiUiBridge::channel();
        let handler = bridge.handler();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            assert!(!handler.confirm("t", "b").await);
            assert_eq!(handler.input("t", None).await, None);
            assert_eq!(handler.select("t", &["a".to_string()]).await, None);
        });
        assert!(
            rx.try_recv().is_err(),
            "an unarmed bridge must not queue dialogs"
        );
    }

    #[test]
    fn armed_bridge_forwards_and_resolves() {
        let (bridge, mut rx) = TuiUiBridge::channel();
        let handler = bridge.handler();
        bridge.arm();
        assert!(bridge.is_armed());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let ask = tokio::spawn(async move { handler.confirm("Delete?", "sure?").await });
            let mut dialog = rx.recv().await.expect("dialog");
            assert_eq!(dialog.title(), "Delete?");
            dialog.resolve(Some(UiResponse::Confirm { accepted: true }));
            assert!(ask.await.expect("join"));
        });
        bridge.disarm();
        assert!(!bridge.is_armed());
    }
}
