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
    /// A mouse-wheel notch.
    ///
    /// The wheel has its own variant (upstream routes it through
    /// `routeWheel`, not through the component mouse dispatch): the
    /// alternate-screen [`App`](crate::App) maps it onto the chat-log
    /// viewport, one line per notch with an Alt multiplier of five
    /// (`packages/tui/src/tui-alt-screen.ts:968-984`).
    Mouse {
        /// True when the wheel moved towards older output (up), false when
        /// it moved towards the tail (down).
        up: bool,
        /// True when Alt was held. Upstream multiplies the step by the
        /// alt-wheel multiplier in that case
        /// (`packages/tui/src/tui-alt-screen.ts:968-971`).
        alt: bool,
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

impl From<CtKeyEvent> for InputEvent {
    fn from(event: CtKeyEvent) -> Self {
        let code = match event.code {
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

    /// Convenience constructor for a wheel notch.
    pub const fn wheel(up: bool, alt: bool) -> Self {
        InputEvent::Mouse { up, alt }
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
        (4, false) => return InputEvent::Mouse { up: true, alt },
        (5, false) => return InputEvent::Mouse { up: false, alt },
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
    fn wheel_constructor_sets_direction_and_modifier() {
        assert_eq!(
            InputEvent::wheel(true, false),
            InputEvent::Mouse {
                up: true,
                alt: false
            }
        );
        assert_eq!(
            InputEvent::wheel(false, true),
            InputEvent::Mouse {
                up: false,
                alt: true
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
        assert_eq!(
            parse_mouse_sequence(b"\x1b[<64;5;5M"),
            Some(InputEvent::Mouse {
                up: true,
                alt: false
            })
        );
        assert_eq!(
            parse_mouse_sequence(b"\x1b[<65;5;5M"),
            Some(InputEvent::Mouse {
                up: false,
                alt: false
            })
        );
        assert_eq!(
            parse_mouse_sequence(b"\x1b[<72;5;5M"),
            Some(InputEvent::Mouse {
                up: true,
                alt: true
            })
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
            Some(InputEvent::Mouse {
                up: true,
                alt: false
            })
        );
        assert_eq!(
            parse_mouse_sequence(b"\x1b[M\x61\x2b\x26"),
            Some(InputEvent::Mouse {
                up: false,
                alt: false
            })
        );
        assert_eq!(
            parse_mouse_sequence(b"\x1b[M\x68\x2b\x26"),
            Some(InputEvent::Mouse {
                up: true,
                alt: true
            })
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
}
