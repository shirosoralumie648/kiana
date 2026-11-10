//! core/auth-guidance.ts。
//!
//! TS: `packages/coding-agent/src/core/auth-guidance.ts`（25 行）。规范：
//! `spec/ca-config-models.md` §2.6；四条文案**逐字**（文档路径取 [`get_docs_path`]，
//! 即 `<package-dir>/docs`）。
//!
//! ```text
//! Use /login to log into a provider via OAuth or API key. See:
//!   <docs>/providers.md
//!   <docs>/models.md
//! ```
//!
//! # 这个文件为什么存在
//! “没有可用模型 / 没有选中模型 / 没有 API key”这三类提示会出现在 CLI 的 stdout、
//! stderr 与错误类型的 `Display` 里。把文案与文档路径的拼接集中在本文件，所有前端才能
//! 给出同一段指引；否则每个调用点都要自己拼 `<docs>/providers.md`，很容易逐渐漂移。
//!
//! # 被谁调用
//! - `kiana-session::sdk`：选不到初始模型时，用 [`format_no_models_available_message`]
//!   作为回退提示。
//! - `kiana-session::agent_session`：`AgentSessionError::NoModelSelected` 与
//!   `AgentSessionError::NoApiKey` 的 `Display` 分别调用
//!   [`format_no_model_selected_message`] 与 [`format_no_api_key_found_message`]。
//! - `kiana-coding-agent`：`list_models` 把“无模型”提示写到 stdout；非交互模式缺少模型
//!   时写到 stderr 并返回退出码 1。
//!
//! # 执行流程与约束
//! 1. 每次调用都重新执行 `get_docs_path()`（不缓存路径），再与 `providers.md` /
//!    `models.md` 拼接。
//! 2. 返回的字符串以 `\n` 分行、**不带末尾换行**；是否换行、是否着色由调用方的输出
//!    接口决定。
//! 3. 文案是**逐字契约**：`tests/auth_guidance.rs` 按完整展开断言，改动空格、空行或
//!    标点都会让测试失败，并偏离 `spec/ca-config-models.md` §2.6 的 TS 文本。
//!
//! # 失败模式
//! 这里不会检查 `providers.md` / `models.md` 是否存在，也不读文件：即使文档缺失，提示
//! 仍会打印 `get_docs_path()` 给出的路径。路径的定位逻辑在 `kiana-core`，本模块只做
//! 字符串拼接。

use kiana_core::config::get_docs_path;

/// `UNKNOWN_PROVIDER`：`formatNoApiKeyFoundMessage` 的哨兵值。
///
/// 只有调用方传入的值与 `"unknown"` **完全相等**时才触发
/// [`format_no_api_key_found_message`] 的 `the selected model` 替换；`"Unknown"`、空串、
/// 带空白或换行的值都原样显示。它是 [`format_no_api_key_found_message`] 与 provider
/// 字符串做相等比较时使用的哨兵；除该函数外，仓库内没有其它代码引用它，测试只断言
/// 其字面量为 `"unknown"`。
pub const UNKNOWN_PROVIDER: &str = "unknown";

/// `path.join(getDocsPath(), name)`。
///
/// `getDocsPath()` 已经过 `path.resolve`；这里用 [`PathBuf::join`]（Node `join` 在 posix
/// 上与本实现一致；Windows 不在验收矩阵内）。
///
/// 返回值只用于拼进用户可见文案：`to_string_lossy()` 会把非 UTF-8 的路径段替换掉，
/// 并且这里不检查文件是否存在。`docs_file` 是仅在本模块内使用的私有辅助函数。
fn docs_file(name: &str) -> String {
    get_docs_path().join(name).to_string_lossy().into_owned()
}

/// `getProviderLoginHelp()`：登录提示（三行）.
///
/// 第一行是固定提示，后两行是缩进两个空格的文档路径；三行以 `\n` 分隔，**无末尾换行**。
/// 每次调用都重新计算 `get_docs_path()`，不在进程内缓存。
///
/// TS: `core/auth-guidance.ts#getProviderLoginHelp`
pub fn get_provider_login_help() -> String {
    [
        "Use /login to log into a provider via OAuth or API key. See:".to_string(),
        format!("  {}", docs_file("providers.md")),
        format!("  {}", docs_file("models.md")),
    ]
    .join("\n")
}

/// `formatNoModelsAvailableMessage()`。
///
/// 在登录提示前加 `"No models available. "`（一个空格分隔），整段仍无末尾换行；用于
/// 一个模型都解析不到的场景。
///
/// TS: `core/auth-guidance.ts#formatNoModelsAvailableMessage`
pub fn format_no_models_available_message() -> String {
    format!("No models available. {}", get_provider_login_help())
}

/// `formatNoModelSelectedMessage()`。
///
/// 输出三段：`"No model selected."`、登录提示、`"Then use /model to select a model."`，
/// 段与段之间各有一个空行（`\n\n`）。
///
/// TS: `core/auth-guidance.ts#formatNoModelSelectedMessage`
pub fn format_no_model_selected_message() -> String {
    format!(
        "No model selected.\n\n{}\n\nThen use /model to select a model.",
        get_provider_login_help()
    )
}

/// `formatNoApiKeyFoundMessage(provider)`：`"unknown"` 显示为 `the selected model`。
///
/// `provider` 原样插入，不做 trim、转义或大小写归一；唯一特判是完全等于
/// [`UNKNOWN_PROVIDER`] 时替换成 `the selected model`。provider 含换行或首尾空格时会被
/// 原样带进文案（测试 `provider_name_is_interpolated_verbatim` 锁定该行为）。
///
/// 与 [`format_no_model_selected_message`] 是两条不同文案：这里针对“已选模型但取不到
/// API key”，不要为省事互相复用。
///
/// TS: `core/auth-guidance.ts#formatNoApiKeyFoundMessage`
pub fn format_no_api_key_found_message(provider: &str) -> String {
    let provider_display = if provider == UNKNOWN_PROVIDER {
        "the selected model"
    } else {
        provider
    };
    format!(
        "No API key found for {provider_display}.\n\n{}",
        get_provider_login_help()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn docs(name: &str) -> String {
        docs_file(name)
    }

    #[test]
    fn login_help_is_three_verbatim_lines() {
        assert_eq!(
            get_provider_login_help(),
            format!(
                "Use /login to log into a provider via OAuth or API key. See:\n  {}\n  {}",
                docs("providers.md"),
                docs("models.md")
            )
        );
    }

    #[test]
    fn no_models_message_prefixes_the_help() {
        assert_eq!(
            format_no_models_available_message(),
            format!("No models available. {}", get_provider_login_help())
        );
    }

    #[test]
    fn no_model_selected_message_wraps_the_help() {
        assert_eq!(
            format_no_model_selected_message(),
            format!(
                "No model selected.\n\n{}\n\nThen use /model to select a model.",
                get_provider_login_help()
            )
        );
    }

    #[test]
    fn unknown_provider_uses_the_selected_model_wording() {
        assert_eq!(
            format_no_api_key_found_message("unknown"),
            format!(
                "No API key found for the selected model.\n\n{}",
                get_provider_login_help()
            )
        );
        assert!(
            format_no_api_key_found_message("anthropic")
                .starts_with("No API key found for anthropic.\n\n")
        );
    }
}
