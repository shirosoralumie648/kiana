//! components/cancellable-loader.ts。
//!
//! 可用 Esc 取消的加载组件：在 [`Loader`] 之上加一个取消令牌
//! （TS 的 `AbortController`，Rust 侧按 ARCHITECTURE §4.2 用
//! [`tokio_util::sync::CancellationToken`]）。
//!
//! # 职责与调用方
//!
//! [`CancellableLoader`] 把 [`Loader`] 的渲染与动画接口原样转发，只在键盘输入上增加一个
//! “取消”动作：命中键位 `tui.select.cancel`（默认 `escape` / `ctrl+c`，可由用户键位覆盖，
//! 见 [`get_keybindings`]）时取消 [`CancellationToken`] 并调用
//! [`CancellableLoader::on_abort`]。`kiana-ui-kit` 的 `BorderedLoader` 在可取消模式下用它
//! 作为内部 loader，并通过 [`CancellableLoader::signal`] 把取消信号交给异步任务。
//!
//! # 取消语义与约束
//!
//! 1. [`CancellableLoader::signal`] 返回同一个令牌的克隆；所有克隆共享取消状态，取消后
//!    [`CancellableLoader::aborted`] 与各克隆的 `is_cancelled()` 立即为真，且没有重置接口。
//! 2. `on_abort` 是同步回调，在按键处理期间直接执行，执行时令牌**已经**处于取消状态。
//!    重复按取消键会重复调用回调（对已取消的令牌再 `cancel` 是无操作）。
//! 3. [`CancellableLoader::dispose`] 只停止底层动画，不取消令牌，也不触发 `on_abort`。
//! 4. 本类型没有覆写 [`Component::handles_input`]，因此取默认值 `true`：TUI 会把键盘输入
//!    路由给它，这是取消键能生效的前提。
//!
//! # 渲染
//!
//! `render` / `invalidate` 全部委托给 [`Loader`]；Loader 的 `render` 总是在最前面插入一个
//! 空行，所以调用方看到的第 2 行才是 spinner + 消息。

use tokio_util::sync::CancellationToken;

use crate::components::loader::{Loader, LoaderIndicatorOptions, RequestRenderCallback};
use crate::components::text_component::StyleFn;
use crate::keybindings::get_keybindings;
use crate::tui::Component;

/// 可取消的加载组件（TS `class CancellableLoader extends Loader`）。
///
/// 用组合代替继承：`Loader` 作为字段保存，渲染与动画方法逐项转发；取消状态独立于 Loader。
pub struct CancellableLoader {
    /// 底层 [`Loader`]（负责文本、spinner 与动画节拍）。
    loader: Loader,
    /// 取消令牌；[`CancellableLoader::signal`] 返回它的克隆。
    token: CancellationToken,
    /// 用户按下 Esc 时调用（TS `onAbort?: () => void`）。
    ///
    /// 按 `FnMut` 保存，可捕获可变状态；取消键每次命中都会调用一次。
    pub on_abort: Option<Box<dyn FnMut()>>,
}

impl CancellableLoader {
    /// `constructor(ui, spinnerColorFn, messageColorFn, message, indicator?)`。
    ///
    /// 新建的令牌尚未取消，`on_abort` 初始为 `None`；其余参数原样交给内部的 [`Loader`]。
    pub fn new(
        request_render: Option<RequestRenderCallback>,
        spinner_color_fn: StyleFn,
        message_color_fn: StyleFn,
        message: &str,
        indicator: Option<LoaderIndicatorOptions>,
    ) -> Self {
        CancellableLoader {
            loader: Loader::new(
                request_render,
                spinner_color_fn,
                message_color_fn,
                message,
                indicator,
            ),
            token: CancellationToken::new(),
            on_abort: None,
        }
    }

    /// `signal` getter：异步任务应 `select!` 的取消令牌。
    ///
    /// 返回的是令牌克隆，与组件共享取消状态；任务端只需持有这个克隆，不必再借用组件。
    pub fn signal(&self) -> CancellationToken {
        self.token.clone()
    }

    /// `aborted` getter：是否已被取消。
    ///
    /// 令牌一旦取消不会恢复（没有重置接口），因此该值只会从 `false` 变为 `true`。
    pub fn aborted(&self) -> bool {
        self.token.is_cancelled()
    }

    /// 底层 [`Loader`]。
    pub fn loader(&self) -> &Loader {
        &self.loader
    }

    /// 底层 [`Loader`]（可变）。
    pub fn loader_mut(&mut self) -> &mut Loader {
        &mut self.loader
    }

    /// `setMessage(message)`。
    ///
    /// 转发到 [`Loader::set_message`]：立即刷新文本并调用重绘回调（若已安装）。
    pub fn set_message(&mut self, message: &str) {
        self.loader.set_message(message);
    }

    /// `start()`。
    ///
    /// 转发到 [`Loader::start`]（刷新显示并（重新）启动动画），不改动取消状态。
    pub fn start(&mut self) {
        self.loader.start();
    }

    /// `stop()`。
    ///
    /// 停止动画但保留当前帧；取消状态不变。
    pub fn stop(&mut self) {
        self.loader.stop();
    }

    /// `dispose()`：停止动画。
    ///
    /// 只做 [`Loader::stop`]；令牌不会被取消，`on_abort` 也不会被调用（见模块文档）。
    pub fn dispose(&mut self) {
        self.loader.stop();
    }

    /// `setIndicator(indicator)`。
    ///
    /// 转发到 [`Loader::set_indicator`]：重置帧计数并重新开始动画。
    pub fn set_indicator(&mut self, indicator: Option<LoaderIndicatorOptions>) {
        self.loader.set_indicator(indicator);
    }

    /// 安装/替换重绘回调。
    ///
    /// 转发到 [`Loader::set_request_render`]；已安装的动画定时器也会改用新回调。
    pub fn set_request_render(&mut self, request_render: Option<RequestRenderCallback>) {
        self.loader.set_request_render(request_render);
    }

    /// 安装动画节拍器（见 [`Loader::set_timer`]）。
    ///
    /// 若动画已启动且帧数大于 1，会立即开始排程。
    pub fn set_timer(&mut self, timer: Box<dyn crate::tui::Timer>) {
        self.loader.set_timer(timer);
    }

    /// 宿主渲染循环节拍（见 [`Loader::tick`]）。
    ///
    /// 返回 `true` 表示按 `now` 计算出的动画帧与当前显示的文本不同，宿主应重绘。
    pub fn tick(&mut self, now: std::time::Instant) -> bool {
        self.loader.tick(now)
    }
}

impl Component for CancellableLoader {
    fn render(&mut self, width: usize) -> Vec<String> {
        // 渲染完全由 Loader 决定（首行空行 + spinner/消息），本组件不加额外装饰。
        self.loader.render(width)
    }

    /// TS `handleInput(data)`：命中 `tui.select.cancel` 时取消令牌并调用 `on_abort`，
    /// 其它按键不做任何事。trait 没有返回值，因此无法向调用方报告“已处理”。
    fn handle_input(&mut self, data: &str) {
        // 每次按键都查全局键位表：`set_keybindings` 可能在运行期替换绑定。
        let kb = get_keybindings();
        if kb.matches(data, "tui.select.cancel") {
            // 先取消令牌再回调，回调执行时观察 signal 已是取消态。
            self.token.cancel();
            if let Some(on_abort) = &mut self.on_abort {
                on_abort();
            }
        }
    }

    fn invalidate(&mut self) {
        // Loader::invalidate 会同步当前动画帧并请求重绘（见 loader.rs）。
        self.loader.invalidate();
    }

    /// TS `dispose()`：停止动画。
    fn dispose(&mut self) {
        // 只停止动画；令牌与 on_abort 不受影响。
        CancellableLoader::dispose(self);
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;
    use std::sync::Arc;

    use super::*;
    use crate::text::visible_width;

    fn identity() -> StyleFn {
        Arc::new(|text: &str| text.to_string())
    }

    #[test]
    fn aborts_on_the_cancel_keybinding() {
        let mut loader = CancellableLoader::new(None, identity(), identity(), "Working...", None);
        let signal = loader.signal();
        let aborted = Rc::new(Cell::new(false));
        let sink = Rc::clone(&aborted);
        loader.on_abort = Some(Box::new(move || sink.set(true)));
        assert!(!loader.aborted());
        assert!(!signal.is_cancelled());

        loader.handle_input("\x1b");

        assert!(loader.aborted());
        assert!(signal.is_cancelled());
        assert!(aborted.get());
    }

    #[test]
    fn ignores_other_keys() {
        let mut loader = CancellableLoader::new(None, identity(), identity(), "Working...", None);
        loader.handle_input("a");
        assert!(!loader.aborted());
    }

    #[test]
    fn renders_through_the_underlying_loader() {
        let mut loader = CancellableLoader::new(None, identity(), identity(), "Working...", None);
        let lines = loader.render(20);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "");
        assert!(lines[1].trim_start().starts_with("⠋ Working..."));
        assert_eq!(visible_width(&lines[1]), 20);
    }
}
