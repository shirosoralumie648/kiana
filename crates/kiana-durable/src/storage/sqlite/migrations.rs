//! `storage/sqlite/migrations.ts`：durable schema 的不可变迁移历史。
//!
//! 版本 1 建全部表与索引；版本 2 重建 `tasks` 表以加入 `sleeping` 状态（SQLite 不能就地修改
//! CHECK 约束）。语句文本是持久化格式的一部分，必须逐字保持稳定（缩进是制表符），因为
//! `sqlite_schema.sql` 会原样保存它们。
//!
//! 迁移器本身来自 [`kiana_chord::sqlite::apply_sqlite_migrations`]：单个事务内应用全部待执行版本，
//! 在 `durable_schema` 表里记录版本，失败整体回滚。
//!
//! # 设计原因
//! - 迁移 SQL 是**持久化格式**的一部分：SQLite 把语句原文存进 `sqlite_schema.sql`，库的
//!   schema 文本因此也是可观察状态。`tests/sqlite_migrations.rs` 的
//!   `statements_are_byte_identical_to_the_reference` 用 TS 生成的 JSON 逐字比对，任何
//!   空格/制表符变化都会被检出；修改语句前必须先确认 TS 侧同步修改。
//! - 版本 2 只能重建 `tasks`：SQLite 不支持修改既有 CHECK 约束。顺序是“建 `_v2` → 复制
//!   数据 → DROP 旧表 → RENAME”，`DROP` 会连同旧表的五个索引一起删除，所以最后按原列序
//!   重建同名索引。整个版本由 chord 的迁移器放在同一个事务里执行（见其模块文档）。
//! - `CURRENT_SQLITE_SCHEMA_VERSION` 是手写常量：`SQLITE_MIGRATIONS` 是 `LazyLock`，
//!   无法在 `const` 求值中取最后一项；测试 `current_version_matches_the_last_migration`
//!   锁定二者一致。
//!
//! # 调用方与限制
//! [`crate::storage::sqlite::storage::SqliteStorage::open`] 只调用
//! [`apply_sqlite_migrations_for_schema`]（schema 由前缀
//! 渲染），所以带前缀的数据库不会误用默认表名；[`apply_sqlite_migrations`] /
//! [`apply_sqlite_migrations_with`] 固定使用 [`DEFAULT_SQLITE_SCHEMA`] 的 `durable_schema`
//! 表名，只适合无前缀数据库。

use std::sync::LazyLock;

use kiana_chord::sqlite::database::{SqliteBoxFuture, SqliteDatabase, SqliteError};

use crate::storage::sqlite::schema::{DEFAULT_SQLITE_SCHEMA, SqliteSchema};

// `SqliteMigration` 与 TS 同形（version + statements）；定义在 chord，供 chord 的迁移器与
// 本模块共用，避免出现两种“迁移”类型无法互传。
pub use kiana_chord::sqlite::SqliteMigration;

/// 按给定 schema 渲染的迁移历史（TS: `createSqliteMigrations`）。
///
/// 返回两条迁移：版本 1 建全部表/索引并插入初始元数据行（25 条语句），版本 2 重建 `tasks`
/// 以接受 `sleeping` 状态（9 条语句）。语句只依赖 `schema` 的标识符，不含运行期数据；
/// 同一 schema 每次调用都产生相同文本。
///
/// 不要用本函数“修补”已发布的迁移：已应用过的版本不会重跑，改动只会影响新建库，并使新旧
/// 库的 `sqlite_schema.sql` 文本分叉。
pub fn create_sqlite_migrations(schema: &SqliteSchema) -> Vec<SqliteMigration> {
    let tables = &schema.tables;
    let indexes = &schema.indexes;
    // 版本 1：一次性建出全部表与索引。语句顺序与 TS 模板一致（表/索引交错），
    // 也是 golden 测试逐条比对的对象，不要重排。
    // `next_id` 是 TEXT：node:sqlite 拒绝超出 JS 安全整数范围的 INTEGER 结果。
    let initial_schema: Vec<String> = vec![
        format!(
            "CREATE TABLE {} (\n\
             \t\t\tsingleton INTEGER PRIMARY KEY CHECK (singleton = 1),\n\
             \t\t\tnext_id TEXT NOT NULL,\n\
             \t\t\tnext_seq INTEGER NOT NULL\n\
             \t\t) STRICT",
            tables.durable_metadata
        ),
        format!(
            "INSERT INTO {} (singleton, next_id, next_seq) VALUES (1, '2', 1)",
            tables.durable_metadata
        ),
        format!(
            "CREATE TABLE {} (\n\
             \t\t\tid INTEGER PRIMARY KEY,\n\
             \t\t\trecord_type TEXT NOT NULL CHECK (record_type IN ('conversation', 'entry', 'task', 'submission', 'document'))\n\
             \t\t) STRICT",
            tables.record_ids
        ),
        format!(
            "CREATE TABLE {} (\n\
             \t\t\tid INTEGER PRIMARY KEY,\n\
             \t\t\towner_conversation_id INTEGER,\n\
             \t\t\towner_task_id INTEGER,\n\
             \t\t\trecord TEXT NOT NULL CHECK (json_valid(record))\n\
             \t\t) STRICT",
            tables.conversations
        ),
        format!(
            "CREATE INDEX {} ON {} (owner_conversation_id, id)",
            indexes.conversations_by_owner_conversation, tables.conversations
        ),
        format!(
            "CREATE INDEX {} ON {} (owner_task_id, id)",
            indexes.conversations_by_owner_task, tables.conversations
        ),
        format!(
            "CREATE TABLE {} (\n\
             \t\t\tid INTEGER PRIMARY KEY,\n\
             \t\t\tconversation_id INTEGER NOT NULL,\n\
             \t\t\thead INTEGER,\n\
             \t\t\tcommit_seq INTEGER NOT NULL,\n\
             \t\t\trecord TEXT NOT NULL CHECK (json_valid(record))\n\
             \t\t) STRICT",
            tables.entries
        ),
        format!(
            "CREATE INDEX {} ON {} (conversation_id, id DESC)",
            indexes.entries_by_conversation, tables.entries
        ),
        format!(
            "CREATE INDEX {} ON {} (conversation_id, id DESC) WHERE head IS NOT NULL",
            indexes.entry_heads_by_conversation, tables.entries
        ),
        format!(
            "CREATE TABLE {} (\n\
             \t\t\tid INTEGER PRIMARY KEY,\n\
             \t\t\tconversation_id INTEGER NOT NULL,\n\
             \t\t\tkind TEXT NOT NULL,\n\
             \t\t\tstatus TEXT NOT NULL CHECK (status IN ('pending', 'running', 'waiting', 'completing', 'terminal')),\n\
             \t\t\tabort_requested INTEGER NOT NULL CHECK (abort_requested IN (0, 1)),\n\
             \t\t\tbackground INTEGER NOT NULL CHECK (background IN (0, 1)),\n\
             \t\t\trecord TEXT NOT NULL CHECK (json_valid(record))\n\
             \t\t) STRICT",
            tables.tasks
        ),
        format!(
            "CREATE INDEX {} ON {} (status, id)",
            indexes.tasks_by_status, tables.tasks
        ),
        format!(
            "CREATE INDEX {} ON {} (conversation_id, id)",
            indexes.tasks_by_conversation, tables.tasks
        ),
        format!(
            "CREATE INDEX {} ON {} (kind, id)",
            indexes.tasks_by_kind, tables.tasks
        ),
        format!(
            "CREATE INDEX {} ON {} (abort_requested, id)",
            indexes.tasks_by_abort_requested, tables.tasks
        ),
        format!(
            "CREATE INDEX {} ON {} (background, id)",
            indexes.tasks_by_background, tables.tasks
        ),
        format!(
            "CREATE TABLE {} (\n\
             \t\t\tid INTEGER PRIMARY KEY,\n\
             \t\t\tconversation_id INTEGER NOT NULL,\n\
             \t\t\trequest_id TEXT,\n\
             \t\t\tstatus TEXT NOT NULL CHECK (status IN ('queued', 'placed', 'done', 'unanswered')),\n\
             \t\t\trecord TEXT NOT NULL CHECK (json_valid(record))\n\
             \t\t) STRICT",
            tables.submissions
        ),
        format!(
            "CREATE INDEX {} ON {} (conversation_id, request_id)",
            indexes.submissions_by_request, tables.submissions
        ),
        format!(
            "CREATE INDEX {} ON {} (conversation_id, id)",
            indexes.submissions_by_conversation, tables.submissions
        ),
        format!(
            "CREATE INDEX {} ON {} (status, id)",
            indexes.submissions_by_status, tables.submissions
        ),
        format!(
            "CREATE TABLE {} (\n\
             \t\t\tid INTEGER PRIMARY KEY,\n\
             \t\t\tkind TEXT NOT NULL,\n\
             \t\t\tfamily INTEGER NOT NULL CHECK (family IN (0, 1)),\n\
             \t\t\tkey_value TEXT NOT NULL,\n\
             \t\t\tscope_kind TEXT NOT NULL CHECK (scope_kind IN ('session', 'conversation', 'task')),\n\
             \t\t\towner_id INTEGER NOT NULL,\n\
             \t\t\tcreated_at INTEGER NOT NULL,\n\
             \t\t\tretired_at INTEGER,\n\
             \t\t\trecord TEXT NOT NULL CHECK (json_valid(record))\n\
             \t\t) STRICT",
            tables.documents
        ),
        format!(
            "CREATE INDEX {}\n\
             \t\t\tON {} (kind, scope_kind, owner_id, family, key_value, created_at DESC, retired_at)",
            indexes.documents_by_address, tables.documents
        ),
        format!(
            "CREATE INDEX {} ON {} (scope_kind, owner_id, id)",
            indexes.documents_by_scope, tables.documents
        ),
        format!(
            "CREATE INDEX {} ON {} (scope_kind, owner_id, kind, id)",
            indexes.documents_by_scope_kind, tables.documents
        ),
        format!(
            "CREATE TABLE {} (\n\
             \t\t\tdocument_id INTEGER NOT NULL,\n\
             \t\t\tseq INTEGER NOT NULL,\n\
             \t\t\tkind TEXT NOT NULL CHECK (kind IN ('base', 'delta')),\n\
             \t\t\tversion INTEGER NOT NULL,\n\
             \t\t\tcontent TEXT NOT NULL CHECK (json_valid(content)),\n\
             \t\t\tPRIMARY KEY (document_id, seq)\n\
             \t\t) STRICT",
            tables.document_revisions
        ),
        format!(
            "CREATE INDEX {} ON {} (document_id, kind, seq DESC)",
            indexes.document_revisions_by_kind, tables.document_revisions
        ),
    ];
    // 版本 2 加入 `sleeping` 状态；SQLite 不能就地改 CHECK，因此重建表。
    // 临时表名是 `<tasks>_v2`（前缀已经包含在 tables.tasks 里），RENAME 之后该名字消失；
    // 迁移测试断言原有 waiting 行与五个索引都保留下来。
    let rebuilt_tasks = format!("{}_v2", tables.tasks);
    let sleeping_status: Vec<String> = vec![
        format!(
            "CREATE TABLE {rebuilt_tasks} (\n\
             \t\t\tid INTEGER PRIMARY KEY,\n\
             \t\t\tconversation_id INTEGER NOT NULL,\n\
             \t\t\tkind TEXT NOT NULL,\n\
             \t\t\tstatus TEXT NOT NULL CHECK (status IN ('pending', 'running', 'waiting', 'sleeping', 'completing', 'terminal')),\n\
             \t\t\tabort_requested INTEGER NOT NULL CHECK (abort_requested IN (0, 1)),\n\
             \t\t\tbackground INTEGER NOT NULL CHECK (background IN (0, 1)),\n\
             \t\t\trecord TEXT NOT NULL CHECK (json_valid(record))\n\
             \t\t) STRICT"
        ),
        // 显式列出所有业务列，不依赖旧表的物理列顺序。
        format!(
            "INSERT INTO {rebuilt_tasks} (id, conversation_id, kind, status, abort_requested, background, record)\n\
             \t\t\tSELECT id, conversation_id, kind, status, abort_requested, background, record FROM {}",
            tables.tasks
        ),
        // DROP 会连同旧表上的索引一起删除，所以接着必须重建五个同名索引。
        format!("DROP TABLE {}", tables.tasks),
        format!("ALTER TABLE {rebuilt_tasks} RENAME TO {}", tables.tasks),
        // 这五个定义与版本 1 中 `tasks` 的索引完全相同（名称、列序、顺序都一致），
        // 便于 golden 测试逐条对照。
        format!(
            "CREATE INDEX {} ON {} (status, id)",
            indexes.tasks_by_status, tables.tasks
        ),
        format!(
            "CREATE INDEX {} ON {} (conversation_id, id)",
            indexes.tasks_by_conversation, tables.tasks
        ),
        format!(
            "CREATE INDEX {} ON {} (kind, id)",
            indexes.tasks_by_kind, tables.tasks
        ),
        format!(
            "CREATE INDEX {} ON {} (abort_requested, id)",
            indexes.tasks_by_abort_requested, tables.tasks
        ),
        format!(
            "CREATE INDEX {} ON {} (background, id)",
            indexes.tasks_by_background, tables.tasks
        ),
    ];
    // 版本号必须从 1 连续；chord 的迁移器在触碰数据库前会校验这一点（见其模块文档）。
    // 已应用过某版本的库不会重跑该版本，所以改动旧 statements 只会影响新库，并让新旧库的
    // schema 文本分叉。
    vec![
        SqliteMigration {
            version: 1,
            statements: initial_schema,
        },
        SqliteMigration {
            version: 2,
            statements: sleeping_status,
        },
    ]
}

/// `SQLITE_MIGRATIONS = createSqliteMigrations(DEFAULT_SQLITE_SCHEMA)`。
///
/// 无前缀的完整迁移历史，首次使用时渲染一次。`LazyLock<Vec<_>>` 而不是 `const`：语句是
/// 运行期 `format!` 出来的 `String`。调用方以切片传入；chord 的迁移器为把数据移入返回的
/// future 会再克隆一次（每条语句的 `String` 都在其中）。
pub static SQLITE_MIGRATIONS: LazyLock<Vec<SqliteMigration>> =
    LazyLock::new(|| create_sqlite_migrations(&DEFAULT_SQLITE_SCHEMA));

/// `CURRENT_SQLITE_SCHEMA_VERSION = SQLITE_MIGRATIONS.at(-1)?.version ?? 0`。
///
/// TS 能对数组取最后一项；Rust 的 `const` 不能读取 `LazyLock`，所以这里是手写值，必须与
/// `create_sqlite_migrations` 的最后一条版本一致（测试
/// `current_version_matches_the_last_migration` 锁定）。
pub const CURRENT_SQLITE_SCHEMA_VERSION: i64 = 2;

/// 应用全部待执行迁移（TS: `applySqliteMigrations(database)`）。
///
/// 使用 [`SQLITE_MIGRATIONS`] 与默认（无前缀）的 `durable_schema` 表名；版本已是最新时，
/// 迁移器仍会执行幂等的建表 / `INSERT OR IGNORE` 并读取版本，然后跳过全部迁移语句。
/// 前缀数据库请改用 [`apply_sqlite_migrations_for_schema`]。
///
/// 错误来自 chord 的迁移器：[`SqliteError`] 携带消息；版本不连续或表名非法时在返回
/// future 之前就同步失败（不触碰数据库）。正常情况下 chord 在单个事务内执行并在失败时
/// 回滚；若回滚本身也失败，返回的是聚合错误（[`SqliteError::is_aggregate`]），此时不能
/// 假定数据库回到调用前状态（见 chord 迁移模块文档）。
pub fn apply_sqlite_migrations<'a, D: SqliteDatabase + ?Sized>(
    database: &'a D,
) -> SqliteBoxFuture<'a, Result<(), SqliteError>> {
    apply_sqlite_migrations_with(database, SQLITE_MIGRATIONS.as_slice())
}

/// 应用给定的迁移历史（TS: `applySqliteMigrations(database, migrations)`）。
///
/// `migrations` 只替换要执行的语句列表；版本号仍写进默认 schema 的 `durable_schema`。
/// `tests/sqlite_migrations.rs` 用 `&SQLITE_MIGRATIONS[..1]` 构造一个只认识版本 1 的库，
/// 再由存储打开时升级到版本 2。版本必须从 1 连续，否则 chord 的迁移器同步报错。
pub fn apply_sqlite_migrations_with<'a, D: SqliteDatabase + ?Sized>(
    database: &'a D,
    migrations: &[SqliteMigration],
) -> SqliteBoxFuture<'a, Result<(), SqliteError>> {
    apply_sqlite_migrations_for_schema_with(database, &DEFAULT_SQLITE_SCHEMA, migrations)
}

/// 为某个 schema 应用它自己的迁移历史（TS: `applySqliteMigrationsForSchema(database, schema)`）。
///
/// 先按 `schema` 渲染一份迁移（[`create_sqlite_migrations`]），再把版本写进
/// `schema.tables.durable_schema`。带前缀的数据库必须走这个入口；
/// `tests/workers_storage.rs` 用 `legacy_` 前缀验证版本 1 → 2 的升级。
pub fn apply_sqlite_migrations_for_schema<'a, D: SqliteDatabase + ?Sized>(
    database: &'a D,
    schema: &SqliteSchema,
) -> SqliteBoxFuture<'a, Result<(), SqliteError>> {
    let migrations = create_sqlite_migrations(schema);
    apply_sqlite_migrations_for_schema_with(database, schema, &migrations)
}

/// 为某个 schema 应用给定的迁移历史（TS: `applySqliteMigrationsForSchema(database, schema, migrations)`）。
///
/// 直接转发到 chord 的迁移器：`migrations` 按给定顺序执行，版本表名取
/// `schema.tables.durable_schema`。调用方负责保证 `migrations` 与 `schema` 配套（语句里的
/// 标识符已按同一前缀渲染）；本函数不检查这一点。
pub fn apply_sqlite_migrations_for_schema_with<'a, D: SqliteDatabase + ?Sized>(
    database: &'a D,
    schema: &SqliteSchema,
    migrations: &[SqliteMigration],
) -> SqliteBoxFuture<'a, Result<(), SqliteError>> {
    kiana_chord::sqlite::apply_sqlite_migrations(
        database,
        migrations,
        &schema.tables.durable_schema,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_version_matches_the_last_migration() {
        assert_eq!(
            SQLITE_MIGRATIONS.last().map(|migration| migration.version),
            Some(CURRENT_SQLITE_SCHEMA_VERSION)
        );
        assert_eq!(CURRENT_SQLITE_SCHEMA_VERSION, 2);
    }

    #[test]
    fn versions_are_contiguous_and_statement_counts_are_stable() {
        assert_eq!(
            SQLITE_MIGRATIONS
                .iter()
                .map(|m| m.version)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(SQLITE_MIGRATIONS[0].statements.len(), 25);
        assert_eq!(SQLITE_MIGRATIONS[1].statements.len(), 9);
    }

    #[test]
    fn prefix_changes_every_rendered_identifier() {
        let schema =
            crate::storage::sqlite::schema::create_sqlite_schema(Some("kiana_")).expect("prefix");
        let migrations = create_sqlite_migrations(&schema);
        assert!(migrations[0].statements[0].starts_with("CREATE TABLE kiana_durable_metadata ("));
        assert!(migrations[0].statements.iter().any(|statement| {
            statement == "CREATE INDEX kiana_entry_heads_by_conversation ON kiana_entries (conversation_id, id DESC) WHERE head IS NOT NULL"
        }));
        assert!(migrations[1].statements[0].starts_with("CREATE TABLE kiana_tasks_v2 ("));
        assert_eq!(
            migrations[1].statements[3],
            "ALTER TABLE kiana_tasks_v2 RENAME TO kiana_tasks"
        );
    }
}
