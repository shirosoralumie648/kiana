//! `utils/json-parse.ts`——宽容 JSON 解析。
//!
//! `repairJson` / `parseJsonWithRepair` 已在 [`kiana_json::js_parse`] 中实现（同一份代码被
//! `kiana-json` 的黄金测试当作契约）；本模块只补 `kiana-ai` 侧的两个入口，并逐字复刻
//! `parseToolCallArguments` 的错误文本格式。
//!
//! TS: `utils/json-parse.ts`
//!
//! # 这个模块解决什么问题
//!
//! 服务商返回的 tool call 参数经常不是合法 JSON：转义写错（`\url`）、文本被截断、根值是
//! 数组/标量。参数最终要交给工具执行，因此需要区分两种诉求：
//!
//! - **流式展示**用 [`parse_streaming_json`]：输入可能不完整，永不失败，尽力给出当前快照；
//! - **最终判定**用 [`parse_tool_call_arguments`]：文本已完整，严格判断是否为 JSON 对象，
//!   失败时把错误文本放进 `arguments_error`，让模型/用户看到原文。
//!
//! 真正的修复算法（`repairJson`）与三级回退（partial-json）都在 `kiana-json` crate 里；
//! 本模块只做类型转换与 `kiana-ai` 侧的契约文本，避免同一套解析出现两个实现。
//!
//! # 被谁调用
//!
//! - [`crate::utils::streaming_json`] 的 `ToolCallArgumentsStream::finish()`：增量解析结束后
//!   的一次完整判定（多个 `api/*` 适配器在流末尾调用它）；
//! - `kiana-extensions` 的 virtual modules 直接调用 [`parse_streaming_json`]；
//! - `kiana-session-core` / `kiana-ai::api::system_one_shared` 等用 [`json_object_from_map`]
//!   在 `serde_json` 与 [`JsonObject`] 之间搬移。
//!
//! # 失败模式
//!
//! - [`parse_json_with_repair`] 返回 `Err(kiana_json::ParseError)`；`ParseError::message`
//!   逐字对齐 V8 `JSON.parse`（可能带 `at position ...` 后缀），调用方不要改写它再做比较。
//! - [`parse_streaming_json`] **不返回错误**：所有回退都失败时得到 `{}`。
//! - [`parse_tool_call_arguments`] **不返回 `Err`**：失败信息在 `arguments_error` 字段，
//!   调用方必须同时检查 `arguments` 与 `arguments_error`，不能只看参数是否为空对象。

use serde_json::Value;

use crate::types::JsonObject;

/// 把 `serde_json::Map`（`preserve_order` 下即 `IndexMap`）转成 [`JsonObject`]，保持键序。
///
/// `serde_json` 的 `Map` 与 `IndexMap` 是两种类型（前者带 `preserve_order` 特性开关），
/// 因此跨类型需要一次搬移。
///
/// 工作区启用了 `serde_json/preserve_order`（见根 `Cargo.toml`），所以 `Map` 的迭代顺序
/// 就是解析/插入顺序；本函数逐项搬移（O(n)），输入被消耗，返回的新表不与输入共享存储。
pub fn json_object_from_map(map: serde_json::Map<String, Value>) -> JsonObject {
    map.into_iter().collect()
}

/// `parseJsonWithRepair(json)`：先严格解析，失败后修复再试。
///
/// 流程（实现位于 [`kiana_json::js_parse::parse_with_repair`]）：
/// 1. 先按严格 JSON 解析，成功即返回；
/// 2. 失败则用 `repairJson` 修复字符串内的非法转义与控制字符，得到修复文本；
/// 3. 修复文本与原文不同 → 用修复文本重试；**若仍失败，返回的是修复后那次解析的错误**；
/// 4. 修复文本与原文相同（无法修复）→ 返回最初的严格解析错误。
///
/// 适用场景是“文本已经完整”（例如 tool call 参数收完）。流式过程中的未完成文本应使用
/// [`parse_streaming_json`]，它还能容忍截断；本函数对 `{"a":` 这类不完整输入会返回 `Err`。
///
/// TS: `utils/json-parse.ts#parseJsonWithRepair`
pub fn parse_json_with_repair(json: &str) -> Result<Value, kiana_json::ParseError> {
    kiana_json::js_parse::parse_with_repair(json)
}

/// `parseStreamingJson(partialJson)`：把（可能未完成的）流式 JSON 解析成当前快照。
///
/// 空/纯空白 → `{}`；否则 `parseJsonWithRepair` → 失败则 `partial-json` 回退（原文、再修复文）
/// → 再失败 `{}`。`(result ?? {})` 的 null 替换已在 [`kiana_json::js_parse::parse_streaming`] 内。
///
/// # 为什么永不失败
///
/// 这是增量 UI 预览的数据源：每次收到文本片段后都可能取快照，输入在任何时刻都可能是被截断
/// 的。因此这里把“解析失败”折叠成 `{}`，而不是把错误抛给上层。需要区分“空对象”与“非法
/// 文本”的地方，必须在流结束时改用 [`parse_tool_call_arguments`]。
///
/// 返回值的第二个分量（`true` 表示来自严格/修复解析，`false` 表示来自 partial-json 回退或
/// 空输入）被丢弃，因为 TS 的 `parseStreamingJson` 不暴露它；`kiana-ai` 只使用值本身。
/// `None` 与 `Some("")` 都返回 `{}`，调用方不需要先判空。
///
/// TS: `utils/json-parse.ts#parseStreamingJson`
pub fn parse_streaming_json(partial_json: Option<&str>) -> Value {
    match partial_json {
        None => Value::Object(serde_json::Map::new()),
        Some(text) => kiana_json::js_parse::parse_streaming(text).0,
    }
}

/// `parseToolCallArguments(json)` 的返回值。
///
/// `Default`（空参数 + 无错误）与空输入、`None` 输入的返回形状相同，调用方可以把它当作
/// “没有参数”的初始值；但**不能**用 `arguments.is_empty()` 判断解析是否失败——合法的
/// `{}` 与解析失败都会得到空对象，必须看 `arguments_error`。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ParsedToolCallArguments {
    /// 解析出的参数；失败时是空对象。
    pub arguments: JsonObject,
    /// 失败时的错误文本（`<message>\n\nReceived arguments:\n<json>`）。
    pub arguments_error: Option<String>,
}

/// `Tool arguments must be a JSON object`（非对象根值时的错误文本）。
///
/// 这是 TS `Error` 的 `message` 原文，会被拼进 `arguments_error`；`crates/kiana-ai/tests/json_parse.rs`
/// 直接用它做断言，因此文本逐字不可改（包括大小写与 `JSON` 的全大写）。
pub const TOOL_ARGUMENTS_NOT_OBJECT: &str = "Tool arguments must be a JSON object";

/// `parseToolCallArguments(json)`：完整文本 → `{arguments, argumentsError}`。
///
/// 规则（`spec/ai-core.md` §2.2.25）：
/// 1. `!json.trim()` → `{arguments:{}}`（**无** `argumentsError`）；
/// 2. `parseJsonWithRepair`；结果必须是**非 null、非数组的 object**，否则抛
///    [`TOOL_ARGUMENTS_NOT_OBJECT`]；
/// 3. 失败 → `{arguments:{}, argumentsError: "<message>\n\nReceived arguments:\n<json>"}`。
///
/// # 关键行为
///
/// - `None` 与空/纯空白文本都返回空参数且**无错误**；区别只在调用方语义（前者表示未提供）。
///   空判断用的是 [`kiana_json::js_trim`]，即 JS 的空白集合（含 U+00A0/U+FEFF），不是
///   Rust 的 `str::trim`。
/// - 只有完整解析成功且根值是非数组对象才算成功；被截断的文本不会“保留已解析前缀”，而是
///   整体失败（测试 `does_not_keep_the_valid_prefix_of_malformed_json` 锁定）。
/// - 错误文本 = `ParseError.message`（或 [`TOOL_ARGUMENTS_NOT_OBJECT`]）+ 两个换行 +
///   `Received arguments:` + 换行 + **原始输入**（不 trim、不截断、不做 JSON 转义）。
///   原文可能很长，调用方展示前应自行考虑长度控制。
/// - 本函数不重试、不联网、无副作用；`json` 参数为 `None` 与 `Some("")` 返回同一个形状。
///
/// TS: `utils/json-parse.ts#parseToolCallArguments`
pub fn parse_tool_call_arguments(json: Option<&str>) -> ParsedToolCallArguments {
    let Some(json) = json else {
        return ParsedToolCallArguments::default();
    };
    if kiana_json::js_trim(json).is_empty() {
        return ParsedToolCallArguments {
            arguments: JsonObject::new(),
            arguments_error: None,
        };
    }
    match kiana_json::js_parse::parse_with_repair(json) {
        Ok(Value::Object(map)) => ParsedToolCallArguments {
            arguments: json_object_from_map(map),
            arguments_error: None,
        },
        Ok(_) => ParsedToolCallArguments {
            arguments: JsonObject::new(),
            arguments_error: Some(format!(
                "{TOOL_ARGUMENTS_NOT_OBJECT}\n\nReceived arguments:\n{json}"
            )),
        },
        Err(error) => ParsedToolCallArguments {
            arguments: JsonObject::new(),
            arguments_error: Some(format!("{}\n\nReceived arguments:\n{json}", error.message)),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_complete_json_and_repairs_invalid_escapes() {
        let parsed = parse_tool_call_arguments(Some(r#"{"kind":"seek","note":"\url"}"#));
        assert_eq!(parsed.arguments_error, None);
        assert_eq!(
            parsed.arguments.get("note").and_then(Value::as_str),
            Some("\\url")
        );
    }

    #[test]
    fn uses_empty_arguments_for_empty_text() {
        assert_eq!(
            parse_tool_call_arguments(Some("")),
            ParsedToolCallArguments::default()
        );
        assert_eq!(
            parse_tool_call_arguments(None),
            ParsedToolCallArguments::default()
        );
        assert_eq!(
            parse_tool_call_arguments(Some("  \n\t ")),
            ParsedToolCallArguments::default()
        );
    }

    #[test]
    fn does_not_keep_the_valid_prefix_of_malformed_json() {
        let raw = r#"{"action":"apply","actions":[{"kind":"seek","time":.5}]}"#;
        let parsed = parse_tool_call_arguments(Some(raw));
        assert!(parsed.arguments.is_empty());
        let error = parsed.arguments_error.expect("error");
        assert!(error.contains("\n\nReceived arguments:\n"));
        assert!(error.ends_with(raw));
    }

    #[test]
    fn does_not_complete_cut_off_json() {
        let raw = r#"{"kind":"seek","clipId":"clip-fl"#;
        let parsed = parse_tool_call_arguments(Some(raw));
        assert!(parsed.arguments.is_empty());
        assert!(parsed.arguments_error.expect("error").ends_with(raw));
    }

    #[test]
    fn rejects_json_that_is_not_an_object() {
        assert_eq!(
            parse_tool_call_arguments(Some("[1,2]"))
                .arguments_error
                .as_deref(),
            Some("Tool arguments must be a JSON object\n\nReceived arguments:\n[1,2]")
        );
        assert!(
            parse_tool_call_arguments(Some("null"))
                .arguments_error
                .is_some()
        );
        assert!(
            parse_tool_call_arguments(Some("\"x\""))
                .arguments_error
                .is_some()
        );
    }

    #[test]
    fn streaming_keeps_invalid_unicode_escapes_literal() {
        assert_eq!(
            parse_streaming_json(Some(r#"{"text":"\url{x}"}"#)),
            serde_json::json!({"text": "\\url{x}"})
        );
        assert_eq!(
            parse_streaming_json(Some(r#"{"text":"café \url"}"#)),
            serde_json::json!({"text": "café \\url"})
        );
        assert_eq!(
            parse_streaming_json(Some(r#"{"text":"ab\u00"#)),
            serde_json::json!({"text": "ab"})
        );
    }
}
