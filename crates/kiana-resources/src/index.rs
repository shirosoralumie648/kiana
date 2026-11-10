//! kiana-resources 的再导出面（crate 的公开 barrel）。
//!
//! 与 `kiana-config` / `kiana-theme` 的约定一致：模块本身以 `pub use crate::<module>;` 保留，
//! 无歧义的叶子符号再扁平导出。**不要**把 `package`、`loader`、`prompts` 等模块整体 glob
//! 进来——它们都有 `Error` / `Result` / `*Result` 类同名项（`README` 里 §5 规则 4）。
//!
//! # 为什么需要这层 barrel
//! crate 根 `lib.rs` 有 `pub use index::*;`。模块名逐一再导出后，外部调用方使用
//! `kiana_resources::system_prompt::build_system_prompt` 这样的模块路径；只有本文件底部
//! 列出的少数叶子符号（diagnostics、loader 的部分类型/函数）被扁平化到 crate 根。
//! 保留模块名是为了对齐 TS 的 `core/<name>.ts` 文件路径，便于按源码位置检索。
//!
//! # 结构
//! - 第一个 `pub use` 块只导出模块名（不展开内容），对应 TS 的模块边界；
//! - 后面的具名 `pub use` 是 TS 导出面的扁平化：按 TS 文件分组，只挑无同名冲突的符号。
//!
//! # 修改约束
//! 这里只有再导出、没有逻辑。新增扁平导出前先搜索 crate 内是否已有同名项；一旦出现二义性，
//! `index::*` 会把冲突传播到 crate 根，所有调用方都会编译失败。各模块自身的行为测试在
//! `crates/kiana-resources/tests/` 下。

pub use crate::bug_report;
pub use crate::crash_log;
pub use crate::diagnostics;
pub use crate::footer_data_provider;
pub use crate::loader;
pub use crate::manifest;
pub use crate::mcp_annotations;
pub use crate::package;
pub use crate::prompts;
pub use crate::skills;
pub use crate::source_info;
pub use crate::system_prompt;
pub use crate::timings;

// 以上只导出模块名；以下按 TS 文件分组扁平导出具体符号。

// ---------------------------------------------------------------------------
// diagnostics.ts
// ---------------------------------------------------------------------------
pub use crate::diagnostics::{DiagnosticKind, ResourceCollision, ResourceDiagnostic, ResourceType};

// ---------------------------------------------------------------------------
// resource-loader.ts（TS 的导出面）
// ---------------------------------------------------------------------------
pub use crate::loader::context_files::load_project_context_files;
pub use crate::loader::warnings::{format_package_install_warning, is_builtin_extension};
pub use crate::loader::{
    AgentsFilesResult, DefaultResourceLoader, DefaultResourceLoaderOptions, PromptsResult,
    ResolveProjectTrust, ResolveProjectTrustFuture, ResourceExtensionPath, ResourceExtensionPaths,
    ResourceLoader, ResourceLoaderError, ResourceLoaderReloadOptions, SkillsResult,
    SystemPromptSource, ThemesResult,
};
