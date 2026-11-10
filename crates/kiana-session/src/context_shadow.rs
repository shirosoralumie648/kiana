//! 只读 Context Shadow（Phase 4）。
//!
//! Shadow 是 `context_report` 的非破坏性投影：它只捕获“下一次请求会包含什么”，
//! 不修改消息、工具或压缩状态。两次捕获可以 [`ContextShadow::diff`] 比较，宿主可
//! 在真正执行动作之前预览上下文变化。
//!
//! **不实现 Enforce**：按 ADR，Enforce 需要先补 `peek` API 并通过逐字节门禁，
//! 当前阶段只提供 Shadow。`is_enforced()` 恒为 `false`，避免调用方误以为影子
//! 会约束真实请求。

use std::collections::HashMap;

use kiana_agent_core::context_report::{ContextReport, ContextSegment};
use serde_json::Value;

/// 一次上下文影子快照。
#[derive(Debug, Clone, PartialEq)]
pub struct ContextShadow {
    report: ContextReport,
}

impl ContextShadow {
    /// 从现有的 `context_report` 捕获（只读）。
    pub fn capture(report: ContextReport) -> Self {
        Self { report }
    }

    /// 底层报告。
    pub fn report(&self) -> &ContextReport {
        &self.report
    }

    /// 下一次请求的总 token。
    pub fn total_tokens(&self) -> u64 {
        self.report.total_tokens
    }

    /// 上下文窗口使用率（`total / window`）。
    pub fn utilization(&self) -> f64 {
        self.report.utilization
    }

    /// 全部分段（工具 / prompt / turn-context / 普通消息）。
    pub fn segments(&self) -> Vec<&ContextSegment> {
        self.report.all_segments()
    }

    /// Shadow 永远不修改真实请求；Enforce 未实现。
    pub fn is_enforced(&self) -> bool {
        false
    }

    /// 与另一次影子快照比较。按 `(kind, label, role)` 分组、组内按出现顺序配对，
    /// 因此重复出现的同标签消息也能稳定比较。
    pub fn diff(&self, other: &ContextShadow) -> ContextShadowDiff {
        let before = group_segments(self.segments());
        let after = group_segments(other.segments());
        let mut keys: Vec<SegmentKey> = before.keys().chain(after.keys()).cloned().collect();
        keys.sort();
        keys.dedup();

        let mut diff = ContextShadowDiff {
            total_tokens_delta: i64::try_from(other.total_tokens())
                .unwrap_or(i64::MAX)
                .saturating_sub(i64::try_from(self.total_tokens()).unwrap_or(i64::MAX)),
            ..ContextShadowDiff::default()
        };
        for key in keys {
            let empty = Vec::new();
            let before_segments = before.get(&key).unwrap_or(&empty);
            let after_segments = after.get(&key).unwrap_or(&empty);
            let paired = before_segments.len().min(after_segments.len());
            for index in 0..paired {
                let before_segment = before_segments[index];
                let after_segment = after_segments[index];
                if before_segment.tokens != after_segment.tokens
                    || before_segment.chars != after_segment.chars
                {
                    diff.changed.push(ContextShadowChange {
                        kind: key.kind.clone(),
                        label: key.label.clone(),
                        role: key.role.clone(),
                        before_tokens: before_segment.tokens,
                        after_tokens: after_segment.tokens,
                        before_chars: before_segment.chars,
                        after_chars: after_segment.chars,
                    });
                }
            }
            diff.removed.extend(
                before_segments[paired..]
                    .iter()
                    .map(|segment| (*segment).clone()),
            );
            diff.added.extend(
                after_segments[paired..]
                    .iter()
                    .map(|segment| (*segment).clone()),
            );
        }
        diff
    }

    /// 序列化成 JSON 值（报告本身已是 camelCase）。
    pub fn to_json(&self) -> Value {
        serde_json::to_value(&self.report).unwrap_or(Value::Null)
    }

    /// `kiana-json` 编码（JS 数字/键序语义）。
    pub fn stringify(&self) -> String {
        kiana_json::stringify(&kiana_json::normalize_numbers(self.to_json()))
    }
}

/// 分段身份：类别 + 展示名 + 角色。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct SegmentKey {
    kind: String,
    label: String,
    role: String,
}

fn group_segments<'a>(
    segments: impl IntoIterator<Item = &'a ContextSegment>,
) -> HashMap<SegmentKey, Vec<&'a ContextSegment>> {
    let mut grouped: HashMap<SegmentKey, Vec<&ContextSegment>> = HashMap::new();
    for segment in segments {
        grouped
            .entry(SegmentKey {
                kind: segment.kind.as_str().to_string(),
                label: segment.label.clone(),
                role: segment.role.clone(),
            })
            .or_default()
            .push(segment);
    }
    grouped
}

/// 两次影子快照的差异。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ContextShadowDiff {
    /// 总 token 变化（`other - self`）。
    pub total_tokens_delta: i64,
    /// `other` 新增的分段。
    pub added: Vec<ContextSegment>,
    /// `other` 不再包含的分段。
    pub removed: Vec<ContextSegment>,
    /// 同身份分段的内容/长度变化。
    pub changed: Vec<ContextShadowChange>,
}

impl ContextShadowDiff {
    /// 是否没有任何变化。
    pub fn is_empty(&self) -> bool {
        self.total_tokens_delta == 0
            && self.added.is_empty()
            && self.removed.is_empty()
            && self.changed.is_empty()
    }
}

/// 同身份分段的变化。
#[derive(Debug, Clone, PartialEq)]
pub struct ContextShadowChange {
    /// 类别。
    pub kind: String,
    /// 展示名。
    pub label: String,
    /// 角色。
    pub role: String,
    /// 变化前 token。
    pub before_tokens: u64,
    /// 变化后 token。
    pub after_tokens: u64,
    /// 变化前字符数。
    pub before_chars: usize,
    /// 变化后字符数。
    pub after_chars: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use kiana_agent_core::context_report::ContextSegmentKind;

    fn segment(kind: ContextSegmentKind, label: &str, tokens: u64, chars: usize) -> ContextSegment {
        ContextSegment {
            kind,
            label: label.to_string(),
            role: "user".to_string(),
            tokens,
            preview: String::new(),
            chars,
        }
    }

    fn report(total: u64, segments: Vec<ContextSegment>) -> ContextReport {
        ContextReport {
            model: "model".to_string(),
            provider: "provider".to_string(),
            total_tokens: total,
            usage_tokens: 0,
            trailing_tokens: 0,
            context_window: Some(1000),
            utilization: total as f64 / 1000.0,
            tool_tokens: 0,
            prompt_tokens: 0,
            turn_context_tokens: 0,
            segment_tokens: 0,
            tools: Vec::new(),
            prompt: Vec::new(),
            turn_context: Vec::new(),
            segments,
            request_message_count: 0,
        }
    }

    #[test]
    fn shadow_is_read_only_and_reports_differences() {
        let before = ContextShadow::capture(report(
            10,
            vec![
                segment(ContextSegmentKind::Segment, "assistant", 4, 20),
                segment(ContextSegmentKind::Segment, "user", 6, 30),
            ],
        ));
        let after = ContextShadow::capture(report(
            14,
            vec![
                segment(ContextSegmentKind::Segment, "assistant", 7, 40),
                segment(ContextSegmentKind::Segment, "toolResult", 7, 50),
            ],
        ));
        assert!(!before.is_enforced());

        let diff = before.diff(&after);
        assert_eq!(diff.total_tokens_delta, 4);
        assert_eq!(diff.changed.len(), 1);
        assert_eq!(diff.changed[0].label, "assistant");
        assert_eq!(diff.changed[0].before_tokens, 4);
        assert_eq!(diff.changed[0].after_tokens, 7);
        assert_eq!(diff.removed.len(), 1);
        assert_eq!(diff.removed[0].label, "user");
        assert_eq!(diff.added.len(), 1);
        assert_eq!(diff.added[0].label, "toolResult");
        assert!(!diff.is_empty());
        assert!(before.diff(&before).is_empty());
    }

    #[test]
    fn duplicate_labels_pair_by_occurrence_order() {
        let before = ContextShadow::capture(report(
            6,
            vec![
                segment(ContextSegmentKind::Segment, "assistant", 3, 10),
                segment(ContextSegmentKind::Segment, "assistant", 3, 10),
            ],
        ));
        let after = ContextShadow::capture(report(
            6,
            vec![
                segment(ContextSegmentKind::Segment, "assistant", 3, 10),
                segment(ContextSegmentKind::Segment, "assistant", 3, 10),
                segment(ContextSegmentKind::Segment, "assistant", 0, 0),
            ],
        ));
        let diff = before.diff(&after);
        assert!(diff.changed.is_empty(), "{diff:?}");
        assert_eq!(diff.added.len(), 1);
        assert_eq!(diff.removed.len(), 0);
    }

    #[test]
    fn encoding_uses_kiana_json() {
        let shadow = ContextShadow::capture(report(1, Vec::new()));
        let encoded = shadow.stringify();
        assert!(encoded.contains("\"totalTokens\":1"), "{encoded}");
        assert!(encoded.contains("\"utilization\":0.001"), "{encoded}");
    }
}
