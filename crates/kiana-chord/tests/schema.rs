//! `packages/chord/src/schema.ts` 的金标准测试。
//!
//! TS 侧没有 `schema.test.ts`：`schema.ts` 的行为由 `contract.test.ts` 的
//! `normalize`/`canonicalIdentity` 间接覆盖。本文件直接吃
//! `fixtures/schema_golden.json` 与 `fixtures/schema_errors.json`（由
//! `scratch/kiana-chord__core/gen_golden.mjs` / `gen_golden2.mjs` 在 TypeBox 1.3.27 + TS 参考实现上
//! 生成），覆盖：
//!
//! - `hash64`（FNV 双通道，UTF-16 码元 + `Math.imul` 回绕）；
//! - `canonicalJson`（键排序、`undefined` 丢弃、JS 数字格式、整数样式键）；
//! - `check` / `errors` 的**全部** 14×16 组合中被 TS 拒绝的那些（keyword/schemaPath/instancePath/
//!   params/message 逐字段）；
//! - `normalize`（Default + Clean）与 `assertValid` 的摘要文本；
//! - `fits` 的函数参数逆变/返回协变。
//!
//! `schemas` 段是 TypeBox 生成物的 **JSON 形态**（`~kind` 不可枚举，JSON 里没有它）：
//! `Default`/`Clean` 按 `~kind` 分派，所以本文件在调用前用 `mark_live_schema` 还原活 schema。

use kiana_chord::errors::ChordErrorCode;
use kiana_chord::schema::{
    assert_valid, canonical_json, check, clean_value, default_value, errors, fits, hash64,
    mark_live_schema, normalize, raw_errors,
};
use serde_json::{Value, json};

fn golden() -> Value {
    serde_json::from_str(include_str!("fixtures/schema_golden.json")).expect("schema golden")
}

fn error_golden() -> Value {
    serde_json::from_str(include_str!("fixtures/schema_errors.json")).expect("schema errors")
}

// ── hash64 ──────────────────────────────────────────────────────────────────────

#[test]
fn hash64_matches_the_typescript_reference() {
    let golden = golden();
    for (name, text) in [
        ("empty", String::new()),
        ("hello", "hello".to_owned()),
        ("unicode", "héllo 😀 世界".to_owned()),
        ("nul", "a\0b".to_owned()),
        ("long", "x".repeat(1000)),
    ] {
        assert_eq!(
            hash64(&text),
            golden["hash64"][name].as_str().expect("hash"),
            "hash64({name})"
        );
    }
}

// ── canonicalJson ───────────────────────────────────────────────────────────────

#[test]
fn canonical_json_matches_the_typescript_reference() {
    let golden = golden();
    let cases = [
        ("nested", json!({"b": 1, "a": [1, {"z": null, "y": true}]})),
        ("undefinedDropped", json!({"a": 1, "c": {"e": 2}})),
        ("arrayUndefined", json!([1, null, 3])),
        (
            "numbers",
            json!({"a": 1e21_f64, "b": 1e-7_f64, "c": 0, "d": 1.5_f64, "e": 1e20_f64}),
        ),
        ("keys", json!({"10": 1, "2": 2, "b": 3, "a": 4})),
    ];
    for (name, value) in cases {
        assert_eq!(
            canonical_json(&value),
            golden["canonicalJson"][name].as_str().expect("json"),
            "canonicalJson({name})"
        );
    }
}

// ── check / errors：14 个 schema × 16 个值的完整叉积 ──────────────────────────────

#[test]
fn check_and_errors_match_typescript_for_the_whole_cross_product() {
    let golden = golden();
    let schemas = golden["schemas"].as_object().expect("schemas");
    let values = golden["values"].as_object().expect("values");
    assert_eq!(schemas.len(), 14, "the fixture enumerates every schema");
    assert_eq!(values.len(), 16, "the fixture enumerates every value");

    let mut rejected = 0usize;
    for (schema_name, schema) in schemas {
        let expected_for_schema = golden["check"][schema_name]
            .as_object()
            .expect("the golden records the rejected combinations per schema")
            .clone();
        // 夹具只记录 `!ok` 的组合：缺席即被接受。
        for key in expected_for_schema.keys() {
            assert!(
                values.contains_key(key),
                "{schema_name}: unknown value {key} in the golden"
            );
        }
        for (value_name, value) in values {
            let expected = expected_for_schema.get(value_name);
            let accepted = check(schema, value);
            let Some(expected) = expected else {
                assert!(accepted, "{schema_name} / {value_name} should be accepted");
                assert_eq!(
                    errors(schema, value),
                    Vec::<Value>::new(),
                    "{schema_name} / {value_name}: an accepted value yields no errors"
                );
                continue;
            };
            rejected += 1;
            assert!(!accepted, "{schema_name} / {value_name} should be rejected");
            let actual = Value::Array(errors(schema, value));
            assert_eq!(
                actual, expected["errors"],
                "{schema_name} / {value_name} errors"
            );
            // `raw_errors` 是同一趟遍历的未本地化版本：条数必须一致。
            assert_eq!(
                raw_errors(schema, value).len(),
                actual.as_array().expect("array").len(),
                "{schema_name} / {value_name}: raw_errors count"
            );
        }
    }
    assert_eq!(
        rejected, 205,
        "the golden carries every rejected combination"
    );
}

// ── 详细的 keyword/params 夹具（22 条，含 TypeBox 的多条同路径错误） ─────────────

#[test]
fn detailed_error_fixtures_match_typescript() {
    let golden = golden();
    let schemas = golden["schemas"].as_object().expect("schemas").clone();
    let values = golden["values"].as_object().expect("values").clone();
    let fixture = error_golden();
    let fixture = fixture.as_object().expect("error fixture");

    let mut cases = 0usize;
    for (name, expected) in fixture {
        // `fits` 是另一段夹具（见下面的测试）。
        if name == "fits" {
            continue;
        }
        let (schema_name, value_name) = name.split_once('.').expect("`schema.value`");
        let schema = &schemas[schema_name];
        let value = &values[value_name];
        let ok = expected["ok"].as_bool().expect("ok");
        assert_eq!(check(schema, value), ok, "case {name}: ok");
        assert_eq!(
            Value::Array(errors(schema, value)),
            expected["errors"],
            "case {name} errors"
        );
        cases += 1;
    }
    assert_eq!(cases, 22, "the fixture enumerates the interesting cases");
}

// ── Default + Clean 的原始管线（夹具的 `normalize` 段） ───────────────────────────

/// 夹具里的 `normalize` 段是 **TypeBox 原始管线**（`Clean(Default(Clone(value ?? {})))`，
/// 不校验）：`gen_golden.mjs` 用的是它自己的 `normalize`，不是 `schema.ts` 里带
/// `assertValid` 的那一个。要区分开，否则 `nullInput`（缺 `session`）在 TS 侧也应当抛错。
///
/// 夹具里的 schema 是 TypeBox 构造物的 JSON 形态（`~kind` 不可枚举，序列化后消失），
/// 而 `Default`/`Clean` 按 `~kind` 分派；因此这里先用 [`mark_live_schema`] 还原"本进程内
/// 构造的活 schema"，与 TS 侧一致。
fn pipeline(schema: &Value, value: Option<&Value>) -> Value {
    let schema = mark_live_schema(schema);
    let seed = match value {
        Some(value) => value.clone(),
        None => Value::Object(serde_json::Map::new()),
    };
    clean_value(&schema, default_value(&schema, seed))
}

#[test]
fn default_and_clean_match_the_typescript_pipeline() {
    let golden = golden();
    let schemas = &golden["schemas"];
    let expected = &golden["normalize"];

    assert_eq!(
        pipeline(
            &schemas["chatInput"],
            Some(&json!({"session": "s", "extra": 1}))
        ),
        expected["chatDefaults"],
        "defaults filled (unknown keys are kept: chatInput has no additionalProperties: false)"
    );
    assert_eq!(
        pipeline(
            &schemas["chatInput"],
            Some(&json!({"session": "s", "conversation": 3}))
        ),
        expected["chatGiven"],
        "an explicit value is not overwritten by its default"
    );
    assert_eq!(
        pipeline(&schemas["chatInput"], None),
        expected["nullInput"],
        "`value ?? {{}}`: a missing input is an empty object"
    );
    assert_eq!(
        pipeline(&schemas["optionalProp"], Some(&json!({"a": "x", "b": 1}))),
        expected["cleaned"],
        "`additionalProperties: false` strips `b`"
    );
    assert_eq!(
        pipeline(
            &json!({"type": "object", "properties": {"a": {"type": "object", "properties": {"b": {"type": "number", "default": 7}}}}}),
            Some(&json!({}))
        ),
        expected["nested"],
        "Default does not invent a missing parent object"
    );
}

// ── normalize（Default + Clean + assertValid） ──────────────────────────────────

#[test]
fn normalize_validates_after_defaulting() {
    let golden = golden();
    let schemas = &golden["schemas"];

    // 合法输入：`conversation` 用默认值补齐（活 schema 才注入默认值，见 `pipeline`）。
    let live = mark_live_schema(&schemas["chatInput"]);
    assert_eq!(
        normalize(&live, Some(&json!({"session": "s"})), "input").expect("normalize"),
        json!({"session": "s", "conversation": 0})
    );
    // `Clean` 去掉多余字段后仍然合法。
    let live = mark_live_schema(&schemas["optionalProp"]);
    assert_eq!(
        normalize(&live, Some(&json!({"a": "x", "b": 1})), "input").expect("normalize"),
        json!({"a": "x"})
    );

    // 失败路径：`invalid/input`；摘要文本（`input: /session must be string`）是稳定契约。
    let live_chat = mark_live_schema(&schemas["chatInput"]);
    let error =
        normalize(&live_chat, Some(&json!({"session": 1})), "input").expect_err("wrong type");
    assert_eq!(error.code, ChordErrorCode::Invalid);
    assert_eq!(error.reason.as_deref(), Some("input"));
    assert_eq!(error.message, "input: /session must be string");

    // 缺必填字段（`nullInput` 的原始管线结果是 `{conversation: 0}`）在这里被拒绝。
    let error = normalize(&live_chat, None, "input").expect_err("missing session");
    assert_eq!(error.code, ChordErrorCode::Invalid);
    assert_eq!(
        error.message,
        "input: / must have required properties session"
    );
}

// ── assertValid 的摘要文本 ──────────────────────────────────────────────────────

#[test]
fn assert_valid_summary_matches_typescript() {
    let golden = golden();
    let expected = &golden["assertValid"];
    let schemas = &golden["schemas"];
    let values = &golden["values"];
    let mut checked = 0usize;
    for (name, schema_name) in [
        ("wrongType", "chatInput"),
        ("extra", "chatInput"),
        ("notObject", "chatInput"),
        ("short", "str"),
        ("mixed", "str"),
    ] {
        let schema = &schemas[schema_name];
        let value = &values[name];
        let expected_summary = expected[name].as_str().expect("summary");
        match assert_valid("input", "must be valid", schema, value) {
            Ok(()) => {
                // `extra` 对 `chatInput` 是合法的（没有 `additionalProperties: false`）。
                assert_eq!(expected_summary, "", "assertValid({name}) passes");
            }
            Err(error) => {
                checked += 1;
                assert_eq!(error.code, ChordErrorCode::Invalid);
                assert_eq!(error.reason.as_deref(), Some("input"));
                let summary = error
                    .message
                    .strip_prefix("must be valid: ")
                    .expect("`what: summary`");
                assert_eq!(summary, expected_summary, "assertValid summary for {name}");
                // `details` 是前 3 条错误（TS 的 `invalid(reason, message, errors)`）。
                let details = error.details.as_ref().expect("details");
                let head = details.as_array().expect("array");
                assert!(head.len() <= 3);
                assert_eq!(
                    head.len(),
                    errors(schema, value).len().min(3),
                    "details carries at most 3 errors"
                );
            }
        }
    }
    assert_eq!(checked, 4, "four of the five cases are invalid");

    // 合法值不报错。
    assert!(
        assert_valid(
            "input",
            "must be valid",
            &schemas["chatInput"],
            &values["ok"]
        )
        .is_ok()
    );
}

// ── fits（契约覆盖的比较器） ────────────────────────────────────────────────────

#[test]
fn fits_matches_typescript() {
    let golden = golden();
    let schemas = &golden["schemas"];
    let pairs = [
        ("fnUnknownToFnString", "fnUnknown", "fnString"),
        ("fnStringToFnUnknown", "fnString", "fnUnknown"),
        ("textViewToNumberView", "textView", "numberView"),
        ("numberViewToTextView", "numberView", "textView"),
        ("chatToSessionOnly", "chatInput", "sessionOnly"),
        ("sessionOnlyToChat", "sessionOnly", "chatInput"),
        ("emptyToEmpty", "emptyObj", "emptyObj"),
    ];
    for (name, sub, sup) in pairs {
        assert_eq!(
            fits(&schemas[sub], &schemas[sup]),
            golden["fits"][name].as_bool().expect("bool"),
            "fits({sub}, {sup})"
        );
        // 第二份夹具（`schema_errors.json` 的 `fits` 段）给出同样的结论。
        assert_eq!(
            fits(&schemas[sub], &schemas[sup]),
            error_golden()["fits"][name].as_bool().expect("bool"),
            "fits({sub}, {sup}) from the second fixture"
        );
    }
}

// ── reviewer 回归：Intersect / Ref / Cyclic 的 Default + Clean ──────────────────

#[test]
fn intersect_default_and_clean_match_typebox() {
    // 单分支 `Type.Intersect`：Evaluate 还原成 Object，默认值与 additionalProperties 都生效。
    let single = json!({
        "allOf": [{
            "type": "object",
            "required": ["a"],
            "properties": {"a": {"type": "number", "default": 1}}
        }]
    });
    assert_eq!(pipeline(&single, Some(&json!({}))), json!({"a": 1}));
    assert_eq!(pipeline(&single, Some(&json!({"a": 5}))), json!({"a": 5}));

    let closed = json!({
        "allOf": [{
            "type": "object",
            "required": ["a"],
            "properties": {"a": {"type": "number"}},
            "additionalProperties": false
        }]
    });
    assert_eq!(
        pipeline(&closed, Some(&json!({"a": 1, "b": 2}))),
        json!({"a": 1})
    );

    // 两分支：`Composite` 合并属性（每侧 required 的并集）。
    let merged = json!({
        "allOf": [
            {"type": "object", "required": ["a"], "properties": {"a": {"type": "number", "default": 1}}},
            {"type": "object", "required": ["b"], "properties": {"b": {"type": "string", "default": "s"}}}
        ]
    });
    assert_eq!(
        pipeline(&merged, Some(&json!({}))),
        json!({"a": 1, "b": "s"})
    );
    assert_eq!(
        pipeline(&merged, Some(&json!({"a": 1, "b": "x", "c": 3}))),
        json!({"a": 1, "b": "x"})
    );

    // Clean 把 Intersect 节点上的 `unevaluatedProperties` 重新解释成 `additionalProperties`。
    let unevaluated = json!({
        "allOf": [{"type": "object", "required": ["a"], "properties": {"a": {"type": "number"}}}],
        "unevaluatedProperties": false
    });
    assert_eq!(
        pipeline(&unevaluated, Some(&json!({"a": 1, "b": 2}))),
        json!({"a": 1})
    );

    // `Union` 分支在 Distribute 里分发，再与另一分支 Composite。
    let distributed = json!({
        "allOf": [
            {"anyOf": [
                {"type": "object", "required": ["x"], "properties": {"x": {"type": "number"}}},
                {"type": "object", "required": ["y"], "properties": {"y": {"type": "string"}}}
            ]},
            {"type": "object", "required": ["z"], "properties": {"z": {"type": "boolean"}}}
        ]
    });
    assert_eq!(
        pipeline(&distributed, Some(&json!({"x": 1, "z": true, "extra": 1}))),
        json!({"x": 1, "z": true})
    );
    assert_eq!(
        pipeline(
            &distributed,
            Some(&json!({"y": "s", "z": true, "extra": 1}))
        ),
        json!({"y": "s", "z": true})
    );
}

#[test]
fn cyclic_and_ref_default_and_clean_match_typebox() {
    // `Type.Cyclic({node: Object(...)}, 'node')` 的 JSON 形态：`$defs` 里每个定义带 `$id`。
    let cyclic = json!({
        "$defs": {
            "node": {
                "$id": "node",
                "type": "object",
                "required": ["value"],
                "properties": {"value": {"type": "number", "default": 1}}
            }
        },
        "$ref": "node"
    });
    assert!(!check(&cyclic, &json!({})), "Cyclic requires value");
    let cyclic_live = mark_live_schema(&cyclic);
    assert_eq!(
        normalize(&cyclic_live, Some(&json!({})), "input").expect("cyclic normalize"),
        json!({"value": 1})
    );
    assert_eq!(
        pipeline(&cyclic, Some(&json!({"value": 2, "extra": 3}))),
        json!({"value": 2})
    );

    // 递归定义：嵌套的 `Ref` 在 Cyclic 的上下文里解析。
    let recursive = json!({
        "$defs": {
            "node": {
                "$id": "node",
                "type": "object",
                "required": ["value"],
                "properties": {
                    "value": {"type": "number", "default": 1},
                    "next": {"$ref": "node"}
                }
            }
        },
        "$ref": "node"
    });
    assert_eq!(
        pipeline(&recursive, Some(&json!({"next": {}}))),
        json!({"value": 1, "next": {"value": 1}})
    );

    // 无上下文的 `Type.Ref`：Default/Clean 原样返回，Check 解析不到 `$ref` → false。
    let reference = json!({"$ref": "node"});
    assert!(!check(&reference, &json!({"a": 1})));
    assert_eq!(
        pipeline(&reference, Some(&json!({"a": 1}))),
        json!({"a": 1})
    );
    let error = normalize(&reference, Some(&json!({"a": 1})), "input").expect_err("dangling ref");
    assert_eq!(error.code, ChordErrorCode::Invalid);
    assert_eq!(error.message, "input: / schema is false");

    // 纯 JSON（无 `~kind`）的悬空 `$ref` 同样不匹配。
    assert!(!check(&json!({"$ref": "node"}), &json!({})));
}

// ── reviewer 回归：Value.Clone 的键序与不安全键 ────────────────────────────────

#[test]
fn clone_and_clean_reproduce_javascript_key_order_and_drop_unsafe_keys() {
    // `Clone` 按 `Guard.Keys` 重建普通对象：数组索引键升序在前，其余保持插入序。
    let value: Value = serde_json::from_str(r#"{"b":1,"2":2,"1":3}"#).expect("json");
    let normalized = normalize(&json!({}), Some(&value), "input").expect("normalize");
    assert_eq!(
        serde_json::to_string(&normalized).expect("json"),
        r#"{"1":3,"2":2,"b":1}"#
    );

    // `Clone` 跳过 `__proto__`/`constructor`/`prototype`（防原型污染）。
    let unsafe_value: Value =
        serde_json::from_str(r#"{"constructor":123,"a":1,"__proto__":2}"#).expect("json");
    let normalized = normalize(&json!({}), Some(&unsafe_value), "input").expect("normalize");
    assert_eq!(
        serde_json::to_string(&normalized).expect("json"),
        r#"{"a":1}"#
    );

    // 活 Object schema 的 Clean 也按 JS 枚举序重建输出对象。
    let live = mark_live_schema(&json!({
        "type": "object",
        "required": ["1", "2", "b"],
        "properties": {
            "1": {"type": "number"},
            "2": {"type": "number"},
            "b": {"type": "number"}
        }
    }));
    let cleaned = clean_value(&live, value);
    assert_eq!(
        serde_json::to_string(&cleaned).expect("json"),
        r#"{"1":3,"2":2,"b":1}"#
    );
}

// ── reviewer 回归：#1 Union 优先级 / #3 Object+patternProperties / #11 自定义 Record ──

#[test]
fn union_priority_sort_keeps_the_first_disjoint_branch() {
    // `Comparer == 1`（disjoint）时已有分支在前：Clean 取第一个能通过 Check 的分支。
    let first_string = json!({
        "anyOf": [
            {"type": "object", "required": ["a"], "properties": {"a": {"type": "string"}}},
            {"type": "object", "required": ["b"], "properties": {"b": {"type": "number"}}}
        ]
    });
    assert_eq!(
        pipeline(&first_string, Some(&json!({"a": "x", "b": 1}))),
        json!({"a": "x"})
    );
    let first_number = json!({
        "anyOf": [
            {"type": "object", "required": ["b"], "properties": {"b": {"type": "number"}}},
            {"type": "object", "required": ["a"], "properties": {"a": {"type": "string"}}}
        ]
    });
    assert_eq!(
        pipeline(&first_number, Some(&json!({"a": "x", "b": 1}))),
        json!({"b": 1})
    );
}

#[test]
fn object_with_pattern_properties_stays_an_object() {
    // `Type.Object(..., { patternProperties })` 是 Object（不是 Record）：默认值照常注入，
    // 未声明的键在 Clean 时被丢弃。
    let schema = json!({
        "type": "object",
        "properties": {"a": {"type": "number", "default": 7}},
        "patternProperties": {"^x": {"type": "string"}}
    });
    assert_eq!(pipeline(&schema, Some(&json!({}))), json!({"a": 7}));
    assert_eq!(
        pipeline(&schema, Some(&json!({"x1": "s"}))),
        json!({"a": 7})
    );
    assert_eq!(
        pipeline(&schema, Some(&json!({"a": 1, "x1": "s", "other": "z"}))),
        json!({"a": 1})
    );
}

#[test]
fn record_with_a_custom_pattern_cleans_every_key() {
    // `UnionPrioritySort` 把自定义 pattern 经 `RecordKey` 退化成 `^.*$`，Clean 保留所有键。
    let schema = json!({
        "type": "object",
        "patternProperties": {"^x": {"type": "number"}}
    });
    assert_eq!(
        pipeline(&schema, Some(&json!({"^x": 1, "x1": 2}))),
        json!({"^x": 1, "x1": 2})
    );
    assert_eq!(pipeline(&schema, Some(&json!({"^x": 1}))), json!({"^x": 1}));
}

// ── reviewer 回归：#5 unevaluatedProperties / unevaluatedItems ─────────────────

#[test]
fn unevaluated_properties_and_items_are_checked() {
    let object_schema = json!({
        "type": "object",
        "properties": {"a": {}},
        "unevaluatedProperties": false
    });
    assert!(check(&object_schema, &json!({"a": 1})));
    assert!(!check(&object_schema, &json!({"a": 1, "c": 2})));
    let found = errors(&object_schema, &json!({"a": 1, "c": 2}));
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0]["keyword"], "unevaluatedProperties");
    assert_eq!(found[0]["schemaPath"], "#");
    assert_eq!(found[0]["params"]["unevaluatedProperties"], json!(["c"]));
    assert_eq!(found[0]["message"], "must not have unevaluated properties");
    // Clean 不会删除 `unevaluatedProperties` 排除的键：normalize 报错。
    let error = normalize(&object_schema, Some(&json!({"a": 1, "c": 2})), "input")
        .expect_err("unevaluated properties");
    assert_eq!(
        error.message,
        "input: / must not have unevaluated properties"
    );

    // 元组形式的 `items` 只求值到前缀长度。
    let items_schema = json!({
        "type": "array",
        "items": [{"type": "number"}],
        "unevaluatedItems": false
    });
    assert!(check(&items_schema, &json!([1])));
    assert!(!check(&items_schema, &json!([1, 2])));
    let found = errors(&items_schema, &json!([1, 2]));
    assert_eq!(found[0]["keyword"], "unevaluatedItems");
    assert_eq!(found[0]["params"]["unevaluatedItems"], json!([1]));

    // `allOf` 分支求值过的属性不再算 unevaluated。
    let all_of = json!({
        "allOf": [{"type": "object", "properties": {"a": {}}}],
        "unevaluatedProperties": false
    });
    assert!(check(&all_of, &json!({"a": 1})));
    assert!(!check(&all_of, &json!({"a": 1, "b": 2})));
    let found = errors(&all_of, &json!({"a": 1, "b": 2}));
    assert_eq!(found[0]["params"]["unevaluatedProperties"], json!(["b"]));
}

// ── reviewer 回归：#6 dependencies 的字符串数组形式 ───────────────────────────

#[test]
fn dependencies_accept_the_string_array_form() {
    let schema = json!({
        "type": "object",
        "properties": {"a": {}},
        "dependencies": {"a": ["b"]}
    });
    assert!(!check(&schema, &json!({"a": 1})));
    let found = errors(&schema, &json!({"a": 1}));
    assert_eq!(found[0]["keyword"], "dependencies");
    assert_eq!(found[0]["params"]["property"], "a");
    assert_eq!(found[0]["params"]["dependencies"], json!(["b"]));
    assert_eq!(
        found[0]["message"],
        "must have properties b when property a is present"
    );
    assert!(check(&schema, &json!({"a": 1, "b": 2})));

    // schema 与字符串数组混合的映射。
    let mixed = json!({
        "type": "object",
        "properties": {"a": {}, "c": {}},
        "dependencies": {"a": ["b"], "c": {"required": ["d"]}}
    });
    let found = errors(&mixed, &json!({"a": 1}));
    assert_eq!(found[0]["keyword"], "dependencies");
    let found = errors(&mixed, &json!({"c": 1}));
    assert_eq!(found[0]["keyword"], "required");
    assert_eq!(found[0]["schemaPath"], "#/dependencies/c");
}

// ── reviewer 回归：#8 if/then 的错误抑制 ──────────────────────────────────────

#[test]
fn if_then_emits_only_the_if_error() {
    let schema = json!({
        "type": "object",
        "required": ["kind"],
        "properties": {"kind": {"type": "string"}, "x": {"type": "number"}},
        "if": {"type": "object", "required": ["kind"], "properties": {"kind": {"const": "a"}}},
        "then": {"required": ["x"]}
    });
    let found = errors(&schema, &json!({"kind": "a"}));
    assert_eq!(found.len(), 1, "then-schema found are discarded: {found:?}");
    assert_eq!(found[0]["keyword"], "if");
    assert_eq!(found[0]["params"]["failingKeyword"], "then");
    assert_eq!(found[0]["message"], "must match \"then\" schema");
    let error = normalize(&schema, Some(&json!({"kind": "a"})), "input").expect_err("then");
    assert_eq!(error.message, "input: / must match \"then\" schema");

    // `else` 分支的错误保留在主流里，外加一条 `if` 错误。
    let schema = json!({
        "type": "object",
        "required": ["kind"],
        "properties": {"kind": {"type": "string"}, "x": {"type": "number"}},
        "if": {"type": "object", "required": ["kind"], "properties": {"kind": {"const": "a"}}},
        "then": {"required": ["x"]},
        "else": {"required": ["y"]}
    });
    let found = errors(&schema, &json!({"kind": "b"}));
    assert!(
        found.iter().any(|error| error["keyword"] == "required"),
        "{found:?}"
    );
    assert!(
        found.iter().any(|error| error["keyword"] == "if"),
        "{found:?}"
    );
}

#[test]
fn if_then_evaluated_keys_feed_unevaluated_properties() {
    // `ErrorIf` 在同一个私有 `ErrorContext` 里评估 `then`；整体成功时把该上下文的已求值
    // 集合 Merge 回主流（失败时丢弃）。TS probe（TypeBox 1.3.27）的输出逐条对照。
    let schema = json!({
        "type": "object",
        "if": {"properties": {"a": {}}, "required": ["a"]},
        "then": {"properties": {"b": {}}, "required": ["b"]},
        "unevaluatedProperties": false
    });
    assert!(check(&schema, &json!({"a": 1, "b": 2})));
    let found = errors(&schema, &json!({"a": 1, "b": 2, "c": 3}));
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0]["params"]["unevaluatedProperties"], json!(["c"]));

    // `then` 失败：then/if 的错误被丢弃，只报一条 `if`；条件已求值的键不回填。
    let found = errors(&schema, &json!({"a": 1}));
    assert_eq!(found.len(), 2, "{found:?}");
    assert_eq!(found[0]["keyword"], "if");
    assert_eq!(found[0]["params"]["failingKeyword"], "then");
    assert_eq!(found[1]["keyword"], "unevaluatedProperties");
    assert_eq!(found[1]["params"]["unevaluatedProperties"], json!(["a"]));
    let found = errors(&schema, &json!({"a": 1, "c": 3}));
    assert_eq!(
        found[1]["params"]["unevaluatedProperties"],
        json!(["a", "c"])
    );

    // 条件失败时 `else` 成功：条件（私有上下文）的已求值集合仍会 Merge。
    let else_schema = json!({
        "type": "object",
        "if": {"properties": {"a": {}}, "required": ["a"]},
        "then": {"properties": {"b": {}}, "required": ["b"]},
        "else": {"properties": {"d": {}}},
        "unevaluatedProperties": false
    });
    assert!(check(&else_schema, &json!({"d": 1})));
    let found = errors(&else_schema, &json!({"e": 1}));
    assert_eq!(found[0]["params"]["unevaluatedProperties"], json!(["e"]));
    let found = errors(&else_schema, &json!({"d": 1, "e": 1}));
    assert_eq!(found[0]["params"]["unevaluatedProperties"], json!(["e"]));

    // 条件失败、但已求值了部分键：`Check` 在失败的关键字处短路（`required` 先于
    // `properties`，键未记录），`Errors` 用 `&` 串联全部关键字（键被记录并在 else 成功时
    // Merge）。TS 的 `Value.Check` / `Value.Errors` 同样在这里分叉，逐条对照。
    let partial = json!({
        "type": "object",
        "if": {"properties": {"a": {}}, "required": ["b"]},
        "else": {"properties": {"c": {}}},
        "unevaluatedProperties": false
    });
    assert!(!check(&partial, &json!({"a": 1, "c": 2})));
    assert!(errors(&partial, &json!({"a": 1, "c": 2})).is_empty());
    let found = errors(&partial, &json!({"a": 1, "c": 2, "d": 3}));
    assert_eq!(found[0]["params"]["unevaluatedProperties"], json!(["d"]));
}

// ── reviewer 回归：#9 JS 原型链上的键名 ──────────────────────────────────────

#[test]
fn prototype_chain_keys_count_as_present() {
    // `key in value`：Object.prototype 上的名字算"存在"。
    assert!(check(
        &json!({"type": "object", "required": ["toString"]}),
        &json!({})
    ));
    assert!(!check(
        &json!({"type": "object", "required": ["constructor"]}),
        &json!({})
    ));

    let properties = json!({"type": "object", "properties": {"toString": {"type": "string"}}});
    assert!(!check(&properties, &json!({})));
    let found = errors(&properties, &json!({}));
    assert_eq!(found[0]["keyword"], "type");
    assert_eq!(found[0]["instancePath"], "/toString");
    assert_eq!(found[0]["message"], "must be string");
    assert!(check(&properties, &json!({"toString": "x"})));

    for (schema, keyword) in [
        (
            json!({"type": "object", "dependencies": {"toString": ["b"]}}),
            "dependencies",
        ),
        (
            json!({"type": "object", "dependentRequired": {"toString": ["b"]}}),
            "dependentRequired",
        ),
    ] {
        let found = errors(&schema, &json!({"a": 1}));
        assert_eq!(found[0]["keyword"], keyword, "{found:?}");
        assert_eq!(found[0]["params"]["property"], "toString");
    }
    let found = errors(
        &json!({"type": "object", "dependentSchemas": {"toString": {"required": ["b"]}}}),
        &json!({"a": 1}),
    );
    assert_eq!(found[0]["schemaPath"], "#/dependentSchemas/toString");
}

// ── reviewer 回归：#16 错误 params 里的索引是 JSON 整数 ────────────────────────

#[test]
fn unique_items_and_one_of_use_numeric_indices() {
    let duplicates = errors(
        &json!({"type": "array", "uniqueItems": true}),
        &json!([1, 1]),
    );
    assert_eq!(
        serde_json::to_string(&duplicates[0]["params"]).expect("json"),
        r#"{"duplicateItems":[1]}"#
    );
    let passing = errors(
        &json!({"oneOf": [{"type": "number"}, {"type": "integer"}]}),
        &json!(1),
    );
    assert_eq!(
        serde_json::to_string(&passing[0]["params"]).expect("json"),
        r#"{"passingSchemas":[0,1]}"#
    );
}
