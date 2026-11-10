//! extensions/llama/ui.ts。
//!
//! `/llama` 的 TUI：模型列表、通用选择、确认、连接错误、Hugging Face 富搜索、进度页与
//! `runWithProgress` 的取消竞态。TS 的 Promise/Focusable 语义用 `oneshot` + `Rc<RefCell<_>>`
//! 复刻；异步搜索用 `spawn_local`（扩展运行时是单线程 `LocalSet`，见 `ARCHITECTURE.md` §4.2）。
//!
//! 布局常量（spec §3.8）：主列表视口 `min(items, 12)`、主列宽 `36..56`；富搜索视口 10；
//! 进度条 40 格（`█` 填充、`─` 空白）。
//!
//! 与 TS 的表示差异：
//! - TS 的 `Theme` 是类实例；Rust 的 `Theme` 不是 `Clone`，`ctx.ui().custom` 工厂只借出
//!   `&Theme`。视图与 `mcp/ui.rs` 一样在工厂里捕获 `ThemeSnapshot`，因此渲染使用宿主
//!   传入的主题（TS: `new LlamaView(tui, theme, keybindings)`），而不是全局当前主题。
//! - TS 的 `inputHandler`/`inputTarget` 是同一对象的两个视图；Rust 用 `InputHandle` 枚举
//!   同时表达它们。
//!
//! # 谁在使用
//! `llama/index.rs` 的 `/llama` 命令是唯一的生产调用方：它先要求 TUI 模式，然后用
//! [`show_llama_ui`] 注册自定义组件，并在 `custom(...).await` 上等到 `done`；
//! 管理器循环、加载与下载的界面交互都通过 [`LlamaUi`] trait，长任务用
//! [`run_with_progress`]。本文件的测试用 `FakeUi` 等 trait 实现把“用户按键”换成
//! `oneshot` 信号。
//!
//! # 一次典型流程
//! 1. `show_llama_ui` 的工厂拿到 `Arc<dyn Tui>`、`&Theme`、`KeybindingsManager` 与 `done`，
//!    捕获主题快照并构造 `LlamaView`。
//! 2. 工厂返回 `LlamaViewComponent`，初始内容是 “Loading…” 状态页。
//! 3. 工厂里的 `spawn_local` 执行 `run(handle)`；它通过 trait 方法切换页面并等待用户输入。
//! 4. `run` 结束（正常或 `Err(AiError)`）后，错误用 `ctx.ui().notify(..., Error)` 显示，
//!    随后调用 `done`，宿主据此结束 `custom`。
//!
//! # 并发与生命周期
//! - UI 状态全部在单线程上：`Rc<RefCell<_>>` 与 `Rc<dyn Fn()>` 都是 `!Send`，
//!   `spawn_local` 只能在 `LocalSet` 里调用；`CustomFactory` 返回的 future 也是非
//!   `Send` 的 `LocalBoxFuture`（见 `kiana-ext-api` 的 `CustomFactory` 文档）。因此视图
//!   句柄无法跨线程。
//! - 每个“等待用户结果”的 future 都由 `oneshot` 支撑：发送端放在
//!   `Rc<RefCell<Option<Sender>>>` 里，由 `send_once` 消费一次。内容被替换或视图关闭时
//!   发送端被 drop，接收端的 `Err` 在调用点折叠成“取消”语义（`None`/`Close`/`false`）。
//! - 重绘通过 `request_render` 完成；闭包用 `Weak` 指回 TUI，避免组件与 TUI 形成
//!   `Arc` 环。
//!
//! # 失败模式
//! - `run` 返回 `Err(AiError)`：只显示 `error.message`，UI 仍会关闭（`done` 总会调用）。
//! - 搜索被新查询或关闭中止：过期结果可以写缓存，但不会落到界面（`run_search` 入口用
//!   `debounce_generation` 与 `query` 拦截延迟任务，收到结果后再用 `closed`/`is_aborted()`/
//!   `query` 判定是否写回界面）。
//! - 进度页的 promise 在 `set_content` 清空状态、用户按取消、或发送端被 drop 时结束；
//!   `run_with_progress` 的取消路径要等 `cancel()` 与 abort 都完成才返回
//!   [`RunProgressOutcome::Cancelled`]。
//!
//! # 阅读顺序
//! 纯格式化函数 → [`LlamaUi`] → `HuggingFaceSearch` → `LlamaView` → [`show_llama_ui`]
//! → [`run_with_progress`] → 文件末尾的测试。

// UI 层把 TS 的 `throw`/`Promise` 直接映射为 `Result<_, AiError>`；`AiError` 是 128 字节的
// 共享契约，公开签名不装箱（与 `kiana-ai`/`kiana-config` 的同类模块一致）。
#![allow(clippy::result_large_err)]

use std::cell::RefCell;
use std::collections::HashMap;
use std::collections::HashSet;
use std::future::Future;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::FutureExt;
use futures::channel::oneshot;
use futures::future::Shared;
use kiana_ai::error::AiError;
use kiana_config::keybindings::KeybindingsManager;
use kiana_ext_api::LocalBoxFuture;
use kiana_ext_api::types::ExtensionCommandContextApi;
use kiana_ext_api::ui::{CustomFactory, NotifyKind};
use kiana_theme::ThemeColor;
use kiana_tui::autocomplete::locale_compare;
use kiana_tui::components::input::{Input, InputOptions};
use kiana_tui::components::select_list::{
    SelectItem, SelectList, SelectListLayoutOptions, SelectListTheme,
};
use kiana_tui::components::spacer::Spacer;
use kiana_tui::components::text_component::Text;
use kiana_tui::fuzzy::fuzzy_filter;
use kiana_tui::text::{truncate_to_width, visible_width};
use kiana_tui::tui::{Component, Container, Focusable, Tui};
use kiana_ui_kit::DynamicBorder;
use kiana_ui_kit::key_hint;

use crate::theme_snapshot::ThemeSnapshot;

use super::client::{LlamaModelInfo, LlamaProgress, js_number};
use super::huggingface::HuggingFaceModel;

/// 下载项的哨兵值（NUL 前缀，不可能与模型 id 冲突）。
const DOWNLOAD_VALUE: &str = "\0download";

/// `/llama` 主菜单的动作。
///
/// TS 的联合 `{type:"model",model} | {type:"download"} | {type:"close"}` 直接映射；
/// 装箱 `model` 会让调用点的 `LlamaManagerAction::Model(model)` 多一层解引用（偏离 TS），
/// 因此接受 `large_enum_variant`。
///
/// 三个分支由 `llama/index.rs` 的 `run_manager_loop` 消费：`Close` 直接结束循环（不再
/// 刷新目录），`Download` 进入 Hugging Face 搜索/下载流程，`Model` 按
/// `model.status.value` 分派到加载或卸载。调用方直接按快照里的 `model.status.value`
/// 分派（`run_manager_loop` 不会重新校验目录）；展示后状态若已变化，分派与后续动作
/// 可能基于旧值（该文件把 `loaded` 与 `sleeping` 都算已加载）。
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum LlamaManagerAction {
    /// `{ type: "model", model }`。
    ///
    /// 列表项的 `value` 是模型 id；选择回调再从展示时建立的 id→模型 映射取回完整快照。
    Model(LlamaModelInfo),
    /// `{ type: "download" }`。
    ///
    /// 选中列表末尾固定的 `Download model…` 哨兵项时返回。
    Download,
    /// `{ type: "close" }`。
    ///
    /// 取消键触发；调用方据此结束管理器循环。
    Close,
}

/// `ProgressState`（`LlamaProgress` + `title`/`model`）。
///
/// 这是 `run_with_progress` 持有的**可变状态**：每次进度更新都克隆一份快照交给
/// [`LlamaUi::update_progress`]，视图不会回写它。`ratio` 与 `detail` 的 `None` 分别表示
/// “不画进度条”和“没有细节行”。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProgressState {
    /// 页面标题。
    pub title: String,
    /// 模型 id；进度页顶部以 `ThemeColor::Text` 显示。
    pub model: String,
    /// 进度文案（首条是 `initial_message`）。
    pub message: String,
    /// 比例。`None` 不渲染进度条；超出 `0..=1` 的值只夹取进度条的填充格数，
    /// 百分比仍按原值取整（见 [`LlamaUi::update_progress`]）。
    pub ratio: Option<f64>,
    /// 细节（`done / total`）。`Some("")` 与 `None` 一样不会渲染空行（代码判断
    /// `!detail.is_empty()`）。
    pub detail: Option<String>,
}

/// `connectionError` 的选择结果。
///
/// 调用方（`llama/index.rs` 的 `read_catalog`）在 `Retry` 时重新拉取目录，在 `Close`
/// 时放弃并返回 `None`。用户按取消键也落到 `Close`：`select` 返回 `None`，不是
/// `"Retry"`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionOutcome {
    /// `"retry"`。
    Retry,
    /// `"close"`。
    Close,
}

/// Hugging Face 搜索回调：`search(query, signal) => Promise<HuggingFaceModel[]>`。
///
/// `query` 是去掉首尾空白后的输入；`AbortScope` 在新查询到达或搜索页关闭时被 abort，
/// 实现应通过它取消 HTTP 请求（`kiana_ai::utils::abort`）。返回的 future 必须是
/// `'static`，因为它在 `spawn_local` 的防抖任务里等待；失败信息会写进搜索页的
/// `status`（见 `run_search`），不会冒泡给调用方。
pub type SearchModelsFn = Rc<
    dyn Fn(
        String,
        kiana_ai::utils::abort::AbortScope,
    ) -> LocalBoxFuture<'static, Result<Vec<HuggingFaceModel>, AiError>>,
>;

/// `/llama` 的 UI 抽象（TS `LlamaUi`）。
///
/// 把“画页面并等用户回答”从管理器逻辑里剥离出来：`llama/index.rs` 只按这个 trait 编写，
/// 测试用 `FakeUi` 直接给出答案，不需要终端。
///
/// 约定：
/// - 所有 future 都是单线程的 `LocalBoxFuture`，在 UI 线程上轮询；实现内部可以使用
///   `Rc<RefCell<_>>`。
/// - 选择类方法会替换当前页面（`confirm`/`connection_error` 在 future 被轮询时委托给
///   `select`）；返回的 future 在用户作出选择、或页面被替换导致发送端被 drop 时完成，
///   后者按“取消”处理。
/// - 调用方顺序 await 每个选择页，不同时发起两次等待。
pub trait LlamaUi {
    /// 主模型列表。
    ///
    /// 返回 `Download`/`Model`/`Close`；取消键与页面被替换都归为 `Close`。视图实现总会
    /// 附加固定的下载项，所以页面上不会出现空列表。
    fn show_models<'a>(
        &'a self,
        server_url: &'a str,
        models: &'a [LlamaModelInfo],
    ) -> LocalBoxFuture<'a, LlamaManagerAction>;
    /// 通用选择（取消 → `None`）。
    ///
    /// 每个选项同时作为 value 与 label；取消键或页面被替换都返回 `None`。
    fn select<'a>(
        &'a self,
        title: &'a str,
        options: &'a [String],
    ) -> LocalBoxFuture<'a, Option<String>>;
    /// 确认（`["Yes", "No"]`）。
    ///
    /// 标题与消息用换行拼接后作为选择页标题；只有选中 `Yes` 才返回 `true`，取消或页面
    /// 被替换返回 `false`。
    fn confirm<'a>(&'a self, title: &'a str, message: &'a str) -> LocalBoxFuture<'a, bool>;
    /// 连接错误（Retry/Close）。
    ///
    /// `server_url` 与 `message` 会拼进多行标题；只有选中 `Retry` 才返回
    /// [`ConnectionOutcome::Retry`]，取消也返回 [`ConnectionOutcome::Close`]。
    fn connection_error<'a>(
        &'a self,
        server_url: &'a str,
        message: &'a str,
    ) -> LocalBoxFuture<'a, ConnectionOutcome>;
    /// 富搜索。
    ///
    /// 回调按需调用；返回选中的模型 id。用户返回或页面被替换时返回 `None`；调用方
    /// （`llama/index.rs` 的 `download_model`）还会把空字符串按取消处理。输入框里符合
    /// `owner/repository[:quant]` 的文本可以不选结果直接确认。
    fn search_models<'a>(&'a self, search: SearchModelsFn) -> LocalBoxFuture<'a, Option<String>>;
    /// 状态页（`showStatus`）。
    ///
    /// 替换当前页面并清空进度状态；这类页面没有输入焦点。
    fn show_status(&self, title: &str, message: &str);
    /// 进度页：返回的 future 在用户按下取消键时完成。
    ///
    /// 实现必须让返回的 future 在用户操作前保持 `Pending`：`run_with_progress` 的竞态
    /// 循环每轮都会重新调用它。`LlamaView` 会复用已有的 promise，并在每次调用时立即按
    /// `state` 重绘。
    fn progress<'a>(&'a self, state: &'a ProgressState) -> LocalBoxFuture<'a, ()>;
    /// 更新进度（已显示进度页时生效）。
    ///
    /// 未显示进度页时是 no-op；只同步重绘，不改变 `progress` 的 promise。
    fn update_progress(&self, state: &ProgressState);
}

// ---------------------------------------------------------------------------
// 纯格式化函数（TS 模块级函数）
// ---------------------------------------------------------------------------

/// `contextLabel(model)`。
///
/// 两阶段，先看模型元数据再看启动参数：
/// 1. `meta.n_ctx`，缺失时退到 `meta.n_ctx_train`；值必须非 0 且非 `NaN` 才格式化
///    （负数会原样格式化）。
/// 2. 否则扫描 `status.args`：从下标 0 开始，跳过不是 `--ctx-size`/`-c`/`-ctx` 的项，
///    把标志**后一个**参数按 JS `Number()` 转成数字，要求有限且 `> 0`。
///
/// 循环条件是 `index < length - 1`（f64），所以最后一个参数永远不会被当作标志；
/// `length` 为 `NaN`（args 缺失/类型不对）时条件为假，循环直接跳过。
fn context_label(model: &LlamaModelInfo) -> Option<String> {
    let meta = model.meta.as_ref();
    let context = meta
        .and_then(|meta| meta.n_ctx)
        .or_else(|| meta.and_then(|meta| meta.n_ctx_train));
    if let Some(context) = context
        && context != 0.0
        && !context.is_nan()
    {
        return Some(format_context(context));
    }
    // TS: `const args = model.status.args ?? []; index < args.length - 1`——非数组也按
    // `.length`/下标（JS 语义）读取，见 `LlamaModelStatusInfo::args_length`/`arg_at`。
    let length = model.status.args_length();
    let mut index = 0usize;
    while (index as f64) < length - 1.0 {
        let flag = model.status.arg_at(index);
        if !matches!(flag.as_str(), Some("--ctx-size" | "-c" | "-ctx")) {
            index += 1;
            continue;
        }
        let value = js_number(&model.status.arg_at(index + 1));
        if value.is_finite() && value > 0.0 {
            return Some(format_context(value));
        }
        index += 1;
    }
    None
}

/// `context >= 1000 ? Math.round(context / 1000) + "k" : String(context)`。
///
/// 数字统一用 `to_radix_string(_, 10)`（JS `String(number)` 的十进制形式），不是 JSON
/// 序列化，因此不会出现 `null`。
fn format_context(context: f64) -> String {
    if context >= 1000.0 {
        format!(
            "{}k",
            kiana_json::js_num::to_radix_string(js_round(context / 1000.0), 10)
        )
    } else {
        kiana_json::js_num::to_radix_string(context, 10)
    }
}

/// `Math.round(value)`（半数向 +∞：`Math.round(-0.5) === -0`、`Math.round(0.5) === 1`；
/// 非有限值原样返回，`NaN` 保持 `NaN`）。
///
/// 实现细节：负数半格用 `(-0.5..0.0)` 特判成 `-0.0`。区间**包含** `-0.5`、排除 `0.0`
/// 与其后的正数；若不特判，`(value + 0.5).floor()` 会得到 `+0.0`，与 JS 的符号不一致。
/// 其余值统一走 `floor(value + 0.5)`，因此正负半数都向 +∞ 取整。
fn js_round(value: f64) -> f64 {
    if !value.is_finite() {
        return value;
    }
    if (-0.5..0.0).contains(&value) {
        return -0.0;
    }
    (value + 0.5).floor()
}

/// `Math.min(left, right)`（任一为 `NaN` 时结果为 `NaN`）。
///
/// Rust 的 `f64::min` 会忽略 `NaN`，必须显式复刻 JS 的传播行为；进度条的夹取依赖它。
fn js_math_min(left: f64, right: f64) -> f64 {
    if left.is_nan() || right.is_nan() {
        f64::NAN
    } else if left < right {
        left
    } else {
        right
    }
}

/// `Math.max(left, right)`（任一为 `NaN` 时结果为 `NaN`）。
///
/// 与 `f64::max` 的差别同 [`js_math_min`]：Rust 会忽略 `NaN`。
fn js_math_max(left: f64, right: f64) -> f64 {
    if left.is_nan() || right.is_nan() {
        f64::NAN
    } else if left > right {
        left
    } else {
        right
    }
}

/// `String.prototype.repeat(count)` 的次数：`NaN`/非正数 → 0。
///
/// 有限的非整数按截断取整（`as usize`），与 JS 的 ToIntegerOrInfinity 一致。
fn js_repeat_count(value: f64) -> usize {
    if !value.is_finite() || value <= 0.0 {
        0
    } else {
        value as usize
    }
}

/// `modelDescription(model)`。
///
/// 只显示“状态”与“上下文长度”两段，用 `" · "` 连接：`loaded` 与 `sleeping` 都显示成
/// `"loaded"`（sleeping 是已加载但休眠）；`unloaded` 不显示状态段；其它未知状态原样
/// 显示（服务端新增状态不会被吞掉）。context 只在已加载时追加。
fn model_description(model: &LlamaModelInfo) -> String {
    let mut details: Vec<String> = Vec::new();
    let loaded = model.status.value == "loaded" || model.status.value == "sleeping";
    if loaded {
        details.push("loaded".to_string());
    } else if model.status.value != "unloaded" {
        details.push(model.status.value.clone());
    }
    if loaded && let Some(context) = context_label(model) {
        details.push(format!("{context} context"));
    }
    details.join(" · ")
}

/// `compactCount(value)`。
///
/// 阈值：`>= 1e6` 用 `M`（`>= 1e7` 保留 0 位小数，否则 1 位）；`>= 1e3` 用 `k`
/// （`>= 1e5` 保留 0 位，否则 1 位）；更小直接 `String(value)`。负值与 `NaN` 走最后
/// 一支；`Infinity` 命中 `M` 分支，得到 `"InfinityM"`（测试锁定）。
fn compact_count(value: f64) -> String {
    if value >= 1_000_000.0 {
        return format!(
            "{}M",
            kiana_ai::model_catalog::js_to_fixed(
                value / 1_000_000.0,
                if value >= 10_000_000.0 { 0 } else { 1 }
            )
        );
    }
    if value >= 1_000.0 {
        return format!(
            "{}k",
            kiana_ai::model_catalog::js_to_fixed(
                value / 1_000.0,
                if value >= 100_000.0 { 0 } else { 1 }
            )
        );
    }
    // `String(value)`：非有限值是 "NaN"/"Infinity"/"-Infinity"（不是 JSON 的 "null"）。
    kiana_json::js_num::to_radix_string(value, 10)
}

/// `selectTheme(theme)`（TS 的局部映射，scrollInfo 用 dim、noMatch 用 warning）。
///
/// 每个样式函数捕获自己的 `ThemeSnapshot` 克隆：`SelectListTheme` 的字段是
/// `Send + Sync + 'static` 的 `Arc` 闭包，不能借用宿主临时给出的 `&Theme`。
fn select_theme(theme: &ThemeSnapshot) -> SelectListTheme {
    let style = |color: ThemeColor| -> kiana_tui::components::text_component::StyleFn {
        let theme = theme.clone();
        Arc::new(move |text: &str| theme.fg(color, text))
    };
    SelectListTheme {
        selected_prefix: style(ThemeColor::Accent),
        selected_text: style(ThemeColor::Accent),
        description: style(ThemeColor::Muted),
        scroll_info: style(ThemeColor::Dim),
        no_match: style(ThemeColor::Warning),
    }
}

/// `frame(theme, title, body, footer?)`。
///
/// 垂直顺序：accent 上边框 → 加粗 accent 标题（左右各 1 列留白）→ `body` → 可选的
/// `Spacer` + dim 页脚 → accent 下边框。边框与文字都用传入的主题快照，而不是全局主题。
fn frame(
    theme: &ThemeSnapshot,
    title: &str,
    body: Vec<Box<dyn Component>>,
    footer: Option<&str>,
) -> Container {
    let mut container = Container::new();
    let border_theme = theme.clone();
    container.add_child(Box::new(DynamicBorder::from_fn(move |text: &str| {
        border_theme.fg(ThemeColor::Accent, text)
    })));
    container.add_child(Box::new(Text::with_padding(
        theme.fg(ThemeColor::Accent, &theme.bold(title)),
        1,
        0,
    )));
    for child in body {
        container.add_child(child);
    }
    if let Some(footer) = footer {
        container.add_child(Box::new(Spacer::new()));
        container.add_child(Box::new(Text::with_padding(
            theme.fg(ThemeColor::Dim, footer),
            1,
            0,
        )));
    }
    let bottom_theme = theme.clone();
    container.add_child(Box::new(DynamicBorder::from_fn(move |text: &str| {
        bottom_theme.fg(ThemeColor::Accent, text)
    })));
    container
}

/// 请求重绘（`tui.requestRender()`）：`Arc<dyn Tui>` 的成员是 `&mut self`，而工厂只给
/// `Arc<dyn Tui>`。运行时是单线程 UI 线程（见 `kiana-extensions` 的同名处理），这里短暂取一次
/// 可变引用；`Weak` 避免组件与 TUI 形成 `Arc` 环。
///
/// 返回的 `Rc<dyn Fn()>` 是 `!Send`，视图也只保存在单线程 UI 里，因此这个裸指针不会被带到
/// 别的线程；调用期间局部的 `tui` 持有强引用，指针必然有效。`request_render(false)` 在
/// `kiana-tui` 的实现里只置脏标记，不会同步重入组件。
fn render_requester_for_tui(tui: Arc<dyn Tui>) -> Rc<dyn Fn()> {
    let weak = Arc::downgrade(&tui);
    Rc::new(move || {
        let Some(tui) = weak.upgrade() else {
            return;
        };
        let pointer = Arc::as_ptr(&tui) as *mut dyn Tui;
        // SAFETY: 仅在单线程 UI 线程上调用；`request_render` 只置脏标记，不重入本组件。
        unsafe {
            (*pointer).request_render(false);
        }
    })
}

// ---------------------------------------------------------------------------
// 共享输入句柄（TS 的 inputHandler/inputTarget 组合）
// ---------------------------------------------------------------------------

/// 选择列表或富搜索的共享句柄。
///
/// 同一个 `Rc<RefCell<_>>` 既装在 `Container` 里渲染（`SelectComponent`/
/// `SearchComponent`），又保存在视图状态里接收键盘，因此两条路径看到同一份选择下标与
/// 输入内容。
///
/// `set_focused` 只对富搜索生效：`SelectList` 没有焦点概念（选中行由它自己渲染），只有
/// 内部的 `Input` 实现 `Focusable`。
#[derive(Clone)]
enum InputHandle {
    Select(Rc<RefCell<SelectList>>),
    Search(Rc<RefCell<HuggingFaceSearch>>),
}

impl InputHandle {
    fn handle_input(&self, data: &str) {
        match self {
            InputHandle::Select(list) => list.borrow_mut().handle_input(data),
            InputHandle::Search(search) => HuggingFaceSearch::handle_input(search, data),
        }
    }

    fn set_focused(&self, focused: bool) {
        match self {
            InputHandle::Select(_) => {}
            InputHandle::Search(search) => {
                Focusable::set_focused(&mut search.borrow_mut().input, focused);
            }
        }
    }
}

/// 把共享的 `SelectList` 装进 `Container`。
///
/// 用共享句柄而不是值：视图状态还要通过 [`InputHandle`] 把按键送到同一个实例。
struct SelectComponent(Rc<RefCell<SelectList>>);

impl Component for SelectComponent {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.0.borrow_mut().render(width)
    }
    fn handle_input(&mut self, data: &str) {
        self.0.borrow_mut().handle_input(data);
    }
    fn invalidate(&mut self) {
        self.0.borrow_mut().invalidate();
    }
}

/// 把共享的 `HuggingFaceSearch` 装进 `Container`。
struct SearchComponent(Rc<RefCell<HuggingFaceSearch>>);

impl Component for SearchComponent {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.0.borrow_mut().render_lines(width)
    }
    fn handle_input(&mut self, data: &str) {
        HuggingFaceSearch::handle_input(&self.0, data);
    }
    /// 无缓存可失效：提示行与 `Spacer` 每次渲染都新建，`Input` 直接渲染自身，结果容器
    /// 在 `rebuild_results` 里整体清空重建。
    fn invalidate(&mut self) {}
}

/// 发送一次结果（`oneshot::Sender` 只能消费一次）。
///
/// `take()` 让重复调用（例如选择回调之后又触发取消）变成 no-op；接收端可能已经被替换或
/// 丢弃，`send` 的错误被忽略——所有调用点的 future 都把 `Err` 当作取消。
fn send_once<T>(slot: &Rc<RefCell<Option<oneshot::Sender<T>>>>, value: T) {
    if let Some(sender) = slot.borrow_mut().take() {
        let _ = sender.send(value);
    }
}

// ---------------------------------------------------------------------------
// HuggingFaceSearch
// ---------------------------------------------------------------------------

/// 搜索确认输入的形状：`owner/repository[:quant]`
/// （TS `/^[^/\s]+\/[^:\s]+(?::[^\s:]+)?$/u` 的逐字翻译）。
///
/// 用途：输入框文本（已 trim）整体符合这个形状时，回车直接把它当作模型 id 返回，
/// 不必等搜索结果。两端锚定，`:` 之后部分可选且不能再含冒号。
///
/// JS 的 `\s` 与 Rust regex 的 `\s` 不同：前者包含 `U+FEFF`、不包含 `U+0085`（见
/// [`super::client::is_js_whitespace`]），因此显式列出 JS 的空格集合。
static EXACT_MODEL_PATTERN: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    const JS_WHITESPACE: &str = concat!(
        r"\t\n\x0B\f\r \x{00A0}\x{1680}",
        r"\x{2000}-\x{200A}\x{2028}\x{2029}\x{202F}\x{205F}\x{3000}\x{FEFF}"
    );
    regex::Regex::new(&format!(
        r"^[^/{JS_WHITESPACE}]+/[^:{JS_WHITESPACE}]+(?::[^{JS_WHITESPACE}:]+)?$"
    ))
    .expect("valid search pattern")
});

/// `class HuggingFaceSearch`（Container + Focusable）。
///
/// 一次“下载模型”搜索页的全部状态。TS 里它继承 `Container`；Rust 里它自己不是
/// `Container`，而由 `SearchComponent` 在渲染时调用 `render_lines` 拼出
/// 提示行 + 输入框 + 结果列表，内部只用 `results_container` 保存结果行。
///
/// 防抖与竞态用两个单调计数：
/// - `debounce_generation`：查询变化/关闭时 +1，延迟 500 ms 的任务用它判断自己是否过期；
/// - `request_generation`：标记当前在飞的请求，`run_search` 结束时只清除属于自己那一代
///   的 `request`，不会把新请求的句柄清掉。
///
/// `cache` 来自视图的 `ViewState.search_cache`，跨多次打开搜索页共享，键是查询的小写形式。
struct HuggingFaceSearch {
    render: Rc<dyn Fn()>,
    theme: ThemeSnapshot,
    keybindings: Arc<KeybindingsManager>,
    search: SearchModelsFn,
    /// 查询（小写）→ 完整结果；只缓存成功结果。
    cache: Rc<RefCell<HashMap<String, Vec<HuggingFaceModel>>>>,
    /// `close` 时取走并调用一次（`FnOnce`）；调用后为 `None`。
    on_select_model: Option<Box<dyn FnOnce(Option<String>)>>,
    input: Input,
    results_container: Container,
    results: Vec<HuggingFaceModel>,
    filtered_results: Vec<HuggingFaceModel>,
    selected_index: usize,
    query: String,
    /// 结果区下方的提示。`rebuild_results` 会把 `"Searching Hugging Face…"` 与结果为空
    /// 的情况一起判断是否显示状态行；这些字面量由 `schedule_search`/`run_search` 写入，
    /// 改动时要成对修改。
    status: String,
    /// 防抖代次：每次查询变化/关闭 +1。
    debounce_generation: u64,
    /// 在飞请求的代次，用于 `finally` 式清理。
    request_generation: u64,
    /// 在飞请求的取消句柄；新查询或关闭时 abort。
    request: Option<kiana_ai::utils::abort::AbortScope>,
    /// `close` 幂等标记：置位后过期结果不再落到界面。
    closed: bool,
}

type SearchRc = Rc<RefCell<HuggingFaceSearch>>;

impl HuggingFaceSearch {
    /// 构造并立刻渲染一次（`updateResults()`）；`status` 初值提示用户至少输入 2 个字符。
    fn new(
        render: Rc<dyn Fn()>,
        theme: ThemeSnapshot,
        keybindings: Arc<KeybindingsManager>,
        search: SearchModelsFn,
        cache: Rc<RefCell<HashMap<String, Vec<HuggingFaceModel>>>>,
        on_select_model: Box<dyn FnOnce(Option<String>)>,
    ) -> SearchRc {
        let this = Rc::new(RefCell::new(HuggingFaceSearch {
            render,
            theme,
            keybindings,
            search,
            cache,
            on_select_model: Some(on_select_model),
            input: Input::new(InputOptions::default()),
            results_container: Container::new(),
            results: Vec::new(),
            filtered_results: Vec::new(),
            selected_index: 0,
            query: String::new(),
            status: "Type at least 2 characters".to_string(),
            debounce_generation: 0,
            request_generation: 0,
            request: None,
            closed: false,
        }));
        Self::update_results(&this);
        this
    }

    /// `updateResults()`：重建结果区并请求重绘。
    ///
    /// 先在 `RefCell` 借用内完成重建，再在借用外调用渲染回调：渲染回调由宿主提供，
    /// 不让 `RefCell` 借用跨越这次调用。
    fn update_results(this: &SearchRc) {
        let render = {
            let mut search = this.borrow_mut();
            search.rebuild_results();
            Rc::clone(&search.render)
        };
        render();
    }

    /// 重建可见结果区（`updateResults` 的渲染部分）。
    ///
    /// 视口 10 行，起点为 `max(0, min(selected_index - 5, len - 10))`：选中项尽量居中，
    /// 靠近两端时贴边；结果少于 10 条时 `len - 10` 为负，起点回到 0。窗口没有覆盖全部
    /// 结果时追加 `(当前/总数)` 行。结果为空、或状态是 `"Searching Hugging Face…"` 时再
    /// 显示状态行（状态字符串由 `schedule_search`/`run_search` 写入，改动要成对进行）。
    ///
    /// 选中行整行用 accent，其余行只有右侧的 `downloads` 详情用 muted；行首前缀分别是
    /// `"→ "` 与 `"  "`（两个字符宽）。
    fn rebuild_results(&mut self) {
        self.results_container.clear();
        let max_visible = 10usize;
        let length = self.filtered_results.len();
        let start = std::cmp::max(
            0isize,
            std::cmp::min(
                self.selected_index as isize - (max_visible as isize / 2),
                length as isize - max_visible as isize,
            ),
        ) as usize;
        let end = std::cmp::min(start + max_visible, length);
        for index in start..end {
            let model = &self.filtered_results[index];
            let prefix = if index == self.selected_index {
                "→ "
            } else {
                "  "
            };
            let details = format!("{} downloads", compact_count(model.downloads));
            let line = if index == self.selected_index {
                self.theme.fg(
                    ThemeColor::Accent,
                    &format!("{prefix}{}  {details}", model.id),
                )
            } else {
                format!(
                    "{prefix}{}{}",
                    model.id,
                    self.theme.fg(ThemeColor::Muted, &format!("  {details}"))
                )
            };
            self.results_container
                .add_child(Box::new(Text::with_padding(line, 0, 0)));
        }
        if start > 0 || end < length {
            self.results_container
                .add_child(Box::new(Text::with_padding(
                    self.theme.fg(
                        ThemeColor::Dim,
                        &format!("  ({}/{})", self.selected_index + 1, length),
                    ),
                    0,
                    0,
                )));
        }
        if length == 0 || self.status == "Searching Hugging Face…" {
            self.results_container
                .add_child(Box::new(Text::with_padding(
                    self.theme
                        .fg(ThemeColor::Dim, &format!("  {}", self.status)),
                    0,
                    0,
                )));
        }
    }

    /// `filterResults()`：按当前查询过滤并夹取选中下标。
    ///
    /// 先让 [`fuzzy_filter`] 对 `results` 打分排序，取出命中的 id 集合，再**按原顺序**
    /// 过滤 `results`：界面顺序保持 Hugging Face 返回的顺序，而不是模糊分数顺序（TS 用
    /// Set + filter，语义相同）。query 为空时全部保留。过滤后把 `selected_index` 夹到
    /// `len - 1`（空列表为 0），再重绘。
    fn filter_results(this: &SearchRc) {
        {
            let mut search = this.borrow_mut();
            if !search.query.is_empty() {
                let matches: HashSet<String> =
                    fuzzy_filter(search.results.clone(), &search.query, |model| {
                        model.id.clone()
                    })
                    .into_iter()
                    .map(|model| model.id)
                    .collect();
                search.filtered_results = search
                    .results
                    .iter()
                    .filter(|model| matches.contains(&model.id))
                    .cloned()
                    .collect();
            } else {
                search.filtered_results = search.results.clone();
            }
            let max_index = search.filtered_results.len().saturating_sub(1);
            if search.selected_index > max_index {
                search.selected_index = max_index;
            }
        }
        Self::update_results(this);
    }

    /// `scheduleSearch()`：500 ms 防抖 + 缓存。
    ///
    /// 每次调用先 +1 `debounce_generation` 并中止在飞请求，因此旧查询的延迟任务醒来后
    /// 一定会被 `run_search` 判为过期。查询长度按 **UTF-16 码元**计
    /// （`encode_utf16().count()`，对应 JS `query.length`），少于 2 个码元只更新提示、
    /// 不发请求。缓存键是查询的 `to_lowercase()`；命中时立即替换 `results` 并按是否为空
    /// 设置状态，未命中则把状态设为 `"Searching Hugging Face…"`，把
    /// `(generation, query)` 交给 `spawn_local` 延迟 500 ms。两条路径都先
    /// `filter_results`（未命中时先显示“搜索中”）。
    fn schedule_search(this: &SearchRc) {
        let action = {
            let mut search = this.borrow_mut();
            // 先中止旧请求、再启动新查询：旧查询的延迟任务醒来后在 `run_search` 入口因
            // `debounce_generation` 不符直接返回；已在飞行的请求则由 `request.is_aborted()`
            // 与 `search.query` 比对判为过期。
            search.debounce_generation += 1;
            let generation = search.debounce_generation;
            if let Some(request) = search.request.take() {
                request.abort();
            }
            if search.query.encode_utf16().count() < 2 {
                search.status = "Type at least 2 characters".to_string();
                None
            } else {
                let key = search.query.to_lowercase();
                let cached = search.cache.borrow().get(&key).cloned();
                match cached {
                    Some(cached) => {
                        search.results = cached.clone();
                        search.status = if cached.is_empty() {
                            "No GGUF models found".to_string()
                        } else {
                            String::new()
                        };
                        None
                    }
                    None => {
                        search.status = "Searching Hugging Face…".to_string();
                        Some((generation, search.query.clone()))
                    }
                }
            }
        };
        match action {
            None => Self::filter_results(this),
            Some((generation, query)) => {
                Self::filter_results(this);
                let this = Rc::clone(this);
                tokio::task::spawn_local(async move {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    Self::run_search(this, query, generation).await;
                });
            }
        }
    }

    /// `runSearch(query)`。
    ///
    /// 阶段：
    /// 1. 建新的 `AbortScope`，在借用内再次核对 `closed`/代次/query（500 ms 防抖期间
    ///    用户可能已改词或关闭）；通过后登记为当前 `request`。
    /// 2. 克隆搜索回调（不跨 `await` 持有 `RefCell` 借用）并等待结果。
    /// 3. 成功结果**无论是否过期都写入缓存**：缓存键是本次查询，内容对同一查询始终有效；
    ///    失败不写。
    /// 4. 若 `closed`、请求已 abort 或当前 query 已变，丢弃结果、不重绘。
    /// 5. 否则替换 `results`、选中项归零、按空/非空设置状态，并在借用外重绘。
    /// 6. 最后只在自己仍是当前请求时清空 `request`。
    ///
    /// 失败信息直接放进 `status`（`AiError.message`），不向调用方传播。
    async fn run_search(this: SearchRc, query: String, generation: u64) {
        let request = kiana_ai::utils::abort::AbortScope::new();
        {
            let mut search = this.borrow_mut();
            if search.closed || search.debounce_generation != generation || search.query != query {
                return;
            }
            search.request = Some(request.clone());
            search.request_generation = generation;
        }
        let search_fn = this.borrow().search.clone();
        let outcome = search_fn(query.clone(), request.clone()).await;
        let mut refresh = false;
        {
            let mut search = this.borrow_mut();
            // 缓存写入与“是否过期”无关：结果属于本次 query，缓存键也用它；即使页面已
            // 关闭或已改词，这份数据仍可被下次相同查询复用。
            if let Ok(results) = &outcome {
                search
                    .cache
                    .borrow_mut()
                    .insert(query.to_lowercase(), results.clone());
            }
            let stale = search.closed || request.is_aborted() || search.query != query;
            if !stale {
                match outcome {
                    Ok(results) => {
                        search.results = results;
                        search.selected_index = 0;
                        search.status = if search.results.is_empty() {
                            "No GGUF models found".to_string()
                        } else {
                            String::new()
                        };
                    }
                    Err(error) => {
                        search.results = Vec::new();
                        search.status = error.message;
                    }
                }
                refresh = true;
            }
            if search.request_generation == generation {
                search.request = None;
            }
        }
        if refresh {
            Self::filter_results(&this);
        }
    }

    /// `close(model)`（幂等）。
    ///
    /// 第一次调用：置 `closed`、使防抖代次失效、中止在飞请求并取走 `on_select_model`
    /// （`FnOnce` 只能调用一次）。回调在借用外执行，它发送 `oneshot` 结果；重复调用直接
    /// 返回。
    fn close(this: &SearchRc, model: Option<String>) {
        let callback = {
            let mut search = this.borrow_mut();
            if search.closed {
                return;
            }
            search.closed = true;
            search.debounce_generation += 1;
            if let Some(request) = search.request.take() {
                request.abort();
            }
            search.on_select_model.take()
        };
        if let Some(callback) = callback {
            callback(model);
        }
    }

    /// `handleInput(data)`。
    ///
    /// 上/下键在过滤结果里循环；确认键优先使用“能解析成 `owner/repo[:quant]` 的原始输入”，
    /// 否则取当前选中项；取消键以 `None` 关闭。空字符串 id 视为假值、不关闭（TS
    /// `if (selected)`）。其余按键交给内部 `Input`，每次输入后对值做 JS `trim`，只有
    /// 查询变化才重新调度搜索。
    ///
    /// 借用注意：确认/取消分支先 `drop(search)` 再调用 `close`，因为 `close` 会再次借用
    /// 同一个 `RefCell`。
    fn handle_input(this: &SearchRc, data: &str) {
        enum Next {
            Nothing,
            UpdateResults,
            ScheduleSearch,
        }
        let next = {
            let mut search = this.borrow_mut();
            if search.keybindings.matches(data, "tui.select.up") {
                if search.filtered_results.is_empty() {
                    Next::Nothing
                } else {
                    search.selected_index = if search.selected_index == 0 {
                        search.filtered_results.len() - 1
                    } else {
                        search.selected_index - 1
                    };
                    Next::UpdateResults
                }
            } else if search.keybindings.matches(data, "tui.select.down") {
                if search.filtered_results.is_empty() {
                    Next::Nothing
                } else {
                    search.selected_index =
                        if search.selected_index == search.filtered_results.len() - 1 {
                            0
                        } else {
                            search.selected_index + 1
                        };
                    Next::UpdateResults
                }
            } else if search.keybindings.matches(data, "tui.select.confirm") {
                let exact = if EXACT_MODEL_PATTERN.is_match(&search.query) {
                    Some(search.query.clone())
                } else {
                    None
                };
                let selected = exact.or_else(|| {
                    search
                        .filtered_results
                        .get(search.selected_index)
                        .map(|model| model.id.clone())
                });
                drop(search);
                // TS: `if (selected) this.close(selected)`——空 id 是假值，不关闭。
                if let Some(selected) = selected.filter(|selected| !selected.is_empty()) {
                    Self::close(this, Some(selected));
                }
                return;
            } else if search.keybindings.matches(data, "tui.select.cancel") {
                drop(search);
                Self::close(this, None);
                return;
            } else {
                search.input.handle_input(data);
                let query = kiana_json::js_string::js_trim(search.input.get_value()).to_string();
                if query == search.query {
                    Next::Nothing
                } else {
                    search.query = query;
                    Next::ScheduleSearch
                }
            }
        };
        match next {
            Next::Nothing => {}
            Next::UpdateResults => Self::update_results(this),
            Next::ScheduleSearch => Self::schedule_search(this),
        }
    }

    /// 组件渲染（TS 继承 `Container` 的 children）。
    ///
    /// 顺序：dim 提示行 → 输入框 → `Spacer` → 结果容器。外框与标题由 `frame` 负责。
    fn render_lines(&mut self, width: usize) -> Vec<String> {
        let mut lines = Vec::new();
        lines.extend(
            Text::with_padding(
                self.theme
                    .fg(ThemeColor::Dim, "Model name or owner/repository[:quant]"),
                1,
                0,
            )
            .render(width),
        );
        lines.extend(self.input.render(width));
        lines.extend(Spacer::new().render(width));
        lines.extend(self.results_container.render(width));
        lines
    }
}

// ---------------------------------------------------------------------------
// LlamaView
// ---------------------------------------------------------------------------

/// 视图状态（TS 的实例字段）。
///
/// `progress_promise`/`progress_resolver` 是一对：`progress()` 懒创建它们，把 receiver 的
/// `Shared` 交给调用方、sender 留在这里；用户按取消或 `set_content` 清状态时 sender 被
/// 取出/丢弃，所有 `Shared` 克隆随之完成（`progress()` 的等待方用 `let _ =` 忽略结果）。
struct ViewState {
    search_cache: Rc<RefCell<HashMap<String, Vec<HuggingFaceModel>>>>,
    content: Container,
    input_handler: Option<InputHandle>,
    input_target: Option<InputHandle>,
    progress_promise: Option<Shared<oneshot::Receiver<()>>>,
    progress_resolver: Option<oneshot::Sender<()>>,
    showing_progress: bool,
    focused: bool,
}

/// `class LlamaView`（LlamaUi + Focusable）。
///
/// 负责页面切换与键盘分发：`content` 是当前页面，`input_handler` 接收按键，
/// `input_target` 是需要焦点的子组件（只有富搜索的输入框）。`handle_input` 先处理进度页
/// 的取消键，其余按键转发给 `input_handler`，每次处理后都请求重绘。
struct LlamaView {
    render: Rc<dyn Fn()>,
    theme: ThemeSnapshot,
    keybindings: Arc<KeybindingsManager>,
    state: RefCell<ViewState>,
}

impl LlamaView {
    /// 构造视图（初始内容为 `Loading…` 状态页）。
    fn new(
        render: Rc<dyn Fn()>,
        theme: ThemeSnapshot,
        keybindings: Arc<KeybindingsManager>,
    ) -> Self {
        let content = frame(
            &theme,
            "llama.cpp models",
            vec![Box::new(Text::with_padding(
                theme.fg(ThemeColor::Muted, "Loading…"),
                1,
                1,
            ))],
            None,
        );
        LlamaView {
            render,
            theme,
            keybindings,
            state: RefCell::new(ViewState {
                search_cache: Rc::new(RefCell::new(HashMap::new())),
                content,
                input_handler: None,
                input_target: None,
                progress_promise: None,
                progress_resolver: None,
                showing_progress: false,
                focused: false,
            }),
        }
    }

    /// `setContent(content, inputHandler?, inputTarget?)`。
    ///
    /// 处理顺序：先取消旧目标的焦点 → 清空进度 promise/resolver（在飞的进度页
    /// 因此完成）→ 换内容与处理器 → 按视图当前焦点状态给新目标设焦点 → 请求重绘。
    /// 清进度状态与换内容在同一段 `borrow_mut` 临界区内完成，单线程下不会与
    /// `handle_input` 交错；`progress_resolver` 被清空并 drop，旧 promise 随之完成。
    fn set_content(
        &self,
        content: Container,
        input_handler: Option<InputHandle>,
        input_target: Option<InputHandle>,
    ) {
        {
            let mut state = self.state.borrow_mut();
            if let Some(target) = &state.input_target {
                target.set_focused(false);
            }
            state.progress_promise = None;
            state.progress_resolver = None;
            state.showing_progress = false;
            state.content = content;
            state.input_handler = input_handler;
            state.input_target = input_target;
            if let Some(target) = &state.input_target {
                target.set_focused(state.focused);
            }
        }
        (self.render)();
    }

    /// `handleInput(data)`。
    ///
    /// 进度页优先：存在 `progress_resolver` 且按下取消键时，取走 sender、清空 promise
    /// 并 `send(())`，不再把按键传给下层（进度页没有输入控件）。其余情况转发给
    /// `input_handler`（可能为 `None`），然后统一请求重绘。
    fn handle_input(&self, data: &str) {
        {
            let mut state = self.state.borrow_mut();
            if state.progress_resolver.is_some()
                && self.keybindings.matches(data, "tui.select.cancel")
            {
                let resolver = state.progress_resolver.take();
                state.progress_promise = None;
                drop(state);
                if let Some(resolver) = resolver {
                    let _ = resolver.send(());
                }
                return;
            }
        }
        let handler = self.state.borrow().input_handler.as_ref().cloned();
        if let Some(handler) = handler {
            handler.handle_input(data);
        }
        (self.render)();
    }

    /// `render(width)`：超宽行按可见宽度截断。
    ///
    /// `visible_width` 忽略 ANSI 序列；只有真的超宽才调用 `truncate_to_width`，不改变
    /// 未超宽行的字节。
    fn render_view(&self, width: usize) -> Vec<String> {
        let mut lines = self.state.borrow_mut().content.render(width);
        for line in &mut lines {
            if visible_width(line) > width {
                *line = truncate_to_width(line, width, "", false);
            }
        }
        lines
    }

    fn invalidate_view(&self) {
        self.state.borrow_mut().content.invalidate();
    }

    fn is_focused(&self) -> bool {
        self.state.borrow().focused
    }

    fn set_view_focused(&self, focused: bool) {
        let mut state = self.state.borrow_mut();
        state.focused = focused;
        if let Some(target) = &state.input_target {
            target.set_focused(focused);
        }
    }
}

impl LlamaUi for LlamaView {
    fn show_models<'a>(
        &'a self,
        server_url: &'a str,
        models: &'a [LlamaModelInfo],
    ) -> LocalBoxFuture<'a, LlamaManagerAction> {
        // 先做三件事：排好序的副本、id→模型 映射、列表项（全是值拷贝，后续选择不借用入参）。
        let mut sorted: Vec<LlamaModelInfo> = models.to_vec();
        // TS: `Number(right.status.value === "loaded") -
        // Number(left.status.value === "loaded") || localeCompare`。
        // 只有 `loaded` 参与优先排序；sleeping 与其它状态同组，按 id 的 localeCompare 排。
        sorted.sort_by(|left, right| {
            let loaded = i32::from(right.status.value == "loaded")
                - i32::from(left.status.value == "loaded");
            if loaded != 0 {
                return loaded.cmp(&0);
            }
            locale_compare(&left.id, &right.id)
        });
        // 选择回调只能拿到 SelectItem 的 value（模型 id），因此先建映射；重复 id 时后写的
        // 覆盖先写的（与 TS `new Map(sorted.map(...))` 一致）。
        let by_id: HashMap<String, LlamaModelInfo> = sorted
            .iter()
            .map(|model| (model.id.clone(), model.clone()))
            .collect();
        let mut items: Vec<SelectItem> = sorted
            .iter()
            .map(|model| SelectItem {
                value: model.id.clone(),
                label: model.id.clone(),
                description: Some(model_description(model)),
            })
            .collect();
        // 固定的下载项用 NUL 前缀的哨兵值区分；它总在最后，所以列表永不为空。
        items.push(SelectItem {
            value: DOWNLOAD_VALUE.to_string(),
            label: "Download model…".to_string(),
            description: Some("Hugging Face owner/repository[:quant]".to_string()),
        });
        // 视口高度 `min(items, 12)`（含下载项），与 spec §3.8 一致。
        let max_visible = items.len().min(12);
        let (sender, receiver) = oneshot::channel();
        let sender = Rc::new(RefCell::new(Some(sender)));
        let sender_for_select = Rc::clone(&sender);
        let by_id_for_select = by_id.clone();
        // on_select 与 on_cancel 共享同一个 sender，`send_once` 保证结果只会解析一次。
        let on_select = move |item: &SelectItem| {
            if item.value == DOWNLOAD_VALUE {
                send_once(&sender_for_select, LlamaManagerAction::Download);
            } else if let Some(model) = by_id_for_select.get(&item.value) {
                send_once(&sender_for_select, LlamaManagerAction::Model(model.clone()));
            }
        };
        let sender_for_cancel = Rc::clone(&sender);
        let on_cancel = move || send_once(&sender_for_cancel, LlamaManagerAction::Close);
        let footer = format!(
            "{} • {}",
            key_hint("tui.select.confirm", "load/unload/download"),
            key_hint("tui.select.cancel", "close")
        );
        let list = Rc::new(RefCell::new(SelectList::new(
            items,
            max_visible,
            select_theme(&self.theme),
            SelectListLayoutOptions {
                min_primary_column_width: Some(36.0),
                max_primary_column_width: Some(56.0),
                truncate_primary: None,
            },
        )));
        {
            let mut list_ref = list.borrow_mut();
            list_ref.on_select = Some(Box::new(on_select));
            list_ref.on_cancel = Some(Box::new(on_cancel));
        }
        let content = frame(
            &self.theme,
            "llama.cpp models",
            vec![
                Box::new(Text::with_padding(
                    self.theme.fg(ThemeColor::Dim, server_url),
                    1,
                    0,
                )),
                Box::new(Spacer::new()),
                Box::new(SelectComponent(Rc::clone(&list))),
            ],
            Some(&footer),
        );
        self.set_content(content, Some(InputHandle::Select(list)), None);
        // 页面被替换/视图释放时 sender 被 drop，接收端得到 `Err`，按取消（Close）处理。
        Box::pin(async move { receiver.await.unwrap_or(LlamaManagerAction::Close) })
    }

    /// 通用选择页：选项同时作为 value 与 label，没有描述列。
    ///
    /// 取消时 `on_cancel` 丢弃 sender，`receiver.await` 得到 `Err` → `None`。
    fn select<'a>(
        &'a self,
        title: &'a str,
        options: &'a [String],
    ) -> LocalBoxFuture<'a, Option<String>> {
        let items: Vec<SelectItem> = options
            .iter()
            .map(|option| SelectItem {
                value: option.clone(),
                label: option.clone(),
                description: None,
            })
            .collect();
        let max_visible = items.len().min(12);
        let (sender, receiver) = oneshot::channel::<String>();
        let sender = Rc::new(RefCell::new(Some(sender)));
        let sender_for_select = Rc::clone(&sender);
        let on_select = move |item: &SelectItem| {
            send_once(&sender_for_select, item.value.clone());
        };
        let sender_for_cancel = Rc::clone(&sender);
        let on_cancel = move || {
            // 取消时丢弃 sender：接收端得到 `Err` → `None`。
            sender_for_cancel.borrow_mut().take();
        };
        let list = Rc::new(RefCell::new(SelectList::new(
            items,
            max_visible,
            select_theme(&self.theme),
            SelectListLayoutOptions::default(),
        )));
        {
            let mut list_ref = list.borrow_mut();
            list_ref.on_select = Some(Box::new(on_select));
            list_ref.on_cancel = Some(Box::new(on_cancel));
        }
        let footer = format!(
            "{} • {}",
            key_hint("tui.select.confirm", "select"),
            key_hint("tui.select.cancel", "cancel")
        );
        let content = frame(
            &self.theme,
            title,
            vec![
                Box::new(Spacer::new()),
                Box::new(SelectComponent(Rc::clone(&list))),
            ],
            Some(&footer),
        );
        self.set_content(content, Some(InputHandle::Select(list)), None);
        Box::pin(async move { receiver.await.ok() })
    }

    fn confirm<'a>(&'a self, title: &'a str, message: &'a str) -> LocalBoxFuture<'a, bool> {
        // 复用通用选择页，把消息用换行拼进标题（TS 的模板字符串）。
        Box::pin(async move {
            let answer = self
                .select(
                    &format!("{title}\n{message}"),
                    &["Yes".to_string(), "No".to_string()],
                )
                .await;
            answer.as_deref() == Some("Yes")
        })
    }

    fn connection_error<'a>(
        &'a self,
        server_url: &'a str,
        message: &'a str,
    ) -> LocalBoxFuture<'a, ConnectionOutcome> {
        // 只有精确等于 "Retry" 才重试；取消（None）与其它值都按 Close 处理。
        Box::pin(async move {
            let choice = self
                .select(
                    &format!("llama.cpp unavailable\n{server_url}\n\n{message}"),
                    &["Retry".to_string(), "Close".to_string()],
                )
                .await;
            if choice.as_deref() == Some("Retry") {
                ConnectionOutcome::Retry
            } else {
                ConnectionOutcome::Close
            }
        })
    }

    /// 富搜索页：新建 `HuggingFaceSearch`，但复用视图里的搜索缓存。
    ///
    /// `inputHandler` 与 `inputTarget` 是同一个组件：键盘由 view 转发给它，焦点也设在它的
    /// `Input` 上（`InputHandle::set_focused` 只对 Search 生效）。
    fn search_models<'a>(&'a self, search: SearchModelsFn) -> LocalBoxFuture<'a, Option<String>> {
        let (sender, receiver) = oneshot::channel::<Option<String>>();
        let sender = Rc::new(RefCell::new(Some(sender)));
        let sender_for_select = Rc::clone(&sender);
        let component = HuggingFaceSearch::new(
            Rc::clone(&self.render),
            self.theme.clone(),
            Arc::clone(&self.keybindings),
            search,
            Rc::clone(&self.state.borrow().search_cache),
            Box::new(move |model| send_once(&sender_for_select, model)),
        );
        let footer = format!(
            "{} • {}",
            key_hint("tui.select.confirm", "select"),
            key_hint("tui.select.cancel", "back")
        );
        let content = frame(
            &self.theme,
            "Download model",
            vec![
                Box::new(Spacer::new()),
                Box::new(SearchComponent(Rc::clone(&component))),
            ],
            Some(&footer),
        );
        let handle = InputHandle::Search(Rc::clone(&component));
        self.set_content(content, Some(handle.clone()), Some(handle));
        // 回调发 `None`（用户返回）与 sender 被 drop（页面替换）都得到 `None`。
        Box::pin(async move { receiver.await.ok().flatten() })
    }

    fn show_status(&self, title: &str, message: &str) {
        let content = frame(
            &self.theme,
            title,
            vec![
                Box::new(Spacer::new()),
                Box::new(Text::with_padding(
                    self.theme.fg(ThemeColor::Muted, message),
                    1,
                    0,
                )),
            ],
            None,
        );
        self.set_content(content, None, None);
    }

    /// 进度页：懒创建一个 promise，重复调用共享同一份 receiver 的 `Shared` 克隆。
    ///
    /// `progress()` 在懒创建 promise 时把 `showing_progress` 置为 `true` 并调用
    /// `update_progress` 渲染；`update_progress` 只读取该标记，未显示进度页时直接返回。
    /// 用户按取消键由 `LlamaView::handle_input` 解析 promise。
    fn progress<'a>(&'a self, state: &'a ProgressState) -> LocalBoxFuture<'a, ()> {
        let promise = {
            let mut view = self.state.borrow_mut();
            if view.progress_promise.is_none() {
                let (sender, receiver) = oneshot::channel();
                view.progress_resolver = Some(sender);
                view.progress_promise = Some(receiver.shared());
            }
            view.showing_progress = true;
            view.progress_promise.clone()
        };
        self.update_progress(state);
        match promise {
            Some(promise) => Box::pin(async move {
                // sender 被 drop（set_content）时这个 future 也会立即结束，同样视为页面结束。
                let _ = promise.await;
            }),
            // 创建后 promise 必为 Some；这个分支只是防御。
            None => Box::pin(async {}),
        }
    }

    /// 重绘进度页（`updateProgress`）。
    ///
    /// 未显示进度页时直接返回。页面直接替换 `content`，不经过 `set_content`：因此不清空
    /// progress promise，只清空 `input_handler`——进度页只响应视图级的取消键。
    fn update_progress(&self, state: &ProgressState) {
        let render = {
            let mut view = self.state.borrow_mut();
            if !view.showing_progress {
                return;
            }
            let theme = &self.theme;
            let mut body: Vec<Box<dyn Component>> = vec![
                Box::new(Text::with_padding(
                    theme.fg(ThemeColor::Text, &state.model),
                    1,
                    0,
                )),
                Box::new(Spacer::new()),
                Box::new(Text::with_padding(
                    theme.fg(ThemeColor::Muted, &state.message),
                    1,
                    0,
                )),
            ];
            if let Some(ratio) = state.ratio {
                const AVAILABLE: usize = 40;
                // TS: `Math.round(Math.max(0, Math.min(1, ratio)) * available)`；JS 的
                // Math.min/Math.max 对 NaN 传播 NaN（Rust 的 f64::min/max 会忽略 NaN），
                // `String.repeat(NaN)` 是 0 次。
                let bounded = js_math_max(0.0, js_math_min(1.0, ratio));
                let filled = js_round(bounded * AVAILABLE as f64);
                // 百分比用**原始** ratio，未再夹取：ratio=2 显示 200% 但进度条已满
                // （40 格）。NaN 比例得到 "NaN%"、格子数为 0。
                let percent = js_round(ratio * 100.0);
                let filled_count = js_repeat_count(filled);
                let empty_count = js_repeat_count(AVAILABLE as f64 - filled);
                let bar = format!(
                    "{}{} {}%",
                    "█".repeat(filled_count),
                    "─".repeat(empty_count),
                    // `${Math.round(ratio * 100)}` 是 `String(number)`。
                    kiana_json::js_num::to_radix_string(percent, 10)
                );
                body.push(Box::new(Text::with_padding(
                    theme.fg(ThemeColor::Accent, &bar),
                    1,
                    0,
                )));
            }
            if let Some(detail) = &state.detail
                && !detail.is_empty()
            {
                body.push(Box::new(Text::with_padding(
                    theme.fg(ThemeColor::Dim, detail),
                    1,
                    0,
                )));
            }
            view.content = frame(
                theme,
                &state.title,
                body,
                Some(&key_hint("tui.select.cancel", "stop")),
            );
            // 进度页没有输入控件；只清 handler，不动 input_target（见方法文档）。
            view.input_handler = None;
            Rc::clone(&self.render)
        };
        render();
    }
}

/// `LlamaView` 的 `LlamaUi` 句柄（共享同一个视图）。
///
/// `Clone` 只克隆 `Rc`；trait 方法全部转发到底层视图，因此 `run` 闭包与宿主组件看到的
/// 是同一份页面状态。句柄是 `!Send`，只能在 UI 线程使用。
#[derive(Clone)]
pub struct LlamaViewHandle(Rc<LlamaView>);

impl LlamaUi for LlamaViewHandle {
    fn show_models<'a>(
        &'a self,
        server_url: &'a str,
        models: &'a [LlamaModelInfo],
    ) -> LocalBoxFuture<'a, LlamaManagerAction> {
        self.0.show_models(server_url, models)
    }
    fn select<'a>(
        &'a self,
        title: &'a str,
        options: &'a [String],
    ) -> LocalBoxFuture<'a, Option<String>> {
        self.0.select(title, options)
    }
    fn confirm<'a>(&'a self, title: &'a str, message: &'a str) -> LocalBoxFuture<'a, bool> {
        self.0.confirm(title, message)
    }
    fn connection_error<'a>(
        &'a self,
        server_url: &'a str,
        message: &'a str,
    ) -> LocalBoxFuture<'a, ConnectionOutcome> {
        self.0.connection_error(server_url, message)
    }
    fn search_models<'a>(&'a self, search: SearchModelsFn) -> LocalBoxFuture<'a, Option<String>> {
        self.0.search_models(search)
    }
    fn show_status(&self, title: &str, message: &str) {
        self.0.show_status(title, message);
    }
    fn progress<'a>(&'a self, state: &'a ProgressState) -> LocalBoxFuture<'a, ()> {
        self.0.progress(state)
    }
    fn update_progress(&self, state: &ProgressState) {
        self.0.update_progress(state);
    }
}

/// 视图组件（`Component + Focusable`）。
///
/// 宿主只认识 `Component`：渲染、输入与失效都转发给 `LlamaView`；`as_focusable` 指回
/// 自身，让 TUI 设置的焦点落到视图状态，再由 `input_target` 传给输入框。
struct LlamaViewComponent(Rc<LlamaView>);

impl Component for LlamaViewComponent {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.0.render_view(width)
    }
    fn handle_input(&mut self, data: &str) {
        self.0.handle_input(data);
    }
    fn invalidate(&mut self) {
        self.0.invalidate_view();
    }
    fn as_focusable(&mut self) -> Option<&mut dyn Focusable> {
        Some(self)
    }
}

impl Focusable for LlamaViewComponent {
    fn focused(&self) -> bool {
        self.0.is_focused()
    }
    fn set_focused(&mut self, focused: bool) {
        self.0.set_view_focused(focused);
    }
}

/// `showLlamaUi(ctx, run)`。
///
/// 工厂同步返回视图组件；`run` 在 `spawn_local`（单线程 UI 运行时）里执行，出错时
/// `ctx.ui.notify(error.message, "error")` 后 `done()`。
///
/// 参数与生命周期：
/// - `run` 是 `FnOnce + Send + 'static`，被放进 `Mutex<Option<_>>` 供 `Send + Sync` 的
///   工厂取走；工厂若被宿主再次调用，`run` 已是 `None`，此时直接 `done`。
/// - 工厂 future 返回 `Box<dyn Component>`；调用 `done` 是结束 `custom` 的正常途径，
///   `custom` 的返回值（擦除后的 `Box<dyn Any + Send>`）在这里被丢弃。
/// - 在无 UI 的宿主上 `custom` 会立即完成且不调用工厂（见
///   `kiana_ext_api::ui::ExtensionUiContext::custom` 的文档），因此 `run` 不会执行。
pub async fn show_llama_ui<F, Fut>(ctx: Arc<dyn ExtensionCommandContextApi>, run: F)
where
    F: FnOnce(LlamaViewHandle) -> Fut + Send + 'static,
    Fut: Future<Output = Result<(), AiError>> + 'static,
{
    let run_slot = Arc::new(Mutex::new(Some(run)));
    let caller_ctx = Arc::clone(&ctx);
    let factory: CustomFactory = Arc::new(move |tui, theme, keybindings, done| {
        // TS: `new LlamaView(tui, theme, keybindings)`；宿主的 `&Theme` 只在这一刻可借，
        // 组件要活到工厂返回之后，因此捕获快照（与 `mcp/ui.rs` 相同）。
        let theme = ThemeSnapshot::capture(theme);
        // `CustomFactory` 要求 `Send + Sync`，`FnOnce` 的 `run` 只能放在 `Mutex` 里取走；
        // 锁中毒时取回内部值即可（这里没有需要维持的跨字段不变量）。取完立刻释放锁，
        // 用户代码不会在临界区里执行。
        let run = run_slot
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        let ctx = Arc::clone(&ctx);
        Box::pin(async move {
            let view = Rc::new(LlamaView::new(
                render_requester_for_tui(tui),
                theme,
                keybindings,
            ));
            let handle = LlamaViewHandle(Rc::clone(&view));
            // `spawn_local` 必须在工厂 future 被轮询时（LocalSet 上下文里）调用。
            tokio::task::spawn_local(async move {
                let result = match run {
                    Some(run) => run(handle).await,
                    // 工厂被再次调用时 run 已被取走：把这次调用视为立即完成。
                    None => Ok(()),
                };
                // TS 的 Promise 只有一条错误通道：显示 `error.message` 后仍然关闭 UI。
                if let Err(error) = result {
                    ctx.ui().notify(&error.message, Some(NotifyKind::Error));
                }
                done(Box::new(()));
            });
            Box::new(LlamaViewComponent(view)) as Box<dyn Component>
        })
    });
    // 这里一直挂起到宿主调用 `done`；返回值是 `done` 的载荷，`/llama` 命令不消费它。
    let _ = caller_ctx.ui().custom(factory, None).await;
}

// ---------------------------------------------------------------------------
// runWithProgress
// ---------------------------------------------------------------------------

/// `runWithProgress` 的结果。
#[derive(Debug)]
pub enum RunProgressOutcome<T> {
    /// `{ cancelled: true }`。
    ///
    /// 只表示用户确认取消、且取消动作与 abort 都已执行；运行本身的结果（包括错误）
    /// 在取消路径被丢弃。
    Cancelled,
    /// `{ cancelled: false, value }`。
    ///
    /// 运行正常结束。运行返回 `Err` 时不会产生 `Completed`：非取消路径上
    /// [`run_with_progress`] 把该错误原样返回，取消路径则丢弃运行结果。
    Completed(T),
}

/// 进度更新回调（`Object.assign(state, progress)` 的等价物）。
///
/// 执行体用它推送进度；[`run_with_progress`] 把字段按 `Object.assign` 语义合并进
/// `ProgressState` 后调用 [`LlamaUi::update_progress`]。回调是同步的，返回时 UI 已收到
/// 快照。`Rc` 表示它只在单线程 UI 上使用。
pub type ProgressUpdateHandle<'a> = Rc<dyn Fn(LlamaProgress) + 'a>;

/// `runWithProgress` 的执行体。
///
/// 参数是取消作用域与进度更新回调；future 的生命周期 `'a` 允许借用调用方的数据。
/// future 只会被创建一次；正常结束与取消成功两条路径都会把它轮询到结束（取消动作
/// 返回 `Err` 时函数直接返回，future 被丢弃）。
pub type RunWithProgressFn<'a, T> = Box<
    dyn FnOnce(
            kiana_ai::utils::abort::AbortScope,
            ProgressUpdateHandle<'a>,
        ) -> LocalBoxFuture<'a, Result<T, AiError>>
        + 'a,
>;

/// `runWithProgress` 的取消动作。
///
/// 在 abort 之前执行（例如先 `unload` 模型）。返回 `Err` 时错误会原样传播，但 abort
/// 已经发生——顺序见 [`run_with_progress`] 的取消分支。
pub type CancelFn<'a> = Box<dyn FnOnce() -> LocalBoxFuture<'a, Result<(), AiError>> + 'a>;

/// `runWithProgress` 的选项。
pub struct RunWithProgressOptions<'a, T> {
    /// 标题。
    pub title: String,
    /// 正文模型 id。
    pub model: String,
    /// 首条消息。`ratio`/`detail` 初始为 `None`（进度条第一帧不显示）。
    pub initial_message: String,
    /// 取消确认标题。
    pub cancel_title: String,
    /// 取消确认正文（调用方传模型 id）。
    pub cancel_message: String,
    /// 执行体：`(signal, update) => Promise<T>`。
    pub run: RunWithProgressFn<'a, T>,
    /// 取消动作（先执行它，再 abort）。
    pub cancel: CancelFn<'a>,
}

/// `runWithProgress(ui, options)`。
///
/// 取消路径：`ui.progress(state)`（每次循环**重新创建**）先到 → `confirm` →
/// 执行 `cancel()`（若失败则原样抛出，但 controller 仍 abort）→ abort →
/// `await settled` → `{ cancelled: true }`。`completed` 标记用 `settled_done` 复刻。
///
/// 执行顺序：
/// 1. 建 `AbortScope` 与共享 `ProgressState`；`run` future 只创建、不在这一步轮询。
/// 2. 每轮循环拿当前状态快照调用 `ui.progress(&snapshot)`，然后在 `tokio::select!`
///    （`biased`，先轮询运行结果）里等待“运行结束”或“用户按取消”。
/// 3. 用户按取消后进入内层 select：确认框打开期间运行仍可能结束；此时先记下结果，等
///    用户回答后按 `if (!stop || completed) continue` 结束循环并返回运行结果。
/// 4. 确认要停：执行 `cancel()`、以 `Error("Cancelled")` abort，再等运行结束，然后返回
///    [`RunProgressOutcome::Cancelled`]；运行自己的错误被丢弃（TS 同样不传播）。
///
/// 不变量：
/// - `cancel` 只会被取走一次；取走后的分支必然返回，后续循环不会再用到它。
/// - 循环退出时 `settled_done` 与 `settled_result` 总是一起赋值，因此末尾的 `None`
///   分支只是兜底（正常路径不可达）。
/// - [`LlamaUi::progress`] 的实现必须在用户操作前保持 `Pending`；立即就绪会让这个循环
///   不断弹确认框。
///
/// # 错误
/// 运行本身的 `Err(AiError)` 原样返回；取消动作的 `Err` 用 `?` 返回（此时 abort 已
/// 发生）；两者都不会被包装成新的错误类型。
pub async fn run_with_progress<'a, T>(
    ui: &'a dyn LlamaUi,
    options: RunWithProgressOptions<'a, T>,
) -> Result<RunProgressOutcome<T>, AiError> {
    let RunWithProgressOptions {
        title,
        model,
        initial_message,
        cancel_title,
        cancel_message,
        run,
        cancel,
    } = options;
    let controller = kiana_ai::utils::abort::AbortScope::new();
    let state = Rc::new(RefCell::new(ProgressState {
        title,
        model,
        message: initial_message,
        ratio: None,
        detail: None,
    }));
    let update: ProgressUpdateHandle<'a> = {
        let state = Rc::clone(&state);
        Rc::new(move |progress: LlamaProgress| {
            let snapshot = {
                let mut state = state.borrow_mut();
                // TS: `Object.assign(state, progress)`——只有对象里存在的键才覆盖。
                state.message = progress.message;
                if let Some(ratio) = progress.ratio {
                    state.ratio = ratio;
                }
                if let Some(detail) = progress.detail {
                    state.detail = detail;
                }
                state.clone()
            };
            ui.update_progress(&snapshot);
        })
    };
    let mut cancel = Some(cancel);
    // `settled` 只被反复轮询、不会被 move：结果先缓存在 `settled_result`，循环结束后
    // 再统一决定返回 `Ok(Completed)` 还是 `Err`。`settled_done` 复刻 TS 的 `completed`。
    let mut settled = Box::pin(run(controller.clone(), update));
    let mut settled_result: Option<Result<T, AiError>> = None;
    let mut settled_done = false;
    while !settled_done {
        // 每轮都用当前快照重新拿 progress future；视图实现会复用同一个 promise。
        let snapshot = state.borrow().clone();
        let mut progress_future = ui.progress(&snapshot);
        // `biased` + 先写运行分支：两者同时就绪时优先采用运行结束，与 TS
        // `Promise.race([settled, progress])` 的顺序一致。
        tokio::select! {
            biased;
            result = &mut settled => {
                settled_result = Some(result);
                settled_done = true;
            }
            () = &mut progress_future => {
                let mut confirm_future = ui.confirm(&cancel_title, &cancel_message);
                // TS: `const stop = await ui.confirm(...)`——确认框必须等到用户回答；
                // 运行在此期间完成时只记录 `completed`（TS 里由 `settled.finally` 置位），
                // 回答后由 `if (!stop || completed) continue` 结束循环并返回运行结果。
                let stop = loop {
                    tokio::select! {
                        biased;
                        result = &mut settled, if !settled_done => {
                            settled_result = Some(result);
                            settled_done = true;
                        }
                        stop = &mut confirm_future => break stop,
                    }
                };
                if !stop || settled_done {
                    continue;
                }
                let Some(cancel_run) = cancel.take() else {
                    // 防御分支：`take` 之后的分支必然返回，正常流程不会第二轮走到这里。
                    continue;
                };
                // 先执行取消动作，再 abort：即使取消动作出错，abort 也必须发生（TS 的
                // try/finally）。错误在 abort 之后用 `?` 传播。
                let cancel_result = cancel_run().await;
                controller.abort_with(kiana_ai::utils::abort::AbortReason {
                    name: "Error".to_string(),
                    message: "Cancelled".to_string(),
                });
                cancel_result?;
                // 取消路径丢弃运行结果：无论运行是成功还是报错，都返回 Cancelled。
                let _ = (&mut settled).await;
                return Ok(RunProgressOutcome::Cancelled);
            }
        }
    }
    match settled_result {
        Some(Ok(value)) => Ok(RunProgressOutcome::Completed(value)),
        Some(Err(error)) => Err(error),
        // 兜底：`settled_done` 为真时 `settled_result` 必为 `Some`，正常路径不可达。
        None => Err(AiError::new(
            kiana_ai::error::AiErrorKind::Other,
            "The operation was aborted",
        )),
    }
}

// 测试分三组：纯格式化/JS 数字语义、视图布局快照（insta，快照文件在
// `src/llama/snapshots/`）、`run_with_progress` 的完成/取消/确认竞态。
// `FakeUi`/`DelayedConfirmUi` 把“用户按键”建模成可注入的 oneshot 信号。
#[cfg(test)]
mod tests {
    use super::super::client::LlamaModelStatusInfo;
    use super::*;
    use kiana_theme::theme::Theme;
    use std::pin::Pin;
    use std::task::{Context, Poll};

    fn model(id: &str, status: &str) -> LlamaModelInfo {
        LlamaModelInfo {
            id: id.to_string(),
            status: LlamaModelStatusInfo {
                value: status.to_string(),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn dark_theme() -> Arc<Theme> {
        let theme = kiana_theme::theme::get_theme_by_name("dark").expect("dark theme");
        // `keyHint`（kiana-ui-kit）使用全局主题；TUI 进程在启动时初始化它。
        kiana_theme::theme::set_theme_instance(Arc::clone(&theme));
        theme
    }

    fn keybindings() -> Arc<KeybindingsManager> {
        Arc::new(KeybindingsManager::new(Default::default(), None))
    }

    fn view() -> LlamaView {
        LlamaView::new(
            Rc::new(|| {}),
            ThemeSnapshot::capture(&dark_theme()),
            keybindings(),
        )
    }

    /// 评审回归：TS `new LlamaView(tui, theme, keybindings)` 用的是 `ctx.ui.custom` 交给工厂的
    /// 主题，而不是全局当前主题（与 `mcp/ui.rs` 的 `manager_view_uses_the_theme_passed_to_the_factory`
    /// 相同）。
    #[test]
    fn view_uses_the_theme_passed_to_the_factory() {
        kiana_theme::theme::init_theme(Some("dark"), false);
        let light = kiana_theme::theme::get_theme_by_name("light").expect("builtin light theme");
        let dark = kiana_theme::theme::get_theme_by_name("dark").expect("builtin dark theme");
        let view = LlamaView::new(
            Rc::new(|| {}),
            ThemeSnapshot::capture(&light),
            keybindings(),
        );
        let rendered = view.render_view(80).join("\n");
        assert!(
            rendered.contains(&light.fg(ThemeColor::Accent, &light.bold("llama.cpp models"))),
            "view must color the title with the passed theme"
        );
        assert!(
            !rendered.contains(&dark.fg(ThemeColor::Accent, &dark.bold("llama.cpp models"))),
            "view must not fall back to the global theme"
        );
        assert!(
            rendered.contains(&light.fg(ThemeColor::Muted, "Loading…")),
            "view must color the body with the passed theme"
        );
    }

    fn render_text(view: &LlamaView, width: usize) -> String {
        view.render_view(width)
            .into_iter()
            .map(|line| {
                kiana_core::ansi::strip_ansi(&line)
                    .replace(kiana_tui::tui::FAKE_CURSOR_START, "")
                    .replace(kiana_tui::tui::FAKE_CURSOR_END, "")
                    .replace(kiana_tui::tui::CURSOR_MARKER, "")
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn poll_pending<T>(future: Pin<&mut (dyn Future<Output = T> + '_)>) -> Poll<T> {
        let waker = futures::task::noop_waker();
        let mut context = Context::from_waker(&waker);
        future.poll(&mut context)
    }

    #[test]
    fn compact_counts_match_javascript() {
        assert_eq!(compact_count(999.0), "999");
        assert_eq!(compact_count(1500.0), "1.5k");
        assert_eq!(compact_count(100_000.0), "100k");
        assert_eq!(compact_count(1_200_000.0), "1.2M");
        assert_eq!(compact_count(10_000_000.0), "10M");
        assert_eq!(compact_count(1999.5), "2.0k");
        assert_eq!(compact_count(99_999.0), "100.0k");
        assert_eq!(compact_count(999_999.0), "1000k");
        assert_eq!(compact_count(9_999_999.0), "10.0M");
        // `String(value)` 的非有限值写法，不是 JSON 的 "null"。
        assert_eq!(compact_count(f64::NAN), "NaN");
        // `Infinity >= 1e6` → `(Infinity / 1e6).toFixed(0)` = "Infinity" + "M"。
        assert_eq!(compact_count(f64::INFINITY), "InfinityM");
        assert_eq!(compact_count(f64::NEG_INFINITY), "-Infinity");
    }

    #[test]
    fn rounds_like_javascript_math_round() {
        assert_eq!(js_round(0.5), 1.0);
        assert_eq!(js_round(1.5), 2.0);
        assert_eq!(js_round(-1.5), -1.0);
        assert_eq!(js_round(-0.5), -0.0);
        assert!(js_round(f64::NAN).is_nan());
        assert_eq!(js_round(f64::INFINITY), f64::INFINITY);
    }

    #[test]
    fn exact_model_pattern_uses_the_javascript_whitespace_set() {
        assert!(EXACT_MODEL_PATTERN.is_match("owner/repo"));
        assert!(EXACT_MODEL_PATTERN.is_match("owner/repo:Q4_K_M"));
        assert!(EXACT_MODEL_PATTERN.is_match("owner/a/b:Q4"));
        assert!(!EXACT_MODEL_PATTERN.is_match("owner/"));
        assert!(!EXACT_MODEL_PATTERN.is_match("/repo"));
        assert!(!EXACT_MODEL_PATTERN.is_match("owner/repo:"));
        assert!(!EXACT_MODEL_PATTERN.is_match("owner/repo:Q 4"));
        assert!(!EXACT_MODEL_PATTERN.is_match("own er/repo"));
        // JS `\s` 含 U+FEFF、不含 U+0085（Rust regex 的 `\s` 恰好相反）。
        assert!(!EXACT_MODEL_PATTERN.is_match("a\u{feff}/b"));
        assert!(EXACT_MODEL_PATTERN.is_match("a\u{0085}/b"));
    }

    #[test]
    fn context_labels_match_typescript() {
        let mut info = model("m", "loaded");
        info.meta = Some(super::super::client::LlamaModelMeta {
            n_ctx: Some(32768.0),
            ..Default::default()
        });
        assert_eq!(context_label(&info).as_deref(), Some("33k"));
        info.meta = Some(super::super::client::LlamaModelMeta {
            n_ctx: Some(512.0),
            ..Default::default()
        });
        assert_eq!(context_label(&info).as_deref(), Some("512"));
        info.meta = None;
        info.status.args = serde_json::json!(["-c", "8000"]);
        assert_eq!(context_label(&info).as_deref(), Some("8k"));
        info.status.args = serde_json::json!(["--ctx-size", "999.5"]);
        assert_eq!(context_label(&info).as_deref(), Some("999.5"));
        info.status.args = serde_json::json!(["-ctx", "1500"]);
        assert_eq!(context_label(&info).as_deref(), Some("2k"));
        info.status.args = serde_json::json!(["-c", "999499"]);
        assert_eq!(context_label(&info).as_deref(), Some("999k"));
        // 非数组 args 也按 JS 的 `.length`/数字键读取。
        info.status.args = serde_json::json!({
            "length": 3,
            "0": "llama-server",
            "1": "-c",
            "2": "8192"
        });
        assert_eq!(context_label(&info).as_deref(), Some("8k"));
        // 字符串按 UTF-16 码元索引：单字符标记永远不可能匹配，但读取不 panic。
        info.status.args = serde_json::json!("-c");
        assert_eq!(context_label(&info), None);
    }

    #[test]
    fn descriptions_match_typescript() {
        let mut info = model("m", "loaded");
        info.meta = Some(super::super::client::LlamaModelMeta {
            n_ctx: Some(32768.0),
            ..Default::default()
        });
        assert_eq!(model_description(&info), "loaded · 33k context");
        assert_eq!(model_description(&model("m", "loading")), "loading");
        assert_eq!(model_description(&model("m", "unloaded")), "");
        assert_eq!(model_description(&model("m", "sleeping")), "loaded");
    }

    #[test]
    fn initial_view_snapshot_width_80() {
        let view = view();
        insta::assert_snapshot!("llama_view_initial_80", render_text(&view, 80));
    }

    #[test]
    fn initial_view_snapshot_width_120() {
        let view = view();
        insta::assert_snapshot!("llama_view_initial_120", render_text(&view, 120));
    }

    #[test]
    fn models_view_snapshot_width_80() {
        let view = view();
        let models = vec![
            {
                let mut info = model("loaded-model", "loaded");
                info.meta = Some(super::super::client::LlamaModelMeta {
                    n_ctx: Some(32768.0),
                    ..Default::default()
                });
                info
            },
            model("sleeping-model", "sleeping"),
            model("loading-model", "loading"),
        ];
        let mut future = view.show_models("http://127.0.0.1:8080", &models);
        let poll = poll_pending(future.as_mut());
        assert!(poll.is_pending());
        insta::assert_snapshot!("llama_models_80", render_text(&view, 80));
    }

    #[test]
    fn progress_view_snapshot_width_80() {
        let view = view();
        let state = ProgressState {
            title: "Loading model".to_string(),
            model: "qwen".to_string(),
            message: "Loading load tensors".to_string(),
            ratio: Some(0.75),
            detail: Some("768 B / 1.00 KiB".to_string()),
        };
        let _future = view.progress(&state);
        insta::assert_snapshot!("llama_progress_80", render_text(&view, 80));
    }

    #[test]
    fn search_view_snapshot_width_120() {
        let view = view();
        let search: SearchModelsFn = Rc::new(|_query, _signal| Box::pin(async { Ok(Vec::new()) }));
        let _future = view.search_models(search);
        insta::assert_snapshot!("llama_search_120", render_text(&view, 120));
    }

    /// 测试替身：`progress` 每次调用都新建一个 `oneshot` 并替换保存的 sender，
    /// `trigger_cancel` 模拟用户按取消键（触发最近一次）；`states` 记录每次
    /// `update_progress` 收到的快照。
    #[derive(Default)]
    struct FakeUi {
        progress_sender: Mutex<Option<oneshot::Sender<()>>>,
        states: Mutex<Vec<ProgressState>>,
    }

    impl FakeUi {
        fn trigger_cancel(&self) {
            if let Some(sender) = self.progress_sender.lock().unwrap().take() {
                let _ = sender.send(());
            }
        }
    }

    impl LlamaUi for FakeUi {
        fn show_models<'a>(
            &'a self,
            _server_url: &'a str,
            _models: &'a [LlamaModelInfo],
        ) -> LocalBoxFuture<'a, LlamaManagerAction> {
            Box::pin(async { LlamaManagerAction::Close })
        }
        fn select<'a>(
            &'a self,
            _title: &'a str,
            _options: &'a [String],
        ) -> LocalBoxFuture<'a, Option<String>> {
            Box::pin(async { None })
        }
        fn confirm<'a>(&'a self, _title: &'a str, _message: &'a str) -> LocalBoxFuture<'a, bool> {
            Box::pin(async { true })
        }
        fn connection_error<'a>(
            &'a self,
            _server_url: &'a str,
            _message: &'a str,
        ) -> LocalBoxFuture<'a, ConnectionOutcome> {
            Box::pin(async { ConnectionOutcome::Close })
        }
        fn search_models<'a>(
            &'a self,
            _search: SearchModelsFn,
        ) -> LocalBoxFuture<'a, Option<String>> {
            Box::pin(async { None })
        }
        fn show_status(&self, _title: &str, _message: &str) {}
        fn progress<'a>(&'a self, _state: &'a ProgressState) -> LocalBoxFuture<'a, ()> {
            // 每次调用都创建新的 promise（TS 在 resolve 后清空 progressPromise）。
            let (sender, receiver) = oneshot::channel();
            *self.progress_sender.lock().unwrap() = Some(sender);
            Box::pin(async move {
                let _ = receiver.await;
            })
        }
        fn update_progress(&self, state: &ProgressState) {
            self.states.lock().unwrap().push(state.clone());
        }
    }

    #[test]
    fn run_with_progress_returns_completed_value() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let ui = FakeUi::default();
        let outcome = runtime
            .block_on(run_with_progress(
                &ui,
                RunWithProgressOptions {
                    title: "Loading model".to_string(),
                    model: "qwen".to_string(),
                    initial_message: "Starting…".to_string(),
                    cancel_title: "Stop loading?".to_string(),
                    cancel_message: "qwen".to_string(),
                    run: Box::new(move |_signal, update| {
                        update(LlamaProgress {
                            message: "Loading model".to_string(),
                            ratio: Some(Some(0.5)),
                            detail: None,
                        });
                        Box::pin(async { Ok::<u32, AiError>(42) })
                    }),
                    cancel: Box::new(|| Box::pin(async { Ok(()) })),
                },
            ))
            .expect("run");
        assert!(matches!(outcome, RunProgressOutcome::Completed(42)));
        assert_eq!(ui.states.lock().unwrap().len(), 1);
        assert_eq!(ui.states.lock().unwrap()[0].message, "Loading model");
    }

    /// TS: `Object.assign(state, progress)`——缺省的键保留旧值，显式 `undefined` 才清除。
    /// `loadAndWait` 的首条 `{ message: "Loading model" }` 不能抹掉 SSE 已送来的比例/细节。
    #[test]
    fn partial_progress_updates_preserve_ratio_and_detail() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let ui = FakeUi::default();
        let outcome = runtime
            .block_on(run_with_progress(
                &ui,
                RunWithProgressOptions {
                    title: "Loading model".to_string(),
                    model: "qwen".to_string(),
                    initial_message: "Starting…".to_string(),
                    cancel_title: "Stop loading?".to_string(),
                    cancel_message: "qwen".to_string(),
                    run: Box::new(move |_signal, update| {
                        update(LlamaProgress {
                            message: "Loading load tensors".to_string(),
                            ratio: Some(Some(0.5)),
                            detail: Some(Some("512 B / 1.00 KiB".to_string())),
                        });
                        // `{ message }`：ratio/detail 键不存在 → 保留。
                        update(LlamaProgress::message("Loading model"));
                        // `{ message, ratio: undefined }`：ratio 键存在但为 undefined → 清除。
                        update(LlamaProgress {
                            message: "Loading model".to_string(),
                            ratio: Some(None),
                            detail: None,
                        });
                        Box::pin(async { Ok::<u32, AiError>(1) })
                    }),
                    cancel: Box::new(|| Box::pin(async { Ok(()) })),
                },
            ))
            .expect("run");
        assert!(matches!(outcome, RunProgressOutcome::Completed(1)));
        let states = ui.states.lock().unwrap();
        assert_eq!(states.len(), 3);
        // 第二条：只更新 message，比例与细节都保留。
        assert_eq!(states[1].message, "Loading model");
        assert_eq!(states[1].ratio, Some(0.5));
        assert_eq!(states[1].detail.as_deref(), Some("512 B / 1.00 KiB"));
        // 第三条：ratio 显式 undefined（清除），detail 键不存在（保留）。
        assert_eq!(states[2].ratio, None);
        assert_eq!(states[2].detail.as_deref(), Some("512 B / 1.00 KiB"));
    }

    #[test]
    fn run_with_progress_confirms_cancel_then_aborts() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let ui = Arc::new(FakeUi::default());
        let trigger = Arc::clone(&ui);
        let cancel_called = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let cancel_called_by_run = Arc::clone(&cancel_called);
        let outcome = runtime.block_on(async move {
            let trigger_task = tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(10)).await;
                trigger.trigger_cancel();
            });
            let result = run_with_progress(
                ui.as_ref(),
                RunWithProgressOptions {
                    title: "Loading model".to_string(),
                    model: "qwen".to_string(),
                    initial_message: "Starting…".to_string(),
                    cancel_title: "Stop loading?".to_string(),
                    cancel_message: "qwen".to_string(),
                    run: Box::new(|signal, _update| {
                        Box::pin(async move {
                            signal.cancelled().await;
                            Err::<u32, AiError>(AiError::new(
                                kiana_ai::error::AiErrorKind::Aborted,
                                "Cancelled",
                            ))
                        })
                    }),
                    cancel: Box::new(move || {
                        cancel_called_by_run.store(true, std::sync::atomic::Ordering::SeqCst);
                        Box::pin(async { Ok(()) })
                    }),
                },
            )
            .await;
            let _ = trigger_task.await;
            result
        });
        assert!(matches!(
            outcome.expect("run"),
            RunProgressOutcome::Cancelled
        ));
        assert!(cancel_called.load(std::sync::atomic::Ordering::SeqCst));
    }

    /// 进度页的取消确认框在运行结束后也必须等用户回答（TS `const stop = await ui.confirm(...)`）：
    /// 运行先 settle 时不能丢弃 confirm future（旧实现会立即返回并留下一个无人接收的
    /// SelectList）；回答后按 `completed` 返回运行结果。
    struct DelayedConfirmUi {
        progress_sender: Mutex<Option<oneshot::Sender<()>>>,
        confirm_receiver: Mutex<Option<oneshot::Receiver<bool>>>,
        states: Mutex<Vec<ProgressState>>,
    }

    impl LlamaUi for DelayedConfirmUi {
        fn show_models<'a>(
            &'a self,
            _server_url: &'a str,
            _models: &'a [LlamaModelInfo],
        ) -> LocalBoxFuture<'a, LlamaManagerAction> {
            Box::pin(async { LlamaManagerAction::Close })
        }
        fn select<'a>(
            &'a self,
            _title: &'a str,
            _options: &'a [String],
        ) -> LocalBoxFuture<'a, Option<String>> {
            Box::pin(async { None })
        }
        fn confirm<'a>(&'a self, _title: &'a str, _message: &'a str) -> LocalBoxFuture<'a, bool> {
            let receiver = self
                .confirm_receiver
                .lock()
                .unwrap()
                .take()
                .expect("confirm must be called once");
            Box::pin(async move { receiver.await.unwrap_or(false) })
        }
        fn connection_error<'a>(
            &'a self,
            _server_url: &'a str,
            _message: &'a str,
        ) -> LocalBoxFuture<'a, ConnectionOutcome> {
            Box::pin(async { ConnectionOutcome::Close })
        }
        fn search_models<'a>(
            &'a self,
            _search: SearchModelsFn,
        ) -> LocalBoxFuture<'a, Option<String>> {
            Box::pin(async { None })
        }
        fn show_status(&self, _title: &str, _message: &str) {}
        fn progress<'a>(&'a self, _state: &'a ProgressState) -> LocalBoxFuture<'a, ()> {
            let (sender, receiver) = oneshot::channel();
            *self.progress_sender.lock().unwrap() = Some(sender);
            Box::pin(async move {
                let _ = receiver.await;
            })
        }
        fn update_progress(&self, state: &ProgressState) {
            self.states.lock().unwrap().push(state.clone());
        }
    }

    #[test]
    fn run_with_progress_waits_for_an_open_confirm_after_the_run_settles() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let (run_sender, run_receiver) = oneshot::channel::<()>();
            let (confirm_sender, confirm_receiver) = oneshot::channel::<bool>();
            let ui = Arc::new(DelayedConfirmUi {
                progress_sender: Mutex::new(None),
                confirm_receiver: Mutex::new(Some(confirm_receiver)),
                states: Mutex::new(Vec::new()),
            });
            let mut future = Box::pin(run_with_progress(
                ui.as_ref(),
                RunWithProgressOptions {
                    title: "Loading model".to_string(),
                    model: "qwen".to_string(),
                    initial_message: "Starting…".to_string(),
                    cancel_title: "Stop loading?".to_string(),
                    cancel_message: "qwen".to_string(),
                    run: Box::new(move |_signal, _update| {
                        Box::pin(async move {
                            let _ = run_receiver.await;
                            Ok::<u32, AiError>(7)
                        })
                    }),
                    cancel: Box::new(|| Box::pin(async { Ok(()) })),
                },
            ));

            // 第一轮：进度 promise 尚未 resolve，future 保持挂起并注册 sender。
            tokio::select! {
                biased;
                result = &mut future => panic!("must wait for the progress promise: {result:?}"),
                () = tokio::time::sleep(Duration::from_millis(10)) => {}
            }
            let sender = ui
                .progress_sender
                .lock()
                .unwrap()
                .take()
                .expect("progress polled");
            let _ = sender.send(());

            // 让 future 走到 `await ui.confirm(...)`：confirm 取得接收端并开始等待。
            let mut confirm_open = false;
            for _ in 0..100 {
                if ui.confirm_receiver.lock().unwrap().is_none() {
                    confirm_open = true;
                    break;
                }
                tokio::select! {
                    biased;
                    result = &mut future => panic!("must wait for the confirm dialog: {result:?}"),
                    () = tokio::time::sleep(Duration::from_millis(1)) => {}
                }
            }
            assert!(confirm_open, "confirm dialog must be open");

            // 运行在对话框打开期间 settle：TS 仍要等用户回答。
            let _ = run_sender.send(());
            tokio::select! {
                biased;
                result = &mut future => {
                    panic!("returned while the confirm dialog was unanswered: {result:?}")
                }
                () = tokio::time::sleep(Duration::from_millis(100)) => {}
            }

            // 回答后返回运行结果（`completed` 分支 → `{cancelled:false, value}`）。
            let _ = confirm_sender.send(true);
            let outcome = future.await.expect("run");
            assert!(matches!(outcome, RunProgressOutcome::Completed(7)));
        });
    }

    /// `Math.round(ratio * 100)` 用 `String(number)`：非有限比例显示 "NaN%"/"Infinity%"，
    /// 不能是 JSON 的 "null%"；`String.repeat(NaN)` 是 0 次（NaN 行没有填充/空白格）。
    #[test]
    fn progress_view_formats_non_finite_ratios_like_javascript() {
        let view = view();
        let mut state = ProgressState {
            title: "Loading model".to_string(),
            model: "qwen".to_string(),
            message: "Loading model".to_string(),
            ratio: Some(f64::NAN),
            detail: None,
        };
        let future = view.progress(&state);
        let text = render_text(&view, 120);
        assert!(text.contains(" NaN%"), "unexpected NaN bar: {text}");
        assert!(!text.contains("null"));
        drop(future);

        state.ratio = Some(f64::INFINITY);
        view.update_progress(&state);
        let text = render_text(&view, 120);
        assert!(
            text.contains(" Infinity%"),
            "unexpected Infinity bar: {text}"
        );
        let bar_line = text
            .lines()
            .find(|line| line.contains('█'))
            .expect("progress bar line");
        assert_eq!(bar_line.chars().filter(|c| *c == '█').count(), 40);
        assert!(!text.contains("null"));
    }

    #[test]
    fn progress_bar_has_forty_cells() {
        let view = view();
        let state = ProgressState {
            title: "Downloading model".to_string(),
            model: "owner/repo:Q4_K_M".to_string(),
            message: "Downloading model".to_string(),
            ratio: Some(0.5),
            detail: None,
        };
        let _future = view.progress(&state);
        let text = render_text(&view, 80);
        let bar_line = text
            .lines()
            .find(|line| line.contains('█'))
            .expect("progress bar line");
        let filled = bar_line
            .chars()
            .filter(|character| *character == '█')
            .count();
        let empty = bar_line
            .chars()
            .filter(|character| *character == '─')
            .count();
        assert_eq!(filled, 20);
        assert_eq!(empty, 20);
        assert!(bar_line.contains("50%"));
    }
}
