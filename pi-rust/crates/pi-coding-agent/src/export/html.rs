//! Self-contained HTML session export.
//!
//! Rust port of `packages/coding-agent/src/core/export-html/index.ts`'
//! `generateHtml`. The upstream module reads five static template assets from
//! disk at export time; this port embeds byte-identical copies at compile
//! time via [`include_str!`] so the `pi` binary stays a single file and the
//! export works with no install tree next to the executable.
//!
//! Nothing in the template is rewritten: Rust only
//!
//! 1. assembles the [`SessionData`] payload (same field names as upstream),
//! 2. base64-encodes it into `{{SESSION_DATA}}`,
//! 3. expands the theme variables into `{{THEME_VARS}}`, and
//! 4. substitutes the vendored `marked` / `highlight.js` bundles.
//!
//! Rendering of the actual conversation happens in the browser, exactly as
//! upstream does it.

use super::theme::{export_colors, generate_theme_vars};
use super::SessionData;

/// `assets/export-html/template.html` — verbatim copy from
/// `packages/coding-agent/src/core/export-html/template.html`.
pub const TEMPLATE_HTML: &str = include_str!("../../assets/export-html/template.html");

/// `assets/export-html/template.css` — verbatim copy from upstream.
pub const TEMPLATE_CSS: &str = include_str!("../../assets/export-html/template.css");

/// `assets/export-html/template.js` — verbatim copy from upstream.
pub const TEMPLATE_JS: &str = include_str!("../../assets/export-html/template.js");

/// `assets/export-html/vendor/marked.min.js` — verbatim copy from upstream.
pub const MARKED_JS: &str = include_str!("../../assets/export-html/vendor/marked.min.js");

/// `assets/export-html/vendor/highlight.min.js` — verbatim copy from upstream.
pub const HIGHLIGHT_JS: &str = include_str!("../../assets/export-html/vendor/highlight.min.js");

/// Render a full self-contained HTML document for `session_data`
/// (upstream `generateHtml`).
///
/// `theme_name` selects the palette; `None` falls back to the default theme
/// (`dark`) exactly like `getResolvedThemeColors(undefined)` does.
pub fn generate_html(session_data: &SessionData, theme_name: Option<&str>) -> String {
    let theme_vars = generate_theme_vars(theme_name);
    let colors = export_colors(theme_name);

    // Base64-encode the payload so no character in the JSON can break out of
    // the `<script type="application/json">` element.
    let payload = serde_json::to_vec(session_data).unwrap_or_else(|_| b"{}".to_vec());
    let session_data_base64 = base64_encode(&payload);

    let css = TEMPLATE_CSS
        .replace("{{THEME_VARS}}", &theme_vars)
        .replace("{{BODY_BG}}", &colors.page_bg)
        .replace("{{CONTAINER_BG}}", &colors.card_bg)
        .replace("{{INFO_BG}}", &colors.info_bg);

    // Replacement order mirrors upstream: CSS first, then the app bundle,
    // then the payload and the vendored libraries.
    TEMPLATE_HTML
        .replace("{{CSS}}", &css)
        .replace("{{JS}}", TEMPLATE_JS)
        .replace("{{SESSION_DATA}}", &session_data_base64)
        .replace("{{MARKED_JS}}", MARKED_JS)
        .replace("{{HIGHLIGHT_JS}}", HIGHLIGHT_JS)
}

/// Standard base64 (RFC 4648, `+/` alphabet with `=` padding).
///
/// Hand-rolled on purpose: the payload is the only base64 this crate needs
/// and the workspace deliberately does not carry a `base64` dependency.
pub fn base64_encode(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = chunk.get(1).copied().map(u32::from);
        let b2 = chunk.get(2).copied().map(u32::from);
        let triple = (b0 << 16) | (b1.unwrap_or(0) << 8) | b2.unwrap_or(0);
        out.push(ALPHABET[((triple >> 18) & 0x3f) as usize] as char);
        out.push(ALPHABET[((triple >> 12) & 0x3f) as usize] as char);
        if b1.is_some() {
            out.push(ALPHABET[((triple >> 6) & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
        if b2.is_some() {
            out.push(ALPHABET[(triple & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::export::session_file;

    #[test]
    fn base64_matches_rfc4648_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        // UTF-8 payloads round-trip through the same alphabet.
        assert_eq!(base64_encode("会话".as_bytes()), "5Lya6K+d");
    }

    #[test]
    fn html_contains_doctype_payload_and_theme_vars() {
        let data = session_file::fixture_session_data();
        let html = generate_html(&data, Some("dark"));

        assert!(html.starts_with("<!DOCTYPE html"), "missing doctype");
        assert!(html.contains("id=\"session-data\""), "missing payload node");
        assert!(html.contains("--exportPageBg:"), "missing theme vars");
        assert!(html.contains("--body-bg:"), "missing body bg");
        // The payload is base64 of the serialised SessionData.
        let payload = serde_json::to_vec(&data).expect("serialise");
        assert!(
            html.contains(&base64_encode(&payload)),
            "base64 payload missing"
        );
        // Vendored libraries are inlined, not linked.
        assert!(html.contains("hljs"), "highlight.js missing");
        assert!(html.contains("marked"), "marked missing");
    }

    #[test]
    fn html_is_stable_for_the_same_fixture() {
        let data = session_file::fixture_session_data();
        assert_eq!(
            generate_html(&data, Some("dark")),
            generate_html(&data, Some("dark"))
        );
        // Different palettes must produce different documents.
        assert_ne!(
            generate_html(&data, Some("dark")),
            generate_html(&data, Some("light"))
        );
    }
}
