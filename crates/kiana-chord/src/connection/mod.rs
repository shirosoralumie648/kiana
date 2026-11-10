//! connection 目录。
//!
//! TODO: 按 TS 源逐文件移植；本 crate 内不得出现第二份实现。
//!
//! # 现状：骨架占位
//!
//! 本模块只声明 `feed` 与 `relay` 两个子模块，自身不定义任何类型、函数或 trait，两个子模块
//! 也只有文档、没有定义任何条目。因此连接功能（`Connection`、`AcceptOptions`/`LinkOptions`、
//! `hold` 的状态推送与中继）尚未移植，本 crate 内目前没有可用的连接类型。
//!
//! `crate::index` 的再导出表把 `AcceptOptions`/`LinkOptions` 记在本模块名下，但这两个类型现在
//! 并不存在。`lib.rs` 的“现状与边界”把 `connection` 与 `cell`/`host`/`object` 等一起列为
//! “只有文件级文档、没有任何可调用项的占位”；`index.rs` 的“现阶段（阶段 1，lead 单元）”则把
//! `connection` 归入“属于别的单元”，并说明再导出行在合并阶段由 lead 补齐。按名字找不到定义时，
//! 先回到这里确认。
//!
//! # 与 TS 的对应关系
//!
//! `ARCHITECTURE.md` 的 `kiana-chord` 文件表登记为：
//!
//! ```text
//! connection/feed.rs    connection/feed.ts
//! connection/relay.rs   connection/relay.ts
//! ```
//!
//! 但规格 `spec/chord-core.md` 文首“约束”下的源文件范围（未编号）把连接实现列为
//! 单个源文件 `packages/chord/src/{contract,object,connection,host,reload,remote,manager}.ts`，
//! §2.4 也按单文件编号；本工作区检出的 TS 参考树同样只有 `packages/chord/src/connection.ts`
//! （1327 行），没有 `connection/` 子目录。因此不能把这两个文件名当作 TS 目录结构的镜像，
//! 职责边界应以 §2.4 的条目描述为准。
//!
//! # 约束
//!
//! 连接是有状态的协议端点：规格 §2.4.5 列出每条 `Connection` 的 `#nextId`、`#unacked`、
//! `#pending`、`served`、`relayedRefs` 等字段。这类状态不能由两份实现各自维护，否则同一条
//! 链路会出现两套 id 与窗口记录。移植时只在本目录保留唯一实现，其他模块通过这里的公开接口
//! 使用它，不要在 `host`/`remote` 等模块里另写一套 hold/state 处理。
//!
//! # 阅读顺序
//!
//! 1. `spec/chord-core.md` §1.1 的术语表，以及 §2.4（`connection.ts` 的逐项规格）；
//! 2. TS `packages/chord/src/connection.ts`；
//! 3. 本目录实现落地后，按 `feed`（状态订阅）→ `relay`（hold 中继）的顺序阅读。

// --- 子模块 ---
pub mod feed;
pub mod relay;
