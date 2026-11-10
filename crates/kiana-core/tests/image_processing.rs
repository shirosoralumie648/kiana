//! `utils/image-resize{,-core}.ts` + `utils/image-convert.ts` + `utils/exif-orientation.ts` 的移植测试。
//!
//! TS: `test/image-processing.test.ts`（`convertToPng` / `loadPngTranscoder` / `resizeImage` /
//! `formatDimensionNote`），并补充 EXIF 方向矩阵（TS 只覆盖 orientation 6）。
//!
//! 图片字节断言按 `ARCHITECTURE.md` §8 第 20 条放宽：只比对尺寸/格式/base64 限额；PNG 允许
//! 末位差异。`formatDimensionNote` 的文本用 TS 参考实现生成的黄金向量逐字比对。
#![cfg(feature = "images")]

use kiana_core::image_process::{
    ImageResizeOptions, ResizedImage, apply_exif_orientation, convert_to_png,
    ensure_png_transcoder, format_dimension_note, load_png_transcoder, resize_image,
};
use photon_rs::PhotonImage;

/// 小 2x2 红色 PNG（base64，ImageMagick 生成）。
const TINY_PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAIAAAACAQMAAABIeJ9nAAAAIGNIUk0AAHomAACAhAAA+gAAAIDoAAB1MAAA6mAAADqYAAAXcJy6UTwAAAAGUExURf8AAP///0EdNBEAAAABYktHRAH/Ai3eAAAAB3RJTUUH6gEOADM5Ddoh/wAAAAxJREFUCNdjYGBgAAAABAABJzQnCgAAACV0RVh0ZGF0ZTpjcmVhdGUAMjAyNi0wMS0xNFQwMDo1MTo1NyswMDowMOnKzHgAAAAldEVYdGRhdGU6bW9kaWZ5ADIwMjYtMDEtMTRUMDA6NTE6NTcrMDA6MDCYl3TEAAAAKHRFWHRkYXRlOnRpbWVzdGFtcAAyMDI2LTAxLTE0VDAwOjUxOjU3KzAwOjAwz4JVGwAAAABJRU5ErkJggg==";

/// 小 2x2 蓝色 JPEG（base64）。
const TINY_JPEG: &str = "/9j/4AAQSkZJRgABAQAAAQABAAD/2wBDAAMCAgMCAgMDAwMEAwMEBQgFBQQEBQoHBwYIDAoMDAsKCwsNDhIQDQ4RDgsLEBYQERMUFRUVDA8XGBYUGBIUFRT/2wBDAQMEBAUEBQkFBQkUDQsNFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBT/wAARCAACAAIDAREAAhEBAxEB/8QAFAABAAAAAAAAAAAAAAAAAAAACf/EABQQAQAAAAAAAAAAAAAAAAAAAAD/xAAVAQEBAAAAAAAAAAAAAAAAAAAGCf/EABQRAQAAAAAAAAAAAAAAAAAAAAD/2gAMAwEAAhEDEQA/AD3VTB3/2Q==";

/// 2x1 JPEG（配合 EXIF orientation 6 得到 1x2）。
const TINY_JPEG_2X1: &str = "/9j/4AAQSkZJRgABAgAAAQABAAD/wAARCAABAAIDAREAAhEBAxEB/9sAQwADAgIDAgIDAwMDBAMDBAUIBQUEBAUKBwcGCAwKDAwLCgsLDQ4SEA0OEQ4LCxAWEBETFBUVFQwPFxgWFBgSFBUU/9sAQwEDBAQFBAUJBQUJFA0LDRQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQU/8QAHwAAAQUBAQEBAQEAAAAAAAAAAAECAwQFBgcICQoL/8QAtRAAAgEDAwIEAwUFBAQAAAF9AQIDAAQRBRIhMUEGE1FhByJxFDKBkaEII0KxwRVS0fAkM2JyggkKFhcYGRolJicoKSo0NTY3ODk6Q0RFRkdISUpTVFVWV1hZWmNkZWZnaGlqc3R1dnd4eXqDhIWGh4iJipKTlJWWl5iZmqKjpKWmp6ipqrKztLW2t7i5usLDxMXGx8jJytLT1NXW19jZ2uHi4+Tl5ufo6erx8vP09fb3+Pn6/8QAHwEAAwEBAQEBAQEBAQAAAAAAAAECAwQFBgcICQoL/8QAtREAAgECBAQDBAcFBAQAAQJ3AAECAxEEBSExBhJBUQdhcRMiMoEIFEKRobHBCSMzUvAVYnLRChYkNOEl8RcYGRomJygpKjU2Nzg5OkNERUZHSElKU1RVVldYWVpjZGVmZ2hpanN0dXZ3eHl6goOEhYaHiImKkpOUlZaXmJmaoqOkpaanqKmqsrO0tba3uLm6wsPExcbHyMnK0tPU1dbX2Nna4uPk5ebn6Onq8vP09fb3+Pn6/9oADAMBAAIRAxEAPwD4H8Q/8h/Uv+vmX/0M1/o1wJ/ySWU/9g1D/wBNRMOM/wDkp8z/AOv9b/05I//Z";

/// 100x100 灰度 PNG。
const MEDIUM_PNG_100X100: &str = "iVBORw0KGgoAAAANSUhEUgAAAGQAAABkCAAAAABVicqIAAAAAmJLR0QA/4ePzL8AAAAHdElNRQfqAQ4AMzkN2iH/AAAAP0lEQVRo3u3NQQEAAAQEMASXXYrz2gqst/Lm4ZBIJBKJRCKRSCQSiUQikUgkEolEIpFIJBKJRCKRSCQSiSTsAP1cAUZeKtreAAAAJXRFWHRkYXRlOmNyZWF0ZQAyMDI2LTAxLTE0VDAwOjUxOjU3KzAwOjAw6crMeAAAACV0RVh0ZGF0ZTptb2RpZnkAMjAyNi0wMS0xNFQwMDo1MTo1NyswMDowMJiXdMQAAAAodEVYdGRhdGU6dGltZXN0YW1wADIwMjYtMDEtMTRUMDA6NTE6NTcrMDA6MDDPglUbAAAAAElFTkSuQmCC";

/// 200x200 彩色 PNG。
const LARGE_PNG_200X200: &str = "iVBORw0KGgoAAAANSUhEUgAAAMgAAADIEAIAAAByquWKAAAAIGNIUk0AAHomAACAhAAA+gAAAIDoAAB1MAAA6mAAADqYAAAXcJy6UTwAAAAGYktHRP///////wlY99wAAAAHdElNRQfqAQ4AMzqU03BFAAAEEElEQVR42u3dvWqUURSF4XXMRAyIhSAWQvwnWtjaWusFx9bW1kKD/wELESxEiBjDjJdwysOC57mCXW32+zGcGbtdACpsdi+T8Xz1GABzm91xMl6sHgNgblzcT/Y+rh4DYG6cv0/2H64eA2BOEgI1fHQHaoyzbXIwVo8BMCcJgRqSEKgxft9Lrn5aPQbA3Ph1klw7Wj0GwJwkBGpYWECN8fMiub63egyAORcWUMPCAmqMH3eTG59XjwEwN76/S24+Wj0GwJwkBGpYWECN8e1fcmuzegyAORcWUMPCAmqM09vJ4dfVYwDMjS9vkzuPV48BMCcJgRqeSAZqjA/nyYP91WMAzElCoIYkBGqMk8Pk6HT1GABzm+3L5JIkBApIQqCGj+5AjfHmb/Lk8uoxAOYkIVBDEgI1LCygxmZ7nFyShEABFxZQw8ICaozXf5KnV1aPATDnwgJqWFhADQsLqOFnDUANFxZQw8ICaoxXZ8mzg9VjAMy5sIAaFhZQw3tYQA1/QgHUkIRADUkI1JCEQA1JCNSQhEANFxZQwzcsoIYkBGpIQqCGJARqSEKghiQEalhYQA1/QgHUcGEBNSwsoIYkBGq4sIAaFhZQw8ICaviGBdRwYQE1LCyghiQEariwgBoWFlDDe1hADS+OAjUkIVBDEgI1JCFQQxICNSQhUMOFBdTwDQuoIQmBGpIQqCEJgRqSEKghCYEaFhZQw4ujQA0XFlDDwgJqSEKghgsLqGFhATUsLKCGb1hADRcWUMPCAmpIQqCGCwuoYWEBNbyHBdTw4ihQQxICNSQhUEMSAjUkIVBDEgI1XFhADd+wgBqSEKghCYEakhCoIQmBGpIQqGFhATW8OArUcGEBNSwsoIYkBGq4sIAaFhZQw8ICaviGBdRwYQE1LCyghiQEariwgBoWFlDDe1hADS+OAjUkIVBDEgI1JCFQQxICNSQhUMOFBdTwDQuoIQmBGpIQqCEJgRqSEKghCYEa49lB8ups9RgAc14cBWpIQqCGhQXUkIRADRcWUMPCAmqMp1eS139WjwEw5xsWUEMSAjUsLKCGJARquLCAGhYWUGM8uZy8+bt6DIA5L44CNSQhUMMTyUCNcXSanByuHgNgThICNSQhUGM82E8+nK8eA2DOzxqAGpIQqDHuPE6+vF09BsDcOPyanN5ePQbAnCQEavgdFlBj3Nok3/6tHgNgzoujQA1JCNQYNx8l39+tHgNgbtz4nPy4u3oMgDlJCNSwsIAa4/pe8vNi9RgAc37WANSQhECNce0o+XWyegyAuXH1U/L73uoxAOYkIVDDwgJqjIORnG1XjwEw508ogBqSEKgx9h8m5+9XjwEwN/Y+Jhf3V48BMCcJgRpjPE+2x6vHAJgbSbLbrR4DYO4/GqiSgXN+ksgAAAAldEVYdGRhdGU6Y3JlYXRlADIwMjYtMDEtMTRUMDA6NTE6NTcrMDA6MDDpysx4AAAAJXRFWHRkYXRlOm1vZGlmeQAyMDI2LTAxLTE0VDAwOjUxOjU3KzAwOjAwmJd0xAAAACh0RVh0ZGF0ZTp0aW1lc3RhbXAAMjAyNi0wMS0xNFQwMDo1MTo1NyswMDowMM+CVRsAAAAASUVORK5CYII=";

fn image_bytes(base64_data: &str) -> Vec<u8> {
    kiana_tui::terminal_image::base64_decode(base64_data)
}

fn decode(base64_data: &str) -> Vec<u8> {
    kiana_tui::terminal_image::base64_decode(base64_data)
}

fn png_dimensions(base64_data: &str) -> (u32, u32) {
    let png = decode(base64_data);
    (
        u32::from_be_bytes(png[16..20].try_into().expect("width")),
        u32::from_be_bytes(png[20..24].try_into().expect("height")),
    )
}

fn app1_segment(payload: &[u8]) -> Vec<u8> {
    let mut segment = vec![0u8; payload.len() + 4];
    segment[0] = 0xff;
    segment[1] = 0xe1;
    segment[2..4].copy_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
    segment[4..].copy_from_slice(payload);
    segment
}

/// TS 的 `jpegWithXmpBeforeOrientation()`：XMP APP1 在 Exif APP1 之前。
fn jpeg_with_xmp_before_orientation() -> String {
    let jpeg = image_bytes(TINY_JPEG_2X1);
    let xmp =
        app1_segment(b"http://ns.adobe.com/xap/1.0/\0<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"/>");
    let mut orientation_payload = b"Exif\0\0".to_vec();
    orientation_payload.extend_from_slice(&hex_bytes(
        "49492a0008000000010012010300010000000600000000000000",
    ));
    let orientation6 = app1_segment(&orientation_payload);

    let mut combined = Vec::new();
    combined.extend_from_slice(&jpeg[0..2]);
    combined.extend_from_slice(&xmp);
    combined.extend_from_slice(&orientation6);
    combined.extend_from_slice(&jpeg[2..]);
    base64_encode(&combined)
}

fn hex_bytes(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&text[index..index + 2], 16).expect("hex"))
        .collect()
}

fn base64_encode(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// 构造带指定 orientation 的 JPEG APP1 Exif 段（TIFF 只含一个 Orientation 条目）。
fn jpeg_with_orientation(orientation: u16) -> Vec<u8> {
    let mut tiff = Vec::new();
    tiff.extend_from_slice(b"II"); // little-endian
    tiff.extend_from_slice(&0x002a_u16.to_le_bytes());
    tiff.extend_from_slice(&8_u32.to_le_bytes()); // IFD offset
    tiff.extend_from_slice(&1_u16.to_le_bytes()); // entry count
    tiff.extend_from_slice(&0x0112_u16.to_le_bytes()); // tag: Orientation
    tiff.extend_from_slice(&3_u16.to_le_bytes()); // type: SHORT
    tiff.extend_from_slice(&1_u32.to_le_bytes()); // count
    tiff.extend_from_slice(&u32::from(orientation).to_le_bytes());
    tiff.extend_from_slice(&0_u32.to_le_bytes()); // next IFD

    let mut payload = b"Exif\0\0".to_vec();
    payload.extend_from_slice(&tiff);
    let segment = app1_segment(&payload);

    let mut jpeg = vec![0xff, 0xd8];
    jpeg.extend_from_slice(&segment);
    jpeg.extend_from_slice(&[0xff, 0xd9]);
    jpeg
}

/// 2x2 图：红通道依次 10/20/30/40（行优先）。
fn image_2x2() -> PhotonImage {
    let mut raw = Vec::new();
    for value in [10_u8, 20, 30, 40] {
        raw.extend_from_slice(&[value, 0, 0, 255]);
    }
    PhotonImage::new(raw, 2, 2)
}

fn reds(image: &PhotonImage) -> Vec<u8> {
    image
        .get_raw_pixels()
        .chunks(4)
        .map(|pixel| pixel[0])
        .collect()
}

// ---------------------------------------------------------------------------
// convertToPng
// ---------------------------------------------------------------------------

#[test]
fn should_return_original_data_for_png_input() {
    let result = convert_to_png(TINY_PNG, "image/png").expect("png passthrough");
    assert_eq!(result.data, TINY_PNG);
    assert_eq!(result.mime_type, "image/png");
}

#[test]
fn should_convert_jpeg_to_png() {
    let result = convert_to_png(TINY_JPEG, "image/jpeg").expect("convert jpeg");
    assert_eq!(result.mime_type, "image/png");
    let buffer = decode(&result.data);
    assert_eq!(&buffer[0..4], &[0x89, 0x50, 0x4e, 0x47]);
}

#[test]
fn should_apply_exif_orientation_after_an_xmp_app1_segment() {
    let result = convert_to_png(&jpeg_with_xmp_before_orientation(), "image/jpeg")
        .expect("convert with exif");
    assert_eq!(png_dimensions(&result.data), (1, 2));
}

#[test]
fn convert_to_png_returns_none_for_undecodable_input() {
    assert!(convert_to_png(&base64_encode(b"not an image"), "image/jpeg").is_none());
}

// Issue #10292: kiana-tui uses this transcoder to show non-PNG images on Kitty-protocol terminals.
#[test]
fn load_png_transcoder_converts_synchronously_to_oriented_png_data() {
    let transcoder = load_png_transcoder().expect("photon is available");
    let png =
        decode(&transcoder(&jpeg_with_xmp_before_orientation(), "image/jpeg").expect("transcoded"));
    assert_eq!(
        (
            u32::from_be_bytes(png[16..20].try_into().expect("width")),
            u32::from_be_bytes(png[20..24].try_into().expect("height")),
        ),
        (1, 2)
    );
    assert!(transcoder(&base64_encode(b"not an image"), "image/jpeg").is_none());
}

// ---------------------------------------------------------------------------
// resizeImage
// ---------------------------------------------------------------------------

#[test]
fn should_keep_caller_input_bytes_intact() {
    let input = image_bytes(TINY_PNG);
    let original_len = input.len();
    let original_first = input[0];

    let result = resize_image(
        &input,
        "image/png",
        Some(ImageResizeOptions {
            max_width: 100,
            max_height: 100,
            max_bytes: 1024 * 1024,
            jpeg_quality: None,
        }),
    );

    assert!(result.is_some());
    assert_eq!(input.len(), original_len);
    assert_eq!(input[0], original_first);
}

#[test]
fn should_return_original_image_if_within_limits() {
    let result = resize_image(
        &image_bytes(TINY_PNG),
        "image/png",
        Some(ImageResizeOptions {
            max_width: 100,
            max_height: 100,
            max_bytes: 1024 * 1024,
            jpeg_quality: None,
        }),
    )
    .expect("within limits");

    assert!(!result.was_resized);
    assert_eq!(result.data, TINY_PNG);
    assert_eq!(result.original_width, 2);
    assert_eq!(result.original_height, 2);
    assert_eq!(result.width, 2);
    assert_eq!(result.height, 2);
}

#[test]
fn should_resize_image_exceeding_dimension_limits() {
    let result = resize_image(
        &image_bytes(MEDIUM_PNG_100X100),
        "image/png",
        Some(ImageResizeOptions {
            max_width: 50,
            max_height: 50,
            max_bytes: 1024 * 1024,
            jpeg_quality: None,
        }),
    )
    .expect("resized");

    assert!(result.was_resized);
    assert_eq!(result.original_width, 100);
    assert_eq!(result.original_height, 100);
    assert!(result.width <= 50);
    assert!(result.height <= 50);
}

#[test]
fn should_resize_image_exceeding_byte_limit() {
    let original_buffer = image_bytes(LARGE_PNG_200X200);
    let original_size = original_buffer.len();

    let max_bytes = (LARGE_PNG_200X200.len() as f64 * 0.9).floor() as u64;
    let result = resize_image(
        &original_buffer,
        "image/png",
        Some(ImageResizeOptions {
            max_width: 2000,
            max_height: 2000,
            max_bytes,
            jpeg_quality: None,
        }),
    )
    .expect("resized below byte limit");

    let result_buffer = decode(&result.data);
    assert!(result_buffer.len() < original_size);
    assert!(result.data.len() < LARGE_PNG_200X200.len());
}

#[test]
fn should_return_null_when_image_cannot_be_resized_below_max_bytes() {
    let result = resize_image(
        &image_bytes(LARGE_PNG_200X200),
        "image/png",
        Some(ImageResizeOptions {
            max_width: 2000,
            max_height: 2000,
            max_bytes: 1,
            jpeg_quality: None,
        }),
    );
    assert!(result.is_none());
}

#[test]
fn should_handle_jpeg_input() {
    let result = resize_image(
        &image_bytes(TINY_JPEG),
        "image/jpeg",
        Some(ImageResizeOptions {
            max_width: 100,
            max_height: 100,
            max_bytes: 1024 * 1024,
            jpeg_quality: None,
        }),
    )
    .expect("jpeg within limits");

    assert!(!result.was_resized);
    assert_eq!(result.original_width, 2);
    assert_eq!(result.original_height, 2);
}

#[test]
fn returns_none_for_undecodable_input() {
    assert!(resize_image(b"not an image", "image/png", None).is_none());
}

// ---------------------------------------------------------------------------
// formatDimensionNote
// ---------------------------------------------------------------------------

fn resized(original_width: u32, original_height: u32, width: u32, height: u32) -> ResizedImage {
    ResizedImage {
        data: String::new(),
        mime_type: "image/png".to_string(),
        original_width,
        original_height,
        width,
        height,
        was_resized: true,
    }
}

#[test]
fn should_return_undefined_for_non_resized_images() {
    let note = format_dimension_note(&ResizedImage {
        data: String::new(),
        mime_type: "image/png".to_string(),
        original_width: 100,
        original_height: 100,
        width: 100,
        height: 100,
        was_resized: false,
    });
    assert_eq!(note, None);
}

#[test]
fn should_return_formatted_note_for_resized_images() {
    let note = format_dimension_note(&resized(2000, 1000, 1000, 500)).expect("note");
    assert!(note.contains("original 2000x1000"));
    assert!(note.contains("displayed at 1000x500"));
    assert!(note.contains("2.00")); // scale factor
}

/// 黄金向量：由 TS 参考实现（Node 26）生成，逐字比对 `scale.toFixed(2)`。
#[test]
fn format_dimension_note_matches_the_ts_golden_vectors() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/media_clipboard.json")).expect("fixture json");
    let cases = fixture["dimension_notes"]
        .as_array()
        .expect("dimension_notes");
    assert!(!cases.is_empty());
    for case in cases {
        let result = resized(
            case["original_width"].as_u64().expect("ow") as u32,
            case["original_height"].as_u64().expect("oh") as u32,
            case["width"].as_u64().expect("w") as u32,
            case["height"].as_u64().expect("h") as u32,
        );
        assert_eq!(
            format_dimension_note(&result).as_deref(),
            case["note"].as_str(),
            "case {case}"
        );
    }
}

// ---------------------------------------------------------------------------
// EXIF orientation 矩阵（直接测试 applyExifOrientation）
// ---------------------------------------------------------------------------

#[test]
fn applies_every_exif_orientation_to_rgba_pixels() {
    let expected: [(u16, Vec<u8>); 8] = [
        (1, vec![10, 20, 30, 40]),
        (2, vec![20, 10, 40, 30]),
        (3, vec![40, 30, 20, 10]),
        (4, vec![30, 40, 10, 20]),
        (5, vec![10, 30, 20, 40]),
        (6, vec![30, 10, 40, 20]),
        (7, vec![40, 20, 30, 10]),
        (8, vec![20, 40, 10, 30]),
    ];
    for (orientation, reds_expected) in expected {
        let bytes = jpeg_with_orientation(orientation);
        let image = apply_exif_orientation(image_2x2(), &bytes);
        assert_eq!(image.get_width(), 2, "orientation {orientation} width");
        assert_eq!(image.get_height(), 2, "orientation {orientation} height");
        assert_eq!(
            reds(&image),
            reds_expected,
            "orientation {orientation} pixels"
        );
    }
}

#[test]
fn rotations_swap_width_and_height() {
    let mut raw = Vec::new();
    for value in [10_u8, 20] {
        raw.extend_from_slice(&[value, 0, 0, 255]);
    }
    let image = PhotonImage::new(raw, 2, 1);

    let rotated = apply_exif_orientation(image, &jpeg_with_orientation(6));
    assert_eq!((rotated.get_width(), rotated.get_height()), (1, 2));
    assert_eq!(reds(&rotated), vec![10, 20]);
}

#[test]
fn ignores_missing_or_invalid_orientation() {
    // 没有 EXIF → orientation 1 → 原样（同一像素）。
    let unchanged = apply_exif_orientation(image_2x2(), b"not a jpeg");
    assert_eq!(reds(&unchanged), vec![10, 20, 30, 40]);

    // 越界值（0 / 9）→ 回落到 1。
    for orientation in [0_u16, 9] {
        let image = apply_exif_orientation(image_2x2(), &jpeg_with_orientation(orientation));
        assert_eq!(reds(&image), vec![10, 20, 30, 40]);
    }
}

// ---------------------------------------------------------------------------
// ensurePngTranscoder
// ---------------------------------------------------------------------------

#[test]
fn ensure_png_transcoder_registers_once_on_kitty_terminals() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    kiana_tui::set_capabilities(kiana_tui::TerminalCapabilities {
        images: Some(kiana_tui::ImageProtocol::Kitty),
        true_color: false,
        hyperlinks: false,
    });

    let calls = std::sync::Arc::new(AtomicUsize::new(0));
    let calls_for_callback = calls.clone();
    ensure_png_transcoder(move || {
        calls_for_callback.fetch_add(1, Ordering::SeqCst);
    });
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // 已注册：后续调用直接返回（TS 的 `pngTranscoderRegistered` 早退）。
    let second = std::sync::Arc::new(AtomicUsize::new(0));
    let second_for_callback = second.clone();
    ensure_png_transcoder(move || {
        second_for_callback.fetch_add(1, Ordering::SeqCst);
    });
    assert_eq!(second.load(Ordering::SeqCst), 0);
}

#[test]
fn ensure_png_transcoder_ignores_non_kitty_terminals() {
    use std::sync::atomic::{AtomicBool, Ordering};

    kiana_tui::set_capabilities(kiana_tui::TerminalCapabilities {
        images: Some(kiana_tui::ImageProtocol::Iterm2),
        true_color: false,
        hyperlinks: false,
    });

    let called = std::sync::Arc::new(AtomicBool::new(false));
    let called_for_callback = called.clone();
    ensure_png_transcoder(move || {
        called_for_callback.store(true, Ordering::SeqCst);
    });
    assert!(!called.load(Ordering::SeqCst));
}
