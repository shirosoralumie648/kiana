//! `CacheWarmer` + 三个格式化函数。
//!
//! TS: `packages/coding-agent/src/core/cache-warmer.ts`（453 行）。规范：
//! `spec/ca-config-models.md` §2.28（含 §2.28.1-§2.28.9）、§3.7。
//!
//! 在缓存条目过期前，用「输出上限 1 token」重发一次原请求以续命；每次成功预热记为
//! `cache_warm` 用量。
//!
//! ## Rust 侧的可测试性缝
//!
//! - **时钟与定时器**：[`CacheWarmerTimer`] 抽象 `setTimeout` / `clearTimeout` /
//!   `Date.now()`；默认实现是 tokio 定时器，测试用「手动时钟 + 手动触发」。
//! - **会话视图**：TS 只需要 `Pick<SessionManager, "appendUsage" | "getBranch">`；
//!   [`CacheWarmerSession`] 是等价的本地 trait。`SessionManager` 定义在
//!   `kiana-session-core`，下游 crate 无法为「外部 trait + 外部类型」写 impl（孤儿规则），
//!   因此本 crate 直接为 `kiana_session_core::session_manager::SessionManager` 实现。
//! - **模型视图**：TS 只需要 `Pick<ModelRuntime, "streamSimple">`；[`CacheWarmerModels`]
//!   由 [`ModelRuntime`] 直接实现。
//! - **扩展决策**：[`CacheWarmingDecideFn`] 对应 `ExtensionRunner.emitCacheWarmingDecision`
//!   （`kiana-extensions` 适配）。
//!
//! ## 与 TS 的已知差异
//!
//! - TS 的 `catch` 也覆盖「扩展决策回调抛异常」；Rust 的回调类型是 future，panic 不在这里
//!   捕获（适配层负责把扩展错误转成 `event.action`）。`decide` 返回的 future 无错误类型。
//! - `setTimeout(...).unref()` 在 Rust 没有对应物：tokio 任务本身不会吊住进程。
//! - `ModelsSimpleStreamOptions` / `SimpleStreamOptions` / `StreamOptions` 没有 `Clone`
//!   （冻结契约），本文件用 `clone_*` 手写逐字段拷贝。
//!
//! ## 这个文件解决什么问题
//!
//! provider 的 prompt cache 有 TTL：缓存条目过期后，下一次真实请求要为整段 prompt 重新
//! 付「写缓存」或「未缓存输入」的价格。`CacheWarmer` 在 TTL 快到期时，用同一份请求、
//! 把输出上限压到 1 个 token 重放一次，让 provider 侧的缓存条目续期；代价是一次
//! cache read 加 1 个 output token，收益是后续真实请求继续按 cache read 计价。
//! 只有「估计能省下的钱 - 这次刷新的钱」达到 $0.05
//! （[`CACHE_WARMING_MINIMUM_EXPECTED_SAVINGS`]）才真的发请求，否则停在 `stop`；
//! 扩展可以通过 [`CacheWarmingDecideFn`] 覆盖这个结论。
//!
//! ## 执行流程（一次完整生命周期）
//!
//! 1. 真实请求经 `kiana-session` 的 `stream_fn` 进入 [`CacheWarmer::start`]。`start` 依次
//!    校验 mode、可重放性（[`is_replayable`]）、TTL（[`get_prompt_cache_ttl_ms`]）、
//!    延迟（[`get_cache_warming_delay_ms`]），把请求存成内部的 `ActiveRun` 并排期。
//! 2. 定时器在 TTL 的 90% 到期，触发 `Core::refresh_run`。
//! 3. `refresh_run` 先做两组校验（run 仍是当前、mode 允许、`is_current()` 仍为真；
//!    以及定时器没有迟到超过 `refresh_deadline_at`），再 `evaluate` 算经济性，把决策
//!    事件交给扩展回调，只有结论是 warm 才真正重放。
//! 4. 重放成功（`stop_reason` 既不是 `error` 也不是 `aborted`）→
//!    [`CacheWarmerSession::append_usage`]（kind 为 `"cache_warm"`）→ `on_warmed` →
//!    再排下一次，形成循环，直到 run 被替换、被取消，或撞上安全窗口。
//!
//! ## 状态与不变量
//!
//! - `Inner.run` 至多一个：`start` 与 `stop` 都先经 `clear_run` 把它取走，再写入新状态。
//!   `run.id` 单调递增，所有异步续体都用 `run.id == run_id` 判断自己是否还在操作
//!   「同一个 run」。
//! - `run.timer.is_none()` 就是「refreshing」的判据（与 TS 相同）：`refresh_run` 的第一步
//!   就是把 `run.timer` 置 `None`，因此「没有定时器」=「预热请求在飞行中」。
//!   [`CacheWarmingStatus`] 的 `state` 正是由它推出的。
//! - `Inner.inactive` 保存没有 run 时的状态快照（含最近一次 stop 的 reason / decision），
//!   [`CacheWarmer::status`] 在没有 run 时直接返回它。
//!
//! ## 并发与线程
//!
//! - 预热器自身的可变状态只有 `Mutex<Inner>`；会话视图另有
//!   `Arc<Mutex<dyn CacheWarmerSession>>`（`append_usage` 需要 `&mut self`，见下一条）。
//!   `inner` 的锁从不在 `.await` 期间持有（持锁的都是同步
//!   语句或独立的 `{}` 块）。`is_current`、`get_mode`、`decide`、`stream_simple` 这些
//!   外部回调都在锁外调用。
//! - 会话视图是 `Arc<Mutex<dyn CacheWarmerSession>>`（`append_usage` 需要 `&mut self`）。
//!   锁中毒一律用 `into_inner()` 恢复：预热是尽力而为，不该让一次 panic 之后的读写也失败。
//! - 定时器回调返回 [`CacheWarmerFuture`]（`BoxFuture<'static, ()>`），由定时器实现决定
//!   在哪个任务上轮询；默认的 [`TokioCacheWarmerTimer`] 用 `tokio::spawn`。
//!
//! ## 边界与失败模式
//!
//! - 预热是 best-effort，整个公开 API 没有 `Result`：流返回 `Err`、`stop_reason` 为
//!   `error`/`aborted`、`append_usage` 失败，都只影响「这次是否记账」，不会中断循环，
//!   也不会把错误交给调用方。
//! - 两个安全窗口都从 `ActiveRun.started_at` 起算，不是从最近一次预热起算：
//!   streaming 阶段 1 小时（[`MAX_WARMING_AGE_MS`]），idle 阶段 30 分钟
//!   （[`MAX_IDLE_WARMING_AGE_MS`]）。
//! - 还有一个更细的截止时间 `refresh_deadline_at`：排期时写成
//!   `next_warm_at + (ttl_ms - delay_ms) / 2`，即只允许定时器迟到「一半的过期余量」；
//!   超过就放弃本次刷新，不再发请求。
//! - `cancel` 或替换 run 会清掉定时器并取消该 run 的 `CancellationToken`（它被塞进重放
//!   请求的 `signal`）。已经开始的 `refresh` 不会被打断执行，但流结束后（或因取消而
//!   提前结束）的 `validate_run` 会发现 run 已被替换而放弃记账与续排。
//!
//! ## 修改时的影响面
//!
//! - 所有 `reason` 文案与 spec §2.28.8 逐字对齐，是用户可见字符串（`/session` 直接展示）：
//!   改字会影响输出，且其中多数已被 `tests/cache_warmer.rs` 与黄金夹具断言。
//! - [`format_cache_warming_status`] / [`format_cache_warming_usage`] 的输出由黄金夹具
//!   `tests/fixtures/cache_warmer/golden.json`（TS 参考实现生成）锁定。
//! - `clone_*` 必须与 `kiana-ai` 的选项结构同步：结构体字面量列全字段，上游新增字段会
//!   让本文件编译失败，从而提醒补上拷贝。

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures::future::BoxFuture;
use kiana_ai::event_stream::AssistantMessageEventStream;
use kiana_ai::model_catalog::js_to_fixed;
use kiana_ai::models::{ModelsSimpleStreamOptions, calculate_cost};
use kiana_ai::types::{
    CacheRetention, Context, Model, PromptCacheTier, SimpleStreamOptions, StopReason,
    StreamOptions, Usage,
};
use kiana_ai::utils::provider_env::get_provider_env_value;
use kiana_session_core::session_manager::{SessionEntry, SessionManager, UsageEntry};
use tokio_util::sync::CancellationToken;

use crate::models::runtime::ModelRuntime;
use crate::settings::CacheWarmingMode;

/// 流式预热绝不越过真实请求之后这么久。
///
/// 从 `ActiveRun.started_at`（本次真实请求进入 `start` 的时刻）起算，而不是从最近一次
/// 预热起算；`schedule` 用它挡掉「下一次预热已经超出安全窗口」的排期。
pub const MAX_WARMING_AGE_MS: i64 = 60 * 60_000;
/// idle 预热用更短的窗口（续命概率随年龄下降）。
///
/// 同样从 `started_at` 起算；`on_agent_settled` 切到 idle 阶段后立刻用它做一次判定，
/// `schedule` 也会按当前 phase 选择这个上限。
pub const MAX_IDLE_WARMING_AGE_MS: i64 = 30 * 60_000;
/// 只有预期节省至少这么多美元时才发送刷新。
///
/// `evaluate` 用它把 `expected_savings` 折成 `Warm` / `Stop`；`format_cache_warming_economics`
/// 把它格式化成 `$0.050` 作为用户可见文案里的比较阈值。
pub const CACHE_WARMING_MINIMUM_EXPECTED_SAVINGS: f64 = 0.05;
/// agent 空闲时、缓存条目过期前真实请求到来的概率。
///
/// `evaluate` 在 idle 阶段用它替代 streaming 阶段的 1.0：agent 空闲时并不保证还会有
/// 下一次请求，因此把收益按概率打折。
pub const IDLE_CONTINUATION_PROBABILITY: f64 = 0.15;

// ---------------------------------------------------------------------------
// 纯函数
// ---------------------------------------------------------------------------

/// `getCacheWarmingDelayMs(ttlMs)`：在 90% TTL 时刷新，并保留至少 10 秒余量（下限 1 ms）。
///
/// 输入是缓存条目寿命（毫秒），输出是相对「现在」的延迟（毫秒）；`None` 表示这个 TTL
/// 太短、不值得排期（`ttl_ms <= 10_000`），调用方 `start` 会把它当作
/// `cache lifetime unavailable` 停止。
///
/// `min(ttl * 0.9, ttl - 10_000)` 保证刷新点至少比过期早 10 秒；`.max(1.0)` 保证延迟
/// 非零——ttl 略大于 10_000 时 `floor(min(...))` 可能为 0（例如 `ttl = 10_000.5` 时
/// `min` 为 0.5、floor 后为 0），`.max(1.0)` 兜底到 1 ms。
///
/// TS: `core/cache-warmer.ts#getCacheWarmingDelayMs`
pub fn get_cache_warming_delay_ms(ttl_ms: f64) -> Option<f64> {
    if ttl_ms <= 10_000.0 {
        return None;
    }
    Some((ttl_ms * 0.9).min(ttl_ms - 10_000.0).floor().max(1.0))
}

/// `getPromptCacheTtlMs(model, options)`：请求写入的缓存条目寿命（毫秒）。
///
/// retention 未显式指定时，`KIANA_CACHE_RETENTION=long`（经 `getProviderEnvValue` 查
/// `options.env` 再进程环境）→ long，否则 short。
///
/// 返回值是「模型目录里的秒数 × 1000」。三种情况返回 `None`，调用方必须区分：
///
/// - `cache_retention` 显式为 [`CacheRetention::None`]（请求自己关闭了缓存）→
///   `start` 报 `request disabled prompt caching`；
/// - 模型没有 `prompt_cache` 表 → `start` 报 `cache lifetime unavailable`；
/// - 有表但该档位没有配置秒数 → 同样报 `cache lifetime unavailable`。
///
/// 环境变量比较是精确的字符串 `"long"`（区分大小写）；其他任何取值都按 short 处理。
///
/// TS: `core/cache-warmer.ts#getPromptCacheTtlMs`
pub fn get_prompt_cache_ttl_ms(model: &Model, options: Option<&StreamOptions>) -> Option<f64> {
    let retention = options
        .and_then(|options| options.cache_retention)
        .unwrap_or(
            if get_provider_env_value(
                "KIANA_CACHE_RETENTION",
                options.and_then(|options| options.env.as_ref()),
            )
            .as_deref()
                == Some("long")
            {
                CacheRetention::Long
            } else {
                CacheRetention::Short
            },
        );
    if retention == CacheRetention::None {
        return None;
    }
    let tier = match retention {
        CacheRetention::Long => PromptCacheTier::Long,
        _ => PromptCacheTier::Short,
    };
    model
        .prompt_cache
        .as_ref()?
        .get(&tier)
        .copied()
        .map(|seconds| seconds * 1000.0)
}

/// `isReplayable(model, options)`：重放是否安全。
///
/// Anthropic 且启用了 reasoning 时，只有 `compat.forceAdaptiveThinking` 才安全（否则
/// `budget_tokens` 由 `max_tokens` 推导，重放会改变缓存键）。
///
/// 这里的「安全」只针对缓存语义：`refresh_run` 重放时会把 `max_tokens` 改成 `1`，如果
/// provider 会依据 `max_tokens` 推导其它请求字段，重放就不再是「原请求的等价重发」，
/// 缓存键可能变化，续命也就失去意义。代码对非 Anthropic、或没有启用 reasoning 的请求
/// 直接放行；其他 provider 的缓存键是否受 `max_tokens` 影响未在仓库文档中说明（待核实）——
/// spec §2.28.4 只解释了 Anthropic + reasoning 时 `budget_tokens` 由 `max_tokens` 推导的情形。
///
/// TS: `core/cache-warmer.ts#isReplayable`
pub fn is_replayable(model: &Model, options: Option<&SimpleStreamOptions>) -> bool {
    if options.is_none_or(|options| options.reasoning.is_none())
        || model.api != "anthropic-messages"
    {
        return true;
    }
    model
        .compat
        .as_ref()
        .and_then(|compat| compat.get("forceAdaptiveThinking"))
        .and_then(serde_json::Value::as_bool)
        == Some(true)
}

/// `lastPromptTokens(entries)`：分支上最近一条 assistant 消息的 prompt 大小。
///
/// 倒序扫描（`get_branch` 返回根→叶，因此最后一条 assistant 消息在末尾方向），
/// 返回 `usage.input + usage.cache_read + usage.cache_write`——即上一轮请求实际占用的
/// prompt token 总量，`evaluate` 拿它当下一轮请求规模的代理。
///
/// 分支上没有 assistant 消息（例如新会话只有 user 消息）时返回 `0`，`evaluate` 会据此
/// 判定经济性不可用（`economics_available = false`）。
fn last_prompt_tokens(entries: &[SessionEntry]) -> u64 {
    for entry in entries.iter().rev() {
        if let SessionEntry::Message(message_entry) = entry
            && let Some(message) = message_entry.message.as_assistant()
        {
            return message.usage.input + message.usage.cache_read + message.usage.cache_write;
        }
    }
    0
}

/// `price()` 的 token 组合。
#[derive(Debug, Clone, Copy, Default)]
struct PriceTokens {
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
}

/// `price(model, tokens)`：用完整 `Usage`（其余为 0）调用 `calculateCost`。
///
/// 只构造四个 token 字段，其余（含 `total_tokens` 与 `cost`）保持默认；`calculate_cost`
/// 会就地写回 `cost`，这里只取 `total`，临时 `Usage` 用完即弃。
///
/// 注意 `calculate_cost` 的分档判断用 `input + cache_read + cache_write`：把 token 放进
/// 哪个字段会同时影响单价档位的选择，所以调用点必须按 `calculate_cost` 期望的字段布局填充
/// （`evaluate` 里的三种价格就是按这个规则挑字段的）。
fn price(model: &Model, tokens: PriceTokens) -> f64 {
    let mut usage = Usage {
        input: tokens.input,
        output: tokens.output,
        cache_read: tokens.cache_read,
        cache_write: tokens.cache_write,
        ..Usage::default()
    };
    calculate_cost(model, &mut usage).total
}

// ---------------------------------------------------------------------------
// 类型
// ---------------------------------------------------------------------------

/// `CacheWarmingAction`。
///
/// 本文件内部只把它当作「要不要发这次重放」的开关；对外的另一个用途是扩展决策的返回值
/// （[`CacheWarmingDecideFn`]），扩展用 `"warm"` / `"stop"` 两个字面量表达覆盖。
///
/// TS: `core/cache-warmer.ts#CacheWarmingAction`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheWarmingAction {
    /// `"warm"`。
    Warm,
    /// `"stop"`。
    Stop,
}

impl CacheWarmingAction {
    /// TS 的字面量。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Warm => "warm",
            Self::Stop => "stop",
        }
    }
}

/// 预热阶段：`"streaming" | "idle"`。
///
/// `start` 后的 run 一律从 `Streaming` 开始；`on_agent_settled` 在 mode 不是 `Streaming`
/// 时（idle 或 off 都算）把它切成 `Idle`——`Off` 并不是「允许继续保温」，只是切到
/// `Idle` 后由后续 `validate_run` / `schedule` 拦截。阶段影响两件事：`evaluate` 的
/// 续命概率（1.0 vs 0.15）与
/// `schedule` 选用的安全窗口（1 小时 vs 30 分钟），也出现在用户可见文案里。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheWarmingPhase {
    /// agent run 仍在进行。
    Streaming,
    /// agent 已空闲。
    Idle,
}

impl CacheWarmingPhase {
    /// TS 的字面量。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Streaming => "streaming",
            Self::Idle => "idle",
        }
    }
}

/// `CacheWarmingDecision`：一次 warm-or-stop 决策的输入与结果。
///
/// TS: `core/cache-warmer.ts#CacheWarmingDecision`
#[derive(Debug, Clone, PartialEq)]
pub struct CacheWarmingDecision {
    /// 阶段。
    pub phase: CacheWarmingPhase,
    /// 本次刷新的价格：prompt 的 cache read + 1 个输出 token。
    pub warm_cost: f64,
    /// 缓存丢失时下一次真实请求的额外价格。
    pub miss_cost: f64,
    /// 缓存条目过期前真实请求到来的估计概率。
    pub continuation_probability: f64,
    /// `continuationProbability * missCost - warmCost`。
    pub expected_savings: f64,
    /// prompt 大小或模型价格未知时为 false。
    pub economics_available: bool,
    /// `expectedSavings` 至少 $0.05 时为 `"warm"`。
    pub action: CacheWarmingAction,
}

/// `CacheWarmingDecisionEvent`：每次刷新前发给扩展的决策事件。
///
/// TS: `core/cache-warmer.ts#CacheWarmingDecisionEvent`
#[derive(Debug, Clone, PartialEq)]
pub struct CacheWarmingDecisionEvent {
    /// 本次刷新价格。
    pub warm_cost: f64,
    /// 缓存丢失的额外价格。
    pub miss_cost: f64,
    /// 续命概率。
    pub continuation_probability: f64,
    /// kiana 的决定。
    pub action: CacheWarmingAction,
}

impl CacheWarmingDecisionEvent {
    /// 事件的 `type` 字段（扩展按它分派）。
    pub fn event_type(&self) -> &'static str {
        "cache_warming_decision"
    }
}

/// `CacheWarmingDecisionEventResult`：扩展可以覆盖 `action`。
///
/// TS: `core/cache-warmer.ts#CacheWarmingDecisionEventResult`
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CacheWarmingDecisionEventResult {
    /// `Some` 时覆盖 kiana 的决定。
    pub action: Option<CacheWarmingAction>,
}

/// `CacheWarmingStatus.state`。
///
/// TS: `core/cache-warmer.ts#CacheWarmingStatus`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CacheWarmingState {
    /// `"inactive"`。
    #[default]
    Inactive,
    /// `"scheduled"`：定时器已武装。
    Scheduled,
    /// `"refreshing"`：预热请求在飞行中。
    Refreshing,
}

impl CacheWarmingState {
    /// TS 的字面量。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inactive => "inactive",
            Self::Scheduled => "scheduled",
            Self::Refreshing => "refreshing",
        }
    }
}

/// `CacheWarmingStatus`：`/session` 展示的状态。
///
/// 由 [`CacheWarmer::status`] 现算：它要么返回 `Inner.inactive`（没有 run 时），要么用
/// 当前 run 的快照 + 重新 `evaluate` 的决策拼出来。`reason` 是 spec §2.28.8 里逐字的英文
/// 原因串（不是本地化文案），[`format_cache_warming_status`] 直接把它嵌进输出。
///
/// TS: `core/cache-warmer.ts#CacheWarmingStatus`
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CacheWarmingStatus {
    /// 状态。
    pub state: CacheWarmingState,
    /// 没有排期的原因。
    pub reason: Option<String>,
    /// 下一次预热的毫秒时间戳。
    pub next_warm_at: Option<i64>,
    /// 待执行的决策，或导致停止的决策。
    pub decision: Option<CacheWarmingDecision>,
    /// 扩展是否改过 `decision.action`。
    pub extension_override: Option<bool>,
}

/// `CacheWarmRequest`：要保温的请求，按原样发送。
///
/// 三个字段都是进入 `start` 那一刻的快照。`refresh_run` 每次重放前会逐字段 clone
/// `options`，再覆盖三个字段：`max_tokens = Some(1)`（只让 provider 续命，不产生真正的
/// 输出）、`max_retries = Some(0)`（重放不重试）、`signal = 该 run 的 CancellationToken`
/// （因此原请求的 `signal` 不会被预热取消）。
///
/// TS: `core/cache-warmer.ts#CacheWarmRequest`
pub struct CacheWarmRequest {
    /// 模型。
    pub model: Model,
    /// 上下文。
    pub context: Context,
    /// 请求选项。
    pub options: ModelsSimpleStreamOptions,
}

// ---------------------------------------------------------------------------
// 可注入的视图 / 定时器
// ---------------------------------------------------------------------------

/// `Pick<SessionManager, "appendUsage" | "getBranch">` 的等价 trait。
///
/// 测试用 fake；直接持有 `SessionManager` 的宿主可用下方的 [`SessionManager`] impl；
/// 本仓库的生产装配（`kiana-session/src/sdk.rs`）使用 `CacheWarmerSessionSlot` 实现该
/// trait。`append_usage` 的
/// `None` 表示 TS 里 `appendUsage` 抛出异常：`refresh` 的 `try/catch` 会吞掉它，
/// 不调用 `onWarmed`，并继续调度下一次预热。
///
/// 契约：
///
/// - `append_usage` 返回**持久化后的** `UsageEntry`；`refresh_run` 会把这个值原样交给
///   `on_warmed`（调用方看到的必须是同一条记录，不能是另造的副本）。
/// - `get_branch` 的顺序必须是根在前、叶在后：`evaluate` 倒着找最近一条 assistant 消息。
/// - 两个方法都通过 `Arc<Mutex<…>>` 调用（见 [`SharedCacheWarmerSession`]），因此
///   `get_branch` 持锁期间会阻塞同一会话上的 `append_usage`。
pub trait CacheWarmerSession: Send {
    /// `appendUsage(kind, provider, model, usage, note)`；失败时返回 `None`。
    fn append_usage(
        &mut self,
        kind: &str,
        provider: &str,
        model: &str,
        usage: &Usage,
        note: Option<&str>,
    ) -> Option<UsageEntry>;
    /// `getBranch()`。
    fn get_branch(&self) -> Vec<SessionEntry>;

    /// 分支上最近一条 assistant 消息的 prompt token 总数。
    ///
    /// `evaluate` 每次查询都会调用它（`status()` 也会）。默认实现从 `get_branch()`
    /// 计算，会克隆整条分支；能直接访问条目的实现应覆写成 O(1) 的叶子回溯。
    fn last_prompt_tokens(&self) -> u64 {
        last_prompt_tokens(&self.get_branch())
    }
}

/// 真实 `SessionManager` 的适配（TS `sdk.ts` 直接把 `SessionManager` 传给 `CacheWarmer`）。
///
/// - `appendUsage` 的 `SessionManagerError` 对应 TS 的异常：折叠成 `None`，由
///   `refresh` 的 catch 路径吞掉（不写 `onWarmed`、继续调度）。错误详情在这里丢弃，
///   与 TS 的 `catch {}` 不看错误内容一致。
/// - `getBranch()` 不传参数：`get_branch(None)` 从当前
///   leaf 回溯到根，返回根→叶顺序。
impl CacheWarmerSession for SessionManager {
    fn append_usage(
        &mut self,
        kind: &str,
        provider: &str,
        model: &str,
        usage: &Usage,
        note: Option<&str>,
    ) -> Option<UsageEntry> {
        SessionManager::append_usage(self, kind, provider, model, usage.clone(), note).ok()
    }

    fn get_branch(&self) -> Vec<SessionEntry> {
        SessionManager::get_branch(self, None)
    }

    fn last_prompt_tokens(&self) -> u64 {
        SessionManager::last_prompt_tokens(self)
    }
}

/// 共享会话视图（`Arc<Mutex<...>>`，因为 `appendUsage` 需要 `&mut self`）。
///
/// 生产装配见 `kiana-session/src/sdk.rs`：先用一个「槽」占位，`AgentSession` 构造完成后
/// 再把会话注入槽位。槽位让 `Core` 不直接持有 `AgentSession`，这样形成的
/// `Core → CacheWarmer → 槽 → AgentSession → Core` 引用环可在会话替换/销毁时由
/// `release_cache_warmer_session`（`agent_session_runtime.rs`）清空槽位来打断；
/// 注入本身只是完成装配。
pub type SharedCacheWarmerSession = Arc<Mutex<dyn CacheWarmerSession>>;

/// `Pick<ModelRuntime, "streamSimple">` 的等价 trait。
///
/// 实现必须 `Send + Sync`（`Core` 会被多个任务共享）。约定 `stream_simple` 只负责
/// **启动**请求并返回流句柄，真正的等待由 `refresh_run` 的 `stream.result().await` 完成；
/// 若实现内部同步阻塞到响应结束，会占住轮询它的任务。
pub trait CacheWarmerModels: Send + Sync {
    /// `streamSimple(model, context, options)`。
    fn stream_simple(
        &self,
        model: &Model,
        context: &Context,
        options: ModelsSimpleStreamOptions,
    ) -> AssistantMessageEventStream;
}

impl CacheWarmerModels for ModelRuntime {
    fn stream_simple(
        &self,
        model: &Model,
        context: &Context,
        options: ModelsSimpleStreamOptions,
    ) -> AssistantMessageEventStream {
        ModelRuntime::stream_simple(self, model, context, options)
    }
}

/// 预热刷新的回调 future。
///
/// `'static` 是硬约束：定时器实现会把它移进独立任务（默认实现用 `tokio::spawn`），
/// 因此 future 不能借用 `CacheWarmer`，只能自己持有 `Arc` 克隆。
pub type CacheWarmerFuture = BoxFuture<'static, ()>;

/// 定时器到期时执行的回调。
///
/// `FnOnce`：一次排期最多执行一次。取消语义由 [`CacheWarmerTimerHandle::clear`] 提供。
pub type CacheWarmerTimerCallback = Box<dyn FnOnce() -> CacheWarmerFuture + Send>;

/// `clearTimeout` 的句柄。
pub trait CacheWarmerTimerHandle: Send + Sync {
    /// 取消尚未触发的回调。
    ///
    /// 只保证「回调不会开始」：如果回调已经启动、正 await 在内部 future 上，`clear`
    /// 不会中断它。所以 `clear_run` 清掉定时器之后还会 `controller.cancel()`——单靠
    /// `clear` 停不下已经开始的刷新。
    /// 重复调用应当是安全的（默认实现里 `CancellationToken::cancel` 幂等）。
    fn clear(&self);
}

/// `setTimeout` + `Date.now()` 的注入点。
///
/// 生产实现是 [`TokioCacheWarmerTimer`]；测试用「手动时钟 + 手动触发」的 fake，
/// 因此这里不能用 `tokio::time` 的 API 直接写死。
pub trait CacheWarmerTimer: Send + Sync {
    /// 安排一次回调（毫秒），返回可取消的句柄。
    ///
    /// 延迟一定是非负的（`schedule` 已经 `.max(0)`），但实现仍应自行兜底。
    fn set_timeout(
        &self,
        delay_ms: i64,
        callback: CacheWarmerTimerCallback,
    ) -> Box<dyn CacheWarmerTimerHandle>;
    /// `Date.now()`。
    ///
    /// 毫秒时间戳。它是本文件唯一的「当前时间」来源：`next_warm_at`、`started_at`、
    /// `refresh_deadline_at` 都是同一时间基准下的值，测试可以整体平移。
    fn now(&self) -> i64;
}

/// 默认定时器：tokio `sleep` + `CancellationToken`。
///
/// 每次 `set_timeout` 在 tokio 运行时内创建一个独立的 `CancellationToken` 与一个
/// `Handle::spawn` 任务，用 `select!` 在「睡够」与「被 clear」之间二选一；无运行时
/// （同步上下文）时不创建任务，回调永不触发。延迟先 `.max(0)` 再转 `u64`；
/// `unwrap_or(u64::MAX)` 是防御分支（非负 `i64` 的转换不会失败）。
///
/// `now()` 用 `SystemTime` 计算毫秒时间戳；系统时钟早于 `UNIX_EPOCH` 时返回 0——这会让
/// 所有基于它的截止判断一起偏移，属于异常时钟下的降级行为。
#[derive(Debug, Default)]
pub struct TokioCacheWarmerTimer;

struct TokioTimerHandle {
    cancellation: CancellationToken,
}

impl CacheWarmerTimerHandle for TokioTimerHandle {
    fn clear(&self) {
        self.cancellation.cancel();
    }
}

impl CacheWarmerTimer for TokioCacheWarmerTimer {
    fn set_timeout(
        &self,
        delay_ms: i64,
        callback: CacheWarmerTimerCallback,
    ) -> Box<dyn CacheWarmerTimerHandle> {
        let cancellation = CancellationToken::new();
        let token = cancellation.clone();
        let delay = Duration::from_millis(u64::try_from(delay_ms.max(0)).unwrap_or(u64::MAX));
        // 没有 tokio 运行时（同步上下文）时回调永不触发；这与 TS 里进程没有事件循环等价。
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                tokio::select! {
                    () = tokio::time::sleep(delay) => callback().await,
                    () = token.cancelled() => {}
                }
            });
        }
        Box::new(TokioTimerHandle { cancellation })
    }

    fn now(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_millis() as i64)
    }
}

/// 扩展决策回调：`decide(event) -> Promise<action>`。
///
/// 返回的 future 没有错误类型，所以扩展失败必须由适配层内部消化并回落到
/// `event.action`（`kiana-session/src/sdk.rs` 的 `decide` 适配用 `Result::ok` +
/// `unwrap_or(default_action)` 消化 `emit_cache_warming_decision` 的 `DispatchError`）。
/// panic 同样不在这里捕获：
/// 默认 tokio 定时器路径下，panic 会让那个 `tokio::spawn` 任务结束，而 `refresh_run`
/// 在调用前已把 `run.timer` 置 `None`，该 run 会停在 `refreshing`，直到下一次
/// `start` / `cancel` / `stop` 把它替换掉。
pub type CacheWarmingDecideFn =
    Arc<dyn Fn(CacheWarmingDecisionEvent) -> BoxFuture<'static, CacheWarmingAction> + Send + Sync>;

/// 默认决策：原样采纳 kiana 算出的 action（TS 的 `async event => event.action`）。
fn default_decide() -> CacheWarmingDecideFn {
    Arc::new(|event| Box::pin(async move { event.action }))
}

/// `CacheWarmer` 的构造选项。
///
/// 全部依赖都是注入的：生产装配在 `kiana-session/src/sdk.rs#create_agent_session`，
/// 测试装配在 `tests/cache_warmer.rs`（fake session + 手动定时器）。`get_mode` 是
/// 函数而不是值，因为 `Core::mode()` 每次判断都会重新调用它——设置改变后不需要重建
/// warmer（`AgentSession::set_cache_warming_mode` 负责再调 `on_mode_changed` 对账）。
pub struct CacheWarmerOptions {
    /// `Pick<ModelRuntime, "streamSimple">`。
    pub models: Arc<dyn CacheWarmerModels>,
    /// `Pick<SessionManager, "appendUsage" | "getBranch">`。
    pub session: SharedCacheWarmerSession,
    /// 当前 `cacheWarming` 模式（每次决策时读取）。
    pub get_mode: Arc<dyn Fn() -> CacheWarmingMode + Send + Sync>,
    /// 扩展决策回调；缺省是直接用 kiana 的决定。
    pub decide: Option<CacheWarmingDecideFn>,
    /// 定时器；缺省是 [`TokioCacheWarmerTimer`]。
    pub timer: Option<Arc<dyn CacheWarmerTimer>>,
}

// ---------------------------------------------------------------------------
// 内部状态
// ---------------------------------------------------------------------------

/// 一个正在保温的 run：请求快照 + 时间参数 + 取消令牌 + 排期状态。
///
/// 字段分三组：
///
/// - **请求快照**：`model` / `context` / `options`，`refresh_run` 每次重放时 clone 一份；
///   请求本身不会因为会话后续变化而被更新。
/// - **时间参数**：`started_at` 在 `start` 时写入后不再变化，两个安全窗口都从它起算；
///   `ttl_ms` / `delay_ms` 在 `start` 时由模型与 retention 算出，之后也不再重算；
///   `next_warm_at` 与 `refresh_deadline_at` 由每次 `schedule` 覆盖。
/// - **状态**：`id` 用于识别「还是不是同一个 run」，`phase` 区分 streaming/idle，
///   `extension_override` 记录最近一次决策是否被扩展改过，`timer` 是已武装的定时器
///   （`None` 表示正在刷新，见文件头的不变量），`controller` 是重放请求的取消令牌。
///
/// `is_current` 是外部闭包（生产上是「模型与消息前缀都没变」的检查），`status` 与
/// `validate_run` 在锁外调用它。
struct ActiveRun {
    id: u64,
    model: Model,
    context: Context,
    options: ModelsSimpleStreamOptions,
    is_current: Arc<dyn Fn() -> bool + Send + Sync>,
    ttl_ms: f64,
    delay_ms: f64,
    refresh_deadline_at: i64,
    started_at: i64,
    controller: CancellationToken,
    phase: CacheWarmingPhase,
    next_warm_at: i64,
    extension_override: bool,
    timer: Option<Box<dyn CacheWarmerTimerHandle>>,
}

/// `Core.inner` 的内容：可变状态全在这里。
///
/// - `run`：当前保温中的 run；`None` 表示没有排期。
/// - `inactive`：没有 run 时 `status()` 的返回值。`stop()` 会把这次停止的 reason /
///   decision / extension_override 写进它，因此「刚停下」的状态仍能显示停止原因与决策。
/// - `next_run_id`：`start` 时自增，用来区分新旧 run；从 0 开始，所以第一个 run 的 id 是 1。
/// - `on_warmed`：预热成功后的回调，只保存最后一次 `set_on_warmed` 的设置。
#[derive(Default)]
struct Inner {
    run: Option<ActiveRun>,
    inactive: CacheWarmingStatus,
    next_run_id: u64,
    on_warmed: Option<Arc<dyn Fn(UsageEntry) + Send + Sync>>,
}

/// 预热器的全部状态与依赖；[`CacheWarmer`] 只是它的一层 `Arc` 包装。
///
/// 拆成两层是为了让定时器回调能持有 `Arc<Core>` 并把 future 变成 `'static`，同时让
/// `Clone` 出来的 `CacheWarmer` 共享同一份 `inner`。
struct Core {
    models: Arc<dyn CacheWarmerModels>,
    session: SharedCacheWarmerSession,
    get_mode: Arc<dyn Fn() -> CacheWarmingMode + Send + Sync>,
    decide: CacheWarmingDecideFn,
    timer: Arc<dyn CacheWarmerTimer>,
    inner: Mutex<Inner>,
}

/// 取 `inner` 锁，并把中毒状态当作正常值继续用。
///
/// 预热是尽力而为：即使某次调用 panic 打断了临界区，后续的状态读取/写入也不该跟着失败
/// （否则一次 panic 会让整个 warmer 永久不可用）。本文件访问 `inner` 的所有地方都走这个
/// 函数，没有直接 `.lock().unwrap()` 的路径。
fn lock(inner: &Mutex<Inner>) -> MutexGuard<'_, Inner> {
    inner
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 保持一个 prompt 缓存条目存活的预热器。
///
/// `start` 会替换之前的 run；预热请求永远不会延长固定的安全窗口。
///
/// `Clone` 只克隆内部的 `Arc<Core>`：所有克隆共享同一个状态机，任一句柄的
/// `start` / `cancel` / `on_agent_settled` 对其它句柄立即可见。
///
/// TS: `core/cache-warmer.ts#CacheWarmer`
#[derive(Clone)]
pub struct CacheWarmer {
    core: Arc<Core>,
}

impl CacheWarmer {
    /// 构造（对应 TS 构造函数）。
    ///
    /// 初始状态是 `inactive` + reason `"waiting for first request"`（spec 逐字），在 mode
    /// 不为 off 时第一次 `start` 之前 `status()` 返回的就是它（mode 为 off 时 `status()`
    /// 直接返回 `cache warming disabled`）。`decide` 缺省为原样采纳 kiana 的 action，
    /// `timer` 缺省为 [`TokioCacheWarmerTimer`]。
    pub fn new(options: CacheWarmerOptions) -> Self {
        CacheWarmer {
            core: Arc::new(Core {
                models: options.models,
                session: options.session,
                get_mode: options.get_mode,
                decide: options.decide.unwrap_or_else(default_decide),
                timer: options
                    .timer
                    .unwrap_or_else(|| Arc::new(TokioCacheWarmerTimer)),
                inner: Mutex::new(Inner {
                    run: None,
                    inactive: CacheWarmingStatus {
                        state: CacheWarmingState::Inactive,
                        reason: Some("waiting for first request".to_string()),
                        ..Default::default()
                    },
                    next_run_id: 0,
                    on_warmed: None,
                }),
            }),
        }
    }

    /// `onWarmed`：每次成功预热后收到持久化的 usage 条目。
    ///
    /// 设置会整体替换旧回调（不是追加），传 `None` 清除。回调收到的是
    /// `append_usage` 返回的同一条 `UsageEntry`。`AgentSession::new` 用它把条目转成
    /// `EntryAppended` 事件；`AgentSession::dispose` 会先传 `None` 再 `cancel`，
    /// 避免关闭后还有回调写事件。
    pub fn set_on_warmed(&self, callback: Option<Arc<dyn Fn(UsageEntry) + Send + Sync>>) {
        lock(&self.core.inner).on_warmed = callback;
    }

    /// `get status()`。
    ///
    /// 只读：不修改 `run`、不清定时器——即使 mode 已经是 off，它也直接返回一个临时状态，
    /// 真正的清理由 `on_mode_changed` 负责。判定顺序与 spec 逐字一致：
    ///
    /// 1. mode 为 off → 固定 `inactive` + `cache warming disabled`；
    /// 2. 没有 run → 返回最近一次 `stop` 留下的 `Inner.inactive`；
    /// 3. `is_current()` 为假 → `inactive` + `conversation context changed`；
    /// 4. 重新 `evaluate`；经济性不可用且当前不在刷新 → `inactive` +
    ///    `cache economics unavailable`；
    /// 5. 否则按 `run.timer.is_none()` 报 `refreshing` / `scheduled`。
    ///
    /// 第 4 步意味着每次查询都会锁住会话并调用 `get_branch()`：决策不缓存，`status()`
    /// 每次都会重新计算它。
    ///
    /// TS: `core/cache-warmer.ts#CacheWarmer.status`
    pub fn status(&self) -> CacheWarmingStatus {
        let core = &self.core;
        if core.mode() == CacheWarmingMode::Off {
            return CacheWarmingStatus {
                state: CacheWarmingState::Inactive,
                reason: Some("cache warming disabled".to_string()),
                ..Default::default()
            };
        }
        // 锁内只做拷贝：`is_current` 与 `evaluate` 都会回到外部实现（可能锁会话），
        // 不能持着 `inner` 调用它们。
        struct Snapshot {
            model: Model,
            phase: CacheWarmingPhase,
            next_warm_at: i64,
            extension_override: bool,
            refreshing: bool,
            is_current: Arc<dyn Fn() -> bool + Send + Sync>,
        }
        let snapshot = {
            let inner = lock(&core.inner);
            let Some(run) = inner.run.as_ref() else {
                return inner.inactive.clone();
            };
            Snapshot {
                model: run.model.clone(),
                phase: run.phase,
                next_warm_at: run.next_warm_at,
                extension_override: run.extension_override,
                refreshing: run.timer.is_none(),
                is_current: Arc::clone(&run.is_current),
            }
        };
        if !(snapshot.is_current)() {
            return CacheWarmingStatus {
                state: CacheWarmingState::Inactive,
                reason: Some("conversation context changed".to_string()),
                ..Default::default()
            };
        }
        let decision = core.evaluate(&snapshot.model, snapshot.phase);
        if !decision.economics_available && !snapshot.refreshing {
            return CacheWarmingStatus {
                state: CacheWarmingState::Inactive,
                reason: Some("cache economics unavailable".to_string()),
                ..Default::default()
            };
        }
        CacheWarmingStatus {
            state: if snapshot.refreshing {
                CacheWarmingState::Refreshing
            } else {
                CacheWarmingState::Scheduled
            },
            reason: None,
            next_warm_at: Some(snapshot.next_warm_at),
            decision: Some(decision),
            extension_override: Some(snapshot.extension_override),
        }
    }

    /// `start(request, isCurrent)`：保温 `request` 写入的缓存条目，直到 `is_current` 变假。
    ///
    /// 真实请求进入 `stream_fn` 时调用（见 `kiana-session/src/sdk.rs`），所以第一步就是
    /// `clear_run()`：上一个 run 的定时器被清掉、在飞行中的重放被取消。随后按顺序做四项
    /// 校验，任一项失败都会 `stop(reason)` 留下一条 `inactive` 状态后返回（不是静默忽略，
    /// `status()` 要能解释为什么没在保温）。
    ///
    /// `is_current` 只是存进 run，`start` 内部不求值；真正的检查发生在 `status` 与
    /// `validate_run`（定时器到期时、以及每次 await 之后）。
    ///
    /// 注意 options 的三层嵌套：`request.options` 是 `ModelsSimpleStreamOptions`，它的
    /// `.base` 是 `SimpleStreamOptions`（`is_replayable` 用），再往下的 `.base.base` 是
    /// `StreamOptions`（TTL 与 `cache_retention` 用）。
    ///
    /// 这里算出的 `ttl_ms` / `delay_ms` 会一直用到这个 run 结束；模型或 retention 中途
    /// 变化不会重算，只有下一次真实请求（下一次 `start`）才会。
    ///
    /// 没有返回值：排期结果通过 [`CacheWarmer::status`] 观察，成功预热通过
    /// [`CacheWarmer::set_on_warmed`] 的回调与会话里的 usage 条目体现。
    ///
    /// TS: `core/cache-warmer.ts#CacheWarmer.start`
    pub fn start(
        &self,
        request: CacheWarmRequest,
        is_current: Arc<dyn Fn() -> bool + Send + Sync>,
    ) {
        let core = &self.core;
        core.clear_run();
        // 1. mode：关闭时不排期。
        if core.mode() == CacheWarmingMode::Off {
            core.stop("cache warming disabled", None, None);
            return;
        }
        // 2. 可重放性：重放要把 max_tokens 改成 1，某些 reasoning 配置下这会改变缓存键。
        if !is_replayable(&request.model, Some(&request.options.base)) {
            core.stop("request cannot be replayed safely", None, None);
            return;
        }
        // 3. TTL：拿不到缓存寿命就无法计算刷新点。区分「请求自己关了缓存」与
        //    「模型没配 / 档位缺失」，两者给出不同的 reason。
        let Some(ttl_ms) =
            get_prompt_cache_ttl_ms(&request.model, Some(&request.options.base.base))
        else {
            let reason = if request.options.base.base.cache_retention == Some(CacheRetention::None)
            {
                "request disabled prompt caching"
            } else {
                "cache lifetime unavailable"
            };
            core.stop(reason, None, None);
            return;
        };
        // 4. 延迟：TTL 太短（<= 10 秒）时没有安全的刷新点。
        let Some(delay_ms) = get_cache_warming_delay_ms(ttl_ms) else {
            core.stop("cache lifetime unavailable", None, None);
            return;
        };
        // 建立新的 ActiveRun：id 自增，时间参数在此固化，phase 从 streaming 开始；
        // next_warm_at / refresh_deadline_at / timer 留给 schedule 填写。
        let started_at = core.timer.now();
        let run_id = {
            let mut inner = lock(&core.inner);
            inner.next_run_id += 1;
            let run_id = inner.next_run_id;
            inner.run = Some(ActiveRun {
                id: run_id,
                model: request.model,
                context: request.context,
                options: request.options,
                is_current,
                ttl_ms,
                delay_ms,
                refresh_deadline_at: 0,
                started_at,
                controller: CancellationToken::new(),
                phase: CacheWarmingPhase::Streaming,
                next_warm_at: 0,
                extension_override: false,
                timer: None,
            });
            run_id
        };
        core.schedule(run_id);
    }

    /// `onAgentSettled()`：agent run 结束。
    ///
    /// 两种结局：mode 为 streaming 时，保温只服务于「进行中的 run」，run 结束就该停
    /// （`agent run settled`）；否则把 phase 切成 idle，改用 0.15 的续命概率与 30 分钟
    /// 窗口，并立即按新窗口检查一次能否继续。
    ///
    /// 切换 phase 前会再按 `run.id` 校验：读快照与写回之间锁被释放过，run 可能已经被
    /// `start` 替换，此时直接返回、不碰新 run。
    ///
    /// 这里不重新 `schedule`：timer 为 `Some` 时原定时器仍按旧时间点到期，到期后
    /// `refresh_run` 会按 idle 规则重新判断；正在刷新（timer 为 `None`）时没有定时器，
    /// 由本次刷新结束后的 `schedule` 按 idle 规则重排。
    ///
    /// TS: `core/cache-warmer.ts#CacheWarmer.onAgentSettled`
    pub fn on_agent_settled(&self) {
        let core = &self.core;
        let snapshot = {
            let inner = lock(&core.inner);
            inner
                .run
                .as_ref()
                .map(|run| (run.id, run.started_at, run.next_warm_at))
        };
        let Some((run_id, started_at, next_warm_at)) = snapshot else {
            return;
        };
        if core.mode() == CacheWarmingMode::Streaming {
            core.stop("agent run settled", None, None);
            return;
        }
        {
            let mut inner = lock(&core.inner);
            match inner.run.as_mut() {
                Some(run) if run.id == run_id => run.phase = CacheWarmingPhase::Idle,
                _ => return,
            }
        }
        // 用 idle 窗口复查：下一次预热已排到窗口之外，或当前时间已超窗，都不再继续。
        let deadline = started_at + MAX_IDLE_WARMING_AGE_MS;
        if next_warm_at > deadline || core.timer.now() >= deadline {
            core.stop("30-minute idle safety limit reached", None, None);
        }
    }

    /// `onModeChanged()`：持久化的预热模式变化后对账。
    ///
    /// 只在「新模式下当前 run 已不合法」时停止；反过来，模式从 Off 恢复为 idle/streaming
    /// 也不会自动重建 run——那要等下一次真实请求的 `start`。没有 run 时什么都不做。
    ///
    /// TS: `core/cache-warmer.ts#CacheWarmer.onModeChanged`
    pub fn on_mode_changed(&self) {
        let core = &self.core;
        let phase = lock(&core.inner).run.as_ref().map(|run| run.phase);
        if let Some(phase) = phase
            && let Some(reason) = core.mode_stop_reason(phase)
        {
            core.stop(reason, None, None);
        }
    }

    /// `cancel()`。
    ///
    /// 停止当前 run 并把 reason 记成 `"inactive"`。在飞行中的重放会因 run 的
    /// `CancellationToken` 被取消而尽快结束；它结束后的 `validate_run` 会发现 run 已不在，
    /// 因此不会记账、也不会再排下一次。
    ///
    /// TS: `core/cache-warmer.ts#CacheWarmer.cancel`
    pub fn cancel(&self) {
        self.core.stop("inactive", None, None);
    }

    /// 直接触发当前 run 的刷新并等待完成（TS 测试用 `(warmer as any).refresh(run)`）。
    ///
    /// 测试缝（doc-hidden），不是公开 API。走的是与定时器到期同一条 `refresh` 路径：
    /// 会重新校验、重新决策，并且 run 没被替换时会再排下一次。没有 run 时是空操作。
    #[doc(hidden)]
    pub async fn refresh_current_for_test(&self) {
        let run_id = lock(&self.core.inner).run.as_ref().map(|run| run.id);
        if let Some(run_id) = run_id {
            self.core.refresh(run_id).await;
        }
    }
}

impl Core {
    /// 读取当前的 `cacheWarming` 模式。
    ///
    /// 每次都问外部闭包（而不是缓存在 `Core` 里），所以设置一变，下一次判定立刻生效。
    fn mode(&self) -> CacheWarmingMode {
        (self.get_mode)()
    }

    /// 当前模式下这个 phase 的 run 是否必须停止；`None` 表示允许继续。
    ///
    /// - `Off`：任何 phase 都停（`cache warming disabled`）。
    /// - `Streaming`：只在 phase 已是 `Idle` 时停（`agent run settled`）——该模式只保温
    ///   「正在进行的 run」，run 结束后不再续。
    /// - `Idle`：两个 phase 都允许。
    ///
    /// `validate_run` 用它做「定时器到期/await 返回后 mode 是否仍然允许」的检查。
    fn mode_stop_reason(&self, phase: CacheWarmingPhase) -> Option<&'static str> {
        match self.mode() {
            CacheWarmingMode::Off => Some("cache warming disabled"),
            CacheWarmingMode::Streaming if phase == CacheWarmingPhase::Idle => {
                Some("agent run settled")
            }
            CacheWarmingMode::Streaming | CacheWarmingMode::Idle => None,
        }
    }

    /// 丢弃当前 run：先 `take()` 掉引用，再清定时器、取消 `CancellationToken`。
    ///
    /// 顺序是有意的（与 TS 的「先清引用再 abort」一致）：任何并发续体在拿到锁后都会看到
    /// `run == None` 或不同的 id，从而不会再去碰已被取消的请求。`controller.cancel()`
    /// 只是让在飞行中的重放尽快结束，不保证它立刻返回。
    fn clear_run(&self) {
        let run = lock(&self.inner).run.take();
        if let Some(run) = run {
            if let Some(timer) = run.timer {
                timer.clear();
            }
            run.controller.cancel();
        }
    }

    /// 停止保温：清掉 run，并把 `inactive` 快照更新为这次停止的原因与决策。
    ///
    /// `decision` / `extension_override` 由 `refresh_run` 在「算出决策后停止」的路径传入；
    /// 其它路径（cancel、mode 变化等）传 `None`，此时格式化层会退化成
    /// `Inactive (reason)`。这里不碰 mode、不动 `on_warmed`。
    fn stop(
        &self,
        reason: &str,
        decision: Option<CacheWarmingDecision>,
        extension_override: Option<bool>,
    ) {
        self.clear_run();
        let mut inner = lock(&self.inner);
        inner.inactive = CacheWarmingStatus {
            state: CacheWarmingState::Inactive,
            reason: Some(reason.to_string()),
            next_warm_at: None,
            decision,
            extension_override,
        };
    }

    /// 排下一次预热（或判定必须停止）。
    ///
    /// 计算都在锁内完成，「武装定时器」放在锁外（`set_timeout` 是外部实现，可能立即
    /// 调度回调），最后再取一次锁安装句柄。
    ///
    /// 关键点：
    ///
    /// - 每次排期都清掉 `extension_override`：它只描述「刚刚这一次决策」。
    /// - `next_warm_at = now + delay_ms`（`delay_ms` 是 `start` 时算好的 90% TTL）；
    ///   `refresh_deadline_at = next_warm_at + floor((ttl - delay) / 2)`，即只允许定时器
    ///   迟到一半的过期余量（注释里说的「保留一半」）。
    /// - 安全窗口按 phase 选 1 小时 / 30 分钟，从 `started_at` 起算；`next_warm_at`
    ///   已落在窗口外、或当前时间已超窗，就 `stop` 并给出对应的 reason。
    /// - 武装完成后重新校验 `run.id`：run 已被替换就立刻 `clear()`，避免野定时器落在
    ///   无关的 run 上（回调本身进入 `refresh_run` 后也会再按 id 校验一次）。
    ///
    /// 若 run 在两次取锁之间消失或被替换，函数静默返回——停止/替换的一方已经写好了状态。
    fn schedule(self: &Arc<Self>, run_id: u64) {
        let now = self.timer.now();
        let mut stop_reason: Option<&'static str> = None;
        let mut delay_ms: Option<i64> = None;
        {
            let mut inner = lock(&self.inner);
            let Some(run) = inner.run.as_mut() else {
                return;
            };
            if run.id != run_id {
                return;
            }
            // 新一轮排期：上一次决策的扩展覆盖标记不复用。
            run.extension_override = false;
            run.next_warm_at = now + run.delay_ms as i64;
            // 定时器可能因睡眠/事件循环阻塞而迟到：保留一半的过期余量。
            run.refresh_deadline_at =
                run.next_warm_at + ((run.ttl_ms - run.delay_ms) / 2.0).floor() as i64;
            let deadline = run.started_at
                + if run.phase == CacheWarmingPhase::Idle {
                    MAX_IDLE_WARMING_AGE_MS
                } else {
                    MAX_WARMING_AGE_MS
                };
            if run.next_warm_at > deadline || now >= deadline {
                stop_reason = Some(if run.phase == CacheWarmingPhase::Idle {
                    "30-minute idle safety limit reached"
                } else {
                    "one-hour safety limit reached"
                });
            } else {
                delay_ms = Some((run.next_warm_at - now).max(0));
            }
        }
        if let Some(reason) = stop_reason {
            self.stop(reason, None, None);
            return;
        }
        let Some(delay_ms) = delay_ms else {
            return;
        };
        let core = Arc::clone(self);
        let callback: CacheWarmerTimerCallback =
            Box::new(move || Box::pin(async move { core.refresh(run_id).await }));
        let handle = self.timer.set_timeout(delay_ms, callback);
        let mut inner = lock(&self.inner);
        match inner.run.as_mut() {
            Some(run) if run.id == run_id => run.timer = Some(handle),
            _ => handle.clear(),
        }
    }

    /// 计算一次「warm 还是 stop」的经济性决策。
    ///
    /// 三个价格都按同一个 prompt 规模算——
    ///
    /// - `cache_hit_cost`：prompt 全部从缓存读（`cache_read`）；
    /// - `cache_miss_cost`：缓存丢失后重付的钱。模型有 `cache_write` 单价时按写缓存算，
    ///   否则按普通输入算（分支判据是 `model.cost.cache_write > 0`）；
    /// - `warm_cost`：这次刷新本身的钱 = prompt 的 cache read + 1 个 output token。
    ///
    /// `miss_cost = max(0, cache_miss_cost - cache_hit_cost)`，即只算「相比命中多付的差额」。
    /// `continuation_probability`：streaming 阶段为 1.0（run 还在进行，真实请求会继续），
    /// idle 阶段为 [`IDLE_CONTINUATION_PROBABILITY`]。
    /// `economics_available` 要求 prompt 规模与价格均已知（`prompt_tokens > 0` 且至少一个
    /// 价格非零）；为假时，未被扩展覆盖且当前不在刷新中的调用方会报
    /// `cache economics unavailable` 并停止。两个例外：`refreshing`（`run.timer` 已被
    /// `refresh_run` 置 `None`）时 `status()` 跳过该 inactive 状态；扩展把 action 覆盖成
    /// `Warm` 时 `refresh_run` 即使 `economics_available == false` 也会继续重放。
    ///
    /// 只读：会锁一次会话并调用 `get_branch()`，但不修改任何状态；`status()` 也会调用它。
    fn evaluate(&self, model: &Model, phase: CacheWarmingPhase) -> CacheWarmingDecision {
        // prompt 规模：分支上最后一条 assistant 消息的 usage 总量。
        // 走 `last_prompt_tokens()`（SessionManager 实现为 O(1) 叶子回溯），避免每次
        // `status()` 都克隆整条分支。
        let prompt_tokens = {
            let session = self
                .session
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            session.last_prompt_tokens()
        };
        let cache_hit_cost = price(
            model,
            PriceTokens {
                cache_read: prompt_tokens,
                ..PriceTokens::default()
            },
        );
        // 缓存丢失时，同一段 prompt 要么重新写缓存，要么按普通输入重付；
        // 有 cache_write 单价就说明写缓存是单独计价的能力。
        let cache_miss_cost = price(
            model,
            if model.cost.cache_write > 0.0 {
                PriceTokens {
                    cache_write: prompt_tokens,
                    ..PriceTokens::default()
                }
            } else {
                PriceTokens {
                    input: prompt_tokens,
                    ..PriceTokens::default()
                }
            },
        );
        // 刷新成本：读一遍 prompt 缓存 + 1 个输出 token（重放时 max_tokens = 1）。
        let warm_cost = price(
            model,
            PriceTokens {
                cache_read: prompt_tokens,
                output: 1,
                ..PriceTokens::default()
            },
        );
        let miss_cost = (cache_miss_cost - cache_hit_cost).max(0.0);
        let continuation_probability = if phase == CacheWarmingPhase::Idle {
            IDLE_CONTINUATION_PROBABILITY
        } else {
            1.0
        };
        let economics_available =
            prompt_tokens > 0 && (cache_hit_cost > 0.0 || cache_miss_cost > 0.0);
        let expected_savings = continuation_probability * miss_cost - warm_cost;
        CacheWarmingDecision {
            phase,
            warm_cost,
            miss_cost,
            continuation_probability,
            expected_savings,
            economics_available,
            action: if expected_savings >= CACHE_WARMING_MINIMUM_EXPECTED_SAVINGS {
                CacheWarmingAction::Warm
            } else {
                CacheWarmingAction::Stop
            },
        }
    }

    /// 检查 run 是否仍然有效；无效时顺手 `stop` 并返回 `false`。
    ///
    /// 三种无效情形：
    ///
    /// 1. run 不存在，或已被 `start` 替换（id 不匹配）→ 返回 `false` 但**不** stop，
    ///    新 run 的状态由替换方负责，不能在这里覆盖；
    /// 2. 当前 mode 已不允许这个 phase（见 [`Core::mode_stop_reason`]）；
    /// 3. `is_current()` 变假（会话上下文已经变了）。
    ///
    /// 第 2、3 种会写一条 `inactive` 状态；`Option::or` 让 mode 的原因优先于 context 原因。
    /// 注意 `or` 的参数是立即求值的：即使 mode 已经给出原因，`is_current()` 仍会被调用一次。
    /// `is_current` 本身在锁外调用。
    fn validate_run(&self, run_id: u64) -> bool {
        let snapshot = {
            let inner = lock(&self.inner);
            inner
                .run
                .as_ref()
                .filter(|run| run.id == run_id)
                .map(|run| (run.phase, Arc::clone(&run.is_current)))
        };
        let Some((phase, is_current)) = snapshot else {
            return false;
        };
        let reason = self.mode_stop_reason(phase).or(if is_current() {
            None
        } else {
            Some("conversation context changed")
        });
        if let Some(reason) = reason {
            self.stop(reason, None, None);
            return false;
        }
        true
    }

    /// 检查定时器是否迟到超过 `refresh_deadline_at`；迟到则 `stop` 并返回 `true`。
    ///
    /// 边界：`now <= deadline` 都算没迟到（正好等于截止时刻仍然允许刷新）。
    /// run 不存在或已被替换时返回 `false`，交给调用方的 `validate_run` 判断。
    ///
    /// 用途：进程睡眠或事件循环阻塞后，定时器可能到缓存已经过期才醒；这时再重放只是白花
    /// 钱，所以直接放弃本次刷新（reason `cache refresh deadline missed`）。
    fn refresh_deadline_missed(&self, run_id: u64) -> bool {
        let deadline = {
            let inner = lock(&self.inner);
            inner
                .run
                .as_ref()
                .filter(|run| run.id == run_id)
                .map(|run| run.refresh_deadline_at)
        };
        let Some(deadline) = deadline else {
            return false;
        };
        if self.timer.now() <= deadline {
            return false;
        }
        self.stop("cache refresh deadline missed", None, None);
        true
    }

    /// `refresh(run)` 的 async 本体：一次完整的「校验 → 决策 → 重放 → 记账 → 续排」。
    ///
    /// 阶段（对应 spec §2.28.7 的私有 `refresh`）：
    ///
    /// 1. **标记刷新中**：把 `run.timer` 置 `None`（id 仍匹配时）。这是 `status()` 判定
    ///    `refreshing` 的依据。
    /// 2. **前置校验**：`validate_run` + `refresh_deadline_missed`，任一失败直接返回。
    /// 3. **决策**：`evaluate` 算出 kiana 的结论，把只含 warm/miss/probability/action 的
    ///    事件交给 `decide`；扩展返回的 action 覆盖本地结论。await 期间不持锁。
    /// 4. **决策后复检**：扩展可能耗时很久，所以再跑一次两项校验；随后
    ///    `extension_override = action != decision.action`。
    /// 5. **stop 分支**：按「扩展否决 / 预期节省不足 / 经济性不可用」三选一给 reason，
    ///    并把 decision 与 override 一起写进 `inactive` 状态（供 `/session` 展示）。
    /// 6. **warm 分支**：重新快照 model/context/options/controller（run 可能已被替换，
    ///    取不到就返回），然后覆盖三个请求字段：`max_tokens = 1`、`max_retries = 0`、
    ///    `signal = 该 run 的 CancellationToken`。原请求的 signal 不受影响。
    /// 7. **等待结果**：`stream.result().await`；`Err` 表示流没有提交最终消息，静默忽略。
    ///    拿到消息后再 `validate_run`，且只有 `stop_reason` 不是 `error`/`aborted` 才算一次
    ///    成功预热。
    /// 8. **记账**：以 `"cache_warm"` 为 kind 写一条 usage（model 名优先用
    ///    `response_model`，回退到 `message.model`；扩展覆盖时带 note `extension override`）。
    ///    `append_usage` 返回 `None` 表示写失败：不调用 `on_warmed`，但继续往下走。
    /// 9. **续排**：run 仍未被替换时才 `schedule` 下一次，否则到此为止。
    ///
    /// 整个函数不返回错误：预热失败只影响这一轮，绝不能影响主 agent run。
    async fn refresh_run(self: &Arc<Self>, run_id: u64) {
        // 阶段 1：进入「刷新中」；timer 置空后 status() 会报 refreshing。
        {
            let mut inner = lock(&self.inner);
            if let Some(run) = inner.run.as_mut()
                && run.id == run_id
            {
                run.timer = None;
            }
        }
        // 阶段 2：run 是否还是当前、mode 是否仍允许、context 是否没变；定时器是否已迟到。
        if !self.validate_run(run_id) {
            return;
        }
        if self.refresh_deadline_missed(run_id) {
            return;
        }
        // 阶段 3：取当前 run 的模型与阶段（锁内只 clone），算本地决策，再把精简事件
        // 交给扩展回调；`.await` 期间没有任何锁被持有。
        let Some((model, phase)) = ({
            let inner = lock(&self.inner);
            inner
                .run
                .as_ref()
                .filter(|run| run.id == run_id)
                .map(|run| (run.model.clone(), run.phase))
        }) else {
            return;
        };
        let decision = self.evaluate(&model, phase);
        let action = (self.decide)(CacheWarmingDecisionEvent {
            warm_cost: decision.warm_cost,
            miss_cost: decision.miss_cost,
            continuation_probability: decision.continuation_probability,
            action: decision.action,
        })
        .await;
        // 阶段 4：扩展可能 await 很久；用同一组校验确认期间没有失效，再比较 action。
        if !self.validate_run(run_id) || self.refresh_deadline_missed(run_id) {
            return;
        }
        let extension_override = action != decision.action;
        // 阶段 5：stop 的三选一原因——扩展否决优先，其次区分「不划算」与「算不出来」。
        if action == CacheWarmingAction::Stop {
            let reason = if extension_override {
                "stopped by extension"
            } else if decision.economics_available {
                "expected savings below threshold"
            } else {
                "cache economics unavailable"
            };
            self.stop(reason, Some(decision), Some(extension_override));
            return;
        }
        // 记下本次覆盖标记；status() 会把它作为 extension_override 透出。
        {
            let mut inner = lock(&self.inner);
            if let Some(run) = inner.run.as_mut()
                && run.id == run_id
            {
                run.extension_override = extension_override;
            }
        }
        // 阶段 6：重新快照请求参数；run 已在 await 期间被替换就放弃。
        let Some((model, context, options, controller)) = ({
            let inner = lock(&self.inner);
            inner
                .run
                .as_ref()
                .filter(|run| run.id == run_id)
                .map(|run| {
                    (
                        run.model.clone(),
                        run.context.clone(),
                        run.options.duplicate(),
                        run.controller.clone(),
                    )
                })
        }) else {
            return;
        };
        // 重放只需续命：最多 1 个输出 token、不重试、用该 run 自己的取消令牌。
        let mut stream_options = options;
        stream_options.base.base.max_tokens = Some(1);
        stream_options.base.base.max_retries = Some(0);
        stream_options.base.base.signal = Some(controller.clone());
        let stream = self.models.stream_simple(&model, &context, stream_options);
        // 阶段 7：等结果。`Err` 表示流结束了但从未提交最终消息。
        match stream.result().await {
            Err(_) => {}
            Ok(message) => {
                if !self.validate_run(run_id) {
                    return;
                }
                // 失败或被取消的重放不算一次成功预热：不记账。
                if message.stop_reason != StopReason::Error
                    && message.stop_reason != StopReason::Aborted
                {
                    let model_name = message
                        .response_model
                        .clone()
                        .unwrap_or_else(|| message.model.clone());
                    let note = extension_override.then_some("extension override");
                    // 阶段 8：锁会话写 usage；锁块结束后再调用 on_warmed，回调可能反过来
                    // 访问会话或事件总线。
                    let entry = {
                        let mut session = self
                            .session
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        session.append_usage(
                            "cache_warm",
                            &message.provider,
                            &model_name,
                            &message.usage,
                            note,
                        )
                    };
                    // appendUsage 抛异常时 TS 的 catch 会吞掉它：不调用 onWarmed，
                    // 但继续走下面的 schedule。
                    if let Some(entry) = entry {
                        let callback = lock(&self.inner).on_warmed.clone();
                        if let Some(callback) = callback {
                            callback(entry);
                        }
                    }
                }
            }
        }
        // 阶段 9：run 仍是当前才续排；被替换或取消时循环到此结束。
        let still_current = {
            let inner = lock(&self.inner);
            inner.run.as_ref().is_some_and(|run| run.id == run_id)
        };
        if still_current {
            self.schedule(run_id);
        }
    }

    /// 把 [`Core::refresh_run`] 包装成 `'static` future（定时器回调的要求）。
    ///
    /// 克隆 `Arc<Core>` 而不是借用 `self`，因此这个 future 可以交给任意执行器。
    fn refresh(self: &Arc<Self>, run_id: u64) -> CacheWarmerFuture {
        let core = Arc::clone(self);
        Box::pin(async move { core.refresh_run(run_id).await })
    }
}

// ---------------------------------------------------------------------------
// 格式化
// ---------------------------------------------------------------------------

/// `Math.round`：半数向 +∞（Rust 的 `f64::round` 是远离零）。
///
/// `NaN` / `±Infinity` 原样返回，不进入下面的取整比较。
fn js_round(value: f64) -> f64 {
    if !value.is_finite() {
        return value;
    }
    // `Math.round` 是“半数向 +∞”，但**不能**写成 `(value + 0.5).floor()`：
    // `0.49999999999999994 + 0.5` 在 f64 下会舍入到 1.0，而 V8 的 Math.round 返回 0。
    // 用 `value - floor` 比较 0.5 避开这一步舍入（差值本身是精确可表示的）。
    let floor = value.floor();
    if value - floor >= 0.5 {
        floor + 1.0
    } else {
        floor
    }
}

/// `formatDollars(value)`：3 位小数，负数把负号放在 `$` 前。
///
/// 小数部分用 `js_to_fixed`（JS `toFixed` 的语义）而不是 Rust 的 `format!`：负数必须先把
/// 绝对值格式化再手工拼 `-`，否则会得到 `$-1.407` 这种 TS 不会产生的写法。
fn format_dollars(value: f64) -> String {
    if value < 0.0 {
        format!("-${}", js_to_fixed(value.abs(), 3))
    } else {
        format!("${}", js_to_fixed(value, 3))
    }
}

/// `formatCacheWarmingEconomics(decision)`。
///
/// 输出形如 `<概率>% continuation probability[ while agent is running], expected savings
/// $X.XXX >=|< $0.050`：
///
/// - 概率是 `continuation_probability * 100` 经 `js_round`（复刻 `Math.round`）；
/// - `while agent is running` 只在 streaming 阶段出现；
/// - 比较符由 action 决定（warm 用 `>=`，stop 用 `<`），阈值固定为 3 位小数 `$0.050`；
/// - `economics_available == false` 时整段退化为固定文案 `cache economics unavailable`。
fn format_cache_warming_economics(decision: &CacheWarmingDecision) -> String {
    if !decision.economics_available {
        return "cache economics unavailable".to_string();
    }
    let probability = js_round(decision.continuation_probability * 100.0);
    let probability_text = if decision.phase == CacheWarmingPhase::Streaming {
        format!("{probability}% continuation probability while agent is running")
    } else {
        format!("{probability}% continuation probability")
    };
    let comparison = if decision.action == CacheWarmingAction::Warm {
        ">="
    } else {
        "<"
    };
    format!(
        "{probability_text}, expected savings {} {comparison} ${}",
        format_dollars(decision.expected_savings),
        js_to_fixed(CACHE_WARMING_MINIMUM_EXPECTED_SAVINGS, 3)
    )
}

/// `formatCacheWarmingDecisionTime(nextWarmAt, now)`。
///
/// `None` 或已到期（`next_warm_at <= now`）→ `Decision now`；否则把剩余毫秒向上取整到秒
/// （`(diff + 999) / 1000`），再拆成 h/m/s 并只保留非零分量（秒分量在已有其它分量且为 0
/// 时省略）。`now` 由调用方传入（`/session` 用当前毫秒时间戳），因此这个函数可测试、
/// 不自行取时钟。
fn format_cache_warming_decision_time(next_warm_at: Option<i64>, now: i64) -> String {
    let Some(next_warm_at) = next_warm_at else {
        return "Decision now".to_string();
    };
    if next_warm_at <= now {
        return "Decision now".to_string();
    }
    let mut remaining_seconds = (next_warm_at - now + 999) / 1000;
    let hours = remaining_seconds / 3600;
    remaining_seconds %= 3600;
    let minutes = remaining_seconds / 60;
    let seconds = remaining_seconds % 60;
    let mut parts: Vec<String> = Vec::new();
    if hours > 0 {
        parts.push(format!("{hours}h"));
    }
    if minutes > 0 {
        parts.push(format!("{minutes}m"));
    }
    if seconds > 0 || parts.is_empty() {
        parts.push(format!("{seconds}s"));
    }
    format!("Decision in {}", parts.join(" "))
}

/// `/session` 的一行状态。
///
/// 输出模板逐字对应 spec §3.7，四种形态：`Inactive (reason)`、`Stopped (details)`、
/// `Warming cache (details)`、`Decision ... (details)`；`details` 为
/// `extension override, <economics>` 或 `<economics> -> warm|stop`。
///
/// 判定：`decision` 缺失，或「inactive + 经济性不可用 + 没有扩展覆盖」时退化为
/// `Inactive (...)`——这两种情况从未真正算出过可展示的决策。`extension_override` 的
/// `None` 一律按 `false` 处理；`reason` 缺失时显示 `unknown reason`。
///
/// `now` 是毫秒时间戳，只用于 `Scheduled` 形态的倒计时。
///
/// TS: `core/cache-warmer.ts#formatCacheWarmingStatus`
pub fn format_cache_warming_status(status: &CacheWarmingStatus, now: i64) -> String {
    let decision = status.decision.as_ref();
    let extension_override = status.extension_override.unwrap_or(false);
    // 决策一旦被 kiana（或扩展）处理过就带上；没有决策的 inactive 从未走到那一步。
    let Some(decision) = decision else {
        return format!(
            "Inactive ({})",
            status.reason.as_deref().unwrap_or("unknown reason")
        );
    };
    if status.state == CacheWarmingState::Inactive
        && !decision.economics_available
        && !extension_override
    {
        return format!(
            "Inactive ({})",
            status.reason.as_deref().unwrap_or("unknown reason")
        );
    }
    let details = if extension_override {
        format!(
            "extension override, {}",
            format_cache_warming_economics(decision)
        )
    } else {
        format!(
            "{} -> {}",
            format_cache_warming_economics(decision),
            decision.action.as_str()
        )
    };
    match status.state {
        CacheWarmingState::Inactive => format!("Stopped ({details})"),
        CacheWarmingState::Refreshing => format!("Warming cache ({details})"),
        CacheWarmingState::Scheduled => format!(
            "{} ({details})",
            format_cache_warming_decision_time(status.next_warm_at, now)
        ),
    }
}

/// `toFixed(6).replace(/(\.\d{3}\d*?)0+$/, "$1")`：至少保留 3 位小数。
///
/// 先按 JS `toFixed(6)` 得到固定 6 位，再从尾部删 0，直到只剩 3 位小数（`0.1234` 会保留
/// 尾部的 `4`；`1.500` 因 `while` 的 `len > 3` 条件停在 3 位）。10 的整数次幂等无小数点的
/// 输出（`NaN`、`Infinity`、绝对值 >= 1e21 时的部分指数形式）原样返回；其他 >= 1e21 的
/// 指数形式（如 `1.5e+21`）虽含小数点、会走裁剪分支，但其尾部没有多余的 0，结果同样是
/// 原样返回。
fn format_cost_with_trimmed_zeros(value: f64) -> String {
    let fixed = js_to_fixed(value, 6);
    let Some(dot) = fixed.find('.') else {
        return fixed;
    };
    let integer = &fixed[..dot];
    let mut fraction = fixed[dot + 1..].to_string();
    while fraction.len() > 3 && fraction.ends_with('0') {
        fraction.pop();
    }
    format!("{integer}.{fraction}")
}

/// 持久化的缓存预热用量的一行 transcript 文本。
///
/// 模板 `Cache warmed[(note)]: $<cost>`：`note` 为 `None` 或空串时整段括号省略（与 TS 里
/// 空字符串是 falsy 一致）；`cost` 用 `format_cost_with_trimmed_zeros`（至少 3 位、最多
/// 6 位小数）。TUI 的 `addCacheWarmingUsage` 是它的调用方。
///
/// TS: `core/cache-warmer.ts#formatCacheWarmingUsage`
pub fn format_cache_warming_usage(entry: &UsageEntry) -> String {
    let note = entry
        .note
        .as_deref()
        .filter(|note| !note.is_empty())
        .map_or_else(String::new, |note| format!(" ({note})"));
    let cost = format_cost_with_trimmed_zeros(entry.usage.cost.total);
    format!("Cache warmed{note}: ${cost}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delay_is_ninety_percent_with_ten_second_margin() {
        assert_eq!(get_cache_warming_delay_ms(300_000.0), Some(270_000.0));
        assert_eq!(get_cache_warming_delay_ms(60_000.0), Some(50_000.0));
        assert_eq!(get_cache_warming_delay_ms(10_000.0), None);
        assert_eq!(get_cache_warming_delay_ms(10_001.0), Some(1.0));
    }

    #[test]
    fn dollars_use_three_decimals_and_sign_before_the_dollar() {
        assert_eq!(format_dollars(0.36), "$0.360");
        assert_eq!(format_dollars(-1.40685), "-$1.407");
        assert_eq!(format_dollars(0.0), "$0.000");
    }

    #[test]
    fn cost_trimming_keeps_at_least_three_decimals() {
        assert_eq!(format_cost_with_trimmed_zeros(0.0), "0.000");
        assert_eq!(format_cost_with_trimmed_zeros(0.1), "0.100");
        assert_eq!(format_cost_with_trimmed_zeros(0.05), "0.050");
        assert_eq!(format_cost_with_trimmed_zeros(0.123), "0.123");
        assert_eq!(format_cost_with_trimmed_zeros(0.1234), "0.1234");
        assert_eq!(format_cost_with_trimmed_zeros(0.123456), "0.123456");
        assert_eq!(format_cost_with_trimmed_zeros(1.0), "1.000");
        assert_eq!(format_cost_with_trimmed_zeros(1.5), "1.500");
        assert_eq!(format_cost_with_trimmed_zeros(12.3456789), "12.345679");
        assert_eq!(format_cost_with_trimmed_zeros(100.0), "100.000");
        assert_eq!(format_cost_with_trimmed_zeros(0.00004), "0.00004");
    }

    #[test]
    fn decision_time_formats_hours_minutes_seconds() {
        assert_eq!(format_cache_warming_decision_time(None, 0), "Decision now");
        assert_eq!(
            format_cache_warming_decision_time(Some(0), 0),
            "Decision now"
        );
        assert_eq!(
            format_cache_warming_decision_time(Some(3_723_000), 0),
            "Decision in 1h 2m 3s"
        );
        assert_eq!(
            format_cache_warming_decision_time(Some(120_000), 0),
            "Decision in 2m"
        );
        assert_eq!(
            format_cache_warming_decision_time(Some(42_500), 0),
            "Decision in 43s"
        );
        assert_eq!(
            format_cache_warming_decision_time(Some(1_000), 0),
            "Decision in 1s"
        );
        assert_eq!(
            format_cache_warming_decision_time(Some(10_000), 20_000),
            "Decision now"
        );
    }

    #[test]
    fn round_matches_js_half_up() {
        assert_eq!(js_round(0.5), 1.0);
        assert_eq!(js_round(-0.5), 0.0);
        assert_eq!(js_round(1.5), 2.0);
        assert_eq!(js_round(0.49999999999999994), 0.0);
    }
}
