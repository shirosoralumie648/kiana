//! extensions/codemode/execute.ts：在沙箱里运行一个 codemode 脚本。
//!
//! 从 tool.ts 拆出，经 execute.lazy.ts 加载，因此只有真正运行脚本时才加载沙箱运行时
//! （TS 侧是动态 import；Rust 侧对应 `kiana-codemode` 的 wasm 后端在首次执行时才创建 worker）。
//!
//! # 硬性语义
//!
//! - worker 准入是**进程级 FIFO 队列 + 计数**（不是 `Semaphore`：到达顺序被测试断言）；
//!   `KIANA_CODEMODE_MAX_WORKERS` 在**每次**准入时重读；槽位只在 worker **真正退出**时释放。
//! - 脚本等待 worker 的时间计入 `timeout_ms`，等待上限为 `min(timeout_ms, 60_000)`。
//! - 输出整形顺序固定：`format_output` → 错误段 → 未展示图片提示 → `join_adjacent_text`
//!   → `truncate_output` → `save_images` → `join_adjacent_text` → 头部。
//!
//!   后两步不可交换：截断发生在插入图片路径标签之前，保证“已保存到 `<path>`”的路径不会被
//!   token 预算截掉（`spec/ca-builtin-extensions.md` §2.12.11 的“顺序要点”）。
//!
//! # 这个文件负责什么
//!
//! 它是 `codemode` 工具的**执行后半段**：`codemode/tool.rs` 负责工具声明与 loadout，模型返回
//! 脚本后才由 [`execute_codemode`] 完成校验、借 worker、运行与结果整形。边界是：本文件不实现
//! 沙箱协议，也不解释 JavaScript；VM 生命周期、deadline、取消与 store 序列化都在
//! `kiana-codemode`（`CodemodeSandbox`）里，本文件只提供工具/全局函数回调与一次执行的选项。
//!
//! # 被谁调用
//!
//! - 生产路径：`codemode/tool.rs` 的 `CodemodeToolExecutable::execute` 转发到
//!   [`execute_codemode`]；它先把占位上下文还原成 `None`，对应 TS 的 `ctx === undefined`。
//! - 测试：`tests/codemode_worker_admission.rs` 直接调用 [`execute_codemode`]，并用
//!   `active_codemode_workers` 断言进程级容量；[`read_codemode_store`] 由
//!   `tests/codemode_store.rs` 单独覆盖。
//!
//! # 一次执行的主要阶段
//!
//! 1. 校验 `input.code` 的类型（按 TS 的 TypeError 文案失败），再用 `parse_codemode_source`
//!    拆出脚本正文与 `// @options:`（`timeout_ms`、`max_output_tokens`）。
//! 2. 发布一次空 `details`：调用方的 `on_update` 先收到这次调用，随后才产生嵌套调用记录。
//!    接着为当前可调用工具生成声明样本（`samples`）与 `CodemodeTool` 包装。
//! 3. 申请进程级 worker 槽位。拿不到时**不返回 `Err`**，而是构造 `is_error` 结果；文案区分
//!    abort、脚本 deadline 先到、纯容量不足三种情况。
//! 4. 组装 globals（`searchTools`/`describeTool`/`describeNamespace`；`models` 开启时再加
//!    `models.*`），有 `ctx` 时从会话分支重放 `codemode-store` 作为 `load()` 初值，构造并
//!    执行 sandbox。
//! 5. 结算：成功分支把 `store()` 写入交给 `options.append_entry`；把输出按上面固定的顺序整形；
//!    最后附上 `details.calls`、`details.fullOutputPath` 与 `models.*` 的 usage。
//!
//! # 并发与失败模式
//!
//! - 嵌套调用记录与 `models.*` usage 通过 `Arc<Mutex<...>>` 在沙箱回调之间共享，生成图片
//!   计数用 `Arc<AtomicUsize>`。本文件的调用点都在释放锁之后才调用 `publish()`（`publish`
//!   内部的
//!   `snapshot_details` 会自己取一次锁）：`publish` 会同步执行调用方的 `on_update`，不应在
//!   持锁期间调用外部回调。
//! - 等待队列的取消安全由 `WaitGuard` 负责：future 被丢弃时撤销排队，或归还已记账但无人接手
//!   的槽位；`WorkerReservation` 负责“已预留但沙箱从未启动”的归还。
//! - 脚本执行错误（`CodemodeResult::Err`，含超时、abort、沙箱异常）不是 `Err`：工具返回
//!   `is_error: Some(true)` 的结果，保留部分输出与调用历史。返回 `Err` 的覆盖执行前的非法
//!   input、源码解析、sandbox 构造失败，以及执行后 `save_images` 的未知图片 MIME 扩展名错误
//!   （见下一条）。
//! - 输出截断与图片落盘是两条独立的写文件路径，都走 `kiana_core::output_files`。写失败不会
//!   让工具结果变成 `Err`（截断提示与图片标签里改写成错误文本）；例外是图片 MIME 没有已知
//!   扩展名，`save_images` 会返回 `AgentToolError`。

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use indexmap::IndexMap;
use kiana_agent_core::{
    AgentTool, AgentToolCallOutcome, AgentToolError, AgentToolResult, AgentToolUpdateCallback,
};
use kiana_ai::models::{
    ClassifierOptions, ImagesOptions, ModelsClassifierOptions, ModelsImagesOptions,
};
use kiana_ai::types::{
    BoolQuestionCriteria, ClassifierContext, ClassifierQuestion, ClassifierStopReason,
    ImageContent, ImageKind, ImagesContext, ImagesStopReason, JsonValue, Model, ModelType,
    TextContent, TextKind, Usage, UserContentBlock,
};
use kiana_codemode::types::CodemodeResultErr;
use kiana_codemode::{
    CodemodeExecuteOptions, CodemodeOutputItem, CodemodeResult, CodemodeSandbox,
    CodemodeSandboxBaseOptions, CodemodeSandboxOptions, CodemodeTool, CodemodeToolContext,
    parse_codemode_source, render_tool_sample, to_codemode_identifier,
};
use kiana_core::config::get_codemode_worker_specifier;
use kiana_core::output_files::write_output_file;
use kiana_ext_api::types::{ExecuteToolOptions, ExtensionToolContextApi};
use kiana_session_core::session_manager::SessionEntry;
use kiana_session_core::usage_totals::combine_usage;
use kiana_tools::truncate::format_size;
use serde_json::Map;
use tokio_util::sync::CancellationToken;

use crate::codemode::tool::{
    CODEMODE_DOCS_PATH, CODEMODE_STORE_ENTRY_TYPE, CodemodeNestedCall, CodemodeNestedCallStatus,
    CodemodeStoreEntryData, CodemodeToolDetails, CodemodeToolOptions, get_codemode_callable_tools,
    to_codemode_declaration,
};
use crate::tool_search::tool::{
    Bm25Ranker, DEFAULT_TOOL_SEARCH_LIMIT, ToolRanker, create_tool_search_document,
};

/// 嵌套调用参数预览的最大字符数。
const ARGS_PREVIEW_CHARS: usize = 200;
/// 嵌套调用错误预览的最大字符数。
const ERROR_PREVIEW_CHARS: usize = 500;
/// 一个脚本可以同时在途的 `models.classify()` / `models.generateImages()` 调用数。
const MAX_CONCURRENT_MODEL_CALLS: usize = 4;
/// QuickJS VM 的堆上限（256 MiB）。
const CODEMODE_MEMORY_LIMIT_BYTES: f64 = 268_435_456.0;
/// `KIANA_CODEMODE_MAX_WORKERS` 缺省值（跨调用、跨会话共享；允许一个替换正在退出的 worker）。
const DEFAULT_MAX_CODEMODE_WORKERS: usize = 2;
/// 脚本等待空闲 worker 的上限（毫秒）。
const CODEMODE_WORKER_WAIT_MS: f64 = 60_000.0;
/// 脚本输出的默认 token 预算。
const DEFAULT_MAX_OUTPUT_TOKENS: f64 = 10_000.0;
/// 估算 token 时的每 token 字符数。
const CHARS_PER_TOKEN: usize = 4;
/// `image()` 接受的图片类型到扩展名。
const IMAGE_EXTENSIONS: [(&str, &str); 4] = [
    ("image/png", ".png"),
    ("image/jpeg", ".jpg"),
    ("image/gif", ".gif"),
    ("image/webp", ".webp"),
];

/// 进程级活跃 worker 数。
static ACTIVE_CODEMODE_WORKERS: AtomicUsize = AtomicUsize::new(0);
/// 等 worker 的脚本，按到达顺序。
static CODEMODE_WAITERS: Mutex<VecDeque<Arc<WorkerWaiter>>> = Mutex::new(VecDeque::new());

/// 返回当前活跃 worker 数（测试用；不属于 TS 面）。
#[doc(hidden)]
pub fn active_codemode_workers() -> usize {
    ACTIVE_CODEMODE_WORKERS.load(Ordering::SeqCst)
}

/// 一次准入等待的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkerAdmission {
    /// 预留成功。
    Reserved,
    /// 调用方 signal 中止。
    Aborted,
    /// 超时。
    Timeout,
}

/// 等待者尚未结算。
const WAITER_UNSETTLED: u8 = 0;
/// 等待者已拿到槽位（`Reserved`）。
const WAITER_RESERVED: u8 = 1;
/// 等待者因 abort 结算。
const WAITER_ABORTED: u8 = 2;
/// 等待者因超时结算。
const WAITER_TIMEOUT: u8 = 3;

/// 结算状态对应的 code。
fn waiter_code(outcome: WorkerAdmission) -> u8 {
    match outcome {
        WorkerAdmission::Reserved => WAITER_RESERVED,
        WorkerAdmission::Aborted => WAITER_ABORTED,
        WorkerAdmission::Timeout => WAITER_TIMEOUT,
    }
}

/// 队列里的一个等待者。
///
/// `state` 只会从 [`WAITER_UNSETTLED`] 迁移一次：`admit_codemode_workers` 记 `Reserved`，
/// 调用方自己记 `Aborted`/`Timeout`。谁赢得 CAS 谁负责后续动作，因此 grant 与取消的竞态
/// 不需要额外的锁。
struct WorkerWaiter {
    state: AtomicU8,
    sender: Mutex<Option<tokio::sync::oneshot::Sender<WorkerAdmission>>>,
}

impl WorkerWaiter {
    fn new(sender: tokio::sync::oneshot::Sender<WorkerAdmission>) -> Self {
        WorkerWaiter {
            state: AtomicU8::new(WAITER_UNSETTLED),
            sender: Mutex::new(Some(sender)),
        }
    }

    /// 尝试结算；返回它是否由本次调用结算。
    fn try_settle(&self, outcome: WorkerAdmission) -> bool {
        if self
            .state
            .compare_exchange(
                WAITER_UNSETTLED,
                waiter_code(outcome),
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .is_err()
        {
            return false;
        }
        let sender = self
            .sender
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(sender) = sender {
            let _ = sender.send(outcome);
        }
        true
    }

    fn state_code(&self) -> u8 {
        self.state.load(Ordering::SeqCst)
    }
}

/// 等待队列的卫兵：future 被宿主丢弃（而不是自己返回）时撤销排队，或在 grant 已记账但
/// 接收方已消失时归还槽位。TS 的 Promise 不可能这样被丢弃，Rust 的 future 可以，所以这层
/// 清理是取消安全所必需的。
struct WaitGuard {
    waiter: Arc<WorkerWaiter>,
    /// 等待结果已确定为 `Reserved`：槽位所有权已交给调用方。
    owns_slot: bool,
}

impl WaitGuard {
    fn new(waiter: Arc<WorkerWaiter>) -> Self {
        WaitGuard {
            waiter,
            owns_slot: false,
        }
    }

    fn take_slot(&mut self) {
        self.owns_slot = true;
    }

    /// future 在等待中被丢弃：替它结算，之后 admit 不会再把槽位发到这里。若 CAS 输给了
    /// 并发的 grant（admit 已先记账），槽位同样没人接手，必须归还。
    fn cancel_unsettled(&self) {
        if !self.waiter.try_settle(WorkerAdmission::Aborted)
            && self.waiter.state_code() == WAITER_RESERVED
        {
            release_codemode_worker();
        }
    }
}

impl Drop for WaitGuard {
    fn drop(&mut self) {
        CODEMODE_WAITERS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .retain(|queued| !Arc::ptr_eq(queued, &self.waiter));
        if self.owns_slot {
            return;
        }
        match self.waiter.state_code() {
            // grant 已把槽位记在账上，但没人来接手（future 在收到消息前被丢弃）。
            WAITER_RESERVED => release_codemode_worker(),
            // 还没结算：替被丢弃的 future 结算。
            WAITER_UNSETTLED => self.cancel_unsettled(),
            _ => {}
        }
    }
}

/// `KIANA_CODEMODE_MAX_WORKERS`：正整数（JS `Number()` 语义）否则 2。每次调用都重读。
fn max_codemode_workers() -> usize {
    let raw = std::env::var("KIANA_CODEMODE_MAX_WORKERS").unwrap_or_default();
    let value = kiana_json::js_num::parse_js_number(&raw);
    if kiana_json::is_js_integer(value) && value > 0.0 {
        value as usize
    } else {
        DEFAULT_MAX_CODEMODE_WORKERS
    }
}

/// 按到达顺序把空闲槽位发给等待者。
///
/// 循环直到队列空或容量用尽：在持有队列锁时重读 `KIANA_CODEMODE_MAX_WORKERS`（与 TS 在循环里
/// 重新求值 `maxCodemodeWorkers()` 一致），弹出一个等待者并先给 `ACTIVE_CODEMODE_WORKERS`
/// 加一，再放锁结算。结算失败（等待者在弹出与 grant 之间中止/超时/被丢弃）就把刚加的计数
/// 减回去，继续尝试下一个。
///
/// 唤醒动作（`try_settle` 会 send oneshot）在 `drop(waiters)` 之后执行：等待者的 `Drop`
/// 也要取队列锁，锁内唤醒会让被唤醒方先等本函数放锁再取锁（`WaitGuard::drop` 取队列锁）。
fn admit_codemode_workers() {
    loop {
        let mut waiters = CODEMODE_WAITERS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if waiters.is_empty() {
            break;
        }
        // TS 在循环里重新求值 `maxCodemodeWorkers()`；这里保持同一行为。
        if ACTIVE_CODEMODE_WORKERS.load(Ordering::SeqCst) >= max_codemode_workers() {
            break;
        }
        let Some(waiter) = waiters.pop_front() else {
            break;
        };
        ACTIVE_CODEMODE_WORKERS.fetch_add(1, Ordering::SeqCst);
        drop(waiters);
        if !waiter.try_settle(WorkerAdmission::Reserved) {
            // 等待者在 pop 与 grant 之间中止/超时/被丢弃：归还刚占的槽位。
            ACTIVE_CODEMODE_WORKERS.fetch_sub(1, Ordering::SeqCst);
            continue;
        }
    }
}

/// 释放一个 worker 槽位并唤醒下一个等待者。
///
/// 主要作为 `acquireWorker()` 返回的 release 回调，由 `kiana-codemode` 在 worker 构造失败或
/// 真正退出时调用一次；future 被丢弃且 grant 已记账而无人接手时，也由 `WaitGuard` 归还槽位
/// （正常 abort/timeout 结算不释放槽位；“已预留但沙箱未启动”则由 `WorkerReservation`
/// 归还）。
///
/// 先递减计数（已为 0 时保持 0，`saturating_sub`），再把空出的容量按 FIFO 发给队首等待者。
/// 计数递减与发放不在同一个原子操作里，但发放方只在计数小于上限时加计数，因此并发调用
/// `release` 不会把并发数推过上限。
pub fn release_codemode_worker() {
    let _ = ACTIVE_CODEMODE_WORKERS.try_update(Ordering::SeqCst, Ordering::SeqCst, |active| {
        Some(active.saturating_sub(1))
    });
    admit_codemode_workers();
}

/// 预留槽位的卫兵：`execute_codemode` 的 future 在 `acquire_worker` 之前被丢弃时归还槽位。
///
/// 一旦 `acquire_worker` 把 `reserved` 置为 false，槽位就归 sandbox 的 worker 生命周期
/// （真实退出时释放）；正常路径在沙箱关闭后立即 `drop` 本卫兵，与 TS 的 `finally` 同步。
struct WorkerReservation {
    reserved: Arc<AtomicBool>,
}

impl Drop for WorkerReservation {
    fn drop(&mut self) {
        if self.reserved.swap(false, Ordering::SeqCst) {
            release_codemode_worker();
        }
    }
}

/// 构造传给 kiana-codemode 的 `acquireWorker()`。
///
/// 第一次调用把槽位的所有权从本次执行转交给 worker 生命周期（release 在 worker 构造失败或
/// 真正退出时调用一次）；结果结算后仍在推进的启动会再次调用它，此时 panic
/// `"The script already settled"`（host 用 `catch_unwind` 接住，以实现同步抛出）。
fn create_acquire_worker(
    reserved: Arc<AtomicBool>,
) -> kiana_codemode::types::CodemodeAcquireWorker {
    Arc::new(move || -> kiana_codemode::types::CodemodeWorkerRelease {
        if !reserved.swap(false, Ordering::SeqCst) {
            panic!("The script already settled");
        }
        Box::new(release_codemode_worker)
    })
}

/// 预留一个 worker 槽位，占满时按到达顺序等待。
///
/// 步骤与返回值的三种状态见 [`WorkerAdmission`]：
/// - 入口先看 signal，已取消直接 `Aborted`，不进入队列；
/// - 入队后立即 `admit_codemode_workers`，再用 `try_recv` 消费同步 grant（否则 0 毫秒计时器
///   可能先就绪而误报超时）；
/// - 否则在 `receiver`、`signal.cancelled()`、`sleep` 之间 `biased` 选择：`biased` 让已经就绪
///   的 grant 优先于同一轮就绪的取消/超时，避免“已经拿到槽位却报超时”。
///
/// 取消安全：`WaitGuard` 在 future 被丢弃时替它结算或归还槽位。`timeout_ms` 由调用方截到
/// `min(脚本 deadline, 60_000)`，因此这里总是有限值。
async fn reserve_codemode_worker(
    signal: Option<&CancellationToken>,
    timeout_ms: f64,
) -> WorkerAdmission {
    if signal.is_some_and(CancellationToken::is_cancelled) {
        return WorkerAdmission::Aborted;
    }
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let waiter = Arc::new(WorkerWaiter::new(sender));
    CODEMODE_WAITERS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .push_back(Arc::clone(&waiter));
    let mut guard = WaitGuard::new(Arc::clone(&waiter));
    admit_codemode_workers();
    let mut receiver = receiver;
    // TS 里 `admitCodemodeWorkers()` 在 Promise 执行器内同步运行，grant 的 resolve 先于
    // `setTimeout(0)` 回调；因此已预留时绝不能因为 0 毫秒计时器而误报超时。
    if let Ok(outcome) = receiver.try_recv() {
        if outcome == WorkerAdmission::Reserved {
            guard.take_slot();
        }
        return outcome;
    }
    let duration = Duration::from_millis(timeout_ms.max(0.0) as u64);
    let sleep = tokio::time::sleep(duration);
    tokio::pin!(sleep);
    let outcome = match signal {
        Some(signal) => {
            tokio::select! {
                biased;
                result = &mut receiver => result.unwrap_or(WorkerAdmission::Timeout),
                _ = signal.cancelled() => WorkerAdmission::Aborted,
                _ = &mut sleep => WorkerAdmission::Timeout,
            }
        }
        None => {
            tokio::select! {
                biased;
                result = &mut receiver => result.unwrap_or(WorkerAdmission::Timeout),
                _ = &mut sleep => WorkerAdmission::Timeout,
            }
        }
    };
    if outcome == WorkerAdmission::Reserved {
        guard.take_slot();
        return WorkerAdmission::Reserved;
    }
    // 取消等待：若 grant 已经先一步赢得 CAS，槽位按 TS 的 settle 语义归本脚本。
    if waiter.try_settle(outcome) {
        outcome
    } else {
        guard.take_slot();
        WorkerAdmission::Reserved
    }
}

/// `truncateText(text, maxChars)`：按 UTF-16 code unit 截断，尾部 `...`。
fn truncate_text(text: &str, max_chars: usize) -> String {
    if utf16_len(text) > max_chars {
        format!("{}...", utf16_slice(text, 0, max_chars.saturating_sub(3)))
    } else {
        text.to_string()
    }
}

/// `previewArgs(args)`：紧凑 JSON，截到 200 字符。
fn preview_args(args: &JsonValue) -> String {
    // kiana-codemode 用 `null` 表示 TS 的 `undefined`（无参调用）；TS 的 previewArgs(undefined)
    // 返回空串，因此这里同样返回空串。
    if args.is_null() {
        return String::new();
    }
    truncate_text(&kiana_json::js_object::stringify(args), ARGS_PREVIEW_CHARS)
}

/// `textOf(result)`：只拼文本块，用 `\n` 连接。
fn text_of(result: &AgentToolResult<JsonValue>) -> String {
    result
        .content
        .iter()
        .filter_map(|block| match block {
            UserContentBlock::Text(text) => Some(text.text.as_str()),
            UserContentBlock::Image(_) => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// JS 的 `JSON.stringify(value)`；`None` 表示 `undefined`。
fn json_stringify(value: Option<&JsonValue>) -> String {
    match value {
        Some(value) => kiana_json::js_object::stringify(value),
        None => "undefined".to_string(),
    }
}

/// `toModelType(value)`。
fn to_model_type(value: Option<&JsonValue>) -> Result<ModelType, String> {
    match value.and_then(JsonValue::as_str) {
        Some("chat") => Ok(ModelType::Chat),
        Some("image") => Ok(ModelType::Image),
        Some("classifier") => Ok(ModelType::Classifier),
        _ => Err(format!(
            "Unknown model type {}. Use \"chat\", \"image\", or \"classifier\".",
            json_stringify(value)
        )),
    }
}

/// `toProvider(value)`：`undefined`/`null` → `None`；非字符串 → 错误。
fn to_provider(value: Option<&JsonValue>) -> Result<Option<String>, String> {
    match value {
        None | Some(JsonValue::Null) => Ok(None),
        Some(JsonValue::String(provider)) => Ok(Some(provider.clone())),
        Some(_) => Err("provider must be a string".to_string()),
    }
}

/// 脚本拿到的目录条目：`{...model}` 但去掉 `headers`（models.json 的头可能带凭据）。
fn to_model_info(model: &Model) -> JsonValue {
    let mut value = serde_json::to_value(model).unwrap_or(JsonValue::Null);
    if let JsonValue::Object(object) = &mut value {
        object.shift_remove("headers");
    }
    value
}

/// `an image`、`a classifier`。
fn with_article(word: &str) -> String {
    let first = word.chars().next().unwrap_or(' ');
    if matches!(first, 'a' | 'e' | 'i' | 'o' | 'u') {
        format!("an {word}")
    } else {
        format!("a {word}")
    }
}

/// 脚本值在错误消息里的描述：`undefined`、`a string`、`an array`，或它的键（`{ prompt }`）。
fn describe_value(value: Option<&JsonValue>) -> String {
    match value {
        None => "undefined".to_string(),
        Some(JsonValue::Null) => "null".to_string(),
        Some(JsonValue::Array(array)) => {
            if array.is_empty() {
                "an empty array".to_string()
            } else {
                "an array".to_string()
            }
        }
        Some(JsonValue::Object(object)) => {
            if object.is_empty() {
                "{}".to_string()
            } else {
                let keys: Vec<&str> = object.keys().take(6).map(String::as_str).collect();
                let suffix = if object.len() > 6 { ", ..." } else { "" };
                format!("{{ {}{suffix} }}", keys.join(", "))
            }
        }
        Some(JsonValue::String(_)) => "a string".to_string(),
        Some(JsonValue::Number(_)) => "a number".to_string(),
        Some(JsonValue::Bool(_)) => "a boolean".to_string(),
    }
}

/// `CLASSIFIER_CONTEXT_SHAPE`（逐字）。
const CLASSIFIER_CONTEXT_SHAPE: &str = "'{ state: { ... }, images?: [{ type: \"image\", data: <base64>, mimeType }], questions: { <id>: { type: \"choice\", instructions, criteria: { <label>: <meaning> } } | { type: \"score\", instructions, criteria: [<lowest level>, ..., <highest level>] } | { type: \"bool\", instructions, criteria: { true: <meaning>, false: <meaning> } } } }'";

/// 检查脚本的 classifier context，让错误在本地以预期形状失败，而不是 provider 报错。
///
/// 所有失败共用同一段提示：`models.classify() <问题>. Expected context: ... See "Classify"
/// in <docs>`。校验顺序：context 必须是对象 → `state` 必须是对象 → 可选 `images` 必须是
/// `{ type: "image", data, mimeType }` 数组 → `questions` 必须是非空对象；每个问题按 `type`
/// 校验 `instructions` 与 `criteria`（`choice` 为非空的“标签→含义”字符串映射、`score` 为
/// 非空字符串数组、`bool` 为 `{ true, false }`）。
///
/// `state` 的值原样克隆（不做 JSON 语义转换）；问题与 criteria 的顺序按脚本对象键序保存到
/// `IndexMap`。返回的 `ClassifierContext` 直接交给 `models.classify`。
fn check_classifier_context(context: Option<&JsonValue>) -> Result<ClassifierContext, String> {
    let fail = |problem: &str| {
        format!(
            "models.classify() {problem}. Expected context: {CLASSIFIER_CONTEXT_SHAPE}. See \"Classify\" in {}.",
            *CODEMODE_DOCS_PATH
        )
    };
    let Some(JsonValue::Object(context)) = context else {
        return Err(fail(&format!(
            "expects a context object as its second argument, got {}",
            describe_value(context)
        )));
    };
    let state: IndexMap<String, JsonValue> = match context.get("state") {
        Some(JsonValue::Object(state)) => state
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
        other => {
            return Err(fail(&format!(
                "context.state must be an object, got {}",
                describe_value(other)
            )));
        }
    };
    let mut images = None;
    if let Some(value) = context.get("images") {
        let Some(array) = value.as_array() else {
            return Err(fail(&format!(
                "context.images must be an array, got {}",
                describe_value(Some(value))
            )));
        };
        let mut blocks = Vec::with_capacity(array.len());
        for (index, image) in array.iter().enumerate() {
            let parsed = match image {
                JsonValue::Object(image) => {
                    let data = image.get("data").and_then(JsonValue::as_str);
                    let mime_type = image.get("mimeType").and_then(JsonValue::as_str);
                    match (
                        image.get("type").and_then(JsonValue::as_str),
                        data,
                        mime_type,
                    ) {
                        (Some("image"), Some(data), Some(mime_type)) => Some(ImageContent {
                            kind_image: ImageKind::Image,
                            data: data.to_string(),
                            mime_type: mime_type.to_string(),
                        }),
                        _ => None,
                    }
                }
                _ => None,
            };
            match parsed {
                Some(parsed) => blocks.push(parsed),
                None => {
                    return Err(fail(&format!(
                        "context.images[{index}] must be an image block, got {}",
                        describe_value(Some(image))
                    )));
                }
            }
        }
        images = Some(blocks);
    }
    let questions = match context.get("questions") {
        Some(JsonValue::Object(questions)) if !questions.is_empty() => questions,
        other => {
            return Err(fail(&format!(
                "context.questions must map question IDs to questions, got {}",
                describe_value(other)
            )));
        }
    };
    let is_strings = |values: Vec<&JsonValue>| {
        !values.is_empty() && values.iter().all(|value| value.is_string())
    };
    let mut parsed_questions: IndexMap<String, ClassifierQuestion> = IndexMap::new();
    for (id, question) in questions {
        let at = format!("context.questions.{id}");
        let JsonValue::Object(question) = question else {
            return Err(fail(&format!(
                "{at} must be a question object, got {}",
                describe_value(Some(question))
            )));
        };
        let instructions = match question.get("instructions").and_then(JsonValue::as_str) {
            Some(instructions) => instructions.to_string(),
            None => return Err(fail(&format!("{at}.instructions must be a string"))),
        };
        let criteria = question.get("criteria");
        let question_type = question.get("type").and_then(JsonValue::as_str);
        let parsed = match question_type {
            Some("choice") => {
                let Some(JsonValue::Object(criteria)) = criteria else {
                    return Err(fail(&format!(
                        "{at} is a \"choice\" question, so criteria must map each label to its meaning"
                    )));
                };
                let values: Vec<&JsonValue> = criteria.values().collect();
                if !is_strings(values) {
                    return Err(fail(&format!(
                        "{at} is a \"choice\" question, so criteria must map each label to its meaning"
                    )));
                }
                let mut parsed_criteria = IndexMap::new();
                for (label, meaning) in criteria {
                    parsed_criteria.insert(
                        label.clone(),
                        meaning.as_str().unwrap_or_default().to_string(),
                    );
                }
                ClassifierQuestion::Choice {
                    instructions,
                    criteria: parsed_criteria,
                }
            }
            Some("score") => {
                let Some(JsonValue::Array(criteria)) = criteria else {
                    return Err(fail(&format!(
                        "{at} is a \"score\" question, so criteria must list the levels as strings, lowest first"
                    )));
                };
                let values: Vec<&JsonValue> = criteria.iter().collect();
                if !is_strings(values) {
                    return Err(fail(&format!(
                        "{at} is a \"score\" question, so criteria must list the levels as strings, lowest first"
                    )));
                }
                ClassifierQuestion::Score {
                    instructions,
                    criteria: criteria
                        .iter()
                        .filter_map(JsonValue::as_str)
                        .map(str::to_string)
                        .collect(),
                }
            }
            Some("bool") => {
                let labels = criteria.and_then(JsonValue::as_object);
                let true_label = labels
                    .and_then(|labels| labels.get("true"))
                    .and_then(JsonValue::as_str);
                let false_label = labels
                    .and_then(|labels| labels.get("false"))
                    .and_then(JsonValue::as_str);
                match (true_label, false_label) {
                    (Some(true_label), Some(false_label)) => ClassifierQuestion::Bool {
                        instructions,
                        criteria: BoolQuestionCriteria {
                            r#true: true_label.to_string(),
                            r#false: false_label.to_string(),
                        },
                    },
                    _ => {
                        return Err(fail(&format!(
                            "{at} is a \"bool\" question, so criteria must be {{ true: string, false: string }}"
                        )));
                    }
                }
            }
            _ => {
                return Err(fail(&format!(
                    "{at}.type must be \"choice\", \"score\", or \"bool\", got {}",
                    json_stringify(question.get("type"))
                )));
            }
        };
        parsed_questions.insert(id.clone(), parsed);
    }
    Ok(ClassifierContext {
        state,
        images,
        questions: parsed_questions,
    })
}

/// 检查脚本的 image context。
///
/// 要求 context 是对象且 `input` 是非空数组；每个块必须是 `{ type: "text", text }` 或
/// `{ type: "image", data, mimeType }`，其它形状（包括缺字段、未知 type）返回带期望形状的
/// 错误文本。错误提示与 `models.classify` 一样指向 [`CODEMODE_DOCS_PATH`]。
fn check_images_context(context: Option<&JsonValue>) -> Result<ImagesContext, String> {
    let fail = |problem: &str| {
        format!(
            "models.generateImages() {problem}. Expected context: {{ input: [{{ type: \"text\", text: <prompt> }}, ...optional {{ type: \"image\", data: <base64>, mimeType }} references] }}. See \"Generate images\" in {}.",
            *CODEMODE_DOCS_PATH
        )
    };
    let Some(JsonValue::Object(context)) = context else {
        return Err(fail(&format!(
            "expects a context object as its second argument, got {}",
            describe_value(context)
        )));
    };
    let Some(JsonValue::Array(input)) = context.get("input") else {
        return Err(fail(&format!(
            "context.input must be a non-empty array of blocks, got {}",
            describe_value(context.get("input"))
        )));
    };
    if input.is_empty() {
        return Err(fail(&format!(
            "context.input must be a non-empty array of blocks, got {}",
            describe_value(context.get("input"))
        )));
    }
    let mut blocks = Vec::with_capacity(input.len());
    for (index, block) in input.iter().enumerate() {
        let parsed = match block {
            JsonValue::Object(block) => {
                let block_type = block.get("type").and_then(JsonValue::as_str);
                match block_type {
                    Some("text") => block.get("text").and_then(JsonValue::as_str).map(|text| {
                        UserContentBlock::Text(TextContent {
                            kind_text: TextKind::Text,
                            text: text.to_string(),
                            text_signature: None,
                        })
                    }),
                    Some("image") => {
                        let data = block.get("data").and_then(JsonValue::as_str);
                        let mime_type = block.get("mimeType").and_then(JsonValue::as_str);
                        match (data, mime_type) {
                            (Some(data), Some(mime_type)) => {
                                Some(UserContentBlock::Image(ImageContent {
                                    kind_image: ImageKind::Image,
                                    data: data.to_string(),
                                    mime_type: mime_type.to_string(),
                                }))
                            }
                            _ => None,
                        }
                    }
                    _ => None,
                }
            }
            _ => None,
        };
        match parsed {
            Some(parsed) => blocks.push(parsed),
            None => {
                return Err(fail(&format!(
                    "context.input[{index}] must be a text or image block, got {}",
                    describe_value(Some(block))
                )));
            }
        }
    }
    Ok(ImagesContext { input: blocks })
}

/// 嵌套调用的模型结果里行需要报告的字段。
struct ModelCallResult {
    stop_reason: StopKind,
    error_message: Option<String>,
    usage: Option<Usage>,
}

/// 三种停止原因（classifier 与 image 结果共用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StopKind {
    Stop,
    Aborted,
    Error,
}

impl From<ClassifierStopReason> for StopKind {
    fn from(reason: ClassifierStopReason) -> Self {
        match reason {
            ClassifierStopReason::Stop => StopKind::Stop,
            ClassifierStopReason::Aborted => StopKind::Aborted,
            ClassifierStopReason::Error => StopKind::Error,
        }
    }
}

impl From<ImagesStopReason> for StopKind {
    fn from(reason: ImagesStopReason) -> Self {
        match reason {
            ImagesStopReason::Stop => StopKind::Stop,
            ImagesStopReason::Aborted => StopKind::Aborted,
            ImagesStopReason::Error => StopKind::Error,
        }
    }
}

/// 按调用顺序、最多同时运行 `limit` 个调用的限制器。
///
/// 直接使用 tokio 的 [`Semaphore`](tokio::sync::Semaphore)：它在等待 future 被取消时会
/// 自己清理 waiter，permit 随持有者 drop 归还。自建 `oneshot` 队列做不到这一点——脚本被
/// 中止时 sender 留在队列里，下一次 release 的 `send` 失败且不递减 `active`，槽位就永久
/// 丢失；`limit` 次取消后该脚本的所有模型调用都会挂起。
struct ModelCallLimiter {
    semaphore: tokio::sync::Semaphore,
}

impl ModelCallLimiter {
    fn new(limit: usize) -> Arc<Self> {
        Arc::new(ModelCallLimiter {
            semaphore: tokio::sync::Semaphore::new(limit),
        })
    }

    /// 取一个 permit；调用方 drop permit（包括 future 被取消）即归还槽位。
    async fn acquire(&self) -> tokio::sync::SemaphorePermit<'_> {
        self.semaphore
            .acquire()
            .await
            .expect("model call limiter is never closed")
    }

    async fn run<T>(&self, run: impl std::future::Future<Output = T>) -> T {
        let permit = self.acquire().await;
        let output = run.await;
        drop(permit);
        output
    }
}

/// 判断一个 store entry 的 `data` 是否是 [`CodemodeStoreEntryData`]。
fn is_store_entry_data(data: Option<&JsonValue>) -> bool {
    let Some(JsonValue::Object(object)) = data else {
        return false;
    };
    let set_ok = matches!(
        object.get("set"),
        Some(JsonValue::Object(_) | JsonValue::Array(_))
    );
    let delete_ok = matches!(object.get("delete"), Some(JsonValue::Array(keys)) if keys.iter().all(JsonValue::is_string));
    set_ok && delete_ok
}

/// `load()` 的值：分支上的 `codemode-store` 条目，从根开始应用。
///
/// 按分支顺序遍历，只处理 `custom_type == "codemode-store"` 且 `data` 形如
/// `{ set: object|array, delete: string[] }` 的条目：`set` 不是对象/数组、或 `delete` 不是
/// 字符串数组时，整条跳过（`is_store_entry_data` 要求两者同时成立）。
///
/// 同一条目内**先 `delete` 再 `set`**，所以同一个键可以“删掉再写回”；先删后插会让键在
/// `IndexMap` 中移动到末尾（`tests/codemode_store.rs` 锁定该顺序）。数组形式的 `set` 用
/// 下标字符串做键，对应 TS 的 `Object.entries` 语义。返回值的插入顺序由 `IndexMap` 保留，
/// `execute_codemode` 直接把它作为 `load()` 的初值快照传给沙箱（沙箱按 JS 对象枚举序序列化：
/// 整数索引键升序在前，其余键保持 `IndexMap` 插入序）。
pub fn read_codemode_store(branch: &[SessionEntry]) -> IndexMap<String, JsonValue> {
    let mut store: IndexMap<String, JsonValue> = IndexMap::new();
    for entry in branch {
        let SessionEntry::Custom(entry) = entry else {
            continue;
        };
        if entry.custom_type != CODEMODE_STORE_ENTRY_TYPE {
            continue;
        }
        let data = entry.data.as_ref();
        if !is_store_entry_data(data) {
            continue;
        }
        let Some(JsonValue::Object(data)) = data else {
            continue;
        };
        if let Some(JsonValue::Array(delete)) = data.get("delete") {
            for key in delete.iter().filter_map(JsonValue::as_str) {
                store.shift_remove(key);
            }
        }
        match data.get("set") {
            Some(JsonValue::Object(set)) => {
                for (key, value) in set {
                    store.insert(key.clone(), value.clone());
                }
            }
            Some(JsonValue::Array(set)) => {
                for (index, value) in set.iter().enumerate() {
                    store.insert(index.to_string(), value.clone());
                }
            }
            _ => {}
        }
    }
    store
}

/// `valueText(value)`：字符串原样；其它值是紧凑 JSON。
fn value_text(value: &JsonValue) -> String {
    match value {
        JsonValue::String(text) => text.clone(),
        other => kiana_json::js_object::stringify(other),
    }
}

fn text_block(text: impl Into<String>) -> UserContentBlock {
    UserContentBlock::Text(TextContent {
        kind_text: TextKind::Text,
        text: text.into(),
        text_signature: None,
    })
}

fn image_block(data: &str, mime_type: &str) -> UserContentBlock {
    UserContentBlock::Image(ImageContent {
        kind_image: ImageKind::Image,
        data: data.to_string(),
        mime_type: mime_type.to_string(),
    })
}

/// 把脚本输出排成模型能分辨的项。
///
/// - 非 console 的文本项保持出现顺序并编号；仅当这类文本项总数大于 1 时才加
///   `==> text i/n <==` 头，图片不占编号；
/// - console 文本不参与编号，全部先收集，最后合并成一个 `<console_output>` 文本项追加在
///   末尾（因此图片之后也可能出现它）；
/// - 图片块保持原顺序、不与文本合并。
fn format_output(output: &[CodemodeOutputItem]) -> Vec<UserContentBlock> {
    let total = output
        .iter()
        .filter(|item| match item {
            CodemodeOutputItem::Text { console, .. } => *console != Some(true),
            CodemodeOutputItem::Image { .. } => false,
        })
        .count();
    let mut items = Vec::new();
    let mut console_lines = Vec::new();
    let mut index = 0;
    for item in output {
        match item {
            CodemodeOutputItem::Image { data, mime_type } => {
                items.push(image_block(data, mime_type));
            }
            CodemodeOutputItem::Text {
                text,
                console: Some(true),
            } => console_lines.push(text.clone()),
            CodemodeOutputItem::Text { text, .. } => {
                index += 1;
                let text = if total > 1 {
                    format!("==> text {index}/{total} <==\n{text}")
                } else {
                    text.clone()
                };
                items.push(text_block(text));
            }
        }
    }
    if !console_lines.is_empty() {
        items.push(text_block(format!(
            "<console_output>\n{}\n</console_output>",
            console_lines.join("\n")
        )));
    }
    items
}

/// 合并相邻文本项，每部分以换行开始；只看最后一个非空部分（避免 O(n²)）。
///
/// “相邻”以图片为分隔：遇到图片先 flush 当前文本段，再推入图片。两个文本之间只在上一个
/// 非空文本不以 `\n` 结尾时插入一个换行，所以原本就以换行结尾的文本不会多出空行；空文本
/// 不更新 `last_part`。文本段用 `parts` 收集后一次 `join`，避免反复拼接。
fn join_adjacent_text(items: Vec<UserContentBlock>) -> Vec<UserContentBlock> {
    let mut joined: Vec<UserContentBlock> = Vec::with_capacity(items.len());
    let mut parts: Vec<String> = Vec::new();
    let mut last_part = String::new();
    let flush = |joined: &mut Vec<UserContentBlock>, parts: &mut Vec<String>| {
        if !parts.is_empty() {
            joined.push(text_block(parts.join("")));
            parts.clear();
        }
    };
    for item in items {
        match item {
            UserContentBlock::Image(_) => {
                flush(&mut joined, &mut parts);
                last_part.clear();
                joined.push(item);
            }
            UserContentBlock::Text(text) => {
                if !last_part.is_empty() && !last_part.ends_with('\n') {
                    parts.push("\n".to_string());
                    last_part = "\n".to_string();
                }
                parts.push(text.text.clone());
                if !text.text.is_empty() {
                    last_part = text.text;
                }
            }
        }
    }
    flush(&mut joined, &mut parts);
    joined
}

/// `formatCallSummary(calls)`：错误文本后的调用清单。
///
/// 明确写出这些调用“不会撤销”，因为脚本失败不会回滚已经执行的嵌套工具；没有调用时给出
/// 单句 "No tool calls were made."。
fn format_call_summary(calls: &[CodemodeNestedCall]) -> String {
    if calls.is_empty() {
        return "No tool calls were made.".to_string();
    }
    format!(
        "Tool calls made before the failure (they are not undone): {}",
        calls
            .iter()
            .map(|call| format!("{} ({})", call.name, status_str(call.status)))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn status_str(status: CodemodeNestedCallStatus) -> &'static str {
    match status {
        CodemodeNestedCallStatus::Running => "running",
        CodemodeNestedCallStatus::Ok => "ok",
        CodemodeNestedCallStatus::Error => "error",
        CodemodeNestedCallStatus::Cancelled => "cancelled",
    }
}

/// `formatError(result, calls)`：失败标题加调用清单。
///
/// `Script` 类错误优先使用沙箱给的 JS `stack`（保留 `at <anonymous>:1:1` 这类帧），没有 stack
/// 才拼 `name: message`；其它 kind 用固定的 `Script timed out:` / `Script aborted:` /
/// `Script sandbox failed:` 前缀。末尾总是接 [`format_call_summary`] 的结果。
fn format_error(result: &CodemodeResultErr, calls: &[CodemodeNestedCall]) -> String {
    let error = &result.error;
    let head = match error.kind {
        kiana_codemode::CodemodeErrorKind::Script => error.stack.clone().unwrap_or_else(|| {
            format!(
                "{}: {}",
                error.name.as_deref().unwrap_or("Error"),
                error.message
            )
        }),
        kiana_codemode::CodemodeErrorKind::Timeout => {
            format!("Script timed out: {}", error.message)
        }
        kiana_codemode::CodemodeErrorKind::Aborted => {
            format!("Script aborted: {}", error.message)
        }
        kiana_codemode::CodemodeErrorKind::Sandbox => {
            format!("Script sandbox failed: {}", error.message)
        }
    };
    format!("{head}\n\n{}", format_call_summary(calls))
}

/// 把完整文本写入临时文件。
fn spill_output(text: &str) -> Result<std::path::PathBuf, String> {
    write_output_file("kiana-codemode", ".txt", text.as_bytes())
        .map_err(|error| node_output_file_error(&error, "kiana-codemode", ".txt"))
}

/// libuv 对常见文件系统 errno 的命名（Linux 数值；其它平台先按 `ErrorKind` 兜底）。
///
/// 与 `mcp/tools.rs` 的同名表一致；Node 的 `uv_strerror(ENAMETOOLONG)` 是 `name too long`
/// （不是 `file name too long`）。
#[cfg(unix)]
const UNIX_ERRNO: [(i32, &str, &str); 14] = [
    (1, "EPERM", "operation not permitted"),
    (2, "ENOENT", "no such file or directory"),
    (13, "EACCES", "permission denied"),
    (16, "EBUSY", "resource busy or locked"),
    (17, "EEXIST", "file already exists"),
    (20, "ENOTDIR", "not a directory"),
    (21, "EISDIR", "illegal operation on a directory"),
    (22, "EINVAL", "invalid argument"),
    (24, "EMFILE", "too many open files"),
    (28, "ENOSPC", "no space left on device"),
    (30, "EROFS", "read-only file system"),
    (36, "ENAMETOOLONG", "name too long"),
    (40, "ELOOP", "too many symbolic links encountered"),
    (122, "EDQUOT", "disk quota exceeded"),
];

/// `(code, description)`：Node/libuv 的 errno 文本。
fn node_errno(error: &std::io::Error) -> Option<(&'static str, &'static str)> {
    #[cfg(unix)]
    if let Some(raw) = error.raw_os_error()
        && let Some(entry) = UNIX_ERRNO.iter().find(|(code, _, _)| *code == raw)
    {
        return Some((entry.1, entry.2));
    }
    match error.kind() {
        std::io::ErrorKind::NotFound => Some(("ENOENT", "no such file or directory")),
        std::io::ErrorKind::PermissionDenied => Some(("EACCES", "permission denied")),
        std::io::ErrorKind::AlreadyExists => Some(("EEXIST", "file already exists")),
        std::io::ErrorKind::InvalidInput => Some(("EINVAL", "invalid argument")),
        _ => None,
    }
}

/// Node `os.tmpdir()` 的等价物（Unix：`TMPDIR || TMP || TEMP || "/tmp"`；Windows：
/// `TEMP || TMP || (SystemRoot || windir) + "\\temp"`，见 `kiana_core::output_files`）。
///
/// 只用于拼出错误文案里的路径：`kiana_core::output_files::write_output_file` 的 `io::Error`
/// 不携带它尝试的路径，而 Node 的 `error.message` 是 `{code}: {desc}, open '<path>'`。
fn node_tmpdir() -> std::path::PathBuf {
    let value = if cfg!(windows) {
        let from_env = ["TEMP", "TMP"]
            .iter()
            .find_map(|key| std::env::var_os(key).filter(|value| !value.is_empty()));
        from_env.unwrap_or_else(|| {
            let mut root = ["SystemRoot", "windir"]
                .iter()
                .find_map(|key| std::env::var_os(key).filter(|value| !value.is_empty()))
                .unwrap_or_else(|| std::ffi::OsString::from("undefined"));
            root.push("\\temp");
            root
        })
    } else {
        ["TMPDIR", "TMP", "TEMP"]
            .iter()
            .find_map(|key| std::env::var_os(key).filter(|value| !value.is_empty()))
            .unwrap_or_else(|| std::ffi::OsString::from("/tmp"))
    };
    let Some(mut text) = value.to_str().map(str::to_owned) else {
        return std::path::PathBuf::from(value);
    };
    // Node 只去掉一个结尾分隔符；Windows 上 `C:\` 保留。
    let separator = std::path::MAIN_SEPARATOR;
    let units: Vec<u16> = text.encode_utf16().collect();
    if units.len() > 1 && units.last() == Some(&u16::from(separator as u8)) {
        let keep = cfg!(windows) && units.get(units.len() - 2) == Some(&u16::from(b':'));
        if !keep {
            text.pop();
        }
    }
    std::path::PathBuf::from(text)
}

/// Node `fs.writeFile` 打开临时文件失败时的 `error.message`：
/// `{CODE}: {描述}, open '<path>'`。未知 errno 退回 `io::Error` 的 Display。
///
/// Node 的路径是 `<tmpdir>/<prefix>-<random hex><extension>`；随机部分本来就不可观测，
/// 这里用时间/进程哈希生成同形路径（真正的写入仍由 `kiana_core::output_files` 完成）。
fn node_output_file_error(error: &std::io::Error, prefix: &str, extension: &str) -> String {
    let Some((code, description)) = node_errno(error) else {
        return error.to_string();
    };
    let path = temp_output_file_path(prefix, extension);
    format!("{code}: {description}, open '{path}'")
}

/// `kiana-core` 临时文件路径的同形文本（见 [`node_output_file_error`]）。
fn temp_output_file_path(prefix: &str, extension: &str) -> String {
    use std::hash::{BuildHasher as _, Hasher as _};
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    hasher.write_u128(nanos);
    hasher.write_u32(std::process::id());
    let hex = format!("{:016x}", hasher.finish());
    let mut directory = node_tmpdir().to_string_lossy().into_owned();
    let separator = std::path::MAIN_SEPARATOR;
    if !directory.ends_with(separator) {
        directory.push(separator);
    }
    format!("{directory}{prefix}-{hex}{extension}")
}

/// 宽松的 Node `Buffer.from(data, "base64")` 解码。
///
/// V8 的解码是宽松的（Node 26 实测）：`-`/`_` 等同 `+`/`/`，字母表之外的字符全部忽略，
/// `=` 终止输入；不足 2 个符号的尾部与最后不完整组的多余位丢弃（`QUJ=` → `4142`）。
/// V8 按 UTF-16 code unit 的**低字节**查表（`unbase64_table[c & 0xFF]`）：`Ł`（U+0141）
/// 等同 `A`，代理项的低字节是 `0x3D` 时等同 `=` 终止输入。
fn node_base64_decode(data: &str) -> Vec<u8> {
    let mut symbols: Vec<u8> = Vec::with_capacity(data.len());
    for unit in data.encode_utf16() {
        let byte = (unit & 0xff) as u8;
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            _ => continue,
        };
        symbols.push(value);
    }
    let mut bytes = Vec::with_capacity(symbols.len() * 3 / 4);
    for chunk in symbols.chunks(4) {
        match chunk {
            [first, second] => bytes.push((first << 2) | (second >> 4)),
            [first, second, third] => {
                bytes.push((first << 2) | (second >> 4));
                bytes.push(((second & 0x0f) << 4) | (third >> 2));
            }
            [first, second, third, fourth] => {
                bytes.push((first << 2) | (second >> 4));
                bytes.push(((second & 0x0f) << 4) | (third >> 2));
                bytes.push(((third & 0x03) << 6) | fourth);
            }
            _ => {}
        }
    }
    bytes
}

/// 保存每张图片到临时文件，并在它前面插入带路径的文本项。同一 `data` 只保存一次。
///
/// - 去重键是 base64 `data` 本身：相同数据只写一次文件、复用同一个标签文本，但每个图片块
///   仍各自输出；
/// - 标签形如 `[Image saved to <path> (<mime>, <size>)]`，大小按解码后的字节数格式化；
/// - MIME 不在 [`IMAGE_EXTENSIONS`] 里时返回 `AgentToolError`，整次工具调用失败（拿不到
///   扩展名，无法落盘）；
/// - 写文件失败**不**让工具失败：标签改成 `[Image (...) could not be saved: <msg>]` 并继续。
fn save_images(items: Vec<UserContentBlock>) -> Result<Vec<UserContentBlock>, AgentToolError> {
    let mut labels: HashMap<String, String> = HashMap::new();
    let mut output = Vec::with_capacity(items.len());
    for item in items {
        let UserContentBlock::Image(image) = &item else {
            output.push(item);
            continue;
        };
        let label = match labels.get(&image.data) {
            Some(label) => label.clone(),
            None => {
                let bytes = node_base64_decode(&image.data);
                let kind = format!("{}, {}", image.mime_type, format_size(bytes.len() as u64));
                let Some((_, extension)) = IMAGE_EXTENSIONS
                    .iter()
                    .find(|(mime_type, _)| *mime_type == image.mime_type)
                else {
                    return Err(AgentToolError {
                        message: format!("No file extension for image type {}", image.mime_type),
                        name: None,
                        stack: None,
                    });
                };
                let label = match write_output_file("kiana-codemode", extension, &bytes) {
                    Ok(path) => format!("[Image saved to {} ({kind})]", path.display()),
                    Err(error) => format!(
                        "[Image ({kind}) could not be saved: {}]",
                        node_output_file_error(&error, "kiana-codemode", extension)
                    ),
                };
                labels.insert(image.data.clone(), label.clone());
                label
            }
        };
        output.push(text_block(label));
        output.push(item);
    }
    Ok(output)
}

/// 应用 token 预算：合并文本超过预算时，文本项变成一个保留开头与结尾的项，图片随后。
/// 完整文本写入临时文件。
///
/// 预算按 `max_tokens * CHARS_PER_TOKEN` 个 UTF-16 code unit 估算；只有所有文本项拼起来的
/// 长度**超过**预算才截断（没有文本项时原样返回）。截断保留头部 `floor(budget / 2)` 与尾部
/// `budget - head`，中间用 `…N tokens truncated…` 连接；`original token count` 用
/// `length.div_ceil(CHARS_PER_TOKEN)`，截断量用 `removed.div_ceil(CHARS_PER_TOKEN)` 向上取整
/// （`length` 是 UTF-16 code unit 数，`removed = length - head_chars - tail_chars`）。
///
/// 全文经 [`spill_output`] 写入临时文件并在末尾附路径；保存失败只把错误文本附在提示里。
/// 返回值的第二个元素是 `details.full_output_path`，未截断或保存失败时为 `None`。截断后的
/// 输出固定是“一个文本块 + 所有图片块”，图片之间的相对顺序不变。
fn truncate_output(
    items: Vec<UserContentBlock>,
    max_tokens: f64,
) -> Result<(Vec<UserContentBlock>, Option<String>), AgentToolError> {
    let texts: Vec<&str> = items
        .iter()
        .filter_map(|item| match item {
            UserContentBlock::Text(text) => Some(text.text.as_str()),
            UserContentBlock::Image(_) => None,
        })
        .collect();
    let combined = texts.join("\n");
    let budget = max_tokens * CHARS_PER_TOKEN as f64;
    let length = utf16_len(&combined);
    if texts.is_empty() || length as f64 <= budget {
        return Ok((items, None));
    }
    let head_chars = (budget / 2.0).floor() as usize;
    let tail_chars = (budget - head_chars as f64) as usize;
    let removed = length - head_chars - tail_chars;
    let head = utf16_slice(&combined, 0, head_chars);
    let tail = if tail_chars > 0 {
        utf16_slice(&combined, length - tail_chars, length)
    } else {
        String::new()
    };
    let mut text = format!(
        "Warning: truncated output (original token count: {})\nTotal output lines: {}\n\n{head}…{} tokens truncated…{tail}",
        length.div_ceil(CHARS_PER_TOKEN),
        combined.split('\n').count(),
        removed.div_ceil(CHARS_PER_TOKEN)
    );
    let spilled = spill_output(&combined);
    let full_output_path = match &spilled {
        Ok(path) => {
            text.push_str(&format!(
                "\n\n[Full output: {} (read with offset/limit)]",
                path.display()
            ));
            Some(path.to_string_lossy().into_owned())
        }
        Err(error) => {
            text.push_str(&format!("\n\n[Could not save the full output: {error}]"));
            None
        }
    };
    let mut output = vec![text_block(text)];
    output.extend(
        items
            .into_iter()
            .filter(|item| matches!(item, UserContentBlock::Image(_))),
    );
    Ok((output, full_output_path))
}

/// 嵌套调用的解析值：声明了 `outputSchema` 的工具解析为其 `structuredContent`（错误结果也
/// 一样）；其它工具解析为文本内容；其它失败以工具错误文本 reject。
fn to_script_value(tool: &AgentTool, outcome: &AgentToolCallOutcome) -> Result<JsonValue, String> {
    if kiana_json::is_truthy(tool.output_schema.as_ref())
        && let Some(structured) = &outcome.result.structured_content
    {
        return Ok(structured.clone());
    }
    let text = text_of(&outcome.result);
    if outcome.is_error {
        return Err(if text.is_empty() {
            format!("Tool \"{}\" failed", tool.name)
        } else {
            text
        });
    }
    Ok(JsonValue::String(text))
}

/// 当前嵌套调用快照（`details.calls`）。
fn snapshot_details(calls: &Arc<Mutex<Vec<CodemodeNestedCall>>>) -> CodemodeToolDetails {
    CodemodeToolDetails {
        calls: calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone(),
        full_output_path: None,
    }
}

/// `input.code` / `input.trim` 在 V8 里抛出的 TypeError。
///
/// TS 直接 `parseCodemodeSource(input.code)`：缺省/非字符串的 `code` 得到的是
/// `Cannot read properties of undefined (reading 'trim')`、`input.trim is not a function` 等，
/// 不是 `CodemodeSourceError`。Agent loop 的 schema 校验会先挡住这些输入，只有 SDK/直接
/// 调用 `AgentTool` 时可达。
fn input_type_error(message: impl Into<String>) -> AgentToolError {
    AgentToolError {
        message: message.into(),
        name: Some("TypeError".to_string()),
        stack: None,
    }
}

/// 运行一个脚本。没有会话上下文时脚本不能调用工具，`store()` 从空开始，写入被丢弃。
///
/// # 参数
///
/// - `input`：`{ "code": "<JavaScript 源码>" }`。缺失或非字符串时按 TS 读取 `input.code` /
///   `input.trim` 的 TypeError 文案返回 `Err`；字符串再交给 `parse_codemode_source`。
/// - `signal`：整次执行的取消来源，覆盖等 worker、沙箱执行与嵌套工具调用。
/// - `on_update`：嵌套调用状态变化时收到 `details` 快照（`content` 为空）；函数开头会先发布
///   一次，让调用方在脚本启动前就能感知到这次调用。
/// - `ctx`：提供工具清单、`execute_tool`、模型注册表与会话分支。`None` 时脚本拿不到工具与
///   `models`，`store()` 起始为空；成功写入是否持久化取决于 `options.append_entry` 是否存在
///   （对应 TS 的 `ctx === undefined`，那时通常没有 appendEntry 可用）。
/// - `options`：见 [`CodemodeToolOptions`]；其中 `get_tool_guidelines` 影响脚本看到的工具
///   声明样本，`get_tool_namespace` 影响 `searchTools()` 的 namespace 过滤与
///   `describeNamespace()`，`models` 控制是否注册 `models.*`，
///   `append_entry` 决定 `store()` 写入能否持久化。
///
/// # 返回值
///
/// 首块文本是 `Script completed` 或 `Script failed` 加 `Wall time <n> seconds` 与 `Output:`，
/// 其后是按文件头固定顺序整形的输出；失败脚本在脚本输出之后附 `Script error:` 段，若有未
/// 展示图片，其后还会追加提示，图片标签也在之后插入。`details` 是
/// [`CodemodeToolDetails`]（嵌套调用快照与全文落盘路径），`usage` 只汇总脚本内 `models.*`
/// 调用（嵌套工具调用的 usage 由会话侧统计）。
///
/// # 错误与失败模式
///
/// 返回 `Err` 覆盖：非法 input、源码解析失败、sandbox 构造失败（`StartError`，例如非法全局
/// 名），以及执行后 `save_images` 的未知 MIME 扩展名错误。脚本自身的超时、abort 与 JS 异常都
/// 以 `CodemodeResult::Err` 返回，被包装成 `is_error: Some(true)` 的正常结果；等不到 worker
/// 时同样返回 `is_error` 结果（文案区分 abort、deadline 先到与容量不足）。两类结果都保留
/// 已经产生的嵌套调用记录。
///
/// # 关键不变量
///
/// - 等待 worker 的时间计入 `timeout_ms`：实际传给 sandbox 的 deadline 是
///   `round(deadline - waited_ms).max(1.0)`，等待与执行不会各自拿到完整预算；
///   `// @options:` 未给 `timeout_ms` 时 deadline 为无穷，沙箱不设总 deadline。
/// - 只有成功结果的 `store()` 写入会交给 `append_entry`；`append_entry` 未配置时静默丢弃。
/// - 执行结束时仍为 `Running` 的嵌套调用统一改成 `Cancelled`，避免 `details` 停在运行中。
pub async fn execute_codemode(
    tool_call_id: String,
    input: JsonValue,
    signal: Option<CancellationToken>,
    on_update: Option<AgentToolUpdateCallback>,
    ctx: Option<Arc<dyn ExtensionToolContextApi>>,
    options: CodemodeToolOptions,
) -> Result<AgentToolResult<JsonValue>, AgentToolError> {
    let started_at = Instant::now();
    // TS: `parseCodemodeSource(input.code)`——`input.code` 的读取与 `input.trim` 的调用各自
    // 可能抛 TypeError，文案取决于输入值的类型。
    let code = match &input {
        JsonValue::Object(map) => match map.get("code") {
            None => {
                return Err(input_type_error(
                    "Cannot read properties of undefined (reading 'trim')",
                ));
            }
            Some(JsonValue::Null) => {
                return Err(input_type_error(
                    "Cannot read properties of null (reading 'trim')",
                ));
            }
            Some(JsonValue::String(code)) => code,
            Some(_) => return Err(input_type_error("input.trim is not a function")),
        },
        JsonValue::Null => {
            return Err(input_type_error(
                "Cannot read properties of null (reading 'code')",
            ));
        }
        // 字符串/数字/布尔/数组上的 `input.code` 都是 `undefined`。
        _ => {
            return Err(input_type_error(
                "Cannot read properties of undefined (reading 'trim')",
            ));
        }
    };
    let parsed = parse_codemode_source(code).map_err(|error| AgentToolError {
        message: error.message.clone(),
        name: Some(error.name().to_string()),
        stack: None,
    })?;
    let calls: Arc<Mutex<Vec<CodemodeNestedCall>>> = Arc::new(Mutex::new(Vec::new()));
    // 脚本 `models.*` 调用的 usage；嵌套工具调用的 usage 由会话侧统计。
    let model_usage: Arc<Mutex<Option<Usage>>> = Arc::new(Mutex::new(None));
    // `models.generateImages()` 返回的图片数，用于发现脚本从未展示的图片。
    let generated_images = Arc::new(AtomicUsize::new(0));

    let publish: Arc<dyn Fn() + Send + Sync> = {
        let calls = Arc::clone(&calls);
        let on_update = on_update.clone();
        Arc::new(move || {
            if let Some(on_update) = &on_update {
                let details =
                    serde_json::to_value(snapshot_details(&calls)).unwrap_or(JsonValue::Null);
                on_update(AgentToolResult {
                    content: Vec::new(),
                    details,
                    structured_content: None,
                    usage: None,
                    is_error: None,
                    terminate: None,
                });
            }
        })
    };
    // 先让行变成 running，启动计时器。
    publish();

    // 工具声明在此生成一次：`samples` 既作为沙箱里每个工具的 `description`，也是
    // `describeTool()` / `searchTools()` 返回给脚本的内容。`get_codemode_callable_tools`
    // 只去掉 `codemode` 自身（脚本不能再启动脚本）。
    let callable = ctx
        .as_ref()
        .map(|ctx| get_codemode_callable_tools(&ctx.tools()))
        .unwrap_or_default();
    let guidelines = options
        .get_tool_guidelines
        .as_ref()
        .map(|get_tool_guidelines| get_tool_guidelines());
    let mut samples: IndexMap<String, String> = IndexMap::new();
    for tool in &callable {
        let tool_guidelines = guidelines
            .as_ref()
            .and_then(|guidelines| guidelines.get(&tool.name));
        let declaration =
            to_codemode_declaration(tool, tool_guidelines.map_or(&[][..], Vec::as_slice));
        samples.insert(tool.name.clone(), render_tool_sample(&declaration, None));
    }
    let samples = Arc::new(samples);
    // 每个可调用工具包成 `CodemodeTool`：脚本调用时先追加一条 Running 记录并 publish，再转给
    // 会话的 `execute_tool`（与直接调用走同一条工具管线），完成后回写状态/耗时/错误预览，
    // 最后按 `to_script_value` 决定脚本看到的值。
    let sandbox_tools: Vec<CodemodeTool> = callable
        .iter()
        .map(|tool| {
            let tool_name = tool.name.clone();
            let tool_for_value = tool.clone();
            let sample = samples.get(&tool.name).cloned().unwrap_or_default();
            let calls = Arc::clone(&calls);
            let publish = Arc::clone(&publish);
            let ctx = ctx.clone();
            let tool_call_id = tool_call_id.clone();
            CodemodeTool {
                name: tool.name.clone(),
                description: Some(sample),
                input_schema: None,
                output_schema: None,
                spread: None,
                signature: None,
                execute: Arc::new(move |args: JsonValue, context: CodemodeToolContext| {
                    let tool_name = tool_name.clone();
                    let tool_for_value = tool_for_value.clone();
                    let calls = Arc::clone(&calls);
                    let publish = Arc::clone(&publish);
                    let ctx = ctx.clone();
                    let tool_call_id = tool_call_id.clone();
                    Box::pin(async move {
                        let record = CodemodeNestedCall {
                            id: format!("{tool_call_id}/?"),
                            name: tool_name.clone(),
                            args: preview_args(&args),
                            status: CodemodeNestedCallStatus::Running,
                            duration_ms: None,
                            error: None,
                            cost: None,
                        };
                        let index = {
                            let mut calls = calls
                                .lock()
                                .unwrap_or_else(|poisoned| poisoned.into_inner());
                            calls.push(record);
                            calls.len() - 1
                        };
                        publish();
                        let call_started = Instant::now();
                        let Some(ctx) = &ctx else {
                            return Err("Tool calls need a session".to_string());
                        };
                        let outcome = ctx
                            .execute_tool(
                                &tool_name,
                                args,
                                Some(ExecuteToolOptions {
                                    signal: Some(context.signal.clone()),
                                    on_update: None,
                                }),
                            )
                            .await;
                        let duration = call_started.elapsed().as_secs_f64() * 1000.0;
                        let error_text = if outcome.is_error {
                            let text = text_of(&outcome.result);
                            let text = if text.is_empty() {
                                format!("Tool \"{tool_name}\" failed")
                            } else {
                                text
                            };
                            Some(truncate_text(&text, ERROR_PREVIEW_CHARS))
                        } else {
                            None
                        };
                        {
                            let mut calls = calls
                                .lock()
                                .unwrap_or_else(|poisoned| poisoned.into_inner());
                            if let Some(record) = calls.get_mut(index) {
                                record.id = outcome.tool_call.id.clone();
                                record.duration_ms = Some(duration);
                                record.status = if outcome.is_error {
                                    if context.signal.is_cancelled() {
                                        CodemodeNestedCallStatus::Cancelled
                                    } else {
                                        CodemodeNestedCallStatus::Error
                                    }
                                } else {
                                    CodemodeNestedCallStatus::Ok
                                };
                                record.error = error_text;
                            }
                        }
                        publish();
                        to_script_value(&tool_for_value, &outcome)
                    })
                }),
            }
        })
        .collect();

    // 等待 worker 的时间计入脚本的 deadline。
    // 准入失败也不 panic、不返回 Err：TS 把“脚本没跑起来”呈现为一次脚本失败（is_error 结果），
    // 文案按 abort、deadline 先到（<= 60s）、纯容量不足三种情况区分。
    let deadline = parsed.options.timeout_ms.unwrap_or(f64::INFINITY);
    let wait_started = Instant::now();
    let admission =
        reserve_codemode_worker(signal.as_ref(), deadline.min(CODEMODE_WORKER_WAIT_MS)).await;
    let waited_ms = wait_started.elapsed().as_secs_f64() * 1000.0;
    if admission != WorkerAdmission::Reserved {
        let waited = js_to_fixed(started_at.elapsed().as_secs_f64(), 1);
        let error = if admission == WorkerAdmission::Aborted {
            // CancellationToken 不携带 TS 的 `signal.reason`。Node 的裸 `abort()` 把 reason 设成
            // message 为 "This operation was aborted" 的 DOMException（instanceof Error），因此
            // `reason.message` 就是这条文本；kiana-codemode 的沙箱 abort 路径用同一个默认值。
            "Script aborted: This operation was aborted".to_string()
        } else if deadline <= CODEMODE_WORKER_WAIT_MS {
            format!(
                "Script timed out: Execution timed out after {} ms while waiting for a free codemode worker",
                kiana_json::js_num::format_js_number(deadline)
            )
        } else {
            format!(
                "No codemode worker became free within {} seconds. All {} workers, shared by every session in this process, are held by other scripts. Retry after they finish.",
                kiana_json::js_num::format_js_number(CODEMODE_WORKER_WAIT_MS / 1000.0),
                max_codemode_workers()
            )
        };
        let details = serde_json::to_value(snapshot_details(&calls)).unwrap_or(JsonValue::Null);
        return Ok(AgentToolResult {
            content: vec![
                text_block(format!(
                    "Script failed\nWall time {waited} seconds\nOutput:\n"
                )),
                text_block(format!(
                    "Script error:\n{error}\n\nThe script did not start."
                )),
            ],
            details,
            structured_content: None,
            usage: None,
            is_error: Some(true),
            terminate: None,
        });
    }
    // 预留覆盖本脚本启动的一个 worker；结果结算后才恢复的启动会发现它已释放、不再启动 worker。
    let reserved = Arc::new(AtomicBool::new(true));
    let acquire_worker = create_acquire_worker(Arc::clone(&reserved));
    let reservation = WorkerReservation {
        reserved: Arc::clone(&reserved),
    };

    let timeout_ms = if deadline.is_finite() {
        (deadline - waited_ms).round().max(1.0)
    } else {
        f64::INFINITY
    };
    let mut globals = create_discovery_globals(&callable, Arc::clone(&samples), &options);
    if options.models == Some(true)
        && let Some(ctx) = &ctx
    {
        let add_usage = {
            let model_usage = Arc::clone(&model_usage);
            move |usage: Usage| {
                let mut slot = model_usage
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                *slot = Some(match slot.take() {
                    Some(existing) => combine_usage(&existing, &usage),
                    None => usage,
                });
            }
        };
        let add_generated_images = {
            let generated_images = Arc::clone(&generated_images);
            move |count: usize| {
                generated_images.fetch_add(count, Ordering::SeqCst);
            }
        };
        globals.extend(create_model_globals(
            Arc::clone(ctx),
            tool_call_id.clone(),
            Arc::clone(&calls),
            Arc::clone(&publish),
            Arc::new(add_usage),
            Arc::new(add_generated_images),
        ));
    }

    let sandbox_options = CodemodeSandboxOptions {
        base: CodemodeSandboxBaseOptions {
            tools: Some(sandbox_tools),
            globals: Some(globals),
            timeout_ms: Some(timeout_ms),
            memory_limit_bytes: Some(CODEMODE_MEMORY_LIMIT_BYTES),
        },
        // 原生后端在 worker 启动时加载编译进二进制的同一份 quickjs.wasm（`loadQuickJSWasm`
        // 的默认路径），因此加载仍受 deadline 保护；与 TS 的 `wasm: loadQuickJSWasm(...)` 等价。
        wasm: None,
        worker_url: get_codemode_worker_specifier(),
        acquire_worker: Some(acquire_worker),
        on_worker_started: None,
    };
    let store = ctx
        .as_ref()
        .map(|ctx| read_codemode_store(&ctx.session_manager().get_branch(None)));
    // store 快照在启动前从当前分支重放。沙箱构造失败与 `execute` 的外层错误映射成 `Err`，
    // 脚本失败则是 `Ok(CodemodeResult::Err)`。`close()` 无条件执行，取消并结算在途执行；
    // worker 槽位由 sandbox 在 worker 退出时通过 release 回调归还。
    let execution: Result<CodemodeResult, String> = async {
        let sandbox = CodemodeSandbox::new(sandbox_options).map_err(|error| error.to_string())?;
        let result = sandbox
            .execute(
                &parsed.code,
                CodemodeExecuteOptions {
                    signal: signal.clone(),
                    timeout_ms: None,
                    store,
                },
            )
            .await;
        sandbox.close().await;
        result.map_err(|error| error.to_string())
    }
    .await;
    // 沙箱关闭后立即归还（或确认 worker 生命周期已接手）：与 TS 的 `finally` 同时机。
    drop(reservation);
    let result = match execution {
        Ok(result) => result,
        Err(error) => {
            return Err(AgentToolError {
                message: error,
                name: None,
                stack: None,
            });
        }
    };

    // 脚本结束时仍标记 running 的调用被取消（超时/abort 时结果可能还没回写），
    // 让 `details.calls` 不出现永远停在 running 的条目。
    for call in calls
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .iter_mut()
    {
        if call.status == CodemodeNestedCallStatus::Running {
            call.status = CodemodeNestedCallStatus::Cancelled;
        }
    }

    let mut script_output = match &result {
        CodemodeResult::Ok(ok) => ok.output.clone(),
        CodemodeResult::Err(err) => err.output.clone(),
    };
    if let CodemodeResult::Ok(ok) = &result {
        // 只有成功结果的 store 写入会持久化；`append_entry` 未配置（SDK 直连）时静默丢弃。
        let writes = &ok.store_writes;
        if (!writes.set.is_empty() || !writes.delete.is_empty())
            && let Some(append_entry) = &options.append_entry
        {
            append_entry(
                CODEMODE_STORE_ENTRY_TYPE.to_string(),
                CodemodeStoreEntryData {
                    set: writes.set.clone(),
                    delete: writes.delete.clone(),
                },
            )
            .await;
        }
        // kiana extension：返回值像 text() 一样追加。
        if let Some(value) = &ok.value {
            script_output.push(CodemodeOutputItem::text(value_text(value)));
        }
    }
    let mut items = format_output(&script_output);
    if let CodemodeResult::Err(err) = &result {
        // 失败脚本的部分输出保留在前，错误段（含调用清单）附在最后。这是带
        // `is_error: Some(true)` 的正常工具结果，不是函数的 `Err`。
        let calls_snapshot = calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        items.push(text_block(format!(
            "Script error:\n{}",
            format_error(err, &calls_snapshot)
        )));
    }
    let generated = generated_images.load(Ordering::SeqCst);
    // 脚本调用了 generateImages() 却一个图片块都没输出时，附一条提示让脚本用 image(block)
    // 展示；判断依据是当前 items 里是否存在 `UserContentBlock::Image`。
    if generated > 0
        && !items
            .iter()
            .any(|item| matches!(item, UserContentBlock::Image(_)))
    {
        let suffix = if generated == 1 { "" } else { "s" };
        items.push(text_block(format!(
            "Note: models.generateImages() returned {generated} image{suffix} that the script did not show. Show each image block of result.output with image(block)."
        )));
    }

    // 顺序固定（见文件头）：合并相邻文本 → 按 token 预算截断 → 落盘图片 → 再合并一次，
    // 让每张图片前的“已保存到 <path>”标签紧贴图片块。
    let max_output_tokens = parsed
        .options
        .max_output_tokens
        .unwrap_or(DEFAULT_MAX_OUTPUT_TOKENS);
    let (truncated_items, full_output_path) =
        truncate_output(join_adjacent_text(items), max_output_tokens)?;
    let output = join_adjacent_text(save_images(truncated_items)?);
    // 墙钟时间从进入本函数算起：包含等 worker、组装与脚本执行；保留一位小数，与 TS 的
    // `toFixed(1)` 一致。
    let wall_time = js_to_fixed(started_at.elapsed().as_secs_f64(), 1);
    let ok = result.ok();
    let header = format!(
        "{}\nWall time {wall_time} seconds\nOutput:\n",
        if ok {
            "Script completed"
        } else {
            "Script failed"
        }
    );
    let mut content = Vec::with_capacity(output.len() + 1);
    content.push(text_block(header));
    content.extend(output);
    let details = CodemodeToolDetails {
        calls: calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone(),
        full_output_path,
    };
    let usage = model_usage
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take();
    Ok(AgentToolResult {
        content,
        details: serde_json::to_value(details).unwrap_or(JsonValue::Null),
        structured_content: None,
        usage,
        is_error: (!ok).then_some(true),
        terminate: None,
    })
}

/// JS `Number.prototype.toFixed(1)`（性能时钟用）。
fn js_to_fixed(value: f64, digits: u32) -> String {
    kiana_ai::model_catalog::js_to_fixed(value, digits)
}

/// UTF-16 code unit 数（`string.length`）。
fn utf16_len(value: &str) -> usize {
    value.encode_utf16().count()
}

/// 按 UTF-16 code unit 截取（JS `slice`）；切在代理对中间时输出 U+FFFD。
fn utf16_slice(value: &str, start: usize, end: usize) -> String {
    let units: Vec<u16> = value.encode_utf16().collect();
    let start = start.min(units.len());
    let end = end.min(units.len()).max(start);
    String::from_utf16_lossy(&units[start..end])
}

/// 名字是否命中 namespace：名字或别名，按原样或按相同脚本标识符。
fn is_namespace_name(namespace: &kiana_ext_api::tool_types::ToolNamespace, query: &str) -> bool {
    let query_id = to_codemode_identifier(query);
    std::iter::once(&namespace.name)
        .chain(namespace.aliases.iter().flatten())
        .any(|name| name == query || to_codemode_identifier(name) == query_id)
}

/// `searchTools()`、`describeTool()`、`describeNamespace()`：对脚本的嵌套工具及其 namespace
/// 做排名搜索与查找。三个都是 `spread: true` 的全局函数，`execute` 收到的是参数数组。
///
/// - `searchTools(query, { limit?, namespace? })`：对
///   `create_tool_search_document` 生成的文档跑 BM25。`limit` 缺省
///   [`DEFAULT_TOOL_SEARCH_LIMIT`]，必须是正整数；空字符串 namespace 视为不过滤（TS 的
///   falsy 语义），非字符串报错。namespace 过滤依赖 `options.get_tool_namespace`，没有该
///   回调时带 namespace 的搜索匹配不到任何工具。
/// - `describeTool(name)`：按原工具名或脚本标识符精确查找，返回声明样本；未知名字与 TS 的
///   `undefined` 一样落到 `null`（冻结的 Value 契约表达不了 `undefined`）。
/// - `describeNamespace(name)`：名字或别名匹配后返回 `{ name, description?, instructions?,
///   tools }`，空的 `description`/`instructions` 省略；没有匹配返回 `null`。
///
/// 返回值都是 `Result<JsonValue, String>`：`Err` 在脚本里变成同文案的 `Error`。
fn create_discovery_globals(
    tools: &[AgentTool],
    samples: Arc<IndexMap<String, String>>,
    options: &CodemodeToolOptions,
) -> Vec<CodemodeTool> {
    let ranker = Bm25Ranker::new();
    let samples_for_entry = Arc::clone(&samples);
    let make_entry = move |name: &str| -> JsonValue {
        let mut entry = Map::new();
        entry.insert(
            "name".to_string(),
            JsonValue::String(to_codemode_identifier(name)),
        );
        entry.insert(
            "description".to_string(),
            JsonValue::String(samples_for_entry.get(name).cloned().unwrap_or_default()),
        );
        JsonValue::Object(entry)
    };
    let tools_for_search = tools.to_vec();
    let tools_for_describe = tools.to_vec();
    let tools_for_namespace = tools.to_vec();
    let options_for_search = options.clone();
    let options_for_namespace = options.clone();
    let ranker_for_search = ranker;
    // 三个全局共用同一份 `samples` 快照；`make_entry` 只暴露脚本标识符与声明文本。
    vec![
        CodemodeTool {
            name: "searchTools".to_string(),
            description: None,
            input_schema: None,
            output_schema: None,
            spread: Some(true),
            signature: None,
            execute: Arc::new(move |args: JsonValue, _context: CodemodeToolContext| {
                let tools = tools_for_search.clone();
                let options = options_for_search.clone();
                let ranker = ranker_for_search;
                let make_entry = make_entry.clone();
                Box::pin(async move {
                    let arguments = args.as_array().cloned().unwrap_or_default();
                    let Some(query) = arguments.first().and_then(JsonValue::as_str) else {
                        return Err("searchTools() expects a query string".to_string());
                    };
                    let search_options = arguments.get(1).and_then(JsonValue::as_object);
                    let limit = match search_options.and_then(|options| options.get("limit")) {
                        None | Some(JsonValue::Null) => DEFAULT_TOOL_SEARCH_LIMIT,
                        Some(JsonValue::Number(value)) => value.as_f64().unwrap_or(f64::NAN),
                        Some(_) => f64::NAN,
                    };
                    if !kiana_json::is_js_integer(limit) || limit <= 0.0 {
                        return Err("searchTools() limit must be a positive integer".to_string());
                    }
                    let namespace =
                        match search_options.and_then(|options| options.get("namespace")) {
                            None | Some(JsonValue::Null) => None,
                            // 空字符串在 TS 里是 falsy：`if (namespace && ...)` 不启用过滤。
                            Some(JsonValue::String(namespace)) if namespace.is_empty() => None,
                            Some(JsonValue::String(namespace)) => Some(namespace.clone()),
                            Some(_) => {
                                return Err("searchTools() namespace must be a string".to_string());
                            }
                        };
                    let mut documents = Vec::new();
                    for tool in &tools {
                        let tool_namespace = options
                            .get_tool_namespace
                            .as_ref()
                            .and_then(|get_tool_namespace| get_tool_namespace(&tool.name));
                        if let Some(namespace) = &namespace {
                            let matches = tool_namespace.as_ref().is_some_and(|tool_namespace| {
                                is_namespace_name(tool_namespace, namespace)
                            });
                            if !matches {
                                continue;
                            }
                        }
                        documents.push(create_tool_search_document(tool, tool_namespace.as_ref()));
                    }
                    let matches = ranker.rank(query, &documents, limit);
                    Ok(JsonValue::Array(
                        matches
                            .iter()
                            .map(|matched| make_entry(&matched.name))
                            .collect(),
                    ))
                })
            }),
        },
        CodemodeTool {
            name: "describeTool".to_string(),
            description: None,
            input_schema: None,
            output_schema: None,
            spread: Some(true),
            signature: None,
            execute: Arc::new(move |args: JsonValue, _context: CodemodeToolContext| {
                let tools = tools_for_describe.clone();
                let samples = Arc::clone(&samples);
                Box::pin(async move {
                    let arguments = args.as_array().cloned().unwrap_or_default();
                    let Some(name) = arguments.first().and_then(JsonValue::as_str) else {
                        return Err("describeTool() expects a tool name".to_string());
                    };
                    let tool = tools.iter().find(|tool| {
                        tool.name == name || to_codemode_identifier(&tool.name) == name
                    });
                    // TS 对未知名字返回 `undefined`；冻结的 `CodemodeToolExecute` 只有
                    // `serde_json::Value`，无法表达 undefined，这里只能给 `null`
                    // （脚本里 `=== undefined` 因此为假）。见 kiana-codemode 的类型文档。
                    Ok(match tool {
                        Some(tool) => samples
                            .get(&tool.name)
                            .cloned()
                            .map_or(JsonValue::Null, JsonValue::String),
                        None => JsonValue::Null,
                    })
                })
            }),
        },
        CodemodeTool {
            name: "describeNamespace".to_string(),
            description: None,
            input_schema: None,
            output_schema: None,
            spread: Some(true),
            signature: None,
            execute: Arc::new(move |args: JsonValue, _context: CodemodeToolContext| {
                let tools = tools_for_namespace.clone();
                let options = options_for_namespace.clone();
                Box::pin(async move {
                    let arguments = args.as_array().cloned().unwrap_or_default();
                    let Some(name) = arguments.first().and_then(JsonValue::as_str) else {
                        return Err("describeNamespace() expects a namespace name".to_string());
                    };
                    let mut found: Option<kiana_ext_api::tool_types::ToolNamespace> = None;
                    let mut names = Vec::new();
                    for tool in &tools {
                        let tool_namespace = options
                            .get_tool_namespace
                            .as_ref()
                            .and_then(|get_tool_namespace| get_tool_namespace(&tool.name));
                        let Some(tool_namespace) = tool_namespace else {
                            continue;
                        };
                        if !is_namespace_name(&tool_namespace, name) {
                            continue;
                        }
                        if found.is_none() {
                            found = Some(tool_namespace);
                        }
                        names.push(JsonValue::String(to_codemode_identifier(&tool.name)));
                    }
                    let Some(namespace) = found else {
                        // TS 的 `undefined` 在冻结的 Value 契约里只能落到 `null`。
                        return Ok(JsonValue::Null);
                    };
                    let mut object = Map::new();
                    object.insert("name".to_string(), JsonValue::String(namespace.name));
                    if let Some(description) =
                        namespace.description.filter(|value| !value.is_empty())
                    {
                        object.insert("description".to_string(), JsonValue::String(description));
                    }
                    if let Some(instructions) =
                        namespace.instructions.filter(|value| !value.is_empty())
                    {
                        object.insert("instructions".to_string(), JsonValue::String(instructions));
                    }
                    object.insert("tools".to_string(), JsonValue::Array(names));
                    Ok(JsonValue::Object(object))
                })
            }),
        },
    ]
}

/// `models.*`：脚本可访问的模型目录、分类器与图像生成。
///
/// 注册五个全局：`getModelsOfType`、`getAvailableOfType`、`getModelOfType`、`classify`、
/// `generateImages`。只有 `classify`/`generateImages` 会走 [`ModelCallLimiter`]（每个脚本
/// 一个，上限 [`MAX_CONCURRENT_MODEL_CALLS`]）并登记嵌套调用记录；三个目录查询不登记记录，
/// 其中 `getAvailableOfType` 会带着脚本的取消信号调用模型注册表的异步可用性检查。
///
/// 模型信息交给脚本前由 [`to_model_info`] 去掉 `headers`（可能含凭据）。`classify` /
/// `generateImages` 的返回值是 provider 结果序列化后的 JSON，错误通过结果里的
/// `stop_reason`/`error_message` 表达，而不是 `Err`；模型解析失败（未知 provider/id 或类型
/// 不匹配）与参数形状错误才返回 `Err`。
fn create_model_globals(
    ctx: Arc<dyn ExtensionToolContextApi>,
    tool_call_id: String,
    calls: Arc<Mutex<Vec<CodemodeNestedCall>>>,
    publish: Arc<dyn Fn() + Send + Sync>,
    add_usage: Arc<dyn Fn(Usage) + Send + Sync>,
    add_generated_images: Arc<dyn Fn(usize) + Send + Sync>,
) -> Vec<CodemodeTool> {
    let limiter = ModelCallLimiter::new(MAX_CONCURRENT_MODEL_CALLS);
    let call_count = Arc::new(AtomicUsize::new(0));

    // 每次模型调用先追加一条 Running 记录（id 为 `<tool_call_id>/<name>/<count>`），完成时由
    // `finish_record` 回写状态、耗时、错误预览、单次 cost 并累计整体 usage。
    let push_record = {
        let calls = Arc::clone(&calls);
        let publish = Arc::clone(&publish);
        let tool_call_id = tool_call_id.clone();
        let call_count = Arc::clone(&call_count);
        move |name: &str, args: String| -> usize {
            let count = call_count.fetch_add(1, Ordering::SeqCst) + 1;
            let record = CodemodeNestedCall {
                id: format!("{tool_call_id}/{name}/{count}"),
                name: name.to_string(),
                args,
                status: CodemodeNestedCallStatus::Running,
                duration_ms: None,
                error: None,
                cost: None,
            };
            let index = {
                let mut calls = calls
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                calls.push(record);
                calls.len() - 1
            };
            publish();
            index
        }
    };

    /// 解析脚本给的模型（只按 provider + id），解析失败时的错误文本。
    ///
    /// 第一个参数是模型信息对象（通常来自 `models.getModelOfType`）；这里只读 `provider` 与
    /// `id`，忽略对象里的其它字段。解析不到时区分两种文案：该 provider/id 存在但类型不同
    /// （“是 X 模型，不是 Y 模型”），或完全未知（附 `getAvailableOfType` 提示）。传 `undefined`
    /// 时额外提示 `getModelOfType()` 对未知 provider/id 返回 undefined。
    fn resolve_model(
        name: &str,
        model_type: ModelType,
        args: &[JsonValue],
        ctx: &Arc<dyn ExtensionToolContextApi>,
    ) -> Result<Model, String> {
        let type_name = model_type_str(model_type);
        let list_hint = format!(
            "List the {type_name} models you can use with models.getAvailableOfType(\"{type_name}\")."
        );
        let model = args.first();
        let given = model.and_then(JsonValue::as_object);
        let provider = given
            .and_then(|given| given.get("provider"))
            .and_then(JsonValue::as_str);
        let id = given
            .and_then(|given| given.get("id"))
            .and_then(JsonValue::as_str);
        let (Some(provider), Some(id)) = (provider, id) else {
            let undefined_hint = if matches!(model, None | Some(JsonValue::Null)) {
                " models.getModelOfType() returns undefined for an unknown provider or id."
            } else {
                ""
            };
            return Err(format!(
                "{name}() expects {} model as its first argument, got {}.{undefined_hint} {list_hint}",
                with_article(type_name),
                describe_value(model)
            ));
        };
        let reference = format!("{provider}/{id}");
        let models = ctx.model_registry();
        if let Some(resolved) = models.get_model_of_type(model_type, provider, id) {
            return Ok(resolved);
        }
        let actual_type = [ModelType::Chat, ModelType::Image, ModelType::Classifier]
            .into_iter()
            .find(|other| {
                *other != model_type && models.get_model_of_type(*other, provider, id).is_some()
            });
        Err(match actual_type {
            Some(actual) => format!(
                "\"{reference}\" is {} model, not {} model. {list_hint}",
                with_article(model_type_str(actual)),
                with_article(type_name)
            ),
            None => format!("Unknown {type_name} model \"{reference}\". {list_hint}"),
        })
    }

    fn model_type_str(model_type: ModelType) -> &'static str {
        match model_type {
            ModelType::Chat => "chat",
            ModelType::Image => "image",
            ModelType::Classifier => "classifier",
        }
    }

    // 收尾函数由 classify/generateImages 共用：stop reason 映射为 Ok/Cancelled/Error，只有
    // 非空 error_message 才写 error（截到 500 字符）；usage 存在时记录单次 cost 并汇总到
    // 整次执行的 usage。
    let finish_record = {
        let calls = Arc::clone(&calls);
        let publish = Arc::clone(&publish);
        let add_usage = Arc::clone(&add_usage);
        move |index: usize, started: Instant, result: ModelCallResult| {
            {
                let mut calls = calls
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if let Some(record) = calls.get_mut(index) {
                    record.duration_ms = Some(started.elapsed().as_secs_f64() * 1000.0);
                    record.status = match result.stop_reason {
                        StopKind::Stop => CodemodeNestedCallStatus::Ok,
                        StopKind::Aborted => CodemodeNestedCallStatus::Cancelled,
                        StopKind::Error => CodemodeNestedCallStatus::Error,
                    };
                    if let Some(error) = result
                        .error_message
                        .as_deref()
                        .filter(|error| !error.is_empty())
                    {
                        record.error = Some(truncate_text(error, ERROR_PREVIEW_CHARS));
                    }
                    if let Some(usage) = &result.usage {
                        record.cost = Some(usage.cost.total);
                        add_usage(usage.clone());
                    }
                }
            }
            publish();
        }
    };
    let ctx_for_models = Arc::clone(&ctx);
    let ctx_for_available = Arc::clone(&ctx);
    let ctx_for_get_model = Arc::clone(&ctx);
    let ctx_for_classify = Arc::clone(&ctx);
    let ctx_for_images = Arc::clone(&ctx);
    let push_for_classify = push_record.clone();
    let push_for_images = push_record.clone();
    let finish_for_classify = finish_record.clone();
    let finish_for_images = finish_record.clone();
    let limiter_for_classify = Arc::clone(&limiter);
    let limiter_for_images = Arc::clone(&limiter);

    vec![
        CodemodeTool {
            name: "models.getModelsOfType".to_string(),
            description: None,
            input_schema: None,
            output_schema: None,
            spread: Some(true),
            signature: None,
            execute: Arc::new(move |args: JsonValue, _context: CodemodeToolContext| {
                let ctx = Arc::clone(&ctx_for_models);
                Box::pin(async move {
                    let arguments = args.as_array().cloned().unwrap_or_default();
                    let model_type = to_model_type(arguments.first())?;
                    let provider = to_provider(arguments.get(1))?;
                    let models = ctx.model_registry();
                    Ok(JsonValue::Array(
                        models
                            .get_models_of_type(model_type, provider.as_deref())
                            .iter()
                            .map(to_model_info)
                            .collect(),
                    ))
                })
            }),
        },
        CodemodeTool {
            name: "models.getAvailableOfType".to_string(),
            description: None,
            input_schema: None,
            output_schema: None,
            spread: Some(true),
            signature: None,
            execute: Arc::new(move |args: JsonValue, context: CodemodeToolContext| {
                let ctx = Arc::clone(&ctx_for_available);
                Box::pin(async move {
                    let arguments = args.as_array().cloned().unwrap_or_default();
                    let model_type = to_model_type(arguments.first())?;
                    let provider = to_provider(arguments.get(1))?;
                    let models = ctx.model_registry();
                    let available = models
                        .get_available_of_type(
                            model_type,
                            provider.as_deref(),
                            kiana_ai::auth::types::AuthOperationOptions {
                                signal: Some(kiana_ai::utils::abort::AbortScope::from(
                                    context.signal.clone(),
                                )),
                            },
                        )
                        .await
                        .map_err(|error| error.to_string())?;
                    Ok(JsonValue::Array(
                        available.iter().map(to_model_info).collect(),
                    ))
                })
            }),
        },
        CodemodeTool {
            name: "models.getModelOfType".to_string(),
            description: None,
            input_schema: None,
            output_schema: None,
            spread: Some(true),
            signature: None,
            execute: Arc::new(move |args: JsonValue, _context: CodemodeToolContext| {
                let ctx = Arc::clone(&ctx_for_get_model);
                Box::pin(async move {
                    let arguments = args.as_array().cloned().unwrap_or_default();
                    let provider = arguments.get(1).and_then(JsonValue::as_str);
                    let id = arguments.get(2).and_then(JsonValue::as_str);
                    if provider.is_none() || id.is_none() {
                        let values: Vec<String> = arguments
                            .iter()
                            .map(|value| describe_value(Some(value)))
                            .collect();
                        return Err(format!(
                            "models.getModelOfType(type, provider, id) expects three strings, got ({}). The provider and the id are separate arguments, for example models.getModelOfType(\"classifier\", \"typesafe\", \"jev-latest\").",
                            values.join(", ")
                        ));
                    }
                    let model_type = to_model_type(arguments.first())?;
                    let models = ctx.model_registry();
                    Ok(
                        match models.get_model_of_type(
                            model_type,
                            provider.unwrap_or_default(),
                            id.unwrap_or_default(),
                        ) {
                            Some(model) => to_model_info(&model),
                            // TS 的 `undefined` 在冻结的 Value 契约里只能落到 `null`。
                            None => JsonValue::Null,
                        },
                    )
                })
            }),
        },
        CodemodeTool {
            name: "models.classify".to_string(),
            description: None,
            input_schema: None,
            output_schema: None,
            spread: Some(true),
            signature: None,
            execute: Arc::new(move |args: JsonValue, context: CodemodeToolContext| {
                let ctx = Arc::clone(&ctx_for_classify);
                let push_record = push_for_classify.clone();
                let finish_record = finish_for_classify.clone();
                let limiter = Arc::clone(&limiter_for_classify);
                Box::pin(async move {
                    let arguments = args.as_array().cloned().unwrap_or_default();
                    let resolved =
                        resolve_model("models.classify", ModelType::Classifier, &arguments, &ctx)?;
                    let checked = check_classifier_context(arguments.get(1))?;
                    let index = push_record(
                        "models.classify",
                        format!("{}/{}", resolved.provider, resolved.id),
                    );
                    let started = Instant::now();
                    let models = ctx.model_registry();
                    let result = limiter
                        .run(models.classify(
                            &resolved,
                            &checked,
                            ModelsClassifierOptions {
                                base: ClassifierOptions {
                                    signal: Some(context.signal.clone()),
                                    ..Default::default()
                                },
                                transforms: Default::default(),
                                signal_reason: None,
                            },
                        ))
                        .await;
                    finish_record(
                        index,
                        started,
                        ModelCallResult {
                            stop_reason: result.stop_reason.into(),
                            error_message: result.error_message.clone(),
                            usage: result.usage.clone(),
                        },
                    );
                    Ok(serde_json::to_value(&result).unwrap_or(JsonValue::Null))
                })
            }),
        },
        CodemodeTool {
            name: "models.generateImages".to_string(),
            description: None,
            input_schema: None,
            output_schema: None,
            spread: Some(true),
            signature: None,
            execute: Arc::new(move |args: JsonValue, context: CodemodeToolContext| {
                let ctx = Arc::clone(&ctx_for_images);
                let push_record = push_for_images.clone();
                let finish_record = finish_for_images.clone();
                let limiter = Arc::clone(&limiter_for_images);
                let add_generated_images = Arc::clone(&add_generated_images);
                Box::pin(async move {
                    let arguments = args.as_array().cloned().unwrap_or_default();
                    let resolved =
                        resolve_model("models.generateImages", ModelType::Image, &arguments, &ctx)?;
                    let checked = check_images_context(arguments.get(1))?;
                    let index = push_record(
                        "models.generateImages",
                        format!("{}/{}", resolved.provider, resolved.id),
                    );
                    let started = Instant::now();
                    let models = ctx.model_registry();
                    let result = limiter
                        .run(models.generate_images(
                            &resolved,
                            &checked,
                            ModelsImagesOptions {
                                base: ImagesOptions {
                                    signal: Some(context.signal.clone()),
                                    ..Default::default()
                                },
                                transforms: Default::default(),
                                signal_reason: None,
                            },
                        ))
                        .await;
                    add_generated_images(
                        result
                            .output
                            .iter()
                            .filter(|block| matches!(block, UserContentBlock::Image(_)))
                            .count(),
                    );
                    finish_record(
                        index,
                        started,
                        ModelCallResult {
                            stop_reason: result.stop_reason.into(),
                            error_message: result.error_message.clone(),
                            usage: result.usage.clone(),
                        },
                    );
                    Ok(serde_json::to_value(&result).unwrap_or(JsonValue::Null))
                })
            }),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use kiana_codemode::types::CodemodeResultErr;
    use kiana_codemode::{CodemodeError, CodemodeErrorKind, CodemodeStoreWrites};
    use serde_json::json;

    fn plain_text(item: UserContentBlock) -> String {
        match item {
            UserContentBlock::Text(text) => text.text,
            UserContentBlock::Image(_) => "<image>".to_string(),
        }
    }

    #[test]
    fn formats_multiple_text_items_and_console_output() {
        let output = vec![
            CodemodeOutputItem::text("a"),
            CodemodeOutputItem::console("c"),
            CodemodeOutputItem::text("b"),
            CodemodeOutputItem::image("data", "image/png"),
        ];
        let items = format_output(&output);
        assert_eq!(plain_text(items[0].clone()), "==> text 1/2 <==\na");
        assert_eq!(plain_text(items[1].clone()), "==> text 2/2 <==\nb");
        assert_eq!(plain_text(items[2].clone()), "<image>");
        assert_eq!(
            plain_text(items[3].clone()),
            "<console_output>\nc\n</console_output>"
        );
        let joined = join_adjacent_text(items);
        assert_eq!(
            plain_text(joined[0].clone()),
            "==> text 1/2 <==\na\n==> text 2/2 <==\nb"
        );
        assert_eq!(plain_text(joined[1].clone()), "<image>");
        assert_eq!(
            plain_text(joined[2].clone()),
            "<console_output>\nc\n</console_output>"
        );
    }

    #[test]
    fn joins_adjacent_text_with_a_single_newline_boundary() {
        let items = vec![
            text_block("a"),
            text_block("b\n"),
            text_block(""),
            text_block("c"),
        ];
        let joined = join_adjacent_text(items);
        assert_eq!(joined.len(), 1);
        assert_eq!(plain_text(joined[0].clone()), "a\nb\nc");
    }

    #[test]
    fn previews_arguments_and_values_like_javascript() {
        assert_eq!(preview_args(&JsonValue::Null), "");
        assert_eq!(preview_args(&json!({"a": 1})), "{\"a\":1}");
        assert_eq!(truncate_text("abcdef", 5), "ab...");
        assert_eq!(truncate_text("abcde", 5), "abcde");
        assert_eq!(value_text(&json!("x")), "x");
        assert_eq!(
            value_text(&json!({"a": [1, true, null]})),
            "{\"a\":[1,true,null]}"
        );
    }

    #[test]
    fn describes_script_values_in_error_messages() {
        assert_eq!(describe_value(None), "undefined");
        assert_eq!(describe_value(Some(&JsonValue::Null)), "null");
        assert_eq!(describe_value(Some(&json!([]))), "an empty array");
        assert_eq!(describe_value(Some(&json!([1]))), "an array");
        assert_eq!(describe_value(Some(&json!({}))), "{}");
        assert_eq!(describe_value(Some(&json!({"a": 1, "b": 2}))), "{ a, b }");
        assert_eq!(
            describe_value(Some(&json!({"a":1,"b":2,"c":3,"d":4,"e":5,"f":6,"g":7}))),
            "{ a, b, c, d, e, f, ... }"
        );
        assert_eq!(describe_value(Some(&json!("s"))), "a string");
        assert_eq!(describe_value(Some(&json!(1))), "a number");
        assert_eq!(describe_value(Some(&json!(true))), "a boolean");
    }

    #[test]
    fn formats_script_failures_with_call_summaries() {
        let call = CodemodeNestedCall {
            id: "c/1".to_string(),
            name: "read".to_string(),
            args: "{}".to_string(),
            status: CodemodeNestedCallStatus::Ok,
            duration_ms: None,
            error: None,
            cost: None,
        };
        let timeout = CodemodeResultErr::new(
            CodemodeError::new(CodemodeErrorKind::Timeout, "deadline"),
            Vec::new(),
            Vec::new(),
            None,
        );
        assert_eq!(
            format_error(&timeout, &[]),
            "Script timed out: deadline\n\nNo tool calls were made."
        );
        let aborted = CodemodeResultErr::new(
            CodemodeError::new(CodemodeErrorKind::Aborted, "stop"),
            Vec::new(),
            Vec::new(),
            None,
        );
        assert_eq!(
            format_error(&aborted, std::slice::from_ref(&call)),
            "Script aborted: stop\n\nTool calls made before the failure (they are not undone): read (ok)"
        );
        let mut script = CodemodeError::new(CodemodeErrorKind::Script, "boom");
        script.name = Some("TypeError".to_string());
        let script_result = CodemodeResultErr::new(script, Vec::new(), Vec::new(), None);
        assert_eq!(
            format_error(&script_result, &[]),
            "TypeError: boom\n\nNo tool calls were made."
        );
        let mut with_stack = CodemodeError::new(CodemodeErrorKind::Script, "boom");
        with_stack.stack = Some("TypeError: boom\n    at <anonymous>:1:1".to_string());
        let stacked = CodemodeResultErr::new(with_stack, Vec::new(), Vec::new(), None);
        assert_eq!(
            format_error(&stacked, &[]),
            "TypeError: boom\n    at <anonymous>:1:1\n\nNo tool calls were made."
        );
        let sandbox = CodemodeResultErr::new(
            CodemodeError::new(CodemodeErrorKind::Sandbox, "trap"),
            Vec::new(),
            Vec::new(),
            None,
        );
        assert_eq!(
            format_error(&sandbox, &[]),
            "Script sandbox failed: trap\n\nNo tool calls were made."
        );
        let _ = CodemodeStoreWrites::default();
    }

    #[tokio::test]
    async fn missing_or_non_string_code_raises_the_typescript_type_error() {
        // 期望文案来自 Node 26 的 `parseCodemodeSource(input.code)`（见评审回归）。
        let cases = [
            (
                json!({}),
                "Cannot read properties of undefined (reading 'trim')",
            ),
            (
                json!({"code": null}),
                "Cannot read properties of null (reading 'trim')",
            ),
            (json!({"code": 42}), "input.trim is not a function"),
            (json!({"code": ["a"]}), "input.trim is not a function"),
            (
                json!(null),
                "Cannot read properties of null (reading 'code')",
            ),
            (
                json!("source"),
                "Cannot read properties of undefined (reading 'trim')",
            ),
            (
                json!([1]),
                "Cannot read properties of undefined (reading 'trim')",
            ),
        ];
        for (input, message) in cases {
            let error = execute_codemode(
                "call".to_string(),
                input.clone(),
                None,
                None,
                None,
                CodemodeToolOptions::default(),
            )
            .await
            .expect_err("invalid code is rejected");
            assert_eq!(error.message, message, "input: {input}");
            assert_eq!(error.name.as_deref(), Some("TypeError"), "input: {input}");
        }
    }

    #[tokio::test]
    async fn truncates_the_combined_text_and_keeps_images_after_it() {
        let items = vec![
            text_block("head"),
            text_block("x".repeat(100)),
            image_block("data", "image/png"),
        ];
        let (truncated, full_output_path) = truncate_output(items, 5.0).expect("truncate");
        // 预算 5 token = 20 字符：head 10、tail 10。
        assert_eq!(truncated.len(), 2);
        let text = plain_text(truncated[0].clone());
        assert!(
            text.starts_with(
                "Warning: truncated output (original token count: 27)\nTotal output lines: 2\n\nhead\nxxxxx…"
            ),
            "{text}"
        );
        assert!(text.contains(" tokens truncated…"));
        assert!(text.contains("[Full output: "));
        assert!(text.contains(" (read with offset/limit)]"));
        assert_eq!(plain_text(truncated[1].clone()), "<image>");
        assert!(full_output_path.is_some());
    }

    #[test]
    fn classifier_context_errors_use_the_exact_shape_message() {
        let error = check_classifier_context(Some(&json!({"state": {}}))).unwrap_err();
        assert_eq!(
            error,
            format!(
                "models.classify() context.questions must map question IDs to questions, got undefined. Expected context: {CLASSIFIER_CONTEXT_SHAPE}. See \"Classify\" in {}.",
                *CODEMODE_DOCS_PATH
            )
        );
        let error = check_classifier_context(Some(&json!({
            "state": {},
            "questions": {"q": {"type": "bool", "instructions": "x", "criteria": {"true": "y"}}}
        })))
        .unwrap_err();
        assert!(error.starts_with(
            "models.classify() context.questions.q is a \"bool\" question, so criteria must be { true: string, false: string }."
        ));
        let error = check_classifier_context(Some(&json!({
            "state": {},
            "questions": {"q": {"type": "rank", "instructions": "x", "criteria": []}}
        })))
        .unwrap_err();
        assert!(error.starts_with(
            "models.classify() context.questions.q.type must be \"choice\", \"score\", or \"bool\", got \"rank\"."
        ));
        let parsed = check_classifier_context(Some(&json!({
            "state": {"a": 1},
            "questions": {"q": {"type": "choice", "instructions": "x", "criteria": {"yes": "y"}}}
        })))
        .expect("valid classifier context");
        assert!(parsed.images.is_none());
        assert_eq!(parsed.questions.len(), 1);
    }

    #[test]
    fn images_context_errors_use_the_exact_shape_message() {
        let error = check_images_context(Some(&json!({"input": []}))).unwrap_err();
        assert_eq!(
            error,
            format!(
                "models.generateImages() context.input must be a non-empty array of blocks, got an empty array. Expected context: {{ input: [{{ type: \"text\", text: <prompt> }}, ...optional {{ type: \"image\", data: <base64>, mimeType }} references] }}. See \"Generate images\" in {}.",
                *CODEMODE_DOCS_PATH
            )
        );
        let error = check_images_context(Some(&json!({"input": [{"type": "text"}]}))).unwrap_err();
        assert!(error.starts_with(
            "models.generateImages() context.input[0] must be a text or image block, got { type }."
        ));
        let parsed = check_images_context(Some(&json!({
            "input": [{"type": "text", "text": "draw"}, {"type": "image", "data": "AA==", "mimeType": "image/png"}]
        })))
        .expect("valid images context");
        assert_eq!(parsed.input.len(), 2);
    }

    /// #10748：槽位在 `acquireWorker()` 之后归 worker 生命周期，脚本结算（预留卫兵被丢弃）
    /// 不得归还；只有 worker 真正退出时的 release 才放行下一个等待者。
    ///
    /// TS 的 admission 测试靠 mock `Worker.prototype.terminate` 制造「结果已结算、worker 尚未
    /// 退出」的窗口；Rust 的 worker 线程不可 mock，且实测总是在结果送达前就退出，所以这里
    /// 直接对 execute_codemode 使用的同一份 `acquireWorker` 闭包与预留卫兵断言记账语义。
    #[test]
    fn a_settled_script_keeps_its_slot_until_the_worker_exits() {
        static LOCK: Mutex<()> = Mutex::new(());
        let _guard = LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        ACTIVE_CODEMODE_WORKERS.store(0, Ordering::SeqCst);

        // 模拟一次成功准入：槽位已记在本脚本账上。
        ACTIVE_CODEMODE_WORKERS.fetch_add(1, Ordering::SeqCst);
        let reserved = Arc::new(AtomicBool::new(true));
        let acquire = create_acquire_worker(Arc::clone(&reserved));

        // worker 构造前 acquire：所有权转交给 worker 生命周期，并拿到退出时的 release。
        let release = acquire();
        assert!(!reserved.load(Ordering::SeqCst));

        // 脚本结算：execute_codemode 丢弃预留卫兵。槽位不得因此归还。
        drop(WorkerReservation {
            reserved: Arc::clone(&reserved),
        });
        assert_eq!(active_codemode_workers(), 1);

        // 在结果结算之后才恢复的启动：TS 同步抛 "The script already settled"。
        let late = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| acquire()));
        assert!(late.is_err(), "a second acquire must panic");
        assert_eq!(active_codemode_workers(), 1);

        // worker 真正退出（kiana-codemode 的 exit 事件）才释放槽位。
        release();
        assert_eq!(active_codemode_workers(), 0);
    }

    /// 评审回归：`Buffer.from(data, "base64")` 是 V8 的宽松解码（golden 值来自 Node 26）。
    #[test]
    fn base64_decoding_matches_node_buffer() {
        let cases: &[(&str, &[u8])] = &[
            ("AA-_", &[0x00, 0x0f, 0xbf]),
            ("QUJ=", &[0x41, 0x42]),
            ("QUJDR", &[0x41, 0x42, 0x43]),
            ("QUJD", &[0x41, 0x42, 0x43]),
            ("AQ==", &[0x01]),
            ("A", &[]),
            ("!!!", &[]),
            ("AQ==QUJD", &[0x01]),
            ("Q=UJD", &[]),
            ("AA==AA==", &[0x00]),
            ("QU JD\nRA==", &[0x41, 0x42, 0x43, 0x44]),
            ("-_-_", &[0xfb, 0xff, 0xbf]),
            ("A-A_", &[0x03, 0xe0, 0x3f]),
            ("QUJD$RA==", &[0x41, 0x42, 0x43, 0x44]),
            // V8 按 UTF-16 code unit 的低字节查表：`Ł`(U+0141) 是 `A`，代理项 D83D 是 `=`。
            ("QUJŁ", &[0x41, 0x42, 0x40]),
            ("ŁQUJD", &[0x01, 0x05, 0x09]),
            ("šššš", &[0x69, 0xa6, 0x9a]),
            ("😀QUJD", &[]),
            ("QUJD😀", &[0x41, 0x42, 0x43]),
        ];
        for (input, expected) in cases {
            assert_eq!(node_base64_decode(input), *expected, "{input:?}");
        }
    }

    /// 评审回归：保存失败的文案是 Node 的 `error.message` 形状，不是 Rust 的 Display。
    #[cfg(unix)]
    #[test]
    fn output_write_errors_use_node_messages() {
        let error = std::io::Error::from_raw_os_error(2);
        let message = node_output_file_error(&error, "kiana-codemode", ".txt");
        assert!(
            regex::Regex::new(
                r"^ENOENT: no such file or directory, open '.*kiana-codemode-[0-9a-f]{16}\.txt'$"
            )
            .expect("regex")
            .is_match(&message),
            "{message}"
        );
        let error = std::io::Error::from_raw_os_error(36);
        assert!(
            node_output_file_error(&error, "kiana-codemode", ".png")
                .starts_with("ENAMETOOLONG: name too long, open '"),
            "{}",
            node_output_file_error(&error, "kiana-codemode", ".png")
        );
        // 非 errno 错误保留自己的消息（未知错误的兜底）。
        assert_eq!(
            node_output_file_error(&std::io::Error::other("boom"), "kiana-codemode", ".txt"),
            "boom"
        );
    }

    /// 回归：等待 permit 的 future 被取消（脚本中止/超时）后，槽位必须归还。
    ///
    /// 旧实现用自建 oneshot 队列，取消后 sender 留在队列里，`release` 弹出它、`send`
    /// 失败且不递减 `active`；每个取消的等待者都会永久吃掉一个槽位，`limit` 次之后该
    /// 脚本的所有 `models.*` 调用都会挂起。
    #[tokio::test(flavor = "current_thread")]
    async fn cancelled_acquire_returns_its_slot() {
        let limiter = ModelCallLimiter::new(1);

        // 占住唯一的槽位。
        let (release_holder, holder_released) = tokio::sync::oneshot::channel::<()>();
        let holder = {
            let limiter = Arc::clone(&limiter);
            tokio::spawn(async move {
                limiter
                    .run(async {
                        let _ = holder_released.await;
                    })
                    .await
            })
        };

        // 让三个等待者排队后取消它们（相当于脚本被中止）。
        for _ in 0..3 {
            let limiter = Arc::clone(&limiter);
            let waiter =
                tokio::spawn(async move { limiter.run(std::future::pending::<()>()).await });
            // 给任务一次机会真正进入 `acquire()`。
            tokio::task::yield_now().await;
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            waiter.abort();
            let _ = waiter.await;
        }

        // 释放槽位；若取消的等待者泄漏了容量，后面的 `run` 会永远等不到 permit。
        let _ = release_holder.send(());
        holder.await.expect("holder finishes");
        tokio::time::timeout(std::time::Duration::from_secs(2), limiter.run(async {}))
            .await
            .expect("the slot must be available after cancelled waiters are dropped");
    }

    /// 回归：持有 permit 的 `run` future 被取消时，同样要归还槽位。
    #[tokio::test(flavor = "current_thread")]
    async fn cancelling_a_running_call_returns_its_slot() {
        let limiter = ModelCallLimiter::new(1);
        let running = {
            let limiter = Arc::clone(&limiter);
            tokio::spawn(async move { limiter.run(std::future::pending::<()>()).await })
        };
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        running.abort();
        let _ = running.await;
        tokio::time::timeout(std::time::Duration::from_secs(2), limiter.run(async {}))
            .await
            .expect("the slot must be released when the run future is dropped");
    }
}
