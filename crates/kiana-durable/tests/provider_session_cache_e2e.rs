//! `test/provider-session-cache-e2e.test.ts` 的 Rust 端口（live）。
//!
//! 上游用例是 #10424 的回归覆盖：用 Codex 的 prompt cache，在 8 轮 Durable 运行中验证
//! `cacheRead` 从第二轮起稳定命中，且每次请求转发的是 `kiana.provider` 文档里的 `sessionId`。
//! 没有 `~/.kiana/agent/auth.json` 凭据时上游 `it.skipIf(!codexToken)` 跳过。
//!
//! Rust 端口标记为 `#[ignore]`：kiana-ai 的 `Models` 没有 TS `Proxy` 那种对 `streamSimple` 的
//! 拦截点（provider 以 `Arc<dyn Provider>` 持有），无法在不新增包装层的前提下记录
//! `sessionId`；真实运行还需要 OpenAI Codex OAuth 与 SSE transport。这里保留确定性的辅助
//! 函数与用例骨架，凭据/传输就绪后按同样的断言补上驱动循环。

use kiana_ai::types::Usage;

/// 上游的 `cacheProbe(nonce)`：一段带 nonce 的长重复上下文。
fn cache_probe(nonce: &str) -> String {
    let mut lines = vec![
        "This is a real automated prompt-cache test. The repeated context below is intentional."
            .to_owned(),
        "Read it silently, then reply with exactly: CACHE PROBE READY".to_owned(),
    ];
    for index in 0..180 {
        lines.push(format!(
            "{nonce} immutable cache record {index:03}: alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi omicron kiana rho sigma tau upsilon phi chi psi omega."
        ));
    }
    lines.join("\n")
}

/// 上游的 `cacheHitRate(usage)`：缓存读占 prompt 的比例。
fn cache_hit_rate(usage: &Usage) -> f64 {
    let prompt = usage.input + usage.cache_read + usage.cache_write;
    if prompt == 0 {
        0.0
    } else {
        usage.cache_read as f64 / prompt as f64
    }
}

#[test]
#[ignore = "live: needs OpenAI Codex OAuth credentials and the openai-codex-responses transport"]
fn reuses_the_codex_prompt_cache_across_durable_turns() {
    // 确定性部分：探针形状与命中率计算。
    let probe = cache_probe("0190f0c4-0000-7000-8000-000000000000");
    assert_eq!(probe.lines().count(), 182);
    assert!(probe.starts_with("This is a real automated prompt-cache test."));
    assert!(probe.contains("immutable cache record 000:"));
    assert!(probe.contains("immutable cache record 179:"));

    let first = Usage {
        input: 4_000,
        output: 2,
        cache_read: 0,
        cache_write: 0,
        total_tokens: 4_002,
        cost: Default::default(),
        ..Default::default()
    };
    assert_eq!(cache_hit_rate(&first), 0.0);
    let cached = Usage {
        input: 400,
        output: 2,
        cache_read: 3_600,
        cache_write: 0,
        total_tokens: 4_002,
        cost: Default::default(),
        ..Default::default()
    };
    assert!(cache_hit_rate(&cached) >= 0.9);

    // live 部分（上游 it.skipIf(!codexToken)）：
    // 1. `builtin_models` 上包一层记录 `streamSimple` 的 `options.sessionId`；
    // 2. 以 `Harness::open(MemoryStorage, { models, registry: create_registry(), settings })`
    //    打开，`root()` 选 `openai-codex`/`gpt-6.1-sol`，`resume()`；
    // 3. 循环 8 轮 submit/wait，读 AssistantEntry 的 `model[0].usage`；
    // 4. `harness.snapshot(ProviderDoc)` 的 `session_id` 与每轮转发的值相等；
    // 5. 首轮 prompt >= 4000、cacheRead == 0，命中从第 1 轮（0 起）开始，随后 3 轮命中率 >= 0.9。
    panic!(
        "live provider-session-cache e2e: set up OpenAI Codex OAuth and the transport wrapper before running"
    );
}
