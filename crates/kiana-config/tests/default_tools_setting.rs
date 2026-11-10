//! `packages/coding-agent/test/default-tools-setting.test.ts` 的**纯函数**子集。
//!
//! 该 TS 文件共 20 个 `it`，绝大多数通过 `createAgentSession` 验证初始选择、reload 与
//! resume 的 loadout（这些属于 `kiana-session` 的 agent-session 行为，不在本 crate）。本文件
//! 只移植不依赖 session/资源的工具选择助手：
//!
//! - `getToolListError`（CLI/SDK 的 `--tools` 校验）；
//! - `isToolModifier` / `applyToolModifiers`（`+name` / `-name` 语义）；
//! - `mergeDefaultTools` / `resolveDefaultTools`（settings 层合并与解析）；
//! - `DEFAULT_TOOL_NAMES`。
//!
//! 依赖 `SettingsManager.getDefaultTools()` 的用例在 `tests/settings_manager.rs`（原
//! `settings-manager.test.ts` 的 `defaultTools` describe）。

use kiana_config::settings::tools::{
    DEFAULT_TOOL_NAMES, apply_tool_modifiers, get_tool_list_error, is_tool_modifier,
    merge_default_tools, resolve_default_tools,
};

fn names(entries: &[&str]) -> Vec<String> {
    entries.iter().map(|entry| (*entry).to_string()).collect()
}

#[test]
fn default_tool_names_match_typescript() {
    assert_eq!(DEFAULT_TOOL_NAMES, ["read", "bash", "edit", "write"]);
}

#[test]
fn identifies_tool_modifiers() {
    assert!(is_tool_modifier("+grep"));
    assert!(is_tool_modifier("-write"));
    assert!(!is_tool_modifier("grep"));
    assert!(!is_tool_modifier(""));
    // 修饰符只在首字符生效（`entry.startsWith("+")` / `"-"`）。
    assert!(!is_tool_modifier("grep+"));
}

// TS: "rejects invalid tool modifier options"
//
// TS 通过 `createSession([], { tools: [...] })` 断言
// `Invalid tools option: ${getToolListError(...)}`；SDK 只是给返回值加前缀，这里直接断言
// `getToolListError` 的逐字文案。
#[test]
fn rejects_invalid_tool_modifier_options() {
    assert_eq!(
        get_tool_list_error(&names(&["read", "+grep"])),
        Some("tool names cannot be mixed with +name or -name entries".to_string())
    );
    assert_eq!(
        get_tool_list_error(&names(&["-gr*"])),
        Some("+name and -name entries take exact tool names, not patterns: -gr*".to_string())
    );
    // 合法输入。
    assert_eq!(get_tool_list_error(&names(&["read", "bash"])), None);
    assert_eq!(get_tool_list_error(&names(&["+grep", "-write"])), None);
    assert_eq!(get_tool_list_error(&[]), None);
}

// TS: "applies +name and -name tool options to the default selection"（纯函数部分）
#[test]
fn applies_name_and_name_tool_options_to_the_default_selection() {
    assert_eq!(
        apply_tool_modifiers(
            &names(&["read", "bash", "edit", "write"]),
            &names(&["+grep"])
        ),
        names(&["read", "bash", "edit", "write", "grep"])
    );
    assert_eq!(
        apply_tool_modifiers(
            &names(&["read", "bash", "edit", "write"]),
            &names(&["+inactive_tool", "-write"])
        ),
        names(&["read", "bash", "edit", "inactive_tool"])
    );
}

#[test]
fn applies_tool_modifiers_in_order() {
    // `+name` 只添加一次；`-name` 只删第一个匹配；空名字的 `"+"` 被忽略。
    assert_eq!(
        apply_tool_modifiers(&names(&["read"]), &names(&["+read", "+grep", "+grep", "+"])),
        names(&["read", "grep"])
    );
    assert_eq!(
        apply_tool_modifiers(&names(&["read", "read", "bash"]), &names(&["-read"])),
        names(&["read", "bash"])
    );
    // 顺序：先加后删会删掉刚加的工具。
    assert_eq!(
        apply_tool_modifiers(&[], &names(&["+grep", "-grep"])),
        Vec::<String>::new()
    );
    // 非修饰符条目被忽略。
    assert_eq!(
        apply_tool_modifiers(&names(&["read"]), &names(&["bash", "-read"])),
        Vec::<String>::new()
    );
}

#[test]
fn merges_default_tools_layers() {
    // overrides 未定义 → 保留 base。
    assert_eq!(
        merge_default_tools(Some(&names(&["read"])), None),
        Some(names(&["read"]))
    );
    // 纯修饰符列表追加到 base。
    assert_eq!(
        merge_default_tools(Some(&names(&["read"])), Some(&names(&["+grep"]))),
        Some(names(&["read", "+grep"]))
    );
    // 普通名单整体替换。
    assert_eq!(
        merge_default_tools(Some(&names(&["read"])), Some(&names(&["grep"]))),
        Some(names(&["grep"]))
    );
    // base 缺失 → overrides 原样。
    assert_eq!(
        merge_default_tools(None, Some(&names(&["+grep"]))),
        Some(names(&["+grep"]))
    );
    // 空 overrides 列表：`every` 为真 → 追加空列表（等于 base）。
    let empty: Vec<String> = Vec::new();
    assert_eq!(
        merge_default_tools(Some(&names(&["read"])), Some(&empty)),
        Some(names(&["read"]))
    );
}

#[test]
fn resolves_default_tools_selection() {
    // 空列表 → 空选择（不是内置默认）。
    assert_eq!(resolve_default_tools(&[]), Vec::<String>::new());
    // 有普通名字 → 用它替换内置默认。
    assert_eq!(
        resolve_default_tools(&names(&["grep", "+find"])),
        names(&["grep", "find"])
    );
    // 全是修饰符 → 从内置默认起步。
    assert_eq!(
        resolve_default_tools(&names(&["+codemode", "-write"])),
        names(&["read", "bash", "edit", "codemode"])
    );
    // 重复 `+name` 不重复添加。
    assert_eq!(
        resolve_default_tools(&names(&["read", "+grep", "+read"])),
        names(&["read", "grep"])
    );
}
