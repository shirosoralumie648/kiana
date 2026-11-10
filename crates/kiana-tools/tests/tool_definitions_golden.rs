//! 8 个内置工具定义的黄金测试（fixture 由 TS 参考实现 `createAllToolDefinitions("/workspace")`
//! 的 `JSON.stringify(defs, null, 2)` 直接产出，见 `spec/ca-tools-compaction-export.md` §8.4：
//! 「8 个工具的 schema 与黄金 fixture 逐字节一致」「description/promptSnippet/promptGuidelines 逐字一致」）。
//!
//! 比较是**字节级**的：Rust 侧按 TS 的对象字面量键序重建同样的 JSON，再用
//! `serde_json::to_string_pretty`（与 `JSON.stringify(value, null, 2)` 同格式）输出，
//! 因此键顺序、`undefined` 字段的缺省、数组顺序都被固定。

use kiana_tools::index::{ToolName, create_all_tool_definitions};
use serde_json::{Map, Value};

const GOLDEN: &str = include_str!("fixtures/tool-definitions.golden.json");

fn definition_json(definition: &kiana_ext_api::tool_types::ToolDefinition) -> Value {
    let mut object = Map::new();
    object.insert("name".to_string(), Value::String(definition.name.clone()));
    object.insert("label".to_string(), Value::String(definition.label.clone()));
    object.insert(
        "description".to_string(),
        Value::String(definition.description.clone()),
    );
    if let Some(snippet) = &definition.prompt_snippet {
        object.insert("promptSnippet".to_string(), Value::String(snippet.clone()));
    }
    if let Some(guidelines) = &definition.prompt_guidelines {
        object.insert(
            "promptGuidelines".to_string(),
            Value::Array(guidelines.iter().cloned().map(Value::String).collect()),
        );
    }
    object.insert("parameters".to_string(), definition.parameters.clone());
    if let Some(output_schema) = &definition.output_schema {
        object.insert("outputSchema".to_string(), output_schema.clone());
    }
    if let Some(constrained_sampling) = &definition.constrained_sampling {
        object.insert(
            "constrainedSampling".to_string(),
            serde_json::to_value(constrained_sampling).expect("constrained sampling serializes"),
        );
    }
    Value::Object(object)
}

#[test]
fn all_tool_definitions_match_the_ts_golden_byte_for_byte() {
    let definitions = create_all_tool_definitions("/workspace", None);
    let mut actual = Map::new();
    for name in ToolName::ALL {
        let definition = definitions.get(&name).expect("every tool name is defined");
        actual.insert(name.as_str().to_string(), definition_json(definition));
    }
    let actual = serde_json::to_string_pretty(&Value::Object(actual)).expect("serializes");
    assert_eq!(
        actual,
        GOLDEN.trim_end(),
        "tool definitions drifted from the TS reference"
    );
}

#[test]
fn descriptions_are_the_verbatim_ts_texts() {
    let definitions = create_all_tool_definitions("/workspace", None);
    let expected = [
        (
            ToolName::Read,
            "Read the contents of a file. Supports text files and images (jpg, png, gif, webp, bmp). Images are sent as attachments. For text files, output is truncated to 2000 lines or 50KB (whichever is hit first). Use offset/limit for large files. When you need the full file, continue with offset until complete.",
        ),
        (
            ToolName::Bash,
            "Execute a bash command in the current working directory. Returns stdout and stderr. Output is truncated to last 2000 lines or 50KB (whichever is hit first). If truncated, full output is saved to a temp file. Optionally provide a timeout in seconds.",
        ),
        (
            ToolName::PowerShell,
            "Execute a PowerShell command in the current working directory. Returns stdout and stderr. Output is truncated to last 2000 lines or 50KB (whichever is hit first). If truncated, full output is saved to a temp file. Optionally provide a timeout in seconds.",
        ),
        (
            ToolName::Edit,
            "Edit a single file using exact text replacement. Every edits[].oldText must match a unique, non-overlapping region of the original file. If two changes affect the same block or nearby lines, merge them into one edit instead of emitting overlapping edits. Do not include large unchanged regions just to connect distant changes.",
        ),
        (
            ToolName::Write,
            "Write content to a file. Creates the file if it doesn't exist, overwrites if it does. Automatically creates parent directories.",
        ),
        (
            ToolName::Grep,
            "Search file contents for a pattern. Returns matching lines with file paths and line numbers. Respects .gitignore. Output is truncated to 100 matches or 50KB (whichever is hit first). Long lines are truncated to 500 chars.",
        ),
        (
            ToolName::Find,
            "Search for files by glob pattern. Returns matching file paths relative to the search directory. Respects .gitignore. Output is truncated to 1000 results or 50KB (whichever is hit first).",
        ),
        (
            ToolName::Ls,
            "List directory contents. Returns entries sorted alphabetically, with '/' suffix for directories. Includes dotfiles. Output is truncated to 500 entries or 50KB (whichever is hit first).",
        ),
    ];
    for (name, description) in expected {
        assert_eq!(
            definitions.get(&name).expect("defined").description,
            description,
            "{name}"
        );
    }
}

#[test]
fn every_definition_has_an_object_parameter_schema_and_no_additional_properties() {
    // TypeBox `Type.Object` 不写 `additionalProperties`（strict 由 provider 侧转换），
    // 这与 `builtin-tool-strict-mode.test.ts` 的「执行 schema 不变」一致。
    for (name, definition) in create_all_tool_definitions("/workspace", None) {
        assert_eq!(
            definition.parameters.get("type").and_then(Value::as_str),
            Some("object"),
            "{name}"
        );
        assert!(
            definition.parameters.get("additionalProperties").is_none(),
            "{name} must not declare additionalProperties"
        );
    }
}
