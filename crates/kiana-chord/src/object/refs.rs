//! object/refs.ts。
//!
//! TODO: 按 TS 源逐文件移植；本 crate 内不得出现第二份实现。
//!
//! # 职责
//! 实现「引用」——调用方通过 `ctx.get(...)` / `self.get(...)` 得到的对象句柄：读取 state
//! 成员、调用方法、观察 shape/status、最后 `release()`。spec §8.1 把本地引用的实现放在
//! 本文件：`RefCore` trait、`RefImpl`、`MemberReadable`、`REF_BASE`、`notify_status`。
//! 公开协议 trait 在 `crate::types`（`RefBase`、`Ref`、`RefStatus`、`Shape`、
//! `StatusListener`）；本文件实现它们，远程镜像在 `crate::remote`。
//!
//! # 为什么引用不是普通指针
//! 一个引用同时承担三件事：对象的存活租约（有引用时对象不能被释放）、可随 reload/变体
//! 切换的成员视图、以及可观察的状态（loading/live/released/gone）。因此 `RefImpl` 缓存
//! 按服务绑定的 adapter（`accepts`）和按实现生成的视图，成员访问每次对当前已安装实现
//! 求值，而不是在创建引用时固定（spec §2.3.9）。
//!
//! # 状态机（`RefStatus`）
//! - `loading`：等待对象 ready，成员不可读。
//! - `live`：对象已激活，成员可读、方法可调用。
//! - `released`：调用方显式释放；重复释放是幂等的。
//! - `gone`：对象消失或激活失败，`error()` 给出原因。
//!
//! 构造时对象已 `live` 则引用直接 `live`，否则 `loading`；`ready` 跟随对象的 `ready`，
//! 但若引用自身在等待期间已被置为 `gone`/`released`，则以对应错误拒绝——检查的是引用自己的
//! `status`，对象侧只决定 `object.ready` 本身是否 reject。`live()` 只能从 `loading` 进入；
//! `gone()` 不会覆盖 `released`/`gone`；`release()` 从 `loading`/`live` 进入 `released` 并停止
//! 成员 readable。处于 `released`/`gone` 后，成员读取与方法调用必须得到终止错误，而不是
//! `not_ready`。
//!
//! # 成员读取（`MemberReadable`）
//! 每个 state 成员对应一个稳定的 `Readable`，跟随引用当前 view 里的 source 重新绑定：
//! - `value()`：引用终止 → 终止错误；未 `live` 或 view 不可读 → `not_ready`；变体中没有
//!   该成员 → `not_in_variant`；否则读 source 的值。
//! - `subscribe`/`events`：可读时立即回调当前值/发 `snap`；对事件监听者，不可读时先登记
//!   （`#unseen`），首次可读后补一个 `snap`。重新绑定后的首个上游事件（若不是与已见值
//!   相同的重复快照）对事件监听者翻译成 `reset`。
//! - 只有存在监听者时才订阅上游，最后一个监听者移除后解绑（spec §2.3.8）。
//!
//! # 约束
//! - status/shape 通知必须经发布队列（TS `enqueue`）延后到引起变化的事务完成之后，
//!   不能在 commit 中间回调观察者（spec §2.3.7、§2.3.9）。Rust 侧对应 `crate::publish`。
//! - 监听者抛出的错误交给 `host.report`，不能中断同一批次的其他监听者。
//! - 无 JS `Proxy`：`REF_BASE` 中的成员名（contract/status/ready/variant/shape/error/
//!   onShape/onStatus/release）在 Rust 里是 handle 的方法；`then` 必须永远不可用，以保持
//!   「引用不是 thenable」（spec §8.1）。
//!
//! # 现状
//! 本文件目前只有这段说明，没有代码；`crate::types` 只有 trait 定义，尚无本地实现。
