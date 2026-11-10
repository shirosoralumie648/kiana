//! per-tool 审批卡。
//!
//! # 卡片内容
//!
//! | 工具 | 显示 |
//! |---|---|
//! | Bash / PowerShell | 完整命令、重定向（`>`/`>>`/`<`/`2>&1`/heredoc）、子命令（`&&`/`\|\|`/`;`/`\|`/换行） |
//! | FileEdit | 文件路径 + 行级 diff（`-` 删除 / `+` 新增 / 上下文） |
//! | FileWrite | 文件路径 + 覆盖/新建 + 写入内容 |
//! | WebFetch | 只显示域名（不显示 path/query，避免泄露查询参数） |
//! | MCP | server / tool + 脱敏参数 |
//! | Skill | skill 名 + prompt 预览 |
//! | 其他 | 工具名 + 脱敏 JSON |
//!
//! # 决策面
//!
//! * suggestions 固定为 `Allow once` → `Always allow {scope}`（每个可用作用域一条）→
//!   `Reject`；作用域来自 [`ApprovalRequest::always_scopes`]。
//! * decision reason 来自权限引擎（[`ApprovalRequest::decision_reason`]），卡片原样展示，
//!   保证 UI 与引擎给出的理由一致。
//! * 风险解释是可选 **advisory**：`begin_risk` → `resolve_risk`。失败静默隐藏；
//!   3 秒内没有结果也会隐藏（[`RISK_ADVISORY_TIMEOUT_MS`]），不阻塞审批。
//! * 危险动作默认把焦点放在 `Reject`；Enter 不会批准危险动作（只能显式移动到
//!   Allow 项再按 Enter 之外的确认键——本实现里 Enter 在危险动作上直接忽略）。
//!
//! # 与参考实现的关系
//!
//! 卡片分派参考 ClaudeCode `src/components/permissions/**`（Bash/FileEdit/FileWrite/
//! WebFetch/Skill/Fallback 各自的请求组件）与 `permissionExplainer.ts` 的
//! `riskLevel/explanation/reasoning/risk` 结构；交互结果参考第 1 周已落地的
//! `ApprovalDecision`。没有复制其代码。

use std::path::Path;

use kiana_tui::text::truncate_to_width;
use kiana_tui::tui::Component;

use crate::render_core::i18n::Translator;

/// 审批作用域（与第 1 周的 `ApprovalScope` 对齐）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApprovalScope {
    /// 仅本会话（内存）。
    Session,
    /// 本机（`settings.local.json`）。
    Local,
    /// 本工程（`settings.json`）。
    Project,
}

impl ApprovalScope {
    /// 英文展示标签。
    pub fn label(self) -> &'static str {
        match self {
            Self::Session => "this session",
            Self::Local => "this machine",
            Self::Project => "this project",
        }
    }
}

/// 建议的类型。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApprovalSuggestionKind {
    /// 只允许这一次。
    AllowOnce,
    /// 总是允许（指定作用域）。
    AllowAlways(ApprovalScope),
    /// 拒绝。
    Reject,
}

/// 一条建议。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApprovalSuggestion {
    /// 类型。
    pub kind: ApprovalSuggestionKind,
    /// 展示标签。
    pub label: String,
}

/// 风险等级（advisory）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RiskLevel {
    /// 低。
    Low,
    /// 中。
    Medium,
    /// 高。
    High,
}

impl RiskLevel {
    /// 展示文本。
    pub fn label(self) -> &'static str {
        match self {
            Self::Low => "LOW",
            Self::Medium => "MEDIUM",
            Self::High => "HIGH",
        }
    }
}

/// sideQuery 返回的风险解释。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RiskAdvisory {
    /// 等级。
    pub level: RiskLevel,
    /// 这条命令做什么。
    pub explanation: String,
    /// 为什么运行它。
    pub reasoning: String,
    /// 可能出什么问题。
    pub risk: String,
}

/// 风险解释的异步状态。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RiskAdvisoryState {
    /// 未请求 / 已隐藏。
    Hidden,
    /// 分析中。
    Loading {
        /// 开始时间（毫秒）。
        started_ms: i64,
    },
    /// 有结果。
    Shown(RiskAdvisory),
}

/// 风险解释 3 秒超时（没有结果就隐藏）。
pub const RISK_ADVISORY_TIMEOUT_MS: i64 = 3_000;

/// 一个审批请求。
#[derive(Clone, Debug)]
pub struct ApprovalRequest {
    /// 请求 id。
    pub id: String,
    /// 工具名。
    pub tool_name: String,
    /// 原始参数。
    pub args: serde_json::Value,
    /// 工作目录（展示相对路径用）。
    pub cwd: String,
    /// 权限引擎给出的询问原因。
    pub decision_reason: String,
    /// 可用的 always 作用域。
    pub always_scopes: Vec<ApprovalScope>,
    /// 是否危险动作。
    pub dangerous: bool,
}

impl ApprovalRequest {
    /// 用工具名与参数构造请求；默认作用域为 Session/Project/Local。
    pub fn new(tool_name: impl Into<String>, args: serde_json::Value) -> Self {
        let tool_name = tool_name.into();
        Self {
            id: format!("approval-{tool_name}"),
            tool_name,
            args,
            cwd: String::new(),
            decision_reason: String::new(),
            always_scopes: vec![
                ApprovalScope::Session,
                ApprovalScope::Project,
                ApprovalScope::Local,
            ],
            dangerous: false,
        }
    }

    /// 设置请求 id。
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = id.into();
        self
    }

    /// 设置 cwd。
    pub fn with_cwd(mut self, cwd: impl Into<String>) -> Self {
        self.cwd = cwd.into();
        self
    }

    /// 设置引擎原因。
    pub fn with_decision_reason(mut self, reason: impl Into<String>) -> Self {
        self.decision_reason = reason.into();
        self
    }

    /// 设置危险标记。
    pub fn with_dangerous(mut self, dangerous: bool) -> Self {
        self.dangerous = dangerous;
        self
    }

    /// 设置可用作用域。
    pub fn with_always_scopes(mut self, scopes: Vec<ApprovalScope>) -> Self {
        self.always_scopes = scopes;
        self
    }

    /// 生成建议列表（Allow once → Always{scope} → Reject）。
    pub fn suggestions(&self) -> Vec<ApprovalSuggestion> {
        let mut suggestions = vec![ApprovalSuggestion {
            kind: ApprovalSuggestionKind::AllowOnce,
            label: "Allow once".to_string(),
        }];
        for scope in &self.always_scopes {
            suggestions.push(ApprovalSuggestion {
                kind: ApprovalSuggestionKind::AllowAlways(*scope),
                label: format!("Always allow ({})", scope.label()),
            });
        }
        suggestions.push(ApprovalSuggestion {
            kind: ApprovalSuggestionKind::Reject,
            label: "Reject".to_string(),
        });
        suggestions
    }
}

/// `approval.*` 语言包驱动的建议列表（顺序与 [`ApprovalRequest::suggestions`] 一致）。
fn localized_suggestions(
    request: &ApprovalRequest,
    translator: &Translator,
) -> Vec<ApprovalSuggestion> {
    let mut suggestions = vec![ApprovalSuggestion {
        kind: ApprovalSuggestionKind::AllowOnce,
        label: translator.t("approval.allow_once", &[]),
    }];
    for scope in &request.always_scopes {
        suggestions.push(ApprovalSuggestion {
            kind: ApprovalSuggestionKind::AllowAlways(*scope),
            label: translator.t("approval.allow_always", &[("scope", scope.label())]),
        });
    }
    suggestions.push(ApprovalSuggestion {
        kind: ApprovalSuggestionKind::Reject,
        label: translator.t("approval.reject", &[]),
    });
    suggestions
}

/// 用户选择后的决策。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApprovalDecision {
    /// 只允许一次。
    AllowOnce,
    /// 总是允许。
    AllowAlways(ApprovalScope),
    /// 拒绝（可选原因）。
    Reject {
        /// 原因。
        reason: Option<String>,
    },
}

/// 键盘事件。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApprovalKey {
    /// 上移。
    Up,
    /// 下移。
    Down,
    /// 确认。
    Enter,
    /// 取消 = 拒绝。
    Escape,
    /// 其他字符（数字直选）。
    Char(char),
}

/// 按键处理结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApprovalOutcome {
    /// 没有变化。
    Ignored,
    /// 焦点移动。
    Moved,
    /// 产生决定。
    Decision(ApprovalDecision),
}

/// Bash 分析结果。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BashAnalysis {
    /// 子命令（按执行/管道顺序）。
    pub subcommands: Vec<String>,
    /// 重定向（含操作符与目标）。
    pub redirects: Vec<String>,
}

/// 审批卡。
#[derive(Clone, Debug)]
pub struct ApprovalCard {
    /// 请求 id。
    pub request_id: String,
    /// 工具名。
    pub tool_name: String,
    /// 标题。
    pub title: String,
    /// 副标题（路径/域名/目标）。
    pub subtitle: String,
    /// 卡片主体行（不含建议与原因）。
    pub body: Vec<String>,
    /// 引擎原因。
    pub decision_reason: String,
    /// 建议。
    pub suggestions: Vec<ApprovalSuggestion>,
    /// 当前焦点下标。
    pub focused: usize,
    /// 风险解释状态。
    pub risk: RiskAdvisoryState,
    /// 是否危险动作。
    pub dangerous: bool,
    /// 本地化翻译器；`None` 时所有输出保持英文原文（[`ApprovalCard::build`]）。
    pub translator: Option<Translator>,
}

impl ApprovalCard {
    /// 从请求构造卡片（按工具分派主体）。
    pub fn build(request: &ApprovalRequest) -> Self {
        let (title, subtitle, body) = build_body(request);
        let suggestions = request.suggestions();
        let focused = if request.dangerous {
            suggestions
                .iter()
                .position(|suggestion| suggestion.kind == ApprovalSuggestionKind::Reject)
                .unwrap_or(suggestions.len().saturating_sub(1))
        } else {
            0
        };
        Self {
            request_id: request.id.clone(),
            tool_name: request.tool_name.clone(),
            title,
            subtitle,
            body,
            decision_reason: request.decision_reason.clone(),
            suggestions,
            focused,
            risk: RiskAdvisoryState::Hidden,
            dangerous: request.dangerous,
            translator: None,
        }
    }

    /// 构造本地化卡片：标签与固定文案走语言包，卡片主体（命令、路径等）保持原文。
    ///
    /// [`Self::build`] 的英文输出不变；本函数用 `approval.*` key 生成建议与提示，
    /// 并在 [`Self::render_lines`] 中替换标题/原因/风险/建议等固定文案。
    pub fn build_localized(request: &ApprovalRequest, translator: &Translator) -> Self {
        let mut card = Self::build(request);
        card.suggestions = localized_suggestions(request, translator);
        card.translator = Some(translator.clone());
        card
    }

    /// 用新的翻译器重译固定文案（标题/原因/风险/建议标签）。
    ///
    /// 建议标签在 `build_localized` 时就写进了 `ApprovalSuggestion.label`，运行期切换
    /// UI 语言时必须重算，否则已入队/已显示的审批卡会保留旧语言。
    pub fn relocalize(&mut self, translator: &Translator) {
        self.suggestions = self
            .suggestions
            .iter()
            .map(|suggestion| ApprovalSuggestion {
                kind: suggestion.kind,
                label: match suggestion.kind {
                    ApprovalSuggestionKind::AllowOnce => translator.t("approval.allow_once", &[]),
                    ApprovalSuggestionKind::AllowAlways(scope) => {
                        translator.t("approval.allow_always", &[("scope", scope.label())])
                    }
                    ApprovalSuggestionKind::Reject => translator.t("approval.reject", &[]),
                },
            })
            .collect();
        self.translator = Some(translator.clone());
    }

    /// 当前聚焦的建议。
    pub fn focused_suggestion(&self) -> Option<&ApprovalSuggestion> {
        self.suggestions.get(self.focused)
    }

    /// 上/下移动焦点（clamp，不环绕）。
    pub fn move_focus(&mut self, delta: isize) -> bool {
        if self.suggestions.is_empty() {
            return false;
        }
        let next = (self.focused as isize + delta)
            .clamp(0, self.suggestions.len().saturating_sub(1) as isize)
            as usize;
        if next == self.focused {
            return false;
        }
        self.focused = next;
        true
    }

    /// 把焦点移到指定类型的建议（第一个匹配）。
    pub fn focus_kind(&mut self, kind: ApprovalSuggestionKind) -> bool {
        let Some(index) = self
            .suggestions
            .iter()
            .position(|suggestion| suggestion.kind == kind)
        else {
            return false;
        };
        self.focused = index;
        true
    }

    /// 处理一次按键。危险动作上 Enter 不会批准。
    pub fn handle_key(&mut self, key: ApprovalKey) -> ApprovalOutcome {
        match key {
            ApprovalKey::Up => {
                if self.move_focus(-1) {
                    ApprovalOutcome::Moved
                } else {
                    ApprovalOutcome::Ignored
                }
            }
            ApprovalKey::Down => {
                if self.move_focus(1) {
                    ApprovalOutcome::Moved
                } else {
                    ApprovalOutcome::Ignored
                }
            }
            ApprovalKey::Escape => ApprovalOutcome::Decision(ApprovalDecision::Reject {
                reason: Some("Approval was cancelled.".to_string()),
            }),
            ApprovalKey::Char(character) => {
                if let Some(index) = character.to_digit(10) {
                    let index = index as usize;
                    if index >= 1 && index <= self.suggestions.len() {
                        self.focused = index - 1;
                        return self.handle_key(ApprovalKey::Enter);
                    }
                }
                ApprovalOutcome::Ignored
            }
            ApprovalKey::Enter => {
                let Some(suggestion) = self.focused_suggestion().cloned() else {
                    return ApprovalOutcome::Ignored;
                };
                match suggestion.kind {
                    ApprovalSuggestionKind::Reject => {
                        ApprovalOutcome::Decision(ApprovalDecision::Reject { reason: None })
                    }
                    ApprovalSuggestionKind::AllowOnce if !self.dangerous => {
                        ApprovalOutcome::Decision(ApprovalDecision::AllowOnce)
                    }
                    ApprovalSuggestionKind::AllowAlways(scope) if !self.dangerous => {
                        ApprovalOutcome::Decision(ApprovalDecision::AllowAlways(scope))
                    }
                    // 危险动作：Enter 不批准。用户必须显式选择 Reject（或使用其它
                    // 绑定，由接线层提供）。
                    _ => ApprovalOutcome::Ignored,
                }
            }
        }
    }

    /// 危险动作的显式确认入口（接线层应绑定到 Ctrl+Enter 等非 Enter 按键）。
    ///
    /// `Enter` 对危险动作永远返回 `Ignored`；只有这个显式方法能在焦点位于 Allow 项时
    /// 产生批准决定。
    pub fn confirm_dangerous(&self) -> Option<ApprovalDecision> {
        match self.focused_suggestion()?.kind {
            ApprovalSuggestionKind::AllowOnce => Some(ApprovalDecision::AllowOnce),
            ApprovalSuggestionKind::AllowAlways(scope) => {
                Some(ApprovalDecision::AllowAlways(scope))
            }
            ApprovalSuggestionKind::Reject => Some(ApprovalDecision::Reject { reason: None }),
        }
    }

    /// 开始异步风险解释。
    pub fn begin_risk(&mut self, now_ms: i64) {
        self.risk = RiskAdvisoryState::Loading { started_ms: now_ms };
    }

    /// 风险解释返回：成功展示；失败静默隐藏（不阻塞审批）。
    pub fn resolve_risk(&mut self, result: Result<RiskAdvisory, ()>) {
        self.risk = match result {
            Ok(advisory) => RiskAdvisoryState::Shown(advisory),
            Err(()) => RiskAdvisoryState::Hidden,
        };
    }

    /// 超时推进：分析超过 3 秒仍无结果就隐藏。
    pub fn tick(&mut self, now_ms: i64) -> bool {
        let hide = match &self.risk {
            RiskAdvisoryState::Loading { started_ms } => {
                now_ms.saturating_sub(*started_ms) >= RISK_ADVISORY_TIMEOUT_MS
            }
            _ => false,
        };
        if hide {
            self.risk = RiskAdvisoryState::Hidden;
        }
        hide
    }

    /// 渲染成纯文本行（按宽度截断）。
    ///
    /// 设置了 `translator`（[`Self::build_localized`]）时，标题/原因/风险/建议等固定文案
    /// 走语言包；`translator` 为 `None` 时输出与历史英文版本逐字一致。
    pub fn render_lines(&self, width: usize) -> Vec<String> {
        let translator = self.translator.as_ref();
        let mut lines = Vec::new();
        let title_label = translator
            .map(|translator| translator.t("approval.title", &[]))
            .unwrap_or_else(|| "Permission required".to_string());
        lines.push(format!("{title_label}: {}", self.title));
        if !self.subtitle.is_empty() {
            lines.push(format!("  {}", self.subtitle));
        }
        for line in &self.body {
            lines.push(format!("  {line}"));
        }
        if !self.decision_reason.is_empty() {
            let reason_label = translator
                .map(|translator| translator.t("approval.reason", &[]))
                .unwrap_or_else(|| "Reason".to_string());
            lines.push(format!("{reason_label}: {}", self.decision_reason));
        }
        match &self.risk {
            RiskAdvisoryState::Hidden => {}
            RiskAdvisoryState::Loading { .. } => {
                lines.push(
                    translator
                        .map(|translator| translator.t("approval.risk_loading", &[]))
                        .unwrap_or_else(|| "Risk analysis: analyzing... (advisory)".to_string()),
                );
            }
            RiskAdvisoryState::Shown(advisory) => {
                let risk_label = translator
                    .map(|translator| translator.t("approval.risk_advisory", &[]))
                    .unwrap_or_else(|| "Risk (advisory)".to_string());
                lines.push(format!(
                    "{risk_label}: {} — {}",
                    advisory.level.label(),
                    advisory.explanation
                ));
                if !advisory.risk.is_empty() {
                    let what_label = translator
                        .map(|translator| translator.t("approval.risk_what", &[]))
                        .unwrap_or_else(|| "What could go wrong".to_string());
                    lines.push(format!("  {what_label}: {}", advisory.risk));
                }
            }
        }
        lines.push(
            translator
                .map(|translator| translator.t("approval.suggestions", &[]))
                .unwrap_or_else(|| "Suggestions".to_string()),
        );
        for (index, suggestion) in self.suggestions.iter().enumerate() {
            let marker = if index == self.focused { ">" } else { " " };
            lines.push(format!("{marker} {}. {}", index + 1, suggestion.label));
        }
        if self.dangerous {
            lines.push(
                translator
                    .map(|translator| translator.t("approval.dangerous", &[]))
                    .unwrap_or_else(|| {
                        "Dangerous action: default selection is Reject; Enter does not approve."
                            .to_string()
                    }),
            );
        }
        lines
            .into_iter()
            .map(|line| truncate_to_width(&line, width, "...", false))
            .collect()
    }
}

/// `Component` 适配层：把审批卡挂进现有 TUI。
#[derive(Clone, Debug)]
pub struct ApprovalCardComponent {
    /// 卡片。
    pub card: ApprovalCard,
    /// 最近一次按键结果。
    pub last_outcome: ApprovalOutcome,
}

impl ApprovalCardComponent {
    /// 包装一张卡片。
    pub fn new(card: ApprovalCard) -> Self {
        Self {
            card,
            last_outcome: ApprovalOutcome::Ignored,
        }
    }

    /// 已产生的决定（如果用户已经选择）。
    pub fn decision(&self) -> Option<&ApprovalDecision> {
        match &self.last_outcome {
            ApprovalOutcome::Decision(decision) => Some(decision),
            _ => None,
        }
    }
}

impl Component for ApprovalCardComponent {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.card.render_lines(width)
    }

    fn handle_input(&mut self, data: &str) {
        let key = if data == "\x1b[A" || data == "k" {
            Some(ApprovalKey::Up)
        } else if data == "\x1b[B" || data == "j" {
            Some(ApprovalKey::Down)
        } else if data == "\r" || data == "\n" {
            Some(ApprovalKey::Enter)
        } else if data == "\x1b" {
            Some(ApprovalKey::Escape)
        } else if data.chars().count() == 1 {
            data.chars().next().map(ApprovalKey::Char)
        } else {
            None
        };
        if let Some(key) = key {
            self.last_outcome = self.card.handle_key(key);
        }
    }

    fn handles_input(&self) -> bool {
        true
    }
}

fn build_body(request: &ApprovalRequest) -> (String, String, Vec<String>) {
    let name = request.tool_name.to_ascii_lowercase();
    let tool_name = request.tool_name.as_str();
    if name == "bash" || name == "powershell" || name == "shell" {
        let command = string_arg(&request.args, &["command", "cmd", "script"]).unwrap_or_default();
        let analysis = analyze_bash(&command);
        let mut body = vec!["Command:".to_string()];
        body.extend(command.lines().map(|line| format!("| {line}")));
        if !analysis.redirects.is_empty() {
            body.push("Redirects:".to_string());
            body.extend(analysis.redirects.iter().map(|item| format!("  {item}")));
        }
        if analysis.subcommands.len() > 1 {
            body.push("Subcommands:".to_string());
            for (index, item) in analysis.subcommands.iter().enumerate() {
                body.push(format!("  {}. {item}", index + 1));
            }
        }
        return (
            format!("Run shell command ({tool_name})"),
            String::new(),
            body,
        );
    }

    if matches!(
        name.as_str(),
        "edit" | "file_edit" | "edit_file" | "str_replace"
    ) {
        let path = string_arg(&request.args, &["file_path", "path", "file"]).unwrap_or_default();
        let old = string_arg(&request.args, &["old_string", "old_str", "old"]).unwrap_or_default();
        let new = string_arg(&request.args, &["new_string", "new_str", "new"]).unwrap_or_default();
        let mut body = diff_body(&old, &new);
        if body.is_empty() {
            body.push("(no textual changes)".to_string());
        }
        return (
            format!("Edit file ({tool_name})"),
            display_path(&path, &request.cwd),
            body,
        );
    }

    if matches!(name.as_str(), "write" | "file_write" | "write_file") {
        let path = string_arg(&request.args, &["file_path", "path", "file"]).unwrap_or_default();
        let content = string_arg(&request.args, &["content", "text"]).unwrap_or_default();
        let exists = request
            .args
            .get("file_exists")
            .and_then(serde_json::Value::as_bool)
            .or_else(|| {
                request
                    .args
                    .get("old_content")
                    .map(|value| !value.is_null())
            })
            .unwrap_or(true);
        let action = if exists { "Overwrite" } else { "Create" };
        let mut body = vec![format!("Action: {action} file")];
        let lines: Vec<&str> = content.lines().collect();
        for line in lines.iter().take(40) {
            body.push(format!("| {line}"));
        }
        if lines.len() > 40 {
            body.push(format!("| ... {} more lines", lines.len() - 40));
        }
        if lines.is_empty() {
            body.push("| (empty content)".to_string());
        }
        return (
            format!("Write file ({tool_name})"),
            display_path(&path, &request.cwd),
            body,
        );
    }

    if matches!(name.as_str(), "webfetch" | "web_fetch" | "fetch") {
        let url = string_arg(&request.args, &["url"]).unwrap_or_default();
        let domain = url::Url::parse(&url)
            .ok()
            .and_then(|parsed| parsed.host_str().map(str::to_string))
            .unwrap_or_else(|| "(invalid url)".to_string());
        let body = vec![format!("Domain: {domain}")];
        return (format!("Fetch URL ({tool_name})"), domain, body);
    }

    if name.starts_with("mcp__") {
        let rest = name.trim_start_matches("mcp__");
        let (server, tool) = rest.split_once("__").unwrap_or((rest, ""));
        let mut body = vec![
            format!("MCP server: {server}"),
            format!("Tool: {}", if tool.is_empty() { rest } else { tool }),
        ];
        body.push(format!("Arguments: {}", summarize_args(&request.args)));
        return ("MCP tool call".to_string(), String::new(), body);
    }

    if name == "skill" || name.ends_with("__skill") {
        let skill =
            string_arg(&request.args, &["skill", "name"]).unwrap_or_else(|| tool_name.to_string());
        let prompt =
            string_arg(&request.args, &["prompt", "input", "arguments"]).unwrap_or_default();
        let mut body = vec![format!("Skill: {skill}")];
        if !prompt.is_empty() {
            body.push("Prompt:".to_string());
            let preview: String = prompt.chars().take(300).collect();
            body.extend(preview.lines().map(|line| format!("| {line}")));
            if prompt.chars().count() > 300 {
                body.push("| ...".to_string());
            }
        }
        return ("Skill invocation".to_string(), String::new(), body);
    }

    (
        format!("Tool call ({tool_name})"),
        String::new(),
        vec![format!("Arguments: {}", summarize_args(&request.args))],
    )
}

fn string_arg(args: &serde_json::Value, keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Some(value) = args.get(*key).and_then(serde_json::Value::as_str) {
            return Some(value.to_string());
        }
    }
    None
}

fn display_path(path: &str, cwd: &str) -> String {
    let path = Path::new(path);
    if cwd.is_empty() {
        return path.display().to_string();
    }
    path.strip_prefix(cwd)
        .map(|relative| relative.display().to_string())
        .unwrap_or_else(|_| path.display().to_string())
}

/// 简化行 diff：公共前缀/后缀之外的删除行与新增行。
fn diff_body(old: &str, new: &str) -> Vec<String> {
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();
    if old_lines == new_lines {
        return Vec::new();
    }
    let mut prefix = 0usize;
    while prefix < old_lines.len()
        && prefix < new_lines.len()
        && old_lines[prefix] == new_lines[prefix]
    {
        prefix += 1;
    }
    let mut suffix = 0usize;
    while suffix < old_lines.len().saturating_sub(prefix)
        && suffix < new_lines.len().saturating_sub(prefix)
        && old_lines[old_lines.len() - 1 - suffix] == new_lines[new_lines.len() - 1 - suffix]
    {
        suffix += 1;
    }
    let mut body = Vec::new();
    for line in old_lines[..prefix].iter().take(2) {
        body.push(format!("  {line}"));
    }
    for line in &old_lines[prefix..old_lines.len() - suffix] {
        body.push(format!("- {line}"));
    }
    for line in &new_lines[prefix..new_lines.len() - suffix] {
        body.push(format!("+ {line}"));
    }
    for line in new_lines[new_lines.len() - suffix..].iter().take(2) {
        body.push(format!("  {line}"));
    }
    body
}

/// 解析 shell 命令：引号外的 `&&`/`||`/`;`/`|`/换行拆子命令，重定向单独提取。
pub fn analyze_bash(command: &str) -> BashAnalysis {
    let mut subcommands = Vec::new();
    let mut redirects = Vec::new();
    let mut start = 0usize;
    let mut index = 0usize;
    let bytes = command.as_bytes();
    let mut quote: Option<u8> = None;
    let mut escaped = false;
    let push_subcommand = |from: usize, to: usize, subcommands: &mut Vec<String>| {
        let text = command[from..to].trim();
        if !text.is_empty() {
            subcommands.push(text.to_string());
        }
    };
    while index < bytes.len() {
        let byte = bytes[index];
        if escaped {
            escaped = false;
            index += 1;
            continue;
        }
        if let Some(active) = quote {
            if byte == b'\\' && active == b'"' {
                escaped = true;
            } else if byte == active {
                quote = None;
            }
            index += 1;
            continue;
        }
        match byte {
            b'\'' | b'"' => {
                quote = Some(byte);
                index += 1;
            }
            b'\\' => {
                escaped = true;
                index += 1;
            }
            b'>' | b'<' => {
                // 重定向：从前面的数字（如 2>）或 & 开始，到目标 token 结束。
                let mut operator_start = index;
                while operator_start > start
                    && command.as_bytes()[operator_start - 1].is_ascii_digit()
                {
                    operator_start -= 1;
                }
                if operator_start == index && index > 0 && bytes[index - 1] == b'&' {
                    operator_start = index - 1;
                }
                let mut operator_end = index + 1;
                if operator_end < bytes.len()
                    && matches!(bytes[operator_end], b'>' | b'<' | b'&' | b'|')
                {
                    operator_end += 1;
                }
                let mut target_start = operator_end;
                while target_start < bytes.len() && bytes[target_start].is_ascii_whitespace() {
                    target_start += 1;
                }
                let mut target_end = target_start;
                let target_slice_end;
                if target_start < bytes.len() && matches!(bytes[target_start], b'\'' | b'"') {
                    let quote = bytes[target_start];
                    target_start += 1;
                    target_end = target_start;
                    while target_end < bytes.len() && bytes[target_end] != quote {
                        target_end += 1;
                    }
                    target_slice_end = target_end;
                    target_end = (target_end + 1).min(bytes.len());
                } else {
                    while target_end < bytes.len()
                        && !bytes[target_end].is_ascii_whitespace()
                        && !matches!(
                            bytes[target_end],
                            b';' | b'|' | b'&' | b'<' | b'>' | b')' | b'('
                        )
                    {
                        target_end += 1;
                    }
                    target_slice_end = target_end;
                }
                let target = command[target_start..target_slice_end].trim();
                let operator = command[operator_start..operator_end].trim();
                if !target.is_empty() {
                    // `2>&1` / `>&2` 是文件描述符复制：`&` 后的数字属于操作符本身，
                    // 不是目标文件。按原样拼回，避免渲染成 `2>& 1`。
                    if operator.ends_with('&') && target.chars().all(|ch| ch.is_ascii_digit()) {
                        redirects.push(format!("{operator}{target}"));
                    } else {
                        redirects.push(format!("{operator} {target}"));
                    }
                } else {
                    redirects.push(operator.to_string());
                }
                index = target_end;
            }
            b';' => {
                push_subcommand(start, index, &mut subcommands);
                start = index + 1;
                index += 1;
            }
            b'\n' => {
                push_subcommand(start, index, &mut subcommands);
                start = index + 1;
                index += 1;
            }
            b'|' => {
                if index + 1 < bytes.len() && bytes[index + 1] == b'|' {
                    push_subcommand(start, index, &mut subcommands);
                    start = index + 2;
                    index += 2;
                } else {
                    push_subcommand(start, index, &mut subcommands);
                    start = index + 1;
                    index += 1;
                }
            }
            b'&' => {
                if index + 1 < bytes.len() && bytes[index + 1] == b'&' {
                    push_subcommand(start, index, &mut subcommands);
                    start = index + 2;
                    index += 2;
                } else {
                    push_subcommand(start, index, &mut subcommands);
                    start = index + 1;
                    index += 1;
                }
            }
            _ => index += 1,
        }
    }
    push_subcommand(start, bytes.len(), &mut subcommands);
    BashAnalysis {
        subcommands,
        redirects,
    }
}

/// 脱敏 JSON：敏感键递归替换为 `"[redacted]"`。
pub fn redact_args(value: &serde_json::Value) -> serde_json::Value {
    redact_value(value, 0)
}

fn redact_value(value: &serde_json::Value, depth: usize) -> serde_json::Value {
    if depth > 8 {
        return serde_json::Value::String("[truncated]".to_string());
    }
    match value {
        serde_json::Value::Object(map) => {
            let mut redacted = serde_json::Map::new();
            for (key, value) in map {
                let lower = key.to_ascii_lowercase();
                let sensitive = [
                    "password",
                    "passwd",
                    "secret",
                    "token",
                    "api_key",
                    "apikey",
                    "authorization",
                    "credential",
                    "private_key",
                ]
                .iter()
                .any(|needle| lower.contains(needle));
                redacted.insert(
                    key.clone(),
                    if sensitive {
                        serde_json::Value::String("[redacted]".to_string())
                    } else {
                        redact_value(value, depth + 1)
                    },
                );
            }
            serde_json::Value::Object(redacted)
        }
        serde_json::Value::Array(values) => serde_json::Value::Array(
            values
                .iter()
                .map(|value| redact_value(value, depth + 1))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn summarize_args(args: &serde_json::Value) -> String {
    let redacted = redact_args(args);
    let mut text =
        serde_json::to_string(&redacted).unwrap_or_else(|_| "[unserializable]".to_string());
    if text.chars().count() > 500 {
        text = text.chars().take(497).collect::<String>() + "...";
    }
    text
}
