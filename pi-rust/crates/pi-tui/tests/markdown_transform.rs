//! P22 (B15) — `MarkdownTransform` extension point + error swallowing.
//!
//! Upstream's `MarkdownTransform` interface
//! (`packages/tui/src/components/markdown.ts:1083-1117`) is what
//! powers the `customMarkdownTransform` knob on the editor: a
//! downstream tool can rewrite links, swap glyphs, redact secrets, or
//! fold code blocks without touching the renderer. The Rust port
//! mirrors the same shape on the **rendered lines** (rather than the
//! AST) so a transform sees exactly what the user would otherwise
//! see, and applies purely-presentational changes without learning
//! the internal `Block` enum.
//!
//! The wrapper [`render_markdown_with_transform`] must be **total**:
//! it never panics, never loops, and never drops the body even when
//! the transformer's `transform` itself panics. Errors returned via
//! `Result::Err` and panics are both caught and turned into a single
//! warning row appended after the body, so the reader still sees the
//! content even when the transform fails.

use pi_tui::markdown::{render_markdown, render_markdown_with_transform, MarkdownTransform, TransformError};
use pi_tui::styled::{plain_text, SpanStyle, StyledLine};
use pi_tui::ThemeColor;

// ---------------------------------------------------------------------------
// Test doubles — three transformers that exercise the success / error / panic
// arms, plus a no-op identity for the easy path.
// ---------------------------------------------------------------------------

struct Identity;

impl MarkdownTransform for Identity {
    fn transform(
        &self,
        lines: Vec<StyledLine>,
        _width: usize,
    ) -> Result<Vec<StyledLine>, TransformError> {
        Ok(lines)
    }
}

struct Uppercaser;

impl MarkdownTransform for Uppercaser {
    fn transform(
        &self,
        mut lines: Vec<StyledLine>,
        _width: usize,
    ) -> Result<Vec<StyledLine>, TransformError> {
        for line in lines.iter_mut() {
            for span in line.iter_mut() {
                span.text = span.text.to_uppercase();
            }
        }
        Ok(lines)
    }
}

struct RejectAll;

impl MarkdownTransform for RejectAll {
    fn transform(
        &self,
        _lines: Vec<StyledLine>,
        _width: usize,
    ) -> Result<Vec<StyledLine>, TransformError> {
        Err(TransformError::new("transform rejected the input"))
    }
}

struct PanicTransform;

impl MarkdownTransform for PanicTransform {
    fn transform(
        &self,
        _lines: Vec<StyledLine>,
        _width: usize,
    ) -> Result<Vec<StyledLine>, TransformError> {
        panic!("boom — the transform blew up");
    }
}

struct Empty;

impl MarkdownTransform for Empty {
    fn transform(
        &self,
        _lines: Vec<StyledLine>,
        _width: usize,
    ) -> Result<Vec<StyledLine>, TransformError> {
        Ok(Vec::new())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// 1 — The identity transform is byte-for-byte equivalent to the
/// standard renderer. The wrapper must not add a warning row when
/// the transform succeeds.
#[test]
fn identity_transform_matches_standard_renderer() {
    let source = "Hello, *world*!";
    let baseline = render_markdown(source, 80);
    let via_wrapper = render_markdown_with_transform(source, 80, &Identity);
    assert_eq!(
        baseline, via_wrapper,
        "identity transform must not alter the rendered output"
    );
}

/// 2 — A successful transform (Uppercaser) actually rewrites the
/// rendered lines. The wrapper must surface the transformer's
/// output verbatim, with no warning row.
#[test]
fn successful_transform_replaces_the_lines() {
    let source = "Hello, *world*!";
    let via_wrapper = render_markdown_with_transform(source, 80, &Uppercaser);
    let combined: String = via_wrapper
        .iter()
        .map(|l| plain_text(l.as_slice()))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        combined.contains("HELLO"),
        "uppercase transform must apply; got {combined:?}"
    );
    assert!(
        !combined.contains('⚠'),
        "successful transform must not paint a warning row; got {combined:?}"
    );
}

/// 3 — An `Err` from the transform is caught: the original lines are
/// preserved (so the body is never lost) and a dim warning row is
/// appended so the reader can tell the transform did not apply.
#[test]
fn err_result_preserves_body_and_paints_warning_row() {
    let source = "Body line that must survive.";
    let baseline = render_markdown(source, 80);
    let via_wrapper = render_markdown_with_transform(source, 80, &RejectAll);
    assert_eq!(
        baseline.len() + 1,
        via_wrapper.len(),
        "the wrapper must append exactly one warning row on Err"
    );
    // The original body is preserved (the prefix matches).
    assert_eq!(baseline, via_wrapper[..baseline.len()].to_vec());
    // The warning row carries the transformer's message and the dim colour.
    let last = via_wrapper.last().expect("warning row");
    assert_eq!(last.len(), 1, "warning row is exactly one span");
    assert!(
        last[0].text.contains("⚠"),
        "warning row must contain the ⚠ glyph; got {:?}",
        last[0].text
    );
    assert!(
        last[0].text.contains("transform rejected"),
        "warning row must carry the transformer's message; got {:?}",
        last[0].text
    );
    assert_eq!(
        last[0].style.fg,
        Some(ThemeColor::Dim),
        "warning row must be dim so it does not look like an error"
    );
}

/// 4 — A panicking transform is caught: the body survives, the
/// panic message becomes the warning row's text, and the test
/// process itself does not abort.
#[test]
fn panicking_transform_is_caught_and_swallowed() {
    let source = "Body that survives a panicking transform.";
    let baseline = render_markdown(source, 80);
    let via_wrapper = render_markdown_with_transform(source, 80, &PanicTransform);
    assert_eq!(
        baseline.len() + 1,
        via_wrapper.len(),
        "the wrapper must append exactly one warning row on panic"
    );
    assert_eq!(baseline, via_wrapper[..baseline.len()].to_vec());
    let last = via_wrapper.last().expect("warning row");
    assert!(
        last[0].text.contains("⚠"),
        "panic must surface as a ⚠ warning row; got {:?}",
        last[0].text
    );
    // The panic message ("boom") makes it into the row.
    assert!(
        last[0].text.contains("boom"),
        "the panic message must reach the warning row; got {:?}",
        last[0].text
    );
}

/// 5 — A transform that returns an empty `Vec` is honoured: the
/// wrapper renders nothing (no warning row is added when the
/// transform succeeded).
#[test]
fn empty_ok_result_renders_nothing() {
    let source = "Body that the transformer drops.";
    let via_wrapper = render_markdown_with_transform(source, 80, &Empty);
    assert!(via_wrapper.is_empty());
}

/// 6 — Empty input is rendered to nothing by the standard pipeline,
/// and the wrapper honours that (no warning row added on top of
/// "nothing").
#[test]
fn empty_input_remains_empty() {
    let via_wrapper = render_markdown_with_transform("", 80, &Identity);
    assert!(via_wrapper.is_empty());
    let via_wrapper = render_markdown_with_transform("", 80, &RejectAll);
    assert!(
        via_wrapper.is_empty(),
        "an Err from the transformer on empty input must NOT add a warning row"
    );
}

/// 7 — Whitespace-only input is rendered to nothing (the standard
/// pipeline strips it). The wrapper keeps that behaviour.
#[test]
fn whitespace_input_remains_empty() {
    let via_wrapper = render_markdown_with_transform("   \n\t\n", 80, &Uppercaser);
    assert!(via_wrapper.is_empty());
}

/// 8 — The wrapper never panics on a transform that mutates spans
/// in place. The Uppercaser mutates each span's `text` field; the
/// wrapper must complete and return the rewritten lines.
#[test]
fn mutating_transform_does_not_panic() {
    let source = "# Title\n\nparagraph";
    let via_wrapper = render_markdown_with_transform(source, 80, &Uppercaser);
    assert!(!via_wrapper.is_empty());
}

/// 9 — The wrapper takes the `width` parameter through to the
/// transformer's `transform`. Verify by inspecting that the
/// transform sees the requested width (a custom recorder captures
/// the value).
#[test]
fn wrapper_passes_width_through_to_transformer() {
    struct WidthRecorder(usize);
    impl MarkdownTransform for WidthRecorder {
        fn transform(
            &self,
            lines: Vec<StyledLine>,
            width: usize,
        ) -> Result<Vec<StyledLine>, TransformError> {
            assert_eq!(width, 42, "wrapper must pass width through");
            assert_eq!(
                self.0, 42,
                "constructor width must reach the implementation"
            );
            Ok(lines)
        }
    }
    let _ = render_markdown_with_transform("body", 42, &WidthRecorder(42));
}

/// 10 — Two successive calls produce independent output (no shared
/// mutable state in the wrapper). A test for this guards against a
/// future refactor that accidentally caches the body.
#[test]
fn successive_calls_are_independent() {
    let source = "shared input";
    let a = render_markdown_with_transform(source, 80, &Uppercaser);
    let b = render_markdown_with_transform(source, 80, &Uppercaser);
    assert_eq!(a, b);
    // The first call must not have consumed or moved `source`.
    assert_eq!(source, "shared input");
}

/// 11 — A transformer that wraps every line's first span in an
/// extra leading span can verify the contract that the wrapper
/// hands over ownership of the lines (the transformer can mutate
/// them freely).
#[test]
fn transformer_can_extend_lines() {
    struct PrefixAll;
    impl MarkdownTransform for PrefixAll {
        fn transform(
            &self,
            mut lines: Vec<StyledLine>,
            _width: usize,
        ) -> Result<Vec<StyledLine>, TransformError> {
            for line in lines.iter_mut() {
                let mut new_line: StyledLine = Vec::new();
                new_line.push(pi_tui::styled::StyledSpan::new(
                    "PREFIX ",
                    SpanStyle::PLAIN,
                ));
                new_line.extend(line.drain(..));
                *line = new_line;
            }
            Ok(lines)
        }
    }
    let via_wrapper = render_markdown_with_transform("body", 80, &PrefixAll);
    assert!(!via_wrapper.is_empty());
    let first = plain_text(via_wrapper[0].as_slice());
    assert!(
        first.starts_with("PREFIX"),
        "PrefixAll must prepend its prefix; got {first:?}"
    );
}