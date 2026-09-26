//! Integration tests for the fuzzy filter behind the searchable
//! [`Selector`].
//!
//! The matcher itself is unit-tested in `crates/pi-tui/src/fuzzy.rs`; these
//! cover the wiring the `/model` and `/resume` pickers rely on: a real
//! fuzzy query narrows and re-orders the list, tokens match across the
//! opaque `value`, the label and the description, and the rendered rows /
//! scroll indicator follow the ranked order.

use pi_tui::fuzzy::{fuzzy_filter, fuzzy_match};
use pi_tui::selector::{Selector, SelectorItem};

fn model_items() -> Vec<SelectorItem> {
    vec![
        SelectorItem::new("model:gpt-5", "GPT-5").with_description("openai"),
        SelectorItem::new("model:claude-sonnet-4-5", "Claude Sonnet 4.5")
            .with_description("anthropic"),
        SelectorItem::new("model:gemini-2.5-pro", "Gemini 2.5 Pro").with_description("google"),
        SelectorItem::new("model:deepseek-v4-pro", "DeepSeek V4 Pro").with_description("deepseek"),
    ]
}

#[test]
fn non_adjacent_query_characters_still_match() {
    // `gpt` is adjacent in `model:gpt-5`, while `gmp` skips characters in
    // `model:gemini-2.5-pro` — both are fuzzy matches.
    assert!(fuzzy_match("gpt", "model:gpt-5").matches);
    assert!(fuzzy_match("gmp", "model:gemini-2.5-pro").matches);
    assert!(!fuzzy_match("gmp", "model:deepseek-v4-pro").matches);
}

#[test]
fn the_selector_narrows_to_fuzzy_matches() {
    let mut sel = Selector::new("Pick a model", model_items());
    sel.set_filter("gmp");
    assert_eq!(sel.filtered_len(), 1);
    assert_eq!(sel.selected_value(), Some("model:gemini-2.5-pro"));
}

#[test]
fn the_selector_ranks_the_best_match_first() {
    let mut sel = Selector::new("Pick a model", model_items());
    // Two items contain `pro`: `gemini-2.5-pro` matches it as a run at a
    // word boundary, while `deepseek-v4-pro` first hits the `p` inside
    // `deepseek` and has to jump — so the ranking is by score, and here
    // it happens to invert the input order.
    sel.set_filter("pro");
    let ranked = sel
        .visible_items()
        .map(|item| item.value.as_str())
        .collect::<Vec<_>>();
    assert_eq!(ranked.len(), 2, "ranked {ranked:?}");
    assert_eq!(ranked[0], "model:gemini-2.5-pro", "ranked {ranked:?}");
    assert_eq!(ranked[1], "model:deepseek-v4-pro", "ranked {ranked:?}");
}

#[test]
fn tokens_match_value_label_and_description_independently() {
    let mut sel = Selector::new("Pick a model", model_items());

    // Provider token from the description + model token from the label.
    sel.set_filter("deepseek v4");
    assert_eq!(sel.filtered_len(), 1);
    assert_eq!(sel.selected_value(), Some("model:deepseek-v4-pro"));

    // Slash-separated provider/model query, as used by the session search.
    sel.set_filter("anthropic/claude");
    assert_eq!(sel.filtered_len(), 1);
    assert_eq!(sel.selected_value(), Some("model:claude-sonnet-4-5"));

    // A token that matches nothing empty the list, not the whole query.
    sel.set_filter("openai zzz");
    assert_eq!(sel.filtered_len(), 0);
    assert!(sel.selected_value().is_none());
}

#[test]
fn an_empty_query_restores_the_original_order() {
    let mut sel = Selector::new("Pick a model", model_items());
    sel.set_filter("gpt");
    assert_eq!(sel.filtered_len(), 1);
    sel.clear_filter();
    assert_eq!(
        sel.visible_items()
            .map(|item| item.value.as_str())
            .collect::<Vec<_>>(),
        vec![
            "model:gpt-5",
            "model:claude-sonnet-4-5",
            "model:gemini-2.5-pro",
            "model:deepseek-v4-pro",
        ],
    );
}

#[test]
fn equal_scores_keep_the_list_order() {
    // `model:` is an equal-quality match for every item (the opaque
    // prefix is identical), so ranking must be stable.
    let mut sel = Selector::new("Pick a model", model_items());
    sel.set_filter("model:");
    assert_eq!(sel.filtered_len(), 4);
    assert_eq!(sel.selected_value(), Some("model:gpt-5"));
    sel.next();
    assert_eq!(sel.selected_value(), Some("model:claude-sonnet-4-5"));
}

#[test]
fn the_free_function_matches_the_selector_ordering() {
    let items = model_items();
    let ranked = fuzzy_filter(&items, "pro", |item| item.search_text());
    let via_selector = {
        let mut sel = Selector::new("Pick a model", items.clone());
        sel.set_filter("pro");
        sel.visible_items().cloned().collect::<Vec<_>>()
    };
    assert_eq!(ranked, via_selector.iter().collect::<Vec<_>>());
}

#[test]
fn rendered_rows_follow_the_ranked_order() {
    let mut sel = Selector::new("Pick a model", model_items()).with_max_visible(10);
    sel.set_filter("gmp");
    let lines = sel.render_lines(60);
    let joined = lines.join("\n");
    assert!(joined.contains("→ Gemini 2.5 Pro"), "rows were {joined:?}");
    // The other rows are filtered out entirely.
    assert_eq!(sel.filtered_len(), 1);
    assert!(sel.render_lines(60).len() < 5);
}
