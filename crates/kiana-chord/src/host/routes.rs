//! host/routes.ts。
//!
//! TODO: 按 TS 源逐文件移植；本 crate 内不得出现第二份实现。
//!
//! ## 计划职责（当前未实现）
//!
//! `spec/chord-core.md` §8.1 的**建议布局**把路由与远端调用入口分到本文件：`#routes`、
//! `#routeFor`、`#resolve`、`#childNamed`、`#hold`、`#acquireRemote`、`relay`。
//!
//! 术语：`Host::route(prefix, route)` 把某个地址前缀交给一个 `Route` 处理，`Route` 提供
//! `authorize`、`resolve`、`principal`（§2.5.1）；TS 侧的 `#routeFor` 按**最长前缀**匹配
//! 选择路由（`pi/packages/chord/src/host.ts`）。`#childNamed` 按连接的配置名查找 downstream
//! host，`#hold`/`#acquireRemote`/`relay` 负责远端绑定的持有与调用转发。
//!
//! ## 现状与约束
//!
//! 本文件目前没有任何条目，`host/mod.rs` 的 `pub mod routes;` 只公开空模块。实现时不要从
//! 其它模块直接构造路由表：`route.principal` 是必填项（缺失时 TS 抛 `TypeError`），授权、
//! 解析与远端绑定的落点都在本文件（§2.5.4）。
