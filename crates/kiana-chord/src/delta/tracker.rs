//! delta/tracker.ts。
//!
//! TODO: 按 TS 源逐文件移植；本 crate 内不得出现第二份实现。
//!
//! ## TS 原文件做什么
//!
//! `tracker.ts`（2292 行）是 delta 的事务层：`track(initial)` 接管一个 JSON 根并给出
//! `value`/`revision`；`beginChange()` 返回一个带 ES Proxy 的 draft，写入先记在覆盖层里；
//! `prepare()` 把脏节点物化成 `Op[]`（op 数超过 `MAX_DELTA_OPERATIONS` 时退化为一条根
//! `r`），`adopt()` 校验 prepared 的归属、状态与 baseRevision 后推进 revision，并把其它
//! 未完成的 change 标记为 stale。数组覆盖层还包含 treap 分段、稳定排序与密集区域优化
//! （完整状态机与 trap 清单见 spec/chord-rest.md §2.30）。
//!
//! ## 为什么 Rust 侧还没有实现
//!
//! tracker 的行为大量依赖 ES Proxy 的 trap：原型链读取、数组 mutator、属性描述符、
//! `ownKeys` 顺序等。`serde_json::Value` 没有这些机制，逐行移植只会得到一个语义不同的
//! 仿制品。spec 给出的方向是显式 Draft API，并在其上生成**相同**的 op 序列；这需要专门
//! 的单元，不属于本文件可以顺带完成的范围。
//!
//! ## 与其他子模块的关系
//!
//! `index`（op 与 codec）、`diff`（修订差异）、`pending`（折叠队列）与
//! `apply_immutable_trusted`（trusted 应用）这些 **Rust 模块**都不依赖 tracker，可以单独
//! 使用。tracker 的依赖方向相反：TS 的 `tracker.ts` 从 `index.ts`（`applyImmutable`、
//! `Op`、`Path` 等）和 `apply-immutable-trusted.ts` 导入，另外使用 `draft.ts` 的 `Draft`
//! 类型与 `json.ts` 的 `copyJson`；它没有导入 `diff.ts` 或 `pending.ts`，所以 `diff` 与
//! `pending` 和 tracker 之间没有依赖关系。注意 TS 的 `index.ts` 反过来 re-export 了
//! tracker 的公开符号（`track`、`appendText` 等），因此“index 不依赖 tracker”只对 Rust
//! 模块成立。
//!
//! “可写草案 + 提交 op”并非完全没有对应能力：`kiana-durable` 的 `session::transaction`
//! 提供值级的 `Tracker`/`Change`/`DocDraft`，`Change::prepare` 用 `diff_revisions` 从
//! base 与当前活值的快照差异生成 op，`abort`/`consume` 推进草稿状态。缺少的是 TS
//! tracker 的 overlay/Proxy、增量 op-log，以及与 TS 逐字节相同的 op 序列保证；这部分
//! 仍属于 kiana-chord 的 delta tracker。
