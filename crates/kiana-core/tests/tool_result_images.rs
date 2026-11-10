//! `utils/tool-result-images.ts` 的移植测试（TS: `test/tool-result-images.test.ts`）。
//!
//! `Cow::Borrowed` 对应 TS 的「返回同一数组引用」：调用方（`kiana-session`）用
//! `matches!(.., Cow::Borrowed(_))` 判断是否改写结果。
#![cfg(feature = "images")]

use std::borrow::Cow;
use std::io::Write as _;

use kiana_ai::{ImageContent, TextContent, UserContentBlock};
use kiana_core::image_process::{
    NormalizeToolResultImagesOptions, ToolResultContent, normalize_tool_result_images,
};

const TINY_PNG_BASE64: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8DwHwAFBQIAX8jx0gAAAABJRU5ErkJggg==";

/// TS 的 `pngChunk(type, body)`。
fn png_chunk(kind: &str, body: &[u8]) -> Vec<u8> {
    let mut header = Vec::new();
    header.extend_from_slice(&(body.len() as u32).to_be_bytes());
    header.extend_from_slice(kind.as_bytes());
    let mut checksum = crc32fast::Hasher::new();
    checksum.update(&header[4..]);
    checksum.update(body);
    let mut chunk = header;
    chunk.extend_from_slice(body);
    chunk.extend_from_slice(&checksum.finalize().to_be_bytes());
    chunk
}

/// TS 的 `createPng(width, height)`：8-bit 灰度 PNG（不引入编码器）。
fn create_png(width: u32, height: u32) -> Vec<u8> {
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.push(8); // bit depth
    ihdr.push(0); // color type: grayscale
    ihdr.extend_from_slice(&[0, 0, 0]);

    let stride = width as usize + 1;
    let mut raw = vec![0u8; stride * height as usize];
    for row in 0..height as usize {
        let value = (row % 256) as u8;
        raw[row * stride + 1..(row + 1) * stride].fill(value);
    }
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(&raw).expect("deflate");
    let compressed = encoder.finish().expect("finish deflate");

    let mut png = vec![0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
    png.extend_from_slice(&png_chunk("IHDR", &ihdr));
    png.extend_from_slice(&png_chunk("IDAT", &compressed));
    png.extend_from_slice(&png_chunk("IEND", &[]));
    png
}

fn read_png_dimensions(base64_data: &str) -> (u32, u32) {
    let buffer = kiana_tui::terminal_image::base64_decode(base64_data);
    (
        u32::from_be_bytes(buffer[16..20].try_into().expect("width")),
        u32::from_be_bytes(buffer[20..24].try_into().expect("height")),
    )
}

fn create_tiny_bmp_1x1_red_24bpp() -> Vec<u8> {
    let mut buffer = vec![0u8; 58];
    let file_size = buffer.len() as u32;
    buffer[0] = b'B';
    buffer[1] = b'M';
    buffer[2..6].copy_from_slice(&file_size.to_le_bytes());
    buffer[10..14].copy_from_slice(&54u32.to_le_bytes());
    buffer[14..18].copy_from_slice(&40u32.to_le_bytes());
    buffer[18..22].copy_from_slice(&1i32.to_le_bytes());
    buffer[22..26].copy_from_slice(&1i32.to_le_bytes());
    buffer[26..28].copy_from_slice(&1u16.to_le_bytes());
    buffer[28..30].copy_from_slice(&24u16.to_le_bytes());
    buffer[30..34].copy_from_slice(&0u32.to_le_bytes());
    buffer[34..38].copy_from_slice(&4u32.to_le_bytes());
    buffer[56] = 0xff;
    buffer
}

fn image_block(bytes: &[u8], mime_type: &str) -> ToolResultContent {
    use base64::Engine as _;
    UserContentBlock::Image(ImageContent {
        data: base64::engine::general_purpose::STANDARD.encode(bytes),
        mime_type: mime_type.to_string(),
        ..Default::default()
    })
}

fn text_block(text: &str) -> ToolResultContent {
    UserContentBlock::Text(TextContent {
        text: text.to_string(),
        ..Default::default()
    })
}

fn block_type(block: &ToolResultContent) -> &'static str {
    match block {
        UserContentBlock::Text(_) => "text",
        UserContentBlock::Image(_) => "image",
    }
}

#[allow(
    clippy::ptr_arg,
    reason = "helper 只判断变体；按值接收会移动 Cow，调用点还要继续使用它"
)]
fn is_borrowed(result: &Cow<'_, [ToolResultContent]>) -> bool {
    matches!(result, Cow::Borrowed(_))
}

// ---------------------------------------------------------------------------
// describe("normalizeToolResultImages")
// ---------------------------------------------------------------------------

#[test]
fn returns_the_original_array_when_there_are_no_image_blocks() {
    let content: Vec<ToolResultContent> = vec![text_block("no images here")];

    let result = normalize_tool_result_images(&content, None);
    assert!(is_borrowed(&result));
    assert!(std::ptr::eq(result.as_ref().as_ptr(), content.as_ptr()));
}

#[test]
fn returns_the_original_array_when_images_are_already_within_limits() {
    let content: Vec<ToolResultContent> = vec![
        text_block("screenshot"),
        image_block(
            &kiana_tui::terminal_image::base64_decode(TINY_PNG_BASE64),
            "image/png",
        ),
    ];

    let result = normalize_tool_result_images(&content, None);
    assert!(is_borrowed(&result));
    assert!(std::ptr::eq(result.as_ref().as_ptr(), content.as_ptr()));
}

#[test]
fn resizes_oversized_images_and_reports_the_original_dimensions() {
    let content: Vec<ToolResultContent> = vec![image_block(&create_png(2400, 4800), "image/png")];

    let normalized = normalize_tool_result_images(&content, None);
    assert!(!is_borrowed(&normalized));
    assert_eq!(normalized.len(), 2);

    let image = &normalized[0];
    assert_eq!(block_type(image), "image");
    let UserContentBlock::Image(image) = image else {
        unreachable!()
    };
    let (width, height) = read_png_dimensions(&image.data);
    assert!(width <= 2000, "width {width}");
    assert!(height <= 2000, "height {height}");

    let note = &normalized[1];
    assert_eq!(block_type(note), "text");
    let UserContentBlock::Text(note) = note else {
        unreachable!()
    };
    assert!(note.text.contains("original 2400x4800"), "{}", note.text);
}

#[test]
fn leaves_oversized_images_alone_when_auto_resize_is_disabled() {
    let content: Vec<ToolResultContent> = vec![image_block(&create_png(2400, 4800), "image/png")];

    let normalized = normalize_tool_result_images(
        &content,
        Some(NormalizeToolResultImagesOptions {
            auto_resize_images: Some(false),
            resize_options: None,
        }),
    );

    assert!(is_borrowed(&normalized));
}

#[test]
fn converts_unsupported_image_formats_even_when_auto_resize_is_disabled() {
    let content: Vec<ToolResultContent> =
        vec![image_block(&create_tiny_bmp_1x1_red_24bpp(), "image/bmp")];

    let normalized = normalize_tool_result_images(
        &content,
        Some(NormalizeToolResultImagesOptions {
            auto_resize_images: Some(false),
            resize_options: None,
        }),
    );

    assert!(!is_borrowed(&normalized));
    let UserContentBlock::Image(image) = &normalized[0] else {
        panic!("expected an image block");
    };
    assert_eq!(image.mime_type, "image/png");
    let UserContentBlock::Text(hint) = &normalized[1] else {
        panic!("expected a text hint");
    };
    assert_eq!(hint.text, "[Image converted from image/bmp to image/png.]");
}

#[test]
fn keeps_undecodable_images_instead_of_dropping_tool_output() {
    use base64::Engine as _;
    let content: Vec<ToolResultContent> = vec![UserContentBlock::Image(ImageContent {
        data: base64::engine::general_purpose::STANDARD.encode(b"not-an-image"),
        mime_type: "image/png".to_string(),
        ..Default::default()
    })];

    let normalized = normalize_tool_result_images(&content, None);
    assert!(is_borrowed(&normalized));
}

#[test]
fn preserves_surrounding_text_blocks_and_their_order() {
    let content: Vec<ToolResultContent> = vec![
        text_block("before"),
        image_block(&create_png(2400, 100), "image/png"),
        text_block("after"),
    ];

    let normalized = normalize_tool_result_images(&content, None);
    let types: Vec<&str> = normalized.iter().map(block_type).collect();
    assert_eq!(types, ["text", "image", "text", "text"]);

    let UserContentBlock::Text(first) = &normalized[0] else {
        panic!("first block");
    };
    assert_eq!(first.text, "before");
    let UserContentBlock::Text(last) = &normalized[3] else {
        panic!("last block");
    };
    assert_eq!(last.text, "after");
}

#[test]
fn uses_model_resize_options_when_provided() {
    use kiana_ai::ModelImageResizeOptions;

    // 2400x100 的图，模型配置把宽度限制到 100：结果尺寸必须遵守该配置。
    let content: Vec<ToolResultContent> = vec![image_block(&create_png(2400, 100), "image/png")];
    let normalized = normalize_tool_result_images(
        &content,
        Some(NormalizeToolResultImagesOptions {
            auto_resize_images: Some(true),
            resize_options: Some(ModelImageResizeOptions {
                max_width: Some(100),
                ..Default::default()
            }),
        }),
    );

    assert!(!is_borrowed(&normalized));
    let UserContentBlock::Image(image) = &normalized[0] else {
        panic!("expected an image block");
    };
    let (width, height) = read_png_dimensions(&image.data);
    assert!(width <= 100, "width {width}");
    assert!(height <= 2000, "height {height}");
}
