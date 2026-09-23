//! Capability-layer tests for `pi_tui::terminal_image` (LUM-1188 slice 1).
//!
//! Every detection branch is exercised through the pure
//! `detect_capabilities_with` rule, so no test mutates the process environment.

use pi_tui::terminal_image as ti;

fn inputs() -> ti::CapabilityInputs {
    ti::CapabilityInputs::default()
}

fn kitty() -> ti::TerminalCapabilities {
    ti::TerminalCapabilities {
        images: Some(ti::ImageProtocol::Kitty),
        true_color: true,
        hyperlinks: true,
    }
}

fn iterm2() -> ti::TerminalCapabilities {
    ti::TerminalCapabilities {
        images: Some(ti::ImageProtocol::Iterm2),
        true_color: true,
        hyperlinks: true,
    }
}

fn no_images(true_color: bool, hyperlinks: bool) -> ti::TerminalCapabilities {
    ti::TerminalCapabilities {
        images: None,
        true_color,
        hyperlinks,
    }
}

#[test]
fn tmux_reports_no_images_and_defers_hyperlinks_to_the_probe() {
    let with_tmux_var = ti::CapabilityInputs {
        tmux: true,
        term_program: Some("kitty".into()),
        ..inputs()
    };
    // Even a kitty `TERM_PROGRAM` must not be trusted through tmux.
    assert_eq!(
        ti::detect_capabilities_with(&with_tmux_var, false),
        no_images(false, false)
    );
    assert_eq!(
        ti::detect_capabilities_with(&with_tmux_var, true),
        no_images(false, true)
    );

    let via_term = ti::CapabilityInputs {
        term: Some("tmux-256color".into()),
        color_term: Some("truecolor".into()),
        ..inputs()
    };
    assert_eq!(
        ti::detect_capabilities_with(&via_term, true),
        no_images(true, true)
    );
}

#[test]
fn screen_never_gets_hyperlinks() {
    let screen = ti::CapabilityInputs {
        term: Some("screen-256color".into()),
        color_term: Some("24bit".into()),
        ..inputs()
    };
    // Upstream ignores the tmux probe result on the screen arm.
    assert_eq!(
        ti::detect_capabilities_with(&screen, true),
        no_images(true, false)
    );
}

#[test]
fn kitty_family_is_detected_from_window_id_program_or_term() {
    let window_id = ti::CapabilityInputs {
        kitty_window_id: true,
        ..inputs()
    };
    assert_eq!(ti::detect_capabilities_with(&window_id, false), kitty());

    let program = ti::CapabilityInputs {
        term_program: Some("kitty".into()),
        ..inputs()
    };
    assert_eq!(ti::detect_capabilities_with(&program, false), kitty());

    for ghostty in [
        ti::CapabilityInputs {
            term_program: Some("ghostty".into()),
            ..inputs()
        },
        ti::CapabilityInputs {
            term: Some("xterm-ghostty".into()),
            ..inputs()
        },
        ti::CapabilityInputs {
            ghostty_resources_dir: true,
            ..inputs()
        },
    ] {
        assert_eq!(ti::detect_capabilities_with(&ghostty, false), kitty());
    }

    for wezterm in [
        ti::CapabilityInputs {
            wezterm_pane: true,
            ..inputs()
        },
        ti::CapabilityInputs {
            term_program: Some("wezterm".into()),
            ..inputs()
        },
    ] {
        assert_eq!(ti::detect_capabilities_with(&wezterm, false), kitty());
    }

    for warp in [
        ti::CapabilityInputs {
            term_program: Some("warpterminal".into()),
            ..inputs()
        },
        ti::CapabilityInputs {
            warp_session_id: true,
            ..inputs()
        },
        ti::CapabilityInputs {
            warp_terminal_session_uuid: true,
            ..inputs()
        },
    ] {
        assert_eq!(ti::detect_capabilities_with(&warp, false), kitty());
    }
}

#[test]
fn iterm2_is_detected_from_session_id_or_program() {
    let session = ti::CapabilityInputs {
        iterm_session_id: true,
        ..inputs()
    };
    assert_eq!(ti::detect_capabilities_with(&session, false), iterm2());

    // `TERM_PROGRAM` arrives lowercased (see `capability_inputs_from_env`), so
    // the comparison is against `"iterm.app"`.
    let program = ti::CapabilityInputs {
        term_program: Some("iterm.app".into()),
        ..inputs()
    };
    assert_eq!(ti::detect_capabilities_with(&program, false), iterm2());
}

#[test]
fn true_color_without_images_terminals() {
    // Windows Terminal.
    let wt = ti::CapabilityInputs {
        wt_session: true,
        ..inputs()
    };
    assert_eq!(
        ti::detect_capabilities_with(&wt, false),
        no_images(true, true)
    );

    for program in ["alacritty", "vscode", "zed"] {
        let inputs = ti::CapabilityInputs {
            term_program: Some(program.into()),
            ..inputs()
        };
        assert_eq!(
            ti::detect_capabilities_with(&inputs, false),
            no_images(true, true),
            "{program}"
        );
    }

    let jediterm = ti::CapabilityInputs {
        terminal_emulator: Some("jetbrains-jediterm".into()),
        ..inputs()
    };
    assert_eq!(
        ti::detect_capabilities_with(&jediterm, false),
        no_images(true, false)
    );

    let windows_console = ti::CapabilityInputs {
        is_windows_console: true,
        ..inputs()
    };
    assert_eq!(
        ti::detect_capabilities_with(&windows_console, false),
        no_images(true, false)
    );
}

#[test]
fn unknown_terminal_is_conservative_but_keeps_the_true_colour_hint() {
    assert_eq!(
        ti::detect_capabilities_with(&inputs(), false),
        no_images(false, false)
    );

    let hinted = ti::CapabilityInputs {
        color_term: Some("truecolor".into()),
        ..inputs()
    };
    assert_eq!(
        ti::detect_capabilities_with(&hinted, false),
        no_images(true, false)
    );

    let hinted_24bit = ti::CapabilityInputs {
        color_term: Some("24bit".into()),
        ..inputs()
    };
    assert_eq!(
        ti::detect_capabilities_with(&hinted_24bit, false),
        no_images(true, false)
    );

    // A non-truecolour hint must not enable it.
    let weak_hint = ti::CapabilityInputs {
        color_term: Some("256".into()),
        ..inputs()
    };
    assert_eq!(
        ti::detect_capabilities_with(&weak_hint, false),
        no_images(false, false)
    );
}

#[test]
fn tmux_wins_over_a_kitty_program() {
    // Upstream checks tmux before every positive detection arm.
    let inputs = ti::CapabilityInputs {
        tmux: true,
        term_program: Some("ghostty".into()),
        ..inputs()
    };
    assert_eq!(
        ti::detect_capabilities_with(&inputs, false),
        no_images(false, false)
    );
}

#[test]
fn env_overrides_beat_detection() {
    // PI_IMAGE_PROTOCOL.
    let mut caps = no_images(false, false);
    ti::apply_env_overrides(&mut caps, Some("kitty"), None, None);
    assert_eq!(caps.images, Some(ti::ImageProtocol::Kitty));

    let mut caps = no_images(false, false);
    ti::apply_env_overrides(&mut caps, Some("iTerm2"), None, None);
    assert_eq!(caps.images, Some(ti::ImageProtocol::Iterm2));

    for forced_off in ["none", "0"] {
        let mut caps = kitty();
        ti::apply_env_overrides(&mut caps, Some(forced_off), None, None);
        assert_eq!(caps.images, None, "{forced_off}");
    }

    // An unrecognised protocol leaves the detection in place.
    let mut caps = kitty();
    ti::apply_env_overrides(&mut caps, Some("sixel"), None, None);
    assert_eq!(caps, kitty());

    // PI_TRUE_COLOR / PI_HYPERLINKS accept only "1" and "0".
    let mut caps = kitty();
    ti::apply_env_overrides(&mut caps, None, Some("0"), Some("0"));
    assert_eq!(
        caps,
        ti::TerminalCapabilities {
            images: Some(ti::ImageProtocol::Kitty),
            true_color: false,
            hyperlinks: false,
        }
    );

    let mut caps = no_images(false, false);
    ti::apply_env_overrides(&mut caps, None, Some("1"), Some("1"));
    assert_eq!(caps, no_images(true, true));

    // "true"/"false" are not the documented override spellings.
    let mut caps = no_images(false, false);
    ti::apply_env_overrides(&mut caps, None, Some("true"), Some("false"));
    assert_eq!(caps, no_images(false, false));
}

#[test]
fn protocol_spelling_matches_upstream() {
    assert_eq!(ti::ImageProtocol::Kitty.as_str(), "kitty");
    assert_eq!(ti::ImageProtocol::Iterm2.as_str(), "iterm2");
}

#[test]
fn image_line_detection_covers_prefix_and_mid_line_sequences() {
    assert!(ti::is_image_line("\u{1b}_Ga=T,f=100;AAAA\u{1b}\\"));
    assert!(ti::is_image_line("\u{1b}]1337;File=inline=1:AAAA\u{7}"));
    // Multi-row images are preceded by a cursor-up sequence.
    assert!(ti::is_image_line("\u{1b}[3A\u{1b}_Ga=T,f=100;AAAA\u{1b}\\"));
    assert!(ti::is_image_line("text\u{1b}]1337;File=inline=1:AAAA\u{7}"));

    assert!(!ti::is_image_line(""));
    assert!(!ti::is_image_line("plain text"));
    assert!(!ti::is_image_line("[Image: shot.png [image/png] 800x600]"));
    assert_eq!(ti::KITTY_PREFIX, "\u{1b}_G");
    assert_eq!(ti::ITERM2_PREFIX, "\u{1b}]1337;File=");
}

#[test]
fn cell_dimensions_default_and_round_trip() {
    assert_eq!(
        ti::get_cell_dimensions(),
        ti::CellDimensions {
            width_px: 9,
            height_px: 18
        }
    );
    assert_eq!(
        ti::CellDimensions::default(),
        ti::CellDimensions {
            width_px: 9,
            height_px: 18
        }
    );

    ti::set_cell_dimensions(ti::CellDimensions {
        width_px: 10,
        height_px: 20,
    });
    assert_eq!(
        ti::get_cell_dimensions(),
        ti::CellDimensions {
            width_px: 10,
            height_px: 20
        }
    );
    ti::set_cell_dimensions(ti::CellDimensions::default());
}

/// The process-global capability cache, exercised in a single test so it
/// cannot race other tests running in parallel threads.
#[test]
fn capability_cache_overrides_and_reset() {
    // A pinned value is returned verbatim and survives repeat reads.
    let pinned = iterm2();
    ti::set_capabilities(pinned);
    assert_eq!(ti::get_capabilities(), pinned);
    assert_eq!(ti::get_capabilities(), pinned);

    // Overrides must invalidate the cache, and pinning them beats detection.
    ti::set_capability_overrides(ti::CapabilityOverrides {
        images: ti::Override::Set(ti::ImageProtocol::Kitty),
        true_color: ti::Override::Unset,
        hyperlinks: ti::Override::Set(false),
    });
    let detected = ti::get_capabilities();
    assert_eq!(detected.images, Some(ti::ImageProtocol::Kitty));
    assert!(!detected.hyperlinks);

    // A no-op override keeps the cached record (upstream short-circuits too).
    ti::set_capabilities(pinned);
    ti::set_capability_overrides(ti::CapabilityOverrides {
        images: ti::Override::Set(ti::ImageProtocol::Kitty),
        true_color: ti::Override::Unset,
        hyperlinks: ti::Override::Set(false),
    });
    assert_eq!(ti::get_capabilities(), pinned);

    // `Override::Clear` forces images off.
    ti::set_capability_overrides(ti::CapabilityOverrides {
        images: ti::Override::Clear,
        ..ti::CapabilityOverrides::NONE
    });
    assert_eq!(ti::get_capabilities().images, None);

    // Back to unmodified detection.
    ti::set_capability_overrides(ti::CapabilityOverrides::NONE);
    ti::reset_capabilities_cache();
    let detected = ti::get_capabilities();
    assert_eq!(detected, ti::detect_capabilities_from_env());
}

#[test]
fn override_helpers_speak_unset_clear_and_set() {
    assert_eq!(ti::Override::<bool>::default(), ti::Override::Unset);
    assert_eq!(ti::Override::Unset.value(), None::<Option<bool>>);
    assert_eq!(ti::Override::<bool>::Clear.value(), Some(None));
    assert_eq!(ti::Override::Set(true).value(), Some(Some(true)));

    let mut slot = true;
    ti::Override::Unset.apply_to(&mut slot);
    assert!(slot);
    ti::Override::Set(false).apply_to(&mut slot);
    assert!(!slot);
    ti::Override::Clear.apply_to(&mut slot);
    assert!(!slot);
}

#[test]
fn image_ids_stay_inside_the_upstream_range() {
    let mut seen = std::collections::HashSet::new();
    for _ in 0..256 {
        let id = ti::allocate_image_id();
        assert!((1..=0xffff_fffe).contains(&id), "id out of range: {id}");
        seen.insert(id);
    }
    // 256 draws from a 4-billion space colliding is a broken generator, not
    // bad luck.
    assert!(seen.len() > 250, "only {} distinct ids", seen.len());
}
