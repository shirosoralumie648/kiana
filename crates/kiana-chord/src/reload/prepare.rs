//! reload/prepare.rs：热更新的准备阶段（TS `Reload.prepare()` 及其辅助方法）。
//!
//! TODO: 按 TS 源逐文件移植；本 crate 内不得出现第二份实现。
//!
//! ## 当前状态
//!
//! 空占位：本文件没有类型、函数或导入，父模块 [`crate::reload`] 下也没有调用方。下面记录
//! 的是 `spec/chord-core.md` §8.1 分配给本文件的职责，供移植时对照；它不是已实现的行为。
//!
//! ## 计划职责
//!
//! §8.1 把 `waitForRetirement`/`load`/`checkProviders`/`prepareAll`/`prepareObject`/
//! `checkRefs`/`entries` 分到本文件。它们都是 TS `reload.ts` 中 `Reload` 的方法；
//! `prepare()` 按下面的顺序调用（`spec/chord-rest.md` §2.20，顺序即语义）：
//!
//! ```text
//! waitForRetirement -> load -> checkProviders -> prepareAll
//!   -> host.gating = changed -> prepareAll（带 gateDeadline）
//!   -> checkRefs -> host.buildTables(entries) -> gate -> 存活检查 -> stage
//! ```
//!
//! ## 各步要点（TS 侧行为）
//!
//! - `waitForRetirement`：先让上一 code version 退役。最多等 `retireDeadline`（默认 5000
//!   ms）直到上一版本消失或 host 报告可退役（`retirable`）；超时且仍未消失时，用
//!   `superseded` 中止它的 roots，再给 `min(retireDeadline, ABORTED_ROOTS_GRACE)`（默认
//!   1000 ms，retireDeadline 更短时收敛窗口也随之缩短）收敛；仍未消失就调用 `host.retire()`。
//! - `load`：同步构建 pending service 表（保留未被替换、也未列入 `remove` 的服务），逐个
//!   运行 facet 的 `setup`，据此计算 `changed`/`removed` 并建立 `host.pending`。
//! - `checkProviders`：同名服务的版本号未变但 `coverage` 判定不兼容 → `contract_mismatch`；
//!   只有 `changed` 中仍存在的服务的 `uses` 才 `bind`，未变或已移除的服务跳过。
//! - `prepareAll`：循环到不动点——收集 changed 类型里尚未成为候选、且 `object.service` 不是
//!   新 service 的 live/activating 对象（已运行新代码的对象跳过），先等它们的 `ready`，再对
//!   仍为 live 的对象执行 `prepareObject`。`prepare()` 调用它两次：
//!   第一次处理当前对象；设置 `host.gating` 后第二次接纳仍在飞行中的激活，此后候选集是
//!   最终的。
//! - `prepareObject`：用新 factory 以“录制模式”跑出 `run` 并建立候选；逐 cell 检查版本
//!   （降低 → `invalid`；升高 → 进 gate；同版本改 schema → `invalid`）；独占资源以及依赖
//!   被推迟资源的新资源推迟到 gate；只有 `reasons` 为空时才执行 `initCells` + `dryRun`。
//! - `checkRefs`：每个活到新版本的引用都要绑定到新 provider。host 的 bootstrap 引用和
//!   `client` 引用不在这里处理：`client` 引用在 commit 的 install 中才 `bind`，仅失败时收到
//!   `upgrade_required` 并 release，成功时走 `shapeChanged`；它们不阻塞 reload。
//! - 末尾的存活检查与 `stage`：在关键 await 之后检查协调者是否已中止本 reload；stage 对
//!   非 state 源立即订阅并缓冲事件，使订阅失败在 prepare 阶段暴露；state/readable-like 源
//!   则在 `activate()`（commit 的 install）时才订阅并取提交时的当前值，其订阅失败到 commit
//!   才暴露。
//!
//! ## 失败路径
//!
//! `prepare()` 捕获任何错误后先 `await rollback()`（实现在 `commit.rs`），再抛
//! `reloadError`：消息加上 `reload aborted: ` 前缀，但保留底层 code（例如
//! `contract_mismatch`）。若消息在去掉可能的 `code: ` 前缀后已经以 `reload aborted`
//! 开头，则原样返回，不重复包装。
//!
//! ## 约束
//!
//! - gated 对象（`reasons` 非空）的 `initCells`/`dryRun` 必须留到 gate 阶段，否则会基于
//!   尚未迁移的值或尚未替换的独占资源做校验。
//! - 本阶段不得发布观察者通知：stage 的 listener 事件只入队，commit 时才激活。
//! - 会失败的检查必须在本阶段完成；commit 假定它们已经通过。
