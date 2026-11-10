//! `core/compaction/index.ts` 的再导出面。
//!
//! TS 源：`packages/coding-agent/src/core/compaction/index.ts`
//!
//! TS 里 `branch-summarization.ts` 通过 `export type { FileOperations }` 重复导出
//! `utils.ts` 的类型（同一声明，合法）。Rust 侧只在 [`crate::utils`] 定义一次
//! `FileOperations`，避免 glob 再导出出现名字歧义。
//!
//! # 这一层为什么存在
//!
//! TS 的 `index.ts` 是该目录的公共门面（`export *` 三个模块），对外入口（agent-session、
//! RPC 与 SDK 入口）从它导入；部分内部模块仍直接引用兄弟文件。
//! Rust 侧用三条 glob `pub use` 复刻同一表面；crate 根（`lib.rs`）又执行
//! `pub use crate::index::*`，因此本文件新增的公开名字会立刻成为 `kiana_compaction` 的
//! 公共 API。
//!
//! # 修改约束
//!
//! - 三个子模块共享同一命名空间：同名但不同的项在被引用时会产生歧义错误，所以跨模块
//!   共享的类型只在唯一模块定义（当前是 [`crate::utils`] 的 `FileOperations` /
//!   `FileValue`）；
//! - 名字与 TS 的对应关系由 `kiana-coding-agent/src/sdk_exports.rs` 的 camelCase 别名维护
//!   （`generateBranchSummary`、`collectEntriesForBranchSummary` 等）；改名会让别名失效；
//! - 本文件没有逻辑，不要在这里添加函数或类型。

pub use crate::branch_summarization::*;
pub use crate::compaction::*;
pub use crate::utils::*;
