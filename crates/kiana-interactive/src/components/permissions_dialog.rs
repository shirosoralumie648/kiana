//! `/permissions` 面板：权限模式、规则与工作区目录的检查/编辑界面。
//!
//! 交互骨架照搬 qwen-code `packages/cli/src/ui/components/PermissionsDialog.tsx`
//! （四个标签页 + 内部视图状态机），规则模型与持久化语义按 Kiana 的权限体系重写。
//! TS 参照实现没有 `/permissions` 命令，本组件是产品增量，见
//! `docs/history/porting/tui-panels-design.md` §3。
//!
//! # 这个模块解决什么问题
//!
//! 权限模式、allow/ask/deny 规则和工作区目录分散在四层来源（managed/CLI、user、project、
//! local settings），用户看不到合并结果，也很难安全地修改。本组件把快照按决策分组展示，
//! 并给出新增/删除的引导流程：
//!
//! - 规则标签页（Allow / Ask / Deny）列出**有效**规则及其来源作用域；
//! - Workspace 标签页列出可写工作区根（第一项是会话 cwd，永远不可删）；
//! - `m` 切换权限模式（read-only/default/accept-edits/bypass），再选择持久化作用域。
//!
//! # 职责边界（为什么组件不直接读写 settings）
//!
//! 组件只持有**快照**与**视图状态机**：
//!
//! - 快照 [`PermissionSnapshot`] 由命令层从 `PermissionEngine::policy()` 与三层 settings
//!   组装（见 [`PermissionSnapshot::build`] 与 [`collect_settings_layers`]）。组件不读文件：
//!   文件可能损坏、项目可能未受信，而引擎已经完成了分层合并、`null` 语义与路径归一化。
//! - 所有写操作（新增/删除规则、增删目录、切换模式）都通过 [`PermissionsCallbacks`] 交回
//!   宿主。宿主写盘并刷新运行期引擎后调用 [`PermissionsDialog::set_snapshot`] 回填；
//!   写盘失败可以把错误文本通过 [`PermissionsDialog::set_notice`] 显示出来。
//! - 视图状态（标签页、输入串、确认选择）全部在组件内部，宿主不需要知道当前在哪一步。
//!   这与 `/settings` 的分层一致：命令模块只组装快照与回调。
//!
//! # 状态机
//!
//! ```text
//! RuleList ──m──> ModeSelect ──Enter──> ModeScopeSelect ──Enter──> RuleList
//!    │                └──────── Esc 逐级回退 ────────┘
//!    ├─Enter(Add rule)/a──> AddRuleInput ──Enter──> AddRuleScope ──Enter──> RuleList
//!    ├─Enter(rule row)/d──> DeleteRuleConfirm ──Enter(Yes)──> RuleList
//!    └─Tab/←/→──> Ask / Deny / WorkspaceList
//!
//! WorkspaceList ──Enter(Add directory)──> AddDirectoryInput ──Enter──> AddDirectoryScope
//!    │                                                └────────Esc────> WorkspaceList
//!    └─Enter(dir row)/d──> RemoveDirectoryConfirm ──Enter(Yes)──> WorkspaceList
//! ```
//!
//! 设计文档 §3.3 的 `PermissionsView` 省略了模式与目录的作用域选择视图；§3.8/§3.7 又要求
//! “先选作用域再写盘”，因此这里补上 [`PermissionsView::ModeScopeSelect`] 与
//! [`PermissionsView::AddDirectoryScope`]。其余变体与设计文档逐字对应。
//!
//! # 键盘约定
//!
//! 命令键（`m`/`r`/`d`/`a`）只在**非过滤模式**生效；按 `/` 进入过滤后，所有可打印字符
//! （包括 `m`/`d`）都追加到查询串。这是必要的：规则文本里包含这些字母，若无条件把
//! 可打印字符当命令，输入 `mcp` 就会误开模式选择。过滤用大小写不敏感的**子串**匹配
//! （`SelectList::set_filter` 是前缀匹配，且 `value` 用来做稳定 id，不能拿来搜索）。
//!
//! # 边界与失败模式
//!
//! - 回调返回 `Err(String)`：输入/确认视图保留并显示错误行，宿主可以原地重试；写盘失败
//!   **不**更新运行期（由宿主保证顺序）。
//! - 权限层关闭（`snapshot.mode == None`）：新增规则/目录被拒绝（fail-closed），必须先
//!   用 `m` 切换模式；切换模式本身允许，因为 `session.set_permission_mode` 会创建引擎。
//! - 项目未受信：Project/Local 作用域行显示禁用原因，Enter 只报错、不调用回调。
//! - `ManagedOrCli` 来源的规则/目录不可删除；确认视图只显示警告，Enter 只设置 notice。
//! - 渲染依赖全局主题已初始化；未初始化时退化为无颜色文本而不是 panic（与 footer 的
//!   `expect` 不同，面板不承担终端启动路径的失败面）。测试仍会先 `init_theme`。
//! - 组件不接收鼠标事件：P0 只保证键盘（见设计文档 §3.4 末尾）。
//!
//! TS 源：`packages/cli/src/ui/components/PermissionsDialog.tsx`（qwen-code，只读参考）。

use std::any::Any;
use std::path::{Path, PathBuf};

use kiana_config::settings::SettingsManager;
use kiana_config::settings::permissions as permissions_settings;
use kiana_permissions::{
    PermissionEngine, PermissionFlags, PermissionMode, PermissionRule, RuleDecision, parse_cli_rule,
};
use kiana_theme::{THEME, ThemeColor};
use kiana_tui::keybindings::get_keybindings;
use kiana_tui::text::{truncate_to_width_default, visible_width};
use kiana_tui::tui::{Component, Focusable};
use serde_json::{Map, Value};

use crate::components::approval_mode_indicator::permission_mode_visual;

/// 列表一次 PageUp/PageDown 移动的行数。
const PAGE_SIZE: usize = 8;

/// 规则标签页（workspace 是同一面板里的第四个标签，不是规则决策）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RuleTab {
    /// Allow 规则。
    Allow,
    /// Ask 规则。
    Ask,
    /// Deny 规则。
    Deny,
    /// 工作区目录。
    Workspace,
}

impl RuleTab {
    /// 标签顺序（Tab 正向循环）。
    pub const ALL: [RuleTab; 4] = [
        RuleTab::Allow,
        RuleTab::Ask,
        RuleTab::Deny,
        RuleTab::Workspace,
    ];

    /// 标签文本。
    pub fn label(self) -> &'static str {
        match self {
            Self::Allow => "Allow",
            Self::Ask => "Ask",
            Self::Deny => "Deny",
            Self::Workspace => "Workspace",
        }
    }

    /// 对应的规则决策；Workspace 不是规则页，返回 `None`。
    pub fn decision(self) -> Option<RuleDecision> {
        match self {
            Self::Allow => Some(RuleDecision::Allow),
            Self::Ask => Some(RuleDecision::Ask),
            Self::Deny => Some(RuleDecision::Deny),
            Self::Workspace => None,
        }
    }

    /// 空列表提示。
    pub fn empty_message(self) -> &'static str {
        match self {
            Self::Allow => "No allow rules.",
            Self::Ask => "No ask rules.",
            Self::Deny => "No deny rules.",
            Self::Workspace => "No workspace directories.",
        }
    }

    /// 下标（[`Self::ALL`] 顺序）。
    pub fn index(self) -> usize {
        match self {
            Self::Allow => 0,
            Self::Ask => 1,
            Self::Deny => 2,
            Self::Workspace => 3,
        }
    }

    /// 从下标还原；越界回绕到 Allow。
    pub fn from_index(index: isize) -> Self {
        let len = Self::ALL.len() as isize;
        Self::ALL[(index.rem_euclid(len)) as usize]
    }
}

/// 一条规则的文件来源作用域。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PermissionRuleScope {
    /// `<agent-dir>/settings.json`。
    User,
    /// `<cwd>/.kiana/settings.json`（项目受信时才可写）。
    Project,
    /// `<cwd>/.kiana/settings.local.json`（项目受信时才可写）。
    Local,
    /// 文件里找不到来源：managed policy 或 `--allow/--deny`；不可在此删除。
    ManagedOrCli,
}

impl PermissionRuleScope {
    /// 稳定短标签。
    pub fn label(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Project => "project",
            Self::Local => "local",
            Self::ManagedOrCli => "managed/cli",
        }
    }

    /// 是否是可编辑的文件作用域。
    pub fn is_editable(self) -> bool {
        !matches!(self, Self::ManagedOrCli)
    }
}

/// 一条有效规则及其来源/可删除性。
#[derive(Debug, Clone, PartialEq)]
pub struct PermissionRuleEntry {
    /// 解析后的规则（引擎实际执行的形态）。
    pub rule: PermissionRule,
    /// 规则的规范 JSON 形态（[`PermissionRule::to_settings_value`]），删除/写回时用。
    ///
    /// 注意：磁盘上的简写（`permissions.allow` 里的字符串）可能与该对象形态不同，
    /// 因此删除实现应按 [`Self::rule`] 的等值匹配逐层清理，而不是只比较本字段。
    pub raw: Value,
    /// 决策（与 `rule.decision()` 相同；便于按标签页分组）。
    pub decision: RuleDecision,
    /// 出现该规则的文件作用域（去重、按 user → project → local 的顺序）。
    /// 找不到来源时是 `[ManagedOrCli]`。
    pub scopes: Vec<PermissionRuleScope>,
    /// 是否可以在本面板删除（来源全部可编辑）。
    pub removable: bool,
}

/// 权限快照：面板渲染所需的全部只读数据。
///
/// 对应设计文档 §3.2 的 `PermissionSnapshot`；额外增加 [`Self::workspace_scopes`]，
/// 因为 `workspace_roots` 只是裸路径，UI 需要来源标注才能显示“这个目录来自哪里”。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PermissionSnapshot {
    /// 当前模式；`None` 表示权限层关闭（off）。
    pub mode: Option<PermissionMode>,
    /// managed policy 是否禁用 `bypass`。
    pub bypass_disabled: bool,
    /// 引擎里的有效规则（按规范 JSON 去重）。
    pub rules: Vec<PermissionRuleEntry>,
    /// 可写工作区根；第一项是会话 cwd，不可删除。
    pub workspace_roots: Vec<PathBuf>,
    /// 与 [`Self::workspace_roots`] 等长的来源标注；无法归因的根是 `[ManagedOrCli]`。
    pub workspace_scopes: Vec<Vec<PermissionRuleScope>>,
    /// 本会话「总是允许」的条目数。
    pub session_allow_count: usize,
    /// 文件读取/解析错误（头部 warning 展示，不阻断其余数据）。
    pub errors: Vec<String>,
}

/// 一层 settings 里的原始 `permissions` 对象。
#[derive(Debug, Clone, PartialEq)]
pub struct PermissionSettingsLayer {
    /// 该层的作用域。
    pub scope: PermissionRuleScope,
    /// `permissions` 对象；文件/字段不存在时是 `None`。
    pub permissions: Option<Value>,
}

impl PermissionSettingsLayer {
    /// 构造一层。
    pub fn new(scope: PermissionRuleScope, permissions: Option<Value>) -> Self {
        Self { scope, permissions }
    }
}

/// [`PermissionSnapshot::build`] 的输入。
#[derive(Debug, Clone, Default)]
pub struct PermissionSnapshotInput {
    /// 会话工作目录（相对路径/`~` 的解析基准，也是默认的第一工作区根）。
    pub cwd: PathBuf,
    /// 当前模式；`None` = 权限层关闭。
    pub mode: Option<PermissionMode>,
    /// managed policy 是否禁用 `bypass`。
    pub bypass_disabled: bool,
    /// 引擎里的有效规则。
    pub policy_rules: Vec<PermissionRule>,
    /// 引擎里的工作区根（引擎已做路径归一化；留空时回退到 `[cwd]`）。
    pub workspace_roots: Vec<PathBuf>,
    /// 本会话「总是允许」的条目数。
    pub session_allow_count: usize,
    /// settings 文件层（user/project/local）。
    pub layers: Vec<PermissionSettingsLayer>,
    /// 已知的文件读取/解析错误。
    pub errors: Vec<String>,
}

impl PermissionSnapshot {
    /// 从引擎策略与 settings 层组装快照。
    ///
    /// 算法（确定性，供测试锁定）：
    /// 1. 遍历每层 `permissions`，解析 `rules` 数组（走与启动路径相同的引擎解析）与
    ///    `allow/deny/ask` 简写（[`parse_cli_rule`]），得到 `(scope, raw, rule)`；
    ///    解析失败记入 `errors`，不阻断其余层。
    /// 2. 遍历 `policy_rules`（引擎的有效规则），按 [`PermissionRule::to_settings_value`]
    ///    去重；对每条规则收集在所有层里出现的作用域。找不到来源时标 `ManagedOrCli`。
    /// 3. 工作区根：第一项是 cwd；其余根按各层 `addDirs`/`workspaceRoots`/
    ///    `additionalDirectories` 的原始字符串（归一化后比较）归因，找不到则
    ///    `ManagedOrCli`。
    ///
    /// `removable` 只由来源决定，不判断“删除后是否还有低层同名规则”；后者由宿主在
    /// 重新取快照时自然体现。
    pub fn build(input: PermissionSnapshotInput) -> Self {
        let PermissionSnapshotInput {
            cwd,
            mode,
            bypass_disabled,
            policy_rules,
            workspace_roots,
            session_allow_count,
            layers,
            mut errors,
        } = input;

        // 1. 解析文件层。这里的错误是“文件内容有问题”，与写盘错误分开累积。
        let mut layer_rules: Vec<(PermissionRuleScope, Value, PermissionRule)> = Vec::new();
        for layer in &layers {
            // `{"permissions": null}` 与“没有配置”等价（与 `read_permissions_file` 一致），
            // 不能当成形状错误。
            let Some(permissions) = &layer.permissions else {
                continue;
            };
            if permissions.is_null() {
                continue;
            }
            let Some(object) = permissions.as_object() else {
                errors.push(format!(
                    "permissions in {} settings must be a JSON object",
                    layer.scope.label()
                ));
                continue;
            };
            if let Some(value) = object.get("rules") {
                let Some(items) = value.as_array() else {
                    errors.push(format!(
                        "permissions.rules in {} settings must be an array",
                        layer.scope.label()
                    ));
                    continue;
                };
                for raw in items {
                    match parse_settings_rule_value(raw, &cwd) {
                        Some(rule) => layer_rules.push((layer.scope, raw.clone(), rule)),
                        None => errors.push(format!(
                            "Ignored malformed {} rule: {}",
                            layer.scope.label(),
                            raw
                        )),
                    }
                }
            }
            for (key, decision) in [
                ("allow", RuleDecision::Allow),
                ("deny", RuleDecision::Deny),
                ("ask", RuleDecision::Ask),
            ] {
                let Some(value) = object.get(key) else {
                    continue;
                };
                let Some(items) = value.as_array() else {
                    errors.push(format!(
                        "permissions.{key} in {} settings must be an array",
                        layer.scope.label()
                    ));
                    continue;
                };
                for item in items {
                    let Some(text) = item.as_str() else {
                        errors.push(format!(
                            "Ignored malformed {} {key} entry: {item}",
                            layer.scope.label()
                        ));
                        continue;
                    };
                    match parse_shorthand_rule_value(text, decision, &cwd) {
                        Some(rule) => layer_rules.push((layer.scope, item.clone(), rule)),
                        None => errors.push(format!(
                            "Ignored malformed {} {key} rule: {text}",
                            layer.scope.label()
                        )),
                    }
                }
            }
        }

        // 2. 有效规则 → 条目。`to_settings_value()` 是规则的规范键：同一规则出现在多层
        // 时它相等，因此既用来去重也用来写回/删除。
        let mut seen: Vec<Value> = Vec::new();
        let mut rules = Vec::new();
        for rule in policy_rules {
            let raw = rule.to_settings_value();
            if seen.contains(&raw) {
                continue;
            }
            seen.push(raw.clone());
            let mut scopes: Vec<PermissionRuleScope> = Vec::new();
            for (scope, _, parsed) in &layer_rules {
                if parsed == &rule && !scopes.contains(scope) {
                    scopes.push(*scope);
                }
            }
            let removable = !scopes.is_empty() && scopes.iter().all(|scope| scope.is_editable());
            if scopes.is_empty() {
                scopes.push(PermissionRuleScope::ManagedOrCli);
            }
            rules.push(PermissionRuleEntry {
                decision: rule.decision(),
                rule,
                raw,
                scopes,
                removable,
            });
        }

        // 3. 工作区根归因。第一项固定是 cwd，不加来源（UI 显示 “always included”）。
        let roots = if workspace_roots.is_empty() {
            vec![cwd.clone()]
        } else {
            workspace_roots
        };
        let mut workspace_scopes: Vec<Vec<PermissionRuleScope>> = vec![Vec::new(); roots.len()];
        for (index, root) in roots.iter().enumerate().skip(1) {
            let resolved = resolve_workspace_path(&cwd, root);
            let mut scopes: Vec<PermissionRuleScope> = Vec::new();
            for layer in &layers {
                let Some(permissions) = &layer.permissions else {
                    continue;
                };
                if permissions.is_null() {
                    continue;
                }
                let Some(object) = permissions.as_object() else {
                    continue;
                };
                for key in ["addDirs", "workspaceRoots", "additionalDirectories"] {
                    let Some(items) = object.get(key).and_then(Value::as_array) else {
                        continue;
                    };
                    for item in items {
                        let Some(text) = item.as_str() else {
                            continue;
                        };
                        if resolve_workspace_path(&cwd, Path::new(text)) == resolved
                            && !scopes.contains(&layer.scope)
                        {
                            scopes.push(layer.scope);
                        }
                    }
                }
            }
            if scopes.is_empty() {
                scopes.push(PermissionRuleScope::ManagedOrCli);
            }
            workspace_scopes[index] = scopes;
        }

        Self {
            mode,
            bypass_disabled,
            rules,
            workspace_roots: roots,
            workspace_scopes,
            session_allow_count,
            errors,
        }
    }
}

/// 从 `SettingsManager` + local settings 文件读取三层 `permissions`。
///
/// - User：`get_global_settings().raw("permissions")`；
/// - Project：`get_project_settings().raw("permissions")`（仅项目受信时）；
/// - Local：`read_permissions_file(local_settings_path(cwd))`（仅项目受信时）。
///
/// 未受信时 project/local 层不读取（与启动路径一致：未受信项目的 settings 不参与权限
/// 合并），也不产生错误——这不是失败，而是策略。
pub fn collect_settings_layers(
    settings_manager: &SettingsManager,
    cwd: &Path,
) -> (Vec<PermissionSettingsLayer>, Vec<String>) {
    let mut layers = Vec::new();
    let mut errors = Vec::new();
    layers.push(PermissionSettingsLayer::new(
        PermissionRuleScope::User,
        settings_manager.get_global_settings().raw("permissions"),
    ));
    if settings_manager.is_project_trusted() {
        layers.push(PermissionSettingsLayer::new(
            PermissionRuleScope::Project,
            settings_manager.get_project_settings().raw("permissions"),
        ));
        let local_path = permissions_settings::local_settings_path(cwd);
        match permissions_settings::read_permissions_file(&local_path) {
            Ok(permissions) => layers.push(PermissionSettingsLayer::new(
                PermissionRuleScope::Local,
                permissions,
            )),
            Err(error) => errors.push(error.to_string()),
        }
    }
    (layers, errors)
}

/// 把 settings 里的一条 `rules` 条目解析成 [`PermissionRule`]。
///
/// `kiana_permissions` 的 `rule_from_settings_value` 是私有的；这里用**同一个引擎**
/// 构造一个只含该条目的临时 settings 再取回策略里的规则，保证与启动路径的解析逐字一致
/// （包括 `exact`/`wildcard`/`actions`/`approved` 等分支），而不是在 UI 里另写一份解析器。
///
/// 传入无法解析的条目时返回 `None`；空规则数组不会走到这里。
pub fn parse_settings_rule_value(raw: &Value, cwd: &Path) -> Option<PermissionRule> {
    let settings = Value::Object(Map::from_iter([(
        "rules".to_string(),
        Value::Array(vec![raw.clone()]),
    )]));
    let engine = PermissionEngine::from_settings_and_flags(
        cwd,
        Some(&settings),
        &PermissionFlags::default(),
    )?;
    let mut rules = engine.policy().rules;
    if rules.len() == 1 { rules.pop() } else { None }
}

/// 解析 `permissions.allow/deny/ask` 简写，并做与引擎一致的路径归一化。
///
/// 引擎构造策略时会调用 `normalize_rule_paths`：规则里的 `path:` 前缀按工作区根
/// 展开 `~`、`$HOME`、`@` 与相对路径。若这里只跑 `parse_cli_rule`，`path:generated`
/// 会保留相对形态，与 `policy_rules` 里的绝对规则比对失败，规则被误标成
/// `ManagedOrCli`（不可删除）。走同一个引擎入口可以保证两侧逐字相同。
pub(crate) fn parse_shorthand_rule_value(
    text: &str,
    decision: RuleDecision,
    cwd: &Path,
) -> Option<PermissionRule> {
    let key = match decision {
        RuleDecision::Allow => "allow",
        RuleDecision::Deny => "deny",
        RuleDecision::Ask => "ask",
    };
    let settings = Value::Object(Map::from_iter([(
        key.to_string(),
        Value::Array(vec![Value::String(text.to_string())]),
    )]));
    let engine = PermissionEngine::from_settings_and_flags(
        cwd,
        Some(&settings),
        &PermissionFlags::default(),
    )?;
    let mut rules = engine.policy().rules;
    if rules.len() == 1 { rules.pop() } else { None }
}

/// 规则的稳定展示文本。
///
/// 与设计文档 §3.3 的 `PermissionRule::display_text()` 对应（该方法在本任务范围内不在
/// `kiana_permissions` 中，暂以自由函数提供）。前缀固定为
/// `tool:` / `command:` / `path:` / `exact:` / `wildcard:` / `approved:`。
pub fn display_rule_text(rule: &PermissionRule) -> String {
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

/// 规则的原因文本；没有时返回 `None`。
pub fn rule_reason(rule: &PermissionRule) -> Option<&str> {
    match rule {
        PermissionRule::Tool { reason, .. }
        | PermissionRule::CommandPrefix { reason, .. }
        | PermissionRule::PathPrefix { reason, .. }
        | PermissionRule::ExactCommand { reason, .. }
        | PermissionRule::CommandWildcard { reason, .. } => reason.as_deref(),
        PermissionRule::Approved { rule, .. } => rule_reason(rule),
    }
}

/// 面板底部的一行提示（info = 绿色，error = 红色）。
///
/// 设计文档把 `PanelNotice` 归到 `panel_common`；该共享件由 Integrate agent 提供。
/// 在它落地前这里先定义等价的最小类型，字段/语义一致，后续可无痛替换。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PanelNotice {
    /// 操作成功的提示。
    Info(String),
    /// 操作失败/被拒绝的提示。
    Error(String),
}

impl PanelNotice {
    /// 成功提示。
    pub fn info(text: impl Into<String>) -> Self {
        Self::Info(text.into())
    }

    /// 错误提示。
    pub fn error(text: impl Into<String>) -> Self {
        Self::Error(text.into())
    }

    /// 是否是错误。
    pub fn is_error(&self) -> bool {
        matches!(self, Self::Error(_))
    }

    /// 文本。
    pub fn text(&self) -> &str {
        match self {
            Self::Info(text) | Self::Error(text) => text,
        }
    }
}

/// 规则/目录要写入的文件作用域（新增时的选择项）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionRuleScopeChoice {
    /// `.kiana/settings.json`（项目受信才可写）。
    Project,
    /// `.kiana/settings.local.json`（项目受信才可写）。
    Local,
    /// `<agent-dir>/settings.json`。
    User,
}

impl PermissionRuleScopeChoice {
    /// 选择顺序（Project 第一项，与 qwen 一致）。
    pub const ALL: [PermissionRuleScopeChoice; 3] = [
        PermissionRuleScopeChoice::Project,
        PermissionRuleScopeChoice::Local,
        PermissionRuleScopeChoice::User,
    ];

    /// 选项标题。
    pub fn label(self) -> &'static str {
        match self {
            Self::Project => "Project settings",
            Self::Local => "Local settings",
            Self::User => "User settings",
        }
    }

    /// 写入的文件（稳定展示文本，不访问文件系统）。
    pub fn file_description(self) -> &'static str {
        match self {
            Self::Project => ".kiana/settings.json",
            Self::Local => ".kiana/settings.local.json",
            Self::User => "<agent-dir>/settings.json",
        }
    }

    /// 对应的快照来源。
    pub fn scope(self) -> PermissionRuleScope {
        match self {
            Self::Project => PermissionRuleScope::Project,
            Self::Local => PermissionRuleScope::Local,
            Self::User => PermissionRuleScope::User,
        }
    }

    /// 是否要求项目受信。
    pub fn requires_trust(self) -> bool {
        matches!(self, Self::Project | Self::Local)
    }
}

/// 模式切换的持久化作用域。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModePersistenceScope {
    /// 只改本会话（默认，避免误持久化）。
    Session,
    /// 写入项目 settings。
    Project,
    /// 写入项目本地 settings。
    Local,
    /// 写入用户 settings。
    User,
}

impl ModePersistenceScope {
    /// 选择顺序（默认 Session 第一项，与 qwen `/approval-mode` 的命令语义一致）。
    pub const ALL: [ModePersistenceScope; 4] = [
        ModePersistenceScope::Session,
        ModePersistenceScope::Project,
        ModePersistenceScope::Local,
        ModePersistenceScope::User,
    ];

    /// 选项标题。
    pub fn label(self) -> &'static str {
        match self {
            Self::Session => "Session only",
            Self::Project => "Project settings",
            Self::Local => "Local settings",
            Self::User => "User settings",
        }
    }

    /// 描述。
    pub fn description(self) -> &'static str {
        match self {
            Self::Session => "this session only",
            Self::Project => ".kiana/settings.json",
            Self::Local => ".kiana/settings.local.json",
            Self::User => "<agent-dir>/settings.json",
        }
    }

    /// Project/Local 需要项目受信。
    pub fn requires_trust(self) -> bool {
        matches!(self, Self::Project | Self::Local)
    }
}

/// 面板的视图状态。
///
/// 前八个变体与设计文档 §3.3 对应；`ModeScopeSelect` / `AddDirectoryScope` 是为
/// §3.8/§3.7 的作用域选择补的（见模块文档）。
#[derive(Debug, Clone, PartialEq)]
pub enum PermissionsView {
    /// 规则列表（含 `+ Add rule…` 首行）。
    RuleList {
        /// 当前标签页。
        tab: RuleTab,
    },
    /// 模式选择。
    ModeSelect {
        /// 选中下标（[`PermissionMode`] 顺序：read-only/default/accept-edits/bypass）。
        selected: usize,
        /// 返回时回到的标签页。
        return_tab: RuleTab,
    },
    /// 模式持久化作用域选择。
    ModeScopeSelect {
        /// 已选模式。
        mode: PermissionMode,
        /// 选中下标（[`ModePersistenceScope::ALL`] 顺序）。
        selected: usize,
        /// 返回时回到的标签页。
        return_tab: RuleTab,
    },
    /// 输入新规则。
    AddRuleInput {
        /// 输入串。
        text: String,
        /// 解析错误。
        error: Option<String>,
    },
    /// 新规则的作用域选择。
    AddRuleScope {
        /// 已解析规则。
        rule: PermissionRule,
        /// 原始输入（Esc 返回时保留）。
        text: String,
        /// 选中下标（[`PermissionRuleScopeChoice::ALL`] 顺序）。
        selected: usize,
        /// 错误。
        error: Option<String>,
    },
    /// 删除规则确认。
    DeleteRuleConfirm {
        /// 待删除条目。
        entry: PermissionRuleEntry,
        /// 0 = No，1 = Yes。
        selected: usize,
        /// 错误。
        error: Option<String>,
    },
    /// 工作区目录列表（含 `+ Add directory…` 首行）。
    WorkspaceList,
    /// 输入新目录。
    AddDirectoryInput {
        /// 输入串。
        text: String,
        /// 校验错误。
        error: Option<String>,
    },
    /// 新目录的作用域选择。
    AddDirectoryScope {
        /// 归一化后的绝对路径。
        path: PathBuf,
        /// 原始输入（Esc 返回时保留）。
        text: String,
        /// 选中下标。
        selected: usize,
        /// 错误。
        error: Option<String>,
    },
    /// 删除目录确认。
    RemoveDirectoryConfirm {
        /// 待删除根。
        root: PathBuf,
        /// 0 = No，1 = Yes。
        selected: usize,
        /// 错误。
        error: Option<String>,
    },
}

/// 新增规则的写回调。
pub type AddRuleCallback =
    Box<dyn FnMut(&PermissionRule, PermissionRuleScopeChoice) -> Result<(), String>>;

/// 删除规则的写回调。
pub type DeleteRuleCallback = Box<dyn FnMut(&PermissionRuleEntry) -> Result<(), String>>;

/// 切换模式的写回调。
pub type SetModeCallback =
    Box<dyn FnMut(PermissionMode, ModePersistenceScope) -> Result<(), String>>;

/// 新增工作区目录的写回调。
pub type AddDirectoryCallback =
    Box<dyn FnMut(&Path, PermissionRuleScopeChoice) -> Result<(), String>>;

/// 删除工作区目录的写回调。
pub type RemoveDirectoryCallback = Box<dyn FnMut(&Path) -> Result<(), String>>;

/// 宿主提供的回调面。
///
/// 所有写操作都返回 `Result`：`Err(text)` 会显示在输入/确认视图里并保持视图，便于原地
/// 重试；`Ok(())` 表示宿主已接受操作（组件随后回到列表并显示成功 notice）。
///
/// 写盘是同步入队（`SettingsManager::update_project_permissions` 等），`flush().await`
/// 与 `drain_errors()` 由宿主在 `spawn_local` 里完成；其延迟错误通过
/// [`PermissionsDialog::set_notice`] 回填，不阻塞输入。
pub struct PermissionsCallbacks {
    /// 新增规则。
    pub on_add_rule: AddRuleCallback,
    /// 删除规则（`entry` 含来源与 `removable`）。
    pub on_delete_rule: DeleteRuleCallback,
    /// 切换模式。
    pub on_set_mode: SetModeCallback,
    /// 新增工作区目录。
    pub on_add_directory: AddDirectoryCallback,
    /// 删除工作区目录。
    pub on_remove_directory: RemoveDirectoryCallback,
    /// 重新取快照（`r` 键）。
    pub on_refresh: Box<dyn FnMut()>,
    /// 关闭面板（Esc）。
    pub on_close: Box<dyn FnMut()>,
}

impl Default for PermissionsCallbacks {
    fn default() -> Self {
        Self {
            on_add_rule: Box::new(|_, _| Ok(())),
            on_delete_rule: Box::new(|_| Ok(())),
            on_set_mode: Box::new(|_, _| Ok(())),
            on_add_directory: Box::new(|_, _| Ok(())),
            on_remove_directory: Box::new(|_| Ok(())),
            on_refresh: Box::new(|| {}),
            on_close: Box::new(|| {}),
        }
    }
}

/// [`PermissionsDialog::new`] 的参数。
pub struct PermissionsDialogOptions {
    /// 初始快照。
    pub snapshot: PermissionSnapshot,
    /// 会话工作目录（工作区路径校验与 `~` 展开的基准）。
    pub cwd: PathBuf,
    /// 项目是否受信（决定 Project/Local 作用域是否可写）。
    pub project_trusted: bool,
    /// 宿主回调。
    pub callbacks: PermissionsCallbacks,
}

/// 规则列表行：首行永远是 `+ Add rule…`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuleRow {
    Add,
    Rule(usize),
}

/// 工作区列表行：首行永远是 `+ Add directory…`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkspaceRow {
    Add,
    Root(usize),
}

/// `/permissions` 面板组件。
///
/// 实现 [`Component`] + [`Focusable`]：`handles_input()` 恒为 `true`，
/// `as_focusable()` 返回自身，挂载时用 `SelectorFocus::Component`。
pub struct PermissionsDialog {
    snapshot: PermissionSnapshot,
    view: PermissionsView,
    cwd: PathBuf,
    project_trusted: bool,
    /// 当前列表标签页（即使处于输入/确认视图也保留，返回时用）。
    tab: RuleTab,
    rule_query: String,
    ws_query: String,
    filter_active: bool,
    selected_rule: usize,
    selected_dir: usize,
    notice: Option<PanelNotice>,
    callbacks: PermissionsCallbacks,
    focused: bool,
}

impl PermissionsDialog {
    /// 构造面板。初始视图是 Allow 标签页，选中 `+ Add rule…`。
    pub fn new(options: PermissionsDialogOptions) -> Self {
        let mut dialog = Self {
            snapshot: options.snapshot,
            view: PermissionsView::RuleList {
                tab: RuleTab::Allow,
            },
            cwd: options.cwd,
            project_trusted: options.project_trusted,
            tab: RuleTab::Allow,
            rule_query: String::new(),
            ws_query: String::new(),
            filter_active: false,
            selected_rule: 0,
            selected_dir: 0,
            notice: None,
            callbacks: options.callbacks,
            focused: false,
        };
        dialog.clamp_selections();
        dialog
    }

    // -- 只读访问（测试与宿主） ---------------------------------------------

    /// 当前快照。
    pub fn snapshot(&self) -> &PermissionSnapshot {
        &self.snapshot
    }

    /// 当前视图。
    pub fn view(&self) -> &PermissionsView {
        &self.view
    }

    /// 当前列表标签页。
    pub fn tab(&self) -> RuleTab {
        self.tab
    }

    /// 规则列表选中行（0 = `+ Add rule…`）。
    pub fn selected_rule(&self) -> usize {
        self.selected_rule
    }

    /// 工作区列表选中行（0 = `+ Add directory…`，1 = cwd）。
    pub fn selected_dir(&self) -> usize {
        self.selected_dir
    }

    /// 当前 notice。
    pub fn notice(&self) -> Option<&PanelNotice> {
        self.notice.as_ref()
    }

    /// 规则过滤查询串。
    pub fn rule_query(&self) -> &str {
        &self.rule_query
    }

    /// 工作区过滤查询串。
    pub fn ws_query(&self) -> &str {
        &self.ws_query
    }

    /// 是否处于过滤模式。
    pub fn is_filter_active(&self) -> bool {
        self.filter_active
    }

    /// 项目是否受信。
    pub fn project_trusted(&self) -> bool {
        self.project_trusted
    }

    /// 会话 cwd。
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    // -- 宿主回填 -----------------------------------------------------------

    /// 回填新快照（`r` 刷新、写操作成功后由宿主调用）。
    ///
    /// 保留当前视图与标签页，只把越界的选中下标夹回有效范围。
    pub fn set_snapshot(&mut self, snapshot: PermissionSnapshot) {
        self.snapshot = snapshot;
        self.clamp_selections();
    }

    /// 设置/清除底部提示。
    pub fn set_notice(&mut self, notice: Option<PanelNotice>) {
        self.notice = notice;
    }

    /// 更新项目受信状态（打开面板后信任状态可能变化，虽然罕见）。
    pub fn set_project_trusted(&mut self, trusted: bool) {
        self.project_trusted = trusted;
    }

    // -- 行构造 -------------------------------------------------------------

    fn rule_rows(&self) -> Vec<RuleRow> {
        let mut rows = vec![RuleRow::Add];
        let Some(decision) = self.tab.decision() else {
            return rows;
        };
        let query = self.rule_query.to_lowercase();
        for (index, entry) in self.snapshot.rules.iter().enumerate() {
            if entry.decision != decision {
                continue;
            }
            if !query.is_empty()
                && !display_rule_text(&entry.rule)
                    .to_lowercase()
                    .contains(&query)
            {
                continue;
            }
            rows.push(RuleRow::Rule(index));
        }
        rows
    }

    fn workspace_roots(&self) -> Vec<PathBuf> {
        if self.snapshot.workspace_roots.is_empty() {
            vec![self.cwd.clone()]
        } else {
            self.snapshot.workspace_roots.clone()
        }
    }

    fn workspace_scopes(&self, index: usize) -> Vec<PermissionRuleScope> {
        self.snapshot
            .workspace_scopes
            .get(index)
            .cloned()
            .unwrap_or_else(|| {
                if index == 0 {
                    Vec::new()
                } else {
                    vec![PermissionRuleScope::ManagedOrCli]
                }
            })
    }

    fn workspace_rows(&self) -> Vec<WorkspaceRow> {
        let mut rows = vec![WorkspaceRow::Add];
        let query = self.ws_query.to_lowercase();
        for (index, root) in self.workspace_roots().iter().enumerate() {
            if !query.is_empty() && !root.to_string_lossy().to_lowercase().contains(&query) {
                continue;
            }
            rows.push(WorkspaceRow::Root(index));
        }
        rows
    }

    /// 工作区根是否可删除：cwd（0）永远不可删；不可归因/来自 managed/CLI 的根不可删。
    fn workspace_root_removable(&self, index: usize) -> bool {
        if index == 0 {
            return false;
        }
        let scopes = self.workspace_scopes(index);
        !scopes.is_empty() && scopes.iter().all(|scope| scope.is_editable())
    }

    fn clamp_selections(&mut self) {
        let rule_len = self.rule_rows().len();
        self.selected_rule = self.selected_rule.min(rule_len.saturating_sub(1));
        let workspace_len = self.workspace_rows().len();
        self.selected_dir = self.selected_dir.min(workspace_len.saturating_sub(1));
    }

    /// 当前选中的规则行。
    ///
    /// `RuleRow::Rule(index)` 指向快照里的稳定下标：过滤只改可见行集合、不改快照，
    /// 因此行值就是设计 §3.4 要求的“选中项 id”。
    fn selected_rule_row(&self) -> Option<RuleRow> {
        self.rule_rows().get(self.selected_rule).copied()
    }

    /// 当前选中的工作区行。
    fn selected_workspace_row(&self) -> Option<WorkspaceRow> {
        self.workspace_rows().get(self.selected_dir).copied()
    }

    /// 按行身份还原规则选中项；目标行被过滤掉时退回位置夹取。
    fn restore_rule_row(&mut self, previous: Option<RuleRow>) {
        let rows = self.rule_rows();
        self.selected_rule = previous
            .and_then(|previous| rows.iter().position(|row| *row == previous))
            .unwrap_or_else(|| self.selected_rule.min(rows.len().saturating_sub(1)));
    }

    /// 按行身份还原工作区选中项；目标行被过滤掉时退回位置夹取。
    fn restore_workspace_row(&mut self, previous: Option<WorkspaceRow>) {
        let rows = self.workspace_rows();
        self.selected_dir = previous
            .and_then(|previous| rows.iter().position(|row| *row == previous))
            .unwrap_or_else(|| self.selected_dir.min(rows.len().saturating_sub(1)));
    }

    fn move_selection(&mut self, workspace: bool, up: bool, page: bool) {
        let len = if workspace {
            self.workspace_rows().len()
        } else {
            self.rule_rows().len()
        };
        if len == 0 {
            return;
        }
        let selected = if workspace {
            &mut self.selected_dir
        } else {
            &mut self.selected_rule
        };
        *selected = (*selected).min(len - 1);
        if page {
            let delta = PAGE_SIZE.min(len - 1).max(1);
            *selected = if up {
                selected.saturating_sub(delta)
            } else {
                (*selected + delta).min(len - 1)
            };
        } else if up {
            *selected = if *selected == 0 {
                len - 1
            } else {
                *selected - 1
            };
        } else {
            *selected = if *selected + 1 >= len {
                0
            } else {
                *selected + 1
            };
        }
    }

    fn current_tab(&self) -> RuleTab {
        match &self.view {
            PermissionsView::RuleList { tab } => *tab,
            PermissionsView::WorkspaceList => RuleTab::Workspace,
            _ => self.tab,
        }
    }

    /// 标签页对应的列表视图：Workspace 是独立变体，其余三个是 `RuleList`。
    fn list_view(tab: RuleTab) -> PermissionsView {
        match tab {
            RuleTab::Workspace => PermissionsView::WorkspaceList,
            tab => PermissionsView::RuleList { tab },
        }
    }

    // -- 键处理 -------------------------------------------------------------

    fn is_backspace(key: &str) -> bool {
        get_keybindings().matches(key, "tui.editor.deleteCharBackward")
            || key == "\x7f"
            || key == "\x08"
    }

    fn is_confirm(key: &str) -> bool {
        get_keybindings().matches(key, "tui.select.confirm") || key == "\n" || key == "\r"
    }

    fn is_cancel(key: &str) -> bool {
        get_keybindings().matches(key, "tui.select.cancel")
    }

    /// 取出可打印输入；控制序列（方向键、功能键）返回 `None`。
    fn printable_input(key: &str) -> Option<String> {
        if let Some(decoded) = kiana_tui::keys::decode_printable_key(key) {
            return Some(decoded);
        }
        if key.is_empty()
            || key.starts_with('\x1b')
            || key == "\t"
            || key == "\n"
            || key == "\r"
            || key == "\x7f"
            || key == "\x08"
            || key.chars().any(char::is_control)
        {
            return None;
        }
        Some(key.to_string())
    }

    fn cycle_tab(&mut self, forward: bool) {
        let current = self.current_tab().index() as isize;
        let next = RuleTab::from_index(current + if forward { 1 } else { -1 });
        self.tab = next;
        self.filter_active = false;
        self.notice = None;
        self.view = Self::list_view(next);
        self.clamp_selections();
    }

    fn close(&mut self) {
        (self.callbacks.on_close)();
    }

    fn activate_rule_row(&mut self) {
        let rows = self.rule_rows();
        let Some(row) = rows.get(self.selected_rule).copied() else {
            return;
        };
        match row {
            RuleRow::Add => {
                self.notice = None;
                self.view = PermissionsView::AddRuleInput {
                    text: String::new(),
                    error: None,
                };
            }
            RuleRow::Rule(index) => {
                let entry = self.snapshot.rules[index].clone();
                self.notice = None;
                self.view = PermissionsView::DeleteRuleConfirm {
                    entry,
                    selected: 1,
                    error: None,
                };
            }
        }
    }

    fn open_delete_rule_confirm(&mut self) {
        let rows = self.rule_rows();
        if let Some(RuleRow::Rule(index)) = rows.get(self.selected_rule).copied() {
            let entry = self.snapshot.rules[index].clone();
            self.notice = None;
            self.view = PermissionsView::DeleteRuleConfirm {
                entry,
                selected: 1,
                error: None,
            };
        }
    }

    fn open_remove_directory_confirm(&mut self) {
        let rows = self.workspace_rows();
        let Some(WorkspaceRow::Root(index)) = rows.get(self.selected_dir).copied() else {
            return;
        };
        let root = self.workspace_roots()[index].clone();
        if index == 0 {
            self.notice = Some(PanelNotice::error(
                "The working directory cannot be removed.",
            ));
            return;
        }
        self.notice = None;
        self.view = PermissionsView::RemoveDirectoryConfirm {
            root,
            selected: 1,
            error: None,
        };
    }

    fn handle_list_input(&mut self, key: &str, workspace: bool) {
        // 标签切换优先：过滤模式下也能切页。
        if key == "\t" {
            self.cycle_tab(true);
            return;
        }
        if key == "\x1b[Z" || get_keybindings().matches(key, "tui.editor.cursorLeft") {
            self.cycle_tab(false);
            return;
        }
        if get_keybindings().matches(key, "tui.editor.cursorRight") {
            self.cycle_tab(true);
            return;
        }

        if self.filter_active {
            if Self::is_cancel(key) {
                // 退出过滤/清空 query 会重建可见行：按行身份还原选中项，避免下标
                // 落到另一条规则上（选中态漂移后 Enter/d 会操作错误的规则）。
                let rule_row = self.selected_rule_row();
                let workspace_row = self.selected_workspace_row();
                self.filter_active = false;
                if workspace {
                    self.ws_query.clear();
                } else {
                    self.rule_query.clear();
                }
                self.restore_rule_row(rule_row);
                self.restore_workspace_row(workspace_row);
                return;
            }
            if Self::is_confirm(key) {
                // 过滤中 Enter 只提交过滤、退出过滤模式，不激活行。
                self.filter_active = false;
                return;
            }
            if Self::is_backspace(key) {
                let rule_row = self.selected_rule_row();
                let workspace_row = self.selected_workspace_row();
                let query = if workspace {
                    &mut self.ws_query
                } else {
                    &mut self.rule_query
                };
                query.pop();
                self.restore_rule_row(rule_row);
                self.restore_workspace_row(workspace_row);
                return;
            }
            if let Some(text) = Self::printable_input(key) {
                let rule_row = self.selected_rule_row();
                let workspace_row = self.selected_workspace_row();
                let query = if workspace {
                    &mut self.ws_query
                } else {
                    &mut self.rule_query
                };
                query.push_str(&text);
                self.restore_rule_row(rule_row);
                self.restore_workspace_row(workspace_row);
            }
            return;
        }

        if key == "/" {
            let rule_row = self.selected_rule_row();
            let workspace_row = self.selected_workspace_row();
            self.filter_active = true;
            if workspace {
                self.ws_query.clear();
            } else {
                self.rule_query.clear();
            }
            self.restore_rule_row(rule_row);
            self.restore_workspace_row(workspace_row);
            return;
        }
        if get_keybindings().matches(key, "tui.select.up") {
            self.move_selection(workspace, true, false);
            return;
        }
        if get_keybindings().matches(key, "tui.select.down") {
            self.move_selection(workspace, false, false);
            return;
        }
        if get_keybindings().matches(key, "tui.select.pageUp") {
            self.move_selection(workspace, true, true);
            return;
        }
        if get_keybindings().matches(key, "tui.select.pageDown") {
            self.move_selection(workspace, false, true);
            return;
        }
        if Self::is_confirm(key) {
            if workspace {
                match self.workspace_rows().get(self.selected_dir).copied() {
                    Some(WorkspaceRow::Add) => {
                        self.notice = None;
                        self.view = PermissionsView::AddDirectoryInput {
                            text: String::new(),
                            error: None,
                        };
                    }
                    Some(WorkspaceRow::Root(index)) => {
                        if index == 0 {
                            self.notice = Some(PanelNotice::error(
                                "The working directory cannot be removed.",
                            ));
                        } else {
                            self.open_remove_directory_confirm();
                        }
                    }
                    None => {}
                }
            } else {
                self.activate_rule_row();
            }
            return;
        }
        if Self::is_cancel(key) {
            self.close();
            return;
        }
        match key {
            "m" => {
                let selected = self
                    .snapshot
                    .mode
                    .map(mode_index)
                    .unwrap_or(mode_index(PermissionMode::Default));
                self.notice = None;
                self.view = PermissionsView::ModeSelect {
                    selected,
                    return_tab: self.tab,
                };
            }
            "r" => (self.callbacks.on_refresh)(),
            "d" => {
                if workspace {
                    self.open_remove_directory_confirm();
                } else {
                    self.open_delete_rule_confirm();
                }
            }
            "a" if !workspace => {
                self.notice = None;
                self.view = PermissionsView::AddRuleInput {
                    text: String::new(),
                    error: None,
                };
            }
            _ => {}
        }
    }

    fn handle_mode_select_input(&mut self, key: &str) {
        let PermissionsView::ModeSelect {
            selected,
            return_tab,
        } = self.view.clone()
        else {
            return;
        };
        if get_keybindings().matches(key, "tui.select.up") {
            let next = if selected == 0 { 3 } else { selected - 1 };
            self.view = PermissionsView::ModeSelect {
                selected: next,
                return_tab,
            };
            return;
        }
        if get_keybindings().matches(key, "tui.select.down") {
            let next = if selected + 1 >= 4 { 0 } else { selected + 1 };
            self.view = PermissionsView::ModeSelect {
                selected: next,
                return_tab,
            };
            return;
        }
        if Self::is_cancel(key) {
            self.notice = None;
            self.view = Self::list_view(return_tab);
            self.tab = return_tab;
            return;
        }
        if !Self::is_confirm(key) {
            return;
        }
        let mode = mode_at(selected);
        if mode.is_bypass() && self.snapshot.bypass_disabled {
            self.notice = Some(PanelNotice::error("Bypass mode is disabled by policy."));
            return;
        }
        self.notice = None;
        self.view = PermissionsView::ModeScopeSelect {
            mode,
            selected: 0,
            return_tab,
        };
    }

    fn handle_mode_scope_input(&mut self, key: &str) {
        let PermissionsView::ModeScopeSelect {
            mode,
            selected,
            return_tab,
        } = self.view.clone()
        else {
            return;
        };
        let len = ModePersistenceScope::ALL.len();
        if get_keybindings().matches(key, "tui.select.up") {
            let next = if selected == 0 { len - 1 } else { selected - 1 };
            self.view = PermissionsView::ModeScopeSelect {
                mode,
                selected: next,
                return_tab,
            };
            return;
        }
        if get_keybindings().matches(key, "tui.select.down") {
            let next = if selected + 1 >= len { 0 } else { selected + 1 };
            self.view = PermissionsView::ModeScopeSelect {
                mode,
                selected: next,
                return_tab,
            };
            return;
        }
        if Self::is_cancel(key) {
            self.notice = None;
            self.view = PermissionsView::ModeSelect {
                selected: mode_index(mode),
                return_tab,
            };
            return;
        }
        if !Self::is_confirm(key) {
            return;
        }
        let scope = ModePersistenceScope::ALL[selected.min(len - 1)];
        if scope.requires_trust() && !self.project_trusted {
            self.notice = Some(PanelNotice::error("Project is not trusted; cannot write."));
            return;
        }
        match (self.callbacks.on_set_mode)(mode, scope) {
            Ok(()) => {
                self.snapshot.mode = Some(mode);
                self.tab = return_tab;
                self.view = Self::list_view(return_tab);
                self.notice = Some(PanelNotice::info(format!(
                    "Permission mode set to {}",
                    mode.as_str()
                )));
            }
            Err(error) => {
                self.notice = Some(PanelNotice::error(error));
            }
        }
    }

    fn handle_add_rule_input(&mut self, key: &str) {
        let PermissionsView::AddRuleInput { mut text, .. } = self.view.clone() else {
            return;
        };
        if Self::is_cancel(key) {
            self.view = Self::list_view(self.tab);
            return;
        }
        if Self::is_backspace(key) {
            text.pop();
            self.view = PermissionsView::AddRuleInput { text, error: None };
            return;
        }
        if Self::is_confirm(key) {
            let Some(decision) = self.tab.decision() else {
                self.view = Self::list_view(self.tab);
                return;
            };
            match parse_cli_rule(text.trim(), decision) {
                Some(rule) => {
                    self.view = PermissionsView::AddRuleScope {
                        rule,
                        text,
                        selected: self.default_scope_index(),
                        error: None,
                    };
                }
                None => {
                    self.view = PermissionsView::AddRuleInput {
                        text,
                        error: Some(
                            "Malformed rule. Use tool:<name>, command:<prefix>, path:<dir>, or a bare tool name."
                                .to_string(),
                        ),
                    };
                }
            }
            return;
        }
        if let Some(input) = Self::printable_input(key) {
            text.push_str(&input);
            self.view = PermissionsView::AddRuleInput { text, error: None };
        }
    }

    fn default_scope_index(&self) -> usize {
        // 顺序 Project/Local/User；未受信时回退到 User。
        if self.project_trusted {
            0
        } else {
            PermissionRuleScopeChoice::ALL.len() - 1
        }
    }

    fn handle_add_rule_scope(&mut self, key: &str) {
        let PermissionsView::AddRuleScope {
            rule,
            text,
            selected,
            ..
        } = self.view.clone()
        else {
            return;
        };
        let len = PermissionRuleScopeChoice::ALL.len();
        if get_keybindings().matches(key, "tui.select.up") {
            let next = if selected == 0 { len - 1 } else { selected - 1 };
            self.view = PermissionsView::AddRuleScope {
                rule,
                text,
                selected: next,
                error: None,
            };
            return;
        }
        if get_keybindings().matches(key, "tui.select.down") {
            let next = if selected + 1 >= len { 0 } else { selected + 1 };
            self.view = PermissionsView::AddRuleScope {
                rule,
                text,
                selected: next,
                error: None,
            };
            return;
        }
        if Self::is_cancel(key) {
            self.view = PermissionsView::AddRuleInput { text, error: None };
            return;
        }
        if !Self::is_confirm(key) {
            return;
        }
        let choice = PermissionRuleScopeChoice::ALL[selected.min(len - 1)];
        if self.snapshot.mode.is_none() {
            self.view = PermissionsView::AddRuleScope {
                rule,
                text,
                selected,
                error: Some("Permission checks are disabled. Change the mode first.".to_string()),
            };
            return;
        }
        if choice.requires_trust() && !self.project_trusted {
            self.view = PermissionsView::AddRuleScope {
                rule,
                text,
                selected,
                error: Some("Project is not trusted; cannot write.".to_string()),
            };
            return;
        }
        match (self.callbacks.on_add_rule)(&rule, choice) {
            Ok(()) => {
                let label = display_rule_text(&rule);
                self.view = Self::list_view(self.tab);
                self.notice = Some(PanelNotice::info(format!("Added {label}")));
            }
            Err(error) => {
                self.view = PermissionsView::AddRuleScope {
                    rule,
                    text,
                    selected,
                    error: Some(error),
                };
            }
        }
    }

    fn handle_delete_rule_confirm(&mut self, key: &str) {
        let PermissionsView::DeleteRuleConfirm {
            entry,
            selected,
            error,
        } = self.view.clone()
        else {
            return;
        };
        if get_keybindings().matches(key, "tui.select.up")
            || get_keybindings().matches(key, "tui.select.down")
            || get_keybindings().matches(key, "tui.editor.cursorLeft")
            || get_keybindings().matches(key, "tui.editor.cursorRight")
        {
            self.view = PermissionsView::DeleteRuleConfirm {
                entry,
                selected: if selected == 0 { 1 } else { 0 },
                error,
            };
            return;
        }
        if Self::is_cancel(key) || key == "n" {
            self.view = Self::list_view(self.tab);
            return;
        }
        let yes = key == "y" || (Self::is_confirm(key) && selected == 1);
        if !yes {
            if Self::is_confirm(key) && selected == 0 {
                self.view = Self::list_view(self.tab);
            }
            return;
        }
        if !entry.removable {
            self.view = Self::list_view(self.tab);
            self.notice = Some(PanelNotice::error(
                "This rule is enforced by managed policy or the CLI and cannot be removed here.",
            ));
            return;
        }
        match (self.callbacks.on_delete_rule)(&entry) {
            Ok(()) => {
                let label = display_rule_text(&entry.rule);
                self.view = Self::list_view(self.tab);
                self.notice = Some(PanelNotice::info(format!("Removed {label}")));
            }
            Err(error) => {
                self.view = PermissionsView::DeleteRuleConfirm {
                    entry,
                    selected,
                    error: Some(error),
                };
            }
        }
    }

    fn handle_add_directory_input(&mut self, key: &str) {
        let PermissionsView::AddDirectoryInput { mut text, .. } = self.view.clone() else {
            return;
        };
        if Self::is_cancel(key) {
            self.view = PermissionsView::WorkspaceList;
            return;
        }
        if Self::is_backspace(key) {
            text.pop();
            self.view = PermissionsView::AddDirectoryInput { text, error: None };
            return;
        }
        if Self::is_confirm(key) {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                return;
            }
            match self.validate_workspace_directory(trimmed) {
                Ok(path) => {
                    self.view = PermissionsView::AddDirectoryScope {
                        path,
                        text,
                        selected: self.default_scope_index(),
                        error: None,
                    };
                }
                Err(error) => {
                    self.view = PermissionsView::AddDirectoryInput {
                        text,
                        error: Some(error),
                    };
                }
            }
            return;
        }
        if let Some(input) = Self::printable_input(key) {
            text.push_str(&input);
            self.view = PermissionsView::AddDirectoryInput { text, error: None };
        }
    }

    /// 校验新目录（qwen 的顺序）：存在 → 是目录 → 不重复 → 不是已有根的子孙。
    fn validate_workspace_directory(&self, text: &str) -> Result<PathBuf, String> {
        let candidate = resolve_workspace_path(&self.cwd, Path::new(text));
        if !candidate.exists() {
            return Err("Directory does not exist.".to_string());
        }
        if !candidate.is_dir() {
            return Err("Path is not a directory.".to_string());
        }
        let roots = self.workspace_roots();
        for root in &roots {
            let root = resolve_workspace_path(&self.cwd, root);
            if root == candidate {
                return Err("This directory is already in the workspace.".to_string());
            }
        }
        for root in &roots {
            let root = resolve_workspace_path(&self.cwd, root);
            if candidate.starts_with(&root) {
                return Err("This directory is inside an already-included directory.".to_string());
            }
        }
        Ok(candidate)
    }

    fn handle_add_directory_scope(&mut self, key: &str) {
        let PermissionsView::AddDirectoryScope {
            path,
            text,
            selected,
            ..
        } = self.view.clone()
        else {
            return;
        };
        let len = PermissionRuleScopeChoice::ALL.len();
        if get_keybindings().matches(key, "tui.select.up") {
            let next = if selected == 0 { len - 1 } else { selected - 1 };
            self.view = PermissionsView::AddDirectoryScope {
                path,
                text,
                selected: next,
                error: None,
            };
            return;
        }
        if get_keybindings().matches(key, "tui.select.down") {
            let next = if selected + 1 >= len { 0 } else { selected + 1 };
            self.view = PermissionsView::AddDirectoryScope {
                path,
                text,
                selected: next,
                error: None,
            };
            return;
        }
        if Self::is_cancel(key) {
            self.view = PermissionsView::AddDirectoryInput { text, error: None };
            return;
        }
        if !Self::is_confirm(key) {
            return;
        }
        let choice = PermissionRuleScopeChoice::ALL[selected.min(len - 1)];
        if self.snapshot.mode.is_none() {
            self.view = PermissionsView::AddDirectoryScope {
                path,
                text,
                selected,
                error: Some("Permission checks are disabled. Change the mode first.".to_string()),
            };
            return;
        }
        if choice.requires_trust() && !self.project_trusted {
            self.view = PermissionsView::AddDirectoryScope {
                path,
                text,
                selected,
                error: Some("Project is not trusted; cannot write.".to_string()),
            };
            return;
        }
        match (self.callbacks.on_add_directory)(&path, choice) {
            Ok(()) => {
                self.view = PermissionsView::WorkspaceList;
                self.notice = Some(PanelNotice::info(format!(
                    "Added directory {}",
                    path.display()
                )));
            }
            Err(error) => {
                self.view = PermissionsView::AddDirectoryScope {
                    path,
                    text,
                    selected,
                    error: Some(error),
                };
            }
        }
    }

    fn handle_remove_directory_confirm(&mut self, key: &str) {
        let PermissionsView::RemoveDirectoryConfirm {
            root,
            selected,
            error,
        } = self.view.clone()
        else {
            return;
        };
        if get_keybindings().matches(key, "tui.select.up")
            || get_keybindings().matches(key, "tui.select.down")
            || get_keybindings().matches(key, "tui.editor.cursorLeft")
            || get_keybindings().matches(key, "tui.editor.cursorRight")
        {
            self.view = PermissionsView::RemoveDirectoryConfirm {
                root,
                selected: if selected == 0 { 1 } else { 0 },
                error,
            };
            return;
        }
        if Self::is_cancel(key) || key == "n" {
            self.view = PermissionsView::WorkspaceList;
            return;
        }
        let yes = key == "y" || (Self::is_confirm(key) && selected == 1);
        if !yes {
            if Self::is_confirm(key) && selected == 0 {
                self.view = PermissionsView::WorkspaceList;
            }
            return;
        }
        let removable = self
            .workspace_roots()
            .iter()
            .position(|candidate| candidate == &root)
            .map(|index| self.workspace_root_removable(index))
            .unwrap_or(false);
        if !removable {
            self.view = PermissionsView::WorkspaceList;
            self.notice = Some(PanelNotice::error(
                "This directory comes from managed policy or the CLI and cannot be removed here.",
            ));
            return;
        }
        match (self.callbacks.on_remove_directory)(&root) {
            Ok(()) => {
                self.view = PermissionsView::WorkspaceList;
                self.notice = Some(PanelNotice::info(format!(
                    "Removed directory {}",
                    root.display()
                )));
            }
            Err(error) => {
                self.view = PermissionsView::RemoveDirectoryConfirm {
                    root,
                    selected,
                    error: Some(error),
                };
            }
        }
    }

    // -- 渲染 ---------------------------------------------------------------

    fn render_tabs(&self) -> String {
        let active = self.current_tab();
        RuleTab::ALL
            .iter()
            .map(|tab| {
                if *tab == active {
                    fg(ThemeColor::Accent, &format!("[{}]", tab.label()))
                } else {
                    tab.label().to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("  ")
    }

    fn render_mode_line(&self) -> String {
        let mode = match self.snapshot.mode {
            Some(mode) => {
                let visual = permission_mode_visual(mode);
                let text = fg(visual.color, visual.text);
                if visual.bold { bold(&text) } else { text }
            }
            None => fg(ThemeColor::Dim, "off"),
        };
        format!(" Mode: {mode}   {}", fg(ThemeColor::Muted, "(m to change)"))
    }

    fn render_filter_line(&self, workspace: bool) -> Option<String> {
        let query = if workspace {
            &self.ws_query
        } else {
            &self.rule_query
        };
        if !self.filter_active && query.is_empty() {
            return None;
        }
        let cursor = if self.filter_active { "▏" } else { "" };
        Some(format!(
            " {} {}{cursor}",
            fg(ThemeColor::Accent, "filter:"),
            fg(ThemeColor::Text, query)
        ))
    }

    fn render_rule_list(&self) -> Vec<String> {
        let rows = self.rule_rows();
        let mut lines = Vec::new();
        let labels: Vec<String> = rows
            .iter()
            .map(|row| match row {
                RuleRow::Add => "+ Add rule…".to_string(),
                RuleRow::Rule(index) => display_rule_text(&self.snapshot.rules[*index].rule),
            })
            .collect();
        let label_width = labels
            .iter()
            .map(|label| visible_width(label))
            .max()
            .unwrap_or(0)
            .min(48);
        for (position, row) in rows.iter().enumerate() {
            let selected = position == self.selected_rule;
            let prefix = if selected {
                fg(ThemeColor::Accent, "→ ")
            } else {
                "  ".to_string()
            };
            let label = &labels[position];
            let padding = " ".repeat(label_width.saturating_sub(visible_width(label)) + 2);
            let description = match row {
                RuleRow::Add => String::new(),
                RuleRow::Rule(index) => self.describe_rule_entry(&self.snapshot.rules[*index]),
            };
            let styled_label = if selected {
                fg(ThemeColor::Accent, label)
            } else {
                fg(ThemeColor::Text, label)
            };
            if description.is_empty() {
                lines.push(format!("{prefix}{styled_label}"));
            } else {
                lines.push(format!(
                    "{prefix}{styled_label}{padding}{}",
                    fg(ThemeColor::Muted, &description)
                ));
            }
        }
        if rows.len() == 1 {
            lines.push(format!(
                "  {}",
                fg(ThemeColor::Muted, self.tab.empty_message())
            ));
        }
        lines
    }

    fn describe_rule_entry(&self, entry: &PermissionRuleEntry) -> String {
        let mut parts = Vec::new();
        if !entry.scopes.is_empty() {
            parts.push(
                entry
                    .scopes
                    .iter()
                    .map(|scope| scope.label())
                    .collect::<Vec<_>>()
                    .join(", "),
            );
        }
        if let Some(reason) = rule_reason(&entry.rule) {
            parts.push(reason.to_string());
        }
        parts.join(" · ")
    }

    fn render_workspace_list(&self) -> Vec<String> {
        let rows = self.workspace_rows();
        let mut lines = Vec::new();
        let roots = self.workspace_roots();
        let labels: Vec<String> = rows
            .iter()
            .map(|row| match row {
                WorkspaceRow::Add => "+ Add directory…".to_string(),
                WorkspaceRow::Root(index) => roots[*index].display().to_string(),
            })
            .collect();
        let label_width = labels
            .iter()
            .map(|label| visible_width(label))
            .max()
            .unwrap_or(0)
            .min(56);
        for (position, row) in rows.iter().enumerate() {
            let selected = position == self.selected_dir;
            let prefix = if selected {
                fg(ThemeColor::Accent, "→ ")
            } else {
                "  ".to_string()
            };
            let label = &labels[position];
            let padding = " ".repeat(label_width.saturating_sub(visible_width(label)) + 2);
            let description = match row {
                WorkspaceRow::Add => String::new(),
                WorkspaceRow::Root(0) => "working directory (always included)".to_string(),
                WorkspaceRow::Root(index) => self
                    .workspace_scopes(*index)
                    .iter()
                    .map(|scope| scope.label())
                    .collect::<Vec<_>>()
                    .join(", "),
            };
            let styled_label = if selected {
                fg(ThemeColor::Accent, label)
            } else {
                fg(ThemeColor::Text, label)
            };
            if description.is_empty() {
                lines.push(format!("{prefix}{styled_label}"));
            } else {
                lines.push(format!(
                    "{prefix}{styled_label}{padding}{}",
                    fg(ThemeColor::Muted, &description)
                ));
            }
        }
        lines
    }

    fn render_scope_choices(&self, selected: usize) -> Vec<String> {
        let mut lines = Vec::new();
        for (index, choice) in PermissionRuleScopeChoice::ALL.iter().enumerate() {
            let disabled = choice.requires_trust() && !self.project_trusted;
            let marker = if index == selected {
                fg(ThemeColor::Accent, "→ ")
            } else {
                "  ".to_string()
            };
            let mut line = format!(
                "{marker}{}  {}",
                choice.label(),
                fg(ThemeColor::Muted, choice.file_description())
            );
            if disabled {
                line.push_str(&format!(
                    "  {}",
                    fg(ThemeColor::Warning, "Project is not trusted; cannot write.")
                ));
            }
            lines.push(line);
        }
        lines
    }

    fn render_mode_choices(&self, selected: usize) -> Vec<String> {
        let mut lines = Vec::new();
        for (index, mode) in [
            PermissionMode::ReadOnly,
            PermissionMode::Default,
            PermissionMode::AcceptEdits,
            PermissionMode::BypassPermissions,
        ]
        .iter()
        .enumerate()
        {
            let marker = if index == selected {
                fg(ThemeColor::Accent, "→ ")
            } else {
                "  ".to_string()
            };
            let visual = permission_mode_visual(*mode);
            let text = fg(visual.color, visual.text);
            let mut line = format!(
                "{marker}{text}  {}",
                fg(ThemeColor::Muted, mode.description())
            );
            if mode.is_bypass() && self.snapshot.bypass_disabled {
                line.push_str(&format!(
                    "  {}",
                    fg(ThemeColor::Warning, "Disabled by policy.")
                ));
            }
            lines.push(line);
        }
        lines
    }

    fn render_mode_scope_choices(&self, selected: usize) -> Vec<String> {
        let mut lines = Vec::new();
        for (index, scope) in ModePersistenceScope::ALL.iter().enumerate() {
            let marker = if index == selected {
                fg(ThemeColor::Accent, "→ ")
            } else {
                "  ".to_string()
            };
            let mut line = format!(
                "{marker}{}  {}",
                scope.label(),
                fg(ThemeColor::Muted, scope.description())
            );
            if scope.requires_trust() && !self.project_trusted {
                line.push_str(&format!(
                    "  {}",
                    fg(ThemeColor::Warning, "Project is not trusted; cannot write.")
                ));
            }
            lines.push(line);
        }
        lines
    }

    fn render_confirm_choice(&self, selected: usize) -> String {
        let checkbox = |checked: bool| if checked { "[x]" } else { "[ ]" };
        format!(
            "  {} No   {} Yes",
            checkbox(selected == 0),
            checkbox(selected == 1)
        )
    }

    fn render_body(&self) -> Vec<String> {
        let mut lines = Vec::new();
        lines.push(format!(" {}", fg(ThemeColor::Accent, &bold("Permissions"))));
        lines.push(self.render_mode_line());
        if self.snapshot.session_allow_count > 0 {
            lines.push(format!(
                " {} {}",
                fg(ThemeColor::Muted, "Session approvals:"),
                fg(
                    ThemeColor::Text,
                    &self.snapshot.session_allow_count.to_string()
                )
            ));
        }
        for error in &self.snapshot.errors {
            lines.push(fg(ThemeColor::Warning, &format!(" ⚠ {error}")));
        }
        lines.push(format!(" {}", self.render_tabs()));
        if let Some(notice) = &self.notice {
            let (color, label) = if notice.is_error() {
                (ThemeColor::Error, "Error:")
            } else {
                (ThemeColor::Success, "Info:")
            };
            lines.push(format!(
                " {} {}",
                fg(color, label),
                fg(color, notice.text())
            ));
        }

        match &self.view {
            PermissionsView::RuleList { .. } => {
                if let Some(filter) = self.render_filter_line(false) {
                    lines.push(filter);
                }
                lines.extend(self.render_rule_list());
                lines.push(String::new());
                lines.push(format!(
                    " {}",
                    fg(
                        ThemeColor::Muted,
                        "Enter select · Tab switch · Esc close · / filter · m mode · r refresh · d delete"
                    )
                ));
            }
            PermissionsView::WorkspaceList => {
                if let Some(filter) = self.render_filter_line(true) {
                    lines.push(filter);
                }
                lines.extend(self.render_workspace_list());
                lines.push(String::new());
                lines.push(format!(
                    " {}",
                    fg(
                        ThemeColor::Muted,
                        "Enter select · Tab switch · Esc close · / filter · m mode · r refresh · d remove"
                    )
                ));
            }
            PermissionsView::ModeSelect { selected, .. } => {
                lines.push(format!(
                    " {}",
                    fg(ThemeColor::Text, "Select permission mode")
                ));
                lines.extend(self.render_mode_choices(*selected));
                lines.push(String::new());
                lines.push(format!(
                    " {}",
                    fg(ThemeColor::Muted, "↑↓ navigate · Enter select · Esc back")
                ));
            }
            PermissionsView::ModeScopeSelect { mode, selected, .. } => {
                lines.push(format!(
                    " {} {}",
                    fg(ThemeColor::Text, "Save permission mode"),
                    fg(ThemeColor::Accent, mode.as_str())
                ));
                lines.extend(self.render_mode_scope_choices(*selected));
                lines.push(String::new());
                lines.push(format!(
                    " {}",
                    fg(ThemeColor::Muted, "↑↓ navigate · Enter apply · Esc back")
                ));
            }
            PermissionsView::AddRuleInput { text, error } => {
                lines.push(format!(
                    " {}",
                    fg(
                        ThemeColor::Text,
                        "Enter permission rule (tool:<name>, command:<prefix>, path:<dir>, or a bare tool name)"
                    )
                ));
                lines.push(format!(
                    " {} {}{}",
                    fg(ThemeColor::Accent, ">"),
                    fg(ThemeColor::Text, text),
                    fg(ThemeColor::Muted, "▏")
                ));
                if let Some(error) = error {
                    lines.push(fg(ThemeColor::Error, &format!(" {error}")));
                }
                lines.push(String::new());
                lines.push(format!(
                    " {}",
                    fg(ThemeColor::Muted, "Enter confirm · Esc back")
                ));
            }
            PermissionsView::AddRuleScope {
                rule,
                selected,
                error,
                ..
            } => {
                lines.push(format!(
                    " {} {}",
                    fg(ThemeColor::Text, "Add rule to"),
                    fg(ThemeColor::Accent, &display_rule_text(rule))
                ));
                lines.extend(self.render_scope_choices(*selected));
                if let Some(error) = error {
                    lines.push(fg(ThemeColor::Error, &format!(" {error}")));
                }
                lines.push(String::new());
                lines.push(format!(
                    " {}",
                    fg(ThemeColor::Muted, "↑↓ navigate · Enter confirm · Esc back")
                ));
            }
            PermissionsView::DeleteRuleConfirm {
                entry,
                selected,
                error,
            } => {
                lines.push(format!(
                    " {}",
                    fg(ThemeColor::Text, &bold("Delete permission rule?"))
                ));
                lines.push(format!(
                    "   {}",
                    fg(ThemeColor::Text, &display_rule_text(&entry.rule))
                ));
                lines.push(format!(
                    "   {} {}",
                    fg(ThemeColor::Muted, "scope:"),
                    fg(
                        ThemeColor::Text,
                        &entry
                            .scopes
                            .iter()
                            .map(|scope| scope.label())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                ));
                if let Some(reason) = rule_reason(&entry.rule) {
                    lines.push(format!(
                        "   {} {}",
                        fg(ThemeColor::Muted, "reason:"),
                        fg(ThemeColor::Text, reason)
                    ));
                }
                if !entry.removable {
                    lines.push(fg(
                        ThemeColor::Warning,
                        " This rule is enforced by managed policy or the CLI and cannot be removed here.",
                    ));
                }
                lines.push(self.render_confirm_choice(*selected));
                if let Some(error) = error {
                    lines.push(fg(ThemeColor::Error, &format!(" {error}")));
                }
                lines.push(String::new());
                lines.push(format!(
                    " {}",
                    fg(ThemeColor::Muted, "↑↓ choose · Enter confirm · Esc cancel")
                ));
            }
            PermissionsView::AddDirectoryInput { text, error } => {
                lines.push(format!(
                    " {}",
                    fg(ThemeColor::Text, "Add directory to workspace")
                ));
                lines.push(format!(
                    " {} {}{}",
                    fg(ThemeColor::Accent, ">"),
                    fg(ThemeColor::Text, text),
                    fg(ThemeColor::Muted, "▏")
                ));
                if let Some(error) = error {
                    lines.push(fg(ThemeColor::Error, &format!(" {error}")));
                }
                lines.push(String::new());
                lines.push(format!(
                    " {}",
                    fg(ThemeColor::Muted, "Enter add · Esc back")
                ));
            }
            PermissionsView::AddDirectoryScope {
                path,
                selected,
                error,
                ..
            } => {
                lines.push(format!(
                    " {} {}",
                    fg(ThemeColor::Text, "Add directory to"),
                    fg(ThemeColor::Accent, &path.display().to_string())
                ));
                lines.extend(self.render_scope_choices(*selected));
                if let Some(error) = error {
                    lines.push(fg(ThemeColor::Error, &format!(" {error}")));
                }
                lines.push(String::new());
                lines.push(format!(
                    " {}",
                    fg(ThemeColor::Muted, "↑↓ navigate · Enter confirm · Esc back")
                ));
            }
            PermissionsView::RemoveDirectoryConfirm {
                root,
                selected,
                error,
            } => {
                lines.push(format!(
                    " {}",
                    fg(ThemeColor::Text, &bold("Remove directory from workspace?"))
                ));
                lines.push(format!(
                    "   {}",
                    fg(ThemeColor::Text, &root.display().to_string())
                ));
                if !self
                    .workspace_roots()
                    .iter()
                    .position(|candidate| candidate == root)
                    .map(|index| self.workspace_root_removable(index))
                    .unwrap_or(false)
                {
                    lines.push(fg(
                        ThemeColor::Warning,
                        " This directory comes from managed policy or the CLI and cannot be removed here.",
                    ));
                }
                lines.push(self.render_confirm_choice(*selected));
                if let Some(error) = error {
                    lines.push(fg(ThemeColor::Error, &format!(" {error}")));
                }
                lines.push(String::new());
                lines.push(format!(
                    " {}",
                    fg(ThemeColor::Muted, "↑↓ choose · Enter confirm · Esc cancel")
                ));
            }
        }

        lines
    }
}

impl Component for PermissionsDialog {
    fn render(&mut self, width: usize) -> Vec<String> {
        let mut lines = Vec::new();
        lines.push(fg(ThemeColor::BorderMuted, &"─".repeat(width)));
        for line in self.render_body() {
            lines.push(truncate_to_width_default(&line, width));
        }
        lines.push(fg(ThemeColor::BorderMuted, &"─".repeat(width)));
        lines
    }

    fn handle_input(&mut self, key_data: &str) {
        let view = self.view.clone();
        match view {
            PermissionsView::RuleList { .. } => self.handle_list_input(key_data, false),
            PermissionsView::WorkspaceList => self.handle_list_input(key_data, true),
            PermissionsView::ModeSelect { .. } => self.handle_mode_select_input(key_data),
            PermissionsView::ModeScopeSelect { .. } => self.handle_mode_scope_input(key_data),
            PermissionsView::AddRuleInput { .. } => self.handle_add_rule_input(key_data),
            PermissionsView::AddRuleScope { .. } => self.handle_add_rule_scope(key_data),
            PermissionsView::DeleteRuleConfirm { .. } => self.handle_delete_rule_confirm(key_data),
            PermissionsView::AddDirectoryInput { .. } => self.handle_add_directory_input(key_data),
            PermissionsView::AddDirectoryScope { .. } => self.handle_add_directory_scope(key_data),
            PermissionsView::RemoveDirectoryConfirm { .. } => {
                self.handle_remove_directory_confirm(key_data)
            }
        }
    }

    fn handles_input(&self) -> bool {
        true
    }

    fn as_focusable(&mut self) -> Option<&mut dyn Focusable> {
        Some(self)
    }

    fn as_any(&self) -> Option<&dyn Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn Any> {
        Some(self)
    }

    fn width(&self) -> Option<f64> {
        None
    }
}

impl Focusable for PermissionsDialog {
    fn focused(&self) -> bool {
        self.focused
    }

    fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }
}

/// 模式在 `ModeSelect` 列表里的顺序（与 [`PermissionMode`] 的语义顺序一致）。
fn mode_index(mode: PermissionMode) -> usize {
    match mode {
        PermissionMode::ReadOnly => 0,
        PermissionMode::Default => 1,
        PermissionMode::AcceptEdits => 2,
        PermissionMode::BypassPermissions => 3,
    }
}

/// 下标 → 模式；越界回绕。
fn mode_at(index: usize) -> PermissionMode {
    match index % 4 {
        0 => PermissionMode::ReadOnly,
        1 => PermissionMode::Default,
        2 => PermissionMode::AcceptEdits,
        _ => PermissionMode::BypassPermissions,
    }
}

/// 主题前景色；主题未初始化时退化为无颜色（面板不 panic）。
fn fg(color: ThemeColor, text: &str) -> String {
    THEME.fg(color, text).unwrap_or_else(|_| text.to_string())
}

/// 主题粗体；主题未初始化时退化为原文本。
fn bold(text: &str) -> String {
    THEME.bold(text).unwrap_or_else(|_| text.to_string())
}

/// 按会话 cwd 解析工作区路径：与权限引擎使用同一套解析（`~`、`$HOME`、`@`、
/// unicode 空格、`..` 的符号链接语义），否则面板校验/归因会与引擎判定分叉。
/// 空路径保持为空。
fn resolve_workspace_path(cwd: &Path, path: &Path) -> PathBuf {
    if path.as_os_str().is_empty() {
        return path.to_path_buf();
    }
    kiana_permissions::resolve_permission_path(cwd, &path.to_string_lossy())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_paths_use_the_engine_resolution() {
        let cwd = Path::new("/repo");
        // CLI 的 `@` 前缀必须与工具侧/权限引擎一样被去掉，否则合法目录会被判成
        // “不存在”（`/repo/@/tmp/extra`）。
        assert_eq!(
            resolve_workspace_path(cwd, Path::new("@/tmp/extra")),
            PathBuf::from("/tmp/extra")
        );
        assert_eq!(
            resolve_workspace_path(cwd, Path::new("generated")),
            PathBuf::from("/repo/generated")
        );
        assert_eq!(resolve_workspace_path(cwd, Path::new("")), PathBuf::new());
    }
}
