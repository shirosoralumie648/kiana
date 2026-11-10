//! 回归（fixer, minor）：默认流函数必须是 `streamSimple` 的规范实例。
//!
//! TS `sdk.ts` 用模块级 `setDefaultStreamFn(streamSimple)` 安装**同一个函数对象**；
//! `_getSummarizationRequestAuth` 的 `agent.streamFunction === streamSimple` 身份判断依赖
//! 这一点。Rust 修复前 `ensure_default_stream_fn` 又包了一层 `Arc`，默认构造的 Agent 会被
//! 误判成“自定义流函数”，摘要因此跳过必需认证。

use std::sync::Arc;

use kiana_agent_core::agent::{Agent, AgentOptions};

#[test]
fn default_stream_function_is_the_canonical_stream_simple_instance() {
    kiana_session::sdk::ensure_default_stream_fn();
    let default = kiana_agent_core::stream_fn::get_default_stream_fn().expect("default installed");
    assert!(
        Arc::ptr_eq(
            &default,
            &kiana_session::agent_session::stream_simple_stream_fn()
        ),
        "the process default must be the canonical stream_simple instance"
    );

    // 未显式传 streamFn 的 Agent 使用同一个实例（TS 的 `streamFunction === streamSimple`）。
    let agent = Agent::new(AgentOptions::default()).expect("agent");
    assert!(
        Arc::ptr_eq(
            &agent.stream_function(),
            &kiana_session::agent_session::stream_simple_stream_fn()
        ),
        "a default-constructed Agent must classify as the built-in stream function"
    );
}
