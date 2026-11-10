//! 黑盒测试的公共工具：启动构建出的 `kiana-env` 二进制、读握手行、按帧收发。
//!
//! 只用标准库 + `serde_json`（daemon 自己的依赖），不引入额外 dev-dependency。
//! 帧格式见 `crates/kiana-env/docs/protocol.md`。

// 每个测试二进制只用到其中一部分帮助函数。
#![allow(dead_code)]

use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

pub const REQUEST: u8 = 1;
pub const RESULT: u8 = 2;
pub const ERROR: u8 = 3;
pub const EVENT: u8 = 4;
pub const CANCEL: u8 = 5;
pub const PING: u8 = 6;

/// 测试用的 token：32 个小写十六进制字符，与客户端生成的一致。
pub const TOKEN: &str = "0123456789abcdef0123456789abcdef";

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// 一帧（`docs/protocol.md`）。
#[derive(Debug, Clone)]
pub struct Frame {
    pub kind: u8,
    pub id: u32,
    pub json: Value,
    pub payload: Vec<u8>,
}

impl Frame {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.payload).into_owned()
    }
}

enum Message {
    Handshake(String),
    Frame(Frame),
    Ended,
}

/// 一个运行中的 daemon 进程，加上已解析的帧流。
pub struct Daemon {
    child: Child,
    stdin: Option<ChildStdin>,
    messages: Receiver<Message>,
    stderr: Receiver<Vec<u8>>,
    /// 为别的请求先到达的帧，按请求 id 暂存（事件与回复交错到达）。
    stashed: HashMap<u32, VecDeque<Frame>>,
}

impl Daemon {
    /// `kiana-env serve --token <TOKEN> [extra…]`。
    pub fn start(extra_args: &[&str]) -> Daemon {
        let mut command = Command::new(env!("CARGO_BIN_EXE_kiana-env"));
        command
            .arg("serve")
            .arg("--token")
            .arg(TOKEN)
            .args(extra_args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        Daemon::spawn(command)
    }

    /// 以任意参数启动 `kiana-env`（CLI 行为测试用）。
    pub fn spawn_with_args(args: &[&str]) -> Daemon {
        let mut command = Command::new(env!("CARGO_BIN_EXE_kiana-env"));
        command
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        Daemon::spawn(command)
    }

    fn spawn(mut command: Command) -> Daemon {
        let mut child = command.spawn().expect("spawn kiana-env");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = child.stdout.take().expect("stdout");
        let mut stderr_pipe = child.stderr.take().expect("stderr");
        let (sender, messages) = mpsc::channel();
        thread::spawn(move || read_stdout(BufReader::new(stdout), sender));
        let (stderr_sender, stderr) = mpsc::channel();
        thread::spawn(move || {
            let mut text = Vec::new();
            let _ = stderr_pipe.read_to_end(&mut text);
            let _ = stderr_sender.send(text);
        });
        Daemon {
            child,
            stdin: Some(stdin),
            messages,
            stderr,
            stashed: HashMap::new(),
        }
    }

    /// 等握手行 `KIANA-ENV <token>\n`。
    pub fn handshake(&mut self) -> String {
        match self.messages.recv_timeout(DEFAULT_TIMEOUT) {
            Ok(Message::Handshake(line)) => line,
            Ok(_) => panic!("kiana-env printed frames before its handshake"),
            Err(error) => panic!("kiana-env did not print its handshake: {error}"),
        }
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    pub fn send_frame(&mut self, kind: u8, id: u32, json: &Value, payload: &[u8]) {
        let body = serde_json::to_vec(json).expect("serialize request JSON");
        let length = 9 + body.len() + payload.len();
        let mut header = Vec::with_capacity(13);
        header.extend_from_slice(&(length as u32).to_be_bytes());
        header.push(kind);
        header.extend_from_slice(&id.to_be_bytes());
        header.extend_from_slice(&(body.len() as u32).to_be_bytes());
        let stdin = self.stdin.as_mut().expect("stdin is open");
        stdin.write_all(&header).expect("write frame header");
        stdin.write_all(&body).expect("write frame JSON");
        stdin.write_all(payload).expect("write frame payload");
        stdin.flush().expect("flush frame");
    }

    pub fn send_json(&mut self, kind: u8, id: u32, json: &Value) {
        self.send_frame(kind, id, json, &[]);
    }

    /// 原始字节（帧长度/JSON 长度越界等用例）。
    pub fn send_raw(&mut self, bytes: &[u8]) {
        let stdin = self.stdin.as_mut().expect("stdin is open");
        stdin.write_all(bytes).expect("write raw bytes");
        stdin.flush().expect("flush raw bytes");
    }

    pub fn request(&mut self, id: u32, op: &str, mut json: Value) {
        object(&mut json).insert("op".into(), json!(op));
        self.send_json(REQUEST, id, &json);
    }

    pub fn request_payload(&mut self, id: u32, op: &str, mut json: Value, payload: &[u8]) {
        object(&mut json).insert("op".into(), json!(op));
        self.send_frame(REQUEST, id, &json, payload);
    }

    pub fn cancel(&mut self, id: u32, kill: bool) {
        let json = if kill {
            json!({ "mode": "kill" })
        } else {
            json!({})
        };
        self.send_json(CANCEL, id, &json);
    }

    /// 下一帧；daemon 退出（stdout EOF）或超时时为 `None`。
    pub fn recv(&mut self, timeout: Duration) -> Option<Frame> {
        match self.messages.recv_timeout(timeout) {
            Ok(Message::Frame(frame)) => Some(frame),
            Ok(Message::Handshake(line)) => panic!("unexpected second handshake {line:?}"),
            Ok(Message::Ended) => None,
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => None,
        }
    }

    fn stash(&mut self, frame: Frame) {
        self.stashed.entry(frame.id).or_default().push_back(frame);
    }

    fn take_stashed(&mut self, id: u32, wanted: impl Fn(&Frame) -> bool) -> Option<Frame> {
        let queue = self.stashed.get_mut(&id)?;
        let position = queue.iter().position(wanted)?;
        queue.remove(position)
    }

    /// 请求 `id` 的下一帧（事件或回复），别的请求的帧先暂存。
    pub fn wait_any(&mut self, id: u32, timeout: Duration) -> Frame {
        if let Some(frame) = self.take_stashed(id, |_| true) {
            return frame;
        }
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match self.recv(remaining) {
                Some(frame) if frame.id == id => return frame,
                Some(frame) => self.stash(frame),
                None => panic!("no frame for request {id} within {timeout:?}"),
            }
        }
    }

    /// 下一帧，跳过 `ping`。
    pub fn next_frame(&mut self) -> Frame {
        loop {
            match self.recv(DEFAULT_TIMEOUT) {
                Some(frame) if frame.kind == PING => continue,
                Some(frame) => return frame,
                None => panic!("kiana-env closed its stdout while a frame was expected"),
            }
        }
    }

    /// 请求 `id` 的 RESULT/ERROR 帧，跳过同 id 的事件。
    pub fn reply(&mut self, id: u32) -> Frame {
        self.reply_with_events(id).1
    }

    /// 请求 `id` 的事件帧，直到它的 RESULT/ERROR。
    pub fn reply_with_events(&mut self, id: u32) -> (Vec<Frame>, Frame) {
        let mut events = Vec::new();
        loop {
            let frame = self.wait_any(id, DEFAULT_TIMEOUT);
            match frame.kind {
                EVENT => events.push(frame),
                RESULT | ERROR => return (events, frame),
                other => panic!(
                    "unexpected frame kind {other} id {} for request {id}",
                    frame.id
                ),
            }
        }
    }

    /// 等一个 `id` 的事件帧（`exec`/`pty`/`watch` 的进度）。
    pub fn wait_event(&mut self, id: u32, timeout: Duration) -> Frame {
        if let Some(frame) = self.take_stashed(id, |frame| frame.kind == EVENT) {
            return frame;
        }
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match self.recv(remaining) {
                Some(frame) if frame.kind == EVENT && frame.id == id => return frame,
                Some(frame) => self.stash(frame),
                None => panic!("no event for request {id} within {timeout:?}"),
            }
        }
    }

    /// 发送请求并返回 RESULT 的 JSON，断言它是成功。
    pub fn call(&mut self, id: u32, op: &str, json: Value) -> Value {
        self.request(id, op, json);
        ok(self.reply(id))
    }

    /// 发送带 payload 的请求并返回 RESULT 的 JSON，断言它是成功。
    pub fn call_payload(&mut self, id: u32, op: &str, json: Value, payload: &[u8]) -> Value {
        self.request_payload(id, op, json, payload);
        ok(self.reply(id))
    }

    /// 发送请求并返回 ERROR 的 JSON，断言它是失败。
    pub fn call_error(&mut self, id: u32, op: &str, json: Value) -> Value {
        self.request(id, op, json);
        error(self.reply(id))
    }

    /// 发送带 payload 的请求并返回 ERROR 的 JSON，断言它是失败。
    pub fn call_payload_error(&mut self, id: u32, op: &str, json: Value, payload: &[u8]) -> Value {
        self.request_payload(id, op, json, payload);
        error(self.reply(id))
    }

    pub fn close_stdin(&mut self) {
        self.stdin.take();
    }

    /// 等进程退出；超时则杀掉并失败。
    pub fn wait(&mut self, timeout: Duration) -> ExitStatus {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.child.try_wait().expect("try_wait") {
                return status;
            }
            if Instant::now() >= deadline {
                let _ = self.child.kill();
                let status = self.child.wait().expect("wait");
                panic!("kiana-env did not exit within {timeout:?} (status {status})");
            }
            thread::sleep(Duration::from_millis(5));
        }
    }

    /// 进程退出后的 stderr 文本。
    pub fn stderr_text(&mut self) -> String {
        match self.stderr.recv_timeout(Duration::from_secs(10)) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(error) => panic!("stderr did not end: {error}"),
        }
    }

    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.stdin.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn read_stdout(mut reader: impl BufRead, sender: Sender<Message>) {
    let mut line = Vec::new();
    match reader.read_until(b'\n', &mut line) {
        Ok(0) | Err(_) => {
            let _ = sender.send(Message::Ended);
            return;
        }
        Ok(_) => {}
    }
    if sender
        .send(Message::Handshake(
            String::from_utf8_lossy(&line).into_owned(),
        ))
        .is_err()
    {
        return;
    }
    loop {
        match read_frame(&mut reader) {
            Ok(Some(frame)) => {
                if sender.send(Message::Frame(frame)).is_err() {
                    return;
                }
            }
            Ok(None) | Err(_) => {
                let _ = sender.send(Message::Ended);
                return;
            }
        }
    }
}

fn read_frame(reader: &mut impl BufRead) -> std::io::Result<Option<Frame>> {
    let mut length = [0u8; 4];
    match reader.read_exact(&mut length) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error),
    }
    let length = u32::from_be_bytes(length) as usize;
    if !(9..=16 * 1024 * 1024).contains(&length) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "frame length out of range",
        ));
    }
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body)?;
    let kind = body[0];
    let id = u32::from_be_bytes([body[1], body[2], body[3], body[4]]);
    let json_length = u32::from_be_bytes([body[5], body[6], body[7], body[8]]) as usize;
    if 9 + json_length > length {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "JSON length out of range",
        ));
    }
    let json = if json_length == 0 {
        json!({})
    } else {
        serde_json::from_slice(&body[9..9 + json_length]).unwrap_or(Value::Null)
    };
    Ok(Some(Frame {
        kind,
        id,
        json,
        payload: body[9 + json_length..].to_vec(),
    }))
}

/// 请求 JSON 必须是对象。
pub fn object(json: &mut Value) -> &mut serde_json::Map<String, Value> {
    json.as_object_mut()
        .expect("request JSON must be an object")
}

/// 断言 RESULT 帧并返回其 JSON。
pub fn ok(frame: Frame) -> Value {
    assert_eq!(
        frame.kind, RESULT,
        "expected a result, got error {:?}",
        frame.json
    );
    frame.json
}

/// 断言 ERROR 帧并返回其 JSON。
pub fn error(frame: Frame) -> Value {
    assert_eq!(
        frame.kind, ERROR,
        "expected an error, got result {:?}",
        frame.json
    );
    frame.json
}

/// 断言错误码与消息。
pub fn assert_error(json: &Value, code: &str, message: &str) {
    assert_eq!(json["code"], json!(code), "error JSON: {json}");
    assert_eq!(json["message"], json!(message), "error JSON: {json}");
}

/// `hello` 的结果。
pub fn hello(daemon: &mut Daemon) -> Value {
    daemon.call(0, "hello", json!({}))
}

/// 一个自动删除的临时目录（标准库实现，不引 `tempfile`）。
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    pub fn new(tag: &str) -> TempDir {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "kiana-env-rs-{tag}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).expect("create temp dir");
        TempDir { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn str(&self) -> String {
        self.path.to_string_lossy().into_owned()
    }

    pub fn join(&self, name: &str) -> String {
        self.path.join(name).to_string_lossy().into_owned()
    }

    pub fn write(&self, name: &str, content: &[u8]) {
        let path = self.path.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent");
        }
        std::fs::write(path, content).expect("write file");
    }

    pub fn mkdir(&self, name: &str) {
        std::fs::create_dir_all(self.path.join(name)).expect("create dir");
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
