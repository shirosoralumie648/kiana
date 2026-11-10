//! `sqlite/migrations.ts`：原子地应用所有待执行迁移，并在具名元数据表里记录版本。
//!
//! 调用方：`kiana-durable` 的 `storage::sqlite::migrations`（包装成 durable schema 的迁移
//! 器），以及本 crate 的 `tests/sqlite_node.rs`。
//!
//! # 执行流程
//! 1. **先校验，不碰数据库**：表名前缀（[`validate_sqlite_schema_prefix`]）与版本连续性
//!    都在函数调用时同步完成——早于返回的 future 被 poll，更早于开启事务。非法输入不会
//!    创建任何表。
//! 2. 在**同一个事务**里：建元数据表（若不存在）→ 插入 `(1, 0)`（已存在则忽略）→ 读取
//!    当前版本 → 版本比支持的新则失败 → 按顺序执行版本大于当前值的迁移，每条执行完
//!    立即把版本写回。
//! 3. 任一步失败，事务整体回滚：元数据表与本次已执行的 DDL/DML 都不会留下。
//!
//! # 元数据表
//! 表名由调用方给出，是单行表：`singleton = 1` 主键 + `version INTEGER`，并声明 `STRICT`
//! （版本列保证是整数）。`version` 表示最后一个成功完成的迁移版本；空迁移列表等价于只建
//! 表、版本 0。
//!
//! # 注意
//! - 迁移语句按给定顺序**原样**执行，本函数不检查也不改写它们；信任边界是“调用方提供
//!   可信的迁移列表”。
//! - 表名通过字符串插值进入 SQL（标识符不能用绑定参数），唯一防线是
//!   [`validate_sqlite_schema_prefix`]；不要绕过本函数自行拼表名。
//! - 版本必须从 1 开始连续，否则在触碰数据库前报固定错误。
//! - 失败时事务已回滚，数据库保持调用前的状态；但若适配器连回滚也失败，返回的是聚合错误
//!   （见 [`SqliteError::is_aggregate`]），此时不能假定数据库回到了调用前的状态。

use crate::sqlite::database::{SqliteBoxFuture, SqliteDatabase, SqliteError, SqliteValue};
use crate::sqlite::schema::validate_sqlite_schema_prefix;

/// 一条迁移（TS: `SqliteMigration`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqliteMigration {
    /// 版本号，必须从 1 开始连续。
    pub version: i64,
    /// 该版本的语句，按顺序执行。
    pub statements: Vec<String>,
}

impl SqliteMigration {
    /// 构造迁移。
    pub fn new(version: i64, statements: impl IntoIterator<Item = impl Into<String>>) -> Self {
        SqliteMigration {
            version,
            statements: statements.into_iter().map(Into::into).collect(),
        }
    }
}

/// 应用所有待执行迁移（TS: `applySqliteMigrations`）。
///
/// 版本必须从 1 开始连续，否则返回
/// `"SQLite migrations must have contiguous versions starting at 1"`。
/// 全部工作在一个事务里完成（首次建表、逐条迁移、逐条 `UPDATE version`）；任何一步失败
/// 都会整体回滚。表名非法或版本不连续时不会触碰数据库；这两项校验在调用时同步完成，
/// 返回的 future 只承载事务本身。
pub fn apply_sqlite_migrations<'a, D: SqliteDatabase + ?Sized>(
    database: &'a D,
    migrations: &[SqliteMigration],
    table: &str,
) -> SqliteBoxFuture<'a, Result<(), SqliteError>> {
    // 与 TS 一样，先校验版本与表名，再碰数据库。
    if let Err(error) = validate_sqlite_schema_prefix(table) {
        return Box::pin(std::future::ready(Err(SqliteError::new(error))));
    }
    for (index, migration) in migrations.iter().enumerate() {
        if migration.version != index as i64 + 1 {
            return Box::pin(std::future::ready(Err(SqliteError::new(
                "SQLite migrations must have contiguous versions starting at 1",
            ))));
        }
    }
    let migrations = migrations.to_vec();
    let table = table.to_owned();
    // 迁移与表名在返回 future 前复制：返回的 future 绑定 `database` 的生命周期 `'a`，而
    // `migrations` 的借用生命周期独立于 `'a`，事务回调需要能移动到 future 里的拥有值。
    Box::pin(async move {
        database
            .transaction(Box::new(move |transaction| {
                Box::pin(async move {
                    // singleton 主键 + CHECK (singleton = 1) 保证最多一行；下面 INSERT 保证
                    // 至少一行。IF NOT EXISTS 让重复调用与已有表兼容。
                    transaction
                        .exec(&format!(
                            "CREATE TABLE IF NOT EXISTS {table} (\n\
                             \t\t\tsingleton INTEGER PRIMARY KEY CHECK (singleton = 1),\n\
                             \t\t\tversion INTEGER NOT NULL CHECK (version >= 0)\n\
                             \t\t) STRICT"
                        ))
                        .await?;
                    // 首次使用时写入版本 0；已有行时 OR IGNORE 不会覆盖已记录的版本。
                    transaction
                        .run(
                            &format!(
                                "INSERT OR IGNORE INTO {table} (singleton, version) VALUES (1, 0)"
                            ),
                            &[],
                        )
                        .await?;
                    let row = transaction
                        .get(&format!("SELECT version FROM {table} WHERE singleton = 1"), &[])
                        .await?;
                    let Some(row) = row else {
                        // 上一条 INSERT 之后仍没有行：元数据表状态异常，报错并整体回滚。
                        return Err(SqliteError::new("SQLite schema metadata is missing"));
                    };
                    // TS 的 `row.version` 在列缺失时是 `undefined`：与任何数比较都为 false。
                    // 这里落到 0 的效果相同：不触发“版本过新”错误，但所有迁移都会被重跑。
                    let row_version = row.get_i64("version").unwrap_or(0);
                    // 版本连续（前面已校验）时，最后一条就是最大版本；空列表为 0。
                    let current_version = migrations.last().map(|migration| migration.version).unwrap_or(0);
                    if row_version > current_version {
                        // 数据库比这份代码新：不能降级，整个事务（含元数据表创建）回滚。
                        return Err(SqliteError::new(format!(
                            "SQLite schema version {row_version} is newer than supported version {current_version}"
                        )));
                    }
                    for migration in &migrations {
                        if migration.version <= row_version {
                            continue;
                        }
                        // 同一条迁移的语句按给定顺序执行；全部成功后才推进版本。
                        for statement in &migration.statements {
                            transaction.exec(statement).await?;
                        }
                        transaction
                            .run(
                                &format!("UPDATE {table} SET version = ? WHERE singleton = 1"),
                                &[SqliteValue::Integer(migration.version)],
                            )
                            .await?;
                    }
                    // 事务回调的返回类型被擦除；这里放入 `()`，由 `?` 直接丢弃（本函数
                    // 不 downcast）。
                    Ok(Box::new(()) as crate::sqlite::database::ErasedSqliteValue)
                })
            }))
            .await?;
        Ok(())
    })
}
