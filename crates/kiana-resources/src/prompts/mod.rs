//! `core/prompt-templates.ts`：`PromptTemplate` / `load_prompt_templates`。
//!
//! 逐字移植 `packages/coding-agent/src/core/prompt-templates.ts`（325 行）。
//! `parse_command_args` / `substitute_args` / `expand_prompt_template` 分别位于
//! [`args`] / [`substitute`] / [`expand`]。
//!
//! frontmatter 解析复用 [`kiana_core::frontmatter::parse_frontmatter`]（`yaml` 包的 Rust 等价物）。
//!
//! # 职责与边界
//!
//! 本模块只负责两件事：把磁盘上的 Markdown 提示模板读成 [`PromptTemplate`] 列表，以及
//! 在用户输入以 `/name` 开头时把参数代入模板正文。它**不**负责按 name 去重、override、
//! 或修正来源信息——这些在拿到结果后由 [`crate::loader`] 完成。
//!
//! # 加载流程
//!
//! 1. `include_defaults` 为真时，先扫 `agentDir/prompts`（全局），再扫
//!    `cwd/.kiana/prompts`（项目）；
//! 2. 然后按 `prompt_paths` 的给定顺序逐个处理（目录取其直接子项，或单个 `.md` 文件）；
//! 3. 目录内的条目先按文件名排序再加载，保证结果稳定（`tests/prompt_templates.rs` 的
//!    `should_load_templates_in_sorted_readdir_order` 锁定该行为）；
//! 4. 文件名去掉尾部 `.md` 即为模板名；frontmatter 或文件读取失败只产生警告诊断并跳过
//!    该文件，不会让整次加载失败。
//!
//! # 与调用方的约定
//!
//! [`crate::loader`] 的 `update_prompts_from_paths` 以 `include_defaults: false` 调用
//! [`load_prompt_templates`]，随后自行按 name 去重、应用 override，并用来源信息表覆盖
//! `PromptTemplate::source_info`。因此本模块返回的 `source_info` 只是初始估计，不是最终值。
//!
//! # 失败模式
//!
//! 公开入口返回结构体而非 `Result`：文件读取与 frontmatter 解析问题降级成
//! [`kiana_core::diagnostics::ResourceDiagnostic`]；不存在的目录/路径（以及目录
//! `read_dir` 失败）被静默跳过，不产生诊断。

pub mod args;
pub mod expand;
pub mod substitute;

use kiana_core::diagnostics::ResourceDiagnostic;
use kiana_core::frontmatter::parse_frontmatter;
use kiana_core::source_info::{
    SourceInfo, SourceScope, SyntheticSourceInfoOptions, create_synthetic_source_info,
};

use crate::skills::{
    basename, dirname, join_path, node_read_error_message, resolve_config_path,
    resolve_path_default, resolve_path_trimmed, warning,
};

/// 一个从 Markdown 文件加载的提示模板。TS: `prompt-templates.ts#PromptTemplate`。
///
/// 不变量：
/// - `name` 只由文件名决定（去掉末尾 `.md`），与 frontmatter 内容无关；
/// - `content` 是 frontmatter 之后的正文原文，尚未代入参数；
/// - `description` 在 frontmatter 没有非空 `description` 时回退到正文第一条非空行，
///   并截断到 60 个 UTF-16 码元（见 `utf16_slice`）；
/// - `source_info` 可能被 [`crate::loader`] 后续覆盖，不能当作最终值。
#[derive(Debug, Clone, PartialEq)]
pub struct PromptTemplate {
    /// 模板名（文件名去掉尾部 `.md`）。
    pub name: String,
    /// 描述（frontmatter 或正文第一行）。
    pub description: String,
    /// `argument-hint`（空串不写入）。
    pub argument_hint: Option<String>,
    /// 正文（`parseFrontmatter` 的 body）。
    pub content: String,
    /// 来源信息。
    pub source_info: SourceInfo,
    /// 模板文件的绝对路径。
    pub file_path: String,
}

/// `load_prompt_templates` 的选项。TS: `prompt-templates.ts#LoadPromptTemplatesOptions`。
///
/// `cwd` 与 `agent_dir` 会先经 `resolve_path_default` 处理（展开开头的 `~`；相对路径按
/// 进程当前目录解析）；`prompt_paths` 中的相对路径按已解析的 `cwd` 解析并做 trim。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadPromptTemplatesOptions {
    /// 项目本地模板的工作目录。
    pub cwd: String,
    /// 全局模板的 agent 配置目录。
    pub agent_dir: String,
    /// 显式模板路径（文件或目录）。
    pub prompt_paths: Vec<String>,
    /// 是否包含默认模板目录。
    pub include_defaults: bool,
}

/// 加载结果。TS: `prompt-templates.ts#LoadPromptTemplatesResult`。
///
/// `diagnostics` 与 `templates` 相互独立：单个文件失败只丢掉自己的条目并追加诊断，
/// 其余模板照常返回。加载过程不做去重。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LoadPromptTemplatesResult {
    /// 模板（global → project → 显式路径顺序）。
    pub templates: Vec<PromptTemplate>,
    /// 诊断。
    pub diagnostics: Vec<ResourceDiagnostic>,
}

/// 从单个文件加载模板。TS: `prompt-templates.ts#loadTemplateFromFile`。
///
/// 读取失败（`ENOENT` / `EACCES` 等）或 frontmatter 解析失败时，返回 `None` 并附带一条
/// 警告诊断；调用方不需要（也无法）区分这两种失败。警告诊断的 message 含失败原因，
/// `ResourceDiagnostic::path` 携带文件路径（读取失败的 message 里还包含路径）。
///
/// 非 UTF-8 字节用 `String::from_utf8_lossy` 替换为 U+FFFD：Node 的
/// `readFileSync(path, "utf-8")` 同样用替换字符而不是报错。
fn load_template_from_file(
    file_path: &str,
    source_info: SourceInfo,
) -> (Option<PromptTemplate>, Vec<ResourceDiagnostic>) {
    let mut diagnostics: Vec<ResourceDiagnostic> = Vec::new();
    let bytes = match std::fs::read(file_path) {
        Ok(bytes) => bytes,
        Err(error) => {
            diagnostics.push(warning(
                node_read_error_message(&error, "open", file_path),
                file_path,
            ));
            return (None, diagnostics);
        }
    };
    // Node 的 `readFileSync(path, "utf-8")` 对非法 UTF-8 使用替换字符。
    let raw_content = String::from_utf8_lossy(&bytes);

    let parsed = match parse_frontmatter(&raw_content) {
        Ok(parsed) => parsed,
        Err(error) => {
            diagnostics.push(warning(error.to_string(), file_path));
            return (None, diagnostics);
        }
    };

    // 模板名只取 basename，并去掉末尾 ".md"；后缀判断大小写敏感。
    let file_name = basename(file_path);
    let name = file_name
        .strip_suffix(".md")
        .unwrap_or(&file_name)
        .to_string();

    // 描述：frontmatter 的字符串值，否则正文第一行非空行（最多 60 个 UTF-16 码元 + "..."）。
    let mut description = parsed
        .frontmatter
        .get("description")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    if description.is_empty() {
        let first_line = parsed
            .body
            .split('\n')
            .find(|line| !kiana_json::js_string::js_trim(line).is_empty());
        if let Some(first_line) = first_line {
            description = utf16_slice(first_line, 60);
            if first_line.encode_utf16().count() > 60 {
                description.push_str("...");
            }
        }
    }

    // 空字符串视为“未提供”，转成 None（`should_ignore_empty_argument_hint` 锁定）。
    let argument_hint = parsed
        .frontmatter
        .get("argument-hint")
        .and_then(serde_json::Value::as_str)
        .filter(|hint| !hint.is_empty())
        .map(str::to_string);

    (
        Some(PromptTemplate {
            name,
            description,
            argument_hint,
            content: parsed.body,
            source_info,
            file_path: file_path.to_string(),
        }),
        diagnostics,
    )
}

/// JS `string.slice(0, max_units)`（UTF-16 code unit）。
///
/// `ARCHITECTURE.md` §4.6：截断必须**不拆代理对**——切点落在代理对中间时把切点回退一个
/// code unit（比 TS 的 `slice` 少一个，与 `kiana-tui` 的 `BoundedTerminalWriter` 一致）。
/// JS 会产生孤立代理项，Rust 的 `String` 无法表示，这是类型层无法消除的差异；回退切分
/// 避免了 `U+FFFD` 污染输出。
fn utf16_slice(text: &str, max_units: usize) -> String {
    let units: Vec<u16> = text.encode_utf16().collect();
    let mut end = max_units.min(units.len());
    if end > 0
        && end < units.len()
        && (0xd800..=0xdbff).contains(&units[end - 1])
        && (0xdc00..=0xdfff).contains(&units[end])
    {
        end -= 1;
    }
    String::from_utf16_lossy(&units[..end])
}

/// 扫描目录的直接子项（非递归），加载 `.md` 文件。
///
/// TS: `prompt-templates.ts#loadTemplatesFromDir`。
///
/// 目录不存在或 `read_dir` 失败时返回空结果且不产生诊断（是否视为错误由调用方决定）。
/// 条目先收集再按文件名字符串序排序，然后逐个加载，顺序稳定。
/// 符号链接通过 `metadata`（跟随链接）判断最终目标是否为普通文件，断链跳过；
/// 仅接受小写 `.md` 后缀。
fn load_templates_from_dir(
    dir: &str,
    get_source_info: &dyn Fn(&str) -> SourceInfo,
) -> LoadPromptTemplatesResult {
    let mut templates: Vec<PromptTemplate> = Vec::new();
    let mut diagnostics: Vec<ResourceDiagnostic> = Vec::new();

    if !std::path::Path::new(dir).exists() {
        return LoadPromptTemplatesResult {
            templates,
            diagnostics,
        };
    }

    let Ok(read_dir) = std::fs::read_dir(dir) else {
        return LoadPromptTemplatesResult {
            templates,
            diagnostics,
        };
    };
    // Node 的 `readdirSync` 由 libuv `uv_fs_scandir` 按文件名（strcmp）排序后返回。
    let mut entries: Vec<(String, std::fs::FileType)> = Vec::new();
    for entry in read_dir.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        match entry.file_type() {
            Ok(file_type) => entries.push((name, file_type)),
            Err(_) => continue,
        }
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, file_type) in entries {
        let full_path = join_path(dir, &name);

        let mut is_file = file_type.is_file();
        let is_symlink = file_type.is_symlink();
        if is_symlink {
            // Dirent 的 file_type 描述链接本身；用 metadata 跟随链接看最终目标。
            match std::fs::metadata(&full_path) {
                Ok(metadata) => is_file = metadata.is_file(),
                Err(_) => continue,
            }
        }

        if is_file && name.ends_with(".md") {
            let (template, template_diagnostics) =
                load_template_from_file(&full_path, get_source_info(&full_path));
            if let Some(template) = template {
                templates.push(template);
            }
            diagnostics.extend(template_diagnostics);
        }
    }

    LoadPromptTemplatesResult {
        templates,
        diagnostics,
    }
}

/// 从所有配置位置加载提示模板。TS: `prompt-templates.ts#loadPromptTemplates`。
///
/// 顺序：`agentDir/prompts`、`cwd/{CONFIG_DIR_NAME}/prompts`（`includeDefaults` 时），然后按
/// `prompt_paths` 顺序。不存在的显式路径**静默跳过**（与 skills 不同）。
///
/// 补充约定：
/// - `include_defaults` 只控制上面两个默认目录，**不影响** `prompt_paths`；
/// - 目录内部按文件名排序，目录之间按上述顺序拼接，因此结果是确定性的；
/// - 本函数不去重，同名模板会按出现顺序全部保留，去重由调用方（`loader`）负责；
/// - 显式路径 `stat` 失败（如权限不足）会记录一条 `warning` 诊断，而不是返回值错误；
/// - 每个模板的 `source_info` 由路径所属的默认目录推断 scope：全局目录为
///   `SourceScope::User`，项目目录为 `SourceScope::Project`，其余路径 scope 为 `None`。
pub fn load_prompt_templates(options: LoadPromptTemplatesOptions) -> LoadPromptTemplatesResult {
    let resolved_cwd = resolve_path_default(&options.cwd);
    let resolved_agent_dir = resolve_path_default(&options.agent_dir);

    let mut templates: Vec<PromptTemplate> = Vec::new();
    let mut diagnostics: Vec<ResourceDiagnostic> = Vec::new();
    let add_result = |result: LoadPromptTemplatesResult,
                      templates: &mut Vec<PromptTemplate>,
                      diagnostics: &mut Vec<ResourceDiagnostic>| {
        templates.extend(result.templates);
        diagnostics.extend(result.diagnostics);
    };

    let global_prompts_dir = join_path(&resolved_agent_dir, "prompts");
    let project_prompts_dir = resolve_config_path(&resolved_cwd, "prompts");

    // 词法前缀判定（不做 canonicalize）：先统一 root 的写法，再比较 `root + 分隔符`
    // 前缀，避免 `/a/bc` 被误判成在 `/a/b` 之下。
    let is_under_path = |target: &str, root: &str| -> bool {
        let normalized_root = resolve_path_default(root);
        if target == normalized_root {
            return true;
        }
        let prefix = if normalized_root.ends_with(std::path::MAIN_SEPARATOR) {
            normalized_root
        } else {
            format!("{normalized_root}{}", std::path::MAIN_SEPARATOR)
        };
        target.starts_with(&prefix)
    };

    // 位于默认目录下的模板可确定 scope；显式路径则按“目录取自身、文件取父目录”
    // 生成 base_dir，scope 留空。
    // 判定不做 canonicalize：符号链接不会被解析，scope 只看词法路径前缀，
    // 与 TS 用字符串前缀判断的行为一致。
    let get_source_info = |resolved_path: &str| -> SourceInfo {
        if is_under_path(resolved_path, &global_prompts_dir) {
            return create_synthetic_source_info(
                resolved_path,
                SyntheticSourceInfoOptions {
                    source: "local".to_string(),
                    scope: Some(SourceScope::User),
                    origin: None,
                    base_dir: Some(global_prompts_dir.clone()),
                },
            );
        }
        if is_under_path(resolved_path, &project_prompts_dir) {
            return create_synthetic_source_info(
                resolved_path,
                SyntheticSourceInfoOptions {
                    source: "local".to_string(),
                    scope: Some(SourceScope::Project),
                    origin: None,
                    base_dir: Some(project_prompts_dir.clone()),
                },
            );
        }
        let base_dir = match std::fs::metadata(resolved_path) {
            Ok(metadata) if metadata.is_dir() => resolved_path.to_string(),
            _ => dirname(resolved_path),
        };
        create_synthetic_source_info(
            resolved_path,
            SyntheticSourceInfoOptions {
                source: "local".to_string(),
                scope: None,
                origin: None,
                base_dir: Some(base_dir),
            },
        )
    };

    if options.include_defaults {
        // 默认目录先全局后项目；目录内再按文件名排序，两层顺序叠加即最终顺序。
        add_result(
            load_templates_from_dir(&global_prompts_dir, &get_source_info),
            &mut templates,
            &mut diagnostics,
        );
        add_result(
            load_templates_from_dir(&project_prompts_dir, &get_source_info),
            &mut templates,
            &mut diagnostics,
        );
    }

    for raw_path in &options.prompt_paths {
        let resolved_path = resolve_path_trimmed(raw_path, &resolved_cwd);
        if !std::path::Path::new(&resolved_path).exists() {
            continue;
        }

        // 目录：整个目录按上面的目录流程加载；单文件：仅当扩展名恰好是 ".md"
        // （小写、大小写敏感）才加载，其他后缀静默忽略。路径存在性已在上面用
        // `exists()` 检查，此处 `metadata` 返回 `Err` 只对应两次调用之间状态变化
        // 的竞态，与 TS 的 try/catch 一样记一条 warning 诊断。
        match std::fs::metadata(&resolved_path) {
            Ok(metadata) => {
                if metadata.is_dir() {
                    add_result(
                        load_templates_from_dir(&resolved_path, &get_source_info),
                        &mut templates,
                        &mut diagnostics,
                    );
                } else if metadata.is_file() && resolved_path.ends_with(".md") {
                    let (template, template_diagnostics) =
                        load_template_from_file(&resolved_path, get_source_info(&resolved_path));
                    if let Some(template) = template {
                        templates.push(template);
                    }
                    diagnostics.extend(template_diagnostics);
                }
            }
            Err(error) => {
                diagnostics.push(warning(
                    node_read_error_message(&error, "stat", &resolved_path),
                    &resolved_path,
                ));
            }
        }
    }

    LoadPromptTemplatesResult {
        templates,
        diagnostics,
    }
}

/// 展开匹配模板名的提示文本；未匹配时原样返回。
///
/// TS: `prompt-templates.ts#expandPromptTemplate`。实现位于 [`expand`]，这里提供便捷入口，
/// 便于调用方只依赖 `kiana_resources::prompts::*`。语义与错误边界见
/// [`expand::expand_prompt_template`]。
pub fn expand_prompt_template(text: &str, templates: &[PromptTemplate]) -> String {
    expand::expand_prompt_template(text, templates)
}
