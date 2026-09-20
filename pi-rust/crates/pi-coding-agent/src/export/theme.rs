//! Theme → CSS bridge for the HTML session export.
//!
//! Rust port of the export-facing half of
//! `packages/coding-agent/src/modes/interactive/theme/theme.ts`:
//! [`getResolvedThemeColors`] (every theme token as a CSS-compatible hex
//! string) and [`getThemeExportColors`] (the optional `export` section of a
//! theme document), plus the `deriveExportColors` / `adjustBrightness`
//! fallback chain `export-html/index.ts` uses when a theme does not declare
//! explicit page colours.
//!
//! The palette itself is not re-parsed here: [`pi_tui::theme::ThemeJson`]
//! already models `vars` / `colors` / `export` exactly like the TS loader,
//! so this module only adds the CSS formatting and the derived-colour maths.

use std::collections::BTreeMap;

use pi_tui::theme::{
    ansi256_to_hex, builtin_theme_json, default_custom_themes_dir, resolve_var_refs, ColorValue,
    ThemeJson,
};

/// Theme used when the caller passes no name — upstream falls back to the
/// default theme (`getDefaultTheme()`), which is `dark`.
pub const DEFAULT_THEME_NAME: &str = "dark";

/// Background colour used when a theme has no `userMessageBg` token
/// (upstream's literal fallback in `generateThemeVars`).
const FALLBACK_USER_MESSAGE_BG: &str = "#343541";

/// Resolved page / card / info backgrounds used by the HTML template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportColors {
    /// `--body-bg` / `--exportPageBg`.
    pub page_bg: String,
    /// `--container-bg` / `--exportCardBg`.
    pub card_bg: String,
    /// `--info-bg` / `--exportInfoBg`.
    pub info_bg: String,
}

/// Load a theme document by name: built-ins first, then
/// `<custom-themes-dir>/<name>.json` (upstream `loadThemeJson`).
///
/// Returns `None` when the name cannot be resolved or the document does not
/// parse, which mirrors upstream's `try { … } catch { return {}; }` in
/// `getThemeExportColors`.
pub fn load_theme_json(theme_name: Option<&str>) -> Option<ThemeJson> {
    let name = theme_name.unwrap_or(DEFAULT_THEME_NAME);
    if let Some(json) = builtin_theme_json(name) {
        return Some(json);
    }
    let dir = default_custom_themes_dir()?;
    let path = dir.join(format!("{name}.json"));
    let content = std::fs::read_to_string(&path).ok()?;
    ThemeJson::parse(&path.display().to_string(), &content).ok()
}

/// Every resolved theme token as a CSS-compatible colour string, keyed by
/// token name (upstream `getResolvedThemeColors`).
///
/// Falls back to an empty map for an unknown theme so the export still
/// renders with the template's own defaults.
pub fn resolved_theme_colors(theme_name: Option<&str>) -> BTreeMap<String, String> {
    load_theme_json(theme_name)
        .and_then(|json| json.css_colors().ok())
        .unwrap_or_default()
}

/// The explicit `export` section of a theme document, if any
/// (upstream `getThemeExportColors`).
///
/// `ColorValue::Reset` (the empty string upstream) resolves to `None`, as
/// does an unknown theme or an unresolvable variable reference.
pub fn theme_export_colors(
    theme_name: Option<&str>,
) -> (Option<String>, Option<String>, Option<String>) {
    let Some(json) = load_theme_json(theme_name) else {
        return (None, None, None);
    };
    let Some(export) = json.export.as_ref() else {
        return (None, None, None);
    };
    let resolve = |value: &Option<ColorValue>| -> Option<String> {
        let value = value.as_ref()?;
        match resolve_var_refs(value, &json.vars).ok()? {
            ColorValue::Index(index) => Some(ansi256_to_hex(index)),
            ColorValue::Hex(hex) => Some(hex),
            // `""` means "terminal default"; upstream returns undefined here.
            ColorValue::Reset => None,
            // `resolve_var_refs` replaces every reference; reaching this arm
            // means the document referenced a missing variable.
            ColorValue::Var(_) => None,
        }
    };
    (
        resolve(&export.page_bg),
        resolve(&export.card_bg),
        resolve(&export.info_bg),
    )
}

/// Build the `{{THEME_VARS}}` block: one `--token: value;` line per resolved
/// theme colour followed by the three `--export*` custom properties
/// (upstream `generateThemeVars`).
pub fn generate_theme_vars(theme_name: Option<&str>) -> String {
    let colors = resolved_theme_colors(theme_name);
    let mut lines: Vec<String> = colors
        .iter()
        .map(|(key, value)| format!("--{key}: {value};"))
        .collect();

    let (page_bg, card_bg, info_bg) = theme_export_colors(theme_name);
    let derived = derive_export_colors(user_message_bg(&colors));
    lines.push(format!(
        "--exportPageBg: {};",
        page_bg.unwrap_or(derived.page_bg)
    ));
    lines.push(format!(
        "--exportCardBg: {};",
        card_bg.unwrap_or(derived.card_bg)
    ));
    lines.push(format!(
        "--exportInfoBg: {};",
        info_bg.unwrap_or(derived.info_bg)
    ));

    lines.join("\n      ")
}

/// Resolve the page / card / info backgrounds for the template's own CSS
/// variables (`{{BODY_BG}}` / `{{CONTAINER_BG}}` / `{{INFO_BG}}`).
pub fn export_colors(theme_name: Option<&str>) -> ExportColors {
    let colors = resolved_theme_colors(theme_name);
    let (page_bg, card_bg, info_bg) = theme_export_colors(theme_name);
    let derived = derive_export_colors(user_message_bg(&colors));
    ExportColors {
        page_bg: page_bg.unwrap_or(derived.page_bg),
        card_bg: card_bg.unwrap_or(derived.card_bg),
        info_bg: info_bg.unwrap_or(derived.info_bg),
    }
}

fn user_message_bg(colors: &BTreeMap<String, String>) -> &str {
    colors
        .get("userMessageBg")
        .map(String::as_str)
        .unwrap_or(FALLBACK_USER_MESSAGE_BG)
}

/// RGB triplet parsed from a CSS colour string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Rgb {
    r: u8,
    g: u8,
    b: u8,
}

/// Parse `#RRGGBB` and `rgb(r, g, b)` colour strings; anything else is
/// unsupported and yields `None` (upstream `parseColor`).
fn parse_color(color: &str) -> Option<Rgb> {
    let color = color.trim();
    if let Some(hex) = color.strip_prefix('#') {
        if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        return Some(Rgb {
            r: u8::from_str_radix(&hex[0..2], 16).ok()?,
            g: u8::from_str_radix(&hex[2..4], 16).ok()?,
            b: u8::from_str_radix(&hex[4..6], 16).ok()?,
        });
    }
    let inner = color
        .strip_prefix("rgb")?
        .trim_start()
        .strip_prefix('(')?
        .strip_suffix(')')?;
    let mut parts = inner.split(',').map(str::trim);
    let r = parts.next()?.parse::<u8>().ok()?;
    let g = parts.next()?.parse::<u8>().ok()?;
    let b = parts.next()?.parse::<u8>().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some(Rgb { r, g, b })
}

/// Relative luminance of a colour, 0 (black) … 1 (white)
/// (upstream `getLuminance`).
fn get_luminance(rgb: Rgb) -> f64 {
    fn to_linear(channel: u8) -> f64 {
        let s = f64::from(channel) / 255.0;
        if s <= 0.03928 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    }
    0.2126 * to_linear(rgb.r) + 0.7152 * to_linear(rgb.g) + 0.0722 * to_linear(rgb.b)
}

/// Scale each channel by `factor`, clamping to `0..=255` and returning the
/// upstream `rgb(r, g, b)` spelling (upstream `adjustBrightness`).
fn adjust_brightness(color: &str, factor: f64) -> String {
    let Some(rgb) = parse_color(color) else {
        return color.to_string();
    };
    let adjust =
        |channel: u8| -> u8 { (f64::from(channel) * factor).round().clamp(0.0, 255.0) as u8 };
    format!(
        "rgb({}, {}, {})",
        adjust(rgb.r),
        adjust(rgb.g),
        adjust(rgb.b)
    )
}

/// Derive page/card/info backgrounds from the user-message background
/// (upstream `deriveExportColors`).
fn derive_export_colors(base_color: &str) -> ExportColors {
    let Some(rgb) = parse_color(base_color) else {
        return ExportColors {
            page_bg: "rgb(24, 24, 30)".to_string(),
            card_bg: "rgb(30, 30, 36)".to_string(),
            info_bg: "rgb(60, 55, 40)".to_string(),
        };
    };

    if get_luminance(rgb) > 0.5 {
        return ExportColors {
            page_bg: adjust_brightness(base_color, 0.96),
            card_bg: base_color.to_string(),
            info_bg: format!(
                "rgb({}, {}, {})",
                rgb.r.saturating_add(10),
                rgb.g.saturating_add(5),
                rgb.b.saturating_sub(20)
            ),
        };
    }
    ExportColors {
        page_bg: adjust_brightness(base_color, 0.7),
        card_bg: adjust_brightness(base_color, 0.85),
        info_bg: format!(
            "rgb({}, {}, {})",
            rgb.r.saturating_add(20),
            rgb.g.saturating_add(15),
            rgb.b
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hex_and_rgb_colors() {
        assert_eq!(
            parse_color("#102030"),
            Some(Rgb {
                r: 0x10,
                g: 0x20,
                b: 0x30
            })
        );
        assert_eq!(parse_color("rgb(1, 2, 3)"), Some(Rgb { r: 1, g: 2, b: 3 }));
        assert_eq!(parse_color("hsl(1, 2%, 3%)"), None);
        assert_eq!(parse_color("#12345"), None);
    }

    #[test]
    fn derives_dark_and_light_palettes() {
        let dark = derive_export_colors("#343541");
        assert_eq!(dark.card_bg, "rgb(44, 45, 55)");
        assert!(dark.page_bg.starts_with("rgb("));

        let light = derive_export_colors("#ffffff");
        assert_eq!(light.card_bg, "#ffffff");
        assert_eq!(light.page_bg, "rgb(245, 245, 245)");
    }

    #[test]
    fn dark_theme_vars_include_export_properties() {
        let vars = generate_theme_vars(Some("dark"));
        assert!(vars.contains("--exportPageBg:"), "{vars}");
        assert!(vars.contains("--exportCardBg:"), "{vars}");
        assert!(vars.contains("--exportInfoBg:"), "{vars}");
        assert!(vars.contains("--userMessageBg:"), "{vars}");
    }

    #[test]
    fn unknown_theme_degrades_to_derived_colors() {
        let colors = export_colors(Some("does-not-exist"));
        assert!(colors.page_bg.starts_with("rgb("), "{:?}", colors);
    }
}
