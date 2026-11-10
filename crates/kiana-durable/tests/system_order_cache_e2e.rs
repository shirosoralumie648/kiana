//! `test/system-order-cache-e2e.test.ts` 的 Rust 端口（live）。
//!
//! 上游用例是 #10542 的回归覆盖：系统消息必须领跑请求，工具列表在会话中途新增时才不会
//! 改变缓存前缀。没有 Anthropic 凭据时上游 `it.skipIf(!anthropicToken)` 跳过。
//!
//! Rust 端口标记为 `#[ignore]`：真实运行需要 Anthropic OAuth 与 `anthropic-messages`
//! transport；这里保留确定性的 `instructions(nonce)` 与 `committedOrder(messages)`（旧顺序
//! 的复现函数），并断言两条路径的排序差异。

use kiana_ai::types::{Message, SystemRole, UserContent, UserMessage, UserRole};

/// 上游的 `instructions(nonce)`：一段带 nonce 的长重复上下文。
fn instructions(nonce: &str) -> String {
    let mut lines = vec![
        "This is an automated prompt-cache test. The repeated context below is intentional."
            .to_owned(),
        "Never call tools. Reply with exactly the text the user asks for.".to_owned(),
    ];
    for index in 0..250 {
        lines.push(format!(
            "{nonce} cache record {index:03}: alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi omicron kiana rho sigma tau upsilon phi chi psi omega."
        ));
    }
    lines.join("\n")
}

fn is_system(message: &Message) -> bool {
    matches!(message, Message::System(system) if system.role == SystemRole::System)
}

fn is_user(message: &Message) -> bool {
    matches!(message, Message::User(user) if user.role == UserRole::User)
}

/// 上游的 `committedOrder(messages)`：把领跑的 system 消息放回用户消息之前的位置
/// （修复前的请求顺序）。
fn committed_order(messages: &[Message]) -> Vec<Message> {
    if messages.first().is_none_or(|message| !is_system(message)) {
        return messages.to_vec();
    }
    let system = messages[0].clone();
    let rest = &messages[1..];
    let at = rest
        .iter()
        .position(|message| !is_user(message))
        .unwrap_or(rest.len());
    let mut ordered = Vec::with_capacity(messages.len());
    ordered.extend_from_slice(&rest[..at]);
    ordered.push(system);
    ordered.extend_from_slice(&rest[at..]);
    ordered
}

fn system_message() -> Message {
    Message::System(kiana_ai::types::SystemMessage {
        role: SystemRole::System,
        content: kiana_ai::types::SystemContent::Text("system".to_owned()),
        sections: None,
        tools_removed: None,
        tools_added: None,
        timestamp: 1,
    })
}

fn user_message(text: &str) -> Message {
    Message::User(UserMessage {
        role: UserRole::User,
        content: UserContent::Text(text.to_owned()),
        timestamp: 2,
    })
}

#[test]
#[ignore = "live: needs Anthropic OAuth credentials and the anthropic-messages transport"]
fn keeps_the_prompt_cache_across_a_tool_change_only_when_the_system_message_leads() {
    // 确定性部分：旧顺序把 system 放到用户消息之后，新顺序保持它领跑。
    let leading = vec![
        system_message(),
        user_message("first"),
        user_message("second"),
    ];
    let legacy = committed_order(&leading);
    assert!(is_user(&legacy[0]));
    assert!(is_user(&legacy[1]));
    assert!(is_system(&legacy[2]));

    let already_user_led = vec![user_message("first"), system_message()];
    assert_eq!(
        committed_order(&already_user_led)
            .iter()
            .map(|message| match message {
                Message::User(_) => "user",
                Message::System(_) => "system",
                _ => "other",
            })
            .collect::<Vec<_>>(),
        vec!["user", "system"]
    );

    let instructions = instructions("0190f0c4-0000-7000-8000-000000000000");
    assert_eq!(instructions.lines().count(), 252);
    assert!(instructions.contains("cache record 000:"));
    assert!(instructions.contains("cache record 249:"));

    // live 部分（上游 it.skipIf(!anthropicToken)）：
    // 1. `cacheReadAfterToolChange(token, legacyOrder)`：两轮 ask，第二轮前
    //    `root.configure({ tools: [first, second] })`；
    // 2. 旧顺序断言第二轮 cacheRead 占比 < 0.2，新顺序 > 0.8。
    panic!(
        "live system-order-cache e2e: set up Anthropic OAuth and the transport wrapper before running"
    );
}
