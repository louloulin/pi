//! Integration coverage for the theme module: built-in and on-disk loading,
//! validation errors, ANSI rendering and controller fallback.

use std::fs;
use std::path::PathBuf;

use pi_tui::theme::{
    available_themes, builtin_theme, builtin_theme_json, default_theme_name, load_theme,
    load_theme_from_path, ColorMode, ThemeBg, ThemeColor, ThemeController, ThemeError, ThemeJson,
    BUILTIN_DARK_JSON,
};

fn temp_dir(label: &str) -> PathBuf {
    let mut dir = std::env::temp_dir();
    dir.push(format!("pi-tui-theme-{}-{label}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

/// A valid custom theme: the built-in dark document with another name.
fn custom_theme_text(name: &str) -> String {
    BUILTIN_DARK_JSON.replace("\"name\": \"dark\"", &format!("\"name\": \"{name}\""))
}

#[test]
fn builtin_themes_resolve_every_slot_in_both_modes() {
    for name in ["dark", "light"] {
        for mode in [ColorMode::TrueColor, ColorMode::Ansi256] {
            let theme = builtin_theme(name, mode).expect("built-in theme loads");
            assert_eq!(theme.name(), Some(name));
            assert_eq!(theme.color_mode(), mode);
            assert_eq!(theme.source_path(), None);
            assert_eq!(theme.is_light(), name == "light");

            for slot in ThemeColor::ALL {
                let ansi = theme.get_fg_ansi(slot);
                assert!(ansi.starts_with('\u{1b}'), "{name}/{slot}: {ansi:?}");
                assert!(ansi.ends_with('m'));
            }
            for slot in ThemeBg::ALL {
                let ansi = theme.get_bg_ansi(slot);
                assert!(ansi.starts_with('\u{1b}'), "{name}/{slot}: {ansi:?}");
                assert!(ansi.ends_with('m'));
            }

            let rendered = theme.fg(ThemeColor::Accent, "hi");
            assert!(rendered.ends_with("hi\u{1b}[39m"));
            let rendered = theme.bg(ThemeBg::UserMessageBg, "hi");
            assert!(rendered.ends_with("hi\u{1b}[49m"));
        }
    }
}

#[test]
fn custom_themes_load_from_a_directory() {
    let dir = temp_dir("custom");
    let path = dir.join("midnight.json");
    fs::write(&path, custom_theme_text("midnight")).expect("write theme");

    let theme = load_theme("midnight", ColorMode::TrueColor, Some(&dir)).expect("load custom");
    assert_eq!(theme.name(), Some("midnight"));
    assert_eq!(theme.source_path(), Some(path.as_path()));
    assert!(!theme.is_light());

    let by_path = load_theme_from_path(&path, ColorMode::Ansi256).expect("load by path");
    assert_eq!(by_path.name(), Some("midnight"));

    assert!(matches!(
        load_theme("nope", ColorMode::TrueColor, Some(&dir)),
        Err(ThemeError::ThemeNotFound(name)) if name == "nope"
    ));
    assert!(matches!(
        load_theme("midnight", ColorMode::TrueColor, None),
        Err(ThemeError::ThemeNotFound(_))
    ));

    fs::remove_dir_all(&dir).ok();
}

#[test]
fn missing_required_colors_are_reported_with_the_token_list() {
    let content = r##"{
        "name": "partial",
        "colors": { "accent": "#8abeb7", "text": "#e5e5e7" }
    }"##;
    let error = ThemeJson::parse("partial.json", content).unwrap_err();
    match &error {
        ThemeError::MissingRequiredColors { label, missing } => {
            assert_eq!(label, "partial.json");
            assert_eq!(missing.len(), 49);
            assert!(!missing.contains(&"accent".to_string()));
            assert!(missing.contains(&"error".to_string()));
        }
        other => panic!("unexpected error: {other}"),
    }
    let message = error.to_string();
    assert!(message.contains("Missing required color tokens"));
    assert!(message.contains("  - error"));
}

#[test]
fn invalid_documents_report_their_label() {
    let error = ThemeJson::parse("broken.json", "{ not json").unwrap_err();
    assert!(matches!(error, ThemeError::InvalidJson { label, .. } if label == "broken.json"));

    let error = ThemeJson::parse("slash.json", &custom_theme_text("light/dark")).unwrap_err();
    assert_eq!(
        error,
        ThemeError::InvalidThemeName {
            name: "light/dark".to_string()
        }
    );
}

#[test]
fn available_themes_lists_builtins_and_valid_custom_files() {
    let dir = temp_dir("available");
    fs::write(dir.join("zzz.json"), custom_theme_text("zzz-custom")).expect("write theme");
    fs::write(dir.join("broken.json"), "{ nope").expect("write broken theme");
    fs::write(dir.join("notes.txt"), "ignored").expect("write ignored file");

    let names = available_themes(Some(&dir));
    assert!(names.contains(&"dark".to_string()));
    assert!(names.contains(&"light".to_string()));
    // The file name is irrelevant; the document's `name` is used and invalid
    // documents are skipped.
    assert!(names.contains(&"zzz-custom".to_string()));
    assert!(!names.contains(&"broken".to_string()));
    assert_eq!(names, {
        let mut sorted = names.clone();
        sorted.sort();
        sorted
    });

    assert_eq!(
        available_themes(None),
        vec!["dark".to_string(), "light".to_string()]
    );

    fs::remove_dir_all(&dir).ok();
}

#[test]
fn controller_switches_themes_and_falls_back_to_dark() {
    let dir = temp_dir("controller");
    fs::write(dir.join("midnight.json"), custom_theme_text("midnight")).expect("write theme");

    let mut controller =
        ThemeController::init(Some("midnight"), ColorMode::TrueColor, Some(dir.clone()));
    assert_eq!(controller.current_name(), Some("midnight"));
    assert_eq!(controller.theme().name(), Some("midnight"));

    controller.set_theme("light").expect("switch to built-in");
    assert_eq!(controller.current_name(), Some("light"));
    assert!(controller.theme().is_light());

    let error = controller.set_theme("missing").unwrap_err();
    assert!(matches!(error, ThemeError::ThemeNotFound(_)));
    assert_eq!(controller.current_name(), Some("dark"));
    assert!(!controller.theme().is_light());

    fs::remove_dir_all(&dir).ok();
}

#[test]
fn reset_and_index_colours_render_without_hex_conversion() {
    let content = custom_theme_text("mapped")
        .replace("\"accent\": \"accent\"", "\"accent\": 205")
        .replace("\"text\": \"text\"", "\"text\": \"\"");
    let json = ThemeJson::parse("mapped", &content).expect("parse mapped theme");
    let theme = builtin_theme("dark", ColorMode::TrueColor).expect("sanity");

    let json_theme = pi_tui::theme::Theme::from_json(&json, ColorMode::TrueColor).expect("build");
    assert_eq!(
        json_theme.get_fg_ansi(ThemeColor::Accent),
        "\u{1b}[38;5;205m"
    );
    assert_eq!(json_theme.get_fg_ansi(ThemeColor::Text), "\u{1b}[39m");
    // Untouched slots still resolve through their `vars` references.
    assert_eq!(
        json_theme.get_fg_ansi(ThemeColor::Success),
        theme.get_fg_ansi(ThemeColor::Success)
    );
}

#[test]
fn documents_round_trip_through_serde() {
    let json = builtin_theme_json("light").expect("built-in light");
    let text = serde_json::to_string(&json).expect("serialize");
    let reparsed = ThemeJson::parse("round-trip", &text).expect("reparse");
    assert_eq!(json, reparsed);
}

#[test]
fn default_theme_name_follows_the_terminal_background() {
    assert_eq!(default_theme_name(None), "dark");
    assert_eq!(default_theme_name(Some("15;0")), "dark");
    assert_eq!(default_theme_name(Some("0;15")), "light");
}
