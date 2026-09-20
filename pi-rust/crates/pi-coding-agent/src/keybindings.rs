//! The coding-agent keybinding configuration layer.
//!
//! Rust port of `packages/coding-agent/src/core/keybindings.ts`, which sits
//! on top of the `pi-tui` registry ([`pi_tui::keybindings`]) and adds:
//!
//! * [`app_default_keybindings`] — the coding-agent-only `app.*` ids
//!   (upstream `AppKeybindings`, 43 entries), with the platform-forked
//!   chords and the Rust-only `app.header` added on top.
//!   defaults.
//! * [`merged_definitions`] — upstream `KEYBINDINGS`: the `pi-tui` default
//!   table with four `tui.*` defaults overridden, followed by the `app.*`
//!   entries.
//! * [`windows_keybindings`] — upstream `useWindowsKeybindings`, the
//!   Windows / WSL default selector.
//! * [`KEYBINDING_NAME_MIGRATIONS`] and [`migrate_keybindings_config`] — the
//!   legacy-name rewrite (59 entries, e.g. `interrupt` → `app.interrupt`).
//! * [`load_from_file`] — `<agent_dir>/keybindings.json` loading (BOM
//!   stripping, JSON validity, top-level object check, type filtering).
//! * [`KeybindingsManager`] — upstream `KeybindingsManager`, which extends
//!   [`pi_tui::keybindings::KeybindingsManager`] with file loading,
//!   [`reload`](KeybindingsManager::reload) and
//!   [`get_effective_config`](KeybindingsManager::get_effective_config).
//!
//! # Deliberate deviations from the TypeScript source
//!
//! * **Platform is a value, not a process global.** Upstream computes
//!   `useWindowsKeybindings()` once at module load and bakes the result into
//!   `KEYBINDINGS`. Here [`Platform`] and [`Env`] are explicit parameters of
//!   the definition builders, so the `win32`, WSL and `darwin` default sets
//!   are all reachable (and testable) on any host. [`Platform::current`] /
//!   [`process_env`] reproduce the upstream detection for callers that want
//!   it.
//! * **`Platform::Darwin`, not `"macos"`.** `std::env::consts::OS` reports
//!   `"macos"` where Node reports `"darwin"`; [`Platform::from_name`] accepts
//!   both. Unknown platforms are kept as [`Platform::Other`] and behave like
//!   non-Windows.
//! * **`Env` values are checked for emptiness.** Upstream's
//!   `Boolean(env.WSL_DISTRO_NAME || env.WSL_INTEROP)` treats an empty string
//!   as absent, so `windows_keybindings` requires a non-empty value too.
//! * **The raw config is an ordered `Vec`.** `serde_json::Map` is a `BTreeMap`
//!   without the `preserve_order` feature, so it cannot represent the
//!   declaration order [`order_keybindings_config`] produces. The raw config
//!   is therefore a [`RawKeybindingsConfig`] (an ordered list of pairs);
//!   ordering is set-based and independent of the file's own key order, which
//!   is what upstream's final `orderKeybindingsConfig` produces anyway.
//! * **`get_effective_config` returns pairs.** The `pi-tui` manager exposes
//!   `get_resolved_bindings() -> Vec<(id, keys)>` rather than upstream's
//!   `Record<Keybinding, KeyId | KeyId[]>`, and this layer mirrors that.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use pi_tui::keybindings::{
    tui_default_keybindings, KeybindingDefinition, KeybindingsConfig,
    KeybindingsManager as TuiKeybindingsManager,
};
use serde_json::{Map, Value};

use crate::paths::{agent_dir_or_default, strip_bom};

/// The file this layer reads inside an agent directory.
pub const KEYBINDINGS_FILE_NAME: &str = "keybindings.json";

/// The platforms the default tables fork on.
///
/// Upstream uses `NodeJS.Platform`; the Rust port keeps the three platforms
/// the defaults actually distinguish and folds everything else into
/// [`Platform::Other`] (which behaves like a non-Windows platform).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Platform {
    /// Native Windows (`"win32"`).
    Win32,
    /// Linux (`"linux"`), including WSL (see [`windows_keybindings`]).
    Linux,
    /// macOS (`"darwin"` in Node, `"macos"` in `std::env::consts::OS`).
    Darwin,
    /// Any other platform name.
    Other(String),
}

impl Platform {
    /// Parse a Node-style platform name (`"win32"`, `"linux"`, `"darwin"`,
    /// `"macos"`, …).
    pub fn from_name(name: &str) -> Self {
        match name {
            "win32" => Self::Win32,
            "linux" => Self::Linux,
            "darwin" | "macos" => Self::Darwin,
            other => Self::Other(other.to_string()),
        }
    }

    /// The platform this process runs on ([`std::env::consts::OS`]).
    pub fn current() -> Self {
        Self::from_name(std::env::consts::OS)
    }

    /// The upstream-style name of this platform.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Win32 => "win32",
            Self::Linux => "linux",
            Self::Darwin => "darwin",
            Self::Other(name) => name.as_str(),
        }
    }

    /// True for [`Platform::Win32`].
    pub fn is_windows(&self) -> bool {
        matches!(self, Self::Win32)
    }
}

/// The environment variables the platform probe reads.
///
/// A `BTreeMap` keeps the probe deterministic and makes the WSL detection
/// testable without touching the process environment.
pub type Env = BTreeMap<String, String>;

/// A snapshot of the process environment, for [`windows_keybindings`].
pub fn process_env() -> Env {
    std::env::vars().collect()
}

/// Upstream `useWindowsKeybindings`: native Windows, or Linux under WSL.
///
/// WSL is detected from a non-empty `WSL_DISTRO_NAME` or `WSL_INTEROP`.
/// `WT_SESSION` (Windows Terminal) is deliberately *not* a signal — a plain
/// Linux session inside Windows Terminal keeps the non-Windows defaults.
pub fn windows_keybindings(platform: &Platform, env: &Env) -> bool {
    if platform.is_windows() {
        return true;
    }
    if !matches!(platform, Platform::Linux) {
        return false;
    }
    is_truthy(env.get("WSL_DISTRO_NAME")) || is_truthy(env.get("WSL_INTEROP"))
}

/// Upstream's `Boolean(value)`: present and non-empty.
fn is_truthy(value: Option<&String>) -> bool {
    value.is_some_and(|value| !value.is_empty())
}

/// The ids of every coding-agent-only keybinding, in table order.
///
/// Upstream declares these as the `AppKeybindings` interface; this array is
/// the same list and its length is asserted by the tests.
pub const APP_KEYBINDING_IDS: [&str; 44] = [
    "app.interrupt",
    "app.clear",
    "app.exit",
    "app.suspend",
    "app.thinking.cycle",
    "app.thinking.save",
    "app.model.cycleForward",
    "app.model.cycleBackward",
    "app.model.select",
    "app.tools.expand",
    "app.thinking.toggle",
    // Rust-only: upstream expands the startup header from `app.tools.expand`
    // (`setToolsExpanded` → `builtInHeader.setExpanded`), so it has no header
    // id of its own. This port keeps the two folds separate and gives the
    // header toggle its own id so it is overridable and listed by `/hotkeys`.
    "app.header",
    "app.session.toggleNamedFilter",
    "app.editor.external",
    "app.message.copy",
    "app.message.followUp",
    "app.message.dequeue",
    "app.clipboard.pasteImage",
    "app.session.new",
    "app.session.tree",
    "app.session.fork",
    "app.session.resume",
    "app.tree.foldOrUp",
    "app.tree.unfoldOrDown",
    "app.tree.editLabel",
    "app.tree.toggleLabelTimestamp",
    "app.session.togglePath",
    "app.session.toggleSort",
    "app.session.rename",
    "app.session.delete",
    "app.session.deleteNoninvasive",
    "app.models.save",
    "app.models.enableAll",
    "app.models.clearAll",
    "app.models.toggleProvider",
    "app.models.reorderUp",
    "app.models.reorderDown",
    "app.tree.filter.default",
    "app.tree.filter.noTools",
    "app.tree.filter.userOnly",
    "app.tree.filter.labeledOnly",
    "app.tree.filter.all",
    "app.tree.filter.cycleForward",
    "app.tree.filter.cycleBackward",
];

/// The `app.*` default table — upstream's `AppKeybindings` definitions.
///
/// Seven entries fork on the platform:
/// `app.suspend` (unbound on Windows), `app.model.cycleBackward`,
/// `app.message.followUp`, `app.message.dequeue`,
/// `app.clipboard.pasteImage`, `app.tree.foldOrUp` (macOS chord order) and
/// `app.tree.unfoldOrDown`.
pub fn app_default_keybindings(
    platform: &Platform,
    env: &Env,
) -> Vec<(String, KeybindingDefinition)> {
    let windows = windows_keybindings(platform, env);
    let win32 = platform.is_windows();
    let darwin = matches!(platform, Platform::Darwin);

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
        entry("app.interrupt", ["escape"], "Cancel or abort"),
        entry("app.clear", ["ctrl+c"], "Clear editor"),
        entry("app.exit", ["ctrl+d"], "Exit when editor is empty"),
        entry(
            "app.suspend",
            if win32 { Vec::new() } else { vec!["ctrl+z"] },
            "Suspend to background",
        ),
        entry("app.thinking.cycle", ["shift+tab"], "Cycle thinking level"),
        entry("app.thinking.save", ["ctrl+s"], "Save thinking level"),
        entry("app.model.cycleForward", ["ctrl+p"], "Cycle to next model"),
        entry(
            "app.model.cycleBackward",
            if windows {
                vec!["alt+p"]
            } else {
                vec!["shift+ctrl+p"]
            },
            "Cycle to previous model",
        ),
        entry("app.model.select", ["ctrl+l"], "Open model selector"),
        entry("app.tools.expand", ["ctrl+o"], "Toggle tool output"),
        entry("app.thinking.toggle", ["ctrl+t"], "Toggle thinking blocks"),
        // Rust-only addition: the startup header's fold chord. `alt+h` is free
        // across every platform table this port installs, and the id exists so
        // `keybindings.json` can move it and `/hotkeys` can list it.
        entry("app.header", ["alt+h"], "Toggle startup header"),
        entry(
            "app.session.toggleNamedFilter",
            ["ctrl+n"],
            "Toggle named session filter",
        ),
        entry("app.editor.external", ["ctrl+g"], "Open external editor"),
        entry("app.message.copy", ["ctrl+x"], "Copy message to clipboard"),
        entry(
            "app.message.followUp",
            if windows {
                vec!["ctrl+q"]
            } else {
                vec!["alt+enter"]
            },
            "Queue follow-up message",
        ),
        entry(
            "app.message.dequeue",
            if windows {
                vec!["alt+q"]
            } else {
                vec!["alt+up"]
            },
            "Restore queued messages",
        ),
        entry(
            "app.clipboard.pasteImage",
            if windows {
                vec!["alt+v"]
            } else {
                vec!["ctrl+v"]
            },
            "Paste image from clipboard (text fallback)",
        ),
        // Upstream leaves `app.session.new` unbound (`defaultKeys: []`).
        // Stage 60 requires a real chord so the action is reachable and
        // honestly listed by `/hotkeys`; `alt+n` is free across all
        // platform tables and unambiguous in the terminals pi targets.
        entry("app.session.new", ["alt+n"], "Start a new session"),
        entry("app.session.tree", NO_KEYS, "Open session tree"),
        entry("app.session.fork", NO_KEYS, "Fork current session"),
        entry("app.session.resume", NO_KEYS, "Resume a session"),
        entry(
            "app.tree.foldOrUp",
            if darwin {
                vec!["alt+left", "ctrl+left"]
            } else {
                vec!["ctrl+left", "alt+left"]
            },
            "Fold tree branch or move up",
        ),
        entry(
            "app.tree.unfoldOrDown",
            if darwin {
                vec!["alt+right", "ctrl+right"]
            } else {
                vec!["ctrl+right", "alt+right"]
            },
            "Unfold tree branch or move down",
        ),
        entry("app.tree.editLabel", ["shift+l"], "Edit tree label"),
        entry(
            "app.tree.toggleLabelTimestamp",
            ["shift+t"],
            "Toggle tree label timestamps",
        ),
        entry(
            "app.session.togglePath",
            ["ctrl+p"],
            "Toggle session path display",
        ),
        entry(
            "app.session.toggleSort",
            ["ctrl+s"],
            "Toggle session sort mode",
        ),
        entry("app.session.rename", ["ctrl+r"], "Rename session"),
        entry("app.session.delete", ["ctrl+d"], "Delete session"),
        entry(
            "app.session.deleteNoninvasive",
            ["ctrl+backspace"],
            "Delete session when query is empty",
        ),
        entry("app.models.save", ["ctrl+s"], "Save model selection"),
        entry("app.models.enableAll", ["ctrl+a"], "Enable all models"),
        entry("app.models.clearAll", ["ctrl+x"], "Clear all models"),
        entry(
            "app.models.toggleProvider",
            ["ctrl+p"],
            "Toggle all models for provider",
        ),
        entry("app.models.reorderUp", ["alt+up"], "Move model up in order"),
        entry(
            "app.models.reorderDown",
            ["alt+down"],
            "Move model down in order",
        ),
        entry(
            "app.tree.filter.default",
            ["ctrl+d"],
            "Tree filter: default view",
        ),
        entry(
            "app.tree.filter.noTools",
            ["ctrl+t"],
            "Tree filter: hide tool results",
        ),
        entry(
            "app.tree.filter.userOnly",
            ["ctrl+u"],
            "Tree filter: user messages only",
        ),
        entry(
            "app.tree.filter.labeledOnly",
            ["ctrl+l"],
            "Tree filter: labeled entries only",
        ),
        entry(
            "app.tree.filter.all",
            ["ctrl+a"],
            "Tree filter: show all entries",
        ),
        entry(
            "app.tree.filter.cycleForward",
            ["ctrl+o"],
            "Tree filter: cycle forward",
        ),
        entry(
            "app.tree.filter.cycleBackward",
            ["shift+ctrl+o"],
            "Tree filter: cycle backward",
        ),
    ]
}

/// Upstream `KEYBINDINGS`: the full coding-agent default table.
///
/// The `pi-tui` defaults come first — with four `tui.*` entries overridden in
/// place — followed by the 44 `app.*` entries. The definitions carry the
/// platform forks, so this is the table to hand to the manager and to use for
/// [`order_keybindings_config`].
pub fn merged_definitions(platform: &Platform, env: &Env) -> Vec<(String, KeybindingDefinition)> {
    let windows = windows_keybindings(platform, env);
    let win32 = platform.is_windows();

    let mut definitions = tui_default_keybindings();
    for (id, definition) in definitions.iter_mut() {
        match id.as_str() {
            "tui.editor.undo" => {
                definition.default_keys = vec![if win32 {
                    "ctrl+z"
                } else if windows {
                    "alt+z"
                } else {
                    "ctrl+-"
                }
                .to_string()];
            }
            "tui.altScreen.previousPrompt" => {
                definition.default_keys = key_list(if windows {
                    &["ctrl+up"][..]
                } else {
                    &["ctrl+shift+up", "ctrl+up"][..]
                });
            }
            "tui.altScreen.nextPrompt" => {
                definition.default_keys = key_list(if windows {
                    &["ctrl+down"][..]
                } else {
                    &["ctrl+shift+down", "ctrl+down"][..]
                });
            }
            "tui.altScreen.search" => {
                definition.default_keys = key_list(if windows {
                    &["ctrl+f"][..]
                } else {
                    &["ctrl+shift+f"][..]
                });
            }
            _ => {}
        }
    }

    definitions.extend(app_default_keybindings(platform, env));
    definitions
}

fn key_list(keys: &[&str]) -> Vec<String> {
    keys.iter().map(|key| (*key).to_string()).collect()
}

/// Legacy keybinding id → namespaced id, in upstream order (59 entries).
pub const KEYBINDING_NAME_MIGRATIONS: [(&str, &str); 59] = [
    ("cursorUp", "tui.editor.cursorUp"),
    ("cursorDown", "tui.editor.cursorDown"),
    ("cursorLeft", "tui.editor.cursorLeft"),
    ("cursorRight", "tui.editor.cursorRight"),
    ("cursorWordLeft", "tui.editor.cursorWordLeft"),
    ("cursorWordRight", "tui.editor.cursorWordRight"),
    ("cursorLineStart", "tui.editor.cursorLineStart"),
    ("cursorLineEnd", "tui.editor.cursorLineEnd"),
    ("jumpForward", "tui.editor.jumpForward"),
    ("jumpBackward", "tui.editor.jumpBackward"),
    ("pageUp", "tui.editor.pageUp"),
    ("pageDown", "tui.editor.pageDown"),
    ("deleteCharBackward", "tui.editor.deleteCharBackward"),
    ("deleteCharForward", "tui.editor.deleteCharForward"),
    ("deleteWordBackward", "tui.editor.deleteWordBackward"),
    ("deleteWordForward", "tui.editor.deleteWordForward"),
    ("deleteToLineStart", "tui.editor.deleteToLineStart"),
    ("deleteToLineEnd", "tui.editor.deleteToLineEnd"),
    ("yank", "tui.editor.yank"),
    ("yankPop", "tui.editor.yankPop"),
    ("undo", "tui.editor.undo"),
    ("newLine", "tui.input.newLine"),
    ("submit", "tui.input.submit"),
    ("tab", "tui.input.tab"),
    ("copy", "tui.input.copy"),
    ("selectUp", "tui.select.up"),
    ("selectDown", "tui.select.down"),
    ("selectPageUp", "tui.select.pageUp"),
    ("selectPageDown", "tui.select.pageDown"),
    ("selectConfirm", "tui.select.confirm"),
    ("selectCancel", "tui.select.cancel"),
    ("interrupt", "app.interrupt"),
    ("clear", "app.clear"),
    ("exit", "app.exit"),
    ("suspend", "app.suspend"),
    ("cycleThinkingLevel", "app.thinking.cycle"),
    ("cycleModelForward", "app.model.cycleForward"),
    ("cycleModelBackward", "app.model.cycleBackward"),
    ("selectModel", "app.model.select"),
    ("expandTools", "app.tools.expand"),
    ("toggleThinking", "app.thinking.toggle"),
    ("toggleSessionNamedFilter", "app.session.toggleNamedFilter"),
    ("externalEditor", "app.editor.external"),
    ("followUp", "app.message.followUp"),
    ("dequeue", "app.message.dequeue"),
    ("pasteImage", "app.clipboard.pasteImage"),
    ("newSession", "app.session.new"),
    ("tree", "app.session.tree"),
    ("fork", "app.session.fork"),
    ("resume", "app.session.resume"),
    ("treeFoldOrUp", "app.tree.foldOrUp"),
    ("treeUnfoldOrDown", "app.tree.unfoldOrDown"),
    ("treeEditLabel", "app.tree.editLabel"),
    ("treeToggleLabelTimestamp", "app.tree.toggleLabelTimestamp"),
    ("toggleSessionPath", "app.session.togglePath"),
    ("toggleSessionSort", "app.session.toggleSort"),
    ("renameSession", "app.session.rename"),
    ("deleteSession", "app.session.delete"),
    ("deleteSessionNoninvasive", "app.session.deleteNoninvasive"),
];

/// The namespaced id a legacy name maps to, if any.
pub fn migrate_keybinding_name(key: &str) -> Option<&'static str> {
    KEYBINDING_NAME_MIGRATIONS
        .iter()
        .find(|(legacy, _)| *legacy == key)
        .map(|(_, next)| *next)
}

/// True when `key` is a legacy (pre-namespace) keybinding id.
pub fn is_legacy_keybinding_name(key: &str) -> bool {
    migrate_keybinding_name(key).is_some()
}

/// An ordered raw config: `(id, value)` pairs, before type filtering.
///
/// See the module docs for why this is not a `serde_json::Map`.
pub type RawKeybindingsConfig = Vec<(String, Value)>;

/// Upstream `migrateKeybindingsConfig`, using the current platform's table.
pub fn migrate_keybindings_config(raw: &Map<String, Value>) -> (RawKeybindingsConfig, bool) {
    migrate_keybindings_config_with_table(
        raw,
        &merged_definitions(&Platform::current(), &process_env()),
    )
}

/// Upstream `migrateKeybindingsConfig` with an explicit order table.
///
/// Every legacy id is renamed. When the target id is already present in the
/// *raw* config the legacy entry is dropped and the namespaced value wins
/// (upstream `Object.hasOwn`). Returns the ordered config and whether anything
/// was renamed or dropped.
pub fn migrate_keybindings_config_with_table(
    raw: &Map<String, Value>,
    definitions: &[(String, KeybindingDefinition)],
) -> (RawKeybindingsConfig, bool) {
    let mut config: RawKeybindingsConfig = Vec::with_capacity(raw.len());
    let mut migrated = false;

    for (key, value) in raw {
        let next_key = match migrate_keybinding_name(key) {
            Some(next) => next,
            None => key.as_str(),
        };
        let renamed = next_key != key.as_str();
        if renamed {
            migrated = true;
        }
        if renamed && raw.contains_key(next_key) {
            migrated = true;
            continue;
        }
        config.push((next_key.to_string(), value.clone()));
    }

    (order_keybindings_config(config, definitions), migrated)
}

/// Upstream `orderKeybindingsConfig`: table order first, then unknown ids in
/// lexicographic order.
pub fn order_keybindings_config(
    config: RawKeybindingsConfig,
    definitions: &[(String, KeybindingDefinition)],
) -> RawKeybindingsConfig {
    let mut remaining = config;
    let mut ordered = Vec::with_capacity(remaining.len());

    for (id, _) in definitions {
        if let Some(index) = remaining.iter().position(|(key, _)| key == id) {
            ordered.push(remaining.remove(index));
        }
    }

    remaining.sort_by(|(left, _), (right, _)| left.cmp(right));
    ordered.extend(remaining);
    ordered
}

/// Upstream `toKeybindingsConfig`: keep strings and arrays of strings, drop
/// everything else.
///
/// An empty array is kept as a deliberate "unbound" override.
pub fn to_keybindings_config(config: &RawKeybindingsConfig) -> KeybindingsConfig {
    let mut bindings = KeybindingsConfig::new();
    for (id, value) in config {
        match value {
            Value::String(key) => {
                bindings.set(id.clone(), [key.clone()]);
            }
            Value::Array(keys) if keys.iter().all(Value::is_string) => {
                bindings.set(
                    id.clone(),
                    keys.iter().filter_map(Value::as_str).map(str::to_string),
                );
            }
            _ => {}
        }
    }
    bindings
}

/// Upstream `loadRawConfig`: `None` when the file is missing, unreadable, not
/// valid JSON, or not a JSON object.
///
/// A leading BOM is stripped before parsing.
pub fn load_raw_config(path: &Path) -> Option<Map<String, Value>> {
    let raw = std::fs::read_to_string(path).ok()?;
    let parsed: Value = serde_json::from_str(strip_bom(&raw)).ok()?;
    match parsed {
        Value::Object(map) => Some(map),
        _ => None,
    }
}

/// Load `<agent_dir>/keybindings.json` into user bindings.
///
/// A missing / invalid / non-object file yields an empty config. Legacy ids
/// are migrated in memory, the result is ordered, and non-string entries are
/// dropped.
pub fn load_from_file(path: &Path) -> KeybindingsConfig {
    load_from_file_with_table(
        path,
        &merged_definitions(&Platform::current(), &process_env()),
    )
}

/// [`load_from_file`] with an explicit order table.
pub fn load_from_file_with_table(
    path: &Path,
    definitions: &[(String, KeybindingDefinition)],
) -> KeybindingsConfig {
    let Some(raw) = load_raw_config(path) else {
        return KeybindingsConfig::new();
    };
    let (config, _migrated) = migrate_keybindings_config_with_table(&raw, definitions);
    to_keybindings_config(&config)
}

/// Upstream `KeybindingsManager`: the `pi-tui` manager plus the config file.
///
/// The inner [`pi_tui::keybindings::KeybindingsManager`] is re-exposed through
/// delegating methods so callers do not need to reach into it for the common
/// operations.
#[derive(Debug, Clone)]
pub struct KeybindingsManager {
    definitions: Vec<(String, KeybindingDefinition)>,
    config_path: Option<PathBuf>,
    inner: TuiKeybindingsManager,
}

impl KeybindingsManager {
    /// Build a manager from an explicit definition table and user bindings.
    pub fn new(
        definitions: Vec<(String, KeybindingDefinition)>,
        user_bindings: KeybindingsConfig,
        config_path: Option<PathBuf>,
    ) -> Self {
        let inner = TuiKeybindingsManager::new(definitions.clone(), user_bindings);
        Self {
            definitions,
            config_path,
            inner,
        }
    }

    /// Read `<agent_dir>/keybindings.json` with the current platform's table.
    pub fn create(agent_dir: impl AsRef<Path>) -> Self {
        Self::create_with_platform(agent_dir, &Platform::current(), &process_env())
    }

    /// [`KeybindingsManager::create`] with an explicit platform and
    /// environment.
    pub fn create_with_platform(
        agent_dir: impl AsRef<Path>,
        platform: &Platform,
        env: &Env,
    ) -> Self {
        let definitions = merged_definitions(platform, env);
        let config_path = agent_dir.as_ref().join(KEYBINDINGS_FILE_NAME);
        let user_bindings = load_from_file_with_table(&config_path, &definitions);
        Self::new(definitions, user_bindings, Some(config_path))
    }

    /// [`KeybindingsManager::create`] with the default agent directory
    /// ([`agent_dir_or_default`], i.e. `~/.pi/agent`).
    pub fn create_default() -> Self {
        Self::create(agent_dir_or_default())
    }

    /// Re-read the config file and re-resolve every binding.
    ///
    /// Does nothing when the manager has no config path.
    pub fn reload(&mut self) {
        let Some(path) = self.config_path.clone() else {
            return;
        };
        let user_bindings = load_from_file_with_table(&path, &self.definitions);
        self.inner.set_user_bindings(user_bindings);
    }

    /// The effective bindings — upstream `getEffectiveConfig`, which is
    /// `getResolvedBindings()`.
    pub fn get_effective_config(&self) -> Vec<(String, Vec<String>)> {
        self.inner.get_resolved_bindings()
    }

    /// Path of the config file this manager reloads, if any.
    pub fn config_path(&self) -> Option<&Path> {
        self.config_path.as_deref()
    }

    /// The definition table this manager resolves against.
    pub fn definitions(&self) -> &[(String, KeybindingDefinition)] {
        &self.definitions
    }

    /// The underlying `pi-tui` manager.
    pub fn inner(&self) -> &TuiKeybindingsManager {
        &self.inner
    }

    /// Consume this manager and return the underlying `pi-tui` manager.
    pub fn into_inner(self) -> TuiKeybindingsManager {
        self.inner
    }

    /// The configured overrides, in declaration order.
    pub fn get_user_bindings(&self) -> &KeybindingsConfig {
        self.inner.get_user_bindings()
    }

    /// Replace the user overrides and re-resolve every binding.
    pub fn set_user_bindings(&mut self, user_bindings: KeybindingsConfig) {
        self.inner.set_user_bindings(user_bindings);
    }

    /// Every definition's effective chords, in table order.
    pub fn get_resolved_bindings(&self) -> Vec<(String, Vec<String>)> {
        self.inner.get_resolved_bindings()
    }

    /// The chords bound to `keybinding` (`[]` when unknown or unbound).
    pub fn get_keys(&self, keybinding: &str) -> Vec<String> {
        self.inner.get_keys(keybinding)
    }

    /// The definition of `keybinding`, if it is part of the table.
    pub fn get_definition(&self, keybinding: &str) -> Option<&KeybindingDefinition> {
        self.inner.get_definition(keybinding)
    }

    /// The user overrides that claim the same chord.
    pub fn get_conflicts(&self) -> &[pi_tui::keybindings::KeybindingConflict] {
        self.inner.get_conflicts()
    }

    /// The definition ids, in table order.
    pub fn keybindings(&self) -> impl Iterator<Item = &str> + '_ {
        self.inner.keybindings()
    }

    /// True when the event triggers `keybinding`.
    pub fn matches(&self, event: &pi_tui::input::InputEvent, keybinding: &str) -> bool {
        self.inner.matches(event, keybinding)
    }
}

/// Install `manager`'s resolved table as the process-wide `pi-tui`
/// keybindings.
///
/// The `pi-tui` components resolve every chord through
/// [`pi_tui::keybindings::get_keybindings`], so this is the bridge from the
/// coding-agent config layer to the consumer: upstream
/// `setKeybindings(manager)`.
pub fn install_keybindings(manager: &KeybindingsManager) {
    pi_tui::keybindings::set_keybindings(manager.inner().clone());
}

/// Build the merged coding-agent table for `agent_dir` and install it as the
/// process-wide `pi-tui` keybindings.
///
/// Returns the manager so the caller can later
/// [`reload_keybindings`] it. The interactive TTY path is the only caller:
/// print / RPC / no-TTY runs have no chords to resolve and must keep the
/// registry's `pi-tui` defaults.
pub fn install_keybindings_from(agent_dir: impl AsRef<Path>) -> KeybindingsManager {
    let manager = KeybindingsManager::create(agent_dir);
    install_keybindings(&manager);
    manager
}

/// Re-read `keybindings.json` and re-install the resolved table.
///
/// Installing once at startup is not enough on its own: the registry holds a
/// clone of the inner table, so a changed `keybindings.json` only reaches the
/// TUI when the new table is installed again.
pub fn reload_keybindings(manager: &mut KeybindingsManager) {
    manager.reload();
    install_keybindings(manager);
}
