//! Slice 2 of the terminal-image port: encoders, the kitty metadata registry
//! and `cropKittyImageLine` (upstream `terminal-image.ts:215-433`).
//!
//! Everything here is offline: payloads are inline base64 strings, no fixture
//! files and no child processes. The kitty registry is process-global, so every
//! test that registers metadata takes [`registry_guard`] — that serialises the
//! order-sensitive cases (re-registration, the 1001-entry eviction) and stops
//! one test's registrations from evicting another's between register and query.

use std::sync::{Mutex, MutexGuard, PoisonError};

use pi_tui::{
    crop_kitty_image_line, decoded_base64_len, delete_all_kitty_images,
    delete_all_kitty_placements, delete_kitty_image, encode_iterm2, encode_kitty,
    get_kitty_image_metadata, get_kitty_image_placement, register_kitty_image_metadata,
    Iterm2EncodeOptions, KittyEncodeOptions, KittyImageMetadata,
};

/// Serialises every test that touches the kitty metadata registry.
static REGISTRY_GUARD: Mutex<()> = Mutex::new(());

fn registry_guard() -> MutexGuard<'static, ()> {
    REGISTRY_GUARD
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

fn metadata(
    image_id: u32,
    columns: u32,
    rows: u32,
    width_px: u32,
    height_px: u32,
) -> KittyImageMetadata {
    KittyImageMetadata {
        image_id,
        columns,
        rows,
        width_px,
        height_px,
    }
}

// ---------------------------------------------------------------------------
// Group 1 — encoders
// ---------------------------------------------------------------------------

#[test]
fn encode_kitty_single_chunk_matches_upstream() {
    let options = KittyEncodeOptions {
        columns: Some(10),
        rows: Some(5),
        image_id: Some(7),
        move_cursor: Some(false),
    };
    assert_eq!(
        encode_kitty("AAAA", &options),
        "\x1b_Ga=T,f=100,q=2,C=1,c=10,r=5,i=7;AAAA\x1b\\"
    );

    // `moveCursor` defaults to true, so anything but an explicit `false` stays out.
    let defaults = KittyEncodeOptions {
        move_cursor: Some(true),
        ..KittyEncodeOptions::default()
    };
    assert_eq!(
        encode_kitty("AAAA", &defaults),
        "\x1b_Ga=T,f=100,q=2;AAAA\x1b\\"
    );
    assert_eq!(
        encode_kitty("AAAA", &KittyEncodeOptions::default()),
        "\x1b_Ga=T,f=100,q=2;AAAA\x1b\\"
    );

    // Upstream's truthiness checks skip zero-valued options.
    let zeros = KittyEncodeOptions {
        columns: Some(0),
        rows: Some(0),
        image_id: Some(0),
        move_cursor: None,
    };
    assert_eq!(
        encode_kitty("AAAA", &zeros),
        "\x1b_Ga=T,f=100,q=2;AAAA\x1b\\"
    );
}

#[test]
fn encode_kitty_chunks_long_payloads_by_byte() {
    // 2 * 4096 + 10 bytes → first chunk, one middle chunk, last chunk.
    let data = "A".repeat(2 * 4096 + 10);
    let encoded = encode_kitty(&data, &KittyEncodeOptions::default());

    let first = format!("\x1b_Ga=T,f=100,q=2,m=1;{}\x1b\\", "A".repeat(4096));
    let middle = format!("\x1b_Gm=1;{}\x1b\\", "A".repeat(4096));
    let last = format!("\x1b_Gm=0;{}\x1b\\", "A".repeat(10));
    assert_eq!(encoded, format!("{first}{middle}{last}"));
    assert_eq!(encoded.matches("\x1b\\").count(), 3);

    // The concatenation is reversible at the chunk level: the payload pieces
    // re-join to the original.
    let rejoined: String = encoded
        .split("\x1b\\")
        .filter(|segment| !segment.is_empty())
        .map(|segment| segment.rsplit(';').next().unwrap_or("").to_string())
        .collect();
    assert_eq!(rejoined, data);

    // Exactly CHUNK_SIZE still goes out as one command.
    let exact = "B".repeat(4096);
    let single = encode_kitty(&exact, &KittyEncodeOptions::default());
    assert_eq!(single.matches("\x1b\\").count(), 1);
    assert_eq!(single, format!("\x1b_Ga=T,f=100,q=2;{exact}\x1b\\"));
}

#[test]
fn delete_commands_are_the_pinned_sequences() {
    assert_eq!(delete_kitty_image(42), "\x1b_Ga=d,d=I,i=42,q=2\x1b\\");
    // Uppercase A frees the image data as well…
    assert_eq!(delete_all_kitty_images(), "\x1b_Ga=d,d=A,q=2\x1b\\");
    // …lowercase a only drops the placements.
    assert_eq!(delete_all_kitty_placements(), "\x1b_Ga=d,d=a,q=2\x1b\\");
}

#[test]
fn decoded_base64_len_counts_padding() {
    assert_eq!(decoded_base64_len(""), 0);
    assert_eq!(decoded_base64_len("Zg=="), 1);
    assert_eq!(decoded_base64_len("Zm8="), 2);
    assert_eq!(decoded_base64_len("Zm9v"), 3);
    assert_eq!(decoded_base64_len("aGVsbG8gd29ybGQh"), 12);
}

#[test]
fn encode_iterm2_matches_upstream() {
    // base64 of "hello world!" (12 decoded bytes).
    let payload = "aGVsbG8gd29ybGQh";
    let full = Iterm2EncodeOptions {
        width: Some("auto".to_string()),
        height: Some("5".to_string()),
        name: Some("test.png".to_string()),
        preserve_aspect_ratio: Some(false),
        inline: Some(false),
    };
    assert_eq!(
        encode_iterm2(payload, &full),
        "\x1b]1337;File=inline=0;size=12;width=auto;height=5;name=dGVzdC5wbmc=;preserveAspectRatio=0:aGVsbG8gd29ybGQh\x07"
    );

    // Defaults: inline, no preserveAspectRatio, no optional params.
    assert_eq!(
        encode_iterm2(payload, &Iterm2EncodeOptions::default()),
        "\x1b]1337;File=inline=1;size=12:aGVsbG8gd29ybGQh\x07"
    );

    // preserveAspectRatio only appears for an explicit `false`.
    let keep = Iterm2EncodeOptions {
        preserve_aspect_ratio: Some(true),
        ..Iterm2EncodeOptions::default()
    };
    assert_eq!(
        encode_iterm2(payload, &keep),
        "\x1b]1337;File=inline=1;size=12:aGVsbG8gd29ybGQh\x07"
    );

    // An empty name is falsy upstream and emits nothing.
    let empty_name = Iterm2EncodeOptions {
        name: Some(String::new()),
        ..Iterm2EncodeOptions::default()
    };
    assert_eq!(
        encode_iterm2(payload, &empty_name),
        "\x1b]1337;File=inline=1;size=12:aGVsbG8gd29ybGQh\x07"
    );
}

// ---------------------------------------------------------------------------
// Group 2 — metadata registry + placement
// ---------------------------------------------------------------------------

#[test]
fn metadata_round_trips_and_reregistration_bumps_generation() {
    let _guard = registry_guard();
    let entry = metadata(4242, 10, 6, 120, 120);
    register_kitty_image_metadata(entry);

    let line = "\x1b_Ga=T,f=100,q=2,i=4242,c=10,r=6;AAAA\x1b\\";
    assert_eq!(get_kitty_image_metadata(line), Some(entry));

    let first = get_kitty_image_placement(line).expect("placement after first registration");
    register_kitty_image_metadata(entry);
    let second = get_kitty_image_placement(line).expect("placement after re-registration");
    assert!(
        second.transmission_generation > first.transmission_generation,
        "re-registration must move the id to the back of the generation order"
    );

    // The documented public record hides the generation.
    let public = get_kitty_image_metadata(line).expect("metadata present");
    assert_eq!(public.image_id, 4242);
    assert_eq!(public.columns, 10);
    assert_eq!(public.rows, 6);

    // A line without `i=`, or with an unregistered id, resolves to nothing.
    assert_eq!(
        get_kitty_image_metadata("\x1b_Ga=T,f=100,q=2;AAAA\x1b\\"),
        None
    );
    assert_eq!(
        get_kitty_image_metadata("\x1b_Ga=T,f=100,q=2,i=999999;AAAA\x1b\\"),
        None
    );
}

#[test]
fn registry_evicts_the_oldest_entry_past_1000() {
    let _guard = registry_guard();
    for id in 100_000..101_001 {
        register_kitty_image_metadata(metadata(id, 1, 1, 1, 1));
    }

    let oldest = "\x1b_Ga=T,f=100,q=2,i=100000;AAAA\x1b\\";
    let newest = "\x1b_Ga=T,f=100,q=2,i=101000;AAAA\x1b\\";
    assert_eq!(
        get_kitty_image_metadata(oldest),
        None,
        "oldest must be evicted"
    );
    assert_eq!(
        get_kitty_image_metadata(newest),
        Some(metadata(101000, 1, 1, 1, 1))
    );
}

#[test]
fn placement_of_single_and_multi_chunk_lines() {
    let _guard = registry_guard();
    register_kitty_image_metadata(metadata(77, 10, 5, 40, 30));

    let single = "\x1b_Ga=T,f=100,q=2,i=77,c=10,r=5;AAAA\x1b\\";
    let placement = get_kitty_image_placement(single).expect("single-chunk placement");
    assert_eq!(placement.image_id, 77);
    // Only placement-relevant control keys survive, in their original order;
    // `a=T`, `f=100` and `q=2` are dropped.
    assert_eq!(placement.sequence, "\x1b_Ga=p,q=2,i=77,c=10,r=5\x1b\\");
    assert_eq!(placement.transmission_bytes, single.len());
    assert_eq!(placement.estimated_decoded_bytes, 40 * 30 * 4);
    assert_eq!(placement.replacement_line, placement.sequence);

    // Multi-chunk (`m=1` continuation): the transmission spans both commands, and
    // `m` is not a placement control key.
    register_kitty_image_metadata(metadata(88, 8, 4, 40, 30));
    let multi = "\x1b_Ga=T,f=100,q=2,i=88,m=1;AAAA\x1b\\\x1b_Gm=0;BBBB\x1b\\";
    let placement = get_kitty_image_placement(multi).expect("multi-chunk placement");
    assert_eq!(placement.sequence, "\x1b_Ga=p,q=2,i=88\x1b\\");
    assert_eq!(placement.transmission_bytes, multi.len());
    assert_eq!(placement.replacement_line, placement.sequence);

    // A dangling continuation with no terminator yields nothing.
    let truncated = "\x1b_Ga=T,f=100,q=2,i=88,m=1;AAAA\x1b\\";
    assert_eq!(get_kitty_image_placement(truncated), None);
}

// ---------------------------------------------------------------------------
// Group 3 — cropping
// ---------------------------------------------------------------------------

#[test]
fn crop_returns_the_line_untouched_when_nothing_is_hidden() {
    let _guard = registry_guard();
    register_kitty_image_metadata(metadata(555, 10, 6, 120, 120));
    let line = "\x1b_Ga=T,f=100,q=2,i=555,c=10,r=6;AAAA\x1b\\";

    assert_eq!(crop_kitty_image_line(line, 0, 6), line);
    // visibleRows beyond the image still covers the whole image.
    assert_eq!(crop_kitty_image_line(line, 0, 1000), line);
    // Out of range / nothing visible.
    assert_eq!(crop_kitty_image_line(line, 6, 3), line);
    assert_eq!(crop_kitty_image_line(line, 7, 3), line);
    assert_eq!(crop_kitty_image_line(line, 0, 0), line);
    assert_eq!(crop_kitty_image_line(line, 2, 0), line);
    // Unregistered image: untouched.
    assert_eq!(
        crop_kitty_image_line("\x1b_Ga=T,f=100,q=2,i=999999;AAAA\x1b\\", 2, 3),
        "\x1b_Ga=T,f=100,q=2,i=999999;AAAA\x1b\\"
    );
}

#[test]
fn crop_computes_the_pixel_window_with_floor_and_ceil() {
    let _guard = registry_guard();
    register_kitty_image_metadata(metadata(555, 10, 6, 120, 120));
    let line = "\x1b_Ga=T,f=100,q=2,i=555,c=10,r=6;AAAA\x1b\\";

    // hiddenRows=2, rows=6, heightPx=120, visibleRows=3 → croppedRows=3,
    // sourceY=floor(120*2/6)=40, sourceEnd=ceil(120*5/6)=100, sourceHeight=60.
    assert_eq!(
        crop_kitty_image_line(line, 2, 3),
        "\x1b_Ga=T,f=100,q=2,i=555,c=10,y=40,h=60,r=3;AAAA\x1b\\"
    );

    // Rounding to a non-integral cell: rows=3, heightPx=10, hiddenRows=1,
    // visibleRows=1 → sourceY=floor(10/3)=3, sourceEnd=ceil(10*2/3)=7,
    // sourceHeight=7-3=4.
    register_kitty_image_metadata(metadata(556, 4, 3, 8, 10));
    let fractional = "\x1b_Ga=T,f=100,q=2,i=556,c=4,r=3;BBBB\x1b\\";
    assert_eq!(
        crop_kitty_image_line(fractional, 1, 1),
        "\x1b_Ga=T,f=100,q=2,i=556,c=4,y=3,h=4,r=1;BBBB\x1b\\"
    );

    // At least one pixel row is emitted even when the window collapses.
    register_kitty_image_metadata(metadata(557, 1, 1000, 1, 1));
    let tiny = "\x1b_Ga=T,f=100,q=2,i=557,c=1,r=1000;CCCC\x1b\\";
    let cropped = crop_kitty_image_line(tiny, 1, 1);
    assert!(
        cropped.contains("h=1"),
        "sourceHeight floors at 1: {cropped}"
    );
}
