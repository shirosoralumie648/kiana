//! emit_project_trust_event 与 ProjectTrustContext（types.ts 的声明在 kiana-ext-api）。
//!
//! TS: `core/extensions/runner.ts#emitProjectTrustEvent`（`runner.ts:302–330`）。
//!
//! 语义：
//!
//! - 按扩展加载顺序、每个扩展内 handler 注册顺序调用 `project_trust` handler；
//! - **第一个非 `"undecided"` 的结果获胜**，后续 handler 全部跳过（包括同一扩展里后面的
//!   handler——TS 注释明确允许单个扩展注册多个 project_trust handler）；
//! - handler 抛错不中断循环：错误收进 `errors`（`error.message` 或 `String(error)`，stack
//!   同理），继续下一个 handler；
//! - 不接受 handler 位置上的标准事件 handler（loader 保证 `on("project_trust", ...)` 用
//!   `ProjectTrustHandler` 形态登记）。
//!
//! 这是个自由函数而不是 `ExtensionRunner` 的方法：项目信任在会话创建之前评估
//! （`ca-extensions.md` §2.3.5），此时还没有 runner。派发仍走 `dispatch.rs` 的同一套
//! JS 桥（`__kiana_dispatch.emitProjectTrust`）与同一个快照语义。
//!
//! # 执行流程
//!
//! 1. [`emit_project_trust_event`] 建立错误收集器，把错误回调交给派发层；
//! 2. `dispatch::run_project_trust_dispatch` 先用 `loader::refresh_extension_handlers`
//!    把每个扩展的 handler 表刷成 live 状态——扩展工厂在求值期间调用
//!    `kiana.on("project_trust", ...)` 注册的 handler 因此不会漏掉（TS 里 runner 与扩展
//!    共享同一个 `Map`）；
//! 3. 快照按扩展顺序展开，`js/dispatch.js#emitProjectTrust` 顺序 `await` 每个 handler：
//!    `trusted === "undecided"` 继续，其它值立即返回；抛错（含返回 `null`/`undefined`
//!    引发的 TypeError）经 `report` 进入错误收集器后继续下一个 handler；
//! 4. 全部 handler 都未决定时 JS 返回 `null`，Rust 侧映射为 `None`；否则反序列化成
//!    [`ProjectTrustEventResult`]。
//!
//! # 失败模式
//!
//! - 单个 handler 的错误不会中断派发，`result` 与 `errors` 可以同时非空；
//! - 基础设施失败（引擎未安装、结果无法反序列化）由派发层返回 `Err`，本函数把它追加为
//!   一条 `extension_path` 为空的 [`ExtensionError`]（TS 会直接 reject；这里是启动路径，
//!   调用方需要一份可展示的错误列表）；
//! - 必须在安装引擎的同一线程上 `await`：派发层的引擎登记是线程本地的
//!   （`dispatch.rs` 的 `CURRENT_ENGINES`），换线程只会得到 `EngineNotInstalled`。
//!
//! # 与 TS 的差异
//!
//! 每个 handler 收到的是按 JSON 重新反序列化的事件值和一个 `ProjectTrustContext` 克隆
//! （其中 UI 回调的 `Arc` 与原上下文共享），不是同一个对象；handler 对事件/上下文自身
//! 字段的修改不会传给后续 handler。
//!
//! # 错误顺序
//!
//! `errors` 按 handler 出错顺序排列；若有基础设施错误，它排在最后。

use std::sync::{Arc, Mutex};

use kiana_ext_api::events::{ProjectTrustContext, ProjectTrustEvent, ProjectTrustEventResult};
use kiana_ext_api::types::{ExtensionError, LoadExtensionsResult};

use crate::runner::dispatch::{self, DispatchError};

/// `emitProjectTrustEvent` 的返回值。TS: `{ result?: ProjectTrustEventResult; errors:
/// ExtensionError[] }`。
///
/// # 不变量
///
/// - `result` 只保留第一个非 `undecided` 的决定，之后不再调用任何 handler；
/// - `result.is_some()` 时 `errors` 仍可能非空（在决定者之前失败的 handler）；
/// - 基础设施错误产生的条目 `event == "project_trust"`、`extension_path` 为空。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProjectTrustOutcome {
    /// 第一个非 `undecided` 的决定（没有时 `None`）。
    pub result: Option<ProjectTrustEventResult>,
    /// 被捕获的 handler 错误（按发生顺序）。
    pub errors: Vec<ExtensionError>,
}

/// 发出 `project_trust` 事件。TS: `runner.ts#emitProjectTrustEvent`。
///
/// 引擎与 `js/dispatch.js` 必须已安装（crate-facade 的启动顺序；测试里用
/// `dispatch::install` + prelude）。基础设施失败（引擎缺失 / 结果无法反序列化）也会作为
/// 一条 `ExtensionError` 返回，不让调用方 panic——项目信任是启动路径，错误必须可展示。
///
/// # 参数
///
/// - `extensions_result`：预信任阶段加载的扩展集合。本函数只读 `extensions` 字段
///   （`errors`/`warnings`/`runtime` 不参与），并先按 live 登记刷新 handler 表；
/// - `event`/`ctx`：原样交给 JS 派发层；`ctx` 按 handler 逐个克隆。
///
/// # 返回
///
/// 顺序与“第一个非 `undecided` 获胜”的规则见 [`ProjectTrustOutcome`]。handler 级错误
/// 已收集进 `errors`，派发继续；没有 handler 作决定时 `result` 为 `None`。
///
/// # 失败
///
/// 不 panic：基础设施失败时 `result` 为 `None`，错误列表末尾追加一条 `extension_path`
/// 为空的 [`ExtensionError`]；此前收集到的 handler 错误仍保留。
pub async fn emit_project_trust_event(
    extensions_result: &LoadExtensionsResult,
    event: ProjectTrustEvent,
    ctx: ProjectTrustContext,
) -> ProjectTrustOutcome {
    // 用 `Arc<Mutex<Vec<_>>>` 而不是 `RefCell`：错误回调（`dispatch::ErrorSink`）是
    // `Send + Sync` 的 trait 对象，收集结果又要在 `await` 之后读回。handler 实际都在同一
    // 线程上调用，锁不承担并发保护；`into_inner` 让锁中毒（此前的持锁者 panic）不阻断
    // 启动路径。
    let errors: Arc<Mutex<Vec<ExtensionError>>> = Arc::new(Mutex::new(Vec::new()));
    let sink_errors = Arc::clone(&errors);
    let sink: dispatch::ErrorSink = Arc::new(move |error: ExtensionError| {
        sink_errors
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(error);
    });

    // 快照刷新、JS 调用与结果反序列化都在派发层；本层只聚合错误并归一化返回值。
    let result =
        dispatch::run_project_trust_dispatch(&extensions_result.extensions, event, ctx, sink).await;

    let mut collected = std::mem::take(
        &mut *errors
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()),
    );
    match result {
        Ok(result) => ProjectTrustOutcome {
            result,
            errors: collected,
        },
        Err(error) => {
            // 基础设施错误没有可用的部分结果：`result` 置 `None`，但此前收集的 handler
            // 错误保留，基础设施错误追加在最后。
            collected.push(dispatch_error_to_extension_error(error));
            ProjectTrustOutcome {
                result: None,
                errors: collected,
            }
        }
    }
}

/// 基础设施错误 → `ExtensionError`（TS 里这类错误会直接 reject，不会进 `errors`；
/// 项目信任是启动路径，Rust 侧选择把它作为一条错误返回）。
fn dispatch_error_to_extension_error(error: DispatchError) -> ExtensionError {
    // `ExtensionError` 没有 name 字段：`Js` 变体只保留 message/stack，name 被丢弃；
    // 其它变体用 `Display` 文本（固定文案或反序列化错误）。
    match error {
        DispatchError::Js { message, stack, .. } => ExtensionError {
            extension_path: String::new(),
            event: "project_trust".to_owned(),
            error: message,
            stack,
        },
        other => ExtensionError {
            extension_path: String::new(),
            event: "project_trust".to_owned(),
            error: other.to_string(),
            stack: None,
        },
    }
}
