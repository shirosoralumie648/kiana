//! `collectSettingsDiagnostics` / `deduplicateDiagnostics`（settings 诊断）。
//!
//! TS: `packages/coding-agent/src/core/settings-diagnostics.ts`。
//!
//! TS 里这两个函数返回/接收 `AgentSessionRuntimeDiagnostic`
//! （`{ type: "info" | "warning" | "error", message: string }`，定义在
//! `core/agent-session-services.ts`）。该类型**不在** `ARCHITECTURE.md` §3.5 的契约上移
//! 清单里，而 `kiana-config`（L5）不能依赖 `kiana-session`（L11）。因此本模块定义：
//!
//! - [`DiagnosticKind`] 与本地 trait [`DiagnosticLike`]；
//! - [`SettingsDiagnostic`]（settings 子系统的诊断，实现 `DiagnosticLike`）；
//! - 泛型的 [`deduplicate_diagnostics`]，`kiana-session` 可以为自己的
//!   `AgentSessionRuntimeDiagnostic` 实现 [`DiagnosticLike`]，再用同一函数去重混合列表。
//!
//! 若架构者以后把 `RuntimeDiagnostic` 上移到 `kiana-core`（§3.5），把 [`SettingsDiagnostic`]
//! 换成再导出即可。
//!
//! # 顺序与消费语义
//!
//! [`collect_settings_diagnostics`] 调用 [`SettingsManager::drain_errors`]，因此它**取走**
//! 并清空 manager 当前的错误列表：同一批错误只会被收集一次，之后再次调用只会看到新产生
//! 的错误。输出顺序与错误产生顺序一致（`drain_errors` 用 `mem::take` 取出整个 `Vec`）。
//!
//! [`deduplicate_diagnostics`] 同样保留**首次出现**的顺序：`HashSet` 只负责判重，结果
//! 按输入顺序压入 `Vec`。键是 `type` 与 `message` 的组合，所以同一文案的 warning 与
//! error 不会被合并（测试 `deduplicates_diagnostics_by_type_and_message` 覆盖）。

use crate::settings::SettingsManager;

/// `AgentSessionRuntimeDiagnostic["type"]`（`"info" | "warning" | "error"`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiagnosticKind {
    /// `"info"`。
    Info,
    /// `"warning"`。
    Warning,
    /// `"error"`。
    Error,
}

impl DiagnosticKind {
    /// TS 的字面量。
    pub fn as_str(self) -> &'static str {
        match self {
            DiagnosticKind::Info => "info",
            DiagnosticKind::Warning => "warning",
            DiagnosticKind::Error => "error",
        }
    }
}

/// 诊断对象的最小接口：类型与文案。
///
/// `kiana-session` 的 `AgentSessionRuntimeDiagnostic` 实现此 trait 后即可与
/// [`SettingsDiagnostic`] 一起传给 [`deduplicate_diagnostics`]。
pub trait DiagnosticLike {
    /// `diagnostic.type`。
    fn diagnostic_kind(&self) -> DiagnosticKind;
    /// `diagnostic.message`。
    fn diagnostic_message(&self) -> &str;
}

/// settings 子系统的诊断（TS: `{ type: "warning", message }`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsDiagnostic {
    /// `type`。
    pub kind: DiagnosticKind,
    /// `message`。
    pub message: String,
}

impl DiagnosticLike for SettingsDiagnostic {
    fn diagnostic_kind(&self) -> DiagnosticKind {
        self.kind
    }

    fn diagnostic_message(&self) -> &str {
        &self.message
    }
}

/// `collectSettingsDiagnostics(settingsManager)`：把 `drainErrors()` 的错误转成 warning。
///
/// 逐字文案：
/// - 有 `path` → `Invalid settings file ${path}: ${error.message}`
/// - 无 `path` → `Invalid ${scope} settings: ${error.message}`
pub fn collect_settings_diagnostics(settings_manager: &SettingsManager) -> Vec<SettingsDiagnostic> {
    settings_manager
        .drain_errors()
        .into_iter()
        .map(|error| SettingsDiagnostic {
            kind: DiagnosticKind::Warning,
            // 空路径等同于没有路径：`fromStorage` 构造的 manager 两个路径都是 `None`，
            // 只有 `create*` 才知道对应的文件名。
            message: match &error.path {
                Some(path) if !path.is_empty() => {
                    format!("Invalid settings file {path}: {}", error.error)
                }
                _ => format!("Invalid {} settings: {}", error.scope.as_str(), error.error),
            },
        })
        .collect()
}

/// `deduplicateDiagnostics(diagnostics)`：按 `type\0message` 去重，保留首次出现。
pub fn deduplicate_diagnostics<T: DiagnosticLike + Clone>(diagnostics: &[T]) -> Vec<T> {
    let mut seen = std::collections::HashSet::new();
    let mut result = Vec::with_capacity(diagnostics.len());
    for diagnostic in diagnostics {
        // 键里带上 `type`：只按 message 去重会把同文案的 warning/error 合并掉。
        // 用 NUL 作分隔符：type 是固定词表，message 是任意文本，分隔符用于降低拼接歧义。
        let key = format!(
            "{}\0{}",
            diagnostic.diagnostic_kind().as_str(),
            diagnostic.diagnostic_message()
        );
        if seen.insert(key) {
            result.push(diagnostic.clone());
        }
    }
    result
}
