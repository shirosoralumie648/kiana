//! `tools/image.ts` / `tools/image-processor.ts`：图片格式探测、尺寸读取与内联图片上限。
//!
//! TS 里 `image.ts` 只有同步的字节探测；`detectSupportedImageMimeTypeOf` 接受一个可位置读取的
//! 字节源（`read` 是 Promise，因此是异步的）。Rust 侧用 [`ByteSource`] 表达同一个契约，
//! `tools/read.rs` 用一个包住 `BinaryReader` + 上下文的适配器实现它。
//!
//! `image-processor.ts` 的类型与其常量在本文件（TS 没有同名 Rust 文件）：[`ImageLimits`]、
//! [`PreparedImage`] 的形状由 `images/node.rs` 冻结，这里 re-export 以免出现第二份定义。
//!
//! # 职责与边界
//! `read` 在把文件当文本读之前要先判断它是不是图片，而且不能为了判断读完整文件：本模块
//! 只读固定大小的头（32 字节）以及 PNG 的 chunk 头。格式探测只回答"是不是受支持的图片"
//! 和"头部声明的宽高"，不解码像素；真正的解码、EXIF 扶正、缩放与重编码由
//! [`ImageProcessor`] 的实现负责（`images/node.rs`）。
//!
//! # 数据流
//! 1. `read` 打开二进制 reader，用 `ReaderSource` 适配成本文件的 [`ByteSource`]。
//! 2. `detect_supported_image_mime_type_of` 返回 `Some(mime)` 时走图片分支，`None` 时按
//!    文本读取。
//! 3. 有 `ImageProcessor` 时把整份字节交给 `prepare`；没有处理器时要求 MIME 在
//!    [`INLINE_IMAGE_TYPES`] 内，并只用 [`image_dimensions`] 检查头部声明的宽高。
//!
//! # 约束与失败模式
//! - 探测函数对无法识别的格式返回 `None`，不会 panic；越界读取由内部 `byte_at` 容错成 0。
//!   异步版本唯一的错误来源是底层 `ByteSource::read` 的 `FileError`。
//! - 探测结果只是文件头声明，不保证整份文件可解码；可解码性由 `ImageProcessor` 判断，
//!   失败返回 `None`，`read` 再把它变成 `unsupported_image` 错误。
//! - `image/bmp` 能被探测出来，但不在 [`INLINE_IMAGE_TYPES`] 中；没有处理器时 `read` 会
//!   把它当作"需要转换"的图片（见 `crate::tools::read` 的 `read_image`）。
//! - `image_dimensions` 只支持 PNG/GIF/WebP/JPEG；JPEG 会顺序跳过各 segment，长度字段异常
//!   时提前返回 `None`，所有读取都有长度检查，不会越界。
//!
//! 格式与尺寸的参考用例在 `tests/tools_images.rs`，数据来自
//! `tests/fixtures/tools_images_cases.json`（由 TS 实现生成）。

use std::collections::HashSet;
use std::sync::LazyLock;

use async_trait::async_trait;

use crate::env::FileError;

pub use crate::images::node::{ImageLimits, PreparedImage, ResizedDimensions};

/// `DEFAULT_IMAGE_LIMITS`：`{ maxWidth: 2000, maxHeight: 2000, maxBytes: 4.5 * 1024 * 1024 }`。
///
/// 宽高以像素计；`max_bytes` 是 **base64 文本**的字节上限（见 `ImageLimits::max_bytes`），
/// 不是原始文件字节数。模型目录没有给出更具体的 resize 限制时使用这组值。
pub const DEFAULT_IMAGE_LIMITS: ImageLimits = ImageLimits {
    max_width: 2000,
    max_height: 2000,
    max_bytes: 4_718_592,
};

/// 每进程一份的 `INLINE_IMAGE_TYPES`：provider 都能内联接收的格式。
///
/// `read` 在没有 [`ImageProcessor`] 时用 `contains` 判断能否原样内联。
pub static INLINE_IMAGE_TYPES: LazyLock<HashSet<&'static str>> =
    LazyLock::new(|| HashSet::from(["image/png", "image/jpeg", "image/gif", "image/webp"]));

/// `ImageProcessor`：把图片解码、扶正、缩放、重新编码到模型能接收的形状。
///
/// 输入是整份原始字节与探测出的 MIME；输出是可直接作为图片附件交给模型的
/// [`PreparedImage`]（base64 文本、输出 MIME、可选的缩放/格式转换说明）。返回 `None`
/// 表示"无法解码或压不进 [`ImageLimits`]"，调用方（`crate::tools::read` 的 `read_image`）
/// 会把它转成 `unsupported_image` 错误，不会重试或降级到原始字节。
///
/// `Send + Sync` 是必需的：处理器被放在 `Arc` 里随 [`crate::tools::read::ReadToolOptions`]
/// 被多个调用共享。实现里的错误都折叠成 `None`，trait 本身不暴露错误类型。
///
/// TS 的 `prepare` 返回 Promise（处理器可以在事件循环之外干活），所以这里是 async trait。
#[async_trait]
pub trait ImageProcessor: Send + Sync {
    /// 能放进 `limits` 的图片；无法解码或压不进限额时返回 `None`（TS 的 `undefined`）。
    ///
    /// `bytes` 是整份文件；`mime_type` 是 [`detect_supported_image_mime_type_of`] 的结果，
    /// 处理器可以据此选择解码器，也可以输出不同的 MIME（见 `PreparedImage::mime_type`）。
    async fn prepare(
        &self,
        bytes: &[u8],
        mime_type: &str,
        limits: ImageLimits,
    ) -> Option<PreparedImage>;
}

/// 标准 base64（`btoa` 的等价物）。
///
/// 使用标准字母表（`+`/`/`），每 3 字节一组编码，余 1 字节补 `==`、余 2 字节补 `=`；空输入
/// 得到空串。输出总是 4 的倍数，长度等于 [`base64_length`]。
///
/// TS 分块只是为了不把几 MB 的参数一次性传给 `String.fromCharCode.apply`；编码结果相同。
pub fn to_base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(bytes.len().div_ceil(3) * 4);
    let (chunks, remainder) = bytes.as_chunks::<3>();
    for chunk in chunks {
        let value = (u32::from(chunk[0]) << 16) | (u32::from(chunk[1]) << 8) | u32::from(chunk[2]);
        output.push(ALPHABET[(value >> 18) as usize & 0x3f] as char);
        output.push(ALPHABET[(value >> 12) as usize & 0x3f] as char);
        output.push(ALPHABET[(value >> 6) as usize & 0x3f] as char);
        output.push(ALPHABET[value as usize & 0x3f] as char);
    }
    match remainder.len() {
        1 => {
            let value = u32::from(remainder[0]) << 16;
            output.push(ALPHABET[(value >> 18) as usize & 0x3f] as char);
            output.push(ALPHABET[(value >> 12) as usize & 0x3f] as char);
            output.push('=');
            output.push('=');
        }
        2 => {
            let value = (u32::from(remainder[0]) << 16) | (u32::from(remainder[1]) << 8);
            output.push(ALPHABET[(value >> 18) as usize & 0x3f] as char);
            output.push(ALPHABET[(value >> 12) as usize & 0x3f] as char);
            output.push(ALPHABET[(value >> 6) as usize & 0x3f] as char);
            output.push('=');
        }
        _ => {}
    }
    output
}

/// `base64Length(byteLength)`：`Math.ceil(byteLength / 3) * 4`。
///
/// 即这 `byte_length` 个原始字节经 [`to_base64`] 后的字符数（含末尾的 `=`）。没有图片
/// 处理器时，`read` 用它在不读取整份数据的情况下先判断图片是否会超过
/// `ImageLimits::max_bytes`（该字段计的正是 base64 文本长度）。
pub fn base64_length(byte_length: usize) -> usize {
    byte_length.div_ceil(3) * 4
}

// ---------------------------------------------------------------------------
// tools/image.ts
// ---------------------------------------------------------------------------

/// `PNG_SIGNATURE`。
const PNG_SIGNATURE: [u8; 8] = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
/// `HEADER_BYTES`：除 APNG chunk 遍历外所有检查需要的字节数（BMP 读到 offset 29）。
const HEADER_BYTES: usize = 32;
/// `BLOCK_BYTES`：APNG chunk 遍历的分块大小。
const BLOCK_BYTES: usize = 64 * 1024;

/// `ByteSource`：对一个大小为 `size` 的文件的定位读取。
///
/// 存在的意义是让探测代码不必持有完整文件：`read` 工具把 `BinaryReader` + 上下文包成一个
/// 适配器（`crate::tools::read` 的 `ReaderSource`），探测逻辑则只依赖这两个方法。
#[async_trait]
pub trait ByteSource: Send + Sync {
    /// 文件大小（字节）。
    fn size(&self) -> u64;
    /// `offset` 起最多 `length` 字节；仅在文件末尾会少。
    ///
    /// 实现必须在文件末尾返回实际可读的字节数（可以少于 `length`），而不是补零或报错；
    /// 探测代码依赖 `size` 判断 chunk 头是否完整。
    async fn read(&self, offset: u64, length: usize) -> Result<Vec<u8>, FileError>;
}

/// `detectSupportedImageMimeTypeOf`：整份文件的格式探测，只读头部与（PNG 的）chunk 头，
/// 直到第一个 `acTL` 或 `IDAT`。
///
/// 先读 `HEADER_BYTES` 字节：不是 PNG 签名时把这段头交给同步版
/// [`detect_supported_image_mime_type`]（32 字节足够覆盖 BMP 需要读到 offset 29 的检查）；
/// 是 PNG 签名时还要扫描 chunk，确认它是静态 PNG（`acTL` 在 `IDAT` 之前则不支持 APNG）。
///
/// # 错误
/// 只把 `ByteSource::read` 的 `FileError` 原样返回。格式不支持时得到 `Ok(None)`；这是头部
/// 探测，文件被截断不会报错，也不保证探测出的图片之后一定能解码。
pub async fn detect_supported_image_mime_type_of(
    source: &dyn ByteSource,
) -> Result<Option<String>, FileError> {
    let header = source.read(0, HEADER_BYTES).await?;
    if !starts_with(&header, &PNG_SIGNATURE) {
        return Ok(detect_supported_image_mime_type(&header));
    }
    if is_png(&header) && !is_animated_png_of(source).await? {
        Ok(Some("image/png".to_owned()))
    } else {
        Ok(None)
    }
}

/// `isAnimatedPng` 的分块读取版本。
///
/// 沿 chunk 链前进：块缓存命中时直接从 `block` 取 8 字节 chunk 头，未命中才从当前
/// `offset` 重新读一块（最多 `BLOCK_BYTES`）。它不需要把整份文件读进内存；通常只看
/// 头部附近的少量块。
// 循环条件保证 chunk 头完整；chunk 长度算出的下一偏移回退或越过文件末尾时按"不是动画"
// 处理（返回 false）。这与同步版对截断输入的处理一致，不返回错误。
async fn is_animated_png_of(source: &dyn ByteSource) -> Result<bool, FileError> {
    let mut block: Vec<u8> = Vec::new();
    let mut block_start: u64 = 0;
    let size = source.size();
    let mut offset = PNG_SIGNATURE.len() as u64;
    while offset + 8 <= size {
        if offset < block_start || offset + 8 > block_start + block.len() as u64 {
            block_start = offset;
            block = source.read(offset, BLOCK_BYTES).await?;
        }
        let start = (offset - block_start) as usize;
        let chunk_header = block.get(start..start + 8).unwrap_or(&[]);
        let chunk_length = u64::from(read_uint32_be(chunk_header, 0));
        if starts_with_ascii(chunk_header, 4, "acTL") {
            return Ok(true);
        }
        if starts_with_ascii(chunk_header, 4, "IDAT") {
            return Ok(false);
        }
        let next_offset = offset + 8 + chunk_length + 4;
        if next_offset <= offset || next_offset > size {
            return Ok(false);
        }
        offset = next_offset;
    }
    Ok(false)
}

/// `imageDimensions` 的结果：`{ width, height }`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ImageDimensions {
    /// 宽（像素）。
    pub width: u32,
    /// 高（像素）。
    pub height: u32,
}

/// `detectSupportedImageMimeType(buffer)`：按字节头判断可内联的图片格式。
///
/// 判定顺序：
/// 1. `ff d8 ff`：JPEG；但第 4 字节为 `f7` 时是 JPG 扩展流，返回 `None`。
/// 2. PNG 签名：必须是长度 13 的 IHDR，且 chunk 链在 `IDAT` 之前没有 `acTL`（APNG）。
/// 3. `GIF87a` 或 `GIF89a`：GIF。
/// 4. `RIFF` 且 offset 8 为 `WEBP`：WebP。
/// 5. `BM` 且通过 `is_bmp` 的 DIB 头检查：BMP。
///
/// 只读传入切片，不要求是完整文件；越界位置由 `byte_at` 当成 0。各分支的长度检查只保证
/// 读取不越界，不保证结构完整——例如只有 3 字节的 `ff d8 ff` 仍会被判为 JPEG。
pub fn detect_supported_image_mime_type(buffer: &[u8]) -> Option<String> {
    if starts_with(buffer, &[0xff, 0xd8, 0xff]) {
        return if byte_at(buffer, 3) == 0xf7 {
            None
        } else {
            Some("image/jpeg".to_owned())
        };
    }
    if starts_with(buffer, &PNG_SIGNATURE) {
        return if is_png(buffer) && !is_animated_png(buffer) {
            Some("image/png".to_owned())
        } else {
            None
        };
    }
    if starts_with_ascii(buffer, 0, "GIF87a") || starts_with_ascii(buffer, 0, "GIF89a") {
        return Some("image/gif".to_owned());
    }
    if starts_with_ascii(buffer, 0, "RIFF") && starts_with_ascii(buffer, 8, "WEBP") {
        return Some("image/webp".to_owned());
    }
    if starts_with_ascii(buffer, 0, "BM") && is_bmp(buffer) {
        return Some("image/bmp".to_owned());
    }
    None
}

// PNG 合法性的最小检查：IHDR 必须是第一个 chunk、长度为 13、类型为 "IHDR"。
// 不校验 CRC 或图像数据——这里只用来区分"看起来像 PNG"和随便带 PNG 签名的文件。
fn is_png(buffer: &[u8]) -> bool {
    buffer.len() >= 16
        && read_uint32_be(buffer, PNG_SIGNATURE.len()) == 13
        && starts_with_ascii(buffer, 12, "IHDR")
}

// 同步版 APNG 扫描：在调用方给的 buffer 上沿 chunk 链走，在 IDAT 之前遇到 acTL 就是
// APNG。chunk 长度与下一偏移用 u64 计算，避免回绕；越界/回退即停止并判为不是动画。
fn is_animated_png(buffer: &[u8]) -> bool {
    let mut offset = PNG_SIGNATURE.len();
    while offset + 8 <= buffer.len() {
        let chunk_length = u64::from(read_uint32_be(buffer, offset));
        let chunk_type_offset = offset + 4;
        if starts_with_ascii(buffer, chunk_type_offset, "acTL") {
            return true;
        }
        if starts_with_ascii(buffer, chunk_type_offset, "IDAT") {
            return false;
        }
        let next_offset = offset as u64 + 8 + chunk_length + 4;
        if next_offset <= offset as u64 || next_offset > buffer.len() as u64 {
            return false;
        }
        offset = next_offset as usize;
    }
    false
}

/// `imageDimensions(bytes, mimeType)`：不解码图片，只读头部声明的宽高。
///
/// 支持的 MIME 与读取位置：
/// - `image/png`：IHDR 的 32 位大端宽/高（offset 16/20），要求至少 24 字节。
/// - `image/gif`：逻辑屏幕宽/高（offset 6/8，小端 16 位），要求至少 10 字节。
/// - `image/webp`：按 chunk 类型分三种——`VP8 ` 读 14 位宽高；`VP8L` 读 14 位"宽高减一"
///   后各加一；`VP8X` 读 24 位"宽高减一"后各加一。
/// - `image/jpeg`：从 offset 2 扫描 marker，按 segment 长度跳过非 SOF 段，在 SOF0–SOF15
///   （排除共用区间的 DHT `c4`、JPG `c8`、DAC `cc`）里取高/宽。
///
/// 其他 MIME（包括 `image/bmp`）返回 `None`。所有结果都是**头部声明的原始尺寸**，不考虑
/// EXIF 方向；数据不足或结构不符返回 `None`。
pub fn image_dimensions(bytes: &[u8], mime_type: &str) -> Option<ImageDimensions> {
    match mime_type {
        "image/png" => {
            if bytes.len() < 24 {
                return None;
            }
            Some(ImageDimensions {
                width: read_uint32_be(bytes, 16),
                height: read_uint32_be(bytes, 20),
            })
        }
        "image/gif" => {
            if bytes.len() < 10 {
                return None;
            }
            Some(ImageDimensions {
                width: u32::from(read_uint16_le(bytes, 6)),
                height: u32::from(read_uint16_le(bytes, 8)),
            })
        }
        "image/webp" => {
            if bytes.len() < 30 {
                return None;
            }
            if starts_with_ascii(bytes, 12, "VP8 ") {
                return Some(ImageDimensions {
                    width: u32::from(read_uint16_le(bytes, 26)) & 0x3fff,
                    height: u32::from(read_uint16_le(bytes, 28)) & 0x3fff,
                });
            }
            if starts_with_ascii(bytes, 12, "VP8L") {
                let bits = read_uint32_le(bytes, 21);
                return Some(ImageDimensions {
                    width: (bits & 0x3fff) + 1,
                    height: ((bits >> 14) & 0x3fff) + 1,
                });
            }
            if starts_with_ascii(bytes, 12, "VP8X") {
                let uint24 = |offset: usize| -> u32 {
                    u32::from(read_uint16_le(bytes, offset))
                        + (u32::from(byte_at(bytes, offset + 2)) << 16)
                };
                return Some(ImageDimensions {
                    width: uint24(24) + 1,
                    height: uint24(27) + 1,
                });
            }
            None
        }
        "image/jpeg" => {
            let mut offset = 2usize;
            while offset + 9 <= bytes.len() {
                if byte_at(bytes, offset) != 0xff {
                    return None;
                }
                let marker = byte_at(bytes, offset + 1);
                if marker == 0xff {
                    offset += 1;
                    continue;
                }
                // SOF0 到 SOF15，除去与其共享区间的 DHT (C4)、JPG (C8)、DAC (CC)。
                if (0xc0..=0xcf).contains(&marker)
                    && marker != 0xc4
                    && marker != 0xc8
                    && marker != 0xcc
                {
                    return Some(ImageDimensions {
                        width: u32::from(read_uint16_be(bytes, offset + 7)),
                        height: u32::from(read_uint16_be(bytes, offset + 5)),
                    });
                }
                offset += 2 + usize::from(read_uint16_be(bytes, offset + 2));
            }
            None
        }
        _ => None,
    }
}

// BMP 可用性检查：文件大小、像素数据偏移与 DIB 头大小必须自洽。
// DIB 头为 12（OS/2）时平面数/色深在 offset 22/24；40..=124 时在 26/28（因此至少要 30
// 字节）。平面必须是 1，色深必须是 1/4/8/16/24/32 之一；其他 DIB 头大小一律拒绝。
fn is_bmp(buffer: &[u8]) -> bool {
    if buffer.len() < 26 {
        return false;
    }
    let declared_file_size = read_uint32_le(buffer, 2);
    let pixel_data_offset = read_uint32_le(buffer, 10);
    let dib_header_size = read_uint32_le(buffer, 14);
    if declared_file_size != 0 && declared_file_size < 26 {
        return false;
    }
    // TS 的 `14 + dibHeaderSize` 是双精度浮点，不会溢出；用 u64 保持同样的比较结果
    // （`dibHeaderSize == 0xffff_ffff` 时 u32 相加会在 debug 构建 panic）。
    if u64::from(pixel_data_offset) < 14 + u64::from(dib_header_size) {
        return false;
    }
    if declared_file_size != 0 && pixel_data_offset >= declared_file_size {
        return false;
    }

    let (color_planes, bits_per_pixel) = if dib_header_size == 12 {
        (read_uint16_le(buffer, 22), read_uint16_le(buffer, 24))
    } else if (40..=124).contains(&dib_header_size) {
        if buffer.len() < 30 {
            return false;
        }
        (read_uint16_le(buffer, 26), read_uint16_le(buffer, 28))
    } else {
        return false;
    };
    color_planes == 1 && matches!(bits_per_pixel, 1 | 4 | 8 | 16 | 24 | 32)
}

// 越界读作 0：所有解析器共享这个无 panic 的读取函数，不另做下标检查；"够不够长"由各分支
// 自己判断（有的先比长度，有的直接与签名常量比较）。
fn byte_at(buffer: &[u8], offset: usize) -> u8 {
    buffer.get(offset).copied().unwrap_or(0)
}

fn read_uint16_le(buffer: &[u8], offset: usize) -> u16 {
    u16::from(byte_at(buffer, offset)) | (u16::from(byte_at(buffer, offset + 1)) << 8)
}

fn read_uint16_be(buffer: &[u8], offset: usize) -> u16 {
    (u16::from(byte_at(buffer, offset)) << 8) | u16::from(byte_at(buffer, offset + 1))
}

fn read_uint32_be(buffer: &[u8], offset: usize) -> u32 {
    (u32::from(byte_at(buffer, offset)) << 24)
        | (u32::from(byte_at(buffer, offset + 1)) << 16)
        | (u32::from(byte_at(buffer, offset + 2)) << 8)
        | u32::from(byte_at(buffer, offset + 3))
}

fn read_uint32_le(buffer: &[u8], offset: usize) -> u32 {
    u32::from(byte_at(buffer, offset))
        | (u32::from(byte_at(buffer, offset + 1)) << 8)
        | (u32::from(byte_at(buffer, offset + 2)) << 16)
        | (u32::from(byte_at(buffer, offset + 3)) << 24)
}

fn starts_with(buffer: &[u8], bytes: &[u8]) -> bool {
    buffer.len() >= bytes.len() && buffer[..bytes.len()] == *bytes
}

fn starts_with_ascii(buffer: &[u8], offset: usize, text: &str) -> bool {
    let bytes = text.as_bytes();
    buffer.len() >= offset + bytes.len() && &buffer[offset..offset + bytes.len()] == bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_reference_for_every_remainder() {
        for length in [0usize, 1, 2, 3, 4, 5, 8191, 8192, 8193] {
            let bytes: Vec<u8> = (0..length).map(|index| (index * 37 % 256) as u8).collect();
            let mut expected = String::new();
            // 用 base64 crate 的等价实现（本 crate 无该依赖）：手写参考，按 6 位分组。
            let mut buffer = 0u32;
            let mut bits = 0u32;
            for byte in &bytes {
                buffer = (buffer << 8) | u32::from(*byte);
                bits += 8;
                while bits >= 6 {
                    bits -= 6;
                    expected.push(
                        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"
                            [(buffer >> bits) as usize & 0x3f] as char,
                    );
                }
                buffer &= (1 << bits) - 1;
            }
            if bits > 0 {
                expected.push(
                    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"
                        [((buffer << (6 - bits)) & 0x3f) as usize] as char,
                );
            }
            while !expected.len().is_multiple_of(4) {
                expected.push('=');
            }
            assert_eq!(to_base64(&bytes), expected, "length {length}");
        }
    }

    #[test]
    fn base64_length_matches_ceil_rule() {
        assert_eq!(base64_length(0), 0);
        assert_eq!(base64_length(1), 4);
        assert_eq!(base64_length(3), 4);
        assert_eq!(base64_length(4), 8);
    }
}
