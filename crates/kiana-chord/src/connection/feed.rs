//! connection/feed.ts。
//!
//! TODO: 按 TS 源逐文件移植；本 crate 内不得出现第二份实现。
//!
//! # 现状
//!
//! 空模块：没有任何条目，只由 `crate::connection` 的 `pub mod feed;` 声明。它不导出可调用的
//! API，本 crate 内也没有代码引用它；`connection` 目录下尚未有连接实现。
//!
//! # 预期职责（尚未实现）
//!
//! `ARCHITECTURE.md` 把本文件登记为 TS `connection/feed.ts` 的对应物，但规格
//! `spec/chord-core.md` 文首“约束”下的源文件范围（未编号）/§2.4 把连接实现当作单文件
//! `connection.ts`，本工作区检出的 TS 参考树也没有 `connection/` 子目录。按规格 §2.4.2
//! “内部数据类型”，`Feed` 的形状是：
//!
//! ```text
//! interface Feed { unsubscribe: () => void; encoder: Encoder; buffer: StateEvent<unknown>[] | undefined }
//! ```
//!
//! 它不是包的导出项，而是连接内部的状态订阅记录：规格 §2.4.9 的 `#attach` 为某个 state 成员
//! 构造 feed（`unsubscribe`/`encoder`/`buffer`）并登记到 `served.feeds`；`readable.events`
//! 立即发出的首个事件由 `#attach` 取 `first.value` 直接构造 `#snapshot(...)` 帧（默认 `snap`，
//! resend 时为 `reset`）；其后到 `#replay` 前的事件才压入 `buffer`，`#replay` 清空 buffer 并按
//! 序调用 `#feedEvent`；`#feedEvent` 在收到 reset 时重建编码器。`Feed` 依赖 `Connection` 的
//! hold 编号与引用的成员读取，不能脱离 §2.4.9 的流程单独使用。
//!
//! # 注意
//!
//! 移植时不要把它提升为跨模块共享的通用订阅设施：按规格它是连接私有的记账结构（§2.4.2 把它
//! 列在“内部数据类型”下），其转发路径经过连接的 `sendState`，受 §2.4.6 的窗口/重同步机制
//! 约束。这与文件头的 TODO 一致——本 crate 内只能有一份实现。
