//! `SettingsList` — the settings list component.
//!
//! Mirrors upstream `SettingsList`
//! (`packages/tui/src/components/settings-list.ts`). Wraps the
//! existing [`crate::components::settings::SettingsList`].

use crate::component::Component;
use crate::components::settings::{SettingItem, SettingsList};
use crate::utils::styled::StyledLine;

/// Upstream `SettingsListComponentTheme` — theme placeholder.
#[derive(Debug, Clone, Default)]
pub struct SettingsListComponentTheme;

/// Upstream `SettingsList` component (extension-facing). The Rust
/// port wraps [`SettingsList`] so the extension surface has a
/// `Component` impl.
pub struct SettingsListComponent {
    inner: SettingsList,
}

impl SettingsListComponent {
    /// Build from a list of items, defaulting `max_visible` to the
    /// item count so every item is visible at once.
    pub fn new(items: Vec<SettingItem>) -> Self {
        let max_visible = items.len().max(1);
        Self { inner: SettingsList::new(items, max_visible) }
    }

    /// Replace the items wholesale by rebuilding the inner list.
    pub fn set_items(&mut self, items: Vec<SettingItem>) {
        let max_visible = items.len().max(1);
        self.inner = SettingsList::new(items, max_visible);
    }

    /// Borrow the inner list.
    pub fn inner(&self) -> &SettingsList {
        &self.inner
    }

    /// Mutable borrow.
    pub fn inner_mut(&mut self) -> &mut SettingsList {
        &mut self.inner
    }
}

impl Component for SettingsListComponent {
    fn render(&self, _width: u16) -> Vec<StyledLine> {
        // Each item becomes one line.
        self.inner.items().iter()
            .map(|item| {
                let label = format!("{}: {}", item.label, item.current_value);
                vec![crate::utils::styled::StyledSpan::new(label, crate::utils::styled::SpanStyle::PLAIN)]
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::settings::SettingItem;

    #[test]
    fn settings_list_renders_items() {
        let items = vec![
            SettingItem::new("theme", "Theme").with_values(vec!["a", "b"], "a"),
            SettingItem::new("font", "Font").with_values(vec!["mono"], "mono"),
        ];
        let c = SettingsListComponent::new(items);
        let lines = c.render(40);
        assert_eq!(lines.len(), 2);
        assert!(crate::utils::styled::plain_text(&lines[0]).contains("Theme"));
    }

    #[test]
    fn settings_list_set_items_replaces() {
        let mut c = SettingsListComponent::new(vec![SettingItem::new("a", "A")]);
        c.set_items(vec![
            SettingItem::new("x", "X").with_values(vec!["9"], "9"),
            SettingItem::new("y", "Y").with_values(vec!["8"], "8"),
        ]);
        let lines = c.render(40);
        assert_eq!(lines.len(), 2);
    }
}