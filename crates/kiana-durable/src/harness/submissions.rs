//! `harness/submissions.ts`（277 行）：提交的接纳、等待与撤回。
//!
//! TS 的 `Submissions` 持有一个 `SessionImpl` 与一组 waiter。Rust 侧同构：
//! - `submit()` 在 `kiana.durable.submit` span 里**一次**提交完成接纳（[`admit_submission`]）；
//! - `wait()` 在变更线上检查记录并登记 waiter，避免结算提交落在两步之间；
//! - `abort()` 在 `commitWith` 的事务里撤回 queued 提交并移除 inbox 项。
//!
//! 与 TS 的差异（交付说明）：TS 的 `commitWith` 暴露具体 `Transaction`（带 `submission(id)`）。
//! 这里 `admitSubmission` 只接收 `Tx`，因此按 id 取提交的实现留在事务内部；
//! `start_run` 经由 `Tx::create_task_with` 的 `submissionIds` 建立任务 trace/link 的部分与 TS
//! 一致。
//!
//! # 与 Session 变更线的关系
//! 接纳、状态写入与撤回都在 Session 的变更线（`SessionImpl::commit_with`）上执行，所以
//! “读记录 → 判断 → 写状态”不会被别的提交插队；`wait()` 把“检查是否已结算 + 登记 waiter”
//! 也放在同一条线上的只读任务里（`read_on_line`），登记完成后再离开变更线等待，避免结算提交
//! 落在两步之间导致永远等不到。
//!
//! # 监听器与等待者
//! 构造时在 Session 上注册两个内部监听器：
//! - 提交监听器只处理 `ObservedCommit::Stored`（内存延迟提交只改文档，不会改提交表），遇到
//!   `status` 为 `done`/`unanswered` 的提交记录就 `Waiters::resolve` 对应 ID 的等待者；
//! - 关闭监听器把 `closed` 置真并 `reject_all`，所有已登记等待立即以 `closed_error` 结束。
//!
//! `Waiters` 按提交 ID 保存 oneshot 发送端；`Waiters::add` 把接收端交给等待方，每个等待只会
//! 结算一次；调用方上下文取消时，由 `Waiters::add` 注册的 abort 订阅送达取消原因。
//!
//! # 对外状态与记录的差异
//! [`AdmissionStatus`] 是 TS 的三值判别，不是记录里的原始状态：记录的 `unanswered` 在对外
//! 视图里并入 `done`（`AdmissionStatus::of_record`）。要区分成功（`done`）与未回答
//! （`unanswered`），读 [`Submissions::wait`]（终态）或 [`Submissions::status`]（当前记录）
//! 返回的完整记录。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use kiana_chord::context::Context;
use kiana_telemetry::{
    AttributeValue, SpanAttributes, SpanLink, SpanStatus, TelemetryContext, TelemetrySpan,
    create_typed_span_starter, noop_telemetry_context_as_context,
};

use crate::errors::ConversationBusy;
use crate::harness::generation::start_run;
use crate::harness::inbox::{
    BoundaryAt, INBOX_DOC, InboxItem, InputMode, QueueModes, append_input, apply_boundary,
    has_queued_reset, is_display_only, is_stale, pick_input_entry, prepare_boundary,
    remove_inbox_item,
};
use crate::harness::live::LIVE_DOC;
use crate::harness::telemetry::DURABLE_TELEMETRY_SCHEMA;
use crate::harness::types::{
    InputEntry, SettledSubmissionRecord, Submission, SubmissionAbortOutcome, SubmissionDraft,
    SubmissionDraftKind, WhenBusy,
};
use crate::harness::util::{WaitResult, Waiters, closed_error};
use crate::session::ContextSource;
use crate::session::session::{InternalListener, ObservedCommit, SessionImpl};
use crate::types::{
    BoxError, CommitChange, CommitOptions, ConversationId, DocDraft, DocTarget, JsonValue, Storage,
    SubmissionCreate, SubmissionId, SubmissionKind, SubmissionRecord, SubmissionSettlement,
    SubmissionStatus, TableCommitChange, Tx, id_from_number,
};

/// `Submissions.abort()` 的结果（TS: `AbortResult | "not_found"`）。
///
/// 与对外 `SubmissionAbortOutcome` 的区别只在 `NotFound`：句柄（`Submission::abort`）知道
/// 自己存在，会把 `NotFound` 映射成错误；Harness 级入口按 ID 查找，因此如实返回 `NotFound`
/// （`harness/harness.rs` 的 `abort_submission`）。serde 用 snake_case，序列化值即
/// `aborted`、`already_placed`、`settled`、`not_found`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AbortResult {
    /// 已撤回 queued 提交并移除它的 inbox 项。
    Aborted,
    /// 提交已经放置，无法撤回。
    AlreadyPlaced,
    /// 提交已经结算。
    Settled,
    /// 未知提交或属于别的会话。
    NotFound,
}

/// 一次接纳的结果（TS: `Admission`）。
///
/// `status` 是调用方视角的三态，不是记录里的原始状态（见模块文档）；要拿到最终记录
/// （含 `reason`/`answer`）应调用 [`Submissions::wait`]。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Admission {
    /// 提交 ID。
    pub id: SubmissionId,
    /// 状态。
    pub status: AdmissionStatus,
}

/// `Admission["status"]`。
///
/// - `Queued`：已创建 `queued` 记录并放进 inbox，等待运行边界；
/// - `Placed`：输入已追加进转录并启动（或加入）一次运行；
/// - `Done`：提交已经结算（`done` 或 `unanswered`），不需要再等待。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AdmissionStatus {
    /// 排队等待边界。
    Queued,
    /// 已进入转录。
    Placed,
    /// 已成功。
    Done,
}

impl AdmissionStatus {
    /// TS 的判别值。
    pub fn as_str(self) -> &'static str {
        match self {
            AdmissionStatus::Queued => "queued",
            AdmissionStatus::Placed => "placed",
            AdmissionStatus::Done => "done",
        }
    }

    fn of_record(status: SubmissionStatus) -> Self {
        match status {
            SubmissionStatus::Queued => AdmissionStatus::Queued,
            SubmissionStatus::Placed => AdmissionStatus::Placed,
            SubmissionStatus::Done | SubmissionStatus::Unanswered => AdmissionStatus::Done,
        }
    }
}

fn submission_kind_str(kind: SubmissionKind) -> &'static str {
    match kind {
        SubmissionKind::Input => "input",
        SubmissionKind::Write => "write",
    }
}

fn draft_kind_str(draft: &SubmissionDraft) -> &'static str {
    match draft.kind {
        SubmissionDraftKind::Input { .. } => "input",
        SubmissionDraftKind::Write { .. } => "write",
    }
}

fn is_settled(record: &SubmissionRecord) -> bool {
    matches!(
        record.status,
        SubmissionStatus::Done | SubmissionStatus::Unanswered
    )
}

/// 拒绝一个未类型化调用方写错的输入条目：`kind` 必须是不以 `kiana.` 开头的非空字符串。
///
/// 只用于 input 草稿里可选的 `entry`；write 草稿的 `EntryDraft.kind` 不受此限制。`kiana.`
/// 前缀保留给内建文档/条目，避免宿主条目冒充内建 kind。错误文本会原样带出调用方传入的
/// `kind`，按文本匹配的调用方需要注意。
pub fn check_input_entry(entry: &InputEntry) -> Result<(), BoxError> {
    let kind = &entry.kind;
    if kind.is_empty() || kind.starts_with("kiana.") {
        return Err(format!(
            "An input entry's kind must be a non-empty string not starting with \"kiana.\", got {kind}"
        )
        .into());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Submission drafts (TS: SubmissionDraft)
// ---------------------------------------------------------------------------

// Rust 的 [`SubmissionDraft`] 是 `harness/types.rs` 里的结构体（判别枚举 + requestId）。

// ---------------------------------------------------------------------------
// Submissions
// ---------------------------------------------------------------------------

/// 一个 Harness 的持久提交：接纳、等待与撤回。
///
/// `Clone` 只克隆 `Arc<SubmissionsInner>`，Session、等待表与两个观察句柄都共享；从
/// `SubmissionHandle` 回到 `status`/`wait`/`abort` 也走同一份 inner。
pub struct Submissions {
    inner: Arc<SubmissionsInner>,
}

struct SubmissionsInner {
    /// 提交与观察所在的 Session。
    session: SessionImpl,
    now: Arc<dyn Fn() -> i64 + Send + Sync>,
    /// 每次接纳时读取，在 Session 线上。
    queue_modes: Arc<dyn Fn() -> Result<QueueModes, BoxError> + Send + Sync>,
    /// 启用任务调度；提交与等待都请求推进。
    resume: Arc<dyn Fn() + Send + Sync>,
    /// 接纳 span 的父级（input 草稿用）；调用方没提供遥测时是 noop 上下文。
    telemetry: Arc<dyn TelemetryContext>,
    /// 按提交 ID 等待终态的等待表；提交监听器与关闭监听器共享它。
    waiters: Waiters<SubmissionId, SettledSubmissionRecord>,
    /// 关闭监听器置真；`wait()` 在登记等待前检查它。
    closed: Arc<AtomicBool>,
    /// 提交观察的注销闭包。`Unsubscribe` 是 `FnOnce` 盒子，这里只保存、从不调用；真正的
    /// 监听器在 Session 关闭时随提交/关闭监听表清空（`session/session.rs` 的关闭流程）。
    _commit_unsubscribe: crate::types::Unsubscribe,
    /// 关闭观察的注销闭包；生命周期同上。
    _close_unsubscribe: crate::types::Unsubscribe,
}

impl Clone for Submissions {
    fn clone(&self) -> Self {
        Submissions {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl Submissions {
    /// 新建并挂上 Session 的提交观察与关闭观察。
    ///
    /// TS 额外接收 `Storage`；这里从 `session.storage()` 取，语义相同。队列模式回调不能失败；
    /// 需要把宿主 settings 的错误传回 `submit()` 的调用方时用 [`Submissions::new_fallible`]。
    /// `telemetry` 为 `None` 时使用 noop 上下文。
    ///
    /// # 错误
    /// Session 已不可用时 `observe_commits`/`subscribe_close` 返回错误，构造失败；调用方
    /// （`Harness` 的 `build_harness`）会让 `Harness::open` 一并失败。
    pub fn new(
        session: SessionImpl,
        now: Arc<dyn Fn() -> i64 + Send + Sync>,
        queue_modes: Arc<dyn Fn() -> QueueModes + Send + Sync>,
        resume: Arc<dyn Fn() + Send + Sync>,
        telemetry: Option<Arc<dyn TelemetryContext>>,
    ) -> Result<Self, BoxError> {
        let queue_modes: Arc<dyn Fn() -> Result<QueueModes, BoxError> + Send + Sync> =
            Arc::new(move || Ok(queue_modes()));
        Self::new_fallible(session, now, queue_modes, resume, telemetry)
    }

    /// 同 [`Submissions::new`]，但队列模式回调可以失败：错误让这次 `submit()` 以同一错误拒绝，
    /// 且什么都不入队（TS 的 `Submissions.submit()` 在提交回调里直接调用宿主 settings）。
    pub fn new_fallible(
        session: SessionImpl,
        now: Arc<dyn Fn() -> i64 + Send + Sync>,
        queue_modes: Arc<dyn Fn() -> Result<QueueModes, BoxError> + Send + Sync>,
        resume: Arc<dyn Fn() + Send + Sync>,
        telemetry: Option<Arc<dyn TelemetryContext>>,
    ) -> Result<Self, BoxError> {
        let telemetry = telemetry.unwrap_or_else(noop_telemetry_context_as_context);
        let waiters: Waiters<SubmissionId, SettledSubmissionRecord> = Waiters::new();
        let closed = Arc::new(AtomicBool::new(false));
        let listener_waiters = waiters.clone();
        let listener_closed = Arc::clone(&closed);
        let listener: InternalListener = Arc::new(move |publication, _context| {
            // 只在内存里的提交只改文档。
            if matches!(publication, ObservedCommit::Memory { .. }) {
                return;
            }
            for change in publication.changes() {
                let CommitChange::Table(TableCommitChange::Submission { value }) = change else {
                    continue;
                };
                if is_settled(&value) {
                    let id = value.id;
                    listener_waiters.resolve(&id, SettledSubmissionRecord::new(value));
                }
            }
        });
        let commit_unsubscribe = session.observe_commits(listener)?;
        let close_waiters = waiters.clone();
        let close_session = session.clone();
        let close_unsubscribe = session.subscribe_close(std::sync::Arc::new(move || {
            listener_closed.store(true, Ordering::SeqCst);
            close_waiters.reject_all(closed_error(&close_session));
        }))?;
        let inner = Arc::new(SubmissionsInner {
            session,
            now,
            queue_modes,
            resume,
            telemetry,
            waiters,
            closed: Arc::clone(&closed),
            _commit_unsubscribe: commit_unsubscribe,
            _close_unsubscribe: close_unsubscribe,
        });
        Ok(Submissions { inner })
    }

    /// 一次提交完成接纳（spec §6）；见 [`admit_submission`]。
    ///
    /// 先 `resume()` 启用调度，再在 `kiana.durable.submit` span 里用**一次** `commit_with`
    /// 完成接纳（创建提交记录、入队/放置条目、可能启动运行）。span 的父级：input 用构造时
    /// 传入的 harness telemetry，write 用调用方 `context.telemetry()`；input 还会把调用方的
    /// span 作为 link，便于把用户输入关联到发起它的调用。
    ///
    /// 时钟与队列模式在提交回调内部求值（测试
    /// `a_queued_submit_reads_its_clock_and_queue_modes_on_the_session_line` 锁定），所以
    /// 排在其他提交之后执行的 `submit()` 用的是真正接纳时刻的值，而不是调用时刻的值。
    ///
    /// # 返回值
    /// 成功时返回只含 `(id, Submissions)` 的句柄；`Admission.status` 不会存进句柄，需要时用
    /// `wait()`/`status()` 重新读记录。
    ///
    /// # 错误
    /// 队列模式回调失败、提交失败或接纳规则拒绝（例如 [`ConversationBusy`]）都原样返回；
    /// 提交使用调用方的上下文（`context.context_ref()`），调用被取消同样会让接纳失败。
    /// span 只标 `error` 状态而不写错误文本（错误可能含调用方内容）。成功时 span 追加
    /// `submission.id` 与 `result`（`queued`/`placed`/`done`）。
    pub async fn submit(
        &self,
        conversation_id: ConversationId,
        draft: SubmissionDraft,
        context: &dyn Context,
    ) -> Result<Arc<dyn Submission>, BoxError> {
        (self.inner.resume)();
        let input = matches!(draft.kind, SubmissionDraftKind::Input { .. });
        let parent: Arc<dyn TelemetryContext> = if input {
            Arc::clone(&self.inner.telemetry)
        } else {
            context.telemetry()
        };
        let starter =
            create_typed_span_starter(parent, std::slice::from_ref(&*DURABLE_TELEMETRY_SCHEMA));
        let mut attributes = SpanAttributes::new();
        attributes.insert(
            "submission.type".to_owned(),
            AttributeValue::String(draft_kind_str(&draft).to_owned()),
        );
        attributes.insert(
            "conversation.id".to_owned(),
            AttributeValue::Number(conversation_id.get() as f64),
        );
        let submissions = self.clone();
        let caller_telemetry = context.telemetry();
        let context_ref = context.context_ref();
        starter
            .start("kiana.durable.submit", attributes, move |span, _child| {
                let submissions = submissions.clone();
                let draft = draft.clone();
                let caller_telemetry = Arc::clone(&caller_telemetry);
                let context_ref = Arc::clone(&context_ref);
                Box::pin(async move {
                    if input {
                        span.add_link(SpanLink {
                            context: caller_telemetry.span_context_id(),
                            attributes: None,
                        });
                    }
                    let session = submissions.inner.session.clone();
                    let admit_span = Arc::clone(&span);
                    let commit_context = Arc::clone(&context_ref);
                    let commit_draft = draft.clone();
                    let commit_submissions = submissions.clone();
                    let outcome = session
                        .commit_with(
                            move |tx| {
                                let span = Arc::clone(&admit_span);
                                let draft = commit_draft.clone();
                                let submissions = commit_submissions.clone();
                                Box::pin(async move {
                                    // TS 在 `commitWith` 回调里求值 `this.#now()` 与
                                    // `this.#queueModes()`：排队到 Session 线上的提交用真正
                                    // 接纳时的时钟与队列模式。
                                    let now = (submissions.inner.now)();
                                    let modes = (submissions.inner.queue_modes)()?;
                                    admit_submission(
                                        tx,
                                        conversation_id,
                                        &draft,
                                        now,
                                        modes,
                                        Some(span.as_ref()),
                                    )
                                    .await
                                })
                            },
                            commit_context,
                            CommitOptions::default(),
                        )
                        .await;
                    match outcome {
                        Ok(admission) => {
                            let mut attributes = SpanAttributes::new();
                            attributes.insert(
                                "submission.id".to_owned(),
                                AttributeValue::Number(admission.id.get() as f64),
                            );
                            attributes.insert(
                                "result".to_owned(),
                                AttributeValue::String(admission.status.as_str().to_owned()),
                            );
                            span.set_attributes(attributes);
                            Ok(Arc::new(SubmissionHandle {
                                id: admission.id,
                                submissions,
                            }) as Arc<dyn Submission>)
                        }
                        Err(error) => {
                            // 错误文本可能含调用方内容；只记录状态。
                            span.set_status(SpanStatus::Error { error: None });
                            Err(error)
                        }
                    }
                })
            })
            .await
            .map_err(|error| Box::new(error) as BoxError)?
    }

    /// 已有提交的句柄；不存在时 `None`。
    ///
    /// 读取在变更线上执行（`read_on_line`），因此看到的是此前已接纳提交完成后的状态。
    /// 返回的句柄不缓存记录：`status`/`wait`/`abort` 都会重新读存储。
    pub async fn get(
        &self,
        id: SubmissionId,
        context: &dyn Context,
    ) -> Result<Option<Arc<dyn Submission>>, BoxError> {
        let storage = self.inner.session.storage().clone();
        let context_ref = context.context_ref();
        let record = self
            .inner
            .session
            .read_on_line(async move { storage.submission(id, &*context_ref).await })
            .await?;
        Ok(record.map(|record| {
            Arc::new(SubmissionHandle {
                id: record.id,
                submissions: self.clone(),
            }) as Arc<dyn Submission>
        }))
    }

    /// 当前记录；不存在时抛 `Submission ${id} does not exist`。
    ///
    /// 与 [`Submissions::get`] 不同，缺失记录在这里是错误；读取同样在变更线上完成。
    pub async fn status(
        &self,
        id: SubmissionId,
        context: &dyn Context,
    ) -> Result<SubmissionRecord, BoxError> {
        let storage = self.inner.session.storage().clone();
        let context_ref = context.context_ref();
        let record = self
            .inner
            .session
            .read_on_line(async move { storage.submission(id, &*context_ref).await })
            .await?;
        match record {
            Some(record) => Ok(record),
            None => Err(format!("Submission {id} does not exist").into()),
        }
    }

    /// 等待终态。检查与登记在同一变更线上，中间不会有结算提交。
    ///
    /// 流程：先 `resume()`；在变更线上读记录，已结算（`done`/`unanswered`）立即返回；否则
    /// 检查 `closed`，再 `Waiters::add` 登记。登记成功后离开变更线 `await` 接收端，不占住
    /// 变更线，其他提交仍可推进到结算。
    ///
    /// # 错误
    /// - 记录不存在：`Submission {id} does not exist`。
    /// - 会话关闭/失败：`closed_error`（无失败原因是 `HarnessClosed`，否则是带 cause 的
    ///   `SessionFailed`）。
    /// - 调用方上下文在等待期间被取消：等待以 abort 错误结束（`Waiters` 的 abort 订阅送达）。
    /// - 发送端被丢弃（所有 `Waiters` 克隆都消失，通道随之关闭）：按关闭错误处理。
    pub async fn wait(
        &self,
        id: SubmissionId,
        context: &dyn Context,
    ) -> Result<SettledSubmissionRecord, BoxError> {
        (self.inner.resume)();
        let storage = self.inner.session.storage().clone();
        let context_ref = context.context_ref();
        let waiters = self.inner.waiters.clone();
        let closed = Arc::clone(&self.inner.closed);
        let session = self.inner.session.clone();
        let receiver = self
            .inner
            .session
            .read_on_line(async move {
                let record = storage.submission(id, &*context_ref).await?;
                let Some(record) = record else {
                    return Err(format!("Submission {id} does not exist").into());
                };
                if is_settled(&record) {
                    return Ok(WaitStart::Settled(SettledSubmissionRecord::new(record)));
                }
                // 关闭会同步拒绝已登记的 waiter，也可能在这次读取期间开始。
                if closed.load(Ordering::SeqCst) {
                    return Err(closed_error(&session));
                }
                let receiver = waiters.add(id, &*context_ref)?;
                Ok(WaitStart::Pending(receiver))
            })
            .await?;
        match receiver {
            WaitStart::Settled(record) => Ok(record),
            WaitStart::Pending(receiver) => match receiver.await {
                Ok(result) => result,
                Err(_) => Err(closed_error(&self.inner.session)),
            },
        }
    }

    /// 撤回一个 queued 提交并移除它的 inbox 项；已放置与已结算的提交如实报告。
    ///
    /// 整个“读记录 → 判断 → 结算 + 移出 inbox”是一条 `commit_with`，与并发的 `submit`/
    /// `wait` 在变更线上串行，检查之后不存在被抢走的窗口。`conversation_id` 给定时，属于
    /// 别的会话的提交按 `NotFound` 处理（与不存在不可区分）。
    ///
    /// queued 提交以 `SubmissionSettlement::Unanswered { reason: "aborted", detail: None }`
    /// 结算，并调用 `remove_inbox_item`（它只按 ID 移除一条，缺失时静默通过）。注意
    /// `Tx::settle_submission` 只把变化排入事务、没有返回值，真正的转换在 `assemble()`；
    /// abort 只把 queued 转成 unanswered，该转换总是合法（失败只会来自 `remove_inbox_item`
    /// 读取损坏的 inbox 等其它路径）。
    pub async fn abort(
        &self,
        id: SubmissionId,
        context: &dyn Context,
        conversation_id: Option<ConversationId>,
    ) -> Result<AbortResult, BoxError> {
        let session = self.inner.session.clone();
        let context_ref = context.context_ref();
        session
            .commit_with(
                move |tx| {
                    Box::pin(async move {
                        let record = tx.submission(id).await?;
                        let Some(record) = record else {
                            return Ok(AbortResult::NotFound);
                        };
                        if let Some(conversation_id) = conversation_id
                            && record.conversation_id != conversation_id
                        {
                            return Ok(AbortResult::NotFound);
                        }
                        if record.status == SubmissionStatus::Queued {
                            tx.settle_submission(
                                id,
                                SubmissionSettlement::Unanswered {
                                    reason: "aborted".to_owned(),
                                    detail: None,
                                },
                            );
                            remove_inbox_item(tx, record.conversation_id, id).await?;
                            return Ok(AbortResult::Aborted);
                        }
                        Ok(if record.status == SubmissionStatus::Placed {
                            AbortResult::AlreadyPlaced
                        } else {
                            AbortResult::Settled
                        })
                    })
                },
                context_ref,
                CommitOptions::default(),
            )
            .await
    }
}

/// `wait()` 在线内得到的两条路：记录已经结算，或拿到登记好的接收端。
///
/// 之所以不在 `read_on_line` 的任务里直接 `await` 接收端：那会占住 Session 的变更线直到
/// 提交结算，而结算本身需要提交到同一条线上，双方会互相等待。先把接收端带出变更线，再由
/// 调用方等待。
#[allow(clippy::large_enum_variant)]
enum WaitStart {
    Settled(SettledSubmissionRecord),
    Pending(tokio::sync::oneshot::Receiver<WaitResult<SettledSubmissionRecord>>),
}

/// 一个已接纳提交的宿主对象（TS: `SubmissionHandle`）。
///
/// 只保存 `id` 与 `Submissions`，不缓存记录：所有查询都重新读 Storage，因此会话关闭后调用
/// 会得到错误而不是陈旧数据。`abort` 把内部的 `AbortResult::NotFound` 映射成
/// `Submission {id} does not exist` 错误（[`SubmissionAbortOutcome`] 没有该变体）。
struct SubmissionHandle {
    id: SubmissionId,
    submissions: Submissions,
}

#[async_trait]
impl Submission for SubmissionHandle {
    fn id(&self) -> SubmissionId {
        self.id
    }

    async fn status(&self, context: &dyn Context) -> Result<SubmissionRecord, BoxError> {
        self.submissions.status(self.id, context).await
    }

    async fn wait(&self, context: &dyn Context) -> Result<SettledSubmissionRecord, BoxError> {
        self.submissions.wait(self.id, context).await
    }

    async fn abort(&self, context: &dyn Context) -> Result<SubmissionAbortOutcome, BoxError> {
        let outcome = self.submissions.abort(self.id, context, None).await?;
        match outcome {
            AbortResult::Aborted => Ok(SubmissionAbortOutcome::Aborted),
            AbortResult::AlreadyPlaced => Ok(SubmissionAbortOutcome::AlreadyPlaced),
            AbortResult::Settled => Ok(SubmissionAbortOutcome::Settled),
            AbortResult::NotFound => Err(format!("Submission {} does not exist", self.id).into()),
        }
    }
}

// ---------------------------------------------------------------------------
// admitSubmission
// ---------------------------------------------------------------------------

/// 在提交里接纳一次提交（spec §6）；`Conversation.submit()` 与会话拥有的压缩共用它。
///
/// 已知的 request ID 直接返回既有提交；只含 `kind`/`data` 的展示型写入立刻追加并结算 `done`，
/// 除非它排在一次 reset 之后（那时它必须落在 reset 之后的上下文里）。忙碌的会话把其它提交排进
/// `kiana.inbox`，或对 `whenBusy: "reject"` 的输入抛 [`ConversationBusy`]。空闲会话若已有排队项，则
/// 把它排在它们之后并跑一次 final 边界；否则空闲输入放置自己的条目并启动运行，空闲写入追加条目
/// 并结算 `done`（head 落在活动范围之前时结算 `stale`）。
///
/// # 阶段
/// 1. 校验 input 条目、按 `request_id` 去重；
/// 2. 展示型写入的快速路径（除非队列里有 reset）；
/// 3. 读 `kiana.live` 判断忙碌、给 steer 输入接上当前运行的 trace、执行 reject 策略；
/// 4. 空闲时准备边界（读 head 与 inbox，必须在第一次表写入之前）；
/// 5. 排队路径：创建 queued 记录并入 inbox；空闲且已有排队项时应用 final 边界；
/// 6. 空闲且 inbox 为空：写入直接落定，或输入追加条目后启动运行。
///
/// # 参数
/// `now` 与 `queue_modes` 由调用方在变更线上求值（[`Submissions::submit`] 的回调里读取），
/// 保证排队后的接纳使用真正接纳时刻的设置。`span` 用于把调用链注入新建提交的 `trace`，
/// 并在 steer 输入上追加当前运行任务的 link；没有遥测时传 `None`。
#[allow(clippy::too_many_lines)]
pub async fn admit_submission(
    tx: &dyn Tx,
    conversation_id: ConversationId,
    draft: &SubmissionDraft,
    now: i64,
    queue_modes: QueueModes,
    span: Option<&dyn TelemetrySpan>,
) -> Result<Admission, BoxError> {
    let kind = draft_kind_str(draft);
    // 未类型化调用方可以带自定义 entry；空 kind 与 `kiana.` 前缀在这里拒绝。
    if let SubmissionDraftKind::Input {
        entry: Some(entry), ..
    } = &draft.kind
    {
        check_input_entry(entry)?;
    }
    // `request_id` 去重先于任何写入：同一会话里重复提交返回已有记录；两次 kind 不同说明
    // 调用方复用了错误的重试键，直接报错。
    if let Some(request_id) = &draft.request_id
        && let Some(existing) = tx
            .submission_by_request(conversation_id, request_id)
            .await?
    {
        {
            if submission_kind_str(existing.kind) != kind {
                return Err(format!(
                    "Request {request_id} already identifies a submission of type {}",
                    submission_kind_str(existing.kind)
                )
                .into());
            }
            return Ok(Admission {
                id: existing.id,
                status: AdmissionStatus::of_record(existing.status),
            });
        }
    }
    let trace = span.and_then(|span| {
        span.propagation()
            .and_then(|propagation| propagation.inject(span))
    });
    // 展示型写入不改变模型看到的东西，因此不需要边界——除非它排在一次 reset 之后。
    if let SubmissionDraftKind::Write { entry } = &draft.kind
        && is_display_only(entry)
    {
        let inbox = tx
            .doc(&DocTarget::conversation(&INBOX_DOC, conversation_id))
            .await?;
        let items = inbox_items(&inbox)?;
        if !has_queued_reset(&items) {
            let appended = tx.append_entry(conversation_id, entry.clone()).await?;
            let record = tx
                .create_submission(SubmissionCreate {
                    trace,
                    conversation_id,
                    request_id: draft.request_id.clone(),
                    kind: SubmissionKind::Write,
                    status: SubmissionStatus::Done,
                    entry: Some(appended.id),
                    answer: None,
                    reason: None,
                    detail: None,
                })
                .await?;
            return Ok(Admission {
                id: record.id,
                status: AdmissionStatus::Done,
            });
        }
    }
    let live_target = DocTarget::conversation(&LIVE_DOC, conversation_id);
    let mut live = tx.doc(&live_target).await?;
    // 忙碌 = `kiana.live` 里有非 null 的 `run`；`taskId` 同时用于下面 steer 的 span link。
    let run_task_id = live
        .value()
        .get("run")
        .filter(|run| !run.is_null())
        .and_then(|run| run.get("taskId"))
        .and_then(JsonValue::as_i64);
    let busy = live.value().get("run").is_some_and(|run| !run.is_null());
    // steer 输入会插进当前运行，把当前运行任务的 trace 作为 link，便于把输入与它影响的那次
    // 运行关联起来；运行没有 trace（旧记录）或没有遥测时跳过。
    if let SubmissionDraftKind::Input {
        when_busy: Some(WhenBusy::Steer),
        ..
    } = &draft.kind
        && let Some(run_task_id) = run_task_id
        && let Some(span) = span
        && let Some(propagation) = span.propagation()
    {
        let run = tx.task(id_from_number(run_task_id)).await?;
        if let Some(trace) = run.and_then(|record| record.trace) {
            let link_context = propagation.extract(Some(&trace));
            span.add_link(SpanLink {
                context: link_context.span_context_id(),
                attributes: None,
            });
        }
    }
    // `whenBusy: "reject"` 只在忙碌时失败；空闲时三个策略值都不会触发 reject，按下面的
    // 空闲/排队路径处理（可能入队后由边界放置）。
    if busy
        && matches!(
            draft.kind,
            SubmissionDraftKind::Input {
                when_busy: Some(WhenBusy::Reject),
                ..
            }
        )
    {
        return Err(Box::new(ConversationBusy::new(conversation_id)));
    }
    // 边界要读表，所以在第一次表写入之前准备；忙碌的会话不需要边界。
    let mut boundary = if busy {
        None
    } else {
        Some(prepare_boundary(tx, conversation_id, queue_modes).await?)
    };
    // 两条路分开：忙碌（boundary 为 None，只入 inbox，等下一次运行边界：`PostTools` 或
    // `Final`），或空闲但 inbox 已有排队项（本次提交要排在它们之后，并让 final 边界先清一批）。
    if boundary.is_none() || boundary_inbox_len(boundary.as_ref())? > 0 {
        let record = tx
            .create_submission(SubmissionCreate {
                trace: trace.clone(),
                conversation_id,
                request_id: draft.request_id.clone(),
                kind: match draft.kind {
                    SubmissionDraftKind::Input { .. } => SubmissionKind::Input,
                    SubmissionDraftKind::Write { .. } => SubmissionKind::Write,
                },
                status: SubmissionStatus::Queued,
                entry: None,
                answer: None,
                reason: None,
                detail: None,
            })
            .await?;
        let id = record.id;
        // 宿主可能把可选字段留成 `undefined`；草稿只接受严格 JSON。
        let item = match &draft.kind {
            SubmissionDraftKind::Write { entry } => InboxItem::Write {
                id,
                entry: entry.clone(),
            },
            SubmissionDraftKind::Input {
                content,
                when_busy,
                entry,
            } => InboxItem::Input {
                id,
                mode: if *when_busy == Some(WhenBusy::Steer) {
                    InputMode::Steer
                } else {
                    InputMode::FollowUp
                },
                content: content.clone(),
                entry: entry.as_ref().map(pick_input_entry),
            },
        };
        // 忙碌：直接写 inbox 文档，提交保持 queued。空闲：把项写进已经读好的 boundary
        // 草稿，随后在同一事务里应用 final 边界。
        match boundary.as_mut() {
            None => {
                let mut inbox = tx
                    .doc(&DocTarget::conversation(&INBOX_DOC, conversation_id))
                    .await?;
                push_inbox_item_into(&mut inbox, &item)?;
                return Ok(Admission {
                    id,
                    status: AdmissionStatus::Queued,
                });
            }
            Some(boundary) => push_inbox_item_into(&mut boundary.inbox, &item)?,
        }
        let boundary = boundary.as_mut().expect("checked above");
        // final 边界放置未 stale 的写入与已选中的 steer/follow-up；stale 写入结算为
        // unanswered("stale")，对外 admission 状态仍映射为 done。只要放置了用户项就启动运行。
        // 状态以实际放置结果为准：输入只有成为“最后一个被放置的用户项”
        // 才是 placed，否则仍留在队列里（一对一队列模式可能只选走更早的项）。
        let result = apply_boundary(tx, boundary, BoundaryAt::Final, now).await?;
        if !result.users.is_empty() {
            start_run(tx, conversation_id, &mut live, &result.users).await?;
        }
        let status = match &draft.kind {
            SubmissionDraftKind::Write { .. } => AdmissionStatus::Done,
            SubmissionDraftKind::Input { .. } => {
                if result.users.last() == Some(&id) {
                    AdmissionStatus::Placed
                } else {
                    AdmissionStatus::Queued
                }
            }
        };
        return Ok(Admission { id, status });
    }
    // 空闲且 inbox 为空：这次提交自己开启活动范围，不需要入队。
    let boundary = boundary.take().expect("idle branch has a boundary");
    if let SubmissionDraftKind::Write { entry } = &draft.kind {
        // head 指向活动范围之前的写入会带回已经切掉的历史，直接结算为
        // unanswered("stale")；对外状态仍映射成 done。
        if is_stale(&boundary, entry) {
            let record = tx
                .create_submission(SubmissionCreate {
                    trace,
                    conversation_id,
                    request_id: draft.request_id.clone(),
                    kind: SubmissionKind::Write,
                    status: SubmissionStatus::Unanswered,
                    entry: None,
                    answer: None,
                    reason: Some("stale".to_owned()),
                    detail: None,
                })
                .await?;
            return Ok(Admission {
                id: record.id,
                status: AdmissionStatus::Done,
            });
        }
        let appended = tx.append_entry(conversation_id, entry.clone()).await?;
        let record = tx
            .create_submission(SubmissionCreate {
                trace,
                conversation_id,
                request_id: draft.request_id.clone(),
                kind: SubmissionKind::Write,
                status: SubmissionStatus::Done,
                entry: Some(appended.id),
                answer: None,
                reason: None,
                detail: None,
            })
            .await?;
        return Ok(Admission {
            id: record.id,
            status: AdmissionStatus::Done,
        });
    }
    let SubmissionDraftKind::Input { content, entry, .. } = &draft.kind else {
        unreachable!("write handled above");
    };
    // 空闲输入：追加条目、创建 placed 记录，并用这条提交启动运行。
    let appended = append_input(tx, conversation_id, content, entry.as_ref(), now).await?;
    let record = tx
        .create_submission(SubmissionCreate {
            trace,
            conversation_id,
            request_id: draft.request_id.clone(),
            kind: SubmissionKind::Input,
            status: SubmissionStatus::Placed,
            entry: Some(appended.id),
            answer: None,
            reason: None,
            detail: None,
        })
        .await?;
    let id = record.id;
    start_run(tx, conversation_id, &mut live, &[id]).await?;
    Ok(Admission {
        id,
        status: AdmissionStatus::Placed,
    })
}

/// 读取 inbox 草稿里的 `items` 并逐项反序列化成 [`InboxItem`]。
///
/// 与 `harness/inbox.rs` 的 `read_raw_items` 不同，这里走类型化解析：未知 `mode`、缺字段等
/// 形状错误会让整个调用返回错误。展示型写入的 reset 判断与空闲提交的 `boundary_inbox_len`
/// 都经过它，因此外部写坏的 inbox 会让这两条路径失败；边界路径则保留原始 JSON 项、只对选中
/// 的项做解析。
fn inbox_items(draft: &DocDraft) -> Result<Vec<InboxItem>, BoxError> {
    match draft.value().get("items") {
        None => Ok(Vec::new()),
        Some(JsonValue::Array(items)) => items
            .iter()
            .map(|item| Ok(serde_json::from_value(item.clone())?))
            .collect(),
        Some(_) => Err("kiana.inbox items must be an array".into()),
    }
}

/// 边界草稿里现有的 inbox 项数；边界为 `None`（忙碌）时返回 0。
///
/// 调用点前面的 `boundary.is_none() ||` 会先短路，所以 `None` 分支只为保持函数自洽。
fn boundary_inbox_len(
    boundary: Option<&crate::harness::inbox::Boundary>,
) -> Result<usize, BoxError> {
    match boundary {
        None => Ok(0),
        Some(boundary) => Ok(inbox_items(&boundary.inbox)?.len()),
    }
}

/// 把一项追加进 inbox 草稿的 `items`（TS: `items.push(...)`）。
///
/// `items` 缺失时新建数组；存在但不是数组时报错（`kiana.inbox` 的 `items` 必须是数组）。
fn push_inbox_item_into(draft: &mut DocDraft, item: &InboxItem) -> Result<(), BoxError> {
    let value = serde_json::to_value(item)?;
    match draft.value_mut().get_mut("items") {
        Some(JsonValue::Array(items)) => {
            items.push(value);
            Ok(())
        }
        None => {
            draft
                .value_mut()
                .insert("items".to_owned(), JsonValue::Array(vec![value]));
            Ok(())
        }
        Some(_) => Err("kiana.inbox items must be an array".into()),
    }
}
