//! 退出码矩阵（spec §3.4）的端到端 seam 测试：RPC 的 EOF / 扩展 shutdownHandler /
//! SIGTERM / SIGHUP，以及 print 模式的同组信号语义。
//!
//! 这些测试只使用各生产单元冻结的 seam，不新增任何 hook：
//!
//! - RPC：`RpcServer::run_with_input`（stdin EOF -> 0）、`rebind_session` 捕获
//!   `ExtensionBindings::shutdown_handler` 后 `check_shutdown_requested`（-> 0）、
//!   `RunSignal::{Term,Hup}` 的纯映射与 `shutdown_for_signal`（143/129 + flush 规则）。
//! - print：`print_signal_exit_code` / `print_signal_skips_flush` 与
//!   `run_print_mode_signal_for_test`（同一组退出码 143/129、dispose 恰好一次、stdout 零字节）。
//!   注意 print 的 flush 规则与 RPC 不同：TS `print-mode.ts` 的两条信号路径都不调用
//!   `flushRawStdout`（spec §2.3.2、§4.7），只有 RPC 的 SIGHUP 才 flush（spec §2.6.9）。
//!
//! 所有 `run_*_mode` / `run_with_input` future 都是 `!Send`，因此在
//! `tokio::task::LocalSet` 里驱动。真实 OS 信号只能用 `kiana` 二进制在子进程里验证
//! （`kiana` 由 kiana-coding-agent stage 8 提供），这些用例全部 `#[ignore]` 且仅 `cfg(unix)`。
//!
//! TS 对应：`shutdown()` 的 `process.exit(143/129)` 分支，见 `rpc-mode.ts`
//! `registerSignalHandlers` 与 `print-mode.ts` 的同一逻辑。Rust 侧不调用
//! `process::exit`，退出码通过返回值表达，便于断言 dispose/flush 的顺序。

#![allow(clippy::arc_with_non_send_sync)]

use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use async_trait::async_trait;
use kiana_ext_api::types::{ForkOptions, NewSessionOptions, SwitchSessionOptions};
use kiana_ext_api::{BoxFuture, LocalBoxFuture};
use kiana_rpc_print::rpc_mode::{AgentSessionFacade, RpcServer, RpcSessionHost, RunSignal};
use kiana_rpc_print::{
    PrintModeOptions, PrintRuntimeHost, PrintSession, PrintSignal, RawStdout,
    print_signal_exit_code, print_signal_skips_flush, run_print_mode_signal_for_test,
};
use kiana_session::agent_session::{
    AgentSession, AgentSessionEventListener, AgentSessionUnsubscribe, ExtensionBindings,
};
use kiana_session::agent_session_runtime::{ForkSessionResult, SessionReplacementResult};

// ============================================================================
// 共享夹具
// ============================================================================

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 在 `LocalSet` 上驱动 `!Send` future（与生产调用方一致）。
fn run<F: Future>(future: F) -> F::Output {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    runtime.block_on(tokio::task::LocalSet::new().run_until(future))
}

/// mock 宿主与 stdout 共享的事件序列，用于断言 “dispose 在返回退出码之前”。
#[derive(Clone, Default)]
struct OrderLog(Arc<Mutex<Vec<String>>>);

impl OrderLog {
    fn push(&self, event: &str) {
        lock(&self.0).push(event.to_string());
    }

    fn events(&self) -> Vec<String> {
        lock(&self.0).clone()
    }
}

/// 记录写入的 [`RawStdout`] 替身（不触进程级 stdout）。
struct CaptureStdout {
    writes: Mutex<Vec<String>>,
    flush_calls: AtomicUsize,
    wait_calls: AtomicUsize,
    take_overs: AtomicUsize,
    order: OrderLog,
}

impl CaptureStdout {
    fn new(order: OrderLog) -> Self {
        Self {
            writes: Mutex::new(Vec::new()),
            flush_calls: AtomicUsize::new(0),
            wait_calls: AtomicUsize::new(0),
            take_overs: AtomicUsize::new(0),
            order,
        }
    }

    fn writes(&self) -> Vec<String> {
        lock(&self.writes).clone()
    }

    fn flush_calls(&self) -> usize {
        self.flush_calls.load(Ordering::SeqCst)
    }

    fn take_overs(&self) -> usize {
        self.take_overs.load(Ordering::SeqCst)
    }
}

impl RawStdout for CaptureStdout {
    fn write_raw(&self, text: &str) {
        if !text.is_empty() {
            lock(&self.writes).push(text.to_string());
        }
    }

    fn wait_for_backpressure(&self) -> BoxFuture<'_, ()> {
        self.wait_calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {})
    }

    fn flush(&self) -> BoxFuture<'_, ()> {
        self.flush_calls.fetch_add(1, Ordering::SeqCst);
        self.order.push("flush");
        Box::pin(async {})
    }

    fn take_over(&self) {
        self.take_overs.fetch_add(1, Ordering::SeqCst);
    }
}

// ============================================================================
// RPC 服务端夹具
// ============================================================================

/// 只覆写服务端实际调用的三个方法（其余默认体都会委托给会 panic 的 `agent_session`）。
struct MockFacade {
    binds: AtomicUsize,
    bindings: Mutex<Option<ExtensionBindings>>,
}

impl MockFacade {
    fn new() -> Self {
        Self {
            binds: AtomicUsize::new(0),
            bindings: Mutex::new(None),
        }
    }

    fn bind_count(&self) -> usize {
        self.binds.load(Ordering::SeqCst)
    }

    fn take_bindings(&self) -> Option<ExtensionBindings> {
        lock(&self.bindings).take()
    }
}

#[async_trait(?Send)]
impl AgentSessionFacade for MockFacade {
    fn agent_session(&self) -> AgentSession {
        panic!("the exit-code test overrides every method the server calls")
    }

    async fn bind_extensions(&self, bindings: ExtensionBindings) {
        self.binds.fetch_add(1, Ordering::SeqCst);
        *lock(&self.bindings) = Some(bindings);
    }

    fn subscribe(&self, _listener: AgentSessionEventListener) -> AgentSessionUnsubscribe {
        Box::new(|| {})
    }
}

/// [`RpcSessionHost`] 的 mock：dispose 计数并写入顺序日志。
struct MockHost {
    facade: Arc<MockFacade>,
    dispose_calls: AtomicUsize,
    order: OrderLog,
}

impl MockHost {
    fn new(facade: Arc<MockFacade>, order: OrderLog) -> Self {
        Self {
            facade,
            dispose_calls: AtomicUsize::new(0),
            order,
        }
    }

    fn dispose_calls(&self) -> usize {
        self.dispose_calls.load(Ordering::SeqCst)
    }
}

#[async_trait(?Send)]
impl RpcSessionHost for MockHost {
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
        self.dispose_calls.fetch_add(1, Ordering::SeqCst);
        self.order.push("dispose");
    }
}

// ============================================================================
// print 模式夹具
// ============================================================================

type RebindCallback = Arc<dyn Fn() -> LocalBoxFuture<'static, ()> + Send + Sync>;

/// [`PrintRuntimeHost`] 的 mock：信号路径只调用 `dispose()`。
struct MockPrintHost {
    dispose_calls: AtomicUsize,
    order: OrderLog,
}

impl MockPrintHost {
    fn new(order: OrderLog) -> Self {
        Self {
            dispose_calls: AtomicUsize::new(0),
            order,
        }
    }

    fn dispose_calls(&self) -> usize {
        self.dispose_calls.load(Ordering::SeqCst)
    }
}

#[async_trait(?Send)]
impl PrintRuntimeHost for MockPrintHost {
    fn session(&self) -> Arc<dyn PrintSession> {
        panic!("the print signal path disposes the runtime without reading the session")
    }

    fn set_rebind_session(&self, _callback: RebindCallback) {}

    async fn new_session(
        &self,
        _options: Option<NewSessionOptions>,
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

    async fn switch_session(
        &self,
        _session_path: String,
        _options: Option<SwitchSessionOptions>,
    ) -> Result<SessionReplacementResult, String> {
        Err("unused".to_string())
    }

    async fn dispose(&self) {
        self.dispose_calls.fetch_add(1, Ordering::SeqCst);
        self.order.push("dispose");
    }
}

// ============================================================================
// RPC：stdin EOF -> 0
// ============================================================================

/// spec §3.4：RPC stdin `end` -> 0；dispose 恰好一次，随后 flush，再返回退出码。
#[test]
fn rpc_stdin_eof_exits_with_zero() {
    run(async {
        let order = OrderLog::default();
        let facade = Arc::new(MockFacade::new());
        let host = Arc::new(MockHost::new(Arc::clone(&facade), order.clone()));
        let io = Arc::new(CaptureStdout::new(order.clone()));
        let server = RpcServer::new(
            Arc::clone(&host) as Arc<dyn RpcSessionHost>,
            Arc::clone(&io) as Arc<dyn RawStdout>,
        );

        let empty: &[u8] = &[];
        let code = Arc::clone(&server).run_with_input(empty).await;
        order.push("returned");

        assert_eq!(code, 0);
        assert_eq!(
            facade.bind_count(),
            1,
            "rebindSession binds extensions once"
        );
        assert_eq!(host.dispose_calls(), 1, "dispose runs exactly once");
        assert_eq!(io.flush_calls(), 1, "EOF shutdown flushes stdout");
        assert!(io.writes().is_empty(), "no protocol records on empty stdin");
        assert_eq!(
            io.take_overs(),
            0,
            "the server never takes over stdout itself"
        );
        // dispose 在 flush 之前，二者都在退出码返回之前。
        assert_eq!(order.events(), vec!["dispose", "flush", "returned"]);
    });
}

// ============================================================================
// RPC：扩展 shutdownHandler -> 0
// ============================================================================

/// spec §3.4：扩展请求关停（`shutdownHandler`）-> 0；经 `agent_settled`/命令结束检查后生效。
#[test]
fn rpc_extension_shutdown_handler_exits_with_zero() {
    run(async {
        let order = OrderLog::default();
        let facade = Arc::new(MockFacade::new());
        let host = Arc::new(MockHost::new(Arc::clone(&facade), order.clone()));
        let io = Arc::new(CaptureStdout::new(order.clone()));
        let server = RpcServer::new(
            Arc::clone(&host) as Arc<dyn RpcSessionHost>,
            Arc::clone(&io) as Arc<dyn RawStdout>,
        );

        // 未请求关停时什么都不做。
        assert_eq!(server.check_shutdown_requested().await, None);
        assert_eq!(host.dispose_calls(), 0);

        // `rebindSession()` 把扩展绑定交出去；捕获其中的 shutdownHandler。
        server.rebind_session().await;
        let bindings = facade.take_bindings().expect("bindExtensions was called");
        let shutdown_handler = bindings
            .shutdown_handler
            .expect("RPC mode installs a shutdown handler");
        shutdown_handler();

        let code = server.check_shutdown_requested().await;
        order.push("returned");

        assert_eq!(code, Some(0));
        assert_eq!(host.dispose_calls(), 1, "dispose runs exactly once");
        assert_eq!(io.flush_calls(), 1, "extension shutdown is not SIGTERM");
        assert!(io.writes().is_empty());
        assert_eq!(order.events(), vec!["dispose", "flush", "returned"]);
    });
}

// ============================================================================
// RPC：纯映射与 shutdown_for_signal
// ============================================================================

/// 纯映射：SIGTERM -> 143（跳过 flush），SIGHUP -> 129（flush）。
#[test]
fn run_signal_mapping_matches_the_exit_code_matrix() {
    assert_eq!(RunSignal::Term.exit_code(), 143);
    assert_eq!(RunSignal::Hup.exit_code(), 129);
    assert!(RunSignal::Term.skips_flush());
    assert!(!RunSignal::Hup.skips_flush());
}

/// `shutdown_for_signal`：退出码、flush 次数、dispose 顺序与次数。
#[test]
fn shutdown_for_signal_matches_the_exit_code_matrix() {
    run(async {
        for (signal, expected_code, expected_flushes) in [
            (RunSignal::Term, 143i32, 0usize),
            (RunSignal::Hup, 129i32, 1usize),
        ] {
            let order = OrderLog::default();
            let facade = Arc::new(MockFacade::new());
            let host = Arc::new(MockHost::new(facade, order.clone()));
            let io = Arc::new(CaptureStdout::new(order.clone()));
            let server = RpcServer::new(
                Arc::clone(&host) as Arc<dyn RpcSessionHost>,
                Arc::clone(&io) as Arc<dyn RawStdout>,
            );

            let code = server.shutdown_for_signal(Some(signal)).await;
            order.push("returned");

            assert_eq!(code, expected_code);
            assert_eq!(code, signal.exit_code());
            assert_eq!(host.dispose_calls(), 1, "dispose runs exactly once");
            assert_eq!(io.flush_calls(), expected_flushes);
            assert!(io.writes().is_empty());
            let mut expected = vec!["dispose"];
            if expected_flushes == 1 {
                expected.push("flush");
            }
            expected.push("returned");
            assert_eq!(order.events(), expected);
        }
    });
}

/// 重入关停模拟 TS 的 `process.exit(exitCode)`：第二个信号保持自己的退出码，
/// 不重复 dispose，也不 flush（第一个 SIGTERM 已跳过 flush）。
#[test]
fn reentrant_signal_shutdown_keeps_its_own_exit_code_without_second_dispose() {
    run(async {
        let order = OrderLog::default();
        let facade = Arc::new(MockFacade::new());
        let host = Arc::new(MockHost::new(facade, order.clone()));
        let io = Arc::new(CaptureStdout::new(order.clone()));
        let server = RpcServer::new(
            Arc::clone(&host) as Arc<dyn RpcSessionHost>,
            Arc::clone(&io) as Arc<dyn RawStdout>,
        );

        assert_eq!(server.shutdown_for_signal(Some(RunSignal::Term)).await, 143);
        assert_eq!(server.shutdown_for_signal(Some(RunSignal::Hup)).await, 129);
        assert_eq!(host.dispose_calls(), 1);
        assert_eq!(io.flush_calls(), 0);
        assert_eq!(order.events(), vec!["dispose"]);
    });
}

// ============================================================================
// print：同一退出码矩阵
// ============================================================================

/// print 的纯映射与 `run_print_mode_signal_for_test`：退出码与 RPC 相同（143/129），
/// 但 flush 规则不同——TS `print-mode.ts` 的 handler 只做 kill -> dispose ->
/// `process.exit(129|143)`，两条信号路径都**不** flush（spec §2.3.2、§4.7）。
#[test]
fn print_signal_seams_match_the_ts_exit_code_matrix() {
    run(async {
        for (signal, expected_code, expected_flushes) in [
            (PrintSignal::Term, 143i32, 0usize),
            (PrintSignal::Hup, 129i32, 0usize),
        ] {
            let order = OrderLog::default();
            let host = Arc::new(MockPrintHost::new(order.clone()));
            let io = Arc::new(CaptureStdout::new(order.clone()));

            let code = run_print_mode_signal_for_test(
                Arc::clone(&host) as Arc<dyn PrintRuntimeHost>,
                PrintModeOptions::default(),
                Arc::clone(&io) as Arc<dyn RawStdout>,
                signal,
            )
            .await;
            order.push("returned");

            assert_eq!(code, print_signal_exit_code(signal));
            assert_eq!(code, expected_code);
            assert!(
                print_signal_skips_flush(signal),
                "TS print-mode.ts never flushes on a signal"
            );
            assert_eq!(host.dispose_calls(), 1, "dispose runs exactly once");
            assert_eq!(io.flush_calls(), expected_flushes);
            assert!(io.writes().is_empty(), "signal path writes no stdout bytes");
            // 回归（review finding，major）：SIGHUP 不得 flush——TS 的 handler 只做
            // kill -> dispose -> process.exit(129)，等待中的 raw stdout 写入直接丢弃。
            // 修复前 HUP 会多一条 "flush"（且 flushRawStdout 的错误可能把 129 变成 1）。
            assert_eq!(order.events(), vec!["dispose", "returned"]);
        }
    });
}

// ============================================================================
// 真实 OS 信号（需要 `kiana` 二进制 -> #[ignore]）
// ============================================================================

/// 子进程场景需要 kiana-coding-agent stage 8 产出的 `kiana` 二进制。默认不运行；设置
/// `KIANA_BIN=/path/to/kiana` 后可用 `cargo test -p kiana-rpc-print --test signal_exit_codes -- --ignored`
/// 验证。断言 TS 的 `process.exit(143/129)` 语义（信号 -> 主动退出码，而不是被信号杀死）。
#[cfg(unix)]
mod real_os_signals {
    use std::process::{Child, Command, Stdio};
    use std::time::Duration;

    fn kiana_binary() -> std::path::PathBuf {
        std::env::var_os("KIANA_BIN")
            .map(std::path::PathBuf::from)
            .expect(
                "set KIANA_BIN to the `kiana` executable (built by the kiana-coding-agent stage-8 unit) \
             to run this ignored signal test",
            )
    }

    fn spawn_kiana(args: &[&str]) -> Child {
        Command::new(kiana_binary())
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn the kiana binary")
    }

    fn send(child: &Child, signal: libc::c_int) {
        // SAFETY: `kill(pid, signal)` 只向刚 spawn 的子进程发送标准信号。
        let result = unsafe { libc::kill(child.id() as libc::c_int, signal) };
        assert_eq!(result, 0, "kill({signal}) must succeed");
    }

    fn wait_for_signal_exit(args: &[&str], signal: libc::c_int, expected: i32) {
        let mut child = spawn_kiana(args);
        // 给 CLI 时间进入模式主循环并安装信号处理器。
        std::thread::sleep(Duration::from_millis(750));
        send(&child, signal);
        let status = child.wait().expect("wait for the kiana process");
        assert_eq!(
            status.code(),
            Some(expected),
            "kiana must exit with the TS signal exit code, not die from the signal"
        );
    }

    #[test]
    #[ignore = "needs the `kiana` binary from kiana-coding-agent stage 8 (set KIANA_BIN)"]
    fn rpc_sigterm_exits_with_143() {
        wait_for_signal_exit(&["--mode", "rpc"], libc::SIGTERM, 143);
    }

    #[test]
    #[ignore = "needs the `kiana` binary from kiana-coding-agent stage 8 (set KIANA_BIN)"]
    fn rpc_sighup_exits_with_129() {
        wait_for_signal_exit(&["--mode", "rpc"], libc::SIGHUP, 129);
    }

    #[test]
    #[ignore = "needs the `kiana` binary from kiana-coding-agent stage 8 (set KIANA_BIN)"]
    fn print_sigterm_exits_with_143() {
        wait_for_signal_exit(&["--print", "hi"], libc::SIGTERM, 143);
    }

    #[test]
    #[ignore = "needs the `kiana` binary from kiana-coding-agent stage 8 (set KIANA_BIN)"]
    fn print_sighup_exits_with_129() {
        wait_for_signal_exit(&["--print", "hi"], libc::SIGHUP, 129);
    }
}
