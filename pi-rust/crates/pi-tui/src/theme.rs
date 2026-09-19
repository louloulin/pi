//! Theme model, JSON loading/validation and ANSI colour resolution.
//!
//! Rust port of `packages/coding-agent/src/modes/interactive/theme/theme.ts`
//! and `theme-json.ts` (plus the `dark.json` / `light.json` built-ins, embedded
//! verbatim below). The TS module is a 1234-line file that mixes palette
//! resolution with chalk, a file watcher, syntax highlighting and per-component
//! theme adapters; this port keeps the parts that are pure data + string
//! formatting:
//!
//! * [`ThemeColor`] / [`ThemeBg`] — the 49 foreground and 7 background slots.
//! * [`ThemeJson`] — the on-disk document (`vars` / `colors` / `export`),
//!   including the required-token validation the typebox schema performs
//!   upstream, with the same "missing required color tokens" report.
//! * [`Theme`] — resolved palette with the same fallbacks for the five optional
//!   slots and the same `fg()` / `bg()` ANSI wrapping (including the
//!   colour-only resets `\x1b[39m` / `\x1b[49m`).
//! * [`ThemeController`] — name → theme resolution (built-in, then the custom
//!   themes directory), with the upstream "invalid theme falls back to dark"
//!   behaviour.
//!
//! Deliberately **not** ported here (belongs to the coding-agent layer that
//! consumes the palette): the chalk proxies, the `fs.watch` live reload, the
//! shiki/CLI highlight adapters, `MarkdownTheme` / `SelectListTheme` /
//! `SettingsListTheme` adapters and the `theme` global proxy.
//!
//! # Examples
//!
//! ```
//! use pi_tui::theme::{builtin_theme, ColorMode, ThemeBg, ThemeColor};
//!
//! let theme = builtin_theme("dark", ColorMode::TrueColor).unwrap();
//! assert_eq!(theme.get_fg_ansi(ThemeColor::Accent), "\x1b[38;2;138;190;183m");
//! assert_eq!(
//!     theme.bg(ThemeBg::SelectedBg, "x"),
//!     "\x1b[48;2;58;58;74mx\x1b[49m"
//! );
//! ```

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};

use pi_agent_core::ThinkingLevel;
use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

/// The built-in `dark` theme, byte-identical to upstream `dark.json`.
pub const BUILTIN_DARK_JSON: &str = include_str!("../assets/themes/dark.json");

/// The built-in `light` theme, byte-identical to upstream `light.json`.
pub const BUILTIN_LIGHT_JSON: &str = include_str!("../assets/themes/light.json");

/// Environment variable that overrides the agent config directory
/// (upstream `PI_CODING_AGENT_DIR`).
pub const ENV_AGENT_DIR: &str = "PI_CODING_AGENT_DIR";

/// Environment variable the terminal background hint is read from
/// (upstream `COLORFGBG`).
pub const ENV_COLORFGBG: &str = "COLORFGBG";

// ============================================================================
// Errors
// ============================================================================

/// Everything that can go wrong while parsing, resolving or loading a theme.
#[derive(Debug, Error, PartialEq, Eq, Clone)]
pub enum ThemeError {
    /// The document is not valid JSON.
    #[error("Failed to parse theme {label}: {message}")]
    InvalidJson {
        /// Label naming the document (a path or theme name).
        label: String,
        /// The underlying serde message.
        message: String,
    },
    /// A theme name contained `/`, which is reserved for `light/dark` auto
    /// settings upstream.
    #[error(
        "Invalid theme name \"{name}\": theme names cannot contain \"/\" because it is reserved \
         for automatic light/dark theme settings."
    )]
    InvalidThemeName {
        /// The offending name.
        name: String,
    },
    /// One or more required colour tokens are absent (upstream typebox report).
    #[error("{}", format_missing_colors(label, missing))]
    MissingRequiredColors {
        /// Label naming the theme.
        label: String,
        /// The missing tokens, sorted.
        missing: Vec<String>,
    },
    /// A required colour token is missing after fallback resolution.
    #[error("Invalid theme {label}: missing color token \"{token}\"")]
    MissingColorToken {
        /// Label naming the theme.
        label: String,
        /// The token name.
        token: String,
    },
    /// A colour string was not `#rrggbb`.
    #[error("Invalid hex color: {0}")]
    InvalidHexColor(String),
    /// A value that should have been resolved still references a variable.
    #[error("Invalid color value: {0}")]
    InvalidColorValue(String),
    /// A `vars` reference that does not exist.
    #[error("Variable reference not found: {0}")]
    VariableNotFound(String),
    /// Two `vars` referencing each other.
    #[error("Circular variable reference detected: {0}")]
    CircularVariableReference(String),
    /// The theme is neither built-in nor present in the custom themes dir.
    #[error("Theme not found: {0}")]
    ThemeNotFound(String),
    /// Reading a theme file failed.
    #[error("Failed to read theme {path}: {message}")]
    Io {
        /// Path that could not be read.
        path: String,
        /// The underlying OS message.
        message: String,
    },
}

fn format_missing_colors(label: &str, missing: &[String]) -> String {
    let mut message = format!("Invalid theme \"{label}\":\n");
    if !missing.is_empty() {
        message.push_str("\nMissing required color tokens:\n");
        message.push_str(
            &missing
                .iter()
                .map(|color| format!("  - {color}"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
        message.push_str("\n\nPlease add these colors to your theme's \"colors\" object.");
        message.push_str("\nSee the built-in themes (dark.json, light.json) for reference values.");
    }
    message
}

// ============================================================================
// Colour values
// ============================================================================

/// A theme colour value.
///
/// Mirrors the TypeScript `ColorValue = string | number` union: a `#rrggbb`
/// literal, the empty string (terminal default), a 256-colour index, or — before
/// resolution — a `vars` reference.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ColorValue {
    /// The empty string: use the terminal's default colour.
    Reset,
    /// A `#rrggbb` literal.
    Hex(String),
    /// A 256-colour palette index (`0..=255`).
    Index(u8),
    /// A reference into the document's `vars` map. Resolution replaces this;
    /// finding one afterwards is an error.
    Var(String),
}

impl ColorValue {
    /// Classify a raw JSON string the way upstream does: `""` → default,
    /// `#…` → hex literal, anything else → variable reference.
    pub fn from_text(text: &str) -> Self {
        if text.is_empty() {
            Self::Reset
        } else if text.starts_with('#') {
            Self::Hex(text.to_string())
        } else {
            Self::Var(text.to_string())
        }
    }

    /// The hex literal, when this value is one.
    pub fn as_hex(&self) -> Option<&str> {
        match self {
            Self::Hex(hex) => Some(hex),
            _ => None,
        }
    }
}

impl fmt::Display for ColorValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Reset => f.write_str("\"\""),
            Self::Hex(hex) => write!(f, "{hex}"),
            Self::Index(index) => write!(f, "{index}"),
            Self::Var(name) => write!(f, "{name}"),
        }
    }
}

impl Serialize for ColorValue {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Reset => serializer.serialize_str(""),
            Self::Hex(hex) => serializer.serialize_str(hex),
            Self::Var(name) => serializer.serialize_str(name),
            Self::Index(index) => serializer.serialize_u8(*index),
        }
    }
}

struct ColorValueVisitor;

impl<'de> Visitor<'de> for ColorValueVisitor {
    type Value = ColorValue;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a hex color string, a variable name, or a 0..=255 palette index")
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
        Ok(ColorValue::from_text(value))
    }

    fn visit_i64<E: de::Error>(self, value: i64) -> Result<Self::Value, E> {
        if (0..=255).contains(&value) {
            Ok(ColorValue::Index(value as u8))
        } else {
            Err(E::custom(format!("palette index out of range: {value}")))
        }
    }

    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Self::Value, E> {
        if value <= 255 {
            Ok(ColorValue::Index(value as u8))
        } else {
            Err(E::custom(format!("palette index out of range: {value}")))
        }
    }
}

impl<'de> Deserialize<'de> for ColorValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(ColorValueVisitor)
    }
}

// ============================================================================
// Theme slots
// ============================================================================

/// A foreground theme colour (upstream `ThemeColor`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ThemeColor {
    /// Primary accent colour.
    Accent,
    /// Default border colour.
    Border,
    /// Highlighted border colour.
    BorderAccent,
    /// Muted border colour.
    BorderMuted,
    /// Success text.
    Success,
    /// Error text.
    Error,
    /// Warning text.
    Warning,
    /// Muted text.
    Muted,
    /// Dimmed text.
    Dim,
    /// Default text.
    Text,
    /// Reasoning / thinking text.
    ThinkingText,
    /// Scrollbar track (optional; falls back to [`ThemeColor::Muted`]).
    ScrollbarTrack,
    /// Scrollbar thumb (optional; falls back to [`ThemeColor::Text`]).
    ScrollbarThumb,
    /// Search match text (optional; falls back to [`ThemeColor::Text`]).
    SearchMatchText,
    /// User message text.
    UserMessageText,
    /// Custom message text.
    CustomMessageText,
    /// Custom message label.
    CustomMessageLabel,
    /// Tool title.
    ToolTitle,
    /// Tool output.
    ToolOutput,
    /// Markdown heading.
    MdHeading,
    /// Markdown link text.
    MdLink,
    /// Markdown link URL.
    MdLinkUrl,
    /// Markdown inline code.
    MdCode,
    /// Markdown code block body.
    MdCodeBlock,
    /// Markdown code block border.
    MdCodeBlockBorder,
    /// Markdown quote body.
    MdQuote,
    /// Markdown quote border.
    MdQuoteBorder,
    /// Markdown horizontal rule.
    MdHr,
    /// Markdown list bullet.
    MdListBullet,
    /// Added lines in tool diffs.
    ToolDiffAdded,
    /// Removed lines in tool diffs.
    ToolDiffRemoved,
    /// Context lines in tool diffs.
    ToolDiffContext,
    /// Syntax: comments.
    SyntaxComment,
    /// Syntax: keywords.
    SyntaxKeyword,
    /// Syntax: function names.
    SyntaxFunction,
    /// Syntax: variables.
    SyntaxVariable,
    /// Syntax: strings.
    SyntaxString,
    /// Syntax: numbers.
    SyntaxNumber,
    /// Syntax: types.
    SyntaxType,
    /// Syntax: operators.
    SyntaxOperator,
    /// Syntax: punctuation.
    SyntaxPunctuation,
    /// Thinking border: off.
    ThinkingOff,
    /// Thinking border: minimal.
    ThinkingMinimal,
    /// Thinking border: low.
    ThinkingLow,
    /// Thinking border: medium.
    ThinkingMedium,
    /// Thinking border: high.
    ThinkingHigh,
    /// Thinking border: extra high.
    ThinkingXhigh,
    /// Thinking border: max (optional; falls back to [`ThemeColor::ThinkingXhigh`]).
    ThinkingMax,
    /// Bash mode border.
    BashMode,
}

impl ThemeColor {
    /// Every foreground slot, in upstream declaration order.
    pub const ALL: [ThemeColor; 49] = [
        ThemeColor::Accent,
        ThemeColor::Border,
        ThemeColor::BorderAccent,
        ThemeColor::BorderMuted,
        ThemeColor::Success,
        ThemeColor::Error,
        ThemeColor::Warning,
        ThemeColor::Muted,
        ThemeColor::Dim,
        ThemeColor::Text,
        ThemeColor::ThinkingText,
        ThemeColor::ScrollbarTrack,
        ThemeColor::ScrollbarThumb,
        ThemeColor::SearchMatchText,
        ThemeColor::UserMessageText,
        ThemeColor::CustomMessageText,
        ThemeColor::CustomMessageLabel,
        ThemeColor::ToolTitle,
        ThemeColor::ToolOutput,
        ThemeColor::MdHeading,
        ThemeColor::MdLink,
        ThemeColor::MdLinkUrl,
        ThemeColor::MdCode,
        ThemeColor::MdCodeBlock,
        ThemeColor::MdCodeBlockBorder,
        ThemeColor::MdQuote,
        ThemeColor::MdQuoteBorder,
        ThemeColor::MdHr,
        ThemeColor::MdListBullet,
        ThemeColor::ToolDiffAdded,
        ThemeColor::ToolDiffRemoved,
        ThemeColor::ToolDiffContext,
        ThemeColor::SyntaxComment,
        ThemeColor::SyntaxKeyword,
        ThemeColor::SyntaxFunction,
        ThemeColor::SyntaxVariable,
        ThemeColor::SyntaxString,
        ThemeColor::SyntaxNumber,
        ThemeColor::SyntaxType,
        ThemeColor::SyntaxOperator,
        ThemeColor::SyntaxPunctuation,
        ThemeColor::ThinkingOff,
        ThemeColor::ThinkingMinimal,
        ThemeColor::ThinkingLow,
        ThemeColor::ThinkingMedium,
        ThemeColor::ThinkingHigh,
        ThemeColor::ThinkingXhigh,
        ThemeColor::ThinkingMax,
        ThemeColor::BashMode,
    ];

    /// The JSON key used for this slot.
    pub fn key(self) -> &'static str {
        match self {
            ThemeColor::Accent => "accent",
            ThemeColor::Border => "border",
            ThemeColor::BorderAccent => "borderAccent",
            ThemeColor::BorderMuted => "borderMuted",
            ThemeColor::Success => "success",
            ThemeColor::Error => "error",
            ThemeColor::Warning => "warning",
            ThemeColor::Muted => "muted",
            ThemeColor::Dim => "dim",
            ThemeColor::Text => "text",
            ThemeColor::ThinkingText => "thinkingText",
            ThemeColor::ScrollbarTrack => "scrollbarTrack",
            ThemeColor::ScrollbarThumb => "scrollbarThumb",
            ThemeColor::SearchMatchText => "searchMatchText",
            ThemeColor::UserMessageText => "userMessageText",
            ThemeColor::CustomMessageText => "customMessageText",
            ThemeColor::CustomMessageLabel => "customMessageLabel",
            ThemeColor::ToolTitle => "toolTitle",
            ThemeColor::ToolOutput => "toolOutput",
            ThemeColor::MdHeading => "mdHeading",
            ThemeColor::MdLink => "mdLink",
            ThemeColor::MdLinkUrl => "mdLinkUrl",
            ThemeColor::MdCode => "mdCode",
            ThemeColor::MdCodeBlock => "mdCodeBlock",
            ThemeColor::MdCodeBlockBorder => "mdCodeBlockBorder",
            ThemeColor::MdQuote => "mdQuote",
            ThemeColor::MdQuoteBorder => "mdQuoteBorder",
            ThemeColor::MdHr => "mdHr",
            ThemeColor::MdListBullet => "mdListBullet",
            ThemeColor::ToolDiffAdded => "toolDiffAdded",
            ThemeColor::ToolDiffRemoved => "toolDiffRemoved",
            ThemeColor::ToolDiffContext => "toolDiffContext",
            ThemeColor::SyntaxComment => "syntaxComment",
            ThemeColor::SyntaxKeyword => "syntaxKeyword",
            ThemeColor::SyntaxFunction => "syntaxFunction",
            ThemeColor::SyntaxVariable => "syntaxVariable",
            ThemeColor::SyntaxString => "syntaxString",
            ThemeColor::SyntaxNumber => "syntaxNumber",
            ThemeColor::SyntaxType => "syntaxType",
            ThemeColor::SyntaxOperator => "syntaxOperator",
            ThemeColor::SyntaxPunctuation => "syntaxPunctuation",
            ThemeColor::ThinkingOff => "thinkingOff",
            ThemeColor::ThinkingMinimal => "thinkingMinimal",
            ThemeColor::ThinkingLow => "thinkingLow",
            ThemeColor::ThinkingMedium => "thinkingMedium",
            ThemeColor::ThinkingHigh => "thinkingHigh",
            ThemeColor::ThinkingXhigh => "thinkingXhigh",
            ThemeColor::ThinkingMax => "thinkingMax",
            ThemeColor::BashMode => "bashMode",
        }
    }

    /// The fallback slot upstream substitutes when this one is absent, if any.
    pub fn fallback(self) -> Option<ThemeColor> {
        match self {
            ThemeColor::ScrollbarTrack => Some(ThemeColor::Muted),
            ThemeColor::ScrollbarThumb => Some(ThemeColor::Text),
            ThemeColor::SearchMatchText => Some(ThemeColor::Text),
            ThemeColor::ThinkingMax => Some(ThemeColor::ThinkingXhigh),
            _ => None,
        }
    }

    /// True when the token may be omitted from a theme document.
    pub fn is_optional(self) -> bool {
        self.fallback().is_some()
    }

    fn from_key(key: &str) -> Option<ThemeColor> {
        ThemeColor::ALL.into_iter().find(|slot| slot.key() == key)
    }
}

impl fmt::Display for ThemeColor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.key())
    }
}

/// A background theme colour (upstream `ThemeBg`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ThemeBg {
    /// Background of the selected list row.
    SelectedBg,
    /// Background of a search match (optional; falls back to
    /// [`ThemeBg::SelectedBg`]).
    SearchMatchBg,
    /// Background of a user message.
    UserMessageBg,
    /// Background of a custom message.
    CustomMessageBg,
    /// Background of a pending tool call.
    ToolPendingBg,
    /// Background of a successful tool call.
    ToolSuccessBg,
    /// Background of a failed tool call.
    ToolErrorBg,
}

impl ThemeBg {
    /// Every background slot, in upstream declaration order.
    pub const ALL: [ThemeBg; 7] = [
        ThemeBg::SelectedBg,
        ThemeBg::SearchMatchBg,
        ThemeBg::UserMessageBg,
        ThemeBg::CustomMessageBg,
        ThemeBg::ToolPendingBg,
        ThemeBg::ToolSuccessBg,
        ThemeBg::ToolErrorBg,
    ];

    /// The JSON key used for this slot.
    pub fn key(self) -> &'static str {
        match self {
            ThemeBg::SelectedBg => "selectedBg",
            ThemeBg::SearchMatchBg => "searchMatchBg",
            ThemeBg::UserMessageBg => "userMessageBg",
            ThemeBg::CustomMessageBg => "customMessageBg",
            ThemeBg::ToolPendingBg => "toolPendingBg",
            ThemeBg::ToolSuccessBg => "toolSuccessBg",
            ThemeBg::ToolErrorBg => "toolErrorBg",
        }
    }

    /// The fallback slot upstream substitutes when this one is absent, if any.
    pub fn fallback(self) -> Option<ThemeBg> {
        match self {
            ThemeBg::SearchMatchBg => Some(ThemeBg::SelectedBg),
            _ => None,
        }
    }

    /// True when the token may be omitted from a theme document.
    pub fn is_optional(self) -> bool {
        self.fallback().is_some()
    }

    fn from_key(key: &str) -> Option<ThemeBg> {
        ThemeBg::ALL.into_iter().find(|slot| slot.key() == key)
    }
}

impl fmt::Display for ThemeBg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.key())
    }
}

// ============================================================================
// Color utilities (upstream theme.ts)
// ============================================================================

/// The 6x6x6 colour-cube channel values (indices 0-5).
const CUBE_VALUES: [u8; 6] = [0, 95, 135, 175, 215, 255];

/// Grayscale ramp values (indices 232-255, 24 grays from 8 to 238).
const GRAY_VALUES: [u8; 24] = [
    8, 18, 28, 38, 48, 58, 68, 78, 88, 98, 108, 118, 128, 138, 148, 158, 168, 178, 188, 198, 208,
    218, 228, 238,
];

/// Which ANSI colour depth a [`Theme`] renders for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ColorMode {
    /// 24-bit `\x1b[38;2;r;g;bm` output.
    #[default]
    TrueColor,
    /// 8-bit `\x1b[38;5;Nm` output; hex colours are quantised.
    Ansi256,
}

impl ColorMode {
    /// Pick the mode the way upstream `createTheme` does from
    /// `getCapabilities().trueColor`.
    pub fn from_true_color(true_color: bool) -> Self {
        if true_color {
            Self::TrueColor
        } else {
            Self::Ansi256
        }
    }
}

/// Parse `#rrggbb` into channel values (upstream `hexToRgb`).
pub fn hex_to_rgb(hex: &str) -> Result<(u8, u8, u8), ThemeError> {
    let cleaned = hex.strip_prefix('#').unwrap_or(hex);
    if cleaned.len() != 6 {
        return Err(ThemeError::InvalidHexColor(hex.to_string()));
    }
    let parse = |range: std::ops::Range<usize>| {
        u8::from_str_radix(&cleaned[range], 16)
            .map_err(|_| ThemeError::InvalidHexColor(hex.to_string()))
    };
    Ok((parse(0..2)?, parse(2..4)?, parse(4..6)?))
}

fn find_closest_cube_index(value: u8) -> usize {
    let mut min_dist = f64::INFINITY;
    let mut min_idx = 0;
    for (index, cube) in CUBE_VALUES.iter().enumerate() {
        let dist = (value as f64 - *cube as f64).abs();
        if dist < min_dist {
            min_dist = dist;
            min_idx = index;
        }
    }
    min_idx
}

fn find_closest_gray_index(gray: u8) -> usize {
    let mut min_dist = f64::INFINITY;
    let mut min_idx = 0;
    for (index, value) in GRAY_VALUES.iter().enumerate() {
        let dist = (gray as f64 - *value as f64).abs();
        if dist < min_dist {
            min_dist = dist;
            min_idx = index;
        }
    }
    min_idx
}

/// Weighted Euclidean distance (the eye is more sensitive to green).
fn color_distance(left: (u8, u8, u8), right: (u8, u8, u8)) -> f64 {
    let dr = left.0 as f64 - right.0 as f64;
    let dg = left.1 as f64 - right.1 as f64;
    let db = left.2 as f64 - right.2 as f64;
    dr * dr * 0.299 + dg * dg * 0.587 + db * db * 0.114
}

/// Quantise RGB to the nearest xterm-256 index (upstream `rgbTo256`).
pub fn rgb_to_256(r: u8, g: u8, b: u8) -> u8 {
    let r_idx = find_closest_cube_index(r);
    let g_idx = find_closest_cube_index(g);
    let b_idx = find_closest_cube_index(b);
    let cube = (CUBE_VALUES[r_idx], CUBE_VALUES[g_idx], CUBE_VALUES[b_idx]);
    let cube_index = 16 + 36 * r_idx as u16 + 6 * g_idx as u16 + b_idx as u16;
    let cube_dist = color_distance((r, g, b), cube);

    let gray = (0.299 * r as f64 + 0.587 * g as f64 + 0.114 * b as f64).round() as u8;
    let gray_idx = find_closest_gray_index(gray);
    let gray_value = GRAY_VALUES[gray_idx];
    let gray_index = 232 + gray_idx as u16;
    let gray_dist = color_distance((r, g, b), (gray_value, gray_value, gray_value));

    let max_c = r.max(g).max(b);
    let min_c = r.min(g).min(b);
    let spread = max_c - min_c;

    // Only consider grayscale when the colour is nearly neutral and closer.
    if spread < 10 && gray_dist < cube_dist {
        return gray_index as u8;
    }

    cube_index as u8
}

/// Quantise a `#rrggbb` literal to a 256-colour index (upstream `hexTo256`).
pub fn hex_to_256(hex: &str) -> Result<u8, ThemeError> {
    let (r, g, b) = hex_to_rgb(hex)?;
    Ok(rgb_to_256(r, g, b))
}

/// The xterm-256 palette entry as `#rrggbb` (upstream `ansi256ToHex`).
pub fn ansi256_to_hex(index: u8) -> String {
    const BASIC_COLORS: [&str; 16] = [
        "#000000", "#800000", "#008000", "#808000", "#000080", "#800080", "#008080", "#c0c0c0",
        "#808080", "#ff0000", "#00ff00", "#ffff00", "#0000ff", "#ff00ff", "#00ffff", "#ffffff",
    ];
    if index < 16 {
        return BASIC_COLORS[index as usize].to_string();
    }
    if index < 232 {
        let cube = index as u16 - 16;
        let to_hex = |n: u16| -> String {
            let value = if n == 0 { 0 } else { 55 + n * 40 };
            format!("{value:02x}")
        };
        let r = cube / 36;
        let g = (cube % 36) / 6;
        let b = cube % 6;
        return format!("#{}{}{}", to_hex(r), to_hex(g), to_hex(b));
    }
    let gray = 8 + (index as u16 - 232) * 10;
    format!("#{gray:02x}{gray:02x}{gray:02x}")
}

/// Foreground ANSI sequence for a resolved colour (upstream `fgAnsi`).
pub fn fg_ansi(color: &ColorValue, mode: ColorMode) -> Result<String, ThemeError> {
    match color {
        ColorValue::Reset => Ok("\x1b[39m".to_string()),
        ColorValue::Index(index) => Ok(format!("\x1b[38;5;{index}m")),
        ColorValue::Hex(hex) => match mode {
            ColorMode::TrueColor => {
                let (r, g, b) = hex_to_rgb(hex)?;
                Ok(format!("\x1b[38;2;{r};{g};{b}m"))
            }
            ColorMode::Ansi256 => Ok(format!("\x1b[38;5;{}m", hex_to_256(hex)?)),
        },
        ColorValue::Var(name) => Err(ThemeError::InvalidColorValue(name.clone())),
    }
}

/// Background ANSI sequence for a resolved colour (upstream `bgAnsi`).
pub fn bg_ansi(color: &ColorValue, mode: ColorMode) -> Result<String, ThemeError> {
    match color {
        ColorValue::Reset => Ok("\x1b[49m".to_string()),
        ColorValue::Index(index) => Ok(format!("\x1b[48;5;{index}m")),
        ColorValue::Hex(hex) => match mode {
            ColorMode::TrueColor => {
                let (r, g, b) = hex_to_rgb(hex)?;
                Ok(format!("\x1b[48;2;{r};{g};{b}m"))
            }
            ColorMode::Ansi256 => Ok(format!("\x1b[48;5;{}m", hex_to_256(hex)?)),
        },
        ColorValue::Var(name) => Err(ThemeError::InvalidColorValue(name.clone())),
    }
}

/// Follow `vars` references until a literal is reached (upstream `resolveVarRefs`).
pub fn resolve_var_refs(
    value: &ColorValue,
    vars: &BTreeMap<String, ColorValue>,
) -> Result<ColorValue, ThemeError> {
    let mut visited = HashSet::new();
    let mut current = value.clone();
    loop {
        match &current {
            ColorValue::Var(name) => {
                if !visited.insert(name.clone()) {
                    return Err(ThemeError::CircularVariableReference(name.clone()));
                }
                let next = vars
                    .get(name)
                    .ok_or_else(|| ThemeError::VariableNotFound(name.clone()))?;
                current = next.clone();
            }
            other => return Ok(other.clone()),
        }
    }
}

// ============================================================================
// Theme JSON document
// ============================================================================

/// The `export` section of a theme document (upstream `ThemeJson.export`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThemeExport {
    /// Page background for HTML export.
    #[serde(rename = "pageBg", default)]
    pub page_bg: Option<ColorValue>,
    /// Card background for HTML export.
    #[serde(rename = "cardBg", default)]
    pub card_bg: Option<ColorValue>,
    /// Info banner background for HTML export.
    #[serde(rename = "infoBg", default)]
    pub info_bg: Option<ColorValue>,
}

/// A parsed theme document (`dark.json` / `light.json` / user themes).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThemeJson {
    /// Optional `$schema` pointer; ignored beyond round-tripping.
    #[serde(rename = "$schema", default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    /// Theme name; must not contain `/`.
    pub name: String,
    /// Reusable colour variables.
    #[serde(default)]
    pub vars: BTreeMap<String, ColorValue>,
    /// Token → colour map.
    pub colors: BTreeMap<String, ColorValue>,
    /// Optional export-only colours.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub export: Option<ThemeExport>,
}

impl ThemeJson {
    /// Parse and validate a theme document (upstream
    /// `parseThemeJsonContent` + `validateThemeJson`).
    pub fn parse(label: &str, content: &str) -> Result<Self, ThemeError> {
        let document: Self =
            serde_json::from_str(strip_bom(content)).map_err(|error| ThemeError::InvalidJson {
                label: label.to_string(),
                message: error.to_string(),
            })?;
        document.validate(label)?;
        Ok(document)
    }

    /// Check the invariants the typebox schema enforces upstream: a `name`
    /// without `/`, and every required colour token present.
    pub fn validate(&self, label: &str) -> Result<(), ThemeError> {
        if self.name.contains('/') {
            return Err(ThemeError::InvalidThemeName {
                name: self.name.clone(),
            });
        }
        let mut missing: Vec<String> = ThemeColor::ALL
            .into_iter()
            .filter(|slot| !slot.is_optional())
            .map(ThemeColor::key)
            .chain(
                ThemeBg::ALL
                    .into_iter()
                    .filter(|slot| !slot.is_optional())
                    .map(ThemeBg::key),
            )
            .filter(|key| !self.colors.contains_key(*key))
            .map(str::to_string)
            .collect();
        missing.sort();
        missing.dedup();
        if missing.is_empty() {
            Ok(())
        } else {
            Err(ThemeError::MissingRequiredColors {
                label: label.to_string(),
                missing,
            })
        }
    }

    /// Apply the optional-slot fallbacks and resolve every `vars` reference.
    pub fn resolved_colors(&self) -> Result<HashMap<String, ColorValue>, ThemeError> {
        let mut colors: HashMap<String, ColorValue> = self.colors.clone().into_iter().collect();
        for slot in ThemeColor::ALL {
            if let Some(fallback) = slot.fallback() {
                if !colors.contains_key(slot.key()) {
                    if let Some(value) = colors.get(fallback.key()) {
                        colors.insert(slot.key().to_string(), value.clone());
                    }
                }
            }
        }
        let mut resolved = HashMap::with_capacity(colors.len());
        for (slot, value) in &colors {
            resolved.insert(slot.clone(), resolve_var_refs(value, &self.vars)?);
        }
        Ok(resolved)
    }

    /// Resolved colours as CSS-compatible hex strings (upstream
    /// `getResolvedThemeColors`).
    pub fn css_colors(&self) -> Result<BTreeMap<String, String>, ThemeError> {
        let resolved = self.resolved_colors()?;
        let default_text = if self.name == "light" {
            "#000000"
        } else {
            "#e5e5e7"
        };
        let mut css = BTreeMap::new();
        for (slot, value) in resolved {
            let hex = match value {
                ColorValue::Index(index) => ansi256_to_hex(index),
                ColorValue::Reset => default_text.to_string(),
                ColorValue::Hex(hex) => hex,
                ColorValue::Var(name) => return Err(ThemeError::InvalidColorValue(name)),
            };
            css.insert(slot, hex);
        }
        Ok(css)
    }
}

/// Remove a UTF-8 BOM, matching upstream `stripBom`.
pub fn strip_bom(content: &str) -> &str {
    content.strip_prefix('\u{feff}').unwrap_or(content)
}

// ============================================================================
// Theme
// ============================================================================

/// A resolved theme: every slot has an ANSI prefix for the active colour mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Theme {
    name: Option<String>,
    source_path: Option<PathBuf>,
    mode: ColorMode,
    fg: HashMap<ThemeColor, String>,
    bg: HashMap<ThemeBg, String>,
}

impl Theme {
    /// Build a theme from already-resolved slot maps.
    ///
    /// Optional slots are filled from their fallbacks exactly like the
    /// upstream constructor, and every missing required slot is an error
    /// rather than a silently uncoloured string.
    pub fn new(
        fg_values: HashMap<ThemeColor, ColorValue>,
        bg_values: HashMap<ThemeBg, ColorValue>,
        mode: ColorMode,
        name: Option<String>,
        source_path: Option<PathBuf>,
    ) -> Result<Self, ThemeError> {
        let label = name.clone().unwrap_or_else(|| "<in-memory>".to_string());
        let mut fg = HashMap::new();
        for slot in ThemeColor::ALL {
            let value = fg_values
                .get(&slot)
                .or_else(|| slot.fallback().and_then(|slot| fg_values.get(&slot)))
                .ok_or_else(|| ThemeError::MissingColorToken {
                    label: label.clone(),
                    token: slot.key().to_string(),
                })?;
            fg.insert(slot, fg_ansi(value, mode)?);
        }
        let mut bg = HashMap::new();
        for slot in ThemeBg::ALL {
            let value = bg_values
                .get(&slot)
                .or_else(|| slot.fallback().and_then(|slot| bg_values.get(&slot)))
                .ok_or_else(|| ThemeError::MissingColorToken {
                    label: label.clone(),
                    token: slot.key().to_string(),
                })?;
            bg.insert(slot, bg_ansi(value, mode)?);
        }
        Ok(Self {
            name,
            source_path,
            mode,
            fg,
            bg,
        })
    }

    /// Build a theme from a validated document (upstream `createTheme`).
    pub fn from_json(json: &ThemeJson, mode: ColorMode) -> Result<Self, ThemeError> {
        json.validate(&json.name)?;
        let resolved = json.resolved_colors()?;
        let mut fg_values = HashMap::new();
        let mut bg_values = HashMap::new();
        for (key, value) in resolved {
            if let Some(slot) = ThemeColor::from_key(&key) {
                fg_values.insert(slot, value);
            } else if let Some(slot) = ThemeBg::from_key(&key) {
                bg_values.insert(slot, value);
            }
            // Unknown extra keys are ignored, matching the (non-strict) schema.
        }
        Self::new(fg_values, bg_values, mode, Some(json.name.clone()), None)
    }

    /// The theme name, when it came from a document.
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// The file the theme was loaded from, when it was not built in.
    pub fn source_path(&self) -> Option<&Path> {
        self.source_path.as_deref()
    }

    /// The colour mode this palette renders for.
    pub fn color_mode(&self) -> ColorMode {
        self.mode
    }

    /// The raw foreground ANSI prefix for a slot.
    pub fn get_fg_ansi(&self, color: ThemeColor) -> &str {
        self.fg
            .get(&color)
            .map(String::as_str)
            .unwrap_or("\x1b[39m")
    }

    /// The raw background ANSI prefix for a slot.
    pub fn get_bg_ansi(&self, color: ThemeBg) -> &str {
        self.bg
            .get(&color)
            .map(String::as_str)
            .unwrap_or("\x1b[49m")
    }

    /// Wrap `text` in a foreground colour (resetting only the foreground).
    pub fn fg(&self, color: ThemeColor, text: &str) -> String {
        format!("{}{}\x1b[39m", self.get_fg_ansi(color), text)
    }

    /// Wrap `text` in a background colour (resetting only the background).
    pub fn bg(&self, color: ThemeBg, text: &str) -> String {
        format!("{}{}\x1b[49m", self.get_bg_ansi(color), text)
    }

    /// Bold `text` (chalk `bold`).
    pub fn bold(&self, text: &str) -> String {
        format!("\x1b[1m{text}\x1b[22m")
    }

    /// Italicise `text` (chalk `italic`).
    pub fn italic(&self, text: &str) -> String {
        format!("\x1b[3m{text}\x1b[23m")
    }

    /// Underline `text` (chalk `underline`).
    pub fn underline(&self, text: &str) -> String {
        format!("\x1b[4m{text}\x1b[24m")
    }

    /// Invert `text` (chalk `inverse`).
    pub fn inverse(&self, text: &str) -> String {
        format!("\x1b[7m{text}\x1b[27m")
    }

    /// Strike through `text` (chalk `strikethrough`).
    pub fn strikethrough(&self, text: &str) -> String {
        format!("\x1b[9m{text}\x1b[29m")
    }

    /// The border colour for a thinking level (upstream
    /// `getThinkingBorderColor`).
    pub fn thinking_border(&self, level: ThinkingLevel, text: &str) -> String {
        let slot = match level {
            ThinkingLevel::Off => ThemeColor::ThinkingOff,
            ThinkingLevel::Minimal => ThemeColor::ThinkingMinimal,
            ThinkingLevel::Low => ThemeColor::ThinkingLow,
            ThinkingLevel::Medium => ThemeColor::ThinkingMedium,
            ThinkingLevel::High => ThemeColor::ThinkingHigh,
            ThinkingLevel::Xhigh => ThemeColor::ThinkingXhigh,
            ThinkingLevel::Max => ThemeColor::ThinkingMax,
        };
        self.fg(slot, text)
    }

    /// The bash-mode border colour (upstream `getBashModeBorderColor`).
    pub fn bash_mode_border(&self, text: &str) -> String {
        self.fg(ThemeColor::BashMode, text)
    }

    /// True for the built-in `light` theme (upstream `isLightTheme`).
    pub fn is_light(&self) -> bool {
        is_light_theme(self.name.as_deref())
    }
}

/// Whether a theme name denotes a light theme (upstream `isLightTheme`).
pub fn is_light_theme(name: Option<&str>) -> bool {
    name == Some("light")
}

// ============================================================================
// Loading
// ============================================================================

/// The built-in theme documents, in upstream `getBuiltinThemes` order.
pub fn builtin_theme_json(name: &str) -> Option<ThemeJson> {
    let (label, content) = match name {
        "dark" => ("dark", BUILTIN_DARK_JSON),
        "light" => ("light", BUILTIN_LIGHT_JSON),
        _ => return None,
    };
    // The embedded documents are covered by tests; a failure here means the
    // assets were edited into an invalid state.
    ThemeJson::parse(label, content).ok()
}

/// The built-in theme names.
pub fn builtin_theme_names() -> Vec<String> {
    vec!["dark".to_string(), "light".to_string()]
}

/// Build a built-in theme (upstream `createTheme(getBuiltinThemes()[name])`).
pub fn builtin_theme(name: &str, mode: ColorMode) -> Result<Theme, ThemeError> {
    let json =
        builtin_theme_json(name).ok_or_else(|| ThemeError::ThemeNotFound(name.to_string()))?;
    Theme::from_json(&json, mode)
}

/// The default custom themes directory: `$PI_CODING_AGENT_DIR/themes`, else
/// `~/.pi/agent/themes` (upstream `getCustomThemesDir`).
pub fn default_custom_themes_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os(ENV_AGENT_DIR) {
        return Some(PathBuf::from(dir).join("themes"));
    }
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    Some(PathBuf::from(home).join(".pi").join("agent").join("themes"))
}

/// Load a theme by name: built-ins first, then `<custom_dir>/<name>.json`
/// (upstream `loadTheme` / `loadThemeJson`).
pub fn load_theme(
    name: &str,
    mode: ColorMode,
    custom_dir: Option<&Path>,
) -> Result<Theme, ThemeError> {
    if let Some(json) = builtin_theme_json(name) {
        return Theme::from_json(&json, mode);
    }
    let dir = custom_dir.ok_or_else(|| ThemeError::ThemeNotFound(name.to_string()))?;
    let path = dir.join(format!("{name}.json"));
    if !path.is_file() {
        return Err(ThemeError::ThemeNotFound(name.to_string()));
    }
    load_theme_from_path(&path, mode)
}

/// Load a theme document from disk (upstream `loadThemeFromPath`).
pub fn load_theme_from_path(path: &Path, mode: ColorMode) -> Result<Theme, ThemeError> {
    let content = std::fs::read_to_string(path).map_err(|error| ThemeError::Io {
        path: path.display().to_string(),
        message: error.to_string(),
    })?;
    let label = path.display().to_string();
    let json = ThemeJson::parse(&label, &content)?;
    let mut theme = Theme::from_json(&json, mode)?;
    theme.source_path = Some(path.to_path_buf());
    Ok(theme)
}

/// Theme names available from the built-ins plus the custom themes directory,
/// sorted (upstream `getAvailableThemesWithPaths`). Unparseable custom files are
/// skipped, as upstream does.
pub fn available_themes(custom_dir: Option<&Path>) -> Vec<String> {
    let mut names: Vec<String> = builtin_theme_names();
    if let Some(dir) = custom_dir {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                    continue;
                }
                let Ok(content) = std::fs::read_to_string(&path) else {
                    continue;
                };
                let label = path.display().to_string();
                if let Ok(json) = ThemeJson::parse(&label, &content) {
                    names.push(json.name);
                }
            }
        }
    }
    names.sort();
    names.dedup();
    names
}

/// Load a theme by name, returning `None` instead of an error
/// (upstream `getThemeByName`).
pub fn get_theme_by_name(name: &str, mode: ColorMode, custom_dir: Option<&Path>) -> Option<Theme> {
    load_theme(name, mode, custom_dir).ok()
}

// ============================================================================
// Terminal theme resolution
// ============================================================================

/// The dark/light axis a theme setting resolves against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalTheme {
    /// Dark terminal background.
    Dark,
    /// Light terminal background.
    Light,
}

impl TerminalTheme {
    /// The theme name for this axis.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dark => "dark",
            Self::Light => "light",
        }
    }
}

/// Where a [`TerminalThemeDetection`] came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalThemeSource {
    /// An OSC 11 query answered with the real background colour.
    TerminalBackground,
    /// The `COLORFGBG` environment hint.
    ColorFgbg,
    /// Neither was available; dark is assumed.
    Fallback,
}

impl TerminalThemeSource {
    /// The upstream string form.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TerminalBackground => "terminal background",
            Self::ColorFgbg => "COLORFGBG",
            Self::Fallback => "fallback",
        }
    }
}

/// How much the detection should be trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalThemeConfidence {
    /// The background colour was read directly.
    High,
    /// A heuristic was used.
    Low,
}

impl TerminalThemeConfidence {
    /// The upstream string form.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::High => "high",
            Self::Low => "low",
        }
    }
}

/// The result of a terminal background probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalThemeDetection {
    /// Detected axis.
    pub theme: TerminalTheme,
    /// How it was detected.
    pub source: TerminalThemeSource,
    /// Human-readable detail, matching the upstream strings.
    pub detail: String,
    /// Confidence in the result.
    pub confidence: TerminalThemeConfidence,
}

/// Parse a `light/dark` auto-theme setting (upstream `parseAutoThemeSetting`).
pub fn parse_auto_theme_setting(setting: Option<&str>) -> Option<(String, String)> {
    let setting = setting?;
    let slash = setting.find('/')?;
    if setting[slash + 1..].contains('/') {
        return None;
    }
    let light = setting[..slash].trim();
    let dark = setting[slash + 1..].trim();
    if light.is_empty() || dark.is_empty() {
        return None;
    }
    Some((light.to_string(), dark.to_string()))
}

/// Resolve a theme setting against the detected terminal axis
/// (upstream `resolveThemeSetting`).
pub fn resolve_theme_setting(setting: Option<&str>, terminal: TerminalTheme) -> Option<String> {
    if let Some((light, dark)) = parse_auto_theme_setting(setting) {
        return Some(if terminal == TerminalTheme::Light {
            light
        } else {
            dark
        });
    }
    match setting {
        Some(setting) if setting.contains('/') => None,
        Some(setting) => Some(setting.to_string()),
        None => None,
    }
}

/// The background palette index encoded in `COLORFGBG`, scanning from the end
/// (upstream `getColorFgBgBackgroundIndex`).
pub fn color_fgbg_background_index(colorfgbg: &str) -> Option<u8> {
    for part in colorfgbg.split(';').rev() {
        if let Some(value) = parse_leading_i64(part) {
            if (0..=255).contains(&value) {
                return Some(value as u8);
            }
        }
    }
    None
}

/// Parse a leading base-10 integer, as JS `parseInt` does (`"15abc"` → 15).
fn parse_leading_i64(text: &str) -> Option<i64> {
    let trimmed = text.trim_start();
    let mut chars = trimmed.char_indices().peekable();
    let mut end = 0;
    if let Some((_, sign)) = chars.peek() {
        if *sign == '+' || *sign == '-' {
            chars.next();
            end = 1;
        }
    }
    let mut saw_digit = false;
    for (index, ch) in chars {
        if ch.is_ascii_digit() {
            saw_digit = true;
            end = index + 1;
        } else {
            break;
        }
    }
    if !saw_digit {
        return None;
    }
    trimmed.get(..end)?.parse::<i64>().ok()
}

/// Relative luminance of an sRGB colour (upstream `getRgbColorLuminance`).
pub fn rgb_luminance(rgb: (u8, u8, u8)) -> f64 {
    let to_linear = |channel: u8| {
        let value = channel as f64 / 255.0;
        if value <= 0.03928 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * to_linear(rgb.0) + 0.7152 * to_linear(rgb.1) + 0.0722 * to_linear(rgb.2)
}

/// Pick dark/light from a background colour (upstream `getThemeForRgbColor`).
pub fn theme_for_rgb(rgb: (u8, u8, u8)) -> TerminalTheme {
    if rgb_luminance(rgb) >= 0.5 {
        TerminalTheme::Light
    } else {
        TerminalTheme::Dark
    }
}

/// Luminance of a 256-colour index (upstream `getAnsiColorLuminance`).
pub fn ansi_color_luminance(index: u8) -> f64 {
    let hex = ansi256_to_hex(index);
    hex_to_rgb(&hex)
        .map(rgb_luminance)
        .unwrap_or_else(|_| rgb_luminance((0, 0, 0)))
}

/// Detect the terminal axis from a `COLORFGBG` value (upstream
/// `detectTerminalBackgroundFromEnv`, without the OSC 11 query).
pub fn detect_terminal_background_from_env(colorfgbg: Option<&str>) -> TerminalThemeDetection {
    if let Some(index) = colorfgbg.and_then(color_fgbg_background_index) {
        return TerminalThemeDetection {
            theme: if ansi_color_luminance(index) >= 0.5 {
                TerminalTheme::Light
            } else {
                TerminalTheme::Dark
            },
            source: TerminalThemeSource::ColorFgbg,
            detail: format!("background color index {index}"),
            confidence: TerminalThemeConfidence::High,
        };
    }
    TerminalThemeDetection {
        theme: TerminalTheme::Dark,
        source: TerminalThemeSource::Fallback,
        detail: "no terminal background hint found".to_string(),
        confidence: TerminalThemeConfidence::Low,
    }
}

/// The theme name used when none is configured (upstream `getDefaultTheme`).
pub fn default_theme_name(colorfgbg: Option<&str>) -> String {
    detect_terminal_background_from_env(colorfgbg)
        .theme
        .as_str()
        .to_string()
}

// ============================================================================
// Controller
// ============================================================================

/// Holds the active theme and reproduces upstream `initTheme` / `setTheme`
/// fallback semantics (an invalid theme never leaves the caller without a
/// palette).
#[derive(Debug, Clone)]
pub struct ThemeController {
    current_name: Option<String>,
    theme: Theme,
    mode: ColorMode,
    custom_dir: Option<PathBuf>,
}

impl ThemeController {
    /// Create a controller, loading `theme_name` or the terminal-derived
    /// default. Invalid or missing themes fall back to `dark`.
    pub fn init(
        theme_name: Option<&str>,
        mode: ColorMode,
        custom_dir: Option<PathBuf>,
    ) -> ThemeController {
        let colorfgbg = std::env::var(ENV_COLORFGBG).ok();
        let resolved = theme_name
            .map(str::to_string)
            .unwrap_or_else(|| default_theme_name(colorfgbg.as_deref()));
        let mut controller = ThemeController {
            current_name: Some(resolved.clone()),
            theme: Theme::from_json(
                &builtin_theme_json("dark").expect("built-in dark theme is valid"),
                mode,
            )
            .expect("built-in dark theme is valid"),
            mode,
            custom_dir,
        };
        let _ = controller.set_theme(&resolved);
        controller
    }

    /// Switch themes, mirroring upstream `setTheme`: on failure the controller
    /// drops to `dark` and returns the original error.
    pub fn set_theme(&mut self, name: &str) -> Result<(), ThemeError> {
        match load_theme(name, self.mode, self.custom_dir.as_deref()) {
            Ok(theme) => {
                self.theme = theme;
                self.current_name = Some(name.to_string());
                Ok(())
            }
            Err(error) => {
                self.theme = Theme::from_json(
                    &builtin_theme_json("dark").expect("built-in dark theme is valid"),
                    self.mode,
                )
                .expect("built-in dark theme is valid");
                self.current_name = Some("dark".to_string());
                Err(error)
            }
        }
    }

    /// Install an already-built theme (upstream `setThemeInstance`).
    pub fn set_theme_instance(&mut self, theme: Theme) {
        self.theme = theme;
        self.current_name = None;
    }

    /// The active theme.
    pub fn theme(&self) -> &Theme {
        &self.theme
    }

    /// The active theme name (`None` for in-memory instances).
    pub fn current_name(&self) -> Option<&str> {
        self.current_name.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn light_fixture() -> Theme {
        builtin_theme("light", ColorMode::TrueColor).expect("light theme loads")
    }

    #[test]
    fn builtin_themes_parse_and_validate() {
        for name in builtin_theme_names() {
            let json = builtin_theme_json(&name).expect("built-in json");
            assert_eq!(json.name, name);
            json.validate(&name).expect("built-in theme validates");
        }
    }

    #[test]
    fn optional_slots_fall_back_to_their_source_token() {
        // The built-ins define every optional token, so remove them to exercise
        // the upstream `withThemeColorFallbacks` path.
        let mut json = builtin_theme_json("dark").unwrap();
        for key in [
            "scrollbarTrack",
            "scrollbarThumb",
            "searchMatchText",
            "thinkingMax",
            "searchMatchBg",
        ] {
            assert!(json.colors.remove(key).is_some(), "{key} was present");
        }
        json.validate("dark-without-optional").unwrap();
        let theme = Theme::from_json(&json, ColorMode::TrueColor).unwrap();
        assert_eq!(
            theme.get_fg_ansi(ThemeColor::ScrollbarTrack),
            theme.get_fg_ansi(ThemeColor::Muted)
        );
        assert_eq!(
            theme.get_fg_ansi(ThemeColor::ScrollbarThumb),
            theme.get_fg_ansi(ThemeColor::Text)
        );
        assert_eq!(
            theme.get_fg_ansi(ThemeColor::ThinkingMax),
            theme.get_fg_ansi(ThemeColor::ThinkingXhigh)
        );
        assert_eq!(
            theme.get_fg_ansi(ThemeColor::SearchMatchText),
            theme.get_fg_ansi(ThemeColor::Text)
        );
        assert_eq!(
            theme.get_bg_ansi(ThemeBg::SearchMatchBg),
            theme.get_bg_ansi(ThemeBg::SelectedBg)
        );

        // A token that is present is never overwritten by its fallback.
        let builtin = builtin_theme("dark", ColorMode::TrueColor).unwrap();
        assert_ne!(
            builtin.get_fg_ansi(ThemeColor::ThinkingMax),
            builtin.get_fg_ansi(ThemeColor::ThinkingXhigh)
        );
    }

    #[test]
    fn hex_quantises_to_the_same_256_index_as_upstream() {
        // Values cross-checked against the upstream TypeScript implementation.
        assert_eq!(hex_to_256("#000000").unwrap(), 16);
        assert_eq!(hex_to_256("#ffffff").unwrap(), 231);
        assert_eq!(hex_to_256("#ff0000").unwrap(), 196);
        assert_eq!(hex_to_256("#808080").unwrap(), 244);
        // 212 is nearer the 215 cube step than the 208 gray step, so the cube
        // wins even though the colour is neutral.
        assert_eq!(hex_to_256("#d4d4d4").unwrap(), 188);
        // Real theme tokens: dark `accent`, light `accent`, dark `darkGray`.
        assert_eq!(hex_to_256("#8abeb7").unwrap(), 109);
        assert_eq!(hex_to_256("#5a8080").unwrap(), 66);
        assert_eq!(hex_to_256("#505050").unwrap(), 239);
    }

    #[test]
    fn var_references_resolve_through_chains() {
        let vars: BTreeMap<String, ColorValue> = BTreeMap::from([
            ("a".to_string(), ColorValue::Var("b".to_string())),
            ("b".to_string(), ColorValue::Hex("#123456".to_string())),
        ]);
        assert_eq!(
            resolve_var_refs(&ColorValue::Var("a".to_string()), &vars).unwrap(),
            ColorValue::Hex("#123456".to_string())
        );
    }

    #[test]
    fn unknown_and_circular_var_references_are_reported() {
        let missing = BTreeMap::new();
        assert_eq!(
            resolve_var_refs(&ColorValue::Var("nope".to_string()), &missing),
            Err(ThemeError::VariableNotFound("nope".to_string()))
        );
        let circular = BTreeMap::from([
            ("a".to_string(), ColorValue::Var("b".to_string())),
            ("b".to_string(), ColorValue::Var("a".to_string())),
        ]);
        assert!(matches!(
            resolve_var_refs(&ColorValue::Var("a".to_string()), &circular),
            Err(ThemeError::CircularVariableReference(_))
        ));
    }

    #[test]
    fn validation_reports_every_missing_token() {
        let document = ThemeJson {
            schema: None,
            name: "partial".to_string(),
            vars: BTreeMap::new(),
            colors: BTreeMap::from([("text".to_string(), ColorValue::Hex("#000000".to_string()))]),
            export: None,
        };
        let error = document.validate("partial").unwrap_err();
        match error {
            ThemeError::MissingRequiredColors { missing, .. } => {
                assert_eq!(missing.len(), 50);
                assert!(!missing.contains(&"text".to_string()));
                assert!(missing.contains(&"accent".to_string()));
                assert!(missing.contains(&"selectedBg".to_string()));
                // Optional tokens are never reported.
                assert!(!missing.contains(&"scrollbarTrack".to_string()));
                assert!(!missing.contains(&"searchMatchBg".to_string()));
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn theme_names_may_not_contain_a_slash() {
        let mut json = builtin_theme_json("dark").unwrap();
        json.name = "light/dark".to_string();
        assert_eq!(
            json.validate("light/dark"),
            Err(ThemeError::InvalidThemeName {
                name: "light/dark".to_string()
            })
        );
    }

    #[test]
    fn fg_and_bg_reset_only_their_own_attribute() {
        let theme = light_fixture();
        let accent = theme.get_fg_ansi(ThemeColor::Accent);
        assert_eq!(
            theme.fg(ThemeColor::Accent, "hi"),
            format!("{accent}hi\x1b[39m")
        );
        let selected = theme.get_bg_ansi(ThemeBg::SelectedBg);
        assert_eq!(
            theme.bg(ThemeBg::SelectedBg, "hi"),
            format!("{selected}hi\x1b[49m")
        );
    }

    #[test]
    fn ansi256_mode_quantises_every_hex_token() {
        let truecolor = builtin_theme("dark", ColorMode::TrueColor).unwrap();
        let quantised = builtin_theme("dark", ColorMode::Ansi256).unwrap();
        assert!(truecolor
            .get_fg_ansi(ThemeColor::Accent)
            .starts_with("\x1b[38;2;"));
        assert!(quantised
            .get_fg_ansi(ThemeColor::Accent)
            .starts_with("\x1b[38;5;"));
        assert_eq!(
            quantised.get_fg_ansi(ThemeColor::Accent),
            format!("\x1b[38;5;{}m", hex_to_256("#8abeb7").unwrap())
        );
    }

    #[test]
    fn thinking_and_bash_borders_use_their_slots() {
        let theme = light_fixture();
        assert_eq!(
            theme.thinking_border(ThinkingLevel::Medium, "x"),
            theme.fg(ThemeColor::ThinkingMedium, "x")
        );
        assert_eq!(
            theme.thinking_border(ThinkingLevel::Max, "x"),
            theme.fg(ThemeColor::ThinkingMax, "x")
        );
        assert_eq!(
            theme.bash_mode_border("x"),
            theme.fg(ThemeColor::BashMode, "x")
        );
    }

    #[test]
    fn auto_theme_setting_requires_exactly_one_slash() {
        assert_eq!(
            parse_auto_theme_setting(Some("light/dark")),
            Some(("light".to_string(), "dark".to_string()))
        );
        assert_eq!(
            parse_auto_theme_setting(Some("my-light / my-dark")),
            Some(("my-light".to_string(), "my-dark".to_string()))
        );
        assert_eq!(parse_auto_theme_setting(Some("dark")), None);
        assert_eq!(parse_auto_theme_setting(Some("a/b/c")), None);
        assert_eq!(parse_auto_theme_setting(Some("/dark")), None);
        assert_eq!(parse_auto_theme_setting(None), None);

        assert_eq!(
            resolve_theme_setting(Some("light/dark"), TerminalTheme::Light),
            Some("light".to_string())
        );
        assert_eq!(
            resolve_theme_setting(Some("light/dark"), TerminalTheme::Dark),
            Some("dark".to_string())
        );
        assert_eq!(
            resolve_theme_setting(Some("solarized"), TerminalTheme::Dark),
            Some("solarized".to_string())
        );
        assert_eq!(
            resolve_theme_setting(Some("a/b/c"), TerminalTheme::Dark),
            None
        );
    }

    #[test]
    fn colorfgbg_detection_matches_upstream_heuristics() {
        // COLORFGBG is `fg;bg`, and upstream scans from the end.
        let light = detect_terminal_background_from_env(Some("0;15"));
        assert_eq!(light.theme, TerminalTheme::Light);
        assert_eq!(light.source, TerminalThemeSource::ColorFgbg);
        assert_eq!(light.detail, "background color index 15");
        assert_eq!(light.confidence, TerminalThemeConfidence::High);

        let dark = detect_terminal_background_from_env(Some("15;0"));
        assert_eq!(dark.theme, TerminalTheme::Dark);
        assert_eq!(dark.detail, "background color index 0");

        let fallback = detect_terminal_background_from_env(None);
        assert_eq!(fallback.theme, TerminalTheme::Dark);
        assert_eq!(fallback.source, TerminalThemeSource::Fallback);
        assert_eq!(fallback.confidence, TerminalThemeConfidence::Low);

        assert_eq!(color_fgbg_background_index("garbage"), None);
        assert_eq!(color_fgbg_background_index(""), None);
        assert_eq!(color_fgbg_background_index("300;7"), Some(7));
        // `parseInt` keeps the leading digits, and the scan skips bad parts.
        assert_eq!(color_fgbg_background_index("oops;12abc"), Some(12));
        assert_eq!(ansi256_to_hex(0), "#000000");
        assert_eq!(ansi256_to_hex(16), "#000000");
        assert_eq!(ansi256_to_hex(231), "#ffffff");
        assert_eq!(ansi256_to_hex(232), "#080808");
        assert_eq!(ansi256_to_hex(255), "#eeeeee");
        assert_eq!(theme_for_rgb((255, 255, 255)), TerminalTheme::Light);
        assert_eq!(theme_for_rgb((0, 0, 0)), TerminalTheme::Dark);
    }

    #[test]
    fn controller_falls_back_to_dark_on_invalid_theme() {
        let mut controller = ThemeController::init(Some("dark"), ColorMode::TrueColor, None);
        assert_eq!(controller.current_name(), Some("dark"));
        let error = controller.set_theme("does-not-exist").unwrap_err();
        assert_eq!(
            error,
            ThemeError::ThemeNotFound("does-not-exist".to_string())
        );
        assert_eq!(controller.current_name(), Some("dark"));
    }

    #[test]
    fn css_colors_resolve_numbers_and_defaults() {
        let css = builtin_theme_json("light").unwrap().css_colors().unwrap();
        assert_eq!(css.get("accent").map(String::as_str), Some("#5a8080"));
        assert_eq!(css.get("selectedBg").map(String::as_str), Some("#d0d0e0"));
    }
}
