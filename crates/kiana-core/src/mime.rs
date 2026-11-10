//! utils/mime.ts。
//!
//! TS 源：`packages/coding-agent/src/utils/mime.ts`。
//!
//! 按字节魔数判定图片来源的 MIME 类型（**不**用扩展名猜测、也不用 `mime` crate 猜）：
//!
//! - JPEG：`FF D8 FF`，但 `FF D8 FF F7` 视为不支持（`buffer[3] === 0xF7` → `null`）；
//! - PNG：签名 + 合法 IHDR（长度 13、类型 `IHDR`、长度 ≥ 16），且不是 APNG（`IDAT` 之前
//!   出现 `acTL`）；
//! - GIF：`GIF87a` / `GIF89a`；
//! - WebP：`RIFF` + offset 8 处 `WEBP`；
//! - BMP：`BM` + 严格的头部结构校验（见 [`detect_supported_image_mime_type`]）。
//!
//! 文件版本只读前 **4100** 字节（`IMAGE_TYPE_SNIFF_BYTES`）。
//!
//! ## 为什么需要这个模块
//!
//! 调用方在读入文件内容之前要先判断“这是不是一张受支持的图片”：`read` 工具据此决定走图片
//! 分支还是文本/二进制分支，`@file` 处理据此决定是否构造图片附件。TS 的判定只依赖内容前缀
//! （不看扩展名），所以改名后的图片仍能被识别；本移植必须保持同一套字节规则，否则同一个
//! 文件在 TS 与 Rust 两侧会走不同分支。
//!
//! ## 判定流程
//!
//! 一次判定按固定顺序尝试五种签名，命中即返回：
//!
//! 1. JPEG：前 3 字节 `FF D8 FF`，且第 4 字节不是 `F7`（JPEG-LS）；
//! 2. PNG：8 字节签名 + 合法 IHDR + 不是 APNG；
//! 3. GIF：`GIF87a` / `GIF89a`；
//! 4. WebP：`RIFF` + offset 8 处的 `WEBP`；
//! 5. BMP：`BM` + 结构校验。
//!
//! 数字字段的越界字节按 0 读取（`copied`，对应 JS 的 `buffer[i] ?? 0`），前缀比较则先要求
//! 长度足够；因此任意短的输入既不会 panic，也不会误报比它更长的签名。
//!
//! ## 重要约束
//!
//! - 只读取前 4100 字节；`is_animated_png` 在第一个 `IDAT` 处停止扫描，因此 `IDAT` 之后的
//!   `acTL` 不会导致拒绝（测试 `accepts_png_when_idat_precedes_actl` 锁定该行为）。
//! - 不校验 CRC、不做解码：chunk 长度越过 buffer 末尾或无法让 offset 前进时扫描提前结束；
//!   其它伪造长度只会让后续解析错位（可能把数据当成 chunk 头），实现不会按声明长度分配内存。
//! - 返回值是拥有所有权的 `String` 字面量；测试里通常用 `as_deref()` 与 `Some("image/png")`
//!   这类字符串字面量比较（`tests/mime.rs`），生产调用方则把 `String` 直接交给
//!   `process_image`。
//!
//! ## 调用方
//!
//! - `kiana-tools/src/read.rs` 的 `LocalReadOperations::detect_image_mime_type`；
//! - `kiana-coding-agent/src/cli/file_processor.rs`（`@file` 图片附件）；
//! - `kiana-coding-agent/src/sdk_exports.rs` 以 TS 原名 `detectSupportedImageMimeTypeFromFile`
//!   再导出。

use std::io::Read as _;
use std::path::Path;

/// `IMAGE_TYPE_SNIFF_BYTES`。
const IMAGE_TYPE_SNIFF_BYTES: usize = 4100;

/// `PNG_SIGNATURE`。
const PNG_SIGNATURE: [u8; 8] = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];

/// `detectSupportedImageMimeType(buffer)`：按魔数返回受支持的图片 MIME 类型。
///
/// 返回 `Some("image/jpeg" | "image/png" | "image/gif" | "image/webp" | "image/bmp")`，
/// 其它（含被拒绝的 JPEG-LS 标记、动画 PNG）返回 `None`。
///
/// 判定顺序固定为 JPEG → PNG → GIF → WebP → BMP，命中即返回，后续规则不再执行。例如前 3
/// 字节是 `FF D8 FF` 的输入不会再被当作 PNG/GIF 检查。
///
/// 输入允许任意短：前缀比较先检查长度，数字字段越界按 0 读取。因此 `FF D8 FF`（只有 3
/// 字节）仍返回 `Some("image/jpeg")`，而 `FF D8` 返回 `None`。
///
/// `None` 只表示“不是受支持的五种图片”，不代表文件损坏或不可读；I/O 错误由
/// [`detect_supported_image_mime_type_from_file`] 的 `Result` 表达。
pub fn detect_supported_image_mime_type(buffer: &[u8]) -> Option<String> {
    if starts_with(buffer, &[0xff, 0xd8, 0xff]) {
        return if buffer.get(3) == Some(&0xf7) {
            None
        } else {
            Some("image/jpeg".to_string())
        };
    }
    if starts_with(buffer, &PNG_SIGNATURE) {
        return if is_png(buffer) && !is_animated_png(buffer) {
            Some("image/png".to_string())
        } else {
            None
        };
    }
    if starts_with_ascii(buffer, 0, "GIF87a") || starts_with_ascii(buffer, 0, "GIF89a") {
        return Some("image/gif".to_string());
    }
    if starts_with_ascii(buffer, 0, "RIFF") && starts_with_ascii(buffer, 8, "WEBP") {
        return Some("image/webp".to_string());
    }
    if starts_with_ascii(buffer, 0, "BM") && is_bmp(buffer) {
        return Some("image/bmp".to_string());
    }
    None
}

/// `detectSupportedImageMimeTypeFromFile(filePath)`：读文件前 4100 字节后做魔数判定。
///
/// I/O 错误（文件不存在、无权限、目录等）原样返回；读取成功但没有匹配返回 `Ok(None)`。
/// 读取通过 `take(4100).read_to_end` 流式完成，不会把大文件整个读入内存；返回的 MIME
/// 字符串与文件句柄无关（拥有所有权）。
///
/// 错误不携带路径：需要 Node 风格 `ENOENT: ..., open '<path>'` 文案的调用方必须自行补上
/// 路径（`kiana-tools/src/read.rs` 与 `kiana-coding-agent/src/cli/file_processor.rs` 都这样
/// 处理）。
pub fn detect_supported_image_mime_type_from_file(
    file_path: &Path,
) -> std::io::Result<Option<String>> {
    let file = std::fs::File::open(file_path)?;
    let mut buffer = Vec::with_capacity(IMAGE_TYPE_SNIFF_BYTES);
    file.take(IMAGE_TYPE_SNIFF_BYTES as u64)
        .read_to_end(&mut buffer)?;
    Ok(detect_supported_image_mime_type(&buffer))
}

/// `isPng(buffer)`：长度 ≥ 16，IHDR chunk 长度 13，offset 12 处是 `IHDR`。
///
/// PNG 的第一个 chunk 必须是 IHDR，`IHDR` 的数据长度固定为 13；因此只看到 8 字节签名
/// 不足以认定为 PNG（`detect` 会返回 `None`，测试 `rejects_png_without_ihdr` 覆盖）。
fn is_png(buffer: &[u8]) -> bool {
    buffer.len() >= 16
        && read_uint32_be(buffer, PNG_SIGNATURE.len()) == 13
        && starts_with_ascii(buffer, 12, "IHDR")
}

/// `isAnimatedPng(buffer)`：从 offset 8 遍历 chunk；`acTL` → true，`IDAT` → false。
///
/// PNG chunk 的布局是 `[长度: 4 字节大端][类型: 4 字节][数据][CRC: 4 字节]`。这里只关心
/// 类型字段，并且与 TS 一样**在第一个 `IDAT` 处停止**：APNG 的 `acTL` 若出现在 `IDAT`
/// 之后，本函数不会看到它（测试 `accepts_png_when_idat_precedes_actl` 锁定该行为）。
/// 遍历到不能自洽的 chunk（长度越过 buffer 末尾）时返回 false，表示“已扫描的部分没有
/// `acTL`”，而不是判定文件损坏。
fn is_animated_png(buffer: &[u8]) -> bool {
    let mut offset = PNG_SIGNATURE.len();
    while offset + 8 <= buffer.len() {
        let chunk_length = read_uint32_be(buffer, offset) as usize;
        let chunk_type_offset = offset + 4;
        if starts_with_ascii(buffer, chunk_type_offset, "acTL") {
            return true;
        }
        if starts_with_ascii(buffer, chunk_type_offset, "IDAT") {
            return false;
        }
        // `offset + 8 + chunkLength + 4`，防止 usize 溢出。
        let next_offset = offset
            .saturating_add(8)
            .saturating_add(chunk_length)
            .saturating_add(4);
        if next_offset <= offset || next_offset > buffer.len() {
            return false;
        }
        offset = next_offset;
    }
    false
}

/// `isBmp(buffer)`：结构校验，规则与 TS 逐条一致。
///
/// BMP 由两个头组成：14 字节的文件头（`BM`、声明文件大小、像素数据偏移）与紧随其后的
/// DIB 头（自身长度、宽高、平面数、位深）。只看 `BM` 前缀会把任意文本误判成图片，所以
/// TS 还要求：
///
/// 1. 至少 26 字节（容得下 BITMAPCOREHEADER 的 planes/bpp 字段）；
/// 2. 声明的文件大小非 0 时不得小于 26；像素数据偏移必须 ≥ `14 + DIB 头长度`，且小于
///    声明的文件大小（声明大小为 0 表示未知，跳过上限检查）；
/// 3. DIB 头只接受 12（BITMAPCOREHEADER）或 40..=124（BITMAPINFOHEADER 及其扩展），
///    后者至少需要 30 字节才能读到 planes/bpp；
/// 4. 平面数必须为 1，位深必须是 1/4/8/16/24/32。
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
    if pixel_data_offset < 14 + dib_header_size {
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

// 以下读取器都通过 `copied` 取字节：越界位置读作 0，因此调用方只需保证“字段起始偏移”
// 在预期的语义位置上，长度不足不会 panic，只会得到按 0 填充的值。返回值比字段本身宽
// （16 位字段返回 u32，32 位字段返回 u64），供上层的长度比较与 `usize` 转换直接使用。
fn read_uint16_le(buffer: &[u8], offset: usize) -> u32 {
    u32::from(copied(buffer, offset)) + (u32::from(copied(buffer, offset + 1)) << 8)
}

fn read_uint32_be(buffer: &[u8], offset: usize) -> u64 {
    (u64::from(copied(buffer, offset)) << 24)
        + (u64::from(copied(buffer, offset + 1)) << 16)
        + (u64::from(copied(buffer, offset + 2)) << 8)
        + u64::from(copied(buffer, offset + 3))
}

fn read_uint32_le(buffer: &[u8], offset: usize) -> u64 {
    u64::from(copied(buffer, offset))
        + (u64::from(copied(buffer, offset + 1)) << 8)
        + (u64::from(copied(buffer, offset + 2)) << 16)
        + (u64::from(copied(buffer, offset + 3)) << 24)
}

/// JS 的 `buffer[i] ?? 0`：越界读 0。
fn copied(buffer: &[u8], offset: usize) -> u8 {
    buffer.get(offset).copied().unwrap_or(0)
}

fn starts_with(buffer: &[u8], bytes: &[u8]) -> bool {
    buffer.len() >= bytes.len() && buffer.starts_with(bytes)
}

/// 从 `offset` 开始逐字节比较 ASCII 字面量；长度不足返回 false，绝不越界。
fn starts_with_ascii(buffer: &[u8], offset: usize, text: &str) -> bool {
    if buffer.len() < offset + text.len() {
        return false;
    }
    text.bytes()
        .enumerate()
        .all(|(index, byte)| buffer[offset + index] == byte)
}
