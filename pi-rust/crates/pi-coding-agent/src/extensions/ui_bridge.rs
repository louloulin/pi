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
use std::sync::{Arc, Weak};

use async_trait::async_trait;
use parking_lot::Mutex;
use pi_extensions::{
    JsComponent, UiCustomAnchor, UiCustomOptions, UiHandler, UiRegionHost, UiWidgetPlacement,
};
use pi_protocol::{UiLevel, UiPromptKind, UiRequest, UiResponse};
use pi_tui::dialog::Dialog;
use pi_tui::input::KeyCode;
use pi_tui::styled::{SpanStyle, StyledLine, StyledSpan};
use pi_tui::{App, Component, CustomHandle, CustomOptions, Key, OverlayAnchor, WidgetPlacement};
use tokio::sync::{mpsc, oneshot};

use crate::extensions::lifecycle::UiPromptObserver;
use crate::extensions::wiring::StderrUiHandler;

/// Receiver half of the bridge — the App drains it every tick.
pub type TuiUiReceiver = mpsc::UnboundedReceiver<Dialog>;

/// Slot holding the observer that is told when a blocking prompt opens and
/// closes.
///
/// The bridge is built before the extension runtime (the host needs the
/// handler, the runtime needs the host), so the observer is installed
/// afterwards — before the interactive loop can show a prompt. A `Weak` keeps
/// the runtime-to-handler-to-observer chain from forming a cycle.
pub type PromptObserverSlot = Arc<parking_lot::Mutex<Option<Weak<dyn UiPromptObserver>>>>;

/// Sender half of the bridge.
///
/// Cheap to clone: wiring keeps one clone to install the handler and
/// the interactive loop keeps another to arm / disarm the gate.
#[derive(Clone)]
pub struct TuiUiBridge {
    tx: mpsc::UnboundedSender<Dialog>,
    ready: Arc<AtomicBool>,
    handler: Arc<dyn UiHandler>,
    prompt_observer: PromptObserverSlot,
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
        let prompt_observer: PromptObserverSlot = Arc::new(parking_lot::Mutex::new(None));
        let handler: Arc<dyn UiHandler> = Arc::new(TuiUiHandler {
            tx: tx.clone(),
            ready: ready.clone(),
            fallback: StderrUiHandler,
            prompt_observer: prompt_observer.clone(),
        });
        (
            Self {
                tx,
                ready,
                handler,
                prompt_observer,
            },
            rx,
        )
    }

    /// Install the observer told about `ui_prompt_start` / `ui_prompt_end`.
    ///
    /// Called once the extension runtime exists. Passing a `Weak` means a
    /// dropped runtime silently stops the notifications instead of keeping it
    /// alive through the UI handler.
    pub fn set_prompt_observer(&self, observer: Weak<dyn UiPromptObserver>) {
        *self.prompt_observer.lock() = Some(observer);
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
    region_host: Arc<TuiRegionHost>,
    regions: Option<TuiRegionReceiver>,
}

impl TuiUi {
    /// Create the pair.
    pub fn new() -> Self {
        let (bridge, dialogs) = TuiUiBridge::channel();
        let (region_host, regions) = TuiRegionHost::channel();
        Self {
            bridge,
            dialogs: Some(dialogs),
            region_host: Arc::new(region_host),
            regions: Some(regions),
        }
    }

    /// The handler-bearing half.
    pub fn bridge(&self) -> &TuiUiBridge {
        &self.bridge
    }

    /// The region host to install through
    /// [`ExtensionLoadOptions::ui_region_host`](crate::extensions::wiring::ExtensionLoadOptions::ui_region_host).
    pub fn region_host(&self) -> Arc<TuiRegionHost> {
        self.region_host.clone()
    }

    /// Take the receiver (once) to hand it to the App.
    pub fn take_dialogs(&mut self) -> Option<TuiUiReceiver> {
        self.dialogs.take()
    }

    /// Take the region receiver + its sender (once) to hand to the
    /// interactive loop's [`RegionPump`].
    pub fn take_regions(&mut self) -> Option<(TuiRegionReceiver, mpsc::UnboundedSender<RegionOp>)> {
        self.regions
            .take()
            .map(|receiver| (receiver, self.region_host.sender()))
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
    /// Told when a blocking prompt opens / closes (upstream
    /// `ui_prompt_start` / `ui_prompt_end`). Empty until the extension
    /// runtime exists, and `Weak` so it cannot pin the runtime.
    prompt_observer: PromptObserverSlot,
}

impl TuiUiHandler {
    /// Forward a prompt edge to the observer, when one is installed.
    async fn notify_prompt(&self, kind: UiPromptKind, title: Option<&str>, started: bool) {
        let observer = self.prompt_observer.lock().as_ref().and_then(Weak::upgrade);
        let Some(observer) = observer else {
            return;
        };
        if started {
            observer.prompt_started(kind, title).await;
        } else {
            observer.prompt_finished(kind, title).await;
        }
    }

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
        self.notify_prompt(UiPromptKind::Confirm, Some(title), true)
            .await;
        let answer = match self
            .ask(UiRequest::Confirm {
                title: title.to_string(),
                body: body.to_string(),
            })
            .await
        {
            Some(UiResponse::Confirm { accepted }) => accepted,
            _ => false,
        };
        self.notify_prompt(UiPromptKind::Confirm, Some(title), false)
            .await;
        answer
    }

    async fn input(&self, title: &str, placeholder: Option<&str>) -> Option<String> {
        if !self.ready.load(Ordering::SeqCst) {
            return self.fallback.input(title, placeholder).await;
        }
        self.notify_prompt(UiPromptKind::Input, Some(title), true)
            .await;
        let answer = match self
            .ask(UiRequest::Input {
                title: title.to_string(),
                placeholder: placeholder.map(str::to_string),
            })
            .await
        {
            Some(UiResponse::Input { value }) => Some(value),
            _ => None,
        };
        self.notify_prompt(UiPromptKind::Input, Some(title), false)
            .await;
        answer
    }

    async fn select(&self, title: &str, options: &[String]) -> Option<String> {
        if !self.ready.load(Ordering::SeqCst) {
            return self.fallback.select(title, options).await;
        }
        self.notify_prompt(UiPromptKind::Select, Some(title), true)
            .await;
        let answer = match self
            .ask(UiRequest::Select {
                title: title.to_string(),
                options: options.to_vec(),
            })
            .await
        {
            Some(UiResponse::Select { value }) => Some(value),
            _ => None,
        };
        self.notify_prompt(UiPromptKind::Select, Some(title), false)
            .await;
        answer
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

// ---------------------------------------------------------------------------
// Region / overlay bridge
//
// `ctx.ui.setWidget / setHeader / setFooter / setEditorComponent / setStatus /
// custom` are *not* request/response like a dialog: the extension mutates a region
// and moves on. So they travel the other way — the extension host pushes a
// [`RegionOp`] into an unbounded channel, and the interactive loop applies
// it to the [`App`] once per tick ([`RegionPump::pump`]). Rendering is the
// loop's job, not the extension's: this module only adapts a JS component
// onto [`pi_tui::Component`] and keeps its cached lines fresh.
//
// The App routes keyboard input internally and [`pi_tui::Component`] is
// synchronous, so a proxy cannot round-trip to QuickJS inside
// `handle_input`. Instead the key is queued on the proxy and delivered to
// the JS `handleInput(data)` on the next pump; the proxy answers "consumed"
// whenever the component declares a handler at all (upstream leaves the
// decision to the focus model, which this port does not have).

/// One region mutation on its way from the extension host to the App.
#[derive(Debug)]
pub enum RegionOp {
    /// Install (`Some`) or clear (`None`) the widget registered under `key`.
    Widget {
        /// Extension-chosen widget key.
        key: String,
        /// Where the widget sits relative to the editor region.
        placement: WidgetPlacement,
        /// The JS component, or `None` to clear the slot.
        component: Option<JsComponent>,
    },
    /// Install (`Some`) or clear (`None`) the header.
    Header(Option<JsComponent>),
    /// Install (`Some`) or clear (`None`) the footer.
    Footer(Option<JsComponent>),
    /// Install (`Some`) or clear (`None`) the editor region.
    Editor(Option<JsComponent>),
    /// Install (`Some`) or clear (`None`) one extension status text —
    /// `ctx.ui.setStatus(key, text)`, drawn as the footer's third row.
    Status {
        /// Extension-chosen status key.
        key: String,
        /// Status text, or `None` for upstream's `undefined` clear.
        text: Option<String>,
    },
    /// Set the terminal window/tab title — `ctx.ui.setTitle(title)`.
    ///
    /// A different sink from every other region op: the App holds the value
    /// and the interactive loop writes the OSC 0 sequence to the tty
    /// (`App::take_terminal_title`), because the sequence must not enter the
    /// frame buffer.
    Title(String),
    /// Open a `ctx.ui.custom` session.
    OpenCustom {
        /// Session token the shim allocated for the `custom()` call.
        session: u64,
        /// The factory's root component.
        component: JsComponent,
        /// Overlay geometry the factory asked for.
        options: UiCustomOptions,
    },
    /// Close a `custom` session with the factory's `done()` value.
    CloseCustom {
        /// Session token to close.
        session: u64,
        /// Value the factory passed to `done()`.
        result: Option<String>,
    },
    /// Show / hide an open `custom` session.
    SetCustomVisible {
        /// Session token to toggle.
        session: u64,
        /// New visibility.
        visible: bool,
    },
    /// The App dropped a component; dispose its QuickJS counterpart.
    Dispose(JsComponent),
}

/// Receiver half of the region bridge: everything the loop has to apply.
pub type TuiRegionReceiver = mpsc::UnboundedReceiver<RegionOp>;

/// [`UiRegionHost`] that forwards region mutations to the interactive loop.
///
/// A [`crate::extensions::wiring::load`] pass installs this on
/// [`HostOptions::ui_region_host`](pi_extensions::HostOptions::ui_region_host)
/// so the JS shim's `ctx.ui.setWidget` (and friends) reach real TUI state.
/// Without it the host denies those methods, which keeps
/// `ctx.hasUI == false` runs honest.
#[derive(Debug, Clone)]
pub struct TuiRegionHost {
    tx: mpsc::UnboundedSender<RegionOp>,
}

impl TuiRegionHost {
    /// Create the host and the receiver the interactive loop drains.
    pub fn channel() -> (Self, TuiRegionReceiver) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Self { tx }, rx)
    }

    /// The sender half, shared with the component proxies so a disposal
    /// triggered by the App reaches the pump.
    pub fn sender(&self) -> mpsc::UnboundedSender<RegionOp> {
        self.tx.clone()
    }
}

#[async_trait]
impl UiRegionHost for TuiRegionHost {
    async fn set_widget(
        &self,
        key: String,
        placement: UiWidgetPlacement,
        component: Option<JsComponent>,
    ) {
        let _ = self.tx.send(RegionOp::Widget {
            key,
            placement: widget_placement(placement),
            component,
        });
    }

    async fn set_header(&self, component: Option<JsComponent>) {
        let _ = self.tx.send(RegionOp::Header(component));
    }

    async fn set_footer(&self, component: Option<JsComponent>) {
        let _ = self.tx.send(RegionOp::Footer(component));
    }

    async fn set_editor_component(&self, component: Option<JsComponent>) {
        let _ = self.tx.send(RegionOp::Editor(component));
    }

    async fn set_status(&self, key: String, text: Option<String>) {
        let _ = self.tx.send(RegionOp::Status { key, text });
    }

    async fn set_title(&self, title: String) {
        let _ = self.tx.send(RegionOp::Title(title));
    }

    async fn open_custom(&self, session: u64, component: JsComponent, options: UiCustomOptions) {
        let _ = self.tx.send(RegionOp::OpenCustom {
            session,
            component,
            options,
        });
    }

    async fn close_custom(&self, session: u64, result: Option<String>) {
        let _ = self.tx.send(RegionOp::CloseCustom { session, result });
    }

    async fn set_custom_visible(&self, session: u64, visible: bool) {
        let _ = self
            .tx
            .send(RegionOp::SetCustomVisible { session, visible });
    }
}

/// Map the host's widget placement onto the TUI's.
///
/// Both enums are foreign here — `pi-extensions` owns one, `pi-tui` the
/// other — so the orphan rule rules out a `From` impl and this stays a
/// free function.
fn widget_placement(value: UiWidgetPlacement) -> WidgetPlacement {
    match value {
        UiWidgetPlacement::Above => WidgetPlacement::Above,
        UiWidgetPlacement::Below => WidgetPlacement::Below,
    }
}

/// Translate the host's `custom()` options into the TUI's, collapsing the
/// edge anchors upstream supports onto this port's centre anchor.
fn custom_options(value: UiCustomOptions) -> CustomOptions {
    let anchor = match value.anchor.unwrap_or_default() {
        UiCustomAnchor::TopLeft => OverlayAnchor::TopLeft,
        UiCustomAnchor::TopRight => OverlayAnchor::TopRight,
        UiCustomAnchor::BottomLeft => OverlayAnchor::BottomLeft,
        UiCustomAnchor::BottomRight => OverlayAnchor::BottomRight,
        UiCustomAnchor::Center => OverlayAnchor::Center,
    };
    CustomOptions {
        overlay: value.overlay,
        width: value.width,
        max_height: value.max_height,
        anchor,
        margin: value.margin,
    }
}

/// Shared state between a JS component and its [`Component`] proxy.
struct RegionShared {
    /// The QuickJS handle the proxy renders and forwards keys to.
    component: JsComponent,
    /// Lines from the last render, read by the synchronous
    /// [`Component::render`].
    lines: Mutex<Vec<StyledLine>>,
    /// Keys the App handed to the proxy, delivered on the next pump.
    pending_keys: Mutex<Vec<String>>,
    /// Whether the component declared `handleInput` (learned on the first
    /// render). Until then the proxy declines every key.
    has_input: AtomicBool,
    /// Set once [`Component::dispose`] ran; the pump stops rendering and
    /// drops the entry.
    disposed: AtomicBool,
}

/// [`Component`] adapter around one JS component.
struct RegionProxy {
    shared: Arc<RegionShared>,
    tx: mpsc::UnboundedSender<RegionOp>,
}

impl Component for RegionProxy {
    fn render(&self, _width: u16) -> Vec<StyledLine> {
        self.shared.lines.lock().clone()
    }

    fn handle_input(&mut self, key: Key) -> bool {
        if !self.shared.has_input.load(Ordering::Relaxed) {
            return false;
        }
        self.shared.pending_keys.lock().push(key_to_input_data(key));
        true
    }

    fn dispose(&mut self) {
        if self.shared.disposed.swap(true, Ordering::SeqCst) {
            return;
        }
        // The App disposes a component from synchronous code, so the JS
        // `dispose()` cannot run here; queue it for the next pump.
        let _ = self
            .tx
            .send(RegionOp::Dispose(self.shared.component.clone()));
    }
}

/// An open `ctx.ui.custom` session.
struct CustomSession {
    /// Session token the shim allocated.
    session: u64,
    /// The App's handle, for `setVisible`.
    handle: CustomHandle,
}

/// Drives queued region mutations into the [`App`] and keeps every JS
/// component's cached lines fresh.
///
/// The interactive loop owns one and calls [`RegionPump::pump`] once per
/// tick, before drawing: mutations land first so a `setHeader` in the same
/// tick as the frame shows up in it. Drop the pump when the loop exits —
/// the App's own `Drop` disposes the attached components.
pub struct RegionPump {
    /// Incoming mutations. `None` once the receiver disconnected (the host
    /// was dropped), which makes `pump` a no-op.
    ops: Option<TuiRegionReceiver>,
    /// Sender shared with the proxies, so their queued disposal arrives here.
    tx: mpsc::UnboundedSender<RegionOp>,
    /// Every live proxy's shared state, refreshed each tick.
    live: Vec<Arc<RegionShared>>,
    /// The open `custom` session, if any.
    custom: Option<CustomSession>,
}

impl RegionPump {
    /// Create a pump over one bridge's receiver.
    pub fn new(ops: TuiRegionReceiver, tx: mpsc::UnboundedSender<RegionOp>) -> Self {
        Self {
            ops: Some(ops),
            tx,
            live: Vec::new(),
            custom: None,
        }
    }

    /// Apply every queued mutation, then re-render the live components for
    /// `width` and deliver any keys the App collected since the last tick.
    pub async fn pump(&mut self, app: &mut App, width: u16) {
        while let Some(op) = self.next_op() {
            self.apply(app, op).await;
        }
        self.refresh(width).await;
    }

    /// Whether a `custom` overlay is currently open through this pump.
    pub fn custom_open(&self) -> bool {
        self.custom.is_some()
    }

    fn next_op(&mut self) -> Option<RegionOp> {
        self.ops.as_mut()?.try_recv().ok()
    }

    async fn apply(&mut self, app: &mut App, op: RegionOp) {
        match op {
            RegionOp::Widget {
                key,
                placement,
                component,
            } => app.set_widget(key, self.wrap(component), placement),
            RegionOp::Header(component) => app.set_header(self.wrap(component)),
            RegionOp::Footer(component) => app.set_footer(self.wrap(component)),
            RegionOp::Editor(component) => app.set_editor_component(self.wrap(component)),
            RegionOp::Status { key, text } => {
                app.set_extension_status(&key, text.as_deref());
            }
            RegionOp::Title(title) => app.set_terminal_title(title),
            RegionOp::OpenCustom {
                session,
                component,
                options,
            } => {
                let shared = self.register(component);
                let proxy = self.proxy(shared);
                let handle = app.open_custom(proxy, custom_options(options));
                // `App::open_custom` closes an existing overlay first, so
                // any previous session is already disposed. Replacing the
                // record keeps `setVisible` pointing at the live handle.
                self.custom = Some(CustomSession { session, handle });
            }
            RegionOp::CloseCustom { session, result } => {
                if self.custom.as_ref().is_some_and(|c| c.session == session) {
                    app.close_custom(result);
                    self.custom = None;
                }
            }
            RegionOp::SetCustomVisible { session, visible } => {
                if let Some(custom) = self.custom.as_ref().filter(|c| c.session == session) {
                    custom.handle.set_visible(visible);
                }
            }
            RegionOp::Dispose(component) => component.dispose().await,
        }
    }

    /// Wrap an incoming JS component in a proxy and start tracking it.
    fn wrap(&mut self, component: Option<JsComponent>) -> Option<Box<dyn Component>> {
        component.map(|component| {
            let shared = self.register(component);
            self.proxy(shared)
        })
    }

    fn register(&mut self, component: JsComponent) -> Arc<RegionShared> {
        let shared = Arc::new(RegionShared {
            component,
            lines: Mutex::new(Vec::new()),
            pending_keys: Mutex::new(Vec::new()),
            has_input: AtomicBool::new(false),
            disposed: AtomicBool::new(false),
        });
        self.live.push(shared.clone());
        shared
    }

    fn proxy(&self, shared: Arc<RegionShared>) -> Box<dyn Component> {
        Box::new(RegionProxy {
            shared,
            tx: self.tx.clone(),
        })
    }

    async fn refresh(&mut self, width: u16) {
        let live = std::mem::take(&mut self.live);
        let mut keep = Vec::with_capacity(live.len());
        for shared in live {
            if shared.disposed.load(Ordering::SeqCst) {
                continue;
            }
            let keys = std::mem::take(&mut *shared.pending_keys.lock());
            for data in keys {
                shared.component.handle_input(&data).await;
            }
            let rendered = shared.component.render(width).await;
            shared
                .has_input
                .store(rendered.has_input, Ordering::Relaxed);
            *shared.lines.lock() = rendered
                .lines
                .iter()
                .map(|text| vec![StyledSpan::new(text.clone(), SpanStyle::PLAIN)])
                .collect();
            keep.push(shared);
        }
        self.live = keep;
    }
}

/// Convert a port [`Key`] back into the raw terminal bytes a JS
/// `handleInput(data)` expects.
///
/// The editor side parses terminal input *into* [`Key`]s; extension
/// components (upstream `matchesKey` / `Key.ctrl('c')` helpers) compare
/// against the original escape sequences, so the round trip has to rebuild
/// them.
fn key_to_input_data(key: Key) -> String {
    let alt = key.modifiers.alt;
    let body = match key.code {
        KeyCode::Char(c) => {
            if key.modifiers.control {
                let lower = c.to_ascii_lowercase();
                if lower.is_ascii_lowercase() {
                    char::from_u32(lower as u32 - b'a' as u32 + 1)
                        .unwrap_or(c)
                        .to_string()
                } else if lower == '@' || lower == ' ' {
                    "\0".to_string()
                } else {
                    c.to_string()
                }
            } else {
                c.to_string()
            }
        }
        KeyCode::Enter => "\r".to_string(),
        KeyCode::Tab => "\t".to_string(),
        KeyCode::BackTab => "\x1b[Z".to_string(),
        KeyCode::Backspace => "\x7f".to_string(),
        KeyCode::Esc => "\x1b".to_string(),
        KeyCode::Left => "\x1b[D".to_string(),
        KeyCode::Right => "\x1b[C".to_string(),
        KeyCode::Up => "\x1b[A".to_string(),
        KeyCode::Down => "\x1b[B".to_string(),
        KeyCode::Home => "\x1b[H".to_string(),
        KeyCode::End => "\x1b[F".to_string(),
        KeyCode::PageUp => "\x1b[5~".to_string(),
        KeyCode::PageDown => "\x1b[6~".to_string(),
        KeyCode::Delete => "\x1b[3~".to_string(),
        KeyCode::Insert => "\x1b[2~".to_string(),
        KeyCode::F(n) => match n {
            1 => "\x1bOP".to_string(),
            2 => "\x1bOQ".to_string(),
            3 => "\x1bOR".to_string(),
            4 => "\x1bOS".to_string(),
            5 => "\x1b[15~".to_string(),
            6 => "\x1b[17~".to_string(),
            7 => "\x1b[18~".to_string(),
            8 => "\x1b[19~".to_string(),
            9 => "\x1b[20~".to_string(),
            10 => "\x1b[21~".to_string(),
            11 => "\x1b[23~".to_string(),
            12 => "\x1b[24~".to_string(),
            _ => String::new(),
        },
        KeyCode::Other => String::new(),
    };
    if alt {
        format!("\x1b{body}")
    } else {
        body
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
