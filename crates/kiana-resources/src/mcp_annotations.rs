//! MCP `annotations` → 权限决策的纯策略。
//!
//! MCP server 在 `tools/list` 里返回的 hints 是**不可信提示**，只能收窄/触发确认，不能
//! 当作授权（见 `docs/history/porting/plan.md` 第 5 周）。本模块把这段策略从会话里独立出来，
//! 方便单元测试；会话接线见 `docs/history/porting/handoffs/phase-05.md`。
//!
//! # 语义
//!
//! - `readOnlyHint = true`：**只在** `read-only` 模式把 MCP 工具的模式级拒绝放宽为 allow；
//!   其它模式不因该 hint 放行（default 模式仍然 ask）。
//! - `destructiveHint = true`：在 `default`/`accept-edits` 模式强制 ask —— 即使已有 allow
//!   规则命中也必须再次确认。
//! - `openWorldHint = true`：额外 ask，同样可以覆盖 allow 规则。
//! - 显式 deny 规则永远优先：调用方把 `explicit_deny = true` 传进来时原样返回引擎决策。
//! - 没有任何 hint 时保持引擎决策（MCP 在 default 模式本来就是 ask/write 语义）。
//! - hints 不产生持久授权：`destructive`/`open_world` 工具只允许“本次”批准。
//!
//! 该策略不读取、也不修改任何持久状态，因此不会把 hint 变成 `AllowAlways` 规则。

use kiana_ext_api::tool_types::ToolAnnotations;
use kiana_permissions::{PermissionDecision, PermissionMode};

/// `destructiveHint` 强制 ask 时使用的决策原因。
pub const DESTRUCTIVE_HINT_REASON: &str =
    "MCP tool declares destructiveHint; confirmation is required even when a rule allows it.";
/// `openWorldHint` 额外 ask 时使用的决策原因。
pub const OPEN_WORLD_HINT_REASON: &str =
    "MCP tool declares openWorldHint; an extra confirmation is required.";
/// read-only 模式下 `readOnlyHint` 放行时使用的决策原因。
pub const READ_ONLY_HINT_REASON: &str =
    "read-only mode allows an MCP tool that declares readOnlyHint.";

/// 把一个 MCP 工具的 annotations 应用到引擎给出的基础决策上。
///
/// 参数：
/// - `annotations`：工具声明的 hints；`None` 等价于全部未声明；
/// - `mode`：当前权限模式；
/// - `base`：`PermissionEngine::evaluate` 的结论；
/// - `explicit_deny`：调用方是否已经发现显式 deny 规则命中（引擎的 `Deny` 无法区分
///   “规则拒绝”与“read-only 模式默认拒绝”）。为 `true` 时 hint 一律无效。
pub fn apply_mcp_annotations(
    annotations: Option<&ToolAnnotations>,
    mode: PermissionMode,
    base: PermissionDecision,
    explicit_deny: bool,
) -> PermissionDecision {
    if explicit_deny || mode.is_bypass() {
        return base;
    }
    let read_only = annotations.is_some_and(|annotations| annotations.read_only_hint == Some(true));
    let destructive =
        annotations.is_some_and(|annotations| annotations.destructive_hint == Some(true));
    let open_world =
        annotations.is_some_and(|annotations| annotations.open_world_hint == Some(true));

    match mode {
        PermissionMode::ReadOnly => {
            if read_only {
                PermissionDecision::Allow {
                    reason: READ_ONLY_HINT_REASON.to_string(),
                }
            } else {
                base
            }
        }
        PermissionMode::Default | PermissionMode::AcceptEdits => {
            // 引擎的 Deny（规则/灾难性命令）不能被 hint 翻案。
            if matches!(base, PermissionDecision::Deny { .. }) {
                return base;
            }
            if destructive {
                return require_ask(&base, DESTRUCTIVE_HINT_REASON);
            }
            if open_world {
                return require_ask(&base, OPEN_WORLD_HINT_REASON);
            }
            base
        }
        PermissionMode::BypassPermissions => base,
    }
}

/// 把 `base` 变成 ask；已有 ask 的 prompt 保留，否则生成一句可读提示。
fn require_ask(base: &PermissionDecision, reason: &str) -> PermissionDecision {
    match base {
        PermissionDecision::Ask { prompt, .. } => PermissionDecision::Ask {
            prompt: prompt.clone(),
            reason: reason.to_string(),
        },
        _ => PermissionDecision::Ask {
            prompt: format!("{reason} Allow this call?"),
            reason: reason.to_string(),
        },
    }
}

/// 该工具的批准是否允许写成持久授权（`AllowAlways`）。
///
/// `destructiveHint`/`openWorldHint` 工具每次调用都必须重新确认，会话不应把
/// `AllowAlways` 落盘或写进 session 规则；`readOnlyHint` 本身不授予任何持久权限。
pub fn allows_persistent_approval(annotations: Option<&ToolAnnotations>) -> bool {
    !annotations.is_some_and(|annotations| {
        annotations.destructive_hint == Some(true) || annotations.open_world_hint == Some(true)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use kiana_ext_api::tool_types::ToolAnnotations;

    fn annotations(
        read_only: Option<bool>,
        destructive: Option<bool>,
        open_world: Option<bool>,
    ) -> ToolAnnotations {
        ToolAnnotations {
            read_only_hint: read_only,
            destructive_hint: destructive,
            idempotent_hint: None,
            open_world_hint: open_world,
        }
    }

    fn ask() -> PermissionDecision {
        PermissionDecision::Ask {
            prompt: "Allow mcp__docs__write?".to_string(),
            reason: "default mode asks before mcp actions".to_string(),
        }
    }

    fn allow() -> PermissionDecision {
        PermissionDecision::Allow {
            reason: "allow rule matched".to_string(),
        }
    }

    fn deny() -> PermissionDecision {
        PermissionDecision::Deny {
            reason: "deny rule matched".to_string(),
        }
    }

    #[test]
    fn no_hints_keep_the_engine_decision() {
        let base = ask();
        assert_eq!(
            apply_mcp_annotations(None, PermissionMode::Default, base.clone(), false),
            base
        );
        assert_eq!(
            apply_mcp_annotations(
                Some(&annotations(None, None, None)),
                PermissionMode::Default,
                base.clone(),
                false
            ),
            base
        );
    }

    #[test]
    fn read_only_hint_only_allows_in_read_only_mode() {
        let hints = annotations(Some(true), None, None);
        let mode_denial = PermissionDecision::Deny {
            reason: "Permission mode 'read-only' does not allow mcp actions".to_string(),
        };
        assert_eq!(
            apply_mcp_annotations(
                Some(&hints),
                PermissionMode::ReadOnly,
                mode_denial.clone(),
                false
            ),
            PermissionDecision::Allow {
                reason: READ_ONLY_HINT_REASON.to_string()
            }
        );
        // default 模式不因 readOnlyHint 放行。
        assert_eq!(
            apply_mcp_annotations(Some(&hints), PermissionMode::Default, ask(), false),
            ask()
        );
    }

    #[test]
    fn destructive_hint_forces_ask_even_over_an_allow_rule() {
        let hints = annotations(None, Some(true), None);
        let decision = apply_mcp_annotations(Some(&hints), PermissionMode::Default, allow(), false);
        assert!(decision.is_ask(), "{decision:?}");
        assert_eq!(decision.reason(), DESTRUCTIVE_HINT_REASON);
        // 已有的 ask prompt 被保留。
        let decision = apply_mcp_annotations(Some(&hints), PermissionMode::Default, ask(), false);
        match decision {
            PermissionDecision::Ask { prompt, reason } => {
                assert_eq!(prompt, "Allow mcp__docs__write?");
                assert_eq!(reason, DESTRUCTIVE_HINT_REASON);
            }
            other => panic!("expected ask, got {other:?}"),
        }
    }

    #[test]
    fn open_world_hint_adds_an_extra_ask() {
        let hints = annotations(None, None, Some(true));
        let decision = apply_mcp_annotations(Some(&hints), PermissionMode::Default, allow(), false);
        assert!(decision.is_ask(), "{decision:?}");
        assert_eq!(decision.reason(), OPEN_WORLD_HINT_REASON);
    }

    #[test]
    fn hints_never_override_a_deny_rule() {
        for hints in [
            annotations(Some(true), None, None),
            annotations(None, Some(true), None),
            annotations(None, None, Some(true)),
        ] {
            assert!(
                apply_mcp_annotations(Some(&hints), PermissionMode::Default, deny(), true)
                    .is_deny()
            );
            // 即使调用方漏传 `explicit_deny`，default 模式下的 Deny 也不会被翻案。
            assert!(
                apply_mcp_annotations(Some(&hints), PermissionMode::Default, deny(), false)
                    .is_deny()
            );
        }
        // read-only 模式下显式 deny 优先于 readOnlyHint。
        let hints = annotations(Some(true), None, None);
        assert!(
            apply_mcp_annotations(Some(&hints), PermissionMode::ReadOnly, deny(), true).is_deny()
        );
    }

    #[test]
    fn bypass_mode_is_not_changed_by_hints() {
        let hints = annotations(Some(true), Some(true), Some(true));
        assert!(
            apply_mcp_annotations(
                Some(&hints),
                PermissionMode::BypassPermissions,
                allow(),
                false
            )
            .is_allow()
        );
    }

    #[test]
    fn destructive_and_open_world_tools_disallow_persistent_approval() {
        assert!(allows_persistent_approval(None));
        assert!(allows_persistent_approval(Some(&annotations(
            Some(true),
            None,
            None
        ))));
        assert!(!allows_persistent_approval(Some(&annotations(
            None,
            Some(true),
            None
        ))));
        assert!(!allows_persistent_approval(Some(&annotations(
            None,
            None,
            Some(true)
        ))));
    }
}
