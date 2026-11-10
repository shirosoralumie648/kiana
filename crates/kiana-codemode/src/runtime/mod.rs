//! runtime 目录：`packages/codemode/src/runtime/**` 的逐文件移植。
//!
//! 每个 TS 文件恰好对应一个子模块；本 crate 内不得出现第二份实现。
//!
//! # 本目录负责什么
//!
//! TS 的 `runtime/` 是 codemode 的执行层：把一段 JavaScript 放进编译成 wasm 的 QuickJS，
//! 并在脚本与宿主之间转发工具调用、输出与终态消息。脚本可见的 API（`tools`、`text()`、
//! `store` 等）定义在 `js/prelude.js`（见 [`prelude_source`]），一次执行的记账、deadline、
//! abort 与结果结算在 [`execution`]，本目录其余模块决定 VM 在哪里跑、怎么起。
//!
//! # 子模块分工
//!
//! - [`protocol`]：宿主 ↔ VM 的消息类型，是本目录所有模块的共同词汇；
//! - [`vm`]：在专用线程上创建 QuickJS，`deliver` 宿主回复，`dispose` 释放实例；
//! - [`execution`]：[`execution::CodemodeSandboxBase`]、[`execution::VmLauncher`]（起 VM 的
//!   接缝）、[`execution::VmChannel`]（宿主 → VM 的接缝）与 [`execution::HostEventLoop`]
//!   （inline 后端用的「宿主事件循环」闸门）；
//! - [`host`]：worker 后端，每次执行一个 `std::thread`；
//! - [`inline`]：在调用线程上（实际是 `vm.rs` 的专用 VM 线程）跑 VM，remote 后端也复用它；
//! - [`remote`]：VM 跑在远端、工具留在调用方的后端；
//! - [`worker`]：worker 线程入口 `run_worker`；
//! - [`prelude_source`]：内嵌 prelude 的字节、上限常量与校验值。
//!
//! # 阅读顺序
//!
//! `protocol` → `vm` → `execution` → `host` 或 `inline` → `remote`/`worker`。本文件只有
//! 模块声明，没有运行逻辑；这里的模块声明决定哪些子模块参与编译以及对外可见性（删除一条
//! `pub mod` 会把对应实现移出编译）。

// --- 子模块 ---
pub mod execution;
pub mod host;
pub mod inline;
pub mod prelude_source;
pub mod protocol;
pub mod remote;
pub mod vm;
pub mod worker;
