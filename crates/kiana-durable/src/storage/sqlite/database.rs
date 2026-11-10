//! `storage/sqlite/database.ts`：可移植 SQLite 门面的再导出。
//!
//! TS 只再导出 chord 的 `SqliteDatabase` / `SqliteExecutor` / `SqliteValue` 三个类型；
//! Rust 同样直接复用 [`kiana_chord::sqlite`] 的实现，不包装、不复制。
//!
//! # 为什么是再导出
//! 与 TS 一样直接再导出。durable 的 SQL 代码只通过这套异步门面执行语句（`SqliteStorage`
//! 以 `Arc<dyn SqliteDatabase>` 接收驱动）。复用 chord 的 trait 与类型，保证 durable 与
//! 驱动共享同一套值/行/错误定义，避免出现两份类型分叉。
//!
//! # 边界
//! 与 TS 的再导出面一致，只暴露这三个名字。`SqliteRow`、`SqliteError`、
//! `SqliteDatabaseExt` 等仍应直接从 [`kiana_chord::sqlite`] 引用，本模块不转出口。

pub use kiana_chord::sqlite::database::{SqliteDatabase, SqliteExecutor, SqliteValue};
