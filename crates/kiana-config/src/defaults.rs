//! `core/defaults.ts`、`core/experimental.ts`、`core/radius.ts`、`core/telemetry.ts`。
//!
//! 四个小模块从 `kiana-core` 移到本 crate，理由见 `ARCHITECTURE.md` §3.3：
//! 它们的读者要么在 `kiana-config`（`model-resolver.rs`、`client_identity.rs`），要么在上层
//! （`kiana-coding-agent`、`kiana-interactive`、`kiana-resources`），放在 `kiana-config` 不会造成反向依赖，
//! 且 `radius.rs` 需要 `kiana-ai` 的 `providers::radius_config`（`kiana-core` 不应承担该语义）。
//!
//! 逐项移植（`ca-config-models.md` §2.1-§2.4）：
//!
//! - [`DEFAULT_THINKING_LEVEL`]（`"medium"`）、[`THINKING_LEVEL_OPTIONS`]（含 `"off"`，7 个）；
//! - [`are_experimental_features_enabled`]（`KIANA_EXPERIMENTAL === "1"`）；
//! - [`RADIUS_PROVIDER_ID`]、[`ENV_RADIUS_GATEWAY`]、[`RADIUS_MCP_URL`]、
//!   [`get_radius_gateway_url`]；
//! - [`is_install_telemetry_enabled`]（`KIANA_TELEMETRY` 与 settings 的优先级）。
//!
//! # 这个文件为什么存在
//! 这些符号大多是纯常量/纯函数（`RADIUS_MCP_URL` 是带进程级惰性缓存的静态，另有三个
//! 函数读取环境变量），但读者分散在多个 crate：`kiana-config` 的模型解析器、
//! `kiana-session`（默认 thinking level）、`kiana-interactive`（设置界面、footer、安装
//! 遥测上报、Radius 登录）、`kiana-coding-agent`（首次设置界面）与 `kiana-resources`
//! （bug 报告里的网关地址）。集中到一处可以避免每个 crate 各存一份默认值而逐渐漂移。
//!
//! # 类型上的一个坑
//! 本文件的 `ThinkingLevel` 来自 `kiana_agent_core`，而 `kiana_agent_core::ThinkingLevel`
//! 是 `kiana_ai::types::ModelThinkingLevel` 的再导出：**7 个值、包含 `"off"`**。它与
//! `kiana_ai::types::ThinkingLevel`（6 个值、不含 `"off"`，用于请求的 `reasoning`）不是
//! 同一个类型，不能互相替换。
//!
//! # 环境变量的读取时机
//! - `KIANA_EXPERIMENTAL`：每次调用 [`are_experimental_features_enabled`] 都重新读取，
//!   只认 `"1"`，大小写敏感。
//! - `KIANA_TELEMETRY`：每次调用 [`is_install_telemetry_enabled`] 都重新读取；只要
//!   **已定义**（含空串）就完全覆盖 settings。
//! - `KIANA_RADIUS_GATEWAY`：每次调用 [`get_radius_gateway_url`] 都重新读取；而
//!   [`RADIUS_MCP_URL`] 由**默认**网关在首次访问时算出，不读该变量。
//!
//! 三处都用 `std::env::var`：变量缺失或值不是合法 UTF-8 时得到 `Err`，此时
//! `are_experimental_features_enabled` 返回 `false`，`is_install_telemetry_enabled`
//! 回落到 settings，`get_radius_gateway_url` 使用默认网关。
//!
//! # 测试
//! `tests/defaults.rs` 与 `tests/experimental.rs` 锁定上述取值与优先级；测试用互斥锁
//! 串行化同一测试二进制内的环境变量读写。

use std::sync::LazyLock;

use kiana_agent_core::ThinkingLevel;
use kiana_ai::providers::radius_config::{DEFAULT_RADIUS_GATEWAY, normalize_radius_gateway_url};

use crate::settings::SettingsManager;

/// `DEFAULT_THINKING_LEVEL`：`"medium"`。
///
/// 仅作为缺省值：用户或模型没有显式档位时，模型解析器与 UI 回退到它。类型是含 `"off"`
/// 的 7 值枚举，`Medium` 只是其中一个档位，不是“开启/关闭推理”的布尔开关。
pub const DEFAULT_THINKING_LEVEL: ThinkingLevel = ThinkingLevel::Medium;

/// `THINKING_LEVEL_OPTIONS`：UI 展示顺序（含 `"off"`，顺序不可改）。
///
/// 顺序从关闭到最强：`off`（关闭）、`minimal`、`low`、`medium`、`high`、`xhigh`、
/// `max`；UI 按数组顺序渲染档位列表。测试 `thinking_level_options_keep_ui_order` 逐项
/// 锁定，增删或重排会让设置界面与 TS 的顺序不一致。
pub const THINKING_LEVEL_OPTIONS: [ThinkingLevel; 7] = [
    ThinkingLevel::Off,
    ThinkingLevel::Minimal,
    ThinkingLevel::Low,
    ThinkingLevel::Medium,
    ThinkingLevel::High,
    ThinkingLevel::Xhigh,
    ThinkingLevel::Max,
];

/// `areExperimentalFeaturesEnabled()`：只认字符串 `"1"`；`"0"` / `""` / `"true"` 都是 `false`。
///
/// 每次调用都读进程环境，不缓存；大小写敏感（`"TRUE"` 也是 `false`），读取失败（变量
/// 缺失或不是合法 UTF-8）同样返回 `false`。当前用于 footer 的 `xp` 标记与首次设置界面
/// 的实验特性选项。
pub fn are_experimental_features_enabled() -> bool {
    std::env::var("KIANA_EXPERIMENTAL").is_ok_and(|value| value == "1")
}

/// `RADIUS_PROVIDER_ID`。
///
/// 内置 Radius provider 的 id；`radius_provider` 在未指定 `id` 时也使用同一字面量。
/// 调用方用它查找或比较 provider（例如判断登录/分享流程是否针对 Radius），不是环境
/// 变量名。
pub const RADIUS_PROVIDER_ID: &str = "radius";

/// `ENV_RADIUS_GATEWAY`。
///
/// 覆盖 Radius 网关地址的环境变量名。只有 [`get_radius_gateway_url`] 读取它；
/// [`RADIUS_MCP_URL`] 始终基于默认网关，不受该变量影响。
pub const ENV_RADIUS_GATEWAY: &str = "KIANA_RADIUS_GATEWAY";

/// `RADIUS_MCP_URL`：内置 Radius provider 登录的网关 MCP 端点。
///
/// TS 在模块加载时求值一次；Rust 用 [`LazyLock`] 保持同样的“进程内固定”语义。
///
/// 它由**默认**网关 `DEFAULT_RADIUS_GATEWAY` 规范化后加 `/mcp` 得到，因此
/// `KIANA_RADIUS_GATEWAY` 不会改变它；默认值 `https://radius.kiana.dev` 没有尾随 `/`，
/// 结果是 `https://radius.kiana.dev/mcp`。交互式 `/login` 流程用它比较或构造网关 MCP
/// 服务器配置。
pub static RADIUS_MCP_URL: LazyLock<String> = LazyLock::new(|| {
    format!(
        "{}/mcp",
        normalize_radius_gateway_url(DEFAULT_RADIUS_GATEWAY)
    )
});

/// `getRadiusGatewayUrl()`：每次调用都读 `KIANA_RADIUS_GATEWAY`（不缓存）。
///
/// 变量缺失或不是合法 UTF-8 时使用 `DEFAULT_RADIUS_GATEWAY`；随后经
/// `normalize_radius_gateway_url` 处理：没有 `http(s)://` 前缀就补 `https://`，并删除
/// **所有**尾随 `/`。它不做主机名或路径校验，非法字符串也会被补上前缀原样返回。调用方
/// （例如 bug 报告）把它直接当作网关地址使用。
pub fn get_radius_gateway_url() -> String {
    let value =
        std::env::var(ENV_RADIUS_GATEWAY).unwrap_or_else(|_| DEFAULT_RADIUS_GATEWAY.to_string());
    normalize_radius_gateway_url(&value)
}

/// `isTruthyEnvFlag(value)`：`"1"` / `"true"` / `"yes"`（大小写不敏感）。
///
/// `None`、空串以及除 `"1"`/`"true"`/`"yes"` 外的其它字符串均为 `false`，包括
/// `"0"`、`"no"`、`"on"`、`"2"`。不做
/// trim，因此 `" true"` 也是 `false`——真值只有这三个字面量（大小写折叠后比较）。
fn is_truthy_env_flag(value: Option<&str>) -> bool {
    let Some(value) = value else {
        return false;
    };
    if value.is_empty() {
        return false;
    }
    value == "1" || value.to_lowercase() == "true" || value.to_lowercase() == "yes"
}

/// `isInstallTelemetryEnabled(settingsManager)`：`KIANA_TELEMETRY` 只要**已定义**（含空串）
/// 就完全覆盖设置；空串 → `false`。
///
/// 每次调用都读环境变量，不缓存。变量缺失（或值不是合法 UTF-8，`std::env::var` 返回
/// `Err`）时回落到 `settingsManager.get_enable_install_telemetry()`。调用方包括客户端
/// 身份构造与交互式启动时的安装遥测上报，两者都以“本次调用”的结果为准。
pub fn is_install_telemetry_enabled(settings_manager: &SettingsManager) -> bool {
    let telemetry_env = std::env::var("KIANA_TELEMETRY").ok();
    is_install_telemetry_enabled_with_env(settings_manager, telemetry_env.as_deref())
}

/// [`is_install_telemetry_enabled`] 的显式环境值版本（测试用）：`None` = 未定义，
/// 此时回落到 settings；`Some("")` = 已定义但为空 → `false`。
///
/// 本函数不读进程环境：只要传入 `Some`，任何值（包括 `"0"` 与空串）都会完全覆盖
/// settings。把 `None` 与 `Some("")` 区分开正是它存在的理由——前者回落，后者强制关闭。
pub fn is_install_telemetry_enabled_with_env(
    settings_manager: &SettingsManager,
    telemetry_env: Option<&str>,
) -> bool {
    match telemetry_env {
        Some(value) => is_truthy_env_flag(Some(value)),
        None => settings_manager.get_enable_install_telemetry(),
    }
}
