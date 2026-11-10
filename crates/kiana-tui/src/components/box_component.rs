//! components/box.ts。
//!
//! 容器组件：给所有子项加左右/上下留白与可选背景色，并缓存渲染结果。
//!
//! # 职责与调用方
//!
//! [`BoxComponent`] 是 TS `Box` 的 Rust 版本（改名的原因见结构体文档）：把子项按加入顺序
//! 自上而下堆叠，每行左侧补 `padding_x` 个空格、顶部/底部各插入 `padding_y` 行空白，再把
//! 整行补齐到 `width` 后交给背景回调着色。工具渲染器与交互层用它包住消息/结果内容
//! （例如 `kiana-tools/src/renderers/edit.rs`、`kiana-interactive/src/components/*`），
//! crate 根另以 `Box` 别名再导出它（见 `crate::index`）。
//!
//! # 一次 render 的流程
//!
//! 1. 没有子项时直接返回空 `Vec`（不改渲染缓存，也不改 `mouse_layout`）。
//! 2. 以 `content_width = max(1, width - 2 * padding_x)` 渲染每个子项，同时把
//!    `(子项地址, 行数)` 写入 `mouse_layout`，供之后的鼠标命中使用。
//! 3. 若子项都没有输出行，返回空 `Vec`（`mouse_layout` 已按上一步写入，渲染缓存不更新）。
//! 4. 用 `bg_fn("test")` 采样背景回调，与缓存中的宽度、采样值和各子项原始行逐项比较；
//!    完全一致则直接返回缓存的最终行。
//! 5. 否则重新拼装：上留白 + （左留白 + 子项行 + 右侧补白）+ 下留白，每一行都调用背景
//!    回调；结果写入缓存。
//!
//! # 缓存与约束
//!
//! - 渲染缓存保存的是**已加留白和背景**的最终行；子项的 `render` 每次仍会执行（子项自己的
//!   动画/状态需要推进），缓存省掉的是留白、补白与背景着色。
//! - 背景函数没有变化通知，缓存靠 `bg_fn("test")` 的采样结果判断是否要重建：采样相同就
//!   认为背景没变。若背景函数只在其它输入上变化而 `"test"` 采样不变，缓存会继续返回旧
//!   背景；`set_bg_fn` 因此不主动清缓存（见该方法文档）。
//! - `mouse_layout` 只在下一次渲染非空子项时整体重建；`add_child` / `remove_child` /
//!   `clear` 只清渲染缓存，不清它。删除子项后留下的缓存项若按地址找不到对应子项，会被跳过。
//!   清空子项后的 `render` 会提前返回，也不会清掉它。
//! - 子项身份用 `&dyn Component` 的数据指针表示（见 `component_id`），因此同一个子项在
//!   整个生命周期里必须保持同一地址；`Box<dyn Component>` 的堆分配满足这一点。

use crate::components::text_component::StyleFn;
use crate::text::{flatten_lines, visible_width};
use crate::tui::{Component, TuiMouseEvent, TuiMouseEventResult, dispatch_mouse_event};

/// 渲染缓存（TS `RenderCache`）。
///
/// `child_lines` 是**未加留白/背景**的子项原始输出，用来判断能否复用 `lines`；`bg_sample`
/// 是 `bg_fn("test")` 的采样结果，用来发现背景函数换过（见模块文档「缓存与约束」）。
struct RenderCache {
    child_lines: Vec<String>,
    width: usize,
    bg_sample: Option<String>,
    lines: Vec<String>,
}

/// 鼠标布局缓存（TS `mouseLayout`）。
///
/// TS 缓存的是 `{ component, height }` 对象引用；Rust 的子项由 `Box` 独占，无法把引用存进
/// 缓存，因此记录组件地址作为稳定身份（与 `tui::Container` 的 `mouse_layout` 同一做法）。
/// 缓存项对应的子项已被移除时，TS 会把事件交给那个**已脱离树**的对象；Rust 里对象已被
/// 释放，只能跳过该缓存项（不会像按下标配对那样误投给另一个仍然存活的子项）。
///
/// `width` 是最近一次 `render` 使用的 `content_width`；`children` 是当时的子项顺序与行数。
/// 该缓存在每次渲染**非空**子项时整体重建（即使渲染缓存命中）。注意 `add_child` 只清渲染
/// 缓存，所以新增子项要等到下一次 `render` 才会写入这里；在那之前，仅当已有缓存、且事件
/// 宽度换算出的 `content_width` 与该缓存的 `width` 相同（`handle_mouse` 直接复用）时，
/// 新子项才收不到鼠标事件。宽度不一致（事件宽度变化，或之后改过 padding）时
/// `handle_mouse` 会现场重渲染所有子项求高度，新子项可能立即被命中。
struct MouseLayout {
    width: usize,
    /// `(子项地址, 渲染高度)`，顺序 == TS 的 `mouseLayout.children`。
    children: Vec<(usize, usize)>,
}

/// 组件地址（指针身份）。
///
/// 取自 `&dyn Component` 的**数据指针**，用于 `remove_child` 与 `mouse_layout` 的匹配。
/// 零尺寸（ZST）组件的 boxed 指针可能是同一个悬挂地址，此时按地址无法区分多个实例。
fn component_id(component: &dyn Component) -> usize {
    component as *const dyn Component as *const () as usize
}

/// 容器组件（TS `class Box`）。
///
/// 名字差异：Rust 的 `Box` 是语言内建类型，无法在同一模块里定义同名结构体而不遮蔽它
/// （子项列表本身就要用 `Box<dyn Component>`），因此命名为 `BoxComponent`；公开导出面
/// 仍可用 `BoxComponent as Box` 别名（见 `crate::index`）。
pub struct BoxComponent {
    /// 子项（TS 的公开字段 `children`）。
    pub children: Vec<Box<dyn Component>>,
    /// 每行左侧的留白列数；宽度预算会左右各扣一份（`width - 2 * padding_x`）。
    padding_x: usize,
    /// 顶部/底部各插入的空行数。
    padding_y: usize,
    /// 整行背景着色函数；`None` 表示只补空格、不加 ANSI。
    bg_fn: Option<StyleFn>,
    /// 上一次渲染的最终行缓存（见模块文档「缓存与约束」）。
    cache: Option<RenderCache>,
    /// 最近一次渲染的子项顺序与行数，用于鼠标命中（见 [`MouseLayout`]）。
    mouse_layout: Option<MouseLayout>,
}

impl Default for BoxComponent {
    fn default() -> Self {
        BoxComponent::new(1, 1, None)
    }
}

impl BoxComponent {
    /// `constructor(paddingX = 1, paddingY = 1, bgFn?)`。
    ///
    /// `padding_x` 会从宽度预算的两侧各扣一份（结果最少 1 列），`padding_y` 是上、下各插入
    /// 的空行数。`Default` 等价于 `new(1, 1, None)`。
    pub fn new(padding_x: usize, padding_y: usize, bg_fn: Option<StyleFn>) -> Self {
        BoxComponent {
            children: Vec::new(),
            padding_x,
            padding_y,
            bg_fn,
            cache: None,
            mouse_layout: None,
        }
    }

    /// `addChild(component)`。
    ///
    /// 追加到末尾（即渲染顺序，也是鼠标命中的自上而下顺序），并丢弃渲染缓存。`mouse_layout`
    /// 不清：新子项要等到下一次 `render` 才会进入鼠标命中表。
    pub fn add_child(&mut self, component: Box<dyn Component>) {
        self.children.push(component);
        self.invalidate_cache();
    }

    /// `removeChild(component)`：按指针比较。
    ///
    /// 只移除第一个地址相同的子项并丢弃渲染缓存；`mouse_layout` 里的旧条目保留到下一次
    /// 渲染非空子项时，在此之前命中该条目会被跳过（见 `MouseLayout` 与模块测试）。
    pub fn remove_child(&mut self, component: &dyn Component) {
        let target = component as *const dyn Component as *const () as usize;
        let index = self.children.iter().position(|child| {
            child.as_ref() as *const dyn Component as *const () as usize == target
        });
        if let Some(index) = index {
            self.children.remove(index);
            self.invalidate_cache();
        }
    }

    /// `clear()`。
    ///
    /// 清空子项与渲染缓存；`mouse_layout` 同样保留到下一次渲染非空子项时（旧条目全部按
    /// 地址落空）。
    pub fn clear(&mut self) {
        self.children.clear();
        self.invalidate_cache();
    }

    /// `setBgFn(bgFn)`：**不**清缓存（TS 通过采样输出检测变化）。
    ///
    /// 是否重建由下一次 `render` 的 `bg_fn("test")` 采样决定：新旧函数在 `"test"` 上输出
    /// 相同就沿用旧行。
    pub fn set_bg_fn(&mut self, bg_fn: Option<StyleFn>) {
        self.bg_fn = bg_fn;
    }

    /// `setPaddingX(paddingX)`。
    ///
    /// 会丢弃渲染缓存。`mouse_layout` 不主动清，但 `handle_mouse` 会按事件宽度和**当前**
    /// padding 重新计算 `content_width`；与缓存宽度不符时现场重渲染子项求高度。
    pub fn set_padding_x(&mut self, padding_x: usize) {
        self.padding_x = padding_x;
        self.invalidate_cache();
    }

    /// 丢弃渲染缓存；`mouse_layout` 不受影响。
    fn invalidate_cache(&mut self) {
        self.cache = None;
    }

    /// 判断缓存能否用于本次输出：宽度、背景采样、子项原始行（逐行比较）都必须一致。
    fn match_cache(&self, width: usize, child_lines: &[String], bg_sample: Option<&str>) -> bool {
        let Some(cache) = &self.cache else {
            return false;
        };
        cache.width == width
            && cache.bg_sample.as_deref() == bg_sample
            && cache.child_lines.len() == child_lines.len()
            && cache
                .child_lines
                .iter()
                .zip(child_lines)
                .all(|(cached, line)| cached == line)
    }

    /// 把一行补齐到 `width` 列后交给背景回调。
    ///
    /// 先按可见宽度（ANSI 序列不计入，见 [`visible_width`]）算需要补的空格，再整体交给
    /// `bg_fn`，因此背景函数拿到的是满宽文本，可以在两端安全地加转义序列。子项输出已经比
    /// `width` 宽时不再补空格（`saturating_sub` 得到 0）。
    fn apply_bg(&self, line: &str, width: usize) -> String {
        let visible_len = visible_width(line);
        let padding_needed = width.saturating_sub(visible_len);
        let padded = format!("{line}{}", " ".repeat(padding_needed));
        match &self.bg_fn {
            Some(bg_fn) => bg_fn(&padded),
            None => padded,
        }
    }
}

impl Component for BoxComponent {
    fn render(&mut self, width: usize) -> Vec<String> {
        if self.children.is_empty() {
            // 无子项时连上下留白也不输出，直接返回空。
            return Vec::new();
        }

        // 宽度预算：左右各扣 padding_x，至少保留 1 列。
        let content_width = (width.saturating_sub(self.padding_x * 2)).max(1);
        let left_pad = " ".repeat(self.padding_x);

        // 每次都重新渲染所有子项：子项可能有动画或自身缓存，不能为了命中缓存而跳过。
        // 行数同时用于重建 mouse_layout。
        let mut child_lines: Vec<String> = Vec::new();
        let mut mouse_children: Vec<(usize, usize)> = Vec::with_capacity(self.children.len());
        for child in self.children.iter_mut() {
            let id = component_id(child.as_ref());
            let lines = child.render(content_width);
            mouse_children.push((id, lines.len()));
            for line in lines {
                child_lines.push(line);
            }
        }
        // 鼠标布局在缓存判断之前更新，保证 handle_mouse 用的总是最近一帧的行数。
        self.mouse_layout = Some(MouseLayout {
            width: content_width,
            children: mouse_children,
        });

        if child_lines.is_empty() {
            // 所有子项都输出 0 行：不生成纯留白块，也不更新渲染缓存。
            return Vec::new();
        }

        // 采样背景函数：它没有变更通知，只能靠固定输入判断输出是否仍与缓存一致。
        let bg_sample = self.bg_fn.as_ref().map(|bg_fn| bg_fn("test"));
        if self.match_cache(width, &child_lines, bg_sample.as_deref()) {
            // 命中：clone 一份，避免调用方改到缓存内容。
            return self.cache.as_ref().expect("cache matched").lines.clone();
        }

        // 上留白 → 子项行（左侧留白；apply_bg 负责右侧补白与背景）→ 下留白。
        let mut result: Vec<String> = Vec::new();
        for _ in 0..self.padding_y {
            result.push(self.apply_bg("", width));
        }
        for line in &child_lines {
            result.push(self.apply_bg(&format!("{left_pad}{line}"), width));
        }
        for _ in 0..self.padding_y {
            result.push(self.apply_bg("", width));
        }

        // TS 的 flattenLines 只是 V8 的字符串扁平化提示；Rust 版是保留签名的 no-op
        // （见 text.rs），这里调用只为与 TS 的结构对齐。
        flatten_lines(&mut result);
        // 缓存同时保存原始子项行与最终行：前者用于下次比较，后者直接返回。
        self.cache = Some(RenderCache {
            child_lines,
            width,
            bg_sample,
            lines: result.clone(),
        });
        result
    }

    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        // 用事件自带的宽度把坐标换算到内容区；落在外侧留白（上/左）或内容区右边界之外时
        // 不命中。
        let content_width = (event.width.saturating_sub(self.padding_x * 2)).max(1);
        let content_y = event.y - self.padding_y as isize;
        let content_x = event.x - self.padding_x as isize;
        if content_y < 0 || content_x < 0 || content_x >= content_width as isize {
            return None;
        }

        // 缓存宽度与当前内容宽度一致才复用；否则现场按 content_width 重渲染子项求高度。
        let mouse_children: Vec<(usize, usize)> = match &self.mouse_layout {
            Some(layout) if layout.width == content_width => layout.children.clone(),
            _ => self
                .children
                .iter_mut()
                .map(|child| {
                    let id = component_id(child.as_ref());
                    (id, child.render(content_width).len())
                })
                .collect(),
        };

        // 按缓存顺序自上而下找第一个高度区间包含 content_y 的子项。
        let mut child_y: isize = 0;
        for (child_id, height) in mouse_children {
            if content_y >= child_y && content_y < child_y + height as isize {
                let Some(index) = self
                    .children
                    .iter()
                    .position(|child| component_id(child.as_ref()) == child_id)
                else {
                    // 缓存项指向已移除的子项（TS 会投递给那个脱离树的对象）：Rust 里对象
                    // 已释放，跳过该项并继续按缓存高度向下寻找。
                    child_y += height as isize;
                    continue;
                };
                let child = &mut self.children[index];
                // 换算成子项局部坐标；screen_x/screen_y 保持原值，备用屏重放要靠它反算。
                let mut child_event = event.clone();
                child_event.x = content_x;
                child_event.y = content_y - child_y;
                child_event.width = content_width;
                child_event.height = height;
                return dispatch_mouse_event(child.as_mut(), &child_event);
            }
            child_y += height as isize;
        }
        None
    }

    fn handles_input(&self) -> bool {
        // TS 的 Box 没有 handleInput；`false` 让 TUI 不把键盘输入交给它。
        false
    }

    /// TS 的 `Box` 不是 `Container`（焦点遍历看不见子项），但它的 `handleMouse` 会转发给
    /// 子项；备用屏的鼠标重放导航需要能下钻到命中的子项。
    fn mouse_children_mut(&mut self) -> Option<&mut Vec<Box<dyn Component>>> {
        Some(&mut self.children)
    }

    fn invalidate(&mut self) {
        // 清自身渲染缓存并递归子项；mouse_layout 保留到下一帧（陈旧条目在命中时跳过）。
        self.invalidate_cache();
        for child in self.children.iter_mut() {
            child.invalidate();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::*;
    use crate::tui::{TuiMouseButton, TuiMouseEventType};

    /// 固定行数、记录鼠标事件的探针。
    struct Rows {
        lines: Vec<String>,
        events: Rc<RefCell<Vec<(isize, usize)>>>,
    }

    impl Component for Rows {
        fn render(&mut self, _width: usize) -> Vec<String> {
            self.lines.clone()
        }

        fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
            self.events.borrow_mut().push((event.y, event.height));
            Some(TuiMouseEventResult {
                handled: Some(true),
                ..TuiMouseEventResult::default()
            })
        }
    }

    fn rows(names: &[&str], events: &Rc<RefCell<Vec<(isize, usize)>>>) -> Box<dyn Component> {
        Box::new(Rows {
            lines: names.iter().map(|name| (*name).to_string()).collect(),
            events: Rc::clone(events),
        })
    }

    fn press_at(y: isize, width: usize, height: usize) -> TuiMouseEvent {
        TuiMouseEvent {
            event_type: TuiMouseEventType::Press,
            button: TuiMouseButton::Left,
            x: 1,
            y,
            screen_x: 1,
            screen_y: y,
            width,
            height,
            shift: false,
            alt: false,
            ctrl: false,
            wheel_delta: None,
            click_count: None,
        }
    }

    /// 回归（minor）：`mouseLayout` 缓存的是渲染时的子项身份，不能把缓存高度按下标
    /// 与**当前**子项列表配对（否则 removeChild 之后事件会误投给下一个存活子项）。
    /// TS 会投递给已脱离树的对象；Rust 对象已释放，因此该项被跳过（与 `Container` 一致）。
    #[test]
    fn stale_mouse_layout_entries_are_skipped_not_repaired_with_live_children() {
        let a_events = Rc::new(RefCell::new(Vec::new()));
        let b_events = Rc::new(RefCell::new(Vec::new()));
        let mut box_ = BoxComponent::new(1, 1, None);
        box_.add_child(rows(&["a1", "a2"], &a_events));
        box_.add_child(rows(&["b1"], &b_events));

        // 以 20 列渲染一次，填充 mouseLayout（contentWidth = 20 - 2 = 18）。
        assert_eq!(box_.render(20).len(), 5); // 上留白 + 3 行子项 + 下留白

        // 移除 A（与 TS 的 removeChild 一样只清渲染缓存，不清 mouseLayout）。
        let removed = box_.children.remove(0);
        drop(removed);

        // content_y = 1 原本落在已移除的 A 的第 2 行（TS 投递给脱离树的 A）。
        let event = press_at(2, 20, 5);
        let result = box_.handle_mouse(&event);
        assert!(
            result.is_none(),
            "must not dispatch to a live child at a stale slot"
        );
        assert!(
            b_events.borrow().is_empty(),
            "stale entry must not be re-paired with the surviving child: {:?}",
            b_events.borrow()
        );

        // content_y = 2 仍按缓存高度落在 B 上（y=0，height=1）。
        let event = press_at(3, 20, 5);
        assert!(box_.handle_mouse(&event).is_some());
        assert_eq!(b_events.borrow().as_slice(), [(0, 1)]);
    }
}
