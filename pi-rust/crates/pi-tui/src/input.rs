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

/// Component-level input event. New variants can be added without
/// breaking the [`From`] conversions below — unknown crossterm events
/// collapse into [`InputEvent::Ignored`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputEvent {
    /// A key event.
    Key(Key),
    /// A mouse-wheel notch.
    ///
    /// Only the wheel is modelled: it is the one mouse input the
    /// alternate-screen [`App`](crate::App) consumes, mapping it onto the
    /// chat-log viewport. Everything else (moves, clicks, drags) stays
    /// [`InputEvent::Ignored`].
    Mouse {
        /// True when the wheel moved towards older output (up), false when
        /// it moved towards the tail (down).
        up: bool,
        /// True when Alt was held. Upstream multiplies the step by the
        /// alt-wheel multiplier in that case
        /// (`packages/tui/src/tui-alt-screen.ts:968-971`).
        alt: bool,
    },
    /// Resize — the [`App`](crate::App) re-flows the layout and redraws.
    Resize {
        /// New width in columns.
        width: u16,
        /// New height in rows.
        height: u16,
    },
    /// Anything we cannot map to a component-level event (mouse moves,
    /// focus reports, …). Components treat it as a no-op.
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
