//! object/invoke.ts。
//!
//! TODO: 按 TS 源逐文件移植；本 crate 内不得出现第二份实现。
//!
//! # 职责
//! 本文件是对象对外「工作入口」的准入与生命周期控制。spec §8.1 把它对应到
//! `call`/`invoke`/`lease`/`guard`/`rootEnded`/`admit`/`dispose`/`unwind`：方法调用、
//! 监听器事件的调用包装、cell 写入守卫、root 租约、以及对象的释放。调用路径的 TS 依据是
//! `spec/chord-core.md` §2.3.23-§2.3.25。
//!
//! # 一次方法调用的顺序（迁移时必须保持）
//! 1. `ctx.check(host)`：ctx 必须属于当前宿主、未过期、未 abort；跨宿主 →
//!    `invalid/foreign_ctx`，调用已结束 → `aborted/expired`，被强制退役 → `superseded`。
//! 2. 引用已 `released`/`gone` → 终止错误；引用 `loading` → 等 `ready`，同时可被 ctx 取消。
//! 3. 引用或对象不是 `live` → `gone`（对象状态）或引用的终止错误。
//! 4. `admit(ctx)`（见下）。
//! 5. 取 `ctx.codeVersion` 对应的实现；没有 → `superseded`。
//! 6. 用 `ref.viewOf(installed)` 取该 code version 的实现视图（`installed` 是
//!    `ctx.codeVersion` 对应的实现，可能是已保留的旧 code version；包含 `accepts` adapter）
//!    找到成员；不是方法 → `not_in_variant`。
//! 7. 仅 remote 契约逐次校验参数：按消费者自己的 schema 与提供者 schema 依次裁剪
//!    （additive 规则），再 `assertValid`。本地调用不逐次校验（spec §2.3.24）。
//! 8. `lease(ctx)` 记录 root 对对象的占用；为本次调用建 child ctx；`#invoke` 执行并计数：
//!    `takesArgs`（`method` 为 undefined 或 `method.args` 存在）时以 `fn(input, child)` 调用，
//!    否则方法不接收参数，以 `fn(child)` 调用；结束后关闭 child。
//! 9. `returns` 为假（方法声明无返回值）时丢弃实现的返回值、返回 `undefined`；否则若
//!    `validated?.result` 存在（remote 契约），先 `assertValid("result", ...)`
//!    （`invalid/result`）再返回结果。
//!
//! # 准入（`admit`）的拒绝条件
//! 按 spec §2.3.23 的顺序，任一成立即拒绝：
//! - 宿主已失败 → `unavailable/host_failed`；
//! - ctx 的 code version 已退役 → `superseded`；
//! - ctx 的 code version 早于对象被升级到的版本 → `upgrade_conflict`；
//! - 对象正在 reload gate（reload 期间的升级闸门）中，且该 root 不在 gate 打开那一刻对象
//!   租约集合（`leases` 的键）里，即未被「祖父化」→ `upgrade_conflict`。
//!
//! cell 写入还要先过 `guard`：工厂运行期间禁止写入（`invalid/read_only`），ctx 必须有效，
//! 对象已 `disposed` → `gone`；`task:` 前缀的 root 跳过 `admit`（任务的写入不受 gate/
//! 升级限制）。
//!
//! # 租约与存活
//! `lease(ctx)` 把 `rootId` 记入对象的租约表并通知宿主（`host.touch`），使 reload gate 知道
//! 这个 root 还在使用对象；`rootEnded(rootId)` 删除租约，若 gate 的祖父集合因此清空则通知
//! `drained`。`#invoke` 还把每个方法/监听器调用登记到对象的在飞集合：对象在没有引用时
//! 也要等这些调用结束才能释放（spec §2.3.23-§2.3.25）。
//!
//! # 释放（`dispose` / `unwind`）
//! - `dispose(forced, reason)` 幂等：第一次调用创建释放 future 并缓存，后续返回同一个。
//!   强制模式立即把所有引用置 `gone` 并 abort `callAbort`（取消在飞调用）；两种模式都等待
//!   所有 invocation settle，再按 `openOrder` 逆序关闭资源、释放引用，最后置 `disposed`
//!   并通知宿主。
//! - `unwind(run)` 用于激活失败或 reload 丢弃候选：先 detach 该 run 的派生，再逆序关闭它
//!   创建的资源、释放它创建的引用；清理步骤用 `runAll` 语义全部执行后统一报错。
//!
//! # 现状
//! 本文件目前只有这段说明，没有代码。
