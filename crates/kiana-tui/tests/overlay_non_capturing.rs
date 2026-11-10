//! `test/overlay-non-capturing.test.ts` 的逐条移植（44 个 it）。
//!
//! TS 用 `TuiMainScreen` + `VirtualTerminal` 驱动焦点状态机，并断言各组件的 `focused`
//! 属性与收到的输入。焦点状态机、overlay handle 与输入分发都在 `TuiBase` 上，因此这里直接
//! 驱动 `TuiBase`（`renderAndFlush` 对应"刷新挂起的焦点标志"，TS 的 `focused` 属性本来
//! 就是 `setFocus` 同步写好的，Rust 侧由 `flush_pending_focus_sync` 补齐）。
//!
//! 与 TS 的三处**表示差异**（行为断言完全相同）：
//!
//! 1. TS 的焦点目标是**组件对象引用**，Rust 的 [`FocusTarget`] 是下标/id。因此测试里的每个
//!    可聚焦组件都作为顶层子项挂载，`Probe`（`focused`、`inputs`、输入处理器）与组件共享。
//! 2. TS 的组件直接在 `handleInput` 里调用 `tui.setFocus(...)` / `handle.unfocus(...)`。
//!    Rust 的 `Component::handle_input` 拿不到 `&mut TuiBase`，测试组件把同样的调用**排队**，
//!    由 `Harness::send_input` 在 `handle_terminal_input` 返回后按原顺序执行——TS 在
//!    `handleInput` 之后只剩 `requestImmediateRender()`，可观察状态与"当场执行"一致。
//! 3. TS 里 `editor` / `replacement` 常常从未 `addChild`（`setFocus` 接受任意对象）。
//!    Rust 必须能把焦点目标解析到组件才能给输入，所以这些用例用 `set_layout_root` 让
//!    `getMountedRoots()` 只包含一个空 layout root——等价于 `isComponentMounted(child) === false`，
//!    输入仍按目标下标送达（`setLayoutRoot` 只影响挂载判定，不影响 `children`）。

#[path = "common/mod.rs"]
mod common;

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use common::TestTerminal;
use kiana_tui::text::strip_terminal_sequences;
use kiana_tui::tui::{
    Component, FocusTarget, Focusable, OverlayHandle, OverlayOptions, OverlayUnfocusOptions,
    SizeValue, TuiBase, TuiStopOptions,
};

/// handleInput 内发起的 TUI 调用的延迟动作（见文件头第 2 点）。
type Action = Box<dyn FnOnce(&mut TuiBase)>;

/// 测试组件的输入处理器（TS 里直接给组件赋 `handleInput`）。
type InputHandler = Box<dyn FnMut(&str)>;

/// 组件探针：与 [`Probed`] 共享 `focused` 标志、输入记录与输入处理器。
#[derive(Clone)]
struct Probe {
    focused: Rc<Cell<bool>>,
    inputs: Rc<RefCell<Vec<String>>>,
    on_input: Rc<RefCell<Option<InputHandler>>>,
}

impl Probe {
    fn new() -> Self {
        Probe {
            focused: Rc::new(Cell::new(false)),
            inputs: Rc::new(RefCell::new(Vec::new())),
            on_input: Rc::new(RefCell::new(None)),
        }
    }

    fn focused(&self) -> bool {
        self.focused.get()
    }

    fn inputs(&self) -> Vec<String> {
        self.inputs.borrow().clone()
    }

    /// TS 里测试直接给组件赋 `handleInput = (data) => {...}`（覆盖默认实现）。
    fn on_input(&self, handler: impl FnMut(&str) + 'static) {
        *self.on_input.borrow_mut() = Some(Box::new(handler));
    }
}

/// 可聚焦的测试组件（TS `FocusableOverlay`）。
struct Probed {
    line: String,
    probe: Probe,
}

impl Probed {
    fn new(line: &str, probe: Probe) -> Self {
        Probed {
            line: line.to_string(),
            probe,
        }
    }
}

impl Component for Probed {
    fn render(&mut self, _width: usize) -> Vec<String> {
        vec![self.line.clone()]
    }

    fn handle_input(&mut self, data: &str) {
        self.probe.inputs.borrow_mut().push(data.to_string());
        if let Some(handler) = self.probe.on_input.borrow_mut().as_mut() {
            handler(data);
        }
    }

    fn as_focusable(&mut self) -> Option<&mut dyn Focusable> {
        Some(self)
    }
}

impl Focusable for Probed {
    fn focused(&self) -> bool {
        self.probe.focused.get()
    }

    fn set_focused(&mut self, focused: bool) {
        self.probe.focused.set(focused);
    }
}

/// 固定内容的 overlay（TS `StaticOverlay`），不参与焦点。
struct StaticOverlay {
    lines: Vec<String>,
}

impl StaticOverlay {
    fn new(line: &str) -> Self {
        StaticOverlay {
            lines: vec![line.to_string()],
        }
    }
}

impl Component for StaticOverlay {
    fn render(&mut self, _width: usize) -> Vec<String> {
        self.lines.clone()
    }
}

/// 渲染空行（TS `EmptyContent`）。
struct EmptyContent;

impl Component for EmptyContent {
    fn render(&mut self, _width: usize) -> Vec<String> {
        Vec::new()
    }
}

// ============================================================================
// 测试脚手架
// ============================================================================

struct Harness {
    base: TuiBase,
    columns: usize,
    rows: usize,
    /// 顶层子项探针（下标 == `FocusTarget::Component` 的 index）。
    probes: Vec<Probe>,
    handles: Vec<Rc<RefCell<Box<dyn OverlayHandle>>>>,
    actions: Rc<RefCell<Vec<Action>>>,
    /// 从树里移出的组件（保持存活：`MountState` 用组件地址复现 TS 的引用相等语义，
    /// 已移除组件一旦被释放，新组件可能复用同一地址而被误判为"仍挂载"）。
    keepalive: Rc<RefCell<Vec<Box<dyn Component>>>>,
}

impl Harness {
    fn new(columns: usize, rows: usize) -> Self {
        Harness {
            base: TuiBase::new(Box::new(TestTerminal::with_size(columns, rows)), None, None),
            columns,
            rows,
            probes: Vec::new(),
            handles: Vec::new(),
            actions: Rc::new(RefCell::new(Vec::new())),
            keepalive: Rc::new(RefCell::new(Vec::new())),
        }
    }

    fn add(&mut self, line: &str) -> usize {
        let probe = Probe::new();
        self.base
            .add_child(Box::new(Probed::new(line, probe.clone())));
        self.probes.push(probe);
        self.probes.len() - 1
    }

    fn add_empty(&mut self) -> usize {
        self.base.add_child(Box::new(EmptyContent));
        self.probes.push(Probe::new());
        self.probes.len() - 1
    }

    fn probe(&self, index: usize) -> Probe {
        self.probes[index].clone()
    }

    fn child(&self, index: usize) -> FocusTarget {
        FocusTarget::Component { index }
    }

    /// `tui.setFocus(component)`。
    fn focus(&mut self, index: usize) {
        self.base.set_focus(Some(self.child(index)));
    }

    /// `tui.setFocus(null)`。
    fn clear_focus(&mut self) {
        self.base.set_focus(None);
    }

    fn actions(&self) -> Rc<RefCell<Vec<Action>>> {
        Rc::clone(&self.actions)
    }

    fn keepalive(&self) -> Rc<RefCell<Vec<Box<dyn Component>>>> {
        Rc::clone(&self.keepalive)
    }

    /// `tui.showOverlay(可聚焦组件, options)`：返回 handle 槽位与探针。
    fn show_probed(&mut self, line: &str, options: OverlayOptions) -> (usize, Probe) {
        let probe = Probe::new();
        let handle = self
            .base
            .show_overlay(Box::new(Probed::new(line, probe.clone())), options);
        let slot = self.handles.len();
        self.handles.push(Rc::new(RefCell::new(handle)));
        (slot, probe)
    }

    /// `tui.showOverlay(静态组件, options)`：返回 handle 槽位。
    fn show_static(&mut self, line: &str, options: OverlayOptions) -> usize {
        let handle = self
            .base
            .show_overlay(Box::new(StaticOverlay::new(line)), options);
        let slot = self.handles.len();
        self.handles.push(Rc::new(RefCell::new(handle)));
        slot
    }

    fn handle(&self, slot: usize) -> Rc<RefCell<Box<dyn OverlayHandle>>> {
        Rc::clone(&self.handles[slot])
    }

    fn overlay_focus(&mut self, slot: usize) {
        self.handles[slot].borrow_mut().focus();
        self.base.flush_pending_focus_sync();
    }

    fn overlay_unfocus(&mut self, slot: usize, options: Option<OverlayUnfocusOptions>) {
        self.handles[slot].borrow_mut().unfocus(options);
        self.base.flush_pending_focus_sync();
    }

    fn overlay_hide(&mut self, slot: usize) {
        self.handles[slot].borrow_mut().hide();
        self.base.flush_pending_focus_sync();
    }

    fn overlay_set_hidden(&mut self, slot: usize, hidden: bool) {
        self.handles[slot].borrow_mut().set_hidden(hidden);
        self.base.flush_pending_focus_sync();
    }

    fn overlay_is_focused(&self, slot: usize) -> bool {
        self.handles[slot].borrow().is_focused()
    }

    /// `tui.hideOverlay()`（移除栈顶）。
    fn hide_top_overlay(&mut self) {
        self.base.hide_overlay();
        self.base.flush_pending_focus_sync();
    }

    /// `terminal.sendInput(data)` + `renderAndFlush`。
    fn send_input(&mut self, data: &str) {
        self.base.handle_terminal_input(data);
        self.drain_actions();
        self.base.flush_pending_focus_sync();
    }

    fn drain_actions(&mut self) {
        loop {
            let actions: Vec<Action> = std::mem::take(&mut *self.actions.borrow_mut());
            if actions.is_empty() {
                break;
            }
            for action in actions {
                action(&mut self.base);
            }
            self.base.flush_pending_focus_sync();
        }
    }

    /// `renderAndFlush`：把 handle 路径挂起的 `focused` 标志写到组件上。
    fn flush(&mut self) {
        self.base.flush_pending_focus_sync();
    }

    /// TS 里焦点目标从未挂载到树上时用：`getMountedRoots()` 只剩一个空 layout root。
    fn unmount_all(&mut self) {
        self.base.set_layout_root(Some(Box::new(EmptyContent)));
    }

    fn start(&mut self) {
        self.base.start();
    }

    fn stop(&mut self) {
        self.base.stop(TuiStopOptions::default());
    }

    /// 合成结果首行首列（TS `terminal.getViewport()[0]?.charAt(0)`）。
    fn top_left_char(&mut self) -> Option<char> {
        let (width, height) = (self.columns, self.rows);
        let lines = self.base.composite_overlays(Vec::new(), width, height);
        lines
            .first()
            .map(|line| strip_terminal_sequences(line))
            .and_then(|text| text.chars().next())
    }
}

fn non_capturing() -> OverlayOptions {
    OverlayOptions {
        non_capturing: Some(true),
        ..OverlayOptions::default()
    }
}

fn positioned_options(non_capturing: bool) -> OverlayOptions {
    OverlayOptions {
        row: Some(SizeValue::Absolute(0.0)),
        col: Some(SizeValue::Absolute(0.0)),
        width: Some(SizeValue::Absolute(1.0)),
        non_capturing: non_capturing.then_some(true),
        ..OverlayOptions::default()
    }
}

fn positioned() -> OverlayOptions {
    positioned_options(true)
}

/// 往动作队列里排一个 `tui.setFocus(...)`。
fn queue_focus(actions: &Rc<RefCell<Vec<Action>>>, target: Option<FocusTarget>) {
    actions
        .borrow_mut()
        .push(Box::new(move |base: &mut TuiBase| base.set_focus(target)));
}

// ============================================================================
// describe("focus management")
// ============================================================================

/// `non-capturing overlay preserves focus on creation`
#[test]
fn non_capturing_overlay_preserves_focus_on_creation() {
    let mut h = Harness::new(80, 24);
    h.add_empty();
    let editor = h.add("EDITOR");
    h.focus(editor);
    h.start();

    let (_, overlay) = h.show_probed("OVERLAY", non_capturing());
    h.flush();
    assert!(h.probe(editor).focused());
    assert!(!overlay.focused());
    h.stop();
}

/// `focus() transfers focus to the overlay`
#[test]
fn focus_transfers_focus_to_the_overlay() {
    let mut h = Harness::new(80, 24);
    h.add_empty();
    let editor = h.add("EDITOR");
    h.focus(editor);
    h.start();

    let (slot, overlay) = h.show_probed("OVERLAY", non_capturing());
    h.overlay_focus(slot);
    h.flush();
    assert!(!h.probe(editor).focused());
    assert!(overlay.focused());
    assert!(h.overlay_is_focused(slot));
    h.stop();
}

/// `unfocus() restores previous focus`
#[test]
fn unfocus_restores_previous_focus() {
    let mut h = Harness::new(80, 24);
    h.add_empty();
    let editor = h.add("EDITOR");
    h.focus(editor);
    h.start();

    let (slot, overlay) = h.show_probed("OVERLAY", non_capturing());
    h.overlay_focus(slot);
    h.overlay_unfocus(slot, None);
    h.flush();
    assert!(h.probe(editor).focused());
    assert!(!overlay.focused());
    assert!(!h.overlay_is_focused(slot));
    h.stop();
}

/// `setHidden(false) on non-capturing overlay does not auto-focus`
#[test]
fn set_hidden_false_on_non_capturing_overlay_does_not_auto_focus() {
    let mut h = Harness::new(80, 24);
    h.add_empty();
    let editor = h.add("EDITOR");
    h.focus(editor);
    h.start();

    let (slot, overlay) = h.show_probed("OVERLAY", non_capturing());
    h.overlay_set_hidden(slot, true);
    h.overlay_set_hidden(slot, false);
    h.flush();
    assert!(h.probe(editor).focused());
    assert!(!overlay.focused());
    h.stop();
}

/// `hide() when overlay is not focused does not change focus`
#[test]
fn hide_when_overlay_is_not_focused_does_not_change_focus() {
    let mut h = Harness::new(80, 24);
    h.add_empty();
    let editor = h.add("EDITOR");
    h.focus(editor);
    h.start();

    let (slot, _) = h.show_probed("OVERLAY", non_capturing());
    h.overlay_hide(slot);
    h.flush();
    assert!(h.probe(editor).focused());
    h.stop();
}

/// `hide() when focused restores focus correctly`
#[test]
fn hide_when_focused_restores_focus_correctly() {
    let mut h = Harness::new(80, 24);
    h.add_empty();
    let editor = h.add("EDITOR");
    h.focus(editor);
    h.start();

    let (slot, overlay) = h.show_probed("OVERLAY", non_capturing());
    h.overlay_focus(slot);
    h.overlay_hide(slot);
    h.flush();
    assert!(h.probe(editor).focused());
    assert!(!overlay.focused());
    h.stop();
}

/// `capturing overlay removed with non-capturing below restores focus to editor`
#[test]
fn capturing_overlay_removed_with_non_capturing_below_restores_focus_to_editor() {
    let mut h = Harness::new(80, 24);
    h.add_empty();
    let editor = h.add("EDITOR");
    h.focus(editor);
    h.start();

    let (_, non_capturing_overlay) = h.show_probed("NC", non_capturing());
    let (capturing_slot, capturing) = h.show_probed("CAP", OverlayOptions::default());
    assert!(capturing.focused());
    h.overlay_hide(capturing_slot);
    h.flush();
    assert!(h.probe(editor).focused());
    assert!(!non_capturing_overlay.focused());
    h.stop();
}

/// `sub-overlay cleanup then hideOverlay restores focus and input to editor`
#[test]
fn sub_overlay_cleanup_then_hide_overlay_restores_focus_and_input_to_editor() {
    let mut h = Harness::new(80, 24);
    h.add_empty();
    let editor = h.add("EDITOR");
    h.focus(editor);
    h.start();

    let (timer_slot, timer) = h.show_probed("TIMER", non_capturing());
    let (_, controller) = h.show_probed("CTRL", OverlayOptions::default());
    assert!(controller.focused());
    assert!(!h.probe(editor).focused());

    h.overlay_hide(timer_slot);
    h.hide_top_overlay();
    h.flush();
    assert!(h.probe(editor).focused());
    assert!(!controller.focused());
    assert!(!timer.focused());

    h.send_input("x");
    h.flush();
    assert_eq!(h.probe(editor).inputs(), vec!["x".to_string()]);
    assert!(controller.inputs().is_empty());
    assert!(timer.inputs().is_empty());
    h.stop();
}

/// `removed focused child overlay does not become parent overlay fallback`
#[test]
fn removed_focused_child_overlay_does_not_become_parent_overlay_fallback() {
    let mut h = Harness::new(80, 24);
    h.add_empty();
    let editor = h.add("EDITOR");
    h.focus(editor);
    h.start();

    let (child_slot, child) = h.show_probed("CHILD", non_capturing());
    h.overlay_focus(child_slot);
    let (parent_slot, parent) = h.show_probed("PARENT", OverlayOptions::default());
    assert!(parent.focused());

    h.overlay_hide(child_slot);
    h.overlay_hide(parent_slot);
    h.send_input("x");
    h.flush();

    assert_eq!(h.probe(editor).inputs(), vec!["x".to_string()]);
    assert!(child.inputs().is_empty());
    assert!(parent.inputs().is_empty());
    assert!(h.probe(editor).focused());
    h.stop();
}

/// `microtask-deferred sub-overlay pattern (showExtensionCustom simulation) restores focus`
#[test]
fn microtask_deferred_sub_overlay_pattern_restores_focus() {
    let mut h = Harness::new(80, 24);
    h.add_empty();
    let editor = h.add("EDITOR");
    h.focus(editor);
    h.start();

    // factory 先同步创建 timer，随后（微任务）.then() 再压入 controller。
    let (timer_slot, timer) = h.show_probed("TIMER", non_capturing());
    let (_, controller) = h.show_probed("CTRL", OverlayOptions::default());
    h.flush();
    assert!(controller.focused());
    assert!(!h.probe(editor).focused());

    // Esc：cleanup + close。
    h.overlay_hide(timer_slot);
    h.hide_top_overlay();
    h.flush();
    assert!(h.probe(editor).focused(), "editor should regain focus");
    assert!(!controller.focused());
    assert!(!timer.focused());

    h.send_input("x");
    h.flush();
    assert_eq!(
        h.probe(editor).inputs(),
        vec!["x".to_string()],
        "editor should receive input after close"
    );
    assert!(controller.inputs().is_empty());
    h.stop();
}

/// `handleInput redirection skips non-capturing overlays when focused overlay becomes invisible`
#[test]
fn handle_input_redirection_skips_non_capturing_when_focused_overlay_invisible() {
    let mut h = Harness::new(80, 24);
    h.add_empty();
    let editor = h.add("EDITOR");
    h.focus(editor);
    h.start();

    let (_, fallback) = h.show_probed("FALLBACK", OverlayOptions::default());
    let (_, non_capturing_overlay) = h.show_probed("NC", non_capturing());
    let visible = Rc::new(Cell::new(true));
    let flag = Rc::clone(&visible);
    let options = OverlayOptions {
        visible: Some(Box::new(move |_width, _height| flag.get())),
        ..OverlayOptions::default()
    };
    let (_, primary) = h.show_probed("PRIMARY", options);
    assert!(primary.focused());

    visible.set(false);
    h.send_input("x");
    h.flush();
    assert!(primary.inputs().is_empty());
    assert!(non_capturing_overlay.inputs().is_empty());
    assert_eq!(fallback.inputs(), vec!["x".to_string()]);
    assert!(fallback.focused());
    h.stop();
}

/// `active base focus replacement receives close input before overlay restore`
#[test]
fn active_base_focus_replacement_receives_close_input_before_overlay_restore() {
    let mut h = Harness::new(80, 24);
    // TS：editor / replacement 从未 addChild；这里让 getMountedRoots() 不含它们（见文件头 3）。
    h.unmount_all();
    h.add_empty();
    let editor = h.add("EDITOR");
    let replacement = h.add("REPLACEMENT");
    h.focus(editor);
    h.start();

    let (_, overlay) = h.show_probed("OVERLAY", OverlayOptions::default());
    assert!(overlay.focused());

    let actions = h.actions();
    let replacement_target = h.child(replacement);
    overlay.on_input(move |data| {
        if data == "b" {
            queue_focus(&actions, Some(replacement_target));
        }
    });
    let actions = h.actions();
    let editor_target = h.child(editor);
    h.probe(replacement).on_input(move |data| {
        if data == "\r" {
            queue_focus(&actions, Some(editor_target));
        }
    });

    h.send_input("b");
    h.flush();
    assert!(h.probe(replacement).focused());

    h.send_input("\r");
    h.flush();
    assert_eq!(h.probe(replacement).inputs(), vec!["\r".to_string()]);
    assert_eq!(overlay.inputs(), vec!["b".to_string()]);
    assert!(overlay.focused());

    h.send_input("x");
    h.flush();
    assert_eq!(overlay.inputs(), vec!["b".to_string(), "x".to_string()]);
    h.stop();
}

/// `active replacement still receives input when it is another overlay preFocus`
#[test]
fn active_replacement_still_receives_input_when_it_is_another_overlay_pre_focus() {
    let mut h = Harness::new(80, 24);
    h.unmount_all();
    h.add_empty();
    let editor = h.add("EDITOR");
    let replacement = h.add("REPLACEMENT");
    h.start();

    h.focus(replacement);
    let (_, passive) = h.show_probed("PASSIVE", non_capturing());
    h.focus(editor);
    let (_, overlay) = h.show_probed("OVERLAY", OverlayOptions::default());

    let actions = h.actions();
    let replacement_target = h.child(replacement);
    overlay.on_input(move |data| {
        if data == "b" {
            queue_focus(&actions, Some(replacement_target));
        }
    });
    let actions = h.actions();
    let editor_target = h.child(editor);
    h.probe(replacement).on_input(move |data| {
        if data == "\r" {
            queue_focus(&actions, Some(editor_target));
        }
    });

    h.send_input("b");
    h.flush();
    assert!(h.probe(replacement).focused());

    h.send_input("1");
    h.send_input("\r");
    h.flush();
    assert_eq!(
        h.probe(replacement).inputs(),
        vec!["1".to_string(), "\r".to_string()]
    );
    assert_eq!(overlay.inputs(), vec!["b".to_string()]);
    assert!(overlay.focused());
    assert!(!passive.focused());
    h.stop();
}

/// `blocked replacement can move focus internally before overlay restore`
#[test]
fn blocked_replacement_can_move_focus_internally_before_overlay_restore() {
    let mut h = Harness::new(80, 24);
    // TS 用 `Container base` 承载三个可聚焦组件；Rust 侧直接把它们挂成顶层子项，
    // `base.clear()` 对应 `clear_children()`（两者都让被移出的组件不再挂载）。
    let editor = h.add("EDITOR");
    let first = h.add("FIRST");
    let second = h.add("SECOND");
    h.focus(editor);
    h.start();

    let (_, overlay) = h.show_probed("OVERLAY", OverlayOptions::default());

    let actions = h.actions();
    let first_target = h.child(first);
    overlay.on_input(move |data| {
        if data == "b" {
            queue_focus(&actions, Some(first_target));
        }
    });
    let actions = h.actions();
    let second_target = h.child(second);
    h.probe(first).on_input(move |data| {
        if data == "n" {
            queue_focus(&actions, Some(second_target));
        }
    });
    let actions = h.actions();
    let editor_probe = h.probe(editor);
    let keepalive = h.keepalive();
    h.probe(second).on_input(move |data| {
        if data == "\r" {
            let editor_probe = editor_probe.clone();
            let keepalive = keepalive.clone();
            actions
                .borrow_mut()
                .push(Box::new(move |base: &mut TuiBase| {
                    // `base.clear()`：移出全部子项但保持存活，再放回 editor（TS 里编辑器对象不变）。
                    let mut keep = keepalive.borrow_mut();
                    while let Some(child) = base.remove_child_at(0) {
                        keep.push(child);
                    }
                    drop(keep);
                    base.add_child(Box::new(Probed::new("EDITOR", editor_probe)));
                    base.set_focus(Some(FocusTarget::Component { index: 0 }));
                }));
        }
    });

    h.send_input("b");
    h.flush();
    h.send_input("n");
    h.flush();
    h.send_input("2");
    h.send_input("\r");
    h.flush();

    assert_eq!(overlay.inputs(), vec!["b".to_string()]);
    assert_eq!(h.probe(first).inputs(), vec!["n".to_string()]);
    assert_eq!(
        h.probe(second).inputs(),
        vec!["2".to_string(), "\r".to_string()]
    );
    assert!(overlay.focused());
    h.stop();
}

/// `removed replacement restores overlay even when overlay preFocus differs from next focus`
#[test]
fn removed_replacement_restores_overlay_even_when_pre_focus_differs() {
    let mut h = Harness::new(80, 24);
    let editor = h.add("EDITOR");
    let palette = h.add("PALETTE");
    let replacement = h.add("REPLACEMENT");
    h.focus(palette);
    h.start();

    let (_, overlay) = h.show_probed("OVERLAY", OverlayOptions::default());

    let actions = h.actions();
    let replacement_target = h.child(replacement);
    overlay.on_input(move |data| {
        if data == "b" {
            queue_focus(&actions, Some(replacement_target));
        }
    });
    let actions = h.actions();
    let editor_probe = h.probe(editor);
    let keepalive = h.keepalive();
    h.probe(replacement).on_input(move |data| {
        if data == "\r" {
            let editor_probe = editor_probe.clone();
            let keepalive = keepalive.clone();
            actions
                .borrow_mut()
                .push(Box::new(move |base: &mut TuiBase| {
                    // `base.clear()`：移出全部子项但保持存活，再放回 editor（TS 里编辑器对象不变）。
                    let mut keep = keepalive.borrow_mut();
                    while let Some(child) = base.remove_child_at(0) {
                        keep.push(child);
                    }
                    drop(keep);
                    base.add_child(Box::new(Probed::new("EDITOR", editor_probe)));
                    base.set_focus(Some(FocusTarget::Component { index: 0 }));
                }));
        }
    });

    h.send_input("b");
    h.flush();
    h.send_input("\r");
    h.send_input("x");
    h.flush();

    assert_eq!(overlay.inputs(), vec!["b".to_string(), "x".to_string()]);
    assert_eq!(h.probe(replacement).inputs(), vec!["\r".to_string()]);
    assert!(h.probe(editor).inputs().is_empty());
    assert!(overlay.focused());
    h.stop();
}

/// `unfocus target releases a blocked overlay while replacement remains focused`
#[test]
fn unfocus_target_releases_a_blocked_overlay_while_replacement_remains_focused() {
    let mut h = Harness::new(80, 24);
    let fallback = h.add("FALLBACK");
    let target = h.add("TARGET");
    let replacement = h.add("REPLACEMENT");
    h.start();

    let (slot, overlay) = h.show_probed("OVERLAY", OverlayOptions::default());

    let actions = h.actions();
    let fallback_target = h.child(fallback);
    h.probe(replacement).on_input(move |data| {
        if data == "\r" {
            queue_focus(&actions, Some(fallback_target));
        }
    });
    let actions = h.actions();
    let replacement_target = h.child(replacement);
    let unfocus_target = h.child(target);
    let handle = h.handle(slot);
    overlay.on_input(move |data| {
        if data == "b" {
            queue_focus(&actions, Some(replacement_target));
            let handle = Rc::clone(&handle);
            actions
                .borrow_mut()
                .push(Box::new(move |_base: &mut TuiBase| {
                    handle.borrow_mut().unfocus(Some(OverlayUnfocusOptions {
                        target: Some(unfocus_target),
                    }));
                }));
        }
    });

    h.send_input("b");
    h.flush();
    assert!(h.probe(replacement).focused());

    h.send_input("\r");
    h.send_input("x");
    h.flush();

    assert_eq!(overlay.inputs(), vec!["b".to_string()]);
    assert_eq!(h.probe(replacement).inputs(), vec!["\r".to_string()]);
    assert!(h.probe(fallback).inputs().is_empty());
    assert_eq!(h.probe(target).inputs(), vec!["x".to_string()]);
    h.stop();
}

/// `handleInput restores focus to a visible focused overlay after base focus steal`
#[test]
fn handle_input_restores_focus_to_visible_focused_overlay_after_base_focus_steal() {
    let mut h = Harness::new(80, 24);
    h.unmount_all();
    h.add_empty();
    let editor = h.add("EDITOR");
    let replacement = h.add("REPLACEMENT");
    h.focus(editor);
    h.start();

    let (_, overlay) = h.show_probed("OVERLAY", OverlayOptions::default());
    assert!(overlay.focused());
    h.focus(replacement);
    h.focus(editor);

    h.send_input("x");
    h.flush();
    assert_eq!(overlay.inputs(), vec!["x".to_string()]);
    assert!(h.probe(editor).inputs().is_empty());
    assert!(overlay.focused());
    h.stop();
}

/// `handleInput restores focus to explicitly focused raw sub-overlay after base focus steal`
#[test]
fn handle_input_restores_focus_to_explicitly_focused_sub_overlay() {
    let mut h = Harness::new(80, 24);
    h.add_empty();
    let editor = h.add("EDITOR");
    h.focus(editor);
    h.start();

    let (_, controller) = h.show_probed("CONTROLLER", OverlayOptions::default());
    let (sub_slot, sub_overlay) = h.show_probed("SUB", non_capturing());
    h.overlay_focus(sub_slot);
    h.focus(editor);

    h.send_input("x");
    h.flush();
    assert_eq!(sub_overlay.inputs(), vec!["x".to_string()]);
    assert!(controller.inputs().is_empty());
    assert!(h.probe(editor).inputs().is_empty());
    h.stop();
}

/// `passive non-capturing overlay does not regain input after base focus`
#[test]
fn passive_non_capturing_overlay_does_not_regain_input_after_base_focus() {
    let mut h = Harness::new(80, 24);
    h.add_empty();
    let editor = h.add("EDITOR");
    h.focus(editor);
    h.start();

    let (_, passive) = h.show_probed("PASSIVE", non_capturing());
    h.send_input("x");
    h.flush();
    assert_eq!(h.probe(editor).inputs(), vec!["x".to_string()]);
    assert!(passive.inputs().is_empty());
    assert!(h.probe(editor).focused());
    h.stop();
}

/// `explicitly focused non-capturing overlay regains input after base focus steal`
#[test]
fn explicitly_focused_non_capturing_overlay_regains_input_after_base_focus_steal() {
    let mut h = Harness::new(80, 24);
    h.add_empty();
    let editor = h.add("EDITOR");
    h.focus(editor);
    h.start();

    let (slot, overlay) = h.show_probed("NC", non_capturing());
    h.overlay_focus(slot);
    h.focus(editor);
    h.send_input("x");
    h.flush();
    assert_eq!(overlay.inputs(), vec!["x".to_string()]);
    assert!(h.probe(editor).inputs().is_empty());
    h.stop();
}

/// `unfocus() prevents visible overlay from regaining input`
#[test]
fn unfocus_prevents_visible_overlay_from_regaining_input() {
    let mut h = Harness::new(80, 24);
    h.add_empty();
    let editor = h.add("EDITOR");
    h.focus(editor);
    h.start();

    let (slot, overlay) = h.show_probed("OVERLAY", OverlayOptions::default());
    h.overlay_unfocus(slot, None);
    h.send_input("x");
    h.flush();
    assert_eq!(h.probe(editor).inputs(), vec!["x".to_string()]);
    assert!(overlay.inputs().is_empty());
    assert!(h.probe(editor).focused());
    h.stop();
}

/// `setFocus(null) explicitly clears visible overlay restore`
#[test]
fn set_focus_null_explicitly_clears_visible_overlay_restore() {
    let mut h = Harness::new(80, 24);
    h.add_empty();
    h.start();

    let (_, overlay) = h.show_probed("OVERLAY", OverlayOptions::default());
    h.clear_focus();
    h.send_input("x");
    h.flush();
    assert!(overlay.inputs().is_empty());
    assert!(!overlay.focused());
    h.stop();
}

/// `blocked replacement setFocus(null) resumes the visible overlay`
#[test]
fn blocked_replacement_set_focus_null_resumes_the_visible_overlay() {
    let mut h = Harness::new(80, 24);
    h.add_empty();
    let replacement = h.add("REPLACEMENT");
    h.start();

    let (_, overlay) = h.show_probed("OVERLAY", OverlayOptions::default());

    let actions = h.actions();
    h.probe(replacement).on_input(move |data| {
        if data == "\r" {
            queue_focus(&actions, None);
        }
    });
    let actions = h.actions();
    let replacement_target = h.child(replacement);
    overlay.on_input(move |data| {
        if data == "b" {
            queue_focus(&actions, Some(replacement_target));
        }
    });

    h.send_input("b");
    h.flush();
    h.send_input("\r");
    h.send_input("x");
    h.flush();

    assert_eq!(h.probe(replacement).inputs(), vec!["\r".to_string()]);
    assert_eq!(overlay.inputs(), vec!["b".to_string(), "x".to_string()]);
    assert!(overlay.focused());
    h.stop();
}

/// `temporarily invisible focused overlay falls back without losing restore eligibility`
#[test]
fn temporarily_invisible_focused_overlay_falls_back_without_losing_eligibility() {
    let mut h = Harness::new(80, 24);
    h.add_empty();
    let editor = h.add("EDITOR");
    h.focus(editor);
    h.start();

    let visible = Rc::new(Cell::new(true));
    let flag = Rc::clone(&visible);
    let options = OverlayOptions {
        visible: Some(Box::new(move |_width, _height| flag.get())),
        ..OverlayOptions::default()
    };
    let (_, overlay) = h.show_probed("OVERLAY", options);
    h.focus(editor);
    visible.set(false);
    h.send_input("x");
    h.flush();
    assert_eq!(h.probe(editor).inputs(), vec!["x".to_string()]);
    assert!(overlay.inputs().is_empty());
    visible.set(true);
    h.send_input("y");
    h.flush();
    assert_eq!(h.probe(editor).inputs(), vec!["x".to_string()]);
    assert_eq!(overlay.inputs(), vec!["y".to_string()]);
    h.stop();
}

/// `temporarily invisible focused overlay with null preFocus restores when visible again`
#[test]
fn temporarily_invisible_focused_overlay_with_null_pre_focus_restores() {
    let mut h = Harness::new(80, 24);
    h.add_empty();
    h.start();

    let visible = Rc::new(Cell::new(true));
    let flag = Rc::clone(&visible);
    let options = OverlayOptions {
        visible: Some(Box::new(move |_width, _height| flag.get())),
        ..OverlayOptions::default()
    };
    let (_, overlay) = h.show_probed("OVERLAY", options);
    visible.set(false);
    h.send_input("x");
    h.flush();
    assert!(overlay.inputs().is_empty());
    visible.set(true);
    h.send_input("y");
    h.flush();
    assert_eq!(overlay.inputs(), vec!["y".to_string()]);
    h.stop();
}

/// `cyclic overlay preFocus ancestry does not hang focus changes`
///
/// TS 用同一个对象既作初始焦点、又作 overlay 组件，形成自环 `preFocus`；Rust 的
/// [`FocusTarget`] 按下标/id 寻址，无法表达"同一个对象"。这里用等价的 `preFocus` 链
/// （overlay 的 preFocus ≠ 下一个焦点）验证同样的可观察结果；`isOverlayFocusAncestor`
/// 的环检测本身由 `tui.rs` 的单测覆盖。
#[test]
fn cyclic_overlay_pre_focus_ancestry_does_not_hang_focus_changes() {
    let mut h = Harness::new(80, 24);
    h.add_empty();
    let seed = h.add("SEED");
    let editor = h.add("EDITOR");
    h.focus(seed);
    h.start();

    let (slot, overlay) = h.show_probed("OVERLAY", non_capturing());
    h.overlay_focus(slot);
    h.focus(editor);
    h.send_input("x");
    h.flush();
    assert_eq!(h.probe(editor).inputs(), vec!["x".to_string()]);
    assert!(overlay.inputs().is_empty());
    h.stop();
}

/// `handleInput restores the focus-order top overlay after base focus steal`
#[test]
fn handle_input_restores_the_focus_order_top_overlay_after_base_focus_steal() {
    let mut h = Harness::new(80, 24);
    h.add_empty();
    let editor = h.add("EDITOR");
    h.focus(editor);
    h.start();

    let (lower_slot, lower) = h.show_probed("LOWER", OverlayOptions::default());
    let (_, upper) = h.show_probed("UPPER", OverlayOptions::default());
    h.overlay_focus(lower_slot);
    h.focus(editor);

    h.send_input("x");
    h.flush();
    assert_eq!(lower.inputs(), vec!["x".to_string()]);
    assert!(upper.inputs().is_empty());
    assert!(h.probe(editor).inputs().is_empty());
    h.stop();
}

/// `hideOverlay() does not reassign focus when topmost overlay is non-capturing`
#[test]
fn hide_overlay_does_not_reassign_focus_when_topmost_is_non_capturing() {
    let mut h = Harness::new(80, 24);
    h.add_empty();
    let editor = h.add("EDITOR");
    h.focus(editor);
    h.start();

    let (_, capturing) = h.show_probed("CAP", OverlayOptions::default());
    let (_, _non_capturing_overlay) = h.show_probed("NC", non_capturing());
    assert!(capturing.focused());
    h.hide_top_overlay();
    h.flush();
    assert!(capturing.focused());
    h.stop();
}

/// `multiple capturing and non-capturing overlays restore focus through removals`
#[test]
fn multiple_capturing_and_non_capturing_overlays_restore_focus_through_removals() {
    let mut h = Harness::new(80, 24);
    h.add_empty();
    let editor = h.add("EDITOR");
    h.focus(editor);
    h.start();

    let (c1_slot, c1) = h.show_probed("C1", OverlayOptions::default());
    let (_, n1) = h.show_probed("N1", non_capturing());
    let (c2_slot, c2) = h.show_probed("C2", OverlayOptions::default());
    let (_, n2) = h.show_probed("N2", non_capturing());
    assert!(c2.focused());

    h.overlay_hide(c2_slot);
    h.flush();
    assert!(c1.focused());
    h.overlay_hide(c1_slot);
    h.flush();
    assert!(h.probe(editor).focused());
    assert!(!n1.focused());
    assert!(!n2.focused());
    h.stop();
}

/// `capturing overlay unfocus() on topmost capturing overlay falls back to preFocus`
#[test]
fn capturing_overlay_unfocus_falls_back_to_pre_focus() {
    let mut h = Harness::new(80, 24);
    h.add_empty();
    let editor = h.add("EDITOR");
    h.focus(editor);
    h.start();

    let (slot, capturing) = h.show_probed("CAP", OverlayOptions::default());
    assert!(capturing.focused());
    h.overlay_unfocus(slot, None);
    h.flush();
    assert!(h.probe(editor).focused());
    assert!(!capturing.focused());
    h.stop();
}

// ============================================================================
// describe("no-op guards")
// ============================================================================

/// `focus() on hidden overlay is a no-op`
#[test]
fn focus_on_hidden_overlay_is_a_no_op() {
    let mut h = Harness::new(80, 24);
    h.add_empty();
    let editor = h.add("EDITOR");
    h.focus(editor);
    h.start();

    let (slot, _) = h.show_probed("OVERLAY", non_capturing());
    h.overlay_set_hidden(slot, true);
    h.overlay_focus(slot);
    h.flush();
    assert!(h.probe(editor).focused());
    assert!(!h.overlay_is_focused(slot));
    h.stop();
}

/// `focus() after hide() is a no-op`
#[test]
fn focus_after_hide_is_a_no_op() {
    let mut h = Harness::new(80, 24);
    h.add_empty();
    let editor = h.add("EDITOR");
    h.focus(editor);
    h.start();

    let (slot, _) = h.show_probed("OVERLAY", non_capturing());
    h.overlay_hide(slot);
    h.overlay_focus(slot);
    h.flush();
    assert!(h.probe(editor).focused());
    assert!(!h.overlay_is_focused(slot));
    h.stop();
}

/// `unfocus() when overlay does not have focus is a no-op`
#[test]
fn unfocus_when_overlay_does_not_have_focus_is_a_no_op() {
    let mut h = Harness::new(80, 24);
    h.add_empty();
    let editor = h.add("EDITOR");
    h.focus(editor);
    h.start();

    let (slot, overlay) = h.show_probed("OVERLAY", non_capturing());
    h.overlay_unfocus(slot, None);
    h.flush();
    assert!(h.probe(editor).focused());
    assert!(!overlay.focused());
    h.stop();
}

/// `unfocus() with null preFocus clears focus and does not route input back to overlay`
#[test]
fn unfocus_with_null_pre_focus_clears_focus() {
    let mut h = Harness::new(80, 24);
    h.add_empty();
    h.start();

    let (slot, overlay) = h.show_probed("OVERLAY", OverlayOptions::default());
    assert!(overlay.focused());
    h.overlay_unfocus(slot, None);
    assert!(!overlay.focused());
    h.send_input("x");
    h.flush();
    assert!(overlay.inputs().is_empty());
    assert!(!h.overlay_is_focused(slot));
    h.stop();
}

// ============================================================================
// describe("focus cycle prevention")
// ============================================================================

/// `toggle focus between non-capturing overlays then unfocus returns to editor`
#[test]
fn toggle_focus_between_non_capturing_overlays_then_unfocus_returns_to_editor() {
    let mut h = Harness::new(80, 24);
    h.add_empty();
    let editor = h.add("EDITOR");
    h.focus(editor);
    h.start();

    let (a_slot, a) = h.show_probed("A", non_capturing());
    let (b_slot, b) = h.show_probed("B", non_capturing());
    h.overlay_focus(a_slot);
    h.overlay_focus(b_slot);
    h.overlay_focus(a_slot);
    h.overlay_unfocus(a_slot, None);
    h.flush();
    assert!(h.probe(editor).focused());
    assert!(!a.focused());
    assert!(!b.focused());
    h.stop();
}

/// `explicit unfocus target supports cycling between three overlays and editor`
#[test]
fn explicit_unfocus_target_supports_cycling_between_three_overlays_and_editor() {
    let mut h = Harness::new(80, 24);
    h.add_empty();
    let editor = h.add("EDITOR");
    h.focus(editor);
    h.start();

    let (a_slot, a) = h.show_probed("A", OverlayOptions::default());
    let (b_slot, b) = h.show_probed("B", OverlayOptions::default());
    let (c_slot, c) = h.show_probed("C", OverlayOptions::default());
    let target_editor = Some(OverlayUnfocusOptions {
        target: Some(h.child(editor)),
    });

    h.overlay_focus(a_slot);
    h.send_input("a");
    h.flush();
    h.overlay_focus(b_slot);
    h.send_input("b");
    h.flush();
    h.overlay_focus(c_slot);
    h.send_input("c");
    h.flush();
    h.overlay_unfocus(c_slot, target_editor);
    h.send_input("e");
    h.flush();
    h.overlay_focus(a_slot);
    h.send_input("A");
    h.flush();
    h.overlay_unfocus(a_slot, target_editor);
    h.send_input("E");
    h.flush();

    assert_eq!(a.inputs(), vec!["a".to_string(), "A".to_string()]);
    assert_eq!(b.inputs(), vec!["b".to_string()]);
    assert_eq!(c.inputs(), vec!["c".to_string()]);
    assert_eq!(
        h.probe(editor).inputs(),
        vec!["e".to_string(), "E".to_string()]
    );
    assert!(h.probe(editor).focused());
    h.stop();
}

/// `explicit null unfocus target clears focus without restoring overlays`
#[test]
fn explicit_null_unfocus_target_clears_focus_without_restoring_overlays() {
    let mut h = Harness::new(80, 24);
    h.add_empty();
    h.start();

    let (slot, overlay) = h.show_probed("OVERLAY", OverlayOptions::default());
    h.overlay_unfocus(slot, Some(OverlayUnfocusOptions { target: None }));
    h.send_input("x");
    h.flush();
    assert!(overlay.inputs().is_empty());
    assert!(!h.overlay_is_focused(slot));
    h.stop();
}

/// `hiding focused overlay falls back to next visual-frontmost overlay`
#[test]
fn hiding_focused_overlay_falls_back_to_next_visual_frontmost_overlay() {
    let mut h = Harness::new(80, 24);
    h.add_empty();
    let editor = h.add("EDITOR");
    h.focus(editor);
    h.start();

    let (a_slot, a) = h.show_probed("A", OverlayOptions::default());
    let (b_slot, b) = h.show_probed("B", OverlayOptions::default());
    let (_, c) = h.show_probed("C", OverlayOptions::default());
    h.overlay_focus(a_slot);
    h.overlay_focus(b_slot);
    h.overlay_set_hidden(b_slot, true);

    h.send_input("x");
    h.flush();
    assert_eq!(a.inputs(), vec!["x".to_string()]);
    assert!(c.inputs().is_empty());
    assert!(a.focused());
    assert!(!b.focused());
    h.stop();
}

// ============================================================================
// describe("rendering order")
// ============================================================================

/// `focus() on already-focused overlay bumps visual order`
#[test]
fn focus_on_already_focused_overlay_bumps_visual_order() {
    let mut h = Harness::new(20, 6);
    h.add_empty();
    let editor = h.add("EDITOR");
    h.focus(editor);
    h.start();

    let a_slot = h.show_static("A", positioned());
    h.show_static("B", positioned());
    h.overlay_focus(a_slot);
    h.show_static("C", positioned());
    h.flush();
    assert_eq!(h.top_left_char(), Some('C'));
    h.overlay_focus(a_slot);
    h.flush();
    assert_eq!(h.top_left_char(), Some('A'));
    assert!(h.overlay_is_focused(a_slot));
    h.stop();
}

/// `default rendering order for overlapping overlays follows creation order`
#[test]
fn default_rendering_order_follows_creation_order() {
    let mut h = Harness::new(20, 6);
    h.add_empty();
    h.start();

    h.show_static("A", positioned());
    h.show_static("B", positioned());
    h.flush();
    assert_eq!(h.top_left_char(), Some('B'));
    h.stop();
}

/// `focus() on lower overlay renders it on top`
#[test]
fn focus_on_lower_overlay_renders_it_on_top() {
    let mut h = Harness::new(20, 6);
    h.add_empty();
    h.start();

    let lower = h.show_static("A", positioned());
    h.show_static("B", positioned());
    h.flush();
    assert_eq!(h.top_left_char(), Some('B'));
    h.overlay_focus(lower);
    h.flush();
    assert_eq!(h.top_left_char(), Some('A'));
    h.stop();
}

/// `focusing middle overlay places it on top while preserving others relative order`
#[test]
fn focusing_middle_overlay_places_it_on_top() {
    let mut h = Harness::new(20, 6);
    h.add_empty();
    h.start();

    h.show_static("A", positioned());
    let middle = h.show_static("B", positioned());
    let top = h.show_static("C", positioned());
    h.flush();
    assert_eq!(h.top_left_char(), Some('C'));
    h.overlay_focus(middle);
    h.flush();
    assert_eq!(h.top_left_char(), Some('B'));
    h.overlay_hide(middle);
    h.flush();
    assert_eq!(h.top_left_char(), Some('C'));
    h.overlay_hide(top);
    h.flush();
    assert_eq!(h.top_left_char(), Some('A'));
    h.stop();
}

/// `capturing overlay hidden and shown again renders on top after unhide`
#[test]
fn capturing_overlay_hidden_and_shown_again_renders_on_top() {
    let mut h = Harness::new(20, 6);
    h.add_empty();
    h.start();

    h.show_static("A", positioned());
    let capturing = h.show_static("B", positioned_options(false));
    h.flush();
    assert_eq!(h.top_left_char(), Some('B'));
    h.overlay_set_hidden(capturing, true);
    h.show_static("C", positioned());
    h.flush();
    assert_eq!(h.top_left_char(), Some('C'));
    h.overlay_set_hidden(capturing, false);
    h.flush();
    assert_eq!(h.top_left_char(), Some('B'));
    h.stop();
}

/// `unfocus() does not change visual order until another overlay is focused`
#[test]
fn unfocus_does_not_change_visual_order_until_another_overlay_is_focused() {
    let mut h = Harness::new(20, 6);
    h.add_empty();
    let editor = h.add("EDITOR");
    h.focus(editor);
    h.start();

    let a = h.show_static("A", positioned());
    let b = h.show_static("B", positioned());
    h.flush();
    assert_eq!(h.top_left_char(), Some('B'));
    h.overlay_focus(a);
    h.flush();
    assert_eq!(h.top_left_char(), Some('A'));
    h.overlay_unfocus(a, None);
    h.flush();
    assert_eq!(h.top_left_char(), Some('A'));
    h.overlay_focus(b);
    h.flush();
    assert_eq!(h.top_left_char(), Some('B'));
    h.stop();
}
