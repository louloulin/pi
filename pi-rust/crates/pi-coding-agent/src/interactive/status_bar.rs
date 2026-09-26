//! Footer status metrics + default-model selection.
//!
//! Migrated from `interactive.rs:L1589-L1650 + L4800-L4813` on 2026-09-26
//! (PR2 of the M1 single-file split; see `docs/API_STABILITY.md` and
//! `scripts/architecture_no_regression.sh`).
//!
//! Everything the footer status line (`(provider)`, ` (sub)`, ` (auto)`,
//! `$cost`) reads from the driver is computed here. These helpers are
//! referenced by [`super::run_interactive`] (model switch, model cycle)
//! and [`super::interactive_app_config`] (provider count seed).

use pi_ai::models::Models;
use pi_protocol::{Api, Model, ProviderId};
use pi_tui::app::App;

use super::InteractiveOptions;

/// LUM-1467 — hand the footer the facts only the driver owns.
///
/// Upstream's `FooterComponent` reads them off `session.state.model` and
/// `footerData` (`footer.ts:139-197`):
///
/// * how many providers are routable (`getAvailableProviderCount() > 1`),
///   which decides the model's `(provider) ` prefix — counted from the same
///   catalog `/model` and `Ctrl+P` cycle through (`options.models`);
/// * whether the active provider bills by subscription, which decides the
///   ` (sub)` suffix;
/// * whether auto-compaction is on, which decides the gauge's ` (auto)`;
/// * the active model's per-1M-token rates, so [`pi_tui::StatusData`] can
///   turn the usage events it already receives into the `$cost` part.
///
/// The rate lookup is the static catalog's (`model_pricing`); a model the
/// catalog has no rates for leaves the cost unknown, exactly like a provider
/// that reports no cost upstream.
pub(super) fn sync_status_metrics(app: &mut App, options: &InteractiveOptions, model: &Model) {
    app.set_status_provider(
        available_provider_count(options),
        Some(model.provider.0.clone()),
    );
    app.set_status_subscription(is_subscription_provider(&model.provider.0));
    app.set_status_auto_compact(options.compaction.enabled);
    app.set_status_pricing(
        pi_ai::providers::registry::model_pricing(&model.provider.0, &model.id).map(|pricing| {
            pi_tui::StatusPricing {
                input_micro_usd: pricing.input_micro_usd,
                output_micro_usd: pricing.output_micro_usd,
                cache_read_micro_usd: pricing.cache_read_micro_usd,
                cache_write_micro_usd: pricing.cache_write_micro_usd,
            }
        }),
    );
}

/// How many providers this session can route to — the same count the footer's
/// `(provider) ` prefix and `footerData.getAvailableProviderCount()` are built
/// from (`footer.ts:192`).
pub(super) fn available_provider_count(options: &InteractiveOptions) -> usize {
    let mut providers: Vec<&str> = options
        .models
        .iter()
        .map(|(provider, _)| provider.0.as_str())
        .collect();
    providers.sort_unstable();
    providers.dedup();
    providers.len()
}

/// Upstream's subscription test (`footer.ts:139-140`): Kimi Coding bills
/// through a subscription even though it authenticates with an API key, or
/// the model runtime reports the provider as subscription-backed.
///
/// Only the literal half is ported: the OAuth/subscription-first providers
/// (`kimi-coding`, `github-copilot`, `openai-codex`) are not registered in
/// this build yet (see `pi-ai/src/auth/provider_registry.rs`), so
/// `modelRuntime.isUsingSubscription` has nothing to answer — the same gap
/// the provider-auth module documents.
pub(super) fn is_subscription_provider(provider: &str) -> bool {
    provider == "kimi-coding"
}

/// Pick a deterministic default model when `run_interactive` is launched
/// without one. The catalog is already sorted upstream
/// (`AgentSession._pickInitialModel`, `agent-session.ts:2126`), so the
/// first entry is the same answer everywhere; the fallback only exists
/// for tests that hand in an empty catalog and want something printable.
pub(super) fn default_model(models: &Models) -> Model {
    super::sorted_models(models)
        .into_iter()
        .next()
        .map(|(_, m)| m)
        .unwrap_or_else(|| Model {
            provider: ProviderId::new("faux"),
            id: "faux-model".into(),
            api: Api::Faux,
            label: Some("Faux test model".into()),
            context_window: 8192,
            max_output_tokens: 1024,
        })
}