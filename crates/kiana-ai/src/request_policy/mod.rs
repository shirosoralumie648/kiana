//! Provider 请求策略：能力门、输出预算、错误分类、可重试、输入预检、usage 记账、
//! prompt cache 语义与声明式 provider 校验。
//!
//! 这些模块解决同一个问题：**在发请求之前就知道会不会失败，失败后知道该重试、该压缩还是
//! 该换模型**。它们都是纯函数/纯数据，不读环境、不发 IO、不持有全局状态；接线由调用方
//! （agent loop / session / provider 适配器）负责，见 `docs/history/porting/handoffs/phase-09.md`。
//!
//! 与 TS `packages/ai` 的关系：本模块不是 TS 的逐行移植，而是参考 cline provider-option
//! 规则表后提炼的三件事——能力门（[`capabilities`]）、预算（[`budget`]）、分类
//! （[`classification`]）——加上 Kiana 需要的预检/usage/cache/声明式校验。规则表其余
//! 内容（每个 provider 的具体参数映射）没有搬过来。
//!
//! # 使用顺序
//!
//! ```text
//! prepare_request(&RequestPolicyInput { .. })?  // 能力门 → 预检 → 预算 → cache
//!     -> plan.max_tokens
//!     -> plan.cache（是否注入断点）
//!     -> 发请求
//!     -> 失败时 classify_provider_error / is_retryable_provider_error
//!     -> 成功时 account_provider_usage
//! ```
//!
//! # 模块
//!
//! - [`capabilities`]：manifest × adapter 双重能力门，发网络前返回 `Unsupported`；
//! - [`budget`]：输出预算公式与 typed context overflow；
//! - [`classification`]：typed 优先 + depth 8 结构遍历的错误分类与可重试判定；
//! - [`precheck`]：请求前输入 token 预检（超窗零请求）；
//! - [`usage`]：canonical → 用户声明 → provider-reported 的 usage 记账；
//! - [`cache`]：`ExplicitBreakpoints`/`ImplicitStrict`/`ImplicitTolerant`/`Uncached`
//!   与断点注入、warmer 资格；
//! - [`declarative`]：声明式 provider 的加载期校验、坏条目跳过与未知字段往返；
//! - [`policy`]：以上能力的组合入口。

pub mod budget;
pub mod cache;
pub mod capabilities;
pub mod classification;
pub mod declarative;
pub mod policy;
pub mod precheck;
pub mod usage;

pub use budget::{
    BudgetCap, ContextOverflow, OUTPUT_FLOOR_FRACTION, OUTPUT_RESERVE_TOKENS, OutputBudget,
    OutputBudgetError, OutputBudgetInput, PRODUCT_DEFAULT_MAX_OUTPUT_TOKENS, compute_output_budget,
    output_budget_for_model, output_floor,
};
pub use cache::{
    CacheSemantics, apply_cache_breakpoints, cache_warmer_eligible,
    has_known_prompt_cache_lifetime, prompt_cache_lifetime_seconds,
};
pub use capabilities::{
    CapabilityGate, CapabilitySet, CapabilitySide, MissingCapability, ModelOperation,
    ProviderCapabilityManifest, ProviderModelCapability, RequestRequirements,
    UnsupportedCapability,
};
pub use classification::{
    MAX_WALK_DEPTH, ProviderErrorClass, ProviderErrorSignals, RETRYABLE_STATUS_CODES,
    TypedProviderError, classify_provider_error, classify_provider_error_with_typed,
    classify_typed, collect_provider_error_signals, is_retryable_provider_error,
    is_retryable_status,
};
pub use declarative::{
    DeclarativeDiagnostic, DeclarativeDiagnosticCode, DeclarativeLoadResult, DeclarativeModel,
    DeclarativeProvider, DeclarativeProviderError, is_valid_identifier, load_declarative_provider,
};
pub use policy::{
    RequestPlan, RequestPolicyError, RequestPolicyInput, error_kind, is_context_window,
    is_unsupported, policy_ai_error, prepare_request,
};
pub use precheck::{
    ContextWindowExceeded, InputPrecheck, estimate_input_tokens, precheck_input,
    precheck_input_with_defaults,
};
pub use usage::{
    AccountedUsage, CostSource, account_provider_usage, canonical_usage, has_known_model_pricing,
    resolve_usage,
};
