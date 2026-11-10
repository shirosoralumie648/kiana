//! storage/sqlite 目录。
//!
//! 与 TS `packages/durable/src/storage/sqlite/` 一一对应：
//! - `schema`：迁移与运行期查询共用的标识符表；
//! - `migrations`：不可变迁移历史与迁移器；
//! - `storage`：`SqliteStorage`（可移植 SQLite 门面之上的 `Storage`）；
//! - `database`：chord 门面类型的再导出；
//! - `node`：`rusqlite` 驱动与 `openNodeSqliteStorage`（feature `sqlite`）；
//! - `cloudflare`：不移植的 Durable Object 占位（ARCHITECTURE §8 第 3 条）。
//!
//! # 边界与依赖
//! 本目录只实现“SQLite 后端”这一件事：不定义记录类型，也不规定 `Storage` 的语义。
//! 记录类型与 [`crate::types::Storage`] 契约来自 `crate::types`，SQL 执行/事务由
//! [`kiana_chord::sqlite`] 的异步门面提供。依赖方向是 `node` → `storage` + `schema`，
//! `storage` → `schema` + `migrations`；`database` 只是转出口 chord 的门面类型，
//! 不依赖其他子模块。
//!
//! # 编译门控
//! 只有 `node` 需要 feature `sqlite`（它同时打开 `rusqlite` 与 `kiana-chord/sqlite-node`）。
//! `schema`/`migrations`/`database`/`cloudflare` 无条件编译：`cloudflare` 的 feature 开关
//! 不改变本目录 cloudflare 模块的行为（该模块没有 `cfg`，打开器永远返回“不支持”）。
//!
//! # 阅读顺序
//! 1. [`schema`]：前缀校验与标识符表；2. [`migrations`]：建表 SQL 与版本；
//! 3. [`storage`]：`Storage` 的 SQL 语义；4. `node`：把真实文件驱动接到上面三者。

pub mod cloudflare;
pub mod database;
pub mod migrations;
#[cfg(feature = "sqlite")]
pub mod node;
pub mod schema;
pub mod storage;

pub use migrations::{
    CURRENT_SQLITE_SCHEMA_VERSION, SQLITE_MIGRATIONS, SqliteMigration, apply_sqlite_migrations,
};
pub use storage::{SqliteStorage, SqliteStorageOptions};
