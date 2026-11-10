//! `images/node.ts` / `images/node-worker.ts` / `images/index.ts`——Photon 等价物。
//!
//! ## 后端选择（`ARCHITECTURE.md` §8 第 20 条）
//!
//! TS 依赖 `@silvia-odwyer/photon`（0.3.3，durable）与 `photon-node`（0.3.4，coding-agent）的
//! **wasm**。Rust 侧使用同一个 Photon 源码的 crate `photon-rs`（0.3.3），**原生编译**：
//!
//! - `resize(..., SamplingFilter::Lanczos3)` 内部就是 `image` crate 的 `imageops::resize`，
//!   与 wasm 版是同一份代码路径；
//! - 解码用 `photon-rs` 依赖的同一份 `image` 0.24（`decode_image`），JPEG 还固定到 wasm
//!   构建里的 `jpeg-decoder` 0.3.1 并启用 `platform_independent`——wasm32 上没有 x86 SSE2 的
//!   IDCT/YCbCr 快速路径，原生默认会走它并差出末位；
//! - `image` 0.25 的 PNG 编码器输出与 `photon-rs` 的 `image` 0.24 输出**逐字节相同**，
//!   所以 PNG/JPEG 的编码层不需要额外适配。
//!
//! 因此 `prepare()` 的“尺寸/格式/是否重缩放/base64 长度在限制内”与参考实现一致；
//! `tests/fixtures/tools_images_exact.json` 由 TS 的 wasm 实现生成，断言 JPEG（含 EXIF 方向、
//! 缩放、字节限额）与 PNG→JPEG 重编码的输出**逐字节相同**（`tests/tools_images.rs`）。
//!
//! ## 无法解码的图片
//!
//! `PhotonImage::new_from_byteslice` 内部是 `image::load_from_memory(...).unwrap()`，失败会
//! panic 并把 panic 信息写到 stderr；TS 的 `catch { return undefined; }` 没有任何输出。所以
//! `decode_image` 直接用同一份 `image` 做可失败解码，失败安静地返回 `None`。
//!
//! ## 没有 worker 线程
//!
//! TS 用 worker 是因为 Photon 的 wasm 线性内存永不回缩（大图会一直占着）。原生实现没有这个
//! 问题，`create_node_photon_images` 的 `worker` 选项仍然保留（默认 `true`）以维持 API
//! 形状，但始终走调用线程；`idle_ms` 同理（对原生实现无意义）。
//!
//! ## 执行流程（[`NodePhotonImages::prepare`]）
//!
//! 1. 用 `exif_orientation` 读方向（纯字节解析，不依赖 Photon）；
//! 2. 用 `image` 0.24 解码为 RGBA8（`decode_image`），失败立即返回 `None`；
//! 3. **原样直通**：输入 MIME 属于 `INLINE_IMAGE_TYPES`、方向为 1、宽高都在上限内、且
//!    base64 长度不超过 `maxBytes` 时，直接返回原字节的 base64，不重新编码；
//! 4. 否则按 EXIF 方向扶正：5..=8 走 `transposed`（会交换宽高），2/3/4 用水平/垂直翻转
//!    组合（3 是两者都翻）；
//! 5. 按 `maxWidth`/`maxHeight` 计算等比缩放系数（最多 1.0，只缩不放），四舍五入后每边
//!    至少保留 1 像素；
//! 6. 在当前尺寸下按固定顺序尝试编码（见 `encode_within`）；没有一个候选满足字节上限时，
//!    把尺寸乘 0.75 向下取整再来一轮，直到成功或已经试过 1x1（1x1 也放不下则返回 `None`）。
//!
//! 第 3 步**要求先解码**：一张无法解码的图片即使字节本身在上限内也不会直通，而是返回
//! `None`。工具层因此会把“坏图片”报成错误而不是把原始字节发给模型
//! （`tests/tools_images.rs` 的 `reports_bytes_it_cannot_decode_as_an_error`）。
//!
//! ## 输出字段的含义
//!
//! - `PreparedImage::data` 是 base64 文本（标准字母表、带 `=` 补齐），不是原始字节，也
//!   没有 data URI 前缀；
//! - `PreparedImage::mime_type` 是**输出**格式，可能与入参不同（例如 BMP→PNG、PNG→JPEG）；
//! - `PreparedImage::converted_from` 只在输出格式与入参不同时是 `Some(入参 MIME)`；
//! - `PreparedImage::resized.from` 是**扶正之后**的宽高；仅因 EXIF 方向导致宽高交换不算
//!   “resized”（`tests/fixtures/tools_images_exact.json` 里 orientation-5..8 的 `resized`
//!   全为 `null`）；
//! - `ImageLimits::max_bytes` 比较的是 base64 字符串的字节长度（`base64_length`），不是
//!   编码后的原始字节数。
//!
//! ## 调用方与阻塞行为
//!
//! 本 crate 内的消费方是 `crate::tools::read` 的 `read_image`：它通过 `ImageProcessor` trait
//! 调用本模块，`None` 会被转成 “cannot be prepared for the model” 的工具错误。trait 虽为
//! `async`，实现里只是同步调用 [`NodePhotonImages::prepare`]，解码、扶正、缩放与编码全部
//! 在轮询该 future 的执行器线程上完成，不会切到阻塞线程池。
//!
//! ## 未开启 `images` feature 时
//!
//! `photon-rs` 是可选依赖；没有它时 [`NodePhotonImages::prepare`] 一律返回 `None`（与“无法
//! 解码”相同），而 [`crate::images::exif::exif_orientation`] 仍然可用。

#[cfg(feature = "images")]
use photon_rs::PhotonImage;
#[cfg(feature = "images")]
use photon_rs::transform::{SamplingFilter, fliph, flipv, resize};

#[cfg(feature = "images")]
use crate::images::exif::exif_orientation;
#[cfg(feature = "images")]
use crate::tools::image::{INLINE_IMAGE_TYPES, base64_length, to_base64};

/// `createNodePhotonImages` 的选项；原生实现只保存、不使用。
///
/// 两个字段都只为维持 TS 的调用形状（见模块文档“没有 worker 线程”），由
/// [`NodePhotonImages::options`] 原样返回。
#[derive(Debug, Clone)]
pub struct NodePhotonImagesOptions {
    /// 默认 `true`（TS 用 worker 线程）。Rust 原生实现忽略此值，保留字段以对齐 API。
    pub worker: bool,
    /// 默认 30_000。Rust 原生实现忽略此值。
    pub idle_ms: u64,
}

impl Default for NodePhotonImagesOptions {
    fn default() -> Self {
        NodePhotonImagesOptions {
            worker: true,
            idle_ms: 30_000,
        }
    }
}

/// `ImageLimits`（`tools/image-processor.ts`）：provider 的内联图片上限。
///
/// 三个字段由 `tools::read` 从模型目录的 `input_limits.images.resize` 填充；模型没有声明
/// 时回落到 `DEFAULT_IMAGE_LIMITS`（2000×2000、4.5 MiB）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageLimits {
    /// 允许的最大宽度（像素）；超过时等比缩小，不会放大。
    pub max_width: u32,
    /// 允许的最大高度（像素）；超过时等比缩小，不会放大。
    pub max_height: u32,
    /// base64 字符串的**字节长度**上限（不是原始字节数）。
    pub max_bytes: usize,
}

/// `prepare` 的成功结果：可以直接作为图片附件交给模型的 base64 数据。
///
/// 对应 TS 的 `PreparedImage`；`tools::read` 用它拼装图片块，并把 `resized`/`converted_from`
/// 变成给用户看的诊断文案。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedImage {
    /// 标准 base64 文本（无 data URI 前缀）。
    pub data: String,
    /// **输出** MIME；重编码后可能与调用方传入的 `mime_type` 不同。
    pub mime_type: String,
    /// `resized: { from, to }`：只有重新缩放过才存在（`from` 是扶正后的尺寸）。
    pub resized: Option<ResizedDimensions>,
    /// `convertedFrom`：只有输出格式与输入不同时才存在，值为输入 MIME。
    pub converted_from: Option<String>,
}

/// `resized: { from: {width,height}, to: {width,height} }`。
///
/// 两组都是像素。`from` 是 EXIF 扶正之后的宽高（方向 5..=8 已交换），`to` 是本次实际
/// 编码的尺寸。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResizedDimensions {
    /// 扶正后的原始尺寸 `(width, height)`。
    pub from: (u32, u32),
    /// 实际输出的尺寸 `(width, height)`。
    pub to: (u32, u32),
}

/// `createPhotonImages(wasm)`：TS 接收已实例化的 wasm 模块；Rust 侧原生链接，没有这个参数。
///
/// 与 [`create_node_photon_images`] 返回同一类型：两者在 TS 里的差异只在于 Photon 的初始化方式
/// （wasm 模块 vs 惰性单例），`prepare` 的行为完全相同。
pub fn create_photon_images() -> NodePhotonImages {
    create_node_photon_images(NodePhotonImagesOptions::default())
}

/// `createNodePhotonImages(options)`：Node 后端的等价物。
///
/// 只保存选项（可通过 [`NodePhotonImages::options`] 读回），不创建线程，也不初始化任何
/// 全局状态；真正的解码/编码发生在每次 [`NodePhotonImages::prepare`] 调用里。
pub fn create_node_photon_images(options: NodePhotonImagesOptions) -> NodePhotonImages {
    NodePhotonImages { options }
}

/// `createNodePhotonImages()` 的返回值。
///
/// 持有创建时的选项；没有内部可变状态，`Clone` 出的副本行为一致。
#[derive(Debug, Clone)]
pub struct NodePhotonImages {
    options: NodePhotonImagesOptions,
}

impl NodePhotonImages {
    /// 创建时传入的选项（原生实现只保存、不使用，见 [`NodePhotonImagesOptions`]）。
    pub fn options(&self) -> &NodePhotonImagesOptions {
        &self.options
    }

    /// `prepare(bytes, mimeType, limits)`：解码 → EXIF 扶正 → 缩放到上限 → 编码到 base64 限额内。
    ///
    /// 失败（无法解码、压不到限额）返回 `None`（TS 的 `undefined`）。
    ///
    /// # 参数
    ///
    /// - `bytes`：完整文件字节；函数**总会先解码**，即使最终原样直通；
    /// - `mime_type`：调用方探测出的输入格式，用于判断能否直通、选择编码尝试顺序，以及
    ///   在格式变化时填写 `converted_from`；
    /// - `limits`：输出宽高上限与 base64 字节上限，见 [`ImageLimits`]。
    ///
    /// # 返回
    ///
    /// 成功时 `data` 是 base64 文本，可能经历方向扶正、缩放与格式转换；未开启 `images`
    /// feature 时恒为 `None`。
    pub fn prepare(
        &self,
        bytes: &[u8],
        mime_type: &str,
        limits: ImageLimits,
    ) -> Option<PreparedImage> {
        #[cfg(feature = "images")]
        {
            prepare_with_photon(bytes, mime_type, limits)
        }
        #[cfg(not(feature = "images"))]
        {
            let _ = (bytes, mime_type, limits);
            None
        }
    }
}

/// `createNodePhotonImages()` 的返回值就是 TS 的 `ImageProcessor`：把同步的 [`NodePhotonImages::prepare`]
/// 接到异步 trait 上（原生实现不切线程，见模块文档）。
///
/// 实现体不 `await` 也不 `spawn_blocking`：解码、扶正、缩放与编码都同步发生在轮询该
/// future 的执行器线程上。本仓库的 `tools::read` 直接调用它。
#[async_trait::async_trait]
impl crate::tools::image::ImageProcessor for NodePhotonImages {
    async fn prepare(
        &self,
        bytes: &[u8],
        mime_type: &str,
        limits: ImageLimits,
    ) -> Option<PreparedImage> {
        NodePhotonImages::prepare(self, bytes, mime_type, limits)
    }
}

/// `JPEG_QUALITIES`（`images/index.ts`）：按顺序尝试的 JPEG 质量。
///
/// 顺序决定同尺寸下最终选中的质量，进而决定输出字节；`tests/fixtures/tools_images_exact.json`
/// 对重新编码的结果做了逐字节断言，因此不能随意重排。
pub const JPEG_QUALITIES: [u8; 4] = [80, 70, 55, 40];

/// 把 `prepare_inner` 的意外 panic 兜底成 `None`。
///
/// `decode_image` 已经绕开了 Photon 内部的 `unwrap()`，这里再包一层只是最后的保险：
/// 任何 panic 都按 TS 的 `catch { return undefined; }` 处理。用 `AssertUnwindSafe`
/// 的前提是：闭包只借用输入并向调用者返回新值，本模块没有 panic 后需要恢复的共享状态。
#[cfg(feature = "images")]
fn prepare_with_photon(
    bytes: &[u8],
    mime_type: &str,
    limits: ImageLimits,
) -> Option<PreparedImage> {
    // 解码已经是可失败的（[`decode_image`]）；这里保留 `catch_unwind` 作为最后兜底，
    // 与 TS 把 Photon 的所有抛出统一变成 `undefined` 一致。
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        prepare_inner(bytes, mime_type, limits)
    }))
    .ok()
    .flatten()
}

/// 解码图片：与 `PhotonImage::new_from_byteslice` 完全相同的 `image` 0.24 调用
/// （`load_from_memory` + `to_rgba8`），但失败时返回 `None`。
///
/// photon-rs 的实现是 `image::load_from_memory(slice).unwrap()`：无法解码时会 panic 并把
/// panic 信息打到 stderr，而 TS 的 `catch { return undefined; }` 不产生任何输出。直接用同一份
/// `image` 0.24 解码既能得到相同的像素，又能像 TS 一样安静地失败。
///
/// `image024` 是 `Cargo.toml` 里为 photon-rs 内部解码固定到 0.24 的依赖；换成别的版本就
/// 无法保证解码像素与 wasm 构建一致，重新编码的字节也可能随之变化。
#[cfg(feature = "images")]
fn decode_image(bytes: &[u8]) -> Option<PhotonImage> {
    let decoded = image024::load_from_memory(bytes).ok()?;
    let rgba = decoded.to_rgba8();
    let (width, height) = (rgba.width(), rgba.height());
    Some(PhotonImage::new(rgba.into_raw(), width, height))
}

/// `prepare` 的实际流程（仅 `images` feature 下编译）；阶段划分见模块文档。
///
/// 关键不变量：
///
/// - 每一轮编码都从**同一张**已扶正的全尺寸图重新缩放，不会在上一轮结果上累积重采样；
/// - `target` 每轮按 0.75 缩小且下限为 1，循环必然终止；
/// - `resized` 只在 `target` 与扶正后的尺寸不同时存在。
#[cfg(feature = "images")]
fn prepare_inner(bytes: &[u8], mime_type: &str, limits: ImageLimits) -> Option<PreparedImage> {
    // 阶段 1-2：读 EXIF 方向并解码。解码失败立即返回 None；直通路径同样要求图片可解码。
    let orientation = exif_orientation(bytes);
    let inline = INLINE_IMAGE_TYPES.contains(mime_type);
    let mut image = decode_image(bytes)?;
    // 阶段 3：格式可内联、方向为 1、宽高与 base64 长度都满足限制时，原样返回输入字节。
    if inline
        && orientation == 1
        && image.get_width() <= limits.max_width
        && image.get_height() <= limits.max_height
        && base64_length(bytes.len()) <= limits.max_bytes
    {
        return Some(PreparedImage {
            data: to_base64(bytes),
            mime_type: mime_type.to_owned(),
            resized: None,
            converted_from: None,
        });
    }
    // 阶段 4：扶正。5..=8 会交换宽高，交给 transposed；2 只水平翻、4 只垂直翻、
    // 3 两个方向都翻。
    if orientation >= 5 {
        let upright = transposed(&image, orientation);
        image = PhotonImage::new(upright.pixels, upright.width, upright.height);
    } else if orientation != 1 {
        if orientation != 4 {
            fliph(&mut image);
        }
        if orientation != 2 {
            flipv(&mut image);
        }
    }
    // 阶段 5：等比缩放到宽高上限。`scale` 不超过 1.0，所以只会缩小、不会放大。
    let width = image.get_width();
    let height = image.get_height();
    let scale = 1.0_f64
        .min(f64::from(limits.max_width) / f64::from(width))
        .min(f64::from(limits.max_height) / f64::from(height));
    let mut target = (
        ((f64::from(width) * scale).round() as u32).max(1),
        ((f64::from(height) * scale).round() as u32).max(1),
    );
    // 阶段 6：按当前目标尺寸尝试编码；放不下就把目标缩到 75%（向下取整）再试。
    loop {
        if let Some(encoded) =
            encode_within(&image, target, limits.max_bytes, mime_type == "image/jpeg")
        {
            // `resized` 相对的是扶正后的尺寸；仅 EXIF 旋转导致的宽高交换不算缩放。
            let resized = if target.0 == width && target.1 == height {
                None
            } else {
                Some(ResizedDimensions {
                    from: (width, height),
                    to: target,
                })
            };
            // 输出格式与输入不同才记录 `converted_from`。
            let converted_from = if encoded.mime_type == mime_type {
                None
            } else {
                Some(mime_type.to_owned())
            };
            return Some(PreparedImage {
                data: encoded.data,
                mime_type: encoded.mime_type,
                resized,
                converted_from,
            });
        }
        // 连 1x1 都放不下：字节上限小于任何可编码结果，放弃。
        if target.0 == 1 && target.1 == 1 {
            return None;
        }
        target = (
            ((f64::from(target.0) * 0.75).floor() as u32).max(1),
            ((f64::from(target.1) * 0.75).floor() as u32).max(1),
        );
    }
}

/// 一次成功编码的结果；`data` 是 base64，`byte_length` 保留原始字节数用于限额比较。
#[cfg(feature = "images")]
struct Encoded {
    data: String,
    mime_type: String,
    /// 编码后的原始字节数：`base64Length` 的输入是 `bytes.byteLength`，不是 base64 字符串长度。
    byte_length: usize,
}

/// 图片在 `size` 下第一个 base64 装得进 `maxBytes` 的编码：PNG 后 JPEG 降质量，`jpegFirst` 时反过来。
///
/// 尝试顺序是输出契约的一部分：
///
/// - `jpeg_first`（输入 MIME 是 `image/jpeg`）先按 [`JPEG_QUALITIES`] 从高到低试 JPEG，
///   都不行再试 PNG；
/// - 其余输入先试 PNG，再按 [`JPEG_QUALITIES`] 试 JPEG；
/// - 目标尺寸与当前图片一致时不重采样，直接编码原像素；
/// - 所有候选都重新从传入的图片编码，比较的是 `base64_length(原始字节数)`，即输出 base64
///   字符串的字节长度。
///
/// 全部候选都超限时返回 `None`，由调用方缩小尺寸后重试。
#[cfg(feature = "images")]
fn encode_within(
    image: &PhotonImage,
    size: (u32, u32),
    max_bytes: usize,
    jpeg_first: bool,
) -> Option<Encoded> {
    let same = size.0 == image.get_width() && size.1 == image.get_height();
    let scaled = if same {
        None
    } else {
        Some(resize(image, size.0, size.1, SamplingFilter::Lanczos3))
    };
    let target = scaled.as_ref().unwrap_or(image);
    let encode = |bytes: Vec<u8>, mime_type: &str| Encoded {
        data: to_base64(&bytes),
        mime_type: mime_type.to_owned(),
        byte_length: bytes.len(),
    };
    let png = || encode(target.get_bytes(), "image/png");
    let jpeg = |quality: u8| encode(target.get_bytes_jpeg(quality), "image/jpeg");
    let fits = |encoded: &Encoded| base64_length(encoded.byte_length) <= max_bytes;
    if jpeg_first {
        for quality in JPEG_QUALITIES {
            let encoded = jpeg(quality);
            if fits(&encoded) {
                return Some(encoded);
            }
        }
        let encoded = png();
        if fits(&encoded) {
            return Some(encoded);
        }
    } else {
        let encoded = png();
        if fits(&encoded) {
            return Some(encoded);
        }
        for quality in JPEG_QUALITIES {
            let encoded = jpeg(quality);
            if fits(&encoded) {
                return Some(encoded);
            }
        }
    }
    None
}

/// `transposed` 的结果：宽高交换后的 RGBA 像素与尺寸。
#[cfg(feature = "images")]
struct Upright {
    pixels: Vec<u8>,
    width: u32,
    height: u32,
}

/// EXIF 方向 5 到 8（会交换宽高的方向）扶正后的 RGBA 像素。
///
/// 输出尺寸是 `(width: 输入高度, height: 输入宽度)`，因此目标索引的行宽是 `height`
/// （等于输出宽度）。设输入坐标为 `(x, y)`、原点在左上，四个方向的落点：
///
/// | orientation | 输出坐标 | 含义 |
/// |---|---|---|
/// | 5 | `(y, x)` | 转置 |
/// | 6 | `(height-1-y, x)` | 顺时针 90° |
/// | 7 | `(height-1-y, width-1-x)` | 转置 + 180° |
/// | 8 | `(y, width-1-x)` | 逆时针 90° |
///
/// 每个输入像素恰好写入一个输出位置，`pixels` 与输入等长（每像素 4 字节）。方向 2/3/4
/// 不经过本函数。
#[cfg(feature = "images")]
fn transposed(image: &PhotonImage, orientation: u8) -> Upright {
    let width = image.get_width();
    let height = image.get_height();
    let source = image.get_raw_pixels();
    let mut pixels = vec![0u8; source.len()];
    for y in 0..height {
        for x in 0..width {
            // (x, y) 在扶正后图片（宽为 `height`）里的落点。调用点保证 orientation 只能是
            // 5..=8，因此 `_` 分支就是 8（逆时针 90°）。
            let (out_x, out_y) = match orientation {
                5 => (y, x),
                6 => (height - 1 - y, x),
                7 => (height - 1 - y, width - 1 - x),
                _ => (y, width - 1 - x),
            };
            let source_index = (y as usize * width as usize + x as usize) * 4;
            // 输出行宽是 height（= 输入高度），所以目标索引的步长用 height。
            let target_index = (out_y as usize * height as usize + out_x as usize) * 4;
            pixels[target_index..target_index + 4]
                .copy_from_slice(&source[source_index..source_index + 4]);
        }
    }
    Upright {
        pixels,
        width: height,
        height: width,
    }
}
