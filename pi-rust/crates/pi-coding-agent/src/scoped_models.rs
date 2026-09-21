//! `/scoped-models` — the "Model Configuration" panel.
//!
//! Port of `packages/coding-agent/src/modes/interactive/components/scoped-models-selector.ts`.
//! Upstream's component is a `Container` that owns the whole list; this port
//! keeps the *state machine* here and renders through the shared
//! [`pi_tui::selector::Selector`] the other pickers use, so the panel inherits
//! search, windowing and the footer plumbing (same split as `/resume` and
//! `/tree`, see `PickerState`).
//!
//! What the panel does — exactly upstream's semantics:
//!
//! | chord | id | behaviour |
//! |---|---|---|
//! | `Enter` | `tui.select.confirm` | toggle the highlighted model |
//! | `Ctrl+A` | `app.models.enableAll` | enable every listed model (the *filtered* set when a search is active) |
//! | `Ctrl+X` | `app.models.clearAll` | clear the enabled set (filtered when a search is active) |
//! | `Ctrl+P` | `app.models.toggleProvider` | enable/disable every model of the highlighted model's provider |
//! | `Alt+Up` | `app.models.reorderUp` | move the highlighted *enabled* model one slot up |
//! | `Alt+Down` | `app.models.reorderDown` | move it one slot down |
//! | `Ctrl+S` | `app.models.save` | persist the current selection to `settings.json` |
//!
//! Changes are **session-only** until `Ctrl+S` (upstream comment on the
//! component: "Changes are session-only until explicitly persisted with
//! Ctrl+S"), which is why the panel tracks [`ScopedModelsPanel::dirty`] and
//! shows `(unsaved)` in its footer.
//!
//! The enabled set is `Option<Vec<String>>`: `None` means "every model is in
//! scope, no filter" — upstream's `EnabledIds = string[] | null`. Every
//! function below is a line-by-line port of the module-level helpers in
//! `scoped-models-selector.ts:17-70`; the tests at the bottom pin the
//! behaviours that are easy to get subtly wrong (collapse-back-to-`None`,
//! reorder bounds, `clearAll` when nothing was ever filtered).

use std::collections::HashMap;

use pi_protocol::{Model, ProviderId};
use pi_tui::fuzzy;
use pi_tui::locale::format_chord;
use pi_tui::selector::SelectorItem;

/// The upstream `EnabledIds` type: `None` = all enabled, `Some(ids)` = an
/// explicit ordered allow-list.
pub type EnabledModels = Option<Vec<String>>;

/// The selector title, and the value prefix that lets the driver recognise
/// the picker (`picker_kind`).
pub const PANEL_TITLE: &str = "Model Configuration";
/// Values the panel renders carry this prefix, like `resume:` / `tree:`.
pub const VALUE_PREFIX: &str = "scoped:";
/// Upstream `maxVisible = 8` (`scoped-models-selector.ts:135`).
pub const MAX_VISIBLE: usize = 8;

/// `provider/modelId` — the identity upstream keys its map by.
pub fn full_id(provider: &ProviderId, model: &Model) -> String {
    format!("{provider}/{}", model.id)
}

/// The model id inside a panel value (`scoped:anthropic/claude-x` →
/// `anthropic/claude-x`).
pub fn value_to_id(value: &str) -> Option<&str> {
    value.strip_prefix(VALUE_PREFIX)
}

/// Upstream `isEnabled`.
pub fn is_enabled(enabled: &EnabledModels, id: &str) -> bool {
    match enabled {
        None => true,
        Some(ids) => ids.iter().any(|candidate| candidate == id),
    }
}

/// Upstream `normalizeEnabled`: collapse an explicit list back to `None`
/// when it covers every available model, so "all" never needs an explicit
/// list of 27 ids.
pub fn normalize_enabled(result: Vec<String>, all_ids: &[String]) -> EnabledModels {
    if result.len() == all_ids.len() && result.iter().all(|id| all_ids.contains(id)) {
        None
    } else {
        Some(result)
    }
}

/// Upstream `toggle`. Toggling *on* appends to the end of the list and
/// re-collapses when the last model is enabled.
pub fn toggle(enabled: &EnabledModels, all_ids: &[String], id: &str) -> EnabledModels {
    match enabled {
        None => Some(
            all_ids
                .iter()
                .filter(|candidate| candidate.as_str() != id)
                .cloned()
                .collect(),
        ),
        Some(ids) => match ids.iter().position(|candidate| candidate == id) {
            Some(index) => {
                let mut next = ids.clone();
                next.remove(index);
                Some(next)
            }
            None => {
                let mut next = ids.clone();
                next.push(id.to_string());
                normalize_enabled(next, all_ids)
            }
        },
    }
}

/// Upstream `enableAll`. `None` means "no search filter, target the whole
/// catalog"; `Some(list)` is the filtered subset, which may legitimately be
/// empty (a query that matches nothing enables nothing).
pub fn enable_all(
    enabled: &EnabledModels,
    all_ids: &[String],
    targets: Option<&[String]>,
) -> EnabledModels {
    let Some(ids) = enabled else {
        // Already all enabled.
        return None;
    };
    let targets: Vec<String> = match targets {
        Some(targets) => targets.to_vec(),
        None => all_ids.to_vec(),
    };
    let mut result = ids.clone();
    for id in targets {
        if !result.contains(&id) {
            result.push(id);
        }
    }
    normalize_enabled(result, all_ids)
}

/// Upstream `clearAll`. With `targets`, only those ids leave the scope; with
/// `None`, the scope becomes explicitly empty (`Some(vec![])`).
pub fn clear_all(
    enabled: &EnabledModels,
    all_ids: &[String],
    targets: Option<&[String]>,
) -> EnabledModels {
    match enabled {
        None => match targets {
            Some(targets) => Some(
                all_ids
                    .iter()
                    .filter(|id| !targets.contains(id))
                    .cloned()
                    .collect(),
            ),
            None => Some(Vec::new()),
        },
        Some(ids) => {
            // No filter: clearing means clearing the whole selection.
            let targets: Vec<String> = match targets {
                Some(targets) => targets.to_vec(),
                None => ids.clone(),
            };
            Some(
                ids.iter()
                    .filter(|id| !targets.contains(id))
                    .cloned()
                    .collect(),
            )
        }
    }
}

/// Upstream `move`: swap `id` with its neighbour `delta` slots away inside
/// the enabled list. Out-of-range moves (and an id that is not enabled)
/// return the list unchanged.
pub fn move_model(enabled: &EnabledModels, id: &str, delta: isize) -> EnabledModels {
    let Some(ids) = enabled else {
        return None;
    };
    let Some(index) = ids.iter().position(|candidate| candidate == id) else {
        return Some(ids.clone());
    };
    let new_index = index as isize + delta;
    if new_index < 0 || new_index >= ids.len() as isize {
        return Some(ids.clone());
    }
    let mut result = ids.clone();
    result.swap(index, new_index as usize);
    Some(result)
}

/// Upstream `getSortedIds`: the enabled models in their explicit order,
/// followed by every remaining catalog model in catalog order.
pub fn sorted_ids(enabled: &EnabledModels, all_ids: &[String]) -> Vec<String> {
    match enabled {
        None => all_ids.to_vec(),
        Some(ids) => {
            let mut result = ids.clone();
            result.extend(
                all_ids
                    .iter()
                    .filter(|id| !ids.iter().any(|candidate| candidate == *id))
                    .cloned(),
            );
            result
        }
    }
}

/// The whole panel: the catalog it browses, the enabled set, and the
/// transient UI state upstream keeps in `ScopedModelsSelectorComponent`.
///
/// It lives in `PickerState` so it outlives a selector open — upstream's
/// component is created per open but *seeded* from the session scope, and
/// re-opening it must not lose the user's selection or its dirty flag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopedModelsPanel {
    /// Every catalog model as `provider/modelId`, in the catalog's stable
    /// order (see `sorted_models`).
    all_ids: Vec<String>,
    /// `provider` per id — the provider column and `app.models.toggleProvider`
    /// need it, and upstream keeps the same `modelsById` map.
    providers: HashMap<String, String>,
    /// The enabled set (`None` = all).
    enabled: EnabledModels,
    /// Upstream `isDirty`: a change that has not been written to
    /// `settings.json` yet.
    dirty: bool,
    /// Status line under the list (`onPersist`'s "saved" confirmation, or a
    /// failed write).
    status: Option<String>,
}

impl ScopedModelsPanel {
    /// Build the panel for `catalog`, seeded from the persisted
    /// `enabledModels` list.
    ///
    /// Ids in `configured` that the catalog does not know are kept: upstream
    /// reports them as unavailable rows (`a3ee1d286`, "expose unavailable
    /// scoped models") instead of dropping the user's configuration.
    pub fn new(catalog: &[(ProviderId, Model)], configured: Option<&[String]>) -> Self {
        let mut panel = Self {
            all_ids: Vec::new(),
            providers: HashMap::new(),
            enabled: None,
            dirty: false,
            status: None,
        };
        panel.update_catalog(catalog);
        if let Some(configured) = configured {
            panel.enabled = Some(configured.to_vec());
        }
        panel
    }

    /// Replace the catalog without touching the scope (upstream
    /// `updateModels`). Providers can gain/lose models between opens (the
    /// panel refreshes catalogues), so the id list is re-derived.
    pub fn update_catalog(&mut self, catalog: &[(ProviderId, Model)]) {
        self.all_ids.clear();
        self.providers.clear();
        for (provider, model) in catalog {
            let id = full_id(provider, model);
            self.providers.insert(id.clone(), provider.to_string());
            self.all_ids.push(id);
        }
    }

    /// The live scope (`None` = every model).
    pub fn enabled(&self) -> &EnabledModels {
        &self.enabled
    }

    /// True when a change is not persisted yet.
    pub fn dirty(&self) -> bool {
        self.dirty
    }

    /// The last status message, if any.
    pub fn status(&self) -> Option<&str> {
        self.status.as_deref()
    }

    /// Record the outcome of a `Ctrl+S` write.
    pub fn mark_saved(&mut self, status: impl Into<String>) {
        self.dirty = false;
        self.status = Some(status.into());
    }

    /// The order the list renders in.
    pub fn visible_ids(&self) -> Vec<String> {
        sorted_ids(&self.enabled, &self.all_ids)
    }

    /// Upstream `updateSessionModels`: the ordered scope the `Ctrl+P` cycle
    /// should walk, or `None` when the cycle must use the whole catalog.
    ///
    /// The three `None` cases are upstream's, verbatim: no explicit scope,
    /// an empty scope, and "every available model is enabled" all leave
    /// `session.scopedModels` empty, and an empty scope cycles everything.
    pub fn cycle_scope(&self) -> Option<Vec<String>> {
        let ids = self.enabled.as_ref()?;
        if ids.is_empty() {
            return None;
        }
        if !ids.iter().any(|id| self.all_ids.contains(id)) {
            return None;
        }
        if self
            .all_ids
            .iter()
            .all(|id| ids.iter().any(|candidate| candidate == id))
        {
            return None;
        }
        Some(
            ids.iter()
                .filter(|id| self.all_ids.contains(id))
                .cloned()
                .collect(),
        )
    }

    /// The ids a search query matches, in the renderer's rank order — the
    /// target set for `enableAll` / `clearAll` while a filter is active.
    pub fn filtered_ids(&self, query: &str) -> Vec<String> {
        let items = self.items();
        fuzzy::fuzzy_rank(&items, query, |item| item.search_text())
            .into_iter()
            .filter_map(|index| value_to_id(&items[index].value).map(str::to_string))
            .collect()
    }

    /// The rows, in `visible_ids` order.
    pub fn items(&self) -> Vec<SelectorItem> {
        self.visible_ids()
            .into_iter()
            .map(|id| {
                let enabled = is_enabled(&self.enabled, &id);
                let provider = self.providers.get(&id);
                let label = match (enabled, provider.is_some()) {
                    (true, true) => format!("✓ {id}"),
                    (false, true) => id.clone(),
                    // Not in the catalog any more: upstream strikes the id
                    // through and badges it `[unavailable]`.
                    (true, false) => format!("✓ {id}"),
                    (false, false) => id.clone(),
                };
                let description = match provider {
                    Some(provider) => provider.clone(),
                    None => "unavailable".to_string(),
                };
                SelectorItem::new(format!("{VALUE_PREFIX}{id}"), label)
                    .with_description(description)
            })
            .collect()
    }

    /// Upstream `getFooterText`, rendered as selector footer lines.
    ///
    /// Upstream builds **one** long `Text` line and lets the component tree
    /// wrap it; the shared [`pi_tui::selector::Selector`] footer does not wrap
    /// (it is drawn into exactly one row per entry), so the port splits the
    /// same information across four short lines instead — at 120 columns
    /// upstream's single line already ran past the edge and the clipped tail
    /// took the `(unsaved)` marker with it (LUM-1274 PTY capture).
    pub fn footer(&self) -> Vec<String> {
        let enabled_count = self
            .enabled
            .as_ref()
            .map(|ids| ids.iter().filter(|id| self.all_ids.contains(*id)).count())
            .unwrap_or(self.all_ids.len());
        let unavailable = self
            .enabled
            .as_ref()
            .map(|ids| ids.iter().filter(|id| !self.all_ids.contains(*id)).count())
            .unwrap_or(0);
        let count = if self.enabled.is_none() {
            "all enabled".to_string()
        } else if unavailable > 0 {
            format!(
                "{enabled_count}/{} enabled · {unavailable} unavailable",
                self.all_ids.len()
            )
        } else {
            format!("{enabled_count}/{} enabled", self.all_ids.len())
        };
        let mut line = format!(
            "  {} toggle · {} enable all · {} clear all",
            chord("tui.select.confirm", "enter"),
            chord("app.models.enableAll", "ctrl+a"),
            chord("app.models.clearAll", "ctrl+x"),
        );
        let mut lines = vec![
            line,
            format!(
                "  {} toggle provider · {} / {} reorder · {} save",
                chord("app.models.toggleProvider", "ctrl+p"),
                chord("app.models.reorderUp", "alt+up"),
                chord("app.models.reorderDown", "alt+down"),
                chord("app.models.save", "ctrl+s"),
            ),
            format!(
                "  Session-only. {} to save to settings.",
                chord("app.models.save", "ctrl+s")
            ),
        ];
        line = format!("  {count}");
        if self.dirty {
            // Upstream appends the same marker: `theme.fg("warning", "(unsaved)")`.
            line.push_str(" (unsaved)");
        }
        lines.push(line);
        if let Some(status) = &self.status {
            lines.push(format!("  {status}"));
        }
        lines
    }

    /// Toggle one model (`Enter`). Returns true when the scope changed.
    pub fn toggle(&mut self, id: &str) -> bool {
        let next = toggle(&self.enabled, &self.all_ids, id);
        self.commit(next)
    }

    /// Enable every listed model, or the whole catalog when `targets` is
    /// `None` (no search active).
    pub fn enable_all(&mut self, targets: Option<&[String]>) -> bool {
        let next = enable_all(&self.enabled, &self.all_ids, targets);
        self.commit(next)
    }

    /// Clear the enabled set, or only `targets` when a search is active.
    pub fn clear_all(&mut self, targets: Option<&[String]>) -> bool {
        let next = clear_all(&self.enabled, &self.all_ids, targets);
        self.commit(next)
    }

    /// Toggle every model of the highlighted model's provider: all-on →
    /// clear that provider, otherwise enable it.
    pub fn toggle_provider(&mut self, id: &str) -> bool {
        let Some(provider) = self.providers.get(id).cloned() else {
            // Upstream requires `item?.model`; an unavailable row is a no-op.
            return false;
        };
        let provider_ids: Vec<String> = self
            .all_ids
            .iter()
            .filter(|candidate| self.providers.get(*candidate) == Some(&provider))
            .cloned()
            .collect();
        let all_enabled = provider_ids
            .iter()
            .all(|candidate| is_enabled(&self.enabled, candidate));
        let next = if all_enabled {
            clear_all(&self.enabled, &self.all_ids, Some(&provider_ids))
        } else {
            enable_all(&self.enabled, &self.all_ids, Some(&provider_ids))
        };
        self.commit(next)
    }

    /// Move an enabled model inside the scope. Returns true when it moved.
    pub fn reorder(&mut self, id: &str, delta: isize) -> bool {
        if self.enabled.is_none() || !is_enabled(&self.enabled, id) {
            return false;
        }
        let next = move_model(&self.enabled, id, delta);
        self.commit(next)
    }

    /// Store `next`, flagging dirtiness only when something really changed
    /// (upstream sets `isDirty = true` unconditionally on those chords, but a
    /// no-op reorder that never moved should not claim an unsaved edit).
    fn commit(&mut self, next: EnabledModels) -> bool {
        if next == self.enabled {
            return false;
        }
        self.enabled = next;
        self.dirty = true;
        self.status = None;
        true
    }
}

/// The chord currently bound to `id`, or `builtin` when the table has none.
///
/// Upstream renders the footer through `keyDisplayText(id)`, which reads the
/// effective keybindings, so a user who rebinds `app.models.save` sees the
/// new chord in the panel. The `pi-tui` registry is process-global and the
/// driver installs the merged table at startup (see `install_keybindings`).
fn chord(id: &str, builtin: &str) -> String {
    let keybindings = pi_tui::keybindings::get_keybindings();
    let keys = keybindings.get_keys(id);
    let key = keys.first().map(String::as_str).unwrap_or(builtin);
    format_chord(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_protocol::Api;

    fn model(provider: &str, id: &str) -> Model {
        Model {
            provider: ProviderId::new(provider),
            id: id.to_string(),
            api: Api::Faux,
            label: None,
            context_window: 0,
            max_output_tokens: 0,
        }
    }

    fn catalog(ids: &[(&str, &str)]) -> Vec<(ProviderId, Model)> {
        ids.iter()
            .map(|(provider, id)| (ProviderId::new(*provider), model(provider, id)))
            .collect()
    }

    fn all_ids(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|id| (*id).to_string()).collect()
    }

    #[test]
    fn is_enabled_treats_none_as_everything() {
        assert!(is_enabled(&None, "anthropic/a"));
        assert!(is_enabled(&Some(all_ids(&["anthropic/a"])), "anthropic/a"));
        assert!(!is_enabled(&Some(all_ids(&["anthropic/a"])), "anthropic/b"));
    }

    #[test]
    fn normalize_collapses_a_complete_list_to_none() {
        let all = all_ids(&["a", "b"]);
        assert_eq!(normalize_enabled(all_ids(&["a", "b"]), &all), None);
        assert_eq!(
            normalize_enabled(all_ids(&["a"]), &all),
            Some(all_ids(&["a"]))
        );
    }

    #[test]
    fn toggle_from_none_disables_exactly_one() {
        let all = all_ids(&["a", "b", "c"]);
        assert_eq!(toggle(&None, &all, "b"), Some(all_ids(&["a", "c"])));
    }

    #[test]
    fn toggle_appends_and_re_collapses() {
        let all = all_ids(&["a", "b"]);
        assert_eq!(
            toggle(&Some(all_ids(&["a"])), &all, "b"),
            None,
            "enabling the last missing model is `all enabled` again"
        );
        assert_eq!(
            toggle(&Some(all_ids(&["a", "c"])), &all, "b"),
            Some(all_ids(&["a", "c", "b"]))
        );
    }

    #[test]
    fn toggle_removes_an_enabled_model() {
        let all = all_ids(&["a", "b"]);
        assert_eq!(
            toggle(&Some(all_ids(&["a", "b"])), &all, "a"),
            Some(all_ids(&["b"]))
        );
    }

    #[test]
    fn enable_all_without_targets_is_none() {
        let all = all_ids(&["a", "b"]);
        let enabled = Some(all_ids(&["a"]));
        assert_eq!(enable_all(&enabled, &all, Some(&all)), None);
        assert_eq!(enable_all(&None, &all, Some(&all)), None);
    }

    #[test]
    fn enable_all_targets_only_the_filtered_subset() {
        let all = all_ids(&["a", "b", "c"]);
        let enabled = Some(all_ids(&["a"]));
        let targets = all_ids(&["b"]);
        assert_eq!(
            enable_all(&enabled, &all, Some(&targets)),
            Some(all_ids(&["a", "b"]))
        );
    }

    #[test]
    fn clear_all_from_none_keeps_everything_but_the_targets() {
        let all = all_ids(&["a", "b", "c"]);
        let targets = all_ids(&["b"]);
        assert_eq!(
            clear_all(&None, &all, Some(&targets)),
            Some(all_ids(&["a", "c"]))
        );
        assert_eq!(clear_all(&None, &all, None), Some(Vec::new()));
    }

    #[test]
    fn clear_all_from_a_list_removes_the_targets() {
        let all = all_ids(&["a", "b", "c"]);
        let enabled = Some(all_ids(&["a", "b"]));
        assert_eq!(
            clear_all(&enabled, &all, Some(&all_ids(&["b"]))),
            Some(all_ids(&["a"]))
        );
        assert_eq!(clear_all(&enabled, &all, None), Some(Vec::new()));
    }

    #[test]
    fn move_swaps_neighbours_and_clamps() {
        let enabled = Some(all_ids(&["a", "b", "c"]));
        assert_eq!(
            move_model(&enabled, "b", -1),
            Some(all_ids(&["b", "a", "c"]))
        );
        assert_eq!(
            move_model(&enabled, "a", -1),
            enabled,
            "moving the first item up is a no-op"
        );
        assert_eq!(
            move_model(&enabled, "c", 1),
            enabled,
            "moving the last item down is a no-op"
        );
        assert_eq!(move_model(&None, "a", -1), None);
        assert_eq!(
            move_model(&Some(Vec::new()), "a", -1),
            Some(Vec::new()),
            "an id that is not in the list leaves it unchanged"
        );
    }

    #[test]
    fn sorted_ids_puts_enabled_models_first_in_their_own_order() {
        let all = all_ids(&["a", "b", "c"]);
        assert_eq!(sorted_ids(&None, &all), all);
        assert_eq!(
            sorted_ids(&Some(all_ids(&["c", "a"])), &all),
            all_ids(&["c", "a", "b"])
        );
    }

    #[test]
    fn panel_items_mark_enabled_rows_and_keep_unavailable_ones() {
        let catalog = catalog(&[("anthropic", "a"), ("openai", "b")]);
        let panel = ScopedModelsPanel::new(&catalog, Some(&all_ids(&["anthropic/a", "ghost/x"])));
        let items = panel.items();
        assert_eq!(items[0].value, "scoped:anthropic/a");
        assert_eq!(items[0].label, "✓ anthropic/a");
        assert_eq!(items[0].description.as_deref(), Some("anthropic"));
        // Enabled ids come first in their own order, the unavailable one
        // included — upstream's `getSortedIds` does not reorder by
        // availability.
        assert_eq!(items[1].value, "scoped:ghost/x");
        assert_eq!(items[1].label, "✓ ghost/x");
        assert_eq!(items[1].description.as_deref(), Some("unavailable"));
        assert_eq!(items[2].value, "scoped:openai/b");
        assert_eq!(items[2].label, "openai/b");
    }

    #[test]
    fn panel_footer_reports_the_count_and_dirty_state() {
        let catalog = catalog(&[("anthropic", "a"), ("openai", "b")]);
        let mut panel = ScopedModelsPanel::new(&catalog, None);
        // The state line is the last of four (three legend lines + the state).
        assert!(panel.footer()[3].contains("all enabled"));
        assert!(!panel.footer()[3].contains("(unsaved)"));
        assert!(panel.toggle("anthropic/a"));
        assert!(panel.dirty());
        assert!(panel.footer()[3].contains("1/2 enabled"));
        assert!(panel.footer()[3].contains("(unsaved)"));
        assert!(panel.footer()[1].contains("save"));
        assert!(
            panel.footer().iter().all(|line| line.chars().count() < 80),
            "every footer line must survive an 80-column terminal: {:?}",
            panel.footer()
        );
        panel.mark_saved("Model selection saved to settings");
        assert!(!panel.dirty());
        assert!(panel.status().is_some());
    }

    #[test]
    fn panel_toggle_provider_flips_every_model_of_that_provider() {
        let catalog = catalog(&[("anthropic", "a"), ("anthropic", "b"), ("openai", "c")]);
        let mut panel = ScopedModelsPanel::new(&catalog, None);
        // All enabled → clearing the provider disables exactly its two models.
        assert!(panel.toggle_provider("anthropic/a"));
        assert_eq!(panel.enabled(), &Some(all_ids(&["openai/c"])));
        // Not all enabled → enabling it brings both back.
        assert!(panel.toggle_provider("anthropic/b"));
        assert_eq!(panel.enabled(), &None);
        // An unavailable row has no provider to toggle.
        let mut panel = ScopedModelsPanel::new(&catalog, Some(&all_ids(&["ghost/x"])));
        assert!(!panel.toggle_provider("ghost/x"));
    }

    #[test]
    fn panel_reorder_only_moves_enabled_rows() {
        let catalog = catalog(&[("anthropic", "a"), ("anthropic", "b")]);
        let mut panel = ScopedModelsPanel::new(&catalog, None);
        // `None` (all enabled) has no explicit order to reorder.
        assert!(!panel.reorder("anthropic/a", 1));
        panel.toggle("anthropic/a");
        assert_eq!(panel.enabled(), &Some(all_ids(&["anthropic/b"])));
        assert!(!panel.reorder("anthropic/a", 1), "a is not enabled");
        assert!(!panel.reorder("anthropic/b", 0), "delta 0 cannot move");
    }

    #[test]
    fn panel_reorder_moves_within_the_enabled_list() {
        let catalog = catalog(&[("anthropic", "a"), ("anthropic", "b"), ("openai", "c")]);
        let mut panel = ScopedModelsPanel::new(&catalog, None);
        // One toggle from "all enabled" leaves exactly the other two, which is
        // already the list this test wants.
        panel.toggle("anthropic/a");
        assert_eq!(
            panel.enabled(),
            &Some(all_ids(&["anthropic/b", "openai/c"]))
        );
        assert!(panel.reorder("openai/c", -1));
        assert_eq!(
            panel.enabled(),
            &Some(all_ids(&["openai/c", "anthropic/b"]))
        );
        assert_eq!(
            panel.visible_ids(),
            all_ids(&["openai/c", "anthropic/b", "anthropic/a"]),
            "the moved row is first in the rendered order"
        );
    }

    #[test]
    fn panel_commit_reports_no_change_for_a_no_op() {
        let catalog = catalog(&[("anthropic", "a")]);
        let mut panel = ScopedModelsPanel::new(&catalog, None);
        assert!(panel.toggle("anthropic/a"), "disables the only model");
        // An unavailable row has no provider, so the provider chord is a no-op
        // that must not mark the panel dirty again.
        panel.mark_saved("saved");
        assert!(!panel.toggle_provider("ghost/x"));
        assert!(!panel.dirty(), "a rejected change is not an edit");
    }

    #[test]
    fn panel_toggling_an_unavailable_id_adds_it_to_the_scope() {
        // Upstream has no availability guard on `toggle`: an id the catalog
        // does not know is still a row the user can flip, which is how a
        // hand-written `enabledModels` entry stays editable.
        let catalog = catalog(&[("anthropic", "a")]);
        let mut panel = ScopedModelsPanel::new(&catalog, Some(&all_ids(&["ghost/x"])));
        assert!(panel.toggle("ghost/x"));
        assert_eq!(panel.enabled(), &Some(Vec::new()));
    }

    #[test]
    fn cycle_scope_matches_upstreams_three_none_cases() {
        let catalog = catalog(&[("anthropic", "a"), ("anthropic", "b"), ("openai", "c")]);
        // No explicit scope.
        let panel = ScopedModelsPanel::new(&catalog, None);
        assert_eq!(panel.cycle_scope(), None);
        // Empty scope.
        let panel = ScopedModelsPanel::new(&catalog, Some(&[]));
        assert_eq!(panel.cycle_scope(), None);
        // Everything available enabled.
        let panel = ScopedModelsPanel::new(
            &catalog,
            Some(&all_ids(&["anthropic/a", "anthropic/b", "openai/c"])),
        );
        assert_eq!(panel.cycle_scope(), None);
        // Only ids the catalog does not know.
        let panel = ScopedModelsPanel::new(&catalog, Some(&all_ids(&["ghost/x"])));
        assert_eq!(panel.cycle_scope(), None);
        // A real subset, in its explicit order.
        let panel = ScopedModelsPanel::new(&catalog, Some(&all_ids(&["openai/c", "anthropic/b"])));
        assert_eq!(
            panel.cycle_scope(),
            Some(all_ids(&["openai/c", "anthropic/b"]))
        );
    }

    #[test]
    fn filtered_ids_follows_the_search_query() {
        let catalog = catalog(&[("anthropic", "sonnet"), ("openai", "gpt")]);
        let panel = ScopedModelsPanel::new(&catalog, None);
        assert_eq!(panel.filtered_ids("").len(), 2);
        assert_eq!(panel.filtered_ids("sonnet"), all_ids(&["anthropic/sonnet"]));
    }

    #[test]
    fn update_catalog_keeps_the_scope() {
        let first = catalog(&[("anthropic", "a")]);
        let mut panel = ScopedModelsPanel::new(&first, Some(&all_ids(&["anthropic/a"])));
        panel.update_catalog(&catalog(&[("anthropic", "a"), ("openai", "b")]));
        assert_eq!(panel.enabled(), &Some(all_ids(&["anthropic/a"])));
        assert_eq!(panel.visible_ids().len(), 2);
    }

    #[test]
    fn value_round_trips_through_the_prefix() {
        assert_eq!(value_to_id("scoped:openai/gpt"), Some("openai/gpt"));
        assert_eq!(value_to_id("model:gpt"), None);
    }

    #[test]
    fn full_id_joins_provider_and_model() {
        assert_eq!(
            full_id(
                &ProviderId::new("anthropic"),
                &model("anthropic", "claude-sonnet-4")
            ),
            "anthropic/claude-sonnet-4"
        );
    }
}
