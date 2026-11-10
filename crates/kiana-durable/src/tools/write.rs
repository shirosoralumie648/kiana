//! `tools/write.ts`：写入文件（必要时创建父目录），同一文件的写入串行化。
//!
//! # 这个模块负责什么
//! 注册并执行 `write` 工具：把模型给的路径规范化成绝对路径，在“同一文件”的变更队列里调用
//! `ExecutionEnv::write_file`（Node 实现会在写入前递归创建父目录，见 `env/node.rs`），最后
//! 返回一条成功文本。工具本身不解析内容、不做换行转换，`content` 原样交给环境。
//!
//! # 执行流程
//! 1. `execute_write` 用 serde 反序列化 [`WriteToolInput`]；缺字段或类型不符时返回 `Err`
//!    （`harness/tool.rs` 会折算成工具错误结果）；
//! 2. `require_env` 取本次调用的执行环境；没有配置环境时报普通错误；
//! 3. `resolve_tool_path` 先做 Unicode 空白归一化、去掉模型常加的 `@` 前缀，再转成绝对
//!    路径（见 `tools/path_utils.rs`）；
//! 4. `with_file_mutation_queue` 以 `env.id()` + 规范路径为 key 串行化同一文件的变更。队列
//!    内部先检查取消，再 `write_file`（覆盖写），写完后再次检查取消；
//! 5. 成功时返回 `Successfully wrote to {input.path}`。
//!
//! # 为什么需要队列
//! `edit` 是读-改-写，它的读取与写回都在同一个队列槽位内完成（见 `tools/edit.rs` 的
//! `execute_edit`）。`write` 也走同一把 key，因此这两个工具对同一文件的修改不会交错；
//! 不同文件、不同文件系统互不等待。队列只在**本进程内**有效，以 `env.id()` 区分文件系统
//! 命名空间；它**不是**对 `bash` 命令或其他进程的锁（`file_mutation_queue` 的模块文档
//! 也写明了这一点）。
//!
//! # 取消与错误
//! 取消是协作式的：`aborted` 直接读 `Context::abort_signal()`。
//! - 写前已取消：不调用 `write_file`，返回 `Operation aborted`；
//! - `write_file` 成功返回后才发现取消：写入可能已经落盘，但调用仍返回
//!   `Operation aborted`，不会报成功。`write_file` 自身也接收 `Context`，写入过程中的
//!   取消由它的实现决定错误形态。
//!
//! 因此调用方不能把 `Operation aborted` 当作“文件一定没被改动”的证据。
//! `tests/tools.rs::keeps_the_mutation_queue_locked_until_an_aborted_write_settles` 锁定了
//! “取消后队列仍要等这次写入落定，第二次写入才能开始”的行为。
//!
//! # 注意事项
//! - 返回文本里的路径是调用方传入的**原始** `path`（可能是相对路径），不是解析后的绝对
//!   路径；
//! - 非取消的 `write_file` `FileError` 被装箱成 `BoxError` 向上抛，由 harness 折算成工具
//!   错误结果并以 `failed` 结算；取消错误（如 Node 实现因取消返回的 `FileErrorCode::Aborted`，
//!   见 `env/node.rs` 的 `cancellable_io`）在 runtime 信号已取消时由 harness 走 abort 路径
//!   （以 `aborted` 结算），不生成 `tool_error`。两种路径都不会让进程失败；
//! - 本模块不检查目标是否存在或是否是目录：创建、覆盖与父目录处理都由
//!   `ExecutionEnv::write_file` 的实现决定（Node 实现见 `env/node.rs` 的 `write_file`）。

use std::sync::Arc;

use kiana_ai::{TextContent, TextKind, UserContentBlock};
use kiana_chord::context::Context;
use serde_json::{Value as JsonValue, json};

use crate::env::FileContent;
use crate::harness::define::define_tool;
use crate::harness::types::{
    ToolExecuteFn, ToolExecutionApi, ToolExecutionResult, ToolRegistration,
};
use crate::tools::env::require_env;
use crate::tools::file_mutation_queue::with_file_mutation_queue;
use crate::tools::path_utils::resolve_tool_path;
use crate::types::BoxError;

/// `write` 的参数（TS: `writeSchema`）。
///
/// 两个字段都必填；`content` 是完整的新文件内容，不是补丁。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteToolInput {
    /// 文件路径（相对或绝对）。
    ///
    /// 会先经 `resolve_tool_path` 做 Unicode 空白归一化并去掉 `@` 前缀，再转成绝对路径。
    pub path: String,
    /// 要写入的内容。
    pub content: String,
}

/// 上下文是否已取消；没有取消信号（例如后台上下文）时视为未取消。
///
/// `write` 在队列槽位里调用它两次：写前一次、写后一次（见 `execute_write`）。
fn aborted(context: &dyn Context) -> bool {
    context
        .abort_signal()
        .is_some_and(|signal| signal.is_aborted())
}

/// `createWriteTool()`：注册名为 `write` 的工具。
///
/// schema 只有 `path`/`content` 两个必填字符串；`description` 与 `parameters` 被
/// `tests/tools.rs::tool_definitions_match_the_reference` 与 TS 逐字比对。返回的注册没有
/// `prepare_arguments`、`structured_output_schema`、`replay`、`output_limits`，执行体是
/// 指向 `execute_write` 的闭包，因此参数修复、结构化输出校验与输出截断都走 harness 的
/// 默认行为。
pub fn create_write_tool() -> ToolRegistration {
    let parameters = json!({
        "type": "object",
        "required": ["path", "content"],
        "properties": {
            "path": {
                "type": "string",
                "description": "Path to the file to write (relative or absolute)"
            },
            "content": {
                "type": "string",
                "description": "Content to write to the file"
            }
        }
    });
    let execute: ToolExecuteFn = Arc::new(|args, api, context| {
        Box::pin(async move { execute_write(args, api, context).await })
    });
    define_tool(ToolRegistration {
        tool: kiana_ai::Tool {
            name: "write".to_owned(),
            description: "Write content to a file. Creates the file if it doesn't exist, overwrites if it does. Automatically creates parent directories.".to_owned(),
            parameters,
            constrained_sampling: None,
        },
        replay: None,
        execution_mode: None,
        prepare_arguments: None,
        structured_output_schema: None,
        callers: None,
        deferred: false,
        output_limits: None,
        execute: Some(execute),
    })
}

/// `write` 的执行体：解析路径、进入该文件的变更队列、检查取消、写入、再检查取消。
///
/// 队列以 `env.id()` 与规范路径为 key，因此同一文件系统上同一文件的 `write` 与 `edit`
/// 不会交错；不同文件或不同文件系统不互相等待。写前检查取消可以避免白写；写后检查取消
/// 则保证：`write_file` 返回成功之后才发现取消的调用不会报成功——即使文件已经写完。
///
/// 成功时返回一个文本块 `Successfully wrote to {input.path}`（原始路径，不是绝对路径）。
/// 非取消的 `write_file` `FileError` 被装箱成 `BoxError` 向上抛，由 harness 折算成工具
/// 错误结果并以 `failed` 结算；取消错误（见 `env/node.rs` 的 `cancellable_io`）在信号已
/// 取消时走 abort 路径（以 `aborted` 结算），不生成 `tool_error`。
async fn execute_write(
    args: JsonValue,
    api: Arc<dyn ToolExecutionApi>,
    context: Arc<dyn Context>,
) -> Result<ToolExecutionResult, BoxError> {
    let input: WriteToolInput = serde_json::from_value(args)?;
    let env = require_env(api.as_ref())?;
    let absolute_path = resolve_tool_path(env.as_ref(), &input.path, context.as_ref()).await?;
    with_file_mutation_queue(
        env.as_ref(),
        &absolute_path,
        || async {
            if aborted(context.as_ref()) {
                return Err(std::io::Error::other("Operation aborted").into());
            }
            env.write_file(
                &absolute_path,
                FileContent::Text(input.content.clone()),
                context.as_ref(),
            )
            .await
            .map_err(BoxError::from)?;
            // 写入已经返回（可能已经落盘）；此时发现取消仍按错误上报，不把这次调用说成成功。
            if aborted(context.as_ref()) {
                return Err(std::io::Error::other("Operation aborted").into());
            }
            Ok(ToolExecutionResult {
                output: Some(vec![UserContentBlock::Text(TextContent {
                    kind_text: TextKind::Text,
                    text: format!("Successfully wrote to {}", input.path),
                    text_signature: None,
                })]),
                ..ToolExecutionResult::default()
            })
        },
        context.as_ref(),
    )
    .await
}
