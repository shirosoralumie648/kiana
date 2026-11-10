//! `core/key-id-schema.ts`：`KeyIdSchema` / `keybindingValueSchema` 的手写校验。
//!
//! TS 源：`packages/coding-agent/src/core/key-id-schema.ts`（21 行）。
//! 规范：`spec/ca-config-models.md` §2.25（模式逐字）、§4.8、§8.3 第 7 条。
//!
//! ## 为什么手写
//!
//! TypeBox 的正则模式是 ECMAScript 语义，其中负向前瞻
//! `(?!.*(?:ctrl\+.*ctrl\+|...))` 在 Rust 的 `regex` crate 不可用（不支持 lookaround）。
//! 本文件按 §2.25 的语义手写等价校验：
//!
//! 1. 同一修饰键（`ctrl`/`shift`/`alt`/`super`）不得出现两次——负向前瞻
//!    `(?!(?:m\+.*m\+))` 只关心字符串中 `m+` 出现的次数；
//! 2. 修饰键前缀 0–4 个，每个必须恰好是 `ctrl`/`shift`/`alt`/`super` 之一后跟 `+`；
//! 3. 剩余部分必须是基础键：单个 ASCII `a`–`z` / `0`–`9`，或逐字命中 [`BASE_KEYS`]
//!    （`Key` 对象的字符串值；**大小写敏感**）。
//!
//! 正则的 `{0,4}` 是贪婪的，失败时回溯；因此校验按“尽量多的修饰键前缀”尝试，
//! 从 4 递减到 0。`+` 本身是合法基础键（`"+"`、`"ctrl++"` 均合法）。
//!
//! [`KEY_ID_PATTERN`] 保持 TS 生成的模式**逐字**不变（用于 `KeybindingsSchema`），
//! 仅运行时校验走手写实现。
//!
//! ## 两套表示必须保持等价
//!
//! 本文件同时维护两份互不依赖的实现：
//!
//! - **schema 文本**：[`KEY_ID_PATTERN`] 是 TS 生成的 343 字符字面量，只写进
//!   [`key_id_schema`] / [`keybinding_value_schema`] 的 `pattern` 字段，供
//!   `tests/fixtures/keybindings_schema.json` 逐字节比对；
//! - **运行时谓词**：[`is_valid_key_id`] 用普通字符串操作实现同一语义——`regex` crate
//!   不支持 lookaround，无法直接执行该模式。
//!
//! 两者之间没有代码级交叉校验：只改一边不会让另一边报错。防线分两侧：
//! **schema 文本**由 `tests/keybindings_schema.rs` 的
//! `whole_schema_matches_the_typescript_golden`（以及
//! `pattern_and_description_are_verbatim` 的长度/片段检查）逐字节锁定；
//! **运行时谓词**由同一测试文件把 [`is_valid_key_id`] 与 TS 参考实现生成的
//! `key_id_fuzz_cases.json`（当前 867 例）逐例比较，以及本文件末尾的单元测试锁定。
//! 后两者都不读取 [`KEY_ID_PATTERN`]，无法发现 pattern 单独被改成同长度、保留
//! 负向前瞻片段的错误值。
//!
//! ## `is_valid_key_id` 的执行顺序
//!
//! 1. 重复修饰键：分别统计 `ctrl+`/`shift+`/`alt+`/`super+` 在**整个字符串**中出现的
//!    次数，任一大于 1 立即返回 `false`；
//! 2. 修饰键前缀：从 4 个递减到 0 个逐个剥掉；某一段不是四个修饰键之一就放弃该轮；
//! 3. 基础键：剩余部分命中 [`BASE_KEYS`]，或是单个 ASCII `a`–`z` / `0`–`9`。
//!
//! 第 1 步独立于第 2、3 步：`"super+super+a"` 按后两步可以解析成「两个修饰键 + 基础键
//! `a`」，但 `super+` 出现两次，会在第 1 步被拒。这与 TS 正则的负向前瞻一致——负向前瞻
//! 在字符串起点独立检查重复，与主模式的修饰键数量回溯无关。
//!
//! ## 注意事项
//!
//! - [`MODIFIERS`] 与 [`KEY_ID_PATTERN`] 是两份独立字面量，改一个不会更新另一个；
//!   模式必须从 TS 重新生成（测试锁定长度 343 与负向前瞻片段）。
//! - [`BASE_KEYS`] 同样不参与正则拼接：新增基础键要同时改本表并重新生成 pattern。
//! - 本文件只回答「字符串是否是合法 KeyId」，不解析成键序列；真正的按键匹配在
//!   `kiana_tui::keys::matches_key`。

use std::sync::LazyLock;

use serde_json::{Map, Value};

/// `modifiers = ["ctrl", "shift", "alt", "super"] as const`。
///
/// 同时服务于运行时校验（[`is_valid_key_id`] 的重复检测）与 TS 生成的正则；两者是
/// 独立副本，修改本常量不会改变 [`KEY_ID_PATTERN`]。
pub const MODIFIERS: [&str; 4] = ["ctrl", "shift", "alt", "super"];

/// `KeyIdSchema` 的 `pattern`，逐字取自生成的 `schemas/keybindings.schema.json`
/// （`$defs.KeybindingValue.anyOf[0].pattern`，343 字符）。
///
/// 仅供 schema 输出使用（[`key_id_schema`]、[`keybinding_value_schema`]）：`regex` crate
/// 编译不了其中的 lookaround，运行时校验走手写实现。测试
/// `pattern_and_description_are_verbatim` 锁定长度与片段，不能手改。
pub const KEY_ID_PATTERN: &str = r"^(?!.*(?:ctrl\+.*ctrl\+|shift\+.*shift\+|alt\+.*alt\+|super\+.*super\+))(?:(?:ctrl|shift|alt|super)\+){0,4}(?:[a-z0-9]|escape|esc|enter|return|tab|space|backspace|delete|insert|clear|home|end|pageUp|pageDown|up|down|left|right|f1|f2|f3|f4|f5|f6|f7|f8|f9|f10|f11|f12|`|-|=|\[|\]|\\|;|'|,|\.|/|!|@|#|\$|%|\^|&|\*|\(|\)|_|\+|\||~|\{|\}|:|<|>|\?)$";

/// `KeyIdSchema` 的 `description`（**【逐字】**）。
///
/// 由 [`key_id_schema`] 写入 `description` 字段，测试逐字断言。
pub const KEY_ID_DESCRIPTION: &str =
    "Key identifier with optional ctrl, shift, alt, or super modifiers.";

/// `Object.values(Key).filter(typeof === "string")` 的**源码顺序**（61 项）。
///
/// 来自 `packages/tui/src/keys.ts` 的 `Key` 对象：特殊键 18 个、`f1`–`f12` 12 个、
/// 符号键 31 个（合计 61）。函数成员（`ctrl`/`shift`/…）被 `typeof v === "string"` 过滤掉。
///
/// 运行时按**精确字符串**匹配（大小写敏感、不做前缀匹配），由 `is_base_key` 使用；
/// 单个 `a`–`z`/`0`–`9` 不在此表（走 ASCII 字节判断）。`tests/keybindings_schema.rs`
/// 用 `key_id_base_keys.json` 锁定这 61 项的内容与顺序；该表不参与
/// [`KEY_ID_PATTERN`] 的拼接。
pub const BASE_KEYS: [&str; 61] = [
    "escape",
    "esc",
    "enter",
    "return",
    "tab",
    "space",
    "backspace",
    "delete",
    "insert",
    "clear",
    "home",
    "end",
    "pageUp",
    "pageDown",
    "up",
    "down",
    "left",
    "right",
    "f1",
    "f2",
    "f3",
    "f4",
    "f5",
    "f6",
    "f7",
    "f8",
    "f9",
    "f10",
    "f11",
    "f12",
    "`",
    "-",
    "=",
    "[",
    "]",
    "\\",
    ";",
    "'",
    ",",
    ".",
    "/",
    "!",
    "@",
    "#",
    "$",
    "%",
    "^",
    "&",
    "*",
    "(",
    ")",
    "_",
    "+",
    "|",
    "~",
    "{",
    "}",
    ":",
    "<",
    ">",
    "?",
];

/// `KeyIdSchema`：`Type.Unsafe<KeyId>(Type.String({pattern, description}))` 的 JSON 值。
///
/// 键序与 `JSON.stringify` 一致：`type` → `pattern` → `description`。
///
/// 每次调用新建一个 `Value`。键序是冻结契约（由 `tests/fixtures/keybindings_schema.json`
/// 的字节比对锁定），任何改写都必须保持 `type` → `pattern` → `description` 的键序；
/// 这里是 TypeBox 对象的原始形态，没有 `$schema`/`$id` 外壳。
pub fn key_id_schema() -> Value {
    let mut schema = Map::new();
    schema.insert("type".to_string(), Value::String("string".to_string()));
    schema.insert(
        "pattern".to_string(),
        Value::String(KEY_ID_PATTERN.to_string()),
    );
    schema.insert(
        "description".to_string(),
        Value::String(KEY_ID_DESCRIPTION.to_string()),
    );
    Value::Object(schema)
}

/// `KeybindingValueSchema = Type.Union([KeyIdSchema, Type.Array(KeyIdSchema)])` 的 JSON 值
/// （无顶层描述；键序 `anyOf`）。
///
/// 数组分支的 `items` 是**新构造**的 KeyId schema 而非 `$ref`（TS 的
/// `Type.Array(KeyIdSchema)` 在 JSON 中同样展开）；`$ref` 去重属于生成脚本阶段，
/// 见 [`super::schema`] 的模块文档。
pub fn keybinding_value_schema() -> Value {
    let mut any_of = Vec::with_capacity(2);
    // 分支顺序 [KeyId, KeyId[]] 与 TS 的 Type.Union 一致，测试按索引断言。
    any_of.push(key_id_schema());
    let mut array_schema = Map::new();
    array_schema.insert("type".to_string(), Value::String("array".to_string()));
    array_schema.insert("items".to_string(), key_id_schema());
    any_of.push(Value::Object(array_schema));

    let mut union = Map::new();
    union.insert("anyOf".to_string(), Value::Array(any_of));
    Value::Object(union)
}

/// `keybindingValueSchema({description})`：在联合外层追加 `description`（**键序 `anyOf` 在前**）。
///
/// `options` 在 TypeBox 中是展开到联合对象上的注解，因此描述位于 `anyOf` 之后。
/// 即使传入空串也会写出 `"description": ""`，不会省略该键。
pub fn keybinding_value_schema_with_description(description: &str) -> Value {
    let mut schema = keybinding_value_schema();
    if let Value::Object(ref mut map) = schema {
        map.insert(
            "description".to_string(),
            Value::String(description.to_string()),
        );
    }
    schema
}

/// `KeyIdSchema` 的静态副本（生成 JSON Schema 时反复使用）。
///
/// 首次解引用时调用 [`key_id_schema`] 构造一次，之后共享只读；要修改必须先 `clone()`。
pub static KEY_ID_SCHEMA: LazyLock<Value> = LazyLock::new(key_id_schema);

/// `KeybindingValueSchema` 的静态副本。
///
/// 首次解引用时调用 [`keybinding_value_schema`] 构造一次；由 [`super::schema`] 以
/// `pub use` 再导出。
pub static KEYBINDING_VALUE_SCHEMA: LazyLock<Value> = LazyLock::new(keybinding_value_schema);

/// 运行时 `KeyId` 校验（`KeyIdSchema` 的 TypeBox `Check`）。
///
/// 等价的 ECMAScript 正则语义见模块文档。基础键大小写敏感；修饰键拒绝重复。
/// 返回 `bool`，没有错误类型；不做 trim、大小写归一或前缀匹配。空串、`"ctrl+"`（缺
/// 基础键）、多于 4 个修饰键、任何非 ASCII 基础键都返回 `false`。
pub fn is_valid_key_id(value: &str) -> bool {
    // 1. `(?!.*(?:m\+.*m\+))`：同一修饰键不得出现两次（`.*` 允许任意中间文本）。
    // `match_indices` 统计的是**不重叠**匹配；`ctrl+`/`shift+`/`alt+`/`super+` 都没有
    // 自重叠的前后缀，所以「不重叠次数 > 1」与「字符串里出现两次」等价。
    for modifier in MODIFIERS {
        let needle = format!("{modifier}+");
        if value.match_indices(&needle).count() > 1 {
            return false;
        }
    }

    // 2. `(?:(?:ctrl|shift|alt|super)\+){0,4}` 贪婪匹配并从 4 回溯到 0。
    // 从大到小尝试只是对应正则的贪婪顺序；判定的是「存在一种切分」，顺序不影响最终
    // 布尔值。上限 4 与 TS 正则一致：5 个修饰键在阶段 1 就会因至少有重复而被拒。
    for modifier_count in (0..=4).rev() {
        if let Some(base) = strip_modifier_prefix(value, modifier_count)
            && is_base_key(base)
        {
            return true;
        }
    }
    // 没有任何切分成功：前缀段名不合法（如 `control+`）或基础键不在集合内。
    false
}

/// 基础键：`(?:[a-z0-9]|escape|esc|...|>)`。
///
/// `[a-z0-9]` 是**单个** ASCII 小写字母或数字（JS 正则按 UTF-16 码元匹配，
/// 因此非 ASCII 字符与多字节字符都不算）。
///
/// 两个分支互斥：[`BASE_KEYS`] 只含特殊键、`f1`–`f12` 与符号，不含 `a`–`z`/`0`–`9`；
/// `bytes.len() == 1` 同时排除空串和所有多字节 UTF-8 字符（`"é"`、`"中"` 都不合法）。
fn is_base_key(base: &str) -> bool {
    if BASE_KEYS.contains(&base) {
        return true;
    }
    let bytes = base.as_bytes();
    bytes.len() == 1 && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
}

/// 依次剥掉 `count` 个 `modifier+` 前缀，返回剩余的基础键部分。
///
/// 每段按第一个 `+` 切分，段名必须恰好是 `ctrl`/`shift`/`alt`/`super`（大小写敏感，
/// `"Ctrl+x"` 返回 `None`）。剩余部分允许为空或仍含 `+`（`"ctrl++"` 剥一个前缀后剩
/// `"+"`），合法性由调用方的 `is_base_key` 判断。
fn strip_modifier_prefix(value: &str, count: usize) -> Option<&str> {
    let mut rest = value;
    for _ in 0..count {
        let (modifier, after) = rest.split_once('+')?;
        if !MODIFIERS.contains(&modifier) {
            return None;
        }
        rest = after;
    }
    Some(rest)
}

/// `validateKeybindingValue.Check(value)`：`KeyId` 或 `KeyId[]`（数组元素逐个校验）。
///
/// 非字符串的标量、对象、嵌套数组都判为非法；空数组合法。
///
/// 数组里任一元素失败即整体 `false`，不会部分接受。父模块的 `key_id_list_from_value`
/// 用相同的分派规则做类型化转换（把字符串变成 `KeyIdList`），两者需要一起维护。
pub fn is_valid_keybinding_value(value: &Value) -> bool {
    match value {
        Value::String(binding) => is_valid_key_id(binding),
        Value::Array(bindings) => bindings
            .iter()
            .all(|binding| matches!(binding, Value::String(value) if is_valid_key_id(value))),
        _ => false,
    }
}

// 锁定文档示例/反例，并抽样检查 BASE_KEYS 的长度与边界位置；完整 61 项内容与顺序
// 由 tests/keybindings_schema.rs 的 base_keys_match_the_generated_fixture 对
// key_id_base_keys.json 断言。与 TS 的差分语料也在 tests/keybindings_schema.rs。
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_the_documented_valid_bindings() {
        for binding in [
            "a",
            "9",
            "pageUp",
            "+",
            "ctrl+shift+x",
            "alt+ctrl+?",
            "ctrl+shift+alt+super+f12",
            "ctrl++",
            "ctrl+-",
        ] {
            assert!(is_valid_key_id(binding), "{binding:?}");
        }
    }

    #[test]
    fn rejects_the_documented_invalid_bindings() {
        for binding in [
            "",
            "Ctrl+x",
            "control+x",
            "ctrl+not-a-key",
            "ctrl+ctrl+x",
            "ctrl+shift+alt+super+ctrl+x",
            "ctrl+",
            "ctrl+ctrl",
            "++++",
            "ctrl++++",
            "ctrl+a+\n",
        ] {
            assert!(!is_valid_key_id(binding), "{binding:?}");
        }
    }

    #[test]
    fn base_key_list_matches_the_ts_key_object() {
        assert_eq!(BASE_KEYS.len(), 61);
        // `Key` 的顺序：特殊键 → f1..f12 → 符号键。
        assert_eq!(BASE_KEYS[0], "escape");
        assert_eq!(BASE_KEYS[17], "right");
        assert_eq!(BASE_KEYS[29], "f12");
        assert_eq!(BASE_KEYS[30], "`");
        assert_eq!(BASE_KEYS[60], "?");
    }
}
