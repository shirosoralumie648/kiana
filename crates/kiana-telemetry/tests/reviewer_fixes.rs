//! 评审发现的回归测试（由 TS 参考实现录制，见 `fixtures/cursor_fidelity.json`）。
//!
//! 覆盖：
//!
//! 1. **cursor 的浮点往返**（review 发现 #3）：`findTraces` 的续页判据是
//!    `first.startedMs === after[0]`（严格相等），`JSON.stringify` 输出的是最短往返表示，
//!    因此解码必须**精确**还原同一个 `f64`。`kiana_json::parse`（serde_json 快速路径）在部分
//!    数值上差 1 ULP，会让 cursor 永远匹配不上（重复返回同一页 / 丢 trace）。
//! 2. **cursor 的溢出字面量**（review 发现 #4）：`JSON.parse("1e400")` 是 `Infinity`，不是错误。
//! 3. **`start_span_try` 的 Result 展平**（review 发现 #5）：回调返回 `Err` 时按「回调抛出」
//!    处理 —— 错误原样返回，span 记录 `{ status: "error", error: { name: error.name, message } }`
//!    （`name: None` 的 rejection 值记裸 `{ status: "error" }`）。
//!    同一文件也固定 `start_span_typed` 的 TS 对齐语义（回调返回的 `Ok(Err(_))` 是**值**）。
//! 4. **NaN `startedMs` 的排序**（review 发现 #2/#7）：`findTraces` 对含 NaN 起点的
//!    trace 必须给出与 V8 TimSort 完全相同的顺序（此前是 Rust 稳定排序，顺序不同）。
//! 5. **丢弃 future 不结算**（评审 major）：TS 的 `finally` 只在 `await callback(span)` 完成时
//!    运行，被丢弃 promise 的 span 永久 `settled = false`、对 `findTraces` / `getTrace` 不可见。
//! 6. **自动 error 状态的 `name`**（评审 major）：`Error` 的 `name` 被复制，非 `Error` 的
//!    rejection 值只写 `{ status: "error" }`。
//!
//! fixture：`fixtures/cursor_fidelity.json`（node 26 跑 `upstream-reference` 录制，trace id 的 scope
//! 前缀替换成 `<scope>`）。

use std::sync::{Arc, Mutex};

use kiana_telemetry::{
    Clock, InMemoryTelemetryContext, SpanOptions, SpanStatus, StatusError, TelemetryContextExt,
    TelemetryError, TraceQuery,
};
use serde_json::Value;

fn fixture() -> Value {
    serde_json::from_str(include_str!("fixtures/cursor_fidelity.json")).expect("cursor_fidelity")
}

struct ScriptedClock {
    now: Mutex<f64>,
    monotonic: Mutex<f64>,
}

impl ScriptedClock {
    fn new() -> Arc<ScriptedClock> {
        Arc::new(ScriptedClock {
            now: Mutex::new(0.0),
            monotonic: Mutex::new(0.0),
        })
    }

    fn set(&self, value: f64) {
        *self.now.lock().expect("clock") = value;
        *self.monotonic.lock().expect("clock") = 0.0;
    }
}

impl Clock for ScriptedClock {
    fn now_ms(&self) -> f64 {
        *self.now.lock().expect("clock")
    }

    fn now_monotonic_ms(&self) -> f64 {
        *self.monotonic.lock().expect("clock")
    }
}

fn marker(text: &str) -> Option<f64> {
    match text {
        "__NaN" => Some(f64::NAN),
        "__Infinity" => Some(f64::INFINITY),
        "__-Infinity" => Some(f64::NEG_INFINITY),
        _ => None,
    }
}

/// 用脚本时钟依次记录 span；每条 `(name, startedMs)` 直接成为一次 `Date.now()` 的值。
async fn store(entries: &[(&str, f64)]) -> (InMemoryTelemetryContext, Arc<ScriptedClock>) {
    let clock = ScriptedClock::new();
    let telemetry = InMemoryTelemetryContext::with_clock(Arc::clone(&clock) as Arc<dyn Clock>);
    for (name, start) in entries {
        clock.set(*start);
        telemetry
            .start_span_typed(
                SpanOptions {
                    name: (*name).to_string(),
                    attributes: None,
                    links: None,
                },
                |_| Box::pin(async move {}),
            )
            .await
            .expect("span");
    }
    clock.set(0.0);
    (telemetry, clock)
}

fn names(page: &kiana_telemetry::TracePage) -> Vec<String> {
    page.traces
        .iter()
        .filter_map(|trace| trace.spans.first().map(|span| span.name.clone()))
        .collect()
}

fn starts(page: &kiana_telemetry::TracePage) -> Vec<f64> {
    page.traces
        .iter()
        .filter_map(|trace| trace.spans.first().map(|span| span.started_ms))
        .collect()
}

fn expected_names(value: &Value, key: &str) -> Vec<String> {
    value[key]
        .as_array()
        .expect("names")
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect()
}

/// fixture 里的 `started` 是**字符串**：`JSON.stringify` 的文本形式（含 `__NaN` 标记）。
///
/// 不能用 JSON 数字 —— `serde_json` 的快速浮点路径会把它读成差 1 ULP 的值，测试自身就
/// 复现了被修复的 bug（见 review 发现 #3）。
fn expected_starts(value: &Value, key: &str) -> Vec<f64> {
    value[key]
        .as_array()
        .expect("started")
        .iter()
        .map(|item| match item {
            Value::String(text) => match marker(text) {
                Some(number) => number,
                None => text
                    .parse::<f64>()
                    .unwrap_or_else(|_| panic!("started {text}")),
            },
            other => other.as_f64().expect("started"),
        })
        .collect()
}

fn assert_page(page: &kiana_telemetry::TracePage, expected: &Value) {
    assert_eq!(names(page), expected_names(expected, "names"));
    let actual_starts = starts(page);
    let wanted_starts = expected_starts(expected, "started");
    assert_eq!(actual_starts.len(), wanted_starts.len());
    for (index, (actual, wanted)) in actual_starts.iter().zip(&wanted_starts).enumerate() {
        assert!(
            (actual.is_nan() && wanted.is_nan()) || actual.to_bits() == wanted.to_bits(),
            "startedMs[{index}]: {actual:?} != {wanted:?}"
        );
    }
}

fn assert_cursor(actual: Option<&str>, expected: &Value, scope: &str) {
    match expected {
        Value::Null => assert_eq!(actual, None),
        Value::String(wanted) => {
            let wanted = wanted.replace("<scope>", scope);
            assert_eq!(actual, Some(wanted.as_str()));
        }
        other => panic!("unexpected cursor expectation {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// #3：小数毫秒的续页必须精确往返
// ---------------------------------------------------------------------------

#[tokio::test]
async fn fractional_started_ms_paginates_exactly_like_typescript() {
    let fixture = fixture();
    let (telemetry, _clock) = store(&[
        ("a", 1_700_000_000_000.657_5_f64),
        ("b", 1_700_000_000_000.657_5_f64),
        ("c", 1_700_000_000_001.5_f64),
        ("d", 1_700_000_000_002.25_f64),
    ])
    .await;
    let scope = telemetry.scope().to_string();

    let all = telemetry.find_traces(None).await.expect("all");
    assert_page(&all, &fixture["fractionalAll"]);

    // 逐页 1 条：一共 4 页，顺序 d, c, a, b（a/b 共享起点，id 平局）。
    let pages = fixture["fractionalPages"].as_array().expect("pages");
    let mut cursor: Option<String> = None;
    for (index, expected) in pages.iter().enumerate() {
        let page = telemetry
            .find_traces(Some(TraceQuery {
                limit: Some(1),
                cursor: cursor.clone(),
                ..TraceQuery::default()
            }))
            .await
            .unwrap_or_else(|error| panic!("page {index}: {error}"));
        assert_page(&page, expected);
        assert_cursor(page.cursor.as_deref(), &expected["cursor"], &scope);
        cursor = page.cursor.clone();
    }
    assert_eq!(cursor, None, "最后一页没有 cursor");

    // limit 2 的两页。
    let expected = fixture["fractionalLimit2"].as_array().expect("limit2");
    let first = telemetry
        .find_traces(Some(TraceQuery {
            limit: Some(2),
            ..TraceQuery::default()
        }))
        .await
        .expect("limit 2 page 1");
    assert_page(&first, &expected[0]);
    assert_cursor(first.cursor.as_deref(), &expected[0]["cursor"], &scope);
    let second = telemetry
        .find_traces(Some(TraceQuery {
            limit: Some(2),
            cursor: first.cursor,
            ..TraceQuery::default()
        }))
        .await
        .expect("limit 2 page 2");
    assert_page(&second, &expected[1]);
    assert_cursor(second.cursor.as_deref(), &expected[1]["cursor"], &scope);
}

/// 严格相等判据的直接断言：`[1700000000000.6575, ""]` 通过 `<` 排除 d/c，通过 `===` 命中 a/b。
///
/// 解码若差 1 ULP，`===` 不成立而 `<` 也不成立，三个 trace 全部被过滤 —— 空页。
#[tokio::test]
async fn cursor_threshold_compares_started_ms_exactly() {
    let fixture = fixture();
    let (telemetry, _clock) = store(&[
        ("a", 1_700_000_000_000.657_5_f64),
        ("b", 1_700_000_000_000.657_5_f64),
        ("c", 1_700_000_000_001.5_f64),
        ("d", 1_700_000_000_002.25_f64),
    ])
    .await;
    let literals = &fixture["cursorLiterals"];
    for (cursor, expected) in literals.as_object().expect("cursorLiterals") {
        let page = telemetry
            .find_traces(Some(TraceQuery {
                limit: Some(100),
                cursor: Some(cursor.clone()),
                ..TraceQuery::default()
            }))
            .await
            .unwrap_or_else(|error| panic!("cursor {cursor}: {error}"));
        assert_eq!(
            names(&page),
            expected_names(expected, "names"),
            "cursor {cursor}"
        );
        assert_eq!(page.cursor, None, "cursor {cursor}");
    }
}

// ---------------------------------------------------------------------------
// #4：溢出字面量 → Infinity（JSON.parse 语义）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cursor_overflow_matches_json_parse_infinity() {
    let (telemetry, _clock) = store(&[
        ("a", 1_700_000_000_000.657_5_f64),
        ("b", 1_700_000_000_001.5_f64),
    ])
    .await;

    // `[1e400, ""]` 的 after[0] 是 Infinity ⇒ `startedMs < Infinity` 恒真 ⇒ 全部保留。
    let page = telemetry
        .find_traces(Some(TraceQuery {
            limit: Some(100),
            cursor: Some("[1e400,\"\"]".to_string()),
            ..TraceQuery::default()
        }))
        .await
        .expect("1e400 cursor");
    assert_eq!(names(&page), vec!["b".to_string(), "a".to_string()]);

    // `[-1e400, ""]` 是 -Infinity ⇒ 全部排除。
    let page = telemetry
        .find_traces(Some(TraceQuery {
            limit: Some(100),
            cursor: Some("[-1e400,\"\"]".to_string()),
            ..TraceQuery::default()
        }))
        .await
        .expect("-1e400 cursor");
    assert_eq!(names(&page), Vec::<String>::new());

    // 下溢保持 0：`[1e-400, ""]` 与 `[0, ""]` 等价（这里没有 0 起点 ⇒ 空页）。
    let page = telemetry
        .find_traces(Some(TraceQuery {
            limit: Some(100),
            cursor: Some("[1e-400,\"\"]".to_string()),
            ..TraceQuery::default()
        }))
        .await
        .expect("1e-400 cursor");
    assert_eq!(names(&page), Vec::<String>::new());
}

/// 非法 JSON 仍是错误（`JSON.parse` 抛 `SyntaxError`）；宽松接受（`01`、`+1`、`Infinity`
/// 字面量、单引号、尾逗号）都不允许。
#[tokio::test]
async fn cursor_rejects_non_json_literals() {
    let (telemetry, _clock) = store(&[("a", 1.0)]).await;
    for cursor in [
        "01",
        "+1",
        "[Infinity,\"\"]",
        "[NaN,\"\"]",
        "'a'",
        "[\"a\",]",
        "{a:1}",
        "[1 2]",
        "",
        "1 2",
    ] {
        let result = telemetry
            .find_traces(Some(TraceQuery {
                limit: Some(1),
                cursor: Some(cursor.to_string()),
                ..TraceQuery::default()
            }))
            .await;
        assert!(result.is_err(), "cursor {cursor:?} should be a parse error");
    }
}

// ---------------------------------------------------------------------------
// #5：Result 展平
// ---------------------------------------------------------------------------

/// `start_span_try` 把回调的 `Err` 当作 throw：错误原样返回，span 标记 error 并结算。
#[tokio::test]
async fn start_span_try_propagates_callback_error_and_marks_span() {
    let (telemetry, _clock) = store(&[]).await;
    let error = telemetry
        .start_span_try(span_options("expected-failure"), |_| {
            Box::pin(async move { Err::<(), _>(TelemetryError::new("boom")) })
        })
        .await
        .expect_err("callback error must reject");
    assert_eq!(error.message, "boom");

    let spans = telemetry.get_spans();
    let recorded = spans
        .iter()
        .find(|span| span.name == "expected-failure")
        .expect("recorded span");
    assert_eq!(
        recorded.status,
        SpanStatus::Error {
            error: Some(StatusError {
                name: "Error".to_string(),
                message: "boom".to_string(),
            }),
        }
    );
    assert!(recorded.settled, "span must be settled");
    assert!(recorded.end_sequence.is_some());
    let stored = telemetry
        .get_trace(&recorded.context["kiana-memory-trace"])
        .await
        .expect("stored trace");
    let span = stored
        .spans
        .iter()
        .find(|span| span.name == "expected-failure")
        .expect("stored span");
    assert!(span.ended_ms.is_finite(), "endedMs must be recorded");
}

/// 回调已显式设置状态时，`start_span_try` 不覆盖它（TS 的 `explicitStatus`）。
#[tokio::test]
async fn start_span_try_keeps_explicit_status() {
    let (telemetry, _clock) = store(&[]).await;
    let error = telemetry
        .start_span_try(span_options("explicit"), |span| {
            span.set_status(SpanStatus::Error {
                error: Some(StatusError {
                    name: "Expected".to_string(),
                    message: "first".to_string(),
                }),
            });
            Box::pin(async move { Err::<(), _>(TelemetryError::new("boom")) })
        })
        .await
        .expect_err("callback error must reject");
    assert_eq!(error.message, "boom");
    let spans = telemetry.get_spans();
    let recorded = spans
        .iter()
        .find(|span| span.name == "explicit")
        .expect("recorded span");
    assert_eq!(
        recorded.status,
        SpanStatus::Error {
            error: Some(StatusError {
                name: "Expected".to_string(),
                message: "first".to_string(),
            }),
        }
    );
}

/// `start_span_try` 的 `Ok` 路径照常返回回调值。
#[tokio::test]
async fn start_span_try_returns_callback_value() {
    let (telemetry, _clock) = store(&[]).await;
    let value = telemetry
        .start_span_try(span_options("success"), |_| {
            Box::pin(async move { Ok::<_, TelemetryError>(7) })
        })
        .await
        .expect("ok");
    assert_eq!(value, 7);
    let spans = telemetry.get_spans();
    let recorded = spans
        .iter()
        .find(|span| span.name == "success")
        .expect("recorded span");
    assert_eq!(recorded.status, SpanStatus::Ok);
}

/// `start_span_typed` 保持 TS 的 `Promise<T>` 语义：回调**返回**的 `Result` 是值本身，
/// 不是拒绝。要「失败即拒绝」请用 `start_span_try`。
#[tokio::test]
async fn start_span_typed_treats_returned_result_as_value() {
    let (telemetry, _clock) = store(&[]).await;
    let value: Result<(), TelemetryError> = telemetry
        .start_span_typed(span_options("returned-result"), |_| {
            Box::pin(async move { Err(TelemetryError::new("boom")) })
        })
        .await
        .expect("a returned Result is a value, not a rejection");
    assert_eq!(value.expect_err("inner error").message, "boom");
    let spans = telemetry.get_spans();
    let recorded = spans
        .iter()
        .find(|span| span.name == "returned-result")
        .expect("recorded span");
    assert_eq!(recorded.status, SpanStatus::Ok);
    assert!(recorded.settled);
}

fn span_options(name: &str) -> SpanOptions {
    SpanOptions {
        name: name.to_string(),
        attributes: None,
        links: None,
    }
}

// ---------------------------------------------------------------------------
// #6：abandoned span 保持未结算（与 TS 的 `finally` 对齐）
// ---------------------------------------------------------------------------

/// 在回调 await 期间丢弃 future：TS 的 `finally` **不会**运行 —— 它只在 `await callback(span)`
/// 完成时执行。被丢弃 promise 的 inner work 永不完成 ⇒ span 永久 `settled = false`
/// （`endSequence` undefined），被 `storedSpans()` 的 `settled` 过滤排除在
/// `findTraces` / `getTrace` 之外，只有 `getSpans()` 快照还能看到它。
///
/// Node 26 实测（同样形状）：`after 20ms spans: [{settled:false}]`、`findTraces: 0`，
/// 只有 resolve gate 并 `await active` 之后才变成 `[{settled:true,endSequence:1}]`、`findTraces: 1`。
#[tokio::test]
async fn dropping_mid_await_leaves_the_span_unsettled_like_typescript() {
    use kiana_telemetry::testing::poll_once;

    let (telemetry, clock) = store(&[]).await;
    let admitted = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let admitted_in_callback = Arc::clone(&admitted);
    let mut future = Box::pin(
        telemetry.start_span_typed(span_options("abandoned"), move |_| {
            admitted_in_callback.store(true, std::sync::atomic::Ordering::SeqCst);
            Box::pin(async move {
                // 永不就绪：future 只可能在外部被 drop。
                std::future::pending::<()>().await;
            })
        }),
    );
    // 首次 poll：创建 span、进入回调，然后返回 Pending。
    assert!(poll_once(&mut future).is_pending());
    assert!(admitted.load(std::sync::atomic::Ordering::SeqCst));
    drop(future);
    clock.set(20_000.0);

    // 已记录但未结算：`getSpans()` 看得到，没有 endedMs 的替代物（`endSequence`）也没有。
    let spans = telemetry.get_spans();
    let span = spans
        .iter()
        .find(|span| span.name == "abandoned")
        .expect("recorded span is visible in getSpans");
    assert!(
        !span.settled,
        "an abandoned callback never runs TS's finally"
    );
    assert_eq!(span.end_sequence, None);

    // 未结算 ⇒ 被 `storedSpans().filter(settled)` 过滤掉。
    let page = telemetry.find_traces(None).await.expect("traces");
    assert!(
        page.traces.is_empty(),
        "unsettled spans stay invisible to findTraces"
    );
    let trace_id = span.context["kiana-memory-trace"].clone();
    assert!(
        telemetry.get_trace(&trace_id).await.is_none(),
        "unsettled spans stay invisible to getTrace"
    );

    // 只结算一次：正常完成的 span 拿到第一个 endSequence（被丢弃的那个没有占用序号）。
    telemetry
        .start_span_typed(span_options("completed"), |_| Box::pin(async move {}))
        .await
        .expect("span");
    let completed = telemetry
        .get_spans()
        .into_iter()
        .find(|span| span.name == "completed")
        .expect("completed span");
    assert_eq!(completed.end_sequence, Some(1.0));
    let abandoned = telemetry
        .get_spans()
        .into_iter()
        .find(|span| span.name == "abandoned")
        .expect("abandoned span");
    assert!(
        !abandoned.settled && abandoned.end_sequence.is_none(),
        "the abandoned span stays unsettled forever"
    );
}

/// 取消后 resolve 同一回调的 gate 不会「补结算」：TS 里 inner work 完成时 `finally` 才会运行，
/// 而 Rust 的 future 已经被 drop、回调不可能再推进。两者的可观察结果都是「这条 trace 不出现」。
#[tokio::test]
async fn settling_after_cancellation_is_impossible_in_both_models() {
    use kiana_telemetry::testing::poll_once;

    let (telemetry, _clock) = store(&[]).await;
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let gate_for_cb = Arc::clone(&gate);
    let mut future = Box::pin(
        telemetry.start_span_typed(span_options("cancelled"), move |_| {
            let gate = Arc::clone(&gate_for_cb);
            Box::pin(async move {
                let _permit = gate.acquire().await.expect("semaphore");
            })
        }),
    );
    assert!(poll_once(&mut future).is_pending());
    drop(future);
    gate.add_permits(1);

    assert!(
        telemetry
            .find_traces(None)
            .await
            .expect("traces")
            .traces
            .is_empty(),
        "a cancelled span is never stored"
    );
    let spans = telemetry.get_spans();
    assert_eq!(spans.len(), 1);
    assert!(!spans[0].settled);
}

// ---------------------------------------------------------------------------
// #8：自动 error 状态映射（TS 的 `error instanceof Error` 分支）
// ---------------------------------------------------------------------------

/// TS: `error instanceof Error ? { status: "error", error: { name: error.name, message } }
/// : { status: "error" }`。
///
/// Node 26 实测：`TypeError` → `{"status":"error","error":{"name":"TypeError","message":"boom"}}`；
/// 自定义 `name = "MyErr"` 原样保留；普通对象/字符串/数字/`undefined`/`null` → `{"status":"error"}`。
#[tokio::test]
async fn rejection_status_copies_the_javascript_error_name() {
    let (telemetry, _clock) = store(&[]).await;

    telemetry
        .start_span_try(span_options("type-error"), |_| {
            Box::pin(async move { Err::<(), _>(TelemetryError::with_name("TypeError", "boom")) })
        })
        .await
        .expect_err("rejects");
    telemetry
        .start_span_try(span_options("custom-error"), |_| {
            Box::pin(async move { Err::<(), _>(TelemetryError::with_name("MyErr", "boom")) })
        })
        .await
        .expect_err("rejects");
    telemetry
        .start_span_try(span_options("default-error"), |_| {
            Box::pin(async move { Err::<(), _>(TelemetryError::new("boom")) })
        })
        .await
        .expect_err("rejects");
    telemetry
        .start_span_try(span_options("non-error"), |_| {
            Box::pin(async move { Err::<(), _>(TelemetryError::rejection("boom")) })
        })
        .await
        .expect_err("rejects");

    let spans = telemetry.get_spans();
    let status = |name: &str| {
        spans
            .iter()
            .find(|span| span.name == name)
            .expect("recorded span")
            .status
            .clone()
    };
    assert_eq!(
        status("type-error"),
        SpanStatus::Error {
            error: Some(StatusError {
                name: "TypeError".to_string(),
                message: "boom".to_string(),
            }),
        }
    );
    assert_eq!(
        status("custom-error"),
        SpanStatus::Error {
            error: Some(StatusError {
                name: "MyErr".to_string(),
                message: "boom".to_string(),
            }),
        }
    );
    assert_eq!(
        status("default-error"),
        SpanStatus::Error {
            error: Some(StatusError {
                name: "Error".to_string(),
                message: "boom".to_string(),
            }),
        }
    );
    // 非 Error 的 rejection 值：裸 `{ status: "error" }`，不带 `error` 详情。
    assert_eq!(status("non-error"), SpanStatus::Error { error: None });
    // 上述四条都在结算后进入 store：rejection 不影响可见性。
    assert_eq!(
        names(&telemetry.find_traces(None).await.expect("traces")).len(),
        4
    );
}

/// 显式状态优先于自动映射（TS 的 `explicitStatus`），包括非 Error rejection。
#[tokio::test]
async fn explicit_status_wins_over_the_automatic_error_mapping() {
    let (telemetry, _clock) = store(&[]).await;
    telemetry
        .start_span_try(span_options("explicit"), |span| {
            span.set_status(SpanStatus::Error {
                error: Some(StatusError {
                    name: "Expected".to_string(),
                    message: "first".to_string(),
                }),
            });
            Box::pin(async move { Err::<(), _>(TelemetryError::with_name("MyErr", "boom")) })
        })
        .await
        .expect_err("rejects");
    let spans = telemetry.get_spans();
    let recorded = spans
        .iter()
        .find(|span| span.name == "explicit")
        .expect("recorded span");
    assert_eq!(
        recorded.status,
        SpanStatus::Error {
            error: Some(StatusError {
                name: "Expected".to_string(),
                message: "first".to_string(),
            }),
        }
    );
}

// ---------------------------------------------------------------------------
// #2 / #7：NaN 起点的排序与 V8 TimSort 一致
// ---------------------------------------------------------------------------

/// 把 entries 记录成同一 trace 的子 span（`extract` 一个外部父上下文），再从 `getTrace`
/// 读回 `storedSpans` 的排序结果 —— 与 TS 脚本里的 `storedScenario` 一一对应。
async fn stored_trace_names(entries: &[(&str, f64)]) -> Vec<String> {
    let clock = ScriptedClock::new();
    let telemetry = InMemoryTelemetryContext::with_clock(Arc::clone(&clock) as Arc<dyn Clock>);
    let mut carrier = kiana_telemetry::TelemetryCarrier::new();
    carrier.insert("kiana-memory-scope".to_string(), "remote".to_string());
    carrier.insert("kiana-memory-span".to_string(), "0".to_string());
    carrier.insert("kiana-memory-trace".to_string(), "T".to_string());
    let parent = telemetry.extract(Some(&carrier));
    for (name, start) in entries {
        clock.set(*start);
        parent
            .start_span_typed(
                SpanOptions {
                    name: (*name).to_string(),
                    attributes: None,
                    links: None,
                },
                |_| Box::pin(async move {}),
            )
            .await
            .expect("span");
    }
    telemetry
        .get_trace("T")
        .await
        .expect("stored trace")
        .spans
        .into_iter()
        .map(|span| span.name)
        .collect()
}

type NanCase = (Vec<f64>, Vec<(&'static str, f64)>);

fn nan_inputs() -> Vec<NanCase> {
    // (输入起点, 用 s0..sN 命名的 entries)
    let raw: Vec<Vec<f64>> = vec![
        vec![f64::NAN, 2.0, f64::NAN, 1.0, 3.0],
        vec![4.0, 1.0, f64::NAN, 3.0, 2.0],
        vec![6.0, 5.0, f64::NAN, 4.0, 3.0, 2.0, 1.0],
        vec![5.0, f64::NAN, 5.0, 1.0],
        vec![f64::NAN, f64::NAN, 1.0],
        vec![
            3.0,
            f64::NAN,
            1.0,
            f64::NAN,
            2.0,
            5.0,
            4.0,
            f64::NAN,
            7.0,
            6.0,
            8.0,
            0.0,
        ],
        vec![f64::NAN],
        vec![1.0, f64::NAN],
    ];
    raw.into_iter()
        .map(|values| {
            let names: Vec<&'static str> = [
                "s0", "s1", "s2", "s3", "s4", "s5", "s6", "s7", "s8", "s9", "s10", "s11",
            ]
            .into_iter()
            .take(values.len())
            .collect();
            (values.clone(), names.into_iter().zip(values).collect())
        })
        .collect()
}

#[tokio::test]
async fn find_traces_nan_ordering_matches_v8_timsort() {
    let fixture = fixture();
    let cases = fixture["nanCases"].as_array().expect("nanCases");
    let inputs = nan_inputs();
    assert_eq!(cases.len(), inputs.len());
    for (case, (_values, entries)) in cases.iter().zip(&inputs) {
        let (telemetry, _clock) = store(entries).await;
        let page = telemetry.find_traces(None).await.expect("traces");
        assert_eq!(
            names(&page),
            expected_names(case, "findTraces"),
            "findTraces {}",
            case["started"]
        );
    }
}

#[tokio::test]
async fn stored_spans_nan_ordering_matches_v8_timsort() {
    let fixture = fixture();
    let cases = fixture["nanCases"].as_array().expect("nanCases");
    for (case, (_values, entries)) in cases.iter().zip(nan_inputs()) {
        assert_eq!(
            stored_trace_names(&entries).await,
            expected_names(case, "storedSpans"),
            "storedSpans {}",
            case["started"]
        );
    }
}

/// 小数起点的 `storedSpans` 顺序（`a.startedMs - b.startedMs || id`）：起点相同则按 id 升序。
#[tokio::test]
async fn stored_spans_fractional_ordering_matches_typescript() {
    let fixture = fixture();
    let expected = &fixture["fractionalStored"];
    let ordered = stored_trace_names(&[
        ("a", 1_700_000_000_000.657_5_f64),
        ("b", 1_700_000_000_000.657_5_f64),
        ("c", 1_700_000_000_001.5_f64),
        ("d", 1_700_000_000_002.25_f64),
    ])
    .await;
    assert_eq!(ordered, expected_names(expected, "names"));
}
