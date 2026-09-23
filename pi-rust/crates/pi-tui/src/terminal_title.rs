//! Terminal window/tab title — the port of upstream's `Terminal.setTitle`
//! (`packages/tui/src/terminal.ts:520-523`) plus the automatic title the
//! interactive mode derives from the session.
//!
//! Upstream writes the title as **OSC 0**, terminated by BEL:
//!
//! ```ts
//! setTitle(title: string): void {
//!     // OSC 0;title BEL - set terminal window title
//!     process.stdout.write(`\x1b]0;${title}\x07`);
//! }
//! ```
//!
//! Two callers reach it:
//!
//! * `ctx.ui.setTitle(title)` — a plugin owns the title for as long as it
//!   keeps setting it (`interactive-mode.ts:2443`), and
//! * the interactive mode's own `updateTerminalTitle()`
//!   (`interactive-mode.ts:1017-1028`), which composes
//!   `"<APP_TITLE> - <session name> - <cwd basename>"` (dropping the session
//!   name when the session has none) on init, on `/new`, on `/resume`, on
//!   `/name` and on `session_info_changed`.
//!
//! This module owns the wire format and the composition. `pi-tui` never
//! touches stdout: the App only *queues* a title and the driver writes it
//! ([`crate::App::take_terminal_title`]), which keeps the sequence out of the
//! frame buffer and lets tests assert both halves separately.
//!
//! Deliberate deviations from upstream, all of them narrowing the surface:
//!
//! 1. **Control characters are stripped.** Upstream interpolates the title
//!    into the sequence verbatim, so a session name or plugin title that
//!    contains `ESC` / `BEL` can inject arbitrary terminal control. Titles
//!    here go through [`sanitize_title`] first; a `BEL` in the input becomes a
//!    space instead of terminating the sequence early. The same contract
//!    `StatusBar::sanitize_status_text` already applies to extension status
//!    text (LUM-1481).
//! 2. **No title is restored on exit.** Upstream does not restore the previous
//!    title either — it leaves its own behind — so there is nothing to port;
//!    recorded here because a reader will look for it.
//! 3. **The app title is this port's header title (`"pi"`), not upstream's
//!    `"π"`.** Upstream's `APP_TITLE` is `piConfig?.name ? APP_NAME : "π"`
//!    (`config.ts:503`), i.e. a branded config name or the glyph. The port's
//!    header already renders [`crate::locale::HEADER_TITLE`] (`"pi"`), and a
//!    title that disagrees with the header would be a bug of its own.
//! 4. **A cwd that is a filesystem root contributes nothing** instead of an
//!    empty component (`path.basename("/")` upstream is `""`, producing
//!    `"π - "`).

use crate::locale::HEADER_TITLE;

/// The sequence that opens a title: OSC, command `0`, then the text.
pub const TITLE_OPEN: &str = "\u{1b}]0;";

/// The sequence that closes a title (BEL, upstream's `\x07`).
pub const TITLE_CLOSE: &str = "\u{7}";

/// The OSC 0 sequence that sets the terminal window/tab title to `title`.
///
/// Byte-for-byte upstream's `` `\x1b]0;${title}\x07` ``
/// (`packages/tui/src/terminal.ts:521`). The caller is responsible for
/// sanitising: use [`sanitize_title`] first, which is what
/// [`crate::App::set_terminal_title`] does.
pub fn title_sequence(title: &str) -> String {
    format!("{TITLE_OPEN}{title}{TITLE_CLOSE}")
}

/// Replace every control character in `title` with a space, then trim.
///
/// The sequence is terminated by BEL, so an unescaped `BEL` (or an `ESC` that
/// starts another control sequence) inside the title would let the text escape
/// into the terminal. C0, DEL and C1 are all mapped to `' '` — a title is
/// single-line chrome, so "show it as a space" is the same call the footer
/// makes for extension status text. Each control character gets its own space
/// (runs are not collapsed): interior spacing is the caller's, and a plugin
/// that deliberately writes double spaces keeps them.
///
/// ```text
/// "a\u{7}b"        -> "a b"
/// "a\u{7}\u{1b}b"    -> "a  b"
/// "  padded  "   -> "padded"
/// ```
pub fn sanitize_title(title: &str) -> String {
    title
        .chars()
        .map(|c| if is_control(c) { ' ' } else { c })
        .collect::<String>()
        .trim()
        .to_string()
}

/// Whether `c` is a C0, DEL or C1 control character.
fn is_control(c: char) -> bool {
    matches!(c, '\u{0}'..='\u{1f}' | '\u{7f}' | '\u{80}'..='\u{9f}')
}

/// The last path component of `path`, or `None` for a root / empty path.
///
/// Handles both separators because the port runs on Windows: `C:\Users\me\repo`
/// and `/home/me/repo` both give `repo`. Trailing separators are ignored, so
/// `repo/` gives `repo` (Node's `path.basename` does the same), and a bare
/// drive (`C:\`, `C:`) has no basename just like `/`.
pub fn path_basename(path: &str) -> Option<&str> {
    let trimmed = path.trim_end_matches(['/', '\\']);
    // `/`, `\`, `//` and `C:\` are roots; a bare `C:` is the drive too.
    if trimmed.is_empty() || trimmed.ends_with(':') {
        return None;
    }
    match trimmed.rfind(['/', '\\']) {
        Some(index) => {
            let base = &trimmed[index + 1..];
            (!base.is_empty()).then_some(base)
        }
        None => Some(trimmed),
    }
}

/// The automatic title for a session: `"<app> - <name> - <cwd basename>"`.
///
/// Mirrors upstream's `updateTerminalTitle` (`interactive-mode.ts:1017-1028`)
/// with two components instead of the fixed two upstream always has: an absent
/// or whitespace-only session name is dropped (`"<app> - <cwd>"`) and an absent
/// cwd is dropped too, so the result is never `"<app> - "`.
pub fn auto_title(app_title: &str, session_name: Option<&str>, cwd: Option<&str>) -> String {
    let app_title = sanitize_title(app_title);
    let app_title = if app_title.is_empty() {
        HEADER_TITLE.to_string()
    } else {
        app_title
    };
    let name = session_name.map(sanitize_title).filter(|n| !n.is_empty());
    let base = cwd
        .map(sanitize_title)
        .and_then(|path| path_basename(&path).map(str::to_string));
    match (name, base) {
        (Some(name), Some(base)) => format!("{app_title} - {name} - {base}"),
        (Some(name), None) => format!("{app_title} - {name}"),
        (None, Some(base)) => format!("{app_title} - {base}"),
        (None, None) => app_title,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sequence_is_osc_zero_bel() {
        assert_eq!(title_sequence("hello"), "\u{1b}]0;hello\u{7}");
        // Exactly what upstream's template literal produces; keep the bytes
        // pinned so a "prettier" ST (`\x1b\\`) can never slip in unnoticed.
        assert_eq!(title_sequence("π - repo"), "\x1b]0;π - repo\x07");
    }

    #[test]
    fn sanitize_replaces_controls_instead_of_dropping_them() {
        assert_eq!(sanitize_title("a\u{7}b"), "a b");
        assert_eq!(sanitize_title("a\u{1b}]2;evil\u{7}"), "a ]2;evil");
        assert_eq!(sanitize_title("line\nbreak\ttab"), "line break tab");
        assert_eq!(sanitize_title("  padded  "), "padded");
        assert_eq!(sanitize_title("\u{7}\u{1b}"), "");
    }

    #[test]
    fn sanitize_keeps_wide_and_non_ascii_text() {
        assert_eq!(sanitize_title("π - 会话"), "π - 会话");
    }

    #[test]
    fn basename_handles_both_separators_and_roots() {
        assert_eq!(path_basename("/home/me/repo"), Some("repo"));
        assert_eq!(path_basename(r"C:\Users\me\repo"), Some("repo"));
        assert_eq!(path_basename("repo/"), Some("repo"));
        assert_eq!(path_basename(r"repo\"), Some("repo"));
        assert_eq!(path_basename("repo"), Some("repo"));
        assert_eq!(path_basename("/"), None);
        assert_eq!(path_basename("//"), None);
        assert_eq!(path_basename(r"C:\"), None);
        assert_eq!(path_basename("C:"), None);
        assert_eq!(path_basename(""), None);
    }

    #[test]
    fn auto_title_matches_upstreams_shape() {
        assert_eq!(
            auto_title("pi", Some("my session"), Some("/srv/repo")),
            "pi - my session - repo"
        );
        assert_eq!(auto_title("pi", None, Some("/srv/repo")), "pi - repo");
        assert_eq!(auto_title("pi", Some("demo"), None), "pi - demo");
        assert_eq!(auto_title("pi", None, None), "pi");
        assert_eq!(
            auto_title("pi", Some("   "), Some("/srv/repo")),
            "pi - repo"
        );
        assert_eq!(auto_title("", None, None), "pi");
    }

    #[test]
    fn auto_title_sanitizes_both_components() {
        assert_eq!(
            auto_title("pi", Some("bad\u{7}name"), Some("/srv/re\u{1b}po")),
            "pi - bad name - re po"
        );
    }
}
