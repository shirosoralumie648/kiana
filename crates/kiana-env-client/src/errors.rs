//! errors.ts。
//!
//! TS 的错误是 `Error` 子类（`FileError` / `RemoteError`）。Rust 侧用 [`EnvError`] 承载 TS 的
//! `unknown`：`toFileError(error, fallbackPath)` 按“已是 `FileError` 原样返回、`RemoteError`
//! 按 code 映射、其它错误包成 `unknown`”逐字移植（spec §2.1.2）。
//!
//! TS: packages/env/src/errors.ts#abortResult / #toFileError
//!
//! # 这个模块解决什么问题
//! 文件相关 trait（定义在 `kiana-durable` 的 `FileSystem`、`DirReader`、`TextLineReader`、
//! `BinaryReader`）只返回 [`FileError`]；同一 crate 的 `Shell::exec` 是例外，返回
//! `ExecutionError`，不属于本模块的转换范围（本模块只处理文件类错误）。这一层拿到的错误
//! 可能是 daemon 的 [`RemoteError`]、已经转换过的 [`FileError`]，或本地构造的字符串。三种来源
//! 如果各自向上冒泡，每个调用方都要重复判断；[`to_file_error`] 把它们统一收敛成 `FileError`，
//! [`abort_result`] 则把 `AbortSignal` 状态变成同一种 `aborted` 错误。
//!
//! # 转换规则
//! - 已经是 `FileError`：原样返回，连 `path` 都不改。`list_files_over` 用这个变体把
//!   “daemon 不支持 listFiles”这类本地构造的错误穿过 `to_file_error`，避免被重新映射。
//! - `RemoteError`：`path` 优先取 daemon 给的 `error.path`（只有 JSON 字符串才算，见
//!   [`RemoteError::new`]），否则用调用方给的 fallback；`message` 保留 daemon 原文；
//!   `cause` 保存 `RemoteError` 本身（`Arc` 共享）。
//! - 其它：`FileErrorCode::Unknown` + fallback path，`cause` 只保留消息文本。
//!
//! # 注意事项
//! daemon code → `FileErrorCode` 的映射是协议契约（spec §2.1.2），未列出的 code 一律
//! `unknown`；不要为未知 code 猜测更具体的语义，否则远端与本地行为会分叉。本模块只做类型
//! 转换，不发请求，也不主动检查取消——取消检查由调用方在请求前后调用 [`abort_result`] 完成。

use std::sync::Arc;

use kiana_chord::context::AbortSignal;
use kiana_durable::env::{FileError, FileErrorCode};

use crate::connection::RemoteError;

/// TS 的 `error: unknown`（[`to_file_error`] 的入参）。
///
/// 连接层只会产生 [`EnvError::Remote`]；[`EnvError::File`] 对应“已经是 `FileError` 时不重包装”；
/// [`EnvError::Other`] 对应 `error instanceof Error ? error : new Error(String(error))`。
///
/// 用枚举而不是 `Box<dyn Error>`：一是 [`EnvError`] 派生 `Clone`（`Box<dyn Error>` 不实现
/// `Clone`，而这里携带的 [`FileError`] 与 [`RemoteError`] 都是 `Clone`）；二是 [`to_file_error`]
/// 用穷尽 `match` 逐条实现 TS 的三种分支（`File` 原样返回、`Remote` 按 code 映射、其它包成
/// `Other`），与 spec §2.1.2 的 `instanceof` 判断一一对应。
///
/// TS: packages/env/src/errors.ts#toFileError
#[derive(Debug, Clone)]
pub enum EnvError {
    /// 已经是 `FileError`：原样返回。
    File(FileError),
    /// daemon 报告的失败。
    Remote(RemoteError),
    /// 其它错误，只保留消息。
    Other(String),
}

impl EnvError {
    /// `error.message`。
    pub fn message(&self) -> &str {
        match self {
            EnvError::File(error) => &error.message,
            EnvError::Remote(error) => error.message(),
            EnvError::Other(message) => message,
        }
    }
}

impl std::fmt::Display for EnvError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.message())
    }
}

impl std::error::Error for EnvError {}

impl From<FileError> for EnvError {
    fn from(error: FileError) -> Self {
        EnvError::File(error)
    }
}

impl From<RemoteError> for EnvError {
    fn from(error: RemoteError) -> Self {
        EnvError::Remote(error)
    }
}

impl From<&str> for EnvError {
    fn from(message: &str) -> Self {
        EnvError::Other(message.to_owned())
    }
}

impl From<String> for EnvError {
    fn from(message: String) -> Self {
        EnvError::Other(message)
    }
}

/// `new Error(message)` 的载体：`FileError.cause` 需要 `Arc<dyn Error>`。
#[derive(Debug)]
struct MessageCause(String);

impl std::fmt::Display for MessageCause {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for MessageCause {}

/// `abortResult(signal, path)`：已中止时返回 `err(FileError("aborted", "aborted", path))`。
///
/// 注意消息固定为小写 `"aborted"`，不是 Node 的 `"The operation was aborted"`。
///
/// 返回值：`None` = 没有 signal 或尚未中止（调用方继续执行）；`Some(Err(_))` = 已中止。
/// 典型用法是在一次请求前后各调用一次：请求前返回 `Some` 就不发请求，请求后返回 `Some`
/// 则即使 daemon 已经给出结果也以 `aborted` 结算（见 `list_files_over`）。
///
/// TS: packages/env/src/errors.ts#abortResult
pub fn abort_result<T>(
    signal: Option<&AbortSignal>,
    path: Option<&str>,
) -> Option<Result<T, FileError>> {
    let signal = signal?;
    if !signal.is_aborted() {
        return None;
    }
    Some(Err(FileError {
        code: FileErrorCode::Aborted,
        message: "aborted".to_owned(),
        path: path.map(str::to_owned),
        cause: None,
    }))
}

/// `toFileError(error, fallbackPath)`：`NodeExecutionEnv` 的 Node 错误码 → `FileError` 码映射。
///
/// 具体规则（spec §2.1.2）：
/// - `EnvError::File`：原样返回，`path` 不变（对应 TS 的 `instanceof FileError`）。
/// - `EnvError::Remote`：`path = error.path ?? fallback_path`；`message` 保留 daemon 原文；
///   `cause` 是 `RemoteError` 本身。daemon code 的映射：
///
///   | daemon `code` | `FileErrorCode` |
///   |---|---|
///   | `aborted` | `Aborted` |
///   | `ENOENT` | `NotFound` |
///   | `EACCES`、`EPERM` | `PermissionDenied` |
///   | `ENOTDIR` | `NotDirectory` |
///   | `EISDIR` | `IsDirectory` |
///   | `EINVAL`、`SYMLINK`、`NOT_REGULAR` | `Invalid` |
///   | 其它（`ERR_FS_EISDIR`、`EMFILE`、`EBADF`、`timeout` …） | `Unknown` |
///
/// - `EnvError::Other`：`Unknown` + `fallback_path`，`cause` 只保留消息。
///
/// 映射表由 `to_file_error_maps_every_daemon_code` 逐项锁定；新增 daemon 错误码时先确认
/// 它在 TS 侧的归类，再改这里，否则本地与远端环境会返回不同的 `FileErrorCode`。
///
/// TS: packages/env/src/errors.ts#toFileError
pub fn to_file_error(error: EnvError, fallback_path: Option<&str>) -> FileError {
    let fallback = fallback_path.map(str::to_owned);
    match error {
        EnvError::File(error) => error,
        EnvError::Remote(error) => {
            // TS 的 `error.path ?? fallbackPath`：daemon 没给字符串 path 时才用调用方的路径。
            let path = error.path.clone().or(fallback);
            let code = match error.code.as_str() {
                "aborted" => FileErrorCode::Aborted,
                "ENOENT" => FileErrorCode::NotFound,
                "EACCES" | "EPERM" => FileErrorCode::PermissionDenied,
                "ENOTDIR" => FileErrorCode::NotDirectory,
                "EISDIR" => FileErrorCode::IsDirectory,
                "EINVAL" | "SYMLINK" | "NOT_REGULAR" => FileErrorCode::Invalid,
                _ => FileErrorCode::Unknown,
            };
            FileError {
                code,
                message: error.message().to_owned(),
                path,
                cause: Some(Arc::new(error)),
            }
        }
        EnvError::Other(message) => FileError {
            code: FileErrorCode::Unknown,
            message: message.clone(),
            path: fallback,
            cause: Some(Arc::new(MessageCause(message))),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn remote(fields: serde_json::Value) -> RemoteError {
        match fields {
            serde_json::Value::Object(map) => RemoteError::new(map),
            other => panic!("expected an object, got {other}"),
        }
    }

    #[test]
    fn to_file_error_keeps_an_existing_file_error() {
        let original = FileError::new(FileErrorCode::NotFound, "gone").with_path("a");
        let mapped = to_file_error(EnvError::File(original.clone()), Some("fallback"));
        assert_eq!(mapped.code, FileErrorCode::NotFound);
        assert_eq!(mapped.message, "gone");
        assert_eq!(mapped.path.as_deref(), Some("a"));
    }

    #[test]
    fn to_file_error_maps_every_daemon_code() {
        let cases = [
            ("aborted", FileErrorCode::Aborted),
            ("ENOENT", FileErrorCode::NotFound),
            ("EACCES", FileErrorCode::PermissionDenied),
            ("EPERM", FileErrorCode::PermissionDenied),
            ("ENOTDIR", FileErrorCode::NotDirectory),
            ("EISDIR", FileErrorCode::IsDirectory),
            ("EINVAL", FileErrorCode::Invalid),
            ("SYMLINK", FileErrorCode::Invalid),
            ("NOT_REGULAR", FileErrorCode::Invalid),
            ("ERR_FS_EISDIR", FileErrorCode::Unknown),
            ("EMFILE", FileErrorCode::Unknown),
            ("EBADF", FileErrorCode::Unknown),
            ("ENOTSUP", FileErrorCode::Unknown),
            ("timeout", FileErrorCode::Unknown),
            ("spawn_error", FileErrorCode::Unknown),
        ];
        for (code, expected) in cases {
            let mapped = to_file_error(
                EnvError::Remote(remote(serde_json::json!({
                    "code": code,
                    "message": format!("failed with {code}"),
                }))),
                Some("fallback"),
            );
            assert_eq!(mapped.code, expected, "code {code}");
            assert_eq!(mapped.message, format!("failed with {code}"));
            assert_eq!(mapped.path.as_deref(), Some("fallback"));
            assert!(mapped.cause.is_some());
        }
    }

    #[test]
    fn to_file_error_prefers_the_daemon_path() {
        let mapped = to_file_error(
            EnvError::Remote(remote(serde_json::json!({
                "code": "ENOENT",
                "message": "no such file",
                "path": "/remote/path",
            }))),
            Some("fallback"),
        );
        assert_eq!(mapped.path.as_deref(), Some("/remote/path"));
    }

    #[test]
    fn to_file_error_wraps_other_errors_as_unknown() {
        let mapped = to_file_error(EnvError::Other("boom".to_owned()), Some("fallback"));
        assert_eq!(mapped.code, FileErrorCode::Unknown);
        assert_eq!(mapped.message, "boom");
        assert_eq!(mapped.path.as_deref(), Some("fallback"));
        assert_eq!(
            mapped.cause.as_ref().map(|cause| cause.to_string()),
            Some("boom".to_owned())
        );
    }

    #[test]
    fn to_file_error_leaves_the_path_unset_without_a_fallback() {
        let mapped = to_file_error(EnvError::Other("boom".to_owned()), None);
        assert!(mapped.path.is_none());
    }

    #[test]
    fn abort_result_reports_aborted_with_the_lowercase_message() {
        let signal = AbortSignal::new();
        assert!(abort_result::<()>(Some(&signal), Some("a")).is_none());
        signal.abort_without_reason();
        let result = abort_result::<()>(Some(&signal), Some("a")).expect("aborted");
        let error = result.expect_err("aborted result");
        assert_eq!(error.code, FileErrorCode::Aborted);
        assert_eq!(error.message, "aborted");
        assert_eq!(error.path.as_deref(), Some("a"));
        assert!(abort_result::<()>(None, Some("a")).is_none());
    }
}
