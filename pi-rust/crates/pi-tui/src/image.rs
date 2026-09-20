//! `Image` — the port of `packages/tui/src/components/image.ts` (127 lines).
//!
//! The component is the last link of the terminal-image chain: it owns a
//! base64 payload plus its pixel size, asks [`terminal_image::render_image`]
//! for the inline-image sequence, and returns the rows the TUI must account
//! for — the kitty sequence on the first row, or (iTerm2) `rows - 1` empty
//! rows followed by a cursor-up + sequence, exactly as upstream does. When the
//! terminal has no image protocol — or the render returns nothing — it degrades
//! to the single-line [`terminal_image::image_fallback`] label.
//!
//! # Divergence from the issue text: `Vec<StyledLine>`, not `Vec<String>`
//!
//! This crate's [`Component`] trait renders to
//! [`StyledLine`](crate::styled::StyledLine)s whose [`SpanStyle`] slots the
//! host resolves against the live [`Theme`](crate::theme::Theme) at write time
//! (`component.rs`, LUM-1184). That is why [`ImageTheme`] here is a style slot
//! rather than upstream's `fallbackColor: (str) => string` closure, and why
//! `Component::render` returns styled lines: a component can never hard-code a
//! colour and a theme swap takes effect on the next frame.
//!
//! The upstream-shaped string view is still available as
//! [`Image::render_lines`], and the escape-sequence lines carry a
//! [`SpanStyle::PLAIN`] span — the kitty/iTerm2 sequences *are* the content of
//! those rows. Truncation is therefore the only place the ANSI/OSC-8 aware
//! [`truncate_to_width`] matters, and it runs on text that
//! [`terminal_image::image_fallback`] has already decorated with an OSC 8
//! `file://` link for absolute filenames — the crate-native equivalent of
//! upstream's "style the fallback, then truncate it".
//!
//! ```no_run
//! use pi_tui::{Component, Image, ImageTheme, SpanStyle, ThemeColor};
//!
//! let image = Image::new("iVBORw0KGgoAAAANSUhEUgAAAUAAAADw", "image/png",
//!     ImageTheme::fallback(SpanStyle::fg(ThemeColor::ToolOutput)));
//! let lines = Component::render(&image, 80);
//! # let _ = lines;
//! ```

use parking_lot::Mutex;

use crate::component::Component;
use crate::hyperlink::{close_hyperlink, visible_width};
use crate::styled::{plain_text, SpanStyle, StyledLine, StyledSpan};
use crate::terminal_image::{
    allocate_image_id, get_capabilities, get_cell_dimensions, get_image_dimensions, image_fallback,
    render_image, CellDimensions, ImageDimensions, ImageProtocol, ImageRenderOptions,
};

/// The default ellipsis [`truncate_to_width`] appends.
const ELLIPSIS: &str = "...";

/// The pixel size [`Image`] falls back to when the payload carries no header.
///
/// Upstream's `{ widthPx: 800, heightPx: 600 }` (`image.ts:43`).
const DEFAULT_DIMENSIONS: ImageDimensions = ImageDimensions {
    width_px: 800,
    height_px: 600,
};

/// Truncate `text` to at most `max_width` visible columns.
///
/// The Rust counterpart of upstream's `truncateToWidth`
/// (`packages/tui/src/utils.ts:1064`), reduced to the fixed `"..."` ellipsis
/// and no padding (its two extra knobs have no consumer in this port):
///
/// * Visible width `<= max_width` returns `text` unchanged — no rewriting at
///   all, not even a re-wrap of the escapes.
/// * Otherwise the result is the longest prefix that fits `max_width - 3`
///   visible columns plus `"..."`. A `max_width` of `0` yields `""`; `1..=3`
///   yields the ellipsis clipped to `max_width`.
/// * ANSI (CSI) and OSC sequences are copied verbatim and never cut in half:
///   their width is zero, so [`visible_width`] — the crate's ANSI/OSC-8 aware
///   width — is the only width algorithm involved.
/// * If the retained prefix leaves an OSC 8 hyperlink open, the result is
///   closed with [`close_hyperlink`] so the ellipsis (and whatever the host
///   prints next) cannot inherit the link.
///
/// Upstream additionally pads, handles tabs and drops trailing SGR resets;
/// none of that is reachable from the image fallback, which is plain text plus
/// at most one OSC 8 link.
pub fn truncate_to_width(text: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }
    if visible_width(text) <= max_width {
        return text.to_string();
    }
    if max_width <= ELLIPSIS.len() {
        // Upstream clips the ellipsis itself when it does not fit.
        return ELLIPSIS[..max_width].to_string();
    }

    let target = max_width - ELLIPSIS.len();
    let (mut kept, link_open) = copy_prefix_to_width(text, target);
    kept.push_str(ELLIPSIS);
    if link_open {
        kept.push_str(&close_hyperlink());
    }
    kept
}

/// Copy the longest prefix of `text` whose visible width is at most `target`.
///
/// Returns the prefix (escape sequences included, never split) and whether an
/// OSC 8 hyperlink was still open when the cut was reached.
fn copy_prefix_to_width(text: &str, target: usize) -> (String, bool) {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut visible = 0usize;
    let mut link_open = false;
    let mut i = 0usize;

    while i < bytes.len() && visible < target {
        if bytes[i] != 0x1b {
            let ch = text[i..].chars().next().expect("index is a char boundary");
            out.push(ch);
            visible += 1;
            i += ch.len_utf8();
            continue;
        }
        // Escape sequence: CSI (ESC [ … final) or OSC (ESC ] … BEL / ST).
        // Both are zero-width and are copied whole, so a cut never lands
        // inside one.
        match bytes.get(i + 1) {
            Some(b'[') => {
                let start = i;
                i += 2;
                while i < bytes.len() && !(0x40..=0x7e).contains(&bytes[i]) {
                    i += 1;
                }
                i = (i + 1).min(bytes.len());
                out.push_str(&text[start..i]);
            }
            Some(b']') => {
                let start = i;
                i += 2;
                let payload_start = i;
                let mut payload_end = bytes.len();
                while i < bytes.len() {
                    if bytes[i] == 0x07 {
                        payload_end = i;
                        i += 1;
                        break;
                    }
                    if bytes[i] == 0x1b && bytes.get(i + 1) == Some(&b'\\') {
                        payload_end = i;
                        i += 2;
                        break;
                    }
                    i += 1;
                }
                out.push_str(&text[start..i]);
                // OSC 8 payload is `8;params;URI`; an empty URI closes.
                if let Some(rest) = text[payload_start..payload_end].strip_prefix("8;") {
                    if let Some((_params, uri)) = rest.split_once(';') {
                        link_open = !uri.is_empty();
                    }
                }
            }
            // A lone or unsupported escape: upstream's parser fails open by
            // dropping the ESC byte; the width of what follows is unchanged.
            Some(_) | None => i += 1,
        }
    }

    (out, link_open)
}

/// The style slot an [`Image`] paints its fallback label with.
///
/// Upstream's `ImageTheme` is `{ fallbackColor: (str) => string }`
/// (`image.ts:9-11`); in this crate the consumer passes the slot instead of a
/// pre-resolved colour — `ImageTheme::fallback(
/// SpanStyle::fg(ThemeColor::ToolOutput))` at the tool-result call site
/// (`tool-execution.ts:391`) and `ThemeColor::Muted` for the announcement
/// (`earendil-announcement.ts:44`). The host resolves it through the live
/// theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ImageTheme {
    /// Fallback-label style slot. [`SpanStyle::PLAIN`] is the default.
    pub fallback: SpanStyle,
}

impl ImageTheme {
    /// A theme whose fallback label carries `slot`.
    pub fn fallback(slot: SpanStyle) -> Self {
        Self { fallback: slot }
    }
}

/// Construction options for [`Image`].
///
/// Mirrors upstream's `ImageOptions` (`image.ts:13-19`). Every field is
/// optional; `image_id` is only meaningful for kitty, where it reuses an
/// existing image (animations, updates).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ImageOptions {
    /// Maximum render width in terminal cells. Defaults to `60`.
    pub max_width_cells: Option<u32>,
    /// Maximum render height in terminal cells.
    ///
    /// Defaults to the upstream formula `ceil(max_width * cellW / cellH)`,
    /// i.e. a height that keeps the width's aspect budget.
    pub max_height_cells: Option<u32>,
    /// Filename shown in the fallback label (and linked when absolute).
    pub filename: Option<String>,
    /// Kitty image id to reuse instead of allocating a fresh one.
    pub image_id: Option<u32>,
}

/// One rendered frame plus the width it was rendered for.
#[derive(Debug)]
struct CachedImage {
    width: u16,
    lines: Vec<StyledLine>,
}

/// A terminal image component.
///
/// Render it through [`Component::render`] (the host path) or read the
/// upstream-shaped strings with [`Image::render_lines`]. The lines are cached
/// per width; [`Image::invalidate`] drops the cache, which is what a consumer
/// must call after writing a new payload or changing the image id.
#[derive(Debug)]
pub struct Image {
    base64_data: String,
    mime_type: String,
    dimensions: ImageDimensions,
    theme: ImageTheme,
    options: ImageOptions,
    image_id: Mutex<Option<u32>>,
    cached: Mutex<Option<CachedImage>>,
}

impl Image {
    /// Build an image from its payload, its MIME type and a fallback theme.
    ///
    /// The pixel size is parsed out of `base64_data`
    /// ([`get_image_dimensions`]); a payload whose header this crate cannot
    /// read falls back to `800x600`, as upstream's constructor does
    /// (`image.ts:43`). Use [`Image::with_dimensions`] to supply the size
    /// explicitly and [`Image::with_options`] to set the size budget, the
    /// filename or a kitty image id.
    pub fn new(
        base64_data: impl Into<String>,
        mime_type: impl Into<String>,
        theme: ImageTheme,
    ) -> Self {
        let base64_data = base64_data.into();
        let mime_type = mime_type.into();
        let dimensions =
            get_image_dimensions(&base64_data, &mime_type).unwrap_or(DEFAULT_DIMENSIONS);
        Self {
            base64_data,
            mime_type,
            dimensions,
            theme,
            options: ImageOptions::default(),
            image_id: Mutex::new(None),
            cached: Mutex::new(None),
        }
    }

    /// Replace the options (and seed the kitty image id from `options.image_id`).
    pub fn with_options(mut self, options: ImageOptions) -> Self {
        self.image_id = Mutex::new(options.image_id);
        self.options = options;
        self
    }

    /// Override the parsed pixel size.
    pub fn with_dimensions(mut self, dimensions: ImageDimensions) -> Self {
        self.dimensions = dimensions;
        self
    }

    /// The kitty image id this component uses, if one was allocated or given.
    pub fn get_image_id(&self) -> Option<u32> {
        *self.image_id.lock()
    }

    /// Drop the cached lines so the next render recomputes them.
    pub fn invalidate(&self) {
        *self.cached.lock() = None;
    }

    /// The rendered rows as strings — upstream's `Component.render` shape.
    ///
    /// The styling slot of the fallback row is resolved by the host on the
    /// [`Component::render`] path, so it is invisible here; everything else
    /// (the kitty/iTerm2 sequences, the row count, the truncation) is
    /// identical.
    pub fn render_lines(&self, width: u16) -> Vec<String> {
        Component::render(self, width)
            .iter()
            .map(|line| plain_text(line))
            .collect()
    }

    /// The fallback label, truncated to `width` with the ANSI/OSC-8 aware
    /// helper. `image_fallback` may already have wrapped an absolute filename
    /// in an OSC 8 link, which is what makes the aware truncation necessary.
    fn fallback_line(&self, width: u16) -> StyledLine {
        let fallback = image_fallback(
            &self.mime_type,
            Some(self.dimensions),
            self.options.filename.as_deref(),
        );
        let text = truncate_to_width(&fallback, usize::from(width));
        vec![StyledSpan::new(text, self.theme.fallback)]
    }

    /// The uncached render. Mirrors upstream's `render` body (`image.ts:52-125`).
    fn build_lines(&self, width: u16) -> Vec<StyledLine> {
        let max_width = u32::from(width.saturating_sub(2))
            .min(self.options.max_width_cells.unwrap_or(60))
            .max(1);
        let cell_dimensions = get_cell_dimensions();
        let default_max_height = default_max_height(max_width, cell_dimensions);
        let max_height = self.options.max_height_cells.unwrap_or(default_max_height);

        let Some(protocol) = get_capabilities().images else {
            return vec![self.fallback_line(width)];
        };

        if protocol == ImageProtocol::Kitty && self.image_id.lock().is_none() {
            *self.image_id.lock() = Some(allocate_image_id());
        }
        let image_id = self.get_image_id();

        let result = render_image(
            &self.base64_data,
            self.dimensions,
            &ImageRenderOptions {
                max_width_cells: Some(max_width),
                max_height_cells: Some(max_height),
                image_id,
                preserve_aspect_ratio: None,
                move_cursor: Some(false),
            },
        );

        let Some(result) = result else {
            return vec![self.fallback_line(width)];
        };
        // Kitty echoes the id it rendered with; keep it for later cleanup.
        if let Some(rendered_id) = result.image_id {
            *self.image_id.lock() = Some(rendered_id);
        }

        let mut lines: Vec<StyledLine> = Vec::with_capacity(result.rows as usize);
        if protocol == ImageProtocol::Kitty {
            // C=1 keeps the cursor put, so the sequence goes on row 1 and the
            // remaining rows are accounted for as empty lines.
            lines.push(sequence_line(&result.sequence));
            for _ in 1..result.rows {
                lines.push(Vec::new());
            }
        } else {
            // iTerm2: empty rows first, then cursor-up + sequence on the last
            // row so the image lands inside the scroll area.
            for _ in 1..result.rows {
                lines.push(Vec::new());
            }
            let move_up = if result.rows > 1 {
                format!("\u{1b}[{}A", result.rows - 1)
            } else {
                String::new()
            };
            lines.push(sequence_line(&format!("{move_up}{}", result.sequence)));
        }
        lines
    }
}

impl Component for Image {
    fn render(&self, width: u16) -> Vec<StyledLine> {
        if let Some(cached) = self.cached.lock().as_ref() {
            if cached.width == width {
                return cached.lines.clone();
            }
        }
        let lines = self.build_lines(width);
        *self.cached.lock() = Some(CachedImage {
            width,
            lines: lines.clone(),
        });
        lines
    }
}

/// One row whose whole content is an inline-image escape sequence.
fn sequence_line(sequence: &str) -> StyledLine {
    vec![StyledSpan::new(sequence.to_string(), SpanStyle::PLAIN)]
}

/// Upstream's `Math.max(1, Math.ceil(maxWidth * cellW / cellH))`
/// (`image.ts:56-57`). A zero cell height is treated as `1` rather than
/// dividing by zero.
fn default_max_height(max_width: u32, cell_dimensions: CellDimensions) -> u32 {
    let width_px = u64::from(max_width) * u64::from(cell_dimensions.width_px);
    let height_px = u64::from(cell_dimensions.height_px).max(1);
    let rows = width_px.div_ceil(height_px);
    u32::try_from(rows).unwrap_or(u32::MAX).max(1)
}
