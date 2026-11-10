//! Agent loop 失控保护（loop guard）。
//!
//! 这一层只回答「当前这次观察是否像原地打转」，不负责改写 transcript：它输出
//! [`LoopDetection`]，由 agent loop 在**下一次 `prepare_request` 之前**把提醒作为一条 system
//! 消息追加到请求末尾；达到 `stop_at` 时 agent loop 返回
//! [`crate::agent_loop::AgentLoopError::LoopDetected`]，而不是 panic。
//!
//! # 检测器
//!
//! | 检测器 | 观察点 | 默认 reminder / stop |
//! |---|---|---|
//! | [`LoopDetectorKind::ConsecutiveToolCalls`] | 连续相同的 `repeat_key` | 3 / 5 |
//! | [`LoopDetectorKind::GlobalRepeatedToolCalls`] | 窗口内同一 key 的总次数 | 8 / 15 |
//! | [`LoopDetectorKind::AlternatingToolCalls`] | A-B-A-B 交替周期 | 3 / 5 个周期 |
//! | [`LoopDetectorKind::ReadFileLoop`] | 同一路径的连续读 | 4 / 8 |
//! | [`LoopDetectorKind::ShellStagnation`] | 同一 shell 命令且输出逐字相同 | 2 / 4 |
//! | [`LoopDetectorKind::ActionStagnation`] | 同一非 shell 调用且结果相同 | 4 / 6 |
//! | [`LoopDetectorKind::InvalidArgumentRepeat`] | 参数校验失败且调用相同 | 2 / 3 |
//! | [`LoopDetectorKind::ExecutionFailureRepeat`] | 执行失败且调用相同 | 3 / 5 |
//! | [`LoopDetectorKind::RepeatedThought`] | 连续相同 thinking | 2 / 3 |
//! | [`LoopDetectorKind::ContentChanting`] | 短块高频重复 | 6 / 10 |
//! | [`LoopDetectorKind::LongCycleRepetition`] | 长块逐字重复 | 2 / 4 |
//! | [`LoopDetectorKind::TurnToolCallLimit`] | 单条 assistant 的 tool call 数 | 100 / 1000 |
//!
//! 每个阈值都按「先 Warn、再 Stop」工作：达到 `reminder_at` 时只产生提醒，达到 `stop_at`
//! 时硬停。默认阈值刻意保守（先观察），需要更严格时通过 [`LoopGuardConfig`] 调整。
//!
//! # repeat_key
//!
//! `repeat_key = sha256(tool_name + canonical_json(args))`，其中 `canonical_json` 递归排序
//! 对象键；顶层 `description` / `display` 字段在计算前剔除（它们只影响 UI，不应让同一动作
//! 看起来不同）。
//!
//! # 为什么提醒不在工具调用之间注入
//!
//! 正常的 transcript 形状是 `assistant(tool_calls)` → `toolResult`*。如果把提醒插在两者
//! 之间，provider 会看到一条孤立的 assistant tool call 或 system 消息，部分 provider 会直接
//! 拒绝请求。因此 [`LoopGuard::take_reminder`] 只缓存提醒，agent loop 在下一次请求前才追加。

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use sha2::{Digest, Sha256};

use kiana_ai::types::{AssistantContentBlock, AssistantMessage};

/// 检测器种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LoopDetectorKind {
    /// 连续相同工具调用（同一个 key 相邻出现）。
    ConsecutiveToolCalls,
    /// 全局重复（窗口内同一 key 的总次数）。
    GlobalRepeatedToolCalls,
    /// A-B 交替。
    AlternatingToolCalls,
    /// 同一文件重复读取。
    ReadFileLoop,
    /// shell 命令与输出同时停滞。
    ShellStagnation,
    /// 非 shell 动作与结果同时停滞。
    ActionStagnation,
    /// 非法参数重复。
    InvalidArgumentRepeat,
    /// 执行失败重复。
    ExecutionFailureRepeat,
    /// 重复 thought。
    RepeatedThought,
    /// 内容 chanting（短块高频重复）。
    ContentChanting,
    /// 长周期逐字重复。
    LongCycleRepetition,
    /// 单条 assistant 消息的工具调用总量。
    TurnToolCallLimit,
}

impl LoopDetectorKind {
    /// 稳定字符串（日志与提醒文本）。
    pub fn as_str(self) -> &'static str {
        match self {
            LoopDetectorKind::ConsecutiveToolCalls => "consecutive_tool_calls",
            LoopDetectorKind::GlobalRepeatedToolCalls => "global_repeated_tool_calls",
            LoopDetectorKind::AlternatingToolCalls => "alternating_tool_calls",
            LoopDetectorKind::ReadFileLoop => "read_file_loop",
            LoopDetectorKind::ShellStagnation => "shell_stagnation",
            LoopDetectorKind::ActionStagnation => "action_stagnation",
            LoopDetectorKind::InvalidArgumentRepeat => "invalid_argument_repeat",
            LoopDetectorKind::ExecutionFailureRepeat => "execution_failure_repeat",
            LoopDetectorKind::RepeatedThought => "repeated_thought",
            LoopDetectorKind::ContentChanting => "content_chanting",
            LoopDetectorKind::LongCycleRepetition => "long_cycle_repetition",
            LoopDetectorKind::TurnToolCallLimit => "turn_tool_call_limit",
        }
    }
}

/// 一个检测器的阈值。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DetectorThreshold {
    /// 达到该计数时发提醒。
    pub reminder_at: u32,
    /// 达到该计数时硬停。
    pub stop_at: u32,
}

impl DetectorThreshold {
    /// 构造并校验 `reminder_at <= stop_at`（非法输入把 stop 抬到 reminder）。
    pub fn new(reminder_at: u32, stop_at: u32) -> Self {
        let reminder_at = reminder_at.max(1);
        DetectorThreshold {
            reminder_at,
            stop_at: stop_at.max(reminder_at),
        }
    }
}

/// loop guard 配置。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoopGuardConfig {
    /// 总开关；关闭后所有观察都返回 [`LoopGuardOutcome::Continue`]。
    pub enabled: bool,
    /// 默认行为：`true` 时达到 `stop_at` 立即硬停；`false` 时先 Warn。
    pub stop_on_detection: bool,
    /// 每种检测器的阈值（缺省用 [`LoopGuardConfig::default_thresholds`]）。
    pub thresholds: BTreeMap<LoopDetectorKind, DetectorThreshold>,
    /// 单条 assistant 消息的软工具调用上限。
    pub soft_tool_calls_per_turn: u32,
    /// 硬上限相对软上限的倍数。
    pub hard_tool_call_multiplier: u32,
    /// 全局重复检测窗口（最近 N 次工具调用）。
    pub global_window: usize,
}

impl Default for LoopGuardConfig {
    fn default() -> Self {
        LoopGuardConfig {
            enabled: true,
            stop_on_detection: false,
            thresholds: Self::default_thresholds(),
            soft_tool_calls_per_turn: 100,
            hard_tool_call_multiplier: 10,
            global_window: 100,
        }
    }
}

impl LoopGuardConfig {
    /// 默认阈值表。
    pub fn default_thresholds() -> BTreeMap<LoopDetectorKind, DetectorThreshold> {
        use LoopDetectorKind::*;
        let mut thresholds = BTreeMap::new();
        thresholds.insert(ConsecutiveToolCalls, DetectorThreshold::new(3, 5));
        thresholds.insert(GlobalRepeatedToolCalls, DetectorThreshold::new(8, 15));
        thresholds.insert(AlternatingToolCalls, DetectorThreshold::new(3, 5));
        thresholds.insert(ReadFileLoop, DetectorThreshold::new(4, 8));
        thresholds.insert(ShellStagnation, DetectorThreshold::new(2, 4));
        thresholds.insert(ActionStagnation, DetectorThreshold::new(4, 6));
        thresholds.insert(InvalidArgumentRepeat, DetectorThreshold::new(2, 3));
        thresholds.insert(ExecutionFailureRepeat, DetectorThreshold::new(3, 5));
        thresholds.insert(RepeatedThought, DetectorThreshold::new(2, 3));
        thresholds.insert(ContentChanting, DetectorThreshold::new(6, 10));
        thresholds.insert(LongCycleRepetition, DetectorThreshold::new(2, 4));
        thresholds.insert(TurnToolCallLimit, DetectorThreshold::new(100, 1_000));
        thresholds
    }

    /// 覆盖一个检测器的阈值。
    pub fn with_threshold(mut self, kind: LoopDetectorKind, threshold: DetectorThreshold) -> Self {
        self.thresholds.insert(kind, threshold);
        self
    }

    fn threshold(&self, kind: LoopDetectorKind) -> DetectorThreshold {
        self.thresholds
            .get(&kind)
            .copied()
            .unwrap_or_else(|| DetectorThreshold::new(2, 3))
    }
}

/// 一次命中。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoopDetection {
    /// 检测器。
    pub kind: LoopDetectorKind,
    /// 命中次数（达到的阈值计数）。
    pub count: u32,
    /// 触发提醒的阈值。
    pub reminder_at: u32,
    /// 硬停阈值。
    pub stop_at: u32,
    /// `repeat_key`（工具类检测器）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeat_key: Option<String>,
    /// 工具名（工具类检测器）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    /// 人类可读描述。
    pub message: String,
}

/// 观察结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "action")]
pub enum LoopGuardOutcome {
    /// 正常。
    Continue,
    /// 达到 reminder 阈值：提醒已缓存，下一次请求前注入。
    Warn(LoopDetection),
    /// 达到 stop 阈值：agent loop 应硬停。
    Stop(LoopDetection),
}

impl LoopGuardOutcome {
    /// 是否为硬停。
    pub fn is_stop(&self) -> bool {
        matches!(self, LoopGuardOutcome::Stop(_))
    }

    /// 合并两个结果，Stop 优先、其次 Warn。
    pub fn merge(self, other: LoopGuardOutcome) -> LoopGuardOutcome {
        match (self, other) {
            (LoopGuardOutcome::Stop(detection), _) => LoopGuardOutcome::Stop(detection),
            (_, LoopGuardOutcome::Stop(detection)) => LoopGuardOutcome::Stop(detection),
            (LoopGuardOutcome::Warn(detection), _) => LoopGuardOutcome::Warn(detection),
            (_, LoopGuardOutcome::Warn(detection)) => LoopGuardOutcome::Warn(detection),
            _ => LoopGuardOutcome::Continue,
        }
    }
}

/// 一次工具调用的历史记录。
#[derive(Debug, Clone)]
struct ToolCallRecord {
    key: String,
    read_target: Option<String>,
    shell: bool,
    output_hash: Option<String>,
    is_error: bool,
    invalid_arguments: bool,
    result_seen: bool,
}

/// 共享句柄：agent loop 与调用方共用一个 guard。
pub type LoopGuardHandle = Arc<std::sync::Mutex<LoopGuard>>;

/// loop guard 状态机。
#[derive(Debug, Clone)]
pub struct LoopGuard {
    config: LoopGuardConfig,
    history: VecDeque<ToolCallRecord>,
    /// 还没有收到 tool result 的调用（同一 key 可能在同一批里出现多次，按 FIFO 取）。
    pending_result: HashMap<String, Vec<usize>>,
    /// 已发过提醒的 `(kind, key)`，避免同一动作每轮都提醒。
    warned: HashSet<(LoopDetectorKind, String)>,
    pending_reminder: Option<String>,
    stopped: Option<LoopDetection>,
    last_thinking: Option<String>,
    consecutive_thinking: u32,
    /// assistant 文本（用于 chanting / 长周期重复）。
    content_history: String,
    content_processed: usize,
    /// 已处理的 64 字符块的哈希 → 位置。
    content_chunks: HashMap<String, Vec<usize>>,
    /// 当前是否处在 Markdown 围栏代码块内（跨 `observe_content` 调用保持）。
    content_in_code_block: bool,
    /// 最近一次 turn 的工具调用数。
    turn_tool_calls: u32,
}

impl Default for LoopGuard {
    fn default() -> Self {
        LoopGuard::new(LoopGuardConfig::default())
    }
}

impl LoopGuard {
    /// 构造。
    pub fn new(config: LoopGuardConfig) -> Self {
        LoopGuard {
            config,
            history: VecDeque::new(),
            pending_result: HashMap::new(),
            warned: HashSet::new(),
            pending_reminder: None,
            stopped: None,
            last_thinking: None,
            consecutive_thinking: 0,
            content_history: String::new(),
            content_processed: 0,
            content_chunks: HashMap::new(),
            content_in_code_block: false,
            turn_tool_calls: 0,
        }
    }

    /// 当前配置。
    pub fn config(&self) -> &LoopGuardConfig {
        &self.config
    }

    /// 已硬停时返回命中详情。
    pub fn stopped(&self) -> Option<&LoopDetection> {
        self.stopped.as_ref()
    }

    /// 取出待注入的提醒（只返回一次）。
    pub fn take_reminder(&mut self) -> Option<String> {
        self.pending_reminder.take()
    }

    /// 清空会话历史（保留配置与已停状态）。
    pub fn clear_history(&mut self) {
        self.history.clear();
        self.pending_result.clear();
        self.warned.clear();
        self.pending_reminder = None;
        self.last_thinking = None;
        self.consecutive_thinking = 0;
        self.content_history.clear();
        self.content_processed = 0;
        self.content_chunks.clear();
        self.content_in_code_block = false;
        self.turn_tool_calls = 0;
    }

    /// 观察一条 assistant 消息：工具调用、thinking 与文本。
    pub fn observe_assistant(&mut self, message: &AssistantMessage) -> LoopGuardOutcome {
        if !self.config.enabled {
            return LoopGuardOutcome::Continue;
        }
        if let Some(detection) = &self.stopped {
            return LoopGuardOutcome::Stop(detection.clone());
        }

        let mut outcome = LoopGuardOutcome::Continue;
        let tool_calls: Vec<&kiana_ai::types::ToolCall> = message
            .content
            .iter()
            .filter_map(|block| match block {
                AssistantContentBlock::ToolCall(call) => Some(call),
                _ => None,
            })
            .collect();

        self.turn_tool_calls = tool_calls.len() as u32;

        for call in &tool_calls {
            let outcome_for_call = self.observe_tool_call(&call.name, &call.arguments);
            outcome = outcome.merge(outcome_for_call);
            if matches!(outcome, LoopGuardOutcome::Stop(_)) {
                return outcome;
            }
        }

        // 同一批工具调用达到软/硬上限：按「本 turn 的工具调用数」处理。
        //
        // 阈值有两个来源：`soft_tool_calls_per_turn` + `hard_tool_call_multiplier`，
        // 以及 `thresholds[TurnToolCallLimit]`。两者都可能被单独配置，取更严格（更小）
        // 的那个，否则显式配置的 `stop_at` 会变成死字段。
        let limit = self.config.threshold(LoopDetectorKind::TurnToolCallLimit);
        // 0 表示“未配置”而不是“每条消息都停”：与 `DetectorThreshold::new` 一样抬到 1。
        let soft = self
            .config
            .soft_tool_calls_per_turn
            .max(1)
            .min(limit.reminder_at.max(1));
        let hard = self
            .config
            .soft_tool_calls_per_turn
            .saturating_mul(self.config.hard_tool_call_multiplier.max(1))
            .min(limit.stop_at)
            .max(soft);
        // "达到"阈值即触发（`>=`）：恰好等于 hard 也必须停下。
        if self.turn_tool_calls >= hard {
            let detection = self.detection(
                LoopDetectorKind::TurnToolCallLimit,
                self.turn_tool_calls,
                DetectorThreshold {
                    reminder_at: soft,
                    stop_at: hard,
                },
                None,
                None,
                format!(
                    "assistant turn issued {} tool calls (hard limit {hard}); break the batch into smaller steps",
                    self.turn_tool_calls
                ),
            );
            return self.stop_or_warn(detection);
        }
        if self.turn_tool_calls >= soft {
            let detection = self.detection(
                LoopDetectorKind::TurnToolCallLimit,
                self.turn_tool_calls,
                DetectorThreshold {
                    reminder_at: soft,
                    stop_at: hard,
                },
                None,
                None,
                format!(
                    "assistant turn issued {} tool calls (soft limit {soft}); consider a smaller batch",
                    self.turn_tool_calls
                ),
            );
            outcome = outcome.merge(self.warn_only(detection));
        }

        // 思考内容重复。没有 thinking 的 assistant 消息会打断“连续相同思考”，
        // 必须重置计数，否则“有思考 → 纯文本 → 同样思考”会被误判成连续复读。
        let thinking_text = assistant_thinking(message);
        if thinking_text.trim().is_empty() {
            self.consecutive_thinking = 0;
            self.last_thinking = None;
        } else {
            outcome = outcome.merge(self.observe_thinking(&thinking_text));
        }

        // assistant 可见文本：chanting 与长周期重复。
        let text = assistant_text(message);
        if !text.trim().is_empty() {
            outcome = outcome.merge(self.observe_content(&text));
        }

        outcome
    }

    /// 观察一次工具调用的参数（在真正执行之前调用）。
    pub fn observe_tool_call(
        &mut self,
        tool_name: &str,
        arguments: &indexmap::IndexMap<String, JsonValue>,
    ) -> LoopGuardOutcome {
        if !self.config.enabled {
            return LoopGuardOutcome::Continue;
        }
        if let Some(detection) = &self.stopped {
            return LoopGuardOutcome::Stop(detection.clone());
        }

        let key = repeat_key(tool_name, arguments);
        let record = ToolCallRecord {
            key: key.clone(),
            read_target: read_target(tool_name, arguments),
            shell: is_shell_tool(tool_name),
            output_hash: None,
            is_error: false,
            invalid_arguments: false,
            result_seen: false,
        };
        let index = self.history.len();
        self.history.push_back(record);
        self.pending_result
            .entry(key.clone())
            .or_default()
            .push(index);
        while self.history.len() > self.config.global_window.max(16) {
            let removed = self.history.pop_front();
            if let Some(removed) = removed
                && let Some(positions) = self.pending_result.get_mut(&removed.key)
            {
                positions.retain(|position| *position != 0);
                if positions.is_empty() {
                    self.pending_result.remove(&removed.key);
                }
            }
            // 位置索引整体左移一位。
            for position in self.pending_result.values_mut().flatten() {
                *position = position.saturating_sub(1);
            }
        }

        let mut outcome = self.observe_consecutive(&key, tool_name);
        if matches!(outcome, LoopGuardOutcome::Stop(_)) {
            return outcome;
        }
        outcome = outcome.merge(self.observe_global(&key, tool_name));
        if matches!(outcome, LoopGuardOutcome::Stop(_)) {
            return outcome;
        }
        outcome = outcome.merge(self.observe_alternating(tool_name));
        if matches!(outcome, LoopGuardOutcome::Stop(_)) {
            return outcome;
        }
        outcome = outcome.merge(self.observe_read_file(tool_name));
        outcome
    }

    /// 观察一次工具结果（执行之后调用）。
    pub fn observe_tool_result(
        &mut self,
        tool_name: &str,
        arguments: &indexmap::IndexMap<String, JsonValue>,
        output_text: &str,
        is_error: bool,
    ) -> LoopGuardOutcome {
        if !self.config.enabled {
            return LoopGuardOutcome::Continue;
        }
        if let Some(detection) = &self.stopped {
            return LoopGuardOutcome::Stop(detection.clone());
        }
        let key = repeat_key(tool_name, arguments);
        let Some(position) = self
            .pending_result
            .get_mut(&key)
            .and_then(|positions| positions.first().copied())
        else {
            return LoopGuardOutcome::Continue;
        };
        if let Some(positions) = self.pending_result.get_mut(&key) {
            positions.remove(0);
            if positions.is_empty() {
                self.pending_result.remove(&key);
            }
        }
        let Some(record) = self.history.get_mut(position) else {
            return LoopGuardOutcome::Continue;
        };
        record.output_hash = Some(hash_text(output_text));
        record.is_error = is_error;
        record.invalid_arguments = is_error && looks_like_invalid_arguments(output_text);
        record.result_seen = true;
        let shell = record.shell;
        let invalid_arguments = record.invalid_arguments;

        let mut outcome = LoopGuardOutcome::Continue;
        if shell {
            outcome = outcome.merge(self.observe_shell_stagnation(&key, tool_name));
        } else {
            outcome = outcome.merge(self.observe_action_stagnation(&key, tool_name));
        }
        if invalid_arguments {
            outcome = outcome.merge(self.observe_invalid_arguments(&key, tool_name));
        }
        if is_error {
            outcome = outcome.merge(self.observe_execution_failure(&key, tool_name));
        }
        outcome
    }

    /// 供预算/会话层查询的摘要。
    pub fn snapshot(&self) -> LoopGuardSnapshot {
        LoopGuardSnapshot {
            observed_tool_calls: self.history.len(),
            pending_reminder: self.pending_reminder.clone(),
            stopped: self.stopped.clone(),
        }
    }

    // -----------------------------------------------------------------
    // 各检测器
    // -----------------------------------------------------------------

    fn observe_consecutive(&mut self, key: &str, tool_name: &str) -> LoopGuardOutcome {
        let count = self
            .history
            .iter()
            .rev()
            .take_while(|record| record.key == key)
            .count() as u32;
        let threshold = self
            .config
            .threshold(LoopDetectorKind::ConsecutiveToolCalls);
        if count < threshold.reminder_at {
            return LoopGuardOutcome::Continue;
        }
        let detection = self.detection(
            LoopDetectorKind::ConsecutiveToolCalls,
            count,
            threshold,
            Some(key),
            Some(tool_name),
            format!("tool `{tool_name}` called with identical arguments {count} times in a row"),
        );
        self.stop_or_warn(detection)
    }

    fn observe_global(&mut self, key: &str, tool_name: &str) -> LoopGuardOutcome {
        let count = self
            .history
            .iter()
            .filter(|record| record.key == key)
            .count() as u32;
        let threshold = self
            .config
            .threshold(LoopDetectorKind::GlobalRepeatedToolCalls);
        if count < threshold.reminder_at {
            return LoopGuardOutcome::Continue;
        }
        let detection = self.detection(
            LoopDetectorKind::GlobalRepeatedToolCalls,
            count,
            threshold,
            Some(key),
            Some(tool_name),
            format!(
                "tool `{tool_name}` called with identical arguments {count} times in the recent window"
            ),
        );
        self.stop_or_warn(detection)
    }

    fn observe_alternating(&mut self, tool_name: &str) -> LoopGuardOutcome {
        let keys: Vec<&str> = self
            .history
            .iter()
            .map(|record| record.key.as_str())
            .collect();
        if keys.len() < 4 {
            return LoopGuardOutcome::Continue;
        }
        let last = keys[keys.len() - 1];
        let previous = keys[keys.len() - 2];
        if last == previous {
            return LoopGuardOutcome::Continue;
        }
        let mut length = 0usize;
        let mut expected = last;
        for index in (0..keys.len()).rev() {
            if keys[index] == expected {
                length += 1;
                expected = if expected == last { previous } else { last };
            } else {
                break;
            }
        }
        let cycles = (length / 2) as u32;
        let threshold = self
            .config
            .threshold(LoopDetectorKind::AlternatingToolCalls);
        if cycles < threshold.reminder_at {
            return LoopGuardOutcome::Continue;
        }
        let detection = self.detection(
            LoopDetectorKind::AlternatingToolCalls,
            cycles,
            threshold,
            Some(last),
            Some(tool_name),
            format!(
                "tools alternate between two identical calls for {cycles} cycles; change the approach"
            ),
        );
        self.stop_or_warn(detection)
    }

    fn observe_read_file(&mut self, tool_name: &str) -> LoopGuardOutcome {
        let Some(target) = self
            .history
            .back()
            .and_then(|record| record.read_target.clone())
        else {
            return LoopGuardOutcome::Continue;
        };
        let count = self
            .history
            .iter()
            .rev()
            .take_while(|record| record.read_target.as_deref() == Some(target.as_str()))
            .count() as u32;
        let threshold = self.config.threshold(LoopDetectorKind::ReadFileLoop);
        if count < threshold.reminder_at {
            return LoopGuardOutcome::Continue;
        }
        let detection = self.detection(
            LoopDetectorKind::ReadFileLoop,
            count,
            threshold,
            None,
            Some(tool_name),
            format!("file `{target}` read {count} times in a row without other progress"),
        );
        self.stop_or_warn(detection)
    }

    fn observe_shell_stagnation(&mut self, key: &str, tool_name: &str) -> LoopGuardOutcome {
        let count = self
            .history
            .iter()
            .rev()
            .take_while(|record| record.shell && record.key == key && record.output_hash.is_some())
            .count() as u32;
        let threshold = self.config.threshold(LoopDetectorKind::ShellStagnation);
        if count < threshold.reminder_at {
            return LoopGuardOutcome::Continue;
        }
        // 连续段里输出必须逐字相同；只要有一次变化就视为有进展。
        let outputs: Vec<&str> = self
            .history
            .iter()
            .rev()
            .take(count as usize)
            .filter_map(|record| record.output_hash.as_deref())
            .collect();
        if outputs
            .first()
            .is_none_or(|first| outputs.iter().any(|candidate| *candidate != *first))
        {
            return LoopGuardOutcome::Continue;
        }
        let detection = self.detection(
            LoopDetectorKind::ShellStagnation,
            count,
            threshold,
            Some(key),
            Some(tool_name),
            format!(
                "shell command produced identical output {count} times in a row; polling without state change"
            ),
        );
        self.stop_or_warn(detection)
    }

    fn observe_action_stagnation(&mut self, key: &str, tool_name: &str) -> LoopGuardOutcome {
        let count = self
            .history
            .iter()
            .rev()
            .take_while(|record| !record.shell && record.key == key && record.output_hash.is_some())
            .count() as u32;
        let threshold = self.config.threshold(LoopDetectorKind::ActionStagnation);
        if count < threshold.reminder_at {
            return LoopGuardOutcome::Continue;
        }
        let outputs: Vec<&str> = self
            .history
            .iter()
            .rev()
            .take(count as usize)
            .filter_map(|record| record.output_hash.as_deref())
            .collect();
        if outputs
            .first()
            .is_none_or(|first| outputs.iter().any(|candidate| *candidate != *first))
        {
            return LoopGuardOutcome::Continue;
        }
        let detection = self.detection(
            LoopDetectorKind::ActionStagnation,
            count,
            threshold,
            Some(key),
            Some(tool_name),
            format!(
                "tool `{tool_name}` produced the same result {count} times without changing state"
            ),
        );
        self.stop_or_warn(detection)
    }

    fn observe_invalid_arguments(&mut self, key: &str, tool_name: &str) -> LoopGuardOutcome {
        let count = self
            .history
            .iter()
            .rev()
            .take_while(|record| record.key == key && record.invalid_arguments)
            .count() as u32;
        let threshold = self
            .config
            .threshold(LoopDetectorKind::InvalidArgumentRepeat);
        if count < threshold.reminder_at {
            return LoopGuardOutcome::Continue;
        }
        let detection = self.detection(
            LoopDetectorKind::InvalidArgumentRepeat,
            count,
            threshold,
            Some(key),
            Some(tool_name),
            format!("tool `{tool_name}` rejected its arguments {count} times in a row"),
        );
        self.stop_or_warn(detection)
    }

    fn observe_execution_failure(&mut self, key: &str, tool_name: &str) -> LoopGuardOutcome {
        let count = self
            .history
            .iter()
            .rev()
            .take_while(|record| record.key == key && record.is_error)
            .count() as u32;
        let threshold = self
            .config
            .threshold(LoopDetectorKind::ExecutionFailureRepeat);
        if count < threshold.reminder_at {
            return LoopGuardOutcome::Continue;
        }
        let detection = self.detection(
            LoopDetectorKind::ExecutionFailureRepeat,
            count,
            threshold,
            Some(key),
            Some(tool_name),
            format!("tool `{tool_name}` failed {count} times in a row with the same arguments"),
        );
        self.stop_or_warn(detection)
    }

    fn observe_thinking(&mut self, text: &str) -> LoopGuardOutcome {
        let normalized = normalize_text(text);
        if self.last_thinking.as_deref() == Some(normalized.as_str()) {
            self.consecutive_thinking += 1;
        } else {
            self.consecutive_thinking = 1;
            self.last_thinking = Some(normalized);
        }
        let threshold = self.config.threshold(LoopDetectorKind::RepeatedThought);
        if self.consecutive_thinking < threshold.reminder_at {
            return LoopGuardOutcome::Continue;
        }
        let detection = self.detection(
            LoopDetectorKind::RepeatedThought,
            self.consecutive_thinking,
            threshold,
            None,
            None,
            format!(
                "assistant repeated the same thinking {} times in a row",
                self.consecutive_thinking
            ),
        );
        self.stop_or_warn(detection)
    }

    fn observe_content(&mut self, text: &str) -> LoopGuardOutcome {
        // 与参考实现（gemini-cli `checkContentLoop`）一致：先剔除围栏代码块，并在
        // 表格/列表/标题/引用/分隔线等结构边界上重置统计。整块消息里的代码与排版
        // 重复不应被当成“模型在复读”。
        let (visible, in_code_block) = strip_code_fences(text, self.content_in_code_block);
        self.content_in_code_block = in_code_block;
        let mut outcome = LoopGuardOutcome::Continue;
        // 参考实现按流式 delta 处理；这里是整块 assistant 文本，因此按行近似：结构行
        // 的边界语义是“重置并跳过”（参考实现里这类 delta 之后也几乎不会形成 64 字符
        // 窗口），普通行累积后参与窗口检测。
        for line in visible.split_inclusive('\n') {
            if is_markdown_divider(line) || is_markdown_structural(line) {
                self.reset_content_tracking();
                continue;
            }
            self.append_content(line);
            outcome = outcome.merge(self.detect_content_chanting());
            if matches!(outcome, LoopGuardOutcome::Stop(_)) {
                return outcome;
            }
            outcome = outcome.merge(self.detect_long_cycle());
            if matches!(outcome, LoopGuardOutcome::Stop(_)) {
                return outcome;
            }
        }
        outcome
    }

    fn append_content(&mut self, text: &str) {
        self.content_history.push_str(text);
        // 上限 8000 字符：足够覆盖两次长周期重复，又不会无限增长。
        const MAX_HISTORY: usize = 8_000;
        if self.content_history.len() > MAX_HISTORY {
            let excess = self.content_history.len() - MAX_HISTORY;
            // `String::drain` 需要 UTF-8 边界；从 excess 起向后找到最近的安全切点。
            let mut split = excess;
            while split < self.content_history.len()
                && !self.content_history.is_char_boundary(split)
            {
                split += 1;
            }
            self.content_history.drain(..split);
            self.content_processed = self.content_processed.saturating_sub(split);
            for positions in self.content_chunks.values_mut() {
                positions.retain(|position| *position >= split);
                for position in positions.iter_mut() {
                    *position -= split;
                }
            }
            // 所有位置都落在被裁掉的前缀里之后，这个 hash 不会再命中；不清理会让
            // 每个历史 64 字节块永久占一个 HashMap 条目。
            self.content_chunks
                .retain(|_, positions| !positions.is_empty());
        }
    }

    fn reset_content_tracking(&mut self) {
        self.content_history.clear();
        self.content_processed = 0;
        self.content_chunks.clear();
    }

    fn detect_content_chanting(&mut self) -> LoopGuardOutcome {
        const CHUNK: usize = 64;
        const MAX_DISTANCE: usize = CHUNK * 5;
        let mut outcome = LoopGuardOutcome::Continue;
        let history_len = self.content_history.len();
        while self.content_processed + CHUNK <= history_len {
            // 仅在 UTF-8 边界上切块，避免把多字节字符切开。这里从当前游标向后最多
            // 走 3 个字节，不能每轮都从 0 扫描 `char_indices()`（那是 O(n²)）。
            let mut start = self.content_processed;
            while start < history_len && !self.content_history.is_char_boundary(start) {
                start += 1;
            }
            if start + CHUNK > history_len {
                break;
            }
            let mut end = start + CHUNK;
            while end < history_len && !self.content_history.is_char_boundary(end) {
                end += 1;
            }
            if end - start < CHUNK / 2 {
                break;
            }
            let chunk = &self.content_history[start..end];
            let hash = hash_text(chunk);
            let positions = self.content_chunks.entry(hash).or_default();
            positions.push(start);
            let threshold = self.config.threshold(LoopDetectorKind::ContentChanting);
            if positions.len() as u32 >= threshold.reminder_at {
                let recent: Vec<usize> = positions
                    .iter()
                    .rev()
                    .take(threshold.stop_at as usize)
                    .copied()
                    .collect();
                let mut recent = recent;
                recent.reverse();
                let distance = recent
                    .last()
                    .copied()
                    .unwrap_or(start)
                    .saturating_sub(recent.first().copied().unwrap_or(start));
                let average = if recent.len() > 1 {
                    distance / (recent.len() - 1)
                } else {
                    distance
                };
                if average <= MAX_DISTANCE {
                    let count = recent.len() as u32;
                    let detection = self.detection(
                        LoopDetectorKind::ContentChanting,
                        count,
                        threshold,
                        None,
                        None,
                        format!(
                            "assistant content repeats the same {CHUNK}-char chunk {count} times in a short window"
                        ),
                    );
                    outcome = outcome.merge(self.stop_or_warn(detection));
                }
            }
            self.content_processed = start + 1;
        }
        outcome
    }

    fn detect_long_cycle(&mut self) -> LoopGuardOutcome {
        const LONG_CHUNK: usize = 160;
        const WINDOW: usize = 4_000;
        if self.content_processed < LONG_CHUNK {
            return LoopGuardOutcome::Continue;
        }
        let history_len = self.content_history.len();
        if history_len < LONG_CHUNK * 2 {
            return LoopGuardOutcome::Continue;
        }
        let mut start = history_len - LONG_CHUNK;
        while start < history_len && !self.content_history.is_char_boundary(start) {
            start += 1;
        }
        if start + LONG_CHUNK > history_len {
            return LoopGuardOutcome::Continue;
        }
        let tail = &self.content_history[start..history_len];
        // 向前搜索同一段长文本；只在窗口内找，避免把正常重复的模板算成循环。
        let search_start = start.saturating_sub(WINDOW);
        let mut window_start = search_start;
        while window_start < start && !self.content_history.is_char_boundary(window_start) {
            window_start += 1;
        }
        let search_window = &self.content_history[window_start..start];
        let Some(offset) = search_window.rfind(tail) else {
            return LoopGuardOutcome::Continue;
        };
        let found = window_start + offset;
        // 出现第二次记为 2，第三次记为 3；只统计尾部连续重复。
        let mut count = 2u32;
        let mut cursor = found;
        while cursor >= LONG_CHUNK {
            let previous_start = cursor - LONG_CHUNK;
            if previous_start < window_start
                || !self.content_history.is_char_boundary(previous_start)
                || !self.content_history[previous_start..].starts_with(tail)
            {
                break;
            }
            count += 1;
            cursor = previous_start;
        }
        let threshold = self.config.threshold(LoopDetectorKind::LongCycleRepetition);
        if count < threshold.reminder_at {
            return LoopGuardOutcome::Continue;
        }
        let detection = self.detection(
            LoopDetectorKind::LongCycleRepetition,
            count,
            threshold,
            None,
            None,
            format!("assistant repeated a {LONG_CHUNK}-char block {count} times verbatim"),
        );
        self.stop_or_warn(detection)
    }

    // -----------------------------------------------------------------
    // 结果构造
    // -----------------------------------------------------------------

    fn detection(
        &self,
        kind: LoopDetectorKind,
        count: u32,
        threshold: DetectorThreshold,
        repeat_key: Option<&str>,
        tool_name: Option<&str>,
        message: String,
    ) -> LoopDetection {
        LoopDetection {
            kind,
            count,
            reminder_at: threshold.reminder_at,
            stop_at: threshold.stop_at,
            repeat_key: repeat_key.map(str::to_string),
            tool_name: tool_name.map(str::to_string),
            message,
        }
    }

    fn stop_or_warn(&mut self, detection: LoopDetection) -> LoopGuardOutcome {
        if detection.count >= detection.stop_at || self.config.stop_on_detection {
            self.stopped = Some(detection.clone());
            LoopGuardOutcome::Stop(detection)
        } else {
            self.warn_only(detection)
        }
    }

    fn warn_only(&mut self, detection: LoopDetection) -> LoopGuardOutcome {
        let key = detection
            .repeat_key
            .clone()
            .unwrap_or_else(|| detection.kind.as_str().to_string());
        if !self.warned.insert((detection.kind, key)) {
            return LoopGuardOutcome::Continue;
        }
        let reminder = format!(
            "<loop-guard kind=\"{}\" count=\"{}\" reminder-at=\"{}\" stop-at=\"{}\">{}\n\
             Stop repeating this action. Change the arguments, the approach, or explain why the \
             repetition is necessary before continuing.</loop-guard>",
            detection.kind.as_str(),
            detection.count,
            detection.reminder_at,
            detection.stop_at,
            detection.message
        );
        self.pending_reminder = Some(reminder);
        LoopGuardOutcome::Warn(detection)
    }
}

/// loop guard 摘要。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoopGuardSnapshot {
    /// 已观察的工具调用数。
    pub observed_tool_calls: usize,
    /// 尚未注入的提醒。
    pub pending_reminder: Option<String>,
    /// 硬停详情。
    pub stopped: Option<LoopDetection>,
}

/// `repeat_key = sha256(tool_name + canonical_json(args))`。
///
/// 顶层 `description` / `display` 在计算前剔除；对象键递归排序。
pub fn repeat_key(tool_name: &str, arguments: &indexmap::IndexMap<String, JsonValue>) -> String {
    let mut object = arguments.clone();
    object.shift_remove("description");
    object.shift_remove("display");
    let canonical = canonical_json(&JsonValue::Object(object.into_iter().collect()));
    let text = format!("{tool_name}{canonical}");
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    hex::encode(hasher.finalize())
}

/// 规范化 JSON：对象键递归排序，数组保持顺序。
pub fn canonical_json(value: &JsonValue) -> String {
    kiana_json::js_object::stringify(&sort_json(value))
}

fn sort_json(value: &JsonValue) -> JsonValue {
    match value {
        JsonValue::Object(object) => {
            let mut keys: Vec<&String> = object.keys().collect();
            keys.sort();
            let mut sorted = serde_json::Map::new();
            for key in keys {
                sorted.insert(key.clone(), sort_json(&object[key]));
            }
            JsonValue::Object(sorted)
        }
        JsonValue::Array(items) => JsonValue::Array(items.iter().map(sort_json).collect()),
        other => other.clone(),
    }
}

/// 去掉 Markdown 围栏代码块，返回 `(可分析文本, 是否仍在代码块内)`。
///
/// 围栏行本身也被丢弃；跨调用保持 `in_code_block`，因此一个块以 ```` ``` ```` 打开、
/// 下一个块才关闭时也不会把代码内容计入复读统计。
fn strip_code_fences(text: &str, in_code_block: bool) -> (String, bool) {
    let mut visible = String::new();
    let mut in_code = in_code_block;
    for line in text.split_inclusive('\n') {
        if line.trim_start().starts_with("```") {
            in_code = !in_code;
            continue;
        }
        if !in_code {
            visible.push_str(line);
        }
    }
    (visible, in_code)
}

/// 整块文本是否只是一条 Markdown 分隔线（`---+`、`===`、`***`、box drawing 等）。
fn is_markdown_divider(text: &str) -> bool {
    let trimmed = text.trim();
    !trimmed.is_empty()
        && trimmed.chars().all(|ch| {
            matches!(ch, '+' | '-' | '_' | '=' | '*') || ('\u{2500}'..='\u{257f}').contains(&ch)
        })
}

/// 是否包含表格/列表/标题/引用等结构元素。
///
/// 逐行检查即可：这些标记只在行首（可有缩进）生效。与参考实现的正则集合对应，
/// 用来在“不同排版元素”的边界上重置 64 字节窗口统计。
fn is_markdown_structural(text: &str) -> bool {
    text.lines().any(|line| {
        let trimmed = line.trim_start();
        if trimmed.is_empty() {
            return false;
        }
        if trimmed.starts_with('|') && trimmed[1..].contains('|') {
            return true;
        }
        let border = trimmed
            .chars()
            .take_while(|ch| matches!(ch, '|' | '+' | '-'))
            .count();
        if border >= 3 {
            return true;
        }
        let mut chars = trimmed.chars();
        match chars.next() {
            Some('*' | '-' | '+') => chars.next().is_some_and(char::is_whitespace),
            Some('#') => {
                let hashes = trimmed.chars().take_while(|ch| *ch == '#').count();
                trimmed[hashes..]
                    .chars()
                    .next()
                    .is_some_and(char::is_whitespace)
            }
            Some('>') => chars.next().is_some_and(char::is_whitespace),
            Some(ch) if ch.is_ascii_digit() => {
                let digits = trimmed.chars().take_while(|ch| ch.is_ascii_digit()).count();
                trimmed[digits..]
                    .strip_prefix('.')
                    .is_some_and(|rest| rest.chars().next().is_some_and(char::is_whitespace))
            }
            _ => false,
        }
    })
}

fn hash_text(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    hex::encode(hasher.finalize())
}

fn normalize_text(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn read_target(
    tool_name: &str,
    arguments: &indexmap::IndexMap<String, JsonValue>,
) -> Option<String> {
    let normalized = tool_name.to_ascii_lowercase();
    let is_read = matches!(
        normalized.as_str(),
        "read" | "read_file" | "readfile" | "read-file" | "read_file_tool"
    );
    if !is_read {
        return None;
    }
    for key in ["path", "file_path", "filePath", "file", "filepath"] {
        if let Some(JsonValue::String(path)) = arguments.get(key) {
            return Some(path.replace('\\', "/"));
        }
    }
    None
}

fn is_shell_tool(tool_name: &str) -> bool {
    matches!(
        tool_name.to_ascii_lowercase().as_str(),
        "bash" | "shell" | "execute_bash" | "run_shell" | "run_shell_command" | "powershell"
    )
}

fn looks_like_invalid_arguments(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    [
        "invalid arguments",
        "invalid argument",
        "schema validation",
        "validation failed",
        "failed to parse arguments",
        "arguments_error",
        "does not match schema",
        "is not of type",
        "missing required property",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

/// 从 assistant 消息里取文本（供调用方构造提醒时复用）。
pub fn assistant_text(message: &AssistantMessage) -> String {
    message
        .content
        .iter()
        .filter_map(|block| match block {
            AssistantContentBlock::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 从 assistant 消息里取 thinking 文本。
pub fn assistant_thinking(message: &AssistantMessage) -> String {
    message
        .content
        .iter()
        .filter_map(|block| match block {
            AssistantContentBlock::Thinking(thinking) => Some(thinking.thinking.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 工具结果文本（`ToolResultMessage.content` 的纯文本视图）。
pub fn tool_result_text(result: &kiana_ai::types::ToolResultMessage) -> String {
    kiana_ai::utils::text::content_text_user(&result.content, "\n")
}
