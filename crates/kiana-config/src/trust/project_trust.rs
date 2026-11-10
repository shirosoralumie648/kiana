//! `resolveProjectTrusted` / prompt / selector helpers。
//!
//! TS: `core/project-trust.ts`
//!
//! kiana-config 不能依赖 kiana-extensions，因此这里只做**决策**：项目信任上下文用本地 trait
//! （[`ProjectTrustUi`]）表达，扩展事件由注入的 [`ProjectTrustEmitter`] 触发；事件发送与
//! `LoadExtensionsResult` 适配留在 kiana-extensions（`emit_project_trust_event`）。
//!
//! 判定顺序（不得改变，顺序决定信任决策的优先级）：
//! 1. `trustOverride` 给出 → 返回它；
//! 2. 没有需要信任的项目资源 → `true`；
//! 3. 扩展 `project_trust` 事件的结果（`trusted === "yes"`，`remember === true` 时写入）；
//! 4. `trustStore.get(cwd)` 的最近决定；
//! 5. `defaultProjectTrust`（`always` / `never` / `ask`）；
//! 6. 无 UI → `false`；
//! 7. UI 选择（选中则保存 updates 并返回其 `trusted`；未选中 → `false`）。
//!
//! # 边界与依赖方向
//!
//! 决策需要两件事：读写 trust store（[`ProjectTrustStore`]）和询问用户/扩展。后者通过
//! [`ProjectTrustUi`] 与 [`ProjectTrustEmitter`] 两个 trait 注入：kiana-config 只定义接口，
//! 具体实现由上层提供（`kiana-coding-agent` 的 CLI 适配层；扩展事件的实际分发是
//! `kiana-extensions` 的 `emit_project_trust_event`）。因此本文件不构造 UI、不解析扩展，
//! 也不依赖 `kiana-extensions`。
//!
//! # 输入与输出
//!
//! 输入是 [`ResolveProjectTrustedOptions`]，输出是 `Result<bool, TrustError>`：`true` 表示
//! 允许加载项目本地资源，`false` 表示拒绝。返回值只表达结论，不区分“用户点了不信任”还是
//! “没有 UI 可用”。
//!
//! # 失败模式
//!
//! - trust store 的读/写失败（I/O、损坏 JSON、锁竞争）会中断判定并以 [`TrustError`] 返回；
//!   调用方自行决定如何处理（`kiana-coding-agent` 记录首个错误，之后转成运行时创建失败）。
//! - 扩展错误只经 `on_extension_error` 回调上报，**不会**变成 `Err`，也不影响后续判定。
//! - 扩展结果优先于 trust store 中的历史决定（不再调用 `get`）；但 `remember == Some(true)`
//!   时会经 `set` 读改写并写盘，store 的读写错误会以 [`TrustError`] 返回。
//! - UI 取消（`select` 返回 `None`）或返回未知标签都按“未选中”处理：最终 `false`，不写盘。
//! - 只有扩展 `remember == Some(true)` 或用户选中带 `updates` 的选项时才会写盘。
//!
//! # 同步 I/O 与 async
//!
//! [`resolve_project_trusted`] 是 async 函数（需要 await 扩展与 UI），但 [`ProjectTrustStore`]
//! 的读写是同步的：锁竞争时 `auth::lock` 的 `std::thread::sleep` 重试（最多 9 次 × 20 ms）
//! 发生在调用者的执行器线程上。正常路径（无竞争）只有普通文件 I/O。

use std::sync::Arc;

use async_trait::async_trait;
use kiana_core::config::{APP_NAME, CONFIG_DIR_NAME};

use crate::settings::defaults::SETTINGS_DEFAULTS;
use crate::settings::schema::DefaultProjectTrust;
use crate::trust::manager::{
    ProjectTrustOption, ProjectTrustStore, TrustError, get_project_trust_options,
    has_trust_requiring_project_resources,
};

/// `AppMode = "interactive" | "print" | "json" | "rpc"`。
///
/// 本 crate 的决策逻辑不读取它；`kiana-coding-agent` 的适配层用它决定是否展示选择器，
/// 并映射为扩展事件的 `ExtensionMode`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppMode {
    /// 交互式 TUI。
    Interactive,
    /// 单次打印。
    Print,
    /// JSON 输出模式。
    Json,
    /// RPC 模式。
    Rpc,
}

/// `{ type: "project_trust", cwd }`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectTrustEvent {
    /// 被询问的项目目录。
    pub cwd: String,
}

/// `emitProjectTrustEvent` 的单个扩展错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectTrustEventError {
    /// 扩展路径（`error.extensionPath`）。
    pub extension_path: String,
    /// 扩展报告的错误文本（`error.error`）。
    pub error: String,
}

/// 扩展对 `project_trust` 事件的回答。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectTrustEventOutcome {
    /// `"yes"` 表示信任，其余为不信任。
    pub trusted: String,
    /// `true` 时把决定写入 trust store。
    pub remember: Option<bool>,
}

/// `emitProjectTrustEvent(...)` 的返回值：可选结果 + 错误列表。
///
/// `result` 与 `errors` 可以同时非空：[`resolve_project_trusted`] 先逐个回调错误，再采用
/// `result`。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EmitProjectTrustResult {
    /// 扩展返回的结果（没有扩展处理时为 `None`）。
    pub result: Option<ProjectTrustEventOutcome>,
    /// 每个失败扩展的错误。
    pub errors: Vec<ProjectTrustEventError>,
}

/// 扩展 `project_trust` 事件发送器（由 kiana-extensions 适配 `LoadExtensionsResult`）。
///
/// 返回的 `result` 为 `Some` 时，判定直接采用它并跳过对 store 历史决定的查询（`remember`
/// 时仍会写入）；为 `None` 时表示
/// 没有扩展给出决定，继续走后续步骤。
// `async_trait` 生成的 boxed future 自带 `#[must_use]`；clippy 的 double_must_use 看不到宏内
// 属性（与 kiana-ai 的 5 个 trait 相同），因此在这里显式关闭。
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait ProjectTrustEmitter: Send + Sync {
    /// `emitProjectTrustEvent(extensionsResult, event, context)`。
    async fn emit_project_trust(&self, event: ProjectTrustEvent) -> EmitProjectTrustResult;
}

/// `ctx.ui.select(title, items)`（TUI 适配层实现）。
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait ProjectTrustUi: Send + Sync {
    /// 返回被选中的选项标签；取消 → `None`。
    ///
    /// 调用方按**字符串逐字相等**在候选列表中反查选项，因此实现必须返回传入 `options` 中的
    /// 原始标签；返回未知标签与取消同义（都得到 `None`，最终判为不信任）。
    async fn select(&self, title: &str, options: Vec<String>) -> Option<String>;
}

/// `ProjectTrustContext` 中与决策相关的子集。
///
/// 在需要询问用户的路径上（第 6–7 步）：`has_ui == false` 时直接返回 `false`；
/// `has_ui == true` 但 `ui` 为 `None` 时，`select_project_trust_option` 返回 `None`，同样
/// 得到 `false`。
#[derive(Clone, Default)]
pub struct ProjectTrustContext {
    /// `hasUI`。
    pub has_ui: bool,
    /// `ui`（`has_ui` 为 true 时应有值）。
    pub ui: Option<Arc<dyn ProjectTrustUi>>,
}

impl std::fmt::Debug for ProjectTrustContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProjectTrustContext")
            .field("has_ui", &self.has_ui)
            .field("has_ui_impl", &self.ui.is_some())
            .finish()
    }
}

/// 扩展错误回调（`onExtensionError`）。
///
/// 参数是已经格式化好的完整文案：`Extension "<extension_path>" project_trust error: <error>`，
/// 调用方直接展示即可。
pub type ExtensionErrorCallback = Arc<dyn Fn(&str) + Send + Sync>;

/// `ResolveProjectTrustedOptions`。
///
/// `default_project_trust` 为 `None` 表示调用方未显式设置，按
/// `SETTINGS_DEFAULTS.default_project_trust` 处理（当前默认是 `Ask`）。
/// `emit_project_trust` 为 `None` 表示没有扩展结果：跳过扩展事件。
pub struct ResolveProjectTrustedOptions {
    /// 项目目录。
    pub cwd: String,
    /// 信任存储。
    pub trust_store: ProjectTrustStore,
    /// `trustOverride`（`Some` 时直接采用）。
    pub trust_override: Option<bool>,
    /// `defaultProjectTrust`（缺省读 `SETTINGS_DEFAULTS`）。
    pub default_project_trust: Option<DefaultProjectTrust>,
    /// 项目信任上下文。
    pub project_trust_context: ProjectTrustContext,
    /// 扩展事件发送器（对应 TS 的 `extensionsResult` 存在与否）。
    pub emit_project_trust: Option<Arc<dyn ProjectTrustEmitter>>,
    /// 扩展错误回调。
    pub on_extension_error: Option<ExtensionErrorCallback>,
}

/// `formatProjectTrustPrompt(cwd)`（逐字）。
///
/// 文案中的 `kiana` 与 `.kiana` 分别来自 `APP_NAME`、`CONFIG_DIR_NAME`；字节由
/// `tests/fixtures/trust/prompt.json` 锁定。
pub fn format_project_trust_prompt(cwd: &str) -> String {
    format!(
        "Trust project folder?\n{cwd}\n\nThis allows {APP_NAME} to load {CONFIG_DIR_NAME} settings and resources, install missing project packages, and execute project extensions."
    )
}

/// `selectProjectTrustOption(cwd, ctx)`：会话级选项 + 按 label 匹配。
///
/// 用**含会话级选项**的完整列表询问 UI，并按 label 精确匹配回 [`ProjectTrustOption`]。
/// 没有 `ui` 实现时直接返回 `None`；本函数不检查 `has_ui`（调用方负责）。
async fn select_project_trust_option(
    cwd: &str,
    context: &ProjectTrustContext,
) -> Option<ProjectTrustOption> {
    let options = get_project_trust_options(cwd, true);
    let Some(ui) = &context.ui else {
        return None;
    };
    let labels: Vec<String> = options.iter().map(|option| option.label.clone()).collect();
    let selected = ui.select(&format_project_trust_prompt(cwd), labels).await?;
    options.into_iter().find(|option| option.label == selected)
}

/// `saveProjectTrustPromptResult(trustStore, result)`：`updates` 非空时写入。
///
/// `updates` 为空（两个会话级选项）时**不触碰磁盘**；非空时一次写入全部更新。写盘失败原样
/// 返回 trust store 的错误。
pub fn save_project_trust_prompt_result(
    trust_store: &ProjectTrustStore,
    result: &ProjectTrustOption,
) -> Result<(), TrustError> {
    if !result.updates.is_empty() {
        trust_store.set_many(&result.updates)?;
    }
    Ok(())
}

/// `resolveProjectTrusted(options)`：按固定顺序决定项目是否受信任。
///
/// 判定顺序（任一分支命中即返回，不再往下走；与模块文档的 1–7 对应）：
/// 1. `trust_override`；
/// 2. 没有 trust-requiring 资源 → `true`；
/// 3. 扩展 `project_trust` 事件：先回调所有错误；若扩展给了结果，按 `trusted == "yes"`
///    求值，`remember == Some(true)` 时写回 store，然后返回；
/// 4. trust store 的最近决定；
/// 5. `default_project_trust`（`None` → `SETTINGS_DEFAULTS`，即 `Ask`）；`Always` / `Never`
///    直接返回；
/// 6. `Ask` 且 `has_ui == false` → `false`；
/// 7. UI 选择：选中则保存其 `updates` 并返回 `trusted`；取消或未知标签 → `false`。
///
/// # 错误
///
/// 只有 trust store 的读（`get`）与写（`set` / `set_many`）错误会以 [`TrustError`] 返回；
/// 扩展与 UI 的错误不会传播，扩展错误只触发 `on_extension_error`。
pub async fn resolve_project_trusted(
    options: ResolveProjectTrustedOptions,
) -> Result<bool, TrustError> {
    if let Some(trust_override) = options.trust_override {
        return Ok(trust_override);
    }
    if !has_trust_requiring_project_resources(&options.cwd) {
        return Ok(true);
    }

    if let Some(emitter) = &options.emit_project_trust {
        let emitted = emitter
            .emit_project_trust(ProjectTrustEvent {
                cwd: options.cwd.clone(),
            })
            .await;
        for error in &emitted.errors {
            if let Some(on_extension_error) = &options.on_extension_error {
                on_extension_error(&format!(
                    "Extension \"{}\" project_trust error: {}",
                    error.extension_path, error.error
                ));
            }
        }
        if let Some(result) = emitted.result {
            let trusted = result.trusted == "yes";
            if result.remember == Some(true) {
                options.trust_store.set(&options.cwd, Some(trusted))?;
            }
            return Ok(trusted);
        }
    }

    if let Some(decision) = options.trust_store.get(&options.cwd)? {
        return Ok(decision);
    }

    let default_project_trust = options
        .default_project_trust
        .unwrap_or(SETTINGS_DEFAULTS.default_project_trust);
    match default_project_trust {
        DefaultProjectTrust::Always => return Ok(true),
        DefaultProjectTrust::Never => return Ok(false),
        DefaultProjectTrust::Ask => {}
    }

    if !options.project_trust_context.has_ui {
        return Ok(false);
    }

    if let Some(selected) =
        select_project_trust_option(&options.cwd, &options.project_trust_context).await
    {
        save_project_trust_prompt_result(&options.trust_store, &selected)?;
        return Ok(selected.trusted);
    }
    Ok(false)
}
