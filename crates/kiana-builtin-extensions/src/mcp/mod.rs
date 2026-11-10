//! `extensions/mcp/`（9 个文件）。
//!
//! MCP（Model Context Protocol）客户端扩展的全部实现；TS 目录
//! `packages/coding-agent/src/extensions/mcp/`。各 TS `*.lazy.ts` 的动态 import 包装并入同名
//! 模块（`runtime.lazy.ts`/`cli.lazy.ts` 的 `loadMcpRuntime`/`loadMcpCommand` 并入
//! `runtime.rs`/`cli.rs`，见两文件头与 `spec/ca-builtin-extensions.md` §1.5）。
//!
//! # 这个模块解决什么问题
//!
//! MCP 服务器（stdio 子进程或 HTTP 服务）向客户端暴露工具与资源等能力。本模块把它们接入
//! kiana：从 `mcp.json` 读配置、建立并维持连接、把协议数据适配成 kiana 的工具定义与模型
//! 内容、记录服务器日志，并提供 `kiana mcp` 命令行与 `/mcp` 管理器。
//!
//! # 子模块与依赖方向
//!
//! - [`config`]：`mcp.json` 的读取、全局/项目合并、编辑与重写；不建连接、不解析 OAuth。
//! - [`runtime`]：一个 [`runtime::McpServerConnection`] 对应一个服务器，负责懒连接、重连、
//!   重试、会话过期与 401；消费 [`config::McpServerEntry`]，不读 `mcp.json`
//!   （凭据文件的读写由 [`oauth`] 的存储访问承担）。
//! - [`oauth`]：OAuth 授权码流程与 `mcp-auth.json` 凭据存储，被 [`runtime`] 与登录入口共用。
//! - [`tools`] / [`resources`]：把 `tools/*`、`resources/*` 的协议结果适配成 kiana 工具与
//!   模型可见内容。
//! - [`ui`]：`/mcp` 管理器视图（菜单、状态页、登录页）。
//! - [`log`]：`notifications/message` → `mcp.log` 的日志行格式化与轮转。
//! - [`cli`]：`kiana mcp add/remove/list/login/logout` 子命令。
//! - [`index`]：内建扩展本体（会话生命周期、工具注册、系统提示 section），编排以上模块。
//!
//! # 阅读顺序
//!
//! 先读 [`config`]（配置形状与合并规则），再读 [`runtime`]（连接与错误状态机）；
//! [`index`] 是调用链主干，之后读 [`tools`]/[`resources`] 的结果转换与 [`cli`] 的命令行外壳。
//! [`log`] 与 [`oauth`] 相对独立，可按需阅读。
//!
//! # 注意
//!
//! - crate 根（`lib.rs`）只重导出 `cli`/`index`/`tools` 的部分符号；`config` 的类型由
//!   `kiana_builtin_extensions::mcp::config` 直接提供（集成测试也这样导入）。
//! - [`config`] 的编辑函数会重写整个文件（保留未知键、键序与检测到的缩进），调用方不能
//!   假定 `mcp.json` 在写回后保持原字节布局。

// --- 子模块 ---
pub mod cli;
pub mod config;
pub mod filter;
pub mod index;
pub mod log;
pub mod oauth;
pub mod resources;
pub mod runtime;
pub mod tools;
pub mod ui;
