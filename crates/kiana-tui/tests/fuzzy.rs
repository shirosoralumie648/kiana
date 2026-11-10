//! `test/fuzzy.test.ts`：模糊匹配打分与过滤排序。
//!
//! TS 源：`packages/tui/test/fuzzy.test.ts`（17 个 `it`，标题保留在函数名中）。
//! 分数是固定的逐位数值，任何实现都必须完全一致，因此这里用 `assert_eq!` 断言精确值。

use kiana_tui::fuzzy::{fuzzy_filter, fuzzy_match};

// ---------------------------------------------------------------------------
// describe("fuzzyMatch")
// ---------------------------------------------------------------------------

#[test]
fn empty_query_matches_everything_with_score_0() {
    let result = fuzzy_match("", "anything");
    assert!(result.matches);
    assert_eq!(result.score, 0.0);
}

#[test]
fn query_longer_than_text_does_not_match() {
    let result = fuzzy_match("longquery", "short");
    assert!(!result.matches);
}

#[test]
fn exact_match_has_good_score() {
    let result = fuzzy_match("test", "test");
    assert!(result.matches);
    assert!(result.score < 0.0); // 连续命中加分为负
    assert_eq!(result.score, -159.4);
}

#[test]
fn characters_must_appear_in_order() {
    let match_in_order = fuzzy_match("abc", "aXbXc");
    assert!(match_in_order.matches);

    let match_out_of_order = fuzzy_match("abc", "cba");
    assert!(!match_out_of_order.matches);
}

#[test]
fn case_insensitive_matching() {
    assert!(fuzzy_match("ABC", "abc").matches);
    assert!(fuzzy_match("abc", "ABC").matches);
}

#[test]
fn consecutive_matches_score_better_than_scattered_matches() {
    let consecutive = fuzzy_match("foo", "foobar");
    let scattered = fuzzy_match("foo", "f_o_o_bar");

    assert!(consecutive.matches);
    assert!(scattered.matches);
    assert!(consecutive.score < scattered.score);
    assert_eq!(consecutive.score, -39.699999999999996);
    assert_eq!(scattered.score, -30.400000000000002);
}

#[test]
fn word_boundary_matches_score_better() {
    let at_boundary = fuzzy_match("fb", "foo-bar");
    let not_at_boundary = fuzzy_match("fb", "afbx");

    assert!(at_boundary.matches);
    assert!(not_at_boundary.matches);
    assert!(at_boundary.score < not_at_boundary.score);
    assert_eq!(at_boundary.score, -18.6);
    assert_eq!(not_at_boundary.score, -4.7);
}

// #11220: 贪心对齐从 "openrouter" 取走了 o、p、u，导致真正的 "opus" 反而最差
#[test]
fn scores_a_contiguous_occurrence_instead_of_an_earlier_scattered_alignment() {
    let opus = fuzzy_match("opus", "openrouter openrouter/anthropic/claude-opus-4.1");
    let sonnet = fuzzy_match("opus", "openrouter openrouter/anthropic/claude-sonnet-4.5");
    let gpt_oss = fuzzy_match("opus", "openrouter openrouter/openai/gpt-oss-120b");

    assert!(opus.score < sonnet.score);
    assert!(opus.score < gpt_oss.score);
    assert_eq!(opus.score, -23.8);
    assert_eq!(sonnet.score, 41.6);
    assert_eq!(gpt_oss.score, 41.1);
}

#[test]
fn uses_the_best_contiguous_occurrence_not_the_first() {
    let embedded_first = fuzzy_match("opus", "xxopus opus");
    let boundary_only = fuzzy_match("opus", "xxxxxx opus");

    assert_eq!(embedded_first.score, boundary_only.score);
    assert_eq!(embedded_first.score, -36.6);
}

#[test]
fn matches_swapped_alpha_numeric_tokens() {
    let result = fuzzy_match("codex52", "gpt-5.2-codex");
    assert!(result.matches);
    assert_eq!(result.score, -64.99999999999999);
}

// ---------------------------------------------------------------------------
// describe("fuzzyFilter")
// ---------------------------------------------------------------------------

#[test]
fn empty_query_returns_all_items_unchanged() {
    let items = vec!["apple", "banana", "cherry"];
    let result = fuzzy_filter(items.clone(), "", |x| (*x).to_string());
    assert_eq!(result, items);
}

#[test]
fn filters_out_non_matching_items() {
    let items = vec!["apple", "banana", "cherry"];
    let result = fuzzy_filter(items, "an", |x| (*x).to_string());
    assert!(result.contains(&"banana"));
    assert!(!result.contains(&"apple"));
    assert!(!result.contains(&"cherry"));
}

#[test]
fn sorts_results_by_match_quality() {
    let items = vec!["a_p_p", "app", "application"];
    let result = fuzzy_filter(items, "app", |x| (*x).to_string());

    // "app" 应排第一（开头处的精确连续命中）
    assert_eq!(result[0], "app");
    // TS 实测顺序（"application" 的连续命中比 "a_p_p" 的分散命中更靠前）
    assert_eq!(result, vec!["app", "application", "a_p_p"]);
}

#[test]
fn prioritizes_exact_matches_over_longer_prefix_matches() {
    let items = vec!["clone", "cl"];
    let result = fuzzy_filter(items, "cl", |x| (*x).to_string());

    assert_eq!(result, vec!["cl", "clone"]);
}

#[test]
fn works_with_custom_get_text_function() {
    let items = vec![("foo", 1), ("bar", 2), ("foobar", 3)];
    let result = fuzzy_filter(items, "foo", |item| item.0.to_string());

    assert_eq!(result.len(), 2);
    assert!(result.iter().map(|r| r.0).any(|name| name == "foo"));
    assert!(result.iter().map(|r| r.0).any(|name| name == "foobar"));
}

// #11220: "radius radius/gpt" 里按顺序也能取出 a、s、t、r、a，于是每个 radius 模型都在那里命中
#[test]
fn ranks_a_model_containing_the_query_above_models_matching_a_shared_provider_prefix() {
    let items = vec![
        "radius radius/gpt-5.4 radius gpt-5.4 GPT 5.4",
        "radius radius/agentic radius agentic Agentic",
        "radius radius/gpt-6-sol radius gpt-6-sol GPT 6 Sol",
        "radius radius/gpt-6-astra radius gpt-6-astra GPT 6 Astra",
    ];
    let result = fuzzy_filter(items.clone(), "astra", |x| (*x).to_string());

    assert_eq!(result[0], items[3]);
}

#[test]
fn matches_slash_separated_provider_model_queries_against_reordered_text() {
    let item = ("gpt-5.5", "openai-codex");
    let result = fuzzy_filter(vec![item], "openai-codex/gpt-5.5", |model| {
        format!("{} {}", model.0, model.1)
    });

    assert_eq!(result, vec![item]);
}

// ---------------------------------------------------------------------------
// 回归：`scorePositions` 的 `lastMatchIndex` 从 `-1` 起步
// ---------------------------------------------------------------------------

/// TS 里 `lastMatchIndex` 初值是 `-1`，所以 `i === 0` 也满足 `lastMatchIndex === i - 1`，
/// **首个命中也拿连续命中加分**。曾经把它写成 `Option<usize>`（首个命中不加分），
/// 导致所有分数整体偏移 5。
#[test]
fn first_match_at_index_zero_still_earns_the_consecutive_bonus() {
    // 单字符在开头：-5（连续） -10（词边界） + 0（位置）= -15
    assert_eq!(fuzzy_match("t", "test").score, -15.0);
    // 同一字符出现在中间：没有连续加分，位置惩罚 0.1
    assert_eq!(fuzzy_match("e", "test").score, 0.1);
    // "test" 全串相等再 -100
    assert_eq!(fuzzy_match("test", "test").score, -159.4);
}

/// UTF-16 码元索引：BMP 之外的字符占两个码元，且不同字符的高位代理相同、低位不同。
#[test]
fn uses_utf16_code_unit_indices_for_supplementary_characters() {
    // 位置从 0 开始计"码元"：🙂 占 0、1 两个码元
    let with_emoji = fuzzy_match("a", "🙂a");
    let without = fuzzy_match("a", "xxa");
    assert_eq!(with_emoji.score, without.score);
    // 查询里的代理码元只能匹配同一个字符（不会把 🙂 的高位代理当成 😀 的低位代理）
    assert!(!fuzzy_match("🙂", "😀").matches);
}
