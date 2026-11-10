//! list-files.ts：经 daemon 的 `listFiles`，以及使用它的 `LocalExecutionEnv`。
//!
//! 记录格式：`<kind 字节 f|d|l><UTF-8 路径><0x00>` 重复（daemon 的 `list.rs`）。
//!
//! # 这个文件解决什么问题
//! Durable 的 `FileSystem` 有可选的 `listFiles`，本地 [`NodeExecutionEnv`] 用的是 trait 的
//! 默认实现（返回 `not_supported`），没有与远端 daemon 相同语义的目录树列举。daemon 端有
//! 一个 Rust 实现（`kiana-env` 的 `list.rs`），本模块负责
//! 把请求发过去并解析二进制结果，让本地与远端环境共用同一份列举逻辑：
//! - `RemoteExecutionEnv::list_files`（`remote_env.rs`）解析路径后直接调用 [`list_files_over`]；
//! - `LocalExecutionEnv::list_files` 先用本地 `NodeExecutionEnv::absolute_path` 解析，再调用它。
//!
//! # 主要流程
//! 1. 校验 [`ListFilesOptions`] 的数值上界（复现 TS 的 `Number.isSafeInteger` 检查）；
//! 2. 请求前检查 `Context` 的取消信号；
//! 3. 通过 `hello.features` 判断 daemon 是否支持 `listFiles`（旧 daemon 没有该操作）；
//! 4. 只把调用方显式给出的字段放进 JSON，其余交给 daemon 默认值；
//! 5. 请求返回后再检查一次取消，然后按 `<kind 字节><UTF-8 路径><NUL>` 解析 payload；
//! 6. 用 daemon 的 `truncated` 标志包装成 [`FileListing`]。
//!
//! # 注意事项
//! - payload 解析是防御式的：遇到未知 kind 字节只跳过该记录，遇到缺失的 NUL 就停止；这些情况
//!   不会报错，因为 daemon 正常输出时不会出现。
//! - 条目的 `path` 是相对被列出目录的路径，用 `/` 分隔（[`FileListEntry`] 的约定），不是完整路径。
//! - 不支持 `listFiles` 的旧 daemon 返回 `not_supported`，调用方可以据此回退（集成测试
//!   `reports listFiles as not supported by daemons released before it` 锁定）。
//!
//! TS: packages/env/src/list-files.ts

use std::sync::Arc;

use async_trait::async_trait;
use indexmap::IndexMap;
use kiana_chord::context::Context;
use kiana_durable::env::node::{NodeEnvOptions, NodeExecutionEnv, NodeWatchOptions};
use kiana_durable::env::{
    BinaryReader, CreateDirOptions, CreateTempFileOptions, DirReader, FileContent, FileError,
    FileErrorCode, FileInfo, FileKind, FileListEntry, FileListing, FileSystem, FileWatcher,
    ListFilesOptions, OpenBinaryReaderOptions, ReadTextLinesOptions, RemoveOptions, Shell,
    ShellCommand, ShellExecOptions, ShellExecResult, TextLineReader, WatchCallback, WatchTarget,
};
use serde_json::Value;

use crate::connection::{Connection, Json, RequestOptions};
use crate::errors::{EnvError, abort_result, to_file_error};

/// `listFiles` 记录的 kind 字节：`f`、`d`、`l`。
///
/// 与 daemon 的 `list.rs` 一致；其它字节返回 `None`，调用方跳过该记录但继续解析后续记录。
fn kind_of(byte: u8) -> Option<FileKind> {
    match byte {
        b'f' => Some(FileKind::File),
        b'd' => Some(FileKind::Directory),
        b'l' => Some(FileKind::Symlink),
        _ => None,
    }
}

/// JS 的 `Number.MAX_SAFE_INTEGER`。
///
/// 值为 2^53−1。`max_entries`/`max_bytes` 在 Rust 里是 `usize`，负数与非整数在类型层面已
/// 不可能，因此只需复现 TS 的上界检查；`timeout_ms` 是 `f64`，还要单独检查有限性与整数性。
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

/// `listFilesOver`：经 daemon 列一个绝对路径。daemon 不支持 `listFiles`（早于该功能的版本）时以
/// `not_supported` 失败，调用方据此回退到自己的遍历。
///
/// 参数：
/// - `path`：必须是绝对路径。两个调用方都会先解析：`LocalExecutionEnv::list_files` 用
///   `NodeExecutionEnv::absolute_path`，`RemoteExecutionEnv::list_files` 用其私有的 `resolve`
///   （与该类型的 `absolute_path` 同一套规则）。
/// - `options`：`None` 表示全部用 daemon 默认值；`Some` 里为 `None` 的字段不会出现在 JSON 中。
/// - `context`：只用于 `abort_signal()`；取消会在发请求前和收到回复后各检查一次。
///
/// 返回的 [`FileListing`] 中 `entries[i].path` 是相对 `path` 的路径（`/` 分隔）；顺序由
/// daemon 决定（深度优先、父目录在前、兄弟按名字字节序，见 daemon 的 `list.rs`）。
/// 失败路径分两段：选项校验（`max_entries`/`max_bytes`/`timeout_ms`）与首次取消检查直接返回
/// [`FileError`]；`async` 块内的连接、请求与回复后取消错误在末尾经 [`to_file_error`] 归一化为
/// [`FileError`]。
///
/// TS: packages/env/src/list-files.ts#listFilesOver
pub async fn list_files_over(
    connection: &Connection,
    path: &str,
    options: Option<&ListFilesOptions>,
    context: &dyn Context,
) -> Result<FileListing, FileError> {
    // 1. 选项校验：Rust 的类型已经排除了负值与非整数（usize），这里只复现 TS 的
    //    `Number.isSafeInteger` 上界；`timeout_ms` 是 f64，需要完整检查。
    if let Some(options) = options {
        if let Some(value) = options.max_entries
            && value > MAX_SAFE_INTEGER as usize
        {
            return Err(FileError::new(
                FileErrorCode::Invalid,
                "maxEntries must be a non-negative safe integer",
            )
            .with_path(path));
        }
        if let Some(value) = options.max_bytes
            && value > MAX_SAFE_INTEGER as usize
        {
            return Err(FileError::new(
                FileErrorCode::Invalid,
                "maxBytes must be a non-negative safe integer",
            )
            .with_path(path));
        }
        if let Some(value) = options.timeout_ms
            && (!value.is_finite()
                || value.fract() != 0.0
                || value < 0.0
                || value > MAX_SAFE_INTEGER)
        {
            return Err(FileError::new(
                FileErrorCode::Invalid,
                "timeoutMs must be a non-negative safe integer",
            )
            .with_path(path));
        }
    }
    // 2. 发请求前检查取消：已中止就不产生任何网络往返。
    let signal = context.abort_signal();
    if let Some(aborted) = abort_result::<FileListing>(signal.as_ref(), Some(path)) {
        return aborted;
    }
    let result: Result<FileListing, EnvError> = async {
        // 3. 旧 daemon 不认识 listFiles：`features` 缺失或列表里没有它 → not_supported。
        let info = connection.info().await.map_err(EnvError::Remote)?;
        let supported = info
            .features
            .as_ref()
            .is_some_and(|features| features.iter().any(|feature| feature == "listFiles"));
        if !supported {
            return Err(EnvError::File(
                FileError::new(
                    FileErrorCode::NotSupported,
                    "The kiana-env daemon does not list files; update it",
                )
                .with_path(path),
            ));
        }
        // 4. 默认值（hidden/gitignore=true 等）只在 daemon 一侧维护，客户端不复制一份，
        //    避免两端默认值漂移。
        // Fields left undefined are not sent; the daemon applies the defaults.
        let mut body = Json::new();
        body.insert("path".to_owned(), Value::String(path.to_owned()));
        if let Some(options) = options {
            if let Some(hidden) = options.hidden {
                body.insert("hidden".to_owned(), Value::Bool(hidden));
            }
            if let Some(gitignore) = options.gitignore {
                body.insert("gitignore".to_owned(), Value::Bool(gitignore));
            }
            if let Some(exclude) = &options.exclude {
                body.insert(
                    "exclude".to_owned(),
                    Value::Array(exclude.iter().cloned().map(Value::String).collect()),
                );
            }
            if let Some(max_entries) = options.max_entries {
                body.insert("maxEntries".to_owned(), Value::from(max_entries));
            }
            if let Some(max_bytes) = options.max_bytes {
                body.insert("maxBytes".to_owned(), Value::from(max_bytes));
            }
            if let Some(timeout_ms) = options.timeout_ms {
                body.insert("timeoutMs".to_owned(), crate::js_number(timeout_ms));
            }
        }
        let reply = connection
            .request(
                "listFiles",
                body,
                RequestOptions {
                    signal: signal.clone(),
                    ..RequestOptions::default()
                },
            )
            .await
            .map_err(EnvError::Remote)?;
        // 5. 回复已到但 signal 可能在这期间中止：aborted 优先于解析结果，调用方期望取消错误而不是可能过期的列表。
        if let Some(aborted) = abort_result::<FileListing>(signal.as_ref(), Some(path)) {
            return aborted.map_err(EnvError::File);
        }
        // 6. 记录是 `<kind><path><NUL>`；缺 NUL 说明 payload 在记录中间结束，停止解析并保留
        //    已解析的条目（daemon 正常不会产生这种输出）。未知 kind 字节只跳过该记录。
        //    路径经 `decode_utf8_lossy` 解码（对应 TS 的 `bytes.toString("utf8")`）：非法字节
        //    序列替换为 U+FFFD，而不是让整个列表失败。
        // Records of a kind byte, the UTF-8 path, and a NUL.
        let mut entries = Vec::new();
        let bytes = &reply.payload;
        let mut start = 0usize;
        while start < bytes.len() {
            let Some(end) = bytes[start..].iter().position(|byte| *byte == 0) else {
                break;
            };
            let end = start + end;
            if let Some(kind) = kind_of(bytes[start]) {
                entries.push(FileListEntry {
                    path: crate::decode_utf8_lossy(&bytes[start + 1..end]),
                    kind,
                });
            }
            start = end + 1;
        }
        // 7. `truncated` 只在 daemon 明确给 JSON `true` 时为真；缺失或 false 都按未截断处理。
        let truncated = reply.json.get("truncated") == Some(&Value::Bool(true));
        Ok(FileListing { entries, truncated })
    }
    .await;
    // async 块内的错误（连接、请求、回复后的取消）统一映射；前面的选项校验与首次取消检查
    // 已经直接返回 FileError。
    result.map_err(|error| to_file_error(error, Some(path)))
}

/// `new LocalExecutionEnv(options)` 的选项：`NodeExecutionEnv` 的选项外加 `connection`。
///
/// 除 `connection` 外的四个字段（`cwd`、`shell_path`、`shell_env`、`watch`）原样传给
/// [`NodeExecutionEnv`]（本地文件与 shell 语义）；`connection` 只用于 `listFiles`。注意
/// `watch` 是 [`NodeWatchOptions`]（本机原生/轮询观察器），不是远端的
/// [`crate::watch::RemoteWatchOptions`]。
///
/// TS: packages/env/src/list-files.ts#LocalExecutionEnvOptions
#[derive(Clone)]
pub struct LocalExecutionEnvOptions {
    /// 本机 daemon 的连接，`Connection::new(vec![local_daemon().expect("local kiana-env daemon")])`，
    /// 用于 `listFiles`。
    pub connection: Arc<Connection>,
    /// 当前工作目录。
    pub cwd: String,
    /// 显式的 bash 路径。
    pub shell_path: Option<String>,
    /// 构造时固定的环境变量。
    pub shell_env: Option<IndexMap<String, String>>,
    /// 本地文件监视选项。
    pub watch: Option<NodeWatchOptions>,
}

impl LocalExecutionEnvOptions {
    /// 只给出连接与 cwd 的选项。
    pub fn new(connection: Arc<Connection>, cwd: impl Into<String>) -> Self {
        LocalExecutionEnvOptions {
            connection,
            cwd: cwd.into(),
            shell_path: None,
            shell_env: None,
            watch: None,
        }
    }

    // 投影出与 NodeExecutionEnv 共享的字段；`connection` 不属于 NodeEnvOptions。
    fn node(&self) -> NodeEnvOptions {
        NodeEnvOptions {
            cwd: self.cwd.clone(),
            shell_path: self.shell_path.clone(),
            shell_env: self.shell_env.clone(),
            watch: self.watch.clone(),
        }
    }
}

/// `NodeExecutionEnv` + 经本机 kiana-env daemon 的 `listFiles`，让本地与远端环境用同一份列文件代码。
/// 其余全部是 `NodeExecutionEnv` 的。连接归调用方所有，由调用方关闭；多个环境可以共享它。
///
/// # 边界
/// - 除 `list_files` 外的所有 `FileSystem`/`Shell` 方法都直接转发给内部的 `NodeExecutionEnv`，
///   因此本地读写、bash、watch 的行为与直接使用 `NodeExecutionEnv` 完全一致；`id()` 也返回它的
///   `"node:local"`。
/// - `list_files` 是唯一的远端路径：先用本地 `absolute_path` 解析，再把绝对路径交给
///   `connection` 背后的 daemon。因此 `connection` 必须连到**本机**的 kiana-env daemon
///   （`Connection::new(vec![local_daemon().expect("local kiana-env daemon")])`）；连到远端时，
///   列出的会是远端机器上的同名路径，与本环境的其它操作不一致。
/// - `LocalExecutionEnv` 不拥有连接：没有 `Drop`，调用方负责 `Connection::close()`。
///
/// TS: packages/env/src/list-files.ts#LocalExecutionEnv
pub struct LocalExecutionEnv {
    inner: NodeExecutionEnv,
    connection: Arc<Connection>,
}

impl std::fmt::Debug for LocalExecutionEnv {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LocalExecutionEnv")
            .field("cwd", &self.cwd())
            .finish_non_exhaustive()
    }
}

impl LocalExecutionEnv {
    /// `new LocalExecutionEnv({ connection, cwd, shellPath?, shellEnv?, watch? })`。
    pub fn new(options: LocalExecutionEnvOptions) -> Self {
        LocalExecutionEnv {
            inner: NodeExecutionEnv::new(options.node()),
            connection: options.connection,
        }
    }
}

// 除 `list_files` 外的实现全部逐方法直接转发给 NodeExecutionEnv：本地环境的生产者只把
// listFiles 换成 daemon 版本，其余语义（路径、编码、错误消息）必须与本地实现保持一致。
#[async_trait]
impl FileSystem for LocalExecutionEnv {
    fn id(&self) -> &str {
        // 对调用方仍是本地环境：id 报告 "node:local"，LocalExecutionEnv 只替换了 listFiles。
        NodeExecutionEnv::ID
    }

    fn cwd(&self) -> &str {
        self.inner.cwd()
    }

    fn set_cwd(&self, cwd: String) {
        self.inner.set_cwd(cwd);
    }

    async fn absolute_path(&self, path: &str, context: &dyn Context) -> Result<String, FileError> {
        FileSystem::absolute_path(&self.inner, path, context).await
    }

    async fn join_path(
        &self,
        parts: &[String],
        context: &dyn Context,
    ) -> Result<String, FileError> {
        FileSystem::join_path(&self.inner, parts, context).await
    }

    async fn read_text_file(&self, path: &str, context: &dyn Context) -> Result<String, FileError> {
        FileSystem::read_text_file(&self.inner, path, context).await
    }

    async fn open_text_line_reader(
        &self,
        path: &str,
        context: &dyn Context,
    ) -> Result<Arc<dyn TextLineReader>, FileError> {
        FileSystem::open_text_line_reader(&self.inner, path, context).await
    }

    async fn read_text_lines(
        &self,
        path: &str,
        options: Option<&ReadTextLinesOptions>,
        context: &dyn Context,
    ) -> Result<Vec<String>, FileError> {
        FileSystem::read_text_lines(&self.inner, path, options, context).await
    }

    async fn read_binary_file(
        &self,
        path: &str,
        context: &dyn Context,
    ) -> Result<Vec<u8>, FileError> {
        FileSystem::read_binary_file(&self.inner, path, context).await
    }

    async fn open_binary_reader(
        &self,
        path: &str,
        options: Option<&OpenBinaryReaderOptions>,
        context: &dyn Context,
    ) -> Result<Arc<dyn BinaryReader>, FileError> {
        FileSystem::open_binary_reader(&self.inner, path, options, context).await
    }

    async fn write_file(
        &self,
        path: &str,
        content: FileContent,
        context: &dyn Context,
    ) -> Result<(), FileError> {
        FileSystem::write_file(&self.inner, path, content, context).await
    }

    async fn append_file(
        &self,
        path: &str,
        content: FileContent,
        context: &dyn Context,
    ) -> Result<(), FileError> {
        FileSystem::append_file(&self.inner, path, content, context).await
    }

    async fn truncate_file(
        &self,
        path: &str,
        size: u64,
        context: &dyn Context,
    ) -> Result<(), FileError> {
        FileSystem::truncate_file(&self.inner, path, size, context).await
    }

    async fn flush_file(&self, path: &str, context: &dyn Context) -> Result<(), FileError> {
        FileSystem::flush_file(&self.inner, path, context).await
    }

    async fn rename_file(
        &self,
        source_path: &str,
        destination_path: &str,
        context: &dyn Context,
    ) -> Result<(), FileError> {
        FileSystem::rename_file(&self.inner, source_path, destination_path, context).await
    }

    async fn file_info(&self, path: &str, context: &dyn Context) -> Result<FileInfo, FileError> {
        FileSystem::file_info(&self.inner, path, context).await
    }

    async fn list_dir(
        &self,
        path: &str,
        context: &dyn Context,
    ) -> Result<Vec<FileInfo>, FileError> {
        FileSystem::list_dir(&self.inner, path, context).await
    }

    async fn list_files(
        &self,
        path: &str,
        options: Option<&ListFilesOptions>,
        context: &dyn Context,
    ) -> Result<FileListing, FileError> {
        // 先按本地路径语义解析成绝对路径：daemon 对相对路径不做格式校验，而是按它自己的
        // cwd 解释（`list::run` 只要求 path 是字符串，随后 `fs::metadata(root)` 解析），
        // 不先解析就可能列出错误目录。解析失败原样返回 NodeExecutionEnv 的 FileError，
        // 不再经过 to_file_error。
        let resolved = FileSystem::absolute_path(&self.inner, path, context).await?;
        list_files_over(&self.connection, &resolved, options, context).await
    }

    async fn open_dir_reader(
        &self,
        path: &str,
        context: &dyn Context,
    ) -> Result<Arc<dyn DirReader>, FileError> {
        FileSystem::open_dir_reader(&self.inner, path, context).await
    }

    async fn watch(
        &self,
        targets: &[WatchTarget],
        on_change: WatchCallback,
        context: &dyn Context,
    ) -> Result<Arc<dyn FileWatcher>, FileError> {
        FileSystem::watch(&self.inner, targets, on_change, context).await
    }

    async fn canonical_path(&self, path: &str, context: &dyn Context) -> Result<String, FileError> {
        FileSystem::canonical_path(&self.inner, path, context).await
    }

    async fn exists(&self, path: &str, context: &dyn Context) -> Result<bool, FileError> {
        FileSystem::exists(&self.inner, path, context).await
    }

    async fn create_dir(
        &self,
        path: &str,
        options: Option<&CreateDirOptions>,
        context: &dyn Context,
    ) -> Result<(), FileError> {
        FileSystem::create_dir(&self.inner, path, options, context).await
    }

    async fn remove(
        &self,
        path: &str,
        options: Option<&RemoveOptions>,
        context: &dyn Context,
    ) -> Result<(), FileError> {
        FileSystem::remove(&self.inner, path, options, context).await
    }

    async fn create_temp_dir(
        &self,
        prefix: Option<&str>,
        context: &dyn Context,
    ) -> Result<String, FileError> {
        FileSystem::create_temp_dir(&self.inner, prefix, context).await
    }

    async fn create_temp_file(
        &self,
        options: Option<&CreateTempFileOptions>,
        context: &dyn Context,
    ) -> Result<String, FileError> {
        FileSystem::create_temp_file(&self.inner, options, context).await
    }

    async fn cleanup(&self, context: &dyn Context) {
        // FileSystem 与 Shell 两个 trait 的 cleanup 都指向同一个 NodeExecutionEnv::cleanup。
        NodeExecutionEnv::cleanup(&self.inner, context).await;
    }
}

// Shell 也全部转发；LocalExecutionEnv 不改变命令执行方式。
#[async_trait]
impl Shell for LocalExecutionEnv {
    async fn exec(
        &self,
        command: ShellCommand,
        options: Option<&ShellExecOptions>,
        context: &dyn Context,
    ) -> Result<ShellExecResult, kiana_durable::env::ExecutionError> {
        Shell::exec(&self.inner, command, options, context).await
    }

    async fn cleanup(&self, context: &dyn Context) {
        NodeExecutionEnv::cleanup(&self.inner, context).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_bytes_map_to_the_file_kinds() {
        assert_eq!(kind_of(b'f'), Some(FileKind::File));
        assert_eq!(kind_of(b'd'), Some(FileKind::Directory));
        assert_eq!(kind_of(b'l'), Some(FileKind::Symlink));
        assert_eq!(kind_of(b'x'), None);
    }
}
