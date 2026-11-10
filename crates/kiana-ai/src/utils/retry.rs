//! `utils/retry.ts`——assistant 调用的有界重试策略与错误分类。
//!
//! TS: `packages/ai/src/utils/retry.ts`。
//!
//! 两部分：
//! - [`is_retryable_assistant_error`]：判断失败消息是否像瞬时 provider/传输错误
//!   （`NON_RETRYABLE_PROVIDER_LIMIT_ERROR_PATTERN` 先排除配额/计费耗尽）；
//! - [`retry_assistant_call`]：有界次数 + 指数退避的重试循环，回调与取消语义逐条对应。
//!
//! TS 用 `new RegExp(patterns.join("|"), "i")` 把几十条模式合并成两条正则；两条正则在 JS 里
//! 都用到了**后顾** `(?<![a-z0-9])`，因此这里用 [`fancy_regex`]（工作区已 pin）而不是
//! `regex` crate。
//!
//! # 被谁调用
//! - `kiana-compaction` 的 `complete_summarization` 与 `kiana-resources` 的 bug report
//!   摘要路径用 [`retry_assistant_call`] 包住一次模型调用；
//! - `kiana-session` 的 `agent_session` 在自动重试路径上复用
//!   [`is_retryable_assistant_error`] 与 [`retry_delay_ms_js`]（它有自己的状态机与事件，
//!   不直接调用本模块的重试循环）。
//!
//! # 执行流程（[`retry_assistant_call`]）
//! 1. `max_attempts` 只取「策略启用时的 `max_retries`」，否则为 0；首次调用不计入重试次数。
//! 2. 每轮先 `produce().await` 拿到一条 `AssistantMessage`，再按 `stop_reason` 分流：
//!    `Aborted` 直接返回；非 `Error` 视为成功返回；`Error` 才检查预算与可重试性。
//! 3. 决定重试时 `attempt += 1`，算出退避毫秒数，依次发出 `on_retry_scheduled` →
//!    退避睡眠 → `on_retry_attempt_start`，然后进入下一轮。
//! 4. 只要「已经发生过至少一次重试」，无论最终成功、失败还是中止，都会发一次
//!    `on_retry_finished`；一次重试都没发生时不发该回调。
//!
//! # 重要约束与失败模式
//! - `signal` 只用于打断**退避睡眠**；`produce` 返回的 future 不会被 race 或取消，闭包
//!   内部必须自己观察取消（压缩/摘要的调用方就是把同一个 signal 传进 provider 层）。
//! - 回调是 `async` 的，且在循环里被 `await`；回调长时间不返回会直接拖慢重试节奏。
//! - 两条正则在首次判定时编译并缓存在 `OnceLock` 里。模式串是编译期常量，若编译失败会在
//!   首次调用时 panic（`expect`），属于不可恢复的内部错误。
//! - 退避睡眠被中止时，返回的是「最后一次错误响应」的副本：`content`/`usage` 等字段保留，
//!   `error_message` 清空、`stop_reason` 改为 `Aborted`。

use crate::types::{AssistantMessage, StopReason};
use crate::utils::abort::AbortScope;
use fancy_regex::Regex;
use std::sync::OnceLock;

/// `DEFAULT_MAX_AGENT_RETRY_DELAY_MS`。
///
/// 单位毫秒（常量值 `60_000` 即 60 秒）。[`RetryPolicy::max_agent_delay_ms`] 为 `None` 时，
/// 退避延迟按这个值封顶。
pub const DEFAULT_MAX_AGENT_RETRY_DELAY_MS: u64 = 60_000;

/// `NON_RETRYABLE_PROVIDER_LIMIT_ERROR_PATTERN` 的分支（顺序即匹配优先级）。
const NON_RETRYABLE_PATTERNS: &[&str] = &[
    "GoUsageLimitError",
    "FreeUsageLimitError",
    "Monthly usage limit reached",
    "available balance",
    "insufficient_quota",
    "out of budget",
    "quota exceeded",
    "billing",
    "subscription_sharing_usage_limit_exceeded",
    "subscription_sharing_workplace_individual_usage_limit_exceeded",
];

/// `RETRYABLE_PROVIDER_ERROR_PATTERN` 的分支（顺序即匹配优先级，逐条注释见 TS 源）。
const RETRYABLE_PATTERNS: &[&str] = &[
    // 通用 provider 过载、HTTP 状态与服务器瞬时故障。
    "overloaded",
    "server_busy",
    "servers are currently busy",
    "currently experiencing high demand",
    "model is at capacity",
    "rate.?limit",
    "too many requests",
    // 状态码不能是更长单词/数字的一部分，例如请求 id 或字节数（#11057）。
    "(?<![a-z0-9])(?:429|500|502|503|504|520|524)(?![a-z0-9])",
    // 也匹配 "The service is temporarily unavailable"（#10965）。
    "service.?(?:is.?)?(?:temporarily.?)?unavailable",
    "server.?error",
    "internal.?error",
    // 上游瞬时故障的 wrapper/provider 文本，含 OpenRouter 的 "Provider returned error"（#2264）
    // 与网关措辞 "The upstream provider returned an error"（#10842）。
    "provider.?returned.?(?:an.?)?error",
    "exceeded request buffer limit while retrying upstream",
    // 网关转发上游响应流失败，以 OpenAI Responses 的 `response.failed` 事件送达、无错误码（#10920）。
    "unable to forward upstream",
    "upstream.?request.?failed",
    "upstream_stream_read_error",
    "stream was interrupted",
    "http/?2 stream failed",
    // 网络、代理与 fetch 传输失败（含 Codex 的 "upstream connect"/"connection refused"/
    // "reset before headers"（#733）与 OpenRouter 断连（#3317））。
    "network.?error",
    "connection.?error",
    "connection.?refused",
    "connection.?lost",
    "other side closed",
    "fetch failed",
    // node-fetch 系统错误，例如 google-vertex 的 Google ADC OAuth token 刷新（#10996）。
    "request to \\S+ failed, reason:",
    "getaddrinfo",
    "ENOTFOUND",
    "EAI_AGAIN",
    "upstream.?connect",
    "reset before headers",
    "socket hang up",
    "socket connection was closed",
    "timed? out",
    "timeout",
    "terminated",
    // WebSocket 传输可能给出 close/error 文本而不是 HTTP/fetch 文本。
    "websocket.?closed",
    "websocket.?error",
    // SDK/传输的过早流结束（Anthropic #4433、Bedrock/Smithy #3594）。
    "ended without",
    "stream ended before message_stop",
    "stream ended before a terminal response event",
    "http2 request did not get a response",
    "stream disconnected before completion",
    "pending stream has been canceled",
    // provider 要求的重试延迟上限失败应走外层重试策略，让调用方展示/中止退避（#1123）。
    "retry delay",
    // OpenAI Responses 与 Bedrock 流中显式给出的重试指引（#6019）。
    "you can retry your request",
    "try your request again",
    "please retry your request",
    // 基于 gRPC 的 provider（例如 NVIDIA NIM）。
    "ResourceExhausted",
    // Sign in with ChatGPT：用量或用户数据暂时不可用。
    "subscription_sharing_usage_unavailable",
    "subscription_sharing_user_unavailable",
];

/// 惰性编译的「不可重试」正则：把 [`NON_RETRYABLE_PATTERNS`] 用 `|` 拼成大小写不敏感的
/// 单条表达式（对应 TS 的 `new RegExp(patterns.join("|"), "i")`）。
///
/// `OnceLock` 保证只编译一次；首次判定时才付出编译成本。`expect` 的前提是常量模式表本身
/// 合法——这是编译期就固定的输入，失败只可能是代码被改坏。
fn non_retryable_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(&format!("(?i){}", NON_RETRYABLE_PATTERNS.join("|")))
            .expect("valid non-retryable pattern")
    })
}

/// 惰性编译的「可重试」正则；构造方式与 [`non_retryable_pattern`] 相同。
fn retryable_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(&format!("(?i){}", RETRYABLE_PATTERNS.join("|")))
            .expect("valid retryable pattern")
    })
}

/// `RetryPolicy`；`maxAgentDelayMs` 缺省为 60 秒。
///
/// `Default` 派生出的策略是 **`enabled: false`、`max_retries: 0`、`base_delay_ms: 0`**，
/// 也就是默认完全不重试；调用方必须显式打开 `enabled`。
///
/// 注意 `max_agent_delay_ms: None` 不是「无上限」，而是回落到
/// [`DEFAULT_MAX_AGENT_RETRY_DELAY_MS`]；`Some(0)` 才是把退避延迟压成 0。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RetryPolicy {
    /// 是否启用重试；`false` 时 `max_retries` 被忽略，等价于上限 0。
    pub enabled: bool,
    /// 最大重试次数（0 = 不重试）。首次调用不计为一次重试。
    pub max_retries: u64,
    /// 基础延迟（毫秒）；第 n 次尝试的延迟是 `baseDelayMs * 2^(n-1)`（抖动前）。
    pub base_delay_ms: u64,
    /// agent 级重试延迟上限（毫秒）；`None` 表示 60 秒。
    pub max_agent_delay_ms: Option<u64>,
}

/// `Number.MAX_SAFE_INTEGER`（2^53 - 1）。
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

/// JS `Number.isSafeInteger(value)`。
///
/// `(-0.0).fract()` 得到 `+0.0`（与 `0.0` 相等），因此会被判为安全整数——与 JS 的
/// `Number.isSafeInteger(-0) === true` 一致。
fn is_safe_integer(value: f64) -> bool {
    value.is_finite() && value.fract() == 0.0 && value.abs() <= MAX_SAFE_INTEGER
}

/// `retryDelayMs(policy, attempt)` 的 JS 数值版本：
/// `min(Number.isSafeInteger(delay) ? delay : MAX_SAFE_INTEGER, maxAgentDelayMs ?? 60s)`，
/// 其中 `delay = baseDelayMs * 2 ** max(0, attempt - 1)`。
///
/// 参数用 `f64` 以保留 JS number 的全部行为（小数、负数、超过 `MAX_SAFE_INTEGER` 的配置）。
/// 注意 TS 在 `delay` 不是安全整数（例如 `baseDelayMs: 0.5`）时**直接退回
/// `Number.MAX_SAFE_INTEGER`**，再由上限截断，而不是取整。
///
/// # 边界
/// 若 `max_agent_delay_ms` 传入 `NaN`，这里与 JS 有差别：`Math.min(x, NaN)` 得到 `NaN`，
/// 而 Rust 的 `f64::min` 返回非 NaN 的那一侧。从 JSON 读出的配置不会产生 `NaN`
/// （JSON 没有该字面量）；只有直接调用本函数并显式传入 `f64::NAN` 时才会遇到。
pub fn retry_delay_ms_js(base_delay_ms: f64, max_agent_delay_ms: Option<f64>, attempt: f64) -> f64 {
    let delay = base_delay_ms * 2f64.powf((attempt - 1.0).max(0.0));
    let safe_delay = if is_safe_integer(delay) {
        delay
    } else {
        MAX_SAFE_INTEGER
    };
    safe_delay.min(max_agent_delay_ms.unwrap_or(DEFAULT_MAX_AGENT_RETRY_DELAY_MS as f64))
}

/// `retryDelayMs(policy, attempt)`：`min(baseDelay * 2^(attempt-1), maxAgentDelayMs ?? 60s)`。
///
/// Rust 的 [`RetryPolicy`] 用 `u64` 保存配置（跨 crate 源码兼容），因此这个便捷重载只覆盖
/// 可表示的非负整数域；小数/负数配置走 [`retry_delay_ms_js`]。
pub fn retry_delay_ms(base_delay_ms: u64, max_agent_delay_ms: Option<u64>, attempt: u64) -> u64 {
    let delay = retry_delay_ms_js(
        base_delay_ms as f64,
        max_agent_delay_ms.map(|delay| delay as f64),
        attempt as f64,
    );
    // 非负整数输入下结果一定是 `[0, MAX_SAFE_INTEGER]` 内的安全整数。
    delay.max(0.0) as u64
}

/// `RetryCallbacks`：每次重试前后的回调。
///
/// TS 的回调可以是 async；Rust 侧统一用返回 `Future` 的闭包表达，取消/错误语义由调用方处理。
/// 回调返回的 `Future`（TS 的回调是 `async`）。
///
/// 闭包必须 `Send`（回调会被存进 `Arc<dyn Fn ... + Send + Sync>`），所以捕获的共享状态要
/// 用线程安全容器——测试里用的是 `Arc<Mutex<Vec<...>>>`。`Future` 的 `Output` 固定为 `()`：
/// 回调不能改变重试结果，也不能把错误传回循环。
pub type RetryCallbackFuture = std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>;
/// `onRetryScheduled(attempt, maxAttempts, delayMs, errorMessage)`。
///
/// 参数依次是：第几次重试（从 1 开始）、允许的重试上限、本次退避的毫秒数、触发重试的
/// 错误文本（已把空消息归一成 `"Unknown error"`）。
pub type OnRetryScheduled =
    std::sync::Arc<dyn Fn(u64, u64, u64, String) -> RetryCallbackFuture + Send + Sync>;
/// `onRetryAttemptStart()`。
///
/// 在退避睡眠**正常结束之后**、下一次 `produce` 之前调用；因中止而提前结束的睡眠不会
/// 触发它。
pub type OnRetryAttemptStart = std::sync::Arc<dyn Fn() -> RetryCallbackFuture + Send + Sync>;
/// `onRetryFinished(success, attempts, errorMessage?)`。
///
/// 参数依次是：最终是否成功、最后一次重试的序号、最终错误文本。只有「发生过重试」的
/// 结束路径会调用它；中止分支也可能只写前两个参数（`error_message` 传 `None`）。
pub type OnRetryFinished =
    std::sync::Arc<dyn Fn(bool, u64, Option<String>) -> RetryCallbackFuture + Send + Sync>;

/// 重试循环的三个可选通知钩子。
///
/// 三个字段都可以为 `None`（省略即不发通知）。`on_retry_scheduled` 与 `on_retry_finished`
/// 接收的 `attempt` 是**重试序号**，从 1 开始，不含首次调用；`on_retry_attempt_start`
/// 不带参数。触发顺序与次数由 [`retry_assistant_call`] 决定。
#[derive(Default, Clone)]
pub struct RetryCallbacks {
    /// 每次重试的退避睡眠**之前**发出（attempt 从 1 开始）。
    pub on_retry_scheduled: Option<OnRetryScheduled>,
    /// 退避睡眠**之后**、重试调用开始之前发出。
    pub on_retry_attempt_start: Option<OnRetryAttemptStart>,
    /// 循环结束时发一次：`success` 表示后续某次调用正常完成。
    pub on_retry_finished: Option<OnRetryFinished>,
}

impl std::fmt::Debug for RetryCallbacks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RetryCallbacks")
            .field("on_retry_scheduled", &self.on_retry_scheduled.is_some())
            .field(
                "on_retry_attempt_start",
                &self.on_retry_attempt_start.is_some(),
            )
            .field("on_retry_finished", &self.on_retry_finished.is_some())
            .finish()
    }
}

/// `retryAssistantCall(produce, policy, signal, callbacks)`。
///
/// 行为（逐条对应 TS 注释）：
/// - 成功响应立即返回；中止是终态、**永不重试**，但若发生在已安排重试之后会报告为失败。
/// - 退避期间的中止也会归一化成 `stopReason: "aborted"` 的 `AssistantMessage`，调用方无需关心
///   取消发生在哪个阶段。
/// - 不可重试错误（含配额/计费耗尽）立即返回，让确定性错误快速失败。
/// - 否则指数退避重试至多 `maxRetries` 次，并在相应时机发出回调。
///
/// # 参数
/// - `produce`：每次尝试调用的闭包（`FnMut`，可携带可变状态）。它返回的
///   `AssistantMessage` 必须以 `stop_reason` 表达结果；本函数不捕获 panic，也不处理闭包
///   内部自己的重试。
/// - `policy`：重试策略。`None` 或 `enabled == false` 都等价于「不重试」。
/// - `signal`：只用于打断退避睡眠；`None` 时退避不可取消。
/// - `callbacks`：可选回调集，见 [`RetryCallbacks`]。
///
/// # 返回
/// 永远返回一条 `AssistantMessage`（不是 `Result`）。终止原因由 `stop_reason` 表达，
/// 调用方要自己区分 `Error`（失败）、`Aborted`（取消）与正常结束。
///
/// # 不变量
/// 进入重试分支时必有 `max_attempts > 0`，也就意味着 `policy` 是 `Some` 且 `enabled`；
/// 所以循环里的 `policy.expect("enabled policy")` 不会触发。
pub async fn retry_assistant_call<F, Fut>(
    mut produce: F,
    policy: Option<&RetryPolicy>,
    signal: Option<&AbortScope>,
    callbacks: Option<&RetryCallbacks>,
) -> AssistantMessage
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = AssistantMessage>,
{
    // 策略缺失或未启用时上限为 0：循环第一次遇到 Error 就会直接返回，不会分配任何重试。
    let max_attempts = match policy {
        Some(policy) if policy.enabled => policy.max_retries,
        _ => 0,
    };

    // `attempt` 只统计「已决定发起」的重试次数；首次调用对应 attempt == 0。
    let mut attempt = 0u64;
    // `(重试序号, 错误文本)`：用来决定 `on_retry_finished` 是否发、以及发什么。
    // 为 `None` 表示还没有发生过重试，此时所有结束路径都不发 finished 回调。
    let mut last_retry: Option<(u64, String)> = None;
    loop {
        let response = produce().await;

        // 中止：终态但不算成功；中止的消息永不重试。
        if response.stop_reason == StopReason::Aborted {
            if let (Some(last), Some(callbacks)) = (&last_retry, callbacks)
                && let Some(finished) = &callbacks.on_retry_finished
            {
                finished(false, last.0, None).await;
            }
            return response;
        }

        // 成功：非 error、非 abort 的响应原样返回。
        if response.stop_reason != StopReason::Error {
            if let (Some(last), Some(callbacks)) = (&last_retry, callbacks)
                && let Some(finished) = &callbacks.on_retry_finished
            {
                finished(true, last.0, None).await;
            }
            return response;
        }

        // 不可重试或预算耗尽：返回最后的错误消息。
        // 顺序是先看预算再看分类，但两者都不做重试，因此不会影响回调参数。
        if attempt >= max_attempts || !is_retryable_assistant_error(&response) {
            if let (Some(last), Some(callbacks)) = (&last_retry, callbacks)
                && let Some(finished) = &callbacks.on_retry_finished
            {
                finished(false, last.0, response.error_message.clone()).await;
            }
            return response;
        }

        attempt += 1;
        // 空消息归一成 "Unknown error"（TS 的 `errorMessage || "Unknown error"`）：
        // on_retry_scheduled 与 last_retry 使用这个归一化文本；on_retry_finished 在
        // 成功/中止响应路径传 `None`，在预算耗尽路径传原始 `response.error_message`。
        let error_message = response
            .error_message
            .clone()
            .filter(|message| !message.is_empty())
            .unwrap_or_else(|| "Unknown error".to_string());
        last_retry = Some((attempt, error_message.clone()));
        // 能走到这里说明 max_attempts > 0，因此策略一定存在且已启用（见函数文档的不变量）。
        let policy = policy.expect("enabled policy");
        let delay_ms = retry_delay_ms(policy.base_delay_ms, policy.max_agent_delay_ms, attempt);
        if let Some(callbacks) = callbacks
            && let Some(scheduled) = &callbacks.on_retry_scheduled
        {
            scheduled(attempt, max_attempts, delay_ms, error_message.clone()).await;
        }

        // 退避期间的中止归一化成与 provider 流中止相同的 `AssistantMessage`，
        // 调用方无需关心取消发生在何时。
        let aborted = match signal {
            Some(scope) => sleep_with_abort(delay_ms, scope).await.is_err(),
            None => {
                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                false
            }
        };
        if aborted {
            if let Some(callbacks) = callbacks
                && let Some(finished) = &callbacks.on_retry_finished
            {
                finished(false, attempt, Some(error_message)).await;
            }
            // 不重新调用 produce：直接把最后一次失败响应改写成 aborted 终态返回。
            // `error_message = None` 对应 TS 的「删掉 errorMessage 键」，序列化时该键消失
            // （`skip_serializing_if = "Option::is_none"`）。
            let mut aborted = response;
            aborted.error_message = None;
            aborted.stop_reason = StopReason::Aborted;
            return aborted;
        }
        if let Some(callbacks) = callbacks
            && let Some(start) = &callbacks.on_retry_attempt_start
        {
            start().await;
        }
    }
}

/// `sleep(ms, signal)`：被中止时返回 `Err`（对应 TS 的 `RetrySleepAbortError`）。
///
/// 先查一次 `is_aborted` 是为了让「已经取消」的调用立即返回，不必先创建并轮询定时器；
/// 检查之后才发生的中止由 `select!` 的 `cancelled()` 分支处理。
///
/// 与 `crate::utils::sleep::sleep` 的区别：这里用 `Err(())`，不构造 `AiError`。TS 的重试
/// 内部也是独立的 `RetrySleepAbortError`，取消原因在这里被丢弃——调用方只关心「被取消」，
/// 最终返回的消息里不带取消原因。
async fn sleep_with_abort(ms: u64, scope: &AbortScope) -> Result<(), ()> {
    if scope.is_aborted() {
        return Err(());
    }
    let token = scope.token();
    tokio::select! {
        _ = tokio::time::sleep(std::time::Duration::from_millis(ms)) => Ok(()),
        _ = token.cancelled() => Err(()),
    }
}

/// `isRetryableAssistantError(message)`。
///
/// 判定顺序（靠前的规则优先）：
/// 1. `stop_reason` 必须是 `Error`，且 `error_message` 存在且非空；
/// 2. 命中 `NON_RETRYABLE_PATTERNS`（配额/计费耗尽等确定性错误）→ 不可重试；
/// 3. 命中 `RETRYABLE_PATTERNS` → 可重试；两条都不命中 → 不重试。
///
/// 正则大小写不敏感。`is_match` 返回 `Err`（例如回溯上限）时按「不命中」处理，因此第 2 步
/// 出错会继续检查第 3 步，第 3 步出错的总体结果是 `false`——分类失败永远不会 panic。
pub fn is_retryable_assistant_error(message: &AssistantMessage) -> bool {
    if message.stop_reason != StopReason::Error {
        return false;
    }
    let Some(error_message) = message.error_message.as_deref() else {
        return false;
    };
    if error_message.is_empty() {
        return false;
    }
    if non_retryable_pattern()
        .is_match(error_message)
        .unwrap_or(false)
    {
        return false;
    }
    retryable_pattern().is_match(error_message).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn error(message: &str) -> AssistantMessage {
        AssistantMessage {
            stop_reason: StopReason::Error,
            error_message: Some(message.to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn retry_delay_grows_exponentially_up_to_the_cap() {
        assert_eq!(retry_delay_ms(500, None, 1), 500);
        assert_eq!(retry_delay_ms(500, None, 2), 1_000);
        assert_eq!(retry_delay_ms(500, None, 3), 2_000);
        assert_eq!(retry_delay_ms(500, Some(1_000), 5), 1_000);
        assert_eq!(retry_delay_ms(0, None, 1), 0);
        // `attempt = 0` 走 `Math.max(0, attempt - 1)`。
        assert_eq!(retry_delay_ms(500, None, 0), 500);
        // 默认上限 60 秒。
        assert_eq!(retry_delay_ms(500, None, 10), 60_000);
        // 超出 `MAX_SAFE_INTEGER` 时饱和为 `Number.MAX_SAFE_INTEGER`。
        assert_eq!(
            retry_delay_ms(u64::MAX, Some(u64::MAX), 1),
            9_007_199_254_740_991
        );
        assert_eq!(
            retry_delay_ms(u64::MAX, Some(u64::MAX), 2),
            9_007_199_254_740_991
        );
    }

    #[test]
    fn retry_delay_matches_js_numeric_semantics() {
        // 小数 baseDelayMs：`delay` 不是安全整数 -> `Number.MAX_SAFE_INTEGER` -> 上限。
        assert_eq!(retry_delay_ms_js(0.5, None, 1.0), 60_000.0);
        assert_eq!(retry_delay_ms_js(0.5, Some(1e9), 1.0), 1e9);
        // 负数配置原样参与 `Math.min`。
        assert_eq!(retry_delay_ms_js(-10.0, None, 1.0), -10.0);
        // 小数 attempt：`2 ** 1.5` 让 delay 不是整数 -> MAX_SAFE_INTEGER（这里上限放开）。
        assert_eq!(
            retry_delay_ms_js(1_000.0, Some(f64::INFINITY), 2.5),
            9_007_199_254_740_991.0
        );
        // 显式上限（包括 0）不会被 `??` 的默认值覆盖。
        assert_eq!(retry_delay_ms_js(500.0, Some(0.0), 1.0), 0.0);
        // 超过 MAX_SAFE_INTEGER 的 base 也退回 MAX_SAFE_INTEGER。
        assert_eq!(
            retry_delay_ms_js(9_007_199_254_740_992.0, Some(f64::INFINITY), 1.0),
            9_007_199_254_740_991.0
        );
        // 安全整数按原值返回。
        assert_eq!(retry_delay_ms_js(250.0, Some(1e6), 3.0), 1_000.0);
    }

    #[test]
    fn classifies_transient_errors() {
        for message in [
            "Overloaded",
            "429 rate limit exceeded",
            "server_busy",
            "The service is temporarily unavailable",
            "Internal error",
            "Provider returned error",
            "stream ended without a message_stop",
            "fetch failed",
            "upstream connect error",
            "socket hang up",
            "request timed out",
            "websocket closed",
            "ResourceExhausted",
            "503",
        ] {
            assert!(
                is_retryable_assistant_error(&error(message)),
                "should retry: {message}"
            );
        }
    }

    #[test]
    fn never_retries_quota_or_billing_errors() {
        for message in [
            "GoUsageLimitError: monthly limit",
            "FreeUsageLimitError",
            "Monthly usage limit reached",
            "insufficient_quota",
            "quota exceeded",
            "billing",
            "subscription_sharing_usage_limit_exceeded",
        ] {
            assert!(
                !is_retryable_assistant_error(&error(message)),
                "should not retry: {message}"
            );
        }
    }

    #[test]
    fn ignores_status_codes_inside_words_and_numbers() {
        assert!(!is_retryable_assistant_error(&error(
            "request id abc429def failed"
        )));
        assert!(!is_retryable_assistant_error(&error("1429500 bytes")));
        assert!(is_retryable_assistant_error(&error("HTTP 502 bad gateway")));
    }

    #[test]
    fn only_error_stop_reasons_are_retried() {
        let mut message = error("overloaded");
        message.stop_reason = StopReason::Stop;
        assert!(!is_retryable_assistant_error(&message));
        let mut message = error("overloaded");
        message.error_message = None;
        assert!(!is_retryable_assistant_error(&message));
    }
}
