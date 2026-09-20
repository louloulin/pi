//! A small, dependency-free line / word diff.
//!
//! Rust port of the `diff` npm package's `diffLines` and `diffWords`
//! entry points, which the upstream `edit` tool uses for its
//! `generateDiffString` / `generateUnifiedPatch` output and for the
//! intra-line highlighting in
//! `modes/interactive/components/diff.ts`.
//!
//! The npm package is Myers-based (`git diff`'s algorithm). This module
//! implements the same Myers O(ND) shortest-edit-script search and then
//! groups the resulting token runs into the same part shape the callers
//! rely on:
//!
//! * [`DiffPart::kind`] is `Equal` / `Removed` / `Added`.
//! * [`DiffPart::value`] concatenates the source tokens (each line keeps
//!   its original terminator, so the parts reconstruct the input exactly).
//! * A run of changes is always emitted as all removed tokens followed by
//!   all added tokens, matching `diffLines`' output ordering.
//!
//! The two public tokenizers differ only in granularity:
//! [`diff_lines`] compares whole lines, [`diff_words`] compares
//! whitespace / word / punctuation runs so callers can highlight the
//! changed words inside a modified line.

/// What happened to a run of tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffKind {
    /// The run is present in both inputs.
    Equal,
    /// The run is present only in the second input.
    Added,
    /// The run is present only in the first input.
    Removed,
}

/// One run of tokens with its change status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffPart {
    /// Change status of the run.
    pub kind: DiffKind,
    /// Concatenated source tokens.
    pub value: String,
}

/// One step of the Myers edit script.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    /// Both sides advance (`a[i] == b[j]`).
    Equal(usize, usize),
    /// `a[i]` is deleted.
    Delete(usize),
    /// `b[j]` is inserted.
    Insert(usize),
}

/// Tokenize `content` into lines, keeping each line's `\n` terminator.
///
/// Equivalent to the upstream `splitLinesWithEndings` (`/[^\n]*\n|[^\n]+/g`):
/// every returned slice ends with `\n` except possibly the final one, and the
/// empty string yields no tokens.
fn split_lines_with_endings(content: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    for (idx, byte) in content.bytes().enumerate() {
        if byte == b'\n' {
            out.push(&content[start..=idx]);
            start = idx + 1;
        }
    }
    if start < content.len() {
        out.push(&content[start..]);
    }
    out
}

/// The token category used by the word tokenizer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WordCat {
    /// Whitespace run.
    Space,
    /// Alphanumeric / `_` run.
    Word,
    /// Punctuation run.
    Punct,
}

/// Tokenize into whitespace / word / punctuation runs.
///
/// The npm `diffWords` tokenizer is not reproduced byte-for-byte; this
/// grouping keeps the same visible behaviour (words and their neighbouring
/// punctuation are highlighted separately, whitespace is never highlighted)
/// without porting jsdiff's `wordChars` tables.
fn tokenize_words(content: &str) -> Vec<&str> {
    fn cat(ch: char) -> WordCat {
        if ch.is_whitespace() {
            WordCat::Space
        } else if ch.is_alphanumeric() || ch == '_' {
            WordCat::Word
        } else {
            WordCat::Punct
        }
    }

    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    let mut current = WordCat::Space;
    for (idx, ch) in content.char_indices() {
        let this = cat(ch);
        match start {
            Some(_) if this != current => {
                out.push(&content[start.unwrap()..idx]);
                start = Some(idx);
                current = this;
            }
            None => {
                start = Some(idx);
                current = this;
            }
            Some(_) => {}
        }
    }
    if let Some(start) = start {
        out.push(&content[start..]);
    }
    out
}

/// Myers O(ND) shortest edit script between two token slices.
///
/// Keeps the full "V per depth" trace so the script can be reconstructed;
/// memory is O((N+M)^2) in the worst case, which is fine for the file sizes
/// an edit produces (jsdiff pays the same cost).
fn myers_steps(a: &[&str], b: &[&str]) -> Vec<Step> {
    let n = a.len() as isize;
    let m = b.len() as isize;
    let max = (n + m) as usize;
    let offset = (max + 1) as isize;
    let mut v = vec![0isize; 2 * (max + 1) + 1];
    let mut trace: Vec<Vec<isize>> = Vec::new();

    for d in 0..=max as isize {
        trace.push(v.clone());
        let mut k = -d;
        while k <= d {
            let idx = (k + offset) as usize;
            let down = k == -d || (k != d && v[idx - 1] < v[idx + 1]);
            let mut x = if down { v[idx + 1] } else { v[idx - 1] + 1 };
            let mut y = x - k;
            while x < n && y < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            v[idx] = x;
            if x >= n && y >= m {
                return backtrack(&trace, n, m, d, offset);
            }
            k += 2;
        }
    }
    Vec::new()
}

/// Walk the stored `V` snapshots backwards to recover the edit script.
fn backtrack(trace: &[Vec<isize>], n: isize, m: isize, d_max: isize, offset: isize) -> Vec<Step> {
    let mut x = n;
    let mut y = m;
    let mut path = Vec::new();

    for d in (0..=d_max).rev() {
        let v = &trace[d as usize];
        let k = x - y;
        let prev_k =
            if k == -d || (k != d && v[(k - 1 + offset) as usize] < v[(k + 1 + offset) as usize]) {
                k + 1
            } else {
                k - 1
            };
        let prev_x = v[(prev_k + offset) as usize];
        let prev_y = prev_x - prev_k;

        while x > prev_x && y > prev_y {
            path.push(Step::Equal((x - 1) as usize, (y - 1) as usize));
            x -= 1;
            y -= 1;
        }
        if d > 0 {
            if x == prev_x {
                path.push(Step::Insert((y - 1) as usize));
                y -= 1;
            } else {
                path.push(Step::Delete((x - 1) as usize));
                x -= 1;
            }
        }
    }

    path.reverse();
    path
}

/// Turn an edit script into `diff`-shaped parts, grouping runs and always
/// emitting removed tokens before added ones.
fn parts_from_steps(a: &[&str], b: &[&str], steps: &[Step]) -> Vec<DiffPart> {
    let mut parts: Vec<DiffPart> = Vec::new();
    let mut removed: Vec<&str> = Vec::new();
    let mut added: Vec<&str> = Vec::new();

    fn push(parts: &mut Vec<DiffPart>, kind: DiffKind, value: String) {
        if value.is_empty() {
            return;
        }
        if let Some(last) = parts.last_mut() {
            if last.kind == kind {
                last.value.push_str(&value);
                return;
            }
        }
        parts.push(DiffPart { kind, value });
    }

    fn flush(parts: &mut Vec<DiffPart>, removed: &mut Vec<&str>, added: &mut Vec<&str>) {
        if !removed.is_empty() {
            push(parts, DiffKind::Removed, removed.concat());
            removed.clear();
        }
        if !added.is_empty() {
            push(parts, DiffKind::Added, added.concat());
            added.clear();
        }
    }

    for step in steps {
        match *step {
            Step::Equal(i, _) => {
                flush(&mut parts, &mut removed, &mut added);
                push(&mut parts, DiffKind::Equal, a[i].to_string());
            }
            Step::Delete(i) => removed.push(a[i]),
            Step::Insert(j) => added.push(b[j]),
        }
    }
    flush(&mut parts, &mut removed, &mut added);
    parts
}

fn diff_tokens(a: &[&str], b: &[&str]) -> Vec<DiffPart> {
    let steps = myers_steps(a, b);
    parts_from_steps(a, b, &steps)
}

/// Line-granular diff (npm `diff.diffLines`).
///
/// Each input is split into lines that keep their terminators, so the
/// concatenation of all `Equal` + `Removed` values is the first input and
/// `Equal` + `Added` is the second.
pub fn diff_lines(old_content: &str, new_content: &str) -> Vec<DiffPart> {
    let a = split_lines_with_endings(old_content);
    let b = split_lines_with_endings(new_content);
    diff_tokens(&a, &b)
}

/// Word-granular diff (npm `diff.diffWords`-equivalent) used for intra-line
/// highlighting.
pub fn diff_words(old_content: &str, new_content: &str) -> Vec<DiffPart> {
    let a = tokenize_words(old_content);
    let b = tokenize_words(new_content);
    diff_tokens(&a, &b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(parts: &[DiffPart]) -> Vec<DiffKind> {
        parts.iter().map(|p| p.kind).collect()
    }

    #[test]
    fn equal_content_is_a_single_equal_part() {
        let parts = diff_lines("a\nb\n", "a\nb\n");
        assert_eq!(kinds(&parts), vec![DiffKind::Equal]);
        assert_eq!(parts[0].value, "a\nb\n");
    }

    #[test]
    fn changed_line_splits_into_removed_then_added() {
        let parts = diff_lines("a\nb\nc\n", "a\nX\nc\n");
        assert_eq!(
            kinds(&parts),
            vec![
                DiffKind::Equal,
                DiffKind::Removed,
                DiffKind::Added,
                DiffKind::Equal
            ]
        );
        assert_eq!(parts[1].value, "b\n");
        assert_eq!(parts[2].value, "X\n");
    }

    #[test]
    fn parts_reconstruct_both_inputs() {
        let old = "one\ntwo\nthree\nfour\n";
        let new = "one\nTWO\nthree\nfour\nfive\n";
        for (old_content, new_content) in [(old, new), (new, old)] {
            let parts = diff_lines(old_content, new_content);
            let rebuilt_old: String = parts
                .iter()
                .filter(|p| p.kind != DiffKind::Added)
                .map(|p| p.value.as_str())
                .collect();
            let rebuilt_new: String = parts
                .iter()
                .filter(|p| p.kind != DiffKind::Removed)
                .map(|p| p.value.as_str())
                .collect();
            assert_eq!(rebuilt_old, old_content);
            assert_eq!(rebuilt_new, new_content);
        }
    }

    #[test]
    fn pure_insert_and_pure_delete() {
        let inserted = diff_lines("a\nc\n", "a\nb\nc\n");
        assert_eq!(
            kinds(&inserted),
            vec![DiffKind::Equal, DiffKind::Added, DiffKind::Equal]
        );
        let deleted = diff_lines("a\nb\nc\n", "a\nc\n");
        assert_eq!(
            kinds(&deleted),
            vec![DiffKind::Equal, DiffKind::Removed, DiffKind::Equal]
        );
    }

    #[test]
    fn empty_inputs_are_handled() {
        assert!(diff_lines("", "").is_empty());
        let added = diff_lines("", "a\n");
        assert_eq!(added.len(), 1);
        assert_eq!(added[0].kind, DiffKind::Added);
        let removed = diff_lines("a\n", "");
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].kind, DiffKind::Removed);
    }

    #[test]
    fn final_line_without_newline_is_preserved() {
        let parts = diff_lines("a", "a\n");
        // The missing terminator makes the final token differ.
        assert!(parts.iter().any(|p| p.kind == DiffKind::Added));
        let rebuilt_old: String = parts
            .iter()
            .filter(|p| p.kind != DiffKind::Added)
            .map(|p| p.value.as_str())
            .collect();
        let rebuilt_new: String = parts
            .iter()
            .filter(|p| p.kind != DiffKind::Removed)
            .map(|p| p.value.as_str())
            .collect();
        assert_eq!(rebuilt_old, "a");
        assert_eq!(rebuilt_new, "a\n");
    }

    #[test]
    fn word_diff_marks_only_the_changed_word() {
        let parts = diff_words("let x = 1;", "let y = 1;");
        assert_eq!(
            kinds(&parts),
            vec![
                DiffKind::Equal,
                DiffKind::Removed,
                DiffKind::Added,
                DiffKind::Equal
            ]
        );
        assert_eq!(parts[1].value, "x");
        assert_eq!(parts[2].value, "y");
    }

    #[test]
    fn word_diff_reconstructs_both_sides() {
        let old = "  foo(bar, baz)";
        let new = "  foo(qux, baz)";
        let parts = diff_words(old, new);
        let rebuilt_old: String = parts
            .iter()
            .filter(|p| p.kind != DiffKind::Added)
            .map(|p| p.value.as_str())
            .collect();
        let rebuilt_new: String = parts
            .iter()
            .filter(|p| p.kind != DiffKind::Removed)
            .map(|p| p.value.as_str())
            .collect();
        assert_eq!(rebuilt_old, old);
        assert_eq!(rebuilt_new, new);
    }

    #[test]
    fn multibyte_lines_round_trip() {
        let old = "你好，世界\n第二行\n";
        let new = "你好，世界\n改过的行\n";
        let parts = diff_lines(old, new);
        assert_eq!(parts[1].value, "第二行\n");
        assert_eq!(parts[2].value, "改过的行\n");
    }
}
