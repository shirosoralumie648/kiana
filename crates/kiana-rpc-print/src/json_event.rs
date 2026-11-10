//! modes/json-event.ts：把 `AgentSessionEvent` 投影成 JSON/RPC 的 wire 事件。
//!
//! TS: `packages/coding-agent/src/modes/json-event.ts`
//! （`toJsonEvent` / `toJsonAssistantMessageEvent` / `JsonAgentSessionEvent`；spec §2.2、§4.4.2）。
//!
//! ## 投影规则（spec §2.2.2/§2.2.3）
//!
//! - 非 `message_update` 事件原样序列化（TS 返回同一对象引用）。
//! - `message_update` 要求 `message.role === "assistant"`，否则抛
//!   `message_update message is not an assistant message`。
//! - 投影后的键序固定为 `type`、`usage`、`assistantMessageEvent`；`message` 被丢弃。
//! - 内层事件删除 `partial`；`toolcall_start` 追加 `id`、`toolName`（取自
//!   `partial.content[contentIndex]`，越界或类型不对抛
//!   `toolcall_start content at index <n> is not a tool call`）。
//!
//! ## M3 键序修复（`ARCHITECTURE.md` §7.3 M3-verification item 1）
//!
//! TS 运行时构造序与 Rust 结构体声明序不同。`JSON.stringify` 按**插入顺序**输出对象键，
//! 而 TS 的插入顺序由「对象字面量 + 后续赋值」决定；Rust 只能按结构体字段序序列化。
//! [`reorder_wire_keys`] 在写出层把这些对象的键序还原成 TS 运行时顺序：
//!
//! 1. `agent-loop.ts#withToolChanges` 把 `toolsAdded`/`toolsRemoved` 解构出来再追加到
//!    system 消息**末尾**（`timestamp` 之后），Rust 的 `SystemMessage` 把它们放在
//!    `timestamp` 之前。
//! 2. assistant 消息：TS 先写字面量 `role`、`content`、`api`、`provider`、`model`、
//!    `usage`、`stopReason`、`timestamp`，随后由 provider / event-stream / agent-loop 依次
//!    赋值 `providerThinkingLevel`、`responseId`、`responseModel`、`rawStopReason`、
//!    `errorMessage`，再由 `event-stream.ts#time` 追加 `durationMs`、由
//!    `agent-loop.ts:419` 追加 `thinkingLevel`；Rust 的 `AssistantMessage` 把这些字段
//!    分散在 `usage`/`stopReason`/`timestamp` 两侧。
//!    faux provider 例外：`fauxAssistantMessage` 把 `deferred`/`errorMessage`/`responseId`
//!    写在 `timestamp` **之前**（`createErrorMessage` 同序），中止消息
//!    `createAbortedMessage` 则把新加的 `errorMessage` 追加到 `timestamp` 之后。
//! 3. `agent-loop.ts#createToolResultMessage` 把 `durationMs` 写在 `timestamp` **之前**，
//!    `agent-session.ts:1112` 之后才追加 `nestedCalls`（**最末**）；Rust 的
//!    `ToolResultMessage` 顺序是 `... nestedCalls, isError, timestamp, durationMs`。
//! 4. `usage` 对象：大多数 provider 直接内联字面量 `input`、`output`、`cacheRead`、
//!    `cacheWrite`、`reasoning?`、`totalTokens`、`cost`（与 Rust 结构体序一致）；
//!    `anthropic-messages` / `bedrock-converse-stream` 则先写字面量再**追加**
//!    `cacheWrite1h`、`reasoning` 到 `cost` 之后（`anthropic-messages.ts:725,876`、
//!    `bedrock-converse-stream.ts:724`），wire 层按 `api` 还原。
//! 5. 模型对象（`get_state` / `set_model` / `cycle_model` / `get_available_models` 的
//!    `data`）在 TS 里就是模型目录 JSON 的原始对象，键序由目录/覆盖层的构造顺序决定；
//!    Rust 的 [`kiana_ai::types::Model`] 只能按结构体声明序序列化（spec §4.4.2 要求透传目录
//!    实际顺序）。[`reorder_wire_keys`] 对可识别的模型对象按 kiana-ai 内嵌目录的原始键序重排，
//!    并把 serde 丢弃的目录专属键（radius 的 `lab`/`enabled`/`providers`/`free`）补回；
//!    models.json 自定义模型按 `provider-composer.ts#modelFromJson` 的字面量序重排。
//!
//! [`reorder_wire_keys`] 在写出层做一次递归重排，覆盖 `message_start`/`message_end`/
//! `turn_end`/`agent_end`/`entry_appended`/`get_entries` 等所有携带消息对象的记录。
//! 因此 print_mode / rpc_mode 必须把每条 stdout 记录都交给 [`serialize_wire_line`] /
//! [`serialize_event_line`]，**不要**直接序列化记录。递归重排对任意嵌套的透明载荷
//! （如 `tool_execution_start.args`）同样生效——这是 spec §4.4.2 规定的实现方式；只有带
//! `role`/`stopReason`/`thinkingLevel` 或完整模型签名的对象会被改动，正常工具参数不会命中。
//!
//! 已知无法还原的情况：models.json 自定义模型的**嵌套**对象（`inputLimits` 等）在 TS 里
//! 保持用户 JSON 的键序，Rust 侧丢失了来源；扩展/远端注入的模型、`kiana-messages` 由服务端
//! 给出的 usage 键序同理。扩展构造的 assistant 消息若带完整消息签名，其字面量键序同样无法
//! 区分（wire 层按 Rust 结构体的顺序还原）。
//!
//! ## 一次事件的写出流程
//!
//! 1. 会话层给出 [`kiana_session::AgentSessionEvent`]（借用的枚举）。
//! 2. [`to_json_event`]：`message_update` 走手工投影（固定键序、丢弃 `message`、内层事件
//!    删除 `partial` 并在 `toolcall_start` 上补 `id`/`toolName`）；其余事件直接
//!    `serde_json::to_value` 得到等价对象。
//! 3. [`reorder_wire_keys`]（来自 `kiana-agent-core`）在 [`Value`] 上递归重排消息对象、
//!    修复 Rust 结构体字段序与 TS 运行时插入序的差异。
//! 4. [`serialize_json_line`] 用 `kiana_json::js_object::stringify` 生成 `JSON.stringify`
//!    等价的文本并追加 `\n`。
//!
//! [`serialize_wire_line`] 是同一流程去掉第 2 步（用于本来就不是会话事件的记录，例如
//! RPC 的 `success`/`error` 应答与 print 的会话头）。
//!
//! ## 失败模式与调用方约定
//!
//! 本模块只返回错误，不做 I/O。两个可观察错误（`WireError::NotAssistantMessage`、
//! `WireError::ToolCallStartNotToolCall`）对应 TS 的 `throw`；`WireError::Serialize` 只
//! 来自 serde 序列化失败。调用方（`print_mode` / `rpc_mode` 的事件监听器）收到错误时
//! **不写出该记录、不写 stderr**，并把该次 run 标记为投影失败，后续事件一并跳过，直到
//! 真实的 `agent_settled` 才恢复（rpc_mode；print_mode 的 listener 还会在 prompt 返回后
//! 解除抑制，见 `crates/kiana-rpc-print/src/print_mode.rs`）。这是对 TS「异常让 agent run
//! 失败」的近似，不是完整复刻。
//!
//! 注意：第 3 步是**递归**的，作用于任意嵌套的透明载荷，因此把非协议数据（例如工具参数）
//! 交给 [`serialize_wire_line`] 时也会被扫描；只有命中已知对象形状（`role` 加该角色所需
//! 的字段组合，例如 system 的 tool 变更 + `content` + `timestamp`、带 `stopReason` 的
//! assistant、`toolResult` 的完整签名，或可识别的模型对象）才会改动。判断细节见
//! `kiana_agent_core::wire_keys` 里各 `reorder_*` 函数的守卫；不要在协议外的地方复用这些
//! 函数以图省事。

use serde::Serialize;
use serde_json::{Map, Value};

use kiana_agent_core::AgentMessage;
use kiana_agent_core::wire_keys::reorder_usage_object;
pub use kiana_agent_core::wire_keys::reorder_wire_keys;
use kiana_ai::types::{AssistantContentBlock, AssistantMessageEvent, Message};
use kiana_session::AgentSessionEvent;

use crate::jsonl::serialize_json_line;

/// JSON/RPC stdout 上的会话事件。
///
/// TS 的 `JsonAgentSessionEvent` 是编译期联合（`Exclude<AgentSessionEvent, message_update>` +
/// 投影后的 `message_update`）；Rust 侧只在 wire 上存在，因此用 [`Value`] 承载。
pub type JsonAgentSessionEvent = Value;

/// wire 投影错误（文本逐字对齐 TS 的 `Error.message`）。
///
/// 不派生 `Clone`/`PartialEq`：[`serde_json::Error`] 两个都不支持；需要断言错误文本时比较
/// `Display`（测试里用 `to_string()`）。
#[derive(Debug, thiserror::Error)]
pub enum WireError {
    /// TS: `throw new Error("message_update message is not an assistant message")`。
    #[error("message_update message is not an assistant message")]
    NotAssistantMessage,
    /// TS: ``throw new Error(`toolcall_start content at index ${contentIndex} is not a tool call`)``。
    #[error("toolcall_start content at index {index} is not a tool call")]
    ToolCallStartNotToolCall {
        /// `contentIndex`（JS 数组下标）。
        index: usize,
    },
    /// `serde_json::to_value` 失败；正常值不会触发，保证投影路径不 panic。
    #[doc(hidden)]
    #[error(transparent)]
    Serialize(#[from] serde_json::Error),
}

/// `toJsonEvent(event)`：会话事件 -> wire 事件。
///
/// 非 `message_update` 直接 `serde_json::to_value`（TS 返回同一引用；Rust 侧克隆成 [`Value`]）。
///
/// `message_update` 分支的键序是**构造顺序**，不是结构体序：`type` 一定存在，`usage`
/// 仅在消息能提供 usage 时插入（标准消息总有；自定义消息缺 `usage` 键时整个键省略，对应
/// TS 的 `undefined`），`assistantMessageEvent` 永远在最后。
///
/// 错误（全部对应 TS 的 `throw`，调用方应视为“该事件不可投影”）：
/// - [`WireError::NotAssistantMessage`]：`message.role !== "assistant"`；
/// - [`WireError::ToolCallStartNotToolCall`]：内层事件或 `contentIndex` 越界；
/// - `WireError::Serialize`：serde 序列化失败（正常值不会触发）。
///
/// 本函数不做键序修复，需要写出时用 [`serialize_event_line`]。
pub fn to_json_event(event: &AgentSessionEvent) -> Result<JsonAgentSessionEvent, WireError> {
    match event {
        AgentSessionEvent::MessageUpdate {
            message,
            assistant_message_event,
        } => {
            if message.role() != "assistant" {
                return Err(WireError::NotAssistantMessage);
            }
            // TS 返回新对象，键插入顺序固定为 type、usage、assistantMessageEvent。
            let mut value = Map::new();
            value.insert(
                "type".to_string(),
                Value::String("message_update".to_string()),
            );
            if let Some(usage) = message_usage(message)? {
                value.insert("usage".to_string(), usage);
            }
            value.insert(
                "assistantMessageEvent".to_string(),
                to_json_assistant_message_event(assistant_message_event)?,
            );
            Ok(Value::Object(value))
        }
        other => Ok(serde_json::to_value(other)?),
    }
}

/// `event.message.usage`：标准 assistant 消息用结构体序列化；自定义消息读原始 `usage` 键
/// （缺失即 `undefined`，序列化时省略该键）。
///
/// `message_update` 的顶层 `usage` 不在 message 对象里，递归重排看不到它的 `api`，
/// 因此在这里按 `assistant.api` 还原 `anthropic-messages`/`bedrock-converse-stream` 的
/// 运行时键序（TS 直接透传 `event.message.usage`）。自定义消息的 `usage` 由扩展构造，
/// 保持原键序。
///
/// 返回 `Ok(None)` 表示该消息没有 usage，调用方应省略 `usage` 键。在本文件的唯一调用点
/// （已通过 `role == "assistant"` 校验之后），`None` 只会来自没有 `usage` 键的自定义消息；
/// 自定义消息的 `usage` 键存在且为 `null` 时返回 `Ok(Some(Value::Null))`——TS 在那里同样
/// 会写出 `"usage": null`。`Standard` 的非 assistant 分支由 `match` 兜底返回 `None`。
/// 错误仅来自 serde 序列化。
fn message_usage(message: &AgentMessage) -> Result<Option<Value>, WireError> {
    match message {
        AgentMessage::Standard(Message::Assistant(assistant)) => {
            let mut usage = serde_json::to_value(&assistant.usage)?;
            if let Value::Object(map) = &mut usage {
                reorder_usage_object(map, &assistant.api);
            }
            Ok(Some(usage))
        }
        AgentMessage::Custom(object) => Ok(object.get("usage").cloned()),
        _ => Ok(None),
    }
}

/// `toJsonAssistantMessageEvent(event)`：删除 `partial`，`toolcall_start` 追加 id/toolName。
fn to_json_assistant_message_event(event: &AssistantMessageEvent) -> Result<Value, WireError> {
    let mut value = serde_json::to_value(event)?;
    let Value::Object(map) = &mut value else {
        return Ok(value);
    };
    // 所有带 partial 的变体在 wire 上都删除该键；done/error 本来就没有。
    // 用 `shift_remove` 保持其余键的相对顺序（`remove` 在 preserve_order 下是 swap_remove）。
    map.shift_remove("partial");
    if let AssistantMessageEvent::ToolCallStart {
        content_index,
        partial,
    } = event
    {
        let Some(AssistantContentBlock::ToolCall(tool_call)) = partial.content.get(*content_index)
        else {
            return Err(WireError::ToolCallStartNotToolCall {
                index: *content_index,
            });
        };
        // `{ ...deltaEvent, id, toolName }`：追加在 contentIndex 之后。
        map.insert("id".to_string(), Value::String(tool_call.id.clone()));
        map.insert(
            "toolName".to_string(),
            Value::String(tool_call.name.clone()),
        );
    }
    Ok(value)
}

/// `serde_json::to_value` + [`reorder_wire_keys`]。
///
/// 先经 `kiana_agent_core::wire_keys::to_wire_value` 序列化（该函数文档说明了
/// `AnyModel::Remote` 的标记约定及其当前不可达的现状），再就地重排键序。返回的 [`Value`]
/// 可继续被调用方修改；错误只来自底层的 `serde_json` 序列化。需要直接拿 wire 对象（而非
/// 一行文本）时用它；需要写出时优先用 [`serialize_wire_line`]，以免漏掉 JS 数字/转义规则。
pub fn to_wire_value<T: Serialize>(value: &T) -> Result<Value, serde_json::Error> {
    let mut value = kiana_agent_core::wire_keys::to_wire_value(value)?;
    reorder_wire_keys(&mut value);
    Ok(value)
}

/// 任意 wire 记录 -> 一行 JSONL（键序修复 + `JSON.stringify` + `\n`）。
///
/// 用于不是 [`kiana_session::AgentSessionEvent`] 的记录：print 的会话头、RPC 的
/// `success`/`error` 应答、扩展 UI 请求等。返回值自带结尾 `\n`，调用方直接用
/// [`RawStdout::write_raw`](crate::RawStdout::write_raw) 写出即可，**不要**再补换行。
///
/// 错误：`serde_json` 序列化失败（与 [`to_wire_value`] 同源）。注意数字格式、字符串转义
/// 与键序由 [`serialize_json_line`] 统一处理，绕过本函数直接 `serde_json::to_string`
/// 会破坏与 TS 的字节级一致（`ARCHITECTURE.md` §4.3）。
pub fn serialize_wire_line<T: Serialize>(value: &T) -> Result<String, serde_json::Error> {
    let value = to_wire_value(value)?;
    Ok(serialize_json_line(&value))
}

/// 会话事件 -> 一行 JSONL（投影 + 键序修复 + `JSON.stringify` + `\n`）。
///
/// print/json 与 RPC 模式的**每条**事件记录都必须经过它。
///
/// 错误类型是 [`WireError`]（包含投影错误与序列化错误），调用方通常据此进入“投影失败”
/// 状态而不是写出错误行；见本文件模块文档的“失败模式”一节。
pub fn serialize_event_line(event: &AgentSessionEvent) -> Result<String, WireError> {
    let mut value = to_json_event(event)?;
    reorder_wire_keys(&mut value);
    Ok(serialize_json_line(&value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use kiana_agent_core::AgentMessage;
    use kiana_ai::types::{
        AssistantContentBlock, AssistantMessage, AssistantMessageEvent, AssistantRole, Message,
        StopReason, ToolCall, ToolCallKind, Usage,
    };

    fn assistant_message() -> AssistantMessage {
        AssistantMessage {
            role: AssistantRole::Assistant,
            usage: Usage::default(),
            stop_reason: StopReason::Stop,
            ..AssistantMessage::default()
        }
    }

    #[test]
    fn non_message_update_events_pass_through() {
        let event = AgentSessionEvent::AgentStart;
        let value = to_json_event(&event).unwrap();
        assert_eq!(value, serde_json::json!({"type": "agent_start"}));
    }

    #[test]
    fn message_update_rejects_non_assistant() {
        let event = AgentSessionEvent::MessageUpdate {
            message: AgentMessage::Standard(Message::User(kiana_ai::types::UserMessage {
                role: kiana_ai::types::UserRole::User,
                content: kiana_ai::types::UserContent::Text("hi".into()),
                timestamp: 0,
            })),
            assistant_message_event: AssistantMessageEvent::Start {
                partial: Box::new(assistant_message()),
            },
        };
        assert_eq!(
            to_json_event(&event).unwrap_err().to_string(),
            "message_update message is not an assistant message"
        );
    }

    #[test]
    fn toolcall_start_out_of_range_reports_index() {
        let event = AgentSessionEvent::MessageUpdate {
            message: AgentMessage::Standard(Message::Assistant(assistant_message())),
            assistant_message_event: AssistantMessageEvent::ToolCallStart {
                content_index: 3,
                partial: Box::new(assistant_message()),
            },
        };
        assert_eq!(
            to_json_event(&event).unwrap_err().to_string(),
            "toolcall_start content at index 3 is not a tool call"
        );
    }

    #[test]
    fn toolcall_start_appends_id_and_name() {
        let mut partial = assistant_message();
        partial
            .content
            .push(AssistantContentBlock::ToolCall(ToolCall {
                kind_tool_call: ToolCallKind::ToolCall,
                id: "call_x".into(),
                name: "write".into(),
                arguments: Default::default(),
                thought_signature: None,
                namespace: None,
                arguments_error: None,
            }));
        let event = AgentSessionEvent::MessageUpdate {
            message: AgentMessage::Standard(Message::Assistant(partial.clone())),
            assistant_message_event: AssistantMessageEvent::ToolCallStart {
                content_index: 0,
                partial: Box::new(partial),
            },
        };
        let value = to_json_event(&event).unwrap();
        let inner = &value["assistantMessageEvent"];
        assert_eq!(inner["type"], "toolcall_start");
        assert_eq!(inner["contentIndex"], 0);
        assert_eq!(inner["id"], "call_x");
        assert_eq!(inner["toolName"], "write");
        assert!(inner.get("partial").is_none());
    }
}
