//! `bash` 工具的 `background=true` 适配层。
//!
//! 工具层（`kiana-tools`）不能依赖 `AgentSession`，而 `background` 需要会话级的
//! [`BackgroundTaskManager`](super::BackgroundTaskManager)。本模块在会话注册内建工具时
//! 包一层 [`AgentToolExecute`]：
//!
//! - `background` 不是 `true`：原样调用底层 bash 工具，行为完全不变；
//! - `background: true` 且 continuity 启用：用同一个工具参数启动后台任务，立即把 task id
//!   返回给模型；任务在 manager 拥有的 tokio task 里继续，父 turn 结束不取消；
//! - `background: true` 但 continuity 关闭：返回可操作错误，绝不静默改成前台执行。
//!
//! 后台执行的输出走 [`BackgroundTaskContext::output`]（硬上限 + `O_NOFOLLOW`），最终
//! 文本由 manager 的 `finish` 写入；terminal 通知由 durable journal 投递回父会话
//! （见 `agent_session::take_background_notification_messages`）。

use std::sync::Arc;

use kiana_agent_core::types::{
    AgentTool, AgentToolError, AgentToolExecute, AgentToolResult, AgentToolUpdateCallback,
    BoxFuture,
};
use kiana_ai::types::JsonValue;
use tokio_util::sync::CancellationToken;

use super::{
    BackgroundTaskContext, BackgroundTaskManager, BackgroundTaskSpec, TaskOutcome, TaskUsage,
};

/// 用后台执行能力包装内建 `bash` 工具。
///
/// `manager` 为 `None`（continuity/background 关闭）时仍包装：`background: true` 会得到
/// 明确错误，而不是退化为前台执行。返回值保留原工具的所有元数据，只替换 `execute`。
pub fn wrap_bash_background(
    tool: AgentTool,
    manager: Option<Arc<BackgroundTaskManager>>,
    parent_session_id: String,
) -> AgentTool {
    let inner = Arc::clone(&tool.execute);
    // 只有 continuity 开启时才把 `background` 加进工具声明：基础 bash schema 必须与 TS
    // 黄金 fixture 逐字节一致（`kiana-tools/tests/tool_definitions_golden.rs`），后台
    // 能力是会话层的可选扩展。
    let parameters = if manager.is_some() {
        with_background_parameter(tool.parameters.clone())
    } else {
        tool.parameters.clone()
    };
    AgentTool {
        execute: Arc::new(BashBackgroundExecute {
            inner,
            manager,
            parent_session_id,
        }),
        parameters,
        ..tool
    }
}

/// 在 bash 的 JSON Schema 里追加可选 `background` 属性（保持其余字段原样）。
fn with_background_parameter(mut parameters: JsonValue) -> JsonValue {
    let Some(object) = parameters.as_object_mut() else {
        return parameters;
    };
    let Some(properties) = object
        .get_mut("properties")
        .and_then(JsonValue::as_object_mut)
    else {
        return parameters;
    };
    properties.insert(
        "background".to_string(),
        serde_json::json!({
            "type": "boolean",
            "description": "Run in the background and return a task id immediately; terminal output is delivered to the parent session (requires continuity)"
        }),
    );
    parameters
}

struct BashBackgroundExecute {
    inner: Arc<dyn AgentToolExecute>,
    manager: Option<Arc<BackgroundTaskManager>>,
    parent_session_id: String,
}

impl AgentToolExecute for BashBackgroundExecute {
    fn execute<'a>(
        &'a self,
        tool_call_id: &'a str,
        params: JsonValue,
        signal: Option<&'a CancellationToken>,
        on_update: Option<AgentToolUpdateCallback>,
    ) -> BoxFuture<'a, Result<AgentToolResult<JsonValue>, AgentToolError>> {
        let background = params
            .get("background")
            .and_then(JsonValue::as_bool)
            .unwrap_or(false);
        if !background {
            return self.inner.execute(tool_call_id, params, signal, on_update);
        }
        Box::pin(async move {
            let Some(manager) = self.manager.clone() else {
                return Err(AgentToolError {
                    message: "background execution requires continuity (set the `continuity` setting or KIANA_CONTINUITY=1)".to_string(),
                    name: None,
                    stack: None,
                });
            };
            // 底层 bash 不识别 `background`；剥掉它避免进入 spawn 环境或参数校验。
            let mut params = params;
            if let Some(object) = params.as_object_mut() {
                object.remove("background");
            }
            let command = params
                .get("command")
                .and_then(JsonValue::as_str)
                .unwrap_or_default()
                .to_string();
            let description = command
                .lines()
                .next()
                .unwrap_or_default()
                .trim()
                .to_string();
            let spec = BackgroundTaskSpec::new(
                self.parent_session_id.clone(),
                format!("tool:{tool_call_id}"),
                "bash",
            )
            .with_description(description);

            let executor = Arc::clone(&self.inner);
            let call_id = tool_call_id.to_string();
            let initial_update = on_update.clone();
            // 任务局部 cwd 覆盖不会跨 `tokio::spawn` 传播：在这里（仍处在子代理的
            // 覆盖作用域内）取一次，后台任务里重新包起来。否则隔离子代理的后台命令
            // 会退回父仓库 cwd，改到真实工作区。
            let cwd_override = kiana_extensions::current_cwd_override();
            // 父 turn 结束不取消后台任务：原工具调用的 signal 不能当作任务生命周期；
            // 后台任务只由显式 `cancel_background_task` 取消。
            let _ = signal;
            let handle = manager
                .start(spec, move |context| async move {
                    let work =
                        run_background_bash(executor, call_id, params, context, initial_update);
                    match cwd_override {
                        Some(cwd) => kiana_extensions::with_cwd_override(cwd, work).await,
                        None => work.await,
                    }
                })
                .await
                .map_err(|error| AgentToolError {
                    message: format!("cannot start background task: {error}"),
                    name: None,
                    stack: None,
                })?;
            let output_path = handle.output_path.to_string_lossy().into_owned();
            Ok(AgentToolResult {
                content: vec![kiana_ai::types::UserContentBlock::Text(
                    kiana_ai::types::TextContent {
                        kind_text: kiana_ai::types::TextKind::Text,
                        text: format!(
                            "Background task started: {}\nOutput file: {output_path}\nThe task continues after this turn; its terminal status is delivered to this session.",
                            handle.task_id
                        ),
                        text_signature: None,
                    },
                )],
                details: serde_json::json!({
                    "taskId": handle.task_id,
                    "outputPath": output_path,
                }),
                structured_content: None,
                usage: None,
                is_error: None,
                terminate: None,
            })
        })
    }
}

async fn run_background_bash(
    executor: Arc<dyn AgentToolExecute>,
    tool_call_id: String,
    params: JsonValue,
    context: BackgroundTaskContext,
    on_update: Option<AgentToolUpdateCallback>,
) -> TaskOutcome {
    // 工具更新是**累积快照**（输出超过上限后是滚动保留的尾部），只把相对上一份快照
    // 新增的尾巴写进输出文件，避免重复或错位。
    let streamed = Arc::new(std::sync::Mutex::new(String::new()));
    let output = context.output.clone();
    let progress: AgentToolUpdateCallback = {
        let streamed = Arc::clone(&streamed);
        let output = output.clone();
        Arc::new(move |result: AgentToolResult<JsonValue>| {
            let text = crate::continuity::tool_result_text(&result.content);
            let mut previous = streamed
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            stream_snapshot(&output, &mut previous, &text);
        })
    };
    // 把宿主的 on_update 也接上（如果存在），两者都收到同样的部分结果。
    let combined: AgentToolUpdateCallback = {
        let host = on_update;
        let progress = Arc::clone(&progress);
        Arc::new(move |result: AgentToolResult<JsonValue>| {
            progress(result.clone());
            if let Some(host) = &host {
                host(result);
            }
        })
    };
    // 后台任务的生命周期只由显式取消驱动（父 turn 结束不取消），因此把 manager 的
    // cancel token 交给底层工具：`cancel_background_task` 才能真正终止子进程。
    let signal = context.cancel.clone();
    let result = executor
        .execute(&tool_call_id, params, Some(&signal), Some(combined))
        .await;
    // 后台任务没有逐工具执行记录（receipts 只有 hash）；文件类 criterion 仍可判定，
    // 依赖输出内容/tests_passed 的 criterion 会明确成为 `Unverified`，不会被静默通过。
    let acceptance = crate::acceptance::evaluate_acceptance(
        &context.acceptance_criteria,
        &crate::acceptance::AcceptanceContext::new(&context.workdir, &[]),
    );
    let acceptance = Some(acceptance);
    match result {
        Ok(result) => {
            let text = crate::continuity::tool_result_text(&result.content);
            let usage = result.usage.as_ref().map(|usage| TaskUsage {
                input_tokens: usage.input,
                output_tokens: usage.output,
                total_tokens: usage.total_tokens,
            });
            // 流式更新已经把大部分文本写进输出文件；这里用同一套重叠逻辑补齐尚未流出
            // 的尾部，避免同一段结果在文件里出现两次。
            {
                let mut previous = streamed
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                stream_snapshot(&output, &mut previous, &text);
            }
            if result.is_error == Some(true) {
                // 非零退出时 `content` 是完整输出：不要再把它塞进 `terminal.reason`
                // （journal 只需要一句可读摘要）。退出码优先取 structuredContent。
                let exit_code = result
                    .structured_content
                    .as_ref()
                    .and_then(|value| value.get("exit_code"))
                    .and_then(JsonValue::as_i64)
                    .map(|code| code as i32);
                let reason = match exit_code {
                    Some(code) => format!("Command exited with code {code}"),
                    None => "background command failed".to_string(),
                };
                return TaskOutcome {
                    status: crate::background::journal::BackgroundTaskStatus::Failed,
                    reason: Some(reason),
                    exit_code,
                    output: String::new(),
                    usage: usage.unwrap_or_default(),
                    acceptance,
                };
            }
            TaskOutcome::completed(String::new())
                .with_usage(usage.unwrap_or_default())
                .with_acceptance(acceptance)
        }
        Err(error) => TaskOutcome::failed(error.message).with_acceptance(acceptance),
    }
}

/// 输出文件只允许追加；`text` 是工具的累积快照，`previous` 是上次写入的快照。
///
/// 快照通常是上一份的扩展，直接追加后缀即可。输出超过上限后 bash 会做**尾部截断**
/// （丢掉头部），此时两份快照不构成前缀关系；按“旧快照后缀 == 新快照前缀”的最大
/// 重叠来找真正新增的尾巴。找不到重叠（例如工具整段重写结果）就整段追加：宁可重复，
/// 也不能丢输出或按错误字节偏移切片。
fn stream_snapshot(output: &super::TaskOutputHandle, previous: &mut String, text: &str) {
    stream_snapshot_with(
        |chunk| {
            let _ = output.append(chunk);
        },
        previous,
        text,
    );
}

/// [`stream_snapshot`] 的追加回调版本（便于单测不依赖输出句柄）。
fn stream_snapshot_with(mut append: impl FnMut(&str), previous: &mut String, text: &str) {
    if text.is_empty() {
        return;
    }
    if text.starts_with(previous.as_str()) {
        let delta = &text[previous.len()..];
        if !delta.is_empty() {
            append(delta);
        }
        previous.clear();
        previous.push_str(text);
        return;
    }
    let overlap = suffix_prefix_overlap(previous, text);
    if overlap < text.len() {
        append(&text[overlap..]);
    }
    previous.clear();
    previous.push_str(text);
}

/// 旧快照的后缀与新快照的前缀的最长重叠字节数（KMP，O(n)；只接受 UTF-8 字符边界）。
///
/// 滚动窗口每次只丢一小段，真正的重叠通常接近整个窗口；用 KMP 求 `text` 的最长前缀
/// 同时是 `previous` 的后缀，避免逐长度回扫在 100 KiB 级窗口上退化成二次复杂度。
fn suffix_prefix_overlap(previous: &str, text: &str) -> usize {
    let text_bytes = text.as_bytes();
    let previous_bytes = previous.as_bytes();
    if text_bytes.is_empty() || previous_bytes.is_empty() {
        return 0;
    }
    // `text` 的 KMP 前缀函数。
    let mut prefix = vec![0usize; text_bytes.len()];
    let mut matched = 0usize;
    for index in 1..text_bytes.len() {
        while matched > 0 && text_bytes[index] != text_bytes[matched] {
            matched = prefix[matched - 1];
        }
        if text_bytes[index] == text_bytes[matched] {
            matched += 1;
        }
        prefix[index] = matched;
    }
    // 在 `previous` 上跑自动机；结束时 `matched` 是最长「text 前缀 == previous 后缀」。
    matched = 0;
    for &byte in previous_bytes {
        while matched > 0 && (matched == text_bytes.len() || byte != text_bytes[matched]) {
            matched = prefix[matched - 1];
        }
        if matched < text_bytes.len() && byte == text_bytes[matched] {
            matched += 1;
        }
    }
    if matched > 0
        && text.is_char_boundary(matched)
        && previous.is_char_boundary(previous.len() - matched)
    {
        matched
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stream(chunks: &[&str]) -> String {
        let mut previous = String::new();
        let mut written = String::new();
        for chunk in chunks {
            stream_snapshot_with(|text| written.push_str(text), &mut previous, chunk);
        }
        written
    }

    #[test]
    fn appends_only_the_new_tail_of_a_growing_snapshot() {
        assert_eq!(stream(&["", "a", "ab", "abc"]), "abc");
    }

    #[test]
    fn rolling_truncated_snapshots_append_only_new_output() {
        // 窗口丢掉头部后，新快照不再以上一份快照为前缀；按后缀/前缀重叠找回新增尾巴。
        assert_eq!(stream(&["abcdef", "cdefgh", "efghij"]), "abcdefghij");
    }

    #[test]
    fn rewritten_snapshots_do_not_lose_output() {
        // 完全重写且没有重叠：整段追加，宁可重复也不丢输出。
        assert_eq!(stream(&["abc", "xyz"]), "abcxyz");
    }

    #[test]
    fn overlapping_snapshots_respect_utf8_boundaries() {
        assert_eq!(stream(&["一二三四", "二三四五"]), "一二三四五");
        // 新快照完全包含在旧快照尾部：没有新增内容。
        assert_eq!(stream(&["a一二三", "二三"]), "a一二三");
    }

    #[test]
    fn longest_overlap_is_exact() {
        assert_eq!(suffix_prefix_overlap("abcabc", "abcx"), 3);
        assert_eq!(suffix_prefix_overlap("xyz", "abc"), 0);
        assert_eq!(suffix_prefix_overlap("", "abc"), 0);
        assert_eq!(suffix_prefix_overlap("abc", ""), 0);
    }
}

#[cfg(test)]
mod execution_tests {
    use super::*;
    use crate::background::journal::BackgroundTaskStatus;
    use crate::background::{BackgroundTaskContext, BackgroundTaskOutput, TaskOutputHandle};
    use kiana_agent_core::types::{AgentToolError, AgentToolResult, AgentToolUpdateCallback};
    use kiana_ai::types::{TextContent, TextKind, UserContentBlock};
    use serde_json::json;
    use std::sync::Arc;
    use std::time::Duration;

    fn text_result(text: &str) -> AgentToolResult<JsonValue> {
        AgentToolResult {
            content: vec![UserContentBlock::Text(TextContent {
                kind_text: TextKind::Text,
                text: text.to_string(),
                text_signature: None,
            })],
            details: JsonValue::Null,
            structured_content: None,
            usage: None,
            is_error: None,
            terminate: None,
        }
    }

    struct NonZeroExit;

    impl AgentToolExecute for NonZeroExit {
        fn execute<'a>(
            &'a self,
            _tool_call_id: &'a str,
            _params: JsonValue,
            _signal: Option<&'a CancellationToken>,
            on_update: Option<AgentToolUpdateCallback>,
        ) -> BoxFuture<'a, Result<AgentToolResult<JsonValue>, AgentToolError>> {
            Box::pin(async move {
                if let Some(on_update) = on_update {
                    on_update(text_result("partial\n"));
                }
                let mut result = text_result("partial\nboom\n");
                result.structured_content = Some(json!({"exit_code": 3}));
                result.is_error = Some(true);
                Ok(result)
            })
        }
    }

    struct WaitForCancel;

    impl AgentToolExecute for WaitForCancel {
        fn execute<'a>(
            &'a self,
            _tool_call_id: &'a str,
            _params: JsonValue,
            signal: Option<&'a CancellationToken>,
            _on_update: Option<AgentToolUpdateCallback>,
        ) -> BoxFuture<'a, Result<AgentToolResult<JsonValue>, AgentToolError>> {
            Box::pin(async move {
                let token = signal
                    .expect("background runs always pass a signal")
                    .clone();
                token.cancelled().await;
                Err(AgentToolError {
                    message: "aborted".to_string(),
                    name: None,
                    stack: None,
                })
            })
        }
    }

    fn context(
        dir: &std::path::Path,
        cancel: CancellationToken,
    ) -> (BackgroundTaskContext, std::path::PathBuf) {
        let path = dir.join("task.output");
        let output = BackgroundTaskOutput::open(&path, 1024 * 1024).expect("open output");
        (
            BackgroundTaskContext {
                task_id: "task".to_string(),
                cancel,
                output: TaskOutputHandle::from_output(output),
                acceptance_criteria: Vec::new(),
                workdir: dir.to_path_buf(),
            },
            path,
        )
    }

    #[tokio::test]
    async fn nonzero_exit_uses_a_short_reason_and_keeps_output_in_the_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let (context, path) = context(temp.path(), CancellationToken::new());
        let outcome = run_background_bash(
            Arc::new(NonZeroExit),
            "call".to_string(),
            JsonValue::Null,
            context,
            None,
        )
        .await;

        assert_eq!(outcome.status, BackgroundTaskStatus::Failed);
        assert_eq!(
            outcome.reason.as_deref(),
            Some("Command exited with code 3"),
            "reason must not be the whole command output"
        );
        assert_eq!(outcome.exit_code, Some(3));
        let written = std::fs::read_to_string(&path).expect("read output");
        assert!(written.contains("boom"), "{written:?}");
    }

    #[tokio::test]
    async fn cancellation_reaches_the_underlying_tool() {
        let temp = tempfile::tempdir().expect("tempdir");
        let cancel = CancellationToken::new();
        let (context, _path) = context(temp.path(), cancel.clone());
        let run = tokio::spawn(run_background_bash(
            Arc::new(WaitForCancel),
            "call".to_string(),
            JsonValue::Null,
            context,
            None,
        ));
        tokio::time::sleep(Duration::from_millis(20)).await;
        cancel.cancel();
        let outcome = tokio::time::timeout(Duration::from_secs(2), run)
            .await
            .expect("cancel must unblock the tool")
            .expect("join");
        assert_eq!(outcome.status, BackgroundTaskStatus::Failed);
        assert_eq!(outcome.reason.as_deref(), Some("aborted"));
    }
}
