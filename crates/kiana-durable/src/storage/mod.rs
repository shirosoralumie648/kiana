//! storage 目录。
//!
//! 与 TS 的 `packages/durable/src/storage/` 一一对应：`scan` 是内建后端共用的游标逻辑，
//! `memory` 是参考语义实现，`jsonl` / `sqlite` 是持久化后端，`indexeddb` 是不移植的浏览器
//! 后端的占位（ARCHITECTURE.md §8 第 2 条）。
//!
//! # 这个目录负责什么
//!
//! [`crate::types::Storage`] trait 定义了存储后端的公共契约（提交、查询、扫描、游标）。
//! 本目录的每个子模块各提供一种实现或一份共享逻辑；示例与一致性测试会直接构造具体后端
//! （例如 [`MemoryStorage`]），改动这些公开类型会同时影响它们。
//!
//! # 子模块的分工
//!
//! - `scan`：纯函数，不持有状态。会话/条目/任务/提交这些“按 ID 顺序扫描 + 续页游标”的
//!   后端都通过 [`scan::scan_start`] 解释游标、通过 [`scan::next_cursor`] 生成下一页游标；
//!   语义只有这一份实现。文档扫描不调用 [`scan::scan_start`]（`DocumentQuery` 没有顺序
//!   字段，`after` 由 `cursor_id` 校验，游标里的 `order` 被忽略），但它仍经 `page` 使用
//!   [`scan::next_cursor`] 生成续页游标。
//! - `memory`：[`MemoryStorage`] 是参考语义实现，也是其它后端语义的基准。它持有全部状态并
//!   提供脱离式（写入与读取都克隆）的记录快照。
//! - `jsonl`：可移植的持久化后端；运行期把读取、`mint_id` 与 `close` 转发给内部的
//!   [`MemoryStorage`] 镜像，写入路径负责落盘与恢复（见 `storage/jsonl/storage.rs` 的模块文档）。
//! - `sqlite`：SQLite 后端。本地 `rusqlite` 驱动在 `sqlite::node`，该模块在
//!   `storage/sqlite/mod.rs` 里由 feature `sqlite` 门控；同一文件中的 `storage` 等模块
//!   没有 `cfg`，因此默认 feature 下也会编译。
//! - `indexeddb`：浏览器后端占位，构造即返回明确的 not-supported 错误；不要用其它后端模拟它。
//!
//! # 共享的游标语义
//!
//! 扫描接口接收 `cursor: Option<&Cursor>`（一个后端拥有的 JSON 对象），调用方只应原样回传。
//! 内建后端统一调用 `scan_start(requested, cursor, fallback)` 得到扫描起点：游标里的 `order`
//! 优先于本次查询的 `order`，两者冲突时报 `StorageRequestError`；具体规则与逐字错误消息见
//! [`scan::scan_start`]。
//!
//! # 注意事项
//!
//! - 本文件只在目录级别 `pub use` 出 [`MemoryStorage`] / [`PreparedMemoryCommit`]；其余类型
//!   走子模块路径（`storage::sqlite::SqliteStorage` 等）。
//! - 后端差异（原子性、重新打开后的可见性、锁与 I/O 边界）由各子模块文档描述，本文件不重复。
//! - `indexeddb` 与 `sqlite::cloudflare` 的占位是**产品边界**，不是待办；理由见
//!   ARCHITECTURE.md §8 第 2、3 条与 `indexeddb.rs` 的模块文档。

// --- 子模块 ---
pub mod indexeddb;
pub mod jsonl;
pub mod memory;
pub mod scan;
pub mod sqlite;

// `MemoryStorage` 是 `Storage` 的参考实现，也是 `examples/01_documents.rs` 与所有 backend
// conformance 用例的构造点；与 TS 的 `./storage/memory` 子路径导出同义。
pub use memory::{MemoryStorage, PreparedMemoryCommit};
