//! components/image.ts。
//!
//! 图片组件：按终端能力输出 Kitty/iTerm2 图片序列；不支持时输出文本兜底。
//! Kitty 协议只接受 PNG，非 PNG 图片经可注册的转码器转换（LRU 缓存上限 32）。
//!
//! # 职责与调用方
//!
//! [`Image`] 是 TUI 里显示图片内容的组件。渲染时先查 [`get_capabilities`] 报告的图片协议：
//! Kitty 输出 `\x1b_G…` 传输序列，iTerm2 输出 OSC 1337 `File=` 序列；终端不支持图片、数据
//! 为空或转换失败时退化为一行文本（[`image_fallback`]，如
//! `[Image: foo.png [image/png] 800x600]`），并可用主题色着色。工具执行结果与公告组件用它
//! 显示图片（例如 `kiana-interactive/src/components/tool_execution.rs`）。
//!
//! # 渲染流程（`Component::render`）
//!
//! 1. **缓存**：`cached_width == width` 时直接返回上次的行；能力或转码器变化不会自动失效，
//!    必须调用 `Component::invalidate`（模块测试即如此）。
//! 2. **预算**：`max_width = max(1, min(width - 2, options.max_width_cells.unwrap_or(60)))`；
//!    默认高度上限 = `ceil(max_width * 单元格宽 / 单元格高)`，让默认裁剪框在像素上近似方形。
//! 3. **转码**：Kitty 只接受 PNG。MIME 不是 `image/png` 时尝试 [`ImageTranscoder`]；成功则
//!    改用 PNG 头里解析出的尺寸（转码可能应用 EXIF 旋转）。没有转码器或转码失败时数据为
//!    `None`，走文本兜底。
//! 4. **编码**：Kitty 首次走图片路径时用 [`allocate_image_id`] 申请图片 ID（也可由
//!    [`ImageOptions::image_id`] 指定以复用同一张图）；`render_image` 以
//!    `move_cursor: Some(false)` 对应 Kitty 的 `C=1`（不让终端移动光标），所以序列放在第一
//!    行，其余 `rows - 1` 行是空占位行。iTerm2 先放 `rows - 1` 个空行，最后一行以光标上移
//!    前缀 + 序列结束。
//! 5. **兜底与缓存**：序列行或兜底行按宽度缓存。
//!
//! # 全局状态与约束
//!
//! - 转码器与 PNG 缓存都放在 `thread_local!`（对应 TS 的模块级变量），只在安装它的线程可见。
//! - PNG 缓存以 **base64 数据本身** 为键（不含 MIME），命中会把条目移到队尾；插入超过 32
//!   条时淘汰最旧的；转码失败（`None`）也会被缓存。`set_image_transcoder` 替换转码器时
//!   清空缓存，便于换实现后重试。
//! - 每个 [`Image`] 自己还保存成功的 `png_data`；失败不写入该字段，注册新转码器并
//!   `invalidate()` 后会再次尝试。
//! - 图片行是含 ESC 序列的单个“行”；[`crate::text::visible_width`] 会把 Kitty 的
//!   `ESC _ … ESC \` 与 iTerm2 的 OSC 1337 序列视为零宽，不能用来得到图片实际占用的列数。
//!   列数由协议参数决定（Kitty 的 `c=`、iTerm2 的 `width=`；`get_kitty_image_metadata`
//!   可从注册表读回 Kitty 的 `columns`，`layout.rs` 用同一元数据的 `rows` 做纵向裁剪）。
//!   消费时先用 `is_image_line` 判断，不要按普通文本切分：[`crate::tui::composite_tui_line`]
//!   只在**基行**是图片行时原样返回基行（图片作为 overlay 时仍会走拼接路径）。

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use crate::components::text_component::StyleFn;
use crate::terminal_image::{
    ImageDimensions, ImageProtocol, ImageRenderOptions, allocate_image_id, get_capabilities,
    get_cell_dimensions, get_image_dimensions, get_png_dimensions, image_fallback, render_image,
};
use crate::text::truncate_to_width;
use crate::tui::Component;

/// 非 PNG 图片的转码器（TS `ImageTranscoder`）。
///
/// 参数是 `(base64 数据, MIME)`，返回 base64 编码的 PNG；`None` 表示无法转换。只在
/// “Kitty + MIME 不是 `image/png`”时调用，结果按 base64 数据缓存（见模块文档）。
pub type ImageTranscoder = Arc<dyn Fn(&str, &str) -> Option<String> + Send + Sync>;

thread_local! {
    /// TS 模块级 `let imageTranscoder`。
    static IMAGE_TRANSCODER: RefCell<Option<ImageTranscoder>> = const { RefCell::new(None) };
    /// TS 模块级 `const pngCache = new Map<string, string | null>()`（LRU，上限 32）。
    static PNG_CACHE: RefCell<PngCache> = RefCell::new(PngCache::default());
}

/// PNG 转码结果缓存（对应 TS 模块级 `pngCache`）。
///
/// 键是 base64 数据（不含 MIME），值 `None` 表示上次转码失败。`order` 维护 LRU 顺序，
/// 插入后超过 32 条时淘汰最旧的一条。
#[derive(Default)]
struct PngCache {
    map: HashMap<String, Option<String>>,
    /// `Map` 的插入顺序（delete + set 会移到末尾）。
    /// `get` 命中同样会移到末尾，因此实际维护的是 LRU 顺序。
    order: VecDeque<String>,
}

impl PngCache {
    /// 读取并提升为最新（LRU）；外层 `None` 表示键不存在，内层是缓存值。
    fn get(&mut self, key: &str) -> Option<Option<String>> {
        let value = self.map.get(key).cloned()?;
        // 命中后移动到最新。
        if let Some(position) = self.order.iter().position(|entry| entry == key) {
            self.order.remove(position);
        }
        self.order.push_back(key.to_string());
        Some(value)
    }

    /// 写入/更新并提升为最新；缓存超过 32 条时淘汰最旧的一条。
    fn set(&mut self, key: String, value: Option<String>) {
        if self.map.insert(key.clone(), value).is_none() {
            self.order.push_back(key);
        } else if let Some(position) = self.order.iter().position(|entry| *entry == key) {
            self.order.remove(position);
            self.order.push_back(key);
        }
        if self.map.len() > 32
            && let Some(oldest) = self.order.pop_front()
        {
            self.map.remove(&oldest);
        }
    }

    /// 清空缓存（替换转码器时调用）。
    fn clear(&mut self) {
        self.map.clear();
        self.order.clear();
    }
}

/// 注册 Kitty 上非 PNG 图片的转码器（不注册时这类图片走文本兜底）。
///
/// 替换当前线程的转码器并清空 PNG 缓存（旧实现的结果不再可信）；传 `None` 移除转码器。
/// 此后在 Kitty 下，尚未成功转码（`png_data` 为 `None`）的非 PNG 图片会回退为文本；已经
/// 保存 `png_data` 的实例直接使用保存的结果（不再查询转码器），同宽渲染缓存命中的实例
/// 在缓存失效前也不受影响。
pub fn set_image_transcoder(transcoder: Option<ImageTranscoder>) {
    IMAGE_TRANSCODER.with(|current| *current.borrow_mut() = transcoder);
    PNG_CACHE.with(|cache| cache.borrow_mut().clear());
}

/// `toPng(base64Data, mimeType)`：带 LRU 缓存（含失败的 `None`）。
///
/// 没有注册转码器时直接返回 `None`（不查询也不写缓存）；命中缓存直接返回；否则调用
/// 转码器并把结果（包括失败）写回缓存。
fn to_png(base64_data: &str, mime_type: &str) -> Option<String> {
    let transcoder = IMAGE_TRANSCODER.with(|current| current.borrow().clone())?;
    if let Some(cached) = PNG_CACHE.with(|cache| cache.borrow_mut().get(base64_data)) {
        return cached;
    }
    let png = transcoder(base64_data, mime_type);
    PNG_CACHE.with(|cache| cache.borrow_mut().set(base64_data.to_string(), png.clone()));
    png
}

/// 主题（TS `ImageTheme`）。
///
/// 只影响文本兜底行；图片序列本身不携带主题颜色。
#[derive(Clone)]
pub struct ImageTheme {
    /// 给兜底文本着色；每次兜底渲染调用一次。
    pub fallback_color: StyleFn,
}

impl ImageTheme {
    /// 恒等主题（不添加 ANSI）。
    pub fn plain() -> Self {
        ImageTheme {
            fallback_color: Arc::new(|text: &str| text.to_string()),
        }
    }
}

/// 选项（TS `ImageOptions`）。
///
/// 除 `filename`（只用于文本兜底）外，其余字段都影响图片协议分支。
#[derive(Debug, Clone, Default)]
pub struct ImageOptions {
    /// 图片列数上限；`None` 时 `Image::render` 用 60，且受 `渲染宽度 - 2` 限制（最少 1 列）。
    pub max_width_cells: Option<f64>,
    /// 行数上限；`None` 时按实际 `max_width` 与单元格像素尺寸推导（见模块文档）。
    pub max_height_cells: Option<f64>,
    /// 仅用于文本兜底的显示名（可被 `~/` 压缩、在支持超链接时包成 OSC 8 链接）。
    pub filename: Option<String>,
    /// Kitty 图片 ID；提供时复用该 ID（动画/更新）。
    ///
    /// `None` 时 Kitty 分支在首次走图片路径时用 `allocate_image_id` 申请一个。
    pub image_id: Option<u64>,
}

/// 图片组件（TS `class Image`）。
///
/// 构造时确定 `dimensions`（显式传入 → 按 MIME 解析 → 800x600 默认值）。Kitty 分支可能在
/// 首次走图片路径时申请 `image_id`，之后同一实例复用该 ID。渲染结果按宽度缓存，见模块文档。
pub struct Image {
    /// 原始 base64 数据（不做解码/校验）。
    base64_data: String,
    /// 原始 MIME 类型；只有精确等于 `image/png` 时才跳过转码。
    mime_type: String,
    /// 构造时确定的像素尺寸；不会因转码后的 PNG 尺寸而改写。
    dimensions: ImageDimensions,
    /// 兜底文本的主题。
    theme: ImageTheme,
    /// 构造选项（宽度/高度上限、文件名、可选图片 ID）。
    options: ImageOptions,
    /// Kitty 图片 ID；`None` 表示尚未指定/申请（iTerm2 分支不使用）。
    image_id: Option<u64>,
    /// Kitty 的 PNG 数据。失败不被记住（下次 render 可被新转码器重试）。
    ///
    /// 这里只保存成功结果；`to_png` 的共享缓存会记住失败（`None`），注册新转码器会清空它。
    png_data: Option<String>,

    /// 按宽度命中的渲染缓存（ANSI 序列原样保存）；`invalidate` 清空。
    cached_lines: Option<Vec<String>>,
    /// 与 `cached_lines` 对应的渲染宽度。
    cached_width: Option<usize>,
}

impl Image {
    /// `constructor(base64Data, mimeType, theme, options = {}, dimensions?)`。
    ///
    /// 尺寸解析顺序：显式 `dimensions` → [`get_image_dimensions`]（只识别 png/jpeg/gif/webp，
    /// MIME 精确匹配）→ 800x600 兜底。`options.image_id` 原样保存在组件里，供 Kitty 复用。
    pub fn new(
        base64_data: impl Into<String>,
        mime_type: impl Into<String>,
        theme: ImageTheme,
        options: ImageOptions,
        dimensions: Option<ImageDimensions>,
    ) -> Self {
        let base64_data = base64_data.into();
        let mime_type = mime_type.into();
        let dimensions = dimensions
            .or_else(|| get_image_dimensions(&base64_data, &mime_type))
            .unwrap_or(ImageDimensions {
                width_px: 800.0,
                height_px: 600.0,
            });
        Image {
            image_id: options.image_id,
            base64_data,
            mime_type,
            dimensions,
            theme,
            options,
            png_data: None,
            cached_lines: None,
            cached_width: None,
        }
    }

    /// 取得本图片使用的 Kitty 图片 ID（如果有）。
    ///
    /// 首次走 Kitty 图片路径之前只有构造时传入的 `options.image_id`；之后可能是新申请的 ID。
    pub fn get_image_id(&self) -> Option<u64> {
        self.image_id
    }

    /// 原始尺寸。
    ///
    /// 是构造时确定的尺寸；转码后的 PNG 尺寸只用于当次渲染，不写回这里。
    pub fn dimensions(&self) -> ImageDimensions {
        self.dimensions
    }
}

impl Component for Image {
    fn invalidate(&mut self) {
        // 只清渲染缓存；png_data 与 image_id 保留（避免同一实例重复转码/重复申请 ID）。
        self.cached_lines = None;
        self.cached_width = None;
    }

    fn handles_input(&self) -> bool {
        // TS 的 Image 是展示组件，没有 handleInput。
        false
    }

    fn render(&mut self, width: usize) -> Vec<String> {
        // 缓存只以宽度为键：能力/转码器变化不会自动失效（见模块文档）。
        if let (Some(lines), Some(cached_width)) = (&self.cached_lines, self.cached_width)
            && cached_width == width
        {
            return lines.clone();
        }

        // 宽度预算：左右各预留 1 列；默认上限 60 列。
        let max_width = f64::max(
            1.0,
            f64::min(
                width as f64 - 2.0,
                self.options.max_width_cells.unwrap_or(60.0),
            ),
        );
        let cell_dimensions = get_cell_dimensions();
        // 默认高度上限：把 max_width 列对应的像素宽度换算成行数，使裁剪框在像素上近似方形。
        let default_max_height = f64::max(
            1.0,
            (max_width * cell_dimensions.width_px / cell_dimensions.height_px).ceil(),
        );
        let max_height = self.options.max_height_cells.unwrap_or(default_max_height);

        let caps = get_capabilities();
        let mut data: Option<String> = Some(self.base64_data.clone());
        let mut dimensions = self.dimensions;
        // 只有 Kitty 且 MIME 不是 image/png 时才需要转码；iTerm2 直接送原始数据。
        if caps.images == Some(ImageProtocol::Kitty) && self.mime_type != "image/png" {
            if self.png_data.is_none() {
                // 成功结果保存在实例上；失败时保持 None，后续渲染会再查共享缓存
                // （注册新转码器会清空它）。
                self.png_data = to_png(&self.base64_data, &self.mime_type);
            }
            data = self.png_data.clone();
            // 转码可能应用 EXIF 旋转，因此优先取 PNG 自己的尺寸。
            if let Some(png) = &data
                && let Some(png_dimensions) = get_png_dimensions(png)
            {
                dimensions = png_dimensions;
            }
        }

        let lines: Vec<String>;
        // TS 的 `if (caps.images && data)` 是**真值**判定：`""` 与 `null` 一样走文本回退
        // （空 base64、或转码器返回空串时）。Rust 的 `Option<String>` 只能表达"有无"，
        // 因此这里还要排除空串。
        let has_data = data.as_deref().is_some_and(|data| !data.is_empty());
        // 能力存在且数据非空才走图片协议，否则统一走文本兜底。
        if caps.images.is_some() && has_data {
            // Kitty 首次走图片路径时申请 ID；iTerm2 不使用图片 ID。
            if caps.images == Some(ImageProtocol::Kitty) && self.image_id.is_none() {
                self.image_id = Some(allocate_image_id());
            }
            // move_cursor=false 让 Kitty 加 C=1（不移动光标）；占位行数由返回的 rows 决定。
            let rendered = render_image(
                data.as_deref().expect("checked data"),
                dimensions,
                ImageRenderOptions {
                    max_width_cells: Some(max_width),
                    max_height_cells: Some(max_height),
                    preserve_aspect_ratio: None,
                    image_id: self.image_id,
                    move_cursor: Some(false),
                },
            );

            match rendered {
                Some(result) => {
                    // 记住图片 ID，供之后清理。
                    if let Some(image_id) = result.image_id {
                        self.image_id = Some(image_id);
                    }

                    if caps.images == Some(ImageProtocol::Kitty) {
                        // Kitty：C=1 阻止光标移动，无需光标位移。
                        let mut kitty_lines = vec![result.sequence];
                        // 返回 `rows` 行，让 TUI 计入图片高度。
                        for _ in 0..result.rows.saturating_sub(1) {
                            kitty_lines.push(String::new());
                        }
                        lines = kitty_lines;
                    } else {
                        // iTerm2：前 `rows-1` 行为空行，最后一行上移光标后绘制图片，
                        // 再由 TUI 把光标移回，保持光标记账在滚动区内。
                        let mut iterm_lines: Vec<String> =
                            vec![String::new(); result.rows.saturating_sub(1)];
                        let row_offset = result.rows.saturating_sub(1);
                        let move_up = if row_offset > 0 {
                            format!("\x1b[{row_offset}A")
                        } else {
                            String::new()
                        };
                        iterm_lines.push(format!("{move_up}{}", result.sequence));
                        lines = iterm_lines;
                    }
                }
                None => {
                    // render_image 返回 None：与“不支持图片”同一路径，退回文本行。
                    // 兜底使用构造时的原始尺寸（不是转码后的 PNG 尺寸）。
                    let fallback = image_fallback(
                        &self.mime_type,
                        Some(self.dimensions),
                        self.options.filename.as_deref(),
                    );
                    lines = vec![truncate_to_width(
                        &(self.theme.fallback_color)(&fallback),
                        width,
                        "...",
                        false,
                    )];
                }
            }
        } else {
            // 能力缺失、数据为空或转码失败：文本兜底（尺寸同样取构造时的值）。
            let fallback = image_fallback(
                &self.mime_type,
                Some(self.dimensions),
                self.options.filename.as_deref(),
            );
            lines = vec![truncate_to_width(
                &(self.theme.fallback_color)(&fallback),
                width,
                "...",
                false,
            )];
        }

        // 按宽度缓存：下一次同宽渲染直接复用（能力变化需显式 invalidate）。
        self.cached_lines = Some(lines.clone());
        self.cached_width = Some(width);

        lines
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::terminal_image::{
        CellDimensions, KittyImageMetadata, TerminalCapabilities, crop_kitty_image_line,
        get_kitty_image_metadata, get_kitty_image_placement, home_dir, reset_capabilities_cache,
        set_capabilities, set_cell_dimensions,
    };
    use crate::text::visible_width;

    // `Buffer.from("jpeg").toString("base64")`
    const JPEG: &str = "amVwZWc=";
    // 40x10 最小 PNG（签名 + IHDR），足以让 `getPngDimensions` 解析。
    const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAACgAAAAK";

    fn caps(
        images: Option<ImageProtocol>,
        true_color: bool,
        hyperlinks: bool,
    ) -> TerminalCapabilities {
        TerminalCapabilities {
            images,
            true_color,
            hyperlinks,
        }
    }

    fn kitty_caps() -> TerminalCapabilities {
        caps(Some(ImageProtocol::Kitty), true, true)
    }

    fn cells(width_px: f64, height_px: f64) {
        set_cell_dimensions(CellDimensions {
            width_px,
            height_px,
        });
    }

    fn dims(width_px: f64, height_px: f64) -> ImageDimensions {
        ImageDimensions {
            width_px,
            height_px,
        }
    }

    /// 还原测试前的全局状态（TS 的 `afterEach`）。
    fn reset() {
        set_image_transcoder(None);
        reset_capabilities_cache();
        cells(9.0, 18.0);
    }

    fn make(
        data: &str,
        mime_type: &str,
        options: ImageOptions,
        dimensions: Option<ImageDimensions>,
    ) -> Image {
        Image::new(data, mime_type, ImageTheme::plain(), options, dimensions)
    }

    fn transcoder(calls: Arc<Mutex<Vec<String>>>) -> ImageTranscoder {
        Arc::new(move |data: &str, _mime_type: &str| {
            calls.lock().expect("calls lock").push(data.to_string());
            if data == JPEG {
                Some(PNG.to_string())
            } else {
                None
            }
        })
    }

    fn render_once(data: &str, mime_type: &str) -> Vec<String> {
        make(data, mime_type, ImageOptions::default(), None).render(80)
    }

    #[test]
    fn caps_image_component_height_to_a_square_pixel_box_by_default() {
        set_capabilities(kitty_caps());
        cells(10.0, 20.0);
        let mut image = make(
            "AAAA",
            "image/png",
            ImageOptions {
                max_width_cells: Some(10.0),
                ..Default::default()
            },
            Some(dims(10.0, 100.0)),
        );
        let lines = image.render(12);
        assert_eq!(lines.len(), 5);
        assert!(lines[0].contains(",c=1,r=5"));
        reset();
    }

    #[test]
    fn places_image_sequence_on_first_line_with_empty_padding_rows() {
        set_capabilities(kitty_caps());
        cells(10.0, 10.0);
        let mut image = make(
            "AAAA",
            "image/png",
            ImageOptions {
                max_width_cells: Some(2.0),
                ..Default::default()
            },
            Some(dims(20.0, 20.0)),
        );
        let lines = image.render(4);
        let image_id = image.get_image_id().expect("kitty image id");
        assert!(lines[0].starts_with("\x1b_G"));
        assert!(lines[0].contains(",C=1,"));
        assert!(lines[0].contains(&format!(",i={image_id}")));
        assert!(lines[0].ends_with("\x1b\\"));
        assert_eq!(lines.len(), 2);
        assert!(lines[1].is_empty());
        reset();
    }

    #[test]
    fn truncates_long_image_fallback_lines_to_render_width() {
        set_capabilities(caps(None, false, false));
        let Some(home) = home_dir() else {
            return;
        };
        let filename = format!(
            "{}.png",
            "generated-image-with-a-very-long-absolute-path".repeat(4)
        );
        let path = home.join("images").join(filename);
        let theme = ImageTheme {
            fallback_color: Arc::new(|value: &str| format!("\x1b[33m{value}\x1b[0m")),
        };
        let mut image = Image::new(
            "AAAA",
            "image/png",
            theme,
            ImageOptions {
                filename: Some(path.to_string_lossy().into_owned()),
                ..Default::default()
            },
            Some(dims(1280.0, 720.0)),
        );
        let lines = image.render(40);
        assert_eq!(lines.len(), 1);
        let width = visible_width(&lines[0]);
        assert!(
            width <= 40,
            "fallback line wider than 40: visible={width} raw={:?}",
            lines[0]
        );
        assert!(
            lines[0].contains("..."),
            "expected ellipsis when truncating long fallback path"
        );
        assert!(
            lines[0].contains("~"),
            "expected home-shortened path in fallback"
        );
        reset();
    }

    #[test]
    fn keeps_kitty_placement_reserved_lines_and_cropping_metadata_consistent() {
        set_capabilities(kitty_caps());
        cells(9.0, 18.0);
        let mut image = make(
            "AAAA",
            "image/png",
            ImageOptions {
                max_width_cells: Some(60.0),
                image_id: Some(8938),
                ..Default::default()
            },
            Some(dims(615.0, 86.0)),
        );
        let lines = image.render(62);
        assert_eq!(lines.len(), 4);
        assert_eq!(
            lines[1..].to_vec(),
            vec![String::new(), String::new(), String::new()]
        );
        assert!(lines[0].contains(",c=60,r=4,i=8938;"));
        assert_eq!(
            get_kitty_image_metadata(&lines[0]),
            Some(KittyImageMetadata {
                image_id: 8938,
                columns: 60,
                rows: 4,
                width_px: 615.0,
                height_px: 86.0,
            })
        );
        let cropped = crop_kitty_image_line(&lines[0], 1, 2);
        assert_eq!(
            get_kitty_image_placement(&cropped).map(|placement| placement.sequence),
            Some("\x1b_Ga=p,q=2,C=1,c=60,i=8938,y=21,h=44,r=2\x1b\\".to_string())
        );

        let narrower = image.render(32);
        assert_eq!(narrower.len(), 2);
        assert!(narrower[0].contains(",c=30,r=2,i=8938;"));
        assert_eq!(
            get_kitty_image_metadata(&narrower[0]).map(|metadata| metadata.rows),
            Some(2)
        );
        reset();
    }

    #[test]
    fn keeps_height_limited_kitty_columns_reservations_and_crop_metadata_consistent() {
        set_capabilities(kitty_caps());
        cells(14.0, 28.0);
        let mut image = make(
            "AAAA",
            "image/png",
            ImageOptions {
                max_width_cells: Some(30.0),
                image_id: Some(8938),
                ..Default::default()
            },
            Some(dims(400.0, 900.0)),
        );
        let lines = image.render(32);
        assert_eq!(lines.len(), 15);
        assert!(lines[0].contains(",c=13,r=15,i=8938;"));
        assert_eq!(
            get_kitty_image_metadata(&lines[0]),
            Some(KittyImageMetadata {
                image_id: 8938,
                columns: 13,
                rows: 15,
                width_px: 400.0,
                height_px: 900.0,
            })
        );
        assert_eq!(
            get_kitty_image_placement(&crop_kitty_image_line(&lines[0], 1, 2))
                .map(|placement| placement.sequence),
            Some("\x1b_Ga=p,q=2,C=1,c=13,i=8938,y=60,h=120,r=2\x1b\\".to_string())
        );

        let narrower = image.render(22);
        assert_eq!(narrower.len(), 10);
        assert!(narrower[0].contains(",c=9,r=10,i=8938;"));
        reset();
    }

    #[test]
    fn keeps_iterm2_ceiling_based_reserved_lines_and_cursor_offset() {
        set_capabilities(caps(Some(ImageProtocol::Iterm2), true, true));
        cells(9.0, 18.0);
        let mut image = make(
            "AAAA",
            "image/png",
            ImageOptions {
                max_width_cells: Some(60.0),
                ..Default::default()
            },
            Some(dims(615.0, 86.0)),
        );
        assert_eq!(
            image.render(62),
            vec![
                String::new(),
                String::new(),
                String::new(),
                String::new(),
                "\x1b[4A\x1b]1337;File=inline=1;size=3;width=60;height=auto:AAAA\u{7}".to_string(),
            ]
        );
        reset();
    }

    #[test]
    fn sends_converted_png_data_sized_from_the_png() {
        set_capabilities(kitty_caps());
        cells(10.0, 10.0);
        let calls = Arc::new(Mutex::new(Vec::new()));
        set_image_transcoder(Some(transcoder(Arc::clone(&calls))));
        let lines = make(
            JPEG,
            "image/jpeg",
            ImageOptions::default(),
            Some(dims(20.0, 20.0)),
        )
        .render(20);
        assert!(lines[0].contains("f=100"));
        assert!(lines[0].contains(&format!(";{PNG}\u{1b}\\")));
        // 40x10 的 PNG 在 18 列时是 5 行，而不是源尺寸 20x20 的 18 行。
        assert_eq!(lines.len(), 5);
        reset();
    }

    #[test]
    fn renders_a_text_fallback_until_a_working_transcoder_is_registered() {
        set_capabilities(kitty_caps());
        cells(10.0, 10.0);
        let mut image = make(JPEG, "image/jpeg", ImageOptions::default(), None);
        assert!(image.render(80)[0].starts_with("[Image: [image/jpeg]"));
        set_image_transcoder(Some(Arc::new(|_: &str, _: &str| None)));
        image.invalidate();
        assert!(image.render(80)[0].starts_with("[Image: [image/jpeg]"));
        let calls = Arc::new(Mutex::new(Vec::new()));
        set_image_transcoder(Some(transcoder(Arc::clone(&calls))));
        image.invalidate();
        assert!(image.render(80)[0].contains("\x1b_G"));
        reset();
    }

    #[test]
    fn converts_each_image_once() {
        set_capabilities(kitty_caps());
        cells(10.0, 10.0);
        let calls = Arc::new(Mutex::new(Vec::new()));
        set_image_transcoder(Some(transcoder(Arc::clone(&calls))));
        let mut image = make(JPEG, "image/jpeg", ImageOptions::default(), None);
        image.render(80);
        // 新实例命中共享缓存。
        render_once(JPEG, "image/jpeg");
        // 逐出共享缓存中的条目。
        for index in 0..40 {
            render_once(&format!("other-{index}"), "image/jpeg");
        }
        image.invalidate();
        // 实例保留自己的 PNG。
        image.render(40);
        let converted = calls
            .lock()
            .expect("calls lock")
            .iter()
            .filter(|data| data.as_str() == JPEG)
            .count();
        assert_eq!(converted, 1);
        reset();
    }

    #[test]
    fn does_not_convert_png_data_or_iterm2_output() {
        set_capabilities(kitty_caps());
        cells(10.0, 10.0);
        let calls = Arc::new(Mutex::new(Vec::new()));
        set_image_transcoder(Some(transcoder(Arc::clone(&calls))));
        let png_lines = make(PNG, "image/png", ImageOptions::default(), None).render(20);
        assert!(png_lines[0].contains(&format!(";{PNG}\u{1b}\\")));

        set_capabilities(caps(Some(ImageProtocol::Iterm2), true, true));
        let jpeg_lines = make(JPEG, "image/jpeg", ImageOptions::default(), None).render(20);
        assert!(
            jpeg_lines
                .last()
                .expect("at least one line")
                .ends_with(&format!(":{JPEG}\u{7}"))
        );
        assert!(calls.lock().expect("calls lock").is_empty());
        reset();
    }
    /// 回归（minor）：TS 的 `if (caps.images && data)` 是真值判定，空串走文本回退。
    #[test]
    fn renders_the_text_fallback_for_empty_image_data() {
        set_capabilities(kitty_caps());
        cells(10.0, 10.0);
        let lines = make("", "image/png", ImageOptions::default(), None).render(40);
        assert_eq!(lines.len(), 1);
        assert!(
            lines[0].starts_with("[Image: [image/png]"),
            "empty payload must fall back to text, got {:?}",
            lines[0]
        );
        reset();
    }

    /// 同上：转码器返回空串（非 null）时 TS 的 `if (data)` 也为假。
    #[test]
    fn renders_the_text_fallback_when_the_transcoder_returns_empty_data() {
        set_capabilities(kitty_caps());
        cells(10.0, 10.0);
        set_image_transcoder(Some(Arc::new(|_: &str, _: &str| Some(String::new()))));
        let lines = make(JPEG, "image/jpeg", ImageOptions::default(), None).render(40);
        assert_eq!(lines.len(), 1);
        assert!(
            lines[0].starts_with("[Image: [image/jpeg]"),
            "empty transcoder result must fall back to text, got {:?}",
            lines[0]
        );
        reset();
    }
}
