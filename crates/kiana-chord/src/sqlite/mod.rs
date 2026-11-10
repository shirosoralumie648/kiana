//! sqlite 目录（feature `sqlite`）：可移植 SQLite 接口。
//!
//! TS: `packages/chord/src/sqlite/{index,database,schema,migrations,node,cloudflare}.ts`。
//! `database`/`schema`/`migrations` 与后端无关，始终编译；`node`（`rusqlite`）由
//! feature `sqlite-node` 打开；`cloudflare` 是明确的 not supported 占位（ARCHITECTURE §8）。
//!
//! # 这个模块解决什么问题
//! 上层（主要是 `kiana-durable` 的 `storage::sqlite`）只依赖一套与后端无关的异步门面：
//! [`SqliteDatabase`] / [`SqliteExecutor`] 定义 SQL 操作与事务契约，[`SqliteValue`] /
//! [`SqliteRow`] 定义绑定值与结果行。具体驱动实现 trait 后以 `Arc<dyn SqliteDatabase>`
//! 注入（`kiana-durable` 的 `storage::sqlite::storage` 就是这么用的），调用方不需要知道
//! 是文件库还是别的后端。
//!
//! # 文件分工与阅读顺序
//! 1. [`database`]：值/行/错误类型，`SqliteExecutor`、`SqliteDatabase` 与泛型事务辅助
//!    `SqliteDatabaseExt`。先读它的 trait 契约。
//! 2. [`schema`]：把前缀当 SQLite 标识符拼进 SQL 之前做字符集校验。
//! 3. [`migrations`]：在单个事务里按版本应用迁移，并把版本写进具名元数据表。
//! 4. `node`：`rusqlite` 实现，含串行队列、事务、取消，以及与 `node:sqlite` 的错误/绑定兼容；
//!    仅 feature `sqlite-node` 编译（`kiana-durable` 的 `sqlite` feature 会打开它）。
//! 5. `cloudflare`：Durable Object SQLite 的占位，构造入口永远返回“不支持”。
//!
//! # feature 关系
//! `database`/`schema`/`migrations` 不依赖 `rusqlite`，任何 feature 组合都能编译；
//! `sqlite-node`、`sqlite-cloudflare`、`telemetry-sqlite` 都会打开 `sqlite`
//! （见 crate 的 `Cargo.toml`），但只有 `sqlite-node` 会编译出 `node` 模块。

pub mod cloudflare;
pub mod database;
pub mod migrations;
#[cfg(feature = "sqlite-node")]
pub mod node;
pub mod schema;

pub use database::{
    ErasedSqliteValue, SqliteBoxFuture, SqliteDatabase, SqliteDatabaseExt, SqliteError,
    SqliteErrorKind, SqliteExecutor, SqliteRow, SqliteValue, TransactionCallback,
};
pub use migrations::{SqliteMigration, apply_sqlite_migrations};
pub use schema::validate_sqlite_schema_prefix;
