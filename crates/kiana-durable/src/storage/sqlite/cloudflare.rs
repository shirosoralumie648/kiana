//! `storage/sqlite/cloudflare.ts`——**不移植**（决策：ARCHITECTURE.md §8 第 3 条）。
//!
//! TS 只是把 `@earendil-works/chord/sqlite/cloudflare` 的 `DurableObjectSqliteDatabase` 与
//! `DurableObjectSqliteStorage` 再导出，并加一个 `openDurableObjectSqliteStorage`。Rust 侧没有
//! `DurableObjectStorage` 等价物（它是 Workers 运行时注入的 SQL 执行器），因此无论
//! feature `cloudflare` 是否开启，本模块都由父模块无条件编译（`sqlite/mod.rs` 里是没有
//! `cfg` 的 `pub mod cloudflare;`），打开器也始终返回
//! [`StorageBackendUnsupported::Cloudflare`]，`Display` 逐字
//! `Cloudflare Durable Object storage is not supported in the Rust build`。
//!
//! 真正的 SQLite 后端是 [`super::database`]/[`super::storage`]（feature `sqlite`，`rusqlite`）。
//!
//! # 注意事项
//! - `cloudflare` feature 名保留是为了将来接入 Workers 目标时只替换实现
//!   （ARCHITECTURE §8 第 3 条）；它当前不参与本模块的编译门控。
//! - 占位类型没有字段：TS 里它们的实例来自 Workers 注入的 `ctx.storage`，Rust 侧没有
//!   对应运行时，构造不出有意义的连接参数。`Debug`/`Clone`/`Default` 只是保持再导出类型
//!   可构造的同形性，`tests/sqlite_cloudflare.rs` 断言了 `Debug` 名称与错误文案。
//! - 错误类型复用 [`crate::storage::indexeddb::StorageBackendUnsupported`]，而不是在
//!   本模块另建一个：两个后端共享同一个“浏览器/边缘运行时不可用”的 `Display` 契约。

pub use crate::storage::indexeddb::StorageBackendUnsupported;

/// `DurableObjectSqliteStorage` 的占位（TS 是 `chord/sqlite/cloudflare` 的再导出）。
///
/// 空结构体：本类型只用于让 [`open_durable_object_sqlite_storage`] 的参数类型存在，
/// 不持有连接或状态。
#[derive(Debug, Clone, Default)]
pub struct DurableObjectSqliteStorage;

/// `DurableObjectSqliteDatabase` 的占位。
///
/// 空结构体，永远不会被真正构造为可用的数据库句柄；打开器只把它用作 `Ok` 类型，
/// 但没有任何路径会返回 `Ok`。
#[derive(Debug, Clone, Default)]
pub struct DurableObjectSqliteDatabase;

/// `openDurableObjectSqliteStorage(...)`：永远返回“不支持”。
///
/// 不做任何 I/O，也不检查 `_options`：参数只为与 TS 的签名同形。返回
/// `Err(StorageBackendUnsupported::Cloudflare)`，`Display` 逐字
/// `Cloudflare Durable Object storage is not supported in the Rust build`
/// （`tests/sqlite_cloudflare.rs` 断言）。调用方不应把它当作可重试错误——它表示该后端
/// 在此构建中根本不存在。
pub fn open_durable_object_sqlite_storage(
    _options: DurableObjectSqliteStorage,
) -> Result<DurableObjectSqliteDatabase, StorageBackendUnsupported> {
    Err(StorageBackendUnsupported::Cloudflare)
}
