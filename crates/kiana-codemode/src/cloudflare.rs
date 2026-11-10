//! `cloudflare.ts`——**不移植**（决策：`ARCHITECTURE.md` §8 第 3 条）。
//!
//! TS 的入口依赖三样 Workers 专属能力：
//!
//! 1. `import quickjsWasmImport from "quickjs-wasi/quickjs.wasm"`——Wrangler 的
//!    `CompiledWasm` 规则把它变成 `WebAssembly.Module`（Workers 不能运行时编译 wasm 字节）；
//! 2. `import { DurableObject } from "cloudflare:workers"`——`CodemodeSandboxDurableObject`
//!    的父类与本 crate 无法提供的 RPC 传输；
//! 3. `RemoteCodemodeSandbox` 依赖 Durable Object 的 `script_name` 绑定（跨 Worker 隔离区调用）。
//!
//! Rust 侧没有 `wasm32-unknown-unknown` 的 M1 目标，也没有 DO 绑定层。本模块是
//! **占位**（`ARCHITECTURE.md` §8 第 3 条）：「编译通过、调用即明确报错」。模块无条件编译
//! （与骨架及 `kiana-durable`/`kiana-chord` 的同批占位一致），调用即返回逐字的 not supported 错误。
//! `cloudflare` feature 名保留为空开关，是为了阶段 8 之后若引入 wasm32 目标**只需替换实现**，
//! 下游的 `use` 与 feature 组合不用改。
//!
//! `portable` 面（`RemoteCodemodeSandbox` 的客户端侧）与 Cloudflare 无关，始终可用，
//! 见 `kiana_codemode::portable`；TS 的 `/cloudflare` 入口也先 `export * from "./portable.ts"`，
//! 这里用同样的一条 `pub use` 复刻（见文件末尾）。
//!
//! 实现者**不要**用 `wasmtime` 在进程内模拟隔离区：`DEFAULT_REMOTE_MEMORY_LIMIT_BYTES` 的
//! 内存上限、脚本阻塞调用方线程的行为与 Workers 的 isolate 不同，`codemode` 的 remote 测试
//! 会得到不同的超时语义。
//!
//! # 与 TS 的签名差异
//!
//! TS 的两个入口都是同步的：`cloudflareQuickJSWasm` 是 `CodemodeWasmModule` 常量，
//! `createInlineCodemodeSandbox(options?)` 直接返回 `InlineCodemodeSandbox`。Rust 占位把两者
//! 的返回类型写成 `Result<(), CloudflareCodemodeUnsupported>` 并恒返回 `Err`，调用方只能走
//! 错误分支，不会误以为拿到了可用的 wasm 模块或沙箱。
//!
//! # 验证
//!
//! `crates/kiana-codemode/tests/cloudflare.rs` 在默认 feature 下运行（本模块无条件编译）：
//! 它锁定常量 33554432、两条入口的错误文案逐字相等，并检查 `pub use crate::portable::*`
//! 暴露的 portable 面。

/// `DEFAULT_REMOTE_MEMORY_LIMIT_BYTES`：Durable Object 给每个 VM 的默认内存上限（32 MiB）。
pub const DEFAULT_REMOTE_MEMORY_LIMIT_BYTES: usize = 32 * 1024 * 1024;

/// “不支持”错误的固定文案（进入 `Display`，逐字）。
///
/// 与 [`CloudflareCodemodeUnsupported`] 的 `#[error("...")]` 是两份独立字符串：`#[error]`
/// 的格式串本身必须是字面量（但可用 `#[error("{}", UNSUPPORTED_MESSAGE)]` 引用常量）；
/// 当前仍保留两份独立字符串，改一处必须同步另一处。
/// `tests/cloudflare.rs` 的 `unsupported_message_is_verbatim` 同时断言二者相等，漏改会失败。
pub const UNSUPPORTED_MESSAGE: &str =
    "Cloudflare Workers codemode is not supported in the Rust build";

/// Cloudflare Workers 后端不可用。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("Cloudflare Workers codemode is not supported in the Rust build")]
pub struct CloudflareCodemodeUnsupported;

/// `cloudflareQuickJSWasm`：Wrangler 编译好的 `quickjs.wasm`（永远失败）。
///
/// TS 里它是不带参数的常量；Rust 用函数是为了给“后端不可用”一个可返回的错误通道。
///
/// # 错误
///
/// 总是返回 [`CloudflareCodemodeUnsupported`]。函数不检查平台、不做 IO，任何目标上结果相同。
pub fn cloudflare_quickjs_wasm() -> Result<(), CloudflareCodemodeUnsupported> {
    Err(CloudflareCodemodeUnsupported)
}

/// `createInlineCodemodeSandbox()`（永远失败）。
///
/// 等价的可用入口是 `kiana_codemode::runtime::inline::InlineCodemodeSandbox`（feature `wasm`）。
///
/// # 错误
///
/// 总是返回 [`CloudflareCodemodeUnsupported`]。TS 的 `options` 参数在占位签名里被省略，所以
/// 这里不接受配置、也不会尝试加载 `quickjs.wasm`。
pub fn create_inline_codemode_sandbox() -> Result<(), CloudflareCodemodeUnsupported> {
    Err(CloudflareCodemodeUnsupported)
}

// TS: `export * from "./portable.ts";`（`packages/codemode/src/cloudflare.ts` 的第一条导出）。
//
// 这是 `/cloudflare` 与 `/portable` 公开面重合的原因：从 `kiana_codemode::cloudflare` 拿到的
// `CodemodeSandboxBase`/`InlineCodemodeSandbox`/`RemoteCodemodeSandbox`/`serveCodemodeRemote`/
// `MAX_*`/协议类型/全部 `Codemode*` 类型，与 `kiana_codemode::portable` 完全同一批符号。
// 用 glob 再导出一份，而不是手抄名字，避免清单漂移。
pub use crate::portable::*;
