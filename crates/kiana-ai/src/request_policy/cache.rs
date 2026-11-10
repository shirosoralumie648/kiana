//! Prompt cache 语义与断点注入。
//!
//! 缓存行为由 `(provider, model)` 决定，而不是由「请求恰好走哪个 wire format」决定：
//!
//! - [`CacheSemantics::ExplicitBreakpoints`]：调用方必须显式放置断点（Anthropic 方言）；
//!   只有标记过的字节前缀可复用；
//! - [`CacheSemantics::ImplicitStrict`]：provider 只复用从开头逐字节一致的前缀；
//! - [`CacheSemantics::ImplicitTolerant`]：provider 会复用最长匹配前缀（允许中间有差异）；
//! - [`CacheSemantics::Uncached`]：没有已知的 prompt cache。
//!
//! 这张语义表决定两件事：
//!
//! 1. 是否向 payload 注入 `cache_control` 断点——只有 `ExplicitBreakpoints` 注入；
//! 2. 是否具备 cache-warmer 资格——`Uncached` 没有资格，其余语义还要求模型目录声明了
//!    非零的 `promptCache` TTL（没有已知寿命就无法安排预热）。
//!
//! 默认值取 `ImplicitStrict`：对任何缓存实现都安全（不会因为多放了断点被 provider 拒绝）。
//!
//! 设计参考 goose `goose-provider-types/src/cache_semantics.rs`（枚举、`for_model` 映射、
//! OpenRouter/Anthropic 断点放置与 20 block lookback）；Kiana 用 `serde_json::Value`
//! 重写，并裁剪为「语义 + 断点注入 + warmer 资格」三件事。

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::types::{Model, PromptCacheTier};

/// prompt cache 语义。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CacheSemantics {
    /// 显式断点；最多允许的断点数由 provider 决定（Anthropic 为 4）。
    ExplicitBreakpoints {
        /// 单次请求允许的最大断点数。
        max_breakpoints: usize,
    },
    /// 只复用从头逐字节一致的前缀。
    ImplicitStrict,
    /// 复用最长匹配前缀。
    ImplicitTolerant,
    /// 无已知缓存。
    Uncached,
}

impl CacheSemantics {
    /// 已知 `(provider, model)` 的语义；未知组合回落到 `ImplicitStrict`（安全默认）。
    ///
    /// provider id 与 `kiana-ai` 的内置目录一致（`anthropic`、`openai`、`openrouter`、
    /// `kimi-coding`、`zai`……）。模型名判断只做保守的子串匹配，不试图枚举全部模型。
    pub fn for_model(provider: &str, model: &str) -> Self {
        let provider = provider.trim().to_ascii_lowercase();
        let model = model.trim().to_ascii_lowercase();
        match provider.as_str() {
            // Anthropic 方言显式断点。
            "anthropic" | "minimax" | "zai" | "kimi-coding" => {
                CacheSemantics::ExplicitBreakpoints { max_breakpoints: 4 }
            }
            // Bedrock/Vertex/Databricks 上的 Claude 同样走 Anthropic 断点。
            "amazon-bedrock" | "bedrock" | "google-vertex" | "google-vertex-ai" | "databricks"
                if model.contains("claude") =>
            {
                CacheSemantics::ExplicitBreakpoints { max_breakpoints: 4 }
            }
            // OpenRouter/LiteLLM 中转的 Anthropic 模型保留断点语义。
            "openrouter" | "litellm" if model.starts_with("anthropic/") => {
                CacheSemantics::ExplicitBreakpoints { max_breakpoints: 4 }
            }
            // OpenAI Responses 是严格前缀；Chat Completions 允许最长前缀。
            "openai" | "azure" | "azure-openai" | "github-copilot" => {
                if is_openai_responses_model(&model) {
                    CacheSemantics::ImplicitStrict
                } else {
                    CacheSemantics::ImplicitTolerant
                }
            }
            // 明确观察过「最长前缀」行为的兼容服务。
            "moonshotai" | "moonshotai-cn" | "deepseek" | "groq" | "together" | "fireworks"
            | "mistral" | "xai" | "zhipu" | "alibaba" | "qwen" => CacheSemantics::ImplicitTolerant,
            // 无缓存。
            "snowflake" | "sagemaker" => CacheSemantics::Uncached,
            _ => CacheSemantics::ImplicitStrict,
        }
    }

    /// 是否使用显式断点。
    pub fn uses_explicit_breakpoints(self) -> bool {
        matches!(self, CacheSemantics::ExplicitBreakpoints { .. })
    }

    /// 单次请求的最大断点数；非显式语义为 0。
    pub fn max_breakpoints(self) -> usize {
        match self {
            CacheSemantics::ExplicitBreakpoints { max_breakpoints } => max_breakpoints,
            _ => 0,
        }
    }

    /// 是否有任何缓存。
    pub fn is_cached(self) -> bool {
        !matches!(self, CacheSemantics::Uncached)
    }
}

/// 是否为 OpenAI Responses 系列（严格前缀缓存）。
fn is_openai_responses_model(model: &str) -> bool {
    model.contains("gpt-5")
        || model.contains("gpt-6")
        || model.contains("o3")
        || model.contains("o4")
        || model.contains("codex")
}

/// 模型目录里的 `promptCache` 是否声明了可用寿命（任一分档 > 0）。
pub fn has_known_prompt_cache_lifetime(model: &Model) -> bool {
    model.prompt_cache.as_ref().is_some_and(|tiers| {
        tiers
            .values()
            .any(|seconds| seconds.is_finite() && *seconds > 0.0)
    })
}

/// 该模型是否有 cache-warmer 资格。
///
/// 条件：语义有缓存且模型目录声明了非零 TTL。显式断点语义并不要求 warmer——Anthropic
/// 习惯上由请求自身放断点——但计划把 cache warmer 作为统一机制时仍要求已知寿命。
pub fn cache_warmer_eligible(model: &Model, semantics: CacheSemantics) -> bool {
    semantics.is_cached() && has_known_prompt_cache_lifetime(model)
}

/// 模型某个缓存档位的寿命（秒）；未声明返回 `None`。
pub fn prompt_cache_lifetime_seconds(model: &Model, tier: PromptCacheTier) -> Option<f64> {
    model
        .prompt_cache
        .as_ref()
        .and_then(|tiers| tiers.get(&tier))
        .copied()
        .filter(|seconds| seconds.is_finite() && *seconds > 0.0)
}

/// Anthropic 的 lookback 窗口（以 content block 计）。
const LOOKBACK_BLOCKS: usize = 20;

/// 按 [`CacheSemantics`] 决定是否注入断点；返回注入的断点数。
///
/// `ExplicitBreakpoints` 会修改 payload（`messages`/`tools`），其它语义完全不碰 payload，
/// 返回 0。断点放置规则：
///
/// 1. 最后一条「可缓存」的消息的最后一个 content block；
/// 2. 倒数约 [`LOOKBACK_BLOCKS`] 个 block 处再放一个锚点（防止并行工具调用一次追加太多
///    block 后，下一次请求的尾部锚点落在 lookback 窗口之外）；
/// 3. system 消息的最后一个 block；
/// 4. tools 数组最后一项的 `function.cache_control`。
///
/// 空文本、空 tool result、thinking/reasoning 块不打断点（Anthropic 会拒绝空文本上的
/// `cache_control`）。
pub fn apply_cache_breakpoints(payload: &mut Value, semantics: CacheSemantics) -> usize {
    if !semantics.uses_explicit_breakpoints() {
        return 0;
    }
    let mut injected = 0;
    let max = semantics.max_breakpoints();
    let mut marked: std::collections::BTreeSet<usize> = std::collections::BTreeSet::new();

    if let Some(messages) = payload.get_mut("messages").and_then(Value::as_array_mut) {
        let offsets = cumulative_block_offsets(messages);
        if let Some(primary) = (0..messages.len())
            .rev()
            .find(|&index| has_cacheable_content(&messages[index]))
        {
            if max == 0 || injected < max {
                mark_last_content_block(&mut messages[primary]);
                marked.insert(primary);
                injected += 1;
            }
            let target = offsets[primary].saturating_sub(LOOKBACK_BLOCKS);
            if let Some(secondary) = (0..primary)
                .rev()
                .find(|&index| offsets[index] <= target && has_cacheable_content(&messages[index]))
                && (max == 0 || injected < max)
            {
                mark_last_content_block(&mut messages[secondary]);
                marked.insert(secondary);
                injected += 1;
            }
        }
        if let Some(index) = messages
            .iter()
            .position(|message| message.get("role") == Some(&json!("system")))
            && !marked.contains(&index)
            && (max == 0 || injected < max)
        {
            let system = &mut messages[index];
            if has_cacheable_content(system) {
                mark_last_content_block(system);
                marked.insert(index);
                injected += 1;
            }
        }
    }

    if let Some(tools) = payload.get_mut("tools").and_then(Value::as_array_mut)
        && (max == 0 || injected < max)
        && let Some(last_tool) = tools.last_mut()
        && let Some(function) = last_tool.get_mut("function").and_then(Value::as_object_mut)
    {
        function.insert("cache_control".to_string(), json!({ "type": "ephemeral" }));
        injected += 1;
    }

    injected
}

/// 每条消息的累计 content block 数（到该消息末尾为止）。
fn cumulative_block_offsets(messages: &[Value]) -> Vec<usize> {
    let mut total = 0;
    messages
        .iter()
        .map(|message| {
            total += match message.get("content") {
                Some(Value::Array(blocks)) => blocks.len(),
                Some(Value::String(_)) => 1,
                _ => 0,
            };
            total += message
                .get("tool_calls")
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            if message.get("reasoning_content").is_some() {
                total += 1;
            }
            total
        })
        .collect()
}

/// 消息的最后一个 content block 是否能承载 `cache_control`。
fn has_cacheable_content(message: &Value) -> bool {
    match message.get("content") {
        Some(Value::String(text)) => !text.trim().is_empty(),
        Some(Value::Array(blocks)) => {
            blocks
                .last()
                .and_then(Value::as_object)
                .is_some_and(|block| match block.get("type").and_then(Value::as_str) {
                    Some("text") => block
                        .get("text")
                        .and_then(Value::as_str)
                        .is_some_and(|text| !text.trim().is_empty()),
                    Some("image_url") | Some("image") => true,
                    _ => false,
                })
        }
        _ => false,
    }
}

/// 给最后一个 content block 打上 `cache_control`（不可缓存时 no-op）。
fn mark_last_content_block(message: &mut Value) {
    if !has_cacheable_content(message) {
        return;
    }
    let Some(content) = message.get_mut("content") else {
        return;
    };
    if let Some(text) = content.as_str() {
        *content = json!([{
            "type": "text",
            "text": text,
            "cache_control": { "type": "ephemeral" }
        }]);
        return;
    }
    if let Some(block) = content
        .as_array_mut()
        .and_then(|blocks| blocks.last_mut())
        .and_then(Value::as_object_mut)
    {
        block.insert("cache_control".to_string(), json!({ "type": "ephemeral" }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ModelPromptCache;
    use indexmap::IndexMap;

    fn model(prompt_cache: Option<ModelPromptCache>) -> Model {
        Model {
            id: "m".into(),
            provider: "anthropic".into(),
            prompt_cache,
            ..Model::default()
        }
    }

    #[test]
    fn resolves_registered_identifiers_and_defaults_to_strict() {
        let explicit = CacheSemantics::ExplicitBreakpoints { max_breakpoints: 4 };
        for (provider, model, expected) in [
            ("anthropic", "claude-sonnet-4-5", explicit),
            ("kimi-coding", "kimi-for-coding", explicit),
            ("amazon-bedrock", "anthropic.claude-sonnet-4-5", explicit),
            ("minimax", "MiniMax-M2.5", explicit),
            ("openrouter", "anthropic/claude-sonnet-4-5", explicit),
            (
                "openrouter",
                "openai/gpt-5.2",
                CacheSemantics::ImplicitStrict,
            ),
            ("openai", "gpt-5.2", CacheSemantics::ImplicitStrict),
            ("openai", "gpt-4.1-mini", CacheSemantics::ImplicitTolerant),
            ("snowflake", "llama-3.3-70b", CacheSemantics::Uncached),
            (
                "some-new-vendor",
                "some-model",
                CacheSemantics::ImplicitStrict,
            ),
        ] {
            assert_eq!(
                CacheSemantics::for_model(provider, model),
                expected,
                "{provider}/{model}"
            );
        }
    }

    #[test]
    fn explicit_breakpoints_cover_system_tools_and_the_tail_message() {
        let mut payload = json!({
            "model": "anthropic/claude-sonnet-4-5",
            "messages": [
                {"role": "system", "content": "be careful"},
                {"role": "user", "content": "first"},
                {"role": "assistant", "content": "ok"},
                {"role": "user", "content": "second"},
                {"role": "assistant", "content": "ok"},
                {"role": "user", "content": "third"}
            ],
            "tools": [
                {"type": "function", "function": {"name": "a"}},
                {"type": "function", "function": {"name": "b"}}
            ]
        });
        let injected = apply_cache_breakpoints(
            &mut payload,
            CacheSemantics::ExplicitBreakpoints { max_breakpoints: 4 },
        );
        assert_eq!(injected, 3);
        let messages = payload["messages"].as_array().unwrap();
        assert_eq!(
            messages[0]["content"][0]["cache_control"]["type"],
            "ephemeral"
        );
        assert!(messages[1]["content"].is_string());
        assert!(messages[3]["content"].is_string());
        assert_eq!(
            messages[5]["content"][0]["cache_control"]["type"],
            "ephemeral"
        );
        let tools = payload["tools"].as_array().unwrap();
        assert!(tools[0]["function"].get("cache_control").is_none());
        assert_eq!(tools[1]["function"]["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn implicit_tolerant_does_not_inject_breakpoints() {
        let payload = json!({
            "messages": [{"role": "system", "content": "be careful"}],
            "tools": [{"type": "function", "function": {"name": "a"}}]
        });
        let mut untouched = payload.clone();
        let injected = apply_cache_breakpoints(&mut untouched, CacheSemantics::ImplicitTolerant);
        assert_eq!(injected, 0);
        assert_eq!(untouched, payload);
    }

    #[test]
    fn unmarkable_tail_falls_back_to_the_last_markable_message() {
        let mut payload = json!({
            "messages": [
                {"role": "tool", "content": "result", "tool_call_id": "call_0"},
                {"role": "assistant", "content": null, "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {"name": "noop", "arguments": "{}"}
                }]},
                {"role": "tool", "content": "", "tool_call_id": "call_1"}
            ]
        });
        apply_cache_breakpoints(
            &mut payload,
            CacheSemantics::ExplicitBreakpoints { max_breakpoints: 4 },
        );
        let messages = payload["messages"].as_array().unwrap();
        assert_eq!(messages[2]["content"], json!(""));
        assert_eq!(
            messages[0]["content"][0]["cache_control"]["type"],
            "ephemeral"
        );
        assert!(messages[1].get("cache_control").is_none());
    }

    #[test]
    fn secondary_anchor_trails_the_tail_and_respects_the_breakpoint_budget() {
        let mut messages = vec![json!({"role": "system", "content": "be careful"})];
        for index in 0..25 {
            messages.push(json!({
                "role": "assistant",
                "content": null,
                "reasoning_content": format!("thinking {index}"),
                "tool_calls": [{
                    "id": format!("call_{index}"),
                    "type": "function",
                    "function": {"name": "read_file", "arguments": "{}"}
                }]
            }));
            messages.push(json!({
                "role": "tool",
                "content": format!("result {index}"),
                "tool_call_id": format!("call_{index}")
            }));
        }
        let mut payload = json!({
            "messages": messages,
            "tools": [{"type": "function", "function": {"name": "read_file"}}]
        });
        let injected = apply_cache_breakpoints(
            &mut payload,
            CacheSemantics::ExplicitBreakpoints { max_breakpoints: 4 },
        );
        // 1 system + 2 anchors + 1 tool = 4。
        assert_eq!(injected, 4);
        assert_eq!(payload.to_string().matches("cache_control").count(), 4);
    }

    #[test]
    fn zero_max_breakpoints_means_unlimited_injection() {
        let mut payload = json!({
            "messages": [{"role": "system", "content": "be careful"}],
            "tools": [{"type": "function", "function": {"name": "a"}}]
        });
        let injected = apply_cache_breakpoints(
            &mut payload,
            CacheSemantics::ExplicitBreakpoints { max_breakpoints: 0 },
        );
        assert_eq!(injected, 2);
    }

    #[test]
    fn array_content_user_messages_get_a_breakpoint() {
        let mut payload = json!({
            "messages": [
                {"role": "user", "content": [
                    {"type": "text", "text": "look at this"},
                    {"type": "image_url", "image_url": {"url": "data:image/png;base64,xyz"}}
                ]}
            ]
        });
        apply_cache_breakpoints(
            &mut payload,
            CacheSemantics::ExplicitBreakpoints { max_breakpoints: 4 },
        );
        let blocks = payload["messages"][0]["content"].as_array().unwrap();
        assert!(blocks[0].get("cache_control").is_none());
        assert_eq!(blocks[1]["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn warmer_requires_a_known_positive_lifetime_and_a_cache() {
        let mut tiers = IndexMap::new();
        tiers.insert(PromptCacheTier::Short, 300.0);
        let cached_model = model(Some(tiers));
        assert!(cache_warmer_eligible(
            &cached_model,
            CacheSemantics::ExplicitBreakpoints { max_breakpoints: 4 }
        ));
        assert!(!cache_warmer_eligible(
            &cached_model,
            CacheSemantics::Uncached
        ));
        assert!(!cache_warmer_eligible(
            &model(None),
            CacheSemantics::ImplicitStrict
        ));

        let mut zero = IndexMap::new();
        zero.insert(PromptCacheTier::Short, 0.0);
        assert!(!cache_warmer_eligible(
            &model(Some(zero)),
            CacheSemantics::ImplicitStrict
        ));
    }

    #[test]
    fn prompt_cache_lifetime_filters_non_positive_values() {
        let mut tiers = IndexMap::new();
        tiers.insert(PromptCacheTier::Long, 3_600.0);
        let model = model(Some(tiers));
        assert_eq!(
            prompt_cache_lifetime_seconds(&model, PromptCacheTier::Long),
            Some(3_600.0)
        );
        assert_eq!(
            prompt_cache_lifetime_seconds(&model, PromptCacheTier::Short),
            None
        );
    }
}
