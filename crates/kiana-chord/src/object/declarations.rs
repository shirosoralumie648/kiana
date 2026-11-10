//! object/declarations.ts。
//!
//! TODO: 按 TS 源逐文件移植；本 crate 内不得出现第二份实现。
//!
//! # 职责
//! 工厂函数通过 `self` 声明的东西（状态单元、引用、资源、监听器、任务、派生值、注册表条目）
//! 会被收集成 `Decls`；按 spec §8.1，`Decls`/`Self` 的共享类型放在 `mod.rs`，本文件负责
//! 「声明如何变成运行期副作用」这一半：监听器的暂存/切换，以及任务的启动/停止。
//! 对应 TS `ObjectImpl` 的 `stageListeners`、`startRuntime`、`startTasks`、`#startTask`、
//! `stopListeners`、`stopTasks`（`spec/chord-core.md` §2.3.20-§2.3.22）。
//! spec §8.1 的文件清单还列了 `registries`；TS 里注册表的校验与重建由 `HostCore` 的
//! `checkRegistrations`/`registriesChanged` 完成（§2.3.4、§2.3.12），移植时需确认这两个
//! 入口的落点。
//!
//! # 为什么要分「暂存」和「启动」
//! 激活与 reload 都是两阶段：先构造候选 `Decls`，确认资源、引用、注册都成功，才允许副作用
//! 生效。监听器因此先暂存（`stageListeners`），返回可 `activate` 或 `discard` 的句柄。
//! 暂存期间「订阅但不投递」：非状态源立即订阅，事件只进缓冲区，直到 `activate` 才投递，
//! 因此 commit 前不会运行任何 handler；候选被放弃时 `discard()` 退订全部 fresh 订阅。
//!
//! # 监听器的关键语义
//! - **跨 reload 复用**：reload 前后同一个 `name` 的监听器，若订阅对象（资源本身或其
//!   source，引用相等）与 `select` 相同，就保留旧订阅、只换回调，不重新调用 `select`。
//!   `select` 相同按函数源码文本比较（`sameSelect`，spec §2.3.11）：每次 factory run 的
//!   内联箭头都是新对象，但源码相同。对只允许单订阅者的源，这是 reload 能存活的关键。
//! - **暂存期缓冲**：非状态源暂存时立即订阅，事件先缓冲，`activate` 后按序投递；状态源
//!   （cell/新的 derivation，或 readable-like 的 held 对象 state 成员；TS 的 `isSource`/
//!   `isReadableLike`）等到 `activate` 才订阅，避免读到候选阶段的旧值。`discard` 退订全部
//!   新订阅；`stageListeners` 中途失败也要退掉已经建立的部分，不能留下半订阅。
//!
//! # 任务的关键语义
//! - 任务的 key 是位置（`#0`、`#1`…），任务没有名字。
//! - 新一轮任务启动前，若上一轮被 abort 的任务还没结束，要等它们全部结束或等
//!   `host.taskStopDeadline`；超时会向宿主报告一个 `aborted` 错误，然后仍启动替代任务。
//!   旧任务在 deadline 前全部结束时新旧两轮不并行；超时只上报并照常启动替代任务，因此
//!   超时窗口内旧任务可能仍在运行，两轮会并行。延迟启动用代号（`#taskRuns`）判定，期间若又
//!   start/stop 或对象已不是 `live`，本次启动作废。
//! - 每个任务拿到 `holder: true` 的 `CtxImpl`，`rootId` 为 `task:<address>:<key>`；
//!   任务里发起的调用各自开新 root（spec §2.3.21）。
//!
//! # 错误处理
//! 监听器源缺少 `subscribe()` → `invalid/implementation`（`listener <key>: source has no
//! subscribe()`）。任务抛错时，若 ctx 未被 abort 则交给 `host.report`；abort 导致的退出不报告。
//!
//! # 现状
//! 本文件目前只有这段说明，没有代码；实现依据是 spec §2.3.20-§2.3.22 与 TS `object.ts`。
