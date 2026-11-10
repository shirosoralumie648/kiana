//! storage/jsonl 目录。
//!
//! TS 源：`packages/durable/src/storage/jsonl/{index,node,storage}.ts`。
//!
//! - [`storage`] 是可移植的 JSONL 后端（`JsonlStorage`）；
//! - [`node`] 是本地 Node 文件系统的入口（`openNodeJsonlStorage`）。
//!
//! `index.ts` 的导出面（`JsonlStorage`、`JsonlCorruptionError`、`JsonlStorageOptions`、
//! `JsonlStoragePoisonedError`）在模块根可用。
//!
//! # 目录结构
//! `storage` 是可移植实现：`JsonlStorage::open` 接受任意 `FileSystem`，磁盘格式、提交点与
//! 恢复流程都定义在该模块（见其模块文档）。`node` 只做一件事：构造本地 `NodeExecutionEnv`
//! 并调用 `JsonlStorage::open`。结构上把「持久化语义」与「平台绑定」分开：需要测试替身或
//! 其他文件系统时可以直接使用 `storage`，不必引入本地 Node 语义。
//!
//! # 调用关系
//! 示例 `examples/18_json.rs`、`tests/jsonl_storage.rs`、`tests/storage_memory.rs` 与
//! `tests/storage_bench.rs` 都从本模块根导入 [`open_node_jsonl_storage`] 打开本地目录；
//! 之后的读写都是 [`JsonlStorage`] 上的 `Storage` trait 方法。持久化格式是线协议，改动
//! `storage` 会同时影响重启恢复与 TS 对照测试。
//!
//! # 导出面
//! 根上的 `pub use` 对应 TS `index.ts` 与 `node.ts`：`index.ts` 提供 [`JsonlStorage`]、
//! [`JsonlCorruptionError`]、[`JsonlStorageOptions`]、[`JsonlStoragePoisonedError`]；
//! `node.ts` 额外提供 [`open_node_jsonl_storage`] 并转出 [`JsonlStorageOptions`]（TS 的
//! `node.ts` 还转出 `JsonlStorage`，Rust 侧统一由本模块根提供）。因此 `JsonlStorageOptions`
//! 在 `storage::jsonl::node` 与模块根两个路径下都可用。

// --- 子模块 ---
pub mod node;
pub mod storage;

pub use node::open_node_jsonl_storage;
pub use storage::{
    JsonlCorruptionError, JsonlStorage, JsonlStorageOptions, JsonlStoragePoisonedError,
};
