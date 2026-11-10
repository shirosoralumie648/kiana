//! `sqlite/database.ts`：可移植的 SQLite 门面。
//!
//! TS 的 `transaction<T>` 是泛型回调；Rust 侧沿用 `kiana-telemetry` 的
//! “对象安全擦除形式 + 泛型扩展 trait” 复刻（[`SqliteDatabase::transaction`] 与
//! [`SqliteDatabaseExt::transaction_typed`]）。
//!
//! 行对象的形状：TS 是 `Record<string, SqliteValue>`，列名→值；Rust 用
//! [`SqliteRow`]（`IndexMap` 保留列序）。
//!
//! # 为什么分成“擦除的事务”与“泛型扩展”两段
//! 泛型方法会让 trait 无法作为 trait object 使用，而实际调用方需要
//! `Arc<dyn SqliteDatabase>` 来注入不同驱动。折中是：[`SqliteDatabase::transaction`]
//! 接收返回 [`ErasedSqliteValue`]（`Box<dyn Any + Send>`）的回调，保持对象安全；
//! [`SqliteDatabaseExt::transaction_typed`] 再把具体类型 `T` 包进去并 `downcast` 还原。
//! 因此 `T` 必须是 `Send + 'static`，类型不匹配时得到固定的
//! `"transaction callback returned an unexpected type"` 错误，不会 panic。
//!
//! # 数据结构的不变量
//! - [`SqliteRow`] 的键是列名，顺序是数据库返回的列顺序；同一列名出现两次时，
//!   后来的值覆盖先前的值、位置保持首次插入的位置（`IndexMap::insert` 的语义）。
//! - [`SqliteValue`] 区分整数与浮点；`SqliteValue::Real` 的 `PartialEq` 是派生的，
//!   因此 `NaN` 不等于自身。
//! - [`SqliteError`] 的 `Display` 只输出 `message`；`AggregateError` 的细节在
//!   `errors` 字段里，消息是固定文案。

use std::any::Any;
use std::future::Future;
use std::pin::Pin;

use indexmap::IndexMap;

/// 异步返回值：适配器等异步 API 返回的 boxed future。
///
/// 用 `Box<dyn Future + Send>` 而不是 `async fn`，是为了让 [`SqliteExecutor`] /
/// [`SqliteDatabase`] 保持对象安全（驱动以 `Arc<dyn SqliteDatabase>` 注入），同时要求
/// future 可被发送到其他线程执行。
pub type SqliteBoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// 便携 SQLite 驱动支持的值（TS: `SqliteValue = null | number | bigint | string | Uint8Array`）。
///
/// TS 的 `number` 同时表示整数与浮点；Rust 侧显式区分 [`SqliteValue::Integer`] 与
/// [`SqliteValue::Real`]（绑定语义与 `node:sqlite` 对整数/浮点的区分一致）。
#[derive(Debug, Clone, PartialEq)]
pub enum SqliteValue {
    /// `null`。
    Null,
    /// 整数（TS 的整数值 `number` / `bigint`）。
    Integer(i64),
    /// 浮点数。
    Real(f64),
    /// 文本。
    Text(String),
    /// BLOB（TS `Uint8Array`）。
    Blob(Vec<u8>),
}

impl SqliteValue {
    /// 类型名（诊断用）。
    pub fn type_name(&self) -> &'static str {
        match self {
            SqliteValue::Null => "null",
            SqliteValue::Integer(_) => "integer",
            SqliteValue::Real(_) => "real",
            SqliteValue::Text(_) => "text",
            SqliteValue::Blob(_) => "blob",
        }
    }

    /// 是否为 SQL NULL。
    pub fn is_null(&self) -> bool {
        matches!(self, SqliteValue::Null)
    }
}

impl From<i64> for SqliteValue {
    fn from(value: i64) -> Self {
        SqliteValue::Integer(value)
    }
}

impl From<i32> for SqliteValue {
    fn from(value: i32) -> Self {
        SqliteValue::Integer(i64::from(value))
    }
}

impl From<f64> for SqliteValue {
    fn from(value: f64) -> Self {
        SqliteValue::Real(value)
    }
}

impl From<&str> for SqliteValue {
    fn from(value: &str) -> Self {
        SqliteValue::Text(value.to_owned())
    }
}

impl From<String> for SqliteValue {
    fn from(value: String) -> Self {
        SqliteValue::Text(value)
    }
}

impl From<Vec<u8>> for SqliteValue {
    fn from(value: Vec<u8>) -> Self {
        SqliteValue::Blob(value)
    }
}

impl<T: Into<SqliteValue>> From<Option<T>> for SqliteValue {
    fn from(value: Option<T>) -> Self {
        match value {
            Some(value) => value.into(),
            None => SqliteValue::Null,
        }
    }
}

/// 一行查询结果：列名→值，保留列序（TS 的行是普通对象）。
///
/// 键是数据库返回的列名。列名重复时（例如连接两张都有 `id` 的表），后写入的值覆盖先前的
/// 值，位置保持首次插入的顺序；调用方不能依赖两个同名列同时可见。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SqliteRow(pub IndexMap<String, SqliteValue>);

impl SqliteRow {
    /// 空行。
    pub fn new() -> Self {
        SqliteRow::default()
    }

    /// 由 `(列名, 值)` 对构造（测试与适配器使用）。
    ///
    /// 按迭代顺序插入；重复列名与 [`SqliteRow`] 的规则一致（值覆盖、位置保留）。
    pub fn from_pairs<I, K>(pairs: I) -> Self
    where
        I: IntoIterator<Item = (K, SqliteValue)>,
        K: Into<String>,
    {
        let mut values = IndexMap::new();
        for (name, value) in pairs {
            values.insert(name.into(), value);
        }
        SqliteRow(values)
    }

    /// 列值。
    pub fn get(&self, column: &str) -> Option<&SqliteValue> {
        self.0.get(column)
    }

    /// 列是否存在。
    pub fn contains(&self, column: &str) -> bool {
        self.0.contains_key(column)
    }

    /// 列数。
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// 是否没有列。
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// 整数列（只接受 [`SqliteValue::Integer`]）。
    pub fn get_i64(&self, column: &str) -> Option<i64> {
        match self.0.get(column) {
            Some(SqliteValue::Integer(value)) => Some(*value),
            _ => None,
        }
    }

    /// 浮点列（整数会提升为 f64）。
    pub fn get_f64(&self, column: &str) -> Option<f64> {
        match self.0.get(column) {
            Some(SqliteValue::Real(value)) => Some(*value),
            Some(SqliteValue::Integer(value)) => Some(*value as f64),
            _ => None,
        }
    }

    /// 文本列。
    pub fn get_text(&self, column: &str) -> Option<&str> {
        match self.0.get(column) {
            Some(SqliteValue::Text(value)) => Some(value),
            _ => None,
        }
    }

    /// BLOB 列。
    pub fn get_blob(&self, column: &str) -> Option<&[u8]> {
        match self.0.get(column) {
            Some(SqliteValue::Blob(value)) => Some(value),
            _ => None,
        }
    }

    /// 借用底层映射。
    pub fn as_map(&self) -> &IndexMap<String, SqliteValue> {
        &self.0
    }

    /// 取出底层映射。
    pub fn into_map(self) -> IndexMap<String, SqliteValue> {
        self.0
    }
}

/// 错误类别：TS 的 `Error` 与 `AggregateError`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SqliteErrorKind {
    /// 普通 `Error`。
    Error,
    /// `AggregateError`。
    AggregateError,
}

/// SQLite 错误。TS 侧错误是不透明的；Rust 只保证 [`Display`](std::fmt::Display)
/// （`Error.message`）稳定，并保留 `AggregateError` 的 `errors` 与固定消息。
///
/// 没有错误码字段（驱动相关的错误码不会向上暴露）；需要区分的调用方只能比较
/// `message` 或 `kind`。
#[derive(Debug, Clone, PartialEq)]
pub struct SqliteError {
    /// `Error.message`。
    pub message: String,
    /// 类别。
    pub kind: SqliteErrorKind,
    /// `AggregateError.errors`（普通错误为空）。
    pub errors: Vec<SqliteError>,
}

impl SqliteError {
    /// 普通错误。
    pub fn new(message: impl Into<String>) -> Self {
        SqliteError {
            message: message.into(),
            kind: SqliteErrorKind::Error,
            errors: Vec::new(),
        }
    }

    /// `AggregateError([error, rollbackError], "SQLite transaction failed and rollback failed")`。
    pub fn aggregate(errors: Vec<SqliteError>) -> Self {
        SqliteError {
            message: "SQLite transaction failed and rollback failed".to_owned(),
            kind: SqliteErrorKind::AggregateError,
            errors,
        }
    }

    /// 是否为聚合错误。
    pub fn is_aggregate(&self) -> bool {
        self.kind == SqliteErrorKind::AggregateError
    }
}

impl std::fmt::Display for SqliteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SqliteError {}

/// 擦除后的事务回调返回值（调用方 downcast）。
///
/// `Any` 要求被擦除的 `T: 'static`；[`SqliteDatabaseExt::transaction_typed`] 负责把回调
/// 返回的 `T` 装箱，并在事务提交后 downcast 回来。
pub type ErasedSqliteValue = Box<dyn Any + Send>;

/// 擦除后的事务回调：接收事务句柄，返回擦除值。
///
/// 回调必须把所有工作放在传入的句柄上（直接调用数据库会等待当前事务，见
/// [`SqliteDatabase::transaction`]）。适配器负责在回调失败时回滚，并把回调错误
/// 原样（除非回滚也失败）交还给调用方。
pub type TransactionCallback<'a> = Box<
    dyn FnOnce(
            Box<dyn SqliteExecutor + 'a>,
        ) -> SqliteBoxFuture<'a, Result<ErasedSqliteValue, SqliteError>>
        + Send
        + 'a,
>;

/// 数据库与事务句柄共享的异步 SQL 操作（TS: `SqliteExecutor`）。
///
/// `exec` 执行不带绑定的 SQL 文本，可含多条语句；`run`/`get`/`all` 执行一条语句并带
/// 位置绑定。适配器可能按 SQL 文本缓存预编译语句，因此调用方应使用绑定而不是拼接。
///
/// 四个方法都返回 [`SqliteBoxFuture`]，所以 trait 是对象安全的：事务句柄以
/// `Box<dyn SqliteExecutor + '_>` 交给回调。空 SQL、多余参数、`run` 遇到返回行等具体
/// 语义由适配器定义，契约见 `node` 模块的实现与测试。
pub trait SqliteExecutor: Send + Sync {
    /// 执行 SQL 文本（可多条语句）。
    fn exec<'a>(&'a self, sql: &'a str) -> SqliteBoxFuture<'a, Result<(), SqliteError>>;
    /// 执行一条语句。
    fn run<'a>(
        &'a self,
        sql: &'a str,
        params: &'a [SqliteValue],
    ) -> SqliteBoxFuture<'a, Result<(), SqliteError>>;
    /// 执行查询并返回第一行（没有行为 `None`）。
    fn get<'a>(
        &'a self,
        sql: &'a str,
        params: &'a [SqliteValue],
    ) -> SqliteBoxFuture<'a, Result<Option<SqliteRow>, SqliteError>>;
    /// 执行查询并返回全部行。
    fn all<'a>(
        &'a self,
        sql: &'a str,
        params: &'a [SqliteValue],
    ) -> SqliteBoxFuture<'a, Result<Vec<SqliteRow>, SqliteError>>;
}

/// 最小可移植 SQLite 数据库门面（TS: `SqliteDatabase`）。
///
/// 所有操作都是异步的，适配器可以在调用方运行时之外执行。
///
/// `transaction` 把事务句柄传给回调；回调里的所有工作都必须使用该句柄，回调结算后句柄
/// 失效。适配器必须把无关操作与其他事务排到该事务之后。事务返回的 future 在提交或回滚
/// 之后才结算。因此从回调内部调用数据库本身（包括 `transaction` 或 `close`）会等待该
/// 事务、永不结算。
///
/// 回调失败时，适配器必须先回滚再以**同一个**错误失败；若回滚也失败，必须以不同的错误
/// 失败（例如 [`SqliteError::aggregate`]），以免调用方把回调错误误认为回滚已保证。
pub trait SqliteDatabase: SqliteExecutor {
    /// 在事务中运行 `callback`。
    fn transaction<'a>(
        &'a self,
        callback: TransactionCallback<'a>,
    ) -> SqliteBoxFuture<'a, Result<ErasedSqliteValue, SqliteError>>;

    /// 关闭数据库。
    ///
    /// 关闭后其它操作的行为由适配器决定；是否幂等、失败文案也由适配器决定（`node` 实现
    /// 幂等，之后的操作返回 `database is not open`）。
    fn close(&self) -> SqliteBoxFuture<'_, Result<(), SqliteError>>;
}

/// 泛型事务辅助：TS 的 `transaction<T>`。
///
/// 文件末尾的 blanket impl 让所有 [`SqliteDatabase`]（包括 `dyn SqliteDatabase`）自动
/// 获得该方法，调用方无需为具体驱动单独实现。
pub trait SqliteDatabaseExt: SqliteDatabase {
    /// 同 [`SqliteDatabase::transaction`]，但回调返回具体类型 `T`。
    ///
    /// 失败路径与内层一致：回调错误原样返回；若适配器回滚也失败，返回适配器给出的聚合
    /// 错误。额外的一种失败是 downcast 不匹配（只可能来自实现方破坏类型约定），消息为
    /// `"transaction callback returned an unexpected type"`。
    fn transaction_typed<'a, T, F>(
        &'a self,
        callback: F,
    ) -> SqliteBoxFuture<'a, Result<T, SqliteError>>
    where
        T: Send + 'static,
        F: FnOnce(Box<dyn SqliteExecutor + 'a>) -> SqliteBoxFuture<'a, Result<T, SqliteError>>
            + Send
            + 'a,
    {
        Box::pin(async move {
            let erased: TransactionCallback<'a> = Box::new(move |handle| {
                Box::pin(async move {
                    let value = callback(handle).await?;
                    Ok(Box::new(value) as ErasedSqliteValue)
                })
            });
            let value = self.transaction(erased).await?;
            value
                .downcast::<T>()
                .map(|value| *value)
                .map_err(|_| SqliteError::new("transaction callback returned an unexpected type"))
        })
    }
}

impl<D: SqliteDatabase + ?Sized> SqliteDatabaseExt for D {}
