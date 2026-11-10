//! auth/oauth/load.ts——九个 OAuth 流程的加载入口。
//!
//! TS 用变量 specifier 的 `import()` 把 Node-only 流程代码挡在浏览器 bundle 之外；Rust 没有
//! 动态 import，`load_*` 直接返回内置实现（每个流程一个模块级单例，与 TS 的模块级
//! `export const xxxOAuth` 一致）。`register_bundled_oauth_flow_loaders` 保留为**可复写**的
//! 全局（后注册覆盖先注册），`bun-oauth.ts` 的 `registerBunOAuthFlows()` 用它显式登记同一组
//! 实现；未注册时 `load_*` 回落到内置实现。
//!
//! TS: `src/auth/oauth/load.ts`
//!
//! # 为什么还要保留这张“登记表”
//!
//! TS 侧未登记时用变量 specifier 的 `import()` 懒加载对应模块；Bun 单文件二进制里动态
//! specifier 会失效，所以 `bun-oauth.ts` 调用 `registerBundledOAuthFlowLoaders` 抢先登记
//! 实现。Rust 侧所有实现都静态链接、内置回落永远可用；保留这张表是为了维持同一 API 形状
//! （显式初始化点与测试替换），而不是运行必需。
//!
//! # 加载流程
//!
//! 每个 `load_*` 走同一模式：
//!
//! 1. `bundled_loaders` 在锁内克隆全局表的 `Arc`，随即释放锁；
//! 2. 有登记表 → 调用表里对应的闭包（闭包自己决定返回哪个实现）；
//! 3. 没有 → `Arc::clone` 模块级 `LazyLock` 单例。
//!
//! 两条路径都返回立即可用的 [`LazyOAuthFuture`]（`Ok`），不会失败。
//!
//! # 并发与全局状态
//!
//! - 八个单例用 `LazyLock` 惰性构造（radius 没有单例，按调用新建）：首次被解引用时才解析
//!   依赖（含 `default_oauth_fetch()`），之后同进程内共享。测试若用
//!   `set_default_oauth_fetch_for_tests`，必须在第一次加载前。
//! - 登记表是 `Mutex<Option<Arc<OAuthFlowLoaders>>>`：读路径只在锁内做快照，调用闭包发生
//!   在锁外，因此闭包内部再调 `load_*` 不会自锁。后注册覆盖先注册。
//! - `LazyOAuth`（`auth/helpers.rs`）用 `OnceCell` 记忆加载结果（成功与失败都记），因此
//!   这里每次返回新 future 不会让 provider 反复构造流程。
//!
//! # 与 TS 的差异
//!
//! - `get_oauth_auth(id)` 是按 id 查询的 Rust 侧入口（TS 只有九个具名 loader），并且
//!   **不经过**登记表，总是返回内置实现；radius 是唯一每次新建实例的流程。
//! - TS 的 loader 返回 `OAuthAuth | Promise<OAuthAuth>`，这里统一成 future。

use std::sync::{Arc, LazyLock, Mutex};

use crate::auth::helpers::LazyOAuthFuture;
use crate::auth::oauth::anthropic::AnthropicOAuth;
use crate::auth::oauth::github_copilot::GitHubCopilotOAuth;
use crate::auth::oauth::kimi_coding::KimiCodingOAuth;
use crate::auth::oauth::meta::MetaOAuth;
use crate::auth::oauth::openai_chatgpt::OpenAIChatGPTOAuth;
use crate::auth::oauth::openai_codex::OpenAICodexOAuth;
use crate::auth::oauth::openrouter::OpenRouterOAuth;
use crate::auth::oauth::radius::{RadiusOAuthOptions, create_radius_oauth};
use crate::auth::oauth::xai::XaiOAuth;
use crate::auth::types::OAuthAuth;
use crate::providers::radius_config::DEFAULT_RADIUS_GATEWAY;

/// `OAuthFlowLoaders`：可替换的流程加载表。
///
/// TS 的 `load` 可以是同步值或 Promise；Rust 侧统一返回 `Arc`（没有异步 import 的必要）。
///
/// 每个字段是一个进程级可替换的工厂。闭包必须 `Send + Sync`：表放在全局 `Mutex` 里、
/// 会被任意线程读取。`builtin_oauth_flow_loaders()` 的前八个条目返回模块单例；`radius`
/// 带参数，闭包每次调用都新建实例（不按 gateway 缓存），只有上层 `lazy_oauth` 会按 provider
/// 缓存一次加载结果。表本身不缓存结果。
pub struct OAuthFlowLoaders {
    /// `anthropic`。
    pub anthropic: Arc<dyn Fn() -> Arc<dyn OAuthAuth> + Send + Sync>,
    /// `openaiCodex`。
    pub openai_codex: Arc<dyn Fn() -> Arc<dyn OAuthAuth> + Send + Sync>,
    /// `openaiChatGPT`。
    pub openai_chatgpt: Arc<dyn Fn() -> Arc<dyn OAuthAuth> + Send + Sync>,
    /// `githubCopilot`。
    pub github_copilot: Arc<dyn Fn() -> Arc<dyn OAuthAuth> + Send + Sync>,
    /// `openrouter`。
    pub openrouter: Arc<dyn Fn() -> Arc<dyn OAuthAuth> + Send + Sync>,
    /// `kimiCoding`。
    pub kimi_coding: Arc<dyn Fn() -> Arc<dyn OAuthAuth> + Send + Sync>,
    /// `meta`。
    pub meta: Arc<dyn Fn() -> Arc<dyn OAuthAuth> + Send + Sync>,
    /// `xai`。
    pub xai: Arc<dyn Fn() -> Arc<dyn OAuthAuth> + Send + Sync>,
    /// `radius(options)`。
    ///
    /// 唯一带参数的条目：`name` 与 `gateway` 由调用方给出，每次调用可产生不同实例。
    pub radius: Arc<dyn Fn(RadiusOAuthOptions) -> Arc<dyn OAuthAuth> + Send + Sync>,
}

/// 模块级单例（TS 的 `export const anthropicOAuth` 等）。
///
/// `LazyLock` 在**首次解引用**时构造，构造时会解析流程自己的默认依赖（如
/// `default_oauth_fetch()`），因此测试替身必须在那之前安装。多个 `load_*` 调用返回同一个
/// `Arc`（测试 `loaders_return_the_module_singletons` 用 `Arc::ptr_eq` 锁定）。
static ANTHROPIC: LazyLock<Arc<dyn OAuthAuth>> = LazyLock::new(|| Arc::new(AnthropicOAuth::new()));
static OPENAI_CODEX: LazyLock<Arc<dyn OAuthAuth>> =
    LazyLock::new(|| Arc::new(OpenAICodexOAuth::new()));
static OPENAI_CHATGPT: LazyLock<Arc<dyn OAuthAuth>> =
    LazyLock::new(|| Arc::new(OpenAIChatGPTOAuth::new()));
static GITHUB_COPILOT: LazyLock<Arc<dyn OAuthAuth>> =
    LazyLock::new(|| Arc::new(GitHubCopilotOAuth::new()));
static OPENROUTER: LazyLock<Arc<dyn OAuthAuth>> =
    LazyLock::new(|| Arc::new(OpenRouterOAuth::new()));
static KIMI_CODING: LazyLock<Arc<dyn OAuthAuth>> =
    LazyLock::new(|| Arc::new(KimiCodingOAuth::new()));
static META: LazyLock<Arc<dyn OAuthAuth>> = LazyLock::new(|| Arc::new(MetaOAuth::new()));
static XAI: LazyLock<Arc<dyn OAuthAuth>> = LazyLock::new(|| Arc::new(XaiOAuth::new()));

/// `bundledLoaders`：`None` 时回落到内置实现。
///
/// 锁内只做快照克隆，`bundled_loaders` 返回后即释放锁；真正调用闭包发生在锁外，所以闭包
/// 内部再读这张表不会死锁。锁中毒时取回内层值（与仓库其它全局表一致），不让一个 panic 的
/// 测试永久毒化后续用例。
static BUNDLED_LOADERS: Mutex<Option<Arc<OAuthFlowLoaders>>> = Mutex::new(None);

/// `registerBundledOAuthFlowLoaders(loaders)`：后注册覆盖先注册。
///
/// TS: `src/auth/oauth/load.ts#registerBundledOAuthFlowLoaders`
///
/// 覆盖是整体替换：旧表被丢弃，但已克隆出去的 `Arc` 仍可继续使用。并发调用时最终留下
/// 哪一张由 `Mutex` 的写入顺序决定，没有合并语义。
pub fn register_bundled_oauth_flow_loaders(loaders: OAuthFlowLoaders) {
    *BUNDLED_LOADERS
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = Some(Arc::new(loaders));
}

/// 清除已注册的 bundled loaders（测试用；TS 没有对应导出）。
///
/// 恢复成“未登记”状态后，`load_*` 回落到内置单例。测试若注册过自定义表，必须在收尾时
/// 调用它，否则同进程内后续用例会看到残留（`bun_oauth` 的测试即如此收尾）。
pub fn clear_bundled_oauth_flow_loaders() {
    *BUNDLED_LOADERS
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = None;
}

/// 当前登记表的快照（`None` = 未登记，回落到内置单例）。
///
/// 唯一的读路径：在锁内 `clone` 出 `Arc` 后立刻释放锁，调用方随后在锁外调用闭包。
fn bundled_loaders() -> Option<Arc<OAuthFlowLoaders>> {
    BUNDLED_LOADERS
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone()
}

/// 内置实现组成的 [`OAuthFlowLoaders`]（`bun-oauth.ts` 的登记内容）。
///
/// 每次调用新建一张表，但闭包只 `Arc::clone` 模块单例，反复登记不会创建新流程实例；
/// `radius` 例外，它每次调用 [`create_radius_oauth`] 新建实例（与 TS 的
/// `createRadiusOAuth(options)` 一致）。
pub fn builtin_oauth_flow_loaders() -> OAuthFlowLoaders {
    OAuthFlowLoaders {
        anthropic: Arc::new(|| Arc::clone(&ANTHROPIC)),
        openai_codex: Arc::new(|| Arc::clone(&OPENAI_CODEX)),
        openai_chatgpt: Arc::new(|| Arc::clone(&OPENAI_CHATGPT)),
        github_copilot: Arc::new(|| Arc::clone(&GITHUB_COPILOT)),
        openrouter: Arc::new(|| Arc::clone(&OPENROUTER)),
        kimi_coding: Arc::new(|| Arc::clone(&KIMI_CODING)),
        meta: Arc::new(|| Arc::clone(&META)),
        xai: Arc::new(|| Arc::clone(&XAI)),
        radius: Arc::new(create_radius_oauth),
    }
}

/// 把已就绪的实例包成立即完成的 [`LazyOAuthFuture`]。
///
/// TS 的 loader 可以是同步值或 Promise；这里统一成 future，调用方无需关心分支。
fn ready(auth: Arc<dyn OAuthAuth>) -> LazyOAuthFuture {
    Box::pin(async move { Ok(auth) })
}

/// `loadAnthropicOAuth()`。
///
/// 有登记表时调用 `anthropic` 条目，否则返回模块单例 `ANTHROPIC`。
pub fn load_anthropic_oauth() -> LazyOAuthFuture {
    if let Some(loaders) = bundled_loaders() {
        return ready((loaders.anthropic)());
    }
    ready(Arc::clone(&ANTHROPIC))
}

/// `loadOpenAICodexOAuth()`。
///
/// 有登记表时调用 `openai_codex` 条目，否则返回模块单例 `OPENAI_CODEX`。
pub fn load_openai_codex_oauth() -> LazyOAuthFuture {
    if let Some(loaders) = bundled_loaders() {
        return ready((loaders.openai_codex)());
    }
    ready(Arc::clone(&OPENAI_CODEX))
}

/// `loadOpenAIChatGPTOAuth()`。
///
/// 有登记表时调用 `openai_chatgpt` 条目，否则返回模块单例 `OPENAI_CHATGPT`。
pub fn load_openai_chatgpt_oauth() -> LazyOAuthFuture {
    if let Some(loaders) = bundled_loaders() {
        return ready((loaders.openai_chatgpt)());
    }
    ready(Arc::clone(&OPENAI_CHATGPT))
}

/// `loadGitHubCopilotOAuth()`。
///
/// 有登记表时调用 `github_copilot` 条目，否则返回模块单例 `GITHUB_COPILOT`。
pub fn load_github_copilot_oauth() -> LazyOAuthFuture {
    if let Some(loaders) = bundled_loaders() {
        return ready((loaders.github_copilot)());
    }
    ready(Arc::clone(&GITHUB_COPILOT))
}

/// `loadOpenRouterOAuth()`。
///
/// 有登记表时调用 `openrouter` 条目，否则返回模块单例 `OPENROUTER`。
pub fn load_openrouter_oauth() -> LazyOAuthFuture {
    if let Some(loaders) = bundled_loaders() {
        return ready((loaders.openrouter)());
    }
    ready(Arc::clone(&OPENROUTER))
}

/// `loadKimiCodingOAuth()`。
///
/// 有登记表时调用 `kimi_coding` 条目，否则返回模块单例 `KIMI_CODING`。
pub fn load_kimi_coding_oauth() -> LazyOAuthFuture {
    if let Some(loaders) = bundled_loaders() {
        return ready((loaders.kimi_coding)());
    }
    ready(Arc::clone(&KIMI_CODING))
}

/// `loadMetaOAuth()`。
///
/// 有登记表时调用 `meta` 条目，否则返回模块单例 `META`。
pub fn load_meta_oauth() -> LazyOAuthFuture {
    if let Some(loaders) = bundled_loaders() {
        return ready((loaders.meta)());
    }
    ready(Arc::clone(&META))
}

/// `loadXaiOAuth()`。
///
/// 有登记表时调用 `xai` 条目，否则返回模块单例 `XAI`。
pub fn load_xai_oauth() -> LazyOAuthFuture {
    if let Some(loaders) = bundled_loaders() {
        return ready((loaders.xai)());
    }
    ready(Arc::clone(&XAI))
}

/// `loadRadiusOAuth({name, gateway})`。
///
/// `options` 原样交给登记表的 `radius` 闭包；未登记时走 [`create_radius_oauth`]，每次调用
/// 创建一个新实例（其余八个流程共享单例）。
pub fn load_radius_oauth(options: RadiusOAuthOptions) -> LazyOAuthFuture {
    if let Some(loaders) = bundled_loaders() {
        return ready((loaders.radius)(options));
    }
    ready(create_radius_oauth(options))
}

/// 按 provider id（或流程名）取内置 OAuth 流程（懒加载注册表的等价入口）。
///
/// 认得的 id：`anthropic`、`openai-codex`/`openai_codex`、`openai`/`openai-chatgpt`/
/// `openai_chatgpt`、`github-copilot`/`github_copilot`、`openrouter`、`kimi-coding`/
/// `kimi_coding`、`meta`、`xai`、`radius`（默认名 `Radius` 与默认 gateway）。
///
/// 这是 Rust 侧新增的按 id 查询入口（TS 只有九个具名 loader）：**不经过**登记表，因此用
/// [`register_bundled_oauth_flow_loaders`] 替换的实现不会在这里生效。`radius` 每次调用都
/// 新建实例，其余返回模块单例；未知 id 返回 `None`。
pub fn get_oauth_auth(id: &str) -> Option<Arc<dyn OAuthAuth>> {
    match id {
        "anthropic" => Some(Arc::clone(&ANTHROPIC)),
        "openai-codex" | "openai_codex" => Some(Arc::clone(&OPENAI_CODEX)),
        "openai" | "openai-chatgpt" | "openai_chatgpt" => Some(Arc::clone(&OPENAI_CHATGPT)),
        "github-copilot" | "github_copilot" => Some(Arc::clone(&GITHUB_COPILOT)),
        "openrouter" => Some(Arc::clone(&OPENROUTER)),
        "kimi-coding" | "kimi_coding" => Some(Arc::clone(&KIMI_CODING)),
        "meta" => Some(Arc::clone(&META)),
        "xai" => Some(Arc::clone(&XAI)),
        "radius" => Some(create_radius_oauth(RadiusOAuthOptions {
            name: "Radius".into(),
            gateway: DEFAULT_RADIUS_GATEWAY.into(),
        })),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn loaders_return_the_module_singletons() {
        let first = load_anthropic_oauth().await.expect("anthropic");
        let second = load_anthropic_oauth().await.expect("anthropic");
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(first.name(), "Anthropic (Claude Pro/Max)");
    }

    #[tokio::test]
    async fn radius_loader_normalizes_the_gateway() {
        let oauth = load_radius_oauth(RadiusOAuthOptions {
            name: "Company".into(),
            gateway: "radius.example///".into(),
        })
        .await
        .expect("radius");
        assert_eq!(oauth.name(), "Company");
    }

    #[test]
    fn get_oauth_auth_recognizes_provider_ids() {
        assert_eq!(
            get_oauth_auth("anthropic").map(|auth| auth.name().to_string()),
            Some("Anthropic (Claude Pro/Max)".into())
        );
        assert_eq!(
            get_oauth_auth("openai").map(|auth| auth.name().to_string()),
            Some("OpenAI (ChatGPT subscription)".into())
        );
        assert!(get_oauth_auth("unknown").is_none());
    }
}
