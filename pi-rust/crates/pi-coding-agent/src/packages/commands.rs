//! `pi` package-ecosystem subcommands.
//!
//! Entry point for `pi version`, `pi list-models`, `pi update-models`,
//! `pi install`, `pi remove`, and `pi list`. `main.rs` maps its CLI
//! `Command` variants onto [`PackageCommand`] and calls [`run`], so each
//! `ModeTarget` arm stays a couple of lines.

use std::io;
use std::path::{Path, PathBuf};

use pi_ai::models::Models;
use pi_ai::providers::registry::model_pricing;
use thiserror::Error;

use super::installer::{self, InstallError, RealFetcher};
use super::registry::{Registry, RegistryError};
use super::spec::SpecError;
use crate::print_mode::OutputFormat;

/// A resolved package subcommand.
#[derive(Debug, Clone)]
pub enum PackageCommand {
    /// `pi version [--output json]`.
    Version {
        /// Output format (`text` or `json`).
        output: OutputFormat,
    },
    /// `pi list-models [--output json]`.
    ListModels {
        /// Output format (`text` or `json`).
        output: OutputFormat,
    },
    /// `pi update-models`.
    UpdateModels,
    /// `pi install <spec> [--dir <root>]`.
    Install {
        /// Package spec.
        spec: String,
        /// Explicit pi root override.
        dir: Option<PathBuf>,
    },
    /// `pi remove <spec> [--dir <root>]`.
    Remove {
        /// Package spec.
        spec: String,
        /// Explicit pi root override.
        dir: Option<PathBuf>,
    },
    /// `pi list [--dir <root>] [--output json]`.
    List {
        /// Explicit pi root override.
        dir: Option<PathBuf>,
        /// Output format (`text` or `json`).
        output: OutputFormat,
    },
}

/// Errors surfaced by package subcommands.
#[derive(Debug, Error)]
pub enum CommandError {
    /// Invalid spec.
    #[error(transparent)]
    Spec(#[from] SpecError),
    /// Install / remove failure.
    #[error(transparent)]
    Install(#[from] InstallError),
    /// Registry read / write failure.
    #[error(transparent)]
    Registry(#[from] RegistryError),
    /// `pi remove` found no matching package.
    #[error("no matching package found for {0}")]
    NotFound(String),
    /// A filesystem operation failed.
    #[error(transparent)]
    Io(#[from] io::Error),
    /// JSON serialization failed.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    /// Anything else surfaced by a subcommand (e.g. refreshing a remote
    /// catalog). The string is shown verbatim to the user; the exit
    /// code maps to `EX_SOFTWARE` (70) so callers can distinguish
    /// genuine I/O failures from runtime / network ones.
    #[error("{0}")]
    Other(String),
}

impl CommandError {
    /// `sysexits.h`-style exit code for the CLI.
    pub fn exit_code(&self) -> u8 {
        match self {
            CommandError::Spec(_) => 64,                           // EX_USAGE
            CommandError::NotFound(_) => 66,                       // EX_NOINPUT
            CommandError::Install(err) => err.exit_code(),         // maps 64/66/69/74
            CommandError::Registry(_) | CommandError::Io(_) => 74, // EX_IOERR
            CommandError::Json(_) | CommandError::Other(_) => 70,   // EX_SOFTWARE
        }
    }
}

/// Execute a package subcommand, writing results to stdout.
pub fn run(command: PackageCommand, models: &Models) -> Result<(), CommandError> {
    match command {
        PackageCommand::Version { output } => run_version(models, output),
        PackageCommand::ListModels { output } => run_list_models(models, output),
        PackageCommand::UpdateModels => run_update_models(models),
        PackageCommand::Install { spec, dir } => run_install(&spec, dir.as_deref()),
        PackageCommand::Remove { spec, dir } => run_remove(&spec, dir.as_deref()),
        PackageCommand::List { dir, output } => run_list(dir.as_deref(), output),
    }
}

fn run_version(models: &Models, output: OutputFormat) -> Result<(), CommandError> {
    let version = env!("CARGO_PKG_VERSION");
    let providers = provider_ids(models);
    match output {
        OutputFormat::Json | OutputFormat::JsonEvents => {
            let payload = serde_json::json!({
                "name": "pi",
                "version": version,
                "providers": providers,
            });
            println!("{}", serde_json::to_string(&payload)?);
        }
        OutputFormat::Text => {
            println!("pi {version}");
            println!("providers:");
            for provider in &providers {
                println!("  {provider}");
            }
        }
    }
    Ok(())
}

fn run_list_models(models: &Models, output: OutputFormat) -> Result<(), CommandError> {
    let entries = sorted_models(models);
    match output {
        OutputFormat::Json | OutputFormat::JsonEvents => {
            let payload = serde_json::json!({
                "models": entries
                    .iter()
                    .map(|(provider, model)| {
                        let pricing = model_pricing(provider, &model.id).map(|p| {
                            serde_json::json!({
                                "inputPer1M": p.input_usd(),
                                "outputPer1M": p.output_usd(),
                                "cacheReadPer1M": p.cache_read_usd(),
                                "cacheWritePer1M": p.cache_write_usd(),
                            })
                        });
                        serde_json::json!({
                            "provider": provider,
                            "id": model.id,
                            "label": model.label,
                            "contextWindow": model.context_window,
                            "maxOutputTokens": model.max_output_tokens,
                            "pricing": pricing,
                        })
                    })
                    .collect::<Vec<_>>(),
            });
            println!("{}", serde_json::to_string(&payload)?);
        }
        OutputFormat::Text => {
            for (provider, model) in &entries {
                let label = model
                    .label
                    .as_deref()
                    .map(|label| format!("  {label}"))
                    .unwrap_or_default();
                let pricing = model_pricing(provider, &model.id)
                    .map(|p| {
                        format!(
                            "  price=$ {:.2}/$ {:.2} per 1M in/out",
                            p.input_usd(),
                            p.output_usd()
                        )
                    })
                    .unwrap_or_default();
                println!(
                    "{}/{}  context={}  max_output_tokens={}{}{}",
                    provider,
                    model.id,
                    model.context_window,
                    model.max_output_tokens,
                    label,
                    pricing
                );
            }
        }
    }
    Ok(())
}

fn run_update_models(models: &Models) -> Result<(), CommandError> {
    // P38: when the user has set `PI_MODELS_CATALOG_URL` to a remote
    // `models.json` (the same envelope shape `pi_ai::Models::load_models_json`
    // parses), refresh it through `pi_ai::ModelsStore` so ETag /
    // `Last-Modified` keep the wire quiet on no-op refreshes. The
    // refreshed body lands in `~/.pi/agent/models-cache.json`; the next
    // process start picks it up via `ModelsStore::cached_models`. Without
    // an env var, the catalog is compiled in (see
    // `main.rs::build_default_models`) and the legacy "0 changes" line
    // tells scripts the command is a no-op.
    if let Ok(url) = std::env::var("PI_MODELS_CATALOG_URL") {
        if let Some(agent_dir) = crate::paths::agent_dir() {
            return run_update_models_via_store(models, &agent_dir.join("models-cache.json"), &url);
        }
    }
    let count = models.iter().count();
    println!("Model catalogs refreshed: {count} models (0 changes)");
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
fn run_update_models_via_store(
    _models: &Models,
    cache_path: &std::path::Path,
    url: &str,
) -> Result<(), CommandError> {
    // `ModelsStore::refresh_sync` builds its own runtime + client so this
    // module does not need a direct `reqwest` dependency.
    let mut store = pi_ai::ModelsStore::load_or_default(cache_path.to_path_buf());
    let user_agent = concat!("pi-rust/", env!("CARGO_PKG_VERSION"));
    match store.refresh_sync(url, user_agent) {
        Ok(pi_ai::RefreshOutcome::NotModified) => {
            println!(
                "Model catalogs refreshed: 0 changes (etag={:?}, last-modified={:?})",
                store.etag(),
                store.last_modified(),
            );
            Ok(())
        }
        Ok(pi_ai::RefreshOutcome::Updated { provider_count }) => {
            println!(
                "Model catalogs refreshed: {provider_count} providers updated (cache: {})",
                cache_path.display()
            );
            Ok(())
        }
        Err(err) => Err(CommandError::Other(format!("{err}"))),
    }
}

#[cfg(target_arch = "wasm32")]
fn run_update_models_via_store(
    _models: &Models,
    _cache_path: &std::path::Path,
    _url: &str,
) -> Result<(), CommandError> {
    let count = _models.iter().count();
    println!("Model catalogs refreshed: {count} models (0 changes)");
    Ok(())
}

fn run_install(spec: &str, dir: Option<&Path>) -> Result<(), CommandError> {
    let root = installer::resolve_root(dir);
    let cwd = std::env::current_dir()?;
    let entry = installer::install(&root, &cwd, spec, &RealFetcher)?;
    println!("Installed {} ({})", entry.spec, entry.resolved);
    Ok(())
}

fn run_remove(spec: &str, dir: Option<&Path>) -> Result<(), CommandError> {
    let root = installer::resolve_root(dir);
    if installer::remove(&root, spec)? {
        println!("Removed {spec}");
        Ok(())
    } else {
        Err(CommandError::NotFound(spec.to_string()))
    }
}

fn run_list(dir: Option<&Path>, output: OutputFormat) -> Result<(), CommandError> {
    let root = installer::resolve_root(dir);
    let registry = Registry::load(&root)?;
    let packages = registry.packages();
    match output {
        OutputFormat::Json | OutputFormat::JsonEvents => {
            let payload = serde_json::json!({ "packages": packages });
            println!("{}", serde_json::to_string(&payload)?);
        }
        OutputFormat::Text => {
            for entry in packages {
                println!("{}  {}  {}", entry.name, entry.spec, entry.resolved);
            }
        }
    }
    Ok(())
}

fn provider_ids(models: &Models) -> Vec<String> {
    let mut providers: Vec<String> = models
        .iter()
        .map(|(provider, _)| provider.to_string())
        .collect();
    providers.sort();
    providers.dedup();
    providers
}

fn sorted_models(models: &Models) -> Vec<(String, pi_protocol::Model)> {
    let mut entries: Vec<(String, pi_protocol::Model)> = models
        .iter()
        .map(|(provider, model)| (provider.to_string(), model.clone()))
        .collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.id.cmp(&b.1.id)));
    entries
}
