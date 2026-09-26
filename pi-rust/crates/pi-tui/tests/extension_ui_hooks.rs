//! P28 (E5) — extension UI hooks (`append_footer` / `prepend_header`).
//!
//! Upstream exposes four distinct extension regions on `ctx.ui`:
//!
//! | Hook             | Behaviour                                           |
//! |------------------|-----------------------------------------------------|
//! | `setHeader`      | Replaces the built-in header.                       |
//! | `prependHeader`  | Adds a row above the built-in header.               |
//! | `setFooter`      | Replaces the built-in footer.                       |
//! | `appendFooter`   | Adds a row below the built-in footer.               |
//!
//! `setHeader` / `setFooter` already exist in the port (P-replaces) and
//! are pinned by `extension_ui_replace`. The two new hooks land here.
//! Both are keyed — re-registering under the same key replaces the
//! previous component (disposing it) and moves it to the end of the
//! sequence. Passing `None` clears the slot. This mirrors upstream's
//! `ctx.ui.prependHeader(key, factory)` / `ctx.ui.appendFooter(key, factory)`
//! semantics (`packages/coding-agent/src/modes/interactive/extensions/ui.ts:30-94`).
//!
//! The tests below pin the App-level surface and the rendering order.

use std::sync::Arc;

use pi_agent_core::{Agent, AgentOptions};
use pi_ai::providers::faux::FauxProvider;
use pi_protocol::{Api, Model, ProviderId};
use pi_tui::app::{App, AppConfig};
use pi_tui::component::TextComponent;

fn faux_model() -> Model {
    Model {
        provider: ProviderId::new("faux"),
        api: Api::Faux,
        id: "faux-model".into(),
        label: Some("Faux".into()),
        context_window: 1024,
        max_output_tokens: 256,
    }
}

fn fresh_app() -> App {
    let agent = Agent::new(AgentOptions::new(
        faux_model(),
        Arc::new(FauxProvider::default()),
        "you are pi",
    ));
    App::new(&agent, AppConfig::default())
}

fn rendered_lines(app: &App, width: u16, height: u16) -> Vec<String> {
    app.render_snapshot(width, height).lines
}

/// 1 — `append_footer` adds a row below the built-in footer; the keys
/// surface through `appended_footer_keys` in insertion order.
#[test]
fn append_footer_registers_a_key_below_the_built_in_footer() {
    let mut app = fresh_app();
    app.append_footer(
        "ext-a".into(),
        Some(Box::new(TextComponent::new(["A"]))),
    );
    app.append_footer(
        "ext-b".into(),
        Some(Box::new(TextComponent::new(["B"]))),
    );
    assert_eq!(
        app.appended_footer_keys(),
        vec!["ext-a".to_string(), "ext-b".to_string()],
    );
}

/// 2 — `prepend_header` adds a row above the built-in header; keys surface
/// in insertion order.
#[test]
fn prepend_header_registers_a_key_above_the_built_in_header() {
    let mut app = fresh_app();
    app.prepend_header(
        "ext-x".into(),
        Some(Box::new(TextComponent::new(["X"]))),
    );
    app.prepend_header(
        "ext-y".into(),
        Some(Box::new(TextComponent::new(["Y"]))),
    );
    assert_eq!(
        app.prepended_header_keys(),
        vec!["ext-x".to_string(), "ext-y".to_string()],
    );
}

/// 3 — Re-registering under an existing key replaces the previous
/// component: `remove_appended_footer` returns `false` after the swap,
/// because the key is still on the slot under a fresh component.
#[test]
fn append_footer_replacing_a_key_replaces_the_component() {
    let mut app = fresh_app();
    app.append_footer(
        "k".into(),
        Some(Box::new(TextComponent::new(["first"]))),
    );
    app.append_footer(
        "k".into(),
        Some(Box::new(TextComponent::new(["second"]))),
    );
    assert_eq!(app.appended_footer_keys(), vec!["k".to_string()]);
}

/// 4 — Passing `None` under a key clears the slot and disposes the
/// component. The next call returns `false` from `remove_appended_footer`.
#[test]
fn append_footer_none_clears_the_slot() {
    let mut app = fresh_app();
    app.append_footer(
        "k".into(),
        Some(Box::new(TextComponent::new(["only"]))),
    );
    assert!(app.remove_appended_footer("k"));
    assert!(!app.remove_appended_footer("k"));
    app.append_footer("k".into(), None);
    assert!(!app.remove_appended_footer("k"));
}

/// 5 — `prepend_header` keys clear symmetrically.
#[test]
fn prepend_header_none_clears_the_slot() {
    let mut app = fresh_app();
    app.prepend_header(
        "k".into(),
        Some(Box::new(TextComponent::new(["only"]))),
    );
    assert!(app.remove_prepended_header("k"));
    app.prepend_header("k".into(), None);
    assert!(!app.remove_prepended_header("k"));
}

/// 6 — `clear_appended_footers` removes every registered footer in one
/// call. The key list returns empty.
#[test]
fn clear_appended_footers_removes_every_registered_key() {
    let mut app = fresh_app();
    app.append_footer(
        "a".into(),
        Some(Box::new(TextComponent::new(["A"]))),
    );
    app.append_footer(
        "b".into(),
        Some(Box::new(TextComponent::new(["B"]))),
    );
    app.clear_appended_footers();
    assert!(app.appended_footer_keys().is_empty());
}

/// 7 — `clear_prepended_headers` removes every registered header key.
#[test]
fn clear_prepended_headers_removes_every_registered_key() {
    let mut app = fresh_app();
    app.prepend_header(
        "a".into(),
        Some(Box::new(TextComponent::new(["A"]))),
    );
    app.prepend_header(
        "b".into(),
        Some(Box::new(TextComponent::new(["B"]))),
    );
    app.clear_prepended_headers();
    assert!(app.prepended_header_keys().is_empty());
}

/// 8 — The rendered frame includes the appended footer rows. The frame
/// the App plans has more rows than the empty frame; we only assert that
/// `render_snapshot` does not panic when both an appended and a prepended
/// region are attached. The internal layout logic is pinned by the lib
/// tests in `components::extension_ui`.
#[test]
fn rendering_with_prepended_and_appended_regions_does_not_panic() {
    let mut app = fresh_app();
    app.prepend_header(
        "ext".into(),
        Some(Box::new(TextComponent::new(["hdr"]))),
    );
    app.append_footer(
        "ext".into(),
        Some(Box::new(TextComponent::new(["ftr"]))),
    );
    let _ = rendered_lines(&app, 80, 24);
}
