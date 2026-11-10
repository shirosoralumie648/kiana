//! auth-url.ts。
//!
//! 登录 URL + 点击提示 + 复制提示。宿主在按下 `app.message.copy` 时调用 [`AuthUrlComponent::copy`]，
//! 因为长 URL 会换行，往往无法整段选中或点击（SSH、tmux）。
//!
//! TS 源：`packages/coding-agent/src/modes/interactive/components/auth-url.ts`。
//!
//! 与 TS 的差异：
//! * TS 的 `TUI` 参数换成 `Option<RequestRenderCallback>`（TUI 适配，见 ARCHITECTURE §4）。
//! * `process.platform === "darwin"` → `cfg!(target_os = "macos")`。
//! * `copy()` 内部走 `kiana_core::clipboard::copy_to_clipboard`（错误 `Display` 逐字等于 TS 的
//!   `error.message`）；为了测试不碰真实剪贴板，额外提供 [`AuthUrlComponent::copy_with`] 注入点。
//!
//! # 结构与一次复制
//!
//! 组件本体只是一个 [`Container`]，固定两个子项（都是 `Text::with_padding(_, 1, 0)`，即左右
//! 各 1 列、没有上下留白）：
//!
//! 1. URL 行：`theme.fg("accent", hyperlink(url, url))`。终端支持 OSC 8 时 URL 本身可点击；
//! 2. 提示行：dim 色的点击提示（同样包成指向该 URL 的链接）+ dim 色的 `•` + 调用方着色的后缀。
//!
//! 构造时立即写入一次初始提示（`key_hint("app.message.copy", "to copy")`），并立即请求一次
//! 重绘。此后每次提示变化都整行重建字符串（[`Text`] 内部按原文缓存，`set_text` 会清掉缓存），
//! 再请求一次重绘。
//!
//! 复制流程：`copy()` 把 `url` 的副本交给剪贴板实现并等待结果；成功显示
//! `Copied URL to clipboard`（success token），失败显示错误消息的 `Display` 文本（error
//! token）。**错误不会返回给调用方**，提示行是唯一的反馈通道。`copy()` 不会自行 spawn 任务，
//! 是否放到后台执行由调用方决定；两个真实调用方都是在后台任务里驱动 `copy_with`（`copy`
//! 只是“默认剪贴板实现”的一层包装）。
//!
//! # 调用方
//!
//! * `kiana-interactive/src/components/login_dialog.rs` 的 OAuth 登录对话框；
//! * `kiana-builtin-extensions/src/mcp/ui.rs` 的 MCP 登录视图（`McpManagerView`）。
//!
//! 两者都在复制完成后通过共享句柄借出同一个组件改写提示行（组件可能已从内容区移除，
//! 但实例仍被保留）。`kiana-interactive` 的 `login_dialog.rs` 直接按模块路径
//! `kiana_ui_kit::auth_url::AuthUrlComponent` 导入；`components/index.rs` 没有再导出本类型。

use std::cell::RefCell;
use std::rc::Rc;

use kiana_theme::ThemeColor;
use kiana_tui::components::loader::RequestRenderCallback;
use kiana_tui::components::text_component::Text;
use kiana_tui::terminal_image::hyperlink;
use kiana_tui::tui::{Component, Container, TuiMouseEvent, TuiMouseEventResult};

use crate::keybinding_hints::key_hint;

/// 按全局主题着色（TS `theme.fg(token, str)`）。
fn fg(color: ThemeColor, text: &str) -> String {
    kiana_theme::THEME
        .fg(color, text)
        .expect("theme must be initialized before rendering (TS throws here)")
}

/// 平台相关的点击提示文案（TS 在每次 `setHint` 时重新计算）。
fn click_hint_for(is_macos: bool) -> &'static str {
    if is_macos {
        "Cmd+click to open"
    } else {
        "Ctrl+click to open"
    }
}

/// 当前平台的点击提示文案。
fn click_hint() -> &'static str {
    click_hint_for(cfg!(target_os = "macos"))
}

/// 共享 [`Text`] 的组件代理：让容器持有子项的同时，`AuthUrlComponent` 仍能改写提示文本。
///
/// [`Container::add_child`] 取得 `Box<dyn Component>` 的所有权，直接放入 `Text` 之后就没有
/// 句柄可以再 `set_text`。这里用 `Rc<RefCell<Text>>` 同时持有：容器得到代理对象，组件本体
/// 保留同一份 `Text` 的句柄。代理的 `handles_input` 恒为 `false`，提示行不接收输入。
struct SharedText(Rc<RefCell<Text>>);

impl Component for SharedText {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.0.borrow_mut().render(width)
    }

    fn handles_input(&self) -> bool {
        false
    }

    fn invalidate(&mut self) {
        self.0.borrow_mut().invalidate();
    }
}

/// 登录 URL 组件（TS `class AuthUrlComponent extends Container`）。
///
/// 只有两个子组件：URL 文本与提示文本（都没有 Spacer / DynamicBorder）。
pub struct AuthUrlComponent {
    /// 登录 URL（TS `readonly url`）。
    pub url: String,
    request_render: Option<RequestRenderCallback>,
    container: Container,
    hint: Rc<RefCell<Text>>,
}

impl AuthUrlComponent {
    /// TS `constructor(tui, url)`：两个 `Text(..., 1, 0)`，构造时立即设置初始提示。
    ///
    /// 副作用：构造即调用私有的 `set_hint`，所以传入 `Some(request_render)` 会
    /// 在返回前触发一次重绘请求。URL 被原样保存（不校验、不改写），既用于渲染，也用于
    /// 点击提示里的链接目标与复制。
    pub fn new(request_render: Option<RequestRenderCallback>, url: impl Into<String>) -> Self {
        let url = url.into();
        let mut container = Container::new();
        container.add_child(Box::new(Text::with_padding(
            fg(ThemeColor::Accent, &hyperlink(&url, &url)),
            1,
            0,
        )));
        let hint = Rc::new(RefCell::new(Text::with_padding("", 1, 0)));
        container.add_child(Box::new(SharedText(Rc::clone(&hint))));

        let mut component = AuthUrlComponent {
            url,
            request_render,
            container,
            hint,
        };
        let initial = key_hint("app.message.copy", "to copy");
        component.set_hint(&initial);
        component
    }

    /// 设置提示行（TS 私有 `setHint(suffix)`）。
    ///
    /// `suffix` 已经带色（调用方传入），这里**不再**套色。每次重新计算点击提示并请求重绘。
    ///
    /// 点击提示同样被包成指向 `self.url` 的 OSC 8 链接（与 URL 行共用同一个目标），因此用户
    /// 可以直接点击提示文字打开授权页。拼接格式固定为 `提示 • 后缀`，两段之间是半角空格；
    /// 后缀为空时行尾会留下空格（与 TS 的模板字符串一致）。
    fn set_hint(&mut self, suffix: &str) {
        let text = format!(
            "{} {} {}",
            fg(ThemeColor::Dim, &hyperlink(click_hint(), &self.url)),
            fg(ThemeColor::Dim, "•"),
            suffix
        );
        self.hint.borrow_mut().set_text(text);
        if let Some(request_render) = &self.request_render {
            request_render();
        }
    }

    /// TS `async copy()`：复制 URL，成功/失败都改写提示；错误不会抛出。
    ///
    /// future 在剪贴板操作完成前不会返回。需要界面在复制期间继续响应时，调用方应自行把它
    /// 放进后台任务，而不是在按键处理里直接 `await`。
    pub async fn copy(&mut self) {
        self.copy_with(|url| async move { kiana_core::clipboard::copy_to_clipboard(&url).await })
            .await;
    }

    /// 可注入剪贴板实现的 `copy()`（测试接缝，TS 用 `vi.mock` 达到同样目的）。
    ///
    /// `copy` 收到 URL 的副本并返回 `Result<(), E>`（`E: Display`，`Display` 必须逐字等于
    /// TS 的 `error.message`）。成功 → `success` token 的 `Copied URL to clipboard`；
    /// 失败 → `error` token 的错误消息，两者都会请求重绘。
    pub async fn copy_with<F, Fut, E>(&mut self, copy: F)
    where
        F: FnOnce(String) -> Fut,
        Fut: std::future::Future<Output = Result<(), E>>,
        E: std::fmt::Display,
    {
        match copy(self.url.clone()).await {
            Ok(()) => self.set_hint(&fg(ThemeColor::Success, "Copied URL to clipboard")),
            Err(error) => self.set_hint(&fg(ThemeColor::Error, &error.to_string())),
        }
    }

    /// 当前提示文本（已着色；测试用）。
    ///
    /// 返回的是拼接好的完整字符串（含 OSC 8 链接与 ANSI 颜色序列），不是纯文本；空后缀时
    /// 末尾会保留模板里的空格。
    pub fn hint_text(&self) -> String {
        self.hint.borrow().text().to_string()
    }
}

impl Component for AuthUrlComponent {
    /// 按子项顺序拼接：URL 行在前、提示行在后；两者都可能因宽度不足而折行。
    fn render(&mut self, width: usize) -> Vec<String> {
        self.container.render(width)
    }

    fn handles_input(&self) -> bool {
        // TS `AuthUrlComponent` 没有 `handleInput`（`Container` 本身也没有）。
        false
    }

    /// 按行把鼠标事件路由给子项（`Container` 用子项渲染高度定位）；未命中返回 `None`。
    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.container.handle_mouse(event)
    }

    fn invalidate(&mut self) {
        self.container.invalidate();
    }

    fn container_children(&self) -> Option<&[Box<dyn Component>]> {
        Some(&self.container.children)
    }

    fn container_children_mut(&mut self) -> Option<&mut Vec<Box<dyn Component>>> {
        Some(&mut self.container.children)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn click_hint_depends_on_platform() {
        assert_eq!(click_hint_for(true), "Cmd+click to open");
        assert_eq!(click_hint_for(false), "Ctrl+click to open");
    }
}
