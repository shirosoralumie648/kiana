//! `modes/index.ts` 再导出面 + spec §2.9 的 18 个 crate 根名字的编译期审计。
//!
//! TS 侧**没有** `modes/index.ts` 的测试（纯再导出，16 行无运行时代码）；15 个名字的
//! 可见性只由 `packages/coding-agent/src/index.ts:419-435` 的再导出块间接保证。Rust 侧
//! 用本文件把两层表面钉死：
//!
//! - crate 根 `kiana_rpc_print`：§2.9 的 18 个去重名字（6 个值 + 12 个类型）；
//! - `kiana_rpc_print::index`：TS `modes/index.ts` 的 13 个非交互名字（15 个标识符，含
//!   `runPrintMode` / `runRpcMode` 两个 camelCase 别名）。
//!
//! 所有断言都是编译期的 `use` / 类型标注；另有一条运行时断言确认别名与主名是**同一项**
//! （`type_name_of_val` 相同），以及 `index` 的类型就是 crate 根的类型（不是复制品）。
//!
//! 刻意不在 `index` 里的名字：`RpcSlashCommand`、`RpcCommandType`、
//! `to_json_event`、`serialize_json_line`、`attach_jsonl_line_reader`；它们只在 crate 根
//! 可用。`InteractiveMode` / `InteractiveModeOptions` 属 `kiana-interactive`，本 crate 不依赖
//! TUI 子树，因此也不在 `index` 里（spec §2.1 的依赖副作用说明）。

#![allow(non_snake_case)]

use std::sync::Arc;

use kiana_rpc_print::index::{
    JsonAgentSessionEvent as IndexJsonAgentSessionEvent, ModelInfo as IndexModelInfo,
    PrintModeOptions as IndexPrintModeOptions, RpcClient as IndexRpcClient,
    RpcClientOptions as IndexRpcClientOptions, RpcCommand as IndexRpcCommand,
    RpcEventListener as IndexRpcEventListener, RpcExtensionUIRequest as IndexRpcExtensionUIRequest,
    RpcExtensionUIResponse as IndexRpcExtensionUIResponse, RpcResponse as IndexRpcResponse,
    RpcSessionState as IndexRpcSessionState, run_print_mode, run_rpc_mode, runPrintMode,
    runRpcMode,
};
use kiana_rpc_print::{
    JsonAgentSessionEvent, ModelInfo, PrintModeOptions, RawStdout, RpcClient, RpcClientOptions,
    RpcCommand, RpcCommandType, RpcEventListener, RpcExtensionUIRequest, RpcExtensionUIResponse,
    RpcResponse, RpcSessionState, RpcSlashCommand, WireError, attach_jsonl_line_reader,
    run_print_mode as root_run_print_mode, run_rpc_mode as root_run_rpc_mode, serialize_json_line,
    to_json_event,
};

/// spec §2.9 的 6 个值名字：每个都必须能从 crate 根解析为可用值。
#[test]
fn crate_root_exposes_the_six_value_names() {
    // 函数项可以原样引用（签名见下方 `crate_root_function_signatures_are_frozen`）。
    let _ = to_json_event;
    let _ = serialize_json_line;
    let _ = attach_jsonl_line_reader::<&[u8]>;
    let _ = root_run_print_mode;
    let _ = root_run_rpc_mode;
    // `RpcClient` 是 class（TS）/ struct（Rust）：构造器即值面的证据。
    let _: fn(RpcClientOptions) -> RpcClient = RpcClient::new;
}

/// spec §2.9 的 12 个类型名字：每个都必须能从 crate 根解析。
#[test]
fn crate_root_exposes_the_twelve_type_names() {
    let _: Option<PrintModeOptions> = None;
    let _: Option<JsonAgentSessionEvent> = None;
    let _: Option<ModelInfo> = None;
    let _: Option<RpcClientOptions> = None;
    let _: Option<RpcEventListener> = None;
    let _: Option<RpcCommand> = None;
    let _: Option<RpcSlashCommand> = None;
    let _: Option<RpcSessionState> = None;
    let _: Option<RpcResponse> = None;
    let _: Option<RpcExtensionUIRequest> = None;
    let _: Option<RpcExtensionUIResponse> = None;
    let _: Option<RpcCommandType> = None;
}

/// 18 个名字与 §8.1 的签名一起构成冻结契约；这里把函数签名也钉死。
#[test]
fn crate_root_function_signatures_are_frozen() {
    let _: fn(&kiana_session::AgentSessionEvent) -> Result<JsonAgentSessionEvent, WireError> =
        to_json_event;
    let _: fn(&serde_json::Value) -> String = serialize_json_line;
    // `attach_jsonl_line_reader` 的泛型参数绑定 `AsyncRead + Unpin`。
    fn _assert_attach<R: tokio::io::AsyncRead + Unpin>() {
        let _ = attach_jsonl_line_reader::<R>;
    }
}

/// TS `modes/index.ts` 的 13 个非交互名字（15 个 Rust 标识符）全部从 `index` 可解析。
#[test]
fn facade_exposes_the_thirteen_non_interactive_ts_names() {
    let _: Option<IndexPrintModeOptions> = None;
    let _: Option<IndexJsonAgentSessionEvent> = None;
    let _: Option<IndexModelInfo> = None;
    let _: Option<IndexRpcClientOptions> = None;
    let _: Option<IndexRpcEventListener> = None;
    let _: Option<IndexRpcCommand> = None;
    let _: Option<IndexRpcSessionState> = None;
    let _: Option<IndexRpcResponse> = None;
    let _: Option<IndexRpcExtensionUIRequest> = None;
    let _: Option<IndexRpcExtensionUIResponse> = None;
    let _: fn(IndexRpcClientOptions) -> IndexRpcClient = IndexRpcClient::new;
    // snake_case 主名 + camelCase 别名（别名只存在于 `index`）。
    let _ = (run_print_mode, runPrintMode, run_rpc_mode, runRpcMode);
}

/// 别名不是包装函数：`runPrintMode` / `runRpcMode` 与 crate 根的 snake_case 函数是同一项。
#[test]
fn facade_names_are_the_same_items_as_the_crate_root_ones() {
    assert_eq!(
        std::any::type_name_of_val(&run_print_mode),
        std::any::type_name_of_val(&root_run_print_mode)
    );
    assert_eq!(
        std::any::type_name_of_val(&runPrintMode),
        std::any::type_name_of_val(&root_run_print_mode)
    );
    assert_eq!(
        std::any::type_name_of_val(&run_rpc_mode),
        std::any::type_name_of_val(&root_run_rpc_mode)
    );
    assert_eq!(
        std::any::type_name_of_val(&runRpcMode),
        std::any::type_name_of_val(&root_run_rpc_mode)
    );
}

/// `index` 再导出的类型就是 crate 根的类型（同一类型，可直接互换）。
#[test]
fn facade_types_are_the_crate_root_types() {
    let _: Option<IndexPrintModeOptions> = None::<PrintModeOptions>;
    let _: Option<PrintModeOptions> = None::<IndexPrintModeOptions>;
    let _: Option<IndexJsonAgentSessionEvent> = None::<JsonAgentSessionEvent>;
    let _: Option<IndexModelInfo> = None::<ModelInfo>;
    let _: Option<IndexRpcClientOptions> = None::<RpcClientOptions>;
    let _: Option<IndexRpcEventListener> = None::<RpcEventListener>;
    let _: Option<IndexRpcCommand> = None::<RpcCommand>;
    let _: Option<IndexRpcSessionState> = None::<RpcSessionState>;
    let _: Option<IndexRpcResponse> = None::<RpcResponse>;
    let _: Option<IndexRpcExtensionUIRequest> = None::<RpcExtensionUIRequest>;
    let _: Option<IndexRpcExtensionUIResponse> = None::<RpcExtensionUIResponse>;
    let factory: fn(IndexRpcClientOptions) -> IndexRpcClient = RpcClient::new;
    let _ = factory;
}

/// TS `rpc-mode.ts` 的再导出面：5 个协议类型也能从 `rpc_mode` 模块解析（与
/// `rpc_mode.ts` 的 `export type { ... } from "./rpc-types.ts"` 对应）。
#[test]
fn rpc_mode_module_reexports_the_five_type_names() {
    use kiana_rpc_print::rpc_mode::{
        RpcCommand as ModeRpcCommand, RpcExtensionUIRequest as ModeRpcExtensionUIRequest,
        RpcExtensionUIResponse as ModeRpcExtensionUIResponse, RpcResponse as ModeRpcResponse,
        RpcSessionState as ModeRpcSessionState,
    };

    let _: Option<ModeRpcCommand> = None::<RpcCommand>;
    let _: Option<ModeRpcExtensionUIRequest> = None::<RpcExtensionUIRequest>;
    let _: Option<ModeRpcExtensionUIResponse> = None::<RpcExtensionUIResponse>;
    let _: Option<ModeRpcResponse> = None::<RpcResponse>;
    let _: Option<ModeRpcSessionState> = None::<RpcSessionState>;
}

/// TS 15 个名字的归属清单（编译期无法枚举模块导出，这里把映射写成数据以防漂移）。
#[test]
fn ts_fifteen_name_surface_is_fully_accounted_for() {
    // `modes/index.ts` 的 15 个 TS 名字，按 §2.1 顺序。
    const TS_NAMES: [&str; 15] = [
        "InteractiveMode",
        "InteractiveModeOptions",
        "JsonAgentSessionEvent",
        "PrintModeOptions",
        "runPrintMode",
        "ModelInfo",
        "RpcClient",
        "RpcClientOptions",
        "RpcEventListener",
        "runRpcMode",
        "RpcCommand",
        "RpcExtensionUIRequest",
        "RpcExtensionUIResponse",
        "RpcResponse",
        "RpcSessionState",
    ];
    // 本 crate `index` 提供的 13 个非交互名字。
    const PROVIDED: [&str; 13] = [
        "JsonAgentSessionEvent",
        "PrintModeOptions",
        "runPrintMode",
        "ModelInfo",
        "RpcClient",
        "RpcClientOptions",
        "RpcEventListener",
        "runRpcMode",
        "RpcCommand",
        "RpcExtensionUIRequest",
        "RpcExtensionUIResponse",
        "RpcResponse",
        "RpcSessionState",
    ];
    // 由 `kiana-interactive` 提供的 2 个交互模式名字。
    const ELSEWHERE: [&str; 2] = ["InteractiveMode", "InteractiveModeOptions"];

    assert_eq!(TS_NAMES.len(), PROVIDED.len() + ELSEWHERE.len());
    for name in PROVIDED.iter().chain(ELSEWHERE.iter()) {
        assert!(TS_NAMES.contains(name), "unexpected TS export name: {name}");
    }
    for name in TS_NAMES {
        assert!(
            PROVIDED.contains(&name) || ELSEWHERE.contains(&name),
            "TS export name is not accounted for: {name}"
        );
    }
}

/// 供签名测试引用的 trait 名（`Arc<dyn RawStdout>` 是 `run_print_mode` 的第三个参数类型）。
#[allow(dead_code)]
fn _raw_stdout_is_object_safe(value: Arc<dyn RawStdout>) {
    let _ = value;
}
