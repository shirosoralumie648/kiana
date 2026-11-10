//! `utils/zip.ts` 的测试（TS 无对应测试文件）。
//!
//! 逐字节校验 local/central/EOCD 布局（`spec/ca-cli.md` §2.21.15），并用 `flate2` 解压回读；
//! 不依赖外部 `unzip`/`python`。

use std::io::Read as _;

use chrono::{Datelike, Local, Timelike};
use flate2::read::DeflateDecoder;
use kiana_core::zip::{ZipEntry, write_zip_archive};

fn read_u16(buffer: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([buffer[offset], buffer[offset + 1]])
}

fn read_u32(buffer: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        buffer[offset],
        buffer[offset + 1],
        buffer[offset + 2],
        buffer[offset + 3],
    ])
}

fn inflate_raw(data: &[u8]) -> Vec<u8> {
    let mut decoder = DeflateDecoder::new(data);
    let mut out = Vec::new();
    decoder.read_to_end(&mut out).expect("raw deflate decodes");
    out
}

fn dos_fields() -> (u16, u16) {
    let now = Local::now();
    let time = ((now.hour() << 11) | (now.minute() << 5) | (now.second() >> 1)) as u16;
    let day = (((now.year().max(1980) - 1980) << 9)
        | ((now.month() as i32) << 5)
        | now.day() as i32) as u16;
    (time, day)
}

#[test]
fn writes_classic_zip_layout_readable_back() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("report.zip");
    let entries = vec![
        ZipEntry {
            name: "a.txt".to_string(),
            data: b"hello zip".to_vec(),
        },
        ZipEntry {
            name: "dir/b.bin".to_string(),
            data: (0u8..=255).collect(),
        },
    ];
    let (before_time, before_day) = dos_fields();
    write_zip_archive(&path, &entries).expect("write zip");
    let (after_time, after_day) = dos_fields();

    let archive = std::fs::read(&path).expect("read zip");
    let mut offset = 0usize;

    for entry in &entries {
        // local header（30 字节）
        assert_eq!(read_u32(&archive, offset), 0x0403_4b50);
        assert_eq!(read_u16(&archive, offset + 4), 20, "version needed");
        assert_eq!(read_u16(&archive, offset + 6), 0x0800, "UTF-8 flag");
        assert_eq!(read_u16(&archive, offset + 8), 8, "deflate method");
        let time = read_u16(&archive, offset + 10);
        let day = read_u16(&archive, offset + 12);
        assert!(
            (time == before_time && day == before_day) || (time == after_time && day == after_day),
            "DOS timestamp must be local now"
        );
        assert_eq!(
            read_u32(&archive, offset + 14),
            crc32fast::hash(&entry.data)
        );
        let compressed_len = read_u32(&archive, offset + 18) as usize;
        assert_eq!(read_u32(&archive, offset + 22) as usize, entry.data.len());
        let name_len = read_u16(&archive, offset + 26) as usize;
        assert_eq!(
            &archive[offset + 30..offset + 30 + name_len],
            entry.name.as_bytes()
        );

        let name = &archive[offset + 30..offset + 30 + name_len];
        assert_eq!(name, entry.name.as_bytes());
        let data_start = offset + 30 + name_len;
        let compressed = &archive[data_start..data_start + compressed_len];
        assert_eq!(
            inflate_raw(compressed),
            entry.data,
            "decompress {}",
            entry.name
        );

        offset = data_start + compressed_len;
    }

    let central_start = offset;
    let mut central_offset = 0u32;
    for entry in &entries {
        assert_eq!(read_u32(&archive, offset), 0x0201_4b50);
        assert_eq!(read_u16(&archive, offset + 4), 20, "version made by");
        assert_eq!(read_u16(&archive, offset + 6), 20, "version needed");
        assert_eq!(read_u16(&archive, offset + 8), 0x0800);
        assert_eq!(read_u16(&archive, offset + 10), 8);
        assert_eq!(
            read_u32(&archive, offset + 16),
            crc32fast::hash(&entry.data)
        );
        let compressed_len = read_u32(&archive, offset + 20) as usize;
        assert_eq!(read_u32(&archive, offset + 24) as usize, entry.data.len());
        let name_len = read_u16(&archive, offset + 28) as usize;
        assert_eq!(
            read_u32(&archive, offset + 42),
            central_offset,
            "local header offset"
        );
        assert_eq!(
            &archive[offset + 46..offset + 46 + name_len],
            entry.name.as_bytes()
        );
        offset = offset + 46 + name_len;
        central_offset += (30 + name_len + compressed_len) as u32;
    }
    let central_len = offset - central_start;

    // EOCD（22 字节）
    assert_eq!(archive.len(), offset + 22);
    assert_eq!(read_u32(&archive, offset), 0x0605_4b50);
    assert_eq!(read_u16(&archive, offset + 8) as usize, entries.len());
    assert_eq!(read_u16(&archive, offset + 10) as usize, entries.len());
    assert_eq!(read_u32(&archive, offset + 12) as usize, central_len);
    assert_eq!(read_u32(&archive, offset + 16) as usize, central_start);
}

#[test]
fn writes_empty_archive_as_eocd_only() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("empty.zip");
    write_zip_archive(&path, &[]).expect("write empty zip");
    let archive = std::fs::read(&path).expect("read zip");
    assert_eq!(archive.len(), 22);
    assert_eq!(read_u32(&archive, 0), 0x0605_4b50);
    assert_eq!(read_u16(&archive, 8), 0);
    assert_eq!(read_u16(&archive, 10), 0);
    assert_eq!(read_u32(&archive, 12), 0);
    assert_eq!(read_u32(&archive, 16), 0);
}

#[test]
fn utf8_names_are_flagged_and_preserved() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("utf8.zip");
    let entry = ZipEntry {
        name: "报告/日志.txt".to_string(),
        data: "内容".as_bytes().to_vec(),
    };
    write_zip_archive(&path, std::slice::from_ref(&entry)).expect("write zip");
    let archive = std::fs::read(&path).expect("read zip");
    assert_eq!(read_u16(&archive, 6), 0x0800);
    let name_len = read_u16(&archive, 26) as usize;
    assert_eq!(&archive[30..30 + name_len], entry.name.as_bytes());
}

#[test]
fn reports_io_errors_for_missing_directories() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("missing").join("x.zip");
    let error = write_zip_archive(&path, &[]).expect_err("missing directory must fail");
    assert!(matches!(error.kind(), std::io::ErrorKind::NotFound));
}
