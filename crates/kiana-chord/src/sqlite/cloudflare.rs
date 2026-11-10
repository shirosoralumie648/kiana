//! `sqlite/cloudflare.ts`——**不移植**（决策：`ARCHITECTURE.md` §8 第 3 条）。
//!
//! TS 的适配器把 `ctx.storage.sql`（Durable Object SQLite binding）包成 `SqliteDatabase`：
//! 一个 `SerialQueue` 串行化同一 storage 上的调用，`transaction(closure)` 期间持有的排他锁，
//! 以及 `exec(query, ...bindings)` 的游标/`toArray` 语义。这些行为全部依赖 Workers 运行时
//! 提供的 `DurableObjectStorage`，Rust 侧没有等价物（`rusqlite` 是本地文件库，不是 DO 的
//! 单线程 SQLite binding）。
//!
//! 保留本模块的原因与 `kiana-durable::storage::indexeddb` 相同：文件所有权与类型面不丢。
//! feature `sqlite-cloudflare` 打开时构造入口返回明确的 not supported 错误（文案逐字），
//! 让下游按“后端不可用”处理，而不是链接失败。
//!
//! 实现者**不要**用 `rusqlite::Connection` 冒充 `DurableObjectStorage`：`SerialQueue` 的
//! “空闲时同步执行、事务中排队”语义与本地连接的事务语义不同，`sqlite-conformance` 会失败。
//!
//! # 编译与调用边界
//! 本模块在 `mod.rs` 中无条件声明，源码里没有 `#[cfg(feature = "sqlite-cloudflare")]`；
//! 因此下面的构造入口在任何 feature 组合下都存在，并且都返回错误。`Cargo.toml` 里的
//! `sqlite-cloudflare = ["sqlite"]` 目前只是启用 `sqlite` 依赖的别名，源码中没有对应的
//! `cfg` 分支。
//!
//! [`UNSUPPORTED_MESSAGE`] 与 `CloudflareSqliteUnsupported` 的 `Display` 文案是两份独立的
//! 字符串，修改时需同步；本 crate 没有测试断言二者相等（`kiana-durable` 的占位测试只覆盖
//! 它自己的错误类型）。

/// “不支持”错误的固定文案（进入 `Display`，逐字）。
///
/// 与 `CloudflareSqliteUnsupported` 的 `#[error(...)]` 文案相同，但两者没有共享常量；
/// 改一处必须改另一处。
pub const UNSUPPORTED_MESSAGE: &str =
    "Cloudflare Durable Object SQLite is not supported in the Rust build";

/// Cloudflare Durable Object SQLite 后端的占位错误。
///
/// 是零大小、`Copy` 的单位错误；`Display` 输出与 [`UNSUPPORTED_MESSAGE`] 相同的固定文案。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("Cloudflare Durable Object SQLite is not supported in the Rust build")]
pub struct CloudflareSqliteUnsupported;

/// `DurableObjectSqliteStorage`：TS 里是 `ctx.storage` 的结构化类型。
///
/// Rust 没有这个对象，保留空类型只为文档与签名形状；无法构造。
#[derive(Debug)]
pub struct DurableObjectSqliteStorage {
    _private: (),
}

/// `new DurableObjectSqliteDatabase(storage)` 的等价物（永远失败）。
///
/// TS: `sqlite/cloudflare.ts#DurableObjectSqliteDatabase`
///
/// 参数类型 `DurableObjectSqliteStorage` 的字段私有，本模块外无法构造，所以这个入口实际
/// 不可从 crate 外调用；保留它只是为了对齐 TS 的签名与类型面。函数体直接返回 `Err`，
/// 与 TS 的 constructor 一样是同步入口，没有副作用。
pub fn open_durable_object_sqlite_database(
    _storage: &DurableObjectSqliteStorage,
) -> Result<DurableObjectSqliteDatabase, CloudflareSqliteUnsupported> {
    Err(CloudflareSqliteUnsupported)
}

/// `DurableObjectSqliteDatabase`：`SqliteDatabase` 的 Workers 实现。
///
/// TS 继承 `DurableObjectSqliteExecutor` 并实现 `SqliteDatabase`；Rust 侧由
/// [`super::database`] 的 trait 定义，本占位只保留类型名。
#[derive(Debug)]
pub struct DurableObjectSqliteDatabase {
    _private: (),
}
