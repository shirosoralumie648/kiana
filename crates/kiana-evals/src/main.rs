//! packages/evals/src/cli.ts 的等价入口。
//!
//! 只做三件事：建 tokio 运行时、调用 [`kiana_evals::cli::run`]、把错误的 `Display` 文本打到
//! stderr 并以 1 退出（Node 打印堆栈；spec §3.5 记录 Rust 侧只打印 message）。
//! 正常路径的退出码由 `cli::run` 决定（`report.blockedPairs` 非空 → 1）。
//!
//! # 为什么是薄入口
//!
//! TS 的 `cli.ts` 既是入口又是主逻辑：模块顶层直接执行，不导出任何符号。Rust 侧需要
//! 一个具体的 `[[bin]]` 目标（`Cargo.toml` 的 `name = "kiana-evals"`），于是把「进程外壳」
//! 与「业务编排」拆开：本文件只负责运行时与退出码，其余全部在 [`kiana_evals::cli`]。
//! 好处是 `cli::run_with` 能由测试注入参数、包根与环境闭包，不启动真实二进制也能驱动
//! 整条流水线；`tests/docker.rs` 两条路都走——用 `run_with` 测产物，用
//! `CARGO_BIN_EXE_kiana-evals` 黑盒测本文件的错误路径。
//!
//! 本文件是独立的 crate 根，只能使用 `kiana_evals` 库的公开接口。业务逻辑加在这里会绕开
//! `run_with` 的测试覆盖，也会让 `cli.rs` 里逐字对齐 TS 的文本契约多出一份实现。
//!
//! # 启动顺序与线程模型
//!
//! 1. `main` 先构造 `Builder::new_multi_thread()` 运行时；`enable_all()` 启用 I/O 与
//!    时间驱动，worker 线程数未显式设置（取 tokio 默认值）。
//! 2. `Builder::build()` 可能失败：它返回 `io::Error`（运行时所需的 OS 资源无法初始化）。
//!    代码用 `match` 接住并打印 `Display`、以 1 退出，没有 `unwrap`，因此这条路径不 panic。
//! 3. `Runtime::block_on` 在 main 线程上驱动 [`kiana_evals::cli::run`] 直到结束。
//! 4. `run` 返回 `Ok(code)` 时 `code` 就是进程退出码；返回 `Err` 时打印其 `Display` 并
//!    折算成 1。两条路径最后都走 `std::process::exit`。
//!
//! 流水线是串行的：`cli::run` 内的 await 顺序执行，`src/` 中没有 `tokio::spawn`；`cli::run`
//! 路径上的异步只有 `tokio::fs` 的文件 I/O。业务步骤的先后由 await 顺序决定，与 worker
//! 线程数无关。
//!
//! # 退出码
//!
//! | 退出码 | 触发条件 |
//! |---|---|
//! | 0 | `cli::run` 返回 `Ok(0)`：报告已写出，且不存在 blocked pair |
//! | 1 | `cli::run` 返回 `Ok(1)`：报告中存在 blocked pair |
//! | 1 | `cli::run` 返回 `Err`：参数、文件 I/O、docker 启动/用例发现或报告读取等基础设施错误 |
//! | 1 | tokio 运行时构造失败 |
//!
//! blocked pair 与运行失败共用退出码 1，调用方无法只凭退出码区分二者。`Err` 打印的是
//! [`kiana_evals::cli::CliError`] 的 `Display`，即 TS 的 `error.message`；
//! `tests/docker.rs#binary_reports_errors_on_stderr_with_exit_code_one` 逐字断言了其中
//! 一条（stdout 为空、stderr 恰为消息加换行、退出码 1），不要在这里包装、加前缀或改变
//! 换行。
//!
//! # 注意事项
//!
//! - 成功路径的进度行、报告文本与 `Artifacts: <dir>` 全部由 `cli::run` 打印到 stdout；
//!   本文件只在出错时写 stderr，自身不产出 stdout。
//! - 参数由 [`kiana_evals::cli::run`] 自己从 `std::env::args_os().skip(1)` 读取（等价
//!   `process.argv.slice(2)`），本文件没有 argv 转发代码。
//! - 本文件没有安装 panic hook：`cli::run` 内的 panic 不会走上面的「stderr + 退出码 1」
//!   映射，而是按 Rust 默认 panic 行为终止。
//!
//! # 阅读顺序
//!
//! 本文件（进程外壳）→ [`kiana_evals::cli`]（八个阶段、产物与失败模式）→
//! `kiana_evals::plan` / `kiana_evals::docker` / `kiana_evals::report`。

fn main() {
    // 手动构造运行时：`build()` 的失败被当作普通错误处理（打印后退出 1），而不是 panic；
    // `enable_all()` 启用 I/O 与时间驱动。
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    };
    // `run()` 的 `Ok` 值就是预期退出码；`Err` 在这里统一折算为 1。
    let exit_code = match runtime.block_on(kiana_evals::cli::run()) {
        Ok(exit_code) => exit_code,
        Err(error) => {
            eprintln!("{error}");
            1
        }
    };
    // 唯一出口：显式设置进程退出码；`main` 本身返回 `()`，不会隐式决定状态码。
    std::process::exit(exit_code);
}
