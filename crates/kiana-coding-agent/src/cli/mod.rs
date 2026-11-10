//! `cli/**`（12 个文件）：参数解析、认证子命令、启动期 UI。
//!
//! # 这个模块负责什么
//! 它覆盖命令行进程从 `argv` 到“真正跑起来”之间的一次性工作：
//!
//! - [`args`]：把 `argv` 解析成 [`args::Args`]，并检测未知选项、诊断与 `--` 终止符；
//! - [`setup`]：进程级初始化（标题、环境标记、HTTP dispatcher）以及所有子模块共用的
//!   输出/样式/退出码抽象；
//! - [`auth_command`]/[`auth_check`]/[`credential_print`]：`kiana auth ...` 子命令；
//! - [`config_selector`]/[`session_picker`]/[`startup_ui`]/[`project_trust`]：启动期 TUI
//!   对话框、`--resume` 选择器与项目信任询问；
//! - [`file_processor`]/[`initial_message`]：把 `@file` 与 stdin 拼成第一条用户消息；
//! - [`list_models`]：`--list-models` 的表格输出。
//!
//! # 共同约定
//! - 本模块不直接 `println!`/`eprintln!`：用户可见字节统一走 [`setup::Output`]，这样测试
//!   能用 `Output::capture()` 断言逐字输出，stdout 的“被接管”语义也只有一处实现。
//! - 需要 `process.exit(code)` 的路径返回 [`setup::CliExit`]（即 [`setup::CliResult`] 的
//!   `Err`），由 `kiana`/`kiana-rpc` 二进制入口决定真正退出；库调用方不会被杀进程。
//! - 需要 TUI 的模块建立在 `Rc` 之上，必须在 `LocalSet` 上调用。
//!
//! # 阅读顺序
//! [`args`]（认识 `Args`）→ [`setup`]（输出与退出约定）→ 按任务选读其余文件。

pub mod args;
pub mod auth_check;
pub mod auth_command;
pub mod config_selector;
pub mod credential_print;
pub mod file_processor;
pub mod initial_message;
pub mod list_models;
pub mod project_trust;
pub mod session_picker;
pub mod setup;
pub mod startup_ui;

// 共享基础设施：`crate::cli::{Output, Style, CliExit, CliResult}` 与
// `crate::cli::setup::*` 等价，方便各调用方选择较短路径。
pub use setup::{CliExit, CliResult, Output, Style};
