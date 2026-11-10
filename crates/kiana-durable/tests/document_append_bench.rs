//! `test/document-append-bench.ts` 的 Rust 版（基准，默认被忽略）。
//!
//! 每次提交追加一项：一个文档里的数组 vs 每个项一个 family member（journal 的两种存法）。
//! 报告各规模下的毫秒数与每次追加的微秒数。
//!
//! 运行：`cargo test -p kiana-durable --test document_append_bench -- --ignored --nocapture`
//! 规模可用 `KIANA_BENCH_APPENDS`（逗号分隔）覆盖。

mod support;

use std::sync::Arc;
use std::time::Instant;

use kiana_chord::context::Context;
use kiana_durable::session::{SessionImpl, SessionOptions};
use kiana_durable::{
    BoxError, DocDefinition, DocFamilyToken, DocTarget, DocumentSemantics, JsonObject, JsonValue,
};
use serde_json::json;
use support::*;

fn ctx() -> &'static dyn Context {
    &**kiana_chord::context::BACKGROUND_CONTEXT
}

fn journal_definition() -> kiana_durable::DocToken {
    kiana_durable::define_doc(
        DocDefinition::new("bench.journal", 1, DocumentSemantics::SESSION).with_initial(|_| {
            let mut value = JsonObject::new();
            value.insert("entries".to_owned(), json!([]));
            value
        }),
    )
}

fn member_definition() -> DocFamilyToken {
    kiana_durable::define_doc_family(
        DocDefinition::family("bench.member", 1, DocumentSemantics::SESSION).with_initial(|_| {
            let mut value = JsonObject::new();
            value.insert("issued".to_owned(), json!(0));
            value.insert("at".to_owned(), json!(0));
            value
        }),
    )
}

fn open() -> SessionImpl {
    SessionImpl::new(Arc::new(MemoryStorage::new()), SessionOptions::default())
}

async fn run_array(n: usize) -> Result<u128, BoxError> {
    let session = open();
    let journal = journal_definition();
    let doc_target = DocTarget::session(&journal);
    let started = Instant::now();
    for index in 0..n {
        let doc_target = doc_target.clone();
        session
            .commit_with(
                move |tx| {
                    let doc_target = doc_target.clone();
                    Box::pin(async move {
                        let mut draft = tx.doc(&doc_target).await?;
                        let entry =
                            json!({ "key": format!("c{index}"), "issued": index, "at": index });
                        let entries = draft
                            .get_mut("entries")
                            .and_then(|entries| entries.as_array_mut())
                            .expect("entries is an array");
                        entries.push(entry);
                        Ok(())
                    })
                },
                ctx(),
                Default::default(),
            )
            .await?;
    }
    let elapsed = started.elapsed().as_millis();
    session.close(ctx()).await;
    Ok(elapsed)
}

async fn run_family(n: usize) -> Result<u128, BoxError> {
    let session = open();
    let member = member_definition();
    let started = Instant::now();
    for index in 0..n {
        let doc_target = DocTarget::session_family(&member, format!("c{index}"), JsonValue::Null);
        session
            .commit_with(
                move |tx| {
                    let doc_target = doc_target.clone();
                    Box::pin(async move {
                        let mut draft = tx.doc(&doc_target).await?;
                        draft.insert("issued".to_owned(), json!(index));
                        draft.insert("at".to_owned(), json!(index));
                        Ok(())
                    })
                },
                ctx(),
                Default::default(),
            )
            .await?;
    }
    let elapsed = started.elapsed().as_millis();
    session.close(ctx()).await;
    Ok(elapsed)
}

#[tokio::test]
#[ignore = "基准：用 --ignored --nocapture 运行"]
async fn document_append() {
    let sizes: Vec<usize> = std::env::var("KIANA_BENCH_APPENDS")
        .unwrap_or_else(|_| "200,800,2000".to_owned())
        .split(',')
        .filter_map(|value| value.trim().parse().ok())
        .collect();
    for n in sizes {
        let array = run_array(n).await.expect("the array run succeeds");
        let family = run_family(n).await.expect("the family run succeeds");
        let per = |ms: u128| (ms as f64 * 1000.0 / n as f64).round() as u128;
        println!(
            "n={n}: array {array} ms ({} µs/append), family {family} ms ({} µs/append)",
            per(array),
            per(family)
        );
    }
}
