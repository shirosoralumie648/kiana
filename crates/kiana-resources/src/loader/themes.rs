//! 主题发现与加载（调 `kiana-theme` 的 `loadThemeFromPath`）。
//!
//! TS: `packages/coding-agent/src/core/resource-loader.ts` 的
//! `loadThemes` / `loadThemesFromDir` / `loadThemeFromFile`。
//!
//! 诊断级别是 `warning`（与 additional 主题路径的 `error` 不同）；
//! 目录里的符号链接先 `statSync` 判断是否普通文件，失败则静默跳过。
//!
//! # 负责的边界
//! 只负责“找文件 + 调 `kiana-theme` 的加载函数 + 把失败转成 warning 诊断”。同名去重
//! （首见保留）与 override 都在调用方 `DefaultResourceLoader::update_themes_from_paths`
//! 完成；本模块不修改已加载的主题，也不写文件系统。
//!
//! # 执行流程
//! 1. `include_defaults` 为真时先扫 `agent_dir/themes` 与 `cwd/.kiana/themes`（全局在前）；
//! 2. 再按 `paths` 顺序逐条处理：不存在 → warning；目录 → 扫描；`.json` 文件 → 加载；
//!    其它 → warning；
//! 3. 每个条目失败只产生一条 warning，不中断后续条目；函数本身不返回 `Result`。
//!
//! # 注意事项
//! - 本模块的诊断统一是 `DiagnosticKind::Warning`；调用方 `reload` 还会为“additional
//!   主题路径不存在”另补 `Error` 诊断（`loader/mod.rs` 第 23 步），两者不要混淆。
//! - 目录内的顺序按文件名排序，同名主题去重的胜者依赖这个顺序（测试
//!   `themes_load_in_node_readdir_name_order` 锁定）。

use std::path::Path;
use std::sync::Arc;

use kiana_core::diagnostics::{DiagnosticKind, ResourceDiagnostic};
use kiana_theme::{Theme, load_theme_from_path};
use kiana_tui::TerminalColorMode;

use crate::loader::source_info_map::{node_fs_error_message, resolve_resource_path};

/// `loadThemes` 的结果。
#[derive(Debug, Clone, Default)]
pub struct ThemesLoadResult {
    /// 已加载的主题（目录顺序 + 显式路径顺序）。
    pub themes: Vec<Arc<Theme>>,
    /// 诊断（`warning`）。
    pub diagnostics: Vec<ResourceDiagnostic>,
}

fn warning(message: impl Into<String>, path: &str) -> ResourceDiagnostic {
    ResourceDiagnostic {
        kind: DiagnosticKind::Warning,
        message: message.into(),
        path: Some(path.to_string()),
        collision: None,
    }
}

/// 加载主题路径列表。TS: `resource-loader.ts#loadThemes`。
///
/// - `include_defaults` 时先扫 `agentDir/themes` 与 `cwd/.kiana/themes`。
/// - 每条路径：不存在 → `theme path does not exist`；目录 → [`load_themes_from_dir`]；
///   以 `.json` 结尾的文件 → [`load_theme_from_file`]；其它 → `theme path is not a json file`。
/// - `stat` 失败 → 用 Node `error.message`（`EACCES: permission denied, stat '<path>'`）
///   作为 `warning` 的 message。
///
/// 路径一律先经 `resolve_resource_path`（展开 `~`、转换 `file://`、trim）；合成路径原样
/// 保留。`include_defaults` 为真时先扫 `agent_dir/themes`、再扫 `cwd/.kiana/themes`；
/// `reload` 走的是 `include_defaults = false`（默认目录已由包管理器发现并并入 `paths`）。
/// 本函数**不**做同名去重，去重由调用方随后执行。
pub fn load_themes(
    cwd: &str,
    agent_dir: &str,
    paths: &[String],
    include_defaults: bool,
    color_mode: TerminalColorMode,
) -> ThemesLoadResult {
    let mut themes: Vec<Arc<Theme>> = Vec::new();
    let mut diagnostics: Vec<ResourceDiagnostic> = Vec::new();

    if include_defaults {
        let config_dir =
            crate::loader::source_info_map::join_path(cwd, kiana_core::config::CONFIG_DIR_NAME);
        let default_dirs = [
            crate::loader::source_info_map::join_path(agent_dir, "themes"),
            crate::loader::source_info_map::join_path(&config_dir, "themes"),
        ];
        for dir in &default_dirs {
            load_themes_from_dir(dir, &mut themes, &mut diagnostics, color_mode);
        }
    }

    for path in paths {
        let resolved = resolve_resource_path(cwd, path);
        if !Path::new(&resolved).exists() {
            diagnostics.push(warning("theme path does not exist", &resolved));
            continue;
        }

        match std::fs::metadata(&resolved) {
            Ok(stat) => {
                if stat.is_dir() {
                    load_themes_from_dir(&resolved, &mut themes, &mut diagnostics, color_mode);
                } else if stat.is_file() && resolved.ends_with(".json") {
                    load_theme_from_file(&resolved, &mut themes, &mut diagnostics, color_mode);
                } else {
                    diagnostics.push(warning("theme path is not a json file", &resolved));
                }
            }
            Err(error) => {
                diagnostics.push(warning(
                    node_fs_error_message(&error, "stat", &resolved),
                    &resolved,
                ));
            }
        }
    }

    ThemesLoadResult {
        themes,
        diagnostics,
    }
}

/// 扫描一个主题目录。TS: `resource-loader.ts#loadThemesFromDir`。
///
/// 不存在则直接返回；`readdirSync` 失败 → `warning`（message 为 Node `error.message`，
/// 如 `EACCES: permission denied, scandir '<dir>'`，path 为目录）。
///
/// 条目先按 `file_name` 排序再逐个处理；非普通文件（含目录与断链符号链接）静默跳过，名字
/// 不以 `.json` 结尾的也跳过（区分大小写）。`file_type()` 本身失败时只跳过该条目，不产生
/// 诊断。函数不递归进入子目录。
pub fn load_themes_from_dir(
    dir: &str,
    themes: &mut Vec<Arc<Theme>>,
    diagnostics: &mut Vec<ResourceDiagnostic>,
    color_mode: TerminalColorMode,
) {
    if !Path::new(dir).exists() {
        return;
    }

    // TS 的 `readdirSync` 一次性返回全部条目：迭代中途出错时整个调用抛错，
    // 已读到的条目一个也不处理（Node 文案用 `scandir`）。
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries.collect::<Result<Vec<_>, _>>(),
        Err(error) => Err(error),
    };
    let mut entries = match entries {
        Ok(entries) => entries,
        Err(error) => {
            diagnostics.push(warning(node_fs_error_message(&error, "scandir", dir), dir));
            return;
        }
    };
    // Node 的 `readdirSync`（libuv `uv_fs_scandir`）按文件名（strcmp）排序后返回；
    // `std::fs::read_dir` 是 getdents 原始顺序。主题顺序与 `dedupeThemes` 的胜者
    // 都取决于这个顺序。
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(_) => continue,
        };
        let mut is_file = file_type.is_file();
        if file_type.is_symlink() {
            match std::fs::metadata(entry.path()) {
                Ok(stat) => is_file = stat.is_file(),
                Err(_) => continue,
            }
        }
        if !is_file {
            continue;
        }
        if !name.ends_with(".json") {
            continue;
        }
        let file_path = crate::loader::source_info_map::join_path(dir, &name);
        load_theme_from_file(&file_path, themes, diagnostics, color_mode);
    }
}

/// 加载单个主题文件。TS: `resource-loader.ts#loadThemeFromFile`。
///
/// 解析失败 → `warning`，message 是 `kiana-theme` 的 `ThemeError` 文案（直接使用其
/// Display，避免外层再包装一层）。`kiana_theme` 的读取错误已经按 Node 的
/// `ENOENT: no such file or directory, open '<path>'` 形式生成（`node_fs_read_error`），
/// 这里**不要**再用 `std::io::Error` 的 Display 覆盖。
///
/// 成功时把 `Arc<Theme>` 追加到 `themes`（与 getter 共享同一实例）；失败时只追加一条
/// warning，不影响调用方继续处理其它文件。`Some(color_mode)` 指定 Theme 使用的终端色彩
/// 模式，`None` 时由 `kiana-theme` 取终端默认值。
pub fn load_theme_from_file(
    file_path: &str,
    themes: &mut Vec<Arc<Theme>>,
    diagnostics: &mut Vec<ResourceDiagnostic>,
    color_mode: TerminalColorMode,
) {
    match load_theme_from_path(Path::new(file_path), Some(color_mode)) {
        Ok(theme) => themes.push(theme),
        Err(error) => diagnostics.push(warning(error.to_string(), file_path)),
    }
}
