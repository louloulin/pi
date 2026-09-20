//! Slice 3 of the terminal-image port: geometry, the four pixel-size parsers,
//! `render_image` and `image_fallback` (upstream `terminal-image.ts:435-696`).
//!
//! Everything here is offline and fixture-free — each pixel-format sample is an
//! inline base64 string of a few dozen bytes. The capability cache and `$HOME`
//! are process-global, so every test that touches them lives in the single
//! `render_fallback_and_home_are_sequenced` case: no other test in this binary
//! reads either value, and a `#[test]` runs in its own process.

use pi_tui::{
    calculate_image_cell_size, calculate_image_rows, decoded_base64_len, get_gif_dimensions,
    get_image_dimensions, get_jpeg_dimensions, get_kitty_image_metadata, get_png_dimensions,
    get_webp_dimensions, hyperlink, image_fallback, render_image, set_capabilities,
    shorten_image_path, CellDimensions, ImageCellSize, ImageDimensions, ImageProtocol,
    ImageRenderOptions, KittyImageMetadata, TerminalCapabilities,
};

/// PNG: 8-byte signature, IHDR chunk header, 320x240 (24 bytes decoded).
const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAUAAAADw";
/// GIF89a with a 320x240 logical screen descriptor (10 bytes decoded).
const GIF: &str = "R0lGODlhQAHwAA==";
/// JPEG: SOI followed immediately by a SOF0 carrying 320x240 (21 bytes).
const JPEG: &str = "/9j/wAARCADwAUADAREAAhEBAxEB";
/// WebP, lossy `VP8 ` container (30 bytes decoded).
const WEBP_VP8: &str = "UklGRgAAAABXRUJQVlA4IAAAAAAAAACdASpAAfAA";
/// WebP, lossless `VP8L` container (30 bytes decoded).
const WEBP_VP8L: &str = "UklGRgAAAABXRUJQVlA4TAAAAAAvP8E7AAAAAAAA";
/// WebP, extended `VP8X` container (30 bytes decoded).
const WEBP_VP8X: &str = "UklGRgAAAABXRUJQVlA4WAAAAAAAAAAAPwEA7wAA";

/// The size every inline sample above carries.
const SAMPLE_SIZE: ImageDimensions = ImageDimensions {
    width_px: 320,
    height_px: 240,
};

fn cell_size(
    width_px: u32,
    height_px: u32,
    max_width_cells: u32,
    max_height_cells: Option<u32>,
    cell_dimensions: CellDimensions,
) -> ImageCellSize {
    calculate_image_cell_size(
        ImageDimensions {
            width_px,
            height_px,
        },
        max_width_cells,
        max_height_cells,
        cell_dimensions,
    )
}

// ---------------------------------------------------------------------------
// Group 1 — geometry
// ---------------------------------------------------------------------------

#[test]
fn geometry_clamps_rounds_up_and_honours_both_budgets() {
    let default_cells = CellDimensions::default();

    // `Math.max(1, floor(x))` clamps a zero budget up to one cell.
    assert_eq!(
        cell_size(320, 240, 0, None, default_cells),
        ImageCellSize {
            columns: 1,
            rows: 1
        }
    );
    assert_eq!(
        cell_size(320, 240, 1, None, default_cells),
        ImageCellSize {
            columns: 1,
            rows: 1
        }
    );

    // Extreme aspect ratio: the width budget binds, the height collapses to 1.
    assert_eq!(
        cell_size(1000, 10, 80, None, default_cells),
        ImageCellSize {
            columns: 80,
            rows: 1
        }
    );

    // Both budgets given: the tighter one (height) wins.
    assert_eq!(
        cell_size(100, 100, 80, Some(10), default_cells),
        ImageCellSize {
            columns: 20,
            rows: 10
        }
    );
    assert_eq!(
        cell_size(320, 240, 80, Some(10), default_cells),
        ImageCellSize {
            columns: 27,
            rows: 10
        }
    );

    // Non-square cells (10x20): rows come out of the cell height, not the width.
    assert_eq!(
        calculate_image_rows(
            ImageDimensions {
                width_px: 100,
                height_px: 100
            },
            40,
            CellDimensions {
                width_px: 10,
                height_px: 20
            },
        ),
        20
    );
    assert_eq!(
        cell_size(
            300,
            7,
            10,
            None,
            CellDimensions {
                width_px: 10,
                height_px: 20
            }
        ),
        ImageCellSize {
            columns: 10,
            rows: 1
        }
    );
    assert_eq!(
        cell_size(
            100,
            100,
            5,
            Some(3),
            CellDimensions {
                width_px: 12,
                height_px: 24
            }
        ),
        ImageCellSize {
            columns: 5,
            rows: 3
        }
    );

    // A zero-sized image is treated as 1x1, exactly like upstream.
    assert_eq!(
        cell_size(0, 0, 10, None, default_cells),
        ImageCellSize {
            columns: 10,
            rows: 5
        }
    );

    // A realistic 16:9 still, rounded up, not truncated.
    assert_eq!(
        cell_size(1920, 1080, 80, None, default_cells),
        ImageCellSize {
            columns: 80,
            rows: 23
        }
    );
}

// ---------------------------------------------------------------------------
// Group 2 — the four pixel-size parsers, all from inline headers
// ---------------------------------------------------------------------------

#[test]
fn pixel_dimensions_come_from_the_inline_headers() {
    assert_eq!(get_png_dimensions(PNG), Some(SAMPLE_SIZE));
    assert_eq!(get_gif_dimensions(GIF), Some(SAMPLE_SIZE));
    assert_eq!(get_jpeg_dimensions(JPEG), Some(SAMPLE_SIZE));
    assert_eq!(get_webp_dimensions(WEBP_VP8), Some(SAMPLE_SIZE));
    assert_eq!(get_webp_dimensions(WEBP_VP8L), Some(SAMPLE_SIZE));
    assert_eq!(get_webp_dimensions(WEBP_VP8X), Some(SAMPLE_SIZE));

    // The MIME switch picks the matching parser.
    assert_eq!(get_image_dimensions(PNG, "image/png"), Some(SAMPLE_SIZE));
    assert_eq!(get_image_dimensions(GIF, "image/gif"), Some(SAMPLE_SIZE));
    assert_eq!(get_image_dimensions(JPEG, "image/jpeg"), Some(SAMPLE_SIZE));
    assert_eq!(
        get_image_dimensions(WEBP_VP8, "image/webp"),
        Some(SAMPLE_SIZE)
    );

    // A mismatched or unknown MIME type never guesses.
    assert_eq!(get_image_dimensions(PNG, "image/jpeg"), None);
    assert_eq!(get_image_dimensions(JPEG, "image/png"), None);
    assert_eq!(get_image_dimensions(PNG, "image/bmp"), None);
    assert_eq!(get_image_dimensions(PNG, ""), None);

    // Truncated headers are rejected rather than read out of bounds.
    assert_eq!(get_png_dimensions(&PNG[..16]), None);
    assert_eq!(get_gif_dimensions(&GIF[..8]), None);
    assert_eq!(get_jpeg_dimensions(&JPEG[..12]), None);
    assert_eq!(get_webp_dimensions(&WEBP_VP8[..28]), None);
    assert_eq!(get_image_dimensions("", "image/png"), None);

    // Right length, wrong magic.
    assert_eq!(get_png_dimensions(&"A".repeat(40)), None);
    assert_eq!(get_gif_dimensions(&"A".repeat(16)), None);
    assert_eq!(get_jpeg_dimensions(&"A".repeat(40)), None);
    assert_eq!(get_webp_dimensions(&"A".repeat(40)), None);
}

// ---------------------------------------------------------------------------
// Group 3 — rendering, the fallback and `$HOME`, all sequenced in one test
// ---------------------------------------------------------------------------

#[test]
fn render_fallback_and_home_are_sequenced() {
    let original_home = std::env::var_os("HOME");

    let kitty_caps = TerminalCapabilities {
        images: Some(ImageProtocol::Kitty),
        true_color: true,
        hyperlinks: true,
    };
    let iterm2_caps = TerminalCapabilities {
        images: Some(ImageProtocol::Iterm2),
        true_color: true,
        hyperlinks: true,
    };
    let no_images = TerminalCapabilities {
        images: None,
        true_color: true,
        hyperlinks: false,
    };

    // -- kitty, default options: `max_width_cells` defaults to 80 -------------
    set_capabilities(kitty_caps);
    let default_box =
        render_image(PNG, SAMPLE_SIZE, &ImageRenderOptions::default()).expect("kitty renders");
    assert_eq!((default_box.columns, default_box.rows), (80, 30));
    assert_eq!(default_box.image_id, None);
    assert_eq!(
        default_box.sequence,
        format!("\x1b_Ga=T,f=100,q=2,c=80,r=30;{PNG}\x1b\\")
    );
    // Without an id there is no metadata to look up.
    assert_eq!(get_kitty_image_metadata(&default_box.sequence), None);

    // -- kitty, explicit box, an id and `move_cursor: false` ------------------
    let sized = render_image(
        PNG,
        SAMPLE_SIZE,
        &ImageRenderOptions {
            max_width_cells: Some(20),
            image_id: Some(4242),
            move_cursor: Some(false),
            ..ImageRenderOptions::default()
        },
    )
    .expect("kitty renders");
    assert_eq!((sized.columns, sized.rows), (20, 8));
    assert_eq!(sized.image_id, Some(4242));
    assert_eq!(
        sized.sequence,
        format!("\x1b_Ga=T,f=100,q=2,C=1,c=20,r=8,i=4242;{PNG}\x1b\\")
    );
    // The id was registered before encoding, so the line resolves.
    assert_eq!(
        get_kitty_image_metadata(&sized.sequence),
        Some(KittyImageMetadata {
            image_id: 4242,
            columns: 20,
            rows: 8,
            width_px: SAMPLE_SIZE.width_px,
            height_px: SAMPLE_SIZE.height_px,
        })
    );

    // -- kitty, both budgets --------------------------------------------------
    let bounded = render_image(
        PNG,
        SAMPLE_SIZE,
        &ImageRenderOptions {
            max_width_cells: Some(80),
            max_height_cells: Some(10),
            ..ImageRenderOptions::default()
        },
    )
    .expect("kitty renders");
    assert_eq!((bounded.columns, bounded.rows), (27, 10));

    // -- iTerm2: the aspect ratio is delegated to the terminal ----------------
    set_capabilities(iterm2_caps);
    let iterm2_default =
        render_image(PNG, SAMPLE_SIZE, &ImageRenderOptions::default()).expect("iTerm2 renders");
    assert_eq!((iterm2_default.columns, iterm2_default.rows), (80, 30));
    assert_eq!(iterm2_default.image_id, None);
    assert_eq!(
        iterm2_default.sequence,
        format!(
            "\x1b]1337;File=inline=1;size={};width=80;height=auto:{PNG}\x07",
            decoded_base64_len(PNG)
        )
    );
    let iterm2_stretched = render_image(
        PNG,
        SAMPLE_SIZE,
        &ImageRenderOptions {
            preserve_aspect_ratio: Some(false),
            ..ImageRenderOptions::default()
        },
    )
    .expect("iTerm2 renders");
    assert!(iterm2_stretched.sequence.contains("preserveAspectRatio=0"));
    assert!(!iterm2_default.sequence.contains("preserveAspectRatio"));

    // -- no inline-image capability: render nothing ---------------------------
    set_capabilities(no_images);
    assert_eq!(
        render_image(PNG, SAMPLE_SIZE, &ImageRenderOptions::default()),
        None
    );

    // -- `image_fallback`: no hyperlinks --------------------------------------
    assert_eq!(
        image_fallback("image/png", Some(SAMPLE_SIZE), None),
        "[Image: [image/png] 320x240]"
    );
    std::env::set_var("HOME", "/home/tester");
    assert_eq!(
        image_fallback("image/png", None, Some("/tmp/pics/a.png")),
        "[Image: /tmp/pics/a.png [image/png]]"
    );
    assert_eq!(
        image_fallback(
            "image/png",
            Some(SAMPLE_SIZE),
            Some("/home/tester/pics/a.png")
        ),
        "[Image: ~/pics/a.png [image/png] 320x240]"
    );
    assert_eq!(
        image_fallback("image/png", Some(SAMPLE_SIZE), Some("relative/a.png")),
        "[Image: relative/a.png [image/png] 320x240]"
    );
    // An empty name counts as absent, like upstream's truthiness check.
    assert_eq!(
        image_fallback("image/jpeg", Some(SAMPLE_SIZE), Some("")),
        "[Image: [image/jpeg] 320x240]"
    );

    // -- `image_fallback`: OSC 8 links the shortened display name -------------
    set_capabilities(TerminalCapabilities {
        images: None,
        true_color: true,
        hyperlinks: true,
    });
    assert_eq!(
        image_fallback(
            "image/png",
            Some(SAMPLE_SIZE),
            Some("/home/tester/pics/a.png")
        ),
        format!(
            "[Image: {} [image/png] 320x240]",
            hyperlink("~/pics/a.png", "file:///home/tester/pics/a.png")
        )
    );
    // A relative path is not linkable, even with hyperlinks on.
    assert_eq!(
        image_fallback("image/png", None, Some("relative/a.png")),
        "[Image: relative/a.png [image/png]]"
    );

    // -- `shorten_image_path` ------------------------------------------------
    assert_eq!(shorten_image_path("/home/tester"), "~");
    assert_eq!(
        shorten_image_path("/home/tester/pics/a.png"),
        "~/pics/a.png"
    );
    assert_eq!(shorten_image_path("/tmp/pics/a.png"), "/tmp/pics/a.png");
    // A shared prefix is not a home match.
    assert_eq!(
        shorten_image_path("/home/tester2/a.png"),
        "/home/tester2/a.png"
    );
    std::env::remove_var("HOME");
    assert_eq!(
        shorten_image_path("/home/tester/a.png"),
        "/home/tester/a.png"
    );
    std::env::set_var("HOME", "");
    assert_eq!(
        shorten_image_path("/home/tester/a.png"),
        "/home/tester/a.png"
    );

    match original_home {
        Some(home) => std::env::set_var("HOME", home),
        None => std::env::remove_var("HOME"),
    }
}
