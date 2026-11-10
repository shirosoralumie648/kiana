//! `testing/storage-conformance.ts`：与 runner 无关的 `Storage` 一致性用例。
//!
//! 用例名与顺序逐字来自 TS；每个用例都通过 `AdoptionChecked`（TS 的 `checkAdoption`）包装
//! storage，验证 `adopt` 恰好一次、带提交序号且在 `commit` resolve 之前，失败的提交不调用；
//! 批次在 adopt 时已可见由 `adopts a durable commit once before resolving …` 用例单独验证。
//! 包装器只观察调用时机、次数与序号，不证明后端内部的持久化顺序（见 `AdoptionChecked` 文档）。
//!
//! # 这个文件为什么存在
//!
//! `Storage` 有多个后端（`storage/memory.rs`、`storage/jsonl/`、`storage/sqlite/`）。如果每个后端各写
//! 一遍“提交是原子的”“游标能续页”“文档能按历史点物化”这类测试，契约会随实现漂移；把用例集中
//! 到这里之后，后端只需提供“怎么拿到一个 storage”
//! （[`StorageConformanceOptions::with_storage`]）与“断言失败怎么报”
//! （[`StorageConformanceOptions::assertions`]），跑的就是同一组 26 个用例（保证各后端契约
//! 一致）。
//!
//! 调用方：
//!
//! - `testing/runner.rs` 的 `register_storage_conformance` 把用例注册给 Vitest/Jest 形状的 runner；
//! - `tests/storage_conformance.rs` 锁定用例标题与顺序，并验证 runner 的转发；
//! - `tests/jsonl_storage.rs`、`tests/sqlite_storage.rs` 直接调用 [`create_storage_conformance`]，
//!   对每个用例执行 `(case.run)().await`（两个后端都各有“普通”和“跨 reopen”两种 provider）。
//!
//! # 执行流程
//!
//! 1. [`create_storage_conformance`] 按 TS 顺序构造 26 个 [`StorageConformanceCase`]；
//! 2. 每个用例的 `run` 调用 `with_storage(use_storage)`，由 provider 准备 storage 并调用 `use`
//!    （`StorageConformanceProvider` 的契约是恰好调用并等待一次）；
//! 3. `use_storage` 把 storage 包进 `AdoptionChecked`（校验 `commit` 的 `adopt` 契约），再把
//!    包装后的 storage 与断言集合交给测试体；
//! 4. 测试体反复 `commit` / 读取，把读出的记录序列化成 JSON 后交给断言集合比较。
//!
//! # 对 provider 的隐含要求
//!
//! 每个用例都会调用 `create_root` 提交 ID 1 的根会话，第一个用例还断言“未写入时 `mint_id`
//! 先返回 2”。所以 provider 必须为每次 `use` 提供干净的 storage（新目录/新库或等价空状态）；
//! 复用已写过的 storage 会在第一个用例就失败。这个要求由用例断言体现，provider 的类型无法强制。
//!
//! # 注意事项
//!
//! - 记录比较走 serde JSON：`StorageWrite` 用 `serde_json::from_value` 由 JSON 字面量构造，读出的
//!   记录再用 `to_value` 序列化，对应 TS 的 `toEqual` 结构比较；字段名以 serde 的 camelCase 为准。
//! - 多个用例在提交后原地修改自己构造的 JSON（如 `detaches retained writes`），这是用来验证
//!   storage 深拷贝的输入变异，不是用例数据写错。
//! - `unwrap_result` 把存储错误转成 panic：普通读取失败会以 panic 暴露。需要匹配错误消息的预期失败
//!   用 `assert_rejects`（按“包含子串”匹配）；个别用例直接断言 `result.is_err()`，只证明失败本身、
//!   不校验消息（如 `copies stored document bases independently …` 的歧义复制源与 kind 不匹配）。
//! - 用例本身不做超时控制；超时与重试由调用方（runner 或 `#[tokio::test]`）决定。
//!
//! # 与 TS 的对应
//!
//! - 记录由 TS 对象字面量改写为 JSON 值：`StorageWrite` 有 serde 实现，`serde_json::from_value`
//!   就是 TS 的对象字面量构造。比较时把后端返回的记录序列化成 JSON（TS 的 `toEqual` 是结构比较）。
//! - `expect(x).toBeUndefined()` 对应 `Option::None`；映射为 JSON `null` 后与 `undefined` 的
//!   TS 语义一致。
//! - TS 的 `Storage.entry(id)` / `entry(conversationId, id)` 多载在 Rust 里是 `entry` /
//!   `visible_entry`。
//!
//! # 阅读顺序
//!
//! 1. `AdoptionChecked` 的 `commit`：所有用例共同依赖的 adopt 契约检查；
//! 2. `conformance_case` 与 [`create_storage_conformance`]：用例如何被包装与注册；
//! 3. 具体用例按组阅读（每个组前有 `//` 说明），用例名本身说明它锁定什么行为；
//! 4. 需要定位失败时看 `AdoptionChecked` 的错误消息与各用例 `assert_rejects` 的期望子串。

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::Poll;

use async_trait::async_trait;
use kiana_chord::context::{BACKGROUND_CONTEXT, Context};
use serde::Serialize;
use serde_json::{Value, json};

use crate::testing::types::{
    CaseFuture, ConformanceFuture, StorageConformanceAssertions, StorageConformanceCase,
    StorageConformanceOptions, StorageUse,
};
use crate::types::{
    AdoptFn, BoxError, ConversationId, ConversationQuery, ConversationRecord, Cursor,
    DocumentAddress, DocumentId, DocumentPoint, DocumentQuery, DocumentRecord, DocumentScope,
    EntryId, EntryQuery, EntryRecord, IdKind, Page, ROOT_CONVERSATION_ID, ScanOrder, Seq, Storage,
    StorageWrite, StoredDocument, StoredEntry, SubmissionId, SubmissionQuery, SubmissionRecord,
    TaskId, TaskQuery, TaskRecord, TaskStatus, id_from_number,
};

/// 整个套件共用的进程级空根 context。
///
/// [`BACKGROUND_CONTEXT`] 没有 abort signal、没有业务值、telemetry 为 NOOP（见 `kiana-chord` 的
/// `EmptyContext` 文档），所以用例不会因取消而中断，行为断言里也不会混入 telemetry 状态。
fn context() -> &'static dyn Context {
    &**BACKGROUND_CONTEXT
}

/// Storage 层提交没有内存 owner 需要 adopt。
///
/// 返回的空回调仍会被 `AdoptionChecked` 计数：契约要求每个成功的 `commit` 恰好调用一次 adopt，
/// 所以“什么都不做”不等于“可以不调用”。
fn adopt() -> AdoptFn {
    Box::new(|_seq: Seq| {})
}

/// 在 `adopt` 回调里发起的一次读取。
///
/// TS 在回调里写 `read = storage.entry(entryId, context)`：调用点同步执行到第一个 `await`，
/// 因此读在 adopt 时就已经开始。Rust 的 `async fn` 要到第一次 poll 才执行函数体，所以
/// [`start_read`] 在回调里立刻 poll 一次（让读取像同步调用那样在回调内启动）：已完成的读取直接
/// 留下结果，未完成的 future 等提交结束后再 await。
#[allow(clippy::large_enum_variant)]
enum StartedRead {
    Pending(Pin<Box<dyn Future<Output = Result<Option<StoredEntry>, BoxError>> + Send>>),
    Ready(Result<Option<StoredEntry>, BoxError>),
}

impl StartedRead {
    async fn await_read(self) -> Result<Option<StoredEntry>, BoxError> {
        match self {
            StartedRead::Ready(result) => result,
            StartedRead::Pending(read) => read.await,
        }
    }
}

/// `read = storage.entry(id, context)`：调用处立即开始执行读取（poll 一次），返回之后可 await 的句柄。
fn start_read(storage: &Arc<dyn Storage>, id: EntryId) -> StartedRead {
    let storage = storage.clone();
    let mut read: Pin<Box<dyn Future<Output = Result<Option<StoredEntry>, BoxError>> + Send>> =
        Box::pin(async move { storage.entry(id, context()).await });
    let mut poll_context = std::task::Context::from_waker(futures::task::noop_waker_ref());
    match read.as_mut().poll(&mut poll_context) {
        Poll::Ready(result) => StartedRead::Ready(result),
        Poll::Pending => StartedRead::Pending(read),
    }
}

/// 把测试里的 JSON 对象字面量解析成 [`StorageWrite`]（对应 TS 的对象字面量）。
///
/// 形状不对会 panic：这些字面量是用例的一部分，写错应当立刻暴露，而不是变成后端返回的错误。
fn write(value: Value) -> StorageWrite {
    serde_json::from_value(value).expect("storage write shape")
}

/// 浅合并两个 JSON 对象：`extra` 的同名键覆盖 `base`，其余键保留。
///
/// 用例用它从基础记录派生状态变体（例如整体替换 `state`）；不做递归合并，嵌套对象会被整个替换。
fn merge(mut base: Value, extra: Value) -> Value {
    if let (Value::Object(base), Value::Object(extra)) = (&mut base, extra) {
        for (key, value) in extra {
            base.insert(key, value);
        }
    }
    base
}

/// 序列化成 JSON 值；被断言的对象都应可序列化，失败即 panic。
fn json_of<T: Serialize + ?Sized>(value: &T) -> Value {
    serde_json::to_value(value).expect("conformance value serializes")
}

/// `Some` 序列化成值，`None` 映射为 JSON `null`——即 TS 的 `undefined` 在 `toBeUndefined`
/// 断言里的表示（见模块文档的“与 TS 的对应”）。
fn optional_json<T: Serialize + ?Sized>(value: Option<&T>) -> Value {
    match value {
        Some(value) => json_of(value),
        None => Value::Null,
    }
}

/// 把 [`StoredDocument`] 手工映射成存储文档的 JSON 字段形状（`record`/`version`/`value`/`deltasSinceBase`）。
///
/// [`StoredDocument`] 没有 serde 实现（见 `types.rs`），不能像其他记录那样直接 `to_value`，
/// 所以比较前在这里显式组装；字段名是当前稳定契约，改动需同步测试。
fn stored_document_json(document: &StoredDocument) -> Value {
    json!({
        "record": json_of(&document.record),
        "version": document.version,
        "value": document.value,
        "deltasSinceBase": document.deltas_since_base,
    })
}

/// 把 `Result` 解成值或 panic（错误按 `Display` 输出）。
///
/// 用于“这一步失败就说明后端违反契约”的读取；需要断言失败的操作用 `assert_rejects`。
fn unwrap_result<T>(result: Result<T, BoxError>) -> T {
    match result {
        Ok(value) => value,
        Err(error) => panic!("{error}"),
    }
}

// ID 工厂：`mint_id` 从全局数字命名空间返回一个新的候选 ID，这里再经 `id_from_number` 打上
// 对应记录类型的品牌。大多数用例先 mint 再写以避免与已提交记录的 ID 冲突；少数用例刻意使用
// 显式 ID（根会话 1、乱序 10/20/30、冲突 100、上界 2^53-1）来验证乱序索引、ID 冲突与耗尽。
async fn mint(storage: &dyn Storage, kind: IdKind) -> i64 {
    unwrap_result(storage.mint_id(kind).await)
}

async fn mint_entry(storage: &dyn Storage) -> EntryId {
    id_from_number(mint(storage, IdKind::Entry).await)
}

async fn mint_conversation(storage: &dyn Storage) -> ConversationId {
    id_from_number(mint(storage, IdKind::Conversation).await)
}

async fn mint_task(storage: &dyn Storage) -> TaskId {
    id_from_number(mint(storage, IdKind::Task).await)
}

async fn mint_submission(storage: &dyn Storage) -> SubmissionId {
    id_from_number(mint(storage, IdKind::Submission).await)
}

async fn mint_document(storage: &dyn Storage) -> DocumentId {
    id_from_number(mint(storage, IdKind::Document).await)
}

/// 提交一组写入并返回序号；只用于按契约应当成功的提交，失败直接 panic。
async fn commit_writes(storage: &dyn Storage, writes: &[StorageWrite]) -> Seq {
    unwrap_result(storage.commit(writes, adopt(), context()).await)
}

/// 提交 ID 为 [`ROOT_CONVERSATION_ID`] 的根会话并返回它。
///
/// 每个用例都从这里开始：条目、任务、提交记录大多挂在这个会话下，fork 用例还把它当作祖先链
/// 的起点。重复提交同一个 ID 会被后端拒绝（第一个用例专门断言了这一点），所以一个用例只应
/// 调用一次；`with_storage` 若复用同一份已写过的 storage，这里会立刻失败。
async fn create_root(storage: &dyn Storage) -> ConversationId {
    commit_writes(
        storage,
        &[write(json!({
            "type": "conversation",
            "value": { "id": ROOT_CONVERSATION_ID.get() },
        }))],
    )
    .await;
    ROOT_CONVERSATION_ID
}

// 以下构造器把 TS 测试里的对象字面量原样写成 JSON：`entry_json`/`document_record` 负责基础
// 字段，`merge` 叠加各用例的差异字段，`*_write` 再套上 `StorageWrite` 的 `type` 标签。
// 字段名必须与 serde 的 camelCase 表示一致（见 `types.rs` 的 `StorageWrite` 及各记录类型）。
fn pending_task(id: TaskId, conversation_id: ConversationId, phase: &str) -> Value {
    json!({
        "id": id.get(),
        "conversationId": conversation_id.get(),
        "kind": "test.task",
        "version": 1,
        "input": { "value": id.get() },
        "state": { "status": "pending", "checkpoint": { "phase": phase } },
        "background": false,
        "abortRequested": false,
    })
}

fn entry_json(id: EntryId, conversation_id: ConversationId, kind: &str, extra: Value) -> Value {
    merge(
        json!({
            "id": id.get(),
            "conversationId": conversation_id.get(),
            "kind": kind,
        }),
        extra,
    )
}

fn entry_write(
    id: EntryId,
    conversation_id: ConversationId,
    kind: &str,
    extra: Value,
) -> StorageWrite {
    write(json!({
        "type": "entry",
        "value": entry_json(id, conversation_id, kind, extra),
    }))
}

fn task_write(value: Value) -> StorageWrite {
    write(json!({ "type": "task", "value": value }))
}

fn submission_write(value: Value) -> StorageWrite {
    write(json!({ "type": "submission", "value": value }))
}

fn conversation_write(value: Value) -> StorageWrite {
    write(json!({ "type": "conversation", "value": value }))
}

fn document_record(id: DocumentId, kind: &str, scope: Value, extra: Value) -> Value {
    merge(
        json!({ "id": id.get(), "kind": kind, "scope": scope }),
        extra,
    )
}

// 文档命令有四种形状：create（新化身 + 初始 base）、copy（新化身 + 来源点）、retire，
// 以及 change（同一命令既可能是 base 也可能是 delta，见 `document_base_write`/`document_delta_write`）。
fn document_create_write(record: Value, version: i64, value: Value) -> StorageWrite {
    write(json!({
        "type": "document.create",
        "record": record,
        "content": { "kind": "base", "version": version, "value": value },
    }))
}

fn document_base_write(id: DocumentId, version: i64, value: Value) -> StorageWrite {
    write(json!({
        "type": "document.change",
        "id": id.get(),
        "content": { "kind": "base", "version": version, "value": value },
    }))
}

fn document_delta_write(id: DocumentId, version: i64, ops: Value) -> StorageWrite {
    write(json!({
        "type": "document.change",
        "id": id.get(),
        "content": { "kind": "delta", "version": version, "ops": ops },
    }))
}

fn document_retire_write(id: DocumentId) -> StorageWrite {
    write(json!({ "type": "document.retire", "id": id.get() }))
}

fn document_copy_write(record: Value, source_id: DocumentId, at: Value) -> StorageWrite {
    write(json!({
        "type": "document.copy",
        "record": record,
        "source": { "id": source_id.get(), "at": at },
    }))
}

async fn document_at(
    storage: &dyn Storage,
    id: DocumentId,
    point: DocumentPoint,
) -> Option<StoredDocument> {
    unwrap_result(storage.document(id, point, context()).await)
}

async fn find_document_at(
    storage: &dyn Storage,
    address: &DocumentAddress,
    point: DocumentPoint,
) -> Option<DocumentRecord> {
    unwrap_result(storage.find_document(address, point, context()).await)
}

async fn scan_documents_page(
    storage: &dyn Storage,
    query: &DocumentQuery,
    limit: usize,
    cursor: Option<&Cursor>,
) -> Page<DocumentRecord> {
    unwrap_result(
        storage
            .scan_documents(query, limit, cursor, context())
            .await,
    )
}

/// 构造条目扫描查询；`order` 为 `None` 表示用后端默认顺序（条目表默认降序，见 `EntryQuery::order`）。
fn entry_query(
    conversation_id: ConversationId,
    min_entry_id: Option<EntryId>,
    max_entry_id: Option<EntryId>,
    order: Option<ScanOrder>,
) -> EntryQuery {
    EntryQuery {
        conversation_id,
        min_entry_id,
        max_entry_id,
        order,
    }
}

/// 构造文档的逻辑地址：`kind` + 精确作用域 + 可选 `key`。
///
/// `key` 为 `None` 表示单例文档，`Some` 表示同 kind 下的家族成员（同一作用域与 kind 下可有
/// 多个 key 不同的化身）。注意 `Some(Value::Null)` 仍是家族成员，与 `None` 不是同一个地址
/// （见 `DocumentCreate::key` 的文档）。
fn document_address(kind: &str, scope: DocumentScope, key: Option<Value>) -> DocumentAddress {
    DocumentAddress {
        kind: kind.to_owned(),
        scope,
        key,
    }
}

// 断言辅助：测试体只描述期望的 JSON 形状，`Option` → `null` 等表示差异在这里统一。
fn assert_ok(assertions: &dyn StorageConformanceAssertions, value: bool, message: &str) {
    assertions.ok(value, Some(message));
}

fn assert_strict(assertions: &dyn StorageConformanceAssertions, actual: Value, expected: Value) {
    assertions.strict_equal(&actual, &expected);
}

fn assert_deep(assertions: &dyn StorageConformanceAssertions, actual: Value, expected: Value) {
    assertions.deep_equal(&actual, &expected);
}

fn assert_partial(assertions: &dyn StorageConformanceAssertions, actual: Value, expected: Value) {
    assertions.partial_deep_equal(&actual, &expected);
}

fn assert_greater(assertions: &dyn StorageConformanceAssertions, actual: i64, expected: i64) {
    assertions.greater_than(actual as f64, expected as f64);
}

/// 断言操作失败，且错误消息**包含** `message`（Vitest `rejects.toThrow(string)` 的子串语义，
/// 不是精确相等）。
async fn assert_rejects(
    assertions: &dyn StorageConformanceAssertions,
    future: ConformanceFuture<'_>,
    message: &str,
) {
    assertions.rejects(future, message).await;
}

/// `expect(x).toBeUndefined()`。
fn assert_undefined<T: Serialize + ?Sized>(
    assertions: &dyn StorageConformanceAssertions,
    value: Option<&T>,
) {
    assert_strict(assertions, optional_json(value), Value::Null);
}

/// `expect(x).toBeUndefined()` for a materialized document（`StoredDocument` 没有 serde 实现，
/// 无法走 [`assert_undefined`]）。
fn assert_no_document(
    assertions: &dyn StorageConformanceAssertions,
    document: Option<&StoredDocument>,
) {
    assertions.ok(document.is_none(), None);
}

/// 把一页记录投影成 ID 数组的 JSON，便于用 `assert_deep` 同时比较内容与顺序。
fn ids_of<T, F: Fn(&T) -> i64>(items: &[T], id_of: F) -> Value {
    json!(items.iter().map(id_of).collect::<Vec<_>>())
}

// ---------------------------------------------------------------------------
// checkAdoption
// ---------------------------------------------------------------------------

/// `storage`，每个 `commit` 都按 adopt 契约检查：`adopt` 恰好一次、带提交序号、在 resolve 之前，
/// 失败的提交从不调用。
///
/// 包装只增强 `commit`，其余方法原样转发。`commit` 在调用 `inner.commit` 前构造一个记录闭包：
///
/// - 闭包先断言 `settled` 仍为 false（包装器尚未观察到 `inner.commit` 返回），然后把序号记进
///   `Mutex<Vec<Seq>>` 并调用调用方传入的 `adopt`；
/// - `inner.commit` 返回后置 `settled = true`，再把记录到的序号与结果对照：成功必须是
///   `[返回的 seq]`，失败必须是 `[]`（失败的提交从不调用 adopt）。
///
/// 这套检查依赖 [`AdoptFn`] 的契约“批次持久后、下一次提交开始前同步调用一次”（见
/// `Storage::commit`）：闭包运行在 `inner.commit` 的调用栈上，`settled` 只在它返回后置位；
/// 若后端在返回之后才调用 adopt，会以 `Storage.commit() adopted after it settled` 失败。
/// 它检查的是调用时机/次数/序号，不证明后端内部的持久化顺序——后者由
/// `adopts a durable commit once before resolving …` 用例在 adopt 回调里发起读取来验证。
///
/// `Arc<Mutex<Vec<Seq>>>` 与 `AtomicBool` 用于在闭包与返回后的检查之间共享状态；闭包需要
/// `Fn + Send + Sync`，不能直接可变借用外部变量。锁只用于保护 `Vec`、不跨 `await` 持有，保证
/// 记录本身没有数据竞争；“adopt 在 commit 返回前被调用”这一点仍以后端的同步契约为前提，
/// 不是锁能证明的。
struct AdoptionChecked {
    inner: Arc<dyn Storage>,
    assertions: Arc<dyn StorageConformanceAssertions>,
}

/// 一页扫描结果里比较所需的两部分：ID 列表与续页游标（`None` 表示没有下一页）。
struct PageIds {
    ids: Vec<i64>,
    next: Option<Cursor>,
}

/// 三个走同一套“顺序 + 游标”断言的表。
///
/// 会话、任务、提交的扫描 API 形状相同（query + limit + cursor），用例用同一套逻辑验证升/降序
/// 全量遍历、游标续页与方向不匹配的拒绝；条目表有 fork 历史语义、文档表有历史点语义，各自单独覆盖。
#[derive(Clone, Copy)]
enum ScanTable {
    Conversations,
    Tasks,
    Submissions,
}

/// 对三张表之一执行一次分页扫描，取出 ID 列表与续页游标。
///
/// 三张表的查询类型不同，这里用 [`ScanTable`] 分派；`cursor` 与 `order` 的语义完全交给后端
/// （有游标时以游标为准，见各 `*Query::order` 字段的文档）。
async fn scan_id_page(
    storage: &dyn Storage,
    table: ScanTable,
    order: Option<ScanOrder>,
    cursor: Option<&Cursor>,
    limit: usize,
) -> Result<PageIds, BoxError> {
    match table {
        ScanTable::Conversations => {
            let query = ConversationQuery {
                order,
                ..ConversationQuery::default()
            };
            let page = storage
                .scan_conversations(&query, limit, cursor, context())
                .await?;
            Ok(PageIds {
                ids: page.items.iter().map(|item| item.id.get()).collect(),
                next: page.next,
            })
        }
        ScanTable::Tasks => {
            let query = TaskQuery {
                order,
                ..TaskQuery::default()
            };
            let page = storage.scan_tasks(&query, limit, cursor, context()).await?;
            Ok(PageIds {
                ids: page.items.iter().map(|item| item.id.get()).collect(),
                next: page.next,
            })
        }
        ScanTable::Submissions => {
            let query = SubmissionQuery {
                order,
                ..SubmissionQuery::default()
            };
            let page = storage
                .scan_submissions(&query, limit, cursor, context())
                .await?;
            Ok(PageIds {
                ids: page.items.iter().map(|item| item.id.get()).collect(),
                next: page.next,
            })
        }
    }
}

/// 以 `limit = 2` 逐页遍历整张表，收集全部 ID。
///
/// 第一页带 `order`，后续页只带游标、不再重复 `order`——这正是查询文档所说的“有游标时以游标
/// 为准”。用例用它断言全量顺序，因此也顺带验证了续页不依赖调用方再次传方向。
async fn scan_all_ids(
    storage: &dyn Storage,
    table: ScanTable,
    order: Option<ScanOrder>,
) -> Vec<i64> {
    let mut found = Vec::new();
    let mut page = unwrap_result(scan_id_page(storage, table, order, None, 2).await);
    found.extend(page.ids);
    while let Some(cursor) = page.next.clone() {
        page = unwrap_result(scan_id_page(storage, table, None, Some(&cursor), 2).await);
        found.extend(page.ids);
    }
    found
}

/// 用给定顺序和游标开始一次表扫描；用于验证游标方向不匹配时拒绝。
///
/// 返回未执行的 future 而不是结果，因为断言接口要求把“应当失败的操作”以 future 形式传入。
fn scan_with_cursor_future(
    storage: Arc<dyn Storage>,
    table: ScanTable,
    order: Option<ScanOrder>,
    cursor: Cursor,
) -> ConformanceFuture<'static> {
    Box::pin(async move {
        match table {
            ScanTable::Conversations => {
                let query = ConversationQuery {
                    order,
                    ..ConversationQuery::default()
                };
                storage
                    .scan_conversations(&query, 2, Some(&cursor), context())
                    .await
                    .map(|page| {
                        json!(
                            page.items
                                .iter()
                                .map(|item| item.id.get())
                                .collect::<Vec<_>>()
                        )
                    })
            }
            ScanTable::Tasks => {
                let query = TaskQuery {
                    order,
                    ..TaskQuery::default()
                };
                storage
                    .scan_tasks(&query, 2, Some(&cursor), context())
                    .await
                    .map(|page| {
                        json!(
                            page.items
                                .iter()
                                .map(|item| item.id.get())
                                .collect::<Vec<_>>()
                        )
                    })
            }
            ScanTable::Submissions => {
                let query = SubmissionQuery {
                    order,
                    ..SubmissionQuery::default()
                };
                storage
                    .scan_submissions(&query, 2, Some(&cursor), context())
                    .await
                    .map(|page| {
                        json!(
                            page.items
                                .iter()
                                .map(|item| item.id.get())
                                .collect::<Vec<_>>()
                        )
                    })
            }
        }
    })
}

/// 用 `limit = 1` 逐页遍历一个提交查询，收集全部 ID。
///
/// 用于断言过滤条件（会话、状态）与默认顺序；页大小固定为 1，保证多结果时确实走游标续页路径。
async fn submission_ids(storage: &dyn Storage, query: &SubmissionQuery) -> Vec<i64> {
    let mut found = Vec::new();
    let mut cursor: Option<Cursor> = None;
    loop {
        let page = unwrap_result(
            storage
                .scan_submissions(query, 1, cursor.as_ref(), context())
                .await,
        );
        found.extend(page.items.iter().map(|item| item.id.get()));
        match page.next {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    found
}

#[async_trait]
impl Storage for AdoptionChecked {
    async fn commit(
        &self,
        writes: &[StorageWrite],
        adopt: AdoptFn,
        commit_context: &dyn Context,
    ) -> Result<Seq, BoxError> {
        let adopted: Arc<Mutex<Vec<Seq>>> = Arc::new(Mutex::new(Vec::new()));
        let settled = Arc::new(AtomicBool::new(false));
        let recording_adopted = adopted.clone();
        let recording_settled = settled.clone();
        let recording_assertions = self.assertions.clone();
        let recording: AdoptFn = Box::new(move |seq: Seq| {
            recording_assertions.ok(
                !recording_settled.load(Ordering::SeqCst),
                Some("Storage.commit() adopted after it settled"),
            );
            recording_adopted.lock().unwrap().push(seq);
            adopt(seq);
        });
        let result = self.inner.commit(writes, recording, commit_context).await;
        match result {
            Ok(seq) => {
                settled.store(true, Ordering::SeqCst);
                let adopted = adopted.lock().unwrap();
                self.assertions.deep_equal(
                    &json!(adopted.iter().map(|seq| seq.get()).collect::<Vec<_>>()),
                    &json!([seq.get()]),
                );
                Ok(seq)
            }
            Err(error) => {
                settled.store(true, Ordering::SeqCst);
                let adopted = adopted.lock().unwrap();
                self.assertions.deep_equal(
                    &json!(adopted.iter().map(|seq| seq.get()).collect::<Vec<_>>()),
                    &json!([]),
                );
                Err(error)
            }
        }
    }

    // 除 `commit` 外的所有方法都只是转发：套件只通过这些调用的结果观察持久化状态，
    // 不需要为它们增加记录；校验点集中在 `commit` 的 adopt 契约上。
    async fn mint_id(&self, kind: IdKind) -> Result<i64, BoxError> {
        self.inner.mint_id(kind).await
    }

    async fn conversation(
        &self,
        id: ConversationId,
        context: &dyn Context,
    ) -> Result<Option<ConversationRecord>, BoxError> {
        self.inner.conversation(id, context).await
    }

    async fn scan_conversations(
        &self,
        query: &ConversationQuery,
        limit: usize,
        cursor: Option<&Cursor>,
        context: &dyn Context,
    ) -> Result<Page<ConversationRecord>, BoxError> {
        self.inner
            .scan_conversations(query, limit, cursor, context)
            .await
    }

    async fn entry(
        &self,
        id: EntryId,
        context: &dyn Context,
    ) -> Result<Option<StoredEntry>, BoxError> {
        self.inner.entry(id, context).await
    }

    async fn visible_entry(
        &self,
        conversation_id: ConversationId,
        id: EntryId,
        context: &dyn Context,
    ) -> Result<Option<StoredEntry>, BoxError> {
        self.inner.visible_entry(conversation_id, id, context).await
    }

    async fn find_latest_head_marker(
        &self,
        conversation_id: ConversationId,
        at_or_before_entry_id: Option<EntryId>,
        context: &dyn Context,
    ) -> Result<Option<EntryRecord>, BoxError> {
        self.inner
            .find_latest_head_marker(conversation_id, at_or_before_entry_id, context)
            .await
    }

    async fn scan_entries(
        &self,
        query: &EntryQuery,
        limit: usize,
        cursor: Option<&Cursor>,
        context: &dyn Context,
    ) -> Result<Page<EntryRecord>, BoxError> {
        self.inner.scan_entries(query, limit, cursor, context).await
    }

    async fn task(
        &self,
        id: TaskId,
        context: &dyn Context,
    ) -> Result<Option<TaskRecord>, BoxError> {
        self.inner.task(id, context).await
    }

    async fn scan_tasks(
        &self,
        query: &TaskQuery,
        limit: usize,
        cursor: Option<&Cursor>,
        context: &dyn Context,
    ) -> Result<Page<TaskRecord>, BoxError> {
        self.inner.scan_tasks(query, limit, cursor, context).await
    }

    async fn submission(
        &self,
        id: SubmissionId,
        context: &dyn Context,
    ) -> Result<Option<SubmissionRecord>, BoxError> {
        self.inner.submission(id, context).await
    }

    async fn scan_submissions(
        &self,
        query: &SubmissionQuery,
        limit: usize,
        cursor: Option<&Cursor>,
        context: &dyn Context,
    ) -> Result<Page<SubmissionRecord>, BoxError> {
        self.inner
            .scan_submissions(query, limit, cursor, context)
            .await
    }

    async fn submission_by_request(
        &self,
        conversation_id: ConversationId,
        request_id: &str,
        context: &dyn Context,
    ) -> Result<Option<SubmissionRecord>, BoxError> {
        self.inner
            .submission_by_request(conversation_id, request_id, context)
            .await
    }

    async fn find_document(
        &self,
        address: &DocumentAddress,
        at: DocumentPoint,
        context: &dyn Context,
    ) -> Result<Option<DocumentRecord>, BoxError> {
        self.inner.find_document(address, at, context).await
    }

    async fn document(
        &self,
        id: DocumentId,
        at: DocumentPoint,
        context: &dyn Context,
    ) -> Result<Option<StoredDocument>, BoxError> {
        self.inner.document(id, at, context).await
    }

    async fn scan_documents(
        &self,
        query: &DocumentQuery,
        limit: usize,
        cursor: Option<&Cursor>,
        context: &dyn Context,
    ) -> Result<Page<DocumentRecord>, BoxError> {
        self.inner
            .scan_documents(query, limit, cursor, context)
            .await
    }

    async fn close(&self, context: &dyn Context) {
        self.inner.close(context).await;
    }
}

/// 把一个测试体包装成可重复执行的 [`StorageConformanceCase`]。
///
/// `test` 收到包装后的 storage 与断言集合；`options` 在构造时克隆，所以返回的用例不借用
/// `options`，可以在 `options` 离开作用域后继续运行。每次执行 `run` 都会：
///
/// 1. 重新调用 `options.with_storage`，由 provider 准备一份 storage；
/// 2. 新建 `StorageUse` 闭包与 `AdoptionChecked` 包装，把同一份 `test` 与断言集合传进去；
/// 3. 等待 provider 完成对 `use` 的调用。
///
/// `StorageConformanceCase::run` 是 `CaseRun`（`Arc<dyn Fn() -> CaseFuture + Send + Sync>`），`run` 是
/// `Fn`，同一用例可被重复执行；本仓库的测试与 runner 适配各执行一次，是否重复由调用方决定。
/// `test` 闭包被克隆共享，用例体按无状态编写；storage 每次由 provider 重新提供，运行之间不共享
/// 存储状态。
fn conformance_case<F, Fut>(
    options: &StorageConformanceOptions,
    name: &str,
    test: F,
) -> StorageConformanceCase
where
    F: Fn(Arc<dyn Storage>, Arc<dyn StorageConformanceAssertions>) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let assertions = options.assertions.clone();
    let with_storage = options.with_storage.clone();
    let test = Arc::new(test);
    StorageConformanceCase {
        name: name.to_owned(),
        run: Arc::new(move || {
            let with_storage = with_storage.clone();
            let assertions = assertions.clone();
            let test = test.clone();
            Box::pin(async move {
                let use_assertions = assertions.clone();
                let use_storage: StorageUse = Arc::new(move |storage: Arc<dyn Storage>| {
                    let test = test.clone();
                    let assertions = use_assertions.clone();
                    let checked: Arc<dyn Storage> = Arc::new(AdoptionChecked {
                        inner: storage,
                        assertions: assertions.clone(),
                    });
                    let future: CaseFuture =
                        Box::pin(async move { test(checked, assertions).await });
                    future
                });
                with_storage(use_storage).await;
            })
        }),
    }
}

/// `createStorageConformance(options)`：构造完整的 26 个 Storage 一致性用例。
///
/// 输入是断言集合与 storage provider（见 [`StorageConformanceOptions`]）；输出是按 TS 顺序排列的
/// 用例列表。本函数不做任何 I/O、也不在构造期调用 provider：只有调用方执行
/// [`StorageConformanceCase::run`] 时才会去拿 storage。失败通过 `options.assertions` 报告。
///
/// 不变量：
///
/// - 用例名与顺序逐字来自 `packages/durable/src/testing/storage-conformance.ts`，由
///   `tests/storage_conformance.rs` 的 `STORAGE_CASE_NAMES` 锁定；新增/删除/重排会让该测试失败。
/// - 每个用例都经 `conformance_case` 包装，因此 `commit` 的 adopt 契约总是被检查。
/// - 用例假定 provider 每次 `use` 提供干净 storage（原因见模块文档）。
///
/// 结构：前半覆盖 ID 分配、提交原子性、深拷贝与表扫描/游标；中间是任务与提交记录；后半是文档的
/// 历史重建、复制、地址索引与生命周期原子性；最后是索引字符串、全局 ID 空间耗尽与 close 语义。
pub fn create_storage_conformance(
    options: &StorageConformanceOptions,
) -> Vec<StorageConformanceCase> {
    let mut cases: Vec<StorageConformanceCase> = Vec::new();

    // 组 1：根会话与全局 ID。根会话固定占用 ID 1，全局计数器从 2 开始；重复使用已有 ID 必须被拒绝。
    cases.push(conformance_case(
        options,
        "reserves ID 1 for the immutable root conversation",
        |storage, assert| async move {
            let assert = assert.as_ref();
            assert_strict(
                assert,
                json!(mint(storage.as_ref(), IdKind::Conversation).await),
                json!(2),
            );
            let root = create_root(storage.as_ref()).await;
            assert_strict(assert, json!(root.get()), json!(ROOT_CONVERSATION_ID.get()));
            let conversation =
                unwrap_result(storage.conversation(ROOT_CONVERSATION_ID, context()).await);
            assert_deep(
                assert,
                optional_json(conversation.as_ref()),
                json!({ "id": ROOT_CONVERSATION_ID.get() }),
            );
            let failing = storage.clone();
            assert_rejects(
                assert,
                Box::pin(async move {
                    let writes = [conversation_write(
                        json!({ "id": ROOT_CONVERSATION_ID.get() }),
                    )];
                    failing
                        .commit(&writes, adopt(), context())
                        .await
                        .map(|seq| json!(seq.get()))
                }),
                &format!(
                    "ID {} already belongs to conversation",
                    ROOT_CONVERSATION_ID.get()
                ),
            )
            .await;
        },
    ));

    // 组 2：提交契约。adopt 的时机与次数、adopt 回调里发起读取必须能看到本批次，以及混合表写入的
    // 全有或全无（失败批次的四张表写入都不能留下痕迹）。
    cases.push(conformance_case(
        options,
        "adopts a durable commit once before resolving, and never a failed one",
        |storage, assert| async move {
            let assert = assert.as_ref();
            let root = create_root(storage.as_ref()).await;
            let events: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
            let entry_id = mint_entry(storage.as_ref()).await;
            // 读取在 adopt 回调里发起（TS: `read = storage.entry(entryId, context)`），提交
            // 返回后才 await：批次在被 adopt 时就必须已经可见。
            let pending: Arc<Mutex<Option<StartedRead>>> = Arc::new(Mutex::new(None));
            let seq = {
                let writes = [entry_write(entry_id, root, "message", json!({}))];
                let adopt_events = events.clone();
                let adopt_pending = pending.clone();
                let read_storage = storage.clone();
                unwrap_result(
                    storage
                        .commit(
                            &writes,
                            Box::new(move |adopted: Seq| {
                                adopt_events
                                    .lock()
                                    .unwrap()
                                    .push(format!("adopt {}", adopted.get()));
                                *adopt_pending.lock().unwrap() =
                                    Some(start_read(&read_storage, entry_id));
                            }),
                            context(),
                        )
                        .await,
                )
            };
            events
                .lock()
                .unwrap()
                .push(format!("resolve {}", seq.get()));
            assert_deep(
                assert,
                json!(events.lock().unwrap().clone()),
                json!([
                    format!("adopt {}", seq.get()),
                    format!("resolve {}", seq.get())
                ]),
            );
            let started = pending
                .lock()
                .unwrap()
                .take()
                .expect("the adopt callback started the read");
            let read = unwrap_result(started.await_read().await);
            assert_strict(
                assert,
                json!(read.as_ref().map(|entry| entry.commit_seq.get())),
                json!(seq.get()),
            );

            events.lock().unwrap().clear();
            let failing = storage.clone();
            let adopt_events = events.clone();
            assert_rejects(
                assert,
                Box::pin(async move {
                    let writes = [conversation_write(json!({ "id": root.get() }))];
                    failing
                        .commit(
                            &writes,
                            Box::new(move |adopted: Seq| {
                                adopt_events
                                    .lock()
                                    .unwrap()
                                    .push(format!("adopt {}", adopted.get()));
                            }),
                            context(),
                        )
                        .await
                        .map(|seq| json!(seq.get()))
                }),
                &format!("ID {} already belongs to conversation", root.get()),
            )
            .await;
            assert_deep(assert, json!(events.lock().unwrap().clone()), json!([]));

            let next = {
                let id = mint_entry(storage.as_ref()).await;
                let writes = [entry_write(id, root, "message", json!({}))];
                let adopt_events = events.clone();
                unwrap_result(
                    storage
                        .commit(
                            &writes,
                            Box::new(move |adopted: Seq| {
                                adopt_events
                                    .lock()
                                    .unwrap()
                                    .push(format!("adopt {}", adopted.get()));
                            }),
                            context(),
                        )
                        .await,
                )
            };
            assert_deep(
                assert,
                json!(events.lock().unwrap().clone()),
                json!([format!("adopt {}", next.get())]),
            );
            assert_greater(assert, next.get(), seq.get());
        },
    ));

    cases.push(conformance_case(
        options,
        "commits mixed table writes atomically and rolls all of them back on failure",
        |storage, assert| async move {
            let assert = assert.as_ref();
            let root = create_root(storage.as_ref()).await;
            let entry_id = mint_entry(storage.as_ref()).await;
            let task_id = mint_task(storage.as_ref()).await;
            let submission_id = mint_submission(storage.as_ref()).await;
            let task = pending_task(task_id, root, "ready");
            let input = json!({
                "id": submission_id.get(),
                "conversationId": root.get(),
                "requestId": "request-1",
                "type": "input",
                "status": "placed",
                "entry": entry_id.get(),
            });
            let initial_seq = commit_writes(
                storage.as_ref(),
                &[
                    entry_write(entry_id, root, "user", json!({ "data": { "text": "hello" } })),
                    task_write(task.clone()),
                    submission_write(input.clone()),
                ],
            )
            .await;

            let stored = unwrap_result(storage.entry(entry_id, context()).await);
            assert_deep(
                assert,
                optional_json(stored.as_ref()),
                json!({
                    "entry": entry_json(entry_id, root, "user", json!({ "data": { "text": "hello" } })),
                    "commitSeq": initial_seq.get(),
                }),
            );
            let stored_task = unwrap_result(storage.task(task_id, context()).await);
            assert_deep(assert, optional_json(stored_task.as_ref()), task.clone());
            let stored_input = unwrap_result(storage.submission(submission_id, context()).await);
            assert_deep(assert, optional_json(stored_input.as_ref()), input.clone());

            // 失败批次同时改任务、提交、条目，并在最后重复提交根会话：只要那一条触发唯一性错误，
            // 前三张表的写入也必须一起回滚（回滚后逐表读取旧值，再确认后续提交仍能拿到更大的序号）。
            let transient_entry_id = mint_entry(storage.as_ref()).await;
            let running_task = merge(
                task.clone(),
                json!({ "state": { "status": "running", "checkpoint": { "phase": "effect" } } }),
            );
            let done_input = merge(
                input.clone(),
                json!({ "status": "done", "answer": transient_entry_id.get() }),
            );
            let failing = storage.clone();
            assert_rejects(
                assert,
                Box::pin(async move {
                    let writes = [
                        task_write(running_task),
                        submission_write(done_input),
                        entry_write(transient_entry_id, root, "assistant", json!({})),
                        conversation_write(json!({ "id": root.get() })),
                    ];
                    failing
                        .commit(&writes, adopt(), context())
                        .await
                        .map(|seq| json!(seq.get()))
                }),
                &format!("ID {} already belongs to conversation", root.get()),
            )
            .await;

            let stored_task = unwrap_result(storage.task(task_id, context()).await);
            assert_deep(assert, optional_json(stored_task.as_ref()), task);
            let stored_input = unwrap_result(storage.submission(submission_id, context()).await);
            assert_deep(assert, optional_json(stored_input.as_ref()), input);
            assert_undefined(
                assert,
                unwrap_result(storage.entry(transient_entry_id, context()).await).as_ref(),
            );
            let after_id = mint_entry(storage.as_ref()).await;
            let after_rollback_seq = commit_writes(
                storage.as_ref(),
                &[entry_write(after_id, root, "after-rollback", json!({}))],
            )
            .await;
            assert_greater(assert, after_rollback_seq.get(), initial_seq.get());
        },
    ));

    // 组 3：深拷贝。写入时传入的记录、读出的记录都不能与调用方共享可变 JSON；`__proto__`/
    // `constructor` 这类键按普通 own key 保存，不能被当成原型链操作。
    cases.push(conformance_case(
        options,
        "detaches retained writes and every returned record",
        |storage, assert| async move {
            let assert = assert.as_ref();
            let root = create_root(storage.as_ref()).await;
            let entry_id = mint_entry(storage.as_ref()).await;
            let task_id = mint_task(storage.as_ref()).await;
            let submission_id = mint_submission(storage.as_ref()).await;
            let mut entry_data = json!({ "nested": [1, 2] });
            let mut checkpoint = json!({ "phase": "ready", "nested": { "count": 1 } });
            let mut detail = json!({ "codes": ["initial"] });
            let stored_entry = entry_json(entry_id, root, "note", json!({ "data": entry_data.clone() }));
            let stored_task = merge(
                pending_task(task_id, root, "ready"),
                json!({ "state": { "status": "pending", "checkpoint": checkpoint.clone() } }),
            );
            let stored_input = json!({
                "id": submission_id.get(),
                "conversationId": root.get(),
                "type": "input",
                "status": "unanswered",
                "reason": "failed",
                "detail": detail.clone(),
            });
            commit_writes(
                storage.as_ref(),
                &[
                    write(json!({ "type": "entry", "value": stored_entry })),
                    task_write(stored_task),
                    submission_write(stored_input),
                ],
            )
            .await;

            entry_data["nested"].as_array_mut().unwrap().push(json!(3));
            checkpoint["nested"]["count"] = json!(2);
            detail["codes"].as_array_mut().unwrap().push(json!("mutated"));
            let stored = unwrap_result(storage.entry(entry_id, context()).await);
            assert_deep(
                assert,
                json!(stored.as_ref().and_then(|stored| stored.entry.data.clone())),
                json!({ "nested": [1, 2] }),
            );
            let stored_task = unwrap_result(storage.task(task_id, context()).await);
            assert_deep(
                assert,
                json!(stored_task.as_ref().map(|task| json_of(&task.state))),
                json!({ "status": "pending", "checkpoint": { "phase": "ready", "nested": { "count": 1 } } }),
            );
            let stored_input = unwrap_result(storage.submission(submission_id, context()).await);
            assert_deep(
                assert,
                json!(stored_input.as_ref().and_then(|submission| submission.detail.clone())),
                json!({ "codes": ["initial"] }),
            );

            let mut read_entry = unwrap_result(storage.entry(entry_id, context()).await)
                .expect("stored entry")
                .entry;
            if let Some(data) = read_entry.data.as_mut() {
                data["nested"].as_array_mut().unwrap().push(json!(9));
            }
            let mut read_task = unwrap_result(storage.task(task_id, context()).await).expect("stored task");
            if let crate::types::TaskState::Pending { checkpoint } = &mut read_task.state {
                checkpoint["nested"]["count"] = json!(9);
            }
            let mut read_input =
                unwrap_result(storage.submission(submission_id, context()).await).expect("stored submission");
            if let Some(detail) = read_input.detail.as_mut() {
                detail["codes"].as_array_mut().unwrap().push(json!("read mutation"));
            }

            let stored = unwrap_result(storage.entry(entry_id, context()).await);
            assert_deep(
                assert,
                json!(stored.as_ref().and_then(|stored| stored.entry.data.clone())),
                json!({ "nested": [1, 2] }),
            );
            let stored_task = unwrap_result(storage.task(task_id, context()).await);
            assert_deep(
                assert,
                json!(stored_task.as_ref().map(|task| json_of(&task.state))),
                json!({ "status": "pending", "checkpoint": { "phase": "ready", "nested": { "count": 1 } } }),
            );
            let stored_input = unwrap_result(storage.submission(submission_id, context()).await);
            assert_deep(
                assert,
                json!(stored_input.as_ref().and_then(|submission| submission.detail.clone())),
                json!({ "codes": ["initial"] }),
            );
        },
    ));

    cases.push(conformance_case(
        options,
        "detaches prototype-like JSON keys without changing object prototypes",
        |storage, assert| async move {
            let assert = assert.as_ref();
            let root = create_root(storage.as_ref()).await;
            let entry_id = mint_entry(storage.as_ref()).await;
            let mut data: Value = serde_json::from_str(
                r#"{"__proto__":{"polluted":false},"constructor":{"label":"stored"},"toString":"value"}"#,
            )
            .expect("prototype-like JSON");
            commit_writes(
                storage.as_ref(),
                &[entry_write(entry_id, root, "note", json!({ "data": data.clone() }))],
            )
            .await;

            data["__proto__"]["polluted"] = json!(true);
            data["constructor"]["label"] = json!("mutated");
            let first_read = unwrap_result(storage.entry(entry_id, context()).await)
                .expect("stored entry")
                .entry
                .data
                .expect("data");
            // JS 还检查 `Object.getPrototypeOf` 与全局原型未被污染；Rust 的 JSON 对象没有原型链，
            // 这里只保留“own key 原样保留”的实质断言。
            assert_deep(
                assert,
                first_read.get("__proto__").cloned().unwrap_or(Value::Null),
                json!({ "polluted": false }),
            );
            assert_deep(
                assert,
                first_read.get("constructor").cloned().unwrap_or(Value::Null),
                json!({ "label": "stored" }),
            );
            assert_strict(
                assert,
                first_read.get("toString").cloned().unwrap_or(Value::Null),
                json!("value"),
            );

            let mut first_read = first_read;
            first_read["__proto__"]["polluted"] = json!(true);
            let second_read = unwrap_result(storage.entry(entry_id, context()).await)
                .expect("stored entry")
                .entry
                .data
                .expect("data");
            assert_deep(
                assert,
                second_read.get("__proto__").cloned().unwrap_or(Value::Null),
                json!({ "polluted": false }),
            );
            assert_deep(
                assert,
                second_read.get("constructor").cloned().unwrap_or(Value::Null),
                json!({ "label": "stored" }),
            );
            assert_strict(
                assert,
                second_read.get("toString").cloned().unwrap_or(Value::Null),
                json!("value"),
            );
        },
    ));

    // 组 4：条目与扫描。乱序提交仍按 ID 建立索引；游标在新提交之后继续；会话按 ID 与 owner 边
    // 分页；跨 fork 祖先的历史视图按 fork 点截断，扫描/可见性/head marker 三者一致。
    cases.push(conformance_case(
        options,
        "indexes entries committed out of ID order",
        |storage, assert| async move {
            let assert = assert.as_ref();
            let root = create_root(storage.as_ref()).await;
            commit_writes(
                storage.as_ref(),
                &[
                    entry_write(id_from_number(30), root, "message", json!({})),
                    entry_write(id_from_number(10), root, "message", json!({})),
                    entry_write(
                        id_from_number(20),
                        root,
                        "marker",
                        json!({ "head": id_from_number::<crate::types::EntryBrand>(10).get() }),
                    ),
                ],
            )
            .await;
            let page = unwrap_result(
                storage
                    .scan_entries(&entry_query(root, None, None, None), 10, None, context())
                    .await,
            );
            assert_deep(
                assert,
                ids_of(&page.items, |item| item.id.get()),
                json!([30, 20, 10]),
            );
            let marker =
                unwrap_result(storage.find_latest_head_marker(root, None, context()).await);
            assert_strict(
                assert,
                json!(marker.as_ref().map(|entry| entry.id.get())),
                json!(20),
            );
        },
    ));

    cases.push(conformance_case(
        options,
        "continues an entry cursor below its last item after a newer commit",
        |storage, assert| async move {
            let assert = assert.as_ref();
            let root = create_root(storage.as_ref()).await;
            let oldest_id = mint_entry(storage.as_ref()).await;
            let middle_id = mint_entry(storage.as_ref()).await;
            let newest_id = mint_entry(storage.as_ref()).await;
            commit_writes(
                storage.as_ref(),
                &[
                    entry_write(oldest_id, root, "message", json!({})),
                    entry_write(middle_id, root, "message", json!({})),
                    entry_write(newest_id, root, "message", json!({})),
                ],
            )
            .await;

            let first = unwrap_result(
                storage
                    .scan_entries(&entry_query(root, None, None, None), 2, None, context())
                    .await,
            );
            assert_deep(
                assert,
                ids_of(&first.items, |item| item.id.get()),
                json!([newest_id.get(), middle_id.get()]),
            );
            let appended_id = mint_entry(storage.as_ref()).await;
            commit_writes(
                storage.as_ref(),
                &[entry_write(appended_id, root, "message", json!({}))],
            )
            .await;
            let second = unwrap_result(
                storage
                    .scan_entries(
                        &entry_query(root, None, None, None),
                        2,
                        first.next.as_ref(),
                        context(),
                    )
                    .await,
            );
            assert_deep(
                assert,
                ids_of(&second.items, |item| item.id.get()),
                json!([oldest_id.get()]),
            );
            assert_undefined(assert, second.next.as_ref());
        },
    ));

    cases.push(conformance_case(
        options,
        "paginates conversations by opaque cursor in ascending ID order",
        |storage, assert| async move {
            let assert = assert.as_ref();
            let root = create_root(storage.as_ref()).await;
            let second_id = mint_conversation(storage.as_ref()).await;
            let third_id = mint_conversation(storage.as_ref()).await;
            commit_writes(
                storage.as_ref(),
                &[
                    conversation_write(json!({ "id": third_id.get() })),
                    conversation_write(json!({ "id": second_id.get() })),
                ],
            )
            .await;

            let first = unwrap_result(
                storage
                    .scan_conversations(&ConversationQuery::default(), 2, None, context())
                    .await,
            );
            assert_deep(
                assert,
                ids_of(&first.items, |item| item.id.get()),
                json!([root.get(), second_id.get()]),
            );
            let cursor = first.next.clone().expect("first page has a cursor");
            // `JSON.parse(JSON.stringify(cursor))`：游标只是让调用方原样回传的 JSON。
            let round_tripped: Cursor =
                serde_json::from_value(json_of(&cursor)).expect("cursor round trip");
            let second = unwrap_result(
                storage
                    .scan_conversations(
                        &ConversationQuery::default(),
                        2,
                        Some(&round_tripped),
                        context(),
                    )
                    .await,
            );
            assert_deep(
                assert,
                ids_of(&second.items, |item| item.id.get()),
                json!([third_id.get()]),
            );
            assert_undefined(assert, second.next.as_ref());
        },
    ));

    cases.push(conformance_case(
        options,
        "filters and pages conversations by durable owner edges",
        |storage, assert| async move {
            let assert = assert.as_ref();
            let root = create_root(storage.as_ref()).await;
            let other_owner_id = mint_conversation(storage.as_ref()).await;
            let first_task_id = mint_task(storage.as_ref()).await;
            let second_task_id = mint_task(storage.as_ref()).await;
            let first_id = mint_conversation(storage.as_ref()).await;
            let second_id = mint_conversation(storage.as_ref()).await;
            let third_id = mint_conversation(storage.as_ref()).await;
            commit_writes(
                storage.as_ref(),
                &[
                    conversation_write(json!({ "id": other_owner_id.get() })),
                    conversation_write(json!({
                        "id": first_id.get(),
                        "owner": { "conversationId": root.get(), "taskId": first_task_id.get() },
                    })),
                    conversation_write(json!({
                        "id": second_id.get(),
                        "owner": { "conversationId": root.get(), "taskId": second_task_id.get() },
                    })),
                    conversation_write(json!({
                        "id": third_id.get(),
                        "owner": { "conversationId": other_owner_id.get(), "taskId": first_task_id.get() },
                    })),
                ],
            )
            .await;

            let first = unwrap_result(
                storage
                    .scan_conversations(
                        &ConversationQuery {
                            owner_conversation_id: Some(root),
                            ..ConversationQuery::default()
                        },
                        1,
                        None,
                        context(),
                    )
                    .await,
            );
            assert_deep(
                assert,
                ids_of(&first.items, |item| item.id.get()),
                json!([first_id.get()]),
            );
            let cursor = first.next.clone().expect("first page has a cursor");
            let second = unwrap_result(
                storage
                    .scan_conversations(
                        &ConversationQuery {
                            owner_conversation_id: Some(root),
                            ..ConversationQuery::default()
                        },
                        1,
                        Some(&cursor),
                        context(),
                    )
                    .await,
            );
            assert_deep(
                assert,
                ids_of(&second.items, |item| item.id.get()),
                json!([second_id.get()]),
            );
            assert_undefined(assert, second.next.as_ref());
            let by_task = unwrap_result(
                storage
                    .scan_conversations(
                        &ConversationQuery {
                            owner_task_id: Some(first_task_id),
                            ..ConversationQuery::default()
                        },
                        10,
                        None,
                        context(),
                    )
                    .await,
            );
            assert_deep(
                assert,
                ids_of(&by_task.items, |item| item.id.get()),
                json!([first_id.get(), third_id.get()]),
            );
            // 两个 owner 过滤条件同时给出时取交集（AND）：first_id 同时满足，third_id 只满足 task 边。
            let both = unwrap_result(
                storage
                    .scan_conversations(
                        &ConversationQuery {
                            owner_conversation_id: Some(root),
                            owner_task_id: Some(first_task_id),
                            order: None,
                        },
                        10,
                        None,
                        context(),
                    )
                    .await,
            );
            assert_deep(
                assert,
                ids_of(&both.items, |item| item.id.get()),
                json!([first_id.get()]),
            );
        },
    ));

    cases.push(conformance_case(
        options,
        "scans deep fork history newest-first through every ancestor cap",
        |storage, assert| async move {
            let assert = assert.as_ref();
            let root = create_root(storage.as_ref()).await;
            // 场景树（括号里是 fork 点）：
            //   root:       [root_first, root_fork_point(marker→root_first), excluded_same_commit]
            //               之后又追加 excluded_later
            //   child:      在 root_fork_point 处 fork，[child_fork_point, child_excluded]
            //               之后又追加 child_excluded_later
            //   grandchild: 在 child_fork_point 处 fork，[grandchild_head(marker→自身), grandchild_tail]
            //
            // 可见历史（新→旧）因此是 grandchild 段、child 段、root 段，每段都在自己的 fork 点
            // 截止；同一次提交里 fork 点之后的条目、以及 fork 之后追加的条目都不可见。
            let root_first = mint_entry(storage.as_ref()).await;
            let root_fork_point = mint_entry(storage.as_ref()).await;
            let root_excluded_same_commit = mint_entry(storage.as_ref()).await;
            let root_entries_seq = commit_writes(
                storage.as_ref(),
                &[
                    entry_write(root_first, root, "message", json!({})),
                    entry_write(
                        root_fork_point,
                        root,
                        "marker",
                        json!({ "head": root_first.get() }),
                    ),
                    entry_write(root_excluded_same_commit, root, "message", json!({})),
                ],
            )
            .await;
            let child_id = mint_conversation(storage.as_ref()).await;
            commit_writes(
                storage.as_ref(),
                &[conversation_write(json!({
                    "id": child_id.get(),
                    "parent": { "conversationId": root.get(), "at": root_fork_point.get() },
                }))],
            )
            .await;
            let child_fork_point = mint_entry(storage.as_ref()).await;
            let child_excluded = mint_entry(storage.as_ref()).await;
            commit_writes(
                storage.as_ref(),
                &[
                    entry_write(child_fork_point, child_id, "note", json!({})),
                    entry_write(child_excluded, child_id, "message", json!({})),
                ],
            )
            .await;
            let root_excluded_later = mint_entry(storage.as_ref()).await;
            commit_writes(
                storage.as_ref(),
                &[entry_write(root_excluded_later, root, "message", json!({}))],
            )
            .await;
            let grandchild_id = mint_conversation(storage.as_ref()).await;
            commit_writes(
                storage.as_ref(),
                &[conversation_write(json!({
                    "id": grandchild_id.get(),
                    "parent": { "conversationId": child_id.get(), "at": child_fork_point.get() },
                }))],
            )
            .await;
            let grandchild_head = mint_entry(storage.as_ref()).await;
            let grandchild_tail = mint_entry(storage.as_ref()).await;
            let grandchild_entries_seq = commit_writes(
                storage.as_ref(),
                &[
                    entry_write(
                        grandchild_head,
                        grandchild_id,
                        "marker",
                        json!({ "head": grandchild_head.get() }),
                    ),
                    entry_write(grandchild_tail, grandchild_id, "message", json!({})),
                ],
            )
            .await;
            let child_excluded_later = mint_entry(storage.as_ref()).await;
            commit_writes(
                storage.as_ref(),
                &[entry_write(
                    child_excluded_later,
                    child_id,
                    "message",
                    json!({}),
                )],
            )
            .await;

            let first = unwrap_result(
                storage
                    .scan_entries(
                        &entry_query(grandchild_id, None, None, None),
                        2,
                        None,
                        context(),
                    )
                    .await,
            );
            assert_deep(
                assert,
                ids_of(&first.items, |item| item.id.get()),
                json!([grandchild_tail.get(), grandchild_head.get()]),
            );
            let second = unwrap_result(
                storage
                    .scan_entries(
                        &entry_query(grandchild_id, None, None, None),
                        2,
                        first.next.as_ref(),
                        context(),
                    )
                    .await,
            );
            assert_deep(
                assert,
                ids_of(&second.items, |item| item.id.get()),
                json!([child_fork_point.get(), root_fork_point.get()]),
            );
            let third = unwrap_result(
                storage
                    .scan_entries(
                        &entry_query(grandchild_id, None, None, None),
                        2,
                        second.next.as_ref(),
                        context(),
                    )
                    .await,
            );
            assert_deep(
                assert,
                ids_of(&third.items, |item| item.id.get()),
                json!([root_first.get()]),
            );
            assert_undefined(assert, third.next.as_ref());

            // 从旧到新：根的分段、再每层 fork 的分段，各自在上一个 fork 点截断。
            let ascending = entry_query(grandchild_id, None, None, Some(ScanOrder::Ascending));
            let first_up =
                unwrap_result(storage.scan_entries(&ascending, 2, None, context()).await);
            assert_deep(
                assert,
                ids_of(&first_up.items, |item| item.id.get()),
                json!([root_first.get(), root_fork_point.get()]),
            );
            let second_up = unwrap_result(
                storage
                    .scan_entries(&ascending, 2, first_up.next.as_ref(), context())
                    .await,
            );
            assert_deep(
                assert,
                ids_of(&second_up.items, |item| item.id.get()),
                json!([child_fork_point.get(), grandchild_head.get()]),
            );
            // 这一页不传 order（`None`，即默认降序），但游标来自升序扫描：续页仍按游标的方向
            // 升序继续。与后面“显式 Descending + 升序游标被拒绝”合起来，说明只有显式方向与游标
            // 冲突才是错误，缺省方向不参与冲突判定。
            let third_up = unwrap_result(
                storage
                    .scan_entries(
                        &entry_query(grandchild_id, None, None, None),
                        2,
                        second_up.next.as_ref(),
                        context(),
                    )
                    .await,
            );
            assert_deep(
                assert,
                ids_of(&third_up.items, |item| item.id.get()),
                json!([grandchild_tail.get()]),
            );
            assert_undefined(assert, third_up.next.as_ref());
            let bounded = unwrap_result(
                storage
                    .scan_entries(
                        &entry_query(
                            grandchild_id,
                            Some(root_fork_point),
                            Some(child_fork_point),
                            Some(ScanOrder::Ascending),
                        ),
                        10,
                        None,
                        context(),
                    )
                    .await,
            );
            assert_deep(
                assert,
                ids_of(&bounded.items, |item| item.id.get()),
                json!([root_fork_point.get(), child_fork_point.get()]),
            );
            let storage2 = storage.clone();
            let cursor = first_up.next.clone().expect("ascending cursor");
            assert_rejects(
                assert,
                Box::pin(async move {
                    storage2
                        .scan_entries(
                            &entry_query(grandchild_id, None, None, Some(ScanOrder::Descending)),
                            2,
                            Some(&cursor),
                            context(),
                        )
                        .await
                        .map(|page| {
                            json!(
                                page.items
                                    .iter()
                                    .map(|item| item.id.get())
                                    .collect::<Vec<_>>()
                            )
                        })
                }),
                "cursor",
            )
            .await;

            let current_marker = unwrap_result(
                storage
                    .find_latest_head_marker(grandchild_id, None, context())
                    .await,
            );
            assert_strict(
                assert,
                json!(current_marker.as_ref().map(|entry| entry.id.get())),
                json!(grandchild_head.get()),
            );
            assert_strict(
                assert,
                json!(
                    current_marker
                        .as_ref()
                        .and_then(|entry| entry.head.map(|head| head.get()))
                ),
                json!(grandchild_head.get()),
            );
            let historical_marker = unwrap_result(
                storage
                    .find_latest_head_marker(grandchild_id, Some(child_fork_point), context())
                    .await,
            );
            assert_strict(
                assert,
                json!(historical_marker.as_ref().map(|entry| entry.id.get())),
                json!(root_fork_point.get()),
            );
            assert_strict(
                assert,
                json!(
                    historical_marker
                        .as_ref()
                        .and_then(|entry| entry.head.map(|head| head.get()))
                ),
                json!(root_first.get()),
            );
            assert_undefined(
                assert,
                unwrap_result(
                    storage
                        .find_latest_head_marker(grandchild_id, Some(root_first), context())
                        .await,
                )
                .as_ref(),
            );

            let active_first = unwrap_result(
                storage
                    .scan_entries(
                        &entry_query(grandchild_id, Some(grandchild_head), None, None),
                        1,
                        None,
                        context(),
                    )
                    .await,
            );
            assert_deep(
                assert,
                ids_of(&active_first.items, |item| item.id.get()),
                json!([grandchild_tail.get()]),
            );
            assert_ok(
                assert,
                active_first.next.is_some(),
                "active scan has a cursor",
            );
            let active_second = unwrap_result(
                storage
                    .scan_entries(
                        &entry_query(grandchild_id, Some(grandchild_head), None, None),
                        1,
                        active_first.next.as_ref(),
                        context(),
                    )
                    .await,
            );
            assert_deep(
                assert,
                ids_of(&active_second.items, |item| item.id.get()),
                json!([grandchild_head.get()]),
            );
            assert_undefined(assert, active_second.next.as_ref());

            let ranged = unwrap_result(
                storage
                    .scan_entries(
                        &entry_query(
                            grandchild_id,
                            Some(root_first),
                            Some(child_fork_point),
                            None,
                        ),
                        10,
                        None,
                        context(),
                    )
                    .await,
            );
            assert_deep(
                assert,
                ids_of(&ranged.items, |item| item.id.get()),
                json!([
                    child_fork_point.get(),
                    root_fork_point.get(),
                    root_first.get()
                ]),
            );

            let stored = unwrap_result(storage.entry(root_first, context()).await);
            assert_deep(
                assert,
                optional_json(stored.as_ref()),
                json!({
                    "entry": entry_json(root_first, root, "message", json!({})),
                    "commitSeq": root_entries_seq.get(),
                }),
            );
            assert_strict(
                assert,
                json!(
                    unwrap_result(storage.entry(root_fork_point, context()).await)
                        .map(|entry| entry.commit_seq.get())
                ),
                json!(root_entries_seq.get()),
            );
            assert_strict(
                assert,
                json!(
                    unwrap_result(storage.entry(grandchild_head, context()).await)
                        .map(|entry| entry.commit_seq.get())
                ),
                json!(grandchild_entries_seq.get()),
            );
            assert_strict(
                assert,
                json!(
                    unwrap_result(storage.entry(grandchild_tail, context()).await)
                        .map(|entry| entry.commit_seq.get())
                ),
                json!(grandchild_entries_seq.get()),
            );
            assert_undefined(
                assert,
                unwrap_result(
                    storage
                        .entry(
                            id_from_number::<crate::types::EntryBrand>(999_999),
                            context(),
                        )
                        .await,
                )
                .as_ref(),
            );

            let visible = unwrap_result(
                storage
                    .visible_entry(grandchild_id, root_first, context())
                    .await,
            );
            assert_deep(
                assert,
                optional_json(visible.as_ref()),
                json!({
                    "entry": entry_json(root_first, root, "message", json!({})),
                    "commitSeq": root_entries_seq.get(),
                }),
            );
            assert_strict(
                assert,
                json!(
                    unwrap_result(
                        storage
                            .visible_entry(grandchild_id, child_fork_point, context())
                            .await
                    )
                    .map(|entry| entry.entry.conversation_id.get())
                ),
                json!(child_id.get()),
            );
            assert_strict(
                assert,
                json!(
                    unwrap_result(
                        storage
                            .visible_entry(grandchild_id, grandchild_tail, context())
                            .await
                    )
                    .map(|entry| entry.commit_seq.get())
                ),
                json!(grandchild_entries_seq.get()),
            );
            for excluded in [
                root_excluded_same_commit,
                root_excluded_later,
                child_excluded,
                child_excluded_later,
            ] {
                assert_undefined(
                    assert,
                    unwrap_result(
                        storage
                            .visible_entry(grandchild_id, excluded, context())
                            .await,
                    )
                    .as_ref(),
                );
            }
            assert_undefined(
                assert,
                unwrap_result(
                    storage
                        .visible_entry(root, grandchild_head, context())
                        .await,
                )
                .as_ref(),
            );
            assert_undefined(
                assert,
                unwrap_result(
                    storage
                        .visible_entry(
                            grandchild_id,
                            id_from_number::<crate::types::EntryBrand>(999_999),
                            context(),
                        )
                        .await,
                )
                .as_ref(),
            );
            let storage2 = storage.clone();
            assert_rejects(
                assert,
                Box::pin(async move {
                    storage2
                        .visible_entry(
                            id_from_number::<crate::types::ConversationBrand>(999_999),
                            root_first,
                            context(),
                        )
                        .await
                        .map(|entry| json!(entry.is_some()))
                }),
                "Unknown conversation",
            )
            .await;
            let storage2 = storage.clone();
            assert_rejects(
                assert,
                Box::pin(async move {
                    storage2
                        .scan_entries(
                            &entry_query(
                                id_from_number::<crate::types::ConversationBrand>(999_999),
                                None,
                                None,
                                None,
                            ),
                            10,
                            None,
                            context(),
                        )
                        .await
                        .map(|page| json!(page.items.len()))
                }),
                "Unknown conversation",
            )
            .await;
        },
    ));

    // 组 5：表扫描、任务与提交记录。三张表的升/降序与游标方向；任务记录是整体替换（含状态迁移），
    // 提交的 requestId 是“会话内去重键”，两者都按完整记录覆盖并支持多条件过滤。
    cases.push(conformance_case(
        options,
        "scans tables in either ID order and continues a cursor in its order",
        |storage, assert| async move {
            let assert = assert.as_ref();
            let root = create_root(storage.as_ref()).await;
            let mut conversation_ids = vec![root.get()];
            let mut task_ids: Vec<i64> = Vec::new();
            let mut submission_ids: Vec<i64> = Vec::new();
            for _ in 0..3 {
                let conversation_id = mint_conversation(storage.as_ref()).await;
                let task_id = mint_task(storage.as_ref()).await;
                let submission_id = mint_submission(storage.as_ref()).await;
                commit_writes(
                    storage.as_ref(),
                    &[
                        conversation_write(json!({ "id": conversation_id.get() })),
                        task_write(pending_task(task_id, root, "ready")),
                        submission_write(json!({
                            "id": submission_id.get(),
                            "conversationId": root.get(),
                            "type": "input",
                            "status": "queued",
                        })),
                    ],
                )
                .await;
                conversation_ids.push(conversation_id.get());
                task_ids.push(task_id.get());
                submission_ids.push(submission_id.get());
            }
            for (table, ids) in [
                (ScanTable::Conversations, conversation_ids),
                (ScanTable::Tasks, task_ids),
                (ScanTable::Submissions, submission_ids),
            ] {
                let reversed: Vec<i64> = ids.iter().rev().copied().collect();
                assert_deep(
                    assert,
                    json!(scan_all_ids(storage.as_ref(), table, None).await),
                    json!(ids),
                );
                assert_deep(
                    assert,
                    json!(scan_all_ids(storage.as_ref(), table, Some(ScanOrder::Ascending)).await),
                    json!(ids),
                );
                assert_deep(
                    assert,
                    json!(scan_all_ids(storage.as_ref(), table, Some(ScanOrder::Descending)).await),
                    json!(reversed),
                );
                let descending = unwrap_result(
                    scan_id_page(
                        storage.as_ref(),
                        table,
                        Some(ScanOrder::Descending),
                        None,
                        2,
                    )
                    .await,
                );
                let cursor = descending.next.clone().expect("descending cursor");
                let second = unwrap_result(
                    scan_id_page(
                        storage.as_ref(),
                        table,
                        Some(ScanOrder::Descending),
                        Some(&cursor),
                        2,
                    )
                    .await,
                );
                // JS `reversed.slice(2, 4)` 会把越界的结束下标截到长度（会话表 4 条，任务/提交表 3 条）。
                let start = std::cmp::min(2, reversed.len());
                let end = std::cmp::min(4, reversed.len());
                assert_deep(assert, json!(second.ids), json!(&reversed[start..end]));
                let storage2 = storage.clone();
                assert_rejects(
                    assert,
                    Box::pin(async move {
                        scan_with_cursor_future(storage2, table, Some(ScanOrder::Ascending), cursor)
                            .await
                    }),
                    "cursor",
                )
                .await;
            }
        },
    ));

    cases.push(conformance_case(
        options,
        "replaces complete task records and pages filtered task scans",
        |storage, assert| async move {
            let assert = assert.as_ref();
            let root = create_root(storage.as_ref()).await;
            let first_id = mint_task(storage.as_ref()).await;
            let second_id = mint_task(storage.as_ref()).await;
            let third_id = mint_task(storage.as_ref()).await;
            let first = merge(
                pending_task(first_id, root, "ready"),
                json!({ "memos": { "winner": "first" } }),
            );
            let second = merge(
                pending_task(second_id, root, "ready"),
                json!({ "background": true }),
            );
            let third = merge(
                pending_task(third_id, root, "ready"),
                json!({ "abortRequested": true }),
            );
            commit_writes(
                storage.as_ref(),
                &[
                    task_write(first.clone()),
                    task_write(second.clone()),
                    task_write(third.clone()),
                ],
            )
            .await;

            let running = merge(
                first.clone(),
                json!({
                    "state": { "status": "running", "checkpoint": { "phase": "effect", "attempt": 1 } },
                    "abortRequested": true,
                }),
            );
            commit_writes(storage.as_ref(), &[task_write(running.clone())]).await;
            let stored = unwrap_result(storage.task(first_id, context()).await);
            assert_deep(assert, optional_json(stored.as_ref()), running.clone());
            let terminal = json!({
                "id": first_id.get(),
                "conversationId": root.get(),
                "kind": "test.task",
                "version": 1,
                "input": { "value": first_id.get() },
                "state": {
                    "status": "terminal",
                    "outcome": { "status": "completed", "result": { "entryId": 99 } },
                },
                "background": false,
                "abortRequested": true,
            });
            commit_writes(storage.as_ref(), &[task_write(terminal.clone())]).await;
            let stored = unwrap_result(storage.task(first_id, context()).await);
            assert_deep(assert, optional_json(stored.as_ref()), terminal.clone());

            let pending_page = unwrap_result(
                storage
                    .scan_tasks(
                        &TaskQuery {
                            status: Some(TaskStatus::Pending),
                            ..TaskQuery::default()
                        },
                        1,
                        None,
                        context(),
                    )
                    .await,
            );
            assert_deep(
                assert,
                ids_of(&pending_page.items, |item| item.id.get()),
                json!([second_id.get()]),
            );
            assert_ok(assert, pending_page.next.is_some(), "pending scan has a cursor");
            let pending_second = unwrap_result(
                storage
                    .scan_tasks(
                        &TaskQuery {
                            status: Some(TaskStatus::Pending),
                            ..TaskQuery::default()
                        },
                        1,
                        pending_page.next.as_ref(),
                        context(),
                    )
                    .await,
            );
            assert_deep(
                assert,
                ids_of(&pending_second.items, |item| item.id.get()),
                json!([third_id.get()]),
            );
            let terminal_page = unwrap_result(
                storage
                    .scan_tasks(
                        &TaskQuery {
                            status: Some(TaskStatus::Terminal),
                            abort_requested: Some(true),
                            ..TaskQuery::default()
                        },
                        10,
                        None,
                        context(),
                    )
                    .await,
            );
            assert_deep(
                assert,
                json!(terminal_page.items.iter().map(json_of).collect::<Vec<_>>()),
                json!([terminal]),
            );
            let background_page = unwrap_result(
                storage
                    .scan_tasks(
                        &TaskQuery {
                            background: Some(true),
                            ..TaskQuery::default()
                        },
                        10,
                        None,
                        context(),
                    )
                    .await,
            );
            assert_deep(
                assert,
                ids_of(&background_page.items, |item| item.id.get()),
                json!([second_id.get()]),
            );
        },
    ));

    cases.push(conformance_case(
        options,
        "stores owners and scans waiting and completing tasks by status",
        |storage, assert| async move {
            let assert = assert.as_ref();
            let root = create_root(storage.as_ref()).await;
            let owner_id = mint_task(storage.as_ref()).await;
            let waiting_id = mint_task(storage.as_ref()).await;
            let completing_id = mint_task(storage.as_ref()).await;
            let owner = pending_task(owner_id, root, "ready");
            let waiting = merge(
                pending_task(waiting_id, root, "ready"),
                json!({
                    "owner": owner_id.get(),
                    "state": {
                        "status": "waiting",
                        "checkpoint": { "phase": "next" },
                        "on": [owner_id.get()],
                        "policy": "allSettled",
                    },
                    "memos": { "kept": true },
                }),
            );
            let completing_base = json!({
                "id": completing_id.get(),
                "conversationId": root.get(),
                "kind": "test.task",
                "version": 1,
                "input": { "value": completing_id.get() },
                "background": false,
                "abortRequested": false,
            });
            let completing = merge(
                completing_base,
                json!({
                    "owner": owner_id.get(),
                    "state": {
                        "status": "completing",
                        "outcome": { "status": "failed", "error": { "message": "held" } },
                    },
                }),
            );
            commit_writes(
                storage.as_ref(),
                &[
                    task_write(owner.clone()),
                    task_write(waiting.clone()),
                    task_write(completing.clone()),
                ],
            )
            .await;
            let stored = unwrap_result(storage.task(waiting_id, context()).await);
            assert_deep(assert, optional_json(stored.as_ref()), waiting.clone());
            let stored = unwrap_result(storage.task(completing_id, context()).await);
            assert_deep(assert, optional_json(stored.as_ref()), completing.clone());

            let by_status = |status: TaskStatus| {
                let storage = storage.clone();
                async move {
                    unwrap_result(
                        storage
                            .scan_tasks(
                                &TaskQuery {
                                    status: Some(status),
                                    ..TaskQuery::default()
                                },
                                10,
                                None,
                                context(),
                            )
                            .await,
                    )
                    .items
                }
            };
            assert_deep(
                assert,
                json!(
                    by_status(TaskStatus::Waiting)
                        .await
                        .iter()
                        .map(json_of)
                        .collect::<Vec<_>>()
                ),
                json!([waiting]),
            );
            assert_deep(
                assert,
                json!(
                    by_status(TaskStatus::Completing)
                        .await
                        .iter()
                        .map(json_of)
                        .collect::<Vec<_>>()
                ),
                json!([completing.clone()]),
            );
            assert_deep(
                assert,
                ids_of(&by_status(TaskStatus::Pending).await, |item| item.id.get()),
                json!([owner_id.get()]),
            );
            let outcome = completing["state"]["outcome"].clone();
            let terminal = merge(
                completing.clone(),
                json!({ "state": { "status": "terminal", "outcome": outcome } }),
            );
            commit_writes(storage.as_ref(), &[task_write(terminal.clone())]).await;
            assert_deep(
                assert,
                json!(
                    by_status(TaskStatus::Completing)
                        .await
                        .iter()
                        .map(json_of)
                        .collect::<Vec<_>>()
                ),
                json!([]),
            );
            assert_deep(
                assert,
                json!(
                    by_status(TaskStatus::Terminal)
                        .await
                        .iter()
                        .map(json_of)
                        .collect::<Vec<_>>()
                ),
                json!([terminal]),
            );
        },
    ));

    cases.push(conformance_case(
        options,
        "stores and scans sleeping tasks by status",
        |storage, assert| async move {
            let assert = assert.as_ref();
            let root = create_root(storage.as_ref()).await;
            let sleeping_id = mint_task(storage.as_ref()).await;
            let sleeping = merge(
                pending_task(sleeping_id, root, "ready"),
                json!({
                    "state": {
                        "status": "sleeping",
                        "checkpoint": { "phase": "next" },
                        "until": 61_000,
                    },
                    "memos": { "kept": true },
                }),
            );
            commit_writes(storage.as_ref(), &[task_write(sleeping.clone())]).await;
            let stored = unwrap_result(storage.task(sleeping_id, context()).await);
            assert_deep(assert, optional_json(stored.as_ref()), sleeping.clone());
            let scan = |status: TaskStatus| {
                let storage = storage.clone();
                async move {
                    unwrap_result(
                        storage
                            .scan_tasks(
                                &TaskQuery {
                                    status: Some(status),
                                    ..TaskQuery::default()
                                },
                                10,
                                None,
                                context(),
                            )
                            .await,
                    )
                    .items
                }
            };
            assert_deep(
                assert,
                json!(
                    scan(TaskStatus::Sleeping)
                        .await
                        .iter()
                        .map(json_of)
                        .collect::<Vec<_>>()
                ),
                json!([sleeping.clone()]),
            );
            let running = merge(
                sleeping.clone(),
                json!({ "state": { "status": "running", "checkpoint": { "phase": "next" } } }),
            );
            commit_writes(storage.as_ref(), &[task_write(running.clone())]).await;
            assert_deep(
                assert,
                json!(
                    scan(TaskStatus::Sleeping)
                        .await
                        .iter()
                        .map(json_of)
                        .collect::<Vec<_>>()
                ),
                json!([]),
            );
            assert_deep(
                assert,
                json!(
                    scan(TaskStatus::Running)
                        .await
                        .iter()
                        .map(json_of)
                        .collect::<Vec<_>>()
                ),
                json!([running]),
            );
        },
    ));

    cases.push(conformance_case(
        options,
        "indexes request IDs per conversation and replaces complete submission records",
        |storage, assert| async move {
            let assert = assert.as_ref();
            let root = create_root(storage.as_ref()).await;
            let second_conversation_id = mint_conversation(storage.as_ref()).await;
            commit_writes(
                storage.as_ref(),
                &[conversation_write(
                    json!({ "id": second_conversation_id.get() }),
                )],
            )
            .await;
            let first_id = mint_submission(storage.as_ref()).await;
            let second_id = mint_submission(storage.as_ref()).await;
            let other_conversation_id = mint_submission(storage.as_ref()).await;
            let first = json!({
                "id": first_id.get(),
                "conversationId": root.get(),
                "requestId": "same",
                "type": "input",
                "status": "queued",
            });
            let second = json!({
                "id": second_id.get(),
                "conversationId": root.get(),
                "requestId": "other",
                "type": "input",
                "status": "queued",
            });
            let other_conversation = json!({
                "id": other_conversation_id.get(),
                "conversationId": second_conversation_id.get(),
                "requestId": "same",
                "type": "input",
                "status": "queued",
            });
            commit_writes(
                storage.as_ref(),
                &[
                    submission_write(first.clone()),
                    submission_write(second.clone()),
                    submission_write(other_conversation.clone()),
                ],
            )
            .await;
            assert_deep(
                assert,
                optional_json(
                    unwrap_result(storage.submission_by_request(root, "same", context()).await)
                        .as_ref(),
                ),
                first.clone(),
            );
            assert_deep(
                assert,
                optional_json(
                    unwrap_result(
                        storage
                            .submission_by_request(second_conversation_id, "same", context())
                            .await,
                    )
                    .as_ref(),
                ),
                other_conversation.clone(),
            );

            let placed_second = merge(
                second.clone(),
                json!({ "status": "placed", "entry": mint_entry(storage.as_ref()).await.get() }),
            );
            commit_writes(storage.as_ref(), &[submission_write(placed_second.clone())]).await;
            let stored = unwrap_result(storage.submission(second_id, context()).await);
            assert_deep(
                assert,
                optional_json(stored.as_ref()),
                placed_second.clone(),
            );
            assert_deep(
                assert,
                optional_json(
                    unwrap_result(
                        storage
                            .submission_by_request(root, "other", context())
                            .await,
                    )
                    .as_ref(),
                ),
                placed_second.clone(),
            );

            assert_deep(
                assert,
                json!(submission_ids(storage.as_ref(), &SubmissionQuery::default()).await),
                json!([first_id.get(), second_id.get(), other_conversation_id.get()]),
            );
            assert_deep(
                assert,
                json!(
                    submission_ids(
                        storage.as_ref(),
                        &SubmissionQuery {
                            conversation_id: Some(root),
                            ..SubmissionQuery::default()
                        }
                    )
                    .await
                ),
                json!([first_id.get(), second_id.get()]),
            );
            assert_deep(
                assert,
                json!(
                    submission_ids(
                        storage.as_ref(),
                        &SubmissionQuery {
                            status: Some(crate::types::SubmissionStatus::Queued),
                            ..SubmissionQuery::default()
                        }
                    )
                    .await
                ),
                json!([first_id.get(), other_conversation_id.get()]),
            );
            assert_deep(
                assert,
                json!(
                    submission_ids(
                        storage.as_ref(),
                        &SubmissionQuery {
                            status: Some(crate::types::SubmissionStatus::Placed),
                            ..SubmissionQuery::default()
                        }
                    )
                    .await
                ),
                json!([second_id.get()]),
            );
            assert_deep(
                assert,
                json!(
                    submission_ids(
                        storage.as_ref(),
                        &SubmissionQuery {
                            conversation_id: Some(second_conversation_id),
                            status: Some(crate::types::SubmissionStatus::Queued),
                            order: None,
                        }
                    )
                    .await
                ),
                json!([other_conversation_id.get()]),
            );
            assert_deep(
                assert,
                json!(
                    submission_ids(
                        storage.as_ref(),
                        &SubmissionQuery {
                            conversation_id: Some(second_conversation_id),
                            status: Some(crate::types::SubmissionStatus::Placed),
                            order: None,
                        }
                    )
                    .await
                ),
                json!([]),
            );
            let placed_page = unwrap_result(
                storage
                    .scan_submissions(
                        &SubmissionQuery {
                            status: Some(crate::types::SubmissionStatus::Placed),
                            ..SubmissionQuery::default()
                        },
                        10,
                        None,
                        context(),
                    )
                    .await,
            );
            assert_deep(
                assert,
                json!(placed_page.items.iter().map(json_of).collect::<Vec<_>>()),
                json!([placed_second]),
            );
        },
    ));

    cases.push(conformance_case(
        options,
        "stores passive write submissions without input-only lifecycle states",
        |storage, assert| async move {
            let assert = assert.as_ref();
            let root = create_root(storage.as_ref()).await;
            let done_id = mint_submission(storage.as_ref()).await;
            let failed_id = mint_submission(storage.as_ref()).await;
            let queued_done = json!({
                "id": done_id.get(),
                "conversationId": root.get(),
                "requestId": "passive-done",
                "type": "write",
                "status": "queued",
            });
            let queued_failed = json!({
                "id": failed_id.get(),
                "conversationId": root.get(),
                "requestId": "passive-failed",
                "type": "write",
                "status": "queued",
            });
            commit_writes(
                storage.as_ref(),
                &[
                    submission_write(queued_done.clone()),
                    submission_write(queued_failed.clone()),
                ],
            )
            .await;
            let done = merge(
                queued_done,
                json!({ "status": "done", "entry": mint_entry(storage.as_ref()).await.get() }),
            );
            let unanswered = merge(
                queued_failed,
                json!({ "status": "unanswered", "reason": "closed", "detail": { "retryable": false } }),
            );
            commit_writes(
                storage.as_ref(),
                &[submission_write(done.clone()), submission_write(unanswered.clone())],
            )
            .await;
            let stored = unwrap_result(storage.submission(done_id, context()).await);
            assert_deep(assert, optional_json(stored.as_ref()), done.clone());
            assert_deep(
                assert,
                optional_json(
                    unwrap_result(storage.submission_by_request(root, "passive-done", context()).await)
                        .as_ref(),
                ),
                done,
            );
            let stored = unwrap_result(storage.submission(failed_id, context()).await);
            assert_deep(assert, optional_json(stored.as_ref()), unanswered.clone());
            assert_deep(
                assert,
                optional_json(
                    unwrap_result(
                        storage
                            .submission_by_request(root, "passive-failed", context())
                            .await,
                    )
                    .as_ref(),
                ),
                unanswered,
            );
        },
    ));

    // 组 6a：文档物化。base 与 delta 组成的可回退历史：任一历史提交点都要重建出当时的完整值，
    // 调用方之后修改自己手里的 JSON 不能影响已存内容，复制出的化身与源之后的变更相互独立；
    // 版本迁移只能通过新的 base 完成，只保留 current 的文档拒绝历史读取。
    cases.push(conformance_case(
        options,
        "reconstructs rewindable documents and preserves half-open incarnations",
        |storage, assert| async move {
            let assert = assert.as_ref();
            let root = create_root(storage.as_ref()).await;
            // 时间线：v1 base → v1 delta（p/s 操作）→ v2 base（checkpoint）→ v2 delta（整值替换 r）。
            // 历史读取必须按“当时生效的 base + 之后回放的 delta”重建；`deltasSinceBase` 统计选中
            // base 之后回放的 delta 数。最后在同一个提交里 create 新化身 + retire 旧化身，验证半开
            // 生命周期：之后旧的化身当前读取为空、地址解析切到新化身，但历史点仍解析到旧化身。
            let first_id = mint_document(storage.as_ref()).await;
            let first_record = document_record(
                first_id,
                "conversation.notes",
                json!({ "kind": "conversation", "conversationId": root.get() }),
                json!({ "history": "rewindable", "fork": "asOf" }),
            );
            let mut initial = json!({ "items": ["a"], "nested": { "count": 1 } });
            let created_at = commit_writes(
                storage.as_ref(),
                &[document_create_write(
                    first_record.clone(),
                    1,
                    initial.clone(),
                )],
            )
            .await;
            let mut appended = json!(["b"]);
            let ops = json!([
                ["p", ["items"], 1, 0, appended.clone()],
                ["s", ["nested", "count"], 2],
            ]);
            let changed_at =
                commit_writes(storage.as_ref(), &[document_delta_write(first_id, 1, ops)]).await;

            initial["items"]
                .as_array_mut()
                .expect("items array")
                .push(json!("caller mutation"));
            appended
                .as_array_mut()
                .expect("appended array")
                .push(json!("caller mutation"));
            let stored = document_at(storage.as_ref(), first_id, DocumentPoint::At(created_at))
                .await
                .expect("created document");
            assert_partial(
                assert,
                stored_document_json(&stored),
                json!({
                    "version": 1,
                    "value": { "items": ["a"], "nested": { "count": 1 } },
                    "deltasSinceBase": 0,
                }),
            );
            let changed = document_at(storage.as_ref(), first_id, DocumentPoint::At(changed_at))
                .await
                .expect("changed document");
            assert_deep(
                assert,
                json_of(&changed.value),
                json!({ "items": ["a", "b"], "nested": { "count": 2 } }),
            );
            assert_strict(assert, json!(changed.deltas_since_base), json!(1));
            let mut read_value = changed.value.clone();
            read_value["items"]
                .as_array_mut()
                .expect("items array")
                .push(json!("read mutation"));
            let current = document_at(storage.as_ref(), first_id, DocumentPoint::Current)
                .await
                .expect("current document");
            assert_deep(
                assert,
                json_of(&current.value),
                json!({ "items": ["a", "b"], "nested": { "count": 2 } }),
            );

            let checkpoint_at = commit_writes(
                storage.as_ref(),
                &[document_base_write(
                    first_id,
                    2,
                    json!({ "items": ["checkpoint"], "nested": { "count": 3 } }),
                )],
            )
            .await;
            let replaced_at = commit_writes(
                storage.as_ref(),
                &[document_delta_write(
                    first_id,
                    2,
                    json!([["r", { "items": ["replacement"], "nested": { "count": 4 } }]]),
                )],
            )
            .await;
            let stored = document_at(storage.as_ref(), first_id, DocumentPoint::At(changed_at))
                .await
                .expect("historical document");
            assert_partial(
                assert,
                stored_document_json(&stored),
                json!({ "version": 1, "value": { "items": ["a", "b"], "nested": { "count": 2 } } }),
            );
            let stored = document_at(storage.as_ref(), first_id, DocumentPoint::At(checkpoint_at))
                .await
                .expect("checkpoint document");
            assert_partial(
                assert,
                stored_document_json(&stored),
                json!({
                    "version": 2,
                    "value": { "items": ["checkpoint"], "nested": { "count": 3 } },
                    "deltasSinceBase": 0,
                }),
            );
            let stored = document_at(storage.as_ref(), first_id, DocumentPoint::At(replaced_at))
                .await
                .expect("replaced document");
            assert_partial(
                assert,
                stored_document_json(&stored),
                json!({
                    "value": { "items": ["replacement"], "nested": { "count": 4 } },
                    "deltasSinceBase": 1,
                }),
            );
            let current = document_at(storage.as_ref(), first_id, DocumentPoint::Current)
                .await
                .expect("current document");
            assert_strict(assert, json!(current.deltas_since_base), json!(1));

            let second_id = mint_document(storage.as_ref()).await;
            let retired_at = commit_writes(
                storage.as_ref(),
                &[
                    document_create_write(
                        merge(first_record.clone(), json!({ "id": second_id.get() })),
                        1,
                        json!({ "items": ["new"] }),
                    ),
                    document_retire_write(first_id),
                    document_delta_write(first_id, 2, json!([["s", ["retiring"], true]])),
                ],
            )
            .await;
            let address = document_address(
                "conversation.notes",
                DocumentScope::Conversation {
                    conversation_id: root,
                },
                None,
            );
            let found =
                find_document_at(storage.as_ref(), &address, DocumentPoint::At(changed_at)).await;
            assert_strict(
                assert,
                json!(found.as_ref().map(|record| record.id.get())),
                json!(first_id.get()),
            );
            let found =
                find_document_at(storage.as_ref(), &address, DocumentPoint::At(retired_at)).await;
            assert_partial(
                assert,
                optional_json(found.as_ref()),
                json!({ "id": second_id.get(), "createdAt": retired_at.get() }),
            );
            let scope = DocumentScope::Conversation {
                conversation_id: root,
            };
            let page = scan_documents_page(
                storage.as_ref(),
                &DocumentQuery {
                    scope,
                    at: DocumentPoint::At(changed_at),
                    kind: None,
                },
                10,
                None,
            )
            .await;
            assert_deep(
                assert,
                ids_of(&page.items, |item| item.id.get()),
                json!([first_id.get()]),
            );
            let page = scan_documents_page(
                storage.as_ref(),
                &DocumentQuery {
                    scope,
                    at: DocumentPoint::At(retired_at),
                    kind: None,
                },
                10,
                None,
            )
            .await;
            assert_deep(
                assert,
                ids_of(&page.items, |item| item.id.get()),
                json!([second_id.get()]),
            );
            assert_no_document(
                assert,
                document_at(storage.as_ref(), first_id, DocumentPoint::At(retired_at))
                    .await
                    .as_ref(),
            );
            let stored = document_at(storage.as_ref(), second_id, DocumentPoint::Current)
                .await
                .expect("replacement document");
            assert_deep(assert, json_of(&stored.value), json!({ "items": ["new"] }));
        },
    ));

    cases.push(conformance_case(
        options,
        "streams long document tails across root replacement deltas",
        |storage, assert| async move {
            let assert = assert.as_ref();
            let root = create_root(storage.as_ref()).await;
            let id = mint_document(storage.as_ref()).await;
            let record = document_record(
                id,
                "conversation.long-tail",
                json!({ "kind": "conversation", "conversationId": root.get() }),
                json!({ "history": "rewindable", "fork": "asOf" }),
            );
            // 场景：512 行的初始 base，先叠 24 个 delta，再用一条 `r`（整值替换）delta 替换整个
            // 根值，最后再叠 24 个 delta。四个时间点（创建、替换前、替换点、当前）都必须得到当时
            // 的完整值，覆盖“长 delta 链跨过根替换”的重建路径；末尾修改读出的 value 再读一次，
            // 确认每次返回的都是独立副本。
            let rows: Vec<Value> = (0..512)
                .map(|value| json!({ "value": value, "stable": format!("row-{value}") }))
                .collect();
            let initial = json!({ "revision": 0, "rows": rows });
            let created_at = commit_writes(
                storage.as_ref(),
                &[document_create_write(record, 1, initial.clone())],
            )
            .await;

            let mut before_replacement = initial.clone();
            let mut before_replacement_at = created_at;
            for revision in 1..=24i64 {
                let index = ((revision * 17) % 512) as usize;
                before_replacement["rows"][index]["value"] = json!(-revision);
                before_replacement["revision"] = json!(revision);
                before_replacement_at = commit_writes(
                    storage.as_ref(),
                    &[document_delta_write(
                        id,
                        1,
                        json!([
                            ["s", ["rows", index, "value"], -revision],
                            ["s", ["revision"], revision],
                        ]),
                    )],
                )
                .await;
            }

            let replacement_rows: Vec<Value> = (0..512)
                .map(|value| json!({ "value": 10_000 + value, "stable": format!("new-{value}") }))
                .collect();
            let mut replacement = json!({ "revision": 100, "rows": replacement_rows });
            let replacement_snapshot = replacement.clone();
            let replacement_at = commit_writes(
                storage.as_ref(),
                &[document_delta_write(
                    id,
                    1,
                    json!([["r", replacement.clone()]]),
                )],
            )
            .await;
            replacement["rows"][0]["value"] = json!(-999);

            let mut current = replacement_snapshot.clone();
            for revision in 101..=124i64 {
                let index = ((revision * 19) % 512) as usize;
                current["rows"][index]["value"] = json!(-revision);
                current["revision"] = json!(revision);
                commit_writes(
                    storage.as_ref(),
                    &[document_delta_write(
                        id,
                        1,
                        json!([
                            ["s", ["rows", index, "value"], -revision],
                            ["s", ["revision"], revision],
                        ]),
                    )],
                )
                .await;
            }

            let stored = document_at(storage.as_ref(), id, DocumentPoint::At(created_at))
                .await
                .expect("created document");
            assert_deep(assert, json_of(&stored.value), initial);
            let stored = document_at(
                storage.as_ref(),
                id,
                DocumentPoint::At(before_replacement_at),
            )
            .await
            .expect("pre-replacement document");
            assert_deep(assert, json_of(&stored.value), before_replacement);
            let stored = document_at(storage.as_ref(), id, DocumentPoint::At(replacement_at))
                .await
                .expect("replacement document");
            assert_deep(assert, json_of(&stored.value), replacement_snapshot);
            let mut read = document_at(storage.as_ref(), id, DocumentPoint::Current)
                .await
                .expect("current document");
            assert_deep(assert, json_of(&read.value), current.clone());
            read.value["rows"][0]["value"] = json!(-1_000);
            let stored = document_at(storage.as_ref(), id, DocumentPoint::Current)
                .await
                .expect("current document");
            assert_deep(assert, json_of(&stored.value), current);
        },
    ));

    cases.push(conformance_case(
        options,
        "copies stored document bases independently and rejects ambiguous sources",
        |storage, assert| async move {
            let assert = assert.as_ref();
            let root = create_root(storage.as_ref()).await;
            let child_id = mint_conversation(storage.as_ref()).await;
            let second_child_id = mint_conversation(storage.as_ref()).await;
            commit_writes(
                storage.as_ref(),
                &[
                    conversation_write(json!({ "id": child_id.get() })),
                    conversation_write(json!({ "id": second_child_id.get() })),
                ],
            )
            .await;
            // 源文档先创建 v2 base，再叠一个 delta；副本分别取历史点（created_at）、当前点，
            // 以及创建后立即退役的副本。断言副本是独立快照：之后修改/退役源文档不影响已复制的值。
            // 后半验证两种被整体拒绝的复制：同一提交里既复制又退役源（来源歧义），以及目的 kind
            // 与来源 kind 不一致。
            let source_id = mint_document(storage.as_ref()).await;
            let source_record = document_record(
                source_id,
                "copy.source",
                json!({ "kind": "conversation", "conversationId": root.get() }),
                json!({ "history": "rewindable", "fork": "asOf" }),
            );
            let created_at = commit_writes(
                storage.as_ref(),
                &[document_create_write(
                    source_record,
                    2,
                    json!({ "count": 1, "rows": [{ "value": "base" }] }),
                )],
            )
            .await;
            commit_writes(
                storage.as_ref(),
                &[document_delta_write(
                    source_id,
                    2,
                    json!([
                        ["s", ["count"], 2],
                        ["p", ["rows"], 1, 0, [{ "value": "current" }]],
                    ]),
                )],
            )
            .await;
            let historical_copy_id = mint_document(storage.as_ref()).await;
            let current_copy_id = mint_document(storage.as_ref()).await;
            let retired_copy_id = mint_document(storage.as_ref()).await;
            let child_record = |id: DocumentId, conversation_id: ConversationId| {
                document_record(
                    id,
                    "copy.source",
                    json!({ "kind": "conversation", "conversationId": conversation_id.get() }),
                    json!({ "history": "rewindable", "fork": "asOf" }),
                )
            };
            commit_writes(
                storage.as_ref(),
                &[
                    document_copy_write(
                        child_record(historical_copy_id, child_id),
                        source_id,
                        json!(created_at.get()),
                    ),
                    document_copy_write(
                        child_record(current_copy_id, second_child_id),
                        source_id,
                        json!("current"),
                    ),
                    document_copy_write(
                        child_record(retired_copy_id, root),
                        source_id,
                        json!("current"),
                    ),
                    document_retire_write(retired_copy_id),
                ],
            )
            .await;
            let stored = document_at(
                storage.as_ref(),
                historical_copy_id,
                DocumentPoint::Current,
            )
            .await
            .expect("historical copy");
            assert_partial(
                assert,
                stored_document_json(&stored),
                json!({ "version": 2, "value": { "count": 1, "rows": [{ "value": "base" }] } }),
            );
            let stored = document_at(storage.as_ref(), current_copy_id, DocumentPoint::Current)
                .await
                .expect("current copy");
            assert_partial(
                assert,
                stored_document_json(&stored),
                json!({
                    "version": 2,
                    "value": { "count": 2, "rows": [{ "value": "base" }, { "value": "current" }] },
                }),
            );
            assert_no_document(
                assert,
                document_at(storage.as_ref(), retired_copy_id, DocumentPoint::Current)
                    .await
                    .as_ref(),
            );

            commit_writes(
                storage.as_ref(),
                &[
                    document_base_write(source_id, 2, json!({ "count": 99, "rows": [] })),
                    document_retire_write(source_id),
                ],
            )
            .await;
            let stored = document_at(storage.as_ref(), current_copy_id, DocumentPoint::Current)
                .await
                .expect("current copy");
            assert_deep(
                assert,
                json_of(&stored.value),
                json!({ "count": 2, "rows": [{ "value": "base" }, { "value": "current" }] }),
            );

            let latest_source_id = mint_document(storage.as_ref()).await;
            let latest_copy_id = mint_document(storage.as_ref()).await;
            let latest_source = document_record(
                latest_source_id,
                "copy.latest",
                json!({ "kind": "conversation", "conversationId": root.get() }),
                json!({ "history": "latest", "fork": "current" }),
            );
            commit_writes(
                storage.as_ref(),
                &[document_create_write(
                    latest_source.clone(),
                    4,
                    json!({ "retained": "copy" }),
                )],
            )
            .await;
            commit_writes(
                storage.as_ref(),
                &[document_copy_write(
                    merge(
                        merge(
                            latest_source.clone(),
                            json!({ "id": latest_copy_id.get() }),
                        ),
                        json!({ "scope": { "kind": "conversation", "conversationId": child_id.get() } }),
                    ),
                    latest_source_id,
                    json!("current"),
                )],
            )
            .await;
            commit_writes(
                storage.as_ref(),
                &[
                    document_base_write(latest_source_id, 4, json!({ "retained": "source-only" })),
                    document_retire_write(latest_source_id),
                ],
            )
            .await;
            let stored = document_at(storage.as_ref(), latest_copy_id, DocumentPoint::Current)
                .await
                .expect("latest copy");
            assert_partial(
                assert,
                stored_document_json(&stored),
                json!({ "version": 4, "value": { "retained": "copy" } }),
            );

            let conflict_id = mint_document(storage.as_ref()).await;
            let conflict = storage
                .commit(
                    &[
                        document_copy_write(
                            child_record(conflict_id, child_id),
                            current_copy_id,
                            json!("current"),
                        ),
                        document_retire_write(current_copy_id),
                    ],
                    adopt(),
                    context(),
                )
                .await;
            assert_ok(assert, conflict.is_err(), "ambiguous copy source is rejected");
            assert_no_document(
                assert,
                document_at(storage.as_ref(), conflict_id, DocumentPoint::Current)
                    .await
                    .as_ref(),
            );
            let stored = document_at(storage.as_ref(), current_copy_id, DocumentPoint::Current)
                .await
                .expect("current copy");
            assert_deep(
                assert,
                json_of(&stored.value),
                json!({ "count": 2, "rows": [{ "value": "base" }, { "value": "current" }] }),
            );

            let mismatch_id = mint_document(storage.as_ref()).await;
            let mut mismatch_record = child_record(mismatch_id, child_id);
            mismatch_record["kind"] = json!("copy.mismatch");
            let mismatch = storage
                .commit(
                    &[document_copy_write(
                        mismatch_record,
                        current_copy_id,
                        json!("current"),
                    )],
                    adopt(),
                    context(),
                )
                .await;
            assert_ok(assert, mismatch.is_err(), "copy kind mismatch is rejected");
            assert_no_document(
                assert,
                document_at(storage.as_ref(), mismatch_id, DocumentPoint::Current)
                    .await
                    .as_ref(),
            );
        },
    ));

    cases.push(conformance_case(
        options,
        "uses bases for version transitions and rejects historical reads of current-only documents",
        |storage, assert| async move {
            let assert = assert.as_ref();
            create_root(storage.as_ref()).await;
            let id = mint_document(storage.as_ref()).await;
            let record = document_record(
                id,
                "session.settings",
                json!({ "kind": "session" }),
                json!({}),
            );
            commit_writes(
                storage.as_ref(),
                &[document_create_write(record, 1, json!({ "count": 1 }))],
            )
            .await;
            commit_writes(
                storage.as_ref(),
                &[document_delta_write(id, 1, json!([["s", ["count"], 2]]))],
            )
            .await;
            let migrated_at = commit_writes(
                storage.as_ref(),
                &[document_base_write(id, 2, json!({ "count": 3 }))],
            )
            .await;
            let stored = document_at(storage.as_ref(), id, DocumentPoint::Current)
                .await
                .expect("migrated document");
            assert_partial(
                assert,
                stored_document_json(&stored),
                json!({ "version": 2, "value": { "count": 3 } }),
            );
            let storage2 = storage.clone();
            assert_rejects(
                assert,
                Box::pin(async move {
                    storage2
                        .document(id, DocumentPoint::At(migrated_at), context())
                        .await
                        .map(|document| json!(document.is_some()))
                }),
                "does not retain historical content",
            )
            .await;
            let storage2 = storage.clone();
            assert_rejects(
                assert,
                Box::pin(async move {
                    storage2
                        .commit(
                            &[document_delta_write(id, 1, json!([["s", ["count"], 4]]))],
                            adopt(),
                            context(),
                        )
                        .await
                        .map(|seq| json!(seq.get()))
                }),
                "version transition requires a base",
            )
            .await;
            let stored = document_at(storage.as_ref(), id, DocumentPoint::Current)
                .await
                .expect("current document");
            assert_deep(assert, json_of(&stored.value), json!({ "count": 3 }));
            commit_writes(storage.as_ref(), &[document_retire_write(id)]).await;
            assert_no_document(
                assert,
                document_at(storage.as_ref(), id, DocumentPoint::Current)
                    .await
                    .as_ref(),
            );
        },
    ));

    // 组 6b：文档索引与生命周期。逻辑地址查找与按作用域的扫描互不替代；创建/变更/退役失败时，
    // 记录表与二级索引（地址、requestId）必须一起回滚。
    cases.push(conformance_case(
        options,
        "indexes logical addresses and exact-scope scans independently",
        |storage, assert| async move {
            let assert = assert.as_ref();
            let root = create_root(storage.as_ref()).await;
            let first_id = mint_document(storage.as_ref()).await;
            let second_id = mint_document(storage.as_ref()).await;
            let conversation_document_id = mint_document(storage.as_ref()).await;
            let task_id = mint_task(storage.as_ref()).await;
            let task_singleton_id = mint_document(storage.as_ref()).await;
            let task_family_id = mint_document(storage.as_ref()).await;
            let task_other_kind_id = mint_document(storage.as_ref()).await;
            let created_at = commit_writes(
                storage.as_ref(),
                &[
                    task_write(pending_task(task_id, root, "ready")),
                    document_create_write(
                        document_record(
                            first_id,
                            "cache",
                            json!({ "kind": "session" }),
                            json!({ "key": "__proto__" }),
                        ),
                        1,
                        json!({ "owner": "first" }),
                    ),
                    document_create_write(
                        document_record(
                            second_id,
                            "cache",
                            json!({ "kind": "session" }),
                            json!({ "key": "constructor" }),
                        ),
                        1,
                        json!({ "owner": "second" }),
                    ),
                    document_create_write(
                        document_record(
                            conversation_document_id,
                            "cache",
                            json!({ "kind": "conversation", "conversationId": root.get() }),
                            json!({ "history": "latest", "fork": "current", "key": "__proto__" }),
                        ),
                        1,
                        json!({ "owner": "conversation" }),
                    ),
                    document_create_write(
                        document_record(
                            task_singleton_id,
                            "task.cache",
                            json!({ "kind": "task", "taskId": task_id.get() }),
                            json!({}),
                        ),
                        1,
                        json!({ "owner": "singleton" }),
                    ),
                    document_create_write(
                        document_record(
                            task_family_id,
                            "task.cache",
                            json!({ "kind": "task", "taskId": task_id.get() }),
                            json!({ "key": "member" }),
                        ),
                        1,
                        json!({ "owner": "family" }),
                    ),
                    document_create_write(
                        document_record(
                            task_other_kind_id,
                            "task.other",
                            json!({ "kind": "task", "taskId": task_id.get() }),
                            json!({}),
                        ),
                        1,
                        json!({ "owner": "other" }),
                    ),
                ],
            )
            .await;

            let found = find_document_at(
                storage.as_ref(),
                &document_address("cache", DocumentScope::Session, Some(json!("__proto__"))),
                DocumentPoint::Current,
            )
            .await;
            assert_strict(
                assert,
                json!(found.map(|record| record.id.get())),
                json!(first_id.get()),
            );
            let session_query = DocumentQuery {
                scope: DocumentScope::Session,
                at: DocumentPoint::Current,
                kind: None,
            };
            let page = scan_documents_page(storage.as_ref(), &session_query, 1, None).await;
            assert_strict(assert, json!(page.items.len()), json!(1));
            let first_page = scan_documents_page(storage.as_ref(), &session_query, 1, None).await;
            let second_page = scan_documents_page(
                storage.as_ref(),
                &session_query,
                1,
                first_page.next.as_ref(),
            )
            .await;
            let mut scanned_ids: Vec<i64> =
                first_page.items.iter().map(|item| item.id.get()).collect();
            scanned_ids.extend(second_page.items.iter().map(|item| item.id.get()));
            assert_deep(
                assert,
                json!(scanned_ids),
                json!([first_id.get(), second_id.get()]),
            );
            let page = scan_documents_page(
                storage.as_ref(),
                &DocumentQuery {
                    scope: DocumentScope::Conversation {
                        conversation_id: root,
                    },
                    at: DocumentPoint::Current,
                    kind: None,
                },
                10,
                None,
            )
            .await;
            assert_deep(
                assert,
                ids_of(&page.items, |item| item.id.get()),
                json!([conversation_document_id.get()]),
            );
            let found = find_document_at(
                storage.as_ref(),
                &document_address("task.cache", DocumentScope::Task { task_id }, None),
                DocumentPoint::Current,
            )
            .await;
            assert_strict(
                assert,
                json!(found.map(|record| record.id.get())),
                json!(task_singleton_id.get()),
            );
            let found = find_document_at(
                storage.as_ref(),
                &document_address(
                    "task.cache",
                    DocumentScope::Task { task_id },
                    Some(json!("member")),
                ),
                DocumentPoint::Current,
            )
            .await;
            assert_strict(
                assert,
                json!(found.map(|record| record.id.get())),
                json!(task_family_id.get()),
            );
            let page = scan_documents_page(
                storage.as_ref(),
                &DocumentQuery {
                    scope: DocumentScope::Task { task_id },
                    at: DocumentPoint::Current,
                    kind: Some("task.cache".to_owned()),
                },
                10,
                None,
            )
            .await;
            assert_deep(
                assert,
                ids_of(&page.items, |item| item.id.get()),
                json!([task_singleton_id.get(), task_family_id.get()]),
            );
            let storage2 = storage.clone();
            assert_rejects(
                assert,
                Box::pin(async move {
                    storage2
                        .document(task_singleton_id, DocumentPoint::At(created_at), context())
                        .await
                        .map(|document| json!(document.is_some()))
                }),
                "does not retain historical content",
            )
            .await;
        },
    ));

    cases.push(conformance_case(
        options,
        "keeps document lifecycle failures atomic and gives create-plus-retire an empty lifetime",
        |storage, assert| async move {
            let assert = assert.as_ref();
            let root = create_root(storage.as_ref()).await;
            let first_id = mint_document(storage.as_ref()).await;
            let second_id = mint_document(storage.as_ref()).await;
            let record = document_record(
                first_id,
                "singleton",
                json!({ "kind": "session" }),
                json!({}),
            );
            commit_writes(
                storage.as_ref(),
                &[document_create_write(
                    record.clone(),
                    1,
                    json!({ "value": 1 }),
                )],
            )
            .await;
            let storage2 = storage.clone();
            let duplicate_record = record.clone();
            assert_rejects(
                assert,
                Box::pin(async move {
                    storage2
                        .commit(
                            &[
                                document_create_write(
                                    merge(duplicate_record, json!({ "id": second_id.get() })),
                                    1,
                                    json!({ "value": 2 }),
                                ),
                                document_delta_write(first_id, 1, json!([])),
                            ],
                            adopt(),
                            context(),
                        )
                        .await
                        .map(|seq| json!(seq.get()))
                }),
                "already has a current incarnation",
            )
            .await;
            let stored = document_at(storage.as_ref(), first_id, DocumentPoint::Current)
                .await
                .expect("first document");
            assert_deep(assert, json_of(&stored.value), json!({ "value": 1 }));
            assert_no_document(
                assert,
                document_at(storage.as_ref(), second_id, DocumentPoint::Current)
                    .await
                    .as_ref(),
            );

            let empty_id = mint_document(storage.as_ref()).await;
            let empty_at = commit_writes(
                storage.as_ref(),
                &[
                    document_create_write(
                        document_record(
                            empty_id,
                            "singleton",
                            json!({ "kind": "conversation", "conversationId": root.get() }),
                            json!({ "key": "empty", "history": "rewindable", "fork": "initial" }),
                        ),
                        1,
                        json!({}),
                    ),
                    document_retire_write(empty_id),
                ],
            )
            .await;
            assert_no_document(
                assert,
                document_at(storage.as_ref(), empty_id, DocumentPoint::Current)
                    .await
                    .as_ref(),
            );
            assert_no_document(
                assert,
                document_at(storage.as_ref(), empty_id, DocumentPoint::At(empty_at))
                    .await
                    .as_ref(),
            );
            let found = find_document_at(
                storage.as_ref(),
                &document_address(
                    "singleton",
                    DocumentScope::Conversation {
                        conversation_id: root,
                    },
                    Some(json!("empty")),
                ),
                DocumentPoint::At(empty_at),
            )
            .await;
            assert_undefined(assert, found.as_ref());
        },
    ));

    cases.push(conformance_case(
        options,
        "rolls back record tables and secondary indexes when a document command fails",
        |storage, assert| async move {
            let assert = assert.as_ref();
            let root = create_root(storage.as_ref()).await;
            let task_id = mint_task(storage.as_ref()).await;
            let submission_id = mint_submission(storage.as_ref()).await;
            let document_id = mint_document(storage.as_ref()).await;
            let task = pending_task(task_id, root, "ready");
            let submission = json!({
                "id": submission_id.get(),
                "conversationId": root.get(),
                "requestId": "atomic",
                "type": "input",
                "status": "queued",
            });
            let record = document_record(
                document_id,
                "atomic",
                json!({ "kind": "session" }),
                json!({}),
            );
            let baseline_seq = commit_writes(
                storage.as_ref(),
                &[
                    task_write(task.clone()),
                    submission_write(submission.clone()),
                    document_create_write(record.clone(), 1, json!({ "count": 1 })),
                ],
            )
            .await;

            let entry_id = mint_entry(storage.as_ref()).await;
            let conflicting_document_id = mint_document(storage.as_ref()).await;
            let running_task = merge(
                task.clone(),
                json!({ "state": { "status": "running", "checkpoint": { "phase": "effect" } } }),
            );
            let unanswered = merge(
                submission.clone(),
                json!({ "status": "unanswered", "reason": "failed" }),
            );
            let conflicting_record = merge(
                record.clone(),
                json!({ "id": conflicting_document_id.get() }),
            );
            let storage2 = storage.clone();
            assert_rejects(
                assert,
                Box::pin(async move {
                    storage2
                        .commit(
                            &[
                                task_write(running_task),
                                submission_write(unanswered),
                                entry_write(entry_id, root, "transient", json!({})),
                                document_create_write(conflicting_record, 1, json!({ "count": 2 })),
                            ],
                            adopt(),
                            context(),
                        )
                        .await
                        .map(|seq| json!(seq.get()))
                }),
                "already has a current incarnation",
            )
            .await;

            let stored_task = unwrap_result(storage.task(task_id, context()).await);
            assert_deep(assert, optional_json(stored_task.as_ref()), task.clone());
            let page = unwrap_result(
                storage
                    .scan_tasks(
                        &TaskQuery {
                            status: Some(TaskStatus::Pending),
                            ..TaskQuery::default()
                        },
                        10,
                        None,
                        context(),
                    )
                    .await,
            );
            assert_deep(
                assert,
                json!(page.items.iter().map(json_of).collect::<Vec<_>>()),
                json!([task.clone()]),
            );
            let stored_submission = unwrap_result(
                storage
                    .submission_by_request(root, "atomic", context())
                    .await,
            );
            assert_deep(
                assert,
                optional_json(stored_submission.as_ref()),
                submission.clone(),
            );
            assert_undefined(
                assert,
                unwrap_result(storage.entry(entry_id, context()).await).as_ref(),
            );
            assert_no_document(
                assert,
                document_at(
                    storage.as_ref(),
                    conflicting_document_id,
                    DocumentPoint::Current,
                )
                .await
                .as_ref(),
            );
            let found = find_document_at(
                storage.as_ref(),
                &document_address("atomic", DocumentScope::Session, None),
                DocumentPoint::Current,
            )
            .await;
            assert_strict(
                assert,
                json!(found.map(|record| record.id.get())),
                json!(document_id.get()),
            );
            let after_rollback_seq = commit_writes(
                storage.as_ref(),
                &[document_delta_write(
                    document_id,
                    1,
                    json!([["s", ["count"], 3]]),
                )],
            )
            .await;
            assert_greater(assert, after_rollback_seq.get(), baseline_seq.get());
        },
    ));

    // 组 7：索引字符串。`kind`、`requestId`、文档 key 作为索引键必须原样往返，不能被截断，
    // 也不能在编码转换中损失信息（用例用码点相邻的 U+10000/U+10001 区分两个键，一个键的
    // 查询不能命中另一个键）。
    cases.push(conformance_case(
        options,
        "keeps indexed string identities lossless",
        |storage, assert| async move {
            let assert = assert.as_ref();
            let root = create_root(storage.as_ref()).await;
            // TS 用孤立代理项 "\ud800"/"\ud801" 作为被索引的身份；Rust 的 `String` 只能是合法
            // UTF-8（ARCHITECTURE §4.9 冻结的 `TaskRecord.kind: String`、`SubmissionRecord.request_id`、
            // `DocumentCreate.kind` 都是 String），孤立代理项在类型上不可表示。spec
            // env-evals-durable-testing §2.3.6 用例 24 要求“WTF-8 或显式 UTF-16”，而
            // spec/durable-rest §8.9 第 19 条给出相反且更具体的决定：Rust 侧字符串必须是合法
            // UTF-8，孤立代理项在进入存储前拒绝或替换为 U+FFFD。此处按后者取值，改用
            // U+10000/U+10001（UTF-16 下是代理对，UTF-8 下是 4 字节）——仍能发现索引键被截断/
            // 规范化的后端，但“孤立代理项无损”这一种损失在 Rust API 上不可表达，无法覆盖。
            let first = "\u{10000}";
            let second = "\u{10001}";
            let first_task_id = mint_task(storage.as_ref()).await;
            let second_task_id = mint_task(storage.as_ref()).await;
            let first_submission_id = mint_submission(storage.as_ref()).await;
            let second_submission_id = mint_submission(storage.as_ref()).await;
            let first_kind_document_id = mint_document(storage.as_ref()).await;
            let second_kind_document_id = mint_document(storage.as_ref()).await;
            let first_key_document_id = mint_document(storage.as_ref()).await;
            let second_key_document_id = mint_document(storage.as_ref()).await;
            commit_writes(
                storage.as_ref(),
                &[
                    task_write(merge(
                        pending_task(first_task_id, root, "ready"),
                        json!({ "kind": first }),
                    )),
                    task_write(merge(
                        pending_task(second_task_id, root, "ready"),
                        json!({ "kind": second }),
                    )),
                    submission_write(json!({
                        "id": first_submission_id.get(),
                        "conversationId": root.get(),
                        "requestId": first,
                        "type": "input",
                        "status": "queued",
                    })),
                    submission_write(json!({
                        "id": second_submission_id.get(),
                        "conversationId": root.get(),
                        "requestId": second,
                        "type": "input",
                        "status": "queued",
                    })),
                    document_create_write(
                        document_record(
                            first_kind_document_id,
                            first,
                            json!({ "kind": "session" }),
                            json!({}),
                        ),
                        1,
                        json!({ "identity": "first kind" }),
                    ),
                    document_create_write(
                        document_record(
                            second_kind_document_id,
                            second,
                            json!({ "kind": "session" }),
                            json!({}),
                        ),
                        1,
                        json!({ "identity": "second kind" }),
                    ),
                    document_create_write(
                        document_record(
                            first_key_document_id,
                            "family",
                            json!({ "kind": "session" }),
                            json!({ "key": first }),
                        ),
                        1,
                        json!({ "identity": "first key" }),
                    ),
                    document_create_write(
                        document_record(
                            second_key_document_id,
                            "family",
                            json!({ "kind": "session" }),
                            json!({ "key": second }),
                        ),
                        1,
                        json!({ "identity": "second key" }),
                    ),
                ],
            )
            .await;

            let page = unwrap_result(
                storage
                    .scan_tasks(
                        &TaskQuery {
                            kind: Some(first.to_owned()),
                            ..TaskQuery::default()
                        },
                        10,
                        None,
                        context(),
                    )
                    .await,
            );
            assert_deep(
                assert,
                ids_of(&page.items, |item| item.id.get()),
                json!([first_task_id.get()]),
            );
            let page = unwrap_result(
                storage
                    .scan_tasks(
                        &TaskQuery {
                            kind: Some(second.to_owned()),
                            ..TaskQuery::default()
                        },
                        10,
                        None,
                        context(),
                    )
                    .await,
            );
            assert_deep(
                assert,
                ids_of(&page.items, |item| item.id.get()),
                json!([second_task_id.get()]),
            );
            let stored =
                unwrap_result(storage.task(first_task_id, context()).await).expect("first task");
            assert_strict(assert, json!(stored.kind), json!(first));
            let stored =
                unwrap_result(storage.task(second_task_id, context()).await).expect("second task");
            assert_strict(assert, json!(stored.kind), json!(second));
            let stored = unwrap_result(storage.submission_by_request(root, first, context()).await)
                .expect("first submission");
            assert_strict(assert, json!(stored.request_id.as_deref()), json!(first));
            let stored = unwrap_result(storage.submission_by_request(root, first, context()).await)
                .expect("first submission");
            assert_strict(
                assert,
                json!(stored.id.get()),
                json!(first_submission_id.get()),
            );
            let stored =
                unwrap_result(storage.submission_by_request(root, second, context()).await)
                    .expect("second submission");
            assert_strict(
                assert,
                json!(stored.id.get()),
                json!(second_submission_id.get()),
            );
            let found = find_document_at(
                storage.as_ref(),
                &document_address(first, DocumentScope::Session, None),
                DocumentPoint::Current,
            )
            .await;
            assert_strict(
                assert,
                json!(found.map(|record| record.id.get())),
                json!(first_kind_document_id.get()),
            );
            let found = find_document_at(
                storage.as_ref(),
                &document_address(second, DocumentScope::Session, None),
                DocumentPoint::Current,
            )
            .await;
            assert_strict(
                assert,
                json!(found.map(|record| record.id.get())),
                json!(second_kind_document_id.get()),
            );
            let found = find_document_at(
                storage.as_ref(),
                &document_address("family", DocumentScope::Session, Some(json!(first))),
                DocumentPoint::Current,
            )
            .await;
            assert_strict(
                assert,
                json!(found.map(|record| record.id.get())),
                json!(first_key_document_id.get()),
            );
            let found = find_document_at(
                storage.as_ref(),
                &document_address("family", DocumentScope::Session, Some(json!(second))),
                DocumentPoint::Current,
            )
            .await;
            assert_strict(
                assert,
                json!(found.map(|record| record.id.get())),
                json!(second_key_document_id.get()),
            );
            let page = scan_documents_page(
                storage.as_ref(),
                &DocumentQuery {
                    scope: DocumentScope::Session,
                    at: DocumentPoint::Current,
                    kind: Some(first.to_owned()),
                },
                10,
                None,
            )
            .await;
            assert_deep(
                assert,
                ids_of(&page.items, |item| item.id.get()),
                json!([first_kind_document_id.get()]),
            );
        },
    ));

    // 组 8：全局 ID 空间与关闭。所有记录类型共用一个数字命名空间；ID 上界是 JS 安全整数；
    // close 是终态，之后的读、提交（即使空批次）与 mint 都必须拒绝。
    cases.push(conformance_case(
        options,
        "keeps one global record ID namespace and rejects exhausted ID minting",
        |storage, assert| async move {
            let assert = assert.as_ref();
            let root = create_root(storage.as_ref()).await;
            // 命名空间跨记录类型共享：显式写入 entry ID 100 后 mint 必须跳到 101；用 task 复用
            // 数字 100 必须因“ID 已属于 entry”被拒绝。
            let explicit_entry_id = id_from_number::<crate::types::EntryBrand>(100);
            commit_writes(
                storage.as_ref(),
                &[entry_write(explicit_entry_id, root, "message", json!({}))],
            )
            .await;
            assert_strict(
                assert,
                json!(mint(storage.as_ref(), IdKind::Entry).await),
                json!(101),
            );
            let message = format!("ID {} already belongs to entry", explicit_entry_id.get());
            let storage2 = storage.clone();
            assert_rejects(
                assert,
                Box::pin(async move {
                    storage2
                        .commit(
                            &[task_write(pending_task(
                                id_from_number::<crate::types::TaskBrand>(explicit_entry_id.get()),
                                root,
                                "ready",
                            ))],
                            adopt(),
                            context(),
                        )
                        .await
                        .map(|seq| json!(seq.get()))
                }),
                &message,
            )
            .await;

            // JS 的 `Number.MAX_SAFE_INTEGER`（2^53-1）。ID 在 TS 侧是 number，超过它就无法
            // 精确表示，所以 ID 空间上界取这个值；写到上界后继续 mint 必须连续失败。
            let max_safe_integer = 9_007_199_254_740_991_i64;
            commit_writes(
                storage.as_ref(),
                &[entry_write(
                    id_from_number::<crate::types::EntryBrand>(max_safe_integer),
                    root,
                    "last-id",
                    json!({}),
                )],
            )
            .await;
            let storage2 = storage.clone();
            assert_rejects(
                assert,
                Box::pin(async move { storage2.mint_id(IdKind::Entry).await.map(|id| json!(id)) }),
                "ID space is exhausted",
            )
            .await;
            let storage2 = storage.clone();
            assert_rejects(
                assert,
                Box::pin(async move { storage2.mint_id(IdKind::Entry).await.map(|id| json!(id)) }),
                "ID space is exhausted",
            )
            .await;
        },
    ));

    cases.push(conformance_case(
        options,
        "rejects every operation after close",
        |storage, assert| async move {
            let assert = assert.as_ref();
            create_root(storage.as_ref()).await;
            storage.as_ref().close(context()).await;
            let storage2 = storage.clone();
            assert_rejects(
                assert,
                Box::pin(async move {
                    storage2
                        .conversation(ROOT_CONVERSATION_ID, context())
                        .await
                        .map(|record| json!(record.is_some()))
                }),
                "closed",
            )
            .await;
            let storage2 = storage.clone();
            assert_rejects(
                assert,
                Box::pin(async move {
                    storage2
                        .commit(&[], adopt(), context())
                        .await
                        .map(|seq| json!(seq.get()))
                }),
                "closed",
            )
            .await;
            let storage2 = storage.clone();
            assert_rejects(
                assert,
                Box::pin(async move {
                    storage2
                        .mint_id(IdKind::Conversation)
                        .await
                        .map(|id| json!(id))
                }),
                "closed",
            )
            .await;
        },
    ));

    cases
}
