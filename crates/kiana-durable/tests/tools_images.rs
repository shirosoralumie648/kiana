//! `packages/durable/test/tools-images.test.ts` 的移植：图片读取、EXIF 方向、尺寸探测与
//! `createNodePhotonImages`。
//!
//! 与 TS 的差异只在测试手段：
//! - TS 用 Photon 的 wasm 生成/解码测试图片；Rust 用同一个 Photon 源码的 `photon-rs`（仅在
//!   `images` feature 下链接）。不需要解码的用例（限额、尺寸表头、EXIF）用最小字节构造。
//! - TS 的 `worker` 线程与事件循环停顿在原生实现里不存在（`ARCHITECTURE.md` §8 第 20 条），
//!   相关断言改为“两种选项给出相同结果”。
//! - `toBase64` 的堆占用测试是 JS 运行时的行为；这里只验证每个余数长度与 3 MiB 输入的正确性。

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use kiana_ai::{
    Model, ModelImageInputLimits, ModelImageResizeOptions, ModelInputLimits, ModelInputModality,
};
use kiana_durable::BoxError;
use kiana_durable::env::{BinaryReader, ExecutionEnv, FileError, FileSystem, LineRange, LineScan};
use kiana_durable::harness::types::ToolExecutionResult;
use kiana_durable::images::exif::exif_orientation;
use kiana_durable::tools::image::{
    ImageProcessor, detect_supported_image_mime_type, image_dimensions, to_base64,
};
use kiana_durable::tools::read::{ReadToolOptions, create_read_tool};
use serde_json::json;

#[path = "support/tools_env.rs"]
mod tools_support;

use tools_support::{FakeApi, ReaderHook, TempDir, TestEnv, diagnostic_text};

fn background() -> kiana_chord::context::ContextRef {
    kiana_chord::context::BACKGROUND_CONTEXT.clone()
}

fn fixture_text(name: &str) -> Option<serde_json::Value> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// TS `tools-images.test.ts` / 参考脚本生成的用例表（detect/dimension/exif/prepare/orientation）。
fn images_fixture() -> serde_json::Value {
    fixture_text("tools_images_cases.json").expect("tools_images_cases.json")
}
// ---------------------------------------------------------------------------
// 字节构造（不需要解码器的最小图片）
// ---------------------------------------------------------------------------

/// 签名 + IHDR + 一个 `payload` 字节的 IDAT 头：足够做格式探测、宽度/高度读取与限额判断。
fn png(width: u32, height: u32, payload: usize) -> Vec<u8> {
    let mut bytes = vec![0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
    bytes.extend_from_slice(&13u32.to_be_bytes());
    bytes.extend_from_slice(b"IHDR");
    bytes.extend_from_slice(&width.to_be_bytes());
    bytes.extend_from_slice(&height.to_be_bytes());
    bytes.extend_from_slice(&[8, 6, 0, 0, 0]);
    bytes.extend_from_slice(&[0, 0, 0, 0]);
    bytes.extend_from_slice(&(payload as u32).to_be_bytes());
    bytes.extend_from_slice(b"IDAT");
    bytes.extend(std::iter::repeat_n(0u8, payload));
    bytes.extend_from_slice(&[0, 0, 0, 0]);
    bytes
}

fn base64_decode(data: &str) -> Vec<u8> {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut lookup = [255u8; 256];
    for (index, byte) in ALPHABET.iter().enumerate() {
        lookup[*byte as usize] = index as u8;
    }
    let mut output = Vec::new();
    let mut buffer: u32 = 0;
    let mut bits = 0;
    for byte in data.bytes() {
        if byte == b'=' {
            break;
        }
        let Some(&value) = lookup.get(byte as usize) else {
            continue;
        };
        if value == 255 {
            continue;
        }
        buffer = (buffer << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            output.push((buffer >> bits) as u8);
        }
    }
    output
}

/// 1x1 GIF，把逻辑屏幕尺寸改成 `width`x`height`。
fn gif(width: u16, height: u16) -> Vec<u8> {
    let mut bytes = base64_decode("R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7");
    bytes[6..8].copy_from_slice(&width.to_le_bytes());
    bytes[8..10].copy_from_slice(&height.to_le_bytes());
    bytes
}

/// 24 位 BMP，provider 不能内联。
fn bmp(width: u32, height: u32) -> Vec<u8> {
    let row = (width as usize * 3).div_ceil(4) * 4;
    let pixel_bytes = row * height as usize;
    let mut bytes = vec![0u8; 54 + pixel_bytes];
    let file_length = bytes.len() as u32;
    bytes[0] = 0x42;
    bytes[1] = 0x4d;
    bytes[2..6].copy_from_slice(&file_length.to_le_bytes());
    bytes[10..14].copy_from_slice(&54u32.to_le_bytes());
    bytes[14..18].copy_from_slice(&40u32.to_le_bytes());
    bytes[18..22].copy_from_slice(&(width as i32).to_le_bytes());
    bytes[22..26].copy_from_slice(&(height as i32).to_le_bytes());
    bytes[26..28].copy_from_slice(&1u16.to_le_bytes());
    bytes[28..30].copy_from_slice(&24u16.to_le_bytes());
    bytes[34..38].copy_from_slice(&(pixel_bytes as u32).to_le_bytes());
    for y in 0..height as usize {
        for x in 0..width as usize {
            let at = 54 + y * row + x * 3;
            bytes[at..at + 3].copy_from_slice(&[0, 0, 255]);
        }
    }
    bytes
}

/// 最小 JPEG：SOI + APP0 + SOF0，声明 `width`x`height`（不需要能解码）。
fn jpeg_with_dimensions(width: u16, height: u16) -> Vec<u8> {
    let mut bytes = vec![0xff, 0xd8];
    // APP0（JFIF），长度 16。
    bytes.extend_from_slice(&[0xff, 0xe0, 0x00, 0x10]);
    bytes.extend_from_slice(&[0x4a, 0x46, 0x49, 0x46, 0, 1, 1, 0, 0, 1, 0, 1, 0, 0]);
    // SOF0，长度 17：精度、高、宽、3 个分量。
    bytes.extend_from_slice(&[0xff, 0xc0, 0x00, 0x11, 0x08]);
    bytes.extend_from_slice(&height.to_be_bytes());
    bytes.extend_from_slice(&width.to_be_bytes());
    bytes.extend_from_slice(&[0x03, 1, 0x22, 0, 2, 0x11, 1, 3, 0x11, 1]);
    bytes
}

/// 一个“只有 EXIF 方向”的 TIFF 头。
fn tiff(orientation: u16, little_endian: bool) -> Vec<u8> {
    let mut bytes = vec![0u8; 26];
    let write_u16 = |bytes: &mut Vec<u8>, at: usize, value: u16| {
        let raw = if little_endian {
            value.to_le_bytes()
        } else {
            value.to_be_bytes()
        };
        bytes[at..at + 2].copy_from_slice(&raw);
    };
    let write_u32 = |bytes: &mut Vec<u8>, at: usize, value: u32| {
        let raw = if little_endian {
            value.to_le_bytes()
        } else {
            value.to_be_bytes()
        };
        bytes[at..at + 4].copy_from_slice(&raw);
    };
    if little_endian {
        bytes[0] = 0x49;
        bytes[1] = 0x49;
    } else {
        bytes[0] = 0x4d;
        bytes[1] = 0x4d;
    }
    write_u16(&mut bytes, 2, 42);
    write_u32(&mut bytes, 4, 8);
    write_u16(&mut bytes, 8, 1);
    write_u16(&mut bytes, 10, 0x0112);
    write_u16(&mut bytes, 12, 3);
    write_u32(&mut bytes, 14, 1);
    write_u16(&mut bytes, 18, orientation);
    bytes
}

const EXIF_HEADER: [u8; 6] = [0x45, 0x78, 0x69, 0x66, 0, 0];

/// `jpeg` 前插入一段 APP1（可选先有一段 APP0），EXIF 方向为 `orientation`。
fn with_orientation(jpeg: &[u8], orientation: u16, little: bool, app0: bool) -> Vec<u8> {
    let mut exif = EXIF_HEADER.to_vec();
    exif.extend(tiff(orientation, little));
    let length = (exif.len() + 2) as u16;
    let mut app1 = vec![0xff, 0xe1];
    app1.extend_from_slice(&length.to_be_bytes());
    app1.extend(exif);
    let app0_bytes: [u8; 18] = [
        0xff, 0xe0, 0, 16, 0x4a, 0x46, 0x49, 0x46, 0, 1, 1, 0, 0, 1, 0, 1, 0, 0,
    ];
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&jpeg[..2]);
    if app0 {
        bytes.extend_from_slice(&app0_bytes);
    }
    bytes.extend(app1);
    bytes.extend_from_slice(&jpeg[2..]);
    bytes
}

/// RIFF WebP：一个 dummy `VP8 ` chunk 与一个 `EXIF` chunk（TIFF 数据裸放或带 `Exif\0\0` 前缀）。
fn webp_with_orientation(orientation: u16, prefixed: bool) -> Vec<u8> {
    fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(kind);
        bytes.extend_from_slice(&(data.len() as u32).to_le_bytes());
        bytes.extend_from_slice(data);
        if data.len() % 2 == 1 {
            bytes.push(0);
        }
        bytes
    }
    let mut exif = Vec::new();
    if prefixed {
        exif.extend_from_slice(&EXIF_HEADER);
    }
    exif.extend(tiff(orientation, true));
    let mut body = b"WEBP".to_vec();
    body.extend(chunk(b"VP8 ", &[1, 2, 3]));
    body.extend(chunk(b"EXIF", &exif));
    let mut bytes = b"RIFF".to_vec();
    bytes.extend_from_slice(&(body.len() as u32).to_le_bytes());
    bytes.extend(body);
    bytes
}

fn has_orientation_webp(orientation: u16, prefixed: bool) -> u8 {
    exif_orientation(&webp_with_orientation(orientation, prefixed))
}

// ---------------------------------------------------------------------------
// 读取执行环境：记录每次定位读取（以及读之后的回调）
// ---------------------------------------------------------------------------

#[derive(Default)]
struct ReadRecorder {
    largest: AtomicUsize,
}

impl ReadRecorder {
    fn largest(&self) -> usize {
        self.largest.load(Ordering::SeqCst)
    }
}

struct HookedReader {
    inner: Arc<dyn BinaryReader>,
    recorder: Arc<ReadRecorder>,
    on_read: Option<Arc<dyn Fn(u64, usize) + Send + Sync>>,
}

#[async_trait]
impl BinaryReader for HookedReader {
    async fn info(
        &self,
        context: &dyn kiana_chord::context::Context,
    ) -> Result<kiana_durable::env::FileInfo, FileError> {
        self.inner.info(context).await
    }

    async fn read(
        &self,
        offset: u64,
        length: usize,
        context: &dyn kiana_chord::context::Context,
    ) -> Result<Vec<u8>, FileError> {
        self.recorder.largest.fetch_max(length, Ordering::SeqCst);
        let result = self.inner.read(offset, length, context).await;
        if let Some(on_read) = &self.on_read {
            on_read(offset, length);
        }
        result
    }

    async fn scan_lines(
        &self,
        range: LineRange,
        context: &dyn kiana_chord::context::Context,
    ) -> Result<LineScan, FileError> {
        self.inner.scan_lines(range, context).await
    }

    async fn close(&self, context: &dyn kiana_chord::context::Context) {
        self.inner.close(context).await
    }
}

fn read_env(
    dir: &TempDir,
    on_read: Option<Arc<dyn Fn(u64, usize) + Send + Sync>>,
) -> (Arc<TestEnv>, Arc<ReadRecorder>) {
    let recorder = Arc::new(ReadRecorder::default());
    let rec = Arc::clone(&recorder);
    let hook: ReaderHook = Arc::new(move |inner, path, options, context| {
        let rec = Arc::clone(&rec);
        let on_read = on_read.clone();
        Box::pin(async move {
            let reader = inner
                .open_binary_reader(&path, options.as_ref(), context)
                .await?;
            Ok(Arc::new(HookedReader {
                inner: reader,
                recorder: rec,
                on_read,
            }) as Arc<dyn BinaryReader>)
        })
    });
    let env = Arc::new(TestEnv::new(dir.path()).with_reader_hook(hook));
    (env, recorder)
}

// ---------------------------------------------------------------------------
// read 的调用器
// ---------------------------------------------------------------------------

fn image_model(input: Vec<ModelInputModality>, resize: Option<ModelImageResizeOptions>) -> Model {
    let input_limits = resize.map(|resize| ModelInputLimits {
        max_request_bytes: None,
        images: Some(ModelImageInputLimits {
            resize: Some(resize),
            max_per_message: None,
            max_per_request: None,
        }),
    });
    Model {
        input,
        input_limits,
        ..Model::default()
    }
}

fn text_and_image() -> Model {
    image_model(
        vec![ModelInputModality::Text, ModelInputModality::Image],
        None,
    )
}

async fn read(
    dir: &TempDir,
    name: &str,
    bytes: &[u8],
    images: Option<Arc<dyn ImageProcessor>>,
    model: Model,
    env: Option<Arc<TestEnv>>,
) -> (Result<ToolExecutionResult, BoxError>, Arc<FakeApi>) {
    std::fs::write(format!("{}/{}", dir.path(), name), bytes).expect("write image");
    let env: Arc<dyn ExecutionEnv> = match env {
        Some(env) => env,
        None => Arc::new(TestEnv::new(dir.path())),
    };
    let api = FakeApi::with_model(Some(env), model);
    let tool = create_read_tool(ReadToolOptions { images });
    let execute = tool.execute.as_ref().expect("execute");
    let result = execute(json!({ "path": name }), api.clone(), background()).await;
    (result, api)
}

async fn read_ok(
    dir: &TempDir,
    name: &str,
    bytes: &[u8],
    images: Option<Arc<dyn ImageProcessor>>,
    model: Model,
    env: Option<Arc<TestEnv>>,
) -> (ToolExecutionResult, Arc<FakeApi>) {
    let (result, api) = read(dir, name, bytes, images, model, env).await;
    (result.expect("the read returns a result"), api)
}

fn only_image(result: &ToolExecutionResult) -> &kiana_ai::ImageContent {
    match result.output.as_deref() {
        Some([kiana_ai::UserContentBlock::Image(image)]) => image,
        other => panic!("expected one image block, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// 没有图片处理器时的 read
// ---------------------------------------------------------------------------

#[tokio::test]
async fn returns_a_supported_image_within_the_limits_as_it_is() {
    let dir = TempDir::new();
    let image = png(8, 6, 4);
    let (result, api) = read_ok(&dir, "a.png", &image, None, text_and_image(), None).await;
    let block = only_image(&result);
    assert_eq!(block.data, to_base64(&image));
    assert_eq!(block.mime_type, "image/png");
    assert_eq!(diagnostic_text(&result), "Read image file [image/png].");
    assert_eq!(api.streamed(), "");

    let tiny_gif = gif(1, 1);
    let (tiny, _api) = read_ok(&dir, "a.gif", &tiny_gif, None, text_and_image(), None).await;
    let block = only_image(&tiny);
    assert_eq!(block.data, to_base64(&tiny_gif));
    assert_eq!(block.mime_type, "image/gif");
}

#[tokio::test]
async fn refuses_an_image_too_large_to_send_without_reading_it() {
    let dir = TempDir::new();
    let big = png(1100, 1100, 3_540_000);
    let (env, reads) = read_env(&dir, None);
    let (result, _api) = read_ok(&dir, "big.png", &big, None, text_and_image(), Some(env)).await;
    assert!(matches!(result.output.as_deref(), Some([])));
    assert_eq!(result.is_error, Some(true));
    let message = diagnostic_text(&result);
    assert!(
        message.starts_with("big.png is an image (image/png) of ")
            && message.contains(", too large to send"),
        "{message}"
    );
    // 只有头部与 chunk 遍历，单次读取不超过 64 KiB。
    assert!(
        reads.largest() <= 64 * 1024,
        "largest read was {}",
        reads.largest()
    );
}

#[tokio::test]
async fn refuses_an_image_wider_or_taller_than_the_limits_by_its_header() {
    let dir = TempDir::new();
    let (wide, _api) = read_ok(
        &dir,
        "wide.png",
        &png(2001, 2, 4),
        None,
        text_and_image(),
        None,
    )
    .await;
    assert!(matches!(wide.output.as_deref(), Some([])));
    assert_eq!(wide.is_error, Some(true));
    assert_eq!(
        diagnostic_text(&wide),
        "wide.png is an image (image/png) of 2001x2, larger than 2000x2000, and no image processor is configured to shrink it"
    );

    let (tall, _api) = read_ok(
        &dir,
        "tall.gif",
        &gif(1, 3000),
        None,
        text_and_image(),
        None,
    )
    .await;
    assert!(
        diagnostic_text(&tall)
            .starts_with("tall.gif is an image (image/gif) of 1x3000, larger than 2000x2000,"),
        "{}",
        diagnostic_text(&tall)
    );
}

#[tokio::test]
async fn refuses_a_format_that_needs_converting() {
    let dir = TempDir::new();
    let (result, _api) = read_ok(&dir, "a.bmp", &bmp(4, 4), None, text_and_image(), None).await;
    assert!(matches!(result.output.as_deref(), Some([])));
    assert_eq!(result.is_error, Some(true));
    assert_eq!(
        diagnostic_text(&result),
        "a.bmp is an image (image/bmp) that needs converting, and no image processor is configured"
    );
}

#[tokio::test]
async fn says_when_the_model_sees_a_placeholder_instead_of_the_image() {
    let dir = TempDir::new();
    let (result, _api) = read_ok(
        &dir,
        "a.png",
        &png(4, 4, 4),
        None,
        image_model(vec![ModelInputModality::Text], None),
        None,
    )
    .await;
    assert_eq!(result.output.as_ref().map(Vec::len), Some(1));
    assert_eq!(
        diagnostic_text(&result),
        "Read image file [image/png]. The current model does not support images; it sees a placeholder instead."
    );
}

#[tokio::test]
async fn applies_the_models_image_limits_instead_of_the_defaults() {
    let dir = TempDir::new();
    let png_bytes = png(8, 6, 4);
    let (refused, _api) = read_ok(
        &dir,
        "a.png",
        &png_bytes,
        None,
        image_model(
            vec![ModelInputModality::Text, ModelInputModality::Image],
            Some(ModelImageResizeOptions {
                max_width: Some(4),
                max_height: None,
                max_bytes: None,
                jpeg_quality: None,
            }),
        ),
        None,
    )
    .await;
    assert!(
        diagnostic_text(&refused).contains("of 8x6, larger than 4x2000,"),
        "{}",
        diagnostic_text(&refused)
    );
}

#[tokio::test]
async fn reads_an_image_that_grows_while_it_is_read_again_rather_than_returning_a_cut_off_image() {
    let dir = TempDir::new();
    let image = png(8, 6, 4);
    let path = format!("{}/growing.png", dir.path());
    let image_length = image.len();
    let whole_reads: Arc<std::sync::Mutex<Vec<usize>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorded = Arc::clone(&whole_reads);
    let callback_path = path.clone();
    let callback: Arc<dyn Fn(u64, usize) + Send + Sync> = Arc::new(move |offset, length| {
        if offset != 0 || length < image_length {
            return;
        }
        recorded.lock().unwrap().push(length);
        if recorded.lock().unwrap().len() == 1 {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&callback_path)
                .expect("open growing.png");
            file.write_all(&[0u8; 16]).expect("append");
        }
    });
    let (env, _recorder) = read_env(&dir, Some(callback));
    let (result, _api) = read_ok(
        &dir,
        "growing.png",
        &image,
        None,
        text_and_image(),
        Some(env),
    )
    .await;
    assert_eq!(
        *whole_reads.lock().unwrap(),
        vec![image.len(), image.len() + 16]
    );
    let mut after = image.clone();
    after.extend_from_slice(&[0u8; 16]);
    assert_eq!(&only_image(&result).data, &to_base64(&after));
}

#[tokio::test]
async fn fails_when_the_image_changes_during_the_second_read_too() {
    let dir = TempDir::new();
    let image = png(8, 6, 4);
    let path = format!("{}/busy.png", dir.path());
    let image_length = image.len();
    let callback: Arc<dyn Fn(u64, usize) + Send + Sync> = Arc::new(move |offset, length| {
        if offset != 0 || length < image_length {
            return;
        }
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("open busy.png");
        file.write_all(&[0u8]).expect("append");
    });
    let (env, _recorder) = read_env(&dir, Some(callback));
    let (result, _api) = read(&dir, "busy.png", &image, None, text_and_image(), Some(env)).await;
    let error = result.expect_err("the image changes on every read");
    assert_eq!(error.to_string(), "busy.png changed while it was read");
}

#[test]
fn lists_bmp_in_the_description_only_with_a_processor() {
    assert!(
        create_read_tool(ReadToolOptions::default())
            .tool
            .description
            .contains("(jpg, png, gif, webp)")
    );
}

// ---------------------------------------------------------------------------
// EXIF 方向（不需要解码器）
// ---------------------------------------------------------------------------

#[test]
fn reads_big_and_little_endian_jpeg_exif_after_an_app0_segment_and_1_when_absent_or_out_of_range() {
    let jpeg = jpeg_with_dimensions(2, 2);
    assert_eq!(
        exif_orientation(&with_orientation(&jpeg, 6, false, false)),
        6
    );
    assert_eq!(
        exif_orientation(&with_orientation(&jpeg, 8, true, false)),
        8
    );
    assert_eq!(
        exif_orientation(&with_orientation(&jpeg, 5, false, true)),
        5
    );
    assert_eq!(
        exif_orientation(&with_orientation(&jpeg, 9, false, false)),
        1
    );
    assert_eq!(exif_orientation(&jpeg), 1);
    assert_eq!(exif_orientation(&png(2, 2, 0)), 1);
}

#[test]
fn reads_webp_exif_chunks_with_and_without_the_exif_prefix() {
    assert_eq!(has_orientation_webp(6, false), 6);
    assert_eq!(has_orientation_webp(3, true), 3);
}

// ---------------------------------------------------------------------------
// imageDimensions（不需要解码器）
// ---------------------------------------------------------------------------

#[test]
fn reads_the_size_each_format_declares_in_its_header() {
    assert_eq!(
        image_dimensions(&png(7, 5, 0), "image/png"),
        Some(kiana_durable::tools::image::ImageDimensions {
            width: 7,
            height: 5
        })
    );
    assert_eq!(
        image_dimensions(&jpeg_with_dimensions(7, 5), "image/jpeg"),
        Some(kiana_durable::tools::image::ImageDimensions {
            width: 7,
            height: 5
        })
    );
    assert_eq!(
        image_dimensions(
            &with_orientation(&jpeg_with_dimensions(7, 5), 6, false, true),
            "image/jpeg"
        ),
        Some(kiana_durable::tools::image::ImageDimensions {
            width: 7,
            height: 5
        })
    );
    assert_eq!(
        image_dimensions(&gif(300, 200), "image/gif"),
        Some(kiana_durable::tools::image::ImageDimensions {
            width: 300,
            height: 200
        })
    );
    fn webp(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut bytes = b"RIFF".to_vec();
        bytes.extend_from_slice(&[0, 0, 0, 0]);
        bytes.extend_from_slice(b"WEBP");
        bytes.extend_from_slice(kind);
        bytes.extend_from_slice(&[0, 0, 0, 0]);
        bytes.extend_from_slice(data);
        bytes.extend(std::iter::repeat_n(0u8, 16));
        bytes
    }
    // VP8：frame tag、起始码，然后 14 位的宽和高。
    assert_eq!(
        image_dimensions(
            &webp(
                b"VP8 ",
                &[0, 0, 0, 0x9d, 0x01, 0x2a, 0x2c, 0x01, 0xc8, 0x00]
            ),
            "image/webp"
        ),
        Some(kiana_durable::tools::image::ImageDimensions {
            width: 300,
            height: 200
        })
    );
    // VP8L：签名，然后 14 位的 width - 1 与 height - 1。
    let bits: u32 = 299 | (199 << 14);
    assert_eq!(
        image_dimensions(
            &webp(
                b"VP8L",
                &[
                    0x2f,
                    bits as u8,
                    (bits >> 8) as u8,
                    (bits >> 16) as u8,
                    (bits >> 24) as u8,
                ]
            ),
            "image/webp"
        ),
        Some(kiana_durable::tools::image::ImageDimensions {
            width: 300,
            height: 200
        })
    );
    // VP8X：flags，然后 24 位的 width - 1 与 height - 1。
    assert_eq!(
        image_dimensions(
            &webp(b"VP8X", &[0, 0, 0, 0, 0x2b, 0x01, 0, 0xc7, 0, 0]),
            "image/webp"
        ),
        Some(kiana_durable::tools::image::ImageDimensions {
            width: 300,
            height: 200
        })
    );
    assert_eq!(image_dimensions(&[0xff, 0xd8, 0xff], "image/jpeg"), None);
}

// ---------------------------------------------------------------------------
// TS 参考用例表：格式探测 / 尺寸 / EXIF 方向（不需要解码器）
// ---------------------------------------------------------------------------

fn case_bytes(case: &serde_json::Value) -> Vec<u8> {
    base64_decode(case["bytes_b64"].as_str().expect("bytes_b64"))
}

/// `tests/fixtures/tools_images_cases.json` 的 `detect_cases`（由 TS
/// `detectSupportedImageMimeType` 生成，含空文件、文本、扩展 JPEG 标记与 BMP）。
#[test]
fn detects_the_mime_types_of_the_reference_cases() {
    let fixture = images_fixture();
    let cases = fixture["detect_cases"].as_array().expect("detect_cases");
    assert_eq!(cases.len(), 9);
    for case in cases {
        let name = case["name"].as_str().expect("name");
        assert_eq!(
            detect_supported_image_mime_type(&case_bytes(case)),
            case["expected"].as_str().map(str::to_owned),
            "{name}"
        );
    }
}

/// `dimension_cases`：每种格式声明在头部里的宽高；截断的头返回 `undefined`（`None`）。
#[test]
fn reads_the_dimensions_of_the_reference_cases() {
    let fixture = images_fixture();
    let cases = fixture["dimension_cases"]
        .as_array()
        .expect("dimension_cases");
    assert_eq!(cases.len(), 9);
    for case in cases {
        let name = case["name"].as_str().expect("name");
        let actual = image_dimensions(
            &case_bytes(case),
            case["mimeType"].as_str().expect("mimeType"),
        );
        let expected = &case["expected"];
        if expected.is_null() {
            assert_eq!(actual, None, "{name}");
        } else {
            let actual = actual.unwrap_or_else(|| panic!("{name}: expected dimensions"));
            assert_eq!(
                json!({ "width": actual.width, "height": actual.height }),
                *expected,
                "{name}"
            );
        }
    }
}

/// `exif_cases`：大端/小端 TIFF、APP0 之后的 APP1、XMP 之后的 EXIF（合法与长度声明错误的
/// APP1）、裸 WebP/带 `Exif\0\0` 前缀的 WebP，以及越界/截断时回退到 1。
#[test]
fn reads_the_exif_orientation_of_the_reference_cases() {
    let fixture = images_fixture();
    let cases = fixture["exif_cases"].as_array().expect("exif_cases");
    assert_eq!(cases.len(), 11);
    for case in cases {
        let name = case["name"].as_str().expect("name");
        assert_eq!(
            exif_orientation(&case_bytes(case)),
            case["expected"].as_u64().expect("expected") as u8,
            "{name}"
        );
    }
}

// ---------------------------------------------------------------------------
// toBase64
// ---------------------------------------------------------------------------

fn reference_base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::new();
    for chunk in bytes.chunks(3) {
        let first = u32::from(chunk[0]);
        let second = u32::from(chunk.get(1).copied().unwrap_or(0));
        let third = u32::from(chunk.get(2).copied().unwrap_or(0));
        let value = (first << 16) | (second << 8) | third;
        output.push(ALPHABET[(value >> 18) as usize & 0x3f] as char);
        output.push(ALPHABET[(value >> 12) as usize & 0x3f] as char);
        if chunk.len() > 1 {
            output.push(ALPHABET[(value >> 6) as usize & 0x3f] as char);
        } else {
            output.push('=');
        }
        if chunk.len() > 2 {
            output.push(ALPHABET[value as usize & 0x3f] as char);
        } else {
            output.push('=');
        }
    }
    output
}

#[test]
fn matches_a_reference_encoder_for_every_remainder_length() {
    for length in [0usize, 1, 2, 3, 4, 5, 8191, 8192, 8193, 10_000, 16_385] {
        let bytes: Vec<u8> = (0..length).map(|index| (index * 37) as u8).collect();
        assert_eq!(
            to_base64(&bytes),
            reference_base64(&bytes),
            "length {length}"
        );
    }
}

#[test]
fn encodes_a_3_mib_image() {
    let bytes: Vec<u8> = (0..3_147_295usize)
        .map(|index| (index * 37) as u8)
        .collect();
    assert_eq!(to_base64(&bytes), reference_base64(&bytes));
}

// ---------------------------------------------------------------------------
// 带 Photon 图片处理器（`images` feature）时的 read 与 createNodePhotonImages
// ---------------------------------------------------------------------------

#[cfg(feature = "images")]
mod with_processor {
    use super::*;

    use kiana_durable::images::node::{
        NodePhotonImagesOptions, create_node_photon_images, create_photon_images,
    };
    use kiana_durable::tools::image::{DEFAULT_IMAGE_LIMITS, ImageLimits, ResizedDimensions};
    use photon_rs::PhotonImage;
    use sha2::{Digest, Sha256};

    type Rgb = [u8; 3];

    fn processor() -> Arc<dyn ImageProcessor> {
        Arc::new(create_photon_images())
    }

    /// 一张 `width`x`height` 的图：红色，左上角一个绿色像素。
    fn pixels(width: u32, height: u32) -> Vec<u8> {
        let mut rgba = vec![0u8; width as usize * height as usize * 4];
        for index in 0..(width as usize * height as usize) {
            rgba[index * 4..index * 4 + 4].copy_from_slice(&[255, 0, 0, 255]);
        }
        rgba[0..4].copy_from_slice(&[0, 255, 0, 255]);
        rgba
    }

    fn encode_rgba(rgba: Vec<u8>, width: u32, height: u32, format: &str) -> Vec<u8> {
        let image = PhotonImage::new(rgba, width, height);
        if format == "png" {
            image.get_bytes()
        } else {
            image.get_bytes_jpeg(95)
        }
    }

    fn encode(width: u32, height: u32, format: &str) -> Vec<u8> {
        encode_rgba(pixels(width, height), width, height, format)
    }

    /// 噪声：PNG 与 JPEG 都压不动，让字节上限决定。
    fn noisy_png(width: u32, height: u32) -> Vec<u8> {
        let mut rgba = vec![0u8; width as usize * height as usize * 4];
        let mut seed: u32 = 1;
        for (index, byte) in rgba.iter_mut().enumerate() {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            *byte = if index % 4 == 3 {
                255
            } else {
                (seed >> 24) as u8
            };
        }
        encode_rgba(rgba, width, height, "png")
    }

    fn decoded_image(data: &str) -> (u32, u32, Vec<u8>) {
        let image = PhotonImage::new_from_byteslice(base64_decode(data));
        (
            image.get_width(),
            image.get_height(),
            image.get_raw_pixels(),
        )
    }

    #[tokio::test]
    async fn passes_an_image_that_needs_no_change_through_untouched() {
        let dir = TempDir::new();
        let image = encode(8, 6, "png");
        let (result, _api) = read_ok(
            &dir,
            "a.png",
            &image,
            Some(processor()),
            text_and_image(),
            None,
        )
        .await;
        let block = only_image(&result);
        assert_eq!(block.data, to_base64(&image));
        assert_eq!(block.mime_type, "image/png");
        assert_eq!(diagnostic_text(&result), "Read image file [image/png].");

        let (tiny, _api) = read_ok(
            &dir,
            "a.gif",
            &gif(1, 1),
            Some(processor()),
            text_and_image(),
            None,
        )
        .await;
        assert_eq!(only_image(&tiny).mime_type, "image/gif");
    }

    #[tokio::test]
    async fn scales_an_image_down_to_the_maximum_size_and_says_how_to_map_coordinates_back() {
        let dir = TempDir::new();
        let (result, _api) = read_ok(
            &dir,
            "wide.png",
            &encode(4000, 1000, "png"),
            Some(processor()),
            text_and_image(),
            None,
        )
        .await;
        let block = only_image(&result);
        let (width, height, _rgba) = decoded_image(&block.data);
        assert_eq!((width, height), (2000, 500));
        assert_eq!(
            diagnostic_text(&result),
            format!(
                "Read image file [{}]. Resized from 4000x1000 to 2000x500. Multiply coordinates by 2.00 to map them to the original.",
                block.mime_type
            )
        );
    }

    #[tokio::test]
    async fn re_encodes_a_png_too_large_for_the_byte_limit_as_jpeg_at_its_size_and_shrinks_it_when_that_is_not_enough()
     {
        let dir = TempDir::new();
        let (result, _api) = read_ok(
            &dir,
            "big.png",
            &noisy_png(1100, 1100),
            Some(processor()),
            text_and_image(),
            None,
        )
        .await;
        let jpeg = only_image(&result);
        assert_eq!(jpeg.mime_type, "image/jpeg");
        assert_eq!(
            diagnostic_text(&result),
            "Read image file [image/jpeg]. Converted from image/png to image/jpeg."
        );
        assert!(
            jpeg.data.len() <= DEFAULT_IMAGE_LIMITS.max_bytes,
            "{} > {}",
            jpeg.data.len(),
            DEFAULT_IMAGE_LIMITS.max_bytes
        );
        let (width, height, _rgba) = decoded_image(&jpeg.data);
        assert_eq!((width, height), (1100, 1100));

        let tight = ImageLimits {
            max_bytes: 200_000,
            ..DEFAULT_IMAGE_LIMITS
        };
        let shrunk = create_photon_images()
            .prepare(&noisy_png(2000, 2000), "image/png", tight)
            .expect("a shrunk image fits");
        assert!(shrunk.data.len() <= tight.max_bytes);
        let resized = shrunk.resized.expect("resized dimensions");
        assert_eq!(resized.from, (2000, 2000));
        assert!(resized.to.0 < 2000, "{:?}", resized.to);
    }

    #[test]
    fn keeps_at_least_one_pixel_on_each_side_of_an_extreme_aspect_ratio() {
        let result = create_photon_images()
            .prepare(&encode(6000, 2, "png"), "image/png", DEFAULT_IMAGE_LIMITS)
            .expect("prepared");
        assert_eq!(
            result.resized,
            Some(ResizedDimensions {
                from: (6000, 2),
                to: (2000, 1),
            })
        );
    }

    #[test]
    fn gives_up_when_not_even_one_pixel_fits_the_byte_limit() {
        let limits = ImageLimits {
            max_bytes: 8,
            ..DEFAULT_IMAGE_LIMITS
        };
        assert!(
            create_photon_images()
                .prepare(&encode(4, 4, "png"), "image/png", limits)
                .is_none()
        );
    }

    #[test]
    fn keeps_a_jpeg_that_must_be_re_encoded_a_jpeg() {
        let jpeg = with_orientation(&encode(40, 30, "jpeg"), 3, false, false);
        let result = create_photon_images()
            .prepare(&jpeg, "image/jpeg", DEFAULT_IMAGE_LIMITS)
            .expect("prepared");
        assert_eq!(result.mime_type, "image/jpeg");
    }

    #[tokio::test]
    async fn converts_a_format_providers_do_not_take_inline_to_png() {
        let dir = TempDir::new();
        let (result, _api) = read_ok(
            &dir,
            "a.bmp",
            &bmp(4, 4),
            Some(processor()),
            text_and_image(),
            None,
        )
        .await;
        assert_eq!(only_image(&result).mime_type, "image/png");
        assert_eq!(
            diagnostic_text(&result),
            "Read image file [image/png]. Converted from image/bmp to image/png."
        );
    }

    #[tokio::test]
    async fn reports_bytes_it_cannot_decode_as_an_error() {
        let dir = TempDir::new();
        let broken = encode(4, 4, "png")[..40].to_vec();
        let (result, _api) = read_ok(
            &dir,
            "broken.png",
            &broken,
            Some(processor()),
            text_and_image(),
            None,
        )
        .await;
        assert!(matches!(result.output.as_deref(), Some([])));
        assert_eq!(result.is_error, Some(true));
        assert_eq!(
            diagnostic_text(&result),
            "broken.png is an image (image/png) that cannot be prepared for the model"
        );
    }

    #[tokio::test]
    async fn is_what_create_coding_tools_with_images_gives_read() {
        let dir = TempDir::new();
        let extension = kiana_durable::tools::create_coding_tools(ReadToolOptions {
            images: Some(processor()),
        });
        let tool = extension
            .tools()
            .into_iter()
            .find(|candidate| candidate.tool.name == "read")
            .expect("coding tools include read");
        std::fs::write(format!("{}/b.bmp", dir.path()), bmp(4, 4)).expect("write b.bmp");
        let env: Arc<dyn ExecutionEnv> = Arc::new(TestEnv::new(dir.path()));
        let api = FakeApi::with_model(Some(env), text_and_image());
        let execute = tool.execute.as_ref().expect("execute");
        let result = execute(json!({ "path": "b.bmp" }), api, background())
            .await
            .expect("read result");
        assert_eq!(only_image(&result).mime_type, "image/png");
    }

    #[test]
    fn lists_bmp_in_the_description_with_a_processor() {
        assert!(
            create_read_tool(ReadToolOptions {
                images: Some(processor())
            })
            .tool
            .description
            .contains("(jpg, png, gif, webp, bmp)")
        );
    }

    // -- EXIF 方向：把每个方向扶正 ------------------------------------------

    const RED: Rgb = [255, 0, 0];
    const GREEN: Rgb = [0, 255, 0];
    const BLUE: Rgb = [0, 0, 255];
    const WHITE: Rgb = [255, 255, 255];
    const BLACK: Rgb = [0, 0, 0];
    const YELLOW: Rgb = [255, 255, 0];

    fn stored() -> Vec<Vec<Rgb>> {
        vec![vec![RED, GREEN, BLUE], vec![WHITE, BLACK, YELLOW]]
    }

    fn mirror(grid: &[Vec<Rgb>]) -> Vec<Vec<Rgb>> {
        grid.iter()
            .map(|row| row.iter().rev().copied().collect())
            .collect()
    }

    /// 顺时针四分之一圈：左列从下往上成为顶行。
    fn turn(grid: &[Vec<Rgb>]) -> Vec<Vec<Rgb>> {
        let columns = grid[0].len();
        (0..columns)
            .map(|x| grid.iter().map(|row| row[x]).rev().collect())
            .collect()
    }

    fn upright(orientation: u8) -> Vec<Vec<Rgb>> {
        let stored = stored();
        match orientation {
            1 => stored.clone(),
            2 => mirror(&stored),
            3 => turn(&turn(&stored)),
            4 => turn(&turn(&mirror(&stored))),
            5 => turn(&turn(&turn(&mirror(&stored)))),
            6 => turn(&stored),
            7 => turn(&mirror(&stored)),
            _ => turn(&turn(&turn(&stored))),
        }
    }

    fn blocks(colors: &[Vec<Rgb>], block: u32) -> (Vec<u8>, u32, u32) {
        let width = colors[0].len() as u32 * block;
        let height = colors.len() as u32 * block;
        let mut rgba = vec![0u8; width as usize * height as usize * 4];
        for y in 0..height as usize {
            for x in 0..width as usize {
                let color = colors[y / block as usize][x / block as usize];
                let at = (y * width as usize + x) * 4;
                rgba[at..at + 4].copy_from_slice(&[color[0], color[1], color[2], 255]);
            }
        }
        (rgba, width, height)
    }

    #[test]
    fn turns_every_orientation_upright() {
        for orientation in 1..=8u8 {
            let (rgba, width, height) = blocks(&stored(), 16);
            let jpeg = with_orientation(
                &encode_rgba(rgba, width, height, "jpeg"),
                u16::from(orientation),
                false,
                false,
            );
            let prepared = create_photon_images()
                .prepare(&jpeg, "image/jpeg", DEFAULT_IMAGE_LIMITS)
                .unwrap_or_else(|| panic!("orientation {orientation} prepares"));
            let (image_width, image_height, pixels) = decoded_image(&prepared.data);
            let expected = upright(orientation);
            assert_eq!(
                (image_width, image_height),
                (expected[0].len() as u32 * 16, expected.len() as u32 * 16),
                "orientation {orientation}"
            );
            // 每个方块中心的颜色，JPEG 噪声下按整通道取 255/0。
            for (by, row) in expected.iter().enumerate() {
                for (bx, color) in row.iter().enumerate() {
                    let at = ((by * 16 + 8) * image_width as usize + bx * 16 + 8) * 4;
                    let seen: Vec<u8> = pixels[at..at + 3]
                        .iter()
                        .map(|channel| if *channel > 127 { 255 } else { 0 })
                        .collect();
                    assert_eq!(
                        seen,
                        color.to_vec(),
                        "orientation {orientation} block ({bx},{by})"
                    );
                }
            }
        }
    }

    // -- createNodePhotonImages ----------------------------------------------

    /// JPEG 输入（含 EXIF 方向、缩放、字节限额）与 PNG→JPEG 重编码是当前稳定契约，要求与
    /// photon wasm 构建输出**相同的字节**：解码用的是同一份 `image` 0.24 和 wasm 构建里的
    /// `jpeg-decoder` 0.3.1（平台无关路径），方向扶正、缩放与重编码路径也相同，改动需同步
    /// 测试。期望值由参考实现生成（`tests/fixtures/tools_images_exact.json`）。
    #[test]
    fn prepares_images_exactly_like_the_photon_wasm_build() {
        let fixture = fixture_text("tools_images_exact.json").expect("exact image fixture");
        for case in fixture.as_array().expect("cases") {
            let name = case["name"].as_str().unwrap();
            let input = base64_decode(case["input"].as_str().unwrap());
            let mime_type = case["mimeType"].as_str().unwrap();
            let limits = &case["limits"];
            let limits = ImageLimits {
                max_width: limits["maxWidth"].as_u64().unwrap() as u32,
                max_height: limits["maxHeight"].as_u64().unwrap() as u32,
                max_bytes: limits["maxBytes"].as_u64().unwrap() as usize,
            };
            let actual = create_photon_images().prepare(&input, mime_type, limits);
            let expected = &case["result"];
            if expected.is_null() {
                assert!(actual.is_none(), "{name}: expected no prepared image");
                continue;
            }
            let actual = actual.unwrap_or_else(|| panic!("{name}: expected a prepared image"));
            assert_eq!(
                actual.data,
                expected["data"].as_str().unwrap(),
                "{name}: data"
            );
            assert_eq!(
                actual.mime_type,
                expected["mimeType"].as_str().unwrap(),
                "{name}: mimeType"
            );
            let actual_resized = actual.resized.map(|resized| {
                json!({
                    "from": { "width": resized.from.0, "height": resized.from.1 },
                    "to": { "width": resized.to.0, "height": resized.to.1 },
                })
            });
            if case["result"]["resized"].is_null() {
                assert!(
                    actual_resized.is_none(),
                    "{name}: unexpected resized {actual_resized:?}"
                );
            } else {
                assert_eq!(
                    actual_resized.as_ref(),
                    Some(&case["result"]["resized"]),
                    "{name}: resized"
                );
            }
            assert_eq!(
                actual.converted_from,
                expected["convertedFrom"].as_str().map(str::to_owned),
                "{name}: convertedFrom"
            );
        }
    }

    /// `noisyPng` 的确定性重建：`s = (s * 1103515245 + 12345) >>> 0` 由 JS 双精度浮点求值
    /// （乘积会超过 2^53 而被舍入，等价于 f64 乘法后再取模 2^32，**不是** u32 回绕乘法）。
    /// 这样重建的输入与夹具生成的字节一致。
    fn js_noisy_png(width: u32, height: u32, seed: u32) -> Vec<u8> {
        let mut rgba = vec![0u8; width as usize * height as usize * 4];
        let mut state = f64::from(seed);
        for (index, byte) in rgba.iter_mut().enumerate() {
            state = (state * 1_103_515_245.0 + 12_345.0) % 4_294_967_296.0;
            *byte = if index % 4 == 3 {
                255
            } else {
                (state / 16_777_216.0).floor() as u8
            };
        }
        encode_rgba(rgba, width, height, "png")
    }

    fn case_limits(case: &serde_json::Value) -> ImageLimits {
        let limits = &case["limits"];
        ImageLimits {
            max_width: limits["maxWidth"].as_u64().expect("maxWidth") as u32,
            max_height: limits["maxHeight"].as_u64().expect("maxHeight") as u32,
            max_bytes: limits["maxBytes"].as_u64().expect("maxBytes") as usize,
        }
    }

    /// 用例的输入字节：小用例直接存了 base64；大用例只存生成器参数，按 TS 测试的生成器重建
    /// （Photon 的 PNG 编码层在 wasm 与 native 下逐字节相同，重建结果是相同的输入）。
    fn case_input(case: &serde_json::Value) -> Vec<u8> {
        if let Some(encoded) = case["input_b64"].as_str() {
            return base64_decode(encoded);
        }
        let generator = &case["input_generator"];
        let width = generator["width"].as_u64().expect("width") as u32;
        let height = generator["height"].as_u64().expect("height") as u32;
        match generator["kind"].as_str().expect("kind") {
            "pixels" => encode(width, height, "png"),
            "noise" => js_noisy_png(
                width,
                height,
                generator["seed"].as_u64().expect("seed") as u32,
            ),
            other => panic!("unknown input generator {other}"),
        }
    }

    /// `tests/fixtures/tools_images_cases.json` 的 `prepare_cases`：12 个用例的 `data`、
    /// `mimeType`、`resized`、`convertedFrom` 全部与 TS 参考一致；大用例以 base64 长度 +
    /// 原始字节的 sha256 断言。这些用例都逐字节相同（输入可精确重建，Photon 的解码/缩放/编码
    /// 路径在 wasm 与 native 下一致），§8 第 20 条允许的末位舍入差异没有出现。
    #[test]
    fn prepares_the_reference_cases() {
        let fixture = images_fixture();
        let processor = create_photon_images();
        let cases = fixture["prepare_cases"].as_array().expect("prepare_cases");
        assert_eq!(cases.len(), 12);
        for case in cases {
            let name = case["name"].as_str().expect("name");
            let input = case_input(case);
            let actual = processor.prepare(
                &input,
                case["mimeType"].as_str().expect("mimeType"),
                case_limits(case),
            );
            let expected = &case["expected"];
            if expected.is_null() {
                assert!(actual.is_none(), "{name}: expected no prepared image");
                continue;
            }
            let actual = actual.unwrap_or_else(|| panic!("{name}: expected a prepared image"));
            assert_eq!(
                actual.mime_type,
                expected["mimeType"].as_str().expect("mimeType"),
                "{name}: mimeType"
            );
            let resized = actual.resized.map(|resized| {
                json!({
                    "from": { "width": resized.from.0, "height": resized.from.1 },
                    "to": { "width": resized.to.0, "height": resized.to.1 },
                })
            });
            if expected["resized"].is_null() {
                assert!(resized.is_none(), "{name}: unexpected resized {resized:?}");
            } else {
                assert_eq!(
                    resized.as_ref(),
                    Some(&expected["resized"]),
                    "{name}: resized"
                );
            }
            assert_eq!(
                actual.converted_from.as_deref(),
                expected["convertedFrom"].as_str(),
                "{name}: convertedFrom"
            );
            if let Some(data) = expected["data"].as_str() {
                assert_eq!(actual.data, data, "{name}: data");
            } else {
                assert_eq!(
                    actual.data.len(),
                    expected["data_len"].as_u64().expect("data_len") as usize,
                    "{name}: data length"
                );
                let digest = Sha256::digest(base64_decode(&actual.data));
                let hash: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
                assert_eq!(
                    hash,
                    expected["data_sha256"].as_str().expect("data_sha256"),
                    "{name}: data sha256"
                );
            }
        }
    }

    /// `orientation_cases`：8 个方向的输入经 EXIF 扶正后与原图一致，输出 JPEG 与 TS 逐字节相同。
    #[test]
    fn prepares_every_reference_exif_orientation() {
        let fixture = images_fixture();
        let processor = create_photon_images();
        let cases = fixture["orientation_cases"]
            .as_array()
            .expect("orientation_cases");
        assert_eq!(cases.len(), 8);
        for case in cases {
            let orientation = case["orientation"].as_u64().expect("orientation");
            let input = base64_decode(case["input_b64"].as_str().expect("input_b64"));
            let actual = processor
                .prepare(&input, "image/jpeg", DEFAULT_IMAGE_LIMITS)
                .unwrap_or_else(|| panic!("orientation {orientation}: expected a prepared image"));
            assert_eq!(
                actual.data,
                case["expected"]["data"].as_str().expect("data"),
                "orientation {orientation}: data"
            );
            assert_eq!(
                actual.mime_type,
                case["expected"]["mimeType"].as_str().expect("mimeType"),
                "orientation {orientation}: mimeType"
            );
            assert!(actual.resized.is_none(), "orientation {orientation}");
            assert!(actual.converted_from.is_none(), "orientation {orientation}");
        }
    }

    #[test]
    fn gives_the_same_result_as_the_calling_thread_and_keeps_the_callers_bytes() {
        let image = noisy_png(400, 300);
        let copy = image.clone();
        let threaded = create_photon_images().prepare(&image, "image/png", DEFAULT_IMAGE_LIMITS);
        let inline = create_node_photon_images(NodePhotonImagesOptions {
            worker: false,
            idle_ms: 30_000,
        })
        .prepare(&image, "image/png", DEFAULT_IMAGE_LIMITS);
        assert_eq!(threaded, inline);
        assert_eq!(image, copy);
    }

    #[tokio::test]
    async fn prepares_concurrent_images_and_keeps_working_after_the_idle_period() {
        let images = Arc::new(create_node_photon_images(NodePhotonImagesOptions {
            worker: true,
            idle_ms: 10,
        }));
        let small_bmp = bmp(4, 4);
        let wide_png = encode(4000, 1000, "png");
        let broken = [1u8, 2, 3];
        let (bmp_result, wide_result, broken_result) = tokio::join!(
            ImageProcessor::prepare(
                images.as_ref(),
                &small_bmp,
                "image/bmp",
                DEFAULT_IMAGE_LIMITS
            ),
            ImageProcessor::prepare(
                images.as_ref(),
                &wide_png,
                "image/png",
                DEFAULT_IMAGE_LIMITS
            ),
            ImageProcessor::prepare(images.as_ref(), &broken, "image/png", DEFAULT_IMAGE_LIMITS),
        );
        assert_eq!(bmp_result.expect("bmp prepares").mime_type, "image/png");
        assert_eq!(
            wide_result.expect("wide prepares").resized.map(|r| r.to),
            Some((2000, 500))
        );
        assert!(broken_result.is_none());
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let after_idle = create_node_photon_images(NodePhotonImagesOptions {
            worker: true,
            idle_ms: 10,
        })
        .prepare(&bmp(2, 2), "image/bmp", DEFAULT_IMAGE_LIMITS)
        .expect("bmp prepares after the idle period");
        assert_eq!(after_idle.mime_type, "image/png");
    }

    /// 只含签名与 IHDR 的截断 PNG：`image/png` 的头能解析，但像素数据缺失。
    fn truncated_png() -> Vec<u8> {
        let mut bytes = encode(1, 1, "png");
        bytes.truncate(40);
        bytes
    }

    /// 回归：无法解码的图片返回 `None`，且不向 stderr 打印任何东西。TS 的
    /// `catch { return undefined; }` 没有输出；旧实现让 photon-rs 的
    /// `image::load_from_memory(...).unwrap()` panic，默认 panic hook 会写 stderr。
    ///
    /// stderr 是进程级的，为了不受并行测试影响，在子进程里跑 [`undecodable_image_probe`]。
    #[test]
    fn an_undecodable_image_returns_none_without_printing_a_panic() {
        let output = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([
                "--exact",
                "with_processor::undecodable_image_probe",
                "--nocapture",
            ])
            .env("KIANA_DURABLE_UNDECODABLE_IMAGE_PROBE", "1")
            .output()
            .expect("run the probe in a child process");
        assert!(
            output.status.success(),
            "probe failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("PROBE_RAN"),
            "the probe test did not run:\n{}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert!(
            output.stderr.is_empty(),
            "prepare wrote to stderr:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// [`an_undecodable_image_returns_none_without_printing_a_panic`] 的子进程侧。
    #[test]
    fn undecodable_image_probe() {
        if std::env::var_os("KIANA_DURABLE_UNDECODABLE_IMAGE_PROBE").is_none() {
            return;
        }
        println!("PROBE_RAN");
        assert!(
            create_photon_images()
                .prepare(&truncated_png(), "image/png", DEFAULT_IMAGE_LIMITS)
                .is_none()
        );
    }
}
