//! `ModelsError` 的回归测试（review round 1 的 finding 8）。
//!
//! TS `utils/models-error.ts` 的六个 code、`name = "ModelsError"`、以及 `withCauseDetail`
//! 的 `message` 拼接规则（`spec/ai-auth.md:93`、`spec/ai-core.md:1190`）。

use kiana_ai::error::{ModelsError, ModelsErrorCode};

const ALL_CODES: [ModelsErrorCode; 6] = [
    ModelsErrorCode::ModelSource,
    ModelsErrorCode::ModelValidation,
    ModelsErrorCode::Provider,
    ModelsErrorCode::Stream,
    ModelsErrorCode::Auth,
    ModelsErrorCode::OAuth,
];

/// TS `ModelsErrorCode` 的六个字符串字面量，逐字。
#[test]
fn codes_match_the_typescript_literals() {
    let expected = [
        "model_source",
        "model_validation",
        "provider",
        "stream",
        "auth",
        "oauth",
    ];
    for (code, literal) in ALL_CODES.iter().zip(expected) {
        assert_eq!(code.as_str(), literal);
        assert_eq!(code.to_string(), literal);
        assert_eq!(ModelsErrorCode::from_str(literal), Some(*code));
        // 序列化成同一个字面量（会话文件里看到的就是它）。
        assert_eq!(
            serde_json::to_value(code).expect("serializes"),
            serde_json::json!(literal)
        );
    }
    assert_eq!(ModelsErrorCode::from_str("unknown"), None);
    // 反序列化未知值报错（不静默映射到某个默认值）。
    assert!(serde_json::from_str::<ModelsErrorCode>("\"unknown\"").is_err());
}

/// `new ModelsError(code, message)`：`message` 原样使用，`name` 是 `"ModelsError"`。
#[test]
fn name_and_plain_message_match_the_typescript_class() {
    let error = ModelsError::new(
        ModelsErrorCode::Provider,
        "Model openai/gpt-image is not a chat model",
    );
    assert_eq!(error.name(), "ModelsError");
    assert_eq!(error.code, ModelsErrorCode::Provider);
    assert_eq!(
        error.to_string(),
        "Model openai/gpt-image is not a chat model"
    );
    assert_eq!(error.cause_detail, None);
}

/// `withCauseDetail`：`${message}: ${formatThrownValue(cause).trim()}`，空细节或已被包含时不追加。
#[test]
fn cause_detail_is_appended_like_with_cause_detail() {
    let appended = ModelsError::with_cause_detail(
        ModelsErrorCode::Auth,
        "Failed to refresh token",
        "  401 Unauthorized \n",
    );
    assert_eq!(
        appended.message,
        "Failed to refresh token: 401 Unauthorized"
    );
    assert_eq!(appended.cause_detail.as_deref(), Some("401 Unauthorized"));

    // 空 / 纯空白的 cause → 不追加。
    let empty = ModelsError::with_cause_detail(ModelsErrorCode::Auth, "msg", "   ");
    assert_eq!(empty.message, "msg");
    assert_eq!(empty.cause_detail, None);

    // message 已经包含该细节（trim 后）→ 不重复追加。
    let contained =
        ModelsError::with_cause_detail(ModelsErrorCode::Stream, "stream failed: boom", "boom");
    assert_eq!(contained.message, "stream failed: boom");
    assert_eq!(contained.cause_detail, None);
}
