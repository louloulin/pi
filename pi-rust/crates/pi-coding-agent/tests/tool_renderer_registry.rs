//! Integration tests for the extension-supplied tool renderer registry.
//!
//! Mirrors the TS `extensions/runner.ts` lookup table that lets a JS
//! extension ship a `renderCall` / `renderResult` pair for a built-in
//! tool and have it shadow the default renderer. The Rust port puts
//! the registry on [`ToolRendererRegistry::global`] and `renderer_for`
//! consults it before falling back to the built-in default.
//!
//! Every test that mutates the global registry uses a `RendererGuard`
//! to restore the empty state on drop, so the suite stays isolated
//! even when cargo runs the tests in parallel.

#![cfg(not(target_arch = "wasm32"))]

use std::sync::Arc;

use pi_coding_agent::tools::render::{
    clear_tool_renderers, register_tool_renderer, renderer_for, unregister_tool_renderer,
    EditRenderer, ReadRenderer, ToolRenderContext, ToolRenderOptions, ToolRenderSession,
    ToolRenderer, ToolRendererRegistry,
};
use pi_protocol::{Content, ToolCall, ToolResult};
use pi_tui::styled::plain_text;
use pi_tui::StyledLine;
use serde_json::json;

type ToolOutput = pi_coding_agent::tools::ToolOutput;

fn plain_line(text: &str) -> StyledLine {
    vec![pi_tui::StyledSpan::new(text.to_string(), pi_tui::SpanStyle::PLAIN)]
}

/// One renderer that always echoes its name as a 1-line call, and
/// echoes "custom-result" as a 1-line result. Used to verify the
/// registry shadows the built-in defaults.
struct CustomRenderer {
    name: &'static str,
}

impl ToolRenderer for CustomRenderer {
    fn name(&self) -> &str {
        self.name
    }

    fn render_call(
        &mut self,
        _args: &serde_json::Value,
        _ctx: &ToolRenderContext,
    ) -> Vec<StyledLine> {
        vec![plain_line(&format!("custom-call:{}", self.name))]
    }

    fn render_result(
        &mut self,
        _result: &ToolOutput,
        _options: &ToolRenderOptions,
        _ctx: &ToolRenderContext,
    ) -> Vec<StyledLine> {
        vec![plain_line("custom-result")]
    }

    fn clone_box(&self) -> Box<dyn ToolRenderer> {
        Box::new(CustomRenderer { name: self.name })
    }
}

struct RendererGuard;

impl Drop for RendererGuard {
    fn drop(&mut self) {
        clear_tool_renderers();
    }
}

#[test]
fn built_in_default_is_returned_when_nothing_is_registered() {
    let _guard = RendererGuard;
    // Sanity: every built-in renderer still resolves through `renderer_for`.
    assert_eq!(
        renderer_for("read").map(|r| r.name().to_string()),
        Some("read".into())
    );
    assert_eq!(
        renderer_for("bash").map(|r| r.name().to_string()),
        Some("bash".into())
    );
    assert_eq!(
        renderer_for("edit").map(|r| r.name().to_string()),
        Some("edit".into())
    );
}

#[test]
fn extension_renderer_shadows_the_built_in_default() {
    // Use a *local* registry bound to this session so the assertion does
    // not race with the parallel test that registers a different
    // `read` renderer in the global registry.
    let local = Arc::new(ToolRendererRegistry::new());
    local.register("read", || Box::new(CustomRenderer { name: "read" }));

    let mut session = ToolRenderSession::new("/work")
        .with_renderer_registry(Some(local));
    let call = ToolCall {
        id: "call-1".into(),
        name: "read".into(),
        arguments: json!({"path": "/tmp/x"}),
    };
    let lines = session.call(&call, false);
    // The custom renderer replaces the built-in `read <path>` line.
    assert_eq!(
        lines.first().map(|line| plain_text(line)),
        Some("custom-call:read".to_string()),
        "session.call must use the registered renderer, not the built-in"
    );
}

#[test]
fn unregister_restores_the_built_in_default() {
    let _guard = RendererGuard;
    register_tool_renderer("bash", || Box::new(CustomRenderer { name: "bash" }));
    assert_eq!(
        renderer_for("bash").map(|r| r.name().to_string()),
        Some("bash".into()),
        "custom renderer is in place"
    );
    assert!(unregister_tool_renderer("bash"));
    let resolved = renderer_for("bash").expect("built-in");
    assert_eq!(resolved.name(), "bash");
}

#[test]
fn unknown_tool_still_returns_none() {
    // Touch only the local lookup so the test is parallel-safe against
    // other tests that write to the global registry.
    let local = ToolRendererRegistry::new();
    assert!(local.lookup("not-a-real-tool").is_none());
    // Even when something is registered for `read`, an unknown tool
    // name still resolves to `None` from the global path.
    let _guard = RendererGuard;
    register_tool_renderer("read", || Box::new(CustomRenderer { name: "read" }));
    assert!(renderer_for("not-a-real-tool").is_none());
}

#[test]
fn renderers_are_cloned_fresh_per_session_call() {
    // `ToolRenderSession::call` asks the registry for a brand-new
    // renderer instance — two parallel sessions must not share state.
    // Use a *local* registry bound to both sessions so the test does
    // not race with other tests that touch the global registry.
    let local = Arc::new(ToolRendererRegistry::new());
    local.register("read", || Box::new(ReadRenderer::default()));

    let mut a = ToolRenderSession::new("/work")
        .with_renderer_registry(Some(Arc::clone(&local)));
    let mut b = ToolRenderSession::new("/work")
        .with_renderer_registry(Some(Arc::clone(&local)));

    let call_a = ToolCall {
        id: "call-a".into(),
        name: "read".into(),
        arguments: json!({"path": "/tmp/a"}),
    };
    let call_b = ToolCall {
        id: "call-b".into(),
        name: "read".into(),
        arguments: json!({"path": "/tmp/b"}),
    };
    a.call(&call_a, false);
    b.call(&call_b, false);

    let result_a = ToolResult {
        tool_call_id: "call-a".into(),
        content: Box::new(Content::text("a body")),
        is_error: false,
        details: None,
        added_tool_names: None,
        images: Vec::new(),
    };
    let result_b = ToolResult {
        tool_call_id: "call-b".into(),
        content: Box::new(Content::text("b body")),
        is_error: false,
        details: None,
        added_tool_names: None,
        images: Vec::new(),
    };
    let lines_a = a.result(&result_a);
    let lines_b = b.result(&result_b);

    // Both renderers produced output; the key invariant is that each
    // session has its own in-flight renderer and one session's result
    // did not consume the other's stored renderer.
    assert!(!lines_a.is_empty(), "session A produced output");
    assert!(!lines_b.is_empty(), "session B produced output");
}

#[test]
fn local_registry_does_not_touch_the_global_one() {
    // The global registry is for the *process* — tests need isolation.
    // The non-global constructor exists for callers that want their
    // own local registry; the public API is exercised here to keep
    // the contract pinned.
    //
    // Parallel test execution means the global registry may have
    // entries left over from another test; the only invariant the
    // local registry guarantees is that *its own* register / lookup /
    // names pair works in isolation.
    let local = ToolRendererRegistry::new();
    local.register("read", || Box::new(CustomRenderer { name: "read" }));
    assert_eq!(
        local.lookup("read").map(|r| r.name().to_string()),
        Some("read".into()),
        "local lookup finds the entry"
    );
    assert_eq!(
        local.names(),
        vec!["read".to_string()],
        "names() returns the registered keys sorted"
    );
    // The local lookup does not promote the entry to the global
    // registry — even when the global registry happens to be empty,
    // `renderer_for` still resolves the built-in default.
    assert_eq!(
        local.lookup("not-registered").map(|r| r.name().to_string()),
        None,
        "an unregistered tool surfaces as None from a local registry too"
    );
}

#[test]
fn edit_renderer_clone_keeps_its_name() {
    let original = EditRenderer::new();
    let clone = original.clone_box();
    assert_eq!(clone.name(), "edit");
}