//! Web 搜索与抓取：引擎链、结果归一化、并行 fetch 与 SSRF 防护。
//!
//! TS 源（设计参照）：`packages/coding-agent/src/web/**`、`tools/WebFetchTool/**`、
//! `tools/WebSearchTool/**`。本模块只借鉴行为与状态机，实现按 Kiana 的类型重写。
//!
//! # 分层
//!
//! - [`ssrf`]：URL / IP / DNS 三层校验。任何网络请求发出前都必须先过这里。
//! - [`http`]：最小 HTTP 传输契约 [`http::WebTransport`]；生产实现
//!   [`http::ReqwestWebTransport`] 使用禁用代理、连接时校验 DNS 的专用 client，
//!   测试注入脚本化替身，因此本模块的单测不访问网络。
//! - [`search`]：`SearchEngine` trait、有序引擎链（首选 + fallback + 其余）、
//!   单引擎 3 次退避、整链 `max_retries`、`SearchResult`/`SearchResponse` 归一化，
//!   以及 Brave / Google CSE / Bing 三个官方 API 适配器。
//! - [`fetch`]：并行抓取、HTML → 文本、10k 字符截断、每个 URL 独立的 SSRF 校验。
//!
//! # 安全边界
//!
//! 静态校验只能挡住 IP 字面量与已知元数据域名；域名可能在 DNS 阶段解析到内网地址。
//! 因此分三层：
//!
//! 1. 发送前 [`ssrf::resolve_public_host`] 预检，任一被禁地址即拒绝；
//! 2. 传输层使用手动重定向（`Policy::none()`），每一跳重新执行
//!    [`ssrf::validate_url`] + [`ssrf::resolve_public_host`]，被禁的下一跳**不会发出**；
//!    跨源（含 https → http）重定向会清空调用方 header 并把请求降级为 GET、丢弃 body，
//!    避免 API 凭据泄露给第三方；
//! 3. 连接时 [`http`] 的 resolver 再次校验 DNS 结果，并禁用环境变量代理（代理会在
//!    代理端解析域名），关闭预检与连接之间的 DNS rebinding 窗口。
//!
//! 响应体按块读取并累计上限，超过 [`MAX_FETCH_BYTES`] 立即失败，不会先把整个 body
//! 读进内存。
//!
//! # 默认值
//!
//! - `MAX_FETCH_CHARS = 10_000`（按 Unicode 字符截断，不会切坏 UTF-8）；
//! - `MAX_FETCH_BYTES = 2 MiB`（响应体硬上限）；
//! - 单引擎尝试 3 次，退避 250ms 起指数增长；
//! - 整链最多 6 次重试（不含每个引擎的首次尝试）。

pub mod fetch;
pub mod http;
pub mod search;
pub mod ssrf;
pub mod tools;

pub use tools::{web_fetch_tool_definition, web_search_tool_definition, web_tools_extension};

pub use fetch::{
    FetchBatch, FetchFailure, FetchOptions, FetchedPage, MAX_FETCH_BYTES, MAX_FETCH_CHARS,
    MAX_FETCH_URLS, fetch_urls, html_to_text, truncate_chars,
};
pub use http::{
    ReqwestWebTransport, WebHttpRequest, WebHttpResponse, WebTransport, WebTransportError,
};
pub use search::{
    BingSearchEngine, BraveSearchEngine, EngineFailure, EngineRole, GoogleCseEngine, Recency,
    SearchChain, SearchChainConfig, SearchChainError, SearchEngine, SearchEngineError, SearchQuery,
    SearchResponse, SearchResult,
};
pub use ssrf::{
    ParsedUrl, UrlError, is_blocked_ip, is_metadata_ip, resolve_public_host, validate_url,
};
