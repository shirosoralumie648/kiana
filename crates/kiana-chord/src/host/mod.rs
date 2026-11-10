//! host 目录。
//!
//! TODO: 按 TS 源逐文件移植；本 crate 内不得出现第二份实现。
//!
//! ## 在 crate 中的位置
//!
//! `lib.rs` 以 `pub mod host;` 公开本模块；`index.rs` 的再导出表把“host 的 3 个值 + 类型”
//! 指向 [`crate::host`]。但在 host 单元落地前本模块没有可再导出的条目，所以 `index.rs`
//! 目前不引用这里的任何名字（见该文件“现阶段”一节）。
//!
//! ## 当前状态
//!
//! 本目录的五个文件都只有文件级文档（`mod.rs` 另有四条 `pub mod` 声明）：还没有
//! `HostImpl`、`Host` trait 或任何 host 行为，也没有调用方与测试。这些文件先存在，是为了
//! 固定模块路径，让单元可以逐个填充而不改动父模块声明；在实现完成前，`crate::host` 及其
//! 子模块不提供任何可调用项。
//!
//! ## 计划布局
//!
//! `spec/chord-core.md` §8.1 的**建议布局**（与 `ARCHITECTURE.md` 第 923-927 行的文件映射
//! 一致）：
//!
//! ```text
//! host/mod.rs   HostImpl + 公开 Host trait
//! roots.rs      root/joinRoot/endRoot/foreignRootEnded/retire/allowRetire/watchRetirement
//! tables.rs     buildTables/registriesChanged/installTables/sameTable
//! routes.rs     #routes/#routeFor/#resolve/#childNamed/#hold/#acquireRemote/relay
//! facets.rs     FacetApi/defineFacet/install/load/resolve/facetRunChanged/serveInto/startHostObject
//! ```
//!
//! 注意：`ARCHITECTURE.md` 把四个子模块分别映射到 TS 的 `host/facets.ts`、`host/roots.ts`、
//! `host/routes.ts`、`host/tables.ts`，但当前检出的 TS 源（`pi/packages/chord/src/`）只有
//! 单文件 `host.ts`，没有 `host/` 目录；两边的对应关系待核实。
//!
//! ## 修改约束
//!
//! 按 §8.1，`HostCore`（`object` 需要的宿主视图）保持为内部 trait，不进入公开面；`Host` 是
//! 用户可见的公开 trait，放在本文件。四个子模块共享同一份宿主状态，不得各自复制。

// --- 子模块 ---
pub mod facets;
pub mod roots;
pub mod routes;
pub mod tables;
