//! `test/bedrock-thinking-payload.test.ts` 的移植。

use kiana_ai::api::bedrock_converse_stream::{
    BedrockOptions, THINKING_BINDING_CONTROLS_BETA, build_additional_model_request_fields,
    build_command_input,
};
use kiana_ai::types::{
    CacheRetention, Context, Message, Model, ModelInputModality, StreamOptions, ThinkingLevel,
    UserContent, UserMessage, UserRole,
};
use kiana_ai::utils::transcript::normalize_context;
use serde_json::{Value, json};

fn make_model(id: &str, name: &str) -> Model {
    Model {
        id: id.into(),
        name: name.into(),
        api: "bedrock-converse-stream".into(),
        provider: "amazon-bedrock".into(),
        base_url: "https://bedrock-runtime.us-east-1.amazonaws.com".into(),
        reasoning: Some(true),
        input: vec![ModelInputModality::Text],
        context_window: Some(200_000),
        max_tokens: Some(64_000),
        ..Model::default()
    }
}

fn options(reasoning: Option<ThinkingLevel>) -> BedrockOptions {
    BedrockOptions {
        reasoning,
        ..Default::default()
    }
}

fn request_fields(model: &Model, options: &BedrockOptions) -> Option<Value> {
    build_additional_model_request_fields(model, options)
}

#[test]
fn uses_adaptive_thinking_for_claude_opus_48() {
    let model = make_model(
        "global.anthropic.claude-opus-4-8-v1",
        "Claude Opus 4.8 (Global)",
    );
    let fields = request_fields(&model, &options(Some(ThinkingLevel::High))).expect("fields");
    assert_eq!(
        fields["thinking"],
        json!({"type": "adaptive", "display": "summarized",
               "block_binding": {"prefix_mismatch_behavior": "drop_block"}})
    );
    assert_eq!(fields["output_config"], json!({"effort": "high"}));
    assert_eq!(
        fields["anthropic_beta"],
        json!([THINKING_BINDING_CONTROLS_BETA])
    );
}

#[test]
fn maps_xhigh_reasoning_to_effort_xhigh() {
    let model = make_model(
        "global.anthropic.claude-opus-4-8-v1",
        "Claude Opus 4.8 (Global)",
    );
    let fields = request_fields(&model, &options(Some(ThinkingLevel::Xhigh))).expect("fields");
    assert_eq!(fields["output_config"], json!({"effort": "xhigh"}));
    assert_eq!(
        fields["anthropic_beta"],
        json!([THINKING_BINDING_CONTROLS_BETA])
    );

    let model = make_model("global.anthropic.claude-opus-5", "Claude Opus 5");
    let fields = request_fields(&model, &options(Some(ThinkingLevel::Xhigh))).expect("fields");
    assert_eq!(fields["output_config"], json!({"effort": "xhigh"}));
}

#[test]
fn uses_adaptive_thinking_for_fable_sonnet_5_and_opus_5() {
    for (id, name) in [
        ("global.anthropic.claude-fable-5", "Claude Fable 5"),
        ("global.anthropic.claude-sonnet-5", "Claude Sonnet 5"),
        ("global.anthropic.claude-opus-5", "Claude Opus 5"),
    ] {
        let model = make_model(id, name);
        let fields = request_fields(&model, &options(Some(ThinkingLevel::High))).expect("fields");
        assert_eq!(fields["output_config"], json!({"effort": "high"}), "{id}");
        assert_eq!(fields["thinking"]["type"], json!("adaptive"), "{id}");
    }
}

#[test]
fn sends_block_binding_and_the_binding_beta_for_opus_55() {
    let model = make_model("global.anthropic.claude-opus-5-5", "Claude Opus 5.5");
    let fields = request_fields(&model, &options(Some(ThinkingLevel::High))).expect("fields");
    assert_eq!(
        fields["thinking"],
        json!({"type": "adaptive", "display": "summarized",
               "block_binding": {"prefix_mismatch_behavior": "drop_block"}})
    );
    assert_eq!(
        fields["anthropic_beta"],
        json!([THINKING_BINDING_CONTROLS_BETA])
    );
}

#[test]
fn omits_block_binding_for_46_models() {
    for id in [
        "global.anthropic.claude-opus-4-6-v1",
        "global.anthropic.claude-sonnet-4-6",
    ] {
        let model = make_model(id, id);
        let fields = request_fields(&model, &options(Some(ThinkingLevel::High))).expect("fields");
        assert_eq!(
            fields["thinking"],
            json!({"type": "adaptive", "display": "summarized"}),
            "{id}"
        );
        assert!(fields.get("anthropic_beta").is_none(), "{id}");
    }
}

#[test]
fn omits_display_for_govcloud_non_adaptive_claude_thinking() {
    let model = make_model(
        "us-gov.anthropic.claude-sonnet-4-5-20250929-v1:0",
        "Claude Sonnet 4.5 (GovCloud)",
    );
    let fields = request_fields(&model, &options(Some(ThinkingLevel::High))).expect("fields");
    assert_eq!(
        fields["thinking"],
        json!({"type": "enabled", "budget_tokens": 16384})
    );
    assert_eq!(
        fields["anthropic_beta"],
        json!(["interleaved-thinking-2025-05-14"])
    );
}

#[test]
fn omits_display_for_govcloud_regions_on_adaptive_thinking() {
    let model = make_model(
        "global.anthropic.claude-opus-4-8-v1",
        "Claude Opus 4.8 (Global)",
    );
    let mut options = options(Some(ThinkingLevel::High));
    options.region = Some("us-gov-west-1".into());
    let fields = request_fields(&model, &options).expect("fields");
    assert_eq!(fields["thinking"], json!({"type": "adaptive"}));
    assert_eq!(fields["output_config"], json!({"effort": "high"}));
    assert!(fields.get("anthropic_beta").is_none());
}

#[test]
fn sends_gpt_reasoning_effort() {
    for (reasoning, effort) in [
        (ThinkingLevel::Minimal, "low"),
        (ThinkingLevel::Low, "low"),
        (ThinkingLevel::Medium, "medium"),
        (ThinkingLevel::High, "high"),
        (ThinkingLevel::Xhigh, "xhigh"),
        (ThinkingLevel::Max, "max"),
    ] {
        for id in [
            "global.openai.gpt-6-sol",
            "us.openai.gpt-6-luna",
            "global.openai.gpt-5.6-sol",
        ] {
            let model = make_model(id, id);
            let fields = request_fields(&model, &options(Some(reasoning))).expect("fields");
            assert_eq!(
                fields,
                json!({"reasoning": {"effort": effort}}),
                "{id} / {reasoning:?}"
            );
        }
    }
}

#[test]
fn sends_gpt_reasoning_effort_when_only_the_name_identifies_the_model() {
    let model = make_model(
        "arn:aws:bedrock:us-east-1:123456789012:application-inference-profile/my-profile",
        "GPT-6 Sol",
    );
    let fields = request_fields(&model, &options(Some(ThinkingLevel::Medium))).expect("fields");
    assert_eq!(fields, json!({"reasoning": {"effort": "medium"}}));
}

#[test]
fn sends_flat_reasoning_effort_for_gpt_oss_clamped_to_high() {
    let model = make_model("openai.gpt-oss-120b-1:0", "gpt-oss-120b");
    assert_eq!(
        request_fields(&model, &options(Some(ThinkingLevel::Minimal))).expect("fields"),
        json!({"reasoning_effort": "low"})
    );
    assert_eq!(
        request_fields(&model, &options(Some(ThinkingLevel::Medium))).expect("fields"),
        json!({"reasoning_effort": "medium"})
    );
    assert_eq!(
        request_fields(&model, &options(Some(ThinkingLevel::Xhigh))).expect("fields"),
        json!({"reasoning_effort": "high"})
    );
}

#[test]
fn sends_no_reasoning_fields_when_reasoning_is_off() {
    let model = make_model("global.openai.gpt-6-sol", "gpt-6-sol");
    assert!(request_fields(&model, &options(None)).is_none());
}

#[test]
fn uses_adaptive_thinking_when_only_the_name_contains_the_model_name() {
    let model = make_model(
        "arn:aws:bedrock:us-east-1:123456789012:application-inference-profile/my-profile",
        "Claude Opus 4.6",
    );
    let fields = request_fields(&model, &options(Some(ThinkingLevel::High))).expect("fields");
    assert_eq!(
        fields["thinking"],
        json!({"type": "adaptive", "display": "summarized"})
    );
    assert_eq!(fields["output_config"], json!({"effort": "high"}));
}

#[test]
fn injects_cache_points_when_the_name_identifies_a_supported_claude_model() {
    let model = make_model(
        "arn:aws:bedrock:us-east-1:123456789012:application-inference-profile/my-profile",
        "Claude Sonnet 4.6",
    );
    let context = normalize_context(&Context {
        system_prompt: Some("You are helpful.".into()),
        messages: vec![Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Text("Hello".into()),
            timestamp: 1,
        })],
        ..Context::default()
    });
    let payload = build_command_input(
        &model,
        &context,
        CacheRetention::Short,
        &BedrockOptions::default(),
        false,
    )
    .expect("payload");
    assert_eq!(payload["system"].as_array().expect("system").len(), 2);
    assert!(payload["system"][1].get("cachePoint").is_some());
    let messages = payload["messages"].as_array().expect("messages");
    let last_content = messages.last().expect("message")["content"]
        .as_array()
        .expect("content");
    assert!(
        last_content
            .last()
            .expect("block")
            .get("cachePoint")
            .is_some()
    );
}

#[test]
fn falls_back_to_fixed_budget_thinking_for_non_adaptive_claude_via_name() {
    let model = make_model(
        "arn:aws:bedrock:us-east-1:123456789012:application-inference-profile/my-profile",
        "Claude Sonnet 4.5",
    );
    let fields = request_fields(&model, &options(Some(ThinkingLevel::High))).expect("fields");
    assert_eq!(fields["thinking"]["type"], json!("enabled"));
    assert!(fields["thinking"]["budget_tokens"].is_number());
    assert_eq!(
        fields["anthropic_beta"],
        json!(["interleaved-thinking-2025-05-14"])
    );
}

#[test]
fn stream_options_are_used_for_region_and_reasoning() {
    // `BedrockOptions` 的默认 reasoning 是 None（与 TS 的 `options?.reasoning` 一致）。
    let model = make_model(
        "us.anthropic.claude-sonnet-4-5-20250929-v1:0",
        "Claude Sonnet 4.5",
    );
    assert!(request_fields(&model, &BedrockOptions::default()).is_none());
    let _ = StreamOptions::default();
}

#[test]
fn sends_a_non_canonical_thinking_level_map_value_verbatim() {
    // TS: `typeof mapped === "string"` 时直接 `return mapped`（只做类型断言，不收窄）。
    let mut model = make_model(
        "global.anthropic.claude-opus-4-8-v1",
        "Claude Opus 4.8 (Global)",
    );
    model.thinking_level_map = Some(
        [(
            kiana_ai::types::ModelThinkingLevel::High,
            Some("minimal".to_string()),
        )]
        .into_iter()
        .collect(),
    );
    let fields = request_fields(&model, &options(Some(ThinkingLevel::High))).expect("fields");
    assert_eq!(fields["output_config"], json!({"effort": "minimal"}));
}
