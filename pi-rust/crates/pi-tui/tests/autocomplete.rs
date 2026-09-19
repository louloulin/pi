//! Integration coverage for the autocomplete port wired into [`Editor`].
//!
//! These tests drive the public editor surface the interactive mode uses:
//! install a [`CombinedAutocompleteProvider`], type trigger characters, and
//! assert on the resulting dropdown and buffer. They cover the acceptance
//! criteria for LUM-1122:
//!
//! 1. Typing `/` yields command candidates; the arrow keys / `Tab` select
//!    one and `Enter` applies it.
//! 2. Slash-command completion matches name **and** description and is
//!    ranked by `fuzzy_rank`.
//! 3. `@` yields file candidates relative to the provider's base path,
//!    including `dir/` and `~/` prefixes.
//! 4. `applyCompletion` leaves the cursor in the right place, including
//!    after multi-byte characters and for directories.
//! 5. Triggers only fire at token boundaries.
//! 6. `Esc` closes the dropdown without rewriting the input.
//! 7. `Tab` with the dropdown closed forces a file completion
//!    (`shouldTriggerFileCompletion` gating included).

use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use pi_tui::autocomplete::{
    AutocompleteItem, AutocompleteProvider, CombinedAutocompleteProvider, SlashCommand,
};
use pi_tui::input::{KeyCode, KeyModifiers};
use pi_tui::{Editor, EditorAction, Key};

/// A scratch directory that is removed when the test ends.
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(label: &str) -> Self {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "pi-tui-autocomplete-{}-{label}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create temp dir");
        Self { path }
    }

    fn path(&self) -> &PathBuf {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// Restores `HOME` when the test ends.
struct HomeGuard(Option<OsString>);

impl Drop for HomeGuard {
    fn drop(&mut self) {
        match self.0.take() {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
    }
}

fn key(code: KeyCode) -> Key {
    Key::new(code, KeyModifiers::NONE)
}

/// Type a string one character at a time, the way a terminal delivers
/// keystrokes (this is what exercises the trigger logic).
fn type_text(editor: &mut Editor, text: &str) {
    for c in text.chars() {
        editor.handle_key(Key::char(c));
    }
}

fn editor_with(provider: CombinedAutocompleteProvider) -> Editor {
    let mut editor = Editor::new();
    editor.set_autocomplete_provider(Arc::new(provider));
    editor
}

fn command_provider() -> CombinedAutocompleteProvider {
    CombinedAutocompleteProvider::new(
        vec![
            SlashCommand::new("help").with_description("show this help text"),
            SlashCommand::new("clear").with_description("wipe the message view"),
            SlashCommand::new("render").with_description("redraw the view"),
        ],
        ".",
    )
}

// -- command completion ------------------------------------------------

#[test]
fn typing_slash_shows_candidates_and_arrow_tab_applies() {
    let mut editor = editor_with(command_provider());
    type_text(&mut editor, "/");

    assert!(editor.is_showing_autocomplete());
    assert_eq!(editor.autocomplete_prefix(), "/");
    assert_eq!(editor.autocomplete_items().len(), 3);
    assert_eq!(editor.autocomplete_selected(), 0);

    // Arrow down moves the highlight; `Tab` applies it.
    assert_eq!(editor.handle_key(key(KeyCode::Down)), EditorAction::Changed);
    assert_eq!(editor.autocomplete_selected(), 1);
    assert_eq!(editor.handle_key(key(KeyCode::Tab)), EditorAction::Changed);
    assert_eq!(editor.text(), "/clear ");
    assert_eq!(editor.cursor(), "/clear ".len());
    assert!(!editor.is_showing_autocomplete());
}

#[test]
fn enter_applies_the_selected_command_and_submits_it() {
    let mut editor = editor_with(command_provider());
    type_text(&mut editor, "/cle");
    assert!(editor.is_showing_autocomplete());
    assert_eq!(editor.autocomplete_items()[0].value, "clear");

    // Upstream falls through to submit for a `/`-prefixed prefix, so the
    // applied command is submitted with its trailing space in one key.
    assert_eq!(
        editor.handle_key(key(KeyCode::Enter)),
        EditorAction::Submit("/clear ".to_string())
    );
    assert_eq!(editor.text(), "/clear ");
}

#[test]
fn command_candidates_match_name_and_description() {
    let mut editor = editor_with(command_provider());
    // A pure name match.
    type_text(&mut editor, "/ren");
    assert_eq!(editor.autocomplete_items()[0].value, "render");

    // A description-only match still surfaces the command.
    let mut editor = editor_with(command_provider());
    type_text(&mut editor, "/message");
    assert!(editor.is_showing_autocomplete());
    assert_eq!(editor.autocomplete_items()[0].value, "clear");
    assert_eq!(
        editor.autocomplete_items()[0].description.as_deref(),
        Some("wipe the message view")
    );
}

#[test]
fn exact_command_match_is_ranked_first() {
    let provider = CombinedAutocompleteProvider::new(
        vec![
            SlashCommand::new("app"),
            SlashCommand::new("a_p_p"),
            SlashCommand::new("application"),
        ],
        ".",
    );
    let mut editor = editor_with(provider);
    type_text(&mut editor, "/app");
    assert_eq!(editor.autocomplete_items()[0].value, "app");
}

#[test]
fn no_candidates_leaves_the_dropdown_closed() {
    let mut editor = editor_with(command_provider());
    type_text(&mut editor, "/zzz");
    assert!(!editor.is_showing_autocomplete());
    assert_eq!(editor.text(), "/zzz");
}

#[test]
fn non_trigger_characters_do_not_open_the_dropdown() {
    let mut editor = editor_with(command_provider());
    type_text(&mut editor, "hello");
    assert!(!editor.is_showing_autocomplete());
    assert_eq!(editor.text(), "hello");

    // `#` is a default trigger character, but this provider has no
    // candidates for it, so the dropdown stays closed.
    let mut editor = editor_with(command_provider());
    type_text(&mut editor, "#");
    assert!(!editor.is_showing_autocomplete());
    assert_eq!(editor.text(), "#");
}

#[test]
fn esc_closes_the_dropdown_without_rewriting_the_input() {
    let mut editor = editor_with(command_provider());
    type_text(&mut editor, "/");
    assert!(editor.is_showing_autocomplete());

    assert_eq!(editor.handle_key(key(KeyCode::Esc)), EditorAction::None);
    assert!(!editor.is_showing_autocomplete());
    // The buffer is exactly what the user typed: no completion applied.
    assert_eq!(editor.text(), "/");
    assert_eq!(editor.cursor(), 1);
}

#[test]
fn typing_after_cancel_reopens_for_the_new_prefix() {
    let mut editor = editor_with(command_provider());
    type_text(&mut editor, "/");
    editor.handle_key(key(KeyCode::Esc));
    assert!(!editor.is_showing_autocomplete());

    type_text(&mut editor, "cl");
    assert!(editor.is_showing_autocomplete());
    assert_eq!(editor.autocomplete_prefix(), "/cl");
    assert_eq!(editor.autocomplete_items()[0].value, "clear");
}

// -- Tab force ---------------------------------------------------------

#[test]
fn tab_forces_a_file_completion_when_the_dropdown_is_closed() {
    let dir = TempDir::new("force");
    fs::write(dir.path().join("readme.md"), "").unwrap();
    fs::write(dir.path().join("notes.txt"), "").unwrap();
    let provider = CombinedAutocompleteProvider::new(Vec::new(), dir.path().clone());
    let mut editor = editor_with(provider);

    type_text(&mut editor, "look at ");
    assert!(!editor.is_showing_autocomplete());

    // Two candidates, so the explicit Tab opens the dropdown instead of
    // auto-applying a lone match.
    assert_eq!(editor.handle_key(key(KeyCode::Tab)), EditorAction::Changed);
    assert!(editor.is_showing_autocomplete());
    assert_eq!(editor.autocomplete_items().len(), 2);

    editor.handle_key(key(KeyCode::Down));
    assert_eq!(
        editor.handle_key(key(KeyCode::Enter)),
        EditorAction::Changed
    );
    // A bare path completion adds no trailing space (only `@` file and
    // slash-command completions do).
    assert_eq!(editor.text(), "look at readme.md");
}

#[test]
fn tab_auto_applies_a_lone_forced_file_candidate() {
    let dir = TempDir::new("lone");
    fs::write(dir.path().join("readme.md"), "").unwrap();
    let provider = CombinedAutocompleteProvider::new(Vec::new(), dir.path().clone());
    let mut editor = editor_with(provider);

    type_text(&mut editor, "open ");
    assert_eq!(editor.handle_key(key(KeyCode::Tab)), EditorAction::Changed);
    assert_eq!(editor.text(), "open readme.md");
    assert!(!editor.is_showing_autocomplete());
}

#[test]
fn should_trigger_file_completion_skips_command_names() {
    let provider = CombinedAutocompleteProvider::new(Vec::new(), ".");
    assert!(!provider.should_trigger_file_completion(&["/model".to_string()], 0, 6));
    assert!(provider.should_trigger_file_completion(&["hello ".to_string()], 0, 6));
    assert!(provider.should_trigger_file_completion(&["/model src/".to_string()], 0, 10));
}

// -- `@` file completion ----------------------------------------------

fn dir_provider(dir: &TempDir) -> CombinedAutocompleteProvider {
    fs::create_dir_all(dir.path().join("src")).unwrap();
    fs::write(dir.path().join("src/main.rs"), "").unwrap();
    fs::write(dir.path().join("src/lib.rs"), "").unwrap();
    fs::write(dir.path().join("README.md"), "").unwrap();
    CombinedAutocompleteProvider::new(Vec::new(), dir.path().clone())
}

#[test]
fn at_trigger_lists_directory_entries() {
    let dir = TempDir::new("at-root");
    let provider = dir_provider(&dir);
    let mut editor = editor_with(provider);

    type_text(&mut editor, "@");
    assert!(editor.is_showing_autocomplete());
    assert_eq!(editor.autocomplete_prefix(), "@");
    let labels: Vec<&str> = editor
        .autocomplete_items()
        .iter()
        .map(|item| item.label.as_str())
        .collect();
    assert!(labels.contains(&"src/"));
    assert!(labels.contains(&"README.md"));
}

#[test]
fn directory_prefix_completes_and_keeps_the_cursor_after_the_slash() {
    let dir = TempDir::new("at-dir");
    let provider = dir_provider(&dir);
    let mut editor = editor_with(provider);

    type_text(&mut editor, "@src");
    assert!(editor.is_showing_autocomplete());
    assert_eq!(editor.autocomplete_items()[0].label, "src/");
    assert_eq!(editor.autocomplete_items()[0].value, "@src/");

    // Applying a directory adds no trailing space so the user can keep
    // completing inside it.
    assert_eq!(editor.handle_key(key(KeyCode::Tab)), EditorAction::Changed);
    assert_eq!(editor.text(), "@src/");
    assert_eq!(editor.cursor(), "@src/".len());

    type_text(&mut editor, "ma");
    assert_eq!(editor.autocomplete_items()[0].label, "main.rs");
    assert_eq!(
        editor.handle_key(key(KeyCode::Enter)),
        EditorAction::Changed
    );
    assert_eq!(editor.text(), "@src/main.rs ");
    assert_eq!(editor.cursor(), "@src/main.rs ".len());
}

#[test]
fn home_prefix_resolves_against_home() {
    let home = TempDir::new("home");
    fs::create_dir_all(home.path().join("notes")).unwrap();
    fs::write(home.path().join("notes/todo.md"), "").unwrap();

    let _guard = HomeGuard(std::env::var_os("HOME"));
    std::env::set_var("HOME", home.path());

    let provider = CombinedAutocompleteProvider::new(Vec::new(), home.path().clone());
    let mut editor = editor_with(provider);
    type_text(&mut editor, "@~/notes/");

    assert!(editor.is_showing_autocomplete());
    assert_eq!(editor.autocomplete_prefix(), "@~/notes/");
    assert_eq!(editor.autocomplete_items()[0].label, "todo.md");
    assert_eq!(editor.autocomplete_items()[0].value, "@~/notes/todo.md");

    assert_eq!(editor.handle_key(key(KeyCode::Tab)), EditorAction::Changed);
    assert_eq!(editor.text(), "@~/notes/todo.md ");
}

#[test]
fn apply_completion_keeps_the_cursor_on_a_char_boundary_after_multibyte_text() {
    let dir = TempDir::new("multibyte");
    fs::write(dir.path().join("café.txt"), "").unwrap();
    let provider = CombinedAutocompleteProvider::new(Vec::new(), dir.path().clone());
    let mut editor = editor_with(provider);

    type_text(&mut editor, "héllo @caf");
    assert!(editor.is_showing_autocomplete());
    assert_eq!(editor.autocomplete_items()[0].label, "café.txt");

    assert_eq!(
        editor.handle_key(key(KeyCode::Enter)),
        EditorAction::Changed
    );
    assert_eq!(editor.text(), "héllo @café.txt ");
    assert_eq!(editor.cursor(), editor.text().len());
    assert!(editor.text().is_char_boundary(editor.cursor()));
}

#[test]
fn apply_completion_cursor_is_correct_for_a_multibyte_prefix() {
    // Exercise `apply_completion` directly with a multi-byte prefix so the
    // byte-offset arithmetic is checked, not just the editor path.
    let provider = CombinedAutocompleteProvider::new(Vec::new(), ".");
    let item = AutocompleteItem::new("@src/主.rs", "主.rs");
    let result = provider.apply_completion(&["@主".to_string()], 0, "@主".len(), &item, "@主");
    assert_eq!(result.lines, vec!["@src/主.rs ".to_string()]);
    assert_eq!(result.cursor_col, "@src/主.rs ".len());
    assert!(result.lines[0].is_char_boundary(result.cursor_col));
}

// -- token boundaries --------------------------------------------------

#[test]
fn at_trigger_requires_a_token_boundary() {
    let dir = TempDir::new("boundary");
    fs::write(dir.path().join("readme.md"), "").unwrap();
    let provider = CombinedAutocompleteProvider::new(Vec::new(), dir.path().clone());
    let mut editor = editor_with(provider);

    // `a@` is not a token start, so the dropdown stays closed.
    type_text(&mut editor, "a");
    type_text(&mut editor, "@");
    assert!(!editor.is_showing_autocomplete());
    assert_eq!(editor.text(), "a@");

    // After a space it is.
    type_text(&mut editor, " ");
    assert!(!editor.is_showing_autocomplete());
    type_text(&mut editor, "@");
    assert!(editor.is_showing_autocomplete());
}

#[test]
fn trigger_pattern_does_not_fire_inside_a_word() {
    let mut editor = editor_with(command_provider());
    type_text(&mut editor, "email foo@bar");
    assert!(!editor.is_showing_autocomplete());
    assert_eq!(editor.text(), "email foo@bar");
}

// -- dropdown rendering ------------------------------------------------

#[test]
fn dropdown_rendering_marks_the_selection_and_windows_long_lists() {
    let mut editor = editor_with(command_provider());
    type_text(&mut editor, "/");
    let rows = editor.autocomplete_render_lines(40);
    assert_eq!(rows.len(), 3);
    assert!(rows[0].starts_with("❯ "), "{rows:?}");
    assert!(rows[0].contains("help"));
    assert!(rows[1].starts_with("  "), "{rows:?}");

    // Six candidates but a three-row window: the visible rows are windowed
    // and a `(n/total)` hint is appended.
    let provider = CombinedAutocompleteProvider::new(
        (0..6)
            .map(|i| SlashCommand::new(format!("cmd{i}")))
            .collect(),
        ".",
    );
    let mut editor = editor_with(provider);
    editor.set_autocomplete_max_visible(3);
    assert_eq!(editor.autocomplete_max_visible(), 3);
    type_text(&mut editor, "/");
    let rows = editor.autocomplete_render_lines(40);
    assert_eq!(rows.len(), 4);
    assert!(rows[3].contains("/6"), "{rows:?}");

    // The height is clamped into 3..=20.
    editor.set_autocomplete_max_visible(100);
    assert_eq!(editor.autocomplete_max_visible(), 20);
    editor.set_autocomplete_max_visible(1);
    assert_eq!(editor.autocomplete_max_visible(), 3);
}

#[test]
fn move_autocomplete_wraps_around_the_candidate_list() {
    let mut editor = editor_with(command_provider());
    type_text(&mut editor, "/");
    assert_eq!(editor.autocomplete_selected(), 0);
    editor.handle_key(key(KeyCode::Up));
    assert_eq!(
        editor.autocomplete_selected(),
        editor.autocomplete_items().len() - 1
    );
    editor.handle_key(key(KeyCode::Down));
    assert_eq!(editor.autocomplete_selected(), 0);
}

// -- provider unit surface --------------------------------------------

fn provider_of(commands: Vec<SlashCommand>, base: &TempDir) -> CombinedAutocompleteProvider {
    CombinedAutocompleteProvider::new(commands, base.path().clone())
}

#[test]
fn get_suggestions_returns_none_outside_a_completable_context() {
    let dir = TempDir::new("none");
    let provider = provider_of(vec![SlashCommand::new("help")], &dir);
    assert!(provider
        .get_suggestions(&["just prose".to_string()], 0, 10, false)
        .is_none());
    assert!(provider
        .get_suggestions(&["/unknown".to_string()], 0, 8, false)
        .is_none());
}

#[test]
fn argument_completion_uses_the_argument_prefix() {
    let dir = TempDir::new("args");
    let completer: pi_tui::autocomplete::ArgumentCompletions = Arc::new(|argument: &str| {
        Some(vec![AutocompleteItem::new(
            format!("{argument}-done"),
            "done",
        )])
    });
    let provider = provider_of(
        vec![SlashCommand::new("trust").with_argument_completions(completer)],
        &dir,
    );
    let suggestions = provider
        .get_suggestions(&["/trust ye".to_string()], 0, 9, false)
        .expect("argument suggestions");
    assert_eq!(suggestions.prefix, "ye");
    assert_eq!(suggestions.items[0].value, "ye-done");
    assert!(provider
        .get_suggestions(&["/trust ye".to_string()], 0, 9, true)
        .is_none());
}
