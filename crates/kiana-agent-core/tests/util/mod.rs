//! `packages/agent/test/utils/*.ts` 的 Rust 等价物，外加 `examples/mcp-codemode/tools.ts`
//! 里**不依赖** `kiana-mcp`/`kiana-codemode` 的部分（`toToolName`、`textOf`、模型可见的 codemode
//! 工具描述，以及基于 `run_tool_call` 的 `createNestedToolRunner`）。
//!
//! 约定：每个集成测试目标自己声明 `mod util;`（`tests/util/mod.rs` 因此可能包含当前目标
//! 没用到的项，模块级 `#![allow(dead_code)]` 是共享测试工具的既定做法）。
//!
//! ## 为什么没有 `examples/mcp_codemode.rs`
//!
//! TS 示例 `examples/mcp-codemode/{main,tools}.ts` 依赖 `@earendil-works/kiana-mcp` 与
//! `@earendil-works/kiana-codemode`。`ARCHITECTURE.md` §2.2 规定 `kiana-agent-core` 唯一的出边是
//! `kiana-ai`（示例也同样不例外），所以 Rust 侧不提供可编译的该示例：`createMcpTools` /
//! `createCodemodeTool` 需要 `McpClient` 与 `CodemodeSandbox`，属于
//! `kiana-builtin-extensions`/`kiana-mcp` 的示例表面。本模块只保留 `kiana-agent-core` 自己能拥有的
//! 部分：逐字的 codemode 工具描述（`spec/agent-codemode.md` §3.7）、`toToolName`，以及
//! [`create_nested_tool_runner`]（`runToolCall` 走 agent 自己的工具管线）。

#![allow(dead_code)]
#![allow(unused_imports)]

pub mod calculate;
pub mod get_current_time;
pub mod wait_for_tick;

pub use calculate::{
    calculate, calculate_parameters, calculate_tool, create_calculate_tool_with_usage,
};
pub use get_current_time::{
    GetCurrentTimeResult, format_local_time_at, format_time_at, get_current_time,
    get_current_time_tool, iso_string, now_unix_millis,
};
pub use wait_for_tick::wait_for_tick;

use kiana_agent_core::types::{AgentToolCallOutcome, AgentToolResult, BoxFuture, JsonValue};
use kiana_ai::types::{TextContent, TextKind, UserContentBlock};

/// `{ type: "text", text }`（TS 里文本内容块的形状）。
pub fn text_block(text: impl Into<String>) -> UserContentBlock {
    UserContentBlock::Text(TextContent {
        kind_text: TextKind::Text,
        text: text.into(),
        text_signature: None,
    })
}

/// `textOf(result)`：拼接结果里的全部文本块（`join("\n")`）。
pub fn text_of(result: &AgentToolResult<JsonValue>) -> String {
    result
        .content
        .iter()
        .filter_map(|block| match block {
            UserContentBlock::Text(text) => Some(text.text.as_str()),
            UserContentBlock::Image(_) => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `toToolName(prefix, name)`：`` `${prefix}_${name}` `` 把 `[^A-Za-z0-9_-]` 换成 `_`，
/// 再按 **UTF-16 码元** `slice(0, 64)`（provider 工具名限 64 个 `[A-Za-z0-9_-]` 字符）。
pub fn to_tool_name(prefix: &str, name: &str) -> String {
    let raw = format!("{prefix}_{name}");
    let mut out = String::with_capacity(raw.len().min(64));
    for unit in raw.encode_utf16().take(64) {
        // 孤立代理项（JS 里也只是一个码元）替换成 `_`。
        let ch = char::from_u32(u32::from(unit)).unwrap_or('_');
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    out
}

/// `createNestedToolRunner(agent)` 的 Rust 形态：调用方给出 `(工具名, 参数, 取消令牌)`，
/// 得到 [`AgentToolCallOutcome`]（错误用 `Err(String)` 表达，见下）。
pub type NestedToolRunner = std::sync::Arc<
    dyn Fn(
            String,
            JsonValue,
            Option<tokio_util::sync::CancellationToken>,
        ) -> BoxFuture<'static, Result<AgentToolCallOutcome, String>>
        + Send
        + Sync,
>;

/// `createNestedToolRunner(agent)`：把嵌套工具调用送进 agent 自己的工具管线（参数校验 +
/// `beforeToolCall`/`afterToolCall` 钩子），调用 id 依次是 `nested-1`、`nested-2`…
///
/// TS 在没有 assistant 消息时同步 `throw new Error("Nested tool calls need an assistant
/// message")`，Rust 用 `Err` 携带同一句话。TS 的 `args` 是 `unknown`（`(args ?? {}) as
/// JsonObject`），这里只接受 JSON 对象或 `null`（后者等价于 `{}`），其他类型返回 `Err`。
pub fn create_nested_tool_runner(agent: &kiana_agent_core::agent::Agent) -> NestedToolRunner {
    use std::sync::atomic::{AtomicU64, Ordering};

    use kiana_agent_core::agent_loop::{RunToolCallOptions, run_tool_call};
    use kiana_agent_core::types::{AgentContext, AgentToolCall};
    use kiana_ai::types::{JsonObject, ToolCallKind};

    let agent = agent.clone();
    let next_id = std::sync::Arc::new(AtomicU64::new(1));
    std::sync::Arc::new(move |name, args, signal| {
        // TS 的 `agent.state` 是 getter：每次调用取当前快照。
        let state = agent.state();
        let Some(assistant_message) = state
            .messages
            .iter()
            .rev()
            .find_map(|message| message.as_assistant())
            .cloned()
        else {
            return Box::pin(async {
                Err("Nested tool calls need an assistant message".to_string())
            });
        };
        let arguments = match args {
            JsonValue::Object(arguments) => arguments.into_iter().collect(),
            JsonValue::Null => JsonObject::new(),
            other => {
                return Box::pin(async move {
                    Err(format!(
                        "Nested tool arguments must be an object, got {other}"
                    ))
                });
            }
        };
        let id = next_id.fetch_add(1, Ordering::SeqCst);
        let tool_call = AgentToolCall {
            kind_tool_call: ToolCallKind::ToolCall,
            id: format!("nested-{id}"),
            name,
            arguments,
            ..Default::default()
        };
        let options = RunToolCallOptions {
            tools: state.tools.clone(),
            assistant_message,
            context: AgentContext {
                messages: state.messages.clone(),
                tools: Some(state.tools),
            },
            signal,
            on_update: None,
            before_tool_call: agent.before_tool_call(),
            after_tool_call: agent.after_tool_call(),
        };
        Box::pin(async move { Ok(run_tool_call(tool_call, options).await) })
    })
}

/// `CODEMODE_TOOL_NAME = "codemode"`。
pub const CODEMODE_TOOL_NAME: &str = "codemode";

/// `createCodemodeTool` 描述的固定前 6 行（`spec/agent-codemode.md` §3.7 逐字）。
///
/// TS 源码里这是一个 `[string, string, string, string, "", "```ts", renderDeclarations(...), "```"]`
/// 的数组 `.join("\n")`；第 4 项（0 基）是空行。
pub const CODEMODE_DESCRIPTION_HEAD: [&str; 6] = [
    "Run JavaScript that calls other tools, to chain calls, run them in parallel, or filter large results.",
    "The code is the body of an async function. There is no Node, file system, network, or timers.",
    "Call tools as `await tools.<name>(args)`. Output with `text(value)` or `console.log()`, or `return` a value.",
    "Only the output and the return value reach you, not the results of the tool calls.",
    "",
    "```ts",
];

/// `createCodemodeTool` 的 `description`：固定前 6 行 + `renderDeclarations(...)` 的输出 +
/// 结尾的 ` ``` `，用 `\n` 连接（最后一行之后**没有**额外换行）。
pub fn codemode_description(declarations: &str) -> String {
    let mut lines = CODEMODE_DESCRIPTION_HEAD.to_vec();
    lines.push(declarations);
    lines.push("```");
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tool_names_are_sanitized_and_truncated() {
        assert_eq!(to_tool_name("mcp", "read_file"), "mcp_read_file");
        assert_eq!(to_tool_name("mcp", "a.b c/d"), "mcp_a_b_c_d");
        assert_eq!(to_tool_name("mcp", "ƒ(x)"), "mcp___x_");
        // 非 BMP 字符在 JS 里是两个码元 → 两个下划线（连同前缀的下划线共三个）。
        assert_eq!(to_tool_name("mcp", "😀"), "mcp___");
        let long_name = "x".repeat(100);
        let truncated = to_tool_name("mcp", &long_name);
        assert_eq!(truncated.len(), 64);
        assert!(truncated.starts_with("mcp_xxx"));
    }

    #[test]
    fn codemode_description_is_the_verbatim_eight_line_join() {
        let description =
            codemode_description("declare function echo(args: { text: string }): string;");
        assert_eq!(
            description,
            concat!(
                "Run JavaScript that calls other tools, to chain calls, run them in parallel, or filter large results.\n",
                "The code is the body of an async function. There is no Node, file system, network, or timers.\n",
                "Call tools as `await tools.<name>(args)`. Output with `text(value)` or `console.log()`, or `return` a value.\n",
                "Only the output and the return value reach you, not the results of the tool calls.\n",
                "\n",
                "```ts\n",
                "declare function echo(args: { text: string }): string;\n",
                "```"
            )
        );
        let lines: Vec<&str> = description.split('\n').collect();
        assert_eq!(lines.len(), 8);
        assert_eq!(lines[4], "");
        assert_eq!(lines[5], "```ts");
        assert_eq!(lines[7], "```");
        assert!(!description.ends_with('\n'));
    }

    #[test]
    fn codemode_description_supports_multi_line_declarations() {
        let description =
            codemode_description("declare function a(): void;\ndeclare function b(): void;");
        let lines: Vec<&str> = description.split('\n').collect();
        assert_eq!(lines.len(), 9);
        assert_eq!(lines[6], "declare function a(): void;");
        assert_eq!(lines[7], "declare function b(): void;");
        assert_eq!(lines[8], "```");
    }

    #[test]
    fn text_of_joins_text_blocks_with_newlines() {
        let result = AgentToolResult {
            content: vec![text_block("first"), text_block("second")],
            details: json!({}),
            structured_content: None,
            usage: None,
            is_error: None,
            terminate: None,
        };
        assert_eq!(text_of(&result), "first\nsecond");
    }

    // -- createNestedToolRunner ------------------------------------------------

    fn test_agent() -> kiana_agent_core::agent::Agent {
        kiana_agent_core::agent::Agent::new(kiana_agent_core::agent::AgentOptions {
            // 嵌套工具调用不会走到流函数；给一个与 e2e 相同的 `streamSimple` 形状即可。
            stream_fn: Some(std::sync::Arc::new(|model, context, options| {
                Box::pin(async move {
                    Ok(kiana_ai::compat::stream_simple(
                        &model,
                        context.as_context(),
                        options.unwrap_or_default(),
                    ))
                })
            })),
            ..Default::default()
        })
        .expect("agent construction")
    }

    fn assistant_with_text(text: &str) -> kiana_agent_core::types::AgentMessage {
        use kiana_ai::types::{
            AssistantContentBlock, AssistantMessage, AssistantRole, Message, TextContent, TextKind,
        };
        kiana_agent_core::types::AgentMessage::Standard(Message::Assistant(AssistantMessage {
            role: AssistantRole::Assistant,
            content: vec![AssistantContentBlock::Text(TextContent {
                kind_text: TextKind::Text,
                text: text.to_string(),
                text_signature: None,
            })],
            ..Default::default()
        }))
    }

    struct RecordingExecute {
        seen: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl kiana_agent_core::types::AgentToolExecute for RecordingExecute {
        fn execute<'a>(
            &'a self,
            tool_call_id: &'a str,
            _params: JsonValue,
            _signal: Option<&'a tokio_util::sync::CancellationToken>,
            _on_update: Option<kiana_agent_core::types::AgentToolUpdateCallback>,
        ) -> BoxFuture<
            'a,
            Result<AgentToolResult<JsonValue>, kiana_agent_core::types::AgentToolError>,
        > {
            Box::pin(async move {
                self.seen.lock().unwrap().push(tool_call_id.to_string());
                Ok(AgentToolResult {
                    content: vec![text_block("ok")],
                    details: JsonValue::Null,
                    structured_content: None,
                    usage: None,
                    is_error: None,
                    terminate: None,
                })
            })
        }
    }

    #[tokio::test]
    async fn nested_tool_runner_uses_the_agent_pipeline() {
        use std::sync::{Arc, Mutex};

        let agent = test_agent();
        let runner = create_nested_tool_runner(&agent);

        // 没有 assistant 消息 → TS 的 `throw new Error("Nested tool calls need an assistant message")`。
        let error = runner("echo".to_string(), json!({}), None)
            .await
            .expect_err("no assistant message");
        assert_eq!(error, "Nested tool calls need an assistant message");

        let seen = Arc::new(Mutex::new(Vec::new()));
        let tool = kiana_agent_core::types::AgentTool {
            name: "echo".to_string(),
            label: "Echo".to_string(),
            description: "Echo".to_string(),
            parameters: json!({"type": "object"}),
            constrained_sampling: None,
            output_schema: None,
            prepare_arguments: None,
            execution_mode: None,
            replay: None,
            execute: Arc::new(RecordingExecute {
                seen: Arc::clone(&seen),
            }),
        };
        agent.set_tools(vec![tool]);
        agent.set_messages(vec![assistant_with_text("hi")]);

        let first = runner("echo".to_string(), json!({"text": "a"}), None)
            .await
            .expect("first call");
        let second = runner("echo".to_string(), json!({"text": "b"}), None)
            .await
            .expect("second call");
        assert_eq!(first.tool_call.id, "nested-1");
        assert_eq!(second.tool_call.id, "nested-2");
        assert!(!first.is_error && !second.is_error);
        assert_eq!(
            *seen.lock().unwrap(),
            vec!["nested-1".to_string(), "nested-2".to_string()]
        );

        // 未知工具：`runToolCall` 以 `isError` 的结果返回（永不 panic）。
        let unknown = runner("nope".to_string(), JsonValue::Null, None)
            .await
            .expect("outcome");
        assert!(unknown.is_error);
        assert_eq!(unknown.tool_call.id, "nested-3");
    }
}
