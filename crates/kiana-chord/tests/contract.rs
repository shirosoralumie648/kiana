//! `packages/chord/test/contract.test.ts` 的 Rust 端口（7 条 `it`）。
//!
//! TS 用 `node:assert/strict` + TypeBox；这里是 `assert_eq!`/`match` + 手写的 TypeBox JSON
//! （`schema.rs` 直接吃 JSON 关键字，见模块文档）。夹具 `fixtures/contract_golden.json`
//! 由 `scratch/kiana-chord__core/gen_contract.mjs` 在 TS 参考实现上生成：指纹、description
//! JSON、`coverage`/`bind` 的 reason 与错误消息都逐字断言。

use std::sync::Arc;

use indexmap::IndexMap;
use kiana_chord::contract::{
    Accepts, Adapter, AnyContract, Description, DescriptionScope, LOCAL_VARIANT, LocalDef,
    LocalMember, MemberDescription, MethodSpec, Placement, RESERVED_MEMBERS, RemoteDef, VariantDef,
    bind, contract_from_description, contract_from_description_json, coverage, host_family,
    host_named, js_object_order, local, method, normalize_input, opaque, placed_on, placement_text,
    remote, remote_from_json, resolve_host,
};
use kiana_chord::errors::{
    ChordAggregateError, ChordError, ChordErrorCode, invalid, is_chord_error,
};
use kiana_chord::schema::{
    canonical_identity, canonical_json, fits, hash64, mark_live_schema, normalize,
    strip_live_markers,
};
use serde_json::{Value, json};

fn golden() -> Value {
    serde_json::from_str(include_str!("fixtures/contract_golden.json")).expect("contract golden")
}

fn view() -> Value {
    json!({"type": "object", "required": ["text"], "properties": {"text": {"type": "string"}}})
}

fn chat_input() -> Value {
    json!({
        "type": "object",
        "required": ["session", "conversation"],
        "properties": {
            "session": {"type": "string"},
            "conversation": {"type": "number", "default": 0}
        }
    })
}

fn send_args() -> Value {
    json!({"type": "object", "required": ["text"], "properties": {"text": {"type": "string"}}})
}

fn send_result() -> Value {
    json!({"type": "object", "required": ["id"], "properties": {"id": {"type": "number"}}})
}

fn empty_object() -> Value {
    json!({"type": "object", "properties": {}})
}

/// `const chatV1 = remote("t.chat", {…})`（contract.test.ts 的夹具）。
fn chat_v1() -> kiana_chord::contract::RemoteContract {
    let mut variants = IndexMap::new();
    variants.insert("viewer".to_owned(), VariantDef::new().state("view", view()));
    variants.insert(
        "participant".to_owned(),
        VariantDef::new().state("view", view()).method(
            "send",
            MethodSpec::with(Some(send_args()), Some(send_result())),
        ),
    );
    remote(
        "t.chat",
        RemoteDef {
            version: 1,
            input: chat_input(),
            host: host_family("session:", "session").expect("family placement"),
            variants,
        },
    )
    .expect("chatV1")
}

/// `chatV1.def`（TS 里 `...chatV1.def` 展开）。
fn chat_def() -> RemoteDef {
    RemoteDef {
        version: 1,
        input: chat_input(),
        host: host_family("session:", "session").expect("family placement"),
        variants: chat_v1().def.variants,
    }
}

fn family() -> Placement {
    host_family("session:", "session").expect("family placement")
}

/// `isChordError(error, "invalid") && error.reason === "input"`。
fn is_invalid_input(error: &ChordError) -> bool {
    is_chord_error(error, Some(ChordErrorCode::Invalid)) && error.reason.as_deref() == Some("input")
}

// ── it("normalizes input and omits defaults from the identity") ────────────────────

#[test]
fn normalizes_input_and_omits_defaults_from_the_identity() {
    let chat = chat_v1();
    let golden = golden();
    let a = normalize(
        &chat.base.input,
        Some(&json!({"session": "s", "extra": 1})),
        "input",
    )
    .expect("normalize");
    assert_eq!(
        a, golden["normalizeInput"],
        "normalize(chatV1.input, {{ session, extra }})"
    );
    let b = normalize(
        &chat.base.input,
        Some(&json!({"conversation": 0, "session": "s"})),
        "input",
    )
    .expect("normalize");
    assert_eq!(
        canonical_identity(&chat.base.input, &a),
        canonical_identity(&chat.base.input, &b),
        "key order does not matter"
    );
    assert_eq!(
        canonical_identity(&chat.base.input, &a),
        golden["identityA"].as_str().expect("identityA")
    );
    assert_eq!(
        canonical_identity(&chat.base.input, &b),
        golden["identityB"].as_str().expect("identityB")
    );
    assert_eq!(
        canonical_identity(&chat.base.input, &a),
        r#"{"session":"s"}"#,
        "the default `conversation: 0` is omitted"
    );

    let error = normalize(&chat.base.input, Some(&json!({"session": 1})), "input")
        .expect_err("not a string");
    assert!(
        is_invalid_input(&error),
        "expected invalid/input, got {error}"
    );
}

// ── it("fingerprints are stable and description-based") ───────────────────────────

#[test]
fn fingerprints_are_stable_and_description_based() {
    let chat = chat_v1();
    let golden = golden();
    let copy = remote("t.chat", chat_def()).expect("copy");
    assert_eq!(
        copy.base.fingerprint, chat.base.fingerprint,
        "the same definition fingerprints the same"
    );
    let v2 = remote(
        "t.chat",
        RemoteDef {
            version: 2,
            ..chat_def()
        },
    )
    .expect("v2");
    assert_ne!(
        v2.base.fingerprint, chat.base.fingerprint,
        "a version bump changes it"
    );

    // 夹具：description JSON 与指纹逐字一致。
    assert_eq!(
        chat.base.fingerprint,
        golden["chatV1"]["fingerprint"].as_str().expect("fp")
    );
    assert_eq!(
        chat.base.description.to_json(),
        golden["chatV1"]["description"],
        "the description is the wire form"
    );
    assert_eq!(
        copy.base.fingerprint,
        golden["chatV1Copy"]["fingerprint"].as_str().expect("fp")
    );
    assert_eq!(
        v2.base.fingerprint,
        golden["chatV2"]["fingerprint"].as_str().expect("fp")
    );
    assert_eq!(
        hash64(&canonical_json(&chat.base.description.to_json())),
        chat.base.fingerprint,
        "the fingerprint is hash64(canonicalJson(description))"
    );
}

// ── it("rejects reserved member names and opaque remote members") ──────────────────

#[test]
fn rejects_reserved_member_names_and_opaque_remote_members() {
    let mut members = IndexMap::new();
    members.insert("status".to_owned(), LocalMember::Method(method()));
    let error = local(
        "t.x",
        LocalDef {
            version: 1,
            input: empty_object(),
            members,
        },
    )
    .expect_err("reserved");
    assert_eq!(error.code, ChordErrorCode::Invalid);
    assert_eq!(error.reason.as_deref(), Some("contract"));
    assert_eq!(error.message, "member name is reserved: status");

    let mut variants = IndexMap::new();
    variants.insert(
        "a".to_owned(),
        VariantDef::new().state("h", LocalMember::Opaque(opaque("x@1").expect("opaque"))),
    );
    let error = remote(
        "t.x",
        RemoteDef {
            version: 1,
            input: empty_object(),
            host: host_named("server").expect("named"),
            variants,
        },
    )
    .expect_err("remote opaque");
    assert_eq!(
        error.message,
        "remote contracts cannot expose opaque members: h"
    );

    assert!(opaque("x@1").is_ok());
    assert!(opaque("x").is_err());
    assert!(opaque("@1").is_err());
    assert!(opaque("x@v1").is_err());
}

// ── it("covers additive changes and refuses breaking ones") ────────────────────────

#[test]
fn covers_additive_changes_and_refuses_breaking_ones() {
    let chat = chat_v1();
    let golden = golden();

    // 加法变更：多一个 state 字段、多一个可选参数、多一个结果字段。
    let mut variants = chat_def().variants;
    variants.insert(
        "participant".to_owned(),
        VariantDef::new()
            .state(
                "view",
                json!({"type": "object", "required": ["text", "extra"], "properties": {"text": {"type": "string"}, "extra": {"type": "number"}}}),
            )
            .method(
                "send",
                MethodSpec::with(
                    Some(json!({"type": "object", "required": ["text"], "properties": {"text": {"type": "string"}, "urgent": {"type": "boolean"}}})),
                    Some(json!({"type": "object", "required": ["id", "at"], "properties": {"id": {"type": "number"}, "at": {"type": "number"}}})),
                ),
            ),
    );
    let additive = remote(
        "t.chat",
        RemoteDef {
            version: 1,
            input: chat_input(),
            host: family(),
            variants,
        },
    )
    .expect("additive");
    assert_eq!(
        coverage(&additive.base.description, &chat.base.description),
        None,
        "additive changes are covered"
    );
    let provider: AnyContract = additive.into();
    let consumer: AnyContract = chat.clone().into();
    assert!(
        bind(&provider, None, &consumer).expect("bind").is_none(),
        "identical coverage binds without an adapter"
    );

    // 破坏性：新的必填参数。
    let mut variants = chat_def().variants;
    variants.insert(
        "participant".to_owned(),
        VariantDef::new().state("view", view()).method(
            "send",
            MethodSpec::with(
                Some(json!({"type": "object", "required": ["text", "urgent"], "properties": {"text": {"type": "string"}, "urgent": {"type": "boolean"}}})),
                Some(send_result()),
            ),
        ),
    );
    let required = remote(
        "t.chat",
        RemoteDef {
            version: 1,
            input: chat_input(),
            host: family(),
            variants,
        },
    )
    .expect("required");
    let reason = coverage(&required.base.description, &chat.base.description).expect("breaking");
    assert!(
        reason.contains("arguments"),
        "expected an arguments complaint, got {reason:?}"
    );
    assert_eq!(
        reason,
        golden["required"]["coverage"].as_str().expect("golden")
    );
    let provider: AnyContract = required.into();
    let error = bind(&provider, None, &consumer)
        .err()
        .expect("contract mismatch");
    assert_eq!(error.code, ChordErrorCode::ContractMismatch);
    assert_eq!(
        error.message,
        golden["required"]["bindError"]["message"]
            .as_str()
            .expect("golden")
            .strip_prefix("contract_mismatch: ")
            .expect("golden carries the code prefix"),
        "bind's message is the bare text; Display adds the code"
    );
    assert_eq!(
        error.full_message(),
        golden["required"]["bindError"]["message"]
            .as_str()
            .expect("golden")
    );

    // 破坏性：状态 schema 变了。
    let mut variants = chat_def().variants;
    variants.insert(
        "viewer".to_owned(),
        VariantDef::new().state(
            "view",
            json!({"type": "object", "required": ["text"], "properties": {"text": {"type": "number"}}}),
        ),
    );
    let retyped = remote(
        "t.chat",
        RemoteDef {
            version: 1,
            input: chat_input(),
            host: family(),
            variants,
        },
    )
    .expect("retyped");
    let reason = coverage(&retyped.base.description, &chat.base.description).expect("breaking");
    assert!(
        reason.contains("state"),
        "expected a state complaint, got {reason:?}"
    );
    assert_eq!(
        reason,
        golden["retyped"]["coverage"].as_str().expect("golden")
    );
}

// ── it("methods without arguments or result: …") ──────────────────────────────────

#[test]
fn methods_without_arguments_or_result_describe_without_them() {
    let of = |members: IndexMap<String, LocalMember>| {
        local(
            "t.methods",
            LocalDef {
                version: 1,
                input: empty_object(),
                members,
            },
        )
        .expect("local")
        .base
        .description
        .clone()
    };
    let none = of(IndexMap::from([(
        "m".to_owned(),
        LocalMember::Method(method()),
    )]));
    let with_args = of(IndexMap::from([(
        "m".to_owned(),
        LocalMember::Method(MethodSpec::args(json!({"type": "string"}))),
    )]));
    let with_result = of(IndexMap::from([(
        "m".to_owned(),
        LocalMember::Method(MethodSpec::with(None, Some(json!({"type": "number"})))),
    )]));

    // `none.variants[""].m` 是 `{kind: "method"}`（没有 args/result 键）。
    let golden = golden();
    let member = &none.variants[LOCAL_VARIANT]["m"];
    assert_eq!(
        serde_json::to_value(member).expect("json"),
        golden["methods"]["noneMember"]
    );
    assert_eq!(member.kind(), kiana_chord::contract::MemberKind::Method);
    assert!(member.args().is_none());
    assert!(member.result().is_none());

    assert_eq!(coverage(&none, &none), None);
    let reason = coverage(&with_args, &none).expect("provider expects arguments");
    assert!(reason.contains("arguments"), "got {reason:?}");
    assert_eq!(
        reason,
        golden["methods"]["argsNone"].as_str().expect("golden")
    );
    let reason = coverage(&none, &with_args).expect("consumer sends arguments");
    assert!(reason.contains("arguments"), "got {reason:?}");
    assert_eq!(
        reason,
        golden["methods"]["noneArgs"].as_str().expect("golden")
    );
    assert_eq!(
        coverage(&with_result, &none),
        None,
        "a void consumer ignores a result"
    );
    let reason = coverage(&none, &with_result).expect("consumer expects a result");
    assert!(reason.contains("result"), "got {reason:?}");
    assert_eq!(
        reason,
        golden["methods"]["noneResult"].as_str().expect("golden")
    );
}

// ── it("binds other versions only through accepts adapters") ──────────────────────

#[test]
fn binds_other_versions_only_through_accepts_adapters() {
    let chat = chat_v1();
    let golden = golden();
    let mut variants = IndexMap::new();
    variants.insert("viewer".to_owned(), VariantDef::new().state("body", view()));
    let v2 = remote(
        "t.chat",
        RemoteDef {
            version: 2,
            input: chat_input(),
            host: family(),
            variants,
        },
    )
    .expect("v2");

    let provider: AnyContract = v2.into();
    let consumer: AnyContract = chat.into();
    let error = bind(&provider, None, &consumer).err().expect("no adapter");
    assert_eq!(error.code, ChordErrorCode::ContractMismatch);
    assert_eq!(
        error.full_message(),
        golden["v2BindError"]["message"].as_str().expect("golden"),
        "a missing adapter is reported with provider/consumer versions"
    );

    // `accepts: { 1: (current) => ({ variant: "viewer", view: current.body }) }`。
    let adapter: Adapter = Arc::new(
        |current: Value| json!({"variant": "viewer", "view": current.get("body").cloned().unwrap_or(Value::Null)}),
    );
    let mut accepts = Accepts::new();
    accepts.insert(1, Arc::clone(&adapter));
    let bound = bind(&provider, Some(&accepts), &consumer)
        .expect("adapter")
        .expect("an adapter is returned");
    assert_eq!(
        bound(json!({"body": 1})),
        json!({"variant": "viewer", "view": 1})
    );
    assert_eq!(golden["v2BindAdapter"], json!("same"));
}

// ── it("compares opaque members by name@version and function members structurally") ─

#[test]
fn compares_opaque_members_by_id_and_function_members_structurally() {
    let golden = golden();
    let mut members = IndexMap::new();
    members.insert(
        "harness".to_owned(),
        LocalMember::Opaque(opaque("h@1").expect("opaque")),
    );
    members.insert(
        "onChange".to_owned(),
        LocalMember::Method(MethodSpec::args(json!({
            "type": "function",
            "parameters": [{"type": "string"}],
            "returnType": {"type": "void"}
        }))),
    );
    let a = local(
        "t.idx",
        LocalDef {
            version: 1,
            input: empty_object(),
            members: members.clone(),
        },
    )
    .expect("a");
    members.insert(
        "harness".to_owned(),
        LocalMember::Opaque(opaque("h@2").expect("opaque")),
    );
    let b = local(
        "t.idx",
        LocalDef {
            version: 1,
            input: empty_object(),
            members,
        },
    )
    .expect("b");

    let reason = coverage(&b.base.description, &a.base.description).expect("opaque ids differ");
    assert!(reason.contains("opaque"), "got {reason:?}");
    assert_eq!(
        reason,
        golden["opaque"]["coverage"].as_str().expect("golden")
    );

    // 函数成员结构化比较：参数逆变、返回协变。
    let wide = json!({"type": "function", "parameters": [{}], "returnType": {"type": "void"}});
    let narrow = json!({"type": "function", "parameters": [{"type": "string"}], "returnType": {"type": "void"}});
    assert!(
        fits(&wide, &narrow),
        "an unknown-argument function covers a string-argument one"
    );
    assert!(!fits(&narrow, &wide), "the reverse does not hold");
}

// ── 夹具里的常量与构造错误（gen_contract.mjs 的 constants/errors/token* 段） ────────

#[test]
fn constants_and_constructor_errors_match_the_typescript_reference() {
    let golden = golden();

    assert_eq!(LOCAL_VARIANT, "");
    let reserved: Vec<&str> = RESERVED_MEMBERS.to_vec();
    assert_eq!(
        serde_json::to_value(&reserved).expect("json"),
        golden["constants"]["RESERVED_MEMBERS"]
    );
    assert_eq!(
        placement_text(&host_named("server").expect("named")),
        golden["constants"]["placementTextNamed"]
            .as_str()
            .expect("golden")
    );
    assert_eq!(
        placement_text(&family()),
        golden["constants"]["placementTextFamily"]
            .as_str()
            .expect("golden")
    );
    assert!(placed_on(&host_named("server").expect("named"), "server"));
    assert_eq!(
        golden["constants"]["placedOnFamilyExact"],
        json!(placed_on(&family(), "session:")),
        "the bare prefix is not a host name"
    );
    assert_eq!(
        golden["constants"]["placedOnFamilyLonger"],
        json!(placed_on(&family(), "session:S1")),
        "prefix + anything non-empty is on the family"
    );
    assert!(placed_on(&family(), "session:S1:x"));

    // resolveHost：family 从 input 取名字段。
    assert_eq!(
        resolve_host("t", &family(), &json!({"session": "S1"})).expect("host"),
        golden["resolveHostOk"].as_str().expect("golden")
    );
    let error = resolve_host("t", &family(), &json!({"session": 1})).expect_err("no host");
    assert_eq!(error.code, ChordErrorCode::Invalid);
    assert_eq!(error.reason.as_deref(), Some("placement"));
    // TS 的 `error.message` 带 `code/reason: ` 前缀（Rust 的 `message` 是裸文本）。
    assert_eq!(
        error.full_message(),
        golden["resolveHostError"]["message"]
            .as_str()
            .expect("golden")
    );

    // 构造器错误逐字（TS 抛 TypeError；Rust 映射成 invalid/contract）。
    let errors = &golden["errors"];
    let check = |error: ChordError, key: &str| {
        assert_eq!(error.code, ChordErrorCode::Invalid, "{key}");
        assert_eq!(error.reason.as_deref(), Some("contract"), "{key}");
        assert_eq!(
            error.message,
            errors[key]["message"].as_str().expect("message"),
            "{key}"
        );
    };
    check(
        local(
            "t",
            LocalDef {
                version: 1,
                input: empty_object(),
                members: IndexMap::from([("status".to_owned(), LocalMember::Method(method()))]),
            },
        )
        .expect_err("reserved"),
        "reserved",
    );
    check(
        local(
            "",
            LocalDef {
                version: 1,
                input: empty_object(),
                members: IndexMap::new(),
            },
        )
        .expect_err("empty name"),
        "emptyName",
    );
    check(
        local(
            "t",
            LocalDef {
                version: 0,
                input: empty_object(),
                members: IndexMap::new(),
            },
        )
        .expect_err("bad version"),
        "badVersion",
    );
    check(
        remote(
            "t",
            RemoteDef {
                version: 1,
                input: empty_object(),
                host: host_named("server").expect("named"),
                variants: IndexMap::new(),
            },
        )
        .expect_err("no variants"),
        "noVariants",
    );
    // 同一个变体里同名成员既是 state 又是 method。
    let mut variant = VariantDef::new().state("x", json!({"type": "string"}));
    variant
        .methods
        .insert("x".to_owned(), LocalMember::Method(method()));
    check(
        remote(
            "t",
            RemoteDef {
                version: 1,
                input: empty_object(),
                host: host_named("server").expect("named"),
                variants: IndexMap::from([("a".to_owned(), variant)]),
            },
        )
        .expect_err("both kinds"),
        "bothKinds",
    );
    // `methods.x` 不是 `method()`（TS 里把一个普通 schema 塞进 methods）。
    let mut variant = VariantDef::new();
    variant.methods.insert(
        "x".to_owned(),
        LocalMember::State(json!({"type": "string"})),
    );
    check(
        remote(
            "t",
            RemoteDef {
                version: 1,
                input: empty_object(),
                host: host_named("server").expect("named"),
                variants: IndexMap::from([("v".to_owned(), variant)]),
            },
        )
        .expect_err("not a method"),
        "notMethod",
    );
    check(opaque("x").expect_err("bad opaque"), "opaqueBad");
    check(host_named("").expect_err("empty host"), "hostNamedEmpty");
    check(
        host_family("", "session").expect_err("no prefix"),
        "hostFamilyPrefix",
    );
    check(
        host_family("session:", "").expect_err("no key"),
        "hostFamilyKey",
    );

    // `contractFromDescription`：只有 `remote != host.is_some()` 才报 malformed token
    // （空名字是**接受**的，见 tokenErrors.emptyName = null）。
    let mut description = chat_v1().base.description.clone();
    description.name = String::new();
    description.host = Some(host_named("server").expect("named"));
    assert!(
        contract_from_description(description).is_ok(),
        "an empty name is accepted"
    );
    let mut remote_without_host = chat_v1().base.description.clone();
    remote_without_host.host = None;
    let error = contract_from_description(remote_without_host).expect_err("remote without host");
    assert_eq!(error.code, ChordErrorCode::Invalid);
    assert_eq!(error.reason.as_deref(), Some("token"));
    assert_eq!(error.message, "malformed contract token");

    let mut local_with_host = local(
        "t.local",
        LocalDef {
            version: 1,
            input: empty_object(),
            members: IndexMap::new(),
        },
    )
    .expect("local")
    .base
    .description
    .clone();
    local_with_host.host = Some(host_named("server").expect("named"));
    assert!(contract_from_description(local_with_host).is_err());

    // TS 的 token 只有 name/scope/version/input/host/variants。
    let token = chat_v1().base.description.to_json();
    assert_eq!(
        token
            .as_object()
            .expect("object")
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        vec![
            "name".to_owned(),
            "scope".to_owned(),
            "version".to_owned(),
            "input".to_owned(),
            "host".to_owned(),
            "variants".to_owned()
        ],
        "description key order matches the TS builder"
    );
    assert_eq!(token["name"], golden["token"]["name"]);
    assert_eq!(token["scope"], golden["token"]["scope"]);
    assert_eq!(token["version"], golden["token"]["version"]);

    // JSON 入口：与 `Description::from_json` 等价。
    let round_trip =
        contract_from_description_json(&golden["chatV1"]["description"]).expect("token");
    assert_eq!(round_trip.fingerprint(), chat_v1().base.fingerprint);
    assert!(round_trip.is_remote());
    assert_eq!(round_trip.scope(), DescriptionScope::Remote);
    let bad = contract_from_description_json(&json!({"name": "t", "scope": "weird", "version": 1}))
        .expect_err("bad scope");
    assert_eq!(bad.message, "malformed contract token");
}

// ── errors.ts 的 `runAll`（契约清理路径用）与 `Description` 的成员访问 ─────────────

#[test]
fn run_all_reports_one_error_or_an_aggregate() {
    fn boxed(
        step: impl FnOnce() -> Result<(), Box<dyn std::error::Error + Send + Sync>> + Send + 'static,
    ) -> Box<dyn FnOnce() -> Result<(), Box<dyn std::error::Error + Send + Sync>>> {
        Box::new(step)
    }

    let single = vec![
        boxed(|| Ok(())),
        boxed(|| {
            Err(Box::new(invalid("cleanup", "one")) as Box<dyn std::error::Error + Send + Sync>)
        }),
    ];
    let error = kiana_chord::errors::run_all_sync(single, "cleanup failed").expect_err("one error");
    assert_eq!(error.to_string(), "invalid/cleanup: one");

    let several = vec![
        boxed(|| {
            Err(Box::new(invalid("cleanup", "one")) as Box<dyn std::error::Error + Send + Sync>)
        }),
        boxed(|| {
            Err(Box::new(invalid("cleanup", "two")) as Box<dyn std::error::Error + Send + Sync>)
        }),
    ];
    let error =
        kiana_chord::errors::run_all_sync(several, "cleanup failed").expect_err("two errors");
    let aggregate = error
        .downcast_ref::<ChordAggregateError>()
        .expect("AggregateError");
    assert_eq!(aggregate.message, "cleanup failed");
    assert_eq!(aggregate.errors.len(), 2);
    assert_eq!(aggregate.to_string(), "cleanup failed");
}

#[test]
fn description_member_accessors_follow_the_kind() {
    let chat = chat_v1();
    let participant = &chat.base.description.variants["participant"];
    let view_member = &participant["view"];
    assert_eq!(view_member.kind(), kiana_chord::contract::MemberKind::State);
    // 活 schema 带 `~kind` 标记（TS 里是不可枚举属性，`assert.deepEqual` 看不见）；比较时剥除。
    assert_eq!(
        view_member
            .schema()
            .map(kiana_chord::schema::strip_live_markers),
        Some(view())
    );
    assert!(view_member.args().is_none());
    let send = &participant["send"];
    assert_eq!(send.kind(), kiana_chord::contract::MemberKind::Method);
    assert_eq!(
        send.args().map(kiana_chord::schema::strip_live_markers),
        Some(send_args())
    );
    assert_eq!(
        send.result().map(kiana_chord::schema::strip_live_markers),
        Some(send_result())
    );
    assert!(send.schema().is_none());
    assert_eq!(
        chat.base
            .description
            .member("participant", "send")
            .map(MemberDescription::kind),
        Some(kiana_chord::contract::MemberKind::Method)
    );
    assert_eq!(
        chat.base.description.member("viewer", "send"),
        None,
        "members are per variant"
    );
    let _: &Description = &chat.base.description;
}

// ── 审查回归：契约成员名的原型链语义、JSON schema 不是活 schema、键序 ─────────────────

/// 关键回归：`remote()` 的 `if (key in members)` 是普通 `in`，原型链上的名字（`toString`、
/// `constructor`、`valueOf`……）即使没有同名 state 成员也算"已存在"。
#[test]
fn remote_rejects_methods_named_after_object_prototype() {
    for name in [
        "toString",
        "constructor",
        "valueOf",
        "hasOwnProperty",
        "isPrototypeOf",
        "propertyIsEnumerable",
    ] {
        let mut variants = IndexMap::new();
        variants.insert(
            "a".to_owned(),
            VariantDef::new().method(name, MethodSpec::new()),
        );
        let error = remote(
            "t",
            RemoteDef {
                version: 1,
                input: json!({"type": "object", "properties": {}}),
                host: host_named("h").expect("host"),
                variants,
            },
        )
        .expect_err(name);
        assert_eq!(error.code, ChordErrorCode::Invalid);
        assert_eq!(error.reason.as_deref(), Some("contract"));
        assert_eq!(
            error.message,
            format!("member {name} is both state and method in t.a")
        );
    }
}

/// `__proto__` 也是 `Object.prototype` 上的访问器：方法名命中 `in`；state 名的赋值则设置原型
/// （不产生自有键，`Object.keys` 看不见）。
#[test]
fn proto_member_names_follow_javascript_assignment_semantics() {
    let mut variants = IndexMap::new();
    variants.insert(
        "a".to_owned(),
        VariantDef::new().method("__proto__", MethodSpec::new()),
    );
    let error = remote(
        "t",
        RemoteDef {
            version: 1,
            input: json!({"type": "object", "properties": {}}),
            host: host_named("h").expect("host"),
            variants,
        },
    )
    .expect_err("__proto__ method");
    assert_eq!(
        error.message,
        "member __proto__ is both state and method in t.a"
    );

    let mut variants = IndexMap::new();
    variants.insert(
        "a".to_owned(),
        VariantDef::new()
            .state("__proto__", view())
            .state("toString", view()),
    );
    let contract = remote(
        "t",
        RemoteDef {
            version: 1,
            input: json!({"type": "object", "properties": {}}),
            host: host_named("h").expect("host"),
            variants,
        },
    )
    .expect("state proto names are allowed");
    let members = &contract.base.description.variants["a"];
    assert_eq!(js_object_order(members), vec!["toString".to_owned()]);
}

/// `local()` 没有 `in` 检查：`toString` 成为普通自有键；`__proto__` 仍然被 setter 吃掉。
#[test]
fn local_members_keep_prototype_names_but_drop_proto() {
    let mut members: IndexMap<String, LocalMember> = IndexMap::new();
    members.insert("toString".to_owned(), MethodSpec::new().into());
    members.insert("__proto__".to_owned(), MethodSpec::new().into());
    let contract = local(
        "t",
        LocalDef {
            version: 1,
            input: json!({"type": "object", "properties": {}}),
            members,
        },
    )
    .expect("local");
    assert_eq!(
        js_object_order(&contract.base.description.variants[LOCAL_VARIANT]),
        vec!["toString".to_owned()]
    );
}

/// 关键回归：`Default`/`Clean` 按 `~kind` 分派。`remote()` 收到的声明 schema 是活 schema
/// （等价于 TypeBox 构造物）；`remote_from_json()` 的 schema 来自 JSON，没有 `~kind`。
#[test]
fn live_contracts_default_but_json_contracts_do_not() {
    let input = json!({
        "type": "object",
        "required": ["session", "conversation"],
        "properties": {
            "session": {"type": "string"},
            "conversation": {"type": "number", "default": 0}
        },
        "additionalProperties": false
    });
    let mut variants = IndexMap::new();
    variants.insert("a".to_owned(), VariantDef::new());
    let live = remote(
        "t",
        RemoteDef {
            version: 1,
            input: input.clone(),
            host: host_named("h").expect("host"),
            variants,
        },
    )
    .expect("live contract");
    assert_eq!(
        normalize_input(
            &AnyContract::Remote(live.clone()),
            Some(&json!({"session": "s"}))
        )
        .expect("live normalize"),
        json!({"session": "s", "conversation": 0})
    );
    assert_ne!(
        strip_live_markers(&live.base.input),
        live.base.input,
        "the declared schema carries `~kind` markers internally"
    );

    assert_eq!(
        normalize(
            &live.base.input,
            Some(&json!({"session": "s", "extra": 1})),
            "input"
        )
        .expect("live normalize cleans and defaults"),
        json!({"session": "s", "conversation": 0})
    );

    let wire = remote_from_json(
        "t",
        &json!({
            "version": 1,
            "input": input,
            "host": {"name": "h"},
            "variants": {"a": {}}
        }),
    )
    .expect("json contract");
    // 审查给出的原样文本：JSON 传输后的 schema 既不补默认值也不清洗，三条错误一次报出。
    let error = normalize(
        &wire.base.input,
        Some(&json!({"session": "s", "extra": 1})),
        "input",
    )
    .expect_err("no defaults");
    assert_eq!(
        error.message,
        "input: / must have required properties conversation; /extra schema is false; / must not have additional properties"
    );
    assert_eq!(
        error.full_message(),
        "invalid/input: input: / must have required properties conversation; /extra schema is false; / must not have additional properties"
    );
    // `normalizeInput` 走 `contract.input()`，文本带契约名。
    let error = normalize_input(&AnyContract::Remote(wire), Some(&json!({"session": "s"})))
        .expect_err("no defaults");
    assert_eq!(
        error.message,
        "t input: / must have required properties conversation"
    );
}

/// 关键回归：描述序列化剥掉 Rust 侧的 `~kind` 标记（TS 里它不可枚举，`JSON.stringify` 看不见）。
#[test]
fn description_json_never_contains_kind_markers() {
    let chat = chat_v1();
    assert!(
        chat.def.input.as_object().unwrap().contains_key("~kind"),
        "the live def keeps markers"
    );
    let json = chat.base.description.to_json();
    assert!(!serde_json::to_string(&json).unwrap().contains("~kind"));
    assert_eq!(
        json["input"],
        strip_live_markers(&chat.base.input),
        "description.input is the JSON view of the declared schema"
    );
}

/// 关键回归（minor）：schema 对象的键按 JS 自有键序排列（整数索引键升序在前），
/// 指纹由 `canonicalJson`（键排序）计算，因此键序差异不影响指纹。
#[test]
fn contract_input_keys_follow_javascript_own_key_order() {
    let input = json!({
        "type": "object",
        "required": ["1", "2", "b", "a"],
        "properties": {
            "b": {"type": "string"},
            "a": {"type": "string"},
            "2": {"type": "string"},
            "1": {"type": "string"}
        }
    });
    let mut variants = IndexMap::new();
    variants.insert(
        "a".to_owned(),
        VariantDef::new().method("m", MethodSpec::new()),
    );
    let contract = remote(
        "t",
        RemoteDef {
            version: 1,
            input,
            host: host_named("h").expect("host"),
            variants,
        },
    )
    .expect("contract");
    let description = contract.base.description.to_json();
    let property_keys: Vec<&String> = description["input"]["properties"]
        .as_object()
        .expect("properties")
        .keys()
        .collect();
    assert_eq!(property_keys, vec!["1", "2", "b", "a"]);
    assert_eq!(contract.base.fingerprint(), "a375cb6ad1ef47ca");
}

/// 标记是声明边界的幂等操作：把一个契约的 `input` 原样再声明一次，标记集合不应变化
/// （TS 里重复交给 TypeBox 也仍是同一个 `~kind`）。
#[test]
fn mark_live_schema_is_idempotent() {
    let schema = mark_live_schema(&json!({"type": "object", "properties": {}}));
    assert_eq!(mark_live_schema(&schema), schema);
}

// ── reviewer 回归：#12 额外字段必须计入指纹 ────────────────────────────────────

#[test]
fn contract_from_description_keeps_extra_fields_in_the_fingerprint() {
    let golden = golden();
    let base = golden["chatV1"]["description"].clone();
    // TS：`fingerprint = hash64(canonicalJson(description))`，description 原样（含额外字段）。
    assert_eq!(
        contract_from_description_json(&base)
            .expect("base token")
            .fingerprint(),
        "e8ee3f28594f8426"
    );

    // 顶层额外字段。
    let mut top = base.clone();
    top["extra"] = json!(1);
    assert_eq!(
        contract_from_description_json(&top)
            .expect("top-level extra")
            .fingerprint(),
        "3239d533a17d8e96"
    );

    // 成员上的额外字段。
    let mut member = base.clone();
    member["variants"]["participant"]["send"]["note"] = json!("x");
    assert_eq!(
        contract_from_description_json(&member)
            .expect("member extra")
            .fingerprint(),
        "449db8ced804b3fb"
    );

    // 成员 schema 里的额外字段（Value 原样保留）。
    let mut schema_extra = base;
    schema_extra["variants"]["participant"]["send"]["args"]["extra"] = json!(true);
    assert_eq!(
        contract_from_description_json(&schema_extra)
            .expect("schema extra")
            .fingerprint(),
        "078a75c449232a6d"
    );
}
