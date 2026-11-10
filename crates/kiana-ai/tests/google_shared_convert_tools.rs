//! `test/google-shared-convert-tools.test.ts` 的移植。

use kiana_ai::api::google_shared::{
    convert_tools, resolve_google_function_calling_mode, supports_google_strict_tool_sampling,
};
use kiana_ai::types::{ConstrainedSampling, ConstrainedSamplingConfig, StrictMode, Tool};
use serde_json::{Value, json};

fn make_tool(parameters: Value) -> Tool {
    Tool {
        name: "test_tool".into(),
        description: "A test tool".into(),
        parameters,
        constrained_sampling: None,
    }
}

fn declaration(result: &Option<Value>) -> Value {
    result.as_ref().expect("tools").as_array().expect("array")[0]
        .get("functionDeclarations")
        .and_then(Value::as_array)
        .expect("declarations")[0]
        .clone()
}

#[test]
fn strips_json_schema_meta_keys_from_parameters_when_use_parameters_is_true() {
    let tools = vec![make_tool(json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "$id": "urn:bash-tool",
        "$comment": "A bash tool for demonstration",
        "$defs": {"commandDef": {"type": "string"}},
        "definitions": {"legacyDef": {"type": "number"}},
        "type": "object",
        "properties": {"command": {"type": "string"}},
        "required": ["command"],
    }))];
    let result = convert_tools(&tools, true, true).expect("tools");
    let decl = declaration(&result);
    assert_eq!(
        decl["parameters"],
        json!({"type": "object", "properties": {"command": {"type": "string"}}, "required": ["command"]})
    );
    for key in ["$schema", "$id", "$comment", "$defs", "definitions"] {
        assert!(decl["parameters"].get(key).is_none(), "{key} removed");
    }
}

#[test]
fn recursively_strips_nested_json_schema_meta_keys() {
    let tools = vec![make_tool(json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "properties": {"deep": {"$schema": "s", "$id": "urn:nested", "type": "string"}},
    }))];
    let decl = declaration(&convert_tools(&tools, true, true).expect("tools"));
    assert_eq!(
        decl["parameters"],
        json!({"type": "object", "properties": {"deep": {"type": "string"}}})
    );
}

#[test]
fn preserves_ref_while_stripping_meta_keys() {
    let tools = vec![make_tool(json!({
        "$schema": "s",
        "type": "object",
        "properties": {"refProp": {"$ref": "#/$defs/someDef", "type": "string"}},
    }))];
    let decl = declaration(&convert_tools(&tools, true, true).expect("tools"));
    assert_eq!(
        decl["parameters"],
        json!({"type": "object", "properties": {"refProp": {"$ref": "#/$defs/someDef", "type": "string"}}})
    );
}

#[test]
fn does_not_mutate_the_original_tool_parameters() {
    let original = json!({
        "$schema": "s",
        "type": "object",
        "properties": {"command": {"type": "string"}},
        "required": ["command"],
    });
    let tools = vec![make_tool(original.clone())];
    convert_tools(&tools, true, true).expect("tools");
    assert_eq!(tools[0].parameters, original);
}

#[test]
fn preserves_schema_in_parameters_json_schema_when_use_parameters_is_false() {
    let tools = vec![make_tool(json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "properties": {"command": {"type": "string"}},
        "required": ["command"],
    }))];
    let decl = declaration(&convert_tools(&tools, false, true).expect("tools"));
    assert_eq!(
        decl["parametersJsonSchema"]["$schema"],
        json!("http://json-schema.org/draft-07/schema#")
    );
}

#[test]
fn handles_tools_without_schema_gracefully() {
    let tools = vec![make_tool(json!({
        "type": "object",
        "properties": {"path": {"type": "string"}},
        "required": ["path"],
    }))];
    let decl = declaration(&convert_tools(&tools, true, true).expect("tools"));
    assert_eq!(
        decl["parameters"],
        json!({"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]})
    );
}

#[test]
fn uses_validated_function_calling_for_strict_tools_on_gemini_3() {
    let mut tool = make_tool(json!({"type": "object", "properties": {}}));
    tool.constrained_sampling = Some(ConstrainedSampling::Config(
        ConstrainedSamplingConfig::JsonSchema {
            strict: StrictMode::Require,
        },
    ));
    assert!(supports_google_strict_tool_sampling(
        "gemini-3.1-pro-preview"
    ));
    assert!(!supports_google_strict_tool_sampling("gemini-2.5-pro"));
    assert_eq!(
        resolve_google_function_calling_mode(&[tool.clone()], None, true).expect("mode"),
        Some("VALIDATED")
    );
    let error =
        resolve_google_function_calling_mode(&[tool], None, false).expect_err("strict error");
    assert!(
        error
            .message
            .contains("Tool \"test_tool\" requires JSON-schema constrained sampling")
    );
}

#[test]
fn returns_none_for_empty_tool_list() {
    assert!(convert_tools(&[], false, true).expect("tools").is_none());
    assert!(convert_tools(&[], true, true).expect("tools").is_none());
}

#[test]
fn preserves_literal_data_under_non_schema_keywords() {
    for key in ["default", "examples", "enum", "const", "x-custom"] {
        let literal = json!({"const": "literal-data", "properties": {"nested": {"const": 1}}});
        let value = if key == "enum" || key == "examples" {
            json!([literal])
        } else {
            literal
        };
        let parameters = json!({
            "type": "object",
            "properties": {"value": {"type": "object", key: value}},
        });
        let original = parameters.clone();
        let decl = declaration(
            &convert_tools(&[make_tool(parameters.clone())], false, true).expect("tools"),
        );
        assert_eq!(decl["parametersJsonSchema"], original, "key {key}");
        assert_eq!(parameters, original);
    }
}

#[test]
fn preserves_names_while_rewriting_schemas_in_name_maps() {
    for key in [
        "properties",
        "patternProperties",
        "$defs",
        "definitions",
        "dependentSchemas",
        "dependencies",
    ] {
        let parameters = json!({
            "type": "object",
            key: {
                "__proto__": {"const": "x"},
                "const": {"const": "y"},
                "properties": {"const": "z"},
            },
        });
        let expected = json!({
            "type": "object",
            key: {
                "__proto__": {"enum": ["x"]},
                "const": {"enum": ["y"]},
                "properties": {"enum": ["z"]},
            },
        });
        let decl =
            declaration(&convert_tools(&[make_tool(parameters)], false, true).expect("tools"));
        assert_eq!(decl["parametersJsonSchema"], expected, "key {key}");
    }
}

#[test]
fn rewrites_only_schema_bearing_keywords() {
    let parameters = json!({
        "type": "object",
        "additionalProperties": {"const": "x"},
        "unevaluatedProperties": false,
        "propertyNames": {"const": "name"},
        "allOf": [{"if": {"const": "x"}, "then": {"const": "y"}, "else": {"not": {"const": "z"}}}],
        "properties": {
            "list": {
                "type": "array",
                "items": [{"const": 1}],
                "prefixItems": [{"const": 2}],
                "additionalItems": {"const": 3},
                "contains": {"const": 4},
                "unevaluatedItems": {"const": 5},
            }
        },
        "dependencies": {"list": ["other"]},
    });
    let decl = declaration(&convert_tools(&[make_tool(parameters)], false, true).expect("tools"));
    assert_eq!(
        decl["parametersJsonSchema"],
        json!({
            "type": "object",
            "additionalProperties": {"enum": ["x"]},
            "unevaluatedProperties": false,
            "propertyNames": {"enum": ["name"]},
            "allOf": [{"if": {"enum": ["x"]}, "then": {"enum": ["y"]}, "else": {"not": {"enum": ["z"]}}}],
            "properties": {
                "list": {
                    "type": "array",
                    "items": [{"enum": [1]}],
                    "prefixItems": [{"enum": [2]}],
                    "additionalItems": {"enum": [3]},
                    "contains": {"enum": [4]},
                    "unevaluatedItems": {"enum": [5]},
                }
            },
            "dependencies": {"list": ["other"]},
        })
    );
}

#[test]
fn rewrites_string_and_number_const_values_to_single_value_enums() {
    let parameters = json!({
        "type": "object",
        "properties": {
            "origin": {"type": "string", "const": "zq-src-7"},
            "mode": {"anyOf": [{"type": "string", "const": "a"}, {"type": "string", "const": "b"}]},
            "level": {"oneOf": [{"const": 1}, {"const": 2}]},
            "flag": {"type": "boolean", "const": true},
            "list": {"type": "array", "items": {"const": "x"}},
            "const": {"type": "string"},
        },
        "required": ["origin"],
    });
    let decl = declaration(&convert_tools(&[make_tool(parameters)], false, true).expect("tools"));
    assert_eq!(
        decl["parametersJsonSchema"],
        json!({
            "type": "object",
            "properties": {
                "origin": {"type": "string", "enum": ["zq-src-7"]},
                "mode": {"anyOf": [{"type": "string", "enum": ["a"]}, {"type": "string", "enum": ["b"]}]},
                "level": {"oneOf": [{"enum": [1]}, {"enum": [2]}]},
                "flag": {"type": "boolean", "const": true},
                "list": {"type": "array", "items": {"enum": ["x"]}},
                "const": {"type": "string"},
            },
            "required": ["origin"],
        })
    );
}

/// findings 回归：TS 的 `return toolChoice ? mapToolChoice(toolChoice) : undefined` 对空字符串
/// `toolChoice` 返回 `undefined`（不配置 functionCallingConfig.mode）。
#[test]
fn omits_function_calling_mode_for_empty_tool_choice() {
    let tool = make_tool(json!({"type": "object", "properties": {}}));
    assert_eq!(
        resolve_google_function_calling_mode(std::slice::from_ref(&tool), Some(""), false)
            .expect("mode"),
        None
    );
    assert_eq!(
        resolve_google_function_calling_mode(std::slice::from_ref(&tool), Some("auto"), false)
            .expect("mode"),
        Some("AUTO")
    );
    assert_eq!(
        resolve_google_function_calling_mode(std::slice::from_ref(&tool), Some("none"), false)
            .expect("mode"),
        Some("NONE")
    );
    assert_eq!(
        resolve_google_function_calling_mode(std::slice::from_ref(&tool), Some("any"), false)
            .expect("mode"),
        Some("ANY")
    );
    assert_eq!(
        resolve_google_function_calling_mode(std::slice::from_ref(&tool), None, false)
            .expect("mode"),
        None
    );
}
