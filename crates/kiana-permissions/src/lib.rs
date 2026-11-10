//! 工具调用权限引擎。
//!
//! # 这个 crate 解决什么问题
//!
//! `kiana-sandbox` 回答「命令在什么隔离环境里跑」；本 crate 回答「这次工具调用是否允许」。
//! 两者独立：可以只开沙箱不审批，也可以只审批不沙箱，推荐同时开启。
//!
//! 设计参考 Codex 的 `AskForApproval` / `execpolicy` 与 Claude Code 的 permission modes，
//! 但策略模型按 Kiana 的工具集合重写：
//!
//! - `read` / `ls` / `grep` / `find` → 读；
//! - `edit` / `write` → 写；
//! - `bash` / `powershell` → 执行（附带命令、写入路径与网络启发式）；
//! - `mcp__*` → MCP 工具；
//! - 其它 → `Other`，默认询问。
//!
//! # 模式
//!
//! | 模式 | 读 | 写 | 执行 | 网络 / MCP / 其它 |
//! |---|---|---|---|---|
//! | `read-only` | 允许 | 拒绝 | 拒绝 | 拒绝 |
//! | `default` | 允许 | 询问 | 询问 | 询问 |
//! | `accept-edits` | 允许 | 工作区内允许，区外询问 | 询问 | 询问 |
//! | `bypass` | 允许 | 允许 | 允许 | 允许 |
//!
//! # 决策顺序
//!
//! 1. 灾难性命令（`rm -rf /`、`mkfs`、`dd of=/dev/...`、fork bomb、`curl | sh`）→ Deny；
//!    只有 `bypass` 模式跳过这一步。
//! 2. `Deny` 规则。
//! 3. `.git`/`.kiana` 受保护目录写入 → 每次 Ask（不受任何批准影响）。
//! 4. 用户「总是允许」生成的 `Approved` 规则（`Session` 作用域对应会话批准）。
//! 5. `Ask` 规则（含配置里的 `permissions.ask`）。
//! 6. `Allow` 规则。
//! 7. 模式默认。
//!
//! 规则是 deny-overrides：显式 Deny 永远赢；配置里的 `Ask` 优先于 `Allow`，所以
//! “允许 bash，但 `rm` 要先问”可以用 `Tool{bash,Allow}` + `CommandPrefix{["rm"],Ask}`
//! 表达。用户通过「总是允许」生成的 `Approved` 规则是显式批准，先于 `Ask` 规则执行；
//! 但 Deny、灾难性命令与 `.git`/`.kiana` 安全检查仍然优先，受保护目录写入每次都要问。
//!
//! # 命令规则的安全边界
//!
//! 规则有三种命令形态：`ExactCommand`（完整命令）、`CommandPrefix`（前缀）与
//! `CommandWildcard`（显式 `*` 通配）。**Allow 型命令规则只对单一简单命令生效**：
//! 包含 `;`、`|`、`&`、`>`、`<`、反引号、`$(`、换行或未闭合引号的命令不会被前缀
//! allow 规则放行，否则 `git status; rm -rf /` 会被 `["git","status"]` 规则放过去。
//! Ask/Deny 型规则会扫描复合命令的每一段（包括 `bash -c` 内层与命令替换），因为
//! 拒绝只会收窄权限。分析失败/超过 50 个子命令时，[`ShellAnalysis`] 返回
//! `Unknown`/`Incomplete`，引擎在应用任何 Allow 规则之前先按 Ask 处理（bypass 与
//! read-only 除外：前者显式关闭检查，后者直接 Deny）。
//!
//! [`ShellAnalysis::virtual_operations`] 还会把 `cat`/`ls`/`curl`/`wget`/重定向映射成
//! `read_file`/`list_directory`/`web_fetch`/`write_file`，让路径规则也能约束 shell 的
//! 等价操作。这只是启发式而不是完整 shell 解析；真正的隔离由 `kiana-sandbox` 负责。
//!
//! # 与 UI 的关系
//!
//! 本 crate 只产出 [`PermissionDecision`]；`Ask` 由调用方决定怎么问。`kiana-session`
//! 把 `Ask` 接到结构化审批：用户可选「只允许一次」「总是允许（会话/本地/项目）」或
//! 「拒绝（可填原因）」，结果见 [`ApprovalDecision`]。`AllowAlways` 只生成 `Allow`
//! 规则，`Deny` 规则与灾难性命令检查仍然优先。
//!
//! # 环境变量
//!
//! `KIANA_PERMISSIONS`：`read-only` / `default` / `accept-edits` / `bypass` / `off`。
//! 未设置或 `off` 时不启用权限检查；未知值按 `default` 处理（更安全，不会静默放行）。

mod approval;
mod command;
mod mode;
mod policy;
mod request;
mod shell;

pub use approval::{ApprovalDecision, ApprovalScope, approval_rules};
pub use mode::PermissionMode;
pub use policy::{
    PermissionDecision, PermissionEngine, PermissionFlags, PermissionPolicy, PermissionRule,
    RuleDecision, parse_cli_rule,
};
pub use request::{ActionKind, PermissionRequest, resolve_permission_path};
pub use shell::{
    MAX_SUBCOMMANDS, Redirection, RedirectionKind, ShellAnalysis, ShellAnalysisStatus,
    ShellAnalyzer, ShellKind, ShellRuleSuggestion, ShellSubcommand, VirtualOperation,
};
