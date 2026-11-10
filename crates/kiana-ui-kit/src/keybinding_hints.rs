//! keybinding-hints.ts（keyHint/keyText）。
//!
//! 键位提示格式化。TS 源：
//! `packages/coding-agent/src/modes/interactive/components/keybinding-hints.ts`。
//!
//! 与 TS 的差异：
//! * `KeyTextFormatOptions` 在 TS 里所有字段可选；Rust 用 `bool` + `Default`（`false`）。
//! * `process.platform === "darwin"` → `cfg!(target_os = "macos")`。为了在 Linux 测试里也能
//!   覆盖 macOS 分支，内部实现接收一个可注入的 `is_macos` 参数（公共 API 用
//!   `cfg!(target_os = "macos")`）。
//! * `charAt(0).toUpperCase()` 按 **UTF-16 code unit** 取首字符并做完整的 Unicode 大写
//!   展开（`ß` → `SS`、`ﬁ` → `FI`、`ǰ` → `J̌`）；astral 字符的首 code unit 是代理项，
//!   JS 没有大小写映射，因此保持原样。见私有函数 `capitalize_first_code_unit`。
//! * 空键位列表得到空串，`keyHint` 仍会输出描述前的那个空格（dim 段为空）。
//!
//! # 无状态、每次读全局
//!
//! `key_text`/`key_display_text` 每次调用都重新读取 `get_keybindings()`（运行期可被
//! `set_keybindings` 替换）；`key_hint`/`raw_key_hint` 还对 `kiana_theme::THEME` 着色
//! （每次 `fg` 都解析当前主题）。`format_key_text` 是纯字符串变换，不读任何全局状态。
//! 因此调用方可以在每帧渲染时直接调用，键位绑定与主题切换会立刻反映到输出；需要复用
//! 同一结果时由调用方自己保存。
//!
//! 输出分两类：[`key_hint`] 与 [`raw_key_hint`] 返回带 ANSI 转义序列的着色字符串（dim 键位 /
//! muted 描述）；[`key_text`]、[`key_display_text`]、[`format_key_text`] 返回纯文本。需要
//! 计算可见宽度时，前者必须先剥离序列。
//!
//! # 调用方
//!
//! `kiana-tools` 的各工具渲染器（read/find/write/bash/ls 等）、`kiana-builtin-extensions`
//! 与 `kiana-interactive` 的对话框都用它拼接底部键位提示；`format_key_text` /
//! `key_display_text` 也被 `kiana-interactive` 的 hotkeys 命令与 3D 彩蛋直接用于展示。

use kiana_theme::ThemeColor;
use kiana_tui::get_keybindings;
use kiana_tui::keybindings::Keybinding;

/// `KeyTextFormatOptions`（TS 里是可选字段）。
///
/// `Default`（`capitalize: false`）对应 TS 不传 options 的行为；[`key_text`] 与
/// [`raw_key_hint`] 用它，[`key_display_text`] 打开大写。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KeyTextFormatOptions {
    /// 是否把每段的首字母大写（TS `capitalize?: boolean`）。
    pub capitalize: bool,
}

/// 按全局主题着色（TS `theme.fg(token, str)`）。
fn fg(color: ThemeColor, text: &str) -> String {
    kiana_theme::THEME
        .fg(color, text)
        .expect("theme must be initialized before rendering (TS throws here)")
}

/// TS `displayPart.charAt(0).toUpperCase() + displayPart.slice(1)`。
///
/// JS 的 `charAt(0)` 取的是 **UTF-16 code unit**，`toUpperCase()` 对该 unit 做完整的
/// Unicode 大写展开（可能变成多个标量），再与 `slice(1)` 拼接：
///
/// * BMP 字符：`ß` → `SS`、`ﬁ` → `FI`、`ŉ` → `ʼN`（U+02BC `N`）、`ǰ` → `J̌`；
/// * astral 字符：首 unit 是孤立代理项，没有大小写映射，整串原样返回
///   （例如 `𐐨` 不会被大写成 `𐐀`）；
/// * 空串：返回空串。
fn capitalize_first_code_unit(text: &str) -> String {
    let Some(first_unit) = text.encode_utf16().next() else {
        return String::new();
    };
    // 代理项（astral 字符的高位/低位）: `char::from_u32` 返回 None，JS 也保持不变。
    let Some(first_char) = char::from_u32(u32::from(first_unit)) else {
        return text.to_string();
    };
    let mut result: String = first_char.to_uppercase().collect();
    // `first_char` 是首 code unit 解出的完整标量，`len_utf8()` 即剩余部分的起始偏移。
    result.push_str(&text[first_char.len_utf8()..]);
    result
}

/// `formatKeyPart(part, options)`，`is_macos` 是 `process.platform === "darwin"` 的注入点。
fn format_key_part_for(part: &str, options: KeyTextFormatOptions, is_macos: bool) -> String {
    let display_part = if is_macos && part.to_lowercase() == "alt" {
        "option"
    } else {
        part
    };
    if options.capitalize {
        capitalize_first_code_unit(display_part)
    } else {
        display_part.to_string()
    }
}

/// 用当前平台重写 `formatKeyPart`。
fn format_key_part(part: &str, options: KeyTextFormatOptions) -> String {
    format_key_part_for(part, options, cfg!(target_os = "macos"))
}

/// `formatKeyText(key, options)`：先按 `"/"` 分段，再按 `"+"` 分段；空段保留。
///
/// `"/"` 表示“任一可接受的键”（如 `escape/ctrl+c`），`"+"` 表示同时按下的组合。切分是
/// 逐字进行的、不做 trim：`"a//b"` 会产生一个空段，输出仍是 `"a//b"`。每段交给
/// `format_key_part`：macOS 上整段恰为 `alt`（大小写不敏感）时替换成 `option`；
/// `capitalize` 为真时按 JS 的 UTF-16 语义大写段首（见私有函数
/// `capitalize_first_code_unit` 的说明）。
///
/// 本函数不查键位表、不读主题，是纯字符串变换，可安全用于任意输入。
pub fn format_key_text(key: &str, options: KeyTextFormatOptions) -> String {
    key.split('/')
        .map(|segment| {
            segment
                .split('+')
                .map(|part| format_key_part(part, options))
                .collect::<Vec<_>>()
                .join("+")
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// `formatKeys(keys, options)`（模块私有）：空数组 → 空串。
fn format_keys(keys: &[String], options: KeyTextFormatOptions) -> String {
    if keys.is_empty() {
        return String::new();
    }
    format_key_text(&keys.join("/"), options)
}

/// `keyText(keybinding)`。
///
/// 每次调用读取全局键位表；未知或显式解绑的键位得到空串。多个键位按 `get_keys` 的顺序用
/// `"/"` 连接，格式化为 `escape/ctrl+c` 这样的形式。
pub fn key_text(keybinding: Keybinding) -> String {
    format_keys(
        &get_keybindings().get_keys(keybinding),
        KeyTextFormatOptions::default(),
    )
}

/// `keyDisplayText(keybinding)`（每段首字母大写）。
///
/// 与 [`key_text`] 使用同一个键位表，只是打开 `capitalize`：`escape/ctrl+c` →
/// `Escape/Ctrl+C`，供面向用户的菜单/提示使用。未绑定时同样返回空串。
pub fn key_display_text(keybinding: Keybinding) -> String {
    format_keys(
        &get_keybindings().get_keys(keybinding),
        KeyTextFormatOptions { capitalize: true },
    )
}

/// `keyHint(keybinding, description)`：dim 键位 + muted 描述（半角空格分隔）。
///
/// 键位未绑定时 dim 段是空串，但描述前的空格仍然保留（`" {description}"` 用一个
/// `fg(muted, ...)` 整体着色），测试
/// `key_hint_uses_dim_keys_and_muted_description` 锁定了这一点。
pub fn key_hint(keybinding: Keybinding, description: &str) -> String {
    fg(ThemeColor::Dim, &key_text(keybinding)) + &fg(ThemeColor::Muted, &format!(" {description}"))
}

/// `rawKeyHint(key, description)`：直接格式化一个原始键位串。
///
/// 不查键位表，`key` 可以是任意展示串（例如 `"↑↓"`、`"space"`）；格式选项固定为默认
/// （不做首字母大写），因此 `"alt+left"` 在 macOS 上仍会变成 `option+left`。
pub fn raw_key_hint(key: &str, description: &str) -> String {
    fg(
        ThemeColor::Dim,
        &format_key_text(key, KeyTextFormatOptions::default()),
    ) + &fg(ThemeColor::Muted, &format!(" {description}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_key_parts_with_plus_and_slash() {
        let options = KeyTextFormatOptions::default();
        assert_eq!(format_key_text("ctrl+shift+up", options), "ctrl+shift+up");
        assert_eq!(
            format_key_text("ctrl+shift+up/ctrl+up", options),
            "ctrl+shift+up/ctrl+up"
        );
        // 空段保留（JS `"a//b"` → `"a//b"`）。
        assert_eq!(format_key_text("a//b", options), "a//b");
        assert_eq!(format_key_text("", options), "");
    }

    #[test]
    fn capitalizes_only_the_first_character() {
        let options = KeyTextFormatOptions { capitalize: true };
        assert_eq!(format_key_text("escape", options), "Escape");
        assert_eq!(format_key_text("ctrl+s", options), "Ctrl+S");
        assert_eq!(format_key_text("ctrl+shift+up", options), "Ctrl+Shift+Up");
        // 空段的大写结果仍是空串。
        assert_eq!(format_key_text("a//b", options), "A//B");
    }

    /// JS `charAt(0).toUpperCase() + slice(1)`：完整 Unicode 大写展开、按 UTF-16 code unit
    /// 取首字符；astral 字符不变（首 unit 是孤立代理项）。golden 见
    /// `tests/fixtures/keybinding_format.json`（由真实 TS 模块生成）。
    #[test]
    fn capitalizes_with_js_utf16_expansion_semantics() {
        let options = KeyTextFormatOptions { capitalize: true };
        assert_eq!(format_key_text("ß", options), "SS");
        assert_eq!(format_key_text("ßx", options), "SSx");
        assert_eq!(format_key_text("ﬁsh", options), "FIsh");
        assert_eq!(format_key_text("ŉa", options), "ʼNa");
        assert_eq!(format_key_text("ǰa", options), "J̌a");
        assert_eq!(format_key_text("ﬃ", options), "FFI");
        assert_eq!(format_key_text("ẖ", options), "H̱");
        assert_eq!(format_key_text("ΐ", options), "Ϊ́");
        assert_eq!(format_key_text("ctrl+ß", options), "Ctrl+SS");
        // astral 字符：`charAt(0)` 是高位代理项，没有大小写映射。
        assert_eq!(format_key_text("\u{10428}abc", options), "\u{10428}abc");
        assert_eq!(format_key_text("\u{10400}", options), "\u{10400}");
        assert_eq!(format_key_text("😀+a", options), "😀+A");
    }

    /// 非 capitalize 模式完全不改变大小写（`rawKeyHint` 走这条路径）。
    #[test]
    fn non_capitalized_formatting_preserves_the_original_text() {
        let options = KeyTextFormatOptions::default();
        assert_eq!(format_key_text("ß", options), "ß");
        assert_eq!(format_key_text("ﬁ", options), "ﬁ");
        assert_eq!(format_key_text("ŉ", options), "ŉ");
        assert_eq!(format_key_text("\u{10428}", options), "\u{10428}");
        assert_eq!(format_key_text("ALT", options), "ALT");
    }

    #[test]
    fn maps_alt_to_option_only_on_macos() {
        let options = KeyTextFormatOptions::default();
        assert_eq!(format_key_part_for("alt", options, true), "option");
        assert_eq!(format_key_part_for("ALT", options, true), "option");
        assert_eq!(format_key_part_for("alt", options, false), "alt");
        assert_eq!(
            format_key_part_for("alt+left", options, true),
            "alt+left",
            "只有整段恰好是 alt 才替换"
        );
        assert_eq!(
            format_key_part_for("alt", KeyTextFormatOptions { capitalize: true }, true),
            "Option"
        );
    }

    #[test]
    fn empty_key_list_formats_to_empty_string() {
        assert_eq!(format_keys(&[], KeyTextFormatOptions::default()), "");
        assert_eq!(
            format_keys(
                &["escape".to_string(), "ctrl+c".to_string()],
                KeyTextFormatOptions::default()
            ),
            "escape/ctrl+c"
        );
    }
}
