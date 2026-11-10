//! OAuth：`oauth/{types,errors,discovery}.ts`（本单元）与 `oauth/{flow,provider,callback}.ts`
//! （由其他单元实现）。
//!
//! TS 源：`packages/mcp/src/oauth/**`（`spec/telemetry-mcp.md` §18、§19.2、§20.3、§27.2、§27.5）。
//!
//! # 这个模块负责什么
//!
//! MCP 远程服务器的 OAuth 授权：发现授权服务器与受保护资源的元数据、PKCE 授权码流程、
//! 动态客户端注册与令牌刷新、回环 HTTP 回调、授权状态持久化。它是**工具箱**，不决定何时
//! 发起登录：HTTP 传输只认识 `AuthProvider`，由调用方把 OAuth 接上去。
//! 需要发网络请求的子模块（[`discovery`]、[`flow`]）都经
//! `crate::auth_provider::McpFetch` 发 HTTP，因此测试可以整体替换 HTTP 栈。
//!
//! # 在系统中的位置
//!
//! TS 的 `oauth/index.ts` 是独立入口，crate 根（[`crate::index`]）**不**包含这里的名字，
//! 下游按 `kiana_mcp::oauth::...` 路径使用（本文件按 TS 的分组顺序再导出 44 个名字）。
//! 主要调用方是另一个 crate `kiana-builtin-extensions` 的 `mcp/oauth.rs`：它实现
//! `AuthProvider`、构造 [`McpOAuthProvider`]、调用 [`authorize_mcp`] 完成登录，并自己实现
//! [`McpOAuthStateStore`] 把凭据写入 `mcp-auth.json`。
//!
//! # 子模块
//!
//! | 模块 | TS 源 | 状态 |
//! |---|---|---|
//! | [`types`] | `oauth/types.ts` | 本单元：元数据结构 + `parse_*`（全部失败是 `Error("Invalid …")`） |
//! | [`errors`] | `oauth/errors.ts` | 本单元：5 个错误类 + [`errors::OAuthFailure`] 信封 |
//! | [`discovery`] | `oauth/discovery.ts` | 本单元：`parseWwwAuthenticate`、well-known 发现、`selectResource` |
//! | [`flow`] | `oauth/flow.ts` | PKCE/令牌请求/DCR/`authorizeMcp` |
//! | [`provider`] | `oauth/provider.ts` | `McpOAuthProvider`（`McpOAuthState` 的持久化形状） |
//! | [`callback`] | `oauth/callback.ts` | `OAuthCallbackServer`（回环 HTTP 回调） |
//!
//! # 再导出面（`oauth/index.ts`，44 个名字，逐字顺序）
//!
//! TS 的 `src/oauth/index.ts` 有 44 个导出（§19.2），Rust 侧的对应关系：
//!
//! | # | TS 名字 | Rust |
//! |---|---|---|
//! | 1–4 | `OAuthCallback`、`OAuthCallbackPage`、`OAuthCallbackServer`、`OAuthCallbackServerOptions` | [`callback`] |
//! | 5 | `buildAuthorizationServerDiscoveryUrls` | [`discovery::build_authorization_server_discovery_urls`] |
//! | 6 | `discoverAuthorizationServerMetadata` | [`discovery::discover_authorization_server_metadata`] |
//! | 7 | `discoverOAuthServerInfo` | [`discovery::discover_oauth_server_info`] |
//! | 8 | `discoverProtectedResourceMetadata` | [`discovery::discover_protected_resource_metadata`] |
//! | 9 | `parseWwwAuthenticate` | [`discovery::parse_www_authenticate`] |
//! | 10 | `resourceUrlFromServerUrl` | [`discovery::resource_url_from_server_url`] |
//! | 11 | `selectResource` | [`discovery::select_resource`] |
//! | 12–16 | `McpOAuthAuthorizationRequiredError`、`OAuthError`、`OAuthInsecureEndpointError`、`OAuthIssuerMismatchError`、`OAuthRegistrationError` | [`errors`] 的 5 个类型 |
//! | 17–29 | `AddClientAuthentication`、`adaptOAuthProvider`、`authorizeMcp`、`exchangeAuthorizationCode`、`OAuthClientMetadataDocument`、`OAuthClientProvider`、`OAuthFlowOptions`、`OAuthFlowResult`、`refreshAuthorization`、`registerClient`、`startAuthorization`、`stepUpScope`、`TokenRequestOptions` | `flow` 模块（其他单元） |
//! | 30–34 | `McpOAuthProvider`、`McpOAuthProviderOptions`、`McpOAuthState`、`McpOAuthStateStore`、`MemoryOAuthStateStore` | [`provider`] |
//! | 35–44 | `AuthorizationServerMetadata`、`OAuthChallenge`、`OAuthClientInformation`、`OAuthClientInformationFull`、`OAuthClientInformationMixed`、`OAuthClientMetadata`、`OAuthDiscoveryState`、`OAuthProtectedResourceMetadata`、`OAuthServerInfo`、`OAuthTokens` | [`types`] |
//!
//! 44 个名字按 TS 的**相对顺序**在下面再导出（`oauth/index.ts` 的导出面）。
//!
//! # 维护约束
//!
//! 再导出清单必须与 `oauth/index.ts` 逐名对应：`crates/kiana-mcp/tests/api_surface.rs` 用
//! `use kiana_mcp::oauth::{...}` 在编译期断言全部名字存在且路径正确。子模块里新增的 `pub`
//! 名字**不会**自动进入本模块的公开面；要公开必须同时改本文件、`spec/telemetry-mcp.md`
//! §19.2 与这份测试。反之，删掉子模块里的名字会让本文件编译失败，从而保证公开面不会
//! 悄悄偏离 TS 清单。
//!
//! # 刻意**不**再导出的名字（`spec/telemetry-mcp.md` §30.1）
//!
//! `safeUrl`、`object`、`compact`、`requiredString`、`absent`、`optionalString`、
//! `optionalStrings`、`isHttpUrl`、`parse*`（`oauth/types.ts`）与 `pathSuffix`、`isDiscoveryMiss`、
//! `field`、`fetchMetadata`、`discard`（`oauth/discovery.ts`）在 TS 里**没有**从
//! `src/oauth/index.ts` 导出：它们在 [`types`]/[`discovery`] 里是 `pub`（测试与包内按路径使用），
//! 但**不**进本模块的再导出面，否则 Rust 的公开 API 会大于 TS 包。
//!
//! # 许可
//!
//! `oauth/types.ts`、`oauth/discovery.ts`（以及 `oauth/flow.ts`）改编自
//! `modelcontextprotocol/typescript-sdk` v1.29.0 —— `packages/mcp/LICENSES/` 下的 MIT 文本：
//!
//! ```text
//! MIT License
//!
//! Copyright (c) 2024 Anthropic, PBC
//!
//! Permission is hereby granted, free of charge, to any person obtaining a copy
//! of this software and associated documentation files (the "Software"), to deal
//! in the Software without restriction, including without limitation the rights
//! to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
//! copies of the Software, and to permit persons to whom the Software is
//! furnished to do so, subject to the following conditions:
//!
//! The above copyright notice and this permission notice shall be included in all
//! copies or substantial portions of the Software.
//!
//! THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
//! IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
//! FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
//! AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
//! LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
//! OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
//! SOFTWARE.
//! ```
//!
//! 移植保留同样的归属：上述源文件的原始版权与许可文本见 TS 仓库的 `packages/mcp/LICENSES/`
//! 与本节。

pub mod callback;
pub mod discovery;
pub mod errors;
pub mod flow;
pub mod provider;
pub mod types;

// --- oauth/callback.ts（第 1–4 个） -----------------------------------------
pub use callback::{
    OAuthCallback, OAuthCallbackPage, OAuthCallbackServer, OAuthCallbackServerOptions,
};

// --- oauth/discovery.ts（第 5–11 个） ---------------------------------------
pub use discovery::{
    build_authorization_server_discovery_urls, discover_authorization_server_metadata,
    discover_oauth_server_info, discover_protected_resource_metadata, parse_www_authenticate,
    resource_url_from_server_url, select_resource,
};

// --- oauth/errors.ts（第 12–16 个） -----------------------------------------
pub use errors::{
    McpOAuthAuthorizationRequiredError, OAuthError, OAuthInsecureEndpointError,
    OAuthIssuerMismatchError, OAuthRegistrationError,
};

// --- oauth/flow.ts（第 17–29 个） ------------------------------------------
pub use flow::{
    AddClientAuthentication, OAuthClientMetadataDocument, OAuthClientProvider, OAuthFlowOptions,
    OAuthFlowResult, TokenRequestOptions, adapt_oauth_provider, authorize_mcp,
    exchange_authorization_code, refresh_authorization, register_client, start_authorization,
    step_up_scope,
};

// --- oauth/provider.ts（第 30–34 个） ---------------------------------------
pub use provider::{
    McpOAuthProvider, McpOAuthProviderOptions, McpOAuthState, McpOAuthStateStore,
    MemoryOAuthStateStore,
};

// --- oauth/types.ts（第 35–44 个） ------------------------------------------
pub use types::{
    AuthorizationServerMetadata, OAuthChallenge, OAuthClientInformation,
    OAuthClientInformationFull, OAuthClientInformationMixed, OAuthClientMetadata,
    OAuthDiscoveryState, OAuthProtectedResourceMetadata, OAuthServerInfo, OAuthTokens,
};
