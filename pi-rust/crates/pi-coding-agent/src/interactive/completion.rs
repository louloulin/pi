//! Extension surface helpers used by `run_interactive`.
//!
//! Migrated from `interactive.rs` on 2026-09-26 (PR6 of the M1 single-file
//! split; see `scripts/architecture_no_regression.sh`).
//!
//! Four functions cluster here because they all project the loaded
//! extension report onto the driver's runtime:
//!
//! - [`extension_header_for`] turns the report into the header row that
//!   `run_interactive` paints before the first prompt.
//! - [`install_composer_autocomplete`] publishes the built-in
//!   `/`-command autocomplete provider on the editor.
//! - [`install_extension_autocomplete`] hands that provider to the
//!   extension host and stacks the JS chain on top (LUM-1448, upstream
//!   `setupAutocompleteProvider`).
//! - [`extension_autocomplete_commands`] reads the registered commands
//!   out of the extension report and shapes them as dropdown rows for
//!   the composer.
//!
//! None of these are part of the picker or session machinery; they are
//! the extension → driver plumbing the driver needs once at startup.

use std::path::PathBuf;
use std::sync::Arc;

use pi_tui::app::App;
use pi_tui::app::ExtensionHeader;
use pi_tui::autocomplete::AutocompleteProvider;

use crate::extensions::wiring::ExtensionRuntime;

use super::InteractiveOptions;

/// Project the extension report onto the startup header's extension row.
///
/// `--no-extensions` is its own state (the header says so); an empty report
/// hides the row, which keeps a no-extension run byte-identical to the
/// pre-Stage-71 header.
pub(super) fn extension_header_for(options: &InteractiveOptions) -> ExtensionHeader {
    let report = &options.extension_report;
    if report.disabled {
        return ExtensionHeader::Disabled;
    }
    if report.loaded.is_empty() {
        return ExtensionHeader::Hidden;
    }
    let home = crate::paths::home_dir();
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    ExtensionHeader::Loaded {
        count: report.loaded.len(),
        names: report
            .loaded
            .iter()
            .map(|path| crate::commands::display_path(path, home.as_deref(), &cwd))
            .collect(),
    }
}

/// Install the built-in `/`-command autocomplete provider and return it.
///
/// The same instance is handed to the extension host as the chain's
/// `current` delegate, so the JS chain and the editor complete through
/// one provider (LUM-1448).
pub(super) fn install_composer_autocomplete(
    app: &mut App,
    base_path: PathBuf,
    extra: Vec<pi_tui::autocomplete::SlashCommand>,
) -> Arc<dyn AutocompleteProvider> {
    let mut commands = crate::commands::slash::autocomplete_commands();
    commands.extend(extra);
    let provider: Arc<dyn AutocompleteProvider> = Arc::new(
        pi_tui::autocomplete::CombinedAutocompleteProvider::new(commands, base_path),
    );
    app.prompt_mut()
        .editor_mut()
        .set_autocomplete_provider(provider.clone());
    provider
}

/// Stack the extension wrapper chain on top of the built-in provider.
///
/// The wrapper chain itself lives in the shim (the wrappers are JS
/// closures), so what happens here is: hand the built-in provider to the
/// host as the chain's `current` delegate, ask the chain for its
/// deduplicated trigger table
/// (`interactive-mode.ts:736-743`), and install the composed provider —
/// base underneath, JS chain on top — on the editor. Returns `true`
/// when a chain was installed; `false` leaves the built-in provider in
/// place.
pub(super) async fn install_extension_autocomplete(
    app: &mut App,
    runtime: &Arc<ExtensionRuntime>,
    base: Arc<dyn AutocompleteProvider>,
) -> bool {
    runtime.autocomplete_base().set(base.clone());
    if !runtime.has_autocomplete_wrapper() {
        return false;
    }
    let Some(trigger_characters) = runtime.rebuild_autocomplete().await else {
        return false;
    };
    let Some(host) = runtime.host().cloned() else {
        return false;
    };
    // The factory receives the provider it stacks on (the built-in one)
    // and hands it to the bridge as the fallback used whenever the JS
    // chain cannot answer; `compose_provider` is what turns the chain's
    // trigger characters into the editor's table.
    let composed =
        crate::extensions::autocomplete::compose_provider(host, base, trigger_characters);
    app.prompt_mut()
        .editor_mut()
        .set_autocomplete_provider(composed);
    true
}

/// The extension-registered commands as dropdown rows.
pub(super) fn extension_autocomplete_commands(
    options: &InteractiveOptions,
) -> Vec<pi_tui::autocomplete::SlashCommand> {
    options
        .extensions
        .as_ref()
        .map(|runtime| {
            runtime
                .commands()
                .iter()
                .map(|command| {
                    let entry = pi_tui::autocomplete::SlashCommand::new(command.name.clone());
                    if command.description.is_empty() {
                        entry
                    } else {
                        entry.with_description(command.description.clone())
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}