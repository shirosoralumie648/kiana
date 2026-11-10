//! 子代理工具表：独立 tool pool、模型可见 schema 与 dispatch 二次校验。
//!
//! 父会话的工具注册表在 `AgentSession::Core` 里，包含内建、SDK 与扩展工具；子代理
//! 不能直接共享可变注册表，否则扩展刷新/激活名单会互相影响。这里在创建子代理时做一次
//! **快照**，再按权限集合过滤，得到不可变的 [`ToolPool`]：
//!
//! - [`ToolPool::schemas`] 只返回允许列表内的工具，父代理 denied 的工具不会出现在
//!   模型请求里（DoD：「父 deny 的工具从 schema 消失」）；
//! - [`ToolPool::get`] 在 dispatch 时再次检查集合与 denylist，绕过 schema 直接构造
//!   工具名也会失败（DoD：「直接 dispatch 也失败」）。
//!
//! 工具定义本身不复制、不重写，只克隆 `AgentTool`（内部是 `Arc` 回调），因此子代理
//! 执行与父会话执行的是同一份工具实现。

use std::collections::BTreeSet;

use indexmap::IndexMap;
use kiana_agent_core::types::AgentTool;
use kiana_ai::types::JsonValue;

use super::canonical_tool::{CanonicalToolId, ToolPattern, ToolPermissionSet};

/// 模型可见的工具声明。
#[derive(Debug, Clone, PartialEq)]
pub struct ToolSchema {
    /// 模型调用的原始工具名。
    pub name: String,
    /// 规范 id（日志/测试用）。
    pub canonical: String,
    /// 工具说明。
    pub description: String,
    /// JSON Schema。
    pub parameters: JsonValue,
}

/// 工具不可用时的原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolAccessError {
    /// 工具名不在注册表里。
    Unknown {
        /// 工具名。
        tool: String,
    },
    /// 工具被允许集合排除。
    NotAllowed {
        /// 规范 id。
        canonical: String,
    },
    /// 工具命中 denylist。
    Denied {
        /// 规范 id。
        canonical: String,
    },
}

impl std::fmt::Display for ToolAccessError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unknown { tool } => write!(
                formatter,
                "tool `{tool}` is not registered for this subagent"
            ),
            Self::NotAllowed { canonical } => {
                write!(
                    formatter,
                    "tool `{canonical}` is not in the subagent's allowed tool set"
                )
            }
            Self::Denied { canonical } => {
                write!(
                    formatter,
                    "tool `{canonical}` is disallowed for this subagent"
                )
            }
        }
    }
}

impl std::error::Error for ToolAccessError {}

/// 子代理的不可变工具表。
#[derive(Debug, Clone, Default)]
pub struct ToolPool {
    tools: IndexMap<String, AgentTool>,
}

impl ToolPool {
    /// 从工具列表构造（按名字去重，后出现者覆盖）。
    pub fn new(tools: impl IntoIterator<Item = AgentTool>) -> Self {
        let mut map = IndexMap::new();
        for tool in tools {
            map.insert(tool.name.clone(), tool);
        }
        Self { tools: map }
    }

    /// 工具数量。
    pub fn len(&self) -> usize {
        self.tools.len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    /// 按原始名查找。
    pub fn get(&self, name: &str) -> Option<&AgentTool> {
        self.tools.get(name)
    }

    /// 工具名（保持插入序）。
    pub fn names(&self) -> Vec<String> {
        self.tools.keys().cloned().collect()
    }

    /// 迭代工具。
    pub fn iter(&self) -> impl Iterator<Item = &AgentTool> {
        self.tools.values()
    }

    /// 所有工具的规范 id。
    pub fn canonical_ids(&self) -> Vec<CanonicalToolId> {
        self.tools
            .keys()
            .map(|name| CanonicalToolId::parse(name))
            .collect()
    }

    /// 规范 id 集合。
    pub fn canonical_set(&self) -> BTreeSet<CanonicalToolId> {
        self.canonical_ids().into_iter().collect()
    }

    /// 按允许集合与 denylist 过滤，生成子代理工具表。
    pub fn filter(&self, allowed: &ToolPermissionSet, denied: &[ToolPattern]) -> Self {
        let mut filtered = IndexMap::new();
        for (name, tool) in &self.tools {
            let id = CanonicalToolId::parse(name);
            if !allowed.allows(&id) {
                continue;
            }
            if denied.iter().any(|pattern| pattern.matches(&id, name)) {
                continue;
            }
            filtered.insert(name.clone(), tool.clone());
        }
        Self { tools: filtered }
    }

    /// 模型可见的 schema，只包含允许的工具。
    pub fn schemas(&self) -> Vec<ToolSchema> {
        self.tools
            .values()
            .map(|tool| ToolSchema {
                name: tool.name.clone(),
                canonical: CanonicalToolId::parse(&tool.name).display(),
                description: tool.description.clone(),
                parameters: tool.parameters.clone(),
            })
            .collect()
    }

    /// dispatch 二次校验：注册表、允许集合与 denylist 都通过才返回工具。
    pub fn get_checked(
        &self,
        name: &str,
        allowed: &ToolPermissionSet,
        denied: &[ToolPattern],
    ) -> Result<&AgentTool, ToolAccessError> {
        let tool = self
            .tools
            .get(name)
            .ok_or_else(|| ToolAccessError::Unknown {
                tool: name.to_string(),
            })?;
        let id = CanonicalToolId::parse(name);
        if denied.iter().any(|pattern| pattern.matches(&id, name)) {
            return Err(ToolAccessError::Denied {
                canonical: id.display(),
            });
        }
        if !allowed.allows(&id) {
            return Err(ToolAccessError::NotAllowed {
                canonical: id.display(),
            });
        }
        Ok(tool)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use kiana_agent_core::types::{AgentTool, AgentToolError, AgentToolResult, BoxFuture};

    use super::*;
    use crate::subagent::canonical_tool::ToolSelection;

    struct NamedExecute(String);

    impl kiana_agent_core::types::AgentToolExecute for NamedExecute {
        fn execute<'a>(
            &'a self,
            _tool_call_id: &'a str,
            _params: JsonValue,
            _signal: Option<&'a tokio_util::sync::CancellationToken>,
            _on_update: Option<kiana_agent_core::types::AgentToolUpdateCallback>,
        ) -> BoxFuture<'a, Result<AgentToolResult<JsonValue>, AgentToolError>> {
            let name = self.0.clone();
            Box::pin(async move {
                Ok(AgentToolResult {
                    content: vec![kiana_ai::types::UserContentBlock::Text(
                        kiana_ai::types::TextContent {
                            kind_text: kiana_ai::types::TextKind::Text,
                            text: format!("{name} ran"),
                            text_signature: None,
                        },
                    )],
                    details: JsonValue::Null,
                    structured_content: None,
                    usage: None,
                    is_error: None,
                    terminate: None,
                })
            })
        }
    }

    fn tool(name: &str) -> AgentTool {
        AgentTool {
            name: name.to_string(),
            label: name.to_string(),
            description: format!("{name} tool"),
            parameters: serde_json::json!({"type": "object"}),
            constrained_sampling: None,
            output_schema: None,
            prepare_arguments: None,
            execution_mode: None,
            replay: None,
            execute: Arc::new(NamedExecute(name.to_string())),
        }
    }

    #[test]
    fn schemas_only_contain_allowed_tools() {
        let parent = ToolPool::new([tool("read"), tool("bash")]);
        let parent_allowed = ToolPermissionSet::resolve(
            &ToolPermissionSet::all(),
            parent.canonical_ids(),
            Some(&ToolSelection::Tools(vec![ToolPattern::new("read")])),
        );
        let child = parent.filter(&parent_allowed, &[]);
        assert_eq!(child.names(), vec!["read"]);
        let schemas = child.schemas();
        assert_eq!(schemas.len(), 1);
        assert_eq!(schemas[0].name, "read");
        assert!(
            child.get("bash").is_none(),
            "denied tool is not in the pool"
        );
    }

    #[test]
    fn dispatch_rechecks_denylist_even_if_pool_contains_the_tool() {
        let pool = ToolPool::new([tool("read"), tool("bash")]);
        let allowed = ToolPermissionSet::all();
        assert!(pool.get_checked("read", &allowed, &[]).is_ok());
        let denied = vec![ToolPattern::new("bash")];
        let error = pool.get_checked("bash", &allowed, &denied).unwrap_err();
        assert!(matches!(error, ToolAccessError::Denied { .. }));
    }

    #[test]
    fn dispatch_rechecks_allow_set() {
        let pool = ToolPool::new([tool("read"), tool("bash")]);
        let allowed = ToolPermissionSet::from_ids([CanonicalToolId::parse("read")]);
        let error = pool.get_checked("bash", &allowed, &[]).unwrap_err();
        assert!(matches!(error, ToolAccessError::NotAllowed { .. }));
    }
}
