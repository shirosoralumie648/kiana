//! utils/identity.ts。
//!
//! TS: `src/utils/identity.ts`。`ClientIdentity` / `ClientAttribution` / `UserAgentMode` /
//! `ClientIdentityTarget` 的类型定义在 `types.rs`（`ClientIdentity` 需要 serde 三态
//! `attribution?: T | null`）；本模块提供派生函数与 `withClientIdentity` 包装。
//!
//! **运行期描述**：TS 读 `process.versions`（bun 优先，其次 node）。Rust 进程没有这些字段，
//! 因此按同一优先级探测宿主运行时（`bun --version` / `node --version`，首次使用时执行一次）；
//! 都不可用（或 wasm 目标）时走 TS 的 `(browser)` 分支。见 [`runtime_description`]。
//!
//! # 这个模块解决什么问题
//!
//! 多数适配器在请求头里声明“是谁在调用”：一个 `User-Agent`，
//! 以及可选的服务商专用归属头（OpenRouter 的 `HTTP-Referer`/`X-OpenRouter-*`、NVIDIA 的
//! `X-BILLING-INVOKE-ORIGIN`、OpenCode 的 `x-opencode-client` / `x-opencode-session`）。
//! 这些头由三层身份合并而来：
//!
//! 1. 创建 [`crate::models::Models`] 时的默认 identity（`CreateModelsOptions.identity`）；
//! 2. [`with_client_identity`] 包装器注入的 identity；
//! 3. 单次请求的 `StreamOptions.identity`（最高优先级）。
//!
//! 合并一律走 [`merge_client_identity`]（逐字段覆盖；`attribution` 的三态见该函数）。
//! 请求路径在 `models::apply_auth` 中只取出最终 identity，多数适配器再调用
//! [`get_identity_headers`] 组装实际请求头（少数适配器会覆盖 UA，见 [`format_user_agent`]）。
//!
//! # 主要流程
//!
//! - UA：[`format_user_agent`] 生成 `kiana/<version> (<platform>; <runtime>; <arch>)`；
//!   `short` 模式只输出 `kiana`，`{ custom }` 原样输出，运行时不可用时输出
//!   `kiana/<version> (browser)`。
//! - attribution：[`get_attribution_headers`] 按 OpenRouter → NVIDIA → OpenCode 的顺序判定
//!   目标，命中第一个即返回；identity 没有 attribution（`None` 或 `Some(None)`）时返回空表。
//! - 会话头：[`get_open_code_session_headers`] 只在目标属于 OpenCode 且会话 id 非空时返回
//!   `x-opencode-session`。
//! - 包装器：[`with_client_identity`] 不复制 `Models`，只是把所有方法委托给原对象，并在
//!   请求类方法上把默认 identity 合并进 `options`。
//!
//! 仓库内主要使用方：`kiana-ai` 的 `models::apply_auth` 与各 `api/*` 适配器、`kiana-config`
//! （客户端身份配置与 OpenCode 会话头）、`kiana-agent-core`（会话级 identity 合并）、
//! `kiana-durable`（harness 的 `with_client_identity` 包装）、`kiana-core` / `kiana-resources`
//! （UA 与运行时描述）。
//!
//! # 重要约束与失败模式
//!
//! - **运行时探测有副作用与成本**：[`runtime_description`] 第一次调用会尝试执行
//!   `bun --version`，失败再执行 `node --version`；结果（包括“都没有”）被 [`LazyLock`]
//!   永久缓存。找不到可执行文件、退出码非 0、stdout 为空都视为不可用，UA 退回 `(browser)`。
//!   探测没有超时：若 `bun`/`node` 可执行文件挂起，首次调用会一直等待。
//! - **目标判定混用两种规则**：OpenRouter 用**子串**匹配 `openrouter.ai`（对齐 TS 的
//!   `baseUrl.includes(...)`，因此 `not-a-url-openrouter.ai` 这类非法 URL 也会命中）；
//!   NVIDIA/OpenCode 用 `url::Url` 解析后的 **hostname 精确相等**（解析失败为 false）。
//!   两者不能互相替换，修改前先看 `spec/ai-core.md` §2.2.22。
//! - **头名大小写与顺序是契约**：`get_identity_headers` 先写 `User-Agent`，再写 attribution；
//!   测试 `identity_headers_put_user_agent_first` 锁定该顺序。header 名沿用各渠道要求的
//!   原始大小写，后续由 `models::merge_headers` / `provider_headers_to_record` 做大小写不敏感
//!   覆盖。
//! - 本模块的头部辅助函数只生成头；identity 合并与 [`with_client_identity`] 只做内存合并和
//!   透明委托，不直接发 HTTP 请求、不直接读写凭证。`get_auth` 不经过 identity 合并（见
//!   [`ClientIdentityModels`] 的说明）。

use std::sync::{Arc, LazyLock};

use async_trait::async_trait;

use crate::auth::resolve::AuthResolutionOverrides;
use crate::auth::types::{
    AuthCheck, AuthError, AuthInteraction, AuthOperationOptions, AuthResult, AuthType, Credential,
    LoginOptions,
};
use crate::error::AiError;
use crate::event_stream::AssistantMessageEventStream;
use crate::models::{
    GetAuthTarget, Models, ModelsClassifierOptions, ModelsDeferredCancelOptions,
    ModelsDeferredFetchOptions, ModelsImagesOptions, ModelsRefreshOptions, ModelsRefreshResult,
    ModelsSimpleStreamOptions, ModelsStreamOptions, MutableModels,
};
use crate::types::{
    AnyModel, AssistantImages, AssistantMessage, ClassifierContext, ClassifierResult,
    ClientAttribution, ClientIdentity, Context, DeferredHandle, ImagesContext, Model, ModelType,
    ProviderHeaders, UserAgentMode,
};
use crate::utils::version::KIANA_AI_VERSION;

/// kiana 的 attribution（`kiana-agent-core`、`kiana-durable`、coding agent 使用）。
///
/// 用 [`LazyLock`] 是为了首次访问时构造一次，之后全进程共享同一份只读值；底层构造函数
/// [`crate::types::kiana_attribution`] 每次返回新副本，需要修改 attribution 的调用方应
/// `clone()` 静态量而不是试图修改它（静态量本身没有可变入口）。
///
/// 内容与 TS 的 `KIANA_ATTRIBUTION` 一致：`name = "kiana"`、`url = "https://kiana.dev"`、
/// `categories = ["cli-agent"]`（测试与 `spec/ai-core.md` §2.2.22 均引用该值）。
///
/// TS: `src/utils/identity.ts#KIANA_ATTRIBUTION`
pub static KIANA_ATTRIBUTION: LazyLock<ClientAttribution> =
    LazyLock::new(crate::types::kiana_attribution);

/// 请求目标的 provider 与端点。
///
/// 只用于选择 attribution 渠道与是否添加 OpenCode 会话头，不参与请求 URL 的构造。两个字段
/// 都按原值参与判定：`provider` 名优先于 URL（例如 provider 为 `openrouter` 时即使 base URL
/// 指向别处也算 OpenRouter）；反之 URL 也能让自定义 provider 命中渠道。
///
/// TS: `src/utils/identity.ts#ClientIdentityTarget`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientIdentityTarget {
    /// provider id。
    pub provider: String,
    /// 请求的 base URL；OpenRouter 做子串匹配，NVIDIA/OpenCode 做 hostname 精确匹配。
    pub base_url: String,
}

impl ClientIdentityTarget {
    /// 构造目标。
    pub fn new(provider: impl Into<String>, base_url: impl Into<String>) -> Self {
        ClientIdentityTarget {
            provider: provider.into(),
            base_url: base_url.into(),
        }
    }
}

const NVIDIA_NIM_HOST: &str = "integrate.api.nvidia.com";
const OPENCODE_HOST: &str = "opencode.ai";

/// 逐字段合并身份；后者优先，`undefined` 字段跳过（`null` attribution **会**覆盖）。
///
/// # 参数与返回
///
/// 按迭代顺序处理，`None` 源被跳过，`Some` 源中为 `None` 的字段表示“本次不设置”，保留
/// 之前的值；`attribution = Some(None)` 表示显式清空（TS 的 JSON `null`），会覆盖之前的值。
///
/// 只要出现过至少一个 `Some` 源就返回 `Some`——即使该源所有字段都是缺省值，结果也是一个
/// 全 `None` 的结构；只有所有源都是 `None` 才返回 `None`。
///
/// # 所有权
///
/// 每个源按值传入，字段被移动进结果，因此调用后不能再使用源；需要保留原值时先 `clone()`
/// （[`with_client_identity`] 的包装器在每次请求上都克隆自己的 identity）。
///
/// TS: `src/utils/identity.ts#mergeClientIdentity`
pub fn merge_client_identity(
    sources: impl IntoIterator<Item = Option<ClientIdentity>>,
) -> Option<ClientIdentity> {
    let mut merged: Option<ClientIdentity> = None;
    for source in sources {
        let Some(source) = source else {
            continue;
        };
        let target = merged.get_or_insert_with(ClientIdentity::default);
        if source.user_agent.is_some() {
            target.user_agent = source.user_agent;
        }
        if source.attribution.is_some() {
            target.attribution = source.attribution;
        }
        if source.version.is_some() {
            target.version = source.version;
        }
    }
    merged
}

/// `process.platform` 的 Node 取值。
///
/// 只映射与 Rust `std::env::consts::OS` 不同的常见平台（`macos` → `darwin`、`windows` →
/// `win32`）；其余值原样返回。结果会进入 UA 的 `<platform>` 段，是服务端可见的客户端指纹，
/// 不要为了“看起来更整齐”而改动映射。
pub fn node_platform() -> &'static str {
    match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    }
}

/// `process.arch` 的 Node 取值。
///
/// 与 [`node_platform`] 一样只映射常见架构（`x86_64` → `x64`、`x86` → `ia32`、
/// `aarch64` → `arm64` 等），未列出的值原样返回。`std::env::consts::ARCH` 的取值集合与
/// Node 的 `process.arch` 并不完全重合，未覆盖组合是否与 Node 同名未逐一核对。
pub fn node_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x64",
        "x86" => "ia32",
        "aarch64" => "arm64",
        "powerpc64" => "ppc64",
        "loongarch64" => "loong64",
        other => other,
    }
}

/// 探测单个运行时的版本（`bun --version` 不带 `v`，`node --version` 带 `v`，与
/// `process.versions.bun` / `process.version` 一致）。
///
/// 执行 `<executable> --version`，把 stdout 整段 `trim` 后返回。失败模式：可执行文件不存在
/// 或无权限（`output()` 返回 `Err`）、退出码非 0、stdout 为空 → `None`。stdin/stderr 都接到
/// null，避免子进程等待输入或在终端留下输出。
///
/// **没有超时控制**：`Command::output()` 会一直等到子进程退出；若探测程序挂起，首次 UA
/// 生成也会挂起（结果由 [`runtime_description`] 缓存，后续调用不会再探测）。
fn probe_runtime_version(executable: &str) -> Option<String> {
    let output = std::process::Command::new(executable)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let version = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if version.is_empty() {
        None
    } else {
        Some(version)
    }
}

/// `process.versions` 的等价物：detailed `User-Agent` 里的 `<runtime>/<version>`。
///
/// TS 在同一次进程里读 `process.versions.bun`（优先）与 `process.versions.node`。
/// Rust 进程本身没有这些字段，因此按同一优先级探测**宿主**可用的 JS 运行时：
/// `bun --version` → `bun/<ver>`，否则 `node --version` → `node/<ver>`。
/// 两者都不可用，或编译目标是 wasm（等价 TS 的 `typeof process === "undefined"`）时返回
/// `None`，UA 使用 `kiana/<version> (browser)`。
///
/// 探测在首次调用本函数（或 [`format_user_agent`]，它总会先求值本函数）时执行一次，结果
/// 由 [`LazyLock`] 在进程内缓存。否定结果同样被缓存：一旦首次探测时两个运行时都不可用，
/// 之后即使 PATH 里出现 `bun`/`node` 也不会重新探测。
///
/// 每次调用返回缓存字符串的克隆（内容很短），调用方可以自由持有；缓存本身不会被清空。
pub fn runtime_description() -> Option<String> {
    static DETECTED: LazyLock<Option<String>> = LazyLock::new(detect_runtime);
    DETECTED.clone()
}

fn detect_runtime() -> Option<String> {
    // wasm 目标下不尝试启动外部进程，等价 TS 的 `typeof process === "undefined"`，直接走
    // browser 分支。探测顺序必须是 bun 优先，才能与 `process.versions.bun ?? process.version`
    // 的取值优先级一致。
    if cfg!(target_arch = "wasm32") {
        return None;
    }
    if let Some(version) = probe_runtime_version("bun") {
        return Some(format!("bun/{version}"));
    }
    probe_runtime_version("node").map(|version| format!("node/{version}"))
}

/// `getClientVersion(identity?)`：`identity?.version ?? KIANA_AI_VERSION`。
///
/// 返回拥有所有权的字符串。[`crate::utils::version::KIANA_AI_VERSION`] 是常量，identity
/// 缺省或 `version` 为 `None` 时用它；空字符串是合法值，会原样返回（TS 的 `??` 不把空串
/// 当作缺省）。
///
/// TS: `src/utils/identity.ts#getClientVersion`
pub fn get_client_version(identity: Option<&ClientIdentity>) -> String {
    identity
        .and_then(|identity| identity.version.clone())
        .unwrap_or_else(|| KIANA_AI_VERSION.to_owned())
}

/// `formatUserAgent(identity?)` 的运行时无关部分（`runtime` 为 `None` 对应 browser）。
///
/// 分支优先级（靠前的规则优先）：
/// 1. `{ custom }` → 原样返回（不解析、不拼接）；
/// 2. `"short"` → `"kiana"`（不带版本号与平台）；
/// 3. 其余（`"detailed"`、缺省、以及 `Short` 里除 `"short"` 之外的任意字符串）→ 详细格式。
///
/// 详细格式固定为 `kiana/<version> (<platform>; <runtime>; <arch>)`；没有 runtime 时退化为
/// `kiana/<version> (browser)`。三段的分隔符与顺序都不能改：测试用 `assert_eq!` 比较完整的
/// UA 字符串，`spec/ca-cli.md` §2.21.4 也规定了默认格式，改动需同步该规范与测试。
fn format_user_agent_with_runtime(
    identity: Option<&ClientIdentity>,
    runtime: Option<&str>,
) -> String {
    let mode = identity.and_then(|identity| identity.user_agent.as_ref());
    match mode {
        Some(UserAgentMode::Custom { custom }) => custom.clone(),
        // TS 只在 `mode === "short"` 时返回 "kiana"；`Short` 里的其它字符串（运行期反序列化
        // 可能接受）会落到详细格式，这里用守卫复刻同一条件。
        Some(UserAgentMode::Short(value)) if value == "short" => "kiana".to_owned(),
        _ => {
            let product = format!("kiana/{}", get_client_version(identity));
            match runtime {
                Some(runtime) => {
                    format!(
                        "{product} ({}; {runtime}; {})",
                        node_platform(),
                        node_arch()
                    )
                }
                None => format!("{product} (browser)"),
            }
        }
    }
}

/// `formatUserAgent(identity?)`。
///
/// 多数适配器组装默认请求头时经 [`get_identity_headers`] 间接调用本函数；它也是本模块
/// 对外暴露的 UA 格式化入口。注意它并非全 crate 唯一写 UA 的地方：个别适配器会用固定值
/// 覆盖（例如 GitHub Copilot 的 `GitHubCopilotChat/...`、Anthropic 的 `Anthropic/JS ...`），
/// Bedrock 另有自己的 UA 分支（`api/bedrock_converse_stream.rs`）。
///
/// `identity` 为 `None` 或字段缺省时按 `detailed` 处理；`version` 缺省时使用
/// [`crate::utils::version::KIANA_AI_VERSION`]。除运行时探测外没有副作用。
///
/// 调用成本：首次调用会触发 [`runtime_description`] 的子进程探测（详见该函数的说明），之后
/// 只是读取缓存并做字符串格式化。即使 `short`/`custom` 模式最终不使用运行时信息，实参也会
/// 先被求值，因此首次调用同样会触发探测。
///
/// TS: `src/utils/identity.ts#formatUserAgent`
pub fn format_user_agent(identity: Option<&ClientIdentity>) -> String {
    format_user_agent_with_runtime(identity, runtime_description().as_deref())
}

/// `new URL(baseUrl).hostname === expectedHost`；解析失败为 false。
///
/// 只比较 hostname（忽略 scheme、端口与路径）；主机名按 `url` crate 的 WHATWG 规则归一化。
/// 解析失败一律返回 false 而不是报错，让调用方把“非法 URL”当作“不属于该渠道”。
fn matches_host(base_url: &str, expected_host: &str) -> bool {
    match url::Url::parse(base_url) {
        Ok(url) => url.host_str() == Some(expected_host),
        Err(_) => false,
    }
}

// TS `baseUrl.includes("openrouter.ai")`：子串匹配而非 hostname 比较，因此自定义 provider
// 使用形如 `not-a-url-openrouter.ai` 的 base URL 也会被归为 OpenRouter（测试锁定该行为）。
fn is_open_router(target: &ClientIdentityTarget) -> bool {
    target.provider == "openrouter" || target.base_url.contains("openrouter.ai")
}

// provider 名优先；否则要求 URL 解析成功且 hostname 精确等于 NVIDIA NIM 的域名。
fn is_nvidia_nim(target: &ClientIdentityTarget) -> bool {
    target.provider == "nvidia" || matches_host(&target.base_url, NVIDIA_NIM_HOST)
}

// OpenCode 有两个 provider 别名（`opencode` 与 `opencode-go`），URL 侧同样要求 hostname
// 精确相等；`opencode.ai.evil.example` 不会命中。
fn is_open_code(target: &ClientIdentityTarget) -> bool {
    target.provider == "opencode"
        || target.provider == "opencode-go"
        || matches_host(&target.base_url, OPENCODE_HOST)
}

/// `name.charAt(0).toUpperCase() + name.slice(1)`：只大写第一个 UTF-16 码元。
///
/// `charAt(0)` 取的是**码元**：若名字以非 BMP 字符（代理对）开头，`toUpperCase()` 保持该
/// 代理项不变，与 `slice(1)` 拼回后仍是原字符。按 Unicode 标量取首字符恰好复现该行为
/// （BMP 首字符时标量边界 = 码元边界）。
///
/// 其余部分用 `chars.as_str()` 原样拼回，不做第二次大写。`to_uppercase` 与 JS 的
/// `toUpperCase` 一样可能把一个字符扩成多个（例如 `ß` → `SS`），这是预期结果。
fn capitalize_first(name: &str) -> String {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    let mut capitalized: String = first.to_uppercase().collect();
    capitalized.push_str(chars.as_str());
    capitalized
}

/// `getAttributionHeaders(target, identity?)`。
///
/// 优先级：openrouter > nvidia > opencode。空 attribution 返回空 map。
///
/// # 返回值
///
/// 头名使用各渠道要求的原始大小写（OpenRouter 为 `HTTP-Referer`、`X-OpenRouter-Title`、
/// `X-OpenRouter-Categories`；NVIDIA 为 `X-BILLING-INVOKE-ORIGIN`；OpenCode 为
/// `x-opencode-client`），值都是 `Some`。没有 attribution 或目标不属于任何渠道时返回**空
/// 表**而不是 `None`，调用方可以直接遍历合并。
///
/// # 渠道判定
///
/// 三个分支互斥且按 OpenRouter → NVIDIA → OpenCode 顺序短路：命中第一个即返回，同时满足
/// 多个条件时不会叠加头。`X-OpenRouter-Categories` 只在 `categories` 非空时添加，
/// `X-BILLING-INVOKE-ORIGIN` 的值只大写首字符（实现见私有的 `capitalize_first`）。
///
/// TS: `src/utils/identity.ts#getAttributionHeaders`
pub fn get_attribution_headers(
    target: &ClientIdentityTarget,
    identity: Option<&ClientIdentity>,
) -> ProviderHeaders {
    let attribution =
        identity.and_then(|identity| identity.attribution.as_ref().and_then(|a| a.as_ref()));
    let Some(attribution) = attribution else {
        return ProviderHeaders::new();
    };
    if is_open_router(target) {
        let mut headers = ProviderHeaders::new();
        headers.insert("HTTP-Referer".into(), Some(attribution.url.clone()));
        headers.insert("X-OpenRouter-Title".into(), Some(attribution.name.clone()));
        if let Some(categories) = &attribution.categories
            && !categories.is_empty()
        {
            headers.insert("X-OpenRouter-Categories".into(), Some(categories.join(",")));
        }
        return headers;
    }
    // NVIDIA expects a capitalized origin, such as `Kiana`.
    if is_nvidia_nim(target) {
        let mut headers = ProviderHeaders::new();
        headers.insert(
            "X-BILLING-INVOKE-ORIGIN".into(),
            Some(capitalize_first(&attribution.name)),
        );
        return headers;
    }
    if is_open_code(target) {
        let mut headers = ProviderHeaders::new();
        headers.insert("x-opencode-client".into(), Some(attribution.name.clone()));
        return headers;
    }
    ProviderHeaders::new()
}

/// OpenCode 的会话路由头（仅当 `sessionId` 非空且目标是 OpenCode）。
///
/// `session_id` 为 `None` 或空串时返回 `None`；目标不是 OpenCode 时同样返回 `None`。返回值
/// 只有 `x-opencode-session` 一个键，值原样使用会话 id，不做编码或截断。
///
/// 这个头在 `models::apply_auth` 的头合并里位于最低优先级：provider auth、模型静态头与
/// 显式请求头都可以按大小写不敏感的方式覆盖它。
///
/// TS: `src/utils/identity.ts#getOpenCodeSessionHeaders`
pub fn get_open_code_session_headers(
    target: &ClientIdentityTarget,
    session_id: Option<&str>,
) -> Option<ProviderHeaders> {
    let session_id = session_id.filter(|session_id| !session_id.is_empty())?;
    if !is_open_code(target) {
        return None;
    }
    let mut headers = ProviderHeaders::new();
    headers.insert("x-opencode-session".into(), Some(session_id.to_owned()));
    Some(headers)
}

/// 身份默认头：`User-Agent` 在前，随后是 attribution。
///
/// # 顺序与覆盖
///
/// `User-Agent` 一定是第一个键，随后按 [`get_attribution_headers`] 的插入顺序追加；本函数
/// 不做大小写去重。调用方随后应使用 `models::merge_headers` 或
/// [`crate::utils::headers::provider_headers_to_record`] 做大小写不敏感合并：模型静态头与
/// 显式请求头会覆盖这里的默认值，`None` 表示抑制/删除。
///
/// `identity` 为 `None` 时仍会生成默认的详细 UA（运行时不可用时为 `(browser)`），因此
/// 返回值至少包含一个头。
///
/// TS: `src/utils/identity.ts#getIdentityHeaders`
pub fn get_identity_headers(
    target: &ClientIdentityTarget,
    identity: Option<&ClientIdentity>,
) -> ProviderHeaders {
    let mut headers = ProviderHeaders::new();
    headers.insert("User-Agent".into(), Some(format_user_agent(identity)));
    for (name, value) in get_attribution_headers(target, identity) {
        headers.insert(name, value);
    }
    headers
}

// ---------------------------------------------------------------------------
// withClientIdentity
// ---------------------------------------------------------------------------

/// `withClientIdentity` 的输入类型分派：`Models` 包装后仍是 `Models`，
/// `MutableModels` 包装后仍是 `MutableModels`（TS 的重载语义）。
///
/// Rust 没有函数重载，因此用 trait 把“输入类型 → 输出类型”的映射放进关联类型
/// [`Self::Output`]。目前只为 `Arc<dyn Models>` 与 `Arc<dyn MutableModels>` 实现；其它类型
/// 在编译期就不能调用 [`with_client_identity`]，这是有意的约束（包装结果本身也需要 `Arc`
/// 才能与原对象共享同一份 provider 集合）。
pub trait ClientIdentityModels {
    /// 包装后的类型。
    type Output;
    /// 实现体。
    ///
    /// 实现应返回一个“其余行为全部委托、请求方法合并 identity”的新对象；调用方通常不直接
    /// 调用本方法，而是用 [`with_client_identity`]。
    fn wrap_client_identity(self, identity: ClientIdentity) -> Self::Output;
}

// `Arc<dyn Models>` → 仍是 `Arc<dyn Models>`：包装后调用方无需知道具体实现类型。
impl ClientIdentityModels for Arc<dyn Models> {
    type Output = Arc<dyn Models>;
    fn wrap_client_identity(self, identity: ClientIdentity) -> Self::Output {
        Arc::new(IdentityModels {
            inner: self,
            identity,
        })
    }
}

// `Arc<dyn MutableModels>` → 仍是 `Arc<dyn MutableModels>`：可变性不因包装而丢失，写操作
// 仍作用在同一份共享状态上（测试 `merges_request_identity_over_the_models_and_wrapper_identities`
// 用 `delete_provider` 锁定这一点）。
impl ClientIdentityModels for Arc<dyn MutableModels> {
    type Output = Arc<dyn MutableModels>;
    fn wrap_client_identity(self, identity: ClientIdentity) -> Self::Output {
        Arc::new(IdentityMutableModels {
            inner: self,
            identity,
        })
    }
}

/// `models` 的请求默认身份覆盖：请求的 `identity` 逐字段覆盖它。
///
/// # 优先级
///
/// 包装器只把 `identity` 当作**默认值**：之后每次请求会把包装器的 identity 与
/// `options.identity` 逐字段合并（请求优先，见 `merge_options_identity`），再交给被包装的
/// `Models`；被包装对象自己的默认 identity 仍更底层。最终优先级：
/// 请求 > 包装器 > `Models` 创建时的默认值。
///
/// # 不变式
///
/// - 不复制原对象：读写都作用在同一份 provider 集合上（`Arc` 共享），包装期间对原对象的
///   `set_provider`/`delete_provider` 立即对包装器可见。
/// - `get_auth` 原样委托，**不**注入 identity（与 TS 的 `models.getAuth.bind(models)` 一致）；
///   `login`/`logout`/`refresh` 等非请求方法同样不参与 identity 合并。
/// - 若原对象是 `MutableModels`，返回的包装器仍可写（`set_provider`/`delete_provider`/
///   `clear_providers` 直接转发）。
///
/// # 失败模式
///
/// 本函数不会失败。风险在“忘记合并”：`models::Models` 新增请求方法时，若包装器的宏实现
/// 没有把 identity 合入新方法的 options，包装器的默认身份会被静默忽略（见 `delegate_models!`
/// 的说明）。
///
/// TS: `src/utils/identity.ts#withClientIdentity`（两个重载）
pub fn with_client_identity<M: ClientIdentityModels>(
    models: M,
    identity: ClientIdentity,
) -> M::Output {
    models.wrap_client_identity(identity)
}

// 包装器 identity 在前、请求 identity 在后：后者逐字段覆盖前者（见 merge_client_identity）。
// `take()` 先把原值从 options 里取走再合并，避免多一次克隆。
//
// 注意结果**总是 `Some`**：第一个源（包装器自己的 identity）固定存在，即使它的字段全缺省。
// 这会把请求 options 的 identity 从 `None` 变成 `Some(默认结构)`；对适配器而言两者等价
// （缺省字段按 detailed / 无 attribution 处理），但比较 `Option` 形状的调用方会看到差别。
fn merge_options_identity(
    identity: &ClientIdentity,
    base: Option<ClientIdentity>,
) -> Option<ClientIdentity> {
    merge_client_identity([Some(identity.clone()), base])
}

// `Arc<dyn Models>` 的包装器：持有默认 identity，除请求方法的 identity 合并外全部委托。
struct IdentityModels {
    inner: Arc<dyn Models>,
    identity: ClientIdentity,
}

// 兼有 `MutableModels` 的包装器：`Models` 部分与 IdentityModels 相同，写操作另行转发。
struct IdentityMutableModels {
    inner: Arc<dyn MutableModels>,
    identity: ClientIdentity,
}

/// 包装一个模型集合：除 `get_auth` 与写操作外的所有方法委托；请求方法合并 identity。
///
/// `get_auth` 也在这个 `impl` 里转发，只是**不**注入 identity（方法体内有 TS 对照注释）；
/// `refresh`/`check_auth`/`get_available*`/`login`/`logout` 等同样只是原样委托。
///
/// # 为什么用宏
///
/// 两个包装器（`IdentityModels` / `IdentityMutableModels`）的 `Models` 实现相同，
/// 只有 `MutableModels` 的写方法不同。宏生成两份 `impl`，避免新增或修改方法时只改了一份。
///
/// # 请求方法的 identity 合并位置
///
/// 选项结构是嵌套的，`identity` 字段的位置随方法不同（写错层级会编译失败）：
/// - `stream`/`complete`/`cancel_deferred`/`generate_images`/`classify`：
///   `options.base.identity`；
/// - `stream_simple`/`complete_simple`/`stream_deferred`/`fetch_deferred`：
///   `options.base.base.identity`。
///
/// # 失败模式
///
/// `Models` 的方法都是必填的（trait 里没有默认实现），因此新增方法后若不同步修改本宏，
/// 会直接编译失败——这比静默漂移安全。更隐蔽的风险是：方法携带 identity 却忘记合并，
/// 包装器的默认身份会被静默丢弃。新增请求方法时按上面的清单核对字段路径。
macro_rules! delegate_models {
    ($ty:ty) => {
        #[async_trait]
        impl Models for $ty {
            fn get_providers(&self) -> Vec<Arc<dyn crate::models::Provider>> {
                self.inner.get_providers()
            }
            fn get_provider(&self, id: &str) -> Option<Arc<dyn crate::models::Provider>> {
                self.inner.get_provider(id)
            }
            fn get_models(&self, provider: Option<&str>) -> Vec<Model> {
                self.inner.get_models(provider)
            }
            fn get_model(&self, provider: &str, id: &str) -> Option<Model> {
                self.inner.get_model(provider, id)
            }
            fn get_models_of_type(
                &self,
                model_type: ModelType,
                provider: Option<&str>,
            ) -> Vec<Model> {
                self.inner.get_models_of_type(model_type, provider)
            }
            fn get_model_of_type(
                &self,
                model_type: ModelType,
                provider: &str,
                id: &str,
            ) -> Option<Model> {
                self.inner.get_model_of_type(model_type, provider, id)
            }
            fn get_all_models(&self, provider: Option<&str>) -> Vec<AnyModel> {
                self.inner.get_all_models(provider)
            }
            async fn refresh(
                &self,
                options: ModelsRefreshOptions,
            ) -> Result<ModelsRefreshResult, AiError> {
                self.inner.refresh(options).await
            }
            async fn check_auth(
                &self,
                provider_id: &str,
                options: AuthOperationOptions,
            ) -> Result<Option<AuthCheck>, AuthError> {
                self.inner.check_auth(provider_id, options).await
            }
            async fn get_available(
                &self,
                provider_id: Option<&str>,
                options: AuthOperationOptions,
            ) -> Result<Vec<Model>, AuthError> {
                self.inner.get_available(provider_id, options).await
            }
            async fn get_available_of_type(
                &self,
                model_type: ModelType,
                provider_id: Option<&str>,
                options: AuthOperationOptions,
            ) -> Result<Vec<Model>, AuthError> {
                self.inner
                    .get_available_of_type(model_type, provider_id, options)
                    .await
            }
            async fn get_all_available(
                &self,
                provider_id: Option<&str>,
                options: AuthOperationOptions,
            ) -> Result<Vec<AnyModel>, AuthError> {
                self.inner.get_all_available(provider_id, options).await
            }
            async fn get_auth(
                &self,
                target: GetAuthTarget<'_>,
                overrides: AuthResolutionOverrides,
            ) -> Result<Option<AuthResult>, AuthError> {
                // TS: `models.getAuth.bind(models)` —— 不注入 identity。
                self.inner.get_auth(target, overrides).await
            }
            async fn login(
                &self,
                provider_id: &str,
                auth_type: AuthType,
                interaction: &dyn AuthInteraction,
                options: LoginOptions,
            ) -> Result<Credential, AuthError> {
                self.inner
                    .login(provider_id, auth_type, interaction, options)
                    .await
            }
            async fn logout(
                &self,
                provider_id: &str,
                options: AuthOperationOptions,
            ) -> Result<(), AuthError> {
                self.inner.logout(provider_id, options).await
            }
            fn stream(
                &self,
                model: &Model,
                context: &Context,
                mut options: ModelsStreamOptions,
            ) -> AssistantMessageEventStream {
                options.base.identity =
                    merge_options_identity(&self.identity, options.base.identity.take());
                self.inner.stream(model, context, options)
            }
            async fn complete(
                &self,
                model: &Model,
                context: &Context,
                mut options: ModelsStreamOptions,
            ) -> AssistantMessage {
                options.base.identity =
                    merge_options_identity(&self.identity, options.base.identity.take());
                self.inner.complete(model, context, options).await
            }
            fn stream_simple(
                &self,
                model: &Model,
                context: &Context,
                mut options: ModelsSimpleStreamOptions,
            ) -> AssistantMessageEventStream {
                options.base.base.identity =
                    merge_options_identity(&self.identity, options.base.base.identity.take());
                self.inner.stream_simple(model, context, options)
            }
            async fn complete_simple(
                &self,
                model: &Model,
                context: &Context,
                mut options: ModelsSimpleStreamOptions,
            ) -> AssistantMessage {
                options.base.base.identity =
                    merge_options_identity(&self.identity, options.base.base.identity.take());
                self.inner.complete_simple(model, context, options).await
            }
            fn stream_deferred(
                &self,
                model: &Model,
                handle: &DeferredHandle,
                mut options: ModelsDeferredFetchOptions,
            ) -> AssistantMessageEventStream {
                options.base.base.identity =
                    merge_options_identity(&self.identity, options.base.base.identity.take());
                self.inner.stream_deferred(model, handle, options)
            }
            async fn fetch_deferred(
                &self,
                model: &Model,
                handle: &DeferredHandle,
                mut options: ModelsDeferredFetchOptions,
            ) -> AssistantMessage {
                options.base.base.identity =
                    merge_options_identity(&self.identity, options.base.base.identity.take());
                self.inner.fetch_deferred(model, handle, options).await
            }
            async fn cancel_deferred(
                &self,
                model: &Model,
                handle: &DeferredHandle,
                mut options: ModelsDeferredCancelOptions,
            ) -> Result<(), AiError> {
                options.base.identity =
                    merge_options_identity(&self.identity, options.base.identity.take());
                self.inner.cancel_deferred(model, handle, options).await
            }
            async fn generate_images(
                &self,
                model: &Model,
                context: &ImagesContext,
                mut options: ModelsImagesOptions,
            ) -> AssistantImages {
                options.base.identity =
                    merge_options_identity(&self.identity, options.base.identity.take());
                self.inner.generate_images(model, context, options).await
            }
            async fn classify(
                &self,
                model: &Model,
                context: &ClassifierContext,
                mut options: ModelsClassifierOptions,
            ) -> ClassifierResult {
                options.base.identity =
                    merge_options_identity(&self.identity, options.base.identity.take());
                self.inner.classify(model, context, options).await
            }
        }
    };
}

delegate_models!(IdentityModels);
delegate_models!(IdentityMutableModels);

// 写操作原样转发：包装不改变可变性，也不缓存 provider 列表，与 TS 的
// `MutableModels` 重载保持一致。
impl MutableModels for IdentityMutableModels {
    fn set_provider(&self, provider: Arc<dyn crate::models::Provider>) {
        self.inner.set_provider(provider);
    }
    fn delete_provider(&self, id: &str) {
        self.inner.delete_provider(id);
    }
    fn clear_providers(&self) {
        self.inner.clear_providers();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_user_agents() {
        let runtime = runtime_description();
        let expected = match runtime.as_deref() {
            Some(runtime) => format!(
                "kiana/{KIANA_AI_VERSION} ({}; {runtime}; {})",
                node_platform(),
                node_arch()
            ),
            None => format!("kiana/{KIANA_AI_VERSION} (browser)"),
        };
        assert_eq!(format_user_agent(None), expected);
        assert_eq!(
            format_user_agent(Some(&ClientIdentity {
                version: Some("9.9.9".into()),
                ..Default::default()
            })),
            expected.replace(KIANA_AI_VERSION, "9.9.9")
        );
        assert_eq!(
            format_user_agent(Some(&ClientIdentity {
                user_agent: Some(UserAgentMode::Short("short".into())),
                version: Some("9.9.9".into()),
                ..Default::default()
            })),
            "kiana"
        );
        assert_eq!(
            format_user_agent(Some(&ClientIdentity {
                user_agent: Some(UserAgentMode::Custom {
                    custom: "acme/1.0".into()
                }),
                ..Default::default()
            })),
            "acme/1.0"
        );
    }

    #[test]
    fn browser_user_agent_when_no_runtime_is_available() {
        assert_eq!(
            format_user_agent_with_runtime(None, None),
            format!("kiana/{KIANA_AI_VERSION} (browser)")
        );
        assert_eq!(
            format_user_agent_with_runtime(None, Some("node/v22.19.0")),
            format!(
                "kiana/{KIANA_AI_VERSION} ({}; node/v22.19.0; {})",
                node_platform(),
                node_arch()
            )
        );
    }

    #[test]
    fn capitalizes_only_the_first_code_unit() {
        assert_eq!(capitalize_first(""), "");
        assert_eq!(capitalize_first("acme"), "Acme");
        assert_eq!(capitalize_first("Kiana"), "Kiana");
        // TS: "😀".charAt(0) 是高代理项，toUpperCase 不变，与 slice(1) 拼回原字符。
        assert_eq!(capitalize_first("😀acme"), "😀acme");
        assert_eq!(capitalize_first("élan"), "Élan");
    }

    #[test]
    fn merges_identities_field_by_field() {
        assert!(merge_client_identity([None, None]).is_none());
        let merged = merge_client_identity([
            Some(ClientIdentity {
                attribution: Some(Some(KIANA_ATTRIBUTION.clone())),
                user_agent: Some(UserAgentMode::Short("short".into())),
                version: None,
            }),
            Some(ClientIdentity {
                attribution: None,
                user_agent: None,
                version: Some("1.0.0".into()),
            }),
        ])
        .unwrap();
        assert_eq!(
            merged.user_agent,
            Some(UserAgentMode::Short("short".into()))
        );
        assert_eq!(merged.attribution, Some(Some(KIANA_ATTRIBUTION.clone())));
        assert_eq!(merged.version.as_deref(), Some("1.0.0"));
        let cleared = merge_client_identity([
            Some(ClientIdentity {
                attribution: Some(Some(KIANA_ATTRIBUTION.clone())),
                ..Default::default()
            }),
            Some(ClientIdentity {
                attribution: Some(None),
                ..Default::default()
            }),
        ])
        .unwrap();
        assert_eq!(cleared.attribution, Some(None));
    }

    #[test]
    fn attribute_headers_follow_provider_priority() {
        let openrouter = ClientIdentityTarget::new("openrouter", "https://openrouter.ai/api/v1");
        let identity = ClientIdentity {
            attribution: Some(Some(KIANA_ATTRIBUTION.clone())),
            ..Default::default()
        };
        let headers = get_attribution_headers(&openrouter, Some(&identity));
        assert_eq!(
            headers.get("HTTP-Referer").cloned().flatten().as_deref(),
            Some("https://kiana.dev")
        );
        assert_eq!(
            headers
                .get("X-OpenRouter-Categories")
                .cloned()
                .flatten()
                .as_deref(),
            Some("cli-agent")
        );
        let nvidia = ClientIdentityTarget::new("nvidia", "https://example.test/v1");
        assert_eq!(
            get_attribution_headers(&nvidia, Some(&identity))
                .get("X-BILLING-INVOKE-ORIGIN")
                .cloned()
                .flatten()
                .as_deref(),
            Some("Kiana")
        );
        let opencode = ClientIdentityTarget::new("custom", "https://opencode.ai/zen/v1");
        assert_eq!(
            get_attribution_headers(&opencode, Some(&identity))
                .get("x-opencode-client")
                .cloned()
                .flatten()
                .as_deref(),
            Some("kiana")
        );
        assert!(get_attribution_headers(&openrouter, None).is_empty());
    }

    #[test]
    fn identity_headers_put_user_agent_first() {
        let target = ClientIdentityTarget::new("openrouter", "https://openrouter.ai/api/v1");
        let identity = ClientIdentity {
            attribution: Some(Some(KIANA_ATTRIBUTION.clone())),
            user_agent: Some(UserAgentMode::Short("short".into())),
            ..Default::default()
        };
        let headers = get_identity_headers(&target, Some(&identity));
        let keys: Vec<&String> = headers.keys().collect();
        assert_eq!(keys[0], "User-Agent");
        assert_eq!(
            headers.get("User-Agent").cloned().flatten().as_deref(),
            Some("kiana")
        );
    }

    #[test]
    fn session_headers_only_for_opencode_targets() {
        let opencode = ClientIdentityTarget::new("opencode", "https://opencode.ai/zen/v1");
        assert_eq!(
            get_open_code_session_headers(&opencode, Some("session-1"))
                .unwrap()
                .get("x-opencode-session")
                .cloned()
                .flatten()
                .as_deref(),
            Some("session-1")
        );
        assert!(get_open_code_session_headers(&opencode, None).is_none());
        let other = ClientIdentityTarget::new("other", "https://example.test");
        assert!(get_open_code_session_headers(&other, Some("s")).is_none());
    }
}
