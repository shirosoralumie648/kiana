//! `findTraces` 的 cursor / limit 边界黄金向量（由 TS 生成，见 `cursor_pages.json`）。
//!
//! TS 的 `limit` 是 number，允许负数/小数/NaN/Infinity；冻结契约是 `Option<u32>`，所以只有
//! `0` 可达（TS 里 `limit: 0` 在有匹配时抛 `TypeError: Cannot read properties of undefined
//! (reading 'spans')`），其余负/小数/NaN 分支记录在 `remainingGaps`（见 spec §28.2 与报告）。

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use kiana_telemetry::{
    Clock, InMemoryTelemetryContext, SpanOptions, TelemetryContextExt, TraceQuery,
};
use serde_json::Value;

struct ScriptedClock {
    now: AtomicU64,
}

impl Clock for ScriptedClock {
    fn now_ms(&self) -> f64 {
        self.now.load(Ordering::SeqCst) as f64
    }

    fn now_monotonic_ms(&self) -> f64 {
        0.0
    }
}

fn fixture() -> Value {
    serde_json::from_str(include_str!("fixtures/cursor_pages.json")).expect("cursor_pages.json")
}

async fn store(names_and_starts: &[(&str, u64)]) -> (InMemoryTelemetryContext, String) {
    let clock = Arc::new(ScriptedClock {
        now: AtomicU64::new(0),
    });
    let telemetry = InMemoryTelemetryContext::with_clock(clock.clone());
    for (name, start) in names_and_starts {
        clock.now.store(*start, Ordering::SeqCst);
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
    let scope = telemetry.scope().to_string();
    (telemetry, scope)
}

fn normalize(value: &str, scope: &str) -> String {
    value.replace(scope, "00000000-0000-4000-8000-000000000000")
}

fn names(page: &kiana_telemetry::TracePage) -> Vec<String> {
    page.traces
        .iter()
        .filter_map(|trace| trace.spans.first().map(|span| span.name.clone()))
        .collect()
}

fn fixture_names(entry: &Value) -> Vec<String> {
    entry
        .get("traces")
        .and_then(Value::as_array)
        .map(|traces| {
            traces
                .iter()
                .filter_map(|trace| {
                    trace.as_str().map(str::to_string).or_else(|| {
                        trace
                            .get("spans")
                            .and_then(Value::as_array)
                            .and_then(|spans| spans.first())
                            .and_then(|span| span.get("name"))
                            .and_then(Value::as_str)
                            .map(str::to_string)
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn four_trace_store_matches_ts_pages() {
    let fixture = fixture();
    let (telemetry, scope) =
        store(&[("a", 10_000), ("b", 20_000), ("c", 20_000), ("d", 30_000)]).await;

    let all = telemetry.find_traces(None).await.expect("all");
    assert_eq!(names(&all), fixture_names(&fixture["all"]));
    assert_eq!(all.cursor, None);

    let limit1 = telemetry
        .find_traces(Some(TraceQuery {
            limit: Some(1),
            ..TraceQuery::default()
        }))
        .await
        .expect("limit 1");
    assert_eq!(names(&limit1), fixture_names(&fixture["limit1p1"]));
    let cursor = limit1.cursor.clone().expect("cursor");
    assert_eq!(
        normalize(&cursor, &scope),
        fixture["limit1p1"]["cursor"].as_str().unwrap()
    );
    let page2 = telemetry
        .find_traces(Some(TraceQuery {
            limit: Some(1),
            cursor: Some(cursor),
            ..TraceQuery::default()
        }))
        .await
        .expect("limit 1 page 2");
    assert_eq!(names(&page2), fixture_names(&fixture["limit1p2"]));
    assert_eq!(
        page2.cursor.as_deref().map(|c| normalize(c, &scope)),
        fixture["limit1p2"]["cursor"].as_str().map(str::to_string)
    );

    // `cursor: "null"` → 等价于无 cursor。
    let null_cursor = telemetry
        .find_traces(Some(TraceQuery {
            cursor: Some("null".to_string()),
            ..TraceQuery::default()
        }))
        .await
        .expect("null cursor");
    assert_eq!(names(&null_cursor), fixture_names(&fixture["cursorNull"]));

    // "[]" / "\"ab\"" / "{}" / "5" → 比较恒假，空页且无 cursor（不抛）。
    for key in [
        "cursorEmptyArray",
        "cursorString",
        "cursorObject",
        "cursorNumber",
    ] {
        let cursor = match key {
            "cursorEmptyArray" => "[]",
            "cursorString" => "\"ab\"",
            "cursorObject" => "{}",
            _ => "5",
        };
        let page = telemetry
            .find_traces(Some(TraceQuery {
                cursor: Some(cursor.to_string()),
                ..TraceQuery::default()
            }))
            .await
            .unwrap_or_else(|error| panic!("{key}: {error}"));
        assert_eq!(names(&page), fixture_names(&fixture[key]), "{key}");
        assert_eq!(page.cursor, None, "{key}");
    }

    // 非法 JSON → 解析错误（TS 是 SyntaxError；Rust 的 message 不同，只断言「报错」）。
    let error = telemetry
        .find_traces(Some(TraceQuery {
            cursor: Some("not json".to_string()),
            ..TraceQuery::default()
        }))
        .await
        .expect_err("invalid cursor");
    assert!(!error.message.is_empty());
    // TS: `JSON.parse` 抛 `SyntaxError`。消息文本是 V8 专有文案（spec §22.5 允许近似），
    // 但**错误名**是用户可见契约（`error.name`），必须保持 `SyntaxError`；改动需同步测试。
    assert_eq!(
        error.name(),
        Some("SyntaxError"),
        "JSON.parse throws SyntaxError"
    );
    assert_eq!(
        fixture["cursorBad"]["errorKind"].as_str(),
        Some("parse"),
        "fixture records a parse error"
    );
}

#[tokio::test]
async fn three_trace_store_matches_ts_pages_and_limit_zero() {
    let fixture = fixture();
    let (telemetry, scope) = store(&[("t1", 20_000), ("t2", 30_000), ("t3", 40_000)]).await;

    let all = telemetry.find_traces(None).await.expect("all");
    assert_eq!(names(&all), fixture_names(&fixture["three-all"]));

    let limit2 = telemetry
        .find_traces(Some(TraceQuery {
            limit: Some(2),
            ..TraceQuery::default()
        }))
        .await
        .expect("limit 2");
    assert_eq!(names(&limit2), fixture_names(&fixture["three-limit2"]));
    let cursor = limit2.cursor.clone().expect("cursor");
    assert_eq!(
        normalize(&cursor, &scope),
        fixture["three-limit2"]["cursor"].as_str().unwrap()
    );
    let page2 = telemetry
        .find_traces(Some(TraceQuery {
            limit: Some(2),
            cursor: Some(cursor),
            ..TraceQuery::default()
        }))
        .await
        .expect("page 2");
    assert_eq!(names(&page2), fixture_names(&fixture["three-limit2page2"]));
    assert_eq!(page2.cursor, None);

    // `limit >= total` → 全部、无 cursor。
    for limit in [3u32, 4] {
        let page = telemetry
            .find_traces(Some(TraceQuery {
                limit: Some(limit),
                ..TraceQuery::default()
            }))
            .await
            .expect("limit >= total");
        assert_eq!(names(&page), fixture_names(&fixture["three-limit3"]));
        assert_eq!(page.cursor, None);
    }

    // `limit: 0` 且有匹配 → TS 的 TypeError 文本（Rust 返回 Err 而不是 panic）。
    let error = telemetry
        .find_traces(Some(TraceQuery {
            limit: Some(0),
            ..TraceQuery::default()
        }))
        .await
        .expect_err("limit 0 with matches");
    assert_eq!(
        error.message,
        "Cannot read properties of undefined (reading 'spans')"
    );
    // `traces.at(-1)` 对 undefined 读属性 → `TypeError`（错误文本是当前稳定契约，改动需同步测试）。
    assert_eq!(
        error.name(),
        Some("TypeError"),
        "at(-1) read throws TypeError"
    );
    assert_eq!(
        fixture["three-limitNeg3"]["errorKind"].as_str(),
        Some("type")
    );
}

#[tokio::test]
async fn empty_store_returns_empty_pages() {
    let fixture = fixture();
    let (telemetry, _) = store(&[]).await;
    let empty = telemetry
        .find_traces(Some(TraceQuery {
            limit: Some(0),
            ..TraceQuery::default()
        }))
        .await
        .expect("empty store limit 0");
    assert_eq!(names(&empty), fixture_names(&fixture["empty-limit0"]));
    assert_eq!(empty.cursor, None);
    let positive = telemetry
        .find_traces(Some(TraceQuery {
            limit: Some(5),
            ..TraceQuery::default()
        }))
        .await
        .expect("empty store positive limit");
    assert_eq!(names(&positive), fixture_names(&fixture["empty-limitPos"]));
}
