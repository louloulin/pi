//! Pre-rendering of the tools the HTML template does not render itself.
//!
//! Port of `preRenderCustomTools` (`export-html/index.ts`) plus
//! `createToolHtmlRenderer` (`export-html/tool-renderer.ts`).
//!
//! `template.js` has a hand-written renderer for a fixed set of tools —
//! [`TEMPLATE_RENDERED_TOOLS`] — and falls back to a JSON dump for every
//! other tool call unless the payload carries pre-rendered HTML for it. This
//! module produces that payload entry: for each tool call outside the
//! template set it renders the call and its result through the Rust
//! [`ToolRenderer`](crate::tools::ToolRenderer) (the same presentation the
//! text fallback uses), paints the
//! [`StyledLine`](pi_tui::StyledLine)s with
//! [`render_lines_ansi`](crate::tools::render_lines_ansi), and converts the
//! ANSI to inline-styled HTML with [`ansi_lines_to_html`](super::ansi_to_html::ansi_lines_to_html).
//!
//! Traversal order and the two guards are upstream's:
//!
//! * a `toolCall` is skipped when its name is in [`TEMPLATE_RENDERED_TOOLS`];
//! * a `toolResult` is rendered when its call already produced HTML **or**
//!   when its name is outside the template set — the first clause is what
//!   still renders a result whose call was hidden by the blacklist.
//!
//! A tool without a Rust renderer keeps upstream's "no custom renderer"
//! semantics: `render_call` yields nothing, so no entry is produced and
//! `template.js` falls back to the JSON view. JS extension tools
//! (`getToolDefinition(name).renderCall`) are in that class today; see the
//! divergence note in `docs/SESSION_EXPORT.md`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Tools rendered directly by `template.js`, which therefore must not be
/// pre-rendered (upstream `TEMPLATE_RENDERED_TOOLS`).
pub const TEMPLATE_RENDERED_TOOLS: [&str; 5] = ["bash", "read", "write", "edit", "ls"];

/// True when `template.js` renders the tool itself.
pub fn is_template_rendered(name: &str) -> bool {
    TEMPLATE_RENDERED_TOOLS.contains(&name)
}

/// Pre-rendered HTML for one tool call and its result
/// (upstream `RenderedToolHtml`).
///
/// Every field is optional: a call with no renderer produces no entry at
/// all, and a result whose collapsed and expanded renderings are identical
/// omits `resultHtmlCollapsed`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderedToolHtml {
    /// Rendered tool-call header (`template.js` `renderedTools[id].callHtml`).
    #[serde(rename = "callHtml", default, skip_serializing_if = "Option::is_none")]
    pub call_html: Option<String>,
    /// Collapsed result body, present only when it differs from the expanded
    /// one (`resultHtmlCollapsed`).
    #[serde(
        rename = "resultHtmlCollapsed",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub result_html_collapsed: Option<String>,
    /// Expanded result body (`resultHtmlExpanded`).
    #[serde(
        rename = "resultHtmlExpanded",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub result_html_expanded: Option<String>,
}

/// Pre-render every non-template tool in `entries`, keyed by tool-call id.
///
/// Returns `None` when nothing was rendered, which is upstream's
/// "only include if we actually rendered something" rule: the payload then
/// omits `renderedTools` instead of carrying an empty object.
///
/// `cwd` is the session working directory recorded in the header; paths in
/// the rendered HTML are shown relative to it. `theme_name` selects the
/// palette the ANSI is painted with (and must match the one passed to
/// [`generate_html`](super::generate_html) so the pre-rendered colours agree
/// with the rest of the document).
#[cfg(not(target_arch = "wasm32"))]
pub fn pre_render_custom_tools(
    entries: &[Value],
    cwd: &str,
    theme_name: Option<&str>,
) -> Option<BTreeMap<String, RenderedToolHtml>> {
    use std::collections::HashMap;

    use crate::tools::ToolRenderer;

    let Some(theme) = super::theme::resolved_theme(theme_name) else {
        // An unresolvable theme leaves the ANSI unstylable; the JSON fallback
        // is a better answer than a half-painted document.
        return None;
    };

    let mut rendered: BTreeMap<String, RenderedToolHtml> = BTreeMap::new();
    // Upstream keeps one component per tool-call id so `renderResult` sees
    // the state `renderCall` cached. A Rust renderer bundles that state, so
    // the instance is kept here and reused when the result carries the same
    // tool name.
    let mut renderers: HashMap<String, Box<dyn ToolRenderer>> = HashMap::new();

    for entry in entries {
        if entry.get("type").and_then(Value::as_str) != Some("message") {
            continue;
        }
        let Some(message) = entry.get("message") else {
            continue;
        };

        match message.get("role").and_then(Value::as_str) {
            Some("assistant") => {
                let Some(content) = message.get("content").and_then(Value::as_array) else {
                    continue;
                };
                for block in content {
                    if block.get("type").and_then(Value::as_str) != Some("toolCall") {
                        continue;
                    }
                    let Some(name) = block.get("name").and_then(Value::as_str) else {
                        continue;
                    };
                    if is_template_rendered(name) {
                        continue;
                    }
                    let Some(id) = block.get("id").and_then(Value::as_str) else {
                        continue;
                    };
                    let Some(mut renderer) = crate::tools::renderer_for(name) else {
                        continue;
                    };

                    let arguments = block.get("arguments").cloned().unwrap_or(Value::Null);
                    let ctx = crate::tools::ToolRenderContext::new(cwd);
                    let lines = renderer.render_call(&arguments, &ctx);
                    let call_html =
                        super::ansi_to_html::ansi_lines_to_html(&ansi_lines(&lines, &theme));
                    if !call_html.is_empty() {
                        rendered.insert(
                            id.to_string(),
                            RenderedToolHtml {
                                call_html: Some(call_html),
                                ..RenderedToolHtml::default()
                            },
                        );
                    }
                    renderers.insert(id.to_string(), renderer);
                }
            }
            Some("toolResult") => {
                let Some(call_id) = message.get("toolCallId").and_then(Value::as_str) else {
                    continue;
                };
                let tool_name = message
                    .get("toolName")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let existing = rendered.contains_key(call_id);
                // Upstream: `existing || !TEMPLATE_RENDERED_TOOLS.has(toolName)`.
                if !existing && is_template_rendered(tool_name) {
                    continue;
                }
                let Some(result) =
                    render_tool_result(message, call_id, tool_name, cwd, &theme, &mut renderers)
                else {
                    continue;
                };
                let entry = rendered.entry(call_id.to_string()).or_default();
                if let Some(collapsed) = result.collapsed {
                    entry.result_html_collapsed = Some(collapsed);
                }
                entry.result_html_expanded = Some(result.expanded);
            }
            _ => {}
        }
    }

    (!rendered.is_empty()).then_some(rendered)
}

/// wasm builds have no tool renderers (`tools::render` is native-only), so
/// the payload keeps upstream's empty-`renderedTools` shape.
#[cfg(target_arch = "wasm32")]
pub fn pre_render_custom_tools(
    _entries: &[Value],
    _cwd: &str,
    _theme_name: Option<&str>,
) -> Option<BTreeMap<String, RenderedToolHtml>> {
    None
}

/// Paint styled lines with ANSI, one string per rendered line.
#[cfg(not(target_arch = "wasm32"))]
fn ansi_lines(lines: &[pi_tui::StyledLine], theme: &pi_tui::Theme) -> Vec<String> {
    crate::tools::render_lines_ansi(lines, theme)
        .split('\n')
        .map(str::to_string)
        .collect()
}

/// Drop leading/trailing visually blank lines
/// (upstream `trimRenderedResultLines`).
#[cfg(not(target_arch = "wasm32"))]
fn trim_blank_lines(lines: Vec<String>) -> Vec<String> {
    let is_blank = |line: &String| super::ansi_to_html::strip_ansi_sgr(line).trim().is_empty();
    let start = lines
        .iter()
        .position(|line| !is_blank(line))
        .unwrap_or(lines.len());
    let end = lines
        .iter()
        .rposition(|line| !is_blank(line))
        .map(|index| index + 1)
        .unwrap_or(start);
    lines[start..end].to_vec()
}

/// `ansi_lines_to_html` with upstream's blank-line trimming applied.
#[cfg(not(target_arch = "wasm32"))]
fn result_html(lines: &[pi_tui::StyledLine], theme: &pi_tui::Theme) -> String {
    super::ansi_to_html::ansi_lines_to_html(&trim_blank_lines(ansi_lines(lines, theme)))
}

/// Content blocks of a `toolResult` entry as a [`ToolOutput`](crate::tools::ToolOutput).
#[cfg(not(target_arch = "wasm32"))]
fn tool_output_from_message(message: &Value) -> crate::tools::ToolOutput {
    use pi_protocol::{Content, ImageContent};

    let content: Vec<Content> =
        if let Some(blocks) = message.get("content").and_then(Value::as_array) {
            blocks
                .iter()
                .filter_map(|block| match block.get("type").and_then(Value::as_str) {
                    Some("text") => Some(Content::text(
                        block.get("text").and_then(Value::as_str).unwrap_or(""),
                    )),
                    Some("image") => Some(Content::Image(ImageContent {
                        mime_type: block
                            .get("mimeType")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        data: block
                            .get("data")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                    })),
                    _ => None,
                })
                .collect()
        } else {
            // Upstream `AgentMessage` content is a block array; a bare string
            // is accepted defensively so hand-written session files still
            // render.
            message
                .get("content")
                .and_then(Value::as_str)
                .map(|text| vec![Content::text(text)])
                .unwrap_or_default()
        };

    crate::tools::ToolOutput {
        content,
        details: match message.get("details") {
            None | Some(Value::Null) => None,
            Some(value) => Some(value.clone()),
        },
    }
}

/// The two renderings upstream's `renderResult` returns.
#[cfg(not(target_arch = "wasm32"))]
struct RenderedResult {
    collapsed: Option<String>,
    expanded: String,
}

/// Render one result through the call's renderer (or a fresh one for the
/// result's own tool name), mirroring upstream's collapsed/expanded pair.
///
/// Upstream looks the tool definition up by the result's `toolName` and only
/// passes the call's component along as `lastComponent`; a Rust renderer
/// bundles that state, so the stored instance is reused only when the names
/// agree and otherwise a fresh renderer is built — same outcome for the
/// matching case, and the result's own renderer for a mismatched one.
#[cfg(not(target_arch = "wasm32"))]
fn render_tool_result(
    message: &Value,
    call_id: &str,
    tool_name: &str,
    cwd: &str,
    theme: &pi_tui::Theme,
    renderers: &mut std::collections::HashMap<String, Box<dyn crate::tools::ToolRenderer>>,
) -> Option<RenderedResult> {
    let stored = renderers.remove(call_id);
    let mut renderer = match stored {
        Some(renderer) if renderer.name() == tool_name => renderer,
        _ => crate::tools::renderer_for(tool_name)?,
    };
    let output = tool_output_from_message(message);
    let is_error = message
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let base_ctx = crate::tools::ToolRenderContext::new(cwd).with_is_error(is_error);

    // Upstream renders collapsed then expanded on the same component, so the
    // same renderer instance sees both passes.
    let collapsed_lines = renderer.render_result(
        &output,
        &crate::tools::ToolRenderOptions::collapsed(),
        &base_ctx.clone().with_expanded(false),
    );
    let collapsed = result_html(&collapsed_lines, theme);
    let expanded_lines = renderer.render_result(
        &output,
        &crate::tools::ToolRenderOptions::expanded(),
        &base_ctx.with_expanded(true),
    );
    let expanded = result_html(&expanded_lines, theme);

    Some(RenderedResult {
        collapsed: (!collapsed.is_empty() && collapsed != expanded).then_some(collapsed),
        expanded,
    })
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use serde_json::json;

    use super::*;

    fn assistant_call(id: &str, name: &str, arguments: Value) -> Value {
        json!({
            "type": "message",
            "id": format!("e-{id}"),
            "message": {
                "role": "assistant",
                "content": [{"type": "toolCall", "id": id, "name": name, "arguments": arguments}],
            },
        })
    }

    fn tool_result(id: &str, name: &str, text: &str) -> Value {
        json!({
            "type": "message",
            "id": format!("r-{id}"),
            "message": {
                "role": "toolResult",
                "toolCallId": id,
                "toolName": name,
                "content": [{"type": "text", "text": text}],
                "isError": false,
            },
        })
    }

    #[test]
    fn template_rendered_tools_cover_upstreams_set() {
        for name in ["bash", "read", "write", "edit", "ls"] {
            assert!(is_template_rendered(name), "{name}");
        }
        for name in ["find", "grep", "custom_tool", ""] {
            assert!(!is_template_rendered(name), "{name}");
        }
    }

    #[test]
    fn blacklisted_calls_and_results_are_not_pre_rendered() {
        let entries = vec![
            assistant_call("c1", "bash", json!({"command": "ls"})),
            tool_result("c1", "bash", "a.txt"),
        ];
        assert_eq!(
            pre_render_custom_tools(&entries, "/tmp", Some("dark")),
            None
        );
    }

    #[test]
    fn find_and_grep_calls_and_results_get_html() {
        let entries = vec![
            assistant_call("c1", "find", json!({"pattern": "*.rs", "path": "."})),
            tool_result("c1", "find", "src/main.rs\nsrc/lib.rs"),
            assistant_call("c2", "grep", json!({"pattern": "fn main", "path": "src"})),
            tool_result("c2", "grep", "src/main.rs:1:fn main() {}"),
        ];
        let rendered = pre_render_custom_tools(&entries, "/tmp", Some("dark")).expect("rendered");

        assert_eq!(rendered.len(), 2);
        let find = &rendered["c1"];
        assert!(
            find.call_html.as_deref().unwrap().contains("<span style="),
            "{find:?}"
        );
        assert!(
            find.call_html.as_deref().unwrap().contains("find"),
            "{find:?}"
        );
        assert!(
            find.result_html_expanded
                .as_deref()
                .unwrap()
                .contains("src/main.rs"),
            "{find:?}"
        );
        let grep = &rendered["c2"];
        assert!(grep.call_html.as_deref().unwrap().contains("/fn main/"));
        assert!(grep
            .result_html_expanded
            .as_deref()
            .unwrap()
            .contains("fn main"));
    }

    #[test]
    fn tools_without_a_rust_renderer_are_omitted() {
        // Upstream's JS-extension tools render through `getToolDefinition`;
        // the Rust side has no renderer for this name, so the payload omits
        // it and `template.js` falls back to JSON.
        let entries = vec![
            assistant_call("c1", "my_extension_tool", json!({"x": 1})),
            tool_result("c1", "my_extension_tool", "done"),
        ];
        assert_eq!(
            pre_render_custom_tools(&entries, "/tmp", Some("dark")),
            None
        );
    }

    #[test]
    fn a_result_is_rendered_when_its_call_already_has_html() {
        // The result carries a template-rendered name while a pre-rendered
        // entry exists for its id: upstream's first clause wins and the
        // result is still rendered.
        let entries = vec![
            assistant_call("c1", "find", json!({"pattern": "a", "path": "."})),
            tool_result("c1", "bash", "a.txt"),
        ];
        let rendered = pre_render_custom_tools(&entries, "/tmp", Some("dark")).expect("rendered");
        let tool = &rendered["c1"];
        assert!(tool.call_html.is_some());
        assert!(
            tool.result_html_expanded
                .as_deref()
                .unwrap()
                .contains("a.txt"),
            "{tool:?}"
        );
    }

    #[test]
    fn non_message_entries_are_skipped() {
        let entries = vec![
            json!({"type": "label", "id": "l1"}),
            json!({"type": "message", "id": "m1"}),
        ];
        assert_eq!(
            pre_render_custom_tools(&entries, "/tmp", Some("dark")),
            None
        );
    }
}
