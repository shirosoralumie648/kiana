//! `packages/chord/test/types.test.ts` 的 Rust 端口。
//!
//! TS 的这条测试是**纯类型级**的（`expectTypeOf` + `// @ts-expect-error`，由 `tsc` 检查，
//! 运行期只断言 `expectTypeOf(check).toBeFunction()`）。Rust 的对应物是编译期 trait 约束，
//! 因此本文件把它拆成两半：
//!
//! - **编译期**：下面的 `assert_ref_surface` 用真实的 `types`/`declare` 类型写出 TS 里被
//!   `expectTypeOf` 断言的形状（state 成员是 `Readable<T>`、方法可调用、`Ctx::get` 返回
//!   `BoxFuture<Result<Box<dyn Ref>, _>>`、资源只有 `Subscribable` 才能 `expose`）。写错了就
//!   编译失败——这正是 `tsc` 在 TS 侧的作用。
//! - **运行期**：`reference_types_compile_and_keep_their_shape` 用真实契约构造引用/声明所需的
//!   元数据，确认类型面与实际值一致（`Shape.members`、`RefStatus` 字符串、`GetOptions` 默认值）。
//!
//! TS 的 `// @ts-expect-error void doc.edit`（别的 variant 的成员不可见）在 Rust 里是
//! "`variants` 按 variant 名索引"：`description.variants["reader"]` 没有 `"edit"` 键。

use indexmap::IndexMap;
use kiana_chord::contract::{
    AnyContract, Description, MethodSpec, RemoteDef, VariantDef, host_named, remote,
};
use kiana_chord::declare::Subscribable;
use kiana_chord::types::{
    Ctx, GetOptions, Readable, Ref, RefStatus, Shape, StateEvent, StateEventKind, StateSource,
    ValueListener,
};

fn doc() -> AnyContract {
    let mut variants = IndexMap::new();
    variants.insert(
        "reader".to_owned(),
        VariantDef::new().state("text", serde_json::json!({"type": "string"})),
    );
    variants.insert(
        "editor".to_owned(),
        VariantDef::new()
            .state("text", serde_json::json!({"type": "string"}))
            .method(
                "edit",
                MethodSpec::args(serde_json::json!({"type": "string"})),
            ),
    );
    remote(
        "t.e8-types-doc",
        RemoteDef {
            version: 1,
            input: serde_json::json!({"type": "object", "properties": {}}),
            host: host_named("server").expect("named"),
            variants,
        },
    )
    .expect("doc")
    .into()
}

fn single() -> AnyContract {
    let mut variants = IndexMap::new();
    variants.insert(
        "main".to_owned(),
        VariantDef::new()
            .state("n", serde_json::json!({"type": "number"}))
            .method("bump", MethodSpec::new()),
    );
    remote(
        "t.e8-types-single",
        RemoteDef {
            version: 1,
            input: serde_json::json!({"type": "object", "properties": {}}),
            host: host_named("server").expect("named"),
            variants,
        },
    )
    .expect("single")
    .into()
}

/// TS：`const check = async (host: Host, ctx: Ctx) => { … }`。
///
/// 这里用泛型把 TS 里被 `expectTypeOf` 断言的每个形状写出来：编译器检查它们**存在且类型正确**。
/// 函数体永远不跑（没有实现），只用于编译期断言。
#[allow(dead_code)]
fn assert_ref_surface(reference: &dyn Ref, ctx: &dyn Ctx, contract: &AnyContract) {
    // `expectTypeOf(doc.text).toEqualTypeOf<Readable<string>>()`：状态成员是 `Readable<T>`。
    let text: Option<Box<dyn Readable<serde_json::Value>>> = reference.state_member("text");
    drop(text);

    // `if (doc.variant === "editor") expectTypeOf(doc.edit).toBeFunction()`：方法可调用，
    // 返回一个 future。`call` 的名字/参数/返回值形状必须与 `Ref` 一致。
    let _call: kiana_telemetry::index::BoxFuture<
        '_,
        Result<Option<serde_json::Value>, kiana_chord::errors::ChordError>,
    > = reference.call("edit", Some(serde_json::json!("hi")), ctx);

    // `expectTypeOf(host.get(Single, {})).toEqualTypeOf<Promise<Ref<typeof Single>>>()`：
    // `Ctx::get` 返回就绪的引用（`Box<dyn Ref>`），失败是 `ChordError`。
    let _get: kiana_telemetry::index::BoxFuture<
        '_,
        Result<Box<dyn Ref>, kiana_chord::errors::ChordError>,
    > = ctx.get(contract, serde_json::json!({}), GetOptions::default());

    // `RefBase` 的只读面：contract / status / ready / variant / shape / error。
    let _contract: &AnyContract = reference.contract();
    let _status: RefStatus = reference.status();
    let _ready = reference.ready();
    let _variant: Option<String> = reference.variant();
    let _shape: Option<Shape> = reference.shape();
    let _error: Option<kiana_chord::errors::ChordError> = reference.error();
}

/// `Subscribable<E>`：`self.on(…)` 的源；`subscribe` 返回取消函数。
#[allow(dead_code)]
fn assert_subscribable_surface<S: Subscribable<u64>>(source: &S) {
    let _unsubscribe: Box<dyn FnOnce() + Send + 'static> =
        source.subscribe(Box::new(|_event: u64| {}));
}

/// `StateSource<T> = Readable<T>`（TS 的类型别名）：`value` / `subscribe` / `events`。
#[allow(dead_code)]
fn assert_state_source_is_readable<S: StateSource<u64>>(source: &S) {
    let _: u64 = source.value();
    let _sub = source.subscribe(Box::new(|_value: &u64| {}) as ValueListener<u64>);
    let _events = source.events(Box::new(|event: StateEvent<u64>| {
        let _ = event.kind();
    }));
}

#[test]
fn reference_types_compile_and_keep_their_shape() {
    // TS 的 `expectTypeOf(check).toBeFunction()`：类型面存在（上面的函数能编译）。
    let _ = assert_ref_surface;
    let _ = assert_subscribable_surface::<NoSource>;
    let _ = assert_state_source_is_readable::<NoSource>;

    let contract = doc();
    assert!(contract.is_remote());
    assert_eq!(contract.name(), "t.e8-types-doc");

    // `doc.text` 在 reader 与 editor 两个 variant 里都有；`edit` 只在 editor。
    let description = contract.description();
    assert_eq!(
        description.member("reader", "text").map(|m| m.kind()),
        Some(kiana_chord::contract::MemberKind::State)
    );
    assert_eq!(
        description.member("editor", "edit").map(|m| m.kind()),
        Some(kiana_chord::contract::MemberKind::Method)
    );
    assert_eq!(
        description.member("reader", "edit"),
        None,
        "`edit` exists only in the editor variant"
    );

    // `RefStatus` 的字符串是稳定契约（线上/序列化表示），改动需同步测试。
    assert_eq!(RefStatus::Loading.as_str(), "loading");
    assert_eq!(RefStatus::Live.as_str(), "live");
    assert_eq!(RefStatus::Released.as_str(), "released");
    assert_eq!(RefStatus::Gone.as_str(), "gone");

    // `GetOptions`：`options?.state ?? true`。
    assert!(GetOptions::default().wants_state());
    assert!(!GetOptions::default().with_state(false).wants_state());
    assert!(GetOptions::new().principal().is_none());
    assert_eq!(
        GetOptions::new()
            .with_principal(serde_json::json!({"user": "u"}))
            .principal(),
        Some(&serde_json::json!({"user": "u"}))
    );

    // `Shape`：`{variant?, members, contract}`；variant 缺省不出现。
    let single = single();
    let shape = Shape {
        variant: Some("main".to_owned()),
        members: vec!["n".to_owned(), "bump".to_owned()],
        contract: single.fingerprint().to_owned(),
    };
    let json = serde_json::to_value(&shape).expect("shape json");
    assert_eq!(
        json,
        serde_json::json!({"variant": "main", "members": ["n", "bump"], "contract": single.fingerprint()})
    );
    let local_shape = Shape {
        variant: None,
        members: vec![],
        contract: "x".to_owned(),
    };
    assert_eq!(
        serde_json::to_value(&local_shape).expect("shape json"),
        serde_json::json!({"members": [], "contract": "x"})
    );

    // `StateEvent<T>`：`snap`/`ops`/`reset` 的判别与取值。
    let snap: StateEvent<u32> = StateEvent::Snap { value: 1 };
    assert_eq!(snap.kind(), StateEventKind::Snap);
    assert_eq!(snap.value(), &1);
    let ops: StateEvent<u32> = StateEvent::Ops {
        ops: vec![kiana_chord::delta::Op::Replace(serde_json::json!(2))],
        value: 2,
    };
    assert_eq!(ops.kind(), StateEventKind::Ops);
    assert_eq!(ops.value(), &2);
    assert_eq!(ops.into_value(), 2);
    let reset: StateEvent<u32> = StateEvent::Reset { value: 0 };
    assert_eq!(reset.kind(), StateEventKind::Reset);

    // `Description` 的成员表是按 variant 索引的（TS 的变体联合）。
    let _: &Description = description;
}

/// 一个什么也不做的 `Readable<u64>` + `Subscribable<u64>`，只用于让编译期形状检查具象化。
#[derive(Debug)]
struct NoSource;

impl Readable<u64> for NoSource {
    fn value(&self) -> u64 {
        0
    }

    fn subscribe(&self, listener: ValueListener<u64>) -> kiana_chord::types::Unsubscribe {
        let mut listener = listener;
        listener(&0);
        Box::new(|| {})
    }

    fn events(
        &self,
        listener: kiana_chord::types::EventListener<u64>,
    ) -> kiana_chord::types::Unsubscribe {
        let mut listener = listener;
        listener(StateEvent::Snap { value: 0 });
        Box::new(|| {})
    }
}

impl Subscribable<u64> for NoSource {
    fn subscribe(
        &self,
        listener: Box<dyn Fn(u64) + Send + 'static>,
    ) -> Box<dyn FnOnce() + Send + 'static> {
        listener(0);
        Box::new(|| {})
    }
}
