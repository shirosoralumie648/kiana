//! host/tables.ts。
//!
//! TODO: 按 TS 源逐文件移植；本 crate 内不得出现第二份实现。
//!
//! ## 计划职责（当前未实现）
//!
//! `spec/chord-core.md` §8.1 的**建议布局**把注册表表格的构建与安装分到本文件：
//! `buildTables`、`registriesChanged`、`installTables`、`sameTable`。
//!
//! 注册表视图的公开契约已经在 [`crate::registry::RegistryView`]，其文档写明具体实现由
//! `host::tables` 提供（`HostImpl::registry`）。本文件当前没有实现该 trait，也没有任何其它
//! 条目。缺失实现的语义见 `spec/chord-core.md` §2.5.4 的 `registry`：视图按 ctx 的 code
//! version 取表，code version 已不在保留期时返回 `superseded`；`subscribe` 注册后立即同步
//! 回调一次（另见 [`crate::registry`] 的模块文档）。
//!
//! ## 约束
//!
//! 表的 key 顺序是 first-registration order，Rust 侧用
//! [`crate::registry::RegistryTable`]（`IndexMap`）保持；冲突策略由
//! [`crate::registry::Registry`] 的 `conflict` 决定。实现时不得在其它文件另写一份表构建
//! 逻辑（见上面的 TODO）。
