//! auth 目录：凭据与 OAuth。
//!
//! 逐文件对应 `packages/ai/src/auth/**`：
//! - [`types`]：`ProviderAuth` / `Credential` / `CredentialStore` / `AuthPrompt` / `AuthEvent` 等；
//! - [`context`]、[`helpers`]、[`credential_store`]、[`resolve`]：默认 context、`envApiKeyAuth` /
//!   `lazyOAuth`、进程内凭据存储、`resolveProviderAuth` 与双重检查锁刷新；
//! - [`oauth`]：PKCE、设备码轮询、loopback 回调服务器（各 provider 流程由其他单元/文件实现）。
//!
//! `src/oauth.ts`（包入口 `@earendil-works/kiana-ai/oauth`）是**仅类型**的兼容 barrel：TS 模块
//! 运行时为空，Rust 侧对应 [`oauth_compat`] 的类型 re-export，不得泄漏任何内置流程实现。
//!
//! # 模块边界与依赖方向
//! [`types`] 是唯一被所有子模块依赖的契约层（trait 与数据结构）；[`context`]、
//! [`credential_store`]、[`helpers`] 是三个默认实现，彼此不依赖；[`resolve`] 在上层把它们与
//! provider 自己的 `ApiKeyAuth` / `OAuthAuth` 组合成一次鉴权解析。[`oauth`] 子树为
//! `lazy_oauth` 的加载闭包提供实际的 OAuth 流程实现，这里只声明模块，不会执行它们。
//!
//! # 一次典型的鉴权解析
//! `Models::get_auth` → [`resolve`] 的 `resolve_provider_auth` → `CredentialStore::read` 读
//! 存储凭证 →（OAuth 需要刷新时）在 `CredentialStore::modify` 内调用 `OAuthAuth::refresh`
//! 并写回 → `ApiKeyAuth::resolve` 或 `OAuthAuth::to_auth` 得到 `AuthResult`。什么都没存且
//! provider 有 api-key handler 时，才回退到 [`context`] 提供的环境变量/文件；纯 OAuth
//! provider 直接返回“未配置”。
//!
//! # 设计原因
//! 「凭证存在哪」（`CredentialStore`）、「环境长什么样」（`AuthContext`）、「provider 怎么
//! 鉴权」（`ApiKeyAuth` / `OAuthAuth`）是三件正交的事。拆成三个 trait 后宿主可以分别替换：
//! 测试注入内存实现，浏览器/受限环境注入自己的环境访问，生产注入持久化存储，而 `Models` 与
//! provider 定义不需要知道具体来源。
//!
//! # 阅读顺序
//! 1. [`types`]：先看四个核心 trait 与 `Credential` / `AuthResult` 的形状；
//! 2. [`context`] 与 [`credential_store`]：两个最直接的默认实现；
//! 3. [`helpers`]：provider 声明鉴权方式时用的两个工厂；
//! 4. [`resolve`]：把上述零件组合成一次 `resolveProviderAuth` 调用（最复杂，最后读）。
//!
//! # 注意事项
//! - 所有 trait 都要求 `Send + Sync`：实现会被放进 `Arc` 跨任务共享，不要依赖线程局部状态。
//! - 默认 `AuthContext` 直接读进程环境：并行测试若修改进程 env 会互相干扰，应注入自定义
//!   实现（见 `helpers.rs` 测试里的 `TestContext`）。
//! - `CredentialStore::modify` 的回调运行在该 provider 的串行门内，回调里不得再对同一
//!   provider 调用同一 store 的 `modify` / `delete`。

// --- 子模块 ---
pub mod context;
pub mod credential_store;
pub mod helpers;
pub mod oauth;
pub mod resolve;
pub mod types;

/// `src/oauth.ts`（`@earendil-works/kiana-ai/oauth`）的仅类型兼容入口。
///
/// TS 的 7 个类型重导出：`OAuthAuthInfo`、`OAuthCredentials`、`OAuthDeviceCodeInfo`、
/// `OAuthLoginCallbacks`、`OAuthPrompt`、`OAuthSelectOption`、`OAuthSelectPrompt`
/// （`OAuthCredentials` 最终来自 [`types`]，其余来自
/// [`crate::compat::extension_oauth_types`]）。**没有**任何内置 OAuth 流程常量。
///
/// crate 根的 `pub mod oauth_compat;` barrel 由 ai-core/merge 步骤接线（本单元不拥有
/// `lib.rs`）；这里先提供同名模块，便于直接 `pub use auth::oauth_compat;`。
///
/// 现状（对照 `lib.rs`）：实际接线是 `pub use auth::oauth_compat;` 的 re-export，而不是在
/// crate 根再定义一个同名模块；因此 `crate::oauth_compat` 与 `crate::auth::oauth_compat`
/// 指向同一个模块，修改这里的 re-export 名单会同时影响两个路径。
pub mod oauth_compat {
    pub use crate::auth::types::OAuthCredentials;
    pub use crate::compat::extension_oauth_types::{
        OAuthAuthInfo, OAuthDeviceCodeInfo, OAuthLoginCallbacks, OAuthPrompt, OAuthSelectOption,
        OAuthSelectPrompt,
    };
}

#[cfg(test)]
mod tests {
    // `src/oauth.ts` 是包入口 `@earendil-works/kiana-ai/oauth`：crate 根的
    // `pub use auth::oauth_compat;` 必须让这 7 个类型可达。
    #[allow(unused_imports)]
    use crate::oauth_compat::{
        OAuthAuthInfo, OAuthCredentials, OAuthDeviceCodeInfo, OAuthLoginCallbacks, OAuthPrompt,
        OAuthSelectOption, OAuthSelectPrompt,
    };

    #[test]
    fn crate_root_oauth_barrel_exposes_the_seven_extension_types() {
        let option = OAuthSelectOption {
            id: "a".into(),
            label: "A".into(),
        };
        assert_eq!(option.label, "A");
        assert!(OAuthCredentials::default().refresh.is_empty());
    }
}
