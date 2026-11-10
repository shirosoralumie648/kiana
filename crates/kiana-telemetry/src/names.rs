//! 会话遥测的指标名与低基数标签约定。
//!
//! 指标名集中在这里，避免字符串散落在各 crate；标签（attributes）必须低基数且不含内容：
//!
//! - **不要**把工具名、模型 id、参数、prompt、路径放进指标标签——它们基数高或包含敏感
//!   信息；工具名只允许出现在 span 属性（`gen_ai.tool.name`）里；
//! - 工具指标用 [`TOOL_KIND_*`] 三分类（builtin/mcp/other）；
//! - `cost_source` 用 `canonical`/`user_declared`/`provider_reported`；
//! - SSE 事件类型只记录有界的事件类别（`start`/`delta`/`end`/`error` 等），不透传原始
//!   JSON。
//!
//! 设计与命名参考 OTel GenAI 语义约定的 `gen_ai.*` 属性名；指标名按 Kiana 的
//! `kiana.*` 命名空间。

/// 会话轮次计数。
pub const SESSION_TURNS: &str = "kiana.session.turns";
/// turn 的 TTFT（首个 token 延迟，毫秒）。
pub const TURN_TTFT_MS: &str = "kiana.session.turn.ttft";
/// turn 的 TTFM（首个有意义 token 延迟，毫秒；跳过 thinking/空增量）。
pub const TURN_TTFM_MS: &str = "kiana.session.turn.ttfm";
/// API 调用耗时（毫秒）。
pub const API_CALL_DURATION_MS: &str = "kiana.api.call.duration";
/// API 调用 token 计数（标签 `type`）。
pub const API_CALL_TOKENS: &str = "kiana.api.call.tokens";
/// API 调用费用（USD，标签 `cost_source`）。
pub const API_CALL_COST_USD: &str = "kiana.api.call.cost";
/// SSE 事件计数（标签 `event.type`）。
pub const SSE_EVENTS: &str = "kiana.api.sse.events";
/// 工具调用耗时（毫秒，标签 `status`/`tool.kind`）。
pub const TOOL_CALL_DURATION_MS: &str = "kiana.tool.call.duration";
/// 工具调用计数（标签 `status`/`tool.kind`，**不含工具名**）。
pub const TOOL_CALLS: &str = "kiana.tool.calls";
/// 压缩耗时（毫秒，标签 `trigger`）。
pub const COMPACTION_DURATION_MS: &str = "kiana.compaction.duration";
/// 压缩计数（标签 `trigger`/`outcome`）。
pub const COMPACTIONS: &str = "kiana.compaction.count";
/// hook 执行耗时（毫秒，标签 `hook.event`）。
pub const HOOK_RUN_DURATION_MS: &str = "kiana.hook.run.duration";
/// hook 执行计数（标签 `hook.event`/`outcome`）。
pub const HOOK_RUNS: &str = "kiana.hook.runs";
/// 启动阶段耗时（毫秒，标签 `phase`）。
pub const STARTUP_PHASE_DURATION_MS: &str = "kiana.startup.phase.duration";

/// 工具来源：内置工具。
pub const TOOL_KIND_BUILTIN: &str = "builtin";
/// 工具来源：MCP/扩展工具。
pub const TOOL_KIND_MCP: &str = "mcp";
/// 工具来源：其它/未知。
pub const TOOL_KIND_OTHER: &str = "other";

/// 全部指标名（测试与诊断用）。
pub const ALL_METRIC_NAMES: &[&str] = &[
    SESSION_TURNS,
    TURN_TTFT_MS,
    TURN_TTFM_MS,
    API_CALL_DURATION_MS,
    API_CALL_TOKENS,
    API_CALL_COST_USD,
    SSE_EVENTS,
    TOOL_CALL_DURATION_MS,
    TOOL_CALLS,
    COMPACTION_DURATION_MS,
    COMPACTIONS,
    HOOK_RUN_DURATION_MS,
    HOOK_RUNS,
    STARTUP_PHASE_DURATION_MS,
];

/// 一个工具名的低基数分类。
///
/// 只看命名形状：`mcp__server__tool` / `server__tool` 归为 `mcp`；列出的内置工具归为
/// `builtin`；其余归为 `other`。返回 `'static` 文本，可直接作为指标标签。
pub fn tool_kind(tool_name: &str) -> &'static str {
    const BUILTIN: &[&str] = &[
        "bash",
        "powershell",
        "read",
        "write",
        "edit",
        "multiedit",
        "multi_edit",
        "applypatch",
        "apply_patch",
        "glob",
        "grep",
        "ls",
        "find",
        "web_fetch",
        "web_search",
        "task",
        "todo",
        "todo_write",
        "todowrite",
        "notebook_edit",
        "notebookedit",
    ];
    let normalized = tool_name.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return TOOL_KIND_OTHER;
    }
    if normalized.starts_with("mcp__") || normalized.contains("__") {
        return TOOL_KIND_MCP;
    }
    if BUILTIN.contains(&normalized.as_str()) {
        return TOOL_KIND_BUILTIN;
    }
    TOOL_KIND_OTHER
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metric_names_are_unique_and_namespaced() {
        let mut seen = std::collections::BTreeSet::new();
        for name in ALL_METRIC_NAMES {
            assert!(name.starts_with("kiana."), "{name}");
            assert!(seen.insert(*name), "duplicate metric name: {name}");
        }
    }

    #[test]
    fn tool_names_are_reduced_to_low_cardinality_kinds() {
        assert_eq!(tool_kind("bash"), TOOL_KIND_BUILTIN);
        assert_eq!(tool_kind("Read"), TOOL_KIND_BUILTIN);
        assert_eq!(tool_kind("apply_patch"), TOOL_KIND_BUILTIN);
        assert_eq!(tool_kind("mcp__github__create_issue"), TOOL_KIND_MCP);
        assert_eq!(tool_kind("github__create_issue"), TOOL_KIND_MCP);
        assert_eq!(tool_kind("some_custom_tool"), TOOL_KIND_OTHER);
        assert_eq!(tool_kind(""), TOOL_KIND_OTHER);
    }
}
