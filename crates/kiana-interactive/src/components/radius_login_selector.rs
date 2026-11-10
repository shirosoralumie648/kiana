//! components/radius-login-selector.ts。
//!
//! `/login` 菜单里的 `Sign in with Radius` 选项：选中时用 Radius logo 四色做流动微光。
//! 微光只在 Radius 选项上使用，不暴露给其它选择器。
//!
//! ## 与 TS 的表示差异
//!
//! * `setInterval` 动画帧改为宿主注入 [`Timer`]（与 `CountdownTimer`/`Loader` 同一约定；
//!   未注入时（`AnimationState.timer` 为 `None`）不自行推进）。任何一次渲染都会按经过
//!   时间算出同一时刻的帧。
//! * `performance.now()` → `std::time::Instant`（单调时钟）。
//! * `for (const char of text)` 按 Unicode 码点迭代 → Rust `chars()`；`label.slice(text.length)`
//!   按 UTF-16 code unit 切片 → `js_slice_utf16`。
//!
//! ## 职责与调用方
//!
//! `/login` 命令构造登录方式列表时使用（调用方
//! `interactive_mode::commands::auth`）：普通选项原样交给内层
//! [`ExtensionSelectorComponent`]，只有当 Radius 选项被高亮时才把该行重绘成四色微光。
//! 微光状态完全封装在本组件内，内层选择器与其它选择器都看不到它。
//!
//! ## 执行流程
//!
//! 1. [`create_login_menu_selector`] 建立共享的动画状态，并把 select/cancel 回调包一层：
//!    任一路径离开菜单都会先停止动画，再调用宿主回调。
//! 2. 宿主通过 [`RadiusLoginMenuComponent::set_timer`] 注入定时器后，`schedule` 每 50ms
//!    申请一次重绘并立即重新排下一帧；回调只有在 `animating` 为真时才真正调用
//!    `request_render`，因此未高亮 Radius 行时定时器链空转但不触发渲染。
//! 3. `render` 先把内层输出原样渲染，再用“选中行的完整渲染文本”定位 Radius 行；命中后按
//!    构造以来的经过时间计算色带，替换该行。
//!
//! ## 约束与失败模式
//!
//! * 帧是经过时间的纯函数，不保存逐帧状态：宿主晚些渲染时会直接算出当前相位，而不是从头
//!   播放。没有注入定时器时不会自动重绘，但每次渲染仍按构造以来的经过时间取帧。
//! * 主题未初始化时 `fg` / `radius_shimmer` 会 panic，与 TS 访问未初始化主题即抛错一致。
//! * `render` 按渲染文本精确匹配选中行；若内层选择器的选中行格式改变（例如 `→ ` 前缀或
//!   左留白不同），匹配会失败——此时只是不显示微光并停用动画，不会报错。
//! * 定时器回调持 `Weak<AnimationState>`，避免“状态持有定时器、定时器回调持有状态”的
//!   引用环；`dispose` 之后即使宿主仍触发旧回调，也不会再申请渲染。
//!
//! ## 与宿主的定时器约定（重要）
//!
//! `AnimationState.timer` 初值是 `None`，所以 [`create_login_menu_selector`] 返回的组件
//! **不会自己动**：只有宿主调用 [`RadiusLoginMenuComponent::set_timer`] 之后，`schedule` 才会
//! 排下第一帧（每帧 `ANIMATION_FRAME_MS` = 50 ms）。当前仓库里唯一的调用方
//! `interactive_mode::commands::auth` 只负责构造组件和在 `dispose` 回调里释放它，没有调用
//! `set_timer`；注入定时器的路径只出现在集成测试
//! `tests/components_small_selectors.rs::radius_login_selector_shimmers_the_selected_option`。
//! 未注入时微光不会逐帧刷新，但每次 `render` 仍会按“构造以来的经过时间”算出当前相位，
//! 因此宿主因其它原因重绘时画面依旧是正确的时刻。
//!
//! ### 相对 TS 的补充
//!
//! TS 版本在构造时就启动 `setInterval`；Rust 侧把“排第一帧”的时机推迟到 `set_timer`。
//! 停止动画（选中/取消）之后实例不会再自行续排，直到宿主再次调用 `set_timer`；`dispose`
//! 之后即使再 `set_timer` 也不会恢复。
//!
//! ## 生命周期
//!
//! 定时器链一旦开始就无条件自我续排（每帧结束再 `set` 下一帧），`animating` 只决定这一帧
//! 是否调用 `request_render`；`stop_animation` 自增 `generation` 并 `cancel`，让已排队但
//! 尚未执行的回调失效。`dispose` 额外置 `disposed`，此后 `schedule` 和任何迟到回调都直接
//! 返回，`set_timer` 也只会替换定时器而不再排程。

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::LazyLock;
use std::time::Instant;

use kiana_theme::ThemeColor;
use kiana_tui::colors::Color;
use kiana_tui::components::loader::RequestRenderCallback;
use kiana_tui::components::text_component::Text;
use kiana_tui::terminal_colors::ColorMixSpace;
use kiana_tui::tui::{Component, Timer};
use kiana_tui::{foreground_ansi, mix_colors, parse_color};

use crate::components::extension_selector::ExtensionSelectorComponent;

/// Radius logo 的四种颜色，按流动顺序。
///
/// `parse_color` 返回 `Option`，对代码里硬编码的合法十六进制不会失败；这里 `expect`
/// 表达“常量表必须有效”的意图，一旦有人手改错就会在首次使用时 panic。
static RADIUS_COLORS: LazyLock<[Color; 4]> = LazyLock::new(|| {
    ["#4d9abf", "#83ccd2", "#f1be57", "#f09082"]
        .map(|hex| parse_color(hex).expect("valid Radius hex color"))
});

/// 每个色带的宽度（字符）。
const CHARS_PER_COLOR: usize = 4;
/// 每秒流动的字符数。
const CHARS_PER_SECOND: f64 = 10.0;
/// 动画帧间隔（毫秒）。
const ANIMATION_FRAME_MS: u64 = 50;

/// 用主题前景色给 `text` 上色。
///
/// 主题未初始化时 `THEME.fg` 返回 `Err`，这里直接 `expect` panic：TS 版本访问未初始化的
/// 主题同样会抛错，移植时保留这一失败模式而不是静默降级成无色文本。
fn fg(color: ThemeColor, text: &str) -> String {
    kiana_theme::THEME
        .fg(color, text)
        .expect("theme must be initialized before rendering (TS throws here)")
}

/// JS `string.length`（UTF-16 code unit 数）。
fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// JS `text.slice(start)`（按 UTF-16 code unit；孤立代理项写成 U+FFFD）。
fn js_slice_utf16(text: &str, start: usize) -> String {
    let units: Vec<u16> = text.encode_utf16().collect();
    if start >= units.len() {
        return String::new();
    }
    String::from_utf16_lossy(&units[start..])
}

/// Color `text` with the Radius logo colors flowing left to right；`elapsedMs` is the animation time。
///
/// 把每个字符的位置映射到长度为 `4 * 4` 的循环色带上：位置随 `elapsed_ms` 增大而左移，
/// 相邻色带之间用 smoothstep（`t²(3-2t)`）平滑混合。返回值自带 ANSI 前景色序列，末尾补
/// `\x1b[39m` 复位默认前景色，调用方在其后拼接的文本不会被染色。
fn radius_shimmer(text: &str, elapsed_ms: f64) -> String {
    let mode = kiana_theme::THEME
        .get_color_mode()
        .expect("theme must be initialized before rendering (TS throws here)");
    let cycle = (RADIUS_COLORS.len() * CHARS_PER_COLOR) as f64;
    // 经过时间换算成“已流动的字符数”；随时间增大，色带位置相对文本向左移动。
    let offset = (elapsed_ms / 1000.0) * CHARS_PER_SECOND;
    let mut result = String::new();
    let mut index: f64 = 0.0;
    for character in text.chars() {
        // Rust 的 `%` 对负数保留符号，因此再做一次 `+ cycle` 归一化到 [0, cycle)。
        let position = (((index - offset) % cycle) + cycle) % cycle;
        // band 是所处色带，t 是在该色带内的进度（0..1）。
        let band = (position / CHARS_PER_COLOR as f64).floor() as usize;
        let t = position / CHARS_PER_COLOR as f64 - band as f64;
        // Smoothstep keeps each band recognizable while still blending into the next one.
        let amount = t * t * (3.0 - 2.0 * t);
        let from = RADIUS_COLORS[band % RADIUS_COLORS.len()];
        let to = RADIUS_COLORS[(band + 1) % RADIUS_COLORS.len()];
        result.push_str(&foreground_ansi(
            mix_colors(from, to, amount, ColorMixSpace::Srgb),
            mode,
        ));
        result.push(character);
        index += 1.0;
    }
    format!("{result}\x1b[39m")
}

/// `RadiusOption`：`label` 是完整选项，以动画的 `text` 开头。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RadiusOption {
    /// 完整选项文本（与 `ExtensionSelectorComponent` 的选项一致）。
    pub label: String,
    /// 参与微光的开头子串。
    pub text: String,
}

/// 动画共享状态（定时器回调持 `Weak`，避免引用环）。
struct AnimationState {
    /// 当前渲染是否检测到 Radius 行被选中；只有为真时定时器回调才申请重绘。
    animating: Cell<bool>,
    /// `dispose` 之后永久为真：所有定时器回调与 `schedule` 都据此退出。
    disposed: Cell<bool>,
    /// 每次停止动画自增；旧回调发现代数不符就退出，防止宿主在 `cancel` 后仍调用它。
    generation: Cell<u64>,
    /// 宿主定时器；`None` 表示尚未注入，`schedule` 直接返回。
    timer: RefCell<Option<Box<dyn Timer>>>,
    /// 宿主的重绘请求；构造时固定，之后不再变化。
    request_render: Option<RequestRenderCallback>,
    /// 动画起点（`performance.now()` 的构造时刻）。
    start: Instant,
}

/// 安排下一帧：把回调写进宿主定时器；回调触发后重新调用自身，形成固定间隔的重绘链。
/// 代数（generation）检查让 `stop_animation` 之后仍可能被宿主触发的旧回调变成 no-op。
///
/// 注意续排是无条件的：即使 `animating` 为假，回调末尾仍会 `schedule` 下一帧，因此整条链
/// 在停止前一直以 50 ms 间隔“空转”（只是不调用 `request_render`）。该写法假设宿主定时器
/// 在每次 `set` 后只保留最新的待触发回调——集成测试里的 `FakeTimer` 就是这样保存单个
/// `pending` 的；若某个宿主实现会累积多个回调，同一实例会收到多次续排。
fn schedule(animation: &Rc<AnimationState>) {
    if animation.disposed.get() || animation.timer.borrow().is_none() {
        return;
    }
    let generation = animation.generation.get();
    let weak = Rc::downgrade(animation);
    if let Some(timer) = animation.timer.borrow_mut().as_mut() {
        timer.set(
            ANIMATION_FRAME_MS,
            Box::new(move || {
                let Some(animation) = weak.upgrade() else {
                    return;
                };
                if animation.disposed.get() || animation.generation.get() != generation {
                    return;
                }
                if animation.animating.get()
                    && let Some(request_render) = &animation.request_render
                {
                    request_render();
                }
                schedule(&animation);
            }),
        );
    }
}

/// 停止重绘链：自增代数使已排队的回调失效，取消宿主定时器，并清空 `animating`。
fn stop_animation(animation: &AnimationState) {
    animation.generation.set(animation.generation.get() + 1);
    if let Some(timer) = animation.timer.borrow_mut().as_mut() {
        timer.cancel();
    }
    animation.animating.set(false);
}

/// 带 Radius 微光的 `/login` 选择器（TS 私有 `RadiusLoginMenuComponent`）。
///
/// 渲染与输入都委托给内层 [`ExtensionSelectorComponent`]；本组件只多持有动画状态与
/// [`RadiusOption`]，并在每次 `render` 时覆盖选中的 Radius 行。
pub struct RadiusLoginMenuComponent {
    inner: ExtensionSelectorComponent,
    radius_option: RadiusOption,
    animation: Rc<AnimationState>,
}

impl RadiusLoginMenuComponent {
    /// 安装动画时钟（宿主定时器；未安装时不会自行推进，但渲染仍按经过时间取帧）。
    ///
    /// 替换旧定时器时会先 `cancel` 它，再从当前时刻重新排一帧；组件已 `dispose` 时只替换
    /// 不排程。
    pub fn set_timer(&mut self, timer: Box<dyn Timer>) {
        let old = self.animation.timer.borrow_mut().replace(timer);
        if let Some(mut old) = old {
            old.cancel();
        }
        schedule(&self.animation);
    }

    /// 内层选择器（`set_timer`/`dispose` 之外的组件 API）。
    ///
    /// 本组件不会把 `set_timer` 转发给内层；内层是以默认选项构造的，没有倒计时。
    pub fn inner(&self) -> &ExtensionSelectorComponent {
        &self.inner
    }

    /// 可变内层选择器。
    ///
    /// 经它调用内层的 `set_timer` 只影响内层倒计时（这里没有倒计时），与微光动画无关。
    pub fn inner_mut(&mut self) -> &mut ExtensionSelectorComponent {
        &mut self.inner
    }

    /// TS `dispose()`：停止动画再 dispose 内层。
    ///
    /// 置 `disposed` 后 `schedule` 与已排队的回调都会直接返回；之后再调用 `set_timer`
    /// 也不会重新启动动画。
    pub fn dispose(&mut self) {
        self.animation.disposed.set(true);
        stop_animation(&self.animation);
        self.inner.dispose();
    }
}

impl Component for RadiusLoginMenuComponent {
    /// 渲染内层选择器，并在 Radius 行被选中时用微光替换该行。
    ///
    /// 选中判定依赖内层对选中行的渲染结果与本函数用 `→ ` + `label` 重建的文本逐字相同
    /// （同样经过 `Text::with_padding(_, 1, 0)`）；匹配不到就只是关闭动画，不影响正常渲染。
    fn render(&mut self, width: usize) -> Vec<String> {
        let mut lines = self.inner.render(width);

        // 先在“未加微光”的输出里定位 Radius 行：用与内层相同的格式重建选中行文本。
        let label = &self.radius_option.label;
        let selected_line = Text::with_padding(
            fg(ThemeColor::Accent, "→ ") + &fg(ThemeColor::Accent, label),
            1,
            0,
        )
        .render(width)
        .first()
        .cloned();
        let index = selected_line
            .as_ref()
            .and_then(|selected| lines.iter().position(|line| line == selected));
        // 只有 Radius 行当前可见且被高亮时，后续帧才会真正调用 `request_render`；定时器链
        // 本身照常续排（见 `schedule`）。
        self.animation.animating.set(index.is_some());
        if let Some(index) = index {
            let elapsed_ms = self.animation_start_ms();
            // 只给 `text` 前缀上色，剩余 `label` 保持普通强调色（TS 的 slice 语义）。
            let shimmer = radius_shimmer(&self.radius_option.text, elapsed_ms);
            let suffix = js_slice_utf16(
                &self.radius_option.label,
                utf16_len(&self.radius_option.text),
            );
            let animated_line = fg(ThemeColor::Accent, "→ ") + &shimmer + &suffix;
            lines[index] = Text::with_padding(animated_line, 1, 0)
                .render(width)
                .first()
                .cloned()
                .unwrap_or_default();
        }
        lines
    }

    /// 全部按键原样交给内层选择器；选择/取消时由构造时安装的回调停止动画。
    fn handle_input(&mut self, key_data: &str) {
        self.inner.handle_input(key_data);
    }

    /// 与内层一致：本组件接收输入（再由 `handle_input` 转发）。
    fn handles_input(&self) -> bool {
        true
    }

    /// 转发内层的容器子项，保持焦点遍历能看到选择列表。
    fn container_children(&self) -> Option<&[Box<dyn Component>]> {
        self.inner.container_children()
    }
}

impl RadiusLoginMenuComponent {
    /// `performance.now() - animationStart`：返回构造以来经过的毫秒数。
    fn animation_start_ms(&self) -> f64 {
        // `performance.now() - this.animationStart`：动画起点在构造时。
        self.animation.start.elapsed().as_secs_f64() * 1000.0
    }
}

/// 顶层 `/login` 选择器（TS `createLoginMenuSelector(tui, title, options, radiusOption, onSelect, onCancel)`）。
///
/// `request_render` 是宿主的重绘请求，注入定时器后才会被动画帧使用；`radius_option.label`
/// 必须与 `options` 中的某一项完全一致，否则不会出现微光（`render` 按文本匹配）。
/// `on_select` / `on_cancel` 总是在动画停止之后调用。
///
/// 返回的组件只配置了动画状态，宿主要再调用 [`RadiusLoginMenuComponent::set_timer`] 才能
/// 让微光自己动起来；在此之前每次渲染仍会按构造以来经过的时间取帧。
///
/// 内层用 `ExtensionSelectorOptions::default()` 构造：没有倒计时（`timeout` 为 `None`）、
/// 没有 `app.tools.expand` 处理器，也没有把 `request_render` 传给内层。因此
/// `RadiusLoginMenuComponent::set_timer` 只驱动微光，不会转交给内层的 `CountdownTimer`
/// （内层本来也没有倒计时）。
pub fn create_login_menu_selector(
    request_render: RequestRenderCallback,
    title: &str,
    options: Vec<String>,
    radius_option: RadiusOption,
    mut on_select: impl FnMut(&str) + 'static,
    mut on_cancel: impl FnMut() + 'static,
) -> RadiusLoginMenuComponent {
    let animation = Rc::new(AnimationState {
        animating: Cell::new(false),
        disposed: Cell::new(false),
        generation: Cell::new(0),
        timer: RefCell::new(None),
        request_render: Some(request_render),
        start: Instant::now(),
    });
    // 两个回调各持一份 Rc：内层选择器只要触发选择或取消，就在调用宿主回调前停掉动画。
    let select_animation = Rc::clone(&animation);
    let cancel_animation = Rc::clone(&animation);
    let inner = ExtensionSelectorComponent::new(
        title,
        options,
        move |option: &str| {
            stop_animation(&select_animation);
            on_select(option);
        },
        move || {
            stop_animation(&cancel_animation);
            on_cancel();
        },
        crate::components::extension_selector::ExtensionSelectorOptions::default(),
    );
    RadiusLoginMenuComponent {
        inner,
        radius_option,
        animation,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_slice_matches_js_slice_semantics() {
        assert_eq!(js_slice_utf16("abcdef", 3), "def");
        assert_eq!(js_slice_utf16("abc", 3), "");
        assert_eq!(js_slice_utf16("abc", 5), "");
        // 代理对：按 code unit 切到中间会产生孤立代理项 → U+FFFD。
        assert_eq!(js_slice_utf16("a😀b", 2), "\u{FFFD}b");
        assert_eq!(utf16_len("a😀b"), 4);
    }
}
