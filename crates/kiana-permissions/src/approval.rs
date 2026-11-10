//! 结构化审批结果。
//!
//! `Ask` 只说明「需要用户确认」，本模块定义用户确认之后能选的几种结果：
//!
//! - [`ApprovalDecision::AllowOnce`]：只放行这一次，不产生规则，下一次同样的调用仍会问；
//! - [`ApprovalDecision::AllowAlways`]：生成一条 `Allow` 规则并记在某个作用域，
//!   `Session` 只存内存，`Local` 写项目本地配置（`settings.local.json`），`Project`
//!   写项目配置（`.kiana/settings.json`）；
//! - [`ApprovalDecision::Reject`]：拒绝并把可选原因回传给模型。
//!
//! `AllowAlways` **不能覆盖 `Deny`**：生成的只是一条 `Allow` 规则，引擎仍按
//! `Deny → Ask → Allow` 匹配，显式拒绝和灾难性命令检查永远先执行。
//!
//! # 规则粒度
//!
//! 「总是允许」的 key 不能太粗（`bash:*` 会把 `rm -rf /` 也放过去），也不能太细
//! （每次命令都问）。这里的取法与 [`PermissionRequest::approval_key`] 一致：
//!
//! - 单一简单命令且第二个词是已知子命令（`git status`、`cargo test`）→
//!   `program + subcommand` 前缀规则；
//! - 其它简单命令（`make build`、带选项的 `git -C /tmp status`）→ 完整精确命令；
//! - 复合命令/分析不完整/Unknown → 完整精确命令（[`PermissionRule::ExactCommand`]）；
//! - 文件工具按路径前缀记；
//! - MCP/未知工具按工具名记（没有更细的规则类型可用）。
//!
//! 规则建议由 [`crate::shell::ShellAnalysis::rule_suggestions`] 生成，**不会**生成
//! `prog *`：`git *` 会把 `git push` 等完全不同的操作也放行。

use serde::{Deserialize, Serialize};

use crate::policy::{PermissionRule, RuleDecision};
use crate::request::{ActionKind, PermissionRequest};

/// `AllowAlways` 的持久化作用域。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ApprovalScope {
    /// 只在本会话内存里生效，重启后失效。
    Session,
    /// 写入项目本地配置（`.kiana/settings.local.json`，通常不入版本库）。
    Local,
    /// 写入项目配置（`.kiana/settings.json`）。
    Project,
}

impl ApprovalScope {
    /// 稳定字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Local => "local",
            Self::Project => "project",
        }
    }
}

/// 一次审批的用户选择。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "kebab-case")]
pub enum ApprovalDecision {
    /// 只允许这一次。
    AllowOnce,
    /// 允许并记住。
    AllowAlways {
        /// 记忆作用域。
        scope: ApprovalScope,
    },
    /// 拒绝；`reason` 会回传给模型。
    Reject {
        /// 用户给出的原因（可空）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
}

impl ApprovalDecision {
    /// 给事件流/审计用的稳定字符串。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::AllowOnce => "allow-once",
            Self::AllowAlways { .. } => "allow-always",
            Self::Reject { .. } => "reject",
        }
    }

    /// `AllowAlways` 的作用域；其余决策返回 `None`。
    pub fn scope(&self) -> Option<ApprovalScope> {
        match self {
            Self::AllowAlways { scope } => Some(*scope),
            _ => None,
        }
    }

    /// 拒绝原因（`Reject` 且用户填写时）。
    pub fn reject_reason(&self) -> Option<&str> {
        match self {
            Self::Reject { reason } => reason.as_deref(),
            _ => None,
        }
    }

    /// 为 `AllowAlways` 生成要写入策略的规则；其它决策返回空。
    ///
    /// 生成的规则永远是 `Allow`：`Deny` 规则与灾难性命令检查在引擎里先于本规则执行，
    /// 因此「总是允许」不会成为提权入口。
    pub fn rules_for(&self, request: &PermissionRequest) -> Vec<PermissionRule> {
        match self {
            Self::AllowAlways { .. } => approval_rules(request),
            _ => Vec::new(),
        }
    }
}

/// 为「总是允许」构造规则。
///
/// 返回多条是因为文件工具一次可能写多个路径：每个路径一条 `PathPrefix` 规则，
/// 避免用一条宽泛的工具规则把整个工具放行。
///
/// 所有规则都用 [`PermissionRule::Approved`] 包装并记住请求的工具来源：扩展/SDK 工具
/// 可以覆盖内建名（自定义 `edit`），未包装的 `Tool{"edit", Allow}` 会在之后放行真正的
/// 内建 `edit`，在 read-only 模式下也一样。包装后的规则还先于 `Ask` 规则执行，让
/// Local/Project 作用域的「总是允许」真正生效。
pub fn approval_rules(request: &PermissionRequest) -> Vec<PermissionRule> {
    let reason = Some(format!(
        "Approved by the user for tool '{}'",
        request.tool_name
    ));
    let approve = |rule: PermissionRule| PermissionRule::Approved {
        rule: Box::new(rule),
        builtin: request.builtin,
    };
    match request.action {
        ActionKind::Execute => {
            let command = request.command.as_deref().unwrap_or_default().trim();
            if command.is_empty() {
                // 命令不可解析时不能退化成 Tool Allow（那会放行整个 bash）；
                // 不生成规则，「总是允许」对这次调用等同于只允许一次。
                return Vec::new();
            }
            // 规则建议由 ShellAnalyzer 生成：只有“精确命令”或
            // “program + subcommand 前缀”，绝不生成 `prog *`。复合命令/分析不可信
            // 时退化为完整精确命令。
            let suggestions = request
                .shell
                .as_ref()
                .map(crate::shell::ShellAnalysis::rule_suggestions)
                .unwrap_or_else(|| {
                    crate::shell::ShellAnalysis::analyze(command).rule_suggestions()
                });
            suggestions
                .into_iter()
                .map(|suggestion| match suggestion {
                    crate::shell::ShellRuleSuggestion::Exact { command } => approve(
                        PermissionRule::exact_command(command, RuleDecision::Allow)
                            .with_reason(reason.clone().unwrap_or_default()),
                    ),
                    crate::shell::ShellRuleSuggestion::Prefix { tokens } => {
                        approve(PermissionRule::CommandPrefix {
                            tokens,
                            decision: RuleDecision::Allow,
                            reason: reason.clone(),
                        })
                    }
                })
                .collect()
        }
        ActionKind::Write | ActionKind::Read => {
            if request.paths.is_empty() {
                // 路径不可解析时不能退化成整个工具放行。
                return Vec::new();
            }
            request
                .paths
                .iter()
                .map(|path| {
                    approve(PermissionRule::PathPrefix {
                        prefix: path.clone(),
                        // 只对当初批准的动作生效：单文件写审批不能变成 shell/MCP 的通行证。
                        actions: Some(vec![request.action]),
                        decision: RuleDecision::Allow,
                        reason: reason.clone(),
                    })
                })
                .collect()
        }
        ActionKind::Mcp | ActionKind::Other | ActionKind::Network => {
            vec![approve(PermissionRule::tool(
                &request.tool_name,
                RuleDecision::Allow,
            ))]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{PermissionEngine, PermissionFlags};
    use serde_json::json;

    /// 取出 [`PermissionRule::Approved`] 包装的底层规则。
    fn inner(rule: &PermissionRule) -> &PermissionRule {
        match rule {
            PermissionRule::Approved { rule, .. } => rule,
            other => other,
        }
    }

    #[test]
    fn simple_commands_are_remembered_by_two_words() {
        let request = PermissionRequest::from_tool_call(
            "bash",
            &json!({"command": "git status --short"}),
            "/workspace",
        );
        let rules = ApprovalDecision::AllowAlways {
            scope: ApprovalScope::Session,
        }
        .rules_for(&request);
        assert_eq!(rules.len(), 1);
        assert!(rules[0].is_approved(), "审批规则必须带 Approved 标记");
        assert!(matches!(
            inner(&rules[0]),
            PermissionRule::CommandPrefix { tokens, .. } if tokens == &vec!["git".to_string(), "status".to_string()]
        ));
    }

    #[test]
    fn env_assignments_are_skipped_in_remembered_rules() {
        let request = PermissionRequest::from_tool_call(
            "bash",
            &json!({"command": "FOO=1 git status"}),
            "/workspace",
        );
        let rules = ApprovalDecision::AllowAlways {
            scope: ApprovalScope::Project,
        }
        .rules_for(&request);
        assert!(matches!(
            inner(&rules[0]),
            PermissionRule::CommandPrefix { tokens, .. }
                if tokens == &vec!["git".to_string(), "status".to_string()]
        ));
    }

    #[test]
    fn unusable_targets_do_not_become_tool_wide_allow_rules() {
        let bash = PermissionRequest::from_tool_call("bash", &json!({"command": 42}), "/workspace");
        assert!(approval_rules(&bash).is_empty());
        let write = PermissionRequest::from_tool_call("write", &json!({}), "/workspace");
        assert!(approval_rules(&write).is_empty());
    }

    #[test]
    fn compound_commands_are_remembered_in_full() {
        let request = PermissionRequest::from_tool_call(
            "bash",
            &json!({"command": "echo a; rm -rf ./build"}),
            "/workspace",
        );
        let rules = ApprovalDecision::AllowAlways {
            scope: ApprovalScope::Project,
        }
        .rules_for(&request);
        assert!(matches!(
            inner(&rules[0]),
            PermissionRule::ExactCommand { command, .. } if command == "echo a; rm -rf ./build"
        ));
    }

    #[test]
    fn write_approvals_are_path_scoped() {
        let request = PermissionRequest::from_tool_call(
            "write",
            &json!({"path": "/workspace/out.txt"}),
            "/workspace",
        );
        let rules = ApprovalDecision::AllowAlways {
            scope: ApprovalScope::Local,
        }
        .rules_for(&request);
        assert!(matches!(
            inner(&rules[0]),
            PermissionRule::PathPrefix { prefix, .. } if prefix == std::path::Path::new("/workspace/out.txt")
        ));
    }

    #[test]
    fn allow_once_and_reject_generate_no_rules() {
        let request =
            PermissionRequest::from_tool_call("bash", &json!({"command": "echo hi"}), "/workspace");
        assert!(ApprovalDecision::AllowOnce.rules_for(&request).is_empty());
        assert!(
            ApprovalDecision::Reject { reason: None }
                .rules_for(&request)
                .is_empty()
        );
    }

    /// 关键回归：自定义/SDK 工具覆盖内建名（`edit`/`write`）时，审批写出的规则不能
    /// 在之后放行真正的内建工具（包括 read-only 模式）。
    #[test]
    fn custom_tool_approvals_cannot_grant_builtin_tool_permissions() {
        let custom_edit =
            PermissionRequest::from_tool_call_with("edit", &json!({}), "/workspace", false);
        assert_eq!(custom_edit.action, ActionKind::Other);
        let rules = ApprovalDecision::AllowAlways {
            scope: ApprovalScope::Local,
        }
        .rules_for(&custom_edit);
        assert_eq!(rules.len(), 1);
        let PermissionRule::Approved { builtin, .. } = &rules[0] else {
            panic!("approval rule must be Approved: {:?}", rules[0]);
        };
        assert!(!builtin, "自定义工具的批准必须标记为自定义来源");

        // 持久化 → 重新加载到全新引擎。
        let settings = json!({
            "mode": "read-only",
            "rules": rules.iter().map(PermissionRule::to_settings_value).collect::<Vec<_>>(),
        });
        let engine = PermissionEngine::from_settings_and_flags(
            "/workspace",
            Some(&settings),
            &PermissionFlags::default(),
        )
        .expect("rules enable the engine");

        // 内建 edit 不能命中：形态上是 Write，且来源是内建。
        let builtin_edit = PermissionRequest::from_tool_call(
            "edit",
            &json!({"path": "/workspace/secret.txt"}),
            "/workspace",
        );
        assert!(
            !engine.evaluate(&builtin_edit).is_allow(),
            "自定义 edit 的批准不能放行内建 edit（read-only 下更不能）: {:?}",
            engine.evaluate(&builtin_edit)
        );

        // 同样的调用（自定义工具）仍然被批准。
        let custom_again =
            PermissionRequest::from_tool_call_with("edit", &json!({}), "/workspace", false);
        assert!(engine.evaluate(&custom_again).is_allow());
    }
}
