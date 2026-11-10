//! `storage/sqlite/schema.ts`：迁移与运行期查询共用的标识符表。
//!
//! TS 用两张 `as const` 对象（`TABLE_NAMES` / `INDEX_NAMES`）加 `createSqliteSchema(prefix?)`
//! 生成最终的 SQLite 标识符。值本身是**不可变历史**：永不修改/删除既有值；重命名只新增条目，
//! 让历史迁移继续渲染它当年的对象名。Rust 用两个字段结构体承载同一张表（字段名是 camelCase
//! 的 snake_case 形式），顺序与 TS 对象字面量一致。
//!
//! # 为什么值不可变
//! 标识符会被 `format!` 直接拼进 `CREATE TABLE`/`SELECT` 等 SQL（标识符不能用绑定参数）。
//! 既有数据库里的对象名是持久化契约：改掉一个值会让新代码渲染的名字与老库中的实际对象
//! 不一致，而版本已是最新时迁移不会重跑、无法修复；因此既有字段只增不改，新对象放进
//! **新的**迁移版本使用。
//!
//! # 前缀如何生效
//! `create_sqlite_schema` 先校验前缀，再用 `SqliteSchema::with_prefix` 把同一个字符串拼到
//! 每个标识符前面；`DEFAULT_SQLITE_SCHEMA` 等价于空前缀。前缀不迁移既有数据：同一数据库
//! 文件里带前缀与不带前缀的对象是两套独立对象
//! （`tests/sqlite_storage.rs` 的 `namespaces_tables_and_indexes_with_a_prefix_across_reopen`）。
//!
//! # 注意事项
//! - 这些名字同时进入迁移（`migrations`）与运行期 SQL（`storage`）；改一个字段必须确认
//!   两处渲染一致，否则查询会指向另一套表。
//! - `Some("")` 会被校验拒绝；只有 `None` 才会渲染成无前缀（见 [`create_sqlite_schema`]）。

use std::sync::LazyLock;

pub use kiana_chord::sqlite::validate_sqlite_schema_prefix;

/// `createSqliteSchema` 的前缀校验失败（TS 的 `validateSqliteSchemaPrefix` 抛出的 `Error`）。
///
/// 只带消息、没有错误码：chord 的校验器只返回字符串，调用方要区分“非法字符”与“保留
/// 前缀”只能检查消息内容（`Invalid ...` / `... is reserved`）；
/// `tests/sqlite_cloudflare.rs` 用 `contains("Invalid")` / `contains("reserved")` 判断。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct SqliteSchemaError {
    /// 完整消息（逐字来自 chord 的校验器）。
    pub message: String,
}

/// `TABLE_NAMES`（不带前缀）。
// 每次调用新建一份 String：`with_prefix` 需要拥有值，这些无前缀名字只在构造 schema 时使用。
fn table_names() -> SqliteTables {
    fn owned(value: &str) -> String {
        value.to_owned()
    }
    SqliteTables {
        durable_schema: owned("durable_schema"),
        durable_metadata: owned("durable_metadata"),
        record_ids: owned("record_ids"),
        conversations: owned("conversations"),
        entries: owned("entries"),
        tasks: owned("tasks"),
        submissions: owned("submissions"),
        documents: owned("documents"),
        document_revisions: owned("document_revisions"),
    }
}

/// `INDEX_NAMES`（不带前缀）。
fn index_names() -> SqliteIndexes {
    fn owned(value: &str) -> String {
        value.to_owned()
    }
    SqliteIndexes {
        conversations_by_owner_conversation: owned("conversations_by_owner_conversation"),
        conversations_by_owner_task: owned("conversations_by_owner_task"),
        entries_by_conversation: owned("entries_by_conversation"),
        entry_heads_by_conversation: owned("entry_heads_by_conversation"),
        tasks_by_status: owned("tasks_by_status"),
        tasks_by_conversation: owned("tasks_by_conversation"),
        tasks_by_kind: owned("tasks_by_kind"),
        tasks_by_abort_requested: owned("tasks_by_abort_requested"),
        tasks_by_background: owned("tasks_by_background"),
        submissions_by_request: owned("submissions_by_request"),
        submissions_by_conversation: owned("submissions_by_conversation"),
        submissions_by_status: owned("submissions_by_status"),
        documents_by_address: owned("documents_by_address"),
        documents_by_scope: owned("documents_by_scope"),
        documents_by_scope_kind: owned("documents_by_scope_kind"),
        document_revisions_by_kind: owned("document_revisions_by_kind"),
    }
}

/// `SqliteSchema.tables`（TS: `SqliteIdentifiers<typeof TABLE_NAMES>`）。
///
/// 每个字段保存**最终**标识符（已带前缀）。字段顺序不是 SQL 依赖，但保持与 `TABLE_NAMES`
/// 一致便于对照；不要用位置索引访问。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqliteTables {
    /// `durable_schema`：迁移版本表。
    ///
    /// 由 chord 的迁移器创建并维护，单行 `(singleton = 1, version)`。
    pub durable_schema: String,
    /// `durable_metadata`：`next_id`/`next_seq` 单行元数据。
    ///
    /// 版本 1 迁移插入初值 `(1, '2', 1)`；`SqliteStorage::open` 读取它重建 `nextId` 缓存。
    pub durable_metadata: String,
    /// `record_ids`：全局 ID → 记录类型。
    pub record_ids: String,
    /// `conversations`。
    pub conversations: String,
    /// `entries`。
    pub entries: String,
    /// `tasks`。
    pub tasks: String,
    /// `submissions`。
    pub submissions: String,
    /// `documents`。
    pub documents: String,
    /// `document_revisions`。
    pub document_revisions: String,
}

/// `SqliteSchema.indexes`（TS: `SqliteIdentifiers<typeof INDEX_NAMES>`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqliteIndexes {
    /// 按创建者会话查会话（`owner_conversation_id, id`）。
    pub conversations_by_owner_conversation: String,
    /// 按创建者任务查会话（`owner_task_id, id`）。
    pub conversations_by_owner_task: String,
    /// 会话的条目序列（`conversation_id, id DESC`，支持倒序扫描）。
    pub entries_by_conversation: String,
    /// 会话的 head 条目；`WHERE head IS NOT NULL` 的部分索引，只收录 head 行。
    pub entry_heads_by_conversation: String,
    /// 按状态查任务。
    pub tasks_by_status: String,
    /// 按会话查任务。
    pub tasks_by_conversation: String,
    /// 按定义名查任务。
    pub tasks_by_kind: String,
    /// 按 abort 标记查任务。
    pub tasks_by_abort_requested: String,
    /// 按 background 标志查任务。
    pub tasks_by_background: String,
    /// 按会话 + 请求键查提交。
    pub submissions_by_request: String,
    /// 按会话查提交。
    pub submissions_by_conversation: String,
    /// 按状态查提交。
    pub submissions_by_status: String,
    /// 精确文档地址查询：`(kind, scope_kind, owner_id, family, key_value)` 等值列在前，
    /// 随后是 `created_at DESC` 与 `retired_at`，对应 `find_document` 的过滤与取最新一行的排序。
    pub documents_by_address: String,
    /// 按作用域查文档。
    pub documents_by_scope: String,
    /// 按作用域 + kind 查文档。
    pub documents_by_scope_kind: String,
    /// 文档修订的 kind 尾部（`document_id, kind, seq DESC`），支持取读取点之前最后一条 base。
    pub document_revisions_by_kind: String,
}

/// 迁移与运行期查询使用的全部标识符（TS: `SqliteSchema`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqliteSchema {
    /// 表名。
    pub tables: SqliteTables,
    /// 索引名。
    pub indexes: SqliteIndexes,
}

impl SqliteSchema {
    /// 在全部标识符前拼接 `prefix`（TS: `prefixIdentifiers`）。
    // `prefix` 必须已经过校验；本函数只做字符串拼接，不转义、不检查字符集。
    // `DEFAULT_SQLITE_SCHEMA` 直接用空串调用这里，因此它不会触发一次多余的校验。
    fn with_prefix(prefix: &str) -> Self {
        fn prefixed(name: &str, prefix: &str) -> String {
            let mut value = String::with_capacity(prefix.len() + name.len());
            value.push_str(prefix);
            value.push_str(name);
            value
        }
        let tables = table_names();
        let indexes = index_names();
        SqliteSchema {
            tables: SqliteTables {
                durable_schema: prefixed(&tables.durable_schema, prefix),
                durable_metadata: prefixed(&tables.durable_metadata, prefix),
                record_ids: prefixed(&tables.record_ids, prefix),
                conversations: prefixed(&tables.conversations, prefix),
                entries: prefixed(&tables.entries, prefix),
                tasks: prefixed(&tables.tasks, prefix),
                submissions: prefixed(&tables.submissions, prefix),
                documents: prefixed(&tables.documents, prefix),
                document_revisions: prefixed(&tables.document_revisions, prefix),
            },
            indexes: SqliteIndexes {
                conversations_by_owner_conversation: prefixed(
                    &indexes.conversations_by_owner_conversation,
                    prefix,
                ),
                conversations_by_owner_task: prefixed(&indexes.conversations_by_owner_task, prefix),
                entries_by_conversation: prefixed(&indexes.entries_by_conversation, prefix),
                entry_heads_by_conversation: prefixed(&indexes.entry_heads_by_conversation, prefix),
                tasks_by_status: prefixed(&indexes.tasks_by_status, prefix),
                tasks_by_conversation: prefixed(&indexes.tasks_by_conversation, prefix),
                tasks_by_kind: prefixed(&indexes.tasks_by_kind, prefix),
                tasks_by_abort_requested: prefixed(&indexes.tasks_by_abort_requested, prefix),
                tasks_by_background: prefixed(&indexes.tasks_by_background, prefix),
                submissions_by_request: prefixed(&indexes.submissions_by_request, prefix),
                submissions_by_conversation: prefixed(&indexes.submissions_by_conversation, prefix),
                submissions_by_status: prefixed(&indexes.submissions_by_status, prefix),
                documents_by_address: prefixed(&indexes.documents_by_address, prefix),
                documents_by_scope: prefixed(&indexes.documents_by_scope, prefix),
                documents_by_scope_kind: prefixed(&indexes.documents_by_scope_kind, prefix),
                document_revisions_by_kind: prefixed(&indexes.document_revisions_by_kind, prefix),
            },
        }
    }
}

/// `createSqliteSchema(prefix?)`：构造每个迁移与存储查询使用的标识符。
///
/// `None` 直接渲染无前缀标识符；`Some(prefix)` 必须先通过
/// [`validate_sqlite_schema_prefix`]。空串不在合法字符集内，所以 `Some("")` 返回 `Err`
/// 而不是“无前缀”（TS 的 `prefix !== undefined` 分支同样会校验；
/// `tests/sqlite_cloudflare.rs` 覆盖空串，`tests/sqlite_storage.rs` 覆盖非法前缀不会
/// 创建数据库文件）。
///
/// 返回的 schema 只做字符串渲染，不访问数据库；同一输入总是得到相同结果。
pub fn create_sqlite_schema(prefix: Option<&str>) -> Result<SqliteSchema, SqliteSchemaError> {
    // `prefix !== undefined` 才校验：显式 `""` 会报 invalid，`undefined` 跳过。
    if let Some(prefix) = prefix {
        validate_sqlite_schema_prefix(prefix).map_err(|message| SqliteSchemaError { message })?;
    }
    Ok(SqliteSchema::with_prefix(prefix.unwrap_or("")))
}

/// `DEFAULT_SQLITE_SCHEMA = createSqliteSchema()`。
///
/// 无前缀标识符。`apply_sqlite_migrations` / `apply_sqlite_migrations_with` 使用它；
/// `SqliteStorage` 打开时用 `create_sqlite_schema(options.prefix.as_deref())` 渲染
/// （见 `storage` 模块的 `SqliteStorage::initialize`），只有 `prefix == None` 时才得到与
/// 这个 static **值相等的新实例**；带前缀时渲染的是另一套标识符。用 `LazyLock` 是因为
/// 字段是运行期分配的 `String`，无法写成 `const`；首次使用时构造一次。
pub static DEFAULT_SQLITE_SCHEMA: LazyLock<SqliteSchema> =
    LazyLock::new(|| SqliteSchema::with_prefix(""));

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_schema_uses_the_unprefixed_names() {
        assert_eq!(
            DEFAULT_SQLITE_SCHEMA.tables.durable_schema,
            "durable_schema"
        );
        assert_eq!(DEFAULT_SQLITE_SCHEMA.tables.tasks, "tasks");
        assert_eq!(
            DEFAULT_SQLITE_SCHEMA.indexes.tasks_by_status,
            "tasks_by_status"
        );
    }

    #[test]
    fn prefix_is_concatenated_to_every_identifier() {
        let schema = create_sqlite_schema(Some("kiana_")).expect("valid prefix");
        assert_eq!(schema.tables.durable_schema, "kiana_durable_schema");
        assert_eq!(schema.tables.document_revisions, "kiana_document_revisions");
        assert_eq!(
            schema.indexes.tasks_by_background,
            "kiana_tasks_by_background"
        );
    }

    #[test]
    fn rejects_invalid_and_reserved_prefixes_with_the_chord_messages() {
        let error = create_sqlite_schema(Some("bad-prefix")).expect_err("invalid prefix");
        assert_eq!(
            error.message,
            format!(
                "Invalid SQLite schema prefix {}",
                kiana_json::js_string::js_quote("bad-prefix")
            )
        );
        let error = create_sqlite_schema(Some("sqlite_kiana_")).expect_err("reserved prefix");
        assert_eq!(
            error.message,
            format!(
                "SQLite schema prefix {} is reserved",
                kiana_json::js_string::js_quote("sqlite_kiana_")
            )
        );
    }

    #[test]
    fn an_empty_prefix_is_rejected_like_the_reference() {
        // TS: `createSqliteSchema("")` 走 `prefix !== undefined` 分支，空串不是合法标识符。
        assert!(create_sqlite_schema(Some("")).is_err());
    }
}
