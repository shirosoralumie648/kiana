//! `utils/mime.ts` 的移植测试。
//!
//! TS 没有独立的 `mime.test.ts`；魔数用例来自 `image-process.test.ts`（GIF/BMP），其余按
//! spec §2.21.19 的每条规则补齐（JPEG F7、PNG IHDR/APNG、WebP、BMP 结构校验、文件读取）。

use std::io::Write as _;

use kiana_core::mime::{
    detect_supported_image_mime_type, detect_supported_image_mime_type_from_file,
};

const PNG_SIGNATURE: [u8; 8] = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];

fn png_chunk(kind: &str, body: &[u8]) -> Vec<u8> {
    let mut chunk = Vec::new();
    chunk.extend_from_slice(&(body.len() as u32).to_be_bytes());
    chunk.extend_from_slice(kind.as_bytes());
    chunk.extend_from_slice(body);
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(&chunk[4..]);
    chunk.extend_from_slice(&hasher.finalize().to_be_bytes());
    chunk
}

fn ihdr_body(width: u32, height: u32) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&width.to_be_bytes());
    body.extend_from_slice(&height.to_be_bytes());
    body.push(8); // bit depth
    body.push(0); // color type: grayscale
    body.extend_from_slice(&[0, 0, 0]); // compression/filter/interlace
    body
}

fn png_with_chunks(chunks: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut png = PNG_SIGNATURE.to_vec();
    for (kind, body) in chunks {
        png.extend_from_slice(&png_chunk(kind, body));
    }
    png
}

/// TS 的 `createTinyBmp1x1Red24bpp`。
fn create_tiny_bmp_1x1_red_24bpp() -> Vec<u8> {
    let mut buffer = vec![0u8; 58];
    let file_size = buffer.len() as u32;
    buffer[0] = b'B';
    buffer[1] = b'M';
    write_u32_le(&mut buffer, 2, file_size);
    write_u32_le(&mut buffer, 10, 54);
    write_u32_le(&mut buffer, 14, 40);
    write_i32_le(&mut buffer, 18, 1);
    write_i32_le(&mut buffer, 22, 1);
    write_u16_le(&mut buffer, 26, 1);
    write_u16_le(&mut buffer, 28, 24);
    write_u32_le(&mut buffer, 30, 0);
    write_u32_le(&mut buffer, 34, 4);
    buffer[56] = 0xff;
    buffer
}

fn write_u16_le(buffer: &mut [u8], offset: usize, value: u16) {
    buffer[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn write_u32_le(buffer: &mut [u8], offset: usize, value: u32) {
    buffer[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn write_i32_le(buffer: &mut [u8], offset: usize, value: i32) {
    buffer[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn detect(bytes: &[u8]) -> Option<String> {
    detect_supported_image_mime_type(bytes)
}

// ---------------------------------------------------------------------------
// GIF / WebP
// ---------------------------------------------------------------------------

#[test]
fn detects_the_complete_gif87a_signature() {
    assert_eq!(detect(b"GIF87a").as_deref(), Some("image/gif"));
}

#[test]
fn detects_the_complete_gif89a_signature() {
    assert_eq!(detect(b"GIF89a").as_deref(), Some("image/gif"));
}

#[test]
fn rejects_incomplete_gif_signatures() {
    assert_eq!(detect(b"GIF87"), None);
    assert_eq!(detect(b"GIF8"), None);
    assert_eq!(detect(b""), None);
}

#[test]
fn detects_webp_from_riff_container() {
    let mut bytes = b"RIFF".to_vec();
    bytes.extend_from_slice(&[0x24, 0, 0, 0]);
    bytes.extend_from_slice(b"WEBPVP8 ");
    assert_eq!(detect(&bytes).as_deref(), Some("image/webp"));
}

#[test]
fn rejects_riff_without_webp_marker() {
    let mut bytes = b"RIFF".to_vec();
    bytes.extend_from_slice(&[0x24, 0, 0, 0]);
    bytes.extend_from_slice(b"WAVEfmt ");
    assert_eq!(detect(&bytes), None);
    assert_eq!(detect(b"RIFF"), None);
}

// ---------------------------------------------------------------------------
// JPEG
// ---------------------------------------------------------------------------

#[test]
fn detects_jpeg_from_magic_bytes() {
    assert_eq!(
        detect(&[0xff, 0xd8, 0xff, 0xe0]).as_deref(),
        Some("image/jpeg")
    );
}

#[test]
fn rejects_the_jpeg_ls_f7_marker() {
    assert_eq!(detect(&[0xff, 0xd8, 0xff, 0xf7]), None);
}

#[test]
fn a_three_byte_jpeg_prefix_is_accepted() {
    // `buffer[3]` 越界是 `undefined`，不等于 0xF7。
    assert_eq!(detect(&[0xff, 0xd8, 0xff]).as_deref(), Some("image/jpeg"));
    assert_eq!(detect(&[0xff, 0xd8]), None);
}

// ---------------------------------------------------------------------------
// PNG
// ---------------------------------------------------------------------------

#[test]
fn detects_png_with_a_valid_ihdr() {
    let png = png_with_chunks(&[
        ("IHDR", ihdr_body(1, 1)),
        ("IDAT", vec![0; 4]),
        ("IEND", Vec::new()),
    ]);
    assert_eq!(detect(&png).as_deref(), Some("image/png"));
}

#[test]
fn rejects_png_without_ihdr() {
    // 只有签名：长度 < 16。
    assert_eq!(detect(&PNG_SIGNATURE), None);

    // IHDR 长度不是 13。
    let mut ihdr = ihdr_body(1, 1);
    ihdr.push(0);
    let png = png_with_chunks(&[("IHDR", ihdr)]);
    assert_eq!(detect(&png), None);

    // offset 12 不是 IHDR。
    let png = png_with_chunks(&[("IDAT", vec![0; 13])]);
    assert_eq!(detect(&png), None);
}

#[test]
fn rejects_animated_png_with_actl_before_idat() {
    let png = png_with_chunks(&[
        ("IHDR", ihdr_body(2, 2)),
        ("acTL", vec![0; 8]),
        ("IDAT", vec![0; 4]),
        ("IEND", Vec::new()),
    ]);
    assert_eq!(detect(&png), None);
}

#[test]
fn accepts_png_when_idat_precedes_actl() {
    let png = png_with_chunks(&[
        ("IHDR", ihdr_body(2, 2)),
        ("IDAT", vec![0; 4]),
        ("acTL", vec![0; 8]),
        ("IEND", Vec::new()),
    ]);
    assert_eq!(detect(&png).as_deref(), Some("image/png"));
}

#[test]
fn stops_scanning_when_a_chunk_overruns_the_buffer() {
    // chunk 长度巨大 → `nextOffset > buffer.length` → false（不影响 IHDR 判定）。
    let mut png = PNG_SIGNATURE.to_vec();
    png.extend_from_slice(&png_chunk("IHDR", &ihdr_body(1, 1)));
    png.extend_from_slice(&0xffff_ffffu32.to_be_bytes());
    png.extend_from_slice(b"tEXt");
    assert_eq!(detect(&png).as_deref(), Some("image/png"));
}

// ---------------------------------------------------------------------------
// BMP
// ---------------------------------------------------------------------------

#[test]
fn detects_bmp_files_from_magic_bytes() {
    assert_eq!(
        detect(&create_tiny_bmp_1x1_red_24bpp()).as_deref(),
        Some("image/bmp")
    );
}

#[test]
fn rejects_short_bmp_files() {
    let mut bytes = create_tiny_bmp_1x1_red_24bpp();
    bytes.truncate(25);
    assert_eq!(detect(&bytes), None);
}

#[test]
fn accepts_bmp_with_zero_declared_file_size() {
    let mut bytes = create_tiny_bmp_1x1_red_24bpp();
    write_u32_le(&mut bytes, 2, 0);
    assert_eq!(detect(&bytes).as_deref(), Some("image/bmp"));
}

#[test]
fn rejects_bmp_when_declared_size_is_below_the_header_minimum() {
    let mut bytes = create_tiny_bmp_1x1_red_24bpp();
    write_u32_le(&mut bytes, 2, 25);
    assert_eq!(detect(&bytes), None);
}

#[test]
fn rejects_bmp_when_pixel_data_overlaps_the_headers() {
    let mut bytes = create_tiny_bmp_1x1_red_24bpp();
    write_u32_le(&mut bytes, 10, 53); // 14 + 40 = 54
    assert_eq!(detect(&bytes), None);

    // 等于 54 是允许的（`<` 才拒绝）。
    write_u32_le(&mut bytes, 10, 54);
    assert_eq!(detect(&bytes).as_deref(), Some("image/bmp"));
}

#[test]
fn rejects_bmp_when_pixel_data_starts_at_or_after_the_declared_size() {
    let mut bytes = create_tiny_bmp_1x1_red_24bpp();
    write_u32_le(&mut bytes, 2, 54);
    write_u32_le(&mut bytes, 10, 54);
    assert_eq!(detect(&bytes), None);
}

#[test]
fn supports_bitmap_core_headers() {
    let mut bytes = vec![0u8; 26];
    bytes[0] = b'B';
    bytes[1] = b'M';
    write_u32_le(&mut bytes, 2, 0);
    write_u32_le(&mut bytes, 10, 26);
    write_u32_le(&mut bytes, 14, 12);
    write_u16_le(&mut bytes, 22, 1);
    write_u16_le(&mut bytes, 24, 24);
    assert_eq!(detect(&bytes).as_deref(), Some("image/bmp"));

    // planes 不是 1。
    write_u16_le(&mut bytes, 22, 2);
    assert_eq!(detect(&bytes), None);
}

#[test]
fn rejects_unknown_dib_header_sizes() {
    let mut bytes = create_tiny_bmp_1x1_red_24bpp();
    write_u32_le(&mut bytes, 14, 16);
    assert_eq!(detect(&bytes), None);

    write_u32_le(&mut bytes, 14, 125);
    assert_eq!(detect(&bytes), None);
}

#[test]
fn requires_30_bytes_for_40_to_124_byte_headers() {
    let mut bytes = create_tiny_bmp_1x1_red_24bpp();
    bytes.truncate(29);
    // 文件仍长于 26，但 BITMAPINFOHEADER 需要 offset 28 的 bit depth。
    assert_eq!(bytes.len(), 29);
    assert_eq!(detect(&bytes), None);
    bytes.push(0);
    assert_eq!(detect(&bytes).as_deref(), Some("image/bmp"));
}

#[test]
fn rejects_unsupported_bit_depths() {
    for bits in [0u16, 2, 15, 48] {
        let mut bytes = create_tiny_bmp_1x1_red_24bpp();
        write_u16_le(&mut bytes, 28, bits);
        assert_eq!(detect(&bytes), None, "bpp {bits} must be rejected");
    }
    for bits in [1u16, 4, 8, 16, 24, 32] {
        let mut bytes = create_tiny_bmp_1x1_red_24bpp();
        write_u16_le(&mut bytes, 28, bits);
        assert_eq!(detect(&bytes).as_deref(), Some("image/bmp"));
    }
}

#[test]
fn bm_prefix_alone_is_not_a_bmp() {
    assert_eq!(detect(b"BM"), None);
    assert_eq!(detect(b"BM\0\0\0\0"), None);
}

// ---------------------------------------------------------------------------
// detectSupportedImageMimeTypeFromFile
// ---------------------------------------------------------------------------

#[test]
fn reads_the_first_4100_bytes_from_a_file() {
    let directory = tempfile::tempdir().expect("temp dir");
    let path = directory.path().join("image.png");
    let mut file = std::fs::File::create(&path).expect("create");
    let png = png_with_chunks(&[("IHDR", ihdr_body(1, 1)), ("IDAT", vec![0; 4])]);
    file.write_all(&png).expect("write png");
    // 之后的字节是垃圾数据：只读前 4100 字节。
    file.write_all(&vec![0u8; 8192]).expect("write padding");
    drop(file);

    assert_eq!(
        detect_supported_image_mime_type_from_file(&path)
            .expect("read")
            .as_deref(),
        Some("image/png")
    );
}

#[test]
fn returns_none_for_files_without_a_known_image_magic() {
    let directory = tempfile::tempdir().expect("temp dir");
    let path = directory.path().join("notes.txt");
    std::fs::write(&path, b"hello world").expect("write");
    assert_eq!(
        detect_supported_image_mime_type_from_file(&path).expect("read"),
        None
    );
}

#[test]
fn propagates_io_errors_for_missing_files() {
    let error = detect_supported_image_mime_type_from_file(std::path::Path::new(
        "/definitely/not/a/real/file.png",
    ))
    .expect_err("missing file must be an error");
    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
}

/// 差分黄金向量：由 TS 参考实现（Node 26，固定 PRNG 种子）生成，覆盖随机字节、五种格式的
/// 变异、BMP 头字段组合与 PNG chunk 顺序。
#[test]
fn matches_the_ts_reference_on_generated_vectors() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/mime_vectors.json")).expect("fixture json");
    let cases = fixture["cases"].as_array().expect("cases");
    assert!(cases.len() > 300, "expected a meaningful corpus");
    for case in cases {
        let hex = case["hex"].as_str().expect("hex");
        let bytes = decode_hex(hex);
        let expected = case["expected"].as_str().map(str::to_string);
        assert_eq!(
            detect_supported_image_mime_type(&bytes),
            expected,
            "hex: {hex}"
        );
    }
}

fn decode_hex(text: &str) -> Vec<u8> {
    assert!(text.len().is_multiple_of(2), "odd hex length");
    (0..text.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&text[index..index + 2], 16).expect("hex digit"))
        .collect()
}
