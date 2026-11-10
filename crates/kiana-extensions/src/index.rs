//! `core/extensions/index.ts` 的再导出面。
//!
//! TS 的 index.ts 是 225 行纯再导出，对外 193 个唯一名字：
//! `types.ts` 177（167 类型 + `defineTool`/9 个 `is*` 守卫）、`loader.ts` 4、
//! `runner.ts` 7（`ExtensionRunner` + 6 个 handler/监听器类型）、`wrapper.ts` 2、
//! `../slash-commands.ts` 2、`../source-info.ts` 1（`spec/ca-extensions.md` §2.4）。
//!
//! Rust 侧的命名映射（`ARCHITECTURE.md` §5.2 的冻结名）：
//! `ExtensionAPI` → [`ExtensionApi`]、`ExtensionContext` → [`ExtensionContextApi`]、
//! `ExtensionToolContext` → [`ExtensionToolContextApi`]、
//! `ExtensionCommandContext` → [`ExtensionCommandContextApi`]、
//! `ExtensionUIContext` → [`ExtensionUiContext`]、`ReplacedSessionContext` →
//! [`ReplacedSessionContextApi`]；函数用 snake_case。其余名字一一对应。
//!
//! 下游 crate 一律 `use kiana_extensions::{...}`（crate 根做了 `pub use index::*`），
//! 不要从 `kiana-ext-api` 的模块路径直接取——那会绕过本 crate 的门面。
//!
//! 比 TS index.ts 多出的 Rust 名字（不改变 193 的语义，只是让装配方少绕路）：
//! `load_extensions_cached`（`loader.ts` 有、index.ts 没再导出；`clear_extension_cache`
//! 仍从 `loader::factory_cache` 取，不在 crate 根，见 `ca-extensions.md` §5.2）、
//! `emit_project_trust_event`/`ProjectTrustOutcome`（`runner.ts` 的自由函数，
//! 项目信任发生在 runner 创建之前，装配方需要直接调用）、以及 `engine` /
//! `virtual_modules` 的装配期类型。
//!
//! # 为什么是一个纯 barrel 文件
//!
//! TS 的 `index.ts` 是扩展 API 的唯一公开入口，本身没有任何逻辑；本文件保持同样形态：
//! 下游只需要记住一个 import 面，不必知道某个类型来自 `types.ts`、`loader.ts` 还是
//! `runner.ts`。crate 根另有 `pub use index::*`，所以 `kiana_extensions::X` 与
//! `kiana_extensions::index::X` 是同一项（`tests/public_surface.rs` 的
//! `the_barrel_module_path_matches_the_crate_root` 对两个路径做了同类型断言）。
//! 本文件除下面 5 个 handler 类型别名外不定义任何实现，也不应加入运行期逻辑。
//!
//! # 结构
//!
//! `pub use` 块与 TS 源一一对应：
//!
//! - `kiana_ext_api::index::*`：`types.ts` 的类型与 helper（`kiana-ext-api` 是冻结契约）；
//! - `kiana-core` 的 `SlashCommandInfo`/`SlashCommandSource`/`SourceInfo`：对应
//!   `../slash-commands.ts` 与 `../source-info.ts`；
//! - loader / runner / project_trust / wrapper：运行期函数与类型；
//! - 5 个 `*Handler` 别名：TS `runner.ts` 声明、`index.ts` 再导出的 handler 类型
//!   （第 6 个 `ExtensionErrorListener` 由上面的 runner 块再导出，不在这里定义）；
//! - `engine` / `virtual_modules`：Rust-only 的装配期类型，TS 侧由 jiti 与
//!   `virtual-modules.ts` 承担。
//!
//! # Feature 门控
//!
//! 只有 `kiana_ext_api` 的类型面、`kiana-core` 的三个类型和 5 个 handler 别名无条件编译；
//! loader/runner/project_trust/wrapper/engine/virtual_modules 的块都带
//! `#[cfg(feature = "runtime")]`。`--no-default-features` 时本 crate 退化为纯类型面
//! （`Cargo.toml` 里所有集成测试都用 `required-features = ["runtime"]` 跳过）。
//!
//! # 维护约束
//!
//! 公开名字集合由 `tests/public_surface.rs` 的编译期 `use` 名单锁定（193 个 TS 名字 +
//! 23 个 Rust-only 名字）；增删名字要同步该清单。`clear_extension_cache` 不在本 barrel：
//! 按 `ca-extensions.md` §5.2 从 `loader::factory_cache` 取。

// ===========================================================================
// types.ts：186 个唯一名字（177 个 TS 导出 + 9 个 Rust-only helper）
// ===========================================================================

// 无 `cfg`：`--no-default-features` 时这是 crate 唯一的类型面。
pub use kiana_ext_api::index::*;

// ===========================================================================
// ../slash-commands.ts 与 ../source-info.ts
// ===========================================================================

// 两个模块的类型定义在 kiana-core（依赖方向 kiana-extensions → kiana-core），这里只转出，
// 与 TS 在兄弟模块声明、由 `index.ts` 再导出的结构一致。
pub use kiana_core::slash_commands::{SlashCommandInfo, SlashCommandSource};
pub use kiana_core::source_info::SourceInfo;

// ===========================================================================
// loader.ts
// ===========================================================================

// 运行期入口。`load_extensions_cached` 是 `loader.ts` 提供、但 TS `index.ts` 没有再导出的
// Rust-only 补充（见文件头的差异清单）。
#[cfg(feature = "runtime")]
pub use crate::loader::{
    create_extension_runtime, discover_and_load_extensions, load_extension_from_factory,
    load_extensions, load_extensions_cached,
};

// ===========================================================================
// runner.ts
// ===========================================================================

// `ExtensionRunner` 与错误监听器；`emit_session_shutdown_event` 是 `runner.ts` 的模块级
// 函数（`ca-extensions.md` §2.3.5），不是 runner 的方法。
#[cfg(feature = "runtime")]
pub use crate::runner::{
    ExtensionErrorListener, ExtensionRunner, current_cwd_override, emit_session_shutdown_event,
    with_cwd_override,
};

// ===========================================================================
// project_trust（`runner.ts#emitProjectTrustEvent`）
// ===========================================================================

// 项目信任在 `ExtensionRunner` 创建之前评估，所以是自由函数而不是 runner 方法（见
// project_trust.rs 的模块文档与 `ca-extensions.md` §2.3.5）。
#[cfg(feature = "runtime")]
pub use crate::project_trust::{ProjectTrustOutcome, emit_project_trust_event};

// ===========================================================================
// wrapper.ts
// ===========================================================================

// 两个适配器；字段投影与“每次执行新建上下文”的实现都在 kiana-tools 的
// `wrap_tool_definition`（见 wrapper.rs 的模块文档）。
#[cfg(feature = "runtime")]
pub use crate::wrapper::{wrap_registered_tool, wrap_registered_tools};

// ===========================================================================
// runner.ts 的 handler 类型（本文件定义 5 个；`ExtensionErrorListener` 已在上面的
// runner 块再导出）
// ===========================================================================
//
// TS 的 `runner.ts` 声明这些别名、`index.ts` 再导出；Rust 的 runner 模块只给
// `ExtensionErrorListener` 起了名字，其余签名内联在 `ExtensionCommandContextActions`
// （new_session / fork / navigate_tree / switch_session）与 `ExtensionContextActions`
// （shutdown）的字段里。公开面要求 TS 的 6 个类型（5 个 handler + 监听器）都可用，
// 因此这里补齐缺的 5 个；签名与 `kiana-ext-api` 的字段类型逐字一致，可以直接赋值。
//
// 类型形状有两条约束：闭包对象本身是 `Send + Sync`（可以跨线程持有），但返回的
// `LocalBoxFuture` 不是 `Send`。TUI 实现（如 kiana-interactive 的 `InteractiveMode`）
// 在异步体里借用线程本地的 `Rc` 状态，因此只能在驱动器所在线程上 poll，不能
// `tokio::spawn` 到多线程运行时。

use std::sync::Arc;

use kiana_ext_api::LocalBoxFuture;
use kiana_ext_api::types::{
    ForkOptions, NavigateTreeOptions, NewSessionOptions, NewSessionResult, SwitchSessionOptions,
};

/// `runner.ts#NewSessionHandler`：`(options?) => Promise<{ cancelled }>`。
pub type NewSessionHandler = Arc<
    dyn Fn(Option<NewSessionOptions>) -> LocalBoxFuture<'static, NewSessionResult> + Send + Sync,
>;

/// `runner.ts#ForkHandler`：`(entryId, options?) => Promise<{ cancelled }>`。
pub type ForkHandler = Arc<
    dyn Fn(String, Option<ForkOptions>) -> LocalBoxFuture<'static, NewSessionResult> + Send + Sync,
>;

/// `runner.ts#NavigateTreeHandler`：`(targetId, options?) => Promise<{ cancelled }>`。
pub type NavigateTreeHandler = Arc<
    dyn Fn(String, Option<NavigateTreeOptions>) -> LocalBoxFuture<'static, NewSessionResult>
        + Send
        + Sync,
>;

/// `runner.ts#SwitchSessionHandler`：`(sessionPath, options?) => Promise<{ cancelled }>`。
pub type SwitchSessionHandler = Arc<
    dyn Fn(String, Option<SwitchSessionOptions>) -> LocalBoxFuture<'static, NewSessionResult>
        + Send
        + Sync,
>;

/// `runner.ts#ShutdownHandler`：`() => void`。
pub type ShutdownHandler = Arc<dyn Fn() + Send + Sync>;

// ===========================================================================
// engine / virtual_modules 的装配期类型（Rust-only；TS 由 jiti/virtual-modules.ts 承担）
// ===========================================================================

// 装配器（lib.rs 的 `Generation`/`ExtensionHost`）与各 crate 的测试使用；不是 TS 扩展 API
// 的一部分，但从 barrel 导出可以让调用方不必引用内部模块路径。
#[cfg(feature = "runtime")]
pub use crate::engine::{
    EngineConfig, EngineResult, HostCall, HostCompletion, HostCompletionSender, HostValue,
    JsEngine, JsError, JsErrorKind, JsHandleId, take_exception,
};
#[cfg(feature = "runtime")]
pub use crate::virtual_modules::{
    BridgeKind, BridgeMember, ConstantValue, NATIVE_BRIDGE_GLOBAL, NamespaceSpec, VIRTUAL_GLOBAL,
};
