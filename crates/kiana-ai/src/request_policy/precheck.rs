//! 发请求前的输入 token 预检。
//!
//! 用 `utils::estimate::estimate_context_tokens` 对归一化上下文做字符启发式估算，在真正
//! 建立 HTTP 连接之前判断请求是否已经放不进模型窗口。命中时返回 typed
//! [`ContextWindowExceeded`]；调用方必须直接走压缩/换模型路径，**不得发出任何请求**
//! （与 DoD 的「provider mock 断言零请求」对应）。
//!
//! 预检口径与 [`super::budget`] 一致：`context_window - estimated - reserve < 1` 即失败。
//! 因此 `estimated` 恰好等于窗口、或只剩不到 1 个输出 token 的请求都会在本地被拦下。
//!
//! 与 `utils::overflow` 的关系：overflow 是**响应后**按 provider 文案/usage 的判定；
//! 预检发生在**请求前**，两者互补。结构化分类见 [`super::classification`]：
//! 预检失败属于 `ContextWindow`，不应重试。

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::{AiError, AiErrorKind};
use crate::request_policy::budget::OUTPUT_RESERVE_TOKENS;
use crate::types::{Model, TranscriptContext};
use crate::utils::estimate::estimate_context_tokens;

/// 预检结果（成功）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InputPrecheck {
    /// 模型上下文窗口；`None`/`Some(0)` 表示未知。
    pub context_window: Option<u64>,
    /// 估算的输入 token。
    pub estimated_input_tokens: u64,
    /// 预留 token。
    pub reserve_tokens: u64,
    /// 剩余可用输出空间；窗口未知时为 `None`。
    pub available: Option<u64>,
}

/// 预检失败：输入（含预留）超过窗口。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextWindowExceeded {
    /// 模型上下文窗口。
    pub context_window: u64,
    /// 估算输入 token。
    pub estimated_input_tokens: u64,
    /// 预留 token。
    pub reserve_tokens: u64,
    /// `context_window - estimated - reserve`（可为负）。
    pub available: i64,
}

impl fmt::Display for ContextWindowExceeded {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "input exceeds the context window before dispatch: context_window={}, \
             estimated_input={}, reserve={}, available={}",
            self.context_window, self.estimated_input_tokens, self.reserve_tokens, self.available
        )
    }
}

impl std::error::Error for ContextWindowExceeded {}

impl From<ContextWindowExceeded> for AiError {
    fn from(error: ContextWindowExceeded) -> Self {
        AiError::new(AiErrorKind::Request, error.to_string())
            .with_name("ContextWindowExceeded")
            .with_detail(serde_json::to_value(error).unwrap_or(serde_json::Value::Null))
    }
}

/// 估算一次请求的输入 token。
pub fn estimate_input_tokens(context: &TranscriptContext) -> u64 {
    estimate_context_tokens(context).tokens
}

/// 预检输入是否放得进模型窗口。
///
/// - 窗口未知（`None` 或 0）：恒成功，`available = None`；
/// - 窗口已知：`available = window - estimated - reserve`，`< 1` 即返回
///   [`ContextWindowExceeded`]。
pub fn precheck_input(
    context: &TranscriptContext,
    model: &Model,
    reserve_tokens: u64,
) -> Result<InputPrecheck, ContextWindowExceeded> {
    let estimated_input_tokens = estimate_input_tokens(context);
    match model.context_window {
        Some(context_window) if context_window > 0 => {
            let available =
                context_window as i128 - estimated_input_tokens as i128 - reserve_tokens as i128;
            if available < 1 {
                return Err(ContextWindowExceeded {
                    context_window,
                    estimated_input_tokens,
                    reserve_tokens,
                    available: available.clamp(i64::MIN as i128, i64::MAX as i128) as i64,
                });
            }
            Ok(InputPrecheck {
                context_window: Some(context_window),
                estimated_input_tokens,
                reserve_tokens,
                available: Some(available as u64),
            })
        }
        _ => Ok(InputPrecheck {
            context_window: None,
            estimated_input_tokens,
            reserve_tokens,
            available: None,
        }),
    }
}

/// 用默认 1024 预留做预检。
pub fn precheck_input_with_defaults(
    context: &TranscriptContext,
    model: &Model,
) -> Result<InputPrecheck, ContextWindowExceeded> {
    precheck_input(context, model, OUTPUT_RESERVE_TOKENS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{TranscriptContext, UserMessage, UserRole};

    fn context_with(text: &str) -> TranscriptContext {
        // 3.5 字符/token：7000 字符约 2000 token。
        let mut context = TranscriptContext::default();
        context
            .0
            .messages
            .push(crate::types::Message::User(UserMessage {
                role: UserRole::User,
                content: crate::types::UserContent::Text(text.to_string()),
                timestamp: 0,
            }));
        context
    }

    fn model(window: Option<u64>) -> Model {
        Model {
            context_window: window,
            ..Model::default()
        }
    }

    #[test]
    fn rejects_input_that_cannot_leave_room_for_one_output_token() {
        let context = context_with(&"x".repeat(35_000));
        let error = precheck_input(&context, &model(Some(10_000)), 1_024).expect_err("overflow");
        assert_eq!(error.context_window, 10_000);
        assert_eq!(error.estimated_input_tokens, 10_000);
        assert_eq!(error.available, -1_024);
        assert!(error.to_string().contains("context window"));
    }

    #[test]
    fn unknown_window_never_rejects() {
        let context = context_with(&"x".repeat(700_000));
        let precheck = precheck_input(&context, &model(None), 1_024).expect("unknown window");
        assert_eq!(precheck.available, None);
        let precheck = precheck_input(&context, &model(Some(0)), 1_024).expect("zero window");
        assert_eq!(precheck.available, None);
    }

    #[test]
    fn zero_requests_when_the_precheck_fails() {
        let context = context_with(&"x".repeat(70_000));
        let mut dispatched = 0;
        let result = precheck_input(&context, &model(Some(10_000)), 1_024).map(|_| {
            dispatched += 1;
        });
        assert!(result.is_err());
        assert_eq!(dispatched, 0, "precheck failure must not dispatch");
    }

    #[test]
    fn accepts_input_that_fits() {
        let context = context_with(&"x".repeat(3_500));
        let precheck = precheck_input(&context, &model(Some(10_000)), 1_024).expect("fits");
        assert_eq!(precheck.estimated_input_tokens, 1_000);
        assert_eq!(precheck.available, Some(7_976));
    }
}
