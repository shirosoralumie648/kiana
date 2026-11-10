//! `core/settings-manager.ts` 的工具选择助手。
//!
//! 逐字移植 `settings-manager.ts` 的以下导出：
//!
//! - [`DEFAULT_TOOL_NAMES`]：`defaultTools` 未改变选择时的启动工具；
//! - [`is_tool_modifier`]：条目是否为 `+name` / `-name` 修饰符；
//! - [`get_tool_list_error`]：校验 `--tools` / SDK `tools` 列表；
//! - [`apply_tool_modifiers`]：按顺序应用 `+name` / `-name`。
//!
//! 以及两个辅助函数（TS 里是模块私有；Rust 侧 [`merge_default_tools`] 是 `pub`，
//! `tests/default_tools_setting.rs` 直接调用它；`merge_default_tools_raw` 是 `pub(crate)`，
//! 只给 `settings::mod` 的 `deep_merge_settings` 使用）：
//!
//! - [`merge_default_tools`]：合并 global/project 两层的 `defaultTools`；
//! - [`resolve_default_tools`]：把合并后的列表解析成最终工具名列表。
//!
//! # 两套入口的语义差异
//!
//! settings 层合并（project 覆盖 global）时，`defaultTools` 允许「纯 `+name` / `-name`
//! 列表」追加到继承列表，不做混用校验；而调用方传入的工具列表（`--tools` / SDK
//! `tools`）必须是纯名单或纯修饰符，混用在 [`get_tool_list_error`] 处直接报错。
//! [`resolve_default_tools`] 再把最终列表解析成实际工具名：纯名单整体替换
//! [`DEFAULT_TOOL_NAMES`]，纯修饰符从内置默认起步，空列表得到空选择。
//!
//! TS: `packages/coding-agent/src/core/settings-manager.ts`

use serde_json::Value;

/// Tools enabled at startup when `defaultTools` does not change them.
///
/// TS: `DEFAULT_TOOL_NAMES`。
pub const DEFAULT_TOOL_NAMES: [&str; 4] = ["read", "bash", "edit", "write"];

/// Whether a tool selection entry is a `+name` or `-name` modifier.
///
/// TS: `isToolModifier(entry)`（`entry` 的 `unknown` 类型在 Rust 侧收窄成 `&str`；
/// 非字符串条目由 `is_tool_modifier_value` 处理；后者是 `pub(crate)`，故用行内代码而非
/// rustdoc 链接）。
pub fn is_tool_modifier(entry: &str) -> bool {
    entry.starts_with('+') || entry.starts_with('-')
}

/// [`is_tool_modifier`] 的 JSON 值版本：`typeof entry === "string" && ...`。
pub(crate) fn is_tool_modifier_value(entry: &Value) -> bool {
    entry.as_str().is_some_and(is_tool_modifier)
}

/// Validate a tool list from `--tools` or the SDK `tools` option. It is either an allowlist of
/// plain names and patterns or a list of only `+name`/`-name` entries with exact names. Returns
/// the problem, or `None` when the list is valid.
///
/// 检查顺序：先看是否混用（有修饰符但不是全部），再在被判定为修饰符的条目里找 `*`；
/// 所以 `["read", "-gr*"]` 报混用而不是模式错误。纯名单里的 `*` 是合法的 allowlist
/// 模式，不在这里校验。返回值不带前缀：SDK 包装成 `Invalid tools option: {message}`，
/// CLI 的 `--tools` 包装成 `--tools: {message}`。
///
/// TS: `getToolListError(entries)`。
pub fn get_tool_list_error(entries: &[String]) -> Option<String> {
    let modifiers: Vec<&String> = entries
        .iter()
        .filter(|entry| is_tool_modifier(entry))
        .collect();
    if modifiers.is_empty() {
        return None;
    }
    if modifiers.len() < entries.len() {
        return Some("tool names cannot be mixed with +name or -name entries".to_string());
    }
    if let Some(pattern) = modifiers.iter().find(|entry| entry.contains('*')) {
        return Some(format!(
            "+name and -name entries take exact tool names, not patterns: {pattern}"
        ));
    }
    None
}

/// Apply the `+name` and `-name` entries of `entries` to `base` in order: `+name` adds a tool and
/// `-name` removes one. Other entries are ignored.
///
/// TS: `applyToolModifiers(base, entries)`。`"+"`（name 为空）被忽略；重复 `+name` 不重复添加；
/// `-name` 只删第一个匹配。
///
/// 返回新列表，`base` 不被修改。`entry[1..]` 的切片安全：`is_tool_modifier` 已保证首字符
/// 是单字节 ASCII 的 `+` / `-`。空名字的 `-` 只可能删掉基列表里已有的空字符串条目。
pub fn apply_tool_modifiers(base: &[String], entries: &[String]) -> Vec<String> {
    let mut tools = base.to_vec();
    for entry in entries {
        if !is_tool_modifier(entry) {
            continue;
        }
        let name = &entry[1..];
        let index = tools.iter().position(|tool| tool == name);
        if entry.starts_with('+') {
            if index.is_none() && !name.is_empty() {
                tools.push(name.to_string());
            }
        } else if let Some(index) = index {
            tools.remove(index);
        }
    }
    tools
}

/// Merge `defaultTools` of two settings layers. A list with plain tool names replaces the
/// inherited one; a list of only `+name`/`-name` entries is appended, so it modifies the
/// inherited selection.
///
/// `overrides` 为 `None` 时返回 `base` 的克隆；`base` 为 `None` 时修饰符列表没有可继承的
/// 对象，原样返回 `overrides`。空 `overrides` 列表满足 `all`（对空集合恒真），等价于追加
/// 空列表，结果等于 `base`。
///
/// TS: `mergeDefaultTools(base, overrides)`。
pub fn merge_default_tools(
    base: Option<&Vec<String>>,
    overrides: Option<&Vec<String>>,
) -> Option<Vec<String>> {
    let Some(overrides) = overrides else {
        return base.cloned();
    };
    // Settings files are not validated; a malformed value replaces instead of throwing here.
    let Some(base) = base else {
        return Some(overrides.clone());
    };
    if !overrides.iter().all(|entry| is_tool_modifier(entry)) {
        return Some(overrides.clone());
    }
    let mut merged = base.clone();
    merged.extend(overrides.iter().cloned());
    Some(merged)
}

/// [`merge_default_tools`] 的原始 JSON 版本。
///
/// `deepMergeSettings` 在 TS 里对两个普通对象做字段合并，`defaultTools` 可能是文件里的任意
/// JSON（字符串、混合数组等）；解析失败的值只存在于 [`crate::settings::schema::Settings::extra`]，
/// 因此这里直接读原始值，语义与 TS 的 `Array.isArray` 检查一致。
///
/// 具体分支：`overrides` 缺失 → `None`；`base` 不是数组、或 `overrides` 不是数组、或
/// `overrides` 里含非字符串条目 → `overrides` 原样替换；只有两侧都是数组且 override 全是
/// 字符串修饰符时才追加。替换后的非法值由调用方决定如何回退（`get_default_tools` 把
/// 非数组当作空选择）。
pub(crate) fn merge_default_tools_raw(
    base: Option<&Value>,
    overrides: Option<&Value>,
) -> Option<Value> {
    let overrides = overrides?;
    match base {
        Some(Value::Array(base_items)) => match overrides {
            Value::Array(over_items) if over_items.iter().all(is_tool_modifier_value) => {
                let mut merged = base_items.clone();
                merged.extend(over_items.iter().cloned());
                Some(Value::Array(merged))
            }
            _ => Some(overrides.clone()),
        },
        _ => Some(overrides.clone()),
    }
}

/// Resolve a merged `defaultTools` list: plain names replace `DEFAULT_TOOL_NAMES`, then `+name`
/// adds and `-name` removes a tool, in list order.
///
/// TS: `resolveDefaultTools(entries)`。有条目且全是修饰符时从 [`DEFAULT_TOOL_NAMES`] 起步；
/// 否则以 plain 列表为基（空列表 → 空基）。
///
/// 助记：**空列表是空选择**（不是内置默认），只有「非空且全为修饰符」才从内置默认起步。
/// 基选好后再按原列表顺序应用所有修饰符，例如 `["grep", "+find", "-grep"]` 的基是
/// `["grep"]`，先加 `find` 再删 `grep`，结果 `["find"]`；非修饰符条目在这一步被忽略。
pub fn resolve_default_tools(entries: &[String]) -> Vec<String> {
    let plain: Vec<String> = entries
        .iter()
        .filter(|entry| !is_tool_modifier(entry))
        .cloned()
        .collect();
    let base: Vec<String> = if !plain.is_empty() || entries.is_empty() {
        plain
    } else {
        DEFAULT_TOOL_NAMES
            .iter()
            .map(|name| (*name).to_string())
            .collect()
    };
    apply_tool_modifiers(&base, entries)
}
