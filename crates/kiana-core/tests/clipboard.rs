//! `utils/clipboard.ts` 的移植测试（TS: `test/clipboard.test.ts`）。
//!
//! TS 用 `vi.mock` 替换 `node:os.platform`、`process.env`、`clipboard-command` 与
//! `@earendil-works/kiana-tui`；Rust 侧用 `#[doc(hidden)]` 的注入点
//! [`ClipboardTestIo`] 达到同样目的（平台/环境/原生命中/命令执行器/stdout 写入器）。

use std::ffi::OsString;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kiana_core::clipboard::{
    ClipboardCommandRequest, ClipboardCommandRunner, ClipboardTestIo, copy_to_clipboard_with,
    open_browser_with, read_clipboard_file_paths_with, read_clipboard_text_with,
};
use kiana_tui::native::{ClipboardError, ClipboardFuture, ClipboardValue, NativeClipboard};

// ---------------------------------------------------------------------------
// 环境与命令 mock
// ---------------------------------------------------------------------------

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
        self.0.lock().expect("command log lock").clone()
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
            .expect("command log lock")
            .push(request.clone());
        let result = respond(&request);
        Box::pin(async move { result })
    });
    (runner, log)
}

fn base64(text: &str) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(text.as_bytes())
}

fn osc52(text: &str) -> String {
    format!("\x1b]52;c;{}\x07", base64(text))
}

// ---------------------------------------------------------------------------
// 原生剪贴板 mock
// ---------------------------------------------------------------------------

type SetTextFuture =
    Pin<Box<dyn Future<Output = Result<ClipboardValue<()>, ClipboardError>> + Send>>;
type SetTextFactory = Arc<dyn Fn(String) -> SetTextFuture + Send + Sync>;

struct NativeInner {
    text: Mutex<Result<ClipboardValue<String>, ClipboardError>>,
    image: Mutex<Result<ClipboardValue<Vec<u8>>, ClipboardError>>,
    set_text: Mutex<Option<SetTextFactory>>,
    file_paths: Mutex<Option<Result<ClipboardValue<Vec<String>>, ClipboardError>>>,
    text_calls: AtomicUsize,
    image_calls: AtomicUsize,
    set_text_calls: Mutex<Vec<String>>,
}

/// 可配置的 `NativeClipboard` 替身（默认：getText → null、getImage → null、没有 setText）。
struct NativeMock {
    inner: Arc<NativeInner>,
}

impl NativeMock {
    fn new() -> Self {
        NativeMock {
            inner: Arc::new(NativeInner {
                text: Mutex::new(Ok(ClipboardValue::Empty)),
                image: Mutex::new(Ok(ClipboardValue::Empty)),
                set_text: Mutex::new(None),
                file_paths: Mutex::new(None),
                text_calls: AtomicUsize::new(0),
                image_calls: AtomicUsize::new(0),
                set_text_calls: Mutex::new(Vec::new()),
            }),
        }
    }

    fn with_text(self, value: Result<ClipboardValue<String>, ClipboardError>) -> Self {
        *self.inner.text.lock().expect("text lock") = value;
        self
    }

    fn with_set_text(
        self,
        factory: impl Fn(String) -> SetTextFuture + Send + Sync + 'static,
    ) -> Self {
        *self.inner.set_text.lock().expect("set_text lock") = Some(Arc::new(factory));
        self
    }

    fn with_default_set_text(self) -> Self {
        self.with_set_text(|_text| Box::pin(async { Ok(ClipboardValue::Value(())) }))
    }

    fn with_file_paths(self, value: Result<ClipboardValue<Vec<String>>, ClipboardError>) -> Self {
        *self.inner.file_paths.lock().expect("file paths lock") = Some(value);
        self
    }

    fn handle(&self) -> Arc<dyn NativeClipboard> {
        self.inner.clone()
    }

    fn text_calls(&self) -> usize {
        self.inner.text_calls.load(Ordering::SeqCst)
    }

    fn set_text_calls(&self) -> Vec<String> {
        self.inner
            .set_text_calls
            .lock()
            .expect("set_text calls")
            .clone()
    }
}

impl NativeClipboard for NativeInner {
    fn get_text(&self) -> ClipboardFuture<'_, String> {
        self.text_calls.fetch_add(1, Ordering::SeqCst);
        let value = self.text.lock().expect("text lock").clone();
        Box::pin(async move { value })
    }

    fn get_image(&self) -> ClipboardFuture<'_, Vec<u8>> {
        self.image_calls.fetch_add(1, Ordering::SeqCst);
        let value = self.image.lock().expect("image lock").clone();
        Box::pin(async move { value })
    }

    fn set_text(&self, text: String) -> Option<ClipboardFuture<'_, ()>> {
        let factory = self.set_text.lock().expect("set_text lock").clone()?;
        self.set_text_calls
            .lock()
            .expect("set_text calls")
            .push(text.clone());
        let future = factory(text);
        Some(future)
    }

    fn get_file_paths(&self) -> Option<ClipboardFuture<'_, Vec<String>>> {
        let value = self.file_paths.lock().expect("file paths lock").clone()?;
        Some(Box::pin(async move { value }))
    }
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

struct Harness {
    io: ClipboardTestIo,
    writes: Arc<Mutex<Vec<String>>>,
    commands: CommandLog,
}

fn make_io(platform: &str, env: &[(&str, &str)]) -> Harness {
    let writes = Arc::new(Mutex::new(Vec::new()));
    let writes_for_io = writes.clone();
    let (runner, commands) = command_mock(|_| Some(Vec::new()));
    Harness {
        io: ClipboardTestIo {
            platform: platform.to_string(),
            env: Some(entries(env)),
            native: None,
            run_command: runner,
            write_stdout: Arc::new(move |text: &str| {
                writes_for_io.lock().expect("writes").push(text.to_string());
            }),
        },
        writes,
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

    fn writes(&self) -> Vec<String> {
        self.writes.lock().expect("writes").clone()
    }
}

// ---------------------------------------------------------------------------
// readClipboardText
// ---------------------------------------------------------------------------

#[tokio::test]
async fn awaits_native_clipboard_text_and_catches_rejected_reads() {
    let native = NativeMock::new().with_text(Ok(ClipboardValue::Value("clipboard text".into())));
    let harness = make_io("darwin", &[]).with_native(&native);
    assert_eq!(
        read_clipboard_text_with(&harness.io).await.as_deref(),
        Some("clipboard text")
    );

    let rejecting = NativeMock::new().with_text(Err(ClipboardError::new("clipboard unavailable")));
    let harness = make_io("darwin", &[]).with_native(&rejecting);
    assert_eq!(read_clipboard_text_with(&harness.io).await, None);
}

#[tokio::test]
async fn command_results_stop_the_fallback_including_empty_text() {
    // #7248 的回归测试：空的 Wayland 内容不能回落到陈旧的 X11 剪贴板。
    let cases: [(&str, &str, &[&str], &[&str]); 3] = [
        (
            "WAYLAND_DISPLAY",
            "wl-paste",
            &["--no-newline", "--type", "text"],
            &["wl-paste"],
        ),
        (
            "DISPLAY",
            "xclip",
            &["-selection", "clipboard", "-out"],
            &["xclip"],
        ),
        (
            "DISPLAY",
            "xsel",
            &["--clipboard", "--output"],
            &["xclip", "xsel"],
        ),
    ];

    for (env_key, command, args, expected_calls) in cases {
        for text in ["clipboard text", ""] {
            let native = NativeMock::new().with_text(Ok(ClipboardValue::Value("stale".into())));
            let harness = make_io("linux", &[("DISPLAY", ":0"), (env_key, "1")])
                .with_native(&native)
                .with_commands(move |request| {
                    if request.command == command {
                        Some(text.as_bytes().to_vec())
                    } else {
                        None
                    }
                });
            let expected_args: Vec<String> = args.iter().map(|arg| (*arg).to_string()).collect();

            let result = read_clipboard_text_with(&harness.io).await;
            assert_eq!(
                result.as_deref(),
                if text.is_empty() { None } else { Some(text) }
            );
            assert_eq!(harness.commands.names(), expected_calls);
            let last = harness.commands.calls().last().cloned().expect("one call");
            assert_eq!(last.command, command);
            assert_eq!(last.args, expected_args);
            assert_eq!(last.timeout_ms, 5000);
            assert_eq!(native.text_calls(), 0);
        }
    }
}

#[tokio::test]
async fn uses_native_x11_after_command_failures() {
    let cases: [Result<ClipboardValue<String>, ClipboardError>; 4] = [
        Ok(ClipboardValue::Value("native text".into())),
        Ok(ClipboardValue::Value(String::new())),
        Ok(ClipboardValue::Empty),
        Ok(ClipboardValue::Unavailable),
    ];

    for native_result in cases {
        let expected = match &native_result {
            Ok(ClipboardValue::Value(text)) if !text.is_empty() => Some(text.clone()),
            _ => None,
        };
        let native = NativeMock::new().with_text(native_result);
        let harness = make_io(
            "linux",
            &[("DISPLAY", ":0"), ("WAYLAND_DISPLAY", "wayland-0")],
        )
        .with_native(&native)
        .with_commands(|_| None);

        assert_eq!(
            read_clipboard_text_with(&harness.io).await,
            expected,
            "native result"
        );
        assert_eq!(native.text_calls(), 1);
        assert_eq!(harness.commands.names(), ["wl-paste", "xclip", "xsel"]);
    }
}

#[tokio::test]
async fn termux_reads_termux_clipboard_get_on_android() {
    // #10391 的回归测试：Termux 报告的 platform 是 "android"。
    for text in ["clipboard text", ""] {
        let native = NativeMock::new().with_text(Ok(ClipboardValue::Value("native".into())));
        let harness = make_io("android", &[("TERMUX_VERSION", "0.119")])
            .with_native(&native)
            .with_commands(move |_| Some(text.as_bytes().to_vec()));

        assert_eq!(
            read_clipboard_text_with(&harness.io).await.as_deref(),
            if text.is_empty() { None } else { Some(text) }
        );
        let calls = harness.commands.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].command, "termux-clipboard-get");
        assert!(calls[0].args.is_empty());
        assert_eq!(calls[0].timeout_ms, 5000);
        assert_eq!(native.text_calls(), 0);
    }
}

#[tokio::test]
async fn falls_back_to_x11_tools_when_wl_paste_is_unavailable() {
    let native = NativeMock::new().with_text(Ok(ClipboardValue::Value("native".into())));
    let harness = make_io(
        "linux",
        &[("WAYLAND_DISPLAY", "wayland-0"), ("DISPLAY", ":0")],
    )
    .with_native(&native)
    .with_commands(|request| {
        if request.command == "wl-paste" {
            None
        } else {
            Some(b"X11 text".to_vec())
        }
    });

    assert_eq!(
        read_clipboard_text_with(&harness.io).await.as_deref(),
        Some("X11 text")
    );
    assert_eq!(native.text_calls(), 0);
}

#[tokio::test]
async fn reads_native_file_paths_and_maps_empty_to_none() {
    let native = NativeMock::new().with_file_paths(Ok(ClipboardValue::Value(vec![
        "/tmp/a.txt".to_string(),
        "/tmp/b.txt".to_string(),
    ])));
    let harness = make_io("darwin", &[]).with_native(&native);
    assert_eq!(
        read_clipboard_file_paths_with(&harness.io).await,
        Ok(Some(vec![
            "/tmp/a.txt".to_string(),
            "/tmp/b.txt".to_string()
        ]))
    );

    let empty = NativeMock::new().with_file_paths(Ok(ClipboardValue::Value(Vec::new())));
    let harness = make_io("darwin", &[]).with_native(&empty);
    assert_eq!(read_clipboard_file_paths_with(&harness.io).await, Ok(None));

    // `null` / `undefined` → null。
    for value in [ClipboardValue::Empty, ClipboardValue::Unavailable] {
        let none = NativeMock::new().with_file_paths(Ok(value));
        let harness = make_io("darwin", &[]).with_native(&none);
        assert_eq!(read_clipboard_file_paths_with(&harness.io).await, Ok(None));
    }

    // 后端不导出 getFilePaths → null。
    let unsupported = NativeMock::new();
    let harness = make_io("win32", &[]).with_native(&unsupported);
    assert_eq!(read_clipboard_file_paths_with(&harness.io).await, Ok(None));

    // 没有原生后端 → null。
    let harness = make_io("linux", &[]);
    assert_eq!(read_clipboard_file_paths_with(&harness.io).await, Ok(None));
}

/// TS 的 `readClipboardFilePaths` 不捕获 `getFilePaths()` 的拒绝：Promise rejection 向上传播，
/// `handleClipboardPaste` 的 `catch` 显示 `Failed to paste from clipboard: <message>` 并中止粘贴，
/// 不再回落到图片/文本。注入版必须保留这个 `Err`（`clipboard-paste-file-paths.test.ts` 的
/// "native file-path errors are shown without falling through to the icon image"）。
#[tokio::test]
async fn propagates_rejected_native_file_path_reads() {
    let native = NativeMock::new().with_file_paths(Err(ClipboardError::cancelled()));
    let harness = make_io("darwin", &[]).with_native(&native);
    assert_eq!(
        read_clipboard_file_paths_with(&harness.io).await,
        Err(ClipboardError::cancelled())
    );
}

// ---------------------------------------------------------------------------
// copyToClipboard
// ---------------------------------------------------------------------------

#[tokio::test]
async fn local_native_success_skips_osc52_and_commands() {
    let native = NativeMock::new().with_default_set_text();
    let harness = make_io("darwin", &[]).with_native(&native);

    copy_to_clipboard_with("hello", &harness.io)
        .await
        .expect("copied");

    assert_eq!(native.set_text_calls(), ["hello"]);
    assert!(harness.writes().is_empty());
    assert!(harness.commands.names().is_empty());
}

#[tokio::test]
async fn linux_skips_the_native_writer() {
    let native = NativeMock::new().with_default_set_text();
    let harness = make_io("linux", &[("DISPLAY", ":0")]).with_native(&native);

    copy_to_clipboard_with("hello", &harness.io)
        .await
        .expect("copied");

    assert!(native.set_text_calls().is_empty());
    let calls = harness.commands.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].command, "xclip");
    assert_eq!(calls[0].args, ["-selection", "clipboard"]);
    assert_eq!(calls[0].input.as_deref(), Some("hello"));
    assert_eq!(calls[0].timeout_ms, 5000);
}

#[tokio::test]
async fn waits_for_the_native_write_before_emitting_remote_osc52() {
    let (complete_tx, complete_rx) = tokio::sync::oneshot::channel::<()>();
    let complete_rx = Arc::new(Mutex::new(Some(complete_rx)));
    let started = Arc::new(tokio::sync::Notify::new());
    let started_for_mock = started.clone();

    let native = NativeMock::new().with_set_text(move |_text| {
        let receiver = complete_rx.lock().expect("receiver").take();
        let started = started_for_mock.clone();
        Box::pin(async move {
            started.notify_one();
            if let Some(receiver) = receiver {
                let _ = receiver.await;
            }
            Ok(ClipboardValue::Value(()))
        })
    });

    let harness = make_io("darwin", &[("SSH_CONNECTION", "client server")]).with_native(&native);
    let writes = harness.writes.clone();
    let commands = harness.commands.clone();
    let io = harness.io;
    let copy = tokio::spawn(async move { copy_to_clipboard_with("hello", &io).await });

    started.notified().await;
    assert!(writes.lock().expect("writes").is_empty());

    complete_tx.send(()).expect("complete");
    copy.await.expect("join").expect("copied");

    assert_eq!(*writes.lock().expect("writes"), vec![osc52("hello")]);
    assert!(commands.names().is_empty());
}

#[tokio::test]
async fn kiana_clipboard_osc52_emits_osc52_after_a_successful_copy() {
    // #10826 / #10835 / #10114 的回归测试：没有 SSH 标记的远程客户端。
    for (platform, display) in [("darwin", None), ("win32", None), ("linux", Some(":99"))] {
        let native = NativeMock::new().with_default_set_text();
        let mut env: Vec<(&str, &str)> = vec![("KIANA_CLIPBOARD_OSC52", "1")];
        if let Some(display) = display {
            env.push(("DISPLAY", display));
        }
        let harness = make_io(platform, &env).with_native(&native);

        copy_to_clipboard_with("hello", &harness.io)
            .await
            .expect("copied");

        if platform == "linux" {
            let calls = harness.commands.calls();
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].command, "xclip");
            assert_eq!(calls[0].args, ["-selection", "clipboard"]);
            assert_eq!(calls[0].input.as_deref(), Some("hello"));
            assert_eq!(calls[0].timeout_ms, 5000);
        } else {
            assert_eq!(native.set_text_calls(), ["hello"]);
        }
        assert_eq!(harness.writes(), vec![osc52("hello")]);
    }
}

#[tokio::test]
async fn a_rejected_native_write_falls_back_to_pbcopy() {
    let native = NativeMock::new()
        .with_set_text(|_text| Box::pin(async { Err(ClipboardError::new("native failed")) }));
    let harness = make_io("darwin", &[]).with_native(&native);

    copy_to_clipboard_with("hello", &harness.io)
        .await
        .expect("copied through pbcopy");

    let calls = harness.commands.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].command, "pbcopy");
    assert!(calls[0].args.is_empty());
    assert_eq!(calls[0].input.as_deref(), Some("hello"));
    assert_eq!(calls[0].timeout_ms, 5000);
    assert!(harness.writes().is_empty());
}

#[tokio::test]
async fn a_read_only_native_clipboard_uses_the_command_writer() {
    let native = NativeMock::new(); // 没有 setText
    let harness = make_io("darwin", &[]).with_native(&native);

    copy_to_clipboard_with("hello", &harness.io)
        .await
        .expect("copied");

    assert_eq!(harness.commands.calls().len(), 1);
    assert_eq!(harness.commands.names(), ["pbcopy"]);
}

#[tokio::test]
async fn tries_xclip_and_xsel_after_wl_copy_fails() {
    let harness = make_io(
        "linux",
        &[("WAYLAND_DISPLAY", "wayland-0"), ("DISPLAY", ":0")],
    )
    .with_commands(|request| {
        if request.command == "xsel" {
            Some(Vec::new())
        } else {
            None
        }
    });

    copy_to_clipboard_with("hello", &harness.io)
        .await
        .expect("copied");

    assert_eq!(harness.commands.names(), ["wl-copy", "xclip", "xsel"]);
    assert!(harness.writes().is_empty());
}

#[tokio::test]
async fn local_linux_failure_does_not_report_an_unverified_osc52_write_as_success() {
    // #9618 的回归测试。
    let harness = make_io("linux", &[("DISPLAY", ":0")]).with_commands(|_| None);

    let error = copy_to_clipboard_with("hello", &harness.io)
        .await
        .expect_err("must fail");
    assert_eq!(
        error.message(),
        "Clipboard unavailable: install `xclip` or `xsel`, or check X11 access"
    );
    assert_eq!(harness.commands.names(), ["xclip", "xsel"]);
    assert!(harness.writes().is_empty());
}

#[tokio::test]
async fn display_less_linux_falls_back_to_osc52() {
    // #9688 的回归测试：没有 X11/Wayland 访问的容器。
    let harness = make_io("linux", &[]);

    copy_to_clipboard_with("hello", &harness.io)
        .await
        .expect("copied via OSC 52");

    assert!(harness.commands.names().is_empty());
    assert_eq!(harness.writes(), vec![osc52("hello")]);
}

#[tokio::test]
async fn wsl_without_a_display_writes_the_windows_clipboard_through_powershell() {
    // #9688 的回归测试：关闭 WSLg 的 WSL。
    let written: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let written_for_mock = written.clone();
    let harness =
        make_io("linux", &[("WSL_DISTRO_NAME", "Ubuntu")]).with_commands(move |request| {
            if request.command != "wslpath" {
                return Some(Vec::new());
            }
            let path = &request.args[1];
            *written_for_mock.lock().expect("written") =
                Some(std::fs::read_to_string(path).expect("clipboard temp file"));
            Some(b"\\\\wsl.localhost\\Ubuntu\\tmp\\clip.txt\n".to_vec())
        });

    copy_to_clipboard_with("héllo", &harness.io)
        .await
        .expect("copied");

    assert_eq!(harness.commands.names(), ["wslpath", "powershell.exe"]);
    assert_eq!(written.lock().expect("written").as_deref(), Some("héllo"));

    let calls = harness.commands.calls();
    let wslpath_args = &calls[0].args;
    assert!(
        !std::path::Path::new(&wslpath_args[1]).exists(),
        "temp file must be deleted"
    );
    let powershell_args = &calls[1].args;
    assert!(powershell_args[2].contains("Set-Clipboard"));
    assert!(
        powershell_args[2].contains("'\\\\wsl.localhost\\Ubuntu\\tmp\\clip.txt'"),
        "script: {}",
        powershell_args[2]
    );
    assert!(harness.writes().is_empty());
}

#[tokio::test]
async fn wsl_falls_back_to_osc52_when_windows_interop_is_unavailable() {
    let harness = make_io("linux", &[("WSL_DISTRO_NAME", "Ubuntu")]).with_commands(|_| None);

    copy_to_clipboard_with("hello", &harness.io)
        .await
        .expect("copied");

    assert_eq!(harness.commands.names(), ["wslpath"]);
    assert_eq!(harness.writes(), vec![osc52("hello")]);
}

#[tokio::test]
async fn wsl_in_windows_terminal_prefers_osc52_over_powershell() {
    let harness = make_io(
        "linux",
        &[("WSL_DISTRO_NAME", "Ubuntu"), ("WT_SESSION", "session")],
    )
    .with_commands(|_| panic!("no command must run"));

    copy_to_clipboard_with("hello", &harness.io)
        .await
        .expect("copied");

    assert!(harness.commands.names().is_empty());
    assert_eq!(harness.writes(), vec![osc52("hello")]);
}

#[tokio::test]
async fn wsl_in_windows_terminal_emits_osc52_once_in_a_remote_session() {
    let harness = make_io(
        "linux",
        &[
            ("WSL_DISTRO_NAME", "Ubuntu"),
            ("WT_SESSION", "session"),
            ("SSH_CONNECTION", "client server"),
        ],
    )
    .with_commands(|_| panic!("no command must run"));

    copy_to_clipboard_with("hello", &harness.io)
        .await
        .expect("copied");

    assert!(harness.commands.names().is_empty());
    assert_eq!(harness.writes(), vec![osc52("hello")]);
}

#[tokio::test]
async fn wsl_in_windows_terminal_uses_powershell_for_oversized_osc52_payloads() {
    let harness = make_io(
        "linux",
        &[("WSL_DISTRO_NAME", "Ubuntu"), ("WT_SESSION", "session")],
    )
    .with_commands(|request| {
        if request.command == "wslpath" {
            Some(b"C:\\clip.txt".to_vec())
        } else {
            Some(Vec::new())
        }
    });

    copy_to_clipboard_with(&"x".repeat(80_000), &harness.io)
        .await
        .expect("copied through PowerShell");

    assert_eq!(harness.commands.names(), ["wslpath", "powershell.exe"]);
    assert!(harness.writes().is_empty());
}

#[tokio::test]
async fn wsl_with_a_display_prefers_the_linux_clipboard_tools() {
    let harness = make_io(
        "linux",
        &[
            ("WSL_DISTRO_NAME", "Ubuntu"),
            ("WAYLAND_DISPLAY", "wayland-0"),
        ],
    );

    copy_to_clipboard_with("hello", &harness.io)
        .await
        .expect("copied");

    assert_eq!(harness.commands.names(), ["wl-copy"]);
    assert!(harness.writes().is_empty());
}

#[tokio::test]
async fn reports_the_wayland_clipboard_tool_instead_of_the_x11_fallback() {
    let harness = make_io(
        "linux",
        &[("WAYLAND_DISPLAY", "wayland-0"), ("DISPLAY", ":0")],
    )
    .with_commands(|_| None);

    let error = copy_to_clipboard_with("hello", &harness.io)
        .await
        .expect_err("must fail");
    assert_eq!(
        error.message(),
        "Clipboard unavailable: install `wl-clipboard` (`wl-copy`) or check Wayland access"
    );
    assert_eq!(harness.commands.names(), ["wl-copy", "xclip", "xsel"]);
}

#[tokio::test]
async fn termux_on_android_writes_through_termux_clipboard_set() {
    let harness = make_io("android", &[("TERMUX_VERSION", "0.119")]);

    copy_to_clipboard_with("hello", &harness.io)
        .await
        .expect("copied");

    let calls = harness.commands.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].command, "termux-clipboard-set");
    assert!(calls[0].args.is_empty());
    assert_eq!(calls[0].input.as_deref(), Some("hello"));
    assert_eq!(calls[0].timeout_ms, 5000);
    assert!(harness.writes().is_empty());
}

#[tokio::test]
async fn reports_the_termux_api_requirement_on_android() {
    // #10391 的回归测试：Termux 报告的 platform 是 "android"。
    let harness = make_io("android", &[("TERMUX_VERSION", "0.119")]).with_commands(|_| None);

    let error = copy_to_clipboard_with("hello", &harness.io)
        .await
        .expect_err("must fail");
    assert_eq!(
        error.message(),
        "Clipboard unavailable: install the Termux:API app and `termux-api` package"
    );
}

#[tokio::test]
async fn uses_osc52_when_native_and_command_writes_fail_in_a_remote_session() {
    let native = NativeMock::new()
        .with_set_text(|_text| Box::pin(async { Err(ClipboardError::new("native failed")) }));
    let harness = make_io("darwin", &[("SSH_CONNECTION", "client server")])
        .with_native(&native)
        .with_commands(|_| None);

    copy_to_clipboard_with("hello", &harness.io)
        .await
        .expect("copied via OSC 52");

    assert_eq!(harness.writes(), vec![osc52("hello")]);
}

#[tokio::test]
async fn does_not_emit_oversized_osc52_payloads() {
    let native = NativeMock::new()
        .with_set_text(|_text| Box::pin(async { Err(ClipboardError::new("native failed")) }));
    let harness = make_io("darwin", &[("SSH_CONNECTION", "client server")])
        .with_native(&native)
        .with_commands(|_| None);

    let error = copy_to_clipboard_with(&"x".repeat(80_000), &harness.io)
        .await
        .expect_err("must fail");
    assert_eq!(
        error.message(),
        "Clipboard unavailable: text exceeds the OSC 52 size limit"
    );
    assert!(harness.writes().is_empty());
}

#[tokio::test]
async fn emits_the_exact_osc52_sequence() {
    let harness = make_io("linux", &[]);
    copy_to_clipboard_with("café 日本語", &harness.io)
        .await
        .expect("copied");
    assert_eq!(harness.writes(), vec![osc52("café 日本語")]);
}

// ---------------------------------------------------------------------------
// OSC 52 stdout 写入：必须立即到达终端（回归）
// ---------------------------------------------------------------------------

/// 回归测试：生产写入器（[`ClipboardTestIo::from_process`]）必须在写完后 flush。
///
/// TS 的 `process.stdout.write` 直接把字节交给终端/管道；Rust 的 stdout 是行缓冲的，
/// 而 OSC 52 序列没有换行。没有 flush 时字节会滞留到之后某次换行写出或进程退出。
///
/// 用「自执行」子进程验证：子进程发出 OSC 52 后只 sleep、不再写任何东西，父进程必须在
/// 子进程存活期间从管道里读到完整序列。
#[test]
fn osc52_writes_reach_stdout_immediately() {
    const HELPER_ENV: &str = "KIANA_OSC52_FLUSH_HELPER";
    const TEST_NAME: &str = "osc52_writes_reach_stdout_immediately";

    if std::env::var_os(HELPER_ENV).is_some() {
        // 子进程分支：用真实的 stdout 写入器发 OSC 52，然后保持存活且不再输出。
        let run_command: ClipboardCommandRunner =
            Arc::new(|_request: ClipboardCommandRequest| Box::pin(async { None }));
        let io = ClipboardTestIo {
            platform: "linux".to_string(),
            env: Some(vec![(
                OsString::from("SSH_CONNECTION"),
                OsString::from("client server"),
            )]),
            native: None,
            run_command,
            write_stdout: ClipboardTestIo::from_process().write_stdout,
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            copy_to_clipboard_with("flush-probe", &io)
                .await
                .expect("copied via OSC 52");
        });
        // 关键：不再写 stdout，也不退出。未 flush 的序列会一直留在缓冲区里。
        std::thread::sleep(Duration::from_secs(10));
        return;
    }

    let executable = std::env::current_exe().expect("test executable");
    let mut child = std::process::Command::new(executable)
        .args(["--exact", TEST_NAME, "--nocapture"])
        .env(HELPER_ENV, "1")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn helper");

    let mut child_stdout = child.stdout.take().expect("helper stdout");
    let (sender, receiver) = std::sync::mpsc::channel::<Vec<u8>>();
    let reader = std::thread::spawn(move || {
        use std::io::Read as _;
        let mut buffer = [0u8; 4096];
        loop {
            match child_stdout.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    if sender.send(buffer[..read].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });

    let expected = osc52("flush-probe");
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut received: Vec<u8> = Vec::new();
    let mut found = false;
    while !found {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match receiver.recv_timeout(remaining) {
            Ok(chunk) => {
                received.extend_from_slice(&chunk);
                found = received
                    .windows(expected.len())
                    .any(|window| window == expected.as_bytes());
            }
            Err(_) => break,
        }
    }

    let _ = child.kill();
    let _ = child.wait();
    drop(receiver);
    let _ = reader.join();

    assert!(
        found,
        "OSC 52 sequence did not reach stdout while the writer was alive (buffered?); received {:?}",
        String::from_utf8_lossy(&received)
    );
}

// ---------------------------------------------------------------------------
// openBrowser
// ---------------------------------------------------------------------------

#[test]
fn open_browser_uses_the_platform_launcher_without_a_shell() {
    let captured = Arc::new(Mutex::new(Vec::<(String, Vec<String>)>::new()));
    let cases: [(&str, &str, &[&str]); 3] = [
        ("darwin", "open", &["https://example.com"]),
        (
            "win32",
            "rundll32",
            &["url.dll,FileProtocolHandler", "https://example.com"],
        ),
        ("linux", "xdg-open", &["https://example.com"]),
    ];
    for (platform, expected_command, expected_args) in cases {
        captured.lock().expect("captured").clear();
        let target = Arc::clone(&captured);
        open_browser_with("https://example.com", platform, &move |command, args| {
            target
                .lock()
                .expect("captured")
                .push((command.to_string(), args.to_vec()));
        });
        let calls = captured.lock().expect("captured").clone();
        assert_eq!(calls.len(), 1, "{platform}");
        assert_eq!(calls[0].0, expected_command);
        assert_eq!(calls[0].1, expected_args);
    }
}
