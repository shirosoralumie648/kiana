//! components/footer.ts。
//!
//! 底部三行信息：pwd/分支/会话名、用量统计 + 模型、扩展状态。
//!
//! TS 源：`packages/coding-agent/src/modes/interactive/components/footer.ts`。
//!
//! ## 职责与边界
//!
//! 这是一个只读渲染组件：`handles_input` 恒为 `false`，不接收键盘事件，也不修改会话。
//! 它把三类数据源拼成 2–3 行文本（pwd 行 + 统计行固定输出，扩展状态非空时追加第三行）：
//! - 会话（`session`）：当前模型、上下文用量、整条会话的 usage 条目；
//! - `ReadonlyFooterDataProvider`：git 分支、扩展状态、可用 provider 数；
//! - 进程环境：`HOME` / `USERPROFILE`（用于 `~` 缩写）。
//!
//! 调用方是 `interactive_mode`：本组件被包在 `Rc<RefCell<...>>` 中按帧调用
//! `render(width)`；切换会话时调用 `set_session` 替换数据源。
//!
//! ## 渲染流程
//!
//! `render` 每次调用都重新收集数据（本实现没有跨帧缓存）：
//! 1. 全量扫描 `session_manager.get_entries()` 汇总 usage（见 `get_session_stats`）；
//! 2. 组装第一行 pwd：`~` 缩写 → 追加 git 分支 → 追加会话名；
//! 3. 组装第二行：左侧为 ↑/↓/R/W/CH/费用/上下文百分比/xp，右侧为模型名（含思考等级、
//!    虚拟模型路由目标、provider 前缀），两侧各自截断后补空格对齐；
//! 4. 若扩展状态表非空，追加第三行（按键名排序、逐项清洗换行后以空格拼接）。
//!
//! ## 注意事项
//!
//! - 所有输出行的可见宽度都保证 `<= width`（测试 `keeps_all_lines_within_width_*` 等锁定）。
//!   左侧统计放不下时用 `...` 截断；右侧模型名可能先丢掉 `(provider)` 前缀，再被截断。
//! - 主题必须已初始化：`fg` / `bold` 在未初始化时 panic，对应 TS 在同样条件下抛异常。
//! - 返回的是带 ANSI 的字符串；同一行由多个着色片段拼接，测试通常先用
//!   `strip_ansi` 去色再断言。
//!
//! ## Public API for InteractiveMode
//!
//! - [`FooterComponent::new`]（`AgentSession`, `Arc<dyn ReadonlyFooterDataProvider>`）
//! - `set_session` / `set_auto_compact_enabled` / `invalidate` / `dispose`
//! - [`format_tokens`] / [`format_cwd_for_footer`]
//! - （`#[doc(hidden)]` 测试入口：`from_parts` + trait [`FooterSession`]）

use std::cmp::Ordering;
use std::path::{Component as PathComponent, Path, PathBuf};
use std::sync::Arc;

use kiana_agent_core::AgentMessage;
use kiana_ai::model_catalog::js_to_fixed;
use kiana_ai::types::{Message, ModelThinkingLevel};
use kiana_ext_api::types::ContextUsage;
use kiana_ext_api::ui::ReadonlyFooterDataProvider;
use kiana_permissions::PermissionMode;
use kiana_session::agent_session::{AgentSession, RoutedModel};
use kiana_session_core::session_manager::{ReadonlySessionManager, SessionEntry};
use kiana_session_core::usage_totals::{UsageTotals, add_usage_to_totals, create_usage_totals};
use kiana_theme::theme_tokens::ThemeColor;
use kiana_tui::autocomplete::locale_compare;
use kiana_tui::text::{truncate_to_width, visible_width};
use kiana_tui::tui::Component;

use crate::components::approval_mode_indicator::permission_mode_visual;

/// 原生路径分隔符（`path.sep`）。
const SEP: char = std::path::MAIN_SEPARATOR;

fn fg(color: ThemeColor, text: &str) -> String {
    kiana_theme::THEME
        .fg(color, text)
        .expect("theme must be initialized before rendering (TS throws here)")
}

/// 生产环境的会话视图：`AgentSession` 的最小只读面。
///
/// 存在的理由：TS 测试用对象字面量伪造 `AgentSession`；Rust 的 `AgentSession` 是具体结构体，
/// 无法伪造。公开构造器仍只接受 `AgentSession`（[`FooterComponent::new`]），测试用
/// [`FooterComponent::from_parts`]（`#[doc(hidden)]`）注入替身。
///
/// 所有方法都是只读查询，不改变会话状态；`render` 每帧按需调用其中若干方法。
/// 模型相关的返回值是按字段复制出的快照（`FooterModel` / `FooterRoutedModel`），
/// `Option::None` 表示该数据当前不可用；`session_manager()` 则返回共享的只读句柄。
#[doc(hidden)]
pub trait FooterSession {
    /// `session.model`（`state.model`）。
    fn model(&self) -> Option<FooterModel>;
    /// `session.thinkingLevel`。
    fn thinking_level(&self) -> ModelThinkingLevel;
    /// `session.sessionManager`。
    fn session_manager(&self) -> Arc<dyn ReadonlySessionManager>;
    /// `session.getContextUsage()`。
    fn get_context_usage(&self) -> Option<ContextUsage>;
    /// `session.routedModel`。
    fn routed_model(&self) -> Option<FooterRoutedModel>;
    /// `session.modelRuntime.isUsingSubscription(provider)`。
    fn is_using_subscription(&self, provider: &str) -> bool;
}

/// footer 需要的模型字段（`state.model` 的子集）。
#[doc(hidden)]
#[derive(Debug, Clone, PartialEq)]
pub struct FooterModel {
    /// `model.id`。
    pub id: String,
    /// `model.provider`。
    pub provider: String,
    /// `model.contextWindow`。
    pub context_window: Option<u64>,
    /// `model.reasoning`（`undefined` 视为 false）。
    pub reasoning: bool,
}

/// footer 需要的 routed model 字段。
#[doc(hidden)]
#[derive(Debug, Clone, PartialEq)]
pub struct FooterRoutedModel {
    /// `routed.model.id`。
    pub model_id: String,
    /// `routed.thinkingLevel`。
    pub thinking_level: Option<ModelThinkingLevel>,
}

impl FooterSession for AgentSession {
    fn model(&self) -> Option<FooterModel> {
        AgentSession::model(self).map(|model| FooterModel {
            id: model.id,
            provider: model.provider,
            context_window: model.context_window,
            reasoning: model.reasoning.unwrap_or(false),
        })
    }

    fn thinking_level(&self) -> ModelThinkingLevel {
        AgentSession::thinking_level(self)
    }

    fn session_manager(&self) -> Arc<dyn ReadonlySessionManager> {
        AgentSession::session_manager(self)
    }

    fn get_context_usage(&self) -> Option<ContextUsage> {
        AgentSession::get_context_usage(self)
    }

    fn routed_model(&self) -> Option<FooterRoutedModel> {
        AgentSession::routed_model(self).map(
            |RoutedModel {
                 model,
                 thinking_level,
             }| {
                FooterRoutedModel {
                    model_id: model.id,
                    thinking_level,
                }
            },
        )
    }

    fn is_using_subscription(&self, provider: &str) -> bool {
        self.model_runtime().is_using_subscription(provider)
    }
}

/// `sanitizeStatusText(text)`：换行/制表符替换成空格，折叠连续空格，再按 JS 规则 trim。
fn sanitize_status_text(text: &str) -> String {
    let replaced: String = text
        .chars()
        .map(|ch| match ch {
            '\r' | '\n' | '\t' => ' ',
            other => other,
        })
        .collect();
    let mut collapsed = String::with_capacity(replaced.len());
    let mut last_was_space = false;
    for ch in replaced.chars() {
        if ch == ' ' {
            if last_was_space {
                continue;
            }
            last_was_space = true;
        } else {
            last_was_space = false;
        }
        collapsed.push(ch);
    }
    kiana_json::js_string::js_trim(&collapsed).to_string()
}

/// `formatTokens(count)`：紧凑 token 数（k/M 分档）。
///
/// 分档边界与 TS 逐字对齐（`tests/fixtures/render_goldens_dark.json` 的 `format_tokens`
/// 锁定）：
/// - `< 1000`：十进制原样，如 `999`；
/// - `< 10_000`：一位小数的 k，如 `1.0k`；
/// - `< 1_000_000`：整数 k，如 `10k`；注意 `999_999` 会得到 `1000k`；
/// - `< 10_000_000`：一位小数的 M，如 `1.0M`；
/// - 其余：整数 M，如 `10M`。
///
/// 取整使用 JS `Math.round` 语义（半数向 +∞），保留一位小数使用 `toFixed(1)` 的
/// 实现 `js_to_fixed`；这是当前稳定契约，改动需同步测试（goldens 锁定）。
pub fn format_tokens(count: u64) -> String {
    if count < 1000 {
        return count.to_string();
    }
    if count < 10_000 {
        return format!("{}k", js_to_fixed(count as f64 / 1000.0, 1));
    }
    if count < 1_000_000 {
        return format!("{}k", js_round(count as f64 / 1000.0));
    }
    if count < 10_000_000 {
        return format!("{}M", js_to_fixed(count as f64 / 1_000_000.0, 1));
    }
    format!("{}M", js_round(count as f64 / 1_000_000.0))
}

/// `Math.round`：半数向 +∞（JS 语义；正数等价于 `floor(x + 0.5)`）。
fn js_round(value: f64) -> String {
    if !value.is_finite() {
        return if value.is_nan() {
            "NaN".to_string()
        } else if value < 0.0 {
            "-Infinity".to_string()
        } else {
            "Infinity".to_string()
        };
    }
    let rounded = (value + 0.5).floor();
    kiana_json::js_num::format_js_number(rounded)
}

/// 词法归一化路径（`path.resolve` 的纯字符串部分；相对路径基于 `std::env::current_dir`）。
fn resolve_lexically(path: &str) -> PathBuf {
    let path = Path::new(path);
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("/"))
            .join(path)
    };
    let mut out = PathBuf::new();
    for component in absolute.components() {
        match component {
            PathComponent::Prefix(prefix) => out.push(prefix.as_os_str()),
            PathComponent::RootDir => out.push(PathComponent::RootDir.as_os_str()),
            PathComponent::CurDir => {}
            PathComponent::ParentDir => {
                // Node 的 resolve 在根处截断 `..`。
                out.pop();
            }
            PathComponent::Normal(part) => out.push(part),
        }
    }
    out
}

/// Node `path.relative(from, to)`（同一平台）的逐字语义。
fn relative_path(from: &Path, to: &Path) -> String {
    let from_components: Vec<PathComponent<'_>> = from.components().collect();
    let to_components: Vec<PathComponent<'_>> = to.components().collect();
    // Node 在根/盘符不同时直接返回目标绝对路径（Windows 跨盘）。
    let from_root = from_components.first();
    let to_root = to_components.first();
    let same_root = match (from_root, to_root) {
        (Some(PathComponent::RootDir), Some(PathComponent::RootDir)) => true,
        (Some(PathComponent::Prefix(a)), Some(PathComponent::Prefix(b))) => a == b,
        (None, None) => true,
        _ => false,
    };
    if !same_root {
        return to.to_string_lossy().into_owned();
    }
    let mut common = 0usize;
    while common < from_components.len()
        && common < to_components.len()
        && from_components[common] == to_components[common]
    {
        common += 1;
    }
    let mut parts: Vec<String> = Vec::new();
    for _ in common..from_components.len() {
        parts.push("..".to_string());
    }
    for component in &to_components[common..] {
        match component {
            PathComponent::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
            PathComponent::CurDir => {}
            PathComponent::ParentDir => parts.push("..".to_string()),
            PathComponent::RootDir => {}
            PathComponent::Prefix(prefix) => {
                parts.push(prefix.as_os_str().to_string_lossy().into_owned())
            }
        }
    }
    parts.join(&SEP.to_string())
}

/// `formatCwdForFooter(cwd, home)`：只对 home 的**精确前缀**做 `~` 缩写。
///
/// 先对两边做词法规范化（相对路径基于 `std::env::current_dir`；`.` 丢弃，`..` 回退一级，
/// 到根后再 `..` 被截断），再计算 `cwd` 相对 home 的路径：
/// - `cwd == home` → `"~"`；
/// - `cwd` 在 home 之下 → `"~/子路径"`；
/// - 其他（包括共享字符串前缀但路径分量不同的 `/home/user2` vs `/home/user`）→ 返回
///   调用方传入的 `cwd` 原值，不做任何改写。
///
/// `home` 为 `None` 或空串时同样原样返回。`current_dir()` 失败时按 `/` 作为基准目录。
/// 这是纯路径字符串运算，不访问文件系统，因此不要求目录真实存在。
pub fn format_cwd_for_footer(cwd: &str, home: Option<&str>) -> String {
    let Some(home) = home.filter(|home| !home.is_empty()) else {
        return cwd.to_string();
    };
    let resolved_cwd = resolve_lexically(cwd);
    let resolved_home = resolve_lexically(home);
    let relative_to_home = relative_path(&resolved_home, &resolved_cwd);
    let is_inside_home = relative_to_home.is_empty()
        || (relative_to_home != ".."
            && !relative_to_home.starts_with(&format!("..{SEP}"))
            && !Path::new(&relative_to_home).is_absolute());
    if !is_inside_home {
        return cwd.to_string();
    }
    if relative_to_home.is_empty() {
        "~".to_string()
    } else {
        format!("~{SEP}{relative_to_home}")
    }
}

/// `process.env.HOME || process.env.USERPROFILE`。
fn home_dir_from_env() -> Option<String> {
    std::env::var("HOME")
        .ok()
        .filter(|value| !value.is_empty())
        .or_else(|| {
            std::env::var("USERPROFILE")
                .ok()
                .filter(|value| !value.is_empty())
        })
}

/// `ModelThinkingLevel` 的 TS 字面量。
fn thinking_level_name(level: ModelThinkingLevel) -> &'static str {
    match level {
        ModelThinkingLevel::Off => "off",
        ModelThinkingLevel::Minimal => "minimal",
        ModelThinkingLevel::Low => "low",
        ModelThinkingLevel::Medium => "medium",
        ModelThinkingLevel::High => "high",
        ModelThinkingLevel::Xhigh => "xhigh",
        ModelThinkingLevel::Max => "max",
    }
}

/// `class FooterComponent implements Component`。
///
/// 输出行数固定为 2 行，或在扩展状态表非空时 3 行；内容全部在 `render` 时按需生成，
/// 结构体本身只保存数据源与两个 UI 开关：
/// - `session`：会话数据源，生产环境用 [`FooterComponent::new`] 包装 `AgentSession`；
/// - `footer_data`：git 分支、扩展状态、可用 provider 数；
/// - `auto_compact_enabled`：是否在上下文百分比后追加 `(auto)`，默认 `true`；
/// - `permission_mode`：权限模式指示器（`None` 时不输出任何字符，保证 footer golden
///   逐字节不变）。模式切换入口在 `/permissions` 面板，footer 只负责持续展示。
///
/// 本组件不持有 TUI 焦点，也不处理输入（`handles_input` 返回 `false`）。
pub struct FooterComponent {
    auto_compact_enabled: bool,
    session: Box<dyn FooterSession>,
    footer_data: Arc<dyn ReadonlyFooterDataProvider>,
    permission_mode: Option<PermissionMode>,
}

impl FooterComponent {
    /// `constructor(session, footerData)`。
    ///
    /// 把具体的 `AgentSession` 装箱为 `FooterSession`，`auto_compact_enabled` 初始化为
    /// `true`，并在装箱前读取一次当前权限模式（CLI `--permission-mode` 在会话创建前
    /// 生效，因此这里拿到的是最终值）。构造本身不渲染，首次 `render` 才会访问其余
    /// 数据源。
    pub fn new(session: AgentSession, footer_data: Arc<dyn ReadonlyFooterDataProvider>) -> Self {
        let permission_mode = session.permission_mode();
        FooterComponent {
            auto_compact_enabled: true,
            session: Box::new(session),
            footer_data,
            permission_mode,
        }
    }

    /// 测试/嵌入用的替身构造（公开构造器的签名保持不变）。
    ///
    /// 与 [`FooterComponent::new`] 的唯一差别是直接接收 `Box<dyn FooterSession>`，
    /// 因此测试可以在 crate 外实现 trait 注入伪会话；`auto_compact_enabled` 同样默认为
    /// `true`。这是增量公开面（TS 无对应构造器），调用方不应依赖它做生产接线。
    #[doc(hidden)]
    pub fn from_parts(
        session: Box<dyn FooterSession>,
        footer_data: Arc<dyn ReadonlyFooterDataProvider>,
    ) -> Self {
        FooterComponent {
            auto_compact_enabled: true,
            session,
            footer_data,
            permission_mode: None,
        }
    }

    /// `setSession(session)`。
    ///
    /// 会话 rebind 时同步重读权限模式，避免 footer 继续显示旧会话的模式。
    pub fn set_session(&mut self, session: AgentSession) {
        self.permission_mode = session.permission_mode();
        self.session = Box::new(session);
    }

    /// 更新权限模式指示器（`None` 表示权限层关闭，footer 不追加任何字符）。
    ///
    /// 由 `/permissions` 面板的 `ModeSelect` 应用后调用；`AgentSession` 的 rebind 路径
    /// 通过 [`Self::set_session`] 自动刷新。
    pub fn set_permission_mode(&mut self, mode: Option<PermissionMode>) {
        self.permission_mode = mode;
    }

    /// 当前权限模式（测试接缝）。
    pub fn permission_mode(&self) -> Option<PermissionMode> {
        self.permission_mode
    }

    /// `setAutoCompactEnabled(enabled)`。
    pub fn set_auto_compact_enabled(&mut self, enabled: bool) {
        self.auto_compact_enabled = enabled;
    }

    /// 当前 auto-compact 标记。
    pub fn auto_compact_enabled(&self) -> bool {
        self.auto_compact_enabled
    }

    /// `invalidate()`：no-op（git 分支缓存由 provider 负责）。
    pub fn invalidate(&mut self) {}

    /// `dispose()`：no-op。
    pub fn dispose(&mut self) {}
}

/// `getSessionStats()` 的结果。
struct SessionStats {
    usage_totals: UsageTotals,
    latest_cache_hit_rate: Option<f64>,
}

impl FooterComponent {
    /// 汇总整条会话的用量。
    ///
    /// TS 用 `sessionManager.getEntryCount()` 作为缓存键；`ReadonlySessionManager` 没有该
    /// 方法（冻结契约），因此每帧重算（输出相同，纯性能差异）。
    ///
    /// 计入的条目（其余一律忽略）：
    /// - `SessionEntry::Usage`：直接累加；
    /// - `SessionEntry::Message`：只处理 `Assistant`（累加 `usage`）与 `ToolResult`
    ///   （`usage` 为 `Some` 时累加）；
    /// - `SessionEntry::BranchSummary` / `SessionEntry::Compaction`：`usage` 为 `Some`
    ///   时累加。
    ///
    /// 缓存命中率取 `get_entries()` 顺序中**最后一条** assistant 消息：分母为
    /// `input + cache_read + cache_write`（不含 output），分子为 `cache_read`，结果乘 100。
    /// 分母为 0 时置 `None`，因此此前的命中率会被覆盖掉。
    fn get_session_stats(&self) -> SessionStats {
        let session_manager = self.session.session_manager();
        let mut usage_totals = create_usage_totals();
        let mut latest_cache_hit_rate: Option<f64> = None;

        for entry in session_manager.get_entries() {
            match &entry {
                SessionEntry::Usage(entry) => {
                    add_usage_to_totals(&mut usage_totals, &entry.usage);
                }
                SessionEntry::Message(entry) => match &entry.message {
                    AgentMessage::Standard(Message::Assistant(message)) => {
                        add_usage_to_totals(&mut usage_totals, &message.usage);
                        let latest_prompt_tokens = message.usage.input
                            + message.usage.cache_read
                            + message.usage.cache_write;
                        latest_cache_hit_rate = if latest_prompt_tokens > 0 {
                            Some(
                                message.usage.cache_read as f64 / latest_prompt_tokens as f64
                                    * 100.0,
                            )
                        } else {
                            None
                        };
                    }
                    AgentMessage::Standard(Message::ToolResult(message)) => {
                        if let Some(usage) = &message.usage {
                            add_usage_to_totals(&mut usage_totals, usage);
                        }
                    }
                    _ => {}
                },
                SessionEntry::BranchSummary(entry) => {
                    if let Some(usage) = &entry.usage {
                        add_usage_to_totals(&mut usage_totals, usage);
                    }
                }
                SessionEntry::Compaction(entry) => {
                    if let Some(usage) = &entry.usage {
                        add_usage_to_totals(&mut usage_totals, usage);
                    }
                }
                _ => {}
            }
        }

        SessionStats {
            usage_totals,
            latest_cache_hit_rate,
        }
    }
}

impl Component for FooterComponent {
    fn render(&mut self, width: usize) -> Vec<String> {
        // 阶段 1：一次性取齐本帧的数据快照（模型、整条会话的 usage、上下文占用）。
        let model = self.session.model();
        let SessionStats {
            usage_totals,
            latest_cache_hit_rate,
        } = self.get_session_stats();
        let context_usage = self.session.get_context_usage();
        // 上下文窗口优先用 ContextUsage 自带的窗口，其次用模型声明，都没有时按 0 处理。
        let context_window = context_usage
            .as_ref()
            .map(|usage| usage.context_window)
            .or_else(|| model.as_ref().and_then(|model| model.context_window))
            .unwrap_or(0);
        let context_percent_value = context_usage
            .as_ref()
            .and_then(|usage| usage.percent)
            .unwrap_or(0.0);
        // TS：`contextUsage?.percent !== null` —— 只有显式 `null` 才是 `"?"`。
        // 整个 `ContextUsage` 缺失时这里走 `0.0`，不是 `"?"`（测试
        // `no_context_usage_renders_zero_percent_with_the_model_window` 锁定）。
        let context_percent = if matches!(&context_usage, Some(usage) if usage.percent.is_none()) {
            "?".to_string()
        } else {
            js_to_fixed(context_percent_value, 1)
        };

        // 阶段 2：第一行 pwd。后缀只在值非空时追加，顺序固定为分支再会话名。
        let session_manager = self.session.session_manager();
        let mut pwd =
            format_cwd_for_footer(session_manager.get_cwd(), home_dir_from_env().as_deref());
        if let Some(branch) = self.footer_data.get_git_branch()
            && !branch.is_empty()
        {
            pwd = format!("{pwd} ({branch})");
        }
        if let Some(session_name) = session_manager.get_session_name()
            && !session_name.is_empty()
        {
            pwd = format!("{pwd} • {session_name}");
        }

        // 阶段 3：第二行左半部分。token 类统计项（↑/↓/R/W/CH）只在非零时出现，顺序固定；
        // 两个例外：费用项在订阅计费（using_subscription）时即使 cost 为 0 也会出现
        // （显示 `$0.000 (sub)`）；实验特性开启时末尾还会追加 xp 项。
        // 因此「空会话只剩上下文百分比一项」仅在非订阅、未开实验特性时成立。
        let mut stats_parts: Vec<String> = Vec::new();
        if usage_totals.input != 0 {
            stats_parts.push(format!("↑{}", format_tokens(usage_totals.input)));
        }
        if usage_totals.output != 0 {
            stats_parts.push(format!("↓{}", format_tokens(usage_totals.output)));
        }
        if usage_totals.cache_read != 0 {
            stats_parts.push(format!("R{}", format_tokens(usage_totals.cache_read)));
        }
        if usage_totals.cache_write != 0 {
            stats_parts.push(format!("W{}", format_tokens(usage_totals.cache_write)));
        }
        if (usage_totals.cache_read > 0 || usage_totals.cache_write > 0)
            && let Some(rate) = latest_cache_hit_rate
        {
            stats_parts.push(format!("CH{}%", js_to_fixed(rate, 1)));
        }

        // Kimi Coding 虽是 API-key 认证，但按订阅计费。
        // 其他 provider 由 session 判定；任一路径成立都显示 `(sub)`。
        let using_subscription = match &model {
            Some(model) => {
                model.provider == "kimi-coding"
                    || self.session.is_using_subscription(&model.provider)
            }
            None => false,
        };
        if (usage_totals.cost != 0.0 && !usage_totals.cost.is_nan()) || using_subscription {
            let cost_str = format!(
                "${}{}",
                js_to_fixed(usage_totals.cost, 3),
                if using_subscription { " (sub)" } else { "" }
            );
            stats_parts.push(cost_str);
        }

        let auto_indicator = if self.auto_compact_enabled {
            " (auto)"
        } else {
            ""
        };
        let context_percent_display = if context_percent == "?" {
            format!("?/{}{auto_indicator}", format_tokens(context_window))
        } else {
            format!(
                "{context_percent}%/{}{auto_indicator}",
                format_tokens(context_window)
            )
        };
        // 阈值是严格大于：90% 恰好不告警、70% 恰好不变黄。
        let context_percent_str = if context_percent_value > 90.0 {
            fg(ThemeColor::Error, &context_percent_display)
        } else if context_percent_value > 70.0 {
            fg(ThemeColor::Warning, &context_percent_display)
        } else {
            context_percent_display
        };
        stats_parts.push(context_percent_str);
        if kiana_config::defaults::are_experimental_features_enabled() {
            stats_parts.push(format!(
                "{} {}",
                fg(ThemeColor::Dim, "•"),
                bold(&fg(ThemeColor::Warning, "xp"))
            ));
        }

        // 左栏先按完整内容测量；超过整行宽度时先截断自己，避免把右栏挤没。
        let mut stats_left = stats_parts.join(" ");
        let mut stats_left_width = visible_width(&stats_left);
        if stats_left_width > width {
            stats_left = truncate_to_width(&stats_left, width, "...", false);
            stats_left_width = visible_width(&stats_left);
        }

        // 阶段 4：第二行右半部分。模型名缺失或为空时显示 `no-model`。
        const MIN_PADDING: usize = 2;
        let model_name = model
            .as_ref()
            .map(|model| model.id.clone())
            .filter(|id| !id.is_empty())
            .unwrap_or_else(|| "no-model".to_string());
        let mut right_side_without_provider = model_name;
        // 只有模型声明支持 reasoning 才展示思考等级；`off` 带 `thinking` 前缀以示区分。
        if let Some(model) = &model
            && model.reasoning
        {
            let level = self.session.thinking_level();
            let level = thinking_level_name(level);
            right_side_without_provider = if level == "off" {
                format!("{right_side_without_provider} • thinking off")
            } else {
                format!("{right_side_without_provider} • {level}")
            };
        }
        // 虚拟模型（例如 `auto`）被路由到物理模型时，追加 `→ 物理模型 [• 等级]`。
        if let Some(routed) = self.session.routed_model() {
            let level = routed
                .thinking_level
                .map(|level| format!(" • {}", thinking_level_name(level)))
                .unwrap_or_default();
            right_side_without_provider =
                format!("{right_side_without_provider} → {}{level}", routed.model_id);
        }
        // 权限模式段放在最右端：`None` 时不追加任何字符，既有 golden 逐字节不变。
        // 颜色段最后应用；它之后没有其它内容，内层 reset 不会破坏布局。
        if let Some(mode) = self.permission_mode {
            let visual = permission_mode_visual(mode);
            let mode_text = if visual.bold {
                bold(visual.text)
            } else {
                visual.text.to_string()
            };
            right_side_without_provider = format!(
                "{right_side_without_provider} • {}",
                fg(visual.color, &mode_text)
            );
        }

        // 有多个可用 provider 时才加 `(provider)` 前缀；若因此放不下，退回无前缀版本。
        let mut right_side = right_side_without_provider.clone();
        if self.footer_data.get_available_provider_count() > 1
            && let Some(model) = &model
        {
            right_side = format!("({}) {right_side_without_provider}", model.provider);
            if stats_left_width + MIN_PADDING + visible_width(&right_side) > width {
                right_side = right_side_without_provider;
            }
        }

        // 整行能容纳时用空格把右栏推到行尾；否则右栏截断，且优先让左栏完整可读。
        let right_side_width = visible_width(&right_side);
        let total_needed = stats_left_width + MIN_PADDING + right_side_width;
        // `statsLine.slice(statsLeft.length)`：statsLine 总是以 statsLeft 原样开头，
        // 因此 remainder 就是 padding + rightSide（或空串）。
        let remainder = if total_needed <= width {
            let padding = " ".repeat(width - stats_left_width - right_side_width);
            format!("{padding}{right_side}")
        } else {
            let available_for_right = width.saturating_sub(stats_left_width + MIN_PADDING);
            if available_for_right > 0 {
                let truncated_right =
                    truncate_to_width(&right_side, available_for_right, "", false);
                let truncated_right_width = visible_width(&truncated_right);
                let padding = " ".repeat(
                    width
                        .saturating_sub(stats_left_width)
                        .saturating_sub(truncated_right_width),
                );
                format!("{padding}{truncated_right}")
            } else {
                String::new()
            }
        };

        // statsLeft 与 remainder 分别套 dim（上下文百分比段的 reset 会清掉外层 dim）。
        let dim_stats_left = fg(ThemeColor::Dim, &stats_left);
        let dim_remainder = fg(ThemeColor::Dim, &remainder);
        let pwd_line = truncate_to_width(
            &fg(ThemeColor::Dim, &pwd),
            width,
            &fg(ThemeColor::Dim, "..."),
            false,
        );
        let mut lines = vec![pwd_line, format!("{dim_stats_left}{dim_remainder}")];

        // 阶段 5：第三行（可选）。排序按 key 的 locale 规则，而不是 map 的插入顺序；
        // 每个状态文本先折叠空白/换行再拼接，避免扩展输出破坏单行布局。
        let extension_statuses = self.footer_data.get_extension_statuses();
        if !extension_statuses.is_empty() {
            let mut entries: Vec<(&String, &String)> = extension_statuses.iter().collect();
            entries.sort_by(|(a, _), (b, _)| locale_compare(a, b));
            let status_line = entries
                .into_iter()
                .map(|(_, text)| sanitize_status_text(text))
                .collect::<Vec<String>>()
                .join(" ");
            lines.push(truncate_to_width(
                &status_line,
                width,
                &fg(ThemeColor::Dim, "..."),
                false,
            ));
        }

        lines
    }

    fn handles_input(&self) -> bool {
        false
    }

    fn invalidate(&mut self) {
        // no-op：git 分支缓存由 provider 负责。
    }
}

fn bold(text: &str) -> String {
    kiana_theme::THEME
        .bold(text)
        .expect("theme must be initialized before rendering (TS throws here)")
}

/// 扩展状态排序的比较器（供测试复现 `localeCompare`）。
///
/// 生产渲染路径在 `render` 内直接调用 `locale_compare`；本函数只是把同一比较规则以公开
/// 函数形式提供，便于 crate 外复现「状态行顺序」。collator 不可用时会退化为字节序。
pub fn compare_status_keys(a: &str, b: &str) -> Ordering {
    locale_compare(a, b)
}
