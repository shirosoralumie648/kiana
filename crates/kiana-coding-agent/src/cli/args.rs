//! `cli/args.ts`（479 行）：`Args` / `Diagnostic` / `parse_args` / `print_help` / thinking 校验。
//!
//! 逐条复刻 `parseArgs` 的分支顺序与边角行为（spec/ca-cli.md §2.5.2 + Audit §A.1）：
//! 末尾缺值的长选项落入未知长选项分支、`--name` 会消费以 `-` 开头的值、
//! `--print` 只消费非 `@` 且（非 `-` 或 `---`）开头的下一项，等等。
//!
//! TS: `packages/coding-agent/src/cli/args.ts`
//!
//! # 这个文件解决什么问题
//! 启动流程在创建会话、加载扩展之前，必须先把原始 argv 分成几类：内置选项（[`Args`] 的
//! 具名字段）、扩展注册的 flag（此时还不知道有哪些扩展，只能先暂存）、消息文本与 `@file`。
//! [`parse_args`] 就是这个纯分类层：只读 `&[String]`，不访问环境变量、文件系统或网络，
//! 返回一个 [`Args`] 值。没有它，后续每条命令都得自己重复处理 `--`、`@file`、缺值与
//! 未知选项的优先级。
//!
//! 直接调用方：`app::main`（对所有 argv 做一次全局解析）、`app::run_auth_command` /
//! `execute_auth_command`（对 `auth` 命令剥离后的剩余参数再解析一次）；另外
//! `sdk_exports` 把 [`parse_args`] 以 `parseArgs` 的名字再导出给 SDK 使用者。
//!
//! # 解析流程
//! 单趟从左到右扫描，分支顺序本身是契约（`tests/args.rs` 的 `audit_*` 用例锁定）：
//! 1. `--`：其后所有 token 不再按选项识别，`@` 开头的进 `file_args`（去掉 `@`），
//!    其余进 `messages`，然后结束循环；
//! 2. 内置选项按固定顺序精确匹配；带值选项大多带 `i + 1 < len` 前置条件；
//! 3. 单独的 `@...` 进 `file_args`；
//! 4. 其余 `--long` 进 `unknown_flags`（`--long=value` 先按 `=` 切分）；
//! 5. 其余 `-...` 记一条 `Unknown option` 错误；
//! 6. 剩下的 token 作为位置消息。
//!
//! 顺序不能调整：`--print` 与未知长选项都可能“顺带”消费下一项；若把未知长选项分支
//! 提前，很多带值内置选项的取值与消息会被误吞。
//!
//! # 失败模式
//! [`parse_args`] **不返回 `Result`**：缺值、非法枚举值都累积在 [`Args::diagnostics`]，
//! 由调用方统一消费（`app::main` 打印每条诊断，只要存在 error 就返回退出码 1；
//! `--thinking` 的非法值只是 warning）。因此调用方必须检查 `diagnostics`，只读具名字段
//! 会把错误输入当成默认值继续执行。未知长选项不进 `diagnostics`：它可能是扩展在运行时
//! 注册的 flag，是否真的未知由扩展加载后的下游判定。
//!
//! # 输出侧契约
//! [`help_text`] / [`print_help`] 的文本是逐字冻结的用户可见契约（`tests/golden/help*.txt`
//! 由 TS 参考实现生成）：模板中的空格、换行、选项顺序都不能调整；扩展 flag 段由运行时
//! 拼接，见私有函数 `extension_flags_text`。

use indexmap::IndexMap;
use kiana_agent_core::types::ThinkingLevel;
use kiana_config::settings::TuiMode;
use kiana_config::settings::tools::get_tool_list_error;
use kiana_ext_api::types::{ExtensionFlag, FlagType, FlagValue};
use kiana_permissions::{PermissionMode, RuleDecision, parse_cli_rule};
use kiana_tui::text::trim_js;

use crate::cli::setup::{Output, Style};

/// `Mode`：`"text" | "json" | "rpc"`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// `"text"`。
    Text,
    /// `"json"`。
    Json,
    /// `"rpc"`。
    Rpc,
}

impl Mode {
    /// TS 字面量。
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Text => "text",
            Mode::Json => "json",
            Mode::Rpc => "rpc",
        }
    }

    /// 从 TS 字面量解析；非法值返回 `None`。
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "text" => Some(Mode::Text),
            "json" => Some(Mode::Json),
            "rpc" => Some(Mode::Rpc),
            _ => None,
        }
    }
}

impl std::fmt::Display for Mode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// `listModels?: string | true` 的 Rust 表达。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListModelsArg {
    /// `--list-models`（无搜索模式）。
    True,
    /// `--list-models <search>`。
    Search(String),
}

/// `diagnostics[].type`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticKind {
    /// `"warning"`。
    Warning,
    /// `"error"`。
    Error,
}

/// 一条参数解析诊断。
///
/// 只携带类型与裸消息；`app::main` 负责加 `Error: ` / `Warning: ` 前缀、着色并写 stderr，
/// 只要存在 error 就以退出码 1 结束（warning 不阻止启动）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// 类型。
    pub kind: DiagnosticKind,
    /// 逐字消息（不含 `Error: `/`Warning: ` 前缀）。
    pub message: String,
}

/// `Args`（spec/ca-cli.md §2.5.1 全字段）。
///
/// 纯数据产物：`Option` 表示“用户是否给了这个选项”（因此 `--name ""` 是
/// `Some("")`，非空校验留给调用方）；`bool` 只表示选项出现；可重复选项追加进
/// `Vec`（如 `--append-system-prompt`、`--extension`），不可重复的后者覆盖前者
/// （如 `--model`）。[`Args::unknown_flags`] 用 `IndexMap` 保留插入顺序（重复项更新
/// 值但保留首次出现的位置），[`Args::diagnostics`] 按解析顺序累积。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Args {
    /// `--provider`。
    pub provider: Option<String>,
    /// `--model`。
    pub model: Option<String>,
    /// `--api-key`。
    pub api_key: Option<String>,
    /// `--system-prompt`。
    pub system_prompt: Option<String>,
    /// `--append-system-prompt`（可多次）。
    pub append_system_prompt: Option<Vec<String>>,
    /// `--thinking`。
    pub thinking: Option<ThinkingLevel>,
    /// `--continue` / `-c`。
    pub r#continue: bool,
    /// `--resume` / `-r`。
    pub resume: bool,
    /// `--help` / `-h`。
    pub help: bool,
    /// `--version` / `-v`。
    pub version: bool,
    /// `--mode`（重复出现时后者覆盖前者；`kiana-rpc` 入口依赖这一点，让用户传入的
    /// `--mode` 覆盖它强制添加的 `rpc`）。
    pub mode: Option<Mode>,
    /// `--name` / `-n`。
    pub name: Option<String>,
    /// `--no-session`。
    pub no_session: bool,
    /// `--session`。
    pub session: Option<String>,
    /// `--session-id`。
    pub session_id: Option<String>,
    /// `--fork`。
    pub fork: Option<String>,
    /// `--session-dir`。
    pub session_dir: Option<String>,
    /// `--models`。
    pub models: Option<Vec<String>>,
    /// `--tools` / `-t`。
    pub tools: Option<Vec<String>>,
    /// `--exclude-tools` / `-xt`。
    pub exclude_tools: Option<Vec<String>>,
    /// `--max-turns <n>`：本地 turn 预算（覆盖 settings.budget.maxTurns）。
    pub max_turns: Option<u64>,
    /// `--max-tokens <n>`：本地累计 token 预算。
    pub max_tokens: Option<u64>,
    /// `--max-cost-usd <usd>`：本地累计费用预算。
    pub max_cost_usd: Option<f64>,
    /// `--max-wall-clock-ms <ms>`：本地墙钟预算。
    pub max_wall_clock_ms: Option<u64>,
    /// `--no-loop-guard`：关闭 loop guard（覆盖 settings.loopGuard）。
    pub no_loop_guard: bool,
    /// `--loop-guard-stop`：loop guard 命中即硬停。
    pub loop_guard_stop: bool,
    /// `--no-tools` / `-nt`。
    pub no_tools: bool,
    /// `--no-builtin-tools` / `-nbt`。
    pub no_builtin_tools: bool,
    /// `--permission-mode`：覆盖 settings/env 的权限模式。
    pub permission_mode: Option<PermissionMode>,
    /// `--allow`（可多次）：放行规则，语法见 `kiana_permissions::parse_cli_rule`。
    pub permission_allow: Vec<String>,
    /// `--deny`（可多次）：拒绝规则。
    pub permission_deny: Vec<String>,
    /// `--add-dir`（可多次）：追加可写工作区根。
    pub add_dirs: Vec<String>,
    /// `--extension` / `-e`。
    pub extensions: Option<Vec<String>>,
    /// `--no-extensions` / `-ne`。
    pub no_extensions: bool,
    /// `--no-mcp`。
    pub no_mcp: bool,
    /// `--print` / `-p`。
    pub print: bool,
    /// `--export`。
    pub export: Option<String>,
    /// `--no-skills` / `-ns`。
    pub no_skills: bool,
    /// `--skill`。
    pub skills: Option<Vec<String>>,
    /// `--prompt-template`。
    pub prompt_templates: Option<Vec<String>>,
    /// `--no-prompt-templates` / `-np`。
    pub no_prompt_templates: bool,
    /// `--theme`。
    pub themes: Option<Vec<String>>,
    /// `--use-theme`。
    pub use_theme: Option<String>,
    /// `--no-themes`。
    pub no_themes: bool,
    /// `--no-context-files` / `-nc`。
    pub no_context_files: bool,
    /// `--list-models [search]`。
    pub list_models: Option<ListModelsArg>,
    /// `--offline`（**死字段**：main 不读取它，见 spec §A.2）。
    pub offline: bool,
    /// `--tui-mode`。
    pub tui_mode: Option<TuiMode>,
    /// `--verbose`。
    pub verbose: bool,
    /// `--approve` / `--no-approve`。
    pub project_trust_override: Option<bool>,
    /// `--serve`：启动 daemon 事件服务（SSE / WebSocket / REST），不进入交互/打印模式。
    pub serve: bool,
    /// `--serve-addr <host:port>`：daemon 监听地址（默认 `127.0.0.1:0`）。
    pub serve_addr: Option<String>,
    /// `--serve-token <token>`：覆盖自动生成的 bearer token。
    pub serve_token: Option<String>,
    /// 位置参数（消息）。
    pub messages: Vec<String>,
    /// `@file` 参数（已去掉 `@`）。
    pub file_args: Vec<String>,
    /// 未知 flag（可能是扩展 flag）：名字 → `true` 或字符串值，保持插入顺序。
    pub unknown_flags: IndexMap<String, FlagValue>,
    /// 诊断。
    pub diagnostics: Vec<Diagnostic>,
}

/// `VALID_THINKING_LEVELS`（顺序即错误消息里的顺序）。
pub const VALID_THINKING_LEVELS: [&str; 7] =
    ["off", "minimal", "low", "medium", "high", "xhigh", "max"];

/// `isValidThinkingLevel(level)`。
pub fn is_valid_thinking_level(level: &str) -> bool {
    VALID_THINKING_LEVELS.contains(&level)
}

/// 把合法的 thinking 字面量解析成枚举。
pub fn parse_thinking_level(level: &str) -> Option<ThinkingLevel> {
    Some(match level {
        "off" => ThinkingLevel::Off,
        "minimal" => ThinkingLevel::Minimal,
        "low" => ThinkingLevel::Low,
        "medium" => ThinkingLevel::Medium,
        "high" => ThinkingLevel::High,
        "xhigh" => ThinkingLevel::Xhigh,
        "max" => ThinkingLevel::Max,
        _ => return None,
    })
}

/// `normalizeSessionName(value)`：JS `trim()` 后非空则返回。
///
/// 去空白使用 `trim_js`（JS 的 whitespace 集合，含 BOM 等 Rust `str::trim` 不处理的字符）。
/// 只有空白时返回 `None`；`app.rs` 据此输出 `Error: --name requires a non-empty value` 并退出 1。
pub fn normalize_session_name(value: &str) -> Option<String> {
    let name = trim_js(value);
    (!name.is_empty()).then(|| name.to_string())
}

fn push_error(result: &mut Args, message: impl Into<String>) {
    result.diagnostics.push(Diagnostic {
        kind: DiagnosticKind::Error,
        message: message.into(),
    });
}

fn push_warning(result: &mut Args, message: impl Into<String>) {
    result.diagnostics.push(Diagnostic {
        kind: DiagnosticKind::Warning,
        message: message.into(),
    });
}

/// 逗号分割 → JS `trim()` → 过滤空串（`--models` / `--tools` / `--exclude-tools`）。
fn split_list(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(trim_js)
        .filter(|entry| !entry.is_empty())
        .map(str::to_string)
        .collect()
}

/// `parseArgs(args)`（逐字规则见 spec/ca-cli.md §2.5.2）。
///
/// # 输入与输出
/// 输入是**不含程序名**的参数切片（调用方先去掉 `argv[0]`）；输出总是 [`Args`]，
/// 解析过程中不产生 `Result`、不打印任何东西。未知长选项与诊断分别落在
/// `unknown_flags` / `diagnostics`，调用方必须自行处理。
///
/// # 关键边界（`tests/args.rs` 逐条锁定）
/// - 大多数带值选项恰好位于末尾时：不报缺值错误、不赋值，继续走后面的分支——
///   长选项 `--model` 变成 `unknown_flags["model"] = Bool(true)`，短选项 `-t` 变成
///   `Unknown option: -t` 错误诊断。例外：`--name` / `-n` 在末尾缺值会报
///   `--name requires a value` 错误（`reports_missing_value`）。
/// - `--mode` / `--use-theme` / `--tui-mode` 是“缺值即 error 且不消费下一项”的
///   选项；`--mode` 的下一项即使以 `-` 开头也不会被当作值消费。
/// - `--name` / `-n` 没有这种保护：只要还有下一项就消费，哪怕它是 `--foo`
///   （`audit_name_consumes_a_leading_dash_value`）。
/// - `--print` 只把“不以 `@` 开头且（不以 `-` 开头或以 `---` 开头）”的下一项当
///   消息，因此 `-p "---\ntitle: ..."` 这类 YAML frontmatter 不会被当成选项。
/// - `--list-models` 的可选搜索值必须不以 `-`/`@` 开头，否则退化为
///   [`ListModelsArg::True`]。
/// - `--permission-mode` / `--allow` / `--deny` / `--add-dir` 的取值不能以 `-` 开头
///   （`--flag=value` 形式除外）：缺值或后面紧跟另一个选项时记 error 诊断，且不消费
///   该选项，后面的 flag 继续按正常流程解析。
/// - 未知长选项：`--x=v` 一定取 `v`（`v` 可为空串）；`--x v` 仅在 `v` 不以
///   `-`/`@` 开头时消费 `v`，否则记 `Bool(true)` 且 `v` 留给后续分支。
pub fn parse_args(args: &[String]) -> Args {
    let mut result = Args::default();

    let mut i = 0usize;
    while i < args.len() {
        let arg = args[i].as_str();

        // 选项终止符：其后的 token 全部按位置参数处理，不再识别任何 flag。
        if arg == "--" {
            for positional_arg in &args[i + 1..] {
                if let Some(file) = positional_arg.strip_prefix('@') {
                    result.file_args.push(file.to_string());
                } else {
                    result.messages.push(positional_arg.clone());
                }
            }
            break;
        } else if arg == "--help" || arg == "-h" {
            result.help = true;
        } else if arg == "--version" || arg == "-v" {
            result.version = true;
        } else if arg == "--mode" {
            let mode = args.get(i + 1);
            match mode {
                None => push_error(&mut result, "--mode requires text, json, or rpc"),
                Some(mode) if mode.starts_with('-') => {
                    push_error(&mut result, "--mode requires text, json, or rpc");
                }
                Some(mode) => {
                    i += 1;
                    match Mode::parse(mode) {
                        Some(parsed) => result.mode = Some(parsed),
                        None => push_error(
                            &mut result,
                            format!("Invalid mode \"{mode}\". Valid values: text, json, rpc"),
                        ),
                    }
                }
            }
        } else if arg == "--continue" || arg == "-c" {
            result.r#continue = true;
        } else if arg == "--resume" || arg == "-r" {
            result.resume = true;
        } else if arg == "--provider" && i + 1 < args.len() {
            i += 1;
            result.provider = Some(args[i].clone());
        } else if arg == "--model" && i + 1 < args.len() {
            i += 1;
            result.model = Some(args[i].clone());
        } else if arg == "--api-key" && i + 1 < args.len() {
            i += 1;
            result.api_key = Some(args[i].clone());
        } else if arg == "--system-prompt" && i + 1 < args.len() {
            i += 1;
            result.system_prompt = Some(args[i].clone());
        } else if arg == "--append-system-prompt" && i + 1 < args.len() {
            i += 1;
            result
                .append_system_prompt
                .get_or_insert_with(Vec::new)
                .push(args[i].clone());
        } else if arg == "--name" || arg == "-n" {
            if i + 1 < args.len() {
                i += 1;
                result.name = Some(args[i].clone());
            } else {
                push_error(&mut result, "--name requires a value");
            }
        } else if arg == "--no-session" {
            result.no_session = true;
        } else if arg == "--session" && i + 1 < args.len() {
            i += 1;
            result.session = Some(args[i].clone());
        } else if arg == "--session-id" && i + 1 < args.len() {
            i += 1;
            result.session_id = Some(args[i].clone());
        } else if arg == "--fork" && i + 1 < args.len() {
            i += 1;
            result.fork = Some(args[i].clone());
        } else if arg == "--session-dir" && i + 1 < args.len() {
            i += 1;
            result.session_dir = Some(args[i].clone());
        } else if arg == "--models" && i + 1 < args.len() {
            i += 1;
            result.models = Some(split_list(&args[i]));
        } else if arg == "--no-tools" || arg == "-nt" {
            result.no_tools = true;
        } else if arg == "--no-builtin-tools" || arg == "-nbt" {
            result.no_builtin_tools = true;
        } else if (arg == "--tools" || arg == "-t") && i + 1 < args.len() {
            i += 1;
            let tools = split_list(&args[i]);
            match get_tool_list_error(&tools) {
                Some(error) => push_error(&mut result, format!("{arg}: {error}")),
                None => result.tools = Some(tools),
            }
        } else if (arg == "--exclude-tools" || arg == "-xt") && i + 1 < args.len() {
            i += 1;
            result.exclude_tools = Some(split_list(&args[i]));
        } else if arg == "--max-turns" && i + 1 < args.len() {
            i += 1;
            match args[i].parse::<u64>() {
                Ok(value) if value > 0 => result.max_turns = Some(value),
                _ => push_error(&mut result, "--max-turns requires a positive integer"),
            }
        } else if arg == "--max-tokens" && i + 1 < args.len() {
            i += 1;
            match args[i].parse::<u64>() {
                Ok(value) if value > 0 => result.max_tokens = Some(value),
                _ => push_error(&mut result, "--max-tokens requires a positive integer"),
            }
        } else if arg == "--max-cost-usd" && i + 1 < args.len() {
            i += 1;
            match args[i].parse::<f64>() {
                Ok(value) if value.is_finite() && value > 0.0 => result.max_cost_usd = Some(value),
                _ => push_error(&mut result, "--max-cost-usd requires a positive number"),
            }
        } else if arg == "--max-wall-clock-ms" && i + 1 < args.len() {
            i += 1;
            match args[i].parse::<u64>() {
                Ok(value) if value > 0 => result.max_wall_clock_ms = Some(value),
                _ => push_error(
                    &mut result,
                    "--max-wall-clock-ms requires a positive integer",
                ),
            }
        } else if arg == "--no-loop-guard" {
            result.no_loop_guard = true;
        } else if arg == "--loop-guard-stop" {
            result.loop_guard_stop = true;
        } else if let Some(value) = arg.strip_prefix("--permission-mode=") {
            match PermissionMode::parse(value) {
                Some(mode) => result.permission_mode = Some(mode),
                None => push_error(
                    &mut result,
                    format!(
                        "Invalid permission mode \"{value}\". Valid values: read-only, default, accept-edits, bypass"
                    ),
                ),
            }
        } else if arg == "--permission-mode" {
            match args.get(i + 1) {
                // 下一个 token 也是选项时不能把它当成模式值：`--deny --permission-mode
                // read-only` 里的 `--permission-mode` 必须留给后续分支，否则会同时丢掉
                // 真正的模式和一条本不存在的规则。
                Some(next) if next.starts_with('-') => push_error(
                    &mut result,
                    "--permission-mode requires read-only, default, accept-edits, or bypass",
                ),
                Some(next) => {
                    i += 1;
                    match PermissionMode::parse(next) {
                        Some(mode) => result.permission_mode = Some(mode),
                        None => push_error(
                            &mut result,
                            format!(
                                "Invalid permission mode \"{next}\". Valid values: read-only, default, accept-edits, bypass"
                            ),
                        ),
                    }
                }
                None => push_error(
                    &mut result,
                    "--permission-mode requires read-only, default, accept-edits, or bypass",
                ),
            }
        } else if let Some(value) = arg.strip_prefix("--allow=") {
            if parse_cli_rule(value, RuleDecision::Allow).is_none() {
                push_error(&mut result, format!("Invalid --allow rule \"{value}\""));
            }
            result.permission_allow.push(value.to_string());
        } else if arg == "--allow" {
            match args.get(i + 1) {
                Some(next) if !next.starts_with('-') => {
                    i += 1;
                    if parse_cli_rule(next, RuleDecision::Allow).is_none() {
                        push_error(&mut result, format!("Invalid --allow rule \"{next}\""));
                    }
                    result.permission_allow.push(next.clone());
                }
                _ => push_error(&mut result, "--allow requires a rule value"),
            }
        } else if let Some(value) = arg.strip_prefix("--deny=") {
            if parse_cli_rule(value, RuleDecision::Deny).is_none() {
                push_error(&mut result, format!("Invalid --deny rule \"{value}\""));
            }
            result.permission_deny.push(value.to_string());
        } else if arg == "--deny" {
            match args.get(i + 1) {
                Some(next) if !next.starts_with('-') => {
                    i += 1;
                    if parse_cli_rule(next, RuleDecision::Deny).is_none() {
                        push_error(&mut result, format!("Invalid --deny rule \"{next}\""));
                    }
                    result.permission_deny.push(next.clone());
                }
                _ => push_error(&mut result, "--deny requires a rule value"),
            }
        } else if let Some(value) = arg.strip_prefix("--add-dir=") {
            if value.is_empty() {
                push_error(&mut result, "--add-dir requires a directory");
            } else {
                result.add_dirs.push(value.to_string());
            }
        } else if arg == "--add-dir" {
            match args.get(i + 1) {
                Some(next) if !next.starts_with('-') => {
                    i += 1;
                    result.add_dirs.push(next.clone());
                }
                _ => push_error(&mut result, "--add-dir requires a directory"),
            }
        } else if arg == "--thinking" && i + 1 < args.len() {
            i += 1;
            let level = args[i].as_str();
            match parse_thinking_level(level) {
                Some(thinking) => result.thinking = Some(thinking),
                None => push_warning(
                    &mut result,
                    format!(
                        "Invalid thinking level \"{level}\". Valid values: {}",
                        VALID_THINKING_LEVELS.join(", ")
                    ),
                ),
            }
        } else if arg == "--print" || arg == "-p" {
            result.print = true;
            // TS 规则：只把“非 @ 且（非 - 或以 --- 开头）”的下一项当消息；
            // `---` 开头的 YAML frontmatter 因此可作为 prompt 紧跟 -p。
            if let Some(next) = args.get(i + 1)
                && !next.starts_with('@')
                && (!next.starts_with('-') || next.starts_with("---"))
            {
                result.messages.push(next.clone());
                i += 1;
            }
        } else if arg == "--export" && i + 1 < args.len() {
            i += 1;
            result.export = Some(args[i].clone());
        } else if (arg == "--extension" || arg == "-e") && i + 1 < args.len() {
            i += 1;
            result
                .extensions
                .get_or_insert_with(Vec::new)
                .push(args[i].clone());
        } else if arg == "--no-extensions" || arg == "-ne" {
            result.no_extensions = true;
        } else if arg == "--no-mcp" {
            result.no_mcp = true;
        } else if arg == "--skill" && i + 1 < args.len() {
            i += 1;
            result
                .skills
                .get_or_insert_with(Vec::new)
                .push(args[i].clone());
        } else if arg == "--prompt-template" && i + 1 < args.len() {
            i += 1;
            result
                .prompt_templates
                .get_or_insert_with(Vec::new)
                .push(args[i].clone());
        } else if arg == "--theme" && i + 1 < args.len() {
            i += 1;
            result
                .themes
                .get_or_insert_with(Vec::new)
                .push(args[i].clone());
        } else if arg == "--use-theme" {
            match args.get(i + 1) {
                None => push_error(&mut result, "--use-theme requires a theme name"),
                Some(theme) if theme.starts_with('-') => {
                    push_error(&mut result, "--use-theme requires a theme name");
                }
                Some(theme) => {
                    result.use_theme = Some(theme.clone());
                    i += 1;
                }
            }
        } else if arg == "--no-skills" || arg == "-ns" {
            result.no_skills = true;
        } else if arg == "--no-prompt-templates" || arg == "-np" {
            result.no_prompt_templates = true;
        } else if arg == "--no-themes" {
            result.no_themes = true;
        } else if arg == "--no-context-files" || arg == "-nc" {
            result.no_context_files = true;
        } else if arg == "--list-models" {
            match args.get(i + 1) {
                Some(next) if !next.starts_with('-') && !next.starts_with('@') => {
                    result.list_models = Some(ListModelsArg::Search(next.clone()));
                    i += 1;
                }
                _ => result.list_models = Some(ListModelsArg::True),
            }
        } else if arg == "--tui-mode" {
            match args.get(i + 1).map(String::as_str) {
                Some("regular") => {
                    result.tui_mode = Some(TuiMode::Regular);
                    i += 1;
                }
                Some("fullscreen") => {
                    result.tui_mode = Some(TuiMode::Fullscreen);
                    i += 1;
                }
                Some(mode) if mode.starts_with('-') => {
                    push_error(&mut result, "--tui-mode requires regular or fullscreen");
                }
                Some(mode) => {
                    i += 1;
                    push_error(
                        &mut result,
                        format!("Invalid TUI mode \"{mode}\". Valid values: regular, fullscreen"),
                    );
                }
                None => {
                    push_error(&mut result, "--tui-mode requires regular or fullscreen");
                }
            }
        } else if arg == "--serve" {
            result.serve = true;
        } else if arg == "--serve-addr" && i + 1 < args.len() {
            i += 1;
            result.serve_addr = Some(args[i].clone());
        } else if arg == "--serve-token" && i + 1 < args.len() {
            i += 1;
            result.serve_token = Some(args[i].clone());
        } else if arg == "--verbose" {
            result.verbose = true;
        } else if arg == "--approve" || arg == "-a" {
            result.project_trust_override = Some(true);
        } else if arg == "--no-approve" || arg == "-na" {
            result.project_trust_override = Some(false);
        } else if arg == "--offline" {
            result.offline = true;
        } else if let Some(file) = arg.strip_prefix('@') {
            result.file_args.push(file.to_string());
        } else if let Some(long) = arg.strip_prefix("--") {
            // 未知长选项可能是扩展注册的 flag：先原样记下，等扩展加载后由下游判定
            // 是否真的未知（因此这里不产生诊断）。
            match long.find('=') {
                Some(eq_index) => {
                    let name = &long[..eq_index];
                    let value = &long[eq_index + 1..];
                    result
                        .unknown_flags
                        .insert(name.to_string(), FlagValue::Str(value.to_string()));
                }
                None => {
                    let next = args.get(i + 1);
                    if let Some(next) = next
                        && !next.starts_with('-')
                        && !next.starts_with('@')
                    {
                        result
                            .unknown_flags
                            .insert(long.to_string(), FlagValue::Str(next.clone()));
                        i += 1;
                    } else {
                        result
                            .unknown_flags
                            .insert(long.to_string(), FlagValue::Bool(true));
                    }
                }
            }
        } else if arg.starts_with('-') {
            // 短选项没有扩展注册机制，无法延后判定，直接记错误。
            push_error(&mut result, format!("Unknown option: {arg}"));
        } else {
            // 其余 token 是位置消息；`@file` 已在上面的分支单独收集。
            result.messages.push(arg.to_string());
        }

        i += 1;
    }

    result
}

// ---------------------------------------------------------------------------
// help
// ---------------------------------------------------------------------------

/// 按 UTF-16 码元长度补空格（JS `String.prototype.padEnd`）。
///
/// 必须按 UTF-16 而不是 Rust 的 `char` 计数：JS 的 `String.length` 统计 UTF-16 码元，
/// 一个 emoji 计 2，CJK 字符计 1；否则含非 BMP 字符的帮助/模型表格会和 TS 的列宽不一致。
/// 长度已达到 `width` 时原样返回（不截断）。`pad_end_utf16` 同时供本文件的帮助文本与
/// `cli/list_models` 的表格对齐使用。
pub(crate) fn pad_end_utf16(text: &str, width: usize) -> String {
    let length = text.encode_utf16().count();
    if length >= width {
        return text.to_string();
    }
    let mut padded = text.to_string();
    padded.push_str(&" ".repeat(width - length));
    padded
}

/// 帮助文本主体（`${...}` 由调用方替换；`{resource}` 等业务花括号保持原样）。
const HELP_TEMPLATE: &str = r##"{bold_open}{app}{bold_close} - AI coding assistant with read, bash, edit, write tools

{bold_open}Usage:{bold_close}
  {app} [options] [--] [@files...] [messages...]

{bold_open}Commands:{bold_close}
  {app} install <source> [-l]     Install extension source and add to settings
  {app} remove <source> [-l]      Remove extension source from settings
  {app} uninstall <source> [-l]   Alias for remove
  {app} update [source|self|kiana]   Update kiana, extensions, or model catalogs
  {app} list                      List installed extensions from settings
  {app} config [-l]               Open TUI to enable/disable package resources (Tab switches scope)
  {app} auth <command>            Print credentials or check provider readiness
  {app} mcp <command>             Manage MCP servers
  {app} <command> --help          Show help for install/remove/uninstall/update/list/config/auth/mcp

{bold_open}Options:{bold_close}
  --provider <name>              Provider to search for --model (requires --model)
  --model <pattern>              Model pattern or ID (supports "provider/id" and optional ":<thinking>")
  --api-key <key>                API key (defaults to env vars)
  --system-prompt <text>         System prompt (default: coding assistant prompt)
  --append-system-prompt <text>  Append text or file contents to the system prompt (can be used multiple times)
  --mode <mode>                  Output mode: text (default), json, or rpc
  --print, -p                    Non-interactive mode: process prompt and exit
  --continue, -c                 Continue previous session
  --resume, -r                   Select a session to resume
  --session <path|id>            Use specific session file or partial UUID
  --session-id <id>              Use exact project session ID, creating it if missing
  --fork <path|id>               Fork specific session file or partial UUID into a new session
  --session-dir <dir>            Directory for session storage and lookup
  --no-session                   Don't save session (ephemeral)
  --name, -n <name>              Set session display name
  --models <patterns>            Comma-separated model patterns for Ctrl+P cycling
                                 Supports globs (anthropic/*, *sonnet*) and fuzzy matching
  --no-tools, -nt                Disable all tools by default (built-in and extension)
  --no-builtin-tools, -nbt       Disable built-in tools by default but keep extension/custom tools enabled
  --tools, -t <tools>            Comma-separated allowlist of tool names or patterns (*) to enable
                                 Keeps MCP tools unless an entry starts with mcp__
                                 Only +name/-name entries add to or remove from the defaults
  --exclude-tools, -xt <tools>   Comma-separated denylist of tool names or patterns (*) to disable
                                 Applies to all tools, MCP tools included
  --permission-mode <mode>       Permission mode: read-only, default, accept-edits, or bypass
  --allow <rule>                 Allow a tool/command/path rule (can be used multiple times)
  --deny <rule>                  Deny a tool/command/path rule (can be used multiple times)
  --add-dir <dir>                Add a writable workspace root (can be used multiple times)
  --thinking <level>             Set thinking level: off, minimal, low, medium, high, xhigh, max
  --extension, -e <path>         Load an extension file or builtin:<name> (can be used multiple times)
  --no-extensions, -ne           Disable extension discovery and built-in extensions (explicit -e paths still work)
  --no-mcp                       Disable built-in MCP support: no servers connect and no MCP tools
  --skill <path>                 Load a skill file or directory (can be used multiple times)
  --no-skills, -ns               Disable skills discovery and loading
  --prompt-template <path>       Load a prompt template file or directory (can be used multiple times)
  --no-prompt-templates, -np     Disable prompt template discovery and loading
  --theme <path>                 Load a theme file or directory (can be used multiple times)
  --use-theme <name[/name]>      Set the initial interactive theme for this run
  --no-themes                    Disable theme discovery and loading
  --no-context-files, -nc        Disable AGENTS.md and CLAUDE.md discovery and loading
  --export <file> [output]       Export session file to HTML and exit
  --list-models [search]         List available models (with optional fuzzy search)
  --verbose                      Force verbose startup (overrides quietStartup setting)
  --tui-mode <mode>              TUI mode: fullscreen (default) or regular
  --serve                        Run the daemon event service (SSE/WebSocket/REST) and exit on SIGINT
  --serve-addr <host:port>       Daemon listen address (default 127.0.0.1:0)
  --serve-token <token>          Override the auto-generated daemon bearer token
  --approve, -a                  Trust project-local files for this run
  --no-approve, -na              Ignore project-local files for this run
  --offline                      Disable startup network operations (same as KIANA_OFFLINE=1)
  --                             End option parsing; treat remaining arguments as messages/files
  --help, -h                     Show this help
  --version, -v                  Show version number

Extensions can register additional flags (e.g., --plan from plan-mode extension).{extension_flags_text}

{bold_open}Examples:{bold_close}
  # Print a provider API key for an external client
  {app} auth print-api-key --provider openai

  # Print an OAuth bearer token for an external client (refreshes if expired)
  {app} auth print-bearer-token --provider openai-codex

  # Interactive mode
  {app}

  # Interactive mode with initial prompt
  {app} "List all .ts files in src/"

  # Include files in initial message
  {app} @prompt.md @image.png "What color is the sky?"

  # Non-interactive mode (process and exit)
  {app} -p "List all .ts files in src/"

  # Prompt beginning with a dash
  {app} -p -- "- Summarize these points"

  # Multiple messages (interactive)
  {app} "Read package.json" "What dependencies do we have?"

  # Continue previous session
  {app} --continue "What did we discuss?"

  # Start a named session
  {app} --name "Refactor auth module"

  # Use different model
  {app} --provider openai --model gpt-4o-mini "Help me refactor this code"

  # Use model with provider prefix (no --provider needed)
  {app} --model openai/gpt-4o "Help me refactor this code"

  # Use model with thinking level shorthand
  {app} --model sonnet:high "Solve this complex problem"

  # Limit model cycling to specific models
  {app} --models claude-sonnet,claude-haiku,gpt-4o

  # Limit to a specific provider with glob pattern
  {app} --models "github-copilot/*"

  # Cycle models with fixed thinking levels
  {app} --models sonnet:high,haiku:low

  # Start with a specific thinking level
  {app} --thinking high "Solve this complex problem"

  # Read-only mode (no file modifications possible)
  {app} --tools read,grep,find,ls -p "Review the code in src/"

  # Add codemode to the default tools
  {app} --tools +codemode

  # Codemode with only the tools of one MCP server
  {app} --tools read,bash,codemode,'mcp__radius__*'

  # Disable one tool while keeping the rest available
  {app} --exclude-tools ask_question

  # Export a session file to HTML
  {app} --export ~/{config_dir}/agent/sessions/--path--/session.jsonl
  {app} --export session.jsonl output.html

{bold_open}Environment Variables:{bold_close}
  ANTHROPIC_AUTH_TOKEN             - Anthropic bearer auth token
  ANTHROPIC_API_KEY                - Anthropic Claude API key
  ANTHROPIC_OAUTH_TOKEN            - Anthropic OAuth token (alternative to API key)
  ANT_LING_API_KEY                 - Ant Ling API key
  OPENAI_API_KEY                   - OpenAI GPT API key
  AZURE_OPENAI_API_KEY             - Azure OpenAI API key
  AZURE_OPENAI_BASE_URL            - Azure OpenAI/Cognitive Services base URL (e.g. https://{resource}.openai.azure.com)
  AZURE_OPENAI_RESOURCE_NAME       - Azure OpenAI resource name (alternative to base URL)
  AZURE_OPENAI_API_VERSION         - Azure OpenAI API version (default: v1)
  AZURE_OPENAI_DEPLOYMENT_NAME_MAP - Azure OpenAI model=deployment map (comma-separated)
  DEEPSEEK_API_KEY                 - DeepSeek API key
  NVIDIA_API_KEY                   - NVIDIA NIM API key
  GEMINI_API_KEY                   - Google Gemini API key
  GROQ_API_KEY                     - Groq API key
  CEREBRAS_API_KEY                 - Cerebras API key
  XAI_API_KEY                      - xAI Grok API key
  FIREWORKS_API_KEY                - Fireworks API key
  TOGETHER_API_KEY                 - Together AI API key
  BASETEN_API_KEY                  - Baseten API key
  OPENROUTER_API_KEY               - OpenRouter API key
  AI_GATEWAY_API_KEY               - Vercel AI Gateway API key
  ZAI_API_KEY                      - ZAI Coding Plan API key (Global)
  ZAI_CODING_CN_API_KEY            - ZAI Coding Plan API key (China)
  MISTRAL_API_KEY                  - Mistral API key
  MINIMAX_API_KEY                  - MiniMax API key
  MOONSHOT_API_KEY                 - Moonshot AI API key
  OPENCODE_API_KEY                 - OpenCode Zen/OpenCode Go API key
  KIMI_API_KEY                     - Kimi For Coding API key
  META_API_KEY                     - Meta Model API key
  CLOUDFLARE_API_KEY               - Cloudflare API token (Workers AI and AI Gateway)
  CLOUDFLARE_ACCOUNT_ID            - Cloudflare account id (required for both)
  CLOUDFLARE_GATEWAY_ID            - Cloudflare AI Gateway slug (required for AI Gateway)
  QWEN_TOKEN_PLAN_API_KEY          - Qwen Token Plan API key (international region)
  QWEN_TOKEN_PLAN_CN_API_KEY       - Qwen Token Plan API key (China region)
  XIAOMI_API_KEY                   - Xiaomi MiMo API key (api.xiaomimimo.com billing)
  XIAOMI_TOKEN_PLAN_CN_API_KEY     - Xiaomi MiMo Token Plan API key (China region)
  XIAOMI_TOKEN_PLAN_AMS_API_KEY    - Xiaomi MiMo Token Plan API key (Amsterdam region)
  XIAOMI_TOKEN_PLAN_SGP_API_KEY    - Xiaomi MiMo Token Plan API key (Singapore region)
  AWS_PROFILE                      - AWS profile for Amazon Bedrock
  AWS_ACCESS_KEY_ID                - AWS access key for Amazon Bedrock
  AWS_SECRET_ACCESS_KEY            - AWS secret key for Amazon Bedrock
  AWS_BEARER_TOKEN_BEDROCK         - Bedrock API key (bearer token)
  AWS_REGION                       - AWS region for Amazon Bedrock (e.g., us-east-1)
  {env_agent_dir} - Config directory (default: ~/{config_dir}/agent)
  {env_session_dir} - Session storage directory (overridden by --session-dir)
  KIANA_API_BASE_URL                  - Base URL for Kiana-hosted services (no default; related network features are unavailable when unset)
  KIANA_INSTALLER_API_BASE            - Managed update release API (default: KIANA_API_BASE_URL + /api/installer/releases)
  KIANA_PACKAGE_DIR                   - Override package directory (for Nix/Guix store paths)
  KIANA_OFFLINE                       - Disable startup network operations when set to 1/true/yes
  KIANA_TELEMETRY                     - Override install telemetry when set to 1/true/yes or 0/false/no
  KIANA_SHARE_VIEWER_URL              - Base URL for /share viewer links (default: KIANA_API_BASE_URL + /session/)

{bold_open}Built-in Tool Names:{bold_close}
  read       - Read file contents
  bash       - Execute bash commands
  powershell - Execute PowerShell commands on Windows
  edit       - Edit files with find/replace
  write      - Write files (creates/overwrites)
  grep       - Search file contents (read-only, off by default)
  find       - Find files by glob pattern (read-only, off by default)
  ls         - List directory contents (read-only, off by default)
"##;

/// 构造扩展段文本（`extensionFlags && extensionFlags.length > 0` 时非空）。
///
/// 每行：两个空格 + `--<name>`（`FlagType::String` 追加 ` <value>`），按 UTF-16 长度补到
/// 30 列，再接描述；描述缺省为 `Registered by <extension_path>`。返回串自带前导与末尾
/// 换行（`\n<title>\n<lines>\n`），插入 `HELP_TEMPLATE` 后正好与前后段落隔开。
/// `None` 与空切片都返回空串（`tests/args.rs` 断言两者输出完全相同）。
fn extension_flags_text(extension_flags: Option<&[ExtensionFlag]>, style: Style) -> String {
    let Some(flags) = extension_flags.filter(|flags| !flags.is_empty()) else {
        return String::new();
    };
    let lines: Vec<String> = flags
        .iter()
        .map(|flag| {
            let value = if flag.r#type == FlagType::String {
                " <value>"
            } else {
                ""
            };
            let description = flag
                .description
                .clone()
                .unwrap_or_else(|| format!("Registered by {}", flag.extension_path));
            format!(
                "{}{}",
                pad_end_utf16(&format!("  --{}{}", flag.name, value), 30),
                description
            )
        })
        .collect();
    format!(
        "\n{}\n{}\n",
        style.bold("Extension CLI Flags:"),
        lines.join("\n")
    )
}

/// `printHelp(extensionFlags?)` 的纯文本版本；返回值包含 `console.log` 追加的最后一个换行。
///
/// 空切片与 `None` 输出完全相同（Audit §A.3）。
pub fn help_text(extension_flags: Option<&[ExtensionFlag]>) -> String {
    help_text_with_style(extension_flags, Style::detect())
}

/// 指定样式的 [`help_text`]（测试用 `Style::plain()` 消除 TTY 差异）。
///
/// 替换顺序固定：先 `{bold_open}`/`{bold_close}`，再 `{app}`/`{config_dir}`/环境变量名，
/// 最后插入 `{extension_flags_text}`。扩展描述是最后注入的，所以描述文本里即使出现
/// `{app}` 之类的字面量也不会被二次替换（与 TS 先做模板替换、再拼接扩展段的顺序一致）。
/// 模板里的 `{resource}` 等业务花括号没有同名占位符，保持原样。
pub fn help_text_with_style(extension_flags: Option<&[ExtensionFlag]>, style: Style) -> String {
    let bold_open = if style.enabled() { "\u{1b}[1m" } else { "" };
    let bold_close = if style.enabled() { "\u{1b}[22m" } else { "" };
    let body = HELP_TEMPLATE
        .replace("{bold_open}", bold_open)
        .replace("{bold_close}", bold_close)
        .replace("{app}", kiana_core::config::APP_NAME)
        .replace("{config_dir}", kiana_core::config::CONFIG_DIR_NAME)
        .replace(
            "{env_agent_dir}",
            &pad_end_utf16(kiana_core::config::ENV_AGENT_DIR, 35),
        )
        .replace(
            "{env_session_dir}",
            &pad_end_utf16(kiana_core::config::ENV_SESSION_DIR, 35),
        )
        .replace(
            "{extension_flags_text}",
            &extension_flags_text(extension_flags, style),
        );
    // console.log 在模板末尾追加一个换行（模板本身以换行结尾 → 最终是两个换行）。
    format!("{body}\n")
}

/// `printHelp(extensionFlags?)`：经进程 [`Output`] 写出帮助（单次 `console.log`）。
///
/// 签名与 spec/ca-cli.md §A.3 冻结契约一致（`Option<&[ExtensionFlag]>`）；需要捕获输出
/// 的调用方用 [`print_help_with_output`]。本函数内部固定使用 [`Output::process`]（真实
/// stdout；被 `output_guard` 接管时写 stderr），不会使用调用方自己的 `Output` 句柄。
pub fn print_help(extension_flags: Option<&[ExtensionFlag]>) {
    print_help_with_output(extension_flags, &Output::process());
}

/// 指定 [`Output`] 的 [`print_help`]（测试/嵌入调用方使用）。
pub fn print_help_with_output(extension_flags: Option<&[ExtensionFlag]>, output: &Output) {
    print_help_with_style(extension_flags, Style::detect(), output);
}

/// 指定 [`Style`] 与 [`Output`] 的 [`print_help`]：测试可用 `Style::plain()` 消除进程
/// 环境（TTY / `FORCE_COLOR`）对 golden 比较的影响。
pub fn print_help_with_style(
    extension_flags: Option<&[ExtensionFlag]>,
    style: Style,
    output: &Output,
) {
    output.write_stdout(&help_text_with_style(extension_flags, style));
}
