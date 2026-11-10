//! 回归（fixer, minor）：replaced session context 的 send 方法必须把错误交给等待的扩展。
//!
//! TS `createReplacedSessionContext` 直接把 `sendCustomMessage`/`sendUserMessage` 挂到
//! `ctx.sendMessage`/`ctx.sendUserMessage` 上：失败时 `await` 的 Promise reject。Rust 冻结的
//! `ReplacedSessionContextApi::send_message` 没有错误通道，因此会话额外实现
//! `send_message_checked`/`send_user_message_checked`。
#![allow(clippy::arc_with_non_send_sync)]

mod common;

use common::*;
use kiana_ai::types::UserContent;

#[tokio::test]
async fn replaced_context_send_methods_report_errors() {
    run(async {
        let harness = create_harness(HarnessOptions {
            with_configured_auth: Some(false),
            ..Default::default()
        })
        .await;
        let ctx = harness.session.create_replaced_session_context();

        let user_error = ctx
            .send_user_message_checked(UserContent::Text("hi".to_string()), None)
            .await
            .expect_err("sendUserMessage must report the missing API key");
        assert!(
            user_error.starts_with("No API key found for faux."),
            "error: {user_error}"
        );

        // `sendCustomMessage(triggerTurn)` 与 TS 一样不做认证预检：请求在 run 内部才失败
        // （agent 的错误响应），因此这里返回 Ok；错误通道本身由 sendUserMessage 覆盖。
        ctx.send_message_checked(kiana_ext_api::types::SendMessageOptions {
            message: kiana_ext_api::types::CustomMessagePayload {
                custom_type: "test".to_string(),
                content: UserContent::Text("hello".to_string()),
                display: false,
                details: None,
            },
            trigger_turn: Some(true),
            deliver_as: None,
        })
        .await
        .expect("sendCustomMessage(triggerTurn) does not preflight auth");
        harness.cleanup();
    })
    .await;
}
