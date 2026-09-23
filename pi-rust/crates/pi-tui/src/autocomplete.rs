//! Autocomplete providers — command (`/`) and file (`@`) completion.
//!
//! A port of upstream `packages/tui/src/autocomplete.ts`. The module owns
//! the [`AutocompleteProvider`] abstraction and the
//! [`CombinedAutocompleteProvider`] that upstream's `editor.ts` drives: a
//! `/`-prefixed token completes slash-command names, an `@`-prefixed token
//! (or a bare path-like token, or an explicit Tab) completes paths relative
//! to a base directory.
//!
//! Unlike upstream, everything here is **synchronous**. Upstream's
//! `getSuggestions` is `async` because it shells out to `fd` with an
//! `AbortSignal`; the Rust editor is synchronous, so the provider returns
//! its result directly and the caller (the [`Editor`](crate::Editor))
//! cancels a stale request by discarding it. This is a deliberate,
//! documented deviation.
//!
//! The `@` fuzzy search does **not** require `fd`. Upstream shells out to
//! `fd` when it is installed and returns no `@` suggestions otherwise; the
//! Rust port always answers from a bounded [`std::fs`] directory walk, so
//! the feature is available on every machine and no new crate dependency
//! (or process spawn) is introduced. The walk skips the same
//! `DEFAULT_IGNORE_NAMES` directories as
//! `crates/pi-coding-agent/src/tools/mod_ignore.rs` (`.git`, `.pi`,
//! `node_modules`, `target`, `dist`, `build`) as a conservative stand-in
//! for `fd --hidden --exclude .git` plus `.gitignore` handling. It is
//! bounded by a result cap, a depth cap and a visited-directory cap so a
//! huge tree cannot stall a keystroke.
//!
//! Command completion filters the command list with [`crate::fuzzy`]
//! (`fuzzy_rank`, upstream `fuzzyFilter`). Upstream matches the command
//! *name* only; this port fuzzy-matches name plus description, which is a
//! superset — a description-only hit is surfaced too.
//!
//! Providers can also be **stacked**: an [`AutocompleteProviderFactory`]
//! wraps the provider underneath it and
//! [`compose_autocomplete_providers`] folds a list of factories onto a base,
//! reporting the chain's deduplicated trigger table — the Rust half of
//! upstream `setupAutocompleteProvider`
//! (`packages/coding-agent/src/modes/interactive/interactive-mode.ts:734-745`),
//! which is how `ctx.ui.addAutocompleteProvider` stacks an extension's
//! provider on top of the built-in one.
//!
//! ```
//! use pi_tui::autocomplete::{
//!     AutocompleteItem, AutocompleteProvider, CombinedAutocompleteProvider, SlashCommand,
//! };
//!
//! let provider = CombinedAutocompleteProvider::new(
//!     vec![SlashCommand::new("help").with_description("show help")],
//!     ".",
//! );
//! let lines = ["/he".to_string()];
//! let suggestions = provider
//!     .get_suggestions(&lines, 0, 3, false)
//!     .expect("a command candidate");
//! assert_eq!(suggestions.prefix, "/he");
//! assert_eq!(suggestions.items[0].value, "help");
//!
//! let applied = provider.apply_completion(&lines, 0, 3, &suggestions.items[0], &suggestions.prefix);
//! assert_eq!(applied.lines, vec!["/help ".to_string()]);
//! assert_eq!(applied.cursor_col, "/help ".len());
//! ```

use std::collections::VecDeque;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::fuzzy::fuzzy_rank;
use crate::selector::SelectListRow;

/// Characters that end a token when the provider scans backwards for the
/// token the cursor sits in (upstream `PATH_DELIMITERS`).
const PATH_DELIMITERS: [char; 5] = [' ', '\t', '"', '\'', '='];

/// Directory names the bounded walk never descends into. Mirrors
/// `crates/pi-coding-agent/src/tools/mod_ignore.rs::DEFAULT_IGNORE_NAMES`.
const IGNORED_DIR_NAMES: [&str; 6] = [".git", ".pi", "node_modules", "target", "dist", "build"];

/// Maximum number of entries a single bounded walk collects before it
/// stops, upstream's `--max-results 100`.
const MAX_WALK_RESULTS: usize = 100;

/// Maximum depth of the bounded walk (the base directory is depth `0`).
const MAX_WALK_DEPTH: usize = 12;

/// Maximum number of directory entries inspected during one walk, so a
/// pathological tree cannot stall a keystroke.
const MAX_WALK_VISITED: usize = 20_000;

/// How many scored entries survive the fuzzy `@` ranking, upstream's
/// `topEntries = scoredEntries.slice(0, 20)`.
const MAX_FUZZY_SUGGESTIONS: usize = 20;

/// One autocomplete candidate — upstream `AutocompleteItem`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutocompleteItem {
    /// Text inserted into the buffer when the item is accepted. Already
    /// carries the `@` / quote decoration the insert site needs.
    pub value: String,
    /// Human-readable label shown in the dropdown.
    pub label: String,
    /// Optional secondary text (a path, a command hint, …).
    pub description: Option<String>,
}

impl AutocompleteItem {
    /// Construct an item with a value and label.
    pub fn new(value: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            label: label.into(),
            description: None,
        }
    }

    /// Builder-style setter for the description.
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }
}

impl SelectListRow for AutocompleteItem {
    fn value(&self) -> &str {
        &self.value
    }

    fn label(&self) -> &str {
        &self.label
    }

    fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }
}

/// The candidate list plus the prefix it was computed for — upstream
/// `AutocompleteSuggestions`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutocompleteSuggestions {
    /// Candidates, best match first.
    pub items: Vec<AutocompleteItem>,
    /// The exact buffer prefix the items complete. Passed back to
    /// [`AutocompleteProvider::apply_completion`] unchanged.
    pub prefix: String,
}

/// Result of applying a completion — upstream's inline return type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionResult {
    /// Buffer lines after the completion.
    pub lines: Vec<String>,
    /// Cursor line after the completion.
    pub cursor_line: usize,
    /// Cursor column (byte offset) after the completion.
    pub cursor_col: usize,
}

/// Callback returning argument completions for a slash command. Receives
/// the text typed after the command name and returns the candidates, or
/// `None` when the command has nothing to offer.
pub type ArgumentCompletions = Arc<dyn Fn(&str) -> Option<Vec<AutocompleteItem>> + Send + Sync>;

/// A slash command that can be completed — upstream `SlashCommand`.
#[derive(Clone)]
pub struct SlashCommand {
    /// Command name, without the leading `/`.
    pub name: String,
    /// Description shown in the dropdown.
    pub description: Option<String>,
    /// Argument hint shown before the description (e.g. `<path>`).
    pub argument_hint: Option<String>,
    /// Optional argument completer.
    pub argument_completions: Option<ArgumentCompletions>,
}

impl fmt::Debug for SlashCommand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SlashCommand")
            .field("name", &self.name)
            .field("description", &self.description)
            .field("argument_hint", &self.argument_hint)
            .field(
                "argument_completions",
                &self.argument_completions.as_ref().map(|_| "<callback>"),
            )
            .finish()
    }
}

impl SlashCommand {
    /// Construct a command with just a name.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: None,
            argument_hint: None,
            argument_completions: None,
        }
    }

    /// Builder-style setter for the description.
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Builder-style setter for the argument hint.
    pub fn with_argument_hint(mut self, hint: impl Into<String>) -> Self {
        self.argument_hint = Some(hint.into());
        self
    }

    /// Builder-style setter for the argument completer.
    pub fn with_argument_completions(mut self, completions: ArgumentCompletions) -> Self {
        self.argument_completions = Some(completions);
        self
    }

    /// Description text shown in the dropdown, upstream's
    /// `hint ? (desc ? \`${hint} — ${desc}\` : hint) : desc`.
    fn display_description(&self) -> Option<String> {
        let description = self.description.as_deref().unwrap_or("");
        match (&self.argument_hint, description.is_empty()) {
            (Some(hint), false) => Some(format!("{hint} — {description}")),
            (Some(hint), true) => Some(hint.clone()),
            (None, false) => Some(description.to_string()),
            (None, true) => None,
        }
    }
}

/// Computes autocomplete candidates for the buffer at a cursor position.
///
/// Upstream's interface is `async` and signal-abortable; this port is
/// synchronous — see the module docs.
pub trait AutocompleteProvider: fmt::Debug + Send + Sync {
    /// Extra characters that should trigger this provider at a token
    /// boundary, on top of the editor's defaults (`@`, `#`).
    fn trigger_characters(&self) -> &[char] {
        &[]
    }

    /// Candidates for the buffer, or `None` when there is nothing to
    /// complete. `force` mirrors upstream's explicit Tab request: it skips
    /// the "does this look like a path / slash command" heuristics and
    /// always tries a file completion.
    fn get_suggestions(
        &self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
        force: bool,
    ) -> Option<AutocompleteSuggestions>;

    /// Apply `item` (computed for `prefix`) to the buffer and return the
    /// new text and cursor.
    fn apply_completion(
        &self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
        item: &AutocompleteItem,
        prefix: &str,
    ) -> CompletionResult;

    /// Whether an explicit Tab request may trigger a file completion —
    /// upstream `shouldTriggerFileCompletion`. Defaults to `true` when a
    /// provider does not override it.
    fn should_trigger_file_completion(
        &self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
    ) -> bool {
        let _ = (lines, cursor_line, cursor_col);
        true
    }
}

/// Wraps the current provider with additional behaviour — upstream
/// `AutocompleteProviderFactory`
/// (`packages/coding-agent/src/core/extensions/types.ts:125`,
/// `packages/coding-agent/src/modes/interactive/interactive-mode.ts:2450`).
///
/// The factory receives the provider it is stacking on top of — the base
/// [`CombinedAutocompleteProvider`] for the first wrapper, the previous
/// wrapper's provider afterwards — and returns the wrapper. That is exactly
/// upstream's chain shape, so `current.getSuggestions(...)` inside a wrapper
/// delegates to everything underneath it.
type AutocompleteProviderFactoryInner =
    dyn Fn(Arc<dyn AutocompleteProvider>) -> Arc<dyn AutocompleteProvider> + Send + Sync;

/// A boxed [`AutocompleteProviderFactoryInner`].
pub type AutocompleteProviderFactory = Arc<AutocompleteProviderFactoryInner>;

/// Outermost provider of a composed chain: forwards every call to the
/// provider underneath and reports the chain's deduplicated trigger table.
///
/// Upstream does the same by assigning
/// `provider.triggerCharacters = [...new Set(triggerCharacters)]` on the last
/// wrapper (`interactive-mode.ts:736-743`); because the Rust trait returns a
/// borrowed slice, the equivalent is this thin wrapper.
#[derive(Debug)]
pub struct TriggeredAutocompleteProvider {
    inner: Arc<dyn AutocompleteProvider>,
    trigger_characters: Vec<char>,
}

impl TriggeredAutocompleteProvider {
    /// Wrap `inner`, overriding the trigger table it reports.
    pub fn new(inner: Arc<dyn AutocompleteProvider>, trigger_characters: Vec<char>) -> Self {
        Self {
            inner,
            trigger_characters,
        }
    }

    /// The provider underneath.
    pub fn inner(&self) -> &Arc<dyn AutocompleteProvider> {
        &self.inner
    }
}

impl AutocompleteProvider for TriggeredAutocompleteProvider {
    fn trigger_characters(&self) -> &[char] {
        &self.trigger_characters
    }

    fn get_suggestions(
        &self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
        force: bool,
    ) -> Option<AutocompleteSuggestions> {
        self.inner
            .get_suggestions(lines, cursor_line, cursor_col, force)
    }

    fn apply_completion(
        &self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
        item: &AutocompleteItem,
        prefix: &str,
    ) -> CompletionResult {
        self.inner
            .apply_completion(lines, cursor_line, cursor_col, item, prefix)
    }

    fn should_trigger_file_completion(
        &self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
    ) -> bool {
        self.inner
            .should_trigger_file_completion(lines, cursor_line, cursor_col)
    }
}

/// Stack `factories` on top of `base`, in registration order, and report the
/// chain's deduplicated trigger table — the Rust counterpart of upstream's
/// `setupAutocompleteProvider`
/// (`packages/coding-agent/src/modes/interactive/interactive-mode.ts:734-745`).
///
/// Each factory is handed the provider it wraps (`current`), so the last
/// wrapper is the one the editor talks to. Trigger characters are collected
/// from every wrapper (not from `base`, which — like upstream's
/// `CombinedAutocompleteProvider` — declares none) and deduplicated in first
/// -seen order; when no factory contributes any, `base`'s own table is left
/// untouched and `base` is returned unchanged.
pub fn compose_autocomplete_providers(
    base: Arc<dyn AutocompleteProvider>,
    factories: &[AutocompleteProviderFactory],
) -> Arc<dyn AutocompleteProvider> {
    let mut provider = base;
    let mut trigger_characters: Vec<char> = Vec::new();
    for factory in factories {
        provider = factory(provider);
        for trigger in provider.trigger_characters() {
            if !trigger_characters.contains(trigger) {
                trigger_characters.push(*trigger);
            }
        }
    }
    if trigger_characters.is_empty() {
        return provider;
    }
    Arc::new(TriggeredAutocompleteProvider::new(
        provider,
        trigger_characters,
    ))
}

/// The `CombinedAutocompleteProvider` port: slash commands plus file
/// paths, dispatched off the token under the cursor.
#[derive(Debug, Clone)]
pub struct CombinedAutocompleteProvider {
    commands: Vec<SlashCommand>,
    base_path: PathBuf,
}

impl CombinedAutocompleteProvider {
    /// Construct a provider completing `commands` and paths relative to
    /// `base_path`.
    pub fn new(commands: Vec<SlashCommand>, base_path: impl Into<PathBuf>) -> Self {
        Self {
            commands,
            base_path: base_path.into(),
        }
    }

    /// Borrow the configured commands.
    pub fn commands(&self) -> &[SlashCommand] {
        &self.commands
    }

    /// The base directory file paths are resolved against.
    pub fn base_path(&self) -> &Path {
        &self.base_path
    }
}

impl AutocompleteProvider for CombinedAutocompleteProvider {
    fn get_suggestions(
        &self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
        force: bool,
    ) -> Option<AutocompleteSuggestions> {
        let current_line = lines.get(cursor_line).map(String::as_str).unwrap_or("");
        let text_before_cursor = safe_prefix(current_line, cursor_col);

        // `@` fuzzy file search.
        if let Some(at_prefix) = extract_at_prefix(text_before_cursor) {
            let (raw_prefix, _, is_quoted_prefix) = parse_path_prefix(&at_prefix);
            let suggestions = self.get_fuzzy_file_suggestions(&raw_prefix, is_quoted_prefix);
            if suggestions.is_empty() {
                return None;
            }
            return Some(AutocompleteSuggestions {
                items: suggestions,
                prefix: at_prefix,
            });
        }

        // Slash commands (name completion, then argument completion).
        if !force && text_before_cursor.starts_with('/') {
            if let Some(space_index) = text_before_cursor.find(' ') {
                let command_name = &text_before_cursor[1..space_index];
                let argument_text = &text_before_cursor[space_index + 1..];
                let command = self.commands.iter().find(|c| c.name == command_name)?;
                let completions = command.argument_completions.as_ref()?;
                let items = completions(argument_text)?;
                if items.is_empty() {
                    return None;
                }
                return Some(AutocompleteSuggestions {
                    items,
                    prefix: argument_text.to_string(),
                });
            }
            return self.command_name_suggestions(text_before_cursor);
        }

        // A path-like token, or a forced Tab.
        let path_prefix = extract_path_prefix(text_before_cursor, force)?;
        let suggestions = self.get_file_suggestions(&path_prefix);
        if suggestions.is_empty() {
            return None;
        }
        Some(AutocompleteSuggestions {
            items: suggestions,
            prefix: path_prefix,
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
        let current_line = lines.get(cursor_line).map(String::as_str).unwrap_or("");
        let cursor_col = cursor_col.min(current_line.len());
        let before_prefix_len = cursor_col.saturating_sub(prefix.len());
        let before_prefix = safe_prefix(current_line, before_prefix_len);
        let after_cursor = &current_line[cursor_col..];
        let is_quoted_prefix = prefix.starts_with('"') || prefix.starts_with("@\"");
        let has_leading_quote_after_cursor = after_cursor.starts_with('"');
        let has_trailing_quote_in_item = item.value.ends_with('"');
        let adjusted_after_cursor =
            if is_quoted_prefix && has_trailing_quote_in_item && has_leading_quote_after_cursor {
                &after_cursor[1..]
            } else {
                after_cursor
            };

        // Slash command name completion: `/` at the start of the line with
        // no path separator after it.
        let is_slash_command = prefix.starts_with('/')
            && before_prefix.trim().is_empty()
            && !prefix[1..].contains('/');
        if is_slash_command {
            let new_line = format!("{before_prefix}/{} {adjusted_after_cursor}", item.value);
            return CompletionResult {
                lines: replace_line(lines, cursor_line, new_line),
                cursor_line,
                cursor_col: before_prefix.len() + item.value.len() + 2,
            };
        }

        // `@` file attachment: directories keep the cursor after the slash
        // (and no trailing space) so the user can keep completing.
        if prefix.starts_with('@') {
            let is_directory = item.label.ends_with('/');
            let suffix = if is_directory { "" } else { " " };
            let new_line = format!(
                "{before_prefix}{}{suffix}{adjusted_after_cursor}",
                item.value
            );
            let cursor_offset = quote_offset(&item.value, is_directory);
            return CompletionResult {
                lines: replace_line(lines, cursor_line, new_line),
                cursor_line,
                cursor_col: before_prefix.len() + cursor_offset + suffix.len(),
            };
        }

        // A slash-command argument completion (`/command arg`).
        let text_before_cursor = safe_prefix(current_line, cursor_col);
        let is_directory = item.label.ends_with('/');
        let cursor_offset = quote_offset(&item.value, is_directory);
        if text_before_cursor.contains('/') && text_before_cursor.contains(' ') {
            let new_line = format!("{before_prefix}{}{adjusted_after_cursor}", item.value);
            return CompletionResult {
                lines: replace_line(lines, cursor_line, new_line),
                cursor_line,
                cursor_col: before_prefix.len() + cursor_offset,
            };
        }

        // A plain path completion.
        let new_line = format!("{before_prefix}{}{adjusted_after_cursor}", item.value);
        CompletionResult {
            lines: replace_line(lines, cursor_line, new_line),
            cursor_line,
            cursor_col: before_prefix.len() + cursor_offset,
        }
    }

    fn should_trigger_file_completion(
        &self,
        lines: &[String],
        cursor_line: usize,
        cursor_col: usize,
    ) -> bool {
        let current_line = lines.get(cursor_line).map(String::as_str).unwrap_or("");
        let text_before_cursor = safe_prefix(current_line, cursor_col);
        let trimmed = text_before_cursor.trim();
        // De Morgan form so `clippy::nonminimal_bool` stays quiet:
        // `!(a && b)` is exactly `!a || b_negated`.
        !trimmed.starts_with('/') || trimmed.contains(' ')
    }
}

impl CombinedAutocompleteProvider {
    /// Candidates for a `/`-prefixed command name.
    ///
    /// Upstream fuzzy-filters on the command name; this port matches the
    /// name plus description (a superset) and ranks with
    /// [`crate::fuzzy::fuzzy_rank`].
    fn command_name_suggestions(
        &self,
        text_before_cursor: &str,
    ) -> Option<AutocompleteSuggestions> {
        let prefix = &text_before_cursor[1..];
        let entries: Vec<CommandEntry> = self
            .commands
            .iter()
            .map(|command| CommandEntry {
                value: command.name.clone(),
                label: command.name.clone(),
                description: command.display_description(),
                search_text: match &command.display_description() {
                    Some(description) => format!("{} {description}", command.name),
                    None => command.name.clone(),
                },
            })
            .collect();
        let ranked = fuzzy_rank(&entries, prefix, |entry: &CommandEntry| {
            entry.search_text.clone()
        });
        if ranked.is_empty() {
            return None;
        }
        let items = ranked
            .into_iter()
            .map(|idx| {
                let entry = &entries[idx];
                AutocompleteItem {
                    value: entry.value.clone(),
                    label: entry.label.clone(),
                    description: entry.description.clone(),
                }
            })
            .collect();
        Some(AutocompleteSuggestions {
            items,
            prefix: text_before_cursor.to_string(),
        })
    }

    /// `@` fuzzy file suggestions: walk the base directory (scoped to the
    /// directory part of `query` when it contains a `/`), score every
    /// entry and keep the best [`MAX_FUZZY_SUGGESTIONS`].
    fn get_fuzzy_file_suggestions(
        &self,
        query: &str,
        is_quoted_prefix: bool,
    ) -> Vec<AutocompleteItem> {
        let scoped = self.resolve_scoped_fuzzy_query(query);
        let (base_dir, fd_query, display_base) = match scoped {
            Some(scoped) => (scoped.base_dir, scoped.query, Some(scoped.display_base)),
            None => (self.base_path.clone(), query.to_string(), None),
        };
        if !base_dir.is_dir() {
            return Vec::new();
        }

        let mut scored: Vec<ScoredEntry> = walk_directory(&base_dir)
            .into_iter()
            .filter_map(|entry| {
                let score = if fd_query.is_empty() {
                    1.0
                } else {
                    score_entry(&entry.path, &fd_query, entry.is_directory)
                };
                if score <= 0.0 {
                    return None;
                }
                let depth = entry
                    .path
                    .split('/')
                    .filter(|part| !part.is_empty())
                    .count();
                Some(ScoredEntry {
                    path: entry.path,
                    is_directory: entry.is_directory,
                    score,
                    depth,
                })
            })
            .collect();

        scored.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then_with(|| a.depth.cmp(&b.depth))
                .then_with(|| a.path.len().cmp(&b.path.len()))
                .then_with(|| a.path.cmp(&b.path))
        });

        scored
            .into_iter()
            .take(MAX_FUZZY_SUGGESTIONS)
            .map(|entry| {
                let display_path = match &display_base {
                    Some(display_base) => scoped_path_for_display(display_base, &entry.path),
                    None => entry.path.clone(),
                };
                let entry_name = Path::new(&entry.path)
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| entry.path.clone());
                let completion_path = if entry.is_directory {
                    format!("{display_path}/")
                } else {
                    display_path.clone()
                };
                let value = build_completion_value(&completion_path, true, is_quoted_prefix);
                AutocompleteItem {
                    value,
                    label: format!("{entry_name}{}", if entry.is_directory { "/" } else { "" }),
                    description: Some(display_path),
                }
            })
            .collect()
    }

    /// Resolve the directory part of a `@` query that contains a `/`,
    /// upstream `resolveScopedFuzzyQuery`. Returns `None` when the query is
    /// unscoped or the directory does not exist.
    fn resolve_scoped_fuzzy_query(&self, raw_query: &str) -> Option<ScopedQuery> {
        let normalized_query = to_display_path(raw_query);
        let slash_index = normalized_query.rfind('/')?;
        let display_base = normalized_query[..=slash_index].to_string();
        let query = normalized_query[slash_index + 1..].to_string();

        let base_dir = if display_base.starts_with("~/") {
            PathBuf::from(expand_home_path(&display_base))
        } else if display_base.starts_with('/') {
            PathBuf::from(&display_base)
        } else {
            self.base_path.join(&display_base)
        };
        if !base_dir.is_dir() {
            return None;
        }
        Some(ScopedQuery {
            base_dir,
            query,
            display_base,
        })
    }

    /// Direct (non-fuzzy) file suggestions for a path prefix, upstream
    /// `getFileSuggestions`. Lists one directory and prefix-matches its
    /// entries case-insensitively.
    fn get_file_suggestions(&self, prefix: &str) -> Vec<AutocompleteItem> {
        let (raw_prefix, is_at_prefix, is_quoted_prefix) = parse_path_prefix(prefix);
        let mut expanded_prefix = raw_prefix.clone();
        if expanded_prefix.starts_with('~') {
            expanded_prefix = expand_home_path(&expanded_prefix);
        }

        let is_root_prefix = raw_prefix.is_empty()
            || raw_prefix == "./"
            || raw_prefix == "../"
            || raw_prefix == "~"
            || raw_prefix == "~/"
            || raw_prefix == "/"
            || (is_at_prefix && raw_prefix.is_empty());

        let (search_dir, search_prefix) = if is_root_prefix || raw_prefix.ends_with('/') {
            let dir = if raw_prefix.starts_with('~') || expanded_prefix.starts_with('/') {
                PathBuf::from(&expanded_prefix)
            } else {
                self.base_path.join(&expanded_prefix)
            };
            (dir, String::new())
        } else {
            let expanded_path = Path::new(&expanded_prefix);
            let dir = if raw_prefix.starts_with('~') || expanded_prefix.starts_with('/') {
                expanded_path
                    .parent()
                    .map(PathBuf::from)
                    .unwrap_or_default()
            } else {
                match expanded_path.parent() {
                    Some(parent) if !parent.as_os_str().is_empty() => self.base_path.join(parent),
                    _ => self.base_path.clone(),
                }
            };
            let file = expanded_path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            (dir, file)
        };

        let Ok(read_dir) = std::fs::read_dir(&search_dir) else {
            return Vec::new();
        };
        let lower_prefix = search_prefix.to_lowercase();
        let mut suggestions: Vec<AutocompleteItem> = Vec::new();
        for entry in read_dir.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.to_lowercase().starts_with(&lower_prefix) {
                continue;
            }
            let is_directory = is_dir_entry(&entry);
            let relative_path = build_relative_path(&raw_prefix, &name);
            let relative_path = to_display_path(&relative_path);
            let path_value = if is_directory {
                format!("{relative_path}/")
            } else {
                relative_path
            };
            let value = build_completion_value(&path_value, is_at_prefix, is_quoted_prefix);
            suggestions.push(AutocompleteItem {
                value,
                label: format!("{name}{}", if is_directory { "/" } else { "" }),
                description: None,
            });
        }

        // Directories first, then case-insensitive by label — upstream's
        // `localeCompare` ordering.
        suggestions.sort_by(|a, b| {
            let a_dir = a.label.ends_with('/');
            let b_dir = b.label.ends_with('/');
            b_dir
                .cmp(&a_dir)
                .then_with(|| a.label.to_lowercase().cmp(&b.label.to_lowercase()))
                .then_with(|| a.label.cmp(&b.label))
        });
        suggestions
    }
}

/// Command projection used by [`fuzzy_rank`].
#[derive(Debug)]
struct CommandEntry {
    value: String,
    label: String,
    description: Option<String>,
    search_text: String,
}

/// A scoped `@` query: `display_base` + `query` relative to `base_dir`.
#[derive(Debug)]
struct ScopedQuery {
    base_dir: PathBuf,
    query: String,
    display_base: String,
}

/// One entry produced by [`walk_directory`].
#[derive(Debug, Clone, PartialEq, Eq)]
struct WalkEntry {
    /// Path relative to the walk root, `/`-separated, no trailing slash.
    path: String,
    is_directory: bool,
}

/// An entry with its fuzzy score and depth.
#[derive(Debug)]
struct ScoredEntry {
    path: String,
    is_directory: bool,
    score: f64,
    depth: usize,
}

/// Score an entry against the `@` query, upstream `scoreEntry` (higher is
/// better; `0` means "does not match").
fn score_entry(file_path: &str, query: &str, is_directory: bool) -> f64 {
    let file_name = Path::new(file_path)
        .file_name()
        .map(|name| name.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let lower_query = query.to_lowercase();

    let mut score = if file_name == lower_query {
        100
    } else if file_name.starts_with(&lower_query) {
        80
    } else if file_name.contains(&lower_query) {
        50
    } else if file_path.to_lowercase().contains(&lower_query) {
        30
    } else {
        0
    };
    if is_directory && score > 0 {
        score += 10;
    }
    f64::from(score)
}

/// Bounded breadth-first directory walk. Returns `/`-separated paths
/// relative to `base_dir`, hidden entries included, ignored directories
/// skipped. Symlinked directories are listed but not descended into, so a
/// link cycle cannot loop the walk.
fn walk_directory(base_dir: &Path) -> Vec<WalkEntry> {
    let mut out: Vec<WalkEntry> = Vec::new();
    let mut queue: VecDeque<(PathBuf, usize)> = VecDeque::new();
    queue.push_back((base_dir.to_path_buf(), 0usize));
    let mut visited = 0usize;

    while let Some((dir, depth)) = queue.pop_front() {
        if out.len() >= MAX_WALK_RESULTS {
            break;
        }
        let Ok(read_dir) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut entries: Vec<std::fs::DirEntry> = read_dir.flatten().collect();
        // Deterministic order; the final ranking breaks ties by path anyway.
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            visited += 1;
            if visited > MAX_WALK_VISITED {
                return out;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let is_directory = is_dir_entry(&entry);
            if is_directory && IGNORED_DIR_NAMES.contains(&name.as_str()) {
                continue;
            }
            let relative_dir = dir.strip_prefix(base_dir).unwrap_or(Path::new(""));
            let relative_path = join_display(relative_dir, &name);
            out.push(WalkEntry {
                path: relative_path,
                is_directory,
            });
            if is_directory && depth < MAX_WALK_DEPTH && !is_symlink(&entry) {
                queue.push_back((entry.path(), depth + 1));
            }
            if out.len() >= MAX_WALK_RESULTS {
                break;
            }
        }
    }
    out
}

/// `true` when the directory entry is a directory (following a symlink
/// when needed).
fn is_dir_entry(entry: &std::fs::DirEntry) -> bool {
    match entry.file_type() {
        Ok(file_type) if file_type.is_dir() => true,
        Ok(file_type) if file_type.is_symlink() => std::fs::metadata(entry.path())
            .map(|meta| meta.is_dir())
            .unwrap_or(false),
        _ => false,
    }
}

/// `true` when the directory entry itself is a symlink.
fn is_symlink(entry: &std::fs::DirEntry) -> bool {
    entry
        .file_type()
        .map(|file_type| file_type.is_symlink())
        .unwrap_or(false)
}

/// Join a relative directory (possibly empty) and an entry name using `/`.
fn join_display(relative_dir: &Path, name: &str) -> String {
    let dir = to_display_path(&relative_dir.to_string_lossy());
    if dir.is_empty() || dir == "." {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}

/// The token under the cursor when it starts with `@`, else `None`.
fn extract_at_prefix(text: &str) -> Option<String> {
    if let Some(quoted) = extract_quoted_prefix(text) {
        if quoted.starts_with("@\"") {
            return Some(quoted);
        }
    }
    let token_start = find_last_delimiter(text).map(|idx| idx + 1).unwrap_or(0);
    if text[token_start..].starts_with('@') {
        Some(text[token_start..].to_string())
    } else {
        None
    }
}

/// The path-like token under the cursor, upstream `extractPathPrefix`.
/// `force_extract` (Tab) always returns the token, heuristics otherwise.
fn extract_path_prefix(text: &str, force_extract: bool) -> Option<String> {
    if let Some(quoted) = extract_quoted_prefix(text) {
        return Some(quoted);
    }
    let path_prefix = match find_last_delimiter(text) {
        Some(idx) => &text[idx + 1..],
        None => text,
    };
    if force_extract {
        return Some(path_prefix.to_string());
    }
    if path_prefix.contains('/') || path_prefix.starts_with('.') || path_prefix.starts_with("~/") {
        return Some(path_prefix.to_string());
    }
    if path_prefix.is_empty() && text.ends_with(' ') {
        return Some(path_prefix.to_string());
    }
    None
}

/// The still-open quoted token, upstream `extractQuotedPrefix`.
fn extract_quoted_prefix(text: &str) -> Option<String> {
    let quote_start = find_unclosed_quote_start(text)?;
    if quote_start > 0 {
        let previous = text[..quote_start].chars().next_back();
        if previous == Some('@') {
            if !is_token_start(text, quote_start - 1) {
                return None;
            }
            return Some(text[quote_start - 1..].to_string());
        }
    }
    if !is_token_start(text, quote_start) {
        return None;
    }
    Some(text[quote_start..].to_string())
}

/// Split `prefix` into the raw path, whether it carried an `@`, and
/// whether it was quoted — upstream `parsePathPrefix`.
fn parse_path_prefix(prefix: &str) -> (String, bool, bool) {
    if let Some(rest) = prefix.strip_prefix("@\"") {
        (rest.to_string(), true, true)
    } else if let Some(rest) = prefix.strip_prefix('"') {
        (rest.to_string(), false, true)
    } else if let Some(rest) = prefix.strip_prefix('@') {
        (rest.to_string(), true, false)
    } else {
        (prefix.to_string(), false, false)
    }
}

/// The inserted completion value, quoting paths with spaces — upstream
/// `buildCompletionValue`.
fn build_completion_value(path: &str, is_at_prefix: bool, is_quoted_prefix: bool) -> String {
    let needs_quotes = is_quoted_prefix || path.contains(' ');
    let prefix = if is_at_prefix { "@" } else { "" };
    if !needs_quotes {
        return format!("{prefix}{path}");
    }
    format!("{prefix}\"{path}\"")
}

/// Cursor offset into `value` for a completion: a quoted directory keeps
/// the cursor before the closing quote.
fn quote_offset(value: &str, is_directory: bool) -> usize {
    if is_directory && value.ends_with('"') {
        value.len() - 1
    } else {
        value.len()
    }
}

/// Replace one line in a cloned buffer — upstream builds a fresh array.
fn replace_line(lines: &[String], cursor_line: usize, new_line: String) -> Vec<String> {
    let mut new_lines = lines.to_vec();
    if cursor_line < new_lines.len() {
        new_lines[cursor_line] = new_line;
    } else {
        new_lines.push(new_line);
    }
    new_lines
}

/// Byte index of the last path delimiter, upstream `findLastDelimiter`.
fn find_last_delimiter(text: &str) -> Option<usize> {
    text.char_indices()
        .filter(|(_, ch)| PATH_DELIMITERS.contains(ch))
        .map(|(idx, _)| idx)
        .next_back()
}

/// Byte index of the unmatched opening `"`, upstream
/// `findUnclosedQuoteStart`.
fn find_unclosed_quote_start(text: &str) -> Option<usize> {
    let mut in_quotes = false;
    let mut quote_start = None;
    for (idx, ch) in text.char_indices() {
        if ch == '"' {
            in_quotes = !in_quotes;
            if in_quotes {
                quote_start = Some(idx);
            }
        }
    }
    if in_quotes {
        quote_start
    } else {
        None
    }
}

/// `true` when the byte index starts a token, upstream `isTokenStart`.
fn is_token_start(text: &str, index: usize) -> bool {
    if index == 0 {
        return true;
    }
    text[..index]
        .chars()
        .next_back()
        .map(|ch| PATH_DELIMITERS.contains(&ch))
        .unwrap_or(true)
}

/// Expand a leading `~/` (or a bare `~`) to the home directory, upstream
/// `expandHomePath`.
fn expand_home_path(path: &str) -> String {
    let Some(home) = home_dir() else {
        return path.to_string();
    };
    let home = to_display_path(&home.to_string_lossy());
    if let Some(rest) = path.strip_prefix("~/") {
        let expanded = format!("{home}/{rest}");
        return expanded;
    }
    if path == "~" {
        return home;
    }
    path.to_string()
}

/// The user's home directory (`HOME`, falling back to `USERPROFILE`).
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
}

/// Reconstruct the display path of a relative entry under a scoped base,
/// upstream `scopedPathForDisplay`.
fn scoped_path_for_display(display_base: &str, relative_path: &str) -> String {
    let normalized = to_display_path(relative_path);
    if display_base == "/" {
        format!("/{normalized}")
    } else {
        format!("{}{normalized}", to_display_path(display_base))
    }
}

/// Build the display path for one directory entry, upstream's branchy
/// `relativePath` construction inside `getFileSuggestions`.
fn build_relative_path(display_prefix: &str, name: &str) -> String {
    if display_prefix.ends_with('/') {
        return format!("{display_prefix}{name}");
    }
    if display_prefix.contains('/') {
        if let Some(home_relative) = display_prefix.strip_prefix("~/") {
            let dir = Path::new(home_relative)
                .parent()
                .map(|parent| to_display_path(&parent.to_string_lossy()))
                .unwrap_or_default();
            if dir.is_empty() || dir == "." {
                return format!("~/{name}");
            }
            return format!("~/{dir}/{name}");
        }
        if display_prefix.starts_with('/') {
            let dir = Path::new(display_prefix)
                .parent()
                .map(|parent| to_display_path(&parent.to_string_lossy()))
                .unwrap_or_default();
            if dir.is_empty() || dir == "/" {
                return format!("/{name}");
            }
            return format!("{dir}/{name}");
        }
        let dir = Path::new(display_prefix)
            .parent()
            .map(|parent| to_display_path(&parent.to_string_lossy()))
            .unwrap_or_default();
        let mut relative = if dir.is_empty() || dir == "." {
            name.to_string()
        } else {
            format!("{dir}/{name}")
        };
        if display_prefix.starts_with("./") && !relative.starts_with("./") {
            relative = format!("./{relative}");
        }
        return relative;
    }
    if display_prefix.starts_with('~') {
        format!("~/{name}")
    } else {
        name.to_string()
    }
}

/// `\` → `/`, upstream `toDisplayPath`.
fn to_display_path(value: &str) -> String {
    value.replace('\\', "/")
}

/// `text[..col]`, clamped to the line and to a UTF-8 char boundary.
fn safe_prefix(text: &str, col: usize) -> &str {
    let mut end = col.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn lines(text: &str) -> Vec<String> {
        vec![text.to_string()]
    }

    #[test]
    fn command_completion_matches_name_and_description() {
        let provider = CombinedAutocompleteProvider::new(
            vec![
                SlashCommand::new("help").with_description("show this help text"),
                SlashCommand::new("clear").with_description("wipe the message view"),
                SlashCommand::new("render").with_description("redraw the view"),
            ],
            ".",
        );
        let suggestions = provider.get_suggestions(&lines("/"), 0, 1, false).unwrap();
        assert_eq!(suggestions.prefix, "/");
        assert_eq!(suggestions.items.len(), 3);

        // Name match.
        let suggestions = provider
            .get_suggestions(&lines("/cl"), 0, 3, false)
            .unwrap();
        assert_eq!(suggestions.items.len(), 1);
        assert_eq!(suggestions.items[0].value, "clear");
        assert_eq!(
            suggestions.items[0].description.as_deref(),
            Some("wipe the message view")
        );

        // Description-only match also surfaces the command.
        let suggestions = provider
            .get_suggestions(&lines("/message"), 0, 8, false)
            .unwrap();
        assert_eq!(suggestions.items[0].value, "clear");
    }

    #[test]
    fn command_completion_ranks_exact_match_first() {
        let provider = CombinedAutocompleteProvider::new(
            vec![
                SlashCommand::new("app"),
                SlashCommand::new("a_p_p"),
                SlashCommand::new("application"),
            ],
            ".",
        );
        let suggestions = provider
            .get_suggestions(&lines("/app"), 0, 4, false)
            .unwrap();
        assert_eq!(suggestions.items[0].value, "app");
    }

    #[test]
    fn command_argument_completion_uses_the_argument_prefix() {
        let completer: ArgumentCompletions = Arc::new(|argument: &str| {
            Some(vec![AutocompleteItem::new(
                format!("{argument}-one"),
                "one",
            )])
        });
        let provider = CombinedAutocompleteProvider::new(
            vec![SlashCommand::new("trust").with_argument_completions(completer)],
            ".",
        );
        let suggestions = provider
            .get_suggestions(&lines("/trust ye"), 0, 9, false)
            .unwrap();
        assert_eq!(suggestions.prefix, "ye");
        assert_eq!(suggestions.items[0].value, "ye-one");

        let result = provider.apply_completion(
            &lines("/trust ye"),
            0,
            9,
            &suggestions.items[0],
            &suggestions.prefix,
        );
        assert_eq!(result.lines, vec!["/trust ye-one".to_string()]);
        assert_eq!(result.cursor_col, "/trust ye-one".len());
    }

    #[test]
    fn apply_completion_rewrites_the_command_name_and_adds_a_space() {
        let provider = CombinedAutocompleteProvider::new(vec![SlashCommand::new("clear")], ".");
        let item = AutocompleteItem::new("clear", "clear");
        let result = provider.apply_completion(&lines("/cl"), 0, 3, &item, "/cl");
        assert_eq!(result.lines, vec!["/clear ".to_string()]);
        assert_eq!(result.cursor_col, 7);
    }

    #[test]
    fn token_boundaries_gate_the_at_trigger() {
        assert_eq!(extract_at_prefix("@src").as_deref(), Some("@src"));
        assert_eq!(extract_at_prefix("hello @src").as_deref(), Some("@src"));
        assert_eq!(extract_at_prefix("a@src"), None);
        assert_eq!(extract_at_prefix("\"@src").as_deref(), Some("@src"));
        assert_eq!(extract_at_prefix("@\"src").as_deref(), Some("@\"src"));
        assert_eq!(extract_at_prefix("#src"), None);
    }

    #[test]
    fn should_trigger_file_completion_skips_slash_commands() {
        let provider = CombinedAutocompleteProvider::new(Vec::new(), ".");
        assert!(!provider.should_trigger_file_completion(&lines("/model"), 0, 6));
        assert!(provider.should_trigger_file_completion(&lines("hello "), 0, 6));
        assert!(provider.should_trigger_file_completion(&lines("/model src/"), 0, 10));
    }

    #[test]
    fn parse_path_prefix_variants() {
        assert_eq!(
            parse_path_prefix("@src/main.rs"),
            ("src/main.rs".to_string(), true, false)
        );
        assert_eq!(
            parse_path_prefix("@\"src/main.rs"),
            ("src/main.rs".to_string(), true, true)
        );
        assert_eq!(
            parse_path_prefix("\"src/main.rs"),
            ("src/main.rs".to_string(), false, true)
        );
        assert_eq!(
            parse_path_prefix("src/main.rs"),
            ("src/main.rs".to_string(), false, false)
        );
    }

    /// A wrapper that answers only for its own trigger token and delegates
    /// everything else to the provider it wraps — the shape upstream's
    /// `#1234` example uses.
    #[derive(Debug)]
    struct MarkerProvider {
        marker: char,
        value: &'static str,
        triggers: Vec<char>,
        seen_current: Arc<Mutex<Vec<&'static str>>>,
    }

    impl AutocompleteProvider for MarkerProvider {
        fn trigger_characters(&self) -> &[char] {
            &self.triggers
        }

        fn get_suggestions(
            &self,
            _lines: &[String],
            _cursor_line: usize,
            _cursor_col: usize,
            _force: bool,
        ) -> Option<AutocompleteSuggestions> {
            self.seen_current.lock().unwrap().push(self.value);
            Some(AutocompleteSuggestions {
                items: vec![AutocompleteItem::new(self.value, self.value)],
                prefix: self.marker.to_string(),
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
            let text = lines.get(cursor_line).cloned().unwrap_or_default();
            let head = &text[..cursor_col.saturating_sub(prefix.len())];
            CompletionResult {
                lines: vec![format!("{head}{}", item.value)],
                cursor_line,
                cursor_col: head.len() + item.value.len(),
            }
        }

        fn should_trigger_file_completion(
            &self,
            _lines: &[String],
            _cursor_line: usize,
            _cursor_col: usize,
        ) -> bool {
            false
        }
    }

    fn marker_factory(
        marker: char,
        value: &'static str,
        triggers: Vec<char>,
        log: Arc<Mutex<Vec<&'static str>>>,
    ) -> AutocompleteProviderFactory {
        let factory = move |current: Arc<dyn AutocompleteProvider>| {
            log.lock().unwrap().push(value);
            let _ = &current;
            Arc::new(MarkerProvider {
                marker,
                value,
                triggers: triggers.clone(),
                seen_current: Arc::new(Mutex::new(Vec::new())),
            }) as Arc<dyn AutocompleteProvider>
        };
        Arc::new(factory)
    }

    #[test]
    fn compose_without_factories_returns_the_base_unchanged() {
        let base: Arc<dyn AutocompleteProvider> =
            Arc::new(CombinedAutocompleteProvider::new(Vec::new(), "."));
        let composed = compose_autocomplete_providers(base, &[]);
        assert!(composed.trigger_characters().is_empty());
    }

    #[test]
    fn compose_applies_wrappers_in_order_and_dedups_triggers() {
        let base: Arc<dyn AutocompleteProvider> =
            Arc::new(CombinedAutocompleteProvider::new(Vec::new(), "."));
        let log = Arc::new(Mutex::new(Vec::new()));
        let factories = vec![
            marker_factory('#', "first", vec!['#', '$'], log.clone()),
            marker_factory('$', "second", vec!['$'], log.clone()),
        ];
        let composed = compose_autocomplete_providers(base, &factories);
        // Every factory ran, in registration order.
        assert_eq!(log.lock().unwrap().as_slice(), &["first", "second"]);
        // First-seen dedup over the chain's wrappers, exactly upstream's
        // `[...new Set(triggerCharacters)]`.
        assert_eq!(composed.trigger_characters(), &['#', '$']);

        // The outermost wrapper is the one that answers.
        let lines = vec!["#1".to_string()];
        let suggestions = composed.get_suggestions(&lines, 0, 2, false).unwrap();
        assert_eq!(suggestions.items[0].value, "second");
        let applied = composed.apply_completion(&lines, 0, 2, &suggestions.items[0], "#1");
        assert_eq!(applied.lines, vec!["second".to_string()]);
        assert!(!composed.should_trigger_file_completion(&lines, 0, 2));
    }

    #[test]
    fn wrapper_gets_the_provider_underneath_as_current() {
        let base: Arc<dyn AutocompleteProvider> = Arc::new(CombinedAutocompleteProvider::new(
            vec![SlashCommand::new("help")],
            ".",
        ));
        let base_for_assert = base.clone();
        let captured: Arc<Mutex<Option<Arc<dyn AutocompleteProvider>>>> =
            Arc::new(Mutex::new(None));
        let captured_in_factory = captured.clone();
        let factory: AutocompleteProviderFactory = Arc::new(move |current| {
            *captured_in_factory.lock().unwrap() = Some(current.clone());
            current
        });
        let composed = compose_autocomplete_providers(base, &[factory]);
        let seen = captured.lock().unwrap().clone().unwrap();
        assert!(Arc::ptr_eq(&seen, &base_for_assert));
        // The factory returned `current` unchanged, so the base's own
        // `/`-completion still works through the composed provider.
        let lines = vec!["/he".to_string()];
        let suggestions = composed.get_suggestions(&lines, 0, 3, false).unwrap();
        assert_eq!(suggestions.items[0].value, "help");
    }

    #[test]
    fn triggered_provider_reports_the_chain_table() {
        let inner: Arc<dyn AutocompleteProvider> = Arc::new(CombinedAutocompleteProvider::new(
            vec![SlashCommand::new("help")],
            ".",
        ));
        let provider = TriggeredAutocompleteProvider::new(inner, vec!['$', '#']);
        assert_eq!(provider.trigger_characters(), &['$', '#']);
        let lines = vec!["/he".to_string()];
        assert_eq!(
            provider
                .get_suggestions(&lines, 0, 3, false)
                .unwrap()
                .prefix,
            "/he"
        );
    }
}
