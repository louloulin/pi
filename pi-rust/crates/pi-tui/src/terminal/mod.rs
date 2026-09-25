//! Backend layer — ratatui + crossterm surface and the protocols that
//! ride on top.
//!
//! The split mirrors the TS `packages/tui/src/tui.ts` + `terminal.ts`
//! pair: a [`ProcessTerminal`] owns the alternate-screen lifecycle and
//! the input loop, and the protocol helpers ([`capabilities`],
//! [`colors`], [`image`], [`title`], [`stdin_buffer`],
//! [`frame_pacer`]) sit beside it.
//!
//! | Module | Concern |
//! |--------|---------|
//! | [`process`] | `ProcessTerminal` + `Terminal` trait + `TerminalError` |
//! | [`capabilities`] | Background / OSC / kitty keyboard / image support probes |
//! | [`colors`] | OSC 11 / OSC 10 background / foreground detection |
//! | [`image`] | Kitty / iTerm2 inline image protocol |
//! | [`title`] | OSC 0 / 2 window title sequences |
//! | [`raw_tty`] | ProcessTerminal platform path on Unix |
//! | [`stdin_buffer`] | Async stdin reader the test harness drives |
//! | [`frame_pacer`] | Per-frame paint scheduling |

pub mod capabilities;
pub mod colors;
pub mod frame_pacer;
pub mod image;
pub mod process;
pub mod raw_tty;
pub mod stdin_buffer;
pub mod title;

pub use crate::terminal::capabilities::{
    current, hyperlinks_supported, image_protocol_supported, reset, set_overrides,
    true_color_supported,
};
pub use crate::terminal::colors::{parse_osc11_background_color, parse_terminal_color_scheme_report, TerminalColorSchemeReport as TerminalColorScheme};
pub use crate::terminal::frame_pacer::FramePacer;
pub use crate::terminal::image::{
    allocate_image_id, apply_env_overrides, calculate_image_cell_size, calculate_image_rows,
    capability_inputs_from_env, crop_kitty_image_line, decoded_base64_len,
    delete_all_kitty_images, delete_all_kitty_placements, delete_kitty_image,
    detect_capabilities_from_env, detect_capabilities_with, encode_iterm2, encode_kitty,
    get_capabilities, get_cell_dimensions, get_gif_dimensions, get_image_dimensions,
    get_jpeg_dimensions, get_kitty_image_metadata, get_kitty_image_placement,
    get_png_dimensions, get_webp_dimensions, image_fallback, is_image_line,
    register_kitty_image_metadata, render_image, reset_capabilities_cache, set_capabilities,
    set_capability_overrides, set_cell_dimensions, shorten_image_path, CapabilityInputs,
    CapabilityOverrides, CellDimensions, ImageCellSize, ImageDimensions, ImageProtocol,
    ImageRenderOptions, Iterm2EncodeOptions, KittyEncodeOptions, KittyImageMetadata,
    KittyImagePlacement, Override, RenderImageResult, TerminalCapabilities, ITERM2_PREFIX,
    KITTY_CHUNK_SIZE, KITTY_PREFIX,
};
pub use crate::terminal::process::{ProcessTerminal, Terminal, TerminalError};
pub use crate::terminal::raw_tty::{flush_pending_to_stderr, is_raw_mode, note, pending_len, set_raw_mode};
pub use crate::terminal::stdin_buffer::{
    StdinBuffer, StdinBufferEventMap, StdinBufferOptions,
};
pub use crate::terminal::title::{
    auto_title, path_basename, sanitize_title, title_sequence, TITLE_CLOSE, TITLE_OPEN,
};