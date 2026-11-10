//! `utils/clipboard-image.ts` 的移植测试（TS: `test/clipboard-image.test.ts`）。
//!
//! TS 用 `vi.mock` 替换 `clipboard-command` 与 `@earendil-works/kiana-tui`。Rust 侧通过
//! `#[doc(hidden)]` 的 [`ClipboardTestIo`] 注入命令执行器与原生命中；测试绝不真正执行
//! `wl-paste`/`xclip`/`powershell.exe`，也不碰真实剪贴板。
#![cfg(feature = "images")]

use std::ffi::OsString;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use kiana_core::clipboard::{ClipboardCommandRequest, ClipboardCommandRunner, ClipboardTestIo};
use kiana_core::image_process::{
    ClipboardImage, ReadClipboardImageOptions, extension_for_image_mime_type, is_wayland_session,
    read_clipboard_image, read_clipboard_image_with,
};
use kiana_tui::native::{ClipboardError, ClipboardFuture, ClipboardValue, NativeClipboard};

/// 14 字节的 PNG 头（ISPNG + IHDR chunk 头）。
fn png() -> Vec<u8> {
    vec![
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13, 0x49, 0x48, 0x44, 0x52,
    ]
}

fn entries(pairs: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
    pairs
        .iter()
        .map(|(key, value)| (OsString::from(key), OsString::from(value)))
        .collect()
}

#[derive(Clone, Default)]
struct CommandLog(Arc<Mutex<Vec<ClipboardCommandRequest>>>);

impl CommandLog {
    fn calls(&self) -> Vec<ClipboardCommandRequest> {
        self.0.lock().expect("command log").clone()
    }

    fn names(&self) -> Vec<String> {
        self.calls()
            .into_iter()
            .map(|request| request.command)
            .collect()
    }
}

fn command_mock<F>(respond: F) -> (ClipboardCommandRunner, CommandLog)
where
    F: Fn(&ClipboardCommandRequest) -> Option<Vec<u8>> + Send + Sync + 'static,
{
    let log = CommandLog::default();
    let log_for_runner = log.clone();
    let respond = Arc::new(respond);
    let runner: ClipboardCommandRunner = Arc::new(move |request: ClipboardCommandRequest| {
        log_for_runner
            .0
            .lock()
            .expect("command log")
            .push(request.clone());
        let result = respond(&request);
        Box::pin(async move { result })
    });
    (runner, log)
}

struct NativeInner {
    image: Mutex<Result<ClipboardValue<Vec<u8>>, ClipboardError>>,
    image_calls: AtomicUsize,
}

/// 只实现 getImage 的替身；getText 恒为空。
struct NativeMock {
    inner: Arc<NativeInner>,
}

impl NativeMock {
    fn new() -> Self {
        NativeMock {
            inner: Arc::new(NativeInner {
                image: Mutex::new(Ok(ClipboardValue::Empty)),
                image_calls: AtomicUsize::new(0),
            }),
        }
    }

    fn with_image(self, value: Result<ClipboardValue<Vec<u8>>, ClipboardError>) -> Self {
        *self.inner.image.lock().expect("image") = value;
        self
    }

    fn handle(&self) -> Arc<dyn NativeClipboard> {
        self.inner.clone()
    }

    fn image_calls(&self) -> usize {
        self.inner.image_calls.load(Ordering::SeqCst)
    }
}

impl NativeClipboard for NativeInner {
    fn get_text(&self) -> ClipboardFuture<'_, String> {
        Box::pin(async { Ok(ClipboardValue::Empty) })
    }

    fn get_image(&self) -> ClipboardFuture<'_, Vec<u8>> {
        self.image_calls.fetch_add(1, Ordering::SeqCst);
        let value = self.image.lock().expect("image").clone();
        Box::pin(async move { value })
    }
}

struct Harness {
    io: ClipboardTestIo,
    commands: CommandLog,
}

fn make_io(platform: &str, env: &[(&str, &str)]) -> Harness {
    let (runner, commands) = command_mock(|_| None);
    Harness {
        io: ClipboardTestIo {
            platform: platform.to_string(),
            env: Some(entries(env)),
            native: None,
            run_command: runner,
            write_stdout: Arc::new(|_text: &str| {}),
        },
        commands,
    }
}

impl Harness {
    fn with_commands<F>(mut self, respond: F) -> Self
    where
        F: Fn(&ClipboardCommandRequest) -> Option<Vec<u8>> + Send + Sync + 'static,
    {
        let (runner, commands) = command_mock(respond);
        self.io.run_command = runner;
        self.commands = commands;
        self
    }

    fn with_native(mut self, native: &NativeMock) -> Self {
        self.io.native = Some(native.handle());
        self
    }
}

fn listing(args: &[String]) -> bool {
    args.iter()
        .any(|arg| arg == "--list-types" || arg == "TARGETS")
}

// ---------------------------------------------------------------------------
// describe("readClipboardImage")
// ---------------------------------------------------------------------------

#[tokio::test]
async fn wayland_and_x11_command_images_stop_the_fallback() {
    type Case = (
        &'static str,
        &'static str,
        &'static [(&'static str, &'static str)],
    );
    let cases: [Case; 2] = [
        (
            "wayland",
            "wl-paste",
            &[("WAYLAND_DISPLAY", "1"), ("DISPLAY", ":0")],
        ),
        ("x11", "xclip", &[("DISPLAY", ":0")]),
    ];
    for (backend, command, env) in cases {
        for present in [true, false] {
            let native = NativeMock::new();
            let expected_png = png();
            let harness =
                make_io("linux", env)
                    .with_native(&native)
                    .with_commands(move |request| {
                        assert_eq!(request.command, command);
                        if listing(&request.args) {
                            Some(if present {
                                b"text/plain\nimage/png\n".to_vec()
                            } else {
                                b"text/plain\n".to_vec()
                            })
                        } else {
                            Some(expected_png.clone())
                        }
                    });

            let image = read_clipboard_image_with(&harness.io)
                .await
                .expect("no transfer error");
            if present {
                assert_eq!(
                    image,
                    Some(ClipboardImage {
                        bytes: png(),
                        mime_type: "image/png".to_string(),
                    }),
                    "{backend} present"
                );
            } else {
                assert_eq!(image, None, "{backend} absent");
            }
            assert_eq!(
                harness.commands.calls().len(),
                if present { 2 } else { 1 },
                "{backend}"
            );
            assert_eq!(native.image_calls(), 0);
        }
    }
}

#[tokio::test]
async fn x11_does_not_probe_image_types_when_targets_fails() {
    // #9786 的回归测试。
    let native = NativeMock::new();
    let harness = make_io("linux", &[("DISPLAY", ":0")])
        .with_native(&native)
        .with_commands(|request| {
            if listing(&request.args) {
                None
            } else {
                Some(b"hello".to_vec())
            }
        });

    assert_eq!(
        read_clipboard_image_with(&harness.io)
            .await
            .expect("no error"),
        None
    );
    let calls = harness.commands.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].command, "xclip");
    assert_eq!(
        calls[0].args,
        ["-selection", "clipboard", "-t", "TARGETS", "-o"]
    );
    assert_eq!(calls[0].timeout_ms, 1000);
    assert_eq!(native.image_calls(), 1);
}

#[tokio::test]
async fn x11_does_not_probe_unadvertised_image_types() {
    let native = NativeMock::new();
    let harness = make_io("linux", &[("DISPLAY", ":0")])
        .with_native(&native)
        .with_commands(|request| {
            if listing(&request.args) {
                Some(b"image/png\n".to_vec())
            } else if request.args.iter().any(|arg| arg == "image/png") {
                None
            } else {
                Some(b"hello".to_vec())
            }
        });

    assert_eq!(
        read_clipboard_image_with(&harness.io)
            .await
            .expect("no error"),
        None
    );
    let probed: Vec<String> = harness
        .commands
        .calls()
        .into_iter()
        .map(|request| request.args[3].clone())
        .collect();
    assert_eq!(probed, ["TARGETS", "image/png"]);
    assert_eq!(native.image_calls(), 1);
}

#[tokio::test]
async fn native_x11_result_stops_the_fallback() {
    let empty = Vec::new();
    let cases: [Option<Vec<u8>>; 3] = [Some(png()), None, Some(empty)];
    for bytes in cases {
        let native = match &bytes {
            Some(bytes) => NativeMock::new().with_image(Ok(ClipboardValue::Value(bytes.clone()))),
            None => NativeMock::new().with_image(Ok(ClipboardValue::Empty)),
        };
        let harness = make_io("linux", &[("DISPLAY", ":0")]).with_native(&native);

        let expected = match &bytes {
            Some(bytes) if !bytes.is_empty() => Some(ClipboardImage {
                bytes: bytes.clone(),
                mime_type: "image/png".to_string(),
            }),
            _ => None,
        };
        assert_eq!(
            read_clipboard_image_with(&harness.io)
                .await
                .expect("no error"),
            expected
        );
        assert_eq!(native.image_calls(), 1);
        assert_eq!(harness.commands.names(), ["xclip"]);
    }
}

#[tokio::test]
async fn wayland_falls_back_to_x11_when_wl_paste_or_the_helper_is_unavailable() {
    for missing_module in [true, false] {
        let unavailable = NativeMock::new().with_image(Ok(ClipboardValue::Unavailable));
        let mut harness = make_io("linux", &[("WAYLAND_DISPLAY", "1")]).with_commands(|request| {
            if request.command == "wl-paste" {
                return None;
            }
            if listing(&request.args) {
                Some(b"image/png\n".to_vec())
            } else {
                Some(png())
            }
        });
        if !missing_module {
            // 原生 helper 存在但 DISPLAY 不可用（getImage → undefined）。
            harness = harness.with_native(&unavailable);
        }

        assert_eq!(
            read_clipboard_image_with(&harness.io)
                .await
                .expect("no error"),
            Some(ClipboardImage {
                bytes: png(),
                mime_type: "image/png".to_string(),
            })
        );
        // Wayland 分支已经拿到图片，原生从未被调用。
        if !missing_module {
            assert_eq!(unavailable.image_calls(), 0);
        }
        assert_eq!(harness.commands.names(), ["wl-paste", "xclip", "xclip"]);
    }
}

#[tokio::test]
async fn wsl_tries_powershell_before_a_broken_native_x11_bridge() {
    let native = NativeMock::new().with_image(Err(ClipboardError::new("Broken X11 bridge")));
    let tmp_file: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let tmp_file_for_mock = tmp_file.clone();
    let harness = make_io("linux", &[("WSL_DISTRO_NAME", "Ubuntu")])
        .with_native(&native)
        .with_commands(move |request| match request.command.as_str() {
            "wl-paste" | "xclip" => None,
            "wslpath" => {
                *tmp_file_for_mock.lock().expect("tmp file") = Some(request.args[1].clone());
                Some(b"C:\\Users\\O'Hare\\clip.png\n".to_vec())
            }
            "powershell.exe" => {
                assert!(
                    request.args[2].contains("$path = 'C:\\Users\\O''Hare\\clip.png'"),
                    "script: {}",
                    request.args[2]
                );
                let path = tmp_file_for_mock
                    .lock()
                    .expect("tmp file")
                    .clone()
                    .expect("wslpath must run before powershell.exe");
                std::fs::write(path, png()).expect("write clipboard image");
                Some(b"ok\n".to_vec())
            }
            other => panic!("Unexpected command: {other}"),
        });

    assert_eq!(
        read_clipboard_image_with(&harness.io)
            .await
            .expect("no error"),
        Some(ClipboardImage {
            bytes: png(),
            mime_type: "image/png".to_string(),
        })
    );
    assert_eq!(native.image_calls(), 0);
}

#[tokio::test]
async fn darwin_and_win32_read_the_native_image_once() {
    for platform in ["darwin", "win32"] {
        let empty = Vec::new();
        let cases: [Option<Vec<u8>>; 4] = [Some(png()), None, Some(empty), None];
        for (index, bytes) in cases.into_iter().enumerate() {
            let native = match index {
                3 => NativeMock::new().with_image(Ok(ClipboardValue::Unavailable)),
                _ => match &bytes {
                    Some(bytes) => {
                        NativeMock::new().with_image(Ok(ClipboardValue::Value(bytes.clone())))
                    }
                    None => NativeMock::new().with_image(Ok(ClipboardValue::Empty)),
                },
            };
            let harness = make_io(platform, &[])
                .with_native(&native)
                .with_commands(|_| panic!("commands must not run on non-Linux platforms"));

            let expected = match &bytes {
                Some(bytes) if !bytes.is_empty() => Some(ClipboardImage {
                    bytes: bytes.clone(),
                    mime_type: "image/png".to_string(),
                }),
                _ => None,
            };
            assert_eq!(
                read_clipboard_image_with(&harness.io)
                    .await
                    .expect("no error"),
                expected,
                "{platform} case {index}"
            );
            assert_eq!(native.image_calls(), 1);
        }
    }
}

#[tokio::test]
async fn returns_null_without_a_native_helper() {
    let harness = make_io("win32", &[]);
    assert_eq!(
        read_clipboard_image_with(&harness.io)
            .await
            .expect("no error"),
        None
    );
    assert!(harness.commands.names().is_empty());
}

#[tokio::test]
async fn propagates_native_transfer_errors_without_fallback() {
    for platform in ["linux", "win32"] {
        let native = NativeMock::new().with_image(Err(ClipboardError::new(
            "Native clipboard operation failed",
        )));
        let harness = make_io(platform, &[("WAYLAND_DISPLAY", "1"), ("DISPLAY", ":0")])
            .with_native(&native)
            .with_commands(|_| None);

        let error = read_clipboard_image_with(&harness.io)
            .await
            .expect_err("native rejection must propagate");
        assert_eq!(
            error,
            ClipboardError::new("Native clipboard operation failed")
        );
        assert_eq!(
            harness.commands.names(),
            if platform == "linux" {
                vec!["wl-paste".to_string(), "xclip".to_string()]
            } else {
                Vec::new()
            }
        );
    }
}

#[tokio::test]
async fn termux_does_not_read_image_clipboards() {
    let native = NativeMock::new().with_image(Ok(ClipboardValue::Value(png())));
    let harness = make_io("linux", &[("TERMUX_VERSION", "0.119")])
        .with_native(&native)
        .with_commands(|_| panic!("Termux must not run clipboard commands"));

    assert_eq!(
        read_clipboard_image_with(&harness.io)
            .await
            .expect("no error"),
        None
    );
    assert_eq!(native.image_calls(), 0);
    assert!(harness.commands.names().is_empty());
}

#[tokio::test]
async fn unknown_native_bytes_are_not_a_supported_image() {
    let native = NativeMock::new().with_image(Ok(ClipboardValue::Value(b"hello".to_vec())));
    let harness = make_io("win32", &[]).with_native(&native);

    // 魔数未知 → `application/octet-stream` → Photon 转换失败 → null。
    assert_eq!(
        read_clipboard_image_with(&harness.io)
            .await
            .expect("no error"),
        None
    );
}

#[tokio::test]
async fn public_options_short_circuit_on_termux_without_touching_the_system() {
    // 触达公开入口而不执行任何真实命令：TERMUX_VERSION 在平台分支之前就返回 null。
    let result = read_clipboard_image(ReadClipboardImageOptions {
        platform: Some("linux".to_string()),
        env: Some(entries(&[("TERMUX_VERSION", "0.119")])),
    })
    .await
    .expect("no error");
    assert_eq!(result, None);
}

#[test]
fn is_wayland_session_and_extension_mapping() {
    let wayland = entries(&[("WAYLAND_DISPLAY", "wayland-0")]);
    assert!(is_wayland_session(Some(&wayland)));
    let xdg = entries(&[("XDG_SESSION_TYPE", "wayland")]);
    assert!(is_wayland_session(Some(&xdg)));
    let x11 = entries(&[("DISPLAY", ":0"), ("XDG_SESSION_TYPE", "x11")]);
    assert!(!is_wayland_session(Some(&x11)));
    assert!(!is_wayland_session(Some(&[])));

    assert_eq!(extension_for_image_mime_type("image/png"), Some("png"));
    assert_eq!(extension_for_image_mime_type("image/jpeg"), Some("jpg"));
    assert_eq!(
        extension_for_image_mime_type("IMAGE/JPEG; charset=x"),
        Some("jpg")
    );
    assert_eq!(extension_for_image_mime_type("image/webp"), Some("webp"));
    assert_eq!(extension_for_image_mime_type("image/gif"), Some("gif"));
    assert_eq!(extension_for_image_mime_type("image/bmp"), None);
    assert_eq!(extension_for_image_mime_type("text/plain"), None);
}
