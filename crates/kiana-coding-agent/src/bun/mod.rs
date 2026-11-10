//! `bun/**` 的等价启动逻辑（documentation-only）。
//!
//! TS 用 `bun build --compile` 打包（`scripts/build-binaries.sh`）：
//!
//! - `bun/cli.ts`：`import "./sandbox-env-setup.ts"; import "./runtime-setup.ts"; import "../cli.ts";`
//!   随后调用 `restoreSandboxEnv()`——在评估读取环境变量的模块之前恢复沙箱环境。
//! - `bun/sandbox-env-setup.ts`：`import "./restore-sandbox-env.ts";`（副作用导入）。
//! - `bun/runtime-setup.ts`：`process.title = APP_NAME`、`process.emitWarning = noop`、
//!   `registerBunOAuthFlows()`、`setBedrockProviderModule(...)`、`setEmbeddedQuickJSWasmPath(...)`。
//! - `bun/restore-sandbox-env.ts`：非 Bun 直接返回；`process.env` 非空返回；否则读
//!   `/proc/self/environ`，按 `\0` 分割并以首个 `=` 恢复变量。
//! - `bun/quickjs-wasm.d.ts`：`quickjs-wasi/quickjs.wasm` 的类型垫片（无运行期代码）。
//!
//! Rust 版是原生二进制：进程启动时环境变量天然完整，不需要从 `/proc/self/environ` 恢复；
//! `runtime-setup` 的 OAuth/Bedrock/QuickJS 注入也没有 Bun 运行时对应物（相关初始化由
//! `kiana-ai`/`kiana-codemode` 各自的单元负责）。因此这里只保留 [`restore_sandbox_env`] 的 no-op：
//! 它是 `bun/**` 在 Rust 公开面上的对应物，本仓库内没有任何调用方（调用它等同于空操作）。
//!
//! **未移植的测试**：`test/restore-sandbox-env.test.ts`（3 例）依赖 Bun/Node 运行时 mock
//! （`process.versions.bun`、`node:fs` 的 `readFileSync` mock）与 `/proc/self/environ` 的
//! 重写；原生 Rust 二进制没有对应行为，按规范 §2.20 记为有意不移植，而不是实现一个表面 test。
//!
//! # 设计原因与边界
//!
//! - **本模块没有运行期行为**：唯一入口是空函数；其余内容只是 TS 启动顺序的对照说明，
//!   不参与执行，启动流程也不会调用它。
//! - **兼容回退在别处**：真正按调用回退读取 `/proc/self/environ` 的是读取方——
//!   `kiana-ai` 的 `utils/provider_env.rs`（仅当进程环境一个变量都没有时读取，结果缓存在
//!   进程级 `Mutex` 中）与 `kiana-resources` 的 `package/exec.rs`（其 `get_env` 同样只在
//!   进程环境为空时回退，每次调用重新读取）。在这里另写一份启动期恢复不会被执行，反而会
//!   形成第二套环境变量来源。
//! - **公开面**：crate 根有 `pub mod bun;`，所以 [`restore_sandbox_env`] 对下游可见；改动
//!   它的签名属于公开 API 变更。

/// `restoreSandboxEnv()`：原生二进制下是 no-op。
///
/// 无参数、返回 `()`、无副作用：调用零次或多次与完全不调用等价。TS 版本只在运行于 Bun
/// 且 `Object.keys(process.env)` 为空时从 `/proc/self/environ` 逐项恢复变量
/// （`spec/ca-cli.md` §2.20）；Rust 启动路径不需要这段逻辑，因此这里刻意保持空实现，
/// 而不是把解析代码搬过来（本仓库没有调用方）。
pub fn restore_sandbox_env() {}
