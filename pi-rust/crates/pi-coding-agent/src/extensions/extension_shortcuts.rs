//! Extension-installed keyboard shortcuts.
//!
//! Plan §5 P0-3 (`registerShortcut real injection`): TS
//! `runner.ts:72-91` keeps a `RESERVED_KEYBINDINGS_FOR_EXTENSION_CONFLICTS`
//! list of 19 editor-global `app.*` / `tui.*` ids, and refuses to let an
//! extension install a shortcut that collides with any of them. The Rust
//! port mirrors that contract — see [`RESERVED_KEYBINDINGS_FOR_EXTENSION_CONFLICTS`].
//!
//! The interactive loop queries [`ExtensionShortcutRegistry::lookup`]
//! from [`crate::interactive::keymap::dispatch_global_chord_table`]
//! before the built-in `app.*` / `tui.*` chords run, so an installed
//! shortcut always claims the key first. Because the built-in table's
//! `restrictOverride` flag also blocks user rebinds for the reserved ids,
//! the two layers stay consistent.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use pi_tui::input::InputEvent;
use pi_tui::keybindings::{key_matches, KeybindingsManager};

/// Keybinding ids whose chords are off-limits to extensions.
///
/// Mirrors `packages/coding-agent/src/core/extensions/runner.ts:71-90`
/// exactly. Keep this list sorted alphabetically and in sync with the TS
/// source — the parity test in `tests/extension_register_shortcut.rs`
/// asserts the count and contents.
pub const RESERVED_KEYBINDINGS_FOR_EXTENSION_CONFLICTS: &[&str] = &[
    "app.clear",
    "app.editor.external",
    "app.exit",
    "app.interrupt",
    "app.message.copy",
    "app.message.followUp",
    "app.model.cycleBackward",
    "app.model.cycleForward",
    "app.model.select",
    "app.suspend",
    "app.thinking.cycle",
    "app.thinking.toggle",
    "app.tools.expand",
    "tui.editor.deleteToLineEnd",
    "tui.input.copy",
    "tui.input.submit",
    "tui.select.cancel",
    "tui.select.confirm",
];

/// One installed extension shortcut.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionShortcut {
    /// Handle returned to the extension; used by `unregister`.
    pub id: u64,
    /// The chord the extension asked for, in `parse_key_id` form
    /// (`"ctrl+t"`, `"alt+enter"`, ...).
    pub chord: String,
    /// Opaque callback payload. The TUI host stores it verbatim — only
    /// the JS shim knows how to decode it back into a handler call.
    pub callback: String,
}

/// Failure modes for [`ExtensionShortcutRegistry::register`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShortcutConflict {
    /// Chord string did not parse (`parse_key_id` returned `None`).
    UnknownChord { chord: String },
    /// Chord collides with a reserved keybinding's current binding set.
    ReservedKey {
        chord: String,
        keybinding: &'static str,
        conflicting_keys: Vec<String>,
    },
}

impl std::fmt::Display for ShortcutConflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownChord { chord } => {
                write!(f, "extension shortcut `{chord}` is not a recognised chord")
            }
            Self::ReservedKey {
                chord,
                keybinding,
                conflicting_keys,
            } => {
                let keys = conflicting_keys.join(", ");
                write!(
                    f,
                    "extension shortcut `{chord}` collides with reserved \
                     keybinding `{keybinding}` (currently bound to: {keys})",
                )
            }
        }
    }
}

impl std::error::Error for ShortcutConflict {}

/// Thread-safe registry of extension-installed shortcuts.
///
/// The TUI host owns one and shares it with the interactive keymap; the
/// extensions crate never sees the concrete type — it only knows the
/// `Result<u64, String>` return value of [`UiRegionHost::register_shortcut`].
#[derive(Debug)]
pub struct ExtensionShortcutRegistry {
    inner: Mutex<HashMap<u64, ExtensionShortcut>>,
    next_id: AtomicU64,
}

impl ExtensionShortcutRegistry {
    /// Create an empty registry.
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
        })
    }

    /// Install a shortcut if it does not collide with any reserved
    /// keybinding's *currently bound* chord set.
    ///
    /// Returns the new handle on success; returns [`ShortcutConflict`]
    /// when the chord is unparseable or collides. The caller is expected
    /// to surface the conflict to the extension shim so the JS side can
    /// fail-fast at registration time.
    pub fn register(
        &self,
        chord: &str,
        callback: String,
        keybindings: &KeybindingsManager,
    ) -> Result<u64, ShortcutConflict> {
        // Reject before we touch state — `parse_key_id` is pure, so an
        // unknown chord never depends on user bindings.
        let parsed = pi_tui::keybindings::parse_key_id(chord)
            .ok_or_else(|| ShortcutConflict::UnknownChord {
                chord: chord.to_string(),
            })?;

        // Reserved-key check. Each reserved id may carry multiple chords
        // (e.g. `app.message.followUp` is `alt+enter`; the upstream `keybindings.json`
        // schema also lets a user add a second chord for the same id),
        // so we walk every chord and report any collision in detail. A
        // user-binding override that *removed* a reserved chord would
        // open the slot — we honour the manager's current view.
        for reserved in RESERVED_KEYBINDINGS_FOR_EXTENSION_CONFLICTS {
            let keys = keybindings.get_keys(reserved);
            if keys.is_empty() {
                // The id is not bound at all (either stripped by the
                // user or unbound on this platform). Skip — nothing to
                // collide with.
                continue;
            }
            let mut conflicting_keys: Vec<String> = Vec::new();
            for key in &keys {
                let needle = key.as_str();
                if pi_tui::keybindings::key_matches(needle, &parsed) {
                    conflicting_keys.push(key.clone());
                }
            }
            if !conflicting_keys.is_empty() {
                return Err(ShortcutConflict::ReservedKey {
                    chord: chord.to_string(),
                    keybinding: reserved,
                    conflicting_keys,
                });
            }
        }

        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let shortcut = ExtensionShortcut {
            id,
            chord: chord.to_string(),
            callback,
        };
        let mut guard = self.inner.lock().expect("extension shortcut mutex poisoned");
        guard.insert(id, shortcut);
        Ok(id)
    }

    /// Look up the first shortcut whose chord matches `event`.
    ///
    /// Returns the [`ExtensionShortcut`] (which carries its `id`) so the
    /// dispatcher can hand the callback back to the JS shim. Multiple
    /// registrations on the same chord keep insertion order — the most
    /// recently installed one wins because we iterate in insertion order
    /// and stop on the first match (mirroring TS's `Map` iteration order).
    pub fn lookup(&self, event: &InputEvent) -> Option<ExtensionShortcut> {
        let guard = self.inner.lock().expect("extension shortcut mutex poisoned");
        guard.values().find(|sc| self.event_matches(sc, event)).cloned()
    }

    /// Look up a shortcut by handle. Used by the JS shim to resolve the
    /// `id` it stored against a `ctx.ui.unregisterShortcut(id)` call.
    pub fn get(&self, id: u64) -> Option<ExtensionShortcut> {
        let guard = self.inner.lock().expect("extension shortcut mutex poisoned");
        guard.get(&id).cloned()
    }

    /// Drop the shortcut with `handle`. Returns `true` when one was
    /// removed, `false` when the handle is unknown.
    pub fn unregister(&self, handle: u64) -> bool {
        let mut guard = self.inner.lock().expect("extension shortcut mutex poisoned");
        guard.remove(&handle).is_some()
    }

    /// Current shortcut count. Useful in tests and `/hotkeys`.
    pub fn len(&self) -> usize {
        let guard = self.inner.lock().expect("extension shortcut mutex poisoned");
        guard.len()
    }

    /// Snapshot of every installed shortcut. Used by `/hotkeys`.
    pub fn snapshot(&self) -> Vec<ExtensionShortcut> {
        let guard = self.inner.lock().expect("extension shortcut mutex poisoned");
        let mut out: Vec<ExtensionShortcut> = guard.values().cloned().collect();
        out.sort_by_key(|sc| sc.id);
        out
    }

    fn event_matches(&self, shortcut: &ExtensionShortcut, event: &InputEvent) -> bool {
        let InputEvent::Key(key) = event else {
            return false;
        };
        key_matches(&shortcut.chord, key)
    }
}

impl Default for ExtensionShortcutRegistry {
    fn default() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
        }
    }
}

/// Helper for [`UiRegionHost::register_shortcut`] implementations: a
/// registry-level convenience that bridges the trait's
/// `(String, String) -> Result<u64, String>` signature to the typed
/// registry API.
pub fn register_on(
    registry: &ExtensionShortcutRegistry,
    keybindings: &KeybindingsManager,
    chord: String,
    callback: String,
) -> Result<u64, String> {
    registry
        .register(&chord, callback, keybindings)
        .map_err(|err| err.to_string())
}