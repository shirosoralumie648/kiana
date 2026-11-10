//! `test/model-search.test.ts` 的 Rust 等价物。

use kiana_interactive::model_search::{
    get_distinct_model_name, get_model_search_text, get_model_selector_search_text,
};
use kiana_tui::fuzzy_filter;

struct Item {
    provider: &'static str,
    id: &'static str,
    name: Option<&'static str>,
}

fn item(provider: &'static str, id: &'static str, name: Option<&'static str>) -> Item {
    Item { provider, id, name }
}

#[test]
fn hides_names_that_restate_the_id() {
    assert_eq!(
        get_distinct_model_name("claude-sonnet-4-5", Some("Claude Sonnet 4.5")),
        None
    );
    assert_eq!(
        get_distinct_model_name("openai/gpt-oss-120b", Some("OpenAI: gpt-oss-120b")),
        None
    );
    assert_eq!(
        get_distinct_model_name(
            "eu.anthropic.claude-sonnet-4-5-20250929-v1:0",
            Some("Claude Sonnet 4.5 (EU)")
        ),
        None
    );
    assert_eq!(get_distinct_model_name("gpt-5", Some("")), None);
    assert_eq!(get_distinct_model_name("gpt-5", None), None);
}

#[test]
fn shows_names_with_words_the_id_lacks() {
    let name = "MiniMax M3 [rasmus@openrouter, parasail]";
    assert_eq!(
        get_distinct_model_name("minimax/minimax-m3", Some(name)),
        Some(name.to_string())
    );
    assert_eq!(
        get_distinct_model_name("gemini-3.1-flash-lite-image", Some("Nano Banana 2 Lite")),
        Some("Nano Banana 2 Lite".to_string())
    );
}

#[test]
fn requires_single_character_words_to_match_a_whole_id_token() {
    assert_eq!(
        get_distinct_model_name("mistral-small-2603", Some("Mistral Small 4")),
        Some("Mistral Small 4".to_string())
    );
}

#[test]
fn search_text_uses_the_ts_formulas() {
    assert_eq!(
        get_model_search_text("gpt-5", "openai", Some("GPT-5")),
        "gpt-5 openai openai/gpt-5 openai gpt-5 GPT-5"
    );
    assert_eq!(
        get_model_selector_search_text("gpt-5", "openai", Some("GPT-5")),
        "openai openai/gpt-5 openai gpt-5 GPT-5"
    );
}

// #11220
#[test]
fn ranks_models_containing_the_query_above_scattered_matches() {
    let models = vec![
        item(
            "openrouter",
            "openai/gpt-oss-120b",
            Some("OpenAI: gpt-oss-120b"),
        ),
        item(
            "openrouter",
            "anthropic/claude-sonnet-4.5",
            Some("Anthropic: Claude Sonnet 4.5"),
        ),
        item(
            "openrouter",
            "anthropic/claude-opus-4.1",
            Some("Anthropic: Claude Opus 4.1"),
        ),
        item("anthropic", "claude-sonnet-4-5", Some("Claude Sonnet 4.5")),
        item("anthropic", "claude-opus-4-1", Some("Claude Opus 4.1")),
    ];

    let result: Vec<String> = fuzzy_filter(models, "opus", |item| {
        get_model_selector_search_text(item.id, item.provider, item.name)
    })
    .into_iter()
    .map(|item| item.id.to_string())
    .collect();

    let mut top: Vec<String> = result.into_iter().take(2).collect();
    top.sort();
    assert_eq!(top, vec!["anthropic/claude-opus-4.1", "claude-opus-4-1"]);
}

// #10065
#[test]
fn ranks_the_typed_model_above_models_sharing_a_long_provider_prefix() {
    let models = vec![
        item(
            "fireworks",
            "accounts/fireworks/models/deepseek-v4-flash-0731",
            None,
        ),
        item("fireworks", "accounts/fireworks/models/kimi-k3", None),
        item(
            "fireworks",
            "accounts/fireworks/routers/firerouter",
            Some("FireRouter"),
        ),
    ];

    let result = fuzzy_filter(models, "firerouter", |item| {
        get_model_selector_search_text(item.id, item.provider, item.name)
    });

    assert_eq!(
        result.first().map(|item| item.id),
        Some("accounts/fireworks/routers/firerouter")
    );
}
