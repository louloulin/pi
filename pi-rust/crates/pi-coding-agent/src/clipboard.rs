//! System-clipboard reading for `app.clipboard.pasteImage` (`Alt+V`).
//!
//! The App cannot see the terminal or the system clipboard, so the driver
//! owns this half of the chord: [`App::take_image_paste_request`] tells it a
//! paste was asked for, the driver reads the clipboard through a
//! [`ClipboardReader`], and hands the result back with [`App::paste_image`]
//! (image chip) or [`App::paste_text`] (text fallback). This mirrors
//! upstream's `handleClipboardPaste`
//! (`packages/coding-agent/src/modes/interactive/interactive-mode.ts:2933`)
//! and its platform-specific `readClipboardImage`
//! (`packages/coding-agent/src/utils/clipboard-image.ts`), with one
//! deliberate deviation: upstream writes the image to a temp file and inserts
//! that path, while the Rust port attaches the bytes as an image content
//! block.
//!
//! The default reader shells out to the platform's clipboard tool
//! (`wl-paste` / `xclip` on Linux, `pngpaste` / `pbpaste` on macOS,
//! PowerShell on Windows and WSL). Every backend is best-effort and
//! time-boxed: a missing tool, an empty clipboard or a stalled command all
//! degrade to [`ClipboardPaste::Empty`] rather than blocking the TUI. Tests
//! inject a [`ClipboardReader`] fake instead of touching the real clipboard.
//!
//! [`App::take_image_paste_request`]: crate::interactive
//! [`App::paste_image`]: crate::interactive
//! [`App::paste_text`]: crate::interactive

use std::time::Duration;

use pi_protocol::ImageContent;

/// How long a clipboard "which types are available?" probe may take.
const LIST_TIMEOUT: Duration = Duration::from_millis(1_000);
/// How long fetching the actual (possibly large) payload may take.
const READ_TIMEOUT: Duration = Duration::from_millis(2_000);
/// PowerShell/wl-paste startup on Windows/WSL is slow; give it more room.
const SLOW_TIMEOUT: Duration = Duration::from_millis(5_000);

/// Image MIME types the port can hand to a provider, most preferred first.
const PREFERRED_IMAGE_MIME_TYPES: &[&str] = &["image/png", "image/jpeg", "image/webp", "image/gif"];

/// What `app.clipboard.pasteImage` found on the system clipboard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardPaste {
    /// An image, ready to attach as an [`ImageContent`] block.
    Image(ImageContent),
    /// No image, but text — pasted like a normal terminal paste.
    Text(String),
    /// Nothing usable on the clipboard.
    Empty,
}

/// Reads the system clipboard for the paste-image chord.
///
/// The driver depends on this trait rather than a concrete backend so tests
/// can drive the image / text / empty paths without a real clipboard.
pub trait ClipboardReader: Send + Sync {
    /// Read the clipboard once. Never blocks longer than the backend
    /// timeouts and never panics.
    fn read(&self) -> ClipboardPaste;
}

/// The production reader: shells out to the platform clipboard tool.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClipboard;

impl SystemClipboard {
    /// Construct the platform reader.
    pub fn new() -> Self {
        Self
    }
}

impl ClipboardReader for SystemClipboard {
    fn read(&self) -> ClipboardPaste {
        read_system_clipboard()
    }
}

fn read_system_clipboard() -> ClipboardPaste {
    if cfg!(target_os = "macos") {
        return read_macos().unwrap_or(ClipboardPaste::Empty);
    }
    if cfg!(target_os = "windows") {
        return read_windows().unwrap_or(ClipboardPaste::Empty);
    }

    // Linux / BSD: Wayland first (its clipboard is separate from X11's),
    // then X11, then the Windows clipboard via PowerShell when running under
    // WSL (screenshots taken on Windows do not reach the Linux side).
    if is_wayland() {
        if let Some(paste) = read_wayland() {
            return paste;
        }
    }
    if !is_wsl() {
        return read_x11().unwrap_or(ClipboardPaste::Empty);
    }
    read_x11()
        .or_else(read_windows)
        .unwrap_or(ClipboardPaste::Empty)
}

fn is_wayland() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some()
        || std::env::var("XDG_SESSION_TYPE")
            .is_ok_and(|value| value.eq_ignore_ascii_case("wayland"))
}

fn is_wsl() -> bool {
    if std::env::var_os("WSL_DISTRO_NAME").is_some() || std::env::var_os("WSLENV").is_some() {
        return true;
    }
    std::fs::read_to_string("/proc/version")
        .map(|release| {
            let release = release.to_ascii_lowercase();
            release.contains("microsoft") || release.contains("wsl")
        })
        .unwrap_or(false)
}

/// `Option` here means "this backend could not be tried / failed"; a
/// successful probe returns a definitive [`ClipboardPaste`].
fn read_wayland() -> Option<ClipboardPaste> {
    let types = run_text("wl-paste", &["--list-types"], LIST_TIMEOUT)?;
    let types = split_types(&types);
    if let Some(mime) = select_image_mime(&types) {
        let bytes = run("wl-paste", &["--type", &mime, "--no-newline"], READ_TIMEOUT)?;
        return Some(image_or_empty(&mime, bytes));
    }
    let text = run_text("wl-paste", &["--no-newline"], LIST_TIMEOUT)?;
    Some(text_or_empty(text))
}

fn read_x11() -> Option<ClipboardPaste> {
    let targets = run_text(
        "xclip",
        &["-selection", "clipboard", "-t", "TARGETS", "-o"],
        LIST_TIMEOUT,
    );
    let types = targets.as_deref().map(split_types).unwrap_or_default();
    if let Some(mime) = select_image_mime(&types) {
        let bytes = run(
            "xclip",
            &["-selection", "clipboard", "-t", &mime, "-o"],
            READ_TIMEOUT,
        )?;
        return Some(image_or_empty(&mime, bytes));
    }
    let text = run_text("xclip", &["-selection", "clipboard", "-o"], LIST_TIMEOUT)?;
    Some(text_or_empty(text))
}

fn read_macos() -> Option<ClipboardPaste> {
    // `pngpaste` is the common way to get raw image bytes out of the macOS
    // pasteboard; a missing binary just falls through to text.
    if let Some(bytes) = run("pngpaste", &["-"], READ_TIMEOUT) {
        if !bytes.is_empty() {
            return Some(image_or_empty("image/png", bytes));
        }
    }
    let text = run_text("pbpaste", &[], LIST_TIMEOUT)?;
    Some(text_or_empty(text))
}

fn read_windows() -> Option<ClipboardPaste> {
    let image_script = concat!(
        "Add-Type -AssemblyName System.Windows.Forms;",
        "Add-Type -AssemblyName System.Drawing;",
        "$img=[System.Windows.Forms.Clipboard]::GetImage();",
        "if ($img) {$ms=New-Object System.IO.MemoryStream;",
        "$img.Save($ms,[System.Drawing.Imaging.ImageFormat]::Png);",
        "[Convert]::ToBase64String($ms.ToArray())}"
    );
    if let Some(text) = run_text(
        "powershell.exe",
        &["-NoProfile", "-Command", image_script],
        SLOW_TIMEOUT,
    ) {
        let base64 = text.trim();
        if !base64.is_empty() {
            return Some(ClipboardPaste::Image(ImageContent {
                mime_type: "image/png".to_string(),
                data: base64.to_string(),
            }));
        }
        // PowerShell produced no image; fall through to its text clipboard.
        if let Some(text) = run_text(
            "powershell.exe",
            &["-NoProfile", "-Command", "Get-Clipboard -Raw"],
            SLOW_TIMEOUT,
        ) {
            let text = text.trim_end_matches(['\r', '\n']).to_string();
            return Some(text_or_empty(text));
        }
    }
    None
}

fn text_or_empty(text: String) -> ClipboardPaste {
    if text.is_empty() {
        ClipboardPaste::Empty
    } else {
        ClipboardPaste::Text(text)
    }
}

fn image_or_empty(mime: &str, bytes: Vec<u8>) -> ClipboardPaste {
    if bytes.is_empty() {
        return ClipboardPaste::Empty;
    }
    ClipboardPaste::Image(ImageContent {
        mime_type: base_mime_type(mime),
        data: pi_tui::base64_encode(&bytes),
    })
}

/// Split a clipboard type listing into trimmed, non-empty entries.
fn split_types(raw: &str) -> Vec<String> {
    raw.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

/// Pick the image MIME type to fetch, preferring a format providers accept.
fn select_image_mime(types: &[String]) -> Option<String> {
    for preferred in PREFERRED_IMAGE_MIME_TYPES {
        if let Some(found) = types.iter().find(|t| base_mime_type(t) == *preferred) {
            return Some(found.clone());
        }
    }
    types
        .iter()
        .find(|t| base_mime_type(t).starts_with("image/"))
        .cloned()
}

/// Strip any `;charset=…` parameters and normalise case.
fn base_mime_type(mime: &str) -> String {
    mime.split(';')
        .next()
        .unwrap_or(mime)
        .trim()
        .to_ascii_lowercase()
}

/// Run `program args…`, returning stdout. `None` when the program is
/// missing, exits non-zero, or does not finish within `timeout` (the child
/// is killed).
fn run(program: &str, args: &[&str], timeout: Duration) -> Option<Vec<u8>> {
    use std::io::Read;
    use std::process::{Command, Stdio};

    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    // Drain the pipe on a helper thread so the timeout can fire even when
    // the child fills the OS pipe buffer.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        let _ = tx.send(buf);
    });
    match rx.recv_timeout(timeout) {
        Ok(buf) => {
            let status = child.wait().ok()?;
            if status.success() {
                Some(buf)
            } else {
                None
            }
        }
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            None
        }
    }
}

fn run_text(program: &str, args: &[&str], timeout: Duration) -> Option<String> {
    let bytes = run(program, args, timeout)?;
    String::from_utf8(bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preferred_mime_wins_over_other_image_types() {
        let types = vec![
            "text/plain".to_string(),
            "image/bmp".to_string(),
            "image/jpeg".to_string(),
            "image/png;charset=binary".to_string(),
        ];
        assert_eq!(
            select_image_mime(&types).as_deref(),
            Some("image/png;charset=binary")
        );
    }

    #[test]
    fn unsupported_image_type_still_matches() {
        let types = vec!["text/html".to_string(), "image/bmp".to_string()];
        assert_eq!(select_image_mime(&types).as_deref(), Some("image/bmp"));
        assert_eq!(select_image_mime(&["text/plain".to_string()]), None);
    }

    #[test]
    fn empty_payload_is_not_an_image() {
        assert_eq!(
            image_or_empty("image/png", Vec::new()),
            ClipboardPaste::Empty
        );
    }

    #[test]
    fn payload_is_base64_encoded_with_normalised_mime() {
        match image_or_empty("image/png;charset=binary", b"png".to_vec()) {
            ClipboardPaste::Image(image) => {
                assert_eq!(image.mime_type, "image/png");
                assert_eq!(image.data, pi_tui::base64_encode(b"png"));
            }
            other => panic!("expected image, got {other:?}"),
        }
    }
}
