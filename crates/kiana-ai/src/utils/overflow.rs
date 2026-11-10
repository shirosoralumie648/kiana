//! `utils/overflow.ts`——上下文溢出检测。
//!
//! TS: `packages/ai/src/utils/overflow.ts`。
//!
//! 三类溢出：
//! 1. **错误型**：多数 provider 返回 `stopReason: "error"` + 可识别的错误文本；
//! 2. **静默型**：请求成功但 `usage.input + cacheRead` 超过上下文窗口（z.ai 风格）；
//! 3. **length 截断型**：MiMo 把超长输入裁到刚好填满窗口，返回 `stopReason: "length"` 且
//!    `output === 0`。
//!
//! 正则表的顺序、大小写不敏感匹配与 `NON_OVERFLOW_PATTERNS` 的排除是当前稳定契约，
//! 改动需同步测试。
//! TS 用 JS 的 `/pattern/i`，其中 `\d`/`\w` 只认 ASCII、`\s` 包含 U+FEFF；Rust 的
//! `regex`/`fancy_regex` 用 Unicode 字符类，会把阿拉伯-印度数字或非 ASCII 单词字符当成
//! `\d`/`\w`，从而误判溢出。这里用 [`regress`]（ECMAScript 引擎）精确复刻 JS 语义。
//!
//! # 为什么需要三层判定
//!
//! 不同 provider 对“输入超窗”的反馈方式不同，只看 `stop_reason` 会漏判：
//!
//! - 多数返回 `error` + 可识别文本（需要正则表，且各家用词不同）；
//! - 少数（z.ai）**成功返回**，只能比较 `usage` 与上下文窗口；
//! - MiMo 把输入截到刚好填满窗口后以 `length` 结束且 `output == 0`。
//!
//! 三种情形按 `stop_reason` 分流、互斥命中，因此这是一个无状态的纯判定器，不修改消息。
//!
//! # 调用方与后果
//!
//! - `kiana-session::agent_session`：[`is_context_overflow`] 为真时禁用普通重试
//!   （`is_retryable_error`）；在压缩已启用（`compaction_settings().enabled`）且消息属于
//!   当前模型（`model_for_message`）时触发一次恢复：`stop_reason == Stop` 的静默溢出只做
//!   压缩、不重试，其它溢出做一次压缩后重试；`overflow_recovery_attempted` 阻止第二次尝试。
//!   [`is_recoverable_length`] 用于 `length` 提前结束时的同类有界压缩重试。
//! - `kiana-durable::harness::generation`：错误消息命中溢出且策略允许时进入压缩流程。
//!
//! # 约束
//!
//! - 判定只读 [`AssistantMessage`] 的 `stop_reason`、`error_message`、`provider` 与
//!   `usage`；其中 token 口径是“净输入 + 缓存命中”，本模块只加 `input + cache_read`
//!   （**不含** `cache_write`）；这是当前稳定口径，改口径会让 z.ai/MiMo 的阈值行为
//!   偏离已锁定的测试。
//! - `context_window == Some(0)` 与 `None` 等价（TS 的 `if (contextWindow && ...)` 把 0
//!   当 falsy），两种静默/截断情形都不检查；错误文本情形不受窗口影响。
//! - 正则只在首次使用时编译并缓存在 `OnceLock` 中，之后复用；模式表是常量，拼写错误会在
//!   首次判定时 panic，而不是静默返回 `false`。
//! - 大小写不敏感用的是 ECMAScript 的 `i` 语义（不是 Rust 的 Unicode 折叠），详见单测
//!   `matches_with_javascript_character_classes`。
//! - 纯布尔判定：无 `Result`、无 IO、无内部可变状态，同一输入重复调用结果相同。
//!   `NON_OVERFLOW_PATTERNS` 在溢出模式之前检查（TS 同样如此），避免把限流等暂时性错误
//!   当作上下文溢出。

use crate::types::{AssistantMessage, StopReason};
use regress::Regex;
use std::sync::OnceLock;

/// 溢出模式表（TS 源：`OVERFLOW_PATTERNS`），顺序固定。
///
/// 每条都是 ECMAScript 正则源文本，由 [`compile`] 以 `i` 标志编译。顺序本身不改变布尔
/// 结果（命中任意一条即返回 `true`），但顺序固定，便于逐条核对和定位差异。
/// 最后一条 HTTP 413 模式用 `^` 锚定字符串开头（JS 无 `m` 标志），只有整串以 `413`
/// 或 `<某前缀> API error (413):` 开头才匹配；`tests/overflow.rs` 与
/// `tests/context_overflow.rs` 锁定这些文案。
const OVERFLOW_PATTERN_SOURCES: &[&str] = &[
    r"prompt (?:is )?too long",               // Anthropic 与 z.ai
    r"prompt exceeds max length",             // z.ai CN endpoint
    r"request_too_large",                     // Anthropic 413
    r"input is too long for requested model", // Amazon Bedrock
    r"exceeds the context window",            // OpenAI
    r"exceeds (?:the )?(?:model'?s )?maximum context length(?: of [\d,]+ tokens?|\s*\([\d,]+\))", // LiteLLM
    r"input token count.*exceeds the maximum", // Google Gemini
    r"maximum prompt length is \d+",           // xAI
    r"reduce the length of the messages",      // Groq
    r"maximum context length is \d+ tokens",   // OpenRouter
    r"exceeds (?:the )?maximum allowed input length of [\d,]+ tokens?", // OpenRouter/Poolside
    r"input \(\d+ tokens\) is longer than the model'?s context length \(\d+ tokens\)", // Together
    r"exceeds the limit of \d+",               // GitHub Copilot
    r"exceeds the available context size",     // llama.cpp
    r"greater than the context length",        // LM Studio
    r"context window exceeds limit",           // MiniMax
    r"exceeded model token limit",             // Kimi For Coding
    r"too large for model with \d+ maximum context length", // Mistral
    r"prompt has [\d,]+ tokens?, but the configured context size is [\d,]+ tokens?", // DS4
    r"model_context_window_exceeded",          // z.ai finish_reason
    r"prompt too long; exceeded (?:max )?context length", // Ollama
    r"range of input length should be",        // DashScope / Qwen
    r"context[_ ]length[_ ]exceeded",          // 通用回退
    r"too many tokens",                        // 通用回退
    r"token limit exceeded",                   // 通用回退
    r"^(?:[\w -]+ API error \(413\):|413(?:\s|:|$))", // HTTP 413
];

/// 非溢出模式（TS: `NON_OVERFLOW_PATTERNS`）：命中即排除。
///
/// 检查发生在所有溢出模式之前：Bedrock 的 `"Throttling error: Too many tokens, ..."`
/// 会命中通用的 `too many tokens` 溢出模式，必须靠这里的 `^(Throttling error|Service
/// unavailable):` 排除；限流与 HTTP 429 同理。命中任意一条就直接返回 `false`，不再看
/// Cerebras 的无 body 模式。
const NON_OVERFLOW_PATTERN_SOURCES: &[&str] = &[
    r"^(Throttling error|Service unavailable):", // AWS Bedrock
    r"rate limit",                               // 通用限流
    r"too many requests",                        // 通用 429
];

/// Cerebras 的无 body 溢出（TS: `CEREBRAS_BODYLESS_OVERFLOW_PATTERN`）。
///
/// 只在 `provider == "cerebras"` 时使用：400 且无 body 对其他 provider 不构成溢出证据
/// （413 有独立的通用模式）。`provider` 比较区分大小写，不做归一化。
const CEREBRAS_BODYLESS_OVERFLOW_SOURCE: &str = r"^4(?:00|13)\s*(?:status code)?\s*\(no body\)";

/// 编译一条 ECMAScript 正则。
///
/// `i` 标志对应 TS 的 `/source/i`：`regress` 按 ECMAScript 语义处理 `\d`/`\w`/`\s`，
/// 因此不会把非 ASCII 数字或 U+FEFF 的差异带进来（模块文档有说明）。模式全部是常量；
/// 语法错误属于开发期问题，`expect` 会在首次编译（`OnceLock` 初始化或
/// [`get_overflow_patterns`]）时 panic，测试会立即暴露。
fn compile(source: &str) -> Regex {
    // TS 的 `/source/i`：ECMAScript 的 `i` 标志（而非 Rust 的 `(?i)` Unicode 折叠）。
    Regex::with_flags(source, "i").expect("valid overflow regex")
}

/// `getOverflowPatterns()`：返回溢出模式（供测试使用）。
///
/// 返回 `regress::Regex`（ECMAScript 语义）；用 [`regress::Regex::find`] 判断是否匹配。
///
/// 每次调用都会重新编译全部模式并返回独立的 `Vec`，不共享内部的 `OnceLock` 缓存；它只
/// 用于测试/诊断，生产判定请走 [`is_context_overflow`]（那里只编译一次）。
pub fn get_overflow_patterns() -> Vec<Regex> {
    OVERFLOW_PATTERN_SOURCES
        .iter()
        .map(|source| compile(source))
        .collect()
}

/// 全局缓存的溢出模式（首次调用时编译）。
///
/// `OnceLock::get_or_init` 负责并发安全：成功时只有一个初始化结果会被发布；模式不可变，
/// 之后所有判定共享同一批 `Regex`。初始化若 panic（常量语法错误），panic 会向调用方
/// 传播，不会留下半初始化的缓存。
fn overflow_patterns() -> &'static Vec<Regex> {
    static PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    PATTERNS.get_or_init(get_overflow_patterns)
}

/// 全局缓存的非溢出模式（首次调用时编译；并发语义同 [`overflow_patterns`]）。
fn non_overflow_patterns() -> &'static Vec<Regex> {
    static PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        NON_OVERFLOW_PATTERN_SOURCES
            .iter()
            .map(|source| compile(source))
            .collect()
    })
}

/// 全局缓存的 Cerebras 无 body 模式（首次匹配时编译）。
fn cerebras_bodyless_overflow() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| compile(CEREBRAS_BODYLESS_OVERFLOW_SOURCE))
}

/// 判断正则是否在文本中任意位置匹配（不等价于整串匹配）。
///
/// `regress` 的 `find` 不修改正则状态；本文件所有模式都没有 `g` 标志，因此不存在 JS
/// 全局正则 `lastIndex` 那样的跨调用状态。
fn is_match(pattern: &Regex, text: &str) -> bool {
    pattern.find(text).is_some()
}

/// `isContextOverflow(message, contextWindow?)`。
///
/// 判定顺序（命中即短路返回，顺序固定便于核对）：
///
/// 1. `stop_reason == Error` 且 `error_message` 存在：先查
///    `NON_OVERFLOW_PATTERN_SOURCES`（任一命中即否定），再查
///    `OVERFLOW_PATTERN_SOURCES`；最后对 `provider == "cerebras"` 补查无 body 的 400/413。
/// 2. `context_window` 非 0 且 `stop_reason == Stop`：
///    `usage.input + usage.cache_read > context_window` 即静默溢出（严格大于）。
/// 3. `context_window` 非 0 且 `stop_reason == Length` 且 `usage.output == 0`：
///    同一输入和 `>= context_window * 0.99` 即截断型溢出。
///
/// # 参数
///
/// - `message`：一次 assistant 调用的最终消息，只读。
/// - `context_window`：模型目录里的窗口大小；`None` 或 `Some(0)` 表示未知，此时只保留
///   情形 1 的文本判定。
///
/// # 返回与边界
///
/// - 返回 `true` 表示调用方应按“上下文溢出”处理（压缩上下文或切换模型），不能只当作
///   可重试错误。
/// - `Error` 但 `error_message` 为 `None` 时不会命中情形 1；`Aborted`/`Length` 即使
///   文本含 413 也不会命中——HTTP 状态匹配只对失败响应生效。
/// - `error_message == Some("")` 在 TS 中因 falsy 被直接跳过；Rust 会进入正则匹配，但
///   所有模式都要求至少一个字符，因此结果相同。
/// - `usage.input + usage.cache_read` 以 `u64` 相加，未做饱和处理：两个值都接近
///   `u64::MAX` 时，debug 构建会因溢出 panic，release 构建会回绕（按工作区默认配置）。
/// - 情形 3 的 `0.99` 是 TS 的原始阈值；改成 `1.0` 会漏掉 MiMo 的边界样本（单测使用
///   `input + cache_read = 1_048_570`、窗口 `1_048_576`）。
pub fn is_context_overflow(message: &AssistantMessage, context_window: Option<u64>) -> bool {
    // 情形 1：错误文本。
    // NON_OVERFLOW 优先，命中后连 Cerebras 特例也不再检查。
    if message.stop_reason == StopReason::Error
        && let Some(error_message) = message.error_message.as_deref()
    {
        let is_non_overflow = non_overflow_patterns()
            .iter()
            .any(|pattern| is_match(pattern, error_message));
        if !is_non_overflow {
            if overflow_patterns()
                .iter()
                .any(|pattern| is_match(pattern, error_message))
            {
                return true;
            }
            // Cerebras 的 400 无 body 只在该 provider 下视为溢出。
            if message.provider == "cerebras"
                && is_match(cerebras_bodyless_overflow(), error_message)
            {
                return true;
            }
        }
    }

    // 情形 2：静默溢出（z.ai 风格）。TS 的 `if (contextWindow && ...)` 把 0 当 falsy。
    let has_context_window = context_window.is_some_and(|window| window != 0);
    if has_context_window
        && let (Some(context_window), StopReason::Stop) = (context_window, message.stop_reason)
    {
        // 口径：只加净输入与缓存命中，不含 cache_write（z.ai/MiMo 阈值依赖此口径）。
        let input_tokens = message.usage.input + message.usage.cache_read;
        if input_tokens > context_window {
            return true;
        }
    }

    // 情形 3：length 截断（MiMo 风格）。
    // 先要求 output == 0，再看输入是否填满窗口的 99%。
    if has_context_window
        && let (Some(context_window), StopReason::Length) = (context_window, message.stop_reason)
        && message.usage.output == 0
    {
        let input_tokens = message.usage.input + message.usage.cache_read;
        if input_tokens as f64 >= context_window as f64 * 0.99 {
            return true;
        }
    }

    false
}

/// `isRecoverableLength(message, desiredMaxOutput)`：length 结束但输出未达预期上限。
///
/// `desired_max_output` 必须是**任何按上下文裁剪之前**的原始上限。
///
/// # 参数与语义
///
/// - `message.stop_reason` 必须是 [`StopReason::Length`]；`Error`/`Aborted` 一律 `false`。
/// - `desired_max_output <= 0` 表示调用方不知道预期上限（例如模型目录没有配置
///   `max_tokens`），此时恒为 `false`，不会触发压缩重试。
/// - 比较为严格小于：输出恰好等于上限说明模型正常写满，不算“提前截断”。
/// - `usage.output` 是 `u64`，这里按 `i64` 比较；超过 `i64::MAX` 的值会回绕为负数。
///
/// 该判定与上下文窗口无关（不需要 `context_window`）；调用方 `kiana-session` 传入的是
/// `model.max_tokens`（原值，不是按上下文裁剪后的值），与 TS 的调用约定一致。
pub fn is_recoverable_length(message: &AssistantMessage, desired_max_output: i64) -> bool {
    message.stop_reason == StopReason::Length
        && desired_max_output > 0
        && (message.usage.output as i64) < desired_max_output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assistant(stop_reason: StopReason, error: Option<&str>) -> AssistantMessage {
        AssistantMessage {
            stop_reason,
            error_message: error.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn matches_with_javascript_character_classes() {
        // JS `\d` 只认 ASCII 数字：阿拉伯-印度数字不算。
        assert!(!is_context_overflow(
            &assistant(StopReason::Error, Some("exceeds the limit of ١٢٣")),
            None
        ));
        // JS `\w` 只认 `[A-Za-z0-9_]`：带变音符号的前缀不能落入 `413` 模式。
        assert!(!is_context_overflow(
            &assistant(StopReason::Error, Some("Über API error (413): failed")),
            None
        ));
        assert!(is_context_overflow(
            &assistant(
                StopReason::Error,
                Some("github-copilot API error (413): failed")
            ),
            None
        ));
        // JS `\s` 包含 U+FEFF（BOM），Unicode `\s` 不含。
        assert!(is_context_overflow(
            &assistant(StopReason::Error, Some("413\u{FEFF}")),
            None
        ));
        let mut cerebras = assistant(StopReason::Error, Some("413\u{FEFF}(no body)"));
        cerebras.provider = "cerebras".into();
        assert!(is_context_overflow(&cerebras, None));
        // JS 的 `i` 标志按 `toUpperCase` 归一化（Kelvin 符号 U+212A 不与 ASCII `k` 等价）。
        assert!(!is_context_overflow(
            &assistant(StopReason::Error, Some("too many toke\u{212A}s")),
            None
        ));
    }

    #[test]
    fn matches_provider_error_texts() {
        for text in [
            "prompt is too long: 213462 tokens > 200000 maximum",
            "413 {\"error\":{\"type\":\"request_too_large\",\"message\":\"Request exceeds the maximum size\"}}",
            "Your input exceeds the context window of this model",
            "Requested token count exceeds the model's maximum context length of 131072 tokens",
            "Input length (265330) exceeds model's maximum context length (262144).",
            "The input token count (1196265) exceeds the maximum number of tokens allowed (1048575)",
            "This model's maximum prompt length is 131072 but the request contains 537812 tokens",
            "Please reduce the length of the messages or completion",
            "This endpoint's maximum context length is 200000 tokens. However, you requested about 250000 tokens",
            "Input length 265330 exceeds the maximum allowed input length of 262144 tokens.",
            "The input (265330 tokens) is longer than the model's context length (262144 tokens).",
            "the request exceeds the available context size, try increasing it",
            "tokens to keep from the initial prompt is greater than the context length",
            "prompt token count of 200000 exceeds the limit of 190000",
            "invalid params, context window exceeds limit",
            "Your request exceeded model token limit: 200000 (requested: 250000)",
            "Prompt has 200000 tokens, but the configured context size is 190000 tokens",
            "Prompt contains 250000 tokens too large for model with 200000 maximum context length",
            "Prompt too long",
            "prompt too long; exceeded max context length by 10 tokens",
            "Range of input length should be [1, 200000]",
            "context_length_exceeded",
            "too many tokens",
            "token limit exceeded",
            "Provider API error (413): failed to parse request",
            "413",
        ] {
            assert!(
                is_context_overflow(&assistant(StopReason::Error, Some(text)), None),
                "should match: {text}"
            );
        }
    }

    #[test]
    fn excludes_throttling_and_rate_limits() {
        for text in [
            "Throttling error: Too many tokens, please wait before trying again.",
            "Service unavailable: too many tokens",
            "Rate limit reached for requests",
            "too many requests",
        ] {
            assert!(
                !is_context_overflow(&assistant(StopReason::Error, Some(text)), None),
                "should not match: {text}"
            );
        }
    }

    #[test]
    fn detects_silent_and_length_overflow() {
        let mut message = assistant(StopReason::Stop, None);
        message.usage.input = 200_001;
        assert!(is_context_overflow(&message, Some(200_000)));
        assert!(!is_context_overflow(&message, None));

        let mut message = assistant(StopReason::Length, None);
        message.usage.input = 198_000;
        message.usage.output = 0;
        assert!(is_context_overflow(&message, Some(200_000)));

        message.usage.output = 1;
        assert!(!is_context_overflow(&message, Some(200_000)));
    }

    #[test]
    fn treats_a_zero_context_window_as_absent() {
        // TS 的 `if (contextWindow && ...)`：0 是 falsy，静默溢出与 length 截断两种情形都跳过。
        let mut message = assistant(StopReason::Stop, None);
        message.usage.input = 10;
        assert!(!is_context_overflow(&message, Some(0)));

        let mut message = assistant(StopReason::Length, None);
        message.usage.input = 10;
        message.usage.output = 0;
        assert!(!is_context_overflow(&message, Some(0)));
    }

    #[test]
    fn cerebras_bodyless_overflow_needs_the_provider() {
        let mut message = assistant(StopReason::Error, Some("400 status code (no body)"));
        assert!(!is_context_overflow(&message, None));
        message.provider = "cerebras".into();
        assert!(is_context_overflow(&message, None));
    }

    #[test]
    fn recoverable_length_compares_against_the_original_limit() {
        let mut message = assistant(StopReason::Length, None);
        message.usage.output = 100;
        assert!(is_recoverable_length(&message, 1000));
        assert!(!is_recoverable_length(&message, 100));
        assert!(!is_recoverable_length(&message, 0));
    }
}
