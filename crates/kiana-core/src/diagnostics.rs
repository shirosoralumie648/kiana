//! `core/diagnostics.ts`：`ResourceCollision` / `ResourceDiagnostic`。
//!
//! 定义在此（而非 `kiana-resources`）是因为 `kiana-extensions::runner` 与 `kiana-interactive` 都要用，
//! 而它们位于 `kiana-resources` 之下（`ARCHITECTURE.md` §3.5）。`kiana-resources::diagnostics` 只再导出。
//!
//! TS: `packages/coding-agent/src/core/diagnostics.ts`。
//!
//! ## 序列化契约（冻结）
//!
//! 这些类型带 `Serialize`/`Deserialize`，序列化形状按 TS 对象形状锁定：
//!
//! - `#[serde(rename_all = "camelCase")]`：`resource_type` → `"resourceType"`、
//!   `winner_path` → `"winnerPath"` 等；`ResourceDiagnostic.kind` 额外用
//!   `#[serde(rename = "type")]` 映射回 TS 的 `"type"`。
//! - 枚举字面量全小写：`"extension"`/`"skill"`/`"prompt"`/`"theme"`、
//!   `"warning"`/`"error"`/`"collision"`。
//! - `Option` 字段为 `None` 时不输出该键；反序列化时缺失键同样合法。
//!
//! 其中 `ResourceCollision`/`ResourceDiagnostic` 还经 `kiana-coding-agent::sdk_exports`
//! 再导出，属于公开契约（与 `spec/ca-resources.md` 的
//! `export type { ResourceCollision, ResourceDiagnostic }` 一致）；`ResourceType` /
//! `DiagnosticKind` 不在该再导出面上，只经 `kiana-core` 与 `kiana-resources::diagnostics`
//! 公开。
//!
//! `crates/kiana-core/tests/diagnostics.rs` 与文件内 `tests` 都用**整串 JSON** 断言键序和
//! 可选字段的省略，改名/重排字段/调整 rename 都会让这些断言失败；
//! `ResourceCollision`/`ResourceDiagnostic` 的再导出同样是对 SDK 使用者的契约。

use serde::{Deserialize, Serialize};

/// 冲突资源的种类。TS: `diagnostics.ts#ResourceCollision.resourceType`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResourceType {
    /// `"extension"`。
    Extension,
    /// `"skill"`。
    Skill,
    /// `"prompt"`。
    Prompt,
    /// `"theme"`。
    Theme,
}

/// 同名资源冲突：两个来源都声明了同一个名字，赢家生效、输家被忽略。
/// TS: `diagnostics.ts#ResourceCollision`。
///
/// 字段顺序是序列化契约：`resourceType`、`name`、`winnerPath`、`loserPath`、`winnerSource`、
/// `loserSource`。`name` 是**必须**字段（技能名、命令/工具/flag 名、prompt 名或主题名）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceCollision {
    /// 资源种类。
    pub resource_type: ResourceType,
    /// 冲突的名字。
    pub name: String,
    /// 生效资源的路径。
    pub winner_path: String,
    /// 被忽略资源的路径。
    pub loser_path: String,
    /// 生效资源的来源标签（如 `"npm:foo"`、`"git:..."`、`"local"`）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub winner_source: Option<String>,
    /// 被忽略资源的来源标签。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub loser_source: Option<String>,
}

/// 诊断级别。TS: `diagnostics.ts#ResourceDiagnostic.type`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiagnosticKind {
    /// `"warning"`。
    Warning,
    /// `"error"`。
    Error,
    /// `"collision"`。
    Collision,
}

/// 资源加载诊断。`message` 是**逐字**用户可见文本。
/// TS: `diagnostics.ts#ResourceDiagnostic`。
///
/// 字段顺序是序列化契约：`type`、`message`、`path`、`collision`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceDiagnostic {
    /// 诊断级别。
    #[serde(rename = "type")]
    pub kind: DiagnosticKind,
    /// 用户可见文本（逐字）。
    pub message: String,
    /// 相关路径。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// 冲突详情（仅 `type == "collision"` 时存在）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub collision: Option<ResourceCollision>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collision_serializes_in_ts_field_order() {
        let collision = ResourceCollision {
            resource_type: ResourceType::Skill,
            name: "review".into(),
            winner_path: "/a/review".into(),
            loser_path: "/b/review".into(),
            winner_source: Some("local".into()),
            loser_source: None,
        };
        let json = serde_json::to_string(&collision).expect("serialize");
        assert_eq!(
            json,
            r#"{"resourceType":"skill","name":"review","winnerPath":"/a/review","loserPath":"/b/review","winnerSource":"local"}"#
        );
    }

    #[test]
    fn diagnostic_omits_absent_optional_fields() {
        let diagnostic = ResourceDiagnostic {
            kind: DiagnosticKind::Warning,
            message: "boom".into(),
            path: None,
            collision: None,
        };
        assert_eq!(
            serde_json::to_string(&diagnostic).expect("serialize"),
            r#"{"type":"warning","message":"boom"}"#
        );

        let with_collision = ResourceDiagnostic {
            kind: DiagnosticKind::Collision,
            message: "dup".into(),
            path: Some("/x".into()),
            collision: Some(ResourceCollision {
                resource_type: ResourceType::Theme,
                name: "dark".into(),
                winner_path: "/w".into(),
                loser_path: "/l".into(),
                winner_source: None,
                loser_source: Some("npm:foo".into()),
            }),
        };
        assert_eq!(
            serde_json::to_string(&with_collision).expect("serialize"),
            r#"{"type":"collision","message":"dup","path":"/x","collision":{"resourceType":"theme","name":"dark","winnerPath":"/w","loserPath":"/l","loserSource":"npm:foo"}}"#
        );
    }

    #[test]
    fn deserializes_all_type_strings() {
        for (literal, kind) in [
            ("warning", DiagnosticKind::Warning),
            ("error", DiagnosticKind::Error),
            ("collision", DiagnosticKind::Collision),
        ] {
            let value: ResourceDiagnostic =
                serde_json::from_str(&format!(r#"{{"type":"{literal}","message":"m"}}"#))
                    .expect("deserialize");
            assert_eq!(value.kind, kind);
        }
    }
}
