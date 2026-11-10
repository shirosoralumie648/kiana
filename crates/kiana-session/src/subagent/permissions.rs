//! 子代理权限上下文：在父会话策略之上做「只能收窄」的投影。
//!
//! 三个层次依次生效：
//!
//! 1. **工具集合**：父代理允许的工具 ∩ 子代理定义请求的工具。父代理的 `Deny` /
//!    allowlist 已经在父工具表里体现，因此这里是最终 schema 与 dispatch 的共同依据。
//! 2. **显式 denylist**：`disallowedTools` 在 schema 与 dispatch 两处都检查；即使
//!    父规则里有一条更宽的 `Allow`，也不能把 denied 工具放回来。
//! 3. **权限引擎**：复用父策略的规则与工作区，但模式取 `min(parent, requested)`；
//!    父会话的「总是允许」记录**不**继承（参考实现中 `allowedTools` 会替换全部
//!    allow 规则，避免父代理的批准泄漏给子代理）。
//!
//! headless 子代理没有交互 UI：引擎返回 `Ask` 时一律转换成 `Deny`。交互式子代理可以
//! 把 `Ask` 交给调用方（[`SubagentPermissionDecision::NeedsApproval`]），由上层决定
//! 是否弹审批卡；本模块绝不把 `Ask` 当允许。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use kiana_ext_api::tool_types::ToolAnnotations;
use kiana_permissions::{
    ActionKind, PermissionDecision, PermissionEngine, PermissionMode, PermissionPolicy,
    PermissionRequest, RuleDecision,
};
use kiana_resources::mcp_annotations::apply_mcp_annotations;

use super::canonical_tool::{CanonicalToolId, ToolPattern, ToolPermissionSet};

/// 子代理的审批能力。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubagentApprovalMode {
    /// 无交互 UI：`Ask` → `Deny`。
    Headless,
    /// 有交互 UI：`Ask` 交给调用方处理。
    Interactive,
}

impl SubagentApprovalMode {
    /// 是否 headless。
    pub fn is_headless(self) -> bool {
        matches!(self, Self::Headless)
    }
}

/// 权限上下文产出的决策。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubagentPermissionDecision {
    /// 允许执行。
    Allow {
        /// 原因。
        reason: String,
    },
    /// 需要父会话审批（只有交互模式会返回）。
    NeedsApproval {
        /// 给审批 UI 的提示。
        prompt: String,
        /// 决策原因。
        reason: String,
    },
    /// 拒绝。
    Deny {
        /// 原因。
        reason: String,
    },
}

impl SubagentPermissionDecision {
    /// 是否允许。
    pub fn is_allow(&self) -> bool {
        matches!(self, Self::Allow { .. })
    }

    /// 是否拒绝。
    pub fn is_deny(&self) -> bool {
        matches!(self, Self::Deny { .. })
    }

    /// 原因文本。
    pub fn reason(&self) -> &str {
        match self {
            Self::Allow { reason } | Self::Deny { reason } => reason,
            Self::NeedsApproval { reason, .. } => reason,
        }
    }
}

/// 子代理的权限上下文。
pub struct SubagentPermissionContext {
    engine: Option<PermissionEngine>,
    tools: ToolPermissionSet,
    denied: Vec<ToolPattern>,
    mode: PermissionMode,
    approval: SubagentApprovalMode,
    workspace_root: PathBuf,
    /// 父会话快照的 MCP 工具 annotations：destructive/openWorld 必须强制 Ask，
    /// readOnly 只在 read-only 模式放宽。子代理不能因为绕开父会话的请求管线而丢失
    /// 这层收紧。
    mcp_annotations: BTreeMap<String, ToolAnnotations>,
}

impl SubagentPermissionContext {
    /// 构造。
    ///
    /// - `parent_policy`：父会话策略；`None` 表示父会话没有启用权限层。
    /// - `parent_mode`：父会话有效模式（没有启用时传 `Default`）。
    /// - `requested_mode`：agent 定义里的 `permissionMode`。
    /// - `tools`：**已经**与父工具表求交后的集合（见
    ///   [`ToolPermissionSet::resolve`]）。
    /// - `denied`：`disallowedTools`。
    pub fn new(
        parent_policy: Option<&PermissionPolicy>,
        parent_mode: PermissionMode,
        requested_mode: Option<PermissionMode>,
        workspace_root: impl Into<PathBuf>,
        tools: ToolPermissionSet,
        denied: Vec<ToolPattern>,
        approval: SubagentApprovalMode,
    ) -> Self {
        let workspace_root = workspace_root.into();
        let mode = stricter_mode(parent_mode, requested_mode.unwrap_or(parent_mode));
        let engine = match parent_policy {
            Some(policy) => {
                let mut policy = policy.clone();
                policy.mode = mode;
                if policy.workspace_roots.is_empty() {
                    policy.workspace_roots.push(workspace_root.clone());
                }
                Some(PermissionEngine::new(policy))
            }
            None => requested_mode.map(|_| {
                PermissionEngine::new(PermissionPolicy::new(mode, workspace_root.clone()))
            }),
        };
        Self {
            engine,
            tools,
            denied,
            mode,
            approval,
            workspace_root,
            mcp_annotations: BTreeMap::new(),
        }
    }

    /// 注入 MCP 工具 annotations 快照（来自父会话的工具注册表）。
    pub fn with_mcp_annotations(mut self, annotations: BTreeMap<String, ToolAnnotations>) -> Self {
        self.mcp_annotations = annotations;
        self
    }

    /// 有效模式（父/子取更严者）。
    pub fn mode(&self) -> PermissionMode {
        self.mode
    }

    /// 有效工具集合。
    pub fn tools(&self) -> &ToolPermissionSet {
        &self.tools
    }

    /// 工作区根。
    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    /// 是否显式 denied。
    pub fn is_denied(&self, id: &CanonicalToolId, raw_name: &str) -> bool {
        self.denied
            .iter()
            .any(|pattern| pattern.matches(id, raw_name))
    }

    /// 工具集合是否允许（不含引擎规则）。
    pub fn allows_tool(&self, id: &CanonicalToolId, raw_name: &str) -> bool {
        !self.is_denied(id, raw_name) && self.tools.allows(id)
    }

    /// 对一次调用做完整决策。
    pub fn evaluate(&self, request: &PermissionRequest) -> SubagentPermissionDecision {
        let id = CanonicalToolId::parse(&request.tool_name);
        if self.is_denied(&id, &request.tool_name) {
            return SubagentPermissionDecision::Deny {
                reason: format!("tool `{}` is disallowed for this subagent", id.display()),
            };
        }
        if !self.tools.allows(&id) {
            return SubagentPermissionDecision::Deny {
                reason: format!(
                    "tool `{}` is not in the subagent's allowed tool set",
                    id.display()
                ),
            };
        }
        let Some(engine) = &self.engine else {
            return SubagentPermissionDecision::Allow {
                reason: "parent session has no permission layer".to_string(),
            };
        };
        let decision = engine.evaluate(request);
        // MCP annotations 只对 MCP 工具生效，且不能把 Deny 翻成 Allow；显式 deny 规则
        // 优先于任何 hint。父会话在 permission_refusal_inner 里做同样的处理，子代理
        // 必须保持一致，否则 destructiveHint 工具会在无确认的情况下执行。
        let decision = if request.action == ActionKind::Mcp {
            let explicit_deny = engine
                .policy()
                .rules
                .iter()
                .any(|rule| rule.decision() == RuleDecision::Deny && rule.matches(request));
            apply_mcp_annotations(
                self.mcp_annotations.get(&request.tool_name),
                self.mode,
                decision,
                explicit_deny,
            )
        } else {
            decision
        };
        match decision {
            PermissionDecision::Allow { reason } => SubagentPermissionDecision::Allow { reason },
            PermissionDecision::Deny { reason } => SubagentPermissionDecision::Deny { reason },
            PermissionDecision::Ask { prompt, reason } => match self.approval {
                SubagentApprovalMode::Headless => SubagentPermissionDecision::Deny {
                    reason: format!("headless subagent cannot request approval: {reason}"),
                },
                SubagentApprovalMode::Interactive => {
                    SubagentPermissionDecision::NeedsApproval { prompt, reason }
                }
            },
        }
    }
}

/// 模式严格度：`ReadOnly < Default < AcceptEdits < BypassPermissions`。
fn mode_rank(mode: PermissionMode) -> u8 {
    match mode {
        PermissionMode::ReadOnly => 0,
        PermissionMode::Default => 1,
        PermissionMode::AcceptEdits => 2,
        PermissionMode::BypassPermissions => 3,
    }
}

/// 取更严的模式。
pub fn stricter_mode(parent: PermissionMode, requested: PermissionMode) -> PermissionMode {
    if mode_rank(requested) <= mode_rank(parent) {
        requested
    } else {
        parent
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subagent::canonical_tool::ToolSelection;
    use serde_json::json;

    fn request(tool: &str, args: serde_json::Value) -> PermissionRequest {
        PermissionRequest::from_tool_call_with(tool, &args, "/tmp/workspace", true)
    }

    fn all_tools() -> ToolPermissionSet {
        ToolPermissionSet::all()
    }

    #[test]
    fn child_cannot_widen_permission_mode() {
        let context = SubagentPermissionContext::new(
            Some(&PermissionPolicy::new(
                PermissionMode::Default,
                "/tmp/workspace",
            )),
            PermissionMode::Default,
            Some(PermissionMode::BypassPermissions),
            "/tmp/workspace",
            all_tools(),
            Vec::new(),
            SubagentApprovalMode::Headless,
        );
        assert_eq!(context.mode(), PermissionMode::Default);
    }

    #[test]
    fn child_can_narrow_permission_mode() {
        let context = SubagentPermissionContext::new(
            Some(&PermissionPolicy::new(
                PermissionMode::BypassPermissions,
                "/tmp/workspace",
            )),
            PermissionMode::BypassPermissions,
            Some(PermissionMode::ReadOnly),
            "/tmp/workspace",
            all_tools(),
            Vec::new(),
            SubagentApprovalMode::Headless,
        );
        assert_eq!(context.mode(), PermissionMode::ReadOnly);
        let decision = context.evaluate(&request("write", json!({"path": "a.txt"})));
        assert!(decision.is_deny(), "{decision:?}");
    }

    #[test]
    fn deny_list_is_checked_before_engine_rules() {
        let mut policy = PermissionPolicy::new(PermissionMode::Default, "/tmp/workspace");
        policy.rules.push(kiana_permissions::PermissionRule::tool(
            "bash",
            kiana_permissions::RuleDecision::Allow,
        ));
        let context = SubagentPermissionContext::new(
            Some(&policy),
            PermissionMode::Default,
            None,
            "/tmp/workspace",
            all_tools(),
            vec![ToolPattern::new("bash")],
            SubagentApprovalMode::Headless,
        );
        let decision = context.evaluate(&request("bash", json!({"command": "echo hi"})));
        assert!(decision.is_deny(), "{decision:?}");
    }

    #[test]
    fn tool_set_is_enforced_before_engine_rules() {
        let tools = ToolPermissionSet::resolve(
            &ToolPermissionSet::all(),
            [CanonicalToolId::parse("read")],
            Some(&ToolSelection::All),
        );
        let context = SubagentPermissionContext::new(
            Some(&PermissionPolicy::new(
                PermissionMode::BypassPermissions,
                "/tmp/workspace",
            )),
            PermissionMode::BypassPermissions,
            None,
            "/tmp/workspace",
            tools,
            Vec::new(),
            SubagentApprovalMode::Headless,
        );
        assert!(
            context
                .evaluate(&request("read", json!({"path": "a.txt"})))
                .is_allow()
        );
        assert!(
            context
                .evaluate(&request("bash", json!({"command": "echo hi"})))
                .is_deny()
        );
    }

    #[test]
    fn headless_ask_becomes_deny() {
        let context = SubagentPermissionContext::new(
            Some(&PermissionPolicy::new(
                PermissionMode::Default,
                "/tmp/workspace",
            )),
            PermissionMode::Default,
            None,
            "/tmp/workspace",
            all_tools(),
            Vec::new(),
            SubagentApprovalMode::Headless,
        );
        // default 模式下 bash 需要询问。
        let decision = context.evaluate(&request("bash", json!({"command": "echo hi"})));
        assert!(decision.is_deny(), "{decision:?}");
        assert!(decision.reason().contains("headless"));
    }

    #[test]
    fn interactive_ask_is_returned_to_caller() {
        let context = SubagentPermissionContext::new(
            Some(&PermissionPolicy::new(
                PermissionMode::Default,
                "/tmp/workspace",
            )),
            PermissionMode::Default,
            None,
            "/tmp/workspace",
            all_tools(),
            Vec::new(),
            SubagentApprovalMode::Interactive,
        );
        let decision = context.evaluate(&request("bash", json!({"command": "echo hi"})));
        assert!(matches!(
            decision,
            SubagentPermissionDecision::NeedsApproval { .. }
        ));
    }

    #[test]
    fn mcp_annotations_still_force_confirmation_inside_a_subagent() {
        let mut policy = PermissionPolicy::new(PermissionMode::Default, "/tmp/workspace");
        policy.rules.push(kiana_permissions::PermissionRule::tool(
            "mcp__github__delete",
            kiana_permissions::RuleDecision::Allow,
        ));
        let context = |annotations| {
            SubagentPermissionContext::new(
                Some(&policy),
                PermissionMode::Default,
                None,
                "/tmp/workspace",
                all_tools(),
                Vec::new(),
                SubagentApprovalMode::Headless,
            )
            .with_mcp_annotations(annotations)
        };
        let request = request("mcp__github__delete", json!({}));
        // 控制组：没有 hints 时 allow 规则生效。
        assert!(context(BTreeMap::new()).evaluate(&request).is_allow());

        let mut annotations = BTreeMap::new();
        annotations.insert(
            "mcp__github__delete".to_string(),
            ToolAnnotations {
                destructive_hint: Some(true),
                ..Default::default()
            },
        );
        let decision = context(annotations).evaluate(&request);
        assert!(decision.is_deny(), "{decision:?}");
        assert!(
            decision.reason().contains("destructiveHint"),
            "{decision:?}"
        );
    }

    #[test]
    fn missing_parent_permission_layer_still_enforces_tool_set() {
        let tools = ToolPermissionSet::resolve(
            &ToolPermissionSet::all(),
            [CanonicalToolId::parse("read")],
            None,
        );
        let context = SubagentPermissionContext::new(
            None,
            PermissionMode::Default,
            None,
            "/tmp/workspace",
            tools,
            Vec::new(),
            SubagentApprovalMode::Headless,
        );
        assert!(
            context
                .evaluate(&request("read", json!({"path": "a.txt"})))
                .is_allow()
        );
        assert!(
            context
                .evaluate(&request("write", json!({"path": "a.txt"})))
                .is_deny()
        );
    }
}
