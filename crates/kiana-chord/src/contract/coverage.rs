//! `contract/coverage.ts`：`coverage`、`bind`、`takesNoArgs` 与 `Accepts`/`Adapter`。
//!
//! 这三个函数决定跨版本绑定是否允许，因此**每条原因字符串都逐字**等于 TS（spec/chord-rest.md
//! §3.3 与 chord-core.md §2.2.9-2.2.11）。
//!
//! 语义（来自 TS 注释）：
//! - 方法参数**逆变**：provider 必须接受 consumer 能发的一切 → `fits(consumer.args, provider.args)`。
//! - 结果**协变**：`fits(provider.result, consumer.result)`。
//! - consumer 不要求结果（`result === undefined`）时接受任何 provider 结果。
//! - state 值 `fits(provider.schema, consumer.schema)`。
//! - consumer 的成员必须是 provider 的子集；provider 多出来的成员被隐藏。
//! - `coverage` **不看** `version`（调用方先比 version）。
//!
//! ## 这个文件在契约生命周期中的位置
//!
//! 契约对象由 `contract` 模块构造（[`crate::contract::remote`] / [`crate::contract::local`]）并
//! 生成指纹；本文件只回答两个问题：
//!
//! 1. [`coverage`]：同一版本下，provider 的实现是否仍覆盖消费者手里的描述（纯判断，无 IO）；
//! 2. [`bind`]：拿到 provider token、可选的 [`Accepts`] 和 consumer token 后，调用方应该直接用
//!    provider，还是改用某个旧版本适配器（[`Adapter`]）。
//!
//! 这些项经 `crate::contract` 再导出（`bind`/`coverage` 以及 [`Accepts`]/[`Adapter`] 还经
//! `crate::index`）；`tests/contract.rs` 用 `tests/fixtures/contract_golden.json` 逐字锁定原因串
//! 与错误消息。
//!
//! ## [`bind`] 的执行流程
//!
//! ```text
//! fingerprint 相同 ────────────────► Ok(None)（同一份描述，直接用 provider）
//! placement 不同 ──────────────────► ContractMismatch（即使提供 accepts 也不桥接）
//! version 相同 ──► coverage 通过 ──► Ok(None)
//!              └─ 失败 ───────────► ContractMismatch（"forgotten version bump?"）
//! version 不同 ──► accepts 命中 ──► Ok(Some(adapter))（不跑 coverage）
//!              └─ 未命中 ─────────► ContractMismatch（no accepts adapter）
//! ```
//!
//! ## 注意事项
//!
//! - 判定顺序是协议的一部分：多个问题同时存在时只有先命中的那条进入 reason/message，测试逐字
//!   断言这些字符串，不能调整检查顺序。
//! - 描述的 `variants`/成员顺序是 JS 对象键序（构造时经
//!   [`crate::contract::describe::js_object_reorder`]），`coverage`/`takes_no_args` 的短路点
//!   取决于这个顺序。
//! - 子类型判断委托 `crate::schema::fits`；它会先剥掉活 schema 的 `~kind` 标记，因此 `remote()`
//!   内存中的描述与从 JSON 读回的描述得到相同结论。

use indexmap::IndexMap;
use serde_json::Value;

use crate::contract::describe::{Description, MemberDescription};
use crate::contract::{AnyContract, Placement};
use crate::errors::{ChordError, ChordErrorCode};
use crate::schema::{canonical_json, fits};

/// 把 provider 当前实现映射到旧版本消费者期望的视图（TS: `Adapter`）。
///
/// TS 里是 `(current: Record<string, unknown>) => Record<string, unknown>`；Rust 侧同样接受并返回
/// JSON 对象。适配器是用户代码，因此允许装箱闭包。
///
/// `Arc` 让同一个适配器既能放进 [`Accepts`] 又能被 [`bind`] 返回给调用方；`Send + Sync` 是
/// 跨线程共享的约束——适配器内部若有可变状态，需要自行同步。
///
/// 输入是 provider 当前的实现记录（通常是 JSON 对象），输出必须是旧版本消费者期望的形状。
/// 签名里没有 `Result`，适配器无法通过返回值报告失败；[`bind`] 只负责挑选，不负责调用它，调用
/// 时机与失败处理由使用返回值的一方决定。
pub type Adapter = std::sync::Arc<dyn Fn(Value) -> Value + Send + Sync>;

/// consumer version → 适配器（TS: `Accepts = Readonly<Record<number, Adapter>>`）。
///
/// JS 的数字键在对象里按数值升序迭代；`BTreeMap` 保持同样的顺序（`coverage`/`bind` 只做查找，
/// 但测试与报告会遍历它）。
///
/// 键是 **consumer** 的 contract version（不是 provider 的）；[`bind`] 只按 `consumer.version()`
/// 查找。同一版本重复插入时后写者胜（`BTreeMap` 的收集语义）。
///
/// 严格说 JS 只把 `0..2^32-2` 的整数样式键提前并按数值排序，更大的版本号在 JS 对象里会退化为
/// 字符串键的插入序；`BTreeMap` 对所有 `i64` 键都按数值序。查找语义不受影响，遍历时需注意这
/// 一理论差异。
pub type Accepts = std::collections::BTreeMap<i64, Adapter>;

/// `samePlacement(a, b)`：`canonicalJson(a.host ?? null) === canonicalJson(b.host ?? null)`。
///
/// 只比较 `host`，不比较 `name`/`scope`/版本；两侧都没有 host（local）时等价于比较 `null`。
/// `canonical_json` 会排序对象键，所以 placement 字段的书写顺序不影响结果。
fn same_placement(a: &Description, b: &Description) -> bool {
    let left = a.host.as_ref().map(placement_json);
    let right = b.host.as_ref().map(placement_json);
    canonical_json(&left.unwrap_or(Value::Null)) == canonical_json(&right.unwrap_or(Value::Null))
}

/// placement 的 JSON（`{name}` / `{prefix,key}`）。
///
/// `Placement` 只含字符串字段，`serde_json::to_value` 不会失败；`expect` 是结构保证，不是运行期
/// 校验。
fn placement_json(placement: &Placement) -> Value {
    serde_json::to_value(placement).expect("placement is JSON")
}

/// `placementOf(description)`：`host === undefined ? "(local)" : placementText(host)`。
///
/// 结果会拼进 [`bind`] 的用户可见错误消息（如 `(local)`、`session:<session>`），属于 TS 逐字
/// 文案的一部分。
fn placement_of(description: &Description) -> String {
    match &description.host {
        None => "(local)".to_owned(),
        Some(placement) => placement.text(),
    }
}

/// `coverage(provider, consumer)`：`provider` 是否覆盖 `consumer`。
///
/// 返回 `None` 表示覆盖；否则是**逐字**的原因字符串。
///
/// 顺序敏感：`different type` → `host` → `input` → 按 `consumer.variants` 的 JS 键序遍历。
///
/// # 检查内容
///
/// - `input`：provider 必须接受 consumer 能发的一切（`fits(consumer.input, provider.input)`）。
/// - consumer 的每个 variant 必须在 provider 中存在；每个 consumer 成员按 kind 分别比较：方法的
///   参数（逆变）与结果（协变）、state 的 schema、opaque 的 id。
/// - provider 多出来的 variant/成员不参与比较，也不会报错（对消费者隐藏）。
///
/// # 边界
///
/// - 只返回**第一个**差异；它依赖 `consumer.variants` 与成员表的遍历顺序，不是差异清单。
/// - 不做归一化、不修改描述：schema 原样交给 `fits`。
/// - 不看 `version`（调用方先比较，[`bind`] 就是这么做的）。
/// - 由 [`crate::contract::local`] 构造的契约唯一 variant 名是空串，所以这类 local 契约的原因串
///   形如 `.m arguments`、`.harness opaque …`（见本文件测试）。经
///   `contract_from_description(_json)` 重建的 local 描述保留 token 里的任意 variant 名
///   （见 [`Description`] 的文档）；variant 名为 `v` 时原因串是 `v.m arguments`。
pub fn coverage(provider: &Description, consumer: &Description) -> Option<String> {
    // 身份层：name 与 scope 一起构成类型身份，任一不同就不是同一个类型。
    if provider.name != consumer.name || provider.scope != consumer.scope {
        return Some("different type".to_owned());
    }
    // 地点层：不同 host 上的同名类型视为不同类型；bind 不允许用适配器绕过这一条。
    if !same_placement(provider, consumer) {
        return Some("host".to_owned());
    }
    // input 是函数参数位置：consumer 会发出的输入，provider 必须全部接受（逆变）。
    if !fits(&consumer.input, &provider.input) {
        return Some("input".to_owned());
    }
    // JS: `for (const [kind, consumerMembers] of Object.entries(consumer.variants))`。
    for (kind, consumer_members) in &consumer.variants {
        let Some(provider_members) = provider.variants.get(kind) else {
            return Some(format!("variant {kind} missing"));
        };
        for (name, wanted) in consumer_members {
            let Some(offered) = provider_members.get(name) else {
                return Some(format!("{kind}.{name} missing or of another kind"));
            };
            if offered.kind() != wanted.kind() {
                return Some(format!("{kind}.{name} missing or of another kind"));
            }
            match (offered, wanted) {
                (
                    MemberDescription::Method {
                        args: offered_args,
                        result: offered_result,
                        ..
                    },
                    MemberDescription::Method {
                        args: wanted_args,
                        result: wanted_result,
                        ..
                    },
                ) => {
                    // 双方都有参数或都没有；不要求结果的消费者接受任何结果。
                    let offered_has_args = offered_args.is_some();
                    let wanted_has_args = wanted_args.is_some();
                    // args 的有无本身是签名的一部分：一方有、一方无属于不兼容。
                    if wanted_has_args != offered_has_args {
                        return Some(format!("{kind}.{name} arguments"));
                    }
                    if let (Some(wanted_args), Some(offered_args)) = (wanted_args, offered_args)
                        && !fits(wanted_args, offered_args)
                    {
                        return Some(format!("{kind}.{name} arguments"));
                    }
                    // 结果只对"消费者要求结果"的情况设限：consumer 没有 result 时不看 provider
                    // 的返回值，因此 provider 多返回什么都不影响覆盖。
                    if let Some(wanted_result) = wanted_result {
                        match offered_result {
                            None => return Some(format!("{kind}.{name} result")),
                            Some(offered_result) => {
                                if !fits(offered_result, wanted_result) {
                                    return Some(format!("{kind}.{name} result"));
                                }
                            }
                        }
                    }
                }
                (
                    MemberDescription::State {
                        schema: offered_schema,
                        ..
                    },
                    MemberDescription::State {
                        schema: wanted_schema,
                        ..
                    },
                ) => {
                    // state 是数据流方向：provider 产出的值必须满足 consumer 的 schema（协变）。
                    if !fits(offered_schema, wanted_schema) {
                        return Some(format!("{kind}.{name} state"));
                    }
                }
                (
                    MemberDescription::Opaque { id: offered_id, .. },
                    MemberDescription::Opaque { id: wanted_id, .. },
                ) if offered_id != wanted_id => {
                    // opaque 按 `name@version` 字符串判同一性，不做结构比较。
                    return Some(format!("{kind}.{name} opaque {offered_id} != {wanted_id}"));
                }
                // 种类已在上面的 `kind()` 比较里排除，不会到这儿。
                _ => {}
            }
        }
    }
    None
}

/// `bind(provider, accepts, consumer)`：consumer token 如何绑定到 provider——直接绑定，或通过
/// `accepts` 适配器。失败抛 `contract_mismatch`。
///
/// 三条消息（逐字）：
/// - `` `${provider.name} lives on ${placementOf(provider.description)}, the consumer expects ${placementOf(consumer.description)}` ``
/// - `` `${provider.name}@${provider.version} does not cover the consumer's description (${reason}); forgotten version bump?` ``
/// - `` `${provider.name}: provider version ${provider.version}, consumer version ${consumer.version}, no accepts adapter` ``
///
/// 移到了别的 host 的类型对消费者而言是另一个类型：**即使有 accepts 也不桥接**。
///
/// # 判定阶梯（短路）
///
/// 1. fingerprint 相同 → `Ok(None)`：canonical JSON 完全一致（version 也相同）。
/// 2. placement 不同 → `ContractMismatch`（文案见上）。
/// 3. version 相同 → 跑 [`coverage`]：通过则 `Ok(None)`，否则带 reason 的错误。
/// 4. version 不同 → 查 `accepts[consumer.version]`：命中返回克隆，否则报错。
///
/// 跨 version 分支不调用 [`coverage`]，也不重新比较 `name`/`scope`/input/成员——兼容性此时完全
/// 由适配器承担。
///
/// # 返回值
///
/// `Ok(None)` 表示直接使用 provider；`Ok(Some(adapter))` 表示调用方必须先把 provider 的当前
/// 实现（JSON 对象）交给适配器，再把它返回的 JSON 当作旧版本实现。[`bind`] 只做选择，不调用
/// 适配器。
pub fn bind(
    provider: &AnyContract,
    accepts: Option<&Accepts>,
    consumer: &AnyContract,
) -> Result<Option<Adapter>, ChordError> {
    if provider.fingerprint() == consumer.fingerprint() {
        return Ok(None);
    }
    if !same_placement(provider.description(), consumer.description()) {
        return Err(ChordError::new(
            ChordErrorCode::ContractMismatch,
            format!(
                "{} lives on {}, the consumer expects {}",
                provider.name(),
                placement_of(provider.description()),
                placement_of(consumer.description())
            ),
        ));
    }
    if provider.version() == consumer.version() {
        return match coverage(provider.description(), consumer.description()) {
            None => Ok(None),
            Some(reason) => Err(ChordError::new(
                ChordErrorCode::ContractMismatch,
                format!(
                    "{}@{} does not cover the consumer's description ({reason}); forgotten version bump?",
                    provider.name(),
                    provider.version()
                ),
            )),
        };
    }
    // 跨版本：只按 consumer 的版本找适配器；provider 的版本只出现在错误文案里。
    match accepts.and_then(|accepts| accepts.get(&consumer.version())) {
        Some(adapter) => Ok(Some(adapter.clone())),
        None => Err(ChordError::new(
            ChordErrorCode::ContractMismatch,
            format!(
                "{}: provider version {}, consumer version {}, no accepts adapter",
                provider.name(),
                provider.version(),
                consumer.version()
            ),
        )),
    }
}

/// `contract` 的方法成员 `name` 是否不接收参数（调用形式 `ref.name(ctx)`）。
///
/// 遍历 `contract.description.variants` 的**值顺序**（JS 的 `Object.values`），第一个
/// `kind === "method"` 的成员决定答案：返回 `member.args === undefined`。找不到返回 `false`
/// （即按"有参数"处理：调用形式 `ref.name(args, ctx?)`）。
///
/// # 边界
///
/// - 同一个名字可能出现在多个 variant 中：按 [`Description::variant_members`] 的顺序取第一个
///   `kind == method` 的成员，后续 variant 里的签名不参与判断。
/// - 名字只以 state/opaque 出现、或完全不存在 → `false`（按"有参数"处理）。
pub fn takes_no_args(contract: &AnyContract, name: &str) -> bool {
    for members in contract.description().variant_members() {
        if let Some(member) = members.get(name)
            && member.kind() == crate::contract::MemberKind::Method
        {
            return member.args().is_none();
        }
    }
    false
}

/// 便捷：把 `(version, adapter)` 对收集成 [`Accepts`]。
///
/// 同一 version 出现多次时后者覆盖前者；结果的迭代顺序是版本升序（[`Accepts`] 是 `BTreeMap`）。
pub fn accepts(entries: impl IntoIterator<Item = (i64, Adapter)>) -> Accepts {
    entries
        .into_iter()
        .collect::<IndexMap<_, _>>()
        .into_iter()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::{
        LOCAL_VARIANT, LocalDef, LocalMember, MethodSpec, Placement, RemoteDef, VariantDef, local,
        method, remote,
    };
    use serde_json::json;

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

    fn send(result: Value) -> MethodSpec {
        MethodSpec::with(
            Some(
                json!({"type": "object", "required": ["text"], "properties": {"text": {"type": "string"}}}),
            ),
            Some(result),
        )
    }

    fn chat(variants: IndexMap<String, VariantDef>) -> crate::contract::RemoteContract {
        remote(
            "t.chat",
            RemoteDef {
                version: 1,
                input: chat_input(),
                host: Placement::family("session:", "session"),
                variants,
            },
        )
        .unwrap()
    }

    fn base_variants() -> IndexMap<String, VariantDef> {
        let mut variants = IndexMap::new();
        variants.insert("viewer".to_owned(), VariantDef::new().state("view", view()));
        variants.insert(
            "participant".to_owned(),
            VariantDef::new()
                .state("view", view())
                .method("send", send(json!({"type": "object", "required": ["id"], "properties": {"id": {"type": "number"}}}))),
        );
        variants
    }

    #[test]
    fn additive_changes_are_covered() {
        let provider = chat(base_variants());
        let mut variants = base_variants();
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
        let additive = chat(variants);
        assert_eq!(
            coverage(&additive.base.description, &provider.base.description),
            None
        );
        let provider_contract: AnyContract = additive.into();
        let consumer: AnyContract = provider.into();
        assert!(bind(&provider_contract, None, &consumer).unwrap().is_none());
    }

    #[test]
    fn required_additions_break_coverage_with_verbatim_reasons() {
        let provider = chat(base_variants());
        let mut variants = base_variants();
        variants.insert(
            "participant".to_owned(),
            VariantDef::new()
                .state("view", view())
                .method(
                    "send",
                    MethodSpec::with(
                        Some(json!({"type": "object", "required": ["text", "urgent"], "properties": {"text": {"type": "string"}, "urgent": {"type": "boolean"}}})),
                        Some(json!({"type": "object", "required": ["id"], "properties": {"id": {"type": "number"}}})),
                    ),
                ),
        );
        let required = chat(variants);
        let reason = coverage(&required.base.description, &provider.base.description).unwrap();
        assert_eq!(reason, "participant.send arguments");

        let provider_contract: AnyContract = required.into();
        let consumer: AnyContract = provider.into();
        let error = bind(&provider_contract, None, &consumer)
            .err()
            .expect("mismatch");
        assert_eq!(error.code, ChordErrorCode::ContractMismatch);
        assert_eq!(
            error.message,
            "t.chat@1 does not cover the consumer's description (participant.send arguments); forgotten version bump?"
        );
    }

    #[test]
    fn retyped_state_breaks_coverage() {
        let provider = chat(base_variants());
        let mut variants = base_variants();
        variants.insert(
            "viewer".to_owned(),
            VariantDef::new().state(
                "view",
                json!({"type": "object", "required": ["text"], "properties": {"text": {"type": "number"}}}),
            ),
        );
        let retyped = chat(variants);
        assert_eq!(
            coverage(&retyped.base.description, &provider.base.description).unwrap(),
            "viewer.view state"
        );
    }

    #[test]
    fn missing_variant_and_kind() {
        let provider = chat(base_variants());
        let mut variants = IndexMap::new();
        variants.insert("viewer".to_owned(), VariantDef::new().state("view", view()));
        let only_viewer = chat(variants);
        assert_eq!(
            coverage(&only_viewer.base.description, &provider.base.description).unwrap(),
            "variant participant missing"
        );

        let mut variants = base_variants();
        variants.insert(
            "participant".to_owned(),
            VariantDef::new()
                .state("view", view())
                .method("send", method()),
        );
        // method() 无参数，consumer 期望有参数。
        let broke = chat(variants);
        assert_eq!(
            coverage(&broke.base.description, &provider.base.description).unwrap(),
            "participant.send arguments"
        );
    }

    #[test]
    fn different_type_and_host() {
        let a = chat(base_variants());
        let mut variants = base_variants();
        variants.insert("viewer".to_owned(), VariantDef::new().state("view", view()));
        let moved = remote(
            "t.chat",
            RemoteDef {
                version: 1,
                input: chat_input(),
                host: Placement::named("server"),
                variants,
            },
        )
        .unwrap();
        assert_eq!(
            coverage(&moved.base.description, &a.base.description).unwrap(),
            "host"
        );

        let other = remote(
            "t.other",
            RemoteDef {
                version: 1,
                input: chat_input(),
                host: Placement::family("session:", "session"),
                variants: base_variants(),
            },
        )
        .unwrap();
        assert_eq!(
            coverage(&other.base.description, &a.base.description).unwrap(),
            "different type"
        );
    }

    #[test]
    fn bind_reports_placement_and_adapter_messages() {
        let provider = chat(base_variants());
        let mut variants = IndexMap::new();
        variants.insert("viewer".to_owned(), VariantDef::new().state("body", view()));
        let v2 = remote(
            "t.chat",
            RemoteDef {
                version: 2,
                input: chat_input(),
                host: Placement::family("session:", "session"),
                variants,
            },
        )
        .unwrap();
        let provider_contract: AnyContract = v2.clone().into();
        let consumer: AnyContract = provider.into();
        let error = bind(&provider_contract, None, &consumer)
            .err()
            .expect("mismatch");
        assert_eq!(
            error.message,
            "t.chat: provider version 2, consumer version 1, no accepts adapter"
        );

        let adapter: Adapter = std::sync::Arc::new(
            |current: Value| json!({"variant": "viewer", "view": current["body"]}),
        );
        let mut accepts = Accepts::new();
        accepts.insert(1, adapter.clone());
        let bound = bind(&provider_contract, Some(&accepts), &consumer)
            .unwrap()
            .unwrap();
        assert_eq!(
            (bound)(json!({"body": 1})),
            json!({"variant": "viewer", "view": 1})
        );

        // 移到别的 host：即使有 accepts 也不桥接。
        let moved = remote(
            "t.chat",
            RemoteDef {
                version: 2,
                input: chat_input(),
                host: Placement::named("server"),
                variants: {
                    let mut v = IndexMap::new();
                    v.insert("viewer".to_owned(), VariantDef::new().state("body", view()));
                    v
                },
            },
        )
        .unwrap();
        let moved_contract: AnyContract = moved.into();
        let error = bind(&moved_contract, Some(&accepts), &consumer)
            .err()
            .expect("mismatch");
        assert_eq!(
            error.message,
            "t.chat lives on server, the consumer expects session:<session>"
        );

        // local 对 local。
        let a = local(
            "t.local",
            LocalDef {
                version: 1,
                input: json!({"type": "object", "properties": {}}),
                members: IndexMap::new(),
            },
        )
        .unwrap();
        let b = local(
            "t.local",
            LocalDef {
                version: 1,
                input: json!({"type": "object", "properties": {}}),
                members: IndexMap::new(),
            },
        )
        .unwrap();
        let x: AnyContract = a.into();
        let y: AnyContract = b.into();
        assert!(bind(&x, None, &y).unwrap().is_none());
    }

    #[test]
    fn opaque_members_compare_by_id() {
        let mut members = IndexMap::new();
        members.insert(
            "harness".to_owned(),
            LocalMember::Opaque(crate::contract::OpaqueSpec::unchecked("h@1")),
        );
        members.insert(
            "onChange".to_owned(),
            LocalMember::Method(MethodSpec::with(
                Some(json!({"type": "function", "parameters": [{"type": "string"}], "returnType": {"type": "void"}})),
                None,
            )),
        );
        let a = local(
            "t.idx",
            LocalDef {
                version: 1,
                input: json!({"type": "object", "properties": {}}),
                members,
            },
        )
        .unwrap();
        let mut members = a.def.members.clone();
        members.insert(
            "harness".to_owned(),
            LocalMember::Opaque(crate::contract::OpaqueSpec::unchecked("h@2")),
        );
        let b = local(
            "t.idx",
            LocalDef {
                version: 1,
                input: json!({"type": "object", "properties": {}}),
                members,
            },
        )
        .unwrap();
        assert_eq!(
            coverage(&b.base.description, &a.base.description).unwrap(),
            ".harness opaque h@2 != h@1"
        );
        assert!(fits(
            &json!({"type": "function", "parameters": [{}], "returnType": {"type": "void"}}),
            &json!({"type": "function", "parameters": [{"type": "string"}], "returnType": {"type": "void"}})
        ));
        assert!(!fits(
            &json!({"type": "function", "parameters": [{"type": "string"}], "returnType": {"type": "void"}}),
            &json!({"type": "function", "parameters": [{}], "returnType": {"type": "void"}})
        ));
    }

    #[test]
    fn method_presence_must_match_and_void_consumer_accepts_results() {
        let of = |members: IndexMap<String, LocalMember>| {
            local(
                "t.methods",
                LocalDef {
                    version: 1,
                    input: json!({"type": "object", "properties": {}}),
                    members,
                },
            )
            .unwrap()
            .base
            .description
        };
        let mut none_members = IndexMap::new();
        none_members.insert("m".to_owned(), LocalMember::Method(method()));
        let none = of(none_members);
        assert_eq!(
            serde_json::to_string(&none.variants[LOCAL_VARIANT]["m"]).unwrap(),
            "{\"kind\":\"method\"}"
        );

        let mut args_members = IndexMap::new();
        args_members.insert(
            "m".to_owned(),
            LocalMember::Method(MethodSpec::args(json!({"type": "string"}))),
        );
        let with_args = of(args_members);

        let mut result_members = IndexMap::new();
        result_members.insert(
            "m".to_owned(),
            LocalMember::Method(MethodSpec::with(None, Some(json!({"type": "number"})))),
        );
        let with_result = of(result_members);

        assert_eq!(coverage(&none, &none), None);
        assert_eq!(
            coverage(&with_args, &none).unwrap(),
            ".m arguments",
            "the consumer sends none, the provider expects some"
        );
        assert_eq!(
            coverage(&none, &with_args).unwrap(),
            ".m arguments",
            "the consumer sends some, the provider takes none"
        );
        assert_eq!(
            coverage(&with_result, &none),
            None,
            "a void consumer ignores a result"
        );
        assert_eq!(
            coverage(&none, &with_result).unwrap(),
            ".m result",
            "a consumer expecting a result needs one"
        );
    }

    #[test]
    fn takes_no_args_reads_first_method_member() {
        let mut members = IndexMap::new();
        members.insert("m".to_owned(), LocalMember::Method(method()));
        let contract = local(
            "t",
            LocalDef {
                version: 1,
                input: json!({"type": "object", "properties": {}}),
                members,
            },
        )
        .unwrap();
        let contract: AnyContract = contract.into();
        assert!(takes_no_args(&contract, "m"));
        assert!(!takes_no_args(&contract, "missing"));

        let mut members = IndexMap::new();
        members.insert(
            "m".to_owned(),
            LocalMember::Method(MethodSpec::args(json!({"type": "string"}))),
        );
        let contract = local(
            "t",
            LocalDef {
                version: 1,
                input: json!({"type": "object", "properties": {}}),
                members,
            },
        )
        .unwrap();
        let contract: AnyContract = contract.into();
        assert!(!takes_no_args(&contract, "m"));
    }
}
