//! core/client-identity.ts。
//!
//! TS: `packages/coding-agent/src/core/client-identity.ts`（16 行）。规范：
//! `spec/ca-config-models.md` §2.5。
//!
//! ```ts
//! return {
//!   userAgent: "detailed",
//!   version: VERSION,
//!   attribution: isInstallTelemetryEnabled(settingsManager) ? KIANA_ATTRIBUTION : null,
//! };
//! ```
//!
//! `attribution: null` 表示**明确关闭**（合并身份时会覆盖默认值）；Rust 侧用
//! `Option<Option<ClientAttribution>>` 的三态表达（`Some(None)`），不能写成缺省的 `None`
//! （那表示“不改动”）。
//!
//! 请求头格式由 `kiana_ai::utils::identity` 负责（detailed UA、OpenRouter / NVIDIA / OpenCode
//! attribution 头），本模块只决定身份内容。
//!
//! # 这个模块解决什么问题
//! `SettingsManager` 的安装遥测开关（`enableInstallTelemetry`，可被 `KIANA_TELEMETRY`
//! 覆盖）决定是否向 provider 声明归因信息。本模块把开关状态翻译成一个
//! `kiana_ai::types::ClientIdentity` 值；三层身份合并与请求头生成都在
//! `kiana_ai::utils::identity`，本模块不接触 HTTP，也不发请求。
//!
//! # 执行流程
//! 1. 读开关：[`get_client_identity`] 从进程环境取 `KIANA_TELEMETRY`；
//!    [`get_client_identity_with_env`] 直接接收该值，供测试与上层注入。
//! 2. 固定字段：`user_agent` 为 `detailed`，`version` 取 `kiana_core::config::VERSION`
//!    （coding-agent 的版本，不是 `kiana-ai` 的版本）。
//! 3. `attribution`：开关为真时克隆 `KIANA_ATTRIBUTION`（`Some(Some(..))`）；为假时写
//!    `Some(None)`（显式关闭）。
//!
//! # 调用方与生效时机
//! `kiana-session` 的 `build_request_options` 在每次请求前调用 [`get_client_identity`]，
//! 把结果写进 `base.base.identity`，因此设置或环境变量变化会在下一次请求生效；本模块
//! 自身不缓存。
//!
//! # 失败模式
//! - `KIANA_TELEMETRY` 未定义，或值不是合法 UTF-8（`std::env::var` 返回 `Err`）时，
//!   回落到 settings；已定义但为空串按“假”处理。真值表见 `crate::defaults`。
//! - `attribution` 永远不会是 `None`：在合并语义里 `None` 表示“本次不设置”，会让下层
//!   身份保留自己的 attribution；这里必须明确写出开或关。

use kiana_ai::types::{ClientAttribution, ClientIdentity, UserAgentMode, UserAgentModeDetailed};
use kiana_ai::utils::identity::KIANA_ATTRIBUTION;
use kiana_core::config::VERSION;

use crate::defaults::{is_install_telemetry_enabled, is_install_telemetry_enabled_with_env};
use crate::settings::SettingsManager;

/// `getClientIdentity(settingsManager)`。
///
/// 每次调用都重新读取 `KIANA_TELEMETRY` 与 settings（无缓存）。返回的 `attribution`
/// 只能是 `Some(Some(KIANA_ATTRIBUTION))` 或 `Some(None)`；`user_agent` 恒为 `detailed`，
/// `version` 恒为 `VERSION`。环境变量与 settings 的优先级见
/// [`crate::defaults::is_install_telemetry_enabled`]。
///
/// TS: `core/client-identity.ts#getClientIdentity`
pub fn get_client_identity(settings_manager: &SettingsManager) -> ClientIdentity {
    let enabled = is_install_telemetry_enabled(settings_manager);
    build_client_identity(enabled)
}

/// [`get_client_identity`] 的显式环境值版本（测试与上层注入用，避免进程环境竞争）。
///
/// `telemetry_env` 是 `KIANA_TELEMETRY` 的值：`None` = 未定义（回落到设置），其余按
/// `isInstallTelemetryEnabled` 的真值表处理。
///
/// 真值表：`Some("1" | "true" | "yes")`（大小写不敏感）为真；`Some("")` 以及其余任何
/// 字符串（含 `"0"`、`"on"`）为假。只要传入 `Some` 就完全覆盖 settings。本函数不读
/// 进程环境，因此不会与其他测试修改环境变量发生竞争。
pub fn get_client_identity_with_env(
    settings_manager: &SettingsManager,
    telemetry_env: Option<&str>,
) -> ClientIdentity {
    let enabled = is_install_telemetry_enabled_with_env(settings_manager, telemetry_env);
    build_client_identity(enabled)
}

/// 固定 `user_agent = detailed` 与 `version = VERSION`，只让 `attribution` 随开关变化。
///
/// 不变量：返回值的 `attribution` 永远是 `Some(..)`。`None` 的合并语义是“本次不设置”，
/// 用它表示关闭会让下层身份的 attribution 继续生效。
fn build_client_identity(telemetry_enabled: bool) -> ClientIdentity {
    ClientIdentity {
        user_agent: Some(UserAgentMode::Detailed(UserAgentModeDetailed::Detailed)),
        version: Some(VERSION.to_string()),
        attribution: Some(attribution_for(telemetry_enabled)),
    }
}

/// `telemetryEnabled ? KIANA_ATTRIBUTION : null`。
///
/// 返回 `None` 由 `build_client_identity` 包进 `Some(..)` 表示显式关闭；单独看这个
/// `None` 不要当成“不改动”（那是外层 `Option<Option<_>>` 的第三态）。
fn attribution_for(telemetry_enabled: bool) -> Option<ClientAttribution> {
    if telemetry_enabled {
        Some(KIANA_ATTRIBUTION.clone())
    } else {
        None
    }
}
