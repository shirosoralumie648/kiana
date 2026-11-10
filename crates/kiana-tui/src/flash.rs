//! components/alt-screen-flash.ts：备用屏的瞬时消息栈。
//!
//! 被 [`crate::alt_screen::TuiAltScreen`] 合成到屏幕顶部若干行。
//!
//! ## 与 TS 的表示差异
//!
//! TS 用 `setTimeout(..., max(0, durationMs))` + `timer.unref()` 在到期时删除条目并请求重绘。
//! Rust 侧没有全局定时器（`crate::tui::Timer` 是渲染调度的单槽定时器，不能复用），因此改成
//! **惰性截止时间**：条目携带 [`std::time::Instant`] 截止点，宿主通过 [`next_deadline`] 安排
//! 唤醒（`TuiAltScreen::render_decision` 到期返回 `Now`），渲染期结算过期条目。可观察行为相同：
//! 条目在到期后的下一帧消失、且**只**产生那一帧（结算不额外补发重绘请求，见
//! [`AltScreenFlashContainer::settle_expired`]）；只是"到期本身"需要宿主的一次渲染/查询来驱动。
//! `flash()` 的插入顺序与 `render()` 的输出顺序保持逐字一致。
//!
//! [`next_deadline`]: AltScreenFlashContainer::next_deadline
//!
//! ## 数据结构与顺序
//!
//! `entries` 是按插入顺序排列的 `Vec`；`render()` 每项输出一行，顺序与 `flash()` 的调用
//! 顺序一致。`TuiAltScreen::composite_flashes` 合成时若条目多于屏幕高度，会保留最新的
//! `height` 条（最早的条目被丢弃），仍从屏幕顶部开始逐行合成。`id`/`next_id` 只为对齐 TS
//! 的 `FlashEntry.id`，惰性结算不读取它。
//!
//! ## 边界
//!
//! - `duration_ms == None` 用默认 1000 ms；`NaN`/`Infinity`、小于 1 ms、大于 `2^31-1 ms`
//!   的取值统一按 1 ms 处理（复刻 Node `setTimeout` 的钳制）。
//! - `Instant::checked_add` 溢出时退回 `now + 2^31-1 ms`；正常输入不会触发。
//! - 到期判定是 `now >= deadline`（`settle_expired` 的 `retain(now < deadline)` 与
//!   `TuiAltScreen::render_decision` 的 `deadline <= now` 一致）：到期的那一帧条目被移除。
//! - 容器持 `Rc<dyn Fn()>`，不是 `Send`/`Sync`，只能在 UI 线程使用。

use std::rc::Rc;
use std::time::{Duration, Instant};

use crate::text::truncate_to_width;
use crate::tui::Component;

/// `DEFAULT_DURATION_MS = 1000`。
const DEFAULT_DURATION_MS: f64 = 1000.0;

/// 一条瞬时消息（TS `FlashEntry`：`{ id, message, timer }`）。
struct FlashEntry {
    /// 条目 id：TS 用它在 `setTimeout` 回调里定位条目；惰性实现按截止时间结算，保留字段以对齐
    /// TS 的数据结构（`spec/tui-core.md` §2.14）。
    #[allow(dead_code, reason = "对齐 TS FlashEntry 结构；惰性结算不需要 id")]
    id: u64,
    message: String,
    /// 到期时刻（`setTimeout` 的等价物）。
    deadline: Instant,
}

/// 瞬时消息容器（TS `AltScreenFlashContainer`）。
///
/// 只负责保存消息与截止时间；渲染、到期唤醒和与屏幕合成都由 [`crate::alt_screen::TuiAltScreen`]
/// 驱动。`flash()` 入队后立即调用一次 `request_render` 请求重绘。
pub struct AltScreenFlashContainer {
    /// 按插入顺序排列；`render()` 的输出顺序与之一致。
    entries: Vec<FlashEntry>,
    /// 单调递增的条目 id（对齐 TS 的 `FlashEntry.id`；惰性结算不依赖它）。
    next_id: u64,
    /// 入队后请求一帧的回调。`TuiAltScreen` 注入的是内部 `requestRender` 句柄：
    /// 同时置 `pending_render`（帧内合并）与 `TuiBase.renderRequested`（宿主唤醒）。
    request_render: Rc<dyn Fn()>,
}

impl AltScreenFlashContainer {
    /// `constructor(requestRender)`。
    ///
    /// `request_render` 由宿主提供并在每次 `flash()` 后调用；容器不保存任何终端状态。
    pub fn new(request_render: Rc<dyn Fn()>) -> Self {
        AltScreenFlashContainer {
            entries: Vec::new(),
            next_id: 0,
            request_render,
        }
    }

    /// `flash(message, durationMs = 1000)`。
    ///
    /// 新条目追加到末尾（后入者在屏幕更下方），并立即请求一帧。`message` 原样保存；
    /// 渲染时再由 `render()` 加上两侧空格、反显并截断到给定宽度。
    ///
    /// TS 的 `Math.max(0, durationMs)` 之后交给 `setTimeout`：非有限值（`NaN`/`Infinity`）与
    /// 落在 `[1, 2^31-1]` 之外的值一律按 1 ms 排程，这里用同样的归一化。
    pub fn flash(&mut self, message: &str, duration_ms: Option<f64>) {
        let duration = duration_ms.unwrap_or(DEFAULT_DURATION_MS);
        let millis = if duration.is_finite() {
            duration.max(0.0)
        } else {
            1.0
        };
        let millis = if (1.0..=2_147_483_647.0).contains(&millis) {
            millis
        } else {
            1.0
        };
        let id = self.next_id;
        self.next_id += 1;
        let deadline = Instant::now()
            .checked_add(Duration::from_millis(millis as u64))
            .unwrap_or_else(|| Instant::now() + Duration::from_millis(2_147_483_647));
        // TS 顺序：先建 timer（unref），再 push，最后 requestRender。惰性实现里"建 timer"
        // 就是算好截止点，顺序不影响结果。
        self.entries.push(FlashEntry {
            id,
            message: message.to_string(),
            deadline,
        });
        (self.request_render)();
    }

    /// `dispose()`：清理所有条目（TS 同时 `clearTimeout`）。
    ///
    /// 只清空 `entries`，不重置 `next_id`（id 只用于对齐 TS，不复用）。
    pub fn dispose(&mut self) {
        self.entries.clear();
    }

    /// 当前条目数（测试/内部使用）。
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 最早一条消息的到期时刻（宿主渲染循环据此安排唤醒；TS 对应 `setTimeout` 的排程）。
    ///
    /// 每次调用遍历全部条目取最小值（O(n)，条目数通常是个位数）；空容器返回 `None`，
    /// [`crate::alt_screen::TuiAltScreen::render_decision`] 此时不会为 flash 安排 `Wait`。
    pub fn next_deadline(&self) -> Option<Instant> {
        self.entries.iter().map(|entry| entry.deadline).min()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 结算到期条目；返回是否有条目被移除。
    ///
    /// 判断是 `now >= deadline`（`retain` 保留 `now < deadline` 的条目）；`render()` 每帧
    /// 先调用它，因此即使宿主绕过 `render_decision` 直接渲染，过期消息也不会被画出来。
    ///
    /// TS 的 `setTimeout` 回调先删除条目、再 `requestRender()`；Rust 没有执行器，宿主通过
    /// [`AltScreenFlashContainer::next_deadline`]（[`crate::alt_screen::TuiAltScreen::render_decision`]）
    /// 得知到期并渲染一帧。渲染期结算**不再**补发重绘请求：当前帧已经反映删除后的结果，
    /// 再请求一帧会多写一次空帧（TS 的定时器回调只排一帧）。宿主若绕过 `render_decision`
    /// 直接渲染，也在本方法所在的这一帧里得到同样的可见结果。
    pub fn settle_expired(&mut self) -> bool {
        let now = Instant::now();
        let before = self.entries.len();
        self.entries.retain(|entry| now < entry.deadline);
        before != self.entries.len()
    }
}

impl Component for AltScreenFlashContainer {
    /// TS 的 `AltScreenFlashContainer` 没有 `handleInput` 成员：作为聚焦的 overlay 时，
    /// `handleTerminalInput` 既不调用它也不请求立即渲染。
    fn handles_input(&self) -> bool {
        false
    }

    /// 无内部缓存需要失效：`render()` 每次都从 `entries` 现算。
    fn invalidate(&mut self) {}

    /// TS `dispose()`：宿主替换/清空组件时清理条目。
    fn dispose(&mut self) {
        AltScreenFlashContainer::dispose(self);
    }

    /// 先结算过期条目，再把每条消息渲染成一行反显文本。
    ///
    /// 每条输出 `message` 两侧各加一个空格、按 `width` 截断（无省略号），再包上
    /// `\x1b[7m…\x1b[27m`（反显/关闭反显）。条目顺序 = 插入顺序。
    fn render(&mut self, width: usize) -> Vec<String> {
        self.settle_expired();
        self.entries
            .iter()
            .map(|entry| {
                let message = truncate_to_width(&format!(" {} ", entry.message), width, "", false);
                format!("\x1b[7m{message}\x1b[27m")
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 评审回归：TS 的 `AltScreenFlashContainer` 没有 `handleInput` 成员，
    /// `handleTerminalInput` 因此不会调用它、也不会请求立即渲染。
    #[test]
    fn flash_container_does_not_handle_input() {
        let container = AltScreenFlashContainer::new(Rc::new(|| {}));
        assert!(!container.handles_input());
    }
}
