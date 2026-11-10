//! 压缩门控：no-op 缓存 + 5 分钟冷却。
//!
//! 设计来源（只借鉴行为，不复制代码）：
//! - `reference_project/oh-my-pi/packages/coding-agent/src/session/session-maintenance.ts`
//!   的 `ManualCompactionNoOpError` 与 rearm 阈值（no-op 后不要反复付摘要成本）；
//! - `reference_project/hermes-agent/agent/context_compressor_prune.py` 的
//!   “reclaim 不足就返回输入对象”与重新 arm 阈值；
//! - 任务要求：“no-op 缓存和 5 分钟冷却”。
//!
//! # 语义
//!
//! - `check(history_hash, now)`：
//!   - 同一历史 hash 上次是 no-op → `NoOpCached`（不调用模型，直接跳过）；
//!   - 距上次 no-op/失败不足冷却时间 → `Cooldown { remaining_ms }`；
//!   - 否则 `Proceed`。
//! - `record(...)`：
//!   - `Succeeded` 清空缓存与冷却（下一次立即允许）；
//!   - `NoOp` 记录 hash 与时间；
//!   - `Failed` 只启动冷却（不缓存 hash，允许冷却后重试）。
//!
//! 时间由调用方传入，测试不需要 sleep。

use std::time::{Duration, Instant};

/// 默认冷却时间：5 分钟。
pub const DEFAULT_COOLDOWN: Duration = Duration::from_secs(5 * 60);

/// 门控决定。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateDecision {
    /// 可以尝试压缩。
    Proceed,
    /// 同一历史刚做过 no-op；直接跳过。
    NoOpCached {
        /// 距缓存的毫秒数（诊断用）。
        age_ms: u64,
    },
    /// 上次失败/ no-op 的冷却尚未结束。
    Cooldown {
        /// 剩余毫秒。
        remaining_ms: u64,
    },
    /// 压缩被设置关闭。
    Disabled,
}

impl GateDecision {
    /// 是否允许调用模型。
    pub fn should_attempt(&self) -> bool {
        matches!(self, GateDecision::Proceed)
    }
}

/// 压缩尝试的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttemptOutcome {
    /// 成功生成并提交了摘要。
    Succeeded,
    /// 没有可压缩内容 / 收益不足 / fallback 也不可用。
    NoOp,
    /// 摘要调用失败。
    Failed,
}

/// no-op 缓存与冷却状态机。
#[derive(Debug, Clone)]
pub struct CompactionGate {
    cooldown: Duration,
    last_attempt: Option<Instant>,
    last_noop_hash: Option<u64>,
    last_noop_at: Option<Instant>,
    last_failed: bool,
    enabled: bool,
}

impl Default for CompactionGate {
    fn default() -> Self {
        Self::new(DEFAULT_COOLDOWN)
    }
}

impl CompactionGate {
    /// 自定义冷却时间。
    pub fn new(cooldown: Duration) -> Self {
        Self {
            cooldown,
            last_attempt: None,
            last_noop_hash: None,
            last_noop_at: None,
            last_failed: false,
            enabled: true,
        }
    }

    /// 关闭/开启压缩门控。
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    /// 冷却时间。
    pub fn cooldown(&self) -> Duration {
        self.cooldown
    }

    /// 检查是否允许尝试。
    pub fn check(&self, history_hash: u64, now: Instant) -> GateDecision {
        if !self.enabled {
            return GateDecision::Disabled;
        }
        if let (Some(hash), Some(at)) = (self.last_noop_hash, self.last_noop_at)
            && hash == history_hash
        {
            return GateDecision::NoOpCached {
                age_ms: now.duration_since(at).as_millis() as u64,
            };
        }
        if let Some(at) = self.last_attempt
            && let Some(elapsed) = now.checked_duration_since(at)
            && elapsed < self.cooldown
        {
            return GateDecision::Cooldown {
                remaining_ms: (self.cooldown - elapsed).as_millis() as u64,
            };
        }
        GateDecision::Proceed
    }

    /// 记录一次尝试的结果。
    pub fn record(&mut self, history_hash: u64, outcome: AttemptOutcome, now: Instant) {
        self.last_attempt = Some(now);
        self.last_failed = outcome == AttemptOutcome::Failed;
        match outcome {
            AttemptOutcome::Succeeded => {
                self.last_noop_hash = None;
                self.last_noop_at = None;
                // 成功不阻止紧接着的下一次（例如用户立刻要求再压）。
                self.last_attempt = None;
            }
            AttemptOutcome::NoOp => {
                self.last_noop_hash = Some(history_hash);
                self.last_noop_at = Some(now);
            }
            AttemptOutcome::Failed => {}
        }
    }

    /// 上一次是否失败（诊断用）。
    pub fn last_failed(&self) -> bool {
        self.last_failed
    }

    /// 清空全部状态。
    pub fn reset(&mut self) {
        *self = Self::new(self.cooldown);
    }
}

/// 计算历史指纹（FNV-1a 64），用于 no-op 缓存。
///
/// 调用方对“会影响压缩结果”的条目序列化后求哈希；只需要稳定、快速，不要求加密强度。
pub fn history_hash(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(seconds: u64) -> Instant {
        Instant::now() + Duration::from_secs(seconds)
    }

    #[test]
    fn noop_is_cached_for_same_history() {
        let mut gate = CompactionGate::default();
        let start = at(0);
        assert_eq!(gate.check(42, start), GateDecision::Proceed);
        gate.record(42, AttemptOutcome::NoOp, start);
        assert!(matches!(
            gate.check(42, start + Duration::from_secs(1)),
            GateDecision::NoOpCached { .. }
        ));
        // 历史变化后只受冷却限制。
        assert!(matches!(
            gate.check(43, start + Duration::from_secs(1)),
            GateDecision::Cooldown { .. }
        ));
    }

    #[test]
    fn cooldown_is_five_minutes_and_expires() {
        let mut gate = CompactionGate::default();
        let start = at(0);
        gate.record(7, AttemptOutcome::Failed, start);
        let decision = gate.check(7, start + Duration::from_secs(60));
        match decision {
            GateDecision::Cooldown { remaining_ms } => {
                assert!(remaining_ms > 3 * 60 * 1000);
            }
            other => panic!("expected cooldown, got {other:?}"),
        }
        assert_eq!(
            gate.check(7, start + Duration::from_secs(301)),
            GateDecision::Proceed
        );
    }

    #[test]
    fn success_resets_and_disabled_blocks() {
        let mut gate = CompactionGate::default();
        let start = at(0);
        gate.record(1, AttemptOutcome::Failed, start);
        gate.record(1, AttemptOutcome::Succeeded, start + Duration::from_secs(1));
        assert_eq!(
            gate.check(1, start + Duration::from_secs(2)),
            GateDecision::Proceed
        );
        gate.set_enabled(false);
        assert_eq!(gate.check(1, start), GateDecision::Disabled);
    }

    #[test]
    fn noop_cache_survives_cooldown_expiry() {
        let mut gate = CompactionGate::default();
        let start = at(0);
        gate.record(9, AttemptOutcome::NoOp, start);
        // 冷却过了，但历史没变，仍然不应该再花钱。
        assert!(matches!(
            gate.check(9, start + Duration::from_secs(3600)),
            GateDecision::NoOpCached { .. }
        ));
    }

    #[test]
    fn history_hash_is_stable() {
        assert_eq!(history_hash(b"abc"), history_hash(b"abc"));
        assert_ne!(history_hash(b"abc"), history_hash(b"abd"));
    }
}
