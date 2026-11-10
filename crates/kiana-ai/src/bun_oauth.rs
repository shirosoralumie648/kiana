//! `bun-oauth.ts`：`registerBunOAuthFlows()`——把 9 个 OAuth 流静态登记进 `auth/oauth/load`。
//!
//! Rust 侧没有 Bun 的隐式动态 import，`load` 的登记表在编译期就是静态的；本模块保留同名
//! 函数作为显式初始化点（幂等）。注意：Rust 原生二进制没有 Bun 的 runtime-setup 注入步骤
//! （见 `kiana-coding-agent/src/bun/mod.rs` 的说明），当前仓库里没有生产调用方；需要与 TS
//! 启动流程逐项对齐时，由上层显式调用。重复调用等价于一次调用（登记内容相同；
//! `register_bundled_oauth_flow_loaders` 允许后注册覆盖先注册）。
//!
//! TS: `src/bun-oauth.ts`
//!
//! # 登记了哪些流
//!
//! `builtin_oauth_flow_loaders()` 的 9 个字段：anthropic、openai_codex、openai_chatgpt、
//! github_copilot、openrouter、kimi_coding、meta、xai、radius（见
//! `auth/oauth/load.rs` 的 `OAuthFlowLoaders`）。数量与字段名都以那里为准；本模块只负责
//! 把这张表写进全局登记表。
//!
//! # 为什么还需要显式初始化
//!
//! 登记表是进程级全局（`load` 内的 `Mutex<Option<Arc<OAuthFlowLoaders>>>`），
//! `register_bundled_oauth_flow_loaders` 后注册覆盖先注册。本函数的语义是把「内置实现」
//! 从隐式回落变成显式登记；即使不调用，`load_*` 也会回落到内置实现。因此它是兼容层的
//! 可选步骤，而不是使用 OAuth 的前置条件。
//!
//! # 注意事项
//!
//! - 全局表没有“退出”语义；测试必须用 `clear_bundled_oauth_flow_loaders` 恢复，否则会
//!   污染同进程的其它测试（下方测试即如此收尾）。
//! - 单次调用是常数时间、无 IO；线程安全由 `load` 的 `Mutex` 提供，本函数不额外加锁。

use crate::auth::oauth::load;

/// `registerBunOAuthFlows()`：登记静态内嵌的全部 OAuth 流程（幂等）。
///
/// TS: `src/bun-oauth.ts#registerBunOAuthFlows`
pub fn register_bun_oauth_flows() {
    load::register_bundled_oauth_flow_loaders(load::builtin_oauth_flow_loaders());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registering_twice_is_idempotent() {
        register_bun_oauth_flows();
        register_bun_oauth_flows();
        // 恢复全局登记表：否则同进程里后续读取 `load_*` 的测试会看到本测试留下的表。
        load::clear_bundled_oauth_flow_loaders();
    }
}
