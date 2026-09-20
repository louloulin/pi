//! Terminal image kernel — slice 1 of the port of
//! `packages/tui/src/terminal-image.ts`.
//!
//! This module currently covers the *capability layer* of the upstream file
//! (`terminal-image.ts:6-211`): the [`TerminalCapabilities`] record and its
//! environment detection, the resettable capability cache, the cell-pixel
//! dimensions, the kitty/iTerm2 line prefixes, [`is_image_line`] and
//! [`allocate_image_id`]. The encoders, kitty metadata/cropping, geometry, the
//! four pixel-parsers, `renderImage` and `imageFallback` land in the following
//! slices (see `FEATURE_PI_RS_STATUS.md`).
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

use crate::hyperlink;

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
            params.push(format!("name={}", crate::clipboard::base64_encode(name.as_bytes())));
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
    kitty_metadata_table().lock().entries.get(&image_id).copied()
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
    let source_end = (height_px * (u64::from(hidden_rows) + u64::from(cropped_rows))).div_ceil(rows);
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
