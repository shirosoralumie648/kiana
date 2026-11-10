//! `DEFAULT_AGENT_IDENTITY`（TS: `packages/agent/src/agent-loop.ts`）。
//!
//! ```ts
//! export const DEFAULT_AGENT_IDENTITY: ClientIdentity = { attribution: KIANA_ATTRIBUTION };
//! ```
//!
//! agent loop 每次请求把它与 `config.identity` 逐字段合并
//! （`mergeClientIdentity(DEFAULT_AGENT_IDENTITY, config.identity)`）。
//! `KIANA_ATTRIBUTION` 是 `{ name: "kiana", url: "https://kiana.dev", categories: ["cli-agent"] }`。
//!
//! # 设计原因与调用位置
//!
//! - `ClientIdentity.attribution` 是三态字段（缺省 = 不改动、`null` = 关闭、具体值 = 设置）。
//!   这里写 `Some(Some(..))` 表示**显式设置** attribution，而不是“沿用合并前的值”；
//!   若写 `None`，合并时会跳过该字段，默认归因就不会生效。
//! - `version` / `user_agent` 保持 `None`：`merge_client_identity` 只在字段为 `Some` 时覆盖，
//!   `None` 表示“本层不表态”，所以调用方自己设置的 UA / 版本号不会被默认身份抹掉。
//! - 生产代码中的唯一调用方是 `agent_loop::stream_assistant_response`：每次发请求前执行
//!   `merge_client_identity([Some(default_agent_identity()), config.options.base.identity.clone()])`，
//!   即“默认归因 + 本次请求的身份逐字段覆盖”。本函数是纯函数，可反复调用。
//!
//! 注意：这里只是数据；真正把 attribution 转成请求头的是 `kiana-ai` 的 identity 工具。

use kiana_ai::types::ClientIdentity;

/// 默认身份：provider 归因头署名 kiana（`{ attribution: KIANA_ATTRIBUTION }`，没有 user-agent 覆盖）。
///
/// 每次调用返回一个**新构造**的值：`attribution = Some(Some(..))`（显式设置，见模块文档），
/// `version` / `user_agent` 为 `None`（不覆盖调用方的设置）。
/// 序列化形状由 `tests/public_api.rs` 锁定：
/// `{"attribution":{"name":"kiana","url":"https://kiana.dev","categories":["cli-agent"]}}`。
pub fn default_agent_identity() -> ClientIdentity {
    ClientIdentity {
        version: None,
        user_agent: None,
        attribution: Some(Some(kiana_ai::types::kiana_attribution())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_kiana_attribution() {
        let identity = default_agent_identity();
        assert_eq!(identity.version, None);
        assert_eq!(identity.user_agent, None);
        assert_eq!(
            identity.attribution,
            Some(Some(kiana_ai::types::kiana_attribution()))
        );
        assert_eq!(
            serde_json::to_string(&identity).unwrap(),
            r#"{"attribution":{"name":"kiana","url":"https://kiana.dev","categories":["cli-agent"]}}"#
        );
    }
}
