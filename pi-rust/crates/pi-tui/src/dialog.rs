//! Modal dialogs for extension UI requests.
//!
//! `ctx.ui.confirm / input / select` inside a JS extension travels the
//! `pi-extensions` host as a [`UiRequest`] and waits for a
//! [`UiResponse`]. In interactive mode the answer is a real key press,
//! so the request is handed to the [`App`](crate::App) as a [`Dialog`]:
//! the App renders it, routes keys to it while it is open, and resolves
//! it by sending the response back over the oneshot the host is
//! awaiting.
//!
//! Semantics mirror the upstream `ExtensionUIContext` escape hatches:
//!
//! | request | accept | deny / cancel |
//! |---------|--------|---------------|
//! | `Confirm` | `Enter` / `y` | `n` / `Esc` / `Ctrl+C` |
//! | `Input` | `Enter` (may be empty) | `Esc` / `Ctrl+C` |
//! | `Select` | `Enter` on the highlighted item | `Esc` / `Ctrl+C` |
//!
//! A cancelled `Confirm` still answers
//! `UiResponse::Confirm { accepted: false }`; cancelled `Input` /
//! `Select` answer `None`, which serialises to `null` on the JS side —
//! exactly what the shim returns today when no UI is available.

use pi_protocol::{UiLevel, UiRequest, UiResponse};
use tokio::sync::oneshot;

use crate::input::{Key, KeyCode};
use crate::prompt::{Prompt, PromptAction};
use crate::selector::{Selector, SelectorAction, SelectorItem};

/// Which flavour of dialog is on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogKind {
    /// One-shot notification.
    Notify,
    /// Yes/no confirmation.
    Confirm,
    /// Free-form text input.
    Input,
    /// Single-select list.
    Select,
}

/// What [`Dialog::handle_key`] decided.
#[derive(Debug, Clone, PartialEq)]
pub enum DialogAction {
    /// Key was not meaningful for this dialog.
    None,
    /// Dialog state changed (cursor / buffer) — caller redraws.
    Changed,
    /// Dialog answered. The caller drops the dialog; the host has
    /// already been notified through the reply channel.
    Resolved(Option<UiResponse>),
}

/// A pending UI request plus the channel used to answer it.
///
/// Constructed by the interactive UI bridge (see
/// `pi-coding-agent/src/extensions/ui_bridge.rs`) from the envelope the
/// `pi-extensions` host sends on every `ctx.ui.*` call.
#[derive(Debug)]
pub struct Dialog {
    request: UiRequest,
    /// `None` once the dialog has been answered.
    reply: Option<oneshot::Sender<Option<UiResponse>>>,
    /// Editor used by [`DialogKind::Input`] (its cursor / kill-line /
    /// history navigation come for free).
    input: Prompt,
    /// List used by [`DialogKind::Select`].
    selector: Selector,
    resolved: Option<Option<UiResponse>>,
}

impl Dialog {
    /// Wrap a UI request and the channel its response must travel on.
    ///
    /// Only `Confirm` / `Input` / `Select` become visible dialogs;
    /// `Notify` is fire-and-forget and should be rendered as a
    /// message instead (see the interactive UI bridge).
    pub fn new(request: UiRequest, reply: oneshot::Sender<Option<UiResponse>>) -> Self {
        let input = match &request {
            UiRequest::Input { placeholder, .. } => {
                let mut prompt = Prompt::new("> ");
                if let Some(placeholder) = placeholder {
                    prompt.set_placeholder(placeholder.clone());
                }
                prompt
            }
            _ => Prompt::new("> "),
        };
        let selector = match &request {
            UiRequest::Select { title, options } => Selector::new(
                format!("Select: {title}"),
                options
                    .iter()
                    .map(|option| SelectorItem::new(option.clone(), option.clone()))
                    .collect(),
            ),
            _ => Selector::new("Select", Vec::new()),
        };
        Self {
            request,
            reply: Some(reply),
            input,
            selector,
            resolved: None,
        }
    }

    /// The originating request.
    pub fn request(&self) -> &UiRequest {
        &self.request
    }

    /// Title shown in the dialog header.
    pub fn title(&self) -> &str {
        match &self.request {
            UiRequest::Notify { .. } => "Notify",
            UiRequest::Confirm { title, .. }
            | UiRequest::Input { title, .. }
            | UiRequest::Select { title, .. } => title,
        }
    }

    /// Dialog flavour.
    pub fn kind(&self) -> DialogKind {
        match &self.request {
            UiRequest::Notify { .. } => DialogKind::Notify,
            UiRequest::Confirm { .. } => DialogKind::Confirm,
            UiRequest::Input { .. } => DialogKind::Input,
            UiRequest::Select { .. } => DialogKind::Select,
        }
    }

    /// Severity, for `Notify` (the App renders the level icon).
    pub fn level(&self) -> UiLevel {
        match &self.request {
            UiRequest::Notify { level, .. } => *level,
            _ => UiLevel::Info,
        }
    }

    /// `(message, level)` for a `Notify` request; `None` otherwise.
    pub fn notify_text(&self) -> Option<(&str, UiLevel)> {
        match &self.request {
            UiRequest::Notify { message, level } => Some((message.as_str(), *level)),
            _ => None,
        }
    }

    /// Current input buffer (empty for non-input dialogs). Chip sentinels
    /// are expanded, matching what the dialog renders.
    pub fn input_text(&self) -> String {
        self.input.text()
    }

    /// Cursor of the select list (0 for non-select dialogs).
    pub fn select_cursor(&self) -> usize {
        self.selector.cursor()
    }

    /// Whether the dialog has already been answered.
    pub fn is_resolved(&self) -> bool {
        self.resolved.is_some()
    }

    /// The response that was sent, if any.
    pub fn response(&self) -> Option<&UiResponse> {
        self.resolved.as_ref().and_then(|r| r.as_ref())
    }

    /// Whether the host has stopped waiting for this dialog (its
    /// timeout fired or the JS promise was dropped). The App uses this
    /// to close dialogs nobody is listening to any more.
    pub fn is_abandoned(&self) -> bool {
        self.reply.as_ref().map(|tx| tx.is_closed()).unwrap_or(true)
    }

    /// The response used when the user dismisses this dialog. `Confirm`
    /// denies; `Input` / `Select` cancel (`null` on the JS side).
    pub fn cancel_response(&self) -> Option<UiResponse> {
        match &self.request {
            UiRequest::Confirm { .. } => Some(UiResponse::Confirm { accepted: false }),
            _ => None,
        }
    }

    /// Resolve with the cancellation semantics for this dialog.
    pub fn cancel(&mut self) -> DialogAction {
        let response = self.cancel_response();
        self.resolve(response)
    }

    /// Answer the dialog, handing `response` to the waiting host.
    ///
    /// Idempotent: a second call is a no-op returning the first answer.
    pub fn resolve(&mut self, response: Option<UiResponse>) -> DialogAction {
        if self.resolved.is_some() {
            return DialogAction::Resolved(self.resolved.clone().flatten());
        }
        if let Some(tx) = self.reply.take() {
            // The receiver is gone when the host already gave up; the
            // dialog is still reported as resolved so the App closes it.
            let _ = tx.send(response.clone());
        }
        self.resolved = Some(response.clone());
        DialogAction::Resolved(response)
    }

    /// Process a key while this dialog is open.
    pub fn handle_key(&mut self, key: Key) -> DialogAction {
        if self.resolved.is_some() {
            return DialogAction::None;
        }
        // Ctrl+C is "cancel" for every dialog; unlike the prompt it
        // must never fall through to "exit the app" while a modal is
        // waiting for an answer.
        if key.code == KeyCode::Char('c') && key.modifiers.control {
            return self.cancel();
        }
        match self.kind() {
            DialogKind::Notify => match key.code {
                KeyCode::Enter | KeyCode::Esc | KeyCode::Char(' ') => {
                    self.resolve(Some(UiResponse::NotifyAck))
                }
                _ => DialogAction::None,
            },
            DialogKind::Confirm => match key.code {
                KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                    self.resolve(Some(UiResponse::Confirm { accepted: true }))
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => self.cancel(),
                _ => DialogAction::None,
            },
            DialogKind::Input => match self.input.handle_key(key) {
                PromptAction::Submit(value) => self.resolve(Some(UiResponse::Input { value })),
                PromptAction::Changed => DialogAction::Changed,
                PromptAction::Interrupt | PromptAction::Eof => self.cancel(),
                PromptAction::None => match key.code {
                    // `Esc` is not a prompt action but is the dialog's
                    // cancel key.
                    KeyCode::Esc => self.cancel(),
                    _ => DialogAction::None,
                },
            },
            DialogKind::Select => match self.selector.handle_key(key) {
                SelectorAction::Selected(value) => self.resolve(Some(UiResponse::Select { value })),
                SelectorAction::Cancelled => self.cancel(),
                SelectorAction::Changed => DialogAction::Changed,
                SelectorAction::None => DialogAction::None,
            },
        }
    }

    /// Render the dialog as flat lines, top to bottom.
    pub fn render_lines(&self, width: u16) -> Vec<String> {
        let width = width as usize;
        let mut lines = Vec::new();
        match &self.request {
            UiRequest::Confirm { title, body } => {
                lines.push(format!("Confirm: {title}"));
                lines.push("─".repeat(width.min(40)));
                lines.extend(wrap(body, width));
                lines.push(String::new());
                lines.push("[Enter/y] accept    [n/Esc] deny".to_string());
            }
            UiRequest::Input { title, .. } => {
                lines.push(format!("Input: {title}"));
                lines.push("─".repeat(width.min(40)));
                if !self.input.placeholder().is_empty() {
                    lines.push(format!("({})", self.input.placeholder()));
                }
                lines.push(self.input.render_line(width as u16));
                lines.push(String::new());
                lines.push("[Enter] submit    [Esc] cancel".to_string());
            }
            UiRequest::Select { .. } => {
                lines.extend(self.selector.render_lines(width as u16));
                lines.push(String::new());
                lines.push("[Enter] choose    [↑/↓] move    [Esc] cancel".to_string());
            }
            UiRequest::Notify { message, level } => {
                lines.push(format!("[{level:?}] {message}"));
            }
        }
        lines
    }
}

/// Greedy word wrap so a long confirmation body stays inside the
/// dialog. Newlines in the input are honoured as hard breaks.
fn wrap(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    for paragraph in text.split('\n') {
        let mut line = String::new();
        for word in paragraph.split_whitespace() {
            let word_len = word.chars().count();
            if line.is_empty() {
                line.push_str(word);
            } else if line.chars().count() + 1 + word_len <= width {
                line.push(' ');
                line.push_str(word);
            } else {
                out.push(std::mem::take(&mut line));
                line.push_str(word);
            }
        }
        out.push(line);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::KeyModifiers;

    fn confirm_dialog() -> (Dialog, oneshot::Receiver<Option<UiResponse>>) {
        let (tx, rx) = oneshot::channel();
        let dialog = Dialog::new(
            UiRequest::Confirm {
                title: "Delete files?".into(),
                body: "This cannot be undone.".into(),
            },
            tx,
        );
        (dialog, rx)
    }

    fn input_dialog() -> (Dialog, oneshot::Receiver<Option<UiResponse>>) {
        let (tx, rx) = oneshot::channel();
        let dialog = Dialog::new(
            UiRequest::Input {
                title: "Branch name".into(),
                placeholder: Some("feature/x".into()),
            },
            tx,
        );
        (dialog, rx)
    }

    fn select_dialog() -> (Dialog, oneshot::Receiver<Option<UiResponse>>) {
        let (tx, rx) = oneshot::channel();
        let dialog = Dialog::new(
            UiRequest::Select {
                title: "Pick a model".into(),
                options: vec!["opus".into(), "sonnet".into(), "haiku".into()],
            },
            tx,
        );
        (dialog, rx)
    }

    fn key(code: KeyCode) -> Key {
        Key::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn confirm_enter_accepts() {
        let (mut dialog, mut rx) = confirm_dialog();
        assert_eq!(dialog.kind(), DialogKind::Confirm);
        assert_eq!(
            dialog.handle_key(key(KeyCode::Enter)),
            DialogAction::Resolved(Some(UiResponse::Confirm { accepted: true })),
        );
        assert!(dialog.is_resolved());
        assert_eq!(
            rx.try_recv(),
            Ok(Some(UiResponse::Confirm { accepted: true }))
        );
    }

    #[test]
    fn confirm_y_accepts_and_n_denies() {
        let (mut y_dialog, mut y_rx) = confirm_dialog();
        y_dialog.handle_key(key(KeyCode::Char('y')));
        assert_eq!(
            y_rx.try_recv(),
            Ok(Some(UiResponse::Confirm { accepted: true }))
        );

        let (mut n_dialog, mut n_rx) = confirm_dialog();
        n_dialog.handle_key(key(KeyCode::Char('n')));
        assert_eq!(
            n_rx.try_recv(),
            Ok(Some(UiResponse::Confirm { accepted: false }))
        );
    }

    #[test]
    fn confirm_esc_and_ctrl_c_deny() {
        let (mut esc_dialog, mut esc_rx) = confirm_dialog();
        esc_dialog.handle_key(key(KeyCode::Esc));
        assert_eq!(
            esc_rx.try_recv(),
            Ok(Some(UiResponse::Confirm { accepted: false }))
        );

        let (mut cc_dialog, mut cc_rx) = confirm_dialog();
        cc_dialog.handle_key(Key::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert_eq!(
            cc_rx.try_recv(),
            Ok(Some(UiResponse::Confirm { accepted: false }))
        );
    }

    #[test]
    fn confirm_ignores_unrelated_keys() {
        let (mut dialog, _rx) = confirm_dialog();
        assert_eq!(
            dialog.handle_key(key(KeyCode::Char('z'))),
            DialogAction::None
        );
        assert!(!dialog.is_resolved());
    }

    #[test]
    fn input_collects_text_then_submits() {
        let (mut dialog, mut rx) = input_dialog();
        assert_eq!(dialog.kind(), DialogKind::Input);
        for ch in "feature/pi".chars() {
            dialog.handle_key(key(KeyCode::Char(ch)));
        }
        assert_eq!(dialog.input_text(), "feature/pi");
        assert_eq!(
            dialog.handle_key(key(KeyCode::Enter)),
            DialogAction::Resolved(Some(UiResponse::Input {
                value: "feature/pi".into()
            })),
        );
        assert_eq!(
            rx.try_recv(),
            Ok(Some(UiResponse::Input {
                value: "feature/pi".into()
            }))
        );
    }

    #[test]
    fn input_cancel_answers_none() {
        for cancel in [
            key(KeyCode::Esc),
            Key::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        ] {
            let (mut dialog, mut rx) = input_dialog();
            dialog.handle_key(key(KeyCode::Char('x')));
            assert_eq!(dialog.handle_key(cancel), DialogAction::Resolved(None));
            // `None` is what the JS shim turns into `null`.
            assert_eq!(rx.try_recv(), Ok(None));
        }
    }

    #[test]
    fn select_enter_returns_highlighted_option() {
        let (mut dialog, mut rx) = select_dialog();
        assert_eq!(dialog.kind(), DialogKind::Select);
        dialog.handle_key(key(KeyCode::Down));
        assert_eq!(dialog.select_cursor(), 1);
        assert_eq!(
            dialog.handle_key(key(KeyCode::Enter)),
            DialogAction::Resolved(Some(UiResponse::Select {
                value: "sonnet".into()
            })),
        );
        assert_eq!(
            rx.try_recv(),
            Ok(Some(UiResponse::Select {
                value: "sonnet".into()
            }))
        );
    }

    #[test]
    fn select_esc_cancels() {
        let (mut dialog, mut rx) = select_dialog();
        assert_eq!(
            dialog.handle_key(key(KeyCode::Esc)),
            DialogAction::Resolved(None)
        );
        assert_eq!(rx.try_recv(), Ok(None));
    }

    #[test]
    fn resolving_twice_keeps_the_first_answer() {
        let (mut dialog, mut rx) = confirm_dialog();
        dialog.handle_key(key(KeyCode::Enter));
        // A resolved dialog ignores every further key…
        assert_eq!(
            dialog.handle_key(key(KeyCode::Char('n'))),
            DialogAction::None
        );
        // …and the host only ever sees the first answer.
        assert_eq!(
            rx.try_recv(),
            Ok(Some(UiResponse::Confirm { accepted: true }))
        );
        assert_eq!(rx.try_recv(), Err(oneshot::error::TryRecvError::Closed));
    }

    #[test]
    fn abandoned_when_the_host_stops_waiting() {
        let (tx, rx) = oneshot::channel::<Option<UiResponse>>();
        let dialog = Dialog::new(
            UiRequest::Confirm {
                title: "t".into(),
                body: "b".into(),
            },
            tx,
        );
        assert!(!dialog.is_abandoned());
        drop(rx);
        assert!(dialog.is_abandoned());
    }

    #[test]
    fn renders_header_body_and_hints() {
        let (dialog, _rx) = confirm_dialog();
        let lines = dialog.render_lines(40);
        assert_eq!(lines[0], "Confirm: Delete files?");
        assert_eq!(lines[2], "This cannot be undone.");
        assert!(lines.last().unwrap().contains("[Enter/y] accept"));
    }

    #[test]
    fn wraps_long_body_text() {
        let (tx, _rx) = oneshot::channel();
        let dialog = Dialog::new(
            UiRequest::Confirm {
                title: "t".into(),
                body: "one two three four five six".into(),
            },
            tx,
        );
        let lines = dialog.render_lines(10);
        // header, divider, then one wrapped body line per pair of words.
        assert_eq!(&lines[2..5], &["one two", "three four", "five six"]);
    }
}
