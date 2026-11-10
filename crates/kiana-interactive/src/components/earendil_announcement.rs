//! components/earendil-announcement.ts。
//!
//! 公告卡片：`kiana has joined Earendil` + 博客链接 + 可选的 `clankolas.png`。
//!
//! TS 源：`packages/coding-agent/src/modes/interactive/components/earendil-announcement.ts`。
//!
//! ## 与 TS 的差异
//!
//! * 图片不再运行时读文件：`kiana-interactive/assets/clankolas.png` 用 `include_bytes!` 编进
//!   二进制（字节与 TS 仓库一致），见 [`crate::assets`]。因此 TS 的「文件缺失 → 不渲染图片」
//!   分支不可能发生；组件仍按 `Option` 形状组织，保留该分支的代码路径。
//! * TS 的模块级缓存（`cachedImageBase64` / `attemptedImageLoad`）落在 [`crate::assets`] 的
//!   `OnceLock` 上（base64 只编码一次）。
//! * `Container` 是 Rust 的组合成员；`render` / `handle_mouse` / `container_children` 转发，
//!   以便焦点与鼠标遍历（TS 里 `EarendilAnnouncementComponent extends Container`）。
//!
//! ## 被谁调用
//!
//! `/dementedelves` 彩蛋命令（`interactive_mode/commands/misc.rs`）：把整个组件追加到
//! chat 容器，一次性渲染后随转录滚动。也是 `components/index.rs` barrel 的公开再导出面
//! 之一。没有输入处理、没有状态更新——所有内容在 [`EarendilAnnouncementComponent::new`]
//! 里一次构造完成，之后不会改变。
//!
//! ## 执行流程（构造）
//!
//! 按顺序装入内部 `Container`：上边框 → 标题行（Accent 色加粗）→ `Spacer` →
//! 「Read the blog post:」提示（Muted）→ 博客链接（MdLink 色）→ `Spacer` →
//! [`Image`]（仅当 base64 拿得到，当前恒有）→ `Spacer` → 下边框。构造即着色：
//! `fg` / `bold` 在主题未初始化时 panic（与 TS 的抛错行为对齐），因此调用方必须
//! 先初始化主题。
//!
//! ## 注意事项
//!
//! * 图片宽度上限 56 列（`max_width_cells`）；终端不支持图片协议时由 [`Image`] 自己的
//!   回退逻辑处理（Muted 色文本），本组件不做二次判断。
//! * [`EarendilAnnouncementComponent::has_image`] 恒为 `true`（内置资源不会加载失败）；`Option` 形状只是 TS 类型对齐，
//!   不要依赖它区分运行环境。

use std::sync::Arc;

use kiana_theme::ThemeColor;
use kiana_tui::components::image::{Image, ImageOptions, ImageTheme};
use kiana_tui::components::spacer::Spacer;
use kiana_tui::components::text_component::Text;
use kiana_tui::tui::{Component, Container, TuiMouseEvent, TuiMouseEventResult};
use kiana_ui_kit::dynamic_border::DynamicBorder;

use crate::assets::{CLANKOLAS_PNG_FILENAME, load_clankolas_base64};

/// `BLOG_URL`（**【逐字】**）。
pub const BLOG_URL: &str = "https://mariozechner.at/posts/2026-04-08-ive-sold-out/";
/// `IMAGE_FILENAME`。只用于 [`Image`] 的**文本兜底**显示名（可被 `~/` 压缩、支持超链接
/// 时包成 OSC 8 链接），并非磁盘路径；图片协议分支不用它。
pub const IMAGE_FILENAME: &str = CLANKOLAS_PNG_FILENAME;

fn fg(color: ThemeColor, text: &str) -> String {
    kiana_theme::THEME
        .fg(color, text)
        .expect("theme must be initialized before rendering (TS throws here)")
}

fn bold(text: &str) -> String {
    kiana_theme::THEME
        .bold(text)
        .expect("theme must be initialized before rendering (TS throws here)")
}

/// `kiana has joined Earendil` 公告（TS `class EarendilAnnouncementComponent extends Container`）。
pub struct EarendilAnnouncementComponent {
    container: Container,
    /// 是否成功加载了图片（TS `imageBase64` 的真假）。
    has_image: bool,
}

impl Default for EarendilAnnouncementComponent {
    fn default() -> Self {
        Self::new()
    }
}

impl EarendilAnnouncementComponent {
    /// TS `constructor()`。
    pub fn new() -> Self {
        let mut container = Container::new();
        container.add_child(Box::new(DynamicBorder::from_fn(|text| {
            fg(ThemeColor::Accent, text)
        })));
        container.add_child(Box::new(Text::with_padding(
            bold(&fg(ThemeColor::Accent, "kiana has joined Earendil")),
            1,
            0,
        )));
        container.add_child(Box::new(Spacer::with_lines(1.0)));
        container.add_child(Box::new(Text::with_padding(
            fg(ThemeColor::Muted, "Read the blog post:"),
            1,
            0,
        )));
        container.add_child(Box::new(Text::with_padding(
            fg(ThemeColor::MdLink, BLOG_URL),
            1,
            0,
        )));
        container.add_child(Box::new(Spacer::with_lines(1.0)));

        let image_base64 = load_clankolas_base64();
        let has_image = image_base64.is_some();
        if let Some(image_base64) = image_base64 {
            container.add_child(Box::new(Image::new(
                image_base64,
                "image/png",
                ImageTheme {
                    fallback_color: Arc::new(|text: &str| fg(ThemeColor::Muted, text)),
                },
                ImageOptions {
                    max_width_cells: Some(56.0),
                    filename: Some(IMAGE_FILENAME.to_string()),
                    ..Default::default()
                },
                None,
            )));
            container.add_child(Box::new(Spacer::with_lines(1.0)));
        }

        container.add_child(Box::new(DynamicBorder::from_fn(|text| {
            fg(ThemeColor::Accent, text)
        })));

        EarendilAnnouncementComponent {
            container,
            has_image,
        }
    }

    /// 是否渲染了图片（测试/宿主查询；TS 里是构造时的私有分支）。
    ///
    /// 本实现下恒为 `true`：`load_clankolas_base64` 永远返回 `Some`（见模块文档
    /// 「与 TS 的差异」）。
    pub fn has_image(&self) -> bool {
        self.has_image
    }

    /// 子组件数量（测试用）。
    ///
    /// 有图时为 9（边框、标题、Spacer×3、提示、链接、图片），无图时为 8；测试
    /// `tests/earendil_announcement.rs` 用来锁定图片分支确实生效。
    pub fn children_len(&self) -> usize {
        self.container.children.len()
    }
}

impl Component for EarendilAnnouncementComponent {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.container.render(width)
    }

    /// TS `Container.handleMouse`。
    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        self.container.handle_mouse(event)
    }

    /// TS `Container`：容器没有 `handleInput`。
    fn handles_input(&self) -> bool {
        false
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
