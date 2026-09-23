//! Input event abstraction.
//!
//! The TUI components consume a closed [`InputEvent`] enum rather than
//! `crossterm::event::Event` directly. That keeps the components
//! terminal-backend-agnostic, lets unit tests drive them with synthetic
//! events, and gives the [`App`](crate::App) one place to translate
//! raw crossterm input into component-level events.
//!
//! The [`From`] conversions from `crossterm` types live in
//! [`crate::app`]; component tests stay backend-free.
//!
//! # Raw mouse sequences
//!
//! The interactive driver reads events through `crossterm`, which already
//! decodes both mouse encodings a terminal may use once mouse tracking is
//! enabled: the modern SGR form (`ESC [ < b ; x ; y M/m`) and the legacy
//! X10 / "normal" form (`ESC [ M` plus three bytes). [`parse_mouse_sequence`]
//! is the backend-free equivalent of that decoding — the port of upstream's
//! `parseSgrMouseEvent` / `parseWheelEvent` / `isMouseSequence`
//! (`packages/tui/src/tui-alt-screen.ts:939-1000,1613-1616`) — so a host
//! that reads bytes itself (and the tests) gets the same [`InputEvent`]s
//! without a terminal backend, and the X10 path is pinned by tests instead
//! of only being exercised by whatever terminal happened to be attached.
//!
//! # Paste bursts
//!
//! [`PasteBurst`] is the port of codex's `paste_burst`
//! (`codex-rs/tui/src/bottom_pane/paste_burst.rs`): a terminal that does not
//! send bracketed paste (an old emulator, some SSH / multiplexer relays)
//! delivers a paste as a fast run of `KeyCode::Char` / `Enter` / `Tab`
//! events, and this classifier turns such a run back into one paste. Upstream
//! pi-ts has no counterpart — it only ever sees the `paste` event.
//!
//! The port keeps codex's timing model (an inter-character window plus an
//! idle timeout before the buffer flushes) but drops its "hold the first
//! character" flicker suppression: holding a character means the draft can
//! only show it after a timer tick, and this port has no guaranteed ticker
//! between keystrokes. Every character is therefore inserted immediately and
//! a confirmed burst *retroactively* absorbs the prefix it already typed
//! ([`BurstDecision::BeginBurst`]). A misclassification can merge a few
//! characters into one paste unit, but it can never drop one
//! ([`PasteBurst::abort`] hands the whole buffer back).

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode as CtKeyCode, KeyEvent as CtKeyEvent, KeyModifiers as CtModifiers};

/// Single key — a [`KeyCode`] plus any modifiers that were active when
/// the key was pressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Key {
    /// Logical key.
    pub code: KeyCode,
    /// Active modifiers.
    pub modifiers: KeyModifiers,
}

impl Key {
    /// Convenience constructor.
    pub const fn new(code: KeyCode, modifiers: KeyModifiers) -> Self {
        Self { code, modifiers }
    }

    /// Construct a plain character key with no modifiers.
    pub const fn char(c: char) -> Self {
        Self {
            code: KeyCode::Char(c),
            modifiers: KeyModifiers::NONE,
        }
    }

    /// True when no modifiers are active.
    pub const fn is_plain(&self) -> bool {
        self.modifiers.is_empty()
    }
}

/// Logical key — matches the crossterm variant but stripped of
/// platform-specific edge cases.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyCode {
    /// Backspace.
    Backspace,
    /// Enter / Return.
    Enter,
    /// Left arrow.
    Left,
    /// Right arrow.
    Right,
    /// Up arrow.
    Up,
    /// Down arrow.
    Down,
    /// Home.
    Home,
    /// End.
    End,
    /// Page up.
    PageUp,
    /// Page down.
    PageDown,
    /// Tab.
    Tab,
    /// BackTab (Shift+Tab on most terminals).
    BackTab,
    /// Forward Delete.
    Delete,
    /// Insert.
    Insert,
    /// Escape.
    Esc,
    /// A character key.
    Char(char),
    /// Function key `Fn` (n in 1..=12).
    F(u8),
    /// Anything else — keep the original code around so we never
    /// silently drop a key the user expects to work.
    Other,
}

/// Modifier flags — a bitfield so multiple modifiers can be combined.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct KeyModifiers {
    /// Shift held.
    pub shift: bool,
    /// Control held.
    pub control: bool,
    /// Alt held.
    pub alt: bool,
    /// Meta / Super held.
    pub meta: bool,
}

impl KeyModifiers {
    /// No modifiers.
    pub const NONE: Self = Self {
        shift: false,
        control: false,
        alt: false,
        meta: false,
    };

    /// Shift held alone.
    pub const SHIFT: Self = Self {
        shift: true,
        control: false,
        alt: false,
        meta: false,
    };

    /// Control held alone.
    pub const CONTROL: Self = Self {
        shift: false,
        control: true,
        alt: false,
        meta: false,
    };

    /// Alt held alone.
    pub const ALT: Self = Self {
        shift: false,
        control: false,
        alt: true,
        meta: false,
    };

    /// True when no flags are set.
    pub const fn is_empty(&self) -> bool {
        !self.shift && !self.control && !self.alt && !self.meta
    }
}

impl std::ops::BitOr for KeyModifiers {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self {
            shift: self.shift || rhs.shift,
            control: self.control || rhs.control,
            alt: self.alt || rhs.alt,
            meta: self.meta || rhs.meta,
        }
    }
}

/// Mouse button a gesture is performed with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MouseButton {
    /// Primary (usually left) button.
    Left,
    /// Middle button / wheel click.
    Middle,
    /// Secondary (usually right) button.
    Right,
    /// Any button crossterm reports that we do not model.
    Other,
}

/// The gesture a non-wheel mouse event carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MouseGestureKind {
    /// A button went down.
    Press(MouseButton),
    /// A button came back up.
    Release(MouseButton),
    /// Motion while a button is held.
    Drag(MouseButton),
    /// Motion with no button held.
    Move,
}

/// A non-wheel mouse event: what happened, where, with which modifiers.
///
/// Coordinates are 0-based terminal cells measured from the top-left of
/// the screen — the same space `crossterm` reports and the same space the
/// [`App`](crate::App) renders into. The widget kit treats the wheel
/// separately (upstream routes it through `parseWheelEvent` /
/// `routeWheel`), which is why [`InputEvent::Mouse`] stays wheel-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseGesture {
    /// What happened.
    pub kind: MouseGestureKind,
    /// Column, from the left edge of the terminal.
    pub x: u16,
    /// Row, from the top edge of the terminal.
    pub y: u16,
    /// True when Alt was held.
    pub alt: bool,
}

impl MouseGesture {
    /// Convenience constructor.
    pub const fn new(kind: MouseGestureKind, x: u16, y: u16, alt: bool) -> Self {
        Self { kind, x, y, alt }
    }

    /// Convenience constructor for a left-button press, the gesture that
    /// starts a chat-log text selection.
    pub const fn left_press(x: u16, y: u16) -> Self {
        Self::new(MouseGestureKind::Press(MouseButton::Left), x, y, false)
    }

    /// Convenience constructor for a left-button drag — extends an active
    /// selection.
    pub const fn left_drag(x: u16, y: u16) -> Self {
        Self::new(MouseGestureKind::Drag(MouseButton::Left), x, y, false)
    }

    /// Convenience constructor for a left-button release — finalises the
    /// selection (and, with copy-on-select on, copies it).
    pub const fn left_release(x: u16, y: u16) -> Self {
        Self::new(MouseGestureKind::Release(MouseButton::Left), x, y, false)
    }
}

/// Component-level input event. New variants can be added without
/// breaking the [`From`] conversions below — unknown crossterm events
/// collapse into [`InputEvent::Ignored`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputEvent {
    /// A key event.
    Key(Key),
    /// A mouse-wheel notch at an absolute terminal cell.
    ///
    /// The wheel has its own variant because upstream dispatches it in two
    /// stages: `dispatchMouseToOverlay` → `dispatchMouseToLayout` get the
    /// notch first — that is how the composer's `SelectList` scrolls its own
    /// candidates (`packages/tui/src/components/select-list.ts:110-121`) —
    /// and only an unclaimed notch reaches `routeWheel`, which scrolls the
    /// scroll view under the pointer and refreshes the scrollbar hover
    /// (`packages/tui/src/tui-alt-screen.ts:679-694,973-984`). Both stages
    /// need the cell, so the wheel carries it exactly like
    /// [`MouseGesture`] does.
    Mouse {
        /// True when the wheel moved towards older output (up), false when
        /// it moved towards the tail (down).
        up: bool,
        /// True when Alt was held. Upstream multiplies the step by the
        /// alt-wheel multiplier in that case
        /// (`packages/tui/src/tui-alt-screen.ts:968-971`).
        alt: bool,
        /// Column, from the left edge of the terminal (0-based cells).
        x: u16,
        /// Row, from the top edge of the terminal (0-based cells).
        y: u16,
    },
    /// A non-wheel mouse gesture (press / release / drag / move) at an
    /// absolute terminal cell.
    MouseGesture(MouseGesture),
    /// Resize — the [`App`](crate::App) re-flows the layout and redraws.
    Resize {
        /// New width in columns.
        width: u16,
        /// New height in rows.
        height: u16,
    },
    /// Anything we cannot map to a component-level event (focus reports,
    /// scrollbar-less horizontal wheel, …). Components treat it as a
    /// no-op.
    Ignored,
}

/// True when a `crossterm` `Enter` event is really the `Ctrl+J` chord.
///
/// `0x0A` is simultaneously the line-feed byte and the key code for `Ctrl+J`,
/// and the two platforms that matter report it differently:
///
/// * crossterm's **Unix** parser folds it into `Char('j') + CONTROL` — its own
///   source says so (“`\n` = 0xA, which is also the keycode for Ctrl+J … it's
///   better to use Ctrl+J”, `event/sys/unix/parse.rs`).
/// * its **Win32** console backend reports the console record instead: a
///   `KEY_EVENT` whose unicode character is `\n` and whose virtual key is
///   `VK_RETURN`, which crossterm normalises to `Enter + CONTROL`. Measured on
///   Windows 10 under ConPTY: writing `0x0A` to the pty produces exactly that
///   event, and pre-fix the chord was silently dropped — `tui.input.newLine`
///   is `"ctrl+j"`, i.e. `Char('j') + CONTROL`, so `Enter + CONTROL` matched
///   no binding and was discarded as an unmatched control chord.
///
/// Normalising here (rather than teaching `key_matches` a second spelling)
/// keeps [`InputEvent`] identical on both platforms, which is what the rest of
/// the port assumes: `pi-tui` has one editor, one keybinding table and one
/// meaning for the chord it documents as the portable multiline path.
///
/// A real kitty-protocol `Ctrl+Enter` (`CSI 13;5u`) decodes to `Enter +
/// CONTROL` as well and is therefore read as `Ctrl+J` too. Upstream's byte
/// matcher has the same blind spot in reverse (it never matches kitty's
/// `Ctrl+Enter` at all), no binding here uses `ctrl+enter`, and the outcome is
/// the same line break either way — recorded so the choice is visible.
fn is_ctrl_j(modifiers: &CtModifiers) -> bool {
    modifiers.contains(CtModifiers::CONTROL)
        && !modifiers.contains(CtModifiers::SHIFT)
        && !modifiers.contains(CtModifiers::ALT)
        && !modifiers.contains(CtModifiers::SUPER)
        && !modifiers.contains(CtModifiers::META)
}

impl From<CtKeyEvent> for InputEvent {
    fn from(event: CtKeyEvent) -> Self {
        let code = match event.code {
            // `\n` is a key code, not a newline; see [`ctrl_j_from_enter`].
            CtKeyCode::Enter if is_ctrl_j(&event.modifiers) => KeyCode::Char('j'),
            CtKeyCode::Backspace => KeyCode::Backspace,
            CtKeyCode::Enter => KeyCode::Enter,
            CtKeyCode::Left => KeyCode::Left,
            CtKeyCode::Right => KeyCode::Right,
            CtKeyCode::Up => KeyCode::Up,
            CtKeyCode::Down => KeyCode::Down,
            CtKeyCode::Home => KeyCode::Home,
            CtKeyCode::End => KeyCode::End,
            CtKeyCode::PageUp => KeyCode::PageUp,
            CtKeyCode::PageDown => KeyCode::PageDown,
            CtKeyCode::Tab => KeyCode::Tab,
            CtKeyCode::BackTab => KeyCode::BackTab,
            CtKeyCode::Delete => KeyCode::Delete,
            CtKeyCode::Insert => KeyCode::Insert,
            CtKeyCode::Esc => KeyCode::Esc,
            CtKeyCode::Char(c) => KeyCode::Char(c),
            CtKeyCode::F(n) => KeyCode::F(n),
            _ => KeyCode::Other,
        };
        let mut mods = KeyModifiers::NONE;
        if event.modifiers.contains(CtModifiers::SHIFT) {
            mods.shift = true;
        }
        if event.modifiers.contains(CtModifiers::CONTROL) {
            mods.control = true;
        }
        if event.modifiers.contains(CtModifiers::ALT) {
            mods.alt = true;
        }
        if event.modifiers.contains(CtModifiers::SUPER)
            || event.modifiers.contains(CtModifiers::META)
        {
            mods.meta = true;
        }
        InputEvent::Key(Key::new(code, mods))
    }
}

impl InputEvent {
    /// Convenience constructor used by tests.
    pub fn key(code: KeyCode, modifiers: KeyModifiers) -> Self {
        InputEvent::Key(Key::new(code, modifiers))
    }

    /// Convenience constructor for a plain character.
    pub fn character(c: char) -> Self {
        InputEvent::Key(Key::char(c))
    }

    /// Convenience constructor for a wheel notch at `(x, y)`.
    pub const fn wheel(up: bool, alt: bool, x: u16, y: u16) -> Self {
        InputEvent::Mouse { up, alt, x, y }
    }

    /// Convenience constructor for a non-wheel mouse gesture.
    pub const fn gesture(gesture: MouseGesture) -> Self {
        InputEvent::MouseGesture(gesture)
    }
}

/// Decode one complete raw mouse report into a component-level event.
///
/// Terminal mouse tracking comes in two encodings:
///
/// * **SGR** — `ESC [ < b ; x ; y M` (press / motion) or `… m` (release),
///   with 1-based coordinates. Upstream's `parseSgrMouseEvent`
///   (`packages/tui/src/tui-alt-screen.ts:987-995`).
/// * **X10 / normal tracking** — `ESC [ M` followed by exactly three bytes:
///   `Cb` is the button / modifier code plus 32, `Cx` and `Cy` are the
///   1-based column and row plus 32 (subtracting 33 gives the 0-based cell
///   the App renders into). It is the encoding older terminals and `TERM`s
///   fall back to; upstream parses it in `parseWheelEvent` /
///   `isMouseSequence` (`packages/tui/src/tui-alt-screen.ts:952-967,1613-1616`).
///
/// Wheel reports become [`InputEvent::Mouse`] (carrying the Alt flag),
/// presses / releases / drags / bare motion become
/// [`InputEvent::MouseGesture`], and a horizontal wheel — which the App has
/// no consumer for — becomes [`InputEvent::Ignored`]. `None` means the input
/// is not a complete mouse report, so the caller falls through to the key
/// path.
///
/// # X10 decoding
///
/// The `Cb` byte packs the button and the modifiers the way SGR's `b` field
/// did before it was moved into ASCII:
///
/// * bits 0-1 — button: 0 left, 1 middle, 2 right, 3 "no button";
/// * bit 2 (`0x04`) — Shift, bit 3 (`0x08`) — Alt, bit 4 (`0x10`) — Ctrl;
/// * bit 5 (`0x20`) — motion, i.e. the report is a drag or a bare pointer
///   move rather than a press;
/// * bits 6-7 — together with bits 0-1 they extend the button field:
///   `button = low2 | (high2 << 2)`. Numbers 4..=7 are the wheel family
///   (4 up, 5 down, 6/7 horizontal) and 8..=15 are extra buttons neither
///   `crossterm` nor this port models.
///
/// Button 3 (`0x03`) is the X10 release marker, but it is indistinguishable
/// from a bare pointer move once the motion bit is set — only SGR's `m`
/// terminator removes the ambiguity. Legacy terminals cannot, so a `0x03`
/// report is read as a release (what finishes a selection drag) unless the
/// motion bit is set. `crossterm` resolves the same ambiguity the same way
/// (`parse_cb`, `crossterm-0.28.1/src/event/sys/unix/parse.rs:772-806`),
/// which is what the interactive driver actually runs.
pub fn parse_mouse_sequence(sequence: &[u8]) -> Option<InputEvent> {
    if sequence.starts_with(b"\x1b[<") {
        let rest = &sequence[3..];
        // Take the terminator off first: `m` is the only thing that tells a
        // release apart from a press.
        let (body, terminator) = rest.split_at(rest.len().checked_sub(1)?);
        let release = match terminator {
            [b'M'] => false,
            [b'm'] => true,
            _ => return None,
        };
        let body = std::str::from_utf8(body).ok()?;
        let mut parts = body.split(';');
        let cb: u8 = parts.next()?.parse().ok()?;
        let x: u16 = parts.next()?.parse().ok()?;
        let y: u16 = parts.next()?.parse().ok()?;
        if parts.next().is_some() {
            return None;
        }
        // SGR coordinates are 1-based; the App works in 0-based cells.
        return Some(decode_mouse_report(
            cb,
            x.saturating_sub(1),
            y.saturating_sub(1),
            release,
        ));
    }
    // X10 / normal tracking: exactly three bytes after the introducer.
    if sequence.len() == 6 && sequence.starts_with(b"\x1b[M") {
        let cb = sequence[3].checked_sub(32)?;
        return Some(decode_mouse_report(
            cb,
            u16::from(sequence[4].saturating_sub(33)),
            u16::from(sequence[5].saturating_sub(33)),
            false,
        ));
    }
    None
}

/// Whether `sequence` is a mouse report in either encoding — upstream's
/// `isMouseSequence` (`packages/tui/src/tui-alt-screen.ts:1613-1616`).
///
/// A terminal layer uses this to swallow a report it cannot decode instead of
/// leaking the bytes into the key path. Upstream's `\d+` regex is unbounded,
/// so this port additionally rejects fields that would not fit the wire types
/// [`parse_mouse_sequence`] reads: `true` here never means `None` there.
pub fn is_mouse_sequence(sequence: &[u8]) -> bool {
    if sequence.starts_with(b"\x1b[<") {
        let rest = &sequence[3..];
        let Some((&terminator, body)) = rest.split_last() else {
            return false;
        };
        if terminator != b'M' && terminator != b'm' {
            return false;
        }
        let Ok(body) = std::str::from_utf8(body) else {
            return false;
        };
        let mut parts = body.split(';');
        // Fields must be plain digits that fit the wire types the decoder
        // reads (`Cb` is a `u8`, the coordinates `u16`), so a report this
        // predicate accepts is always one [`parse_mouse_sequence`] decodes.
        let numeric = |part: Option<&str>, max: u32| {
            part.is_some_and(|value| {
                !value.is_empty()
                    && value.bytes().all(|byte| byte.is_ascii_digit())
                    && value.parse::<u32>().is_ok_and(|value| value <= max)
            })
        };
        return numeric(parts.next(), u8::MAX.into())
            && numeric(parts.next(), u16::MAX.into())
            && numeric(parts.next(), u16::MAX.into())
            && parts.next().is_none();
    }
    sequence.len() == 6 && sequence.starts_with(b"\x1b[M")
}

/// Decode the `Cb` byte shared by both encodings into an [`InputEvent`].
///
/// Mirrors `crossterm`'s `parse_cb`
/// (`crossterm-0.28.1/src/event/sys/unix/parse.rs:772-806`), bit for bit —
/// see [`parse_mouse_sequence`] for the bit layout. `release` is SGR's
/// lowercase `m`, which `crossterm` applies by turning the decoded press
/// into the matching release (`parse_csi_sgr_mouse`, same file, line 746);
/// the X10 path always passes `false` because it has no terminator to read.
///
/// Shift and Ctrl are dropped on purpose: [`MouseGesture`] carries only
/// `alt`, exactly like [`App::translate_event`](crate::App::translate_event)
/// drops them when it converts a `crossterm::event::MouseEvent`.
fn decode_mouse_report(cb: u8, x: u16, y: u16, release: bool) -> InputEvent {
    let alt = cb & 0b0000_1000 != 0;
    let motion = cb & 0b0010_0000 != 0;
    let button_number = (cb & 0b0000_0011) | ((cb & 0b1100_0000) >> 4);
    // The wheel keeps its own variant, so it is split out before the gesture
    // table. Horizontal notches (6/7) have no consumer in the App, exactly
    // like upstream's `routeWheel`
    // (`packages/tui/src/tui-alt-screen.ts:952-967`).
    match (button_number, motion) {
        (4, false) => return InputEvent::wheel(true, alt, x, y),
        (5, false) => return InputEvent::wheel(false, alt, x, y),
        _ => {}
    }
    let kind = match (button_number, motion) {
        (0, false) => MouseGestureKind::Press(MouseButton::Left),
        (1, false) => MouseGestureKind::Press(MouseButton::Middle),
        (2, false) => MouseGestureKind::Press(MouseButton::Right),
        (0, true) => MouseGestureKind::Drag(MouseButton::Left),
        (1, true) => MouseGestureKind::Drag(MouseButton::Middle),
        (2, true) => MouseGestureKind::Drag(MouseButton::Right),
        // X10's "no button" marker — with the motion bit it is a bare pointer
        // move, without it the button a legacy terminal will not name.
        (3, false) => MouseGestureKind::Release(MouseButton::Left),
        (3, true) | (4, true) | (5, true) => MouseGestureKind::Move,
        // Horizontal wheel, unmodelled buttons, and the button numbers
        // `crossterm` itself refuses to parse.
        _ => return InputEvent::Ignored,
    };
    let kind = if release {
        match kind {
            MouseGestureKind::Press(button) => MouseGestureKind::Release(button),
            other => other,
        }
    } else {
        kind
    };
    InputEvent::MouseGesture(MouseGesture::new(kind, x, y, alt))
}

// ---------------------------------------------------------------------------
// Paste-burst detection (codex `paste_burst`)
// ---------------------------------------------------------------------------

/// Largest delay between two plain characters for them to count as one
/// burst. codex `PASTE_BURST_CHAR_INTERVAL`
/// (`codex-rs/tui/src/bottom_pane/paste_burst.rs`).
///
/// 8 ms is far below any human typing rate (a fast typist is ~100 ms per
/// key), which is exactly what keeps normal input out of the burst path.
pub const PASTE_BURST_CHAR_INTERVAL: Duration = Duration::from_millis(8);

/// Plain characters a run must reach, each within
/// [`PASTE_BURST_CHAR_INTERVAL`] of the previous, before the run is treated
/// as a paste (codex `PASTE_BURST_MIN_CHARS`).
pub const PASTE_BURST_MIN_CHARS: u16 = 3;

/// How long after the last burst character a lone `Enter` is still read as a
/// newline *inside* the paste rather than a submit (codex
/// `PASTE_ENTER_SUPPRESS_WINDOW`).
///
/// A paste that ends in a newline is the case this exists for: without the
/// window, the final newline of a burst would submit the draft the paste was
/// just poured into.
pub const PASTE_ENTER_SUPPRESS_WINDOW: Duration = Duration::from_millis(120);

/// Idle time after the last burst character before the accumulated text is
/// flushed as one paste (codex `PASTE_BURST_ACTIVE_IDLE_TIMEOUT`).
///
/// Windows terminals have been observed to deliver slower bursts, so the
/// port keeps codex's platform split.
#[cfg(windows)]
pub const PASTE_BURST_ACTIVE_IDLE_TIMEOUT: Duration = Duration::from_millis(60);
/// Idle time after the last burst character before the accumulated text is
/// flushed as one paste (codex `PASTE_BURST_ACTIVE_IDLE_TIMEOUT`).
#[cfg(not(windows))]
pub const PASTE_BURST_ACTIVE_IDLE_TIMEOUT: Duration = Duration::from_millis(8);

/// What the caller must do with a plain character handed to
/// [`PasteBurst::on_plain_char`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BurstDecision {
    /// Not paste-like: insert the character as ordinary typing.
    Typed,
    /// The run just became paste-like and the current character is already
    /// in the burst buffer. Remove the last `retro_chars` characters before
    /// the cursor and hand them to [`PasteBurst::absorb_retro`], so the whole
    /// burst sits in one buffer; when the cursor cannot yield them, call
    /// [`PasteBurst::abort`] and insert what it returns as ordinary text.
    BeginBurst {
        /// How many already-typed characters belong to the burst.
        retro_chars: usize,
    },
    /// The character is in the burst buffer; nothing to insert.
    Buffered,
}

/// codex's `paste_burst` classifier: turns a fast run of plain characters
/// into one paste when the terminal sends no bracketed-paste event.
///
/// Pure state machine over an injected [`Instant`] — it owns timing and
/// classification, never the text buffer. The caller feeds it plain
/// characters and applies the returned [`BurstDecision`]; when the burst goes
/// quiet, [`PasteBurst::flush_if_due`] hands back the accumulated text to be
/// inserted through the ordinary paste path (so the marker folding, the undo
/// unit and the registry are all reused).
///
/// A run only becomes a burst when **three** characters arrive within
/// [`PASTE_BURST_CHAR_INTERVAL`] of each other, so ordinary typing never
/// reaches it; and every character is inserted as typing first, so a
/// misclassification degrades to "a few characters inserted together"
/// rather than a lost keystroke.
#[derive(Debug, Clone, Default)]
pub struct PasteBurst {
    /// When the previous plain character arrived.
    last_plain_char: Option<Instant>,
    /// Plain characters in the current run, each within the window of the
    /// previous one.
    consecutive: u16,
    /// While `now <= window_until`, `Enter` still belongs to the burst even
    /// after the buffer has been flushed.
    window_until: Option<Instant>,
    /// Text accumulated behind the marker while a burst is active.
    buffer: String,
    /// True while characters are being accumulated into `buffer`.
    active: bool,
}

impl PasteBurst {
    /// A fresh, idle classifier.
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one plain character (no Control / Alt / Meta modifier) at `now`.
    pub fn on_plain_char(&mut self, c: char, now: Instant) -> BurstDecision {
        let within_window = self
            .last_plain_char
            .is_some_and(|last| now.saturating_duration_since(last) <= PASTE_BURST_CHAR_INTERVAL);
        self.consecutive = if within_window {
            self.consecutive.saturating_add(1)
        } else {
            1
        };
        self.last_plain_char = Some(now);

        if self.active {
            self.buffer.push(c);
            self.window_until = Some(now + PASTE_ENTER_SUPPRESS_WINDOW);
            return BurstDecision::Buffered;
        }
        if within_window && self.consecutive >= PASTE_BURST_MIN_CHARS {
            self.active = true;
            self.buffer.push(c);
            self.window_until = Some(now + PASTE_ENTER_SUPPRESS_WINDOW);
            return BurstDecision::BeginBurst {
                retro_chars: usize::from(self.consecutive - 1),
            };
        }
        BurstDecision::Typed
    }

    /// Fold the already-typed prefix of a burst into its buffer.
    ///
    /// The prefix is prepended because the current character is already
    /// buffered, and buffer order is the order the bytes arrived in.
    pub fn absorb_retro(&mut self, prefix: &str) {
        if !prefix.is_empty() {
            self.buffer.insert_str(0, prefix);
        }
    }

    /// Abandon an active burst, returning everything buffered.
    ///
    /// This is the no-character-is-ever-lost fallback: when the caller
    /// cannot absorb the prefix, it inserts the returned text as ordinary
    /// typing.
    pub fn abort(&mut self) -> String {
        self.active = false;
        self.window_until = None;
        self.consecutive = 0;
        std::mem::take(&mut self.buffer)
    }

    /// True while characters are being accumulated into a burst.
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// The text accumulated so far (empty unless [`PasteBurst::is_active`]).
    pub fn buffered(&self) -> &str {
        &self.buffer
    }

    /// Whether `Enter` should insert a newline instead of submitting.
    ///
    /// True while a burst accumulates and for
    /// [`PASTE_ENTER_SUPPRESS_WINDOW`] after the last burst character, so the
    /// trailing newline of a paste never submits the draft it was pasted
    /// into.
    pub fn newline_should_insert(&self, now: Instant) -> bool {
        self.active || self.window_until.is_some_and(|until| now <= until)
    }

    /// Append a newline / tab to the active burst.
    ///
    /// Returns `false` when no burst is accumulating, in which case the key
    /// keeps its ordinary meaning (submit / complete). A control character
    /// only ever *joins* a burst that is already open: it never starts one.
    pub fn append_control_if_active(&mut self, c: char, now: Instant) -> bool {
        if !self.active {
            return false;
        }
        self.buffer.push(c);
        self.last_plain_char = Some(now);
        self.window_until = Some(now + PASTE_ENTER_SUPPRESS_WINDOW);
        true
    }

    /// Flush the burst once it has gone quiet, returning the text to insert
    /// as one paste.
    ///
    /// `None` while the burst is still accumulating or when there is nothing
    /// buffered.
    pub fn flush_if_due(&mut self, now: Instant) -> Option<String> {
        if !self.active {
            return None;
        }
        let quiet = self.last_plain_char.is_some_and(|last| {
            now.saturating_duration_since(last) > PASTE_BURST_ACTIVE_IDLE_TIMEOUT
        });
        if !quiet {
            return None;
        }
        Some(self.finish())
    }

    /// End the burst now, whatever the clock says — a key that cannot belong
    /// to a paste is about to be handled.
    pub fn flush_now(&mut self) -> Option<String> {
        if !self.active && self.buffer.is_empty() {
            return None;
        }
        let buffer = self.finish();
        (!buffer.is_empty()).then_some(buffer)
    }

    /// Flush and forget the classification window in one step: the next
    /// keystroke starts a fresh run instead of joining the one just ended.
    pub fn flush_now_and_clear(&mut self) -> Option<String> {
        let flushed = self.flush_now();
        self.clear_window();
        flushed
    }

    /// Forget the classification window (the buffer is untouched).
    ///
    /// Used after an authoritative bracketed paste or a non-character key:
    /// the next keystroke must start a fresh run rather than being grouped
    /// with a burst that has already ended.
    pub fn clear_window(&mut self) {
        self.last_plain_char = None;
        self.consecutive = 0;
        self.window_until = None;
    }

    /// When an active burst will be flushable, for a driver that polls.
    ///
    /// `None` when no burst is accumulating.
    pub fn flush_deadline(&self) -> Option<Instant> {
        if !self.active {
            return None;
        }
        self.last_plain_char
            .map(|last| last + PASTE_BURST_ACTIVE_IDLE_TIMEOUT)
    }

    /// Reset the active state, handing the buffer back.
    ///
    /// `window_until` deliberately survives: codex's enter-suppress window
    /// outlives the buffer, so the trailing newline of a paste is still read
    /// as a newline right after the text has been handed over.
    fn finish(&mut self) -> String {
        self.active = false;
        self.consecutive = 0;
        std::mem::take(&mut self.buffer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gesture_constructors_pin_button_coordinates_and_alt() {
        assert_eq!(
            InputEvent::gesture(MouseGesture::left_press(3, 7)),
            InputEvent::MouseGesture(MouseGesture {
                kind: MouseGestureKind::Press(MouseButton::Left),
                x: 3,
                y: 7,
                alt: false,
            })
        );
        assert_eq!(
            MouseGesture::left_drag(4, 8).kind,
            MouseGestureKind::Drag(MouseButton::Left)
        );
        assert_eq!(
            MouseGesture::left_release(4, 8).kind,
            MouseGestureKind::Release(MouseButton::Left)
        );
        assert!(MouseGesture::new(MouseGestureKind::Move, 1, 2, true).alt);
    }

    #[test]
    fn wheel_constructor_sets_direction_modifier_and_cell() {
        assert_eq!(
            InputEvent::wheel(true, false, 4, 9),
            InputEvent::Mouse {
                up: true,
                alt: false,
                x: 4,
                y: 9,
            }
        );
        assert_eq!(
            InputEvent::wheel(false, true, 0, 0),
            InputEvent::Mouse {
                up: false,
                alt: true,
                x: 0,
                y: 0,
            }
        );
    }

    fn gesture(kind: MouseGestureKind, x: u16, y: u16, alt: bool) -> InputEvent {
        InputEvent::gesture(MouseGesture::new(kind, x, y, alt))
    }

    #[test]
    fn sgr_reports_decode_buttons_coordinates_and_terminators() {
        // SGR coordinates are 1-based: `11;6` is cell (10, 5).
        assert_eq!(
            parse_mouse_sequence(b"\x1b[<0;11;6M"),
            Some(gesture(
                MouseGestureKind::Press(MouseButton::Left),
                10,
                5,
                false
            ))
        );
        assert_eq!(
            parse_mouse_sequence(b"\x1b[<1;1;1M"),
            Some(gesture(
                MouseGestureKind::Press(MouseButton::Middle),
                0,
                0,
                false
            ))
        );
        assert_eq!(
            parse_mouse_sequence(b"\x1b[<2;3;4M"),
            Some(gesture(
                MouseGestureKind::Press(MouseButton::Right),
                2,
                3,
                false
            ))
        );
        // The lowercase `m` turns the decoded press into a release, whichever
        // button number the terminal sent.
        assert_eq!(
            parse_mouse_sequence(b"\x1b[<0;11;6m"),
            Some(gesture(
                MouseGestureKind::Release(MouseButton::Left),
                10,
                5,
                false
            ))
        );
        assert_eq!(
            parse_mouse_sequence(b"\x1b[<3;11;6M"),
            Some(gesture(
                MouseGestureKind::Release(MouseButton::Left),
                10,
                5,
                false
            ))
        );
        // Motion bit: a held button drags, `3` alone is a bare move.
        assert_eq!(
            parse_mouse_sequence(b"\x1b[<32;11;6M"),
            Some(gesture(
                MouseGestureKind::Drag(MouseButton::Left),
                10,
                5,
                false
            ))
        );
        assert_eq!(
            parse_mouse_sequence(b"\x1b[<35;11;6M"),
            Some(gesture(MouseGestureKind::Move, 10, 5, false))
        );
        // Alt is bit 3.
        assert_eq!(
            parse_mouse_sequence(b"\x1b[<8;11;6M"),
            Some(gesture(
                MouseGestureKind::Press(MouseButton::Left),
                10,
                5,
                true
            ))
        );
    }

    #[test]
    fn sgr_wheel_reports_become_wheel_events_and_horizontal_is_ignored() {
        // SGR coordinates are 1-based: `5;5` is cell (4, 4), so the notch
        // carries the same cell a gesture report would.
        assert_eq!(
            parse_mouse_sequence(b"\x1b[<64;5;5M"),
            Some(InputEvent::wheel(true, false, 4, 4))
        );
        assert_eq!(
            parse_mouse_sequence(b"\x1b[<65;5;5M"),
            Some(InputEvent::wheel(false, false, 4, 4))
        );
        assert_eq!(
            parse_mouse_sequence(b"\x1b[<72;5;5M"),
            Some(InputEvent::wheel(true, true, 4, 4))
        );
        // Scroll left / right have no consumer in the App.
        assert_eq!(
            parse_mouse_sequence(b"\x1b[<66;5;5M"),
            Some(InputEvent::Ignored)
        );
        assert_eq!(
            parse_mouse_sequence(b"\x1b[<67;5;5M"),
            Some(InputEvent::Ignored)
        );
    }

    #[test]
    fn x10_reports_decode_the_legacy_encoding() {
        // ESC [ M Cb Cx Cy, each byte the value plus 32: button 0 at (10, 5).
        assert_eq!(
            parse_mouse_sequence(b"\x1b[M\x20\x2b\x26"),
            Some(gesture(
                MouseGestureKind::Press(MouseButton::Left),
                10,
                5,
                false
            ))
        );
        // Button 3 is the release marker.
        assert_eq!(
            parse_mouse_sequence(b"\x1b[M\x23\x2b\x26"),
            Some(gesture(
                MouseGestureKind::Release(MouseButton::Left),
                10,
                5,
                false
            ))
        );
        // Motion bit + button 0 is a drag.
        assert_eq!(
            parse_mouse_sequence(b"\x1b[M\x40\x2b\x26"),
            Some(gesture(
                MouseGestureKind::Drag(MouseButton::Left),
                10,
                5,
                false
            ))
        );
        // Motion bit + button 3 is a bare pointer move.
        assert_eq!(
            parse_mouse_sequence(b"\x1b[M\x43\x2b\x26"),
            Some(gesture(MouseGestureKind::Move, 10, 5, false))
        );
        // Wheel family: 0x40 is a notch up, 0x41 down, 0x48 up + Alt.
        assert_eq!(
            parse_mouse_sequence(b"\x1b[M\x60\x2b\x26"),
            Some(InputEvent::wheel(true, false, 10, 5))
        );
        assert_eq!(
            parse_mouse_sequence(b"\x1b[M\x61\x2b\x26"),
            Some(InputEvent::wheel(false, false, 10, 5))
        );
        assert_eq!(
            parse_mouse_sequence(b"\x1b[M\x68\x2b\x26"),
            Some(InputEvent::wheel(true, true, 10, 5))
        );
        // Coordinates are raw bytes, so they are not limited to ASCII: byte
        // 200 is column 167.
        assert_eq!(
            parse_mouse_sequence(b"\x1b[M\x20\xc8\xc9"),
            Some(gesture(
                MouseGestureKind::Press(MouseButton::Left),
                167,
                168,
                false
            ))
        );
    }

    #[test]
    fn incomplete_or_foreign_sequences_are_rejected() {
        assert_eq!(parse_mouse_sequence(b""), None);
        assert_eq!(parse_mouse_sequence(b"a"), None);
        assert_eq!(parse_mouse_sequence(b"\x1b[A"), None, "arrow key");
        assert_eq!(parse_mouse_sequence(b"\x1b[<"), None, "no payload");
        assert_eq!(parse_mouse_sequence(b"\x1b[<0;1;1"), None, "no terminator");
        assert_eq!(parse_mouse_sequence(b"\x1b[<0;1M"), None, "missing column");
        assert_eq!(
            parse_mouse_sequence(b"\x1b[<0;1;1;2M"),
            None,
            "too many fields"
        );
        assert_eq!(parse_mouse_sequence(b"\x1b[<a;1;1M"), None, "not a number");
        assert_eq!(parse_mouse_sequence(b"\x1b[M\x20\x2b"), None, "short X10");
        assert_eq!(
            parse_mouse_sequence(b"\x1b[M\x20\x2b\x26\x00"),
            None,
            "long X10"
        );
    }

    #[test]
    fn is_mouse_sequence_accepts_exactly_what_parsing_accepts() {
        for sequence in [
            b"\x1b[<0;1;1M".as_slice(),
            b"\x1b[<64;1;1m".as_slice(),
            b"\x1b[M\x20\x2b\x26".as_slice(),
        ] {
            assert!(is_mouse_sequence(sequence), "{sequence:?} is a report");
            assert!(
                parse_mouse_sequence(sequence).is_some(),
                "{sequence:?} must also decode"
            );
        }
        for sequence in [
            b"".as_slice(),
            b"\x1b[A".as_slice(),
            b"\x1b[<0;1;1".as_slice(),
            b"\x1b[<0;1M".as_slice(),
            b"\x1b[<0;1;1;2M".as_slice(),
            b"\x1b[<a;1;1M".as_slice(),
            // Out-of-range fields: `Cb` would not fit a `u8`, the column
            // would not fit a `u16`.
            b"\x1b[<256;1;1M".as_slice(),
            b"\x1b[<0;99999;1M".as_slice(),
            b"\x1b[M\x20\x2b".as_slice(),
        ] {
            assert!(!is_mouse_sequence(sequence), "{sequence:?} is not a report");
            assert_eq!(parse_mouse_sequence(sequence), None);
        }
    }

    // -- paste burst ---------------------------------------------------

    /// A clock the tests advance by hand: `Instant` cannot be constructed
    /// from a fixed value, so every step is built by adding to one base.
    fn at(base: Instant, millis: u64) -> Instant {
        base + Duration::from_millis(millis)
    }

    #[test]
    fn three_fast_characters_become_one_burst() {
        let base = Instant::now();
        let mut burst = PasteBurst::new();
        // The first two characters are ordinary typing; nothing is held.
        assert_eq!(burst.on_plain_char('l', at(base, 0)), BurstDecision::Typed);
        assert_eq!(burst.on_plain_char('o', at(base, 2)), BurstDecision::Typed);
        // The third one, inside the window, confirms the burst and names the
        // two characters already typed as its prefix.
        assert_eq!(
            burst.on_plain_char('g', at(base, 4)),
            BurstDecision::BeginBurst { retro_chars: 2 }
        );
        assert!(burst.is_active());
        assert_eq!(burst.buffered(), "g");
        burst.absorb_retro("lo");
        assert_eq!(burst.buffered(), "log");
        // Every further fast character joins the buffer instead of typing.
        assert_eq!(
            burst.on_plain_char('s', at(base, 6)),
            BurstDecision::Buffered
        );
        assert_eq!(burst.buffered(), "logs");
    }

    #[test]
    fn characters_outside_the_window_stay_ordinary_typing() {
        let base = Instant::now();
        let mut burst = PasteBurst::new();
        assert_eq!(burst.on_plain_char('h', at(base, 0)), BurstDecision::Typed);
        // 120 ms later is human typing, not a paste.
        assert_eq!(
            burst.on_plain_char('e', at(base, 120)),
            BurstDecision::Typed
        );
        assert_eq!(
            burst.on_plain_char('y', at(base, 240)),
            BurstDecision::Typed
        );
        assert!(!burst.is_active(), "slow typing never opens a burst");
        assert_eq!(burst.flush_now(), None);
    }

    #[test]
    fn a_burst_flushes_only_after_the_idle_timeout() {
        let base = Instant::now();
        let mut burst = PasteBurst::new();
        let mut typed = String::new();
        for (index, c) in "log line".chars().enumerate() {
            match burst.on_plain_char(c, at(base, index as u64)) {
                BurstDecision::Typed => typed.push(c),
                BurstDecision::BeginBurst { retro_chars } => {
                    // The caller's half: move the already-typed prefix (ASCII
                    // here, so bytes and chars coincide) into the burst.
                    let split = typed.len() - retro_chars;
                    let prefix = typed[split..].to_string();
                    typed.truncate(split);
                    burst.absorb_retro(&prefix);
                }
                BurstDecision::Buffered => {}
            }
        }
        assert!(
            typed.is_empty(),
            "the retro-grab moved every typed character into the burst"
        );
        // Still warm: nothing to flush yet.
        assert_eq!(
            burst.flush_if_due(at(base, 10)),
            None,
            "the burst is still accumulating"
        );
        let idle = PASTE_BURST_ACTIVE_IDLE_TIMEOUT + Duration::from_millis(1);
        let now = at(base, 7) + idle;
        assert_eq!(burst.flush_if_due(now).as_deref(), Some("log line"));
        assert!(!burst.is_active());
        assert_eq!(burst.buffered(), "");
        // The window is spent: a later character starts a fresh run.
        let later = now + Duration::from_millis(50);
        assert_eq!(burst.on_plain_char('x', later), BurstDecision::Typed);
    }

    #[test]
    fn enter_inside_a_burst_is_a_newline_and_the_window_outlives_the_buffer() {
        let base = Instant::now();
        let mut burst = PasteBurst::new();
        let _ = burst.on_plain_char('a', at(base, 0));
        let _ = burst.on_plain_char('b', at(base, 1));
        assert_eq!(
            burst.on_plain_char('c', at(base, 2)),
            BurstDecision::BeginBurst { retro_chars: 2 }
        );
        burst.absorb_retro("ab");
        assert!(burst.append_control_if_active('\n', at(base, 3)));
        assert!(burst.newline_should_insert(at(base, 4)));
        let flushed = burst.flush_now().expect("the burst flushes");
        assert_eq!(flushed, "abc\n");
        // The suppress window is still open: a fast trailing Enter stays a
        // newline instead of submitting the paste. But a control character
        // never *starts* a burst, so it is rejected here.
        assert!(burst.newline_should_insert(at(base, 10)));
        assert!(!burst.append_control_if_active('\n', at(base, 10)));
        assert!(!burst.newline_should_insert(at(base, 500)));
    }

    #[test]
    fn abort_hands_every_buffered_character_back() {
        let base = Instant::now();
        let mut burst = PasteBurst::new();
        let _ = burst.on_plain_char('a', at(base, 0));
        let _ = burst.on_plain_char('b', at(base, 1));
        let _ = burst.on_plain_char('c', at(base, 2));
        let recovered = burst.abort();
        assert_eq!(recovered, "c", "the current char is never lost");
        assert!(!burst.is_active());
        assert_eq!(burst.buffered(), "");
    }

    #[test]
    fn the_flush_deadline_tracks_the_active_burst() {
        let base = Instant::now();
        let mut burst = PasteBurst::new();
        assert_eq!(burst.flush_deadline(), None);
        for (index, c) in "abc".chars().enumerate() {
            let _ = burst.on_plain_char(c, at(base, index as u64));
        }
        assert_eq!(
            burst.flush_deadline(),
            Some(at(base, 2) + PASTE_BURST_ACTIVE_IDLE_TIMEOUT)
        );
        burst.clear_window();
        assert!(burst.is_active(), "clearing the window keeps the buffer");
        assert_eq!(burst.flush_now_and_clear().as_deref(), Some("c"));
        assert_eq!(burst.flush_deadline(), None);
        assert!(!burst.newline_should_insert(base + Duration::from_secs(1)));
    }
}
