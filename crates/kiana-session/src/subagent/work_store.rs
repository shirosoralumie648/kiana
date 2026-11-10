//! Work 账本：Work 生命周期的权威记录与 durable 实现。
//!
//! Phase 0 的 [`super::work::WorkStore`] 只在终态归档结果；崩溃后无法区分“从未开始”
//! 和“运行中丢失”。本模块把记录提前到运行前写入（`running`），终态用一次 CAS 写入
//! （`terminal`），从而让重启后的 sweep 能识别并落定遗留 Work。
//!
//! 存储形态与 `background::durable` 一致：每个父会话一个 durable 文档
//! `kiana.work_records`，由 `Session`/`Tx` 提供原子读改写；`nextWorkId` 也放在同一
//! 文档里用事务分配，避免跨进程从 1 开始重复。这里不实现队列或第二套调度器：
//! 记录只有 `running` / `terminal` 两态，执行仍由 `AgentSession` 在 LocalSet 上驱动。
//!
//! 记录字段与 durable `TaskRecord` 的 identity/attempt/timestamps 词汇对齐，但不使用
//! `TaskScheduler`（它绑定完整 Harness 且 phase handler 要求 `Send`，见
//! `docs/adr/work-orchestration.md` Phase 1 调研结论）。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex};

use async_trait::async_trait;
use kiana_durable::{
    BoxError, DocDefinition, DocTarget, DocToken, DocumentSemantics, SessionChange, Tx, define_doc,
};
use kiana_durable::{JsonObject, JsonValue};
use serde::de::Error as _;
use serde::{Deserialize, Serialize};
use serde_json::Map;

use crate::durable_store::DurableSessionStore;
use crate::subagent::work::{
    RunId, SystemWorkClock, WorkClock, WorkId, WorkRequest, WorkResult, WorkStore, WorkStoreError,
};
use crate::subagent::work_graph::{WorkGraph, WorkGraphSet};

/// Work 记录 schema 版本。
pub const WORK_RECORD_VERSION: u32 = 1;

static WORK_DOC: LazyLock<DocToken> = LazyLock::new(|| {
    define_doc(
        DocDefinition::new("kiana.work_records", 1, DocumentSemantics::SESSION).with_initial(
            |_| {
                let mut value = JsonObject::new();
                value.insert("nextWorkId".to_owned(), JsonValue::from(1_i64));
                value.insert("records".to_owned(), JsonValue::Object(Map::new()));
                value
            },
        ),
    )
});

fn work_target() -> DocTarget {
    DocTarget::session(&WORK_DOC)
}

fn records_of(value: &JsonObject) -> Option<&Map<String, JsonValue>> {
    value.get("records").and_then(JsonValue::as_object)
}

/// 记录里的 `resumedFrom`。
///
/// 编码时身份字段写成十进制字符串；这里兼容旧记录的数字形态。只读这一个字段是
/// 为了在 `begin` 的锚点扫描里避免把每条记录都完整反序列化一遍。
///
/// 无法解析或类型非法时返回错误而不是 `None`：损坏的锚点字段若被当成“没有后继”，
/// one-anchor-one-successor 守卫会 fail open，允许创建第二个 successor。
fn resumed_from_of(value: &JsonValue) -> Result<Option<WorkId>, WorkStoreError> {
    match value.get("resumedFrom") {
        None | Some(JsonValue::Null) => Ok(None),
        Some(JsonValue::String(text)) => text.parse().map(|id| Some(WorkId(id))).map_err(|_| {
            WorkStoreError::new(format!("work record has an invalid `resumedFrom` `{text}`"))
        }),
        Some(JsonValue::Number(number)) => number
            .as_i64()
            .map(|id| Some(WorkId(id)))
            .ok_or_else(|| WorkStoreError::new("work record has a non-integer `resumedFrom`")),
        Some(_) => Err(WorkStoreError::new(
            "work record has a non-scalar `resumedFrom`",
        )),
    }
}

/// 读取侧的 fail-closed 校验：账本文档永远带 `records` 对象，缺失即损坏。
fn records_of_ledger(value: &JsonObject) -> Result<&Map<String, JsonValue>, WorkStoreError> {
    records_of(value)
        .ok_or_else(|| WorkStoreError::new("work ledger is missing the `records` object"))
}

/// 账本中现有记录的最大 `WorkId`（空账本为 0）。
///
/// 记录 key 只应是我们写出的十进制 workId；其它形态说明账本被外部改写，无法
/// 安全判断“下一个未使用的 id”，按损坏账本 fail closed。
fn max_record_work_id(records: &Map<String, JsonValue>) -> Result<i64, BoxError> {
    let mut max = 0i64;
    for key in records.keys() {
        let id = key.parse::<i64>().map_err(|_| -> BoxError {
            Box::new(std::io::Error::other(format!(
                "work ledger has a non-numeric record key `{key}`"
            )))
        })?;
        max = max.max(id);
    }
    Ok(max)
}

/// `nextWorkId` 既接受旧记录的数字形态，也接受新记录的十进制字符串形态。
///
/// 字段缺失只可能是旧版账本：从现有记录的最大 id 之后继续发号，**不能**回落到
/// 1——那会重新发出账本里已经存在的 workId，让 `begin` 命中旧记录或覆盖它。
/// 字段存在但非法（类型错误、无法解析、小于 1、落后于现有记录）说明计数器已损坏，
/// fail closed；静默修正会把损坏转成 id 复用。
fn next_work_id_of(value: &JsonObject) -> Result<i64, BoxError> {
    let records = records_of_ledger(value)?;
    let max_existing = max_record_work_id(records)?;
    let Some(stored) = value.get("nextWorkId") else {
        return Ok(max_existing.saturating_add(1).max(1));
    };
    let parsed = match stored {
        JsonValue::Number(number) => number.as_i64(),
        JsonValue::String(text) => text.parse().ok(),
        _ => None,
    };
    let Some(next) = parsed else {
        return Err(Box::new(std::io::Error::other(
            "work ledger has an invalid `nextWorkId`",
        )));
    };
    if next < 1 {
        return Err(Box::new(std::io::Error::other(
            "work ledger has an invalid `nextWorkId`",
        )));
    }
    if next <= max_existing {
        return Err(Box::new(std::io::Error::other(
            "work ledger `nextWorkId` is behind existing records",
        )));
    }
    Ok(next)
}

/// 写路径与读路径使用同一 fail-closed 校验：缺失 `records` 的文档是损坏账本，
/// 不能由 `begin` 静默重建一个空表（那会丢掉旧记录并绕过锚点检查）。
fn records_mut(value: &mut JsonObject) -> Result<&mut Map<String, JsonValue>, BoxError> {
    value
        .get_mut("records")
        .and_then(JsonValue::as_object_mut)
        .ok_or_else(|| -> BoxError {
            Box::new(std::io::Error::other(
                "work ledger is missing the `records` object",
            ))
        })
}

/// 把 durable 文档里的 `workId` / `resumedFrom` 还原成数字。
///
/// durable JSONL 与 JS `JSON.stringify` 一样按 `f64` 写数字，> 2^53 的 `WorkId`
/// 会丢精度。编码时账本正文里的这些身份字段写成十进制字符串（记录 key 本来就是
/// 字符串），解码时再还原；模型输出等任意 JSON 里的同名键不受影响。
fn decode_record(value: &JsonValue) -> Result<WorkRecord, serde_json::Error> {
    let mut value = value.clone();
    numeric_work_ids(&mut value)?;
    rewrite_ledger_numbers(&mut value, &LedgerNumbers::ParseUnsafe)?;
    serde_json::from_value(value)
}

fn encode_record(record: &WorkRecord) -> Result<JsonValue, BoxError> {
    let mut value =
        serde_json::to_value(record).map_err(|error| -> BoxError { Box::new(error) })?;
    // `serde_json` 把 `inf`/`NaN` 写成 `null`；这样的记录无法再解码（`cost: f64`
    // 拒绝 null），一条坏 usage 会让整个账本永久 fail-closed。落盘前统一写成 0。
    if let Some(cost) = value
        .get_mut("state")
        .and_then(JsonValue::as_object_mut)
        .and_then(|state| state.get_mut("result"))
        .and_then(JsonValue::as_object_mut)
        .and_then(|result| result.get_mut("usage"))
        .and_then(JsonValue::as_object_mut)
        .and_then(|usage| usage.get_mut("cost"))
        && cost.is_null()
    {
        *cost = JsonValue::from(0.0);
    }
    string_work_ids(&mut value);
    rewrite_ledger_numbers(&mut value, &LedgerNumbers::StringifyUnsafe)?;
    Ok(value)
}

/// JS 能精确表示的最大整数（`Number.MAX_SAFE_INTEGER`）。
///
/// durable JSONL 与 `JSON.stringify` 一样把数字写成 f64；超过它的 u64 落盘后会被
/// 舍入，重启时可能超出 u64 范围，让整条记录解码失败（`list`/`recover_work` 随之
/// fail-closed）。账本把这些字段在超限时写成十进制字符串，未超限保持数字，正常
/// 记录的落盘形状不变。
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

enum LedgerNumbers {
    /// 序列化：超限的 u64 改成字符串。
    StringifyUnsafe,
    /// 反序列化：把这些字符串还原成数字（未超限的记录本来就是数字）。
    ParseUnsafe,
}

/// 只改写账本自身的已知数值字段：`WorkResult.terminal.output` 是宿主提供的任意
/// JSON，不能递归替换其中的数字。
fn rewrite_ledger_numbers(
    value: &mut JsonValue,
    mode: &LedgerNumbers,
) -> Result<(), serde_json::Error> {
    let Some(object) = value.as_object_mut() else {
        return Ok(());
    };
    for key in ["createdAtMs", "updatedAtMs"] {
        if let Some(field) = object.get_mut(key) {
            rewrite_ledger_number(field, mode)?;
        }
    }
    if let Some(request) = object.get_mut("request").and_then(JsonValue::as_object_mut) {
        for key in ["tokenBudget", "timeoutSeconds", "maxCalls"] {
            if let Some(field) = request.get_mut(key) {
                rewrite_ledger_number(field, mode)?;
            }
        }
    }
    if let Some(state) = object.get_mut("state").and_then(JsonValue::as_object_mut) {
        if let Some(field) = state.get_mut("terminalSeq") {
            rewrite_ledger_number(field, mode)?;
        }
        if let Some(result) = state.get_mut("result").and_then(JsonValue::as_object_mut) {
            for key in ["startedAtMs", "finishedAtMs"] {
                if let Some(field) = result.get_mut(key) {
                    rewrite_ledger_number(field, mode)?;
                }
            }
            if let Some(usage) = result.get_mut("usage").and_then(JsonValue::as_object_mut) {
                for key in [
                    "inputTokens",
                    "outputTokens",
                    "cacheReadTokens",
                    "cacheWriteTokens",
                    "totalTokens",
                    "toolCalls",
                    "turns",
                ] {
                    if let Some(field) = usage.get_mut(key) {
                        rewrite_ledger_number(field, mode)?;
                    }
                }
            }
            if let Some(evidence) = result.get_mut("evidence").and_then(JsonValue::as_array_mut) {
                for item in evidence {
                    if let Some(field) = item.get_mut("seq") {
                        rewrite_ledger_number(field, mode)?;
                    }
                }
            }
        }
    }
    Ok(())
}

fn rewrite_ledger_number(
    value: &mut JsonValue,
    mode: &LedgerNumbers,
) -> Result<(), serde_json::Error> {
    match mode {
        LedgerNumbers::StringifyUnsafe => {
            if let Some(number) = value.as_u64()
                && number > MAX_SAFE_INTEGER
            {
                *value = JsonValue::String(number.to_string());
            }
        }
        LedgerNumbers::ParseUnsafe => {
            let Some(text) = value.as_str() else {
                return Ok(());
            };
            let number: u64 = text.parse().map_err(|_| {
                serde_json::Error::custom(format!("invalid unsigned integer `{text}`"))
            })?;
            *value = JsonValue::from(number);
        }
    }
    Ok(())
}

fn work_id_to_string(value: &mut JsonValue) {
    if let Some(id) = value.as_i64() {
        *value = JsonValue::String(id.to_string());
    }
}

fn work_id_from_string(value: &mut JsonValue) -> Result<(), serde_json::Error> {
    let Some(text) = value.as_str() else {
        return Ok(());
    };
    let id: i64 = text
        .parse()
        .map_err(|_| serde_json::Error::custom(format!("invalid work id `{text}`")))?;
    *value = JsonValue::from(id);
    Ok(())
}

/// 只改写账本自身的身份字段；`WorkResult.terminal.output` 是任意 JSON，不能递归替换。
fn string_work_ids(value: &mut JsonValue) {
    let Some(object) = value.as_object_mut() else {
        return;
    };
    if let Some(id) = object.get_mut("workId") {
        work_id_to_string(id);
    }
    if let Some(id) = object.get_mut("resumedFrom") {
        work_id_to_string(id);
    }
    if let Some(request) = object.get_mut("request").and_then(JsonValue::as_object_mut)
        && let Some(id) = request.get_mut("workId")
    {
        work_id_to_string(id);
    }
    if let Some(result) = object
        .get_mut("state")
        .and_then(JsonValue::as_object_mut)
        .and_then(|state| state.get_mut("result"))
        .and_then(JsonValue::as_object_mut)
        && let Some(id) = result.get_mut("workId")
    {
        work_id_to_string(id);
    }
}

fn numeric_work_ids(value: &mut JsonValue) -> Result<(), serde_json::Error> {
    let Some(object) = value.as_object_mut() else {
        return Ok(());
    };
    if let Some(id) = object.get_mut("workId") {
        work_id_from_string(id)?;
    }
    if let Some(id) = object.get_mut("resumedFrom") {
        work_id_from_string(id)?;
    }
    if let Some(request) = object.get_mut("request").and_then(JsonValue::as_object_mut)
        && let Some(id) = request.get_mut("workId")
    {
        work_id_from_string(id)?;
    }
    if let Some(result) = object
        .get_mut("state")
        .and_then(JsonValue::as_object_mut)
        .and_then(|state| state.get_mut("result"))
        .and_then(JsonValue::as_object_mut)
        && let Some(id) = result.get_mut("workId")
    {
        work_id_from_string(id)?;
    }
    Ok(())
}

/// Work 的持久状态。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "status",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum WorkState {
    /// 正在运行；`sessionFile` 在准备运行时补齐，供显式恢复使用。
    Running {
        /// 子代理持久会话文件（内存会话为 `None`）。
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            alias = "session_file"
        )]
        session_file: Option<String>,
    },
    /// 已终态；`result` 是权威结果。
    Terminal {
        /// 不可变结果。
        result: Box<WorkResult>,
        /// 终态序号（running 为 0，进入终态后为 1）。
        #[serde(alias = "terminal_seq")]
        terminal_seq: u64,
    },
}

/// 一条 Work 生命周期记录。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkRecord {
    /// 记录 schema 版本。
    pub version: u32,
    /// Work 协议版本。
    pub protocol: u32,
    /// Work 身份。
    pub work_id: WorkId,
    /// 本次执行身份。
    pub run_id: RunId,
    /// 父会话 id。
    pub parent_session_id: String,
    /// 父 turn id（当前没有稳定的 turn 身份时为 `None`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_turn_id: Option<String>,
    /// 显式恢复时指向原 Work。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resumed_from: Option<WorkId>,
    /// 提交时的请求快照。
    pub request: WorkRequest,
    /// 生命周期状态。
    pub state: WorkState,
    /// 已尝试次数（含未来的显式恢复）。
    pub attempts: u32,
    /// 创建时间（Unix 毫秒）。
    pub created_at_ms: u64,
    /// 更新时间（Unix 毫秒）。
    pub updated_at_ms: u64,
    /// 写入记录的进程 id。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    /// 创建 Work 时声明的隔离状态目录（仅自定义 `IsolationOptions.state_dir` 时）。
    ///
    /// 子会话 header 是启动 sweep 推导状态目录的主要线索，但它要等首条消息落盘
    /// 才存在；`begin`/`attach_session` 之后、首条消息之前崩溃的 Work 否则会让
    /// 自定义目录里的 worktree/clone 永久泄漏。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isolation_state_dir: Option<PathBuf>,
    /// 后台执行时对应的 `BackgroundTaskRecord.taskId`。
    ///
    /// 正常路径由后台任务闭包在 `begin` 前写入；启动恢复用它把 Work 账本与后台
    /// attempt 记录对齐（见 `AgentSession::recover_work` 的后台对账）。前台的
    /// Work 没有这个字段。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background_task_id: Option<String>,
}

impl WorkRecord {
    /// 从请求构造一条 Running 记录。
    pub fn new(
        request: WorkRequest,
        parent_session_id: impl Into<String>,
        parent_turn_id: Option<String>,
        pid: Option<u32>,
        now_ms: u64,
    ) -> Self {
        Self {
            version: WORK_RECORD_VERSION,
            protocol: request.protocol,
            work_id: request.work_id,
            run_id: request.run_id.clone(),
            parent_session_id: parent_session_id.into(),
            parent_turn_id,
            resumed_from: None,
            request,
            state: WorkState::Running { session_file: None },
            attempts: 1,
            created_at_ms: now_ms,
            updated_at_ms: now_ms,
            pid,
            isolation_state_dir: None,
            background_task_id: None,
        }
    }

    /// 设置 `resumed_from`。
    pub fn with_resumed_from(mut self, work_id: WorkId) -> Self {
        self.resumed_from = Some(work_id);
        self
    }

    /// 记录自定义隔离状态目录。
    pub fn with_isolation_state_dir(mut self, state_dir: PathBuf) -> Self {
        self.isolation_state_dir = Some(state_dir);
        self
    }

    /// 关联后台任务 attempt。
    pub fn with_background_task_id(mut self, task_id: impl Into<String>) -> Self {
        self.background_task_id = Some(task_id.into());
        self
    }

    /// 是否终态。
    pub fn is_terminal(&self) -> bool {
        matches!(self.state, WorkState::Terminal { .. })
    }

    /// 稳定状态字符串。
    pub fn status(&self) -> &'static str {
        match self.state {
            WorkState::Running { .. } => "running",
            WorkState::Terminal { .. } => "terminal",
        }
    }

    /// 持久会话文件（Running 期或终态结果里）。
    pub fn session_file(&self) -> Option<&str> {
        match &self.state {
            WorkState::Running { session_file } => session_file.as_deref(),
            WorkState::Terminal { result, .. } => result.session_file.as_deref(),
        }
    }

    /// 终态结果。
    pub fn terminal_result(&self) -> Option<&WorkResult> {
        match &self.state {
            WorkState::Terminal { result, .. } => Some(result.as_ref()),
            WorkState::Running { .. } => None,
        }
    }

    /// 终态序号（Running 为 0）。
    pub fn terminal_seq(&self) -> u64 {
        match &self.state {
            WorkState::Running { .. } => 0,
            WorkState::Terminal { terminal_seq, .. } => *terminal_seq,
        }
    }
}

/// `begin` 的结果。
#[derive(Debug, Clone, PartialEq)]
pub enum WorkBegin {
    /// 新记录已写入（Running）。
    Started(WorkRecord),
    /// 同 `workId` 已存在；调用方不得覆盖或重复执行。
    Existing(WorkRecord),
    /// 同一个恢复锚点已经有后继 Work；并发 `resume_work` 的输家不得再建一条。
    AlreadyResumed(WorkRecord),
}

/// `finish` 的结果。
#[derive(Debug, Clone, PartialEq)]
pub enum WorkFinish {
    /// 本次调用赢下 Running → Terminal 的 CAS。
    Finished(WorkRecord),
    /// 记录已终态或 run id 不匹配；本次没有写入。
    NotApplied(WorkRecord),
    /// 记录不存在。
    Missing,
}

/// `recover_work` 的报告。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkRecoveryReport {
    /// 被本次恢复落定为中断的任务。
    pub terminalized: Vec<WorkId>,
    /// 处于 `resume` 策略、等待宿主显式恢复的任务。
    pub resumable: Vec<WorkId>,
    /// 进程仍存活而跳过的任务。
    pub skipped_live: Vec<WorkId>,
}

/// 进程内活 Work 登记表（带引用计数）。
///
/// 与 `background::live_tasks` 同因：同一进程的多个会话共享 pid，仅凭 pid 无法区分
/// “另一个会话仍在跑的任务”与“本会话崩溃后留下的记录”。
///
/// 计数而非集合：`workId` 是宿主可覆盖的幂等键，同一 `(parent, workId, runId)` 可能
/// 被并发登记两次（第二次会被 `begin` 以 `Existing` 拒绝）。如果 Drop 直接删除键，
/// 输家的 guard 会把赢家仍在运行的任务注销，让 `recover_work` 把它误判为崩溃遗留。
fn live_works() -> &'static Mutex<HashMap<LiveWorkKey, usize>> {
    static LIVE_WORKS: std::sync::OnceLock<Mutex<HashMap<LiveWorkKey, usize>>> =
        std::sync::OnceLock::new();
    LIVE_WORKS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 活 Work 登记键：(父会话 id, WorkId, RunId)。
type LiveWorkKey = (String, i64, String);

/// 运行期间的活 Work 占位；drop（完成、失败、panic）时自动注销。
pub(crate) struct WorkLiveGuard {
    key: LiveWorkKey,
}

impl WorkLiveGuard {
    /// 登记一次运行。
    pub(crate) fn register(parent_session_id: &str, work_id: WorkId, run_id: &RunId) -> Self {
        let key = (parent_session_id.to_owned(), work_id.0, run_id.0.clone());
        let mut works = live_works()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *works.entry(key.clone()).or_insert(0) += 1;
        Self { key }
    }
}

impl Drop for WorkLiveGuard {
    fn drop(&mut self) {
        let mut works = live_works()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(count) = works.get_mut(&self.key) else {
            return;
        };
        if *count > 1 {
            *count -= 1;
            return;
        }
        works.remove(&self.key);
    }
}

/// 进程内是否仍有同一 Work run 在跑。
pub(crate) fn work_is_live(parent_session_id: &str, work_id: WorkId, run_id: &RunId) -> bool {
    live_works()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .contains_key(&(parent_session_id.to_owned(), work_id.0, run_id.0.clone()))
}

/// 进程内是否还有**任何** Work 在跑。
///
/// 干净关停前用它决定能否释放 durable 目录租约：在途 Work 仍会写账本与
/// transcript，提前释放会让这些写入落进一个即将被下一个进程接管的目录。
pub(crate) fn any_work_is_live() -> bool {
    !live_works()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .is_empty()
}

/// 一次 durable 提交里的操作。
enum WorkOp {
    /// 分配一个会话内唯一的 `WorkId`。
    Allocate,
    /// 写入 Running 记录或返回已有记录。
    Begin(Box<WorkRecord>),
    /// 补齐 `sessionFile`。
    AttachSession {
        work_id: WorkId,
        run_id: RunId,
        session_file: String,
        updated_at_ms: u64,
    },
    /// 清除后继记录的 `resumedFrom`，把锚点还给宿主重试。
    ReleaseResume {
        work_id: WorkId,
        run_id: RunId,
        updated_at_ms: u64,
    },
    /// Running → Terminal 的 CAS。
    ///
    /// `run_id` 为 `None` 表示结果没有执行身份（`Skipped`/依赖失败）：此时只对
    /// Running 状态做 CAS，不比较 run id。
    Finish {
        work_id: WorkId,
        run_id: Option<RunId>,
        result: Box<WorkResult>,
        updated_at_ms: u64,
    },
}

impl WorkOp {
    /// 稳定的 operation 名（telemetry 属性）。
    fn name(&self) -> &'static str {
        match self {
            Self::Allocate => "allocate",
            Self::Begin(_) => "begin",
            Self::AttachSession { .. } => "attach_session",
            Self::ReleaseResume { .. } => "release_resume",
            Self::Finish { .. } => "finish",
        }
    }

    /// 本次提交涉及的 Work 身份。
    fn identity(&self) -> Option<(WorkId, Option<RunId>)> {
        match self {
            Self::Allocate => None,
            Self::Begin(record) => Some((record.work_id, Some(record.run_id.clone()))),
            Self::AttachSession {
                work_id, run_id, ..
            }
            | Self::ReleaseResume {
                work_id, run_id, ..
            } => Some((*work_id, Some(run_id.clone()))),
            Self::Finish {
                work_id, run_id, ..
            } => Some((*work_id, run_id.clone())),
        }
    }
}

struct WorkChange {
    op: WorkOp,
}

#[async_trait]
impl SessionChange for WorkChange {
    async fn run(&self, tx: &dyn Tx) -> Result<Box<dyn std::any::Any + Send>, BoxError> {
        let mut draft = tx.doc(&work_target()).await?;
        let value = draft.value_mut();
        match &self.op {
            WorkOp::Allocate => {
                // 损坏账本不能继续分配编号：`next_work_id_of` 同时校验 `records`
                // 与计数器，失败时调用方不会拿到一个随后 `begin` 必失败的 id。
                let current = next_work_id_of(value)?;
                let Some(next) = current.checked_add(1) else {
                    return Err(Box::new(std::io::Error::other("work id exhausted")));
                };
                value.insert("nextWorkId".to_owned(), JsonValue::String(next.to_string()));
                Ok(Box::new(Some(WorkId(current))))
            }
            WorkOp::Begin(record) => {
                if record.work_id != record.request.work_id
                    || record.run_id != record.request.run_id
                {
                    return Err(Box::new(std::io::Error::other(
                        "work record identity does not match request",
                    )));
                }
                if !matches!(record.state, WorkState::Running { .. }) {
                    return Err(Box::new(std::io::Error::other(
                        "work record must start in running state",
                    )));
                }
                let key = record.work_id.0.to_string();
                if let Some(stored) = records_of(value).and_then(|records| records.get(&key)) {
                    let existing =
                        decode_record(stored).map_err(|error| -> BoxError { Box::new(error) })?;
                    return Ok(Box::new(WorkBegin::Existing(existing)));
                }
                // 一个恢复锚点只能有一个后继：并发的 `resume_work` 如果不是在
                // 同一个事务里检查，会各自建一条记录并在同一 transcript 上跑两个
                // 运行时。检查与插入都在本事务内，因此是原子的。
                if let Some(origin) = record.resumed_from {
                    for stored in records_of(value).into_iter().flat_map(|map| map.values()) {
                        if resumed_from_of(stored)
                            .map_err(|error| -> BoxError { Box::new(error) })?
                            != Some(origin)
                        {
                            continue;
                        }
                        // 只有命中锚点时才是这次 begin 的拒绝理由。
                        let existing = decode_record(stored)
                            .map_err(|error| -> BoxError { Box::new(error) })?;
                        return Ok(Box::new(WorkBegin::AlreadyResumed(existing)));
                    }
                }
                // 显式 workId 可能大于事务计数器；把计数器推到它之后，避免下一次
                // `allocate_work_id` 返回一个已经存在的 id。`i64::MAX` 没有可推进的
                // 计数器（写回 max 会让 `next_work_id_of` 以“behind existing
                // records”拒绝之后所有 begin/allocate），因此在写入前就报告耗尽。
                let next = record.work_id.0.checked_add(1).ok_or_else(|| -> BoxError {
                    Box::new(std::io::Error::other("work id exhausted"))
                })?;
                if next > next_work_id_of(value)? {
                    value.insert("nextWorkId".to_owned(), JsonValue::String(next.to_string()));
                }
                let encoded = encode_record(record)?;
                records_mut(value)?.insert(key, encoded);
                Ok(Box::new(WorkBegin::Started(record.as_ref().clone())))
            }
            WorkOp::AttachSession {
                work_id,
                run_id,
                session_file,
                updated_at_ms,
            } => {
                let key = work_id.0.to_string();
                let Some(stored) = records_of(value)
                    .and_then(|records| records.get(&key))
                    .cloned()
                else {
                    return Ok(Box::new(None::<WorkRecord>));
                };
                let mut record =
                    decode_record(&stored).map_err(|error| -> BoxError { Box::new(error) })?;
                if record.run_id != *run_id {
                    return Ok(Box::new(None::<WorkRecord>));
                }
                let WorkState::Running {
                    session_file: current,
                } = &mut record.state
                else {
                    return Ok(Box::new(None::<WorkRecord>));
                };
                *current = Some(session_file.clone());
                record.updated_at_ms = *updated_at_ms;
                let encoded = encode_record(&record)?;
                records_mut(value)?.insert(key, encoded);
                Ok(Box::new(Some(record)))
            }
            WorkOp::ReleaseResume {
                work_id,
                run_id,
                updated_at_ms,
            } => {
                let key = work_id.0.to_string();
                let Some(stored) = records_of(value)
                    .and_then(|records| records.get(&key))
                    .cloned()
                else {
                    return Ok(Box::new(false));
                };
                let mut record =
                    decode_record(&stored).map_err(|error| -> BoxError { Box::new(error) })?;
                if record.run_id != *run_id || record.resumed_from.is_none() {
                    return Ok(Box::new(false));
                }
                // 记录本身还有后继时不能归还锚点：后继（或它的后继）可能正在写同一
                // transcript/工作区，提前释放会让上游记录被并发恢复。检查与清除在
                // 同一条事务里完成，`begin` 无法在快照之间插入新的后继。
                for other in records_of(value)
                    .into_iter()
                    .flat_map(|records| records.values())
                {
                    if resumed_from_of(other).map_err(|error| -> BoxError { Box::new(error) })?
                        == Some(*work_id)
                    {
                        return Ok(Box::new(false));
                    }
                }
                record.resumed_from = None;
                record.updated_at_ms = *updated_at_ms;
                let encoded = encode_record(&record)?;
                records_mut(value)?.insert(key, encoded);
                Ok(Box::new(true))
            }
            WorkOp::Finish {
                work_id,
                run_id,
                result,
                updated_at_ms,
            } => {
                let key = work_id.0.to_string();
                let Some(stored) = records_of(value)
                    .and_then(|records| records.get(&key))
                    .cloned()
                else {
                    return Ok(Box::new(WorkFinish::Missing));
                };
                let mut record =
                    decode_record(&stored).map_err(|error| -> BoxError { Box::new(error) })?;
                let run_matches = run_id
                    .as_ref()
                    .is_none_or(|run_id| record.run_id == *run_id);
                if !run_matches || record.is_terminal() {
                    return Ok(Box::new(WorkFinish::NotApplied(record)));
                }
                record.state = WorkState::Terminal {
                    result: result.clone(),
                    terminal_seq: record.terminal_seq().saturating_add(1),
                };
                record.updated_at_ms = *updated_at_ms;
                let encoded = encode_record(&record)?;
                records_mut(value)?.insert(key, encoded);
                Ok(Box::new(WorkFinish::Finished(record)))
            }
        }
    }
}

fn downcast<T: 'static>(value: Box<dyn std::any::Any + Send>) -> Result<T, WorkStoreError> {
    value
        .downcast::<T>()
        .map(|boxed| *boxed)
        .map_err(|_| WorkStoreError::new("unexpected work store commit value"))
}

/// 基于 `kiana-durable` 的 Work 账本。
///
/// 构造本身不触碰文件系统；第一次 `begin` / `allocate_work_id` / `load` 才按父会话
/// 打开 `<root>/<session-key>/` 下的 JSONL 存储。
pub struct DurableWorkStore {
    store: DurableSessionStore,
    clock: Arc<dyn WorkClock>,
    /// 编排层 telemetry 出口。`None` 时 commit 不产生 span；telemetry 永远不影响
    /// 账本提交结果。
    telemetry: std::sync::RwLock<Option<Arc<dyn kiana_telemetry::TelemetryContext>>>,
}

impl std::fmt::Debug for DurableWorkStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DurableWorkStore")
            .field("root", &self.store.root())
            .finish_non_exhaustive()
    }
}

impl DurableWorkStore {
    /// 在 `root` 下创建 Work 账本。
    pub fn new(root: impl Into<std::path::PathBuf>) -> Self {
        Self {
            store: DurableSessionStore::new(root),
            clock: Arc::new(SystemWorkClock),
            telemetry: std::sync::RwLock::new(None),
        }
    }

    /// 使用指定时钟（测试可注入固定值）。
    pub fn with_clock(mut self, clock: Arc<dyn WorkClock>) -> Self {
        self.clock = clock;
        self
    }

    /// 根目录。
    pub fn root(&self) -> &std::path::Path {
        self.store.root()
    }

    /// 注入编排层 telemetry 出口；传入后每次账本提交会追加一个
    /// `kiana.work.commit` span（`kiana.work.*` 属性）。
    ///
    /// 遥测是诊断：span 失败或适配器拒绝不会改变提交结果。
    pub fn set_telemetry(&self, context: Arc<dyn kiana_telemetry::TelemetryContext>) {
        *self
            .telemetry
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(context);
    }

    /// 释放缓存的会话槽位，让出目录租约（干净关停用）。
    ///
    /// 返回 `false` 表示有并发打开正在进行；租约最终由进程退出释放。
    pub(crate) fn release_sessions(&self) -> bool {
        self.store.release_sessions_blocking()
    }

    /// 清除后继记录的 `resumedFrom`（后继在真正运行前失败时调用）。
    pub(crate) async fn release_resume(
        &self,
        parent_session_id: &str,
        work_id: WorkId,
        run_id: &RunId,
    ) -> Result<bool, WorkStoreError> {
        let value = self
            .commit(
                parent_session_id,
                WorkOp::ReleaseResume {
                    work_id,
                    run_id: run_id.clone(),
                    updated_at_ms: self.clock.now_ms(),
                },
            )
            .await?;
        downcast::<bool>(value)
    }

    /// 扫描根目录下所有会话的 Work 记录（启动 sweep 收集 keep-set 用）。
    ///
    /// `.kiana/subagents` 工作区按仓库共享，不能只看当前会话：其它会话里仍未终态的
    /// Work 同样引用着工作区。`all_sessions` 打开单个目录失败时只跳过该目录，因此
    /// `Err` 只覆盖“所有目录都打不开”；调用方仍要把任何错误当作“不要回收”。
    ///
    /// 返回 `(records, skipped)`：`skipped` 是本次没读到的会话目录数（被另一个存活
    /// 进程持有或打开失败）。非零时 keep-set 不完整，调用方必须放弃回收。
    pub(crate) async fn list_all_sessions(
        &self,
    ) -> Result<(Vec<WorkRecord>, usize), WorkStoreError> {
        let (sessions, skipped) = self
            .store
            .all_sessions_metered()
            .await
            .map_err(WorkStoreError::new)?;
        let mut records = Vec::new();
        for (_name, slot) in sessions {
            let Some(value) = slot
                .snapshot(&work_target())
                .await
                .map_err(WorkStoreError::new)?
            else {
                continue;
            };
            for stored in records_of_ledger(&value)?.values() {
                records.push(decode_record(stored).map_err(|error| {
                    WorkStoreError::new(format!("decoding work record: {error}"))
                })?);
            }
        }
        Ok((records, skipped))
    }

    /// 构建单个父会话的 Work 图快照。
    pub(crate) async fn graph_for_session(
        &self,
        parent_session_id: &str,
    ) -> Result<WorkGraph, WorkStoreError> {
        let records = self.list(parent_session_id, false).await?;
        Ok(WorkGraph::from_records(records))
    }

    /// 构建所有可读会话的 Work 图集合。`skipped_sessions` 非零表示图不完整。
    pub(crate) async fn graph_set(&self) -> Result<WorkGraphSet, WorkStoreError> {
        let (records, skipped) = self.list_all_sessions().await?;
        let mut by_session: std::collections::BTreeMap<String, Vec<WorkRecord>> =
            std::collections::BTreeMap::new();
        for record in records {
            by_session
                .entry(record.parent_session_id.clone())
                .or_default()
                .push(record);
        }
        Ok(WorkGraphSet {
            graphs: by_session
                .into_values()
                .map(WorkGraph::from_records)
                .collect(),
            skipped_sessions: skipped,
        })
    }

    async fn commit(
        &self,
        session_id: &str,
        op: WorkOp,
    ) -> Result<Box<dyn std::any::Any + Send>, WorkStoreError> {
        let operation = op.name();
        let identity = op.identity();
        let started = std::time::Instant::now();
        let result = self
            .store
            .commit(session_id, &WorkChange { op })
            .await
            .map_err(WorkStoreError::new);
        self.record_commit_span(
            session_id,
            operation,
            identity,
            started,
            result.as_ref().err().map(|error| error.message.clone()),
        )
        .await;
        result
    }

    /// 记账本提交的编排层 span。放在提交之后：遥测失败不能吞掉或回滚事务。
    async fn record_commit_span(
        &self,
        session_id: &str,
        operation: &'static str,
        identity: Option<(WorkId, Option<RunId>)>,
        started: std::time::Instant,
        error: Option<String>,
    ) {
        use kiana_telemetry::TelemetryContextExt as _;

        let Some(telemetry) = self
            .telemetry
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
        else {
            return;
        };
        let mut attributes = kiana_telemetry::SpanAttributes::new();
        attributes.insert(
            "kiana.work.operation".to_string(),
            kiana_telemetry::AttributeValue::String(operation.to_string()),
        );
        attributes.insert(
            "kiana.work.parent_session_id".to_string(),
            kiana_telemetry::AttributeValue::String(session_id.to_string()),
        );
        if let Some((work_id, run_id)) = identity {
            attributes.insert(
                "kiana.work.id".to_string(),
                kiana_telemetry::AttributeValue::String(work_id.0.to_string()),
            );
            if let Some(run_id) = run_id {
                attributes.insert(
                    "kiana.work.run_id".to_string(),
                    kiana_telemetry::AttributeValue::String(run_id.0),
                );
            }
        }
        attributes.insert(
            "kiana.work.duration_ms".to_string(),
            kiana_telemetry::AttributeValue::Number(started.elapsed().as_millis() as f64),
        );
        attributes.insert(
            "kiana.work.status".to_string(),
            kiana_telemetry::AttributeValue::String(if error.is_some() {
                "error".to_string()
            } else {
                "ok".to_string()
            }),
        );
        let options = kiana_telemetry::SpanOptions {
            name: "kiana.work.commit".to_string(),
            attributes: Some(attributes),
            links: None,
        };
        let _ = telemetry
            .start_span_typed::<(), _>(options, move |span| {
                Box::pin(async move {
                    if let Some(error) = error {
                        span.set_status(kiana_telemetry::SpanStatus::Error {
                            error: Some(kiana_telemetry::StatusError {
                                name: "WorkStoreError".to_string(),
                                message: error,
                            }),
                        });
                    }
                })
            })
            .await;
    }

    async fn list(
        &self,
        parent_session_id: &str,
        only_running: bool,
    ) -> Result<Vec<WorkRecord>, WorkStoreError> {
        let Some(value) = self
            .store
            .snapshot(parent_session_id, &work_target())
            .await
            .map_err(WorkStoreError::new)?
        else {
            return Ok(Vec::new());
        };
        // 文档只可能由本模块创建/更新，`records` 缺失或不是对象意味着账本已损坏：
        // 与 `begin`/`finish` 一致 fail-closed，不能当作空账本静默通过。
        let records = records_of_ledger(&value)?;
        // 与 background journal 一致：账本是权威状态，单条记录损坏必须整体报错，
        // 不能静默丢弃（否则 `recover_work` 会漏掉损坏记录旁边的遗留 Running）。
        // Work 账本没有 TS 参照实现，这里选择 fail-closed 而不是宽松扫描。
        let mut records = records
            .values()
            .map(decode_record)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| WorkStoreError::new(format!("decoding work record: {error}")))?
            .into_iter()
            .filter(|record| record.parent_session_id == parent_session_id)
            .filter(|record| !only_running || !record.is_terminal())
            .collect::<Vec<_>>();
        records.sort_by(|left, right| {
            left.created_at_ms
                .cmp(&right.created_at_ms)
                .then_with(|| left.work_id.0.cmp(&right.work_id.0))
                .then_with(|| left.run_id.0.cmp(&right.run_id.0))
        });
        Ok(records)
    }
}

#[allow(clippy::double_must_use)]
#[async_trait]
impl WorkStore for DurableWorkStore {
    async fn allocate_work_id(&self, parent_session_id: &str) -> Result<WorkId, WorkStoreError> {
        let value = self.commit(parent_session_id, WorkOp::Allocate).await?;
        downcast::<Option<WorkId>>(value)?
            .ok_or_else(|| WorkStoreError::new("work id allocation returned no value"))
    }

    async fn begin(&self, record: WorkRecord) -> Result<WorkBegin, WorkStoreError> {
        let session_id = record.parent_session_id.clone();
        let value = self
            .commit(&session_id, WorkOp::Begin(Box::new(record)))
            .await?;
        downcast::<WorkBegin>(value)
    }

    async fn attach_session(
        &self,
        parent_session_id: &str,
        work_id: WorkId,
        run_id: &RunId,
        session_file: &str,
    ) -> Result<(), WorkStoreError> {
        let value = self
            .commit(
                parent_session_id,
                WorkOp::AttachSession {
                    work_id,
                    run_id: run_id.clone(),
                    session_file: session_file.to_owned(),
                    updated_at_ms: self.clock.now_ms(),
                },
            )
            .await?;
        match downcast::<Option<WorkRecord>>(value)? {
            Some(_) => Ok(()),
            None => Err(WorkStoreError::new(format!(
                "work {} run {} is not running or does not exist",
                work_id.0, run_id.0
            ))),
        }
    }

    async fn release_resume(
        &self,
        parent_session_id: &str,
        work_id: WorkId,
        run_id: &RunId,
    ) -> Result<bool, WorkStoreError> {
        DurableWorkStore::release_resume(self, parent_session_id, work_id, run_id).await
    }

    async fn finish(
        &self,
        parent_session_id: &str,
        result: &WorkResult,
    ) -> Result<WorkFinish, WorkStoreError> {
        let value = self
            .commit(
                parent_session_id,
                WorkOp::Finish {
                    work_id: result.work_id,
                    run_id: result.run_id.clone(),
                    result: Box::new(result.clone()),
                    updated_at_ms: self.clock.now_ms(),
                },
            )
            .await?;
        downcast::<WorkFinish>(value)
    }

    async fn load(
        &self,
        parent_session_id: &str,
        work_id: WorkId,
    ) -> Result<Option<WorkRecord>, WorkStoreError> {
        let Some(value) = self
            .store
            .snapshot(parent_session_id, &work_target())
            .await
            .map_err(WorkStoreError::new)?
        else {
            return Ok(None);
        };
        let records = records_of_ledger(&value)?;
        let Some(stored) = records.get(&work_id.0.to_string()) else {
            return Ok(None);
        };
        decode_record(stored)
            .map(Some)
            .map_err(|error| WorkStoreError::new(format!("decoding work record: {error}")))
    }

    async fn list_recoverable(
        &self,
        parent_session_id: &str,
    ) -> Result<Vec<WorkRecord>, WorkStoreError> {
        self.list(parent_session_id, true).await
    }

    async fn list_for_session(
        &self,
        parent_session_id: &str,
    ) -> Result<Vec<WorkRecord>, WorkStoreError> {
        self.list(parent_session_id, false).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subagent::work::{
        WorkAcceptancePolicy, WorkApproval, WorkEvidence, WorkMode, WorkRecovery, WorkTerminal,
    };

    fn request(work_id: i64, run_id: &str) -> WorkRequest {
        WorkRequest {
            protocol: crate::subagent::work::WORK_PROTOCOL_VERSION,
            work_id: WorkId(work_id),
            run_id: RunId::new(run_id),
            plan_id: None,
            profile: "general-purpose".to_string(),
            prompt: "do the work".to_string(),
            mode: WorkMode::Task,
            approval: WorkApproval::Headless,
            recovery: WorkRecovery::Fail,
            acceptance_policy: WorkAcceptancePolicy::ReportOnly,
            acceptance_criteria: Vec::new(),
            workdir: None,
            isolation: None,
            depth: 1,
            max_turns: None,
            token_budget: None,
            timeout_seconds: None,
            max_calls: None,
            read_only: false,
            resumable: false,
        }
    }

    fn failure(work_id: i64, run_id: &str) -> WorkResult {
        WorkResult::failure(
            &request(work_id, run_id),
            "boom",
            1_700_000_000_000,
            1_700_000_000_001,
        )
    }

    #[test]
    fn records_mut_rejects_a_non_object_records_field() {
        let mut value = JsonObject::new();
        value.insert("records".to_owned(), JsonValue::Bool(true));
        assert!(records_mut(&mut value).is_err());
    }

    #[test]
    fn records_mut_rejects_a_missing_records_field() {
        let mut value = JsonObject::new();
        assert!(
            records_mut(&mut value).is_err(),
            "写路径不能静默重建损坏账本的 records 表"
        );
    }

    struct MissingRecords;

    #[async_trait]
    impl SessionChange for MissingRecords {
        async fn run(&self, tx: &dyn Tx) -> Result<Box<dyn std::any::Any + Send>, BoxError> {
            let mut draft = tx.doc(&work_target()).await?;
            draft.value_mut().shift_remove("records");
            Ok(Box::new(()))
        }
    }

    #[tokio::test]
    async fn begin_fails_closed_when_the_records_map_is_missing() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = DurableWorkStore::new(temp.path());
        store
            .store
            .commit("session-missing", &MissingRecords)
            .await
            .expect("corrupt");

        let record = WorkRecord::new(request(1, "run-1"), "session-missing", None, None, 1);
        assert!(
            store.begin(record).await.is_err(),
            "begin must not rebuild a corrupt ledger or accept a duplicate id"
        );
        assert!(
            store.list_for_session("session-missing").await.is_err(),
            "reads and writes must fail closed on the same corruption"
        );
        assert!(
            store.allocate_work_id("session-missing").await.is_err(),
            "id allocation must not succeed against a corrupt ledger"
        );
    }

    struct NullCost;

    #[async_trait]
    impl SessionChange for NullCost {
        async fn run(&self, tx: &dyn Tx) -> Result<Box<dyn std::any::Any + Send>, BoxError> {
            let mut draft = tx.doc(&work_target()).await?;
            let records = records_mut(draft.value_mut())?;
            records.get_mut("1").expect("record")["state"]["result"]["usage"]["cost"] =
                JsonValue::Null;
            Ok(Box::new(()))
        }
    }

    #[tokio::test]
    async fn non_finite_cost_cannot_corrupt_the_ledger() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = DurableWorkStore::new(temp.path());
        store
            .begin(WorkRecord::new(
                request(1, "run-1"),
                "session-cost",
                None,
                None,
                1,
            ))
            .await
            .expect("begin");
        let mut result = failure(1, "run-1");
        result.usage.cost = f64::INFINITY;
        store.finish("session-cost", &result).await.expect("finish");

        let record = store
            .load("session-cost", WorkId(1))
            .await
            .expect("a non-finite cost must not make the ledger undecodable")
            .expect("record");
        assert_eq!(record.terminal_result().expect("terminal").usage.cost, 0.0);

        // 旧记录里的 JSON null（serde_json 对非有限 f64 的编码）也要能读回。
        store
            .store
            .commit("session-cost", &NullCost)
            .await
            .expect("null cost");
        let record = store
            .load("session-cost", WorkId(1))
            .await
            .expect("null cost must decode")
            .expect("record");
        assert_eq!(record.terminal_result().expect("terminal").usage.cost, 0.0);
    }

    #[test]
    fn invalid_resumed_from_fails_closed() {
        assert!(resumed_from_of(&serde_json::json!({"resumedFrom": "nope"})).is_err());
        assert!(resumed_from_of(&serde_json::json!({"resumedFrom": 1.5})).is_err());
        assert!(resumed_from_of(&serde_json::json!({"resumedFrom": {}})).is_err());
        assert_eq!(
            resumed_from_of(&serde_json::json!({"resumedFrom": "5"})).expect("valid"),
            Some(WorkId(5))
        );
        assert_eq!(
            resumed_from_of(&serde_json::json!({})).expect("absent"),
            None
        );
        assert_eq!(
            resumed_from_of(&serde_json::json!({"resumedFrom": null})).expect("null"),
            None
        );
    }

    struct CorruptRecord;

    #[async_trait]
    impl SessionChange for CorruptRecord {
        async fn run(&self, tx: &dyn Tx) -> Result<Box<dyn std::any::Any + Send>, BoxError> {
            let mut draft = tx.doc(&work_target()).await?;
            records_mut(draft.value_mut())?.insert("corrupt".to_owned(), JsonValue::Bool(true));
            Ok(Box::new(()))
        }
    }

    #[tokio::test]
    async fn list_fails_closed_on_a_corrupt_record() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = DurableWorkStore::new(temp.path());
        store
            .begin(WorkRecord::new(
                request(1, "run-1"),
                "session-bad",
                None,
                None,
                1,
            ))
            .await
            .expect("begin");
        store
            .store
            .commit("session-bad", &CorruptRecord)
            .await
            .expect("corrupt");
        assert!(
            store.list_for_session("session-bad").await.is_err(),
            "a corrupt authoritative record must not be silently dropped"
        );
    }

    #[tokio::test]
    async fn explicit_work_ids_advance_the_allocation_counter() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = DurableWorkStore::new(temp.path());
        store
            .begin(WorkRecord::new(
                request(100, "run-100"),
                "session-explicit",
                None,
                None,
                1,
            ))
            .await
            .expect("begin explicit");
        assert_eq!(
            store
                .allocate_work_id("session-explicit")
                .await
                .expect("allocate after explicit"),
            WorkId(101)
        );

        let error = store
            .begin(WorkRecord::new(
                request(i64::MAX, "run-max"),
                "session-max",
                None,
                None,
                1,
            ))
            .await
            .expect_err("i64::MAX has no room for a counter");
        assert!(error.to_string().contains("work id exhausted"), "{error}");
        // 其余 begin/allocate 不受影响：账本不能因为一次越界请求永久卡死。
        assert!(store.allocate_work_id("session-max").await.is_ok());
        store
            .begin(WorkRecord::new(
                request(i64::MAX - 1, "run-max-1"),
                "session-max",
                None,
                None,
                2,
            ))
            .await
            .expect("the id below i64::MAX is still valid");
        assert!(
            store.allocate_work_id("session-max").await.is_err(),
            "allocation must report exhaustion instead of repeating i64::MAX"
        );
    }

    #[tokio::test]
    async fn a_resume_anchor_can_only_have_one_successor() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = DurableWorkStore::new(temp.path());
        let first = WorkRecord::new(request(10, "run-10"), "session-resume", None, None, 1)
            .with_resumed_from(WorkId(1));
        assert!(matches!(
            store.begin(first).await.expect("first"),
            WorkBegin::Started(_)
        ));

        let second = WorkRecord::new(request(11, "run-11"), "session-resume", None, None, 2)
            .with_resumed_from(WorkId(1));
        match store.begin(second).await.expect("second") {
            WorkBegin::AlreadyResumed(existing) => assert_eq!(existing.work_id, WorkId(10)),
            other => panic!("expected AlreadyResumed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn releasing_a_resume_claim_allows_a_retry() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = DurableWorkStore::new(temp.path());
        store
            .begin(WorkRecord::new(
                request(1, "run-1"),
                "session-release",
                None,
                None,
                1,
            ))
            .await
            .expect("anchor");
        let successor = WorkRecord::new(request(2, "run-2"), "session-release", None, None, 2)
            .with_resumed_from(WorkId(1));
        assert!(matches!(
            store.begin(successor).await.expect("successor"),
            WorkBegin::Started(_)
        ));

        let retry = WorkRecord::new(request(3, "run-3"), "session-release", None, None, 3)
            .with_resumed_from(WorkId(1));
        assert!(matches!(
            store.begin(retry.clone()).await.expect("blocked"),
            WorkBegin::AlreadyResumed(_)
        ));
        assert!(
            store
                .release_resume("session-release", WorkId(2), &RunId::new("run-2"))
                .await
                .expect("release")
        );
        assert!(
            !store
                .release_resume("session-release", WorkId(2), &RunId::new("run-2"))
                .await
                .expect("second release"),
            "releasing twice is a no-op"
        );
        assert!(matches!(
            store.begin(retry).await.expect("retry"),
            WorkBegin::Started(_)
        ));
    }

    /// 回归（round-17 finding #1）：归还锚点必须在事务内复查“该记录仍有后继”。
    /// 否则 W1→W2→W3 的链里，W3 还在运行时归还 W2 会让 W1 被并发恢复。
    #[tokio::test]
    async fn a_resume_claim_cannot_be_released_while_a_successor_claims_the_record() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = DurableWorkStore::new(temp.path());
        for (id, run, resumed_from) in [
            (1_i64, "run-1", None),
            (2_i64, "run-2", Some(WorkId(1))),
            (3_i64, "run-3", Some(WorkId(2))),
        ] {
            let mut record =
                WorkRecord::new(request(id, run), "session-chain", None, None, id as u64);
            if let Some(origin) = resumed_from {
                record = record.with_resumed_from(origin);
            }
            store.begin(record).await.expect("begin");
        }

        assert!(
            !store
                .release_resume("session-chain", WorkId(2), &RunId::new("run-2"))
                .await
                .expect("release blocked"),
            "W3 still claims W2; releasing W1 would allow a concurrent resume"
        );
        assert!(
            store
                .release_resume("session-chain", WorkId(3), &RunId::new("run-3"))
                .await
                .expect("release 3"),
            "W3 has no successor"
        );
        assert!(
            store
                .release_resume("session-chain", WorkId(2), &RunId::new("run-2"))
                .await
                .expect("release 2"),
            "after W3 is released, W2 can release W1"
        );
    }

    #[tokio::test]
    async fn list_all_sessions_sees_every_ledger() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = DurableWorkStore::new(temp.path());
        for (session, work_id) in [("session-a", 21), ("session-b", 22)] {
            store
                .begin(WorkRecord::new(
                    request(work_id, &format!("run-{work_id}")),
                    session,
                    None,
                    None,
                    1,
                ))
                .await
                .expect("begin");
        }
        let (records, skipped) = store.list_all_sessions().await.expect("scan");
        assert_eq!(skipped, 0);
        let mut work_ids = records
            .into_iter()
            .map(|record| record.work_id.0)
            .collect::<Vec<_>>();
        work_ids.sort_unstable();
        assert_eq!(work_ids, vec![21, 22]);
    }

    #[tokio::test]
    async fn finish_accepts_a_result_without_a_run_id() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = DurableWorkStore::new(temp.path());
        store
            .begin(WorkRecord::new(
                request(12, "run-12"),
                "session-skip",
                None,
                None,
                1,
            ))
            .await
            .expect("begin");
        let mut result = failure(12, "run-12");
        result.run_id = None;

        match store.finish("session-skip", &result).await.expect("finish") {
            WorkFinish::Finished(record) => assert!(record.is_terminal()),
            other => panic!("expected Finished, got {other:?}"),
        }
    }

    #[test]
    fn work_state_fields_use_camel_case() {
        let running = serde_json::to_value(WorkState::Running {
            session_file: Some("child.jsonl".to_string()),
        })
        .expect("running value");
        assert_eq!(
            running,
            serde_json::json!({"status": "running", "sessionFile": "child.jsonl"})
        );

        let terminal = serde_json::to_value(WorkState::Terminal {
            result: Box::new(failure(7, "run-7")),
            terminal_seq: 1,
        })
        .expect("terminal value");
        assert_eq!(terminal["status"], "terminal");
        assert_eq!(terminal["terminalSeq"], 1);
        assert!(terminal.get("terminal_seq").is_none());
    }

    #[tokio::test]
    async fn work_id_counter_is_stored_as_a_string() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = DurableWorkStore::new(temp.path());
        store
            .allocate_work_id("session-counter")
            .await
            .expect("allocate");
        let value = store
            .store
            .snapshot("session-counter", &work_target())
            .await
            .expect("snapshot")
            .expect("document");
        assert!(
            value["nextWorkId"].is_string(),
            "the durable counter must not lose precision above 2^53"
        );
    }

    fn ledger(counter: Option<JsonValue>, record_keys: &[&str]) -> JsonObject {
        let mut value = JsonObject::new();
        if let Some(counter) = counter {
            value.insert("nextWorkId".to_owned(), counter);
        }
        let mut records = Map::new();
        for key in record_keys {
            records.insert((*key).to_string(), JsonValue::Object(Map::new()));
        }
        value.insert("records".to_owned(), JsonValue::Object(records));
        value
    }

    #[test]
    fn next_work_id_accepts_numeric_and_string_forms() {
        let mut value = ledger(Some(JsonValue::from(7_i64)), &[]);
        assert_eq!(next_work_id_of(&value).expect("numeric"), 7);

        let large = (1_i64 << 53) + 3;
        value.insert(
            "nextWorkId".to_owned(),
            JsonValue::String(large.to_string()),
        );
        assert_eq!(next_work_id_of(&value).expect("string"), large);
    }

    /// 回归（round-17 finding #4）：旧版文档缺 `nextWorkId` 时从已有记录之后发号；
    /// 字段存在但非法或落后于记录时必须 fail closed，不能回落 1 复用旧 id。
    #[test]
    fn next_work_id_fails_closed_instead_of_reusing_record_ids() {
        let legacy = ledger(None, &["2", "9"]);
        assert_eq!(
            next_work_id_of(&legacy).expect("legacy fallback"),
            10,
            "a legacy document without a counter must continue after its records"
        );

        for counter in [
            JsonValue::Bool(true),
            JsonValue::String("not-a-number".to_string()),
            JsonValue::from(0_i64),
            JsonValue::from(-3_i64),
        ] {
            let value = ledger(Some(counter.clone()), &[]);
            assert!(next_work_id_of(&value).is_err(), "must reject {counter:?}");
        }

        let behind = ledger(Some(JsonValue::from(3_i64)), &["2", "9"]);
        let error = next_work_id_of(&behind).expect_err("counter behind records");
        assert!(
            error.to_string().contains("behind existing records"),
            "{error}"
        );

        let non_numeric = ledger(Some(JsonValue::from(4_i64)), &["not-an-id"]);
        assert!(next_work_id_of(&non_numeric).is_err());
    }

    #[tokio::test]
    async fn large_work_ids_survive_the_durable_round_trip() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = DurableWorkStore::new(temp.path());
        let work_id = WorkId((1_i64 << 53) + 1);
        let resumed_from = WorkId((1_i64 << 53) + 2);
        let record = WorkRecord::new(
            request(work_id.0, "run-large"),
            "session-large",
            None,
            None,
            1_700_000_000_000,
        )
        .with_resumed_from(resumed_from);
        store.begin(record).await.expect("begin");

        let loaded = store
            .load("session-large", work_id)
            .await
            .expect("load")
            .expect("record");
        assert_eq!(loaded.work_id, work_id);
        assert_eq!(loaded.request.work_id, work_id);
        assert_eq!(loaded.resumed_from, Some(resumed_from));

        store
            .finish("session-large", &failure(work_id.0, "run-large"))
            .await
            .expect("finish");
        let loaded = store
            .load("session-large", work_id)
            .await
            .expect("load terminal")
            .expect("record");
        let result = loaded.terminal_result().expect("terminal result");
        assert_eq!(result.work_id, work_id);
    }

    #[test]
    fn live_guard_marks_work_as_live() {
        let session_id = "live-guard-session";
        let work_id = WorkId(9_999_999);
        let run_id = RunId::new("live-guard-run");
        assert!(!work_is_live(session_id, work_id, &run_id));

        let guard = WorkLiveGuard::register(session_id, work_id, &run_id);
        assert!(work_is_live(session_id, work_id, &run_id));
        drop(guard);
        assert!(!work_is_live(session_id, work_id, &run_id));
    }

    #[tokio::test]
    async fn list_for_session_returns_all_records_sorted_and_scoped() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = DurableWorkStore::new(temp.path());
        // 故意乱序写入：读取必须按 createdAtMs/workId 升序，而不是插入顺序。
        store
            .begin(WorkRecord::new(
                request(12, "run-12"),
                "session-a",
                None,
                None,
                2_000,
            ))
            .await
            .expect("begin 12");
        store
            .begin(WorkRecord::new(
                request(11, "run-11"),
                "session-a",
                None,
                None,
                1_000,
            ))
            .await
            .expect("begin 11");
        store
            .begin(WorkRecord::new(
                request(13, "run-13"),
                "session-b",
                None,
                None,
                1_500,
            ))
            .await
            .expect("begin 13");
        store
            .finish("session-a", &failure(12, "run-12"))
            .await
            .expect("finish 12");

        let all = store
            .list_for_session("session-a")
            .await
            .expect("list session-a");
        assert_eq!(
            all.iter()
                .map(|record| record.work_id.0)
                .collect::<Vec<_>>(),
            vec![11, 12]
        );
        assert!(
            all.iter()
                .all(|record| record.parent_session_id == "session-a")
        );

        let recoverable = store
            .list_recoverable("session-a")
            .await
            .expect("list recoverable session-a");
        assert_eq!(
            recoverable
                .iter()
                .map(|record| record.work_id.0)
                .collect::<Vec<_>>(),
            vec![11]
        );
        assert!(!recoverable.iter().any(WorkRecord::is_terminal));

        let other = store
            .list_for_session("session-b")
            .await
            .expect("list session-b");
        assert_eq!(other.len(), 1);
        assert_eq!(other[0].work_id, WorkId(13));
    }

    /// Phase 3：账本提交产生编排层 span；遥测失败不影响提交结果。
    #[tokio::test]
    async fn work_commits_emit_orchestration_spans() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = DurableWorkStore::new(temp.path());
        let telemetry = Arc::new(kiana_telemetry::InMemoryTelemetryContext::new());
        store.set_telemetry(telemetry.clone());
        store
            .begin(WorkRecord::new(
                request(21, "run-21"),
                "session-1",
                None,
                None,
                1,
            ))
            .await
            .expect("begin");

        let spans = telemetry.get_spans();
        let span = spans
            .iter()
            .find(|span| span.name == "kiana.work.commit")
            .expect("work commit span");
        assert!(span.settled);
        assert_eq!(
            span.attributes.get("kiana.work.operation"),
            Some(&kiana_telemetry::AttributeValue::String(
                "begin".to_string()
            ))
        );
        assert_eq!(
            span.attributes.get("kiana.work.id"),
            Some(&kiana_telemetry::AttributeValue::String("21".to_string()))
        );
        assert_eq!(
            span.attributes.get("kiana.work.run_id"),
            Some(&kiana_telemetry::AttributeValue::String(
                "run-21".to_string()
            ))
        );
        assert!(matches!(span.status, kiana_telemetry::SpanStatus::Ok));
    }

    #[tokio::test]
    async fn work_ids_are_allocated_durably_and_survive_reopen() {
        let temp = tempfile::tempdir().expect("tempdir");
        let first = DurableWorkStore::new(temp.path());
        let id1 = first.allocate_work_id("session-a").await.expect("id1");
        let id2 = first.allocate_work_id("session-a").await.expect("id2");
        assert!(id1.0 > 0);
        assert!(id2.0 > id1.0);

        let reopened = DurableWorkStore::new(temp.path());
        let id3 = reopened.allocate_work_id("session-a").await.expect("id3");
        assert!(id3.0 > id2.0, "counter must survive a new store instance");
    }

    #[tokio::test]
    async fn begin_is_idempotent_and_finish_is_a_cas() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = DurableWorkStore::new(temp.path());
        let record = WorkRecord::new(
            request(7, "run-7"),
            "session-a",
            None,
            Some(std::process::id()),
            1_700_000_000_000,
        );
        assert!(matches!(
            store.begin(record.clone()).await.expect("begin"),
            WorkBegin::Started(_)
        ));
        let existing = store.begin(record).await.expect("begin again");
        assert!(matches!(existing, WorkBegin::Existing(_)));

        let result = failure(7, "run-7");
        let won = store.finish("session-a", &result).await.expect("finish");
        let WorkFinish::Finished(finished) = won else {
            panic!("first finish must win: {won:?}");
        };
        assert!(finished.is_terminal());
        assert_eq!(finished.terminal_seq(), 1);
        assert!(matches!(
            store
                .finish("session-a", &result)
                .await
                .expect("finish again"),
            WorkFinish::NotApplied(_)
        ));

        let reopened = DurableWorkStore::new(temp.path());
        let loaded = reopened
            .load("session-a", WorkId(7))
            .await
            .expect("load")
            .expect("record");
        assert!(loaded.is_terminal());
        assert!(matches!(
            loaded.terminal_result().map(|result| &result.terminal),
            Some(WorkTerminal::Failed { .. })
        ));
    }

    #[tokio::test]
    async fn attach_session_is_durable_and_recoverable() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = DurableWorkStore::new(temp.path());
        let record = WorkRecord::new(request(8, "run-8"), "session-a", None, None, 100);
        store.begin(record).await.expect("begin");
        store
            .attach_session(
                "session-a",
                WorkId(8),
                &RunId::new("run-8"),
                "/tmp/child.jsonl",
            )
            .await
            .expect("attach");

        let reopened = DurableWorkStore::new(temp.path());
        let loaded = reopened
            .load("session-a", WorkId(8))
            .await
            .expect("load")
            .expect("record");
        assert_eq!(loaded.session_file(), Some("/tmp/child.jsonl"));
        assert_eq!(
            reopened
                .list_recoverable("session-a")
                .await
                .expect("list")
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn finish_without_a_running_record_reports_missing_or_not_applied() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = DurableWorkStore::new(temp.path());
        let result = failure(9, "run-9");
        assert!(matches!(
            store.finish("session-a", &result).await.expect("finish"),
            WorkFinish::Missing
        ));

        let record = WorkRecord::new(request(9, "run-9"), "session-a", None, None, 100);
        store.begin(record).await.expect("begin");
        let other = failure(9, "run-other");
        assert!(matches!(
            store.finish("session-a", &other).await.expect("finish"),
            WorkFinish::NotApplied(_)
        ));
    }

    #[test]
    fn duplicate_live_guards_share_one_registration() {
        let session_id = "live-guard-refcount";
        let work_id = WorkId(9_999_998);
        let run_id = RunId::new("live-guard-refcount-run");
        let first = WorkLiveGuard::register(session_id, work_id, &run_id);
        let second = WorkLiveGuard::register(session_id, work_id, &run_id);
        drop(second);
        assert!(
            work_is_live(session_id, work_id, &run_id),
            "the loser's guard must not unregister the live run"
        );
        drop(first);
        assert!(!work_is_live(session_id, work_id, &run_id));
    }

    #[tokio::test]
    async fn large_u64_fields_survive_the_durable_round_trip() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = DurableWorkStore::new(temp.path());
        let mut request = request(51, "run-51");
        request.token_budget = Some(u64::MAX);
        request.timeout_seconds = Some(u64::MAX - 1);
        request.max_calls = Some(u64::MAX - 2);
        store
            .begin(WorkRecord::new(
                request,
                "session-u64",
                None,
                None,
                u64::MAX - 3,
            ))
            .await
            .expect("begin");

        let mut result = failure(51, "run-51");
        result.usage.input_tokens = u64::MAX;
        result.usage.output_tokens = u64::MAX - 1;
        result.usage.cache_read_tokens = u64::MAX - 2;
        result.usage.cache_write_tokens = u64::MAX - 3;
        result.usage.total_tokens = u64::MAX - 4;
        result.usage.tool_calls = u64::MAX - 5;
        result.usage.turns = Some(u64::MAX - 6);
        result.started_at_ms = u64::MAX - 7;
        result.finished_at_ms = u64::MAX - 8;
        result.evidence.push(WorkEvidence {
            seq: u64::MAX - 9,
            tool: "bash".to_string(),
            args_sha256: "args".to_string(),
            output_sha256: "output".to_string(),
            status: "completed".to_string(),
        });
        store.finish("session-u64", &result).await.expect("finish");

        let loaded = store
            .load("session-u64", WorkId(51))
            .await
            .expect("load")
            .expect("record");
        assert_eq!(loaded.created_at_ms, u64::MAX - 3);
        assert_eq!(loaded.request.token_budget, Some(u64::MAX));
        assert_eq!(loaded.request.timeout_seconds, Some(u64::MAX - 1));
        assert_eq!(loaded.request.max_calls, Some(u64::MAX - 2));
        let stored = loaded.terminal_result().expect("terminal result");
        assert_eq!(stored.started_at_ms, u64::MAX - 7);
        assert_eq!(stored.finished_at_ms, u64::MAX - 8);
        assert_eq!(stored.evidence[0].seq, u64::MAX - 9);
        assert_eq!(stored.usage.input_tokens, u64::MAX);
        assert_eq!(stored.usage.output_tokens, u64::MAX - 1);
        assert_eq!(stored.usage.cache_read_tokens, u64::MAX - 2);
        assert_eq!(stored.usage.cache_write_tokens, u64::MAX - 3);
        assert_eq!(stored.usage.total_tokens, u64::MAX - 4);
        assert_eq!(stored.usage.tool_calls, u64::MAX - 5);
        assert_eq!(stored.usage.turns, Some(u64::MAX - 6));
    }

    struct DropRecords;

    #[async_trait]
    impl SessionChange for DropRecords {
        async fn run(&self, tx: &dyn Tx) -> Result<Box<dyn std::any::Any + Send>, BoxError> {
            let mut draft = tx.doc(&work_target()).await?;
            draft.value_mut().shift_remove("records");
            Ok(Box::new(()))
        }
    }

    #[tokio::test]
    async fn list_and_load_fail_closed_on_a_missing_records_field() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = DurableWorkStore::new(temp.path());
        store
            .begin(WorkRecord::new(
                request(61, "run-61"),
                "session-missing",
                None,
                None,
                1,
            ))
            .await
            .expect("begin");
        store
            .store
            .commit("session-missing", &DropRecords)
            .await
            .expect("drop records");
        assert!(store.list_for_session("session-missing").await.is_err());
        assert!(store.load("session-missing", WorkId(61)).await.is_err());
    }
}
