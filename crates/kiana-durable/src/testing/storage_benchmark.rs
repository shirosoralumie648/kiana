//! `testing/storage-benchmark.ts`：确定性的存储基准数据集与读写基准。
//!
//! 全部只通过公开的 `Storage` 契约写入；`adopt` 是空函数（存储层提交没有内存 owner）。
//!
//! # 为什么需要这个模块
//!
//! `tests/storage_bench.rs` 与 `tests/storage_memory.rs` 在 memory/sqlite/jsonl 上运行同一数据集；
//! `tests/storage_conformance.rs` 用 `RecordingStorage` 锁定种子数据与基准表的名称/数值/顺序。
//! 数据集与基准表集中在这里；名称、数值与顺序是当前稳定契约，改动需同步上述基准测试。
//! 耗时受运行时影响，不能跨语言直接对照。
//!
//! # 数据如何写入
//!
//! 种子只调用公开的 [`Storage`] 契约（`mint_id` + `commit`），不依赖任何后端的内部表结构；
//! 每条写入都由 JSON 字面量经 `serde_json::from_value` 构造成 [`StorageWrite`]。缺字段或类型
//! 不符会让 `from_value` 返回错误并被 `write` 转成 panic（多余字段则按 serde 默认行为忽略），
//! 这样基准数据的形状错误会在种子阶段暴露，而不是把错误数据写进存储。
//!
//! 种子逻辑不含随机性：同一 scale 产生的批次结构与记录形状固定；具体 ID 由 storage 的
//! `mint_id` 序列决定，因此 [`StorageBenchmarkDataset`] 必须与产生它的 storage 配套使用。
//!
//! # 失败模式
//!
//! - 种子函数返回 `Result<_, BoxError>`：`mint_id`/`commit` 的错误原样上抛；规模无法产生必要
//!   记录（`entry_count == 0` 或 `document_count == 0`）时返回 [`StorageBenchmarkSeedError`]。
//!   函数不提供回滚或清理，失败时 storage 里可能留有已提交的前缀数据。
//! - 读/写基准的 `run` 返回 `f64`，没有错误通道：存储错误统一 panic（见 `expect_benchmark`），
//!   避免把失败记录成 -1/NaN 这样的“测量值”。
//!
//! # 阅读顺序
//!
//! 1. [`StorageBenchmarkScale`]/[`TIMING_SCALE`] 与 [`storage_benchmark_primary_record_count`]；
//! 2. [`seed_storage_benchmark_with_scale`] 的七个写入阶段；
//! 3. [`STORAGE_READ_BENCHMARKS`] 与 [`STORAGE_WRITE_BENCHMARKS`] 两张表；
//! 4. 需要新增测量时，同时更新表和 `tests/storage_conformance.rs` 的逐字断言。

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use kiana_chord::context::Context;
use serde_json::{Value, json};

use crate::types::{
    AdoptFn, BoxError, ConversationId, DocumentAddress, DocumentId, DocumentPoint, DocumentScope,
    EntryId, EntryQuery, IdKind, ROOT_CONVERSATION_ID, Seq, Storage, StorageWrite, TaskId,
    TaskQuery, TaskStatus, id_from_number,
};

use kiana_chord::context::BACKGROUND_CONTEXT;

/// Storage 层提交没有内存 owner 需要 adopt。
fn adopt() -> AdoptFn {
    Box::new(|_seq: Seq| {})
}

fn context() -> &'static dyn Context {
    &**BACKGROUND_CONTEXT
}

/// 把 JSON 字面量转成 [`StorageWrite`]。
///
/// 字段名/类型必须符合 serde 表示（记录用 camelCase）；不匹配时 `from_value` 返回错误，
/// 由 `expect` 直接 panic。基准数据是内部生成的固定形状，因此这里不返回 `Result`。
fn write(value: Value) -> StorageWrite {
    serde_json::from_value(value).expect("benchmark write shape")
}

/// 用无主 `adopt` 与后台上下文提交一批写入，错误原样返回给种子函数。
async fn commit(storage: &dyn Storage, writes: Vec<StorageWrite>) -> Result<Seq, BoxError> {
    storage.commit(&writes, adopt(), context()).await
}

/// `StorageBenchmarkScale`。
///
/// 决定种子数据集规模；`name` 是调用方用来标识/选择规模的标签（如 `tests/storage_memory.rs`
/// 用它与 `KIANA_STORAGE_MEMORY_SCALE` 环境变量匹配），种子逻辑不读取它。`entry_count` 与
/// `document_count` 必须至少为 1，否则种子函数在写完前面的阶段后以
/// [`StorageBenchmarkSeedError`] 失败。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StorageBenchmarkScale {
    /// 规模名。
    pub name: &'static str,
    /// 条目数（根会话下的 `benchmark.entry`）。
    pub entry_count: usize,
    /// 任务数（`benchmark.filtered`/`benchmark.other`）。
    pub task_count: usize,
    /// 文档数（`benchmark.family`；最后一篇是精确地址查找的目标）。
    pub document_count: usize,
}

/// `STORAGE_MEMORY_SCALES`。
///
/// 两档规模供内存占用测试（`tests/storage_memory.rs`）遍历；数值与 TS 相同，
/// `tests/storage_conformance.rs` 锁定名称与各计数。
pub static STORAGE_MEMORY_SCALES: &[StorageBenchmarkScale] = &[
    StorageBenchmarkScale {
        name: "1k",
        entry_count: 1_000,
        task_count: 200,
        document_count: 200,
    },
    StorageBenchmarkScale {
        name: "10k",
        entry_count: 10_000,
        task_count: 2_000,
        document_count: 2_000,
    },
];

/// `TIMING_SCALE`。
///
/// `seed_storage_benchmark` 省略规模时使用的默认值（对应 TS 的默认参数 `scale = TIMING_SCALE`），
/// 也是计时基准（`tests/storage_bench.rs`）使用的规模；内存占用测试改用
/// [`STORAGE_MEMORY_SCALES`]。
pub const TIMING_SCALE: StorageBenchmarkScale = StorageBenchmarkScale {
    name: "timing",
    entry_count: 1_000,
    task_count: 300,
    document_count: 300,
};

const REPLAY_TAILS: [i64; 4] = [0, 16, 128, 1_024];
const HISTORY_SEGMENT_LENGTH: i64 = 128;
const FORK_DEPTH: usize = 8;
const ENTRIES_PER_FORK: usize = 32;
const BATCH_SIZE: usize = 100;

/// `storageBenchmarkPrimaryRecordCount(scale)`。
///
/// 种子数据集的主记录总数：
///
/// ```text
/// 1（根会话，不经过 mint）
/// + entry_count + task_count + document_count
/// + 4（replay 文档）+ 1（historical 文档）
/// + FORK_DEPTH（每层一个新会话）+ FORK_DEPTH * ENTRIES_PER_FORK（每层条目）
/// ```
///
/// 不含次级索引等派生结构。返回值包含根会话，因此种子后 `mint_id` 的调用次数是它减 1
/// （`tests/storage_conformance.rs` 对 TIMING_SCALE 断言 1_870 与这个减一关系）。
pub fn storage_benchmark_primary_record_count(scale: &StorageBenchmarkScale) -> usize {
    1 + scale.entry_count
        + scale.task_count
        + scale.document_count
        + REPLAY_TAILS.len()
        + 1
        + FORK_DEPTH * (1 + ENTRIES_PER_FORK)
}

/// 种子数据集（TS: `StorageBenchmarkDataset`）。
///
/// 只包含基准运行需要的索引；所有 ID/序号都来自刚种子的那个 storage，换一个 storage 或换
/// 一次种子都不再适用。
#[derive(Debug, Clone, PartialEq)]
pub struct StorageBenchmarkDataset {
    /// 第一批里的第一个条目；它同时带 `head` 标记，是根会话唯一的 head。
    pub first_entry_id: EntryId,
    /// 过滤任务扫描期望的条数：`min(50, ceil(task_count / 60))`（见种子函数末尾的说明）。
    pub filtered_task_count: usize,
    /// 最后一个 `benchmark.family` 文档（index = `document_count - 1`）。
    pub exact_document_id: DocumentId,
    /// 它的家族键：`key-{document_count - 1}`。
    pub exact_document_key: String,
    /// 每个 replay tail 的文档 ID；键是 0/16/128/1024，文档的当前 `count` 等于键。
    pub replay_document_ids: std::collections::BTreeMap<i64, DocumentId>,
    /// 带两段历史（中间夹一个新 base 检查点）的文档。
    pub historical_document_id: DocumentId,
    /// 第一段历史最后一次 delta 的提交序号。
    pub ancient_at: Seq,
    /// 第二段历史最后一次 delta 的提交序号。
    pub recent_at: Seq,
    /// 8 层 fork 链最深的会话。
    pub deepest_conversation_id: ConversationId,
    /// 最深会话可继承的祖先 head 条目（根会话第一条、唯一带 `head` 标记者）。
    pub ancestor_head_entry_id: EntryId,
}

/// 种子数据构建失败（TS 的 `throw new Error(...)`）。
///
/// 当前由三处检查返回：`entry_count == 0`、`document_count == 0`，以及历史段没有产生提交
/// （`HISTORY_SEGMENT_LENGTH` 固定为 128，最后一种实际不会发生）。此时 storage 里可能已有
/// 前面阶段提交的数据，本模块不回滚。
#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct StorageBenchmarkSeedError {
    /// 消息。
    pub message: String,
}

impl StorageBenchmarkSeedError {
    fn new(message: impl Into<String>) -> Self {
        StorageBenchmarkSeedError {
            message: message.into(),
        }
    }
}

// 构造确定性的任务记录：status 按 `index % 3` 循环（pending/running/terminal）；
// `index % 4 == 0` 的 kind 是 `benchmark.filtered`，`index % 5 == 0` 的 background=true，
// `index % 7 == 0` 的 abortRequested=true。种子与 `commit mixed ...` 基准共用这个形状；
// filtered_task_count 的公式依赖前三个条件同时成立（即 index 是 60 的倍数）。
fn task(id: TaskId, index: i64) -> Value {
    let statuses = ["pending", "running", "terminal"];
    let status = statuses[index.rem_euclid(3) as usize];
    let common = json!({
        "id": id.get(),
        "conversationId": ROOT_CONVERSATION_ID.get(),
        "kind": if index % 4 == 0 { "benchmark.filtered" } else { "benchmark.other" },
        "version": 1,
        "input": { "index": index },
        "background": index % 5 == 0,
        "abortRequested": index % 7 == 0,
    });
    let state = if status == "terminal" {
        json!({ "status": status, "outcome": { "status": "completed", "result": { "index": index } } })
    } else {
        json!({ "status": status, "checkpoint": { "index": index, "payload": "x".repeat(64) } })
    };
    merge(common, json!({ "state": state }))
}

/// TS 的对象展开：把 `extra` 的键并入 `base`。
fn merge(mut base: Value, extra: Value) -> Value {
    if let (Value::Object(base), Value::Object(extra)) = (&mut base, extra) {
        for (key, value) in extra {
            base.insert(key, value);
        }
    }
    base
}

/// `seedStorageBenchmark(storage)`：按默认规模 [`TIMING_SCALE`] 种子。
///
/// Rust 没有默认参数，默认规模由这个单参数入口提供；
/// 显式规模见 [`seed_storage_benchmark_with_scale`]。
///
/// 等价于 `seed_storage_benchmark_with_scale(storage, &TIMING_SCALE)`；
/// `tests/storage_conformance.rs` 同样用默认入口锁定 TIMING_SCALE 的规模行为。
pub async fn seed_storage_benchmark(
    storage: &dyn Storage,
) -> Result<StorageBenchmarkDataset, BoxError> {
    seed_storage_benchmark_with_scale(storage, &TIMING_SCALE).await
}

/// `seedStorageBenchmark(storage, scale)`：按给定的规模种子。
///
/// 输入是 storage 与规模；输出 [`StorageBenchmarkDataset`]，描述后续读取基准要用的 ID 与
/// 提交序号。执行顺序固定为：根会话 → 条目 → 任务 → `benchmark.family` 文档 → replay
/// 文档及 delta → historical 文档 → 8 层 fork。所有写入都经过 [`Storage::commit`]，其中条目、
/// 任务、文档按 `BATCH_SIZE` 分批；错误从 `mint_id`/`commit` 直接上抛，已提交的批次不会回滚。
///
/// 规模约束：`entry_count >= 1`（否则没有可作为 head 的条目）且 `document_count >= 1`
/// （否则没有精确地址目标），违反时返回 [`StorageBenchmarkSeedError`]。
pub async fn seed_storage_benchmark_with_scale(
    storage: &dyn Storage,
    scale: &StorageBenchmarkScale,
) -> Result<StorageBenchmarkDataset, BoxError> {
    // 阶段 1：根会话。ROOT_CONVERSATION_ID 是保留的 ID 1，不经过 mint_id；后面所有条目都挂在
    // 它下面，fork 链也从它开始。
    commit(
        storage,
        vec![write(json!({
            "type": "conversation",
            "value": { "id": ROOT_CONVERSATION_ID.get() },
        }))],
    )
    .await?;

    // 阶段 2：根会话的条目。按 BATCH_SIZE 分批提交（与 TS 常量一致），每条都先 mint 再写。
    // 只有 index 0 带 `head` 标记，这个条目的 ID 就是数据集里的 first_entry_id。
    let mut first_entry_id: Option<EntryId> = None;
    let mut start = 0usize;
    while start < scale.entry_count {
        let mut writes = Vec::new();
        let end = (start + BATCH_SIZE).min(scale.entry_count);
        for index in start..end {
            let id =
                id_from_number::<crate::types::EntryBrand>(storage.mint_id(IdKind::Entry).await?);
            if index == 0 {
                first_entry_id = Some(id);
            }
            let value = merge(
                json!({
                    "id": id.get(),
                    "conversationId": ROOT_CONVERSATION_ID.get(),
                    "kind": "benchmark.entry",
                    "data": { "index": index, "text": format!("entry-{index}-{}", "x".repeat(96)) },
                }),
                if index == 0 {
                    json!({ "head": id.get() })
                } else {
                    json!({})
                },
            );
            writes.push(write(json!({ "type": "entry", "value": value })));
        }
        commit(storage, writes).await?;
        start = end;
    }

    // 阶段 3：任务。task() 用 index 的模运算固定 status/kind/background/abortRequested，
    // 过滤扫描的期望条数因此可由规模直接算出（见函数末尾的 filtered_task_count）。
    let mut start = 0usize;
    while start < scale.task_count {
        let mut writes = Vec::new();
        let end = (start + BATCH_SIZE).min(scale.task_count);
        for index in start..end {
            let id =
                id_from_number::<crate::types::TaskBrand>(storage.mint_id(IdKind::Task).await?);
            writes.push(write(
                json!({ "type": "task", "value": task(id, index as i64) }),
            ));
        }
        commit(storage, writes).await?;
        start = end;
    }

    // 阶段 4：普通文档（`benchmark.family`）。每篇的 key 是唯一的 `key-{index}`；
    // 最后写入的一篇是 exact_document_id / exact_document_key 的目标，所以 document_count
    // 必须至少为 1。
    let mut exact_document_id: Option<DocumentId> = None;
    let mut start = 0usize;
    while start < scale.document_count {
        let mut writes = Vec::new();
        let end = (start + BATCH_SIZE).min(scale.document_count);
        for index in start..end {
            let id = id_from_number::<crate::types::DocumentBrand>(
                storage.mint_id(IdKind::Document).await?,
            );
            exact_document_id = Some(id);
            writes.push(write(json!({
                "type": "document.create",
                "record": {
                    "id": id.get(),
                    "kind": "benchmark.family",
                    "key": format!("key-{index}"),
                    "scope": { "kind": "session" },
                },
                "content": {
                    "kind": "base",
                    "version": 1,
                    "value": { "index": index, "text": "x".repeat(128) },
                },
            })));
        }
        commit(storage, writes).await?;
        start = end;
    }

    // 阶段 5：replay 文档。4 个文档的最终 `count` 分别是 0/16/128/1024；第 count 轮只给
    // tail >= count 的文档追加 delta，因此每个文档的当前值恰好等于它的 tail，同时产生长度
    // 不同的 delta 链供 replay 读取。tail 0 的文档保持初始值，不追加任何 delta。
    let mut replay_entries: Vec<(i64, DocumentId)> = Vec::new();
    for tail in REPLAY_TAILS {
        let id =
            id_from_number::<crate::types::DocumentBrand>(storage.mint_id(IdKind::Document).await?);
        replay_entries.push((tail, id));
    }
    let mut writes = Vec::new();
    for (tail, id) in &replay_entries {
        writes.push(write(json!({
            "type": "document.create",
            "record": {
                "id": id.get(),
                "kind": "benchmark.replay",
                "key": tail.to_string(),
                "scope": { "kind": "conversation", "conversationId": ROOT_CONVERSATION_ID.get() },
                "history": "rewindable",
                "fork": "asOf",
            },
            "content": { "kind": "base", "version": 1, "value": { "count": 0, "text": "x".repeat(64) } },
        })));
    }
    commit(storage, writes).await?;
    let last_tail = *REPLAY_TAILS.last().expect("replay tails");
    for count in 1..=last_tail {
        let mut writes = Vec::new();
        for (tail, id) in &replay_entries {
            if count <= *tail {
                writes.push(write(json!({
                    "type": "document.change",
                    "id": id.get(),
                    "content": { "kind": "delta", "version": 1, "ops": [["s", ["count"], count]] },
                })));
            }
        }
        commit(storage, writes).await?;
    }

    // 阶段 6：historical 文档。先写 128 条 ancient delta（ancient_at 记最后一条的提交序号），
    // 再写一个新的 base 检查点（count = 128），然后写 128 条 recent delta（recent_at 同理）。
    // 两个读点分别验证“新 base 之前的读”和“之后的读”，期望值是 128 与 256；base 本身
    // 不改变文档的当前值，只改变重建路径。
    let historical_document_id =
        id_from_number::<crate::types::DocumentBrand>(storage.mint_id(IdKind::Document).await?);
    commit(
        storage,
        vec![write(json!({
            "type": "document.create",
            "record": {
                "id": historical_document_id.get(),
                "kind": "benchmark.history",
                "scope": { "kind": "conversation", "conversationId": ROOT_CONVERSATION_ID.get() },
                "history": "rewindable",
                "fork": "asOf",
            },
            "content": { "kind": "base", "version": 1, "value": { "count": 0 } },
        }))],
    )
    .await?;
    let mut ancient_at: Option<Seq> = None;
    for count in 1..=HISTORY_SEGMENT_LENGTH {
        ancient_at = Some(
            commit(
                storage,
                vec![write(json!({
                    "type": "document.change",
                    "id": historical_document_id.get(),
                    "content": { "kind": "delta", "version": 1, "ops": [["s", ["count"], count]] },
                }))],
            )
            .await?,
        );
    }
    commit(
        storage,
        vec![write(json!({
            "type": "document.change",
            "id": historical_document_id.get(),
            "content": {
                "kind": "base",
                "version": 1,
                "value": { "count": HISTORY_SEGMENT_LENGTH },
            },
        }))],
    )
    .await?;
    let ancient_at = ancient_at.ok_or_else(|| {
        StorageBenchmarkSeedError::new("Benchmark history seed produced no commits")
    })?;
    let mut recent_at = ancient_at;
    for count in (HISTORY_SEGMENT_LENGTH + 1)..=(HISTORY_SEGMENT_LENGTH * 2) {
        recent_at = commit(
            storage,
            vec![write(json!({
                "type": "document.change",
                "id": historical_document_id.get(),
                "content": { "kind": "delta", "version": 1, "ops": [["s", ["count"], count]] },
            }))],
        )
        .await?;
    }

    // entry_count 为 0 时没有条目可作根 head 或 fork 起点；前面的阶段已经提交，无法回滚。
    let first_entry_id = first_entry_id
        .ok_or_else(|| StorageBenchmarkSeedError::new("Benchmark scale must create entries"))?;
    // 阶段 7：8 层 fork 链。每层新建一个会话，parent 指向上一个会话的最后一条条目
    // （第一层指向根会话第一条带 head 的条目），再写 32 条属于该会话的条目。最深会话的可见
    // 条目数为 1 + 8 * 32 = 257：8 层各 32 条（含最深会话自己的 32 条），加上根会话中 ID
    // 不大于锚点的第一条；fork 的 parent.at 就是根会话的 first_entry_id，因此根会话只贡献
    // 这一条，entry_count 再大也不增加。这足以让 `fork-depth history scan (100)` 取满一页；
    // 而链上只有根会话第一条带 `head`，所以最深层的 head 查找会命中 ancestor_head_entry_id。
    let mut parent_conversation_id = ROOT_CONVERSATION_ID;
    let mut parent_at = first_entry_id;
    let mut deepest_conversation_id = ROOT_CONVERSATION_ID;
    for depth in 0..FORK_DEPTH {
        let conversation_id = id_from_number::<crate::types::ConversationBrand>(
            storage.mint_id(IdKind::Conversation).await?,
        );
        commit(
            storage,
            vec![write(json!({
                "type": "conversation",
                "value": {
                    "id": conversation_id.get(),
                    "parent": {
                        "conversationId": parent_conversation_id.get(),
                        "at": parent_at.get(),
                    },
                },
            }))],
        )
        .await?;
        let mut ids = Vec::with_capacity(ENTRIES_PER_FORK);
        for _ in 0..ENTRIES_PER_FORK {
            ids.push(id_from_number::<crate::types::EntryBrand>(
                storage.mint_id(IdKind::Entry).await?,
            ));
        }
        let mut writes = Vec::new();
        for (index, id) in ids.iter().enumerate() {
            writes.push(write(json!({
                "type": "entry",
                "value": {
                    "id": id.get(),
                    "conversationId": conversation_id.get(),
                    "kind": "benchmark.fork",
                    "data": { "depth": depth, "index": index },
                },
            })));
        }
        commit(storage, writes).await?;
        parent_conversation_id = conversation_id;
        parent_at = *ids.last().expect("fork entries");
        deepest_conversation_id = conversation_id;
    }

    // document_count 为 0 时没有精确地址目标；同样在写完 fork 之后才报错，storage 里可能已有
    // 前面阶段的数据。
    let exact_document_id = exact_document_id
        .ok_or_else(|| StorageBenchmarkSeedError::new("Benchmark scale must create documents"))?;
    let replay_document_ids: std::collections::BTreeMap<i64, DocumentId> =
        replay_entries.iter().copied().collect();
    // filtered_task_count 的推导：过滤条件是 kind == "benchmark.filtered"（index % 4 == 0）、
    // status == pending（index % 3 == 0）、background == true（index % 5 == 0），三者同时成立的
    // index 是 60 的倍数，数量为 ceil(task_count / 60)；扫描上限是 50，所以取 min。
    Ok(StorageBenchmarkDataset {
        first_entry_id,
        filtered_task_count: 50.min(scale.task_count.div_ceil(60)),
        exact_document_id,
        exact_document_key: format!("key-{}", scale.document_count - 1),
        replay_document_ids,
        historical_document_id,
        ancient_at,
        recent_at,
        deepest_conversation_id,
        ancestor_head_entry_id: first_entry_id,
    })
}

/// 基准运行的 future。
///
/// 输出是观测值（计数或 ID），不是耗时；计时由调用方（如 `tests/storage_bench.rs`）在
/// future 外围绕 `Instant` 完成。`'static` 让 `run` 闭包不必借用调用栈。
pub type BenchmarkFuture = Pin<Box<dyn Future<Output = f64> + Send + 'static>>;

/// `StorageReadBenchmark`。
///
/// 每个基准由 `run`（执行一次读取并给出观测值）与 `expected`（由数据集算出期望值）组成，
/// 调用方比较两者。存储错误会让 `run` panic（见模块文档）；只有“记录不存在”才按 TS 的
/// `?? -1` / `Number(undefined)` 返回 -1 或 `NaN` 哨兵。
#[derive(Clone)]
pub struct StorageReadBenchmark {
    /// 基准名。
    pub name: &'static str,
    /// 运行一次并返回观测值（TS 的 `run(storage, dataset): Promise<number>`）。
    pub run:
        Arc<dyn Fn(Arc<dyn Storage>, StorageBenchmarkDataset) -> BenchmarkFuture + Send + Sync>,
    /// 期望值（TS 的 `expected(dataset): number`）。
    pub expected: Arc<dyn Fn(&StorageBenchmarkDataset) -> f64 + Send + Sync>,
}

impl std::fmt::Debug for StorageReadBenchmark {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StorageReadBenchmark")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

/// TS 里 `run` 返回的 promise 在存储报错时 reject；Rust 的 `run` 输出是 `f64`，因此存储错误
/// 一律 panic（与写基准的 `.expect(...)` 一致），让基准失败，而不是记录一个错误的测量值。
fn expect_benchmark<T>(result: Result<T, BoxError>, label: &str) -> T {
    match result {
        Ok(value) => value,
        Err(error) => panic!("{label}: {error}"),
    }
}

/// `Number((await storage.document(...))?.value.count)`：记录缺失或没有数字 `count` 都是 `NaN`
/// （TS 的 `Number(undefined)`）；存储错误由调用方先经 [`expect_benchmark`] 过滤。
fn document_count(value: Option<&crate::types::StoredDocument>) -> f64 {
    value
        .and_then(|document| document.value.get("count"))
        .and_then(Value::as_f64)
        .unwrap_or(f64::NAN)
}

/// `(await storage.document(...))?.value.count`：缺失 → `NaN`，存储错误 → panic。
fn document_count_of(
    result: Result<Option<crate::types::StoredDocument>, BoxError>,
    label: &str,
) -> f64 {
    document_count(expect_benchmark(result, label).as_ref())
}

/// `STORAGE_READ_BENCHMARKS`（4 个基础项 + 4 个 replay tail + 2 个 historical + 2 个 fork）。
///
/// 名称与顺序逐字来自 TS，`tests/storage_conformance.rs` 的 `benchmark_tables_match_typescript`
/// 锁定。表在首次访问时构造（`LazyLock`），因为闭包无法写成常量；每个 `run` 都重新读取一次，
/// 不缓存结果。缺记录时的哨兵值按 TS 的 `?? -1` / `Number(undefined)` 区分：
/// 三个“精确查找”返回 -1，文档计数返回 `NaN`（见 `document_count`）。
pub static STORAGE_READ_BENCHMARKS: std::sync::LazyLock<Vec<StorageReadBenchmark>> =
    std::sync::LazyLock::new(|| {
        // 基础组：精确读、一次条目分页、过滤任务分页、精确文档地址。
        let mut benchmarks: Vec<StorageReadBenchmark> = vec![
            StorageReadBenchmark {
                name: "exact entry lookup",
                run: Arc::new(|storage, dataset| {
                    Box::pin(async move {
                        // `(await storage.entry(...))?.entry.id ?? -1`：只有记录缺失才是 -1。
                        let entry = expect_benchmark(
                            storage.entry(dataset.first_entry_id, context()).await,
                            "exact entry lookup",
                        );
                        entry.map_or(-1.0, |entry| entry.entry.id.get() as f64)
                    })
                }),
                expected: Arc::new(|dataset| dataset.first_entry_id.get() as f64),
            },
            StorageReadBenchmark {
                name: "entry page scan (100)",
                run: Arc::new(|storage, _dataset| {
                    Box::pin(async move {
                        let query = EntryQuery {
                            conversation_id: ROOT_CONVERSATION_ID,
                            min_entry_id: None,
                            max_entry_id: None,
                            order: None,
                        };
                        let page = expect_benchmark(
                            storage.scan_entries(&query, 100, None, context()).await,
                            "entry page scan (100)",
                        );
                        page.items.len() as f64
                    })
                }),
                expected: Arc::new(|_dataset| 100.0),
            },
            StorageReadBenchmark {
                name: "filtered task scan (50)",
                run: Arc::new(|storage, _dataset| {
                    Box::pin(async move {
                        let query = TaskQuery {
                            kind: Some("benchmark.filtered".to_owned()),
                            status: Some(TaskStatus::Pending),
                            background: Some(true),
                            ..TaskQuery::default()
                        };
                        let page = expect_benchmark(
                            storage.scan_tasks(&query, 50, None, context()).await,
                            "filtered task scan (50)",
                        );
                        page.items.len() as f64
                    })
                }),
                expected: Arc::new(|dataset| dataset.filtered_task_count as f64),
            },
            StorageReadBenchmark {
                name: "exact document address among many",
                run: Arc::new(|storage, dataset| {
                    Box::pin(async move {
                        let address = DocumentAddress {
                            kind: "benchmark.family".to_owned(),
                            scope: DocumentScope::Session,
                            key: Some(Value::String(dataset.exact_document_key.clone())),
                        };
                        let record = expect_benchmark(
                            storage
                                .find_document(&address, DocumentPoint::Current, context())
                                .await,
                            "exact document address among many",
                        );
                        // `(await storage.findDocument(...))?.id ?? -1`。
                        record.map_or(-1.0, |record| record.id.get() as f64)
                    })
                }),
                expected: Arc::new(|dataset| dataset.exact_document_id.get() as f64),
            },
        ];
        // replay 组：4 个文档的当前 `count` 应等于各自 tail（0/16/128/1024）。
        for tail in REPLAY_TAILS {
            let name: &'static str = match tail {
                0 => "document replay tail (0)",
                16 => "document replay tail (16)",
                128 => "document replay tail (128)",
                _ => "document replay tail (1024)",
            };
            benchmarks.push(StorageReadBenchmark {
                name,
                run: Arc::new(move |storage, dataset| {
                    let id = dataset.replay_document_ids.get(&tail).copied();
                    Box::pin(async move {
                        match id {
                            Some(id) => document_count_of(
                                storage
                                    .document(id, DocumentPoint::Current, context())
                                    .await,
                                name,
                            ),
                            None => f64::NAN,
                        }
                    })
                }),
                expected: Arc::new(move |_dataset| tail as f64),
            });
        }
        // historical 组：同一文档在两个提交序号上的读，分别落在新 base 之前/之后。
        benchmarks.push(StorageReadBenchmark {
            name: "ancient historical read before newer base",
            run: Arc::new(|storage, dataset| {
                Box::pin(async move {
                    document_count_of(
                        storage
                            .document(
                                dataset.historical_document_id,
                                DocumentPoint::At(dataset.ancient_at),
                                context(),
                            )
                            .await,
                        "ancient historical read before newer base",
                    )
                })
            }),
            expected: Arc::new(|_dataset| HISTORY_SEGMENT_LENGTH as f64),
        });
        benchmarks.push(StorageReadBenchmark {
            name: "recent historical read after newer base",
            run: Arc::new(|storage, dataset| {
                Box::pin(async move {
                    document_count_of(
                        storage
                            .document(
                                dataset.historical_document_id,
                                DocumentPoint::At(dataset.recent_at),
                                context(),
                            )
                            .await,
                        "recent historical read after newer base",
                    )
                })
            }),
            expected: Arc::new(|_dataset| (HISTORY_SEGMENT_LENGTH * 2) as f64),
        });
        // fork 组：最深会话的可见范围扫描与祖先 head 查找。
        benchmarks.push(StorageReadBenchmark {
            name: "fork-depth history scan (100)",
            run: Arc::new(|storage, dataset| {
                Box::pin(async move {
                    let query = EntryQuery {
                        conversation_id: dataset.deepest_conversation_id,
                        min_entry_id: None,
                        max_entry_id: None,
                        order: None,
                    };
                    let page = expect_benchmark(
                        storage.scan_entries(&query, 100, None, context()).await,
                        "fork-depth history scan (100)",
                    );
                    page.items.len() as f64
                })
            }),
            expected: Arc::new(|_dataset| 100.0),
        });
        benchmarks.push(StorageReadBenchmark {
            name: "fork-depth head lookup",
            run: Arc::new(|storage, dataset| {
                Box::pin(async move {
                    // `(await storage.findLatestHeadMarker(...))?.id ?? -1`。
                    let entry = expect_benchmark(
                        storage
                            .find_latest_head_marker(
                                dataset.deepest_conversation_id,
                                None,
                                context(),
                            )
                            .await,
                        "fork-depth head lookup",
                    );
                    entry.map_or(-1.0, |entry| entry.id.get() as f64)
                })
            }),
            expected: Arc::new(|dataset| dataset.ancestor_head_entry_id.get() as f64),
        });
        benchmarks
    });

/// `seedStorageWriteBenchmark(storage)`：写基准每个样本的共同初始状态。
///
/// 写入根会话（ID 1）与 100 条 `benchmark.baseline` 条目，分两次 commit；写基准的每个样本都从
/// 这份状态开始，然后向同一 storage 追加自己的写入。
/// `tests/storage_conformance.rs` 锁定两次提交与条目的 ID/kind/index。
pub async fn seed_storage_write_benchmark(storage: &dyn Storage) -> Result<(), BoxError> {
    commit(
        storage,
        vec![write(json!({
            "type": "conversation",
            "value": { "id": ROOT_CONVERSATION_ID.get() },
        }))],
    )
    .await?;
    let mut writes = Vec::new();
    for index in 0..100 {
        let id = id_from_number::<crate::types::EntryBrand>(storage.mint_id(IdKind::Entry).await?);
        writes.push(write(json!({
            "type": "entry",
            "value": {
                "id": id.get(),
                "conversationId": ROOT_CONVERSATION_ID.get(),
                "kind": "benchmark.baseline",
                "data": { "index": index },
            },
        })));
    }
    commit(storage, writes).await?;
    Ok(())
}

/// `StorageWriteBenchmark`。
///
/// `run` 执行一次写并返回本次提交的写入条数，`expected` 是该条数的常量；存储错误在闭包内
/// `expect` 成 panic。调用方通常比较 `run` 的返回值与 `expected` 来确认基准数据正确。
#[derive(Clone)]
pub struct StorageWriteBenchmark {
    /// 基准名。
    pub name: &'static str,
    /// 期望写入条数。
    pub expected: f64,
    /// 运行一次并返回写入条数（TS 的 `run(storage): Promise<number>`）。
    pub run: Arc<dyn Fn(Arc<dyn Storage>) -> BenchmarkFuture + Send + Sync>,
}

impl std::fmt::Debug for StorageWriteBenchmark {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StorageWriteBenchmark")
            .field("name", &self.name)
            .field("expected", &self.expected)
            .finish_non_exhaustive()
    }
}

/// `STORAGE_WRITE_BENCHMARKS`。
///
/// 3 项：提交 1 条条目、100 条条目、混合 4 条（entry/task/submission/document）。名称与
/// `expected` 逐字来自 TS，`tests/storage_conformance.rs` 锁定；首次访问时构造。
pub static STORAGE_WRITE_BENCHMARKS: std::sync::LazyLock<Vec<StorageWriteBenchmark>> =
    std::sync::LazyLock::new(|| {
        vec![
            StorageWriteBenchmark {
                name: "commit one entry",
                expected: 1.0,
                run: Arc::new(|storage| {
                    Box::pin(async move {
                        let id = id_from_number::<crate::types::EntryBrand>(
                            storage.mint_id(IdKind::Entry).await.expect("mint entry"),
                        );
                        let writes = vec![write(json!({
                            "type": "entry",
                            "value": {
                                "id": id.get(),
                                "conversationId": ROOT_CONVERSATION_ID.get(),
                                "kind": "benchmark.write",
                                "data": { "text": "x".repeat(128) },
                            },
                        }))];
                        storage
                            .commit(&writes, adopt(), context())
                            .await
                            .expect("commit one entry");
                        1.0
                    })
                }),
            },
            StorageWriteBenchmark {
                name: "commit 100 entries",
                expected: 100.0,
                run: Arc::new(|storage| {
                    Box::pin(async move {
                        let mut writes = Vec::with_capacity(100);
                        for index in 0..100 {
                            let id = id_from_number::<crate::types::EntryBrand>(
                                storage.mint_id(IdKind::Entry).await.expect("mint entry"),
                            );
                            writes.push(write(json!({
                                "type": "entry",
                                "value": {
                                    "id": id.get(),
                                    "conversationId": ROOT_CONVERSATION_ID.get(),
                                    "kind": "benchmark.write",
                                    "data": { "index": index, "text": "x".repeat(128) },
                                },
                            })));
                        }
                        let count = writes.len() as f64;
                        storage
                            .commit(&writes, adopt(), context())
                            .await
                            .expect("commit 100 entries");
                        count
                    })
                }),
            },
            StorageWriteBenchmark {
                name: "commit mixed entry/task/submission/document",
                expected: 4.0,
                run: Arc::new(|storage| {
                    Box::pin(async move {
                        let entry_id = id_from_number::<crate::types::EntryBrand>(
                            storage.mint_id(IdKind::Entry).await.expect("mint entry"),
                        );
                        let task_id = id_from_number::<crate::types::TaskBrand>(
                            storage.mint_id(IdKind::Task).await.expect("mint task"),
                        );
                        let submission_id = id_from_number::<crate::types::SubmissionBrand>(
                            storage
                                .mint_id(IdKind::Submission)
                                .await
                                .expect("mint submission"),
                        );
                        let document_id = id_from_number::<crate::types::DocumentBrand>(
                            storage
                                .mint_id(IdKind::Document)
                                .await
                                .expect("mint document"),
                        );
                        let writes = vec![
                            write(json!({
                                "type": "entry",
                                "value": {
                                    "id": entry_id.get(),
                                    "conversationId": ROOT_CONVERSATION_ID.get(),
                                    "kind": "benchmark.mixed",
                                },
                            })),
                            write(json!({ "type": "task", "value": task(task_id, task_id.get()) })),
                            write(json!({
                                "type": "submission",
                                "value": {
                                    "id": submission_id.get(),
                                    "conversationId": ROOT_CONVERSATION_ID.get(),
                                    "requestId": format!("benchmark-{}", submission_id.get()),
                                    "type": "write",
                                    "status": "done",
                                    "entry": entry_id.get(),
                                },
                            })),
                            write(json!({
                                "type": "document.create",
                                "record": {
                                    "id": document_id.get(),
                                    "kind": "benchmark.mixed",
                                    "key": document_id.get().to_string(),
                                    "scope": { "kind": "session" },
                                },
                                "content": {
                                    "kind": "base",
                                    "version": 1,
                                    "value": { "entryId": entry_id.get(), "taskId": task_id.get() },
                                },
                            })),
                        ];
                        storage
                            .commit(&writes, adopt(), context())
                            .await
                            .expect("commit mixed");
                        4.0
                    })
                }),
            },
        ]
    });
