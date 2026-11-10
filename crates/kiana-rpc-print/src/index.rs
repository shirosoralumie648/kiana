//! `modes/index.ts` 再导出面（spec §2.1、§2.9、§5.4）。
//!
//! TS 的 `modes/index.ts` 是纯再导出入口（16 行，无运行时代码），共 **15** 个名字。
//! 本 crate 只负责其中 **13** 个非交互名字；两个交互模式名字
//! `InteractiveMode` / `InteractiveModeOptions` 属于 `kiana-interactive` crate
//! （TS 的 `modes/index.ts` 顶部有值再导出，会把整个 TUI 子树拖进 RPC 二进制；
//! spec §2.1 明确要求 Rust 侧按 crate 拆分，因此这里**故意不导出**）。
//!
//! | TS 名字 | Rust 名字 |
//! |---|---|
//! | `JsonAgentSessionEvent` | [`JsonAgentSessionEvent`] |
//! | `PrintModeOptions` | [`PrintModeOptions`] |
//! | `runPrintMode` | [`run_print_mode`]（snake_case 主名）+ [`runPrintMode`]（别名） |
//! | `ModelInfo` | [`ModelInfo`] |
//! | `RpcClient` | [`RpcClient`] |
//! | `RpcClientOptions` | [`RpcClientOptions`] |
//! | `RpcEventListener` | [`RpcEventListener`] |
//! | `runRpcMode` | [`run_rpc_mode`]（snake_case 主名）+ [`runRpcMode`]（别名） |
//! | `RpcCommand` | [`RpcCommand`] |
//! | `RpcExtensionUIRequest` | [`RpcExtensionUIRequest`] |
//! | `RpcExtensionUIResponse` | [`RpcExtensionUIResponse`] |
//! | `RpcResponse` | [`RpcResponse`] |
//! | `RpcSessionState` | [`RpcSessionState`] |
//!
//! **刻意省略**：`RpcSlashCommand`、`RpcCommandType`、`toJsonEvent`
//! （Rust: [`crate::to_json_event`]）、`serializeJsonLine`（[`crate::serialize_json_line`]）、
//! `attachJsonlLineReader`（[`crate::attach_jsonl_line_reader`]）**不**从
//! `modes/index.ts` 导出——它们只从各自的源模块导出。Rust 侧这些名字由 crate 根
//! （`lib.rs` 的 glob 再导出）提供，本模块不重复导出。
//!
//! 两个 camelCase 别名只存在于本模块：crate 根保持 snake_case 主名（§2.9 的 18 个
//! 去重名字），而依赖 TS 名字的调用方可以 `use kiana_rpc_print::index::runPrintMode`。
//! `lib.rs` 的 glob 再导出刻意不包含本模块（`index`），避免同一符号的两个名字在根上
//! 打架。
//!
//! ## 设计原因与维护要点
//!
//! - **为什么别名只放在这里**：TS 调用方按 camelCase 导入（`runPrintMode`），而 crate 根按
//!   spec §2.9 的 snake_case 名字冻结。两个表面互不干扰：根上用
//!   `use kiana_rpc_print::run_print_mode`，需要 TS 拼写时用
//!   `use kiana_rpc_print::index::runPrintMode`。别名是 `pub use ... as`，不是包装函数，
//!   因此签名、文档与行为完全共享（本文件测试用 `type_name_of_val` 断言二者是同一项）。
//! - **增删名字的连带成本**：`crates/kiana-rpc-print/tests/export_surface.rs` 逐项断言本模块
//!   与 crate 根的名字和签名；改名或删除一个条目会让该测试编译失败。这是有意的：这里列出
//!   的条目是对 TS `modes/index.ts` 可见面的冻结契约。
//! - **本模块不放逻辑**：TS 的 `modes/index.ts` 是纯再导出（16 行、无运行时代码），Rust 侧
//!   保持同一形态——只有 `pub use`。任何新的类型或函数都应定义在叶子模块，再从这里重命名
//!   导出；反过来，只在本模块新增名字**不会**让它在 crate 根出现（根上的 glob 显式跳过
//!   `index`）。

// `runPrintMode` / `runRpcMode` 是 TS 名字的逐字别名（camelCase），属本模块的公开契约。
// 注：rustc 的 `non_snake_case` 不检查 `use` 重命名，因此不能把 allow 挂在单个 `pub use`
// 上（clippy::useless_attribute 会拒绝）；放在模块级表达同一意图。
#![allow(non_snake_case)]

pub use crate::json_event::JsonAgentSessionEvent;
pub use crate::print_mode::run_print_mode as runPrintMode;
pub use crate::print_mode::{PrintModeOptions, run_print_mode};
pub use crate::rpc_client::{ModelInfo, RpcClient, RpcClientOptions, RpcEventListener};
pub use crate::rpc_mode::run_rpc_mode;
pub use crate::rpc_mode::run_rpc_mode as runRpcMode;
pub use crate::rpc_types::{
    RpcCommand, RpcExtensionUIRequest, RpcExtensionUIResponse, RpcResponse, RpcSessionState,
};

#[cfg(test)]
mod tests {
    use super::*;

    /// 再导出面是既有项的重命名，不是包装函数：别名与主名指向同一项。
    #[test]
    fn camel_case_aliases_are_the_same_items_as_the_snake_case_names() {
        assert_eq!(
            std::any::type_name_of_val(&run_print_mode),
            std::any::type_name_of_val(&runPrintMode)
        );
        assert_eq!(
            std::any::type_name_of_val(&run_rpc_mode),
            std::any::type_name_of_val(&runRpcMode)
        );
    }

    /// 再导出项与 crate 根的 18 个名字是同一批类型（不是本模块的副本）。
    #[test]
    fn re_exported_types_are_the_crate_root_types() {
        let _: Option<crate::PrintModeOptions> = None::<PrintModeOptions>;
        let _: Option<crate::JsonAgentSessionEvent> = None::<JsonAgentSessionEvent>;
        let _: Option<crate::ModelInfo> = None::<ModelInfo>;
        let _: Option<crate::RpcClientOptions> = None::<RpcClientOptions>;
        let _: Option<crate::RpcEventListener> = None::<RpcEventListener>;
        let _: Option<crate::RpcCommand> = None::<RpcCommand>;
        let _: Option<crate::RpcExtensionUIRequest> = None::<RpcExtensionUIRequest>;
        let _: Option<crate::RpcExtensionUIResponse> = None::<RpcExtensionUIResponse>;
        let _: Option<crate::RpcResponse> = None::<RpcResponse>;
        let _: Option<crate::RpcSessionState> = None::<RpcSessionState>;
        let _: fn(crate::RpcClientOptions) -> crate::RpcClient = RpcClient::new;
    }
}
