//! `test/native-clipboard-linux.test.ts` 的 X11 集成部分。
//!
//! TS 用 `Xvfb` + `xclip` + 两个 C fixture（`clipboard-x11-test.c` 的 selection owner 与
//! `clipboard-worker-test.c` 的 worker 探针）；Rust 侧等价物是**进程内的 x11rb selection
//! owner**，因此只需要一个可用的 X server（`DISPLAY`），不需要 `cc`/`xclip`/`pkg-config`。
//!
//! 元数据校验、50 MiB 上限、worker 忙/3 秒 deadline、丢弃未 poll 的 future 这些不依赖 server
//! 的用例在 `src/native/linux_x11.rs` 的单元测试里（对应 C fixture 的 `metadata` 模式与
//! `clipboard-worker-test.cjs`）。
//!
//! display 的获取顺序见 [`XServer::start`]：显式 opt-in 的隔离 `DISPLAY` → 私有 `Xvfb` →
//! 跳过（与 TS 在 `Xvfb` 缺失时的 `skip` 一致）。
//!
//! TS 的 `process-exit`/`worker-exit` 两个 worker 生命周期模式与两个 allocation-accounting
//! 用例针对的是 C 的 `pthread` worker + `clipboard_alloc`：Rust 侧没有手写分配器，worker 线程
//! 随进程结束由内核回收，等价的“泄漏”风险是**未 poll 的 future 永久占住 worker**，已由
//! `linux_x11.rs::tests::dropping_an_unpolled_read_releases_the_worker` 覆盖。

#![cfg(all(target_os = "linux", feature = "x11"))]
// 串行化用的 `std::sync::Mutex` 必须跨越 await 持有：每个测试有独立的 current-thread
// runtime，锁只用来阻止同一二进制里的其它测试抢占 X11 的 `CLIPBOARD` selection。
#![allow(clippy::await_holding_lock)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use kiana_tui::native::linux_x11::LinuxX11Clipboard;
use kiana_tui::native::{ClipboardValue, NativeClipboard};
use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ChangeWindowAttributesAux, ConnectionExt as _, CreateWindowAux, EventMask,
    PropMode, Property, SelectionNotifyEvent, WindowClass,
};
use x11rb::rust_connection::RustConnection;

/// X server 上的 `CLIPBOARD` selection 是全局资源：同一测试二进制内必须串行。
static X11_LOCK: Mutex<()> = Mutex::new(());

fn x11_lock() -> MutexGuard<'static, ()> {
    X11_LOCK.lock().unwrap_or_else(|poison| poison.into_inner())
}

const XCB_NONE: Atom = 0;

/// 进程内 selection owner 的行为（对应 `clipboard-x11-test.c` 的各个 argv 模式）。
enum Mode {
    /// TARGETS 只给一个 target，直接返回内容。
    Direct {
        target: &'static str,
        bytes: Vec<u8>,
    },
    /// 拒绝 TARGETS（`direct-text`）：仍直接提供 UTF8_STRING。
    RefuseTargets { bytes: Vec<u8> },
    /// 只提供 UTF8_STRING（`text only` 的剪贴板）。
    TextOnly { bytes: Vec<u8> },
    /// INCR 分块传输（`clipboard-x11-test.c` 的默认分支）。
    Incr {
        target: &'static str,
        bytes: Vec<u8>,
        chunk: usize,
    },
    /// 收到请求后永不回复（`idle`）：触发 2 秒 transfer deadline。
    Stall,
    /// INCR 的第一块正常，终止块换成不兼容的 `type`（`invalid`）：追加元数据校验失败。
    Invalid {
        target: &'static str,
        bytes: Vec<u8>,
    },
    /// 只发第一块 INCR 就停住（不响应后续 DELETE，也没有终止块）：部分传输后 2 秒 deadline。
    Partial {
        target: &'static str,
        bytes: Vec<u8>,
    },
}

struct IncrTransfer {
    requestor: u32,
    property: Atom,
    chunks: Vec<Vec<u8>>,
    next: usize,
}

/// selection owner 的句柄：Drop 时让线程退出并回收。
struct Owner {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Owner {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Owner {
    fn start(mode: Mode) -> Option<Owner> {
        let (ready, ready_rx) = std::sync::mpsc::channel::<bool>();
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let handle = std::thread::spawn(move || run_owner(mode, thread_stop, ready));
        match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(true) => Some(Owner {
                stop,
                handle: Some(handle),
            }),
            _ => {
                stop.store(true, Ordering::SeqCst);
                let _ = handle.join();
                None
            }
        }
    }
}

/// TS 的 `skip: !dependenciesAvailable`（`cc`/`Xvfb`/`xclip`/`pkg-config --exists xcb`）。
///
/// Rust 侧不需要 `cc`/`xclip`/`pkg-config`（selection owner 在进程内），只需要一个**隔离**的
/// X server。按 TS 的语义取 display：
///
/// 1. `KIANA_TEST_X11_CLIPBOARD=1` + 可连接的 `DISPLAY`：使用调用方准备好的隔离 display（本地用
///    私有 `Xephyr`/`Xvfb` 手动跑套件时用）；
/// 2. 否则尝试启动私有 `Xvfb`（`-displayfd 1`：让 Xvfb 把选中的 display 号写到 stdout，便于拿到可用 display）；每个
///    测试一个，Drop 时杀掉并恢复 `DISPLAY`；
/// 3. 都没有 → 跳过（`Xvfb` 缺失时 TS 同样 skip）。
///
/// **不**直接使用调用方桌面的 `DISPLAY`：那会抢走真实剪贴板的 `CLIPBOARD` selection。
struct XServer {
    child: Option<std::process::Child>,
    previous_display: Option<std::ffi::OsString>,
}

/// 恢复 `DISPLAY`（`None` → 删除）。
///
/// # Safety
/// 调用方必须持有 [`X11_LOCK`]（`DISPLAY` 是进程级环境变量，所有读写都在这把锁下串行）。
unsafe fn restore_display(value: &Option<std::ffi::OsString>) {
    match value {
        Some(value) => unsafe { std::env::set_var("DISPLAY", value) },
        None => unsafe { std::env::remove_var("DISPLAY") },
    }
}

impl XServer {
    fn start() -> Option<XServer> {
        let server = Self::start_inner();
        if server.is_none() {
            // libtest 没有条件 skip：至少把原因打出来，避免“静默通过”。
            static ONCE: std::sync::Once = std::sync::Once::new();
            ONCE.call_once(|| {
                eprintln!(
                    "native_clipboard_linux: skipping X11 tests (需要 Xvfb，或 KIANA_TEST_X11_CLIPBOARD=1 + 隔离的 DISPLAY)"
                );
            });
        }
        server
    }

    fn start_inner() -> Option<XServer> {
        if std::env::var_os("KIANA_TEST_X11_CLIPBOARD").as_deref()
            == Some(std::ffi::OsStr::new("1"))
            && std::env::var_os("DISPLAY").is_some()
            && x11rb::connect(None).is_ok()
        {
            return Some(XServer {
                child: None,
                previous_display: None,
            });
        }

        let mut child = std::process::Command::new("Xvfb")
            .args([
                "-displayfd",
                "1",
                "-screen",
                "0",
                "640x480x24",
                "-nolisten",
                "tcp",
            ])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .ok()?;
        let stdout = child.stdout.take()?;
        let (sender, receiver) = std::sync::mpsc::channel::<String>();
        // `-displayfd 1`：server 就绪后把分到的 display 号写成一行。读线程随 Xvfb 退出结束。
        std::thread::spawn(move || {
            use std::io::BufRead as _;
            let mut line = String::new();
            let _ = std::io::BufReader::new(stdout).read_line(&mut line);
            let _ = sender.send(line);
        });
        let Ok(line) = receiver.recv_timeout(Duration::from_secs(10)) else {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        };
        let number = line.trim();
        if number.is_empty() {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        let display = format!(":{number}");
        let previous_display = std::env::var_os("DISPLAY");
        // SAFETY: 调用方持有 X11_LOCK。
        unsafe { std::env::set_var("DISPLAY", &display) };
        if x11rb::connect(None).is_err() {
            // SAFETY: 调用方持有 X11_LOCK。
            unsafe { restore_display(&previous_display) };
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        Some(XServer {
            child: Some(child),
            previous_display,
        })
    }
}

impl Drop for XServer {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
            // SAFETY: 调用方持有 X11_LOCK（`_lock` 在 `_server` 之前声明，Drop 顺序保证）。
            unsafe { restore_display(&self.previous_display) };
        }
    }
}

fn run_owner(mode: Mode, stop: Arc<AtomicBool>, ready: std::sync::mpsc::Sender<bool>) {
    let connected = x11rb::connect(None);
    let Ok((connection, screen_num)) = connected else {
        let _ = ready.send(false);
        return;
    };
    let root = connection.setup().roots[screen_num].root;
    let Ok(window) = connection.generate_id() else {
        let _ = ready.send(false);
        return;
    };
    let created = connection
        .create_window(
            x11rb::COPY_FROM_PARENT as u8,
            window,
            root,
            0,
            0,
            1,
            1,
            0,
            WindowClass::INPUT_OUTPUT,
            x11rb::COPY_FROM_PARENT,
            &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
        )
        .is_ok();
    let atoms = AtomSet::intern(&connection);
    let ready_ok = created
        && atoms.is_some()
        && connection
            .set_selection_owner(
                window,
                atoms.as_ref().expect("atoms").clipboard,
                x11rb::CURRENT_TIME,
            )
            .is_ok()
        && connection.flush().is_ok();
    let Some(atoms) = atoms else {
        let _ = ready.send(false);
        return;
    };
    if !ready_ok {
        let _ = ready.send(false);
        return;
    }
    if ready.send(true).is_err() {
        return;
    }

    let mut incr: Option<IncrTransfer> = None;
    while !stop.load(Ordering::SeqCst) {
        if let Ok(Some(event)) = connection.poll_for_event() {
            match event {
                Event::SelectionRequest(request) => {
                    handle_selection_request(&connection, &atoms, &mode, &mut incr, &request);
                }
                Event::PropertyNotify(notify) => {
                    let matches = incr.as_ref().is_some_and(|state| {
                        state.requestor == notify.window && state.property == notify.atom
                    });
                    if matches && notify.state == Property::DELETE {
                        send_next_incr_chunk(&connection, &atoms, &mode, &mut incr);
                    }
                }
                _ => {}
            }
        }
        let _ = connection.flush();
        std::thread::sleep(Duration::from_millis(2));
    }
}

struct AtomSet {
    clipboard: Atom,
    targets: Atom,
    incr: Atom,
    utf8: Atom,
    string: Atom,
    image_png: Atom,
}

impl AtomSet {
    fn intern(connection: &RustConnection) -> Option<AtomSet> {
        let atom = |name: &str| -> Option<Atom> {
            connection
                .intern_atom(false, name.as_bytes())
                .ok()?
                .reply()
                .ok()
                .map(|reply| reply.atom)
        };
        let atoms = AtomSet {
            clipboard: atom("CLIPBOARD")?,
            targets: atom("TARGETS")?,
            incr: atom("INCR")?,
            utf8: atom("UTF8_STRING")?,
            string: atom("STRING")?,
            image_png: atom("image/png")?,
        };
        if [
            atoms.clipboard,
            atoms.targets,
            atoms.incr,
            atoms.utf8,
            atoms.string,
            atoms.image_png,
        ]
        .contains(&XCB_NONE)
        {
            return None;
        }
        Some(atoms)
    }
}

fn target_atom(atoms: &AtomSet, name: &str) -> Atom {
    match name {
        "STRING" => atoms.string,
        "image/png" => atoms.image_png,
        _ => atoms.utf8,
    }
}

fn notify(
    connection: &RustConnection,
    request: &x11rb::protocol::xproto::SelectionRequestEvent,
    property: Atom,
) {
    let event = SelectionNotifyEvent {
        response_type: x11rb::protocol::xproto::SELECTION_NOTIFY_EVENT,
        sequence: 0,
        time: request.time,
        requestor: request.requestor,
        selection: request.selection,
        target: request.target,
        property,
    };
    let _ = connection.send_event(false, request.requestor, EventMask::NO_EVENT, event);
}

/// `ChangeProperty`：`data` 是 X 期望的原始字节；`format == 32` 时每 4 字节一个单元。
fn change_property(
    connection: &RustConnection,
    requestor: u32,
    property: Atom,
    type_: Atom,
    format: u8,
    data: &[u8],
) {
    let data_len = if format == 32 {
        (data.len() / 4) as u32
    } else {
        data.len() as u32
    };
    let _ = connection.change_property(
        PropMode::REPLACE,
        requestor,
        property,
        type_,
        format,
        data_len,
        data,
    );
}

/// TARGETS 回复：`ATOM[]`。
fn set_targets(connection: &RustConnection, requestor: u32, property: Atom, targets: &[Atom]) {
    let mut data = Vec::with_capacity(targets.len() * 4);
    for target in targets {
        data.extend_from_slice(&target.to_ne_bytes());
    }
    change_property(
        connection,
        requestor,
        property,
        u32::from(AtomEnum::ATOM),
        32,
        &data,
    );
}

/// 文本回复：`format = 8`。
fn set_text(
    connection: &RustConnection,
    requestor: u32,
    property: Atom,
    type_: Atom,
    bytes: &[u8],
) {
    change_property(connection, requestor, property, type_, 8, bytes);
}

fn handle_selection_request(
    connection: &RustConnection,
    atoms: &AtomSet,
    mode: &Mode,
    incr: &mut Option<IncrTransfer>,
    request: &x11rb::protocol::xproto::SelectionRequestEvent,
) {
    if request.selection != atoms.clipboard {
        notify(connection, request, XCB_NONE);
        return;
    }
    let mut property = request.property;

    match mode {
        Mode::Stall => return,
        Mode::RefuseTargets { bytes } => {
            if request.target == atoms.utf8 {
                set_text(connection, request.requestor, property, atoms.utf8, bytes);
            } else {
                property = XCB_NONE;
            }
        }
        Mode::Direct { target, bytes } => {
            let wanted = target_atom(atoms, target);
            if request.target == atoms.targets {
                set_targets(connection, request.requestor, property, &[wanted]);
            } else if request.target == wanted {
                set_text(connection, request.requestor, property, wanted, bytes);
            } else {
                property = XCB_NONE;
            }
        }
        Mode::TextOnly { bytes } => {
            if request.target == atoms.targets {
                set_targets(connection, request.requestor, property, &[atoms.utf8]);
            } else if request.target == atoms.utf8 {
                set_text(connection, request.requestor, property, atoms.utf8, bytes);
            } else {
                property = XCB_NONE;
            }
        }
        Mode::Incr {
            target,
            bytes,
            chunk,
        } => {
            reply_incr(
                connection, atoms, incr, request, property, target, bytes, *chunk,
            );
            if request.target != atoms.targets && request.target != target_atom(atoms, target) {
                property = XCB_NONE;
            }
        }
        Mode::Invalid { target, bytes } | Mode::Partial { target, bytes } => {
            reply_incr(
                connection,
                atoms,
                incr,
                request,
                property,
                target,
                bytes,
                bytes.len().max(1),
            );
            if request.target != atoms.targets && request.target != target_atom(atoms, target) {
                property = XCB_NONE;
            }
        }
    }

    notify(connection, request, property);
}

/// INCR 回复的公共前半段：TARGETS 回复或 INCR 标记 + 分块。
#[allow(
    clippy::too_many_arguments,
    reason = "测试 fixture：参数与 x11rb 的 SelectionRequest 字段一一对应，拆结构体反而更难对照 C 源"
)]
fn reply_incr(
    connection: &RustConnection,
    atoms: &AtomSet,
    incr: &mut Option<IncrTransfer>,
    request: &x11rb::protocol::xproto::SelectionRequestEvent,
    property: Atom,
    target: &str,
    bytes: &[u8],
    chunk: usize,
) {
    let wanted = target_atom(atoms, target);
    if request.target == atoms.targets {
        set_targets(connection, request.requestor, property, &[wanted]);
        return;
    }
    if request.target != wanted {
        return;
    }
    let _ = connection.change_window_attributes(
        request.requestor,
        &ChangeWindowAttributesAux::new().event_mask(EventMask::PROPERTY_CHANGE),
    );
    // INCR 标记：type=INCR、format=32、空值。
    change_property(connection, request.requestor, property, atoms.incr, 32, &[]);
    let chunks: Vec<Vec<u8>> = bytes
        .chunks(chunk.max(1))
        .map(|part| part.to_vec())
        .collect();
    *incr = Some(IncrTransfer {
        requestor: request.requestor,
        property,
        chunks,
        next: 0,
    });
}

/// 收到 requestor 的 DELETE（它读走上一块）后写下一块；最后一块为空（终止符）。
fn send_next_incr_chunk(
    connection: &RustConnection,
    atoms: &AtomSet,
    mode: &Mode,
    incr: &mut Option<IncrTransfer>,
) {
    let Some(state) = incr.as_mut() else { return };
    let Some(chunk) = state.chunks.get(state.next) else {
        // 终止块：`invalid` 模式故意换 type，让追加元数据校验失败。
        let type_ = match mode {
            Mode::Invalid { .. } => atoms.string,
            _ => atoms.utf8,
        };
        change_property(connection, state.requestor, state.property, type_, 8, &[]);
        *incr = None;
        return;
    };
    change_property(
        connection,
        state.requestor,
        state.property,
        atoms.utf8,
        8,
        chunk,
    );
    state.next += 1;
    if matches!(mode, Mode::Partial { .. }) {
        // 只发第一块：之后既不响应 DELETE 也不发终止块 → 客户端等满 2 秒 deadline。
        *incr = None;
    } else if state.next > state.chunks.len() {
        *incr = None;
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

/// `decodes X11 STRING as Latin-1`
#[tokio::test]
async fn decodes_x11_string_as_latin1() {
    let _lock = x11_lock();
    let Some(_server) = XServer::start() else {
        return;
    };
    let Some(_owner) = Owner::start(Mode::Direct {
        target: "STRING",
        bytes: b"caf\xe9 \xa3\xff".to_vec(),
    }) else {
        return;
    };

    let value = LinuxX11Clipboard::new()
        .get_text()
        .await
        .expect("Latin-1 text read");
    assert_eq!(value, ClipboardValue::Value("café £ÿ".to_string()));
}

/// `reads UTF8_STRING when X11 TARGETS is refused`
#[tokio::test]
async fn reads_utf8_string_when_x11_targets_is_refused() {
    let _lock = x11_lock();
    let Some(_server) = XServer::start() else {
        return;
    };
    let Some(_owner) = Owner::start(Mode::RefuseTargets {
        bytes: "café 日本語".as_bytes().to_vec(),
    }) else {
        return;
    };

    let value = LinuxX11Clipboard::new()
        .get_text()
        .await
        .expect("direct UTF8_STRING read");
    assert_eq!(value, ClipboardValue::Value("café 日本語".to_string()));
}

/// `returns null for empty X11 clipboards and text-only selections`
#[tokio::test]
async fn returns_null_for_empty_x11_clipboards_and_text_only_selections() {
    let _lock = x11_lock();
    let Some(_server) = XServer::start() else {
        return;
    };

    // 没有 owner：SelectionNotify 的 property 为 None。
    let clipboard = LinuxX11Clipboard::new();
    assert_eq!(
        clipboard.get_text().await.expect("empty text read"),
        ClipboardValue::Empty
    );
    assert_eq!(
        clipboard.get_image().await.expect("empty image read"),
        ClipboardValue::Empty
    );

    // 只有文本的 selection：图片读取是 `null`，不是错误。
    let Some(_owner) = Owner::start(Mode::TextOnly {
        bytes: b"text only".to_vec(),
    }) else {
        return;
    };
    assert_eq!(
        LinuxX11Clipboard::new()
            .get_image()
            .await
            .expect("text-only image read"),
        ClipboardValue::Empty
    );
}

/// `preserves X11 Unicode and incremental text/image transfers`
#[tokio::test]
async fn preserves_x11_unicode_and_incremental_text_image_transfers() {
    let _lock = x11_lock();
    let Some(_server) = XServer::start() else {
        return;
    };

    // Unicode 文本（UTF8_STRING）。
    let unicode = "café 日本語".as_bytes().to_vec();
    let Some(_owner) = Owner::start(Mode::Direct {
        target: "UTF8_STRING",
        bytes: unicode.clone(),
    }) else {
        return;
    };
    assert_eq!(
        LinuxX11Clipboard::new()
            .get_text()
            .await
            .expect("unicode text read"),
        ClipboardValue::Value("café 日本語".to_string())
    );
    drop(_owner);

    // 4 MiB INCR 文本（与 TS 一样用 0x78 填充）。
    let large_text = vec![b'x'; 4 * 1024 * 1024];
    let Some(_owner) = Owner::start(Mode::Incr {
        target: "UTF8_STRING",
        bytes: large_text.clone(),
        chunk: 64 * 1024,
    }) else {
        return;
    };
    let value = LinuxX11Clipboard::new()
        .get_text()
        .await
        .expect("incremental text read");
    match value {
        ClipboardValue::Value(text) => {
            assert_eq!(text.len(), large_text.len());
            assert!(text.bytes().all(|byte| byte == b'x'));
        }
        other => panic!("expected the incremental text, got {other:?}"),
    }
    drop(_owner);

    // 4 MiB INCR 图片。
    let large_image = vec![123u8; 4 * 1024 * 1024];
    let Some(_owner) = Owner::start(Mode::Incr {
        target: "image/png",
        bytes: large_image.clone(),
        chunk: 64 * 1024,
    }) else {
        return;
    };
    assert_eq!(
        LinuxX11Clipboard::new()
            .get_image()
            .await
            .expect("incremental image read"),
        ClipboardValue::Value(large_image)
    );
}

/// `reports X11 transfer timeout as an exception, not an unavailable display`
#[tokio::test]
async fn reports_x11_transfer_timeout_as_an_exception_not_an_unavailable_display() {
    let _lock = x11_lock();
    let Some(_server) = XServer::start() else {
        return;
    };
    let Some(_owner) = Owner::start(Mode::Stall) else {
        return;
    };

    let started = Instant::now();
    let error = LinuxX11Clipboard::new()
        .get_text()
        .await
        .expect_err("a stalled owner must reject");
    assert!(
        error.message.contains("X11 clipboard"),
        "unexpected error: {error}"
    );
    assert!(
        started.elapsed() < Duration::from_millis(3500),
        "the transfer must share the 2-second deadline, took {:?}",
        started.elapsed()
    );
}

/// `keeps JS responsive during stalled X11 connection setup`（`getText`/`getImage` 两个 it）
///
/// 不需要真实 X server：监听一个**永不回复 setup** 的 TCP 端口，`DISPLAY=127.0.0.1:<n>`
/// （连接端口 `6000 + n`）。私有工作线程在 setup 上阻塞，调用方最多等 3 秒；服务器 300 ms 后
/// 断开连接时，私有线程失败并给出 `Unavailable`（与 TS 的 `{ ok: true, unavailable: true }`
/// 一致），而不是拒绝。
#[tokio::test]
async fn keeps_the_caller_responsive_during_stalled_connection_setup() {
    use std::net::TcpListener;

    let _lock = x11_lock();
    // 找一个可以当作 X display 的端口（6000 + n，n >= 1）。
    let mut listener = None;
    for display in 60..200u16 {
        if let Ok(bound) = TcpListener::bind(("127.0.0.1", 6000 + display)) {
            listener = Some((display, bound));
            break;
        }
    }
    let Some((display, listener)) = listener else {
        return;
    };
    let address = format!("127.0.0.1:{display}");
    listener
        .set_nonblocking(true)
        .expect("nonblocking listener");

    // 接受连接但从不发送 X setup 回复；300 ms 后关闭，模拟 TS 的 server.destroy()。
    let accepted = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let connections = std::sync::Arc::clone(&accepted);
    let server = std::thread::spawn(move || {
        let started = Instant::now();
        let mut sockets = Vec::new();
        while started.elapsed() < Duration::from_millis(300) {
            match listener.accept() {
                Ok((socket, _)) => {
                    connections.fetch_add(1, Ordering::SeqCst);
                    sockets.push(socket);
                }
                Err(ref error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(_) => break,
            }
        }
        // 关闭所有连接：阻塞中的 connect/setup 失败。
        drop(sockets);
    });

    let previous_display = std::env::var_os("DISPLAY");
    // SAFETY: 所有 DISPLAY 读写都在 X11_LOCK 下串行。
    unsafe { std::env::set_var("DISPLAY", &address) };
    let started = Instant::now();
    let text = LinuxX11Clipboard::new().get_text().await;
    let image = LinuxX11Clipboard::new().get_image().await;
    let elapsed = started.elapsed();
    unsafe {
        match previous_display {
            Some(value) => std::env::set_var("DISPLAY", value),
            None => std::env::remove_var("DISPLAY"),
        }
    }
    let _ = server.join();

    assert_eq!(text, Ok(ClipboardValue::Unavailable));
    assert_eq!(image, Ok(ClipboardValue::Unavailable));
    assert!(
        elapsed < Duration::from_millis(3500),
        "each read keeps the 3-second worker bound, took {elapsed:?}"
    );
    assert!(
        accepted.load(Ordering::SeqCst) > 0,
        "the helper must reach the stalled X11 server"
    );
}

/// `reports X11 transfer disconnect as an exception, not an unavailable display`（元数据部分）
///
/// TS 的 `invalid` 模式 + `frees partially received X11 ... after invalid metadata`：INCR 终止块
/// 的元数据与已累积内容不一致时，读取必须 reject（`Could not read X11 clipboard`），而不是把
/// 不完整内容当成 `Empty`。Rust 侧没有 C 的 `clipboard_free` 可观察，等价断言是错误路径。
#[tokio::test]
async fn rejects_invalid_x11_incremental_metadata() {
    let _lock = x11_lock();
    let Some(_server) = XServer::start() else {
        return;
    };
    let Some(_owner) = Owner::start(Mode::Invalid {
        target: "UTF8_STRING",
        bytes: b"partial ".to_vec(),
    }) else {
        return;
    };

    let started = Instant::now();
    let error = LinuxX11Clipboard::new()
        .get_text()
        .await
        .expect_err("invalid metadata must reject");
    assert!(
        error.message.contains("X11 clipboard"),
        "unexpected error: {error}"
    );
    assert!(
        started.elapsed() < Duration::from_millis(3500),
        "the transfer must share the 2-second deadline, took {:?}",
        started.elapsed()
    );
}

/// `times out stalled X11 text after a partial transfer without blocking JS`
#[tokio::test]
async fn times_out_stalled_x11_text_after_a_partial_transfer() {
    let _lock = x11_lock();
    let Some(_server) = XServer::start() else {
        return;
    };
    let Some(_owner) = Owner::start(Mode::Partial {
        target: "UTF8_STRING",
        bytes: vec![b'x'; 64 * 1024],
    }) else {
        return;
    };

    let started = Instant::now();
    let error = LinuxX11Clipboard::new()
        .get_text()
        .await
        .expect_err("a partially transferred clipboard must reject");
    assert!(
        error.message.contains("X11 clipboard"),
        "unexpected error: {error}"
    );
    assert!(
        started.elapsed() < Duration::from_millis(3500),
        "the partial transfer must share the 2-second deadline, took {:?}",
        started.elapsed()
    );
}

/// `times out stalled X11 image after a partial transfer without blocking JS`
#[tokio::test]
async fn times_out_stalled_x11_image_after_a_partial_transfer() {
    let _lock = x11_lock();
    let Some(_server) = XServer::start() else {
        return;
    };
    let Some(_owner) = Owner::start(Mode::Partial {
        target: "image/png",
        bytes: vec![123u8; 64 * 1024],
    }) else {
        return;
    };

    let started = Instant::now();
    let error = LinuxX11Clipboard::new()
        .get_image()
        .await
        .expect_err("a partially transferred clipboard must reject");
    assert!(
        error.message.contains("X11 clipboard"),
        "unexpected error: {error}"
    );
    assert!(
        started.elapsed() < Duration::from_millis(3500),
        "the partial transfer must share the 2-second deadline, took {:?}",
        started.elapsed()
    );
}

/// `bounds private clipboard work and preserves cleanup: concurrent`
///
/// 私有 worker 忙时其它调用**立即**返回 `Unavailable`（`worker_busy`），不排队、不阻塞。
#[tokio::test]
async fn a_second_read_returns_unavailable_while_the_worker_is_busy() {
    let _lock = x11_lock();
    let Some(_server) = XServer::start() else {
        return;
    };
    let Some(_owner) = Owner::start(Mode::Stall) else {
        return;
    };

    let clipboard = LinuxX11Clipboard::new();
    // 第一个调用占住私有 worker（owner 永不回复，直到 2 秒 deadline）。
    let first = clipboard.get_text();
    let started = Instant::now();
    let second = clipboard
        .get_text()
        .await
        .expect("a busy worker reports undefined");
    assert_eq!(second, ClipboardValue::Unavailable);
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "a busy worker must return immediately, took {:?}",
        started.elapsed()
    );

    // 第一个调用最终以传输错误结束（不会被 busy 状态吞掉）。
    assert!(first.await.is_err());
}
