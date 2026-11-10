//! `packages/ai/test/streaming-tool-arguments.test.ts` 的 Rust 移植（内存量级/释放部分）。
//!
//! TS 用 V8 的 `allocatedBytes`（分配 + 运行期 GC 释放）与 `WeakRef` + 强制 GC 验证：
//! 1. 参数文本放大 10 倍时内存增长约 10 倍（线性），而不是重解析累积文本的约 100 倍；
//! 2. 调用方丢弃流、partial 消息与参数快照后它们能被回收。
//!
//! Rust 没有 V8 堆统计与强制 GC，本文件用测试专用全局分配器统计**存活字节峰值**与
//! **丢弃后的存活字节**（见 `tests/common/memory.rs`）。差异（如实记录）：
//! - TS 的“分配字节”包含运行期释放的垃圾；Rust 侧测存活峰值，等价于“流状态/partial 的驻留量”，
//!   这正是 TS 注释里“重新解析累积文本”会以 100 倍暴露的成本；
//! - Rust 的所有权保证 drop 即释放，因此释放用例断言“丢弃后存活字节回到基线”而不是 WeakRef 回收。

mod common;

#[path = "support/streaming_tool_arguments.rs"]
mod harness;

use common::memory::{CountingAllocator, begin_peak, live_bytes, peak_delta};
use harness::{Path, deltas_of, write_call};
use kiana_ai::types::{AssistantContentBlock, AssistantMessage, StopReason};

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

/// 内存测量是进程级状态；同一测试二进制内的用例串行执行。
static MEASURE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// 跑完一条流并返回最终消息与存活字节峰值。
async fn measure(path: Path, size: usize) -> (AssistantMessage, usize) {
    let (args, text) = write_call(size);
    let deltas = deltas_of(&text, 32);
    let baseline = begin_peak();
    let stream = path.stream_chunked(&deltas, &args, &text);
    while let Some(event) = stream.recv().await {
        // 丢弃每个事件（TS 的 `for await` 同样不保留事件）。
        drop(event);
    }
    let message = stream.result().await.expect("result");
    let peak = peak_delta(baseline);
    (message, peak)
}

fn assert_exact_tool_call(message: &AssistantMessage, size: usize) {
    assert_eq!(message.stop_reason, StopReason::ToolUse);
    let (args, _) = write_call(size);
    let AssistantContentBlock::ToolCall(call) = &message.content[0] else {
        panic!("expected a tool call");
    };
    assert_eq!(call.name, "write");
    assert_eq!(call.arguments, args);
}

async fn check_linear_allocation(path: Path) {
    let _guard = MEASURE_LOCK.lock().await;

    // 预热：触发惰性静态初始化与运行时缓冲，避免它们污染小样本。
    let _ = measure(path, 1_000).await;

    let (_, small) = measure(path, 25_000).await;
    let (large_message, large) = measure(path, 250_000).await;
    assert_exact_tool_call(&large_message, 250_000);

    // 文本放大 10 倍：线性增长约 10 倍，重解析累积文本约 100 倍。
    assert!(small > 0, "{}: zero peak", path.name());
    assert!(
        large < small.saturating_mul(25),
        "{}: peak grew {large} from {small} (not linear)",
        path.name()
    );
    let (_, text) = write_call(250_000);
    assert!(
        large / text.len().max(1) < 5_000,
        "{}: {large} bytes for {} chars",
        path.name(),
        text.len()
    );
}

async fn check_release_after_drop(path: Path) {
    let _guard = MEASURE_LOCK.lock().await;

    // 预热一次，让静态与运行时缓冲在基线之前落定。
    let warmup = measure(path, 500).await.0;
    drop(warmup);

    let baseline = live_bytes();
    {
        let (message, _) = measure(path, 2_000).await;
        assert_exact_tool_call(&message, 2_000);
        drop(message);
    }
    // 让适配器可能 spawn 的转发任务跑完。
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
    let after = live_bytes();
    let slack = 256 * 1024;
    assert!(
        after <= baseline + slack,
        "{}: leaked {} bytes after drop",
        path.name(),
        after.saturating_sub(baseline)
    );
}

#[tokio::test]
async fn anthropic_messages_allocates_linearly_in_the_argument_text() {
    check_linear_allocation(Path::Anthropic).await;
}

#[tokio::test]
async fn openai_completions_allocates_linearly_in_the_argument_text() {
    check_linear_allocation(Path::Completions).await;
}

#[tokio::test]
async fn openai_responses_allocates_linearly_in_the_argument_text() {
    check_linear_allocation(Path::Responses).await;
}

#[tokio::test]
async fn mistral_conversations_allocates_linearly_in_the_argument_text() {
    check_linear_allocation(Path::Mistral).await;
}

#[tokio::test]
async fn kiana_messages_allocates_linearly_in_the_argument_text() {
    check_linear_allocation(Path::KianaMessages).await;
}

#[tokio::test]
async fn faux_allocates_linearly_in_the_argument_text() {
    check_linear_allocation(Path::Faux).await;
}

#[tokio::test]
async fn anthropic_messages_releases_state_after_the_caller_drops_it() {
    check_release_after_drop(Path::Anthropic).await;
}

#[tokio::test]
async fn openai_completions_releases_state_after_the_caller_drops_it() {
    check_release_after_drop(Path::Completions).await;
}

#[tokio::test]
async fn openai_responses_releases_state_after_the_caller_drops_it() {
    check_release_after_drop(Path::Responses).await;
}

#[tokio::test]
async fn mistral_conversations_releases_state_after_the_caller_drops_it() {
    check_release_after_drop(Path::Mistral).await;
}

#[tokio::test]
async fn kiana_messages_releases_state_after_the_caller_drops_it() {
    check_release_after_drop(Path::KianaMessages).await;
}

#[tokio::test]
async fn faux_releases_state_after_the_caller_drops_it() {
    check_release_after_drop(Path::Faux).await;
}
