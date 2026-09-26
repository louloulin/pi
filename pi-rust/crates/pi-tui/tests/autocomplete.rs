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
    compose_autocomplete_providers, AutocompleteItem, AutocompleteProvider,
    AutocompleteProviderFactory, AutocompleteSuggestions, CombinedAutocompleteProvider,
    CompletionResult, SlashCommand,
};
use pi_tui::input::{KeyCode, KeyModifiers};
use pi_tui::{Editor, EditorAction, Key, Selector, SelectorItem, SelectorLayout};

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
    assert!(rows[0].starts_with("→ "), "{rows:?}");
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

/// The dropdown borrows the `SelectList` row layout. N7 right-aligns the
/// description column to the row's right edge (nanopi pattern); the label
/// sits flush with the `→ ` marker and is padded so the description lands
/// at the same column on every row.
#[test]
fn slash_dropdown_aligns_descriptions_into_the_select_list_column() {
    let mut editor = editor_with(command_provider());
    type_text(&mut editor, "/");
    let rows = editor.autocomplete_render_lines(80);
    assert_eq!(rows.len(), 3);

    // Every row is 80 columns wide and ends with its description.
    let suffixes = ["show this help text", "wipe the message view", "redraw the view"];
    let markers = ["→", "  ", "  "];
    for ((row, suffix), marker) in rows.iter().zip(suffixes.iter()).zip(markers.iter()) {
        assert!(row.starts_with(*marker), "row is {row:?}");
        assert!(row.ends_with(suffix), "row is {row:?}");
        assert_eq!(row.chars().count(), 80, "row is {row:?}");
    }
    assert!(rows[0].starts_with("→ help"));
    assert!(rows[1].starts_with("  clear"));
}

/// N7 ellipsizes the label column to keep the description flush with the
/// right edge regardless of the label's natural width — a wide label
/// shortens, a short one pads, and both rows end on the same column.
#[test]
fn slash_dropdown_primary_column_tracks_the_widest_label() {
    let provider = CombinedAutocompleteProvider::new(
        vec![
            SlashCommand::new("a-very-long-command-name").with_description("first"),
            SlashCommand::new("ok").with_description("second"),
        ],
        ".",
    );
    let mut editor = editor_with(provider);
    type_text(&mut editor, "/");
    let rows = editor.autocomplete_render_lines(80);

    let markers = ["→", "  "];
    for ((row, description), marker) in rows
        .iter()
        .zip(["first", "second"])
        .zip(markers.iter())
    {
        assert!(row.starts_with(*marker), "row is {row:?}");
        assert!(row.ends_with(description), "row is {row:?}");
        assert_eq!(row.chars().count(), 80, "row is {row:?}");
    }
    // Short label `ok` is padded to the column.
    assert!(rows[1].starts_with("  ok"), "row is {:?}", rows[1]);
}

/// N7 right-aligns the description column to the row's right edge. The
/// label is only ellipsized when its natural width would collide with the
/// description; otherwise it sits un-truncated.
#[test]
fn slash_dropdown_clamps_a_label_wider_than_the_primary_column() {
    let provider = CombinedAutocompleteProvider::new(
        vec![
            SlashCommand::new("a-command-name-that-is-far-too-long-for-the-column")
                .with_description("kept"),
        ],
        ".",
    );
    let mut editor = editor_with(provider);
    type_text(&mut editor, "/");
    let rows = editor.autocomplete_render_lines(80);

    assert_eq!(rows.len(), 1, "{rows:?}");
    let row = &rows[0];
    assert_eq!(row.chars().count(), 80, "row is {row:?}");
    assert!(row.ends_with("kept"), "row is {row:?}");
    // At width=80 the 54-cell label leaves 24 cells for gap+padding+desc,
    // so no ellipsis is needed.
    assert!(
        row.starts_with("→ a-command-name-that-is-far-too-long-for-the-column"),
        "row is {row:?}"
    );
}

/// Verify the ellipsization kicks in when the label would actually
/// overflow — at width=57 the 50-cell label leaves no slack for gap +
/// description, so N7 truncates the label.
#[test]
fn slash_dropdown_ellipsizes_label_when_it_collides_with_description() {
    let provider = CombinedAutocompleteProvider::new(
        vec![
            SlashCommand::new("a-command-name-that-is-far-too-long-for-the-column")
                .with_description("kept"),
        ],
        ".",
    );
    let mut editor = editor_with(provider);
    type_text(&mut editor, "/");
    let rows = editor.autocomplete_render_lines(57);

    assert_eq!(rows.len(), 1, "{rows:?}");
    let row = &rows[0];
    assert_eq!(row.chars().count(), 57, "row is {row:?}");
    assert!(row.ends_with("kept"), "row is {row:?}");
    // The label must be ellipsized (trailing `…`) and never contain
    // the un-truncated tail of the source label.
    assert!(row.contains('…'), "row is {row:?}");
    assert!(!row.contains("long-for-the-column"), "row is {row:?}");
}

/// Non-slash completions (the `@` file menu) use the `SelectList` default —
/// a fixed 32-column primary column — because their labels are file names
/// (upstream `createAutocompleteList`'s `undefined` layout).
#[test]
fn file_dropdown_uses_the_fixed_primary_column() {
    let dir = TempDir::new("primary-column");
    fs::write(dir.path().join("readme.md"), "").unwrap();
    let provider = CombinedAutocompleteProvider::new(Vec::new(), dir.path().clone());
    let mut editor = editor_with(provider);
    type_text(&mut editor, "@readme");
    let rows = editor.autocomplete_render_lines(80);
    assert_eq!(rows.len(), 1, "{rows:?}");

    // N7 right-aligns the description column to the row's right edge. The
    // label "readme.md" (9 cols) sits flush with the `→ ` marker at column
    // 0 and "readme.md" (the description) is right-aligned at column 79.
    assert!(rows[0].starts_with("→ readme.md"), "{rows:?}");
    assert!(
        rows[0].ends_with("readme.md"),
        "description must be right-aligned, got {rows:?}"
    );
    assert_eq!(rows[0].chars().count(), 80, "row is {rows:?}");
    // And that is exactly where a modal `Selector` with the same row puts it.
    let selector = Selector::new(
        "Pick",
        vec![SelectorItem::new("x", "readme.md").with_description("readme.md")],
    );
    let modal = selector.render_lines(80);
    assert!(
        modal[2].ends_with("readme.md"),
        "modal selector must right-align, got {:?}",
        modal
    );
}

/// Rows narrower than the description-minimum (5 cells of slack — arrow +
/// gap + at least one label + at least one description cell) drop the
/// description column entirely. N7 ellipsizes the label to fit a wide
/// description at the right edge of `width`, so descriptions stay on rows
/// down to that threshold.
#[test]
fn narrow_dropdown_drops_the_description_column() {
    let mut editor = editor_with(command_provider());
    type_text(&mut editor, "/");
    // Widths that allow a 4+1 description column keep descriptions. The
    // selected row preserves the label verbatim; the unselected rows
    // ellipsize the label to leave room for the description.
    for width in [40, 30, 20] {
        let rows = editor.autocomplete_render_lines(width);
        assert!(rows[0].starts_with("→"), "{rows:?}");
        // Every row is right-aligned to `width` (or shorter).
        assert!(
            rows.iter().all(|row| row.chars().count() <= width),
            "{rows:?}"
        );
        // The description survives in some form on every row.
        assert!(
            rows.iter().any(|row| row.contains("help")),
            "{rows:?}"
        );
    }
    // Widths that collapse below the description floor drop the column.
    for width in [8, 7, 6] {
        let rows = editor.autocomplete_render_lines(width);
        assert!(
            !rows.iter().any(|row| row.contains("help text")),
            "{rows:?}"
        );
        assert!(rows[0].starts_with("→"), "{rows:?}");
        assert!(
            rows.iter().all(|row| row.chars().count() <= width),
            "{rows:?}"
        );
    }
}

/// One `SelectList` layout, two lists: with the same rows, the dropdown and
/// the modal `Selector` must place the label and the description identically.
/// This is the regression gate for "the editor grew its own renderer".
#[test]
fn the_dropdown_and_the_modal_selector_share_one_row_layout() {
    let items = vec![
        AutocompleteItem::new("help", "help").with_description("show this help text"),
        AutocompleteItem::new("clear", "clear").with_description("wipe the message view"),
    ];
    let mut editor = Editor::new();
    editor.set_autocomplete_provider(Arc::new(FixedProvider {
        items: items.clone(),
    }));
    editor.handle_key(Key::char('/'));
    // The `/` menu is the one context with upstream's slash layout; the modal
    // selector is told the same bounds, which is the point of the gate.
    assert_eq!(
        editor.autocomplete_layout(),
        SelectorLayout::slash_command(),
        "the slash menu must use upstream's slash bounds"
    );
    let dropdown = editor.autocomplete_render_lines(80);

    let selector = Selector::new(
        "Pick",
        items
            .iter()
            .map(|item| {
                SelectorItem::new(item.value.clone(), item.label.clone())
                    .with_description(item.description.clone().unwrap_or_default())
            })
            .collect(),
    )
    .with_primary_column_width(12, 32);
    // `Selector::render_lines` adds the title and the `─` rule above the rows.
    let modal = selector.render_lines(80);
    assert_eq!(dropdown, modal[2..].to_vec());
}

/// A provider that always offers the same candidates — enough to drive the
/// dropdown without a filesystem.
#[derive(Debug)]
struct FixedProvider {
    items: Vec<AutocompleteItem>,
}

impl AutocompleteProvider for FixedProvider {
    fn get_suggestions(
        &self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
        _force: bool,
    ) -> Option<AutocompleteSuggestions> {
        let line = lines.get(cursor_line)?;
        Some(AutocompleteSuggestions {
            items: self.items.clone(),
            prefix: line[..cursor_col].to_string(),
        })
    }

    fn apply_completion(
        &self,
        lines: &[String],
        cursor_line: usize,
        _cursor_col: usize,
        item: &AutocompleteItem,
        _prefix: &str,
    ) -> CompletionResult {
        let mut lines = lines.to_vec();
        lines[cursor_line] = item.value.clone();
        CompletionResult {
            lines,
            cursor_line,
            cursor_col: item.value.len(),
        }
    }
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

// -- App painting (LUM-1236) -------------------------------------------
//
// The engine above was fully covered while the feature was invisible: the
// `App` owns the composer but never painted the dropdown, so installing a
// provider changed nothing on screen. The port had the editor state and
// the keymap but no painter. These tests pin the painter's placement:
// the list sits immediately above the prompt and grows towards older
// output, and closing it hands the rows back to the transcript.

fn faux_model() -> pi_protocol::Model {
    pi_protocol::Model {
        provider: pi_protocol::ProviderId::new("faux"),
        id: "faux-model".into(),
        api: pi_protocol::Api::Faux,
        label: Some("Faux".into()),
        context_window: 1024,
        max_output_tokens: 256,
    }
}

fn app_with(commands: Vec<SlashCommand>, base: &TempDir) -> pi_tui::app::App {
    let agent = pi_agent_core::Agent::new(pi_agent_core::AgentOptions::new(
        faux_model(),
        Arc::new(pi_ai::providers::faux::FauxProvider::default()),
        "you are pi",
    ));
    let mut app = pi_tui::app::App::new(&agent, pi_tui::app::AppConfig::default());
    app.prompt_mut()
        .editor_mut()
        .set_autocomplete_provider(Arc::new(provider_of(commands, base)));
    app
}

fn type_into(app: &mut pi_tui::app::App, text: &str) {
    for c in text.chars() {
        app.step(pi_tui::InputEvent::Key(key(KeyCode::Char(c))));
    }
}

#[test]
fn the_app_paints_the_dropdown_directly_above_the_prompt() {
    let dir = TempDir::new("app-paint");
    let mut app = app_with(
        vec![
            SlashCommand::new("help").with_description("show help"),
            SlashCommand::new("hotkeys").with_description("list shortcuts"),
        ],
        &dir,
    );

    // Nothing is painted until the trigger is typed.
    let before = app.render_snapshot(48, 12).lines.join("\n");
    assert!(!before.contains('→'), "{before}");

    type_into(&mut app, "/h");
    let snapshot = app.render_snapshot(48, 12);
    let selected: Vec<&String> = snapshot
        .lines
        .iter()
        .filter(|line| line.contains('→'))
        .collect();
    assert_eq!(selected.len(), 1, "{:?}", snapshot.lines);

    let selected_at = snapshot
        .lines
        .iter()
        .position(|line| line.contains('→'))
        .expect("selected row");
    let prompt_at = snapshot
        .lines
        .iter()
        .position(|line| line.contains("/h▍"))
        .expect("prompt row");
    // The list is bottom-anchored: its last row is the one directly above
    // the prompt, and it grows from there towards older output.
    assert!(selected_at < prompt_at, "{:?}", snapshot.lines);
    // Phase 2 (G3) paints the `─` editor border directly above the prompt,
    // so the dropdown's bottom row is two cells up instead of one.
    //
    // N7 right-aligns the description column to the row's right edge. With
    // width=48 the unselected row is `  hotkeys` followed by enough
    // padding to push "list shortcuts" flush against column 48.
    let last = &snapshot.lines[prompt_at - 2];
    assert!(
        last.starts_with("  hotkeys"),
        "row is {last:?}, expected to start with `  hotkeys`"
    );
    assert!(
        last.ends_with("list shortcuts"),
        "row is {last:?}, expected description right-aligned at row end"
    );
    assert_eq!(
        last.chars().count(),
        48,
        "row is {last:?}, expected 48 columns wide"
    );
    assert_eq!(
        snapshot.lines[prompt_at - 1].trim_end(),
        "────────────────────────────────────────────────",
        "Phase 2 / G3 border row sits between dropdown and prompt"
    );
}

#[test]
fn closing_the_dropdown_gives_the_rows_back_to_the_transcript() {
    let dir = TempDir::new("app-close");
    let mut app = app_with(
        vec![SlashCommand::new("help").with_description("show help")],
        &dir,
    );
    app.messages_mut()
        .push(pi_tui::message::MessageItem::assistant("transcript body"));

    type_into(&mut app, "/h");
    assert!(app.render_snapshot(48, 12).lines.join("\n").contains('→'));

    app.step(pi_tui::InputEvent::Key(key(KeyCode::Esc)));
    let after = app.render_snapshot(48, 12).lines.join("\n");
    // No dropdown marker is left in the buffer after Esc.
    assert!(!after.contains('→'), "{after}");
    // `Esc` closed the list without rewriting the input. The composer
    // uses an REVERSED-block caret (`▍`) and no `> ` prefix (P9), so
    // the visible cell is the digit `/h▍`.
    assert!(after.contains("/h▍"), "{after}");
}

#[test]
fn dropdown_rows_are_opaque_over_the_transcript() {
    let dir = TempDir::new("app-opaque");
    let mut app = app_with(
        vec![SlashCommand::new("help").with_description("show help")],
        &dir,
    );
    // `render_snapshot` trims trailing cells, so a row that is *not* padded
    // to the editor width would still show the transcript's characters to
    // the right of the candidate. Six full-width rows guarantee the
    // dropdown lands on top of text.
    for _ in 0..6 {
        app.messages_mut()
            .push(pi_tui::message::MessageItem::assistant(
                "XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX",
            ));
    }

    type_into(&mut app, "/h");
    let snapshot = app.render_snapshot(48, 12);
    let row = snapshot
        .lines
        .iter()
        .find(|line| line.contains('→'))
        .expect("dropdown row");
    assert!(!row.contains('X'), "transcript bled through: {row:?}");
    assert!(row.contains("help"), "{row:?}");
}

// ---------------------------------------------------------------------------
// Composed provider chains (LUM-1448) — the Rust half of
// `ctx.ui.addAutocompleteProvider`.
// ---------------------------------------------------------------------------

/// A wrapper that answers only for its own `marker` token and delegates
/// everything else to the provider beneath it — upstream's `#1234` provider
/// shape (`current.getSuggestions(...)` / `current.applyCompletion(...)`).
#[derive(Debug)]
struct MarkerProvider {
    marker: char,
    values: Vec<&'static str>,
    triggers: Vec<char>,
    current: Arc<dyn AutocompleteProvider>,
}

impl MarkerProvider {
    fn matches(&self, lines: &[String], cursor_line: usize, cursor_col: usize) -> bool {
        let line = lines.get(cursor_line).map(String::as_str).unwrap_or("");
        let before = &line[..cursor_col.min(line.len())];
        before.trim_start().starts_with(self.marker)
    }
}

impl AutocompleteProvider for MarkerProvider {
    fn trigger_characters(&self) -> &[char] {
        &self.triggers
    }

    fn get_suggestions(
        &self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
        force: bool,
    ) -> Option<AutocompleteSuggestions> {
        if !self.matches(lines, cursor_line, cursor_col) {
            return self
                .current
                .get_suggestions(lines, cursor_line, cursor_col, force);
        }
        let line = lines.get(cursor_line).map(String::as_str).unwrap_or("");
        let before = &line[..cursor_col.min(line.len())];
        Some(AutocompleteSuggestions {
            items: self
                .values
                .iter()
                .map(|value| AutocompleteItem::new(*value, *value))
                .collect(),
            prefix: before.trim_start().to_string(),
        })
    }

    fn apply_completion(
        &self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
        item: &AutocompleteItem,
        prefix: &str,
    ) -> CompletionResult {
        if !self.matches(lines, cursor_line, cursor_col) {
            return self
                .current
                .apply_completion(lines, cursor_line, cursor_col, item, prefix);
        }
        let line = lines.get(cursor_line).cloned().unwrap_or_default();
        let head = &line[..cursor_col.saturating_sub(prefix.len())];
        CompletionResult {
            lines: vec![format!("{head}{}", item.value)],
            cursor_line,
            cursor_col: head.len() + item.value.len(),
        }
    }

    fn should_trigger_file_completion(
        &self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
    ) -> bool {
        if !self.matches(lines, cursor_line, cursor_col) {
            return self
                .current
                .should_trigger_file_completion(lines, cursor_line, cursor_col);
        }
        false
    }
}

fn marker_factory(
    marker: char,
    values: Vec<&'static str>,
    triggers: Vec<char>,
) -> AutocompleteProviderFactory {
    Arc::new(move |current: Arc<dyn AutocompleteProvider>| {
        Arc::new(MarkerProvider {
            marker,
            values: values.clone(),
            triggers: triggers.clone(),
            current,
        }) as Arc<dyn AutocompleteProvider>
    })
}

fn composed_editor() -> Editor {
    let mut editor = Editor::new();
    let base: Arc<dyn AutocompleteProvider> = Arc::new(CombinedAutocompleteProvider::new(
        vec![SlashCommand::new("help")],
        ".",
    ));
    let composed = compose_autocomplete_providers(
        base,
        &[
            marker_factory('#', vec!["#2983", "#2753"], vec!['#', '$']),
            marker_factory('$', vec!["$HOME"], vec!['$', '%']),
        ],
    );
    editor.set_autocomplete_provider(composed);
    editor
}

/// The editor's trigger table is the defaults plus the chain's deduplicated
/// triggers — upstream `[...new Set(triggerCharacters)]` over the wrappers
/// (`interactive-mode.ts:736-743`).
#[test]
fn composed_chain_installs_the_deduplicated_trigger_table() {
    let editor = composed_editor();
    assert_eq!(
        editor.autocomplete_trigger_characters(),
        &['@', '#', '$', '%']
    );
}

/// Re-installing a provider replaces the table instead of accumulating it,
/// so a rebuilt chain cannot leave a stale trigger behind (`setAutocomplete
/// TriggerCharacters` rebuilds from the defaults).
#[test]
fn reinstalling_a_provider_drops_stale_triggers() {
    let mut editor = composed_editor();
    assert!(editor.autocomplete_trigger_characters().contains(&'$'));
    editor.set_autocomplete_provider(Arc::new(CombinedAutocompleteProvider::new(
        vec![SlashCommand::new("help")],
        ".",
    )));
    assert_eq!(editor.autocomplete_trigger_characters(), &['@', '#']);
}

/// The `#` trigger the composed chain declares opens the dropdown with the
/// extension's candidates, and the base provider still answers `/`.
#[test]
fn composed_chain_serves_hash_candidates_and_keeps_slash_completion() {
    let mut editor = composed_editor();
    type_text(&mut editor, "#29");
    assert!(editor.is_showing_autocomplete(), "no `#` dropdown");
    assert_eq!(editor.autocomplete_items().len(), 2);
    assert_eq!(editor.autocomplete_prefix(), "#29");
    assert_eq!(editor.autocomplete_items()[0].value, "#2983");

    // Tab accepts the highlighted candidate through the wrapper's
    // `applyCompletion`.
    editor.handle_key(key(KeyCode::Tab));
    assert_eq!(editor.text(), "#2983");
    assert!(!editor.is_showing_autocomplete());

    // The base provider underneath still drives `/`.
    editor.set_text("");
    type_text(&mut editor, "/he");
    assert_eq!(editor.autocomplete_items()[0].value, "help");
}
