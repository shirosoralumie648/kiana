//! `rpc-client-clear-queue.test.ts` / `rpc-client-clone.test.ts` 的 Rust 移植，加上 33 个
//! 命令体的黄金对拍（fixtures 由 TS 参照实现生成）。
//!
//! TS 的两个小测试直接替换私有 `send` / `getData`；Rust 侧没有可替换的等价物，因此用一个
//! **假 RPC 服务器**（`#!/bin/sh` 脚本）代替：它把收到的每一行追加到 `$BODY_FILE`，并按命令
//! 类型回一条成功响应。这样既断言了 `send()` 实际写出的字节（键序、id 在最后），也走完了
//! 真实的子进程/JSONL 路径。
//!
//! 所有测试在单线程 runtime + `LocalSet` 上运行（客户端任务用 `spawn_local`）。
//! 仅 Unix（脚本依赖 shebang 与可执行位）。

#![cfg(unix)]

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use kiana_agent_core::ThinkingLevel;
use kiana_ai::types::{ImageContent, ImageKind};
use kiana_ext_api::events::StreamingBehavior;
use kiana_rpc_print::{ClearQueueResult, RpcClient, RpcClientOptions, SessionChangeResult};
use serde_json::Value;

/// 测试用临时目录（Drop 时递归删除）。
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let unique = format!(
            "{}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or_default()
        );
        let path = std::env::temp_dir().join(format!("kiana-rpc-client-{tag}-{unique}"));
        std::fs::create_dir_all(&path).expect("create temp dir");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// 写一个可执行 shell 脚本并返回路径。
fn write_script(dir: &Path, name: &str, contents: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, contents).expect("write script");
    let mut permissions = std::fs::metadata(&path).expect("stat script").permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&path, permissions).expect("chmod script");
    path
}

/// 在单线程 runtime + `LocalSet` 上跑 future。
fn run_local<F: std::future::Future>(future: F) -> F::Output {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build current-thread runtime");
    tokio::task::LocalSet::new().block_on(&runtime, future)
}

/// 构造一个假服务器客户端：脚本把每条 stdin 记录追加到 `$BODY_FILE`。
fn script_client(script: PathBuf, body_file: &Path) -> RpcClient {
    let mut env = HashMap::new();
    env.insert(
        "BODY_FILE".to_string(),
        Some(body_file.to_string_lossy().into_owned()),
    );
    RpcClient::new(RpcClientOptions {
        cli_path: Some(script),
        env,
        ..RpcClientOptions::default()
    })
}

/// 串行化“写脚本 + spawn”。
///
/// Linux 上 `fork()` 的子进程会继承父进程当时仍打开的可写 fd；如果另一个测试线程正在写
/// 一个新的可执行脚本，而本线程同时 `execve` 该脚本，内核会偶发返回 `ETXTBSY`（text file
/// busy）。测试二进制内用一把全局锁把这两个步骤串起来即可消除该竞态（仅测试纪律，不影响
/// 被测代码）。
static SPAWN_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// 在 [`SPAWN_LOCK`] 保护下写脚本并启动客户端。
///
/// 返回的 [`TempDir`] 必须活到测试结束（脚本与 body 文件都在里面）。
async fn start_script(
    tag: &str,
    contents: &str,
    request_timeout: Option<Duration>,
) -> (TempDir, PathBuf, RpcClient) {
    let guard = SPAWN_LOCK.lock().await;
    let dir = TempDir::new(tag);
    let body_file = dir.path().join("bodies.txt");
    let script = write_script(dir.path(), "server.sh", contents);
    let mut client = script_client(script, &body_file);
    if let Some(timeout) = request_timeout {
        client = client.with_request_timeout_for_test(timeout);
    }
    client.start().await.expect("start");
    drop(guard);
    (dir, body_file, client)
}

/// 通用假服务器：按类型回成功响应（clear_queue / clone 回各自的最小合法 data，其它回 `{}`）。
const FAKE_SERVER: &str = r#"#!/bin/sh
while IFS= read -r line; do
    printf '%s\n' "$line" >> "$BODY_FILE"
    id=$(printf '%s\n' "$line" | sed -n 's/.*"id":"\([^"]*\)"}$/\1/p')
    case "$line" in
        *'"type":"clear_queue"'*)
            printf '{"id":"%s","type":"response","command":"clear_queue","success":true,"data":{"steering":["Change direction"],"followUp":["Summarize when finished"]}}\n' "$id"
            ;;
        *'"type":"clone"'*)
            printf '{"id":"%s","type":"response","command":"clone","success":true,"data":{"cancelled":false}}\n' "$id"
            ;;
        *)
            printf '{"id":"%s","type":"response","success":true,"data":{}}\n' "$id"
            ;;
    esac
done
"#;

/// 读 fixtures 里的命令体黄金表（TS 参照实现 `JSON.stringify({...body, id})` 生成）。
fn golden_bodies() -> HashMap<String, String> {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rpc_client/command_bodies.json");
    let text = std::fs::read_to_string(&path).expect("read command_bodies.json");
    serde_json::from_str(&text).expect("parse command_bodies.json")
}

#[test]
fn clear_queue_sends_ts_body_and_returns_queues() {
    run_local(async {
        let (_dir, body_file, client) = start_script("clear-queue", FAKE_SERVER, None).await;

        let result = client.clear_queue().await.expect("clear_queue");
        assert_eq!(
            result,
            ClearQueueResult {
                steering: vec!["Change direction".to_string()],
                follow_up: vec!["Summarize when finished".to_string()],
            }
        );
        client.stop().await.expect("stop");

        let bodies = std::fs::read_to_string(&body_file).expect("read bodies");
        assert_eq!(
            bodies.lines().collect::<Vec<_>>(),
            vec![r#"{"type":"clear_queue","id":"req_1"}"#]
        );
    });
}

#[test]
fn clone_sends_ts_body_and_returns_cancelled() {
    run_local(async {
        let (_dir, body_file, client) = start_script("clone", FAKE_SERVER, None).await;

        let result = client.clone().await.expect("clone");
        assert_eq!(result, SessionChangeResult { cancelled: false });
        client.stop().await.expect("stop");

        let bodies = std::fs::read_to_string(&body_file).expect("read bodies");
        assert_eq!(
            bodies.lines().collect::<Vec<_>>(),
            vec![r#"{"type":"clone","id":"req_1"}"#]
        );
    });
}

/// 逐条命令对拍 TS 生成的黄金行：键序、可选键省略、id 最后，全部逐字节相等。
#[test]
fn all_command_bodies_match_ts_golden_fixtures() {
    let golden = golden_bodies();

    // 与 `gen_bodies.mts` 完全相同的调用顺序；每个方法恰好发一条记录。
    const CASES: [&str; 41] = [
        "prompt_min",
        "prompt_full",
        "prompt_steer_behavior",
        "steer_min",
        "steer_images",
        "follow_up_min",
        "follow_up_images",
        "abort",
        "clear_queue",
        "new_session_min",
        "new_session_parent",
        "get_state",
        "set_model",
        "cycle_model",
        "get_available_models",
        "set_thinking_level",
        "cycle_thinking_level",
        "get_available_thinking_levels",
        "set_steering_mode",
        "set_follow_up_mode",
        "compact_min",
        "compact_instructions",
        "set_auto_compaction",
        "set_auto_retry",
        "abort_retry",
        "bash",
        "abort_bash",
        "get_session_stats",
        "export_html_min",
        "export_html_path",
        "switch_session",
        "fork",
        "clone",
        "get_fork_messages",
        "get_entries_min",
        "get_entries_since",
        "get_tree",
        "get_last_assistant_text",
        "set_session_name",
        "get_messages",
        "get_commands",
    ];

    run_local(async {
        let (_dir, body_file, client) =
            start_script("golden", FAKE_SERVER, Some(Duration::from_secs(5))).await;

        let image = ImageContent {
            kind_image: ImageKind::Image,
            data: "aGVsbG8=".to_string(),
            mime_type: "image/png".to_string(),
        };
        let images = || Some(vec![image.clone()]);

        // 返回类型各异：黄金测试只关心写出的命令体，忽略解析结果。
        let _ = client.prompt("hello", None, None).await;
        let _ = client
            .prompt("hello", images(), Some(StreamingBehavior::FollowUp))
            .await;
        let _ = client
            .prompt("hello", None, Some(StreamingBehavior::Steer))
            .await;
        let _ = client.steer("look left", None).await;
        let _ = client.steer("look left", images()).await;
        let _ = client.follow_up("afterwards", None).await;
        let _ = client.follow_up("afterwards", images()).await;
        let _ = client.abort().await;
        let _ = client.clear_queue().await;
        let _ = client.new_session(None).await;
        let _ = client
            .new_session(Some("/tmp/parent.jsonl".to_string()))
            .await;
        let _ = client.get_state().await;
        let _ = client.set_model("anthropic", "claude-sonnet-4-5").await;
        let _ = client.cycle_model().await;
        let _ = client.get_available_models().await;
        let _ = client.set_thinking_level(ThinkingLevel::High).await;
        let _ = client.cycle_thinking_level().await;
        let _ = client.get_available_thinking_levels().await;
        let _ = client
            .set_steering_mode(kiana_agent_core::QueueMode::OneAtATime)
            .await;
        let _ = client
            .set_follow_up_mode(kiana_agent_core::QueueMode::All)
            .await;
        let _ = client.compact(None).await;
        let _ = client.compact(Some("keep it short".to_string())).await;
        let _ = client.set_auto_compaction(true).await;
        let _ = client.set_auto_retry(false).await;
        let _ = client.abort_retry().await;
        let _ = client.bash("echo hello").await;
        let _ = client.abort_bash().await;
        let _ = client.get_session_stats().await;
        let _ = client.export_html(None).await;
        let _ = client.export_html(Some("/tmp/out.html".to_string())).await;
        let _ = client.switch_session("/tmp/session.jsonl").await;
        let _ = client.fork("entry-1").await;
        let _ = client.clone().await;
        let _ = client.get_fork_messages().await;
        let _ = client.get_entries(None).await;
        let _ = client.get_entries(Some("entry-2".to_string())).await;
        let _ = client.get_tree().await;
        let _ = client.get_last_assistant_text().await;
        let _ = client.set_session_name("my session").await;
        let _ = client.get_messages().await;
        let _ = client.get_commands().await;

        client.stop().await.expect("stop");

        let bodies = std::fs::read_to_string(&body_file).expect("read bodies");
        let actual: Vec<&str> = bodies.lines().collect();
        assert_eq!(actual.len(), CASES.len(), "unexpected number of commands");

        for (index, (line, case)) in actual.iter().zip(CASES).enumerate() {
            let template = golden
                .get(case)
                .unwrap_or_else(|| panic!("missing golden fixture {case}"));
            let id = format!("req_{}", index + 1);
            let expected = template.replace("\"id\":\"req_1\"}", &format!("\"id\":\"{id}\"}}"));
            assert_eq!(*line, expected, "command body mismatch for {case}");
            assert!(
                line.ends_with(&format!("\"id\":\"{id}\"}}")),
                "id must be the last key for {case}: {line}"
            );
        }
    });
}

/// 30s 请求超时的逐字文本（用 50ms 覆盖快速验证）。
#[test]
fn request_timeout_uses_ts_error_text() {
    run_local(async {
        let (_dir, _body_file, client) = start_script(
            "timeout",
            "#!/bin/sh\nwhile IFS= read -r line; do\n    :\ndone\n",
            Some(Duration::from_millis(50)),
        )
        .await;

        let error = client.get_commands().await.expect_err("must time out");
        assert_eq!(
            error.to_string(),
            "Timeout waiting for response to get_commands. Stderr: "
        );

        client.stop().await.expect("stop");
    });
}

/// TS `getData()` 是 `throw new Error(errorResponse.error)`：`error` 缺失（undefined）时
/// `new Error(undefined).message === ""`，不是字符串 "undefined"。
#[test]
fn failed_response_without_error_field_throws_empty_message() {
    run_local(async {
        let (_dir, _body_file, client) = start_script(
            "empty-error",
            r#"#!/bin/sh
while IFS= read -r line; do
    id=$(printf '%s\n' "$line" | sed -n 's/.*"id":"\([^"]*\)"}$/\1/p')
    printf '{"id":"%s","type":"response","success":false}\n' "$id"
done
"#,
            None,
        )
        .await;

        let error = client.get_state().await.expect_err("failure response");
        assert_eq!(error.to_string(), "");

        client.stop().await.expect("stop");
    });
}

/// 子进程关闭 stdin 后仍在运行时，写失败走 TS 的 stdin `error` 事件文本
/// `Agent process stdin error: write EPIPE. Stderr: ` 并被缓存。
#[test]
fn stdin_write_failure_uses_node_error_text() {
    run_local(async {
        let (_dir, _body_file, client) =
            start_script("stdin-epipe", "#!/bin/sh\nexec 0<&-\nexec sleep 5\n", None).await;

        let error = client.get_commands().await.expect_err("stdin closed");
        assert_eq!(
            error.to_string(),
            "Agent process stdin error: write EPIPE. Stderr: "
        );

        // TS 把该错误存进 exitError；后续请求复用同一错误对象。
        let cached = client.get_commands().await.expect_err("cached error");
        assert_eq!(cached.to_string(), error.to_string());

        client.stop().await.expect("stop");
    });
}

/// stderr 每块用 `String::from_utf8_lossy` 追加（TS 的 `data.toString()`），块大小必须与
/// Node/libuv 的 64 KiB 读缓冲一致：8192 字节的旧缓冲会在 8191 个 ASCII 字节后的多字节字符
/// 中间切开，产生 Node 不会产生的 U+FFFD。
#[test]
fn stderr_keeps_multibyte_characters_across_the_8k_boundary() {
    run_local(async {
        let guard = SPAWN_LOCK.lock().await;
        let dir = TempDir::new("stderr-chunk");
        let payload: Vec<u8> = std::iter::repeat_n(b'a', 8191)
            .chain("é".repeat(500).into_bytes())
            .collect();
        let expected = String::from_utf8(payload.clone()).expect("valid utf-8");
        let payload_file = dir.path().join("payload.bin");
        std::fs::write(&payload_file, &payload).expect("write payload");

        // 单次 write(2)：`cat` 用 128 KiB 缓冲把 9191 字节一次写进管道。
        let script = write_script(
            dir.path(),
            "stderr.sh",
            &format!(
                "#!/bin/sh\ncat '{}' >&2\nread -r line\nexit 0\n",
                payload_file.display()
            ),
        );
        let client = RpcClient::new(RpcClientOptions {
            cli_path: Some(script),
            ..RpcClientOptions::default()
        });
        client.start().await.expect("start");
        drop(guard);

        let mut stderr = client.get_stderr();
        for _ in 0..100 {
            if stderr.len() >= expected.len() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
            stderr = client.get_stderr();
        }
        assert_eq!(stderr, expected);
        assert!(!stderr.contains('\u{FFFD}'), "no replacement characters");

        client.stop().await.expect("stop");
    });
}

/// TS 原样返回服务端的模型对象，`reasoning` 不做类型校验；调用方按真值使用。
/// `ModelInfo.reasoning` 因此按 JS 真值映射（`"yes"` -> true），而不是让整个列表失败。
#[test]
fn non_boolean_model_reasoning_is_coerced_by_js_truthiness() {
    run_local(async {
        let (_dir, _body_file, client) = start_script(
            "model-reasoning",
            r#"#!/bin/sh
while IFS= read -r line; do
    id=$(printf '%s\n' "$line" | sed -n 's/.*"id":"\([^"]*\)"}$/\1/p')
    printf '{"id":"%s","type":"response","success":true,"data":{"models":[{"provider":"anthropic","id":"m","contextWindow":1000,"reasoning":"yes"},{"provider":"anthropic","id":"n","reasoning":0}]}}\n' "$id"
done
"#,
            None,
        )
        .await;

        let models = client
            .get_available_models()
            .await
            .expect("non-boolean reasoning is passed through by TS");
        assert_eq!(models.len(), 2);
        assert!(models[0].reasoning);
        assert!(!models[1].reasoning);
        assert_eq!(models[1].context_window, 0);

        client.stop().await.expect("stop");
    });
}

/// 事件收集器必须在 prompt 之前注册：即使事件在响应之前全部到达也不能丢。
#[test]
fn prompt_and_wait_collects_events_including_agent_settled() {
    run_local(async {
        let (_dir, _body_file, client) = start_script(
            "events",
            r#"#!/bin/sh
while IFS= read -r line; do
    printf '%s\n' "$line" >> "$BODY_FILE"
    id=$(printf '%s\n' "$line" | sed -n 's/.*"id":"\([^"]*\)"}$/\1/p')
    case "$line" in
        *'"type":"prompt"'*)
            printf '{"type":"agent_start"}\n'
            printf '{"type":"agent_settled","aborted":false}\n'
            printf '{"id":"%s","type":"response","success":true,"data":{"disposition":"started"}}\n' "$id"
            ;;
        *)
            printf '{"id":"%s","type":"response","success":true,"data":{}}\n' "$id"
            ;;
    esac
done
"#,
            None,
        )
        .await;

        let events = client
            .prompt_and_wait("hello", None)
            .await
            .expect("prompt_and_wait");
        assert_eq!(events.len(), 2, "events: {events:?}");
        assert_eq!(events[0]["type"], "agent_start");
        assert_eq!(events[1]["type"], "agent_settled");
        assert_eq!(events[1]["aborted"], false);

        client.stop().await.expect("stop");
    });
}

/// TS 的 `waitForIdle()` 在**调用时**注册监听器：常规用法“先创建等待、再发 prompt、最后
/// await”必须能观察到 prompt 期间到达的 `agent_settled`（旧实现在第一次 poll 才注册，事件
/// 已经错过，只能白等 60s 后超时）。
#[test]
fn wait_for_idle_registered_before_prompt_sees_settled_event() {
    run_local(async {
        let (_dir, _body_file, client) = start_script(
            "idle-order",
            r#"#!/bin/sh
while IFS= read -r line; do
    printf '%s\n' "$line" >> "$BODY_FILE"
    id=$(printf '%s\n' "$line" | sed -n 's/.*"id":"\([^"]*\)"}$/\1/p')
    case "$line" in
        *'"type":"prompt"'*)
            printf '{"type":"agent_settled","aborted":false}\n'
            printf '{"id":"%s","type":"response","success":true,"data":{"disposition":"started"}}\n' "$id"
            ;;
        *)
            printf '{"id":"%s","type":"response","success":true,"data":{}}\n' "$id"
            ;;
    esac
done
"#,
            None,
        )
        .await;

        let idle = client.wait_for_idle_with_timeout(Duration::from_millis(500));
        client.prompt("hello", None, None).await.expect("prompt");
        idle.await.expect("agent_settled arrived during prompt");

        client.stop().await.expect("stop");
    });
}

/// pending 中不存在的响应（例如超时之后才到达）必须作为事件广播。
#[test]
fn unknown_response_is_broadcast_as_event() {
    run_local(async {
        let (_dir, _body_file, client) = start_script(
            "broadcast",
            r#"#!/bin/sh
while IFS= read -r line; do
    printf '%s\n' "$line" >> "$BODY_FILE"
    id=$(printf '%s\n' "$line" | sed -n 's/.*"id":"\([^"]*\)"}$/\1/p')
    printf '{"type":"response","id":"req_999","success":false,"error":"late"}\n'
    printf '{"id":"%s","type":"response","success":true,"data":{}}\n' "$id"
done
"#,
            None,
        )
        .await;

        let seen: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
        let listener_seen = Arc::clone(&seen);
        let _unsubscribe = client.on_event(Arc::new(move |event| {
            listener_seen.lock().expect("lock").push(event.clone());
        }));

        client.abort().await.expect("abort");
        // 等待 stdout 泵把事件派发完（响应先于事件写入，但派发在同一 task 内顺序执行）。
        for _ in 0..100 {
            if !seen.lock().expect("lock").is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }

        let events = seen.lock().expect("lock").clone();
        assert_eq!(events.len(), 1, "events: {events:?}");
        assert_eq!(events[0]["type"], "response");
        assert_eq!(events[0]["id"], "req_999");
        assert_eq!(events[0]["success"], false);

        client.stop().await.expect("stop");
    });
}

/// 每秒触发一条 `agent_start` 事件再回响应的假服务器（用于监听器语义测试）。
const EVENT_SERVER: &str = r#"#!/bin/sh
while IFS= read -r line; do
    id=$(printf '%s\n' "$line" | sed -n 's/.*"id":"\([^"]*\)"}$/\1/p')
    printf '{"type":"agent_start"}\n'
    printf '{"id":"%s","type":"response","success":true,"data":{}}\n' "$id"
done
"#;

/// TS `onEvent`：监听器数组允许同一函数重复注册；退订用 `indexOf` + `splice` 移除**第一个**
/// 匹配项（同一个退订闭包调用两次会移除两个同名注册），找不到时无副作用。
#[test]
fn on_event_unsubscribes_the_first_matching_registration_only() {
    run_local(async {
        let (_dir, _body_file, client) = start_script("listener-dup", EVENT_SERVER, None).await;

        let calls = Arc::new(Mutex::new(0usize));
        let listener: kiana_rpc_print::RpcEventListener = {
            let calls = Arc::clone(&calls);
            Arc::new(move |_event: &Value| *calls.lock().expect("lock") += 1)
        };
        let unsubscribe_first = client.on_event(Arc::clone(&listener));
        let unsubscribe_second = client.on_event(Arc::clone(&listener));

        client.abort().await.expect("abort");
        assert_eq!(*calls.lock().expect("lock"), 2, "both registrations fire");

        *calls.lock().expect("lock") = 0;
        unsubscribe_first();
        client.abort().await.expect("abort");
        assert_eq!(*calls.lock().expect("lock"), 1, "only the second remains");

        // 同一个闭包再次调用：`indexOf` 仍能匹配同函数，于是移除第二个注册（TS 行为）。
        *calls.lock().expect("lock") = 0;
        unsubscribe_first();
        client.abort().await.expect("abort");
        assert_eq!(*calls.lock().expect("lock"), 0);

        // 找不到匹配时退订无副作用。
        unsubscribe_second();
        unsubscribe_second();
        client.abort().await.expect("abort");
        assert_eq!(*calls.lock().expect("lock"), 0);

        // 单个注册的常规幂等退订。
        let single_calls = Arc::new(Mutex::new(0usize));
        let single: kiana_rpc_print::RpcEventListener = {
            let single_calls = Arc::clone(&single_calls);
            Arc::new(move |_event: &Value| *single_calls.lock().expect("lock") += 1)
        };
        let unsubscribe_single = client.on_event(single);
        client.abort().await.expect("abort");
        assert_eq!(*single_calls.lock().expect("lock"), 1);
        unsubscribe_single();
        unsubscribe_single();
        client.abort().await.expect("abort");
        assert_eq!(*single_calls.lock().expect("lock"), 1);

        client.stop().await.expect("stop");
    });
}

/// TS `handleLine` 用 `[...this.eventListeners]` 快照派发：监听器在派发中退订不会让同一行
/// 的后续监听器漏事件，但下一行不再收到。
#[test]
fn dispatch_iterates_a_listener_snapshot() {
    run_local(async {
        let (_dir, _body_file, client) =
            start_script("listener-snapshot", EVENT_SERVER, None).await;

        let order: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
        let second_calls = Arc::new(Mutex::new(0usize));

        let second_listener: kiana_rpc_print::RpcEventListener = {
            let second_calls = Arc::clone(&second_calls);
            Arc::new(move |_event: &Value| *second_calls.lock().expect("lock") += 1)
        };
        let unsubscribe_second = client.on_event(Arc::clone(&second_listener));

        let first_listener: kiana_rpc_print::RpcEventListener = {
            let order = Arc::clone(&order);
            Arc::new(move |_event: &Value| {
                order.lock().expect("lock").push("first");
                unsubscribe_second();
            })
        };
        // 注册顺序：second 先、first 后；first 派发时退订 second。
        let _unsubscribe_first = client.on_event(first_listener);

        client.abort().await.expect("abort");
        assert_eq!(*order.lock().expect("lock"), vec!["first"]);
        assert_eq!(
            *second_calls.lock().expect("lock"),
            1,
            "snapshot keeps listeners registered at dispatch start"
        );

        client.abort().await.expect("abort");
        assert_eq!(*order.lock().expect("lock"), vec!["first", "first"]);
        assert_eq!(
            *second_calls.lock().expect("lock"),
            1,
            "removed listener no longer receives later events"
        );

        client.stop().await.expect("stop");
    });
}

/// TS `handleLine` 的 `try/catch` 吞掉非 JSON 行：既不影响事件派发，也不报错。
#[test]
fn non_json_stdout_lines_are_ignored() {
    run_local(async {
        let (_dir, _body_file, client) = start_script(
            "non-json",
            r#"#!/bin/sh
while IFS= read -r line; do
    id=$(printf '%s\n' "$line" | sed -n 's/.*"id":"\([^"]*\)"}$/\1/p')
    printf 'not json at all\n'
    printf '{"type":"agent_start"}\n'
    printf '{"id":"%s","type":"response","success":true,"data":{}}\n' "$id"
done
"#,
            None,
        )
        .await;

        let seen: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
        let listener_seen = Arc::clone(&seen);
        let _unsubscribe = client.on_event(Arc::new(move |event: &Value| {
            listener_seen.lock().expect("lock").push(event.clone());
        }));

        client.abort().await.expect("abort");
        for _ in 0..100 {
            if !seen.lock().expect("lock").is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(seen.lock().expect("lock").len(), 1);
        assert_eq!(seen.lock().expect("lock")[0]["type"], "agent_start");

        client.stop().await.expect("stop");
    });
}

/// TS `start()`：重复启动报 `Client already started`；`stop()` 之后可以重新启动；未启动时
/// `send()` 报 `Client not started`，`stop()` 是 no-op。
#[test]
fn lifecycle_matches_ts_start_stop_errors() {
    run_local(async {
        let not_started = RpcClient::new(RpcClientOptions::default());
        assert_eq!(
            not_started.get_commands().await.unwrap_err().to_string(),
            "Client not started"
        );
        not_started
            .stop()
            .await
            .expect("stop before start is a no-op");

        let (_dir, _body_file, client) = start_script("lifecycle", FAKE_SERVER, None).await;
        assert_eq!(
            client.start().await.unwrap_err().to_string(),
            "Client already started"
        );
        client.stop().await.expect("stop");

        // TS 的 `stop()` 把 `this.process` 置空，因此允许再次 `start()`。
        client.start().await.expect("restart after stop");
        client.stop().await.expect("stop");
    });
}

/// TS `start()` 的 spawn 参数顺序：`--mode rpc`、`--provider`、`--model`、然后是额外 args。
#[test]
fn start_passes_provider_model_and_extra_args_in_order() {
    run_local(async {
        let guard = SPAWN_LOCK.lock().await;
        let dir = TempDir::new("args");
        let args_file = dir.path().join("args.txt");
        let script = write_script(
            dir.path(),
            "args.sh",
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$ARGS_FILE\"\nwhile IFS= read -r line; do :; done\n",
        );
        let mut env = HashMap::new();
        env.insert(
            "ARGS_FILE".to_string(),
            Some(args_file.to_string_lossy().into_owned()),
        );
        let client = RpcClient::new(RpcClientOptions {
            cli_path: Some(script),
            env,
            provider: Some("anthropic".to_string()),
            model: Some("claude-sonnet-4-5".to_string()),
            args: vec!["--no-session".to_string(), "--foo".to_string()],
            ..RpcClientOptions::default()
        });
        client.start().await.expect("start");
        drop(guard);

        let mut recorded = String::new();
        for _ in 0..100 {
            if let Ok(text) = std::fs::read_to_string(&args_file) {
                recorded = text;
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(
            recorded.lines().collect::<Vec<_>>(),
            vec![
                "--mode",
                "rpc",
                "--provider",
                "anthropic",
                "--model",
                "claude-sonnet-4-5",
                "--no-session",
                "--foo",
            ]
        );

        client.stop().await.expect("stop");
    });
}

/// TS `start()` 的 100ms 就绪检查：子进程在这段时间内以退出码结束 -> `start()` 抛
/// `Agent process exited (code=<n> signal=null)`；`this.process` 仍指向该子进程，因此
/// 第二次 `start()` 报 `Client already started`，后续 `send()` 复用缓存的退出错误。
#[test]
fn start_reports_a_child_that_exits_during_the_ready_delay() {
    run_local(async {
        let guard = SPAWN_LOCK.lock().await;
        let dir = TempDir::new("early-exit");
        let script = write_script(dir.path(), "early.sh", "#!/bin/sh\nexit 7\n");
        let client = RpcClient::new(RpcClientOptions {
            cli_path: Some(script),
            ..RpcClientOptions::default()
        });
        drop(guard);

        let error = client.start().await.expect_err("start must fail");
        assert_eq!(
            error.to_string(),
            "Agent process exited (code=7 signal=null). Stderr: "
        );
        assert_eq!(
            client.start().await.unwrap_err().to_string(),
            "Client already started"
        );
        assert_eq!(
            client.get_commands().await.unwrap_err().to_string(),
            error.to_string()
        );
        client.stop().await.expect("stop");
    });
}

/// TS `stop()`：先 SIGTERM，最多等 1000ms；超时后 SIGKILL 并**立即返回**（不再等待退出）。
#[test]
fn stop_kills_after_the_one_second_grace_period() {
    run_local(async {
        let guard = SPAWN_LOCK.lock().await;
        let dir = TempDir::new("stop-kill");
        let pid_file = dir.path().join("pid.txt");
        let script = write_script(
            dir.path(),
            "stubborn.sh",
            "#!/bin/sh\nprintf '%s' \"$$\" > \"$PID_FILE\"\ntrap '' TERM\nwhile :; do sleep 1; done\n",
        );
        let mut env = HashMap::new();
        env.insert(
            "PID_FILE".to_string(),
            Some(pid_file.to_string_lossy().into_owned()),
        );
        let client = RpcClient::new(RpcClientOptions {
            cli_path: Some(script),
            env,
            ..RpcClientOptions::default()
        });
        client.start().await.expect("start");
        drop(guard);

        let mut pid = 0i32;
        for _ in 0..200 {
            if let Ok(text) = std::fs::read_to_string(&pid_file)
                && let Ok(parsed) = text.trim().parse::<i32>()
            {
                pid = parsed;
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(pid > 0, "child pid must be recorded");
        // 让 shell 执行到 `trap '' TERM`（pid 写入发生在它之前）。
        tokio::time::sleep(Duration::from_millis(50)).await;

        let started = std::time::Instant::now();
        client.stop().await.expect("stop");
        let elapsed = started.elapsed();
        assert!(
            elapsed >= Duration::from_millis(900),
            "stop() must wait the 1000ms SIGKILL grace period: {elapsed:?}"
        );

        // 子进程被 SIGKILL 并由 supervisor 回收后，kill(pid, 0) 报 ESRCH。
        let mut gone = false;
        for _ in 0..200 {
            // SAFETY: `kill(pid, 0)` 只探测进程是否存在。
            let result = unsafe { libc::kill(pid, 0) };
            if result == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
                gone = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(gone, "the stubborn child must be killed");
    });
}

// ============================================================================
// test/rpc.test.ts 的移植：需要真实模型凭据与 stage 8 的 `kiana` 二进制，全部 #[ignore]。
// ============================================================================

mod live_rpc_mode {
    use super::*;

    /// `kiana` 可执行文件：优先 `KIANA_TEST_BIN`，否则取测试二进制所在的 `target/<profile>/kiana`。
    fn kiana_bin() -> PathBuf {
        if let Some(path) = std::env::var_os("KIANA_TEST_BIN") {
            return PathBuf::from(path);
        }
        // 测试二进制位于 `<target>/<profile>/deps/`，`kiana` 位于 `<target>/<profile>/kiana`。
        if let Ok(exe) = std::env::current_exe()
            && let Some(profile_dir) = exe.parent().and_then(Path::parent)
        {
            return profile_dir.join("kiana");
        }
        PathBuf::from("kiana")
    }

    fn has_credentials() -> bool {
        std::env::var_os("ANTHROPIC_API_KEY").is_some()
            || std::env::var_os("ANTHROPIC_OAUTH_TOKEN").is_some()
    }

    struct LiveSession {
        client: RpcClient,
        dir: TempDir,
    }

    impl LiveSession {
        fn new(tag: &str) -> Self {
            assert!(
                has_credentials(),
                "requires ANTHROPIC_API_KEY or ANTHROPIC_OAUTH_TOKEN"
            );
            let dir = TempDir::new(tag);
            let mut env = HashMap::new();
            env.insert(
                "KIANA_CODING_AGENT_DIR".to_string(),
                Some(dir.path().to_string_lossy().into_owned()),
            );
            let client = RpcClient::new(RpcClientOptions {
                cli_path: Some(kiana_bin()),
                cwd: Some(
                    Path::new(env!("CARGO_MANIFEST_DIR"))
                        .join("../..")
                        .to_path_buf(),
                ),
                env,
                provider: Some("anthropic".to_string()),
                model: Some("claude-sonnet-4-5".to_string()),
                args: Vec::new(),
            });
            Self { client, dir }
        }

        /// 会话文件里的全部 JSON 记录（TS 直接按行 `JSON.parse`）。
        ///
        /// TS 的 `readdirSync(cwdSessionDir).filter(f => f.endsWith(".jsonl"))` 之后断言
        /// `sessionFiles.length === 1`；这里同样断言恰好一个文件（而不是“随便取一个”）。
        fn session_entries(&self) -> Vec<Value> {
            let sessions = self.dir.path().join("sessions");
            let cwd_dir = std::fs::read_dir(&sessions)
                .expect("read sessions dir")
                .next()
                .expect("at least one cwd session dir")
                .expect("cwd session dir entry")
                .path();
            let files: Vec<PathBuf> = std::fs::read_dir(&cwd_dir)
                .expect("read cwd session dir")
                .map(|entry| entry.expect("session entry").path())
                .filter(|path| {
                    path.extension()
                        .is_some_and(|extension| extension == "jsonl")
                })
                .collect();
            assert_eq!(files.len(), 1, "exactly one .jsonl session file: {files:?}");
            std::fs::read_to_string(&files[0])
                .expect("read session file")
                .lines()
                .filter(|line| !line.is_empty())
                .map(|line| serde_json::from_str(line).expect("parse session line"))
                .collect()
        }
    }

    #[test]
    #[ignore = "requires ANTHROPIC_API_KEY and the kiana binary (stage 8)"]
    fn should_get_state() {
        run_local(async {
            let live = LiveSession::new("state");
            live.client.start().await.expect("start");
            let state = live.client.get_state().await.expect("get_state");
            let model = state.model.expect("model");
            assert_eq!(model.provider, "anthropic");
            assert_eq!(model.id, "claude-sonnet-4-5");
            assert!(!state.is_streaming);
            assert_eq!(state.message_count, 0);
            live.client.stop().await.expect("stop");
        });
    }

    #[test]
    #[ignore = "requires ANTHROPIC_API_KEY and the kiana binary (stage 8)"]
    fn should_save_messages_to_session_file() {
        run_local(async {
            let live = LiveSession::new("save");
            live.client.start().await.expect("start");
            let events = live
                .client
                .prompt_and_wait("Reply with just the word 'hello'", None)
                .await
                .expect("prompt_and_wait");
            let message_ends = events
                .iter()
                .filter(|event| event["type"] == "message_end")
                .count();
            assert!(message_ends >= 2, "user + assistant message_end");
            tokio::time::sleep(Duration::from_millis(200)).await;

            let entries = live.session_entries();
            assert_eq!(entries[0]["type"], "session");
            let roles: Vec<&str> = entries
                .iter()
                .filter(|entry| entry["type"] == "message")
                .map(|entry| entry["message"]["role"].as_str().unwrap_or_default())
                .collect();
            assert!(roles.len() >= 2);
            assert!(roles.contains(&"user"));
            assert!(roles.contains(&"assistant"));
            live.client.stop().await.expect("stop");
        });
    }

    #[test]
    #[ignore = "requires ANTHROPIC_API_KEY and the kiana binary (stage 8)"]
    fn should_handle_manual_compaction() {
        run_local(async {
            let live = LiveSession::new("compact");
            live.client.start().await.expect("start");
            live.client
                .prompt_and_wait("Say hello", None)
                .await
                .expect("prompt_and_wait");
            let result = live.client.compact(None).await.expect("compact");
            assert!(result.get("summary").is_some());
            assert!(result["tokensBefore"].as_f64().unwrap_or_default() > 0.0);
            tokio::time::sleep(Duration::from_millis(200)).await;
            let entries = live.session_entries();
            let compaction_entries: Vec<&Value> = entries
                .iter()
                .filter(|entry| entry["type"] == "compaction")
                .collect();
            assert_eq!(compaction_entries.len(), 1);
            // TS: `expect(compactionEntries[0].summary).toBeDefined()`（缺失键才是 undefined）。
            assert!(
                compaction_entries[0].get("summary").is_some(),
                "persisted compaction entry must define summary: {:?}",
                compaction_entries[0]
            );
            live.client.stop().await.expect("stop");
        });
    }

    #[test]
    #[ignore = "requires ANTHROPIC_API_KEY and the kiana binary (stage 8)"]
    fn should_execute_bash_command() {
        run_local(async {
            let live = LiveSession::new("bash");
            live.client.start().await.expect("start");
            let result = live.client.bash("echo hello").await.expect("bash");
            assert_eq!(result.output.trim(), "hello");
            assert_eq!(result.exit_code, Some(0));
            assert!(!result.cancelled);
            live.client.stop().await.expect("stop");
        });
    }

    #[test]
    #[ignore = "requires ANTHROPIC_API_KEY and the kiana binary (stage 8)"]
    fn should_add_bash_output_to_context() {
        run_local(async {
            let live = LiveSession::new("bash-context");
            live.client.start().await.expect("start");
            live.client
                .prompt_and_wait("Say hi", None)
                .await
                .expect("prompt_and_wait");
            let unique = format!(
                "test-{}",
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_millis())
                    .unwrap_or_default()
            );
            live.client
                .bash(format!("echo {unique}"))
                .await
                .expect("bash");
            tokio::time::sleep(Duration::from_millis(200)).await;
            let entries = live.session_entries();
            let bash_messages: Vec<&Value> = entries
                .iter()
                .filter(|entry| {
                    entry["type"] == "message" && entry["message"]["role"] == "bashExecution"
                })
                .collect();
            assert_eq!(bash_messages.len(), 1);
            assert!(
                bash_messages[0]["message"]["output"]
                    .as_str()
                    .unwrap_or_default()
                    .contains(&unique)
            );
            live.client.stop().await.expect("stop");
        });
    }

    #[test]
    #[ignore = "requires ANTHROPIC_API_KEY and the kiana binary (stage 8)"]
    fn should_include_bash_output_in_llm_context() {
        run_local(async {
            let live = LiveSession::new("bash-llm");
            live.client.start().await.expect("start");
            let unique = format!(
                "unique-{}",
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|duration| duration.as_millis())
                    .unwrap_or_default()
            );
            live.client
                .bash(format!("echo {unique}"))
                .await
                .expect("bash");
            let events = live
                .client
                .prompt_and_wait(
                    "What was the exact output of the echo command I just ran? Reply with just the value, nothing else.",
                    None,
                )
                .await
                .expect("prompt_and_wait");
            let assistant = events
                .iter()
                .find(|event| {
                    event["type"] == "message_end" && event["message"]["role"] == "assistant"
                })
                .expect("assistant message_end");
            let text = assistant["message"]["content"]
                .as_array()
                .and_then(|content| content.iter().find(|block| block["type"] == "text"))
                .and_then(|block| block["text"].as_str())
                .unwrap_or_default();
            assert!(text.contains(&unique), "assistant text: {text}");
            live.client.stop().await.expect("stop");
        });
    }

    #[test]
    #[ignore = "requires ANTHROPIC_API_KEY and the kiana binary (stage 8)"]
    fn should_set_and_get_thinking_level() {
        run_local(async {
            let live = LiveSession::new("thinking-set");
            live.client.start().await.expect("start");
            live.client
                .set_thinking_level(ThinkingLevel::High)
                .await
                .expect("set_thinking_level");
            let state = live.client.get_state().await.expect("get_state");
            assert_eq!(state.thinking_level, ThinkingLevel::High);
            live.client.stop().await.expect("stop");
        });
    }

    #[test]
    #[ignore = "requires ANTHROPIC_API_KEY and the kiana binary (stage 8)"]
    fn should_cycle_thinking_level() {
        run_local(async {
            let live = LiveSession::new("thinking-cycle");
            live.client.start().await.expect("start");
            let initial = live
                .client
                .get_state()
                .await
                .expect("get_state")
                .thinking_level;
            let cycled = live
                .client
                .cycle_thinking_level()
                .await
                .expect("cycle_thinking_level")
                .expect("level");
            assert_ne!(cycled.level, initial);
            let state = live.client.get_state().await.expect("get_state");
            assert_eq!(state.thinking_level, cycled.level);
            live.client.stop().await.expect("stop");
        });
    }

    #[test]
    #[ignore = "requires ANTHROPIC_API_KEY and the kiana binary (stage 8)"]
    fn should_get_available_thinking_levels() {
        run_local(async {
            let live = LiveSession::new("thinking-levels");
            live.client.start().await.expect("start");
            let levels = live
                .client
                .get_available_thinking_levels()
                .await
                .expect("levels");
            assert!(!levels.is_empty());
            let state = live.client.get_state().await.expect("get_state");
            assert!(levels.contains(&state.thinking_level));
            if let Some(cycled) = live.client.cycle_thinking_level().await.expect("cycle") {
                assert!(levels.contains(&cycled.level));
                if levels.len() > 1 {
                    assert_ne!(cycled.level, state.thinking_level);
                }
            }
            live.client.stop().await.expect("stop");
        });
    }

    #[test]
    #[ignore = "requires ANTHROPIC_API_KEY and the kiana binary (stage 8)"]
    fn should_get_available_models() {
        run_local(async {
            let live = LiveSession::new("models");
            live.client.start().await.expect("start");
            let models = live.client.get_available_models().await.expect("models");
            assert!(!models.is_empty());
            for model in models {
                assert!(!model.provider.is_empty());
                assert!(!model.id.is_empty());
                assert!(model.context_window > 0);
                // TS：`expect(typeof model.reasoning).toBe("boolean")`（真实服务端总是
                // 布尔值）。Rust 的 `ModelInfo.reasoning: bool` 对非布尔值按 JS 真值映射。
                let _: bool = model.reasoning;
            }
            live.client.stop().await.expect("stop");
        });
    }

    #[test]
    #[ignore = "requires ANTHROPIC_API_KEY and the kiana binary (stage 8)"]
    fn should_get_session_stats() {
        run_local(async {
            let live = LiveSession::new("stats");
            live.client.start().await.expect("start");
            live.client
                .prompt_and_wait("Hello", None)
                .await
                .expect("prompt_and_wait");
            let stats = live
                .client
                .get_session_stats()
                .await
                .expect("get_session_stats");
            assert!(stats.session_file.is_some());
            assert!(!stats.session_id.is_empty());
            assert!(stats.user_messages >= 1);
            assert!(stats.assistant_messages >= 1);
            live.client.stop().await.expect("stop");
        });
    }

    #[test]
    #[ignore = "requires ANTHROPIC_API_KEY and the kiana binary (stage 8)"]
    fn should_create_new_session() {
        run_local(async {
            let live = LiveSession::new("new-session");
            live.client.start().await.expect("start");
            live.client
                .prompt_and_wait("Hello", None)
                .await
                .expect("prompt_and_wait");
            assert!(live.client.get_state().await.expect("state").message_count > 0);
            let result = live.client.new_session(None).await.expect("new_session");
            assert!(!result.cancelled);
            assert_eq!(
                live.client.get_state().await.expect("state").message_count,
                0
            );
            live.client.stop().await.expect("stop");
        });
    }

    #[test]
    #[ignore = "requires ANTHROPIC_API_KEY and the kiana binary (stage 8)"]
    fn should_export_to_html() {
        run_local(async {
            let live = LiveSession::new("export");
            live.client.start().await.expect("start");
            live.client
                .prompt_and_wait("Hello", None)
                .await
                .expect("prompt_and_wait");
            let result = live.client.export_html(None).await.expect("export_html");
            assert!(result.path.ends_with(".html"));
            assert!(Path::new(&result.path).exists());
            live.client.stop().await.expect("stop");
        });
    }

    #[test]
    #[ignore = "requires ANTHROPIC_API_KEY and the kiana binary (stage 8)"]
    fn should_get_last_assistant_text() {
        run_local(async {
            let live = LiveSession::new("last-text");
            live.client.start().await.expect("start");
            assert!(
                live.client
                    .get_last_assistant_text()
                    .await
                    .expect("initial")
                    .is_none()
            );
            live.client
                .prompt_and_wait("Reply with just: test123", None)
                .await
                .expect("prompt_and_wait");
            let text = live
                .client
                .get_last_assistant_text()
                .await
                .expect("text")
                .expect("some text");
            assert!(text.contains("test123"));
            live.client.stop().await.expect("stop");
        });
    }

    #[test]
    #[ignore = "requires ANTHROPIC_API_KEY and the kiana binary (stage 8)"]
    fn should_get_session_entries_with_since_cursor() {
        run_local(async {
            let live = LiveSession::new("entries");
            live.client.start().await.expect("start");
            live.client
                .prompt_and_wait("Reply with just 'ok'", None)
                .await
                .expect("prompt_and_wait");
            let all = live.client.get_entries(None).await.expect("get_entries");
            assert!(all.entries.len() >= 2);
            for entry in &all.entries {
                assert!(!entry.id().is_empty());
            }
            let last_id = all.entries.last().expect("entry").id().to_string();
            assert_eq!(all.leaf_id.as_deref(), Some(last_id.as_str()));

            let first_id = all.entries[0].id().to_string();
            let since = live
                .client
                .get_entries(Some(first_id))
                .await
                .expect("since");
            let since_ids: Vec<&str> = since.entries.iter().map(|entry| entry.id()).collect();
            let expected: Vec<&str> = all.entries[1..].iter().map(|entry| entry.id()).collect();
            assert_eq!(since_ids, expected);
            assert_eq!(since.leaf_id, all.leaf_id);

            let error = live
                .client
                .get_entries(Some("nonexistent-id".to_string()))
                .await
                .expect_err("unknown since");
            assert!(error.to_string().contains("Entry not found"));
            live.client.stop().await.expect("stop");
        });
    }

    #[test]
    #[ignore = "requires ANTHROPIC_API_KEY and the kiana binary (stage 8)"]
    fn should_get_session_tree() {
        run_local(async {
            let live = LiveSession::new("tree");
            live.client.start().await.expect("start");
            live.client
                .prompt_and_wait("Reply with just 'ok'", None)
                .await
                .expect("prompt_and_wait");
            let entries = live.client.get_entries(None).await.expect("entries");
            let tree = live.client.get_tree().await.expect("tree");
            assert_eq!(tree.leaf_id, entries.leaf_id);
            assert_eq!(tree.tree.len(), 1);

            let mut chain: Vec<String> = Vec::new();
            let mut nodes = &tree.tree;
            while nodes.len() == 1 {
                chain.push(nodes[0].entry.id().to_string());
                nodes = &nodes[0].children;
            }
            assert!(nodes.is_empty());
            let entry_ids: Vec<String> = entries
                .entries
                .iter()
                .map(|entry| entry.id().to_string())
                .collect();
            assert_eq!(chain, entry_ids);
            live.client.stop().await.expect("stop");
        });
    }

    #[test]
    #[ignore = "requires ANTHROPIC_API_KEY and the kiana binary (stage 8)"]
    fn should_retain_pre_compaction_entries_in_get_entries() {
        run_local(async {
            let live = LiveSession::new("retain");
            live.client.start().await.expect("start");
            live.client
                .prompt_and_wait("Reply with just 'ok'", None)
                .await
                .expect("prompt_and_wait");
            let before = live.client.get_entries(None).await.expect("before");
            live.client.compact(None).await.expect("compact");
            let after = live.client.get_entries(None).await.expect("after");
            let prefix: Vec<&str> = after.entries[..before.entries.len()]
                .iter()
                .map(|entry| entry.id())
                .collect();
            let expected: Vec<&str> = before.entries.iter().map(|entry| entry.id()).collect();
            assert_eq!(prefix, expected);
            assert!(
                after
                    .entries
                    .iter()
                    .any(|entry| entry.kind() == "compaction")
            );
            live.client.stop().await.expect("stop");
        });
    }

    #[test]
    #[ignore = "requires ANTHROPIC_API_KEY and the kiana binary (stage 8)"]
    fn should_set_and_get_session_name() {
        run_local(async {
            let live = LiveSession::new("name");
            live.client.start().await.expect("start");
            assert!(
                live.client
                    .get_state()
                    .await
                    .expect("state")
                    .session_name
                    .is_none()
            );
            live.client
                .prompt_and_wait("Reply with just 'ok'", None)
                .await
                .expect("prompt_and_wait");
            live.client
                .set_session_name("my-test-session")
                .await
                .expect("set_session_name");
            let state = live.client.get_state().await.expect("state");
            assert_eq!(state.session_name.as_deref(), Some("my-test-session"));
            tokio::time::sleep(Duration::from_millis(200)).await;
            let entries = live.session_entries();
            let names: Vec<&Value> = entries
                .iter()
                .filter(|entry| entry["type"] == "session_info")
                .collect();
            assert_eq!(names.len(), 1);
            assert_eq!(names[0]["name"], "my-test-session");
            live.client.stop().await.expect("stop");
        });
    }
}
