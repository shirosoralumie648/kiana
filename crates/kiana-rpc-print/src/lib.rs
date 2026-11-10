//! 对应 ca-rpc-print：print/json 单次运行、RPC 服务端与客户端、JSONL。
//!
//! TS 源：packages/coding-agent/src/modes/{index,json-event,print-mode}.ts + modes/rpc/*
//!
//! 本文件是 crate 根：声明 §8.1 的 7 个模块，定义 [`RawStdout`]（由 `kiana_core::output_guard`
//! 提供实现，测试可注入替身），并再导出六个叶子模块的名字。
//!
//! `index` 是后续单元实现的 `modes/index.ts` 再导出面；它的名字与叶子模块重名，
//! 因此**不**做 `pub use index::*`。
//!
//! 写出纪律（`ARCHITECTURE.md` §4.3、spec §4.4.2）：任何写到 stdout 的 JSON 记录都必须经
//! [`serialize_wire_line`] / [`serialize_event_line`] / [`serialize_json_line`]，不得直接
//! `serde_json::to_string`。这三个函数在 [`json_event`] / [`jsonl`] 里实现。
//!
//! # 为什么需要这个 crate
//!
//! 字节级保真是整个移植项目的验收标准：print/JSON 模式与 RPC 模式在 stdout 上写出的每一条
//! JSON 记录（键序、`null` 与缺键的区分、转义与数字格式）都必须逐字节保持当前稳定契约，改动需同步测试，否则解析
//! 另一端的客户端与对拍测试全部失配。这个 crate 把四个 TS 模式文件连同它们的 wire 投影、
//! JSONL 框架与信号接缝聚在一处，并用 [`RawStdout`] 把“协议写通道”从进程级 stdout 里抽出来，
//! 使测试能注入记录型替身而不触碰真实 stdout（spec §2.8 的测试纪律）。
//!
//! # 在系统中的位置
//!
//! 上游：`kiana-coding-agent`——`app.rs` 在参数分发处调用 [`crate::print_mode::run_print_mode`]
//! / [`crate::rpc_mode::run_rpc_mode`] 并传入 [`global_raw_stdout`]；`sdk_exports.rs` 把两个
//! 入口与主要类型再导出为 SDK 面；`bin/kiana-rpc.rs` 在单线程 tokio 运行时 + `LocalSet` 上
//! 驱动 RPC 模式。下游（被调用方）：`kiana_core::output_guard`（[`GlobalRawStdout`] 委托的
//! 真实写通道）、`kiana-agent-core` 的 wire 键序、`kiana-session` 的会话事件。
//!
//! # 一次典型调用
//!
//! 1. print/JSON：`app.rs` -> [`crate::print_mode::run_print_mode`]（注册信号处理器 ->
//!    写 JSON 会话头 -> rebind 会话 -> 逐条 prompt）；text 模式只写最后一条 assistant 消息的
//!    文本块，JSON 模式把每条 `AgentSessionEvent` 经 [`serialize_event_line`] 投影成 wire 行
//!    再 [`RawStdout::write_raw`]。
//! 2. RPC：`bin/kiana-rpc.rs` -> [`crate::rpc_mode::run_rpc_mode`]（先
//!    [`RawStdout::take_over`] 接管 stdout，普通输出改道 stderr，协议记录独占真实 stdout）
//!    -> `RpcServer` 的 stdin/stdout 协议循环。
//! 3. 正常退出路径在退出前 await `flush_raw_stdout` 冲干净写队列；信号退出路径部分跳过
//!    flush（print 的 SIGTERM/SIGHUP、RPC 的 SIGTERM 都跳过，RPC 的 SIGHUP 不跳过——
//!    对齐 TS 契约，见 `kiana_core::output_guard` 的退出纪律说明）。
//!
//! # 边界与失败模式
//!
//! - 退出码由入口函数返回：print 的 `error`/`aborted` 只写 stderr 并返回 1；RPC 的
//!   EOF/扩展关停为 0，SIGTERM 143、SIGHUP 129（见 [`crate::rpc_mode::run_rpc_mode`] 文档）。
//! - wire 投影错误（[`WireError`]）对应 TS 的 `throw`，文本逐字对齐 `Error.message`。
//! - 非 Unix 平台没有 `sigaction`，print 与 RPC 模式都不注册信号处理器（见 `signal`
//!   模块文档）。
//!
//! # 阅读顺序
//!
//! [`rpc_types`]（wire 形状）-> [`json_event`] / [`jsonl`]（投影与 JSONL 框架）->
//! [`print_mode`] / [`rpc_mode`]（两个执行入口）-> [`rpc_client`]（spawn 子进程服务端的
//! 客户端）-> `signal`（`process.on`/`process.off` 语义的 Unix 复刻）。

pub mod daemon;
pub mod event_feed;
pub mod index;
pub mod json_event;
pub mod jsonl;
pub mod print_mode;
pub mod rpc_client;
pub mod rpc_mode;
pub mod rpc_types;

/// 进程信号接缝（`process.on`/`process.off` 的 `sigaction` + self-pipe 实现）。
///
/// print 与 RPC 模式共用；非 Unix 平台没有 `sigaction`，两种模式都不注册处理器。
/// 模块文档解释了为什么不能直接用 `tokio::signal::unix`（tokio 注册的 libc 处理器永不注销，
/// 复刻不了 `process.off` 的可观察语义）。
#[cfg(unix)]
mod signal;

use std::sync::Arc;

use kiana_core::output_guard;
use kiana_ext_api::BoxFuture;

/// 协议 stdout 的“原始写通道”（TS: `core/output-guard.ts` 的 `writeRawStdout` 等四个函数）。
///
/// print / RPC 模式的**每一条** stdout 输出都经这个 trait；直接 `println!` / `std::io::stdout()`
/// 的写法不会被重定向（TS 的接管靠运行时替换 `process.stdout.write`，Rust 无此钩子，所以
/// 写通道必须是显式抽象）。
///
/// 本单元只依赖这个 trait：生产实现是 [`GlobalRawStdout`]（委托给
/// [`kiana_core::output_guard`]），测试可以注入记录写入的替身而**不触**进程级 stdout 状态。
///
/// 语义逐条对齐 TS：
/// - [`write_raw`](Self::write_raw)：“发后不管”，空串丢弃，调用方不等待写出；
/// - [`wait_for_backpressure`](Self::wait_for_backpressure)：自旋到写链尾稳定；
/// - [`flush`](Self::flush)：先等背压再驱动一次底层写回调；
/// - [`take_over`](Self::take_over)：幂等接管 stdout（把普通输出重定向到 stderr）。
///
/// # 调用方须知
///
/// - [`write_raw`](Self::write_raw) 返回只表示字节已入队（由后台写线程异步写出），不代表
///   对端已收到；进程退出前必须 [`flush`](Self::flush)，否则队列里未写出的字节可能丢失
///   （TS 依赖 Node 退出时冲刷 stdout，Rust 没有等价的进程级钩子）。
/// - 若其它线程持续写入，[`wait_for_backpressure`](Self::wait_for_backpressure) 一直自旋，
///   没有超时（与 TS 的链尾比较语义一致）。
/// - 写链上的不可重试 I/O 错误会直接结束进程（TS `process.exit(1)`），不通过 `Result` 上抛。
/// - [`take_over`](Self::take_over) 只影响“普通输出”的落点（改写 stderr）与 TUI 写钩子；
///   经 [`write_raw`](Self::write_raw) 的协议字节始终写真实 stdout（fd 1）。
pub trait RawStdout: Send + Sync {
    /// 写一段协议字节；空串直接丢弃，不进入写链。
    fn write_raw(&self, text: &str);
    /// 等到此前排队的写入都完成（TS: `waitForRawStdoutBackpressure`）。
    fn wait_for_backpressure(&self) -> BoxFuture<'_, ()>;
    /// 冲刷写链（TS: `flushRawStdout`）。
    fn flush(&self) -> BoxFuture<'_, ()>;
    /// 接管 stdout；幂等。
    fn take_over(&self);
}

/// [`RawStdout`] 的默认实现：直接委托给 [`kiana_core::output_guard`] 的进程级通道。
///
/// 后台写线程拥有真实 stdout，所有写入经 `mpsc` 队列串行执行——这就是“字节顺序与调用顺序
/// 一致”的保证来源。这个类型自身没有任何状态（单元结构体），所有共享状态都在
/// `kiana_core::output_guard` 的进程级静态里。
#[derive(Debug, Default, Clone, Copy)]
pub struct GlobalRawStdout;

impl RawStdout for GlobalRawStdout {
    fn write_raw(&self, text: &str) {
        output_guard::write_raw_stdout(text);
    }

    fn wait_for_backpressure(&self) -> BoxFuture<'_, ()> {
        Box::pin(output_guard::wait_for_raw_stdout_backpressure())
    }

    fn flush(&self) -> BoxFuture<'_, ()> {
        Box::pin(output_guard::flush_raw_stdout())
    }

    fn take_over(&self) {
        output_guard::take_over_stdout();
    }
}

/// 进程级 [`RawStdout`]（TS: 直接调用 `output-guard.ts` 的模块级函数）。
///
/// TS 的 `runPrintMode` / `runRpcMode` 直接调用 `output-guard.ts` 的模块级单例函数，
/// 没有通道参数；Rust 把通道显式化为 `Arc<dyn RawStdout>` 参数，生产调用方一律传这个函数
/// 的结果，测试传替身。每次调用都返回新的 `Arc`，但通道本身是进程级单例，多个 `Arc`
/// 共享同一条写链。
pub fn global_raw_stdout() -> Arc<dyn RawStdout> {
    Arc::new(GlobalRawStdout)
}

// 六个叶子模块的再导出面（TS: `modes/index.ts` 的 18 个去重名字由后续单元在 `index` 里组装；
// 这里只暴露各文件自己的符号）。`index` 不参与 glob 再导出，避免同名冲突。
pub use event_feed::*;
pub use json_event::*;
pub use jsonl::*;
// 这三个模块由本 crate 的后续单元实现；空桩阶段 glob 再导出会触发 unused_imports，
// 这里显式豁免（模块填充后豁免自然失效但无害）。
#[allow(unused_imports)]
pub use print_mode::*;
#[allow(unused_imports)]
pub use rpc_client::*;
#[allow(unused_imports)]
pub use rpc_mode::*;
pub use rpc_types::*;

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    /// 记录调用的 [`RawStdout`] 替身；不触碰进程级 stdout（spec §2.8 的测试纪律）。
    #[derive(Default)]
    struct RecordingStdout {
        writes: Mutex<Vec<String>>,
        flushes: Mutex<u32>,
        take_overs: Mutex<u32>,
    }

    impl RawStdout for RecordingStdout {
        fn write_raw(&self, text: &str) {
            self.writes.lock().expect("lock").push(text.to_string());
        }

        fn wait_for_backpressure(&self) -> BoxFuture<'_, ()> {
            Box::pin(async {})
        }

        fn flush(&self) -> BoxFuture<'_, ()> {
            *self.flushes.lock().expect("lock") += 1;
            Box::pin(async {})
        }

        fn take_over(&self) {
            *self.take_overs.lock().expect("lock") += 1;
        }
    }

    #[tokio::test]
    async fn raw_stdout_trait_is_object_safe_and_records_writes() {
        let stdout = RecordingStdout::default();
        stdout.write_raw("{\"a\":1}\n");
        stdout.wait_for_backpressure().await;
        stdout.flush().await;
        stdout.take_over();

        assert_eq!(*stdout.writes.lock().expect("lock"), vec!["{\"a\":1}\n"]);
        assert_eq!(*stdout.flushes.lock().expect("lock"), 1);
        assert_eq!(*stdout.take_overs.lock().expect("lock"), 1);

        let dynamic: std::sync::Arc<dyn RawStdout> =
            std::sync::Arc::new(RecordingStdout::default());
        dynamic.write_raw("x");
    }

    #[test]
    fn global_raw_stdout_is_constructible_without_writing() {
        // 只验证返回类型与对象安全性；不调用任何会触碰进程级 stdout 的方法。
        let stdout: std::sync::Arc<dyn RawStdout> = global_raw_stdout();
        let _: &dyn RawStdout = stdout.as_ref();
    }
}
