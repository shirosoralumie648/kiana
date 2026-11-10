//! `kiana-ai` 可执行文件入口（`cli.ts` 的 node 脚本外壳）。
//!
//! TS 侧 `cli.ts` 是 `package.json` 的 bin 目标，模块加载即执行 `main()`。Rust 没有
//! 模块加载钩子，因此由这个 `[[bin]]` 目标（`Cargo.toml` 已声明 `name = "kiana-ai"`）
//! 直接调用 [`kiana_ai::cli::main`]。参数解析、输出文本与退出码都在 `cli.rs`，这里不添加
//! 任何额外行为。
//!
//! # 设计原因：为什么这里只有一行转发
//!
//! `[[bin]]` 在 Rust 里是一个独立的 crate 根，只能使用 `kiana_ai` 库 crate 的公开接口，
//! 访问不到 `cli.rs` 的私有项。把 CLI 逻辑写进 bin 会有两个代价：
//!
//! - `tests/cli_login.rs` 以子进程方式运行这个二进制（`CARGO_BIN_EXE_kiana-ai`）；实现若
//!   只存在于 bin 中，库 crate `kiana_ai` 的测试与下游都无法直接复用。
//! - `cli.ts` 的逐字输出与退出码契约集中在 `cli.rs` 及其模块文档里，两个入口各写一份会
//!   逐渐产生分歧。
//!
//! 因此本文件保持「薄入口」：`ARCHITECTURE.md` 也明确要求 bin 只调用 `cli.rs`，不要把
//! `cli.ts` 的逻辑搬进 `main.rs`。
//!
//! # 执行流程与运行时
//!
//! 1. 进程启动后直接进入本文件的 `main`；它没有 `#[tokio::main]`，本身是同步函数。
//! 2. [`kiana_ai::cli::main`] 内部用 `std::env::args().skip(1)` 取用户参数（等价
//!    `process.argv.slice(2)`），显式构建多线程 tokio 运行时，再 `block_on` 执行 CLI
//!    主逻辑。
//! 3. 正常结束（包括 stdin 在提示处 EOF，见 `kiana_ai::cli::CliOutcome::Eof`）时本函数
//!    返回，进程退出码为 0。
//! 4. 被处理的错误（含运行时构建失败）由 `cli::main` 把 `Error: <message>` 写到 stderr
//!    并调用 `std::process::exit(1)`，不会返回到这里。
//!
//! # 参数与退出码
//!
//! 本文件的 `main` 自身不解析参数、不做 IO。命令语法（`help`/`list`/`login [provider]`）、
//! 输出文本与退出码语义见 [`kiana_ai::cli::run`] 及模块 `kiana_ai::cli` 的文档；可观察
//! 契约由 `tests/cli_login.rs` 以子进程黑盒测试锁定。
//!
//! # 注意事项
//!
//! - 不要在 `main` 里添加初始化、日志、panic hook 或额外线程：运行时的创建者是
//!   `cli::main`，在这里另起一套会绕开 `cli.rs` 的逐字文本/退出码契约，使子进程测试与
//!   预期输出对不上。
//! - 二进制名 `kiana-ai` 是 `Cargo.toml` 的 `[[bin]].name` 与集成测试里
//!   `CARGO_BIN_EXE_kiana-ai` 共同依赖的契约；改名要同步这两处（`cli.rs` 用法文本中的
//!   npm 包名是另一个独立字面量）。
//! - 未被捕获的 panic 不走 `Error: <message>` + 退出码 1 的路径，而是按 Rust 默认的 panic
//!   行为终止；本文件与 `cli::main` 都未安装 panic hook。

fn main() {
    // 阻塞到 CLI 结束；正常返回即退出码 0。错误已由 `cli::main` 打印并以
    // `std::process::exit(1)` 终止，所以这里没有返回值可检查。
    kiana_ai::cli::main();
}
