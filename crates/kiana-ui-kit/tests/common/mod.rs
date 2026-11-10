//! `kiana-ui-kit` 集成测试的公共设施。
//!
//! 每个测试二进制各自编译本模块；不是所有辅助函数都会被每个二进制用到，因此显式允许
//! 未使用项（这是共享测试工具模块的常见约定）。
#![allow(dead_code)]

use std::cell::{Cell, RefCell};
use std::future::Future;
use std::rc::Rc;
use std::sync::{Arc, Once};
use std::task::{Context, Poll, Waker};

use kiana_theme::{Theme, ThemeColor};
use kiana_tui::components::loader::RequestRenderCallback;
use kiana_tui::keybindings::{
    KeybindingDefinition, KeybindingsConfig, KeybindingsManager, tui_keybindings,
};
use kiana_tui::tui::Timer;
use kiana_tui::visible_width;

/// 初始化主题（进程级一次）并安装测试用键位表（**thread-local**，每个测试线程各一次）。
///
/// `kiana-tui` 的全局键位单例落在 thread-local：测试 harness 为每个测试开一个线程，因此每个
/// 测试开头都必须调用本函数（重复安装只会泄漏一个小对象）。
pub fn setup() {
    static THEME_ONCE: Once = Once::new();
    THEME_ONCE.call_once(|| kiana_theme::init_theme(Some("dark"), false));
    install_keybindings();
}

/// 安装包含 `app.message.copy -> ["ctrl+x"]` 的键位表（`tui.select.cancel` 保持默认
/// `["escape","ctrl+c"]`）。
pub fn install_keybindings() {
    let mut definitions = tui_keybindings();
    definitions.insert(
        "app.message.copy".to_string(),
        KeybindingDefinition::new(["ctrl+x"], "Copy"),
    );
    kiana_tui::set_keybindings(KeybindingsManager::new(
        definitions,
        KeybindingsConfig::new(),
    ));
}

/// 主题是进程级全局状态：需要切换主题的测试用这个互斥锁串行化（同一个测试二进制内共享）。
pub fn theme_lock() -> std::sync::MutexGuard<'static, ()> {
    static THEME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    THEME_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 当前全局主题实例（[`setup`] 之后可用）。
pub fn current_theme() -> Arc<Theme> {
    kiana_theme::THEME
        .current()
        .expect("theme initialized by common::setup")
}

/// `theme.fg(color, text)`（测试断言用）。
pub fn fg(color: ThemeColor, text: &str) -> String {
    kiana_theme::THEME
        .fg(color, text)
        .expect("theme initialized by common::setup")
}

/// `theme.inverse(text)`（测试断言用）。
pub fn inverse(text: &str) -> String {
    kiana_theme::THEME
        .inverse(text)
        .expect("theme initialized by common::setup")
}

/// `theme.getFgAnsi(color)`（测试断言用）。
pub fn fg_ansi(color: ThemeColor) -> String {
    kiana_theme::THEME
        .get_fg_ansi(color)
        .expect("theme initialized by common::setup")
}

/// `requestRender` 计数器。
pub fn request_render() -> (RequestRenderCallback, Rc<Cell<usize>>) {
    let count = Rc::new(Cell::new(0));
    let sink = Rc::clone(&count);
    (Rc::new(move || sink.set(sink.get() + 1)), count)
}

/// 渲染并把各行用 `\n` 连接（保留 ANSI）。
pub fn render_join(component: &mut dyn kiana_tui::Component, width: usize) -> String {
    component.render(width).join("\n")
}

/// 渲染并去掉终端序列（断言可见文本用）。
pub fn render_plain(component: &mut dyn kiana_tui::Component, width: usize) -> String {
    kiana_tui::strip_terminal_sequences(&render_join(component, width))
}

/// §8.4 A/F：每个组件在给定宽度下渲染的每一行都不得超过该宽度。
pub fn assert_width_invariant(component: &mut dyn kiana_tui::Component, widths: &[usize]) {
    for &width in widths {
        let lines = component.render(width);
        for (index, line) in lines.iter().enumerate() {
            let line_width = visible_width(line);
            assert!(
                line_width <= width,
                "line {index} visible width {line_width} exceeds width {width}: {line:?}"
            );
        }
    }
}

/// §8.4 F：常见宽度集合。
pub const WIDTHS: [usize; 6] = [1, 4, 10, 20, 80, 120];

/// 可手动触发的 [`Timer`] 实现（记录 delay 与 cancel 次数）。
#[derive(Clone)]
pub struct FakeTimerHandle(Rc<FakeTimerState>);

struct FakeTimerState {
    pending: RefCell<Option<Box<dyn FnOnce()>>>,
    delays: RefCell<Vec<u64>>,
    cancels: Cell<usize>,
}

/// 创建一对（注入用的定时器，测试侧的句柄）。
pub fn fake_timer() -> (FakeTimer, FakeTimerHandle) {
    let state = Rc::new(FakeTimerState {
        pending: RefCell::new(None),
        delays: RefCell::new(Vec::new()),
        cancels: Cell::new(0),
    });
    (
        FakeTimer {
            state: Rc::clone(&state),
        },
        FakeTimerHandle(state),
    )
}

/// 注入给组件的定时器。
pub struct FakeTimer {
    state: Rc<FakeTimerState>,
}

impl Timer for FakeTimer {
    fn set(&mut self, delay_ms: u64, callback: Box<dyn FnOnce()>) {
        self.state.delays.borrow_mut().push(delay_ms);
        *self.state.pending.borrow_mut() = Some(callback);
    }

    fn cancel(&mut self) {
        self.state.cancels.set(self.state.cancels.get() + 1);
        *self.state.pending.borrow_mut() = None;
    }
}

impl FakeTimerHandle {
    /// 触发当前排程的回调（没有则返回 false）。回调执行前先取出，允许回调里重新排程。
    pub fn fire(&self) -> bool {
        let callback = self.0.pending.borrow_mut().take();
        match callback {
            Some(callback) => {
                callback();
                true
            }
            None => false,
        }
    }

    /// 是否有待触发的回调。
    pub fn is_pending(&self) -> bool {
        self.0.pending.borrow().is_some()
    }

    /// 所有 `set` 的 delay 序列。
    pub fn delays(&self) -> Vec<u64> {
        self.0.delays.borrow().clone()
    }

    /// `cancel()` 的调用次数。
    pub fn cancel_count(&self) -> usize {
        self.0.cancels.get()
    }
}

/// 极简 `block_on`：本 crate 的 async 接缝（`AuthUrlComponent::copy`）没有真实 I/O 挂起。
pub fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    let waker = Waker::noop();
    let mut context = Context::from_waker(waker);
    loop {
        if let Poll::Ready(value) = future.as_mut().poll(&mut context) {
            return value;
        }
    }
}
