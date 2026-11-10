//! 权限策略与决策引擎。

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, RwLock};

use crate::command;
use crate::mode::PermissionMode;
use crate::request::{ActionKind, PermissionRequest, path_is_inside};

/// 单条规则的决策。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RuleDecision {
    /// 放行。
    Allow,
    /// 询问。
    Ask,
    /// 拒绝。
    Deny,
}

impl RuleDecision {
    /// 稳定字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Ask => "ask",
            Self::Deny => "deny",
        }
    }
}

/// 一条权限规则。
///
/// 规则按 `Deny` → `Ask` → `Allow` 的顺序匹配，显式拒绝永远优先。`CommandPrefix`
/// 只对简单命令生效（见 [`crate::command`]）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionRule {
    /// 按工具名匹配；支持开头/结尾的 `*`（`mcp__*`、`*__read`、`*`）。
    Tool {
        /// 工具名模式。
        pattern: String,
        /// 决策。
        decision: RuleDecision,
        /// 展示给用户/模型的原因。
        reason: Option<String>,
    },
    /// 按命令前缀匹配；只对简单命令生效。
    CommandPrefix {
        /// 命令前缀词。
        tokens: Vec<String>,
        /// 决策。
        decision: RuleDecision,
        /// 原因。
        reason: Option<String>,
    },
    /// 按路径前缀匹配。
    PathPrefix {
        /// 路径前缀。
        prefix: PathBuf,
        /// 仅对这些动作生效。`None` 表示任意动作（CLI/settings 显式配置的路径规则）；
        /// 审批生成的规则只作用于当初批准的动作，避免「允许写这个文件」连带放行
        /// shell/MCP 对该路径的访问。
        actions: Option<Vec<ActionKind>>,
        /// 决策。
        decision: RuleDecision,
        /// 原因。
        reason: Option<String>,
    },
    /// 按完整命令精确匹配。
    ///
    /// 复合命令（`echo a; rm -rf ./build`）不能安全地退化成前缀规则，所以「总是允许」
    /// 会生成一条精确命令规则。匹配使用调用方传入的原始命令文本（含
    /// `shellCommandPrefix`），并要求两侧 `trim` 后完全相等。
    ExactCommand {
        /// 完整命令文本。
        command: String,
        /// 决策。
        decision: RuleDecision,
        /// 原因。
        reason: Option<String>,
    },
    /// 按命令通配模式匹配（`*` 匹配任意字符序列）。
    ///
    /// 与 [`Self::CommandPrefix`] 的区别：通配规则显式声明了通配位置，例如
    /// `git status*` 只匹配以该前缀开头的命令，`git *` 只匹配 `git` 及其参数。
    /// Allow 同样只对**单一简单命令**生效；复合命令里的任一段只用于 Ask/Deny。
    CommandWildcard {
        /// 通配模式（含 `*`）。
        pattern: String,
        /// 决策。
        decision: RuleDecision,
        /// 原因。
        reason: Option<String>,
    },
    /// 用户通过「总是允许」显式批准生成的规则。
    ///
    /// 包装的规则保持原有匹配语义（[`Self::Tool`]/[`Self::CommandPrefix`]/...），但：
    ///
    /// - `builtin` 记录生成规则时的工具来源，匹配时要求请求来源相同。扩展/SDK 工具可以
    ///   覆盖内建名（自定义 `edit`），没有这个约束时一条 `Tool{"edit", Allow}` 会在之后
    ///   放行真正的内建 `edit`，在 read-only 模式下也一样；
    /// - 引擎在 `Ask` 规则与 shell 分析检查**之前**应用它：用户已经对同一规则明确选择
    ///   「总是允许」，之后不能因为配置里的 `Ask` 规则再次询问。`Deny` 规则、灾难性
    ///   命令检查与 `.git`/`.kiana` 受保护目录检查仍然先执行。
    Approved {
        /// 包装的底层规则。
        rule: Box<PermissionRule>,
        /// 生成规则时的工具来源（`true` = 内建工具）。
        builtin: bool,
    },
}

impl PermissionRule {
    /// 工具名规则。
    pub fn tool(pattern: impl Into<String>, decision: RuleDecision) -> Self {
        Self::Tool {
            pattern: pattern.into(),
            decision,
            reason: None,
        }
    }

    /// 命令前缀规则。
    pub fn command(tokens: Vec<String>, decision: RuleDecision) -> Self {
        Self::CommandPrefix {
            tokens,
            decision,
            reason: None,
        }
    }

    /// 路径前缀规则。
    pub fn path(prefix: impl Into<PathBuf>, decision: RuleDecision) -> Self {
        Self::PathPrefix {
            prefix: prefix.into(),
            actions: None,
            decision,
            reason: None,
        }
    }

    /// 完整命令规则。
    pub fn exact_command(command: impl Into<String>, decision: RuleDecision) -> Self {
        Self::ExactCommand {
            command: command.into(),
            decision,
            reason: None,
        }
    }

    /// 命令通配规则（`*` 匹配任意字符序列）。
    pub fn wildcard_command(pattern: impl Into<String>, decision: RuleDecision) -> Self {
        Self::CommandWildcard {
            pattern: pattern.into(),
            decision,
            reason: None,
        }
    }

    /// 补充原因。
    pub fn with_reason(self, reason: impl Into<String>) -> Self {
        let reason = reason.into();
        match self {
            Self::Approved { rule, builtin } => Self::Approved {
                rule: Box::new(rule.with_reason(reason)),
                builtin,
            },
            mut rule => {
                match &mut rule {
                    Self::Tool { reason: slot, .. }
                    | Self::CommandPrefix { reason: slot, .. }
                    | Self::PathPrefix { reason: slot, .. }
                    | Self::ExactCommand { reason: slot, .. }
                    | Self::CommandWildcard { reason: slot, .. } => *slot = Some(reason),
                    Self::Approved { .. } => unreachable!("handled above"),
                }
                rule
            }
        }
    }

    /// 规则的决策。
    pub fn decision(&self) -> RuleDecision {
        match self {
            Self::Tool { decision, .. }
            | Self::CommandPrefix { decision, .. }
            | Self::PathPrefix { decision, .. }
            | Self::ExactCommand { decision, .. }
            | Self::CommandWildcard { decision, .. } => *decision,
            Self::Approved { rule, .. } => rule.decision(),
        }
    }

    /// 是否是「总是允许」审批生成的规则。
    pub fn is_approved(&self) -> bool {
        matches!(self, Self::Approved { .. })
    }

    /// 规则的 JSON 形态（写回 settings 用）。    ///
    /// 键序固定为 `tool`/`command`/`path`、`decision`、`reason`、`exact`，与
    /// [`rule_from_settings_value`] 互逆。
    pub fn to_settings_value(&self) -> serde_json::Value {
        let reason = self.reason().map(str::to_string);
        match self {
            Self::Tool {
                pattern, decision, ..
            } => {
                let mut value = serde_json::json!({
                    "tool": pattern,
                    "decision": decision.as_str(),
                });
                if let Some(reason) = reason {
                    value["reason"] = serde_json::Value::String(reason);
                }
                value
            }
            Self::CommandPrefix {
                tokens, decision, ..
            } => {
                let mut value = serde_json::json!({
                    "command": tokens,
                    "decision": decision.as_str(),
                });
                if let Some(reason) = reason {
                    value["reason"] = serde_json::Value::String(reason);
                }
                value
            }
            Self::PathPrefix {
                prefix,
                actions,
                decision,
                ..
            } => {
                let mut value = serde_json::json!({
                    "path": prefix,
                    "decision": decision.as_str(),
                });
                if let Some(actions) = actions {
                    value["actions"] = serde_json::to_value(actions).unwrap_or_default();
                }
                if let Some(reason) = reason {
                    value["reason"] = serde_json::Value::String(reason);
                }
                value
            }
            Self::ExactCommand {
                command, decision, ..
            } => {
                let mut value = serde_json::json!({
                    "command": command,
                    "exact": true,
                    "decision": decision.as_str(),
                });
                if let Some(reason) = reason {
                    value["reason"] = serde_json::Value::String(reason);
                }
                value
            }
            Self::CommandWildcard {
                pattern, decision, ..
            } => {
                let mut value = serde_json::json!({
                    "command": pattern,
                    "wildcard": true,
                    "decision": decision.as_str(),
                });
                if let Some(reason) = reason {
                    value["reason"] = serde_json::Value::String(reason);
                }
                value
            }
            Self::Approved { rule, builtin } => {
                let mut value = rule.to_settings_value();
                if let Some(object) = value.as_object_mut() {
                    object.insert("approved".to_string(), serde_json::Value::Bool(true));
                    object.insert(
                        "origin".to_string(),
                        serde_json::Value::String(
                            if *builtin { "builtin" } else { "custom" }.into(),
                        ),
                    );
                }
                value
            }
        }
    }

    /// Tool 规则是否匹配一个虚拟操作的工具名（`read_file` 等）。
    ///
    /// 只用于 Deny/Ask：virtual operations 是 shell 的等价操作，允许它们走 Tool 的
    /// Allow 规则会扩大权限，因此 Allow 仍只按真实工具名匹配。
    ///
    /// 虚拟操作名来自参考设计的 `read_file`/`write_file`/...；Kiana 的内建工具实际
    /// 叫 `read`/`write`/`ls`，所以 Deny/Ask 同时接受这两套名字（`edit` 是写工具的
    /// 同义名）。否则“拒绝 read 工具”不会约束 `cat file`。
    fn matches_virtual_tool(&self, tool_name: &str) -> bool {
        match self {
            Self::Tool { pattern, .. } => {
                tool_matches(pattern, tool_name)
                    || virtual_tool_aliases(tool_name)
                        .iter()
                        .any(|alias| tool_matches(pattern, alias))
            }
            _ => false,
        }
    }

    /// 这条规则是否命中一次请求。
    ///
    /// 规则匹配是纯函数，供引擎与需要预览规则效果的调用方（UI/测试）使用。
    /// Allow 型命令规则只对单一简单命令生效；Ask/Deny 会扫描复合命令的每一段。
    pub fn matches(&self, request: &PermissionRequest) -> bool {
        match self {
            Self::Tool { pattern, .. } => tool_matches(pattern, &request.tool_name),
            Self::CommandPrefix {
                tokens, decision, ..
            } => {
                if request.action != ActionKind::Execute {
                    return false;
                }
                match decision {
                    // Allow 只对真正的简单命令生效，且**不剥包装器**：
                    // `npm test` 的 Allow 不应连带放行 `sudo npm test`（提权变体）。
                    // 分析不完整（嵌套超限）时路径/命令集合不可信，Allow 一律失效。
                    RuleDecision::Allow => {
                        !request.analysis_incomplete
                            && request.simple_command
                            && single_simple_command_words(request).is_some_and(|words| {
                                command::command_starts_with_raw(words, tokens)
                            })
                    }
                    // Ask/Deny 保守处理：复合命令里任一段命中就按规则处理（同时比较原始词
                    // 与剥掉 `sudo`/`env` 等包装后的词），否则 `echo hi; rm -rf ~` 或
                    // `sudo rm -rf ~` 会绕过规则。PowerShell 用同一方言重新分段：
                    // POSIX 会把 `\;` 当成转义，漏掉它后面的第二条语句。
                    RuleDecision::Ask | RuleDecision::Deny => {
                        command::any_segment_starts_with_kind(
                            request.command.as_deref().unwrap_or_default(),
                            tokens,
                            request_shell_kind(request),
                        )
                    }
                }
            }
            Self::PathPrefix {
                prefix,
                actions,
                decision,
                ..
            } => {
                if let Some(actions) = actions
                    && !actions.contains(&request.action)
                {
                    return false;
                }
                if prefix.as_os_str().is_empty() {
                    // 空前缀是配置错误（`--allow path:`）：Allow 绝不能匹配一切；
                    // Deny/Ask 保持保守（匹配）。
                    return !matches!(decision, RuleDecision::Allow);
                }
                let inside =
                    |path: &std::path::PathBuf| path_is_inside(path, std::slice::from_ref(prefix));
                match decision {
                    // Allow 必须覆盖全部**写入**路径：`touch /ws/ok && echo x > /root/.bashrc`
                    // 里只要有一个路径在外部，就不能整条命令放行。读取路径不参与
                    // （工作区写策略通常允许读区外文件）。
                    RuleDecision::Allow => {
                        !request.analysis_incomplete
                            && !request.paths.is_empty()
                            && request.paths.iter().all(inside)
                    }
                    // Ask/Deny 保守处理：任一读取或写入路径命中就按规则处理。
                    // 分析不完整时路径集合不可信（`cd /x && echo > rel` 里的目标
                    // 并不是会话 cwd 下的 `rel`），无法证明「没有命中」，只能按
                    // 命中处理（fail-closed）。
                    RuleDecision::Ask | RuleDecision::Deny => {
                        request.analysis_incomplete
                            || request.paths.iter().any(inside)
                            || request.read_paths.iter().any(inside)
                    }
                }
            }
            Self::ExactCommand { command, .. } => {
                request.action == ActionKind::Execute
                    && request
                        .command
                        .as_deref()
                        .map(str::trim)
                        .is_some_and(|actual| actual == command.trim())
            }
            Self::CommandWildcard {
                pattern, decision, ..
            } => {
                if request.action != ActionKind::Execute {
                    return false;
                }
                match decision {
                    // Allow：只对单一简单命令生效；复合命令里的任一段不能借用通配规则。
                    RuleDecision::Allow => {
                        !request.analysis_incomplete
                            && request.simple_command
                            && request
                                .shell
                                .as_ref()
                                .map_or(true, |shell| shell.is_simple())
                            && request.command.as_deref().is_some_and(|command| {
                                crate::shell::command_pattern_matches(pattern, command)
                            })
                    }
                    // Ask/Deny：复合命令/包装器/替换里的任一段命中就按规则处理。
                    RuleDecision::Ask | RuleDecision::Deny => {
                        command::any_segment_matches_wildcard_kind(
                            request.command.as_deref().unwrap_or_default(),
                            pattern,
                            request_shell_kind(request),
                        )
                    }
                }
            }
            // 审批规则只在生成它的工具来源上生效：同名自定义工具不能借用内建工具的批准
            // （反之亦然）。来源不一致时规则完全不匹配，Deny/Ask 也不会被错误收窄。
            Self::Approved { rule, builtin } => {
                request.builtin == *builtin && rule.matches(request)
            }
        }
    }

    fn reason(&self) -> Option<&str> {
        match self {
            Self::Tool { reason, .. }
            | Self::CommandPrefix { reason, .. }
            | Self::PathPrefix { reason, .. }
            | Self::ExactCommand { reason, .. }
            | Self::CommandWildcard { reason, .. } => reason.as_deref(),
            Self::Approved { rule, .. } => rule.reason(),
        }
    }
}

/// 请求使用的 shell 方言；手工构造（没有 shell 分析）的请求按 POSIX 处理。
fn request_shell_kind(request: &PermissionRequest) -> crate::shell::ShellKind {
    request
        .shell
        .as_ref()
        .map(|shell| shell.kind)
        .unwrap_or_default()
}

/// Allow 前缀规则可用的词序列：单一简单命令的词。
fn single_simple_command_words(request: &PermissionRequest) -> Option<&[String]> {
    match &request.shell {
        Some(shell) if shell.is_simple() => shell
            .subcommands
            .first()
            .map(|subcommand| subcommand.words.as_slice()),
        Some(_) => None,
        // 手工构造的请求没有 shell 分析时退回旧字段。
        None => request
            .simple_command
            .then_some(request.command_tokens.as_slice()),
    }
}

/// 虚拟操作对应的 Kiana 内建工具名（Deny/Ask 匹配用）。
fn virtual_tool_aliases(tool_name: &str) -> &'static [&'static str] {
    match tool_name {
        "read_file" => &["read"],
        "write_file" => &["write", "edit"],
        "list_directory" => &["ls"],
        _ => &[],
    }
}

/// 用一个决策类别去匹配 shell 的 virtual operations（只用于 Deny/Ask）。
fn virtual_operation_rule(
    policy: &PermissionPolicy,
    request: &PermissionRequest,
    decision: RuleDecision,
) -> Option<String> {
    let shell = request.shell.as_ref()?;
    for operation in shell.virtual_operations(&request.cwd) {
        let tool_name = operation.tool_name();
        if let Some(rule) = policy
            .rules
            .iter()
            .find(|rule| rule.decision() == decision && rule.matches_virtual_tool(tool_name))
        {
            return Some(rule.reason().map_or_else(
                || {
                    format!(
                        "Virtual operation '{tool_name}' from shell command matched a {} rule for tool '{}'.",
                        decision.as_str(),
                        request.tool_name
                    )
                },
                str::to_string,
            ));
        }
    }
    None
}

/// 权限策略。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionPolicy {
    /// 当前模式。
    pub mode: PermissionMode,
    /// 工作区根；`accept-edits` 用它们判断“区内写”。
    pub workspace_roots: Vec<PathBuf>,
    /// 规则。
    pub rules: Vec<PermissionRule>,
}

impl PermissionPolicy {
    /// 用单个工作区根构造。
    pub fn new(mode: PermissionMode, cwd: impl Into<PathBuf>) -> Self {
        Self {
            mode,
            workspace_roots: vec![cwd.into()],
            rules: Vec::new(),
        }
    }

    /// 替换工作区根。
    pub fn with_workspace_roots(mut self, roots: Vec<PathBuf>) -> Self {
        self.workspace_roots = roots;
        self
    }

    /// 追加规则。
    pub fn with_rule(mut self, rule: PermissionRule) -> Self {
        self.rules.push(rule);
        self
    }

    /// 批量追加规则。
    pub fn with_rules(mut self, rules: impl IntoIterator<Item = PermissionRule>) -> Self {
        self.rules.extend(rules);
        self
    }
}

/// 单次调用的权限结论。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionDecision {
    /// 允许。
    Allow {
        /// 原因（规则/模式）。
        reason: String,
    },
    /// 需要询问用户。
    Ask {
        /// 给确认对话框的说明（可以包含完整命令/路径，只用于 UI）。
        prompt: String,
        /// 决策原因（模式/规则/安全策略）；用于审计事件，不含完整参数。
        reason: String,
    },
    /// 拒绝。
    Deny {
        /// 给模型/用户的拒绝原因。
        reason: String,
    },
}

impl PermissionDecision {
    /// 是否允许。
    pub fn is_allow(&self) -> bool {
        matches!(self, Self::Allow { .. })
    }

    /// 是否需要询问。
    pub fn is_ask(&self) -> bool {
        matches!(self, Self::Ask { .. })
    }

    /// 是否拒绝。
    pub fn is_deny(&self) -> bool {
        matches!(self, Self::Deny { .. })
    }

    /// 原因/提示文本。
    pub fn text(&self) -> &str {
        match self {
            Self::Allow { reason } | Self::Deny { reason } => reason,
            Self::Ask { prompt, .. } => prompt,
        }
    }

    /// 决策原因（`Ask` 不含完整参数，用于事件/审计）。
    pub fn reason(&self) -> &str {
        match self {
            Self::Allow { reason } | Self::Deny { reason } => reason,
            Self::Ask { reason, .. } => reason,
        }
    }
}

/// 命令行传入的权限覆盖项。
///
/// 与 settings 里的 `permissions` 对象一起交给
/// [`PermissionEngine::from_settings_and_flags`]。优先级：命令行 > settings > 环境变量；
/// `Deny` 规则永远优先于 `Allow`，与运行时一致。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PermissionFlags {
    /// `--permission-mode`。
    pub mode: Option<PermissionMode>,
    /// `--allow`，每条是 [`parse_cli_rule`] 接受的规则文本。
    pub allow: Vec<String>,
    /// `--deny`，每条是 [`parse_cli_rule`] 接受的规则文本。
    pub deny: Vec<String>,
    /// `--add-dir`，追加到可写工作区根。
    pub add_dirs: Vec<PathBuf>,
}

/// 权限引擎：策略 + 本会话已批准的 key。
#[derive(Debug)]
pub struct PermissionEngine {
    policy: RwLock<PermissionPolicy>,
    session_allow: Mutex<HashSet<String>>,
    /// managed policy 的 `disableBypassPermissionsMode`：为真时任何路径都不能进入
    /// `bypass`（settings、`--permission-mode bypass`、运行期 `set_mode`）。
    bypass_disabled: bool,
}

impl PermissionEngine {
    /// 用策略构造。
    pub fn new(mut policy: PermissionPolicy) -> Self {
        Self::normalize_rule_paths(&mut policy);
        Self {
            policy: RwLock::new(policy),
            session_allow: Mutex::new(HashSet::new()),
            bypass_disabled: false,
        }
    }

    /// 规则前缀与工作区根统一按工作区根解析 `~`/`$HOME`/相对路径。
    fn normalize_rule_paths(policy: &mut PermissionPolicy) {
        let base = policy
            .workspace_roots
            .first()
            .cloned()
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_default();
        for root in &mut policy.workspace_roots {
            *root = crate::request::resolve_rule_path(&base, root);
        }
        for rule in &mut policy.rules {
            if let PermissionRule::PathPrefix { prefix, .. } = rule {
                *prefix = crate::request::resolve_rule_path(&base, prefix);
            }
        }
    }

    /// 从 settings、命令行 flags 与环境变量构造。
    ///
    /// `settings` 是 `settings.json` 里的 `permissions` 对象（工厂最外层，识别
    /// `mode`/`rules`/`allow`/`deny`/`addDirs`/`workspaceRoots` 等字段）；`flags` 是
    /// 命令行覆盖。
    ///
    /// `disableBypassPermissionsMode`（camel/snake，`true` 或 `"disable"`）为真时，
    /// 引擎拒绝进入 `bypass`：settings 里的 `mode: "bypass"` 被降级为 `default`，
    /// `--permission-mode bypass` 与运行期 [`PermissionEngine::set_mode`] 同样被忽略。
    ///
    /// 返回 `None` 表示显式关闭权限检查（settings 或环境变量写 `off`），此时调用方应
    /// 保持与引入权限层之前相同的行为。
    ///
    /// 例外：`disableBypassPermissionsMode` 为真时，同层的 `mode: "off"` 被解释为
    /// `default`（fail-closed），仍然创建引擎。managed policy 必须压过用户/项目层的
    /// 关闭请求，否则 managed deny/allow 规则不会被执行。
    pub fn from_settings_and_flags(
        cwd: impl AsRef<Path>,
        settings: Option<&serde_json::Value>,
        flags: &PermissionFlags,
    ) -> Option<Self> {
        let cwd = cwd.as_ref();
        // managed policy 把 `disableBypassPermissionsMode` 强制写进有效 `permissions`
        // 对象后，由 `from_settings_and_flags` 统一转成引擎标志。
        //
        // 该键存在时 `mode: "off"` **不能**整体关闭权限层：managed policy 的 deny/allow
        // 规则与 bypass 降级都必须继续生效（`settings_engine` 把 off 解释为 default）。
        // 否则用户层（或受信项目层）只要写一个 `mode: "off"` 就能绕过 managed 策略。
        let bypass_disabled = settings.is_some_and(settings_disable_bypass);
        let engine = match Self::settings_engine(cwd, settings) {
            // settings 明确关闭：只有命令行显式给出模式时才重新启用（命令行是更高的层）。
            Some(None) => flags
                .mode
                .map(|mode| Self::new(PermissionPolicy::new(mode, cwd))),
            Some(Some(engine)) => Some(engine),
            // 没有 settings 配置时回退到环境变量。
            None => match Self::from_env(cwd) {
                Some(engine) => Some(engine),
                None => {
                    // 环境变量也没启用；只有命令行显式要求时才创建引擎。
                    if flags.mode.is_none()
                        && flags.allow.is_empty()
                        && flags.deny.is_empty()
                        && flags.add_dirs.is_empty()
                    {
                        return None;
                    }
                    Some(Self::new(PermissionPolicy::new(
                        flags.mode.unwrap_or_default(),
                        cwd,
                    )))
                }
            },
        };
        engine.map(|engine| engine.with_flags_and_bypass(flags, bypass_disabled))
    }

    /// 配置（settings 优先，其次环境变量）是否**显式关闭**权限检查。
    ///
    /// 与 [`Self::from_settings_and_flags`] 的区别：返回 `false` 只表示「没有显式关闭」，
    /// 不代表已经启用。CLI 的 `--allow/--deny/--add-dir` 据此判断能否创建引擎：
    /// 关闭时只有 `--permission-mode` 可以重新启用。
    pub fn configuration_disables(
        cwd: impl AsRef<Path>,
        settings: Option<&serde_json::Value>,
    ) -> bool {
        // 与 `from_settings_and_flags` 使用完全相同的判定：settings 里「有效配置」
        // （mode / 非空规则 / 目录）优先；`{"rules": []}` 这类空对象应继续看环境变量。
        match Self::settings_engine(cwd.as_ref(), settings) {
            Some(engine) => engine.is_none(),
            None => {
                let off = |raw: &str| {
                    matches!(
                        raw.trim().to_ascii_lowercase().as_str(),
                        "off" | "disabled" | "none" | "false" | "0"
                    )
                };
                for name in ["KIANA_PERMISSIONS", "KIANA_PERMISSION_MODE"] {
                    let Some(value) = std::env::var_os(name) else {
                        continue;
                    };
                    let text = value.to_string_lossy();
                    let text = text.trim();
                    if text.is_empty() {
                        continue;
                    }
                    return off(text);
                }
                false
            }
        }
    }

    /// 把 settings 里的 `permissions` 对象解析成引擎。
    ///
    /// 识别字段：
    /// - `mode` / `defaultMode`: `read-only` / `default` / `accept-edits` / `bypass` /
    ///   `off`（未知值按 `default`，保持 fail-closed；两者同时出现时 `mode` 优先）；
    /// - `rules`: 规则数组，每条支持 `{tool, decision, reason}`、`{command: [...], decision,
    ///   reason}`、`{path, decision, reason}`，也接受 `{allow: [...]}` / `{deny: [...]}` /
    ///   `{ask: [...]}` 简写；
    /// - `addDirs` / `workspaceRoots` / `additionalDirectories`: 额外可写根；
    /// - `disableBypassPermissionsMode` / `disable_bypass_permissions_mode`: 只作为
    ///   “需要创建引擎”的信号；真正的降级在 [`Self::from_settings_and_flags`] 里。
    ///
    /// 外层 `None` 表示没有配置，`Some(None)` 表示配置为 `off`。
    ///
    /// `off` 与 `disableBypassPermissionsMode` 同时出现时**不**返回 `Some(None)`：
    /// managed 的禁用键要求权限层继续存在，`off` 被解释为 `default`（继续解析
    /// 同层的 rules，并让 `from_settings_and_flags` 应用 bypass 降级）。
    fn settings_engine(cwd: &Path, settings: Option<&serde_json::Value>) -> Option<Option<Self>> {
        let Some(value) = settings else {
            return None;
        };
        if value.is_null() {
            return None;
        }
        let Some(object) = value.as_object() else {
            // 形状不对：不能当成「未配置」从而把权限层整个关掉。按 default 模式
            // fail-closed，用户修正 settings 之前仍然需要逐次批准。
            return Some(Some(Self::new(PermissionPolicy {
                mode: PermissionMode::Default,
                workspace_roots: vec![cwd.to_path_buf()],
                rules: Vec::new(),
            })));
        };
        // `defaultMode` 是 Claude Code/managed policy 的拼写；`mode` 是本项目的拼写。
        // 两者同时出现时 `mode` 优先（更具体的字段），否则 managed policy 里写下的
        // `defaultMode: "read-only"` 会被静默忽略、整个权限层保持关闭。
        let mode_value = object
            .get("mode")
            .and_then(serde_json::Value::as_str)
            .or_else(|| {
                object
                    .get("defaultMode")
                    .and_then(serde_json::Value::as_str)
            });
        let mut roots = vec![cwd.to_path_buf()];
        // `additionalDirectories` 是配置层认可的额外可写根（strict schema 与分层合并
        // 都接受它）；不在这里读取会让它成为一条静默失效的配置。
        for key in ["addDirs", "workspaceRoots", "additionalDirectories"] {
            if let Some(list) = object.get(key).and_then(serde_json::Value::as_array) {
                roots.extend(
                    list.iter()
                        .filter_map(serde_json::Value::as_str)
                        .map(PathBuf::from),
                );
            }
        }
        let mut rules = Vec::new();
        if let Some(list) = object.get("rules").and_then(serde_json::Value::as_array) {
            for entry in list {
                if let Some(rule) = rule_from_settings_value(entry) {
                    rules.push(rule);
                }
            }
        }
        // `{ "allow": [...], "deny": [...], "ask": [...] }` 简写。`ask` 由配置层
        // 合并、schema 校验，也必须真正进入引擎；否则写下的 Ask 规则会被静默忽略，
        // 本该先询问的调用直接走模式默认。
        for (key, decision) in [
            ("allow", RuleDecision::Allow),
            ("deny", RuleDecision::Deny),
            ("ask", RuleDecision::Ask),
        ] {
            if let Some(list) = object.get(key).and_then(serde_json::Value::as_array) {
                rules.extend(list.iter().filter_map(|entry| {
                    entry
                        .as_str()
                        .and_then(|spec| parse_cli_rule(spec, decision))
                }));
            }
        }

        let off = mode_value.is_some_and(|raw| {
            matches!(
                raw.trim().to_ascii_lowercase().as_str(),
                "off" | "disabled" | "none" | "false" | "0"
            )
        });
        // `disableBypassPermissionsMode` 是 managed policy 的强制键：同层写下的
        // `mode: "off"` 不能把整层权限检查关掉，否则 managed deny/allow 规则与
        // bypass 降级全部失效。把 off 解释为 default（fail-closed），继续解析
        // 规则；`mode_value` 里的字符串无法 parse 时下面会落到 Default。
        if off && !settings_disable_bypass(value) {
            return Some(None);
        }
        let mode = match mode_value {
            Some(raw) => Some(PermissionMode::parse(raw).unwrap_or(PermissionMode::Default)),
            None => None,
        };
        // 只有规则/目录/策略标志而没有 mode 时，用 default 模式让它们生效。
        if mode.is_none() && rules.is_empty() && roots.len() == 1 && !settings_disable_bypass(value)
        {
            return None;
        }
        let mode = mode.unwrap_or_default();
        let policy = PermissionPolicy {
            mode,
            workspace_roots: roots,
            rules,
        };
        Some(Some(Self::new(policy)))
    }

    fn with_flags_and_bypass(mut self, flags: &PermissionFlags, bypass_disabled: bool) -> Self {
        self.bypass_disabled = bypass_disabled;
        self.apply_flags(flags);
        self
    }

    fn apply_flags(&mut self, flags: &PermissionFlags) {
        {
            let mut policy = self
                .policy
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(mode) = flags.mode {
                policy.mode = self.allowed_mode(mode);
            }
            if self.bypass_disabled && policy.mode.is_bypass() {
                policy.mode = PermissionMode::Default;
            }
            for spec in &flags.allow {
                if let Some(rule) = parse_cli_rule(spec, RuleDecision::Allow) {
                    policy.rules.push(rule);
                }
            }
            for spec in &flags.deny {
                if let Some(rule) = parse_cli_rule(spec, RuleDecision::Deny) {
                    policy.rules.push(rule);
                }
            }
            policy
                .workspace_roots
                .extend(flags.add_dirs.iter().cloned());
            Self::normalize_rule_paths(&mut policy);
        }
        // 规则变更后，之前按旧规则记住的会话批准不能继续生效。
        self.clear_session_allow();
    }

    /// 从 `KIANA_PERMISSIONS` / `KIANA_PERMISSION_MODE` 构造。
    ///
    /// 未设置或 `off`/`disabled`/`none` 时返回 `None`（不启用权限检查）。
    /// 未知值按 `default` 处理，而不是静默放行。
    pub fn from_env(cwd: impl AsRef<std::path::Path>) -> Option<Self> {
        // 空值（`export KIANA_PERMISSIONS=`）按**未设置**处理，但不能遮蔽另一个变量：
        // `KIANA_PERMISSIONS="" KIANA_PERMISSION_MODE=read-only` 仍必须是 read-only。
        let mut selected = None;
        for name in ["KIANA_PERMISSIONS", "KIANA_PERMISSION_MODE"] {
            let Some(value) = std::env::var_os(name) else {
                continue;
            };
            if value.to_string_lossy().trim().is_empty() {
                continue;
            }
            selected = Some(value);
            break;
        }
        let raw = selected?;
        let raw = match raw.into_string() {
            Ok(value) => value,
            // 非 UTF-8 不能当成「未设置」——那会静默关闭整层权限检查。按 `default`
            // 处理（与未知值一致），保持 fail-closed。
            Err(_) => {
                return Some(Self::new(PermissionPolicy::new(
                    PermissionMode::Default,
                    cwd.as_ref(),
                )));
            }
        };
        let raw = raw.trim().to_ascii_lowercase();
        if matches!(raw.as_str(), "off" | "disabled" | "none" | "false" | "0") {
            return None;
        }
        let mode = PermissionMode::parse(&raw).unwrap_or(PermissionMode::Default);
        Some(Self::new(PermissionPolicy::new(mode, cwd.as_ref())))
    }

    /// 当前策略的克隆。
    ///
    /// 返回克隆而不是引用，是因为策略可以在运行期被
    /// [`PermissionEngine::set_mode`] / [`PermissionEngine::set_rules`] 修改。
    pub fn policy(&self) -> PermissionPolicy {
        self.policy
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// 当前模式。
    pub fn mode(&self) -> PermissionMode {
        self.policy
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .mode
    }

    /// 运行期切换模式。
    ///
    /// 模式变更会清空本会话的「总是允许」记录：旧模式下的批准不应该在新模式下继续
    /// 生效，否则切到 `read-only` 后仍可能执行此前批准过的命令。
    pub fn set_mode(&self, mode: PermissionMode) {
        let mode = self.allowed_mode(mode);
        self.policy
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .mode = mode;
        self.clear_session_allow();
    }

    /// managed policy 是否禁用了 `bypass` 模式。
    pub fn bypass_permissions_disabled(&self) -> bool {
        self.bypass_disabled
    }

    /// 本会话「总是允许」记录的数量（`/permissions` 面板只读展示）。
    pub fn session_allow_count(&self) -> usize {
        self.session_allow
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len()
    }

    /// 把 managed policy 禁止的模式降级（bypass → default）。
    fn allowed_mode(&self, mode: PermissionMode) -> PermissionMode {
        if self.bypass_disabled && mode.is_bypass() {
            PermissionMode::Default
        } else {
            mode
        }
    }

    /// 替换全部规则。
    pub fn set_rules(&self, rules: Vec<PermissionRule>) {
        let mut policy = self
            .policy
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        policy.rules = rules;
        Self::normalize_rule_paths(&mut policy);
        drop(policy);
        self.clear_session_allow();
    }

    /// 追加规则（不覆盖已有规则）。
    pub fn add_rules(&self, rules: impl IntoIterator<Item = PermissionRule>) {
        let mut policy = self
            .policy
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        policy.rules.extend(rules);
        Self::normalize_rule_paths(&mut policy);
        drop(policy);
        self.clear_session_allow();
    }

    /// 追加一个可写工作区根。
    pub fn add_workspace_root(&self, root: impl Into<PathBuf>) {
        let mut policy = self
            .policy
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let base = policy.workspace_roots.first().cloned().unwrap_or_default();
        let root = crate::request::resolve_rule_path(&base, &root.into());
        policy.workspace_roots.push(root);
    }

    /// 评估一次请求。
    pub fn evaluate(&self, request: &PermissionRequest) -> PermissionDecision {
        let policy = self
            .policy
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // managed policy 禁用 bypass 时，即使策略对象里是 bypass（例如直接构造的
        // `PermissionPolicy`），评估也按 default 处理。
        let mode = if self.bypass_disabled && policy.mode.is_bypass() {
            PermissionMode::Default
        } else {
            policy.mode
        };
        if !mode.is_bypass()
            && let Some(reason) =
                command::catastrophic_reason(request.command.as_deref().unwrap_or_default())
        {
            return PermissionDecision::Deny {
                reason: format!("Permission denied: command is blocked as destructive ({reason})."),
            };
        }

        if let Some(rule) = policy
            .rules
            .iter()
            .find(|rule| rule.decision() == RuleDecision::Deny && rule.matches(request))
        {
            return PermissionDecision::Deny {
                reason: rule.reason().map_or_else(
                    || {
                        format!(
                            "Permission denied by rule for tool '{}' ({}).",
                            request.tool_name,
                            request.action.as_str()
                        )
                    },
                    str::to_string,
                ),
            };
        }

        // virtual operations：`cat`/`ls`/`curl`/重定向被映射成 read_file /
        // list_directory / web_fetch / write_file。Deny/Ask 型 Tool 规则因此也能约束
        // shell 的等价操作；Allow 不参与（避免用读规则扩大 shell 权限）。
        if let Some(reason) = virtual_operation_rule(&policy, request, RuleDecision::Deny) {
            return PermissionDecision::Deny { reason };
        }

        // shell 写入 `.git`/`.kiana` 等于写入会在下一次 git 命令或启动时执行的代码。
        // 这条检查先于会话批准与 Allow 规则：即使用户写过 `Tool{bash,Allow}` 或
        // `PathPrefix{cwd,Allow}`，重定向到这些受保护目录也必须先确认。
        if matches!(request.action, ActionKind::Execute | ActionKind::Write)
            && !mode.is_bypass()
            && mode != PermissionMode::ReadOnly
            && request
                .paths
                .iter()
                .any(|path| is_protected_subpath(path, &policy.workspace_roots))
        {
            return PermissionDecision::Ask {
                prompt: request.approval_message(),
                reason: format!(
                    "{} writes a protected subdirectory (.git/.kiana)",
                    request.action.as_str()
                ),
            };
        }

        // 用户显式「总是允许」生成的规则先于 Ask 规则与 shell 分析检查，但**晚于**
        // Deny/灾难性命令与 .git/.kiana 安全检查：后者每次都必须重新确认。没有这一步，
        // Local/Project 作用域的批准在命中 Ask 规则时会永远无效（每次仍然询问），
        // 只有 Session 作用域因为会话批准检查更早而表现不同。
        if let Some(rule) = policy.rules.iter().find(|rule| {
            rule.is_approved() && rule.decision() == RuleDecision::Allow && rule.matches(request)
        }) {
            return PermissionDecision::Allow {
                reason: rule.reason().map_or_else(
                    || {
                        format!(
                            "Previously approved for tool '{}' ({}).",
                            request.tool_name,
                            request.action.as_str()
                        )
                    },
                    str::to_string,
                ),
            };
        }

        if self.is_session_allowed(request) {
            return PermissionDecision::Allow {
                reason: format!(
                    "Previously approved for this session: {}",
                    request.approval_key()
                ),
            };
        }

        if let Some(rule) = policy
            .rules
            .iter()
            .find(|rule| rule.decision() == RuleDecision::Ask && rule.matches(request))
        {
            let mut prompt = request.approval_message();
            let reason = rule.reason().map_or_else(
                || {
                    format!(
                        "Ask rule matched for tool '{}' ({}).",
                        request.tool_name,
                        request.action.as_str()
                    )
                },
                str::to_string,
            );
            if let Some(rule_reason) = rule.reason() {
                prompt = format!("{rule_reason}\n\n{prompt}");
            }
            return PermissionDecision::Ask { prompt, reason };
        }

        if let Some(reason) = virtual_operation_rule(&policy, request, RuleDecision::Ask) {
            return PermissionDecision::Ask {
                prompt: request.approval_message(),
                reason,
            };
        }

        // 解析失败/超限 → Unknown：不能因为“没匹配到 Deny/Ask”就按 Allow 规则放行。
        // read-only 继续走 base_decision 得到 Deny；bypass 是用户显式关闭全部检查。
        if request.action == ActionKind::Execute
            && !mode.is_bypass()
            && mode != PermissionMode::ReadOnly
            && request
                .shell
                .as_ref()
                .is_some_and(|shell| !shell.is_trustworthy())
        {
            let status = request
                .shell
                .as_ref()
                .map(|shell| shell.status.as_str())
                .unwrap_or("unknown");
            return PermissionDecision::Ask {
                prompt: request.approval_message(),
                reason: format!(
                    "shell command analysis is {status}; refusing to apply allow rules until the command is reviewed"
                ),
            };
        }

        if let Some(rule) = policy
            .rules
            .iter()
            .find(|rule| rule.decision() == RuleDecision::Allow && rule.matches(request))
        {
            return PermissionDecision::Allow {
                reason: rule.reason().map_or_else(
                    || {
                        format!(
                            "Allowed by rule for tool '{}' ({}).",
                            request.tool_name,
                            request.action.as_str()
                        )
                    },
                    str::to_string,
                ),
            };
        }

        base_decision(&policy, mode, request)
    }

    /// 记住“本会话允许”。
    pub fn record_session_allow(&self, request: &PermissionRequest) {
        self.session_allow
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(request.approval_key());
    }

    /// 清空本会话的批准记录。
    pub fn clear_session_allow(&self) {
        self.session_allow
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
    }

    /// 当前会话是否已批准该请求。
    pub fn is_session_allowed(&self, request: &PermissionRequest) -> bool {
        self.session_allow
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .contains(&request.approval_key())
    }

    /// 这次 `Ask` 是否允许用「总是允许」（任何作用域）记住。
    ///
    /// `.git`/`.kiana` 受保护目录的写入是安全确认：`evaluate` 先于会话批准与任何
    /// Allow/审批规则返回 `Ask`。这类确认必须每次重新询问，因此 UI 不应提供「总是
    /// 允许」，持久化入口也必须拒绝。其它 `Ask`（模式默认、显式 `Ask` 规则）可以由
    /// 用户显式批准覆盖。
    pub fn approval_can_persist(&self, request: &PermissionRequest) -> bool {
        let policy = self
            .policy
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mode = if self.bypass_disabled && policy.mode.is_bypass() {
            PermissionMode::Default
        } else {
            policy.mode
        };
        if mode.is_bypass() || mode == PermissionMode::ReadOnly {
            // 这两种模式不会走到受保护目录的 Ask（bypass 全放行，read-only 直接拒绝）。
            return true;
        }
        !(matches!(request.action, ActionKind::Execute | ActionKind::Write)
            && request
                .paths
                .iter()
                .any(|path| is_protected_subpath(path, &policy.workspace_roots)))
    }
}

/// `permissions.disableBypassPermissionsMode` / snake_case 别名是否为真。
///
/// 接受布尔 `true` 与 Claude Code 的字符串形态 `"disable"`（也接受 `"true"`）。
fn settings_disable_bypass(value: &serde_json::Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    [
        "disableBypassPermissionsMode",
        "disable_bypass_permissions_mode",
    ]
    .iter()
    .find_map(|key| object.get(*key))
    .is_some_and(disable_flag_enabled)
}

fn disable_flag_enabled(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Bool(value) => *value,
        serde_json::Value::String(text) => {
            text.eq_ignore_ascii_case("true") || text.eq_ignore_ascii_case("disable")
        }
        _ => false,
    }
}

fn base_decision(
    policy: &PermissionPolicy,
    mode: PermissionMode,
    request: &PermissionRequest,
) -> PermissionDecision {
    match mode {
        PermissionMode::ReadOnly => match request.action {
            ActionKind::Read => PermissionDecision::Allow {
                reason: "read-only mode allows reads".to_string(),
            },
            action => PermissionDecision::Deny {
                reason: format!(
                    "Permission mode 'read-only' does not allow {} actions (tool '{}').",
                    action.as_str(),
                    request.tool_name
                ),
            },
        },
        PermissionMode::Default => match request.action {
            ActionKind::Read => PermissionDecision::Allow {
                reason: "default mode allows reads".to_string(),
            },
            action => PermissionDecision::Ask {
                prompt: request.approval_message(),
                reason: format!(
                    "default mode asks before {} actions (tool '{}').",
                    action.as_str(),
                    request.tool_name
                ),
            },
        },
        PermissionMode::AcceptEdits => match request.action {
            ActionKind::Read => PermissionDecision::Allow {
                reason: "accept-edits mode allows reads".to_string(),
            },
            ActionKind::Write => {
                // `.git` / `.kiana` 与沙箱的只读保护一致：写它们等于写入会在下一次
                // git 命令时执行的代码，不能因为“在工作区内”就自动放行。
                if !request.analysis_incomplete
                    && request.paths_inside(&policy.workspace_roots)
                    && !request
                        .paths
                        .iter()
                        .any(|path| is_protected_subpath(path, &policy.workspace_roots))
                {
                    PermissionDecision::Allow {
                        reason: "write is inside the workspace".to_string(),
                    }
                } else {
                    let reason = if request.analysis_incomplete {
                        "write analysis is incomplete, so the target cannot be proven to be inside the workspace"
                    } else if request
                        .paths
                        .iter()
                        .any(|path| is_protected_subpath(path, &policy.workspace_roots))
                    {
                        "write targets a protected subdirectory (.git/.kiana)"
                    } else {
                        "write is outside the workspace"
                    };
                    PermissionDecision::Ask {
                        prompt: request.approval_message(),
                        reason: reason.to_string(),
                    }
                }
            }
            action => PermissionDecision::Ask {
                prompt: request.approval_message(),
                reason: format!(
                    "accept-edits mode asks before {} actions (tool '{}').",
                    action.as_str(),
                    request.tool_name
                ),
            },
        },
        PermissionMode::BypassPermissions => PermissionDecision::Allow {
            reason: "bypass mode allows every call".to_string(),
        },
    }
}

/// 裸规则文本是否是一个路径（而不是工具名/命令）。
///
/// 除 Unix 的 `/`、`./`、`../`、`~`、`$` 之外，也识别 Windows 盘符绝对路径与 UNC。
fn looks_like_path(spec: &str) -> bool {
    // `/` 绝对路径、`~`/`$` 展开、以及任何以 `.` 开头的相对路径或隐藏目录
    // （`./x`、`../x`、`.git`）。
    if spec.starts_with('/')
        || spec.starts_with('~')
        || spec.starts_with('$')
        || spec.starts_with('.')
    {
        return true;
    }
    // UNC：`\\server\share`。
    if spec.starts_with("\\\\") {
        return true;
    }
    // 盘符绝对路径：`C:\...` / `C:/...`。
    let bytes = spec.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/')
}

/// 解析命令行 `--allow` / `--deny` 的规则文本。
///
/// 支持的写法（大小写不敏感的前缀）：
/// - `tool:<pattern>` / 裸 `<pattern>`：工具名规则，`*` 用于开头/结尾通配；
/// - `command:<words>` / 含空白的文本：命令前缀规则（只对简单命令生效）；
/// - `path:<prefix>` / 绝对路径形态（含 Windows 盘符/UNC，即使包含空白）：路径前缀规则；
/// - `path=` / `command=` / `tool=` 也接受。
///
/// 无法识别的输入返回 `None`，由调用方决定是否报诊断。
pub fn parse_cli_rule(spec: &str, decision: RuleDecision) -> Option<PermissionRule> {
    let spec = spec.trim();
    if spec.is_empty() {
        return None;
    }
    if let Some(value) = strip_prefix_any(spec, &["path:", "path="]) {
        let value = value.trim();
        if value.is_empty() {
            return None;
        }
        return Some(PermissionRule::PathPrefix {
            prefix: PathBuf::from(value),
            actions: None,
            decision,
            reason: None,
        });
    }
    if let Some(value) = strip_prefix_any(spec, &["command:", "command="]) {
        let text = value.trim();
        if text.is_empty() {
            return None;
        }
        // 显式以 `*` 结尾表示通配规则；其它文本仍按前缀规则（只对简单命令生效）。
        if text.ends_with('*') {
            return Some(PermissionRule::wildcard_command(text, decision));
        }
        let tokens = text
            .split_whitespace()
            .map(str::to_string)
            .collect::<Vec<_>>();
        if tokens.is_empty() {
            return None;
        }
        return Some(PermissionRule::CommandPrefix {
            tokens,
            decision,
            reason: None,
        });
    }
    if let Some(value) = strip_prefix_any(spec, &["tool:", "tool="]) {
        let pattern = value.trim();
        if pattern.is_empty() {
            return None;
        }
        return Some(PermissionRule::Tool {
            pattern: pattern.to_string(),
            decision,
            reason: None,
        });
    }
    // 路径判断先于「含空白 → 命令」：`C:\Users\me\Private Files` 这类带空格的
    // Windows 路径（以及 `/opt/My Apps`）必须成为 PathPrefix，否则规则静默失效。
    if looks_like_path(spec) {
        return Some(PermissionRule::PathPrefix {
            prefix: PathBuf::from(spec),
            actions: None,
            decision,
            reason: None,
        });
    }
    if spec.contains(char::is_whitespace) {
        if spec.ends_with('*') {
            return Some(PermissionRule::wildcard_command(spec, decision));
        }
        return Some(PermissionRule::CommandPrefix {
            tokens: spec.split_whitespace().map(str::to_string).collect(),
            decision,
            reason: None,
        });
    }
    Some(PermissionRule::Tool {
        pattern: spec.to_string(),
        decision,
        reason: None,
    })
}

fn strip_prefix_any<'a>(value: &'a str, prefixes: &[&str]) -> Option<&'a str> {
    let lower = value.to_ascii_lowercase();
    prefixes
        .iter()
        .find(|prefix| lower.starts_with(&prefix.to_ascii_lowercase()))
        .map(|prefix| &value[prefix.len()..])
}

fn rule_decision(value: Option<&serde_json::Value>) -> Option<RuleDecision> {
    match value?.as_str()?.trim().to_ascii_lowercase().as_str() {
        "allow" => Some(RuleDecision::Allow),
        "ask" => Some(RuleDecision::Ask),
        "deny" => Some(RuleDecision::Deny),
        _ => None,
    }
}

/// settings 规则里的可选 `actions` 列表（`["read","write"]` 等）。
///
/// 形状错误（不是数组）或没有可识别的动作时返回空列表：规则不作用于任何动作。
/// 不能退化成 `None`——那会让规则作用于全部动作，把作者写错的限制变成授权。
fn parse_rule_actions(value: Option<&serde_json::Value>) -> Option<Vec<ActionKind>> {
    let value = value?;
    let Some(items) = value.as_array() else {
        return Some(Vec::new());
    };
    Some(
        items
            .iter()
            .filter_map(serde_json::Value::as_str)
            .filter_map(|raw| {
                serde_json::from_value(serde_json::Value::String(raw.to_string())).ok()
            })
            .collect::<Vec<ActionKind>>(),
    )
}

/// 把 settings 里的一条规则解析成 [`PermissionRule`]。
fn rule_from_settings_value(value: &serde_json::Value) -> Option<PermissionRule> {
    let object = value.as_object()?;
    let decision = rule_decision(object.get("decision"))?;
    let reason = object
        .get("reason")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    let rule = if let Some(tool) = object.get("tool").and_then(serde_json::Value::as_str) {
        PermissionRule::Tool {
            pattern: tool.to_string(),
            decision,
            reason,
        }
    } else if let Some(command) = object.get("command") {
        let command_text = match command {
            serde_json::Value::String(text) => text.clone(),
            serde_json::Value::Array(items) => items
                .iter()
                .filter_map(serde_json::Value::as_str)
                .collect::<Vec<_>>()
                .join(" "),
            _ => return None,
        };
        if command_text.trim().is_empty() {
            return None;
        }
        // `exact: true` 表示完整命令规则（「总是允许」对复合命令的写回形态）。
        if object
            .get("exact")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
        {
            PermissionRule::ExactCommand {
                command: command_text,
                decision,
                reason,
            }
        } else if object
            .get("wildcard")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
        {
            PermissionRule::CommandWildcard {
                pattern: command_text,
                decision,
                reason,
            }
        } else {
            let tokens = match command {
                serde_json::Value::Array(items) => items
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(str::to_string)
                    .collect::<Vec<_>>(),
                serde_json::Value::String(text) => {
                    text.split_whitespace().map(str::to_string).collect()
                }
                _ => return None,
            };
            if tokens.is_empty() {
                return None;
            }
            PermissionRule::CommandPrefix {
                tokens,
                decision,
                reason,
            }
        }
    } else if let Some(path) = object.get("path").and_then(serde_json::Value::as_str) {
        PermissionRule::PathPrefix {
            prefix: PathBuf::from(path),
            actions: parse_rule_actions(object.get("actions")),
            decision,
            reason,
        }
    } else if let Some(pattern) = object.get("pattern").and_then(serde_json::Value::as_str) {
        PermissionRule::Tool {
            pattern: pattern.to_string(),
            decision,
            reason,
        }
    } else {
        return None;
    };
    // 审批生成的规则带 `approved: true` 与工具来源（`origin`）。来源缺失/非法时丢弃
    // 整条规则：审批规则只会是 Allow，丢弃是 fail-closed；不能退化成按名字匹配的
    // `Tool` 规则（那正是自定义工具借用内建工具批准的漏洞）。
    if object
        .get("approved")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        let builtin = match object.get("origin").and_then(serde_json::Value::as_str) {
            Some("builtin") => true,
            Some("custom") => false,
            _ => return None,
        };
        return Some(PermissionRule::Approved {
            rule: Box::new(rule),
            builtin,
        });
    }
    Some(rule)
}

/// 路径是否落在工作区里的受保护子目录（`.git` / `.kiana`）。
///
/// 必须按 canonical 路径判断：工作区里的 `link -> .git` 会让词法前缀看起来是 `link`，
/// 实际写入却落在 `.git` 里。
fn is_protected_subpath(path: &Path, roots: &[PathBuf]) -> bool {
    let Some(resolved) = crate::request::canonical_with_missing(path) else {
        // 无法解析（链接环等）：按受保护处理，交给用户确认。
        return true;
    };
    roots.iter().any(|root| {
        let root = crate::request::canonical_with_missing(root)
            .unwrap_or_else(|| crate::request::normalize_path(root));
        let Ok(relative) = resolved.strip_prefix(&root) else {
            return false;
        };
        matches!(
            relative.components().next(),
            Some(Component::Normal(name)) if is_protected_name(name)
        )
    })
}

fn is_protected_name(name: &std::ffi::OsStr) -> bool {
    kiana_sandbox::DEFAULT_PROTECTED_SUBDIR_NAMES
        .iter()
        .any(|protected| {
            #[cfg(windows)]
            {
                name.to_string_lossy().eq_ignore_ascii_case(protected)
            }
            #[cfg(not(windows))]
            {
                name == std::ffi::OsStr::new(protected)
            }
        })
}

fn tool_matches(pattern: &str, name: &str) -> bool {
    if pattern == "*" || pattern == name {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix('*') {
        return name.starts_with(prefix);
    }
    if let Some(suffix) = pattern.strip_prefix('*') {
        return name.ends_with(suffix);
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::approval::{ApprovalDecision, ApprovalScope};

    #[test]
    fn tool_patterns_match_end_wildcards() {
        assert!(tool_matches("*", "bash"));
        assert!(tool_matches("mcp__*", "mcp__docs__search"));
        assert!(tool_matches("*__search", "mcp__docs__search"));
        assert!(tool_matches("bash", "bash"));
        assert!(!tool_matches("bash", "bash2"));
    }

    #[test]
    fn command_prefix_requires_simple_command() {
        let request = PermissionRequest::from_tool_call(
            "bash",
            &serde_json::json!({"command": "git status; rm -rf /"}),
            "/workspace",
        );
        let rule = PermissionRule::command(
            vec!["git".to_string(), "status".to_string()],
            RuleDecision::Allow,
        );
        assert!(!rule.matches(&request));
    }

    #[test]
    fn allow_prefix_rule_requires_exact_relative_command_paths() {
        let allow = PermissionRule::command(
            vec!["git".to_string(), "status".to_string()],
            RuleDecision::Allow,
        );
        for command in ["./git status", "sub/git status"] {
            let request = PermissionRequest::from_tool_call(
                "bash",
                &serde_json::json!({ "command": command }),
                "/workspace",
            );
            assert!(!allow.matches(&request), "{command} 不应命中 git 规则");
        }
        for command in ["git status", "/usr/bin/git status"] {
            let request = PermissionRequest::from_tool_call(
                "bash",
                &serde_json::json!({ "command": command }),
                "/workspace",
            );
            assert!(allow.matches(&request), "{command} 应命中 git 规则");
        }
        // 不受信任目录里的同名程序不能借用 Allow 规则。
        let request = PermissionRequest::from_tool_call(
            "bash",
            &serde_json::json!({ "command": "/tmp/evil/git status" }),
            "/workspace",
        );
        assert!(!allow.matches(&request));
    }

    #[test]
    fn command_prefix_sees_through_wrappers_and_absolute_paths() {
        let ask = PermissionRule::command(vec!["rm".to_string()], RuleDecision::Ask);
        for command in [
            "sudo rm -rf ~/Documents",
            "/bin/rm -rf ~/Documents",
            "env rm -rf ~/Documents",
            "timeout 5 rm -rf ~/Documents",
        ] {
            let request = PermissionRequest::from_tool_call(
                "bash",
                &serde_json::json!({ "command": command }),
                "/workspace",
            );
            assert!(ask.matches(&request), "{command} 应命中 rm 规则");
        }
    }

    #[test]
    fn ask_prefix_rule_applies_to_compound_commands() {
        let ask = PermissionRule::command(vec!["rm".to_string()], RuleDecision::Ask);
        let request = PermissionRequest::from_tool_call(
            "bash",
            &serde_json::json!({"command": "echo hi; rm -rf ~/Documents"}),
            "/workspace",
        );
        assert!(!request.simple_command);
        assert!(ask.matches(&request), "复合命令里的 rm 仍应触发 Ask 规则");
    }

    #[test]
    fn ask_prefix_rule_sees_shell_c_and_substitutions() {
        let ask = PermissionRule::command(vec!["rm".to_string()], RuleDecision::Ask);
        for command in [
            r#"bash -c "rm -rf ~/Documents""#,
            r#"sh -c 'rm -rf ~/Documents'"#,
            "echo $(rm -rf ~/Documents)",
            "echo `rm -rf ~/Documents`",
        ] {
            let request = PermissionRequest::from_tool_call(
                "bash",
                &serde_json::json!({ "command": command }),
                "/workspace",
            );
            assert!(ask.matches(&request), "{command} 应命中 rm 规则");
        }
    }

    #[test]
    fn allow_prefix_rule_does_not_match_privileged_variants() {
        let allow = PermissionRule::command(
            vec!["npm".to_string(), "test".to_string()],
            RuleDecision::Allow,
        );
        let plain = PermissionRequest::from_tool_call(
            "bash",
            &serde_json::json!({"command": "npm test"}),
            "/workspace",
        );
        assert!(allow.matches(&plain));

        for command in ["sudo npm test", "env npm test"] {
            let request = PermissionRequest::from_tool_call(
                "bash",
                &serde_json::json!({ "command": command }),
                "/workspace",
            );
            assert!(
                !allow.matches(&request),
                "{command} 不应命中未提权的 Allow 规则"
            );
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn command_name_match_is_case_sensitive_on_unix() {
        let allow = PermissionRule::command(
            vec!["git".to_string(), "status".to_string()],
            RuleDecision::Allow,
        );
        for command in ["./GIT status", "./Git status"] {
            let request = PermissionRequest::from_tool_call(
                "bash",
                &serde_json::json!({ "command": command }),
                "/workspace",
            );
            assert!(
                !allow.matches(&request),
                "Unix 上的命令名区分大小写：{command}"
            );
        }
    }

    #[test]
    fn deny_prefix_rule_matches_the_wrapper_itself() {
        let deny = PermissionRule::command(vec!["sudo".to_string()], RuleDecision::Deny);
        let request = PermissionRequest::from_tool_call(
            "bash",
            &serde_json::json!({"command": "sudo rm -rf /tmp/foo"}),
            "/workspace",
        );
        assert!(deny.matches(&request));
    }

    #[test]
    fn allow_path_rule_rejects_incomplete_analysis() {
        let mut command = "echo x > /root/.bashrc".to_string();
        for _ in 0..40 {
            command = format!("echo $({command})");
        }
        let command = format!("touch /workspace/ok && {command}");
        let request = PermissionRequest::from_tool_call(
            "bash",
            &serde_json::json!({ "command": command }),
            "/workspace",
        );
        assert!(request.analysis_incomplete);
        let rule = PermissionRule::path("/workspace", RuleDecision::Allow);
        assert!(
            !rule.matches(&request),
            "分析不完整时路径集合不可信，Allow 规则必须失效"
        );
    }

    #[test]
    fn accept_edits_asks_before_writing_protected_subpaths() {
        let engine = PermissionEngine::new(PermissionPolicy::new(
            PermissionMode::AcceptEdits,
            "/workspace",
        ));
        for path in [".git/hooks/pre-commit", ".kiana/settings.json"] {
            let request = PermissionRequest::from_tool_call(
                "write",
                &serde_json::json!({ "path": format!("/workspace/{path}") }),
                "/workspace",
            );
            assert!(
                engine.evaluate(&request).is_ask(),
                "{path} 不能被 accept-edits 自动放行"
            );
        }
        let normal = PermissionRequest::from_tool_call(
            "write",
            &serde_json::json!({ "path": "/workspace/src/lib.rs" }),
            "/workspace",
        );
        assert!(engine.evaluate(&normal).is_allow());
    }

    #[cfg(unix)]
    #[test]
    fn accept_edits_protected_subpath_via_symlink() {
        let workspace = tempfile::TempDir::new().expect("workspace");
        std::fs::create_dir_all(workspace.path().join(".git/hooks")).expect("git dir");
        std::os::unix::fs::symlink(".git", workspace.path().join("link")).expect("symlink");

        let engine = PermissionEngine::new(PermissionPolicy::new(
            PermissionMode::AcceptEdits,
            workspace.path().to_path_buf(),
        ));
        let request = PermissionRequest::from_tool_call(
            "write",
            &serde_json::json!({ "path": workspace.path().join("link/hooks/pre-commit").to_string_lossy() }),
            workspace.path().to_path_buf(),
        );
        assert!(
            engine.evaluate(&request).is_ask(),
            "指向 .git 的符号链接不能被 accept-edits 自动放行"
        );
    }

    #[cfg(unix)]
    #[test]
    fn empty_path_prefix_is_rejected_and_never_matches_everything() {
        assert!(parse_cli_rule("path:", RuleDecision::Allow).is_none());
        assert!(parse_cli_rule("path:   ", RuleDecision::Deny).is_none());
        let rule = PermissionRule::PathPrefix {
            prefix: PathBuf::new(),
            actions: None,
            decision: RuleDecision::Allow,
            reason: None,
        };
        let request = PermissionRequest::from_tool_call(
            "write",
            &serde_json::json!({ "path": "/etc/passwd" }),
            "/workspace",
        );
        assert!(!rule.matches(&request), "空前缀的 Allow 不能匹配一切");
    }

    #[test]
    fn tilde_path_rules_are_expanded_against_the_workspace_root() {
        let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
            return;
        };
        let engine =
            PermissionEngine::new(PermissionPolicy::new(PermissionMode::Default, "/workspace"));
        let rule = parse_cli_rule("path:~/secrets", RuleDecision::Deny).expect("rule");
        engine.add_rules([rule]);
        let request = PermissionRequest::from_tool_call(
            "bash",
            &serde_json::json!({
                "command": format!("cat {}/secrets/key", home.display())
            }),
            "/workspace",
        );
        assert!(
            engine.evaluate(&request).is_deny(),
            "`~` 规则必须按 HOME 展开后匹配读取路径"
        );
    }

    #[test]
    fn configuration_disables_only_recognizes_explicit_off() {
        assert!(PermissionEngine::configuration_disables(
            "/workspace",
            Some(&serde_json::json!({"mode": "off"}))
        ));
        assert!(!PermissionEngine::configuration_disables(
            "/workspace",
            Some(&serde_json::json!({"mode": "read-only"}))
        ));
        assert!(!PermissionEngine::configuration_disables(
            "/workspace",
            Some(&serde_json::json!({"rules": []}))
        ));
    }

    #[test]
    fn approval_path_rule_is_scoped_to_the_approved_action() {
        let rule = PermissionRule::PathPrefix {
            prefix: PathBuf::from("/workspace"),
            actions: Some(vec![ActionKind::Write]),
            decision: RuleDecision::Allow,
            reason: None,
        };
        let write = PermissionRequest::from_tool_call(
            "write",
            &serde_json::json!({ "path": "/workspace/notes.txt" }),
            "/workspace",
        );
        assert!(rule.matches(&write), "审批时的动作必须命中");
        let shell = PermissionRequest::from_tool_call(
            "bash",
            &serde_json::json!({ "command": "cat /workspace/notes.txt" }),
            "/workspace",
        );
        assert!(
            !rule.matches(&shell),
            "单文件写审批不能变成 shell 对该路径的通行证"
        );
    }

    #[test]
    fn malformed_rule_actions_do_not_widen_the_rule() {
        let rule = rule_from_settings_value(&serde_json::json!({
            "path": "/workspace",
            "decision": "allow",
            "actions": "write"
        }))
        .expect("rule parses");
        let write = PermissionRequest::from_tool_call(
            "write",
            &serde_json::json!({ "path": "/workspace/notes.txt" }),
            "/workspace",
        );
        assert!(
            !rule.matches(&write),
            "无法识别的 actions 必须让规则不匹配任何动作，而不是匹配全部"
        );
    }

    #[test]
    fn write_allow_rules_cannot_bypass_the_protected_subdirectory_guard() {
        let engine = PermissionEngine::new(PermissionPolicy {
            mode: PermissionMode::Default,
            workspace_roots: vec![PathBuf::from("/workspace")],
            rules: vec![PermissionRule::tool("write", RuleDecision::Allow)],
        });
        let protected = PermissionRequest::from_tool_call(
            "write",
            &serde_json::json!({ "path": "/workspace/.git/hooks/pre-commit" }),
            "/workspace",
        );
        assert!(
            engine.evaluate(&protected).is_ask(),
            "Allow 规则不能放行 .git/.kiana 的写入"
        );
        let normal = PermissionRequest::from_tool_call(
            "write",
            &serde_json::json!({ "path": "/workspace/src/lib.rs" }),
            "/workspace",
        );
        assert!(engine.evaluate(&normal).is_allow());
    }

    #[test]
    fn malformed_permissions_object_does_not_disable_the_engine() {
        let malformed = serde_json::json!(5);
        assert!(
            !PermissionEngine::configuration_disables("/workspace", Some(&malformed)),
            "形状错误不能被当成 mode: off"
        );
        let engine = PermissionEngine::from_settings_and_flags(
            "/workspace",
            Some(&malformed),
            &PermissionFlags::default(),
        )
        .expect("engine should stay enabled");
        let write = PermissionRequest::from_tool_call(
            "write",
            &serde_json::json!({ "path": "/workspace/notes.txt" }),
            "/workspace",
        );
        assert!(engine.evaluate(&write).is_ask(), "default 模式必须逐次批准");
    }

    #[test]
    fn cli_path_rules_with_spaces_stay_paths() {
        for spec in [
            r"C:\Users\me\Private Files",
            "/opt/My Apps",
            "./build cache",
            ".git",
            r"\\server\share\dir name",
        ] {
            let rule = parse_cli_rule(spec, RuleDecision::Deny).expect("rule parses");
            assert!(
                matches!(rule, PermissionRule::PathPrefix { .. }),
                "{spec} 必须是 PathPrefix"
            );
        }
        // 普通命令仍然按前缀规则处理。
        let rule = parse_cli_rule("git status", RuleDecision::Deny).expect("rule parses");
        assert!(matches!(rule, PermissionRule::CommandPrefix { .. }));
    }

    #[test]
    fn deny_path_rule_resolves_symlink_targets() {
        let workspace = tempfile::TempDir::new().expect("workspace");
        let secret = tempfile::TempDir::new().expect("secret");
        let link = workspace.path().join("link");
        std::os::unix::fs::symlink(secret.path(), &link).expect("symlink");
        std::fs::write(secret.path().join("shadow"), "secret").expect("write");

        let deny = PermissionRule::path(secret.path(), RuleDecision::Deny);
        let request = PermissionRequest::from_tool_call(
            "bash",
            &serde_json::json!({ "command": format!("cat {}/shadow", link.display()) }),
            workspace.path(),
        );
        assert!(deny.matches(&request), "符号链接指向 Deny 目录必须命中");
    }

    #[test]
    fn deny_path_rule_sees_read_and_incomplete_analysis() {
        let deny = PermissionRule::path("/etc", RuleDecision::Deny);
        let read = PermissionRequest::from_tool_call(
            "bash",
            &serde_json::json!({"command": "cat /etc/shadow"}),
            "/workspace",
        );
        assert!(deny.matches(&read), "读取路径也要命中 Deny");

        // `cd` 之后的相对写入无法按会话 cwd 解析：分析不完整时 Deny 必须命中。
        let changed_directory = PermissionRequest::from_tool_call(
            "bash",
            &serde_json::json!({"command": "cd /etc && echo key >> shadow"}),
            "/workspace",
        );
        assert!(deny.matches(&changed_directory));
    }

    #[test]
    fn allow_path_rule_ignores_read_only_paths() {
        let allow = PermissionRule::path("/workspace", RuleDecision::Allow);
        let request = PermissionRequest::from_tool_call(
            "bash",
            &serde_json::json!({"command": "touch /workspace/ok && cat /etc/shadow"}),
            "/workspace",
        );
        assert!(
            allow.matches(&request),
            "读取区外文件不应阻止工作区写入的 Allow"
        );
    }

    #[test]
    fn allow_path_rule_requires_every_path_inside() {
        let rule = PermissionRule::path("/workspace", RuleDecision::Allow);
        let inside = PermissionRequest::from_tool_call(
            "bash",
            &serde_json::json!({"command": "touch /workspace/ok"}),
            "/workspace",
        );
        assert!(rule.matches(&inside));

        let mixed = PermissionRequest::from_tool_call(
            "bash",
            &serde_json::json!({"command": "touch /workspace/ok && echo x > /root/.bashrc"}),
            "/workspace",
        );
        assert!(
            !rule.matches(&mixed),
            "Allow 路径规则不能在存在外部写入路径时整条放行"
        );
    }

    #[test]
    fn settings_mode_controls_base_decision() {
        let cwd = std::env::temp_dir();
        let write = PermissionRequest::from_tool_call(
            "write",
            &serde_json::json!({"path": cwd.join("a.txt")}),
            &cwd,
        );
        let engine = PermissionEngine::from_settings_and_flags(
            &cwd,
            Some(&serde_json::json!({"mode": "read-only"})),
            &PermissionFlags::default(),
        )
        .expect("settings enable the engine");
        assert!(engine.evaluate(&write).is_deny());

        let engine = PermissionEngine::from_settings_and_flags(
            &cwd,
            Some(&serde_json::json!({"mode": "accept-edits"})),
            &PermissionFlags::default(),
        )
        .expect("settings enable the engine");
        assert!(engine.evaluate(&write).is_allow());
    }

    #[test]
    fn settings_off_disables_the_engine_unless_a_flag_sets_a_mode() {
        let cwd = std::env::temp_dir();
        let settings = serde_json::json!({"mode": "off"});
        assert!(
            PermissionEngine::from_settings_and_flags(
                &cwd,
                Some(&settings),
                &PermissionFlags::default()
            )
            .is_none()
        );
        let flags = PermissionFlags {
            mode: Some(PermissionMode::BypassPermissions),
            ..Default::default()
        };
        let engine = PermissionEngine::from_settings_and_flags(&cwd, Some(&settings), &flags)
            .expect("an explicit CLI mode overrides settings off");
        assert_eq!(engine.mode(), PermissionMode::BypassPermissions);
    }

    #[test]
    fn managed_bypass_disable_prevents_off_from_disabling_the_engine() {
        let cwd = std::env::temp_dir();
        // managed policy 把 `disableBypassPermissionsMode` 合并进有效 permissions 后，
        // 用户/项目层写 `mode: "off"` 不能把整层权限检查关掉。
        let settings = serde_json::json!({
            "mode": "off",
            "disableBypassPermissionsMode": true,
            "rules": [{"tool": "bash", "decision": "deny"}]
        });
        assert!(
            !PermissionEngine::configuration_disables(&cwd, Some(&settings)),
            "managed policy must keep permissions enabled"
        );
        let engine = PermissionEngine::from_settings_and_flags(
            &cwd,
            Some(&settings),
            &PermissionFlags::default(),
        )
        .expect("managed policy must keep the engine alive");
        assert!(engine.bypass_permissions_disabled());
        assert_eq!(engine.mode(), PermissionMode::Default);

        // managed deny 规则仍然生效。
        let bash = PermissionRequest::from_tool_call(
            "bash",
            &serde_json::json!({"command": "echo hi"}),
            &cwd,
        );
        assert!(engine.evaluate(&bash).is_deny(), "managed deny must apply");

        // 同层没有规则覆盖的写操作仍然逐次询问（不是静默放行）。
        let write = PermissionRequest::from_tool_call(
            "write",
            &serde_json::json!({"path": cwd.join("notes.txt").to_string_lossy()}),
            &cwd,
        );
        assert!(engine.evaluate(&write).is_ask(), "default mode must ask");

        // 命令行 `--permission-mode bypass` 也被 managed 键降级，而不是重新启用 off。
        let flags = PermissionFlags {
            mode: Some(PermissionMode::BypassPermissions),
            ..Default::default()
        };
        let engine = PermissionEngine::from_settings_and_flags(&cwd, Some(&settings), &flags)
            .expect("engine");
        assert_eq!(engine.mode(), PermissionMode::Default);
        assert!(engine.evaluate(&bash).is_deny());

        // snake_case 与字符串形态同样触发保护。
        let snake = serde_json::json!({
            "mode": "disabled",
            "disable_bypass_permissions_mode": "disable",
        });
        let engine = PermissionEngine::from_settings_and_flags(
            &cwd,
            Some(&snake),
            &PermissionFlags::default(),
        )
        .expect("snake_case managed flag must keep the engine alive");
        assert!(engine.bypass_permissions_disabled());
        assert_eq!(engine.mode(), PermissionMode::Default);
    }

    #[test]
    fn managed_policy_disables_bypass_mode() {
        let cwd = std::env::temp_dir();
        let bash = PermissionRequest::from_tool_call(
            "bash",
            &serde_json::json!({"command": "echo hi"}),
            &cwd,
        );
        // settings 里的 bypass 被 `disableBypassPermissionsMode` 降级为 default。
        let settings = serde_json::json!({
            "mode": "bypass",
            "disableBypassPermissionsMode": true
        });
        let engine = PermissionEngine::from_settings_and_flags(
            &cwd,
            Some(&settings),
            &PermissionFlags::default(),
        )
        .expect("engine");
        assert!(engine.bypass_permissions_disabled());
        assert_eq!(engine.mode(), PermissionMode::Default);
        assert!(engine.evaluate(&bash).is_ask());

        // 命令行 `--permission-mode bypass` 同样不能覆盖 managed policy。
        let flags = PermissionFlags {
            mode: Some(PermissionMode::BypassPermissions),
            ..Default::default()
        };
        let engine = PermissionEngine::from_settings_and_flags(&cwd, Some(&settings), &flags)
            .expect("engine");
        assert_eq!(engine.mode(), PermissionMode::Default);
        assert!(engine.evaluate(&bash).is_ask());

        // 运行期切换也被忽略。
        engine.set_mode(PermissionMode::BypassPermissions);
        assert_eq!(engine.mode(), PermissionMode::Default);
        assert!(engine.evaluate(&bash).is_ask());
    }

    #[test]
    fn bypass_disable_accepts_snake_case_and_string_form() {
        let cwd = std::env::temp_dir();
        let settings = serde_json::json!({
            "mode": "bypass",
            "disable_bypass_permissions_mode": "disable"
        });
        let engine = PermissionEngine::from_settings_and_flags(
            &cwd,
            Some(&settings),
            &PermissionFlags::default(),
        )
        .expect("engine");
        assert_eq!(engine.mode(), PermissionMode::Default);
        assert!(engine.bypass_permissions_disabled());
    }

    #[test]
    fn bypass_disable_flag_alone_still_enables_the_engine() {
        let cwd = std::env::temp_dir();
        let settings = serde_json::json!({"disableBypassPermissionsMode": true});
        assert!(!PermissionEngine::configuration_disables(
            &cwd,
            Some(&settings)
        ));
        let engine = PermissionEngine::from_settings_and_flags(
            &cwd,
            Some(&settings),
            &PermissionFlags::default(),
        )
        .expect("policy flag alone must enable the engine");
        assert_eq!(engine.mode(), PermissionMode::Default);
        assert!(engine.bypass_permissions_disabled());
    }

    #[test]
    fn deny_flag_wins_over_allow_setting() {
        let cwd = std::env::temp_dir();
        let settings = serde_json::json!({
            "mode": "default",
            "rules": [{"tool": "bash", "decision": "allow"}]
        });
        let flags = PermissionFlags {
            deny: vec!["bash".to_string()],
            ..Default::default()
        };
        let engine = PermissionEngine::from_settings_and_flags(&cwd, Some(&settings), &flags)
            .expect("engine");
        let request = PermissionRequest::from_tool_call(
            "bash",
            &serde_json::json!({"command": "echo hi"}),
            &cwd,
        );
        assert!(engine.evaluate(&request).is_deny());
    }

    #[test]
    fn settings_rules_and_add_dirs_are_parsed() {
        let cwd = std::env::temp_dir();
        let settings = serde_json::json!({
            "rules": [
                {"command": ["git", "status"], "decision": "allow"},
                {"path": "/tmp/out", "decision": "deny", "reason": "no"}
            ],
            "addDirs": ["/tmp/extra"]
        });
        let engine = PermissionEngine::from_settings_and_flags(
            &cwd,
            Some(&settings),
            &PermissionFlags::default(),
        )
        .expect("rules enable the engine");
        assert_eq!(engine.policy().workspace_roots.len(), 2);
        let status = PermissionRequest::from_tool_call(
            "bash",
            &serde_json::json!({"command": "git status"}),
            &cwd,
        );
        assert!(engine.evaluate(&status).is_allow());
    }

    #[test]
    fn runtime_mode_and_rule_updates_take_effect() {
        let cwd = std::env::temp_dir();
        let engine = PermissionEngine::new(PermissionPolicy::new(PermissionMode::Default, &cwd));
        let request = PermissionRequest::from_tool_call(
            "bash",
            &serde_json::json!({"command": "echo hi"}),
            &cwd,
        );
        assert!(engine.evaluate(&request).is_ask());
        engine.set_mode(PermissionMode::BypassPermissions);
        assert!(engine.evaluate(&request).is_allow());
        engine.set_mode(PermissionMode::ReadOnly);
        assert!(engine.evaluate(&request).is_deny());
        engine.set_rules(vec![PermissionRule::tool("bash", RuleDecision::Allow)]);
        assert!(
            engine.evaluate(&request).is_allow(),
            "an explicit allow rule may grant execution; deny rules still win"
        );
    }

    #[test]
    fn cli_rule_syntax_is_parsed() {
        assert!(matches!(
            parse_cli_rule("read", RuleDecision::Allow),
            Some(PermissionRule::Tool { .. })
        ));
        assert!(matches!(
            parse_cli_rule("tool:mcp__*", RuleDecision::Allow),
            Some(PermissionRule::Tool { .. })
        ));
        assert!(matches!(
            parse_cli_rule("command:git status", RuleDecision::Deny),
            Some(PermissionRule::CommandPrefix { .. })
        ));
        assert!(matches!(
            parse_cli_rule("/tmp/extra", RuleDecision::Allow),
            Some(PermissionRule::PathPrefix { .. })
        ));
        assert!(parse_cli_rule("   ", RuleDecision::Allow).is_none());
    }

    #[test]
    fn exact_command_rules_match_only_the_same_command() {
        let rule = PermissionRule::exact_command("echo a; echo b", RuleDecision::Allow);
        let same = PermissionRequest::from_tool_call(
            "bash",
            &serde_json::json!({"command": "  echo a; echo b  "}),
            "/workspace",
        );
        assert!(rule.matches(&same));
        let different = PermissionRequest::from_tool_call(
            "bash",
            &serde_json::json!({"command": "echo a; echo b; rm -rf /"}),
            "/workspace",
        );
        assert!(!rule.matches(&different));
        let write =
            PermissionRequest::from_tool_call("write", &serde_json::json!({"path": "/x"}), "/w");
        assert!(!rule.matches(&write), "exact command 只匹配执行类工具");
    }

    #[test]
    fn rules_round_trip_through_settings_json() {
        let rules = vec![
            PermissionRule::command(
                vec!["git".to_string(), "status".to_string()],
                RuleDecision::Allow,
            ),
            PermissionRule::exact_command("echo a; echo b", RuleDecision::Allow),
            PermissionRule::path("/workspace/out", RuleDecision::Allow),
            PermissionRule::tool("mcp__*", RuleDecision::Deny).with_reason("no mcp"),
        ];
        let settings = serde_json::json!({ "rules": rules.iter().map(PermissionRule::to_settings_value).collect::<Vec<_>>() });
        let engine = PermissionEngine::from_settings_and_flags(
            "/workspace",
            Some(&settings),
            &PermissionFlags::default(),
        )
        .expect("rules enable the engine");
        assert_eq!(engine.policy().rules, rules);
    }

    #[test]
    fn always_allow_rules_never_override_deny() {
        let cwd = std::env::temp_dir();
        let engine = PermissionEngine::new(PermissionPolicy::new(PermissionMode::Default, &cwd));
        let request = PermissionRequest::from_tool_call(
            "bash",
            &serde_json::json!({"command": "git status"}),
            &cwd,
        );
        engine.add_rules(vec![PermissionRule::command(
            vec!["git".to_string(), "status".to_string()],
            RuleDecision::Allow,
        )]);
        engine.add_rules(vec![PermissionRule::command(
            vec!["git".to_string()],
            RuleDecision::Deny,
        )]);
        assert!(engine.evaluate(&request).is_deny());
    }

    /// 配置层接受的 `permissions.ask` / `permissions.defaultMode` /
    /// `permissions.additionalDirectories` 必须真正进入引擎。
    #[test]
    fn settings_ask_default_mode_and_additional_directories_are_enforced() {
        let cwd = std::env::temp_dir();
        let engine = PermissionEngine::from_settings_and_flags(
            &cwd,
            Some(&serde_json::json!({
                "ask": ["bash"],
                "additionalDirectories": ["/tmp/extra"]
            })),
            &PermissionFlags::default(),
        )
        .expect("ask/additionalDirectories enable the engine");
        assert_eq!(
            engine.policy().workspace_roots.len(),
            2,
            "additionalDirectories 必须加入工作区根"
        );
        let bash = PermissionRequest::from_tool_call(
            "bash",
            &serde_json::json!({"command": "echo hi"}),
            &cwd,
        );
        assert!(
            engine.evaluate(&bash).is_ask(),
            "permissions.ask 里的 bash 规则必须生效"
        );

        // managed policy 常用的 `defaultMode: "read-only"` 必须等同 `mode`。
        let engine = PermissionEngine::from_settings_and_flags(
            &cwd,
            Some(&serde_json::json!({"defaultMode": "read-only"})),
            &PermissionFlags::default(),
        )
        .expect("defaultMode must enable the engine");
        assert_eq!(engine.mode(), PermissionMode::ReadOnly);
        assert!(engine.evaluate(&bash).is_deny());

        // `mode` 同时存在时优先。
        let engine = PermissionEngine::from_settings_and_flags(
            &cwd,
            Some(&serde_json::json!({"mode": "bypass", "defaultMode": "read-only"})),
            &PermissionFlags::default(),
        )
        .expect("mode wins over defaultMode");
        assert_eq!(engine.mode(), PermissionMode::BypassPermissions);
    }

    /// 「总是允许」的三个作用域都必须真正生效：显式 Ask 规则不能让 Local/Project
    /// 持久化的批准每次重新询问；普通 Allow 规则仍然不能覆盖 Ask。
    #[test]
    fn persisted_approvals_override_ask_rules_for_every_scope() {
        let cwd = std::env::temp_dir();
        let settings = serde_json::json!({"mode": "default", "ask": ["bash"]});
        let request = PermissionRequest::from_tool_call(
            "bash",
            &serde_json::json!({"command": "git status"}),
            &cwd,
        );

        // Session：会话批准在 Ask 规则之前检查（已有行为）。
        let engine = PermissionEngine::from_settings_and_flags(
            &cwd,
            Some(&settings),
            &PermissionFlags::default(),
        )
        .expect("engine");
        assert!(engine.evaluate(&request).is_ask());
        engine.record_session_allow(&request);
        assert!(engine.evaluate(&request).is_allow());

        // Local / Project：规则写回 settings 后，重新构造的引擎也必须先于 Ask 规则应用。
        for scope in [ApprovalScope::Local, ApprovalScope::Project] {
            let rules = ApprovalDecision::AllowAlways { scope }.rules_for(&request);
            let persisted = serde_json::json!({
                "mode": "default",
                "ask": ["bash"],
                "rules": rules.iter().map(PermissionRule::to_settings_value).collect::<Vec<_>>(),
            });
            let engine = PermissionEngine::from_settings_and_flags(
                &cwd,
                Some(&persisted),
                &PermissionFlags::default(),
            )
            .expect("persisted approval enables the engine");
            assert!(
                engine.evaluate(&request).is_allow(),
                "{scope:?} 作用域的批准必须覆盖同名 Ask 规则"
            );
        }

        // 普通 Allow 规则（非审批生成）不能覆盖 Ask 规则：deny > ask > allow 不变。
        let engine = PermissionEngine::from_settings_and_flags(
            &cwd,
            Some(&serde_json::json!({
                "mode": "default",
                "ask": ["bash"],
                "rules": [{"tool": "bash", "decision": "allow"}]
            })),
            &PermissionFlags::default(),
        )
        .expect("engine");
        assert!(
            engine.evaluate(&request).is_ask(),
            "配置里的普通 Allow 规则仍然不能覆盖显式 Ask 规则"
        );
    }

    /// `.git`/`.kiana` 写入必须每次都询问；任何作用域的批准都不能绕过安全检查。
    #[test]
    fn protected_path_writes_always_ask_for_every_scope() {
        let workspace = tempfile::TempDir::new().expect("workspace");
        std::fs::create_dir_all(workspace.path().join(".git/hooks")).expect("git dir");
        std::fs::create_dir_all(workspace.path().join(".kiana")).expect("kiana dir");

        for relative in [".git/hooks/pre-commit", ".kiana/settings.json"] {
            let path = workspace.path().join(relative);
            let request = PermissionRequest::from_tool_call(
                "write",
                &serde_json::json!({ "path": path.to_string_lossy() }),
                workspace.path(),
            );
            let engine = PermissionEngine::new(PermissionPolicy::new(
                PermissionMode::Default,
                workspace.path().to_path_buf(),
            ));
            assert!(engine.evaluate(&request).is_ask(), "{relative}");
            assert!(
                !engine.approval_can_persist(&request),
                "{relative} 不能被「总是允许」记住"
            );

            // Session 作用域：记录批准后仍然重新询问（安全确认先于会话批准）。
            engine.record_session_allow(&request);
            assert!(
                engine.evaluate(&request).is_ask(),
                "{relative}: session 批准不能绕过安全检查"
            );

            // Local / Project：即使规则被写进设置，也仍然重新询问。
            for scope in [ApprovalScope::Local, ApprovalScope::Project] {
                let engine = PermissionEngine::new(PermissionPolicy::new(
                    PermissionMode::Default,
                    workspace.path().to_path_buf(),
                ));
                engine.add_rules(ApprovalDecision::AllowAlways { scope }.rules_for(&request));
                assert!(
                    engine.evaluate(&request).is_ask(),
                    "{relative}: {scope:?} 批准不能绕过安全检查"
                );
            }
        }
    }

    #[test]
    fn powershell_and_bash_share_the_execute_policy() {
        let cwd = std::env::temp_dir();
        let engine = PermissionEngine::new(PermissionPolicy::new(PermissionMode::ReadOnly, &cwd));
        for tool in ["bash", "powershell"] {
            let request = PermissionRequest::from_tool_call(
                tool,
                &serde_json::json!({"command": "echo hi"}),
                &cwd,
            );
            assert!(
                engine.evaluate(&request).is_deny(),
                "{tool} must follow the same read-only policy as bash"
            );
        }
        let engine = PermissionEngine::new(PermissionPolicy::new(
            PermissionMode::BypassPermissions,
            &cwd,
        ));
        for tool in ["bash", "powershell"] {
            let request = PermissionRequest::from_tool_call(
                tool,
                &serde_json::json!({"command": "echo hi"}),
                &cwd,
            );
            assert!(
                engine.evaluate(&request).is_allow(),
                "{tool} must follow the same bypass policy as bash"
            );
        }
    }
}
