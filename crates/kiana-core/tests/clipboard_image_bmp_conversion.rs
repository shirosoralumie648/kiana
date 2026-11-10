//! `utils/clipboard-image.ts` 的 BMP → PNG 转换测试（TS: `test/clipboard-image-bmp-conversion.test.ts`）。
//!
//! WSL2/WSLg 上剪贴板经常提供 `image/bmp` 而不是 `image/png`；这条路径必须用 Photon 转成 PNG。
//! 单独成文件的原因与 TS 相同：命令与原生剪贴板都要返回 BMP。
#![cfg(feature = "images")]

use std::ffi::OsString;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use kiana_core::clipboard::{ClipboardCommandRequest, ClipboardCommandRunner, ClipboardTestIo};
use kiana_core::image_process::read_clipboard_image_with;
use kiana_tui::native::{ClipboardFuture, ClipboardValue, NativeClipboard};

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

struct NativeImage {
    image_calls: AtomicUsize,
}

impl NativeClipboard for NativeImage {
    fn get_text(&self) -> ClipboardFuture<'_, String> {
        Box::pin(async { Ok(ClipboardValue::Empty) })
    }

    fn get_image(&self) -> ClipboardFuture<'_, Vec<u8>> {
        self.image_calls.fetch_add(1, Ordering::SeqCst);
        let bytes = create_tiny_bmp_1x1_red_24bpp();
        Box::pin(async move { Ok(ClipboardValue::Value(bytes)) })
    }
}

#[tokio::test]
async fn converts_command_and_native_bmp_to_png() {
    for platform in ["linux", "win32"] {
        let native = Arc::new(NativeImage {
            image_calls: AtomicUsize::new(0),
        });
        let runner: ClipboardCommandRunner = Arc::new(|request: ClipboardCommandRequest| {
            let response = match request.command.as_str() {
                "wl-paste" if request.args.iter().any(|arg| arg == "--list-types") => {
                    Some(b"image/bmp\n".to_vec())
                }
                "wl-paste" if request.args.iter().any(|arg| arg == "image/bmp") => {
                    Some(create_tiny_bmp_1x1_red_24bpp())
                }
                _ => None,
            };
            Box::pin(async move { response })
        });

        let env: Vec<(OsString, OsString)> = if platform == "linux" {
            vec![(
                OsString::from("WAYLAND_DISPLAY"),
                OsString::from("wayland-0"),
            )]
        } else {
            Vec::new()
        };
        let io = ClipboardTestIo {
            platform: platform.to_string(),
            env: Some(env),
            native: Some(native.clone() as Arc<dyn NativeClipboard>),
            run_command: runner,
            write_stdout: Arc::new(|_text: &str| {}),
        };

        let image = read_clipboard_image_with(&io)
            .await
            .expect("no transfer error")
            .expect("an image is available");
        assert_eq!(image.mime_type, "image/png", "{platform}");
        assert_eq!(&image.bytes[0..4], &[0x89, 0x50, 0x4e, 0x47], "{platform}");
        assert!(!image.bytes.is_empty());
    }
}
