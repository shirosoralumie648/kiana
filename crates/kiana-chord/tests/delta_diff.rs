//! `delta/diff.ts` 的金标准测试：用例由 TS 参考实现生成，`ops` 逐字节比对，并验证
//! `apply_immutable(before, ops) == after`。
//!
//! 生成方式：对每个用例的 `before`/`after` 各做一次独立的 JSON 解析（`JSON.parse`），
//! 再调用 TS 的 `diffRevisions`。这与 Rust 侧可表达的输入完全对应——`serde_json::Value`
//! 是两棵独立的树，没有共享子树，TS 对"独立解析"的输入按引用身份找不到容器锚点。
//! 早先有两例是把 TS 测试里的共享引用输入序列化后生成的，已按同一规则重新生成。

use kiana_chord::delta::Op;
use kiana_chord::delta::diff::diff_revisions;
use kiana_chord::delta::index::{apply_immutable, parse_op};
use serde_json::Value;

fn golden() -> Value {
    serde_json::from_str(include_str!("fixtures/delta_diff_golden.json")).expect("diff golden")
}

fn ops(value: &Value) -> Vec<Op> {
    value
        .as_array()
        .expect("ops array")
        .iter()
        .map(|operation| {
            parse_op(operation)
                .unwrap_or_else(|error| panic!("parse {operation}: {}", error.message()))
        })
        .collect()
}

fn ops_json(ops: &[Op]) -> Value {
    Value::Array(
        ops.iter()
            .map(kiana_chord::delta::index::op_to_json)
            .collect(),
    )
}

#[test]
fn diff_revisions_matches_the_typescript_reference() {
    let golden = golden();
    let cases = golden["cases"].as_array().expect("cases");
    let mut checked = 0usize;
    let mut failures = Vec::new();
    for case in cases {
        let name = case["name"].as_str().unwrap_or("?");
        let before = &case["before"];
        let after = &case["after"];
        let expected = ops(&case["ops"]);
        let actual = diff_revisions(before, after);
        if actual != expected {
            failures.push(format!(
                "{name}: expected {}, got {}",
                serde_json::to_string(&ops_json(&expected)).unwrap_or_default(),
                serde_json::to_string(&ops_json(&actual)).unwrap_or_default()
            ));
            continue;
        }
        let applied = apply_immutable(before.clone(), &actual).expect("apply diff ops");
        assert_eq!(&applied, after, "{name}: apply(ops) != after");
        checked += 1;
    }
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
    assert!(checked >= 400, "expected a broad corpus, checked {checked}");
}

#[test]
fn diff_revisions_does_not_mutate_its_inputs() {
    let before = serde_json::json!({"text": "abcdef", "values": [1, 2, 3]});
    let after = serde_json::json!({"text": "cdefgh", "values": [1, 3, 4]});
    let before_snapshot = before.clone();
    let after_snapshot = after.clone();
    let _ = diff_revisions(&before, &after);
    assert_eq!(before, before_snapshot);
    assert_eq!(after, after_snapshot);
}
