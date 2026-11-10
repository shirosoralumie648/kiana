//! 18 个保留 id + `buildBuiltinKeybindings` + `getShortcuts` 的纯函数版本。
//!
//! TS: `core/extensions/runner.ts` 的 `RESERVED_KEYBINDINGS_FOR_EXTENSION_CONFLICTS`、
//! `buildBuiltinKeybindings` 与 `ExtensionRunner.getShortcuts`。
//!
//! 规范：`spec/ca-extensions.md` §2.3.1、§2.3.2、§2.3.9、§3.3、§8.6 第 7、14 条。
//!
//! # 数据流
//!
//! [`build_builtin_keybindings`] 把用户生效配置展开成“物理键（小写）→ 内建动作”；
//! [`get_shortcuts`] 再用它过滤扩展注册的快捷键：
//!
//! - 命中保留动作 → 丢弃该快捷键并记一条诊断；
//! - 命中非保留动作 → 保留该快捷键（覆盖内建行为）并记一条诊断；
//! - 未命中、但同一个键已被前一个扩展注册 → 保留（后注册者胜出）并记一条诊断。
//!
//! 诊断顺序固定：先内建冲突、后扩展间冲突（内建冲突先决定该键是否被保留动作占用，剩余键才进入扩展间去重）。`has_ui == false` 时每条诊断还会复制一份
//! 顺序相同的文本到 [`ShortcutResolution::warnings`]（本模块不打印，由调用方决定输出方式）。
//!
//! # 归一化与顺序
//!
//! 查找用 `to_lowercase()` 后的键；诊断文案保留注册时的原始拼写（黄金夹具
//! `shortcut_cases.json` 的 `CTRL+C` 用例锁定）。返回的 `shortcuts` 是 `IndexMap`：键顺序
//! = 首次成功插入的顺序，后续覆盖不改变位置。
//!
//! # 调用方
//!
//! [`ExtensionRunner::get_shortcuts`](super::ExtensionRunner::get_shortcuts) 是本模块的门面：
//! 它把 runner 的实时扩展快照、`has_ui()` 与配置一起传进来。快捷键 handler 本身不在本模块
//! 执行，本模块只决定“哪些键可见、哪些冲突”。

use indexmap::IndexMap;
use kiana_config::keybindings::{KeyIdList, KeybindingsConfig};
use kiana_core::diagnostics::{DiagnosticKind, ResourceDiagnostic};
use kiana_ext_api::{Extension, ExtensionShortcut};

/// 与扩展快捷键竞争的保留键位 id（**18 项**，顺序即源码顺序）。
///
/// TS: `runner.ts#RESERVED_KEYBINDINGS_FOR_EXTENSION_CONFLICTS`。
/// 注释语义：只保留“编辑器全局”快捷键；picker 专属绑定不在其中。
///
/// 只有**成员资格**参与判定（`restrict_override`）；顺序用于与 TS 源码逐字核对，
/// `tests/extension_keybindings.rs` 同时锁定数量、内容与顺序。
pub const RESERVED_KEYBINDINGS_FOR_EXTENSION_CONFLICTS: [&str; 18] = [
    "app.interrupt",
    "app.clear",
    "app.exit",
    "app.suspend",
    "app.thinking.cycle",
    "app.model.cycleForward",
    "app.model.cycleBackward",
    "app.model.select",
    "app.tools.expand",
    "app.thinking.toggle",
    "app.editor.external",
    "app.message.copy",
    "app.message.followUp",
    "tui.input.submit",
    "tui.select.confirm",
    "tui.select.cancel",
    "tui.input.copy",
    "tui.editor.deleteToLineEnd",
];

/// 一个物理键最终对应的内建动作。
///
/// TS: `runner.ts` 的 `BuiltInKeyBindings` 值类型
/// `{ keybinding: string; restrictOverride: boolean }`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltinKeybinding {
    /// 动作 id（保留清单里的 string）。
    pub keybinding: String,
    /// 该动作是否在 18 项保留清单里（`true` 时扩展不得覆盖）。
    pub restrict_override: bool,
}

/// `buildBuiltinKeybindings(resolvedKeybindings)`。
///
/// TS: `runner.ts#buildBuiltinKeybindings`。
///
/// 同一物理键被多个动作绑定时，**保留动作胜出**（与遍历顺序无关）：先写入的非保留项会被
/// 后到的保留项覆盖，先写入的保留项不会被非保留项覆盖。键名按 JS `toLowerCase()` 归一
/// （键位 id 全为 ASCII，`str::to_lowercase` 等价）。
///
/// TS 的 `keys === undefined` 分支对应 Rust 的“键不存在”（[`KeybindingsConfig`] 没有
/// `undefined` 值），因此无需跳过。
///
/// 返回的 map 顺序 = 配置遍历顺序 × 每个动作的键列表顺序；同一物理键只保留一个条目。
/// `restrict_override` 是**动作**的属性（是否在 18 项保留清单），因此同一动作绑定的每个键
/// 都带相同的值。非保留动作之间是“后写覆盖先写”。
pub fn build_builtin_keybindings(
    resolved_keybindings: &KeybindingsConfig,
) -> IndexMap<String, BuiltinKeybinding> {
    let mut builtin_keybindings: IndexMap<String, BuiltinKeybinding> = IndexMap::new();
    for (keybinding, keys) in resolved_keybindings {
        let restrict_override =
            RESERVED_KEYBINDINGS_FOR_EXTENSION_CONFLICTS.contains(&keybinding.as_str());
        let key_list: &[String] = match keys {
            KeyIdList::One(key) => std::slice::from_ref(key),
            KeyIdList::Many(keys) => keys,
        };
        for key in key_list {
            let normalized_key = key.to_lowercase();
            // 保留项胜出，与遍历顺序无关：只有“新项非保留且已有项保留”时跳过；
            // 其余组合（保留覆盖非保留、保留覆盖保留、非保留覆盖非保留）都写入。
            if builtin_keybindings
                .get(&normalized_key)
                .is_some_and(|existing| existing.restrict_override)
                && !restrict_override
            {
                continue;
            }
            builtin_keybindings.insert(
                normalized_key,
                BuiltinKeybinding {
                    keybinding: keybinding.clone(),
                    restrict_override,
                },
            );
        }
    }
    builtin_keybindings
}

/// `getShortcuts(resolvedKeybindings)` 的返回值。
///
/// `diagnostics` 对应 TS 的 `shortcutDiagnostics`（`type: "warning"`，`path` 是
/// `shortcut.extensionPath`）。`warnings` 是在 `has_ui == false` 时 TS 额外
/// `console.warn` 的文本，按产生顺序排列；本模块不打印，由调用方（runner-core）输出。
#[derive(Debug, Clone, Default)]
pub struct ShortcutResolution {
    /// 解析后的扩展快捷键（键为小写后的 `KeyId`，后注册者胜出）。
    pub shortcuts: IndexMap<String, ExtensionShortcut>,
    /// 逐条捷径冲突诊断。
    pub diagnostics: Vec<ResourceDiagnostic>,
    /// `has_ui == false` 时需要 `console.warn` 的文本（有 UI 时为空）。
    pub warnings: Vec<String>,
}

/// 解析扩展快捷键并收集冲突诊断。
///
/// TS: `runner.ts#getShortcuts`。按扩展加载顺序、每个扩展内 `shortcuts` 的插入顺序遍历；
/// 返回的 map 键是小写后的键位 id。
///
/// `has_ui` 只影响 [`ShortcutResolution::warnings`]：TS 在 `!this.hasUI()` 时对每条诊断
/// 额外 `console.warn(message)`。本函数不打印（库代码的 `console.warn` 由调用方落到
/// `tracing`/stderr），因此把文本放进返回值。
///
/// 每条诊断的 `kind` 固定为 `Warning`、`path` 为产生冲突的扩展路径、`collision` 为
/// `None`。`has_ui == false` 时 `warnings` 与 `diagnostics` 等长且同序；`has_ui == true`
/// 时 `warnings` 为空。命中保留动作的快捷键不会出现在 `shortcuts` 中；非保留内建冲突与
/// 扩展间冲突只是诊断，条目照常保留。
pub fn get_shortcuts(
    extensions: &[Extension],
    resolved_keybindings: &KeybindingsConfig,
    has_ui: bool,
) -> ShortcutResolution {
    let builtin_keybindings = build_builtin_keybindings(resolved_keybindings);
    let mut resolution = ShortcutResolution::default();

    for ext in extensions {
        for (key, shortcut) in &ext.shortcuts {
            // 查找用小写，诊断文案沿用注册时的原始拼写（TS 的 `key` 变量不归一化）。
            let normalized_key = key.to_lowercase();

            // 一条诊断同时服务两个出口：结构化列表与（无 UI 时的）console.warn 文本。
            let mut add_diagnostic = |message: String, extension_path: &str| {
                resolution.diagnostics.push(ResourceDiagnostic {
                    kind: DiagnosticKind::Warning,
                    message: message.clone(),
                    path: Some(extension_path.to_string()),
                    collision: None,
                });
                if !has_ui {
                    resolution.warnings.push(message);
                }
            };

            match builtin_keybindings.get(&normalized_key) {
                // 保留动作：拒绝覆盖，快捷键不入表。
                Some(builtin) if builtin.restrict_override => {
                    add_diagnostic(
                        format!(
                            "Extension shortcut '{key}' from {} conflicts with built-in shortcut. Skipping.",
                            shortcut.extension_path
                        ),
                        &shortcut.extension_path,
                    );
                    continue;
                }
                // 非保留动作：允许覆盖，但记录“内建快捷键被扩展占用”。
                Some(builtin) => {
                    add_diagnostic(
                        format!(
                            "Extension shortcut conflict: '{key}' is built-in shortcut for {} and {}. Using {}.",
                            builtin.keybinding, shortcut.extension_path, shortcut.extension_path
                        ),
                        &shortcut.extension_path,
                    );
                }
                None => {}
            }

            // 扩展之间同键：诊断后仍写入，后注册者覆盖先注册者。
            if let Some(existing) = resolution.shortcuts.get(&normalized_key) {
                add_diagnostic(
                    format!(
                        "Extension shortcut conflict: '{key}' registered by both {} and {}. Using {}.",
                        existing.extension_path, shortcut.extension_path, shortcut.extension_path
                    ),
                    &shortcut.extension_path,
                );
            }
            // 后注册者胜出。
            resolution
                .shortcuts
                .insert(normalized_key, shortcut.clone());
        }
    }

    resolution
}
