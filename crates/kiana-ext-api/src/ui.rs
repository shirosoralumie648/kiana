//! ExtensionUIContext 与对话框/组件工厂选项。
//!
//! 另含 `ReadonlyFooterDataProvider` trait：TS 的 `types.ts` 从 `footer-data-provider.ts`
//! （`kiana-resources`，L10）导入该只读接口，而本 crate 是 L7，无法反向依赖。接口上移到这里，
//! `kiana-resources::FooterDataProvider` 实现它（`ARCHITECTURE.md` §3.5）。
//!
//! # 同步性
//!
//! [`ExtensionUiContext`] 是 `Send + Sync`（`ARCHITECTURE.md` §5.2 冻结）。它的异步方法返回
//! 非 `Send` 的 [`LocalBoxFuture`]：实现跑在 TUI 单线程上，返回值里可能含
//! `!Send` 的 [`Component`]。传入的每个回调都带 `+ Send + Sync`，
//! 因为实现（`kiana-interactive`/`kiana-rpc-print`）存储在 `Send + Sync` 的 trait 对象里。
//!
//! # 这个文件负责什么
//!
//! 扩展代码不能直接操作 TUI 组件，只能通过宿主提供的 `ctx.ui`。本文件定义这个入口的
//! **全部契约类型**：对话框/组件选项结构体、回调类型别名，以及 [`ExtensionUiContext`] 与
//! [`ReadonlyFooterDataProvider`] 两个 trait。这里没有 UI 行为实现、I/O 或运行时状态；
//! 仅有 `WidgetPlacement::as_str` 与四个固定调试输出的手写 `Debug` 实现。真正行为在
//! `kiana-extensions`（no-op UI、`UiPromptContext` 包装与 JS 桥）、`kiana-interactive`（TUI）、
//! `kiana-rpc-print`（RPC）与 `kiana-tools`（无 UI 占位实现）中。
//!
//! # 文件结构
//!
//! 1. **选项结构体**（`ExtensionUIDialogOptions`、`ExtensionWidgetOptions`、
//!    `WorkingIndicatorOptions`、`CustomOptions` 等）：TS 内联对象/接口的 Rust 化，
//!    TS 的可选字段一律用 `Option` 表达（`None` 对应 `undefined`）。
//! 2. **回调类型别名**（`TerminalInputHandler`、`EditorFactory`、`CustomFactory` 等）：
//!    统一为 `Arc<dyn Fn ... + Send + Sync>`。回调由 `Send + Sync` 的 UI 上下文实现持有，
//!    因此不能捕获 `Rc`/`RefCell` 等 `!Send` 状态。
//! 3. **两个 trait**：只含声明。`index.rs` 的 barrel 不包含 `ReadonlyFooterDataProvider`
//!    （TS 的 `types.ts` 只 import 不 export）与 Rust-only 的 `WidgetContent` 等，
//!    这些类型需经 `kiana_ext_api::ui::` 路径引用。
//!
//! # 一次调用经过哪里
//!
//! 交互式实现（`kiana-interactive` 的 `InteractiveUiContext`）把大多数同步 setter 变成
//! `UiMessage::Request` 发给 TUI 线程后立即返回；异步对话框（select/confirm/input/editor/
//! custom）额外登记一个 oneshot responder，在用户应答时解析（带 opts 的
//! select/confirm/input 还响应 `signal` 取消与 `timeout` 到期），通道断开时退回默认值。
//! RPC 实现把 select/confirm/input/editor 改写成 JSON 请求并等待对端响应；notify 与部分
//! setter 只写出记录，custom、主题查询和其余 setter 直接返回默认值或 no-op。print/json 模式安装 `kiana-extensions`
//! 的进程级 no-op 上下文，其返回值就是各方法文档中的“无 UI 时”结果。
//!
//! # 失败模式与注意事项
//!
//! - 本层方法**不返回 `Result`**：失败用 `None`/`false`/空集合表达；唯一带错误文本的是
//!   [`ExtensionUiContext::set_theme`]（返回 [`ThemeResult`]）。
//! - 异步方法返回 `LocalBoxFuture`（非 `Send`），必须在单线程 `LocalSet` 上 poll；
//!   不要把它交给 `tokio::spawn`（编译期即要求 `Send`）。
//! - [`ExtensionUiContext::theme`] 返回借用 `&Theme` 而不是 `Arc<Theme>`：实现必须保证该
//!   引用在借用期内有效。三个生产实现都把用到的 `Arc` 存入进程级历史表。
//! - [`ReadonlyFooterDataProvider::get_extension_statuses`] 返回实时 map 的引用（不是快照），
//!   因此实现需要内部可变性；`kiana-resources` 用 `UnsafeCell`，写操作限定在 UI 线程。
//! - 这些类型被 `kiana-tools`/`kiana-extensions`/`kiana-interactive`/`kiana-rpc-print`/
//!   `kiana-session` 等直接依赖，而本 crate 是扩展公开 API 的唯一权威定义位置（crate 根
//!   文档）；改动签名会同时波及 Rust 实现与 JS 桥。

use std::any::Any;
use std::sync::Arc;

use indexmap::IndexMap;
use kiana_config::keybindings::KeybindingsManager;
use kiana_core::event_bus::Unsubscribe;
use kiana_theme::theme::Theme;
use kiana_tui::autocomplete::AutocompleteProvider;
use kiana_tui::components::editor::EditorTheme;
use kiana_tui::editor_component::EditorComponent;
use kiana_tui::tui::{Component, OverlayHandle, OverlayOptions, Tui};
use tokio_util::sync::CancellationToken;

use crate::LocalBoxFuture;

/// 扩展 UI 对话框的选项。
///
/// TS: `types.ts#ExtensionUIDialogOptions`。`signal` 可程序化关闭对话框；`timeout` 是**毫秒**，
/// 对话框自动关闭并显示倒计时（交互式），或由 agent 侧自动解析（RPC）。
/// TS 类型是 `number`，用 `f64` 保留 JS 数值语义（任意整数/小数）。
///
/// 实现语义：结果由“用户应答 / `signal` 取消 / `timeout` 到期”三者中最先发生者决定，
/// 取消与超时都返回该方法的默认值（`select`/`input` 为 `None`，`confirm` 为 `false`）。
/// 调用前 `signal` 已取消时，`kiana-extensions` 的 `UiPromptContext` 与 RPC 的 `dialog`
/// 会跳过对话框直接返回默认值。RPC 实现的本地等待把 `0`/`NaN` 的 timeout 视为未设置
/// （`dialog` 里的过滤，与 TS 的 `if (opts?.timeout)` 一致）；交互式实现只为
/// `timeout > 0` 显示倒计时，并用 abort watcher 响应 `signal`。
#[derive(Debug, Clone, Default)]
pub struct ExtensionUIDialogOptions {
    /// 程序化关闭对话框（TS `AbortSignal`）。
    pub signal: Option<CancellationToken>,
    /// 超时（毫秒）。
    pub timeout: Option<f64>,
}

/// per-tool 审批卡的作用域（第 12 周）。
///
/// 与 `kiana-permissions::ApprovalScope` 一一对应，但本 crate 不依赖权限层：会话层
/// 负责把选择映射回 `ApprovalDecision` 并写回规则。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PermissionScopeKind {
    /// 仅本会话。
    Session,
    /// 本机（local settings）。
    Local,
    /// 本项目（project settings）。
    Project,
}

/// 一次 per-tool 审批的请求。
///
/// `args` 是完整工具参数（可能含 secret），只交给 UI 渲染卡片；不要写进
/// `AgentSessionEvent` 或日志。`decision_reason` 必须与权限引擎的 `Ask.reason` 一致。
#[derive(Clone, Debug)]
pub struct PermissionCardRequest {
    /// 工具名。
    pub tool_name: String,
    /// 完整工具参数（卡片据此渲染命令/diff/域名）。
    pub args: serde_json::Value,
    /// 会话工作目录。
    pub cwd: String,
    /// 权限引擎给出的决策原因（原样展示）。
    pub decision_reason: String,
    /// 允许写回的作用域；空向量表示只允许一次性批准。
    pub always_scopes: Vec<PermissionScopeKind>,
    /// 引擎判定的危险动作（危险动作默认聚焦 Reject）。
    pub dangerous: bool,
}

impl PermissionCardRequest {
    /// 审批卡标题：工具名 + 实际目标（命令/路径/URL/参数）+ 工作目录 + 决策原因。
    ///
    /// 这是默认 [`ExtensionUiContext::permission_card`] 的展示契约：用户必须在批准前
    /// 看到真正要执行的命令或写入的路径，而不是只看一句“default mode asks”。`args`
    /// 可能包含 secret，只用于 UI 展示；事件流仍使用会话层脱敏后的摘要。
    pub fn card_title(&self) -> String {
        let mut title = format!("Permission required\nTool: {}", self.tool_name);
        let mut target = None;
        if let Some(command) = display_arg(&self.args, &["command"]) {
            title.push_str("\nCommand: ");
            title.push_str(&truncate_display(command, 400));
            target = Some(());
        } else if let Some(path) = display_arg(&self.args, &["path", "file_path", "filePath"]) {
            title.push_str("\nPath: ");
            title.push_str(&truncate_display(path, 400));
            target = Some(());
        } else if let Some(url) = display_arg(&self.args, &["url"]) {
            title.push_str("\nURL: ");
            title.push_str(&truncate_display(url, 400));
            target = Some(());
        }
        if target.is_none() && !self.args.is_null() {
            // 未知/自定义工具：展示脱敏后的参数摘要（截断，避免把整个文件内容塞进标题）。
            let summary = redacted_display_args(&self.args);
            let summary = serde_json::to_string(&summary).unwrap_or_default();
            if !summary.is_empty() && summary != "null" {
                title.push_str("\nArguments: ");
                title.push_str(&truncate_display(&summary, 300));
            }
        }
        if !self.cwd.is_empty() {
            title.push_str("\nWorking directory: ");
            title.push_str(&self.cwd);
        }
        if !self.decision_reason.is_empty() {
            title.push_str("\nReason: ");
            title.push_str(&self.decision_reason);
        }
        title
    }
}

/// `args` 里第一个存在的字符串字段。
fn display_arg<'a>(args: &'a serde_json::Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| args.get(*key).and_then(serde_json::Value::as_str))
}

fn truncate_display(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        return value.to_string();
    }
    let mut truncated = value.chars().take(limit).collect::<String>();
    truncated.push('…');
    truncated
}

/// 事件/标题里的参数脱敏：敏感键整体替换，其余字符串截断（与权限层的审计摘要同一
/// 思路，本 crate 不依赖权限层，因此在这里保留最小实现）。
fn redacted_display_args(value: &serde_json::Value) -> serde_json::Value {
    fn sensitive(key: &str) -> bool {
        let key = key.to_ascii_lowercase();
        [
            "token",
            "secret",
            "password",
            "passwd",
            "api_key",
            "apikey",
            "api-key",
            "authorization",
            "credential",
            "cookie",
            "private_key",
            "private-key",
            "access_key",
            "access-key",
        ]
        .iter()
        .any(|needle| key.contains(needle))
    }
    fn redact(value: &serde_json::Value, depth: u8) -> serde_json::Value {
        if depth > 4 {
            return serde_json::Value::String("…".to_string());
        }
        match value {
            serde_json::Value::Object(map) => {
                let mut output = serde_json::Map::new();
                for (key, value) in map {
                    if sensitive(key) {
                        output.insert(
                            key.clone(),
                            serde_json::Value::String("[redacted]".to_string()),
                        );
                    } else {
                        output.insert(key.clone(), redact(value, depth + 1));
                    }
                }
                serde_json::Value::Object(output)
            }
            serde_json::Value::Array(items) => serde_json::Value::Array(
                items
                    .iter()
                    .take(16)
                    .map(|item| redact(item, depth + 1))
                    .collect(),
            ),
            serde_json::Value::String(text) => {
                serde_json::Value::String(truncate_display(text, 120))
            }
            other => other.clone(),
        }
    }
    redact(value, 0)
}

/// 审批卡上的选择。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PermissionCardChoice {
    /// 只允许这一次。
    AllowOnce,
    /// 写回指定作用域的规则。
    AllowAlways(PermissionScopeKind),
    /// 拒绝；`reason` 原样回传给模型。
    Reject {
        /// 用户给出的原因。
        reason: Option<String>,
    },
}

/// 扩展 widget 的放置位置。
///
/// TS: `types.ts#WidgetPlacement`（`"aboveEditor" | "belowEditor"`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WidgetPlacement {
    /// `"aboveEditor"`（默认）。
    #[default]
    AboveEditor,
    /// `"belowEditor"`。
    BelowEditor,
}

impl WidgetPlacement {
    /// TS 的字面量（`"aboveEditor"` / `"belowEditor"`）。
    ///
    /// 本类型不在 serde 面上（[`ExtensionWidgetOptions`] 是 UI 选项，`ARCHITECTURE.md` §4.3
    /// 不为 UI 选项提供 serde），该方法给需要线上字面量的调用方（以及测试）使用。
    pub fn as_str(self) -> &'static str {
        match self {
            WidgetPlacement::AboveEditor => "aboveEditor",
            WidgetPlacement::BelowEditor => "belowEditor",
        }
    }
}

/// 扩展 widget 的选项。TS: `types.ts#ExtensionWidgetOptions`。
#[derive(Debug, Clone, Default)]
pub struct ExtensionWidgetOptions {
    /// 渲染位置；缺省 `aboveEditor`。
    pub placement: Option<WidgetPlacement>,
}

/// 原始终端输入监听器。
///
/// TS: `types.ts#TerminalInputHandler`
/// （`(data: string) => { consume?: boolean; data?: string } | undefined`）。
///
/// 返回 `None` 表示不处理（输入继续走默认路径）；`Some` 的字段见
/// [`TerminalInputResult`]。交互式实现把结果原样转成 `kiana-tui` 的
/// `TuiInputListenerResult`：`consume` 为真时终止后续监听器与聚焦组件，`data` 替换输入。
pub type TerminalInputHandler = Arc<dyn Fn(&str) -> Option<TerminalInputResult> + Send + Sync>;

/// [`TerminalInputHandler`] 的返回值。
///
/// TS 的内联对象 `{ consume?: boolean; data?: string }`；返回 `None` 对应 TS 的 `undefined`。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TerminalInputResult {
    /// 是否消费掉该输入（不再传给默认处理）。
    pub consume: Option<bool>,
    /// 替换后的输入数据。
    pub data: Option<String>,
}

/// 流式指示器配置。
///
/// TS: `types.ts#WorkingIndicatorOptions`。`frames: []` 隐藏指示器；自定义帧按原样渲染。
/// TS 的 `intervalMs` 是 `number`，用 `f64` 保留 JS 数值语义。
#[derive(Debug, Clone, Default)]
pub struct WorkingIndicatorOptions {
    /// 动画帧；空数组表示完全隐藏。
    pub frames: Option<Vec<String>>,
    /// 帧间隔（毫秒），仅动画指示器使用。
    pub interval_ms: Option<f64>,
}

/// 自动补全 provider 包装工厂。
///
/// TS: `types.ts#AutocompleteProviderFactory`
/// （`(current: AutocompleteProvider) => AutocompleteProvider`）。
/// kiana-tui 的编辑器持有 `Box<dyn AutocompleteProvider>`，因此这里同样按所有权传递。
///
/// 工厂接收当前（内建）provider 并返回包装后的 provider。`build_autocomplete_provider`
/// 每次构建 provider 时按注册顺序调用每个 wrapper；工厂以 `Arc<dyn Fn>` 保存
/// （`autocomplete_provider_wrappers`），可从共享引用重复调用。
pub type AutocompleteProviderFactory =
    Arc<dyn Fn(Box<dyn AutocompleteProvider>) -> Box<dyn AutocompleteProvider> + Send + Sync>;

/// 自定义编辑器工厂。
///
/// TS: `types.ts#EditorFactory`
/// （`(tui: TUI, theme: EditorTheme, keybindings: KeybindingsManager) => EditorComponent`）。
pub type EditorFactory = Arc<
    dyn Fn(Arc<dyn Tui>, &EditorTheme, Arc<KeybindingsManager>) -> Box<dyn EditorComponent>
        + Send
        + Sync,
>;

/// 通知类型。TS: `types.ts#ExtensionUIContext.notify` 的 `"info" | "warning" | "error"`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NotifyKind {
    /// `"info"`。
    #[default]
    Info,
    /// `"warning"`。
    Warning,
    /// `"error"`。
    Error,
}

/// 一个可用主题的名字与路径。
///
/// TS: `types.ts#ExtensionUIContext.getAllThemes` 的 `{ name: string; path: string | undefined }`。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ThemeInfo {
    /// 主题名。
    pub name: String,
    /// 主题文件路径（内置主题没有）。
    pub path: Option<String>,
}

/// [`ExtensionUiContext::set_theme`] 的结果。
///
/// TS: `{ success: boolean; error?: string }`。无 UI 时的默认结果：
/// `{ success: false, error: "UI not available" }`（逐字；no-op 行为本身在 `kiana-extensions`）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ThemeResult {
    /// 是否成功切换。
    pub success: bool,
    /// 失败原因。
    pub error: Option<String>,
}

/// [`ExtensionUiContext::set_theme`] 的输入：主题名或主题实例。
///
/// TS: `string | Theme`。
#[allow(clippy::large_enum_variant)]
pub enum ThemeOrName {
    /// 按名字切换。
    Name(String),
    /// 直接切换实例（不写设置）。
    Theme(Theme),
}

// 手写 Debug：输出形态被 tests/ui_contract.rs 的 theme_info_and_theme_or_name_debug 固定
// （`Name("dark")`、以 `Theme(` 开头），改动会直接破坏该断言。
impl std::fmt::Debug for ThemeOrName {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ThemeOrName::Name(name) => formatter.debug_tuple("Name").field(name).finish(),
            ThemeOrName::Theme(theme) => formatter.debug_tuple("Theme").field(theme).finish(),
        }
    }
}

/// 组件型 widget 的工厂。
///
/// TS: `types.ts#ExtensionUIContext.setWidget` 第 2 个重载的
/// `(tui: TUI, theme: Theme) => Component & { dispose?(): void }`。
pub type WidgetFactory = Arc<dyn Fn(Arc<dyn Tui>, &Theme) -> Box<dyn Component> + Send + Sync>;

/// widget 的内容。
///
/// TS 的两个重载：`string[]` 或组件工厂；`undefined` 表示清除（调用点用 `Option` 表达）。
pub enum WidgetContent {
    /// 以文本行渲染。
    Lines(Vec<String>),
    /// 以组件工厂渲染。
    Component(WidgetFactory),
}

// 手写 Debug：WidgetFactory 是 trait 对象，没有 Debug；组件变体只打印变体名。
impl std::fmt::Debug for WidgetContent {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WidgetContent::Lines(lines) => formatter.debug_tuple("Lines").field(lines).finish(),
            WidgetContent::Component(_) => formatter.debug_tuple("Component").finish(),
        }
    }
}

/// 自定义 footer 组件的工厂。
///
/// TS: `types.ts#ExtensionUIContext.setFooter` 的
/// `(tui: TUI, theme: Theme, footerData: ReadonlyFooterDataProvider) => Component & { dispose?(): void }`。
pub type FooterFactory = Arc<
    dyn Fn(Arc<dyn Tui>, &Theme, Arc<dyn ReadonlyFooterDataProvider>) -> Box<dyn Component>
        + Send
        + Sync,
>;

/// 自定义 header 组件的工厂。
///
/// TS: `types.ts#ExtensionUIContext.setHeader` 的
/// `(tui: TUI, theme: Theme) => Component & { dispose?(): void }`。
pub type HeaderFactory = Arc<dyn Fn(Arc<dyn Tui>, &Theme) -> Box<dyn Component> + Send + Sync>;

/// [`ExtensionUiContext::custom`] 的 `done(result)` 回调。
///
/// TS 的 `done: (result: T) => void`。`T` 被擦除为 `Box<dyn Any + Send>`，调用方
/// `downcast`（`ARCHITECTURE.md` §5.2 记录的偏离：trait 方法不能有泛型参数）。
///
/// 调用 `done` 是完成 [`ExtensionUiContext::custom`] 的正常途径；交互式实现按请求 token
/// 取走 responder，重复调用 `done` 不会二次解析。宿主销毁（UI 模式已无法升级）时会以
/// 默认值（擦除后的 `()`）解析剩余 responder（`resolve_responder_default`）。
pub type CustomDoneFn = Arc<dyn Fn(Box<dyn Any + Send>) + Send + Sync>;

/// 自定义组件工厂。
///
/// TS: `types.ts#ExtensionUIContext.custom` 的
/// `(tui: TUI, theme: Theme, keybindings: KeybindingsManager, done) => Component | Promise<Component>`。
/// 返回的 future 是非 `Send` 的 `LocalBoxFuture`（组件是 `!Send`）。
///
/// 正常交互流程由工厂调用 `done` 结束；`done` 承载的值类型由工厂与调用方约定
/// （见 [`CustomDoneFn`]）。
pub type CustomFactory = Arc<
    dyn Fn(
            Arc<dyn Tui>,
            &Theme,
            Arc<KeybindingsManager>,
            CustomDoneFn,
        ) -> LocalBoxFuture<'static, Box<dyn Component>>
        + Send
        + Sync,
>;

/// [`ExtensionUiContext::custom`] 的选项。
///
/// TS 的内联对象 `{ overlay?: boolean; overlayOptions?: OverlayOptions | (() => OverlayOptions);
/// onHandle?: (handle: OverlayHandle) => void }`。
///
/// `overlay` 缺省为 `false`（组件占据编辑器槽）；为 `true` 时通过 `Tui::show_overlay` 显示，
/// 并在创建后调用 `on_handle`（`kiana-interactive` 的 `mount_custom_component`）。
pub struct CustomOptions {
    /// 以 overlay 形式显示。
    pub overlay: Option<bool>,
    /// overlay 的定位/尺寸选项（静态或动态）。
    pub overlay_options: Option<OverlayOptionsOrFn>,
    /// overlay 显示后回调句柄（用于控制可见性）。
    pub on_handle: Option<Arc<dyn Fn(Box<dyn OverlayHandle>) + Send + Sync>>,
}

// 手写 Debug：`on_handle` 是 trait 对象，只能打印是否存在；`overlay` 打印实际布尔值，
// `overlay_options` 使用占位 Debug 输出。
impl std::fmt::Debug for CustomOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CustomOptions")
            .field("overlay", &self.overlay)
            .field("overlay_options", &self.overlay_options)
            .field("on_handle", &self.on_handle.is_some())
            .finish()
    }
}

/// overlay 选项的静态值或动态函数（TS `OverlayOptions | (() => OverlayOptions)`）。
pub enum OverlayOptionsOrFn {
    /// 静态值。
    Static(OverlayOptions),
    /// 在组件挂载时求值一次（`kiana-interactive` 的 `mount_custom_component` 直接调用它）。
    Dynamic(Arc<dyn Fn() -> OverlayOptions + Send + Sync>),
}

// 手写 Debug：Dynamic 是闭包，没有 Debug；只打印占位符，避免展开静态选项。
impl std::fmt::Debug for OverlayOptionsOrFn {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OverlayOptionsOrFn::Static(_) => formatter.write_str("Static(OverlayOptions)"),
            OverlayOptionsOrFn::Dynamic(_) => formatter.write_str("Dynamic(..)"),
        }
    }
}

/// 扩展请求交互式 UI 的上下文。
///
/// TS: `types.ts#ExtensionUIContext`。Rust 名按 `ARCHITECTURE.md` §5.2 冻结为
/// `ExtensionUiContext`（不是 `ExtensionUIContext`）。
///
/// 共 28 个成员：27 个方法 + 只读 [`ExtensionUiContext::theme`]；TS 的两个 `setWidget`
/// 重载折叠为一个接收 [`WidgetContent`] 的方法（调用点的 `None` 表示 TS 的 `undefined`/清除）。
///
/// 方法的行为契约（no-op 默认值、无 UI 时的返回值）由 `kiana-extensions`/`kiana-interactive`
/// 实现；本 crate 只有声明。无 UI 时 `set_theme` 的默认结果是
/// `{ success: false, error: "UI not available" }`（逐字）。
///
/// # 为什么必须是对象安全的
///
/// 扩展 runner 以 `Arc<dyn ExtensionUiContext>` 保存并共享实现（`kiana-extensions` 的
/// no-op 单例），工具上下文则以 `&dyn ExtensionUiContext` 暴露（`kiana-tools` 的占位实现），
/// 因此 trait 不能有未加 `where Self: Sized` 的泛型方法（本 trait 也没有保留任何泛型方法）。
/// TS 的泛型 `custom<T>` 只能把结果
/// 擦除成 `Box<dyn Any + Send>`（见 [`CustomDoneFn`]），由调用方 `downcast`；`setWidget`
/// 的两个重载折叠成一个 `Option<WidgetContent>` 参数。
///
/// # 实现约束
///
/// - `Send + Sync` 是对实现的要求：`&self` 可能被跨线程共享，内部可变状态需自行同步。
/// - 异步方法的 future 只借用 `&self`；调用方必须让上下文活到 future 完成。
/// - 回调参数都带 `+ Send + Sync`，但返回值里的 `Box<dyn Component>` 是 `!Send`；
///   本仓库的交互式实现只在 TUI 线程调用这些工厂。
pub trait ExtensionUiContext: Send + Sync {
    /// 显示选择器并返回用户选择。
    ///
    /// TS: `select(title, options, opts?) => Promise<string | undefined>`；无 UI 时 `None`。
    fn select(
        &self,
        title: &str,
        options: &[String],
        opts: Option<ExtensionUIDialogOptions>,
    ) -> LocalBoxFuture<'_, Option<String>>;

    /// per-tool 审批卡（第 12 周接线）。
    ///
    /// 默认实现退化成 [`Self::select`]（+ 拒绝原因输入），因此 RPC/print/扩展侧不需要
    /// 实现；TUI 实现可以覆盖为 `ApprovalCard` + `ModalArbiter`。
    ///
    /// 返回 `None` 表示 UI 已关闭；调用方必须 fail-closed 当成 `Reject`。
    fn permission_card(
        &self,
        request: PermissionCardRequest,
    ) -> LocalBoxFuture<'_, Option<PermissionCardChoice>> {
        Box::pin(async move {
            // 标题必须包含工具名与实际命令/路径（`card_title`），否则用户是在盲批。
            let title = request.card_title();
            // 没有可持久化作用域时（MCP destructive/open-world、`.git`/`.kiana` 安全确认）
            // 不展示「Always allow」：选择后只会得到一个拒绝，属于误导。
            let mut options = vec!["Allow once".to_string()];
            if !request.always_scopes.is_empty() {
                options.push("Always allow".to_string());
            }
            options.push("Reject".to_string());
            let Some(choice) = self.select(&title, &options, None).await else {
                return Some(PermissionCardChoice::Reject {
                    reason: Some("Approval was cancelled.".to_string()),
                });
            };
            Some(match choice.as_str() {
                "Allow once" => PermissionCardChoice::AllowOnce,
                "Always allow" => {
                    // 与第 1 周的结构化审批一致：先选作用域，再写回规则。只展示调用方
                    // 允许的作用域（MCP destructive/空向量时可能只有 Session）。
                    let mut scopes = Vec::new();
                    let mut labels = Vec::new();
                    if request
                        .always_scopes
                        .contains(&PermissionScopeKind::Session)
                    {
                        scopes.push(PermissionScopeKind::Session);
                        labels.push("This session".to_string());
                    }
                    if request
                        .always_scopes
                        .contains(&PermissionScopeKind::Project)
                    {
                        scopes.push(PermissionScopeKind::Project);
                        labels.push("This project".to_string());
                    }
                    if request.always_scopes.contains(&PermissionScopeKind::Local) {
                        scopes.push(PermissionScopeKind::Local);
                        labels.push("This machine (local)".to_string());
                    }
                    if scopes.is_empty() {
                        return Some(PermissionCardChoice::Reject {
                            reason: Some("No persistent approval scope is available.".to_string()),
                        });
                    }
                    let Some(label) = self
                        .select("Remember this permission?", &labels, None)
                        .await
                    else {
                        return Some(PermissionCardChoice::Reject {
                            reason: Some("Approval was cancelled.".to_string()),
                        });
                    };
                    match labels.iter().position(|candidate| candidate == &label) {
                        Some(index) => PermissionCardChoice::AllowAlways(scopes[index]),
                        None => PermissionCardChoice::Reject {
                            reason: Some("Unknown permission scope.".to_string()),
                        },
                    }
                }
                "Reject" => {
                    let reason = self
                        .input("Reason for rejecting (optional)", None, None)
                        .await
                        .and_then(|reason| {
                            let reason = reason.trim().to_string();
                            (!reason.is_empty()).then_some(reason)
                        });
                    PermissionCardChoice::Reject { reason }
                }
                _ => PermissionCardChoice::Reject {
                    reason: Some("Unknown permission choice.".to_string()),
                },
            })
        })
    }

    /// 显示确认对话框。TS: `confirm(title, message, opts?) => Promise<boolean>`；无 UI 时 `false`。
    fn confirm(
        &self,
        title: &str,
        message: &str,
        opts: Option<ExtensionUIDialogOptions>,
    ) -> LocalBoxFuture<'_, bool>;

    /// 显示文本输入框。
    ///
    /// TS: `input(title, placeholder?, opts?) => Promise<string | undefined>`；无 UI 时 `None`。
    fn input(
        &self,
        title: &str,
        placeholder: Option<&str>,
        opts: Option<ExtensionUIDialogOptions>,
    ) -> LocalBoxFuture<'_, Option<String>>;

    /// 显示通知。TS: `notify(message, type?)`；无 UI 时 no-op。
    fn notify(&self, message: &str, kind: Option<NotifyKind>);

    /// 监听原始终端输入；返回退订闭包。TS: `onTerminalInput(handler)`；无 UI 时返回 no-op 退订。
    fn on_terminal_input(&self, handler: TerminalInputHandler) -> Unsubscribe;

    /// 设置 footer/状态栏文本；`None` 表示清除。TS: `setStatus(key, text)`。
    ///
    /// 交互式实现把状态写入 footer 数据源，
    /// [`ReadonlyFooterDataProvider::get_extension_statuses`] 能读到；`None` 删除该 key
    /// （`kiana-interactive` 的 `set_extension_status`）。
    fn set_status(&self, key: &str, text: Option<&str>);

    /// 设置流式期间的 working 消息；`None` 恢复默认。TS: `setWorkingMessage(message?)`。
    fn set_working_message(&self, message: Option<&str>);

    /// 显示/隐藏内建 working loader 行。TS: `setWorkingVisible(visible)`。
    fn set_working_visible(&self, visible: bool);

    /// 配置 working 指示器。TS: `setWorkingIndicator(options?)`；`None` 恢复默认动画。
    fn set_working_indicator(&self, options: Option<WorkingIndicatorOptions>);

    /// 设置隐藏 thinking 块的标签；`None` 恢复默认。TS: `setHiddenThinkingLabel(label?)`。
    fn set_hidden_thinking_label(&self, label: Option<&str>);

    /// 设置显示在编辑器上方/下方的 widget；`None` 清除。
    ///
    /// TS 的两个重载折叠为 `content: Option<WidgetContent>`。同一个 `key` 已存在时替换旧
    /// widget 并对其调用 `dispose`（TS 的 `removeExisting`；`kiana-interactive` 的
    /// `set_extension_widget`）。
    fn set_widget(
        &self,
        key: &str,
        content: Option<WidgetContent>,
        options: Option<ExtensionWidgetOptions>,
    );

    /// 设置自定义 footer；`None` 恢复内建 footer。TS: `setFooter(factory)`。
    fn set_footer(&self, factory: Option<FooterFactory>);

    /// 设置自定义 header；`None` 恢复内建 header。TS: `setHeader(factory)`。
    fn set_header(&self, factory: Option<HeaderFactory>);

    /// 设置终端窗口/标签标题。TS: `setTitle(title)`。
    fn set_title(&self, title: &str);

    /// 显示带键盘焦点的自定义组件，完成后返回 `done` 的结果。
    ///
    /// TS: `custom<T>(factory, options?) => Promise<T>`。`T` 被擦除为
    /// `Box<dyn Any + Send>`（调用方 `downcast`；`T` 必须是 `Any + Send + 'static`）。
    /// 无 UI 时立即以擦除后的 `()` 完成，工厂不会被调用（no-op 与 RPC 实现）。
    fn custom(
        &self,
        factory: CustomFactory,
        options: Option<CustomOptions>,
    ) -> LocalBoxFuture<'_, Box<dyn Any + Send>>;

    /// 把文本粘贴进编辑器（触发 paste 处理，大内容折叠）。TS: `pasteToEditor(text)`。
    fn paste_to_editor(&self, text: &str);

    /// 设置核心输入编辑器的文本。TS: `setEditorText(text)`。
    ///
    /// 交互式实现会同步更新快照，使紧接着的 [`ExtensionUiContext::get_editor_text`] 立即
    /// 看到新值（TS 的同步属性写入）。
    fn set_editor_text(&self, text: &str);

    /// 读取核心输入编辑器的当前文本。TS: `getEditorText()`；无 UI 时空串。
    fn get_editor_text(&self) -> String;

    /// 显示多行文本编辑器。TS: `editor(title, prefill?) => Promise<string | undefined>`。
    fn editor(&self, title: &str, prefill: Option<&str>) -> LocalBoxFuture<'_, Option<String>>;

    /// 在内建 provider 之上叠加额外的自动补全行为。TS: `addAutocompleteProvider(factory)`。
    fn add_autocomplete_provider(&self, factory: AutocompleteProviderFactory);

    /// 设置自定义编辑器组件工厂；`None` 恢复默认编辑器。TS: `setEditorComponent(factory)`。
    fn set_editor_component(&self, factory: Option<EditorFactory>);

    /// 当前自定义编辑器工厂；使用默认编辑器时为 `None`。TS: `getEditorComponent()`。
    fn get_editor_component(&self) -> Option<EditorFactory>;

    /// 当前用于样式化的主题。TS 的只读属性 `theme`。
    ///
    /// 返回借用而不是 `Arc`：实现必须保证引用在 `&self` 借用期内有效。三个生产实现
    /// （`kiana-extensions` 的 no-op、`kiana-interactive`、`kiana-rpc-print`）都把用到的
    /// `Arc<Theme>` 保存在进程级历史表里，因此主题切换后旧引用也不会悬空。
    fn theme(&self) -> &Theme;

    /// 所有可用主题的名字与路径。TS: `getAllThemes()`；无 UI 时空数组。
    fn get_all_themes(&self) -> Vec<ThemeInfo>;

    /// 按名字加载主题（不切换）。TS: `getTheme(name)`；不存在时 `None`。
    fn get_theme(&self, name: &str) -> Option<Theme>;

    /// 按名字或实例切换主题。
    ///
    /// TS: `setTheme(theme: string | Theme) => { success: boolean; error?: string }`；
    /// 无 UI 时 `{ success: false, error: "UI not available" }`。
    fn set_theme(&self, theme: ThemeOrName) -> ThemeResult;

    /// 工具输出当前是否展开。TS: `getToolsExpanded()`；无 UI 时 `false`。
    fn get_tools_expanded(&self) -> bool;

    /// 设置工具输出展开状态。TS: `setToolsExpanded(expanded)`。
    fn set_tools_expanded(&self, expanded: bool);
}

/// 只读 footer 数据提供者。
///
/// TS: `core/footer-data-provider.ts#ReadonlyFooterDataProvider`
/// （`Pick<FooterDataProvider, "getGitBranch" | "getExtensionStatuses" |
/// "getAvailableProviderCount" | "onBranchChange">`）。实现位于
/// `kiana-resources::footer_data_provider`。
///
/// 该 trait 不在 `index.rs` 的 barrel 里（TS 的 `types.ts` 只 import 不 export）；Rust
/// 调用方通过 `kiana_ext_api::ui::ReadonlyFooterDataProvider` 引用。
pub trait ReadonlyFooterDataProvider: Send + Sync {
    /// 当前 git 分支：不在仓库中或 git 不可用时为 `None`；detached HEAD 时是**小写**
    /// 字面量 `"detached"`（`footer-data-provider.ts:126/245/247/260/263`）。
    fn get_git_branch(&self) -> Option<String>;

    /// 通过 `ctx.ui.setStatus()` 设置的扩展状态文本。
    ///
    /// TS 返回**实时**的 `ReadonlyMap`（不是快照），因此这里返回引用。返回引用意味着实现
    /// 需要内部可变性：`kiana-resources::FooterDataProvider` 把表放在 `UnsafeCell` 中，
    /// 写操作限定在 UI 线程（trait 本身只含只读方法）。
    fn get_extension_statuses(&self) -> &IndexMap<String, String>;

    /// 有可用模型的 provider 数量（footer 展示用）。
    fn get_available_provider_count(&self) -> usize;

    /// 订阅 git 分支变化；返回退订闭包。
    ///
    /// TS 的回调**不带参数**；用 [`Unsubscribe`] 作为退订句柄。退订后后续通知不会再取到该
    /// 回调；已开始的一轮通知仍可能调用它一次（`notify_branch_change` 先克隆回调列表再在
    /// 锁外调用）。重复退订是无操作（`kiana-resources` 按注册 id 从回调表移除）。
    fn on_branch_change(&self, callback: Arc<dyn Fn() + Send + Sync>) -> Unsubscribe;
}
