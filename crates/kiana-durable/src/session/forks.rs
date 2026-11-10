//! `session/forks.ts`：选出一份 fork 要复制的全部持久会话文档。
//!
//! # 这个文件解决什么问题
//! fork 一个会话时，子会话要继承父会话（或 fork 点条目所属的祖先会话）中 conversation
//! 文档（笔记、todo、扩展状态等）的值，但必须拿到独立的化身：此后父子任一侧的修改都不能
//! 影响另一侧。本模块只做“选档”：决定哪些逻辑地址需要复制、每个副本从哪个时间点读源值；
//! 它不读取文档内容，也不提交任何写入。真正的复制由提交时的 `StorageWrite::DocumentCopy`
//! （新记录 + `DocumentCopySource`）交给 `Storage::commit` 完成。
//!
//! # 调用方
//! `transaction` 的 `stage_conversation`（`Tx::fork_conversation` 的实现路径）在创建
//! 子会话记录时调用 [`prepare_fork_document_copies`]，把结果登记成惰性 copy 目标：
//! 事务里读过某个副本时会把它物化成完整 `document.create`，没读过的仍以 `document.copy`
//! 写入。
//!
//! # 两轮扫描
//! 1. **as-of 轮**：先 `visible_entry` 校验 fork 点 `at` 对父会话可见，并取得条目实际
//!    所属的会话与 `commit_seq`；然后在**该会话**的 `DocumentPoint::At(commit_seq)`
//!    扫描 `fork: asOf` 的会话文档。因此从祖先会话的条目 fork 时，复制的是条目所属会话
//!    在该提交处的状态，而不是直接父会话的状态。读到的是该次提交结束时的状态，与 fork
//!    点同一次提交里的文档改动会被包含。
//! 2. **current 轮**：在**直接父会话**的 `DocumentPoint::Current` 扫描 `fork: current`
//!    的文档。
//!
//! 两轮都按 `SCAN_PAGE_SIZE` 分页，游标原样回传给后端，直到 `Page::next` 为 `None`。
//! 每一轮只选 `fork` 恰好等于该轮策略的记录，所以 `fork: initial` 或未声明 `fork` 的
//! 会话文档永远不复制：子会话首次读取时按定义初始值新建，而不是复制父值。
//!
//! # 不变量与失败模式
//! - 每个源记录铸一个新文档 ID（`IdKind::Document`）；新记录的作用域恒为子会话，
//!   `kind`/`key`/`fork` 逐字复制。
//! - 同一个子会话逻辑地址（`kind` + 子会话 + `key`）在一次 fork 中最多出现一次；两轮共用
//!   同一个地址集合，所以“as-of 轮选中旧化身、current 轮又选中同地址新化身”也会被
//!   拒绝（`tests/session_forks.rs` 有对应用例）。
//! - 任何错误都以 `io::Error::other` 或存储自身的错误向上返回；本模块不提交存储写入。

use std::collections::HashSet;
use std::io;

use kiana_chord::context::Context;
use serde_json::Value;

use crate::types::{
    BoxError, ConversationId, Cursor, DocumentCopySource, DocumentCreate, DocumentFork,
    DocumentPoint, DocumentQuery, DocumentScope, EntryId, IdKind, Storage, address_id,
    id_from_number,
};

// 两轮扫描共用的分页大小；后端按游标继续，该值只决定每页最多请求多少条。
const SCAN_PAGE_SIZE: usize = 256;

// 一轮扫描使用的 fork 策略：`AsOf` 读 fork 点提交处的值，`Current` 读父会话当前值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ForkPolicy {
    AsOf,
    Current,
}

impl ForkPolicy {
    fn document_fork(self) -> DocumentFork {
        match self {
            ForkPolicy::AsOf => DocumentFork::AsOf,
            ForkPolicy::Current => DocumentFork::Current,
        }
    }
}

/// 一次 fork 随子会话创建的一份定义无关文档复制（TS: `ForkDocumentCopy`）。
///
/// “定义无关”指这里只带存储记录与来源，不携带 `DocDefinition` 或定义版本：使用时由事务
/// 把 `DocTarget` 上的定义套上去，并先校验源记录与 `record` 的地址/语义字段一致。
#[derive(Debug, Clone, PartialEq)]
pub struct ForkDocumentCopy {
    /// 新化身记录：ID 由 `Storage::mint_id` 新铸，作用域指向子会话；此刻尚未写入存储。
    pub record: DocumentCreate,
    /// 来源：源化身 ID 与读取点，提交时由 `StorageWrite::DocumentCopy` 消费。
    pub source: DocumentCopySource,
}

/// 选出一份 fork 要复制的全部持久会话文档（TS: `prepareForkDocumentCopies`）。
///
/// 两轮扫描：先取 `at` 条目原会话在 `entry.commitSeq` 的 `asOf` 文档，再取父会话当前的
/// `current` 文档。同一逻辑地址被选中两次是调用方错误。
///
/// # 参数
/// - `storage`：只用 `visible_entry` / `scan_documents` / `mint_id`，不读取任何文档值。
/// - `parent_conversation_id`：fork 的父会话；`visible_entry` 按它的祖先范围判断 `at`
///   是否可见，current 轮也扫描它。
/// - `at`：fork 点条目 ID。可见但属于祖先会话时，as-of 轮扫描的是条目所属会话。
/// - `child_conversation_id`：子会话 ID；所有新记录的 `scope` 都指向它。调用方传入的是
///   本事务刚分配的会话 ID。
/// - `context`：传给 `visible_entry` 与 `scan_documents` 这两个带 `context` 参数的存储调用，
///   携带取消信号与遥测上下文；`mint_id` 的签名不接收 `context`，故不经过它。
///
/// # 返回
/// 每个被选中的源记录对应一项，同一逻辑地址最多一项；没有任何匹配时返回空 `Vec`。
/// 顺序是 as-of 轮在前、current 轮在后，轮内保持 `scan_documents` 的分页顺序；
/// 本函数不排序。
///
/// # 错误
/// - `at` 对父会话不可见（`visible_entry` 返回 `Ok(None)`）：`io::Error::other`，消息为
///   `Entry {at} is not visible from conversation {parent_conversation_id}`。
/// - 同一子会话地址被选中两次：`io::Error::other`，消息为
///   `Fork selects multiple source documents for {member}`。
/// - `visible_entry` / `scan_documents` / `mint_id` 的错误原样返回。
///
/// # 副作用
/// 每份复制调用一次 `mint_id(IdKind::Document)`；不提交存储写入、不修改已有文档。
pub async fn prepare_fork_document_copies(
    storage: &dyn Storage,
    parent_conversation_id: ConversationId,
    at: EntryId,
    child_conversation_id: ConversationId,
    context: &dyn Context,
) -> Result<Vec<ForkDocumentCopy>, BoxError> {
    let entry = storage
        .visible_entry(parent_conversation_id, at, context)
        .await?;
    let entry = entry.ok_or_else(|| {
        io::Error::other(format!(
            "Entry {at} is not visible from conversation {parent_conversation_id}"
        ))
    })?;

    let mut copies = Vec::new();
    let mut copied_addresses = HashSet::new();
    // as-of 轮：条目可能来自祖先会话（`visible_entry` 允许祖先链），所以扫描条目实际所属
    // 会话在它提交序号处的状态，而不是直接父会话。
    collect_copies(
        storage,
        DocumentScope::Conversation {
            conversation_id: entry.entry.conversation_id,
        },
        DocumentPoint::At(entry.commit_seq),
        ForkPolicy::AsOf,
        child_conversation_id,
        &mut copies,
        &mut copied_addresses,
        context,
    )
    .await?;
    // current 轮：始终取直接父会话的当前状态。同一个 `copied_addresses` 继续用于跨轮判重。
    collect_copies(
        storage,
        DocumentScope::Conversation {
            conversation_id: parent_conversation_id,
        },
        DocumentPoint::Current,
        ForkPolicy::Current,
        child_conversation_id,
        &mut copies,
        &mut copied_addresses,
        context,
    )
    .await?;
    Ok(copies)
}

// 一轮扫描：把 `scope` 在 `at` 处、`fork` 恰好等于 `policy` 的会话文档追加到 `copies`。
// `copied_addresses` 由调用方在两轮之间共享，用于检测同一子会话地址的重复选择；返回
// `Ok(())` 不代表一定有副本。
#[allow(clippy::too_many_arguments)]
async fn collect_copies(
    storage: &dyn Storage,
    scope: DocumentScope,
    at: DocumentPoint,
    policy: ForkPolicy,
    child_conversation_id: ConversationId,
    copies: &mut Vec<ForkDocumentCopy>,
    copied_addresses: &mut HashSet<String>,
    context: &dyn Context,
) -> Result<(), BoxError> {
    let mut cursor: Option<Cursor> = None;
    // 分页读取：游标是不透明的后端状态，原样回传；`next` 为 `None` 时这一轮结束。
    loop {
        let page = storage
            .scan_documents(
                &DocumentQuery {
                    scope,
                    at,
                    kind: None,
                },
                SCAN_PAGE_SIZE,
                cursor.as_ref(),
                context,
            )
            .await?;
        for source in page.items {
            // 只复制会话作用域的文档（`DocumentQuery.scope` 已限定作用域），且只复制
            // fork 语义恰好等于本轮策略的记录：`initial` 或未声明 fork 的文档留给子会话新建。
            if !matches!(source.scope, DocumentScope::Conversation { .. })
                || source.fork != Some(policy.document_fork())
            {
                continue;
            }
            // 每份副本都铸新化身 ID：文档记录重建时绝不复用旧 ID。
            let minted = storage.mint_id(IdKind::Document).await?;
            let record = DocumentCreate {
                id: id_from_number(minted),
                kind: source.kind.clone(),
                key: source.key.clone(),
                scope: DocumentScope::Conversation {
                    conversation_id: child_conversation_id,
                },
                // 源为 `Latest` 时保持 `Latest`，其余值（`Rewindable` 或缺失）都写成
                // `Rewindable`；缺失值也会落到 `else` 分支。
                history: if source.history == Some(crate::types::DocumentHistory::Latest) {
                    Some(crate::types::DocumentHistory::Latest)
                } else {
                    Some(crate::types::DocumentHistory::Rewindable)
                },
                fork: source.fork,
            };
            // 判重用的是新记录（子会话作用域）的地址身份，而不是源地址：两份不同源文档
            // 若映射到同一个子地址，子会话会在该逻辑地址上出现两个新化身，所以在提交前
            // 就失败。
            let copy_address = address_id(&crate::types::DocumentAddress {
                kind: record.kind.clone(),
                scope: record.scope,
                key: record.key.clone(),
            });
            if copied_addresses.contains(&copy_address) {
                // 消息里的成员名只用于诊断：单例是 `kind`，家族成员是 `kind/key`。
                let member = match &record.key {
                    None => record.kind.clone(),
                    Some(key) => format!("{}/{}", record.kind, key_text(key)),
                };
                return Err(io::Error::other(format!(
                    "Fork selects multiple source documents for {member}"
                ))
                .into());
            }
            copied_addresses.insert(copy_address);
            // 源侧只记 (id, at)：提交时由 Storage 在 at 处读取源值，复制成新化身。
            copies.push(ForkDocumentCopy {
                record,
                source: DocumentCopySource { id: source.id, at },
            });
        }
        cursor = page.next;
        if cursor.is_none() {
            break;
        }
    }
    Ok(())
}

/// TS 模板字符串里的 `${key}`：字符串原样，其余 JSON 值按 JS 的字符串化。
///
/// 只用于构造重复地址错误消息里的成员名，不参与地址计算或存储键。
fn key_text(key: &Value) -> String {
    match key {
        Value::String(text) => text.clone(),
        other => kiana_json::js_object::stringify(other),
    }
}
