//! Fuzzy matching for searchable lists.
//!
//! A 1:1 port of upstream `packages/tui/src/fuzzy.ts`: a candidate matches
//! when every query character appears in it **in order**, not necessarily
//! consecutively. Lower scores are better, so a filtered list can be
//! sorted best-first.
//!
//! ```
//! use pi_tui::fuzzy::{fuzzy_filter, fuzzy_match};
//!
//! assert!(fuzzy_match("gpt", "gpt-5").matches);
//! assert!(!fuzzy_match("abc", "cba").matches);
//!
//! let items = ["a_p_p", "app", "application"];
//! assert_eq!(fuzzy_filter(&items, "app", |s| *s)[0], &"app");
//! ```
//!
//! Scoring (mirrors upstream `fuzzyMatch`):
//!
//! * consecutive matches get a growing bonus (`-5`, `-10`, `-15`, …)
//!   (the very first match is counted as consecutive, mirroring upstream's
//!   `lastMatchIndex = -1` sentinel),
//! * a gap between two matched characters costs `2` per skipped character
//!   (not charged for the first match),
//! * matching at a word boundary ([`is_word_boundary`]) is worth `-10`,
//! * matches further into the text cost `0.1` per column,
//! * an exact (case-insensitive) whole-text match is worth another `-100`.
//!
//! Like upstream, a query that fails verbatim is retried with its leading
//! letters and trailing digits swapped (`codex52` → `52codex`), which is
//! what makes `codex52` match `gpt-5.2-codex`; such a match is penalised
//! by `+5`. [`fuzzy_match_all`] splits a query into whitespace- and
//! slash-separated tokens (so `openai-codex/gpt-5.5` searches each part)
//! and [`fuzzy_rank`] / [`fuzzy_filter`] apply that to a whole list.

/// Result of [`fuzzy_match`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FuzzyMatch {
    /// Whether every query character was found in order.
    pub matches: bool,
    /// Quality of the match — lower is better. `0.0` when `matches` is
    /// `false`, and also for an empty query.
    pub score: f64,
}

/// Character classes upstream counts as a word boundary before a match
/// (`/[\s\-_./:]/` in `fuzzy.ts`).
fn is_word_boundary(ch: char) -> bool {
    ch.is_whitespace() || matches!(ch, '-' | '_' | '.' | '/' | ':')
}

/// Score one token against one piece of text.
///
/// Both sides are lowercased first, exactly like upstream, so matching is
/// case-insensitive; the index bookkeeping runs over `char`s so multi-byte
/// text cannot panic or mis-score.
fn score_token(query: &[char], text: &[char]) -> FuzzyMatch {
    if query.is_empty() {
        return FuzzyMatch {
            matches: true,
            score: 0.0,
        };
    }
    if query.len() > text.len() {
        return FuzzyMatch {
            matches: false,
            score: 0.0,
        };
    }

    let mut query_index = 0usize;
    let mut score = 0.0f64;
    // Upstream initialises this to `-1` rather than `null`, which makes the
    // very first match (and only a match at index 0, since `-1 == i - 1`
    // only when `i == 0`) take the consecutive branch. Keep the sentinel.
    let mut last_match_index: i64 = -1;
    let mut consecutive_matches = 0u32;

    for (i, ch) in text.iter().enumerate() {
        if query_index >= query.len() {
            break;
        }
        if *ch != query[query_index] {
            continue;
        }

        if last_match_index == i as i64 - 1 {
            // Reward consecutive matches.
            consecutive_matches += 1;
            score -= f64::from(consecutive_matches) * 5.0;
        } else {
            consecutive_matches = 0;
            // Penalise the gap since the previous match (skipped for the
            // first match, exactly like upstream's `lastMatchIndex >= 0`).
            if last_match_index >= 0 {
                score += (i as i64 - last_match_index - 1) as f64 * 2.0;
            }
        }

        // Reward word-boundary matches.
        if i == 0 || is_word_boundary(text[i - 1]) {
            score -= 10.0;
        }

        // Slight penalty for matching later in the text.
        score += i as f64 * 0.1;

        last_match_index = i as i64;
        query_index += 1;
    }

    if query_index < query.len() {
        return FuzzyMatch {
            matches: false,
            score: 0.0,
        };
    }

    if query == text {
        score -= 100.0;
    }

    FuzzyMatch {
        matches: true,
        score,
    }
}

/// Build the alphanumeric-swapped spelling of `query`, if it has one:
/// `abc123` becomes `123abc` and vice versa. Upstream does this with
/// `^([a-z]+)([0-9]+)$` / `^([0-9]+)([a-z]+)$` over the lowercased query,
/// so any other shape yields `None`.
fn swapped_query(query: &str) -> Option<String> {
    let chars: Vec<char> = query.chars().collect();
    let is_alpha = |c: char| c.is_ascii_lowercase();
    let is_digit = |c: char| c.is_ascii_digit();

    // letters then digits → digits then letters
    let mut i = 0;
    while i < chars.len() && is_alpha(chars[i]) {
        i += 1;
    }
    let letters_end = i;
    while i < chars.len() && is_digit(chars[i]) {
        i += 1;
    }
    if i == chars.len() && letters_end > 0 && i > letters_end {
        let letters: String = chars[..letters_end].iter().collect();
        let digits: String = chars[letters_end..].iter().collect();
        return Some(format!("{digits}{letters}"));
    }

    // digits then letters → letters then digits
    let mut i = 0;
    while i < chars.len() && is_digit(chars[i]) {
        i += 1;
    }
    let digits_end = i;
    while i < chars.len() && is_alpha(chars[i]) {
        i += 1;
    }
    if i == chars.len() && digits_end > 0 && i > digits_end {
        let digits: String = chars[..digits_end].iter().collect();
        let letters: String = chars[digits_end..].iter().collect();
        return Some(format!("{letters}{digits}"));
    }

    None
}

/// Match one token against `text`, upstream `fuzzyMatch`.
///
/// An empty query matches everything with score `0.0`. When the literal
/// spelling does not match, the alphanumeric-swapped spelling is tried and
/// its score is raised by `5.0` — see the module docs.
pub fn fuzzy_match(query: &str, text: &str) -> FuzzyMatch {
    let query_lower = query.to_lowercase();
    let text_lower = text.to_lowercase();
    let query_chars: Vec<char> = query_lower.chars().collect();
    let text_chars: Vec<char> = text_lower.chars().collect();

    let primary = score_token(&query_chars, &text_chars);
    if primary.matches {
        return primary;
    }

    let Some(swapped) = swapped_query(&query_lower) else {
        return primary;
    };
    let swapped_chars: Vec<char> = swapped.chars().collect();
    let swapped_match = score_token(&swapped_chars, &text_chars);
    if !swapped_match.matches {
        return primary;
    }

    FuzzyMatch {
        matches: true,
        score: swapped_match.score + 5.0,
    }
}

/// The whitespace- and slash-separated tokens of `query`, upstream
/// `query.trim().split(/[\s/]+/)`.
fn tokens(query: &str) -> Vec<&str> {
    query
        .split(|c: char| c.is_whitespace() || c == '/')
        .filter(|token| !token.is_empty())
        .collect()
}

/// Score a whole query (every token must match) against one `text`.
///
/// Returns `None` as soon as one token fails, otherwise the summed score
/// of the tokens. A query with no tokens (empty or only separators)
/// matches everything with score `0.0`.
pub fn fuzzy_match_all(query: &str, text: &str) -> Option<f64> {
    let mut total = 0.0;
    let mut matched_any = false;
    for token in tokens(query) {
        matched_any = true;
        let m = fuzzy_match(token, text);
        if !m.matches {
            return None;
        }
        total += m.score;
    }
    if !matched_any {
        return Some(0.0);
    }
    Some(total)
}

/// Rank `items` against `query`, best match first.
///
/// Returns the indices into `items` that match every query token, sorted
/// by score ascending. Ties keep their input order (the sort is stable),
/// which is what upstream's `Array.prototype.sort` does as well. An empty
/// or separator-only query returns every index in input order.
pub fn fuzzy_rank<T, S: AsRef<str>>(
    items: &[T],
    query: &str,
    get_text: impl Fn(&T) -> S,
) -> Vec<usize> {
    if tokens(query).is_empty() {
        return (0..items.len()).collect();
    }

    let mut scored: Vec<(usize, f64)> = Vec::new();
    for (idx, item) in items.iter().enumerate() {
        if let Some(score) = fuzzy_match_all(query, get_text(item).as_ref()) {
            scored.push((idx, score));
        }
    }
    scored.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    scored.into_iter().map(|(idx, _)| idx).collect()
}

/// Filter and sort `items` by fuzzy match quality, upstream
/// `fuzzyFilter`.
///
/// Equivalent to mapping [`fuzzy_rank`] back to references.
pub fn fuzzy_filter<'a, T, S: AsRef<str>>(
    items: &'a [T],
    query: &str,
    get_text: impl Fn(&T) -> S,
) -> Vec<&'a T> {
    fuzzy_rank(items, query, get_text)
        .into_iter()
        .map(|idx| &items[idx])
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // The cases below are ported 1:1 from upstream
    // `packages/tui/test/fuzzy.test.ts`.

    #[test]
    fn empty_query_matches_everything_with_score_zero() {
        let result = fuzzy_match("", "anything");
        assert!(result.matches);
        assert_eq!(result.score, 0.0);
    }

    #[test]
    fn query_longer_than_text_does_not_match() {
        assert!(!fuzzy_match("longquery", "short").matches);
    }

    #[test]
    fn exact_match_has_a_negative_score() {
        let result = fuzzy_match("test", "test");
        assert!(result.matches);
        assert!(result.score < 0.0, "score was {}", result.score);
    }

    #[test]
    fn characters_must_appear_in_order() {
        assert!(fuzzy_match("abc", "aXbXc").matches);
        assert!(!fuzzy_match("abc", "cba").matches);
    }

    #[test]
    fn matching_is_case_insensitive() {
        assert!(fuzzy_match("ABC", "abc").matches);
        assert!(fuzzy_match("abc", "ABC").matches);
    }

    #[test]
    fn consecutive_matches_score_better_than_scattered_ones() {
        let consecutive = fuzzy_match("foo", "foobar");
        let scattered = fuzzy_match("foo", "f_o_o_bar");
        assert!(consecutive.matches);
        assert!(scattered.matches);
        assert!(
            consecutive.score < scattered.score,
            "{} vs {}",
            consecutive.score,
            scattered.score
        );
    }

    #[test]
    fn word_boundary_matches_score_better() {
        let at_boundary = fuzzy_match("fb", "foo-bar");
        let not_at_boundary = fuzzy_match("fb", "afbx");
        assert!(at_boundary.matches);
        assert!(not_at_boundary.matches);
        assert!(
            at_boundary.score < not_at_boundary.score,
            "{} vs {}",
            at_boundary.score,
            not_at_boundary.score
        );
    }

    #[test]
    fn matches_swapped_alphanumeric_tokens() {
        assert!(fuzzy_match("codex52", "gpt-5.2-codex").matches);
    }

    #[test]
    fn a_swapped_match_costs_exactly_five_extra() {
        // `codex52` only matches through the swapped spelling `52codex`,
        // so its score is the raw score of that spelling plus the penalty.
        let text: Vec<char> = "gpt-5.2-codex".chars().collect();
        let swapped: Vec<char> = "52codex".chars().collect();
        let raw = score_token(&swapped, &text);
        let via_query = fuzzy_match("codex52", "gpt-5.2-codex");
        assert!(raw.matches && via_query.matches);
        assert_eq!(via_query.score, raw.score + 5.0);
    }

    #[test]
    fn empty_query_returns_all_items_unchanged() {
        let items = ["apple", "banana", "cherry"];
        let result = fuzzy_filter(&items, "", |x| *x);
        assert_eq!(result, vec![&"apple", &"banana", &"cherry"]);
    }

    #[test]
    fn filters_out_non_matching_items() {
        let items = ["apple", "banana", "cherry"];
        let result = fuzzy_filter(&items, "an", |x| *x);
        assert!(result.contains(&&"banana"));
        assert!(!result.contains(&&"apple"));
        assert!(!result.contains(&&"cherry"));
    }

    #[test]
    fn sorts_results_by_match_quality() {
        let items = ["a_p_p", "app", "application"];
        let result = fuzzy_filter(&items, "app", |x| *x);
        assert_eq!(result[0], &"app");
    }

    #[test]
    fn prioritises_exact_matches_over_longer_prefix_matches() {
        let items = ["clone", "cl"];
        let result = fuzzy_filter(&items, "cl", |x| *x);
        assert_eq!(result, vec![&"cl", &"clone"]);
    }

    #[test]
    fn works_with_a_custom_text_accessor() {
        let items = [("foo", 1), ("bar", 2), ("foobar", 3)];
        let result = fuzzy_filter(&items, "foo", |item| item.0);
        assert_eq!(result.len(), 2);
        assert!(result.iter().any(|item| item.0 == "foo"));
        assert!(result.iter().any(|item| item.0 == "foobar"));
    }

    #[test]
    fn matches_slash_separated_provider_model_queries() {
        let item = ("gpt-5.5", "openai-codex");
        let items = [item];
        let result = fuzzy_filter(&items, "openai-codex/gpt-5.5", |m| {
            format!("{} {}", m.0, m.1)
        });
        assert_eq!(result, vec![&item]);
    }

    #[test]
    fn every_token_must_match() {
        assert!(fuzzy_match_all("foo bar", "bar foo").is_some());
        assert!(fuzzy_match_all("foo zzz", "bar foo").is_none());
        // Only separators behaves like an empty query.
        assert_eq!(fuzzy_match_all("  /  ", "anything"), Some(0.0));
    }

    #[test]
    fn ranking_is_stable_for_equal_scores() {
        let items = ["model:a", "model:b", "model:c"];
        let ranked = fuzzy_rank(&items, "model:", |x| *x);
        assert_eq!(ranked, vec![0, 1, 2]);
    }
}
