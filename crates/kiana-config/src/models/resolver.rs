//! 引用解析 / scope / CLI / initial / restore（`selectReference` 6 步 + `selectFuzzy`）。
//!
//! TS: `packages/coding-agent/src/core/model-resolver.ts`（689 行）。规范：
//! `spec/ca-config-models.md` §2.21（含 §2.21.1-§2.21.14）、§3.1、§3.2、§8.3 #4/#10。
//!
//! ## 语义要点
//!
//! - `provider/rest` 只是提示：全文也可能是别的 provider 的模型 id（OpenRouter 的
//!   `qwen/qwen3-coder:exacto` 含冒号，URL 派生的 provider id 本身可含 `/`）。
//! - 候选先收集再裁决，因此**目录顺序永不决定 provider**；歧义会报告而不是落到更弱的匹配。
//! - `:thinking` 后缀只在全文不匹配任何模型时才考虑（`none`/`strict`/`lenient` 三模式）。
//! - 模糊匹配：别名（`-latest` 或非 `-\d{8}$`）优先于日期版本，再按 `id` 的
//!   `localeCompare` 降序取最新（en-US 基线，见 `kiana_ai::model_catalog::locale_compare`）。
//! - scope 的 glob 按 `minimatch@10.2.6` 的语义复刻（`{nocase:true}`）：brace 展开、
//!   `**` globstar、`*`/`?`/`[...]`/POSIX 类/extglob、dot 规则与前导 `!` 取反；实现见
//!   `minimatch` 与 `minimatch_10` 模块。黄金对照见
//!   `tests/fixtures/resolver/minimatch_cases.json`（随机语料）与
//!   `tests/fixtures/resolver/minimatch_edge_cases.json`（快捷路径/转义 `\|` 交替、
//!   dot 遍历守卫、ECMAScript `Canonicalize` 大小写折叠、`String(number)` 指数形式），
//!   两者均由 TS 参考实现生成。
//!
//! ## Rust 侧的类型缝
//!
//! TS 的 `resolveCliModel` / `findInitialModel` / `restoreModelFromSession` /
//! `resolveModelScope*` 接收 `ModelRuntime`，测试则传 duck-typed 的假对象。Rust 侧用
//! [`ModelResolverRuntime`] trait 保留同一能力；[`ModelRuntime`] 有直接实现，生产代码
//! 传 `&ModelRuntime` 即可。
//!
//! `isValidThinkingLevel` 在 TS 里来自 `cli/args.ts`（`kiana-coding-agent`，L13 依赖方向
//! 不允许）。这里用 [`is_valid_thinking_level`] 本地实现，取值集合与
//! `kiana_ai::models::EXTENDED_THINKING_LEVELS` + `"off"` 保持一致（有测试锁定）。
//!
//! ## 模块结构
//!
//! 按文件内顺序分五部分：
//!
//! 1. 常量与公开类型（默认模型表、请求/结果/诊断类型、运行时 trait）；
//! 2. 引用解析核心：`resolve_model_reference` → `resolve_text` →
//!    `select_reference` / `select_fuzzy`，以及 `:<thinking>` 与 `provider/` 的拆分
//!    辅助函数；
//! 3. 作用域展开：`resolve_model_scope*`；glob 引擎是内嵌模块 `minimatch_10`；
//! 4. CLI / 初始模型 / 会话恢复：`resolve_cli_model`、`find_initial_model`、
//!    `restore_model_from_session`；
//! 5. chalk 颜色判定与单元测试。
//!
//! ## 一次解析的执行流程
//!
//! ```text
//! resolve_model_reference
//!   ├─ 按 request.provider 过滤候选（小写精确匹配；空串视为未提供）
//!   ├─ resolve_text
//!   │    ├─ select_reference：先收集「裸 id 精确匹配 + provider 前缀匹配」的候选，
//!   │    │   再按 provider 是否已配置认证裁决（候选同属一个 provider 时直接 resolved）
//!   │    ├─ fuzzy 时 select_fuzzy：id/name 子串匹配，别名优先于日期版本
//!   │    └─ 仍失败则递归剥离最后一个 `:<suffix>`：none 不剥、strict 只剥合法
//!   │         thinking 等级、lenient 剥掉非法后缀并附带 warning
//!   └─ 仍未命中：若能为显式 provider 找到 anchor，返回带 target 的 not-found，
//!        供调用方构造自定义模型；否则返回不带 target 的 not-found
//! ```
//!
//! 每一层都是**先收集候选、后裁决**，所以目录顺序不决定 provider；测试把同一份目录
//! 正序与倒序各跑一遍来锁定该性质（`crates/kiana-config/tests/model_resolver.rs`）。
//!
//! ## 注意事项
//!
//! - `resolve_model_reference` / `find_exact_model_reference_match` /
//!   `resolve_cli_model` 是同步函数；`find_initial_model` 与
//!   `restore_model_from_session` 声明为 async，但只调用 trait 的同步方法；
//!   `resolve_model_scope` / `resolve_model_scope_with_diagnostics` 通过
//!   `ModelResolverRuntime::get_available` 异步取模型；前者的同步版本
//!   `resolve_model_scope_from_models` 直接接收模型列表。
//! - `resolve_model_scope` 的诊断写 stderr；`restore_model_from_session` 的提示写
//!   stdout（dim）与 stderr（yellow）。是否真的输出颜色由 `chalk_color_enabled` 的
//!   TTY/环境变量判定决定。
//! - 文本处理按 JS 字符串语义（UTF-16 下标、`js_trim`），与 TS 对齐；见「JS 字符串
//!   语义」小节的私有辅助函数。

use std::collections::HashSet;
use std::io::IsTerminal;
use std::sync::LazyLock;

use async_trait::async_trait;
use indexmap::IndexMap;
use kiana_ai::auth::types::{AuthError, AuthOperationOptions};
use kiana_ai::model_catalog::locale_compare;
use kiana_ai::models::models_are_equal;
use kiana_ai::types::{AnyModel, Model, ModelThinkingLevel};
use kiana_json::js_trim;

use crate::defaults::DEFAULT_THINKING_LEVEL;
use crate::models::runtime::ModelRuntime;

// ---------------------------------------------------------------------------
// 常量与校验
// ---------------------------------------------------------------------------

/// `defaultModelPerProvider`：有内置 chat 模型的 provider 的默认模型 id。
///
/// **逐字**（含 `qwen3.7-max` / `qwen3.8-max` / `mimo-v2.6-pro` 三处脱敏占位符——它们是
/// 当前检出里的字面值，测试按字面断言，不得清洗/替换）。键顺序即 TS `Object.keys` 顺序，
/// [`find_initial_model`] / [`restore_model_from_session`] 的第 3/4 步依赖该顺序。
///
/// TS: `core/model-resolver.ts#defaultModelPerProvider`
pub static DEFAULT_MODEL_PER_PROVIDER: LazyLock<IndexMap<&'static str, &'static str>> =
    LazyLock::new(|| {
        let mut map = IndexMap::new();
        for (provider, model) in [
            ("amazon-bedrock", "us.anthropic.claude-opus-4-6-v1"),
            ("ant-ling", "Ring-2.6-1T"),
            ("anthropic", "claude-opus-4-8"),
            ("openai", "gpt-5.5"),
            ("azure", "gpt-5.4"),
            ("openai-codex", "gpt-6.1-sol"),
            ("radius", "balanced"),
            ("nvidia", "nvidia/nemotron-3.5-super-vl-120b-a12b"),
            ("deepseek", "deepseek-v4-pro"),
            ("google", "gemini-3.1-pro-preview"),
            ("google-vertex", "gemini-3.1-pro-preview"),
            ("github-copilot", "gpt-6.1-sol"),
            ("openrouter", "moonshotai/kimi-k2.6"),
            ("vercel-ai-gateway", "zai/glm-5.1"),
            ("xai", "grok-4.7"),
            ("groq", "openai/gpt-oss-120b"),
            ("cerebras", "gpt-oss-120b"),
            ("zai", "glm-5.3"),
            ("zai-coding-cn", "glm-5.3"),
            ("mistral", "devstral-medium-latest"),
            ("minimax", "MiniMax-M2.7"),
            ("minimax-cn", "MiniMax-M2.7"),
            ("moonshotai", "kimi-k2.6"),
            ("moonshotai-cn", "kimi-k2.6"),
            ("huggingface", "moonshotai/Kimi-K2.6"),
            ("fireworks", "accounts/fireworks/models/kimi-k3"),
            ("together", "moonshotai/Kimi-K3"),
            ("baseten", "zai-org/GLM-5.2"),
            ("opencode", "kimi-k2.6"),
            ("opencode-go", "kimi-k3"),
            ("kimi-coding", "kimi-for-coding"),
            ("meta", "muse-spark-1.3"),
            ("cloudflare-workers-ai", "@cf/moonshotai/kimi-k2.6"),
            (
                "cloudflare-ai-gateway",
                "workers-ai/@cf/moonshotai/kimi-k2.6",
            ),
            ("qwen-token-plan", "qwen3.7-max"),
            ("qwen-token-plan-cn", "qwen3.7-max"),
            ("qwen-token-plan-individual", "qwen3.8-max"),
            ("xiaomi", "mimo-v2.6-pro"),
            ("xiaomi-token-plan-cn", "mimo-v2.6-pro"),
            ("xiaomi-token-plan-ams", "mimo-v2.6-pro"),
            ("xiaomi-token-plan-sgp", "mimo-v2.6-pro"),
        ] {
            map.insert(provider, model);
        }
        map
    });

/// `Object.entries(defaultModelPerProvider)`：按声明顺序的 `(provider, model)` 列表。
pub fn default_models_ordered() -> Vec<(String, String)> {
    DEFAULT_MODEL_PER_PROVIDER
        .iter()
        .map(|(provider, model)| ((*provider).to_string(), (*model).to_string()))
        .collect()
}

/// `isValidThinkingLevel(level)`：`off` / `minimal` / `low` / `medium` / `high` / `xhigh`
/// / `max`。
///
/// TS 从 `cli/args.ts` 导入（`kiana-coding-agent`），Rust 侧本地实现，取值集合与
/// `kiana_ai::models::EXTENDED_THINKING_LEVELS` 保持一致（由单元测试锁定）。
pub fn is_valid_thinking_level(level: &str) -> bool {
    parse_thinking_level(level).is_some()
}

/// `isValidThinkingLevel` 的解析版本。
pub fn parse_thinking_level(level: &str) -> Option<ModelThinkingLevel> {
    match level {
        "off" => Some(ModelThinkingLevel::Off),
        "minimal" => Some(ModelThinkingLevel::Minimal),
        "low" => Some(ModelThinkingLevel::Low),
        "medium" => Some(ModelThinkingLevel::Medium),
        "high" => Some(ModelThinkingLevel::High),
        "xhigh" => Some(ModelThinkingLevel::Xhigh),
        "max" => Some(ModelThinkingLevel::Max),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// JS 字符串语义（UTF-16 下标）
// ---------------------------------------------------------------------------

/// JS `String.prototype.length`。
fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// UTF-16 下标 → UTF-8 字节下标；切点落在代理对中间时返回 `None`。
fn utf16_byte_index(text: &str, target: usize) -> Option<usize> {
    let mut units = 0usize;
    for (byte_index, ch) in text.char_indices() {
        if units == target {
            return Some(byte_index);
        }
        units += ch.len_utf16();
        if units > target {
            return None;
        }
    }
    (units == target).then_some(text.len())
}

/// `String.prototype.indexOf(needle, from)`（`from` 以 UTF-16 code unit 计），返回
/// UTF-16 下标。
///
/// 起点落在代理对中间时，JS 从低代理项开始比较；对 `'/'` 这类单码元 needle，从下一个
/// 字符边界开始查找结果相同。
fn js_index_of_from(text: &str, needle: char, from_utf16: usize) -> Option<usize> {
    let byte_start = match utf16_byte_index(text, from_utf16) {
        Some(index) => index,
        None => {
            let mut units = 0usize;
            let mut index = text.len();
            for (byte_index, ch) in text.char_indices() {
                units += ch.len_utf16();
                if units > from_utf16 {
                    index = byte_index;
                    break;
                }
            }
            index
        }
    };
    let offset = text[byte_start..].find(needle)?;
    Some(utf16_len(&text[..byte_start + offset]))
}

/// `String.prototype.substring(start, end)`（UTF-16 下标；`end` 超界取末尾）。
///
/// 切点落在代理对中间时返回 `None`（TS 会产生孤立代理项，Rust 字符串无法表示；调用方
/// 只在本就不可能相等/为空的比较里使用）。
fn js_substring(text: &str, start_utf16: usize, end_utf16: usize) -> Option<&str> {
    let start = utf16_byte_index(text, start_utf16)?;
    let end = if end_utf16 >= utf16_len(text) {
        text.len()
    } else {
        utf16_byte_index(text, end_utf16)?
    };
    text.get(start..end)
}

// ---------------------------------------------------------------------------
// 类型
// ---------------------------------------------------------------------------

/// `ScopedModel`：一个作用域内的模型与可选 thinking 等级。
///
/// TS: `core/model-resolver.ts#ScopedModel`
#[derive(Debug, Clone, PartialEq)]
pub struct ScopedModel {
    /// 模型。
    pub model: Model,
    /// 模式里显式写出的 thinking 等级（`"model:high"`），否则 `None`。
    pub thinking_level: Option<ModelThinkingLevel>,
}

/// `ModelReferenceRequest`：`Bare model id or provider/id, optionally followed by :thinking`。
///
/// TS: `core/model-resolver.ts#ModelReferenceRequest`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelReferenceRequest {
    /// `provider/id` 或裸 id，可带 `:<thinking>`。
    pub text: String,
    /// 只考虑该 provider 的模型。
    pub provider: Option<String>,
    /// 尾部 `:<suffix>` 的处理模式。
    pub thinking_suffix: ThinkingSuffixMode,
    /// 没有精确匹配时按 id / name 子串匹配。
    pub fuzzy: bool,
}

impl Default for ModelReferenceRequest {
    fn default() -> Self {
        Self {
            text: String::new(),
            provider: None,
            thinking_suffix: ThinkingSuffixMode::Strict,
            fuzzy: false,
        }
    }
}

/// `request.thinkingSuffix` 的三模式。
///
/// TS: `core/model-resolver.ts#ModelReferenceRequest.thinkingSuffix`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThinkingSuffixMode {
    /// 后缀留在 id 里。
    None,
    /// 只解析合法的 thinking 等级；其它后缀留在 id 里。
    #[default]
    Strict,
    /// 同 `strict`，但非法后缀会被丢弃并给出警告。
    Lenient,
}

/// `ModelReferenceTarget`：`not-found` 时用于构造自定义模型 id。
///
/// TS: `core/model-resolver.ts#ModelReferenceTarget`
#[derive(Debug, Clone, PartialEq)]
pub struct ModelReferenceTarget {
    /// provider id。
    pub provider: String,
    /// 该 provider 的模型（anchor 在首位，**非空**）。
    pub provider_models: Vec<Model>,
    /// 原样输入的 provider 前缀之后的文本（含可能的 `:<suffix>`）。
    pub model_text: String,
    /// 去掉合法 thinking 后缀后的 id（`thinkingSuffix` 为 `none` 时等于 `model_text`）。
    pub model_id: String,
    /// 从 `model_text` 解析出的 thinking 等级。
    pub thinking_level: Option<ModelThinkingLevel>,
}

/// `ModelReferenceResult`。
///
/// TS: `core/model-resolver.ts#ModelReferenceResult`
// `ModelReferenceResult` 与 TS 的判别联合一一对应；`Model` 本身就是大对象（约 825 字节），
// 装箱只会为每次解析增加一次堆分配和调用方解引用。枚举在每次解析时创建一次，栈上大小可接受，
// 因此保留与 TS 相同的字段形状（ARCHITECTURE §4.10 允许带理由的局部例外）。
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum ModelReferenceResult {
    /// `status: "resolved"`。
    Resolved {
        /// 选中的模型。
        model: Model,
        /// 模式里的 thinking 等级。
        thinking_level: Option<ModelThinkingLevel>,
        /// `lenient` 模式丢弃非法后缀时的警告。
        warning: Option<String>,
    },
    /// `status: "ambiguous"`。
    Ambiguous {
        /// 候选（TS 的 `candidates`）。
        candidates: Vec<Model>,
        /// 可用的候选数（`usable(provider)` 为真的 other 数）。
        usable_count: usize,
        /// 模式里的 thinking 等级。
        thinking_level: Option<ModelThinkingLevel>,
        /// `lenient` 模式丢弃非法后缀时的警告。
        warning: Option<String>,
    },
    /// `status: "not-found"`。
    NotFound {
        /// 显式 provider（参数或 `provider/` 前缀）时给出。
        target: Option<ModelReferenceTarget>,
    },
}

impl ModelReferenceResult {
    /// `status` 字符串。
    pub fn status(&self) -> &'static str {
        match self {
            Self::Resolved { .. } => "resolved",
            Self::Ambiguous { .. } => "ambiguous",
            Self::NotFound { .. } => "not-found",
        }
    }

    /// `result.status === "not-found"`。
    pub fn is_not_found(&self) -> bool {
        matches!(self, Self::NotFound { .. })
    }

    /// `result.model`（仅 resolved）。
    pub fn resolved_model(&self) -> Option<&Model> {
        match self {
            Self::Resolved { model, .. } => Some(model),
            Self::Ambiguous { .. } | Self::NotFound { .. } => None,
        }
    }

    /// `result.thinkingLevel`。
    pub fn thinking_level(&self) -> Option<ModelThinkingLevel> {
        match self {
            Self::Resolved { thinking_level, .. } | Self::Ambiguous { thinking_level, .. } => {
                *thinking_level
            }
            Self::NotFound { .. } => None,
        }
    }

    /// `result.warning`。
    pub fn warning(&self) -> Option<&str> {
        match self {
            Self::Resolved { warning, .. } | Self::Ambiguous { warning, .. } => warning.as_deref(),
            Self::NotFound { .. } => None,
        }
    }
}

/// `ModelScopeDiagnostic.type` 的唯一取值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelScopeDiagnosticType {
    /// `"warning"`。
    Warning,
}

impl ModelScopeDiagnosticType {
    /// TS 的字面量。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Warning => "warning",
        }
    }
}

/// `ModelScopeDiagnostic.code`。
///
/// TS: `core/model-resolver.ts#ModelScopeDiagnostic`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelScopeDiagnosticCode {
    /// `no-match`。
    NoMatch,
    /// `invalid-thinking-level`。
    InvalidThinkingLevel,
    /// `ambiguous`。
    Ambiguous,
}

impl ModelScopeDiagnosticCode {
    /// TS 的字面量。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoMatch => "no-match",
            Self::InvalidThinkingLevel => "invalid-thinking-level",
            Self::Ambiguous => "ambiguous",
        }
    }
}

/// `ModelScopeDiagnostic`。
///
/// TS: `core/model-resolver.ts#ModelScopeDiagnostic`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelScopeDiagnostic {
    /// 恒为 `"warning"`。
    pub diagnostic_type: ModelScopeDiagnosticType,
    /// 诊断码。
    pub code: ModelScopeDiagnosticCode,
    /// 用户可见文案（逐字）。
    pub message: String,
    /// 原始模式。
    pub pattern: String,
}

/// `ResolveModelScopeResult`。
///
/// TS: `core/model-resolver.ts#ResolveModelScopeResult`
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ResolveModelScopeResult {
    /// 展开后的模型（首次出现者胜，去重键为 type + id + provider）。
    pub scoped_models: Vec<ScopedModel>,
    /// 结构化诊断（不会直接打印）。
    pub diagnostics: Vec<ModelScopeDiagnostic>,
}

/// `resolveCliModel(options)` 的输入。
#[derive(Clone)]
pub struct ResolveCliModelOptions<'a> {
    /// `--provider`。
    pub cli_provider: Option<String>,
    /// `--model`。
    pub cli_model: Option<String>,
    /// `--thinking`。
    pub cli_thinking: Option<ModelThinkingLevel>,
    /// 是否提供 `--api-key`（存储凭证不得决定 provider）。
    pub inline_api_key: bool,
    /// 模型运行时视图。
    pub model_runtime: &'a dyn ModelResolverRuntime,
}

/// `ResolveCliModelResult`。
///
/// TS: `core/model-resolver.ts#ResolveCliModelResult`
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ResolveCliModelResult {
    /// 模型（`error` 为 `Some` 时是 `None`）。
    pub model: Option<Model>,
    /// 从 `:<thinking>` 解析出的等级（调用方自行应用）。
    pub thinking_level: Option<ModelThinkingLevel>,
    /// 自定义模型回退警告。
    pub warning: Option<String>,
    /// 供 CLI 展示的错误。
    pub error: Option<String>,
}

/// `findInitialModel(options)` 的输入。
#[derive(Clone)]
pub struct FindInitialModelOptions<'a> {
    /// 作用域模型。
    pub scoped_models: &'a [ScopedModel],
    /// 是否在继续/恢复会话（跳过第 1 步）。
    pub is_continuing: bool,
    /// 设置的 `defaultProvider`。
    pub default_provider: Option<&'a str>,
    /// 设置的 `defaultModel`。
    pub default_model_id: Option<&'a str>,
    /// 设置的 `defaultThinkingLevel`。
    pub default_thinking_level: Option<ModelThinkingLevel>,
    /// 设置的每模型 thinking 等级（键 `provider/id`）。
    pub model_thinking_levels: Option<&'a IndexMap<String, ModelThinkingLevel>>,
    /// 模型运行时视图。
    pub model_runtime: &'a dyn ModelResolverRuntime,
}

/// `InitialModelResult`。
///
/// TS: `core/model-resolver.ts#InitialModelResult`
#[derive(Debug, Clone, PartialEq)]
pub struct InitialModelResult {
    /// 初始模型。
    pub model: Option<Model>,
    /// thinking 等级（TS 里恒有值）。
    pub thinking_level: ModelThinkingLevel,
    /// 回退说明（`findInitialModel` 恒为 `None`）。
    pub fallback_message: Option<String>,
}

/// `restoreModelFromSession` 的结果。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RestoreModelResult {
    /// 恢复或回退后的模型。
    pub model: Option<Model>,
    /// 未能恢复时的回退说明。
    pub fallback_message: Option<String>,
}

// ---------------------------------------------------------------------------
// 模型运行时视图
// ---------------------------------------------------------------------------

/// resolver 需要的 `ModelRuntime` 子集（TS 测试用 duck-typed 假对象）。
///
/// Rust 侧用 trait 保留同一缝；[`ModelRuntime`] 有直接实现。
///
/// 契约：
/// - `get_models` 返回**全部**模型（不按认证过滤）；
/// - `get_available` 按认证过滤，失败时返回 `AuthError`，resolver 不恢复、原样上抛；
/// - `get_available_snapshot` 是同步快照。以 [`ModelRuntime`] 的实现为例，它只读取
///   缓存、不触发刷新，而 `get_available(None, ..)` 会先触发一次可用性刷新再读快照。
/// - `has_configured_auth` 只回答「是否被当前快照标记为已配置
///   （`configured_providers`）」，不等同于存在凭证：注册路径会为无凭证的 provider
///   提前写入该集合（纯虚拟 provider 由 `register_virtual_model` 强行加入，过渡态由
///   `mark_provisionally_configured` 写入）；不触发探测/网络请求。
// `async-trait` 会给每个 async 方法生成 `#[must_use]`，与返回 future 自带的 must_use
// 叠加触发 `double_must_use`（同 kiana-mcp 的既有写法）。
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait ModelResolverRuntime: Send + Sync {
    /// `getModels()`（无 provider 过滤，使用全部模型）。
    fn get_models(&self) -> Vec<Model>;
    /// `getModel(provider, id)`。
    fn get_model(&self, provider: &str, model_id: &str) -> Option<Model>;
    /// `hasConfiguredAuth(provider)`。
    fn has_configured_auth(&self, provider: &str) -> bool;
    /// `getAvailableSnapshot()`。
    fn get_available_snapshot(&self) -> Vec<Model>;
    /// `getAvailable(undefined, options)`。
    async fn get_available(
        &self,
        provider: Option<&str>,
        options: AuthOperationOptions,
    ) -> Result<Vec<Model>, AuthError>;
}

#[async_trait]
impl ModelResolverRuntime for ModelRuntime {
    fn get_models(&self) -> Vec<Model> {
        ModelRuntime::get_models(self, None)
    }

    fn get_model(&self, provider: &str, model_id: &str) -> Option<Model> {
        ModelRuntime::get_model(self, provider, model_id)
    }

    fn has_configured_auth(&self, provider: &str) -> bool {
        ModelRuntime::has_configured_auth(self, provider)
    }

    fn get_available_snapshot(&self) -> Vec<Model> {
        ModelRuntime::get_available_snapshot(self)
    }

    async fn get_available(
        &self,
        provider: Option<&str>,
        options: AuthOperationOptions,
    ) -> Result<Vec<Model>, AuthError> {
        ModelRuntime::get_available(self, provider, options).await
    }
}

// ---------------------------------------------------------------------------
// 核心解析
// ---------------------------------------------------------------------------

/// 一次 `provider/` 前缀拆分。
#[derive(Debug, Clone, PartialEq, Eq)]
struct ProviderSplit {
    provider: String,
    model_text: String,
}

/// 一次 `:<thinking>` 拆分。
#[derive(Debug, Clone, PartialEq, Eq)]
struct ThinkingSplit {
    base: String,
    thinking_level: ModelThinkingLevel,
}

/// `splitThinkingSuffix(text)`：`lastIndexOf(":")`，后缀必须是合法 thinking 等级。
///
/// 注意哨兵判断在取 substring **之后**（等价实现：先找最后一个 `:`，找不到即 `None`）。
fn split_thinking_suffix(text: &str) -> Option<ThinkingSplit> {
    let colon_index = text.rfind(':')?;
    let suffix = &text[colon_index + 1..];
    let thinking_level = parse_thinking_level(suffix)?;
    Some(ThinkingSplit {
        base: text[..colon_index].to_string(),
        thinking_level,
    })
}

/// `splitProviderPrefix(text, models)`：已知 provider id 的最长前缀胜出（同长度取首次出现）。
///
/// provider id 自身可含 `/`；要求 `provider/model` 中第一段去空白后等于 provider，且
/// `modelText` 非空。
fn split_provider_prefix(text: &str, models: &[Model]) -> Option<ProviderSplit> {
    let lower_text = text.to_lowercase();
    let mut best: Option<ProviderSplit> = None;
    for model in models {
        let provider = model.provider.as_str();
        let prefix = js_trim(provider).to_lowercase();
        if prefix.is_empty() {
            continue;
        }
        if let Some(current) = &best
            && utf16_len(&current.provider) >= utf16_len(provider)
        {
            continue;
        }
        let Some(slash_index) = js_index_of_from(&lower_text, '/', utf16_len(&prefix)) else {
            continue;
        };
        let Some(head) = js_substring(&lower_text, 0, slash_index) else {
            continue;
        };
        if js_trim(head) != prefix {
            continue;
        }
        let Some(model_text_raw) = js_substring(text, slash_index + 1, usize::MAX) else {
            continue;
        };
        let model_text = js_trim(model_text_raw);
        if !model_text.is_empty() {
            best = Some(ProviderSplit {
                provider: provider.to_string(),
                model_text: model_text.to_string(),
            });
        }
    }
    best
}

/// `resolveModelReference(request, models, usable)`。
///
/// 分两个阶段：
/// 1. 把 `request.text` 交给私有的 `resolve_text` 做完整解析；只要结果不是
///    `not-found` 就原样返回，不会构造 target。
/// 2. 解析失败且能确定 provider 时，返回带 [`ModelReferenceTarget`] 的 `not-found`。
///    anchor 的选择：`request.provider` 有约束时取过滤后候选的第一个；否则用
///    `splitProviderPrefix` 找到的 provider 在候选池中的第一个模型。调用方（例如
///    [`resolve_cli_model`]）据此把未知 id 当作自定义模型，而不是直接报错。
///
/// `models` 是候选全集；`usable(provider)` 表示该 provider 是否已配置认证。它在两条
/// 路径上的作用不同：`select_reference` 的裁决不按它过滤候选（ambiguous 候选即使全部
/// 不可用也会被报告）；`select_fuzzy` 在候选跨 provider 时先用它过滤匹配池，不可用的
/// 候选会从池与最终的 `candidates` 里消失。`request.provider` 为 `None` 或空串
/// 都视为未指定（JS falsy 语义），非空时按小写精确匹配过滤。
///
/// 返回的 `not-found` 有两种形状：`target: None` 表示没有可用 anchor——未指定
/// provider，或指定了但候选池中没有该 provider 的模型；
/// `target: Some(..)` 的 `provider_models` 保证非空（anchor 在首位）。
///
/// TS: `core/model-resolver.ts#resolveModelReference`
pub fn resolve_model_reference(
    request: &ModelReferenceRequest,
    models: &[Model],
    usable: &dyn Fn(&str) -> bool,
) -> ModelReferenceResult {
    let text = js_trim(&request.text);
    if text.is_empty() {
        return ModelReferenceResult::NotFound { target: None };
    }
    // TS: `request.provider?.toLowerCase()`（不 trim；空串是 falsy，等于没有约束）。
    let constraint = request
        .provider
        .as_deref()
        .map(str::to_lowercase)
        .filter(|constraint| !constraint.is_empty());
    let pool: Vec<Model> = match &constraint {
        Some(constraint) => models
            .iter()
            .filter(|model| model.provider.to_lowercase() == *constraint)
            .cloned()
            .collect(),
        None => models.to_vec(),
    };
    let result = resolve_text(text, request, &pool, usable);
    if !result.is_not_found() {
        return result;
    }

    let split = split_provider_prefix(text, &pool);
    let anchor: Option<&Model> = match &constraint {
        Some(_) => pool.first(),
        None => split
            .as_ref()
            .and_then(|split| pool.iter().find(|model| model.provider == split.provider)),
    };
    let Some(anchor) = anchor else {
        return result;
    };
    let model_text = split
        .as_ref()
        .map_or_else(|| text.to_string(), |split| split.model_text.clone());
    let suffix = if request.thinking_suffix == ThinkingSuffixMode::None {
        None
    } else {
        split_thinking_suffix(&model_text)
    };
    let provider_models = std::iter::once(anchor.clone())
        .chain(
            pool.iter()
                .filter(|model| model.provider == anchor.provider && !std::ptr::eq(*model, anchor))
                .cloned(),
        )
        .collect();
    ModelReferenceResult::NotFound {
        target: Some(ModelReferenceTarget {
            provider: anchor.provider.clone(),
            provider_models,
            model_id: suffix
                .as_ref()
                .map_or_else(|| model_text.clone(), |suffix| suffix.base.clone()),
            thinking_level: suffix.as_ref().map(|suffix| suffix.thinking_level),
            model_text,
        }),
    }
}

/// `findExactModelReferenceMatch(modelReference, models)`。
///
/// TS: `core/model-resolver.ts#findExactModelReferenceMatch`
pub fn find_exact_model_reference_match(model_reference: &str, models: &[Model]) -> Option<Model> {
    let result = resolve_model_reference(
        &ModelReferenceRequest {
            text: model_reference.to_string(),
            provider: None,
            thinking_suffix: ThinkingSuffixMode::None,
            fuzzy: false,
        },
        models,
        &|_| true,
    );
    match result {
        ModelReferenceResult::Resolved { model, .. } => Some(model),
        ModelReferenceResult::Ambiguous { .. } | ModelReferenceResult::NotFound { .. } => None,
    }
}

/// `resolveText`：递归核心。
///
/// 判定顺序（前一步命中即返回）：
///
/// 1. `select_reference`：精确/命名匹配（fuzzy 为真时还包含 provider 内部的模糊匹配）；
/// 2. `request.fuzzy` 时 `select_fuzzy`：id / name 子串匹配；
/// 3. `thinking_suffix == None`：到此为止，后缀始终留在 id 里；
/// 4. 能解析出合法 `:thinking` 后缀：对去掉后缀的文本递归解析，成功则用该等级覆盖
///    内层结果（内层带 warning 时不覆盖，见 `override_thinking_level`）；
/// 5. `strict`：非法后缀不剥离，直接 `not-found`（后缀被当作 id 的一部分）；
/// 6. `lenient`：剥掉最后一个冒号后的任意后缀再递归，成功则附上
///    `Invalid thinking level "..."` 警告并清空 thinking 等级。
///
/// 每层递归都丢掉最后一个 `:` 及其后的文本，字符串严格变短，因此必然终止（深度受
/// 冒号个数限制）。内层的 `not-found` 会原样向上传播，不会退回到更弱的匹配方式。
fn resolve_text(
    text: &str,
    request: &ModelReferenceRequest,
    pool: &[Model],
    usable: &dyn Fn(&str) -> bool,
) -> ModelReferenceResult {
    if let Some(reference) = select_reference(text, pool, request.fuzzy, usable) {
        return reference;
    }

    if request.fuzzy
        && let Some(fuzzy) = select_fuzzy(&fuzzy_matches(text, pool.iter()), usable)
    {
        return fuzzy;
    }

    if request.thinking_suffix == ThinkingSuffixMode::None {
        return ModelReferenceResult::NotFound { target: None };
    }
    if let Some(thinking) = split_thinking_suffix(text) {
        let inner = resolve_text(&thinking.base, request, pool, usable);
        if inner.is_not_found() {
            return inner;
        }
        // 只在 inner 没有警告时使用这个 thinking 等级。
        return override_thinking_level(inner, thinking.thinking_level);
    }
    // 无效后缀保留为 id 的一部分并失败，而不是解析成另一个模型。
    if request.thinking_suffix == ThinkingSuffixMode::Strict {
        return ModelReferenceResult::NotFound { target: None };
    }
    let Some(colon_index) = text.rfind(':') else {
        return ModelReferenceResult::NotFound { target: None };
    };
    let suffix = &text[colon_index + 1..];
    let inner = resolve_text(&text[..colon_index], request, pool, usable);
    if inner.is_not_found() {
        return inner;
    }
    let warning = format!(
        "Invalid thinking level \"{suffix}\" in pattern \"{text}\". Using default instead."
    );
    with_warning(inner, warning)
}

/// `{...inner, thinkingLevel: inner.warning ? undefined : level}`。
///
/// 不变量：`warning` 为 `Some` 时 `thinking_level` 必须为 `None`（与 TS 的三元表达式
/// 一致）。为什么内层已有警告时不再应用本次等级，规范与源码均未给出因果解释（待核实）。
fn override_thinking_level(
    result: ModelReferenceResult,
    level: ModelThinkingLevel,
) -> ModelReferenceResult {
    match result {
        ModelReferenceResult::Resolved {
            model,
            thinking_level: _,
            warning,
        } => ModelReferenceResult::Resolved {
            model,
            thinking_level: if warning.is_some() { None } else { Some(level) },
            warning,
        },
        ModelReferenceResult::Ambiguous {
            candidates,
            usable_count,
            thinking_level: _,
            warning,
        } => ModelReferenceResult::Ambiguous {
            candidates,
            usable_count,
            thinking_level: if warning.is_some() { None } else { Some(level) },
            warning,
        },
        ModelReferenceResult::NotFound { target } => ModelReferenceResult::NotFound { target },
    }
}

/// `{...inner, thinkingLevel: undefined, warning: <new>}`。
///
/// 用于 `lenient` 分支：非法后缀被剥掉，因此内层可能解析出的 thinking 等级必须清空，
/// 只保留新警告。`NotFound` 原样透传（没有可附着的模型）。
fn with_warning(result: ModelReferenceResult, new_warning: String) -> ModelReferenceResult {
    match result {
        ModelReferenceResult::Resolved { model, .. } => ModelReferenceResult::Resolved {
            model,
            thinking_level: None,
            warning: Some(new_warning),
        },
        ModelReferenceResult::Ambiguous {
            candidates,
            usable_count,
            ..
        } => ModelReferenceResult::Ambiguous {
            candidates,
            usable_count,
            thinking_level: None,
            warning: Some(new_warning),
        },
        ModelReferenceResult::NotFound { target } => ModelReferenceResult::NotFound { target },
    }
}

/// `selectReference` 的 6 步裁决。
///
/// `provider/rest` 只是提示；命名的 provider 若可用则胜出，否则唯一可用的裸匹配胜出；
/// 命名的匹配即使不可用也优先于不可用的裸匹配（以便 `--api-key` 首次配置）。
///
/// 步骤（与 `spec/ca-config-models.md` §2.21.7 对应）：
///
/// 1. `raw`：`id` 与全文（小写）相等的裸匹配；
/// 2. `named`：存在 `provider/` 前缀时，依次尝试 provider 内 id 等于 `model_text` 的
///    字面匹配、`raw` 中同 provider 的模型（其 id 等于**全文**）、provider 内的模糊
///    匹配（仅 fuzzy 为真）；
/// 3. `candidates = [named, ...其余 raw]`；为空则返回 `None`，让上层继续尝试；
/// 4. 候选全部来自同一 provider：直接 resolved（此时完全不看认证）；
/// 5. `named` 的 provider 可用：resolved；
/// 6. 其余候选中恰好一个可用：resolved；没有可用的其余候选但有 `named`：resolved
///    （首次用 `--api-key` 配置时也能选中指定 provider）；否则 ambiguous，
///    `usable_count` 是可用的其余候选数。
fn select_reference(
    text: &str,
    pool: &[Model],
    fuzzy: bool,
    usable: &dyn Fn(&str) -> bool,
) -> Option<ModelReferenceResult> {
    let lower_text = text.to_lowercase();
    let raw: Vec<&Model> = pool
        .iter()
        .filter(|model| model.id.to_lowercase() == lower_text)
        .collect();
    let split = split_provider_prefix(text, pool);
    let named: Option<&Model> = split.as_ref().and_then(|split| {
        let provider_models: Vec<&Model> = pool
            .iter()
            .filter(|model| model.provider == split.provider)
            .collect();
        let lower_model_text = split.model_text.to_lowercase();
        // provider 自己的字面 id（OpenRouter 的 `openrouter/free`）压过它的模糊匹配。
        provider_models
            .iter()
            .find(|model| model.id.to_lowercase() == lower_model_text)
            .copied()
            .or_else(|| {
                raw.iter()
                    .find(|model| model.provider == split.provider)
                    .copied()
            })
            .or_else(|| {
                if fuzzy {
                    best_fuzzy_match(&fuzzy_matches(
                        &split.model_text,
                        provider_models.iter().copied(),
                    ))
                } else {
                    None
                }
            })
    });

    let others: Vec<&Model> = raw
        .iter()
        .filter(|model| named.is_none_or(|named| !std::ptr::eq(**model, named)))
        .copied()
        .collect();
    let mut candidates: Vec<&Model> = Vec::new();
    if let Some(named) = named {
        candidates.push(named);
    }
    candidates.extend(others.iter().copied());
    if candidates.is_empty() {
        return None;
    }
    let providers: HashSet<&str> = candidates
        .iter()
        .map(|model| model.provider.as_str())
        .collect();
    if providers.len() == 1 {
        return Some(resolved(candidates[0]));
    }
    if let Some(named) = named
        && usable(&named.provider)
    {
        return Some(resolved(named));
    }
    let usable_others: Vec<&Model> = others
        .iter()
        .filter(|model| usable(&model.provider))
        .copied()
        .collect();
    if usable_others.len() == 1 {
        return Some(resolved(usable_others[0]));
    }
    if usable_others.is_empty()
        && let Some(named) = named
    {
        return Some(resolved(named));
    }
    Some(ModelReferenceResult::Ambiguous {
        candidates: candidates.into_iter().cloned().collect(),
        usable_count: usable_others.len(),
        thinking_level: None,
        warning: None,
    })
}

/// `fuzzyMatches(text, models)`：id 或 name 包含 `text`（小写）。
fn fuzzy_matches<'a>(text: &str, models: impl IntoIterator<Item = &'a Model>) -> Vec<&'a Model> {
    let lower_text = text.to_lowercase();
    models
        .into_iter()
        .filter(|model| {
            model.id.to_lowercase().contains(&lower_text)
                || model.name.to_lowercase().contains(&lower_text)
        })
        .collect()
}

/// `selectFuzzy(matches, usable)`。
///
/// 只有候选跨多个 provider 时才用 `usable` 过滤；单一 provider 时不过滤（该 provider
/// 是否配置认证交给调用方展示）。过滤后的池子里取别名优先、`localeCompare` 降序的
/// 「最新」项；若同一 `id` 仍出现在多个 provider，返回 ambiguous，`candidates` 只含
/// 这些并列项，避免目录顺序决定结果。
///
/// `usable_count` 仅在「候选跨 provider 且 `usable_matches` 非空」时等于并列项数；
/// 否则为 0。因此 0 同时覆盖「未做过滤（单一 provider）」与「过滤后无可用项」（此时
/// `pool` 回退到全部 matches）两种情形，不能用来区分二者；`resolve_cli_model` 也把
/// 0 解释为「没有已认证的匹配 provider」。
fn select_fuzzy(matches: &[&Model], usable: &dyn Fn(&str) -> bool) -> Option<ModelReferenceResult> {
    if matches.is_empty() {
        return None;
    }
    let providers: HashSet<&str> = matches
        .iter()
        .map(|model| model.provider.as_str())
        .collect();
    let multiple_providers = providers.len() > 1;
    let usable_matches: Vec<&Model> = if multiple_providers {
        matches
            .iter()
            .filter(|model| usable(&model.provider))
            .copied()
            .collect()
    } else {
        Vec::new()
    };
    let pool: &[&Model] = if usable_matches.is_empty() {
        matches
    } else {
        &usable_matches
    };
    let best = best_fuzzy_match(pool)?;
    // 同一 id 出现在多个 provider 时，否则会被目录顺序决定。
    let tied: Vec<&Model> = pool
        .iter()
        .filter(|model| model.id == best.id)
        .copied()
        .collect();
    let tied_providers: HashSet<&str> = tied.iter().map(|model| model.provider.as_str()).collect();
    if tied_providers.len() == 1 {
        return Some(resolved(best));
    }
    let usable_count = if usable_matches.is_empty() {
        0
    } else {
        tied.len()
    };
    Some(ModelReferenceResult::Ambiguous {
        candidates: tied.into_iter().cloned().collect(),
        usable_count,
        thinking_level: None,
        warning: None,
    })
}

/// `bestFuzzyMatch(matches)`：别名优先，再按 `b.id.localeCompare(a.id)` 降序取第 0 项。
fn best_fuzzy_match<'a>(matches: &[&'a Model]) -> Option<&'a Model> {
    let aliases: Vec<&Model> = matches
        .iter()
        .filter(|model| is_alias(&model.id))
        .copied()
        .collect();
    let mut pool: Vec<&Model> = if aliases.is_empty() {
        matches.to_vec()
    } else {
        aliases
    };
    // JS `Array.prototype.sort` 是稳定排序；`sort()[0]` 等价于稳定降序后的首项。
    pool.sort_by(|a, b| locale_compare(&b.id, &a.id));
    pool.first().copied()
}

/// `isAlias(id)`：`id.endsWith("-latest") || !/-\d{8}$/.test(id)`。
fn is_alias(id: &str) -> bool {
    id.ends_with("-latest") || !has_date_suffix(id)
}

/// `-\d{8}$`（`\d` 恒为 ASCII 数字）。
fn has_date_suffix(id: &str) -> bool {
    let bytes = id.as_bytes();
    if bytes.len() < 9 {
        return false;
    }
    let start = bytes.len() - 9;
    bytes[start] == b'-' && bytes[start + 1..].iter().all(u8::is_ascii_digit)
}

/// `buildFallbackModel(providerModels, modelId)`：复制 anchor/default 模型，替换 id 与 name。
///
/// 基座选择：`providerModels[0].provider` 在 [`DEFAULT_MODEL_PER_PROVIDER`] 里有默认
/// id、且该 id 出现在 `providerModels` 中时用后者，否则用 `providerModels[0]`（anchor）。
/// 除 `id` / `name` 外的字段（上下文窗口、价格、`reasoning` 等）全部原样继承，调用方
/// 可以在此基础上再改 `reasoning`（见 [`resolve_cli_model`]）。
///
/// Panics：`providerModels` 为空。生产路径上它来自
/// [`ModelReferenceTarget::provider_models`]，由 `resolve_model_reference` 构造时保证
/// 非空（anchor 在首位）。
fn build_fallback_model(provider_models: &[Model], model_id: &str) -> Model {
    let anchor = provider_models
        .first()
        .expect("providerModels is non-empty by construction");
    let default_id = DEFAULT_MODEL_PER_PROVIDER.get(anchor.provider.as_str());
    let base_model = match default_id
        .and_then(|default_id| provider_models.iter().find(|model| model.id == *default_id))
    {
        Some(model) => model,
        None => anchor,
    };
    Model {
        id: model_id.to_string(),
        name: model_id.to_string(),
        ..base_model.clone()
    }
}

fn resolved(model: &Model) -> ModelReferenceResult {
    ModelReferenceResult::Resolved {
        model: model.clone(),
        thinking_level: None,
        warning: None,
    }
}

// ---------------------------------------------------------------------------
// 作用域展开
// ---------------------------------------------------------------------------

/// `modelsAreEqual`（type + id + provider）；resolver 里的模型都是 chat。
fn same_model(left: &Model, right: &Model) -> bool {
    models_are_equal(
        Some(&AnyModel::Chat(left.clone())),
        Some(&AnyModel::Chat(right.clone())),
    )
}

/// minimatch（`minimatch@10.2.6`，`minimatch(path, pattern, { nocase: true })`）的复刻。
///
/// `globset` 无法忠实复刻：它按字节做大小写折叠、`?`/字符类按 UTF-8 字节计数，
/// 且不支持 POSIX 类（`[[:alpha:]]`）与 extglob（`@(a|b)`）。这里按 minimatch 的
/// AST 语义重写：brace 展开（brace-expansion 5.x）、按 `/` 切段、`**` globstar、
/// 单段内的 `*` / `?` / `[...]` / `[[:class:]]` / extglob、dot 规则与前导 `!` 取反。
///
/// 只实现默认选项 + `nocase`（调用点固定 `{nocase: true}`）；`dot`、`partial`、
/// `matchBase`、`nonegate`、`nocomment`、`preserveMultipleSlashes`、
/// `windowsPathsNoEscape` 均未开启。
fn minimatch(candidate: &str, pattern: &str) -> bool {
    minimatch_10::minimatch(candidate, pattern)
}

mod minimatch_10 {
    // 本模块按 `minimatch@10.2.6` 的实现分层：
    // 1. 入口 `minimatch`：长度上限、注释模式、空模式、前导 `!`、brace 展开；
    // 2. 路径切分与段编译：`slash_split` → `level_one_optimize` → `Part`
    //    （`**` / 空段 / `Fast` 快捷路径 / 普通段）；
    // 3. `parse_segment`：UTF-16 码元 → atom AST（`parse_glob_sequence` /
    //    `parse_class` / extglob 递归），再做 fillNegs、大小写折叠、dot 遍历守卫与
    //    `\|` 交替切分；
    // 4. 匹配：`match_one`（globstar 分派）与 `match_segment`（用位置集合模拟正则的
    //    NFA），字符类与 POSIX 类在 `class_matches`；
    // 5. 末尾测试：黄金夹具由 TS 参考实现生成。
    //
    // 所有索引都以 UTF-16 码元为单位，与 JS 正则的匹配单位一致；大小写折叠模式由
    // 段内是否含「非 ASCII 的 POSIX 类」（对应正则的 `u` 标志）决定，见 `FoldMode`。
    use std::collections::BTreeSet;
    use std::sync::LazyLock;

    use regex::Regex;

    const MAX_BRACE: usize = 100_000;
    const MAX_BRACE_LENGTH: usize = 4_000_000;
    const MAX_BRACE_DEPTH: usize = 1_000;
    const MAX_BRACE_REWRITES: usize = 1_000;
    const MAX_GLOBSTAR_RECURSION: usize = 200;

    // -----------------------------------------------------------------------
    // 入口
    // -----------------------------------------------------------------------

    pub(super) fn minimatch(candidate: &str, pattern: &str) -> bool {
        // `assertValidPattern()`（minimatch@10.2.6）：TS 对超过 65536 个 UTF-16 码元的
        // 模式抛 `TypeError: pattern is too long`。本 crate 的 glob 入口没有错误通道
        // （`resolveModelScopeFromModels` 在 TS 里是同步函数、异常会向上冒泡），
        // 这里返回 false（等价于“无匹配”+ no-match 警告），避免二次复杂度的评估。
        if pattern.encode_utf16().count() > 65536 {
            return false;
        }
        // `minimatch()` 的快捷路径：注释模式不匹配任何东西。
        if pattern.starts_with('#') {
            return false;
        }
        if pattern.is_empty() {
            // `Minimatch#make`：空模式只匹配空字符串。
            return candidate.is_empty();
        }
        // `nocase` 只影响匹配阶段：brace 展开必须先按原始大小写做（`{a..A}` 的字母
        // 序列按 charCode 展开），随后每个展开结果再做大小写折叠（见 [`FoldMode`]）。
        let (negate, pattern) = parse_negate(pattern);
        let mut expanded = brace_expand(pattern);
        dedupe(&mut expanded);
        // 与 TS 一样保留原始大小写：逐段折叠见 [`match_segment`] / [`FastTest`]。
        let file = slash_split(candidate);
        for item in &expanded {
            let parts = compile_pattern(item);
            if match_one(&file, &parts, 0, 0) {
                return !negate;
            }
        }
        negate
    }

    fn dedupe(items: &mut Vec<String>) {
        let mut seen = std::collections::HashSet::new();
        items.retain(|item| seen.insert(item.clone()));
    }

    /// `parseNegate()`：前导 `!` 取反（`!!` 取反两次）。
    fn parse_negate(pattern: &str) -> (bool, &str) {
        let mut negate = false;
        let mut offset = 0;
        for (index, character) in pattern.char_indices() {
            if character != '!' {
                break;
            }
            negate = !negate;
            offset = index + 1;
        }
        (negate, &pattern[offset..])
    }

    /// 大小写无关匹配的折叠模式。
    ///
    /// minimatch 的 `nocase` 让每段变成 JS 正则并加 `i` 标志；这里把 pattern 与路径都折叠
    /// 后按大小写敏感比较。ECMAScript 的折叠取决于正则是否带 `u` 标志：
    ///
    /// - **非 `u`**（普通 glob）：`Canonicalize` = `toUpperCase` 的单字符结果，且
    ///   `ch >= 128 && upper < 128` 时保持原样。`k`/`K` 互相匹配而 Kelvin 符号 `K`
    ///   （U+212A）**不**匹配；`ß` 与 `ẞ` 不匹配；`σ`/`ς` 匹配。
    /// - **`u`**（含 POSIX 类时 minimatch 会加 `u`）：simple case folding
    ///   （CaseFolding.txt 的 C/S 映射），Kelvin 符号、`ſ`、`ẞ` 都参与折叠。
    ///
    /// 候选串在非 `u` 模式下按 UTF-16 码元折叠（代理项没有大小写映射），`u` 模式下按码点
    /// 折叠——与 JS 正则的匹配单位一致。
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum FoldMode {
        /// 非 `u` 正则：ECMAScript `Canonicalize`。
        Canonical,
        /// `u` 正则：simple case folding。
        Simple,
    }

    /// `Canonicalize(ch)`（ECMAScript，`Unicode` 为 false）。
    fn canonicalize_char(character: char) -> char {
        // 代理项（只以 u16 形式出现）没有大小写映射，调用方直接保留。
        let mut uppercase = character.to_uppercase();
        match (uppercase.next(), uppercase.next()) {
            (Some(mapped), None) => {
                if u32::from(character) >= 128 && u32::from(mapped) < 128 {
                    character
                } else {
                    mapped
                }
            }
            // `toUpperCase` 返回多个字符（`ß` → `SS`）时不做映射。
            _ => character,
        }
    }

    /// simple case folding：CaseFolding.txt 的 C/S 映射。
    fn simple_fold_char(character: char) -> char {
        if let Some(folded) = simple_case_fold(character) {
            return folded;
        }
        // 没有单字符小写映射的码点保持原样，而不是展开成多个码点。
        let mut lowercase = character.to_lowercase();
        match (lowercase.next(), lowercase.next()) {
            (Some(mapped), None) => mapped,
            _ => character,
        }
    }

    /// 单个 UTF-16 码元在非 `u` 模式下的 `Canonicalize`（代理项保持不变）。
    fn canonicalize_unit(unit: u16) -> u16 {
        if unit < 0x80 {
            return if (u16::from(b'a')..=u16::from(b'z')).contains(&unit) {
                unit - u16::from(b'a' - b'A')
            } else {
                unit
            };
        }
        let Some(character) = char::from_u32(u32::from(unit)) else {
            return unit;
        };
        let mapped = u32::from(canonicalize_char(character));
        if mapped <= 0xffff {
            mapped as u16
        } else {
            unit
        }
    }

    /// 单个 UTF-16 码元在 `u` 模式下的 simple case folding（代理项保持不变）。
    fn simple_fold_unit(unit: u16) -> u16 {
        let Some(character) = char::from_u32(u32::from(unit)) else {
            return unit;
        };
        let mapped = u32::from(simple_fold_char(character));
        if mapped <= 0xffff {
            mapped as u16
        } else {
            unit
        }
    }

    /// CaseFolding.txt 的 C/S 特例（[`simple_case_fold`] 的键）。非 `u` 模式的类范围
    /// 需要它们的 Canonicalize 原像（例如 `[µ-¶]` 匹配 `Μ`、`[ς]` 匹配 `Σ`）。
    const CASE_FOLD_SPECIALS: &[u32] = &[
        0x00b5, 0x017f, 0x0345, 0x03c2, 0x03d0, 0x03d1, 0x03d5, 0x03d6, 0x03f0, 0x03f1, 0x03f5,
        0x1c80, 0x1c81, 0x1c82, 0x1c83, 0x1c84, 0x1c85, 0x1c86, 0x1c87, 0x1c88, 0x1e9b, 0x1fbe,
    ];

    /// 非 `u` 模式下字符类范围的大小写无关成员判定。
    ///
    /// JS 非 `u` 正则的类集合是「范围内每个码点的 `Canonicalize` 像」（逐成员映射，
    /// 不是折叠端点——`[a-K]` 的像不含 `\`）。`folded` 是已折叠的输入码点；匹配即
    /// 判断它的原像是否落在 `[low, high]` 内。
    fn canonical_range_contains(low: u32, high: u32, folded: u32) -> bool {
        let in_range = |value: u32| (low..=high).contains(&value);
        if in_range(folded) {
            return true;
        }
        let Some(character) = char::from_u32(folded) else {
            return false;
        };
        // 小写变体是原像当且仅当它再次 Canonicalize 回 `folded`：Kelvin 符号的
        // 小写是 `k`，但 `Canonicalize(k) = K ≠ Kelvin`，必须排除。
        let mut lowercase = character.to_lowercase();
        let lower_variant = match (lowercase.next(), lowercase.next()) {
            (Some(mapped), None) if u32::from(canonicalize_char(mapped)) == folded => Some(mapped),
            _ => None,
        };
        if let Some(mapped) = lower_variant
            && in_range(u32::from(mapped))
        {
            return true;
        }
        for special in CASE_FOLD_SPECIALS {
            if *special == folded {
                continue;
            }
            let Some(candidate) = char::from_u32(*special) else {
                continue;
            };
            if u32::from(canonicalize_char(candidate)) == folded && in_range(*special) {
                return true;
            }
        }
        false
    }

    /// 折叠整段候选串。非 `u` 模式逐 UTF-16 码元，`u` 模式逐码点。
    fn fold_string(text: &str, mode: FoldMode) -> Vec<u16> {
        match mode {
            FoldMode::Canonical => text.encode_utf16().map(canonicalize_unit).collect(),
            FoldMode::Simple => {
                let mut out = Vec::with_capacity(text.len());
                for character in text.chars() {
                    let folded = simple_fold_char(character);
                    let mut buffer = [0u16; 2];
                    out.extend_from_slice(folded.encode_utf16(&mut buffer));
                }
                out
            }
        }
    }

    /// 折叠已解析的 atom（literal、字符类范围与 extglob 备选）。
    ///
    /// `Simple` 模式下相邻的代理项 literal 必须作为码点一起折叠（`𐐀`/`𐐨` 这类
    /// 星空面字母对在 `/u` 正则里大小写匹配）；`Canonical` 模式逐码元折叠，星空面字符
    /// 的代理项没有映射，与 JS 非 `u` 正则一致。
    fn fold_atoms(atoms: &mut [Atom], mode: FoldMode) {
        let mut index = 0;
        while index < atoms.len() {
            if let Atom::Lit(unit) = atoms[index] {
                if mode == FoldMode::Simple && (0xd800..=0xdbff).contains(&unit) {
                    let low = match atoms.get(index + 1) {
                        Some(Atom::Lit(low)) => Some(*low),
                        _ => None,
                    };
                    if let Some(low) = low
                        && (0xdc00..=0xdfff).contains(&low)
                    {
                        let code_point = 0x10000
                            + ((u32::from(unit) - 0xd800) << 10)
                            + (u32::from(low) - 0xdc00);
                        if let Some(character) = char::from_u32(code_point) {
                            let folded = u32::from(simple_fold_char(character));
                            if folded > 0xffff {
                                let folded = folded - 0x10000;
                                atoms[index] = Atom::Lit((0xd800 + (folded >> 10)) as u16);
                                atoms[index + 1] = Atom::Lit((0xdc00 + (folded & 0x3ff)) as u16);
                            }
                        }
                        index += 2;
                        continue;
                    }
                }
                atoms[index] = Atom::Lit(match mode {
                    FoldMode::Canonical => canonicalize_unit(unit),
                    FoldMode::Simple => simple_fold_unit(unit),
                });
                index += 1;
                continue;
            }
            match &mut atoms[index] {
                Atom::Class(set) => {
                    // `u` 模式的类集合按折叠后的码点比较，可以折叠端点；非 `u` 模式
                    // 的集合是「范围内每个码点的 Canonicalize 像」，端点折叠会错误地
                    // 扩大范围（`[a-K]` 的像不含 `\`），见 [`canonical_range_contains`]。
                    if mode == FoldMode::Simple {
                        for member in &mut set.ranges {
                            if let ClassMember::Range(low, high) = member {
                                let fold = |value: u32| -> u32 {
                                    if value > 0xffff {
                                        value
                                    } else {
                                        u32::from(simple_fold_unit(value as u16))
                                    }
                                };
                                *low = fold(*low);
                                *high = fold(*high);
                            }
                        }
                    }
                }
                Atom::Ext(ext) => {
                    for alternative in &mut ext.alternatives {
                        fold_atoms(&mut alternative.atoms, mode);
                    }
                }
                _ => {}
            }
            index += 1;
        }
    }

    /// CaseFolding.txt 中与 `to_lowercase` 不一致的 C/S 映射（其余由小写映射覆盖）。
    fn simple_case_fold(character: char) -> Option<char> {
        Some(match character {
            '\u{00b5}' => '\u{03bc}',
            '\u{017f}' => 's',
            '\u{0345}' => '\u{03b9}',
            '\u{03c2}' => '\u{03c3}',
            '\u{03d0}' => '\u{03b2}',
            '\u{03d1}' => '\u{03b8}',
            '\u{03d5}' => '\u{03c6}',
            '\u{03d6}' => '\u{03c0}',
            '\u{03f0}' => '\u{03ba}',
            '\u{03f1}' => '\u{03c1}',
            '\u{03f5}' => '\u{03b5}',
            '\u{1c80}' => '\u{0432}',
            '\u{1c81}' => '\u{0434}',
            '\u{1c82}' => '\u{043e}',
            '\u{1c83}' => '\u{0441}',
            '\u{1c84}' | '\u{1c85}' => '\u{0442}',
            '\u{1c86}' => '\u{044a}',
            '\u{1c87}' => '\u{0463}',
            '\u{1c88}' => '\u{a64b}',
            '\u{1e9b}' => '\u{1e61}',
            '\u{1fbe}' => '\u{03b9}',
            _ => return None,
        })
    }

    // -----------------------------------------------------------------------
    // 路径切分
    // -----------------------------------------------------------------------

    /// `split(/\/+/)`：任意长度的 `/` 都算一个分隔符，首尾空段保留。
    fn slash_split(text: &str) -> Vec<String> {
        let mut parts = Vec::new();
        let mut current = String::new();
        let mut characters = text.chars().peekable();
        while let Some(character) = characters.next() {
            if character == '/' {
                parts.push(std::mem::take(&mut current));
                while matches!(characters.peek(), Some('/')) {
                    characters.next();
                }
            } else {
                current.push(character);
            }
        }
        parts.push(current);
        parts
    }

    /// `levelOneOptimize`：合并相邻 `**`、消掉非 `**` 前的 `..`；空结果变 `[""]`。
    fn level_one_optimize(parts: Vec<String>) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for part in parts {
            if part == "**" && out.last().is_some_and(|previous| previous == "**") {
                continue;
            }
            if part == ".."
                && let Some(previous) = out.last()
                && previous != ".."
                && previous != "."
                && previous != "**"
            {
                out.pop();
                continue;
            }
            out.push(part);
        }
        if out.is_empty() {
            out.push(String::new());
        }
        out
    }

    // -----------------------------------------------------------------------
    // brace 展开（brace-expansion@5.0.12）
    // -----------------------------------------------------------------------

    const ESC_SLASH: &str = "\u{1}SLASH\u{1}";
    const ESC_OPEN: &str = "\u{1}OPEN\u{1}";
    const ESC_CLOSE: &str = "\u{1}CLOSE\u{1}";
    const ESC_COMMA: &str = "\u{1}COMMA\u{1}";
    const ESC_PERIOD: &str = "\u{1}PERIOD\u{1}";

    /// `minimatch.braceExpand(pattern)`：没有花括号组时原样返回。
    fn brace_expand(pattern: &str) -> Vec<String> {
        // 快捷路径：`/\{(?:(?!\{).)*\}/`。
        if !has_brace_group(pattern) {
            return vec![pattern.to_string()];
        }
        // Bash 4.3 的怪癖：开头的 `{}` 不被当成组。
        let mut input = pattern.to_string();
        if input.starts_with("{}") {
            input = format!("\\{{\\}}{}", &input[2..]);
        }
        let escaped = escape_braces(&input);
        expand_(&escaped, 0, true)
            .into_iter()
            .map(|item| unescape_braces(&item))
            .collect()
    }

    fn has_brace_group(pattern: &str) -> bool {
        let characters: Vec<char> = pattern.chars().collect();
        for (index, character) in characters.iter().enumerate() {
            if *character != '{' {
                continue;
            }
            for next in &characters[index + 1..] {
                if *next == '{' {
                    break;
                }
                if *next == '}' {
                    return true;
                }
            }
        }
        false
    }

    fn escape_braces(text: &str) -> String {
        text.replace("\\\\", ESC_SLASH)
            .replace("\\{", ESC_OPEN)
            .replace("\\}", ESC_CLOSE)
            .replace("\\,", ESC_COMMA)
            .replace("\\.", ESC_PERIOD)
    }

    fn unescape_braces(text: &str) -> String {
        text.replace(ESC_SLASH, "\\")
            .replace(ESC_OPEN, "{")
            .replace(ESC_CLOSE, "}")
            .replace(ESC_COMMA, ",")
            .replace(ESC_PERIOD, ".")
    }

    /// `balanced('{', '}', str)`：第一个能闭合的 `{...}` 组。
    fn balanced(text: &str) -> Option<(&str, &str, &str)> {
        let bytes = text.as_bytes();
        for start in 0..bytes.len() {
            if bytes[start] != b'{' {
                continue;
            }
            let mut depth = 0i32;
            for (index, byte) in bytes.iter().enumerate().skip(start) {
                if *byte == b'{' {
                    depth += 1;
                } else if *byte == b'}' {
                    depth -= 1;
                    if depth == 0 {
                        return Some((&text[..start], &text[start + 1..index], &text[index + 1..]));
                    }
                }
            }
        }
        None
    }

    /// 把序列端点/步长数值化：先按 `f64` 解析并截断；解析失败时退回首字符码点。
    ///
    /// 字母序列（`{a..z}`）依赖这个回退：`"a"` 不是数字，于是得到 `'a'` 的码点 97，
    /// `expand_sequence` 再把数值当码点转回字符。
    fn numeric(text: &str) -> f64 {
        text.parse::<f64>()
            .map(|value| value.trunc())
            .unwrap_or_else(|_| f64::from(text.chars().next().map_or(0, u32::from)))
    }

    /// 按**顶层**逗号切分（嵌套 `{}` 内的逗号属于内层组），空片段保留。
    fn parse_comma_parts(text: &str) -> Vec<String> {
        let mut parts = Vec::new();
        let mut current = String::new();
        let mut depth = 0i32;
        for character in text.chars() {
            match character {
                '{' => {
                    depth += 1;
                    current.push(character);
                }
                '}' => {
                    depth -= 1;
                    current.push(character);
                }
                ',' if depth == 0 => parts.push(std::mem::take(&mut current)),
                _ => current.push(character),
            }
        }
        parts.push(current);
        parts
    }

    /// 笛卡尔积式拼接：`accumulator` 的每一项与 `pre + value` 组合；`drop_empties`
    /// 丢弃空结果。结果条数超过 `MAX_BRACE` 或总 UTF-16 长度超过
    /// `MAX_BRACE_LENGTH` 时**截断**并返回已有结果（对应 brace-expansion 的上限行为，
    /// 不报错）。
    fn combine(
        accumulator: &[String],
        pre: &str,
        values: &[String],
        drop_empties: bool,
    ) -> Vec<String> {
        let mut out = Vec::new();
        let mut length = 0usize;
        for base in accumulator {
            for value in values {
                if out.len() >= MAX_BRACE {
                    return out;
                }
                let expansion = format!("{base}{pre}{value}");
                if drop_empties && expansion.is_empty() {
                    continue;
                }
                let size: usize = expansion.encode_utf16().count();
                if length + size > MAX_BRACE_LENGTH {
                    return out;
                }
                out.push(expansion);
                length += size;
            }
        }
        out
    }

    static NUMERIC_SEQUENCE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^-?\d+\.\.-?\d+(?:\.\.-?\d+)?$").expect("numeric range regex")
    });
    static ALPHA_SEQUENCE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^[a-zA-Z]\.\.[a-zA-Z](?:\.\.-?\d+)?$").expect("alpha range regex")
    });

    /// 展开 `{x..y}` / `{x..y..step}`（数值或单个字母的序列）。
    ///
    /// - 步长取绝对值且至少 1；`y < x` 时转为反向递减，循环条件随方向切换；
    /// - 端点带前导零（`is_padded`）时按两端较长的字符数补零，负号留在最前；
    /// - 字母序列把数值当码点转回字符（数值来自 `numeric` 的首字符回退）；
    /// - 结果受 `MAX_BRACE` / `MAX_BRACE_LENGTH` 截断；
    /// - `value + increment == value`（f64 精度不足以表示该加法，例如 +1 在
    ///   |value| ≥ 2^53 后）时提前结束：参考实现会把同一个值重复到上限、随后被去重，
    ///   集合等价（见下方内联注释）。
    fn expand_sequence(body: &str) -> Vec<String> {
        let pieces: Vec<&str> = body.split("..").collect();
        let (Some(first), Some(second)) = (pieces.first(), pieces.get(1)) else {
            return Vec::new();
        };
        let is_alpha = ALPHA_SEQUENCE.is_match(body);
        let x = numeric(first);
        let y = numeric(second);
        let width = first.chars().count().max(second.chars().count());
        let mut increment = match pieces.get(2) {
            Some(step) => numeric(step).abs().max(1.0),
            None => 1.0,
        };
        let reverse = y < x;
        if reverse {
            increment = -increment;
        }
        let pad = pieces.iter().any(|piece| is_padded(piece));
        let mut out = Vec::new();
        let mut length = 0usize;
        let mut value = x;
        while (if reverse { value >= y } else { value <= y }) && out.len() < MAX_BRACE {
            let mut text = if is_alpha {
                let code = value as u32;
                char::from_u32(code).map(String::from).unwrap_or_default()
            } else {
                format_js_integer(value)
            };
            if is_alpha && text == "\\" {
                text.clear();
            }
            if !is_alpha && pad {
                let need = width.saturating_sub(text.chars().count());
                if need > 0 {
                    let zeros: String = std::iter::repeat_n('0', need).collect();
                    if value < 0.0 {
                        text = format!("-{zeros}{}", &text[1..]);
                    } else {
                        text = format!("{zeros}{text}");
                    }
                }
            }
            let size: usize = text.encode_utf16().count();
            if length + size > MAX_BRACE_LENGTH {
                break;
            }
            out.push(text);
            length += size;
            // `|value| >= 2^53` 时 `value + 1` 在 f64 里不再变化：brace-expansion 会
            // 把同一个值重复到 `EXPANSION_MAX`（100k），而 minimatch 紧接着用
            // `new Set(...)` 去重。集合等价，这里直接结束，避免 10 万次无意义的格式化。
            if value + increment == value {
                break;
            }
            value += increment;
        }
        out
    }

    /// `String(number)`（brace-expansion 的 `String(i)`）：必须走 ECMAScript
    /// `Number::toString`，`1e21` 要写成 `"1e+21"` 而不是 22 位十进制字面量。
    /// `NaN`/`±Infinity` 是 `parseInt` 溢出时的边界值，与 JS 文案一致。
    fn format_js_integer(value: f64) -> String {
        if value.is_nan() {
            return "NaN".to_string();
        }
        if value.is_infinite() {
            return if value < 0.0 { "-Infinity" } else { "Infinity" }.to_string();
        }
        kiana_json::format_js_number(value)
    }

    /// 端点是否带前导零（可带负号）：`0` 后紧跟数字，如 `01`、`-007`。
    fn is_padded(text: &str) -> bool {
        let bytes = text.as_bytes();
        let mut index = 0;
        if bytes.first() == Some(&b'-') {
            index = 1;
        }
        bytes.get(index) == Some(&b'0') && bytes.get(index + 1).is_some_and(u8::is_ascii_digit)
    }

    /// 递归展开一个已转义的模式串（brace-expansion 的 `expand`）。
    ///
    /// 输入是 `escape_braces` 处理过的串：`\{` / `\}` / `\,` / `\.` / `\\` 已是控制
    /// 字符占位符，不会被误当成分组语法。循环逐组处理：
    ///
    /// 1. `balanced` 取出第一个可闭合的 `{...}`；没有就把剩余文本接到累加器并结束；
    /// 2. `$` 之前的 `{...}`（`${a,b}`）按字面量保留；
    /// 3. 组体匹配数值/字母序列 → `expand_sequence`；含顶层逗号 → 递归展开每个备选；
    ///    两者都不是时，若后面还有可能闭合的 `}` 就重写剩余串再试（上限
    ///    `MAX_BRACE_REWRITES`），否则把整段当字面量；
    /// 4. `drop_empties`：丢弃最终展开为空串的结果（如 `{b,}` → `b`），而不是丢弃
    ///    选项组内的空备选本身——`a{b,}c` 的 `ac` 是空备选与前后缀的正常拼接
    ///    （`combine` 收到的 `drop_empties && post.is_empty()` 为 false）；
    ///    该标志只在最外层、且当前组后没有剩余文本时生效；
    /// 5. 每轮用 `combine` 把前缀与候选值做笛卡尔积，受两个上限截断。
    ///
    /// `depth` 超过 `MAX_BRACE_DEPTH` 时停止展开并原样返回，避免病态嵌套耗尽栈。
    fn expand_(input: &str, depth: usize, is_top: bool) -> Vec<String> {
        if depth > MAX_BRACE_DEPTH {
            return vec![input.to_string()];
        }
        let mut accumulator: Vec<String> = vec![String::new()];
        let mut rest = input.to_string();
        let mut drop_empties = false;
        let mut first_group = true;
        let mut rewrites = 0usize;
        loop {
            let Some((pre, body, post)) = balanced(&rest) else {
                return combine(&accumulator, &rest, &[String::new()], drop_empties);
            };
            let pre = pre.to_string();
            let body = body.to_string();
            let post = post.to_string();
            if pre.ends_with('$') {
                let literal = format!("{pre}{{{body}}}");
                accumulator = combine(
                    &accumulator,
                    &literal,
                    &[String::new()],
                    drop_empties && post.is_empty(),
                );
                first_group = false;
                if post.is_empty() {
                    break;
                }
                rest = post;
                continue;
            }
            let is_numeric = NUMERIC_SEQUENCE.is_match(&body);
            let is_alpha = ALPHA_SEQUENCE.is_match(&body);
            let is_sequence = is_numeric || is_alpha;
            let is_options = body.contains(',');
            if !is_sequence && !is_options {
                if rewrites < MAX_BRACE_REWRITES && post_needs_rewrite(&post) {
                    rewrites += 1;
                    rest = format!("{pre}{{{body}{ESC_CLOSE}{post}");
                    continue;
                }
                let literal = format!("{pre}{{{body}}}{post}");
                return combine(&accumulator, &literal, &[String::new()], drop_empties);
            }
            if first_group {
                drop_empties = is_top && !is_sequence;
                first_group = false;
            }
            let values: Vec<String> = if is_sequence {
                expand_sequence(&body)
            } else {
                let mut parts = parse_comma_parts(&body);
                if parts.len() == 1 {
                    // `x{{a,b}}y` → `x{a}y x{b}y`（再展开一次后仍是字面量）。
                    parts = expand_(&parts[0], depth + 1, false)
                        .into_iter()
                        .map(|item| format!("{{{item}}}"))
                        .collect();
                    if parts.len() == 1 {
                        let literal = format!("{pre}{}", parts[0]);
                        accumulator = combine(
                            &accumulator,
                            &literal,
                            &[String::new()],
                            drop_empties && post.is_empty(),
                        );
                        if post.is_empty() {
                            break;
                        }
                        rest = post;
                        continue;
                    }
                }
                let mut drops_empties = drop_empties && post.is_empty() && pre.is_empty();
                if drops_empties && accumulator.iter().any(|item| !item.is_empty()) {
                    drops_empties = false;
                }
                let mut values: Vec<String> = Vec::new();
                let mut values_length = 0usize;
                'outer: for part in &parts {
                    let expanded = expand_(part, depth + 1, false);
                    for value in expanded {
                        if drops_empties && value.is_empty() {
                            continue;
                        }
                        let size: usize = value.encode_utf16().count();
                        if values.len() >= MAX_BRACE || values_length + size > MAX_BRACE_LENGTH {
                            break 'outer;
                        }
                        values.push(value);
                        values_length += size;
                    }
                }
                values
            };
            accumulator = combine(&accumulator, &pre, &values, drop_empties && post.is_empty());
            if post.is_empty() {
                break;
            }
            rest = post;
        }
        accumulator
    }

    fn post_needs_rewrite(post: &str) -> bool {
        // `/,(?!,).*\}/`：某个逗号（后面不是逗号）之后还有 `}`。
        let characters: Vec<char> = post.chars().collect();
        for (index, character) in characters.iter().enumerate() {
            if *character == ','
                && characters.get(index + 1) != Some(&',')
                && characters[index + 1..].contains(&'}')
            {
                return true;
            }
        }
        false
    }

    // -----------------------------------------------------------------------
    // 单段 AST
    // -----------------------------------------------------------------------

    #[derive(Clone, Debug)]
    enum Atom {
        Lit(u16),
        Star,
        /// 来自「整段只有 `*`」的字符串片段：在 start && end 序列里要求至少一个字符。
        StarCandidate,
        Qmark,
        Class(ClassSet),
        /// 永远不匹配（空字符类、非法字符类）。
        Impossible,
        Ext(Ext),
        /// 转义的 `\|`：minimatch 的 `#parseGlob` 把反斜杠去掉后把裸 `|` 注入正则源码，
        /// 于是它在正则里是**交替**而不是字面量（`reSpecials` 里没有 `|`）。匹配前按
        /// 此标记切分，见 [`match_segment`] / [`match_alternatives`]。
        Pipe,
    }

    #[derive(Clone, Debug)]
    struct Ext {
        kind: ExtKind,
        alternatives: Vec<Alternative>,
        /// 最后一个备选是空串（`@(a|)`）；影响 `!` 组至少消费一个码元的规则。
        empty: bool,
    }

    #[derive(Clone, Debug)]
    struct Alternative {
        atoms: Vec<Atom>,
        /// 该备选是结构上的 start && end 且只含字符串片段：全 `*` 片段要求至少一个字符。
        nonempty_stars: bool,
        /// 该备选作为子 AST 渲染时的 `needNoTrav` 守卫（`@(.*)` 不匹配 "." / ".."）。
        no_traversal: bool,
    }

    impl Alternative {
        fn new(atoms: Vec<Atom>) -> Self {
            Alternative {
                atoms,
                nonempty_stars: false,
                no_traversal: false,
            }
        }
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum ExtKind {
        Bang,
        Question,
        Plus,
        Star,
        At,
    }

    fn ext_kind(unit: u16) -> Option<ExtKind> {
        match unit {
            0x21 => Some(ExtKind::Bang),
            0x3f => Some(ExtKind::Question),
            0x2b => Some(ExtKind::Plus),
            0x2a => Some(ExtKind::Star),
            0x40 => Some(ExtKind::At),
            _ => None,
        }
    }

    #[derive(Clone, Debug)]
    struct ClassSet {
        /// 正成员：单码点范围或 POSIX 类。
        ranges: Vec<ClassMember>,
        /// 取反类（`[:graph:]` 用的 `\P{Z}`/`\P{C}`），与 `negate` 组合成最终集合。
        negs: Vec<PosixNeg>,
        /// `[^...]` / `[!...]`。
        negate: bool,
        /// 含需要码点语义的 POSIX 类时为真（对应正则的 `u` 标志）。
        codepoint: bool,
    }

    #[derive(Clone, Debug)]
    enum ClassMember {
        /// 闭区间 `[low, high]`；单字符是 `low == high`。
        Range(u32, u32),
        /// POSIX 类。
        Posix(PosixClass),
    }

    #[derive(Clone, Copy, Debug)]
    enum PosixNeg {
        /// `\P{Z}` 且 `\P{C}`：既不是分隔符也不是「其它」类。
        ZOrC,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum PosixClass {
        Alnum,
        Alpha,
        Ascii,
        Blank,
        Cntrl,
        Digit,
        Lower,
        Print,
        Punct,
        Space,
        Upper,
        Word,
        Xdigit,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Category {
        Letter,
        LetterNumber,
        DecimalNumber,
        Separator,
        SpaceSeparator,
        Control,
        Lowercase,
        Uppercase,
        Titlecase,
        Punctuation,
        ConnectorPunctuation,
        Other,
    }

    #[derive(Clone, Debug)]
    struct Segment {
        /// `\|` 切分出的正则备选（通常只有一项）。根段的正则形如 `^A|B|C$`：
        /// 第一项只锚定开头、最后一项只锚定结尾、中间项可在任意位置。
        alternatives: Vec<Vec<Atom>>,
        /// 段内没有 glob 魔法（也没有因 `nocase` 大小写不同的字符）时，
        /// `toMMPattern()` 返回反转义后的普通字符串，`#matchOne` 按 `f === p` 精确比较。
        /// 此时 `\|` 是字面量 `|`，不构成正则交替。
        plain: Option<String>,
        /// 段内含 POSIX 类时 minimatch 会给正则加 `u` 标志（码点匹配 + simple case folding）。
        codepoint: bool,
        /// 整段是结构上的 start && end 且只含字符串片段：全 `*` 片段要求至少一个字符。
        nonempty_stars: bool,
        /// 正则开头的 `(?!(?:^|/)\.\.?(?:$|/))`：段首的 `\.` 后跟 `.`/`[` 类时，
        /// 整段（"." / ".."）不能匹配。见 `toRegExpSource` 的 `needNoTrav`。
        no_traversal: bool,
    }

    impl Segment {
        /// 该段使用的大小写折叠模式（`codepoint` 即 `u` 正则）。
        fn fold_mode(&self) -> FoldMode {
            if self.codepoint {
                FoldMode::Simple
            } else {
                FoldMode::Canonical
            }
        }
    }

    #[derive(Clone, Debug)]
    enum Part {
        Globstar,
        /// 空路径段（模式开头或结尾的 `/`；中间的连续 `/` 会被 `slash_split` 合并，
        /// `a//b` 切成 `["a", "b"]`，不产生 `Empty`）。
        Empty,
        /// `Minimatch#parse` 的快捷路径（`starRE` / `starDotExtRE` / `qmarksRE` /
        /// `starDotStarRE` / `dotStarRE`）：命中时 JS 直接替换正则的 `test`，因此
        /// **覆盖** AST 生成的匹配结果（即使模式里含转义）。
        Fast(FastTest),
        Segment(Segment),
    }

    /// 把模式切成段并分类：`**` → `Globstar`，空段 → `Empty`，命中 JS 快捷路径的段 →
    /// `Fast`，其余编译成 AST。`level_one_optimize` 先合并相邻 `**`、消掉非 `**` 前的
    /// `..`（与 minimatch 的预优化一致）。
    fn compile_pattern(pattern: &str) -> Vec<Part> {
        level_one_optimize(slash_split(pattern))
            .into_iter()
            .map(|part| match part.as_str() {
                "**" => Part::Globstar,
                "" => Part::Empty,
                _ => match fast_test(&part) {
                    Some(test) => Part::Fast(test),
                    None => Part::Segment(parse_segment(&part)),
                },
            })
            .collect()
    }

    /// 把单个路径段编译成 [`Segment`]。步骤顺序有语义依赖：
    ///
    /// 1. `parse_glob_sequence` 扫 UTF-16 码元生成 atom（extglob 递归解析）；
    /// 2. `postprocess_root`：整段只有一个 extglob 时按 JS 规则过滤空备选；
    /// 3. `fill_negs`：把 `!` 组之后的模式并进它的每个备选；
    /// 4. 计算 `codepoint`（是否 `u` 模式）与 `plain`（无魔法且
    ///    `toUpperCase() == toLowerCase()` 时退化为精确字符串比较；此时 `\|` 是
    ///    字面量而不是交替）；
    /// 5. `fold_atoms` 折叠大小写——必须放在 `plain` 判定之后，因为 `plain` 取的是
    ///    未折叠的原 atom；
    /// 6. `annotate` / `annotate_no_traversal` 标注 `nonempty_stars` 与遍历守卫；
    /// 7. `split_pipes` 最后拆出 `\|` 交替，匹配阶段不再处理 `Pipe`。
    fn parse_segment(pattern: &str) -> Segment {
        let units: Vec<u16> = pattern.encode_utf16().collect();
        let mut atoms = parse_glob_sequence(&units);
        postprocess_root(&mut atoms, &units);
        fill_negs(&mut atoms, &[]);
        let codepoint = atoms_use_codepoints(&atoms);
        // `toMMPattern()`：没有魔法且 `glob.toUpperCase() === glob.toLowerCase()` 时
        // 返回反转义后的字符串，`#matchOne` 用 `f === p` 精确比较。
        let plain = if atoms.iter().any(atom_has_magic)
            || pattern.to_uppercase() != pattern.to_lowercase()
        {
            None
        } else {
            Some(unescaped_atoms(&atoms))
        };
        let fold = if codepoint {
            FoldMode::Simple
        } else {
            FoldMode::Canonical
        };
        fold_atoms(&mut atoms, fold);
        let nonempty_stars = annotate(&mut atoms, true, true);
        annotate_no_traversal(&mut atoms);
        let no_traversal = no_traversal_guard(pattern, &atoms);
        let alternatives = split_pipes(&atoms);
        Segment {
            alternatives,
            plain,
            codepoint,
            nonempty_stars,
            no_traversal,
        }
    }

    /// `#parseGlob` 会置 `hasMagic` 的 atom（extglob 本身也总是魔法）。
    fn atom_has_magic(atom: &Atom) -> bool {
        matches!(
            atom,
            Atom::Star
                | Atom::StarCandidate
                | Atom::Qmark
                | Atom::Class(_)
                | Atom::Impossible
                | Atom::Ext(_)
        )
    }

    /// `toMMPattern()` 非魔法分支返回的 `unescape(src)`：转义已去掉的字符序列。
    fn unescaped_atoms(atoms: &[Atom]) -> String {
        let units: Vec<u16> = atoms
            .iter()
            .filter_map(|atom| match atom {
                Atom::Lit(unit) => Some(*unit),
                // 非魔法分支里 `\<pipe>` 的源码是裸 `|`，反转义后就是 `|`。
                Atom::Pipe => Some(u16::from(b'|')),
                _ => None,
            })
            .collect();
        String::from_utf16_lossy(&units)
    }

    /// 为每个 extglob 备选计算子 AST 的 `needNoTrav` 守卫（在 `fillNegs` 之后）。
    fn annotate_no_traversal(atoms: &mut [Atom]) {
        for atom in atoms {
            if let Atom::Ext(ext) = atom {
                for alternative in &mut ext.alternatives {
                    alternative.no_traversal = sequence_no_traversal(&alternative.atoms);
                    annotate_no_traversal(&mut alternative.atoms);
                }
            }
        }
    }

    /// 按 [`Atom::Pipe`] 切分 atom 序列（空片段保留，它们等价于正则里的空备选）。
    fn split_pipes(atoms: &[Atom]) -> Vec<Vec<Atom>> {
        let mut out: Vec<Vec<Atom>> = vec![Vec::new()];
        for atom in atoms {
            if matches!(atom, Atom::Pipe) {
                out.push(Vec::new());
            } else if let Some(last) = out.last_mut() {
                last.push(atom.clone());
            }
        }
        out
    }

    /// `toRegExpSource` 的 `needNoTrav`（`dot` 选项关闭时）：
    /// 源码以 `\.` 开头、第三个字符是 `[`/`.`，或以 `\.\.` 开头、第五个字符是 `[`/`.`。
    /// 每个 atom 渲染到正则源码时的首字符见 [`atom_source_first_char`]。
    fn no_traversal_guard(pattern: &str, atoms: &[Atom]) -> bool {
        // `dotTravAllowed`：整段正是 "." 或 ".."（`justDots`）时不加守卫。
        if pattern == "." || pattern == ".." {
            return false;
        }
        sequence_no_traversal(atoms)
    }

    /// `toRegExpSource` 的 `needNoTrav` 判定（不含 `justDots` 特判）。
    fn sequence_no_traversal(atoms: &[Atom]) -> bool {
        let is_lit_dot =
            |atom: Option<&Atom>| matches!(atom, Some(Atom::Lit(unit)) if *unit == u16::from(b'.'));
        if !is_lit_dot(atoms.first()) {
            return false;
        }
        let second = atoms.get(1);
        if let Some(character) = second.and_then(atom_source_first_char)
            && matches!(character, '[' | '.')
        {
            return true;
        }
        if is_lit_dot(second)
            && let Some(character) = atoms.get(2).and_then(atom_source_first_char)
            && matches!(character, '[' | '.')
        {
            return true;
        }
        false
    }

    /// atom 渲染到正则源码时的第一个字符。
    fn atom_source_first_char(atom: &Atom) -> Option<char> {
        match atom {
            Atom::Lit(unit) => {
                let character = char::from_u32(u32::from(*unit))?;
                // `regExpEscape` 会转义这些字符；转义后以 `\` 开头。
                let special = matches!(
                    character,
                    '-' | '['
                        | ']'
                        | '{'
                        | '}'
                        | '('
                        | ')'
                        | '*'
                        | '+'
                        | '?'
                        | '.'
                        | ','
                        | '\\'
                        | '^'
                        | '$'
                        | '|'
                        | '#'
                ) || character.is_whitespace();
                Some(if special { '\\' } else { character })
            }
            Atom::Star | Atom::StarCandidate | Atom::Qmark | Atom::Class(_) => Some('['),
            Atom::Ext(_) => Some('('),
            Atom::Pipe => Some('|'),
            Atom::Impossible => Some('$'),
        }
    }

    /// `Minimatch#parse` 的五条快捷路径，按 JS 的检查顺序匹配**原始**（未反转义）段。
    fn fast_test(part: &str) -> Option<FastTest> {
        let characters: Vec<char> = part.chars().collect();
        let excluded =
            |character: char| matches!(character, '+' | '@' | '!' | '?' | '*' | '[' | '(');
        // starRE: /^\*+$/
        if !characters.is_empty() && characters.iter().all(|character| *character == '*') {
            return Some(FastTest::Star);
        }
        // starDotExtRE: /^\*+([^+@!?*[(]*)$/
        let stars = characters
            .iter()
            .take_while(|character| **character == '*')
            .count();
        if stars > 0 {
            let rest: String = characters[stars..].iter().collect();
            if !rest.chars().any(excluded) {
                return Some(FastTest::StarDotExt {
                    ext: rest.to_lowercase(),
                });
            }
        }
        // qmarksRE: /^\?+([^+@!?*[(]*)?$/
        let qmarks = characters
            .iter()
            .take_while(|character| **character == '?')
            .count();
        if qmarks > 0 {
            let rest: String = characters[qmarks..].iter().collect();
            if !rest.chars().any(excluded) {
                let ext = if rest.is_empty() {
                    None
                } else {
                    Some(rest.to_lowercase())
                };
                return Some(FastTest::Qmarks {
                    length: part.encode_utf16().count(),
                    ext,
                });
            }
        }
        // starDotStarRE: /^\*+\.\*+$/
        if characters.first() == Some(&'*')
            && let Some(dot) = characters.iter().position(|character| *character != '*')
            && characters[dot] == '.'
            && dot + 1 < characters.len()
            && characters[dot + 1..]
                .iter()
                .all(|character| *character == '*')
        {
            return Some(FastTest::StarDotStar);
        }
        // dotStarRE: /^\.\*+$/
        if characters.first() == Some(&'.')
            && characters.len() > 1
            && characters[1..].iter().all(|character| *character == '*')
        {
            return Some(FastTest::DotStar);
        }
        None
    }

    /// `options.dot` 为 false、`nocase` 为 true 时的快捷测试。
    #[derive(Clone, Debug)]
    enum FastTest {
        /// starTest：`f.length !== 0 && !f.startsWith('.')`
        Star,
        /// starDotExtTestNocase：`!f.startsWith('.') && f.toLowerCase().endsWith(ext)`
        StarDotExt { ext: String },
        /// qmarksTestNocase：`f.length === len && !f.startsWith('.')`（+ 后缀）。
        /// `len` 是 `$0.length`，即整个模式的 UTF-16 长度。
        Qmarks { length: usize, ext: Option<String> },
        /// starDotStarTest：`!f.startsWith('.') && f.includes('.')`
        StarDotStar,
        /// dotStarTest：`f !== '.' && f !== '..' && f.startsWith('.')`
        DotStar,
    }

    impl FastTest {
        fn test(&self, file: &str) -> bool {
            match self {
                FastTest::Star => !file.is_empty() && !file.starts_with('.'),
                FastTest::StarDotExt { ext } => {
                    !file.starts_with('.') && file.to_lowercase().ends_with(ext)
                }
                FastTest::Qmarks { length, ext } => {
                    let no_ext = file.encode_utf16().count() == *length && !file.starts_with('.');
                    match ext {
                        None => no_ext,
                        Some(ext) => no_ext && file.to_lowercase().ends_with(ext),
                    }
                }
                FastTest::StarDotStar => !file.starts_with('.') && file.contains('.'),
                FastTest::DotStar => file != "." && file != ".." && file.starts_with('.'),
            }
        }
    }

    /// `partsToRegExp` 的过滤：整段只有一个 extglob（isStart && isEnd）时，空备选不参与
    /// 正则（`@(a|)` 等价于 `@(a)`）；若所有备选都为空且不是 `!`，整个 extglob 退化为字面量。
    fn postprocess_root(atoms: &mut Vec<Atom>, units: &[u16]) {
        let [Atom::Ext(ext)] = atoms.as_mut_slice() else {
            return;
        };
        let kind = ext.kind;
        if kind == ExtKind::Bang {
            // `!` 的备选总带 `(?:$|\/)` 结尾锚点，渲染结果永不为空，因此不过滤。
            return;
        }
        let had_nonempty = ext
            .alternatives
            .iter()
            .any(|alternative| !alternative.atoms.is_empty());
        ext.alternatives
            .retain(|alternative| !alternative.atoms.is_empty());
        if !had_nonempty {
            // 所有备选都为空且不是 `!`：JS 把整个 extglob 变成普通字符串（精确比较），
            // 其中的 `*`/`?`/`[` 不再有 glob 语义（`p/*()` 只匹配字面量 `p/*()`）。
            *atoms = units.iter().map(|unit| Atom::Lit(*unit)).collect();
        }
    }

    /// 计算每个序列的 `noEmpty`（`isStart && isEnd && 只含字符串片段`），并递归标注
    /// extglob 备选。返回当前序列的该项标记。
    fn annotate(sequence: &mut [Atom], is_start: bool, is_end: bool) -> bool {
        let sequence_nonempty =
            is_start && is_end && !sequence.iter().any(|atom| matches!(atom, Atom::Ext(_)));
        let length = sequence.len();
        for (index, atom) in sequence.iter_mut().enumerate() {
            let Atom::Ext(ext) = atom else {
                continue;
            };
            let ext_start = is_start && index == 0;
            let ext_end = is_end && index + 1 == length;
            let count = ext.alternatives.len();
            for (alternative_index, alternative) in ext.alternatives.iter_mut().enumerate() {
                let alternative_start = ext_start && alternative_index == 0;
                let alternative_end = if ext.kind == ExtKind::Bang {
                    true
                } else {
                    ext_end && alternative_index + 1 == count
                };
                alternative.nonempty_stars =
                    annotate(&mut alternative.atoms, alternative_start, alternative_end);
            }
        }
        sequence_nonempty
    }

    /// 段内是否有任何需要码点匹配的字符类（`posix_class_codepoint` 为真的 POSIX 类，
    /// 嵌套在 extglob 备选里也算）。它决定该段按 `u` 正则语义折叠与匹配。
    fn atoms_use_codepoints(atoms: &[Atom]) -> bool {
        atoms.iter().any(|atom| match atom {
            Atom::Class(set) => set.codepoint,
            Atom::Ext(ext) => ext
                .alternatives
                .iter()
                .any(|alternative| atoms_use_codepoints(&alternative.atoms)),
            _ => false,
        })
    }

    /// `#fillNegs`：把 `!` 组之后的所有模式片段并进它的每个备选，使取反覆盖剩余模式。
    fn fill_negs(sequence: &mut [Atom], enclosing_tail: &[Atom]) {
        for index in 0..sequence.len() {
            let suffix: Vec<Atom> = sequence[index + 1..]
                .iter()
                .cloned()
                .chain(enclosing_tail.iter().cloned())
                .collect();
            if let Atom::Ext(ext) = &mut sequence[index] {
                for alternative in &mut ext.alternatives {
                    fill_negs(&mut alternative.atoms, &suffix);
                }
                if ext.kind == ExtKind::Bang && !ext.empty {
                    for alternative in &mut ext.alternatives {
                        alternative.atoms.extend(suffix.iter().cloned());
                    }
                }
            }
        }
    }

    /// `#parseAST` 的根序列：字符串片段转成 glob atom，extglob 递归解析。
    fn parse_glob_sequence(units: &[u16]) -> Vec<Atom> {
        let mut out = Vec::new();
        let mut accumulated: Vec<u16> = Vec::new();
        let mut index = 0;
        let mut escaping = false;
        let mut in_brace = false;
        let mut brace_start = 0;
        let mut brace_negated = false;
        while index < units.len() {
            let unit = units[index];
            index += 1;
            if escaping || unit == u16::from(b'\\') {
                escaping = !escaping;
                accumulated.push(unit);
                continue;
            }
            if in_brace {
                if index == brace_start + 1 {
                    if unit == u16::from(b'^') || unit == u16::from(b'!') {
                        brace_negated = true;
                    }
                } else if unit == u16::from(b']') && !(index == brace_start + 2 && brace_negated) {
                    in_brace = false;
                }
                accumulated.push(unit);
                continue;
            }
            if unit == u16::from(b'[') {
                in_brace = true;
                brace_start = index;
                brace_negated = false;
                accumulated.push(unit);
                continue;
            }
            if let Some(kind) = ext_kind(unit)
                && units.get(index) == Some(&u16::from(b'('))
            {
                push_string(&mut out, &accumulated);
                accumulated.clear();
                match parse_ext(units, index + 1, kind, index - 1) {
                    ParsedExt::Ext(ext, end) => {
                        out.push(Atom::Ext(ext));
                        index = end;
                    }
                    ParsedExt::Unfinished(text) => {
                        out.extend(parse_glob_atoms(&text));
                        index = units.len();
                    }
                }
                continue;
            }
            accumulated.push(unit);
        }
        push_string(&mut out, &accumulated);
        out
    }

    enum ParsedExt {
        Ext(Ext, usize),
        Unfinished(Vec<u16>),
    }

    /// 从 `(` 之后开始解析一个 extglob。
    fn parse_ext(units: &[u16], start: usize, kind: ExtKind, type_index: usize) -> ParsedExt {
        let mut alternatives: Vec<Alternative> = Vec::new();
        let mut current: Vec<Atom> = Vec::new();
        let mut accumulated: Vec<u16> = Vec::new();
        let mut escaping = false;
        let mut in_brace = false;
        let mut brace_start = 0;
        let mut brace_negated = false;
        let mut empty = false;
        let mut index = start;
        while index < units.len() {
            let unit = units[index];
            index += 1;
            if escaping || unit == u16::from(b'\\') {
                escaping = !escaping;
                accumulated.push(unit);
                continue;
            }
            if in_brace {
                if index == brace_start + 1 {
                    if unit == u16::from(b'^') || unit == u16::from(b'!') {
                        brace_negated = true;
                    }
                } else if unit == u16::from(b']') && !(index == brace_start + 2 && brace_negated) {
                    in_brace = false;
                }
                accumulated.push(unit);
                continue;
            }
            if unit == u16::from(b'[') {
                in_brace = true;
                brace_start = index;
                brace_negated = false;
                accumulated.push(unit);
                continue;
            }
            if let Some(nested) = ext_kind(unit)
                && units.get(index) == Some(&u16::from(b'('))
            {
                push_string(&mut current, &accumulated);
                accumulated.clear();
                match parse_ext(units, index + 1, nested, index - 1) {
                    ParsedExt::Ext(ext, end) => {
                        current.push(Atom::Ext(ext));
                        index = end;
                    }
                    ParsedExt::Unfinished(text) => {
                        current.extend(parse_glob_atoms(&text));
                        index = units.len();
                    }
                }
                continue;
            }
            if unit == u16::from(b'|') {
                push_string(&mut current, &accumulated);
                accumulated.clear();
                alternatives.push(Alternative::new(std::mem::take(&mut current)));
                continue;
            }
            if unit == u16::from(b')') {
                // `#emptyExt`：JS 在最终 push 之前检查 `acc === '' && ast.#parts.length === 0`，
                // 而 ast.#parts 此刻恒为空，所以它只表示「最后一个备选是空串」。
                if accumulated.is_empty() {
                    empty = true;
                }
                push_string(&mut current, &accumulated);
                alternatives.push(Alternative::new(current));
                return ParsedExt::Ext(
                    Ext {
                        kind,
                        alternatives,
                        empty,
                    },
                    index,
                );
            }
            accumulated.push(unit);
        }
        // 未闭合的 extglob 整体按字面量重新解析（JS 把 type 置空、parts 设为原文）。
        ParsedExt::Unfinished(units[type_index..].to_vec())
    }

    fn push_string(out: &mut Vec<Atom>, units: &[u16]) {
        if units.is_empty() {
            return;
        }
        out.extend(parse_glob_atoms(units));
    }

    /// `#parseGlob`：把一段普通 glob 文本转成 atom。
    fn parse_glob_atoms(units: &[u16]) -> Vec<Atom> {
        let mut out = Vec::new();
        let mut escaping = false;
        let mut in_star = false;
        let mut index = 0;
        // `#parseGlob` 的 `noEmpty && /^[*]+$/`：整个字符串片段都是 `*`。
        let all_stars = !units.is_empty() && units.iter().all(|unit| *unit == u16::from(b'*'));
        while index < units.len() {
            let unit = units[index];
            if escaping {
                escaping = false;
                // `reSpecials` 不含 `|`：`\|` 去掉反斜杠后是正则交替而不是字面量。
                // 其余转义字符注入正则后仍只表示自身。
                if unit == u16::from(b'|') {
                    out.push(Atom::Pipe);
                } else {
                    out.push(Atom::Lit(unit));
                }
                index += 1;
                continue;
            }
            if unit == u16::from(b'*') {
                if !in_star {
                    out.push(if all_stars {
                        Atom::StarCandidate
                    } else {
                        Atom::Star
                    });
                    in_star = true;
                }
                index += 1;
                continue;
            }
            in_star = false;
            if unit == u16::from(b'\\') {
                if index + 1 == units.len() {
                    out.push(Atom::Lit(unit));
                } else {
                    escaping = true;
                }
                index += 1;
                continue;
            }
            if unit == u16::from(b'[')
                && let Some((class, consumed)) = parse_class(units, index)
            {
                out.push(class);
                index += consumed;
                continue;
            }
            if unit == u16::from(b'?') {
                out.push(Atom::Qmark);
            } else {
                out.push(Atom::Lit(unit));
            }
            index += 1;
        }
        out
    }

    /// POSIX 类名 → 内部表示。`[:graph:]` 在正则里是 `\P{Z}` 与 `\P{C}` 的组合，
    /// 用一个取反类 `Neg(ZOrC)` 表示，判定见 `class_matches`。
    static POSIX_CLASSES: &[(&str, PosixSpec)] = &[
        ("[:alnum:]", PosixSpec::Class(PosixClass::Alnum)),
        ("[:alpha:]", PosixSpec::Class(PosixClass::Alpha)),
        ("[:ascii:]", PosixSpec::Class(PosixClass::Ascii)),
        ("[:blank:]", PosixSpec::Class(PosixClass::Blank)),
        ("[:cntrl:]", PosixSpec::Class(PosixClass::Cntrl)),
        ("[:digit:]", PosixSpec::Class(PosixClass::Digit)),
        ("[:graph:]", PosixSpec::Neg(PosixNeg::ZOrC)),
        ("[:lower:]", PosixSpec::Class(PosixClass::Lower)),
        ("[:print:]", PosixSpec::Class(PosixClass::Print)),
        ("[:punct:]", PosixSpec::Class(PosixClass::Punct)),
        ("[:space:]", PosixSpec::Class(PosixClass::Space)),
        ("[:upper:]", PosixSpec::Class(PosixClass::Upper)),
        ("[:word:]", PosixSpec::Class(PosixClass::Word)),
        ("[:xdigit:]", PosixSpec::Class(PosixClass::Xdigit)),
    ];

    enum PosixSpec {
        Class(PosixClass),
        Neg(PosixNeg),
    }

    /// 该类是否要求 `u`（码点）语义：`[:ascii:]` / `[:xdigit:]` 的成员全在 ASCII 内，
    /// 按 UTF-16 码元匹配与按码点匹配等价，因此不置位。
    fn posix_class_codepoint(class: PosixClass) -> bool {
        !matches!(class, PosixClass::Ascii | PosixClass::Xdigit)
    }

    /// `parseClass()`：返回（atom, 消费的 UTF-16 单元数）；`None` 表示未闭合（按字面量处理）。
    ///
    /// 特殊分支：
    /// - 首字符 `!` / `^` → 取反；
    /// - `[:name:]` 出现在范围起点之后 → 参考实现视为非法（`Impossible`）；
    /// - 没有任何成员的空类 → `Impossible`（永远不匹配）；
    /// - 单个字面量且非取反 → 退化成 `Atom::Lit`，不必走字符类匹配；
    /// - 末尾 `x-]` 的 `-` 按字面量处理（JS `braceEscape` 语义）。
    fn parse_class(units: &[u16], position: usize) -> Option<(Atom, usize)> {
        let mut ranges: Vec<ClassMember> = Vec::new();
        let mut negs: Vec<PosixNeg> = Vec::new();
        let mut saw_start = false;
        let mut codepoint = false;
        let mut escaping = false;
        let mut negate = false;
        let mut end_position = position;
        let mut range_start: Option<u16> = None;
        let mut index = position + 1;
        while index < units.len() {
            let unit = units[index];
            if (unit == u16::from(b'!') || unit == u16::from(b'^')) && index == position + 1 {
                negate = true;
                index += 1;
                continue;
            }
            if unit == u16::from(b']') && saw_start && !escaping {
                end_position = index + 1;
                break;
            }
            saw_start = true;
            if unit == u16::from(b'\\') && !escaping {
                escaping = true;
                index += 1;
                continue;
            }
            if unit == u16::from(b'[') && !escaping {
                let mut matched = false;
                for (name, spec) in POSIX_CLASSES {
                    let name_units: Vec<u16> = name.encode_utf16().collect();
                    if units[index..].starts_with(&name_units) {
                        if range_start.is_some() {
                            return Some((Atom::Impossible, units.len() - position));
                        }
                        index += name_units.len();
                        match spec {
                            PosixSpec::Class(class) => {
                                ranges.push(ClassMember::Posix(*class));
                                codepoint |= posix_class_codepoint(*class);
                            }
                            PosixSpec::Neg(neg) => {
                                negs.push(*neg);
                                codepoint = true;
                            }
                        }
                        matched = true;
                        break;
                    }
                }
                if matched {
                    continue;
                }
            }
            escaping = false;
            if let Some(start) = range_start {
                let start_value = u32::from(start);
                let value = u32::from(unit);
                if value > start_value {
                    ranges.push(ClassMember::Range(start_value, value));
                } else if value == start_value {
                    ranges.push(ClassMember::Range(value, value));
                }
                range_start = None;
                index += 1;
                continue;
            }
            if units.get(index + 1) == Some(&u16::from(b'-'))
                && units.get(index + 2) == Some(&u16::from(b']'))
            {
                // JS `braceEscape(c + '-')`：`-` 在这里是字面量，不是范围。
                ranges.push(ClassMember::Range(u32::from(unit), u32::from(unit)));
                ranges.push(ClassMember::Range(u32::from(b'-'), u32::from(b'-')));
                index += 2;
                continue;
            }
            if units.get(index + 1) == Some(&u16::from(b'-')) {
                range_start = Some(unit);
                index += 2;
                continue;
            }
            ranges.push(ClassMember::Range(u32::from(unit), u32::from(unit)));
            index += 1;
        }
        if end_position < index {
            return None;
        }
        if ranges.is_empty() && negs.is_empty() {
            return Some((Atom::Impossible, units.len() - position));
        }
        if negs.is_empty()
            && !negate
            && let [ClassMember::Range(low, high)] = ranges.as_slice()
            && low == high
        {
            return Some((Atom::Lit(*low as u16), end_position - position));
        }
        Some((
            Atom::Class(ClassSet {
                ranges,
                negs,
                negate,
                codepoint,
            }),
            end_position - position,
        ))
    }

    // -----------------------------------------------------------------------
    // 匹配
    // -----------------------------------------------------------------------

    /// dot 规则：`.`、`..` 与以 `.` 开头的段不能被 `**` / 通配符吞掉。
    fn is_bad_dot(segment: &str) -> bool {
        segment == "." || segment == ".." || segment.starts_with('.')
    }

    /// `#matchOne`；带 `**` 时先走 `#matchGlobstar`（等价于 JS `matchOne` 的分派）。
    fn match_one(
        file: &[String],
        pattern: &[Part],
        file_start: usize,
        pattern_start: usize,
    ) -> bool {
        if pattern[pattern_start..]
            .iter()
            .any(|part| matches!(part, Part::Globstar))
        {
            return match_globstar(file, pattern, file_start, pattern_start);
        }
        let mut file_index = file_start;
        let mut pattern_index = pattern_start;
        while file_index < file.len() && pattern_index < pattern.len() {
            let part = &pattern[pattern_index];
            let segment = &file[file_index];
            let hit = match part {
                Part::Globstar => return false,
                Part::Empty => segment.is_empty(),
                Part::Fast(test) => test.test(segment),
                Part::Segment(segment_pattern) => match_segment(segment_pattern, segment),
            };
            if !hit {
                return false;
            }
            file_index += 1;
            pattern_index += 1;
        }
        // 模式耗尽时还允许路径多出一个**空**段（结尾的 `/`），其余多余段都不匹配。
        if file_index == file.len() && pattern_index == pattern.len() {
            true
        } else if file_index == file.len() {
            false
        } else {
            file_index == file.len() - 1 && file[file_index].is_empty()
        }
    }

    /// 单个路径段的匹配。`plain` 段走精确字符串比较；否则把候选折叠成 UTF-16 码元
    /// 序列，按 `\|` 切出的多个正则备选分别尝试：第一项锚定开头、最后一项锚定结尾、
    /// 中间项可在任意位置（复刻 `^A|B|C$`）。
    fn match_segment(segment_pattern: &Segment, segment: &str) -> bool {
        // 非魔法段：`f === p` 精确比较（反转义后的字面量，大小写敏感）。
        if let Some(plain) = &segment_pattern.plain {
            return segment == plain;
        }
        // `nocase` 在匹配阶段折叠；非 `u` / `u` 模式的折叠规则见 [`FoldMode`]。
        let units = fold_string(segment, segment_pattern.fold_mode());
        let context = MatchContext {
            units: &units,
            codepoint: segment_pattern.codepoint,
            fold: segment_pattern.fold_mode(),
        };
        // `(?!(?:^|/)\.\.?(?:$|/))`：段内不含 `/`，等价于整段不是 "." / ".."。
        let guard_ok = !segment_pattern.no_traversal
            || !(units == [u16::from(b'.')] || units == [u16::from(b'.'), u16::from(b'.')]);
        let count = segment_pattern.alternatives.len();
        if count <= 1 {
            if !guard_ok {
                return false;
            }
            let atoms = segment_pattern
                .alternatives
                .first()
                .map_or(&[][..], Vec::as_slice);
            let start = BTreeSet::from([0usize]);
            let ends = match_atoms(
                atoms,
                &context,
                &start,
                true,
                segment_pattern.nonempty_stars,
            );
            return ends.contains(&context.units.len());
        }
        // 根段的源码是 `^A|B|C$`：第一项只锚定开头、最后一项只锚定结尾、中间项任意位置；
        // 开头的 dot 守卫只作用于第一项（它拼在 `^` 之后、第一个 `|` 之前）。
        for (index, atoms) in segment_pattern.alternatives.iter().enumerate() {
            if index == 0 {
                if !guard_ok {
                    continue;
                }
                let start = BTreeSet::from([0usize]);
                let ends = match_atoms(
                    atoms,
                    &context,
                    &start,
                    true,
                    segment_pattern.nonempty_stars,
                );
                if !ends.is_empty() {
                    return true;
                }
                continue;
            }
            let last = index + 1 == count;
            for position in 0..=context.units.len() {
                let start = BTreeSet::from([position]);
                let ends = match_atoms(
                    atoms,
                    &context,
                    &start,
                    false,
                    segment_pattern.nonempty_stars,
                );
                if if last {
                    ends.contains(&context.units.len())
                } else {
                    !ends.is_empty()
                } {
                    return true;
                }
            }
        }
        false
    }

    /// 已按段模式折叠过大小写的候选串，外加匹配单位：`codepoint` 为真时按码点推进
    /// （对应 JS 正则的 `u` 标志），否则按 UTF-16 码元。
    struct MatchContext<'a> {
        units: &'a [u16],
        codepoint: bool,
        fold: FoldMode,
    }

    impl MatchContext<'_> {
        /// 当前位置的匹配宽度：`u` 模式下一个合法的代理对算 2，其余算 1 个码元。
        fn width_at(&self, position: usize) -> usize {
            if !self.codepoint {
                return 1;
            }
            if position + 1 < self.units.len()
                && (0xd800..=0xdbff).contains(&self.units[position])
                && (0xdc00..=0xdfff).contains(&self.units[position + 1])
            {
                2
            } else {
                1
            }
        }

        /// 当前位置的码点：非 `u` 模式或孤立代理项时就是该码元值。
        fn code_point_at(&self, position: usize) -> u32 {
            let unit = u32::from(self.units[position]);
            if self.codepoint && self.width_at(position) == 2 {
                let low = u32::from(self.units[position + 1]);
                return 0x10000 + ((unit - 0xd800) << 10) + (low - 0xdc00);
            }
            unit
        }

        /// 从 `position` 起所有可停下的边界（含起点），供 `*` / `!` 表示「任意长度」。
        fn positions_after(&self, position: usize) -> Vec<usize> {
            let mut out = Vec::new();
            let mut current = position;
            out.push(current);
            while current < self.units.len() {
                current += self.width_at(current);
                out.push(current);
            }
            out
        }
    }

    /// 用「可达位置集合」模拟正则的 NFA：按顺序对每个 atom 推进集合，返回所有可停下
    /// 的位置。atom 的顺序即连接（concatenation）；`Star` 等会把一个位置扩展成多个。
    /// `start_context` 为真且当前 atom 在段首时，魔法 atom 要应用 `(?!\.)` 守卫
    /// （dot 选项关闭）。
    fn match_atoms(
        atoms: &[Atom],
        context: &MatchContext<'_>,
        positions: &BTreeSet<usize>,
        start_context: bool,
        nonempty_stars: bool,
    ) -> BTreeSet<usize> {
        let mut current: BTreeSet<usize> = positions.clone();
        for (index, atom) in atoms.iter().enumerate() {
            if current.is_empty() {
                return current;
            }
            let is_start = start_context && index == 0;
            // minimatch 的 `(?!\.)`：结构上位于段首的 `*` / `?` / 字符类不能以 `.` 开头
            // （dot 选项关闭）。字面量 `.` 不受影响。
            let start_guard = is_start
                && matches!(
                    atom,
                    Atom::Star | Atom::StarCandidate | Atom::Qmark | Atom::Class(_)
                );
            let mut next = BTreeSet::new();
            for position in &current {
                let position = *position;
                if start_guard && context.units.get(position) == Some(&u16::from(b'.')) {
                    continue;
                }
                match atom {
                    Atom::Lit(unit) => {
                        if context.units.get(position) == Some(unit) {
                            next.insert(position + 1);
                        }
                    }
                    Atom::Star => {
                        next.extend(context.positions_after(position));
                    }
                    Atom::StarCandidate => {
                        let positions = context.positions_after(position);
                        if nonempty_stars {
                            // `[^/]+?`：至少消费一个字符/码点。
                            next.extend(
                                positions
                                    .into_iter()
                                    .filter(|candidate| *candidate > position),
                            );
                        } else {
                            next.extend(positions);
                        }
                    }
                    Atom::Qmark => {
                        if position < context.units.len() {
                            next.insert(position + context.width_at(position));
                        }
                    }
                    Atom::Impossible => {}
                    // 转义 `\|` 已在 `split_pipes` 处切成备选，不应作为字面量参与匹配。
                    Atom::Pipe => {}
                    Atom::Class(class) => {
                        if position < context.units.len() && class_matches(class, context, position)
                        {
                            next.insert(position + context.width_at(position));
                        }
                    }
                    Atom::Ext(ext) => {
                        next.extend(match_ext(ext, context, position, is_start));
                    }
                }
            }
            current = next;
        }
        current
    }

    /// 五种 extglob 的匹配，返回所有可能的结束位置集合：`@` 恰好一次；`?` 零或一次；
    /// `*` 零或多次；`+` 一次或多次。`!` 是负向断言：任一备选（`fill_negs` 已把后续
    /// 模式拼进备选）命中则整体失败，否则可停在任意位置；若最后一个备选为空
    /// （`ext.empty`）则至少消费一个码元。`*` / `+` 用 frontier 逐层扩展，已出现过的
    /// 结束位置不再入队，位置有界，因此循环必然终止。
    fn match_ext(
        ext: &Ext,
        context: &MatchContext<'_>,
        position: usize,
        is_start: bool,
    ) -> BTreeSet<usize> {
        match ext.kind {
            ExtKind::At => match_alternatives(ext, context, position, is_start),
            ExtKind::Question => {
                let mut out = BTreeSet::new();
                out.insert(position);
                out.extend(match_alternatives(ext, context, position, is_start));
                out
            }
            ExtKind::Star | ExtKind::Plus => {
                let mut out = BTreeSet::new();
                let mut frontier = BTreeSet::new();
                if ext.kind == ExtKind::Star {
                    out.insert(position);
                }
                frontier.insert(position);
                let mut first = true;
                while !frontier.is_empty() {
                    let mut next_frontier = BTreeSet::new();
                    for from in &frontier {
                        let matched = match_alternatives(ext, context, *from, is_start && first);
                        for end in matched {
                            if out.insert(end) {
                                next_frontier.insert(end);
                            }
                        }
                    }
                    frontier = next_frontier;
                    first = false;
                }
                out
            }
            ExtKind::Bang => {
                if is_start
                    && position < context.units.len()
                    && context.units[position] == u16::from(b'.')
                {
                    return BTreeSet::new();
                }
                if ext.empty {
                    return context
                        .positions_after(position)
                        .into_iter()
                        .filter(|candidate| *candidate > position)
                        .collect();
                }
                let mut start = BTreeSet::new();
                start.insert(position);
                for alternative in &ext.alternatives {
                    let pieces = split_pipes(&alternative.atoms);
                    let last = pieces.len().saturating_sub(1);
                    let mut hit = false;
                    for (index, piece) in pieces.iter().enumerate() {
                        let ends =
                            match_atoms(piece, context, &start, false, alternative.nonempty_stars);
                        // 末尾片段的 `(?:$|\/)` 锚点只看最后一项；其余分支是前缀匹配。
                        if if index == last {
                            ends.contains(&context.units.len())
                        } else {
                            !ends.is_empty()
                        } {
                            hit = true;
                            break;
                        }
                    }
                    if hit {
                        return BTreeSet::new();
                    }
                }
                context.positions_after(position).into_iter().collect()
            }
        }
    }

    /// 依次尝试 extglob 的每个备选并合并结束位置。备选内部的 `\|` 先切成多个正则
    /// 片段；段首的 dot / 遍历守卫只作用于**第一个片段**的第一项（与 `^A|B$` 中 `^`
    /// 只锚定 A 一致），后续片段是前缀匹配，完全不带这些守卫。
    fn match_alternatives(
        ext: &Ext,
        context: &MatchContext<'_>,
        position: usize,
        start_context: bool,
    ) -> BTreeSet<usize> {
        let start = BTreeSet::from([position]);
        let mut out = BTreeSet::new();
        for alternative in &ext.alternatives {
            // 备选里的转义 `\|` 是正则交替；它拼在同一个子 AST 里，因此段首的
            // `(?!\.)` / 遍历守卫只作用于第一项。
            let pieces = split_pipes(&alternative.atoms);
            for (index, piece) in pieces.iter().enumerate() {
                if index == 0
                    && start_context
                    && alternative.no_traversal
                    && guard_rejects_traversal(context, position)
                {
                    continue;
                }
                out.extend(match_atoms(
                    piece,
                    context,
                    &start,
                    start_context && index == 0,
                    alternative.nonempty_stars,
                ));
            }
        }
        out
    }

    /// `(?!(?:^|/)\.\.?(?:$|/))` 在单个路径段、位置 `position` 处的等价判定：
    /// `^` 只匹配串首，段内没有 `/`，所以只有位置 0 且整段为 "." / ".." 时拒绝。
    fn guard_rejects_traversal(context: &MatchContext<'_>, position: usize) -> bool {
        position == 0
            && (context.units == [u16::from(b'.')]
                || context.units == [u16::from(b'.'), u16::from(b'.')])
    }

    // -----------------------------------------------------------------------
    // 字符类
    // -----------------------------------------------------------------------

    /// 字符类成员判定：`ranges` 是正成员，`negs` 是取反类，四个分支按两者是否为空
    /// 分别把 `negate` 应用到不同组合上。非 `u` 模式的范围匹配见
    /// `canonical_range_contains`。
    fn class_matches(class: &ClassSet, context: &MatchContext<'_>, position: usize) -> bool {
        let code_point = context.code_point_at(position);
        let in_ranges = class.ranges.iter().any(|member| match member {
            ClassMember::Range(low, high) => match context.fold {
                FoldMode::Simple => (*low..=*high).contains(&code_point),
                FoldMode::Canonical => canonical_range_contains(*low, *high, code_point),
            },
            ClassMember::Posix(posix) => posix_matches(*posix, code_point),
        });
        let in_negs = class.negs.iter().any(|neg| match neg {
            PosixNeg::ZOrC => {
                in_category(code_point, Category::Separator)
                    || in_category(code_point, Category::Other)
            }
        });
        match (class.ranges.is_empty(), class.negs.is_empty()) {
            (false, false) => {
                if class.negate {
                    !in_ranges || in_negs
                } else {
                    in_ranges || !in_negs
                }
            }
            (false, true) => in_ranges != class.negate,
            (true, false) => in_negs == class.negate,
            (true, true) => false,
        }
    }

    /// POSIX 类 → Unicode 类别判定。`Lower` / `Upper` 因调用点固定 `nocase`，按
    /// 「有大小写映射的字母」闭包判断（见下方内联注释）。
    fn posix_matches(class: PosixClass, code_point: u32) -> bool {
        match class {
            PosixClass::Ascii => code_point <= 0x7f,
            PosixClass::Xdigit => {
                matches!(code_point, 0x30..=0x39 | 0x41..=0x46 | 0x61..=0x66)
            }
            PosixClass::Alnum => in_any(
                code_point,
                &[
                    Category::Letter,
                    Category::LetterNumber,
                    Category::DecimalNumber,
                ],
            ),
            PosixClass::Alpha => in_any(code_point, &[Category::Letter, Category::LetterNumber]),
            PosixClass::Blank => {
                in_category(code_point, Category::SpaceSeparator) || code_point == 0x09
            }
            PosixClass::Cntrl => in_category(code_point, Category::Control),
            PosixClass::Digit => in_category(code_point, Category::DecimalNumber),
            // regex 的 `i` 标志会做大小写闭包：`\p{Ll}`/`\p{Lu}` 都匹配所有有大小写
            // 映射的字母（Lu/Ll/Lt）。调用点固定 nocase，因此这里直接按闭包判断。
            PosixClass::Lower | PosixClass::Upper => in_any(
                code_point,
                &[
                    Category::Lowercase,
                    Category::Uppercase,
                    Category::Titlecase,
                ],
            ),
            PosixClass::Print => in_category(code_point, Category::Other),
            PosixClass::Punct => in_category(code_point, Category::Punctuation),
            PosixClass::Space => {
                in_category(code_point, Category::Separator) || matches!(code_point, 0x09..=0x0d)
            }
            PosixClass::Word => in_any(
                code_point,
                &[
                    Category::Letter,
                    Category::LetterNumber,
                    Category::DecimalNumber,
                    Category::ConnectorPunctuation,
                ],
            ),
        }
    }

    /// 任一类别命中。
    fn in_any(code_point: u32, categories: &[Category]) -> bool {
        categories
            .iter()
            .any(|category| in_category(code_point, *category))
    }

    /// 类别 → 预编译的 `\p{..}` 正则（每个类别只编译一次）。
    fn category_regex(category: Category) -> &'static Regex {
        static LETTER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\p{L}").expect("regex"));
        static LETTER_NUMBER: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"\p{Nl}").expect("regex"));
        static DECIMAL_NUMBER: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"\p{Nd}").expect("regex"));
        static SEPARATOR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\p{Z}").expect("regex"));
        static SPACE_SEPARATOR: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"\p{Zs}").expect("regex"));
        static CONTROL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\p{Cc}").expect("regex"));
        static LOWERCASE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\p{Ll}").expect("regex"));
        static UPPERCASE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\p{Lu}").expect("regex"));
        static TITLECASE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\p{Lt}").expect("regex"));
        static PUNCTUATION: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"\p{P}").expect("regex"));
        static CONNECTOR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\p{Pc}").expect("regex"));
        static OTHER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\p{C}").expect("regex"));
        match category {
            Category::Letter => &LETTER,
            Category::LetterNumber => &LETTER_NUMBER,
            Category::DecimalNumber => &DECIMAL_NUMBER,
            Category::Separator => &SEPARATOR,
            Category::SpaceSeparator => &SPACE_SEPARATOR,
            Category::Control => &CONTROL,
            Category::Lowercase => &LOWERCASE,
            Category::Uppercase => &UPPERCASE,
            Category::Titlecase => &TITLECASE,
            Category::Punctuation => &PUNCTUATION,
            Category::ConnectorPunctuation => &CONNECTOR,
            Category::Other => &OTHER,
        }
    }

    /// 用正则判定单个码点的 Unicode 类别；孤立代理项按 `Cs`（归入 `Other`）处理，
    /// 与 JS 的 `/u` 字符类一致。
    fn in_category(code_point: u32, category: Category) -> bool {
        if (0xd800..=0xdfff).contains(&code_point) {
            // JS 的 `/u` 字符类把未配对代理项归为 Cs（`\p{C}`），其余类别不匹配。
            return category == Category::Other;
        }
        let Some(character) = char::from_u32(code_point) else {
            return false;
        };
        let mut buffer = [0u8; 4];
        category_regex(category).is_match(character.encode_utf8(&mut buffer))
    }

    // -----------------------------------------------------------------------
    // globstar
    // -----------------------------------------------------------------------

    /// `#matchGlobstar`：处理含 `**` 的模式。把模式在第一个/最后一个 `**` 处切成
    /// head、body、tail：
    ///
    /// 1. head 必须从当前位置逐段匹配；
    /// 2. tail 必须匹配到路径末尾（允许末尾多一个空段，对应结尾的 `/`）；
    /// 3. body 里的 `**` 再把剩余区间切段，交给 `match_globstar_body_sections`
    ///    递归分配；
    /// 4. 被 `**` 吞掉的每一段都不能是 `.` / `..` / 以 `.` 开头（dot 规则）。
    fn match_globstar(
        file: &[String],
        pattern: &[Part],
        file_index: usize,
        pattern_index: usize,
    ) -> bool {
        let first_globstar = pattern[pattern_index..]
            .iter()
            .position(|part| matches!(part, Part::Globstar))
            .map(|offset| offset + pattern_index)
            .expect("caller checked");
        let last_globstar = pattern
            .iter()
            .rposition(|part| matches!(part, Part::Globstar))
            .expect("caller checked");
        let head = &pattern[pattern_index..first_globstar];
        // JS `slice(firstgs + 1, lastgs)`：起点不早于终点时得到空数组。
        let body: &[Part] = if first_globstar >= last_globstar {
            &[]
        } else {
            &pattern[first_globstar + 1..last_globstar]
        };
        let tail = &pattern[last_globstar + 1..];

        let mut file_index = file_index;
        if !head.is_empty() {
            let file_head: Vec<String> = file
                .iter()
                .skip(file_index)
                .take(head.len())
                .cloned()
                .collect();
            if !match_one(&file_head, head, 0, 0) {
                return false;
            }
            file_index += head.len();
        }

        let mut file_tail_match = 0usize;
        if !tail.is_empty() {
            if tail.len() + file_index > file.len() {
                return false;
            }
            let mut tail_start = file.len() - tail.len();
            if match_one(file, tail, tail_start, 0) {
                file_tail_match = tail.len();
            } else {
                if file.last().is_none_or(|segment| !segment.is_empty())
                    || file_index + tail.len() == file.len()
                {
                    return false;
                }
                tail_start -= 1;
                if !match_one(file, tail, tail_start, 0) {
                    return false;
                }
                file_tail_match = tail.len() + 1;
            }
        }

        let file_end = file.len() - file_tail_match;
        if body.is_empty() {
            let mut saw_some = file_tail_match > 0;
            for segment in &file[file_index..file_end] {
                saw_some = true;
                if is_bad_dot(segment) {
                    return false;
                }
            }
            return saw_some;
        }

        let mut body_segments: Vec<(Vec<Part>, usize)> = vec![(Vec::new(), 0)];
        let mut non_globstar_parts = 0usize;
        let mut non_globstar_sums = vec![0usize];
        for part in body {
            if matches!(part, Part::Globstar) {
                non_globstar_sums.push(non_globstar_parts);
                body_segments.push((Vec::new(), 0));
            } else {
                if let Some(last) = body_segments.last_mut() {
                    last.0.push(part.clone());
                }
                non_globstar_parts += 1;
            }
        }
        let mut sum_index = body_segments.len() - 1;
        for section in &mut body_segments {
            section.1 = file_end.saturating_sub(non_globstar_sums[sum_index] + section.0.len());
            sum_index = sum_index.saturating_sub(1);
        }
        match_globstar_body_sections(file, &body_segments, file_index, 0, 0, file_tail_match > 0)
    }

    /// 把 body 的每个「非 `**` 段组」按顺序分配给路径段：每段组必须从分配到的位置
    /// 开始匹配，`**` 可吞掉零到多段但不能吞 dot 段。`sections[i].1` 是该段组最晚的
    /// 起点（由剩余非 globstar 段数反推）。递归深度超过 `MAX_GLOBSTAR_RECURSION` 时
    /// 不再展开（防病态模式）。
    fn match_globstar_body_sections(
        file: &[String],
        sections: &[(Vec<Part>, usize)],
        file_index: usize,
        section_index: usize,
        depth: usize,
        mut saw_tail: bool,
    ) -> bool {
        let Some((section, after)) = sections.get(section_index) else {
            for segment in &file[file_index..] {
                saw_tail = true;
                if is_bad_dot(segment) {
                    return false;
                }
            }
            return saw_tail;
        };
        let mut file_index = file_index;
        while file_index <= *after {
            let file_slice: Vec<String> = file
                .iter()
                .take(file_index + section.len())
                .cloned()
                .collect();
            let matched = match_one(&file_slice, section, file_index, 0);
            if matched && depth < MAX_GLOBSTAR_RECURSION {
                let sub = match_globstar_body_sections(
                    file,
                    sections,
                    file_index + section.len(),
                    section_index + 1,
                    depth + 1,
                    saw_tail,
                );
                if sub {
                    return true;
                }
            }
            let Some(segment) = file.get(file_index) else {
                break;
            };
            if is_bad_dot(segment) {
                return false;
            }
            file_index += 1;
        }
        false
    }

    // -----------------------------------------------------------------------
    // 测试（黄金夹具在 `tests/fixtures/resolver/minimatch_cases.json`）
    // -----------------------------------------------------------------------

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn brace_expansions_match_the_reference() {
            assert_eq!(brace_expand("a{b,c}d"), vec!["abd", "acd"]);
            assert_eq!(brace_expand("a{b,}c"), vec!["abc", "ac"]);
            assert_eq!(brace_expand("a{0..2}d"), vec!["a0d", "a1d", "a2d"]);
            assert_eq!(brace_expand("a{b}c"), vec!["a{b}c"]);
            assert_eq!(brace_expand("p/{}*"), vec!["p/{}*"]);
            assert_eq!(brace_expand("x{{a,b}}y"), vec!["x{a}y", "x{b}y"]);
        }

        #[test]
        fn golden_cases_match_minimatch() {
            // 夹具由 TS 参考实现（minimatch@10.2.6）生成，见
            // `tests/fixtures/resolver/minimatch_cases.json`。
            let fixture: serde_json::Value = serde_json::from_str(include_str!(
                "../../tests/fixtures/resolver/minimatch_cases.json"
            ))
            .expect("minimatch fixture");
            let cases = fixture["cases"].as_array().expect("cases");
            let mut mismatches = 0usize;
            let mut failures = Vec::new();
            for case in cases {
                let candidate = case[0].as_str().expect("candidate");
                let pattern = case[1].as_str().expect("pattern");
                let expected = case[2].as_bool().expect("expected");
                let actual = minimatch(candidate, pattern);
                if actual != expected {
                    mismatches += 1;
                    if failures.len() < 40 {
                        failures.push(format!(
                            "minimatch({candidate:?}, {pattern:?}) = {actual}, expected {expected}"
                        ));
                    }
                }
            }
            assert!(
                mismatches == 0,
                "{mismatches} mismatches:\n{}",
                failures.join("\n")
            );
        }

        #[test]
        fn edge_cases_match_minimatch() {
            // 审查修复的回归夹具（minimatch@10.2.6）：
            // - `parse()` 的快捷路径覆盖 AST（`*\\b` / `*\\.json`，以及 `\|` 被注入
            //   正则后形成的交替：`\|*` / `*\|*`）；
            // - dot 遍历守卫（"." / ".." 不被 `.*`、`[.]*`、`.?`、`[.].*` 等匹配）；
            // - ECMAScript `Canonicalize`（非 `u`）与 simple case folding（`u`）；
            // - brace 数字范围的 `String(number)` 指数形式。
            let fixture: serde_json::Value = serde_json::from_str(include_str!(
                "../../tests/fixtures/resolver/minimatch_edge_cases.json"
            ))
            .expect("minimatch edge fixture");
            let cases = fixture["cases"].as_array().expect("cases");
            let mut mismatches = 0usize;
            let mut failures = Vec::new();
            for case in cases {
                let candidate = case[0].as_str().expect("candidate");
                let pattern = case[1].as_str().expect("pattern");
                let expected = case[2].as_bool().expect("expected");
                let actual = minimatch(candidate, pattern);
                if actual != expected {
                    mismatches += 1;
                    if failures.len() < 40 {
                        failures.push(format!(
                            "minimatch({candidate:?}, {pattern:?}) = {actual}, expected {expected}"
                        ));
                    }
                }
            }
            assert!(
                mismatches == 0,
                "{mismatches} mismatches:\n{}",
                failures.join("\n")
            );
        }

        #[test]
        fn glob_escapes_follow_the_minimatch_fast_paths() {
            // `starDotExtRE` 命中时 JS 用 `endsWith(原始转义后缀)` 覆盖 AST 正则。
            assert!(!minimatch("ab", "*\\b"));
            assert!(!minimatch("a.json", "*\\.json"));
            // `\|` 去掉反斜杠后是正则交替：`^A|B$`。
            assert!(minimatch("anything", "\\|*"));
            assert!(minimatch("x", "*\\|*"));
            assert!(minimatch("aQ", "a\\|b"));
            assert!(minimatch("Qb", "a\\|b"));
            assert!(!minimatch("Q", "a\\|b"));
            // 未转义的 `|` 是字面量。
            assert!(minimatch("a|b", "a|b"));
            assert!(!minimatch("Q", "a|b"));
        }

        #[test]
        fn dot_traversal_is_guarded() {
            for pattern in [".*", "[.]*", ".?", ".[^a]", "[.]?", "[.].*", "..*"] {
                assert!(!minimatch(".", pattern), "{pattern} matches .");
                assert!(!minimatch("..", pattern), "{pattern} matches ..");
            }
            assert!(minimatch(".x", ".*"));
            assert!(minimatch(".x", "[.]*"));
            assert!(minimatch(".x", ".?"));
            assert!(minimatch("..x", "..*"));
            // 第二个点渲染成 `\.`（不是 `[`/`.`）时没有遍历守卫。
            assert!(minimatch("..", ".[.]"));
            // `justDots`：整段就是 "." / ".." 时允许。
            assert!(minimatch(".", "."));
            assert!(minimatch("..", ".."));
            // extglob 子 AST 也带守卫。
            assert!(!minimatch(".", "@(.*)"));
            assert!(!minimatch("..", "@(.*)"));
            assert!(minimatch(".x", "@(.*)"));
            assert!(minimatch("..", "@(..)"));
        }

        #[test]
        fn nocase_uses_ecmascript_canonicalization() {
            // 非 `u`：Canonicalize（Kelvin / ſ / ß 不参与折叠；σ/ς 参与）。
            assert!(minimatch("K", "[K]"));
            assert!(minimatch("k", "[K]"));
            assert!(!minimatch("\u{212A}", "[K]"));
            assert!(!minimatch("\u{017F}", "[S]"));
            assert!(minimatch("S", "[S]"));
            assert!(minimatch("s", "[S]"));
            assert!(!minimatch("\u{03C2}", "[S]"));
            // 快捷路径按完整 `toLowerCase` 比较：`ς` 不会折叠成 `σ`/`Σ`。
            assert!(!minimatch("\u{03C2}", "*\u{03A3}"));
            assert!(!minimatch("\u{03C2}", "*\u{03C3}"));
            assert!(minimatch("\u{03C3}", "*\u{03A3}"));
            // AST（非快捷路径）按 Canonicalize 比较：`σ`/`ς` 都折叠成 `Σ`。
            assert!(minimatch("1\u{03C2}", "[[:digit:]][\u{03C3}]"));
            assert!(minimatch("\u{03C2}", "[\u{03C3}]"));
            assert!(!minimatch("\u{1E9E}", "\u{00DF}"));
            // 快捷路径用完整的 `toLowerCase`（`İ` → `i` + U+0307）。
            assert!(minimatch("\u{0130}", "*\u{0130}"));
            assert!(minimatch("i\u{0307}", "*\u{0130}"));
            assert!(minimatch("i\u{0307}", "?\u{0130}"));
            assert!(!minimatch("\u{0130}", "?\u{0130}"));
            // `u` 模式（段内含 POSIX 类）：simple case folding（Kelvin / ſ 参与折叠）。
            assert!(minimatch("1\u{212A}", "[[:digit:]][k]"));
            assert!(minimatch("1\u{017F}", "[[:digit:]][s]"));
        }

        #[test]
        fn brace_numeric_ranges_use_js_number_formatting() {
            assert!(minimatch(
                "1e+21",
                "*{1000000000000000000000..1000000000000000000000}"
            ));
            assert!(minimatch(
                "x1e+21",
                "*{1000000000000000000000..1000000000000000000000}"
            ));
            assert!(!minimatch(
                "1000000000000000000000",
                "*{1000000000000000000000..1000000000000000000000}"
            ));
        }
    }
}

/// `resolveModelScopeFromModels(patterns, models)`。
///
/// 每个模型都可用，因此凭据不能在多个 provider 的同 id 之间裁决：全部加入并给出警告，
/// 让用户用 `provider/id` 选择。
///
/// 分支与顺序：
///
/// - 模式含 `*` / `?` / `[` 任一字符时按 glob 处理：先剥离可选的 `:<thinking>`，再把
///   去掉后缀的文本当**精确引用**尝试一次（因此 `bracketed-model[1m]` 这类 id 不会被
///   误当 glob）；精确引用 ambiguous 时记录诊断并加入全部候选；只有 not-found 才用
///   `minimatch` 同时匹配 `provider/id` 与裸 `id`。glob 跨 provider 命中本身不产生
///   ambiguous 诊断，仅无命中时产生 no-match。
/// - 其它模式按 `thinkingSuffix: Lenient` + `fuzzy: true` 解析；`warning` 转成
///   `invalid-thinking-level` 诊断；ambiguous 时记录诊断并加入全部候选。
///
/// 去重：`add` 比较 type + id + provider，先到者胜——同一模型再次出现时**不会**更新
/// 已记录的 thinking 等级（后面的模式不能覆盖前面的等级）。诊断按模式处理顺序追加；
/// ambiguous 文案里的 provider 列表按 `localeCompare` 排序。
///
/// TS: `core/model-resolver.ts#resolveModelScopeFromModels`
pub fn resolve_model_scope_from_models(
    patterns: &[String],
    models: &[Model],
) -> ResolveModelScopeResult {
    let mut scoped_models: Vec<ScopedModel> = Vec::new();
    let mut diagnostics: Vec<ModelScopeDiagnostic> = Vec::new();
    let add = |scoped_models: &mut Vec<ScopedModel>,
               model: &Model,
               thinking_level: Option<ModelThinkingLevel>| {
        if !scoped_models
            .iter()
            .any(|scoped| same_model(&scoped.model, model))
        {
            scoped_models.push(ScopedModel {
                model: model.clone(),
                thinking_level,
            });
        }
    };
    let no_match = |diagnostics: &mut Vec<ModelScopeDiagnostic>, pattern: &str| {
        diagnostics.push(ModelScopeDiagnostic {
            diagnostic_type: ModelScopeDiagnosticType::Warning,
            code: ModelScopeDiagnosticCode::NoMatch,
            message: format!("No models match pattern \"{pattern}\""),
            pattern: pattern.to_string(),
        });
    };
    let ambiguous = |diagnostics: &mut Vec<ModelScopeDiagnostic>,
                     pattern: &str,
                     candidates: &[Model]| {
        let mut providers: Vec<&str> = Vec::new();
        for candidate in candidates {
            if !providers.contains(&candidate.provider.as_str()) {
                providers.push(candidate.provider.as_str());
            }
        }
        providers.sort_by(|a, b| locale_compare(a, b));
        diagnostics.push(ModelScopeDiagnostic {
            diagnostic_type: ModelScopeDiagnosticType::Warning,
            code: ModelScopeDiagnosticCode::Ambiguous,
            message: format!(
                "Pattern \"{pattern}\" matches several providers: {}. Use provider/id to choose one.",
                providers.join(", ")
            ),
            pattern: pattern.to_string(),
        });
    };

    for pattern in patterns {
        // 含 glob 字符（`*` / `?` / `[`）的模式。
        if pattern.contains('*') || pattern.contains('?') || pattern.contains('[') {
            // 提取可选 thinking 后缀（如 `provider/*:high`）。
            let thinking = split_thinking_suffix(pattern);
            let glob_pattern = thinking
                .as_ref()
                .map_or_else(|| pattern.as_str(), |thinking| thinking.base.as_str());
            let thinking_level = thinking.as_ref().map(|thinking| thinking.thinking_level);

            // `bracketed-model[1m]` 这类 id 不是 glob。
            let exact = resolve_model_reference(
                &ModelReferenceRequest {
                    text: glob_pattern.to_string(),
                    provider: None,
                    thinking_suffix: ThinkingSuffixMode::None,
                    fuzzy: false,
                },
                models,
                &|_| true,
            );
            let matches: Vec<Model> = match exact {
                ModelReferenceResult::Resolved { model, .. } => vec![model],
                ModelReferenceResult::Ambiguous { candidates, .. } => {
                    ambiguous(&mut diagnostics, pattern, &candidates);
                    candidates
                }
                ModelReferenceResult::NotFound { .. } => models
                    .iter()
                    .filter(|model| {
                        minimatch(&format!("{}/{}", model.provider, model.id), glob_pattern)
                            || minimatch(&model.id, glob_pattern)
                    })
                    .cloned()
                    .collect(),
            };
            if matches.is_empty() {
                no_match(&mut diagnostics, pattern);
            }
            for model in &matches {
                add(&mut scoped_models, model, thinking_level);
            }
            continue;
        }

        let result = resolve_model_reference(
            &ModelReferenceRequest {
                text: pattern.clone(),
                provider: None,
                thinking_suffix: ThinkingSuffixMode::Lenient,
                fuzzy: true,
            },
            models,
            &|_| true,
        );
        if result.is_not_found() {
            no_match(&mut diagnostics, pattern);
            continue;
        }
        if let Some(warning) = result.warning() {
            diagnostics.push(ModelScopeDiagnostic {
                diagnostic_type: ModelScopeDiagnosticType::Warning,
                code: ModelScopeDiagnosticCode::InvalidThinkingLevel,
                message: warning.to_string(),
                pattern: pattern.clone(),
            });
        }
        let thinking_level = result.thinking_level();
        match result {
            ModelReferenceResult::Resolved { model, .. } => {
                add(&mut scoped_models, &model, thinking_level);
            }
            ModelReferenceResult::Ambiguous { candidates, .. } => {
                ambiguous(&mut diagnostics, pattern, &candidates);
                for model in &candidates {
                    add(&mut scoped_models, model, thinking_level);
                }
            }
            ModelReferenceResult::NotFound { .. } => {}
        }
    }

    ResolveModelScopeResult {
        scoped_models,
        diagnostics,
    }
}

/// `resolveModelScopeWithDiagnostics(patterns, modelRuntime, options?)`。
///
/// 模型来自 `get_available(None, options)`（已配置认证的模型）；`AuthError` 原样向上
/// 返回，由调用方展示。诊断不会打印，调用方决定如何消费。
///
/// TS: `core/model-resolver.ts#resolveModelScopeWithDiagnostics`
pub async fn resolve_model_scope_with_diagnostics(
    patterns: &[String],
    model_runtime: &dyn ModelResolverRuntime,
    options: AuthOperationOptions,
) -> Result<ResolveModelScopeResult, AuthError> {
    let models = model_runtime.get_available(None, options).await?;
    Ok(resolve_model_scope_from_models(patterns, &models))
}

/// `resolveModelScope(patterns, modelRuntime, options?)`：逐条诊断打印
/// `Warning: ...`（stderr，chalk.yellow），返回 `scopedModels`。
///
/// 每条诊断写一行到 stderr，是否带 SGR 黄色由 `chalk_color_enabled` 的 TTY/环境变量
/// 判定决定；写入失败与 Node 的 `console.warn` 一样被忽略。需要捕获输出时改用
/// [`resolve_model_scope_writing_to`]。
///
/// TS: `core/model-resolver.ts#resolveModelScope`
pub async fn resolve_model_scope(
    patterns: &[String],
    model_runtime: &dyn ModelResolverRuntime,
    options: AuthOperationOptions,
) -> Result<Vec<ScopedModel>, AuthError> {
    let mut stderr = std::io::stderr();
    resolve_model_scope_writing_to(patterns, model_runtime, options, &mut stderr).await
}

/// [`resolve_model_scope`] 的可注入输出流版本（测试用；对应 TS 里被 `console.warn`
/// spy 观察的那条路径）。
///
/// 每条诊断输出一行 `Warning: <message>`（chalk.yellow 上色，规则见
/// [`chalk_color_enabled`]），按诊断顺序写到 `output`。写失败与 Node 的
/// `console.warn` 一样被忽略。
#[doc(hidden)]
pub async fn resolve_model_scope_writing_to(
    patterns: &[String],
    model_runtime: &dyn ModelResolverRuntime,
    options: AuthOperationOptions,
    output: &mut dyn std::io::Write,
) -> Result<Vec<ScopedModel>, AuthError> {
    let result = resolve_model_scope_with_diagnostics(patterns, model_runtime, options).await?;
    for diagnostic in &result.diagnostics {
        let _ = writeln!(
            output,
            "{}",
            chalk_yellow(&format!("Warning: {}", diagnostic.message))
        );
    }
    Ok(result.scoped_models)
}

// ---------------------------------------------------------------------------
// CLI / 初始模型 / 会话恢复
// ---------------------------------------------------------------------------

/// `resolveCliModel(options)`。
///
/// 支持 `--provider <provider> --model <pattern>`、`--model <provider>/<pattern>` 与模糊匹配。
/// 本函数不应用 thinking 等级，但会从 `<pattern>:<thinking>` 解析并返回给调用方。
///
/// 细节：
///
/// - 候选来自 `model_runtime.get_models()`（**全部**模型，不按认证过滤），这样
///   `--api-key` 首次配置也能解析；`--provider` 必须是模型列表里存在的 provider
///   （大小写不敏感），否则返回 `Unknown provider ...` 错误。
/// - `usable`：存在内联 `--api-key` 时恒为 false，表示存储凭证不得决定 provider。
/// - 解析失败但能确定 provider 时走自定义模型回退：复制基座模型、替换 id/name；
///   若最终 thinking 等级不是 `off`，还会把 `reasoning` 置为 `Some(true)`。
///   显式 `--thinking` 优先于后缀解析出的等级，且此时后缀会留在自定义 id 里。
/// - `error` 为 `Some` 时 `model` 恒为 `None`（其余字段取默认值）。
///
/// TS: `core/model-resolver.ts#resolveCliModel`
pub fn resolve_cli_model(options: &ResolveCliModelOptions<'_>) -> ResolveCliModelResult {
    let ResolveCliModelOptions {
        cli_provider,
        cli_model,
        cli_thinking,
        inline_api_key,
        model_runtime,
    } = options;

    // JS `!cliModel`：`undefined` 与空串都视为未提供。
    let Some(cli_model) = cli_model
        .as_deref()
        .filter(|cli_model| !cli_model.is_empty())
    else {
        return ResolveCliModelResult::default();
    };

    // 重要：这里用**全部**模型，而不是只有已配置认证的模型，以便 `--api-key` 首次配置。
    let models = model_runtime.get_models();
    if models.is_empty() {
        return ResolveCliModelResult {
            error: Some(
                "No models available. Check your installation or add models to models.json."
                    .to_string(),
            ),
            ..Default::default()
        };
    }

    // JS `cliProvider ? ... : undefined`：空串是 falsy，等同未提供。
    let cli_provider = cli_provider
        .as_deref()
        .filter(|cli_provider| !cli_provider.is_empty());
    let provider = cli_provider.and_then(|cli_provider| {
        let lower = cli_provider.to_lowercase();
        models
            .iter()
            .find(|model| model.provider.to_lowercase() == lower)
            .map(|model| model.provider.clone())
    });
    if cli_provider.is_some() && provider.is_none() {
        return ResolveCliModelResult {
            error: Some(format!(
                "Unknown provider \"{}\". Use --list-models to see available providers/models.",
                cli_provider.unwrap_or_default()
            )),
            ..Default::default()
        };
    }

    let usable: Box<dyn Fn(&str) -> bool + '_> = if *inline_api_key {
        // 内联 key 会用于被选中的任何 provider，因此存储凭证不得决定 provider。
        Box::new(|_| false)
    } else {
        Box::new(|provider_id: &str| model_runtime.has_configured_auth(provider_id))
    };
    let result = resolve_model_reference(
        &ModelReferenceRequest {
            text: cli_model.to_string(),
            provider: provider.clone(),
            thinking_suffix: ThinkingSuffixMode::Strict,
            fuzzy: true,
        },
        &models,
        usable.as_ref(),
    );
    match result {
        ModelReferenceResult::Resolved {
            model,
            thinking_level,
            warning,
        } => ResolveCliModelResult {
            model: Some(model),
            thinking_level,
            warning,
            error: None,
        },
        ModelReferenceResult::Ambiguous {
            candidates,
            usable_count,
            ..
        } => {
            let mut matches: Vec<String> = candidates
                .iter()
                .map(|model| format!("{}/{}", model.provider, model.id))
                .collect();
            matches.sort_by(|a, b| locale_compare(a, b));
            let auth_hint = if *inline_api_key {
                "--api-key does not choose a provider."
            } else if usable_count == 0 {
                "No matching provider is authenticated."
            } else {
                "More than one matching provider is authenticated."
            };
            ResolveCliModelResult {
                error: Some(format!(
                    "Model \"{cli_model}\" is ambiguous across providers: {}. {auth_hint} Use --provider or provider/model.",
                    matches.join(", ")
                )),
                ..Default::default()
            }
        }
        ModelReferenceResult::NotFound { target: None } => ResolveCliModelResult {
            error: Some(format!(
                "Model \"{cli_model}\" not found. Use --list-models to see available models."
            )),
            ..Default::default()
        },
        ModelReferenceResult::NotFound {
            target: Some(target),
        } => {
            // 只在没有显式 `--thinking` 时使用 thinking 后缀；否则它留在自定义 id 里。
            let model_id = if cli_thinking.is_some() {
                target.model_text.clone()
            } else {
                target.model_id.clone()
            };
            let fallback_thinking = if cli_thinking.is_some() {
                None
            } else {
                target.thinking_level
            };
            let fallback_model = build_fallback_model(&target.provider_models, &model_id);
            let requested_thinking = cli_thinking.or(fallback_thinking);
            let model = if requested_thinking.is_some_and(|level| level != ModelThinkingLevel::Off)
            {
                Model {
                    reasoning: Some(true),
                    ..fallback_model
                }
            } else {
                fallback_model
            };
            ResolveCliModelResult {
                model: Some(model),
                thinking_level: fallback_thinking,
                warning: Some(format!(
                    "Model \"{model_id}\" not found for provider \"{}\". Using custom model id.",
                    target.provider
                )),
                error: None,
            }
        }
    }
}

/// `findInitialModel(options)`：初始模型优先级。
///
/// 1. 作用域模型的第一个（不在继续/恢复会话时）；
/// 2. 设置里的已保存默认模型（且认证已配置）；
/// 3. 第一个可用的默认 provider 模型，否则第一个可用模型；
/// 4. 无。
///
/// 注意：
///
/// - `is_continuing` 只跳过第 1 步，第 2 步及以后总会执行。
/// - 第 1 步的 thinking 优先级：作用域模式里写出的等级 → `model_thinking_levels`
///   （键为 `provider/id`）→ `default_thinking_level` → `DEFAULT_THINKING_LEVEL`。
/// - 第 2 步要求 `default_provider` / `default_model_id` 都非空，且找到的模型满足
///   `has_configured_auth`；thinking 键用**设置里的原始字符串**
///   （`default_provider/default_model_id`），而不是模型对象里的 provider。
/// - 第 3 步先按 [`DEFAULT_MODEL_PER_PROVIDER`] 的声明顺序找默认模型，再退回第一个
///   可用模型；两步都使用 `DEFAULT_THINKING_LEVEL`，不读取设置里的默认 thinking。
///
/// TS: `core/model-resolver.ts#findInitialModel`
pub async fn find_initial_model(options: &FindInitialModelOptions<'_>) -> InitialModelResult {
    let mut thinking_level = DEFAULT_THINKING_LEVEL;

    // 1. 作用域模型（继续/恢复会话时跳过）。
    if !options.scoped_models.is_empty() && !options.is_continuing {
        let scoped_model = &options.scoped_models[0];
        let key = format!("{}/{}", scoped_model.model.provider, scoped_model.model.id);
        let per_model = options
            .model_thinking_levels
            .and_then(|levels| levels.get(&key).copied());
        return InitialModelResult {
            model: Some(scoped_model.model.clone()),
            thinking_level: scoped_model
                .thinking_level
                .or(per_model)
                .or(options.default_thinking_level)
                .unwrap_or(DEFAULT_THINKING_LEVEL),
            fallback_message: None,
        };
    }

    // 2. 设置里的已保存默认模型（认证已配置时）。
    if let (Some(default_provider), Some(default_model_id)) =
        (options.default_provider, options.default_model_id)
        && !default_provider.is_empty()
        && !default_model_id.is_empty()
        && let Some(found) = options
            .model_runtime
            .get_model(default_provider, default_model_id)
        && options.model_runtime.has_configured_auth(&found.provider)
    {
        let key = format!("{default_provider}/{default_model_id}");
        let per_model = options
            .model_thinking_levels
            .and_then(|levels| levels.get(&key).copied());
        if let Some(per_model) = per_model {
            thinking_level = per_model;
        } else if let Some(default_thinking_level) = options.default_thinking_level {
            thinking_level = default_thinking_level;
        }
        return InitialModelResult {
            model: Some(found),
            thinking_level,
            fallback_message: None,
        };
    }

    // 3. 第一个有有效 API key 的可用模型。
    let available_models = options.model_runtime.get_available_snapshot();
    if !available_models.is_empty() {
        // 先找已知 provider 的默认模型（按 `defaultModelPerProvider` 声明顺序）。
        for (provider, default_id) in DEFAULT_MODEL_PER_PROVIDER.iter() {
            let found = available_models
                .iter()
                .find(|model| model.provider == *provider && model.id == *default_id);
            if let Some(model) = found {
                return InitialModelResult {
                    model: Some(model.clone()),
                    thinking_level: DEFAULT_THINKING_LEVEL,
                    fallback_message: None,
                };
            }
        }
        return InitialModelResult {
            model: Some(available_models[0].clone()),
            thinking_level: DEFAULT_THINKING_LEVEL,
            fallback_message: None,
        };
    }

    // 4. 没有可用模型。
    InitialModelResult {
        model: None,
        thinking_level: DEFAULT_THINKING_LEVEL,
        fallback_message: None,
    }
}

/// `restoreModelFromSession(savedProvider, savedModelId, currentModel, shouldPrintMessages, modelRuntime)`。
///
/// `Restored model: ...` / `Falling back to: ...` 用 chalk.dim（stdout），警告用
/// chalk.yellow（stderr）。
///
/// 判定顺序：
///
/// 1. `get_model(saved_provider, saved_model_id)` 命中**且**该 provider 已配置认证：
///    恢复成功；
/// 2. 否则回退到 `current_model`（调用方当前使用的模型）；
/// 3. 否则从 `get_available_snapshot()` 中按 [`DEFAULT_MODEL_PER_PROVIDER`] 顺序找默认
///    模型，找不到就用第一个可用模型；
/// 4. 都没有：`model = None`、`fallback_message = None`。
///
/// `fallback_message` 在 2、3 两条路径上总为 `Some`，文案里带原因（`model no longer
/// exists` 或 `no auth configured`）。`should_print_messages` 只控制控制台输出，不影响
/// 返回值。
///
/// TS: `core/model-resolver.ts#restoreModelFromSession`
pub async fn restore_model_from_session(
    saved_provider: &str,
    saved_model_id: &str,
    current_model: Option<&Model>,
    should_print_messages: bool,
    model_runtime: &dyn ModelResolverRuntime,
) -> RestoreModelResult {
    let restored_model = model_runtime.get_model(saved_provider, saved_model_id);
    let has_configured_auth = restored_model
        .as_ref()
        .is_some_and(|model| model_runtime.has_configured_auth(&model.provider));

    if let Some(restored_model) = restored_model.as_ref().filter(|_| has_configured_auth) {
        if should_print_messages {
            println!(
                "{}",
                chalk_dim(&format!(
                    "Restored model: {saved_provider}/{saved_model_id}"
                ))
            );
        }
        return RestoreModelResult {
            model: Some(restored_model.clone()),
            fallback_message: None,
        };
    }

    // 模型不存在或没有 API key —— 回退。
    let reason = if restored_model.is_none() {
        "model no longer exists"
    } else {
        "no auth configured"
    };
    if should_print_messages {
        eprintln!(
            "{}",
            chalk_yellow(&format!(
                "Warning: Could not restore model {saved_provider}/{saved_model_id} ({reason})."
            ))
        );
    }

    // 已有模型时用它回退。
    if let Some(current_model) = current_model {
        if should_print_messages {
            println!(
                "{}",
                chalk_dim(&format!(
                    "Falling back to: {}/{}",
                    current_model.provider, current_model.id
                ))
            );
        }
        return RestoreModelResult {
            model: Some(current_model.clone()),
            fallback_message: Some(format!(
                "Could not restore model {saved_provider}/{saved_model_id} ({reason}). Using {}/{}.",
                current_model.provider, current_model.id
            )),
        };
    }

    // 尝试任意可用模型。
    let available_models = model_runtime.get_available_snapshot();
    if !available_models.is_empty() {
        // 先找已知 provider 的默认模型（按 `defaultModelPerProvider` 声明顺序）。
        let mut fallback_model: Option<&Model> = None;
        for (provider, default_id) in DEFAULT_MODEL_PER_PROVIDER.iter() {
            if let Some(model) = available_models
                .iter()
                .find(|model| model.provider == *provider && model.id == *default_id)
            {
                fallback_model = Some(model);
                break;
            }
        }
        let fallback_model = fallback_model.unwrap_or(&available_models[0]);
        if should_print_messages {
            println!(
                "{}",
                chalk_dim(&format!(
                    "Falling back to: {}/{}",
                    fallback_model.provider, fallback_model.id
                ))
            );
        }
        return RestoreModelResult {
            model: Some(fallback_model.clone()),
            fallback_message: Some(format!(
                "Could not restore model {saved_provider}/{saved_model_id} ({reason}). Using {}/{}.",
                fallback_model.provider, fallback_model.id
            )),
        };
    }

    RestoreModelResult {
        model: None,
        fallback_message: None,
    }
}

// ---------------------------------------------------------------------------
// chalk（§3.1）：console.warn / console.log 的颜色
// ---------------------------------------------------------------------------

/// chalk 6 的颜色使能判定（`node_modules/chalk/source/vendor/supports-color` 的
/// `stdout` 分支，取 `level > 0`）。
///
/// chalk 的默认实例用 **stdout** 的检测结果决定所有样式的 level（即使 `console.error`
/// 写 stderr）；chalk 6 的 vendored `supports-color` **不**读取 `NO_COLOR`（只有
/// `FORCE_COLOR` 与 `--no-color` 等 argv 标志），这里如实复刻。
///
/// 本实现的判定顺序：
///
/// 1. argv 的 `--no-color` / `--color` 系列（只认 `--` 终止符之前的参数）；
/// 2. 可解析的 `FORCE_COLOR` 覆盖第 1 步：`false` / `0` 直接关闭；`true` / 空串只表示
///    「启用」；非法值忽略，保留第 1 步结果；
/// 3. `--color=16m` / `full` / `truecolor` 直接开启，`--color=256` 紧随其后；
/// 4. 数字形式的 `FORCE_COLOR`（level 1–3）直接开启；
/// 5. Azure DevOps 流水线（`TF_BUILD` 与 `AGENT_NAME` 同时存在）在任何 TTY 判断
///    之前直接开启；
/// 6. 非 TTY 且没有任何强制开启的来源时关闭（第 5 步命中时不会走到这里）；
/// 7. 依次探测 `TERM=dumb`、Windows、CI 变量、`TEAMCITY_VERSION`、`COLORTERM=
///    truecolor`、`TERM` 的特判值（kitty/ghostty/wezterm）、`TERM_PROGRAM`、
///    `TERM` 的 256 色/基本色能力，再探 `COLORTERM` 存在性，最后回退到强制 level
///    是否大于 0。
///
/// 返回值只是「是否上色」，不区分 256 色与 truecolor 的具体 level。
fn chalk_color_enabled() -> bool {
    let arguments = argv_strings(std::env::args_os());
    let has_flag = |flag: &str| -> bool {
        let prefix = if flag.starts_with('-') {
            ""
        } else if flag.len() == 1 {
            "-"
        } else {
            "--"
        };
        let needle = format!("{prefix}{flag}");
        let position = arguments.iter().position(|argument| *argument == needle);
        let terminator = arguments.iter().position(|argument| argument == "--");
        match position {
            Some(position) => terminator.is_none_or(|terminator| position < terminator),
            None => false,
        }
    };
    let mut flag_force_color: Option<u8> = None;
    if has_flag("no-color")
        || has_flag("no-colors")
        || has_flag("color=false")
        || has_flag("color=never")
    {
        flag_force_color = Some(0);
    } else if has_flag("color")
        || has_flag("colors")
        || has_flag("color=true")
        || has_flag("color=always")
    {
        flag_force_color = Some(1);
    }

    let force_color_value = || -> Option<u8> {
        let value = std::env::var_os("FORCE_COLOR")?;
        parse_force_color_level(&value.to_string_lossy())
    };
    let numeric_force_color = std::env::var_os("FORCE_COLOR").is_some_and(|value| {
        let value = value.to_string_lossy();
        !value.is_empty() && value.chars().all(|character| character.is_ascii_digit())
    });
    if let Some(level) = force_color_value() {
        flag_force_color = Some(level);
    }
    let force_color = flag_force_color;
    if force_color == Some(0) {
        return false;
    }
    if has_flag("color=16m") || has_flag("color=full") || has_flag("color=truecolor") {
        return true;
    }
    if has_flag("color=256") {
        return true;
    }
    // 数字形式的 FORCE_COLOR 指定精确 level；`true` / 空串只表示“启用”，level 继续检测。
    if force_color.is_some() && numeric_force_color {
        return force_color != Some(0);
    }
    let env_var_present = |name: &str| std::env::var_os(name).is_some();
    let env_var_equals =
        |name: &str, expected: &str| std::env::var_os(name).is_some_and(|value| value == expected);
    // Azure DevOps 流水线：在 TTY 检查之前。
    if env_var_present("TF_BUILD") && env_var_present("AGENT_NAME") {
        return true;
    }
    if !std::io::stdout().is_terminal() && force_color.is_none() {
        return false;
    }
    let min = force_color.unwrap_or(0);
    if env_var_equals("TERM", "dumb") {
        return min > 0;
    }
    if cfg!(windows) {
        return true;
    }
    if env_var_present("CI") {
        if ["GITHUB_ACTIONS", "GITEA_ACTIONS", "CIRCLECI"]
            .iter()
            .any(|name| env_var_present(name))
        {
            return true;
        }
        if ["TRAVIS", "APPVEYOR", "GITLAB_CI", "BUILDKITE", "DRONE"]
            .iter()
            .any(|name| env_var_present(name))
            || env_var_equals("CI_NAME", "codeship")
        {
            return true;
        }
        return min > 0;
    }
    if let Some(version) = std::env::var_os("TEAMCITY_VERSION") {
        let version = version.to_string_lossy().into_owned();
        return teamcity_supports_color(&version);
    }
    if env_var_equals("COLORTERM", "truecolor") {
        return true;
    }
    let term = std::env::var_os("TERM").map(|value| value.to_string_lossy().into_owned());
    if let Some(term) = term.as_deref()
        && matches!(term, "xterm-kitty" | "xterm-ghostty" | "wezterm")
    {
        return true;
    }
    if let Some(term_program) = std::env::var_os("TERM_PROGRAM") {
        let term_program = term_program.to_string_lossy();
        if term_program == "iTerm.app" || term_program == "Apple_Terminal" {
            return true;
        }
    }
    if let Some(term) = term.as_deref()
        && (term_has_256_color(term) || term_has_basic_color(term))
    {
        return true;
    }
    if env_var_present("COLORTERM") {
        return true;
    }
    min > 0
}

/// `process.argv` 的等价物：Node 把 argv 字节按 UTF-8 **有损**解码（非法字节变成
/// U+FFFD），永远不会因为非 UTF-8 参数抛错。`std::env::args()` 会 panic，所以这里用
/// `args_os()` + `to_string_lossy()`。
fn argv_strings(arguments: impl IntoIterator<Item = std::ffi::OsString>) -> Vec<String> {
    arguments
        .into_iter()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect()
}

/// `/^(?:9\.0*[1-9]\d*\.|\d{2,}\.)/`：TeamCity 9.1+ 支持颜色。
fn teamcity_supports_color(version: &str) -> bool {
    if let Some(rest) = version.strip_prefix("9.") {
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        if digits > 0 {
            let number: String = rest.chars().take(digits).collect();
            let significant = number.trim_start_matches('0');
            return !significant.is_empty() && rest.as_bytes().get(digits) == Some(&b'.');
        }
        return false;
    }
    let leading_digits = version.bytes().take_while(u8::is_ascii_digit).count();
    leading_digits >= 2 && version.as_bytes().get(leading_digits) == Some(&b'.')
}

/// `/-256(?:color)?$/i`。
fn term_has_256_color(term: &str) -> bool {
    let lower = term.to_lowercase();
    lower.ends_with("-256") || lower.ends_with("-256color")
}

/// `/^screen|^xterm|^vt100|^vt220|^rxvt|color|ansi|cygwin|linux/i`。
fn term_has_basic_color(term: &str) -> bool {
    let lower = term.to_lowercase();
    lower.starts_with("screen")
        || lower.starts_with("xterm")
        || lower.starts_with("vt100")
        || lower.starts_with("vt220")
        || lower.starts_with("rxvt")
        || lower.contains("color")
        || lower.contains("ansi")
        || lower.contains("cygwin")
        || lower.contains("linux")
}

/// supports-color 的 `envForceColor()`：`FORCE_COLOR` → level。
///
/// 数字形式对应 `Math.min(Number.parseInt(value, 10), 3)`；`parseInt` 对任意长的
/// 数字串都成功（超出 f64 时是 Infinity），因此这里用饱和累加并在 3 处钳住，
/// 而不是 `parse::<i64>()`（超长数字会返回 None，导致颜色被错误关闭）。
fn parse_force_color_level(value: &str) -> Option<u8> {
    if value == "false" {
        return Some(0);
    }
    if value == "true" || value.is_empty() {
        return Some(1);
    }
    if !value.chars().all(|character| character.is_ascii_digit()) {
        return None;
    }
    let mut level: u8 = 0;
    for byte in value.bytes() {
        level = level.saturating_mul(10).saturating_add(byte - b'0').min(3);
    }
    Some(level)
}

/// `chalk.yellow(text)`（SGR 33 / 39）。
fn chalk_yellow(text: &str) -> String {
    if chalk_color_enabled() {
        format!("\u{1b}[33m{text}\u{1b}[39m")
    } else {
        text.to_string()
    }
}

/// `chalk.dim(text)`（SGR 2 / 22）。
fn chalk_dim(text: &str) -> String {
    if chalk_color_enabled() {
        format!("\u{1b}[2m{text}\u{1b}[22m")
    } else {
        text.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kiana_ai::models::EXTENDED_THINKING_LEVELS;

    #[test]
    fn argv_decoding_is_lossy_like_process_argv() {
        // chalk/supports-color 读 `process.argv`；Node 对非法 UTF-8 做有损解码，
        // 而 `std::env::args()` 会 panic。回归：非 UTF-8 参数被替换成 U+FFFD。
        let mut arguments = vec![std::ffi::OsString::from("kiana")];
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            arguments.push(std::ffi::OsString::from_vec(vec![0xff, 0xfe, b'-']));
        }
        arguments.push(std::ffi::OsString::from("--no-color"));
        let decoded = argv_strings(arguments);
        assert_eq!(decoded.last().map(String::as_str), Some("--no-color"));
        #[cfg(unix)]
        {
            assert_eq!(decoded.len(), 3);
            assert!(decoded[1].contains('\u{FFFD}'));
        }
    }

    #[test]
    fn thinking_level_validator_matches_extended_levels_plus_off() {
        // `EXTENDED_THINKING_LEVELS` 本身以 `off` 开头，与 TS 的
        // `VALID_THINKING_LEVELS = ["off", "minimal", "low", "medium", "high", "xhigh", "max"]`
        // 一一对应。
        let expected: Vec<String> = EXTENDED_THINKING_LEVELS
            .iter()
            .map(|level| {
                serde_json::to_value(level)
                    .expect("level serializes")
                    .as_str()
                    .expect("level is a string")
                    .to_string()
            })
            .collect();
        assert_eq!(
            expected,
            vec!["off", "minimal", "low", "medium", "high", "xhigh", "max"]
        );
        for level in &expected {
            assert!(is_valid_thinking_level(level), "{level}");
            assert!(parse_thinking_level(level).is_some(), "{level}");
        }
        for invalid in ["", "none", "HIGH", "max ", "extreme", "0"] {
            assert!(!is_valid_thinking_level(invalid), "{invalid}");
        }
        assert_eq!(
            parse_thinking_level("xhigh"),
            Some(ModelThinkingLevel::Xhigh)
        );
    }

    #[test]
    fn is_alias_prefers_plain_ids_and_latest_over_dates() {
        assert!(is_alias("claude-sonnet-4-5"));
        assert!(is_alias("claude-sonnet-4-5-latest"));
        assert!(!is_alias("claude-sonnet-4-5-20250929"));
        assert!(!is_alias("-20250929"));
        assert!(is_alias("a-2025092"));
        // 结尾不是“- + 8 位数字”的任何 id 都是别名（含数字与字母混排）。
        assert!(is_alias("a-2025092x"));
        assert!(is_alias("20250929"));
        assert!(is_alias("model-20250929-1"));
    }

    #[test]
    fn force_color_parses_arbitrarily_long_numeric_levels() {
        // supports-color：`Math.min(Number.parseInt(value, 10), 3)`；超长数字串仍是 3。
        assert_eq!(parse_force_color_level("0"), Some(0));
        assert_eq!(parse_force_color_level("1"), Some(1));
        assert_eq!(parse_force_color_level("3"), Some(3));
        assert_eq!(parse_force_color_level("4"), Some(3));
        assert_eq!(parse_force_color_level("007"), Some(3));
        assert_eq!(parse_force_color_level("000"), Some(0));
        assert_eq!(parse_force_color_level("99999999999999999999"), Some(3));
        let four_hundred_digits = "9".repeat(400);
        assert_eq!(parse_force_color_level(&four_hundred_digits), Some(3));
        assert_eq!(parse_force_color_level("true"), Some(1));
        assert_eq!(parse_force_color_level(""), Some(1));
        assert_eq!(parse_force_color_level("false"), Some(0));
        assert_eq!(parse_force_color_level("4abc"), None);
        assert_eq!(parse_force_color_level("-1"), None);
    }

    #[test]
    fn split_thinking_suffix_uses_the_last_colon() {
        assert_eq!(
            split_thinking_suffix("provider/model:high"),
            Some(ThinkingSplit {
                base: "provider/model".to_string(),
                thinking_level: ModelThinkingLevel::High,
            })
        );
        assert_eq!(
            split_thinking_suffix("a:b:medium").map(|split| split.base),
            Some("a:b".to_string())
        );
        assert_eq!(split_thinking_suffix("qwen/qwen3-coder:exacto"), None);
        assert_eq!(split_thinking_suffix("no-colon"), None);
    }

    #[test]
    fn split_provider_prefix_uses_the_longest_provider_id() {
        let models = vec![
            Model {
                id: "x".into(),
                provider: "llama=http://host:10800".into(),
                ..Model::default()
            },
            Model {
                id: "y".into(),
                provider: "llama=http".into(),
                ..Model::default()
            },
        ];
        assert_eq!(
            split_provider_prefix("llama=http://host:10800/unsloth/qwen", &models),
            Some(ProviderSplit {
                provider: "llama=http://host:10800".to_string(),
                model_text: "unsloth/qwen".to_string(),
            })
        );
        assert_eq!(split_provider_prefix("unknown/model", &models), None);
    }

    #[test]
    fn minimatch_matches_minimatch_semantics_for_the_documented_subset() {
        assert!(minimatch("anthropic/claude-sonnet-4-5", "anthropic/*"));
        assert!(!minimatch("anthropic/claude-sonnet-4-5", "*sonnet*"));
        assert!(minimatch("claude-sonnet-4-5", "*sonnet*"));
        assert!(minimatch("anthropic/claude-sonnet-4-5", "**"));
        assert!(minimatch("openai/gpt-4o", "{openai,azure}/*"));
        assert!(minimatch("gpt-4o", "gpt-[4a]*"));
        assert!(!minimatch("gpt-4o", "gpt-[!4]*"));
        assert!(minimatch("A/B", "a/b"));
        assert!(minimatch("[", "["));
        assert!(!minimatch("#foo", "#*"));
        assert!(minimatch("gpt-4o", "!*sonnet*"));
    }
}
