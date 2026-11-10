//! per-server 工具过滤：`enabledTools` / `disabledTools`（glob）。
//!
//! 设计参考 gemini-cli 的 `includeTools`/`excludeTools`（`mcp-client.ts`：exclude
//! 优先于 include）与 Codex 的命名空间过滤；Kiana 的配置键用 Claude Code 风格的
//! `enabledTools`/`disabledTools`，同时接受 snake_case 别名。
//!
//! # 语义
//!
//! - `disabledTools` 中**任意**模式命中 → 禁用（exclude 优先）；
//! - 否则 `enabledTools` 为空 → 启用；非空时只有命中**至少一个**模式才启用；
//! - 模式是 glob：`*`/`?`/`[abc]`/`**`，按整串匹配。非法 glob 退化为字面量匹配，
//!   不会让服务器启动失败；
//! - 过滤同时作用于**注册前**（模型 schema 里看不到）和**直接调用**（typed 拒绝
//!   `McpToolDisabledError`），避免通过 codemode / 记忆中的旧工具名绕过。
//!
//! 配置键位于 `McpServerConfig` 的未识别字段（`kiana-core` 的校验器原样放进 `extra`），
//! 因此这里从 `config_to_value` 读取，不修改 `kiana-core`。

use globset::{GlobBuilder, GlobMatcher};
use kiana_core::mcp_servers::McpServerConfig;
use kiana_mcp::auth_provider::McpTransportError;
use serde_json::Value;

use crate::mcp::config::McpServerEntry;

/// 允许列表键（camelCase）。
pub const ENABLED_TOOLS_KEY: &str = "enabledTools";
/// 禁止列表键（camelCase）。
pub const DISABLED_TOOLS_KEY: &str = "disabledTools";
/// 允许列表键的 snake_case 别名。
pub const ENABLED_TOOLS_SNAKE_KEY: &str = "enabled_tools";
/// 禁止列表键的 snake_case 别名。
pub const DISABLED_TOOLS_SNAKE_KEY: &str = "disabled_tools";
/// typed 拒绝的错误名。
pub const MCP_TOOL_DISABLED_ERROR: &str = "McpToolDisabledError";

/// 编译后的单条模式。
#[derive(Debug, Clone)]
struct ToolPattern {
    /// 原始模式（错误/诊断用）。
    raw: String,
    /// 编译后的 matcher；非法 glob 时是 `None`，走字面量比较。
    matcher: Option<GlobMatcher>,
}

impl ToolPattern {
    fn compile(raw: &str) -> Self {
        let matcher = GlobBuilder::new(raw)
            .literal_separator(false)
            .build()
            .ok()
            .map(|glob| glob.compile_matcher());
        ToolPattern {
            raw: raw.to_string(),
            matcher,
        }
    }

    fn matches(&self, tool: &str) -> bool {
        match &self.matcher {
            Some(matcher) => matcher.is_match(tool),
            None => self.raw == tool,
        }
    }
}

/// 一个服务器的工具过滤器。
#[derive(Debug, Clone, Default)]
pub struct McpToolFilter {
    /// 允许模式；空表示“未配置，全部允许”。
    enabled: Vec<ToolPattern>,
    /// 禁止模式；任意命中即禁用。
    disabled: Vec<ToolPattern>,
}

impl McpToolFilter {
    /// 从类型化配置构造（未识别键在 `config_to_value` 的结果里）。
    pub fn from_config(config: &McpServerConfig) -> Self {
        Self::from_value(&Value::Object(crate::mcp::config::config_to_value(config)))
    }

    /// 从配置 JSON 对象构造。
    ///
    /// 只接受字符串数组；非数组/非字符串条目被忽略（配置校验在更上层负责形状，
    /// 这里保持宽容以免一个坏条目让整个服务器不可用）。
    pub fn from_value(value: &Value) -> Self {
        let enabled = read_patterns(value, &[ENABLED_TOOLS_KEY, ENABLED_TOOLS_SNAKE_KEY]);
        let disabled = read_patterns(value, &[DISABLED_TOOLS_KEY, DISABLED_TOOLS_SNAKE_KEY]);
        McpToolFilter {
            enabled: enabled
                .iter()
                .map(|raw| ToolPattern::compile(raw))
                .collect(),
            disabled: disabled
                .iter()
                .map(|raw| ToolPattern::compile(raw))
                .collect(),
        }
    }

    /// 是否有任何过滤配置（用于测试/诊断）。
    pub fn is_configured(&self) -> bool {
        !self.enabled.is_empty() || !self.disabled.is_empty()
    }

    /// 该工具名是否允许。
    pub fn is_enabled(&self, tool: &str) -> bool {
        if self.disabled.iter().any(|pattern| pattern.matches(tool)) {
            return false;
        }
        self.enabled.is_empty() || self.enabled.iter().any(|pattern| pattern.matches(tool))
    }
}

/// 读取第一个存在的数组键（camelCase 优先）。
fn read_patterns(value: &Value, keys: &[&str]) -> Vec<String> {
    let Some(object) = value.as_object() else {
        return Vec::new();
    };
    for key in keys {
        if let Some(items) = object.get(*key).and_then(Value::as_array) {
            return items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect();
        }
    }
    Vec::new()
}

/// 注册/直接调用共用的判定：返回 typed 拒绝。
///
/// 错误 `name()` 是 [`MCP_TOOL_DISABLED_ERROR`]，调用方（agent loop）会把消息原样交给
/// 模型；`McpTransportError` 的其它接口（status/body）为空。
pub fn check_tool_enabled(
    entry: &McpServerEntry,
    server: &str,
    tool: &str,
) -> Result<(), McpTransportError> {
    if McpToolFilter::from_config(&entry.config).is_enabled(tool) {
        return Ok(());
    }
    Err(McpTransportError::named(
        MCP_TOOL_DISABLED_ERROR,
        format!("MCP tool \"{server}/{tool}\" is disabled by server configuration."),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use kiana_core::mcp_servers::validate_mcp_server_config;
    use serde_json::json;

    fn entry(config: Value) -> McpServerEntry {
        McpServerEntry {
            name: "srv".to_string(),
            config: validate_mcp_server_config("srv", &config).expect("valid config"),
            source: "test".to_string(),
            scope: None,
            override_: None,
        }
    }

    #[test]
    fn disabled_patterns_take_precedence_over_enabled() {
        let filter = McpToolFilter::from_value(&json!({
            "enabledTools": ["admin_*", "read"],
            "disabledTools": ["admin_*"]
        }));
        assert!(!filter.is_enabled("admin_delete"));
        assert!(filter.is_enabled("read"));
        assert!(!filter.is_enabled("write"));
    }

    #[test]
    fn empty_enabled_list_allows_everything_not_disabled() {
        let filter = McpToolFilter::from_value(&json!({"disabledTools": ["danger_*"]}));
        assert!(filter.is_enabled("anything"));
        assert!(!filter.is_enabled("danger_zone"));
    }

    #[test]
    fn wildcard_question_and_class_patterns_match() {
        let filter = McpToolFilter::from_value(&json!({"enabledTools": ["a?c", "x[0-9]"]}));
        assert!(filter.is_enabled("abc"));
        assert!(!filter.is_enabled("ac"));
        assert!(filter.is_enabled("x5"));
        assert!(!filter.is_enabled("xa"));
    }

    #[test]
    fn snake_case_aliases_are_accepted() {
        let filter = McpToolFilter::from_value(&json!({
            "enabled_tools": ["keep"],
            "disabled_tools": ["keep_but_denied"]
        }));
        assert!(filter.is_enabled("keep"));
        assert!(!filter.is_enabled("keep_but_denied"));
        assert!(!filter.is_enabled("other"));
    }

    #[test]
    fn invalid_glob_falls_back_to_literal_match() {
        let filter = McpToolFilter::from_value(&json!({"disabledTools": ["[bad"]}));
        assert!(filter.is_enabled("anything"));
        assert!(!filter.is_enabled("[bad"));
    }

    #[test]
    fn typed_rejection_names_the_error_and_tool() {
        let server = entry(json!({"command": "x", "disabledTools": ["admin_*"]}));
        let error = check_tool_enabled(&server, "srv", "admin_delete").expect_err("disabled");
        assert_eq!(error.name(), MCP_TOOL_DISABLED_ERROR);
        assert_eq!(
            error.to_string(),
            "MCP tool \"srv/admin_delete\" is disabled by server configuration."
        );
        assert!(check_tool_enabled(&server, "srv", "read").is_ok());
    }
}
