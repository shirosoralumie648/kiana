//! 权限模式。

use std::str::FromStr;

/// 工具调用的权限模式。
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum PermissionMode {
    /// 只读：读允许；写、执行、网络、MCP/其它全部拒绝。
    ReadOnly,
    /// 默认：读允许；其余询问。
    #[default]
    Default,
    /// 接受编辑：工作区内写允许；区外写、执行、网络、MCP/其它询问。
    AcceptEdits,
    /// 全部放行（仍记录决策；灾难性命令也放行）。
    ///
    /// 序列化用 `as_str()` 的 `bypass`（与配置/环境变量一致）；反序列化也接受
    /// 旧拼写 `bypass-permissions`。
    #[serde(rename = "bypass", alias = "bypass-permissions")]
    BypassPermissions,
}

impl PermissionMode {
    /// 解析环境变量/配置里的字符串；未知值返回 `None`。
    ///
    /// 别名覆盖了 sandbox 与 Claude Code 的常见拼写：
    /// `plan` → `read-only`，`workspace-write` → `accept-edits`，
    /// `danger-full-access` → `bypass`。
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "read-only" | "readonly" | "plan" | "ro" => Some(Self::ReadOnly),
            "default" | "ask" => Some(Self::Default),
            "accept-edits" | "acceptedits" | "accept" | "workspace-write" | "workspace" => {
                Some(Self::AcceptEdits)
            }
            "bypass" | "bypass-permissions" | "bypasspermissions" | "danger-full-access"
            | "danger" | "full-access" => Some(Self::BypassPermissions),
            _ => None,
        }
    }

    /// 稳定字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::Default => "default",
            Self::AcceptEdits => "accept-edits",
            Self::BypassPermissions => "bypass",
        }
    }

    /// 给 UI/诊断用的一句话说明。
    pub fn description(self) -> &'static str {
        match self {
            Self::ReadOnly => "read files only; no writes, commands, network, or MCP tools",
            Self::Default => "read freely; ask before writes, commands, network, and MCP tools",
            Self::AcceptEdits => {
                "allow edits inside the workspace; ask before commands, network, and outside writes"
            }
            Self::BypassPermissions => "allow every tool call without asking",
        }
    }

    /// 是否是显式的“全部放行”。
    pub fn is_bypass(self) -> bool {
        matches!(self, Self::BypassPermissions)
    }
}

impl FromStr for PermissionMode {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value).ok_or(())
    }
}

impl std::fmt::Display for PermissionMode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serde_spelling_matches_as_str() {
        for mode in [
            PermissionMode::ReadOnly,
            PermissionMode::Default,
            PermissionMode::AcceptEdits,
            PermissionMode::BypassPermissions,
        ] {
            let json = serde_json::to_string(&mode).expect("serialize");
            assert_eq!(json, format!("\"{}\"", mode.as_str()));
            let back: PermissionMode = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(back, mode);
        }
        // 旧拼写仍可反序列化。
        let legacy: PermissionMode =
            serde_json::from_str("\"bypass-permissions\"").expect("legacy alias");
        assert_eq!(legacy, PermissionMode::BypassPermissions);
    }
}
