//! `utils/image-process.ts` 的移植测试（TS: `test/image-process.test.ts` + spec §2.21.20 的补充）。
#![cfg(feature = "images")]

use kiana_core::image_process::{
    ImageResizeOptions, ProcessImageOptions, ProcessImageResult, process_image,
};
use kiana_core::mime::detect_supported_image_mime_type;

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

/// 100x100 24bpp BMP（行宽 300 字节，本身 4 字节对齐）。
fn create_bmp_100x100() -> Vec<u8> {
    let width = 100usize;
    let height = 100usize;
    let row_bytes = width * 3;
    let size = 54 + row_bytes * height;
    let mut buffer = vec![0u8; size];
    buffer[0] = b'B';
    buffer[1] = b'M';
    write_u32_le(&mut buffer, 2, size as u32);
    write_u32_le(&mut buffer, 10, 54);
    write_u32_le(&mut buffer, 14, 40);
    write_i32_le(&mut buffer, 18, width as i32);
    write_i32_le(&mut buffer, 22, height as i32);
    write_u16_le(&mut buffer, 26, 1);
    write_u16_le(&mut buffer, 28, 24);
    write_u32_le(&mut buffer, 34, (row_bytes * height) as u32);
    for y in 0..height {
        for x in 0..width {
            let offset = 54 + y * row_bytes + x * 3;
            buffer[offset] = (x % 256) as u8;
            buffer[offset + 1] = (y % 256) as u8;
            buffer[offset + 2] = 0x40;
        }
    }
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

fn expect_png_magic(base64_data: &str) {
    let buffer = kiana_tui::terminal_image::base64_decode(base64_data);
    assert_eq!(&buffer[0..4], &[0x89, 0x50, 0x4e, 0x47]);
}

fn decode(base64_data: &str) -> Vec<u8> {
    kiana_tui::terminal_image::base64_decode(base64_data)
}

// ---------------------------------------------------------------------------
// image processing pipeline（TS `describe("image processing pipeline")`）
// ---------------------------------------------------------------------------

#[test]
fn detects_the_complete_gif87a_signature() {
    assert_eq!(
        detect_supported_image_mime_type(b"GIF87a").as_deref(),
        Some("image/gif")
    );
}

#[test]
fn detects_the_complete_gif89a_signature() {
    assert_eq!(
        detect_supported_image_mime_type(b"GIF89a").as_deref(),
        Some("image/gif")
    );
}

#[test]
fn detects_bmp_files_from_magic_bytes() {
    assert_eq!(
        detect_supported_image_mime_type(&create_tiny_bmp_1x1_red_24bpp()).as_deref(),
        Some("image/bmp")
    );
}

#[test]
fn converts_bmp_files_to_png_attachments_when_auto_resize_is_disabled() {
    let result = process_image(
        &create_tiny_bmp_1x1_red_24bpp(),
        "image/bmp",
        Some(ProcessImageOptions {
            auto_resize_images: Some(false),
            resize_options: None,
        }),
    );

    let ProcessImageResult::Ok {
        data,
        mime_type,
        hints,
    } = result
    else {
        panic!("expected a converted image, got {result:?}");
    };
    assert_eq!(mime_type, "image/png");
    assert!(hints.contains(&"[Image converted from image/bmp to image/png.]".to_string()));
    expect_png_magic(&data);
}

#[test]
fn converts_bmp_files_before_auto_resizing() {
    let result = process_image(&create_tiny_bmp_1x1_red_24bpp(), "image/bmp", None);

    let ProcessImageResult::Ok {
        data,
        mime_type,
        hints,
    } = result
    else {
        panic!("expected a converted image, got {result:?}");
    };
    assert_eq!(mime_type, "image/png");
    assert_eq!(
        hints,
        vec!["[Image converted from image/bmp to image/png.]"]
    );
    expect_png_magic(&data);
}

// ---------------------------------------------------------------------------
// 补充：§2.21.20 的失败文案、hints 顺序、MIME 归一化
// ---------------------------------------------------------------------------

#[test]
fn reports_the_conversion_failure_message_for_unsupported_formats() {
    let result = process_image(b"definitely not an image", "image/bmp", None);
    assert_eq!(
        result,
        ProcessImageResult::Err {
            message: "[Image omitted: could not be converted to a supported inline image format.]"
                .to_string()
        }
    );

    // autoResizeImages=false 也要先尝试转换。
    let result = process_image(
        b"definitely not an image",
        "image/tiff",
        Some(ProcessImageOptions {
            auto_resize_images: Some(false),
            resize_options: None,
        }),
    );
    assert!(matches!(result, ProcessImageResult::Err { .. }));
}

#[test]
fn reports_the_resize_failure_message_when_the_byte_limit_is_unreachable() {
    let tiny_png = kiana_tui::terminal_image::base64_decode(
        "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8DwHwAFBQIAX8jx0gAAAABJRU5ErkJggg==",
    );
    let result = process_image(
        &tiny_png,
        "image/png",
        Some(ProcessImageOptions {
            auto_resize_images: Some(true),
            resize_options: Some(ImageResizeOptions {
                max_bytes: 1,
                ..Default::default()
            }),
        }),
    );
    assert_eq!(
        result,
        ProcessImageResult::Err {
            message: "[Image omitted: could not be resized below the inline image size limit.]"
                .to_string()
        }
    );
}

#[test]
fn normalizes_jpg_and_parameterized_mime_types() {
    let tiny_png = kiana_tui::terminal_image::base64_decode(
        "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8DwHwAFBQIAX8jx0gAAAABJRU5ErkJggg==",
    );
    // `image/png; charset=binary` 的 base 是 image/png → 不转换、不缩放。
    let result = process_image(
        &tiny_png,
        "IMAGE/PNG; charset=binary",
        Some(ProcessImageOptions {
            auto_resize_images: Some(false),
            resize_options: None,
        }),
    );
    let ProcessImageResult::Ok {
        data,
        mime_type,
        hints,
    } = result
    else {
        panic!("expected png passthrough, got {result:?}");
    };
    assert_eq!(mime_type, "image/png");
    assert!(hints.is_empty());
    assert_eq!(decode(&data), tiny_png);
}

#[test]
fn puts_the_conversion_hint_before_the_dimension_note() {
    let bmp = create_bmp_100x100();
    let result = process_image(
        &bmp,
        "image/bmp",
        Some(ProcessImageOptions {
            auto_resize_images: Some(true),
            resize_options: Some(ImageResizeOptions {
                max_width: 50,
                max_height: 50,
                ..Default::default()
            }),
        }),
    );

    let ProcessImageResult::Ok {
        data,
        mime_type,
        hints,
    } = result
    else {
        panic!("expected a resized image, got {result:?}");
    };
    assert_eq!(mime_type, "image/png");
    assert_eq!(hints[0], "[Image converted from image/bmp to image/png.]");
    assert!(hints[1].contains("original 100x100"), "hints: {hints:?}");
    assert!(hints[1].contains("displayed at 50x50"), "hints: {hints:?}");
    let png = decode(&data);
    assert_eq!(&png[0..4], &[0x89, 0x50, 0x4e, 0x47]);
}

#[test]
fn returns_the_original_bytes_when_auto_resize_is_disabled() {
    let tiny_png = kiana_tui::terminal_image::base64_decode(
        "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8DwHwAFBQIAX8jx0gAAAABJRU5ErkJggg==",
    );
    let result = process_image(
        &tiny_png,
        "image/png",
        Some(ProcessImageOptions {
            auto_resize_images: Some(false),
            resize_options: None,
        }),
    );
    let ProcessImageResult::Ok { data, hints, .. } = result else {
        panic!("expected success");
    };
    assert_eq!(decode(&data), tiny_png);
    assert!(hints.is_empty());
}

#[test]
fn worker_response_type_constant_is_preserved() {
    // TS-only 常量：Rust 没有 worker，但字符串值必须保留。
    assert_eq!(
        kiana_core::image_process::IMAGE_RESIZE_WORKER_RESPONSE_TYPE,
        "kiana:image-resize-response"
    );
}
