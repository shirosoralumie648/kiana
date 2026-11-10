//! `storage/sqlite/node.ts`：Node 内置 SQLite（`rusqlite` 等价物）上的 durable 存储。
//!
//! 直接再导出 chord 的 Node 驱动，并把 [`SqliteStorageOptions`] 与
//! [`NodeSqliteDatabaseOptions`] 合成 [`NodeSqliteStorageOptions`]（TS 的交叉类型）。
//!
//! 与 TS 相同的细节：前缀在**打开数据库之前**校验，因此非法前缀不会创建目录或数据库文件。
//!
//! # 打开顺序与失败模式
//! 1. 从选项里取出 `prefix`；`Some` 时先调用 `validate_sqlite_schema_prefix` 校验；
//! 2. 校验通过后才调用 [`open_node_sqlite_database`]（它会递归创建父目录并打开文件）；
//! 3. 交给 [`SqliteStorage::open`]，由后者再次校验前缀、应用迁移并读取 `durable_metadata`。
//!
//! 第 1 步是刻意的重复校验：第 2 步一旦执行就会打开连接（文件路径还会先递归创建父目录），
//! 先校验才能保证非法前缀不产生副作用（`tests/sqlite_storage.rs` 的
//! `rejects_an_invalid_prefix_before_creating_the_database` 覆盖）。
//!
//! 错误统一装箱为 [`crate::types::BoxError`]：前缀错误是 [`SqliteSchemaError`]；打开、
//! 迁移、元数据错误保持原类型，`Display` 文案不变（例如元数据被删时报
//! `Durable SQLite metadata is missing`）。
//!
//! # 编译门控
//! 本模块只在 feature `sqlite` 下编译（父模块的 `#[cfg(feature = "sqlite")]`），
//! 该 feature 同时打开 `rusqlite` 与 `kiana-chord/sqlite-node`。

use crate::storage::sqlite::schema::{SqliteSchemaError, validate_sqlite_schema_prefix};
use crate::storage::sqlite::storage::{SqliteStorage, SqliteStorageOptions};
use crate::types::BoxError;

pub use kiana_chord::sqlite::node::{
    NodeSqliteDatabase, NodeSqliteDatabaseOptions, open_node_sqlite_database,
};

/// Node SQLite 连接设置加 durable 存储选项（TS: `NodeSqliteStorageOptions`）。
///
/// TS 用交叉类型表示“连接设置 + `prefix`”；Rust 用组合结构体保留每个字段的可选性与
/// 默认值。连接字段由 [`Self::database_options`] 交给打开器，`prefix` 由 `From` 实现
/// 交给 [`SqliteStorageOptions`]，两者在 [`open_node_sqlite_storage`] 里合流。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NodeSqliteStorageOptions {
    /// WAL 自动 checkpoint 阈值（页）；`None` 用默认 1000，`0` 关闭。
    pub wal_auto_checkpoint_pages: Option<i64>,
    /// SQLite 等待竞争文件锁的毫秒数；`None` 用适配器默认 5000。
    pub busy_timeout_ms: Option<u64>,
    /// durable schema 的表/索引前缀。
    ///
    /// `None` 表示不加前缀。`Some("")` 不是“无前缀”：打开时它会被
    /// `validate_sqlite_schema_prefix` 以“空串不是合法标识符”拒绝（规则见
    /// [`crate::storage::sqlite::schema::create_sqlite_schema`]）；这里有意不把空串
    /// 归一成 `None`，以保持与 TS 相同的报错。
    pub prefix: Option<String>,
}

impl NodeSqliteStorageOptions {
    /// 拆出 chord 的连接设置（TS 的 `const { prefix, ...databaseOptions }`）。
    ///
    /// `prefix` 不属于连接设置，因此不在这里返回；它由 `From<NodeSqliteStorageOptions>
    /// for SqliteStorageOptions` 单独取出。
    pub fn database_options(&self) -> NodeSqliteDatabaseOptions {
        NodeSqliteDatabaseOptions {
            wal_auto_checkpoint_pages: self.wal_auto_checkpoint_pages,
            busy_timeout_ms: self.busy_timeout_ms,
        }
    }
}

// 只搬运 `prefix`：连接字段由 `database_options` 单独取出，两个目标类型各自持有自己那部分。
impl From<NodeSqliteStorageOptions> for SqliteStorageOptions {
    fn from(options: NodeSqliteStorageOptions) -> Self {
        SqliteStorageOptions {
            prefix: options.prefix,
        }
    }
}

/// 打开或创建基于 Node 内置 SQLite 的文件存储（TS: `openNodeSqliteStorage`）。
///
/// `prefix` 先经过 [`validate_sqlite_schema_prefix`] 校验——与 TS 一样在校验之前不会触碰
/// 文件系统；随后打开数据库并交给 [`SqliteStorage::open`]。
///
/// 参数：`path` 是 SQLite 文件路径（`:memory:` 由驱动当作内存库处理，不创建目录）；
/// `options` 同时携带连接设置与 durable schema 前缀。
///
/// 返回已完成迁移的 [`SqliteStorage`]（调用方通常放进 `Arc<dyn Storage>`）。错误装箱为
/// [`BoxError`]，可用 `downcast_ref` 取回 [`SqliteSchemaError`]、chord 的 `SqliteError`
/// 或 [`crate::storage::sqlite::storage::SqliteStorageError`]（例如 `durable_metadata`
/// 行缺失）；迁移失败时 `SqliteStorage::open` 还会尝试关闭数据库并吞掉关闭错误，
/// 最终返回最初的失败。
pub async fn open_node_sqlite_storage(
    path: &str,
    options: NodeSqliteStorageOptions,
) -> Result<SqliteStorage, BoxError> {
    let SqliteStorageOptions { prefix } = SqliteStorageOptions::from(options.clone());
    if let Some(prefix) = prefix.as_deref() {
        validate_sqlite_schema_prefix(prefix)
            .map_err(|message| Box::new(SqliteSchemaError { message }) as BoxError)?;
    }
    let database = open_node_sqlite_database(path, options.database_options())
        .await
        .map_err(|failure| Box::new(failure) as BoxError)?;
    SqliteStorage::open(
        std::sync::Arc::new(database),
        SqliteStorageOptions { prefix },
    )
    .await
}
