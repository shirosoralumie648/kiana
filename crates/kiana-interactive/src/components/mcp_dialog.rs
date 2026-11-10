//! `/mcp` 管理对话框：MCP 服务器列表、启用/禁用、工具数量、最近错误与登录/登出入口。
//!
//! 交互骨架参考 qwen-code `packages/cli/src/ui/components/mcp/MCPManagementDialog.tsx`
//! （导航栈 `server-list/detail/tool-list`，只有列表页的 Esc 关闭面板），数据模型按 Kiana
//! 现有的 MCP 配置与诊断 API 重写。Kiana 的 `/mcp` 已由内建扩展实现
//! （`kiana-builtin-extensions/src/mcp/{index,ui}.rs`），本组件是产品增量里的**独立面板**，
//! 由命令层挂到编辑器槽位；它不替代扩展命令，也不注册新的斜杠命令。
//!
//! # 这个模块解决什么问题
//!
//! MCP 服务器分散在两个配置文件里（全局 `<agent_dir>/mcp.json` 与项目
//! `<cwd>/.kiana/mcp.json`），启用状态、工具数量、连接错误和 OAuth 登录状态又只在运行期
//! 可见。用户没有一处可以同时看到“配置里有什么”和“现在连上了什么”。本组件把两者合并成
//! 一个可键盘操作的列表：
//!
//! - 列表页：每行是 `状态 · 工具数 · 来源`，需要处理的服务器排前面；
//! - 详情页：transport、来源文件、项目覆盖、状态、exposure、工具数、最近错误；
//! - 工具页：服务器上报的工具名与 exposure 覆盖；
//! - 启停、登录、登出、刷新全部通过回调交给宿主，组件自己不碰连接与 OAuth。
//!
//! # 数据源（为什么不调用 `kiana mcp list --json`）
//!
//! 设计文档 `docs/history/porting/tui-panels-design.md` §5.2 已明确：**不在 TUI 面板里
//! 调用 `kiana mcp list --json`**。该命令会为每台服务器建立一次性连接（stdio 子进程、
//! HTTP 请求、OAuth 刷新），失败时以退出码 1 结束，而且与活会话的连接状态重复。
//!
//! 本组件改用两条已有的公开 API：
//!
//! - **配置**：[`load_mcp_config`]（`McpServerEntry` / `LoadedMcpConfig`）读取并合并全局与
//!   项目 `mcp.json`。它只读文件，不解析连接、不访问网络。
//! - **运行期状态**：[`McpServerStatus`]（`kiana-builtin-extensions` 的稳定诊断快照），
//!   字段与 `kiana mcp list --json` 的 `ServerReport` 对齐：`state`、`last_error`、
//!   `tool_count`、`prompt_count`。宿主从活会话的 `McpServerConnection::status(enabled)`
//!   取得后通过 [`McpServerRow::apply_status`] 回填。
//!
//! 因此组件既不会重复 CLI 的连接逻辑，也不会把“配置错误”当成崩溃：配置文件读取失败只
//! 进 [`McpDialogSnapshot::errors`]，在列表顶部以错误色展示。
//!
//! # 启用/禁用写在哪里
//!
//! `mcp.json` 的字段是 `enabled`（缺省 `true`，`false` 表示保留条目但不连接），**不是**
//! `disabled`。仓库里已有写盘 setter：[`update_mcp_server_config`] +
//! `McpServerConfigPatch::enabled`（`kiana-builtin-extensions/src/mcp/config.rs`）。
//! [`write_mcp_server_enabled`] 只是把通道层的“目标文件选择”拼在它前面：
//! `override_path ?? source`，与扩展的 `/mcp` 保存动作（`default_update_config`）一致。
//! 扩展注册的条目（[`McpServerScope::Extension`]）没有可写文件，直接返回错误。
//!
//! # 状态机
//!
//! ```text
//! ServerList ──Enter──> ServerDetail ──Enter(Tools)──> ToolList
//!     │                    │  ↑ ↓ 在动作间移动          │
//!     │                    └────────── Esc ────────────┘
//!     └──Esc（关闭面板）        （ToolList 的 Esc 回详情，详情的 Esc 回列表）
//! ```
//!
//! - 列表页：`↑`/`↓`/PageUp/PageDown 移动，Enter 打开详情，`e` 启停，`l` 登录，`o` 登出，
//!   `r` 刷新，Esc 关闭。
//! - 详情页：`↑`/`↓` 在动作间移动，Enter 执行动作；`e`/`l`/`o`/`r` 快捷键与列表页相同；
//!   Esc 返回列表。
//! - 工具页：`↑`/`↓` 移动，Esc 返回详情。
//!
//! 快捷键只在动作**可用**时调用回调：例如 `l` 仅对 `needs-auth` 的已启用服务器生效，
//! `o` 仅对已连接的服务器生效；否则设置一条 notice 说明原因。这与扩展 `/mcp` 菜单只在
//! 对应状态显示 `Sign in` / `Sign out` 的行为一致（登录/登出回调本身仍由宿主实现）。
//!
//! # 边界与失败模式
//!
//! - 组件不持有连接、不读凭据、不写 `mcp.json`：所有副作用经 [`McpDialogCallbacks`] 回到
//!   宿主。回调返回 `Err(String)` 时面板保留现状并显示错误，便于原地重试。
//! - [`write_mcp_server_enabled`] 是给宿主用的便利函数，错误文案带
//!   `Could not update <path>: ` 前缀（与扩展的 `/mcp` 保存路径一致）。
//! - 主题未初始化时渲染退化为无颜色文本而不是 panic（面板不是启动路径的一环）。
//! - 组件只处理键盘；鼠标事件不在 P0 范围（与 `/permissions` 面板一致）。
//!
//! # 与共享文件的集成
//!
//! 本文件由 Integrate 环节统一在 `components/mod.rs` 声明；在此之前，集成测试通过
//! `#[path = "../src/components/mcp_dialog.rs"]` 把源文件直接编进测试 crate（与
//! `tests/permissions_dialog.rs` 的引入方式相同）。文件内部不引用
//! `crate::components::…` 的其它组件，因此测试不需要额外的命名空间垫片。

use std::any::Any;
use std::path::Path;

use indexmap::IndexMap;
use kiana_builtin_extensions::mcp::config::{
    LoadMcpConfigOptions, LoadedMcpConfig, McpExposure, McpServerConfig, McpServerConfigPatch,
    McpServerEntry, McpServerScope, UpdateMcpServerConfigOptions, load_mcp_config,
    update_mcp_server_config,
};
use kiana_builtin_extensions::mcp::runtime::{McpServerStatus, McpServerStatusState};
use kiana_theme::{THEME, ThemeColor};
use kiana_tui::autocomplete::locale_compare;
use kiana_tui::keybindings::get_keybindings;
use kiana_tui::text::{truncate_to_width_default, visible_width};
use kiana_tui::tui::{Component, Focusable};
use serde_json::Value;

/// 一次 PageUp/PageDown 移动的行数（与 `/permissions` 面板一致）。
const PAGE_SIZE: usize = 8;

/// 服务器名在列表里的最小/最大列宽。
const NAME_COLUMN_MIN: usize = 8;
const NAME_COLUMN_MAX: usize = 24;

/// 单个 MCP 服务器在面板里的完整快照。
///
/// 字段与 `kiana mcp list --json` 的 `ServerReport` 一一对应，只是把 `tools`（名字数组）
/// 与运行期 `status`（计数与最近错误）分成两处：
///
/// - 配置字段（`name`/`scope`/`source`/`override_path`/`enabled`/`exposure`/`transport`/
///   `tool_exposure`）由 [`McpServerRow::from_entry`] 从 [`McpServerEntry`] 构造；
/// - 运行期字段（`status`/`tools`/`resources`）由宿主从活会话取得后
///   [`McpServerRow::apply_status`] 回填。没有活会话时 `status = None`，列表显示
///   `configured` 而不是伪造 `connecting`。
#[derive(Debug, Clone, PartialEq)]
pub struct McpServerRow {
    /// 服务器名（`mcpServers` 的键）。
    pub name: String,
    /// 条目来源：全局 / 项目 / 扩展注册。
    pub scope: McpServerScope,
    /// 定义该条目的配置文件，或注册它的扩展路径。
    pub source: String,
    /// 覆盖这个全局服务器的项目 `mcp.json`（`override` 字段）。
    pub override_path: Option<String>,
    /// 是否启用（`enabled != Some(false)`）。
    pub enabled: bool,
    /// 默认工具暴露方式（缺省 `codemode`）。
    pub exposure: McpExposure,
    /// 人类可读的 transport：HTTP 的 URL，或 `command arg1 arg2`。
    pub transport: String,
    /// 运行期诊断快照；`None` 表示只有配置、没有活会话。
    pub status: Option<McpServerStatus>,
    /// 服务器上报的工具名（`tools/list`）。没有连接时为空。
    pub tools: Vec<String>,
    /// 配置里的单工具 exposure 覆盖（`toolExposure`）。
    pub tool_exposure: IndexMap<String, McpExposure>,
    /// 服务器是否提供 resources（`ServerReport.resources` 的布尔化）。
    pub resources: bool,
}

impl McpServerRow {
    /// 从配置条目构造一行（没有运行期状态）。
    ///
    /// `scope` 缺失时按 `global` 处理（与 `kiana mcp list` 的 `scope ?? "global"` 一致）；
    /// `enabled` 缺省 `true`（`enabled: false` 才禁用）；`exposure` 缺省 `codemode`。
    pub fn from_entry(entry: &McpServerEntry) -> Self {
        let base = entry.config.base();
        Self {
            name: entry.name.clone(),
            scope: entry.scope.unwrap_or(McpServerScope::Global),
            source: entry.source.clone(),
            override_path: entry.override_.clone(),
            enabled: base.enabled != Some(false),
            exposure: base.exposure.unwrap_or(McpExposure::Codemode),
            transport: transport_label(&entry.config),
            status: None,
            tools: Vec::new(),
            tool_exposure: base.tool_exposure.clone().unwrap_or_default(),
            resources: false,
        }
    }

    /// 回填运行期诊断快照。
    ///
    /// 只覆盖 `status`；工具名列表与 resources 由调用方另行设置（它们来自
    /// `McpServerConnection::tools()` / `has_resources()`，`McpServerStatus` 只带计数）。
    pub fn apply_status(&mut self, status: McpServerStatus) {
        self.status = Some(status);
    }

    /// 生效状态：禁用时永远是 `disabled`，否则取运行期状态；没有运行期状态时为 `None`。
    pub fn effective_state(&self) -> Option<McpServerStatusState> {
        if !self.enabled {
            return Some(McpServerStatusState::Disabled);
        }
        self.status.as_ref().map(|status| status.state)
    }

    /// 工具数：禁用的服务器为 0（配置保留但不会连接）；启用时以运行期快照为准，
    /// 没有快照则退化为工具名数组的长度。
    pub fn tool_count(&self) -> usize {
        if !self.enabled {
            return 0;
        }
        self.status
            .as_ref()
            .map(|status| status.tool_count)
            .unwrap_or(self.tools.len())
    }

    /// prompt 数；禁用的服务器或没有运行期快照时为 0。
    pub fn prompt_count(&self) -> usize {
        if !self.enabled {
            return 0;
        }
        self.status
            .as_ref()
            .map(|status| status.prompt_count)
            .unwrap_or(0)
    }

    /// 最近一次失败原因；只有运行期快照会提供。
    pub fn last_error(&self) -> Option<&str> {
        self.status
            .as_ref()
            .and_then(|status| status.last_error.as_deref())
    }
}

/// 面板打开时的一份快照。
///
/// [`McpDialogSnapshot::load`] 直接复用 [`load_mcp_config`]；[`from_loaded_config`] 是给
/// 已经拿到 [`LoadedMcpConfig`] 的调用方（或测试）用的纯转换。两个字段
/// `global_config`/`project_config` 只用于空态文案与详情来源展示。
///
/// [`from_loaded_config`]: McpDialogSnapshot::from_loaded_config
#[derive(Debug, Clone, Default, PartialEq)]
pub struct McpDialogSnapshot {
    /// 合并后的服务器（已按“需要处理优先”排序）。
    pub servers: Vec<McpServerRow>,
    /// `mcp.json` 读取/校验错误（逐字，含文件路径前缀）。
    pub errors: Vec<String>,
    /// 全局 `mcp.json` 路径（`<agent_dir>/mcp.json`）。
    pub global_config: Option<String>,
    /// 项目 `mcp.json` 路径（项目被信任时才存在）。
    pub project_config: Option<String>,
}

impl McpDialogSnapshot {
    /// 从已经读好的配置构造快照（纯函数，不碰文件）。
    ///
    /// 排序与扩展 `/mcp` 的服务器菜单一致：`attention_rank` 升序（需要用户处理的在前），
    /// 同档按 [`locale_compare`]。
    pub fn from_loaded_config(loaded: LoadedMcpConfig) -> Self {
        let mut servers: Vec<McpServerRow> = loaded
            .servers
            .iter()
            .map(McpServerRow::from_entry)
            .collect();
        servers.sort_by(|left, right| {
            attention_rank(left)
                .cmp(&attention_rank(right))
                .then_with(|| locale_compare(&left.name, &right.name))
        });
        Self {
            servers,
            errors: loaded.errors,
            global_config: None,
            project_config: loaded.project_config,
        }
    }

    /// 用现有的配置读取 API 载入全局 + 项目 `mcp.json`。
    ///
    /// 这是面板唯一会读文件的地方；`project_trusted = false` 时项目文件不会被读取
    /// （`load_mcp_config` 的既有语义）。
    pub fn load(options: LoadMcpConfigOptions) -> Self {
        let global_config = options
            .agent_dir
            .join("mcp.json")
            .to_string_lossy()
            .into_owned();
        let loaded = load_mcp_config(options);
        let mut snapshot = Self::from_loaded_config(loaded);
        snapshot.global_config = Some(global_config);
        snapshot
    }
}

/// 列表行/详情的状态标记与颜色。
fn state_marker(row: &McpServerRow) -> (&'static str, ThemeColor) {
    match row.effective_state() {
        Some(McpServerStatusState::Connected) => ("●", ThemeColor::Success),
        Some(McpServerStatusState::NeedsAuth) => ("?", ThemeColor::Warning),
        Some(McpServerStatusState::Failed) => ("✗", ThemeColor::Error),
        Some(McpServerStatusState::Connecting) => ("…", ThemeColor::Warning),
        Some(McpServerStatusState::Disabled) => ("○", ThemeColor::Dim),
        None => ("·", ThemeColor::Dim),
    }
}

/// 状态文案（与扩展 `describe_state` 的措辞一致）。
fn state_text(row: &McpServerRow) -> &'static str {
    match row.effective_state() {
        Some(McpServerStatusState::Connected) => "connected",
        Some(McpServerStatusState::NeedsAuth) => "needs sign-in",
        Some(McpServerStatusState::Failed) => "failed",
        Some(McpServerStatusState::Connecting) => "connecting…",
        Some(McpServerStatusState::Disabled) => "disabled",
        None => "configured",
    }
}

/// `N tool` / `N tools`。
fn tools_text(count: usize) -> String {
    format!("{count} tool{}", if count == 1 { "" } else { "s" })
}

/// 来源列：有项目覆盖时固定写 `global, project override`（与扩展菜单一致）。
fn scope_text(row: &McpServerRow) -> String {
    if row.override_path.is_some() {
        "global, project override".to_string()
    } else {
        scope_label(row.scope).to_string()
    }
}

/// [`McpServerScope`] 的字符串字面量。
pub fn scope_label(scope: McpServerScope) -> &'static str {
    match scope {
        McpServerScope::Global => "global",
        McpServerScope::Project => "project",
        McpServerScope::Extension => "extension",
    }
}

/// `attention_rank`：需要用户先处理的服务器排前面。
///
/// 数值越小越靠前：`needs-auth(0) < failed(1) < connecting/未知(2/3) < connected(4) <
/// disabled(5)`。没有运行期状态的配置行按 `3` 处理（与扩展的“starting”同档）。
fn attention_rank(row: &McpServerRow) -> u8 {
    if !row.enabled {
        return 5;
    }
    match row.status.as_ref().map(|status| status.state) {
        Some(McpServerStatusState::NeedsAuth) => 0,
        Some(McpServerStatusState::Failed) => 1,
        Some(McpServerStatusState::Connecting) => 2,
        Some(McpServerStatusState::Connected) => 4,
        Some(McpServerStatusState::Disabled) => 5,
        None => 3,
    }
}

/// 人类可读的 transport。
///
/// HTTP 条目显示 URL；stdio 条目显示 `command arg1 arg2`。与扩展的 `describe_transport`
/// 一样优先看 `"url" in config`：校验后的 stdio 条目也可能带一个未识别的 `url` 键，此时
/// 按该值的 JS 字符串显示（`null` 显示为空串）。
fn transport_label(config: &McpServerConfig) -> String {
    match config {
        McpServerConfig::Http(config) => config.url.clone(),
        McpServerConfig::Stdio(config) => {
            if let Some(url) = config.extra.get("url") {
                return match url {
                    Value::Null => String::new(),
                    other => kiana_json::js_string::js_string(Some(other)),
                };
            }
            let mut parts = vec![config.command.clone()];
            if let Some(args) = &config.args {
                parts.extend(args.iter().cloned());
            }
            parts.join(" ")
        }
    }
}

/// 详情页的一个动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpServerAction {
    /// 启用服务器（写 `enabled: true`）。
    Enable,
    /// 禁用服务器（写 `enabled: false`）。
    Disable,
    /// 打开登录流程。
    SignIn,
    /// 删除已保存的凭据。
    SignOut,
    /// 查看工具列表。
    Tools,
    /// 返回服务器列表。
    Back,
}

impl McpServerAction {
    /// 动作标签（与扩展 `/mcp` 菜单的英文文案对齐）。
    pub fn label(self) -> &'static str {
        match self {
            Self::Enable => "Enable",
            Self::Disable => "Disable",
            Self::SignIn => "Sign in",
            Self::SignOut => "Sign out",
            Self::Tools => "Tools",
            Self::Back => "Back",
        }
    }
}

/// 某一行当前可用的动作（顺序即详情页的显示顺序）。
///
/// - 启用状态始终可切换；
/// - `Sign in` 只在已启用且 `needs-auth` 时出现，`Sign out` 只在已连接时出现；
/// - `Tools` 只在已连接时出现（与扩展只在 `Connected` 时给工具入口一致）；
/// - `Back` 永远最后。
pub fn server_actions(row: &McpServerRow) -> Vec<McpServerAction> {
    let mut actions = Vec::new();
    if row.enabled {
        actions.push(McpServerAction::Disable);
    } else {
        actions.push(McpServerAction::Enable);
    }
    if row.enabled {
        match row.effective_state() {
            Some(McpServerStatusState::NeedsAuth) => actions.push(McpServerAction::SignIn),
            Some(McpServerStatusState::Connected) => actions.push(McpServerAction::SignOut),
            _ => {}
        }
        if row.effective_state() == Some(McpServerStatusState::Connected) {
            actions.push(McpServerAction::Tools);
        }
    }
    actions.push(McpServerAction::Back);
    actions
}

/// 动作描述（详情页右侧的灰色说明）。
fn action_description(action: McpServerAction, row: &McpServerRow) -> String {
    match action {
        McpServerAction::Enable => "write enabled: true to mcp.json".to_string(),
        McpServerAction::Disable => "write enabled: false to mcp.json".to_string(),
        McpServerAction::SignIn => "opens the browser".to_string(),
        McpServerAction::SignOut => "deletes the stored credentials".to_string(),
        McpServerAction::Tools => format!("{} offered", tools_text(row.tool_count())),
        McpServerAction::Back => "back to the server list".to_string(),
    }
}

/// 切换启用的写回调。
///
/// `enabled` 是**目标**状态（`true` = 启用，`false` = 禁用），不是“是否发生了切换”。
pub type McpToggleCallback = Box<dyn FnMut(&McpServerRow, bool) -> Result<(), String>>;

/// 登录回调。
pub type McpLoginCallback = Box<dyn FnMut(&McpServerRow) -> Result<(), String>>;

/// 登出回调。
pub type McpLogoutCallback = Box<dyn FnMut(&McpServerRow) -> Result<(), String>>;

/// 宿主提供的回调面。
///
/// 所有写操作都返回 `Result`：`Err(text)` 会显示在面板底部并保留当前视图；`Ok(())` 表示
/// 宿主已接受操作（组件随后在本地更新 `enabled` 并显示成功 notice）。登录/登出的异步进度
/// 与完成结果由宿主通过 [`McpDialog::set_notice`] 回填，组件不等待它们。
pub struct McpDialogCallbacks {
    /// 启用/禁用（宿主通常接 [`write_mcp_server_enabled`]）。
    pub on_toggle: McpToggleCallback,
    /// 登录入口。
    pub on_login: McpLoginCallback,
    /// 登出入口。
    pub on_logout: McpLogoutCallback,
    /// 重新载入快照（`r` 键）；宿主在回调里调用 [`McpDialog::set_snapshot`]。
    pub on_refresh: Box<dyn FnMut()>,
    /// 关闭面板（列表页 Esc）。
    pub on_close: Box<dyn FnMut()>,
}

impl Default for McpDialogCallbacks {
    fn default() -> Self {
        Self {
            on_toggle: Box::new(|_, _| Ok(())),
            on_login: Box::new(|_| Ok(())),
            on_logout: Box::new(|_| Ok(())),
            on_refresh: Box::new(|| {}),
            on_close: Box::new(|| {}),
        }
    }
}

/// [`McpDialog::new`] 的参数。
pub struct McpDialogOptions {
    /// 初始快照（[`McpDialogSnapshot::load`] 或宿主自己组装）。
    pub snapshot: McpDialogSnapshot,
    /// 宿主回调。
    pub callbacks: McpDialogCallbacks,
}

/// notice 的级别。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpNoticeKind {
    /// 普通信息（灰色）。
    Info,
    /// 错误（红色）。
    Error,
}

/// 面板底部的一行提示。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpNotice {
    /// 级别。
    pub kind: McpNoticeKind,
    /// 文案。
    pub message: String,
}

impl McpNotice {
    /// 信息提示。
    pub fn info(message: impl Into<String>) -> Self {
        Self {
            kind: McpNoticeKind::Info,
            message: message.into(),
        }
    }

    /// 错误提示。
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            kind: McpNoticeKind::Error,
            message: message.into(),
        }
    }
}

/// 面板当前视图。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpDialogView {
    /// 服务器列表。
    ServerList,
    /// 单个服务器的详情与动作菜单。
    ServerDetail {
        /// 服务器名（快照刷新后按名字重新定位）。
        name: String,
        /// 动作下标。
        selected: usize,
    },
    /// 工具列表。
    ToolList {
        /// 服务器名。
        name: String,
        /// 工具下标。
        selected: usize,
    },
}

impl McpDialogView {
    /// 视图对应的服务器名（列表页为 `None`）。
    pub fn server_name(&self) -> Option<&str> {
        match self {
            Self::ServerList => None,
            Self::ServerDetail { name, .. } | Self::ToolList { name, .. } => Some(name),
        }
    }
}

/// `/mcp` 管理对话框组件。
///
/// 实现 [`Component`] + [`Focusable`]：`handles_input()` 恒为 `true`，
/// `as_focusable()` 返回自身，挂载时用 `SelectorFocus::Component`。组件只保存快照与
/// 视图状态；所有副作用经 [`McpDialogCallbacks`] 交给宿主。
pub struct McpDialog {
    snapshot: McpDialogSnapshot,
    view: McpDialogView,
    /// 列表页的选中行。
    selected: usize,
    notice: Option<McpNotice>,
    callbacks: McpDialogCallbacks,
    focused: bool,
}

impl McpDialog {
    /// 构造面板。初始视图是服务器列表，选中第一行。
    pub fn new(options: McpDialogOptions) -> Self {
        let mut dialog = Self {
            snapshot: options.snapshot,
            view: McpDialogView::ServerList,
            selected: 0,
            notice: None,
            callbacks: options.callbacks,
            focused: false,
        };
        dialog.clamp_selection();
        dialog
    }

    // -- 只读访问（宿主与测试） ---------------------------------------------

    /// 当前快照。
    pub fn snapshot(&self) -> &McpDialogSnapshot {
        &self.snapshot
    }

    /// 当前视图。
    pub fn view(&self) -> &McpDialogView {
        &self.view
    }

    /// 列表页选中行下标。
    pub fn selected(&self) -> usize {
        self.selected
    }

    /// 列表页选中服务器名（空列表时为 `None`）。
    pub fn selected_name(&self) -> Option<&str> {
        self.snapshot
            .servers
            .get(self.selected)
            .map(|row| row.name.as_str())
    }

    /// 当前 notice。
    pub fn notice(&self) -> Option<&McpNotice> {
        self.notice.as_ref()
    }

    /// 当前服务器可用的动作（详情页的顺序；不在详情页时按列表选中行计算）。
    pub fn current_actions(&self) -> Vec<McpServerAction> {
        self.active_row().map(server_actions).unwrap_or_default()
    }

    /// 按名字查一行。
    pub fn row(&self, name: &str) -> Option<&McpServerRow> {
        self.snapshot.servers.iter().find(|row| row.name == name)
    }

    // -- 宿主回填 -----------------------------------------------------------

    /// 替换快照并尽量保持选中项。
    ///
    /// 选中行按服务器名保留（排序可能因状态变化而改变）；详情页/工具页正在看的服务器
    /// 消失时退回列表页。notice 不清空，便于宿主在刷新后继续展示上一条错误。
    pub fn set_snapshot(&mut self, snapshot: McpDialogSnapshot) {
        let previous_name = self
            .snapshot
            .servers
            .get(self.selected)
            .map(|row| row.name.clone());
        let viewing_name = self.view.server_name().map(str::to_string);
        self.snapshot = snapshot;
        if let Some(name) = previous_name
            && let Some(index) = self
                .snapshot
                .servers
                .iter()
                .position(|row| row.name == name)
        {
            self.selected = index;
        }
        self.clamp_selection();
        if let Some(name) = viewing_name {
            if self.row(&name).is_none() {
                self.view = McpDialogView::ServerList;
            } else {
                self.clamp_detail_selection(&name);
            }
        }
    }

    /// 设置 notice（宿主用于异步错误或进度提示）。
    pub fn set_notice(&mut self, notice: Option<McpNotice>) {
        self.notice = notice;
    }

    /// 清空 notice。
    pub fn clear_notice(&mut self) {
        self.notice = None;
    }

    // -- 内部：选中与视图 ---------------------------------------------------

    fn clamp_selection(&mut self) {
        let len = self.snapshot.servers.len();
        if len == 0 {
            self.selected = 0;
        } else if self.selected >= len {
            self.selected = len - 1;
        }
    }

    fn active_row_index(&self) -> Option<usize> {
        match &self.view {
            McpDialogView::ServerList => {
                (self.selected < self.snapshot.servers.len()).then_some(self.selected)
            }
            McpDialogView::ServerDetail { name, .. } | McpDialogView::ToolList { name, .. } => self
                .snapshot
                .servers
                .iter()
                .position(|row| row.name == *name),
        }
    }

    fn active_row(&self) -> Option<&McpServerRow> {
        self.active_row_index()
            .and_then(|index| self.snapshot.servers.get(index))
    }

    fn clamp_detail_selection(&mut self, name: &str) {
        let action_count = self
            .row(name)
            .map(|row| server_actions(row).len())
            .unwrap_or(0);
        let tool_count = self.row(name).map(|row| row.tools.len()).unwrap_or(0);
        match &mut self.view {
            McpDialogView::ServerDetail { selected, .. } => {
                if action_count == 0 {
                    *selected = 0;
                } else if *selected >= action_count {
                    *selected = action_count - 1;
                }
            }
            McpDialogView::ToolList { selected, .. } => {
                if tool_count == 0 {
                    *selected = 0;
                } else if *selected >= tool_count {
                    *selected = tool_count - 1;
                }
            }
            McpDialogView::ServerList => {}
        }
    }

    fn move_list_selection(&mut self, direction: isize, page: bool) {
        let len = self.snapshot.servers.len();
        if len == 0 {
            return;
        }
        let step = if page { PAGE_SIZE as isize } else { 1 };
        let current = self.selected.min(len - 1) as isize;
        let next = (current + direction.signum() * step).clamp(0, len as isize - 1);
        self.selected = next as usize;
    }

    fn move_detail_selection(&mut self, direction: isize, page: bool) {
        let Some(index) = self.active_row_index() else {
            return;
        };
        let count = server_actions(&self.snapshot.servers[index]).len();
        if count == 0 {
            return;
        }
        let step = if page { PAGE_SIZE as isize } else { 1 };
        let next = match &self.view {
            McpDialogView::ServerDetail { selected, .. } => {
                (*selected as isize + direction.signum() * step).clamp(0, count as isize - 1)
            }
            _ => return,
        };
        if let McpDialogView::ServerDetail { selected, .. } = &mut self.view {
            *selected = next as usize;
        }
    }

    fn move_tool_selection(&mut self, direction: isize, page: bool) {
        let Some(index) = self.active_row_index() else {
            return;
        };
        let count = self.snapshot.servers[index].tools.len();
        if count == 0 {
            return;
        }
        let step = if page { PAGE_SIZE as isize } else { 1 };
        let next = match &self.view {
            McpDialogView::ToolList { selected, .. } => {
                (*selected as isize + direction.signum() * step).clamp(0, count as isize - 1)
            }
            _ => return,
        };
        if let McpDialogView::ToolList { selected, .. } = &mut self.view {
            *selected = next as usize;
        }
    }

    // -- 内部：动作 ---------------------------------------------------------

    fn toggle_active(&mut self) {
        if let Some(row) = self.active_row().cloned() {
            self.set_enabled_for_row(&row.name, !row.enabled);
        }
    }

    fn set_enabled_for_row(&mut self, name: &str, enabled: bool) {
        let Some(row) = self.row(name).cloned() else {
            return;
        };
        match (self.callbacks.on_toggle)(&row, enabled) {
            Ok(()) => {
                if let Some(row) = self
                    .snapshot
                    .servers
                    .iter_mut()
                    .find(|row| row.name == name)
                {
                    row.enabled = enabled;
                    // 重新启用时，快照里的 `disabled` 是上一次禁用期间的诊断结果；
                    // 丢掉它，等宿主刷新后拿到真实的 connecting/connected。
                    if enabled
                        && row
                            .status
                            .as_ref()
                            .is_some_and(|status| status.state == McpServerStatusState::Disabled)
                    {
                        row.status = None;
                    }
                }
                self.notice = Some(McpNotice::info(format!(
                    "{} {}",
                    if enabled { "Enabled" } else { "Disabled" },
                    row.name
                )));
                self.clamp_detail_selection(name);
            }
            Err(message) => self.notice = Some(McpNotice::error(message)),
        }
    }

    fn sign_in_active(&mut self) {
        let Some(row) = self.active_row().cloned() else {
            return;
        };
        if !row.enabled {
            self.notice = Some(McpNotice::error("Enable the server before signing in."));
            return;
        }
        if row.effective_state() != Some(McpServerStatusState::NeedsAuth) {
            self.notice = Some(McpNotice::info(
                "Sign in is only available when the server needs authentication.",
            ));
            return;
        }
        match (self.callbacks.on_login)(&row) {
            Ok(()) => {
                self.notice = Some(McpNotice::info(format!(
                    "Sign-in requested for {}.",
                    row.name
                )))
            }
            Err(message) => self.notice = Some(McpNotice::error(message)),
        }
    }

    fn sign_out_active(&mut self) {
        let Some(row) = self.active_row().cloned() else {
            return;
        };
        if !row.enabled || row.effective_state() != Some(McpServerStatusState::Connected) {
            self.notice = Some(McpNotice::info(
                "Sign out is only available for a connected server.",
            ));
            return;
        }
        match (self.callbacks.on_logout)(&row) {
            Ok(()) => {
                self.notice = Some(McpNotice::info(format!(
                    "Sign-out requested for {}.",
                    row.name
                )))
            }
            Err(message) => self.notice = Some(McpNotice::error(message)),
        }
    }

    fn open_tools_active(&mut self) {
        let Some(row) = self.active_row().cloned() else {
            return;
        };
        if row.effective_state() != Some(McpServerStatusState::Connected) {
            self.notice = Some(McpNotice::info(
                "Tools are only available for a connected server.",
            ));
            return;
        }
        self.notice = None;
        self.view = McpDialogView::ToolList {
            name: row.name,
            selected: 0,
        };
    }

    fn refresh(&mut self) {
        (self.callbacks.on_refresh)();
    }

    // -- 内部：按键 ---------------------------------------------------------

    fn is_confirm(key: &str) -> bool {
        get_keybindings().matches(key, "tui.select.confirm") || key == "\n" || key == "\r"
    }

    fn is_cancel(key: &str) -> bool {
        get_keybindings().matches(key, "tui.select.cancel")
    }

    fn is_up(key: &str) -> bool {
        get_keybindings().matches(key, "tui.select.up")
    }

    fn is_down(key: &str) -> bool {
        get_keybindings().matches(key, "tui.select.down")
    }

    fn is_page_up(key: &str) -> bool {
        get_keybindings().matches(key, "tui.select.pageUp")
    }

    fn is_page_down(key: &str) -> bool {
        get_keybindings().matches(key, "tui.select.pageDown")
    }

    fn handle_list_input(&mut self, key: &str) {
        if Self::is_up(key) {
            self.move_list_selection(-1, false);
            return;
        }
        if Self::is_down(key) {
            self.move_list_selection(1, false);
            return;
        }
        if Self::is_page_up(key) {
            self.move_list_selection(-1, true);
            return;
        }
        if Self::is_page_down(key) {
            self.move_list_selection(1, true);
            return;
        }
        if Self::is_confirm(key) {
            if let Some(name) = self.selected_name().map(str::to_string) {
                self.notice = None;
                self.view = McpDialogView::ServerDetail { name, selected: 0 };
            }
            return;
        }
        if Self::is_cancel(key) {
            (self.callbacks.on_close)();
            return;
        }
        match key {
            "e" => self.toggle_active(),
            "l" => self.sign_in_active(),
            "o" => self.sign_out_active(),
            "r" => self.refresh(),
            "t" => self.open_tools_active(),
            _ => {}
        }
    }

    fn handle_detail_input(&mut self, key: &str) {
        if Self::is_up(key) {
            self.move_detail_selection(-1, false);
            return;
        }
        if Self::is_down(key) {
            self.move_detail_selection(1, false);
            return;
        }
        if Self::is_page_up(key) {
            self.move_detail_selection(-1, true);
            return;
        }
        if Self::is_page_down(key) {
            self.move_detail_selection(1, true);
            return;
        }
        if Self::is_confirm(key) {
            let action = match &self.view {
                McpDialogView::ServerDetail { selected, .. } => {
                    self.current_actions().get(*selected).copied()
                }
                _ => None,
            };
            self.execute_action(action);
            return;
        }
        if Self::is_cancel(key) {
            self.notice = None;
            self.view = McpDialogView::ServerList;
            return;
        }
        match key {
            "e" => self.toggle_active(),
            "l" => self.sign_in_active(),
            "o" => self.sign_out_active(),
            "r" => self.refresh(),
            "t" => self.open_tools_active(),
            _ => {}
        }
    }

    fn handle_tools_input(&mut self, key: &str) {
        if Self::is_up(key) {
            self.move_tool_selection(-1, false);
            return;
        }
        if Self::is_down(key) {
            self.move_tool_selection(1, false);
            return;
        }
        if Self::is_page_up(key) {
            self.move_tool_selection(-1, true);
            return;
        }
        if Self::is_page_down(key) {
            self.move_tool_selection(1, true);
            return;
        }
        if Self::is_cancel(key) {
            if let Some(name) = self.view.server_name().map(str::to_string) {
                self.view = McpDialogView::ServerDetail { name, selected: 0 };
            } else {
                self.view = McpDialogView::ServerList;
            }
        }
    }

    fn execute_action(&mut self, action: Option<McpServerAction>) {
        match action {
            Some(McpServerAction::Enable) => {
                if let Some(name) = self.active_row().map(|row| row.name.clone()) {
                    self.set_enabled_for_row(&name, true);
                }
            }
            Some(McpServerAction::Disable) => {
                if let Some(name) = self.active_row().map(|row| row.name.clone()) {
                    self.set_enabled_for_row(&name, false);
                }
            }
            Some(McpServerAction::SignIn) => self.sign_in_active(),
            Some(McpServerAction::SignOut) => self.sign_out_active(),
            Some(McpServerAction::Tools) => self.open_tools_active(),
            Some(McpServerAction::Back) => {
                self.notice = None;
                self.view = McpDialogView::ServerList;
            }
            None => {}
        }
    }

    // -- 内部：渲染 ---------------------------------------------------------

    fn name_column_width(&self) -> usize {
        self.snapshot
            .servers
            .iter()
            .map(|row| visible_width(&row.name))
            .max()
            .unwrap_or(0)
            .clamp(NAME_COLUMN_MIN, NAME_COLUMN_MAX)
    }

    fn render_list(&self) -> Vec<String> {
        let mut lines = Vec::new();
        lines.push(bold(&format!(
            "MCP servers ({})",
            self.snapshot.servers.len()
        )));
        if !self.snapshot.errors.is_empty() {
            lines.push(fg(ThemeColor::Error, "Config errors:"));
            for error in &self.snapshot.errors {
                lines.push(fg(ThemeColor::Error, &format!("  {error}")));
            }
        }
        if self.snapshot.servers.is_empty() {
            let message = match (
                self.snapshot.global_config.as_deref(),
                self.snapshot.project_config.as_deref(),
            ) {
                (Some(global), Some(project)) => {
                    format!("No MCP servers configured. Add them to {global} or {project}.")
                }
                (Some(global), None) => {
                    format!("No MCP servers configured. Add them to {global} or .kiana/mcp.json.")
                }
                _ => "No MCP servers configured. Add them to mcp.json (global) or .kiana/mcp.json."
                    .to_string(),
            };
            lines.push(fg(ThemeColor::Dim, &message));
        } else {
            let name_width = self.name_column_width();
            for (index, row) in self.snapshot.servers.iter().enumerate() {
                lines.push(render_row(row, index == self.selected, name_width));
                // 最近错误对每一行都可见（不只选中行）：列表常被用来判断“哪台坏了”。
                // 完整多行错误在详情页展示，这里只取首行。
                if let Some(error) = row.last_error() {
                    lines.push(fg(
                        ThemeColor::Error,
                        &format!("    ⚠ {}", first_line(error)),
                    ));
                }
            }
        }
        lines.push(fg(
            ThemeColor::Dim,
            "↑↓ choose · Enter details · e toggle · l sign in · o sign out · r refresh · Esc close",
        ));
        if let Some(notice) = &self.notice {
            lines.push(render_notice(notice));
        }
        lines
    }

    fn render_detail(&self, name: &str, selected: usize) -> Vec<String> {
        let Some(row) = self.row(name) else {
            return vec![fg(
                ThemeColor::Error,
                "This server is no longer configured.",
            )];
        };
        let mut lines = vec![bold(&format!("MCP server {}", row.name))];
        lines.push(label_value("transport", &row.transport));
        lines.push(label_value(
            "source",
            &format!("{} ({})", scope_label(row.scope), row.source),
        ));
        if let Some(override_path) = &row.override_path {
            lines.push(label_value("project override", override_path));
        }
        lines.push(label_value("state", state_text(row)));
        lines.push(label_value("exposure", row.exposure.as_str()));
        lines.push(label_value("tools", &row.tool_count().to_string()));
        if row.status.is_some() {
            lines.push(label_value("prompts", &row.prompt_count().to_string()));
            lines.push(label_value(
                "resources",
                if row.resources { "yes" } else { "no" },
            ));
        }
        if let Some(error) = row.last_error() {
            lines.push(fg(ThemeColor::Error, "recent error:"));
            for line in error.lines() {
                lines.push(fg(ThemeColor::Error, &format!("  {line}")));
            }
        }
        lines.push(String::new());
        lines.push(bold("Actions"));
        let actions = server_actions(row);
        let selected = selected.min(actions.len().saturating_sub(1));
        for (index, action) in actions.iter().enumerate() {
            let prefix = if index == selected { "→" } else { " " };
            let description = action_description(*action, row);
            lines.push(format!(
                " {prefix} {}  {}",
                action.label(),
                fg(ThemeColor::Dim, &description)
            ));
        }
        lines.push(fg(
            ThemeColor::Dim,
            "↑↓ choose · Enter select · e toggle · l sign in · o sign out · r refresh · Esc back",
        ));
        if let Some(notice) = &self.notice {
            lines.push(render_notice(notice));
        }
        lines
    }

    fn render_tools(&self, name: &str, selected: usize) -> Vec<String> {
        let Some(row) = self.row(name) else {
            return vec![fg(
                ThemeColor::Error,
                "This server is no longer configured.",
            )];
        };
        let count = row.tool_count();
        let mut lines = vec![bold(&format!("Tools of {} ({count})", row.name))];
        lines.push(label_value("default exposure", row.exposure.as_str()));
        if row.tools.is_empty() {
            lines.push(fg(
                ThemeColor::Dim,
                "No tool names reported for this server.",
            ));
        } else {
            let selected = selected.min(row.tools.len() - 1);
            for (index, tool) in row.tools.iter().enumerate() {
                let prefix = if index == selected { "→" } else { " " };
                let annotation = row
                    .tool_exposure
                    .get(tool.as_str())
                    .filter(|exposure| **exposure != row.exposure)
                    .map(|exposure| format!(" [{}]", exposure.as_str()))
                    .unwrap_or_default();
                lines.push(format!(" {prefix} {tool}{annotation}"));
            }
        }
        if count > row.tools.len() {
            lines.push(fg(
                ThemeColor::Dim,
                &format!("{} tool names unavailable.", count - row.tools.len()),
            ));
        }
        lines.push(fg(ThemeColor::Dim, "↑↓ choose · Esc back"));
        if let Some(notice) = &self.notice {
            lines.push(render_notice(notice));
        }
        lines
    }
}

impl Component for McpDialog {
    fn render(&mut self, width: usize) -> Vec<String> {
        let width = width.max(1);
        let body = match &self.view {
            McpDialogView::ServerList => self.render_list(),
            McpDialogView::ServerDetail { name, selected } => self.render_detail(name, *selected),
            McpDialogView::ToolList { name, selected } => self.render_tools(name, *selected),
        };
        let mut lines = vec![fg(ThemeColor::BorderMuted, &"─".repeat(width))];
        for line in body {
            lines.push(truncate_to_width_default(&line, width));
        }
        lines.push(fg(ThemeColor::BorderMuted, &"─".repeat(width)));
        lines
    }

    fn handle_input(&mut self, key_data: &str) {
        match self.view.clone() {
            McpDialogView::ServerList => self.handle_list_input(key_data),
            McpDialogView::ServerDetail { .. } => self.handle_detail_input(key_data),
            McpDialogView::ToolList { .. } => self.handle_tools_input(key_data),
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

impl Focusable for McpDialog {
    fn focused(&self) -> bool {
        self.focused
    }

    fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }
}

/// 用已有的配置 setter 写入一台服务器的 `enabled` 状态。
///
/// 目标是 `override_path ?? source`；条目本身是项目覆盖时传
/// `override_: true`（允许缺失条目作为覆盖加入），语义与扩展 `/mcp` 的保存动作一致。
/// 扩展注册的条目没有配置文件，返回错误而不是写坏扩展的 `source`。
///
/// 成功时文件里要么写入 `"enabled": false`，要么（重新启用普通条目时）删除该键——
/// 这是 [`update_mcp_server_config`] 的既有行为（缺省即启用）。
pub fn write_mcp_server_enabled(row: &McpServerRow, enabled: bool) -> Result<(), String> {
    if row.scope == McpServerScope::Extension {
        return Err(format!(
            "MCP server \"{}\" is registered by an extension; change it through that extension.",
            row.name
        ));
    }
    let target = row.override_path.as_deref().unwrap_or(row.source.as_str());
    update_mcp_server_config(
        Path::new(target),
        &row.name,
        &McpServerConfigPatch {
            enabled: Some(enabled),
            exposure: None,
        },
        UpdateMcpServerConfigOptions {
            override_: row.override_path.is_some(),
        },
    )
    .map_err(|error| format!("Could not update {target}: {error}"))
}

/// 主题前景色；主题未初始化时退化为无颜色（面板不 panic）。
fn fg(color: ThemeColor, text: &str) -> String {
    THEME.fg(color, text).unwrap_or_else(|_| text.to_string())
}

/// 主题粗体；主题未初始化时退化为原文本。
fn bold(text: &str) -> String {
    THEME.bold(text).unwrap_or_else(|_| text.to_string())
}

/// `label: value` 形式的详情行（label 灰色）。
fn label_value(label: &str, value: &str) -> String {
    format!("  {} {}", fg(ThemeColor::Dim, &format!("{label}:")), value)
}

/// 渲染一行服务器列表。
fn render_row(row: &McpServerRow, selected: bool, name_width: usize) -> String {
    let (marker, color) = state_marker(row);
    let prefix = if selected { "→ " } else { "  " };
    let padded = pad_name(&row.name, name_width);
    let name = if row.enabled {
        padded
    } else {
        fg(ThemeColor::Dim, &padded)
    };
    let description = format!(
        "{} · {} · {}",
        state_text(row),
        tools_text(row.tool_count()),
        scope_text(row)
    );
    format!(
        "{prefix}{} {name}  {}",
        fg(color, marker),
        fg(ThemeColor::Dim, &description)
    )
}

/// 名称列补空格或截断（名称是 ASCII，宽度与字节数一致，仍用 `visible_width` 保持通用）。
fn pad_name(name: &str, width: usize) -> String {
    let current = visible_width(name);
    if current >= width {
        truncate_to_width_default(name, width)
    } else {
        format!("{name}{}", " ".repeat(width - current))
    }
}

/// 错误文本的第一行（列表行内只展示一行，完整内容在详情页）。
fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or("").trim()
}

/// 渲染 notice。
fn render_notice(notice: &McpNotice) -> String {
    match notice.kind {
        McpNoticeKind::Info => fg(ThemeColor::Dim, &format!("ℹ {}", notice.message)),
        McpNoticeKind::Error => fg(ThemeColor::Error, &format!("✗ {}", notice.message)),
    }
}
