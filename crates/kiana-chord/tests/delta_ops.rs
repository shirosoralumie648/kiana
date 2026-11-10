//! `packages/chord/test/delta.test.ts`（不含 tracker 生命周期的部分）与
//! `delta-op-text-retention.test.ts` 的 Rust 端口：应用器、路径校验、codec、UTF-16 语义。
//!
//! TS 的 `apply` 接收任意 `unknown` 元组并在运行期抛 `TypeError`；Rust 的 [`apply`]
//! 接收强类型 [`Op`]，因此"畸形元组"的断言改由 [`parse_op`]（JSON → `Op` 的校验入口）
//! 覆盖；错误消息是稳定契约，改动需同步测试。

use kiana_chord::delta::index::{
    Decoder, Encoder, OpError, RESERVED_SEGMENTS, apply, apply_immutable, apply_immutable_batches,
    assert_safe_path, assert_valid_op, assert_valid_wire_op, decode, encode, is_base, is_replace,
    overlap, owned_slice, parse_op, path_to_json, utf16_len, utf16_slice,
};
use kiana_chord::delta::{Op, Seg};
use serde_json::{Value, json};

fn op(value: Value) -> Op {
    parse_op(&value).unwrap_or_else(|error| panic!("parse {value}: {}", error.message()))
}

fn ops(values: &[Value]) -> Vec<Op> {
    values.iter().cloned().map(op).collect()
}

fn path(segments: &[&str]) -> Vec<Seg> {
    segments.iter().map(|s| Seg::Key((*s).to_owned())).collect()
}

// ── 应用器 ────────────────────────────────────────────────────────────────────

#[test]
fn applies_mutable_operations() {
    let operations = ops(&[
        json!(["a", ["text"], "b"]),
        json!(["p", ["values"], 1, 1, [3, 4]]),
        json!(["s", ["nested", "value"], 2]),
    ]);
    let base =
        json!({"text": "a", "values": [1, 2], "nested": {"value": 1}, "stable": {"value": 9}});
    let snapshot = base.clone();
    let result = apply(base, &operations).expect("apply");
    assert_eq!(
        result,
        json!({"text": "ab", "values": [1, 3, 4], "nested": {"value": 2}, "stable": {"value": 9}})
    );
    assert_eq!(
        snapshot,
        json!({"text": "a", "values": [1, 2], "nested": {"value": 1}, "stable": {"value": 9}})
    );
}

#[test]
fn supports_root_replacement_root_splice_and_permutations() {
    let base = ops(&[json!(["r", [1, 2, 3]])]);
    assert!(is_base(&base));
    assert!(is_replace(&base[0]));
    let mut value = apply(Value::Null, &base).expect("replace");
    value = apply(value, &ops(&[json!(["p", [], 1, 1, [4]])])).expect("splice");
    value = apply(value, &ops(&[json!(["m", [], [2, 0, 1]])])).expect("permute");
    assert_eq!(value, json!([3, 1, 4]));
}

#[test]
fn rejects_unsafe_and_malformed_paths() {
    // TS 的 `assertValidOp` 在 `apply` 之前就会拒绝保留段；Rust 的 `parse_op` 同层。
    let error =
        parse_op(&json!(["s", ["constructor", "prototype", "x"], true])).expect_err("reserved");
    assert_eq!(error.name(), "UnsafePathError");
    assert_eq!(error.message(), "unsafe path segment: constructor");
    // 直接构造 `Op`（绕过 parse）时，`apply` 仍要拒绝。
    let direct = Op::Set(path(&["constructor", "prototype", "x"]), Value::Bool(true));
    let error = apply(json!({}), &[direct]).expect_err("reserved direct");
    assert_eq!(error.name(), "UnsafePathError");

    let error = apply(
        json!({"values": [1]}),
        &ops(&[json!(["s", ["values", 3], 2])]),
    )
    .expect_err("gap");
    assert_eq!(error.name(), "UnsafePathError");
    assert_eq!(error.message(), "unsafe path segment: 3");

    assert!(apply(json!({"value": 1}), &ops(&[json!(["a", ["value"], "x"])])).is_err());
    assert!(parse_op(&json!(["s", "value", 1])).is_err());
    assert_eq!(
        parse_op(&json!(["m", [], [0, 0]]))
            .expect_err("bijection")
            .message(),
        "m permutation is not a bijection"
    );
}

#[test]
fn validates_immutable_operations_before_traversing_the_target() {
    // Rust 的 `Op` 已经过类型检查；这里验证 `apply_immutable` 先校验结构再触碰目标。
    let invalid = Op::Permute(path(&["value"]), vec![0, 0]);
    assert_eq!(
        apply_immutable(json!({"value": 1}), &[invalid])
            .expect_err("invalid")
            .message(),
        "m permutation is not a bijection"
    );
}

#[test]
fn validates_decoded_and_wire_vocabularies_separately() {
    assert!(parse_op(&json!(["s", ["value"], 1])).is_ok());
    assert!(parse_op(&json!(["s", 1])).is_err());
    assert!(assert_valid_wire_op(&json!(["s", 1])).is_ok());
    assert!(assert_valid_wire_op(&json!(["#", 0, ["value"]])).is_ok());
    for wire_only in [
        json!(["s", 1]),
        json!(["d"]),
        json!(["a", "x"]),
        json!(["t", 2]),
        json!(["p", 0, 0, []]),
        json!(["#", 0, ["value"]]),
        json!(["s", 0, 1]),
    ] {
        assert!(parse_op(&wire_only).is_err(), "decoded rejects {wire_only}");
        assert!(
            assert_valid_wire_op(&wire_only).is_ok(),
            "wire accepts {wire_only}"
        );
    }
    for (value, message) in [
        (json!(["#", 0]), "# shape"),
        (json!(["#", -1, ["a"]]), "# shape"),
        (json!(["s", "path", 1]), "path is not an array"),
        (
            json!(["s", ["__proto__"], 1]),
            "unsafe path segment: __proto__",
        ),
    ] {
        let error = assert_valid_wire_op(&value).expect_err(&format!("wire {value}"));
        assert_eq!(error.message(), message, "for {value}");
    }
}

#[test]
fn rejects_unknown_verbs_and_malformed_tuples() {
    for (value, message) in [
        (json!(["ZZZ", ["value"], 9]), "unknown op verb: ZZZ"),
        (json!(["p", ["values"], 0, 0, "not-an-array"]), "p items"),
        (json!(["s", "value", 9]), "path is not an array"),
        (json!({"op": "s"}), "op is not a tuple"),
        (Value::Null, "op is not a tuple"),
        (json!(["t", ["value"], -1]), "t shape"),
        (json!(["a", ["value"], 1]), "a shape"),
        (json!(["#", 0]), "unknown op verb: #"),
    ] {
        let error = parse_op(&value).expect_err(&format!("parse {value}"));
        assert_eq!(error.message(), message, "for {value}");
    }
}

#[test]
fn applies_array_index_rules() {
    assert_eq!(
        apply(
            json!({"values": [1, 2, 3]}),
            &ops(&[json!(["s", ["values", 1], 9])])
        )
        .expect("set"),
        json!({"values": [1, 9, 3]})
    );
    assert_eq!(
        apply(
            json!({"values": [1, 2, 3]}),
            &ops(&[json!(["s", ["values", 3], 9])])
        )
        .expect("append"),
        json!({"values": [1, 2, 3, 9]})
    );
    for value in [
        json!({"values": [1, 2, 3]}),
        json!({"values": []}),
        json!({"values": [1]}),
        json!({"values": ["a"]}),
    ] {
        let bad = match value.as_object().and_then(|m| m.get("values")) {
            Some(Value::Array(items)) if items.is_empty() => {
                json!(["s", ["values", 4_294_967_290i64], 1])
            }
            Some(Value::Array(items)) if items.len() == 3 => json!(["s", ["values", 5], 9]),
            Some(Value::Array(items)) if items.len() == 1 && items[0].is_string() => {
                json!(["a", ["values", "0"], "b"])
            }
            _ => json!(["s", ["values", "0"], 9]),
        };
        assert!(
            apply(value.clone(), &ops(&[bad])).is_err(),
            "accepts {value}"
        );
    }
}

#[test]
fn allows_explicit_growth_and_rejects_deletion_past_the_end() {
    assert_eq!(
        apply(
            json!({"values": [1]}),
            &ops(&[json!(["p", ["values"], 1, 0, [null, null, 9]])])
        )
        .expect("grow"),
        json!({"values": [1, null, null, 9]})
    );
    assert!(apply(json!({"values": [1]}), &ops(&[json!(["d", ["values", 1]])])).is_err());
}

#[test]
fn applies_large_splice_payloads() {
    let items = vec![Value::Null; 300_000];
    let operations = vec![Op::Splice {
        path: path(&["values"]),
        index: 0,
        remove: 0,
        items,
    }];
    let result = apply(json!({"values": []}), &operations).expect("splice");
    assert_eq!(result["values"].as_array().unwrap().len(), 300_000);
}

#[test]
fn clamps_splice_removal_past_the_end() {
    let huge = 1_000_000_000u64;
    assert_eq!(
        apply(
            json!({"values": [1, 2]}),
            &[Op::Splice {
                path: path(&["values"]),
                index: 0,
                remove: usize::try_from(huge).unwrap(),
                items: Vec::new(),
            }]
        )
        .expect("splice"),
        json!({"values": []})
    );
}

#[test]
fn immutable_application_keeps_the_base_and_payload_untouched() {
    let base = json!({"text": "a", "values": [1, 2], "nested": {"value": 1}});
    let snapshot = base.clone();
    let result = apply_immutable(
        base,
        &ops(&[
            json!(["a", ["text"], "b"]),
            json!(["p", ["values"], 1, 1, [3, 4]]),
            json!(["s", ["nested", "value"], 2]),
        ]),
    )
    .expect("immutable");
    assert_eq!(
        result,
        json!({"text": "ab", "values": [1, 3, 4], "nested": {"value": 2}})
    );
    assert_eq!(snapshot["nested"]["value"], json!(1));

    // `r` 之后的操作必须复制 payload，而不是就地修改它。
    let replacement = json!({"nested": {"value": 1}});
    let replacement_snapshot = replacement.clone();
    let next = apply_immutable(
        Value::Null,
        &ops(&[
            json!(["r", replacement.clone()]),
            json!(["s", ["nested", "value"], 2]),
        ]),
    )
    .expect("replacement");
    assert_eq!(replacement, replacement_snapshot);
    assert_eq!(next["nested"]["value"], json!(2));
}

#[test]
fn immutable_batches_match_sequential_and_flattened_replay() {
    let base = json!({"text": "abcdef", "meta": {"count": 0}, "values": [{"id": 1}, {"id": 2}, {"id": 3}]});
    let batches = vec![
        ops(&[
            json!(["s", ["meta", "count"], 1]),
            json!(["p", ["values"], 1, 1, [{"id": 4}]]),
        ]),
        vec![],
        ops(&[
            json!(["m", ["values"], [2, 0, 1]]),
            json!(["s", ["values", 2, "id"], 40]),
        ]),
        ops(&[json!(["t", ["text"], 2]), json!(["a", ["text"], "!"])]),
    ];
    let streamed = apply_immutable_batches(base.clone(), batches.clone()).expect("streamed");
    let mut sequential = base.clone();
    for batch in &batches {
        sequential = apply_immutable(sequential, batch).expect("sequential");
    }
    let flattened: Vec<Op> = batches.iter().flatten().cloned().collect();
    let flat = apply_immutable(base.clone(), &flattened).expect("flat");
    assert_eq!(streamed, sequential);
    assert_eq!(streamed, flat);
    assert_eq!(streamed["text"], json!("cdef!"));
    assert_eq!(base["meta"]["count"], json!(0));
}

// ── 路径安全 ─────────────────────────────────────────────────────────────────

#[test]
fn reserved_segments_are_exported() {
    assert_eq!(RESERVED_SEGMENTS, ["__proto__", "constructor", "prototype"]);
    assert!(assert_safe_path(&path(&["__proto__"])).is_err());
    assert!(assert_safe_path(&path(&["value"])).is_ok());
    assert!(assert_safe_path(&[Seg::Index(-1)]).is_err());
}

#[test]
fn reserved_names_inside_values_are_plain_keys() {
    let value: Value = serde_json::from_str(r#"{"__proto__":{"z":1}}"#).expect("json");
    let out = apply(json!({}), &[Op::Set(path(&["value"]), value.clone())]).expect("set");
    assert_eq!(out["value"], value);
    assert_eq!(out["value"]["__proto__"]["z"], json!(1));
}

// ── Codec ────────────────────────────────────────────────────────────────────

#[test]
fn interns_paths_omits_adjacent_paths_and_round_trips() {
    let mut encoder = Encoder::new();
    let mut decoder = Decoder::new();
    let nested = path(&["nested", "text"]);
    let first = vec![
        Op::Truncate(nested.clone(), 1),
        Op::Append(nested.clone(), "x".into()),
    ];
    let wire = encoder.encode(&first);
    assert_eq!(decoder.decode(&wire).expect("decode"), first);

    let second = vec![Op::Append(nested.clone(), "y".into())];
    let wire = encoder.encode(&second);
    assert_eq!(
        wire,
        vec![json!(["#", 0, ["nested", "text"]]), json!(["a", 0, "y"])]
    );
    assert_eq!(decoder.decode(&wire).expect("decode second"), second);
}

#[test]
fn resets_path_dictionaries_on_a_base() {
    let mut encoder = Encoder::new();
    let value = path(&["value"]);
    encoder.encode(&[Op::Set(value.clone(), json!(1))]);
    encoder.encode(&[Op::Set(value.clone(), json!(2))]);
    assert_eq!(
        encoder.encode(&[Op::Replace(json!({"value": 3}))]),
        vec![json!(["r", {"value": 3}])]
    );
    assert_eq!(
        encoder.encode(&[Op::Set(value.clone(), json!(4))]),
        vec![json!(["s", ["value"], 4])]
    );
}

#[test]
fn rejects_unresolved_short_forms_and_unsafe_interned_paths() {
    assert!(Decoder::new().decode(&[json!(["a", "x"])]).is_err());
    let error = Decoder::new()
        .decode(&[json!(["#", 0, ["__proto__"]]), json!(["s", 0, true])])
        .expect_err("unsafe");
    assert_eq!(error.name(), "UnsafePathError");
    assert_eq!(error.message(), "unsafe path segment: __proto__");
}

#[test]
fn omits_an_adjacent_repeated_path() {
    let value = path(&["value"]);
    assert_eq!(
        encode(&[Op::Set(value.clone(), json!(1)), Op::Set(value, json!(2))]),
        vec![json!(["s", ["value"], 1]), json!(["s", 2])]
    );
}

#[test]
fn interns_on_second_use_rather_than_first() {
    let mut encoder = Encoder::new();
    let deep = path(&["a", "deep"]);
    assert_eq!(
        encoder.encode(&[Op::Append(deep.clone(), "1".into())]),
        vec![json!(["a", ["a", "deep"], "1"])]
    );
    assert_eq!(
        encoder.encode(&[Op::Append(deep.clone(), "2".into())]),
        vec![json!(["#", 0, ["a", "deep"]]), json!(["a", 0, "2"])]
    );
}

#[test]
fn does_not_collide_paths_containing_null_characters() {
    let operations = vec![
        Op::Set(path(&["a\u{0}b"]), json!(1)),
        Op::Set(path(&["a", "b"]), json!(2)),
    ];
    assert_eq!(
        decode(&encode(&operations)).expect("round trip"),
        operations
    );
}

#[test]
fn clears_decoder_ids_on_a_base_batch() {
    let mut decoder = Decoder::new();
    decoder
        .decode(&[json!(["#", 0, ["a"]]), json!(["a", 0, "1"])])
        .expect("first");
    decoder.decode(&[json!(["r", {"a": ""}])]).expect("base");
    let error = decoder
        .decode(&[json!(["a", 0, "2"])])
        .expect_err("cleared");
    assert_eq!(error.message(), "unresolvable path: 0");
}

#[test]
fn makes_batches_after_a_base_self_contained() {
    let mut encoder = Encoder::new();
    let deep = path(&["a", "deep"]);
    encoder.encode(&[Op::Append(deep.clone(), "1".into())]);
    encoder.encode(&[Op::Append(deep.clone(), "2".into())]);
    let base = encoder.encode(&[Op::Replace(json!({"a": {"deep": "x"}}))]);
    let after = encoder.encode(&[Op::Append(deep.clone(), "3".into())]);
    assert_eq!(after, vec![json!(["a", ["a", "deep"], "3"])]);
    let mut decoder = Decoder::new();
    assert_eq!(
        decoder.decode(&base).expect("base"),
        vec![Op::Replace(json!({"a": {"deep": "x"}}))]
    );
    assert_eq!(
        decoder.decode(&after).expect("after"),
        vec![Op::Append(deep, "3".into())]
    );
}

#[test]
fn round_trips_deterministic_mixed_operation_streams() {
    let mut encoder = Encoder::new();
    let mut decoder = Decoder::new();
    let mut batches = Vec::new();
    for index in 0..100usize {
        let segments: Vec<Seg> = vec![
            Seg::Key("rows".into()),
            Seg::Index(index as i64),
            Seg::Key("value".into()),
        ];
        let batch = vec![
            Op::Set(segments, json!(index)),
            Op::Append(path(&["output"]), index.to_string()),
            Op::Splice {
                path: path(&["tail"]),
                index,
                remove: 0,
                items: vec![json!(index)],
            },
        ];
        batches.push(batch);
    }
    for batch in &batches {
        let wire = encoder.encode(batch);
        assert_eq!(&decoder.decode(&wire).expect("round trip"), batch);
    }
}

#[test]
fn decoder_path_json_shape() {
    let value = path(&["a", "b"]);
    assert_eq!(path_to_json(&value), json!(["a", "b"]));
    assert_eq!(
        path_to_json(&[Seg::Key("rows".into()), Seg::Index(3)]),
        json!(["rows", 3])
    );
}

// ── UTF-16 字符串语义 ────────────────────────────────────────────────────────

#[test]
fn overlap_matches_the_reference() {
    assert_eq!(overlap("abcdefgh", "defghxyz", 65_536), 5);
    assert_eq!(overlap("abcdef", "defghi", 0), 0);
    assert_eq!(overlap("", "abc", 10), 0);
    assert_eq!(overlap("abc", "", 10), 0);
    assert_eq!(overlap("aaaa", "aaab", 10), 3);
}

#[test]
fn owned_slice_and_utf16_slicing() {
    assert_eq!(owned_slice("abcdef", 2, None), "cdef");
    assert_eq!(owned_slice("abcdef", 2, Some(4)), "cd");
    assert_eq!(utf16_slice("a😀b", 1, Some(3)), "😀");
    assert_eq!(utf16_len("a😀b"), 4);
    assert_eq!(utf16_len("héllo"), 5);
}

#[test]
fn truncation_counts_utf16_units() {
    // TS 的 `t` 执行 `value.slice(count)`：count 是**丢弃**的前缀 UTF-16 码元数。
    let result =
        apply(json!({"text": "a😀b"}), &[Op::Truncate(path(&["text"]), 1)]).expect("truncate");
    assert_eq!(result["text"], json!("😀b"));
    let result =
        apply(json!({"text": "a😀b"}), &[Op::Truncate(path(&["text"]), 3)]).expect("truncate");
    assert_eq!(result["text"], json!("b"));
}

#[test]
fn error_names_and_messages_match_typescript() {
    let path_error = OpError::Path(kiana_chord::delta::index::PathError::new(&path(&[
        "a", "b",
    ])));
    assert_eq!(path_error.name(), "PathError");
    assert_eq!(path_error.message(), "unresolvable path: [\"a\",\"b\"]");
    let id_error = OpError::Path(kiana_chord::delta::index::PathError::from_id(3));
    assert_eq!(id_error.message(), "unresolvable path: 3");
    let type_error = OpError::Invalid("r arity");
    assert_eq!(type_error.name(), "TypeError");
    assert_eq!(type_error.to_string(), "r arity");
    let unknown = OpError::UnknownVerb("ZZZ".into());
    assert_eq!(unknown.message(), "unknown op verb: ZZZ");
}

#[test]
fn assert_valid_op_accepts_every_decoded_form() {
    for operation in [
        json!(["r", {"value": 1}]),
        json!(["s", ["value"], 1]),
        json!(["d", ["value"]]),
        json!(["a", ["value"], "x"]),
        json!(["t", ["value"], 2]),
        json!(["p", ["value"], 0, 0, []]),
        json!(["m", ["value"], [0]]),
    ] {
        let parsed = op(operation);
        assert!(assert_valid_op(&parsed).is_ok());
    }
}

// ── 回归：对象删键保序、数组下标预检、超大数字 id/下标 ─────────────────────

#[test]
fn deleting_an_object_property_keeps_the_remaining_key_order() {
    // TS 的 `delete parent[key]` 保持其余键的插入序；preserve_order 的 `Map::remove`
    // 是 swap-remove，会把末尾键搬到被删位置。
    let result = apply(
        json!({"a": 1, "b": 2, "c": 3}),
        &ops(&[json!(["d", ["a"]])]),
    )
    .expect("delete");
    let keys: Vec<&str> = result
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(keys, ["b", "c"]);

    let immutable = apply_immutable(
        json!({"a": 1, "b": 2, "c": 3, "d": 4}),
        &ops(&[json!(["d", ["b"]])]),
    )
    .expect("delete immutable");
    let keys: Vec<&str> = immutable
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(keys, ["a", "c", "d"]);
}

#[test]
fn array_index_precheck_runs_before_the_verb() {
    // TS 在 switch 之前对数组父亲执行 assertIndexInRange：index > length 一律
    // UnsafePathError，而不是各 verb 自己的 PathError。
    for operation in [
        json!(["d", ["arr", 5]]),
        json!(["a", ["arr", 5], "x"]),
        json!(["t", ["arr", 5], 1]),
        json!(["s", ["arr", 5], 3]),
    ] {
        let label = operation.to_string();
        let error = apply(json!({"arr": [1, 2]}), &ops(&[operation])).expect_err("past the end");
        assert_eq!(error.name(), "UnsafePathError", "{label}");
        assert_eq!(error.message(), "unsafe path segment: 5", "{label}");
    }
    // index === length 时预检通过，随后由各 verb 失败（s 可以追加一格）。
    for operation in [
        json!(["d", ["arr", 2]]),
        json!(["a", ["arr", 2], "x"]),
        json!(["t", ["arr", 2], 1]),
    ] {
        let label = operation.to_string();
        let error = apply(json!({"arr": [1, 2]}), &ops(&[operation])).expect_err("at the end");
        assert_eq!(error.name(), "PathError", "{label}");
        assert_eq!(error.message(), "unresolvable path: [\"arr\",2]", "{label}");
    }
    assert_eq!(
        apply(json!({"arr": [1, 2]}), &ops(&[json!(["s", ["arr", 2], 3])])).expect("append"),
        json!({"arr": [1, 2, 3]})
    );
}

#[test]
fn decoder_keys_huge_ids_as_javascript_numbers() {
    // TS 的 decoder 用数字本身作 Map 键：2e19 与 3e19 是不同的 id。
    let huge = serde_json::Number::from_f64(2e19).expect("finite");
    let other = serde_json::Number::from_f64(3e19).expect("finite");
    let decoded = decode(&[
        json!(["#", huge.clone(), ["a"]]),
        json!(["#", other.clone(), ["b"]]),
        json!(["d", huge]),
    ])
    .expect("decode");
    assert_eq!(decoded, vec![Op::Delete(path(&["a"]))]);

    let missing = serde_json::Number::from_f64(4e19).expect("finite");
    let error = decode(&[json!(["d", missing])]).expect_err("unresolvable id");
    assert_eq!(error.name(), "PathError");
    assert_eq!(error.message(), "unresolvable path: 40000000000000000000");
}

#[test]
fn applies_splices_with_indices_beyond_i64() {
    let result = apply(
        json!({"a": [1]}),
        &ops(&[json!(["p", ["a"], 1e20, 0, ["x"]])]),
    )
    .expect("splice clamps");
    assert_eq!(result, json!({"a": [1, "x"]}));
    let result = apply(
        json!({"a": [1, 2]}),
        &ops(&[json!(["p", ["a"], 1e20, 1e20, ["x"]])]),
    )
    .expect("splice clamps removal");
    assert_eq!(result, json!({"a": [1, 2, "x"]}));
}

#[test]
fn huge_numeric_path_segments_are_object_keys() {
    // TS: `String(1e20)` / `String(1e21)` 是普通属性键。
    let result = apply(json!({"a": 1}), &ops(&[json!(["s", [1e20], 2])])).expect("write");
    assert_eq!(result["100000000000000000000"], json!(2));
    let result = apply(json!({"a": 1}), &ops(&[json!(["s", [1e21], 2])])).expect("write");
    assert_eq!(result["1e+21"], json!(2));
    // 数组父容器把它当作非数字段拒绝，消息仍是 `String(segment)`。
    let error =
        apply(json!({"a": [1]}), &ops(&[json!(["s", ["a", 1e20], 2])])).expect_err("array segment");
    assert_eq!(error.name(), "UnsafePathError");
    assert_eq!(
        error.message(),
        "unsafe path segment: 100000000000000000000"
    );
}

#[test]
fn numeric_path_segments_in_the_i64_range_use_javascript_number_keys() {
    // JS 的 ToPropertyKey 是 `String(number)`：`String(2 ** 56)` 给出 f64 的最短往返
    // 表示 `"72057594037927940"`，而不是 i64 的精确十进制 `"72057594037927936"`。
    let result = apply(
        json!({"72057594037927940": {"x": 1}}),
        &ops(&[json!(["s", [72057594037927936u64, "x"], 2])]),
    )
    .expect("write through the js key");
    assert_eq!(result, json!({"72057594037927940": {"x": 2}}));

    // 在空对象上写入会创建 JS 的键，而不是精确整数键。
    let result = apply(
        json!({}),
        &ops(&[json!(["s", [1152921504606846976u64], 1])]),
    )
    .expect("create key");
    assert_eq!(result, json!({"1152921504606847000": 1}));

    // 读取/追加也走同一个键。
    let result = apply(
        json!({"1152921504606847000": "a"}),
        &ops(&[json!(["a", [1152921504606846976u64], "b"])]),
    )
    .expect("append");
    assert_eq!(result, json!({"1152921504606847000": "ab"}));

    // 查找失败时 PathError 里的路径同样按 JS 数字格式化。
    let error = apply(
        json!({}),
        &ops(&[json!(["s", [72057594037927936u64, "y"], 1])]),
    )
    .expect_err("unresolvable");
    assert_eq!(error.name(), "PathError");
    assert_eq!(error.message(), "unresolvable path: [72057594037927940]");
}

#[test]
fn truncating_a_surrogate_pair_uses_the_replacement_character() {
    // TS 的 `String.slice` 会留下孤立低代理项（JSON.stringify 为 "\udE00b"）；Rust 的
    // `String` 装不下孤立代理项（ARCHITECTURE §8 的被迫偏差——`JsonValue` 冻结为
    // `serde_json::Value`，`kiana_json` 解析 "\udE00" 时同样替换为 U+FFFD）。这里固定
    // 记录该偏差的实际输出。
    let result =
        apply(json!({"text": "a😀b"}), &[Op::Truncate(path(&["text"]), 2)]).expect("truncate");
    assert_eq!(result["text"], json!("\u{fffd}b"));
}
