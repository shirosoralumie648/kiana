//! 对应 ca-resources：包管理、资源加载、skills、prompt 模板、系统提示。
//!
//! TS 源：packages/coding-agent/src/core/{package-manager,resource-loader,skills,prompt-templates,system-prompt,bug-report,bug-report-upload,crash-log,footer-data-provider}.ts。
//! 包根 `package-manager-cli.ts` 不在这里：它依赖 `cli/config-selector.ts`，归属 `kiana-coding-agent`（`ARCHITECTURE.md` §3.6）。
//! `mcp-servers.ts` 也不在这里：它是 `kiana_core::mcp_servers`（`ARCHITECTURE.md` §5 的 kiana-resources 文件表）。
//!
//! `source_info` / `diagnostics` / `timings` / `manifest` 是 `kiana-core` 定义的再导出（§3.5）。
//!
//! 依赖方向：本 crate 依赖 `kiana-config`/`kiana-theme`/`kiana-ext-api`/`kiana-extensions`，**不**依赖
//! `kiana-session`/`kiana-coding-agent`；`kiana-session` 与 `kiana-coding-agent` 反过来依赖本 crate，因此是 DAG。
//!
//! 公开面汇总在 [`index`]，由下面的 `pub use index::*;` 抬到 crate 根。
//!
//! # 这个 crate 解决什么问题
//!
//! 把分散在 settings 包配置、标准目录（agent 配置目录与项目 `.kiana`）以及调用方额外注入
//! 路径中的可配置资源——扩展、技能、prompt 模板、主题——连同上下文文件
//! （`AGENTS.md`/`CLAUDE.md`）与系统提示，收敛成一份可直接查询的快照。它还提供这些资源
//! 所依赖的包管理（安装/解析/更新），以及 bug 报告、崩溃日志、footer 数据等外围装配件。
//! 没有这一层，每个调用方都要各自处理包解析、启用/禁用、路径优先级、元数据归属与诊断
//! 文案，并且无法保证加载顺序一致。
//!
//! # 在系统中的位置
//!
//! 上游：`kiana-config`（settings）、`kiana-theme`（主题加载）、`kiana-ext-api` /
//! `kiana-extensions`（扩展类型与加载）、`kiana-core`（路径、诊断、来源信息、计时等基础
//! 实现）。下游：`kiana-session` 构造资源加载器并在创建会话时 reload；`kiana-coding-agent`
//! 的 package 子命令与 `kiana-interactive` 的交互模式/`/bug` 流程也直接使用本 crate 的
//! 公开 API。依赖方向必须保持为 DAG：本 crate **不**依赖上述下游 crate。
//!
//! # 核心类型与入口
//!
//! - 资源加载：[`ResourceLoader`] 是“查询快照 + reload”的 trait，[`DefaultResourceLoader`]
//!   是唯一实现；构造参数见 [`DefaultResourceLoaderOptions`]，查询结果是各类 `*Result` 快照。
//! - 包管理：[`package::PackageManager`] / `package::DefaultPackageManager` 负责解析 settings
//!   中的包源并安装、更新；资源加载器在 reload 中调用它取得四类资源的路径。
//! - 可单独使用的加载入口：[`skills::load_skills`]、[`prompts::load_prompt_templates`]、
//!   [`system_prompt::build_system_prompt`]。
//! - 辅助功能：[`bug_report`]（报告收集/归档/上传）、[`crash_log`]（崩溃摘要持久化）、
//!   [`footer_data_provider`]（git 分支与扩展状态）。
//!
//! # 一次典型调用
//!
//! 1. 用 [`DefaultResourceLoaderOptions`] 指定 `cwd`、`agent_dir` 与（可选的）
//!    `SettingsManager`，调用 [`DefaultResourceLoader::new`]；
//! 2. `await` [`DefaultResourceLoader::reload`]（trait 方法为 [`ResourceLoader::reload`]）：
//!    读取 settings、解析并安装缺失的包，再加载扩展/技能/prompt/主题/上下文文件/系统提示；
//! 3. 之后通过 9 个 `get_*` 方法取快照；设置或磁盘内容变化时再次 reload。
//!
//! 具体构造与调用位置见 `crates/kiana-session/src/sdk.rs` 的 `create_agent_session`。
//!
//! # 边界与失败模式
//!
//! - reload 的错误统一是 [`ResourceLoaderError`]（包解析、manifest、IO 等）；失败会提前
//!   返回，但**不回滚**已完成步骤写回的状态（详见 [`loader`] 模块文档）。
//! - 单个资源文件读取或解析失败通常降级为 `ResourceDiagnostic` 警告并跳过；包安装失败在
//!   `resolve` 中记录为安装失败项，同样不中止整次解析。
//! - 资源加载器持有非 `Send` 的扩展工厂，因此创建、reload 与查询应在同一线程驱动
//!   （`LocalSet` / `current_thread` runtime）。
//!
//! # 阅读顺序
//!
//! 先读 [`loader`]（资源快照与 reload 阶段机），再读 [`package`]（包源解析、路径布局与
//! 安装）；需要理解具体文件格式时按 [`skills`] → [`prompts`] → [`system_prompt`] →
//! [`loader::context_files`] 的顺序阅读。`diagnostics` / `source_info` / `timings` /
//! [`manifest`] 只是再导出面，没有逻辑；bug 报告与崩溃日志属于独立辅助链路，可按需阅读。
pub mod bug_report;
pub mod crash_log;
pub mod diagnostics;
pub mod footer_data_provider;
pub mod index;
pub mod loader;
pub mod manifest;
pub mod mcp_annotations;
pub mod package;
pub mod prompts;
pub mod skills;
pub mod source_info;
pub mod system_prompt;
pub mod timings;

pub use index::*;
