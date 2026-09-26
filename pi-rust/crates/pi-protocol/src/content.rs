//! Content blocks carried by [`Message`](crate::Message) entries.
//!
//! Mirrors `packages/ai/src/types.ts` (TextContent, ImageContent, ToolCall,
//! ToolResult). Kept intentionally small in Stage 0 — additional variants
//! land in Stage 1 alongside the provider ports.

use serde::{Deserialize, Serialize};

/// Plain text content block.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextContent {
    /// UTF-8 text.
    pub text: String,
}

/// Image content block. Provider-specific rendering handled in `pi-ai`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageContent {
    /// MIME type (e.g. `image/png`).
    pub mime_type: String,
    /// Base64-encoded image bytes.
    pub data: String,
}

/// A tool call produced by an assistant message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    /// Provider-issued identifier. Echoed back on the [`ToolResult`].
    pub id: String,
    /// Registered tool name.
    pub name: String,
    /// JSON-encoded arguments object.
    pub arguments: serde_json::Value,
}

/// The result of executing a [`ToolCall`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolResult {
    /// Echoes the originating [`ToolCall::id`].
    pub tool_call_id: String,
    /// Tool result content. Use a [`Content::Text`] block for human-readable
    /// output; structured data goes into [`ToolResult::details`].
    ///
    /// `Box<Content>` breaks the otherwise-infinite recursion between
    /// [`ToolResult`] and [`Content::ToolResult`]. Multi-image results
    /// surface their first image here (so a downstream renderer always
    /// sees at least one image) and stash the rest in
    /// [`ToolResult::images`]; the provider adapter reads both and
    /// emits one image block per entry in the wire payload.
    pub content: Box<Content>,
    /// True when the tool failed and the model should treat it as an error.
    #[serde(default)]
    pub is_error: bool,
    /// Optional structured details for tools that want to surface typed data
    /// to the host (e.g. diff metadata, exit codes, structured errors).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
    /// Names of tools this result introduced and made available from this
    /// transcript point onward.
    ///
    /// Mirrors upstream `ToolResultMessage.addedToolNames`
    /// (`packages/agent/src/types.ts`). Providers that support mid-transcript
    /// tool loading (Anthropic `tool_reference` blocks, OpenAI
    /// `additional_tools` items, Kimi's deferred declarations) read it off the
    /// tool-result message; the deferred-tool splitter in `pi-ai` uses it to
    /// keep such tools out of the request prefix.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub added_tool_names: Option<Vec<String>>,
    /// Every image attached to this tool result, in declaration order.
    ///
    /// `ToolResult.content` is a single block (one of the four
    /// [`Content`] variants), so the tool executor parks the first image
    /// there and accumulates the rest here. Providers iterate this list
    /// *in addition to* the single image in `content` when shaping the
    /// wire payload — the anthropic adapter emits one `image` block per
    /// entry, the openai adapters pass them through as
    /// `image_url` parts, and so on. Empty for non-image results so the
    /// default impl stays `Default`-derivable.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<ImageContent>,
}

// Manual `Default` impl — `Content` is a recursive enum (it contains
// `ToolResult(ToolResult)`) and can't derive `Default` automatically.
impl Default for ToolResult {
    fn default() -> Self {
        Self {
            tool_call_id: String::new(),
            content: Box::new(Content::Text(TextContent::default())),
            is_error: false,
            details: None,
            added_tool_names: None,
            images: Vec::new(),
        }
    }
}

/// A single content block — text, image, tool call, or tool result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Content {
    /// Plain text.
    Text(TextContent),
    /// Image attachment.
    Image(ImageContent),
    /// Tool call from the model.
    ToolCall(ToolCall),
    /// Tool result returned to the model.
    ToolResult(ToolResult),
}

impl Content {
    /// Convenience constructor for a text block.
    pub fn text<S: Into<String>>(s: S) -> Self {
        Self::Text(TextContent { text: s.into() })
    }

    /// True if this block is a [`Content::ToolCall`].
    pub fn is_tool_call(&self) -> bool {
        matches!(self, Self::ToolCall(_))
    }
}
