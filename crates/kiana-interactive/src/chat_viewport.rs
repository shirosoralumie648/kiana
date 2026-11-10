//! modes/interactive/chat-viewport.ts。
//!
//! 全屏模式共享的 transcript + 固定输入 dock 布局。
//!
//! # 这个模块解决什么问题
//! TS 的全屏交互界面由「可滚动的消息记录（transcript）」加一条始终贴在底部的输入区
//! （dock：待发送消息、状态行、编辑器、扩展 widget、页脚）组成。本模块只负责把调用方
//! 已经创建好的组件按固定顺序组装成 [`VStack`] 树，不拥有、也不驱动这些组件。
//!
//! # 布局结构
//! ```text
//! root (VStack)
//! ├─ transcript (ScrollView，basis 0 / grow 1 / shrink 1 / minSize 1)
//! └─ dock (VStack，不给 basis / grow 0 / shrink 1 / minSize 1，高度取内容)
//!    ├─ pending_messages
//!    ├─ status
//!    ├─ widgets_above   (可选)
//!    ├─ editor          (minSize 3，其余 dock 子项为 0)
//!    ├─ widgets_below   (可选)
//!    └─ footer
//! ```
//! transcript 吃掉剩余高度；dock 按子项内容高度排列，空间不足时按 `shrink` 压缩。
//! dock 的子项顺序就是 TS 的 `addChild` 顺序：widgets 插在 status 之后 / editor 之后，
//! 分别决定它们显示在编辑器上方还是下方。
//!
//! # 所有权
//! 所有子项都以 [`SharedComponent`]（`Rc<RefCell<dyn Component>>`）传入并挂进布局，
//! 调用方仍保留句柄：启动代码会再把 `transcript` 存进交互模式，供滚动定位使用。因此这里
//! 不能假设自己是组件的唯一持有者。
//!
//! # 边界与失败
//! 构造 transcript 时传入的 [`ScrollViewOptions`] 只有 `axis` 可能非法，而本模块不设置
//! `axis`（保持 `None`），所以 `ScrollView::from_shared` 的 `UnsupportedAxis` 分支不可达，
//! 代码里用 `expect` 表达这一点。

use std::cell::RefCell;
use std::rc::Rc;

use kiana_tui::components::scroll_view::{
    ScrollView, ScrollViewFollow, ScrollViewOptions, ScrollViewScrollbar,
};
use kiana_tui::components::text_component::StyleFn;
use kiana_tui::components::v_stack::{StackChild, StackEntryOptions, StackOptions, VStack};
use kiana_tui::layout_node::{Overscroll, SharedComponent};

/// `ChatViewportOptions`。
///
/// 组件以 [`SharedComponent`]（`Rc<RefCell<dyn Component>>`）传入：TS 里容器保存的是组件
/// 引用，Rust 需要共享句柄才能同时挂进布局并让宿主继续持有。
pub struct ChatViewportOptions {
    /// transcript 的内容。
    pub document: SharedComponent,
    /// 待发送消息区域。
    pub pending_messages: SharedComponent,
    /// 状态行。
    pub status: SharedComponent,
    /// 编辑器。
    pub editor: SharedComponent,
    /// 页脚。
    pub footer: SharedComponent,
    /// 编辑器上方的扩展 widget。
    pub widgets_above: Option<SharedComponent>,
    /// 编辑器下方的扩展 widget。
    pub widgets_below: Option<SharedComponent>,
    /// 滚动条策略；缺省 `"auto"`。
    pub scrollbar: Option<ScrollViewScrollbar>,
    /// 滚动条轨道样式。
    pub scrollbar_track_style: Option<StyleFn>,
    /// 滚动条滑块样式。
    pub scrollbar_thumb_style: Option<StyleFn>,
}

/// `ChatViewport`。
pub struct ChatViewport {
    /// 整体布局（transcript + dock）。
    pub root: VStack,
    /// transcript（与 `root` 里挂载的是同一个滚动视图）。
    pub transcript: Rc<RefCell<ScrollView>>,
}

/// 带布局选项的共享子项。
fn shared_entry(component: SharedComponent, options: StackEntryOptions) -> StackChild {
    StackChild::SharedEntry(component, options)
}

/// `shrink: 1, minSize: N`。
fn shrink_options(min_size: f64) -> StackEntryOptions {
    StackEntryOptions {
        shrink: Some(1.0),
        min_size: Some(min_size),
        ..Default::default()
    }
}

/// `createChatViewport(options)`。
///
/// 组装顺序见模块文档的布局图。返回值同时暴露 [`ChatViewport::root`]（交给备用屏布局）
/// 与 [`ChatViewport::transcript`]（同一个滚动视图的强句柄，供滚动定位/读取滚动条）。
///
/// - `options.scrollbar` 为 `None` 时用 [`ScrollViewScrollbar::Auto`]；
/// - transcript 固定 `follow = End`（新内容到达时跟随末尾）、`primary = true`
///   （作为布局器的主滚动视图）与 `overscroll = Chain`；
/// - 只有 editor 的 `min_size` 是 3，其余 dock 子项为 0。
///
/// 不返回 `Result`：如前所述，本模块生成的滚动选项不可能非法。
pub fn create_chat_viewport(options: ChatViewportOptions) -> ChatViewport {
    let transcript = Rc::new(RefCell::new(
        ScrollView::from_shared(
            Rc::clone(&options.document),
            ScrollViewOptions {
                follow: Some(ScrollViewFollow::End),
                primary: Some(true),
                overscroll: Some(Overscroll::Chain),
                scrollbar: Some(options.scrollbar.unwrap_or(ScrollViewScrollbar::Auto)),
                scrollbar_track_style: options.scrollbar_track_style,
                scrollbar_thumb_style: options.scrollbar_thumb_style,
                ..Default::default()
            },
        )
        .expect("ScrollView options are valid"),
    ));

    // dock 子项顺序即垂直排列顺序：widgets_above 一定排在 editor 之前，
    // widgets_below 一定排在其后；两者都缺席时 dock 就是 4 个固定子项。
    let mut dock_children = vec![
        shared_entry(options.pending_messages, shrink_options(0.0)),
        shared_entry(options.status, shrink_options(0.0)),
    ];
    if let Some(widgets_above) = options.widgets_above {
        dock_children.push(shared_entry(widgets_above, shrink_options(0.0)));
    }
    // editor 的 minSize 是 3，其余是 0。
    dock_children.push(shared_entry(options.editor, shrink_options(3.0)));
    if let Some(widgets_below) = options.widgets_below {
        dock_children.push(shared_entry(widgets_below, shrink_options(0.0)));
    }
    dock_children.push(shared_entry(options.footer, shrink_options(0.0)));

    let dock = VStack::new(dock_children, StackOptions::default());
    let dock_shared: SharedComponent = Rc::new(RefCell::new(dock));
    let transcript_shared: SharedComponent = Rc::clone(&transcript) as SharedComponent;

    // transcript 用 basis 0 + grow 1 占据剩余高度；dock 不给 basis（TS 的 `basis: "auto"`），
    // 由布局器按内容测量其高度，空间不足时只压缩（shrink 1）到 minSize 1。
    let root = VStack::new(
        vec![
            shared_entry(
                transcript_shared,
                StackEntryOptions {
                    basis: Some(0.0),
                    grow: Some(1.0),
                    shrink: Some(1.0),
                    min_size: Some(1.0),
                    ..Default::default()
                },
            ),
            shared_entry(
                dock_shared,
                StackEntryOptions {
                    // TS `basis: "auto"`：Rust 里对应 `None`（不写入条目）。
                    grow: Some(0.0),
                    shrink: Some(1.0),
                    min_size: Some(1.0),
                    ..Default::default()
                },
            ),
        ],
        StackOptions::default(),
    );

    ChatViewport { root, transcript }
}

impl ChatViewport {
    /// transcript 的滚动条策略（TS 测试直接读 `transcript.scrollbar`）。
    pub fn scrollbar(&self) -> ScrollViewScrollbar {
        self.transcript.borrow().scrollbar()
    }
}
