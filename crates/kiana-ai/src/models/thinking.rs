//! clampThinkingLevel / getSupportedThinkingLevels。
//!
//! TS: `src/models.ts`。`EXTENDED_THINKING_LEVELS` 的顺序即钳制方向：
//! `["off", "minimal", "low", "medium", "high", "xhigh", "max"]`。
//!
//! # 这两个函数解决什么问题
//!
//! 模型目录用 `reasoning` + `thinkingLevelMap` 描述“这个模型支持哪些思考档位、每档映射到
//! 服务商的什么取值”。调用方（各 provider 适配器）不应该自己猜，而是先用
//! [`clamp_thinking_level`] 把用户请求的档位收敛到模型声明的范围，再查映射表构造请求；
//! UI 与会话层则用 [`get_supported_thinking_levels`] 展示/校验可选档位。
//! 两者都是纯函数：不读环境、不发请求、没有错误分支，也不修改模型。
//!
//! # 不可变约束
//!
//! [`EXTENDED_THINKING_LEVELS`] 的顺序同时是“强弱顺序”和“索引空间”：
//! [`clamp_thinking_level`] 用它定位请求档位，再向更高/更低方向搜索。若给
//! [`ModelThinkingLevel`] 增加变体却忘记按强弱顺序加入本数组，`position` 会返回 `None`，
//! 该档位会被当成“运行期未知档位”直接回退到最低可用档位——不报错，属于静默降级。
//!
//! # 一个反直觉的边界
//!
//! `reasoning = Some(true)` 且 `thinkingLevelMap` 把全部档位都显式标成 `null` 时，
//! 可用列表是**空**的；[`clamp_thinking_level`] 此时返回 `Off`，并不代表模型真的支持
//! `Off`。调用方应把空列表视为“配置自相矛盾”，而不是“支持关闭思考”。

use crate::types::{Model, ModelThinkingLevel};

/// `EXTENDED_THINKING_LEVELS`（顺序即钳制顺序）。
///
/// 从弱到强排列（`off` 最弱）。[`clamp_thinking_level`] 的搜索方向与
/// [`get_supported_thinking_levels`] 的返回顺序都依赖它；新增档位时必须同步维护本数组
/// （否则会退化为“运行期未知档位”，见模块文档）。
pub const EXTENDED_THINKING_LEVELS: [ModelThinkingLevel; 7] = [
    ModelThinkingLevel::Off,
    ModelThinkingLevel::Minimal,
    ModelThinkingLevel::Low,
    ModelThinkingLevel::Medium,
    ModelThinkingLevel::High,
    ModelThinkingLevel::Xhigh,
    ModelThinkingLevel::Max,
];

/// 模型支持的 kiana thinking level 列表（按扩展顺序）。
///
/// `!model.reasoning` → 仅 `["off"]`；否则过滤：`thinkingLevelMap[level] === null` 排除；
/// `xhigh`/`max` 还要求映射存在（必须显式声明）。
///
/// 规则细节：
///
/// - `reasoning` 为 `None` 或 `Some(false)` 都按“不支持推理”处理，只返回 `Off`；
/// - `thinkingLevelMap` 为 `None` 时，除 `xhigh`/`max` 外的档位全部可用；
/// - 映射值为 `Some(Some(value))` 表示显式支持，`Some(None)`（JSON `null`）表示显式禁用；
/// - 未映射的 `xhigh`/`max` 视为禁用（这两个档位必须显式声明），其余档位未映射默认可用。
///
/// 返回值**可能为空**：所有档位都被显式 `null` 禁用时（见模块文档）。调用方不得假设
/// 列表至少含 `Off`。
///
/// TS: `src/models.ts#getSupportedThinkingLevels`
pub fn get_supported_thinking_levels(model: &Model) -> Vec<ModelThinkingLevel> {
    if model.reasoning != Some(true) {
        return vec![ModelThinkingLevel::Off];
    }
    EXTENDED_THINKING_LEVELS
        .iter()
        .copied()
        .filter(|level| {
            let mapped = model
                .thinking_level_map
                .as_ref()
                .and_then(|map| map.get(level));
            if matches!(mapped, Some(None)) {
                return false;
            }
            if matches!(level, ModelThinkingLevel::Xhigh | ModelThinkingLevel::Max) {
                return mapped.is_some();
            }
            true
        })
        .collect()
}

/// 把请求的 thinking level 钳制到模型支持的最接近档位。
///
/// 已支持则原样返回；未知档位返回 `available[0] ?? "off"`；否则先向更高档找，再向更低档找。
///
/// 算法与 TS 逐分支对应：
///
/// 1. 请求档位在可用列表中 → 直接返回；
/// 2. 请求档位不在 [`EXTENDED_THINKING_LEVELS`] 中 → 返回可用列表第一项或 `Off`。
///    Rust 的 [`ModelThinkingLevel`] 是封闭枚举，正常调用走不到这里；保留该分支是为了
///    与 TS 处理运行期未知档位的行为一致；
/// 3. 从请求档位的位置向**更高**方向找第一个可用档位（切片含自身，但自身已确认不可用）；
/// 4. 找不到再向**更低**方向找（切片不含自身）；
/// 5. 仍然没有（可用列表为空）→ `Off`。
///
/// 因此结果优先“升级”而不是“就近”：只要有更高的可用档位，就不会选择更低的。
/// 函数不会 panic，也不会返回 `None`；返回的 `Off` 不保证在模型的支持列表里。
///
/// TS: `src/models.ts#clampThinkingLevel`
pub fn clamp_thinking_level(model: &Model, level: ModelThinkingLevel) -> ModelThinkingLevel {
    let available = get_supported_thinking_levels(model);
    // 快速路径：请求档位已被模型支持。
    if available.contains(&level) {
        return level;
    }
    let Some(requested_index) = EXTENDED_THINKING_LEVELS.iter().position(|l| *l == level) else {
        // 封闭枚举下不可达；对应 TS 的“运行期未知 level”分支。
        return available
            .first()
            .copied()
            .unwrap_or(ModelThinkingLevel::Off);
    };
    // 先升后降：与 TS 顺序一致，保证有更高档可用时绝不选更低档。
    for candidate in &EXTENDED_THINKING_LEVELS[requested_index..] {
        if available.contains(candidate) {
            return *candidate;
        }
    }
    for candidate in EXTENDED_THINKING_LEVELS[..requested_index].iter().rev() {
        if available.contains(candidate) {
            return *candidate;
        }
    }
    // 可用列表为空（全部档位被显式禁用）时的兜底。
    available
        .first()
        .copied()
        .unwrap_or(ModelThinkingLevel::Off)
}

#[cfg(test)]
mod tests {
    use super::*;
    use indexmap::IndexMap;

    fn reasoning_model(map: Option<IndexMap<ModelThinkingLevel, Option<String>>>) -> Model {
        Model {
            reasoning: Some(true),
            thinking_level_map: map,
            ..Model::default()
        }
    }

    #[test]
    fn non_reasoning_models_support_only_off() {
        let model = Model::default();
        assert_eq!(
            get_supported_thinking_levels(&model),
            vec![ModelThinkingLevel::Off]
        );
        assert_eq!(
            clamp_thinking_level(&model, ModelThinkingLevel::High),
            ModelThinkingLevel::Off
        );
    }

    #[test]
    fn xhigh_and_max_require_explicit_mappings() {
        let mut map = IndexMap::new();
        map.insert(ModelThinkingLevel::Off, Some("none".into()));
        map.insert(ModelThinkingLevel::High, Some("high".into()));
        map.insert(ModelThinkingLevel::Max, Some("max".into()));
        map.insert(ModelThinkingLevel::Xhigh, None);
        let model = reasoning_model(Some(map));
        assert_eq!(
            get_supported_thinking_levels(&model),
            vec![
                ModelThinkingLevel::Off,
                ModelThinkingLevel::Minimal,
                ModelThinkingLevel::Low,
                ModelThinkingLevel::Medium,
                ModelThinkingLevel::High,
                ModelThinkingLevel::Max,
            ]
        );
    }

    #[test]
    fn clamps_up_then_down() {
        let mut map = IndexMap::new();
        map.insert(ModelThinkingLevel::Off, None);
        map.insert(ModelThinkingLevel::Minimal, None);
        map.insert(ModelThinkingLevel::Low, Some("low".into()));
        map.insert(ModelThinkingLevel::Medium, None);
        map.insert(ModelThinkingLevel::High, Some("high".into()));
        let model = reasoning_model(Some(map));
        assert_eq!(
            clamp_thinking_level(&model, ModelThinkingLevel::Medium),
            ModelThinkingLevel::High
        );
        assert_eq!(
            clamp_thinking_level(&model, ModelThinkingLevel::Max),
            ModelThinkingLevel::High
        );
        assert_eq!(
            clamp_thinking_level(&model, ModelThinkingLevel::Off),
            ModelThinkingLevel::Low
        );
    }

    #[test]
    fn absent_map_supports_every_level() {
        let model = reasoning_model(None);
        // `ModelThinkingLevel` 是封闭枚举，不存在「运行时未知档位」；未映射的档位全部可用
        // （只有 `null` 显式禁用，`xhigh`/`max` 需要显式映射）。
        // 未映射的 `xhigh`/`max` 不可用：向更低档回退到 `high`。
        assert_eq!(
            clamp_thinking_level(&model, ModelThinkingLevel::Xhigh),
            ModelThinkingLevel::High
        );
        assert_eq!(
            clamp_thinking_level(&model, ModelThinkingLevel::Off),
            ModelThinkingLevel::Off
        );
    }
}
