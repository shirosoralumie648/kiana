//! `test/session-document-cache-bench.ts` 的 Rust 版（基准，默认被忽略）。
//!
//! 在内存存储上创建 N 个会话，每个会话跑一次短文档工作；然后报告不同保留期下缓存里的文档数，
//! 以及重新读取一个旧会话文档的耗时。
//!
//! 运行：`cargo test -p kiana-durable --test session_document_cache_bench -- --ignored --nocapture`
//! 规模可用 `KIANA_BENCH_CONVERSATIONS` 覆盖。

mod support;

use std::sync::Arc;
use std::time::Instant;

use kiana_chord::context::Context;
use kiana_durable::session::{SessionImpl, SessionOptions};
use kiana_durable::timers::SetTimer;
use kiana_durable::{
    BoxError, ConversationId, DocDefinition, DocTarget, DocumentFork, DocumentHistory,
    DocumentSemantics, JsonObject, ScopeKind,
};
use serde_json::json;
use support::*;

fn ctx() -> &'static dyn Context {
    &**kiana_chord::context::BACKGROUND_CONTEXT
}

fn usage_definition() -> kiana_durable::DocToken {
    kiana_durable::define_doc(
        DocDefinition::new(
            "bench.usage",
            1,
            DocumentSemantics {
                scope: ScopeKind::Conversation,
                history: Some(DocumentHistory::Latest),
                fork: Some(DocumentFork::Initial),
            },
        )
        .with_initial(|_| {
            let mut value = JsonObject::new();
            value.insert("count".to_owned(), json!(0));
            value
        }),
    )
}

fn open(retention_ms: f64) -> SessionImpl {
    let decline: SetTimer = Arc::new(|_callback, _delay, _options| None);
    SessionImpl::new(
        Arc::new(MemoryStorage::new()),
        SessionOptions {
            document_retention_ms: Some(retention_ms),
            set_timer: Some(decline),
            ..Default::default()
        },
    )
}

struct Measurement {
    elapsed_ms: u128,
    cached: usize,
    read_us: u128,
}

async fn run(conversations: usize, retention_ms: f64) -> Result<Measurement, BoxError> {
    let session = open(retention_ms);
    let usage = usage_definition();
    let started = Instant::now();
    let mut first: Option<ConversationId> = None;
    for _ in 0..conversations {
        let conversation_id = create_conversation(&session).await;
        first.get_or_insert(conversation_id);
        let doc_target = DocTarget::conversation(&usage, conversation_id);
        session
            .commit_with(
                move |tx| {
                    let doc_target = doc_target.clone();
                    Box::pin(async move {
                        let mut draft = tx.doc(&doc_target).await?;
                        let count = draft
                            .get("count")
                            .and_then(|value| value.as_i64())
                            .unwrap_or(0);
                        draft.insert("count".to_owned(), json!(count + 1));
                        Ok(())
                    })
                },
                ctx(),
                Default::default(),
            )
            .await?;
    }
    let elapsed_ms = started.elapsed().as_millis();
    session.read_on_line(async { Ok(()) }).await?;
    let cached = session.document_cache_sizes().cached;

    let first = first.expect("at least one conversation");
    let read_started = Instant::now();
    for _ in 0..100 {
        let _ = session
            .snapshot(&DocTarget::conversation(&usage, first), ctx())
            .await?;
    }
    let read_us = read_started.elapsed().as_micros() / 100;
    session.close(ctx()).await;
    Ok(Measurement {
        elapsed_ms,
        cached,
        read_us,
    })
}

#[tokio::test]
#[ignore = "基准：用 --ignored --nocapture 运行"]
async fn session_document_cache() {
    let conversations: usize = std::env::var("KIANA_BENCH_CONVERSATIONS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(256);
    println!("conversations | retention | ms | cached docs | old-doc read");
    for (label, retention) in [
        ("infinite", f64::INFINITY),
        ("10 min", 600_000.0),
        ("50 ms", 50.0),
        ("0", 0.0),
    ] {
        let measurement = run(conversations, retention)
            .await
            .expect("the benchmark run succeeds");
        println!(
            "{conversations:>13} | {label:>9} | {:>6} | {:>11} | {:>8} µs",
            measurement.elapsed_ms, measurement.cached, measurement.read_us
        );
        assert!(
            measurement.cached <= conversations,
            "the cache stays bounded by the number of conversations"
        );
        if retention == 0.0 {
            assert_eq!(measurement.cached, 0, "retention 0 evicts after use");
        }
    }
}
