//! `packages/ai/test/error-body.test.ts`。
//!
//! TS 用手写的 SDK 形状错误对象（Mistral / openai APIError / @google/genai / Bedrock
//! ServiceException）；Rust 侧用 [`ProviderErrorFields`] 逐字段构造同一组形状。
//!
//! 两个 TS 用例无法直译：
//! - 「类实例 `$response.body` 没有 `pipe` 方法」：JS 里类实例序列化会漏内部字段，所以
//!   `isPlainNonEmptyObject` 返回 false。Rust 侧没有类实例，SDK 包装类由调用方映射成
//!   [`ResponseBody::Stream`]，用该变体表达同一语义（TS 里 `pipe` 流与类实例都等价于
//!   「没有可用的 body 文本」）。
//! - 「截断不持有整段 body 存活」依赖 V8 堆统计，Rust 侧不存在该泄漏模型；改为断言截断结果
//!   的前缀/后缀与长度。

use kiana_ai::utils::error_body::{
    MAX_PROVIDER_ERROR_BODY_CHARS, NormalizedProviderError, ProviderErrorFields,
    ProviderErrorValue, ResponseBody, format_provider_error, normalize_provider_error,
    truncate_error_text,
};
use serde_json::json;

fn error(fields: ProviderErrorFields) -> ProviderErrorValue {
    ProviderErrorValue::Error(fields)
}

fn message(message: &str) -> ProviderErrorFields {
    ProviderErrorFields {
        message: message.into(),
        ..Default::default()
    }
}

// describe("normalizeProviderError")

#[test]
fn extracts_status_and_body_from_a_mistral_shaped_error() {
    let norm = normalize_provider_error(&error(ProviderErrorFields {
        status_code: Some(403.0),
        body: Some(r#"{"error":"blocked by gateway WAF"}"#.into()),
        ..message("Mistral request failed")
    }));

    assert_eq!(norm.status, Some(403.0));
    assert_eq!(
        norm.body.as_deref(),
        Some(r#"{"error":"blocked by gateway WAF"}"#)
    );
    assert!(!norm.message_carries_body);
}

#[test]
fn reads_the_parsed_body_off_an_openai_apierror_when_the_message_is_opaque() {
    let norm = normalize_provider_error(&error(ProviderErrorFields {
        status: Some(403.0),
        error_body: Some(json!({"error": "blocked by gateway WAF"})),
        ..message("403 status code (no body)")
    }));

    assert_eq!(norm.status, Some(403.0));
    assert_eq!(
        norm.body.as_deref(),
        Some(r#"{"error":"blocked by gateway WAF"}"#)
    );
    assert!(!norm.message_carries_body);
}

#[test]
fn preserves_the_message_when_google_genai_already_folds_the_body_into_it() {
    let body = json!({"error": {"code": 403, "message": "Permission denied"}});
    let body_text = kiana_json::js_object::stringify(&body);
    let norm = normalize_provider_error(&error(ProviderErrorFields {
        status: Some(403.0),
        ..message(&body_text)
    }));

    assert_eq!(norm.status, Some(403.0));
    assert!(norm.message_carries_body);
    assert_eq!(norm.message, body_text);
}

#[test]
fn extracts_status_and_body_from_a_bedrock_shaped_service_exception() {
    let norm = normalize_provider_error(&error(ProviderErrorFields {
        metadata_status_code: Some(403.0),
        response_status_code: Some(403.0),
        response_body: Some(ResponseBody::Text(
            r#"{"message":"blocked by gateway WAF"}"#.into(),
        )),
        ..message("UnknownError")
    }));

    assert_eq!(norm.status, Some(403.0));
    assert_eq!(
        norm.body.as_deref(),
        Some(r#"{"message":"blocked by gateway WAF"}"#)
    );
    assert!(!norm.message_carries_body);
}

#[test]
fn ignores_a_bedrock_response_stream_instead_of_serializing_its_internals() {
    let norm = normalize_provider_error(&error(ProviderErrorFields {
        metadata_status_code: Some(400.0),
        response_status_code: Some(400.0),
        response_body: Some(ResponseBody::Stream),
        ..message(
            "Invocation of model ID anthropic.claude-opus-5 with on-demand throughput isn't supported.",
        )
    }));

    assert_eq!(norm.status, Some(400.0));
    assert_eq!(norm.body, None);
    assert!(
        norm.message
            .contains("on-demand throughput isn't supported")
    );
    assert!(norm.message_carries_body);
}

#[test]
fn ignores_a_class_instance_response_body_without_a_pipe_method_instead_of_serializing_it() {
    // TS 的 `SdkHttpResponseBody` 类实例：不是普通对象 → 不当作 body。
    let norm = normalize_provider_error(&error(ProviderErrorFields {
        metadata_status_code: Some(400.0),
        response_status_code: Some(400.0),
        response_body: Some(ResponseBody::Stream),
        ..message("Input is too long for requested model.")
    }));

    assert_eq!(norm.status, Some(400.0));
    assert_eq!(norm.body, None);
    assert!(norm.message.contains("Input is too long"));
    assert!(norm.message_carries_body);
}

#[test]
fn ignores_a_class_instance_error_field_instead_of_serializing_it() {
    // TS 的 `SdkInnerError` 类实例：`error` 不是普通 JSON 对象 → 不当作 body。
    let norm = normalize_provider_error(&error(ProviderErrorFields {
        status: Some(502.0),
        ..message("TLS handshake failed")
    }));

    assert_eq!(norm.body, None);
    assert_eq!(norm.message, "TLS handshake failed");
    assert!(norm.message_carries_body);
}

#[test]
fn still_surfaces_a_plain_parsed_json_body_object() {
    let norm = normalize_provider_error(&error(ProviderErrorFields {
        status: Some(400.0),
        error_body: Some(json!({"message": "schema validation failed", "field": "tools[0]"})),
        ..message("400 status code (no body)")
    }));

    assert_eq!(
        norm.body.as_deref(),
        Some(r#"{"message":"schema validation failed","field":"tools[0]"}"#)
    );
    assert!(!norm.message_carries_body);
}

#[test]
fn json_stringifies_a_non_error_thrown_value() {
    let norm = normalize_provider_error(&ProviderErrorValue::Value(json!({"reason": "boom"})));

    assert_eq!(norm.status, None);
    assert_eq!(norm.body, None);
    assert_eq!(norm.message, r#"{"reason":"boom"}"#);
    assert!(!norm.message_carries_body);
}

#[test]
fn treats_an_empty_parsed_body_object_as_no_body() {
    let norm = normalize_provider_error(&error(ProviderErrorFields {
        status: Some(403.0),
        error_body: Some(json!({})),
        ..message("403 status code (no body)")
    }));

    assert_eq!(norm.body, None);
    assert!(norm.message_carries_body);
}

#[test]
fn truncates_the_body_at_the_cap() {
    let long_body = "x".repeat(MAX_PROVIDER_ERROR_BODY_CHARS + 50);
    let norm = normalize_provider_error(&error(ProviderErrorFields {
        status_code: Some(500.0),
        body: Some(long_body.clone()),
        ..message("failed")
    }));

    let body = norm.body.expect("body");
    assert!(body.contains("... [truncated 50 chars]"));
    assert!(body.len() < long_body.len());
}

#[test]
fn sets_messagecarriesbody_when_the_message_already_contains_the_extracted_body() {
    let norm = normalize_provider_error(&error(ProviderErrorFields {
        status_code: Some(500.0),
        body: Some("upstream exploded".into()),
        ..message("500: upstream exploded")
    }));

    assert!(norm.message_carries_body);
}

// describe("formatProviderError")

fn openai_refusal() -> NormalizedProviderError {
    normalize_provider_error(&error(ProviderErrorFields {
        status: Some(403.0),
        error_body: Some(json!({"error": "blocked by gateway WAF"})),
        ..message("403 status code (no body)")
    }))
}

#[test]
fn surfaces_status_and_body_without_a_prefix() {
    let formatted = format_provider_error(&openai_refusal(), None);

    assert!(formatted.contains("403"));
    assert!(formatted.contains("blocked by gateway WAF"));
    assert_ne!(formatted, "403 status code (no body)");
}

#[test]
fn surfaces_a_non_integer_or_out_of_range_status_verbatim() {
    // TS 只要求 `typeof status === "number"`：非整数与越界值不截断、不改写。
    let norm = normalize_provider_error(&error(ProviderErrorFields {
        status: Some(403.5),
        body: Some("blocked".into()),
        ..message("403 status code (no body)")
    }));
    assert_eq!(norm.status, Some(403.5));
    assert_eq!(format_provider_error(&norm, None), "403.5: blocked");

    let norm = normalize_provider_error(&error(ProviderErrorFields {
        status_code: Some(70_000.0),
        body: Some("gateway".into()),
        ..message("Unknown: UnknownError")
    }));
    assert_eq!(norm.status, Some(70_000.0));
    assert_eq!(
        format_provider_error(&norm, Some("P")),
        "P (70000): gateway"
    );
}

#[test]
fn applies_a_provider_prefix_with_status_and_body() {
    assert_eq!(
        format_provider_error(&openai_refusal(), Some("OpenAI API error")),
        r#"OpenAI API error (403): {"error":"blocked by gateway WAF"}"#
    );
}

#[test]
fn preserves_the_message_with_prefix_and_status_when_it_already_carries_the_body() {
    let body =
        kiana_json::js_object::stringify(&json!({"error": {"message": "Permission denied"}}));
    let norm = normalize_provider_error(&error(ProviderErrorFields {
        status: Some(403.0),
        ..message(&body)
    }));

    assert_eq!(
        format_provider_error(&norm, Some("OpenAI API error")),
        format!("OpenAI API error (403): {body}")
    );
}

#[test]
fn returns_the_bare_message_for_a_non_error_value() {
    let norm = normalize_provider_error(&ProviderErrorValue::Value(json!({"reason": "boom"})));
    assert_eq!(format_provider_error(&norm, None), r#"{"reason":"boom"}"#);
}

// describe("truncateErrorText") —— #9265

#[test]
fn keeps_an_excerpt_that_does_not_hold_the_whole_body_alive() {
    // TS 关心的是「切片不持有 6 MB 原串」；Rust 侧 `String` 必然复制，这里断言截断语义本身。
    let body = "<html>".repeat(1_000_000);
    let kept = truncate_error_text(&body, MAX_PROVIDER_ERROR_BODY_CHARS);

    assert!(kept.starts_with("<html><html>"));
    assert!(kept.ends_with(&format!(
        "[truncated {} chars]",
        6_000_000 - MAX_PROVIDER_ERROR_BODY_CHARS
    )));
}
