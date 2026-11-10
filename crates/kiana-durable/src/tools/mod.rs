//! `tools/` 目录：编码工具与共享的 diff/图片辅助。
//!
//! TS 源：`packages/durable/src/tools/*.ts`。`read`/`write`/`edit`/`bash` 组成 `coding-tools`
//! 扩展（[`create_coding_tools`]），`powershell` 需要单独创建；两者都通过 `ToolRegistration`
//! 交给 harness。
//!
//! # 这一层的职责
//! `read`/`write`/`edit`/`bash` 各自导出一个 `create_*_tool()`，返回 `ToolRegistration`
//! （发给 provider 的定义、执行体、参数修复、结构化输出 schema 与输出限制）。其余文件是
//! 共享辅助：`env`、`path_utils`、`file_mutation_queue`、`edit_diff`、`image`。
//! 本文件只做装配：把四个工具包成 [`Extension`]，由调用方安装进 registry。
//! [`crate::harness::define::define_tool`] 与
//! [`crate::harness::define::define_extension`] 只做构造/包装，不校验也不注册；调用它们时
//! 工具还没有生效，必须等 `Extension::tools()` 被 registry 收集并解析后才会成为可调用工具。
//!
//! # 工具集合
//! [`create_coding_tools`] 按 read、write、edit、bash 的顺序返回注册项：
//! - `read` 使用传入的 `ReadToolOptions`，其中 `images` 决定是否启用图片处理器；
//! - `bash` 以 `None` 创建，即没有 `command_prefix`/`prepare` 回调；
//! - `powershell` 不在这个扩展里，需要调用 [`bash::create_powershell_tool`] 单独创建。
//!
//! # 常量实例
//! [`CodingTools`] 是 `create_coding_tools(ReadToolOptions::default())` 的进程级
//! `LazyLock`：第一次解引用时创建，之后每次 `tools()` 都克隆同一组注册项（`execute` 等
//! `Arc` 共享）。TS 用模块级常量；Rust 的 `Arc<dyn Extension>` 不能放进 `const`。
//!
//! # 导出面
//! 这里还 re-export 了各工具的参数/结果类型、`edit_diff` 的纯函数与 `image` 的
//! [`ImageProcessor`] 契约；子模块本身也是 `pub`，调用方可以按文件名导入内部工具。

// --- 子模块 ---
pub mod bash;
pub mod edit;
pub mod edit_diff;
pub mod env;
pub mod file_mutation_queue;
pub mod image;
pub mod path_utils;
pub mod read;
pub mod write;

use std::sync::{Arc, LazyLock};

use crate::harness::define::define_extension;
use crate::harness::types::{Extension, ToolRegistration};

pub use bash::{
    BashExecution, BashPrepare, BashToolInput, BashToolOptions, PowerShellToolInput,
    PowerShellToolOptions, ShellStructuredOutput, create_bash_tool, create_powershell_tool,
};
pub use edit::{EditToolDetails, EditToolInput, create_edit_tool, prepare_edit_arguments};
pub use edit_diff::{
    AppliedEditsResult, Change, DiffStringResult, Edit, FuzzyMatchResult, LineEnding,
    TextReplacement, apply_edits_to_normalized_content,
    apply_replacements_preserving_unchanged_lines, detect_line_ending, diff_lines, fuzzy_find_text,
    generate_diff_string, generate_unified_patch, normalize_for_fuzzy_match, normalize_to_lf,
    restore_line_endings, strip_bom,
};
pub use image::{
    DEFAULT_IMAGE_LIMITS, ImageLimits, ImageProcessor, PreparedImage, ResizedDimensions,
};
pub use read::{ReadToolDetails, ReadToolInput, ReadToolOptions, create_read_tool};
pub use write::{WriteToolInput, create_write_tool};

/// `coding-tools` 扩展：`read`、`write`、`edit` 与 `bash`；不会自动安装。
#[derive(Clone)]
struct CodingToolsExtension {
    tools: Vec<ToolRegistration>,
}

impl Extension for CodingToolsExtension {
    fn name(&self) -> &str {
        "coding-tools"
    }

    fn tools(&self) -> Vec<ToolRegistration> {
        self.tools.clone()
    }
}

/// `createCodingTools(options)`：`read` 使用传入的 `images`。
///
/// 返回一个名为 `coding-tools` 的 [`Extension`]，工具顺序固定为 read、write、edit、bash。
/// `bash` 用 `None` 创建，所以没有命令前缀与 `prepare`；需要这些能力时直接使用
/// [`bash::create_bash_tool`] 并自行组装扩展。扩展不会自动安装，调用方必须把
/// `tools()` 的结果交给 registry。
pub fn create_coding_tools(options: ReadToolOptions) -> Arc<dyn Extension> {
    define_extension(CodingToolsExtension {
        tools: vec![
            create_read_tool(options),
            create_write_tool(),
            create_edit_tool(),
            create_bash_tool(None),
        ],
    })
}

/// `CodingTools = createCodingTools()`：没有图片处理器的 `coding-tools` 扩展。
///
/// TS 用常量；Rust 侧因为 `Arc<dyn Extension>` 不能在常量里构造，用进程级 `LazyLock` 等价物。
///
/// 解引用得到共享的 `Arc`；`tools()` 返回注册项的克隆（`ToolRegistration: Clone` 会克隆
/// `execute` 等 `Arc`）。因此多个调用方可以安装同一实例，工具实现只有一份。
#[allow(non_upper_case_globals)]
pub static CodingTools: LazyLock<Arc<dyn Extension>> =
    LazyLock::new(|| create_coding_tools(ReadToolOptions::default()));
