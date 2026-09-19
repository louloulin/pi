//! OSC 8 terminal hyperlinks — the port of `hyperlink()` and the
//! hyperlink half of `detectCapabilities()` from
//! `packages/tui/src/terminal-image.ts`.
//!
//! An OSC 8 sequence is zero-width for every terminal: a hyperlink-capable
//! terminal turns the enclosed cells into a clickable link and a terminal
//! that does not understand OSC 8 (or has it stripped, e.g. by `tmux` or
//! `screen`) renders the enclosed text unchanged. The sequences therefore
//! must never reach the crate's width or selection math — this module keeps
//! them outside the visible text and exposes [`visible_width`] /
//! [`strip_ansi`] for callers that hold an already-rendered string.
//!
//! Deliberate deviation from upstream: `detectCapabilities` probes `tmux` by
//! running `tmux display-message -p '#{client_termfeatures}'` with a 250 ms
//! timeout. `pi-tui` owns no process handle and must not spawn one from the
//! render path, so tmux/screen default to *no* hyperlinks unless
//! `PI_HYPERLINKS=1` overrides the detection. The override is the same
//! escape hatch upstream documents.

use std::sync::OnceLock;

/// The 8-bit C1 "string terminator" (ST) OSC 8 uses to close a sequence.
pub const OSC8_ST: &str = "\u{1b}\\";

/// Open an OSC 8 hyperlink to `url`.
///
/// Mirrors `` `\x1b]8;;${url}\x1b\\` `` in
/// `packages/tui/src/terminal-image.ts:665`.
pub fn open_hyperlink(url: &str) -> String {
    format!("\u{1b}]8;;{url}{OSC8_ST}")
}

/// Close the active OSC 8 hyperlink.
///
/// Mirrors `` `\x1b]8;;\x1b\\` `` in
/// `packages/tui/src/terminal-image.ts:665`.
pub fn close_hyperlink() -> String {
    format!("\u{1b}]8;;{OSC8_ST}")
}

/// Wrap `text` in an OSC 8 hyperlink to `url`.
///
/// Upstream's `hyperlink(text, url)` (`packages/tui/src/terminal-image.ts:665`)
/// is byte-for-byte this sequence. The returned string is wider than `text`
/// on the wire but exactly as wide on screen: [`visible_width`] agrees with
/// `text.chars().count()`.
pub fn hyperlink(text: &str, url: &str) -> String {
    let mut out = open_hyperlink(url);
    out.push_str(text);
    out.push_str(&close_hyperlink());
    out
}

/// Whether the terminal is known to render OSC 8 hyperlinks.
///
/// The result is detected once per process from the environment, mirroring
/// upstream's cached `getCapabilities().hyperlinks`. Tests and callers that
/// need a specific value should bypass this and set the capability on the
/// component (see `AppConfig::hyperlinks`).
pub fn supports_hyperlinks() -> bool {
    static SUPPORT: OnceLock<bool> = OnceLock::new();
    *SUPPORT.get_or_init(detect_hyperlinks_from_env)
}

/// The uncached form of [`supports_hyperlinks`].
///
/// Exposed so a caller that owns the capability decision (the TUI driver)
/// can re-evaluate it, and so tests can exercise both branches without
/// mutating process-global state.
pub fn detect_hyperlinks_from_env() -> bool {
    fn env_string(key: &str) -> Option<String> {
        std::env::var_os(key).and_then(|v| v.into_string().ok())
    }
    let value = env_string;
    detect_hyperlinks(
        value("PI_HYPERLINKS").as_deref(),
        value("TERM_PROGRAM").as_deref(),
        value("TERMINAL_EMULATOR").as_deref(),
        value("TERM").as_deref(),
        value("TMUX").is_some(),
        value("KITTY_WINDOW_ID").is_some(),
        value("GHOSTTY_RESOURCES_DIR").is_some(),
        value("WEZTERM_PANE").is_some(),
        value("WARP_SESSION_ID").as_deref(),
        value("ITERM_SESSION_ID").is_some(),
        value("WT_SESSION").is_some(),
    )
}

/// The capability rule, with the environment already read.
///
/// `PI_HYPERLINKS` accepts `"1"` / `"0"` and wins over detection. The
/// remaining inputs are the lowercased `TERM_PROGRAM` / `TERMINAL_EMULATOR`
/// / `TERM` values plus the presence flags upstream tests; the order matches
/// `detectCapabilitiesFromEnvironment`
/// (`packages/tui/src/terminal-image.ts:71-133`).
#[allow(clippy::too_many_arguments)]
fn detect_hyperlinks(
    override_value: Option<&str>,
    term_program: Option<&str>,
    terminal_emulator: Option<&str>,
    term: Option<&str>,
    in_tmux: bool,
    kitty_window: bool,
    ghostty_resources: bool,
    wezterm_pane: bool,
    warp_session: Option<&str>,
    iterm_session: bool,
    wt_session: bool,
) -> bool {
    match override_value {
        Some("1") => return true,
        Some("0") => return false,
        _ => {}
    }
    let term_program = term_program.unwrap_or("").to_lowercase();
    let terminal_emulator = terminal_emulator.unwrap_or("").to_lowercase();
    let term = term.unwrap_or("").to_lowercase();

    // tmux strips OSC 8 unless the attached client forwards it; without a
    // subprocess probe (see the module docs) the safe default is "off".
    if in_tmux || term.starts_with("tmux") {
        return false;
    }
    // screen does not forward OSC 8.
    if term.starts_with("screen") {
        return false;
    }
    if kitty_window || term_program == "kitty" {
        return true;
    }
    if term_program == "ghostty" || term.contains("ghostty") || ghostty_resources {
        return true;
    }
    if wezterm_pane || term_program == "wezterm" {
        return true;
    }
    if term_program == "warpterminal" || warp_session.is_some() {
        return true;
    }
    if iterm_session || term_program == "iterm.app" {
        return true;
    }
    // Windows Terminal does not always set WT_SESSION, but when it does the
    // console forwards OSC 8.
    if wt_session {
        return true;
    }
    if matches!(term_program.as_str(), "alacritty" | "vscode" | "zed") {
        return true;
    }
    if terminal_emulator == "jetbrains-jediterm" {
        return false;
    }
    // Unknown terminal: be conservative. A terminal that swallows OSC 8
    // would render only the link text and silently drop the URL, which is
    // worse than the plain `label (url)` fallback.
    false
}

/// Strip ANSI escape sequences (CSI and OSC) from `text`.
///
/// The returned string is what a terminal actually paints. OSC 8
/// open/close sequences and SGR colour codes both disappear.
pub fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] != 0x1b {
            let ch = text[i..].chars().next().expect("valid char boundary");
            out.push(ch);
            i += ch.len_utf8();
            continue;
        }
        match bytes.get(i + 1) {
            // CSI: ESC [ ... final byte in 0x40..=0x7e.
            Some(b'[') => {
                i += 2;
                while i < bytes.len() && !(0x40..=0x7e).contains(&bytes[i]) {
                    i += 1;
                }
                i = (i + 1).min(bytes.len());
            }
            // OSC: terminated by BEL or ST (ESC \).
            Some(b']') => {
                i += 2;
                while i < bytes.len() {
                    if bytes[i] == 0x07 {
                        i += 1;
                        break;
                    }
                    if bytes[i] == 0x1b && bytes.get(i + 1) == Some(&b'\\') {
                        i += 2;
                        break;
                    }
                    i += 1;
                }
            }
            // A lone ESC, or an unsupported escape: drop the ESC byte and
            // let the rest render as text (fail open, never panic).
            Some(_) => i += 1,
            None => i += 1,
        }
    }
    out
}

/// Display width of `text` with all ANSI / OSC sequences removed.
///
/// Counts characters, matching `pi-tui`'s crate-wide width convention
/// (`message.rs` / `selector.rs` / `markdown.rs`): a wide glyph counts as
/// one column.
pub fn visible_width(text: &str) -> usize {
    strip_ansi(text).chars().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hyperlink_matches_upstream_bytes() {
        assert_eq!(
            hyperlink("pi", "https://example.com"),
            "\u{1b}]8;;https://example.com\u{1b}\\pi\u{1b}]8;;\u{1b}\\"
        );
        assert_eq!(
            open_hyperlink("https://x.dev"),
            "\u{1b}]8;;https://x.dev\u{1b}\\"
        );
        assert_eq!(close_hyperlink(), "\u{1b}]8;;\u{1b}\\");
    }

    #[test]
    fn osc8_sequences_are_zero_width() {
        let linked = hyperlink("read me", "https://example.com/a b");
        assert_eq!(visible_width(&linked), "read me".chars().count());
        assert_eq!(strip_ansi(&linked), "read me");
    }

    #[test]
    fn strip_ansi_handles_sgr_csi_and_bel_terminated_osc() {
        let sgr = "\u{1b}[38;5;12mblue\u{1b}[39m";
        assert_eq!(strip_ansi(sgr), "blue");
        // BEL-terminated OSC 8 is unusual but valid; the parser must not eat
        // the visible text after it.
        let bel = "\u{1b}]8;;https://x.dev\u{07}link\u{1b}]8;;\u{07}";
        assert_eq!(strip_ansi(bel), "link");
        assert_eq!(visible_width(bel), 4);
    }

    #[test]
    fn detection_prefers_the_pi_hyperlinks_override() {
        // Override wins over a terminal that would otherwise be "off".
        assert!(detect_hyperlinks(
            Some("1"),
            Some("xterm"),
            None,
            Some("xterm"),
            true,
            false,
            false,
            false,
            None,
            false,
            false,
        ));
        assert!(!detect_hyperlinks(
            Some("0"),
            Some("iTerm.app"),
            None,
            Some("xterm-256color"),
            false,
            true,
            false,
            false,
            None,
            true,
            false,
        ));
    }

    #[test]
    fn detection_matches_upstream_known_terminals() {
        let off = |program: &str, term: &str| {
            detect_hyperlinks(
                None,
                Some(program),
                None,
                Some(term),
                false,
                false,
                false,
                false,
                None,
                false,
                false,
            )
        };
        assert!(off("kitty", "xterm-kitty"));
        assert!(off("ghostty", "xterm-ghostty"));
        assert!(off("wezterm", "xterm-256color"));
        assert!(off("iTerm.app", "xterm-256color"));
        assert!(off("vscode", "xterm-256color"));
        // tmux / screen are stripped, and unknown terminals fail closed.
        assert!(!off("xterm", "tmux-256color"));
        assert!(!off("xterm", "screen-256color"));
        assert!(!off("xterm", "xterm-256color"));
    }
}
