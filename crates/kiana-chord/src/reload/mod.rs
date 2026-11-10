//! reload 目录：两版本热更新（hot reload）的 prepare / gate / commit 三阶段。
//!
//! TODO: 按 TS 源逐文件移植；本 crate 内不得出现第二份实现。
//!
//! ## 当前状态
//!
//! 本目录是**空占位**：三个子模块只有文件级文档，没有 `Reload` 类型、`ReloadOptions`/
//! `ReloadReport` 或任何函数。`lib.rs` 无条件声明 `pub mod reload;`；`index.rs` 的再导出表
//! 把 TS 的 `ReloadOptions`/`ReloadReport` 指向 `crate::reload`，但在本单元实现前
//! `index.rs` 不引用这里的任何名字（见该文件“现阶段”一节）。crate 内没有其它代码引用
//! `reload::*`（`index.rs` 只在文档表格里链接本模块），`tests/` 下也没有针对 reload 的用例。
//!
//! ## 为什么是一个目录
//!
//! TS 侧的主要实现是单文件 `packages/chord/src/reload.ts`（723 行）：`class Reload` 承载
//! prepare/gate/commit 流程，入口是模块级的 `runReload`/`beginReload`/`endReload`。
//! Rust 按执行阶段拆成三个子模块，布局取自 `spec/chord-core.md` §8.1 的建议布局：
//!
//! ```text
//! reload/mod.rs      Reload 状态 + runReload/beginReload/endReload/reloadError/summary
//! reload/prepare.rs  waitForRetirement/load/checkProviders/prepareAll/prepareObject/checkRefs/entries
//! reload/gate.rs     gate/migrate/dryRun/candidateValue
//! reload/commit.rs   commit/#publish/install/rollback
//! ```
//!
//! `index.rs` 的再导出表还要求本目录提供 `ReloadOptions`/`ReloadReport`（TS 定义在
//! `reload.ts` 顶部；§8.1 未单列这两个类型，放在哪个子文件待定）。
//!
//! 拆分只改变文件组织，不改变执行顺序：TS 的 `runReload` 先 `beginReload`，再依次调用
//! `prepare()` 与 `commit()`，并在 `finally` 中 `endReload`（`spec/chord-rest.md` §2.20）。
//! TS 侧由 `HostImpl.reload` 分派：存在下游 host 连接或配置了 `remote`/`remoteRemove` 时走
//! `runClusterReload`（集群协调，见 `cluster.rs` 单元），否则走 `runReload`；非 client
//! 且有 upstream 的子 host 直接拒绝（`unavailable`，reason `not_coordinator`）。
//!
//! ## 三阶段的职责边界
//!
//! - prepare：所有可能失败的异步工作都在这里（解析并运行新 facet、打开资源、等待激活中的
//!   对象、校验引用、迁移后（gate 内 migrate() 末尾）的 dry run（对迁移值和派生值按提交后
//!   适用的 schema 校验）、订阅新 listener 并缓冲事件）。失败时先 rollback，
//!   再抛 reloadError（保留底层错误码）。
//! - gate：只处理带准入门槛的对象（状态版本升高，或本次 run 产生任何独占资源/依赖 deferred
//!   资源的新资源；reason 文案仍为 `exclusive resource replaced`）。先等旧版本的 root
//!   释放 leases，超过 `gateDeadline` 就中止；随后替换独占资源并迁移 cell。
//! - commit：不再执行可能失败的检查，只做同步发布。发布包在
//!   [`crate::publish::hold_publications`] 中；安装完成后先执行协调者传入的回调，再启动
//!   新版本的 task 与 host 对象。
//!
//! ## 重要约束
//!
//! - 顺序即语义：`spec/chord-rest.md` §2.20 明确以“顺序即语义”描述 `Reload` 流程，
//!   移植时不得合并或重排步骤。
//! - 同一 host 同时只能有一个 reload：`beginReload` 在 `host.reloading` 为真时抛
//!   `unavailable`（reason 为 `reload_running`，`spec/chord-rest.md` §2.20）。
//! - 本 crate 只允许一份实现（见上面的 TODO）。
//!
//! ## 阅读顺序
//!
//! `prepare.rs` → `gate.rs` → `commit.rs`；入口函数与 `Reload` 状态属于本文件，尚未实现。
//!
//! 注意：`ARCHITECTURE.md` 把三个子文件映射到 TS 的 `reload/{prepare,gate,commit}.ts`，
//! 但当前检出的 TS 源（`pi/packages/chord/src/`）只有单文件 `reload.ts`，没有这些文件；
//! 以 `spec/chord-core.md` §8.1 的拆分为准。

// --- 子模块 ---
pub mod commit;
pub mod gate;
pub mod prepare;
