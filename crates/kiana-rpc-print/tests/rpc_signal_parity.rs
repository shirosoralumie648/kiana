//! 真实 OS 信号回归（评审发现 9）：`shutdown()` 开头把信号处置还原为注册前的值（历史出处：TS `process.off`）。
//!
//! TS 在 `shutdown` 开头 `process.off` 掉监听器，Node 随即恢复信号的默认处置，
//! 因此 dispose 期间的第二个信号会直接终止进程（父进程看到
//! `signal=SIGTERM/SIGHUP`、`code=null`），且 `runRpcMode` 返回后不再留有处理器。
//! Rust 用 `sigaction` + self-pipe 复刻（`crate::signal`），本文件验证：
//!
//! 1. 第一个真实信号启动关停，并在 `dispose()` 之前把 SIGTERM/SIGHUP 的处置还原为
//!    注册前的值（测试进程里是 `SIG_DFL`）；
//! 2. 关停结束后处置仍是默认值（嵌入式调用方的信号不会被吞掉）。
//!
//! 第二个信号本身不能发给测试进程（默认处置会杀掉测试二进制），因此用
//! `sigaction(sig, NULL, &current)` 断言处置已还原——这正是“第二信号按默认动作退出”的
//! 内核层前提。
//!
//! 这个测试会向自身发送真实信号，因此单独放在一个测试二进制里：`cargo test` 的
//! 其它测试二进制（进程）有自己的信号监听器，不能被这里误伤。

#![allow(clippy::arc_with_non_send_sync)]

use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use async_trait::async_trait;
use kiana_ext_api::LocalBoxFuture;
use kiana_ext_api::types::{ForkOptions, NewSessionOptions, SwitchSessionOptions};
use kiana_rpc_print::RawStdout;
use kiana_rpc_print::rpc_mode::{AgentSessionFacade, RpcServer, RpcSessionHost};
use kiana_session::agent_session::{
    AgentSession, AgentSessionEventListener, AgentSessionUnsubscribe, ExtensionBindings,
};
use kiana_session::agent_session_runtime::{ForkSessionResult, SessionReplacementResult};
use tokio::io::{AsyncRead, ReadBuf};

#[derive(Default)]
struct SinkStdout {
    writes: AtomicUsize,
    flushes: AtomicUsize,
}

impl RawStdout for SinkStdout {
    fn write_raw(&self, text: &str) {
        if !text.is_empty() {
            self.writes.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn wait_for_backpressure(&self) -> kiana_ext_api::BoxFuture<'_, ()> {
        Box::pin(async {})
    }

    fn flush(&self) -> kiana_ext_api::BoxFuture<'_, ()> {
        self.flushes.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {})
    }

    fn take_over(&self) {}
}

/// stdin 永远没有数据，也没有 EOF。
struct NeverReader;

impl AsyncRead for NeverReader {
    fn poll_read(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        _buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Poll::Pending
    }
}

#[derive(Default)]
struct SignalFacade {
    binds: AtomicUsize,
}

#[async_trait(?Send)]
impl AgentSessionFacade for SignalFacade {
    fn agent_session(&self) -> AgentSession {
        panic!("the signal test overrides every method the server calls")
    }

    async fn bind_extensions(&self, _bindings: ExtensionBindings) {
        self.binds.fetch_add(1, Ordering::SeqCst);
    }

    fn subscribe(&self, _listener: AgentSessionEventListener) -> AgentSessionUnsubscribe {
        Box::new(|| {})
    }
}

struct SignalHost {
    facade: Arc<SignalFacade>,
    disposed: AtomicUsize,
    gate: Mutex<Option<Arc<tokio::sync::Notify>>>,
}

#[async_trait(?Send)]
impl RpcSessionHost for SignalHost {
    fn session(&self) -> Arc<dyn AgentSessionFacade> {
        Arc::clone(&self.facade) as Arc<dyn AgentSessionFacade>
    }

    fn set_rebind_session(
        &self,
        _callback: Arc<dyn Fn() -> LocalBoxFuture<'static, ()> + Send + Sync>,
    ) {
    }

    async fn new_session(
        &self,
        _options: Option<NewSessionOptions>,
    ) -> Result<SessionReplacementResult, String> {
        Err("unused".to_string())
    }

    async fn switch_session(
        &self,
        _session_path: String,
        _options: Option<SwitchSessionOptions>,
    ) -> Result<SessionReplacementResult, String> {
        Err("unused".to_string())
    }

    async fn fork(
        &self,
        _entry_id: String,
        _options: Option<ForkOptions>,
    ) -> Result<ForkSessionResult, String> {
        Err("unused".to_string())
    }

    async fn dispose(&self) {
        self.disposed.fetch_add(1, Ordering::SeqCst);
        let gate = self
            .gate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        if let Some(gate) = gate {
            gate.notified().await;
        }
    }
}

async fn wait_until(mut predicate: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !predicate() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for condition"
        );
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}

fn send_signal(signal: libc::c_int) {
    // SAFETY: `kill(getpid(), signal)` 只向本进程发送标准信号。
    let result = unsafe { libc::kill(libc::getpid(), signal) };
    assert_eq!(result, 0, "kill({signal}) must succeed");
}

/// 当前 `sigaction` 处置。
fn current_disposition(signal: libc::c_int) -> libc::sigaction {
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    // SAFETY: 第三个参数为 NULL 时 `sigaction` 只读取当前处置。
    let result = unsafe { libc::sigaction(signal, std::ptr::null(), &mut action) };
    assert_eq!(result, 0, "sigaction({signal}) must succeed");
    action
}

fn assert_default_disposition(signal: libc::c_int) {
    let action = current_disposition(signal);
    assert_eq!(
        action.sa_sigaction,
        libc::SIG_DFL,
        "signal {signal} must be restored to the default disposition"
    );
}

/// 跑一次“真实信号启动关停并卡在 dispose”的场景。
///
/// shutdown 一开始就执行 `process.off` 等价物（还原处置），因此 `dispose()` 还挂着时
/// SIGTERM/SIGHUP 已是默认处置——第二个信号会直接终止进程。
async fn assert_signal_restores_dispositions_while_dispose_hangs(
    first: libc::c_int,
    expected_code: i32,
    expects_flush: bool,
) {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let facade = Arc::new(SignalFacade::default());
            let host = Arc::new(SignalHost {
                facade: Arc::clone(&facade),
                disposed: AtomicUsize::new(0),
                gate: Mutex::new(None),
            });
            let gate = Arc::new(tokio::sync::Notify::new());
            *host
                .gate
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Arc::clone(&gate));
            let stdout = Arc::new(SinkStdout::default());
            let server = RpcServer::new(
                Arc::clone(&host) as Arc<dyn RpcSessionHost>,
                Arc::clone(&stdout) as Arc<dyn RawStdout>,
            );

            let run = tokio::task::spawn_local(Arc::clone(&server).run_with_input(NeverReader));
            wait_until(|| facade.binds.load(Ordering::SeqCst) >= 1).await;
            // `register_signal_handlers()` 紧随 `rebind_session()`；等到它安装完毕再发信号，
            // 否则未注册的 SIGTERM 会直接杀掉测试进程。
            tokio::time::sleep(Duration::from_millis(100)).await;

            // 注册期间：处置是我们自己的处理器。
            assert_ne!(
                current_disposition(libc::SIGTERM).sa_sigaction,
                libc::SIG_DFL,
                "SIGTERM handler must be installed while running"
            );

            // 第一个信号：开始 shutdown，dispose() 挂起。
            send_signal(first);
            wait_until(|| host.disposed.load(Ordering::SeqCst) == 1).await;

            // shutdown 已经进入 dispose：`process.off` 已执行，两个信号都恢复默认处置。
            assert_default_disposition(libc::SIGTERM);
            assert_default_disposition(libc::SIGHUP);

            // 放开 dispose，让第一个 shutdown 收尾。
            gate.notify_waiters();
            let code = tokio::time::timeout(Duration::from_secs(2), run)
                .await
                .expect("the first signal must complete the shutdown")
                .expect("run task");
            assert_eq!(code, expected_code);
            assert_eq!(host.disposed.load(Ordering::SeqCst), 1);
            // SIGTERM 的 shutdown 不 flush；SIGHUP 的 flush 一次。
            assert_eq!(
                stdout.flushes.load(Ordering::SeqCst),
                usize::from(expects_flush)
            );

            // 关停结束：嵌入式调用方拿回默认处置。
            assert_default_disposition(libc::SIGTERM);
            assert_default_disposition(libc::SIGHUP);
        })
        .await;
}

/// EOF 已经触发 `shutdown(0)` 并卡在 `dispose()` 时，处置同样已还原（第二个信号会
/// 按默认动作终止进程），且关停结束后保持默认。
async fn assert_eof_shutdown_restores_dispositions() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let facade = Arc::new(SignalFacade::default());
            let host = Arc::new(SignalHost {
                facade: Arc::clone(&facade),
                disposed: AtomicUsize::new(0),
                gate: Mutex::new(None),
            });
            let gate = Arc::new(tokio::sync::Notify::new());
            *host
                .gate
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Arc::clone(&gate));
            let stdout = Arc::new(SinkStdout::default());
            let server = RpcServer::new(
                Arc::clone(&host) as Arc<dyn RpcSessionHost>,
                Arc::clone(&stdout) as Arc<dyn RawStdout>,
            );

            // 空输入：读完 EOF 后进入 `finish_run`，`dispose()` 挂起。
            let run = tokio::task::spawn_local(Arc::clone(&server).run_with_input(&b""[..]));
            wait_until(|| host.disposed.load(Ordering::SeqCst) == 1).await;

            assert_default_disposition(libc::SIGTERM);
            assert_default_disposition(libc::SIGHUP);

            gate.notify_waiters();
            let code = tokio::time::timeout(Duration::from_secs(2), run)
                .await
                .expect("EOF shutdown completes after dispose is released")
                .expect("run task");
            assert_eq!(code, 0);
            assert_eq!(stdout.flushes.load(Ordering::SeqCst), 1);
            assert_default_disposition(libc::SIGTERM);
            assert_default_disposition(libc::SIGHUP);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn shutdown_restores_default_signal_dispositions() {
    assert_signal_restores_dispositions_while_dispose_hangs(libc::SIGTERM, 143, false).await;
    assert_signal_restores_dispositions_while_dispose_hangs(libc::SIGHUP, 129, true).await;
    assert_eof_shutdown_restores_dispositions().await;
}
