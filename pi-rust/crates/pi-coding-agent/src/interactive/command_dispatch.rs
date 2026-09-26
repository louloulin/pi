//! Slash-command dispatcher + helpers used to render its output.
//!
//! Migrated from `interactive.rs` on 2026-09-26 (PR4 of the M1 single-file
//! split; see `docs/API_STABILITY.md` and
//! `scripts/architecture_no_regression.sh`).
//!
//! `run_slash_command` owns the entire `/`-prefixed command surface the
//! driver recognises. Most variants delegate to a more focused helper
//! (`open_*_selector` for pickers, `set_session_name` for `/name`,
//! `run_compact` for `/compact`, ...); the `SlashCommand::Unknown` arm is
//! the extension-command escape hatch — anything that did not match a
//! built-in is forwarded to the extension runtime when one is loaded.
//!
//! The four small helpers (`extension_command_args`, `command_result_text`,
//! `append_prompt_templates`, `help_text_with_extensions`) cluster here
//! because they only matter while answering a slash command.

use std::path::PathBuf;
use std::sync::Arc;

use pi_agent_core::Agent;
use pi_tui::app::App;
use tokio::sync::Mutex as AsyncMutex;

use crate::commands::tree::clone_session;
use crate::commands::{handle_command, SlashCommand};
use crate::prompt_templates::PromptTemplate;

use super::{
    apply_thinking_level, clone_into_new_session, copy_last_assistant_message, open_current_session,
    open_fork_selector, open_model_selector, open_resume_selector, open_scoped_models_selector,
    open_settings, open_thinking_selector, open_tree_selector, persist_extension_side_effects,
    run_compact, session_directory, set_session_name, settings_sources, start_new_session,
    InteractiveOptions,
};

/// Dispatch a `/<cmd>` line to the matching built-in or extension command.
///
/// Mirrors upstream `runSlashCommand` (`interactive-mode.ts:4475`). The
/// `text` is the raw line the user submitted (the leading `/` is part of
/// it); prompt templates are expanded one layer up in `handle_submitted`,
/// so by the time we land here the line is already a built-in or an
/// extension command.
pub(super) async fn run_slash_command(
    app: &mut App,
    agent: &Arc<AsyncMutex<Agent>>,
    options: &mut InteractiveOptions,
    text: &str,
) -> anyhow::Result<()> {
    let parsed = match handle_command(text) {
        Ok(cmd) => cmd,
        Err(err) => {
            app.info(format!("error: {err}"));
            return Ok(());
        }
    };
    match parsed {
        SlashCommand::Help => {
            let empty = [];
            let commands = options
                .extensions
                .as_ref()
                .map(|runtime| runtime.commands())
                .unwrap_or(empty.as_slice());
            let help = append_prompt_templates(
                help_text_with_extensions(commands),
                &options.prompt_templates,
            );
            // Command-reference output, not user input: the info prefix keeps
            // `/help`'s body from reading as something the user typed
            // (LUM-1238 §15.4).
            app.info_block(help);
        }
        SlashCommand::Clear => {
            app.messages_mut().clear();
        }
        SlashCommand::New => {
            start_new_session(app, agent, options).await;
        }
        SlashCommand::Copy => {
            copy_last_assistant_message(app);
        }
        SlashCommand::Name { name } => match name {
            Some(name) => set_session_name(app, options, &name).await,
            None => match options.session_name.as_deref() {
                Some(name) => app.info(format!("Session name: {name}")),
                None => app.info("usage: /name <name>".to_string()),
            },
        },
        SlashCommand::Exit | SlashCommand::Quit => {
            app.request_exit();
        }
        SlashCommand::Model => {
            open_model_selector(app, options);
        }
        SlashCommand::ScopedModels => {
            open_scoped_models_selector(app, options);
        }
        SlashCommand::Hotkeys => {
            app.info_block(crate::commands::slash::hotkeys_text());
        }
        SlashCommand::Extensions => {
            let home = crate::paths::home_dir();
            let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            app.info(crate::commands::extensions_text(
                &options.extension_report,
                home.as_deref(),
                &cwd,
            ));
        }
        SlashCommand::Reload => {
            // The agent dir is the one the startup path installed the merged
            // keybinding table from (`install_keybindings_from` in
            // `run_interactive`), so the reload reads the same file.
            let _ = crate::reload::reload(
                app,
                &crate::paths::agent_dir_or_default(),
                &settings_sources(),
            );
        }
        SlashCommand::Session => {
            let agent_guard = agent.lock().await;
            let state = agent_guard.state();
            match options.session_name.as_deref() {
                Some(name) => app.info(format!(
                    "session {} — name: {name}, messages={}, model={}",
                    options.session_id,
                    state.messages.len(),
                    agent_guard.model().id
                )),
                None => app.info(format!(
                    "session {} — messages={}, model={}",
                    options.session_id,
                    state.messages.len(),
                    agent_guard.model().id
                )),
            }
        }
        SlashCommand::Export { path } => {
            // Upstream `handleExportCommand`: `.jsonl` writes the session
            // branch as JSONL, anything else writes self-contained HTML.
            // The running TUI theme is forwarded so the export matches
            // what the user sees.
            let agent_guard = agent.lock().await;
            let state = agent_guard.state();
            let tools = options.tool_executor.definitions();
            let theme_name = app.theme().name().map(str::to_string);
            let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            let request = crate::commands::export::ActiveSession {
                session_id: &options.session_id,
                cwd: &cwd,
                messages: &state.messages,
                system_prompt: &state.system_prompt,
                tools: &tools,
            };
            let result = crate::commands::export::run_slash_export(
                request,
                path.as_deref(),
                theme_name.as_deref(),
            );
            drop(agent_guard);
            match result {
                Ok(file_path) => app.info(format!("Session exported to: {}", file_path.display())),
                Err(err) => app.info(format!("Failed to export session: {err}")),
            }
        }
        SlashCommand::Resume => {
            // Stage 5: drive the selector from the SQLite reader via
            // `pi_coding_agent::list_resumable` so the user sees
            // versioned, time-stamped session metadata instead of raw
            // filenames. Stage 65 moved the body into
            // `open_resume_selector` so `app.session.resume` runs the
            // exact same code instead of a second copy.
            open_resume_selector(app, options);
        }
        SlashCommand::Tree => {
            open_tree_selector(app, options);
        }
        SlashCommand::Fork => {
            open_fork_selector(app, options);
        }
        SlashCommand::Clone => {
            let Some(directory) = session_directory(options) else {
                app.info("/clone: session directory not configured".to_string());
                return Ok(());
            };
            let Some((_, reader)) = open_current_session(app, options, "/clone") else {
                return Ok(());
            };
            match clone_session(&directory, &reader, &options.session_id) {
                Ok(created) => {
                    clone_into_new_session(app, agent, options, created).await;
                }
                Err(err) => app.info(format!("/clone: {err}")),
            }
        }
        SlashCommand::Import { path: _ } => {
            // Import and resume a session from a JSONL file.
            app.info("/import: session import not yet implemented in Rust port".to_string());
        }
        SlashCommand::Share => {
            // Share the current session as a GitHub gist.
            app.info("/share: session sharing not yet implemented in Rust port".to_string());
        }
        SlashCommand::Changelog => {
            // Show changelog entries.
            app.info("/changelog: not yet implemented in Rust port".to_string());
        }
        SlashCommand::Login { provider } => {
            // Configure provider authentication. The Rust port surfaces the
            // same hint as upstream's `Models.getApiKeyForProvider`: the
            // env vars the provider reads, in priority order. Setting any one
            // makes the provider's models appear in `/model` (the model
            // selector filters by configured credential — `Models.getAvailable()`
            // mirror). OAuth-first providers stay hidden until P33 ships.
            match provider {
                Some(p) => {
                    let env_vars = pi_ai::providers::registry::api_key_env_vars(&p);
                    if env_vars.is_empty() {
                        let oauth_label = pi_ai::providers::registry::find_provider(&p)
                            .and_then(|spec| spec.oauth)
                            .map(|oauth| oauth.login_label);
                        match oauth_label {
                            Some(label) => app.info(format!(
                                "/login {p}: OAuth flow not yet implemented in Rust port (would show \"{label}\" once P33 lands)"
                            )),
                            None => app.info(format!(
                                "/login {p}: unknown provider (no api_key_env registered)"
                            )),
                        }
                    } else {
                        app.info(format!(
                            "/login {p}: set {} to authenticate; the provider's models will appear in /model once set",
                            env_vars
                                .iter()
                                .map(|name| format!("`{name}`"))
                                .collect::<Vec<_>>()
                                .join(" or ")
                        ));
                    }
                }
                None => {
                    let oauth_only: Vec<&str> = pi_ai::providers::registry::BUILTIN_PROVIDERS
                        .iter()
                        .filter(|s| !s.api_key_env.is_empty())
                        .map(|s| s.id)
                        .collect();
                    app.info(format!(
                        "/login: usage: /login <provider> — known providers: {}",
                        oauth_only.join(", ")
                    ));
                }
            }
        }
        SlashCommand::Logout { provider } => {
            // The Rust port does not own a credentials panel yet (TS upstream
            // has one); until then, `/logout` explains how to clear the
            // credential the user controls. The model selector filters by
            // the same env vars, so once cleared, the provider's models
            // disappear from `/model`. OAuth-first providers stay in the
            // "not yet wired" state until P33 lands.
            match provider {
                Some(p) => {
                    let env_vars = pi_ai::providers::registry::api_key_env_vars(&p);
                    if env_vars.is_empty() {
                        app.info(format!(
                            "/logout {p}: OAuth-based providers not yet wired in Rust port"
                        ));
                    } else {
                        app.info(format!(
                            "/logout {p}: unset {} in your shell and restart the session (no credentials panel yet)",
                            env_vars
                                .iter()
                                .map(|name| format!("`{name}`"))
                                .collect::<Vec<_>>()
                                .join(", ")
                        ));
                    }
                }
                None => app.info("/logout: provider required (usage: /logout <provider>)".to_string()),
            }
        }
        SlashCommand::Trust(action) => {
            let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            let store = crate::trust::ProjectTrustStore::new(&crate::paths::agent_dir_or_default());
            match action {
                Some(decision) => match store.set(&cwd, Some(decision)) {
                    Ok(()) => {
                        let label = if decision { "trusted" } else { "untrusted" };
                        app.info(format!(
                            "Saved trust decision for {}: {label}. Restart pi for this to take effect.",
                            cwd.display()
                        ));
                    }
                    Err(err) => app.info(format!("/trust: {err}")),
                },
                None => {
                    let has_resources = crate::trust::has_trust_requiring_project_resources(&cwd);
                    let saved = match store.get(&cwd) {
                        Ok(Some(true)) => "trusted",
                        Ok(Some(false)) => "untrusted",
                        Ok(None) => "no saved decision (defaults to untrusted)",
                        Err(err) => {
                            app.info(format!("/trust: {err}"));
                            return Ok(());
                        }
                    };
                    app.info(format!(
                        "project trust for {}: {saved}; resources that require trust: {}",
                        cwd.display(),
                        if has_resources { "yes" } else { "no" }
                    ));
                    app.info("usage: /trust yes | /trust no".to_string());
                }
            }
        }
        SlashCommand::Settings => {
            open_settings(app, options, &settings_sources());
        }
        SlashCommand::Thinking { level } => {
            let (supports, available) = {
                let agent_guard = agent.lock().await;
                let supports = crate::thinking::model_supports_thinking(agent_guard.model());
                (
                    supports,
                    crate::thinking::available_thinking_levels(supports),
                )
            };
            match level {
                Some(raw) => {
                    // Upstream matches the argument case-insensitively against
                    // the levels the *model* offers (`handleThinkingCommand`,
                    // `interactive-mode.ts:4789`), so an unsupported level is
                    // reported rather than applied.
                    let requested = crate::thinking::parse_thinking_level(&raw)
                        .filter(|candidate| available.contains(candidate));
                    match requested {
                        Some(level) => {
                            apply_thinking_level(
                                app,
                                agent,
                                level,
                                false,
                                options.extensions.as_ref(),
                            )
                            .await
                        }
                        None => app.info(format!(
                            "Unknown thinking level \"{raw}\". Available levels: {}.",
                            available
                                .iter()
                                .map(|level| level.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        )),
                    }
                }
                None => open_thinking_selector(app, supports),
            }
        }
        SlashCommand::Compact { instructions } => {
            run_compact(app, agent, options, instructions.as_deref()).await;
        }
        SlashCommand::Unknown(name) => {
            // A `/name` that is not a built-in may still belong to an
            // extension (`pi.registerCommand`). Run it when it does.
            let runtime = options.extensions.as_ref();
            if let Some(runtime) = runtime.filter(|r| r.has_command(&name)) {
                let args = extension_command_args(text);
                match runtime.execute_command(&name, args).await {
                    Ok(outcome) => {
                        if let Some(text) = command_result_text(&outcome) {
                            if !text.is_empty() {
                                app.info(text);
                            }
                        }
                        if let Some(err) = &outcome.error {
                            app.info(format!("/{name}: {err}"));
                        }
                    }
                    Err(err) => app.info(format!("/{name}: {err}")),
                }
                persist_extension_side_effects(app, options);
                return Ok(());
            }
            app.info(format!("unknown command /{name} — try /help"));
        }
    }
    Ok(())
}

/// Extract the text the user typed after the command name.
pub(super) fn extension_command_args(text: &str) -> &str {
    let rest = text.trim().trim_start_matches('/');
    match rest.find(char::is_whitespace) {
        Some(idx) => rest[idx..].trim(),
        None => "",
    }
}

/// Render a command handler's return value as display text.
pub(super) fn command_result_text(outcome: &pi_extensions::CommandExecutionOutcome) -> Option<String> {
    match &outcome.result {
        serde_json::Value::Null => None,
        serde_json::Value::String(text) => Some(text.clone()),
        other => Some(other.to_string()),
    }
}

/// Splice the loaded prompt template list into the help text.
pub(super) fn append_prompt_templates(base: String, templates: &[PromptTemplate]) -> String {
    if templates.is_empty() {
        return base;
    }
    let mut section = String::from("prompt templates:\n");
    for template in templates {
        if template.description.is_empty() {
            section.push_str(&format!("  /{}\n", template.name));
        } else {
            section.push_str(&format!(
                "  /{:<16} {}\n",
                template.name, template.description
            ));
        }
    }
    match base.split_once("\nkeys:") {
        Some((head, tail)) => format!("{head}\n\n{section}\nkeys:{tail}"),
        None => format!("{base}\n\n{section}"),
    }
}

/// Splice the extension command list into the built-in help text.
pub(super) fn help_text_with_extensions(commands: &[pi_extensions::RegisteredCommand]) -> String {
    let base = crate::commands::help_text();
    if commands.is_empty() {
        return base;
    }
    let mut section = String::from("extension commands:\n");
    for command in commands {
        if command.description.is_empty() {
            section.push_str(&format!("  /{}\n", command.name));
        } else {
            section.push_str(&format!(
                "  /{:<16} {}\n",
                command.name, command.description
            ));
        }
    }
    match base.split_once("\nkeys:") {
        Some((head, tail)) => format!("{head}\n\n{section}\nkeys:{tail}"),
        None => format!("{base}\n\n{section}"),
    }
}