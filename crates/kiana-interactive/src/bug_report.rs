//! modes/interactive/bug-report.ts。
//!
//! `/bug` 流程：同意提示、可选摘要、构建报告，然后上传或导出为 zip。
//!
//! # 负责的边界
//! 本模块只做交互编排：收集用户选项、驱动编辑器 overlay/加载器、调用 `kiana-resources`
//! 的采集与上传/打包函数、把结果写入会话并清理崩溃日志。元数据采集、诊断、zip 格式与
//! HTTP 上传本身都在 `kiana_resources::bug_report`。
//!
//! # 一次 `/bug` 的执行流程
//! 1. `prompt_for_options`：依次弹出「描述」「是否包含 transcript」「是否生成摘要」
//!    「交付方式」四个 overlay；任一步取消即返回。
//! 2. 若选择生成摘要，在编辑器槽位挂载加载器，调用 `AgentSession` 的
//!    `summarize_for_bug_report` 生成摘要（可取消）。
//! 3. `build_bundle`：采集元数据与诊断；包含 transcript 时把当前会话分支序列化为 JSONL。
//! 4. 交付：上传，或在失败后询问是否改为导出 zip；导出成功后把记录写入会话。
//!
//! # 失败与收尾
//! - 绝大多数错误先经 `show_error`/`show_status` 反馈并返回 `Ok(())`；只有 TS 里会 reject
//!   的路径（网关 URL 解析、`process.cwd()`、会话记录写盘）返回 `Err`，由调用方转成崩溃
//!   路径。
//! - 每个 overlay 都用 `OverlayFinish` 把「恢复编辑器 + 唤醒等待者」绑定到一次完成事件；
//!   恢复可能被推迟到 `await` 之后，避免在输入分发持有 `RefCell` 借用时重入 panic。
//! - `record_in_session` 写入报告记录后，若诊断里含崩溃记录会清空崩溃日志，避免同一份
//!   crash 反复随报告上报。
//!
//! # 调用方
//! `interactive_mode::commands::export_import_share::handle_bug_command` 按当前交互模式
//! 状态构造 [`BugReportContext`]，`await` [`report_bug`]，再按结果恢复编辑器或走崩溃路径。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use kiana_ai::types::Model;
use kiana_config::keybindings::KeybindingsManager;
use kiana_config::models::runtime::ModelRuntimeAuthOverrides;
use kiana_resources::bug_report::upload_bug_report;
use kiana_resources::bug_report::{
    BUG_REPORT_CUSTOM_ENTRY_TYPE, BugReportBundle, BugReportDelivery, BugReportSessionEntryData,
    CollectBugReportMetadataOptions, bug_report_archive_file_name, collect_bug_report_diagnostics,
    collect_bug_report_metadata, write_bug_report_archive,
};
use kiana_resources::crash_log::{clear_crash_log, crash_log_path, read_crash_log};
use kiana_session::agent_session::AgentSession;
use kiana_session_core::session_export::{TrailingEntries, serialize_session_branch};
use kiana_session_core::session_manager::SessionManagerError;
use kiana_tui::components::editor::EditorTui;
use kiana_tui::layout_node::SharedComponent;
use kiana_tui::tui::{Container, Tui};
use kiana_ui_kit::bordered_loader::BorderedLoader;
use serde_json::Value;

use crate::components::extension_editor::{ExtensionEditorComponent, ExtensionEditorOptions};
use crate::components::extension_selector::{ExtensionSelectorComponent, ExtensionSelectorOptions};
use crate::session_share::{
    error_message, get_auth_credential, node_cwd_error, node_write_file_error, share_trailing_entry,
};
use crate::tui_renderer::{
    EditorSlot, SharedLoader, create_bordered_loader, request_render, request_render_callback,
    restore_editor_with_loader,
};

const DISCLAIMER: &str = "This report goes to the Kiana developers (Earendil) and is not shared publicly. It includes your kiana version, operating system, the current model and provider configuration (without API keys), loaded extensions, settings, and provider error diagnostics from this session.";
const TRANSCRIPT_NOTE: &str = "The transcript contains your messages, model output, tool calls and their results, including file contents and command output read during this session.";

/// TS `BugReportContext`。
///
/// `/bug` 流程所需的依赖集合：把会话、TUI、编辑器槽位与两个反馈回调打包，让
/// [`report_bug`] 不依赖交互模式的具体类型。字段由调用方在进入 `/bug` 时按当前状态
/// 快照构造，整个流程期间不再从交互模式读取其他状态。
pub struct BugReportContext {
    /// 会话。
    pub session: AgentSession,
    /// TUI 句柄。
    pub ui: Rc<RefCell<dyn Tui>>,
    /// `editorContainer`：overlay 与加载器都临时挂到这里。
    pub editor_container: Rc<RefCell<Container>>,
    /// 当前编辑器（overlay 结束后恢复）。
    pub editor: SharedComponent,
    /// 内置编辑器需要的 TUI 依赖（`ExtensionEditorComponent` 的构造参数）。
    pub editor_tui: EditorTui,
    /// 应用键位管理器（`app.editor.external`）。
    pub keybindings: Rc<KeybindingsManager>,
    /// `showStatus(message)`：非致命反馈（取消、成功、回退提示）。
    pub show_status: Rc<dyn Fn(&str)>,
    /// `showError(message)`：可恢复错误；调用后流程通常以 `Ok(())` 结束。
    pub show_error: Rc<dyn Fn(&str)>,
}

impl BugReportContext {
    /// 交互模式的编辑器槽位。
    fn editor_slot(&self) -> EditorSlot {
        EditorSlot::new(
            Rc::clone(&self.ui),
            Rc::clone(&self.editor_container),
            Rc::clone(&self.editor),
        )
    }

    /// 当前模型名（`model?.name ?? "the current model"`）。
    fn model_name(&self) -> String {
        self.model()
            .map(|model| model.name)
            .unwrap_or_else(|| "the current model".to_string())
    }

    /// 当前模型（`session.model`）。
    fn model(&self) -> Option<Model> {
        self.session.model()
    }
}

/// `BugReportOptions`：用户在四个 overlay 里做出的选择，在采集与交付前固化。
///
/// 字段语义由 [`prompt_for_options`] 决定，一次成型后 [`report_bug`] 的其余阶段只读不写。
#[derive(Debug, Clone, PartialEq, Eq)]
struct BugReportOptions {
    /// 用户输入的问题描述；空串（JS `trim()` 后）折叠为 `None`。
    hint: Option<String>,
    /// 用户选择附带完整 transcript；与 `include_summary` 互斥（见 `prompt_for_options`）。
    include_session: bool,
    /// 用户拒绝发送 transcript 后选择让模型生成摘要。
    include_summary: bool,
    /// 交付方式（`Upload Report` / 非 Cancel 的其余选项归入 zip 导出）。
    delivery: BugReportDelivery,
}

/// 上传结果：TS 的 `undefined`（继续）/ 错误文本。
enum UploadOutcome {
    /// 完成（成功或取消）。
    Done,
    /// 失败文本（触发"导出为 zip?"回退）。
    Failed(String),
}

/// `reportBug(context, initialHint?)`：`/bug` 流程：同意、可选摘要，然后上传或导出。
///
/// 返回 `Err` 对应 TS 中 `reportBug` 的 rejection（未被内部 `showError` 捕获的异常）：
/// `exportZip` 的 `recordInSession`（`appendCustomEntry` 写盘失败）、`process.cwd()` 失败
/// 与 `new URL(getRadiusGatewayUrl())` 解析失败（后两者被包装进
/// [`SessionManagerError::Io`]）。其余失败都会先 `showError`/`showStatus` 并返回 `Ok`。
///
/// # 参数与副作用
/// - `initial_hint`：命令后的初始文本，作为描述输入框的初始值（可为空）。
/// - 正常完成、用户取消与可恢复错误都返回 `Ok(())`；后者只通过 `show_status`/
///   `show_error` 反馈。
/// - 流程期间临时替换编辑器槽位内容，结束时恢复；成功后向会话追加一条
///   `BUG_REPORT_CUSTOM_ENTRY_TYPE` 记录，诊断含崩溃时还会清空崩溃日志。
pub async fn report_bug(
    context: BugReportContext,
    initial_hint: Option<String>,
) -> Result<(), SessionManagerError> {
    let Some(options) = prompt_for_options(&context, initial_hint).await? else {
        (context.show_status)("Bug report cancelled");
        return Ok(());
    };

    if options.delivery == BugReportDelivery::Upload && kiana_offline() {
        (context.show_error)(
            "Uploading bug reports requires online mode. Use Export as Zip instead.",
        );
        return Ok(());
    }

    let mut summary: Option<String> = None;
    if options.include_summary {
        let loader = show_loader(
            &context,
            &format!("Writing summary with {}...", context.model_name()),
        );
        let signal = loader.borrow().signal();
        let slot = context.editor_slot();
        let result = context
            .session
            .summarize_for_bug_report(options.hint.clone(), signal.clone())
            .await;
        match result {
            Ok(text) => {
                restore_editor_with_loader(&slot, &loader);
                if signal.is_cancelled() {
                    (context.show_status)("Bug report cancelled");
                    return Ok(());
                }
                summary = Some(text);
            }
            Err(error) => {
                restore_editor_with_loader(&slot, &loader);
                if signal.is_cancelled() {
                    (context.show_status)("Bug report cancelled");
                } else {
                    (context.show_error)(&format!(
                        "Failed to write bug report summary: {}",
                        error_message(error)
                    ));
                }
                return Ok(());
            }
        }
    }

    let bundle = match build_bundle(&context.session, &options, summary) {
        Ok(bundle) => bundle,
        Err(error) => {
            (context.show_error)(&format!("Failed to build bug report: {error}"));
            return Ok(());
        }
    };

    // 交付：上传失败时询问是否改为导出；只有明确选择 "Export as Zip" 才继续，
    // 取消或关闭对话框都按取消处理。
    if options.delivery == BugReportDelivery::Upload {
        let failure = upload(&context, &bundle).await;
        let UploadOutcome::Failed(failure) = failure else {
            return Ok(());
        };
        let fallback = choose(
            &context,
            "Upload failed",
            &["Export as Zip", "Cancel"],
            Some(&format!(
                "{failure}\n\nExport the report as a zip archive instead?"
            )),
        )
        .await;
        if fallback.as_deref() != Some("Export as Zip") {
            (context.show_status)("Bug report cancelled");
            return Ok(());
        }
    }
    export_zip(&context, &bundle).await
}

/// `promptForOptions(context, initialHint)`。
///
/// `Err` 对应 TS 里 `new URL(getRadiusGatewayUrl())` 抛出的 `TypeError`：该求值发生在
/// `choose("Bug report", …)` 的 description 里，异常让 `reportBug` reject。
async fn prompt_for_options(
    context: &BugReportContext,
    initial_hint: Option<String>,
) -> Result<Option<BugReportOptions>, SessionManagerError> {
    let hint = input(
        context,
        "Report a bug",
        &format!("{DISCLAIMER}\n\nWhat went wrong? (optional)"),
        initial_hint,
    )
    .await;
    let Some(hint) = hint else {
        return Ok(None);
    };
    let transcript = choose(
        context,
        "Include the session transcript?",
        &["Yes, include the transcript", "No"],
        Some(TRANSCRIPT_NOTE),
    )
    .await;
    let Some(transcript) = transcript else {
        return Ok(None);
    };
    let include_session = transcript != "No";
    // 摘要只在用户拒绝发送 transcript 时询问；两者不会同时为真。
    let mut include_summary = false;
    if !include_session {
        let model_name = context.model_name();
        let model_provider = context
            .model()
            .map(|model| model.provider)
            .unwrap_or_else(|| "your provider".to_string());
        let summary = choose(
            context,
            &format!("Attach a summary written by {model_name} instead?"),
            &["Yes, generate a summary", "No"],
            Some(&format!(
                "The transcript is sent to {model_provider} with your credentials and tokens. Only the generated summary is attached; the transcript stays on your machine."
            )),
        )
        .await;
        let Some(summary) = summary else {
            return Ok(None);
        };
        include_summary = summary != "No";
    }
    // TS `hint.trim()`（JS 空白集合）。
    let description = kiana_tui::text::trim_js(&hint);
    let transcript_state = if include_session {
        "included"
    } else {
        "not included"
    };
    let summary_state = if include_summary {
        format!("written by {}", context.model_name())
    } else {
        "none".to_string()
    };
    // TS 在 description 模板里求值 `new URL(getRadiusGatewayUrl()).host`；解析失败时抛错，
    // `reportBug` reject（不会显示交付选择框）。
    let gateway_host = radius_gateway_host()?;
    let delivery = choose(
        context,
        "Bug report",
        &["Upload Report", "Export as Zip", "Cancel"],
        Some(&format!(
            "Description: {}\nTranscript: {transcript_state}\nSummary: {summary_state}\n\nUpload sends the report to {gateway_host}. Export writes a zip archive to the current directory instead.",
            if description.is_empty() {
                "none"
            } else {
                description
            },
        )),
    )
    .await;
    let Some(delivery) = delivery else {
        return Ok(None);
    };
    if delivery == "Cancel" {
        return Ok(None);
    }
    Ok(Some(BugReportOptions {
        hint: (!description.is_empty()).then(|| description.to_string()),
        include_session,
        include_summary,
        delivery: if delivery == "Upload Report" {
            BugReportDelivery::Upload
        } else {
            BugReportDelivery::Zip
        },
    }))
}

/// `new URL(getRadiusGatewayUrl()).host`。
///
/// `getRadiusGatewayUrl()` 每次调用都读取环境变量 `KIANA_RADIUS_GATEWAY`（未设置时用默认
/// 网关），因此交付说明里的主机名反映的是调用时刻的环境。
///
/// TS 的 `new URL` 失败会抛 `TypeError: Invalid URL`（例如 `KIANA_RADIUS_GATEWAY=https://`）；
/// Rust 用 `SessionManagerError::Io` 承载这条 rejection，`Display` 的错误文本保持稳定
/// （用户可见契约，改动需同步测试）。
fn radius_gateway_host() -> Result<String, SessionManagerError> {
    let gateway = kiana_config::defaults::get_radius_gateway_url();
    radius_host_from_gateway(&gateway)
}

/// `new URL(gateway).host` 的纯函数版本（测试用）。
///
/// 端口规则来自 `url` crate：`port()` 只在显式且非默认端口时返回 `Some`，因此
/// `https://example.com:443` 与 `http://example.com:80` 的主机不带端口；IPv6 主机保留方括号。
fn radius_host_from_gateway(gateway: &str) -> Result<String, SessionManagerError> {
    let url = url::Url::parse(gateway).map_err(|_| {
        SessionManagerError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Invalid URL",
        ))
    })?;
    Ok(match url.port() {
        Some(port) => format!("{}:{port}", url.host_str().unwrap_or_default()),
        None => url.host_str().unwrap_or_default().to_string(),
    })
}

/// `buildBundle(session, options, summary)`。
///
/// 依次采集：扩展与扩展错误、全局/项目设置、元数据、诊断（含读取崩溃日志）；在
/// `include_session` 为真时把当前会话分支序列化为 JSONL，并附带 system prompt 与 tools
/// 组成的尾随条目。
///
/// 注意：当前函数体里的每一步都不可失败，因此实际总是返回 `Ok`；调用方的 `Err` 分支
/// 目前不可达。
fn build_bundle(
    session: &AgentSession,
    options: &BugReportOptions,
    summary: Option<String>,
) -> Result<BugReportBundle, String> {
    let loader = session.resource_loader();
    let extensions = loader.get_extensions();
    let extension_errors: Vec<kiana_resources::bug_report::ExtensionError> = extensions
        .errors
        .iter()
        .map(|error| kiana_resources::bug_report::ExtensionError {
            path: error.path.clone(),
            error: error.error.clone(),
        })
        .collect();
    let settings_manager = session.settings_manager();
    let global_settings = settings_manager.get_global_settings();
    let project_settings = settings_manager.get_project_settings();
    let model = session.model();
    let session_manager = session.session_manager();

    let metadata = collect_bug_report_metadata(&CollectBugReportMetadataOptions {
        id: None,
        hint: options.hint.clone(),
        session_id: session.session_id(),
        cwd: session_manager.get_cwd().to_string(),
        include_session: options.include_session,
        include_summary: summary.is_some(),
        message_count: session.messages().len() as u64,
        model: model.as_ref(),
        model_runtime: session.model_runtime(),
        thinking_level: session.thinking_level(),
        extensions: &extensions.extensions,
        extension_errors: &extension_errors,
        global_settings: &global_settings,
        project_settings: &project_settings,
    });

    let diagnostics = collect_bug_report_diagnostics(
        session_manager.as_ref(),
        &read_crash_log(&crash_log_path(None)),
    );

    let session_jsonl =
        if options.include_session {
            let state = session.state();
            let system_prompt = state.system_prompt;
            let tools = state.tools;
            let trailing: TrailingEntries = std::sync::Arc::new(move |parent_id, timestamp| {
                vec![share_trailing_entry(
                    &system_prompt,
                    &tools,
                    parent_id,
                    timestamp,
                )]
            });
            Some(session.with_session_manager_mut(|manager| {
                serialize_session_branch(manager, Some(&trailing))
            }))
        } else {
            None
        };

    Ok(BugReportBundle {
        metadata,
        diagnostics,
        session_jsonl,
        summary,
    })
}

/// `upload(context, bundle)`：返回 `Failed` 时由调用方询问是否改为导出 zip。
///
/// 流程：先取 `radius` provider 的凭据（provider 不存在时凭据为 `None`，仍尝试上传），
/// 再调用 `upload_bug_report` 并共享同一个取消信号。上传成功后写会话记录；写记录失败在
/// TS 里走同一个 catch，因此这里也把它当作上传失败（触发导出回退），而不是 `Err`。
async fn upload(context: &BugReportContext, bundle: &BugReportBundle) -> UploadOutcome {
    let loader = show_loader(context, "Uploading bug report...");
    let signal = loader.borrow().signal();
    let slot = context.editor_slot();

    let provider = context
        .session
        .model_runtime()
        .get_provider(kiana_config::defaults::RADIUS_PROVIDER_ID);
    let token = if provider.is_some() {
        match context
            .session
            .model_runtime()
            .get_auth_for_provider(
                kiana_config::defaults::RADIUS_PROVIDER_ID,
                ModelRuntimeAuthOverrides {
                    min_oauth_validity_ms: Some(5 * 60_000),
                    ..Default::default()
                },
            )
            .await
        {
            Ok(auth) => get_auth_credential(auth.as_ref()),
            Err(error) => {
                restore_editor_with_loader(&slot, &loader);
                if signal.is_cancelled() {
                    (context.show_status)("Bug report cancelled");
                    return UploadOutcome::Done;
                }
                return UploadOutcome::Failed(error.message);
            }
        }
    } else {
        None
    };

    let result = upload_bug_report(
        bundle,
        kiana_resources::bug_report::UploadBugReportOptions {
            token,
            signal: Some(signal.clone()),
            gateway_url: None,
        },
    )
    .await;
    match result {
        Ok(result) => {
            restore_editor_with_loader(&slot, &loader);
            match record_in_session(&context.session, bundle, BugReportDelivery::Upload, None) {
                Ok(()) => {
                    (context.show_status)(&format!(
                        "Bug report uploaded. Report ID: {}",
                        result.id
                    ));
                    UploadOutcome::Done
                }
                Err(error) => {
                    // TS 的 `recordInSession` 在同一个 try 里：写入失败走 catch（再恢复一次
                    // 编辑器），未取消时把错误当作上传失败交给"Export as Zip?"回退。
                    restore_editor_with_loader(&slot, &loader);
                    if signal.is_cancelled() {
                        (context.show_status)("Bug report cancelled");
                        UploadOutcome::Done
                    } else {
                        UploadOutcome::Failed(error_message(error))
                    }
                }
            }
        }
        Err(error) => {
            restore_editor_with_loader(&slot, &loader);
            if signal.is_cancelled() {
                (context.show_status)("Bug report cancelled");
                UploadOutcome::Done
            } else {
                UploadOutcome::Failed(error.message)
            }
        }
    }
}

/// `exportZip(context, bundle)`。
///
/// 在**当前工作目录**写入 `kiana-bug-report-<id>.zip` 并记录到会话。写盘失败只
/// `show_error` 后返回 `Ok`；`current_dir()` 失败与会话记录失败则返回 `Err`——这两种情况
/// 在 TS 里都在导出函数的 `try/catch` 之外，会 reject 整个 `report_bug`。
async fn export_zip(
    context: &BugReportContext,
    bundle: &BugReportBundle,
) -> Result<(), SessionManagerError> {
    let id = metadata_str(&bundle.metadata, "id");
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        // TS 的 `path.join(process.cwd(), …)` 在 try/catch 之外：`process.cwd()` 抛错
        // （`ENOENT: process.cwd failed with error …`）时 `reportBug` reject，不显示
        // "Failed to write bug report"。
        Err(error) => return Err(SessionManagerError::Io(node_cwd_error(error))),
    };
    let archive_path = cwd.join(bug_report_archive_file_name(&id));
    if let Err(error) = write_bug_report_archive(bundle, &archive_path) {
        // TS 显示 Node fs 异常的 `message`（`EACCES: permission denied, open '<path>'`）。
        (context.show_error)(&format!(
            "Failed to write bug report: {}",
            node_write_file_error(&error, &archive_path)
        ));
        return Ok(());
    }
    // TS 里 `recordInSession` 不在 `exportZip` 的 try/catch 内：`appendCustomEntry` 抛错会
    // 让 `reportBug` reject（不显示成功文案）。Rust 用 `Err` 表示同一条传播路径。
    record_in_session(
        &context.session,
        bundle,
        BugReportDelivery::Zip,
        Some(archive_path.to_string_lossy().into_owned()),
    )?;
    (context.show_status)(&format!(
        "Bug report exported to: {}\nReport ID: {id}",
        archive_path.to_string_lossy()
    ));
    Ok(())
}

/// `recordInSession(session, bundle, delivery)`：写入失败原样返回（TS 抛异常）。
///
/// 从 `bundle.metadata` 的 `Value` 里取出 id/时间/提示/包含标记，组装成
/// `BugReportSessionEntryData` 后以自定义条目类型追加到会话。写入成功且诊断里的
/// `crashes` 非空时清空崩溃日志；这一步的失败被忽略（`clear_crash_log` 无返回值）。
fn record_in_session(
    session: &AgentSession,
    bundle: &BugReportBundle,
    delivery: BugReportDelivery,
    path: Option<String>,
) -> Result<(), SessionManagerError> {
    let metadata = &bundle.metadata;
    let data = BugReportSessionEntryData {
        id: metadata_str(metadata, "id"),
        created_at: metadata_str(metadata, "createdAt"),
        hint: metadata
            .get("hint")
            .and_then(Value::as_str)
            .map(str::to_string),
        session_included: metadata
            .get("session")
            .and_then(|session| session.get("included"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
        summary_included: metadata
            .get("session")
            .and_then(|session| session.get("summaryIncluded"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
        delivery,
        path,
    };
    let data = serde_json::to_value(&data).unwrap_or(Value::Null);
    session.with_session_manager_mut(|manager| {
        manager.append_custom_entry(BUG_REPORT_CUSTOM_ENTRY_TYPE, Some(data))
    })?;
    let crashes = bundle
        .diagnostics
        .get("crashes")
        .and_then(Value::as_array)
        .is_some_and(|crashes| !crashes.is_empty());
    if crashes {
        clear_crash_log(&crash_log_path(None));
    }
    Ok(())
}

/// 读取元数据里的字符串字段；缺失或类型不对时返回空串（不报错）。
fn metadata_str(metadata: &Value, key: &str) -> String {
    metadata
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// 对话框结束时的收尾状态（TS 的 `finish()` 闭包共享变量）。
///
/// 完成事件至多生效一次：`sender` 被 `take` 后变为 `None`，`restored` 记录是否已恢复。
/// 因此 submit 与 cancel 同时到达时只会唤醒等待者一次；恢复动作可能重复执行，但不会再
/// 发送第二个结果。
#[derive(Clone)]
struct OverlayFinish {
    slot: EditorSlot,
    sender: Rc<RefCell<Option<futures::channel::oneshot::Sender<Option<String>>>>>,
    restored: Rc<Cell<bool>>,
}

impl OverlayFinish {
    fn new(slot: EditorSlot) -> (Self, futures::channel::oneshot::Receiver<Option<String>>) {
        let (sender, receiver) = futures::channel::oneshot::channel();
        (
            OverlayFinish {
                slot,
                sender: Rc::new(RefCell::new(Some(sender))),
                restored: Rc::new(Cell::new(false)),
            },
            receiver,
        )
    }

    /// TS `finish()`：`restoreEditor(component)` 然后 resolve。
    ///
    /// 恢复在回调里同步执行（与 TS 相同），但如果容器正被外层借走（Rust 在输入分发期间
    /// 可能持有 `RefCell` 借用），则推迟到 `input`/`choose` 的 await 之后，避免重入 panic。
    /// bug-report 的 `restoreEditor` 末尾有 `ui.requestRender()`（session-share 的同名
    /// 函数没有），见 [`EditorSlot::restore`]。
    fn finish(&self, value: Option<String>) {
        if self.slot.container.try_borrow_mut().is_ok() {
            self.slot.restore();
            request_render(&self.slot.ui);
            self.restored.set(true);
        }
        if let Some(sender) = self.sender.borrow_mut().take() {
            let _ = sender.send(value);
        }
    }

    /// await 之后的兜底恢复（回调里已恢复时无操作）。
    fn restore_if_needed(&self) {
        if !self.restored.get() {
            self.slot.restore();
            request_render(&self.slot.ui);
            self.restored.set(true);
        }
    }
}

/// 把完成状态适配成 `FnMut(Option<String>)`：overlay 的提交/取消回调各自持有一份克隆，
/// 共享同一个单次完成通道。
fn value_finish(state: &OverlayFinish) -> impl FnMut(Option<String>) + 'static {
    let state = state.clone();
    move |value| state.finish(value)
}

/// `input(context, title, description, initialValue)`。
///
/// 把 `ExtensionEditorComponent` 挂到编辑器槽位，等待提交或取消；返回 `Some(text)` 表示
/// 提交（可能是空串），`None` 表示取消。`await` 之后调用 `restore_if_needed` 兜底恢复，
/// 与回调里的恢复配对（谁先到谁执行）。
async fn input(
    context: &BugReportContext,
    title: &str,
    description: &str,
    initial_value: Option<String>,
) -> Option<String> {
    let (state, receiver) = OverlayFinish::new(context.editor_slot());
    let mut submit_state = value_finish(&state);
    let mut cancel_state = value_finish(&state);
    let on_submit = move |value: &str| submit_state(Some(value.to_string()));
    let on_cancel = move || cancel_state(None);

    let component = ExtensionEditorComponent::new(
        Rc::clone(&context.ui),
        context.editor_tui.clone(),
        Rc::clone(&context.keybindings),
        title,
        initial_value.as_deref(),
        on_submit,
        on_cancel,
        ExtensionEditorOptions {
            description: Some(description.to_string()),
            ..Default::default()
        },
        Some(
            context
                .session
                .settings_manager()
                .get_external_editor_command(),
        ),
    );
    context.editor_slot().show(Box::new(component));

    let result = receiver.await.ok().flatten();
    state.restore_if_needed();
    result
}

/// `choose(context, title, options, description?)`。
///
/// 与 [`input`] 相同的挂载/恢复模式，只是组件换成 `ExtensionSelectorComponent`：返回被选
/// 中的选项文本，`None` 表示取消。调用方用字符串比较识别具体选项。
async fn choose(
    context: &BugReportContext,
    title: &str,
    options: &[&str],
    description: Option<&str>,
) -> Option<String> {
    let (state, receiver) = OverlayFinish::new(context.editor_slot());
    let mut select_state = value_finish(&state);
    let mut cancel_state = value_finish(&state);
    let on_select = move |value: &str| select_state(Some(value.to_string()));
    let on_cancel = move || cancel_state(None);

    let request_render = request_render_callback(&context.ui);
    let component = ExtensionSelectorComponent::new(
        title,
        options.iter().map(|option| option.to_string()).collect(),
        on_select,
        on_cancel,
        ExtensionSelectorOptions {
            tui: Some(request_render),
            description: description.map(str::to_string),
            ..Default::default()
        },
    );
    context.editor_slot().show(Box::new(component));

    let result = receiver.await.ok().flatten();
    state.restore_if_needed();
    result
}

/// `showLoader(context, message)`。
fn show_loader(context: &BugReportContext, message: &str) -> Rc<RefCell<BorderedLoader>> {
    let loader = create_bordered_loader(&context.ui, message);
    context
        .editor_slot()
        .show(Box::new(SharedLoader::new(Rc::clone(&loader))));
    loader
}

/// `process.env.KIANA_OFFLINE`（任意非空值都为真）。
fn kiana_offline() -> bool {
    std::env::var_os("KIANA_OFFLINE").is_some_and(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    // 测试目标：`radius_host_from_gateway` 与 Node `new URL(...).host` 的逐例对照
    // （含默认端口省略、IPv6 方括号、IDN 转 punycode、非法 URL 报错文本）。
    // 期望值是 Node 26 的实测结果，改动公式前先在 Node 里复核。
    use super::*;

    #[test]
    fn formats_the_radius_gateway_host_like_the_js_url_api() {
        // Node 26 `new URL(...).host` 实测结果。
        for (gateway, expected) in [
            ("https://radius.example.com", "radius.example.com"),
            ("https://Example.COM:443", "example.com"),
            ("https://example.com:8443", "example.com:8443"),
            ("https://[::1]:8080", "[::1]:8080"),
            ("https://bücher.example", "xn--bcher-kva.example"),
            ("http://example.com:80", "example.com"),
        ] {
            assert_eq!(
                radius_host_from_gateway(gateway).expect("valid gateway"),
                expected,
                "{gateway}"
            );
        }
        // `new URL` 抛 `TypeError: Invalid URL`。
        for gateway in ["https://", "http://", "https://ex ample.com", "https://%"] {
            let error = radius_host_from_gateway(gateway).expect_err(gateway);
            assert_eq!(error.to_string(), "Invalid URL", "{gateway}");
        }
    }
}
