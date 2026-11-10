//! storage/jsonl/node.ts。
//!
//! 用本地 Node 文件系统语义（[`NodeExecutionEnv`]）打开 JSONL 存储目录。
//!
//! # 这个文件负责什么
//! [`JsonlStorage::open`] 只依赖 `FileSystem` 抽象；本文件是把它接到本机磁盘的薄适配层：
//! 取当前进程工作目录、构造 [`NodeExecutionEnv`]、转交 `open`。路径解析、目录创建、日志
//! 读取与恢复都不在这里，而在 `storage` 模块中完成。
//!
//! # 执行流程
//! 1. `process_cwd()` 读取进程工作目录，失败时返回空串（见函数文档）。
//! 2. `NodeExecutionEnv::new(cwd)`：选项只带 cwd，shell/watch 均为默认值。该实例由返回的
//!    `JsonlStorage` 通过 `fs` 字段持有：`open` 的恢复步骤以及之后的提交、回收都经它读写
//!    本机文件系统；调用方拿不到这个 `Arc`。
//! 3. `JsonlStorage::open(directory, env, context, options)`：解析绝对路径、递归创建目录、
//!    读取 `main.jsonl`（文件不存在视为空）并重放已提交状态。
//!
//! # 调用方
//! `examples/18_json.rs`、`tests/jsonl_storage.rs`、`tests/storage_memory.rs` 与
//! `tests/storage_bench.rs` 都经由本函数打开本地目录。相对 `directory` 落在哪个目录由第 1
//! 步的 cwd 决定；修改这里会影响相对路径的解析结果，绝对路径不受影响。
//!
//! # 边界与失败模式
//! - 错误全部来自 [`JsonlStorage::open`]，本函数不做映射：路径解析/建目录失败的消息形如
//!   `JSONL <action> failed: ...`，磁盘内容非法返回 `JsonlCorruptionError`。
//! - cwd 读取失败不单独报错；绝对路径的 `directory` 不受影响，相对路径则依赖
//!   `resolve_path` 重新读取的进程 cwd。
//! - 本文件不提供跨实例协调：同一目录被多个 `JsonlStorage` 同时打开时的行为由 `storage`
//!   模块的并发边界描述（该模块文档说明没有跨 I/O 的提交串行化）。

use std::sync::Arc;

use kiana_chord::context::Context;

use crate::env::node::NodeExecutionEnv;
use crate::storage::jsonl::storage::JsonlStorage;
use crate::types::BoxError;

pub use crate::storage::jsonl::storage::JsonlStorageOptions;

/// `process.cwd()`：`NodeExecutionEnv` 的初始工作目录。
///
/// TS 的 `process.cwd()` 在失败时抛错；这里与 `env/node.rs` 的 `process_cwd()` 一样退化为
/// 空串（调用方随后给出的 `directory` 若是相对路径才受影响）。
///
/// 空串只会写入新环境的 cwd 字段：`resolve_path` 对绝对路径的 `directory` 不读取 cwd，
/// 只有相对路径才以 cwd 为起点解析，因此后一种情况才可能受空串影响。
fn process_cwd() -> String {
    match std::env::current_dir() {
        Ok(path) => path.to_string_lossy().into_owned(),
        Err(_) => String::new(),
    }
}

/// 用本地 Node 文件系统打开或创建 JSONL 存储目录（TS: `openNodeJsonlStorage`）。
///
/// 每次调用都用**当前** `process.cwd()` 新建一个 [`NodeExecutionEnv`]，让相对路径按调用时的
/// cwd 解析。
///
/// # 参数
/// - `directory`：存储目录。相对路径以本次读取的 `process.cwd()` 为基准解析；目录不存在时
///   递归创建。
/// - `context`：透传给 [`JsonlStorage::open`] 的取消信号与运行时上下文。
/// - `options`：[`JsonlStorageOptions`]，控制提交时是否 `fsync`。
///
/// # 返回值
/// 已完成恢复的 [`JsonlStorage`]。读取走内存镜像（运行期读不访问磁盘），写入仍追加到磁盘；
/// 没有析构逻辑，结束时应调用 `Storage::close`。
///
/// # Errors
/// 同 [`JsonlStorage::open`]：路径解析或建目录失败返回 `JSONL <action> failed: ...`，磁盘
/// 结构非法返回 `JsonlCorruptionError`。本函数不做额外包装。
pub async fn open_node_jsonl_storage(
    directory: &str,
    context: &dyn Context,
    options: JsonlStorageOptions,
) -> Result<JsonlStorage, BoxError> {
    JsonlStorage::open(
        directory,
        Arc::new(NodeExecutionEnv::new(process_cwd())),
        context,
        options,
    )
    .await
}
