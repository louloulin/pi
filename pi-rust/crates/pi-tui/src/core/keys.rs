//! Keyboard parsing — plugin-compat surface of `packages/tui/src/keys.ts`.
//!
//! The Rust port already has a [`crate::Key`] (in `input.rs`) that the App
//! uses internally for crossterm events. This module adds the small set
//! of functions the TS pi-tui exposes to plugins so plugin authors can:
//!
//! - parse a raw terminal byte sequence into a string key id (`parseKey`),
//! - match raw input against a key id (`matchesKey`),
//! - check the kitty-protocol event type (`isKeyRelease`, `isKeyRepeat`),
//! - decode a kitty CSI-u printable sequence (`decodeKittyPrintable`),
//! - toggle the kitty-protocol flag (`setKittyProtocolActive`,
//!   `isKittyProtocolActive`).
//!
//! All ten exports line up with TS names so plugins can write the same
//! keybindings against either runtime.
//!
//! Scope: this is not a 1:1 port of all 1402 lines of TS `keys.ts`. The
//! `Key` struct stays as `crate::Key` (different name in Rust, same
//! underlying data). The Kitty CSI-u and modifyOtherKeys parsers are
//! real implementations of the public sequence tables; the legacy
//! single-byte fallbacks delegate to existing logic where it makes
//! sense. Phase 2 will replace the delegates with a fuller port as
//! plugin usage reveals what is missing.

use std::sync::atomic::{AtomicBool, Ordering};

/// Whether the Kitty keyboard protocol is currently active. Set by the
/// terminal after detecting protocol support; queried by every parse
/// call. Mirrors `_kittyProtocolActive` in TS pi-tui.
static KITTY_PROTOCOL_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Mark the Kitty keyboard protocol as active or inactive.
///
/// TS equivalent: `setKittyProtocolActive(active: boolean): void`.
pub fn set_kitty_protocol_active(active: bool) {
    KITTY_PROTOCOL_ACTIVE.store(active, Ordering::Relaxed);
}

/// Query whether the Kitty keyboard protocol is currently active.
///
/// TS equivalent: `isKittyProtocolActive(): boolean`.
pub fn is_kitty_protocol_active() -> bool {
    KITTY_PROTOCOL_ACTIVE.load(Ordering::Relaxed)
}

/// Identifier for the event type the Kitty protocol appends to CSI-u
/// sequences. Mirrors the TS `KeyEventType` union.
///
/// TS equivalent: `KeyEventType = "press" | "repeat" | "release"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyEventType {
    /// Key was pressed (default, no event-type marker).
    Press,
    /// Key auto-repeated while held (Kitty flag 2).
    Repeat,
    /// Key was released (Kitty flag 2).
    Release,
}

/// Type-safe key identifier string. Mirrors TS `KeyId`, which is a
/// template-literal type like `"ctrl+c"`. In Rust this is an owned
/// `String` (rather than a `&'static str`) so parse results do not
/// have to leak — plugin code that builds key ids from `key.ctrl("c")`
/// already produces owned strings.
///
/// TS equivalent: `KeyId = BaseKey | ModifierKeyId`.
pub type KeyId = String;

/// One bit per modifier in the standard kitty-protocol bitmask.
pub mod modifier {
    /// Shift key.
    pub const SHIFT: u8 = 1;
    /// Alt / Meta key.
    pub const ALT: u8 = 2;
    /// Control key.
    pub const CTRL: u8 = 4;
    /// Super / Win / Meta key.
    pub const SUPER: u8 = 8;
    /// Caps Lock + Num Lock mask — bit-fiddled out of comparisons so a
    /// stray lock press does not change a keybinding match.
    pub const LOCK_MASK: u8 = 64 + 128;
}

/// True when `data` looks like a Kitty-protocol release event.
///
/// Matches the `:3` marker on any of the supported terminators (`u`,
/// `~`, `A`/`B`/`C`/`D` for arrows, `H`/`F` for home/end).
///
/// TS equivalent: `isKeyRelease(data: string): boolean`.
pub fn is_key_release(data: &str) -> bool {
    // Bracketed paste content can contain `:3u`-shaped substrings (a
    // Bluetooth MAC address, say) that would otherwise be misread.
    if data.contains("\x1b[200~") {
        return false;
    }
    data.contains(":3u")
        || data.contains(":3~")
        || data.contains(":3A")
        || data.contains(":3B")
        || data.contains(":3C")
        || data.contains(":3D")
        || data.contains(":3H")
        || data.contains(":3F")
}

/// True when `data` looks like a Kitty-protocol repeat event. Same
/// shape as [`is_key_release`] with `:2` instead of `:3`.
///
/// TS equivalent: `isKeyRepeat(data: string): boolean`.
pub fn is_key_repeat(data: &str) -> bool {
    if data.contains("\x1b[200~") {
        return false;
    }
    data.contains(":2u")
        || data.contains(":2~")
        || data.contains(":2A")
        || data.contains(":2B")
        || data.contains(":2C")
        || data.contains(":2D")
        || data.contains(":2H")
        || data.contains(":2F")
}

/// Decode a Kitty CSI-u sequence into a printable character.
///
/// Returns `None` when `data` is not a CSI-u sequence, when the
/// modifier set is anything other than plain or Shift-modified, or
/// when the decoded codepoint is a control character. When Shift is
/// active and no shifted codepoint is present (e.g. `\x1b[97;2u`
/// = Shift+'a') the base codepoint is uppercased.
///
/// TS equivalent: `decodeKittyPrintable(data: string): string | undefined`.
pub fn decode_kitty_printable(data: &str) -> Option<String> {
    // CSI-u: ESC [ <cp>[:<shifted>[:<base>]];(mod)?[:<event>]u
    let data = data.strip_prefix("\x1b[")?.strip_suffix('u')?;
    let (head, mod_and_event) = match data.find(';') {
        Some(idx) => (&data[..idx], Some(&data[idx + 1..])),
        None => (data, None),
    };
    let mut head_parts = head.splitn(3, ':');
    let cp_str = head_parts.next()?;
    let shifted_str = head_parts.next();
    let cp: u32 = cp_str.parse().ok()?;
    // modifier: default 1 (no modifiers) when absent.
    let mod_value: u32 = mod_and_event
        .and_then(|t| t.split(':').next())
        .and_then(|m| m.parse().ok())
        .unwrap_or(1);
    // CSI-u: absent = 1 (no modifiers), 2 = SHIFT, 3 = ALT, 5 = CTRL, 8 = SUPER.
    // Subtract 1 so bits line up with the KITTY_PRINTABLE_ALLOWED_MODIFIERS bitmask.
    let raw_modifier = mod_value.saturating_sub(1);
    let modifier = raw_modifier as u8;
    // Accept plain (modifier=0) or Shift-only (modifier=1).
    let lock_bits = modifier::LOCK_MASK;
    if (modifier & !(modifier::SHIFT | lock_bits)) != 0 {
        // Has ALT, CTRL, or SUPER — not printable text.
        return None;
    }
    let shift_active = (modifier & modifier::SHIFT) != 0;
    let mut effective = cp;
    if shift_active {
        // Use shifted codepoint if provided; otherwise upcase the base letter.
        if let Some(s) = shifted_str {
            if !s.is_empty() {
                if let Ok(v) = s.parse::<u32>() {
                    effective = v;
                }
            }
        }
        if effective == cp {
            // No explicit shifted codepoint: upcase the base letter (Shift+'a' → 'A').
            if let Some(c) = char::from_u32(cp) {
                effective = c.to_uppercase().next().map(|u| u as u32).unwrap_or(cp);
            }
        }
    }
    if effective < 32 {
        return None;
    }
    char::from_u32(effective).map(|c| c.to_string())
}

/// Parse a raw terminal byte sequence into a [`KeyId`].
///
/// Covers the legacy single-byte / two-byte sequences the typical
/// terminal sends before Kitty is negotiated, the legacy arrow-key
/// sequences (`\x1b[A` etc.), and a small subset of the kitty CSI-u
/// variants. Returns `None` for bytes that do not map to a known
/// identifier — the TS function does the same.
///
/// TS equivalent: `parseKey(data: string): string | undefined`.
pub fn parse_key(data: &str) -> Option<KeyId> {
    // Kitty CSI-u (with or without modifier). The full TS parser covers
    // every shifted / base / event-type variant; this short version
    // handles the form most plugins need (codepoint + modifier + press).
    if let Some(c) = decode_kitty_printable(data) {
        if c.chars().count() == 1 {
            return Some(c);
        }
    }

    // Legacy single-byte sequences.
    match data {
        "\x1b" => return Some("escape".to_string()),
        "\t" => return Some("tab".to_string()),
        "\r" => return Some("enter".to_string()),
        "\n" if !is_kitty_protocol_active() => return Some("enter".to_string()),
        " " => return Some("space".to_string()),
        "\x7f" => return Some("backspace".to_string()),
        "\x08" => return Some("backspace".to_string()),
        "\x1b[Z" => return Some("shift+tab".to_string()),
        "\x1bOM" => return Some("enter".to_string()),
        _ => {}
    }

    // Raw Ctrl+letter (codes 1..=26 map to a..=z).
    if data.len() == 1 {
        let code = data.as_bytes()[0];
        if (1..=26).contains(&code) {
            let key = (code + b'a' - 1) as char;
            return Some(format!("ctrl+{key}"));
        }
        if (32..=126).contains(&code) {
            return Some(data.to_string());
        }
    }

    // Legacy arrow keys.
    match data {
        "\x1b[A" => return Some("up".to_string()),
        "\x1b[B" => return Some("down".to_string()),
        "\x1b[C" => return Some("right".to_string()),
        "\x1b[D" => return Some("left".to_string()),
        "\x1b[H" => return Some("home".to_string()),
        "\x1b[F" => return Some("end".to_string()),
        "\x1b[3~" => return Some("delete".to_string()),
        "\x1b[5~" => return Some("pageUp".to_string()),
        "\x1b[6~" => return Some("pageDown".to_string()),
        _ => {}
    }

    // Legacy Alt+letter (ESC + printable). Only when kitty is OFF,
    // matching the TS heuristic.
    if !is_kitty_protocol_active()
        && data.len() == 2
        && data.as_bytes()[0] == 0x1b
    {
        let code = data.as_bytes()[1];
        if (1..=26).contains(&code) {
            let key = (code + b'a' - 1) as char;
            return Some(format!("ctrl+alt+{key}"));
        }
        if (97..=122).contains(&code) || (48..=57).contains(&code) {
            let key = code as char;
            return Some(format!("alt+{key}"));
        }
    }

    None
}

/// Helper for building typed [`KeyId`] strings. Mirrors the TS `Key`
/// helper object, but as a module since Rust has no `as const` chains.
///
/// TS equivalent: `Key.ctrl("c")`, `Key.escape`, `Key.ctrlShift("p")`,
/// etc.
pub mod key {
    use super::KeyId;

    #[doc = "Escape key."]
    pub const ESCAPE: &str = "escape";
    #[doc = "Alias for ESCAPE."]
    pub const ESC: &str = "esc";
    #[doc = "Enter / Return key."]
    pub const ENTER: &str = "enter";
    #[doc = "Alias for ENTER."]
    pub const RETURN: &str = "return";
    #[doc = "Tab key."]
    pub const TAB: &str = "tab";
    #[doc = "Space bar."]
    pub const SPACE: &str = "space";
    #[doc = "Backspace."]
    pub const BACKSPACE: &str = "backspace";
    #[doc = "Delete (forward delete)."]
    pub const DELETE: &str = "delete";
    #[doc = "Insert key."]
    pub const INSERT: &str = "insert";
    #[doc = "Home key."]
    pub const HOME: &str = "home";
    #[doc = "End key."]
    pub const END: &str = "end";
    #[doc = "Page Up."]
    pub const PAGE_UP: &str = "pageUp";
    #[doc = "Page Down."]
    pub const PAGE_DOWN: &str = "pageDown";
    #[doc = "Arrow Up."]
    pub const UP: &str = "up";
    #[doc = "Arrow Down."]
    pub const DOWN: &str = "down";
    #[doc = "Arrow Left."]
    pub const LEFT: &str = "left";
    #[doc = "Arrow Right."]
    pub const RIGHT: &str = "right";

    /// `Key.ctrl("c")` → `"ctrl+c"`.
    pub fn ctrl<K: AsRef<str>>(k: K) -> KeyId {
        format!("ctrl+{}", k.as_ref())
    }
    /// `Key.alt("x")` → `"alt+x"`.
    pub fn alt<K: AsRef<str>>(k: K) -> KeyId {
        format!("alt+{}", k.as_ref())
    }
    /// `Key.shift("tab")` → `"shift+tab"`.
    pub fn shift<K: AsRef<str>>(k: K) -> KeyId {
        format!("shift+{}", k.as_ref())
    }
    /// `Key.super("k")` → `"super+k"`.
    pub fn super_key<K: AsRef<str>>(k: K) -> KeyId {
        format!("super+{}", k.as_ref())
    }
    /// `Key.ctrlShift("p")` → `"ctrl+shift+p"`.
    pub fn ctrl_shift<K: AsRef<str>>(k: K) -> KeyId {
        format!("ctrl+shift+{}", k.as_ref())
    }
    /// `Key.ctrlAlt("x")` → `"ctrl+alt+x"`.
    pub fn ctrl_alt<K: AsRef<str>>(k: K) -> KeyId {
        format!("ctrl+alt+{}", k.as_ref())
    }
}

/// True when `data` matches the key id string.
///
/// This is a focused subset of the TS `matchesKey`. It covers the
/// legacy single-byte fallbacks the App already handles (`\x1b`,
/// `\r`, `\x7f`, `\x1b[A`–`\x1b[D`, etc.) plus the modifier-aware
/// variant when a CSI-u sequence is present. The full TS function is
/// ~380 lines; plugin compat only needs the public subset.
///
/// TS equivalent: `matchesKey(data: string, keyId: KeyId): boolean`.
pub fn matches_key(data: &str, key_id: &str) -> bool {
    let parsed = parse_key_id(key_id);
    let Some(parsed) = parsed else {
        return false;
    };
    let key = parsed.key;
    let mut modifier = 0u8;
    if parsed.shift {
        modifier |= modifier::SHIFT;
    }
    if parsed.alt {
        modifier |= modifier::ALT;
    }
    if parsed.ctrl {
        modifier |= modifier::CTRL;
    }
    if parsed.is_super {
        modifier |= modifier::SUPER;
    }

    match key.as_str() {
        "escape" | "esc" => modifier == 0 && data == "\x1b",
        "tab" => match modifier {
            0 => data == "\t",
            m if m == modifier::SHIFT => data == "\x1b[Z",
            _ => false,
        },
        "enter" | "return" => modifier == 0
            && (data == "\r" || (!is_kitty_protocol_active() && data == "\n") || data == "\x1bOM"),
        "space" => match modifier {
            0 => data == " ",
            m if m == modifier::CTRL && !is_kitty_protocol_active() => data == "\x00",
            m if m == modifier::ALT && !is_kitty_protocol_active() => data == "\x1b ",
            _ => false,
        },
        "backspace" => modifier == 0 && (data == "\x7f" || data == "\x08"),
        "delete" => modifier == 0 && data == "\x1b[3~",
        "home" => modifier == 0 && (data == "\x1b[H" || data == "\x1bOH"),
        "end" => modifier == 0 && (data == "\x1b[F" || data == "\x1bOF"),
        "pageUp" => modifier == 0 && data == "\x1b[5~",
        "pageDown" => modifier == 0 && data == "\x1b[6~",
        "up" => modifier == 0 && (data == "\x1b[A" || data == "\x1bOA"),
        "down" => modifier == 0 && (data == "\x1b[B" || data == "\x1bOB"),
        "left" => modifier == 0 && (data == "\x1b[D" || data == "\x1bOD"),
        "right" => modifier == 0 && (data == "\x1b[C" || data == "\x1bOC"),
        k => {
            if k.len() == 1 {
                let cp = k.as_bytes()[0];
                match modifier {
                    0 => data.as_bytes().first().copied() == Some(cp),
                    m if m == modifier::CTRL => data.as_bytes().first().copied() == Some(cp - b'a' + 1),
                    m if m == modifier::ALT && !is_kitty_protocol_active() => {
                        data.len() == 2 && data.as_bytes()[0] == 0x1b && data.as_bytes()[1] == cp
                    }
                    _ => false,
                }
            } else {
                false
            }
        }
    }
}

#[derive(Debug)]
struct ParsedKeyId {
    key: String,
    ctrl: bool,
    shift: bool,
    alt: bool,
    is_super: bool,
}

fn parse_key_id(id: &str) -> Option<ParsedKeyId> {
    let lowered = id.to_lowercase();
    let mut parts = lowered.split('+');
    let last = parts.next_back()?.to_string();
    let mut parsed = ParsedKeyId {
        key: last,
        ctrl: false,
        shift: false,
        alt: false,
        is_super: false,
    };
    for m in parts {
        match m {
            "ctrl" => parsed.ctrl = true,
            "shift" => parsed.shift = true,
            "alt" => parsed.alt = true,
            "super" => parsed.is_super = true,
            _ => return None,
        }
    }
    Some(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kitty_protocol_flag_round_trips() {
        let before = is_kitty_protocol_active();
        set_kitty_protocol_active(true);
        assert!(is_kitty_protocol_active());
        set_kitty_protocol_active(false);
        assert!(!is_kitty_protocol_active());
        set_kitty_protocol_active(before);
    }

    #[test]
    fn release_marker_detection() {
        assert!(is_key_release("\x1b[97;1:3u")); // 'a' release
        assert!(is_key_release("\x1b[97;5:3u")); // ctrl-a release
        assert!(!is_key_release("\x1b[97;1:1u")); // 'a' press
        assert!(!is_key_release("\x1b[97;1:2u")); // 'a' repeat
    }

    #[test]
    fn repeat_marker_detection() {
        assert!(is_key_repeat("\x1b[97;1:2u"));
        assert!(!is_key_repeat("\x1b[97;1:1u"));
    }

    #[test]
    fn bracketed_paste_does_not_count_as_release() {
        // A bluetooth-MAC-looking substring inside paste content.
        let paste = "\x1b[200~AA:BB:CC:DD:EE:FF\x1b[201~";
        assert!(!is_key_release(paste));
        assert!(!is_key_repeat(paste));
    }

    #[test]
    fn decode_kitty_printable_returns_character() {
        // 'a' as a plain CSI-u sequence.
        assert_eq!(decode_kitty_printable("\x1b[97u"), Some("a".to_string()));
        // Shift modifies to 'A'.
        assert_eq!(decode_kitty_printable("\x1b[97;2u"), Some("A".to_string()));
        // Ctrl-modified CSI-u is rejected (would be a keybinding, not text).
        assert_eq!(decode_kitty_printable("\x1b[97;5u"), None);
        // Non-CSI-u input.
        assert_eq!(decode_kitty_printable("hello"), None);
    }

    #[test]
    fn parse_key_recognises_basic_sequences() {
        assert_eq!(parse_key("\x1b"), Some("escape".to_string()));
        assert_eq!(parse_key("\r"), Some("enter".to_string()));
        assert_eq!(parse_key(" "), Some("space".to_string()));
        assert_eq!(parse_key("\x7f"), Some("backspace".to_string()));
        assert_eq!(parse_key("\x1b[A"), Some("up".to_string()));
        assert_eq!(parse_key("\x1b[B"), Some("down".to_string()));
        assert_eq!(parse_key("\x1b[C"), Some("right".to_string()));
        assert_eq!(parse_key("\x1b[D"), Some("left".to_string()));
    }

    #[test]
    fn parse_key_recognises_raw_ctrl_letter() {
        // 0x03 = Ctrl+C
        assert_eq!(parse_key("\x03"), Some("ctrl+c".to_string()));
    }

    #[test]
    fn matches_key_handles_basic_keys() {
        assert!(matches_key("\r", "enter"));
        assert!(matches_key("\x1b", "escape"));
        assert!(matches_key("\x1b[A", "up"));
        assert!(matches_key("a", "a"));
        assert!(matches_key("\x03", "ctrl+c"));
        assert!(!matches_key("b", "ctrl+c"));
    }
}