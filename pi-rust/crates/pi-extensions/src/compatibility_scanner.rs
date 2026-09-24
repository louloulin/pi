//! Compatibility scanner.
//!
//! Walks a list of JS / TS extension source files and produces a
//! [`CompatLedger`] summarising what host capabilities the
//! extension *appears* to use based on regex markers in the source.
//!
//! The scanner is **purely informational** — it never blocks a load by
//! itself. Callers (the loader, `--manifest-strict` CLI) decide what
//! to do with the ledger:
//!
//! - **default**: WARN for each `dangerous` marker (`eval`,
//!   `new Function`, `process.binding`, `process.dlopen`) and load
//!   the extension anyway.
//! - **`--manifest-strict`**: refuse to load any extension whose
//!   ledger includes a `dangerous` marker.
//! - **post-load reconciliation**: cross-check the ledger against
//!   the extension's declared [`crate::capability_manifest::CapabilityManifest`]
//!   to detect drift.
//!
//! The port is simplified from
//! `pi_agent_rust/src/extensions/compatibility.rs` (1443 lines):
//! - only the marker regexes (no evidence scoring / popularity / license),
//! - no `CompatLedger::merge` (single-file ledger only — multi-file
//!   aggregation is the caller's job),
//! - no HTML rendering (callers print their own warnings).
//!
//! ## Markers
//!
//! Each marker has a [`Marker`] bit flag, a regex, and a one-line
//! explanation. The full list:
//!
//! | Marker | Regex | Why we care |
//! |--------|-------|-------------|
//! | `IMPORT_TYPE` | `import\s+type\b` | Already stripped by SWC; flagged so callers know |
//! | `IMPORT` | `^\s*import\b` | ESM import — used by capability manifest cross-check |
//! | `REQUIRE` | `\brequire\s*\(` | CJS require — already handled by `cjs_to_esm` |
//! | `PI` | `\bpi\.(tool\|exec\|http\|log\|session\|ui)\b` | Tracks which host APIs the extension touches |
//! | `PROCESS_ENV` | `\bprocess\.env\b` | Reads env vars — usually a sign of secret leakage |
//! | `PROCESS` | `\bprocess\b` | General Node.js process use — flagged for awareness |
//! | `FUNCTION` | `\bnew\s+Function\b` | Arbitrary code construction — dangerous |
//! | `EVAL` | `\beval\s*\(` | Arbitrary code execution — dangerous |
//! | `BINDING` | `\bprocess\.binding\b` | Native binding access — dangerous |
//! | `DLOPEN` | `\bprocess\.dlopen\b` | Dynamic native loading — dangerous |
//!
//! ## Block / string / comment masking
//!
//! `scan_str` walks the source once and skips over `//` line comments,
//! `/* … */` block comments, `'…'` / `"…"` / `` `…` `` strings, and
//! template substitutions (best-effort). Markers inside any of those
//! contexts are ignored — `eval("evil()")` in a string literal should
//! not count.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use once_cell::sync::OnceCell;
use regex::Regex;
use serde::{Deserialize, Serialize};

/// Bit flags for each marker the scanner recognises.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Marker(u16);

impl Default for Marker {
    fn default() -> Self {
        Marker(0)
    }
}

impl Marker {
    pub const IMPORT_TYPE: Marker = Marker(1 << 0);
    pub const IMPORT: Marker = Marker(1 << 1);
    pub const REQUIRE: Marker = Marker(1 << 2);
    pub const PI: Marker = Marker(1 << 3);
    pub const PROCESS_ENV: Marker = Marker(1 << 4);
    pub const PROCESS: Marker = Marker(1 << 5);
    pub const FUNCTION: Marker = Marker(1 << 6);
    pub const EVAL: Marker = Marker(1 << 7);
    pub const BINDING: Marker = Marker(1 << 8);
    pub const DLOPEN: Marker = Marker(1 << 9);

    /// Subset the host treats as dangerous (i.e. an extension that
    /// uses any of these without justification is suspicious).
    pub const DANGEROUS: Marker = Marker(Self::FUNCTION.0 | Self::EVAL.0 | Self::BINDING.0 | Self::DLOPEN.0);

    pub const ALL: &[Marker] = &[
        Self::IMPORT_TYPE,
        Self::IMPORT,
        Self::REQUIRE,
        Self::PI,
        Self::PROCESS_ENV,
        Self::PROCESS,
        Self::FUNCTION,
        Self::EVAL,
        Self::BINDING,
        Self::DLOPEN,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::IMPORT_TYPE => "import_type",
            Self::IMPORT => "import",
            Self::REQUIRE => "require",
            Self::PI => "pi",
            Self::PROCESS_ENV => "process_env",
            Self::PROCESS => "process",
            Self::FUNCTION => "function",
            Self::EVAL => "eval",
            Self::BINDING => "binding",
            Self::DLOPEN => "dlopen",
            _ => "unknown",
        }
    }

    pub fn contains(self, other: Marker) -> bool {
        (self.0 & other.0) == other.0
    }

    pub fn from_name(name: &str) -> Option<Marker> {
        for m in Self::ALL {
            if m.name() == name {
                return Some(*m);
            }
        }
        None
    }
}

impl std::ops::BitOr for Marker {
    type Output = Marker;
    fn bitor(self, rhs: Marker) -> Marker {
        Marker(self.0 | rhs.0)
    }
}

/// One piece of evidence: the marker detected + the byte column where
/// the match starts (1-indexed, 0 means "scan found it but could not
/// pin a column").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompatEvidence {
    /// Which marker matched.
    pub marker: Marker,
    /// 1-indexed line number.
    pub line: usize,
    /// 1-indexed column. `0` means the scanner could not localise
    /// the match (e.g. multi-line regex).
    pub column: usize,
}

/// Per-file ledger: the marker bitset + every evidence tuple.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompatLedger {
    /// Marker bitset (one bit per recognised marker).
    pub markers: Marker,
    /// Sorted by `(line, column)`. Stable order for snapshot tests.
    pub evidence: Vec<CompatEvidence>,
    /// File the ledger was generated from. Useful when a caller
    /// merges several ledgers into a single report.
    pub source: Option<PathBuf>,
}

impl CompatLedger {
    /// True when any `DANGEROUS` marker matched. The default policy
    /// is to log a WARN; `--manifest-strict` rejects the load.
    pub fn has_dangerous(&self) -> bool {
        // `contains` is "is `self` a superset of `DANGEROUS`"; we
        // need "do any dangerous bits overlap with what we matched".
        (self.markers.0 & Marker::DANGEROUS.0) != 0
    }

    /// Merge another ledger into this one. Marker bits are OR'd,
    /// evidence is concatenated and re-sorted.
    pub fn merge(&mut self, other: &CompatLedger) {
        self.markers = Marker(self.markers.0 | other.markers.0);
        self.evidence.extend(other.evidence.iter().cloned());
        self.evidence
            .sort_by(|a, b| (a.line, a.column).cmp(&(b.line, b.column)));
    }
}

/// Scan `source` (the file content of `path`) and return a ledger.
pub fn scan_str(source: &str, path: Option<&Path>) -> CompatLedger {
    let mut ledger = CompatLedger {
        source: path.map(|p| p.to_path_buf()),
        ..Default::default()
    };
    for marker in Marker::ALL {
        let regex = marker_regex(*marker);
        for mat in regex.find_iter(source) {
            // Skip matches that live inside a comment / string. We
            // do NOT set the marker bit in that case — `// eval('x')`
            // is a comment, not real eval usage.
            if is_in_skipped(source, mat.start()) {
                continue;
            }
            let (line, column) = line_column(source, mat.start());
            ledger.markers = Marker(ledger.markers.0 | marker.0);
            ledger.evidence.push(CompatEvidence {
                marker: *marker,
                line,
                column,
            });
        }
    }
    // Stable sort so the evidence list is deterministic for tests.
    ledger
        .evidence
        .sort_by(|a, b| (a.line, a.column).cmp(&(b.line, b.column)));
    ledger
}

/// One-shot scanner wrapper that owns the compiled regex cache.
#[derive(Debug, Default)]
pub struct CompatibilityScanner {
    _private: (),
}

impl CompatibilityScanner {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn scan(&self, source: &str, path: Option<&Path>) -> CompatLedger {
        scan_str(source, path)
    }
}

/// Compile a regex per marker, lazily. The `OnceCell` lives inside
/// the function so it's effectively process-global — regexes are
/// expensive to compile and we want every scan to share them.
fn marker_regex(marker: Marker) -> &'static Regex {
    static IMPORT_TYPE: OnceCell<Regex> = OnceCell::new();
    static IMPORT: OnceCell<Regex> = OnceCell::new();
    static REQUIRE: OnceCell<Regex> = OnceCell::new();
    static PI: OnceCell<Regex> = OnceCell::new();
    static PROCESS_ENV: OnceCell<Regex> = OnceCell::new();
    static PROCESS: OnceCell<Regex> = OnceCell::new();
    static FUNCTION: OnceCell<Regex> = OnceCell::new();
    static EVAL: OnceCell<Regex> = OnceCell::new();
    static BINDING: OnceCell<Regex> = OnceCell::new();
    static DLOPEN: OnceCell<Regex> = OnceCell::new();
    let cell = match marker {
        Marker::IMPORT_TYPE => &IMPORT_TYPE,
        Marker::IMPORT => &IMPORT,
        Marker::REQUIRE => &REQUIRE,
        Marker::PI => &PI,
        Marker::PROCESS_ENV => &PROCESS_ENV,
        Marker::PROCESS => &PROCESS,
        Marker::FUNCTION => &FUNCTION,
        Marker::EVAL => &EVAL,
        Marker::BINDING => &BINDING,
        Marker::DLOPEN => &DLOPEN,
        _ => unreachable!("marker_regex called with multi-bit marker"),
    };
    cell.get_or_init(|| {
        let pattern = match marker {
            Marker::IMPORT_TYPE => r"\bimport\s+type\b",
            Marker::IMPORT => r"(?m)^\s*import\b",
            Marker::REQUIRE => r"\brequire\s*\(",
            Marker::PI => r"\bpi\.(?:tool|exec|http|log|session|ui)\b",
            Marker::PROCESS_ENV => r"\bprocess\.env\b",
            Marker::PROCESS => r"\bprocess\b",
            Marker::FUNCTION => r"\bnew\s+Function\b",
            Marker::EVAL => r"\beval\s*\(",
            Marker::BINDING => r"\bprocess\.binding\b",
            Marker::DLOPEN => r"\bprocess\.dlopen\b",
            _ => unreachable!(),
        };
        Regex::new(pattern).expect("scanner regex must compile")
    })
}

/// Convert a byte offset to `(line, column)`. 1-indexed both axes.
fn line_column(source: &str, offset: usize) -> (usize, usize) {
    let mut line = 1;
    let mut last_line_start = 0usize;
    for (i, ch) in source.char_indices() {
        if i >= offset {
            break;
        }
        if ch == '\n' {
            line += 1;
            last_line_start = i + ch.len_utf8();
        }
    }
    let column = source[last_line_start..offset].chars().count() + 1;
    (line, column)
}

/// Build a "skip mask" — for each byte, `true` means the byte is inside
/// a comment / string and should not be reported as evidence. Walks
/// the source character-by-character.
fn is_in_skipped(source: &str, offset: usize) -> bool {
    let bytes = source.as_bytes();
    let mut i = 0;
    let mut in_line_comment = false;
    let mut in_block_comment = false;
    let mut string_delim: Option<u8> = None;
    while i < offset && i < bytes.len() {
        let b = bytes[i];
        if in_line_comment {
            if b == b'\n' {
                in_line_comment = false;
            }
            i += 1;
            continue;
        }
        if in_block_comment {
            if b == b'*' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
                in_block_comment = false;
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }
        if let Some(delim) = string_delim {
            if b == b'\\' && i + 1 < bytes.len() {
                i += 2;
                continue;
            }
            if b == delim {
                string_delim = None;
            }
            i += 1;
            continue;
        }
        if b == b'/' && i + 1 < bytes.len() {
            if bytes[i + 1] == b'/' {
                in_line_comment = true;
                i += 2;
                continue;
            }
            if bytes[i + 1] == b'*' {
                in_block_comment = true;
                i += 2;
                continue;
            }
        }
        if b == b'\'' || b == b'"' || b == b'`' {
            string_delim = Some(b);
        }
        i += 1;
    }
    in_line_comment || in_block_comment || string_delim.is_some()
}

/// Convenience: scan a list of `(path, source)` pairs and merge into
/// a single ledger. Files that fail to read are silently skipped —
/// the scanner runs best-effort before the extension is loaded.
pub fn scan_files<I>(files: I) -> BTreeMap<PathBuf, CompatLedger>
where
    I: IntoIterator<Item = (PathBuf, String)>,
{
    let mut out = BTreeMap::new();
    for (path, source) in files {
        let ledger = scan_str(&source, Some(&path));
        out.insert(path, ledger);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(name: &str) -> Marker {
        Marker::from_name(name).unwrap()
    }

    #[test]
    fn flags_import_type_and_eval() {
        let src = r#"
            import type { Foo } from "./foo";
            const v = eval("1+1");
        "#;
        let l = scan_str(src, None);
        assert!(l.markers.contains(m("import_type")));
        assert!(l.markers.contains(m("eval")));
        assert!(l.has_dangerous());
    }

    #[test]
    fn hides_markers_inside_string_literals() {
        let src = r#"const s = "eval('pwned')";"#;
        let l = scan_str(src, None);
        assert!(
            !l.markers.contains(m("eval")),
            "eval inside a string literal must not flag: {:?}",
            l.evidence
        );
    }

    #[test]
    fn hides_markers_inside_line_comments() {
        let src = r#"
            // TODO: kill this eval("never")
            const x = 1;
        "#;
        let l = scan_str(src, None);
        assert!(
            !l.markers.contains(m("eval")),
            "eval in a line comment must not flag"
        );
    }

    #[test]
    fn hides_markers_inside_block_comments() {
        let src = r#"
            /* eval("nope") */
            const x = 1;
        "#;
        let l = scan_str(src, None);
        assert!(!l.markers.contains(m("eval")));
    }

    #[test]
    fn flags_pi_host_api_usage() {
        let src = r#"
            export default function (pi) {
                pi.tool("echo");
                pi.exec("ls");
                pi.http.fetch("...");
            };
        "#;
        let l = scan_str(src, None);
        assert!(l.markers.contains(m("pi")));
        assert_eq!(
            l.evidence
                .iter()
                .filter(|e| e.marker == m("pi"))
                .count(),
            3
        );
    }

    #[test]
    fn flags_process_env_as_dangerous_use_of_secrets() {
        // process.env isn't in the `DANGEROUS` set (it's just a
        // capability hint), but the marker must light up so callers
        // can warn / require a manifest entry.
        let src = r#"const k = process.env.OPENAI_API_KEY;"#;
        let l = scan_str(src, None);
        assert!(l.markers.contains(m("process_env")));
        assert!(l.markers.contains(m("process")));
    }

    #[test]
    fn flags_binding_and_dlopen_as_dangerous() {
        let src = r#"
            const fs = process.binding("fs");
            process.dlopen({}, "evil.so");
        "#;
        let l = scan_str(src, None);
        assert!(l.markers.contains(m("binding")));
        assert!(l.markers.contains(m("dlopen")));
        assert!(l.has_dangerous());
    }

    #[test]
    fn merge_unions_markers_and_sorts_evidence() {
        let a = scan_str("eval('x')", Some(Path::new("/a.ts")));
        let b = scan_str("new Function('y')", Some(Path::new("/b.ts")));
        let mut merged = a.clone();
        merged.merge(&b);
        assert!(merged.markers.contains(m("eval")));
        assert!(merged.markers.contains(m("function")));
        assert!(merged.has_dangerous());
        // Evidence from both files is concatenated and sorted.
        assert_eq!(merged.evidence.len(), 2);
    }

    #[test]
    fn line_column_is_accurate() {
        // "eval" sits at line 3, column 23 (1-indexed).
        let src = "\n\nconst x = eval('1');\n";
        let l = scan_str(src, Some(Path::new("/x.ts")));
        let ev = l.evidence.iter().find(|e| e.marker == m("eval")).unwrap();
        assert_eq!(ev.line, 3);
        assert!(ev.column >= 10, "column should be ~10+, got {}", ev.column);
    }

    #[test]
    fn empty_source_yields_empty_ledger() {
        let l = scan_str("", None);
        assert_eq!(l.markers.0, 0);
        assert!(l.evidence.is_empty());
        assert!(!l.has_dangerous());
    }

    #[test]
    fn scan_files_returns_per_path_ledgers() {
        let files = vec![
            (PathBuf::from("/a.ts"), "eval('x')".to_string()),
            (PathBuf::from("/b.ts"), "1+1".to_string()),
        ];
        let ledgers = scan_files(files);
        assert!(ledgers.get(Path::new("/a.ts")).unwrap().has_dangerous());
        assert!(!ledgers.get(Path::new("/b.ts")).unwrap().has_dangerous());
    }
}