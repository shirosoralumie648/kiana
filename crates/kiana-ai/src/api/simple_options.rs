//! api/simple-options.ts。
//!
//! `streamSimple` 的共享选项构造：上下文窗口钳制、采样参数合并、思考预算。
//!
//! TS: `packages/ai/src/api/simple-options.ts`。
//!
//! # 这个文件解决什么问题
//! 调用方只提供一份「简单选项」[`SimpleStreamOptions`]，而各适配器需要的是已经算好的
//! [`StreamOptions`]：`max_tokens` 要按上下文窗口收缩、采样参数要按「模型默认 < 思考档位
//! < 请求」三级合并、`api_key` 要按优先级回落。把这些纯计算集中在此，适配器只需负责
//! 各家协议的字段名与请求体形状。
//!
//! # 主要流程
//! 1. [`build_base_options`] 逐字段复制基础选项，并调用 [`resolve_sampling_params`] 与
//!    [`clamp_max_tokens_to_context`] 得到最终的 `sampling_params` / `max_tokens`；
//! 2. [`adjust_max_tokens_for_thinking`] 供「思考预算与回答共享同一个 `max_tokens`」的
//!    适配器（Anthropic、Bedrock Claude）使用：先按预算扩大上限，再把预算夹到
//!    「上限减 [`MIN_ANSWER_TOKENS`]」；
//! 3. [`thinking_budget_for_level`] / [`clamp_thinking_budget_to_answer_room`] 负责档位到
//!    token 数的映射与给回答预留空间。
//!
//! # 约束与失败模式
//! - 本文件的函数都是纯函数：只读参数，不读环境、不发 IO，主要行为由单元测试与
//!   `tests/fixtures/api-shared/simple_options.json` 的黄金值锁定。
//! - [`build_base_options`] 会把 `options.base.extra` 丢弃（`StreamOptions.extra` 恒为空
//!   map）；`SimpleStreamOptions` 的 `tool_choice`/`reasoning`/`deferred`/
//!   `thinking_budgets` 也不在 [`StreamOptions`] 中，适配器必须自己读取原始选项。
//! - 上下文窗口缺省或为 0 时不做钳制，结果至少为 1；窗口有效时钳制只压上界，调用方
//!   显式传入的 0 会保留为 0（见 [`clamp_max_tokens_to_context`] 的边界说明）。
//! - 预算函数用 `expect` 编码「默认预算覆盖 minimal/low/medium/high 四档」这一不变量；
//!   只有 [`DEFAULT_THINKING_BUDGETS`] 被改成缺档位时才会 panic。

use crate::models::clamp_thinking_level;
use crate::types::{
    Model, ModelThinkingLevel, SamplingParams, SimpleStreamOptions, StreamOptions, ThinkingBudgets,
    ThinkingLevel, TranscriptContext,
};
use crate::utils::estimate::estimate_context_tokens;

/// `CONTEXT_SAFETY_TOKENS`。
const CONTEXT_SAFETY_TOKENS: u64 = 4096;
/// `MIN_MAX_TOKENS`。
const MIN_MAX_TOKENS: u64 = 1;

/// `clampMaxTokensToContext(model, context, maxTokens)`。
///
/// `model.contextWindow` 缺省（TS 类型上 chat 模型必有）按 0 处理，等价于「不按上下文钳制」。
///
/// # 计算
/// `available = contextWindow - estimate_context_tokens(context).tokens - CONTEXT_SAFETY_TOKENS`，
/// 再取 `min(maxTokens, max(1, available))`。4096 的安全余量是单独扣除的固定值，不包含在
/// `estimate_context_tokens` 的估算结果里。
///
/// # 边界
/// - 窗口缺省或为 0：直接返回 `max(1, maxTokens)`，连上下文估算都不做。
/// - 上下文估算已超过窗口：`available` 为负，下界被抬到 1；最终仍受 `maxTokens` 上界
///   约束，所以 `maxTokens = 0` 时结果是 0。
/// - 窗口有效时，参数 `maxTokens = 0` 会原样返回 0：钳制只压上界，0 是合法的显式值。
///   （窗口无效时 0 会被抬成 1。）两组行为分别由测试 `never_returns_zero` 与 fixture 的
///   `zeroMax` 用例锁定。
///
/// TS: `src/api/simple-options.ts#clampMaxTokensToContext`
pub fn clamp_max_tokens_to_context(
    model: &Model,
    context: &TranscriptContext,
    max_tokens: u64,
) -> u64 {
    let Some(context_window) = model.context_window else {
        return max_tokens.max(MIN_MAX_TOKENS);
    };
    // TS 的 `model.contextWindow <= 0`；Rust 的 `contextWindow` 是 u64，负数不可表达。
    if context_window == 0 {
        return max_tokens.max(MIN_MAX_TOKENS);
    }
    let used = estimate_context_tokens(context).tokens;
    // 用 i128 计算：窗口很小或估算值很大时 available 会为负，u64 会下溢。
    let available = (context_window as i128) - (used as i128) - (CONTEXT_SAFETY_TOKENS as i128);
    let floored = available.max(MIN_MAX_TOKENS as i128) as u64;
    max_tokens.min(floored)
}

/// `ThinkingLevel`（不含 off）→ `ModelThinkingLevel`。
fn to_model_thinking_level(level: ThinkingLevel) -> ModelThinkingLevel {
    match level {
        ThinkingLevel::Minimal => ModelThinkingLevel::Minimal,
        ThinkingLevel::Low => ModelThinkingLevel::Low,
        ThinkingLevel::Medium => ModelThinkingLevel::Medium,
        ThinkingLevel::High => ModelThinkingLevel::High,
        ThinkingLevel::Xhigh => ModelThinkingLevel::Xhigh,
        ThinkingLevel::Max => ModelThinkingLevel::Max,
    }
}

/// `resolveSamplingParams(model, thinkingLevel, requestParams?)`。
///
/// 覆盖顺序：模型默认 < 思考级别 < 请求；三者都未设置时返回 `None`（调用方不得写字段）。
///
/// # 合并细节
/// 先对 `thinking_level` 调用 [`crate::models::clamp_thinking_level`]：模型不支持的档位按
/// 「先向更高、再向更低」的规则就近映射。`Off` 是否有专属参数表取决于模型的
/// `sampling_params_by_thinking_level`；没有该键时只得到模型默认值。
/// 合并是键级的，后来的来源覆盖同名键的值，但键的位置保持第一次出现的位置——这与 JS
/// 对象展开（`{...a, ...b}`）的可观察结果一致。
///
/// # 返回值的边界
/// `None` 仅当三个来源都缺省。如果某个来源存在但是空 map，返回的仍是 `Some(空 map)`；
/// 调用方不能用「map 是否为空」代替「是否为 `None`」做判断——TS 侧 `{}` 是真值，仍会走
/// 合并分支（最终不产生任何键）。
///
/// TS: `src/api/simple-options.ts#resolveSamplingParams`
pub fn resolve_sampling_params(
    model: &Model,
    thinking_level: ModelThinkingLevel,
    request_params: Option<&SamplingParams>,
) -> Option<SamplingParams> {
    let effective = clamp_thinking_level(model, thinking_level);
    let level_params = model
        .sampling_params_by_thinking_level
        .as_ref()
        .and_then(|by_level| by_level.get(&effective));
    if model.sampling_params.is_none() && level_params.is_none() && request_params.is_none() {
        return None;
    }
    let mut merged = SamplingParams::new();
    // 展开顺序即优先级：后者覆盖同名键的值，键位置保持首次插入（与 JS 对象展开一致）。
    if let Some(params) = &model.sampling_params {
        merged.extend(params.iter().map(|(k, v)| (k.clone(), v.clone())));
    }
    if let Some(params) = level_params {
        merged.extend(params.iter().map(|(k, v)| (k.clone(), v.clone())));
    }
    if let Some(params) = request_params {
        merged.extend(params.iter().map(|(k, v)| (k.clone(), v.clone())));
    }
    Some(merged)
}

/// `buildBaseOptions(model, context, options?, apiKey?)`。
///
/// 逐字段复制 `SimpleStreamOptions`（`extra` 不透传：TS 的对象字面量同样只列具名字段）。
/// `apiKey` 参数优先，空串按 JS `||` 语义回落到 `options.apiKey`。
///
/// # 字段来源
/// 除 `api_key`、`extra` 与以下两项外，其余字段直接取自 `options.base`：
/// - `api_key`：函数参数优先（空串按 JS `||` 语义视为未给），否则回落到
///   `options.base.api_key`；
/// - `extra`：恒为 `Default::default()`（空 map），不从 `options.base` 复制；
/// - `sampling_params`：由 [`resolve_sampling_params`] 按模型默认、思考档位、请求三级合并；
/// - `max_tokens`：先取 `options.base.max_tokens ?? model.max_tokens`（`None` 才回落，
///   `Some(0)` 保留），再交给 [`clamp_max_tokens_to_context`]。
///
/// `extra` 恒为空 map；适配器在调用之后按需往 `base.extra` 插入 API 专属键（例如
/// Anthropic 的 `toolChoice`）。`tool_choice`、`reasoning`、`deferred`、
/// `thinking_budgets` 不属于 [`StreamOptions`]，本函数不会复制。
///
/// # 为什么 `max_tokens` 可以是 `None`
/// TS 在请求与模型都未给上限时把 `undefined` 传进钳制函数，得到 `NaN`，最终
/// `JSON.stringify` 写成 `null`。Rust 的 `Option<u64>` 用 `None` 表示同一个线上形状；
/// 不能改写成 `Some(0)`——0 是一个合法的显式上限，会改变请求含义。
///
/// # 错误
/// 无。上下文估算不会失败；缺失字段保持 `None`，由适配器决定是否写进请求体。
///
/// TS: `src/api/simple-options.ts#buildBaseOptions`
pub fn build_base_options(
    model: &Model,
    context: &TranscriptContext,
    options: Option<&SimpleStreamOptions>,
    api_key: Option<&str>,
) -> StreamOptions {
    let base = options.map(|options| &options.base);
    let thinking_level = options
        .and_then(|options| options.reasoning)
        .map(to_model_thinking_level)
        .unwrap_or(ModelThinkingLevel::Off);
    let sampling_params = resolve_sampling_params(
        model,
        thinking_level,
        options.and_then(|options| options.base.sampling_params.as_ref()),
    );
    // TS: `clampMaxTokensToContext(model, context, options?.maxTokens ?? model.maxTokens)`。
    // 两者都缺省时 TS 把 `undefined` 传进去，钳制结果与 `Math.min` 都是 `NaN`
    // （`JSON.stringify(NaN)` 是 `null`）。冻结的 [`StreamOptions::max_tokens`] 是
    // `Option<u64>`，无法表示 `NaN`：`None` 是唯一会序列化成 `null` 的表示，也保留
    // “调用方没有给出数值上限”的语义（不能用 0 —— 那是一个有效的上限值）。
    let max_tokens = options
        .and_then(|options| options.base.max_tokens)
        .or(model.max_tokens)
        .map(|requested| clamp_max_tokens_to_context(model, context, requested));
    let api_key = match api_key.filter(|key| !key.is_empty()) {
        Some(key) => Some(key.to_string()),
        None => base.and_then(|base| base.api_key.clone()),
    };

    StreamOptions {
        api_key,
        env: base.and_then(|base| base.env.clone()),
        headers: base.and_then(|base| base.headers.clone()),
        identity: base.and_then(|base| base.identity.clone()),
        timeout_ms: base.and_then(|base| base.timeout_ms),
        max_retries: base.and_then(|base| base.max_retries),
        max_retry_delay_ms: base.and_then(|base| base.max_retry_delay_ms),
        temperature: base.and_then(|base| base.temperature),
        sampling_params,
        max_tokens,
        transport: base.and_then(|base| base.transport),
        cache_retention: base.and_then(|base| base.cache_retention),
        session_id: base.and_then(|base| base.session_id.clone()),
        websocket_connect_timeout_ms: base.and_then(|base| base.websocket_connect_timeout_ms),
        metadata: base.and_then(|base| base.metadata.clone()),
        extra: Default::default(),
        signal: base.and_then(|base| base.signal.clone()),
        on_payload: base.and_then(|base| base.on_payload.clone()),
        on_payload_async: base.and_then(|base| base.on_payload_async.clone()),
        on_response: base.and_then(|base| base.on_response.clone()),
        on_provider_stream_event: base.and_then(|base| base.on_provider_stream_event.clone()),
        fetch: base.and_then(|base| base.fetch.clone()),
        telemetry: base.and_then(|base| base.telemetry.clone()),
    }
}

/// Tokens always left for the answer when a thinking budget shares the response ceiling.
///
/// 思考预算与 `max_tokens` 共用同一个上限时，至少留给最终回答的 token 数。与
/// [`DEFAULT_THINKING_BUDGETS`] 一样，该值来自 TS 参考实现，并被 fixture 的
/// `constants.minAnswerTokens` 锁定；`anthropic_messages` 里另有一处硬编码 1024 的同值
/// 逻辑（TS 原文如此，不是本单元的遗漏）。
///
/// TS: `src/api/simple-options.ts#MIN_ANSWER_TOKENS`
pub const MIN_ANSWER_TOKENS: u64 = 1024;

/// `DEFAULT_THINKING_BUDGETS`。
///
/// 每个思考档位的默认预算（token），只有 minimal/low/medium/high 四档。`xhigh`/`max`
/// 经 [`clamp_reasoning`] 归一到 `high`，因此没有独立默认值；调用方传入的自定义
/// [`ThinkingBudgets`] 按档位覆盖这里的值，未覆盖的档位回落到本常量。
///
/// TS: `src/api/simple-options.ts#DEFAULT_THINKING_BUDGETS`
pub const DEFAULT_THINKING_BUDGETS: ThinkingBudgets = ThinkingBudgets {
    minimal: Some(1024),
    low: Some(2048),
    medium: Some(8192),
    high: Some(16384),
};

/// `clampReasoning(effort)`：`xhigh` / `max` 归一到 `high`。
///
/// 这是 TS 参考实现的既定归一规则：[`DEFAULT_THINKING_BUDGETS`] 只定义四档，`xhigh` 与
/// `max` 复用 `high` 的预算。除这两档外原样返回，`None` 仍是 `None`（不会凭空产生档位）。
///
/// TS: `src/api/simple-options.ts#clampReasoning`
pub fn clamp_reasoning(effort: Option<ThinkingLevel>) -> Option<ThinkingLevel> {
    match effort {
        Some(ThinkingLevel::Xhigh) | Some(ThinkingLevel::Max) => Some(ThinkingLevel::High),
        other => other,
    }
}

/// `thinkingBudgetForLevel(reasoningLevel, customBudgets?)`。
///
/// `customBudgets` 里未设置的档位回落到 [`DEFAULT_THINKING_BUDGETS`]（TS 的
/// `{...DEFAULT_THINKING_BUDGETS, ...customBudgets}`）。
///
/// # 不变量与 panic
/// 返回 `custom[档位] ?? default[档位]`。两处 `expect` 把「默认表必须覆盖全部档位」和
/// 「输入一定是已定义的 `ThinkingLevel`」显式化：`ThinkingLevel` 没有 `Off` 变体，所以
/// 不存在「无预算」分支；`DEFAULT_THINKING_BUDGETS` 四档都是 `Some`，正常运行不会 panic，
/// 只有把该常量改成缺档位才会触发。
///
/// TS: `src/api/simple-options.ts#thinkingBudgetForLevel`
pub fn thinking_budget_for_level(
    reasoning_level: ThinkingLevel,
    custom_budgets: Option<&ThinkingBudgets>,
) -> u64 {
    let level = clamp_reasoning(Some(reasoning_level))
        .expect("clamp_reasoning keeps a defined thinking level defined");
    let custom = custom_budgets.cloned().unwrap_or_default();
    let pick = |custom: Option<u64>, default: Option<u64>| {
        custom
            .or(default)
            .expect("default thinking budgets define every level")
    };
    match level {
        ThinkingLevel::Minimal => pick(custom.minimal, DEFAULT_THINKING_BUDGETS.minimal),
        ThinkingLevel::Low => pick(custom.low, DEFAULT_THINKING_BUDGETS.low),
        ThinkingLevel::Medium => pick(custom.medium, DEFAULT_THINKING_BUDGETS.medium),
        ThinkingLevel::High => pick(custom.high, DEFAULT_THINKING_BUDGETS.high),
        // `clamp_reasoning` 已把 xhigh / max 归一为 high。
        ThinkingLevel::Xhigh | ThinkingLevel::Max => {
            pick(custom.high, DEFAULT_THINKING_BUDGETS.high)
        }
    }
}

/// `clampThinkingBudgetToAnswerRoom(thinkingBudget, ceiling)`。
///
/// 语义是 `min(thinking_budget, max(0, ceiling - MIN_ANSWER_TOKENS))`。递减使用
/// `saturating_sub`，因此 `ceiling < 1024` 时返回 0 而不是下溢 panic；返回 0 表示「没有
/// 空间留给思考」，调用方通常据此省略预算字段（见 `openai_completions` 的
/// `resolve_clamped_thinking_budget`）。
///
/// TS: `src/api/simple-options.ts#clampThinkingBudgetToAnswerRoom`
pub fn clamp_thinking_budget_to_answer_room(thinking_budget: u64, ceiling: u64) -> u64 {
    thinking_budget.min(ceiling.saturating_sub(MIN_ANSWER_TOKENS))
}

/// `adjustMaxTokensForThinking(...)` 的返回值（TS 的 `{ maxTokens, thinkingBudget }`）。
///
/// 两个字段都是最终值：调用方应直接使用，不要再把 `thinking_budget` 加到 `max_tokens`
/// 上，也不要反向扣减一次。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdjustMaxTokensForThinking {
    /// 最终的 `max_tokens`。
    ///
    /// 调用方给了基础上限时已加上思考预算，并被模型上限截断。
    pub max_tokens: u64,
    /// 最终的思考预算。
    ///
    /// `max_tokens <= 预算` 时已夹到 `max_tokens - MIN_ANSWER_TOKENS`（不小于 0）。
    pub thinking_budget: u64,
}

/// `adjustMaxTokensForThinking(baseMaxTokens, modelMaxTokens, reasoningLevel, customBudgets?)`。
///
/// `base_max_tokens == None` 表示调用方未设上限：使用模型上限，不再额外加预算。
///
/// # 计算步骤
/// 1. `thinking_budget = thinking_budget_for_level(level, custom)`（`xhigh`/`max` 在内部
///    归一为 `high`）；
/// 2. `base_max_tokens` 为 `None` 时 `max_tokens = model_max_tokens`；否则
///    `max_tokens = min(base + budget, model_max_tokens)`（饱和加法，避免 `u64` 溢出）；
/// 3. 若 `max_tokens <= thinking_budget`（预算会吃掉全部输出），把预算夹到
///    `max_tokens - MIN_ANSWER_TOKENS`（不小于 0，可能为 0）。
///
/// # 为什么 `None` 不追加预算
/// 按 TS 参考实现的设计：`base_max_tokens` 缺省表示调用方没有设上限，此时不再额外加预算，
/// 避免思考预算吞掉全部 `max_tokens`。测试 `adjusts_max_tokens_for_thinking` 用
/// `base = None, modelMax = 1000` 锁定了「预算归 0」的结果。
///
/// # 边界与前提
/// `model_max_tokens` 是必填 `u64`；调用方常用 `model.max_tokens.unwrap_or(0)` 表达模型
/// 未声明上限，此时若 `base_max_tokens` 也是 `None`，结果就是 0，需要后续上下文钳制或
/// 上层逻辑兜底。本函数不做 `MIN_MAX_TOKENS` 之类的兜底。
///
/// TS: `src/api/simple-options.ts#adjustMaxTokensForThinking`
pub fn adjust_max_tokens_for_thinking(
    base_max_tokens: Option<u64>,
    model_max_tokens: u64,
    reasoning_level: ThinkingLevel,
    custom_budgets: Option<&ThinkingBudgets>,
) -> AdjustMaxTokensForThinking {
    let mut thinking_budget = thinking_budget_for_level(reasoning_level, custom_budgets);
    let max_tokens = match base_max_tokens {
        None => model_max_tokens,
        Some(base) => base.saturating_add(thinking_budget).min(model_max_tokens),
    };
    // 预算会吃掉全部输出：把预算夹到「上限减回答保留量」，夹取后可能为 0。
    if max_tokens <= thinking_budget {
        thinking_budget = clamp_thinking_budget_to_answer_room(thinking_budget, max_tokens);
    }
    AdjustMaxTokensForThinking {
        max_tokens,
        thinking_budget,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Context, Message, UserContent, UserMessage, UserRole};

    fn model(context_window: Option<u64>, max_tokens: Option<u64>) -> Model {
        Model {
            id: "test-model".into(),
            name: "Test".into(),
            api: "openai-completions".into(),
            provider: "test".into(),
            base_url: "https://example.invalid".into(),
            input: vec![crate::types::ModelInputModality::Text],
            context_window,
            max_tokens,
            ..Model::default()
        }
    }

    fn context_with_text(text: &str) -> TranscriptContext {
        TranscriptContext(Context {
            messages: vec![Message::User(UserMessage {
                role: UserRole::User,
                content: UserContent::Text(text.into()),
                timestamp: 0,
            })],
            ..Context::default()
        })
    }

    #[test]
    fn clamps_to_context_window_minus_safety() {
        // 128k 窗口、4k 安全余量、短上下文 → maxTokens 原样。
        assert_eq!(
            clamp_max_tokens_to_context(
                &model(Some(128_000), Some(16_000)),
                &context_with_text("hi"),
                8_000
            ),
            8_000
        );
        // 窗口只有 10k，上下文约 1 token → available = 10000 - 1 - 4096 = 5903。
        assert_eq!(
            clamp_max_tokens_to_context(
                &model(Some(10_000), Some(16_000)),
                &context_with_text("hi"),
                8_000
            ),
            5_903
        );
    }

    #[test]
    fn never_returns_zero() {
        assert_eq!(
            clamp_max_tokens_to_context(&model(Some(10), Some(1)), &context_with_text("hi"), 8_000),
            1
        );
        assert_eq!(
            clamp_max_tokens_to_context(&model(Some(0), Some(1)), &context_with_text("hi"), 0),
            1
        );
        assert_eq!(
            clamp_max_tokens_to_context(&model(None, Some(1)), &context_with_text("hi"), 0),
            1
        );
    }

    /// `options.maxTokens` 与 `model.maxTokens` 都缺省：TS 的 `undefined` 经
    /// `clampMaxTokensToContext` 变成 `NaN`（序列化为 `null`）。Rust 的冻结类型是
    /// `Option<u64>`，因此用 `None` 表示同一件事（`Some(0)` / `Some(1)` 都是错误的值）。
    #[test]
    fn missing_max_tokens_stays_unset_instead_of_becoming_zero_or_one() {
        let context = context_with_text("hi");
        for model in [
            model(Some(128_000), None),
            model(None, None),
            model(Some(0), None),
        ] {
            let base = build_base_options(&model, &context, None, None);
            assert_eq!(base.max_tokens, None);
            assert_eq!(
                serde_json::to_value(base.max_tokens).unwrap(),
                serde_json::Value::Null
            );
        }

        // 显式 `maxTokens: 0`（`??` 保留 0）仍然经过钳制，不会被当成缺省。
        let options = SimpleStreamOptions {
            base: StreamOptions {
                max_tokens: Some(0),
                ..StreamOptions::default()
            },
            ..SimpleStreamOptions::default()
        };
        let base = build_base_options(&model(Some(128_000), None), &context, Some(&options), None);
        assert_eq!(base.max_tokens, Some(0));
    }

    #[test]
    fn clamp_reasoning_maps_xhigh_and_max_to_high() {
        assert_eq!(
            clamp_reasoning(Some(ThinkingLevel::Xhigh)),
            Some(ThinkingLevel::High)
        );
        assert_eq!(
            clamp_reasoning(Some(ThinkingLevel::Max)),
            Some(ThinkingLevel::High)
        );
        assert_eq!(
            clamp_reasoning(Some(ThinkingLevel::Low)),
            Some(ThinkingLevel::Low)
        );
        assert_eq!(clamp_reasoning(None), None);
    }

    #[test]
    fn thinking_budget_uses_defaults_and_overrides() {
        assert_eq!(
            thinking_budget_for_level(ThinkingLevel::Minimal, None),
            1024
        );
        assert_eq!(thinking_budget_for_level(ThinkingLevel::High, None), 16384);
        assert_eq!(thinking_budget_for_level(ThinkingLevel::Max, None), 16384);
        let custom = ThinkingBudgets {
            low: Some(7),
            ..ThinkingBudgets::default()
        };
        assert_eq!(
            thinking_budget_for_level(ThinkingLevel::Low, Some(&custom)),
            7
        );
        assert_eq!(
            thinking_budget_for_level(ThinkingLevel::Minimal, Some(&custom)),
            1024
        );
    }

    #[test]
    fn adjusts_max_tokens_for_thinking() {
        let adjusted =
            adjust_max_tokens_for_thinking(Some(1000), 32_000, ThinkingLevel::Medium, None);
        assert_eq!(adjusted.thinking_budget, 8192);
        assert_eq!(adjusted.max_tokens, 9192);

        // 未设调用方上限：不加预算。
        let adjusted = adjust_max_tokens_for_thinking(None, 32_000, ThinkingLevel::Medium, None);
        assert_eq!(adjusted.thinking_budget, 8192);
        assert_eq!(adjusted.max_tokens, 32_000);

        // 上限足够时预算保持原值（maxTokens > thinkingBudget）。
        let adjusted =
            adjust_max_tokens_for_thinking(Some(100), 32_000, ThinkingLevel::Medium, None);
        assert_eq!(adjusted.max_tokens, 8_292);
        assert_eq!(adjusted.thinking_budget, 8_192);

        // 上限不高于预算时，给答案留出 MIN_ANSWER_TOKENS。
        let adjusted = adjust_max_tokens_for_thinking(None, 8_192, ThinkingLevel::Medium, None);
        assert_eq!(adjusted.max_tokens, 8_192);
        assert_eq!(adjusted.thinking_budget, 8_192 - MIN_ANSWER_TOKENS);

        let adjusted = adjust_max_tokens_for_thinking(None, 1000, ThinkingLevel::Medium, None);
        assert_eq!(adjusted.max_tokens, 1000);
        assert_eq!(adjusted.thinking_budget, 0);
    }
}

#[cfg(test)]
mod fixtures {
    //! 与 TS 参考实现（`/home/ctyun/kiana-work/upstream-reference`）逐字段对照的黄金测试。

    use super::*;
    use crate::types::{
        CacheRetention, ClientIdentity, Context, Message, ProviderEnv, ProviderHeaders, Transport,
        UserContent, UserMessage, UserRole,
    };
    use serde::Deserialize;
    use serde_json::{Value, json};

    const FIXTURE: &str = include_str!("../../tests/fixtures/api-shared/simple_options.json");

    fn fixture() -> Value {
        serde_json::from_str(FIXTURE).expect("fixture parses")
    }

    fn context() -> TranscriptContext {
        TranscriptContext::from_normalized(Context {
            messages: vec![Message::User(UserMessage {
                role: UserRole::User,
                content: UserContent::Text("Hello".into()),
                timestamp: 1_700_000_000_000,
            })],
            ..Context::default()
        })
    }

    fn model_of(value: &Value) -> Model {
        serde_json::from_value(value.clone()).expect("model deserializes")
    }

    fn text_level(value: &Value) -> ThinkingLevel {
        serde_json::from_value(value.clone()).expect("thinking level deserializes")
    }

    #[derive(Debug, Default, Deserialize)]
    #[serde(rename_all = "camelCase", default)]
    struct OptionsJson {
        temperature: Option<f64>,
        sampling_params: Option<SamplingParams>,
        max_tokens: Option<u64>,
        api_key: Option<String>,
        cache_retention: Option<CacheRetention>,
        session_id: Option<String>,
        headers: Option<ProviderHeaders>,
        identity: Option<ClientIdentity>,
        timeout_ms: Option<u64>,
        max_retries: Option<u32>,
        max_retry_delay_ms: Option<u64>,
        metadata: Option<crate::types::JsonObject>,
        env: Option<ProviderEnv>,
        transport: Option<Transport>,
        websocket_connect_timeout_ms: Option<u64>,
        reasoning: Option<ThinkingLevel>,
    }

    impl OptionsJson {
        fn into_options(self) -> SimpleStreamOptions {
            SimpleStreamOptions {
                base: StreamOptions {
                    temperature: self.temperature,
                    sampling_params: self.sampling_params,
                    max_tokens: self.max_tokens,
                    api_key: self.api_key,
                    cache_retention: self.cache_retention,
                    session_id: self.session_id,
                    headers: self.headers,
                    identity: self.identity,
                    timeout_ms: self.timeout_ms,
                    max_retries: self.max_retries,
                    max_retry_delay_ms: self.max_retry_delay_ms,
                    metadata: self.metadata,
                    env: self.env,
                    transport: self.transport,
                    websocket_connect_timeout_ms: self.websocket_connect_timeout_ms,
                    ..StreamOptions::default()
                },
                reasoning: self.reasoning,
                ..SimpleStreamOptions::default()
            }
        }
    }

    fn value_or_null<T: serde::Serialize>(value: &Option<T>) -> Value {
        match value {
            Some(value) => serde_json::to_value(value).expect("serializes"),
            None => Value::Null,
        }
    }

    fn base_options_json(options: &StreamOptions) -> Value {
        json!({
            "temperature": options.temperature,
            "samplingParams": value_or_null(&options.sampling_params),
            "maxTokens": options.max_tokens,
            "apiKey": options.api_key,
            "cacheRetention": value_or_null(&options.cache_retention),
            "sessionId": options.session_id,
            "headers": value_or_null(&options.headers),
            "identity": value_or_null(&options.identity),
            "timeoutMs": options.timeout_ms,
            "maxRetries": options.max_retries,
            "maxRetryDelayMs": options.max_retry_delay_ms,
            "metadata": value_or_null(&options.metadata),
            "env": value_or_null(&options.env),
            "transport": value_or_null(&options.transport),
            "websocketConnectTimeoutMs": options.websocket_connect_timeout_ms,
            "hasOnPayload": options.on_payload.is_some(),
            "hasOnResponse": options.on_response.is_some(),
            "hasOnProviderStreamEvent": options.on_provider_stream_event.is_some(),
            "hasSignal": options.signal.is_some(),
        })
    }

    #[test]
    fn build_base_options_matches_typescript_reference() {
        let fixture = fixture();
        for case in fixture["cases"].as_array().expect("cases") {
            let name = case["name"].as_str().unwrap();
            let model = model_of(&case["model"]);
            let mut options: SimpleStreamOptions =
                serde_json::from_value::<OptionsJson>(case["options"].clone())
                    .unwrap_or_else(|error| panic!("case {name} options: {error}"))
                    .into_options();
            if name == "allFields" {
                options.base.on_payload = Some(std::sync::Arc::new(|payload, _model| {
                    let _ = payload;
                    None
                }));
                options.base.on_response = Some(std::sync::Arc::new(|_response, _model| {}));
                options.base.on_provider_stream_event =
                    Some(std::sync::Arc::new(|_event, _model| {}));
                options.base.signal = Some(tokio_util::sync::CancellationToken::new());
            }
            let api_key = case["apiKey"].as_str();
            let base = build_base_options(&model, &context(), Some(&options), api_key);
            let actual = base_options_json(&base);
            let expected = &case["expected"];
            for key in [
                "temperature",
                "samplingParams",
                "maxTokens",
                "apiKey",
                "cacheRetention",
                "sessionId",
                "headers",
                "identity",
                "timeoutMs",
                "maxRetries",
                "maxRetryDelayMs",
                "metadata",
                "env",
                "transport",
                "websocketConnectTimeoutMs",
                "hasOnPayload",
                "hasOnResponse",
                "hasOnProviderStreamEvent",
                "hasSignal",
            ] {
                assert_eq!(
                    kiana_json::js_object::stringify(&actual[key]),
                    kiana_json::js_object::stringify(&expected[key]),
                    "case {name} field {key}"
                );
            }
        }
    }

    #[test]
    fn build_base_options_without_options_matches_reference() {
        let fixture = fixture();
        let case = fixture["cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|case| case["name"] == "defaults")
            .unwrap();
        let model = model_of(&case["model"]);
        let base = build_base_options(&model, &context(), None, None);
        assert_eq!(base.max_tokens, Some(16_384));
        assert!(base.sampling_params.is_none());
        assert!(base.temperature.is_none());
    }

    #[test]
    fn clamp_max_tokens_matches_typescript_reference() {
        let fixture = fixture();
        let contexts = &fixture["contexts"];
        let model = model_of(&fixture["cases"][0]["model"]);
        let mut window_zero = model.clone();
        window_zero.context_window = Some(0);
        assert_eq!(
            clamp_max_tokens_to_context(&window_zero, &context(), 8_000),
            contexts["zeroWindow"].as_u64().unwrap()
        );
        let mut window_ten_k = model.clone();
        window_ten_k.context_window = Some(10_000);
        assert_eq!(
            clamp_max_tokens_to_context(&window_ten_k, &context(), 8_000),
            contexts["clamped"].as_u64().unwrap()
        );
        let mut window_three = model.clone();
        window_three.context_window = Some(3);
        assert_eq!(
            clamp_max_tokens_to_context(&window_three, &context(), 8_000),
            contexts["negativeAvailable"].as_u64().unwrap()
        );
        // `maxTokens: 0` 保持 0（钳制只抬上下限的下界）。
        assert_eq!(
            clamp_max_tokens_to_context(&model, &context(), 0),
            contexts["zeroMax"].as_u64().unwrap()
        );
    }

    #[test]
    fn resolve_sampling_matches_typescript_reference() {
        let fixture = fixture();
        for case in fixture["resolveSampling"].as_array().unwrap() {
            let name = case["name"].as_str().unwrap();
            let model = model_of(&case["model"]);
            let level: ModelThinkingLevel =
                serde_json::from_value(case["level"].clone()).expect("level");
            let request: Option<SamplingParams> = if case["request"].is_null() {
                None
            } else {
                Some(serde_json::from_value(case["request"].clone()).unwrap())
            };
            let actual = resolve_sampling_params(&model, level, request.as_ref());
            let actual = match actual {
                Some(params) => serde_json::to_value(params).unwrap(),
                None => Value::Null,
            };
            assert_eq!(
                kiana_json::js_object::stringify(&actual),
                kiana_json::js_object::stringify(&case["expected"]),
                "case {name}"
            );
        }
    }

    #[test]
    fn thinking_budgets_match_typescript_reference() {
        let fixture = fixture();
        for case in fixture["budgets"].as_array().unwrap() {
            let level = text_level(&case["level"]);
            let custom: Option<ThinkingBudgets> = if case["custom"].is_null() {
                None
            } else {
                Some(serde_json::from_value(case["custom"].clone()).unwrap())
            };
            assert_eq!(
                thinking_budget_for_level(level, custom.as_ref()),
                case["expected"].as_u64().unwrap(),
                "budget {}",
                case["name"].as_str().unwrap()
            );
        }
        for case in fixture["clampReasoning"].as_array().unwrap() {
            let input = if case["input"].is_null() {
                None
            } else {
                Some(text_level(&case["input"]))
            };
            let expected = if case["expected"].is_null() {
                None
            } else {
                Some(text_level(&case["expected"]))
            };
            assert_eq!(clamp_reasoning(input), expected);
        }
        for case in fixture["adjusts"].as_array().unwrap() {
            let base: Option<u64> = if case["base"].is_null() {
                None
            } else {
                Some(case["base"].as_u64().unwrap())
            };
            let custom: Option<ThinkingBudgets> = if case["custom"].is_null() {
                None
            } else {
                Some(serde_json::from_value(case["custom"].clone()).unwrap())
            };
            let adjusted = adjust_max_tokens_for_thinking(
                base,
                case["modelMax"].as_u64().unwrap(),
                text_level(&case["level"]),
                custom.as_ref(),
            );
            assert_eq!(
                adjusted.max_tokens,
                case["expected"]["maxTokens"].as_u64().unwrap()
            );
            assert_eq!(
                adjusted.thinking_budget,
                case["expected"]["thinkingBudget"].as_u64().unwrap()
            );
        }
        assert_eq!(
            MIN_ANSWER_TOKENS,
            fixture["constants"]["minAnswerTokens"].as_u64().unwrap()
        );
        assert_eq!(
            serde_json::to_value(DEFAULT_THINKING_BUDGETS).unwrap(),
            fixture["constants"]["defaults"]
        );
    }
}
