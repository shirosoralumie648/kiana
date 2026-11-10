//! `packages/coding-agent/src/cli.ts`（bin `kiana` 的入口）。
//!
//! 逐条对齐 TS：
//! 1. `setupCli()`（进程标题、`KIANA_CODING_AGENT`/`AI_AGENT` 标记、HTTP dispatcher）；
//! 2. `main(process.argv.slice(2))`。
//!
//! 与 TS 的差异只在宿主形态：Rust 需要显式创建 tokio 运行时，并在
//! [`tokio::task::LocalSet`] 上运行整个应用（`AgentSession`/扩展运行时是 `!Send`；
//! `rquickjs` 线程亲和）。`ExtensionHost` 必须在 `main` 之前装配并一直存活，否则任何
//! 扩展加载都会报 `Extension runtime engine is not installed`。
//!
//! # 这个文件负责的边界
//!
//! `Cargo.toml` 的 `[[bin]] name = "kiana"` 指向本文件；RPC 变体是
//! `src/bin/kiana-rpc.rs`。本文件只做三件事：进程级初始化、把操作系统给的可执行文件名
//! 之后的参数转成 `Vec<String>`、把库返回的退出码交给操作系统。参数解析、子命令分发、
//! 会话创建全部在 [`kiana_coding_agent::main`]（`app.rs` 的 `run_main`）里。
//!
//! # 启动流程（自上而下）
//!
//! 1. 安装主题 JSON 校验器（函数体内注释说明为什么必须在创建运行时对象之前）；
//! 2. [`kiana_coding_agent::setup_cli`]：进程标题、`KIANA_CODING_AGENT`/`AI_AGENT`
//!    环境标记、启动期致命错误的 panic 钩子、HTTP dispatcher；
//! 3. 建立单线程 tokio 运行时与 [`tokio::task::LocalSet`]（原因见下节）；
//! 4. 收集参数：跳过 `argv[0]`，其余按 `to_string_lossy` 解码；
//! 5. 在 `LocalSet` 内依次执行：创建 `ExtensionHost` → `await`
//!    [`kiana_coding_agent::main`] → `await` 原始 stdout 队列排空；
//! 6. 用 `main` 返回的退出码调用 `std::process::exit`。
//!
//! # 为什么是单线程 + LocalSet
//!
//! `LocalSet::block_on` 在**调用它的线程**上轮询根 future，`LocalSet` 上的
//! `spawn_local` 任务也不会迁移到别的线程。扩展运行时由 rquickjs 支撑，其
//! `Context`/`Persistent` 只能在创建它的线程上使用；`AgentSession` 等会话对象同样
//! 是 `!Send`（见文件开头的差异说明）。因此应用不能把这些值交给要求 `Send` 的
//! `tokio::spawn`，本文件选择 current-thread 运行时，把整个 `main` 调用链固定在这一
//! 条线程上。
//!
//! # 退出码与失败模式
//!
//! - 正常情况下退出码来自 [`kiana_coding_agent::main`]：库层用 `CliExit { code }`
//!   表达 TS 的 `process.exit(code)`，`main` 把它折算成 `i32`；
//! - 退出前必须 `await` [`Output::flush`]：print/RPC 的协议字节由
//!   `kiana_core::output_guard` 的后台写线程排队，`std::process::exit` 不会等待它，
//!   提前退出会丢字节。该函数没有返回值，写线程已消失时它会调用
//!   `fatal_stdout_error` 直接以退出码 1 结束进程，覆盖本文件原本要返回的退出码；
//! - tokio 运行时构建失败会 panic（`expect`），默认 panic 以退出码 101 结束，
//!   而不是 Node 的 1（`cli/setup.rs` 的 `FatalStartupError` 文档记录了同一差异）；
//! - `ExtensionHost::new()` 的错误被 `.ok()` 丢弃，引擎装配失败**不会**中止启动：
//!   之后 `load_extensions*` 拿不到线程本地引擎，会为每个路径返回一条
//!   `Extension runtime engine is not installed` 的加载错误，而不是启动失败。
//!
//! # 阅读顺序
//!
//! `cli/setup.rs`（`setup_cli` 与 `Output`）→ 本文件的启动骨架 → `app.rs` 的
//! `main`/`run_main`（真正的启动编排）。

use kiana_coding_agent::Output;

fn main() {
    // TS 的 theme.ts 在模块加载后由 main.ts 立即安装 JSON 校验器；Rust 没有模块加载期
    // 钩子，二进制入口在创建任何运行时对象之前安装（与 TS 的可观察时序一致）。
    // 没有校验器时 `parseThemeJson` 会退化成“只要求一个 `colors` 对象”的快速检查
    // （`kiana_theme::clear_theme_json_validator` 的文档），所以必须早于任何主题加载。
    // `run_main` 开头还会再安装同一个函数（`app.rs` 第 1 步）；setter 是“最后安装者
    // 生效”，值相同，重复安装没有可观察差异。
    kiana_theme::set_theme_json_validator(Box::new(kiana_theme::validate_theme_json));

    // 必须早于运行时的创建：`setup_cli` 内部用 unsafe 写进程环境变量，并安装启动期
    // 致命错误的 panic 钩子（见 `cli/setup.rs` 的 `setup_cli` 与 SAFETY 注释）。
    kiana_coding_agent::setup_cli();

    // 单线程运行时：根 future 由下面的 `LocalSet` 在当前线程轮询，`!Send` 的会话与
    // JS 引擎对象不会跨线程（原因见文件级 `//!` 的“为什么是单线程 + LocalSet”）。
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to build tokio runtime");
    let local = tokio::task::LocalSet::new();

    // 对应 `process.argv.slice(2)`：跳过 `argv[0]`（可执行文件路径）。Node 对 argv 做
    // UTF-8 有损解码，`to_string_lossy` 复刻同一行为（`cli/setup.rs` 的 `argv_strings`）；
    // 非法字节会被替换为 U+FFFD，而不是让进程启动失败。
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect();

    let code = local.block_on(&runtime, async move {
        // 扩展引擎是线程本地的：必须在本线程、且早于任何资源加载创建。
        //
        // 这里用**命名**绑定（下划线开头）而不是 `let _ = ...`：命名绑定活到 async
        // 块结束，宿主（及其 `Generation`）一直持有引擎；`let _` 会在语句结束时立即
        // 析构，线程本地登记随之失效，之后每次扩展加载只能拿到
        // “Extension runtime engine is not installed”。
        //
        // `new()` 的错误被 `.ok()` 丢弃：装配失败时进程照常启动，错误留给扩展加载阶段
        // 逐路径上报（`kiana-extensions` 的 loader 在取不到当前引擎时的分支），不会让
        // CLI 在启动期直接失败。
        let _extension_host = kiana_extensions::ExtensionHost::new().ok();
        // `None`：二进制入口不注入 `MainOptions.extension_factories`；内置扩展由
        // `run_main` 自己追加（`app.rs` 第 2 步）。
        let code = kiana_coding_agent::main(args, None).await;
        // print/RPC 的协议字节由 output_guard 的后台写线程排队，`process::exit` 不会
        // 等它；必须在退出前 await 排空，否则队列里的字节丢失。`Output::flush` 没有
        // 错误返回值：写线程已消失时它直接 `exit(1)`（kiana_core::output_guard 的
        // `fatal_stdout_error`），因此这条路径会盖掉原本的退出码。
        Output::process().flush().await;
        code
    });

    // `std::process::exit` 不运行任何析构函数。此处 async 块已经结束，`_extension_host`
    // 的析构（触发 `Generation` 的拆除、注销 UI 槽登记，见 kiana-extensions 的
    // `GenerationInner::Drop`）与 stdout 排空都已完成，剩下只有退出本身。
    std::process::exit(code);
}
