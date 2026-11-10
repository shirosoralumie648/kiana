//! 逐字段 serde 契约：键顺序、枚举拼写、`undefined` 省略 vs `null`、41 个事件往返。
//!
//! 黄金值来自 TS 源码的对象字面量/接口声明（`core/extensions/types.ts`、
//! `core/cache-warmer.ts`、`core/tools/*.ts`）。测试比较**字符串**而不是 `Value`：
//! `serde_json` 开启 `preserve_order`，字符串比较才能钉住键顺序（`ARCHITECTURE.md` §4.3）。

use indexmap::IndexMap;
use kiana_agent_core::{AgentMessage, AgentToolResult, AgentToolUpdateCallback};
use kiana_ai::types::{
    ImageContent, ImageKind, JsonValue, Message, Model, ModelCost, ModelInputModality, Usage,
    UserContent, UserContentBlock,
};
use kiana_compaction::{CompactionPreparation, CompactionResult};
use kiana_config::cache::warmer::{CacheWarmingAction, CacheWarmingDecisionEvent};
use kiana_core::mcp_servers::{RegisteredMcpServer, validate_mcp_server_config};
use kiana_core::source_info::{SourceInfo, SourceOrigin, SourceScope};
use kiana_ext_api::events::{
    AgentActivityOutcome, AgentBeforeSettleEvent, AgentEndEvent, AgentSettledEvent,
    AgentStartEvent, BashToolCallEvent, BashToolResultEvent, BeforeAgentStartCustomEvent,
    BeforeAgentStartCustomMessageType, BeforeAgentStartEvent, BeforeAgentStartEventType,
    BeforeAgentStartUserEvent, BeforeAgentStartUserMessageType, BeforeProviderHeadersEvent,
    BeforeProviderRequestEvent, BeforeProviderRequestEventType, BoundaryContextPreview,
    BoundaryResult, CompactionEntryDraft, CompactionEntryDraftType, CompactionReason,
    ContextEditEntryDraft, ContextEditEntryDraftType, ContextEvent, ContextEventResult,
    ContextEventType, ContextWithSystemEvent, ContextWithSystemEventType, CustomEntryDraft,
    CustomEntryDraftType, CustomMessageEntryDraft, CustomMessageEntryDraftType,
    CustomToolCallEvent, CustomToolResultEvent, EditToolCallEvent, EditToolResultEvent,
    ExtensionEvent, ExtensionEventKind, ExtensionEventResult, FindToolCallEvent,
    FindToolResultEvent, ForkPosition, GrepToolCallEvent, GrepToolResultEvent, InputEvent,
    InputEventResult, InputSource, LsToolCallEvent, LsToolResultEvent, McpServersChangeEvent,
    McpServersChangeEventType, MessageEndEvent, MessageEndEventResult, MessageStartEvent,
    MessageStartEventType, MessageUpdateEvent, MessageUpdateEventType, ModelSelectEvent,
    ModelSelectEventType, ModelSelectSource, OptionalPatch, PowerShellToolCallEvent,
    PowerShellToolResultEvent, ProjectTrustEvent, ProjectTrustEventType, ProviderStreamEvent,
    ProviderStreamEventType, ReadToolCallEvent, ReadToolResultEvent, ResourcesDiscoverEvent,
    ResourcesDiscoverEventType, ResourcesDiscoverReason, SessionBeforeCompactEvent,
    SessionBeforeCompactEventType, SessionBeforeForkEvent, SessionBeforeForkEventType,
    SessionBeforeSwitchEvent, SessionBeforeSwitchEventType, SessionBeforeTreeEvent,
    SessionBeforeTreeEventType, SessionBoundaryDraft, SessionCompactEvent, SessionCompactEventType,
    SessionCompactFailedEvent, SessionCompactFailedEventType, SessionEvent,
    SessionInfoChangedEvent, SessionInfoChangedEventType, SessionShutdownEvent,
    SessionShutdownEventType, SessionStartEvent, SessionStartEventType, SessionStartReason,
    SessionTreeEvent, SessionTreeEventType, StreamingBehavior, ThinkingLevelSelectEvent,
    ThinkingLevelSelectEventType, ToolCallEvent, ToolExecutionEndEvent, ToolExecutionEndEventType,
    ToolExecutionStartEvent, ToolExecutionStartEventType, ToolExecutionUpdateEvent,
    ToolExecutionUpdateEventType, ToolResultEvent, TreePreparation, TurnEndEvent, TurnEndEventType,
    TurnStartEvent, TurnStartEventType, UIPromptEndEvent, UIPromptEndEventType, UIPromptKind,
    UIPromptReason, UIPromptStartEvent, UIPromptStartEventType, UserBashEvent, UserBashEventType,
    WriteToolCallEvent, WriteToolResultEvent,
};
use kiana_ext_api::system_prompt::{
    BuildSystemPromptOptions, ContextFile, NormalizedBuildSystemPromptOptions, Skill,
};
use kiana_ext_api::tool_result::{BashExecResult, BashExecutionStatus, BashResult};
use kiana_ext_api::tool_types::{
    BashToolDetails, BashToolInput, BashToolName, EditToolDetails, EditToolInput, EditToolName,
    FindToolDetails, FindToolInput, FindToolName, GrepToolDetails, GrepToolInput, GrepToolName,
    LsToolDetails, LsToolInput, LsToolName, PowerShellToolDetails, PowerShellToolInput,
    PowerShellToolName, ReadToolDetails, ReadToolInput, ReadToolName, ReplaceEdit, ToolExposure,
    TruncatedBy, TruncationResult, WriteToolInput, WriteToolName,
};
use kiana_ext_api::types::{
    ContextUsage, CustomMessagePayload, DeliverAs, ExtensionError, ExtensionFlag, FlagType,
    MarkdownMessageType, MarkdownTransformContext, MessageRenderOptions, ProviderChatModelConfig,
    ProviderChatModelType, ProviderClassifierModelConfig, ProviderClassifierModelType,
    ProviderImageModelConfig, ProviderImageModelType, ProviderModelConfig, SendMessageOptions,
    SendUserMessageDeliverAs, SendUserMessageOptions,
};
use kiana_ext_api::ui::WidgetPlacement;
use kiana_session_core::messages::CustomMessage;
use kiana_session_core::session_manager::{BranchSummaryEntry, CompactionEntry, SessionEntry};
use serde::de::DeserializeOwned;
use serde_json::json;

// ---------------------------------------------------------------------------
// 测试辅助
// ---------------------------------------------------------------------------

fn parse<T: DeserializeOwned>(value: serde_json::Value) -> T {
    serde_json::from_value(value).expect("测试夹具必须可反序列化")
}

fn round_trip(event: &ExtensionEvent) -> String {
    let text = serde_json::to_string(event).expect("序列化失败");
    let decoded: ExtensionEvent = serde_json::from_str(&text).expect("反序列化失败");
    assert_eq!(decoded.kind(), event.kind(), "kind 往返不一致: {text}");
    let again = serde_json::to_string(&decoded).expect("重新序列化失败");
    assert_eq!(text, again, "事件 JSON 往返不逐字一致");
    text
}

fn agent_message_user(text: &str) -> AgentMessage {
    parse(json!({ "role": "user", "content": text, "timestamp": 1 }))
}

fn assistant_message() -> Message {
    parse(json!({
        "role": "assistant",
        "content": [],
        "api": "faux",
        "provider": "faux",
        "model": "faux-1",
        "usage": {
            "input": 0,
            "output": 0,
            "cacheRead": 0,
            "cacheWrite": 0,
            "totalTokens": 0,
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 }
        },
        "stopReason": "stop",
        "timestamp": 1
    }))
}

fn session_entry() -> SessionEntry {
    parse(json!({
        "type": "message",
        "id": "e1",
        "timestamp": "2026-01-01T00:00:00.000Z",
        "message": { "role": "user", "content": "hi", "timestamp": 1 }
    }))
}

fn compaction_entry() -> CompactionEntry {
    parse(json!({
        "type": "compaction",
        "id": "c1",
        "timestamp": "2026-01-01T00:00:00.000Z",
        "summary": "s",
        "firstKeptEntryId": "e1",
        "tokensBefore": 10
    }))
}

fn branch_summary_entry() -> BranchSummaryEntry {
    parse(json!({
        "type": "branch_summary",
        "id": "b1",
        "timestamp": "2026-01-01T00:00:00.000Z",
        "fromId": "e1",
        "summary": "s"
    }))
}

fn preparation() -> CompactionPreparation {
    parse(json!({
        "firstKeptEntryId": "e1",
        "messagesToSummarize": [],
        "turnPrefixMessages": [],
        "isSplitTurn": false,
        "tokensBefore": 10,
        "fileOps": { "read": [], "written": [], "edited": [] },
        "settings": { "enabled": true, "reserveTokens": 16384, "keepRecentTokens": 20000 }
    }))
}

fn custom_message() -> CustomMessage {
    parse(json!({
        "role": "custom",
        "customType": "t",
        "content": "hello",
        "display": true,
        "timestamp": 1
    }))
}

fn preview() -> BoundaryContextPreview {
    BoundaryContextPreview {
        context_entries: vec![kiana_session_core::session_manager::ProjectedSessionEntry {
            source_entry: session_entry(),
            messages: vec![agent_message_user("hi")],
        }],
        context_messages: vec![agent_message_user("hi")],
        llm_messages: vec![assistant_message()],
        pending_messages: Vec::new(),
        can_continue: true,
    }
}

fn model() -> Model {
    parse(json!({
        "id": "faux-1",
        "name": "Faux",
        "api": "faux",
        "provider": "faux",
        "baseUrl": "https://example.com",
        "input": ["text"],
        "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
        "contextWindow": 1000,
        "maxTokens": 100
    }))
}

fn normalized_options() -> NormalizedBuildSystemPromptOptions {
    NormalizedBuildSystemPromptOptions {
        custom_prompt: None,
        force_system_prompt: None,
        selected_tools: Vec::new(),
        hidden_tools: Vec::new(),
        tool_snippets: IndexMap::new(),
        tool_guidelines: IndexMap::new(),
        prompt_guidelines: Vec::new(),
        append_system_prompt: String::new(),
        sections: IndexMap::new(),
        cwd: String::new(),
        context_files: Vec::new(),
        skills: Vec::new(),
    }
}

fn tool_result_input() -> IndexMap<String, JsonValue> {
    IndexMap::new()
}

// ---------------------------------------------------------------------------
// 非事件纯数据类型
// ---------------------------------------------------------------------------

#[test]
fn context_usage_writes_required_nulls() {
    let usage = ContextUsage {
        tokens: None,
        context_window: 100,
        percent: None,
    };
    assert_eq!(
        serde_json::to_string(&usage).unwrap(),
        r#"{"tokens":null,"contextWindow":100,"percent":null}"#
    );
    let usage = ContextUsage {
        tokens: Some(12),
        context_window: 100,
        percent: Some(12.5),
    };
    assert_eq!(
        serde_json::to_string(&usage).unwrap(),
        r#"{"tokens":12,"contextWindow":100,"percent":12.5}"#
    );
}

#[test]
fn tool_exposure_uses_kebab_case_spellings() {
    let strings: Vec<String> = [
        ToolExposure::Direct,
        ToolExposure::ModelOnly,
        ToolExposure::Codemode,
        ToolExposure::Deferred,
        ToolExposure::Hidden,
    ]
    .into_iter()
    .map(|value| serde_json::to_string(&value).unwrap())
    .collect();
    assert_eq!(
        strings,
        [
            r#""direct""#,
            r#""model-only""#,
            r#""codemode""#,
            r#""deferred""#,
            r#""hidden""#
        ]
    );
}

#[test]
fn widget_placement_spellings() {
    assert_eq!(WidgetPlacement::AboveEditor.as_str(), "aboveEditor");
    assert_eq!(WidgetPlacement::BelowEditor.as_str(), "belowEditor");
}

#[test]
fn markdown_transform_context_assistant_thinking() {
    let context = MarkdownTransformContext {
        message_type: MarkdownMessageType::AssistantThinking,
        is_streaming: false,
        available_width: 80,
        message_id: None,
        timestamp: None,
    };
    assert_eq!(
        serde_json::to_string(&context).unwrap(),
        r#"{"messageType":"assistant-thinking","isStreaming":false,"availableWidth":80}"#
    );
}

#[test]
fn message_render_options_shape() {
    let options = MessageRenderOptions {
        expanded: true,
        output_pad: 2,
    };
    assert_eq!(
        serde_json::to_string(&options).unwrap(),
        r#"{"expanded":true,"outputPad":2}"#
    );
}

#[test]
fn input_event_uses_follow_up_camel_case() {
    let event = InputEvent {
        event_type: Default::default(),
        text: "hi".into(),
        images: None,
        source: InputSource::Extension,
        streaming_behavior: Some(StreamingBehavior::FollowUp),
    };
    assert_eq!(
        serde_json::to_string(&event).unwrap(),
        r#"{"type":"input","text":"hi","source":"extension","streamingBehavior":"followUp"}"#
    );
}

#[test]
fn send_message_deliver_as_next_turn() {
    let options = SendMessageOptions {
        message: CustomMessagePayload {
            custom_type: "t".into(),
            content: UserContent::Text("hi".into()),
            display: true,
            details: None,
        },
        trigger_turn: None,
        deliver_as: Some(DeliverAs::NextTurn),
    };
    assert_eq!(
        serde_json::to_string(&options).unwrap(),
        r#"{"message":{"customType":"t","content":"hi","display":true},"deliverAs":"nextTurn"}"#
    );
}

#[test]
fn send_user_message_deliver_as_spellings() {
    let options = SendUserMessageOptions {
        deliver_as: Some(SendUserMessageDeliverAs::FollowUp),
        expand_prompt_templates: Some(false),
    };
    assert_eq!(
        serde_json::to_string(&options).unwrap(),
        r#"{"deliverAs":"followUp","expandPromptTemplates":false}"#
    );
}

#[test]
fn boundary_drafts_use_exact_tags() {
    let custom = SessionBoundaryDraft::Custom(CustomEntryDraft {
        event_type: CustomEntryDraftType::Custom,
        custom_type: "t".into(),
        data: None,
    });
    assert_eq!(
        serde_json::to_string(&custom).unwrap(),
        r#"{"type":"custom","customType":"t"}"#
    );

    let message = SessionBoundaryDraft::CustomMessage(CustomMessageEntryDraft {
        event_type: CustomMessageEntryDraftType::CustomMessage,
        custom_type: "t".into(),
        content: UserContent::Text("hi".into()),
        display: true,
        details: None,
    });
    assert_eq!(
        serde_json::to_string(&message).unwrap(),
        r#"{"type":"custom_message","customType":"t","content":"hi","display":true}"#
    );

    let edit = SessionBoundaryDraft::ContextEdit(ContextEditEntryDraft {
        event_type: ContextEditEntryDraftType::ContextEdit,
        target_id: "e1".into(),
        replacement: None,
    });
    assert_eq!(
        serde_json::to_string(&edit).unwrap(),
        r#"{"type":"context_edit","targetId":"e1","replacement":null}"#
    );

    let compaction = SessionBoundaryDraft::Compaction(CompactionEntryDraft {
        event_type: CompactionEntryDraftType::Compaction,
        summary: "s".into(),
        first_kept_entry_id: None,
        details: None,
        usage: None,
    });
    assert_eq!(
        serde_json::to_string(&compaction).unwrap(),
        r#"{"type":"compaction","summary":"s","firstKeptEntryId":null}"#
    );
}

#[test]
fn provider_headers_keep_null_values() {
    let mut headers = IndexMap::new();
    headers.insert("x-a".to_string(), Some("1".to_string()));
    headers.insert("x-del".to_string(), None);
    let event = BeforeProviderHeadersEvent {
        event_type: Default::default(),
        headers,
    };
    assert_eq!(
        serde_json::to_string(&event).unwrap(),
        r#"{"type":"before_provider_headers","headers":{"x-a":"1","x-del":null}}"#
    );
}

#[test]
fn required_undefined_fields_are_omitted() {
    let info = SessionInfoChangedEvent {
        event_type: SessionInfoChangedEventType::SessionInfoChanged,
        name: None,
    };
    assert_eq!(
        serde_json::to_string(&info).unwrap(),
        r#"{"type":"session_info_changed"}"#
    );

    let end = ToolExecutionEndEvent {
        event_type: ToolExecutionEndEventType::ToolExecutionEnd,
        tool_call_id: "c1".into(),
        tool_name: "bash".into(),
        result: json!({"content": []}),
        is_error: false,
        duration_ms: None,
        parent_tool_call_id: None,
    };
    assert_eq!(
        serde_json::to_string(&end).unwrap(),
        r#"{"type":"tool_execution_end","toolCallId":"c1","toolName":"bash","result":{"content":[]},"isError":false}"#
    );

    // ModelSelectEvent.previousModel 是 required-undefined：缺省时省略键。
    let select = ModelSelectEvent {
        event_type: ModelSelectEventType::ModelSelect,
        model: model(),
        previous_model: None,
        source: ModelSelectSource::Cycle,
    };
    let text = serde_json::to_string(&select).unwrap();
    assert!(!text.contains("previousModel"), "{text}");
    assert!(text.starts_with(r#"{"type":"model_select","model":{"#));
    assert!(text.ends_with(r#""source":"cycle"}"#));
}

#[test]
fn tree_types_write_required_nulls() {
    let preparation = TreePreparation {
        target_id: "t".into(),
        old_leaf_id: None,
        common_ancestor_id: None,
        entries_to_summarize: Vec::new(),
        user_wants_summary: false,
        custom_instructions: None,
        replace_instructions: None,
        label: None,
    };
    assert_eq!(
        serde_json::to_string(&preparation).unwrap(),
        r#"{"targetId":"t","oldLeafId":null,"commonAncestorId":null,"entriesToSummarize":[],"userWantsSummary":false}"#
    );

    let event = SessionTreeEvent {
        event_type: SessionTreeEventType::SessionTree,
        new_leaf_id: None,
        old_leaf_id: None,
        summary_entry: None,
        from_extension: None,
    };
    assert_eq!(
        serde_json::to_string(&event).unwrap(),
        r#"{"type":"session_tree","newLeafId":null,"oldLeafId":null}"#
    );
}

#[test]
fn bash_results_shapes() {
    let result = BashResult {
        output: "o".into(),
        exit_code: None,
        cancelled: false,
        truncated: false,
        full_output_path: None,
        execution_status: None,
    };
    assert_eq!(
        serde_json::to_string(&result).unwrap(),
        r#"{"output":"o","cancelled":false,"truncated":false}"#
    );

    let exec = BashExecResult {
        exit_code: None,
        execution_status: None,
    };
    assert_eq!(
        serde_json::to_string(&exec).unwrap(),
        r#"{"exitCode":null}"#
    );
    let exec = BashExecResult {
        exit_code: Some(1),
        execution_status: None,
    };
    assert_eq!(serde_json::to_string(&exec).unwrap(), r#"{"exitCode":1}"#);
}

#[test]
fn bash_execution_status_is_backward_compatible_and_round_trips() {
    // 旧 JSON 不含 executionStatus 键：反序列化成功、字段为 None，重新序列化逐字不变。
    let old_result = r#"{"output":"o","exitCode":2,"cancelled":false,"truncated":false}"#;
    let decoded: BashResult = serde_json::from_str(old_result).expect("old BashResult");
    assert_eq!(decoded.execution_status, None);
    assert_eq!(serde_json::to_string(&decoded).unwrap(), old_result);

    let old_exec = r#"{"exitCode":0}"#;
    let decoded: BashExecResult = serde_json::from_str(old_exec).expect("old BashExecResult");
    assert_eq!(decoded.execution_status, None);
    assert_eq!(serde_json::to_string(&decoded).unwrap(), old_exec);

    // 新 JSON：外部标签 + camelCase 变体名；所有变体都能往返
    // （Unsandboxed 另外覆盖信号终止时 `code` 为 None 的形态）。
    let statuses = [
        BashExecutionStatus::ConfirmedExit { code: 7 },
        BashExecutionStatus::Unsandboxed {
            code: Some(0),
            signal: None,
        },
        BashExecutionStatus::Unsandboxed {
            code: None,
            signal: Some(15),
        },
        BashExecutionStatus::Unconfirmed,
        BashExecutionStatus::BackendUnavailable {
            reason: "no bwrap".to_string(),
        },
        BashExecutionStatus::Interrupted {
            reason: "cancelled before start".to_string(),
        },
    ];
    for status in &statuses {
        let encoded = serde_json::to_string(status).expect("serialize status");
        let decoded: BashExecutionStatus = serde_json::from_str(&encoded).expect("round trip");
        assert_eq!(&decoded, status, "json={encoded}");
    }
    assert_eq!(
        serde_json::to_string(&statuses[0]).unwrap(),
        r#"{"confirmedExit":{"code":7}}"#
    );
    assert_eq!(
        serde_json::to_string(&statuses[3]).unwrap(),
        r#""unconfirmed""#
    );

    // BashResult / BashExecResult 带状态时追加 executionStatus 键，且可完整往返。
    let result = BashResult {
        output: "o".into(),
        exit_code: Some(7),
        cancelled: false,
        truncated: false,
        full_output_path: None,
        execution_status: Some(BashExecutionStatus::ConfirmedExit { code: 7 }),
    };
    let encoded = serde_json::to_string(&result).unwrap();
    assert_eq!(
        encoded,
        r#"{"output":"o","exitCode":7,"cancelled":false,"truncated":false,"executionStatus":{"confirmedExit":{"code":7}}}"#
    );
    assert_eq!(
        serde_json::from_str::<BashResult>(&encoded).unwrap(),
        result
    );

    let exec = BashExecResult {
        exit_code: Some(0),
        execution_status: Some(BashExecutionStatus::Unsandboxed {
            code: Some(0),
            signal: None,
        }),
    };
    let encoded = serde_json::to_string(&exec).unwrap();
    assert_eq!(
        encoded,
        r#"{"exitCode":0,"executionStatus":{"unsandboxed":{"code":0,"signal":null}}}"#
    );
    assert_eq!(
        serde_json::from_str::<BashExecResult>(&encoded).unwrap(),
        exec
    );
}

#[test]
fn truncation_result_field_order_and_null_reason() {
    let result = TruncationResult {
        content: String::new(),
        truncated: false,
        truncated_by: None,
        total_lines: 0,
        total_bytes: 0,
        output_lines: 0,
        output_bytes: 0,
        last_line_partial: false,
        first_line_exceeds_limit: false,
        max_lines: 2000,
        max_bytes: 51200,
    };
    assert_eq!(
        serde_json::to_string(&result).unwrap(),
        r#"{"content":"","truncated":false,"truncatedBy":null,"totalLines":0,"totalBytes":0,"outputLines":0,"outputBytes":0,"lastLinePartial":false,"firstLineExceedsLimit":false,"maxLines":2000,"maxBytes":51200}"#
    );
    let result = TruncationResult {
        truncated_by: Some(TruncatedBy::Lines),
        truncated: true,
        ..result
    };
    assert!(
        serde_json::to_string(&result)
            .unwrap()
            .contains(r#""truncatedBy":"lines""#)
    );
}

#[test]
fn tool_input_and_details_serde() {
    let input = BashToolInput {
        command: "ls".into(),
        timeout: Some(1.5),
    };
    assert_eq!(
        serde_json::to_string(&input).unwrap(),
        r#"{"command":"ls","timeout":1.5}"#
    );
    let _: PowerShellToolInput = input;

    let read = ReadToolInput {
        path: "/f".into(),
        offset: Some(2.0),
        limit: None,
    };
    assert_eq!(
        serde_json::to_string(&read).unwrap(),
        r#"{"path":"/f","offset":2}"#
    );

    let edit = EditToolInput {
        path: "/f".into(),
        edits: vec![ReplaceEdit {
            old_text: "a".into(),
            new_text: "b".into(),
        }],
    };
    assert_eq!(
        serde_json::to_string(&edit).unwrap(),
        r#"{"path":"/f","edits":[{"oldText":"a","newText":"b"}]}"#
    );

    let write = WriteToolInput {
        path: "/f".into(),
        content: "c".into(),
    };
    assert_eq!(
        serde_json::to_string(&write).unwrap(),
        r#"{"path":"/f","content":"c"}"#
    );

    let grep = GrepToolInput {
        pattern: "p".into(),
        ..Default::default()
    };
    assert_eq!(serde_json::to_string(&grep).unwrap(), r#"{"pattern":"p"}"#);

    let find = FindToolInput {
        pattern: "*.rs".into(),
        ..Default::default()
    };
    assert_eq!(
        serde_json::to_string(&find).unwrap(),
        r#"{"pattern":"*.rs"}"#
    );

    let ls = LsToolInput::default();
    assert_eq!(serde_json::to_string(&ls).unwrap(), "{}");

    let bash_details = BashToolDetails {
        truncation: Some(TruncationResult::default()),
        full_output_path: Some("/tmp/x".into()),
    };
    let text = serde_json::to_string(&bash_details).unwrap();
    assert!(text.starts_with(r#"{"truncation":{"content":""#));
    assert!(text.ends_with(r#""fullOutputPath":"/tmp/x"}"#));
    let _: PowerShellToolDetails = bash_details;

    let _ = (
        ReadToolDetails::default(),
        EditToolDetails::default(),
        GrepToolDetails::default(),
        FindToolDetails::default(),
        LsToolDetails::default(),
    );
}

#[test]
fn system_prompt_types() {
    let source = SourceInfo {
        path: "p".into(),
        source: "local".into(),
        scope: SourceScope::User,
        origin: SourceOrigin::TopLevel,
        base_dir: Some("base".into()),
    };
    let skill = Skill {
        name: "n".into(),
        description: "d".into(),
        file_path: "/f/SKILL.md".into(),
        base_dir: "/f".into(),
        source_info: source.clone(),
        disable_model_invocation: false,
    };
    assert_eq!(
        serde_json::to_string(&skill).unwrap(),
        r#"{"name":"n","description":"d","filePath":"/f/SKILL.md","baseDir":"/f","sourceInfo":{"path":"p","source":"local","scope":"user","origin":"top-level","baseDir":"base"},"disableModelInvocation":false}"#
    );

    let options = BuildSystemPromptOptions {
        cwd: "/w".into(),
        selected_tools: Some(vec!["read".into()]),
        skills: Some(vec![skill.clone()]),
        context_files: Some(vec![ContextFile {
            path: "AGENTS.md".into(),
            content: "c".into(),
        }]),
        ..Default::default()
    };
    assert_eq!(
        serde_json::to_string(&options).unwrap(),
        format!(
            r#"{{"selectedTools":["read"],"cwd":"/w","contextFiles":[{{"path":"AGENTS.md","content":"c"}}],"skills":[{}]}}"#,
            serde_json::to_string(&skill).unwrap()
        )
    );
    let decoded: BuildSystemPromptOptions =
        serde_json::from_str(&serde_json::to_string(&options).unwrap()).unwrap();
    assert_eq!(decoded, options);

    let normalized = normalized_options();
    assert_eq!(
        serde_json::to_string(&normalized).unwrap(),
        r#"{"selectedTools":[],"hiddenTools":[],"toolSnippets":{},"toolGuidelines":{},"promptGuidelines":[],"appendSystemPrompt":"","sections":{},"cwd":"","contextFiles":[],"skills":[]}"#
    );
}

#[test]
fn provider_model_config_family() {
    let chat = ProviderChatModelConfig {
        id: "m".into(),
        name: "M".into(),
        api: None,
        base_url: None,
        input: vec![ModelInputModality::Text],
        input_limits: None,
        cost: ModelCost::default(),
        headers: None,
        model_type: None,
        reasoning: true,
        thinking_level_map: None,
        prompt_cache: None,
        context_window: 100,
        max_tokens: 10,
        sampling_params: None,
        compat: None,
    };
    let text = serde_json::to_string(&chat).unwrap();
    assert!(text.starts_with(r#"{"id":"m","name":"M","input":["text"]"#));
    assert!(!text.contains(r#""type""#));
    assert!(text.ends_with(r#""reasoning":true,"contextWindow":100,"maxTokens":10}"#));

    // `type?: "chat"` 是字面量：显式给出时写出 `"type":"chat"`。
    let mut chat_typed = chat.clone();
    chat_typed.model_type = Some(ProviderChatModelType::Chat);
    let text = serde_json::to_string(&chat_typed).unwrap();
    assert!(text.contains(r#""type":"chat""#));

    let image = ProviderImageModelConfig {
        id: "i".into(),
        name: "I".into(),
        api: None,
        base_url: None,
        input: vec![ModelInputModality::Text],
        input_limits: None,
        cost: ModelCost::default(),
        headers: None,
        model_type: ProviderImageModelType::Image,
        output: vec![ModelInputModality::Image],
    };
    let text = serde_json::to_string(&image).unwrap();
    assert!(text.contains(r#""type":"image""#));
    assert!(text.ends_with(r#""output":["image"]}"#));

    let classifier = ProviderClassifierModelConfig {
        id: "c".into(),
        name: "C".into(),
        api: None,
        base_url: None,
        input: vec![ModelInputModality::Text],
        input_limits: None,
        cost: ModelCost::default(),
        headers: None,
        model_type: ProviderClassifierModelType::Classifier,
        context_window: 128,
    };
    let text = serde_json::to_string(&classifier).unwrap();
    assert!(text.contains(r#""type":"classifier""#));

    // chat 的 `type` 可以省略；反序列化归一为 Chat。
    let decoded: ProviderModelConfig =
        serde_json::from_str(&serde_json::to_string(&chat).unwrap()).unwrap();
    assert_eq!(decoded, ProviderModelConfig::Chat(chat));
    let decoded: ProviderModelConfig =
        serde_json::from_str(&serde_json::to_string(&image).unwrap()).unwrap();
    assert_eq!(decoded, ProviderModelConfig::Image(image));
    let decoded: ProviderModelConfig =
        serde_json::from_str(&serde_json::to_string(&classifier).unwrap()).unwrap();
    assert_eq!(decoded, ProviderModelConfig::Classifier(classifier));
}

#[test]
fn extension_flag_and_error_shapes() {
    let flag = ExtensionFlag {
        name: "f".into(),
        description: None,
        r#type: FlagType::Boolean,
        default: Some(kiana_ext_api::types::FlagValue::Bool(true)),
        extension_path: "e".into(),
    };
    assert_eq!(
        serde_json::to_string(&flag).unwrap(),
        r#"{"name":"f","type":"boolean","default":true,"extensionPath":"e"}"#
    );
    let error = ExtensionError {
        extension_path: "e".into(),
        event: "input".into(),
        error: "boom".into(),
        stack: None,
    };
    assert_eq!(
        serde_json::to_string(&error).unwrap(),
        r#"{"extensionPath":"e","event":"input","error":"boom"}"#
    );
}

#[test]
fn optional_patch_distinguishes_absent_and_undefined() {
    let empty = kiana_ext_api::events::ToolResultEventResult::default();
    assert_eq!(serde_json::to_string(&empty).unwrap(), "{}");

    let undefined = kiana_ext_api::events::ToolResultEventResult {
        content: OptionalPatch::Undefined,
        ..Default::default()
    };
    // 两种“无值”状态在 JSON 里都省略键；区别只在内存中。
    assert_eq!(serde_json::to_string(&undefined).unwrap(), "{}");

    let value = kiana_ext_api::events::ToolResultEventResult {
        content: OptionalPatch::Value(Vec::new()),
        ..Default::default()
    };
    assert_eq!(serde_json::to_string(&value).unwrap(), r#"{"content":[]}"#);

    // 缺键 → Absent；显式 null 是**值**（TS 用 `!== undefined` 判断，null 会被应用，
    // 不是“键存在但值为 undefined”）。
    let missing: kiana_ext_api::events::ToolResultEventResult =
        serde_json::from_str(r#"{"isError":true}"#).unwrap();
    assert!(missing.content.is_absent());
    assert_eq!(missing.is_error, OptionalPatch::Value(true));
    let explicit: kiana_ext_api::events::ToolResultEventResult =
        serde_json::from_str(r#"{"details":null}"#).unwrap();
    // `JsonValue` 能表示 null → `Value(JsonValue::Null)`。
    assert_eq!(explicit.details, OptionalPatch::Value(JsonValue::Null));
    // `Value(null)` 序列化回显式 null（TS 的 `JSON.stringify` 写出 `"details":null`）。
    assert_eq!(
        serde_json::to_string(&explicit).unwrap(),
        r#"{"details":null}"#
    );

    // 不能表示 null 的字段（`bool`/`Vec`/`Usage`）：显式 null 必须保留为“有补丁”，
    // 不能退化为 Undefined。TS 的 emitToolResult 用 `r.X !== undefined` 判断，
    // `{isError: null}` 会覆盖字段（null 在 JS 里 falsy）。
    let null_is_error: kiana_ext_api::events::ToolResultEventResult =
        serde_json::from_str(r#"{"isError":null}"#).unwrap();
    assert_eq!(null_is_error.is_error, OptionalPatch::Null);
    assert!(null_is_error.is_error.is_null());
    assert!(!null_is_error.is_error.is_undefined());
    // `Null` 序列化回显式 null，且不会被 `skip_serializing_if` 省略。
    assert_eq!(
        serde_json::to_string(&null_is_error).unwrap(),
        r#"{"isError":null}"#
    );

    let null_content_and_usage: kiana_ext_api::events::ToolResultEventResult =
        serde_json::from_str(r#"{"content":null,"usage":null}"#).unwrap();
    assert_eq!(null_content_and_usage.content, OptionalPatch::Null);
    assert_eq!(null_content_and_usage.usage, OptionalPatch::Null);
    // 键顺序与 TS 声明顺序一致，null 值照常写出。
    assert_eq!(
        serde_json::to_string(&null_content_and_usage).unwrap(),
        r#"{"content":null,"usage":null}"#
    );

    // `structuredContent: null` 是 JsonValue，保留为 `Value(null)`。
    let null_structured: kiana_ext_api::events::ToolResultEventResult =
        serde_json::from_str(r#"{"structuredContent":null}"#).unwrap();
    assert_eq!(
        null_structured.structured_content,
        OptionalPatch::Value(JsonValue::Null)
    );
}

#[test]
fn extension_event_results_serde() {
    let result = ExtensionEventResult::Context(ContextEventResult {
        messages: Some(vec![agent_message_user("hi")]),
    });
    // ExtensionEventResult 不是 serde 类型（UserBash 变体含 BashOperations）；本测试只确认
    // 可构造并匹配（编译期覆盖）。
    match result {
        ExtensionEventResult::Context(ContextEventResult { messages: Some(m) }) => {
            assert_eq!(m.len(), 1);
        }
        _ => panic!("unexpected variant"),
    }

    let input = InputEventResult::Transform {
        text: "t".into(),
        images: Some(vec![ImageContent {
            kind_image: ImageKind::Image,
            data: "AA==".into(),
            mime_type: "image/png".into(),
        }]),
    };
    assert_eq!(
        serde_json::to_string(&input).unwrap(),
        r#"{"action":"transform","text":"t","images":[{"type":"image","data":"AA==","mimeType":"image/png"}]}"#
    );
    let decoded: InputEventResult = serde_json::from_str(r#"{"action":"handled"}"#).unwrap();
    assert_eq!(decoded, InputEventResult::Handled);
    assert_eq!(
        serde_json::to_string(&InputEventResult::Continue).unwrap(),
        r#"{"action":"continue"}"#
    );

    let end = MessageEndEventResult {
        message: Some(agent_message_user("hi")),
    };
    assert!(
        serde_json::to_string(&end)
            .unwrap()
            .contains(r#""role":"user""#)
    );

    let boundary = BoundaryResult {
        entries: None,
        r#continue: Some(false),
    };
    assert_eq!(
        serde_json::to_string(&boundary).unwrap(),
        r#"{"continue":false}"#
    );
}

// ---------------------------------------------------------------------------
// 41 个事件
// ---------------------------------------------------------------------------

fn all_events() -> Vec<ExtensionEvent> {
    let mut events = Vec::new();

    events.push(ExtensionEvent::ProjectTrust(ProjectTrustEvent {
        event_type: ProjectTrustEventType::ProjectTrust,
        cwd: "/w".into(),
    }));
    events.push(ExtensionEvent::ResourcesDiscover(ResourcesDiscoverEvent {
        event_type: ResourcesDiscoverEventType::ResourcesDiscover,
        cwd: "/w".into(),
        reason: ResourcesDiscoverReason::Reload,
    }));
    let config =
        validate_mcp_server_config("jira", &json!({ "url": "https://mcp.example.com/jira" }))
            .expect("有效 MCP 配置");
    events.push(ExtensionEvent::McpServersChange(McpServersChangeEvent {
        event_type: McpServersChangeEventType::McpServersChange,
        servers: vec![RegisteredMcpServer {
            name: "jira".into(),
            config,
            extension_path: "ext".into(),
        }],
    }));
    events.push(ExtensionEvent::Session(SessionEvent::Start(
        SessionStartEvent {
            event_type: SessionStartEventType::SessionStart,
            reason: SessionStartReason::Resume,
            previous_session_file: Some("/p".into()),
        },
    )));
    events.push(ExtensionEvent::Session(SessionEvent::InfoChanged(
        SessionInfoChangedEvent {
            event_type: SessionInfoChangedEventType::SessionInfoChanged,
            name: Some("name".into()),
        },
    )));
    events.push(ExtensionEvent::Session(SessionEvent::BeforeSwitch(
        SessionBeforeSwitchEvent {
            event_type: SessionBeforeSwitchEventType::SessionBeforeSwitch,
            reason: kiana_ext_api::events::SessionBeforeSwitchReason::New,
            target_session_file: None,
        },
    )));
    events.push(ExtensionEvent::Session(SessionEvent::BeforeFork(
        SessionBeforeForkEvent {
            event_type: SessionBeforeForkEventType::SessionBeforeFork,
            entry_id: "e1".into(),
            position: ForkPosition::Before,
        },
    )));
    events.push(ExtensionEvent::Session(SessionEvent::BeforeCompact(
        SessionBeforeCompactEvent {
            event_type: SessionBeforeCompactEventType::SessionBeforeCompact,
            preparation: preparation(),
            branch_entries: vec![session_entry()],
            custom_instructions: Some("ci".into()),
            reason: CompactionReason::Manual,
            will_retry: true,
            signal: tokio_util::sync::CancellationToken::new(),
        },
    )));
    events.push(ExtensionEvent::Session(SessionEvent::Compact(
        SessionCompactEvent {
            event_type: SessionCompactEventType::SessionCompact,
            compaction_entry: compaction_entry(),
            from_extension: false,
            reason: CompactionReason::Threshold,
            will_retry: false,
        },
    )));
    events.push(ExtensionEvent::Session(SessionEvent::CompactFailed(
        SessionCompactFailedEvent {
            event_type: SessionCompactFailedEventType::SessionCompactFailed,
            reason: CompactionReason::Overflow,
            error_message: Some("boom".into()),
            aborted: false,
            will_retry: true,
            from_extension: true,
        },
    )));
    events.push(ExtensionEvent::Session(SessionEvent::Shutdown(
        SessionShutdownEvent {
            event_type: SessionShutdownEventType::SessionShutdown,
            reason: kiana_ext_api::events::SessionShutdownReason::Quit,
            target_session_file: None,
        },
    )));
    events.push(ExtensionEvent::Session(SessionEvent::BeforeTree(
        SessionBeforeTreeEvent {
            event_type: SessionBeforeTreeEventType::SessionBeforeTree,
            preparation: TreePreparation {
                target_id: "t".into(),
                old_leaf_id: Some("e1".into()),
                common_ancestor_id: None,
                entries_to_summarize: vec![session_entry()],
                user_wants_summary: true,
                custom_instructions: None,
                replace_instructions: Some(false),
                label: Some("l".into()),
            },
            signal: tokio_util::sync::CancellationToken::new(),
        },
    )));
    events.push(ExtensionEvent::Session(SessionEvent::Tree(
        SessionTreeEvent {
            event_type: SessionTreeEventType::SessionTree,
            new_leaf_id: Some("e2".into()),
            old_leaf_id: Some("e1".into()),
            summary_entry: Some(branch_summary_entry()),
            from_extension: Some(true),
        },
    )));
    events.push(ExtensionEvent::Context(ContextEvent {
        event_type: ContextEventType::Context,
        messages: vec![agent_message_user("hi")],
    }));
    events.push(ExtensionEvent::ContextWithSystem(ContextWithSystemEvent {
        event_type: ContextWithSystemEventType::ContextWithSystem,
        messages: vec![agent_message_user("hi")],
    }));
    events.push(ExtensionEvent::CacheWarmingDecision(
        CacheWarmingDecisionEvent {
            warm_cost: 0.5,
            miss_cost: 2.5,
            continuation_probability: 0.25,
            action: CacheWarmingAction::Warm,
        },
    ));
    events.push(ExtensionEvent::BeforeProviderRequest(
        BeforeProviderRequestEvent {
            event_type: BeforeProviderRequestEventType::BeforeProviderRequest,
            payload: json!({ "model": "m" }),
        },
    ));
    let mut headers = IndexMap::new();
    headers.insert("x-a".to_string(), Some("1".to_string()));
    headers.insert("x-del".to_string(), None);
    events.push(ExtensionEvent::BeforeProviderHeaders(
        BeforeProviderHeadersEvent {
            event_type: Default::default(),
            headers,
        },
    ));
    events.push(ExtensionEvent::AfterProviderResponse(
        kiana_ext_api::events::AfterProviderResponseEvent {
            event_type: Default::default(),
            status: 200,
            headers: IndexMap::from([("content-type".to_string(), "application/json".to_string())]),
        },
    ));
    events.push(ExtensionEvent::ProviderStreamEvent(ProviderStreamEvent {
        event_type: ProviderStreamEventType::ProviderStreamEvent,
        provider: "faux".into(),
        api: "faux".into(),
        model: "faux-1".into(),
        data: json!({ "type": "start" }),
    }));
    events.push(ExtensionEvent::BeforeAgentStart(
        BeforeAgentStartEvent::User(BeforeAgentStartUserEvent {
            event_type: BeforeAgentStartEventType::BeforeAgentStart,
            system_prompt: "sp".into(),
            system_prompt_options: normalized_options(),
            message_type: BeforeAgentStartUserMessageType::User,
            prompt: "hi".into(),
            images: None,
        }),
    ));
    events.push(ExtensionEvent::BeforeAgentStart(
        BeforeAgentStartEvent::Custom(BeforeAgentStartCustomEvent {
            event_type: BeforeAgentStartEventType::BeforeAgentStart,
            system_prompt: "sp".into(),
            system_prompt_options: normalized_options(),
            message_type: BeforeAgentStartCustomMessageType::Custom,
            message: custom_message(),
        }),
    ));
    events.push(ExtensionEvent::AgentStart(AgentStartEvent {
        event_type: Default::default(),
    }));
    events.push(ExtensionEvent::AgentEnd(AgentEndEvent {
        event_type: Default::default(),
        messages: vec![agent_message_user("hi")],
    }));
    events.push(ExtensionEvent::AgentBeforeSettle(AgentBeforeSettleEvent {
        entries: vec![SessionBoundaryDraft::Custom(CustomEntryDraft {
            event_type: CustomEntryDraftType::Custom,
            custom_type: "t".into(),
            data: None,
        })],
        r#continue: true,
        context: preview(),
        outcome: AgentActivityOutcome::Completed,
        event_type: Default::default(),
    }));
    events.push(ExtensionEvent::AgentSettled(AgentSettledEvent {
        event_type: Default::default(),
        aborted: false,
    }));
    events.push(ExtensionEvent::UIPromptStart(UIPromptStartEvent {
        event_type: UIPromptStartEventType::UIPromptStart,
        reason: UIPromptReason::UiPrompt,
        kind: UIPromptKind::Select,
        title: Some("Pick".into()),
    }));
    events.push(ExtensionEvent::UIPromptEnd(UIPromptEndEvent {
        event_type: UIPromptEndEventType::UIPromptEnd,
        reason: UIPromptReason::UiPrompt,
        kind: UIPromptKind::Editor,
        title: None,
    }));
    events.push(ExtensionEvent::TurnStart(TurnStartEvent {
        event_type: TurnStartEventType::TurnStart,
        turn_index: 3,
        timestamp: 1_700_000_000_000,
    }));
    events.push(ExtensionEvent::TurnEnd(TurnEndEvent {
        entries: Vec::new(),
        r#continue: false,
        context: preview(),
        outcome: AgentActivityOutcome::Aborted,
        event_type: TurnEndEventType::TurnEnd,
        turn_index: 3,
        message: agent_message_user("hi"),
        tool_results: vec![parse(json!({
            "role": "toolResult",
            "toolCallId": "c1",
            "toolName": "bash",
            "content": [],
            "isError": false,
            "timestamp": 1
        }))],
        message_entry_id: "e1".into(),
        tool_result_entry_ids: vec!["e2".into()],
    }));
    events.push(ExtensionEvent::MessageStart(MessageStartEvent {
        event_type: MessageStartEventType::MessageStart,
        message: agent_message_user("hi"),
    }));
    let assistant = parse::<kiana_ai::types::AssistantMessage>(json!({
        "role": "assistant",
        "content": [],
        "api": "faux",
        "provider": "faux",
        "model": "faux-1",
        "usage": {
            "input": 0,
            "output": 0,
            "cacheRead": 0,
            "cacheWrite": 0,
            "totalTokens": 0,
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 }
        },
        "stopReason": "stop",
        "timestamp": 1
    }));
    events.push(ExtensionEvent::MessageUpdate(MessageUpdateEvent {
        event_type: MessageUpdateEventType::MessageUpdate,
        message: agent_message_user("hi"),
        assistant_message_event: kiana_ai::types::AssistantMessageEvent::Start {
            partial: Box::new(assistant),
        },
    }));
    events.push(ExtensionEvent::MessageEnd(MessageEndEvent {
        event_type: Default::default(),
        message: agent_message_user("hi"),
    }));
    events.push(ExtensionEvent::ToolExecutionStart(
        ToolExecutionStartEvent {
            event_type: ToolExecutionStartEventType::ToolExecutionStart,
            tool_call_id: "c1".into(),
            tool_name: "bash".into(),
            args: json!({ "command": "ls" }),
            parent_tool_call_id: None,
        },
    ));
    events.push(ExtensionEvent::ToolExecutionUpdate(
        ToolExecutionUpdateEvent {
            event_type: ToolExecutionUpdateEventType::ToolExecutionUpdate,
            tool_call_id: "c1".into(),
            tool_name: "bash".into(),
            args: json!({ "command": "ls" }),
            partial_result: json!({ "output": "o" }),
            parent_tool_call_id: Some("parent".into()),
        },
    ));
    events.push(ExtensionEvent::ToolExecutionEnd(ToolExecutionEndEvent {
        event_type: ToolExecutionEndEventType::ToolExecutionEnd,
        tool_call_id: "c1".into(),
        tool_name: "bash".into(),
        result: json!({ "content": [] }),
        is_error: true,
        duration_ms: Some(12),
        parent_tool_call_id: None,
    }));
    events.push(ExtensionEvent::ModelSelect(ModelSelectEvent {
        event_type: ModelSelectEventType::ModelSelect,
        model: model(),
        previous_model: Some(model()),
        source: ModelSelectSource::Cycle,
    }));
    events.push(ExtensionEvent::ThinkingLevelSelect(
        ThinkingLevelSelectEvent {
            event_type: ThinkingLevelSelectEventType::ThinkingLevelSelect,
            level: kiana_ai::types::ModelThinkingLevel::Low,
            previous_level: kiana_ai::types::ModelThinkingLevel::Off,
        },
    ));
    events.push(ExtensionEvent::UserBash(UserBashEvent {
        event_type: UserBashEventType::UserBash,
        command: "ls".into(),
        exclude_from_context: true,
        cwd: "/w".into(),
    }));
    events.push(ExtensionEvent::Input(InputEvent {
        event_type: Default::default(),
        text: "hi".into(),
        images: None,
        source: InputSource::Interactive,
        streaming_behavior: None,
    }));

    // 9 个 tool_call 变体。
    events.push(ExtensionEvent::ToolCall(ToolCallEvent::Bash(
        BashToolCallEvent {
            event_type: Default::default(),
            tool_call_id: "c1".into(),
            parent_tool_call_id: None,
            tool_name: BashToolName::Bash,
            input: BashToolInput {
                command: "ls".into(),
                timeout: None,
            },
        },
    )));
    events.push(ExtensionEvent::ToolCall(ToolCallEvent::PowerShell(
        PowerShellToolCallEvent {
            event_type: Default::default(),
            tool_call_id: "c2".into(),
            parent_tool_call_id: None,
            tool_name: PowerShellToolName::PowerShell,
            input: PowerShellToolInput {
                command: "Get-ChildItem".into(),
                timeout: Some(2.0),
            },
        },
    )));
    events.push(ExtensionEvent::ToolCall(ToolCallEvent::Read(
        ReadToolCallEvent {
            event_type: Default::default(),
            tool_call_id: "c3".into(),
            parent_tool_call_id: None,
            tool_name: ReadToolName::Read,
            input: ReadToolInput {
                path: "/f".into(),
                offset: None,
                limit: Some(5.0),
            },
        },
    )));
    events.push(ExtensionEvent::ToolCall(ToolCallEvent::Edit(
        EditToolCallEvent {
            event_type: Default::default(),
            tool_call_id: "c4".into(),
            parent_tool_call_id: None,
            tool_name: EditToolName::Edit,
            input: EditToolInput {
                path: "/f".into(),
                edits: vec![ReplaceEdit {
                    old_text: "a".into(),
                    new_text: "b".into(),
                }],
            },
        },
    )));
    events.push(ExtensionEvent::ToolCall(ToolCallEvent::Write(
        WriteToolCallEvent {
            event_type: Default::default(),
            tool_call_id: "c5".into(),
            parent_tool_call_id: None,
            tool_name: WriteToolName::Write,
            input: WriteToolInput {
                path: "/f".into(),
                content: "c".into(),
            },
        },
    )));
    events.push(ExtensionEvent::ToolCall(ToolCallEvent::Grep(
        GrepToolCallEvent {
            event_type: Default::default(),
            tool_call_id: "c6".into(),
            parent_tool_call_id: None,
            tool_name: GrepToolName::Grep,
            input: GrepToolInput {
                pattern: "p".into(),
                ..Default::default()
            },
        },
    )));
    events.push(ExtensionEvent::ToolCall(ToolCallEvent::Find(
        FindToolCallEvent {
            event_type: Default::default(),
            tool_call_id: "c7".into(),
            parent_tool_call_id: None,
            tool_name: FindToolName::Find,
            input: FindToolInput {
                pattern: "*.rs".into(),
                ..Default::default()
            },
        },
    )));
    events.push(ExtensionEvent::ToolCall(ToolCallEvent::Ls(
        LsToolCallEvent {
            event_type: Default::default(),
            tool_call_id: "c8".into(),
            parent_tool_call_id: None,
            tool_name: LsToolName::Ls,
            input: LsToolInput::default(),
        },
    )));
    events.push(ExtensionEvent::ToolCall(ToolCallEvent::Custom(
        CustomToolCallEvent {
            event_type: Default::default(),
            tool_call_id: "c9".into(),
            parent_tool_call_id: None,
            tool_name: "my_tool".into(),
            input: IndexMap::from([("action".to_string(), json!("do"))]),
        },
    )));

    // 9 个 tool_result 变体。
    events.push(ExtensionEvent::ToolResult(ToolResultEvent::Bash(
        BashToolResultEvent {
            event_type: Default::default(),
            tool_call_id: "r-bash".into(),
            parent_tool_call_id: None,
            input: tool_result_input(),
            content: vec![UserContentBlock::Text(kiana_ai::types::TextContent {
                kind_text: Default::default(),
                text: "ok".into(),
                text_signature: None,
            })],
            structured_content: Some(json!({ "ok": true })),
            is_error: false,
            usage: Some(Usage::default()),
            tool_name: BashToolName::Bash,
            details: Some(BashToolDetails::default()),
        },
    )));
    events.push(ExtensionEvent::ToolResult(ToolResultEvent::PowerShell(
        PowerShellToolResultEvent {
            event_type: Default::default(),
            tool_call_id: "r-ps".into(),
            parent_tool_call_id: None,
            input: tool_result_input(),
            content: Vec::new(),
            structured_content: None,
            is_error: false,
            usage: None,
            tool_name: PowerShellToolName::PowerShell,
            details: Some(PowerShellToolDetails::default()),
        },
    )));
    events.push(ExtensionEvent::ToolResult(ToolResultEvent::Read(
        ReadToolResultEvent {
            event_type: Default::default(),
            tool_call_id: "r-read".into(),
            parent_tool_call_id: None,
            input: tool_result_input(),
            content: Vec::new(),
            structured_content: None,
            is_error: false,
            usage: None,
            tool_name: ReadToolName::Read,
            details: Some(ReadToolDetails::default()),
        },
    )));
    events.push(ExtensionEvent::ToolResult(ToolResultEvent::Edit(
        EditToolResultEvent {
            event_type: Default::default(),
            tool_call_id: "r-edit".into(),
            parent_tool_call_id: None,
            input: tool_result_input(),
            content: Vec::new(),
            structured_content: None,
            is_error: true,
            usage: None,
            tool_name: EditToolName::Edit,
            details: Some(EditToolDetails::default()),
        },
    )));
    events.push(ExtensionEvent::ToolResult(ToolResultEvent::Write(
        WriteToolResultEvent {
            event_type: Default::default(),
            tool_call_id: "r-write".into(),
            parent_tool_call_id: None,
            input: tool_result_input(),
            content: Vec::new(),
            structured_content: None,
            is_error: false,
            usage: None,
            tool_name: WriteToolName::Write,
        },
    )));
    events.push(ExtensionEvent::ToolResult(ToolResultEvent::Grep(
        GrepToolResultEvent {
            event_type: Default::default(),
            tool_call_id: "r-grep".into(),
            parent_tool_call_id: None,
            input: tool_result_input(),
            content: Vec::new(),
            structured_content: None,
            is_error: false,
            usage: None,
            tool_name: GrepToolName::Grep,
            details: Some(GrepToolDetails::default()),
        },
    )));
    events.push(ExtensionEvent::ToolResult(ToolResultEvent::Find(
        FindToolResultEvent {
            event_type: Default::default(),
            tool_call_id: "r-find".into(),
            parent_tool_call_id: None,
            input: tool_result_input(),
            content: Vec::new(),
            structured_content: None,
            is_error: false,
            usage: None,
            tool_name: FindToolName::Find,
            details: Some(FindToolDetails::default()),
        },
    )));
    events.push(ExtensionEvent::ToolResult(ToolResultEvent::Ls(
        LsToolResultEvent {
            event_type: Default::default(),
            tool_call_id: "r-ls".into(),
            parent_tool_call_id: None,
            input: tool_result_input(),
            content: Vec::new(),
            structured_content: None,
            is_error: false,
            usage: None,
            tool_name: LsToolName::Ls,
            details: Some(LsToolDetails::default()),
        },
    )));
    events.push(ExtensionEvent::ToolResult(ToolResultEvent::Custom(
        CustomToolResultEvent {
            event_type: Default::default(),
            tool_call_id: "r-custom".into(),
            parent_tool_call_id: None,
            input: tool_result_input(),
            content: Vec::new(),
            structured_content: None,
            is_error: false,
            usage: None,
            tool_name: "my_tool".into(),
            details: Some(json!({ "x": 1 })),
        },
    )));

    events
}

#[test]
fn all_extension_event_names_round_trip() {
    let events = all_events();
    // 41 个 on() 名字：SessionEvent 的 10 个、tool_call/tool_result 各覆盖全部 9 个变体、
    // before_agent_start 覆盖 user/custom 两个变体。
    let mut kinds: Vec<ExtensionEventKind> = events.iter().map(ExtensionEvent::kind).collect();
    kinds.sort();
    kinds.dedup();
    assert_eq!(kinds.len(), 41, "必须覆盖全部 41 个事件名");

    for event in &events {
        round_trip(event);
    }
}

#[test]
fn before_agent_start_serializes_both_message_types() {
    let events = all_events();
    let mut user = 0;
    let mut custom = 0;
    for event in &events {
        if let ExtensionEvent::BeforeAgentStart(value) = event {
            match value {
                BeforeAgentStartEvent::User(_) => user += 1,
                BeforeAgentStartEvent::Custom(_) => custom += 1,
            }
        }
    }
    assert_eq!((user, custom), (1, 1));
}

#[test]
fn before_compact_and_tree_omit_signal_and_deserialize_uncancelled() {
    for event in all_events() {
        let is_compact = matches!(
            &event,
            ExtensionEvent::Session(SessionEvent::BeforeCompact(_))
        );
        let is_tree = matches!(&event, ExtensionEvent::Session(SessionEvent::BeforeTree(_)));
        if !is_compact && !is_tree {
            continue;
        }
        let text = serde_json::to_string(&event).unwrap();
        assert!(!text.contains("signal"), "serialized JSON 不应包含 signal");
        let decoded: ExtensionEvent = serde_json::from_str(&text).unwrap();
        match decoded {
            ExtensionEvent::Session(SessionEvent::BeforeCompact(value)) => {
                assert!(!value.signal.is_cancelled());
            }
            ExtensionEvent::Session(SessionEvent::BeforeTree(value)) => {
                assert!(!value.signal.is_cancelled());
            }
            _ => panic!("unexpected event"),
        }
    }
}

#[test]
fn ui_prompt_reason_is_ui_prompt_literal() {
    let start = &all_events()[26];
    // 顺序见 all_events：UIPromptStart 是第 27 个压入的事件（0 起）。
    assert_eq!(
        serde_json::to_string(start).unwrap(),
        r#"{"type":"ui_prompt_start","reason":"ui_prompt","kind":"select","title":"Pick"}"#
    );
}

#[test]
fn before_agent_start_user_exact_shape() {
    let event =
        ExtensionEvent::BeforeAgentStart(BeforeAgentStartEvent::User(BeforeAgentStartUserEvent {
            event_type: BeforeAgentStartEventType::BeforeAgentStart,
            system_prompt: "sp".into(),
            system_prompt_options: normalized_options(),
            message_type: BeforeAgentStartUserMessageType::User,
            prompt: "hi".into(),
            images: None,
        }));
    assert_eq!(
        serde_json::to_string(&event).unwrap(),
        r#"{"type":"before_agent_start","systemPrompt":"sp","systemPromptOptions":{"selectedTools":[],"hiddenTools":[],"toolSnippets":{},"toolGuidelines":{},"promptGuidelines":[],"appendSystemPrompt":"","sections":{},"cwd":"","contextFiles":[],"skills":[]},"messageType":"user","prompt":"hi"}"#
    );
}

#[test]
fn bash_tool_call_exact_shape() {
    let event = ExtensionEvent::ToolCall(ToolCallEvent::Bash(BashToolCallEvent {
        event_type: Default::default(),
        tool_call_id: "c1".into(),
        parent_tool_call_id: None,
        tool_name: BashToolName::Bash,
        input: BashToolInput {
            command: "ls".into(),
            timeout: None,
        },
    }));
    assert_eq!(
        serde_json::to_string(&event).unwrap(),
        r#"{"type":"tool_call","toolCallId":"c1","toolName":"bash","input":{"command":"ls"}}"#
    );
}

#[test]
fn bash_tool_result_exact_shape() {
    let event = ExtensionEvent::ToolResult(ToolResultEvent::Bash(BashToolResultEvent {
        event_type: Default::default(),
        tool_call_id: "c1".into(),
        parent_tool_call_id: None,
        input: tool_result_input(),
        content: Vec::new(),
        structured_content: None,
        is_error: false,
        usage: None,
        tool_name: BashToolName::Bash,
        details: None,
    }));
    assert_eq!(
        serde_json::to_string(&event).unwrap(),
        r#"{"type":"tool_result","toolCallId":"c1","input":{},"content":[],"isError":false,"toolName":"bash"}"#
    );
}

#[test]
fn cache_warming_event_exact_shape() {
    let event = ExtensionEvent::CacheWarmingDecision(CacheWarmingDecisionEvent {
        warm_cost: 0.5,
        miss_cost: 2.5,
        continuation_probability: 0.25,
        action: CacheWarmingAction::Stop,
    });
    assert_eq!(
        serde_json::to_string(&event).unwrap(),
        r#"{"type":"cache_warming_decision","warmCost":0.5,"missCost":2.5,"continuationProbability":0.25,"action":"stop"}"#
    );
    let decoded: ExtensionEvent =
        serde_json::from_str(&serde_json::to_string(&event).unwrap()).unwrap();
    match decoded {
        ExtensionEvent::CacheWarmingDecision(value) => {
            assert_eq!(value.action, CacheWarmingAction::Stop);
        }
        _ => panic!("unexpected variant"),
    }
}

#[test]
fn session_event_union_round_trips_standalone() {
    let value = SessionEvent::Compact(SessionCompactEvent {
        event_type: SessionCompactEventType::SessionCompact,
        compaction_entry: compaction_entry(),
        from_extension: true,
        reason: CompactionReason::Manual,
        will_retry: false,
    });
    let text = serde_json::to_string(&value).unwrap();
    let decoded: SessionEvent = serde_json::from_str(&text).unwrap();
    assert_eq!(decoded.kind(), ExtensionEventKind::SessionCompact);
    assert_eq!(serde_json::to_string(&decoded).unwrap(), text);
}

#[test]
fn unknown_event_type_is_an_error() {
    let result: Result<ExtensionEvent, _> = serde_json::from_str(r#"{"type":"nope"}"#);
    let error = result.expect_err("未知事件名必须报错").to_string();
    assert!(
        error.contains("unknown extension event type: nope"),
        "{error}"
    );
}

#[test]
fn unknown_tool_name_deserializes_to_custom() {
    let json = r#"{"type":"tool_call","toolCallId":"c1","toolName":"zzz","input":{}}"#;
    let event: ExtensionEvent = serde_json::from_str(json).unwrap();
    match event {
        ExtensionEvent::ToolCall(ToolCallEvent::Custom(value)) => {
            assert_eq!(value.tool_name, "zzz");
        }
        _ => panic!("未知 toolName 必须落到 Custom"),
    }

    let json = r#"{"type":"tool_result","toolCallId":"c1","toolName":"zzz","input":{},"content":[],"isError":false,"details":null}"#;
    let event: ExtensionEvent = serde_json::from_str(json).unwrap();
    match event {
        ExtensionEvent::ToolResult(ToolResultEvent::Custom(value)) => {
            assert_eq!(value.tool_name, "zzz");
        }
        _ => panic!("未知 toolName 必须落到 Custom"),
    }
}

#[test]
fn custom_tool_result_details_is_an_optional_key() {
    // TS: `CustomToolResultEvent.details` 是 `unknown`，事件用 `details: result.details`
    // 构造；没有结构化 details 的工具给 `undefined`，`JSON.stringify` 省略键。
    // 因此缺 `details` 的 payload 必须能反序列化（曾经因必填 `JsonValue` 报 missing field），
    // 且 `None` 序列化时不写出该键。
    let json = r#"{"type":"tool_result","toolCallId":"c1","toolName":"zzz","input":{},"content":[],"isError":false}"#;
    let event: ExtensionEvent = serde_json::from_str(json).unwrap();
    match event {
        ExtensionEvent::ToolResult(ToolResultEvent::Custom(value)) => {
            assert_eq!(value.details, None);
            assert_eq!(
                serde_json::to_string(&ToolResultEvent::Custom(value)).unwrap(),
                r#"{"type":"tool_result","toolCallId":"c1","input":{},"content":[],"isError":false,"toolName":"zzz"}"#
            );
        }
        _ => panic!("未知 toolName 必须落到 Custom"),
    }

    // 有 details 时按键写出（键序在 `toolName` 之后）。
    let with_details = CustomToolResultEvent {
        event_type: Default::default(),
        tool_call_id: "c2".into(),
        parent_tool_call_id: None,
        input: IndexMap::new(),
        content: Vec::new(),
        structured_content: None,
        is_error: false,
        usage: None,
        tool_name: "zzz".into(),
        details: Some(json!({ "x": 1 })),
    };
    assert_eq!(
        serde_json::to_string(&with_details).unwrap(),
        r#"{"type":"tool_result","toolCallId":"c2","input":{},"content":[],"isError":false,"toolName":"zzz","details":{"x":1}}"#
    );
}

#[test]
fn extra_types_compile_and_round_trip() {
    // 本测试覆盖 `all_events` 未直接用到的类型（防止名字存在但无法构造）。
    let _ = AgentToolResult::<JsonValue> {
        content: Vec::new(),
        details: JsonValue::Null,
        structured_content: None,
        usage: None,
        is_error: None,
        terminate: None,
    };
    let _: Option<AgentToolUpdateCallback> = None;
    let result = CompactionResult {
        summary: "s".into(),
        first_kept_entry_id: "e1".into(),
        tokens_before: 1,
        estimated_tokens_after: None,
        usage: None,
        details: None,
    };
    assert!(
        serde_json::to_string(&result)
            .unwrap()
            .contains(r#""summary":"s""#)
    );
}
