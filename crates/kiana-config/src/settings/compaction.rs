//! compaction token 解析与校验。
//!
//! TS: `packages/coding-agent/src/core/settings-manager.ts#getCompactionTokenSetting`
//! （模块私有）以及 `getCompactionReserveTokens` / `getCompactionKeepRecentTokens` /
//! `getCompactionSettings`。
//!
//! 解析顺序（每个字段独立）：`modelOverrides[`${provider}/${id}`][field]` →
//! `compaction[field]` → 内置默认值（16384 / 20000）。
//!
//! 校验规则（`settings-manager-compaction.test.ts` 锁定）：
//!
//! - 普通值先校验：即使模型 override 有效，普通值非法也会抛错；
//! - 模型键是精确的 `${provider}/${id}`（`provider/family/model` 含两个 `/`，不做 glob）；
//! - 条目必须是可合并对象（`typeof === "object" && !== null && !Array.isArray`）；
//! - 合法值是非负安全整数；`0` 合法；`NaN` / `Infinity`（只能经 `applyOverrides` 注入）
//!   非法，错误文案里的 `String(value)` 是 `"NaN"` / `"Infinity"`；
//! - `enabled` 不支持 per-model override（`modelOverrides[...].enabled` 被忽略）。
//!
//! 错误文案逐字见 `ca-config-models.md` §2.9.11。
//!
//! # 执行流程
//!
//! 1. [`SettingsManager::get_compaction_settings`] 在设置快照里取 `compaction` 子对象；
//!    `enabled` 交给 `settings::mod` 的 `nested_boolean_setting`（带原始值回落），两个
//!    token 字段交给本模块的 `get_compaction_token_setting`；
//! 2. 每个 token 字段独立解析：先校验普通值，再取
//!    `modelOverrides[provider/id][field]`，最终取值优先级是
//!    「override → 普通值 → 内置默认值」，两个字段互不影响（测试
//!    `resolves_each_field_independently_and_keeps_individual_getters_consistent` 锁定）；
//! 3. 任何一步遇到类型/范围非法都立即返回 [`SettingsValueError`]，没有「跳过坏值继续」
//!    的降级路径；调用方 `kiana-session` 的 `compaction_settings` 在 `Err` 时整体回退到
//!    `kiana-compaction` 的 `CompactionSettings::default()`。
//!
//! # 边界
//!
//! - 校验同时覆盖反序列化后的 `f64` 与原始 JSON：JSON 里存不下 `NaN` / `Infinity`，它们
//!   只可能经 `applyOverrides` 注入类型化字段（测试 `reports_non_finite_runtime_values`
//!   覆盖）；
//! - `modelOverrides` 里只要有一个条目无法整体解析成 [`CompactionModelOverride`]，整张
//!   记录就退回到 `schema` 的 `extra` 里保存原始 JSON；因此这里对类型化条目与原始条目
//!   分别处理，避免同级坏条目影响其它合法模型（测试
//!   `reads_valid_entries_alongside_malformed_model_overrides` 覆盖）。

use serde_json::Value;

use crate::settings::SettingsManager;
use crate::settings::SettingsValueError;
use crate::settings::defaults::SETTINGS_DEFAULTS;
use crate::settings::js_string_number;
use crate::settings::schema::{CompactionModelOverride, CompactionSettings, Settings};

/// `Number.MAX_SAFE_INTEGER`。
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

/// `DEFAULT_COMPACTION_TOKEN_SETTINGS`（`reserveTokens` / `keepRecentTokens` 的兜底）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CompactionTokenDefaults {
    /// `SETTINGS_DEFAULTS.compaction.reserveTokens`。
    pub reserve_tokens: f64,
    /// `SETTINGS_DEFAULTS.compaction.keepRecentTokens`。
    pub keep_recent_tokens: f64,
}

/// `DEFAULT_COMPACTION_TOKEN_SETTINGS`。
pub const DEFAULT_COMPACTION_TOKEN_SETTINGS: CompactionTokenDefaults = CompactionTokenDefaults {
    reserve_tokens: SETTINGS_DEFAULTS.compaction.reserve_tokens,
    keep_recent_tokens: SETTINGS_DEFAULTS.compaction.keep_recent_tokens,
};

/// TS `Pick<Model<string>, "provider" | "id">`：模型键的来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactionModelRef<'a> {
    /// `model.provider`。
    pub provider: &'a str,
    /// `model.id`。
    pub id: &'a str,
}

impl<'a> CompactionModelRef<'a> {
    /// 从 `provider` / `id` 构造。
    pub fn new(provider: &'a str, id: &'a str) -> Self {
        Self { provider, id }
    }

    /// 从 `kiana_ai::Model` 构造。
    pub fn from_model(model: &'a kiana_ai::Model) -> Self {
        Self {
            provider: &model.provider,
            id: &model.id,
        }
    }

    /// `${provider}/${id}`：唯一匹配键（精确，不做 glob）。
    pub fn model_key(&self) -> String {
        format!("{}/{}", self.provider, self.id)
    }
}

/// `getCompactionSettings(model?)` 的返回值。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResolvedCompactionSettings {
    /// `enabled`。
    pub enabled: bool,
    /// 解析后的 `reserveTokens`。
    pub reserve_tokens: f64,
    /// 解析后的 `keepRecentTokens`。
    pub keep_recent_tokens: f64,
}

/// `keyof CompactionModelOverride`（TS `"reserveTokens" | "keepRecentTokens"`）。
///
/// 两个字段的校验/回落逻辑完全同构，用这个内部枚举把字段名、默认值与类型化访问集中到
/// 一处，避免复制两份容易漂移的代码。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CompactionTokenField {
    ReserveTokens,
    KeepRecentTokens,
}

impl CompactionTokenField {
    /// JSON 属性名（错误文案与默认值查表用）。
    fn key(self) -> &'static str {
        match self {
            CompactionTokenField::ReserveTokens => "reserveTokens",
            CompactionTokenField::KeepRecentTokens => "keepRecentTokens",
        }
    }

    fn default_value(self) -> f64 {
        match self {
            CompactionTokenField::ReserveTokens => DEFAULT_COMPACTION_TOKEN_SETTINGS.reserve_tokens,
            CompactionTokenField::KeepRecentTokens => {
                DEFAULT_COMPACTION_TOKEN_SETTINGS.keep_recent_tokens
            }
        }
    }

    fn typed_number(self, entry: &CompactionModelOverride) -> Option<f64> {
        match self {
            CompactionTokenField::ReserveTokens => entry.reserve_tokens,
            CompactionTokenField::KeepRecentTokens => entry.keep_recent_tokens,
        }
    }
}

/// `Number.isSafeInteger(value) && value >= 0`（`-0` 与 `0` 等价，视为合法）。
///
/// 用 `f64` 而不是整数类型，是因为 `applyOverrides` 注入的运行时值可以是任意浮点
/// （含 `NaN` / `±Infinity`）；`trunc() == value` 判断整数性，`MAX_SAFE_INTEGER` 复刻
/// JS 的安全整数上界，超出后 `f64` 已无法精确表示相邻整数。
fn is_non_negative_safe_integer(value: f64) -> bool {
    value.is_finite() && value.trunc() == value && (0.0..=MAX_SAFE_INTEGER).contains(&value)
}

/// `String(value)`：JSON 值走 `js_string`，运行期 float 走 `String(number)`。
fn token_value_text(value: &Value) -> String {
    kiana_json::js_string::js_string(Some(value))
}

fn invalid_token_error(prefix: &str, value_text: String) -> SettingsValueError {
    SettingsValueError::new(format!(
        "{prefix}: {value_text}. Expected a non-negative safe integer."
    ))
}

/// 普通值（`compaction[field]`）的校验。
///
/// - `Ok(None)`：字段完全缺失，由 override 或默认值兜底；
/// - `Ok(Some(value))`：合法值；
/// - `Err`：字段存在但非法，调用方必须原样上报（不允许被有效的模型 override 掩盖）。
///
/// 先看类型化字段，再回落原始 JSON：`applyOverrides` 可以把 JSON 表示不了的
/// `NaN` / `Infinity` 直接写进类型化字段；而文件里的 `null`、字符串、布尔、对象、数组
/// 与越界数字都留在原始值路径，错误文案里的值用 JS 的 `String(value)` 拼出。
fn ordinary_token(
    compaction: Option<&CompactionSettings>,
    field: CompactionTokenField,
) -> Result<Option<f64>, SettingsValueError> {
    let Some(compaction) = compaction else {
        return Ok(None);
    };
    // 先看类型化字段：`applyOverrides` 可以把 NaN / Infinity 注入到这里（JSON 里存不下）。
    let typed = match field {
        CompactionTokenField::ReserveTokens => compaction.reserve_tokens,
        CompactionTokenField::KeepRecentTokens => compaction.keep_recent_tokens,
    };
    if let Some(value) = typed {
        if is_non_negative_safe_integer(value) {
            return Ok(Some(value));
        }
        return Err(invalid_token_error(
            &format!("Invalid compaction.{} setting", field.key()),
            js_string_number(value),
        ));
    }
    // 原始值：null / 字符串 / 布尔 / 对象 / 数组 / 越界数字都在这里报错。
    if let Some(raw) = compaction.raw_token_setting(field.key()) {
        if let Some(value) = raw
            .as_f64()
            .filter(|value| is_non_negative_safe_integer(*value))
        {
            return Ok(Some(value));
        }
        return Err(invalid_token_error(
            &format!("Invalid compaction.{} setting", field.key()),
            token_value_text(&raw),
        ));
    }
    Ok(None)
}

/// 模型 override（`compaction.modelOverrides[modelKey]`）的校验。
///
/// 条目存在但不是 JSON 对象（`null`、数字、字符串、布尔、数组）时立即报
/// `Expected an object`，即使该条目根本没有本字段——这是 TS
/// `entry !== undefined && !isMergeableObject(entry)` 检查的位置。条目是对象但字段缺失
/// 时返回 `Ok(None)`，继续回落到普通值/默认值。
///
/// 类型化条目只在整张 `modelOverrides` 都能解析时存在；否则必须按原始对象逐字段读取，
/// 这样同级的坏条目不会让其它合法模型一起失去类型化表示。
fn model_override_token(
    compaction: Option<&CompactionSettings>,
    field: CompactionTokenField,
    model_key: &str,
) -> Result<Option<f64>, SettingsValueError> {
    let Some(compaction) = compaction else {
        return Ok(None);
    };
    let Some(entry_raw) = compaction.raw_model_override(model_key) else {
        return Ok(None);
    };

    // `entry !== undefined && !isMergeableObject(entry)`：数组也不算对象。
    if !entry_raw.is_object() {
        return Err(SettingsValueError::new(format!(
            "Invalid compaction.modelOverrides[\"{model_key}\"] setting: {}. Expected an object.",
            token_value_text(&entry_raw)
        )));
    }

    // 类型化条目只在整张 modelOverrides 都能解析时存在；否则退回到原始对象。
    if let Some(entry) = compaction
        .model_overrides
        .as_ref()
        .and_then(|overrides| overrides.get(model_key))
    {
        if let Some(value) = field.typed_number(entry) {
            if is_non_negative_safe_integer(value) {
                return Ok(Some(value));
            }
            return Err(invalid_token_error(
                &format!(
                    "Invalid compaction.modelOverrides[\"{model_key}\"].{} setting",
                    field.key()
                ),
                js_string_number(value),
            ));
        }
        if let Some(raw) = entry.raw(field.key()) {
            return Err(invalid_token_error(
                &format!(
                    "Invalid compaction.modelOverrides[\"{model_key}\"].{} setting",
                    field.key()
                ),
                token_value_text(&raw),
            ));
        }
        return Ok(None);
    }

    match entry_raw.get(field.key()) {
        Some(raw) => {
            if let Some(value) = raw
                .as_f64()
                .filter(|value| is_non_negative_safe_integer(*value))
            {
                return Ok(Some(value));
            }
            Err(invalid_token_error(
                &format!(
                    "Invalid compaction.modelOverrides[\"{model_key}\"].{} setting",
                    field.key()
                ),
                token_value_text(raw),
            ))
        }
        None => Ok(None),
    }
}

/// 单个 token 字段的完整解析：普通值 → 精确模型 override → 内置默认值。
///
/// 普通值**先**校验（即使 override 有效，普通值非法也要报错），随后才解析模型键；两者
/// 的校验互不影响，各自返回自己的错误文案。
fn get_compaction_token_setting(
    settings: &Settings,
    field: CompactionTokenField,
    model: Option<CompactionModelRef<'_>>,
) -> Result<f64, SettingsValueError> {
    let compaction = settings.compaction.as_ref();
    // 普通值先校验（即使 override 有效）。
    let ordinary = ordinary_token(compaction, field)?;

    let override_value = match model {
        Some(model) => model_override_token(compaction, field, &model.model_key())?,
        None => None,
    };

    Ok(override_value
        .or(ordinary)
        .unwrap_or_else(|| field.default_value()))
}

impl SettingsManager {
    /// `getCompactionReserveTokens(model?)`。
    ///
    /// # Errors
    ///
    /// 普通值或模型 override 不是非负安全整数时返回逐字错误文案。
    pub fn get_compaction_reserve_tokens(
        &self,
        model: Option<CompactionModelRef<'_>>,
    ) -> Result<f64, SettingsValueError> {
        self.with_settings(|settings| {
            get_compaction_token_setting(settings, CompactionTokenField::ReserveTokens, model)
        })
    }

    /// `getCompactionKeepRecentTokens(model?)`。
    ///
    /// # Errors
    ///
    /// 同 [`Self::get_compaction_reserve_tokens`]。
    pub fn get_compaction_keep_recent_tokens(
        &self,
        model: Option<CompactionModelRef<'_>>,
    ) -> Result<f64, SettingsValueError> {
        self.with_settings(|settings| {
            get_compaction_token_setting(settings, CompactionTokenField::KeepRecentTokens, model)
        })
    }

    /// `getCompactionSettings(model?)`：每个 token 独立解析。
    ///
    /// # Errors
    ///
    /// 同 [`Self::get_compaction_reserve_tokens`]。
    pub fn get_compaction_settings(
        &self,
        model: Option<CompactionModelRef<'_>>,
    ) -> Result<ResolvedCompactionSettings, SettingsValueError> {
        self.with_settings(|settings| {
            Ok(ResolvedCompactionSettings {
                // `enabled` 不支持 per-model override：只读顶层布尔，并保留原始值回落。
                enabled: crate::settings::nested_boolean_setting(
                    settings,
                    &["compaction"],
                    "enabled",
                    settings
                        .compaction
                        .as_ref()
                        .and_then(|compaction| compaction.enabled),
                    SETTINGS_DEFAULTS.compaction.enabled,
                ),
                reserve_tokens: get_compaction_token_setting(
                    settings,
                    CompactionTokenField::ReserveTokens,
                    model,
                )?,
                keep_recent_tokens: get_compaction_token_setting(
                    settings,
                    CompactionTokenField::KeepRecentTokens,
                    model,
                )?,
            })
        })
    }
}
