//! `docs` suite — deterministic documentation integrity.
//!
//! Upstream `docs.eval.ts` audits `packages/coding-agent/docs/**/*.md` with a
//! model-backed agent per page (read → grep → `submit_documentation_audit`),
//! which needs a live model and a judgement call per file. The Rust port
//! cannot make prose-vs-code verdicts offline, so it audits the parts that
//! *are* mechanically checkable and that a broken documentation commit always
//! violates:
//!
//! 1. every relative Markdown link in `pi-rust/docs/**`, `pi-rust/README.md`
//!    and the repository `README.md` resolves to an existing path;
//! 2. every fenced code block is closed;
//! 3. every crate the `pi-rust/README.md` crate map claims to ship has a
//!    `crates/<name>/Cargo.toml`.
//!
//! Point 3 is the direction the upstream audit enforces — documentation
//! claims must be backed by implementation — limited to the claims that can be
//! checked without a model. Prose-vs-code semantic audit is listed as a
//! documented divergence in `crates/pi-evals/README.md`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde_json::json;

use crate::harness::{Case, CaseOutput, EvalError, EvalSuite};

/// `pi-rust/` root, derived from this crate's manifest directory.
fn pi_rust_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Markdown files the audit covers, as `(absolute path, display path)`.
fn audited_pages() -> Result<Vec<(PathBuf, String)>, EvalError> {
    let root = pi_rust_root();
    let mut pages = Vec::new();
    for file in markdown_files(&root.join("docs"))? {
        pages.push((
            file.clone(),
            format!(
                "pi-rust/docs/{}",
                file.file_name().unwrap_or_default().to_string_lossy()
            ),
        ));
    }
    let readme = root.join("README.md");
    if readme.exists() {
        pages.push((readme, "pi-rust/README.md".to_string()));
    }
    let root_readme = root.join("../README.md");
    if root_readme.exists() {
        pages.push((root_readme, "README.md".to_string()));
    }
    pages.sort_by(|a, b| a.1.cmp(&b.1));
    Ok(pages)
}

/// Recursively collect `*.md` files, sorted by path.
fn markdown_files(dir: &Path) -> Result<Vec<PathBuf>, EvalError> {
    let mut files = Vec::new();
    if !dir.is_dir() {
        return Ok(files);
    }
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            files.extend(markdown_files(&path)?);
        } else if path.extension().map(|ext| ext == "md").unwrap_or(false) {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

/// Extract inline Markdown link targets from `text`.
fn link_targets(text: &str) -> Vec<String> {
    let mut targets = Vec::new();
    let bytes = text.as_bytes();
    let mut index = 0;
    while index + 1 < bytes.len() {
        if bytes[index] == b']' && bytes[index + 1] == b'(' {
            let start = index + 2;
            let mut end = start;
            let mut depth = 1;
            while end < bytes.len() && depth > 0 {
                match bytes[end] {
                    b'(' => depth += 1,
                    b')' => depth -= 1,
                    _ => {}
                }
                end += 1;
            }
            if depth == 0 {
                let raw = &text[start..end - 1];
                let target = raw
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .trim_matches(|c| c == '<' || c == '>')
                    .to_string();
                if !target.is_empty() {
                    targets.push(target);
                }
            }
            index = end;
        } else {
            index += 1;
        }
    }
    targets
}

/// Whether a link target is a checkable relative path.
fn is_relative_link(target: &str) -> bool {
    !target.starts_with('#')
        && !target.starts_with("mailto:")
        && !target.contains("://")
        && !target.starts_with("data:")
}

/// Number of code-fence lines in `text`.
fn fence_count(text: &str) -> usize {
    text.lines()
        .filter(|line| line.trim_start().starts_with("```"))
        .count()
}

/// Case 1: relative links resolve.
fn links_case() -> Case {
    Case::builder("docs-relative-links-resolve")
        .description("every relative Markdown link under pi-rust docs and READMEs resolves")
        .assertion("links", |output| {
            let broken = output.output["broken"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            if !broken.is_empty() {
                return Err(format!("broken relative links: {broken:?}"));
            }
            if output.output["pages"].as_u64().unwrap_or(0) == 0 {
                return Err("no documentation pages were audited".into());
            }
            if output.output["links_checked"].as_u64().unwrap_or(0) == 0 {
                return Err("no relative links were found to check".into());
            }
            Ok(())
        })
        .run(|| async {
            let pages = audited_pages()?;
            let mut broken = Vec::new();
            let mut links_checked = 0usize;
            for (path, display) in &pages {
                let text = std::fs::read_to_string(path)?;
                let base = path.parent().unwrap_or_else(|| Path::new("."));
                for target in link_targets(&text) {
                    if !is_relative_link(&target) {
                        continue;
                    }
                    links_checked += 1;
                    let clean = target.split('#').next().unwrap_or("").trim();
                    if clean.is_empty() {
                        continue;
                    }
                    let resolved = base.join(clean);
                    if !resolved.exists() {
                        broken.push(json!(format!("{display} -> {target}")));
                    }
                }
            }
            Ok(CaseOutput {
                output: json!({
                    "pages": pages.len(),
                    "links_checked": links_checked,
                    "broken": broken,
                }),
                ..CaseOutput::default()
            })
        })
        .build()
}

/// Case 2: code fences are balanced.
fn fences_case() -> Case {
    Case::builder("docs-code-fences-balanced")
        .description("every fenced code block in the audited pages is closed")
        .assertion("fences", |output| {
            let unbalanced = output.output["unbalanced"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            if !unbalanced.is_empty() {
                return Err(format!("unbalanced code fences: {unbalanced:?}"));
            }
            if output.output["files"].as_u64().unwrap_or(0) == 0 {
                return Err("no documentation files were audited".into());
            }
            Ok(())
        })
        .run(|| async {
            let pages = audited_pages()?;
            let mut unbalanced = Vec::new();
            for (path, display) in &pages {
                let text = std::fs::read_to_string(path)?;
                if fence_count(&text) % 2 != 0 {
                    unbalanced.push(json!(display));
                }
            }
            Ok(CaseOutput {
                output: json!({
                    "files": pages.len(),
                    "unbalanced": unbalanced,
                }),
                ..CaseOutput::default()
            })
        })
        .build()
}

/// Case 3: README crate-map claims exist as crates.
fn crate_map_case() -> Case {
    Case::builder("docs-readme-crate-map")
        .description("every crate claimed by the pi-rust README crate map ships a Cargo.toml")
        .assertion("crate-map", |output| {
            let missing = output.output["missing_crates"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            if !missing.is_empty() {
                return Err(format!(
                    "README claims crates that do not exist: {missing:?}"
                ));
            }
            let claimed = output.output["claimed_crates"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            if claimed.is_empty() {
                return Err("the README crate map is empty or unparseable".into());
            }
            if !claimed.contains(&json!("pi-evals")) {
                return Err("the README crate map does not list pi-evals".into());
            }
            Ok(())
        })
        .run(|| async {
            let root = pi_rust_root();
            let readme = std::fs::read_to_string(root.join("README.md"))?;
            let mut claimed = BTreeSet::new();
            for line in readme.lines() {
                let trimmed = line.trim();
                if !trimmed.starts_with("| `") {
                    continue;
                }
                if let Some(name) = trimmed
                    .split('`')
                    .nth(1)
                    .map(str::trim)
                    .filter(|name| name.starts_with("pi-"))
                {
                    claimed.insert(name.to_string());
                }
            }
            let missing: Vec<serde_json::Value> = claimed
                .iter()
                .filter(|name| !root.join("crates").join(name).join("Cargo.toml").exists())
                .map(|name| json!(name))
                .collect();
            Ok(CaseOutput {
                output: json!({
                    "claimed_crates": claimed.into_iter().collect::<Vec<_>>(),
                    "missing_crates": missing,
                }),
                ..CaseOutput::default()
            })
        })
        .build()
}

/// Build the `docs` suite.
pub fn suite() -> EvalSuite {
    EvalSuite::new("docs")
        .with_case(links_case())
        .with_case(fences_case())
        .with_case(crate_map_case())
}
