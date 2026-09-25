//! Styled spans — the bridge between a component's theme slots and the
//! [`App`](crate::App) buffer writer.
//!
//! A component lays its output out as [`StyledLine`]s: each [`StyledSpan`]
//! carries a run of text plus the [`SpanStyle`] slot that colours it. The App
//! resolves that slot into a [`ratatui::style::Style`] and writes it straight
//! into the render buffer, so themed cells never carry ANSI escapes in their
//! text. The legacy `*_themed` string renderers build the same spans and
//! convert them to ANSI for callers that compare strings.

use ratatui::buffer::{Buffer, Cell};
use ratatui::style::{Color, Modifier, Style};

use crate::utils::hyperlink::hyperlink;
use crate::theme::{hex_to_256, hex_to_rgb, ColorMode, ColorValue, Theme, ThemeBg, ThemeColor};
use crate::utils::width::{char_columns, columns};

/// The theme slot(s) a [`StyledSpan`] renders with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SpanStyle {
    /// Foreground slot, if any.
    pub fg: Option<ThemeColor>,
    /// Background slot, if any.
    pub bg: Option<ThemeBg>,
    /// Whether the run is rendered bold.
    pub bold: bool,
    /// Whether the run is rendered italic.
    pub italic: bool,
    /// Whether the run is rendered underlined.
    pub underline: bool,
    /// Whether the run is rendered struck through.
    pub strikethrough: bool,
    /// Whether the run's foreground and background are swapped
    /// (chalk `inverse` / ANSI `REVERSED`).
    pub inverse: bool,
}

impl SpanStyle {
    /// An unstyled run.
    pub const PLAIN: Self = Self {
        fg: None,
        bg: None,
        bold: false,
        italic: false,
        underline: false,
        strikethrough: false,
        inverse: false,
    };

    /// A run with only a foreground slot.
    pub fn fg(color: ThemeColor) -> Self {
        Self {
            fg: Some(color),
            ..Self::PLAIN
        }
    }

    /// A run with a foreground and a background slot.
    pub fn fg_bg(fg: ThemeColor, bg: ThemeBg) -> Self {
        Self {
            fg: Some(fg),
            bg: Some(bg),
            ..Self::PLAIN
        }
    }

    /// The slots with the bold modifier applied.
    pub fn bold(mut self) -> Self {
        self.bold = true;
        self
    }

    /// The slots with the italic modifier applied.
    pub fn italic(mut self) -> Self {
        self.italic = true;
        self
    }

    /// The slots with the underline modifier applied.
    pub fn underline(mut self) -> Self {
        self.underline = true;
        self
    }

    /// The slots with the strikethrough modifier applied.
    pub fn strikethrough(mut self) -> Self {
        self.strikethrough = true;
        self
    }

    /// The slots with the inverse (reversed video) modifier applied.
    pub fn inverse(mut self) -> Self {
        self.inverse = true;
        self
    }

    /// Render `text` as an ANSI string for this slot.
    ///
    /// A plain theme ([`ColorMode::None`]) returns `text` unchanged. The
    /// wrapping order matches upstream (`theme.fg("accent", theme.bold(text))`,
    /// `theme.bg("selectedBg", theme.fg("accent", text))`): the text
    /// decorations are innermost (each uses its own reset code, so their order
    /// is not observable), then the foreground, then the background.
    pub fn ansi(self, theme: &Theme, text: &str) -> String {
        if theme.is_plain() {
            return text.to_string();
        }
        let mut out = text.to_string();
        if self.bold {
            out = theme.bold(&out);
        }
        if self.italic {
            out = theme.italic(&out);
        }
        if self.underline {
            out = theme.underline(&out);
        }
        if self.strikethrough {
            out = theme.strikethrough(&out);
        }
        if self.inverse {
            out = theme.inverse(&out);
        }
        if let Some(fg) = self.fg {
            out = theme.fg(fg, &out);
        }
        if let Some(bg) = self.bg {
            out = theme.bg(bg, &out);
        }
        out
    }

    /// Resolve this slot into a [`ratatui::style::Style`] for the App's buffer
    /// path. A plain theme resolves to the default (unstyled) style.
    pub fn to_style(self, theme: &Theme) -> Style {
        if theme.is_plain() {
            return Style::default();
        }
        let mut style = Style::default();
        if let Some(color) = self.fg.and_then(|slot| fg_color(theme, slot)) {
            style = style.fg(color);
        }
        if let Some(color) = self.bg.and_then(|slot| bg_color(theme, slot)) {
            style = style.bg(color);
        }
        if self.bold {
            style = style.add_modifier(Modifier::BOLD);
        }
        if self.italic {
            style = style.add_modifier(Modifier::ITALIC);
        }
        if self.underline {
            style = style.add_modifier(Modifier::UNDERLINED);
        }
        if self.strikethrough {
            style = style.add_modifier(Modifier::CROSSED_OUT);
        }
        if self.inverse {
            style = style.add_modifier(Modifier::REVERSED);
        }
        style
    }
}

/// One run of text with its [`SpanStyle`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StyledSpan {
    /// The run's text.
    pub text: String,
    /// The run's theme slot.
    pub style: SpanStyle,
    /// OSC 8 target, if this run is a terminal hyperlink.
    ///
    /// The URL never lives in [`StyledSpan::text`], so width math, wrapping
    /// and selection (all of which read `text` / [`plain_text`]) cannot see
    /// it. Only the two presentation paths — [`themed_text`] and
    /// [`write_styled_line`] — turn it into a sequence. A terminal that does
    /// not understand OSC 8 paints the text unchanged; that is why the
    /// caller decides whether to attach a link at all (see
    /// [`crate::hyperlink`]).
    pub link: Option<String>,
}

impl StyledSpan {
    /// Build a plain span from its text and slot.
    pub fn new(text: impl Into<String>, style: SpanStyle) -> Self {
        Self {
            text: text.into(),
            style,
            link: None,
        }
    }

    /// Build a span that renders as an OSC 8 hyperlink to `url`.
    pub fn linked(text: impl Into<String>, style: SpanStyle, url: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            style,
            link: Some(url.into()),
        }
    }

    /// Attach an OSC 8 target to this span (builder form).
    pub fn with_link(mut self, url: impl Into<String>) -> Self {
        self.link = Some(url.into());
        self
    }
}

/// A rendered line: one or more [`StyledSpan`]s in visual order.
pub type StyledLine = Vec<StyledSpan>;

/// The one-column mark a clipped line ends with (see
/// [`write_styled_line_ellipsized`]).
///
/// `…` (U+2026) rather than the three-column `"..."` upstream's
/// `truncateToWidth` appends: a chrome row is one row per line, so three
/// columns are 7% of a 44-column terminal. The rest of the frame already
/// speaks this dialect — the transcript marks a collapsed block with
/// `* … (+6 lines, Ctrl+O to expand)` and the status bar's zone budget marks
/// a dropped part with the same character (`status.rs::ELLIPSIS`).
///
/// Deliberate deviation from upstream, documented here so the next reader
/// does not "fix" it back to `...`.
pub const CLIP_MARK: char = '\u{2026}';

/// Concatenate a line's text, dropping all styling.
pub fn plain_text(line: &[StyledSpan]) -> String {
    line.iter().map(|span| span.text.as_str()).collect()
}

/// The rendered width of a line in columns.
///
/// Follows the crate-wide convention (see `visual_text.rs` and
/// [`crate::utils::hyperlink::visible_width`]): one column per character, wide glyphs
/// included. Keeping this identical to [`write_styled_line_hyperlinked`]'s
/// `col` counter is what makes the clip arithmetic below exact.
fn line_width(line: &[StyledSpan]) -> usize {
    line.iter().map(|span| columns(&span.text)).sum()
}

/// Render a line as an ANSI string using each span's slot.
///
/// A span carrying an OSC 8 [`link`](StyledSpan::link) is wrapped in the
/// matching open/close sequence after its SGR styling, exactly like
/// upstream's `hyperlink(styledLink, token.href)`
/// (`packages/tui/src/components/markdown.ts:695`). The escapes are
/// zero-width, so [`crate::utils::hyperlink::visible_width`] of the result equals
/// [`plain_text`].
pub fn themed_text(line: &[StyledSpan], theme: &Theme) -> String {
    line.iter()
        .map(|span| match &span.link {
            Some(url) => hyperlink(&span.style.ansi(theme, &span.text), url),
            None => span.style.ansi(theme, &span.text),
        })
        .collect()
}

/// Write a styled line into `buf` at row `y`, starting at column `x0` and
/// clipped to `max_width` columns.
///
/// Every cell gets both the character and the span's resolved style, so the
/// buffer is themed without any ANSI escape ever entering the cell text.
pub fn write_styled_line(
    buf: &mut Buffer,
    x0: u16,
    y: u16,
    max_width: u16,
    line: &[StyledSpan],
    theme: &Theme,
) {
    write_styled_line_hyperlinked(buf, x0, y, max_width, line, theme, false);
}

/// [`write_styled_line`] with OSC 8 hyperlink emission.
///
/// When `hyperlinks` is true, every cell belonging to a linked span gets a
/// self-contained `open + glyph + close` sequence as its symbol. `ratatui`
/// 0.28 has no hyperlink channel on `Cell`, but its `crossterm` backend
/// writes `cell.symbol()` verbatim (`Print(cell.symbol())`), so the sequence
/// reaches the terminal while the cell still occupies exactly one column —
/// the cell grid, the diff and every width computation are unchanged.
/// Wrapping *per cell* (rather than once around the run) keeps every printed
/// sequence balanced, which matters because the backend emits cells
/// independently.
///
/// The live frame passes `true`; the flat `/transcript` snapshot passes
/// `false` so exported text never carries escapes.
pub fn write_styled_line_hyperlinked(
    buf: &mut Buffer,
    x0: u16,
    y: u16,
    max_width: u16,
    line: &[StyledSpan],
    theme: &Theme,
    hyperlinks: bool,
) {
    write_styled_line_inner(
        buf,
        x0,
        y,
        max_width,
        line,
        theme,
        WriteMode {
            hyperlinks,
            tail: Tail::Cut,
        },
    );
}

/// [`write_styled_line`] that says so when it has to drop a line's tail.
///
/// The plain writer stops writing at `max_width` and leaves the rest of the
/// row blank, which renders "this line ends here" and "this line was cut"
/// identically. That is how a 44-column terminal ended up painting the
/// startup header as `hints hidden on a short terminal — Alt+H sho`: the
/// reader cannot tell whether the sentence was over or the terminal too
/// narrow.
///
/// This variant: the tail is dropped and the last written cell becomes
/// [`CLIP_MARK`]. The mark is placed at the **last word boundary** at or
/// before the budget when one is close, so a word is dropped whole instead of
/// being left as a fragment (`sho`) — the same whole-semantic-unit rule the
/// status bar's zone budget follows (`status.rs::NARROW_SACRIFICE_ORDER`).
/// The remaining cells stay blank, so a shortened line never trails stale
/// text from an earlier frame (the live path renders into ratatui's freshly
/// reset back buffer).
///
/// A line that fits is written exactly like [`write_styled_line`] writes it,
/// down to the cells; callers that pin golden frames at wide widths are not
/// affected.
pub fn write_styled_line_ellipsized(
    buf: &mut Buffer,
    x0: u16,
    y: u16,
    max_width: u16,
    line: &[StyledSpan],
    theme: &Theme,
) {
    write_styled_line_inner(
        buf,
        x0,
        y,
        max_width,
        line,
        theme,
        WriteMode {
            hyperlinks: false,
            tail: Tail::Mark(CLIP_MARK),
        },
    );
}

/// What the writer does with a line wider than its region.
#[derive(Debug, Clone, Copy)]
enum Tail {
    /// Stop at the region's edge — upstream's clip, and the pre-LUM-1412
    /// behaviour of every caller.
    Cut,
    /// Give up the trailing partial word and put `mark` in the cell the cut
    /// starts at.
    Mark(char),
}

/// The two orthogonal switches the writers set: hyperlink emission, and how
/// an overflowing line ends.
#[derive(Debug, Clone, Copy)]
struct WriteMode {
    hyperlinks: bool,
    tail: Tail,
}

/// The shared body of the two writers above.
///
/// `tail` decides whether an overflowing line is marked; the mark is only ever
/// written when the line actually overflows, so the fitting path is
/// byte-for-byte the plain one.
fn write_styled_line_inner(
    buf: &mut Buffer,
    x0: u16,
    y: u16,
    max_width: u16,
    line: &[StyledSpan],
    theme: &Theme,
    mode: WriteMode,
) {
    if max_width == 0 {
        return;
    }
    let overflow = line_width(line) > max_width as usize;
    let (limit_cols, limit_chars) = match mode.tail {
        Tail::Mark(_) if overflow => clip_keep(line, max_width as usize),
        _ => (max_width as usize, usize::MAX),
    };
    let mut col = 0usize;
    'spans: for span in line {
        let style = span.style.to_style(theme);
        let link = if mode.hyperlinks {
            span.link.as_deref()
        } else {
            None
        };
        for ch in span.text.chars() {
            let glyph_width = char_columns(ch);
            if col + glyph_width > limit_cols {
                break 'spans;
            }
            if let Some(cell) = buf.cell_mut((x0 + col as u16, y)) {
                match link {
                    Some(url) => {
                        cell.set_symbol(&hyperlink(&ch.to_string(), url));
                    }
                    None => {
                        cell.set_char(ch);
                    }
                }
                cell.set_style(style);
            }
            // A wide glyph owns the cell it was written into **and** the
            // cells its second column covers. `ratatui`'s `set_stringn`
            // resets the latter so a grapheme is never hidden behind stale
            // content from an earlier frame; do the same here, because a
            // neighbouring wide glyph could otherwise leave its right half
            // visible under the left half of this one.
            for offset in 1..glyph_width {
                if let Some(cell) = buf.cell_mut((x0 + (col + offset) as u16, y)) {
                    cell.reset();
                }
            }
            col += glyph_width;
        }
    }
    if overflow {
        if let Tail::Mark(mark) = mode.tail {
            // `clip_keep` always leaves the last column for the mark, so
            // `col` is a cell the row owns even after a word-boundary cut.
            if let Some(cell) = buf.cell_mut((x0 + col as u16, y)) {
                cell.set_char(mark);
                cell.set_style(style_at(line, limit_chars, theme));
            }
        }
    }
}

/// How much of `line` survives a marked clip at `max_width` columns, as
/// `(columns kept, characters consumed)`.
///
/// Always leaves the last column for [`CLIP_MARK`]. Prefers the last
/// whitespace boundary at or before that budget, so a word is dropped whole
/// rather than left as a fragment. A boundary that would throw away more than
/// half of the available columns (one long token after an early space) is not
/// an affordance, it is a different bug: the caller then keeps every column
/// and the word is cut — still marked.
///
/// The two numbers differ once a wide glyph is on the line: the writer budgets
/// in **columns** while the clip mark takes over the style of the
/// **character** it replaced ([`style_at`]), so the caller needs both.
fn clip_keep(line: &[StyledSpan], max_width: usize) -> (usize, usize) {
    let budget = max_width.saturating_sub(1);
    if budget == 0 {
        return (0, 0);
    }
    let mut used = 0usize;
    let mut chars = 0usize;
    let mut boundary: Option<(usize, usize)> = None;
    for ch in plain_text(line).chars() {
        let glyph_width = char_columns(ch);
        if used + glyph_width > budget {
            break;
        }
        // Recorded *before* the blank is consumed, so the blank itself stays
        // out of the kept prefix — upstream's boundary index is "characters
        // before the whitespace".
        if chars > 0 && ch.is_whitespace() {
            boundary = Some((used, chars));
        }
        used += glyph_width;
        chars += 1;
    }
    match boundary {
        Some((cols, count)) if cols * 2 >= budget => (cols, count),
        _ => (used, chars),
    }
}

/// Write a plain (unstyled) string into one buffer row, advancing by terminal
/// **columns**.
///
/// The per-cell painters in the App (`paint_prompt`, the dialog overlay), the
/// status bar's plain renderer and [`MessageView`](crate::MessageView)'s plain
/// `render_to_buffer` path all need the same three rules, so they live here
/// once:
///
/// * a glyph occupies [`char_columns`] cells, not one;
/// * the cells a wide glyph's second column covers are reset, so a stale glyph
///   from an earlier frame can never show through;
/// * nothing is written past `max_width` columns, so a row can never bleed into
///   the region next to it.
pub fn write_plain_row(buf: &mut Buffer, x0: u16, y: u16, max_width: u16, text: &str) {
    let mut col = 0usize;
    for ch in text.chars() {
        let glyph_width = char_columns(ch);
        if col + glyph_width > max_width as usize {
            break;
        }
        if let Some(cell) = buf.cell_mut((x0 + col as u16, y)) {
            cell.set_char(ch);
        }
        for offset in 1..glyph_width {
            if let Some(cell) = buf.cell_mut((x0 + (col + offset) as u16, y)) {
                cell.reset();
            }
        }
        col += glyph_width;
    }
}

/// The text content of one buffer row, **skipping the continuation cells a
/// wide glyph covers**.
///
/// A `ratatui::Buffer` stores a double-width glyph in one cell and leaves the
/// cell it covers blank (`Buffer::set_stringn` resets it). Joining the cell
/// symbols naively therefore reads a CJK row as `你 好` instead of `你好`.
/// This is the same rule `ratatui`'s own `Debug` impl uses
/// (`skip = max(skip, cell.symbol().width()) - 1`), and it is how a terminal
/// reads the same row back.
pub fn buffer_row_text(row: &[Cell]) -> String {
    let mut out = String::new();
    let mut skip = 0usize;
    for cell in row {
        if skip == 0 {
            out.push_str(cell.symbol());
        }
        skip = skip.max(columns(cell.symbol())).saturating_sub(1);
    }
    out
}

/// The style of the character at `index` in `line` — the cell the mark takes
/// over inherits the colour of the text it replaced. Past the end, the last
/// span's style.
fn style_at(line: &[StyledSpan], index: usize, theme: &Theme) -> ratatui::style::Style {
    let mut start = 0usize;
    for span in line {
        let end = start + span.text.chars().count();
        if index < end {
            return span.style.to_style(theme);
        }
        start = end;
    }
    line.last()
        .map_or_else(Style::default, |span| span.style.to_style(theme))
}

/// Resolve a foreground slot to a backend colour.
fn fg_color(theme: &Theme, slot: ThemeColor) -> Option<Color> {
    value_to_color(theme.fg_value(slot), theme.color_mode())
}

/// Resolve a background slot to a backend colour.
fn bg_color(theme: &Theme, slot: ThemeBg) -> Option<Color> {
    value_to_color(theme.bg_value(slot), theme.color_mode())
}

/// Translate a resolved [`ColorValue`] into the matching `ratatui` colour for
/// the active colour mode. `None` means "leave the terminal default".
fn value_to_color(value: Option<&ColorValue>, mode: ColorMode) -> Option<Color> {
    if matches!(mode, ColorMode::None) {
        return None;
    }
    match value? {
        ColorValue::Reset => Some(Color::Reset),
        ColorValue::Index(index) => Some(Color::Indexed(*index)),
        ColorValue::Hex(hex) => match mode {
            ColorMode::TrueColor => hex_to_rgb(hex).ok().map(|(r, g, b)| Color::Rgb(r, g, b)),
            ColorMode::Ansi256 => hex_to_256(hex).ok().map(Color::Indexed),
            ColorMode::Auto => hex_to_256(hex).ok().map(Color::Indexed),
            ColorMode::None => None,
        },
        ColorValue::Var(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::builtin_theme;
    use ratatui::layout::Rect;

    /// The 51-character startup-header line LUM-1412 measured in a real
    /// 44-column PTY (see `locale::header_folded_line`).
    const FOLDED_HINT: &str = "hints hidden on a short terminal — Alt+H shows them";

    fn plain_line(text: &str) -> StyledLine {
        vec![StyledSpan::new(text, SpanStyle::PLAIN)]
    }

    /// Render `line` through the marked writer at `width` and return the whole
    /// row (padding included).
    fn marked_row(line: &[StyledSpan], width: u16, mode: ColorMode) -> (String, Buffer) {
        let area = Rect {
            x: 0,
            y: 0,
            width,
            height: 1,
        };
        let mut buf = Buffer::empty(area);
        let theme = builtin_theme("dark", mode).expect("dark theme");
        write_styled_line_ellipsized(&mut buf, 0, 0, width, line, &theme);
        let row = (0..width)
            .map(|x| buf.cell((x, 0)).expect("cell").symbol().to_string())
            .collect();
        (row, buf)
    }

    fn marked(line: &[StyledSpan], width: u16) -> String {
        marked_row(line, width, ColorMode::None)
            .0
            .trim_end()
            .to_string()
    }

    #[test]
    fn ansi_wrapping_order_matches_upstream() {
        let theme = builtin_theme("dark", ColorMode::TrueColor).expect("dark theme");
        let span = SpanStyle::fg_bg(ThemeColor::Accent, ThemeBg::SelectedBg);
        assert_eq!(
            span.ansi(&theme, "x"),
            "\u{1b}[48;2;58;58;74m\u{1b}[38;2;138;190;183mx\u{1b}[39m\u{1b}[49m"
        );

        let title = SpanStyle::fg(ThemeColor::Accent).bold();
        assert_eq!(
            title.ansi(&theme, "T"),
            "\u{1b}[38;2;138;190;183m\u{1b}[1mT\u{1b}[22m\u{1b}[39m"
        );
    }

    #[test]
    fn inverse_modifier_reverses_video() {
        let theme = builtin_theme("dark", ColorMode::TrueColor).expect("dark theme");
        let span = SpanStyle::fg(ThemeColor::Accent).inverse();
        assert_eq!(
            span.ansi(&theme, "T"),
            "\u{1b}[38;2;138;190;183m\u{1b}[7mT\u{1b}[27m\u{1b}[39m"
        );
        assert_eq!(span.to_style(&theme).add_modifier, Modifier::REVERSED);
        // A plain theme never emits escapes, modifier or not.
        let plain = builtin_theme("dark", ColorMode::None).expect("plain theme");
        assert_eq!(span.ansi(&plain, "T"), "T");
    }

    #[test]
    fn styles_resolve_to_rgb_for_truecolor() {
        let theme = builtin_theme("dark", ColorMode::TrueColor).expect("dark theme");
        let style = SpanStyle::fg_bg(ThemeColor::Accent, ThemeBg::SelectedBg).to_style(&theme);
        assert_eq!(style.fg, Some(Color::Rgb(138, 190, 183)));
        assert_eq!(style.bg, Some(Color::Rgb(58, 58, 74)));
        assert_eq!(style.add_modifier, Modifier::empty());
    }

    #[test]
    fn a_plain_theme_yields_the_default_style() {
        let theme = builtin_theme("dark", ColorMode::None).expect("plain theme");
        let style = SpanStyle::fg(ThemeColor::Accent).bold().to_style(&theme);
        assert_eq!(style, Style::default());
    }

    // ---- LUM-1412: the marked writer -------------------------------------

    /// A line that fits is written exactly like the plain writer writes it,
    /// padding included — the wide-terminal golden frames must not move.
    #[test]
    fn a_fitting_line_is_written_exactly_like_the_plain_writer() {
        let line = plain_line("Faux test model  in 0 out 0");
        let theme = builtin_theme("dark", ColorMode::None).expect("plain theme");
        let area = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 1,
        };
        let (mut marked_buf, mut plain_buf) = (Buffer::empty(area), Buffer::empty(area));
        write_styled_line_ellipsized(&mut marked_buf, 0, 0, 40, &line, &theme);
        write_styled_line(&mut plain_buf, 0, 0, 40, &line, &theme);
        assert_eq!(marked_buf, plain_buf);
        assert!(!marked(&line, 40).contains(CLIP_MARK));
    }

    /// The defect this round fixes, at the exact width it was measured at:
    /// 44 columns of a 51-column startup-header line used to read
    /// `… — Alt+H sho`, a fragment that says nothing about being cut.
    #[test]
    fn an_overlong_chrome_row_drops_a_whole_word_and_marks_it() {
        let line = plain_line(FOLDED_HINT);
        assert_eq!(
            marked(&line, 44),
            "hints hidden on a short terminal — Alt+H\u{2026}"
        );
        // The row is still exactly the region's width, so nothing else on
        // the row is overwritten and the mark is the row's last cell.
        let (row, _) = marked_row(&line, 44, ColorMode::None);
        assert_eq!(row.chars().count(), 44);
        assert_eq!(row.chars().nth(40), Some(CLIP_MARK));
        assert!(row.chars().skip(41).all(|ch| ch == ' '), "{row:?}");
    }

    /// The narrower the row, the more the boundary rule has to give up — but
    /// it always gives up a whole word where one is available, and only cuts
    /// inside a word once the first two words no longer fit (12 columns).
    #[test]
    fn a_narrow_row_gives_up_whole_words_before_it_cuts_inside_one() {
        let line = plain_line(FOLDED_HINT);
        assert_eq!(
            marked(&line, 50),
            "hints hidden on a short terminal — Alt+H shows\u{2026}"
        );
        assert_eq!(
            marked(&line, 40),
            "hints hidden on a short terminal —\u{2026}"
        );
        assert_eq!(marked(&line, 30), "hints hidden on a short\u{2026}");
        assert_eq!(marked(&line, 20), "hints hidden on a\u{2026}");
        assert_eq!(marked(&line, 12), "hints hidde\u{2026}");
    }

    /// Every width from 1 to 120: the row is exactly as wide as the region,
    /// every overflow is marked, and a whole-word cut never leaves the
    /// fragments a hard clip produced on the header line before this round.
    #[test]
    fn a_marked_row_never_overruns_and_never_ends_in_a_fragment() {
        let line = plain_line(FOLDED_HINT);
        let fragments = ["sho", "sh"];
        for width in 1..=120u16 {
            let (row, _) = marked_row(&line, width, ColorMode::None);
            assert_eq!(row.chars().count(), width as usize);
            let visible = row.trim_end();
            if (width as usize) < FOLDED_HINT.chars().count() {
                assert!(
                    visible.ends_with(CLIP_MARK),
                    "width {width} was cut without a mark: {visible:?}"
                );
                for fragment in fragments {
                    assert!(
                        !visible.trim_end_matches(CLIP_MARK).ends_with(fragment),
                        "width {width} left the fragment {fragment:?}: {visible:?}"
                    );
                }
            } else {
                assert_eq!(visible, FOLDED_HINT, "width {width}");
            }
        }
    }

    /// A single long token has no usable boundary before the budget, so the
    /// word is cut at the edge — but still marked, and the row still fits.
    #[test]
    fn a_single_long_token_is_cut_at_the_edge_and_still_marked() {
        let line = plain_line(&format!("a {}", "a".repeat(60)));
        assert_eq!(marked(&line, 20), "a aaaaaaaaaaaaaaaaa\u{2026}");
        // The boundary at index 1 exists but would throw away 38 of the 39
        // usable columns, which is not an affordance: fall back to the edge.
        assert_eq!(
            marked(&line, 40),
            "a aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\u{2026}"
        );
    }

    /// The mark takes over the cell of the character it replaced, so a clipped
    /// themed span keeps its colour instead of turning into an unstyled cell.
    #[test]
    fn the_mark_inherits_the_style_of_the_text_it_replaces() {
        let line: StyledLine = vec![
            StyledSpan::new("hello wor", SpanStyle::fg(ThemeColor::Accent)),
            StyledSpan::new("ld more text", SpanStyle::fg(ThemeColor::Error)),
        ];
        let (_, buf) = marked_row(&line, 12, ColorMode::TrueColor);
        let accent = SpanStyle::fg(ThemeColor::Accent)
            .to_style(&builtin_theme("dark", ColorMode::TrueColor).expect("dark theme"));
        let error = SpanStyle::fg(ThemeColor::Error)
            .to_style(&builtin_theme("dark", ColorMode::TrueColor).expect("dark theme"));
        assert_eq!(buf.cell((0, 0)).expect("cell").style().fg, accent.fg);
        assert_eq!(
            buf.cell((11, 0)).expect("cell").symbol(),
            CLIP_MARK.to_string()
        );
        assert_eq!(buf.cell((11, 0)).expect("cell").style().fg, error.fg);
    }

    /// Degenerate budgets: one column is the mark and nothing else, zero
    /// columns writes nothing at all.
    #[test]
    fn a_one_column_row_is_just_the_mark_and_zero_columns_write_nothing() {
        let line = plain_line(FOLDED_HINT);
        assert_eq!(marked(&line, 1), "\u{2026}");
        let (row, buf) = marked_row(&line, 0, ColorMode::TrueColor);
        assert!(row.is_empty());
        assert!(buf.content().iter().all(|cell| cell.symbol() == " "));
    }

    /// The plain writer keeps its old behaviour: a hard clip with no mark, so a
    /// caller that wants the pre-LUM-1412 frame still has one.
    #[test]
    fn the_plain_writer_still_clips_without_a_mark() {
        let line = plain_line(FOLDED_HINT);
        let theme = builtin_theme("dark", ColorMode::None).expect("plain theme");
        let area = Rect {
            x: 0,
            y: 0,
            width: 44,
            height: 1,
        };
        let mut buf = Buffer::empty(area);
        write_styled_line(&mut buf, 0, 0, 44, &line, &theme);
        let row: String = (0..44)
            .map(|x| buf.cell((x, 0)).expect("cell").symbol().to_string())
            .collect();
        assert_eq!(
            row.trim_end(),
            "hints hidden on a short terminal — Alt+H sho"
        );
    }
}
