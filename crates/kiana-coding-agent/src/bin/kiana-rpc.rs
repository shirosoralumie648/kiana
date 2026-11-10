//! `packages/coding-agent/src/rpc-entry.ts`（bin `kiana-rpc` 的入口）。
//!
//! 与 `cli.ts` 的区别（逐条对齐 TS）：
//! - 进程标题是 `kiana-rpc`（**不**调用 `setupCli()`，否则会被改成 `kiana`）；
//! - 先设置 `KIANA_CODING_AGENT=true`、`AI_AGENT=kiana`、抑制警告（Rust 无全局警告钩子，no-op）；
//! - 先 `configureHttpDispatcher()`，再 `main(["--mode","rpc", ...argv])`。
//!
//! # 为什么单独一个二进制
//!
//! `Cargo.toml` 的 `[[bin]] name = "kiana-rpc"` 指向本文件。外部程序（编辑器插件、
//! 脚本、集成测试）用 RPC 行协议驱动代理时，要求代理以 `rpc` 模式启动：该模式会接管
//! stdout，普通输出改走 stderr，真实 stdout 留给协议字节（`app.rs` 步骤 14）。
//! `kiana --mode rpc` 也能做到，但调用方必须自己记住这个前缀；漏掉时 `resolve_app_mode`
//! 会按 `--print` 与 stdin/stdout 是否 TTY 落到 `Print` 或 `Interactive`。这个二进制只把
//! `["--mode","rpc"]` 拼到用户 argv 前面，再调用 [`kiana_coding_agent::main`]。
//!
//! 注意 `--mode` 在 `parse_args` 里是“后出现者覆盖先出现者”（`cli/args.rs` 每遇到一次
//! `--mode` 就重新赋值），所以用户在 argv 里再传 `--mode text|json` 会覆盖这里强制的
//! `rpc`。本文件不做校验；需要 RPC 模式的调用方不应再传 `--mode`。
//!
//! # 启动顺序与设计原因
//!
//! 1. `set_process_title("kiana-rpc")`：Linux 下写 `/proc/self/comm`，其它平台 no-op；
//! 2. 写 `KIANA_CODING_AGENT=true` 与 `AI_AGENT=kiana`（`unsafe { set_var }`，必须在任何
//!    线程创建之前完成，理由见函数内的 SAFETY 注释）；
//! 3. 安装启动期致命错误 panic 钩子：迁移里的 Node 风格 fs 异常以 `FatalStartupError`
//!    panic 抛出，钩子把它打印成 `Error: <message>` 并以 1 退出，而不是 Rust 默认的 101；
//! 4. 按默认 300s idle 超时配置进程级 HTTP client；
//! 5. 建 current-thread tokio 运行时与 `LocalSet`，在其中创建 `ExtensionHost`、调用
//!    `main`、排空协议输出，最后 `std::process::exit(code)`。
//!
//! 第 1–4 步与 `setup_cli()`（`cli/setup.rs`）做的事相同，唯一区别是进程标题：
//! `setup_cli` 会把标题改成 `kiana`，所以这里不能调用它，只能手动重做这些步骤。
//! 修改其中任何一步时应同时检查 `setup_cli`，否则两个入口的行为会漂移。
//!
//! # 运行时与线程模型
//!
//! 扩展运行时基于 rquickjs（引擎对象有线程亲和性），`AgentSession` 等会话状态也是
//! `!Send`，所以整个应用固定在一个线程上：`new_current_thread` 运行时 +
//! [`tokio::task::LocalSet`]。
//! `ExtensionHost::new()` 会把引擎登记到当前线程的线程本地存储，必须早于任何扩展/资源
//! 加载创建，并且一直存活到 `main` 返回；没有存活宿主时，扩展加载会为每个扩展记录
//! `Extension runtime engine is not installed`（`kiana-extensions/src/loader/mod.rs`）。
//! 这里用 `.ok()` 丢弃构造错误：进程继续启动，但扩展会以该错误被跳过；`app.rs` 在运行期
//! 诊断含 `Failed to load extension` 时额外打印
//! `Hint: Start without extensions using "kiana -ne".` 并返回 1。
//!
//! # 失败模式与退出码
//!
//! - `main` 返回的 `i32` 原样交给 `std::process::exit`。`Output::flush().await` 必须在
//!   exit 之前执行：它排空 `output_guard` 里尚未写出的原始 stdout（RPC 协议字节），而
//!   `process::exit` 不会运行栈上的 Rust 析构函数，队列里的字节会直接丢失。普通 stdout
//!   输出是同步写入的，不依赖这次 flush。若写线程已经消失或写入遇到不可重试的 I/O 错误，
//!   `flush` 内部会调用 `fatal_stdout_error` 直接以退出码 1 结束进程
//!   （`kiana-core/src/output_guard.rs`），不会执行到下面的 exit。
//! - 运行时构造失败走 `expect` panic。钩子只拦截 `FatalStartupError`，所以这种 panic 仍由
//!   默认钩子处理（打印 panic 信息、退出码 101），没有对应的 TS 行为。
//! - `configure_http_dispatcher` 的错误被 `let _` 忽略（例如环境变量里的代理 URL 非法）。
//!   之后第一次使用网络时 `shared_http_client()` 会惰性构造默认 client，构造失败再退回
//!   `reqwest::Client::new()`（`kiana-core/src/http_dispatcher.rs`），因此这里失败不会
//!   中止启动。
//!
//! # 注意事项
//!
//! - `std::env::args_os().skip(1)` 跳过程序名；非 UTF-8 参数经 `to_string_lossy` 有损转换。
//! - 未用 `--mode` 覆盖时，`kiana-rpc @x` 会命中 `app.rs` 的 RPC 校验，以 1 退出并打印
//!   `Error: @file arguments are not supported in RPC mode`。
//! - `tests/package_distribution.rs` 锁定 `kiana-rpc --version` 输出 `1.4.0\n`、退出码 0；
//!   `tests/cli_smoke.rs` 锁定同一命令 stderr 为空，以及迁移错误在 `kiana-rpc` 上输出
//!   `Error: ...` + 1。
//!
//! # 阅读顺序
//!
//! 本文件（启动骨架）→ `cli/setup.rs`（`setup_cli` 与 `Output`）→ `app.rs` 的
//! `run_main`（参数解析后的全部编排）→ `kiana-rpc-print` 的 `run_rpc_mode`（协议循环）。

use kiana_coding_agent::Output;
use kiana_core::config::APP_NAME;
use kiana_core::http_dispatcher::{DEFAULT_HTTP_IDLE_TIMEOUT_MS, configure_http_dispatcher};

fn main() {
    set_process_title(&format!("{APP_NAME}-rpc"));

    // SAFETY: 二进制入口最开头、任何线程创建之前，因此不存在数据竞争。
    unsafe {
        std::env::set_var("KIANA_CODING_AGENT", "true");
        std::env::set_var("AI_AGENT", "kiana");
    }
    // 不调用 `setup_cli()`（它会把进程标题改成 `kiana`），但启动期致命错误（迁移阶段的
    // Node fs 异常）仍必须映射为 Node 的行为：`Error: <message>` + 退出码 1。
    // TS 里这些异常逃出 `main()`，rpc-entry 没有 catch，Node 打印同样的未捕获错误。
    // 钩子内部用 `Once` 保证只安装一次，且只拦截 `FatalStartupError`；其它 panic 继续
    // 交给安装前的那个默认钩子（退出码 101）。
    kiana_coding_agent::cli::setup::install_fatal_startup_panic_hook();
    // TS: `process.emitWarning = (() => {})`；Rust 没有进程级警告钩子，保持 no-op。
    //
    // 与 `setup_cli()` 相同：在 provider SDK 发请求前配置 dispatcher；`app.rs` 步骤 7 在
    // 应用全局设置里的 `httpProxy` 之后会再调用一次 configure。
    // 这里的错误被忽略：`shared_http_client()` 之后会惰性构造默认 client 兜底。
    let _ = configure_http_dispatcher(DEFAULT_HTTP_IDLE_TIMEOUT_MS);

    // 单线程运行时 + `LocalSet`：扩展运行时与会话状态都是 `!Send`，整条 `main` 调用链
    // 不能交给多线程运行时。`enable_all` 打开 IO/time 驱动，供 reqwest 与 `tokio::time`
    // 使用。运行时构造失败不可恢复，直接 panic。
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to build tokio runtime");
    let local = tokio::task::LocalSet::new();

    // TS: `main(["--mode","rpc", ...process.argv.slice(2)])`。`args_os` 的第一项是程序名，
    // 因此 `skip(1)`；非 UTF-8 参数被 `to_string_lossy` 有损转换为字符串。
    let mut args: Vec<String> = Vec::with_capacity(2);
    args.push("--mode".to_string());
    args.push("rpc".to_string());
    args.extend(
        std::env::args_os()
            .skip(1)
            .map(|argument| argument.to_string_lossy().into_owned()),
    );

    let code = local.block_on(&runtime, async move {
        // 扩展引擎在构造时登记到当前线程；必须早于 `main` 创建，并活到 `main` 结束。
        // `.ok()` 表示构造失败不阻止启动：扩展随后会逐个以 “Extension runtime engine
        // is not installed” 报错（见文件级文档）。
        let _extension_host = kiana_extensions::ExtensionHost::new().ok();
        // `None` = 不注入调用方扩展工厂；`main` 只装配内建扩展（`app.rs` 步骤 2）。
        let code = kiana_coding_agent::main(args, None).await;
        // 必须在 exit 之前排空协议输出队列：`process::exit` 不运行栈上的析构函数。
        Output::process().flush().await;
        code
    });

    // 所有用户可见字节已经写出；这是有意的退出点（与 `src/main.rs` 相同）。
    std::process::exit(code);
}

/// `process.title = title` 的 Linux 近似（`/proc/self/comm`，失败静默）。
///
/// 内核把 comm 截断到 15 字节；`kiana-rpc` 只有 9 字节，不会被截断。写入失败（例如
/// `/proc` 不可写）只影响 `ps` 里显示的名字，不影响启动——错误被丢弃。
#[cfg(target_os = "linux")]
fn set_process_title(title: &str) {
    let _ = std::fs::write("/proc/self/comm", title);
}

/// 非 Linux 平台没有可移植的进程标题设置；保持 no-op。
#[cfg(not(target_os = "linux"))]
fn set_process_title(_title: &str) {}
