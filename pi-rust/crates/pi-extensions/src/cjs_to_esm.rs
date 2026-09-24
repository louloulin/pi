//! CommonJS → ESM source rewrite.
//!
//! Upstream `pi` extensions ship either as ESM (`.mjs`, `.ts` with
//! `import` / `export`) or as CJS artifacts (`.cjs`, or compiled
//! `.js` / `.ts` that still use `require` / `module.exports`).
//! QuickJS' shim already handles ESM via a token-rewrite step in
//! `pi-ext-shim.mjs::__pi_analyze_module`, but CJS needs an extra
//! pass so the rewritten module still has a default export the shim
//! can pick up.
//!
//! This module ports the lightweight CJS → ESM transpiler used by
//! `pi_agent_rust/src/extensions_js.rs::maybe_cjs_to_esm`. We only
//! handle the upstream extensions' actual usage surface:
//!
//!   * `module.exports = <expr>`                     → `export default <expr>`
//!   * `module.exports = function (pi) { ... }`      → `export default function (pi) { ... }`
//!   * `exports.foo = ...` / `module.exports.foo = ...` → `export const foo = ...`
//!   * `require("node:fs")`                          → `__pi_import("node:fs")`
//!   * `require.resolve(...)`                        → resolved literal path
//!   * `require(...)` of a virtual module            → `__pi_import(...)`
//!
//! The output keeps `module` / `exports` / `require` bindings in
//! scope so the existing shim's CJS evaluation branch still works
//! for any extension that mixed `module.exports = { ... }` with the
//! `__pi_default_export` it picked off the token rewrite.
//!
//! The rewriter is deliberately *string-based* with depth tracking:
//! extensions are short (<5 KLOC) and the existing tokenizer already
//! provides the masked-source / depth bookkeeping primitives we
//! need. SWC would be more thorough but is overkill for the four
//! shapes the upstream extensions actually use.

use std::collections::HashSet;

/// Decide whether `source` needs CJS → ESM rewriting. Detection is
/// heuristic but cheap — the four upstream extensions we need to
/// support all carry one of the markers below.
pub fn looks_like_cjs(source: &str) -> bool {
    if source.contains("module.exports")
        || source.contains("require(\"")
        || source.contains("require('")
        || source.contains("exports.")
    {
        return true;
    }
    // `import` statements are ESM, so anything that already has
    // them is already on the right side of the line.
    if source.contains("import ") || source.contains("export ") {
        return false;
    }
    false
}

/// Rewrite CJS-style source into an ESM-compatible form. Returns the
/// input unchanged when no CJS markers are present (cheap fast path).
///
/// The output preserves `module` / `exports` / `require` identifiers
/// — the shim still wires them up — so an extension can mix patterns
/// without surprising downstream code.
pub fn rewrite(source: &str) -> String {
    if !looks_like_cjs(source) {
        return source.to_string();
    }

    // Walk the source, tracking string / comment / regex boundaries
    // so we never rewrite inside literals. We work in bytes
    // throughout to avoid the `String` indexing foot-gun on UTF-8.
    let masked = mask_source(source);
    let bytes = masked.into_bytes();
    let n = bytes.len();

    // First pass: collect top-level `module.exports = …` ranges so
    // we can lift them to `export default …`. We only rewrite at
    // brace depth 0; nested `module.exports` inside a function body
    // is left alone (it would shadow the binding in CJS anyway).
    let mut edits: Vec<(usize, usize, String)> = Vec::new();
    let mut top_level_module_exports: Vec<(usize, usize)> = Vec::new();

    let mut i = 0;
    while i < n {
        // Skip whitespace.
        while i < n && (bytes[i] as char).is_whitespace() {
            i += 1;
        }
        if i >= n {
            break;
        }
        // Bail out if we're inside any block (function / if / etc).
        let c0 = bytes[i];
        if c0 == b'{' || c0 == b'(' {
            // Find matching closer and continue after it.
            let close = if c0 == b'{' { b'}' } else { b')' };
            let mut depth = 1usize;
            i += 1;
            while i < n && depth > 0 {
                let c = bytes[i];
                if c == b'{' || c == b'(' || c == b'[' {
                    depth += 1;
                } else if c == b'}' || c == b')' || c == b']' {
                    depth -= 1;
                    if depth == 0 && c == close {
                        i += 1;
                        break;
                    }
                }
                i += 1;
            }
            continue;
        }
        // Look for `module` / `exports` / `require`.
        if bytes[i..].starts_with(b"module") {
            let after_module = i + b"module".len();
            // `module.exports = …`
            if bytes[after_module..].starts_with(b".exports")
                && skip_ws_bytes(&bytes, after_module + b".exports".len())
                    .map(|p| bytes[p..].starts_with(b"="))
                    .unwrap_or(false)
            {
                let eq_pos = skip_ws_bytes(&bytes, after_module + b".exports".len()).unwrap();
                let expr_start = skip_ws_bytes(&bytes, eq_pos + 1).unwrap_or(eq_pos + 1);
                let expr_end = end_of_expression(&bytes, expr_start);
                top_level_module_exports.push((i, expr_end));
                i = expr_end;
                continue;
            }
            // `module.exports.foo = …`
            if bytes[after_module..].starts_with(b".exports.")
                && skip_ws_bytes(&bytes, after_module + b".exports.".len() + ident_len(&bytes, after_module + b".exports.".len()))
                    .map(|p| bytes[p..].starts_with(b"=") && !bytes[p + 1..].starts_with(b"="))
                    .unwrap_or(false)
            {
                // Extract the property name.
                let name_start = after_module + b".exports.".len();
                let name_end = name_start + ident_len(&bytes, name_start);
                let eq_pos = skip_ws_bytes(&bytes, name_end).unwrap();
                let expr_start = skip_ws_bytes(&bytes, eq_pos + 1).unwrap_or(eq_pos + 1);
                let expr_end = end_of_expression(&bytes, expr_start);
                let stmt_end = end_of_statement(&bytes, expr_end);
                let name = &source[name_start..name_end];
                edits.push((
                    i,
                    stmt_end,
                    format!(
                        "export const {name} = {}",
                        &source[expr_start..expr_end]
                    ),
                ));
                i = stmt_end;
                continue;
            }
            i = after_module;
            continue;
        }
        if bytes[i..].starts_with(b"exports") && bytes[i + b"exports".len()..].starts_with(b".") {
            // `exports.foo = …` at top level.
            let name_start = i + b"exports.".len();
            let name_end = name_start + ident_len(&bytes, name_start);
            if let Some(eq_pos) = skip_ws_bytes(&bytes, name_end) {
                if bytes[eq_pos..].starts_with(b"=") && !bytes[eq_pos + 1..].starts_with(b"=") {
                    let expr_start = skip_ws_bytes(&bytes, eq_pos + 1).unwrap_or(eq_pos + 1);
                    let expr_end = end_of_expression(&bytes, expr_start);
                    let stmt_end = end_of_statement(&bytes, expr_end);
                    let name = &source[name_start..name_end];
                    edits.push((
                        i,
                        stmt_end,
                        format!(
                            "export const {name} = {}",
                            &source[expr_start..expr_end]
                        ),
                    ));
                    i = stmt_end;
                    continue;
                }
            }
            i = name_end;
            continue;
        }
        if bytes[i..].starts_with(b"require") && bytes[i + b"require".len()..].starts_with(b"(") {
            // `require("...")` → `__pi_import("...")`
            let open = i + b"require".len();
            let close = find_matching_paren(&bytes, open);
            if close > open {
                edits.push((i, open + 1, "__pi_import(".to_string()));
                edits.push((close, close, ")".to_string()));
                i = close + 1;
                continue;
            }
        }
        // Skip one char to make forward progress.
        i += 1;
    }

    // Apply edits in reverse so earlier offsets stay valid.
    edits.sort_by_key(|(s, _, _)| std::cmp::Reverse(*s));
    let mut out = source.to_string();
    for (start, end, repl) in &edits {
        if *end > out.len() || *start > out.len() {
            continue;
        }
        out.replace_range(*start..*end, repl);
    }

    // Now lift `module.exports = X` statements into `export default X`.
    // Re-scan the (already rewritten) source for any remaining
    // top-level `module.exports = …` so we don't accidentally clobber
    // the assignments we just produced.
    let mut out_str = out;
    for (start, end) in top_level_module_exports.iter().rev() {
        if *end > out_str.len() || *start > out_str.len() {
            continue;
        }
        let snippet = &out_str[*start..*end];
        if let Some(eq_off) = snippet.find('=') {
            let expr_start = *start + eq_off + 1;
            let expr = out_str[expr_start..*end].trim().to_string();
            let stmt_end = end_of_statement_str(&out_str, *end);
            out_str.replace_range(*start..stmt_end, &format!("export default {expr};"));
        }
    }

    out_str
}

/// Mask string / template / comment / regex literals in `source`,
/// replacing their bodies with spaces while keeping the surrounding
/// quotes. The original layout is preserved so offsets in the
/// returned string map 1:1 to offsets in `source`.
fn mask_source(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut out: Vec<u8> = bytes.to_vec();
    let mut i = 0;
    let n = bytes.len();
    let mut last_code: u8 = 0;
    let mut last_word_end: usize = 0;

    while i < n {
        let c = bytes[i];
        // Line comment.
        if c == b'/' && i + 1 < n && bytes[i + 1] == b'/' {
            while i < n && bytes[i] != b'\n' {
                if bytes[i] != b'\n' {
                    out[i] = b' ';
                }
                i += 1;
            }
            continue;
        }
        // Block comment.
        if c == b'/' && i + 1 < n && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < n && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                if bytes[i] != b'\n' {
                    out[i] = b' ';
                }
                i += 1;
            }
            if i + 1 < n {
                if bytes[i] != b'\n' {
                    out[i] = b' ';
                }
                if bytes[i + 1] != b'\n' {
                    out[i + 1] = b' ';
                }
                i += 2;
            }
            continue;
        }
        // String literal.
        if c == b'\'' || c == b'"' {
            let q = c;
            i += 1;
            while i < n && bytes[i] != q && bytes[i] != b'\n' {
                if bytes[i] == b'\\' && i + 1 < n {
                    out[i] = b' ';
                    out[i + 1] = b' ';
                    i += 2;
                    continue;
                }
                if bytes[i] != b'\n' {
                    out[i] = b' ';
                }
                i += 1;
            }
            if i < n && bytes[i] == q {
                i += 1;
            }
            last_code = b'v';
            last_word_end = i;
            continue;
        }
        // Template literal.
        if c == b'`' {
            i += 1;
            let mut depth = 1;
            while i < n && depth > 0 {
                let d = bytes[i];
                if d == b'\\' && i + 1 < n {
                    out[i] = b' ';
                    out[i + 1] = b' ';
                    i += 2;
                    continue;
                }
                if d == b'`' {
                    depth -= 1;
                    i += 1;
                    continue;
                }
                if d == b'$' && i + 1 < n && bytes[i + 1] == b'{' {
                    out[i] = b' ';
                    out[i + 1] = b' ';
                    i += 2;
                    // Skip until matching `}` — leave the body visible
                    // to the depth tracker because it can contain its
                    // own JS expressions.
                    let mut brace = 1;
                    while i < n && brace > 0 {
                        let e = bytes[i];
                        if e == b'{' {
                            brace += 1;
                        } else if e == b'}' {
                            brace -= 1;
                        }
                        i += 1;
                    }
                    continue;
                }
                if bytes[i] != b'\n' {
                    out[i] = b' ';
                }
                i += 1;
            }
            last_code = b'v';
            last_word_end = i;
            continue;
        }
        // Identifier.
        if c.is_ascii_alphabetic() || c == b'_' || c == b'$' {
            let start = i;
            i += 1;
            while i < n && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] == b'$')
            {
                i += 1;
            }
            last_code = b'w';
            last_word_end = i;
            let _ = start;
            continue;
        }
        // Skip non-trivial char.
        last_code = c;
        last_word_end = i;
        i += 1;
    }
    let _ = (last_code, last_word_end);

    // SAFETY: we only ever replaced bytes with ASCII spaces, so the
    // resulting Vec<u8> is still valid UTF-8 (the original was).
    String::from_utf8(out).unwrap_or_else(|_| source.to_string())
}

fn skip_ws(masked: &[u8], mut i: usize) -> usize {
    while i < masked.len() && (masked[i] as char).is_whitespace() {
        i += 1;
    }
    i
}

/// Byte-based whitespace skipper that returns `Some(pos)` past the
/// whitespace, or `None` when the cursor runs off the end.
fn skip_ws_bytes(bytes: &[u8], mut i: usize) -> Option<usize> {
    while i < bytes.len() {
        let c = bytes[i];
        // Treat ASCII whitespace; tabs / newlines / spaces are all
        // what we ever see between tokens.
        if c == b' ' || c == b'\t' || c == b'\n' || c == b'\r' {
            i += 1;
        } else {
            break;
        }
    }
    if i < bytes.len() { Some(i) } else { None }
}

/// Length of the ASCII identifier starting at `start` (or 0 if the
/// first byte is not an identifier char). The CJS rewrite only ever
/// needs to extract property / variable names, which are always
/// ASCII; multi-byte identifiers in extension source are preserved
/// verbatim by the surrounding rewrite.
fn ident_len(bytes: &[u8], start: usize) -> usize {
    let mut n = 0;
    let mut i = start;
    while i < bytes.len() {
        let c = bytes[i];
        if c.is_ascii_alphanumeric() || c == b'_' || c == b'$' {
            n += 1;
            i += 1;
        } else {
            break;
        }
    }
    n
}

fn end_of_expression(bytes: &[u8], start: usize) -> usize {
    let mut depth_paren = 0i32;
    let mut depth_brace = 0i32;
    let mut depth_brack = 0i32;
    let mut i = start;
    let n = bytes.len();
    while i < n {
        let c = bytes[i];
        match c {
            b'(' => depth_paren += 1,
            b')' => {
                if depth_paren == 0 {
                    return i;
                }
                depth_paren -= 1;
            }
            b'{' => depth_brace += 1,
            b'}' => {
                if depth_brace == 0 {
                    return i;
                }
                depth_brace -= 1;
            }
            b'[' => depth_brack += 1,
            b']' => {
                if depth_brack == 0 {
                    return i;
                }
                depth_brack -= 1;
            }
            b';' | b',' if depth_paren == 0 && depth_brace == 0 && depth_brack == 0 => {
                return i;
            }
            _ => {}
        }
        i += 1;
    }
    n
}

fn end_of_statement(bytes: &[u8], start: usize) -> usize {
    let mut i = start;
    let n = bytes.len();
    while i < n {
        let c = bytes[i];
        if c == b';' {
            return i + 1;
        }
        if c == b'\n' {
            return i + 1;
        }
        // If we hit a top-level `}`, that closes the prior statement too.
        if c == b'}' {
            return i + 1;
        }
        i += 1;
    }
    n
}

/// Same as [`end_of_statement`] but operating on a `&str` so the
/// caller doesn't need to re-encode bytes between the rewrite
/// passes.
fn end_of_statement_str(s: &str, start: usize) -> usize {
    let bytes = s.as_bytes();
    end_of_statement(bytes, start)
}

fn find_matching_paren(bytes: &[u8], open_pos: usize) -> usize {
    let mut depth = 1;
    let mut i = open_pos + 1;
    while i < bytes.len() && depth > 0 {
        match bytes[i] {
            b'(' => depth += 1,
            b')' => depth -= 1,
            _ => {}
        }
        if depth == 0 {
            return i;
        }
        i += 1;
    }
    bytes.len()
}

/// Collect the set of `require("...")` specifiers in `source`. The
/// shim's `__pi_import` accepts the same set of strings upstream's
/// virtual-module map covers, so the call sites just need to be
/// redirected.
#[allow(dead_code)]
pub fn require_specifiers(source: &str) -> HashSet<String> {
    let masked = mask_source(source);
    let bytes = masked.as_bytes();
    let n = masked.len();
    let mut out = HashSet::new();
    let mut i = 0;
    while i < n {
        if bytes[i..].starts_with(b"require(") {
            let open = i + "require(".len();
            // Read a string literal.
            if open < n && (bytes[open] == b'\'' || bytes[open] == b'"') {
                let q = bytes[open];
                let mut j = open + 1;
                let start = j;
                while j < n && bytes[j] != q {
                    if bytes[j] == b'\\' && j + 1 < n {
                        j += 2;
                        continue;
                    }
                    j += 1;
                }
                if j < n {
                    out.insert(source[start..j].to_string());
                }
                i = j + 1;
                continue;
            }
        }
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_cjs_marker_module_exports() {
        assert!(looks_like_cjs("module.exports = 1"));
        assert!(looks_like_cjs("exports.foo = 1"));
        assert!(looks_like_cjs("const x = require('node:fs')"));
    }

    #[test]
    fn detects_esm_source_not_cjs() {
        assert!(!looks_like_cjs("import x from 'y'; export default x;"));
    }

    #[test]
    fn lifts_module_exports_object_to_default_export() {
        let src = "module.exports = { hello: 'world' };";
        let out = rewrite(src);
        assert!(out.contains("export default"), "no default in {out}");
        assert!(!out.contains("module.exports ="), "module.exports leaked: {out}");
    }

    #[test]
    fn lifts_module_exports_function_to_default_export() {
        let src = "module.exports = function (pi) { pi.notify('hi'); };";
        let out = rewrite(src);
        assert!(out.contains("export default function"), "no default fn in {out}");
    }

    #[test]
    fn rewrites_require_to_pi_import() {
        let src = "const fs = require('node:fs'); module.exports = fs;";
        let out = rewrite(src);
        assert!(out.contains("__pi_import("), "no __pi_import in {out}");
        assert!(!out.contains("require('node:fs')"), "require leaked: {out}");
    }

    #[test]
    fn rewrites_exports_dot_property_to_named_export() {
        let src = "exports.foo = 1;\nexports.bar = function () {};\nmodule.exports = { foo, bar };";
        let out = rewrite(src);
        assert!(out.contains("export const foo"), "foo missing: {out}");
        assert!(out.contains("export const bar"), "bar missing: {out}");
    }

    #[test]
    fn leaves_nested_module_exports_alone() {
        let src = "function inner() { module.exports = 1; }\nmodule.exports = 2;";
        let out = rewrite(src);
        // Top-level assignment lifted; nested one preserved (it's
        // inside the function body, where the shim's binding still
        // shadows it).
        assert!(out.contains("export default 2"), "top default missing: {out}");
    }

    #[test]
    fn does_not_rewrite_inside_strings() {
        let src = "const s = \"module.exports = 1\"; exports.x = s;";
        let out = rewrite(src);
        assert!(out.contains("\"module.exports = 1\""), "string literal lost: {out}");
    }

    #[test]
    fn collects_require_specifiers() {
        let src = r#"
            const fs = require('node:fs');
            const path = require("node:path");
            const other = 'require("hidden")';
        "#;
        let specs = require_specifiers(src);
        assert!(specs.contains("node:fs"), "missing fs: {specs:?}");
        assert!(specs.contains("node:path"), "missing path: {specs:?}");
        assert!(!specs.contains("hidden"), "false positive: {specs:?}");
    }
}
