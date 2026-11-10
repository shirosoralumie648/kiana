//! host/roots.ts。
//!
//! TODO: 按 TS 源逐文件移植；本 crate 内不得出现第二份实现。
//!
//! ## 计划职责（当前未实现）
//!
//! `spec/chord-core.md` §8.1 的**建议布局**把 root 的生命周期分到本文件：`root`、
//! `joinRoot`、`endRoot`、`foreignRootEnded`、`retire`、`allowRetire`、`watchRetirement`。
//!
//! 术语：root 是一次调用树。宿主为每个 rootId 统计当前在本宿主上运行的调用数，本地 root 的
//! 最后一个调用结束（`finally` 路径）时才 `endRoot`；`retirable` 又要求 previous code version
//! 的 roots 为空，因此 root 计数按 code version 保存（§2.5.4）。
//! `foreignRootEnded` 处理远端链路通知“从它进来的 root 已结束”：只有确实从该链路进来的
//! root 才能被它结束，等到没有携带它的链路时 root 才真正结束（§2.5.4）。
//!
//! ## 现状与约束
//!
//! 本文件目前没有任何条目：`host/mod.rs` 的 `pub mod roots;` 只公开一个空模块。实现时 root
//! 计数需要与 `host/mod.rs` 里的宿主状态共用同一份 code version 表，不得在其它文件复制计数
//! 或退休逻辑（见上面的 TODO）。
