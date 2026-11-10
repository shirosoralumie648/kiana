//! utils/paths.ts。
//!
//! 逐字移植 `packages/coding-agent/src/utils/paths.ts`（140 行）。所有函数保持 TS 的
//! **字符串**语义：它们不碰文件系统（除 `canonicalize_path` / `get_file_revision` /
//! `mark_path_ignored_by_cloud_sync`），只做词法规范化。
//!
//! ## 与 TS 的强制偏差
//!
//! [`normalize_path`] / [`resolve_path`] 的 Rust 签名不能失败，而 TS 的
//! `fileURLToPath()` 对无效 `file://` URL 会抛错。Rust 侧在下列情况**原样返回输入字符串**
//! （不 panic），`tests/paths.rs` 按此断言；TS 源码（`test/paths.test.ts`
//! “throws for invalid file URLs”）在这一点上被有意改写：
//!
//! - 非法百分号转义（`file:///%E0%A4%A`，TS 抛 `URIError`）；
//! - 解码后不是合法 UTF-8（`file:///%FF`，TS 抛 `URIError`）；
//! - **路径部分**包含编码过的 `/`（`file:///a%2Fb`，TS 抛 `ERR_INVALID_FILE_URL_PATH`）；
//!   只检查 pathname：query/fragment 里的 `%2F`/非法转义不影响结果（Node 只解码 pathname）；
//! - 非 localhost 主机（`file://host/share/x`，TS 抛 `ERR_INVALID_FILE_URL_HOST`）或其它
//!   Node 会拒绝的主机形式（端口、userinfo、IPv6）。
//!
//! POSIX 分支按 WHATWG file URL 的 pathname 规则逐段处理（见 `whatwg_file_pathname`），
//! 因此保留了 Node 的额外前导 `/`（`file:////x` -> `//x`）与首段盘符的 `|` 归一化
//! （`file:///c|/x` -> `/c:/x`）；`url::Url::to_file_path` 会折叠它们，不能直接用。
//! 实现用 2000+ 条 Node 26 生成的输入对拍过（含 query/fragment、空段、`.`/`..`、
//! `%2e` 编码的 dot-segment、盘符、tab/newline 与首尾空格）。
//!
//! `resolve_path` 对“仍是 `file://` 前缀”的规范化结果同样原样返回（不再拼到 baseDir 上）。
//!
//! ## node:path 的实现位置
//!
//! `ARCHITECTURE.md` §8 第 23 条规定 `node:path` 的唯一实现是
//! `kiana_durable::env::path::{posix,win32}`，但 `kiana-core` 是 L3、不能依赖 `kiana-durable`，
//! 且本文件只需要 `normalize`/`join`/`resolve`/`relative`/`dirname`/`basename` 的
//! 词法部分。这里自带一份最小实现（`node_path_*`，私有），仅供本 crate 使用。
//!
//! ## 平台分支
//!
//! TS 里 `process.platform === "win32"` 的分支在 Rust 里是 `cfg!(windows)`；
//! `normalize_windows_shell_path` 本身是纯字符串函数，可跨平台测试。
//!
//! ## 调用方
//!
//! 本文件是 L3 基础层的纯工具，没有全局可变状态；同一份实现被多处直接调用：
//! `kiana-extensions` 的 loader 用 [`resolve_path`] 解析扩展入口与缓存 cwd，
//! `kiana-resources` 的 skill/package 代码用 [`resolve_path`] / [`canonicalize_path`] 和
//! [`mark_path_ignored_by_cloud_sync`]，`kiana-config` 的认证存储用 [`get_file_revision`]
//! 比较文件修订号以决定是否重新加载，`kiana-session-core` 的会话管理器用 [`join_path`]
//! 做 `node:path.join` 等价拼接；crate 内的 `config.rs` / `shell.rs` 通过文件末尾的
//! `pub(crate)` 包装函数使用同一套 `node:path` 词法实现。
//!
//! ## 阅读顺序
//!
//! 1. [`PathInputOptions`] -> [`normalize_path`] -> [`resolve_path`]：对外主入口；
//! 2. `posix_*` / `win32_*`：`node:path` 的词法实现（两套互不调用，由 `node_*` 系列按
//!    `cfg!(windows)` 分发）；
//! 3. `canonicalize_path` 与 `file://` 解析：涉及符号链接遍历与 WHATWG URL 语义的部分。
//!
//! ## 注意
//!
//! `win32_normalize` 没有为 `\\?\` / `\\.\` 设备前缀单独开分支，这类输入按 UNC 形态
//! 解析；现有测试只锁定了 `\\?\C:\x` 的原样往返。

use std::path::PathBuf;
use std::sync::LazyLock;

use regex::Regex;

/// JS `/[\\u00A0\\u2000-\\u200A\\u202F\\u205F\\u3000]/g` 的字符集。
fn is_unicode_space(c: char) -> bool {
    matches!(
        c,
        '\u{00A0}' | '\u{2000}'..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}'
    )
}

/// JS `/\s/`（`String.prototype.trim` 与 `\s` 正则）的空白集合。
///
/// Rust 的 [`char::is_whitespace`] 缺少 `\u{FEFF}`（ZWNBSP）且集合略有差异，这里逐字对齐 JS。
pub(crate) fn is_js_whitespace(c: char) -> bool {
    matches!(
        c,
        '\u{0009}'..='\u{000D}'
            | '\u{0020}'
            | '\u{00A0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200A}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202F}'
            | '\u{205F}'
            | '\u{3000}'
            | '\u{FEFF}'
    )
}

/// JS `String.prototype.trim()`。
pub(crate) fn js_trim(input: &str) -> &str {
    input.trim_matches(is_js_whitespace)
}

/// JS `/\s/.test(s)`。
pub(crate) fn contains_js_whitespace(s: &str) -> bool {
    s.chars().any(is_js_whitespace)
}

/// `PathInputOptions`：`normalizePath` 的选项。
///
/// `Default::default()` 必须给出 `expand_tilde = true`、其余 `false`（TS 的
/// `options.expandTilde ?? true`）。
///
/// 这些选项只作用于 [`normalize_path`] 的预处理阶段；[`resolve_path`] 对 `base_dir`
/// 一律使用 `PathInputOptions::default()`，不会把调用方的选项传给基准目录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathInputOptions {
    /// 规范化前 trim 首尾空白（JS 的 `trim()`）。默认 `false`（opt-in）。
    pub trim: bool,
    /// 把开头的 `~` 展开为 home 目录。默认 `true`。
    pub expand_tilde: bool,
    /// `~` 展开使用的 home 目录，缺省 `os.homedir()`。
    pub home_dir: Option<PathBuf>,
    /// 去掉开头的 `@`（CLI 的 `@file` 路径）。默认 `false`。
    pub strip_at_prefix: bool,
    /// 把 Unicode 空格变体归一为普通空格。默认 `false`。
    pub normalize_unicode_spaces: bool,
}

impl Default for PathInputOptions {
    fn default() -> Self {
        Self {
            trim: false,
            expand_tilde: true,
            home_dir: None,
            strip_at_prefix: false,
            normalize_unicode_spaces: false,
        }
    }
}

/// `os.homedir()` 的等价物（字符串形式；取不到时为空串，与 `path.join("")` 的语义一致）。
fn home_dir_string() -> String {
    dirs::home_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn cwd_string() -> String {
    std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| ".".to_string())
}

// =============================================================================
// node:path 词法实现（见模块文档）
// =============================================================================

/// `path.sep`。
const fn native_sep() -> char {
    if cfg!(windows) { '\\' } else { '/' }
}

fn is_posix_sep(c: char) -> bool {
    c == '/'
}

fn is_win32_sep(c: char) -> bool {
    c == '/' || c == '\\'
}

/// `path.posix.normalizeString(path, allowAboveRoot)`，不含 `normalize()` 的尾分隔符/`"."` 包装。
///
/// 用栈保存已保留的段：空段与 `.` 丢弃；`..` 弹掉栈顶的普通段（栈顶已是 `..` 或栈空时，
/// 仅在 `allow_above_root` 下保留 `..`）。
fn posix_normalize_string(path: &str, allow_above_root: bool) -> String {
    let mut out: Vec<&str> = Vec::new();
    for segment in path.split(is_posix_sep) {
        match segment {
            "" | "." => {}
            ".." => {
                if let Some(last) = out.last() {
                    if *last != ".." {
                        out.pop();
                    } else if allow_above_root {
                        out.push("..");
                    }
                } else if allow_above_root {
                    out.push("..");
                }
            }
            other => out.push(other),
        }
    }
    out.join("/")
}

/// `path.posix.normalize(path)`。
fn posix_normalize(path: &str) -> String {
    if path.is_empty() {
        return ".".to_string();
    }
    let is_absolute = path.starts_with('/');
    let trailing_separator = path.ends_with('/');
    let mut normalized = posix_normalize_string(path, !is_absolute);
    if normalized.is_empty() {
        // 所有段都被消解：绝对路径退化为根 `/`；相对路径按是否带尾分隔符退化为
        // `./` 或 `.`（Node 的 normalize 不会返回空串）。
        return if is_absolute {
            "/".to_string()
        } else if trailing_separator {
            "./".to_string()
        } else {
            ".".to_string()
        };
    }
    if trailing_separator {
        normalized.push('/');
    }
    if is_absolute {
        format!("/{normalized}")
    } else {
        normalized
    }
}

/// `path.posix.join(...)`。
fn posix_join(parts: &[&str]) -> String {
    let mut joined = String::new();
    for part in parts {
        if part.is_empty() {
            continue;
        }
        if joined.is_empty() {
            joined.push_str(part);
        } else {
            joined.push('/');
            joined.push_str(part);
        }
    }
    if joined.is_empty() {
        return ".".to_string();
    }
    posix_normalize(&joined)
}

/// `path.posix.isAbsolute(path)`。
fn posix_is_absolute(path: &str) -> bool {
    path.starts_with('/')
}

/// `path.posix.resolve(...)`，参数按 TS 的顺序（`resolve(a, b, ...)`）。
fn posix_resolve(parts: &[&str]) -> String {
    let mut resolved = String::new();
    let mut absolute = false;
    for part in parts.iter().rev() {
        if part.is_empty() {
            continue;
        }
        if resolved.is_empty() {
            resolved = (*part).to_string();
        } else {
            resolved = format!("{part}/{resolved}");
        }
        absolute = posix_is_absolute(part);
        if absolute {
            break;
        }
    }
    if !absolute {
        let cwd = cwd_string();
        resolved = if resolved.is_empty() {
            cwd
        } else {
            format!("{cwd}/{resolved}")
        };
        // Node 在这里把 cwd 拼到前面，结果**必然**是绝对路径；若只看最后一个输入参数
        // 会漏掉这一步，导致下面把前导 `/` 丢掉（`resolve("x", "rel/base")` 的回归）。
        absolute = posix_is_absolute(&resolved);
    }
    let normalized = posix_normalize_string(&resolved, false);
    if absolute {
        format!("/{normalized}")
    } else if normalized.is_empty() {
        ".".to_string()
    } else {
        normalized
    }
}

/// `path.posix.relative(from, to)`。
fn posix_relative(from: &str, to: &str) -> String {
    let from = posix_resolve(&[from]);
    let to = posix_resolve(&[to]);
    if from == to {
        return String::new();
    }
    let from_parts: Vec<&str> = from.split('/').filter(|s| !s.is_empty()).collect();
    let to_parts: Vec<&str> = to.split('/').filter(|s| !s.is_empty()).collect();
    // 公共前缀之后：from 的每个剩余段产出一个 `..`，再接上 to 的剩余段；结果为空表示
    // 两路径相同（Node 的 `relative(x, x)` 返回空串）。
    let common = from_parts
        .iter()
        .zip(to_parts.iter())
        .take_while(|(a, b)| a == b)
        .count();
    let mut out: Vec<&str> = Vec::new();
    out.extend(std::iter::repeat_n("..", from_parts.len() - common));
    out.extend_from_slice(&to_parts[common..]);
    out.join("/")
}

/// `path.posix.dirname(path)`。
fn posix_dirname(path: &str) -> String {
    if path.is_empty() {
        return ".".to_string();
    }
    // 去掉尾部多余的分隔符（根除外）。
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return "/".to_string();
    }
    match trimmed.rfind('/') {
        None => ".".to_string(),
        // Node 对 "//a" 返回 "//"（POSIX 的双斜杠根特例，对应 `end === 1`）。
        Some(1) if trimmed.starts_with("//") => "//".to_string(),
        Some(0) => "/".to_string(),
        Some(idx) => trimmed[..idx].to_string(),
    }
}

/// `path.posix.basename(path)`。
fn posix_basename(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return String::new();
    }
    match trimmed.rfind('/') {
        None => trimmed.to_string(),
        Some(idx) => trimmed[idx + 1..].to_string(),
    }
}

/// win32 路径的 root 分解：`(root_len, root_kind)`。
///
/// `root_len` 是 root 在原始字符串中的字节长度（可能包含一个尾分隔符）。
/// 扫描只按 ASCII 字节（`/`、`\`、`:`）推进，所有切片边界都落在 ASCII 字符上，
/// 不会切开多字节 UTF-8 字符。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Win32RootKind {
    /// 无 root（相对路径）。
    None,
    /// 单个分隔符开头的 root。
    Rooted,
    /// `C:` / `C:\`。
    Drive { absolute: bool },
    /// `\\server\share`。
    Unc { server_end: usize, share_end: usize },
}

fn win32_root_kind(path: &str) -> Win32RootKind {
    if path.is_empty() {
        return Win32RootKind::None;
    }
    if is_win32_sep_byte(path, 0) && is_win32_sep_byte(path, 1) {
        // 尝试解析 UNC：两个非空段。
        let mut idx = 2;
        while idx < path.len() && is_win32_sep_byte(path, idx) {
            idx += 1;
        }
        let server_start = idx;
        while idx < path.len() && !is_win32_sep_byte(path, idx) {
            idx += 1;
        }
        let server_end = idx;
        while idx < path.len() && is_win32_sep_byte(path, idx) {
            idx += 1;
        }
        let share_start = idx;
        while idx < path.len() && !is_win32_sep_byte(path, idx) {
            idx += 1;
        }
        let share_end = idx;
        if server_end > server_start && share_end > share_start {
            return Win32RootKind::Unc {
                server_end,
                share_end,
            };
        }
        return Win32RootKind::Rooted;
    }
    if path.len() >= 2
        && is_drive_letter(path.chars().next().unwrap())
        && path.as_bytes()[1] == b':'
    {
        return Win32RootKind::Drive {
            absolute: is_win32_sep_byte(path, 2),
        };
    }
    if is_win32_sep_byte(path, 0) {
        return Win32RootKind::Rooted;
    }
    Win32RootKind::None
}

/// `path.win32.normalize(path)` 的最小实现（见模块文档：UNC 的罕见形态为近似值）。
///
/// 设备前缀（`\\?\`、`\\.\`）没有专门分支，会按 UNC 形态解析，详见模块文档“注意”一节。
fn win32_normalize(path: &str) -> String {
    if path.is_empty() {
        return ".".to_string();
    }
    let root_kind = win32_root_kind(path);
    let (root_norm, root_len, absolute): (String, usize, bool) = match root_kind {
        Win32RootKind::None => (String::new(), 0, false),
        Win32RootKind::Rooted => ("\\".to_string(), 1, true),
        Win32RootKind::Drive { absolute } => {
            let letter = &path[..1];
            if absolute {
                (format!("{letter}:\\"), 3, true)
            } else {
                (format!("{letter}:"), 2, false)
            }
        }
        Win32RootKind::Unc {
            server_end,
            share_end,
        } => {
            let server = &path[2..server_end];
            let share = &path[server_end..share_end].trim_start_matches(is_win32_sep);
            (format!("\\\\{server}\\{share}"), share_end, true)
        }
    };
    let mut rest = &path[root_len..];
    rest = rest.trim_start_matches(is_win32_sep);
    let trailing_separator = rest.ends_with(is_win32_sep);
    let mut out: Vec<&str> = Vec::new();
    for segment in rest.split(is_win32_sep) {
        match segment {
            "" | "." => {}
            ".." => {
                if let Some(last) = out.last() {
                    if *last != ".." {
                        out.pop();
                    } else if !absolute {
                        out.push("..");
                    }
                } else if !absolute {
                    out.push("..");
                }
            }
            other => out.push(other),
        }
    }
    // 盘符相对路径（`C:foo`）在盘符与第一段之间**不**插分隔符。
    let drive_relative = matches!(root_kind, Win32RootKind::Drive { absolute: false });
    let mut result = root_norm;
    if !out.is_empty() {
        if !result.is_empty() && !result.ends_with('\\') && !drive_relative {
            result.push('\\');
        }
        result.push_str(&out.join("\\"));
    }
    if result.is_empty() {
        return ".".to_string();
    }
    if trailing_separator && !out.is_empty() {
        result.push('\\');
    }
    if result.starts_with("\\\\") && out.is_empty() && !result.ends_with('\\') {
        // Node 对 UNC root 本身补一个尾分隔符（`//a\b` -> `\\a\b\`）。
        result.push('\\');
    }
    if result.len() == 2 && result.ends_with(':') {
        // Node 对裸盘符返回 "C:."。
        result.push('.');
    }
    // Node 的 CVE-2024-36139 防护：相对路径且未解析出 device 时，如果某段的冒号会被
    // Windows 当成盘符（首段形如 `X:`，或 `:` 位于末尾/分隔符前），必须加 `.\` 前缀。
    // 例：`./D:\Windows\...` -> `.\D:\Windows\...`（否则会被当作绝对盘符路径）。
    if matches!(root_kind, Win32RootKind::None) && path.contains(':') {
        let looks_like_drive = {
            let bytes = result.as_bytes();
            bytes.len() >= 2
                && is_drive_letter(result.chars().next().unwrap_or(' '))
                && bytes[1] == b':'
        };
        let colon_before_separator_or_end = path
            .match_indices(':')
            .any(|(index, _)| index + 1 == path.len() || is_win32_sep_byte(path, index + 1));
        if looks_like_drive || colon_before_separator_or_end {
            return format!(".\\{result}");
        }
    }
    result
}

/// 按字节判断 `path[idx]` 是否为 win32 分隔符；索引越界返回 `false`，调用点因此不必
/// 先检查长度（例如判断第 2、3 个字节）。
fn is_win32_sep_byte(path: &str, idx: usize) -> bool {
    path.as_bytes()
        .get(idx)
        .map(|b| *b == b'/' || *b == b'\\')
        .unwrap_or(false)
}

/// 是否为盘符字母：只认 ASCII 字母（JS 的 `/[a-z]/i` 只做 ASCII 大小写折叠，U+017F
/// 这类 Unicode 相似字符不算）。同一 ASCII 折叠规则由 `normalize_windows_shell_path`
/// 的正则测试覆盖；该测试不经过本函数，本函数没有非 ASCII 盘符的单独用例。
fn is_drive_letter(c: char) -> bool {
    c.is_ascii_alphabetic()
}

/// `path.win32.join(...)`。
fn win32_join(parts: &[&str]) -> String {
    let mut joined = String::new();
    for part in parts {
        if part.is_empty() {
            continue;
        }
        if joined.is_empty() {
            joined.push_str(part);
        } else {
            joined.push('\\');
            joined.push_str(part);
        }
    }
    if joined.is_empty() {
        return ".".to_string();
    }
    win32_normalize(&joined)
}

/// `path.win32.isAbsolute(path)`。
fn win32_is_absolute(path: &str) -> bool {
    if path.is_empty() {
        return false;
    }
    let bytes = path.as_bytes();
    if bytes[0] == b'/' || bytes[0] == b'\\' {
        return true;
    }
    bytes.len() > 2
        && is_drive_letter(path.chars().next().unwrap())
        && bytes[1] == b':'
        && is_win32_sep_byte(path, 2)
}

/// `path.win32.dirname(path)`。
fn win32_dirname(path: &str) -> String {
    if path.is_empty() {
        return ".".to_string();
    }
    let root_len = match win32_root_kind(path) {
        Win32RootKind::None => 0,
        Win32RootKind::Rooted => 1,
        Win32RootKind::Drive { absolute } => {
            if absolute {
                3
            } else {
                2
            }
        }
        Win32RootKind::Unc { share_end, .. } => {
            // root 含 share 后的那个分隔符（Node 的 dirname 保留它）。
            if share_end < path.len() {
                share_end + 1
            } else {
                share_end
            }
        }
    };
    let trimmed = path.trim_end_matches(is_win32_sep);
    if trimmed.len() <= root_len {
        return path[..root_len.min(path.len())].to_string();
    }
    match trimmed.rfind(is_win32_sep) {
        Some(idx) if idx >= root_len => trimmed[..idx].to_string(),
        Some(_) => path[..root_len.min(path.len())].to_string(),
        None => {
            if root_len > 0 {
                path[..root_len.min(path.len())].to_string()
            } else {
                ".".to_string()
            }
        }
    }
}

/// `path.win32.basename(path)`。
///
/// 逐字复刻 Node：盘符前缀（`C:`）不参与匹配；尾部分隔符被忽略；`C:\` 的 basename 是
/// 空串（不是 `C:`）。
fn win32_basename(path: &str) -> String {
    let bytes = path.as_bytes();
    let start = if bytes.len() >= 2
        && is_drive_letter(path.chars().next().unwrap_or(' '))
        && bytes[1] == b':'
    {
        2
    } else {
        0
    };
    let mut end: Option<usize> = None;
    let mut index = bytes.len();
    while index > start {
        index -= 1;
        if is_win32_sep_byte(path, index) {
            if let Some(end) = end {
                return path[index + 1..end].to_string();
            }
        } else if end.is_none() {
            end = Some(index + 1);
        }
    }
    match end {
        Some(end) => path[start.min(end)..end].to_string(),
        None => String::new(),
    }
}

/// 当前平台的 `path.resolve(...)`。
fn node_resolve(parts: &[&str]) -> String {
    if cfg!(windows) {
        win32_resolve(parts)
    } else {
        posix_resolve(parts)
    }
}

/// `path.win32.resolve(...)`：从最后一个参数往前拼接，遇到绝对段即停止；没有任何绝对段
/// 时在最前面拼上进程 cwd，最后统一 `win32_normalize`。
fn win32_resolve(parts: &[&str]) -> String {
    let mut resolved = String::new();
    let mut absolute = false;
    for part in parts.iter().rev() {
        if part.is_empty() {
            continue;
        }
        if resolved.is_empty() {
            resolved = (*part).to_string();
        } else {
            resolved = format!("{part}\\{resolved}");
        }
        absolute = win32_is_absolute(part);
        if absolute {
            break;
        }
    }
    if !absolute {
        let cwd = cwd_string();
        resolved = if resolved.is_empty() {
            cwd
        } else {
            format!("{cwd}\\{resolved}")
        };
    }
    let normalized = win32_strip_trailing_separator(win32_normalize(&resolved));
    if absolute {
        normalized
    } else if normalized == "." {
        ".".to_string()
    } else {
        normalized
    }
}

/// `path.win32.resolve` 与 `normalize` 的差异：`resolve` 会去掉末尾分隔符，但保留
/// 文件系统根（`C:\`、`\`、`\\server\share\`）。
///
/// 设备路径（`\\?\C:\`）按 Node 的行为也会被去掉末尾分隔符（`\\?\C:`），因此不按
/// UNC 根保留。
fn win32_strip_trailing_separator(path: String) -> String {
    if path.len() <= 1 {
        return path;
    }
    let device = path.starts_with("\\\\?\\") || path.starts_with("\\\\.\\");
    let preserve = !device
        && match win32_root_kind(&path) {
            Win32RootKind::Rooted => path.len() == 1,
            Win32RootKind::Drive { absolute: true } => path.len() == 3,
            Win32RootKind::Unc { share_end, .. } => path.len() == share_end + 1,
            Win32RootKind::Drive { absolute: false } | Win32RootKind::None => false,
        };
    if preserve {
        return path;
    }
    let trimmed = path.trim_end_matches(is_win32_sep);
    if trimmed.is_empty() {
        "\\".to_string()
    } else {
        trimmed.to_string()
    }
}

fn node_relative(from: &str, to: &str) -> String {
    if cfg!(windows) {
        win32_relative(from, to)
    } else {
        posix_relative(from, to)
    }
}

/// `path.win32.relative(from, to)`：比较不区分大小写；两个路径的盘符或 UNC 服务器名
/// （按分隔符拆分后的首段）不同时，Node 返回 `to` 的绝对形式，这里同样直接返回解析
/// 后的 `to`。同一 UNC 服务器、不同 share 不在此列：实现只比较首段，仍按公共段计算
/// 相对路径（`\\srv\share1\a` 与 `\\srv\share2\b` 得到 `..\..\share2\b`）。
fn win32_relative(from: &str, to: &str) -> String {
    let from = win32_resolve(&[from]);
    let to = win32_resolve(&[to]);
    if from.eq_ignore_ascii_case(&to) {
        return String::new();
    }
    let split = |s: &str| -> Vec<String> {
        s.split(is_win32_sep)
            .filter(|p| !p.is_empty())
            .map(|p| p.to_string())
            .collect()
    };
    let from_parts = split(&from);
    let to_parts = split(&to);
    // 不同盘符或不同 UNC 服务器名（拆分后的首段）时 Node 返回绝对路径；同一服务器的
    // 不同 share 仍按公共段计算相对路径。
    let from_root = from_parts.first().cloned().unwrap_or_default();
    let to_root = to_parts.first().cloned().unwrap_or_default();
    if !from_root.eq_ignore_ascii_case(&to_root) {
        return to;
    }
    let common = from_parts
        .iter()
        .zip(to_parts.iter())
        .take_while(|(a, b)| a.eq_ignore_ascii_case(b))
        .count();
    let mut out: Vec<String> = Vec::new();
    for _ in common..from_parts.len() {
        out.push("..".to_string());
    }
    out.extend(to_parts[common..].iter().cloned());
    out.join("\\")
}

fn node_dirname(path: &str) -> String {
    if cfg!(windows) {
        win32_dirname(path)
    } else {
        posix_dirname(path)
    }
}

fn node_basename(path: &str) -> String {
    if cfg!(windows) {
        win32_basename(path)
    } else {
        posix_basename(path)
    }
}

fn node_is_absolute(path: &str) -> bool {
    if cfg!(windows) {
        win32_is_absolute(path)
    } else {
        posix_is_absolute(path)
    }
}

fn node_join(parts: &[&str]) -> String {
    if cfg!(windows) {
        win32_join(parts)
    } else {
        posix_join(parts)
    }
}

// =============================================================================
// 公开 API
// =============================================================================

// 整串锚定：`^/` + 可选的 `/mnt/` 或 `/cygdrive/` 前缀 + 单个盘符字母 + 可选 `/<路径>`。
// 组 1 = 盘符字母，组 2 = 盘符后的路径（不含前导 `/`）。
static WINDOWS_SHELL_PATH_RE: LazyLock<Regex> = LazyLock::new(|| {
    // JS 的 `/i` 只做 ASCII 大小写折叠（`/[a-z]/i` 不匹配 `ſ`/`K`），Rust 的 `(?i)` 是
    // Unicode 折叠，因此逐字母写出 `[Mm]` 形式，不启用 `(?i)`。
    // JS 的 `.`（不带 s 标志）不匹配 LineTerminator：`\n`、`\r`、U+2028、U+2029；
    // Rust regex 的 `.` 只排除 `\n`，因此这里显式写出字符类。
    Regex::new(
        r"^/(?:[Mm][Nn][Tt]/|[Cc][Yy][Gg][Dd][Rr][Ii][Vv][Ee]/)?([A-Za-z])(?:/([^\n\r\u{2028}\u{2029}]*))?$",
    )
    .expect("valid windows shell path regex")
});

/// 把 Git Bash / MSYS / Cygwin / WSL 的盘符路径转成原生 Windows API 接受的形式。
///
/// 接受 `/c`、`/c/...`、`/mnt/c/...`、`/cygdrive/c/...` 四种形态：`MNT`/`CYGDRIVE`
/// 前缀与盘符只做 ASCII 大小写折叠，盘符输出为大写，`/` 全部换成 `\`；没有后续路径时
/// 输出 `C:\`。不匹配时**原样返回**，包括不以 `/` 开头、以 `//` 开头（UNC 形式）、
/// 含 `\`、以及路径中出现 `\n`/`\r`/U+2028/U+2029 的输入（JS 正则的 `.` 不匹配这些
/// 行终止符，见本文件测试）。
///
/// 纯字符串函数，与宿主平台无关；[`normalize_path`] 只在 `cfg!(windows)` 时调用它。
pub fn normalize_windows_shell_path(file_path: &str) -> String {
    if !file_path.starts_with('/') || file_path.starts_with("//") || file_path.contains('\\') {
        return file_path.to_string();
    }
    let Some(captures) = WINDOWS_SHELL_PATH_RE.captures(file_path) else {
        return file_path.to_string();
    };
    let drive = captures.get(1).map(|m| m.as_str()).unwrap_or_default();
    let suffix = captures.get(2).map(|m| m.as_str().replace('/', "\\"));
    format!("{}:\\{}", drive.to_uppercase(), suffix.unwrap_or_default())
}

/// 把路径解析为真实路径（跟随符号链接）。失败时回退到原始路径。
///
/// 复刻 Node 的 `fs.realpathSync`（**JS 实现**，不是 `realpathSync.native`）：
/// 1. 先 `path.resolve(path)` 做词法归一（`""` -> 进程 cwd，去掉尾部分隔符与 `.`，
///    按词法消解 `..`，因此 `missing/../x` 在 `missing` 不存在时仍能解析）；
/// 2. 再从根开始逐段 `lstat`，遇到符号链接就 `readlink` 并把目标拼回剩余路径、从根重新开始。
///
/// `std::fs::canonicalize` 是 `realpath(3)` 语义：它要求路径上每一段都存在，且文件后的
/// 尾随 `/` 会 ENOTDIR 失败。两者在上述输入上可观察不同，故不用它。
///
/// 空串按 `path.resolve("")` 解析为进程当前目录；符号链接展开轮数用尽（例如链接环）也
/// 算失败。注意失败时返回的是**未做词法规范化的原始入参**，调用方不能把返回值当作
/// “路径存在”的证据。
pub fn canonicalize_path(path: &str) -> String {
    match node_realpath(path) {
        Some(resolved) => resolved,
        None => path.to_string(),
    }
}

/// Node JS 版 `realpathSync` 的展开次数上限（符号链接环在 Node 里抛 `ELOOP`，
/// `canonicalizePath` 捕获后返回原串）。
const MAX_REALPATH_LINK_EXPANSIONS: usize = 40;

/// Node JS 版 `realpathSync` 的迭代骨架：每轮最多展开**一个**符号链接，然后从解析后的
/// 路径重新开始走。
///
/// 返回 `Some(path)` 表示成功走完；`None` 表示 `lstat`/`readlink` 失败或展开轮数用尽
/// （`0..=MAX` 允许 MAX+1 轮，对应 Node 的 `ELOOP`），由调用方回退原始字符串。
fn node_realpath(input: &str) -> Option<String> {
    let mut current = path_resolve(&[input]);
    let mut known_hard: std::collections::HashSet<String> = std::collections::HashSet::new();
    for _ in 0..=MAX_REALPATH_LINK_EXPANSIONS {
        match expand_realpath_links(&current, &mut known_hard) {
            Some(Some(next)) => current = next,
            Some(None) => return Some(current),
            None => return None,
        }
    }
    None
}

/// 走一遍 `path` 的分量。`Some(Some(next))` 表示遇到符号链接、需要从 `next` 重新开始；
/// `Some(None)` 表示走完；`None` 表示 `lstat`/`readlink` 失败（Node 抛错 → 回退原串）。
///
/// `known_hard` 缓存此前轮次已确认**不是**符号链接的前缀：再次走到这些路径时直接跳过
/// `symlink_metadata`，避免每展开一个链接就把整条前缀重新 lstat 一遍。
fn expand_realpath_links(
    path: &str,
    known_hard: &mut std::collections::HashSet<String>,
) -> Option<Option<String>> {
    let (root, rest_start) = realpath_root(path);
    let mut prefix = root;
    let parts: Vec<&str> = path[rest_start..]
        .split(is_native_sep_char)
        .filter(|part| !part.is_empty())
        .collect();
    for (index, part) in parts.iter().enumerate() {
        let base = if prefix.ends_with(is_native_sep_char) {
            format!("{prefix}{part}")
        } else {
            format!("{prefix}{}{part}", native_sep())
        };
        if known_hard.contains(&base) {
            prefix = base;
            continue;
        }
        let metadata = std::fs::symlink_metadata(&base).ok()?;
        if !metadata.file_type().is_symlink() {
            known_hard.insert(base.clone());
            prefix = base;
            continue;
        }
        let target = std::fs::read_link(&base).ok()?;
        let target = target.to_string_lossy();
        let resolved_link = path_resolve(&[&prefix, &target]);
        let remainder = parts[index + 1..].join(&native_sep().to_string());
        return Some(Some(path_resolve(&[&resolved_link, &remainder])));
    }
    Some(None)
}

/// `path` 的 root（与 Node `splitRoot` 一致）及主体在字符串中的起始位置。
fn realpath_root(path: &str) -> (String, usize) {
    if cfg!(windows) {
        let (root, len) = match win32_root_kind(path) {
            Win32RootKind::None => (String::new(), 0),
            Win32RootKind::Rooted => ("\\".to_string(), 1),
            Win32RootKind::Drive { absolute: true } => (path[..3].to_string(), 3),
            Win32RootKind::Drive { absolute: false } => (path[..2].to_string(), 2),
            Win32RootKind::Unc { share_end, .. } => {
                let mut end = share_end;
                if end < path.len() && is_win32_sep_byte(path, end) {
                    end += 1;
                }
                (path[..end].to_string(), end)
            }
        };
        (root, len)
    } else {
        // `path_resolve` 在 POSIX 上总是返回绝对路径（cwd 不可用时防御性退化为相对路径）。
        if path.starts_with('/') {
            ("/".to_string(), 1)
        } else {
            (String::new(), 0)
        }
    }
}

fn is_native_sep_char(c: char) -> bool {
    if cfg!(windows) {
        is_win32_sep(c)
    } else {
        is_posix_sep(c)
    }
}

/// 文件的“修订号”：`${dev}:${ino}:${size}:${mtimeNs}:${ctimeNs}`；读取失败返回 `None`。
///
/// 用 `std::fs::metadata`（跟随符号链接）读取一次元数据。Unix 上五段依次是设备号、
/// inode、字节大小、mtime 纳秒、ctime 纳秒；Windows 分支没有使用 dev/ino（稳定版
/// `std::fs::Metadata` 不直接提供，需要打开句柄用 `GetFileInformationByHandle` 取卷
/// 序列号 + 文件索引），按 §8 第 26 条前两段固定为 `0`，时间用 FILETIME（100ns 单位）
/// 乘 100 换算成纳秒；其它平台退化为 `0:0:{len}:0:0`。
///
/// 调用方（如 `kiana-config` 的认证存储）只把这个字符串当作不透明的“文件是否变化”
/// 令牌做相等比较，不解析各段；各段的含义按平台不同（见上），不能跨平台解释。
pub fn get_file_revision(path: &str) -> Option<String> {
    let metadata = std::fs::metadata(path).ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let mtime_ns =
            i128::from(metadata.mtime()) * 1_000_000_000 + i128::from(metadata.mtime_nsec());
        let ctime_ns =
            i128::from(metadata.ctime()) * 1_000_000_000 + i128::from(metadata.ctime_nsec());
        Some(format!(
            "{}:{}:{}:{}:{}",
            metadata.dev(),
            metadata.ino(),
            metadata.size(),
            mtime_ns,
            ctime_ns
        ))
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        // Windows 分支不用 dev/ino（稳定版 std::fs::Metadata 不直接提供），按 §8 第 26
        // 条记 0；FILETIME 是 100ns 单位。
        Some(format!(
            "0:0:{}:{}:{}",
            metadata.file_size(),
            i128::from(metadata.last_write_time()) * 100,
            i128::from(metadata.creation_time()) * 100
        ))
    }
    #[cfg(not(any(unix, windows)))]
    {
        Some(format!("0:0:{}:0:0", metadata.len()))
    }
}

/// 是否为本地路径（非 `npm:`/`git:`/`github:`/`http:`/`https:`/`ssh:`/`builtin:`）。
///
/// 判断前先按 JS `trim()` 去掉首尾空白（因此 `"  ./foo  "` 算本地路径）；前缀匹配
/// 大小写敏感（`NPM:foo` 仍算本地），空串也算本地。只做前缀判断，不解析 URL、
/// 不访问文件系统。
pub fn is_local_path(value: &str) -> bool {
    let trimmed = js_trim(value);
    !(trimmed.starts_with("npm:")
        || trimmed.starts_with("git:")
        || trimmed.starts_with("github:")
        || trimmed.starts_with("http:")
        || trimmed.starts_with("https:")
        || trimmed.starts_with("ssh:")
        || trimmed.starts_with("builtin:"))
}

/// `file://` URL → 本地路径。解析失败时**原样返回输入**（见模块文档的强制偏差）。
///
/// Node 的 `fileURLToPath` 会抛错的四种情况在这里都退化为“原样返回”：
/// 1. 非法百分号转义（`decodeURIComponent` 抛 `URIError`，例如 `file:///%E0%A4%A`）；
/// 2. 路径包含编码过的 `/`（`%2f`，`ERR_INVALID_FILE_URL_PATH`）；
/// 3. 解码后不是合法 UTF-8（`URIError`，例如 `file:///%FF`）；
/// 4. 非 localhost 主机（`ERR_INVALID_FILE_URL_HOST`）。
///
/// 只检查/解码 **pathname**：`file:///tmp/x?q=%zz`、`file:///tmp/x#a=%2F` 之类
/// 的 query/fragment 不参与判断（Node 的 `fileURLToPath` 只看 pathname）。
fn file_url_to_path(url: &str) -> String {
    if cfg!(windows) {
        file_url_to_path_windows(url)
    } else {
        file_url_to_path_posix(url)
    }
}

/// POSIX 分支：WHATWG file URL 的 pathname 规范化 + `decodeURIComponent`。
fn file_url_to_path_posix(url: &str) -> String {
    let original = url;
    // WHATWG URL 解析先去掉首尾的 C0 控制符/空格，并删除任意位置的 ASCII tab/newline。
    // （首部空格会让 normalize_path 不把它当 file:// URL，因此这里只需处理尾部。）
    let trimmed = url.trim_end_matches(|c: char| c <= ' ');
    let Some(rest) = trimmed.strip_prefix("file://") else {
        return original.to_string();
    };
    let cleaned: String = rest
        .chars()
        .filter(|c| !matches!(c, '\t' | '\n' | '\r'))
        .collect();
    // `?`/`#` 之后是 query/fragment；Node 只对 pathname 做转义检查与解码。
    let rest = cleaned.split(['?', '#']).next().unwrap_or("");
    let Some(path_input) = file_url_path_input(rest) else {
        return original.to_string();
    };
    let pathname = whatwg_file_pathname(&path_input);
    if !has_valid_percent_escapes(&pathname) || contains_encoded_slash(&pathname) {
        return original.to_string();
    }
    match percent_encoding::percent_decode_str(&pathname).decode_utf8() {
        Ok(decoded) => decoded.into_owned(),
        Err(_) => original.to_string(),
    }
}

/// Windows 分支：沿用 `url::Url::to_file_path`（Windows 路径规则不在验收矩阵，§8 第 26 条）。
fn file_url_to_path_windows(url: &str) -> String {
    let Ok(parsed) = url::Url::parse(url) else {
        return url.to_string();
    };
    let pathname = parsed.path();
    if !has_valid_percent_escapes(pathname)
        || contains_encoded_slash(pathname)
        || (cfg!(windows) && contains_encoded_backslash(pathname))
    {
        return url.to_string();
    }
    match parsed.to_file_path() {
        Ok(path) => match path.to_str() {
            Some(path) => path.to_string(),
            None => url.to_string(),
        },
        Err(()) => url.to_string(),
    }
}

/// 从 `file://` 之后的文本里取出路径部分；非 localhost 主机返回 `None`（TS 抛错）。
///
/// WHATWG 规则：host 是第一个 `/`/`\` 之前的部分；空 host 时整段都是路径；形如
/// `C:`/`C|` 的 host 会被当作盘符路径；`localhost` host 被忽略。
fn file_url_path_input(rest: &str) -> Option<String> {
    let host_end = rest.find(['/', '\\']).unwrap_or(rest.len());
    let first_segment = &rest[..host_end];
    if first_segment.is_empty() {
        return Some(rest.to_string());
    }
    if is_windows_drive_letter(first_segment) {
        return Some(rest.to_string());
    }
    if first_segment.eq_ignore_ascii_case("localhost") {
        return Some(rest[host_end..].to_string());
    }
    None
}

/// WHATWG file URL 的 pathname（POSIX）：逐段处理 `.`/`..`/盘符并保留空段。
///
/// `input` 是 host 之后的原始路径文本（空 host 时以 `/` 开头；盘符 host 时不带前导 `/`）。
/// 返回值与 Node `new URL(...).pathname` 相同（百分号转义原样保留）。
/// `url::Url::to_file_path` 会把前导空段折叠成一段，与 Node 不一致，所以这里自己实现。
fn whatwg_file_pathname(input: &str) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut path = String::new();
    let mut index = 0usize;
    // 逐段消费：每轮把从当前位置到下一个 `/`/`\` 的文本写进 `path`，分隔符统一成 `/`，
    // 然后只对刚写入的这一段执行 WHATWG 的 dot-segment 归一化。空段不丢弃，因此
    // `file:////x` 会保留额外的前导 `/`。
    loop {
        let segment_start = path.len();
        let mut buffer = String::new();
        let mut ended_with_separator = false;
        while index < chars.len() {
            let c = chars[index];
            index += 1;
            if c == '/' || c == '\\' {
                path.push_str(&buffer);
                buffer.clear();
                path.push('/');
                ended_with_separator = true;
                break;
            }
            buffer.push(c);
        }
        if !ended_with_separator {
            path.push_str(&buffer);
        }
        let segment = if ended_with_separator {
            path[segment_start..path.len() - 1].to_string()
        } else {
            path[segment_start..].to_string()
        };
        match segment.as_str() {
            ".." | "%2e%2e" | "%2e%2E" | "%2E%2e" | "%2E%2E" | "%2e." | "%2E." | ".%2e"
            | ".%2E" => {
                // `..`：先丢掉当前段（truncate），再按 shorten-a-urls-path 弹掉上一段；
                // 若剩余路径只是一个规范化盘符段（如 `/c:`），上一段受保护不弹。
                path.truncate(segment_start);
                if path.ends_with('/') && last_slash_can_be_removed(&path) {
                    path.pop();
                }
                shorten_file_path(&mut path);
                if ended_with_separator && !path.ends_with('/') {
                    path.push('/');
                }
            }
            "." | "%2e" | "%2E" => {
                // `.`：丢弃段文本即可；若末尾没有分隔符则补一个，保持段边界。
                path.truncate(segment_start);
                if !path.ends_with('/') {
                    path.push('/');
                }
            }
            _ => {
                // `file:///c|/x`：URL 规范把首段（path 还是空的时候）的 `c|` 归一为 `c:`。
                // 当 path 已有多余前导空段时（`file:////c|/x`）不做归一。
                let prefix_before_segment = &path[..segment_start];
                let is_first_segment = segment_start == 0 || prefix_before_segment == "/";
                if is_first_segment && is_windows_drive_letter(&segment) {
                    let letter = segment.chars().next().unwrap_or('c');
                    path.truncate(segment_start);
                    path.push(letter);
                    path.push(':');
                    if ended_with_separator {
                        path.push('/');
                    }
                }
            }
        }
        if !ended_with_separator {
            break;
        }
    }
    if path.is_empty() {
        // special scheme 的 path 永不为空：`file://` -> `/`。
        "/".to_string()
    } else if path.starts_with('/') {
        path
    } else {
        // 盘符 host 形式（`file://C:/x`）的 pathname 以 `/` 开头。
        format!("/{path}")
    }
}

/// `..` 处理时，path 末尾的 `/` 是否可以先删掉。
///
/// WHATWG 只保护“整条路径只有一个规范化盘符段”（`/c:` 或 `c:`）的情况；url crate 的
/// `path_starts_with_windows_drive_letter` 会把任何盘符段都保护起来，导致
/// `file:////c:/..`（Node -> `//`）这类带额外前导空段的路径少 pop 一段。
fn last_slash_can_be_removed(path: &str) -> bool {
    let before = &path[..path.len().saturating_sub(1)];
    let without_root = before.strip_prefix('/').unwrap_or(before);
    !is_normalized_windows_drive_letter(without_root)
}

/// https://url.spec.whatwg.org/#shorten-a-urls-path
fn shorten_file_path(path: &mut String) {
    if path.is_empty() {
        return;
    }
    // path 只有一段且该段是规范化盘符时受保护。
    let without_root = path.strip_prefix('/').unwrap_or(path);
    if is_normalized_windows_drive_letter(without_root) {
        return;
    }
    pop_file_path(path);
}

/// https://url.spec.whatwg.org/#pop-a-urls-path
fn pop_file_path(path: &mut String) {
    if path.is_empty() {
        return;
    }
    match path.rfind('/') {
        Some(position) => {
            let segment_start = position + 1;
            if position == 0 && is_normalized_windows_drive_letter(&path[segment_start..]) {
                return;
            }
            path.truncate(segment_start);
        }
        None => path.clear(),
    }
}

// WHATWG 的 “Windows drive letter” 判定：`X:` 或 `X|`，后面必须是字符串结尾或
// `/`、`\`、`?`、`#` 之一。`is_windows_drive_letter` 要求恰好两个字符；
// `is_normalized_windows_drive_letter` 额外要求第二个字符是 `:`。
fn starts_with_windows_drive_letter(s: &str) -> bool {
    let bytes = s.as_bytes();
    s.len() >= 2
        && bytes[0].is_ascii_alphabetic()
        && matches!(bytes[1], b':' | b'|')
        && (s.len() == 2 || matches!(bytes[2], b'/' | b'\\' | b'?' | b'#'))
}

fn is_windows_drive_letter(s: &str) -> bool {
    s.len() == 2 && starts_with_windows_drive_letter(s)
}

fn is_normalized_windows_drive_letter(s: &str) -> bool {
    is_windows_drive_letter(s) && s.as_bytes()[1] == b':'
}

/// 检查每个 `%` 后面是否恰好是两个 ASCII 十六进制数字；不成立即视为非法转义。
fn has_valid_percent_escapes(url: &str) -> bool {
    let bytes = url.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let valid = index + 2 < bytes.len()
                && bytes[index + 1].is_ascii_hexdigit()
                && bytes[index + 2].is_ascii_hexdigit();
            if !valid {
                return false;
            }
            index += 3;
            continue;
        }
        index += 1;
    }
    true
}

fn contains_encoded_slash(url: &str) -> bool {
    contains_encoded_byte(url, b'/')
}

fn contains_encoded_backslash(url: &str) -> bool {
    contains_encoded_byte(url, b'\\')
}

/// 检测 `%XX` 形式的编码字节（`byte` 的两位十六进制表示，大小写都命中：
/// `%5C` 与 `%5c` 等价）。
///
/// WHATWG/Node 的 `fileURLToPath` 只关心编码的 `/`（`%2F`）与 `\`（`%5C`）；
/// 早期实现把第二个十六进制位硬编码成 `2`，导致只认 `%2F` 而永远匹配不到 `%5C`。
fn contains_encoded_byte(url: &str, byte: u8) -> bool {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let high = HEX[(byte >> 4) as usize];
    let low = HEX[(byte & 0x0f) as usize];
    let bytes = url.as_bytes();
    let mut index = 0;
    while index + 2 < bytes.len() {
        if bytes[index] == b'%'
            && bytes[index + 1].eq_ignore_ascii_case(&high)
            && bytes[index + 2].eq_ignore_ascii_case(&low)
        {
            return true;
        }
        index += 1;
    }
    false
}

/// 规范化路径输入（trim → unicode 空格 → 去 `@` → win32 shell 路径 → `~` 展开 → `file://`）。
///
/// 各步是否执行由 `options` 决定，顺序固定：
/// 1. `trim`：JS `String.prototype.trim()`（空白集合包含 BOM，见 `is_js_whitespace`）；
/// 2. `normalize_unicode_spaces`：把 U+00A0、U+2000-U+200A、U+202F、U+205F、U+3000
///    替换成普通空格（注意不含 U+1680 / U+FEFF）；
/// 3. `strip_at_prefix`：只去掉**一个**开头的 `@`；
/// 4. Windows 上（`cfg!(windows)`）先做 [`normalize_windows_shell_path`]；
/// 5. `expand_tilde`：只有 `~`、`~/...`（Windows 还有 `~\...`）会被展开，`~user`
///    等保持原样；home 取 `options.home_dir`，缺省 `dirs::home_dir()`，都取不到时为空串；
/// 6. 仍以 `file://` 开头的交给 `file_url_to_path`（失败返回见模块文档的强制偏差）。
///
/// 函数不会失败、不访问文件系统；无法解析的 `file://` 输入原样返回。
pub fn normalize_path(input: &str, options: &PathInputOptions) -> String {
    let mut normalized = if options.trim {
        js_trim(input).to_string()
    } else {
        input.to_string()
    };
    if options.normalize_unicode_spaces {
        normalized = normalized
            .chars()
            .map(|c| if is_unicode_space(c) { ' ' } else { c })
            .collect();
    }
    if options.strip_at_prefix
        && let Some(rest) = normalized.strip_prefix('@')
    {
        normalized = rest.to_string();
    }
    if cfg!(windows) {
        normalized = normalize_windows_shell_path(&normalized);
    }

    if options.expand_tilde {
        let home = match &options.home_dir {
            Some(dir) => dir.to_string_lossy().into_owned(),
            None => home_dir_string(),
        };
        if normalized == "~" {
            return home;
        }
        let tilde_slash = normalized.starts_with("~/");
        let tilde_backslash = cfg!(windows) && normalized.starts_with("~\\");
        if tilde_slash || tilde_backslash {
            return node_join(&[&home, &normalized[2..]]);
        }
    }

    if normalized.starts_with("file://") {
        return file_url_to_path(&normalized);
    }

    normalized
}

/// 解析路径：先 [`normalize_path`] 输入与基准目录，再按词法解析（`path.resolve`）。
///
/// - `input` 用调用方传入的 `options` 规范化，`base_dir` 固定用 `PathInputOptions::default()`；
/// - `input` 规范化后仍以 `file://` 开头（只可能是无法解析的 URL）时直接返回它，
///   不会拼到 `base_dir` 上；
/// - 绝对路径忽略 `base_dir`；相对路径与空串按 `base_dir` 拼接后再做 `path.resolve`；
/// - `base_dir` 先经 [`normalize_path`] 的默认选项处理：会展开开头的 `~`、把 `file://`
///   转为本地路径、在 Windows 上转换 Git Bash/MSYS/Cygwin/WSL 的盘符形式；处理结果仍是
///   相对路径或空串时，结果以进程当前目录为基准（`path.resolve` 的行为），因此返回值
///   通常是绝对路径，cwd 不可用时才会退化为相对路径；
/// - 纯词法运算，不检查路径是否存在。
pub fn resolve_path(input: &str, base_dir: &str, options: &PathInputOptions) -> String {
    let normalized = normalize_path(input, options);
    // 无效 `file://` URL 在 `normalize_path` 里原样返回；这里继续原样返回，
    // 不把它当成相对路径拼到 baseDir 上（TS 在这个输入上抛错）。
    if normalized.starts_with("file://") {
        return normalized;
    }
    let normalized_base_dir = normalize_path(base_dir, &PathInputOptions::default());
    if node_is_absolute(&normalized) {
        node_resolve(&[&normalized])
    } else {
        node_resolve(&[&normalized_base_dir, &normalized])
    }
}

/// 若 `file_path` 在 `cwd` 内，返回相对路径（相同时为 `"."`），否则 `None`。
///
/// 两个参数都先经 [`resolve_path`] 解析（`cwd` 以进程当前目录为基准），再做词法
/// `path.relative`；不访问文件系统。判断“在内”时只排除 `..`、`../...` 这类越界相对
/// 路径，以及绝对路径（Windows 上盘符或 UNC 服务器不同时，`path.relative` 直接返回
/// `to` 的绝对形式），因此 `..config/AGENTS.md` 这类名字仍算在 `cwd` 内（当前契约，
/// 见单元测试）。
pub fn get_cwd_relative_path(file_path: &str, cwd: &str) -> Option<String> {
    let cwd_now = cwd_string();
    let resolved_cwd = resolve_path(cwd, &cwd_now, &PathInputOptions::default());
    let resolved_path = resolve_path(file_path, &resolved_cwd, &PathInputOptions::default());
    let relative_path = node_relative(&resolved_cwd, &resolved_path);
    // “在内”= 相对路径不为 `..`、不以 `../` 开头，且不是（Windows 跨盘符或 UNC
    // 服务器不同时 `win32_relative` 返回的）绝对路径。
    let is_inside_cwd = relative_path.is_empty()
        || (relative_path != ".."
            && !relative_path.starts_with(&format!("..{}", native_sep()))
            && !node_is_absolute(&relative_path));
    if !is_inside_cwd {
        return None;
    }
    Some(if relative_path.is_empty() {
        ".".to_string()
    } else {
        relative_path
    })
}

/// 相对 `cwd` 的路径（不在 `cwd` 内时为绝对路径），并把平台分隔符统一成 `/`。
///
/// 先用 [`resolve_path`] 解析 `file_path`，再用 [`get_cwd_relative_path`] 尝试转成
/// 相对形式；不在 `cwd` 内时保留解析后的路径。Windows 上把返回值里的 `\` 换成 `/`
/// 仅用于展示（日志/UI），不改变路径本身的解析结果。
pub fn format_path_relative_to_cwd_or_absolute(file_path: &str, cwd: &str) -> String {
    let absolute_path = resolve_path(file_path, cwd, &PathInputOptions::default());
    let display = get_cwd_relative_path(&absolute_path, cwd).unwrap_or(absolute_path);
    if cfg!(windows) {
        display.replace('\\', "/")
    } else {
        display
    }
}

/// 标记路径不被云同步（Dropbox / iCloud）同步。
///
/// macOS：`xattr -w com.dropbox.ignored 1` + `xattr -w com.apple.fileprovider.ignore#P 1`；
/// Linux：`setfattr -n user.com.dropbox.ignored -v 1`；其它平台空操作。
/// `spawn_process_sync` 的失败被静默（`stdio: "ignore"`）。
///
/// 每个属性值都通过一次**外部命令**设置；macOS 调 `xattr -w <attr> 1 <path>`，Linux 调
/// `setfattr -n <attr> -v 1 <path>`，其它平台属性列表为空、函数立即返回。stdio 设为
/// `Ignore`，返回的 `SpawnSyncResult` 也被 `let _ =` 丢弃，因此退出码与输出都不检查，
/// 设置失败不会返回错误、调用方也无法感知；它只是一次尽力而为的标记。
pub fn mark_path_ignored_by_cloud_sync(path: &str) {
    #[cfg(target_os = "macos")]
    let attrs: &[&str] = &["com.dropbox.ignored", "com.apple.fileprovider.ignore#P"];
    #[cfg(target_os = "linux")]
    let attrs: &[&str] = &["user.com.dropbox.ignored"];
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let attrs: &[&str] = &[];

    let options = crate::child_process::SpawnSyncOptions::ignored();
    for attr in attrs {
        if cfg!(target_os = "macos") {
            let _ = crate::child_process::spawn_process_sync(
                "xattr",
                &[
                    "-w".to_string(),
                    (*attr).to_string(),
                    "1".to_string(),
                    path.to_string(),
                ],
                &options,
            );
        } else {
            let _ = crate::child_process::spawn_process_sync(
                "setfattr",
                &[
                    "-n".to_string(),
                    (*attr).to_string(),
                    "-v".to_string(),
                    "1".to_string(),
                    path.to_string(),
                ],
                &options,
            );
        }
    }
}

// 供 crate 内部使用（config.rs 的路径拆分需要同样的 word 语义）。
/// `path.dirname`（Node 的当前平台语义）。
pub(crate) fn path_dirname(path: &str) -> String {
    node_dirname(path)
}

/// `path.basename`（Node 的当前平台语义）。
pub(crate) fn path_basename(path: &str) -> String {
    node_basename(path)
}

/// `path.join`（Node 的当前平台语义）。
pub(crate) fn path_join(parts: &[&str]) -> String {
    node_join(parts)
}

/// `path.join(parts...)`（Node 的平台语义：多段拼接、词法归一化去掉 `.` 段并消解
/// 部分 `..`、后续绝对路径段不重置结果、全空 → `"."`）。
///
/// 供其它需要统一 `path.join` 语义的 crate 使用（例如会话文件命名需要跨平台一致的路径文本）。
pub fn join_path(parts: &[&str]) -> String {
    node_join(parts)
}

/// `path.resolve`（Node 的当前平台语义，结果以进程 cwd 为基准）。
pub(crate) fn path_resolve(parts: &[&str]) -> String {
    node_resolve(parts)
}

/// `path.sep`：当前平台的分隔符字符。
pub(crate) fn path_sep() -> char {
    native_sep()
}

// ---- 供 shell.rs 使用的 win32 语义（无论宿主平台） ----

/// 强制使用 win32 词法（与 `cfg!(windows)` 无关），供 `shell.rs` 在 Linux 测试里也
/// 复现 Windows 的路径归一化结果。
pub(crate) fn win32_normalize_for_shell(path: &str) -> String {
    win32_normalize(path)
}

/// 强制使用 win32 语义的 `path.join`。
pub(crate) fn win32_join_for_shell(parts: &[&str]) -> String {
    win32_join(parts)
}

/// 强制使用 win32 语义的 `path.dirname`。
pub(crate) fn win32_dirname_for_shell(path: &str) -> String {
    win32_dirname(path)
}

/// 强制使用 win32 语义的 `path.basename`。
pub(crate) fn win32_basename_for_shell(path: &str) -> String {
    win32_basename(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoded_separator_detection_covers_backslash() {
        assert!(contains_encoded_slash("%2F"));
        assert!(contains_encoded_slash("%2f"));
        assert!(!contains_encoded_slash("%5C"));
        assert!(contains_encoded_backslash("%5C"));
        assert!(contains_encoded_backslash("%5c"));
        assert!(!contains_encoded_backslash("%2c"));
        assert!(!contains_encoded_backslash("%2C"));
    }

    // 黄金值由 node 26 的 `path.posix.*` / `path.win32.*` 生成（见单元报告）。
    #[test]
    fn posix_normalize_matches_node() {
        let cases = [
            ("C:/a/./b/../c/", "C:/a/c/"),
            ("//server/share//x", "/server/share/x"),
            ("a/../..", ".."),
            ("", "."),
            ("/", "/"),
            ("/a//b/./c/", "/a/b/c/"),
            ("./", "./"),
        ];
        for (input, expected) in cases {
            assert_eq!(posix_normalize(input), expected, "normalize({input:?})");
        }
    }

    #[test]
    fn posix_resolve_matches_node() {
        let cases = [
            (vec!["/a/b", "c/d"], "/a/b/c/d"),
            (vec!["/a/b", "../c"], "/a/c"),
            (vec!["/a/b", "/x/y"], "/x/y"),
            (vec!["/a/b", "."], "/a/b"),
            (vec!["/a/b", ".."], "/a"),
            (vec!["/a/b/", "c/"], "/a/b/c"),
            (vec!["/a//b", "./c"], "/a/b/c"),
        ];
        for (args, expected) in cases {
            assert_eq!(posix_resolve(&args), expected, "resolve({args:?})");
        }
    }

    #[test]
    fn posix_relative_matches_node() {
        let cases = [
            ("/a/b", "/a/b/c", "c"),
            ("/a/b", "/a", ".."),
            ("/a", "/a/b", "b"),
            ("/", "/a", "a"),
            ("/a/", "/a/b/", "b"),
            ("/a/b", "/a/b", ""),
            ("/a/b", "/x/y", "../../x/y"),
            ("/a/b/c", "/a/d", "../../d"),
            ("/a", "/ab", "../ab"),
        ];
        for (from, to, expected) in cases {
            assert_eq!(
                posix_relative(from, to),
                expected,
                "relative({from:?}, {to:?})"
            );
        }
    }

    #[test]
    fn posix_dirname_basename_match_node() {
        let dirname_cases = [
            ("", "."),
            (".", "."),
            ("/", "/"),
            ("/a", "/"),
            ("/a/b", "/a"),
            ("/a/b/", "/a"),
            ("a", "."),
            ("a/b", "a"),
            ("//a", "//"),
            ("/a/../b", "/a/.."),
        ];
        for (input, expected) in dirname_cases {
            assert_eq!(posix_dirname(input), expected, "dirname({input:?})");
        }
        let basename_cases = [
            ("", ""),
            ("/", ""),
            ("/a", "a"),
            ("/a/b", "b"),
            ("/a/b/", "b"),
            ("a", "a"),
            ("a/b", "b"),
        ];
        for (input, expected) in basename_cases {
            assert_eq!(posix_basename(input), expected, "basename({input:?})");
        }
    }

    #[test]
    fn win32_normalize_matches_node() {
        // 黄金值由 node 26 的 `path.win32.normalize`/`path.win32.basename` 生成。
        let cases: &[(&str, &str, &str)] = &[
            (
                "./D:\\Windows\\System32\\bash.exe",
                ".\\D:\\Windows\\System32\\bash.exe",
                "bash.exe",
            ),
            (
                "D:\\Windows\\System32\\bash.exe",
                "D:\\Windows\\System32\\bash.exe",
                "bash.exe",
            ),
            (
                "d:/WINNT/System32/BASH.EXE",
                "d:\\WINNT\\System32\\BASH.EXE",
                "BASH.EXE",
            ),
            ("C:\\\\a\\\\b", "C:\\a\\b", "b"),
            ("C:/a/./b/../c/", "C:\\a\\c\\", "c"),
            ("//server/share//x", "\\\\server\\share\\x", "x"),
            ("a/../..", "..", ".."),
            ("C:", "C:.", ""),
            ("C:foo\\bar", "C:foo\\bar", "bar"),
            ("C:\\", "C:\\", ""),
            ("C:\\foo\\", "C:\\foo\\", "foo"),
            ("a:b/c", "a:b\\c", "c"),
            ("a:b", "a:b", "b"),
            ("foo:", ".\\foo:", "foo:"),
            (":foo", ":foo", ":foo"),
            ("a:b:c", "a:b:c", "b:c"),
            ("a:b/", "a:b\\", "b"),
            ("a/b:", ".\\a\\b:", "b:"),
            ("x/:/y", ".\\x\\:\\y", "y"),
            (".\\D:\\x", ".\\D:\\x", "x"),
            ("..\\D:\\x", ".\\..\\D:\\x", "x"),
            ("foo/bar", "foo\\bar", "bar"),
            ("\\foo", "\\foo", "foo"),
            ("/foo", "\\foo", "foo"),
            ("a//b", "a\\b", "b"),
            ("a/../../b", "..\\b", "b"),
            ("C:../../x", "C:..\\..\\x", "x"),
            ("\\\\server\\share", "\\\\server\\share\\", "share"),
            ("\\\\server\\share\\", "\\\\server\\share\\", "share"),
            ("\\\\?\\C:\\x", "\\\\?\\C:\\x", "x"),
            ("", ".", ""),
            (".", ".", "."),
            ("..", "..", ".."),
            ("a/", "a\\", "a"),
            ("a\\", "a\\", "a"),
            ("C:..", "C:..", ".."),
            ("C:.", "C:.", "."),
            ("MNT/c/x", "MNT\\c\\x", "x"),
            ("Cygdrive/d/y", "Cygdrive\\d\\y", "y"),
        ];
        for (input, expected_normalize, expected_basename) in cases {
            assert_eq!(
                win32_normalize(input),
                *expected_normalize,
                "win32.normalize({input:?})"
            );
            assert_eq!(
                win32_basename(input),
                *expected_basename,
                "win32.basename({input:?})"
            );
        }
    }

    #[test]
    fn normalize_windows_shell_path_keeps_js_line_terminator_semantics() {
        // JS 的 `.` 不匹配 \n/\r/U+2028/U+2029；这些输入在 TS 里不匹配正则、原样返回。
        for input in ["/c/Users\rfoo", "/c/a\u{2028}b", "/c/a\u{2029}b", "/c/a\nb"] {
            assert_eq!(
                normalize_windows_shell_path(input),
                input,
                "input {input:?}"
            );
        }
        assert_eq!(
            normalize_windows_shell_path("/c/Users/foo"),
            "C:\\Users\\foo"
        );
        assert_eq!(normalize_windows_shell_path("/mnt/c/x"), "C:\\x");
        assert_eq!(normalize_windows_shell_path("/cygdrive/d/y"), "D:\\y");
        assert_eq!(normalize_windows_shell_path("/MNT/C/x"), "C:\\x");
        // `/[a-z]/i` 只做 ASCII 大小写折叠：U+017F（长 s）不是盘符。
        assert_eq!(normalize_windows_shell_path("/\u{017F}/x"), "/\u{017F}/x");
    }

    #[cfg(unix)]
    #[test]
    fn canonicalize_path_matches_node_realpath_semantics() {
        let dir = tempfile::tempdir().unwrap();
        // Node 的 `path.resolve` 会去掉尾部分隔符与 `.`，文件也能解析（realpath(3) 会 ENOTDIR）。
        let file = dir.path().join("file");
        std::fs::write(&file, "x").unwrap();
        let file = std::fs::canonicalize(&file).unwrap_or(file);
        let file_str = file.to_string_lossy().into_owned();
        assert_eq!(canonicalize_path(&file_str), file_str);
        assert_eq!(canonicalize_path(&format!("{file_str}/")), file_str);
        assert_eq!(canonicalize_path(&format!("{file_str}/.")), file_str);

        // 词法消解 `..`：不存在的中间段不会让解析失败。
        let dir_str = std::fs::canonicalize(dir.path())
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert_eq!(
            canonicalize_path(&format!("{dir_str}/missing-abc/..")),
            dir_str
        );

        // `""` -> 进程 cwd（Node 的 `path.resolve("")`）。
        assert_eq!(
            canonicalize_path(""),
            std::env::current_dir()
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_default()
        );

        // 符号链接正常解析；悬空链接与链接环失败 → 原样返回。
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(dir.path(), &link).unwrap();
        assert_eq!(
            canonicalize_path(&link.to_string_lossy()),
            dir_str,
            "symlink should resolve to its target"
        );
        let dangling = dir.path().join("dangling");
        std::os::unix::fs::symlink(dir.path().join("missing-target"), &dangling).unwrap();
        let dangling_str = dangling.to_string_lossy().into_owned();
        assert_eq!(canonicalize_path(&dangling_str), dangling_str);

        let loop_a = dir.path().join("loop-a");
        let loop_b = dir.path().join("loop-b");
        std::os::unix::fs::symlink(&loop_b, &loop_a).unwrap();
        std::os::unix::fs::symlink(&loop_a, &loop_b).unwrap();
        let loop_str = loop_a.to_string_lossy().into_owned();
        assert_eq!(canonicalize_path(&loop_str), loop_str);
    }

    #[test]
    fn win32_resolve_strips_trailing_separators_like_node() {
        assert_eq!(win32_resolve(&["C:\\proj\\"]), "C:\\proj");
        assert_eq!(win32_resolve(&["C:\\"]), "C:\\");
        assert_eq!(
            win32_resolve(&["\\\\server\\share\\"]),
            "\\\\server\\share\\"
        );
        assert_eq!(
            win32_resolve(&["\\\\server\\share\\dir\\"]),
            "\\\\server\\share\\dir"
        );
        assert_eq!(win32_resolve(&["\\\\?\\C:\\"]), "\\\\?\\C:");
    }

    #[test]
    fn win32_join_and_absolute_match_node() {
        assert_eq!(
            win32_join(&["D:\\WinNT\\", "System32", "bash.exe"]),
            "D:\\WinNT\\System32\\bash.exe"
        );
        assert_eq!(
            win32_join(&["D:\\WinNT", "Sysnative", "bash.exe"]),
            "D:\\WinNT\\Sysnative\\bash.exe"
        );
        assert!(win32_is_absolute("C:/"));
        assert!(win32_is_absolute("\\\\s\\x"));
        assert!(!win32_is_absolute("C:"));
        assert!(!win32_is_absolute("a"));
    }

    // 黄金值由 node 26 的 `fileURLToPath()` 生成（见单元报告）。
    #[test]
    fn file_url_to_path_matches_node_goldens() {
        let cases = [
            ("file:///tmp/x?q=%zz", "/tmp/x"),
            ("file:///tmp/x#frag%zz", "/tmp/x"),
            ("file:///tmp/x?a=%2F", "/tmp/x"),
            ("file:///tmp/x?a=100%", "/tmp/x"),
            ("file:///c|/x", "/c:/x"),
            ("file:///C|", "/C:"),
            ("file:///c|", "/c:"),
            ("file:///c|foo", "/c|foo"),
            ("file:///c%7C/x", "/c|/x"),
            ("file:////x", "//x"),
            ("file://///x", "///x"),
            ("file:////", "//"),
            ("file://///", "///"),
            ("file:////../x", "/x"),
            ("file:////..", "/"),
            ("file:////./x", "//x"),
            ("file:////.//x", "///x"),
            ("file:////../..//x", "//x"),
            ("file:////..//x", "//x"),
            ("file://///../x", "//x"),
            ("file:////c|/x", "//c|/x"),
            ("file:////a/../b", "//b"),
            ("file:////a//b", "//a//b"),
            ("file:////localhost/x", "//localhost/x"),
            ("file://localhost//x", "//x"),
            ("file:///..//x", "//x"),
            ("file:///../x", "/x"),
            ("file:///./x", "/x"),
            ("file:////%2e%2e/x", "/x"),
            ("file:////a/%2e%2e/b", "//b"),
            ("file:///c:/..", "/c:/"),
            ("file:///c:/x/..", "/c:/"),
            ("file:///./c|/x", "/c:/x"),
            ("file:///../c|/x", "/c:/x"),
            ("file:///a/../c|/x", "/c:/x"),
            ("file:///c|/../x", "/c:/x"),
            ("file:///tmp/./x", "/tmp/x"),
            ("file:///tmp/../x", "/x"),
            ("file:///tmp/%2e%2e/x", "/x"),
            ("file://localhost/tmp/x", "/tmp/x"),
            ("file://LOCALHOST/tmp/x", "/tmp/x"),
            ("file:///tmp/a%5Cb", "/tmp/a\\b"),
            ("file:///tmp/x/", "/tmp/x/"),
            ("file:///tmp//x", "/tmp//x"),
            ("file://", "/"),
            ("file:///", "/"),
            ("file://C:/x", "/C:/x"),
            ("file://c:/x", "/c:/x"),
            ("file://c|/x", "/c:/x"),
            ("file:///c:\\x", "/c:/x"),
            ("file:///tmp\\x", "/tmp/x"),
            ("file:////?q", "//"),
            ("file:////#frag", "//"),
            ("file:///a b/c", "/a b/c"),
            ("file:///a%20b", "/a b"),
            ("file:///%00x", "/\0x"),
            ("file:// ", "/"),
            ("file://// ", "//"),
            ("file:///a b ", "/a b"),
            ("file:///a\tb\nc", "/abc"),
        ];
        for (input, expected) in cases {
            assert_eq!(
                file_url_to_path(input),
                expected,
                "fileURLToPath({input:?})"
            );
        }
    }

    // Node 在这些输入上抛错；Rust 签名不可失败 → 原样返回（模块文档的强制偏差）。
    #[test]
    fn file_url_to_path_returns_invalid_urls_unchanged() {
        for input in [
            "file:///tmp/x%ZZ",
            "file:///tmp/x%",
            "file:///tmp/x%2",
            "file:///%E0%A4%A",
            "file:///%FF",
            "file:///a%2Fb",
            "file:///a%2fb",
            "file://host/share/x",
            "file://[::1]/x",
            "file://user@localhost/x",
            "file://localhost:80/x",
        ] {
            assert_eq!(file_url_to_path(input), input, "fileURLToPath({input:?})");
        }
    }

    #[test]
    fn unicode_spaces_and_js_trim() {
        assert_eq!(js_trim("\u{FEFF} abc \u{FEFF}"), "abc");
        assert_eq!(js_trim("\u{1680}x\u{2007}"), "x");
        let options = PathInputOptions {
            normalize_unicode_spaces: true,
            ..Default::default()
        };
        assert_eq!(normalize_path("a\u{00A0}b\u{3000}c", &options), "a b c");
    }
}
