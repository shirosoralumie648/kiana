//! `cache-retention.test.ts` 的 Rust 移植（共享层部分）。
//!
//! TS 测试通过捕获 provider 请求体断言 `cache_control` / `prompt_cache_retention` /
//! `prompt_cache_options` / `prompt_cache_key` 的形状。这些字段由各适配器构造；本单元负责
//! 的共享部分只有：
//!
//! 1. `SimpleStreamOptions.cacheRetention` 经 `buildBaseOptions` 原样透传（`none` / `short` /
//!    `long`，以及缺省 `undefined`）；
//! 2. `sessionId` 透传，且 `prompt_cache_key` 一律经
//!    [`clamp_openai_prompt_cache_key`]（≤ 64 个 Unicode 码点）。
//!
//! `KIANA_CACHE_RETENTION` 环境变量与各 provider 的 `supportsLongCacheRetention` 判定在
//! `anthropic-messages` / `openai-completions` / `openai-responses` / `bedrock` 适配器里。
//!
//! ## 跨单元待办（TS 的 19 个声明 / 32 个用例）
//!
//! 适配器级用例必须由拥有实现的单元移植，本文件不重复实现（它们需要真实的请求体捕获）：
//! - `api-anthropic-codex`（`anthropic-messages`）：7 条 —— `cache_control` 的
//!   `{type:"ephemeral"}` / `{type:"ephemeral",ttl:"1h"}`、`KIANA_CACHE_RETENTION=long`、
//!   非 `api.anthropic.com` baseUrl 默认 ttl、`supportsLongCacheRetention: false`（2 条
//!   需 `ANTHROPIC_API_KEY`，应为 `#[ignore]`）。
//! - `api-openai`（`openai-responses`，16 条）：`prompt_cache_options`（`{mode:"explicit"}` /
//!   带 ttl）与 `prompt_cache_retention` 在 6 + 4 个 it.each 模型列表上的矩阵、
//!   `none`/`short`/`long` 的省略规则。
//! - `api-openai`（`openai-completions`，9 条）：`prompt_cache_key` + `prompt_cache_retention`
//!   的 5 个 opencode / 2 个 cerebras it.each 矩阵。
//!
//! 合并时请核对上述单元的 `ownedPaths` 是否包含 `test/cache-retention.test.ts` 的移植。

use kiana_ai::api::openai_prompt_cache::{
    OPENAI_PROMPT_CACHE_KEY_MAX_LENGTH, clamp_openai_prompt_cache_key,
};
use kiana_ai::api::simple_options::build_base_options;
use kiana_ai::types::{
    CacheRetention, Context, Model, SimpleStreamOptions, StreamOptions, TranscriptContext,
    UserContent, UserMessage, UserRole,
};

fn context() -> TranscriptContext {
    TranscriptContext::from_normalized(Context {
        messages: vec![kiana_ai::types::Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Text("Hello".into()),
            timestamp: 1,
        })],
        ..Context::default()
    })
}

fn model() -> Model {
    serde_json::from_value(serde_json::json!({
        "id": "claude-haiku-4-5",
        "name": "Claude Haiku 4.5",
        "api": "anthropic-messages",
        "provider": "anthropic",
        "baseUrl": "https://api.anthropic.com",
        "reasoning": false,
        "input": ["text"],
        "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
        "contextWindow": 200000,
        "maxTokens": 8192
    }))
    .expect("model")
}

fn options(retention: Option<CacheRetention>, session_id: Option<&str>) -> SimpleStreamOptions {
    SimpleStreamOptions {
        base: StreamOptions {
            cache_retention: retention,
            session_id: session_id.map(str::to_string),
            ..StreamOptions::default()
        },
        ..SimpleStreamOptions::default()
    }
}

#[test]
fn build_base_options_passes_cache_retention_through() {
    assert_eq!(
        build_base_options(&model(), &context(), None, None).cache_retention,
        None
    );
    for (retention, expected) in [
        (CacheRetention::None, CacheRetention::None),
        (CacheRetention::Short, CacheRetention::Short),
        (CacheRetention::Long, CacheRetention::Long),
    ] {
        let options = options(Some(retention), None);
        assert_eq!(
            build_base_options(&model(), &context(), Some(&options), None).cache_retention,
            Some(expected)
        );
    }
}

#[test]
fn cache_retention_wire_values_match_typescript() {
    assert_eq!(
        serde_json::to_value(CacheRetention::None).unwrap(),
        serde_json::json!("none")
    );
    assert_eq!(
        serde_json::to_value(CacheRetention::Short).unwrap(),
        serde_json::json!("short")
    );
    assert_eq!(
        serde_json::to_value(CacheRetention::Long).unwrap(),
        serde_json::json!("long")
    );
    assert_eq!(
        serde_json::from_value::<CacheRetention>(serde_json::json!("long")).unwrap(),
        CacheRetention::Long
    );
}

#[test]
fn build_base_options_passes_session_id_through() {
    let options = options(None, Some("session-2"));
    let base = build_base_options(&model(), &context(), Some(&options), None);
    assert_eq!(base.session_id.as_deref(), Some("session-2"));
}

#[test]
fn prompt_cache_key_is_the_clamped_session_id() {
    // 适配器把 `clampOpenAIPromptCacheKey(sessionId)` 写进 `prompt_cache_key`。
    assert_eq!(clamp_openai_prompt_cache_key(None), None);
    assert_eq!(
        clamp_openai_prompt_cache_key(Some("session-2")),
        Some("session-2".to_string())
    );
    let long_session = "s".repeat(80);
    let clamped = clamp_openai_prompt_cache_key(Some(&long_session)).expect("clamped");
    assert_eq!(clamped.chars().count(), OPENAI_PROMPT_CACHE_KEY_MAX_LENGTH);
    assert_eq!(clamped, "s".repeat(64));
}

#[test]
fn cache_retention_is_none_by_default_for_models_without_prompt_cache_metadata() {
    // 缺省不写 `cache_retention` 字段：适配器据此决定 `cache_control` / `prompt_cache_retention`。
    let base = build_base_options(&model(), &context(), None, None);
    assert!(base.cache_retention.is_none());
}
