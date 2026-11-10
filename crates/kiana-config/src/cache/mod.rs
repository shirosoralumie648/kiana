//! `core/cache-warmer.ts` 与 `core/cache-stats.ts`（后者实现在 `kiana-session-core`）。
//!
//! # 模块边界
//!
//! 本文件只是 `kiana-config` 中 `cache` 子树的入口，本身没有逻辑：预热器的全部实现与
//! 文档都在 [`warmer`] 子模块（`core/cache-warmer.ts` 的移植），排查预热问题时从那里读起。
//! 本文件没有再导出 `warmer` 的条目，因此 `kiana_config::cache::CacheWarmer` 这样的路径
//! 不存在，必须写完整的 `kiana_config::cache::warmer::CacheWarmer`。
//!
//! `core/cache-stats.ts` 不放在本 crate，而是 `kiana-session-core::cache_stats`：它按
//! `SessionEntry` 顺序扫描会话，属于 `ARCHITECTURE.md` §3.7 所说的「会话数据层」；该层
//! 因为 `kiana-config` 等 crate 在较低层级就要使用，被整体拆到 `kiana-session-core`。
//!
//! # 调用方与访问路径
//!
//! `kiana-config/src/index.rs` 用 `pub use crate::cache;` 保留模块命名空间（`lib.rs` 再
//! `pub use index::*;`），所以下游按 `kiana_config::cache::warmer::<条目>` 引用：
//!
//! - `kiana-session/src/sdk.rs` 装配 `warmer::CacheWarmer`，并在真实请求的 `stream_fn`
//!   里调用 `start`；`agent_session.rs` 的 `cache_warming_status()` 转发 `status()`。
//! - `kiana-extensions` / `kiana-ext-api` 在扩展决策路径上使用
//!   `warmer::CacheWarmingAction` 与 `warmer::CacheWarmingDecisionEvent`。
//! - `kiana-interactive` 的 `/session` 与 transcript 分别调用
//!   `warmer::format_cache_warming_status`、`warmer::format_cache_warming_usage`。

pub mod warmer;
