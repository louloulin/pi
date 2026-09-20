//! `@file` CLI argument expansion + stdin pipe handling.
//!
//! Mirrors the upstream `packages/coding-agent/src/cli/file-processor.ts`
//! surface for the parts the print mode needs:
//!
//! - Expand every `@<path>` token in the user-supplied prompt into a
//!   `<file name="…">…content…</file>` block (text) or an image
//!   attachment (image MIME type) — same shape the TS port produces.
//! - Drain `stdin` when it is piped (`!is_terminal`) and append the
//!   contents to the prompt so `echo hello | pi --print` works.
//! - Validate the file: must exist, must not be a directory, must not
//!   exceed the 1 MiB cap, and must decode as UTF-8 (for text files).
//!
//! Image attachments are surfaced as [`ImageContent`](pi_protocol::ImageContent)
//! values the agent loop can pass through; the Stage 7 pi-ai code path
//! encodes them in the provider request. For now we only inspect the
//! extension / magic bytes — actual image resizing is out of scope for
//! this stage (the issue calls it out explicitly).
//!
//! Errors are categorized with [`FileError`] so `print_mode` can map
//! them to the sysexits-style exit codes the issue specifies:
//!
//! | error                 | exit code |
//! |-----------------------|-----------|
//! | `NotFound`            | 64 (EX_USAGE) |
//! | `TooLarge`            | 64 (EX_USAGE) |
//! | `IsDirectory`         | 64 (EX_USAGE) |
//! | `NotUtf8`             | 65 (EX_DATAERR) |
//! | `Io` / other          | 74 (EX_IOERR) |

#![forbid(unsafe_code)]

use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use pi_protocol::ImageContent;
use thiserror::Error;

/// Maximum file size (in bytes) the file processor will read for text
/// expansion. Larger files are rejected with [`FileError::TooLarge`] so
/// we do not accidentally OOM the agent on a 200 MiB accidental paste.
pub const MAX_FILE_BYTES: u64 = 1024 * 1024;

/// Failure modes emitted by [`expand_prompt`] and [`read_stdin_if_piped`].
#[derive(Debug, Error)]
pub enum FileError {
    /// File path does not exist on disk.
    #[error("file not found: {path}")]
    NotFound {
        /// Path the user asked for.
        path: PathBuf,
    },
    /// File exists but is a directory.
    #[error("path is a directory: {path}")]
    IsDirectory {
        /// Path that turned out to be a directory.
        path: PathBuf,
    },
    /// File exceeds [`MAX_FILE_BYTES`].
    #[error("file too large (> {limit} bytes): {path}")]
    TooLarge {
        /// Path that exceeded the limit.
        path: PathBuf,
        /// Limit in bytes (echoed for clarity).
        limit: u64,
    },
    /// File is not valid UTF-8.
    #[error("file is not valid UTF-8: {path}")]
    NotUtf8 {
        /// Path that failed UTF-8 decoding.
        path: PathBuf,
    },
    /// Wrapped I/O error from `std::fs`.
    #[error("io error reading {path}: {source}")]
    Io {
        /// Path the operation targeted.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
}

/// sysexits-style exit code for a [`FileError`].
///
/// The mapping follows the convention spelled out in the issue:
///
/// - `EX_USAGE` (64) for malformed arguments (missing file, directory,
///   too large).
/// - `EX_DATAERR` (65) for content that is structurally fine but
///   rejected by validation (e.g. non-UTF-8 bytes).
/// - `EX_IOERR` (74) for everything else (wrapped `std::io::Error`).
impl FileError {
    /// Return the sysexits-style exit code for this error.
    pub fn exit_code(&self) -> u8 {
        match self {
            FileError::NotFound { .. }
            | FileError::IsDirectory { .. }
            | FileError::TooLarge { .. } => 64,
            FileError::NotUtf8 { .. } => 65,
            FileError::Io { .. } => 74,
        }
    }
}

/// Outcome of [`expand_prompt`].
#[derive(Debug, Default, Clone)]
pub struct ExpandedPrompt {
    /// Text the agent receives as the user prompt. Contains the
    /// original prose interleaved with `<file>` blocks for every
    /// `@file` token the user supplied.
    pub text: String,
    /// Image attachments discovered during expansion. These are
    /// surfaced separately because providers expect them as a distinct
    /// field, not folded into the text.
    pub images: Vec<ImageContent>,
}

impl ExpandedPrompt {
    /// True when the expansion produced no text and no images.
    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty() && self.images.is_empty()
    }
}

/// Expand `@file` tokens in `prompt` into `<file>` blocks + image
/// attachments, appending the piped stdin content (when present) at the
/// end. Tokens are detected with a small in-line scanner — we look for
/// an `@` character followed by a non-whitespace run that starts with a
/// path separator (`/`, `./`, `../`, `~`) or an alphanumeric segment
/// the OS recognises as a file.
pub fn expand_prompt(prompt: &str, stdin: Option<&str>) -> Result<ExpandedPrompt, FileError> {
    let mut out = ExpandedPrompt::default();
    let mut cursor = 0usize;
    let bytes = prompt.as_bytes();
    while cursor < bytes.len() {
        let ch = bytes[cursor];
        if ch == b'@' {
            // Skip `@` followed by another `@` (literal escape) — we
            // produce a single `@` and advance past the second one.
            if cursor + 1 < bytes.len() && bytes[cursor + 1] == b'@' {
                out.text.push('@');
                cursor += 2;
                continue;
            }
            // Try to read a path token starting at cursor+1.
            let token_start = cursor + 1;
            let mut token_end = token_start;
            while token_end < bytes.len() {
                let b = bytes[token_end];
                if b.is_ascii_whitespace() {
                    break;
                }
                token_end += 1;
            }
            if token_end > token_start {
                let raw = &prompt[token_start..token_end];
                // Heuristic: a path-like token must contain a `/`,
                // start with `./`, start with `../`, start with `~`,
                // or look like a file (we probe the FS to confirm).
                let looks_like_path = raw.starts_with('/')
                    || raw.starts_with("./")
                    || raw.starts_with("../")
                    || raw.starts_with('~')
                    || raw.contains('/')
                    || Path::new(raw).exists();
                if looks_like_path {
                    let path = resolve_path(raw);
                    match read_file_for_prompt(&path)? {
                        FileRead::Text(text) => {
                            out.text.push_str(&format!(
                                "<file name=\"{}\">\n{}\n</file>\n",
                                path.display(),
                                text
                            ));
                        }
                        FileRead::Image(image) => {
                            // Mirror the upstream TS surface: when an
                            // image is attached we add a placeholder
                            // `<file>` block so the model sees a
                            // `<file>` reference in the text.
                            out.text
                                .push_str(&format!("<file name=\"{}\"></file>\n", path.display()));
                            out.images.push(image);
                        }
                    }
                    cursor = token_end;
                    continue;
                }
            }
        }
        // Default: copy one byte through (UTF-8 safe: `prompt` is a
        // &str so `as_bytes()` is valid; pushing as `char` keeps any
        // multi-byte sequences intact).
        let ch_char = prompt[cursor..]
            .chars()
            .next()
            .expect("cursor sits on a char boundary");
        out.text.push(ch_char);
        cursor += ch_char.len_utf8();
    }

    if let Some(stdin_content) = stdin {
        if !stdin_content.is_empty() {
            if !out.text.is_empty() && !out.text.ends_with('\n') {
                out.text.push('\n');
            }
            out.text.push_str("<stdin>\n");
            out.text.push_str(stdin_content);
            if !stdin_content.ends_with('\n') {
                out.text.push('\n');
            }
            out.text.push_str("</stdin>\n");
        }
    }

    Ok(out)
}

/// Read `stdin` if it is piped (not a TTY). When stdin is a terminal we
/// return `Ok(None)` so callers can append nothing.
pub fn read_stdin_if_piped() -> std::io::Result<Option<String>> {
    use std::io::Read;
    if is_stdin_a_tty() {
        return Ok(None);
    }
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf)?;
    Ok(Some(buf))
}

/// One variant per kind of file the file-processor recognises.
#[derive(Debug)]
pub(crate) enum FileRead {
    /// Text file (UTF-8 decoded).
    Text(String),
    /// Image attachment (base64-encoded payload).
    Image(ImageContent),
}

/// Resolve a CLI-supplied path the way the upstream TS port does:
/// leave absolute paths alone, expand `~` to the home directory,
/// otherwise treat it as relative to the current working directory.
fn resolve_path(raw: &str) -> PathBuf {
    if let Some(stripped) = raw.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(stripped);
        }
    }
    let path = PathBuf::from(raw);
    if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    }
}

/// Read a file and classify it as text or image. Errors are mapped to
/// [`FileError`] so callers can pick the right exit code.
fn read_file_for_prompt(path: &Path) -> Result<FileRead, FileError> {
    let metadata = std::fs::metadata(path).map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            FileError::NotFound {
                path: path.to_path_buf(),
            }
        } else {
            FileError::Io {
                path: path.to_path_buf(),
                source: err,
            }
        }
    })?;
    if metadata.is_dir() {
        return Err(FileError::IsDirectory {
            path: path.to_path_buf(),
        });
    }
    if metadata.len() > MAX_FILE_BYTES {
        return Err(FileError::TooLarge {
            path: path.to_path_buf(),
            limit: MAX_FILE_BYTES,
        });
    }
    let bytes = std::fs::read(path).map_err(|err| FileError::Io {
        path: path.to_path_buf(),
        source: err,
    })?;
    if let Some(mime) = detect_image_mime(path, &bytes) {
        use std::io::Write;
        let mut buf = Vec::with_capacity(bytes.len().div_ceil(3) * 4);
        // Base64 encode the image bytes inline (the agent loop
        // already understands the `ImageContent` shape).
        let mut enc = base64::EncoderWriter::new(&mut buf, &BASE64_TABLE);
        enc.write_all(&bytes).map_err(|err| FileError::Io {
            path: path.to_path_buf(),
            source: err,
        })?;
        drop(enc);
        return Ok(FileRead::Image(ImageContent {
            mime_type: mime.to_string(),
            data: String::from_utf8(buf).unwrap_or_default(),
        }));
    }
    let text = String::from_utf8(bytes).map_err(|_| FileError::NotUtf8 {
        path: path.to_path_buf(),
    })?;
    Ok(FileRead::Text(text))
}

/// Tiny base64 alphabet table (RFC 4648 standard). Defined locally so
/// the file-processor does not pull in the `base64` crate.
const BASE64_TABLE: [u8; 64] = *b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

mod base64 {
    use std::io::Write;

    pub(super) struct EncoderWriter<'a, W: Write> {
        inner: &'a mut W,
        buf: [u8; 3],
        len: usize,
    }

    impl<'a, W: Write> EncoderWriter<'a, W> {
        pub(super) fn new(inner: &'a mut W, _table: &[u8; 64]) -> Self {
            Self {
                inner,
                buf: [0; 3],
                len: 0,
            }
        }
    }

    impl<W: Write> Write for EncoderWriter<'_, W> {
        fn write(&mut self, mut src: &[u8]) -> std::io::Result<usize> {
            let written = src.len();
            while !src.is_empty() {
                let take = src.len().min(3 - self.len);
                self.buf[self.len..self.len + take].copy_from_slice(&src[..take]);
                self.len += take;
                src = &src[take..];
                if self.len == 3 {
                    let n = ((self.buf[0] as u32) << 16)
                        | ((self.buf[1] as u32) << 8)
                        | self.buf[2] as u32;
                    let alphabet =
                        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
                    let out = [
                        alphabet[((n >> 18) & 0x3F) as usize],
                        alphabet[((n >> 12) & 0x3F) as usize],
                        alphabet[((n >> 6) & 0x3F) as usize],
                        alphabet[(n & 0x3F) as usize],
                    ];
                    self.inner.write_all(&out)?;
                    self.len = 0;
                }
            }
            Ok(written)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            self.inner.flush()
        }
    }

    impl<W: Write> Drop for EncoderWriter<'_, W> {
        fn drop(&mut self) {
            // Best-effort flush of any remaining bytes.
            if self.len > 0 {
                let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
                let mut n =
                    ((self.buf[0] as u32) << 16) | ((self.buf[1] as u32) << 8) | self.buf[2] as u32;
                let pad = 3 - self.len;
                n <<= pad * 8;
                let mut out = [alphabet[((n >> 18) & 0x3F) as usize], 0, 0, 0];
                out[1] = alphabet[((n >> 12) & 0x3F) as usize];
                if self.len >= 2 {
                    out[2] = alphabet[((n >> 6) & 0x3F) as usize];
                } else {
                    out[2] = b'=';
                }
                if self.len >= 3 {
                    out[3] = alphabet[(n & 0x3F) as usize];
                } else {
                    out[3] = b'=';
                }
                let _ = self.inner.write_all(&out);
            }
        }
    }
}

/// Detect an image MIME type from the file extension and a few magic
/// bytes. Returns `None` for everything else (text, unknown binary).
fn detect_image_mime(path: &Path, bytes: &[u8]) -> Option<&'static str> {
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase());
    let by_ext = match ext.as_deref() {
        Some("png") => Some("image/png"),
        Some("jpg") | Some("jpeg") => Some("image/jpeg"),
        Some("gif") => Some("image/gif"),
        Some("webp") => Some("image/webp"),
        _ => None,
    };
    if by_ext.is_some() {
        return by_ext;
    }
    // Magic-byte fallback when the extension is missing / unknown.
    if bytes.len() >= 8 && &bytes[..8] == b"\x89PNG\r\n\x1a\n" {
        return Some("image/png");
    }
    if bytes.len() >= 3 && &bytes[..3] == b"\xFF\xD8\xFF" {
        return Some("image/jpeg");
    }
    if bytes.len() >= 6 && (&bytes[..6] == b"GIF87a" || &bytes[..6] == b"GIF89a") {
        return Some("image/gif");
    }
    if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    None
}

/// Decide whether stdin is a TTY. Factored out so it can be overridden
/// from tests by injecting a fake stdin via [`set_stdin_tty_override`].
fn is_stdin_a_tty() -> bool {
    if let Some(value) = stdin_tty_override() {
        return value;
    }
    std::io::stdin().is_terminal()
}

// Test override plumbing — a single `AtomicI8` so the three possible
// states (unset / force-tty / force-piped) all fit without races.
use std::sync::atomic::{AtomicI8, Ordering};
static STDIN_TTY_OVERRIDE: AtomicI8 = AtomicI8::new(STATE_UNSET);
const STATE_UNSET: i8 = 0;
const STATE_FORCE_TTY: i8 = 1;
const STATE_FORCE_PIPED: i8 = 2;

/// Override the result of [`std::io::stdin().is_terminal()`] for tests.
/// Pass `Some(true)` to force a TTY (the piped branch returns `None`);
/// pass `Some(false)` to force a non-TTY (the piped branch returns the
/// piped content). Pass `None` to use the real `is_terminal()`.
pub fn set_stdin_tty_override(value: Option<bool>) {
    let state = match value {
        None => STATE_UNSET,
        Some(true) => STATE_FORCE_TTY,
        Some(false) => STATE_FORCE_PIPED,
    };
    STDIN_TTY_OVERRIDE.store(state, Ordering::SeqCst);
}

fn stdin_tty_override() -> Option<bool> {
    match STDIN_TTY_OVERRIDE.load(Ordering::SeqCst) {
        STATE_FORCE_TTY => Some(true),
        STATE_FORCE_PIPED => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_temp(contents: &[u8], ext: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "pi-coding-agent-fileproc-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("file.{ext}"));
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(contents).unwrap();
        path
    }

    #[test]
    fn expands_at_file_into_block() {
        let path = write_temp(b"hello world", "txt");
        let prompt = format!("please review @{}", path.display());
        let out = expand_prompt(&prompt, None).unwrap();
        assert!(out.images.is_empty());
        assert!(out.text.contains("<file"));
        assert!(out.text.contains("hello world"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn appends_stdin_after_files() {
        let path = write_temp(b"file body", "txt");
        let prompt = format!("@{} on stdin", path.display());
        let stdin = Some("piped content");
        let out = expand_prompt(&prompt, stdin).unwrap();
        assert!(out.text.contains("<stdin>"));
        assert!(out.text.contains("piped content"));
        assert!(out.text.contains("file body"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn rejects_missing_file() {
        let prompt = "@/does/not/exist/file.txt please";
        let err = expand_prompt(prompt, None).unwrap_err();
        assert!(matches!(err, FileError::NotFound { .. }));
        assert_eq!(err.exit_code(), 64);
    }

    #[test]
    fn rejects_directory() {
        let dir = std::env::temp_dir().join(format!(
            "pi-coding-agent-fileproc-dir-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let prompt = format!("@{} inspect", dir.display());
        let err = expand_prompt(&prompt, None).unwrap_err();
        assert!(matches!(err, FileError::IsDirectory { .. }));
        assert_eq!(err.exit_code(), 64);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_oversize_file() {
        let path = write_temp(&vec![b'a'; (MAX_FILE_BYTES as usize) + 1], "txt");
        let prompt = format!("@{} body", path.display());
        let err = expand_prompt(&prompt, None).unwrap_err();
        assert!(matches!(err, FileError::TooLarge { .. }));
        assert_eq!(err.exit_code(), 64);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn detects_image_by_extension() {
        // Minimal PNG signature so detect_image_mime returns image/png
        // even without the extension-based shortcut.
        let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
        bytes.extend_from_slice(&[0; 32]);
        let path = write_temp(&bytes, "png");
        let prompt = format!("@{}", path.display());
        let out = expand_prompt(&prompt, None).unwrap();
        assert_eq!(out.images.len(), 1);
        assert_eq!(out.images[0].mime_type, "image/png");
        assert!(!out.images[0].data.is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn double_at_is_literal() {
        let out = expand_prompt("email me at @@someone", None).unwrap();
        assert!(out.text.contains("email me at @someone"));
    }
}
