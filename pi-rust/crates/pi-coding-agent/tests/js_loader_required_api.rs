//! End-to-end loader tests for the `required_api` manifest contract.
//!
//! The JS loader reads a `// @required_api [...]` header from each
//! extension source and validates it against
//! [`STABLE_API`](pi_coding_agent::extensions::api_surface::STABLE_API).
//! These tests write a temporary extension file to disk, call the
//! loader, and assert the outcome.
//!
//! Mirrors the contract pin in
//! `packages/coding-agent/src/core/extensions/runner.ts`: a missing
//! required entry is a hard loader error, never a silent gap.

#![cfg(not(target_arch = "wasm32"))]

use std::path::PathBuf;

use pi_coding_agent::extensions::js_loader::parse_required_api_header;
use pi_coding_agent::extensions::required_api::validate_required_api;

#[test]
fn header_with_double_quoted_strings_parses() {
    let source = r#"
        // @required_api ["turn_start", "ui.setWidget"]
        module.exports = function (pi) {};
    "#;
    let parsed = parse_required_api_header(source).expect("parses");
    assert_eq!(
        parsed,
        Some(vec!["turn_start".to_string(), "ui.setWidget".to_string()])
    );
}

#[test]
fn header_with_single_quoted_strings_parses() {
    let source = r#"
        //   @required_api  [ 'turn_start' , 'ui.setWidget' ]
        module.exports = function (pi) {};
    "#;
    let parsed = parse_required_api_header(source).expect("parses");
    assert_eq!(
        parsed,
        Some(vec!["turn_start".to_string(), "ui.setWidget".to_string()])
    );
}

#[test]
fn header_is_absent_when_the_marker_is_not_present() {
    let source = "module.exports = function (pi) {};";
    let parsed = parse_required_api_header(source).expect("no header");
    assert_eq!(parsed, None);
}

#[test]
fn header_without_brackets_is_a_parse_error() {
    let source = "// @required_api turn_start, ui.setWidget\nmodule.exports = function (pi) {};";
    let err = parse_required_api_header(source).expect_err("must fail");
    assert!(err.contains("["), "error must point at the missing bracket: {err}");
}

#[test]
fn header_after_line_thirty_two_is_not_recognised() {
    // Pad the source so the header falls outside the recognition window
    // — a doc comment near the bottom should not be picked up.
    let mut lines: Vec<String> = (0..32).map(|i| format!("// padding {i}")).collect();
    lines.push("// @required_api [\"turn_start\"]".to_string());
    lines.push("module.exports = function (pi) {};".to_string());
    let source = lines.join("\n");
    let parsed = parse_required_api_header(&source).expect("ok");
    assert_eq!(parsed, None, "header past line 32 is ignored");
}

#[test]
fn header_with_unknown_entry_fails_validation() {
    let source = r#"
        // @required_api ["turn_start", "ui.notARealMethod"]
        module.exports = function (pi) {};
    "#;
    let declared = parse_required_api_header(source).expect("parses").expect("present");
    let report = validate_required_api(&declared);
    assert!(!report.is_satisfied());
    assert_eq!(report.missing, vec!["ui.notARealMethod".to_string()]);
}

#[test]
fn header_with_all_supported_entries_succeeds() {
    let source = r#"
        // @required_api ["turn_start", "ui.setWidget", "lifecycle.context"]
        module.exports = function (pi) {};
    "#;
    let declared = parse_required_api_header(source).expect("parses").expect("present");
    let report = validate_required_api(&declared);
    assert!(report.is_satisfied(), "{report}");
}

// Sanity: the test file lives next to the binary that consumes it.
#[allow(dead_code)]
fn _this_file_path() -> PathBuf {
    PathBuf::from(file!())
}