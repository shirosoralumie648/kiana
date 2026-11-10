//! `packages/chord/test/delta-pending.test.ts` 的 Rust 端口（`PendingOps` 部分）。
//!
//! TS 的 fuzz 用例依赖 tracker；这里覆盖**折叠语义**、cap、ownership 与 size 的可观察行为。
//! 折叠必须对副本不可见：`apply_immutable(initial, ops)` 与折叠后的回放必须相等。

use kiana_chord::delta::index::{apply_immutable, parse_op};
use kiana_chord::delta::pending::{MAX_PENDING_OPS, PendingOps, PendingTake};
use kiana_chord::delta::{Op, Seg};
use serde_json::{Value, json};

fn op(value: Value) -> Op {
    parse_op(&value).unwrap_or_else(|error| panic!("parse {value}: {}", error.message()))
}

fn ops(values: &[Value]) -> Vec<Op> {
    values.iter().cloned().map(op).collect()
}

fn push_each(queue: &mut PendingOps, values: &[Value]) {
    for value in values {
        queue.push(&[op(value.clone())]);
    }
}

fn take_ops(queue: &mut PendingOps) -> Vec<Op> {
    match queue.take() {
        PendingTake::Ops(ops) => ops,
        PendingTake::Replace => panic!("unexpected overflow"),
    }
}

/// 折叠对回放不可见。
fn assert_replay_equivalent(initial: Value, values: &[Value]) {
    let original = ops(values);
    let mut queue = PendingOps::new();
    push_each(&mut queue, values);
    let folded = take_ops(&mut queue);
    let expected = apply_immutable(initial.clone(), &original).expect("apply original");
    let actual = apply_immutable(initial, &folded).expect("apply folded");
    assert_eq!(actual, expected);
}

#[test]
fn a_leaf_set_in_an_array_element_between_splices_stays_separate() {
    let values = [
        json!(["s", ["arr", 2, "x"], 1]),
        json!(["p", ["arr"], 0, 0, [{"x": 9}]]),
        json!(["s", ["arr", 2, "x"], 2]),
    ];
    let mut queue = PendingOps::new();
    push_each(&mut queue, &values);
    assert_eq!(take_ops(&mut queue), ops(&values));

    let appends = [
        json!(["a", ["arr", 0, "x"], "1"]),
        json!(["p", ["arr"], 0, 0, [{"x": "new"}]]),
        json!(["a", ["arr", 0, "x"], "2"]),
    ];
    let mut queue = PendingOps::new();
    push_each(&mut queue, &appends);
    assert_eq!(take_ops(&mut queue), ops(&appends));
}

#[test]
fn t_then_a_and_appends_around_a_trim_stay_separate() {
    for values in [
        vec![json!(["t", ["s"], 2]), json!(["a", ["s"], "!"])],
        vec![
            json!(["a", ["s"], "AB"]),
            json!(["t", ["s"], 3]),
            json!(["a", ["s"], "C"]),
        ],
    ] {
        let mut queue = PendingOps::new();
        push_each(&mut queue, &values);
        assert_eq!(take_ops(&mut queue).len(), values.len());
    }
}

#[test]
fn an_object_set_at_a_parent_clears_leaf_sets_below_it() {
    let values = [
        json!(["s", ["o", "a"], 2]),
        json!(["s", ["o"], {"a": 5, "b": 6}]),
        json!(["s", ["o", "a"], 7]),
    ];
    let mut queue = PendingOps::new();
    push_each(&mut queue, &values);
    assert_eq!(take_ops(&mut queue).len(), 3);

    let values = [
        json!(["s", ["x"], {"s": "b"}]),
        json!(["a", ["x", "s"], "c"]),
        json!(["s", ["x", "s"], "d"]),
    ];
    let mut queue = PendingOps::new();
    push_each(&mut queue, &values);
    assert_eq!(
        take_ops(&mut queue),
        ops(&[
            json!(["s", ["x"], {"s": "b"}]),
            json!(["s", ["x", "s"], "d"])
        ])
    );
}

#[test]
fn an_ancestor_primitive_set_between_leaf_ops_below_it() {
    let values = [
        json!(["a", ["o", "a"], "1"]),
        json!(["s", ["o", "b"], "q"]),
        json!(["s", ["o"], "flat"]),
        json!(["a", ["o"], "!"]),
    ];
    let mut queue = PendingOps::new();
    push_each(&mut queue, &values);
    assert_eq!(
        take_ops(&mut queue),
        ops(&[
            json!(["a", ["o", "a"], "1"]),
            json!(["s", ["o", "b"], "q"]),
            json!(["s", ["o"], "flat!"]),
        ])
    );

    let values = [
        json!(["s", ["a", "b"], 1]),
        json!(["s", ["a"], null]),
        json!(["s", ["a"], "x"]),
    ];
    let mut queue = PendingOps::new();
    push_each(&mut queue, &values);
    assert_eq!(
        take_ops(&mut queue),
        ops(&[json!(["s", ["a", "b"], 1]), json!(["s", ["a"], "x"])])
    );
}

#[test]
fn deletes_moves_and_splices_clear_the_leaf_index() {
    let cases = [
        (
            json!({"arr": ["a", "b", "c"]}),
            vec![
                json!(["s", ["arr", 2], "C"]),
                json!(["d", ["arr", 0]]),
                json!(["s", ["arr", 1], "Z"]),
            ],
            3,
        ),
        (
            json!({"arr": [1, 2, 3]}),
            vec![
                json!(["s", ["arr", 0], 10]),
                json!(["m", ["arr"], [2, 1, 0]]),
                json!(["s", ["arr", 0], 20]),
            ],
            3,
        ),
        (
            json!({"arr": [{"x": 0}]}),
            vec![
                json!(["s", ["arr", 0, "x"], 1]),
                json!(["p", ["arr"], 1, 0, [{"x": 0}]]),
                json!(["s", ["arr", 0, "x"], 2]),
                json!(["s", ["arr", 1, "x"], 3]),
            ],
            4,
        ),
    ];
    for (initial, values, expected_len) in cases {
        let mut queue = PendingOps::new();
        push_each(&mut queue, &values);
        let folded = take_ops(&mut queue);
        assert_eq!(folded.len(), expected_len);
        assert_eq!(
            apply_immutable(initial.clone(), &folded).expect("folded"),
            apply_immutable(initial, &ops(&values)).expect("original")
        );
    }
}

#[test]
fn keys_of_one_parent_never_collide() {
    assert_replay_equivalent(
        json!({"a": {"b": "x"}, "a.b": "z"}),
        &[
            json!(["a", ["a", "b"], "1"]),
            json!(["s", ["a.b"], "y"]),
            json!(["a", ["a", "b"], "2"]),
        ],
    );
    assert_replay_equivalent(
        json!({"o": {"1": "a"}, "arr": ["x", "y"]}),
        &[
            json!(["s", ["o", "1"], "b"]),
            json!(["s", ["arr", 1], "z"]),
            json!(["s", ["o", "1"], "c"]),
            json!(["s", ["arr", 1], "w"]),
        ],
    );
}

#[test]
fn an_array_set_at_the_end_folds_into_a_later_set_of_that_index() {
    let mut queue = PendingOps::new();
    push_each(
        &mut queue,
        &[
            json!(["s", ["arr", 1], 2]),
            json!(["s", ["arr", 0], 9]),
            json!(["s", ["arr", 1], 3]),
        ],
    );
    assert_eq!(
        take_ops(&mut queue),
        ops(&[json!(["s", ["arr", 1], 3]), json!(["s", ["arr", 0], 9])])
    );
}

#[test]
fn r_drops_everything_before_it() {
    let mut queue = PendingOps::new();
    push_each(
        &mut queue,
        &[
            json!(["s", ["a"], 2]),
            json!(["r", {"b": "x"}]),
            json!(["a", ["b"], "y"]),
        ],
    );
    assert_eq!(
        take_ops(&mut queue),
        ops(&[json!(["r", {"b": "x"}]), json!(["a", ["b"], "y"])])
    );
}

#[test]
fn joins_appends_and_folds_appends_into_a_string_set() {
    let mut queue = PendingOps::new();
    push_each(
        &mut queue,
        &[
            json!(["s", ["s"], "ab"]),
            json!(["a", ["s"], "c"]),
            json!(["s", ["t"], 1]),
            json!(["a", ["s"], "d"]),
        ],
    );
    assert_eq!(
        take_ops(&mut queue),
        ops(&[json!(["s", ["s"], "abcd"]), json!(["s", ["t"], 1])])
    );

    // TS 的孤立代理项用例在 Rust 字符串里不可表示；用等价的合法字符串验证折叠。
    let mut queue = PendingOps::new();
    push_each(
        &mut queue,
        &[
            json!(["a", ["s"], "😀"]),
            json!(["s", ["n"], 1]),
            json!(["a", ["s"], "é"]),
            json!(["a", ["s"], "!"]),
        ],
    );
    assert_eq!(
        take_ops(&mut queue),
        ops(&[json!(["a", ["s"], "😀é!"]), json!(["s", ["n"], 1])])
    );
}

#[test]
fn interleaved_leaves_then_a_structural_op_then_the_same_leaves() {
    let mut queue = PendingOps::new();
    push_each(
        &mut queue,
        &[
            json!(["a", ["a"], "1"]),
            json!(["a", ["b"], "1"]),
            json!(["a", ["a"], "2"]),
            json!(["p", ["list"], 1, 0, [1]]),
            json!(["a", ["b"], "2"]),
            json!(["a", ["a"], "3"]),
        ],
    );
    assert_eq!(
        take_ops(&mut queue),
        ops(&[
            json!(["a", ["a"], "12"]),
            json!(["a", ["b"], "1"]),
            json!(["p", ["list"], 1, 0, [1]]),
            json!(["a", ["b"], "2"]),
            json!(["a", ["a"], "3"]),
        ])
    );
}

fn distinct_sets(count: usize, from: usize) -> Vec<Op> {
    (0..count)
        .map(|index| {
            Op::Set(
                vec![Seg::Key("k".into()), Seg::Index((from + index) as i64)],
                json!(index),
            )
        })
        .collect()
}

#[test]
fn keeps_max_pending_ops_and_replaces_past_them() {
    let mut queue = PendingOps::new();
    queue.push(&[]);
    queue.push(&distinct_sets(MAX_PENDING_OPS, 0));
    let exact = take_ops(&mut queue);
    assert_eq!(exact.len(), MAX_PENDING_OPS);

    queue.push(&distinct_sets(MAX_PENDING_OPS / 2, 0));
    queue.push(&distinct_sets(MAX_PENDING_OPS / 2, MAX_PENDING_OPS / 2));
    let full = take_ops(&mut queue);
    assert_eq!(full.len(), MAX_PENDING_OPS);

    queue.push(&distinct_sets(MAX_PENDING_OPS / 2, 0));
    queue.push(&distinct_sets(MAX_PENDING_OPS / 2 + 1, MAX_PENDING_OPS / 2));
    assert_eq!(queue.take(), PendingTake::Replace);
}

#[test]
fn counts_ops_after_folding() {
    let mut queue = PendingOps::new();
    let append = op(json!(["a", ["text"], "x"]));
    for _ in 0..(3 * MAX_PENDING_OPS) {
        queue.push(std::slice::from_ref(&append));
    }
    assert_eq!(
        take_ops(&mut queue),
        vec![Op::Append(
            vec![Seg::Key("text".into())],
            "x".repeat(3 * MAX_PENDING_OPS)
        )]
    );
}

#[test]
fn accumulates_nothing_once_replaced_until_taken_or_a_base_batch() {
    let mut queue = PendingOps::new();
    queue.push(&distinct_sets(1, 0));
    queue.push(&distinct_sets(MAX_PENDING_OPS + 1, 0));
    queue.push(&distinct_sets(3, 0));
    assert_eq!(queue.take(), PendingTake::Replace);
    assert_eq!(queue.take(), PendingTake::Ops(Vec::new()));

    queue.push(&distinct_sets(1, 0));
    queue.push(&distinct_sets(MAX_PENDING_OPS + 1, 0));
    let fresh = ops(&[json!(["r", {"k": [0]}])]);
    queue.push(&fresh);
    assert_eq!(take_ops(&mut queue), fresh);
    queue.push(&distinct_sets(1, 0));
    queue.push(&fresh);
    queue.push(&[op(json!(["s", ["n"], 1]))]);
    let expected = [fresh.clone(), ops(&[json!(["s", ["n"], 1])])].concat();
    assert_eq!(take_ops(&mut queue), expected);
}

#[test]
fn hands_one_batch_over_as_received_whatever_its_size() {
    let mut queue = PendingOps::new();
    let batch = distinct_sets(MAX_PENDING_OPS + 1, 0);
    queue.push(&batch);
    assert_eq!(take_ops(&mut queue), batch);
    queue.push(&batch);
    queue.push(&[op(json!(["s", ["n"], 1]))]);
    assert_eq!(queue.take(), PendingTake::Replace);
}

#[test]
fn never_mutates_received_batches() {
    let first = ops(&[json!(["a", ["s"], "1"])]);
    let second = ops(&[json!(["a", ["s"], "2"])]);
    let mut queue = PendingOps::new();
    queue.push(&first);
    assert_eq!(take_ops(&mut queue), first);
    queue.push(&first);
    queue.push(&second);
    assert_eq!(take_ops(&mut queue), ops(&[json!(["a", ["s"], "12"])]));
    assert_eq!(first, ops(&[json!(["a", ["s"], "1"])]));
    assert_eq!(second, ops(&[json!(["a", ["s"], "2"])]));
}

#[test]
fn size_counts_pending_after_folding_the_replacement_and_nothing_once_taken() {
    let mut queue = PendingOps::new();
    assert_eq!(queue.size(), 0);
    queue.push(&[]);
    assert_eq!(queue.size(), 0);
    queue.push(&ops(&[json!(["a", ["text"], "a"]), json!(["s", ["n"], 1])]));
    assert_eq!(queue.size(), 2);
    queue.push(&ops(&[
        json!(["a", ["text"], "b"]),
        json!(["s", ["n"], 2]),
        json!(["p", ["list"], 0, 0, [1]]),
    ]));
    assert_eq!(queue.size(), 3);
    queue.take();
    assert_eq!(queue.size(), 0);

    queue.push(&distinct_sets(MAX_PENDING_OPS, 0));
    assert_eq!(queue.size(), MAX_PENDING_OPS);
    queue.push(&[op(json!(["s", ["n"], 1]))]);
    assert_eq!(queue.size(), 1);
    assert_eq!(queue.take(), PendingTake::Replace);
    assert_eq!(queue.size(), 0);
}

#[test]
fn keeps_one_op_per_leaf_path_however_many_commits_write_it() {
    let mut queue = PendingOps::new();
    let mut batch = ops(&[
        json!(["a", ["entries", 0, "text"], "tok "]),
        json!(["s", ["meta", "tokens"], 0]),
    ]);
    for index in 0..50_000 {
        batch[1] = Op::Set(
            vec![Seg::Key("meta".into()), Seg::Key("tokens".into())],
            json!(index),
        );
        queue.push(&batch);
    }
    assert_eq!(
        take_ops(&mut queue),
        vec![
            Op::Append(
                vec![
                    Seg::Key("entries".into()),
                    Seg::Index(0),
                    Seg::Key("text".into())
                ],
                "tok ".repeat(50_000)
            ),
            Op::Set(
                vec![Seg::Key("meta".into()), Seg::Key("tokens".into())],
                json!(49_999)
            ),
        ]
    );
}
