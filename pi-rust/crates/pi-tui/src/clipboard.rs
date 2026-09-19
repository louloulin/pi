//! Clipboard plumbing for the alternate-screen TUI.
//!
//! The [`App`](crate::App) owns no terminal handle: when copy-on-select
//! finishes a text selection it hands the text to the driver through
//! [`App::take_clipboard_request`](crate::App::take_clipboard_request),
//! and the driver decides how to put it on the system clipboard. This
//! module implements the default upstream uses — an OSC 52 write
//! (`packages/tui/src/tui-alt-screen.ts:1459`):
//!
//! ```text
//! ESC ] 52 ; c ; <base64 of the UTF-8 text> BEL
//! ```
//!
//! A host that has a native clipboard bridge can ignore this and copy the
//! text itself; upstream keeps the same seam through its injectable
//! `copySelection` callback
//! (`packages/tui/src/tui-alt-screen.ts:1449-1462`).
//!
//! OSC 52 is best-effort by design: terminals that route it to the system
//! clipboard (Kitty, iTerm2, Windows Terminal, WezTerm, tmux with
//! `set-clipboard on`, …) make it work, while others (notably macOS
//! Terminal.app) print "Copied!" but leave the clipboard untouched. That
//! is the same trade-off upstream documents, and why it lets hosts inject
//! their own implementation.

/// Base64 alphabet (RFC 4648 §4), the only encoding OSC 52 accepts.
const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Base64-encode `bytes` (RFC 4648, with `=` padding).
///
/// Hand-rolled rather than pulling in a base64 crate: the encoder is 15
/// lines, has no configuration surface, and avoids a dependency in a crate
/// whose other needs are all met by the standard library.
pub fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(BASE64[(triple >> 18) as usize & 0x3f] as char);
        out.push(BASE64[(triple >> 12) as usize & 0x3f] as char);
        if chunk.len() > 1 {
            out.push(BASE64[(triple >> 6) as usize & 0x3f] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(BASE64[triple as usize & 0x3f] as char);
        } else {
            out.push('=');
        }
    }
    out
}

/// The OSC 52 escape sequence that puts `text` on the system clipboard
/// (the `c` selection, i.e. the standard clipboard).
///
/// Non-ASCII text is UTF-8 encoded before base64, so emoji and CJK content
/// round-trips through terminals that support the sequence.
pub fn osc52_sequence(text: &str) -> String {
    format!("\x1b]52;c;{}\x07", base64_encode(text.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_rfc4648_vectors() {
        // RFC 4648 §10 test vectors, including all three padding cases.
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn base64_encodes_utf8_bytes_not_code_points() {
        // "你好" is 6 UTF-8 bytes → 8 base64 chars, no padding.
        assert_eq!(base64_encode("你好".as_bytes()), "5L2g5aW9");
        assert_eq!(base64_encode("🙂".as_bytes()), "8J+Zgg==");
    }

    #[test]
    fn osc52_wraps_the_base64_payload_in_the_escape_sequence() {
        assert_eq!(osc52_sequence("hi"), "\x1b]52;c;aGk=\x07");
    }
}
