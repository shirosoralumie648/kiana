//! extensions/codemode/worker.ts。
//!
//! TS 源文件只有一行副作用导入：`import "@earendil-works/kiana-codemode/worker";`
//! （打包时作为独立入口，见 `extensions/codemode/execute.ts` 的 `workerUrl`）。
//!
//! # Rust 侧
//!
//! 本模块**没有逻辑**。worker 入口就是 [`kiana_codemode::runtime::worker`]（编译进
//! `kiana-codemode` 的线程入口 [`kiana_codemode::runtime::worker::run_worker`]），
//! [`kiana_codemode::CodemodeSandbox`] 的默认 spawner 直接调用它。
//!
//! worker 说明符由 `kiana_core::config::get_codemode_worker_specifier()` 解析：原生未打包构建
//! 返回 `None`（使用内置入口），Bun 二进制返回相对源码入口，bundled-node 返回
//! `codemode-worker.js` 的 data: URL。`execute.rs` 把该值原样传给
//! [`kiana_codemode::CodemodeSandboxOptions::worker_url`]；Rust 侧没有「从 URL 载入模块」的机制，
//! 该字段只用于复刻 Node 的入口校验与错误文本（见 `kiana-codemode` 的 `runtime/host.rs`）。
//!
//! # 注意
//!
//! 本模块没有被任何 Rust 代码引用：`pub mod worker;` 只是保持与 TS 目录的一一对应，
//! 它不参与入口选择，也没有导入即启动的副作用（TS 的 `import` 才会启动 worker）。
//! 要追踪真正的 worker 生命周期，读 `kiana-codemode/src/runtime/worker.rs` 的 `run_worker`
//! 以及 `host.rs` 的 spawn 校验；只有 TS 目录结构变化（例如删掉 `worker.ts`）时，
//! 本文件才需要同步调整。
