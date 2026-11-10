//! turn / token / cost / wall-clock 预算。
//!
//! 预算控制器只做决策，不改写请求：agent loop 在每次请求前调用
//! [`BudgetController::begin_turn`]，根据返回的 [`BudgetDecision`]：
//!
//! - 50% / 80%：把 [`BudgetWarning`] 的提醒追加到请求末尾（下一次 `prepare_request`）；
//! - 100%：对 turn / wall-clock 直接停；对 token / cost 进入「final turn」——移除工具列表、
//!   要求模型输出最终文本，并把最终 assistant 消息的 `stop_reason` 设为对应的 cap；
//! - 任何 `completed` / `failed`（`Stop` / `Error` / `Aborted`）都不会被预算覆盖。
//!
//! # 未知价格
//!
//! 模型费用表全为零时无法判断是「免费」还是「没配价」。若配置了 cost 预算，控制器返回
//! [`BudgetStatus::CostUnknown`] 并在快照里置 `cost_unknown = true`，**不会**把费用当成 0
//! 继续按 0 预算放行。没有配置 cost 预算时，未知价格只是记录，不影响运行。
//!
//! # stop reason
//!
//! 预算 cap 用 [`BudgetStopReason`] 表达精确原因（`TurnCapped` / `TokenCapped` /
//! `CostCapped` / `TimeCapped`），并写入 [`StopReason`] 的同名变体
//! （[`StopReason::TurnCapped`] 等）；`raw_stop_reason` 同时保留 camelCase 文本
//! （`turnCapped` 等），供只读原始字符串的消费方使用。把 cap 表达在 `StopReason`
//! 里，会话、UI 与 RPC 才能直接区分“输出被模型截断”（`Length`）与“被本地预算停下”
//! （cap 变体）。`completed` / `failed` 判定（`Stop` / `Error` / `Aborted`）优先，
//! 预算不会覆盖。

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use kiana_ai::types::{Model, StopReason, Usage};

use crate::types::BoxFuture;

/// 预算种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BudgetKind {
    /// 请求轮数。
    Turn,
    /// 累计 token。
    Token,
    /// 累计费用（USD）。
    Cost,
    /// 墙钟时间。
    WallClock,
}

impl BudgetKind {
    /// 稳定字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            BudgetKind::Turn => "turn",
            BudgetKind::Token => "token",
            BudgetKind::Cost => "cost",
            BudgetKind::WallClock => "wallClock",
        }
    }
}

/// 预算 cap 的原因；可映射到 [`StopReason`]。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BudgetStopReason {
    /// turn 预算耗尽。
    TurnCapped,
    /// token 预算耗尽。
    TokenCapped,
    /// cost 预算耗尽。
    CostCapped,
    /// 墙钟预算耗尽。
    TimeCapped,
}

impl BudgetStopReason {
    /// 写入 assistant 消息 `stop_reason` 的精确变体。
    ///
    /// 四个 cap 各自映射到 [`StopReason`] 的同名变体；只有无法区分来源的旧消费者
    /// 才需要看 `Length`，新消费者应直接匹配 cap。
    pub fn stop_reason(self) -> StopReason {
        match self {
            BudgetStopReason::TurnCapped => StopReason::TurnCapped,
            BudgetStopReason::TokenCapped => StopReason::TokenCapped,
            BudgetStopReason::CostCapped => StopReason::CostCapped,
            BudgetStopReason::TimeCapped => StopReason::TimeCapped,
        }
    }

    /// 稳定字符串（与 `StopReason` 的 serde 名一致）。
    pub fn as_str(self) -> &'static str {
        match self {
            BudgetStopReason::TurnCapped => "turnCapped",
            BudgetStopReason::TokenCapped => "tokenCapped",
            BudgetStopReason::CostCapped => "costCapped",
            BudgetStopReason::TimeCapped => "timeCapped",
        }
    }

    /// 对应的预算种类。
    pub fn kind(self) -> BudgetKind {
        match self {
            BudgetStopReason::TurnCapped => BudgetKind::Turn,
            BudgetStopReason::TokenCapped => BudgetKind::Token,
            BudgetStopReason::CostCapped => BudgetKind::Cost,
            BudgetStopReason::TimeCapped => BudgetKind::WallClock,
        }
    }
}

/// 预算配置。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BudgetConfig {
    /// 最大 assistant turn 数（每次请求算一轮）。
    pub max_turns: Option<u64>,
    /// 最大累计 token。
    pub max_tokens: Option<u64>,
    /// 最大累计费用（USD）。
    pub max_cost_usd: Option<f64>,
    /// 最大墙钟时间（毫秒）。
    pub max_wall_clock_ms: Option<u64>,
    /// 提醒比例；默认 `[0.5, 0.8]`，只在首次越过时提醒。
    pub warn_ratios: Vec<f64>,
}

impl Default for BudgetConfig {
    fn default() -> Self {
        BudgetConfig {
            max_turns: None,
            max_tokens: None,
            max_cost_usd: None,
            max_wall_clock_ms: None,
            warn_ratios: vec![0.5, 0.8],
        }
    }
}

impl BudgetConfig {
    /// 构造并过滤非法比例（只保留 `(0, 1)`）。
    pub fn new() -> Self {
        Self::default()
    }

    /// 设置 turn 上限。
    pub fn with_max_turns(mut self, max_turns: u64) -> Self {
        self.max_turns = Some(max_turns);
        self
    }

    /// 设置 token 上限。
    pub fn with_max_tokens(mut self, max_tokens: u64) -> Self {
        self.max_tokens = Some(max_tokens);
        self
    }

    /// 设置 cost 上限。
    pub fn with_max_cost_usd(mut self, max_cost: f64) -> Self {
        self.max_cost_usd = Some(max_cost);
        self
    }

    /// 设置墙钟上限。
    pub fn with_max_wall_clock(mut self, max: Duration) -> Self {
        self.max_wall_clock_ms = Some(max.as_millis() as u64);
        self
    }

    /// 是否启用了任何预算。
    pub fn is_enabled(&self) -> bool {
        self.max_turns.is_some()
            || self.max_tokens.is_some()
            || self.max_cost_usd.is_some()
            || self.max_wall_clock_ms.is_some()
    }

    fn normalized_warn_ratios(&self) -> Vec<f64> {
        let mut ratios: Vec<f64> = self
            .warn_ratios
            .iter()
            .copied()
            .filter(|ratio| ratio.is_finite() && *ratio > 0.0 && *ratio < 1.0)
            .collect();
        ratios.sort_by(|left, right| left.total_cmp(right));
        ratios.dedup_by(|left, right| (*left - *right).abs() < f64::EPSILON);
        ratios
    }
}

/// 一次预算提醒。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BudgetWarning {
    /// 预算种类。
    pub kind: BudgetKind,
    /// 触发比例（0.5 / 0.8）。
    pub ratio: f64,
    /// 已用量。
    pub used: f64,
    /// 上限。
    pub limit: f64,
    /// 注入请求的提醒文本。
    pub message: String,
}

impl BudgetWarning {
    /// 构造提醒文本（agent loop 作为 system 消息追加）。
    pub fn reminder(&self) -> String {
        format!(
            "<budget-warning kind=\"{}\" ratio=\"{:.0}%\">{}\n\
             Keep the current goal in mind and converge: avoid unnecessary tool calls and \
             long detours.</budget-warning>",
            self.kind.as_str(),
            self.ratio * 100.0,
            self.message
        )
    }
}

/// [`BudgetController::begin_turn`] 的决策。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BudgetDecision {
    /// 需要立即停止的 cap（turn / wall-clock）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop: Option<BudgetStopReason>,
    /// 进入 final turn 的 cap（token / cost）：移除工具列表并强制最终文本。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub force_final: Option<BudgetStopReason>,
    /// 本轮新越过的提醒比例。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<BudgetWarning>,
    /// 价格未知（只在配置了 cost 预算时才有意义）。
    pub cost_unknown: bool,
}

impl BudgetDecision {
    /// 是否没有任何动作。
    pub fn is_continue(&self) -> bool {
        self.stop.is_none() && self.force_final.is_none() && self.warnings.is_empty()
    }
}

/// 预算状态。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "status")]
pub enum BudgetStatus {
    /// 未启用或未越过任何阈值。
    Ok,
    /// 已发出提醒。
    Warning,
    /// 价格未知，cost 预算无法判断。
    CostUnknown,
    /// 已停止。
    Capped {
        /// cap 原因。
        reason: BudgetStopReason,
    },
}

/// 预算快照。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BudgetSnapshot {
    /// 已开始的 turn 数。
    pub turns: u64,
    /// 累计 token。
    pub tokens: u64,
    /// 累计费用（USD；价格未知时为 0 且 `cost_unknown = true`）。
    pub cost_usd: f64,
    /// 已用墙钟毫秒。
    pub elapsed_ms: u64,
    /// 状态。
    pub status: BudgetStatus,
    /// 价格是否未知。
    pub cost_unknown: bool,
    /// 已进入 final turn 的 cap。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forced_final: Option<BudgetStopReason>,
    /// 已记录的 cap。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<BudgetStopReason>,
}

/// 共享句柄。
pub type BudgetHandle = Arc<Mutex<BudgetController>>;

/// 墙钟来源（测试可注入）。
pub type BudgetClock = Arc<dyn Fn() -> Instant + Send + Sync>;

/// 预算控制器。
pub struct BudgetController {
    config: BudgetConfig,
    turns: u64,
    tokens: u64,
    cost_usd: f64,
    cost_known: bool,
    cost_unknown: bool,
    started_at: Instant,
    clock: BudgetClock,
    warned: BTreeSet<(BudgetKind, u64)>,
    stop_reason: Option<BudgetStopReason>,
    forced_final: Option<BudgetStopReason>,
    warnings_emitted: u64,
}

impl std::fmt::Debug for BudgetController {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BudgetController")
            .field("config", &self.config)
            .field("turns", &self.turns)
            .field("tokens", &self.tokens)
            .field("cost_usd", &self.cost_usd)
            .field("cost_unknown", &self.cost_unknown)
            .field("stop_reason", &self.stop_reason)
            .field("forced_final", &self.forced_final)
            .finish()
    }
}

impl BudgetController {
    /// 用系统墙钟构造。
    pub fn new(config: BudgetConfig, model: &Model) -> Self {
        Self::new_with_clock(config, model, Arc::new(Instant::now))
    }

    /// 注入墙钟（测试用）。
    pub fn new_with_clock(config: BudgetConfig, model: &Model, clock: BudgetClock) -> Self {
        let cost_known = model_cost_known(model);
        let cost_unknown = !cost_known && config.max_cost_usd.is_some();
        BudgetController {
            config,
            turns: 0,
            tokens: 0,
            cost_usd: 0.0,
            cost_known,
            cost_unknown,
            started_at: clock(),
            clock,
            warned: BTreeSet::new(),
            stop_reason: None,
            forced_final: None,
            warnings_emitted: 0,
        }
    }

    /// 配置。
    pub fn config(&self) -> &BudgetConfig {
        &self.config
    }

    /// 已开始的 turn 数。
    pub fn turns(&self) -> u64 {
        self.turns
    }

    /// 累计 token。
    pub fn tokens(&self) -> u64 {
        self.tokens
    }

    /// 累计费用。
    pub fn cost_usd(&self) -> f64 {
        self.cost_usd
    }

    /// 价格是否未知。
    pub fn cost_unknown(&self) -> bool {
        self.cost_unknown
    }

    /// 已记录的 cap。
    pub fn stop_reason(&self) -> Option<BudgetStopReason> {
        self.stop_reason
    }

    /// 已进入 final turn 的 cap。
    pub fn forced_final(&self) -> Option<BudgetStopReason> {
        self.forced_final
    }

    /// 已用墙钟。
    pub fn elapsed(&self) -> Duration {
        (self.clock)().saturating_duration_since(self.started_at)
    }

    /// 记录一次 assistant 用量。
    pub fn record_usage(&mut self, usage: &Usage) {
        let tokens = if usage.total_tokens > 0 {
            usage.total_tokens
        } else {
            usage.input + usage.output + usage.cache_read + usage.cache_write
        };
        self.tokens = self.tokens.saturating_add(tokens);
        if self.cost_known {
            self.cost_usd += usage.cost.total;
        } else if usage.cost.total > 0.0 {
            // 模型没有配价但 provider 上报了账单：以账单为准并把价格视为已知。
            self.cost_usd += usage.cost.total;
            self.cost_known = true;
            self.cost_unknown = false;
        }
    }

    /// 开始下一次请求（agent loop 在 `prepare_request` 之前调用）。
    pub fn begin_turn(&mut self) -> BudgetDecision {
        if let Some(reason) = self.stop_reason {
            return BudgetDecision {
                stop: Some(reason),
                force_final: None,
                warnings: Vec::new(),
                cost_unknown: self.cost_unknown,
            };
        }
        self.turns = self.turns.saturating_add(1);
        let mut decision = BudgetDecision {
            cost_unknown: self.cost_unknown,
            ..BudgetDecision::default()
        };

        // 先检查 turn / wall-clock：这两类没有「final turn」，直接停。
        if let Some(limit) = self.config.max_turns
            && self.turns > limit
        {
            self.stop_reason = Some(BudgetStopReason::TurnCapped);
            decision.stop = Some(BudgetStopReason::TurnCapped);
            return decision;
        }
        if let Some(limit_ms) = self.config.max_wall_clock_ms
            && self.elapsed().as_millis() as u64 >= limit_ms
        {
            self.stop_reason = Some(BudgetStopReason::TimeCapped);
            decision.stop = Some(BudgetStopReason::TimeCapped);
            return decision;
        }

        // token / cost：越过 100% 时进入 final turn（只进入一次）。
        let token_capped = self
            .config
            .max_tokens
            .is_some_and(|limit| self.tokens >= limit);
        let cost_capped = self
            .config
            .max_cost_usd
            .is_some_and(|limit| self.cost_known && self.cost_usd >= limit);
        if let Some(reason) = self.forced_final {
            decision.force_final = Some(reason);
        } else if token_capped {
            self.forced_final = Some(BudgetStopReason::TokenCapped);
            decision.force_final = Some(BudgetStopReason::TokenCapped);
        } else if cost_capped {
            self.forced_final = Some(BudgetStopReason::CostCapped);
            decision.force_final = Some(BudgetStopReason::CostCapped);
        } else if self.cost_unknown && self.config.max_cost_usd.is_some() {
            decision.cost_unknown = true;
        }

        // 50% / 80% 提醒：turn、token、cost、wall-clock 各自只发一次。
        let ratios = self.config.normalized_warn_ratios();
        if let Some(limit) = self.config.max_turns {
            self.push_warnings(
                BudgetKind::Turn,
                self.turns as f64,
                limit as f64,
                &ratios,
                &mut decision,
            );
        }
        if let Some(limit) = self.config.max_tokens {
            self.push_warnings(
                BudgetKind::Token,
                self.tokens as f64,
                limit as f64,
                &ratios,
                &mut decision,
            );
        }
        if let Some(limit) = self.config.max_cost_usd
            && self.cost_known
        {
            self.push_warnings(
                BudgetKind::Cost,
                self.cost_usd,
                limit,
                &ratios,
                &mut decision,
            );
        }
        if let Some(limit_ms) = self.config.max_wall_clock_ms {
            self.push_warnings(
                BudgetKind::WallClock,
                self.elapsed().as_millis() as f64,
                limit_ms as f64,
                &ratios,
                &mut decision,
            );
        }
        decision
    }

    /// 最终一轮结束后确认 cap；`completed` / `failed` 不会被覆盖。
    ///
    /// 返回值是写入 assistant 消息的 stop reason（若有）。caller 只在
    /// `message.stop_reason` 不属于 `Error` / `Aborted` 时采用。
    pub fn finalize(&mut self, message_stop_reason: StopReason) -> Option<BudgetStopReason> {
        if matches!(message_stop_reason, StopReason::Error | StopReason::Aborted) {
            // 失败/取消优先：预算不覆盖。
            return None;
        }
        let reason = self.forced_final?;
        self.stop_reason = Some(reason);
        Some(reason)
    }

    /// 强制结束（turn / wall-clock 在请求前停止时调用）。
    pub fn mark_capped(&mut self, reason: BudgetStopReason) {
        self.stop_reason = Some(reason);
    }

    /// 快照。
    pub fn snapshot(&self) -> BudgetSnapshot {
        let status = if let Some(reason) = self.stop_reason {
            BudgetStatus::Capped { reason }
        } else if self.cost_unknown {
            BudgetStatus::CostUnknown
        } else if self.warnings_emitted > 0 {
            BudgetStatus::Warning
        } else {
            BudgetStatus::Ok
        };
        BudgetSnapshot {
            turns: self.turns,
            tokens: self.tokens,
            cost_usd: self.cost_usd,
            elapsed_ms: self.elapsed().as_millis() as u64,
            status,
            cost_unknown: self.cost_unknown,
            forced_final: self.forced_final,
            stop_reason: self.stop_reason,
        }
    }

    fn push_warnings(
        &mut self,
        kind: BudgetKind,
        used: f64,
        limit: f64,
        ratios: &[f64],
        decision: &mut BudgetDecision,
    ) {
        if limit <= 0.0 || !used.is_finite() {
            return;
        }
        let ratio = used / limit;
        // 已达 100% 时由 cap 处理，不再重复发 50/80 提醒。
        if ratio >= 1.0 {
            return;
        }
        for (index, warn_ratio) in ratios.iter().enumerate() {
            if ratio < *warn_ratio {
                continue;
            }
            // 用比例的下标作为稳定键：同一档只提醒一次。
            let key = (kind, index as u64);
            if !self.warned.insert(key) {
                continue;
            }
            self.warnings_emitted += 1;
            let message = match kind {
                BudgetKind::Turn => format!(
                    "turn budget {:.0}% used ({}/{} turns)",
                    ratio * 100.0,
                    used as u64,
                    limit as u64
                ),
                BudgetKind::Token => format!(
                    "token budget {:.0}% used ({}/{} tokens)",
                    ratio * 100.0,
                    used as u64,
                    limit as u64
                ),
                BudgetKind::Cost => format!(
                    "cost budget {:.0}% used (${:.4}/${:.4})",
                    ratio * 100.0,
                    used,
                    limit
                ),
                BudgetKind::WallClock => format!(
                    "wall-clock budget {:.0}% used ({} ms/{} ms)",
                    ratio * 100.0,
                    used as u64,
                    limit as u64
                ),
            };
            decision.warnings.push(BudgetWarning {
                kind,
                ratio: *warn_ratio,
                used,
                limit,
                message,
            });
        }
    }
}

/// final turn 的指令文本。
pub fn final_turn_instruction(reason: BudgetStopReason) -> String {
    format!(
        "<budget-exhausted kind=\"{}\">The {} budget is exhausted. Tools are no longer \
         available. Respond with your final answer now: summarize what was completed, what \
         remains, and the concrete next step. Do not request any more tool calls.</budget-exhausted>",
        reason.as_str(),
        reason.kind().as_str()
    )
}

/// 模型费用表是否已知（四项基础价与分档价至少有一个非零）。
pub fn model_cost_known(model: &Model) -> bool {
    let non_zero = |value: f64| value.is_finite() && value != 0.0;
    if non_zero(model.cost.input)
        || non_zero(model.cost.output)
        || non_zero(model.cost.cache_read)
        || non_zero(model.cost.cache_write)
    {
        return true;
    }
    model.cost.tiers.as_ref().is_some_and(|tiers| {
        tiers.iter().any(|tier| {
            non_zero(tier.input)
                || non_zero(tier.output)
                || non_zero(tier.cache_read)
                || non_zero(tier.cache_write)
        })
    })
}

/// 便捷构造共享句柄。
pub fn budget_handle(config: BudgetConfig, model: &Model) -> BudgetHandle {
    Arc::new(Mutex::new(BudgetController::new(config, model)))
}

/// 在 async 环境里安全访问共享控制器。
pub fn with_budget<T>(handle: &BudgetHandle, f: impl FnOnce(&mut BudgetController) -> T) -> T {
    let mut controller = handle
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    f(&mut controller)
}

/// 预算提醒的 `BoxFuture` 适配（保持模块 API 与 agent loop 的异步钩子形状一致）。
pub type BudgetFuture = BoxFuture<'static, BudgetDecision>;

/// 无操作：预算在每次请求前同步决策；保留该别名便于调用方书写钩子签名。
pub fn noop_budget_future(decision: BudgetDecision) -> BudgetFuture {
    Box::pin(async move { decision })
}
