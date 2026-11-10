//! `storage/indexeddb.ts`（1,211 行）——**不移植**（决策：ARCHITECTURE.md §8 第 2 条，GAPS G1.x）。
//!
//! IndexedDB 是浏览器 API，Rust 二进制里没有对应实现，也没有 `wasm32-unknown-unknown`
//! 的 M1 目标。保留这个模块是为了：
//!
//! 1. **文件所有权不丢**：`ARCHITECTURE.md` §5 的“一 TS 文件一 `.rs`”映射保持完整；
//! 2. **类型面可查**：`IndexedDbStorageOptions` / `IndexedDbStorage` 的形状仍在文档里；
//! 3. **下游可依赖**：`kiana-durable::storage::sqlite`/`jsonl`/`memory` 之外的第四种后端在
//!    `storage-conformance` 里按“不适用”处理，而不是编译失败。
//!
//! 本模块没有 feature 门控：无论 Cargo.toml 里的 `wasm` feature 是否启用，[`IndexedDbStorage::open`]
//! 都返回 `Err(StorageBackendUnsupported::IndexedDb { .. })`，错误消息逐字：
//! `IndexedDB storage is not supported in the Rust build`。`wasm` feature 虽然已经声明，但本
//! crate 的源码里没有任何 `cfg(feature = "wasm")`，它当前不改变本模块的编译结果。
//!
//! [`IndexedDbStorage`] 的字段是私有的，唯一接收选项的构造入口就是恒失败的 `open`；derive 的
//! `Default` 仍能造出 options 全为 `None` 的句柄（可以用 `options()` 读回），但本仓库没有调用点。
//!
//! 实现者**不要**在这里加“用 sqlite 模拟 IndexedDB”的兼容层：TS 的 IndexedDB 后端有
//! 事务/游标语义（`DATABASE_VERSION = 1`、文档修订的 base/delta 回放、`incarnation` 生命周期），
//! 模拟出来的行为会与 `storage-conformance` 的 26 个用例不一致。

/// `IndexedDbStorageOptions`。
///
/// 与 TS 的形状差异：TS 的 `IndexedDbStorageOptions` 只有 `indexedDB?: IDBFactory` 一个
/// 工厂字段，数据库名是 `open(name, options)` 的独立参数；Rust 版本把名字与版本收进本结构。
/// 占位实现不会打开数据库：只有 `database_name` 会被带进错误值，`database_version` 被忽略。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IndexedDbStorageOptions {
    /// 数据库名。
    pub database_name: Option<String>,
    /// `DATABASE_VERSION`（TS 固定 1，升级走 `onupgradeneeded`）。
    pub database_version: Option<u32>,
}

/// `IndexedDbStorage`：浏览器后端的占位句柄。
///
/// [`IndexedDbStorage::open`] 恒返回 `Err(StorageBackendUnsupported)`，文案见模块文档。
/// derive 的 `Default` 能构造一个 options 全为 `None` 的句柄，但没有可用的存储功能；
/// 本仓库没有调用点。
#[derive(Debug, Clone, Default)]
pub struct IndexedDbStorage {
    options: IndexedDbStorageOptions,
}

impl IndexedDbStorage {
    /// 构造（永远失败）。
    ///
    /// 不读取浏览器全局对象，也不访问任何文件或数据库：只是把 `options.database_name`
    /// 搬进 [`StorageBackendUnsupported::IndexedDb`] 后返回 `Err`；`options.database_version`
    /// 不进入错误值。
    pub fn open(options: IndexedDbStorageOptions) -> Result<Self, StorageBackendUnsupported> {
        Err(StorageBackendUnsupported::indexeddb(options))
    }

    /// 读回结构体保存的选项。
    ///
    /// 由于 [`IndexedDbStorage::open`] 恒失败，正常运行中只有 `Default` 构造的句柄能调用本方法，
    /// 此时返回全为 `None` 的选项；本仓库没有调用点。
    pub fn options(&self) -> &IndexedDbStorageOptions {
        &self.options
    }
}

/// 浏览器专用后端的统一“不支持”错误。
///
/// `Display` 逐字：`IndexedDB storage is not supported in the Rust build`（IndexedDB）或
/// `Cloudflare Durable Object storage is not supported in the Rust build`（Cloudflare）。
///
/// 两个变体分别对应 `indexeddb.ts` 与 `storage/sqlite/cloudflare.ts`；后者通过
/// `storage::sqlite::cloudflare` 再导出本类型。`PartialEq` 让测试可以按变体与字段断言。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StorageBackendUnsupported {
    /// `indexeddb.ts`
    #[error("IndexedDB storage is not supported in the Rust build")]
    IndexedDb {
        /// 调用方给的名字（仅用于诊断日志，不进入 `Display`）。
        database_name: Option<String>,
    },
    /// `storage/sqlite/cloudflare.ts`、`images/cloudflare.ts`
    #[error("Cloudflare Durable Object storage is not supported in the Rust build")]
    Cloudflare,
}

impl StorageBackendUnsupported {
    /// 由 [`IndexedDbStorage::open`] 使用：把选项里的 `database_name` 搬进错误值。
    ///
    /// `database_version` 被丢弃——错误值只保留诊断用的库名，`Display` 也不包含它。
    pub fn indexeddb(options: IndexedDbStorageOptions) -> Self {
        StorageBackendUnsupported::IndexedDb {
            database_name: options.database_name,
        }
    }
}
