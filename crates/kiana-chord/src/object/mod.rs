//! object 目录。
//!
//! TODO: 按 TS 源逐文件移植；本 crate 内不得出现第二份实现。
//!
//! # 这个目录解决什么问题
//! 这是 Chord 的对象运行时：把「契约 + 工厂函数」变成一个可被 `get` 的活对象，并管理它的
//! 资源、状态单元、引用、监听器、任务、方法调用与释放。TS 侧这些逻辑集中在
//! `packages/chord/src/object.ts`（1932 行，`spec/chord-core.md` §2.3 逐节分析）。
//! Rust 侧按 `spec/chord-core.md` §8.1 的建议拆成五个子模块：
//!
//! - `resource`：资源的打开/关闭、跨 reload 复用与 `ResourceFacade`；
//! - `refs`：本地引用 `RefImpl` 与成员级 `MemberReadable`，以及状态通知；
//! - `declarations`：一次 factory run 的声明如何变成运行期副作用（监听器、任务）；
//! - `object_impl`：`ObjectImpl` 状态机（激活事务、`live`、释放）与资源/引用清理；
//! - `invoke`：方法调用的准入、租约、参数/结果校验、调用计数与 `dispose`/`unwind`。
//!
//! `Service`/`Factory`/`Self`/`Decls`/`Run`/`Installed`/`HostCore`/`CtxImpl` 以及
//! `linked_signal`/`await_ready`/`race_signal` 按 spec §8.1 计划放在本文件，作为五个
//! 子模块共享的类型与工具。
//!
//! # 与其他模块的关系
//! TS 侧的 `object.ts` 被 `host.ts`、`reload.ts`、`remote.ts`、`connection.ts` 直接导入，
//! `index.ts` 再导出 `Self`/`ServeOptions` 等公开类型。Rust 侧按 `ARCHITECTURE.md` 的模块
//! 映射，对应 `crate::host`、`crate::reload`、`crate::remote`、`crate::connection`。
//! 对外暴露的引用/上下文 trait（`RefBase`、`Ref`、`Ctx`、`Readable`）定义在 `crate::types`；
//! 本目录负责实现，不重复定义协议。
//!
//! # 重要约束
//! - Rust 没有 JS `Proxy`：引用是显式 handle，成员通过 `state_member(name)` /
//!   `call(name, args, ctx)` 访问（spec §8.1）。不要按属性名做动态分发。
//! - 引用不能被当成 thenable（带 `then` 方法、会被 `await` 当作 Promise 的对象）：成员访问
//!   中的 `then` 必须永远不可用（返回 `None`），以保持「引用不是 thenable」（spec §8.1）。
//! - TS 的 `Map` 插入序参与语义（声明、成员、任务都按键序处理）；Rust 侧用 `IndexMap`
//!   （spec §8.1），不要换成无序 `HashMap`。
//!
//! # 现状
//! 五个子模块当前都只有 `//!` 占位说明，没有导出任何条目；编译通过不代表对象运行时可用
//! （`docs/UNDERSTANDING_GUIDE.md` 也提醒不要把占位模块当作可用 API）。语义以
//! `spec/chord-core.md` §2.3 与 TS 源为准。
//!
//! # 建议的阅读顺序
//! `resource`（最小状态机）→ `refs`（对象对外的句柄）→ `declarations`（声明变副作用）
//! → `object_impl`（对象生命周期）→ `invoke`（调用路径）。

// --- 子模块 ---
pub mod declarations;
pub mod invoke;
pub mod object_impl;
pub mod refs;
pub mod resource;
