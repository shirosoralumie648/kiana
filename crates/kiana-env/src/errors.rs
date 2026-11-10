//! `errors.rs`：Node 风格的错误（`code` 加诊断消息），错误 JSON 为 `{ code, message, syscall?, path? }`。
//!
//! TS 源：packages/env/daemon/src/errors.rs（原 daemon 本身就是 Rust；逐字移植）。
//! `describe` 表是 libuv `uv_strerror` 的逐字文案；客户端只看 `code`，`message` 仅作诊断。
//! Errors as Node reports them: a code such as `ENOENT` plus a diagnostic message.
//!
//! # 为什么需要这个文件
//! 客户端（Durable 的 `NodeExecutionEnv` 适配层）按 Node 风格的 `code` 决定语义，例如把 `ENOENT`
//! 映射成 `not_found`；协议规定只有 `code` 属于契约，`message` 仅供诊断（docs/protocol.md）。
//! 因此所有失败先收敛成同一个类型，再由 `to_json` 变成线协议中的错误帧，保证每个操作的错误形状一致。
//!
//! # 结构
//! - `Failure`：`code` + 诊断 `message`，可选的 `Details`；
//! - `Details`：`syscall`、`path` 两个常见字段，以及 `extra` 携带的操作特定字段（如 `spillPath`）；
//! - `to_json`：生成 `{ code, message, syscall?, path?, ...extra }`；`main::respond` 用它构造 ERROR 帧。
//!
//! # 约束与失败模式
//! - 客户端只依赖 `code`；`message` 文案不是协议契约（docs/protocol.md），但对拍测试会逐字比较
//!   参照实现的输出，因此本文件的文案仍需保持移植时的精确性。
//! - `describe` 只覆盖 libuv 的 `uv_strerror` 表；I/O 错误的 `code` 由 `crate::sys::error_name` 给出，
//!   表外代码的描述退化为 OS 错误文本（截掉 Rust 追加的 `" (os error N)"` 后缀并转小写）。
//! - `extra` 只合并 JSON 对象；非对象被忽略，同名键会覆盖先写入的字段（测试锁定）。
//! - JSON 键顺序由 serde_json 的 `preserve_order` 特性决定，不构成线协议契约；对拍测试按解析后的值比较。

use serde_json::{Value, json};
use std::io;

/// A failure with a Node-style code; messages are diagnostic only.
///
/// `code` 是客户端的判断依据（如 `ENOENT`、`EBADF`，或执行类代码 `timeout`、`aborted`），
/// `message` 是给人看的诊断文本。附加信息放在私有的 `details` 中：没有附加字段时 `Option<Box<_>>`
/// 不额外分配（只剩两个 `String` 自身的存储）。
///
/// 序列化规则见 `Failure::to_json`。
#[derive(Debug)]
pub struct Failure {
    /// 客户端用来区分错误类别的 Node/libuv 风格代码。
    pub code: String,
    /// 诊断消息；内容可能变化，不应据此判断语义。
    pub message: String,
    details: Option<Box<Details>>,
}

/// 错误的可选项；只有调用方主动补充 `syscall` / `path` / `extra` 时才创建。
#[derive(Debug, Default)]
struct Details {
    /// 触发失败的系统调用名（如 `lstat`、`rename`），序列化为错误 JSON 的 `syscall` 字段。
    syscall: Option<String>,
    /// 出错的路径；双路径调用只记录源路径，序列化为 `path` 字段。
    path: Option<String>,
    /// Extra fields for the client, such as `spillPath`.
    extra: Option<Value>,
}

impl Failure {
    /// 构造一个只有代码与消息的失败，不带 `syscall` / `path`。
    ///
    /// 用于协议层校验失败（`EINVAL`）和守护进程自定义的错误（如 `spawn_error`、`timeout`）。
    pub fn new(code: &str, message: impl Into<String>) -> Failure {
        Failure {
            code: code.to_string(),
            message: message.into(),
            details: None,
        }
    }

    /// 取得（必要时创建）附加字段容器。
    fn details(&mut self) -> &mut Details {
        self.details.get_or_insert_with(Default::default)
    }

    /// Add fields to the error JSON, such as `spillPath`.
    ///
    /// 只接受 JSON 对象：非对象会被静默忽略。合并发生在 `to_json` 的最后一步，因此同名键会覆盖
    /// `code` / `message` / `syscall` / `path` 等已有字段（测试 `extra_fields_merge_into_the_object_only`）。
    pub fn extra(mut self, extra: Value) -> Failure {
        self.details().extra = Some(extra);
        self
    }

    /// An I/O error of `syscall` on `path`, with libuv's error name as code.
    ///
    /// `code` 来自 `crate::sys::error_name`（平台层把 errno 翻译成 libuv 的名字）；
    /// 消息形如 `ENOENT: no such file or directory, lstat '/path'`。
    pub fn io(error: &io::Error, syscall: &str, path: &str) -> Failure {
        Failure::io_between(error, syscall, path, None)
    }

    /// Node's message for an error of a two-path call: `CODE: description, syscall 'from' -> 'to'`.
    ///
    /// 双路径形式（`destination` 为 `Some`）目前只由 `rename` 使用；单路径 I/O 错误经 `Failure::io`
    /// 也调用本函数，此时 `destination` 为 `None`。`path` 字段只记录源路径（与 Node 一致），
    /// 目标路径仅出现在 `message` 里；`syscall` 字段始终记录调用名。
    pub fn io_between(
        error: &io::Error,
        syscall: &str,
        path: &str,
        destination: Option<&str>,
    ) -> Failure {
        let code = crate::sys::error_name(error).to_string();
        let description = describe(&code).map(str::to_string).unwrap_or_else(|| {
            // Unknown to libuv's table: the OS text without Rust's " (os error N)".
            let text = error.to_string();
            text.split(" (os error")
                .next()
                .unwrap_or(&text)
                .to_lowercase()
        });
        let target = match destination {
            Some(destination) => format!("'{path}' -> '{destination}'"),
            None => format!("'{path}'"),
        };
        let mut failure = Failure::new(&code, format!("{code}: {description}, {syscall} {target}"));
        failure.details().syscall = Some(syscall.to_string());
        failure.path(path)
    }

    /// 记录出错路径，供错误 JSON 的 `path` 字段使用。
    pub fn path(mut self, path: &str) -> Failure {
        self.details().path = Some(path.to_string());
        self
    }

    /// 序列化为线协议的错误对象：`{ code, message }` 加上可选的 `syscall`、`path` 与 `extra` 字段。
    ///
    /// `code` 与 `message` 总是存在；`extra` 中的对象键最后合并，可能覆盖前面的同名字段。
    /// 没有附加信息时不会创建 `Details`，输出就是最小的两个字段。
    pub fn to_json(&self) -> Value {
        let mut value = json!({ "code": self.code, "message": self.message });
        let Some(details) = &self.details else {
            return value;
        };
        if let Some(syscall) = &details.syscall {
            value["syscall"] = json!(syscall);
        }
        if let Some(path) = &details.path {
            value["path"] = json!(path);
        }
        if let Some(Value::Object(extra)) = &details.extra {
            for (key, field) in extra {
                value[key] = field.clone();
            }
        }
        value
    }
}

/// libuv's `uv_strerror` texts, which Node uses on every system.
///
/// `describe` 只由 `Failure::io_between` 调用；返回 `None` 表示代码不在表内，此时 `io_between`
/// 使用 `io::Error` 的 Display 文本（截掉 `" (os error N)"` 并转小写）作为描述。`ERR_FS_EISDIR`、
/// `timeout`、`aborted` 等代码由 `Failure::new` 直接构造，不会走到这里。文案是面向用户的稳定契约，
/// 改动需同步测试：测试 `describes_every_libuv_code_verbatim` 用同一张 `TEXTS` 表逐项比对，包括
/// `ENOTSUP` 的 “socket” 措辞（原样保留，不做措辞修正）。
fn describe(code: &str) -> Option<&'static str> {
    Some(match code {
        "EACCES" => "permission denied",
        "EAGAIN" => "resource temporarily unavailable",
        "EBADF" => "bad file descriptor",
        "EBUSY" => "resource busy or locked",
        "EEXIST" => "file already exists",
        "EFBIG" => "file too large",
        "EINVAL" => "invalid argument",
        "EIO" => "i/o error",
        "EISDIR" => "illegal operation on a directory",
        "ELOOP" => "too many symbolic links encountered",
        "EMFILE" => "too many open files",
        "EMLINK" => "too many links",
        "ENAMETOOLONG" => "name too long",
        "ENFILE" => "file table overflow",
        "ENOENT" => "no such file or directory",
        "ENOMEM" => "not enough memory",
        "ENOSPC" => "no space left on device",
        "ENOSYS" => "function not implemented",
        "ENOTDIR" => "not a directory",
        "ENOTEMPTY" => "directory not empty",
        "ENOTSUP" => "operation not supported on socket",
        "EPERM" => "operation not permitted",
        "EROFS" => "read-only file system",
        "ETXTBSY" => "text file is busy",
        "EXDEV" => "cross-device link not permitted",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // libuv's `uv_strerror` table (spec §2.2.4), verbatim, including ENOTSUP's socket wording.
    const TEXTS: [(&str, &str); 25] = [
        ("EACCES", "permission denied"),
        ("EAGAIN", "resource temporarily unavailable"),
        ("EBADF", "bad file descriptor"),
        ("EBUSY", "resource busy or locked"),
        ("EEXIST", "file already exists"),
        ("EFBIG", "file too large"),
        ("EINVAL", "invalid argument"),
        ("EIO", "i/o error"),
        ("EISDIR", "illegal operation on a directory"),
        ("ELOOP", "too many symbolic links encountered"),
        ("EMFILE", "too many open files"),
        ("EMLINK", "too many links"),
        ("ENAMETOOLONG", "name too long"),
        ("ENFILE", "file table overflow"),
        ("ENOENT", "no such file or directory"),
        ("ENOMEM", "not enough memory"),
        ("ENOSPC", "no space left on device"),
        ("ENOSYS", "function not implemented"),
        ("ENOTDIR", "not a directory"),
        ("ENOTEMPTY", "directory not empty"),
        ("ENOTSUP", "operation not supported on socket"),
        ("EPERM", "operation not permitted"),
        ("EROFS", "read-only file system"),
        ("ETXTBSY", "text file is busy"),
        ("EXDEV", "cross-device link not permitted"),
    ];

    #[test]
    fn describes_every_libuv_code_verbatim() {
        for (code, text) in &TEXTS {
            assert_eq!(describe(code), Some(*text), "{code}");
        }
        // Codes outside libuv's table have no text; callers fall back to the OS message.
        assert_eq!(describe("ERR_FS_EISDIR"), None);
        assert_eq!(describe("ENOANO"), None);
        assert_eq!(describe("unknown"), None);
    }

    #[test]
    fn json_has_code_and_message_and_details() {
        let plain = Failure::new("EINVAL", "invalid argument").to_json();
        assert_eq!(
            plain,
            json!({ "code": "EINVAL", "message": "invalid argument" })
        );

        let with_path = Failure::new("ENOENT", "gone").path("/tmp/x").to_json();
        assert_eq!(
            with_path,
            json!({ "code": "ENOENT", "message": "gone", "path": "/tmp/x" })
        );

        let error = io::Error::from_raw_os_error(2);
        let failure = Failure::io(&error, "lstat", "/tmp/missing");
        let value = failure.to_json();
        assert_eq!(value["code"], json!("ENOENT"));
        assert_eq!(
            value["message"],
            json!("ENOENT: no such file or directory, lstat '/tmp/missing'")
        );
        assert_eq!(value["syscall"], json!("lstat"));
        assert_eq!(value["path"], json!("/tmp/missing"));
        // The keys below are this daemon's insertion order (the workspace enables serde_json's
        // `preserve_order`). The reference daemon's serde_json does not, so it emits the same fields
        // in alphabetical order (`code`, `message`, `path`, `syscall`). Key order is deliberately
        // *not* part of the wire contract: clients parse the JSON object, so this assertion records
        // this daemon's output, it does not claim the reference emits the same order
        // (tests/golden.rs compares replies as parsed values for the same reason).
        let keys: Vec<&str> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, ["code", "message", "syscall", "path"]);
    }

    #[test]
    fn a_two_path_error_names_both_paths() {
        let error = io::Error::from_raw_os_error(2);
        let value = Failure::io_between(&error, "rename", "/a", Some("/b")).to_json();
        assert_eq!(
            value["message"],
            json!("ENOENT: no such file or directory, rename '/a' -> '/b'")
        );
        assert_eq!(value["path"], json!("/a"));
    }

    #[test]
    fn unknown_codes_lowercase_the_os_text_without_the_error_number() {
        // libuv has no text for errno 999, so the daemon uses the system message, lowercased, cut before " (os error".
        let error = io::Error::from_raw_os_error(999);
        let value = Failure::io(&error, "read", "/p").to_json();
        assert_eq!(value["code"], json!("UNKNOWN"));
        assert_eq!(
            value["message"],
            json!("UNKNOWN: unknown error 999, read '/p'")
        );
        // An io::Error without an OS code has the OS name UNKNOWN and its own text as the description.
        let custom = io::Error::other("boom");
        let value = Failure::io(&custom, "write", "/p").to_json();
        assert_eq!(value["code"], json!("UNKNOWN"));
        assert_eq!(value["message"], json!("UNKNOWN: boom, write '/p'"));
    }

    #[test]
    fn extra_fields_merge_into_the_object_only() {
        let value = Failure::new("timeout", "timeout")
            .extra(json!({ "spillPath": "/tmp/kiana-output-0.log" }))
            .to_json();
        assert_eq!(value["spillPath"], json!("/tmp/kiana-output-0.log"));
        // Non-object extras are ignored, as `Failure::extra` only merges objects.
        let ignored = Failure::new("timeout", "timeout")
            .extra(json!("nope"))
            .to_json();
        assert_eq!(ignored, json!({ "code": "timeout", "message": "timeout" }));
        // Later fields overwrite earlier ones of the same name.
        let overwritten = Failure::new("EINVAL", "invalid")
            .path("/p")
            .extra(json!({ "path": "/other" }))
            .to_json();
        assert_eq!(overwritten["path"], json!("/other"));
    }
}
