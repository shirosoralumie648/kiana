//! `images/exif.ts`——从 JPEG/WebP 字节里读 EXIF 方向，不解码图片。
//!
//! TS 是手写的 TIFF/APP1/WebP-EXIF 扫描器；这里逐行移植。边界行为（截断的段、非 EXIF 的
//! APP1、WebP 的 `EXIF` chunk 前缀、值为 0 或 >8 时回落 1）是当前稳定契约，改动需同步测试。
//!
//! ## 输入、输出与边界
//!
//! 输入是完整文件字节（任意字节序列都可传入），输出是 `1..=8` 的标准 EXIF Orientation；
//! `1` 同时表示“没有方向信息”和“解析失败”，调用方无法区分。函数只读取 JPEG 段/WebP chunk
//! 的头字段与 TIFF/IFD 条目，不读取像素数据；扫描可能沿段表/chunk 表到达文件尾部，也不校验
//! 图片其余结构。
//!
//! ## 扫描流程
//!
//! 1. `tiff_start` 定位 TIFF 头：JPEG 从 `FFD8` 起逐段跳过，取负载以 `Exif\0\0` 开头的
//!    APP1；WebP 从 RIFF 头之后逐 chunk 跳过，取 `EXIF` chunk（chunk 数据可能再带
//!    `Exif\0\0` 前缀，带与不带都接受）。
//! 2. TIFF 头前两字节为 `II`（0x49 0x49）时按小端解析，**其余一律按大端**。
//! 3. 从 TIFF 头 +4 的偏移读出 IFD0 起点，遍历其条目，找 Tag `0x0112`（Orientation）；
//!    SHORT 值就在条目的值字段（条目 +8），不在 `1..=8` 内则返回 1。
//!
//! ## 已知的宽松之处
//!
//! 只扫描 IFD0 的第一个 `0x0112`，不跟随子 IFD，也不校验条目的 TIFF 类型/计数字段；
//! 截断、越界或扫描结束仍定位不到 EXIF 时静默返回 1；非 EXIF 的段（如 APP0、XMP APP1）
//! 按声明长度跳过，继续扫描后续段（fixture 的 app0-5、xmp-then-exif 覆盖这一行为）。这些
//! 行为与 TS 的手写扫描器一致，由 `tests/fixtures/tools_images_cases.json` 的 `exif_cases`
//! 与 `tests/tools_images.rs` 锁定。

/// `exifOrientation(bytes)`：JPEG 或 WebP 的 EXIF 方向，1 表示缺失/不可读。
///
/// 返回标准的 `1..=8`；`1` 同时覆盖“没有 EXIF”“结构截断/越界”“方向值不在 1..=8”三种情况。
/// 函数只读取 JPEG 段/WebP chunk 的头字段与 TIFF/IFD 条目，不读取像素数据；扫描可能沿
/// 段表/chunk 表到达文件尾部；对任意输入都不会 panic。
pub fn exif_orientation(bytes: &[u8]) -> u8 {
    let tiff = tiff_start(bytes);
    if tiff < 0 || tiff + 8 > bytes.len() as i64 {
        return 1;
    }
    let tiff = tiff as usize;
    let little = bytes[tiff] == 0x49 && bytes[tiff + 1] == 0x49;
    let u16_at = |at: usize| -> u16 {
        if little {
            u16::from(byte_at(bytes, at)) | (u16::from(byte_at(bytes, at + 1)) << 8)
        } else {
            (u16::from(byte_at(bytes, at)) << 8) | u16::from(byte_at(bytes, at + 1))
        }
    };
    let u32_at = |at: usize| -> u32 {
        if little {
            u32::from(byte_at(bytes, at))
                | (u32::from(byte_at(bytes, at + 1)) << 8)
                | (u32::from(byte_at(bytes, at + 2)) << 16)
                | (u32::from(byte_at(bytes, at + 3)) << 24)
        } else {
            (u32::from(byte_at(bytes, at)) << 24)
                | (u32::from(byte_at(bytes, at + 1)) << 16)
                | (u32::from(byte_at(bytes, at + 2)) << 8)
                | u32::from(byte_at(bytes, at + 3))
        }
    };
    let directory = tiff as u64 + u64::from(u32_at(tiff + 4));
    if directory + 2 > bytes.len() as u64 {
        return 1;
    }
    let directory = directory as usize;
    let entries = u16_at(directory);
    for index in 0..entries {
        let entry = directory as u64 + 2 + u64::from(index) * 12;
        if entry + 12 > bytes.len() as u64 {
            return 1;
        }
        let entry = entry as usize;
        // Tag 0x0112 是 Orientation，SHORT 值就在条目里。
        if u16_at(entry) == 0x0112 {
            let value = u16_at(entry + 8);
            return if (1..=8).contains(&value) {
                value as u8
            } else {
                1
            };
        }
    }
    1
}

/// JPEG APP1 或 WebP `EXIF` chunk 的 TIFF 头位置；没有时返回 -1。
fn tiff_start(bytes: &[u8]) -> i64 {
    // JPEG：SOI（FFD8）之后是段表，每段是 0xFF + marker + 2 字节大端长度
    // （长度字段包含它自己的 2 字节，但不含 marker）。
    if byte_at(bytes, 0) == 0xff && byte_at(bytes, 1) == 0xd8 {
        let mut offset: u64 = 2;
        while offset + 4 <= bytes.len() as u64 {
            let offset_usize = offset as usize;
            if byte_at(bytes, offset_usize) != 0xff {
                return -1;
            }
            let marker = byte_at(bytes, offset_usize + 1);
            // 段头前允许出现重复的 0xFF 填充字节。
            if marker == 0xff {
                offset += 1;
                continue;
            }
            // APP1 的负载从段头后 4 字节开始；命中 `Exif\0\0` 时 TIFF 头在
            // 2（marker）+ 2（长度）+ 6（`Exif\0\0`）= 10 字节之后。
            if marker == 0xe1 && is_exif_header(bytes, offset_usize + 4) {
                return (offset + 10) as i64;
            }
            let segment = (u64::from(byte_at(bytes, offset_usize + 2)) << 8)
                | u64::from(byte_at(bytes, offset_usize + 3));
            offset += 2 + segment;
        }
        return -1;
    }
    // WebP：RIFF 容器，偏移 12 起是 chunk；每个 chunk 是 4 字节 fourcc + 小端 u32 大小，
    // 数据不足偶数长度时补 1 字节。
    if ascii(bytes, 0, "RIFF") && ascii(bytes, 8, "WEBP") {
        let mut offset: u64 = 12;
        while offset + 8 <= bytes.len() as u64 {
            let offset_usize = offset as usize;
            let size = u64::from(byte_at(bytes, offset_usize + 4))
                | (u64::from(byte_at(bytes, offset_usize + 5)) << 8)
                | (u64::from(byte_at(bytes, offset_usize + 6)) << 16)
                | (u64::from(byte_at(bytes, offset_usize + 7)) << 24);
            // 有些 WebP 文件在 TIFF 头前加了 "Exif\0\0"。
            if ascii(bytes, offset_usize, "EXIF") {
                return if is_exif_header(bytes, offset_usize + 8) {
                    (offset + 14) as i64
                } else {
                    (offset + 8) as i64
                };
            }
            // RIFF chunk 按偶数大小对齐。
            offset += 8 + size + (size % 2);
        }
    }
    -1
}

/// `offset` 起是否为 `Exif\0\0`。前 4 个字符用 `byte_at`（越界按 0）比较，但结尾的两个
/// NUL 必须真实存在（用 `get`），因此截断在 NUL 之前的负载不会被误认成 EXIF 头。
fn is_exif_header(bytes: &[u8], offset: usize) -> bool {
    ascii(bytes, offset, "Exif")
        && bytes.get(offset + 4).copied() == Some(0)
        && bytes.get(offset + 5).copied() == Some(0)
}

/// 在 `offset` 处逐字节比较 ASCII 文本；越界字节按 0 处理。
fn ascii(bytes: &[u8], offset: usize, text: &str) -> bool {
    for (index, character) in text.bytes().enumerate() {
        if byte_at(bytes, offset + index) != character {
            return false;
        }
    }
    true
}

/// `bytes[offset] ?? 0`：越界按 0 处理，与 TS 里读 `Uint8Array` 的 `undefined` 比较语义一致。
fn byte_at(bytes: &[u8], offset: usize) -> u8 {
    bytes.get(offset).copied().unwrap_or(0)
}
