//! 移植 `packages/coding-agent/test/bug-report.test.ts` 中属于本单元的测试
//! （`bug report redaction` 两例），并补充 `bugReportFiles` / archive / diagnostics 的
//! 契约测试（TS 无对应测试，见 `ca-resources.md` §7.4）。

use kiana_agent_core::types::AgentMessage;
use kiana_ai::types::{
    AssistantMessage, Message, Model, ModelInputModality, StopReason, Usage, UsageCost,
};
use kiana_core::source_info::{
    SourceScope, SyntheticSourceInfoOptions, create_synthetic_source_info,
};
use kiana_resources::bug_report::{
    BugReportBundle, BugReportDelivery, BugReportSessionEntryData, UploadBugReportOptions,
    bug_report_archive_file_name, bug_report_files, collect_bug_report_diagnostics,
    collect_environment, describe_extension, describe_model, is_sensitive_key, redact_json_value,
    redact_url, select_messages, upload_bug_report, write_bug_report_archive,
};
use kiana_resources::crash_log::CrashRecord;
use kiana_session_core::session_manager::SessionManager;
use serde_json::json;

#[test]
fn removes_url_credentials_and_secret_query_parameters() {
    assert_eq!(
        redact_url("https://user:pass@proxy.example.com:8080/"),
        "https://proxy.example.com:8080/"
    );
    assert_eq!(
        redact_url("git:https://pat@github.com/org/repo"),
        "git:https://github.com/org/repo"
    );
    assert_eq!(
        redact_url("https://api.example/v1?api-key=abc&model=x"),
        "https://api.example/v1?api-key=%3Credacted%3E&model=x"
    );
}

#[test]
fn redacts_nested_secret_values_without_hiding_token_counts() {
    let value = json!({
        "apiKey": "sk-123",
        "headers": { "Authorization": "Bearer x", "X-Trace": "1" },
        "compaction": { "reserveTokens": 16_384, "keepRecentTokens": 20_000 },
        "baseUrl": "https://me:secret@example.com/"
    });
    assert_eq!(
        redact_json_value(&value),
        json!({
            "apiKey": "<redacted>",
            "headers": { "Authorization": "<redacted>", "X-Trace": "1" },
            "compaction": { "reserveTokens": 16_384, "keepRecentTokens": 20_000 },
            "baseUrl": "https://example.com/"
        })
    );
}

#[test]
fn sensitive_key_matches_camel_case_and_separator_boundaries() {
    for key in [
        "apiKey",
        "api-key",
        "api_key",
        "apikey",
        "my_secret_x",
        "token",
        "accessToken",
        "PASSWORD",
        "authorization",
        "cookie",
    ] {
        assert!(is_sensitive_key(key), "{key} should be sensitive");
    }
    for key in [
        "mysecrets",
        "foo.token",
        "tokenizer",
        "monkey",
        "compactionTokens",
        "maxTokens",
        "reserveTokens",
    ] {
        assert!(!is_sensitive_key(key), "{key} should not be sensitive");
    }
}

#[test]
fn preserves_unchanged_urls_verbatim() {
    assert_eq!(
        redact_url("https://api.example/v1?model=x"),
        "https://api.example/v1?model=x"
    );
    assert_eq!(redact_url("not a url"), "not a url");
    // 未变化的 URL 不重新序列化（大小写/端口写法保持原样）。
    assert_eq!(
        redact_url("HTTPS://API.EXAMPLE:443/x"),
        "HTTPS://API.EXAMPLE:443/x"
    );
}

#[test]
fn redact_url_serializes_file_and_non_special_urls_like_whatwg() {
    // 期望值由 node 26 的 `new URL(v).toString()` + TS `redactUrl` 生成。
    // url crate 的差异：file URL 丢 host、盘符 `|` 不归一、前导空段被折叠；
    // 非 special scheme 的 authority 里出现 `\` 时 `new URL` 抛错。
    assert_eq!(
        redact_url("file://host/C:/x?token=1"),
        "file://host/C:/x?token=%3Credacted%3E"
    );
    assert_eq!(
        redact_url("file://127.0.0.1/C:/x?token=1"),
        "file://127.0.0.1/C:/x?token=%3Credacted%3E"
    );
    assert_eq!(
        redact_url("file:///C|/x?token=1"),
        "file:///C:/x?token=%3Credacted%3E"
    );
    assert_eq!(
        redact_url("file://[::1]//a?token=1"),
        "file://[::1]//a?token=%3Credacted%3E"
    );
    assert_eq!(
        redact_url("git:/foo:?token=1"),
        "git:/foo:?token=%3Credacted%3E"
    );
    // WHATWG 拒绝（非 special scheme 的 authority 里 `\` 不是分隔符），原样返回。
    for input in [
        "ssh://host:2222\\a\\b?token=1",
        "ssh://host:22\\x?token=1",
        "ssh://host\\a?token=1",
    ] {
        assert_eq!(redact_url(input), input, "input: {input}");
    }
}

#[test]
fn redact_url_matches_the_whatwg_serialization_fixture() {
    // 期望值由 node 26 的 `new URL()` + TS `redactUrl` 生成（见
    // tests/fixtures/bug_report/redact_url_golden.json）。
    let data = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/bug_report/redact_url_golden.json"
    ));
    let cases: Vec<(String, String)> = serde_json::from_str(data).expect("parse fixture");
    assert!(!cases.is_empty());
    let mut failures = Vec::new();
    for (input, expected) in &cases {
        let actual = redact_url(input);
        if &actual != expected {
            failures.push(format!("{input:?}: expected {expected:?}, got {actual:?}"));
        }
    }
    assert!(
        failures.is_empty(),
        "{} mismatches:\n{}",
        failures.len(),
        failures
            .iter()
            .take(40)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn redact_url_preserves_inputs_that_whatwg_new_url_rejects() {
    // `new URL()` 对「空 host + userinfo」的非 special scheme 抛 `Invalid URL`，
    // TS 的 catch 原样返回输入；`url` crate 会丢弃空 userinfo 后照常解析。
    // 期望值由 node 26 的 `new URL(v)`（抛错）+ TS `redactUrl` 生成。
    for input in [
        "ssh://:@?token=1",
        "Foo://:@?api_key=x",
        "ssh://@?token=1",
        "ssh://:@#frag",
        "git+ssh://:@?token=1",
    ] {
        assert_eq!(redact_url(input), input, "input: {input}");
    }
    // 有 host 的同类输入仍然正常脱敏。
    assert_eq!(
        redact_url("ssh://user:pass@host/path?token=1"),
        "ssh://host/path?token=%3Credacted%3E"
    );
}

#[test]
fn bug_report_files_have_fixed_order_and_content() {
    let bundle = BugReportBundle {
        metadata: json!({ "schemaVersion": 1, "id": "abc" }),
        diagnostics: json!({ "schemaVersion": 1, "assistant": [] }),
        session_jsonl: Some("{\"type\":\"message\"}\n".to_string()),
        summary: Some("Summary without newline".to_string()),
    };
    let files = bug_report_files(&bundle);
    assert_eq!(
        files
            .iter()
            .map(|file| (file.name.as_str(), file.content_type.as_str()))
            .collect::<Vec<_>>(),
        vec![
            ("report.json", "application/json"),
            ("diagnostics.json", "application/json"),
            ("session.jsonl", "application/x-ndjson"),
            ("summary.md", "text/markdown"),
        ]
    );
    assert_eq!(
        files[0].data,
        "{\n  \"schemaVersion\": 1,\n  \"id\": \"abc\"\n}\n"
    );
    assert_eq!(
        files[1].data,
        "{\n  \"schemaVersion\": 1,\n  \"assistant\": []\n}\n"
    );
    assert_eq!(files[2].data, "{\"type\":\"message\"}\n");
    assert_eq!(files[3].data, "Summary without newline\n");

    let with_newline = BugReportBundle {
        metadata: json!({ "schemaVersion": 1 }),
        diagnostics: json!({ "schemaVersion": 1 }),
        session_jsonl: None,
        summary: Some("Ends with newline\n".to_string()),
    };
    assert_eq!(
        bug_report_files(&with_newline)[2].data,
        "Ends with newline\n"
    );
}

#[test]
fn archive_name_and_archive_writing() {
    assert_eq!(
        bug_report_archive_file_name("abc"),
        "kiana-bug-report-abc.zip"
    );
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("report.zip");
    let bundle = BugReportBundle {
        metadata: json!({ "schemaVersion": 1 }),
        diagnostics: json!({ "schemaVersion": 1 }),
        session_jsonl: None,
        summary: None,
    };
    write_bug_report_archive(&bundle, &path).expect("write archive");
    let bytes = std::fs::read(&path).expect("read archive");
    assert!(bytes.starts_with(b"PK\x03\x04"));
}

#[test]
fn collect_environment_matches_the_ts_shape() {
    let environment = collect_environment();
    let object = environment.as_object().expect("environment object");
    assert_eq!(
        object.keys().map(String::as_str).collect::<Vec<_>>(),
        vec![
            "version",
            "userAgent",
            "runtime",
            "platform",
            "arch",
            "osRelease",
            "osVersion",
            "shell",
            "terminal",
            "kianaEnvironmentVariables",
        ]
    );
    assert_eq!(
        object.get("version").and_then(|value| value.as_str()),
        Some(kiana_core::config::VERSION)
    );
    assert!(
        object
            .get("userAgent")
            .and_then(|value| value.as_str())
            .is_some_and(|agent| agent.starts_with("kiana/1.4.0"))
    );
    assert!(
        object
            .get("runtime")
            .and_then(|value| value.as_str())
            .is_some_and(|runtime| runtime.starts_with("node/") || runtime.starts_with("bun/"))
    );
    let terminal = object
        .get("terminal")
        .and_then(|value| value.as_object())
        .expect("terminal");
    assert_eq!(
        terminal.keys().map(String::as_str).collect::<Vec<_>>(),
        vec![
            "term",
            "program",
            "programVersion",
            "colorterm",
            "tmux",
            "ssh",
            "ci"
        ]
    );
    let names = object
        .get("kianaEnvironmentVariables")
        .and_then(|value| value.as_array())
        .expect("KIANA names");
    for name in names {
        assert!(name.as_str().unwrap_or_default().starts_with("KIANA_"));
    }
    // 只含名字、不含值；按 UTF-16 code unit 升序。
    let names: Vec<&str> = names.iter().filter_map(|name| name.as_str()).collect();
    let mut sorted = names.clone();
    sorted.sort_by(|left, right| left.encode_utf16().cmp(right.encode_utf16()));
    assert_eq!(names, sorted);
}

fn test_model() -> Model {
    Model {
        id: "claude-sonnet-4-5".to_string(),
        name: "Claude Sonnet 4.5".to_string(),
        api: "anthropic-messages".to_string(),
        provider: "anthropic".to_string(),
        base_url: "https://user:pass@api.anthropic.com?api_key=secret&model=x".to_string(),
        input: vec![ModelInputModality::Text, ModelInputModality::Image],
        cost: Default::default(),
        headers: Some(
            [
                ("Z-Header".to_string(), "1".to_string()),
                ("A-Header".to_string(), "2".to_string()),
            ]
            .into_iter()
            .collect(),
        ),
        reasoning: Some(true),
        context_window: Some(200_000),
        max_tokens: Some(8192),
        ..Default::default()
    }
}

#[test]
fn describe_model_matches_the_ts_field_order_and_redaction() {
    let value = describe_model(&test_model());
    let object = value.as_object().expect("model object");
    assert_eq!(
        object.keys().map(String::as_str).collect::<Vec<_>>(),
        vec![
            "provider",
            "id",
            "name",
            "api",
            "baseUrl",
            "reasoning",
            "input",
            "contextWindow",
            "maxTokens",
            "samplingParams",
            "compat",
            "thinkingLevelMap",
            "headerNames",
        ]
    );
    assert_eq!(
        object.get("baseUrl").and_then(|value| value.as_str()),
        Some("https://api.anthropic.com/?api_key=%3Credacted%3E&model=x")
    );
    assert_eq!(
        object.get("headerNames").cloned(),
        Some(json!(["A-Header", "Z-Header"]))
    );
    assert_eq!(object.get("samplingParams"), Some(&serde_json::Value::Null));
    assert_eq!(object.get("compat"), Some(&serde_json::Value::Null));
    assert_eq!(
        object.get("thinkingLevelMap"),
        Some(&serde_json::Value::Null)
    );
}

#[test]
fn describe_extension_uses_redacted_source_and_origin_literals() {
    let extension = kiana_ext_api::types::Extension {
        path: "builtin:memory".to_string(),
        resolved_path: "builtin:memory".to_string(),
        hidden: Some(true),
        replaceable: None,
        source_info: create_synthetic_source_info(
            "builtin:memory",
            SyntheticSourceInfoOptions {
                source: "git:https://pat@example.com/x".to_string(),
                scope: Some(SourceScope::User),
                origin: Some(kiana_core::source_info::SourceOrigin::Package),
                base_dir: None,
            },
        ),
        handlers: Default::default(),
        tools: Default::default(),
        message_renderers: Default::default(),
        tool_renderers: None,
        markdown_transformer: None,
        entry_renderers: None,
        commands: Default::default(),
        flags: Default::default(),
        shortcuts: Default::default(),
    };
    let value = describe_extension(&extension);
    assert_eq!(
        value,
        json!({
            "path": "builtin:memory",
            "source": "git:https://example.com/x",
            "scope": "user",
            "origin": "package",
            "hidden": true
        })
    );
}

#[test]
fn select_messages_keeps_at_least_one_and_prefers_the_newest() {
    let big = "x".repeat(400); // 100 tokens
    let messages = vec![
        AgentMessage::Custom(Box::new(
            [
                ("role".to_string(), json!("user")),
                ("content".to_string(), json!(big)),
            ]
            .into_iter()
            .collect(),
        )),
        AgentMessage::Custom(Box::new(
            [
                ("role".to_string(), json!("user")),
                ("content".to_string(), json!("tiny")),
            ]
            .into_iter()
            .collect(),
        )),
    ];
    let selected = select_messages(&messages, 1.0);
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0], messages[1]);
}

#[test]
fn collect_diagnostics_lists_failed_assistant_turns_and_strips_notified() {
    let mut manager = SessionManager::in_memory("/tmp", None, None).expect("manager");
    let assistant = AssistantMessage {
        role: kiana_ai::types::AssistantRole::Assistant,
        content: vec![],
        api: "anthropic-messages".to_string(),
        provider: "anthropic".to_string(),
        model: "claude".to_string(),
        usage: Usage {
            cost: UsageCost::default(),
            ..Usage::default()
        },
        stop_reason: StopReason::Error,
        error_message: Some("boom".to_string()),
        timestamp: 1,
        ..AssistantMessage::default()
    };
    manager
        .append_message(AgentMessage::Standard(Message::Assistant(assistant)))
        .expect("append");
    // 一条成功的 assistant turn 不应出现。
    let ok = AssistantMessage {
        role: kiana_ai::types::AssistantRole::Assistant,
        content: vec![],
        api: "anthropic-messages".to_string(),
        provider: "anthropic".to_string(),
        model: "claude".to_string(),
        usage: Usage {
            cost: UsageCost::default(),
            ..Usage::default()
        },
        stop_reason: StopReason::Stop,
        timestamp: 2,
        ..AssistantMessage::default()
    };
    manager
        .append_message(AgentMessage::Standard(Message::Assistant(ok)))
        .expect("append");

    let crash = CrashRecord::from_map(
        [
            ("timestamp".to_string(), json!("2026-01-01T00:00:00.000Z")),
            ("message".to_string(), json!("crash")),
            ("notified".to_string(), json!(true)),
        ]
        .into_iter()
        .collect(),
    );
    let diagnostics = collect_bug_report_diagnostics(&manager, &[crash]);
    let object = diagnostics.as_object().expect("object");
    assert_eq!(
        object.keys().map(String::as_str).collect::<Vec<_>>(),
        vec![
            "schemaVersion",
            "sessionId",
            "entryCount",
            "assistantMessageCount",
            "assistant",
            "crashes",
        ]
    );
    assert_eq!(object.get("assistantMessageCount"), Some(&json!(2)));
    let assistant = object
        .get("assistant")
        .and_then(|value| value.as_array())
        .expect("assistant");
    assert_eq!(assistant.len(), 1);
    assert_eq!(assistant[0].get("errorMessage"), Some(&json!("boom")));
    assert_eq!(assistant[0].get("stopReason"), Some(&json!("error")));
    let crashes = object
        .get("crashes")
        .and_then(|value| value.as_array())
        .expect("crashes");
    assert_eq!(
        crashes[0],
        json!({ "timestamp": "2026-01-01T00:00:00.000Z", "message": "crash" })
    );
}

#[tokio::test]
async fn collect_metadata_matches_the_ts_key_order_and_conditional_cwd() {
    use kiana_config::auth::storage::AuthStorage;
    use kiana_config::models::runtime::{
        CreateModelRuntimeOptions, ModelRuntime, ModelsPathOption,
    };
    use kiana_config::settings::Settings;
    use kiana_resources::bug_report::{
        CollectBugReportMetadataOptions, collect_bug_report_metadata,
    };

    let temp = tempfile::tempdir().expect("tempdir");
    let runtime = ModelRuntime::create(CreateModelRuntimeOptions {
        credentials: Some(std::sync::Arc::new(AuthStorage::create(
            temp.path().join("auth.json").to_str().expect("path"),
        ))),
        models_path: ModelsPathOption::Disabled,
        allow_model_network: false,
        refresh_on_create: Some(false),
        ..Default::default()
    })
    .await
    .expect("runtime");
    let model = runtime.get_model("anthropic", "claude-sonnet-4-5");

    let settings = Settings::default();
    let metadata = collect_bug_report_metadata(&CollectBugReportMetadataOptions {
        id: Some("fixed-id".to_string()),
        hint: Some("  hello  ".to_string()),
        session_id: "session-1".to_string(),
        cwd: "/tmp/project".to_string(),
        include_session: true,
        include_summary: false,
        message_count: 3,
        model: model.as_ref(),
        model_runtime: &runtime,
        thinking_level: kiana_agent_core::types::ThinkingLevel::Off,
        extensions: &[],
        extension_errors: &[],
        global_settings: &settings,
        project_settings: &settings,
    });
    let object = metadata.as_object().expect("metadata");
    assert_eq!(
        object.keys().map(String::as_str).collect::<Vec<_>>(),
        vec![
            "schemaVersion",
            "id",
            "createdAt",
            "hint",
            "environment",
            "session",
            "model",
            "provider",
            "thinkingLevel",
            "extensions",
            "extensionErrors",
            "settings",
        ]
    );
    assert_eq!(object.get("id"), Some(&json!("fixed-id")));
    assert_eq!(object.get("hint"), Some(&json!("hello")));
    assert_eq!(object.get("thinkingLevel"), Some(&json!("off")));
    let created_at = object
        .get("createdAt")
        .and_then(|value| value.as_str())
        .expect("createdAt");
    assert!(created_at.ends_with('Z'));
    assert_eq!(created_at.len(), "2026-01-01T00:00:00.000Z".len());
    let session = object
        .get("session")
        .and_then(|value| value.as_object())
        .expect("session");
    assert_eq!(
        session.keys().map(String::as_str).collect::<Vec<_>>(),
        vec!["id", "included", "summaryIncluded", "messageCount", "cwd"]
    );
    assert_eq!(session.get("cwd"), Some(&json!("/tmp/project")));
    if model.is_some() {
        assert!(object.get("model").is_some_and(|value| value.is_object()));
        assert!(
            object
                .get("provider")
                .is_some_and(|value| value.is_object())
        );
    }
    let settings = object
        .get("settings")
        .and_then(|value| value.as_object())
        .expect("settings");
    assert_eq!(
        settings.keys().map(String::as_str).collect::<Vec<_>>(),
        vec!["global", "project"]
    );

    let without_session = collect_bug_report_metadata(&CollectBugReportMetadataOptions {
        id: Some("fixed-id".to_string()),
        hint: None,
        session_id: "session-1".to_string(),
        cwd: "/tmp/project".to_string(),
        include_session: false,
        include_summary: true,
        message_count: 3,
        model: None,
        model_runtime: &runtime,
        thinking_level: kiana_agent_core::types::ThinkingLevel::Off,
        extensions: &[],
        extension_errors: &[],
        global_settings: &settings_owned(),
        project_settings: &settings_owned(),
    });
    let session = without_session
        .get("session")
        .and_then(|value| value.as_object())
        .expect("session");
    assert!(!session.contains_key("cwd"));
    assert_eq!(without_session.get("hint"), Some(&serde_json::Value::Null));
    assert_eq!(without_session.get("model"), Some(&serde_json::Value::Null));
    assert_eq!(
        without_session.get("provider"),
        Some(&serde_json::Value::Null)
    );
}

fn settings_owned() -> kiana_config::settings::Settings {
    kiana_config::settings::Settings::default()
}

// ---------------------------------------------------------------------------
// generateBugReportSummary（用 fake StreamFn；TS 无对应测试）
// ---------------------------------------------------------------------------

type CapturedContext = std::sync::Arc<std::sync::Mutex<Option<kiana_ai::types::TranscriptContext>>>;
type CapturedOptions = std::sync::Arc<std::sync::Mutex<Option<(Option<u64>, Option<String>)>>>;

fn fake_stream(
    message: kiana_ai::types::AssistantMessage,
) -> (
    kiana_agent_core::types::StreamFn,
    CapturedContext,
    CapturedOptions,
) {
    let captured_context: CapturedContext = std::sync::Arc::new(std::sync::Mutex::new(None));
    let captured_options: CapturedOptions = std::sync::Arc::new(std::sync::Mutex::new(None));
    let context_slot = std::sync::Arc::clone(&captured_context);
    let options_slot = std::sync::Arc::clone(&captured_options);
    let stream_fn: kiana_agent_core::types::StreamFn =
        std::sync::Arc::new(move |_model, context, options| {
            *context_slot
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(context);
            *options_slot
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) =
                options.map(|options| (options.base.max_tokens, options.base.session_id));
            let message = message.clone();
            Box::pin(async move {
                let stream = kiana_ai::event_stream::create_assistant_message_event_stream();
                stream.end(message);
                Ok(stream)
            })
        });
    (stream_fn, captured_context, captured_options)
}

fn summary_messages(count: usize) -> Vec<AgentMessage> {
    (0..count)
        .map(|index| {
            AgentMessage::Custom(Box::new(
                [
                    ("role".to_string(), json!("user")),
                    ("content".to_string(), json!("hello there")),
                    ("timestamp".to_string(), json!(index)),
                ]
                .into_iter()
                .collect(),
            ))
        })
        .collect()
}

fn summary_model() -> Model {
    Model {
        context_window: Some(1000),
        max_tokens: Some(100),
        ..test_model()
    }
}

async fn run_summary(
    message: kiana_ai::types::AssistantMessage,
    messages: &[AgentMessage],
) -> Result<(String, CapturedContext, CapturedOptions), String> {
    use kiana_resources::bug_report::{
        GenerateBugReportSummaryOptions, generate_bug_report_summary,
    };
    let (stream_fn, context, options) = fake_stream(message);
    let model = summary_model();
    let result = generate_bug_report_summary(GenerateBugReportSummaryOptions {
        messages,
        hint: None,
        model: &model,
        api_key: None,
        headers: None,
        env: None,
        signal: None,
        thinking_level: None,
        stream_fn: Some(stream_fn),
        retry: None,
        session_id: Some("session".to_string()),
    })
    .await
    .map_err(|error| error.to_string())?;
    Ok((result, context, options))
}

async fn run_summary_error(
    message: kiana_ai::types::AssistantMessage,
    messages: &[AgentMessage],
) -> String {
    match run_summary(message, messages).await {
        Ok(_) => panic!("expected the summary to fail"),
        Err(error) => error,
    }
}

fn assistant_text(text: &str) -> kiana_ai::types::AssistantMessage {
    let mut message = kiana_ai::types::AssistantMessage {
        stop_reason: StopReason::Stop,
        ..Default::default()
    };
    message.content = vec![kiana_ai::types::AssistantContentBlock::Text(
        kiana_ai::types::TextContent {
            kind_text: kiana_ai::types::TextKind::Text,
            text: text.to_string(),
            text_signature: None,
        },
    )];
    message
}

#[tokio::test]
async fn summary_prompt_contains_the_conversation_and_instructions() {
    let messages = summary_messages(1);
    let (text, context, options) = run_summary(assistant_text("  Report body  "), &messages)
        .await
        .expect("summary");
    assert_eq!(text, "Report body");

    let captured = context
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
        .expect("context");
    let value = serde_json::to_value(captured.as_context()).expect("context json");
    let system = &value["messages"][0];
    assert_eq!(system["role"], json!("system"));
    assert_eq!(
        system["content"],
        json!(kiana_resources::bug_report::BUG_SUMMARY_SYSTEM_PROMPT)
    );
    let prompt = value["messages"][1]["content"][0]["text"]
        .as_str()
        .expect("prompt text");
    assert!(prompt.contains("<conversation>\n[User]: hello there\n</conversation>"));
    assert!(prompt.contains(kiana_resources::bug_report::BUG_SUMMARY_INSTRUCTIONS));
    assert!(!prompt.contains("only the last"));

    let options = options
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
        .expect("options");
    assert_eq!(options.0, Some(100));
    assert_eq!(options.1.as_deref(), Some("session"));
}

#[tokio::test]
async fn summary_reports_truncated_message_counts() {
    let messages: Vec<AgentMessage> = (0..30)
        .map(|index| {
            AgentMessage::Custom(Box::new(
                [
                    ("role".to_string(), json!("user")),
                    ("content".to_string(), json!("x".repeat(100))),
                    ("timestamp".to_string(), json!(index)),
                ]
                .into_iter()
                .collect(),
            ))
        })
        .collect();
    let (_, context, _) = run_summary(assistant_text("ok"), &messages)
        .await
        .expect("summary");
    let captured = context
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
        .expect("context");
    let value = serde_json::to_value(captured.as_context()).expect("context json");
    let prompt = value["messages"][1]["content"][0]["text"]
        .as_str()
        .expect("prompt text");
    assert!(prompt.starts_with("Note: only the last "));
    assert!(prompt.contains(" of 30 messages are shown."));
}

#[tokio::test]
async fn summary_error_branches_use_the_ts_messages() {
    let messages = summary_messages(1);

    let mut aborted = assistant_text("ignored");
    aborted.stop_reason = StopReason::Aborted;
    assert_eq!(
        run_summary_error(aborted, &messages).await,
        "Bug report summary was cancelled"
    );

    let mut failed = assistant_text("ignored");
    failed.stop_reason = StopReason::Error;
    failed.error_message = Some("boom".to_string());
    assert_eq!(
        run_summary_error(failed, &messages).await,
        "Bug report summary failed: boom"
    );

    let mut capped = assistant_text("ignored");
    capped.stop_reason = StopReason::Length;
    assert_eq!(
        run_summary_error(capped, &messages).await,
        "Bug report summary failed: generation hit the token cap and the summary is incomplete"
    );

    let mut tool = assistant_text("ignored");
    tool.content = vec![kiana_ai::types::AssistantContentBlock::ToolCall(
        kiana_ai::types::ToolCall {
            id: "call".to_string(),
            name: "x".to_string(),
            ..Default::default()
        },
    )];
    assert_eq!(
        run_summary_error(tool, &messages).await,
        "Bug report summary attempted to call a tool"
    );

    assert_eq!(
        run_summary_error(assistant_text("   "), &messages).await,
        "Bug report summary was empty"
    );
}

#[test]
fn select_messages_counts_image_blocks_as_4800_chars() {
    use kiana_ai::types::{
        ImageContent, Message, TextContent, UserContent, UserContentBlock, UserMessage, UserRole,
    };

    let user_text = |text: String| {
        AgentMessage::Standard(Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Blocks(vec![UserContentBlock::Text(TextContent {
                text,
                ..Default::default()
            })]),
            timestamp: 0,
        }))
    };
    let image_message = AgentMessage::Standard(Message::User(UserMessage {
        role: UserRole::User,
        content: UserContent::Blocks(vec![UserContentBlock::Image(ImageContent {
            data: "AAAA".to_string(),
            mime_type: "image/png".to_string(),
            ..Default::default()
        })]),
        timestamp: 0,
    }));

    // 预算 600：最新 100 字符（25）+ 2000 字符（500）= 525；再加图片（1200）超出预算。
    let messages = vec![
        image_message,
        user_text("x".repeat(2000)),
        user_text("y".repeat(100)),
    ];
    assert_eq!(select_messages(&messages, 600.0).len(), 2);
}

#[cfg(target_os = "linux")]
#[test]
fn collect_environment_preserves_kernel_version_trailing_space() {
    // Node 的 os.version() 返回 utsname 原值；/proc/sys/kernel/version 末尾的空格保留，
    // 只去掉换行符。
    let raw = std::fs::read_to_string("/proc/sys/kernel/version").expect("version");
    let expected = raw.strip_suffix('\n').unwrap_or(&raw);
    let environment = kiana_resources::bug_report::collect_environment();
    assert_eq!(
        environment
            .get("osVersion")
            .and_then(|value| value.as_str()),
        Some(expected)
    );
}

#[test]
fn redact_url_preserves_whatwg_file_url_path_slashes() {
    // 期望值由 node 26 的 `new URL(v)` + `searchParams.set("token", "<redacted>")` 生成。
    let cases = [
        (
            "file://host//a?token=1",
            "file://host//a?token=%3Credacted%3E",
        ),
        (
            "file://host///a?token=1",
            "file://host///a?token=%3Credacted%3E",
        ),
        (
            "file://host//a%20b?token=1",
            "file://host//a%20b?token=%3Credacted%3E",
        ),
        (
            "file://host//a?token=1#Frag",
            "file://host//a?token=%3Credacted%3E#Frag",
        ),
        ("file:////a?token=1", "file:////a?token=%3Credacted%3E"),
        ("file://///a?token=1", "file://///a?token=%3Credacted%3E"),
        (
            "file://host//a\\b?token=1",
            "file://host//a/b?token=%3Credacted%3E",
        ),
        (
            "file://host/\\a?token=1",
            "file://host//a?token=%3Credacted%3E",
        ),
        (
            "file://host//a/..?token=1",
            "file://host//?token=%3Credacted%3E",
        ),
        (
            "file://host//a/.?token=1",
            "file://host//a/?token=%3Credacted%3E",
        ),
        (
            "file://host/%2e%2e//a?token=1",
            "file://host//a?token=%3Credacted%3E",
        ),
        (
            "file://host/%2e//a?token=1",
            "file://host//a?token=%3Credacted%3E",
        ),
        (
            "file://host/a/..//b?token=1",
            "file://host//b?token=%3Credacted%3E",
        ),
        (
            "file://host////..?token=1",
            "file://host///?token=%3Credacted%3E",
        ),
        (
            "file://host/a//b?token=1",
            "file://host/a//b?token=%3Credacted%3E",
        ),
        (
            "file://host//a//b?token=1",
            "file://host//a//b?token=%3Credacted%3E",
        ),
        ("file://host?token=1", "file://host/?token=%3Credacted%3E"),
        ("file://./a?token=1", "file://./a?token=%3Credacted%3E"),
        ("file://../a?token=1", "file://../a?token=%3Credacted%3E"),
        ("file:a?token=1", "file:///a?token=%3Credacted%3E"),
        ("file:/a?token=1", "file:///a?token=%3Credacted%3E"),
        (
            "FILE://host//a?token=1",
            "file://host//a?token=%3Credacted%3E",
        ),
        (
            "file://localhost//a?token=1",
            "file:////a?token=%3Credacted%3E",
        ),
        // 未变化时原样返回（不重新序列化）。
        ("file://host//a?x=1", "file://host//a?x=1"),
        ("file://host//a#token=1", "file://host//a#token=1"),
    ];
    for (input, expected) in cases {
        assert_eq!(redact_url(input), expected, "input: {input}");
    }
}

// ---------------------------------------------------------------------------
// uploadBugReport 的错误文案（TS 的 JS 真值/字符串化语义）
// ---------------------------------------------------------------------------

/// 启动一个只应答一次的本地 HTTP 服务；返回 (gateway URL, 任务句柄)。
async fn serve_once(
    status: u16,
    reason: &str,
    body: &str,
    content_type: &str,
) -> (String, tokio::task::JoinHandle<()>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind");
    let address = listener.local_addr().expect("addr");
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let handle = tokio::spawn(async move {
        if let Ok((mut socket, _)) = listener.accept().await {
            let mut buffer = [0u8; 65536];
            let _ = socket.read(&mut buffer).await;
            let _ = socket.write_all(response.as_bytes()).await;
            let _ = socket.shutdown().await;
        }
    });
    (format!("http://{address}/"), handle)
}

async fn upload_failure(status: u16, reason: &str, body: &str) -> String {
    let (gateway, handle) = serve_once(status, reason, body, "application/json").await;
    let bundle = BugReportBundle {
        metadata: json!({}),
        diagnostics: json!({}),
        session_jsonl: None,
        summary: None,
    };
    let result = upload_bug_report(
        &bundle,
        UploadBugReportOptions {
            token: None,
            signal: None,
            gateway_url: Some(gateway),
        },
    )
    .await;
    handle.await.expect("server task");
    match result {
        Err(error) => error.message,
        Ok(uploaded) => panic!("expected failure, got {uploaded:?}"),
    }
}

#[tokio::test]
async fn upload_failure_detail_uses_js_truthiness_and_string_conversion() {
    // `!json.ok`：`1` 是真值 → 忽略 description，回落到 statusText。
    assert_eq!(
        upload_failure(
            404,
            "Not Found",
            r#"{"ok":1,"description":"truthy-nonbool"}"#
        )
        .await,
        "Bug report upload failed: Not Found"
    );
    // description 的数字/布尔/数组按 `String(value)` 转换。
    assert_eq!(
        upload_failure(400, "Bad Request", r#"{"ok":false,"description":123}"#).await,
        "Bug report upload failed: 123"
    );
    assert_eq!(
        upload_failure(400, "Bad Request", r#"{"ok":false,"description":true}"#).await,
        "Bug report upload failed: true"
    );
    assert_eq!(
        upload_failure(
            400,
            "Bad Request",
            r#"{"ok":false,"description":[1,null,2]}"#
        )
        .await,
        "Bug report upload failed: 1,,2"
    );
    // description 为空串 → `||` 回落到 error；对象经 `String()` 是 "[object Object]"。
    assert_eq!(
        upload_failure(
            400,
            "Bad Request",
            r#"{"ok":false,"description":"","error":{"a":1}}"#
        )
        .await,
        "Bug report upload failed: [object Object]"
    );
    // 缺少 ok → `!undefined` 为真，description/error 都缺失 → statusText。
    assert_eq!(
        upload_failure(400, "Bad Request", "{}").await,
        "Bug report upload failed: Bad Request"
    );
    // 非 JSON 响应体 → json 为 null → statusText。
    assert_eq!(
        upload_failure(400, "Bad Request", "not json").await,
        "Bug report upload failed: Bad Request"
    );
    // 2xx 但 ok !== true → 仍抛错；detail 为 falsy 时用 statusText。
    assert_eq!(
        upload_failure(200, "OK", r#"{"ok":1,"bug_report":{"id":5}}"#).await,
        "Bug report upload failed: OK"
    );
}

#[tokio::test]
async fn upload_success_requires_ok_true_and_a_string_id() {
    let (gateway, handle) = serve_once(
        200,
        "OK",
        r#"{"ok":true,"bug_report":{"id":"abc"}}"#,
        "application/json",
    )
    .await;
    let bundle = BugReportBundle {
        metadata: json!({}),
        diagnostics: json!({}),
        session_jsonl: None,
        summary: None,
    };
    let uploaded = upload_bug_report(
        &bundle,
        UploadBugReportOptions {
            token: None,
            signal: None,
            gateway_url: Some(gateway),
        },
    )
    .await
    .expect("upload succeeds");
    handle.await.expect("server task");
    assert_eq!(uploaded.id, "abc");
}

#[test]
fn session_entry_data_serializes_in_ts_field_order() {
    let data = BugReportSessionEntryData {
        id: "r1".to_string(),
        created_at: "2026-01-01T00:00:00.000Z".to_string(),
        hint: None,
        session_included: true,
        summary_included: false,
        delivery: BugReportDelivery::Upload,
        path: None,
    };
    assert_eq!(
        serde_json::to_string(&data).expect("serialize"),
        r#"{"id":"r1","createdAt":"2026-01-01T00:00:00.000Z","hint":null,"sessionIncluded":true,"summaryIncluded":false,"delivery":"upload"}"#
    );

    let zipped = BugReportSessionEntryData {
        hint: Some("crash".to_string()),
        delivery: BugReportDelivery::Zip,
        path: Some("/tmp/report.zip".to_string()),
        ..data
    };
    assert_eq!(
        serde_json::to_string(&zipped).expect("serialize"),
        r#"{"id":"r1","createdAt":"2026-01-01T00:00:00.000Z","hint":"crash","sessionIncluded":true,"summaryIncluded":false,"delivery":"zip","path":"/tmp/report.zip"}"#
    );
}
