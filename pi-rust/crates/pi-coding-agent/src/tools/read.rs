//! `ReadTool` — read file contents with offset/limit and truncation.
//!
//! Mirrors `createReadTool` from `packages/coding-agent/src/core/tools/read.ts`.
//! The text half matches upstream byte for byte, including the three
//! continuation notices:
//!
//! * `[Showing lines a-b of N. Use offset=a+1 to continue.]` when the head
//!   truncation stopped on the line limit,
//! * `[Showing lines a-b of N (50.0KB limit). Use offset=…]` when it stopped
//!   on the byte limit,
//! * `[N more lines in file. Use offset=…]` when the caller's own `limit`
//!   stopped early even though truncation had not fired,
//! * `[Line N is xB, exceeds 50.0KB limit. Use bash: sed -n 'Np' <path> | head -c 51200]`
//!   when a single over-long first line cannot be shown at all.
//!
//! The image half ports upstream's `processImage` path as far as a native
//! image backend is needed: a file whose magic bytes identify PNG / JPEG / GIF /
//! WebP is returned as a text note **plus** a [`Content::Image`] block, in the
//! upstream `[text note, image]` order. Upstream additionally resizes the image
//! to the inline provider limits and converts every other format to PNG
//! (`utils/image-resize.ts`, `utils/image-convert.ts`); this port has no image
//! encoder, so an oversized image is passed through unchanged and a format the
//! sniffer recognizes but the inline path does not accept (BMP) is reported as
//! omitted instead of converted. The non-vision-model note
//! (`getNonVisionImageNote`) needs the active model, which the tool does not
//! see, so it is not emitted here.

#![cfg(not(target_arch = "wasm32"))]

use async_trait::async_trait;
use pi_protocol::{Content, ImageContent, TextContent};
use serde::Deserialize;
use serde_json::json;

use super::truncate::{
    format_size, truncate_head, TruncatedBy, TruncationOptions, TruncationResult, DEFAULT_MAX_BYTES,
};
use super::{AbortLike, AgentTool, ToolError, ToolOutput};

/// `read` tool — read the contents of a file.
#[derive(Debug, Default)]
pub struct ReadTool;

#[derive(Debug, Deserialize)]
struct ReadArgs {
    path: String,
    /// 1-indexed first line to read (upstream `readSchema.offset`).
    #[serde(default)]
    offset: Option<i64>,
    /// Maximum number of lines to read (upstream `readSchema.limit`).
    #[serde(default)]
    limit: Option<i64>,
}

/// Structured `details` payload for `read`, mirroring upstream
/// `ReadToolDetails`. Only present when the content was actually truncated.
#[derive(Debug, Clone, serde::Serialize)]
struct ReadToolDetails {
    truncation: TruncationResult,
}

#[async_trait]
impl AgentTool for ReadTool {
    fn name(&self) -> &str {
        "read"
    }

    fn label(&self) -> &str {
        "read"
    }

    fn description(&self) -> &str {
        "Read the contents of a file as UTF-8 text. \
         Output is truncated to 2000 lines or 50KB (whichever is hit first). \
         Use offset/limit for large files. When you need the full file, \
         continue with offset until complete."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["path"],
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to read (relative or absolute)"
                },
                "offset": {
                    "type": "integer",
                    "description": "Line number to start reading from (1-indexed)"
                },
                "limit": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Maximum number of lines to read"
                }
            }
        })
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        abort: AbortLike,
    ) -> Result<ToolOutput, ToolError> {
        if abort.is_cancelled() {
            return Err(ToolError::Aborted);
        }

        let parsed: ReadArgs =
            serde_json::from_value(args).map_err(|e| ToolError::InvalidArguments(e.to_string()))?;

        let bytes = std::fs::read(&parsed.path).map_err(|e| {
            ToolError::Execution(format!("failed to read '{}': {}", parsed.path, e))
        })?;

        // An image file never reaches the text path: it is handed to the
        // caller as an image block (upstream `ops.detectImageMimeType` +
        // `processImage`).
        if let Some(mime_type) = detect_supported_image_mime_type(&bytes) {
            return Ok(image_output(mime_type, &bytes));
        }

        let content = String::from_utf8(bytes).map_err(|e| {
            ToolError::Execution(format!("failed to read '{}': {}", parsed.path, e))
        })?;

        // Upstream splits on `\n` *without* dropping the trailing empty entry:
        // a file that ends with a newline counts one extra (empty) line, which
        // is what `offset` is validated against. `truncate_head` uses the
        // other convention (trailing newline ignored) via
        // `split_lines_for_counting`.
        let all_lines: Vec<&str> = content.split('\n').collect();
        let total_file_lines = all_lines.len();

        // Convert the 1-indexed input into a 0-indexed array access, clamping
        // negatives to 0 exactly like upstream `Math.max(0, offset - 1)`.
        let start_line = parsed
            .offset
            .map(|o| o.saturating_sub(1).max(0) as usize)
            .unwrap_or(0);
        let start_line_display = start_line + 1;

        if start_line >= all_lines.len() {
            return Err(ToolError::Execution(format!(
                "Offset {} is beyond end of file ({} lines total)",
                parsed.offset.unwrap_or(0),
                total_file_lines
            )));
        }

        let mut user_limited_lines: Option<usize> = None;
        let selected_content = match parsed.limit {
            Some(limit) => {
                let limit = limit.max(0) as usize;
                let end_line = (start_line + limit).min(all_lines.len());
                user_limited_lines = Some(end_line - start_line);
                all_lines[start_line..end_line].join("\n")
            }
            None => all_lines[start_line..].join("\n"),
        };

        let truncation = truncate_head(&selected_content, TruncationOptions::default());

        let output_text = if truncation.first_line_exceeds_limit {
            // The first line alone blows the byte budget; an empty body with a
            // hint is more useful than silence.
            let first_line_size = format_size(all_lines[start_line].len());
            format!(
                "[Line {} is {}, exceeds {} limit. Use bash: sed -n '{}p' {} | head -c {}]",
                start_line_display,
                first_line_size,
                format_size(DEFAULT_MAX_BYTES),
                start_line_display,
                parsed.path,
                DEFAULT_MAX_BYTES
            )
        } else if truncation.truncated {
            let end_line_display = start_line_display + truncation.output_lines - 1;
            let next_offset = end_line_display + 1;
            let mut text = truncation.content.clone();
            match truncation.truncated_by {
                Some(TruncatedBy::Lines) => text.push_str(&format!(
                    "\n\n[Showing lines {}-{} of {}. Use offset={} to continue.]",
                    start_line_display, end_line_display, total_file_lines, next_offset
                )),
                _ => text.push_str(&format!(
                    "\n\n[Showing lines {}-{} of {} ({} limit). Use offset={} to continue.]",
                    start_line_display,
                    end_line_display,
                    total_file_lines,
                    format_size(DEFAULT_MAX_BYTES),
                    next_offset
                )),
            }
            text
        } else if let Some(limited) =
            user_limited_lines.filter(|limited| start_line + limited < all_lines.len())
        {
            let remaining = all_lines.len() - (start_line + limited);
            let next_offset = start_line + limited + 1;
            format!(
                "{}\n\n[{} more lines in file. Use offset={} to continue.]",
                truncation.content, remaining, next_offset
            )
        } else {
            truncation.content.clone()
        };

        let mut output = ToolOutput::text(output_text);
        if truncation.truncated {
            output = output.with_details(
                serde_json::to_value(ReadToolDetails { truncation })
                    .map_err(|e| ToolError::Execution(format!("details encode: {}", e)))?,
            );
        }
        Ok(output)
    }
}

// ---------------------------------------------------------------------------
// Image path
// ---------------------------------------------------------------------------

/// MIME types the inline path forwards unchanged to the providers.
///
/// Mirrors `normalizeSupportedImageMimeType` (`utils/image-process.ts:29-44`).
const INLINE_IMAGE_MIME_TYPES: [&str; 4] = ["image/png", "image/jpeg", "image/gif", "image/webp"];

/// Sniff the first bytes of a file for a supported image format.
///
/// Byte-for-byte port of `detectSupportedImageMimeType`
/// (`packages/coding-agent/src/utils/mime.ts`), including its refusals: a JPEG
/// whose fourth byte is `0xf7` (JPEG-LS, not decodable by the inline path), an
/// APNG, and a BMP whose header does not describe a single-plane image.
fn detect_supported_image_mime_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        return (bytes.get(3) != Some(&0xf7)).then_some("image/jpeg");
    }
    if bytes.starts_with(&PNG_SIGNATURE) {
        return (is_png(bytes) && !is_animated_png(bytes)).then_some("image/png");
    }
    if bytes.starts_with(b"GIF") {
        return Some("image/gif");
    }
    if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP".as_slice()) {
        return Some("image/webp");
    }
    if bytes.starts_with(b"BM") && is_bmp(bytes) {
        return Some("image/bmp");
    }
    None
}

const PNG_SIGNATURE: [u8; 8] = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];

/// A PNG whose first chunk is an `IHDR` of the expected length.
fn is_png(bytes: &[u8]) -> bool {
    bytes.len() >= 16
        && read_u32_be(bytes, PNG_SIGNATURE.len()) == 13
        && bytes.get(12..16) == Some(b"IHDR".as_slice())
}

/// True for an APNG (an `acTL` chunk before the first `IDAT`).
fn is_animated_png(bytes: &[u8]) -> bool {
    let mut offset = PNG_SIGNATURE.len();
    while offset + 8 <= bytes.len() {
        let chunk_length = read_u32_be(bytes, offset) as usize;
        let chunk_type = bytes.get(offset + 4..offset + 8).unwrap_or_default();
        if chunk_type == b"acTL" {
            return true;
        }
        if chunk_type == b"IDAT" {
            return false;
        }
        let Some(next) = offset
            .checked_add(8)
            .and_then(|next| next.checked_add(chunk_length))
            .and_then(|next| next.checked_add(4))
        else {
            return false;
        };
        if next <= offset || next > bytes.len() {
            return false;
        }
        offset = next;
    }
    false
}

/// A BMP with a plausible header: single colour plane and a supported depth.
fn is_bmp(bytes: &[u8]) -> bool {
    if bytes.len() < 26 {
        return false;
    }
    let declared_file_size = read_u32_le(bytes, 2);
    let pixel_data_offset = read_u32_le(bytes, 10);
    let dib_header_size = read_u32_le(bytes, 14);
    if declared_file_size != 0 && declared_file_size < 26 {
        return false;
    }
    if pixel_data_offset < 14 + dib_header_size {
        return false;
    }
    if declared_file_size != 0 && pixel_data_offset >= declared_file_size {
        return false;
    }

    let (color_planes, bits_per_pixel) = if dib_header_size == 12 {
        (read_u16_le(bytes, 22), read_u16_le(bytes, 24))
    } else if (40..=124).contains(&dib_header_size) {
        if bytes.len() < 30 {
            return false;
        }
        (read_u16_le(bytes, 26), read_u16_le(bytes, 28))
    } else {
        return false;
    };

    color_planes == 1 && matches!(bits_per_pixel, 1 | 4 | 8 | 16 | 24 | 32)
}

fn read_u16_le(bytes: &[u8], offset: usize) -> u32 {
    u32::from(bytes.get(offset).copied().unwrap_or(0))
        + (u32::from(bytes.get(offset + 1).copied().unwrap_or(0)) << 8)
}

fn read_u32_be(bytes: &[u8], offset: usize) -> u32 {
    u32::from(bytes.get(offset).copied().unwrap_or(0)) * 0x100_0000
        + (u32::from(bytes.get(offset + 1).copied().unwrap_or(0)) << 16)
        + (u32::from(bytes.get(offset + 2).copied().unwrap_or(0)) << 8)
        + u32::from(bytes.get(offset + 3).copied().unwrap_or(0))
}

fn read_u32_le(bytes: &[u8], offset: usize) -> u32 {
    u32::from(bytes.get(offset).copied().unwrap_or(0))
        + (u32::from(bytes.get(offset + 1).copied().unwrap_or(0)) << 8)
        + (u32::from(bytes.get(offset + 2).copied().unwrap_or(0)) << 16)
        + u32::from(bytes.get(offset + 3).copied().unwrap_or(0)) * 0x100_0000
}

/// The `read` result for a file the sniffer identified as an image.
///
/// Mirrors upstream's `content: [{ type: "text", text: `Read image file
/// [${mimeType}]` }, { type: "image", … }]`. A format outside
/// [`INLINE_IMAGE_MIME_TYPES`] cannot be converted without an image backend,
/// which is upstream's `ok: false` branch: the text note carries the omission
/// message and no image block is attached.
fn image_output(mime_type: &str, bytes: &[u8]) -> ToolOutput {
    let note = format!("Read image file [{mime_type}]");
    if !INLINE_IMAGE_MIME_TYPES.contains(&mime_type) {
        return ToolOutput::text(format!(
            "{note}\n[Image omitted: could not be converted to a supported inline image format.]"
        ));
    }
    ToolOutput {
        content: vec![
            Content::Text(TextContent { text: note }),
            Content::Image(ImageContent {
                mime_type: mime_type.to_string(),
                data: pi_tui::base64_encode(bytes),
            }),
        ],
        details: None,
    }
}
