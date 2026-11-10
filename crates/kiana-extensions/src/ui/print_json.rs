//! print/json 模式的 UI 上下文。
//!
//! TS 源：`packages/coding-agent/src/core/extensions/runner.ts` 的 `noOpUIContext`
//! （`ca-extensions.md` §2.3.6、`design-extension-runtime.md` §4.5）。
//!
//! print/json 没有交互式 UI：所有成员都是空操作或逐字默认值，且与交互式共用**同一个**
//! 实现——[`crate::runner::noop_ui`] 的进程级单例。本模块只做再导出，避免两份默认值
//! 漂移；`ExtensionRunner::set_ui_context(None, mode)` 安装的也是同一个 `Arc`，因此
//! `has_ui()` 的 `Arc::ptr_eq` 判定对 print/json 成立。
//!
//! 逐字默认值（`design-extension-runtime.md` §4.5）：
//! `select` → `undefined`、`confirm` → `false`、`input`/`editor`/`getTheme`/`getEditorComponent`
//! → `undefined`、`custom` → `undefined as never`、`getEditorText` → `""`、`getAllThemes` → `[]`、
//! `setTheme` → `{ success: false, error: "UI not available" }`、`getToolsExpanded` → `false`；
//! 其余（`notify`/`setStatus`/`setWidget`/`setFooter`/`setHeader`/`setTitle`/`pasteToEditor`/
//! `setEditorText`/`addAutocompleteProvider`/`setEditorComponent`/`setToolsExpanded`）空操作，
//! `onTerminalInput` → 空退订函数。
//!
//! # 设计原因与身份约束
//!
//! 不另建一份 print/json 专用的空实现，原因不只是省代码，而是**引用身份**：
//! `ExtensionRunner::has_ui` 用 `Arc::ptr_eq` 与进程级单例比较，而不是检查模式或能力位
//! （见 [`crate::runner::noop_ui::is_no_op_ui_context`]）。多一份实现就会让 `has_ui()` 把
//! 一个纯空操作对象当成真实 UI；JS 侧 `ctx.hasUI` 直接转发这个判断
//! （`runner::context` 的 `hasUI` getter），因此扩展代码会误以为对话框可用。
//!
//! 执行流程只有一步：[`print_json_ui_context`] 克隆 `LazyLock` 中的那个 `Arc`；没有参数、
//! 没有错误分支、不安装任何东西，也不读取扩展引擎或线程本地状态。首次调用只完成单例的
//! 懒初始化，因此可以在扩展引擎建立之前调用。
//!
//! `Arc` 身份不是可选的实现细节：`ExtensionRunner::set_ui_context(Some(…))` 会先用
//! [`crate::runner::ui_prompt::UiPromptContext`] 包装（产生新的 `Arc`），所以即使传入的
//! 是空上下文，`has_ui()` 也会变成 `true`（`tests/extensions_runner.rs` 的
//! `set_ui_context_uses_the_no_op_singleton_for_has_ui` 锁定了这一行为）。print/json 的
//! 装配路径是 `set_ui_context(None, mode)`，其 `None` 分支直接取用同一单例。

use std::sync::Arc;

// 这三个名字的权威文档在 `runner::noop_ui`；在此再导出只是给 print/json 的调用方一个路径。
// 不要在本模块新增第二份空实现：`has_ui()` 的身份判定依赖全进程只有一个单例（见模块文档）。
pub use crate::runner::noop_ui::{UI_NOT_AVAILABLE, is_no_op_ui_context, no_op_ui_context};
use kiana_ext_api::ui::ExtensionUiContext;

/// print/json 模式的 UI 上下文（与 [`crate::runner::noop_ui::no_op_ui_context`] 同一实例）。
///
/// 返回进程级单例的 `Arc` 克隆：无参数、不会失败，重复调用之间 `Arc::ptr_eq` 成立。
/// 它不读取扩展引擎或线程本地状态，首次调用只初始化 `LazyLock` 内的空实现。逐成员返回值
/// （`select` → `None`、`confirm` → `false`、`set_theme` → 逐字 `"UI not available"` 等）
/// 见 [`crate::runner::noop_ui`] 的模块文档。
///
/// 身份是契约的一部分：`ExtensionRunner::has_ui` 用 `Arc::ptr_eq` 判断当前上下文是否为
/// 这个单例。把返回值再传给 `ExtensionRunner::set_ui_context(Some(…))` 会先经过
/// [`crate::runner::ui_prompt::UiPromptContext`] 包装，产生新 `Arc`，`has_ui()` 随即变成
/// `true`；print/json 模式的装配应走 `set_ui_context(None, mode)`。当前仓库内对本函数的
/// 直接调用只出现在 `tests/print_json_ui.rs`。
pub fn print_json_ui_context() -> Arc<dyn ExtensionUiContext> {
    no_op_ui_context()
}
