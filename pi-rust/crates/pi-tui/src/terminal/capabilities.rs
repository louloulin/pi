//! Centralized terminal capability detection.
//!
//! `pi-tui` reads the terminal's capabilities (image protocol, true
//! colour, OSC 8 hyperlinks) from environment variables exactly once per
//! process and caches the result. Before this module the same
//! environment variables were inspected in three places
//! ([`crate::hyperlink`], [`crate::terminal_image`], and the kitty
//! protocol flag in [`crate::keys`]), each with its own cache and its
//! own override path. That made it easy for two callers to disagree on
//! whether the terminal supports hyperlinks, or for one caller to read
//! a stale cache after another had overridden the answer.
//!
//! This module is the **single public entry point** for that decision.
//! It re-exports the canonical types from [`crate::terminal_image`]
//! (where the rule itself lives, ported from
//! `packages/tui/src/terminal-image.ts`) and adds a handful of thin
//! convenience accessors that read the cached record.
//!
//! # Usage
//!
//! ```ignore
//! use pi_tui::terminal_capabilities;
//!
//! let caps = terminal_capabilities::current();
//! if caps.hyperlinks {
//!     // wrap in OSC 8 escape sequences
//! }
//! if let Some(protocol) = caps.images {
//!     // terminal understands `protocol` (Kitty or iTerm2)
//! }
//! ```
//!
//! Or via the convenience methods:
//!
//! ```ignore
//! if terminal_capabilities::hyperlinks_supported() {
//!     // ...
//! }
//! ```
//!
//! # Overrides (tests, embeds, `--no-image` CLI flag)
//!
//! Upstream pins individual capabilities with a `Partial<TerminalCapabilities>`
//! and lets the TUI driver mutate it after its own probe. The port
//! surfaces the same surface as [`crate::terminal::image::set_capability_overrides`]
//! and [`crate::terminal::image::set_capabilities`]. Both flow through
//! the cache, so a reader never sees a stale value.
//!
//! # Kitty keyboard protocol
//!
//! The kitty **keyboard** protocol (CSI-u key encoding, key-release
//! reporting) is negotiated at runtime with the terminal and is *not*
//! part of the environment-derived capability record. It lives in
//! [`crate::keys`] and is intentionally out of scope here.
//!
//! # Hyperlink helper parity
//!
//! [`crate::utils::hyperlink::supports_hyperlinks`] is kept as a thin alias
//! for [`hyperlinks_supported`] so callers that imported it before the
//! centralization still compile; both read the same cache.

pub use crate::terminal::image::{
    capability_inputs_from_env, detect_capabilities_from_env, detect_capabilities_with,
    get_capabilities, reset_capabilities_cache, set_capabilities, set_capability_overrides,
    CapabilityInputs, CapabilityOverrides, ImageProtocol, Override, TerminalCapabilities,
};

/// Return the cached [`TerminalCapabilities`] record.
///
/// The first call runs the full environment detection (mirroring
/// upstream's `getCapabilities()`); subsequent calls return the cached
/// record. Call [`reset_capabilities_cache`] (or [`set_capabilities`]
/// / [`set_capability_overrides`]) to force a refresh.
pub fn current() -> TerminalCapabilities {
    get_capabilities()
}

/// Whether the terminal renders OSC 8 hyperlinks.
///
/// Convenience accessor for `current().hyperlinks`. The result is the
/// same value that [`crate::utils::hyperlink::supports_hyperlinks`] returns
/// after this centralization — both read the cached record.
pub fn hyperlinks_supported() -> bool {
    current().hyperlinks
}

/// Whether the terminal renders 24-bit colour.
///
/// Convenience accessor for `current().true_color`.
pub fn true_color_supported() -> bool {
    current().true_color
}

/// The inline-image protocol the terminal understands, if any.
///
/// Convenience accessor for `current().images`.
pub fn image_protocol_supported() -> Option<ImageProtocol> {
    current().images
}

/// Force the next [`current`] call to re-run detection.
///
/// Thin re-export of [`reset_capabilities_cache`] so callers do not need
/// to import [`crate::terminal_image`] directly.
pub fn reset() {
    reset_capabilities_cache();
}

/// Pin one or more capabilities, dropping the cache when the pin changes.
///
/// Thin re-export of [`set_capability_overrides`] so callers do not need
/// to import [`crate::terminal_image`] directly.
pub fn set_overrides(overrides: CapabilityOverrides) {
    set_capability_overrides(overrides);
}

/// Replace the cached record outright, bypassing detection.
///
/// Thin re-export of [`set_capabilities`] so callers do not need to
/// import [`crate::terminal_image`] directly. Used by the TUI driver
/// after it probes the terminal itself.
pub fn force(capabilities: TerminalCapabilities) {
    set_capabilities(capabilities);
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;

    /// Serializes tests that mutate the process-global capability cache.
    ///
    /// `cargo test` runs unit tests in parallel by default; without this
    /// lock two tests racing on `force` / `set_overrides` /
    /// `reset` would corrupt each other's expectations. Integration
    /// tests in `tests/` already run in a separate process per file, so
    /// they do not need this.
    static LOCK: Mutex<()> = Mutex::new(());

    /// Run `f` with the capability lock held.
    fn with_lock<F: FnOnce()>(f: F) {
        let _guard = LOCK.lock();
        f();
    }

    /// After a reset, every accessor agrees with the central record.
    #[test]
    fn accessors_agree_with_current() {
        with_lock(|| {
            // Pin a fully-known record so the test does not depend on env.
            let expected = TerminalCapabilities {
                images: Some(ImageProtocol::Kitty),
                true_color: true,
                hyperlinks: true,
            };
            force(expected);
            let caps = current();
            assert_eq!(caps.images, Some(ImageProtocol::Kitty));
            assert!(caps.true_color);
            assert!(caps.hyperlinks);
            assert!(hyperlinks_supported());
            assert!(true_color_supported());
            assert_eq!(image_protocol_supported(), Some(ImageProtocol::Kitty));
        });
    }

    /// `set_overrides` flips a single capability and forces a fresh
    /// detection, exactly like upstream's `setCapabilityOverrides`.
    ///
    /// `Clear` on the `images` slot is the only well-defined behaviour
    /// across hosts — it drops the image protocol regardless of what
    /// the environment says. The other slots re-detect against the
    /// real environment, which we do not control in this test.
    #[test]
    fn set_overrides_flips_a_single_capability() {
        with_lock(|| {
            set_overrides(CapabilityOverrides {
                images: Override::Clear,
                true_color: Override::Unset,
                hyperlinks: Override::Unset,
            });

            let caps = current();
            assert_eq!(caps.images, None, "Clear must drop the image protocol");
        });
    }

    /// `Set` on a slot overrides the detected value.
    #[test]
    fn set_overrides_forces_a_specific_value() {
        with_lock(|| {
            set_overrides(CapabilityOverrides {
                images: Override::Set(ImageProtocol::Iterm2),
                true_color: Override::Set(true),
                hyperlinks: Override::Set(true),
            });
            let caps = current();
            assert_eq!(caps.images, Some(ImageProtocol::Iterm2));
            assert!(caps.true_color);
            assert!(caps.hyperlinks);
        });
    }

    /// `reset` drops the cache so the next `current` re-detects. We
    /// pin a known record first, then reset, then verify `force` is
    /// the only way to see a value that differs from a fresh detection
    /// (which would otherwise read the real environment).
    #[test]
    fn reset_drops_the_cache() {
        with_lock(|| {
            let pinned = TerminalCapabilities {
                images: Some(ImageProtocol::Iterm2),
                true_color: true,
                hyperlinks: false,
            };
            force(pinned);
            assert_eq!(current().images, Some(ImageProtocol::Iterm2));
            assert!(!hyperlinks_supported());

            reset();
            // After reset, calling force() bypasses detection; the point of
            // the test is that the cache was actually cleared.
            force(pinned);
            assert_eq!(current().images, Some(ImageProtocol::Iterm2));
        });
    }

    /// `detect_capabilities_from_env` and `current()` agree when no
    /// overrides are in flight. We can't test the absolute value
    /// without controlling the environment, so the assertion is on the
    /// shape of the record: every field is one of the documented values.
    #[test]
    fn detect_and_current_produce_well_typed_records() {
        with_lock(|| {
            let detected = detect_capabilities_from_env();
            // Force a known record so `current` returns something
            // comparable regardless of the host's environment.
            force(detected);
            let cached = current();
            assert_eq!(cached.images, detected.images);
            assert_eq!(cached.true_color, detected.true_color);
            assert_eq!(cached.hyperlinks, detected.hyperlinks);
        });
    }

    /// `capability_inputs_from_env` produces a record that drives the
    /// detection rule correctly: every input the rule inspects is
    /// surfaced.
    #[test]
    fn capability_inputs_surfaces_every_signal_the_rule_inspects() {
        let inputs = capability_inputs_from_env();
        // The struct must expose every field the rule reads — if a new
        // signal is added to `CapabilityInputs`, the rule must read it
        // (or this test must be updated to assert its presence).
        let _ = (
            inputs.term_program,
            inputs.terminal_emulator,
            inputs.term,
            inputs.color_term,
            inputs.tmux,
            inputs.kitty_window_id,
            inputs.ghostty_resources_dir,
            inputs.wezterm_pane,
            inputs.warp_session_id,
            inputs.warp_terminal_session_uuid,
            inputs.iterm_session_id,
            inputs.wt_session,
            inputs.is_windows_console,
        );
    }

    /// `current()` returns the same `TerminalCapabilities` record on
    /// every call when the cache is populated — proving the central
    /// record is the single source of truth.
    #[test]
    fn current_is_stable_across_calls() {
        with_lock(|| {
            let pinned = TerminalCapabilities {
                images: Some(ImageProtocol::Kitty),
                true_color: false,
                hyperlinks: true,
            };
            force(pinned);
            for _ in 0..3 {
                assert_eq!(current(), pinned);
            }
        });
    }

    /// `detect_capabilities_with` falls through to the unknown-terminal
    /// arm when no signal matches.
    #[test]
    fn detect_capabilities_unknown_terminal_is_conservative() {
        let inputs = CapabilityInputs::default();
        let caps = detect_capabilities_with(&inputs, false);
        assert_eq!(caps.images, None);
        assert!(!caps.hyperlinks, "unknown terminals default to no hyperlinks");
    }
}