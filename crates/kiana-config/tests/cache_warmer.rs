//! `cache-warmer.test.ts` 的移植（9 条用例中的 8 条；`ExtensionRunner` 那条属于
//! `kiana-extensions` 单元，见文件末尾说明）。
//!
//! 时间语义用**手动时钟 + 手动定时器**复刻 vitest 的 fake timers：不依赖墙钟等待。
//! 格式化与数值来自 TS 参考实现生成的黄金夹具 `tests/fixtures/cache_warmer/golden.json`。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kiana_agent_core::AgentMessage;
use kiana_ai::event_stream::AssistantMessageEventStream;
use kiana_ai::models::ModelsSimpleStreamOptions;
use kiana_ai::types::{
    AssistantMessage, AssistantRole, CacheRetention, Context, Message, Model, SimpleStreamOptions,
    StopReason, StreamOptions, ThinkingLevel, Usage, UsageCost,
};
use kiana_config::cache::warmer::{
    CacheWarmRequest, CacheWarmer, CacheWarmerFuture, CacheWarmerModels, CacheWarmerOptions,
    CacheWarmerSession, CacheWarmerTimer, CacheWarmerTimerCallback, CacheWarmerTimerHandle,
    CacheWarmingAction, CacheWarmingDecideFn, CacheWarmingDecision, CacheWarmingDecisionEvent,
    CacheWarmingPhase, CacheWarmingState, CacheWarmingStatus, SharedCacheWarmerSession,
    format_cache_warming_status, format_cache_warming_usage, get_cache_warming_delay_ms,
    get_prompt_cache_ttl_ms, is_replayable,
};
use kiana_config::settings::CacheWarmingMode;
use kiana_session_core::session_manager::{
    SessionEntry, SessionManager, SessionMessageEntry, SessionMessageType, UsageEntry,
    UsageEntryType,
};
use serde_json::Value;

// ---------------------------------------------------------------------------
// 黄金夹具
// ---------------------------------------------------------------------------

fn golden() -> Value {
    serde_json::from_str(include_str!("fixtures/cache_warmer/golden.json"))
        .expect("cache warmer golden fixture")
}

fn golden_model(name: &str) -> Model {
    serde_json::from_value(golden()["models"][name].clone()).expect("model")
}

#[derive(Debug, Clone)]
struct ReportedUsage {
    kind: String,
    provider: String,
    model: String,
    usage: Usage,
    note: Option<String>,
}

/// `Pick<SessionManager, "appendUsage" | "getBranch">` 的假实现。
#[derive(Default)]
struct FakeSession {
    branch: Vec<SessionEntry>,
    appended: Vec<ReportedUsage>,
    entries: Vec<UsageEntry>,
}

impl FakeSession {
    fn with_branch(branch: Vec<SessionEntry>) -> Self {
        Self {
            branch,
            ..Self::default()
        }
    }

    fn appended(&self) -> Vec<ReportedUsage> {
        self.appended.clone()
    }
}

impl CacheWarmerSession for FakeSession {
    fn append_usage(
        &mut self,
        kind: &str,
        provider: &str,
        model: &str,
        usage: &Usage,
        note: Option<&str>,
    ) -> Option<UsageEntry> {
        self.appended.push(ReportedUsage {
            kind: kind.to_string(),
            provider: provider.to_string(),
            model: model.to_string(),
            usage: usage.clone(),
            note: note.map(str::to_string),
        });
        let entry = UsageEntry {
            kind: UsageEntryType::Marker,
            id: format!("u{}", self.entries.len() + 1),
            parent_id: None,
            timestamp: "1970-01-01T00:00:00.000Z".to_string(),
            kind_name: kind.to_string(),
            provider: provider.to_string(),
            model: model.to_string(),
            usage: usage.clone(),
            note: note.filter(|note| !note.is_empty()).map(str::to_string),
            raw_entry: None,
        };
        self.entries.push(entry.clone());
        Some(entry)
    }

    fn get_branch(&self) -> Vec<SessionEntry> {
        self.branch.clone()
    }
}

// ---------------------------------------------------------------------------
// 手动时钟 / 定时器
// ---------------------------------------------------------------------------

#[derive(Default)]
struct ManualTimerState {
    now: i64,
    next_id: u64,
    timers: Vec<(u64, i64, CacheWarmerTimerCallback)>,
}

#[derive(Default)]
struct ManualTimer {
    state: Arc<Mutex<ManualTimerState>>,
}

struct ManualTimerHandle {
    state: Arc<Mutex<ManualTimerState>>,
    id: u64,
}

impl CacheWarmerTimerHandle for ManualTimerHandle {
    fn clear(&self) {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .timers
            .retain(|(id, _, _)| *id != self.id);
    }
}

impl ManualTimer {
    fn new(now: i64) -> Arc<Self> {
        Arc::new(Self {
            state: Arc::new(Mutex::new(ManualTimerState {
                now,
                ..ManualTimerState::default()
            })),
        })
    }

    fn set_now(&self, now: i64) {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .now = now;
    }

    fn clear_all(&self) {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .timers
            .clear();
    }

    /// `vi.advanceTimersByTimeAsync`：推进时钟并返回所有到期回调的 future。
    fn advance(&self, delta: i64) -> Vec<CacheWarmerFuture> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.now += delta;
        let now = state.now;
        let mut due = Vec::new();
        let mut remaining = Vec::new();
        for timer in std::mem::take(&mut state.timers) {
            if timer.1 <= now {
                due.push(timer);
            } else {
                remaining.push(timer);
            }
        }
        state.timers = remaining;
        drop(state);
        due.sort_by_key(|(id, due_at, _)| (*due_at, *id));
        due.into_iter().map(|(_, _, callback)| callback()).collect()
    }

    fn pending(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .timers
            .len()
    }
}

impl CacheWarmerTimer for ManualTimer {
    fn set_timeout(
        &self,
        delay_ms: i64,
        callback: CacheWarmerTimerCallback,
    ) -> Box<dyn CacheWarmerTimerHandle> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.next_id += 1;
        let id = state.next_id;
        let due = state.now + delay_ms.max(0);
        state.timers.push((id, due, callback));
        Box::new(ManualTimerHandle {
            state: Arc::clone(&self.state),
            id,
        })
    }

    fn now(&self) -> i64 {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .now
    }
}

async fn advance(timer: &ManualTimer, delta: i64) {
    for future in timer.advance(delta) {
        future.await;
    }
}

// ---------------------------------------------------------------------------
// 假模型运行时
// ---------------------------------------------------------------------------

type ResultFn = Arc<dyn Fn(&Model) -> AssistantMessage + Send + Sync>;

struct RecordedCall {
    model: Model,
    options: ModelsSimpleStreamOptions,
}

struct FakeModels {
    calls: Mutex<Vec<RecordedCall>>,
    result: ResultFn,
    gate: Option<Arc<tokio::sync::Notify>>,
}

impl CacheWarmerModels for FakeModels {
    fn stream_simple(
        &self,
        model: &Model,
        _context: &Context,
        options: ModelsSimpleStreamOptions,
    ) -> AssistantMessageEventStream {
        self.calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(RecordedCall {
                model: model.clone(),
                options,
            });
        let stream = AssistantMessageEventStream::new();
        let message = (self.result)(model);
        match &self.gate {
            Some(gate) => {
                let stream = stream.clone();
                let gate = Arc::clone(gate);
                tokio::spawn(async move {
                    gate.notified().await;
                    stream.end(message);
                });
            }
            None => stream.end(message),
        }
        stream
    }
}

// ---------------------------------------------------------------------------
// 测试夹具
// ---------------------------------------------------------------------------

struct Harness {
    warmer: CacheWarmer,
    timer: Arc<ManualTimer>,
    models: Arc<FakeModels>,
    session: Arc<Mutex<FakeSession>>,
    events: Arc<Mutex<Vec<CacheWarmingDecisionEvent>>>,
    warmed: Arc<Mutex<Vec<UsageEntry>>>,
    mode: Arc<Mutex<CacheWarmingMode>>,
}

struct HarnessOptions {
    mode: CacheWarmingMode,
    branch: Vec<SessionEntry>,
    result: ResultFn,
    decide: Option<CacheWarmingDecideFn>,
    gate: Option<Arc<tokio::sync::Notify>>,
    timer: Option<Arc<ManualTimer>>,
}

impl Default for HarnessOptions {
    fn default() -> Self {
        Self {
            mode: CacheWarmingMode::Idle,
            branch: branch_with_prompt(100_000),
            result: Arc::new(|model| response(model, StopReason::Length)),
            decide: None,
            gate: None,
            timer: None,
        }
    }
}

fn harness(options: HarnessOptions) -> Harness {
    let timer = options.timer.unwrap_or_else(|| ManualTimer::new(0));
    let models = Arc::new(FakeModels {
        calls: Mutex::new(Vec::new()),
        result: options.result,
        gate: options.gate,
    });
    let session = Arc::new(Mutex::new(FakeSession::with_branch(options.branch)));
    let mode = Arc::new(Mutex::new(options.mode));
    let events: Arc<Mutex<Vec<CacheWarmingDecisionEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let decide_events = Arc::clone(&events);
    let decide_override = options.decide;
    let decide: CacheWarmingDecideFn = Arc::new(move |event| {
        decide_events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(event.clone());
        match &decide_override {
            Some(decide) => decide(event),
            None => Box::pin(async move { event.action }),
        }
    });
    let get_mode = {
        let mode = Arc::clone(&mode);
        Arc::new(move || *mode.lock().unwrap_or_else(|poisoned| poisoned.into_inner()))
    };
    let shared_session: SharedCacheWarmerSession = session.clone();
    let warmer = CacheWarmer::new(CacheWarmerOptions {
        models: models.clone(),
        session: shared_session,
        get_mode,
        decide: Some(decide),
        timer: Some(timer.clone()),
    });
    let warmed: Arc<Mutex<Vec<UsageEntry>>> = Arc::new(Mutex::new(Vec::new()));
    {
        let warmed = Arc::clone(&warmed);
        warmer.set_on_warmed(Some(Arc::new(move |entry| {
            warmed
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(entry);
        })));
    }
    Harness {
        warmer,
        timer,
        models,
        session,
        events,
        warmed,
        mode,
    }
}

fn warm_usage() -> Usage {
    Usage {
        input: 0,
        output: 1,
        cache_read: 100,
        cache_write: 0,
        total_tokens: 101,
        cost: UsageCost {
            cache_read: 0.01,
            total: 0.01,
            ..Default::default()
        },
        ..Default::default()
    }
}

fn response(model: &Model, stop_reason: StopReason) -> AssistantMessage {
    AssistantMessage {
        role: AssistantRole::Assistant,
        content: Vec::new(),
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        usage: warm_usage(),
        stop_reason,
        timestamp: 0,
        ..Default::default()
    }
}

fn branch_with_prompt(prompt_tokens: u64) -> Vec<SessionEntry> {
    let model = golden_model("adaptive");
    let mut message = response(&model, StopReason::Length);
    message.usage = Usage {
        input: 0,
        output: 10,
        cache_read: prompt_tokens,
        cache_write: 0,
        total_tokens: prompt_tokens + 10,
        cost: warm_usage().cost,
        ..Default::default()
    };
    vec![SessionEntry::Message(SessionMessageEntry {
        kind: SessionMessageType::Marker,
        id: "a".to_string(),
        parent_id: None,
        timestamp: "1970-01-01T00:00:00.000Z".to_string(),
        message: AgentMessage::Standard(Message::Assistant(message)),
        raw_message: None,
    })]
}

fn empty_options() -> ModelsSimpleStreamOptions {
    ModelsSimpleStreamOptions::default()
}

fn request(model: Model, options: ModelsSimpleStreamOptions) -> CacheWarmRequest {
    CacheWarmRequest {
        model,
        context: Context::default(),
        options,
    }
}

fn current() -> Arc<dyn Fn() -> bool + Send + Sync> {
    Arc::new(|| true)
}

fn calls(h: &Harness) -> Vec<(String, String)> {
    h.models
        .calls
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .iter()
        .map(|call| (call.model.provider.clone(), call.model.id.clone()))
        .collect()
}

// ---------------------------------------------------------------------------
// 1. "derives eligibility and timing from retention and provider behavior"
// ---------------------------------------------------------------------------

#[test]
fn derives_eligibility_and_timing_from_retention_and_provider_behavior() {
    let fixture = golden();
    let adaptive = golden_model("adaptive");
    let budget = golden_model("budget");
    let openai = golden_model("openai");
    let unknown = golden_model("unknown");

    let mut long_env = HashMap::new();
    long_env.insert("KIANA_CACHE_RETENTION".to_string(), "long".to_string());
    let env_options = StreamOptions {
        env: Some(long_env.into_iter().collect()),
        ..Default::default()
    };
    let long_options = StreamOptions {
        cache_retention: Some(CacheRetention::Long),
        ..Default::default()
    };
    let none_options = StreamOptions {
        cache_retention: Some(CacheRetention::None),
        ..Default::default()
    };

    let ttls = vec![
        get_prompt_cache_ttl_ms(&adaptive, None),
        get_prompt_cache_ttl_ms(&adaptive, Some(&long_options)),
        get_prompt_cache_ttl_ms(&adaptive, Some(&none_options)),
        get_prompt_cache_ttl_ms(&adaptive, Some(&env_options)),
        get_prompt_cache_ttl_ms(&openai, Some(&long_options)),
        get_prompt_cache_ttl_ms(&unknown, None),
    ];
    let expected_ttls: Vec<Option<f64>> = fixture["eligibility"]["ttls"]
        .as_array()
        .expect("ttls")
        .iter()
        .map(Value::as_f64)
        .collect();
    assert_eq!(ttls, expected_ttls);

    let delays = vec![
        get_cache_warming_delay_ms(300_000.0),
        get_cache_warming_delay_ms(60_000.0),
        get_cache_warming_delay_ms(10_000.0),
    ];
    let expected_delays: Vec<Option<f64>> = fixture["eligibility"]["delays"]
        .as_array()
        .expect("delays")
        .iter()
        .map(Value::as_f64)
        .collect();
    assert_eq!(delays, expected_delays);

    let reasoning = SimpleStreamOptions {
        reasoning: Some(ThinkingLevel::Medium),
        ..Default::default()
    };
    let replayable = vec![
        is_replayable(&budget, Some(&reasoning)),
        is_replayable(&budget, None),
        is_replayable(&adaptive, Some(&reasoning)),
        is_replayable(&openai, Some(&reasoning)),
    ];
    let expected_replayable: Vec<bool> = fixture["eligibility"]["replayable"]
        .as_array()
        .expect("replayable")
        .iter()
        .map(|value| value.as_bool().expect("bool"))
        .collect();
    assert_eq!(replayable, expected_replayable);
}

// ---------------------------------------------------------------------------
// 2. "replays profitable requests and preserves options across repeated refreshes"
// ---------------------------------------------------------------------------

#[tokio::test]
async fn replays_profitable_requests_and_preserves_options_across_repeated_refreshes() {
    let adaptive = golden_model("adaptive");
    let h = harness(HarnessOptions::default());
    let original_signal = tokio_util::sync::CancellationToken::new();
    let options = ModelsSimpleStreamOptions {
        base: SimpleStreamOptions {
            base: StreamOptions {
                session_id: Some("s".to_string()),
                signal: Some(original_signal.clone()),
                ..Default::default()
            },
            reasoning: Some(ThinkingLevel::High),
            ..Default::default()
        },
        transforms: kiana_ai::models::ModelsRequestTransforms {
            transform_headers: Some(Arc::new(|headers| Box::pin(async move { headers }))),
        },
        signal_reason: None,
    };
    h.warmer
        .start(request(adaptive.clone(), options), current());
    advance(&h.timer, 270_000).await;

    {
        let recorded = h
            .models
            .calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert_eq!(recorded.len(), 1);
        let call = &recorded[0];
        assert_eq!(call.model.id, adaptive.id);
        assert_eq!(call.options.base.reasoning, Some(ThinkingLevel::High));
        assert_eq!(call.options.base.base.session_id.as_deref(), Some("s"));
        assert_eq!(call.options.base.base.max_tokens, Some(1));
        assert_eq!(call.options.base.base.max_retries, Some(0));
        assert!(call.options.transforms.transform_headers.is_some());
        let signal = call
            .options
            .base
            .base
            .signal
            .as_ref()
            .expect("refresh gets its own signal");
        assert!(!signal.is_cancelled());
        assert!(!original_signal.is_cancelled());
    }

    {
        let events = h
            .events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert_eq!(events[0].event_type(), "cache_warming_decision");
        assert_eq!(events[0].continuation_probability, 1.0);
        assert_eq!(events[0].action, CacheWarmingAction::Warm);
        assert!((events[0].miss_cost - 0.575).abs() < 1e-12);
        assert!((events[0].warm_cost - 0.050025).abs() < 1e-12);
    }

    {
        let session = h
            .session
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let appended = session.appended();
        assert_eq!(appended.len(), 1);
        assert_eq!(appended[0].kind, "cache_warm");
        assert_eq!(appended[0].provider, adaptive.provider);
        assert_eq!(appended[0].model, adaptive.id);
        assert_eq!(appended[0].usage, warm_usage());
        assert_eq!(appended[0].note, None);
    }

    {
        // TS：`expect(warmedEntries).toEqual([appendUsage.mock.results[0]?.value])`
        // —— onWarmed 收到的必须正是 appendUsage 返回的那个条目。
        let expected_entry = {
            let session = h
                .session
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            session.entries[0].clone()
        };
        let warmed = h
            .warmed
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert_eq!(warmed.as_slice(), std::slice::from_ref(&expected_entry));
        assert_eq!(warmed[0].kind_name, "cache_warm");
        assert_eq!(warmed[0].provider, adaptive.provider);
        assert_eq!(warmed[0].model, adaptive.id);
        assert_eq!(warmed[0].usage, warm_usage());
    }

    advance(&h.timer, 270_000).await;
    assert_eq!(calls(&h).len(), 2);
    h.warmer.cancel();
}

// ---------------------------------------------------------------------------
// 3. "does not issue refreshes after their safe deadline"
// ---------------------------------------------------------------------------

#[tokio::test]
async fn does_not_issue_refreshes_after_their_safe_deadline() {
    let h = harness(HarnessOptions::default());
    h.warmer.start(
        request(golden_model("adaptive"), empty_options()),
        current(),
    );
    // 5 分钟缓存排期在 4m30s，保留 30s 过期余量的一半（15s）。模拟睡眠导致的迟到。
    h.timer.set_now(285_001);
    h.timer.clear_all();
    h.warmer.refresh_current_for_test().await;

    assert_eq!(calls(&h).len(), 0);
    let status = h.warmer.status();
    assert_eq!(status.state, CacheWarmingState::Inactive);
    assert_eq!(
        status.reason.as_deref(),
        Some("cache refresh deadline missed")
    );
}

// ---------------------------------------------------------------------------
// 4. "rechecks the deadline after an extension decision"
// ---------------------------------------------------------------------------

#[tokio::test]
async fn rechecks_the_deadline_after_an_extension_decision() {
    let timer = ManualTimer::new(0);
    let decide_timer = Arc::clone(&timer);
    let h = harness(HarnessOptions {
        timer: Some(Arc::clone(&timer)),
        decide: Some(Arc::new(move |_event| {
            let timer = Arc::clone(&decide_timer);
            Box::pin(async move {
                timer.set_now(285_001);
                CacheWarmingAction::Warm
            })
        })),
        ..HarnessOptions::default()
    });
    h.warmer.start(
        request(golden_model("adaptive"), empty_options()),
        current(),
    );
    h.timer.clear_all();
    h.warmer.refresh_current_for_test().await;

    assert_eq!(calls(&h).len(), 0);
    assert_eq!(h.timer.now(), 285_001);
    let status = h.warmer.status();
    assert_eq!(status.state, CacheWarmingState::Inactive);
    assert_eq!(
        status.reason.as_deref(),
        Some("cache refresh deadline missed")
    );
}

// ---------------------------------------------------------------------------
// 5. "applies economic decisions and extension overrides"
// ---------------------------------------------------------------------------

#[tokio::test]
async fn applies_economic_decisions_and_extension_overrides() {
    // 不划算：5000 token。
    let unprofitable = harness(HarnessOptions {
        branch: branch_with_prompt(5_000),
        ..HarnessOptions::default()
    });
    unprofitable.warmer.start(
        request(golden_model("adaptive"), empty_options()),
        current(),
    );
    advance(&unprofitable.timer, 270_000).await;
    assert_eq!(calls(&unprofitable).len(), 0);
    let status = unprofitable.warmer.status();
    assert_eq!(status.state, CacheWarmingState::Inactive);
    let decision = status.decision.as_ref().expect("decision");
    assert_eq!(decision.action, CacheWarmingAction::Stop);
    assert!(decision.economics_available);
    assert_eq!(status.extension_override, Some(false));

    // 扩展强制预热。
    let forced = harness(HarnessOptions {
        branch: branch_with_prompt(5_000),
        decide: Some(Arc::new(|_event| {
            Box::pin(async { CacheWarmingAction::Warm })
        })),
        ..HarnessOptions::default()
    });
    forced.warmer.start(
        request(golden_model("adaptive"), empty_options()),
        current(),
    );
    advance(&forced.timer, 270_000).await;
    assert_eq!(calls(&forced).len(), 1);
    {
        let warmed = forced
            .warmed
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert_eq!(warmed[0].note.as_deref(), Some("extension override"));
    }
    forced.warmer.cancel();

    // 扩展否决。
    let vetoed = harness(HarnessOptions {
        decide: Some(Arc::new(|_event| {
            Box::pin(async { CacheWarmingAction::Stop })
        })),
        ..HarnessOptions::default()
    });
    vetoed.warmer.start(
        request(golden_model("adaptive"), empty_options()),
        current(),
    );
    advance(&vetoed.timer, 270_000).await;
    assert_eq!(calls(&vetoed).len(), 0);
    let status = vetoed.warmer.status();
    assert_eq!(status.state, CacheWarmingState::Inactive);
    assert_eq!(status.extension_override, Some(true));

    // 经济性不可用。
    let unavailable = harness(HarnessOptions {
        branch: branch_with_prompt(0),
        ..HarnessOptions::default()
    });
    unavailable.warmer.start(
        request(golden_model("adaptive"), empty_options()),
        current(),
    );
    let status = unavailable.warmer.status();
    assert_eq!(status.state, CacheWarmingState::Inactive);
    assert_eq!(
        status.reason.as_deref(),
        Some("cache economics unavailable")
    );
    advance(&unavailable.timer, 270_000).await;
    assert_eq!(calls(&unavailable).len(), 0);
}

// ---------------------------------------------------------------------------
// 6. "stops for unsupported requests, context changes, and mode changes"
// ---------------------------------------------------------------------------

#[tokio::test]
async fn stops_for_unsupported_requests_context_changes_and_mode_changes() {
    let unsupported = harness(HarnessOptions::default());
    set_mode(&unsupported, CacheWarmingMode::Off);
    unsupported.warmer.start(
        request(golden_model("adaptive"), empty_options()),
        current(),
    );
    assert_eq!(
        unsupported.warmer.status().reason.as_deref(),
        Some("cache warming disabled")
    );
    set_mode(&unsupported, CacheWarmingMode::Idle);
    unsupported
        .warmer
        .start(request(golden_model("unknown"), empty_options()), current());
    assert_eq!(
        unsupported.warmer.status().reason.as_deref(),
        Some("cache lifetime unavailable")
    );
    unsupported.warmer.start(
        request(
            golden_model("budget"),
            ModelsSimpleStreamOptions {
                base: SimpleStreamOptions {
                    reasoning: Some(ThinkingLevel::High),
                    ..Default::default()
                },
                ..Default::default()
            },
        ),
        current(),
    );
    assert_eq!(
        unsupported.warmer.status().reason.as_deref(),
        Some("request cannot be replayed safely")
    );

    let still_current = Arc::new(AtomicBool::new(true));
    let flag = Arc::clone(&still_current);
    unsupported.warmer.start(
        request(golden_model("adaptive"), empty_options()),
        Arc::new(move || flag.load(Ordering::SeqCst)),
    );
    still_current.store(false, Ordering::SeqCst);
    assert_eq!(
        unsupported.warmer.status().reason.as_deref(),
        Some("conversation context changed")
    );
    advance(&unsupported.timer, 270_000).await;
    assert_eq!(calls(&unsupported).len(), 0);

    unsupported.warmer.start(
        request(golden_model("adaptive"), empty_options()),
        current(),
    );
    set_mode(&unsupported, CacheWarmingMode::Off);
    advance(&unsupported.timer, 270_000).await;
    assert_eq!(calls(&unsupported).len(), 0);

    let streaming = harness(HarnessOptions {
        mode: CacheWarmingMode::Streaming,
        branch: branch_with_prompt(400_000),
        ..HarnessOptions::default()
    });
    streaming.warmer.start(
        request(golden_model("adaptive"), empty_options()),
        current(),
    );
    streaming.warmer.on_agent_settled();
    assert_eq!(
        streaming.warmer.status().reason.as_deref(),
        Some("agent run settled")
    );
}

fn set_mode(h: &Harness, mode: CacheWarmingMode) {
    *h.mode
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = mode;
}

// ---------------------------------------------------------------------------
// 7. "aborts replaced requests and does not record failed refreshes"
// ---------------------------------------------------------------------------

#[tokio::test]
async fn aborts_replaced_requests_and_does_not_record_failed_refreshes() {
    let gate = Arc::new(tokio::sync::Notify::new());
    let pending = harness(HarnessOptions {
        gate: Some(Arc::clone(&gate)),
        ..HarnessOptions::default()
    });
    pending.warmer.start(
        request(golden_model("adaptive"), empty_options()),
        current(),
    );
    let mut futures = pending.timer.advance(270_000);
    assert_eq!(futures.len(), 1);
    let handle = tokio::spawn(futures.remove(0));
    wait_for_calls(&pending, 1).await;

    pending.warmer.start(
        request(golden_model("adaptive"), empty_options()),
        current(),
    );
    {
        let recorded = pending
            .models
            .calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert!(
            recorded[0]
                .options
                .base
                .base
                .signal
                .as_ref()
                .expect("signal")
                .is_cancelled()
        );
    }
    gate.notify_one();
    handle.await.expect("refresh completes");
    pending.warmer.cancel();
    advance(&pending.timer, 600_000).await;
    assert_eq!(calls(&pending).len(), 1);

    let failed = harness(HarnessOptions {
        result: Arc::new(|model| response(model, StopReason::Error)),
        ..HarnessOptions::default()
    });
    failed.warmer.start(
        request(golden_model("adaptive"), empty_options()),
        current(),
    );
    advance(&failed.timer, 270_000).await;
    {
        let session = failed
            .session
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert!(session.appended().is_empty());
    }
    failed.warmer.cancel();
}

async fn wait_for_calls(h: &Harness, count: usize) {
    for _ in 0..10_000 {
        if calls(h).len() >= count {
            return;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    panic!("refresh never reached streamSimple");
}

// ---------------------------------------------------------------------------
// 8. "formats status and usage entries"
// ---------------------------------------------------------------------------

#[test]
fn formats_status_and_usage_entries() {
    let fixture = golden();
    for sample in fixture["statuses"].as_array().expect("statuses") {
        let status = status_from(&sample["status"]);
        let now = sample["now"].as_i64().expect("now");
        assert_eq!(
            format_cache_warming_status(&status, now),
            sample["formatted"].as_str().expect("formatted"),
            "{}",
            sample["name"]
        );
    }
    for sample in fixture["usages"].as_array().expect("usages") {
        let entry: UsageEntry =
            serde_json::from_value(sample["entry"].clone()).expect("usage entry");
        assert_eq!(
            format_cache_warming_usage(&entry),
            sample["formatted"].as_str().expect("formatted")
        );
    }
}

fn status_from(value: &Value) -> CacheWarmingStatus {
    CacheWarmingStatus {
        state: match value["state"].as_str().expect("state") {
            "inactive" => CacheWarmingState::Inactive,
            "scheduled" => CacheWarmingState::Scheduled,
            "refreshing" => CacheWarmingState::Refreshing,
            other => panic!("unknown state {other}"),
        },
        reason: value["reason"].as_str().map(str::to_string),
        next_warm_at: value["nextWarmAt"].as_i64(),
        decision: value["decision"].as_object().map(decision_from),
        extension_override: value["extensionOverride"].as_bool(),
    }
}

fn decision_from(value: &serde_json::Map<String, Value>) -> CacheWarmingDecision {
    CacheWarmingDecision {
        phase: match value["phase"].as_str().expect("phase") {
            "streaming" => CacheWarmingPhase::Streaming,
            "idle" => CacheWarmingPhase::Idle,
            other => panic!("unknown phase {other}"),
        },
        warm_cost: value["warmCost"].as_f64().expect("warmCost"),
        miss_cost: value["missCost"].as_f64().expect("missCost"),
        continuation_probability: value["continuationProbability"]
            .as_f64()
            .expect("continuationProbability"),
        expected_savings: value["expectedSavings"].as_f64().expect("expectedSavings"),
        economics_available: value["economicsAvailable"]
            .as_bool()
            .expect("economicsAvailable"),
        action: match value["action"].as_str().expect("action") {
            "warm" => CacheWarmingAction::Warm,
            "stop" => CacheWarmingAction::Stop,
            other => panic!("unknown action {other}"),
        },
    }
}

// ---------------------------------------------------------------------------
// 9. "ExtensionRunner.emitCacheWarmingDecision 使用最后一个扩展覆盖"
// ---------------------------------------------------------------------------

// 该用例的断言对象是 `kiana-extensions` 的 `ExtensionRunner`（本单元只定义决策回调
// `CacheWarmingDecideFn`，见 spec §2.28.7）。适配与「最后一个扩展覆盖」的语义属于
// `kiana-extensions` 单元；本单元的 `decide` 注入行为由上面的用例覆盖。

// ---------------------------------------------------------------------------
// 附加：手动定时器自身的哨兵
// ---------------------------------------------------------------------------

#[test]
fn manual_timer_starts_and_clears() {
    let timer = ManualTimer::new(0);
    assert_eq!(timer.now(), 0);
    assert_eq!(timer.pending(), 0);
    let _handle = CacheWarmerTimer::set_timeout(&*timer, 10, Box::new(|| Box::pin(async {})));
    assert_eq!(timer.pending(), 1);
    let futures = timer.advance(10);
    assert_eq!(futures.len(), 1);
}

// ---------------------------------------------------------------------------
// 附加：真实 SessionManager 的 trait 适配（审查修复）
// ---------------------------------------------------------------------------

#[test]
fn real_session_manager_satisfies_the_cache_warmer_session_seam() {
    let mut manager = SessionManager::in_memory("/tmp/kiana-config-warmer", None, None)
        .expect("in-memory session manager");
    assert!(CacheWarmerSession::get_branch(&manager).is_empty());

    let entry = CacheWarmerSession::append_usage(
        &mut manager,
        "cache_warm",
        "anthropic",
        "claude-sonnet-4-5",
        &warm_usage(),
        Some("extension override"),
    )
    .expect("appendUsage succeeds in memory");
    assert_eq!(entry.kind_name, "cache_warm");
    assert_eq!(entry.provider, "anthropic");
    assert_eq!(entry.model, "claude-sonnet-4-5");
    assert_eq!(entry.usage, warm_usage());
    assert_eq!(entry.note.as_deref(), Some("extension override"));

    let branch = CacheWarmerSession::get_branch(&manager);
    assert_eq!(branch.len(), 1);
    assert_eq!(branch[0].kind(), "usage");

    // 生产装配：`Arc<Mutex<SessionManager>>` 可用作 `SharedCacheWarmerSession`。
    let shared: SharedCacheWarmerSession = Arc::new(Mutex::new(manager));
    let branch = shared
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get_branch();
    assert_eq!(branch.len(), 1);
}
