//! utils/{image-process,image-convert,image-resize-core,image-resize,image-resize-worker,
//! exif-orientation,photon,clipboard-image,tool-result-images}.ts。
//!
//! TS 源：`packages/coding-agent/src/utils/{image-process,image-convert,image-resize-core,
//! image-resize,image-resize-worker,exif-orientation,photon,clipboard-image,
//! tool-result-images}.ts`（9 个文件合并为一个模块，见 `ARCHITECTURE.md` §5）。
//!
//! # feature `images`
//!
//! 整个模块由 `images` feature 门控（`ARCHITECTURE.md` §8 第 16 条：阶段 2 起进 default）。
//! **未启用时**模块为空：没有任何图片处理入口（等价 TS 里 Photon 始终加载失败——`processImage`
//! 对不支持的格式返回转换失败、`resizeImage` 返回 `null`）。默认构建（`default = ["images"]`）
//! 是完整实现。
//!
//! # Photon：原生 `photon-rs 0.3.3`（`ARCHITECTURE.md` §8 第 20 条）
//!
//! TS 通过 `@silvia-odwyer/photon-node` 的 wasm 做解码/缩放/编码；Rust 用同一份 Photon 源码的
//! `photon-rs` **原生**编译，不引入 wasm 运行时。实测 JPEG 编码逐字节一致，PNG 的像素数据可能
//! 差 1 个通道末位（解码器版本），因此测试只比对尺寸/格式/base64 限额。
//!
//! `PhotonImage::new_from_byteslice` 内部是 `image::load_from_memory(...).unwrap()`：坏图会
//! **panic**，而 TS 是 `catch` 后返回 `null`。本模块用 `catch_unwind` 复刻 `catch`，并临时
//! 静默**当前线程**的 panic 输出（全局 hook 包一层 thread-local 开关，不影响其它线程），
//! 因此坏图不会把 panic 文本泄漏到 stderr。
//!
//! # 没有 worker
//!
//! `image-resize-worker.ts` 的 `parentPort` 协议在 Rust 无对应：TS 的 `resizeImage` 会先试
//! worker，失败再回退进程内实现；Rust 的 [`resize_image`] 就是进程内实现（结果相同）。
//! TS 导出的 `IMAGE_RESIZE_WORKER_RESPONSE_TYPE` 常量保留（别人可能引用字符串），
//! `ResizeImageWorkerResponse` 类型**不导出**。
//!
//! `photon.ts` 的 wasm 路径补丁（`fs.readFileSync` 重定向）原生构建无意义，`load_photon`
//! 只保留「惰性加载 + 失败缓存为 `None`」的语义（私有）。原生加载是同步的，所以
//! [`ensure_png_transcoder`] 同步完成注册并直接调用 `on_registered`（TS 里是异步 `then`）。
//!
//! # EXIF 方向
//!
//! 按 §2.21.24 手工解析 JPEG APP1 / WebP `EXIF` chunk / TIFF IFD 的 orientation，再用
//! §2.21.24 的 `rotate90` 索引公式变换 RGBA8 像素（Photon 的 `get_raw_pixels` 布局）。
//! **不**用 `kamadak-exif` 的解析结果替代（它在边界与错误分支上与 TS 不同）。
//!
//! # 文件分组与调用方
//!
//! 文件按功能分段（每组开头有 `// =====` 分隔注释）：
//!
//! 1. `image-resize-core.ts` / `image-resize.ts`：缩放与体积收敛；[`resize_image_in_process`]
//!    在 core，[`resize_image`] 与 [`format_dimension_note`] 在 image-resize.ts（worker 部分
//!    在 Rust 无对应），返回 [`ResizedImage`]。
//! 2. `photon.ts`（原生等价物）：只用 `OnceLock` 缓存一个空句柄，见私有函数 `load_photon`。
//! 3. `exif-orientation.ts`：JPEG APP1 / WebP `EXIF` chunk / TIFF IFD 的方向解析与像素变换，
//!    公开入口 [`apply_exif_orientation`]。
//! 4. `image-convert.ts`：[`convert_image_bytes_to_png`] / [`convert_to_png`]，以及给 Kitty
//!    终端用的 [`load_png_transcoder`] / [`ensure_png_transcoder`]。
//! 5. `image-process.ts` / `clipboard-image.ts` / `tool-result-images.ts`：[`process_image`]、
//!    [`read_clipboard_image`]、[`normalize_tool_result_images`] 三条业务入口。
//!
//! 仓库内的调用方：`kiana-tools` 的 `read` 工具与 `kiana-session` 的 `agent_session` 使用
//! `process_image` / `normalize_tool_result_images`；`kiana-interactive` 在界面路径里调用
//! [`ensure_png_transcoder`]，并通过注入版 `read_clipboard_image_with` 读剪贴板；
//! `kiana-coding-agent` 的 `sdk_exports` 按 TS 原名再导出 `convert_to_png` /
//! `format_dimension_note` / `resize_image`。
//!
//! # 一次 process_image 的执行顺序
//!
//! 1. `normalize_image`：基础 MIME 先去掉 `;` 参数、trim、小写；`image/png`、`image/jpeg`
//!    （含 `image/jpg`）、`image/gif`、`image/webp` 原样通过；其它（如 `image/bmp`）用 Photon
//!    解码并重编码为 PNG，同时记录 `converted_from`。
//! 2. `auto_resize_images`（默认 `true`）时调用 `resize_image`：先应用 EXIF 方向，再按宽、
//!    高上限等比缩放，然后按「PNG 候选 → JPEG 质量列表」的顺序取第一个体积严格小于
//!    `max_bytes` 的候选；不缩放时直接把规范化后的字节转 base64。
//! 3. 生成 hints：转换提示在前，尺寸提示（仅 `was_resized` 时）在后。失败返回逐字文案。
//!
//! # 关键约束与失败模式
//!
//! - 体积比较一律是**严格小于**（`<`）：base64 长度等于 `max_bytes` 仍算超限。
//! - 坏图、无法编码不会向上传播错误：Photon 的 panic 由 `catch_silent` 转成 `None`，
//!   `process_image` 再把 `None` 映射成 `[Image omitted: ...]` 文案。
//! - 模块不创建线程：解码/缩放/编码都在调用线程同步执行；只有剪贴板读取是 `async` 的
//!   （执行外部命令并读临时文件）。
//! - [`ensure_png_transcoder`] 的注册状态与 `kiana-tui` 的能力缓存/转码器都是线程本地状态，
//!   必须在同一线程（TUI 渲染线程）调用。
//! - 剪贴板读取区分三态：后端失败（`Unavailable`）继续后面的回退；后端可用但没有图片
//!   （`Empty`）不会再试 `xclip`，但 WSL 仍会试 PowerShell（Windows 截图不会进入 Linux
//!   剪贴板）；原生剪贴板的错误以 `Err(ClipboardError)` 返回，不再回退。

#![cfg(feature = "images")]

use std::any::Any;
use std::borrow::Cow;
use std::cell::Cell;
use std::ffi::OsString;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, OnceLock};

use base64::Engine as _;
use kiana_ai::{ImageContent, ModelImageResizeOptions, TextContent, UserContentBlock};
use kiana_tui::native::{ClipboardError, ClipboardValue};
use kiana_tui::{ImageProtocol, ImageTranscoder, get_capabilities, set_image_transcoder};
use photon_rs::PhotonImage;
use photon_rs::transform::{SamplingFilter, fliph, flipv, resize as photon_resize};

use crate::clipboard::{
    ClipboardTestIo, DEFAULT_TIMEOUT_MS, EnvMap, env_equals, env_truthy, node_tmpdir,
    run_io_command,
};
use crate::mime::detect_supported_image_mime_type;

// ===========================================================================
// image-resize-core.ts
// ===========================================================================

/// `IMAGE_RESIZE_WORKER_RESPONSE_TYPE`：worker 回包上的标记。
///
/// TS 里用来区分 Node 自己在 worker 通道上发的消息（`node --watch` 的
/// `{ "watch:require": [...] }`）。Rust 没有 worker，本常量只是为了保留字符串值，
/// 模块内无人消费。
pub const IMAGE_RESIZE_WORKER_RESPONSE_TYPE: &str = "kiana:image-resize-response";

/// `DEFAULT_MAX_BYTES = 4.5 * 1024 * 1024`（base64 载荷上限，低于 Anthropic 的 5MB）。
const DEFAULT_MAX_BYTES: u64 = 4_718_592;
/// `DEFAULT_OPTIONS.maxWidth`。
const DEFAULT_MAX_WIDTH: u32 = 2000;
/// `DEFAULT_OPTIONS.maxHeight`。
const DEFAULT_MAX_HEIGHT: u32 = 2000;
/// `DEFAULT_OPTIONS.jpegQuality`。
const DEFAULT_JPEG_QUALITY: u8 = 80;

/// `ImageResizeOptions`。
///
/// TS 的字段都是可选；Rust 里 `max_width` / `max_height` / `max_bytes` 用具体值表达默认
/// （[`ImageResizeOptions::default`] 就是 TS 的 `DEFAULT_OPTIONS`），`jpeg_quality` 保留
/// `Option`（`None` = 默认 80）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageResizeOptions {
    /// 最大宽度，默认 2000。
    pub max_width: u32,
    /// 最大高度，默认 2000。
    pub max_height: u32,
    /// base64 载荷的最大字节数，默认 4.5 MB（`4_718_592`）。
    pub max_bytes: u64,
    /// JPEG 质量，`None` 时使用默认 80。
    pub jpeg_quality: Option<u8>,
}

impl Default for ImageResizeOptions {
    fn default() -> Self {
        ImageResizeOptions {
            max_width: DEFAULT_MAX_WIDTH,
            max_height: DEFAULT_MAX_HEIGHT,
            max_bytes: DEFAULT_MAX_BYTES,
            jpeg_quality: None,
        }
    }
}

/// `ResizedImage`。
///
/// `data` 始终是 base64 字符串（不是原始字节）。`was_resized == false` 表示输入本来就在
/// 限额内：此时 `data` 是输入字节的 base64、`width/height == original_*`；`true` 表示做过
/// 缩放/重编码，`mime_type` 由选中的候选决定（`image/png` 或 `image/jpeg`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResizedImage {
    /// base64 编码后的图片数据。
    pub data: String,
    /// MIME 类型：未缩放时回显输入（空串回显为 `image/png`）；缩放后为 `image/png` 或
    /// `image/jpeg`。
    pub mime_type: String,
    /// EXIF 方向变换之后的原始宽度。
    pub original_width: u32,
    /// EXIF 方向变换之后的原始高度。
    pub original_height: u32,
    /// 输出宽度。
    pub width: u32,
    /// 输出高度。
    pub height: u32,
    /// 是否发生了缩放/重编码。
    pub was_resized: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EncodedCandidate {
    data: String,
    encoded_size: usize,
    mime_type: &'static str,
}

fn base64_encode(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// `Buffer.from(buffer).toString("base64")` + `Buffer.byteLength(data, "utf-8")`。
fn encode_candidate(buffer: Vec<u8>, mime_type: &'static str) -> EncodedCandidate {
    let data = base64_encode(&buffer);
    EncodedCandidate {
        // base64 是纯 ASCII，字节数 == 字符数。
        encoded_size: data.len(),
        data,
        mime_type,
    }
}

/// `resizeImageInProcess(inputBytes, mimeType, options?)`。
///
/// 算法逐步骤照 §2.21.22；任何异常（坏图、编码失败）→ `None`。
///
/// 输入 `input_bytes` 是原始图片字节（不是 base64），输出 `data` 是 base64。处理顺序：
///
/// 1. 解码并应用 EXIF 方向，用变换后的图像得到 `original_width/height`；
/// 2. 尺寸都在上限内且输入 base64 长度严格小于 `max_bytes` → 直接返回**输入字节**的 base64
///    （不重编码；像素里的 EXIF 标记仍然存在，但报告宽高已按方向变换，`was_resized=false`）；
/// 3. 否则先按宽度、再按高度等比缩放，从每轮的「PNG + 各质量 JPEG」候选里取第一个体积
///    严格小于 `max_bytes` 的；一轮不达标就把宽高各乘 0.75（向下取整、最小 1）再试，
///    直到 1x1 仍失败才返回 `None`。
///
/// 1x1 也会被尝试一次，所以只要 1x1 的编码小于 `max_bytes` 就能成功。整个函数体在
/// `catch_silent` 中执行：`photon-rs` 在坏图上 panic，这里按 TS 的 `try/catch` 语义转成
/// `None`，不向调用方传播 panic。
///
/// `mime_type` 只在未缩放分支被回显（空串回显为 `image/png`）；缩放成功时 MIME 由候选决定。
pub fn resize_image_in_process(
    input_bytes: &[u8],
    mime_type: &str,
    options: Option<ImageResizeOptions>,
) -> Option<ResizedImage> {
    let options = options.unwrap_or_default();
    // `Math.ceil(inputBytes.byteLength / 3) * 4`。
    let input_base64_size = input_bytes.len().div_ceil(3) * 4;
    catch_silent(AssertUnwindSafe(|| {
        resize_image_inner(input_bytes, mime_type, &options, input_base64_size)
    }))
    .ok()
    .flatten()
}

fn resize_image_inner(
    input_bytes: &[u8],
    mime_type: &str,
    options: &ImageResizeOptions,
    input_base64_size: usize,
) -> Option<ResizedImage> {
    // 原生 photon-rs 是静态链接的，这一步实际不会失败；保留 `?` 只为与 TS 的 loadPhoton()
    // 返回空值的分支同形。
    load_photon()?;

    // `PhotonImage::new_from_byteslice` 在坏图上 panic → 由外层 catch_unwind 转成 None。
    let raw_image = PhotonImage::new_from_byteslice(input_bytes.to_vec());
    // EXIF 方向必须在读取 originalWidth/originalHeight **之前**应用。
    let image = apply_exif_orientation(raw_image, input_bytes);

    // 宽高取自方向变换后的图像：后续缩放目标与返回的 original_* 都用这个坐标系。
    let original_width = image.get_width();
    let original_height = image.get_height();
    // 子类型只在 mime_type 为空时用来拼 `image/<子类型>`；空串会回落到 "png"。
    let format = mime_type.split('/').nth(1).unwrap_or("png");

    // 已在所有限额内（尺寸 **且** base64 大小严格小于 maxBytes）→ 原样返回输入字节。
    if original_width <= options.max_width
        && original_height <= options.max_height
        && (input_base64_size as u64) < options.max_bytes
    {
        return Some(ResizedImage {
            data: base64_encode(input_bytes),
            mime_type: if mime_type.is_empty() {
                format!("image/{format}")
            } else {
                mime_type.to_string()
            },
            original_width,
            original_height,
            width: original_width,
            height: original_height,
            was_resized: false,
        });
    }

    // 先按宽度缩放，再按高度缩放；另一边用 `Math.round`。
    // 顺序是设计约束：两步都可能触发，第二步基于第一步更新后的另一维重算（保持宽高比）。
    let mut target_width = original_width;
    let mut target_height = original_height;
    if target_width > options.max_width {
        target_height = js_round(
            f64::from(target_height) * f64::from(options.max_width) / f64::from(target_width),
        );
        target_width = options.max_width;
    }
    if target_height > options.max_height {
        target_width = js_round(
            f64::from(target_width) * f64::from(options.max_height) / f64::from(target_height),
        );
        target_height = options.max_height;
    }

    // `Array.from(new Set([jpegQuality, 85, 70, 55, 40]))`。
    // 去重避免同一质量重复编码；`jpeg_quality` 排在最前，因此调用方指定的质量优先。
    let mut quality_steps: Vec<u8> = Vec::with_capacity(5);
    for quality in [
        options.jpeg_quality.unwrap_or(DEFAULT_JPEG_QUALITY),
        85,
        70,
        55,
        40,
    ] {
        if !quality_steps.contains(&quality) {
            quality_steps.push(quality);
        }
    }

    let mut current_width = target_width;
    let mut current_height = target_height;
    // 逐轮缩小，直到某个候选满足体积上限。每轮都会重新缩放并编码最多 1 份 PNG + 5 份 JPEG；
    // 实际会走到的终止条件只有「1x1 仍超限」；循环末尾还有一次「取整后尺寸不再变化」的
    // 防御性 break（与 TS 循环同构，按整数尺寸推导不可达，见该处说明）。
    loop {
        let candidates = try_encodings(&image, current_width, current_height, &quality_steps);
        for candidate in candidates {
            // 严格小于（`encodedSize < maxBytes`）。
            if (candidate.encoded_size as u64) < options.max_bytes {
                return Some(ResizedImage {
                    data: candidate.data,
                    mime_type: candidate.mime_type.to_string(),
                    original_width,
                    original_height,
                    width: current_width,
                    height: current_height,
                    was_resized: true,
                });
            }
        }

        // 1x1 的候选刚刚已经试过，继续缩没有意义。
        if current_width == 1 && current_height == 1 {
            break;
        }
        // 两维独立收缩：已经是 1 的维度保持 1，其余 `Math.max(1, Math.floor(v * 0.75))`。
        let next_width = if current_width == 1 {
            1
        } else {
            js_floor_max_one(f64::from(current_width) * 0.75)
        };
        let next_height = if current_height == 1 {
            1
        } else {
            js_floor_max_one(f64::from(current_height) * 0.75)
        };
        // 两维 >=2 时乘 0.75 向下取整必然变小，两维都为 1 时已在上面的分支退出；
        // 此 break 实际不可达，只是与 TS 循环同构的防御。
        if next_width == current_width && next_height == current_height {
            break;
        }
        current_width = next_width;
        current_height = next_height;
    }

    // 所有尺寸的候选都超过上限 → 无法在限额内内联。
    None
}

/// `tryEncodings(width, height, jpegQualities)`：PNG 候选 + 各质量步的 JPEG 候选。
///
/// 先做一次 Lanczos3 缩放，再对同一张缩放结果编码：PNG 永远排第一，所以体积达标时
/// PNG 会优先于 JPEG 被选中（即使输入是 JPEG）。`encoded_size` 是 base64 后的字符数，
/// 也就是实际内联载荷的字节数。
fn try_encodings(
    image: &PhotonImage,
    width: u32,
    height: u32,
    jpeg_qualities: &[u8],
) -> Vec<EncodedCandidate> {
    let resized = photon_resize(image, width, height, SamplingFilter::Lanczos3);
    let mut candidates = Vec::with_capacity(1 + jpeg_qualities.len());
    candidates.push(encode_candidate(resized.get_bytes(), "image/png"));
    for &quality in jpeg_qualities {
        candidates.push(encode_candidate(
            resized.get_bytes_jpeg(quality),
            "image/jpeg",
        ));
    }
    candidates
}

/// `Math.round`（非负输入；半值取大）。
///
/// 调用点只传有限的非负比例；`NaN` 或非正值在这里统一收敛到 0。
fn js_round(value: f64) -> u32 {
    if value.is_nan() || value <= 0.0 {
        0
    } else {
        value.round() as u32
    }
}

/// `Math.max(1, Math.floor(value))`。
///
/// 与 JS 的差别只在 `NaN`：JS 的 `Math.max(1, NaN)` 得到 `NaN`，这里返回 1。
/// 调用点传入的是由 `u32` 宽高算出的有限值，因此正常路径上两者一致。
fn js_floor_max_one(value: f64) -> u32 {
    if value.is_nan() {
        return 1;
    }
    let floored = value.floor();
    if floored < 1.0 { 1 } else { floored as u32 }
}

/// `resizeImage(inputBytes, mimeType, options?)`。
///
/// TS 先尝试 worker，失败回退进程内；Rust 直接运行进程内实现（`resize_image_in_process`）。
///
/// TS 的 worker 是为了让解码/缩放/编码不占用 TUI 事件循环（`image-resize.ts` 的注释）；
/// Rust 侧没有 worker，[`resize_image`] 在调用线程同步完成全部工作，本模块不创建线程。
pub fn resize_image(
    input_bytes: &[u8],
    mime_type: &str,
    options: Option<ImageResizeOptions>,
) -> Option<ResizedImage> {
    resize_image_in_process(input_bytes, mime_type, options)
}

/// `formatDimensionNote(result)`：帮助模型换算坐标的尺寸提示。
///
/// 文案逐字；`scale.toFixed(2)` 用 [`kiana_ai::model_catalog::js_to_fixed`]（JS 语义：正确舍入、
/// 半值取大；不能直接用 Rust 的 `{:.2}`，它在 `0.125` 这类值上会取偶）。
///
/// `was_resized == false` 时返回 `None`（没有缩放就不需要换算坐标）。scale 只用宽度计算
///（`original_width / width`），高度不参与；文案格式见 `spec/ca-cli.md` §2.21.21。
pub fn format_dimension_note(result: &ResizedImage) -> Option<String> {
    if !result.was_resized {
        return None;
    }
    let scale = f64::from(result.original_width) / f64::from(result.width);
    Some(format!(
        "[Image: original {}x{}, displayed at {}x{}. Multiply coordinates by {} to map to original image.]",
        result.original_width,
        result.original_height,
        result.width,
        result.height,
        kiana_ai::model_catalog::js_to_fixed(scale, 2)
    ))
}

// ===========================================================================
// photon.ts（原生等价物）
// ===========================================================================

/// `loadPhoton()` 的返回句柄。原生 `photon-rs` 是静态链接的，没有「加载失败」的运行时路径；
/// 保留 `Option` 是为了照抄 TS 的缓存语义（见 [`load_photon`]）。
struct PhotonHandle;

static PHOTON_MODULE: OnceLock<Option<PhotonHandle>> = OnceLock::new();

/// `loadPhoton()`（私有）：惰性加载并缓存；失败缓存为 `None`。
///
/// `photon.ts` 的 `fs.readFileSync` 补丁与 wasm 文件查找在原生构建里没有对应物，因此这里
/// 只有 `OnceLock` 的「首次调用初始化，之后返回同一个结果」。
///
/// 原生 `photon-rs` 是静态链接的，实际永远返回 `Some`；`Option` 只为保留 TS 加载失败的分支
/// 以及调用点的 `?`（见 `PhotonHandle` 的说明）。
fn load_photon() -> Option<&'static PhotonHandle> {
    PHOTON_MODULE.get_or_init(|| Some(PhotonHandle)).as_ref()
}

thread_local! {
    /// 当前线程是否静默 panic 输出（[`catch_silent`] 的开关）。
    static SUPPRESS_PANIC_OUTPUT: Cell<bool> = const { Cell::new(false) };
}

/// 安装「按线程静默」的 panic hook（只安装一次，包住原有 hook）。
///
/// 全局 `Once` 保证只替换一次；新 hook 先读当前线程的 `SUPPRESS_PANIC_OUTPUT`，
/// 为真才丢弃输出，否则转交给安装前的 hook。因此其它线程的 panic 输出不受影响。
fn install_quiet_panic_hook() {
    static INSTALL: std::sync::Once = std::sync::Once::new();
    INSTALL.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if !SUPPRESS_PANIC_OUTPUT.with(Cell::get) {
                previous(info);
            }
        }));
    });
}

/// TS 的 `try { ... } catch { return null }`：捕获 panic，且不把 panic 文本写到 stderr。
///
/// 进入时把本线程的标志置真并保存旧值，`catch_unwind` 返回后恢复，所以嵌套调用能正确还原
/// 外层状态。本模块的调用点都只取 `.ok()`，panic 载荷被丢弃；标志是 thread-local，
/// 不会静默并发线程的 panic。
fn catch_silent<T>(
    body: impl FnOnce() -> T + std::panic::UnwindSafe,
) -> Result<T, Box<dyn Any + Send>> {
    install_quiet_panic_hook();
    let previous = SUPPRESS_PANIC_OUTPUT.with(|flag| flag.replace(true));
    let result = catch_unwind(body);
    // 无论是否 panic，catch_unwind 返回后都恢复标志，避免后续 panic 被误静默。
    SUPPRESS_PANIC_OUTPUT.with(|flag| flag.set(previous));
    result
}

// ===========================================================================
// exif-orientation.ts
// ===========================================================================

/// `bytes[pos]`（JS 语义：越界/负索引读 0）。
///
/// 读 0 让所有解析函数在截断文件上返回「没有 EXIF」而不是 panic。
fn byte_at(bytes: &[u8], pos: i64) -> u16 {
    if pos < 0 {
        0
    } else {
        u16::from(bytes.get(pos as usize).copied().unwrap_or(0))
    }
}

/// `read16(pos)`（按字节序）。
fn read16(bytes: &[u8], pos: i64, little_endian: bool) -> u16 {
    if little_endian {
        byte_at(bytes, pos) | (byte_at(bytes, pos + 1) << 8)
    } else {
        (byte_at(bytes, pos) << 8) | byte_at(bytes, pos + 1)
    }
}

/// `read32(pos)` 的大端分支（`>>> 0`：无符号 32 位）。
fn read32_be(bytes: &[u8], pos: i64) -> i64 {
    let value = (u32::from(byte_at(bytes, pos)) << 24)
        | (u32::from(byte_at(bytes, pos + 1)) << 16)
        | (u32::from(byte_at(bytes, pos + 2)) << 8)
        | u32::from(byte_at(bytes, pos + 3));
    i64::from(value)
}

/// `read32(pos)` 的小端分支（JS 位运算结果是 **int32**，可能为负）。
///
/// TIFF 的 IFD 偏移与 WebP 的 chunk 大小都走这里，所以两者在实现里都可能为负；后续的
/// `tiff_start + ifd_offset`、`data_start + chunk_size` 原样保留这个符号语义。
fn read32_le(bytes: &[u8], pos: i64) -> i64 {
    let value = u32::from(byte_at(bytes, pos))
        | (u32::from(byte_at(bytes, pos + 1)) << 8)
        | (u32::from(byte_at(bytes, pos + 2)) << 16)
        | (u32::from(byte_at(bytes, pos + 3)) << 24);
    i64::from(value as i32)
}

/// `readOrientationFromTiff(bytes, tiffStart)`。
fn read_orientation_from_tiff(bytes: &[u8], tiff_start: i64) -> u8 {
    let length = bytes.len() as i64;
    // TIFF 头至少 8 字节：字节序(2) + 魔数(2) + 第一个 IFD 的偏移(4)。
    if tiff_start + 8 > length {
        return 1;
    }

    // 0x4949 = "II"（小端）；其它值（包括 "MM" 与非法头）一律按大端处理。
    // 注意这里不校验 TIFF 魔数 0x002A。
    let byte_order = (byte_at(bytes, tiff_start) << 8) | byte_at(bytes, tiff_start + 1);
    let little_endian = byte_order == 0x4949;

    // IFD 偏移是相对 TIFF 头的，不是文件绝对位置。
    let ifd_offset = if little_endian {
        read32_le(bytes, tiff_start + 4)
    } else {
        read32_be(bytes, tiff_start + 4)
    };
    let ifd_start = tiff_start + ifd_offset;
    // 第一个 IFD 以 2 字节的条目数开头。
    if ifd_start + 2 > length {
        return 1;
    }

    // 每个条目固定 12 字节：tag(2) + type(2) + count(4) + value/offset(4)。
    let entry_count = read16(bytes, ifd_start, little_endian);
    for index in 0..i64::from(entry_count) {
        let entry_pos = ifd_start + 2 + index * 12;
        // 条目列表被截断时放弃整个解析（返回 1），不返回已经看到的其它值。
        if entry_pos + 12 > length {
            return 1;
        }
        if read16(bytes, entry_pos, little_endian) == 0x0112 {
            // Orientation 的 value 固定在条目偏移 +8；只有 1..=8 合法。
            let value = read16(bytes, entry_pos + 8, little_endian);
            return if (1..=8).contains(&value) {
                value as u8
            } else {
                1
            };
        }
    }

    1
}

/// `hasExifHeader(bytes, offset)`：`Exif\0\0`。
fn has_exif_header(bytes: &[u8], offset: i64) -> bool {
    byte_at(bytes, offset) == 0x45
        && byte_at(bytes, offset + 1) == 0x78
        && byte_at(bytes, offset + 2) == 0x69
        && byte_at(bytes, offset + 3) == 0x66
        && byte_at(bytes, offset + 4) == 0x00
        && byte_at(bytes, offset + 5) == 0x00
}

/// `findJpegTiffOffset(bytes)`：扫描 JPEG marker，找到第一个带 `Exif\0\0` 的 APP1。
///
/// 段格式是 `0xFF <marker> <2 字节大端长度（含长度字段自身）> <载荷>`，从 SOI 之后的
/// offset 2 开始走；`0xFF` 填充字节只前进 1。只认载荷以 `Exif\0\0` 开头的 APP1，其它
/// APP1（例如 XMP）按段长跳过。任何越界都返回 `None`，调用方按 orientation 1 处理。
fn find_jpeg_tiff_offset(bytes: &[u8]) -> Option<usize> {
    let length = bytes.len() as i64;
    let mut offset: i64 = 2;
    while offset < length - 1 {
        if byte_at(bytes, offset) != 0xff {
            return None;
        }
        let marker = byte_at(bytes, offset + 1);
        if marker == 0xff {
            offset += 1;
            continue;
        }

        if marker == 0xe1 {
            if offset + 4 >= length {
                return None;
            }
            let segment_start = offset + 4;
            if segment_start + 6 > length {
                return None;
            }
            if has_exif_header(bytes, segment_start) {
                // 跳过 6 字节 "Exif\0\0"，返回 TIFF 头的绝对偏移。
                return Some((segment_start + 6) as usize);
            }
        }

        if offset + 4 > length {
            return None;
        }
        // 段长包含它自己的 2 字节，所以下一段起点是 offset + 2 + segment_length。
        let segment_length =
            ((byte_at(bytes, offset + 2) << 8) | byte_at(bytes, offset + 3)) as i64;
        offset += 2 + segment_length;
    }

    None
}

/// `findWebpTiffOffset(bytes)`：遍历 RIFF chunk，找 `EXIF` chunk 的 TIFF 起点。
///
/// RIFF 头 12 字节之后是 chunk：4 字节 id + 4 字节小端长度 + 数据。找到 `EXIF` 就返回，
/// 数据前可能带 6 字节 `Exif\0\0` 前缀（按 chunk 长度判断后跳过）；其它 chunk 按
/// `8 + chunk_size` 推进，并按 RIFF 规则对齐到偶数（`chunk_size % 2`）。
fn find_webp_tiff_offset(bytes: &[u8]) -> Option<usize> {
    let length = bytes.len() as i64;
    let mut offset: i64 = 12;
    while offset + 8 <= length {
        let chunk_id = [
            byte_at(bytes, offset) as u8,
            byte_at(bytes, offset + 1) as u8,
            byte_at(bytes, offset + 2) as u8,
            byte_at(bytes, offset + 3) as u8,
        ];
        // JS 的位运算 → int32（可能为负）。
        let chunk_size = read32_le(bytes, offset + 4);
        let data_start = offset + 8;

        if &chunk_id == b"EXIF" {
            if data_start + chunk_size > length {
                return None;
            }
            // 有些 WebP 在 TIFF 头之前带 "Exif\0\0"。
            let tiff_start = if chunk_size >= 6 && has_exif_header(bytes, data_start) {
                data_start + 6
            } else {
                data_start
            };
            return Some(tiff_start as usize);
        }

        // RIFF chunk 按偶数对齐（JS 的 `%` 对负数保留符号）。
        // `chunk_size` 来自上面的 int32 语义，理论上可为负；推进公式与 TS 完全相同。
        offset = data_start + chunk_size + (chunk_size % 2);
    }

    None
}

/// `getExifOrientation(bytes)`（私有）：JPEG / WebP → TIFF 方向；其它格式 1。
///
/// 只识别 JPEG（`FF D8`）与 WebP（`RIFF....WEBP`）；其它格式（包括 PNG 的 `eXIf` chunk）
/// 不参与方向判定，直接返回 1。
fn get_exif_orientation(bytes: &[u8]) -> u8 {
    let mut tiff_offset: Option<usize> = None;

    // JPEG：以 FF D8 开头。
    if bytes.len() >= 2 && bytes[0] == 0xff && bytes[1] == 0xd8 {
        tiff_offset = find_jpeg_tiff_offset(bytes);
    }
    // WebP：以 RIFF....WEBP 开头。
    else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        tiff_offset = find_webp_tiff_offset(bytes);
    }

    match tiff_offset {
        None => 1,
        Some(offset) => read_orientation_from_tiff(bytes, offset as i64),
    }
}

/// `rotate90(photon, image, dstIndex)`：输出宽高互换的新图。
///
/// `dst_index(x, y, width, height)` 返回目标图（宽高互换后）里的像素线性下标（未乘 4）。
/// 下标用 `u64` 计算；转 `usize` 失败或超出缓冲区时跳过该像素，正常尺寸不会触发这个
/// 防御分支。
fn rotate90(image: &PhotonImage, dst_index: impl Fn(u32, u32, u32, u32) -> u64) -> PhotonImage {
    let width = image.get_width();
    let height = image.get_height();
    let source = image.get_raw_pixels();
    let mut destination = vec![0u8; source.len()];

    // 行优先遍历源图，每个像素按目标下标整块（4 字节 RGBA）拷贝。
    for y in 0..height {
        for x in 0..width {
            let source_index = (u64::from(y) * u64::from(width) + u64::from(x)) * 4;
            let destination_index = dst_index(x, y, width, height) * 4;
            let (Ok(source_index), Ok(destination_index)) = (
                usize::try_from(source_index),
                usize::try_from(destination_index),
            ) else {
                continue;
            };
            if source_index + 4 <= source.len() && destination_index + 4 <= destination.len() {
                destination[destination_index..destination_index + 4]
                    .copy_from_slice(&source[source_index..source_index + 4]);
            }
        }
    }

    // 宽高互换：新图的宽是源图的高。
    PhotonImage::new(destination, height, width)
}

/// `applyExifOrientation(photon, image, originalBytes)`：按 EXIF orientation（1..8）变换像素。
///
/// 翻转原地修改，旋转返回新图（旧图随 RAII 释放）。这是本模块唯一公开的 EXIF 函数；
/// `getExifOrientation` 保持私有（模块内测试覆盖）。
///
/// 入参 `image` 被消费：orientation 2/3/4 原地翻转后返回同一张图；5/6/7/8 返回宽高互换的
/// 新图；1、缺失或非法值返回原图。只改像素，不动 `original_bytes` 里的 EXIF 标记。
pub fn apply_exif_orientation(image: PhotonImage, original_bytes: &[u8]) -> PhotonImage {
    let orientation = get_exif_orientation(original_bytes);
    match orientation {
        2 => {
            let mut image = image;
            fliph(&mut image);
            image
        }
        3 => {
            let mut image = image;
            fliph(&mut image);
            flipv(&mut image);
            image
        }
        4 => {
            let mut image = image;
            flipv(&mut image);
            image
        }
        5 => {
            let mut rotated = rotate90(&image, |x, y, _width, height| {
                u64::from(x) * u64::from(height) + u64::from(height - 1 - y)
            });
            fliph(&mut rotated);
            rotated
        }
        6 => rotate90(&image, |x, y, _width, height| {
            u64::from(x) * u64::from(height) + u64::from(height - 1 - y)
        }),
        7 => {
            let mut rotated = rotate90(&image, |x, y, width, height| {
                u64::from(width - 1 - x) * u64::from(height) + u64::from(y)
            });
            fliph(&mut rotated);
            rotated
        }
        8 => rotate90(&image, |x, y, width, height| {
            u64::from(width - 1 - x) * u64::from(height) + u64::from(y)
        }),
        // orientation 1 与任何非法值都返回原图。
        _ => image,
    }
}

// ===========================================================================
// image-convert.ts
// ===========================================================================

/// `encodePng(photon, bytes)`：解码 → 应用 EXIF 方向 → 编码 PNG。
///
/// 本函数和 [`convert_image_bytes_to_png`] 一样先检查 `load_photon()`；区别只在于这里负责
/// 解码 → 应用 EXIF 方向 → PNG 编码，调用方不必预先检查 photon。
/// 解码/编码的 panic 仍由 `catch_silent` 吞掉并返回 `None`。
fn encode_png(bytes: &[u8]) -> Option<Vec<u8>> {
    load_photon()?;
    catch_silent(AssertUnwindSafe(|| {
        let raw_image = PhotonImage::new_from_byteslice(bytes.to_vec());
        let image = apply_exif_orientation(raw_image, bytes);
        image.get_bytes()
    }))
    .ok()
}

/// `convertImageBytesToPng(bytes)`：任意可解码图片 → PNG 字节；失败 `None`。
///
/// 会先应用 EXIF 方向再编码；无法解码（Photon 在坏图上 panic）时返回 `None`。
pub fn convert_image_bytes_to_png(bytes: &[u8]) -> Option<Vec<u8>> {
    let _photon = load_photon()?;
    encode_png(bytes)
}

/// `convertToPng` 的结果（`{ data, mimeType }`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PngConversion {
    /// base64 编码的 PNG 数据。
    pub data: String,
    /// 恒为 `image/png`。
    pub mime_type: String,
}

/// `convertToPng(base64Data, mimeType)`：非 PNG 转为 PNG；已是 `image/png` 原样返回。
///
/// `mime_type == "image/png"` 是严格字符串相等：带参数（`image/png; charset=binary`）或
/// 大小写不同的输入都会走解码 → 重编码。输入 base64 用 Node 风格的宽松解码
///（`kiana_tui::terminal_image::base64_decode`）：忽略非法字符、接受 URL-safe 字母表，
/// 尾组多余低位不校验（实现注释里列出了与 V8 对齐的依据）。
pub fn convert_to_png(base64_data: &str, mime_type: &str) -> Option<PngConversion> {
    // 已是 PNG，无需转换（严格相等，不做 base 归一化）。
    if mime_type == "image/png" {
        return Some(PngConversion {
            data: base64_data.to_string(),
            mime_type: "image/png".to_string(),
        });
    }

    let bytes = kiana_tui::terminal_image::base64_decode(base64_data);
    let png_bytes = convert_image_bytes_to_png(&bytes)?;
    Some(PngConversion {
        data: base64_encode(&png_bytes),
        mime_type: "image/png".to_string(),
    })
}

/// `loadPngTranscoder()`：返回同步 PNG 转码器（Kitty 终端用），photon 不可用时 `None`。
///
/// 原生构建里 photon 总是可用，因此实际总是 `Some`；保留 `Option` 以对齐 TS 的签名与
/// [`ensure_png_transcoder`] 的失败分支。
///
/// 返回的闭包签名是 `(base64 数据, MIME) -> Option<base64 PNG>`，解码/编码失败返回 `None`。
/// 闭包不捕获状态，本身可跨线程移动；但 `kiana-tui` 的转码器槽位是线程本地的，注册动作
/// （[`ensure_png_transcoder`]）仍要在 TUI 线程完成。
pub fn load_png_transcoder() -> Option<ImageTranscoder> {
    let _photon = load_photon()?;
    Some(Arc::new(|base64_data: &str, _mime_type: &str| {
        // TS：`new Uint8Array(Buffer.from(base64Data, "base64"))`——Node 的宽松解码。
        let bytes = kiana_tui::terminal_image::base64_decode(base64_data);
        encode_png(&bytes).map(|png_bytes| base64_encode(&png_bytes))
    }))
}

thread_local! {
    /// TS 的 `pngTranscoderLoad` + `pngTranscoderRegistered`：
    /// 首次尝试后不再重试（失败也缓存），注册成功后后续调用直接返回。
    static PNG_TRANSCODER_STATE: std::cell::RefCell<PngTranscoderState> =
        const { std::cell::RefCell::new(PngTranscoderState { attempted: false, registered: false }) };
}

/// `attempted`：是否已经尝试过加载（失败也保持 true）；`registered`：转码器是否已写入
/// `kiana-tui` 的线程本地槽位。
struct PngTranscoderState {
    attempted: bool,
    registered: bool,
}

/// `ensurePngTranscoder(onRegistered)`：在 Kitty 协议终端上注册 photon PNG 转码器。
///
/// TS 的早退规则：**已注册或非 kitty** → 直接返回（不再调用 `onRegistered`）；首次调用加载
/// 成功后才调用。原生加载是同步的，所以不存在 TS 的「加载进行中」窗口——本函数同步完成注册并
/// 直接调用 `on_registered`（TS 是 `loadPngTranscoder().then(...)`）。加载失败也缓存（TS 的
/// `pngTranscoderLoad` 同样不会再重试）。
/// `set_image_transcoder` 是 `kiana-tui` 的 thread-local，必须在 TUI 线程调用。
///
/// 状态转换（`PNG_TRANSCODER_STATE`，同样线程本地）：
/// 1. 终端不是 Kitty → 直接返回（连 `attempted` 都不置真，换终端能力后可再试）；
/// 2. `registered` 或 `attempted` → 直接返回，不调用回调；
/// 3. 首次进入：先置 `attempted = true`，再尝试加载；成功则写入转码器槽位、置
///    `registered = true`，返回后调用一次 `on_registered`；
/// 4. 加载失败：只留下 `attempted`，之后不再重试，也不调用回调。
///
/// 回调只在**本次调用完成注册**时执行，用于让调用方重绘此前显示为文本兜底的图片。
pub fn ensure_png_transcoder(on_registered: impl FnOnce()) {
    if get_capabilities().images != Some(ImageProtocol::Kitty) {
        return;
    }

    // 判断与注册在同一段借用里完成；返回值表示“这次调用完成了注册”，用于决定是否回调。
    let registered_now = PNG_TRANSCODER_STATE.with(|state| {
        let mut state = state.borrow_mut();
        if state.registered || state.attempted {
            return false;
        }
        state.attempted = true;
        if let Some(transcoder) = load_png_transcoder() {
            set_image_transcoder(Some(transcoder));
            state.registered = true;
            return true;
        }
        false
    });

    if registered_now {
        on_registered();
    }
}

// ===========================================================================
// image-process.ts
// ===========================================================================

/// `ProcessImageOptions`。
///
/// 两个字段都是 `Option`，`None` 表示使用默认行为：`auto_resize_images` 默认 `true`，
/// `resize_options` 缺省时由 [`ImageResizeOptions::default`] 提供上限与体积预算。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProcessImageOptions {
    /// 是否缩放到内联 provider 限额。默认 `true`。
    pub auto_resize_images: Option<bool>,
    /// 可选的缩放覆盖参数；缺省用 [`ImageResizeOptions::default`]。
    pub resize_options: Option<ImageResizeOptions>,
}

/// `ProcessImageResult`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessImageResult {
    /// 成功：base64 数据、MIME 类型与提示。
    Ok {
        /// base64 编码的图片数据。
        data: String,
        /// MIME 类型。
        mime_type: String,
        /// 提示文本（转换提示在前，尺寸提示在后）。
        hints: Vec<String>,
    },
    /// 失败：逐字的用户可见文案。
    Err {
        /// 失败文案。
        message: String,
    },
}

/// 规范化结果：`bytes` 是后续要内联/缩放的字节；`converted_from` 只在发生格式转换时记录
/// 原始基础 MIME，供生成转换提示。
#[derive(Debug, Clone, PartialEq, Eq)]
struct NormalizedImage {
    bytes: Vec<u8>,
    mime_type: String,
    converted_from: Option<String>,
}

/// `baseMimeType(mimeType)`：去掉 `;` 参数、trim、小写。
fn base_mime_type(mime_type: &str) -> String {
    mime_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_lowercase()
}

/// `normalizeSupportedImageMimeType(mimeType)`。
fn normalize_supported_image_mime_type(mime_type: &str) -> Option<&'static str> {
    match base_mime_type(mime_type).as_str() {
        "image/png" => Some("image/png"),
        "image/jpeg" | "image/jpg" => Some("image/jpeg"),
        "image/gif" => Some("image/gif"),
        "image/webp" => Some("image/webp"),
        _ => None,
    }
}

/// `normalizeImage(bytes, mimeType)`：受支持格式原样；否则尝试转 PNG。
///
/// 受支持格式只做 MIME 归一化（返回静态字符串），**不校验字节内容**：只有启用缩放时才
/// 会在解码/编码阶段失败；`auto_resize_images == false` 时不做校验，坏字节会原样内联。
/// 只有需要转换时才真正解码。
fn normalize_image(bytes: &[u8], mime_type: &str) -> Option<NormalizedImage> {
    if let Some(normalized_mime_type) = normalize_supported_image_mime_type(mime_type) {
        return Some(NormalizedImage {
            bytes: bytes.to_vec(),
            mime_type: normalized_mime_type.to_string(),
            converted_from: None,
        });
    }

    let png_bytes = convert_image_bytes_to_png(bytes)?;
    Some(NormalizedImage {
        bytes: png_bytes,
        mime_type: "image/png".to_string(),
        converted_from: Some(base_mime_type(mime_type)),
    })
}

/// `conversionHint(from, to)`：无 from 或相等 → `None`。
fn conversion_hint(from: Option<&str>, to: &str) -> Option<String> {
    match from {
        None | Some("") => None,
        Some(from) if from == to => None,
        Some(from) => Some(format!("[Image converted from {from} to {to}.]")),
    }
}

/// `processImage(bytes, mimeType, options?)`：把任意图片规范成可内联的 base64 载荷。
///
/// 失败文案逐字：
/// - 无法转换：`[Image omitted: could not be converted to a supported inline image format.]`
/// - 无法缩到限额内：`[Image omitted: could not be resized below the inline image size limit.]`
///
/// hints 顺序：转换提示（`from != to`）在前，尺寸提示在后。
///
/// 处理顺序固定：**先规范化，再（可选）缩放**——即使 `auto_resize_images == false`，不支持的
/// 格式（如 `image/bmp`）仍会被转成 PNG，因为内联只接受少数几种格式。
///
/// `resize_options` 只在缩放分支使用，不缩放时被忽略。缩放分支里转换提示的目标类型取
/// `resized.mime_type`（可能因体积原因变成 `image/jpeg`），而不是中间规范化得到的
/// `image/png`。
///
/// 失败时返回面向用户/模型的整段文案而不是错误类型：`kiana-tools` 的 `read` 会把它拼进
/// 文本结果，`agent_session` 会把它当成 hint。
pub fn process_image(
    bytes: &[u8],
    mime_type: &str,
    options: Option<ProcessImageOptions>,
) -> ProcessImageResult {
    let auto_resize_images = options
        .as_ref()
        .and_then(|options| options.auto_resize_images)
        .unwrap_or(true);
    let resize_options = options.and_then(|options| options.resize_options);

    let Some(normalized) = normalize_image(bytes, mime_type) else {
        return ProcessImageResult::Err {
            message: "[Image omitted: could not be converted to a supported inline image format.]"
                .to_string(),
        };
    };

    // 规范化先于缩放：不支持的格式到这里已经变成 PNG。
    if auto_resize_images {
        let Some(resized) = resize_image(&normalized.bytes, &normalized.mime_type, resize_options)
        else {
            return ProcessImageResult::Err {
                message: "[Image omitted: could not be resized below the inline image size limit.]"
                    .to_string(),
            };
        };

        let mut hints: Vec<String> = Vec::new();
        if let Some(hint) =
            conversion_hint(normalized.converted_from.as_deref(), &resized.mime_type)
        {
            hints.push(hint);
        }
        if let Some(note) = format_dimension_note(&resized) {
            hints.push(note);
        }

        ProcessImageResult::Ok {
            data: resized.data,
            mime_type: resized.mime_type,
            hints,
        }
    } else {
        // 不缩放：直接内联规范化后的字节，hints 只可能有转换提示。
        let mut hints: Vec<String> = Vec::new();
        if let Some(hint) =
            conversion_hint(normalized.converted_from.as_deref(), &normalized.mime_type)
        {
            hints.push(hint);
        }

        ProcessImageResult::Ok {
            data: base64_encode(&normalized.bytes),
            mime_type: normalized.mime_type,
            hints,
        }
    }
}

// ===========================================================================
// clipboard-image.ts
// ===========================================================================

/// `SUPPORTED_IMAGE_MIME_TYPES`。
///
/// **顺序即偏好顺序**：`select_preferred_image_mime_type` 按这个列表挑第一个匹配项。
const SUPPORTED_IMAGE_MIME_TYPES: [&str; 4] =
    ["image/png", "image/jpeg", "image/webp", "image/gif"];

/// `DEFAULT_LIST_TIMEOUT_MS`。
const DEFAULT_LIST_TIMEOUT_MS: u64 = 1_000;
/// `DEFAULT_POWERSHELL_TIMEOUT_MS`。
const DEFAULT_POWERSHELL_TIMEOUT_MS: u64 = 5_000;

/// `ClipboardImage`。
///
/// `bytes` 是原始图片字节（不是 base64）；`mime_type` 是基础 MIME（无参数、小写）：
/// wl-paste/xclip 后端由 `base_mime_type` 归一化，原生后端由魔数识别给出固定值，
/// PowerShell 后端固定为 `image/png`。
/// 读剪贴板时，不支持的格式（如 `image/bmp`）会被转成 PNG 后再返回。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardImage {
    /// 图片字节。
    pub bytes: Vec<u8>,
    /// MIME 类型。
    pub mime_type: String,
}

/// `readClipboardImage(options?)` 的选项。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReadClipboardImageOptions {
    /// 环境变量快照；`None` 读真实进程环境。
    pub env: Option<Vec<(OsString, OsString)>>,
    /// 平台覆盖；`None` 用 `process.platform`。
    pub platform: Option<String>,
}

/// `isWaylandSession(env)`：`WAYLAND_DISPLAY` 或 `XDG_SESSION_TYPE === "wayland"`。
///
/// `env` 为 `None` 时读真实进程环境；空值等同未设置（`env_truthy` 的语义）。
pub fn is_wayland_session(env: Option<&EnvMap>) -> bool {
    env_truthy(env, "WAYLAND_DISPLAY") || env_equals(env, "XDG_SESSION_TYPE", "wayland")
}

/// `extensionForImageMimeType(mimeType)`：受支持格式的扩展名；否则 `None`。
///
/// 参数先去掉 `;` 后缀并小写；返回的是不带点的扩展名。
pub fn extension_for_image_mime_type(mime_type: &str) -> Option<&'static str> {
    match base_mime_type(mime_type).as_str() {
        "image/png" => Some("png"),
        "image/jpeg" => Some("jpg"),
        "image/webp" => Some("webp"),
        "image/gif" => Some("gif"),
        _ => None,
    }
}

/// `selectPreferredImageMimeType(mimeTypes)`（私有）：按偏好顺序返回**原始字符串**。
///
/// 先 trim、丢弃空项，再按 `SUPPORTED_IMAGE_MIME_TYPES` 的顺序找第一个基础 MIME 完全相等的
/// 项（列表里没有 `image/jpg`，所以它不会被优先选中）；都不匹配时退回第一个 `image/` 开头
/// 的类型（`image/bmp` 就是这样被选中的）。返回原始字符串是为了把它原样传给命令行工具，
/// 调用方再自行 `base_mime_type`。
fn select_preferred_image_mime_type(mime_types: &[String]) -> Option<String> {
    let normalized: Vec<(&str, String)> = mime_types
        .iter()
        .map(|mime_type| mime_type.trim())
        .filter(|mime_type| !mime_type.is_empty())
        .map(|mime_type| (mime_type, base_mime_type(mime_type)))
        .collect();

    for preferred in SUPPORTED_IMAGE_MIME_TYPES {
        if let Some((raw, _)) = normalized
            .iter()
            .find(|(_, base)| base.as_str() == preferred)
        {
            return Some((*raw).to_string());
        }
    }

    normalized
        .iter()
        .find(|(_, base)| base.starts_with("image/"))
        .map(|(raw, _)| (*raw).to_string())
}

/// `isSupportedImageMimeType(mimeType)`（私有）。
fn is_supported_image_mime_type(mime_type: &str) -> bool {
    let base = base_mime_type(mime_type);
    SUPPORTED_IMAGE_MIME_TYPES.contains(&base.as_str())
}

/// clipboard-image.ts 的 `convertToPng(bytes)`（私有）：**不**应用 EXIF 方向。
///
/// 与 [`convert_image_bytes_to_png`] 的唯一差别就在这里：只解码再编码，不调用
/// `apply_exif_orientation`（TS 的 `clipboard-image.ts` 私有 `convertToPng` 同样不处理方向）。
fn convert_clipboard_image_to_png(bytes: &[u8]) -> Option<Vec<u8>> {
    load_photon()?;
    catch_silent(AssertUnwindSafe(|| {
        PhotonImage::new_from_byteslice(bytes.to_vec()).get_bytes()
    }))
    .ok()
}

/// TS 的三态：`undefined`（后端失败）/ `null`（后端可用但没有图）/ 图片。
///
/// - `Unavailable`：命令不存在、超时或非 0 退出 → 继续后面的回退；
/// - `Empty`：后端可用但剪贴板没有图片 → 不回到同级的另一个命令；
/// - `Image`：成功。
enum ClipboardProbe {
    Unavailable,
    Empty,
    Image(ClipboardImage),
}

/// JS 的真值判断：只有 `Image` 是 truthy。
///
/// 因此 `Empty` 也是 falsy：WSL 分支在「空」结果时仍会尝试 PowerShell，而
/// `matches!(..., Unavailable)` 只对失败做后续回退。
fn probe_truthy(probe: &ClipboardProbe) -> bool {
    matches!(probe, ClipboardProbe::Image(_))
}

/// `wl-paste` 输出的类型列表：`split(/\r?\n/).map(trim).filter(Boolean)`。
///
/// Rust 版按 `\n` 切分再 `trim`，效果等价（`trim` 会去掉行尾的 `\r`）；非 UTF-8 字节
/// 用替换字符处理，之后的 MIME 比较自然不匹配。
fn parse_mime_type_lines(bytes: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(bytes)
        .split('\n')
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect()
}

/// 用注入的 IO 执行一条剪贴板命令（默认 50 MB 输出上限）。
///
/// 第 4 个参数 `input` 传 `None`；输出上限由 `run_io_command` 固定为 `clipboard.rs` 的
/// `DEFAULT_MAX_BUFFER_BYTES`（50 MB），不由此函数设置；超限与超时都会让命令返回 `None`
/// （调用点归入 `Unavailable`）。
async fn run_image_command(
    io: &ClipboardTestIo,
    command: &str,
    args: Vec<String>,
    timeout_ms: u64,
) -> Option<Vec<u8>> {
    run_io_command(io, command, args, None, timeout_ms).await
}

/// 把 `&str` 参数拼成 `Vec<String>` 后转调 [`run_image_command`]。
async fn image_command(
    io: &ClipboardTestIo,
    command: &str,
    args: &[&str],
    timeout_ms: u64,
) -> Option<Vec<u8>> {
    run_image_command(
        io,
        command,
        args.iter().map(|arg| (*arg).to_string()).collect(),
        timeout_ms,
    )
    .await
}

/// `readClipboardImageViaWlPaste()`。
///
/// 先 `wl-paste --list-types`（1 秒超时）：命令失败 → `Unavailable`；没有可用图片类型 →
/// `Empty`（Wayland 上空的剪贴板不能落回旧的 X11 内容）。选中类型后再取数据（默认 3 秒
/// 超时）：失败 → `Unavailable`，空输出 → `Empty`。返回的 MIME 是去掉参数后的基础类型。
async fn read_clipboard_image_via_wl_paste(io: &ClipboardTestIo) -> ClipboardProbe {
    let Some(list) =
        image_command(io, "wl-paste", &["--list-types"], DEFAULT_LIST_TIMEOUT_MS).await
    else {
        return ClipboardProbe::Unavailable;
    };

    let types = parse_mime_type_lines(&list);
    let Some(selected_type) = select_preferred_image_mime_type(&types) else {
        return ClipboardProbe::Empty;
    };

    let Some(data) = run_image_command(
        io,
        "wl-paste",
        vec![
            "--type".to_string(),
            selected_type.clone(),
            "--no-newline".to_string(),
        ],
        DEFAULT_TIMEOUT_MS,
    )
    .await
    else {
        return ClipboardProbe::Unavailable;
    };
    if data.is_empty() {
        return ClipboardProbe::Empty;
    }

    ClipboardProbe::Image(ClipboardImage {
        bytes: data,
        mime_type: base_mime_type(&selected_type),
    })
}

/// `readClipboardImageViaXclip()`。
///
/// `xclip -selection clipboard -t TARGETS -o`（1 秒超时）列类型：失败 → `Unavailable`；
/// 没有图片类型 → `Empty`。选中后按同一 selection 取数据（默认 3 秒超时）：失败 →
/// `Unavailable`，空输出 → `Empty`。
async fn read_clipboard_image_via_xclip(io: &ClipboardTestIo) -> ClipboardProbe {
    let Some(targets) = image_command(
        io,
        "xclip",
        &["-selection", "clipboard", "-t", "TARGETS", "-o"],
        DEFAULT_LIST_TIMEOUT_MS,
    )
    .await
    else {
        return ClipboardProbe::Unavailable;
    };

    let candidate_types = parse_mime_type_lines(&targets);
    let Some(preferred) = select_preferred_image_mime_type(&candidate_types) else {
        return ClipboardProbe::Empty;
    };

    let Some(data) = run_image_command(
        io,
        "xclip",
        vec![
            "-selection".to_string(),
            "clipboard".to_string(),
            "-t".to_string(),
            preferred.clone(),
            "-o".to_string(),
        ],
        DEFAULT_TIMEOUT_MS,
    )
    .await
    else {
        return ClipboardProbe::Unavailable;
    };
    if data.is_empty() {
        return ClipboardProbe::Empty;
    }

    ClipboardProbe::Image(ClipboardImage {
        bytes: data,
        mime_type: base_mime_type(&preferred),
    })
}

/// `readClipboardImageViaPowerShell()`：WSL 上通过 PowerShell 读 Windows 剪贴板的图片。
///
/// 临时文件放在 `node_tmpdir()`，名字带 UUID；无论成功失败，返回前都会删除它
///（忽略删除错误，对应 TS 的 `finally { unlinkSync } catch {}`）。
async fn read_clipboard_image_via_powershell(io: &ClipboardTestIo) -> Option<ClipboardImage> {
    let tmp_file = node_tmpdir().join(format!("kiana-wsl-clip-{}.png", uuid::Uuid::new_v4()));
    let result = read_clipboard_image_via_powershell_inner(io, &tmp_file).await;
    // finally：忽略清理错误。
    let _ = std::fs::remove_file(&tmp_file);
    result
}

async fn read_clipboard_image_via_powershell_inner(
    io: &ClipboardTestIo,
    tmp_file: &std::path::Path,
) -> Option<ClipboardImage> {
    // 先把 Linux 路径转成 Windows 路径；wslpath 失败或输出为空就没有可写的目标。
    let win_path_result = run_image_command(
        io,
        "wslpath",
        vec!["-w".to_string(), tmp_file.to_string_lossy().into_owned()],
        DEFAULT_LIST_TIMEOUT_MS,
    )
    .await?;
    let win_path = String::from_utf8_lossy(&win_path_result).trim().to_string();
    if win_path.is_empty() {
        return None;
    }

    // PowerShell 单引号字符串里的 ' 要用两个 ' 转义，避免路径提前结束字符串。
    let quoted_win_path = win_path.replace('\'', "''");
    let ps_script = [
        "Add-Type -AssemblyName System.Windows.Forms",
        "Add-Type -AssemblyName System.Drawing",
        &format!("$path = '{quoted_win_path}'"),
        "$img = [System.Windows.Forms.Clipboard]::GetImage()",
        "if ($img) { $img.Save($path, [System.Drawing.Imaging.ImageFormat]::Png); Write-Output 'ok' } else { Write-Output 'empty' }",
    ]
    .join("; ");

    let result = run_image_command(
        io,
        "powershell.exe",
        vec!["-NoProfile".to_string(), "-Command".to_string(), ps_script],
        DEFAULT_POWERSHELL_TIMEOUT_MS,
    )
    .await?;
    let output = String::from_utf8_lossy(&result).trim().to_string();
    // 脚本只在成功保存时输出 ok；'empty' 或其它输出都当作没有图片。
    if output != "ok" {
        return None;
    }

    // 文件由 PowerShell 写出；读取失败或空文件同样按失败处理。
    let bytes = std::fs::read(tmp_file).ok()?;
    if bytes.is_empty() {
        return None;
    }
    Some(ClipboardImage {
        bytes,
        mime_type: "image/png".to_string(),
    })
}

/// `readClipboardImageViaNativeClipboard()`：原生剪贴板（三态必须保留；拒绝向上传播）。
///
/// 原生后端不做回退：`get_image()` 的 `Err` 直接 `?` 返回（TS 的 Promise 拒绝同样不被吞）。
/// 空字节数组 → `Empty`；魔数无法识别时 MIME 记为 `application/octet-stream`，由调用方决定
/// 是否尝试转 PNG。
async fn read_clipboard_image_via_native(
    io: &ClipboardTestIo,
) -> Result<ClipboardProbe, ClipboardError> {
    let Some(clipboard) = &io.native else {
        return Ok(ClipboardProbe::Unavailable);
    };
    match clipboard.get_image().await? {
        ClipboardValue::Unavailable => Ok(ClipboardProbe::Unavailable),
        ClipboardValue::Empty => Ok(ClipboardProbe::Empty),
        ClipboardValue::Value(bytes) => {
            if bytes.is_empty() {
                return Ok(ClipboardProbe::Empty);
            }
            let mime_type = detect_supported_image_mime_type(&bytes)
                .unwrap_or_else(|| "application/octet-stream".to_string());
            Ok(ClipboardProbe::Image(ClipboardImage { bytes, mime_type }))
        }
    }
}

/// `readClipboardImage(options?)`：读取系统剪贴板中的图片。
///
/// Linux 的顺序：Wayland/WSL → `wl-paste`；不可用时 → `xclip`；WSL 且没有图 →
/// PowerShell（保留 Linux 的「空/不可用」区分）；仍不可用 → 原生剪贴板。其它平台直接读原生。
/// 不支持的格式（例如 Windows 把 DIB 包装成 BMP）用 Photon 转 PNG，失败返回 `Ok(None)`。
///
/// **错误**：TS 的这个函数在原生 `getImage()` 拒绝时抛出（测试
/// `propagates native transfer errors without fallback`）；Rust 用 `Err(ClipboardError)` 表达
/// 同一次拒绝（`Ok(None)` 对应 TS 的 `null`）。
///
/// `TERMUX_VERSION` 存在时直接返回 `Ok(None)`（Termux 没有可用的图片剪贴板），不会执行任何
/// 外部命令，也不会读原生剪贴板。
pub async fn read_clipboard_image(
    options: ReadClipboardImageOptions,
) -> Result<Option<ClipboardImage>, ClipboardError> {
    let mut io = ClipboardTestIo::from_process();
    if let Some(platform) = options.platform {
        io.platform = platform;
    }
    if let Some(env) = options.env {
        io.env = Some(env);
    }
    read_clipboard_image_with(&io).await
}

/// [`read_clipboard_image`] 的注入版本（测试用）。
///
/// 回退顺序（Linux；第 1 步对所有平台生效）：
/// 1. `TERMUX_VERSION` 存在 → 立即 `Ok(None)`；
/// 2. Wayland 或 WSL → `wl-paste`；
/// 3. 上一步是 `Unavailable` → `xclip`；
/// 4. 结果不是图片且是 WSL → PowerShell（`Empty` 也会尝试，以保留 Linux 的空/不可用区分）；
/// 5. 仍是 `Unavailable` → 原生剪贴板（错误向上传播）。
///
/// 非 Linux 平台只走原生剪贴板。最终不是 `Image`（`Empty` 或 `Unavailable`）→ `Ok(None)`。
/// `Image` 的 MIME 不在受支持列表时再用 Photon 转 PNG，转换失败也返回 `Ok(None)`。
#[doc(hidden)]
pub async fn read_clipboard_image_with(
    io: &ClipboardTestIo,
) -> Result<Option<ClipboardImage>, ClipboardError> {
    let env = io.env.as_deref();

    // Termux 没有系统图片剪贴板；提前返回，避免执行任何命令，也不碰原生剪贴板。
    if env_truthy(env, "TERMUX_VERSION") {
        return Ok(None);
    }

    let mut image = ClipboardProbe::Unavailable;
    if io.platform == "linux" {
        // WSL 上 Windows 截图不会进入 Linux 剪贴板，所以 WSL 也走命令后端，并额外试
        // PowerShell（见下面第 4 步）。
        let wsl = crate::changelog::is_wsl(env);
        if is_wayland_session(env) || wsl {
            image = read_clipboard_image_via_wl_paste(io).await;
        }
        if matches!(image, ClipboardProbe::Unavailable) {
            image = read_clipboard_image_via_xclip(io).await;
        }
        // 保留 Linux 的「空/不可用」区分：Windows 没有图时仍回落到 Linux 的结果。
        if !probe_truthy(&image)
            && wsl
            && let Some(found) = read_clipboard_image_via_powershell(io).await
        {
            image = ClipboardProbe::Image(found);
        }
        if matches!(image, ClipboardProbe::Unavailable) {
            // 命令后端都失败才读原生；原生错误用 `?` 直接返回。
            image = read_clipboard_image_via_native(io).await?;
        }
    } else {
        // 其它平台只信任原生剪贴板。
        image = read_clipboard_image_via_native(io).await?;
    }

    // Unavailable/Empty 都映射为“没有图片”；只有 Image 继续做格式检查。
    let ClipboardProbe::Image(image) = image else {
        return Ok(None);
    };

    // 转换不支持的格式（例如 Windows 以 BMP 包装的 DIB 数据）到 PNG。
    if !is_supported_image_mime_type(&image.mime_type) {
        let Some(png_bytes) = convert_clipboard_image_to_png(&image.bytes) else {
            return Ok(None);
        };
        return Ok(Some(ClipboardImage {
            bytes: png_bytes,
            mime_type: "image/png".to_string(),
        }));
    }

    Ok(Some(image))
}

// ===========================================================================
// tool-result-images.ts
// ===========================================================================

/// `ToolResultContent = TextContent | ImageContent`（`kiana_ai::UserContentBlock` 的同一联合）。
pub type ToolResultContent = UserContentBlock;

/// `NormalizeToolResultImagesOptions`。
///
/// `resize_options` 的类型是 `kiana_ai` 的模型配置（`u64`），内部会转换成
/// [`ImageResizeOptions`]（`u32`，超出 `u32` 范围的上限钳到 `u32::MAX`）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NormalizeToolResultImagesOptions {
    /// 是否把过大的图片缩到内联 provider 限额。默认 `true`。
    pub auto_resize_images: Option<bool>,
    /// 模型相关的缩放配置；缺省用保守的内置默认值。
    pub resize_options: Option<ModelImageResizeOptions>,
}

/// `ModelImageResizeOptions` → [`ImageResizeOptions`]（缺省字段回落到 TS 的 `DEFAULT_OPTIONS`）。
///
/// `max_width` / `max_height` 是 `u64`：超出 `u32` 时钳为 `u32::MAX`（而不是回落到默认值）；
/// `max_bytes` 与 `jpeg_quality` 直接沿用，缺失字段用 [`ImageResizeOptions::default`]。
fn image_resize_options_from_model(model: &ModelImageResizeOptions) -> ImageResizeOptions {
    let defaults = ImageResizeOptions::default();
    ImageResizeOptions {
        max_width: model
            .max_width
            .map(|value| u32::try_from(value).unwrap_or(u32::MAX))
            .unwrap_or(defaults.max_width),
        max_height: model
            .max_height
            .map(|value| u32::try_from(value).unwrap_or(u32::MAX))
            .unwrap_or(defaults.max_height),
        max_bytes: model.max_bytes.unwrap_or(defaults.max_bytes),
        jpeg_quality: model.jpeg_quality,
    }
}

/// `normalizeToolResultImages(content, options?)`：在图片进入会话历史前规范化一次。
///
/// **为什么需要它**：`read` 工具会走 [`process_image`]，但扩展、MCP 桥、截图工具自己产出的
/// 图片块可能是任意 base64，会直接进入会话历史并出现在之后每次 provider 请求里；超大图片
/// 会让 provider 拒绝整个会话而不只是当前轮（`utils/tool-result-images.ts` 的文档注释
/// 说明了这个动机）。所以进入历史前统一规范化一次。
///
/// - 没有 image 块 → **借用原切片**（TS 返回同一数组引用）；
/// - 非 image 块原样保留；image 块走 [`process_image`]；
/// - 处理失败 → 保留原块（不删除工具输出）；
/// - 成功且 `data`/`mime_type` 未变且 hints 为空 → 保留原块；
/// - 否则替换为新块，并在 hints 非空时追加 `{type:"text", text: hints.join("\n")}`；
/// - 只有真正发生替换时才返回 `Cow::Owned`（调用方用 `matches!(.., Cow::Borrowed(_))`
///   判断是否改写，对应 TS 的 `normalizedContent === content`）。
pub fn normalize_tool_result_images<'a>(
    content: &'a [ToolResultContent],
    options: Option<NormalizeToolResultImagesOptions>,
) -> Cow<'a, [ToolResultContent]> {
    if !content
        .iter()
        .any(|block| matches!(block, UserContentBlock::Image(_)))
    {
        return Cow::Borrowed(content);
    }

    let auto_resize_images = options
        .as_ref()
        .and_then(|options| options.auto_resize_images)
        .unwrap_or(true);
    let resize_options = options
        .as_ref()
        .and_then(|options| options.resize_options.as_ref())
        .map(image_resize_options_from_model);

    let mut normalized: Vec<ToolResultContent> = Vec::with_capacity(content.len());
    let mut changed = false;

    for block in content {
        let UserContentBlock::Image(image) = block else {
            normalized.push(block.clone());
            continue;
        };

        // `Buffer.from(block.data, "base64")`（Node 的宽松解码）。
        let bytes = kiana_tui::terminal_image::base64_decode(&image.data);
        let processed = process_image(
            &bytes,
            &image.mime_type,
            Some(ProcessImageOptions {
                auto_resize_images: Some(auto_resize_images),
                resize_options,
            }),
        );

        let ProcessImageResult::Ok {
            data,
            mime_type,
            hints,
        } = processed
        else {
            // 与 `read` 不同：保留原块。工具已经产出了这张图，失败可能只是图片后端不可用，
            // 静默删除会改变工具现有的行为。
            normalized.push(block.clone());
            continue;
        };

        // 内容与 MIME 都没变且没有提示时保留原块，避免无谓地复制整个结果数组。
        if data == image.data && mime_type == image.mime_type && hints.is_empty() {
            normalized.push(block.clone());
            continue;
        }

        normalized.push(UserContentBlock::Image(ImageContent {
            data,
            mime_type,
            ..Default::default()
        }));
        if !hints.is_empty() {
            normalized.push(UserContentBlock::Text(TextContent {
                text: hints.join("\n"),
                ..Default::default()
            }));
        }
        // 只有真正替换/追加了块才置 changed，最终决定返回 Borrowed 还是 Owned。
        changed = true;
    }

    if changed {
        Cow::Owned(normalized)
    } else {
        Cow::Borrowed(content)
    }
}
