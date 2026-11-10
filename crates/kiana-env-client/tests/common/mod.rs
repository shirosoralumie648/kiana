//! `packages/env/test/daemon.ts` 与 `old-daemon.ts` 的移植，以及测试共用的临时目录。
//!
//! 与 TS 的差异：
//! - daemon 路径：`KIANA_ENV_DAEMON`，否则 `<target>/debug/kiana-env`（`CARGO_TARGET_DIR` 优先，
//!   与 `cargo build -p kiana-env` 的产物一致）；不存在时 panic
//!   `Build the daemon first: cargo build -p kiana-env (missing {path})`。
//! - 旧 daemon：TS 用 `node old-daemon.ts`；Rust 没有独立脚本，改为重新执行**本测试二进制**的
//!   `old_daemon_child` 用例（`--exact common::old_daemon_child --nocapture`），token 经环境变量传入。
//!   在 `sh` 无法正确转发的平台（Windows）上，相关用例跳过。

#![allow(dead_code)]

use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::Arc;

use kiana_chord::context::{
    AbortSignal, BACKGROUND_CONTEXT, Context, ContextRef, with_abort_signal,
};
use kiana_env_client::Connection;

/// 旧 daemon 子进程的标记变量。
pub const OLD_DAEMON_CHILD_ENV: &str = "KIANA_ENV_OLD_DAEMON_CHILD";
/// 旧 daemon 子进程的 token 变量。
pub const OLD_DAEMON_TOKEN_ENV: &str = "KIANA_ENV_OLD_DAEMON_TOKEN";

/// 测试使用的 daemon 二进制路径（`KIANA_ENV_DAEMON` 优先）。
pub fn daemon_path() -> String {
    if let Some(path) = std::env::var_os("KIANA_ENV_DAEMON") {
        let path = path.to_string_lossy().into_owned();
        if !path.is_empty() {
            return path;
        }
    }
    let target = match std::env::var_os("CARGO_TARGET_DIR") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => workspace_root().join("target"),
    };
    let name = if cfg!(windows) {
        "kiana-env.exe"
    } else {
        "kiana-env"
    };
    let path = target.join("debug").join(name);
    if !path.exists() {
        panic!(
            "Build the daemon first: cargo build -p kiana-env (missing {})",
            path.display()
        );
    }
    path.to_string_lossy().into_owned()
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|path| path.parent())
        .map(|path| path.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
}

/// 新连接（固定 daemon 命令）。
pub fn connection() -> Arc<Connection> {
    Arc::new(Connection::new(vec![daemon_path()]))
}

/// `BACKGROUND_CONTEXT`。
pub fn context() -> &'static dyn Context {
    &**BACKGROUND_CONTEXT
}

/// `withAbortSignal(signal, BACKGROUND_CONTEXT)`。
pub fn context_with_signal(signal: &AbortSignal) -> ContextRef {
    with_abort_signal(signal.clone(), BACKGROUND_CONTEXT.clone())
}

/// `process.platform` 的拼写（Rust 的 OS 名映射到 Node 的名字）。
pub fn node_process_platform() -> &'static str {
    match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    }
}

/// `process.arch` 的拼写。
pub fn node_process_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        "x86" => "ia32",
        other => other,
    }
}

/// 一个临时目录，drop 时递归删除。
pub struct TempDir {
    path: PathBuf,
    _inner: Option<tempfile::TempDir>,
}

impl TempDir {
    /// `mkdtempSync(join(tmpdir(), prefix))`。
    pub fn new(prefix: &str) -> Self {
        let inner = tempfile::Builder::new()
            .prefix(prefix)
            .tempdir()
            .expect("create a temporary directory");
        TempDir {
            path: inner.path().to_path_buf(),
            _inner: Some(inner),
        }
    }

    /// 已存在的目录（例如 `mkdtemp` 之后又被 `canonicalize` 的根）。
    pub fn from_path(path: PathBuf) -> Self {
        TempDir { path, _inner: None }
    }

    /// 目录路径。
    pub fn path(&self) -> &str {
        self.path.to_str().expect("the temporary path is UTF-8")
    }

    /// 目录下的路径。
    pub fn join(&self, name: &str) -> String {
        self.path.join(name).to_string_lossy().into_owned()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        if let Some(inner) = self._inner.take() {
            drop(inner);
        } else {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

/// 旧 daemon 的启动命令（`sh` 包装：把 `serve --token <hex>` 的 token 放进环境变量）。
///
/// `Connection` 会在命令末尾追加 `serve --token <hex>`；包装脚本的 `$4` 就是 token。
pub fn old_daemon_command() -> Vec<String> {
    let executable = std::env::current_exe()
        .expect("the test binary path")
        .to_string_lossy()
        .into_owned();
    vec![
        "sh".to_owned(),
        "-c".to_owned(),
        format!(
            "{OLD_DAEMON_CHILD_ENV}=1 {OLD_DAEMON_TOKEN_ENV}=\"$4\" exec \"$1\" --exact common::old_daemon_child --nocapture"
        ),
        "sh".to_owned(),
        executable,
    ]
}

/// 旧 daemon 是否可用（需要 `sh` 包装）。
pub fn old_daemon_supported() -> bool {
    cfg!(unix)
}

/// 重新执行本二进制时运行它：模拟 `hello.features` 之前发布的 daemon。
///
/// 未设置标记变量时立即返回（普通 `cargo test` 也会运行这个用例）。
#[test]
fn old_daemon_child() {
    if std::env::var_os(OLD_DAEMON_CHILD_ENV).is_none() {
        return;
    }
    let token = std::env::var(OLD_DAEMON_TOKEN_ENV).unwrap_or_default();
    serve_old_daemon(&token);
    // Never return to libtest: its report would follow the frame stream.
    std::process::exit(0);
}

fn serve_old_daemon(token: &str) {
    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();
    let _ = writeln!(stdout, "KIANA-ENV {token}");
    let _ = stdout.flush();
    let stdin = std::io::stdin();
    let mut stdin = stdin.lock();
    loop {
        let mut header = [0u8; 4];
        if stdin.read_exact(&mut header).is_err() {
            return;
        }
        let length = u32::from_be_bytes(header) as usize;
        if length < 9 {
            return;
        }
        let mut body = vec![0u8; length];
        if stdin.read_exact(&mut body).is_err() {
            return;
        }
        let kind = body[0];
        let id = u32::from_be_bytes([body[1], body[2], body[3], body[4]]);
        let json_length = u32::from_be_bytes([body[5], body[6], body[7], body[8]]) as usize;
        if 9 + json_length > body.len() {
            return;
        }
        let json: serde_json::Value =
            serde_json::from_slice(&body[9..9 + json_length]).unwrap_or_default();
        if kind != 1 {
            continue;
        }
        let op = json
            .get("op")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        if op == "hello" {
            let reply = serde_json::json!({
                "protocol": 1,
                "version": "1.1.0",
                "os": "linux",
                "arch": "x86_64",
                "home": "/",
                "separator": "/",
                "tmpdir": "/tmp",
                "cwd": "/",
                "driveCwds": {},
                "pid": std::process::id(),
            });
            send_old_frame(&mut stdout, 2, id, &reply);
        } else {
            let reply = serde_json::json!({
                "code": "EINVAL",
                "message": format!("unknown operation {op}"),
            });
            send_old_frame(&mut stdout, 3, id, &reply);
        }
    }
}

fn send_old_frame(stdout: &mut impl Write, kind: u8, id: u32, json: &serde_json::Value) {
    let body = serde_json::to_vec(json).expect("the reply serializes");
    let mut bytes = Vec::with_capacity(13 + body.len());
    bytes.extend_from_slice(&((9 + body.len()) as u32).to_be_bytes());
    bytes.push(kind);
    bytes.extend_from_slice(&id.to_be_bytes());
    bytes.extend_from_slice(&(body.len() as u32).to_be_bytes());
    bytes.extend_from_slice(&body);
    let _ = stdout.write_all(&bytes);
    let _ = stdout.flush();
}

// ---------------------------------------------------------------------------
// 脚本 daemon：记录收到的帧，可延迟回复、可省略 ready/started
// ---------------------------------------------------------------------------

/// 脚本 daemon 子进程的标记变量。
pub const FAKE_DAEMON_CHILD_ENV: &str = "KIANA_ENV_FAKE_DAEMON_CHILD";
/// 脚本 daemon 子进程的 token 变量。
pub const FAKE_DAEMON_TOKEN_ENV: &str = "KIANA_ENV_FAKE_DAEMON_TOKEN";
/// 脚本 daemon 子进程的配置（JSON 文件路径）变量。
pub const FAKE_DAEMON_CONFIG_ENV: &str = "KIANA_ENV_FAKE_DAEMON_CONFIG";

/// 脚本 daemon 的行为（JSON 序列化后经文件传给孩子进程）。
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FakeDaemonConfig {
    /// 收到的每一帧都追加到这个文件，每行一个 `{kind,id,json}` 对象。
    pub log: Option<String>,
    /// `readdir` 回复前的延迟。
    pub readdir_delay_ms: u64,
    /// `readdir` 回复的条目（原始 JSON）；缺省为空。
    pub readdir_entries: Option<serde_json::Value>,
    /// `readdir` 回复一个 `{name:"bad", error:{code:"EACCES"}}` 条目。
    pub readdir_entry_error: bool,
    /// `fstat` 回复前的延迟。
    pub fstat_delay_ms: u64,
    /// 收到 CANCEL 时立即以 `aborted` 错误回复该请求（会执行取消的 daemon）。
    pub cancel_errors: bool,
    /// `watch` 不发 `ready` 事件。
    pub watch_no_ready: bool,
    /// `watch` 回复 RESULT。
    pub watch_result: bool,
    /// `pty` 不发 `started` 事件。
    pub pty_no_started: bool,
    /// `pty` 回复 RESULT。
    pub pty_result: bool,
}

/// 把配置写进 `directory`，返回命令行用的路径。
pub fn write_fake_daemon_config(
    directory: &TempDir,
    config: &FakeDaemonConfig,
) -> std::path::PathBuf {
    let path = directory.path().to_owned();
    let path = std::path::Path::new(&path).join("fake-daemon.json");
    let bytes = serde_json::to_vec(config).expect("the fake daemon config serializes");
    std::fs::write(&path, bytes).expect("write the fake daemon config");
    path
}

/// 脚本 daemon 的启动命令（`sh` 包装；与旧 daemon 相同，Windows 上不可用）。
pub fn fake_daemon_command(config: &std::path::Path) -> Vec<String> {
    let executable = std::env::current_exe()
        .expect("the test binary path")
        .to_string_lossy()
        .into_owned();
    vec![
        "sh".to_owned(),
        "-c".to_owned(),
        format!(
            "{FAKE_DAEMON_CHILD_ENV}=1 {FAKE_DAEMON_TOKEN_ENV}=\"$4\" {FAKE_DAEMON_CONFIG_ENV}='{}' exec \"$1\" --exact common::fake_daemon_child --nocapture",
            config.display()
        ),
        "sh".to_owned(),
        executable,
    ]
}

/// 脚本 daemon 是否可用（需要 `sh` 包装）。
pub fn fake_daemon_supported() -> bool {
    cfg!(unix)
}

/// 重新执行本二进制时运行它：记录请求并按 [`FakeDaemonConfig`] 回答。
///
/// 未设置标记变量时立即返回（普通 `cargo test` 也会运行这个用例）。
#[test]
fn fake_daemon_child() {
    if std::env::var_os(FAKE_DAEMON_CHILD_ENV).is_none() {
        return;
    }
    let token = std::env::var(FAKE_DAEMON_TOKEN_ENV).unwrap_or_default();
    let config = std::env::var_os(FAKE_DAEMON_CONFIG_ENV)
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| serde_json::from_str::<FakeDaemonConfig>(&text).ok())
        .unwrap_or_default();
    serve_fake_daemon(&token, config);
    // Never return to libtest: its report would follow the frame stream.
    std::process::exit(0);
}

fn serve_fake_daemon(token: &str, config: FakeDaemonConfig) {
    use std::sync::{Arc, Mutex};
    type SharedStdout = Arc<Mutex<std::io::Stdout>>;
    type SharedLog = Arc<Mutex<Option<std::fs::File>>>;

    let stdout: SharedStdout = Arc::new(Mutex::new(std::io::stdout()));
    let log: SharedLog = Arc::new(Mutex::new(config.log.as_ref().map(|path| {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .expect("open the fake daemon log")
    })));
    {
        let mut stdout = stdout.lock().expect("stdout lock");
        let _ = writeln!(stdout, "KIANA-ENV {token}");
        let _ = stdout.flush();
    }

    let info = serde_json::json!({
        "name": "x",
        "kind": "file",
        "size": 0,
        "mtimeSec": 0,
        "mtimeNsec": 0,
        "dev": 1,
        "ino": 2,
    });
    let hello = serde_json::json!({
        "protocol": 1,
        "version": "1.1.0",
        "os": "linux",
        "arch": "x86_64",
        "home": "/home/u",
        "separator": "/",
        "tmpdir": "/tmp",
        "cwd": "/work",
        "driveCwds": {},
        "pid": std::process::id(),
        "features": ["listFiles"],
    });

    let stdin = std::io::stdin();
    let mut stdin = stdin.lock();
    loop {
        let mut header = [0u8; 4];
        if stdin.read_exact(&mut header).is_err() {
            return;
        }
        let length = u32::from_be_bytes(header) as usize;
        if length < 9 {
            return;
        }
        let mut body = vec![0u8; length];
        if stdin.read_exact(&mut body).is_err() {
            return;
        }
        let kind = body[0];
        let id = u32::from_be_bytes([body[1], body[2], body[3], body[4]]);
        let json_length = u32::from_be_bytes([body[5], body[6], body[7], body[8]]) as usize;
        if 9 + json_length > body.len() {
            return;
        }
        let json_text = String::from_utf8_lossy(&body[9..9 + json_length]).into_owned();
        {
            let mut log = log.lock().expect("log lock");
            if let Some(file) = log.as_mut() {
                let record = serde_json::json!({"kind": kind, "id": id, "json": json_text});
                let _ = writeln!(file, "{record}");
            }
        }
        let json: serde_json::Value =
            serde_json::from_str(&json_text).unwrap_or(serde_json::Value::Null);
        if kind == 5 {
            // CANCEL：可选的 daemon 会用它结束在途请求。
            if config.cancel_errors {
                send_fake_frame(
                    &stdout,
                    3,
                    id,
                    &serde_json::json!({"code": "aborted", "message": "aborted"}),
                    &[],
                );
            }
            continue;
        }
        if kind != 1 {
            continue;
        }
        let op = json
            .get("op")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        let reply = match op {
            "hello" => Some((2, hello.clone())),
            "open" => Some((
                2,
                serde_json::json!({"handle": 1, "stat": info.clone(), "info": info.clone()}),
            )),
            "opendir" => Some((2, serde_json::json!({"handle": 1}))),
            "fstat" | "lstat" => Some((2, info.clone())),
            "pread" => Some((2, serde_json::json!({}))),
            "scanLines" => Some((
                2,
                serde_json::json!({
                    "newlines": 0,
                    "start": 0,
                    "end": 0,
                    "firstLineEnd": 0,
                    "lastLineStart": 0,
                    "selectedBytes": 0,
                    "firstLineBytes": 0,
                }),
            )),
            "readdir" => {
                let entries = if config.readdir_entry_error {
                    serde_json::json!([{
                        "name": "bad",
                        "error": {"code": "EACCES", "message": "permission denied"},
                    }])
                } else {
                    config
                        .readdir_entries
                        .clone()
                        .unwrap_or_else(|| serde_json::json!([]))
                };
                let reply = serde_json::json!({"entries": entries, "done": true});
                reply_after(&stdout, config.readdir_delay_ms, 2, id, reply);
                None
            }
            "watch" => {
                if !config.watch_no_ready {
                    send_fake_frame(
                        &stdout,
                        4,
                        id,
                        &serde_json::json!({"kind": "ready", "mode": "native"}),
                        &[],
                    );
                }
                if config.watch_result {
                    Some((2, serde_json::json!({})))
                } else {
                    None
                }
            }
            "pty" => {
                if !config.pty_no_started {
                    send_fake_frame(
                        &stdout,
                        4,
                        id,
                        &serde_json::json!({"kind": "started", "pid": 4242}),
                        &[],
                    );
                }
                if config.pty_result {
                    Some((2, serde_json::json!({"exitCode": 0})))
                } else {
                    None
                }
            }
            "ptyWrite" | "ptyResize" => Some((2, serde_json::json!({}))),
            "ptyForeground" => Some((
                2,
                serde_json::json!({"foreground": {"pid": 77, "name": "sleep"}}),
            )),
            "mkdtemp" => Some((2, serde_json::json!({"path": "/tmp/tmp-test"}))),
            "realpath" => Some((
                2,
                serde_json::json!({"path": json.get("path").cloned().unwrap_or_default()}),
            )),
            "listFiles" => Some((2, serde_json::json!({"count": 0, "truncated": false}))),
            "write" => Some((2, serde_json::json!({}))),
            "writeChunk" | "close" | "truncate" | "fsync" | "rename" | "mkdir" | "rm" => {
                Some((2, serde_json::json!({})))
            }
            other => Some((
                3,
                serde_json::json!({
                    "code": "EINVAL",
                    "message": format!("unknown operation {other}"),
                }),
            )),
        };
        if let Some((kind, json)) = reply {
            send_fake_frame(&stdout, kind, id, &json, &[]);
        }
    }
}

fn reply_after(
    stdout: &std::sync::Arc<std::sync::Mutex<std::io::Stdout>>,
    delay_ms: u64,
    kind: u8,
    id: u32,
    json: serde_json::Value,
) {
    if delay_ms == 0 {
        send_fake_frame(stdout, kind, id, &json, &[]);
        return;
    }
    let stdout = std::sync::Arc::clone(stdout);
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(delay_ms));
        send_fake_frame(&stdout, kind, id, &json, &[]);
    });
}

fn send_fake_frame(
    stdout: &std::sync::Arc<std::sync::Mutex<std::io::Stdout>>,
    kind: u8,
    id: u32,
    json: &serde_json::Value,
    payload: &[u8],
) {
    let body = serde_json::to_vec(json).expect("the reply serializes");
    let mut bytes = Vec::with_capacity(13 + body.len() + payload.len());
    bytes.extend_from_slice(&((9 + body.len() + payload.len()) as u32).to_be_bytes());
    bytes.push(kind);
    bytes.extend_from_slice(&id.to_be_bytes());
    bytes.extend_from_slice(&(body.len() as u32).to_be_bytes());
    bytes.extend_from_slice(&body);
    bytes.extend_from_slice(payload);
    let mut stdout = stdout.lock().expect("stdout lock");
    let _ = stdout.write_all(&bytes);
    let _ = stdout.flush();
}

/// 把 TS `it()` 的标题放进任何 panic 消息（Rust 的测试名是 snake_case，标题只在文档里）。
///
/// `run` 的 panic 被捕获后以 `"{name}: {原消息}"` 重新抛出。
pub async fn titled<F>(name: &str, run: F)
where
    F: std::future::Future<Output = ()>,
{
    use futures::FutureExt;
    let guarded = std::panic::AssertUnwindSafe(run).catch_unwind();
    if let Err(payload) = guarded.await {
        panic!("{name}: {}", panic_message(payload));
    }
}

/// panic payload 的文本（`String` 或 `&str`）。
pub fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| {
            payload
                .downcast_ref::<&str>()
                .map(|text| (*text).to_owned())
        })
        .unwrap_or_else(|| "(non-string panic payload)".to_owned())
}
