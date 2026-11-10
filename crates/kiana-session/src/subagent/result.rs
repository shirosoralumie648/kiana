//! 子代理结果契约：terminal 状态、`try_set_terminal` 竞争、usage/receipt 累积。
//!
//! # terminal 契约
//!
//! 一次子代理运行最终只会落在一个值上：
//! [`SubagentTerminal`] 为 `completed` / `failed` / `cancelled` / `timed_out` /
//! `polling_timed_out`，并可选携带 [`SubagentStopReason`]（`token_capped` 等）。
//! 超时与完成可能同时发生：两者都调用 [`TerminalCell::try_set_terminal`]，先到者赢，
//! 后到者得到 `false` 且不会覆盖结果。这保证父代理看到的终态是确定的。
//!
//! # usage / receipt 累积
//!
//! provider 的流式响应既可能按 chunk 上报增量 usage，也可能按消息上报完整 usage；
//! [`UsageAccumulator`] 接收**增量**（[`SubagentStreamChunk::Usage`] 视为一次增量，
//! 由流适配层负责把累计值转成增量），逐项相加。
//!
//! [`ReceiptAccumulator`] 在 `ToolCallStarted` 时记录工具名与参数哈希，在
//! `ToolCallFinished` 时计算输出哈希并分配稳定序号。它只做「从流式 chunk 累积」；
//! 验收（acceptance）与持久化属于 week11b 的 ledger。

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use kiana_ai::types::JsonValue;
use serde::{Deserialize, Serialize};

/// 子代理终态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubagentTerminal {
    /// 正常完成。
    Completed,
    /// 失败。
    Failed,
    /// 被取消。
    Cancelled,
    /// 超时。
    TimedOut,
    /// 轮询超时（后台任务在等待结果时超时）。
    PollingTimedOut,
}

impl SubagentTerminal {
    /// 稳定字符串（与 JSON 契约一致）。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::TimedOut => "timed_out",
            Self::PollingTimedOut => "polling_timed_out",
        }
    }

    /// 是否成功。
    pub fn is_success(self) -> bool {
        matches!(self, Self::Completed)
    }
}

impl std::fmt::Display for SubagentTerminal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// 提前结束的原因（与 terminal 正交）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubagentStopReason {
    /// token 预算耗尽。
    TokenCapped,
    /// turn 上限。
    TurnCapped,
    /// 重复工具调用循环。
    LoopCapped,
    /// 模型输出长度上限。
    ModelLengthCapped,
    /// 工具/模型错误。
    Error,
}

impl SubagentStopReason {
    /// 稳定字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TokenCapped => "token_capped",
            Self::TurnCapped => "turn_capped",
            Self::LoopCapped => "loop_capped",
            Self::ModelLengthCapped => "model_length_capped",
            Self::Error => "error",
        }
    }
}

/// 子代理的累计 usage。
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentUsage {
    /// 净输入 token。
    pub input_tokens: u64,
    /// 输出 token。
    pub output_tokens: u64,
    /// 命中缓存的输入 token。
    pub cache_read_tokens: u64,
    /// 写入缓存的输入 token。
    pub cache_write_tokens: u64,
    /// 总 token（若 provider 未给出，用四项之和）。
    pub total_tokens: u64,
    /// 费用（USD）。
    pub cost: f64,
}

impl SubagentUsage {
    fn billable_fields(&self) -> u64 {
        self.input_tokens
            .saturating_add(self.output_tokens)
            .saturating_add(self.cache_read_tokens)
            .saturating_add(self.cache_write_tokens)
    }

    /// 累加一次增量。
    pub fn add(&mut self, input: u64, output: u64, cache_read: u64, cache_write: u64, cost: f64) {
        // provider 报的 total 可能比四项之和大（未单列的 reasoning/cache 项）。这段
        // “额外量”必须在多次 add 之间保留，否则后续 turn/stage 的 add 会把累计
        // total 覆盖回四项之和，历史额外量静默丢失。
        let provider_extra = self.total_tokens.saturating_sub(self.billable_fields());
        self.input_tokens = self.input_tokens.saturating_add(input);
        self.output_tokens = self.output_tokens.saturating_add(output);
        self.cache_read_tokens = self.cache_read_tokens.saturating_add(cache_read);
        self.cache_write_tokens = self.cache_write_tokens.saturating_add(cache_write);
        self.total_tokens = self.billable_fields().saturating_add(provider_extra);
        self.cost += cost;
    }

    /// 累加另一次运行（例如 staged 流水线的上一个 stage）的 usage 快照。
    ///
    /// 直接用 `add` 只累加四个字段，会丢掉 provider 上报的 `totalTokens` 额外量；
    /// 这里按快照把额外量也并入累计 total。
    pub fn add_snapshot(&mut self, other: &SubagentUsage) {
        self.add(
            other.input_tokens,
            other.output_tokens,
            other.cache_read_tokens,
            other.cache_write_tokens,
            other.cost,
        );
        let extra = other.total_tokens.saturating_sub(other.billable_fields());
        self.total_tokens = self.total_tokens.saturating_add(extra);
    }

    /// 从 `kiana_ai::types::Usage` 累加（缓存字段与费用明细一并计入）。
    pub fn add_usage(&mut self, usage: &kiana_ai::types::Usage) {
        let turn_fields = usage
            .input
            .saturating_add(usage.output)
            .saturating_add(usage.cache_read)
            .saturating_add(usage.cache_write);
        self.add(
            usage.input,
            usage.output,
            usage.cache_read,
            usage.cache_write,
            usage.cost.total,
        );
        // provider 的 total 是“本次调用”的总额：额外量按调用累加，而不是用 max
        // 覆盖累计 total（多轮/多 stage 会丢掉之前调用的额外量）。
        let turn_extra = usage.total_tokens.saturating_sub(turn_fields);
        self.total_tokens = self.total_tokens.saturating_add(turn_extra);
    }
}

/// 一次运行的终态快照。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentTerminalState {
    /// 终态。
    pub terminal: SubagentTerminal,
    /// 提前结束原因。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<SubagentStopReason>,
    /// 结果文本（成功时）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    /// 错误文本（失败时）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// 累计 usage。
    #[serde(default)]
    pub usage: SubagentUsage,
    /// 终态序号（1-based，先写者分配）。
    #[serde(default)]
    pub seq: u64,
}

impl SubagentTerminalState {
    /// 构造成功终态。
    pub fn completed(result: impl Into<String>) -> Self {
        Self {
            terminal: SubagentTerminal::Completed,
            stop_reason: None,
            result: Some(result.into()),
            error: None,
            usage: SubagentUsage::default(),
            seq: 0,
        }
    }

    /// 构造失败终态。
    pub fn failed(error: impl Into<String>) -> Self {
        Self {
            terminal: SubagentTerminal::Failed,
            stop_reason: None,
            result: None,
            error: Some(error.into()),
            usage: SubagentUsage::default(),
            seq: 0,
        }
    }

    /// 构造取消/超时终态。
    pub fn with_terminal(terminal: SubagentTerminal, message: Option<String>) -> Self {
        Self {
            terminal,
            stop_reason: None,
            result: None,
            error: message,
            usage: SubagentUsage::default(),
            seq: 0,
        }
    }

    /// 设置 stop reason。
    pub fn with_stop_reason(mut self, reason: SubagentStopReason) -> Self {
        self.stop_reason = Some(reason);
        self
    }

    /// 设置 usage。
    pub fn with_usage(mut self, usage: SubagentUsage) -> Self {
        self.usage = usage;
        self
    }
}

/// 终态槽：`try_set_terminal` 只有一个赢家。
#[derive(Debug, Default)]
pub struct TerminalCell {
    inner: Mutex<Option<SubagentTerminalState>>,
    next_seq: AtomicU64,
}

impl TerminalCell {
    /// 新建空槽。
    pub fn new() -> Self {
        Self::default()
    }

    /// 尝试写入终态。
    ///
    /// 返回 `true` 表示本次写入获胜；`false` 表示已经有终态，调用方必须保留原值。
    /// 竞争由 `Mutex` 串行化；`seq` 在锁内分配，因此获胜者的 `seq` 总是 1，
    /// 后续失败者不会得到序号。
    pub fn try_set_terminal(&self, mut state: SubagentTerminalState) -> bool {
        let mut guard = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if guard.is_some() {
            return false;
        }
        state.seq = self.next_seq.fetch_add(1, Ordering::SeqCst) + 1;
        *guard = Some(state);
        true
    }

    /// 读取终态。
    pub fn get(&self) -> Option<SubagentTerminalState> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// 是否已有终态。
    pub fn is_terminal(&self) -> bool {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_some()
    }
}

/// 工具结果状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptStatus {
    /// 成功。
    Succeeded,
    /// 失败。
    Failed,
    /// 取消。
    Cancelled,
    /// 超时。
    TimedOut,
    /// 未知（流里没有结果）。
    Unknown,
}

impl ReceiptStatus {
    /// 稳定字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::TimedOut => "timed_out",
            Self::Unknown => "unknown",
        }
    }
}

/// 从流式 chunk 累积出的工具执行凭证。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolReceipt {
    /// 工具名。
    pub tool: String,
    /// 参数 JSON 的 sha256。
    pub args_sha256: String,
    /// 输出 JSON 的 sha256。
    pub output_sha256: String,
    /// 状态。
    pub status: ReceiptStatus,
    /// 稳定的完成序号（1-based）。
    pub seq: u64,
}

/// 子代理流式 chunk（只覆盖结果累积需要的字段）。
#[derive(Debug, Clone, PartialEq)]
pub enum SubagentStreamChunk {
    /// 一次增量 usage。
    Usage {
        /// 净输入 token 增量。
        input_tokens: u64,
        /// 输出 token 增量。
        output_tokens: u64,
        /// 缓存命中增量。
        cache_read_tokens: u64,
        /// 缓存写入增量。
        cache_write_tokens: u64,
        /// 费用增量。
        cost: f64,
    },
    /// 完整 usage 对象（按增量处理）。
    UsageObject(kiana_ai::types::Usage),
    /// 工具调用开始。
    ToolCallStarted {
        /// 调用 id。
        tool_call_id: String,
        /// 工具名。
        tool: String,
        /// 参数。
        arguments: JsonValue,
    },
    /// 工具调用结束。
    ToolCallFinished {
        /// 调用 id。
        tool_call_id: String,
        /// 输出（会自动序列化为 JSON 后哈希）。
        output: JsonValue,
        /// 状态。
        status: ReceiptStatus,
    },
    /// 文本增量（最终结果文本的兜底来源）。
    TextDelta(String),
}

/// usage 累积器。
#[derive(Debug, Default)]
pub struct UsageAccumulator {
    usage: SubagentUsage,
}

impl UsageAccumulator {
    /// 新建。
    pub fn new() -> Self {
        Self::default()
    }

    /// 直接累加。
    pub fn add(&mut self, input: u64, output: u64, cache_read: u64, cache_write: u64, cost: f64) {
        self.usage.add(input, output, cache_read, cache_write, cost);
    }

    /// 从完整 usage 累加。
    pub fn add_usage(&mut self, usage: &kiana_ai::types::Usage) {
        self.usage.add_usage(usage);
    }

    /// 快照。
    pub fn snapshot(&self) -> SubagentUsage {
        self.usage
    }

    /// 已用 token（输入 + 输出 + 缓存读写；用于预算检查）。
    pub fn billable_tokens(&self) -> u64 {
        self.usage
            .input_tokens
            .saturating_add(self.usage.output_tokens)
            .saturating_add(self.usage.cache_read_tokens)
            .saturating_add(self.usage.cache_write_tokens)
    }
}

#[derive(Debug, Clone)]
struct PendingReceipt {
    tool: String,
    args_sha256: String,
}

/// 工具凭证累积器。
#[derive(Debug, Default)]
pub struct ReceiptAccumulator {
    pending: BTreeMap<String, PendingReceipt>,
    receipts: Vec<ToolReceipt>,
    next_seq: u64,
}

impl ReceiptAccumulator {
    /// 新建。
    pub fn new() -> Self {
        Self::default()
    }

    /// 观察一个 chunk。
    pub fn observe(&mut self, chunk: &SubagentStreamChunk) {
        match chunk {
            SubagentStreamChunk::ToolCallStarted {
                tool_call_id,
                tool,
                arguments,
            } => {
                self.pending.insert(
                    tool_call_id.clone(),
                    PendingReceipt {
                        tool: tool.clone(),
                        args_sha256: sha256_json(arguments),
                    },
                );
            }
            SubagentStreamChunk::ToolCallFinished {
                tool_call_id,
                output,
                status,
            } => {
                let pending = self.pending.remove(tool_call_id);
                let (tool, args_sha256) = match pending {
                    Some(pending) => (pending.tool, pending.args_sha256),
                    None => (String::new(), sha256_json(&JsonValue::Null)),
                };
                self.next_seq += 1;
                self.receipts.push(ToolReceipt {
                    tool,
                    args_sha256,
                    output_sha256: sha256_json(output),
                    status: *status,
                    seq: self.next_seq,
                });
            }
            _ => {}
        }
    }

    /// 把仍未结束的调用收尾（取消/崩溃路径），状态由调用方指定。
    pub fn finalize_pending(&mut self, status: ReceiptStatus) {
        let pending: Vec<(String, PendingReceipt)> =
            std::mem::take(&mut self.pending).into_iter().collect();
        for (_tool_call_id, pending) in pending {
            self.next_seq += 1;
            self.receipts.push(ToolReceipt {
                tool: pending.tool,
                args_sha256: pending.args_sha256,
                output_sha256: sha256_json(&JsonValue::Null),
                status,
                seq: self.next_seq,
            });
        }
    }

    /// 已完成的凭证。
    pub fn receipts(&self) -> &[ToolReceipt] {
        &self.receipts
    }
}

/// 对 JSON 值做规范化序列化后取 sha256。
pub fn sha256_json(value: &JsonValue) -> String {
    let bytes = serde_json::to_vec(value).unwrap_or_default();
    sha256_hex(&bytes)
}

/// 对字节取 sha256（小写 hex）。
///
/// 自己实现而不是引入 `sha2`：本工作流禁止修改根 `Cargo.toml` / `Cargo.lock`，
/// 而 `kiana-session` 的依赖表里没有 sha2。实现是标准 FIPS 180-4，测试用公开向量锁定。
pub fn sha256_hex(bytes: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut state: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let bit_length = (bytes.len() as u64).wrapping_mul(8);
    let mut message = Vec::with_capacity(bytes.len() + 72);
    message.extend_from_slice(bytes);
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_length.to_be_bytes());

    for chunk in message.as_chunks::<64>().0 {
        let mut words = [0u32; 64];
        for (index, word) in chunk.as_chunks::<4>().0.iter().enumerate() {
            words[index] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for index in 16..64 {
            let s0 = words[index - 15].rotate_right(7)
                ^ words[index - 15].rotate_right(18)
                ^ (words[index - 15] >> 3);
            let s1 = words[index - 2].rotate_right(17)
                ^ words[index - 2].rotate_right(19)
                ^ (words[index - 2] >> 10);
            words[index] = words[index - 16]
                .wrapping_add(s0)
                .wrapping_add(words[index - 7])
                .wrapping_add(s1);
        }
        let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h) = (
            state[0], state[1], state[2], state[3], state[4], state[5], state[6], state[7],
        );
        for index in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choice = (e & f) ^ ((!e) & g);
            let temp1 = h
                .wrapping_add(s1)
                .wrapping_add(choice)
                .wrapping_add(K[index])
                .wrapping_add(words[index]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(majority);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }
        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
        state[4] = state[4].wrapping_add(e);
        state[5] = state[5].wrapping_add(f);
        state[6] = state[6].wrapping_add(g);
        state[7] = state[7].wrapping_add(h);
    }

    let mut output = String::with_capacity(64);
    for word in state {
        output.push_str(&format!("{word:08x}"));
    }
    output
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[test]
    fn sha256_matches_known_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    #[test]
    fn terminal_race_has_exactly_one_winner() {
        let cell = Arc::new(TerminalCell::new());
        let barrier = Arc::new(std::sync::Barrier::new(8));
        let mut handles = Vec::new();
        for index in 0..8 {
            let cell = Arc::clone(&cell);
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                let state = if index % 2 == 0 {
                    SubagentTerminalState::completed(format!("winner {index}"))
                } else {
                    SubagentTerminalState::with_terminal(
                        SubagentTerminal::TimedOut,
                        Some("timeout".to_string()),
                    )
                };
                cell.try_set_terminal(state)
            }));
        }
        let winners = handles
            .into_iter()
            .map(|handle| handle.join().expect("thread"))
            .filter(|won| *won)
            .count();
        assert_eq!(winners, 1, "exactly one terminal writer wins");
        let state = cell.get().expect("terminal set");
        assert_eq!(state.seq, 1);
        assert!(!cell.try_set_terminal(SubagentTerminalState::failed("late")));
        assert_eq!(cell.get().expect("terminal").seq, 1);
    }

    #[test]
    fn usage_preserves_provider_totals_across_turns() {
        let mut usage = UsageAccumulator::new();
        // provider 总额比四项之和大 50。
        let first = kiana_ai::types::Usage {
            input: 100,
            output: 50,
            total_tokens: 200,
            ..Default::default()
        };
        usage.add_usage(&first);
        assert_eq!(usage.snapshot().total_tokens, 200);

        let second = kiana_ai::types::Usage {
            input: 100,
            output: 50,
            total_tokens: 170,
            ..Default::default()
        };
        usage.add_usage(&second);
        let snapshot = usage.snapshot();
        assert_eq!(snapshot.billable_fields(), 300);
        assert_eq!(
            snapshot.total_tokens, 370,
            "earlier provider-reported totals must survive later turns"
        );
    }

    #[test]
    fn usage_snapshot_merge_preserves_provider_extra() {
        let first = SubagentUsage {
            input_tokens: 100,
            output_tokens: 50,
            cache_read_tokens: 10,
            cache_write_tokens: 5,
            total_tokens: 200,
            cost: 0.25,
        };
        let second = SubagentUsage {
            input_tokens: 20,
            output_tokens: 10,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            total_tokens: 40,
            cost: 0.1,
        };
        let mut merged = SubagentUsage::default();
        merged.add_snapshot(&first);
        merged.add_snapshot(&second);
        assert_eq!(merged.billable_fields(), 195);
        assert_eq!(
            merged.total_tokens,
            195 + 35 + 10,
            "each snapshot's provider extra must be preserved"
        );
        assert!((merged.cost - 0.35).abs() < 1e-12);
    }

    #[test]
    fn usage_accumulates_from_incremental_chunks() {
        let mut usage = UsageAccumulator::new();
        usage.add(10, 5, 2, 1, 0.01);
        usage.add(20, 7, 3, 0, 0.02);
        let snapshot = usage.snapshot();
        assert_eq!(snapshot.input_tokens, 30);
        assert_eq!(snapshot.output_tokens, 12);
        assert_eq!(snapshot.cache_read_tokens, 5);
        assert_eq!(snapshot.cache_write_tokens, 1);
        assert_eq!(snapshot.total_tokens, 48);
        assert!((snapshot.cost - 0.03).abs() < 1e-9);
    }

    #[test]
    fn receipts_are_ordered_and_hash_arguments_and_output() {
        let mut receipts = ReceiptAccumulator::new();
        receipts.observe(&SubagentStreamChunk::ToolCallStarted {
            tool_call_id: "call-1".to_string(),
            tool: "read".to_string(),
            arguments: serde_json::json!({"path": "a.txt"}),
        });
        receipts.observe(&SubagentStreamChunk::ToolCallFinished {
            tool_call_id: "call-1".to_string(),
            output: serde_json::json!({"text": "hello"}),
            status: ReceiptStatus::Succeeded,
        });
        receipts.observe(&SubagentStreamChunk::ToolCallStarted {
            tool_call_id: "call-2".to_string(),
            tool: "bash".to_string(),
            arguments: serde_json::json!({"command": "echo hi"}),
        });
        receipts.observe(&SubagentStreamChunk::ToolCallFinished {
            tool_call_id: "call-2".to_string(),
            output: serde_json::json!({"exit": 0}),
            status: ReceiptStatus::Failed,
        });
        let values = receipts.receipts();
        assert_eq!(values.len(), 2);
        assert_eq!(values[0].seq, 1);
        assert_eq!(values[1].seq, 2);
        assert_eq!(values[0].tool, "read");
        assert_eq!(
            values[0].args_sha256,
            sha256_json(&serde_json::json!({"path": "a.txt"}))
        );
        assert_eq!(
            values[0].output_sha256,
            sha256_json(&serde_json::json!({"text": "hello"}))
        );
        assert_eq!(values[1].status, ReceiptStatus::Failed);
    }

    #[test]
    fn unfinished_receipts_are_finalized_once() {
        let mut receipts = ReceiptAccumulator::new();
        receipts.observe(&SubagentStreamChunk::ToolCallStarted {
            tool_call_id: "call-1".to_string(),
            tool: "write".to_string(),
            arguments: serde_json::json!({"path": "a.txt"}),
        });
        receipts.finalize_pending(ReceiptStatus::Cancelled);
        receipts.finalize_pending(ReceiptStatus::Cancelled);
        assert_eq!(receipts.receipts().len(), 1);
        assert_eq!(receipts.receipts()[0].status, ReceiptStatus::Cancelled);
    }

    #[test]
    fn terminal_serializes_with_snake_case_contract() {
        let state = SubagentTerminalState::completed("ok")
            .with_stop_reason(SubagentStopReason::TokenCapped);
        let value = serde_json::to_value(&state).expect("serialize");
        assert_eq!(value["terminal"], "completed");
        assert_eq!(value["stopReason"], "token_capped");
        assert_eq!(value["result"], "ok");
    }
}
