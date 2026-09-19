//! YAML frontmatter parsing for markdown resources.
//!
//! Rust port of `packages/coding-agent/src/utils/frontmatter.ts`. pi's
//! markdown resources (`SKILL.md`, prompt templates) declare their
//! metadata in a leading `---` fenced YAML block; this module extracts
//! that block and the remaining body.
//!
//! The TS side feeds the block to a full YAML parser. The Rust port only
//! needs the small subset the resource formats actually use — scalar
//! keys, booleans, and block scalars (`|` / `>` with chomping) — so the
//! parser below stays dependency-free instead of pulling in a YAML
//! crate. Unsupported-but-balanced constructs (flow sequences / mappings)
//! are kept as raw scalars rather than interpreted; every consumer so far
//! only reads string and boolean fields.
//!
//! ```
//! use pi_coding_agent::frontmatter::parse_frontmatter;
//!
//! let parsed = parse_frontmatter("---\nname: demo\ndisable-model-invocation: true\n---\nbody\n").unwrap();
//! assert_eq!(parsed.frontmatter.get_str("name"), Some("demo"));
//! assert_eq!(parsed.frontmatter.get_bool("disable-model-invocation"), Some(true));
//! assert_eq!(parsed.body, "body");
//! ```

use std::collections::BTreeMap;

/// Failure modes of [`parse_frontmatter`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrontmatterError {
    /// The fenced block is not valid in the supported YAML subset.
    InvalidYaml(String),
}

impl std::fmt::Display for FrontmatterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidYaml(message) => write!(f, "invalid YAML frontmatter: {message}"),
        }
    }
}

impl std::error::Error for FrontmatterError {}

/// One parsed frontmatter value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrontmatterValue {
    /// A plain or quoted scalar (`name: demo`, `name: "demo"`).
    String(String),
    /// A YAML boolean (`disable-model-invocation: true`).
    Bool(bool),
    /// An empty value or explicit null (`key:` / `key: null`).
    Null,
}

impl FrontmatterValue {
    /// The string payload, if this value is a string.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(value) => Some(value.as_str()),
            _ => None,
        }
    }

    /// The boolean payload, if this value is a boolean.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(value) => Some(*value),
            _ => None,
        }
    }
}

/// Parsed frontmatter keys, in file order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Frontmatter {
    entries: BTreeMap<String, FrontmatterValue>,
}

impl Frontmatter {
    /// Build an empty frontmatter block.
    pub fn new() -> Self {
        Self::default()
    }

    /// Look up a key.
    pub fn get(&self, key: &str) -> Option<&FrontmatterValue> {
        self.entries.get(key)
    }

    /// Look up a string-valued key.
    pub fn get_str(&self, key: &str) -> Option<&str> {
        self.get(key).and_then(FrontmatterValue::as_str)
    }

    /// Look up a boolean-valued key.
    pub fn get_bool(&self, key: &str) -> Option<bool> {
        self.get(key).and_then(FrontmatterValue::as_bool)
    }

    /// Whether the block declared no keys at all.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Number of declared keys.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Iterate over the declared keys in sorted order.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &FrontmatterValue)> {
        self.entries.iter()
    }

    fn insert(&mut self, key: String, value: FrontmatterValue) {
        // Last declaration wins, matching YAML's duplicate-key behaviour
        // for the subset we support.
        self.entries.insert(key, value);
    }
}

/// A parsed markdown resource: its frontmatter plus the body after the
/// fenced block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedFrontmatter {
    /// Metadata declared between the `---` fences.
    pub frontmatter: Frontmatter,
    /// The document body, trimmed, with the fence removed.
    pub body: String,
}

/// Strip a UTF-8 BOM and normalise newlines to `\n`.
fn normalize(content: &str) -> String {
    content.replace("\r\n", "\n").replace('\r', "\n")
}

/// Split `content` into (frontmatter fence contents, body).
///
/// Mirrors the TS `extractFrontmatter`: a document without a leading
/// `---` fence, or with an unterminated one, has no frontmatter.
fn extract_frontmatter(content: &str) -> (Option<String>, String) {
    let normalized = normalize(content);
    let normalized = normalized.strip_prefix('\u{feff}').unwrap_or(&normalized);

    if !normalized.starts_with("---") {
        return (None, normalized.to_string());
    }

    // The TS side searches for "\n---" from byte 3; that means the fence
    // line must be followed by a newline, and the closing fence is the
    // first `---` at the start of a later line.
    let Some(offset) = normalized[3.min(normalized.len())..].find("\n---") else {
        return (None, normalized.to_string());
    };
    let end = 3 + offset;

    let yaml = normalized[4.min(end)..end].to_string();
    let body = normalized[end + 4..].trim().to_string();
    (Some(yaml), body)
}

/// Parse markdown frontmatter.
///
/// Returns the parsed metadata and the body; a document without the
/// fenced block yields empty frontmatter and the whole (newline
/// normalised) document as the body.
pub fn parse_frontmatter(content: &str) -> Result<ParsedFrontmatter, FrontmatterError> {
    let (yaml, body) = extract_frontmatter(content);
    let Some(yaml) = yaml else {
        return Ok(ParsedFrontmatter {
            frontmatter: Frontmatter::new(),
            body,
        });
    };

    Ok(ParsedFrontmatter {
        frontmatter: parse_yaml_subset(&yaml)?,
        body,
    })
}

/// Parse `content` and return only the body (frontmatter removed).
///
/// Matches the TS `stripFrontmatter`: with a fence the body is trimmed,
/// without one the (newline-normalised) document is returned as-is.
pub fn strip_frontmatter(content: &str) -> String {
    match parse_frontmatter(content) {
        Ok(parsed) => parsed.body,
        Err(_) => normalize(content),
    }
}

/// Parse the supported YAML subset.
fn parse_yaml_subset(yaml: &str) -> Result<Frontmatter, FrontmatterError> {
    let lines: Vec<&str> = yaml.split('\n').collect();
    let mut frontmatter = Frontmatter::new();
    let mut index = 0usize;

    while index < lines.len() {
        let line = lines[index];
        let trimmed = line.trim();

        if trimmed.is_empty() || trimmed.starts_with('#') {
            index += 1;
            continue;
        }

        // Indented content outside a block scalar is nested YAML we do
        // not interpret (no resource format uses it).
        if line.starts_with(' ') || line.starts_with('\t') {
            index += 1;
            continue;
        }

        let Some(colon) = trimmed.find(':') else {
            return Err(FrontmatterError::InvalidYaml(format!(
                "expected `key: value`, found {trimmed:?}"
            )));
        };

        let key = trimmed[..colon].trim().to_string();
        let raw_value = trimmed[colon + 1..].trim();

        if key.is_empty() {
            return Err(FrontmatterError::InvalidYaml(
                "frontmatter key must not be empty".to_string(),
            ));
        }

        // Block scalar: gather every following line that is blank or
        // more indented than the key.
        if let Some(header) = block_scalar_header(raw_value) {
            let mut block: Vec<String> = Vec::new();
            let mut cursor = index + 1;
            while cursor < lines.len() {
                let candidate = lines[cursor];
                if candidate.trim().is_empty() {
                    block.push(String::new());
                    cursor += 1;
                    continue;
                }
                if candidate.starts_with(' ') || candidate.starts_with('\t') {
                    block.push(candidate.to_string());
                    cursor += 1;
                    continue;
                }
                break;
            }
            frontmatter.insert(key, FrontmatterValue::String(render_block_scalar(&block, header)));
            index = cursor;
            continue;
        }

        frontmatter.insert(key, parse_scalar(raw_value)?);
        index += 1;
    }

    Ok(frontmatter)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockStyle {
    Literal,
    Folded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockChomp {
    Clip,
    Strip,
    Keep,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BlockScalarHeader {
    style: BlockStyle,
    chomp: BlockChomp,
}

/// Recognise a block-scalar header (`|`, `>-`, …).
fn block_scalar_header(raw: &str) -> Option<BlockScalarHeader> {
    let (indicator, rest) = raw.split_at(1.min(raw.len()));
    let style = match indicator {
        "|" => BlockStyle::Literal,
        ">" => BlockStyle::Folded,
        _ => return None,
    };
    // Only chomping indicators are honoured; explicit indentation
    // indicators (`|2`) fall back to auto-detection.
    let chomp = match rest {
        "" => BlockChomp::Clip,
        "-" => BlockChomp::Strip,
        "+" => BlockChomp::Keep,
        _ => BlockChomp::Clip,
    };
    Some(BlockScalarHeader { style, chomp })
}

/// Render a block scalar body from its raw (still indented) lines.
fn render_block_scalar(lines: &[String], header: BlockScalarHeader) -> String {
    // Trailing blank lines are chomping, not content.
    let mut end = lines.len();
    while end > 0 && lines[end - 1].trim().is_empty() {
        end -= 1;
    }
    let content = &lines[..end];

    let indent = content
        .iter()
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.len() - line.trim_start().len())
        .min()
        .unwrap_or(0);

    let stripped: Vec<&str> = content
        .iter()
        .map(|line| if line.len() >= indent { &line[indent..] } else { line.trim_start() })
        .collect();

    let mut text = if header.style == BlockStyle::Folded {
        // Folded: line breaks become spaces, blank lines become newlines.
        let mut folded = String::new();
        let mut pending_blank = false;
        for (position, line) in stripped.iter().enumerate() {
            if line.trim().is_empty() {
                pending_blank = true;
                continue;
            }
            if position > 0 {
                folded.push_str(if pending_blank { "\n" } else { " " });
            }
            pending_blank = false;
            folded.push_str(line);
        }
        folded
    } else {
        stripped.join("\n")
    };

    match header.chomp {
        BlockChomp::Strip => {}
        BlockChomp::Clip => {
            if !text.is_empty() {
                text.push('\n');
            }
        }
        BlockChomp::Keep => {
            text.push('\n');
            for line in &lines[end..] {
                let _ = line;
                text.push('\n');
            }
        }
    }

    text
}

/// Parse a single-line scalar value.
fn parse_scalar(raw: &str) -> Result<FrontmatterValue, FrontmatterError> {
    if raw.is_empty() {
        return Ok(FrontmatterValue::Null);
    }

    if let Some(rest) = raw.strip_prefix('"') {
        let Some(body) = rest.strip_suffix('"') else {
            return Err(FrontmatterError::InvalidYaml(format!(
                "unterminated double-quoted scalar {raw:?}"
            )));
        };
        return Ok(FrontmatterValue::String(unescape_double_quoted(body)));
    }

    if let Some(rest) = raw.strip_prefix('\'') {
        let Some(body) = rest.strip_suffix('\'') else {
            return Err(FrontmatterError::InvalidYaml(format!(
                "unterminated single-quoted scalar {raw:?}"
            )));
        };
        return Ok(FrontmatterValue::String(body.replace("''", "'")));
    }

    // Flow collections are kept verbatim, but an unbalanced one is a
    // syntax error (upstream's YAML parser reports the same class of
    // failure for `description: [unclosed`).
    check_flow_balance(raw)?;

    let scalar = strip_inline_comment(raw).trim();
    match scalar {
        "~" | "null" | "Null" | "NULL" => Ok(FrontmatterValue::Null),
        "true" | "True" | "TRUE" => Ok(FrontmatterValue::Bool(true)),
        "false" | "False" | "FALSE" => Ok(FrontmatterValue::Bool(false)),
        _ => Ok(FrontmatterValue::String(scalar.to_string())),
    }
}

/// Reject unterminated `[` / `{` flow collections.
fn check_flow_balance(raw: &str) -> Result<(), FrontmatterError> {
    let mut stack: Vec<char> = Vec::new();
    for ch in raw.chars() {
        match ch {
            '[' | '{' => stack.push(ch),
            ']' if stack.pop() != Some('[') => {
                return Err(FrontmatterError::InvalidYaml(
                    "unexpected `]` in flow sequence".to_string(),
                ));
            }
            '}' if stack.pop() != Some('{') => {
                return Err(FrontmatterError::InvalidYaml(
                    "unexpected `}` in flow mapping".to_string(),
                ));
            }
            _ => {}
        }
    }
    if let Some(open) = stack.pop() {
        let label = if open == '[' { "flow sequence" } else { "flow mapping" };
        return Err(FrontmatterError::InvalidYaml(format!(
            "unterminated {label}"
        )));
    }
    Ok(())
}

/// Drop a trailing `# comment` from a plain scalar.
fn strip_inline_comment(raw: &str) -> &str {
    match raw.find(" #") {
        Some(position) => &raw[..position],
        None => raw,
    }
}

fn unescape_double_quoted(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut chars = body.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_scalars_and_booleans() {
        let parsed = parse_frontmatter(
            "---\nname: valid-skill\ndescription: A valid skill.\ndisable-model-invocation: true\n---\n# Body\n",
        )
        .expect("parses");

        assert_eq!(parsed.frontmatter.get_str("name"), Some("valid-skill"));
        assert_eq!(parsed.frontmatter.get_str("description"), Some("A valid skill."));
        assert_eq!(
            parsed.frontmatter.get_bool("disable-model-invocation"),
            Some(true)
        );
        assert_eq!(parsed.body, "# Body");
    }

    #[test]
    fn defaults_disable_model_invocation_to_absent() {
        let parsed = parse_frontmatter("---\nname: demo\n---\n").expect("parses");
        assert_eq!(parsed.frontmatter.get_bool("disable-model-invocation"), None);
    }

    #[test]
    fn ignores_unknown_fields_and_numbers() {
        let parsed = parse_frontmatter("---\nname: unknown-field\nauthor: someone\nversion: 1.0\n---\n")
            .expect("parses");
        assert_eq!(parsed.frontmatter.get_str("author"), Some("someone"));
        assert_eq!(parsed.frontmatter.get_str("version"), Some("1.0"));
        assert_eq!(parsed.frontmatter.len(), 3);
    }

    #[test]
    fn parses_literal_multiline_description() {
        let parsed = parse_frontmatter(
            "---\ndescription: |\n  This is a multiline description.\n  It spans multiple lines.\n---\n",
        )
        .expect("parses");

        let description = parsed.frontmatter.get_str("description").expect("description");
        assert!(description.contains('\n'), "expected a newline in {description:?}");
        assert!(description.contains("This is a multiline description."));
        assert!(description.ends_with('\n'));
    }

    #[test]
    fn folds_folded_block_scalars() {
        let parsed =
            parse_frontmatter("---\ndescription: >-\n  one\n  two\n\n  three\n---\n").expect("parses");
        assert_eq!(parsed.frontmatter.get_str("description"), Some("one two\nthree"));
    }

    #[test]
    fn handles_quoted_scalars() {
        let parsed =
            parse_frontmatter("---\na: \"say \\\"hi\\\"\"\nb: 'it''s fine'\nc: \" # not a comment\"\n---\n")
                .expect("parses");
        assert_eq!(parsed.frontmatter.get_str("a"), Some("say \"hi\""));
        assert_eq!(parsed.frontmatter.get_str("b"), Some("it's fine"));
        assert_eq!(parsed.frontmatter.get_str("c"), Some(" # not a comment"));
    }

    #[test]
    fn strips_trailing_comments_from_plain_scalars() {
        let parsed = parse_frontmatter("---\nname: demo # trailing\n---\n").expect("parses");
        assert_eq!(parsed.frontmatter.get_str("name"), Some("demo"));
    }

    #[test]
    fn rejects_unterminated_flow_collections() {
        let error = parse_frontmatter("---\ndescription: [unclosed bracket\n---\n").expect_err("invalid");
        assert!(matches!(error, FrontmatterError::InvalidYaml(_)));
        assert!(error.to_string().contains("unterminated"));
    }

    #[test]
    fn parses_an_empty_frontmatter_block() {
        let parsed = parse_frontmatter("---\n---\nbody\n").expect("parses");
        assert!(parsed.frontmatter.is_empty());
        assert_eq!(parsed.body, "body");
    }

    #[test]
    fn documents_without_frontmatter_keep_the_body() {
        let parsed = parse_frontmatter("# No frontmatter\n\nHello.\n").expect("parses");
        assert!(parsed.frontmatter.is_empty());
        // No fence: the body is the document, untrimmed (matching the TS
        // `extractFrontmatter`).
        assert_eq!(parsed.body, "# No frontmatter\n\nHello.\n");
    }

    #[test]
    fn strip_frontmatter_removes_the_fence() {
        assert_eq!(strip_frontmatter("---\nname: demo\n---\nbody\n"), "body");
        assert_eq!(strip_frontmatter("body only\n"), "body only\n");
    }

    #[test]
    fn normalises_crlf_and_bom() {
        let parsed = parse_frontmatter("\u{feff}---\r\nname: demo\r\n---\r\nbody\r\n").expect("parses");
        assert_eq!(parsed.frontmatter.get_str("name"), Some("demo"));
        assert_eq!(parsed.body, "body");
    }
}
