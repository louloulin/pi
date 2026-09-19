//! Component-level style adapters over a resolved [`Theme`].
//!
//! This is the Rust port of the per-component theme adapters upstream builds in
//! `packages/coding-agent/src/modes/interactive/theme/theme.ts` — mainly
//! `getSelectListTheme` (`theme.ts:1209-1216`) plus the `theme.fg(...)` calls
//! the message / footer renderers make inline. Upstream hands each component a
//! small object of functions; here a single [`SelectListStyles`] borrows the
//! [`Theme`] and exposes the same surface. Callers drop the returned `String`
//! into their existing `format!` line, so the plain-text layout is unchanged.
//!
//! # Slot mapping (upstream evidence)
//!
//! | method | theme slot | upstream |
//! |--------|------------|----------|
//! | [`SelectListStyles::selected_prefix`] | `accent` | `theme.ts:1211` (`selectedPrefix`) |
//! | [`SelectListStyles::selected_text`] | `accent` + `selectedBg` | see "selected row" below |
//! | [`SelectListStyles::description`] | `muted` | `theme.ts:1213` (`description`) |
//! | [`SelectListStyles::scroll_info`] | `muted` | `theme.ts:1214` (`scrollInfo`) |
//! | [`SelectListStyles::no_match`] | `muted` | `theme.ts:1215` (`noMatch`) |
//!
//! The two select-list consumers are
//! `packages/tui/src/components/select-list.ts:80,103,205,208,216`.
//!
//! # Deliberate differences
//!
//! * **Selected row uses a background.** Upstream `getSelectListTheme`
//!   (`theme.ts:1212`) maps `selectedText` to `theme.fg("accent", text)` only.
//!   But the task for this slice asks for a highlighted *whole row*, and other
//!   upstream lists already do exactly that:
//!   `session-selector.ts:507`, `tree-selector.ts:751-752` and
//!   `tui-renderer.ts:32` all wrap the selected line in
//!   `theme.bg("selectedBg", ...)`. [`SelectListStyles::selected_text`] applies
//!   accent foreground **and** `selectedBg` background, i.e. the union of the
//!   two upstream treatments. The plain `render_lines` path is untouched, so
//!   anything depending on the old text output still sees it.
//! * **One adapter, not three.** Upstream also has `MarkdownTheme` /
//!   `SettingsListTheme`; those components do not exist in `pi-tui` yet (markdown
//!   rendering is explicitly out of scope for this task, see
//!   `docs/FEATURE_PI_RS_STATUS.md`). The generic helpers below
//!   (`accent`/`muted`/`dim`/`text`/…) cover the status bar and message view
//!   without inventing adapters for components that are not ported.

use crate::theme::{Theme, ThemeBg, ThemeColor};

/// Style adapter over a [`Theme`] for the built-in `pi-tui` components.
///
/// Borrow it for the lifetime of a render call and pass it to the
/// `*_themed` render methods ([`Selector::render_lines_themed`],
/// [`StatusBar::render_themed`], [`MessageView::render_lines_themed`]).
///
/// ```
/// use pi_tui::styles::SelectListStyles;
/// use pi_tui::theme::{builtin_theme, ColorMode};
///
/// let theme = builtin_theme("dark", ColorMode::TrueColor).unwrap();
/// let styles = SelectListStyles::new(&theme);
/// assert_eq!(
///     styles.selected_text("x"),
///     "\u{1b}[48;2;58;58;74m\u{1b}[38;2;138;190;183mx\u{1b}[39m\u{1b}[49m"
/// );
/// ```
///
/// [`Selector::render_lines_themed`]: crate::Selector::render_lines_themed
/// [`StatusBar::render_themed`]: crate::StatusBar::render_themed
/// [`MessageView::render_lines_themed`]: crate::MessageView::render_lines_themed
#[derive(Debug, Clone, Copy)]
pub struct SelectListStyles<'a> {
    theme: &'a Theme,
}

impl<'a> SelectListStyles<'a> {
    /// Wrap `theme`.
    pub fn new(theme: &'a Theme) -> Self {
        Self { theme }
    }

    /// The borrowed theme.
    pub fn theme(&self) -> &Theme {
        self.theme
    }

    /// Style the marker in front of the selected row (`accent`), upstream
    /// `SelectListTheme.selectedPrefix` (`theme.ts:1211`).
    pub fn selected_prefix(&self, text: &str) -> String {
        self.theme.fg(ThemeColor::Accent, text)
    }

    /// Style a whole selected row: `accent` foreground over a `selectedBg`
    /// background.
    ///
    /// Deliberate difference from `theme.ts:1212` — see the module docs for the
    /// upstream lists (`session-selector.ts:507` …) that justify the
    /// background.
    pub fn selected_text(&self, text: &str) -> String {
        self.theme.bg(
            ThemeBg::SelectedBg,
            &self.theme.fg(ThemeColor::Accent, text),
        )
    }

    /// Style a row's description column with the weakened `muted` foreground,
    /// upstream `SelectListTheme.description` (`theme.ts:1213`,
    /// `select-list.ts:208`).
    pub fn description(&self, text: &str) -> String {
        self.theme.fg(ThemeColor::Muted, text)
    }

    /// Style the `(n/total)` scroll indicator (`muted`), upstream
    /// `SelectListTheme.scrollInfo` (`theme.ts:1214`, `select-list.ts:103`).
    pub fn scroll_info(&self, text: &str) -> String {
        self.theme.fg(ThemeColor::Muted, text)
    }

    /// Style the "no matching items" line (`muted`), upstream
    /// `SelectListTheme.noMatch` (`theme.ts:1215`, `select-list.ts:80`).
    pub fn no_match(&self, text: &str) -> String {
        self.theme.fg(ThemeColor::Muted, text)
    }

    /// Style with the `accent` slot. Used for the status-bar model and the
    /// user-message prefix.
    pub fn accent(&self, text: &str) -> String {
        self.theme.fg(ThemeColor::Accent, text)
    }

    /// Style with the `muted` slot (session id, non-selected descriptions).
    pub fn muted(&self, text: &str) -> String {
        self.theme.fg(ThemeColor::Muted, text)
    }

    /// Style with the `dim` slot (status-bar token counts, streaming caret),
    /// matching the upstream footer's `theme.fg("dim", …)`
    /// (`footer.ts:236-240`).
    pub fn dim(&self, text: &str) -> String {
        self.theme.fg(ThemeColor::Dim, text)
    }

    /// Style with the default `text` slot (assistant body).
    pub fn text(&self, text: &str) -> String {
        self.theme.fg(ThemeColor::Text, text)
    }

    /// Style a user-message body with `userMessageText`, upstream
    /// `user-message.ts:48`.
    pub fn user_message_text(&self, text: &str) -> String {
        self.theme.fg(ThemeColor::UserMessageText, text)
    }

    /// Style a tool-call title with `toolTitle`, upstream
    /// `tool-execution.ts:153`.
    pub fn tool_title(&self, text: &str) -> String {
        self.theme.fg(ThemeColor::ToolTitle, text)
    }

    /// Style tool output with `toolOutput`, upstream
    /// `tool-execution.ts:165`.
    pub fn tool_output(&self, text: &str) -> String {
        self.theme.fg(ThemeColor::ToolOutput, text)
    }

    /// Style an error with the `error` slot.
    pub fn error(&self, text: &str) -> String {
        self.theme.fg(ThemeColor::Error, text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{builtin_theme, ColorMode};

    fn dark() -> Theme {
        builtin_theme("dark", ColorMode::TrueColor).expect("dark theme")
    }

    #[test]
    fn select_list_slots_match_the_dark_palette() {
        let theme = dark();
        let styles = SelectListStyles::new(&theme);
        // accent #8abeb7, muted #808080, selectedBg #3a3a4a (dark.json).
        assert_eq!(
            styles.selected_text("x"),
            "\u{1b}[48;2;58;58;74m\u{1b}[38;2;138;190;183mx\u{1b}[39m\u{1b}[49m"
        );
        assert_eq!(
            styles.description("x"),
            "\u{1b}[38;2;128;128;128mx\u{1b}[39m"
        );
        assert_eq!(styles.no_match("x"), "\u{1b}[38;2;128;128;128mx\u{1b}[39m");
        assert_eq!(
            styles.scroll_info("x"),
            "\u{1b}[38;2;128;128;128mx\u{1b}[39m"
        );
        assert_eq!(
            styles.selected_prefix("x"),
            "\u{1b}[38;2;138;190;183mx\u{1b}[39m"
        );
    }

    #[test]
    fn a_plain_theme_emits_no_escape_sequences() {
        let theme = builtin_theme("dark", ColorMode::None).expect("plain dark theme");
        let styles = SelectListStyles::new(&theme);
        for styled in [
            styles.selected_prefix("x"),
            styles.selected_text("x"),
            styles.description("x"),
            styles.scroll_info("x"),
            styles.no_match("x"),
            styles.accent("x"),
            styles.muted("x"),
            styles.dim("x"),
            styles.text("x"),
            styles.user_message_text("x"),
            styles.tool_title("x"),
            styles.tool_output("x"),
            styles.error("x"),
        ] {
            assert_eq!(styled, "x");
            assert!(!styled.contains('\u{1b}'));
        }
        assert_eq!(theme.get_fg_ansi(ThemeColor::Accent), "");
        assert_eq!(theme.get_bg_ansi(ThemeBg::SelectedBg), "");
        assert!(theme.is_plain());
    }
}
