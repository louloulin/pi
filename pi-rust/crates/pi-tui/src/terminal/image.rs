//! Terminal image kernel — slice 1 of the port of
//! `packages/tui/src/terminal-image.ts`.
//!
//! This module currently covers the *capability layer* of the upstream file
//! (`terminal-image.ts:6-211`): the [`TerminalCapabilities`] record and its
//! environment detection, the resettable capability cache, the cell-pixel
//! dimensions, the kitty/iTerm2 line prefixes, [`is_image_line`] and
//! [`allocate_image_id`]. Slice 2 added the encoders and the kitty
//! metadata/cropping surface, slice 3 the geometry, the four pixel-size
//! parsers, [`render_image`] and [`image_fallback`]; the three sections below
//! therefore cover all of upstream `terminal-image.ts`.
//!
//! Two divergences from upstream, both shared with [`crate::hyperlink`]:
//!
//! * `detectCapabilities()` probes tmux by spawning
//!   `tmux display-message -p '#{client_termfeatures}'`. `pi-tui` must not
//!   spawn processes from the render path, so the tmux branch asks
//!   [`hyperlink::detect_hyperlinks_from_env`] instead — which is `false`
//!   unless `PI_HYPERLINKS` says otherwise.
//! * Upstream reads `process.env` inline. Here the environment is read once
//!   into [`CapabilityInputs`] and the rule itself
//!   ([`detect_capabilities_with`]) is pure, so every branch is testable
//!   without mutating process-global state.

use parking_lot::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::utils::hyperlink as hyperlink;

/// Inline image protocol the terminal understands.
///
/// Upstream spells this `"kitty" | "iterm2" | null`; the `null` arm is
/// [`Option::None`] here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageProtocol {
    /// Kitty graphics protocol (`\x1b_G…`).
    Kitty,
    /// iTerm2 inline images (`\x1b]1337;File=…`).
    Iterm2,
}

impl ImageProtocol {
    /// The upstream string spelling (`"kitty"` / `"iterm2"`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Kitty => "kitty",
            Self::Iterm2 => "iterm2",
        }
    }
}

/// What the attached terminal can render.
///
/// Mirrors upstream's `TerminalCapabilities` (`terminal-image.ts:10-14`). The
/// conservative default — no images, no true colour, no hyperlinks — is the
/// "unknown terminal" arm of `detectCapabilitiesFromEnvironment`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TerminalCapabilities {
    /// Inline image protocol, or `None` when images are unreliable.
    pub images: Option<ImageProtocol>,
    /// The terminal renders 24-bit colour.
    pub true_color: bool,
    /// The terminal renders OSC 8 hyperlinks (see [`crate::hyperlink`]).
    pub hyperlinks: bool,
}

/// Terminal cell size in pixels.
///
/// Mirrors `CellDimensions` (`terminal-image.ts:17-20`). The default
/// `9 × 18` matches upstream's initial value, which the TUI replaces once the
/// terminal answers its cell-size query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellDimensions {
    /// Cell width in pixels.
    pub width_px: u32,
    /// Cell height in pixels.
    pub height_px: u32,
}

impl Default for CellDimensions {
    fn default() -> Self {
        Self {
            width_px: 9,
            height_px: 18,
        }
    }
}

/// Pixel size of an image, as parsed from its base64 header.
///
/// Mirrors `ImageDimensions` (`terminal-image.ts:22-25`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageDimensions {
    /// Image width in pixels.
    pub width_px: u32,
    /// Image height in pixels.
    pub height_px: u32,
}

/// Options for the (later-slice) `render_image`.
///
/// Mirrors `ImageRenderOptions` (`terminal-image.ts:27-40`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ImageRenderOptions {
    /// Maximum render width in terminal cells.
    pub max_width_cells: Option<u32>,
    /// Maximum render height in terminal cells.
    pub max_height_cells: Option<u32>,
    /// iTerm2 only: keep the aspect ratio. Upstream defaults this to `true`.
    pub preserve_aspect_ratio: Option<bool>,
    /// Kitty only: reuse/replace an existing image id.
    pub image_id: Option<u32>,
    /// Kitty only: let the terminal apply its default cursor movement.
    /// Upstream defaults this to `true`, so only `Some(false)` is meaningful.
    pub move_cursor: Option<bool>,
}

/// Kitty graphics protocol introducer.
pub const KITTY_PREFIX: &str = "\u{1b}_G";

/// iTerm2 inline-image introducer.
pub const ITERM2_PREFIX: &str = "\u{1b}]1337;File=";

/// Whether a rendered line carries an inline-image escape sequence.
///
/// Mirrors `isImageLine` (`terminal-image.ts:196-206`): single-row images start
/// with a prefix, multi-row ones carry the sequence behind a cursor-up
/// sequence, so a substring match is required as well.
pub fn is_image_line(line: &str) -> bool {
    line.starts_with(KITTY_PREFIX)
        || line.starts_with(ITERM2_PREFIX)
        || line.contains(KITTY_PREFIX)
        || line.contains(ITERM2_PREFIX)
}

/// The environment `detectCapabilitiesFromEnvironment` inspects.
///
/// Values are stored already-lowercased where upstream lowercases them; the
/// `has_*` flags mirror upstream's `process.env.X` presence checks.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CapabilityInputs {
    /// Lowercased `TERM_PROGRAM`.
    pub term_program: Option<String>,
    /// Lowercased `TERMINAL_EMULATOR`.
    pub terminal_emulator: Option<String>,
    /// Lowercased `TERM`.
    pub term: Option<String>,
    /// Lowercased `COLORTERM`.
    pub color_term: Option<String>,
    /// `TMUX` is set.
    pub tmux: bool,
    /// `KITTY_WINDOW_ID` is set.
    pub kitty_window_id: bool,
    /// `GHOSTTY_RESOURCES_DIR` is set.
    pub ghostty_resources_dir: bool,
    /// `WEZTERM_PANE` is set.
    pub wezterm_pane: bool,
    /// `WARP_SESSION_ID` is set.
    pub warp_session_id: bool,
    /// `WARP_TERMINAL_SESSION_UUID` is set.
    pub warp_terminal_session_uuid: bool,
    /// `ITERM_SESSION_ID` is set.
    pub iterm_session_id: bool,
    /// `WT_SESSION` is set.
    pub wt_session: bool,
    /// Upstream's `process.platform === "win32"`.
    pub is_windows_console: bool,
}

/// Read the capability environment.
///
/// Non-Unicode values are treated as unset, matching upstream's
/// `process.env.X || ""`.
pub fn capability_inputs_from_env() -> CapabilityInputs {
    fn env_string(key: &str) -> Option<String> {
        std::env::var_os(key)
            .and_then(|v| v.into_string().ok())
            .map(|v| v.to_lowercase())
    }
    CapabilityInputs {
        term_program: env_string("TERM_PROGRAM"),
        terminal_emulator: env_string("TERMINAL_EMULATOR"),
        term: env_string("TERM"),
        color_term: env_string("COLORTERM"),
        tmux: std::env::var_os("TMUX").is_some(),
        kitty_window_id: std::env::var_os("KITTY_WINDOW_ID").is_some(),
        ghostty_resources_dir: std::env::var_os("GHOSTTY_RESOURCES_DIR").is_some(),
        wezterm_pane: std::env::var_os("WEZTERM_PANE").is_some(),
        warp_session_id: std::env::var_os("WARP_SESSION_ID").is_some(),
        warp_terminal_session_uuid: std::env::var_os("WARP_TERMINAL_SESSION_UUID").is_some(),
        iterm_session_id: std::env::var_os("ITERM_SESSION_ID").is_some(),
        wt_session: std::env::var_os("WT_SESSION").is_some(),
        is_windows_console: cfg!(windows),
    }
}

/// The capability rule, with the environment already read.
///
/// `tmux_forwards_hyperlinks` stands in for upstream's
/// `tmuxForwardsHyperlink()` probe callback (`terminal-image.ts:139-141`); it is
/// only consulted in the tmux/screen arms, exactly like upstream.
///
/// The `PI_*` environment overrides are applied on top by
/// [`apply_env_overrides`], which is what upstream's `detectCapabilities()`
/// does after this function returns.
pub fn detect_capabilities_with(
    inputs: &CapabilityInputs,
    tmux_forwards_hyperlinks: bool,
) -> TerminalCapabilities {
    let term_program = inputs.term_program.as_deref().unwrap_or("");
    let term = inputs.term.as_deref().unwrap_or("");
    let terminal_emulator = inputs.terminal_emulator.as_deref().unwrap_or("");
    let color_term = inputs.color_term.as_deref().unwrap_or("");
    let has_true_color_hint = color_term == "truecolor" || color_term == "24bit";

    // tmux only forwards OSC 8 when its client advertises `hyperlinks`, and
    // its image protocols are unreliable, so `images` stays `None`.
    if inputs.tmux || term.starts_with("tmux") {
        return TerminalCapabilities {
            images: None,
            true_color: has_true_color_hint,
            hyperlinks: tmux_forwards_hyperlinks,
        };
    }

    // screen swallows OSC 8.
    if term.starts_with("screen") {
        return TerminalCapabilities {
            images: None,
            true_color: has_true_color_hint,
            hyperlinks: false,
        };
    }

    if inputs.kitty_window_id || term_program == "kitty" {
        return TerminalCapabilities {
            images: Some(ImageProtocol::Kitty),
            true_color: true,
            hyperlinks: true,
        };
    }

    if term_program == "ghostty" || term.contains("ghostty") || inputs.ghostty_resources_dir {
        return TerminalCapabilities {
            images: Some(ImageProtocol::Kitty),
            true_color: true,
            hyperlinks: true,
        };
    }

    if inputs.wezterm_pane || term_program == "wezterm" {
        return TerminalCapabilities {
            images: Some(ImageProtocol::Kitty),
            true_color: true,
            hyperlinks: true,
        };
    }

    if term_program == "warpterminal" || inputs.warp_session_id || inputs.warp_terminal_session_uuid
    {
        return TerminalCapabilities {
            images: Some(ImageProtocol::Kitty),
            true_color: true,
            hyperlinks: true,
        };
    }

    if inputs.iterm_session_id || term_program == "iterm.app" {
        return TerminalCapabilities {
            images: Some(ImageProtocol::Iterm2),
            true_color: true,
            hyperlinks: true,
        };
    }

    if inputs.wt_session {
        return TerminalCapabilities {
            images: None,
            true_color: true,
            hyperlinks: true,
        };
    }

    if term_program == "alacritty" || term_program == "vscode" || term_program == "zed" {
        return TerminalCapabilities {
            images: None,
            true_color: true,
            hyperlinks: true,
        };
    }

    if terminal_emulator == "jetbrains-jediterm" {
        return TerminalCapabilities {
            images: None,
            true_color: true,
            hyperlinks: false,
        };
    }

    // Windows Terminal does not always set `WT_SESSION` (e.g. a cmd.exe started
    // from Win+R). Modern consoles do true colour, but hyperlinks stay off
    // unless positively detected above.
    if inputs.is_windows_console {
        return TerminalCapabilities {
            images: None,
            true_color: true,
            hyperlinks: false,
        };
    }

    // Unknown terminal: be conservative. A terminal that swallows OSC 8 renders
    // the link target invisibly, which loses the URL entirely, so hyperlinks
    // default to off.
    TerminalCapabilities {
        images: None,
        true_color: has_true_color_hint,
        hyperlinks: false,
    }
}

fn parse_bool_override(value: Option<&str>) -> Option<bool> {
    match value {
        Some("1") => Some(true),
        Some("0") => Some(false),
        _ => None,
    }
}

/// Apply the `PI_IMAGE_PROTOCOL` / `PI_TRUE_COLOR` / `PI_HYPERLINKS`
/// environment overrides to an already-detected capability record.
///
/// Mirrors the merge at the end of upstream's `detectCapabilities()`
/// (`terminal-image.ts:150-163`). `PI_IMAGE_PROTOCOL` accepts `"kitty"` /
/// `"iterm2"` (any case), and `"none"` / `"0"` to force images off; anything
/// else leaves the detection in place. The boolean overrides only accept
/// `"1"` / `"0"`.
pub fn apply_env_overrides(
    caps: &mut TerminalCapabilities,
    pi_image_protocol: Option<&str>,
    pi_true_color: Option<&str>,
    pi_hyperlinks: Option<&str>,
) {
    match pi_image_protocol.map(str::to_lowercase).as_deref() {
        Some("kitty") => caps.images = Some(ImageProtocol::Kitty),
        Some("iterm2") => caps.images = Some(ImageProtocol::Iterm2),
        Some("none") | Some("0") => caps.images = None,
        _ => {}
    }
    if let Some(value) = parse_bool_override(pi_true_color) {
        caps.true_color = value;
    }
    if let Some(value) = parse_bool_override(pi_hyperlinks) {
        caps.hyperlinks = value;
    }
}

/// Detect capabilities from the current environment, without caching.
///
/// Mirrors upstream's `detectCapabilities()` (`terminal-image.ts:148-163`),
/// including the `PI_*` overrides. tmux forwarding is answered by
/// [`hyperlink::detect_hyperlinks_from_env`] instead of a `tmux` subprocess.
pub fn detect_capabilities_from_env() -> TerminalCapabilities {
    let tmux_forwards = hyperlink::detect_hyperlinks_from_env();
    let mut caps = detect_capabilities_with(&capability_inputs_from_env(), tmux_forwards);
    let image_protocol = std::env::var("PI_IMAGE_PROTOCOL").ok();
    let true_color = std::env::var("PI_TRUE_COLOR").ok();
    let hyperlinks = std::env::var("PI_HYPERLINKS").ok();
    apply_env_overrides(
        &mut caps,
        image_protocol.as_deref(),
        true_color.as_deref(),
        hyperlinks.as_deref(),
    );
    caps
}

/// One overridable capability: left alone, explicitly cleared, or set.
///
/// Upstream's overrides are a `Partial<TerminalCapabilities>`, where a present
/// key with the value `null` clears `images`. The three states are spelled out
/// here so that "unset" and "clear" cannot be confused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Override<T> {
    /// Leave the detected value alone.
    #[default]
    Unset,
    /// Force the capability off (only meaningful for `images`).
    Clear,
    /// Force a specific value.
    Set(T),
}

impl<T: Copy> Override<T> {
    /// Apply this override to `slot`.
    pub fn apply_to(self, slot: &mut T) {
        if let Self::Set(value) = self {
            *slot = value;
        }
    }
}

/// Capability overrides a caller (the TUI driver, or a test) can pin.
///
/// Mirrors upstream's `capabilityOverrides` (`terminal-image.ts:35`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CapabilityOverrides {
    /// Force the image protocol, or force images off with [`Override::Clear`].
    pub images: Override<ImageProtocol>,
    /// Force true-colour support.
    pub true_color: Override<bool>,
    /// Force hyperlink support.
    pub hyperlinks: Override<bool>,
}

impl CapabilityOverrides {
    /// No overrides at all (`const` so it can seed the process-global state).
    pub const NONE: Self = Self {
        images: Override::Unset,
        true_color: Override::Unset,
        hyperlinks: Override::Unset,
    };

    /// Apply every override to `caps`.
    pub fn apply_to(self, caps: &mut TerminalCapabilities) {
        if let Some(value) = self.images.value() {
            caps.images = value;
        }
        self.true_color.apply_to(&mut caps.true_color);
        self.hyperlinks.apply_to(&mut caps.hyperlinks);
    }
}

impl<T: Copy> Override<T> {
    /// The override as an `Option`, where [`Override::Clear`] is `Some(None)`.
    ///
    /// Only useful for `Option`-valued capabilities; the boolean overrides use
    /// [`Override::apply_to`].
    pub fn value(self) -> Option<Option<T>> {
        match self {
            Self::Unset => None,
            Self::Clear => Some(None),
            Self::Set(value) => Some(Some(value)),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct CapabilityState {
    cached: Option<TerminalCapabilities>,
    overrides: CapabilityOverrides,
}

static CAPABILITIES: Mutex<CapabilityState> = Mutex::new(CapabilityState {
    cached: None,
    overrides: CapabilityOverrides::NONE,
});

/// The process-wide cell dimensions, initially `9 × 18`.
static CELL_DIMENSIONS: Mutex<CellDimensions> = Mutex::new(CellDimensions {
    width_px: 9,
    height_px: 18,
});

/// The current cell dimensions.
///
/// Mirrors `getCellDimensions()` (`terminal-image.ts:40-42`).
pub fn get_cell_dimensions() -> CellDimensions {
    *CELL_DIMENSIONS.lock()
}

/// Replace the cell dimensions (the TUI does this once the terminal answers
/// its cell-size query).
///
/// Mirrors `setCellDimensions()` (`terminal-image.ts:44-46`).
pub fn set_cell_dimensions(dimensions: CellDimensions) {
    *CELL_DIMENSIONS.lock() = dimensions;
}

/// The cached capabilities, detecting them on first use.
///
/// Mirrors `getCapabilities()` (`terminal-image.ts:160-169`): overrides are
/// merged over the detected record, and the result is cached until
/// [`reset_capabilities_cache`] or [`set_capabilities`].
pub fn get_capabilities() -> TerminalCapabilities {
    let mut state = CAPABILITIES.lock();
    if let Some(caps) = state.cached {
        return caps;
    }
    // A pinned hyperlink override also feeds the tmux branch, like upstream's
    // `() => hyperlinks` callback.
    let tmux_forwards = match state.overrides.hyperlinks {
        Override::Set(value) => value,
        _ => hyperlink::detect_hyperlinks_from_env(),
    };
    let mut caps = detect_capabilities_with(&capability_inputs_from_env(), tmux_forwards);
    let image_protocol = std::env::var("PI_IMAGE_PROTOCOL").ok();
    let true_color = std::env::var("PI_TRUE_COLOR").ok();
    let hyperlinks = std::env::var("PI_HYPERLINKS").ok();
    apply_env_overrides(
        &mut caps,
        image_protocol.as_deref(),
        true_color.as_deref(),
        hyperlinks.as_deref(),
    );
    state.overrides.apply_to(&mut caps);
    state.cached = Some(caps);
    caps
}

/// Drop the cached capabilities so the next [`get_capabilities`] re-detects.
///
/// Mirrors `resetCapabilitiesCache()` (`terminal-image.ts:171-173`).
pub fn reset_capabilities_cache() {
    CAPABILITIES.lock().cached = None;
}

/// Pin selected capabilities, dropping the cache when the pin changes.
///
/// Mirrors `setCapabilityOverrides()` (`terminal-image.ts:176-187`): a call
/// that does not change the overrides is a no-op, so the cache survives.
pub fn set_capability_overrides(overrides: CapabilityOverrides) {
    let mut state = CAPABILITIES.lock();
    if state.overrides == overrides {
        return;
    }
    state.overrides = overrides;
    state.cached = None;
}

/// Replace the cached capabilities outright, bypassing detection.
///
/// Mirrors `setCapabilities()` (`terminal-image.ts:189-191`) — the entry tests
/// (and the TUI driver after its own probe) use to exercise both code paths.
pub fn set_capabilities(capabilities: TerminalCapabilities) {
    CAPABILITIES.lock().cached = Some(capabilities);
}

/// Pseudo-random state for [`allocate_image_id`]; `0` means "not seeded yet".
static IMAGE_ID_STATE: Mutex<u64> = Mutex::new(0);

/// Allocate a kitty image id in `1..=0xfffffffe`.
///
/// Mirrors `allocateImageId()` (`terminal-image.ts:210-213`). Upstream uses
/// `Math.random()`; this is an xorshift64\* generator seeded from the clock and
/// the process id, which keeps the port dependency-free (`rand` is not a
/// dependency of `pi-tui`) while preserving the property that matters: ids do
/// not collide across module instances.
pub fn allocate_image_id() -> u32 {
    fn seed() -> u64 {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x9e37_79b9_7f4a_7c15);
        let seeded = nanos ^ ((u64::from(std::process::id())) << 17);
        if seeded == 0 {
            0x9e37_79b9_7f4a_7c15
        } else {
            seeded
        }
    }

    let mut state = IMAGE_ID_STATE.lock();
    if *state == 0 {
        *state = seed();
    }
    let mut x = *state;
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    *state = x;
    let value = x.wrapping_mul(0x2545_f491_4f6c_dd1d);
    // Upstream: Math.floor(Math.random() * 0xfffffffe) + 1
    (value >> 32) as u32 % 0xffff_fffe + 1
}

// ---------------------------------------------------------------------------
// Slice 2 — encoders, kitty metadata/placement and row cropping.
// Ported from `terminal-image.ts:215-433`.
// ---------------------------------------------------------------------------

/// Bytes of base64 payload per kitty transmission chunk.
///
/// Upstream's `CHUNK_SIZE` (`terminal-image.ts:225`). Base64 is ASCII, so
/// chunking on byte offsets is safe.
pub const KITTY_CHUNK_SIZE: usize = 4096;

/// Options for [`encode_kitty`].
///
/// Mirrors the inline options object of upstream's `encodeKitty`
/// (`terminal-image.ts:216-221`). Absent (`None`) and zero behave identically:
/// upstream's truthiness checks skip `0` just like they skip `undefined`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct KittyEncodeOptions {
    /// Columns the image should occupy (`c=`).
    pub columns: Option<u32>,
    /// Rows the image should occupy (`r=`).
    pub rows: Option<u32>,
    /// Reuse/replace this image id (`i=`).
    pub image_id: Option<u32>,
    /// Whether kitty applies its default cursor movement. Upstream defaults
    /// this to `true`, so only `Some(false)` emits `C=1`.
    pub move_cursor: Option<bool>,
}

/// Encode a base64 payload as a (possibly chunked) kitty graphics command.
///
/// Mirrors `encodeKitty` (`terminal-image.ts:215-263`): the first chunk carries
/// the parameters and `m=1`, middle chunks carry `m=1`, and the last chunk
/// carries `m=0`. Chunks are concatenated with no separator.
pub fn encode_kitty(base64_data: &str, options: &KittyEncodeOptions) -> String {
    let mut params: Vec<String> = vec!["a=T".to_string(), "f=100".to_string(), "q=2".to_string()];

    if options.move_cursor == Some(false) {
        params.push("C=1".to_string());
    }
    if let Some(columns) = options.columns.filter(|value| *value != 0) {
        params.push(format!("c={columns}"));
    }
    if let Some(rows) = options.rows.filter(|value| *value != 0) {
        params.push(format!("r={rows}"));
    }
    if let Some(image_id) = options.image_id.filter(|value| *value != 0) {
        params.push(format!("i={image_id}"));
    }
    let joined = params.join(",");

    let bytes = base64_data.as_bytes();
    if bytes.len() <= KITTY_CHUNK_SIZE {
        return format!("\x1b_G{joined};{base64_data}\x1b\\");
    }

    let mut out = String::new();
    let mut offset = 0usize;
    let mut is_first = true;
    while offset < bytes.len() {
        let end = (offset + KITTY_CHUNK_SIZE).min(bytes.len());
        let chunk = String::from_utf8_lossy(&bytes[offset..end]);
        let is_last = offset + KITTY_CHUNK_SIZE >= bytes.len();
        if is_first {
            out.push_str(&format!("\x1b_G{joined},m=1;{chunk}\x1b\\"));
            is_first = false;
        } else if is_last {
            out.push_str(&format!("\x1b_Gm=0;{chunk}\x1b\\"));
        } else {
            out.push_str(&format!("\x1b_Gm=1;{chunk}\x1b\\"));
        }
        offset += KITTY_CHUNK_SIZE;
    }
    out
}

/// Delete one kitty image by id, freeing its data.
///
/// Mirrors `deleteKittyImage` (`terminal-image.ts:265-267`): uppercase `I`.
pub fn delete_kitty_image(image_id: u32) -> String {
    format!("\x1b_Ga=d,d=I,i={image_id},q=2\x1b\\")
}

/// Delete every kitty image, freeing its data.
///
/// Mirrors `deleteAllKittyImages` (`terminal-image.ts:269-273`): uppercase `A`.
pub fn delete_all_kitty_images() -> String {
    "\x1b_Ga=d,d=A,q=2\x1b\\".to_string()
}

/// Delete every kitty placement while keeping the uploaded image data.
///
/// Mirrors `deleteAllKittyPlacements` (`terminal-image.ts:275-277`): lowercase
/// `a` removes placements only.
pub fn delete_all_kitty_placements() -> String {
    "\x1b_Ga=d,d=a,q=2\x1b\\".to_string()
}

/// Options for [`encode_iterm2`].
///
/// Mirrors the inline options object of upstream's `encodeITerm2`
/// (`terminal-image.ts:279-286`). `width` / `height` are strings because
/// upstream accepts `number | string` (including `"auto"`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Iterm2EncodeOptions {
    /// Width as iTerm2 spells it (`width=…`); callers format numbers.
    pub width: Option<String>,
    /// Height as iTerm2 spells it (`height=…`).
    pub height: Option<String>,
    /// File name; upstream base64-encodes it (`name=…`).
    pub name: Option<String>,
    /// Keep the aspect ratio. Only `Some(false)` emits
    /// `preserveAspectRatio=0`.
    pub preserve_aspect_ratio: Option<bool>,
    /// Render inline. `None` means the upstream default of `true`.
    pub inline: Option<bool>,
}

/// Number of bytes `base64_data` decodes to.
///
/// Mirrors Node's `Buffer.byteLength(data, "base64")` (`terminal-image.ts:291`):
/// every 4 base64 characters carry 3 bytes, minus one byte per `=` pad.
pub fn decoded_base64_len(base64_data: &str) -> usize {
    let len = base64_data.len();
    if len == 0 {
        return 0;
    }
    let padding = base64_data
        .as_bytes()
        .iter()
        .rev()
        .take_while(|byte| **byte == b'=')
        .count();
    len / 4 * 3 - padding
}

/// Encode a base64 payload as an iTerm2 inline-image escape sequence.
///
/// Mirrors `encodeITerm2` (`terminal-image.ts:279-308`).
pub fn encode_iterm2(base64_data: &str, options: &Iterm2EncodeOptions) -> String {
    let inline = if options.inline == Some(false) { 0 } else { 1 };
    let mut params: Vec<String> = vec![
        format!("inline={inline}"),
        format!("size={}", decoded_base64_len(base64_data)),
    ];

    if let Some(width) = &options.width {
        params.push(format!("width={width}"));
    }
    if let Some(height) = &options.height {
        params.push(format!("height={height}"));
    }
    if let Some(name) = &options.name {
        if !name.is_empty() {
            params.push(format!(
                "name={}",
                crate::clipboard::base64_encode(name.as_bytes())
            ));
        }
    }
    if options.preserve_aspect_ratio == Some(false) {
        params.push("preserveAspectRatio=0".to_string());
    }

    format!("\x1b]1337;File={}:{}\x07", params.join(";"), base64_data)
}

/// Cell footprint of an image.
///
/// Mirrors `ImageCellSize` (`terminal-image.ts:310-313`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ImageCellSize {
    /// Columns occupied.
    pub columns: u32,
    /// Rows occupied.
    pub rows: u32,
}

/// What a registered kitty image knows about itself.
///
/// Mirrors `KittyImageMetadata` (`terminal-image.ts:315-320`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct KittyImageMetadata {
    /// Kitty image id (`i=`).
    pub image_id: u32,
    /// Columns the image occupies.
    pub columns: u32,
    /// Rows the image occupies.
    pub rows: u32,
    /// Source image width in pixels.
    pub width_px: u32,
    /// Source image height in pixels.
    pub height_px: u32,
}

/// A registered image plus the transmission generation it was registered in.
///
/// Mirrors `RegisteredKittyImageMetadata` (`terminal-image.ts:322-324`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RegisteredKittyImageMetadata {
    metadata: KittyImageMetadata,
    transmission_generation: u64,
}

/// A placement-only command for an image already transmitted to the terminal.
///
/// Mirrors `KittyImagePlacement` (`terminal-image.ts:326-333`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KittyImagePlacement {
    /// Kitty image id the placement refers to.
    pub image_id: u32,
    /// Registration generation of the transmitted image.
    pub transmission_generation: u64,
    /// Bytes of the original transmission that this line carries.
    pub transmission_bytes: usize,
    /// Decoded size the terminal is expected to allocate (`w * h * 4`).
    pub estimated_decoded_bytes: u64,
    /// The placement-only escape sequence.
    pub sequence: String,
    /// `line` with the transmission replaced by `sequence`.
    pub replacement_line: String,
}

/// Control keys kept when rewriting a transmission into a placement.
///
/// Mirrors `KITTY_PLACEMENT_CONTROL_KEYS` (`terminal-image.ts:366-384`).
const KITTY_PLACEMENT_CONTROL_KEYS: [&str; 17] = [
    "i", "p", "x", "y", "w", "h", "X", "Y", "c", "r", "C", "U", "z", "P", "Q", "H", "V",
];

/// Insertion-ordered registry of kitty images.
///
/// Upstream relies on `Map`'s insertion order and deletes the oldest entry once
/// the map exceeds 1000 entries (`terminal-image.ts:336-346`); `order` plays
/// that role here.
struct KittyMetadataTable {
    entries: std::collections::HashMap<u32, RegisteredKittyImageMetadata>,
    order: std::collections::VecDeque<u32>,
}

static KITTY_METADATA: std::sync::OnceLock<Mutex<KittyMetadataTable>> = std::sync::OnceLock::new();

/// The process-wide kitty registry, created on first use.
///
/// `HashMap` has no const constructor, so the table cannot be seeded directly
/// in a `static`; `OnceLock` (already the lazy-state idiom in this crate) keeps
/// the cell initialisation-free.
fn kitty_metadata_table() -> &'static Mutex<KittyMetadataTable> {
    KITTY_METADATA.get_or_init(|| {
        Mutex::new(KittyMetadataTable {
            entries: std::collections::HashMap::new(),
            order: std::collections::VecDeque::new(),
        })
    })
}

/// Monotonic counter stamped onto every registration.
static KITTY_TRANSMISSION_GENERATION: Mutex<u64> = Mutex::new(0);

/// Register (or re-register) an image's metadata.
///
/// Mirrors `registerKittyImageMetadata` (`terminal-image.ts:338-346`):
/// re-registering an id moves it to the back of the insertion order, and the
/// table is trimmed to at most 1000 entries by dropping the oldest.
pub fn register_kitty_image_metadata(metadata: KittyImageMetadata) {
    let generation = {
        let mut counter = KITTY_TRANSMISSION_GENERATION.lock();
        *counter += 1;
        *counter
    };

    let mut table = kitty_metadata_table().lock();
    table.entries.remove(&metadata.image_id);
    table.order.retain(|id| *id != metadata.image_id);
    table.entries.insert(
        metadata.image_id,
        RegisteredKittyImageMetadata {
            metadata,
            transmission_generation: generation,
        },
    );
    table.order.push_back(metadata.image_id);
    while table.entries.len() > 1000 {
        let Some(oldest) = table.order.pop_front() else {
            break;
        };
        table.entries.remove(&oldest);
    }
}

/// The first `\x1b_G<controls>;` in `line`: its index and its controls.
fn find_kitty_control(line: &str) -> Option<(usize, &str)> {
    let index = line.find(KITTY_PREFIX)?;
    let rest = line.get(index + KITTY_PREFIX.len()..)?;
    let semicolon = rest.find(';')?;
    Some((index, &rest[..semicolon]))
}

/// The `i=` value of a kitty control string, if it is a well-formed item.
///
/// Equivalent to upstream's `(?:^|,)i=(\d+)(?:,|$)` without a regex dependency:
/// the item must start with `i=` and the rest must be non-empty ASCII digits.
fn control_image_id(controls: &str) -> Option<u32> {
    for item in controls.split(',') {
        if let Some(value) = item.strip_prefix("i=") {
            if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) {
                return value.parse::<u32>().ok();
            }
        }
    }
    None
}

/// Whether a control string carries the item `m=1` (an unterminated chunk).
fn controls_have_more(controls: &str) -> bool {
    controls.split(',').any(|item| item == "m=1")
}

/// The registry entry an image line's `i=` refers to.
fn get_registered_kitty_image_metadata(line: &str) -> Option<RegisteredKittyImageMetadata> {
    let (_, controls) = find_kitty_control(line)?;
    let image_id = control_image_id(controls)?;
    kitty_metadata_table()
        .lock()
        .entries
        .get(&image_id)
        .copied()
}

/// The metadata of the image an image line references.
///
/// Mirrors `getKittyImageMetadata` (`terminal-image.ts:348-359`); the internal
/// registry's `transmissionGeneration` is not part of the public record.
pub fn get_kitty_image_metadata(line: &str) -> Option<KittyImageMetadata> {
    get_registered_kitty_image_metadata(line).map(|registered| registered.metadata)
}

/// Build a placement-only command for an image line.
///
/// Mirrors `getKittyImagePlacement` (`terminal-image.ts:387-419`), including the
/// walk over `m=1` continuation chunks to find where the transmission ends.
pub fn get_kitty_image_placement(line: &str) -> Option<KittyImagePlacement> {
    let (match_index, match_controls) = find_kitty_control(line)?;
    let metadata = get_registered_kitty_image_metadata(line)?;

    let mut command_start = match_index;
    let mut command_controls = match_controls;
    let mut transmission_end: usize;
    loop {
        let search_from = command_start + KITTY_PREFIX.len();
        let relative = line.get(search_from..)?.find("\x1b\\")?;
        let terminator = search_from + relative;
        transmission_end = terminator + 2;
        if !controls_have_more(command_controls) {
            break;
        }
        command_start = transmission_end;
        let next = line.get(command_start..)?;
        if !next.starts_with(KITTY_PREFIX) {
            return None;
        }
        let controls_start = command_start + KITTY_PREFIX.len();
        let controls_rest = line.get(controls_start..)?;
        let controls_end = controls_rest.find(';')?;
        command_controls = &controls_rest[..controls_end];
    }

    let controls: Vec<&str> = match_controls
        .split(',')
        .filter(|control| {
            let key = control.split('=').next().unwrap_or("");
            KITTY_PLACEMENT_CONTROL_KEYS.contains(&key)
        })
        .collect();
    let sequence = format!("\x1b_Ga=p,q=2,{}\x1b\\", controls.join(","));
    let replacement_line = format!(
        "{}{}{}",
        &line[..match_index],
        sequence,
        &line[transmission_end..]
    );
    Some(KittyImagePlacement {
        image_id: metadata.metadata.image_id,
        transmission_generation: metadata.transmission_generation,
        transmission_bytes: transmission_end - match_index,
        estimated_decoded_bytes: u64::from(metadata.metadata.width_px)
            * u64::from(metadata.metadata.height_px)
            * 4,
        sequence,
        replacement_line,
    })
}

/// Whether a kitty control item is one of the crop keys `y=` / `h=` / `r=`.
fn is_crop_control(control: &str) -> bool {
    control.starts_with("y=") || control.starts_with("h=") || control.starts_with("r=")
}

/// Rewrite an image line so only `visible_rows` of it stay on screen.
///
/// Mirrors `cropKittyImageLine` (`terminal-image.ts:421-433`): the pixel window
/// is derived with `floor` for its start and `ceil` for its end, and at least
/// one pixel row is always emitted.
pub fn crop_kitty_image_line(line: &str, hidden_rows: u32, visible_rows: u32) -> String {
    let Some(metadata) = get_kitty_image_metadata(line) else {
        return line.to_string();
    };
    let Some((match_index, match_controls)) = find_kitty_control(line) else {
        return line.to_string();
    };
    if hidden_rows >= metadata.rows || visible_rows == 0 {
        return line.to_string();
    }
    let cropped_rows = visible_rows.min(metadata.rows - hidden_rows);
    if hidden_rows == 0 && cropped_rows == metadata.rows {
        return line.to_string();
    }

    let rows = u64::from(metadata.rows);
    let height_px = u64::from(metadata.height_px);
    let source_y = height_px * u64::from(hidden_rows) / rows;
    let source_end =
        (height_px * (u64::from(hidden_rows) + u64::from(cropped_rows))).div_ceil(rows);
    let source_height = 1.max(height_px.min(source_end).saturating_sub(source_y));

    let mut controls: Vec<String> = match_controls
        .split(',')
        .filter(|control| !is_crop_control(control))
        .map(str::to_string)
        .collect();
    controls.push(format!("y={source_y}"));
    controls.push(format!("h={source_height}"));
    controls.push(format!("r={cropped_rows}"));

    let match_len = KITTY_PREFIX.len() + match_controls.len() + 1;
    format!(
        "{}\x1b_G{};{}",
        &line[..match_index],
        controls.join(","),
        &line[match_index + match_len..]
    )
}

// ---------------------------------------------------------------------------
// Slice 3 — geometry, the four pixel-size parsers, `renderImage` and
// `imageFallback`.
// Ported from `terminal-image.ts:435-696`.
// ---------------------------------------------------------------------------

/// What [`render_image`] produces.
///
/// Mirrors the object upstream returns (`terminal-image.ts:610-613`); the
/// `imageId` key is [`Option::None`] for iTerm2 and for kitty when the caller
/// did not supply one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderImageResult {
    /// The escape sequence to write to the terminal.
    pub sequence: String,
    /// Columns the image occupies.
    pub columns: u32,
    /// Rows the image occupies.
    pub rows: u32,
    /// Kitty image id, when the caller supplied one.
    pub image_id: Option<u32>,
}

/// Fit an image into a box of cells while keeping its aspect ratio.
///
/// Mirrors `calculateImageCellSize` (`terminal-image.ts:435-467`): the scale is
/// `min(widthScale, heightScale)` and both counts round *up*, so an image never
/// gets fewer cells than it needs. `max_width_cells` / `max_height_cells`
/// below 1 clamp up to 1, as does a zero-sized image.
///
/// The scaling runs in `f64` because upstream does: `Math.ceil` on a value that
/// lands exactly on an integer can still round up when the intermediate product
/// is inexact in binary, and matching that rounding direction is the point of
/// the port.
pub fn calculate_image_cell_size(
    image_dimensions: ImageDimensions,
    max_width_cells: u32,
    max_height_cells: Option<u32>,
    cell_dimensions: CellDimensions,
) -> ImageCellSize {
    // Upstream's `Math.max(1, Math.floor(x))`; `u32` has neither fractional nor
    // negative values, so only the clamp is left.
    let max_width = max_width_cells.max(1);
    let max_height = max_height_cells.map(|cells| cells.max(1));
    let image_width = f64::from(image_dimensions.width_px.max(1));
    let image_height = f64::from(image_dimensions.height_px.max(1));
    let cell_width = f64::from(cell_dimensions.width_px);
    let cell_height = f64::from(cell_dimensions.height_px);

    let width_scale = f64::from(max_width) * cell_width / image_width;
    let height_scale = match max_height {
        Some(max_height) => f64::from(max_height) * cell_height / image_height,
        None => width_scale,
    };
    let scale = width_scale.min(height_scale);

    // `f64 as u32` saturates (Rust >= 1.45), which is what upstream's
    // `Math.min(maxWidth, Infinity)` amounts to for zero-sized cells.
    let columns = (image_width * scale / cell_width).ceil() as u32;
    let rows = (image_height * scale / cell_height).ceil() as u32;

    ImageCellSize {
        columns: columns.clamp(1, max_width),
        rows: match max_height {
            Some(max_height) => rows.clamp(1, max_height),
            None => rows.max(1),
        },
    }
}

/// Rows an image occupies when it is fitted to `target_width_cells` columns.
///
/// Mirrors `calculateImageRows` (`terminal-image.ts:469-475`).
pub fn calculate_image_rows(
    image_dimensions: ImageDimensions,
    target_width_cells: u32,
    cell_dimensions: CellDimensions,
) -> u32 {
    calculate_image_cell_size(image_dimensions, target_width_cells, None, cell_dimensions).rows
}

/// The 6-bit value of one base64 alphabet byte.
fn base64_symbol(byte: u8) -> Option<u8> {
    match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

/// Decode at most `max_bytes` bytes from the front of a base64 payload.
///
/// Upstream hands the whole payload to `Buffer.from(data, "base64")`. The port
/// only ever needs a *header*, so it stops at `max_bytes` (and at the input's
/// `=` padding) instead of decoding a multi-megabyte image twice. Like Node,
/// bytes outside the alphabet are skipped and a trailing partial group of two
/// or three symbols still yields its one or two bytes.
fn decode_base64_prefix(base64_data: &str, max_bytes: usize) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::with_capacity(max_bytes.min(64));
    let mut accumulator: u32 = 0;
    let mut symbols = 0usize;
    for byte in base64_data.bytes() {
        if byte == b'=' {
            break;
        }
        let Some(value) = base64_symbol(byte) else {
            continue;
        };
        accumulator = (accumulator << 6) | u32::from(value);
        symbols += 1;
        if symbols == 4 {
            out.push((accumulator >> 16) as u8);
            out.push((accumulator >> 8) as u8);
            out.push(accumulator as u8);
            accumulator = 0;
            symbols = 0;
            if out.len() >= max_bytes {
                break;
            }
        }
    }
    if out.len() < max_bytes {
        match symbols {
            2 => out.push((accumulator >> 4) as u8),
            3 => {
                out.push((accumulator >> 10) as u8);
                out.push((accumulator >> 2) as u8);
            }
            _ => {}
        }
    }
    out.truncate(max_bytes);
    out
}

/// Big-endian `u16` at `offset`, or `None` when the buffer is too short.
///
/// Upstream's `readUInt16BE`/`readUInt32BE` throw on a short buffer and its
/// callers catch that into `null`; `Option` spells the same contract without
/// exceptions.
fn read_u16_be(buffer: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_be_bytes(
        buffer.get(offset..offset + 2)?.try_into().ok()?,
    ))
}

/// Big-endian `u32` at `offset`, or `None` when the buffer is too short.
fn read_u32_be(buffer: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_be_bytes(
        buffer.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

/// Little-endian `u16` at `offset`, or `None` when the buffer is too short.
fn read_u16_le(buffer: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        buffer.get(offset..offset + 2)?.try_into().ok()?,
    ))
}

/// Little-endian `u32` at `offset`, or `None` when the buffer is too short.
fn read_u32_le(buffer: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        buffer.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

/// Pixel size of a PNG, read from the IHDR header.
///
/// Mirrors `getPngDimensions` (`terminal-image.ts:477-496`): only the 24-byte
/// header is decoded, a wrong signature or a short buffer is `None`.
pub fn get_png_dimensions(base64_data: &str) -> Option<ImageDimensions> {
    const HEADER_BYTES: usize = 24;
    let buffer = decode_base64_prefix(base64_data, HEADER_BYTES);
    if buffer.len() < HEADER_BYTES || buffer[..4] != [0x89, b'P', b'N', b'G'] {
        return None;
    }
    Some(ImageDimensions {
        width_px: read_u32_be(&buffer, 16)?,
        height_px: read_u32_be(&buffer, 20)?,
    })
}

/// Pixel size of a JPEG, read from the first SOF0-SOF2 frame header.
///
/// Mirrors `getJpegDimensions` (`terminal-image.ts:498-539`): the marker walk
/// skips each segment by its big-endian length until a start-of-frame marker
/// turns up. Upstream walks the whole decoded buffer; the port decodes at most
/// `JPEG_HEADER_SCAN_LIMIT` (64 KiB) bytes, so a JPEG whose SOF marker sits
/// behind a
/// larger APP segment (a very large EXIF block, say) is reported as `None`
/// instead of being scanned — a documented divergence that keeps the parser
/// from decoding multi-megabyte payloads.
pub fn get_jpeg_dimensions(base64_data: &str) -> Option<ImageDimensions> {
    let buffer = decode_base64_prefix(base64_data, JPEG_HEADER_SCAN_LIMIT);
    if buffer.len() < 2 || buffer[0] != 0xff || buffer[1] != 0xd8 {
        return None;
    }

    let mut offset = 2usize;
    while offset + 9 < buffer.len() {
        if buffer[offset] != 0xff {
            offset += 1;
            continue;
        }
        let marker = buffer[offset + 1];
        if (0xc0..=0xc2).contains(&marker) {
            return Some(ImageDimensions {
                width_px: u32::from(read_u16_be(&buffer, offset + 7)?),
                height_px: u32::from(read_u16_be(&buffer, offset + 5)?),
            });
        }
        if offset + 3 >= buffer.len() {
            return None;
        }
        let length = read_u16_be(&buffer, offset + 2)?;
        if length < 2 {
            return None;
        }
        offset += 2 + usize::from(length);
    }
    None
}

/// Pixel size of a GIF, read from the logical screen descriptor.
///
/// Mirrors `getGifDimensions` (`terminal-image.ts:541-560`): only the 10-byte
/// header is decoded, and GIF87a/GIF89a are the only accepted signatures.
pub fn get_gif_dimensions(base64_data: &str) -> Option<ImageDimensions> {
    const HEADER_BYTES: usize = 10;
    let buffer = decode_base64_prefix(base64_data, HEADER_BYTES);
    if buffer.len() < HEADER_BYTES {
        return None;
    }
    if buffer[..6] != *b"GIF87a" && buffer[..6] != *b"GIF89a" {
        return None;
    }
    Some(ImageDimensions {
        width_px: u32::from(read_u16_le(&buffer, 6)?),
        height_px: u32::from(read_u16_le(&buffer, 8)?),
    })
}

/// Pixel size of a WebP, for all three container flavours.
///
/// Mirrors `getWebpDimensions` (`terminal-image.ts:562-608`): lossy `VP8 `
/// stores a 14-bit size at offset 26, lossless `VP8L` packs both sizes into a
/// little-endian `u32` at offset 21 (minus one each), and the extended `VP8X`
/// carries the 24-bit sizes minus one at offsets 24 and 27.
pub fn get_webp_dimensions(base64_data: &str) -> Option<ImageDimensions> {
    const HEADER_BYTES: usize = 30;
    let buffer = decode_base64_prefix(base64_data, HEADER_BYTES);
    if buffer.len() < HEADER_BYTES {
        return None;
    }
    if buffer[..4] != *b"RIFF" || buffer[8..12] != *b"WEBP" {
        return None;
    }

    match &buffer[12..16] {
        b"VP8 " => Some(ImageDimensions {
            width_px: u32::from(read_u16_le(&buffer, 26)? & 0x3fff),
            height_px: u32::from(read_u16_le(&buffer, 28)? & 0x3fff),
        }),
        b"VP8L" => {
            let bits = read_u32_le(&buffer, 21)?;
            Some(ImageDimensions {
                width_px: (bits & 0x3fff) + 1,
                height_px: ((bits >> 14) & 0x3fff) + 1,
            })
        }
        b"VP8X" => Some(ImageDimensions {
            width_px: (u32::from(buffer[24])
                | (u32::from(buffer[25]) << 8)
                | (u32::from(buffer[26]) << 16))
                + 1,
            height_px: (u32::from(buffer[27])
                | (u32::from(buffer[28]) << 8)
                | (u32::from(buffer[29]) << 16))
                + 1,
        }),
        _ => None,
    }
}

/// Bytes [`get_jpeg_dimensions`] decodes before giving up on the marker walk.
const JPEG_HEADER_SCAN_LIMIT: usize = 64 * 1024;

/// Pixel size of an image, chosen by MIME type.
///
/// Mirrors `getImageDimensions` (`terminal-image.ts:610-621`); an unsupported
/// MIME type is `None` rather than a guess.
pub fn get_image_dimensions(base64_data: &str, mime_type: &str) -> Option<ImageDimensions> {
    match mime_type {
        "image/png" => get_png_dimensions(base64_data),
        "image/jpeg" => get_jpeg_dimensions(base64_data),
        "image/gif" => get_gif_dimensions(base64_data),
        "image/webp" => get_webp_dimensions(base64_data),
        _ => None,
    }
}

/// Render an image as an inline escape sequence.
///
/// Mirrors `renderImage` (`terminal-image.ts:625-663`). `None` means the
/// terminal cannot show inline images. Upstream's `maxWidthCells` default of
/// 80 and `preserveAspectRatio` default of `true` are applied here, and cell
/// dimensions come from [`get_cell_dimensions`].
///
/// A caller-supplied `image_id` registers the computed box in the kitty
/// metadata table *before* encoding, so the line that is written can later be
/// looked up, placed or cropped (`terminal-image.ts:630-638`).
///
/// The surrounding `moveUp` assembly stays out of this function: it belongs to
/// the component layer (LUM-1192).
pub fn render_image(
    base64_data: &str,
    image_dimensions: ImageDimensions,
    options: &ImageRenderOptions,
) -> Option<RenderImageResult> {
    let protocol = get_capabilities().images?;
    let max_width = options.max_width_cells.unwrap_or(80);
    let size = calculate_image_cell_size(
        image_dimensions,
        max_width,
        options.max_height_cells,
        get_cell_dimensions(),
    );

    match protocol {
        ImageProtocol::Kitty => {
            if let Some(image_id) = options.image_id {
                register_kitty_image_metadata(KittyImageMetadata {
                    image_id,
                    columns: size.columns,
                    rows: size.rows,
                    width_px: image_dimensions.width_px,
                    height_px: image_dimensions.height_px,
                });
            }
            let sequence = encode_kitty(
                base64_data,
                &KittyEncodeOptions {
                    columns: Some(size.columns),
                    rows: Some(size.rows),
                    image_id: options.image_id,
                    move_cursor: options.move_cursor,
                },
            );
            Some(RenderImageResult {
                sequence,
                columns: size.columns,
                rows: size.rows,
                image_id: options.image_id,
            })
        }
        ImageProtocol::Iterm2 => {
            let sequence = encode_iterm2(
                base64_data,
                &Iterm2EncodeOptions {
                    width: Some(size.columns.to_string()),
                    height: Some("auto".to_string()),
                    name: None,
                    preserve_aspect_ratio: Some(options.preserve_aspect_ratio.unwrap_or(true)),
                    inline: None,
                },
            );
            Some(RenderImageResult {
                sequence,
                columns: size.columns,
                rows: size.rows,
                image_id: None,
            })
        }
    }
}

/// Whether `path` is absolute in the POSIX sense.
///
/// Upstream uses Node's `path.isAbsolute`, which also accepts Windows forms.
/// This crate's callers only ever pass POSIX absolute paths, so `starts_with
/// ('/')` is the whole rule (documented divergence).
fn is_absolute_path(path: &str) -> bool {
    path.starts_with('/')
}

/// `$HOME`, or `None` when it is unset or empty.
fn home_dir() -> Option<String> {
    std::env::var("HOME").ok().filter(|home| !home.is_empty())
}

/// Shorten a home-prefixed absolute path to `~/...` for compact display.
///
/// Mirrors `shortenImagePath` (`terminal-image.ts:682-689`). `$HOME` stands in
/// for Node's `os.homedir()` — no `dirs`/`home` crate — and a path that is
/// neither `$HOME` itself nor below it is returned unchanged. An unset or empty
/// `$HOME` disables the shortening, matching upstream's falsy check.
pub fn shorten_image_path(filename: &str) -> String {
    let Some(home) = home_dir() else {
        return filename.to_string();
    };
    if filename == home {
        return "~".to_string();
    }
    // Upstream slices at `home.length`, so the separator stays part of `rest`.
    if let Some(rest) = filename.strip_prefix(&home) {
        if rest.starts_with('/') || rest.starts_with('\\') {
            return format!("~{rest}");
        }
    }
    filename.to_string()
}

/// Text fallback for a terminal that cannot render inline images.
///
/// Mirrors `imageFallback` (`terminal-image.ts:695-707`). The shape is
/// `[Image: <display> [<mime>] <W>x<H>]`, with the display name and the pixel
/// size omitted entirely when there is no filename / no parsed dimensions (an
/// empty filename counts as absent, as upstream's truthiness check does). An
/// absolute path is shortened with [`shorten_image_path`] and, when the
/// terminal renders OSC 8, wrapped in a `file://` hyperlink so the full path
/// stays openable.
///
/// Divergence: upstream builds the target with `pathToFileURL(...).href`, which
/// percent-escapes the path. This crate's callers only pass POSIX absolute
/// paths, so the target is `file://{path}` verbatim — no escaping — which keeps
/// the sequence readable for the paths the TUI actually hands over.
pub fn image_fallback(
    mime_type: &str,
    dimensions: Option<ImageDimensions>,
    filename: Option<&str>,
) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(filename) = filename.filter(|name| !name.is_empty()) {
        let display = shorten_image_path(filename);
        if is_absolute_path(filename) && get_capabilities().hyperlinks {
            parts.push(hyperlink::hyperlink(
                &display,
                &format!("file://{filename}"),
            ));
        } else {
            parts.push(display);
        }
    }
    parts.push(format!("[{mime_type}]"));
    if let Some(dimensions) = dimensions {
        parts.push(format!("{}x{}", dimensions.width_px, dimensions.height_px));
    }
    format!("[Image: {}]", parts.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_image_line_recognises_kitty_prefix() {
        let line = format!("{KITTY_PREFIX}q=2,");
        assert!(is_image_line(&line));
    }

    #[test]
    fn is_image_line_recognises_iterm2_prefix() {
        let line = format!("{ITERM2_PREFIX}name=test");
        assert!(is_image_line(&line));
    }

    #[test]
    fn is_image_line_recognises_substring_match_for_multirow() {
        // A multi-row image carries the sequence mid-line after a
        // cursor-up escape; `is_image_line` must still detect it.
        let line = format!("abc{KITTY_PREFIX}q=2,");
        assert!(is_image_line(&line));
    }

    #[test]
    fn is_image_line_rejects_plain_text() {
        assert!(!is_image_line("hello world"));
        assert!(!is_image_line(""));
    }

    #[test]
    fn allocate_image_id_is_monotonic() {
        // `allocate_image_id` uses a process-global atomic counter that
        // other tests in the suite also bump, so we can't assert a
        // strict ordering between any two consecutive calls. The
        // contract worth pinning here is "returns distinct IDs every
        // call".
        let a = allocate_image_id();
        let b = allocate_image_id();
        let c = allocate_image_id();
        assert_ne!(a, b);
        assert_ne!(b, c);
        assert_ne!(a, c);
    }

    #[test]
    fn decoded_base64_len_decodes_ascii() {
        // 4 base64 chars → 3 bytes.
        assert_eq!(decoded_base64_len("AAAA"), 3);
    }

    #[test]
    fn decoded_base64_len_counts_padding() {
        // "AA==" has 2 padding chars.
        assert_eq!(decoded_base64_len("AA=="), 1);
    }

    #[test]
    fn delete_kitty_image_emits_a_sequence_with_the_id() {
        let seq = delete_kitty_image(7);
        assert!(seq.contains("7"), "id must appear in the sequence: {seq}");
        // ESC _G ... q=2 ... DEL
        assert!(seq.contains("\u{1b}_G"));
    }

    #[test]
    fn delete_all_kitty_images_is_a_single_sequence() {
        let seq = delete_all_kitty_images();
        assert!(seq.contains("\u{1b}_G"));
        assert!(seq.contains("q=2"));
    }

    #[test]
    fn delete_all_kitty_placements_targets_only_placements() {
        let seq = delete_all_kitty_placements();
        assert!(seq.contains("\u{1b}_G"));
        // It is a quiet delete (q=2) targeted at placements.
        assert!(seq.contains("q=2"));
    }

    #[test]
    fn encode_kitty_short_payload_uses_single_chunk() {
        let opts = KittyEncodeOptions::default();
        let out = encode_kitty("AAAA", &opts);
        // Short payloads emit a single sequence with no `m=` marker.
        assert!(out.starts_with("\u{1b}_G"));
        assert!(out.contains("a=T"));
        assert!(out.contains("f=100"));
        assert!(out.contains("q=2"));
        assert!(out.ends_with("\u{1b}\\"));
        assert!(!out.contains(",m="), "got {out}");
    }

    #[test]
    fn encode_kitty_no_cursor_escape_emits_c_flag() {
        let opts = KittyEncodeOptions {
            move_cursor: Some(false),
            ..KittyEncodeOptions::default()
        };
        let out = encode_kitty("AAAA", &opts);
        assert!(out.contains("C=1"));
    }

    #[test]
    fn encode_kitty_columns_and_rows_appear_when_set() {
        let opts = KittyEncodeOptions {
            columns: Some(3),
            rows: Some(2),
            image_id: Some(42),
            ..KittyEncodeOptions::default()
        };
        let out = encode_kitty("AAAA", &opts);
        assert!(out.contains("c=3"));
        assert!(out.contains("r=2"));
        assert!(out.contains("i=42"));
    }

    #[test]
    fn encode_kitty_zero_dimensions_are_dropped() {
        let opts = KittyEncodeOptions {
            columns: Some(0),
            rows: Some(0),
            image_id: Some(0),
            ..KittyEncodeOptions::default()
        };
        let out = encode_kitty("AAAA", &opts);
        // Zero values should NOT produce their `c=`/`r=`/`i=` parameters.
        assert!(!out.contains("c=0"));
        assert!(!out.contains("r=0"));
        assert!(!out.contains("i=0"));
    }

    #[test]
    fn encode_kitty_long_payload_is_chunked_with_m_markers() {
        // Build a payload larger than KITTY_CHUNK_SIZE.
        let payload = "A".repeat(KITTY_CHUNK_SIZE * 3);
        let opts = KittyEncodeOptions::default();
        let out = encode_kitty(&payload, &opts);
        // Chunking should produce at least one `m=1` segment.
        assert!(out.contains(",m=1"));
        // And the final `m=0` terminator.
        assert!(out.contains("m=0"));
    }

    #[test]
    fn encode_iterm2_default_inline_is_one() {
        let opts = Iterm2EncodeOptions::default();
        let out = encode_iterm2("AAAA", &opts);
        assert!(out.starts_with("\u{1b}]1337;File="));
        assert!(out.contains("inline=1"));
        assert!(out.contains("size=3"));
    }

    #[test]
    fn encode_iterm2_explicit_inline_false_emits_zero() {
        let opts = Iterm2EncodeOptions {
            inline: Some(false),
            ..Iterm2EncodeOptions::default()
        };
        let out = encode_iterm2("AAAA", &opts);
        assert!(out.contains("inline=0"));
    }

    #[test]
    fn encode_iterm2_dimensions_are_optional() {
        let opts = Iterm2EncodeOptions {
            width: Some("10".into()),
            height: Some("auto".into()),
            ..Iterm2EncodeOptions::default()
        };
        let out = encode_iterm2("AAAA", &opts);
        assert!(out.contains("width=10"));
        assert!(out.contains("height=auto"));
    }

    #[test]
    fn encode_iterm2_empty_name_is_skipped() {
        let opts = Iterm2EncodeOptions {
            name: Some(String::new()),
            ..Iterm2EncodeOptions::default()
        };
        let out = encode_iterm2("AAAA", &opts);
        assert!(!out.contains("name="));
    }

    #[test]
    fn encode_iterm2_preserve_aspect_ratio_default_is_omitted() {
        // `None` and `Some(true)` both omit the parameter; only
        // `Some(false)` emits `preserveAspectRatio=0`.
        let default = Iterm2EncodeOptions::default();
        let out = encode_iterm2("AAAA", &default);
        assert!(!out.contains("preserveAspectRatio"));
        let yes = Iterm2EncodeOptions {
            preserve_aspect_ratio: Some(true),
            ..Iterm2EncodeOptions::default()
        };
        let out2 = encode_iterm2("AAAA", &yes);
        assert!(!out2.contains("preserveAspectRatio"));
        let no = Iterm2EncodeOptions {
            preserve_aspect_ratio: Some(false),
            ..Iterm2EncodeOptions::default()
        };
        let out3 = encode_iterm2("AAAA", &no);
        assert!(out3.contains("preserveAspectRatio=0"));
    }

    #[test]
    fn decoded_base64_len_zero_for_empty() {
        assert_eq!(decoded_base64_len(""), 0);
    }

    #[test]
    fn decoded_base64_len_handles_real_payload() {
        // "Hello" base64-encoded is "SGVsbG8=" which decodes to 5 bytes.
        assert_eq!(decoded_base64_len("SGVsbG8="), 5);
    }

    #[test]
    fn kitty_metadata_round_trip() {
        // Use a fresh id that won't collide with other tests'
        // registrations.
        let id = allocate_image_id();
        let meta = KittyImageMetadata {
            image_id: id,
            columns: 4,
            rows: 2,
            width_px: 80,
            height_px: 40,
        };
        register_kitty_image_metadata(meta);

        // Build a syntactically valid kitty line for the registered id.
        let line = format!("\x1b_Ga=T,f=100,i={id},q=2;AAAA\x1b\\");
        let got = get_kitty_image_metadata(&line);
        assert_eq!(got, Some(meta));
    }

    #[test]
    fn kitty_metadata_unknown_id_returns_none() {
        // An id we never registered returns None.
        let id = allocate_image_id();
        let line = format!("\x1b_Ga=T,f=100,i={id},q=2;AAAA\x1b\\");
        assert!(get_kitty_image_metadata(&line).is_none());
    }

    #[test]
    fn kitty_placement_returns_sequence_for_registered_image() {
        let id = allocate_image_id();
        let meta = KittyImageMetadata {
            image_id: id,
            columns: 2,
            rows: 1,
            width_px: 16,
            height_px: 8,
        };
        register_kitty_image_metadata(meta);
        let line = format!("\x1b_Ga=T,f=100,i={id},q=2;AAAA\x1b\\");
        let placement = get_kitty_image_placement(&line).expect("placement");
        assert_eq!(placement.image_id, id);
        assert_eq!(placement.estimated_decoded_bytes, (16 * 8 * 4) as u64);
        assert!(!placement.sequence.is_empty());
        assert!(!placement.replacement_line.is_empty());
    }

    #[test]
    fn crop_kitty_image_line_returns_text_for_non_kitty() {
        // Plain text passes through unchanged.
        assert_eq!(crop_kitty_image_line("hello", 1, 1), "hello");
    }
}
