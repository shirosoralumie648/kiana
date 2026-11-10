//! `runtime/protocol.ts` 的移植测试。
//!
//! TS 侧没有独立的 protocol 测试文件（守卫与消息形状由 `sandbox.test.ts` 的
//! `worker host` 组间接覆盖：`raw-worker.ts` 注入畸形消息）。这里把那些间接断言
//! 收敛成直接的守卫/编解码断言，并逐字锁定 spec §3.4 的 `unknown message from the worker`
//! 判定条件（只认 `type` 标签）。

use kiana_codemode::runtime::protocol::{
    CallTarget, HostResult, HostToWorkerMessage, VmGlobalInfo, VmStartData, VmToolInfo, WorkerData,
    WorkerDone, WorkerDoneError, WorkerDoneOutcome, WorkerToHostMessage, is_host_to_worker_message,
    is_worker_to_host_message,
};
use kiana_codemode::{CodemodeDuring, CodemodeOutputItem};
use serde_json::json;

// 守卫只检查对象与 `type` 标签（TS 的 `isWorkerToHostMessage`）。
#[test]
fn worker_to_host_guard_accepts_only_the_six_types() {
    for tag in ["call", "output", "done", "crash", "started", "idle"] {
        assert!(
            is_worker_to_host_message(&json!({ "type": tag })),
            "{tag} must pass the guard"
        );
    }
    for value in [
        json!(null),
        json!(true),
        json!(1),
        json!("call"),
        json!([]),
        json!({}),
        json!({ "type": "result" }),
        json!({ "type": "failure" }),
        json!({ "type": "stop" }),
        json!({ "type": null }),
        json!({ "type": 7 }),
    ] {
        assert!(
            !is_worker_to_host_message(&value),
            "{value} must fail the guard"
        );
    }
    // 其余字段完全不校验。
    assert!(is_worker_to_host_message(
        &json!({ "type": "done", "ok": "nope" })
    ));
    assert!(is_worker_to_host_message(
        &json!({ "type": "call", "id": -1 })
    ));
}

#[test]
fn host_to_worker_guard_accepts_only_result() {
    assert!(is_host_to_worker_message(&json!({ "type": "result" })));
    for value in [
        json!(null),
        json!("result"),
        json!([]),
        json!({}),
        json!({ "type": "call" }),
        json!({ "type": "stop" }),
    ] {
        assert!(
            !is_host_to_worker_message(&value),
            "{value} must fail the guard"
        );
    }
    assert!(is_host_to_worker_message(
        &json!({ "type": "result", "id": 1 })
    ));
}

#[test]
fn call_message_carries_json_text_arguments() {
    let message = WorkerToHostMessage::Call {
        id: 1,
        target: CallTarget::Tool,
        name: "read".into(),
        args: Some(r#"{"path":"a"}"#.into()),
    };
    assert_eq!(
        serde_json::to_value(&message).unwrap(),
        json!({ "type": "call", "id": 1, "target": "tool", "name": "read", "args": "{\"path\":\"a\"}" })
    );
    let global = WorkerToHostMessage::Call {
        id: 2,
        target: CallTarget::Global,
        name: "models.list".into(),
        args: None,
    };
    let encoded = serde_json::to_value(&global).unwrap();
    assert_eq!(encoded["target"], json!("global"));
    assert!(
        encoded.get("args").is_none(),
        "undefined args must be omitted"
    );
}

#[test]
fn done_success_and_failure_share_the_done_tag() {
    let ok = WorkerToHostMessage::done_ok(
        Some("42".into()),
        r#"[["k","1"],["gone"]]"#.into(),
        Some(CodemodeDuring { seq: 1, issued: 2 }),
    );
    assert_eq!(
        serde_json::to_value(&ok).unwrap(),
        json!({
            "type": "done", "ok": true, "value": "42",
            "writes": "[[\"k\",\"1\"],[\"gone\"]]",
            "during": { "seq": 1, "issued": 2 }
        })
    );
    assert_eq!(ok.type_name(), "done");

    let error =
        WorkerToHostMessage::done_error(r#"{"name":"TypeError","message":"boom"}"#.into(), None);
    let encoded = serde_json::to_value(&error).unwrap();
    assert_eq!(encoded["type"], json!("done"));
    assert_eq!(encoded["ok"], json!(false));
    assert!(encoded.get("during").is_none());

    // 往返。
    for message in [ok, error] {
        let text = serde_json::to_string(&message).unwrap();
        assert_eq!(
            serde_json::from_str::<WorkerToHostMessage>(&text).unwrap(),
            message
        );
    }
}

#[test]
fn done_discriminates_on_ok_for_deserialization() {
    let ok: WorkerToHostMessage =
        serde_json::from_str(r#"{"type":"done","ok":true,"value":"1","writes":"[]"}"#).unwrap();
    assert!(matches!(
        ok.done(),
        Some(WorkerDoneOutcome::Ok(WorkerDone { ok: true, .. }))
    ));
    let failed: WorkerToHostMessage =
        serde_json::from_str(r#"{"type":"done","ok":false,"error":"{}"}"#).unwrap();
    assert!(matches!(
        failed.done(),
        Some(WorkerDoneOutcome::Error(WorkerDoneError { ok: false, .. }))
    ));
}

#[test]
fn output_items_use_the_types_output_shape() {
    let text = WorkerToHostMessage::Output {
        item: CodemodeOutputItem::text("hi"),
    };
    assert_eq!(
        serde_json::to_value(&text).unwrap(),
        json!({ "type": "output", "item": { "type": "text", "text": "hi" } })
    );
    let console = WorkerToHostMessage::Output {
        item: CodemodeOutputItem::console("warn"),
    };
    assert_eq!(
        serde_json::to_value(&console).unwrap()["item"],
        json!({ "type": "text", "text": "warn", "console": true })
    );
    let image = WorkerToHostMessage::Output {
        item: CodemodeOutputItem::image("AAAA", "image/png"),
    };
    assert_eq!(
        serde_json::to_value(&image).unwrap()["item"],
        json!({ "type": "image", "data": "AAAA", "mimeType": "image/png" })
    );
}

#[test]
fn crash_started_idle_shapes() {
    assert_eq!(
        serde_json::to_value(WorkerToHostMessage::Crash {
            message: "wasm trap".into()
        })
        .unwrap(),
        json!({ "type": "crash", "message": "wasm trap" })
    );
    assert_eq!(
        serde_json::to_value(WorkerToHostMessage::Started {
            fingerprint: "quickjs-wasi 3.6.2".into()
        })
        .unwrap(),
        json!({ "type": "started", "fingerprint": "quickjs-wasi 3.6.2" })
    );
    assert_eq!(
        serde_json::to_value(WorkerToHostMessage::Idle { seq: 0, issued: 0 }).unwrap(),
        json!({ "type": "idle", "seq": 0, "issued": 0 })
    );
}

#[test]
fn host_result_fields_and_optional_keys() {
    let plain = HostToWorkerMessage::ok(4, Some("true".into()));
    assert_eq!(
        serde_json::to_value(&plain).unwrap(),
        json!({ "type": "result", "id": 4, "ok": true, "payload": "true" })
    );
    assert_eq!(plain.id(), 4);

    let deterministic = HostToWorkerMessage::Result(HostResult {
        id: 5,
        ok: false,
        payload: Some("boom".into()),
        seq: Some(2),
        now: Some(1_700_000_000_000.0),
        properties: Some(r#"{"structuredOutput":1}"#.into()),
    });
    assert_eq!(
        serde_json::to_value(&deterministic).unwrap(),
        json!({
            "type": "result", "id": 5, "ok": false, "payload": "boom",
            // JS 的 `JSON.stringify` 写 `1700000000000`；`JSON.parse` 回来是整数形态，
            // 所以这里用整数字面量（`json!(1_700_000_000_000.0)` 是 `Number::Float`，
            // 与整数 `Number::PosInt` 在 serde_json 里不相等）。
            "seq": 2, "now": 1_700_000_000_000i64,
            "properties": "{\"structuredOutput\":1}"
        })
    );
    // 写出文本是 JS 的 `1700000000000`（`JSON.stringify` 对该数字的输出）：
    // 直接 `to_string` 与「先 `to_value` 再走 `js_object::stringify`」两种路径一致。
    let expected = r#"{"type":"result","id":5,"ok":false,"payload":"boom","seq":2,"now":1700000000000,"properties":"{\"structuredOutput\":1}"}"#;
    assert_eq!(serde_json::to_string(&deterministic).unwrap(), expected);
    assert_eq!(
        kiana_json::js_object::stringify(&serde_json::to_value(&deterministic).unwrap()),
        expected
    );

    // payload 为 undefined（TS）→ 键省略。
    let empty = HostToWorkerMessage::Result(HostResult {
        id: 6,
        ok: true,
        payload: None,
        seq: None,
        now: None,
        properties: None,
    });
    assert_eq!(
        serde_json::to_value(&empty).unwrap(),
        json!({ "type": "result", "id": 6, "ok": true })
    );
}

#[test]
fn vm_start_data_serializes_with_camel_case_and_omits_undefined() {
    let mut store = indexmap::IndexMap::new();
    store.insert("first".to_string(), "1".to_string());
    store.insert("second".to_string(), "\"two\"".to_string());
    let data = VmStartData {
        code: "text(1)".into(),
        tools: vec![VmToolInfo {
            name: "my-tool".into(),
            js_name: "my_tool".into(),
            description: "does things".into(),
        }],
        globals: vec![VmGlobalInfo {
            name: "models.list".into(),
            spread: true,
        }],
        memory_limit_bytes: Some(33_554_432.0),
        store,
        deterministic: None,
    };
    let encoded = serde_json::to_value(&data).unwrap();
    assert_eq!(encoded["tools"][0]["jsName"], json!("my_tool"));
    // JS 的 `JSON.stringify({memoryLimitBytes: 33554432})` 写 `33554432`（不是 `33554432.0`）。
    assert_eq!(encoded["memoryLimitBytes"], json!(33_554_432));
    assert!(
        serde_json::to_string(&data)
            .unwrap()
            .contains("\"memoryLimitBytes\":33554432,"),
        "the JSON text must use the JavaScript number form"
    );
    assert_eq!(encoded["globals"][0]["spread"], json!(true));
    assert!(encoded.get("deterministic").is_none());
    // store 保持插入序（IndexMap / preserve_order）。
    let keys: Vec<&String> = encoded["store"].as_object().unwrap().keys().collect();
    assert_eq!(keys, vec!["first", "second"]);

    // 往返（key 序不变）。
    let text = serde_json::to_string(&data).unwrap();
    assert_eq!(serde_json::from_str::<VmStartData>(&text).unwrap(), data);
}

#[test]
fn worker_data_wraps_start_data() {
    let data = VmStartData {
        code: "1".into(),
        tools: vec![],
        globals: vec![],
        memory_limit_bytes: None,
        store: indexmap::IndexMap::new(),
        deterministic: Some(kiana_codemode::runtime::protocol::VmDeterministic {
            started_at: 5.0,
            seed: 3.0,
        }),
    };
    // WorkerData 含 wasm 模块与中断标志，无法在无 `wasm` feature 时构造；只验证其
    // `start_data` 访问器在类型层面存在（用函数指针锁定签名）。
    let accessor: fn(&WorkerData) -> &VmStartData = WorkerData::start_data;
    let _ = accessor;
    assert_eq!(data.code, "1");
}

// ---------------------------------------------------------------------------
// JSON 文本保真（`ARCHITECTURE.md` §4.3 第 3、4 条）：数字按 ECMAScript
// `Number::toString`、`Record<string, unknown>` 按 JS 枚举序、`null` 与 `undefined` 分开。
//
// 黄金值由 Node 26 的 `JSON.stringify` 生成（脚本：
// `/home/ctyun/kiana-work/scratch/kiana-codemode__core/gen_json_cases.mjs`），用来核对
// 写入 journal / wire 的文本；这是当前稳定契约，改动需同步黄金值。
// ---------------------------------------------------------------------------
mod json_text_golden {
    use kiana_codemode::runtime::protocol::{
        HostResult, HostToWorkerMessage, VmDeterministic, VmStartData,
    };
    use kiana_codemode::source::CodemodeSourceOptions;
    use kiana_codemode::types::CodemodeOutcomeErr;
    use kiana_codemode::{CodemodeCall, CodemodeCallStatus, CodemodeOutcome, CodemodeResult};
    use serde_json::Value;

    fn fixture() -> serde_json::Map<String, Value> {
        let value: Value = serde_json::from_str(include_str!("fixtures/json_text_cases.json"))
            .expect("fixture parses");
        value.as_object().expect("fixture is an object").clone()
    }

    fn expected(name: &str) -> String {
        fixture()[name].as_str().expect("golden is a string").into()
    }

    fn host_result_with_now(now: f64) -> HostToWorkerMessage {
        HostToWorkerMessage::Result(HostResult {
            id: 1,
            ok: true,
            payload: None,
            seq: None,
            now: Some(now),
            properties: None,
        })
    }

    // `CodemodeCall.durationMs`：`3.0` 在 JS 里写 `3`（serde 默认写 `3.0`）。
    #[test]
    fn call_duration_uses_javascript_number_text() {
        let call = |duration_ms: f64| CodemodeCall {
            name: "t".into(),
            status: CodemodeCallStatus::Ok,
            duration_ms,
        };
        assert_eq!(
            serde_json::to_string(&call(3.0)).unwrap(),
            expected("call_duration")
        );
        assert_eq!(
            serde_json::to_string(&call(1.5)).unwrap(),
            expected("call_duration_frac")
        );
        assert_eq!(
            serde_json::to_string(&call(0.000001)).unwrap(),
            expected("call_duration_small")
        );
        assert_eq!(
            serde_json::to_string(&call(1e-7)).unwrap(),
            expected("call_duration_exp")
        );
    }

    // `HostResult.now`：整数形态、epoch 毫秒、`-0`、`1e-6`、`1e21`、`NaN`/`Infinity` → `null`。
    #[test]
    fn host_result_now_uses_javascript_number_text() {
        for (now, name) in [
            (1000.0, "host_now_int"),
            (1_700_000_000_000.0, "host_now_epoch"),
            (-0.0, "host_now_negzero"),
            (1e-6, "host_now_micro"),
            (1e21, "host_now_big"),
            (f64::NAN, "host_now_nan"),
            (f64::INFINITY, "host_now_inf"),
        ] {
            assert_eq!(
                serde_json::to_string(&host_result_with_now(now)).unwrap(),
                expected(name),
                "now = {now:?}"
            );
            // `to_value` 再走 `js_object::stringify` 必须给出同一段文本（journal 的写出路径）。
            let value = serde_json::to_value(host_result_with_now(now)).unwrap();
            assert_eq!(kiana_json::js_object::stringify(&value), expected(name));
        }
    }

    // `VmStartData.memoryLimitBytes` 与 `VmDeterministic { startedAt, seed }`。
    #[test]
    fn vm_start_data_numbers_use_javascript_text() {
        let data = VmStartData {
            code: "1".into(),
            tools: vec![],
            globals: vec![],
            memory_limit_bytes: Some(33_554_432.0),
            store: indexmap::IndexMap::new(),
            deterministic: Some(VmDeterministic {
                started_at: 5.0,
                seed: 7.0,
            }),
        };
        assert_eq!(serde_json::to_string(&data).unwrap(), expected("vm_start"));
    }

    // `CodemodeSourceOptions` 的两个可选数字（`@options` 行解析结果）。
    #[test]
    fn source_options_numbers_use_javascript_text() {
        let options = CodemodeSourceOptions {
            max_output_tokens: Some(1.0),
            timeout_ms: Some(1500.0),
        };
        assert_eq!(
            serde_json::to_string(&options).unwrap(),
            expected("source_options")
        );
    }

    // `CodemodeStoreWrites.set`：JS 枚举序（整数索引键升序在前，其余插入序）。
    #[test]
    fn store_writes_keys_follow_javascript_enumeration_order() {
        let mut set = indexmap::IndexMap::new();
        set.insert("2".to_string(), serde_json::json!(1));
        set.insert("1".to_string(), serde_json::json!(2));
        set.insert("b".to_string(), serde_json::json!(3));
        let writes = kiana_codemode::CodemodeStoreWrites {
            set,
            delete: vec![],
        };
        assert_eq!(
            serde_json::to_string(&writes).unwrap(),
            expected("store_writes")
        );

        // 前导零键不是数组索引（`"01"` 保持插入序的末位）。
        let mut set = indexmap::IndexMap::new();
        set.insert("10".to_string(), serde_json::json!("a"));
        set.insert("2".to_string(), serde_json::json!("b"));
        set.insert("01".to_string(), serde_json::json!("c"));
        let writes = kiana_codemode::CodemodeStoreWrites {
            set,
            delete: vec!["k".into()],
        };
        assert_eq!(
            serde_json::to_string(&writes).unwrap(),
            expected("store_writes_index_order")
        );
    }

    // `CodemodeOutcomeErr.properties` 与 `VmStartData.store` 同属 `Record<string, …>`。
    #[test]
    fn outcome_properties_and_vm_store_follow_javascript_order() {
        let error = CodemodeOutcomeErr {
            ok: false,
            message: "m".into(),
            properties: Some(indexmap::IndexMap::from([
                ("10".to_string(), serde_json::json!(1)),
                ("2".to_string(), serde_json::json!(2)),
                ("a".to_string(), serde_json::json!(3)),
            ])),
        };
        assert_eq!(
            serde_json::to_string(&error).unwrap(),
            expected("outcome_properties")
        );

        let mut store = indexmap::IndexMap::new();
        store.insert("2".to_string(), "b".to_string());
        store.insert("1".to_string(), "a".to_string());
        let data = VmStartData {
            code: "1".into(),
            tools: vec![],
            globals: vec![],
            memory_limit_bytes: None,
            store,
            deterministic: None,
        };
        assert_eq!(serde_json::to_string(&data).unwrap(), expected("vm_store"));
    }

    // `"value": null` 是脚本返回的 JSON `null`；键缺失才是 `undefined`。
    #[test]
    fn explicit_null_is_not_undefined() {
        let result: CodemodeResult = serde_json::from_str(
            r#"{"ok":true,"value":null,"output":[],"calls":[],"storeWrites":{"set":{},"delete":[]}}"#,
        )
        .unwrap();
        assert_eq!(
            serde_json::to_string(&result).unwrap(),
            expected("result_null_value")
        );
        let CodemodeResult::Ok(ok) = &result else {
            panic!("expected success");
        };
        assert_eq!(ok.value, Some(Value::Null));

        let outcome: CodemodeOutcome = serde_json::from_str(r#"{"ok":true,"value":null}"#).unwrap();
        assert_eq!(
            serde_json::to_string(&outcome).unwrap(),
            expected("outcome_null_value")
        );

        // 缺失 == `undefined`：写回时省略键。
        let absent: CodemodeOutcome = serde_json::from_str(r#"{"ok":true}"#).unwrap();
        assert_eq!(serde_json::to_string(&absent).unwrap(), r#"{"ok":true}"#);
        assert_ne!(absent, outcome);
    }

    // `CodemodeHostCall.args`：`null` 是脚本传的 `null`，缺失是无参调用。
    #[test]
    fn host_call_null_args_differ_from_absent_args() {
        let with_null: kiana_codemode::CodemodeHostCall =
            serde_json::from_str(r#"{"id":1,"target":"tool","name":"x","args":null}"#).unwrap();
        let without: kiana_codemode::CodemodeHostCall =
            serde_json::from_str(r#"{"id":1,"target":"tool","name":"x"}"#).unwrap();
        assert_eq!(
            serde_json::to_string(&with_null).unwrap(),
            expected("host_call_null_args")
        );
        assert_eq!(
            serde_json::to_string(&without).unwrap(),
            expected("host_call_absent_args")
        );
        assert_ne!(with_null, without);
        assert_eq!(with_null.args, Some(Value::Null));
        assert_eq!(without.args, None);
    }

    // 差分语料（Node 26 的 `JSON.stringify` 生成，见 fixture 头）：数字覆盖 `-0`、`1e-7`、
    // `1e21`、`2^53` 附近、次正规数、最大有限数；键集合覆盖整数索引、前导零、`4294967295`、
    // 负数、非 ASCII 与插入序。
    mod differential_corpus {
        use kiana_codemode::runtime::protocol::{HostResult, HostToWorkerMessage, VmStartData};
        use kiana_codemode::types::CodemodeOutcomeErr;
        use kiana_codemode::{CodemodeCall, CodemodeCallStatus, CodemodeStoreWrites};
        use serde_json::Value;

        fn fixture() -> Value {
            serde_json::from_str(include_str!("fixtures/json_fuzz_cases.json")).expect("fixture")
        }

        #[test]
        fn numbers_match_the_javascript_text() {
            let fixture = fixture();
            for row in fixture["numbers"].as_array().expect("numbers") {
                let text = row["text"].as_str().expect("text");
                let value: f64 = text.parse().expect("f64");
                assert_eq!(
                    serde_json::to_string(&CodemodeCall {
                        name: "t".into(),
                        status: CodemodeCallStatus::Ok,
                        duration_ms: value,
                    })
                    .unwrap(),
                    row["duration"].as_str().unwrap(),
                    "durationMs for {text}"
                );
                let host = HostToWorkerMessage::Result(HostResult {
                    id: 1,
                    ok: true,
                    payload: None,
                    seq: None,
                    now: Some(value),
                    properties: None,
                });
                let encoded = serde_json::to_string(&host).unwrap();
                // `now` 的文本与 `JSON.stringify({now: value})` 的 `now` 部分相同。
                let expected_now = row["value"].as_str().unwrap();
                let start = encoded.find("\"now\":").expect("now key") + 6;
                let tail = encoded[start..].trim_end_matches('}');
                assert_eq!(tail, expected_now, "now for {text}");
            }
        }

        #[test]
        fn record_keys_match_the_javascript_order() {
            let fixture = fixture();
            for case in fixture["key_sets"].as_array().expect("key sets") {
                let keys: Vec<String> = case["keys"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|key| key.as_str().unwrap().to_string())
                    .collect();
                let mut set = indexmap::IndexMap::new();
                for (index, key) in keys.iter().enumerate() {
                    set.insert(key.clone(), serde_json::json!(index));
                }
                assert_eq!(
                    serde_json::to_string(&CodemodeStoreWrites {
                        set: set.clone(),
                        delete: vec![],
                    })
                    .unwrap(),
                    case["expected"].as_str().unwrap(),
                    "store writes for {keys:?}"
                );
                assert_eq!(
                    serde_json::to_string(&CodemodeOutcomeErr {
                        ok: false,
                        message: "m".into(),
                        properties: Some(set),
                    })
                    .unwrap(),
                    case["props"].as_str().unwrap(),
                    "properties for {keys:?}"
                );
                let mut store = indexmap::IndexMap::new();
                for (index, key) in keys.iter().enumerate() {
                    store.insert(key.clone(), index.to_string());
                }
                assert_eq!(
                    serde_json::to_string(&VmStartData {
                        code: "1".into(),
                        tools: vec![],
                        globals: vec![],
                        memory_limit_bytes: None,
                        store,
                        deterministic: None,
                    })
                    .unwrap(),
                    case["vm"].as_str().unwrap(),
                    "vm store for {keys:?}"
                );
            }
        }
    }

    // 反序列化把 `Record` 的键排成 JS 枚举序（`JSON.parse` 之后 `Object.keys` 的顺序）。
    #[test]
    fn deserialized_records_use_javascript_enumeration_order() {
        let writes: kiana_codemode::CodemodeStoreWrites =
            serde_json::from_str(r#"{"set":{"b":1,"2":2,"1":3},"delete":[]}"#).unwrap();
        assert_eq!(
            writes.set.keys().cloned().collect::<Vec<_>>(),
            ["1", "2", "b"]
        );

        let data: VmStartData = serde_json::from_str(
            r#"{"code":"1","tools":[],"globals":[],"store":{"2":"b","1":"a"}}"#,
        )
        .unwrap();
        assert_eq!(data.store.keys().cloned().collect::<Vec<_>>(), ["1", "2"]);
    }
}
