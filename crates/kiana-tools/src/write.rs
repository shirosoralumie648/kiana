//! core/tools/write.ts。
//!
//! `write` 工具：在 [`crate::file_mutation_queue::try_with_file_mutation_queue`] 里
//! `mkdir -p` + 覆盖写，成功文案使用**原始** `path` 参数（不是绝对路径）。
//!
//! `realpath` key 计算失败（TS 里 `getMutationQueueKey` 的 rethrow）会在 `mkdir` 之前
//! 直接以 Node 的原始文案失败。
//!
//! # 中止语义
//!
//! 与 TS 的注释一致：**不**在 abort 监听里 reject（那会在文件操作仍在飞行时释放
//! mutation 队列）。改为每个 `await` 之后检查 `signal.aborted`；观察到的中止相同，
//! 但队列会一直锁到当前操作落定。
//!
//! # 在 crate 中的位置
//!
//! [`create_write_tool_definition`] / [`create_write_tool`] 由 `crate::index` 的
//! `create_*` 集合按 `ToolName::Write` 调用，并沿 `kiana-coding-agent` 的导出面暴露给下游；
//! `kiana-extensions` 的 `@earendil-works/kiana-coding-agent` 虚拟模块也直接导出这两个构造函数。调用行的内容预览由
//! [`crate::renderers::write`] 负责，本文件只处理执行。
//!
//! # 执行流程
//!
//! 1. 把参数反序列化成 [`WriteToolInput`]（`path`/`content` 都是必填字符串），失败文本原样
//!    成为工具错误消息；
//! 2. `effective_cwd` 优先取扩展上下文里的 cwd；占位上下文或空 cwd 回落到创建工具时传入的
//!    cwd；
//! 3. `resolve_to_cwd` 把 `path` 解析成绝对路径（`~` 展开、Unicode 空格归一、去 `@` 前缀），
//!    `dirname` 取父目录；
//! 4. 在 [`try_with_file_mutation_queue`] 里按该绝对路径排队：mkdir → 写文件；在第一次
//!    mkdir 之前、mkdir 之后与写文件之后各检查一次取消信号；
//! 5. 成功结果只有一条文本 `Successfully wrote to {原始 path}`，`details` 为 JSON `null`。
//!
//! # 失败模式
//!
//! - mutation 队列的 key 计算会在执行闭包之前进行；含 NUL 的路径与 `realpath` 的
//!   ELOOP/EACCES/ENAMETOOLONG 等错误直接返回 Node 风格消息，父目录不会被创建
//!   （测试 `realpath_errors_abort_before_any_side_effect` 锁定）；
//! - `mkdir` 的错误经 `ToolOperationError::from_io` 变成 `mkdir '{path}'` 风格的消息；
//!   写文件时创建失败报 `open '{path}'`，而 `write`/`flush` 失败的消息不带路径（Node 的
//!   `fs.writeFile` 同样如此）；只有可识别的 errno 才带 `{code}: ` 前缀，否则回落为
//!   底层错误的文本。`ToolOperationError::code` 在可识别 errno 时不会单独进入
//!   `AgentToolError`，消息里已经带 code；
//! - 取消信号命中时返回消息 `Operation aborted`，但队列要等当前文件操作落定才释放。
//!
//! # 注意事项
//!
//! 成功文案使用的是**原始** `path` 参数而不是解析后的绝对路径；改动它会让测试与 TS 输出
//! 不一致。默认实现（[`LocalWriteOperations`]）的写文件是全量覆盖（`File::create` 截断已有
//! 内容），不是增量编辑；自定义 [`WriteOperations`] 可以自行决定语义。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use kiana_agent_core::{AgentTool, AgentToolError, AgentToolResult};
use kiana_ai::types::{ConstrainedSampling, JsonValue, TextContent, TextKind, UserContentBlock};
use kiana_ext_api::tool_types::{
    ToolDefinition, ToolDefinitionExecute, ToolRenderers, WriteToolInput,
};
use kiana_ext_api::types::ExtensionToolContextApi;
use kiana_sandbox::ExecutionPolicy;
use tokio_util::sync::CancellationToken;

use crate::definition::wrap_tool_definition;
use crate::file_mutation_queue::try_with_file_mutation_queue;
use crate::path_utils::resolve_to_cwd;
use crate::read::{
    PriorReadStatus, ReadStateRegistry, ToolOperationError, effective_cwd,
    file_changed_since_read_error, prior_read_verification_error, read_before_write_error,
    read_not_text_error, sandbox_denied_error, write_file_bytes,
};
use crate::renderers::write::write_renderers;

/// `writeToolSystemPromptContribution` 的 Rust 形状。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriteToolSystemPromptContribution {
    /// 系统提示 "Available tools" 段的一行摘要。
    pub snippet: &'static str,
    /// 追加到 Guidelines 段的条目。
    pub guidelines: &'static [&'static str],
}

/// `writeToolSystemPromptContribution`。
///
/// 返回值同时用于 `create_write_tool_definition` 的 `prompt_snippet`/`prompt_guidelines`；
/// 测试 `definition_matches_the_prompt_contribution` 保证两处不会漂移。
pub fn write_tool_system_prompt_contribution() -> WriteToolSystemPromptContribution {
    WriteToolSystemPromptContribution {
        snippet: "Create or overwrite files",
        guidelines: &["Use write only for new files or complete rewrites."],
    }
}

/// `write` 的工具描述（逐字）。
pub const WRITE_TOOL_DESCRIPTION: &str = "Write content to a file. Creates the file if it doesn't exist, overwrites if it does. Automatically creates parent directories.";

/// 可插拔的文件写入操作。TS: `WriteOperations`。
///
/// 两个方法都在 mutation 队列的独占区间内被调用：同一队列 key 的操作不会并发，不同 key
/// 仍可能并行，因此实现只需要保证单次调用自身安全。错误用 [`ToolOperationError`] 返回，
/// 调用方只取 `message` 作为 `AgentToolError.message`（`code` 字段不单独保留）。
// `async_trait` 展开的 Future 方法自带 `#[must_use]`，clippy 看不到宏内属性（见 kiana-ai 同类处理）。
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait WriteOperations: Send + Sync {
    /// 把内容写入文件。
    async fn write_file(
        &self,
        absolute_path: &Path,
        content: &str,
    ) -> Result<(), ToolOperationError>;
    /// 递归创建目录。
    async fn mkdir(&self, dir: &Path) -> Result<(), ToolOperationError>;
}

/// 默认操作：本地文件系统（`fs.writeFile(path, content, "utf-8")` / `fs.mkdir(recursive)`）。
///
/// `write_file` 走 `read::write_file_bytes`：`File::create` 会创建或**截断**目标文件，
/// `write_all` 之后显式 `flush` 等待阻塞池里的写完成；`mkdir` 用
/// `tokio::fs::create_dir_all`，目录已存在不算错误。
#[derive(Debug, Default, Clone, Copy)]
pub struct LocalWriteOperations;

#[async_trait]
impl WriteOperations for LocalWriteOperations {
    async fn write_file(
        &self,
        absolute_path: &Path,
        content: &str,
    ) -> Result<(), ToolOperationError> {
        write_file_bytes(absolute_path, content.as_bytes()).await
    }

    async fn mkdir(&self, dir: &Path) -> Result<(), ToolOperationError> {
        tokio::fs::create_dir_all(dir)
            .await
            .map_err(|error| ToolOperationError::from_io("mkdir", dir, error))
    }
}

/// `write` 工具的选项。
#[derive(Clone, Default)]
pub struct WriteToolOptions {
    /// 自定义文件写入操作。默认本地文件系统。
    pub operations: Option<Arc<dyn WriteOperations>>,
    /// 写入门禁使用的执行策略。`None` 时按执行期 cwd 从 `KIANA_SANDBOX_POLICY` 构造。
    ///
    /// 会话层要把权限层的 `addDirs` / 多根工作区合并进同一份策略时传入显式实例。
    pub execution_policy: Option<Arc<ExecutionPolicy>>,
    /// 读记录表。`None` 时使用进程级共享实例（与 read 工具一致）。
    pub read_state: Option<Arc<ReadStateRegistry>>,
}

// `Option<Arc<dyn WriteOperations>>` 无法派生 `Debug`（trait object 没有 Debug 约束），
// 手写实现只报告是否配置了自定义操作，便于日志定位。
impl std::fmt::Debug for WriteToolOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WriteToolOptions")
            .field("operations", &self.operations.is_some())
            .field("execution_policy", &self.execution_policy.is_some())
            .field("read_state", &self.read_state.is_some())
            .finish()
    }
}

/// `writeSchema`（TypeBox 序列化结果，逐字节）。
///
/// 只有 `path`/`content` 两个必填 string 属性；两个 `description` 与 TS 一字不差，
/// 测试 `parameters_schema_is_verbatim` 锁定整个 JSON。模型可见的参数说明来自这里，
/// 改动会同时改变工具定义与会话里保存的工具描述。
pub fn write_parameters_schema() -> JsonValue {
    serde_json::json!({
        "type": "object",
        "required": ["path", "content"],
        "properties": {
            "path": { "type": "string", "description": "Path to the file to write (relative or absolute)" },
            "content": { "type": "string", "description": "Content to write to the file" }
        }
    })
}

/// `{ type: "json_schema", strict: "prefer" }`。
///
/// `Prefer` 表示尽量使用 strict 模式，provider 做不到时回退（`Require` 则会报错）。
fn json_schema_prefer() -> ConstrainedSampling {
    ConstrainedSampling::Config(kiana_ai::types::ConstrainedSamplingConfig::JsonSchema {
        strict: kiana_ai::types::StrictMode::Prefer,
    })
}

/// 取消时的统一错误（TS: `new Error("Operation aborted")`），`name`/`stack` 均不填充。
fn aborted_error() -> AgentToolError {
    AgentToolError {
        message: "Operation aborted".to_string(),
        name: None,
        stack: None,
    }
}

/// 参数反序列化失败：`serde_json` 的错误文本原样作为 `message`。
///
/// schema 校验在 agent loop 里先跑；这里兜住类型不匹配（例如 `content` 是数字）时
/// `from_value` 的错误。
fn invalid_input_error(error: impl std::fmt::Display) -> AgentToolError {
    AgentToolError {
        message: error.to_string(),
        name: None,
        stack: None,
    }
}

/// 把文件操作错误投影成工具错误：只保留 `message`（Node 风格的 code 已在消息前缀里），
/// `code` 字段与 `name`/`stack` 都不进入 `AgentToolError`。
fn operation_error(error: ToolOperationError) -> AgentToolError {
    AgentToolError {
        message: error.message,
        name: None,
        stack: None,
    }
}

/// `path.dirname(absolutePath)`。
///
/// 解析后的路径总是绝对路径：`Path::parent` 对 `/a/b` → `/a`、对 `/a` → `/`，
/// 与 Node 的 `dirname` 一致；根目录没有 parent 时回落到自身（`dirname("/") === "/"`）。
/// 这是纯词法操作，不检查文件系统，目标不存在也能得到父目录。
fn dirname(absolute_path: &Path) -> PathBuf {
    absolute_path
        .parent()
        .map_or_else(|| absolute_path.to_path_buf(), Path::to_path_buf)
}

/// `write` 的 `execute` 实现。
///
/// 持有创建工具时的 cwd、操作实现与写入门禁配置，在多次调用之间共享（`operations` 是
/// `Arc`，实现必须 `Send + Sync`）。每次执行都从 `ctx` 重新取 cwd 并重新解析路径，
/// 不存在跨调用的路径缓存。
struct WriteExecute {
    cwd: String,
    operations: Arc<dyn WriteOperations>,
    execution_policy: Option<Arc<ExecutionPolicy>>,
    read_state: Arc<ReadStateRegistry>,
}

/// 写入门禁：统一执行策略 + read-before-write。返回归一化后的真实路径。
///
/// 必须在 `file_mutation_queue` 的锁内调用：两个并行写同一路径时，后一个会在前一个
/// 完成后重新检查（而不是都基于同一份旧读记录）。
fn check_write_allowed(
    policy: &ExecutionPolicy,
    read_state: &ReadStateRegistry,
    absolute_path: &Path,
) -> Result<PathBuf, AgentToolError> {
    let real_path = policy
        .resolve_write_target("write", absolute_path)
        .map_err(sandbox_denied_error)?;

    // 只有「已存在的普通文件」需要读记录。新建文件天然没有旧内容；目录/设备文件交给
    // 后续 mkdir/write 产生它们自己的错误（保持原有文案）。
    let existing_regular_file = std::fs::metadata(&real_path)
        .map(|metadata| metadata.is_file())
        .unwrap_or(false);
    if existing_regular_file {
        match read_state.check_prior_read(&real_path) {
            PriorReadStatus::Fresh(record) if record.covers_all_lines() => {}
            PriorReadStatus::Fresh(record) => {
                return Err(crate::read::read_before_write_partial_error(
                    &real_path,
                    record.line_count,
                ));
            }
            PriorReadStatus::Missing => return Err(read_before_write_error(&real_path)),
            PriorReadStatus::Changed(reason) => {
                return Err(file_changed_since_read_error(&real_path, &reason));
            }
            PriorReadStatus::Unverifiable(reason) => {
                return Err(prior_read_verification_error(&real_path, &reason));
            }
            PriorReadStatus::NotText => return Err(read_not_text_error(&real_path)),
        }
    }
    Ok(real_path)
}

impl ToolDefinitionExecute for WriteExecute {
    fn execute<'a>(
        &'a self,
        _tool_call_id: &'a str,
        params: JsonValue,
        signal: Option<&'a CancellationToken>,
        _on_update: Option<kiana_agent_core::AgentToolUpdateCallback>,
        ctx: Arc<dyn ExtensionToolContextApi>,
    ) -> kiana_agent_core::types::BoxFuture<'a, Result<AgentToolResult<JsonValue>, AgentToolError>>
    {
        Box::pin(async move {
            // 阶段 1：解析参数。schema 校验由 agent loop 先做，这里只兜住类型不匹配。
            let input = match serde_json::from_value::<WriteToolInput>(params) {
                Ok(input) => input,
                Err(error) => return Err(invalid_input_error(error)),
            };
            let path = input.path;
            let content = input.content;
            // 阶段 2：确定 cwd 与目标绝对路径。resolve_to_cwd 是纯词法解析，不要求文件存在。
            let cwd = effective_cwd(&ctx, &self.cwd);
            let absolute_path = resolve_to_cwd(&path, &cwd);
            let policy = self
                .execution_policy
                .clone()
                .unwrap_or_else(|| Arc::new(ExecutionPolicy::from_env(&cwd)));

            // 阶段 3：按绝对路径串行化。key 计算失败时闭包不会执行（realpath 错误直接返回）。
            try_with_file_mutation_queue(absolute_path.clone(), || async {
                // 不在 abort 监听里 reject：见模块文档。
                let throw_if_aborted = || -> Result<(), AgentToolError> {
                    match signal {
                        Some(token) if token.is_cancelled() => Err(aborted_error()),
                        _ => Ok(()),
                    }
                };

                // 每个文件操作前后都检查取消；已经完成的 mkdir/write 不会回滚（TS 同样如此）。
                throw_if_aborted()?;
                // 写入门禁：策略（realpath/可写根/.git/.kiana/大小写）与读记录都在锁内检查，
                // 结论与同一策略下的 shell 一致。
                let real_path = check_write_allowed(&policy, &self.read_state, &absolute_path)?;
                // 目录也用真实路径推导：符号链接已在门禁里展开，避免 mkdir 与写落在不同位置。
                let dir = dirname(&real_path);
                throw_if_aborted()?;

                self.operations.mkdir(&dir).await.map_err(operation_error)?;
                throw_if_aborted()?;

                self.operations
                    .write_file(&real_path, &content)
                    .await
                    .map_err(operation_error)?;
                // 写成功后旧读快照立即失效；用写出的字节建立新基线（见 read 模块文档）。
                self.read_state
                    .record_write(&real_path, content.as_bytes(), true);
                throw_if_aborted()?;

                // 成功文案使用原始参数 path（不是 absolute_path），这是当前稳定文案，改动需同步测试。
                Ok(AgentToolResult {
                    content: vec![UserContentBlock::Text(TextContent {
                        kind_text: TextKind::Text,
                        text: format!("Successfully wrote to {path}"),
                        text_signature: None,
                    })],
                    details: JsonValue::Null,
                    structured_content: None,
                    usage: None,
                    is_error: None,
                    terminate: None,
                })
            })
            .await
            .map_err(operation_error)?
        })
    }
}

/// 创建 `write` 的 [`ToolDefinition`]。
///
/// TS: `createWriteToolDefinition(cwd, options?)`。
///
/// 生成的字段固定：
/// - `name`/`label` 都是 `"write"`，`description` 逐字来自 [`WRITE_TOOL_DESCRIPTION`]；
/// - `prompt_snippet`/`prompt_guidelines` 来自 [`write_tool_system_prompt_contribution`]；
/// - `parameters` 是 [`write_parameters_schema`]，`constrained_sampling` 为
///   `json_schema` + `strict: "prefer"`，`output_schema` 为 `None`（没有 structuredContent）；
/// - 渲染器来自 [`crate::renderers::write::write_renderers`]；
/// - `execute` 使用 `options.operations`，缺省为 [`LocalWriteOperations`]。
pub fn create_write_tool_definition(
    cwd: impl Into<String>,
    options: Option<WriteToolOptions>,
) -> ToolDefinition {
    let options = options.unwrap_or_default();
    let contribution = write_tool_system_prompt_contribution();
    let renderers: ToolRenderers = write_renderers();
    ToolDefinition {
        name: "write".to_string(),
        label: "write".to_string(),
        description: WRITE_TOOL_DESCRIPTION.to_string(),
        prompt_snippet: Some(contribution.snippet.to_string()),
        prompt_guidelines: Some(
            contribution
                .guidelines
                .iter()
                .map(|guideline| (*guideline).to_string())
                .collect(),
        ),
        parameters: write_parameters_schema(),
        constrained_sampling: Some(json_schema_prefer()),
        render_shell: renderers.render_shell,
        prepare_arguments: None,
        output_schema: None,
        exposure: None,
        namespace: None,
        reaches_tools: None,
        annotations: None,
        default_active: None,
        prepare_loadout: None,
        execution_mode: None,
        execute: Arc::new(WriteExecute {
            cwd: cwd.into(),
            operations: options
                .operations
                .unwrap_or_else(|| Arc::new(LocalWriteOperations)),
            execution_policy: options.execution_policy,
            read_state: options.read_state.unwrap_or_else(ReadStateRegistry::global),
        }),
        render_call: renderers.render_call,
        render_result: renderers.render_result,
    }
}

/// 创建 `write` 的 [`AgentTool`]（`wrapToolDefinition` 投影）。
///
/// `wrap_tool_definition` 只复制 agent loop 需要的字段（name/label/description/parameters/
/// constrained_sampling/output_schema/prepare_arguments/execution_mode/execute），prompt
/// 元数据与渲染器不会进入 [`AgentTool`]；需要这些字段时用
/// [`create_write_tool_definition`]。
pub fn create_write_tool(cwd: impl Into<String>, options: Option<WriteToolOptions>) -> AgentTool {
    wrap_tool_definition(create_write_tool_definition(cwd, options), None)
}
