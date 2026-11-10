//! utils/zip.ts。
//!
//! TS: `packages/coding-agent/src/utils/zip.ts`（72 行）。
//!
//! 经典 ZIP（`spec/ca-cli.md` §2.21.15 / §8.2 第 14 条）：
//!
//! - local header 30 字节：`0x04034b50`、version 20、flags `0x0800`（UTF-8）、method 8
//!   （deflate）、DOS time/day、CRC32、压缩长度、原始长度、name 长度；
//! - central header 46 字节：`0x02014b50`、version made by 20、version needed 20、flags、
//!   method、time/day、CRC32、长度、name 长度、offset（写在 42）；
//! - EOCD 22 字节：`0x06054b50`、条目数（8、10 两处）、central 目录长度（12）、offset（16）。
//!
//! `deflateRawSync` → `flate2` 的 raw deflate（默认压缩级别 6，与 Node 相同）；
//! `zlib.crc32` → `crc32fast`（字节一致）。
//!
//! DOS 时间取**本地时间**：`time = (h<<11)|(m<<5)|(s>>1)`，
//! `day = ((max(1980,year)-1980)<<9)|((month+1)<<5)|date`（`chrono::Local`）。
//!
//! 已知差异：`zlib` 与 `flate2`（miniz_oxide）在相同 level 下的匹配搜索不同，压缩后的
//! **字节**可能不同，但解压结果一致；CRC 与头字段逐字节一致。
//!
//! ## 使用方与写盘边界
//!
//! 仓库内调用方只有 `kiana-resources::bug_report::write_bug_report_archive`（以及
//! `tests/zip.rs`）：把若干内存中的文本条目打成一个 bug report 归档。归档先在内存中完整
//! 组装，再一次性 `std::fs::write`：
//!
//! - 不创建父目录；父目录不存在时返回 `io::ErrorKind::NotFound`（测试断言）；
//! - 目标已存在时被创建/截断覆盖，不是追加或合并；
//! - deflate 失败发生在写盘之前，因此不会留下半成品；写盘本身失败时目标文件可能已被截断。
//!
//! ## 宽度截断（调用方需知的上限）
//!
//! 头字段是固定的 16/32 位宽，代码用 `as u16` / `as u32` 转换长度与偏移，**不做范围
//! 检查**：条目名超过 65535 字节、单条长度或整包偏移超过 4 GiB 时字段会被静默截断，
//! 生成的 ZIP 无法解压。本函数不做任何上限检查，复用到可能超限的场景时需由调用方限制。
//!
//! 条目名一律按 UTF-8 字节写入并置 flag `0x0800`；不生成目录条目（`dir/` 形式的名字要
//! 调用方自己作为普通条目传入），extra field / 注释 / 外部属性的长度字段保持 0。
//!
//! 时间戳：每次调用只取**一次** `Local::now()`，同一归档内所有条目共用同一个 DOS 时间。

use std::io;
use std::path::Path;

use chrono::{Datelike, Local, Timelike};
use flate2::Compression;
use flate2::write::DeflateEncoder;

/// 一个 ZIP 条目（TS `interface ZipEntry { name: string; data: string | Uint8Array }`）。
///
/// 条目顺序即写入归档的顺序；不做路径规范化或去重。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZipEntry {
    /// 归档内的相对路径（可含 `/`），按 UTF-8 字节直接写入；长度必须 < 65536 字节，
    /// 否则 u16 长度字段会截断。
    pub name: String,
    /// 原始（未压缩）内容；写入时做 raw deflate 并计算 CRC32。
    pub data: Vec<u8>,
}

/// TS: `dosDateTime(date)`。
///
/// 返回 `(time, day)` 两个 DOS 位域。`time` 的秒只保留偶数（`second >> 1` 丢弃最低位，
/// DOS 精度 2 秒）；`day` 的年份下限是 1980。TS 公式里的 `month + 1` 是因为 JS
/// `getMonth()` 从 0 开始，而 `chrono` 的 `Datelike::month()` 已从 1 开始，所以这里直接使用。
fn dos_date_time(date: &chrono::DateTime<Local>) -> (u16, u16) {
    let time = ((date.hour() << 11) | (date.minute() << 5) | (date.second() >> 1)) as u16;
    let year = date.year().max(1980);
    let day = (((year - 1980) << 9) | ((date.month() as i32) << 5) | date.day() as i32) as u16;
    (time, day)
}

/// raw deflate（不带 zlib/gzip 头），对应 TS 的 `zlib.deflateRawSync`。
fn deflate_raw(data: &[u8]) -> io::Result<Vec<u8>> {
    use std::io::Write as _;
    let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(data)?;
    encoder.finish()
}

fn write_u16(buffer: &mut [u8], offset: usize, value: u16) {
    buffer[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn write_u32(buffer: &mut [u8], offset: usize, value: u32) {
    buffer[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

/// TS: `createZipArchive(entries)`。
fn create_zip_archive(entries: &[ZipEntry]) -> io::Result<Vec<u8>> {
    let mut files: Vec<u8> = Vec::new();
    let mut directory: Vec<u8> = Vec::new();
    let (time, day) = dos_date_time(&Local::now());
    let mut offset: u32 = 0;

    // 两段缓冲区：`files` 是 local header + 数据区，`directory` 是 central directory，
    // 最后与 EOCD 拼接。`offset` 始终等于下一条 local header 在 `files` 中的起点。
    for entry in entries {
        let name = entry.name.as_bytes();
        let data = &entry.data;
        let compressed = deflate_raw(data)?;
        let checksum = crc32fast::hash(data);

        // local header（30 字节）：version needed=20、UTF-8 flag=0x0800、method=8（deflate）；
        // extra field 长度（offset 28）保持 0。
        let mut local = [0u8; 30];
        write_u32(&mut local, 0, 0x0403_4b50);
        write_u16(&mut local, 4, 20);
        write_u16(&mut local, 6, 0x0800);
        write_u16(&mut local, 8, 8);
        write_u16(&mut local, 10, time);
        write_u16(&mut local, 12, day);
        write_u32(&mut local, 14, checksum);
        write_u32(&mut local, 18, compressed.len() as u32);
        write_u32(&mut local, 22, data.len() as u32);
        write_u16(&mut local, 26, name.len() as u16);

        // central header（46 字节）：version made by 与 version needed 都是 20；offset 42 处
        // 写入该条目 local header 的起始偏移，extra/comment/属性字段保持 0。
        let mut central = [0u8; 46];
        write_u32(&mut central, 0, 0x0201_4b50);
        write_u16(&mut central, 4, 20);
        write_u16(&mut central, 6, 20);
        write_u16(&mut central, 8, 0x0800);
        write_u16(&mut central, 10, 8);
        write_u16(&mut central, 12, time);
        write_u16(&mut central, 14, day);
        write_u32(&mut central, 16, checksum);
        write_u32(&mut central, 20, compressed.len() as u32);
        write_u32(&mut central, 24, data.len() as u32);
        write_u16(&mut central, 28, name.len() as u16);
        write_u32(&mut central, 42, offset);

        files.extend_from_slice(&local);
        files.extend_from_slice(name);
        files.extend_from_slice(&compressed);

        directory.extend_from_slice(&central);
        directory.extend_from_slice(name);

        offset += (local.len() + name.len() + compressed.len()) as u32;
    }

    // EOCD（22 字节）：磁盘号（4/6）与注释长度（20）保持 0；条目数在 8 与 10 两处都写，
    // 单盘归档下两者相同。
    let mut end = [0u8; 22];
    write_u32(&mut end, 0, 0x0605_4b50);
    write_u16(&mut end, 8, entries.len() as u16);
    write_u16(&mut end, 10, entries.len() as u16);
    write_u32(&mut end, 12, directory.len() as u32);
    write_u32(&mut end, 16, offset);

    let mut archive = files;
    archive.extend_from_slice(&directory);
    archive.extend_from_slice(&end);
    Ok(archive)
}

/// TS: `writeZipArchive(filePath, entries)` —— 创建 bug report 使用的小型经典 ZIP 归档。
///
/// 先在内存组装完整归档，再 `std::fs::write` 写出：创建或截断目标文件，**不创建父目录**。
/// 空 `entries` 也会写出一个只有 EOCD 的 22 字节归档（测试断言）。
/// 错误：deflate 编码错误与文件 I/O 错误都原样以 `io::Error` 返回，没有额外包装。
pub fn write_zip_archive(path: &Path, entries: &[ZipEntry]) -> io::Result<()> {
    std::fs::write(path, create_zip_archive(entries)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone as _;

    #[test]
    fn dos_date_time_matches_js_formulas() {
        let date = Local.with_ymd_and_hms(2024, 3, 9, 14, 25, 37).unwrap();
        let (time, day) = dos_date_time(&date);
        assert_eq!(time, (14 << 11) | (25 << 5) | (37 >> 1));
        assert_eq!(day, ((2024 - 1980) << 9) | (3 << 5) | 9);

        // 1980 之前的年份被夹到 1980。
        let date = Local.with_ymd_and_hms(1970, 1, 1, 0, 0, 0).unwrap();
        let (_, day) = dos_date_time(&date);
        assert_eq!(day, (1 << 5) | 1);
    }
}
