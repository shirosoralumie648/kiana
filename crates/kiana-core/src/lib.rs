//! coding-agent 基础层：config 路径/常量、utils/*、启动期基础设施。
//!
//! TS 源：packages/coding-agent/src/config.ts, utils/**, core/{event-bus,exec,timings,diagnostics,source-info,kiana-manifest,output-guard,http-dispatcher,experimental,telemetry,defaults,radius,slash-commands,mcp-servers}.ts
//!
//! 本文件只声明模块树；实现阶段按模块注释里的 TS 路径逐文件移植。
//!
//! # 这个 crate 解决什么问题
//!
//! 它把 `packages/coding-agent` 里**与具体业务无关、但多个子系统都要用**的底层能力集中实现：
//! 配置常量与路径解析、进程/终端交互、文本处理、HTTP 传输、文件与图片处理、版本与更新辅助。
//! `ARCHITECTURE.md` §1 的映射表统计本 crate 对应 47 个 TS 文件、约 5,360 行；Rust 侧按 §5 的
//! 合并规则收拢成 32 个模块（例如 `changelog` 一个模块就合并了 6 个 TS 工具文件），所以模块名
//! 与 TS 文件名并不总是一一对应，每个源文件的 `//!` 开头都会列出它负责的 TS 源。
//!
//! 另一个职责是承载**上移的契约类型**：`source_info`、`diagnostics`、`timings`、`kiana_manifest`、
//! `slash_commands` 定义在这里，而不是最初使用它们的 crate。依据是 `ARCHITECTURE.md` §3.5：
//! 同一符号在整个 workspace 只能有一个定义，且必须放在依赖链上最低、所有读者都能访问的 crate。
//! 本 crate 处于 L3，L4–L13 的下游都能直接引用；定义若留在高层，低层读者就只能复制定义。
//!
//! # 在系统中的位置
//!
//! - 上游（本 crate 依赖）：`kiana-ai`、`kiana-agent-core`、`kiana-tui`，以及 `kiana-json`
//!   （JS 数字/字符串语义）。依赖边与逐条理由见 `ARCHITECTURE.md` §2.2、§2.3。
//! - 下游：15 个 crate 在各自的 `Cargo.toml` 中直接依赖本 crate，即 `kiana-session-core`、
//!   `kiana-config`、`kiana-theme`、`kiana-ui-kit`、`kiana-ext-api`、`kiana-export`、`kiana-tools`、
//!   `kiana-extensions`、`kiana-compaction`、`kiana-builtin-extensions`、`kiana-resources`、
//!   `kiana-session`、`kiana-interactive`、`kiana-rpc-print`、`kiana-coding-agent`；分层见
//!   `ARCHITECTURE.md` §2.1。
//! - 本 crate 位于 L3。§2.2 要求每条依赖边都有实际用途、不允许顺手加依赖，因此这里的
//!   `[dependencies]` 变化会影响整个 workspace 的编译顺序，改之前先核对 §2.2。
//!
//! # 核心概念与类型
//!
//! crate 根（本文件）不定义任何类型、常量或初始化函数：所有公开项都挂在下面的模块里，
//! 调用方一律写完整模块路径（如 `kiana_core::http_dispatcher::shared_http_client`、
//! `kiana_core::changelog::is_wsl`）。模块按职责大致分组：
//!
//! - 路径与文本基础：`config`（`APP_NAME`/`VERSION`/agent 目录等常量与派生路径）、`paths`
//!   （移植 `utils/paths.ts` 的路径语义）、`text`（BOM 处理）、`json`（JSONC 注释与尾逗号剥离）、
//!   `ansi`、`mime`（图片 MIME 探测）、`html`（HTML 实体解码）、`frontmatter`（YAML 1.2 子集）；
//! - 进程与终端：`child_process`、`shell`、`exec`、`output_guard`（stdout 接管）、`clipboard`
//!   （含 OSC 52 与打开浏览器）、`fs_watch`、`sleep`、`abort`；
//! - 网络：`http_dispatcher`（进程级共享 HTTP 客户端）、`management_http`（有界立即重试的管理类
//!   请求）；
//! - 跨 crate 契约与共享设施：`source_info`、`diagnostics`、`timings`、`kiana_manifest`、
//!   `slash_commands`，以及 `event_bus`（发布/订阅总线：`create_event_bus()` 每次返回一个
//!   独立的 `Arc<DefaultEventBus>`，由宿主创建并注入；本 crate 不提供进程级单例）；
//! - 其余工具：`mcp_servers`（MCP 配置校验与注册表）、`changelog`（变更日志/版本检查/自更新/
//!   WSL/deprecation/User-Agent）、`git`（Git URL 解析）、`zip`、`output_files`、`tools_manager`
//!   （fd/ripgrep 的查找与安装）、`syntax_highlight`（highlight.js 10.7.3 的移植）、
//!   `image_process`（由 `images` feature 门控）。
//!
//! # 一次典型调用（启动期）
//!
//! 本 crate 没有统一的 `init`，调用方按需组合。以 `kiana-coding-agent` 的启动流程为例
//! （`crates/kiana-coding-agent/src/app.rs` 的 `run_main`）：
//! 1. 第一步调用 `timings::reset_timings(TimingLabel::Main)` 重置主阶段计时；
//! 2. 解析参数后，在非交互模式且不是纯元数据命令时调用 `output_guard::take_over_stdout()`；
//!    此后经 `Output`、TUI 写钩子等“接管感知”路径的普通输出在接管时改写到 stderr，
//!    而直接调用 `println!` / `std::io::stdout()` 的代码不会被重定向；
//! 3. 期间用 `config` 的常量/目录函数与 `paths` 的规范化函数定位配置、session 与资源目录；
//! 4. 只有 Print/JSON 模式会在 `run_print_mode` 返回后调用 `output_guard::restore_stdout()`；
//!    RPC 分支（`run_rpc_mode` 返回后）以及接管之后的提前返回（例如参数校验失败）都不调用
//!    恢复，接管标志随进程退出而消失。各阶段通过 `timings::print_timings()` 输出耗时
//!    （只有 `KIANA_TIMING=1` 时才有内容）。
//!
//! # 边界与失败模式
//!
//! - 本 crate 不定义统一错误类型，也没有 crate 级错误包装：各模块按需返回 `Result`/`Option`，
//!   错误类型由模块自己定义（例如 `frontmatter::FrontmatterError`、
//!   `http_dispatcher::HttpDispatcherError`、`management_http::FetchError`）。调用方必须按模块
//!   文档分别处理，不能用统一的 `kiana_core::Error` 捕获。
//! - `image_process` 源文件内写有 `#![cfg(feature = "images")]`：关闭 default features 且不启用
//!   `images` 时整个模块不会被编译，任何 `kiana_core::image_process::...` 引用都会直接编译失败。
//!   当前 `Cargo.toml` 的 `default = ["images"]`，默认构建包含该模块。
//! - 进程级副作用集中在少数模块：`output_guard` 接管/恢复 stdout、`http_dispatcher` 的
//!   进程级共享客户端。它们的调用时序、并发与恢复要求写在各模块 `//!` 中，本文件不额外
//!   提供保证。`event_bus` 不属于这一类：每条总线都是调用方通过 `create_event_bus()`
//!   得到的独立实例，多个总线可以并存。
//! - 多处与 TS 的逐字对齐由 `crates/kiana-core/tests/` 下的对拍测试锁定；其中
//!   `paths_reference.rs`、`syntax_highlight_reference.rs` 的 fixture 由 TypeScript 参考实现
//!   生成，并在测试模块文档里列出不参与对拍的已知偏差。
//!
//! # 阅读顺序
//!
//! 1. `config` + `paths`：几乎所有调用方都先接触目录、常量与路径语义；
//! 2. `text`/`json`/`ansi`/`mime`/`html`/`frontmatter`：TS 文本语义的移植，便于理解“为什么逐字
//!    对齐 TS”；
//! 3. `child_process`/`shell`/`exec`/`output_guard`/`clipboard`/`fs_watch`：进程与终端；
//! 4. `http_dispatcher`/`management_http`：网络；
//! 5. `source_info`/`diagnostics`/`timings`/`kiana_manifest`/`slash_commands`：被其它 crate 直接
//!    引用的契约类型，改动会波及这些下游；
//! 6. 其余专用模块按需阅读。
//!
//! 多数模块的 `//!` 给出对应的 TS 源与已知语义差异；测试位置并不统一：有的在
//! `crates/kiana-core/tests/` 下（文件名不一定与模块同名），有的是源文件内的
//! `#[cfg(test)] mod tests`，且并非每个模块头都注明了测试位置。先读模块文档，再按这两处定位测试。
pub mod abort;
pub mod ansi;
// 第 6 周：工具输出落盘 / 预算 / 结构大纲 / 读取缓存（压缩恢复基础设施）。
pub mod artifact_store;
pub mod changelog;
pub mod child_process;
pub mod clipboard;
pub mod config;
pub mod diagnostics;
pub mod event_bus;
pub mod exec;
pub mod file_read_cache;
pub mod frontmatter;
pub mod fs_watch;
pub mod git;
pub mod html;
pub mod http_dispatcher;
pub mod image_process;
pub mod json;
pub mod json_outline;
pub mod kiana_manifest;
pub mod management_http;
pub mod mcp_servers;
pub mod mime;
pub mod output_budget;
pub mod output_files;
pub mod output_guard;
pub mod paths;
pub mod shell;
pub mod slash_commands;
pub mod sleep;
pub mod source_info;
pub mod syntax_highlight;
pub mod text;
pub mod timings;
pub mod tools_manager;
pub mod zip;

// 模块树与 `ARCHITECTURE.md` §5 的 33 个文件一一对应（32 个 `pub mod` + 本文件）。
// 第 6 周新增的 4 个压缩/工具输出基础设施模块不在这 33 个 TS 源文件里：
// `artifact_store`、`output_budget`、`json_outline`、`file_read_cache`。
// 下游一律用模块路径访问（如 `kiana_core::http_dispatcher::shared_http_client`、
// `kiana_core::changelog::is_wsl`）；实现单元**不要**在这里加 `pub use`（并行编译会互相打断）。
