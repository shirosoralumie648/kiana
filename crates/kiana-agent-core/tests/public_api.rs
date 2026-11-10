//! 公开面一致性 + 冻结契约的 JSON 形状。
//!
//! 编译期部分直接把 `packages/agent/src/index.ts` 的公开面逐项 import 一遍（`as _` 只做名字
//! 解析，不产生未使用告警）；运行期部分断言 TS `JSON.stringify` 的形状（键序、camelCase、
//! `undefined` 省略、JS 数字格式）。
//!
//! 其他单元（`agent.rs`、`agent-loop.rs`、`proxy.rs`）的公开项由它们自己的测试覆盖；
//! 这里只覆盖本单元负责的 barrel / types / identity / stream-fn。

use std::sync::Arc;

use kiana_agent_core::index::{
    AfterToolCallContext, AfterToolCallResult, AgentContext, AgentEvent, AgentLoopConfig,
    AgentMessage, AgentState, AgentTool, AgentToolCallOutcome, AgentToolError, AgentToolExecute,
    AgentToolResult, AgentToolUpdateCallback, BeforeToolCallContext, BeforeToolCallResult,
    QueueMode, StreamFn, ThinkingLevel, ToolExecutionMode, default_agent_identity,
    set_default_stream_fn,
};
use kiana_agent_core::stream_fn::{
    clear_default_stream_fn, get_default_stream_fn, require_default_stream_fn,
};

// ---------------------------------------------------------------------------
// 编译期：lib.rs 的 `pub use types::{...}` 列表
// ---------------------------------------------------------------------------

#[allow(unused_imports)]
use kiana_agent_core::{
    AfterToolCallContext as _, AfterToolCallResult as _, AgentContext as _, AgentEvent as _,
    AgentLoopConfig as _, AgentMessage as _, AgentState as _, AgentTool as _,
    AgentToolCallOutcome as _, AgentToolError as _, AgentToolExecute as _, AgentToolResult as _,
    AgentToolUpdateCallback as _, BeforeToolCallContext as _, BeforeToolCallResult as _,
    QueueMode as _, StreamFn as _, ThinkingLevel as _, ToolExecutionMode as _,
};

// ---------------------------------------------------------------------------
// 编译期：types.rs 的其余公开项（TS types.ts 的每一个导出都有对应物）
// ---------------------------------------------------------------------------

#[allow(unused_imports)]
use kiana_agent_core::types::{
    AfterToolCallFn as _, AgentAssistantMessage as _, AgentLoopTurnUpdate as _,
    AgentRequestUpdate as _, AgentTextContent as _, AgentToolCall as _, AgentTurnContext as _,
    AgentTurnDecision as _, AiThinkingLevel as _, BoxFuture as _, FinishTurnFn as _,
    GetApiKeyFn as _, GetMessagesFn as _, JsonObject as _, JsonValue as _,
    PrepareNextTurnContext as _, PrepareNextTurnFn as _, PrepareRequestContext as _,
    PrepareRequestFn as _, ReplayPolicy as _, TransformContextFn as _,
};

// `getDefaultStreamFn` 只能从 stream_fn 模块拿到（不属于包公开面），见 index.rs 的 compile_fail。
#[allow(unused_imports)]
use kiana_agent_core::stream_fn::{get_default_stream_fn as _, set_default_stream_fn as _};

// ---------------------------------------------------------------------------
// identity
// ---------------------------------------------------------------------------

#[test]
fn default_agent_identity_matches_ts() {
    // TS: `export const DEFAULT_AGENT_IDENTITY: ClientIdentity = { attribution: KIANA_ATTRIBUTION };`
    let identity = default_agent_identity();
    assert_eq!(
        identity,
        kiana_ai::types::ClientIdentity {
            version: None,
            user_agent: None,
            attribution: Some(Some(kiana_ai::types::kiana_attribution())),
        }
    );
    let json = serde_json::to_string(&identity).unwrap();
    assert_eq!(
        json,
        r#"{"attribution":{"name":"kiana","url":"https://kiana.dev","categories":["cli-agent"]}}"#
    );
}

// ---------------------------------------------------------------------------
// stream-fn：进程级单例 + 逐字错误消息
// ---------------------------------------------------------------------------

#[test]
fn stream_fn_singleton_and_verbatim_missing_default_message() {
    clear_default_stream_fn();
    assert!(get_default_stream_fn().is_none());
    assert_eq!(
        require_default_stream_fn()
            .err()
            .expect("unset default must fail")
            .to_string(),
        "No default stream function configured. Pass streamFn explicitly or call setDefaultStreamFn()."
    );

    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let calls_for_fn = Arc::clone(&calls);
    let f: StreamFn = Arc::new(move |_model, _context, _options| {
        calls_for_fn.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Box::pin(async { panic!("stream fn is not invoked by this test") })
    });
    // 通过 barrel 的再导出设置，验证 index::setDefaultStreamFn 就是同一个单例。
    set_default_stream_fn(f);
    assert!(get_default_stream_fn().is_some());
    assert!(require_default_stream_fn().is_ok());
    drop(get_default_stream_fn().unwrap());
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);

    clear_default_stream_fn();
    assert!(get_default_stream_fn().is_none());
}

// ---------------------------------------------------------------------------
// AgentMessage：标准消息 vs 自定义 role
// ---------------------------------------------------------------------------

#[test]
fn agent_message_custom_role_round_trips_with_key_order() {
    let custom: AgentMessage =
        serde_json::from_str(r#"{"role":"notification","text":"Info","timestamp":7}"#).unwrap();
    assert!(matches!(custom, AgentMessage::Custom(_)));
    assert_eq!(custom.role(), "notification");
    assert_eq!(custom.timestamp(), Some(7));
    assert!(custom.as_standard().is_none());
    // IndexMap 保持插入序：`role` 仍在第一位。
    assert_eq!(
        serde_json::to_string(&custom).unwrap(),
        r#"{"role":"notification","text":"Info","timestamp":7}"#
    );

    let user: AgentMessage =
        serde_json::from_str(r#"{"role":"user","content":"hi","timestamp":5}"#).unwrap();
    assert!(matches!(
        user,
        AgentMessage::Standard(kiana_ai::types::Message::User(_))
    ));
    assert_eq!(user.role(), "user");
    assert_eq!(user.timestamp(), Some(5));
    assert_eq!(
        serde_json::to_string(&user).unwrap(),
        r#"{"role":"user","content":"hi","timestamp":5}"#
    );
}

#[test]
fn agent_message_clone_and_partial_eq() {
    let a: AgentMessage =
        serde_json::from_str(r#"{"role":"user","content":"hi","timestamp":5}"#).unwrap();
    let b = a.clone();
    assert_eq!(a, b);
    assert_ne!(a.role(), "assistant");
    assert!(a.as_assistant().is_none());
}

// ---------------------------------------------------------------------------
// AgentEvent：camelCase 字段、省略的 durationMs、JS 数字格式
// ---------------------------------------------------------------------------

#[test]
fn agent_event_json_shapes_match_ts() {
    let start = AgentEvent::ToolExecutionStart {
        tool_call_id: "tc1".into(),
        tool_name: "bash".into(),
        args: serde_json::json!({"cmd": "ls"}),
    };
    assert_eq!(
        serde_json::to_string(&start).unwrap(),
        r#"{"type":"tool_execution_start","toolCallId":"tc1","toolName":"bash","args":{"cmd":"ls"}}"#
    );
    assert_eq!(start.kind(), "tool_execution_start");
    // 反序列化同样接受 TS 的 camelCase 字段名（事件跨进程/落盘往返）。
    let round_tripped: AgentEvent =
        serde_json::from_str(&serde_json::to_string(&start).unwrap()).unwrap();
    assert_eq!(round_tripped, start);

    let update = AgentEvent::ToolExecutionUpdate {
        tool_call_id: "tc1".into(),
        tool_name: "bash".into(),
        args: serde_json::json!({}),
        partial_result: serde_json::json!({"content": []}),
    };
    assert_eq!(
        serde_json::to_string(&update).unwrap(),
        r#"{"type":"tool_execution_update","toolCallId":"tc1","toolName":"bash","args":{},"partialResult":{"content":[]}}"#
    );

    let end_without_duration = AgentEvent::ToolExecutionEnd {
        tool_call_id: "tc1".into(),
        tool_name: "bash".into(),
        result: serde_json::json!({"ok": true}),
        is_error: false,
        duration_ms: None,
    };
    assert_eq!(
        serde_json::to_string(&end_without_duration).unwrap(),
        r#"{"type":"tool_execution_end","toolCallId":"tc1","toolName":"bash","result":{"ok":true},"isError":false}"#
    );

    // TS 的 durationMs 是 `Math.round(...)` 的整数；JSON.stringify(12) === "12"（不是 "12.0"）。
    let end_with_duration = AgentEvent::ToolExecutionEnd {
        tool_call_id: "tc1".into(),
        tool_name: "bash".into(),
        result: serde_json::json!(null),
        is_error: true,
        duration_ms: Some(12.0),
    };
    assert_eq!(
        serde_json::to_string(&end_with_duration).unwrap(),
        r#"{"type":"tool_execution_end","toolCallId":"tc1","toolName":"bash","result":null,"isError":true,"durationMs":12}"#
    );
    assert_eq!(end_with_duration.kind(), "tool_execution_end");
    let round_tripped_duration: AgentEvent =
        serde_json::from_str(&serde_json::to_string(&end_with_duration).unwrap()).unwrap();
    assert_eq!(round_tripped_duration, end_with_duration);

    let agent_end = AgentEvent::AgentEnd {
        messages: vec![
            serde_json::from_str(r#"{"role":"user","content":"hi","timestamp":5}"#).unwrap(),
        ],
    };
    assert_eq!(
        serde_json::to_string(&agent_end).unwrap(),
        r#"{"type":"agent_end","messages":[{"role":"user","content":"hi","timestamp":5}]}"#
    );

    let turn_end = AgentEvent::TurnEnd {
        message: serde_json::from_str(r#"{"role":"assistant","content":[],"api":"","provider":"","model":"","usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"stopReason":"stop","timestamp":1}"#).unwrap(),
        tool_results: vec![],
    };
    // 消息体本身由 kiana-ai 的 serde 形状决定（另有单元负责）；这里只断言本单元的事件外壳。
    let turn_end_value = serde_json::to_value(&turn_end).unwrap();
    let turn_end_keys: Vec<&str> = turn_end_value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(turn_end_keys, ["type", "message", "toolResults"]);
    assert_eq!(turn_end_value["type"], "turn_end");
    assert_eq!(turn_end_value["toolResults"], serde_json::json!([]));
    assert_eq!(turn_end_value["message"]["role"], "assistant");

    for ev in [
        AgentEvent::AgentStart,
        AgentEvent::TurnStart,
        AgentEvent::MessageEnd {
            message: serde_json::from_str(r#"{"role":"user","content":"hi","timestamp":5}"#)
                .unwrap(),
        },
    ] {
        let value = serde_json::to_value(&ev).unwrap();
        assert_eq!(value["type"], ev.kind());
    }
}

#[test]
fn agent_event_message_update_field_order() {
    let event = AgentEvent::MessageUpdate {
        message: serde_json::from_str(r#"{"role":"user","content":"hi","timestamp":5}"#).unwrap(),
        assistant_message_event: kiana_ai::types::AssistantMessageEvent::Start {
            partial: Box::new(kiana_ai::types::AssistantMessage::default()),
        },
    };
    let value = serde_json::to_value(&event).unwrap();
    let keys: Vec<&str> = value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    // TS 运行期构造 `{ type, assistantMessageEvent, message }`（agent-loop.ts:445-449），
    // `JSON.stringify` 保持该键序。
    assert_eq!(keys, ["type", "assistantMessageEvent", "message"]);
    assert_eq!(event.kind(), "message_update");
}

// ---------------------------------------------------------------------------
// AgentToolResult / AgentToolCallOutcome / hooks
// ---------------------------------------------------------------------------

fn text_content(text: &str) -> kiana_ai::types::UserContentBlock {
    kiana_ai::types::UserContentBlock::Text(kiana_ai::types::TextContent {
        kind_text: Default::default(),
        text: text.into(),
        text_signature: None,
    })
}

#[test]
fn agent_tool_result_json_shape() {
    let result = AgentToolResult::<serde_json::Value> {
        content: vec![text_content("done")],
        details: serde_json::json!({"path": "a"}),
        structured_content: None,
        usage: None,
        is_error: Some(true),
        terminate: None,
    };
    assert_eq!(
        serde_json::to_string(&result).unwrap(),
        r#"{"content":[{"type":"text","text":"done"}],"details":{"path":"a"},"isError":true}"#
    );

    let full = AgentToolResult::<serde_json::Value> {
        content: vec![],
        details: serde_json::json!({}),
        structured_content: Some(serde_json::json!({"ok": true})),
        usage: None,
        is_error: None,
        terminate: Some(true),
    };
    assert_eq!(
        serde_json::to_string(&full).unwrap(),
        r#"{"content":[],"details":{},"structuredContent":{"ok":true},"terminate":true}"#
    );
}

#[test]
fn agent_tool_result_null_details_is_omitted_on_output() {
    // TS: `structuredContent?: JsonValue` 里 `null` 与 `undefined` 是两种形状，`null` 照常写出。
    // `details` 的 `null` 在 Rust 侧表示 TS 的 `undefined`（见 `json_value_is_null`），写出时省略。
    let result: AgentToolResult =
        serde_json::from_str(r#"{"content":[],"details":null,"structuredContent":null}"#).unwrap();
    assert_eq!(result.details, serde_json::Value::Null);
    assert_eq!(result.structured_content, Some(serde_json::Value::Null));
    assert_eq!(
        serde_json::to_string(&result).unwrap(),
        r#"{"content":[],"structuredContent":null}"#
    );
}

#[test]
fn agent_tool_call_outcome_json_shape() {
    let outcome = AgentToolCallOutcome {
        tool_call: kiana_ai::types::ToolCall {
            id: "tc1".into(),
            name: "bash".into(),
            arguments: Default::default(),
            ..Default::default()
        },
        result: AgentToolResult::<serde_json::Value> {
            content: vec![],
            details: serde_json::json!({}),
            structured_content: None,
            usage: None,
            is_error: None,
            terminate: None,
        },
        is_error: false,
        duration_ms: Some(12.0),
    };
    assert_eq!(
        serde_json::to_string(&outcome).unwrap(),
        r#"{"toolCall":{"type":"toolCall","id":"tc1","name":"bash","arguments":{}},"result":{"content":[],"details":{}},"isError":false,"durationMs":12}"#
    );
}

#[test]
fn hook_result_json_shapes() {
    assert_eq!(
        serde_json::to_string(&BeforeToolCallResult::default()).unwrap(),
        "{}"
    );
    let blocked = BeforeToolCallResult {
        block: Some(true),
        reason: Some("bash is disabled".into()),
        terminate: None,
    };
    assert_eq!(
        serde_json::to_string(&blocked).unwrap(),
        r#"{"block":true,"reason":"bash is disabled"}"#
    );

    let after = AfterToolCallResult {
        content: Some(vec![text_content("override")]),
        details: Some(serde_json::json!({"audited": true})),
        structured_content: None,
        is_error: None,
        usage: None,
        terminate: Some(true),
    };
    assert_eq!(
        serde_json::to_string(&after).unwrap(),
        r#"{"content":[{"type":"text","text":"override"}],"details":{"audited":true},"terminate":true}"#
    );
}

// ---------------------------------------------------------------------------
// 枚举字面量
// ---------------------------------------------------------------------------

#[test]
fn string_literal_unions_serialize_verbatim() {
    assert_eq!(
        serde_json::to_string(&ToolExecutionMode::Sequential).unwrap(),
        "\"sequential\""
    );
    assert_eq!(
        serde_json::to_string(&ToolExecutionMode::Parallel).unwrap(),
        "\"parallel\""
    );
    assert_eq!(serde_json::to_string(&QueueMode::All).unwrap(), "\"all\"");
    assert_eq!(
        serde_json::to_string(&QueueMode::OneAtATime).unwrap(),
        "\"one-at-a-time\""
    );
    assert_eq!(
        serde_json::to_string(&ThinkingLevel::Off).unwrap(),
        "\"off\""
    );
    assert_eq!(
        serde_json::to_string(&ThinkingLevel::Xhigh).unwrap(),
        "\"xhigh\""
    );
    assert_eq!(
        serde_json::to_string(&ThinkingLevel::Max).unwrap(),
        "\"max\""
    );
    assert_eq!(
        serde_json::to_string(&kiana_agent_core::types::ReplayPolicy::Never).unwrap(),
        "\"never\""
    );
    assert_eq!(
        serde_json::to_string(&kiana_agent_core::types::ReplayPolicy::Safe).unwrap(),
        "\"safe\""
    );
    assert_eq!(
        serde_json::to_string(&kiana_agent_core::types::AgentTurnDecision::Continue).unwrap(),
        r#"{"action":"continue"}"#
    );
    assert_eq!(
        serde_json::to_string(&kiana_agent_core::types::AgentTurnDecision::End).unwrap(),
        r#"{"action":"end"}"#
    );
}

// ---------------------------------------------------------------------------
// AgentState / AgentLoopConfig 默认值
// ---------------------------------------------------------------------------

#[test]
fn agent_state_defaults_match_ts_initial_state() {
    let state = AgentState::default();
    assert_eq!(state.system_prompt, "");
    assert!(state.model.is_none());
    assert_eq!(state.thinking_level, ThinkingLevel::Off);
    assert!(state.tools.is_empty());
    assert!(state.messages.is_empty());
    assert!(!state.is_streaming);
    assert!(state.streaming_message.is_none());
    assert!(state.pending_tool_calls.is_empty());
    assert!(state.error_message.is_none());
}

#[test]
fn default_convert_to_llm_keeps_standard_messages_only() {
    let config = AgentLoopConfig::default();
    let messages = vec![
        AgentMessage::Standard(kiana_ai::types::Message::User(
            kiana_ai::types::UserMessage {
                role: Default::default(),
                content: kiana_ai::types::UserContent::Text("hi".into()),
                timestamp: 1,
            },
        )),
        serde_json::from_str(r#"{"role":"notification","text":"Info","timestamp":7}"#).unwrap(),
    ];
    let converted = (config.convert_to_llm)(messages);
    assert_eq!(converted.len(), 1);
    assert_eq!(converted[0].role(), "user");
}

// ---------------------------------------------------------------------------
// AgentTool / AgentToolExecute 形状（编译期 + 运行期）
// ---------------------------------------------------------------------------

struct EchoTool;

impl AgentToolExecute for EchoTool {
    fn execute<'a>(
        &'a self,
        tool_call_id: &'a str,
        params: kiana_agent_core::types::JsonValue,
        _signal: Option<&'a tokio_util::sync::CancellationToken>,
        _on_update: Option<AgentToolUpdateCallback>,
    ) -> kiana_agent_core::types::BoxFuture<
        'a,
        Result<AgentToolResult<kiana_agent_core::types::JsonValue>, AgentToolError>,
    > {
        let id = tool_call_id.to_string();
        Box::pin(async move {
            Ok(AgentToolResult {
                content: vec![text_content(&id)],
                details: params,
                structured_content: None,
                usage: None,
                is_error: None,
                terminate: None,
            })
        })
    }
}

#[tokio::test]
async fn agent_tool_execute_signature_is_usable() {
    let tool = AgentTool {
        name: "echo".into(),
        label: "Echo".into(),
        description: "echo tool".into(),
        parameters: serde_json::json!({"type": "object"}),
        constrained_sampling: None,
        output_schema: None,
        prepare_arguments: None,
        execution_mode: Some(ToolExecutionMode::Sequential),
        replay: Some(kiana_agent_core::types::ReplayPolicy::Safe),
        execute: Arc::new(EchoTool),
    };
    let outcome = tool
        .execute
        .execute("tc9", serde_json::json!({"x": 1}), None, None)
        .await
        .unwrap();
    assert_eq!(outcome.details, serde_json::json!({"x": 1}));
    assert_eq!(outcome.content.len(), 1);
    assert_eq!(tool.execution_mode, Some(ToolExecutionMode::Sequential));
    assert_eq!(
        tool.replay,
        Some(kiana_agent_core::types::ReplayPolicy::Safe)
    );
    // `Debug` 只输出可打印字段，不泄漏回调。
    let debug = format!("{tool:?}");
    assert!(debug.contains("Echo"));
}

#[test]
fn context_and_state_are_cloneable() {
    let context = AgentContext {
        messages: vec![],
        tools: None,
    };
    let _cloned = context.clone();
    let _ = AgentContext::default();
    let _ = AgentState::default();
    // 钩子上下文类型可构造（编译期检查字段集合）。
    fn _accepts(_: BeforeToolCallContext, _: AfterToolCallContext) {}
}

// ---------------------------------------------------------------------------
// 回归：TS 的 `details: undefined`（must deserialize）
//
// TS 的 `AgentToolResult<T = JsonValue | undefined>` 里 `details` 可以缺省
// （`CalculateResult`、codemode/MCP 工具都返回 `details: undefined`，spec §2.1.7），
// `JSON.stringify` 会省略该键。Rust 的泛型 `T` 无法表达 undefined，约定用
// `T::default()`（`JsonValue` 即 `null`、`Option<JsonValue>` 即 `None`）；
// 这里锁住“TS 写出的会话文件必须能读回”这一半契约。
// ---------------------------------------------------------------------------

#[test]
fn agent_tool_result_without_details_deserializes() {
    // spec §2.1.7 `calculate()` 的序列化结果。
    let result: AgentToolResult =
        serde_json::from_str(r#"{"content":[{"type":"text","text":"1 + 1 = 2"}]}"#)
            .expect("TS tool result without details must deserialize");
    assert_eq!(result.details, serde_json::Value::Null);
    assert_eq!(result.structured_content, None);
    assert_eq!(result.usage, None);
    assert_eq!(result.is_error, None);
    assert_eq!(result.terminate, None);

    // `T = Option<JsonValue>` 时缺省读成 `None`（与 TS 的 undefined 一一对应）。
    let optional: AgentToolResult<Option<serde_json::Value>> =
        serde_json::from_str(r#"{"content":[]}"#).unwrap();
    assert_eq!(optional.details, None);

    // 显式 `null`（TS 里与 undefined 不同）仍保留为 Null / Some(Null)。
    let explicit: AgentToolResult =
        serde_json::from_str(r#"{"content":[],"details":null}"#).unwrap();
    assert_eq!(explicit.details, serde_json::Value::Null);
}

#[test]
fn agent_tool_result_omits_absent_details_like_json_stringify() {
    // TS 的 `details: undefined` 在 `JSON.stringify` 里**省略**该键（`calculate()` 的结果就是
    // `{"content":[{"type":"text","text":"1 + 1 = 2"}]}`，spec §2.1.7）；`bash`/`grep`/`ls`/
    // `find`/`write` 等工具也都返回 `details: undefined`。
    // Rust 的冻结契约 `details: T` 无法表达“键不存在”，约定 `T::default()`（`JsonValue::Null`、
    // `Option::None`）表示缺省，写出时同样省略。
    let result = AgentToolResult::<serde_json::Value> {
        content: vec![],
        details: serde_json::Value::Null,
        structured_content: None,
        usage: None,
        is_error: None,
        terminate: None,
    };
    assert_eq!(serde_json::to_string(&result).unwrap(), r#"{"content":[]}"#);

    // 非缺省的 details 照常写出（`createErrorToolResult` 用 `details: {}`）。
    let error_result = AgentToolResult::<serde_json::Value> {
        content: vec![],
        details: serde_json::json!({}),
        structured_content: None,
        usage: None,
        is_error: None,
        terminate: None,
    };
    assert_eq!(
        serde_json::to_string(&error_result).unwrap(),
        r#"{"content":[],"details":{}}"#
    );

    // agent loop 构造 `tool_execution_end.result` 走的 `to_value` 边界同样省略该键。
    let value = serde_json::to_value(&result).unwrap();
    assert_eq!(value, serde_json::json!({"content": []}));

    // `T = Option<JsonValue>` 时 `None` 是缺省（省略）。
    let optional = AgentToolResult::<Option<serde_json::Value>> {
        content: vec![],
        details: None,
        structured_content: None,
        usage: None,
        is_error: None,
        terminate: None,
    };
    assert_eq!(
        serde_json::to_string(&optional).unwrap(),
        r#"{"content":[]}"#
    );
}

// ---------------------------------------------------------------------------
// 回归：afterToolCall 的 `??` 语义（null 与 undefined 等价）
// ---------------------------------------------------------------------------

#[test]
fn after_tool_call_result_treats_json_null_as_absent() {
    // TS `finalizeExecutedToolCall`：`afterResult.details ?? result.details`（其余字段同）。
    let result: AfterToolCallResult = serde_json::from_str(
        r#"{"content":null,"details":null,"structuredContent":null,"isError":null,"usage":null,"terminate":null}"#,
    )
    .expect("null fields must deserialize as absent");
    assert_eq!(result, AfterToolCallResult::default());
    assert_eq!(result.details, None);
    assert_eq!(result.structured_content, None);

    // 显式给值仍保留。
    let provided: AfterToolCallResult =
        serde_json::from_str(r#"{"details":{"audited":true},"structuredContent":42}"#).unwrap();
    assert_eq!(provided.details, Some(serde_json::json!({"audited": true})));
    assert_eq!(provided.structured_content, Some(serde_json::json!(42)));
}

// ---------------------------------------------------------------------------
// 回归：JsonValue 载荷按 JS `JSON.stringify` 语义写出（ARCH §4.3 第 3、4 条）
//
// golden 由 node 生成：
//   JSON.stringify({role:"notification",a:1.0,b:-0.0,big:9007199254740993})
//   === '{"role":"notification","a":1,"b":0,"big":9007199254740992}'
// ---------------------------------------------------------------------------

#[test]
fn custom_message_serializes_with_json_stringify_semantics() {
    let custom: AgentMessage =
        serde_json::from_str(r#"{"role":"notification","a":1.0,"b":-0.0,"big":9007199254740993}"#)
            .unwrap();
    assert!(matches!(custom, AgentMessage::Custom(_)));
    assert_eq!(
        serde_json::to_string(&custom).unwrap(),
        r#"{"role":"notification","a":1,"b":0,"big":9007199254740992}"#
    );

    // 整数索引键（无前导零、< 2^32-1）按 JS 规则升序排最前，其余保持插入序。
    // node: JSON.stringify({role:"notification","2":"two","1":"one",x:"x"})
    //   === '{"1":"one","2":"two","role":"notification","x":"x"}'
    let keys: AgentMessage =
        serde_json::from_str(r#"{"role":"notification","2":"two","1":"one","x":"x"}"#).unwrap();
    assert_eq!(
        serde_json::to_string(&keys).unwrap(),
        r#"{"1":"one","2":"two","role":"notification","x":"x"}"#
    );

    // JS 数字格式化：1e21 → "1e+21"、1e-7 → "1e-7"、0.000001 → "0.000001"。
    // node: JSON.stringify({role:"n",a:1e21,b:1e-7,c:0.000001})
    //   === '{"role":"n","a":1e+21,"b":1e-7,"c":0.000001}'
    let numbers: AgentMessage =
        serde_json::from_str(r#"{"role":"n","a":1e21,"b":1e-7,"c":0.000001}"#).unwrap();
    assert_eq!(
        serde_json::to_string(&numbers).unwrap(),
        r#"{"role":"n","a":1e+21,"b":1e-7,"c":0.000001}"#
    );

    // `to_value` 路径同样可用：RawValue 被解析回数字类型（不是字符串）。
    let value = serde_json::to_value(&custom).unwrap();
    assert_eq!(value["role"], "notification");
    assert!(value["a"].is_u64());
    assert!(value["b"].is_u64());
    assert_eq!(value["big"], serde_json::json!(9007199254740992u64));
}

#[test]
fn event_and_tool_result_json_payloads_serialize_like_json_stringify() {
    let update = AgentEvent::ToolExecutionUpdate {
        tool_call_id: "tc1".into(),
        tool_name: "calc".into(),
        args: serde_json::json!({"n": 1.0, "negZero": -0.0}),
        partial_result: serde_json::json!({"big": 9007199254740993u64}),
    };
    assert_eq!(
        serde_json::to_string(&update).unwrap(),
        r#"{"type":"tool_execution_update","toolCallId":"tc1","toolName":"calc","args":{"n":1,"negZero":0},"partialResult":{"big":9007199254740992}}"#
    );

    let result = AgentToolResult::<serde_json::Value> {
        content: vec![],
        details: serde_json::json!({"ratio": 1.0, "negZero": -0.0}),
        structured_content: Some(serde_json::json!({"n": 1.0})),
        usage: None,
        is_error: None,
        terminate: None,
    };
    assert_eq!(
        serde_json::to_string(&result).unwrap(),
        r#"{"content":[],"details":{"ratio":1,"negZero":0},"structuredContent":{"n":1}}"#
    );

    // 事件里的 `result` 对象也走 JS 语义（包括显式 null 的 details 保留）。
    let end = AgentEvent::ToolExecutionEnd {
        tool_call_id: "tc1".into(),
        tool_name: "calc".into(),
        result: serde_json::json!({"content": [], "details": null, "n": 1.0}),
        is_error: false,
        duration_ms: None,
    };
    assert_eq!(
        serde_json::to_string(&end).unwrap(),
        r#"{"type":"tool_execution_end","toolCallId":"tc1","toolName":"calc","result":{"content":[],"details":null,"n":1},"isError":false}"#
    );
}

// ---------------------------------------------------------------------------
// 回归（reviewer：major）：标准消息 / usage 的数字格式必须与 `JSON.stringify` 语义一致
//
// TS 的 `AgentMessage` 整体走 `JSON.stringify`（会话文件逐字回放），因此 `usage.cost`
// 这类 f64 字段写出的是 `0`、`0.000005`、`5e-7`，而 serde/ryu 会写 `0.0`、`5e-6`。
// golden 由 node 26 生成（`JSON.stringify(message)`，逐行一条）：
//   crates/kiana-agent-core/tests/fixtures/agent_message_js_stringify.jsonl
// ---------------------------------------------------------------------------

#[test]
fn agent_message_standard_serializes_like_json_stringify() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/agent_message_js_stringify.jsonl"
    );
    let golden = std::fs::read_to_string(path).expect("golden fixture must exist");
    let mut lines = 0;
    for (i, line) in golden.lines().enumerate() {
        if line.is_empty() {
            continue;
        }
        lines += 1;
        let message: AgentMessage = serde_json::from_str(line)
            .unwrap_or_else(|err| panic!("golden #{i} must deserialize: {err}"));
        assert_eq!(
            serde_json::to_string(&message).unwrap(),
            line,
            "golden #{i} must re-serialize byte-identically"
        );
    }
    assert_eq!(lines, 5, "fixture should cover 5 message shapes");

    // 最后一条 `bashExecution` 没有标准角色，走 `Custom` 分支。
    let custom: AgentMessage = serde_json::from_str(golden.lines().last().unwrap()).unwrap();
    assert!(matches!(custom, AgentMessage::Custom(_)));
}

#[test]
fn agent_message_standard_deserializes_js_decimals_like_json_parse() {
    // `serde_json` 默认的浮点解析不是正确舍入（差 1 ULP），会把这些 17 位小数变成
    // 0.00024375 / 0.0254715；`float_roundtrip` 打开后位模式与 `JSON.parse` 一致（会话文件互通所需）。
    // node: JSON.parse("0.00024375000000000002") -> 0x3f2ff2e48e8a71df
    //       JSON.parse("0.025471499999999998")  -> 0x3f9a15336deb95e5
    let line = r#"{"role":"assistant","content":[],"api":"anthropic-messages","provider":"anthropic","model":"m","usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0.00024375000000000002,"output":0.025471499999999998,"cacheRead":0,"cacheWrite":0,"total":0.00024375000000000002}},"stopReason":"stop","timestamp":1}"#;
    let message: AgentMessage = serde_json::from_str(line).unwrap();
    let assistant = message.as_assistant().expect("standard assistant message");
    assert_eq!(assistant.usage.cost.input.to_bits(), 0x3f2f_f2e4_8e8a_71df);
    assert_eq!(assistant.usage.cost.output.to_bits(), 0x3f9a_1533_6deb_95e5);
    // 重新写出仍是完整精度（JS 的 `Number.prototype.toString` 最短往返表示）。
    assert_eq!(serde_json::to_string(&message).unwrap(), line);
}

fn zero_usage() -> kiana_ai::types::Usage {
    kiana_ai::types::Usage {
        input: 0,
        output: 0,
        cache_read: 0,
        cache_write: 0,
        cache_write1h: None,
        reasoning: None,
        total_tokens: 0,
        cost: kiana_ai::types::UsageCost {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
            total: 0.0,
            total_from_provider: None,
        },
    }
}

#[test]
fn agent_tool_result_usage_serializes_with_js_numbers() {
    // TS: `JSON.stringify({content:[],details:{},usage})` 的 cost 全零写 `0`。
    let result = AgentToolResult::<serde_json::Value> {
        content: vec![],
        details: serde_json::json!({}),
        structured_content: None,
        usage: Some(zero_usage()),
        is_error: None,
        terminate: None,
    };
    assert_eq!(
        serde_json::to_string(&result).unwrap(),
        r#"{"content":[],"details":{},"usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}}}"#
    );

    // 非零小数同样按 JS 规则（`0.000005` 而不是 `5e-6`）。
    let mut usage = zero_usage();
    usage.cost.input = 0.000005;
    let tiny = AgentToolResult::<serde_json::Value> {
        content: vec![],
        details: serde_json::json!({}),
        structured_content: None,
        usage: Some(usage),
        is_error: None,
        terminate: None,
    };
    assert!(
        serde_json::to_string(&tiny)
            .unwrap()
            .contains(r#""cost":{"input":0.000005,"#),
        "cost must use JS number formatting"
    );
}

#[test]
fn agent_event_turn_end_tool_results_use_js_numbers() {
    let tool_result: kiana_ai::types::ToolResultMessage = serde_json::from_str(
        r#"{"role":"toolResult","toolCallId":"t1","toolName":"calc","content":[],"usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"isError":false,"timestamp":1}"#,
    )
    .unwrap();
    let event = AgentEvent::TurnEnd {
        message: serde_json::from_str(r#"{"role":"user","content":"hi","timestamp":1}"#).unwrap(),
        tool_results: vec![tool_result],
    };
    let json = serde_json::to_string(&event).unwrap();
    assert!(
        json.contains(
            r#""usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}}"#
        ),
        "turn_end toolResults usage must use JS number formatting, got {json}"
    );
}

#[test]
fn agent_event_message_update_partial_uses_js_numbers() {
    let event = AgentEvent::MessageUpdate {
        message: serde_json::from_str(r#"{"role":"user","content":"hi","timestamp":1}"#).unwrap(),
        assistant_message_event: kiana_ai::types::AssistantMessageEvent::Start {
            partial: Box::new(kiana_ai::types::AssistantMessage {
                usage: zero_usage(),
                ..Default::default()
            }),
        },
    };
    let json = serde_json::to_string(&event).unwrap();
    assert!(
        json.contains(r#""cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}"#),
        "message_update partial usage must use JS number formatting, got {json}"
    );
}
