//! Terminal color parsing — 1:1 port of `packages/tui/src/terminal-colors.ts`.
//!
//! Two protocol-level helpers the host wires up when the terminal can
//! report its background colour and its colour scheme:
//!
//! * `is_osc11_background_color_response` / `parse_osc11_background_color`
//!   — the OSC 11 ("report default background colour") reply, in either
//!   `rgb:RR/GG/BB`, `rgb:RRRR/GGGG/BBBB`, or `#RRGGBB` / `#RRRRGGGGBBBB`
//!   form, optionally wrapped in the OSC framing (`ESC ] 11 ; ... BEL` or
//!   `ESC ] 11 ; ... ESC \\`).
//! * `parse_terminal_color_scheme_report` — the OSC 10/12 ("report default
//!   fg / bg") colour-scheme reply. Reports `dark` for `?1` and `light`
//!   for `?2`, mirroring upstream
//!   (`packages/tui/src/terminal-colors.ts:67-73`).
//!
//! All helpers are pure and operate on raw escape sequences; the driver
//! feeds them whatever it reads off the wire.

/// An RGB triple (sRGB, 8 bits per channel).
///
/// Upstream `RgbColor` (`packages/tui/src/terminal-colors.ts:1-5`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RgbColor {
    /// Red channel (0..=255).
    pub r: u8,
    /// Green channel (0..=255).
    pub g: u8,
    /// Blue channel (0..=255).
    pub b: u8,
}

/// Resolved terminal colour-scheme report.
///
/// Mirrors the upstream contract: an OSC 10/12 reply of `?1` means dark,
/// `?2` means light, and anything else is forwarded to the host as an
/// `Unknown(raw)` value so the driver can decide what to do
/// (`packages/tui/src/terminal-colors.ts:67-73`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalColorSchemeReport {
    /// Terminal reports a dark background.
    Dark,
    /// Terminal reports a light background.
    Light,
    /// Terminal reports something we cannot classify (forwarded).
    Unknown(String),
}

const OSC11_RESPONSE_PATTERN: &str = "\x1b]11;";
const COLOR_SCHEME_REPORT_PREFIXES: &[&str] = &["\x1b[?997;", "\x1b[?997;1n", "\x1b[?997;2n"];

fn strip_osc11_framing(input: &str) -> &str {
    let trimmed = input.trim();
    let payload = trimmed
        .strip_prefix("\x1b]11;")
        .or_else(|| trimmed.strip_prefix("11;"))
        .unwrap_or(trimmed);
    // Trim the BEL (`\x07`) or ST (`ESC \\`) terminator. ST is two bytes;
    // strip the trailing ESC if present so the BEL case stays intact.
    let payload = payload
        .strip_suffix("\x1b\\")
        .or_else(|| payload.strip_suffix("\x07"))
        .unwrap_or(payload);
    payload.trim_end_matches('\x1b').trim()
}

/// Returns `true` when `data` looks like an OSC 11 background-colour
/// reply (with or without the OSC framing). Mirrors upstream
/// `isOsc11BackgroundColorResponse`
/// (`packages/tui/src/terminal-colors.ts:31-33`).
pub fn is_osc11_background_color_response(data: &str) -> bool {
    data.contains(OSC11_RESPONSE_PATTERN) || data.starts_with("11;") || data.starts_with("rgb:") || data.starts_with('#')
}

fn hex_to_rgb(hex: &str) -> Option<RgbColor> {
    let normalized = hex.strip_prefix('#').unwrap_or(hex);
    if normalized.len() != 6 && normalized.len() != 12 {
        return None;
    }
    if !normalized.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    if normalized.len() == 6 {
        let r = u8::from_str_radix(&normalized[0..2], 16).ok()?;
        let g = u8::from_str_radix(&normalized[2..4], 16).ok()?;
        let b = u8::from_str_radix(&normalized[4..6], 16).ok()?;
        Some(RgbColor { r, g, b })
    } else {
        let r = parse_osc_hex_channel(&normalized[0..4])?;
        let g = parse_osc_hex_channel(&normalized[4..8])?;
        let b = parse_osc_hex_channel(&normalized[8..12])?;
        Some(RgbColor { r, g, b })
    }
}

fn parse_osc_hex_channel(channel: &str) -> Option<u8> {
    if channel.is_empty() || !channel.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let max = 16u32.pow(channel.len() as u32).saturating_sub(1);
    if max == 0 {
        return None;
    }
    let value = u32::from_str_radix(channel, 16).ok()?;
    Some(((value as u64 * 255) / max as u64) as u8)
}

/// Parse an OSC 11 background-colour reply into an [`RgbColor`].
///
/// Accepts the bare payload (`rgb:RR/GG/BB`, `rgb:RRRR/GGGG/BBBB`,
/// `#RRGGBB`, `#RRRRGGGGBBBB`) or the framed form
/// (`ESC ] 11 ; … BEL` / `ESC ] 11 ; … ST`). Mirrors upstream
/// `parseOsc11BackgroundColor`
/// (`packages/tui/src/terminal-colors.ts:35-65`).
pub fn parse_osc11_background_color(data: &str) -> Option<RgbColor> {
    let payload = strip_osc11_framing(data);

    if let Some(hex) = payload.strip_prefix('#') {
        return hex_to_rgb(hex);
    }

    if let Some(hex) = payload.strip_prefix("hex:") {
        return hex_to_rgb(hex);
    }

    // `rgb:RR/GG/BB` or `rgb:RRRR/GGGG/BBBB` — accept optional `rgba:`
    // / `rgb:` prefix.
    let rgb_value = payload
        .strip_prefix("rgba:")
        .or_else(|| payload.strip_prefix("rgb:"))
        .unwrap_or(payload);
    let mut parts = rgb_value.split('/');
    let r = parse_osc_hex_channel(parts.next()?.trim())?;
    let g = parse_osc_hex_channel(parts.next()?.trim())?;
    let b = parse_osc_hex_channel(parts.next()?.trim())?;
    Some(RgbColor { r, g, b })
}

/// Parse an OSC 10/12 colour-scheme report into a
/// [`TerminalColorSchemeReport`].
///
/// Mirrors upstream `parseTerminalColorSchemeReport`
/// (`packages/tui/src/terminal-colors.ts:67-73`): `?1` / `1` → dark,
/// `?2` / `2` → light, anything else is forwarded verbatim.
pub fn parse_terminal_color_scheme_report(data: &str) -> Option<TerminalColorSchemeReport> {
    let trimmed = data.trim();
    let stripped = trimmed
        .strip_suffix('\x07')
        .or_else(|| trimmed.strip_suffix("\x1b\\"))
        .unwrap_or(trimmed)
        .trim();
    let payload = stripped
        .strip_prefix("\x1b]12;")
        .or_else(|| stripped.strip_prefix("\x1b]10;"))
        .or_else(|| stripped.strip_prefix("12;"))
        .or_else(|| stripped.strip_prefix("10;"))
        .unwrap_or(stripped);

    // Upstream requires the response to consist only of CSI ?997 ; N n
    // repeats; we accept any payload that ends in ?1 / ?2 / 1 / 2 so a
    // raw terminal output (without the OSC framing) still resolves.
    for prefix in COLOR_SCHEME_REPORT_PREFIXES {
        if let Some(rest) = stripped.strip_prefix(prefix) {
            return Some(match rest {
                "1" => TerminalColorSchemeReport::Dark,
                "2" => TerminalColorSchemeReport::Light,
                other => TerminalColorSchemeReport::Unknown(other.to_string()),
            });
        }
    }

    Some(match payload {
        "?1" | "1" => TerminalColorSchemeReport::Dark,
        "?2" | "2" => TerminalColorSchemeReport::Light,
        other => TerminalColorSchemeReport::Unknown(other.to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osc11_response_pattern_matches_framed_form() {
        assert!(is_osc11_background_color_response("\x1b]11;rgb:12/34/56\x07"));
        assert!(is_osc11_background_color_response("\x1b]11;rgb:12/34/56\x1b\\"));
        assert!(is_osc11_background_color_response("11;rgb:12/34/56"));
    }

    #[test]
    fn osc11_response_pattern_matches_unframed_form() {
        assert!(is_osc11_background_color_response("rgb:12/34/56"));
        assert!(is_osc11_background_color_response("#123456"));
    }

    #[test]
    fn osc11_response_pattern_rejects_unrelated_input() {
        assert!(!is_osc11_background_color_response(""));
        assert!(!is_osc11_background_color_response("plain text"));
        assert!(!is_osc11_background_color_response("not-a-color"));
    }

    #[test]
    fn parse_osc11_rgb_two_digit() {
        let c = parse_osc11_background_color("rgb:ff/80/00").unwrap();
        assert_eq!(c, RgbColor { r: 0xff, g: 0x80, b: 0x00 });
    }

    #[test]
    fn parse_osc11_rgb_four_digit() {
        let c = parse_osc11_background_color("rgb:ffff/8080/0000").unwrap();
        assert_eq!(c, RgbColor { r: 0xff, g: 0x80, b: 0x00 });
    }

    #[test]
    fn parse_osc11_hex_six() {
        let c = parse_osc11_background_color("#ff8000").unwrap();
        assert_eq!(c, RgbColor { r: 0xff, g: 0x80, b: 0x00 });
    }

    #[test]
    fn parse_osc11_hex_twelve() {
        let c = parse_osc11_background_color("#ffff80800000").unwrap();
        assert_eq!(c, RgbColor { r: 0xff, g: 0x80, b: 0x00 });
    }

    #[test]
    fn parse_osc11_with_full_framing() {
        let c = parse_osc11_background_color("\x1b]11;rgb:12/34/56\x07").unwrap();
        assert_eq!(c, RgbColor { r: 0x12, g: 0x34, b: 0x56 });
    }

    #[test]
    fn parse_osc11_with_st_terminator() {
        let c = parse_osc11_background_color("\x1b]11;rgb:12/34/56\x1b\\").unwrap();
        assert_eq!(c, RgbColor { r: 0x12, g: 0x34, b: 0x56 });
    }

    #[test]
    fn parse_osc11_rgba_prefix() {
        let c = parse_osc11_background_color("rgba:ff/80/00").unwrap();
        assert_eq!(c, RgbColor { r: 0xff, g: 0x80, b: 0x00 });
    }

    #[test]
    fn parse_osc11_rejects_garbage() {
        assert!(parse_osc11_background_color("nope").is_none());
        assert!(parse_osc11_background_color("rgb:notanumber").is_none());
        assert!(parse_osc11_background_color("#xx").is_none());
    }

    #[test]
    fn color_scheme_report_dark() {
        let s = parse_terminal_color_scheme_report("\x1b]10;?1\x07").unwrap();
        assert_eq!(s, TerminalColorSchemeReport::Dark);
    }

    #[test]
    fn color_scheme_report_light() {
        let s = parse_terminal_color_scheme_report("\x1b]12;?2\x07").unwrap();
        assert_eq!(s, TerminalColorSchemeReport::Light);
    }

    #[test]
    fn color_scheme_report_unknown() {
        let s = parse_terminal_color_scheme_report("\x1b]10;?7\x07").unwrap();
        assert!(matches!(s, TerminalColorSchemeReport::Unknown(ref v) if v == "?7"));
    }

    #[test]
    fn color_scheme_report_unframed() {
        let s = parse_terminal_color_scheme_report("?2").unwrap();
        assert_eq!(s, TerminalColorSchemeReport::Light);
    }
}