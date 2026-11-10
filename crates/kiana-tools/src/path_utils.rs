//! core/tools/path-utils.ts。
//!
//! 路径解析薄层：`~` 展开、Unicode 空格归一、`@` 前缀剥离都由
//! [`kiana_core::paths`] 完成；本模块只保留 `read` 工具独有的 macOS 变体回退
//! （AM/PM 窄不换行空格、NFD、弯引号、NFD+弯引号，顺序固定）。
//!
//! 工具层只有 `read` 使用 [`resolve_read_path`] / [`resolve_read_path_async`]；
//! `write`/`edit`/`ls`/`grep`/`find` 一律用 [`resolve_to_cwd`]（不做回退）。crate 外，
//! `kiana-coding-agent` 的 CLI `@file` 处理（`cli/file_processor.rs`）也调用同步版
//! [`resolve_read_path`]。
//!
//! # JS 语义
//!
//! - `fileExists` / `pathExists` 等价于 `fs.access(p, F_OK)`：任何错误都算「不存在」。
//! - `"NFD"` 用 [`unicode_normalization::UnicodeNormalization::nfd`]。
//! - `pathToFileURL` 不在这里使用；返回类型是 [`PathBuf`]（调用方的文件系统边界），
//!   需要字符串时用 `to_string_lossy()`。
//!
//! # 变体回退的顺序与边界
//!
//! [`resolve_read_path`] / [`resolve_read_path_async`] 的候选顺序固定为：原路径 → AM/PM
//! 窄空格 → NFD → 弯引号 → NFD+弯引号。每个候选都与**原始解析结果**比较，相同就跳过；
//! 第一个存在的立即返回。全都不存在时返回原解析路径——**返回了路径不等于文件存在**，
//! 调用方必须自己再做存在性检查（`read` 的 `operations.access` 就是这一步）。
//!
//! 每次调用都会重新做存在性检查，没有进程级缓存；同步版用阻塞的 `Path::exists`，异步版用
//! `tokio::fs::metadata`，异步调用点应使用异步版。
//!
//! # 注意事项
//!
//! 所有函数都是纯词法转换或存在性检查，不创建、修改或删除文件；解析结果可能是相对路径，
//! 也可能仍是无法解析的 `file://` 字符串（由 `kiana_core::paths` 原样返回）。

use std::path::{Path, PathBuf};

use kiana_core::paths::{PathInputOptions, normalize_path, resolve_path};
use regex::Regex;
use std::sync::LazyLock;
use unicode_normalization::UnicodeNormalization;

/// `NARROW_NO_BREAK_SPACE = " "`。
const NARROW_NO_BREAK_SPACE: char = '\u{202F}';

/// `/ (AM|PM)\./gi`。
static AM_PM_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i) (AM|PM)\.").expect("valid AM/PM regex"));

/// `expandPath` / `resolveToCwd` 共用的选项：`{ normalizeUnicodeSpaces: true, stripAtPrefix: true }`。
///
/// `trim` 保持 [`PathInputOptions`] 的默认值 `false`：工具路径不做首尾空白裁剪。
fn tool_path_options() -> PathInputOptions {
    PathInputOptions {
        normalize_unicode_spaces: true,
        strip_at_prefix: true,
        ..PathInputOptions::default()
    }
}

/// TS: `tryMacOSScreenshotPath(filePath)` —— 把 `" 12.30.45 PM.png"` 里的半角空格换成
/// U+202F（大小写不敏感，`am`/`pm` 也命中）。
fn try_macos_screenshot_path(file_path: &str) -> String {
    AM_PM_RE
        .replace_all(file_path, format!("{NARROW_NO_BREAK_SPACE}$1."))
        .into_owned()
}

/// TS: `tryNFDVariant(filePath)` —— macOS 以 NFD 形式存文件名。
fn try_nfd_variant(file_path: &str) -> String {
    file_path.nfd().collect()
}

/// TS: `tryCurlyQuoteVariant(filePath)` —— macOS 截图名用 U+2019，用户通常输入 U+0027。
fn try_curly_quote_variant(file_path: &str) -> String {
    file_path.replace('\'', "\u{2019}")
}

/// TS: `fileExists(filePath)`（同步 `fs.accessSync(p, F_OK)`）。
///
/// 实现是 `Path::exists()`（内部 `fs::metadata().is_ok()`）：任何错误都当作不存在。
fn file_exists(file_path: &Path) -> bool {
    file_path.exists()
}

/// `pathExists(filePath)`：`fs.access(p, F_OK)` 的异步版本，吞掉所有错误。
///
/// 用 `tokio::fs::metadata`；任何错误（不存在、无权限、路径组件不是目录等）都返回 `false`，
/// 因此调用方不能用它区分失败原因。
pub async fn path_exists(file_path: impl AsRef<Path>) -> bool {
    tokio::fs::metadata(file_path.as_ref()).await.is_ok()
}

/// `expandPath(filePath)`：规范化路径（Unicode 空格、去 `@`、`~` 展开、win32 shell 路径）。
///
/// 纯字符串转换，不访问文件系统；结果可能仍是相对路径（如 `file.txt`），也可能仍是无法
/// 解析的 `file://` 字符串（原样返回，见 `kiana_core::paths` 的模块说明）。
pub fn expand_path(file_path: &str) -> String {
    normalize_path(file_path, &tool_path_options())
}

/// `resolveToCwd(filePath, cwd)`：相对路径按 `cwd` 解析，绝对路径原样。
///
/// 纯词法解析（`kiana_core::paths::resolve_path`），不检查存在性；`cwd` 为空或仍为相对
/// 路径时，以进程当前目录为基准。
pub fn resolve_to_cwd(file_path: &str, cwd: &str) -> PathBuf {
    PathBuf::from(resolve_path(file_path, cwd, &tool_path_options()))
}

/// `resolveReadPath(filePath, cwd)`：同步版，额外尝试 macOS 命名变体。
///
/// 顺序严格为：原路径 → AM/PM 窄空格 → NFD → 弯引号 → NFD+弯引号；全不命中时返回原解析结果。
/// 同步实现用 `Path::exists()`（阻塞 stat），因此只适合非 async 调用点；`read` 工具走
/// [`resolve_read_path_async`]。
pub fn resolve_read_path(file_path: &str, cwd: &str) -> PathBuf {
    let resolved = resolve_to_cwd(file_path, cwd);
    if file_exists(&resolved) {
        return resolved;
    }
    resolve_read_variant(&resolved, file_exists)
}

/// `resolveReadPathAsync(filePath, cwd)`：与 [`resolve_read_path`] 同算法，异步检查存在性。
///
/// 存在性检查用 `tokio::fs::metadata`；任一候选报错都视为不存在（与 `fs.access` 的约定一致）。
pub async fn resolve_read_path_async(file_path: &str, cwd: &str) -> PathBuf {
    let resolved = resolve_to_cwd(file_path, cwd);
    if path_exists(&resolved).await {
        return resolved;
    }

    let resolved_text = resolved.to_string_lossy().into_owned();
    let am_pm_variant = try_macos_screenshot_path(&resolved_text);
    if am_pm_variant != resolved_text && path_exists(Path::new(&am_pm_variant)).await {
        return PathBuf::from(am_pm_variant);
    }

    let nfd_variant = try_nfd_variant(&resolved_text);
    if nfd_variant != resolved_text && path_exists(Path::new(&nfd_variant)).await {
        return PathBuf::from(nfd_variant);
    }

    let curly_variant = try_curly_quote_variant(&resolved_text);
    if curly_variant != resolved_text && path_exists(Path::new(&curly_variant)).await {
        return PathBuf::from(curly_variant);
    }

    let nfd_curly_variant = try_curly_quote_variant(&nfd_variant);
    if nfd_curly_variant != resolved_text && path_exists(Path::new(&nfd_curly_variant)).await {
        return PathBuf::from(nfd_curly_variant);
    }

    resolved
}

/// 共享的变体选择逻辑：`check` 是「路径存在」判定（同步或异步的调用点各自包装）。
///
/// 传入的 `resolved` 必须已经过 [`resolve_to_cwd`]（已展开 `~`、去掉 `@`、归一 Unicode
/// 空格）。变体作用于整个路径字符串（包括目录名），与 TS 的实现一致；返回第一个 `check`
/// 为真的候选，全不命中时返回 `resolved` 的克隆。
fn resolve_read_variant(resolved: &Path, check: impl Fn(&Path) -> bool) -> PathBuf {
    let resolved_text = resolved.to_string_lossy().into_owned();

    // 1. macOS AM/PM 变体（U+202F）。
    let am_pm_variant = try_macos_screenshot_path(&resolved_text);
    if am_pm_variant != resolved_text && check(Path::new(&am_pm_variant)) {
        return PathBuf::from(am_pm_variant);
    }

    // 2. NFD 变体。
    let nfd_variant = try_nfd_variant(&resolved_text);
    if nfd_variant != resolved_text && check(Path::new(&nfd_variant)) {
        return PathBuf::from(nfd_variant);
    }

    // 3. 弯引号变体。
    let curly_variant = try_curly_quote_variant(&resolved_text);
    if curly_variant != resolved_text && check(Path::new(&curly_variant)) {
        return PathBuf::from(curly_variant);
    }

    // 4. NFD + 弯引号（法语 macOS 截图 "Capture d'écran"）。
    let nfd_curly_variant = try_curly_quote_variant(&nfd_variant);
    if nfd_curly_variant != resolved_text && check(Path::new(&nfd_curly_variant)) {
        return PathBuf::from(nfd_curly_variant);
    }

    resolved.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screenshot_variant_is_case_insensitive() {
        assert_eq!(
            try_macos_screenshot_path("Screenshot 10.00.00 AM.png"),
            "Screenshot 10.00.00\u{202F}AM.png"
        );
        assert_eq!(
            try_macos_screenshot_path("Screenshot 10.00.00 am.png"),
            "Screenshot 10.00.00\u{202F}am.png"
        );
        // 不是 AM/PM 的空格不动。
        assert_eq!(
            try_macos_screenshot_path("Screenshot 10.00.00.png"),
            "Screenshot 10.00.00.png"
        );
    }

    #[test]
    fn curly_quote_replaces_all_straight_quotes() {
        assert_eq!(
            try_curly_quote_variant("Capture d'ecran d'ici.txt"),
            "Capture d\u{2019}ecran d\u{2019}ici.txt"
        );
    }
}
