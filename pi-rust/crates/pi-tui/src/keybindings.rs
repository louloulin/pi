//! Keybinding registry — the Rust port of `packages/tui/src/keybindings.ts`.
//!
//! Upstream `@earendil-works/pi-tui` exposes one global, overridable map from
//! an action id (`"tui.editor.cursorLeft"`, `"tui.select.confirm"`, …) to the
//! key chords that trigger it. Downstream packages read it through
//! `getKeybindings()` and may replace the chords via a user config
//! (`tui.input.submit: ["enter", "ctrl+enter"]`).
//!
//! This module ports that registry surface:
//!
//! * [`tui_default_keybindings`] — the default table ([`TUI_KEYBINDINGS`] upstream):
//!   the action ids, their default chords and their descriptions, in the same
//!   order as the TypeScript literal.
//! * [`KeybindingsManager`] — user overrides, conflict reporting and resolution
//!   (`KeybindingsManager` upstream).
//! * [`KeybindingsConfig`] — the user binding set (upstream `KeybindingsConfig`).
//! * [`parse_key_id`] — the key-id vocabulary of `keys.ts` (`"ctrl+shift+up"`,
//!   `"pageUp"`, `"alt+backspace"`, …) as an [`InputEvent`]-level predicate.
//! * [`get_keybindings`] / [`set_keybindings`] — the global accessors.
//!
//! # Matching
//!
//! Upstream matches *raw terminal data* (`matchesKey(data, keyId)`) because the
//! TypeScript TUI parses the terminal protocol itself: Kitty keyboard
//! sequences, `modifyOtherKeys`, and the legacy `\x1b[Z`-style sequences all
//! arrive as strings. The Rust port hands decoding to `crossterm` and consumes
//! the normalised [`InputEvent`], so [`KeybindingsManager::matches`] takes an
//! event instead:
//!
//! ```no_run
//! # use pi_tui::input::{InputEvent, KeyCode, KeyModifiers};
//! # use pi_tui::keybindings::KeybindingsManager;
//! let kb = KeybindingsManager::tui_defaults();
//! let ctrl_j = InputEvent::key(KeyCode::Char('j'), KeyModifiers::CONTROL);
//! assert!(kb.matches(&ctrl_j, "tui.input.newLine"));
//! ```
//!
//! Consequences of that split, all deliberate:
//!
//! * A key id that is not in the [`keys.ts`](parse_key_id) vocabulary (for
//!   example `"clear"`, which [`KeyCode`] cannot represent) never matches.
//!   Unknown modifier names are ignored, exactly like upstream's
//!   `parseKeyId`, so `"hyper+a"` matches plain `a`.
//! * For alphabetic keys the *case* of the delivered character stands in for
//!   the Shift modifier (`Char('A')` is `"shift+a"`): terminals that do not
//!   speak the Kitty keyboard protocol report `shift+a` as `A` with no
//!   modifier bit, which is the same ambiguity upstream resolves in
//!   `normalizeShiftedLetterIdentityCodepoint`.
//! * `Shift+Tab` arrives as [`KeyCode::BackTab`], so `"shift+tab"` matches it
//!   while `"tab"` does not.
//! * `super` maps onto [`KeyModifiers::meta`] (the `crossterm` `SUPER`/`META`
//!   flags are collapsed by the [`InputEvent`] conversion).
//! * Non-key events ([`InputEvent::Mouse`], [`InputEvent::Resize`], …) never
//!   match a keybinding.
//!
//! # Ordering
//!
//! [`KeybindingsManager::get_resolved_bindings`] and
//! [`KeybindingsManager::get_conflicts`] keep upstream's insertion order:
//! definitions follow [`tui_default_keybindings`]'s table order, user bindings
//! follow the order they were added. Duplicate chords inside one binding are
//! dropped (upstream `normalizeKeys`), and a chord claimed by two *user*
//! bindings is reported as a conflict without evicting any default.

use std::sync::{Mutex, OnceLock};

use crate::input::{InputEvent, Key, KeyCode, KeyModifiers};

/// Definitions for a single keybinding: its default chords and what it does.
///
/// Upstream `KeybindingDefinition`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeybindingDefinition {
    /// Default chords, as key ids. Empty means "unbound by default".
    pub default_keys: Vec<String>,
    /// Human-readable description (`None` when upstream omits it).
    pub description: Option<String>,
}

impl KeybindingDefinition {
    /// A definition with no description.
    pub fn new(default_keys: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            default_keys: default_keys.into_iter().map(Into::into).collect(),
            description: None,
        }
    }

    /// A definition with a description.
    pub fn described(
        default_keys: impl IntoIterator<Item = impl Into<String>>,
        description: impl Into<String>,
    ) -> Self {
        Self {
            default_keys: default_keys.into_iter().map(Into::into).collect(),
            description: Some(description.into()),
        }
    }

    /// True when the binding has no default chord at all.
    pub fn is_unbound(&self) -> bool {
        self.default_keys.is_empty()
    }
}

/// One chord claimed by more than one user binding.
///
/// Upstream `KeybindingConflict`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeybindingConflict {
    /// The contested key id.
    pub key: String,
    /// The action ids claiming it, in the order they were configured.
    pub keybindings: Vec<String>,
}

/// User keybinding overrides, in declaration order.
///
/// Upstream `KeybindingsConfig`. An id that is absent keeps its default; an id
/// bound to an empty list is deliberately unbound; ids that match no
/// definition are ignored by [`KeybindingsManager`] (upstream skips them in
/// `rebuild`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeybindingsConfig {
    entries: Vec<(String, Vec<String>)>,
}

impl KeybindingsConfig {
    /// An empty override set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Bind `keybinding` to `keys`, replacing any previous override in place.
    pub fn set(
        &mut self,
        keybinding: impl Into<String>,
        keys: impl IntoIterator<Item = impl Into<String>>,
    ) -> &mut Self {
        let keybinding = keybinding.into();
        let keys: Vec<String> = keys.into_iter().map(Into::into).collect();
        match self.entries.iter_mut().find(|(id, _)| *id == keybinding) {
            Some((_, slot)) => *slot = keys,
            None => self.entries.push((keybinding, keys)),
        }
        self
    }

    /// The chords explicitly configured for `keybinding`, if any.
    pub fn get(&self, keybinding: &str) -> Option<&[String]> {
        self.entries
            .iter()
            .find(|(id, _)| id == keybinding)
            .map(|(_, keys)| keys.as_slice())
    }

    /// Number of configured ids.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when nothing is configured.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Iterate the configured `(keybinding, keys)` pairs in declaration order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &[String])> + '_ {
        self.entries
            .iter()
            .map(|(id, keys)| (id.as_str(), keys.as_slice()))
    }
}

impl<K, V> FromIterator<(K, V)> for KeybindingsConfig
where
    K: Into<String>,
    V: IntoIterator,
    V::Item: Into<String>,
{
    fn from_iter<T: IntoIterator<Item = (K, V)>>(iter: T) -> Self {
        let mut config = Self::new();
        for (keybinding, keys) in iter {
            config.set(keybinding, keys);
        }
        config
    }
}

/// The default keybinding table — upstream `TUI_KEYBINDINGS`, same ids, same
/// chords, same order.
///
/// Every id belongs to the `tui.` namespace: editor and input chords, list
/// selection, and the alternate-screen viewport/search shortcuts.
pub fn tui_default_keybindings() -> Vec<(String, KeybindingDefinition)> {
    /// Explicitly unbound: an empty array literal cannot infer its item type.
    const NO_KEYS: [&str; 0] = [];

    fn entry(
        id: &str,
        keys: impl IntoIterator<Item = impl Into<String>>,
        description: &str,
    ) -> (String, KeybindingDefinition) {
        (
            id.to_string(),
            KeybindingDefinition::described(keys, description),
        )
    }

    vec![
        entry("tui.editor.cursorUp", ["up"], "Move cursor up"),
        entry("tui.editor.cursorDown", ["down"], "Move cursor down"),
        entry(
            "tui.editor.historyPrevious",
            NO_KEYS,
            "Select previous prompt history entry",
        ),
        entry(
            "tui.editor.historyNext",
            NO_KEYS,
            "Select next prompt history entry",
        ),
        // Reverse history search. Not an upstream `packages/tui` binding —
        // upstream leaves `historyPrevious` / `historyNext` unbound and has no
        // search at all; this is codex's composer chord (`Ctrl+R`, with
        // `Ctrl+S` as the forward twin), added so the port's composer can be
        // driven the way codex's is. `app.session.rename` also claims `Ctrl+R`,
        // but only inside the `/resume` picker, which owns the keyboard while
        // it is open (see the coding-agent's `chatinput_chord_conflicts`).
        entry(
            "tui.editor.historySearch",
            ["ctrl+r"],
            "Reverse search prompt history",
        ),
        entry(
            "tui.editor.historySearchNext",
            ["ctrl+s"],
            "Search prompt history forward",
        ),
        entry(
            "tui.editor.historySearch",
            ["ctrl+r"],
            "Search prompt history (reverse incremental search)",
        ),
        entry(
            "tui.editor.historySearchNext",
            ["ctrl+s"],
            "Move to the next (newer) prompt history search match",
        ),
        entry(
            "tui.editor.cursorLeft",
            ["left", "ctrl+b"],
            "Move cursor left",
        ),
        entry(
            "tui.editor.cursorRight",
            ["right", "ctrl+f"],
            "Move cursor right",
        ),
        entry(
            "tui.editor.cursorWordLeft",
            ["alt+left", "ctrl+left", "alt+b"],
            "Move cursor word left",
        ),
        entry(
            "tui.editor.cursorWordRight",
            ["alt+right", "ctrl+right", "alt+f"],
            "Move cursor word right",
        ),
        entry(
            "tui.editor.cursorLineStart",
            ["home", "ctrl+home", "ctrl+a"],
            "Move to line start",
        ),
        entry(
            "tui.editor.cursorLineEnd",
            ["end", "ctrl+end", "ctrl+e"],
            "Move to line end",
        ),
        entry(
            "tui.editor.jumpForward",
            ["ctrl+]"],
            "Jump forward to character",
        ),
        entry(
            "tui.editor.jumpBackward",
            ["ctrl+alt+]"],
            "Jump backward to character",
        ),
        entry("tui.editor.pageUp", ["pageUp", "ctrl+pageUp"], "Page up"),
        entry(
            "tui.editor.pageDown",
            ["pageDown", "ctrl+pageDown"],
            "Page down",
        ),
        entry(
            "tui.editor.deleteCharBackward",
            ["backspace"],
            "Delete character backward",
        ),
        entry(
            "tui.editor.deleteCharForward",
            ["delete", "ctrl+d"],
            "Delete character forward",
        ),
        entry(
            "tui.editor.deleteWordBackward",
            ["ctrl+w", "alt+backspace"],
            "Delete word backward",
        ),
        entry(
            "tui.editor.deleteWordForward",
            ["alt+d", "alt+delete"],
            "Delete word forward",
        ),
        entry(
            "tui.editor.deleteToLineStart",
            ["ctrl+u"],
            "Delete to line start",
        ),
        entry(
            "tui.editor.deleteToLineEnd",
            ["ctrl+k"],
            "Delete to line end",
        ),
        entry("tui.editor.yank", ["ctrl+y"], "Yank"),
        entry("tui.editor.yankPop", ["alt+y"], "Yank pop"),
        entry("tui.editor.undo", ["ctrl+-"], "Undo"),
        entry(
            "tui.input.newLine",
            ["shift+enter", "ctrl+j"],
            "Insert newline",
        ),
        entry("tui.input.submit", ["enter"], "Submit input"),
        entry("tui.input.tab", ["tab"], "Tab / autocomplete"),
        entry("tui.input.copy", ["ctrl+c"], "Copy selection"),
        entry("tui.select.up", ["up"], "Move selection up"),
        entry("tui.select.down", ["down"], "Move selection down"),
        entry("tui.select.pageUp", ["pageUp"], "Selection page up"),
        entry("tui.select.pageDown", ["pageDown"], "Selection page down"),
        entry("tui.select.confirm", ["enter"], "Confirm selection"),
        entry(
            "tui.select.cancel",
            ["escape", "ctrl+c"],
            "Cancel selection",
        ),
        // These intentionally shadow the unmodified editor bindings in
        // fullscreen mode.
        entry(
            "tui.altScreen.pageUp",
            ["pageUp"],
            "Scroll viewport up one page",
        ),
        entry(
            "tui.altScreen.pageDown",
            ["pageDown"],
            "Scroll viewport down one page",
        ),
        entry(
            "tui.altScreen.halfPageUp",
            NO_KEYS,
            "Scroll viewport up half a page",
        ),
        entry(
            "tui.altScreen.halfPageDown",
            NO_KEYS,
            "Scroll viewport down half a page",
        ),
        entry(
            "tui.altScreen.lineUp",
            NO_KEYS,
            "Scroll viewport up one line",
        ),
        entry(
            "tui.altScreen.lineDown",
            NO_KEYS,
            "Scroll viewport down one line",
        ),
        entry(
            "tui.altScreen.previousPrompt",
            ["ctrl+shift+up", "ctrl+up"],
            "Jump to previous semantic prompt",
        ),
        entry(
            "tui.altScreen.nextPrompt",
            ["ctrl+shift+down", "ctrl+down"],
            "Jump to next semantic prompt",
        ),
        entry(
            "tui.altScreen.search",
            ["ctrl+shift+f"],
            "Search the primary scroll view",
        ),
        entry(
            "tui.altScreen.searchNext",
            ["enter", "ctrl+g"],
            "Select the next search match",
        ),
        entry(
            "tui.altScreen.searchPrevious",
            ["shift+enter", "ctrl+shift+g"],
            "Select the previous search match",
        ),
        entry(
            "tui.altScreen.searchClose",
            ["escape"],
            "Close transcript search",
        ),
        entry("tui.altScreen.top", ["home"], "Scroll viewport to top"),
        entry("tui.altScreen.bottom", ["end"], "Scroll viewport to bottom"),
    ]
}

/// The `app.*` action ids this port actually consumes.
///
/// A keybinding table can say that an action *is bound*; it cannot say that
/// the action *does anything*. Both advertisement surfaces — the startup
/// header ([`crate::locale::STARTUP_HINTS`]) and `/hotkeys`
/// (`pi-coding-agent`'s `commands::slash::hotkeys_text`) — used to read a
/// default chord as proof of a working shortcut, so a default-table entry no
/// consumer answers was still advertised. LUM-1240 measured four such dead
/// keys on the port's tip; LUM-1308 closed the last two, `app.suspend` and
/// `app.editor.external`.
///
/// This list is the missing axis. An `app.*` id appears in a hint only when
/// it is both bound and listed here; `tui.*` ids are consumed by the `pi-tui`
/// components themselves and need no entry (see [`app_action_is_consumed`]).
///
/// Keep it in step with the consumers: the driver claims its `app.*` chords in
/// `crates/pi-coding-agent/src/interactive.rs` before the App sees the key,
/// and `crates/pi-tui/src/{app,editor}.rs` consume the rest.
///
/// [# LUM-1245] `app.model.select` was added when the driver started routing
/// `Ctrl+L` to the model selector instead of clearing the transcript.
///
/// [# LUM-1255] the selector-scoped `app.session.*` / `app.tree.*` chords are
/// claimed by the driver while the `/resume` and `/tree` pickers are open
/// (`interactive.rs::handle_picker_key`). Upstream consumes them inside its
/// selector components, so the component-level exemption does not apply here:
/// the port's shared `Selector` ignores them and the driver intercepts them.
///
/// [# LUM-1308] `app.editor.external` (`Ctrl+G`) and `app.suspend` (`Ctrl+Z`)
/// are claimed by the driver's `handle_input_event`, which hands the terminal
/// to `$EDITOR` (`crate::external_editor`) and to `SIGTSTP` respectively. This
/// retires the *whole* false-ad class: every `app.*` id in the merged table now
/// has a consumer.
///
/// [# LUM-1263] `app.tree.editLabel` (`Shift+L`) is claimed by
/// `handle_picker_key` while the `/tree` overlay is open: it opens
/// [`crate::tree::TreeLabelEditor`], whose `Enter` appends the label entry to
/// the session (the last *silent* `app.*` id).
pub const CONSUMED_APP_ACTIONS: &[&str] = &[
    "app.interrupt",
    "app.clear",
    "app.exit",
    "app.suspend",
    "app.editor.external",
    "app.thinking.cycle",
    "app.thinking.save",
    "app.thinking.toggle",
    "app.model.cycleForward",
    "app.model.cycleBackward",
    "app.model.select",
    // [# LUM-1274] the `/scoped-models` panel consumes these six inside the
    // picker (`interactive.rs::handle_picker_key`, `PickerKind::ScopedModels`).
    // They were the last *silent* `app.*` ids: bound in the merged table, with
    // no code path resolving them.
    "app.models.save",
    "app.models.enableAll",
    "app.models.clearAll",
    "app.models.toggleProvider",
    "app.models.reorderUp",
    "app.models.reorderDown",
    "app.tools.expand",
    "app.header",
    "app.message.copy",
    "app.message.followUp",
    "app.message.dequeue",
    "app.clipboard.pasteImage",
    "app.session.new",
    "app.session.tree",
    "app.session.fork",
    "app.session.resume",
    "app.session.toggleSort",
    "app.session.togglePath",
    "app.session.toggleNamedFilter",
    "app.session.rename",
    "app.session.delete",
    "app.session.deleteNoninvasive",
    "app.tree.foldOrUp",
    "app.tree.unfoldOrDown",
    "app.tree.editLabel",
    "app.tree.toggleLabelTimestamp",
    "app.tree.filter.default",
    "app.tree.filter.noTools",
    "app.tree.filter.userOnly",
    "app.tree.filter.labeledOnly",
    "app.tree.filter.all",
    "app.tree.filter.cycleForward",
    "app.tree.filter.cycleBackward",
];

/// True when `id` names an action with a consumer in this port.
///
/// Non-`app.*` ids (the `tui.*` namespace) are component-level: `pi-tui`
/// consumes them itself, so they always count as wired. An `app.*` id counts
/// only when it is in [`CONSUMED_APP_ACTIONS`].
pub fn app_action_is_consumed(id: &str) -> bool {
    if !id.starts_with("app.") {
        return true;
    }
    CONSUMED_APP_ACTIONS.contains(&id)
}

/// Parse a key id (`"ctrl+shift+up"`, `"alt+backspace"`, `"pageUp"`, `"f5"`,
/// `"a"`, `"!"`) into the [`Key`] it denotes.
///
/// The vocabulary mirrors `keys.ts`: letters, digits, the ASCII symbol keys,
/// and the special keys `escape`/`esc`, `enter`/`return`, `tab`, `space`,
/// `backspace`, `delete`, `insert`, `home`, `end`, `pageUp`, `pageDown`, the
/// four arrows, and `f1`–`f12`. Modifier names are `ctrl`, `shift`, `alt` and
/// `super` (order-insensitive); unknown names are ignored like upstream's
/// `parseKeyId`.
///
/// Returns `None` for an empty id, an unknown named key (`"clear"`) or a name
/// longer than one character that is not in the vocabulary.
pub fn parse_key_id(key_id: &str) -> Option<Key> {
    let lowered = key_id.to_ascii_lowercase();
    let mut parts: Vec<&str> = lowered.split('+').collect();
    let base = parts.pop()?;
    if base.is_empty() {
        return None;
    }

    let mut modifiers = KeyModifiers::NONE;
    for modifier in parts {
        match modifier {
            "ctrl" => modifiers.control = true,
            "shift" => modifiers.shift = true,
            "alt" => modifiers.alt = true,
            "super" => modifiers.meta = true,
            _ => {}
        }
    }

    let code = match base {
        "escape" | "esc" => KeyCode::Esc,
        "enter" | "return" => KeyCode::Enter,
        "tab" => KeyCode::Tab,
        "space" => KeyCode::Char(' '),
        "backspace" => KeyCode::Backspace,
        "delete" => KeyCode::Delete,
        "insert" => KeyCode::Insert,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" => KeyCode::PageUp,
        "pagedown" => KeyCode::PageDown,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        single if is_function_key(single) => KeyCode::F(single[1..].parse::<u8>().ok()?),
        single if single.chars().count() == 1 => KeyCode::Char(single.chars().next()?),
        _ => return None,
    };

    Some(Key::new(code, modifiers))
}

/// `f1` … `f12` (upstream `SpecialKey`).
fn is_function_key(name: &str) -> bool {
    let Some(digits) = name.strip_prefix('f') else {
        return false;
    };
    matches!(digits.parse::<u8>(), Ok(1..=12))
}

/// True when `event` triggers `keybinding`, with built-in chords standing in
/// while the installed table does not define the id.
///
/// The `app.*` ids belong to the coding-agent config layer, so a bare
/// `pi-tui` registry (only the `tui.*` ids) leaves them unknown and
/// [`KeybindingsManager::matches`] returns `false`. The caller's built-in
/// chords then stand in, so the standalone component behaviour is preserved;
/// once any table defines the id — even deliberately unbound — the fallback
/// stops applying and the table decides.
///
/// `builtin` chords use the same spelling as the registry ids
/// (`"ctrl+d"`, `"escape"`, ...).
pub fn matches_with_fallback(
    manager: &KeybindingsManager,
    event: &InputEvent,
    keybinding: &str,
    builtin: &[&str],
) -> bool {
    if manager.get_definition(keybinding).is_none() {
        let InputEvent::Key(key) = event else {
            return false;
        };
        return builtin.iter().any(|chord| key_matches(chord, key));
    }
    manager.matches(event, keybinding)
}

/// True when `key` is the chord `key_id` names.
///
/// See the module docs for the matching rules (case-as-Shift for letters,
/// `Shift+Tab` as [`KeyCode::BackTab`], `super` as [`KeyModifiers::meta`]).
pub fn key_matches(key_id: &str, key: &Key) -> bool {
    let Some(expected) = parse_key_id(key_id) else {
        return false;
    };

    // `shift+tab` arrives as `BackTab` on terminals that do not report the
    // shift bit for Tab.
    if matches!(key.code, KeyCode::BackTab) && matches!(expected.code, KeyCode::Tab) {
        return expected.modifiers.shift
            && key.modifiers.control == expected.modifiers.control
            && key.modifiers.alt == expected.modifiers.alt
            && key.modifiers.meta == expected.modifiers.meta;
    }

    match (expected.code, key.code) {
        (KeyCode::Char(expected_char), KeyCode::Char(actual)) => {
            if expected_char.is_ascii_alphabetic() {
                // The case stands in for the Shift modifier, so `shift+a`
                // matches `A` and `a`+SHIFT, while `a` matches only the
                // unshifted spelling.
                let shift = key.modifiers.shift || actual.is_ascii_uppercase();
                expected_char.eq_ignore_ascii_case(&actual)
                    && shift == expected.modifiers.shift
                    && key.modifiers.control == expected.modifiers.control
                    && key.modifiers.alt == expected.modifiers.alt
                    && key.modifiers.meta == expected.modifiers.meta
            } else {
                // Digits and symbols already carry their shifted identity
                // (`!` is `shift+1`), so the Shift bit is not part of the
                // comparison for either side.
                expected_char == actual
                    && key.modifiers.control == expected.modifiers.control
                    && key.modifiers.alt == expected.modifiers.alt
                    && key.modifiers.meta == expected.modifiers.meta
            }
        }
        (expected_code, actual_code) => {
            expected_code == actual_code
                && key.modifiers.control == expected.modifiers.control
                && key.modifiers.alt == expected.modifiers.alt
                && key.modifiers.shift == expected.modifiers.shift
                && key.modifiers.meta == expected.modifiers.meta
        }
    }
}

/// Resolve a keybinding id to its chords, with user overrides applied.
///
/// Upstream `KeybindingsManager`.
#[derive(Debug, Clone)]
pub struct KeybindingsManager {
    definitions: Vec<(String, KeybindingDefinition)>,
    user_bindings: KeybindingsConfig,
    keys_by_id: Vec<(String, Vec<String>)>,
    conflicts: Vec<KeybindingConflict>,
}

impl KeybindingsManager {
    /// Build a manager from a definition table and user overrides.
    ///
    /// Overrides for ids outside `definitions` are ignored (upstream
    /// `rebuild`).
    pub fn new(
        definitions: Vec<(String, KeybindingDefinition)>,
        user_bindings: KeybindingsConfig,
    ) -> Self {
        let mut manager = Self {
            definitions,
            user_bindings,
            keys_by_id: Vec::new(),
            conflicts: Vec::new(),
        };
        manager.rebuild();
        manager
    }

    /// A manager over [`tui_default_keybindings`] with no overrides.
    pub fn tui_defaults() -> Self {
        Self::new(tui_default_keybindings(), KeybindingsConfig::new())
    }

    fn rebuild(&mut self) {
        self.keys_by_id.clear();
        self.conflicts.clear();

        // Chords claimed by more than one *user* binding, in configuration
        // order. Defaults are not evicted by a conflicting override.
        let mut claims: Vec<(String, Vec<String>)> = Vec::new();
        for (keybinding, keys) in self.user_bindings.iter() {
            if !self.definitions.iter().any(|(id, _)| id == keybinding) {
                continue;
            }
            for key in normalize_keys(keys) {
                match claims.iter_mut().find(|(claimed, _)| *claimed == key) {
                    Some((_, claimants)) => {
                        if !claimants.iter().any(|id| id == keybinding) {
                            claimants.push(keybinding.to_string());
                        }
                    }
                    None => claims.push((key, vec![keybinding.to_string()])),
                }
            }
        }
        self.conflicts = claims
            .into_iter()
            .filter(|(_, claimants)| claimants.len() > 1)
            .map(|(key, keybindings)| KeybindingConflict { key, keybindings })
            .collect();

        let mut keys_by_id = Vec::with_capacity(self.definitions.len());
        for (id, definition) in &self.definitions {
            let keys = match self.user_bindings.get(id) {
                Some(user_keys) => normalize_keys(user_keys),
                None => normalize_keys(&definition.default_keys),
            };
            keys_by_id.push((id.clone(), keys));
        }
        self.keys_by_id = keys_by_id;
    }

    /// True when the event triggers `keybinding`.
    ///
    /// Returns `false` for an unknown id, an unbound id, an event that is not
    /// a key event, or a key id this port cannot represent.
    pub fn matches(&self, event: &InputEvent, keybinding: &str) -> bool {
        let InputEvent::Key(key) = event else {
            return false;
        };
        let Some((_, keys)) = self.keys_by_id.iter().find(|(id, _)| id == keybinding) else {
            return false;
        };
        keys.iter().any(|key_id| key_matches(key_id, key))
    }

    /// The chords bound to `keybinding`, or an empty list when it is unknown
    /// or deliberately unbound.
    pub fn get_keys(&self, keybinding: &str) -> Vec<String> {
        self.keys_by_id
            .iter()
            .find(|(id, _)| id == keybinding)
            .map(|(_, keys)| keys.clone())
            .unwrap_or_default()
    }

    /// The definition of `keybinding`, if it is part of the table.
    pub fn get_definition(&self, keybinding: &str) -> Option<&KeybindingDefinition> {
        self.definitions
            .iter()
            .find(|(id, _)| id == keybinding)
            .map(|(_, definition)| definition)
    }

    /// The user overrides that claim the same chord.
    pub fn get_conflicts(&self) -> &[KeybindingConflict] {
        &self.conflicts
    }

    /// Replace the user overrides and re-resolve every binding.
    pub fn set_user_bindings(&mut self, user_bindings: KeybindingsConfig) {
        self.user_bindings = user_bindings;
        self.rebuild();
    }

    /// The configured overrides, in declaration order.
    pub fn get_user_bindings(&self) -> &KeybindingsConfig {
        &self.user_bindings
    }

    /// Every definition's effective chords, in table order.
    pub fn get_resolved_bindings(&self) -> Vec<(String, Vec<String>)> {
        self.keys_by_id.clone()
    }

    /// The definition ids, in table order.
    pub fn keybindings(&self) -> impl Iterator<Item = &str> + '_ {
        self.definitions.iter().map(|(id, _)| id.as_str())
    }
}

impl Default for KeybindingsManager {
    fn default() -> Self {
        Self::tui_defaults()
    }
}

/// Drop duplicate chords, keeping first-seen order (upstream `normalizeKeys`).
fn normalize_keys(keys: &[String]) -> Vec<String> {
    let mut normalized = Vec::with_capacity(keys.len());
    for key in keys {
        if !normalized.iter().any(|seen| seen == key) {
            normalized.push(key.clone());
        }
    }
    normalized
}

/// The effective chords of `keybinding`, formatted for display
/// (`ctrl+o` → `Ctrl+O`), or `None` when the id is unknown or deliberately
/// unbound.
///
/// Upstream `keyText`, but read off the process-wide manager without cloning
/// it: render paths call this once per hint per frame. A multi-chord binding
/// is joined with `/`, exactly like the startup header and `/hotkeys`.
pub fn key_text(keybinding: &str) -> Option<String> {
    let keys = resolved_keys(keybinding)?;
    Some(format_keys(&keys))
}

/// The effective chords of `keybinding`, or `fallback` when the installed
/// table does not contain the id **at all**.
///
/// The two miss cases are deliberately different:
///
/// * unknown id — the table has no opinion (the `app.*` ids live in the
///   coding-agent's table, so a bare `pi-tui` registry cannot resolve
///   `app.tools.expand`): render the shipped `fallback` chord. This is the
///   same rule as `Editor::matches_app_exit`'s hardcoded `ctrl+d`.
/// * known but unbound — the user deliberately removed the chord, so return
///   an empty string and let the caller drop it instead of advertising a key
///   that does nothing.
pub fn key_text_or(keybinding: &str, fallback: &str) -> String {
    let mut guard = lock_global();
    if guard.is_none() {
        *guard = Some(KeybindingsManager::tui_defaults());
    }
    let Some(manager) = guard.as_ref() else {
        return fallback.to_string();
    };
    key_text_in(manager, keybinding, fallback)
}

/// [`key_text_or`] against an explicit table.
///
/// The injectable half exists for the same reason
/// [`Editor::handle_key_with`](crate::editor::Editor::handle_key_with) does:
/// a test can render a legend under a *user override* without mutating the
/// process-wide manager that every other test in the process shares. The
/// resolution rules are identical — unknown id means "the table has no
/// opinion" and yields `fallback`; known-but-unbound yields an empty string.
pub fn key_text_in(manager: &KeybindingsManager, keybinding: &str, fallback: &str) -> String {
    if manager.get_definition(keybinding).is_none() {
        return fallback.to_string();
    }
    format_keys(&manager.get_keys(keybinding))
}

/// [`key_text_in`] with a preference for one specific chord: the shipped
/// `preferred` chord is rendered when the table still binds it, and the full
/// effective set otherwise.
///
/// This is the rule the compact legends need (`/help`'s `keys:` block,
/// upstream's own hand-written legend rows). The registry's default set is
/// deliberately redundant — `tui.editor.cursorLineStart` ships as
/// `home` **and** `ctrl+home` **and** `ctrl+a` — so rendering every chord
/// turns a one-line legend into a 46-column wall. The compact legend names
/// the one chord a reader is expected to learn; `/hotkeys` remains the
/// exhaustive list. The moment the user drops `preferred`, the legend shows
/// what is actually bound rather than a dead chord.
///
/// Miss cases follow [`key_text_in`]: an id the table does not define renders
/// `preferred`, and a deliberately unbound id renders nothing.
pub fn key_text_preferring(
    manager: &KeybindingsManager,
    keybinding: &str,
    preferred: &str,
) -> String {
    if manager.get_definition(keybinding).is_none() {
        return crate::locale::format_chord(preferred);
    }
    let keys = manager.get_keys(keybinding);
    if keys.iter().any(|chord| chord == preferred) {
        return crate::locale::format_chord(preferred);
    }
    format_keys(&keys)
}

/// Upstream `keyHint(keybinding, description)` with a shipped-default
/// `fallback` for ids the installed table does not define
/// ([`key_text_or`]): `Ctrl+O to expand`.
///
/// An unbound id renders the description alone (`to expand`) rather than a
/// dead chord — the rule the startup header already uses for an unbound hint
/// row.
pub fn key_hint_or(keybinding: &str, fallback: &str, description: &str) -> String {
    let keys = key_text_or(keybinding, fallback);
    if keys.is_empty() {
        description.to_string()
    } else {
        format!("{keys} {description}")
    }
}

/// Render resolved chords the way every other surface spells them.
fn format_keys(keys: &[String]) -> String {
    keys.iter()
        .map(|chord| crate::locale::format_chord(chord))
        .collect::<Vec<_>>()
        .join("/")
}

/// The chords currently bound to `keybinding`, or `None` when the id is
/// unknown or unbound. Never fails: an uninitialised slot gets the defaults,
/// exactly like [`get_keybindings`].
fn resolved_keys(keybinding: &str) -> Option<Vec<String>> {
    let mut guard = lock_global();
    if guard.is_none() {
        *guard = Some(KeybindingsManager::tui_defaults());
    }
    let manager = guard.as_ref()?;
    let keys = manager.get_keys(keybinding);
    if keys.is_empty() {
        None
    } else {
        Some(keys)
    }
}

fn global_keybindings() -> &'static Mutex<Option<KeybindingsManager>> {
    static GLOBAL: OnceLock<Mutex<Option<KeybindingsManager>>> = OnceLock::new();
    GLOBAL.get_or_init(|| Mutex::new(None))
}

/// Install the process-wide keybindings manager (upstream `setKeybindings`).
pub fn set_keybindings(keybindings: KeybindingsManager) {
    let mut guard = lock_global();
    *guard = Some(keybindings);
}

/// Forget the installed manager, so the next [`get_keybindings`] rebuilds the
/// defaults. Upstream has no equivalent; this exists for tests and for a
/// caller that wants to drop a custom table.
pub fn reset_keybindings() {
    let mut guard = lock_global();
    *guard = None;
}

/// The process-wide keybindings manager (upstream `getKeybindings`).
///
/// The manager is cloned out of the global slot; install a new one with
/// [`set_keybindings`]. Defaults are built lazily on first use.
pub fn get_keybindings() -> KeybindingsManager {
    let mut guard = lock_global();
    if guard.is_none() {
        *guard = Some(KeybindingsManager::tui_defaults());
    }
    guard
        .as_ref()
        .expect("keybindings are initialised above")
        .clone()
}

/// Lock the global slot, recovering from a poisoned lock — a panic in another
/// thread must not make the registry unusable.
fn lock_global() -> std::sync::MutexGuard<'static, Option<KeybindingsManager>> {
    global_keybindings()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
