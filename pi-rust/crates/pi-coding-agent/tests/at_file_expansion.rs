//! Integration tests for `@file` expansion + stdin piping.
//!
//! Port of the behavioural contract spelled out by
//! `packages/coding-agent/src/cli/file-processor.ts` + `cli/initial-message.ts`:
//!
//! * every `@<path>` token becomes a `<file name="…">…content…</file>`
//!   block (text) or an image attachment (image MIME type);
//! * missing files are surfaced as a typed `FileError::NotFound` so
//!   `print_mode` can map it to the correct sysexits code;
//! * `@@` is a literal escape for a single `@`;
//! * piped stdin (when stdin is not a TTY) is appended at the end of the
//!   prompt in a `<stdin>…</stdin>` block.
//!
//! The unit tests inside [`crate::file_processor`] cover the same paths;
//! this integration test exercises the *public* API surface so a
//! downstream user can rely on the shape without poking inside.

#![cfg(not(target_arch = "wasm32"))]

use std::io::Write;
use std::path::PathBuf;

use pi_coding_agent::file_processor::{expand_prompt, FileError, MAX_FILE_BYTES};
use pi_protocol::ImageContent;

fn write_temp(contents: &[u8], ext: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "pi-at-file-integration-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("file.{ext}"));
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(contents).unwrap();
    path
}

#[test]
fn expands_an_at_file_token_into_a_named_block() {
    let path = write_temp(b"hello world", "txt");
    let prompt = format!("please review @{}", path.display());
    let out = expand_prompt(&prompt, None).expect("expand");
    assert!(out.images.is_empty());
    let expected_name = path.display().to_string();
    assert!(
        out.text.contains(&format!("<file name=\"{expected_name}\">")),
        "expected a `<file>` block with the resolved path; got: {out:?}"
    );
    assert!(
        out.text.contains("hello world"),
        "expected the file body; got: {out:?}"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn expands_multiple_at_file_tokens_in_order() {
    let first = write_temp(b"first body", "txt");
    let second = write_temp(b"second body", "txt");
    let prompt = format!("@{} and @{}", first.display(), second.display());
    let out = expand_prompt(&prompt, None).expect("expand");
    assert!(out.images.is_empty());
    let first_pos = out.text.find("first body").expect("first body present");
    let second_pos = out
        .text
        .find("second body")
        .expect("second body present");
    assert!(
        first_pos < second_pos,
        "files must appear in the prompt in source order; got: {out:?}"
    );
    let _ = std::fs::remove_file(&first);
    let _ = std::fs::remove_file(&second);
}

#[test]
fn missing_file_is_rejected_with_not_found() {
    // A path that cannot exist anywhere.
    let prompt = "@/definitely/not/here/file.txt please";
    let err = expand_prompt(prompt, None).expect_err("missing file is an error");
    assert!(
        matches!(err, FileError::NotFound { .. }),
        "expected NotFound; got {err:?}"
    );
    assert_eq!(err.exit_code(), 64, "EX_USAGE");
}

#[test]
fn literal_at_is_escaped_by_a_double_at() {
    // No `<file>` block should be emitted — `@someone` is just text.
    let out = expand_prompt("email me at @@someone", None).expect("expand");
    assert!(
        out.text.contains("email me at @someone"),
        "double-at must collapse to a literal single @; got: {out:?}"
    );
    assert!(!out.text.contains("<file"));
}

#[test]
fn piped_stdin_appends_after_files() {
    let path = write_temp(b"file body", "txt");
    let prompt = format!("@{} on stdin", path.display());
    let out = expand_prompt(&prompt, Some("piped content")).expect("expand");
    assert!(
        out.text.contains("<stdin>"),
        "piped stdin must be wrapped in <stdin>…</stdin>; got: {out:?}"
    );
    assert!(out.text.contains("piped content"));
    assert!(out.text.contains("file body"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn image_attachment_keeps_placeholder_block_and_separate_image() {
    // Minimal PNG signature + extension triggers the image path.
    let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
    bytes.extend_from_slice(&[0; 32]);
    let path = write_temp(&bytes, "png");
    let prompt = format!("@{}", path.display());
    let out = expand_prompt(&prompt, None).expect("expand");
    assert_eq!(out.images.len(), 1, "image must surface as attachment");
    let image: &ImageContent = &out.images[0];
    assert_eq!(image.mime_type, "image/png");
    assert!(!image.data.is_empty());
    // The text block stays so the model sees a `<file>` reference even
    // for images (mirrors the upstream TS port).
    let expected_name = path.display().to_string();
    assert!(
        out.text.contains(&format!("<file name=\"{expected_name}\"></file>")),
        "image attachment must add a placeholder `<file>` block; got: {out:?}"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn oversized_file_is_rejected_with_too_large() {
    // One byte past the limit so the rejection branch fires.
    let payload = vec![b'a'; (MAX_FILE_BYTES as usize) + 1];
    let path = write_temp(&payload, "txt");
    let prompt = format!("@{} body", path.display());
    let err = expand_prompt(&prompt, None).expect_err("oversize file is an error");
    assert!(matches!(err, FileError::TooLarge { .. }), "got {err:?}");
    assert_eq!(err.exit_code(), 64);
    let _ = std::fs::remove_file(&path);
}