//! 面板命令注册表与 headless 文本回退（`/permissions`、`/tasks`）。
//!
//! `docs/history/porting/tui-panels-design.md` §9.1 要求把面板命令的名字/描述与
//! headless 文本集中到 `kiana-session`：
//!
//! - TUI 侧（`crates/kiana-interactive`）只消费 [`PANEL_COMMANDS`] 做补全与分派；
//! - print 文本模式（`crates/kiana-rpc-print`）在 `session.prompt` 之前调用
//!   [`execute_headless`]，输出稳定文本而不触发模型请求。
//!
//! 注册表**不包含** `/mcp`：它由内建扩展注册（`kiana-builtin-extensions/src/mcp`），
//! 面板与扩展命令的所有权不同，不能在这里同名遮蔽。24 个内置命令同样不在表内
//! （`kiana-core::slash_commands::BUILTIN_SLASH_COMMANDS` 是冻结的 TS 契约）。
//!
//! # 输出格式（新增稳定面）
//!
//! `/permissions`：
//!
//! ```text
//! Permissions
//!   mode: default
//!   workspace roots: /repo, /repo/extra
//!   allow:
//!     tool:read
//!   ask:
//!     (none)
//!   deny:
//!     (none)
//!   session approvals: 2
//! ```
//!
//! `/tasks`：
//!
//! ```text
//! Background tasks (2):
//!   [running] shell  "cargo test --all"  task_8f...  output: /path/task_8f....output
//! ```
//!
//! 两条文本都不写 golden fixture 目录（避免与 TS fixture 混淆），由本文档与单元测试锁定。
//!
//! # 边界与失败模式
//!
//! - 未知参数：返回 `Err("Usage: /<name>")`；调用方（print 模式）写 stderr 并以 1 退出，
//!   TUI 侧则在状态行显示同一句。
//! - 权限层关闭：`mode: off`，规则与目录照常输出（引擎可能仍有 CLI/managed 规则）。
//! - 后台任务 manager 未启用：输出 `Background tasks are not enabled for this session.`，
//!   与“已启用但没有任务”的 `No background tasks.` 区分。
//! - 所有来自任务记录的自由文本先经 [`sanitize_status_text`] 去 ANSI/控制字符并折叠成
//!   单行，避免终端注入。

use kiana_permissions::{PermissionRule, RuleDecision};

use crate::AgentSession;
use crate::background::BackgroundTaskRecord;

/// 面板命令的元数据。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PanelCommandInfo {
    /// 命令名（不含前导 `/`）。
    pub name: &'static str,
    /// 补全里的描述。
    pub description: &'static str,
    /// 参数提示；`None` 表示命令不接受参数。
    pub argument_hint: Option<&'static str>,
    /// 是否有 TUI 面板（`false` = 只有 headless 文本）。
    pub tui: bool,
}

/// 面板命令表。
///
/// 名字必须与 `BUILTIN_SLASH_COMMANDS` 以及扩展命令（`mcp`/`llama`）无交集；
/// `crates/kiana-session/tests/panel_commands.rs` 与 interactive 侧的注册测试锁定这一点。
pub const PANEL_COMMANDS: &[PanelCommandInfo] = &[
    PanelCommandInfo {
        name: "permissions",
        description: "Inspect and edit permission rules and workspace directories",
        argument_hint: None,
        tui: true,
    },
    PanelCommandInfo {
        name: "tasks",
        description: "List and cancel background tasks",
        argument_hint: None,
        tui: true,
    },
];

/// 命令名是否是面板命令（不含前导 `/`；精确匹配，大小写敏感）。
pub fn is_panel_command(name: &str) -> bool {
    PANEL_COMMANDS.iter().any(|command| command.name == name)
}

/// 一条完整输入是否是面板命令（`/name` 或 `/name ...`）。
///
/// print 模式用它做**调用前**过滤：只有可能是面板命令的文本才去取 `AgentSession`，
/// 普通消息不改变既有的 `prompt` 路径（测试替身的 `agent_session()` 可以继续 panic）。
pub fn is_panel_command_text(text: &str) -> bool {
    let text = text.trim();
    let name = text.split_whitespace().next().unwrap_or("");
    name.strip_prefix('/').is_some_and(is_panel_command)
}

/// 面板命令名的迭代器。
pub fn panel_command_names() -> impl Iterator<Item = &'static str> {
    PANEL_COMMANDS.iter().map(|command| command.name)
}

/// headless 文本回退入口。
///
/// 返回 `None` 表示这条文本不是面板命令（调用方继续原有的 `prompt` 流程）；
/// `Some(Ok(text))` 是可直接写入 stdout 的文本；`Some(Err(message))` 是用法错误。
///
/// `text` 不必预先 trim；内部同时接受 `"/tasks"` 与 `"/tasks   "`，但不接受
/// `/tasks foo`（面板命令没有参数）。
pub async fn execute_headless(
    session: &AgentSession,
    text: &str,
) -> Option<Result<String, String>> {
    let text = text.trim();
    let (name, args) = match text.split_once(char::is_whitespace) {
        Some((name, args)) => (name, args.trim()),
        None => (text, ""),
    };
    let name = name.strip_prefix('/')?;
    if !is_panel_command(name) {
        return None;
    }
    if !args.is_empty() {
        return Some(Err(format!("Usage: /{name}")));
    }
    match name {
        "permissions" => Some(Ok(format_permissions(session))),
        "tasks" => Some(format_tasks(session).await),
        _ => None,
    }
}

/// `/permissions` 的 headless 文本。
pub fn format_permissions(session: &AgentSession) -> String {
    let policy = session.permission_policy();
    let mode = policy
        .as_ref()
        .map(|policy| policy.mode.as_str().to_string())
        .unwrap_or_else(|| "off".to_string());
    let workspace_roots = policy
        .as_ref()
        .map(|policy| {
            policy
                .workspace_roots
                .iter()
                .map(|root| sanitize_status_text(&root.display().to_string()))
                .collect::<Vec<String>>()
                .join(", ")
        })
        .filter(|roots| !roots.is_empty())
        .unwrap_or_else(|| "(none)".to_string());
    let rules = policy.map(|policy| policy.rules).unwrap_or_default();

    let mut lines = Vec::new();
    lines.push("Permissions".to_string());
    lines.push(format!("  mode: {mode}"));
    lines.push(format!("  workspace roots: {workspace_roots}"));
    for (label, decision) in [
        ("allow", RuleDecision::Allow),
        ("ask", RuleDecision::Ask),
        ("deny", RuleDecision::Deny),
    ] {
        lines.push(format!("  {label}:"));
        let matching: Vec<&PermissionRule> = rules
            .iter()
            .filter(|rule| rule.decision() == decision)
            .collect();
        if matching.is_empty() {
            lines.push("    (none)".to_string());
        } else {
            for rule in matching {
                lines.push(format!(
                    "    {}",
                    sanitize_status_text(&display_rule_text(rule))
                ));
            }
        }
    }
    lines.push(format!(
        "  session approvals: {}",
        session.permission_session_allow_count()
    ));
    lines.join("\n")
}

/// `/tasks` 的 headless 文本。
pub async fn format_tasks(session: &AgentSession) -> Result<String, String> {
    if session.background_task_manager().is_none() {
        return Ok("Background tasks are not enabled for this session.".to_string());
    }
    let records = session
        .list_background_tasks()
        .await
        .map_err(|error| error.to_string())?;
    Ok(format_task_records(&records))
}

/// 把任务记录格式化成稳定文本（供 [`format_tasks`] 与测试复用）。
pub fn format_task_records(records: &[BackgroundTaskRecord]) -> String {
    let mut records: Vec<&BackgroundTaskRecord> = records.iter().collect();
    records.sort_by(|left, right| {
        left.created_at_ms
            .cmp(&right.created_at_ms)
            .then_with(|| left.task_id.cmp(&right.task_id))
    });
    if records.is_empty() {
        return "No background tasks.".to_string();
    }

    let mut lines = vec![format!("Background tasks ({}):", records.len())];
    for record in records {
        let status = record.status.as_str();
        let kind = task_kind_label(&record.tool);
        let description = sanitize_status_text(record.description.as_deref().unwrap_or(""));
        let task_id = sanitize_status_text(&record.task_id);
        let output = if shows_output_path(&record.tool) {
            format!("  output: {}", sanitize_status_text(&record.output_path))
        } else {
            String::new()
        };
        lines.push(format!(
            "  [{status}] {kind}  \"{description}\"  {task_id}{output}"
        ));
    }
    lines.join("\n")
}

/// `BackgroundTaskRecord.tool` → 展示类别。
fn task_kind_label(tool: &str) -> &str {
    match tool {
        "agent" | "task" | "subagent" => "agent",
        "bash" | "shell" | "execute_bash" | "powershell" => "shell",
        "monitor" | "monitors" => "monitor",
        other => other,
    }
}

/// 是否在文本里输出路径（与 qwen 的 tasksCommand 一致：只对 agent/shell）。
fn shows_output_path(tool: &str) -> bool {
    matches!(task_kind_label(tool), "agent" | "shell")
}

/// 规则的稳定展示文本（与 `/permissions` 面板的 `display_rule_text` 同形）。
fn display_rule_text(rule: &PermissionRule) -> String {
    match rule {
        PermissionRule::Tool { pattern, .. } => format!("tool:{pattern}"),
        PermissionRule::CommandPrefix { tokens, .. } => format!("command:{}", tokens.join(" ")),
        PermissionRule::PathPrefix { prefix, .. } => format!("path:{}", prefix.display()),
        PermissionRule::ExactCommand { command, .. } => format!("exact:{command}"),
        PermissionRule::CommandWildcard { pattern, .. } => format!("wildcard:{pattern}"),
        PermissionRule::Approved { rule, .. } => {
            format!("approved:{}", display_rule_text(rule))
        }
    }
}

/// 去掉 ANSI 转义与 C0/C1 控制字符，把换行/制表折叠成单个空格。
///
/// 任务描述、工具名与输出路径都来自进程/用户输入，直接拼进终端会允许注入转义序列。
/// 与 `/tasks` 面板的 `sanitize_task_text` 语义一致（面板版本在 interactive crate，
/// 这里不能反向依赖它）。
pub fn sanitize_status_text(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut pending_space = false;
    while let Some(character) = chars.next() {
        if character == '\x1b' {
            match chars.peek() {
                // CSI：ESC [ ... 终止字节（@..~）。
                Some('[') => {
                    chars.next();
                    for next in chars.by_ref() {
                        if ('@'..='~').contains(&next) {
                            break;
                        }
                    }
                }
                // OSC/APC/PM：ESC ] | _ | ^ ... BEL 或 ST。
                Some(']') | Some('_') | Some('^') => {
                    chars.next();
                    while let Some(next) = chars.next() {
                        if next == '\x07' {
                            break;
                        }
                        if next == '\x1b' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                Some(_) => {
                    chars.next();
                }
                None => {}
            }
            continue;
        }
        if character.is_control() {
            if matches!(character, '\n' | '\r' | '\t' | '\u{000b}' | '\u{000c}')
                || character.is_whitespace()
            {
                pending_space = !output.is_empty();
            }
            continue;
        }
        if character.is_whitespace() {
            pending_space = !output.is_empty();
            continue;
        }
        if pending_space {
            output.push(' ');
            pending_space = false;
        }
        output.push(character);
    }
    output.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panel_commands_do_not_include_mcp_or_builtins() {
        assert!(is_panel_command("permissions"));
        assert!(is_panel_command("tasks"));
        assert!(!is_panel_command("mcp"));
        assert!(!is_panel_command("settings"));
        assert!(!is_panel_command("llama"));
    }

    #[test]
    fn sanitize_strips_ansi_and_controls() {
        let text = "\x1b[31mred\x1b[0m\nnext\tline\x07";
        assert_eq!(sanitize_status_text(text), "red next line");
    }

    #[test]
    fn task_kind_mapping_matches_panel() {
        assert_eq!(task_kind_label("bash"), "shell");
        assert_eq!(task_kind_label("agent"), "agent");
        assert_eq!(task_kind_label("monitor"), "monitor");
        assert_eq!(task_kind_label("custom"), "custom");
    }
}
