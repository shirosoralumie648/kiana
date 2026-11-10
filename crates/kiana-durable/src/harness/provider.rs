//! `harness/provider.ts`：一个会话对 provider 的稳定身份。
//!
//! TS: `packages/durable/src/harness/provider.ts`。`kiana.provider` 是 latest/initial 文档：
//! 每个 fork 都以全新身份开始，而不是复制父会话的。
//!
//! # 为什么需要它
//! 同一会话的 generation 与压缩请求都要带上同一个 provider session id；fork 出来的会话则要
//! 换一个新 id。这个 id 不属于会话记录本身，而是独立的 `kiana.provider` 文档，因此它会随
//! 会话一起持久化、一起 fork。上游 `test/provider-session-cache-e2e.test.ts` 的 Rust 端口
//! （`tests/provider_session_cache_e2e.rs`，`#[ignore]` 的 live 用例）把“每轮请求转发文档里的
//! `sessionId`、缓存命中从第二轮起稳定”写成验收目标，可以据此理解这个文档的用途。
//!
//! # 调用方与流程
//! 会话创建时 `HarnessHooks::conversation_created` 会先物化这份文档
//! （`harness/harness.rs`），所以正常路径里它总是存在；`view` 也把它挂进会话视图。
//! `generation` 与 `compaction` 的请求阶段调用 [`ensure_provider_session_id`]：
//! 1. 读一次快照；文档存在就直接返回其中的 `sessionId`，不写任何东西；
//! 2. 文档缺失（旧会话）时提交一次 `TaskRuntimeChange`：回调里的 `tx.doc()` 会创建文档并
//!    执行 `initial()`（新 uuidv7），随后把值带回调用方，任务状态保持不变；
//! 3. 快照/提交失败，或提交成功却没带回合法字符串时，调用方拿到错误，当前 phase 失败。
//!
//! # 文档形状与约束
//! 值是 `{ "sessionId": "<uuid-v7>" }`，键名必须与 [`ProviderState`] 的 serde 重命名一致
//! （测试 `provider_state_serializes_as_camel_case` 锁定）。latest/initial 既是 fork 语义的
//! 来源（子会话重新执行 `initial()`），也同时满足事务延迟提交对会话文档的要求
//! （`session/transaction.rs` 的 `holdable`：history 为 latest、fork 为 initial）。
//! `checkpoint_when` 恒为真，因此每次普通变化都落成完整 base，不会累积 delta 链。
//!
//! 注意：已存在但 `sessionId` 缺失或不是字符串的文档只读、不报错，函数返回 `None`
//! （对应 TS 的 `undefined`；测试 `a_snapshot_without_a_string_session_id_yields_none`）。

use std::sync::{LazyLock, Mutex};

use async_trait::async_trait;
use kiana_ai::utils::uuid::uuidv7;
use kiana_chord::context::Context;

use crate::harness::types::{TaskRuntime, TaskRuntimeChange};
use crate::types::{
    BoxError, CommitOptions, DocDefinition, DocTarget, DocToken, DocumentFork, DocumentSemantics,
    JsonObject, JsonValue, TaskRecord, TaskState, Tx, define_doc,
};

/// 一个会话对 provider 的稳定身份（TS: `ProviderState`）。
///
/// 这是对 `kiana.provider` 文档值的类型化视图：文档本身由 [`PROVIDER_DOC`] 的 `initial`
/// 用裸 JSON 构造，`session_id_of` 也不经过这个类型。因此字段的 serde 名（`sessionId`）
/// 必须与文档键名保持一致；测试 `provider_state_serializes_as_camel_case` 锁定序列化形状。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderState {
    /// 稳定的 session id。
    pub session_id: String,
}

/// 内建 provider 状态；每个 fork 都以全新身份开始。
///
/// TS: `ProviderDoc`
///
/// `initial` 每次调用都会生成新的 uuidv7（`uuidv7(None)` 以当前时间为时间戳），所以创建与
/// fork 都拿到新身份；`with_checkpoint_when(|_, _, _| true)` 让每次变化都写成完整 base。
/// 静态值只构造一次（`LazyLock`），各目标共享同一份 `Arc<DocDefinition>`。
pub static PROVIDER_DOC: LazyLock<DocToken> = LazyLock::new(|| {
    define_doc(
        DocDefinition::new(
            "kiana.provider",
            1,
            DocumentSemantics::latest(DocumentFork::Initial),
        )
        .with_initial(|_| {
            let session_id =
                uuidv7(None).expect("uuidv7() must accept the current time as a timestamp");
            let mut value = JsonObject::new();
            value.insert("sessionId".to_owned(), JsonValue::String(session_id));
            value
        })
        .with_checkpoint_when(|_, _, _| true),
    )
});

/// 取锁；中毒时继续使用内部值。
///
/// 这里保护的只是“迁移回调写回的一个 `Option<String>`”，临界区内没有跨字段不变量需要修复，
/// 所以沿用仓库其他模块的 `into_inner()` 做法。
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 从已提交的值里取 `sessionId`。
///
/// 缺失、`null`、数字等非字符串值都返回 `None`（不 trim、不校验 uuid 形状）；已存在文档的
/// 这种形状被视为“没有 id”而不是错误，见 [`ensure_provider_session_id`] 的说明。
fn session_id_of(value: &JsonObject) -> Option<String> {
    value
        .get("sessionId")
        .and_then(JsonValue::as_str)
        .map(str::to_owned)
}

/// 迁移提交：读 `tx.doc()`（触发 `initial()`），把身份带回调用方，状态不变。
///
/// 文档不存在时 `Tx::doc` 会按定义创建并执行 `initial()`（`session/transaction.rs` 的 `doc`
/// 阶段 4），这正是“旧会话补出身份”的机制。`created` 用 `Mutex<Option<String>>` 承载结果，
/// 是因为 `TaskRuntimeChange::run` 只能通过 `&self` 传递信息；`run` 返回 `Ok(None)`，任务
/// 状态保持不变。
struct EnsureProviderSessionId {
    conversation_id: crate::types::ConversationId,
    created: Mutex<Option<String>>,
}

#[async_trait]
impl TaskRuntimeChange for EnsureProviderSessionId {
    async fn run(&self, tx: &dyn Tx, _current: &TaskRecord) -> Result<Option<TaskState>, BoxError> {
        let target = DocTarget::conversation(&PROVIDER_DOC, self.conversation_id);
        let draft = tx.doc(&target).await?;
        *lock(&self.created) = session_id_of(&draft);
        Ok(None)
    }
}

/// 返回已持久化的身份，正常路径不写入。没有 `kiana.provider` 的旧会话会得到一个迁移提交：
/// 它的 `tx.doc()` 在 provider 请求开始之前先跑 `initial()`。
///
/// 返回 `Option<String>`：TS 的 `return existing.sessionId` 在运行期可以是 `undefined`
/// （快照存在但不是本模块写出的形状），调用方把它原样放进 `StreamOptions.sessionId`
/// （Rust: `Option<String>`）。迁移路径的 `created === undefined` 才对应 TS 的抛错。
///
/// # 流程
/// 1. 用 `runtime.snapshot()` 读 `kiana.provider`；命中就返回 `session_id_of` 的结果，全程
///    不写。
/// 2. 未命中才构造 `EnsureProviderSessionId` 并通过 `runtime.commit()` 提交。该提交走任务
///    的变更门控（`TaskRuntimeChange` 的说明：任务已结束、会话关闭、记录不再 `running` 或
///    遇到 abort 标记时提交失败），失败错误原样返回给调用方。
/// 3. 提交成功后从回调留下的值取结果；没有值时报
///    `Conversation {id} has no provider session ID`（防御分支：新建文档的 `initial()` 总会
///    写入字符串 id）。
///
/// # 失败模式
/// 读取/提交错误直接返回，调用方（generation 或 compaction 的 phase）随之失败。这个函数
/// 不修复形状不对的已存在文档，也不重试。
///
/// TS: `ensureProviderSessionId`
pub async fn ensure_provider_session_id<I, S, R, H>(
    runtime: &dyn TaskRuntime<I, S, R, H>,
    context: &dyn Context,
) -> Result<Option<String>, BoxError> {
    let conversation_id = runtime.conversation_id();
    let target = DocTarget::conversation(&PROVIDER_DOC, conversation_id);
    if let Some(existing) = runtime.snapshot(&target, context).await? {
        // TS: `if (existing !== undefined) return existing.sessionId;` — 缺 `sessionId`
        // 或不是字符串时运行期就是 `undefined`，不写迁移提交。
        return Ok(session_id_of(&existing));
    }
    let change = EnsureProviderSessionId {
        conversation_id,
        created: Mutex::new(None),
    };
    runtime
        .commit(&change, context, CommitOptions::default())
        .await?;
    match lock(&change.created).take() {
        Some(session_id) => Ok(Some(session_id)),
        None => Err(format!("Conversation {conversation_id} has no provider session ID").into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_doc_is_latest_initial_and_always_checkpoints() {
        let definition = &PROVIDER_DOC.definition;
        assert_eq!(definition.kind, "kiana.provider");
        assert_eq!(definition.version, 1);
        assert_eq!(
            definition.semantics,
            DocumentSemantics::latest(DocumentFork::Initial)
        );
        assert!(!definition.family);
        let initial = (definition.initial)(None);
        let session_id = session_id_of(&initial).expect("initial value carries a session id");
        // uuidv7 的文本形式：8-4-4-4-12 的十六进制。
        assert_eq!(session_id.len(), 36);
        assert_eq!(session_id.as_bytes()[14], b'7');
        assert!((definition.checkpoint_when)(
            &initial,
            &[],
            crate::types::CheckpointInfo {
                deltas_since_base: 0
            }
        ));
    }

    // 回归（minor）：快照存在但没有可用的字符串 `sessionId`（外部/损坏的文档形状）时，TS 的
    // `return existing.sessionId` 运行期是 `undefined`（且不写迁移提交），不是抛错；Rust 用
    // `None` 表达，调用方把它放进 `StreamOptions.session_id`（`Option<String>`）。
    #[test]
    fn a_snapshot_without_a_string_session_id_yields_none() {
        assert_eq!(session_id_of(&JsonObject::new()), None);
        let mut number = JsonObject::new();
        number.insert("sessionId".to_owned(), JsonValue::from(7));
        assert_eq!(session_id_of(&number), None);
        let mut null = JsonObject::new();
        null.insert("sessionId".to_owned(), JsonValue::Null);
        assert_eq!(session_id_of(&null), None);
        let mut valid = JsonObject::new();
        valid.insert("sessionId".to_owned(), JsonValue::String("abc".to_owned()));
        assert_eq!(session_id_of(&valid), Some("abc".to_owned()));
    }

    #[test]
    fn every_initial_value_is_a_fresh_identity() {
        let first = (PROVIDER_DOC.definition.initial)(None);
        let second = (PROVIDER_DOC.definition.initial)(None);
        assert_ne!(
            session_id_of(&first).unwrap(),
            session_id_of(&second).unwrap()
        );
    }

    #[test]
    fn provider_state_serializes_as_camel_case() {
        let value = serde_json::to_value(ProviderState {
            session_id: "abc".to_owned(),
        })
        .unwrap();
        assert_eq!(value, serde_json::json!({ "sessionId": "abc" }));
    }
}
