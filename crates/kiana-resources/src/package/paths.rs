//! 安装根目录 / `get_temporary_dir` / `resolve_managed_path` / gitignore，
//! 以及 `core/package-manager.ts` 用到的 `node:path` 词法工具。
//!
//! TS 从 `node:path` 取 `basename` / `dirname` / `join` / `relative` / `resolve` / `sep`。
//! 本模块按 `kiana-durable::env::path::posix` 的同一套算法移植（`ARCHITECTURE.md` §8 第 23 条
//! 指定的唯一实现位置在 `kiana-durable`；`kiana-resources` 不能反向依赖它，因此这里保留一份等价
//! 实现，行为以 Node 的 `lib/path.js` 为准）。
//!
//! 与 Node 一致：所有判定只看 `'/'`/`'.'` 两个 ASCII 字符，切片落在分隔符或字符串两端，
//! 因此 UTF-8 字节实现与 UTF-16 等价。（Windows 上 `'\\'` 也算分隔符，见 `is_path_separator`；
//! 是否按 Windows 语义处理由 `cfg!(windows)` 在编译期决定。）
//!
//! # 为什么需要这个文件
//!
//! TS 侧所有安装路径都由 `node:path` 的字符串语义决定（`resolve` / `join` / `relative` 等），
//! 而 `std::path` 不会按词法折叠 `..`、也会把尾随分隔符吃掉；直接改用 `PathBuf` 会改变安装
//! 目录布局、相对路径输出与去重结果。因此这里按 Node `lib/path.js` 的算法实现一份字符串版
//! 工具。`ARCHITECTURE.md` §8 第 23 条把这类实现的唯一位置定在 `kiana-durable`，但依赖方向
//! 禁止 `kiana-resources` 反向依赖它，所以保留这份等价实现；两处没有自动同步机制，修改其中
//! 一侧时需要人工对照另一侧。
//!
//! # 文件结构（三组职责）
//!
//! 1. **`node:path` 词法工具**：`normalize_string` / `path_resolve` / `normalize` / `join` /
//!    `relative` / `dirname` / `basename` / `to_posix_path` / `js_string_cmp`。除
//!    `path_resolve` 与 `relative` 会读取进程当前目录外，其余是纯字符串运算，不访问文件系统。
//! 2. **目录与错误辅助**：`read_dir_sorted`（顺序稳定的 readdir）、`path_exists`、
//!    `create_dir_all_with_mode`、`format_fs_error`、`get_extension_temp_folder`。
//! 3. **安装路径布局与安全边界**：`impl DefaultPackageManager` 中“作用域 → 安装根 → 托管
//!    路径”的方法，以及 `resolve_managed_path` 的越界拒绝与 `ensure_npm_project` /
//!    `ensure_git_ignore` 的目录初始化。
//!
//! # 调用关系
//!
//! 同目录的 `discover` 用 `read_dir_sorted` 固定遍历顺序、用 `relative` + `to_posix_path`
//! 生成 POSIX 相对路径；`patterns` 用 `js_string_cmp` 复刻 JS 的字符串排序、用 `basename` /
//! `dirname` 做模式匹配；`npm` 用 `basename` / `dirname` / `join` 拼装子进程参数与安装
//! 路径，`git` 用 `join` / `dirname` 拼装 marker 路径（marker 文件名另经 `basename` 取
//! 目标目录名），并用 `format_fs_error` 生成用户可见错误；`resolve`、`settings_io` 与 `mod.rs`
//! 通过本文件的安装路径方法与 trust 断言决定实际读写位置。
//!
//! # 重要约束与失败模式
//!
//! - 路径统一用 `String` 表示，分隔符由 `sep` 给出。所有切片端点都落在分隔符或字符串末尾，
//!   因此多字节 UTF-8 字符不会被截断。
//! - `resolve_managed_path` 是安装路径的越界防线：词法归一化后要求结果等于根或以
//!   `根 + 分隔符` 开头。它不解析符号链接，根内指向根外的链接不会被发现。
//! - 本文件里以安装根为基准的 `project` 作用域方法（`get_base_dir_for_scope`、
//!   `get_npm_install_root`、`get_git_install_root`、`get_managed_npm_install_path` 等）
//!   都先经过 `assert_project_trusted_for_scope`；`temporary` 分支不会触发 trust 检查。
//! - `format_fs_error` 的文本会作为 `PackageInstallFailure.message` 展示给用户，改文案等于
//!   改变外部可见输出。
//! - 词法函数本身不返回错误；会失败的是文件系统相关函数（返回 `None` 或
//!   [`PackageManagerError`]），错误文本与 TS 的 `error.message` 对齐。

use std::sync::Mutex;

use super::parse::NpmSource;
use super::types::SourceScope;
use super::{PackageManagerError, err};

/// `path.sep`（平台分隔符；测试与实现都按 Node 的 posix/win32 语义分别处理）。
///
/// 由 `cfg!(windows)` 在编译期选择：Windows 为 `'\\'`，其它平台为 `'/'`。本文件所有
/// 拼路径的地方都走它，不要硬编码 `'/'`。
pub(crate) const fn sep() -> char {
    if cfg!(windows) { '\\' } else { '/' }
}

/// Node `lib/path.js` 里的两个字节常量：`CHAR_FORWARD_SLASH` 与 `CHAR_DOT`。
/// 只在非 Windows 分支用作分隔符判定，以及识别 `.` / `..` 段。
const CHAR_FORWARD_SLASH: u8 = b'/';
const CHAR_DOT: u8 = b'.';

/// 判定一个字节是否是当前平台的路径分隔符。
///
/// Windows 同时接受 `'/'` 与 `'\\'`（与 Node 的 win32 语义一致）；其它平台只接受 `'/'`。
/// 这是字节级判定；分隔符都是 ASCII，UTF-8 多字节序列的任一字节都不会与它们相同。
fn is_path_separator(code: u8) -> bool {
    if cfg!(windows) {
        code == b'/' || code == b'\\'
    } else {
        code == CHAR_FORWARD_SLASH
    }
}

/// Windows 盘符绝对路径：`C:\` / `C:/`（`C:` 与 `C:a` 不是绝对路径，与
/// `path.win32.isAbsolute` 一致）。
fn starts_with_windows_drive_absolute(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() > 2
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && is_path_separator(bytes[2])
}

/// Windows 语义下的路径分隔符（`/` 与 `\` 都算）。
///
/// 与 [`is_path_separator`] 的区别：后者按**编译目标**选择，POSIX 上不把反斜杠当分隔符。
/// Windows 专用算法（本文件里的 `win32_*` 函数）需要能在任意平台上跑单元测试，因此统一
/// 用这个谓词。
fn is_win32_separator(code: u8) -> bool {
    code == b'/' || code == b'\\'
}

/// Windows 盘符前缀（`C:` / `c:`），不要求后面有分隔符。
fn has_windows_drive_prefix(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

/// `\\server\share` 的解析结果（字节下标）。
struct Win32UncRoot {
    /// server 结束、第一个分隔符的下标。
    server_end: usize,
    /// share 起始下标（server 与 share 之间的重复分隔符已跳过）。
    share_start: usize,
    /// share 结束下标（也是根之后剩余部分的起点）。
    share_end: usize,
}

/// 解析 Windows UNC 根（Node 的规则）。
///
/// 两个前导分隔符之后必须紧跟非空的 server；server 之后至少一个分隔符（多余的分隔符
/// 折叠）；share 必须非空。因此 `///srv/share` 不是 UNC，而 `//server//share` 是。
fn win32_unc_root(path: &str) -> Option<Win32UncRoot> {
    let bytes = path.as_bytes();
    if bytes.len() < 2 || !is_win32_separator(bytes[0]) || !is_win32_separator(bytes[1]) {
        return None;
    }
    let mut index = 2usize;
    let server_start = index;
    while index < bytes.len() && !is_win32_separator(bytes[index]) {
        index += 1;
    }
    if index == server_start {
        return None;
    }
    let server_end = index;
    if index >= bytes.len() || !is_win32_separator(bytes[index]) {
        return None;
    }
    while index < bytes.len() && is_win32_separator(bytes[index]) {
        index += 1;
    }
    let share_start = index;
    while index < bytes.len() && !is_win32_separator(bytes[index]) {
        index += 1;
    }
    if index == share_start {
        return None;
    }
    Some(Win32UncRoot {
        server_end,
        share_start,
        share_end: index,
    })
}

/// 把 Windows UNC 路径拆成（根 `\\server\share`, 根之后的部分）。
///
/// `..` 只在根之后归一化，因此不会弹出 server/share。设备路径（`\\?\C:` /
/// `\\.\C:`）也按两段根处理，避免 `..` 越过设备根。
fn split_unc_root(path: &str) -> Option<(String, &str)> {
    if !cfg!(windows) {
        return None;
    }
    let root = win32_unc_root(path)?;
    let server = &path[2..root.server_end];
    let share = &path[root.share_start..root.share_end];
    let canonical = format!("{s}{s}{server}{s}{share}", s = sep());
    Some((canonical, &path[root.share_end..]))
}

/// 把 Windows 盘符绝对路径拆成（根, 根之后的部分）：`C:/a/b` → (`C:\`, `a/b`）。
///
/// 根统一用平台分隔符；剩余部分交给 `normalize_string(..., false)` 归一化，`..` 因此
/// 不会越过盘符根（`C:\..\a` → `C:\a`）。
fn split_windows_drive_root(path: &str) -> Option<(String, &str)> {
    if !cfg!(windows) || !starts_with_windows_drive_absolute(path) {
        return None;
    }
    Some((format!("{}:{}", &path[..1], sep()), &path[3..]))
}

/// Node 的 `normalizeString(path, allowAboveRoot, separator, isPathSeparator)`。
///
/// 纯词法归一化：把输入按分隔符切段，丢弃空段与 `.`，用 `..` 弹出上一段，其余段原样
/// 保留，段间用一个 [`sep`] 连接。返回值不带尾随分隔符（末尾的分隔符会被当作空段丢弃），
/// 也不补前导根分隔符：输入 `/a` 与 `a` 的输出都可能是 `a`，绝对/相对由调用方恢复。
///
/// # 关键规则
///
/// - 空段（连续分隔符）与单个 `.` 段直接丢弃。
/// - `..` 段弹出结果中的最后一段；结果不足两段、或上一段本身已经是 `..` 时不能再弹，
///   此时只有在 `allow_above_root` 为真时才把 `..` 追加到结果前部。绝对路径传 `false`，
///   所以根之上的 `..` 被静默丢弃；相对路径传 `true`，用于保留前导 `..`。
/// - 其余段原样追加；结果非空时先补一个 [`sep`]。
///
/// # 实现要点
///
/// - 循环多迭代一次（`i == bytes.len()`），把“字符串结束”当作隐式分隔符，让最后一段
///   也走同一个提交分支；若最后一个字节本身就是分隔符则直接 `break`，避免重复处理空段。
/// - `dots` 记录当前段到目前为止是否全由 `.` 组成：`1` 表示 `"."`，`2` 表示 `".."`，
///   `-1` 表示已出现其它字节。它只在分隔符处重置，用整数而不是布尔是因为要区分这两种
///   长度。
/// - `last_slash` 是上一个分隔符的字节下标（初值 `-1`），`last_segment_length` 是结果中
///   最后一段的字节长度。`..` 回退时用后者定位截断点；截断后还要重新扫描结果、重算新的
///   最后一段长度，供后续连续的 `..` 使用。
///
/// # 边界
///
/// 输入为空时返回空串（调用方 [`normalize`] 负责把空串映射为 `"."`）。函数不访问文件
/// 系统、不解析符号链接，也不校验路径是否真实存在。
pub(crate) fn normalize_string(path: &str, allow_above_root: bool) -> String {
    normalize_string_with(path, allow_above_root, sep(), is_path_separator)
}

/// [`normalize_string`] 的可参数化版本：分隔符与判定由调用方给出。
///
/// Windows 专用算法（`win32_*`）在任意平台上都用 `('\\', is_win32_separator)` 调用它，
/// 因此可以在 Linux 上跑 Windows 语义的单元测试。
fn normalize_string_with(
    path: &str,
    allow_above_root: bool,
    separator: char,
    is_separator: fn(u8) -> bool,
) -> String {
    let bytes = path.as_bytes();
    let mut res = String::new();
    let mut last_segment_length: usize = 0;
    let mut last_slash: isize = -1;
    let mut dots: isize = 0;
    let mut code: u8 = 0;

    for i in 0..=bytes.len() {
        // 最后一次迭代没有真实字节：用虚拟分隔符收尾最后一段；若倒数第一个字节已经是
        // 分隔符，说明末尾是空段，直接结束。
        if i < bytes.len() {
            code = bytes[i];
        } else if is_separator(code) {
            break;
        } else {
            code = CHAR_FORWARD_SLASH;
        }

        if is_separator(code) {
            // 到此为止刚扫描完一个段，按段的形态分派：空段/`.` 丢弃，`..` 回退，其余追加。
            if last_slash == i as isize - 1 || dots == 1 {
                // NOOP
            } else if dots == 2 {
                // 先确认结果最后一段是否已经是 `..`：是则不能再弹，否则会错误地消掉前导
                // `..`（`../../a` 必须保留两层）。
                let res_bytes = res.as_bytes();
                if res_bytes.len() < 2
                    || last_segment_length != 2
                    || res_bytes[res_bytes.len() - 1] != CHAR_DOT
                    || res_bytes[res_bytes.len() - 2] != CHAR_DOT
                {
                    if res_bytes.len() > 2 {
                        // 结果里还有可弹出的段：`last_segment_length` 给出最后一段长度，
                        // 减 1 得到它前面的分隔符下标；截断后再重算新的最后一段长度。
                        let last_slash_index =
                            res_bytes.len() as isize - last_segment_length as isize - 1;
                        if last_slash_index == -1 {
                            // 最后一段前面没有分隔符：弹出后结果为空。
                            res.clear();
                            last_segment_length = 0;
                        } else {
                            res.truncate(last_slash_index as usize);
                            let rb = res.as_bytes();
                            let last_sep = rb.iter().rposition(|b| *b == separator as u8);
                            last_segment_length = match last_sep {
                                Some(position) => rb.len() - 1 - position,
                                None => rb.len(),
                            };
                        }
                        last_slash = i as isize;
                        dots = 0;
                        continue;
                    } else if !res.is_empty() {
                        // 结果只剩一段（长度 <= 2）：整体清空即可。
                        res.clear();
                        last_segment_length = 0;
                        last_slash = i as isize;
                        dots = 0;
                        continue;
                    }
                }
                // 走到这里说明没有可弹出的段（或上一段本身就是 `..`）：绝对路径直接丢弃
                // 这个 `..`，相对路径把它保留下来。
                if allow_above_root {
                    if !res.is_empty() {
                        res.push(separator);
                        res.push_str("..");
                    } else {
                        res.push_str("..");
                    }
                    last_segment_length = 2;
                }
            } else {
                // 普通段：结果非空时先补分隔符，再追加原文（切片端点必在 ASCII 分隔符或
                // 字符串末尾，不会切开多字节字符）。
                if !res.is_empty() {
                    res.push(separator);
                    res.push_str(&path[(last_slash + 1) as usize..i]);
                } else {
                    res.push_str(&path[(last_slash + 1) as usize..i]);
                }
                last_segment_length = (i as isize - last_slash - 1) as usize;
            }
            // 一个段处理完毕：记录分隔符位置，并让下一个段重新累计点号。
            last_slash = i as isize;
            dots = 0;
        } else if code == CHAR_DOT && dots != -1 {
            // 当前段到目前为止全是点：继续计数；一旦出现其它字节就置 -1，直到下个分隔符
            // 重置。
            dots += 1;
        } else {
            dots = -1;
        }
    }
    res
}

/// 进程当前目录（取不到时回退 `.`，与 `process.cwd()` 可用的情形一致）。
///
/// 每次调用都重新读取进程状态，没有缓存。路径不是合法 UTF-8 时经 `to_string_lossy`
/// 替换为 U+FFFD，返回值可能无法再指向原目录；这是 Rust 侧没有异常通道时的近似。
pub(crate) fn process_cwd() -> String {
    std::env::current_dir()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|_| ".".to_string())
}

/// `path.resolve(...)`（`process.cwd()` 由环境决定）。
///
/// 每次调用都重新读取当前目录；需要可重复结果（测试、固定基准）时用
/// [`path_resolve_with_cwd`]。
pub(crate) fn path_resolve(parts: &[&str]) -> String {
    path_resolve_with_cwd(parts, &process_cwd())
}

/// `path.resolve(...)`，cwd 由调用方给出。
///
/// # 算法（对应 Node 的 `path.resolve`）
///
/// 1. 快路径：没有参数，或只有一个空串 / `"."`，且 `cwd` 是绝对路径时直接返回 `cwd`
///    （不再重新归一化）。
/// 2. 从最后一个参数向前拼接，直到遇到绝对参数；它左边的参数全部作废（Node 语义：后面的
///    绝对路径重置结果）。
/// 3. 仍不是绝对路径时，在最前面补上 `cwd` 与一个分隔符；`cwd` 自身也可能是绝对路径，
///    所以再判断一次。
/// 4. 交给 [`normalize_string`] 折叠 `.`/`..`：绝对路径不允许越过根，相对路径保留前导
///    `..`。
/// 5. 绝对结果补回被 `normalize_string` 去掉的前导分隔符；相对结果为空时返回 `"."`。
///
/// # 约束
///
/// `cwd` 只当普通字符串使用：本函数不验证它是绝对路径。若 `cwd` 相对且所有参数都是相对
/// 路径，返回值也会是相对路径；依赖“resolve 结果绝对”的调用方需自行保证该前提。函数不
/// 访问文件系统，路径不存在也不报错。
pub(crate) fn path_resolve_with_cwd(parts: &[&str], cwd: &str) -> String {
    if (parts.is_empty() || (parts.len() == 1 && (parts[0].is_empty() || parts[0] == ".")))
        && is_absolute(cwd)
    {
        return cwd.to_owned();
    }
    let mut resolved_path = String::new();
    let mut resolved_absolute = false;

    let mut index = parts.len() as isize - 1;
    // 从右往左拼：一旦当前参数是绝对路径，右边的结果已经完整，左边的参数不再影响结果，
    // 因此循环以 resolved_absolute 为终止条件。空串参数跳过（不产生额外分隔符）。
    while index >= 0 && !resolved_absolute {
        let path = parts[index as usize];
        if path.is_empty() {
            index -= 1;
            continue;
        }
        resolved_path = format!("{path}{}{resolved_path}", sep());
        resolved_absolute = is_absolute(path);
        index -= 1;
    }

    if !resolved_absolute {
        // 扫到最左仍没有绝对参数：用 cwd 兜底；cwd 自身可能是绝对路径。
        resolved_path = format!("{cwd}{}{resolved_path}", sep());
        resolved_absolute = is_absolute(cwd);
    }

    let normalized = normalize_string(&resolved_path, !resolved_absolute);

    if resolved_absolute {
        if let Some((root, rest)) = split_windows_drive_root(&resolved_path) {
            let normalized_rest = normalize_string(rest, false);
            if normalized_rest.is_empty() {
                return root;
            }
            return format!("{root}{normalized_rest}");
        }
        if let Some((root, rest)) = split_unc_root(&resolved_path) {
            let normalized_rest = normalize_string(rest, false);
            if normalized_rest.is_empty() {
                // Node 的 resolve 保留 UNC 根的尾分隔符（`\\server\share\`）。
                return format!("{root}{}", sep());
            }
            return format!("{root}{}{normalized_rest}", sep());
        }
        format!("{}{normalized}", sep())
    } else if !normalized.is_empty() {
        normalized
    } else {
        ".".to_owned()
    }
}

/// `path.resolve(...)` 的别名（与 TS 中的 `resolve(a, b)` 对应）。
///
/// 语义与 [`path_resolve`] 完全相同，只是调用点保留接近 TS 的命名。
pub(crate) fn resolve_join(parts: &[&str]) -> String {
    path_resolve(parts)
}

/// `path.normalize(path)`。
///
/// 规则：空串返回 `"."`；连续分隔符折叠、`.` 段丢弃、`..` 弹出上一段（绝对路径在根处
/// 停止）；保留输入的尾随分隔符（`"a/"` → `"a/"`，`"./"` → `"./"`）。归一化后为空时按
/// “是否绝对 / 是否有尾随分隔符”返回 `"/"`、`"./"` 或 `"."`。
///
/// 纯字符串运算，不访问文件系统，也不解析符号链接。
pub(crate) fn normalize(path: &str) -> String {
    if cfg!(windows) {
        return win32_normalize(path);
    }
    if path.is_empty() {
        return ".".to_owned();
    }
    let is_absolute_path = is_absolute(path);
    let trailing_separator = path.as_bytes().last().map(|b| is_path_separator(*b)) == Some(true);
    let mut normalized = normalize_string(path, !is_absolute_path);
    if normalized.is_empty() {
        if is_absolute_path {
            return sep().to_string();
        }
        return if trailing_separator {
            format!(".{}", sep())
        } else {
            ".".to_owned()
        };
    }
    if trailing_separator {
        normalized.push(sep());
    }
    if is_absolute_path {
        format!("{}{normalized}", sep())
    } else {
        normalized
    }
}

/// Node `path.win32.normalize` 的移植（不含保留设备名 `CON` / `COM1` 等分支）。
///
/// Windows 单独实现的原因：根形态多（盘符绝对 `C:\`、盘符相对 `C:a`、UNC、设备路径），
/// 裸盘符 `C:` 要归一化成 `C:.`，含 `:` 的相对结果还要加 `.\` 前缀（CVE-2024-36139）。
/// 函数只依赖 [`is_win32_separator`] 与显式分隔符归一化，因此在 Linux 上也能做 Windows
/// 语义的单元测试。
fn win32_normalize(path: &str) -> String {
    let bytes = path.as_bytes();
    let len = bytes.len();
    if len == 0 {
        return ".".to_owned();
    }
    if len == 1 {
        return if is_win32_separator(bytes[0]) {
            "\\".to_owned()
        } else {
            path.to_owned()
        };
    }
    let mut root_end = 0usize;
    let mut device: Option<String> = None;
    let mut is_absolute_path = false;
    if is_win32_separator(bytes[0]) {
        is_absolute_path = true;
        root_end = 1;
        if is_win32_separator(bytes[1]) {
            match win32_unc_root(path) {
                Some(unc) => {
                    let server = &path[2..unc.server_end];
                    let share = &path[unc.share_start..unc.share_end];
                    if server == "." || server == "?" {
                        // 设备根（`\\?\C:` / `\\.\PHYSICALDRIVE0`）：Node 保留 4 字节
                        // 根，把设备名之后的部分当普通 tail 归一化。
                        device = Some(format!("\\\\{server}"));
                        root_end = 4;
                    } else if unc.share_end == len {
                        // UNC 根且没有剩余部分：补一个尾分隔符。
                        return format!("\\\\{server}\\{share}\\");
                    } else {
                        device = Some(format!("\\\\{server}\\{share}"));
                        root_end = unc.share_end;
                    }
                }
                None => root_end = 1,
            }
        }
    } else if has_windows_drive_prefix(path) {
        device = Some(path[..2].to_owned());
        root_end = 2;
        if len > 2 && is_win32_separator(bytes[2]) {
            is_absolute_path = true;
            root_end = 3;
        }
    }
    let mut tail = if root_end < len {
        normalize_string_with(
            &path[root_end..],
            !is_absolute_path,
            '\\',
            is_win32_separator,
        )
    } else {
        String::new()
    };
    if tail.is_empty() && !is_absolute_path {
        tail = ".".to_owned();
    }
    if !tail.is_empty() && is_win32_separator(bytes[len - 1]) {
        tail.push('\\');
    }
    // CVE-2024-36139：非绝对、无设备根、原路径含 `:` 时，结果可能被 Windows 解释成盘符、
    // 盘符相对路径或 ADS；Node 用 `.\` 前缀把它钉成明确的相对路径。
    if !is_absolute_path && device.is_none() && path.contains(':') {
        let tail_leads_with_drive = tail.len() >= 2
            && tail.as_bytes()[0].is_ascii_alphabetic()
            && tail.as_bytes()[1] == b':';
        let colon_before_separator = bytes.iter().enumerate().any(|(index, byte)| {
            *byte == b':' && (index + 1 == len || is_win32_separator(bytes[index + 1]))
        });
        if tail_leads_with_drive || colon_before_separator {
            return format!(".\\{tail}");
        }
    }
    match device {
        None => {
            if is_absolute_path {
                format!("\\{tail}")
            } else {
                tail
            }
        }
        Some(device) => {
            if is_absolute_path {
                format!("{device}\\{tail}")
            } else {
                format!("{device}{tail}")
            }
        }
    }
}

/// `path.isAbsolute(path)`。
///
/// 首字节是分隔符即为绝对路径（Windows 上 `'/'` 与 `'\\'` 都算，UNC 因此天然命中）。
/// Windows 额外识别盘符形式 `C:\a` / `C:/a`；`C:` 与 `C:a` 仍视为相对路径，与
/// `path.win32.isAbsolute` 一致。POSIX 下不识别盘符（`C:\a` 是普通相对路径）。
pub(crate) fn is_absolute(path: &str) -> bool {
    path.as_bytes().first().map(|b| is_path_separator(*b)) == Some(true)
        || (cfg!(windows) && starts_with_windows_drive_absolute(path))
}

/// `path.join(...parts)`。
///
/// 空串参数被忽略；一个非空参数都没有时返回 `"."`。非空参数按顺序拼接（后面的绝对路径
/// **不会**像 `resolve` 那样截断前面的部分：`join(&["a", "/b"]) == "a/b"`），最后由
/// [`normalize`] 折叠 `.`/`..` 与重复分隔符。
///
/// 不读取 cwd，结果不保证是绝对路径，也不访问文件系统。
pub(crate) fn join(parts: &[&str]) -> String {
    if parts.is_empty() {
        return ".".to_owned();
    }
    let mut joined: Option<String> = None;
    for part in parts {
        // 空串既不产生分隔符也不产生段（Node 的 join 语义）；全部为空时落到下面的 "."。
        if !part.is_empty() {
            match joined.as_mut() {
                Some(current) => {
                    current.push(sep());
                    current.push_str(part);
                }
                None => joined = Some((*part).to_owned()),
            }
        }
    }
    match joined {
        Some(joined) => normalize(&joined),
        None => ".".to_owned(),
    }
}

/// `fs.readdirSync(dir)` 的条目列表（失败返回 `None`）。
///
/// Node 的 `readdirSync`（libuv `uv_fs_scandir`）按文件名字节序排序；`std::fs::read_dir`
/// 返回的是 getdents 原始顺序，因此所有需要与 TS 输出顺序一致的遍历都必须经过这里。
///
/// 返回 `None` 只表示 `read_dir` 打开目录失败（不存在、权限不足、不是目录等）。单个条目
/// 读取失败（`DirEntry::ok()` 为 `None`）会被静默跳过，既不报错也不计入结果。
pub(crate) fn read_dir_sorted(dir: &str) -> Option<Vec<std::fs::DirEntry>> {
    let mut entries: Vec<std::fs::DirEntry> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|entry| entry.ok())
        .collect();
    entries.sort_by_key(|entry| entry.file_name());
    Some(entries)
}

/// `path.relative(from, to)`。
///
/// 两侧都先用进程 cwd 解析（[`path_resolve`]），相同路径返回空串。每次调用读取一次 cwd；
/// 需要固定基准时用 [`relative_with_cwd`]。
pub(crate) fn relative(from: &str, to: &str) -> String {
    relative_with_cwd(from, to, &process_cwd())
}

/// `path.relative(from, to)`，cwd 由调用方给出。
///
/// # 算法
///
/// 1. `from == to` 直接返回 `""`；否则两侧各自 resolve（含归一化），再次相等也返回 `""`。
/// 2. 从下标 1（根分隔符之后）逐字节比较，记录最后一个公共分隔符的位置
///    `last_common_sep`（没有则为 -1，表示只共享根）。
/// 3. 公共前缀之后，`from` 每剩一段就输出一个 `..`，再接上 `to`（已 resolve 的字符串）
///    从 `last_common_sep` 起的剩余部分。
/// 4. 前缀比较正好用完较短一方时有两个特例：`from` 是 `to` 的精确基路径（`to` 的下一个
///    字节就是分隔符）时直接返回 `to` 的剩余段；`from` 是根（`i == 0`）时返回去掉前导
///    分隔符的 `to`。
///
/// # 约束
///
/// 假定两侧 resolve 后都以 [`sep`] 开头：`from_start` / `to_start` 固定为 1，用来跳过根
/// 分隔符。若 `cwd` 不是绝对路径而参数又是相对路径，resolve 会返回相对路径，前缀比较从
/// 第 2 个字节开始，结果不再可靠。比较按字节进行，输出切片只落在分隔符处，不会把多字节
/// UTF-8 字符切成两半。函数不访问文件系统。
pub(crate) fn relative_with_cwd(from: &str, to: &str, cwd: &str) -> String {
    if from == to {
        return String::new();
    }
    let resolved_from = path_resolve_with_cwd(&[from], cwd);
    let resolved_to = path_resolve_with_cwd(&[to], cwd);
    if resolved_from == resolved_to {
        return String::new();
    }
    if cfg!(windows) {
        win32_relative_resolved(&resolved_from, &resolved_to)
    } else {
        posix_relative_resolved(&resolved_from, &resolved_to)
    }
}

/// Node `path.posix.relative` 的算法（两侧都已 resolve）。
fn posix_relative_resolved(from: &str, to: &str) -> String {
    let from_start = 1usize;
    let from_end = from.len();
    let from_len = from_end - from_start;
    let to_start = 1usize;
    let to_len = to.len().saturating_sub(to_start);

    let length = from_len.min(to_len);
    let mut last_common_sep: isize = -1;
    let mut i = 0usize;
    // 下标 0 是两条绝对路径共同的根分隔符，从 1 开始比较内容；last_common_sep 记录最后
    // 一个公共分隔符，决定要回退几级以及 `to` 从哪一段开始保留。
    while i < length {
        let from_code = from.as_bytes()[from_start + i];
        if from_code != to.as_bytes()[to_start + i] {
            break;
        } else if from_code == sep() as u8 {
            last_common_sep = i as isize;
        }
        i += 1;
    }
    if i == length {
        // 公共前缀覆盖了较短的一整条路径：先处理“from 是 to 的精确基路径”和“from 是根”
        // 两个特例；都不命中时沿用循环里记录的最后一个公共分隔符。
        if to_len > length {
            if to.as_bytes().get(to_start + i) == Some(&(sep() as u8)) {
                // `from` 是 `to` 的精确基路径。
                return to[to_start + i + 1..].to_owned();
            }
            if i == 0 {
                // `from` 是根。
                return to[to_start + i..].to_owned();
            }
        } else if from_len > length {
            if from.as_bytes().get(from_start + i) == Some(&(sep() as u8)) {
                // `from` 就是 `to` 的前缀目录边界。
                last_common_sep = i as isize;
            } else if i == 0 {
                last_common_sep = 0;
            }
        }
    }
    let mut out = String::new();
    // 从最后一个公共分隔符的下一位开始，`from` 每剩一个目录就记录一个 `..`。
    let mut index = (from_start as isize + last_common_sep + 1) as usize;
    while index <= from_end {
        if index == from_end || from.as_bytes()[index] == sep() as u8 {
            if out.is_empty() {
                out.push_str("..");
            } else {
                out.push(sep());
                out.push_str("..");
            }
        }
        index += 1;
    }
    format!(
        "{out}{}",
        &to[(to_start as isize + last_common_sep) as usize..]
    )
}

/// Node `path.win32.relative` 的算法（两侧都已 resolve）。
///
/// 与 POSIX 的关键差异：路径比较**不区分大小写**（两侧先小写化，输出仍用 `to` 的原文
/// 大小写）；盘符/UNC 根不同时直接返回 `to`；小写化改变 UTF-16 长度时退回按段比较。
/// 全程在 UTF-16 码元上操作，避免把多字节字符切成两半。
fn win32_relative_resolved(from_orig: &str, to_orig: &str) -> String {
    if from_orig == to_orig {
        return String::new();
    }
    let from_orig_u: Vec<u16> = from_orig.encode_utf16().collect();
    let to_orig_u: Vec<u16> = to_orig.encode_utf16().collect();
    let from_lower: Vec<u16> = from_orig.to_lowercase().encode_utf16().collect();
    let to_lower: Vec<u16> = to_orig.to_lowercase().encode_utf16().collect();
    if from_lower == to_lower {
        return String::new();
    }
    if from_orig_u.len() != from_lower.len() || to_orig_u.len() != to_lower.len() {
        return win32_relative_split(from_orig, to_orig);
    }
    const SEP: u16 = b'\\' as u16;

    // 去掉前导分隔符（UNC 根的两个前导反斜杠），以及 UNC 根本身可能带的尾部分隔符。
    let mut from_start = 0usize;
    while from_start < from_lower.len() && from_lower[from_start] == SEP {
        from_start += 1;
    }
    let mut from_end = from_lower.len();
    while from_end > from_start + 1 && from_lower[from_end - 1] == SEP {
        from_end -= 1;
    }
    let from_len = from_end - from_start;
    let mut to_start = 0usize;
    while to_start < to_lower.len() && to_lower[to_start] == SEP {
        to_start += 1;
    }
    let mut to_end = to_lower.len();
    while to_end > to_start + 1 && to_lower[to_end - 1] == SEP {
        to_end -= 1;
    }
    let to_len = to_end - to_start;

    let length = from_len.min(to_len);
    let mut last_common_sep: isize = -1;
    let mut i = 0usize;
    while i < length {
        let from_code = from_lower[from_start + i];
        if from_code != to_lower[to_start + i] {
            break;
        }
        if from_code == SEP {
            last_common_sep = i as isize;
        }
        i += 1;
    }
    if i != length {
        // 在第一个公共分隔符之前就不同：没有公共根（不同盘符/UNC），返回 `to` 原文。
        if last_common_sep == -1 {
            return to_orig.to_owned();
        }
    } else {
        if to_len > length {
            if to_lower[to_start + i] == SEP {
                return String::from_utf16_lossy(&to_orig_u[to_start + i + 1..]);
            }
            if i == 2 {
                // `from` 是盘符根（`C:\`）。
                return String::from_utf16_lossy(&to_orig_u[to_start + i..]);
            }
        }
        if from_len > length {
            if from_lower[from_start + i] == SEP {
                last_common_sep = i as isize;
            } else if i == 2 {
                // `to` 是盘符根（`C:\`）。
                last_common_sep = 3;
            }
        }
        if last_common_sep == -1 {
            last_common_sep = 0;
        }
    }

    let mut out = String::new();
    let mut index = from_start as isize + last_common_sep + 1;
    while index <= from_end as isize {
        if index == from_end as isize || from_lower[index as usize] == SEP {
            if out.is_empty() {
                out.push_str("..");
            } else {
                out.push_str("\\..");
            }
        }
        index += 1;
    }
    let to_slice_start = ((to_start as isize + last_common_sep).max(0) as usize).min(to_end);
    if !out.is_empty() {
        return format!(
            "{out}{}",
            String::from_utf16_lossy(&to_orig_u[to_slice_start..to_end])
        );
    }
    if to_orig_u.get(to_slice_start) == Some(&SEP) {
        return String::from_utf16_lossy(&to_orig_u[to_slice_start + 1..to_end]);
    }
    String::from_utf16_lossy(&to_orig_u[to_slice_start..to_end])
}

/// `win32_relative_resolved` 的退化分支：小写化改变了 UTF-16 长度时按 `\` 切段比较。
fn win32_relative_split(from_orig: &str, to_orig: &str) -> String {
    let mut from_split: Vec<&str> = from_orig.split('\\').collect();
    let mut to_split: Vec<&str> = to_orig.split('\\').collect();
    if from_split.last() == Some(&"") {
        from_split.pop();
    }
    if to_split.last() == Some(&"") {
        to_split.pop();
    }
    let from_len = from_split.len();
    let to_len = to_split.len();
    let length = from_len.min(to_len);
    let mut i = 0usize;
    while i < length && from_split[i].to_lowercase() == to_split[i].to_lowercase() {
        i += 1;
    }
    if i == 0 {
        return to_orig.to_owned();
    }
    if i == length {
        if to_len > length {
            return to_split[i..].join("\\");
        }
        if from_len > length {
            let mut out = "..\\".repeat(from_len - 1 - i);
            out.push_str("..");
            return out;
        }
        return String::new();
    }
    let mut out = "..\\".repeat(from_len - i);
    out.push_str(&to_split[i..].join("\\"));
    out
}

/// `path.dirname(path)`。
///
/// 从末尾向前扫描最后一个分隔符并截断；不折叠 `.`/`..`、不解析符号链接、不访问文件系统。
/// 没有可用分隔符时，绝对路径返回 `"/"`、相对路径返回 `"."`。两个根的特例：`"/"` 返回
/// `"/"`，而分隔符紧跟根之后（如 `"//a"`）时返回 `"//"`，逐字对齐 Node。
pub(crate) fn dirname(path: &str) -> String {
    if cfg!(windows) {
        win32_dirname(path)
    } else {
        posix_dirname(path)
    }
}

/// Node `path.posix.dirname`。
fn posix_dirname(path: &str) -> String {
    if path.is_empty() {
        return ".".to_owned();
    }
    let has_root = is_absolute(path);
    let mut end: isize = -1;
    let mut matched_slash = true;
    let mut i = path.len() as isize - 1;
    while i >= 1 {
        if path.as_bytes()[i as usize] == sep() as u8 {
            if !matched_slash {
                end = i;
                break;
            }
        } else {
            matched_slash = false;
        }
        i -= 1;
    }
    if end == -1 {
        return if has_root {
            sep().to_string()
        } else {
            ".".to_owned()
        };
    }
    if has_root && end == 1 {
        return format!("{}{}", sep(), sep());
    }
    path[..end as usize].to_owned()
}

/// Node `path.win32.dirname` 的移植（盘符/UNC/设备根都按 Node 的 rootEnd/offset 规则）。
fn win32_dirname(path: &str) -> String {
    let bytes = path.as_bytes();
    let len = bytes.len();
    if len == 0 {
        return ".".to_owned();
    }
    if len == 1 {
        return if is_win32_separator(bytes[0]) {
            path.to_owned()
        } else {
            ".".to_owned()
        };
    }
    let mut root_end: isize = -1;
    let mut offset = 0usize;
    if is_win32_separator(bytes[0]) {
        root_end = 1;
        offset = 1;
        if is_win32_separator(bytes[1])
            && let Some(unc) = win32_unc_root(path)
        {
            if unc.share_end == len {
                return path.to_owned();
            }
            if is_win32_separator(bytes[unc.share_end]) {
                // 根 + 紧随其后的分隔符都算根（与 Node 的 `rootEnd = j + 1` 一致）。
                root_end = (unc.share_end + 1) as isize;
                offset = unc.share_end + 1;
            }
        }
    } else if has_windows_drive_prefix(path) {
        root_end = if len > 2 && is_win32_separator(bytes[2]) {
            3
        } else {
            2
        };
        offset = root_end as usize;
    }
    let mut end: isize = -1;
    let mut matched_slash = true;
    let mut i = len as isize - 1;
    while i >= offset as isize {
        if is_win32_separator(bytes[i as usize]) {
            if !matched_slash {
                end = i;
                break;
            }
        } else {
            matched_slash = false;
        }
        i -= 1;
    }
    if end == -1 {
        if root_end == -1 {
            return ".".to_owned();
        }
        end = root_end;
    }
    path[..end as usize].to_owned()
}

/// `path.basename(path)`（不带扩展名参数的形态）。
///
/// 反向扫描，取最后一个分隔符之后的部分；空串与只含分隔符的路径返回 `""`，尾随分隔符
/// 被忽略（`"a/b/"` → `"b"`）。纯字符串运算，不访问文件系统。本 crate 只用这一种形态，
/// 因此没有实现 Node 的 `ext` 参数。
pub(crate) fn basename(path: &str) -> String {
    if cfg!(windows) {
        win32_basename(path)
    } else {
        posix_basename(path)
    }
}

/// Node `path.posix.basename`（不带扩展名参数）。
fn posix_basename(path: &str) -> String {
    let mut start = 0usize;
    let mut end: isize = -1;
    let mut matched_slash = true;
    let mut i = path.len() as isize - 1;
    while i >= 0 {
        let code = path.as_bytes()[i as usize];
        if code == sep() as u8 {
            if !matched_slash {
                start = i as usize + 1;
                break;
            }
        } else if end == -1 {
            matched_slash = false;
            end = i + 1;
        }
        i -= 1;
    }
    if end == -1 {
        return String::new();
    }
    path[start..end as usize].to_owned()
}

/// Node `path.win32.basename` 的移植：盘符前缀（`C:`）不当作名字的一部分，两个平台分隔符
/// 都算分隔符。
fn win32_basename(path: &str) -> String {
    let bytes = path.as_bytes();
    let len = bytes.len();
    let mut start = if has_windows_drive_prefix(path) { 2 } else { 0 };
    let mut end: isize = -1;
    let mut matched_slash = true;
    let mut i = len as isize - 1;
    while i >= start as isize {
        if is_win32_separator(bytes[i as usize]) {
            if !matched_slash {
                start = i as usize + 1;
                break;
            }
        } else if end == -1 {
            matched_slash = false;
            end = i + 1;
        }
        i -= 1;
    }
    if end == -1 {
        return String::new();
    }
    path[start..end as usize].to_owned()
}

/// `toPosixPath(p) = p.split(sep).join("/")`。
///
/// 只在 Windows 上把 `'\\'` 换成 `'/'`；其它平台原样返回，因此 Unix 路径里的字面反斜杠
/// 不会被改写（它不是分隔符）。`discover` / `patterns` 用它生成并比较 ignore 规则所需的
/// POSIX 相对路径。
pub(crate) fn to_posix_path(path: &str) -> String {
    if cfg!(windows) {
        path.split('\\').collect::<Vec<_>>().join("/")
    } else {
        path.to_string()
    }
}

/// JS 字符串比较（UTF-16 code unit 序），对应 `a < b ? -1 : a > b ? 1 : 0`。
///
/// 按 UTF-16 code unit 逐个比较，而不是按 Unicode 标量值：辅助平面字符由代理对表示，
/// 会先比较高代理。例如 U+1F600 的首个 code unit 是 0xD83D，小于 U+FFFD 的 0xFFFD，
/// 所以在这个序里 emoji 排在 U+FFFD 前面（测试 `js_string_cmp_orders_by_utf16_units`
/// 锁定了这一点）。`patterns` 用它复刻 TS 字符串数组的默认排序。
pub(crate) fn js_string_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let mut left = a.encode_utf16();
    let mut right = b.encode_utf16();
    loop {
        match (left.next(), right.next()) {
            (Some(x), Some(y)) => {
                if x != y {
                    return x.cmp(&y);
                }
            }
            (None, None) => return std::cmp::Ordering::Equal,
            (None, Some(_)) => return std::cmp::Ordering::Less,
            (Some(_), None) => return std::cmp::Ordering::Greater,
        }
    }
}

/// `getExtensionTempFolder(agentDir)`：`<agentDir>/tmp/extensions`，每次调用都 chmod 0700。
///
/// TS: `package-manager.ts#getExtensionTempFolder`。TS 在 `mkdirSync`/`chmodSync` 失败时抛错；
/// Rust 用 [`PackageManagerError`] 透传。
///
/// # 行为
///
/// 1. 路径由 `join` 词法拼成，不要求 `agent_dir` 已存在。
/// 2. Unix 上逐层创建缺失目录并给每层 `0700`，然后**无条件**对叶子目录 `chmod 0700`，
///    已存在的目录也会被改权限；非 Unix 只调用 `create_dir_all`，不设置权限。
/// 3. 任一步失败都转成 [`PackageManagerError`]，文本由 `format_fs_error` 生成，`syscall`
///    分别为 `mkdir` / `chmod`。调用方据此把失败归入安装错误。
///
/// # 边界
///
/// Unix 分支只判断“目标是否是目录”：目标已存在但不是目录时不会报错，随后 chmod 会作用
/// 于该文件并返回其路径。调用方传入的 `agent_dir` 若指向普通文件，创建
/// `<agent_dir>/tmp/extensions` 时 `mkdir` 会失败并得到 `ENOTDIR` 错误（本函数的
/// `is_dir` 检查本身不产生该错误）。
pub fn get_extension_temp_folder(agent_dir: &str) -> Result<String, PackageManagerError> {
    let temp_folder = join(&[agent_dir, "tmp", "extensions"]);
    // Node 的 `mkdirSync(tempFolder, { recursive: true, mode: 0o700 })` 对每一层新建目录
    // 都应用 mode；`create_dir_all` 只会给叶子目录之外的新目录默认权限。
    create_dir_all_with_mode(&temp_folder, 0o700)
        .map_err(|error| err(format_fs_error(&error, "mkdir", &temp_folder)))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&temp_folder, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| err(format_fs_error(&error, "chmod", &temp_folder)))?;
    }
    Ok(temp_folder)
}

/// 递归创建目录，新建的每一层都用 `mode`（等价 Node `mkdirSync({recursive:true, mode})`）。
///
/// # 算法（Unix）
///
/// 1. 目标已是目录 → 直接成功。
/// 2. 从目标向上收集所有不存在的祖先，直到遇到已存在的路径或没有父目录可退。
/// 3. 反向（从最上层开始）逐个用 `DirBuilder::mode(mode)` 创建；若创建时发现已被别的
///    线程/进程抢先创建且确实是目录，视为成功，以容忍并发创建。
///
/// # 约束
///
/// - 非 Unix 忽略 `mode`，直接 `create_dir_all`，因此不保证任何权限位。
/// - 目标已存在但不是目录时直接返回 `Ok`（不检查类型）；中间某一层是文件时，创建其子目录
///   会返回错误并原样向上传播。
fn create_dir_all_with_mode(path: &str, mode: u32) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        use std::path::{Path, PathBuf};

        let target = Path::new(path);
        if target.is_dir() {
            return Ok(());
        }
        // 自底向上收集缺失的层级；遇到已存在的项或无法再取父目录时停止。
        let mut missing: Vec<PathBuf> = Vec::new();
        let mut current = target;
        while !current.as_os_str().is_empty() && !current.exists() {
            missing.push(current.to_path_buf());
            match current.parent() {
                Some(parent) if parent != current => current = parent,
                _ => break,
            }
        }
        // 反向创建：先父后子，保证每一层创建时父目录已存在。
        for directory in missing.into_iter().rev() {
            match std::fs::DirBuilder::new().mode(mode).create(&directory) {
                Ok(()) => {}
                Err(error)
                    if error.kind() == std::io::ErrorKind::AlreadyExists && directory.is_dir() => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = mode;
        std::fs::create_dir_all(path)
    }
}

/// Node 的 fs 错误文本（`ENOENT: no such file or directory, mkdir '/x'`）。
///
/// `error.message` 是本单元会透传的用户可见文本之一（安装失败会进入
/// `PackageInstallFailure.message`）。
///
/// # 覆盖范围与限制
///
/// - 只识别一组固定的 POSIX errno 编号（1/2/13/17/18/20/21/22/28/30 与 ELOOP）；其余
///   `raw_os_error()`（包括 `None`）统一输出 `EIO: i/o error`，原始错误信息丢失。
/// - ELOOP 的编号按平台硬编码：macOS 为 62，其它平台（含 Linux 与 Windows）为 40。
/// - Windows 的 `raw_os_error()` 是 Win32 错误码，与这张 POSIX 表不是同一套编号；命中的
///   条目可能给出错误的名字。
/// - 文案固定为 `<CODE>: <description>, <syscall> '<path>'`，调用方只能靠 `syscall` 与
///   `path` 区分具体是哪个操作失败。
pub(crate) fn format_fs_error(error: &std::io::Error, syscall: &str, path: &str) -> String {
    // errno 编号随平台不同（macOS 的 ELOOP 是 62，Linux 是 40）。
    #[cfg(target_os = "macos")]
    const ELOOP: i32 = 62;
    #[cfg(not(target_os = "macos"))]
    const ELOOP: i32 = 40;
    let code = match error.raw_os_error() {
        Some(1) => "EPERM",
        Some(2) => "ENOENT",
        Some(13) => "EACCES",
        Some(17) => "EEXIST",
        Some(18) => "EXDEV",
        Some(20) => "ENOTDIR",
        Some(21) => "EISDIR",
        Some(22) => "EINVAL",
        Some(28) => "ENOSPC",
        Some(30) => "EROFS",
        Some(value) if value == ELOOP => "ELOOP",
        _ => "EIO",
    };
    let description = match code {
        "EPERM" => "operation not permitted",
        "ENOENT" => "no such file or directory",
        "EACCES" => "permission denied",
        "EEXIST" => "file already exists",
        "EXDEV" => "cross-device link not permitted",
        "ENOTDIR" => "not a directory",
        "EISDIR" => "illegal operation on a directory",
        "EINVAL" => "invalid argument",
        "ENOSPC" => "no space left on device",
        "EROFS" => "read-only file system",
        "ELOOP" => "too many symbolic links encountered",
        _ => "i/o error",
    };
    format!("{code}: {description}, {syscall} '{path}'")
}

/// 全局 npm 根缓存（`globalNpmRoot` + `globalNpmRootCommandKey`）。
///
/// 由 `npm::get_global_npm_root` 读写：`command_key` 是当前 npmCommand 的 `command` 与
/// `args` 用 `\0` 连接成的字符串，key 变化时缓存失效；空结果不写入 `root`（下次调用重新
/// 执行查询）。`DefaultPackageManager` 用 `Arc<Mutex<...>>` 保存它，锁统一经 `npm::lock`
/// 获取（互斥量中毒时取回内部值，不 panic）。
#[derive(Debug, Default)]
pub(crate) struct NpmRootCache {
    /// 缓存值。
    pub root: Option<String>,
    /// 对应的 npmCommand key（argv 用 `\0` 连接）。
    pub command_key: Option<String>,
}

/// `DefaultPackageManager` 的路径 / 安装根相关方法。
impl super::DefaultPackageManager {
    /// `assertProjectTrustedForScope(scope)`。
    ///
    /// 只有 `Project` 作用域会检查 settings 的 project trust；`User` / `Temporary` 直接
    /// 通过。未受信时返回固定文本
    /// `Project is not trusted; refusing to access project package storage`。公开的
    /// `bool` / `Option` 接口会把该错误折叠成 `false` / `None`，需要区分时用对应的
    /// `try_*` 方法。
    pub(crate) fn assert_project_trusted_for_scope(
        &self,
        scope: SourceScope,
    ) -> Result<(), PackageManagerError> {
        if scope == SourceScope::Project && !self.settings_manager.is_project_trusted() {
            return Err(err(
                "Project is not trusted; refusing to access project package storage",
            ));
        }
        Ok(())
    }

    /// `getBaseDirForScope(scope)`。
    ///
    /// - `project`：先过 trust 检查，返回 `<cwd>/.kiana`；
    /// - `user`：返回 `agent_dir`；
    /// - `temporary`：返回 `cwd`（没有单独的临时基准目录）。
    ///
    /// 只做词法拼接，不创建目录、不检查存在性；`project` 未受信时返回 trust 错误。
    pub(crate) fn get_base_dir_for_scope(
        &self,
        scope: SourceScope,
    ) -> Result<String, PackageManagerError> {
        if scope == SourceScope::Project {
            self.assert_project_trusted_for_scope(scope)?;
            return Ok(join(&[&self.cwd, kiana_core::config::CONFIG_DIR_NAME]));
        }
        if scope == SourceScope::User {
            return Ok(self.agent_dir.clone());
        }
        Ok(self.cwd.clone())
    }

    /// `getNpmInstallRoot(scope, temporary)`。
    ///
    /// `temporary == true` 时优先返回 `get_temporary_dir("npm")` 的结果，不再看 `scope`，
    /// 因此即使 `scope` 是 project 也不会触发 trust 检查；该调用会顺带确保
    /// `<agent_dir>/tmp/extensions` 存在（见 [`get_extension_temp_folder`]）。
    /// 否则 `project` → trust 检查 + `<cwd>/.kiana/npm`，其余 → `<agent_dir>/npm`。
    pub(crate) fn get_npm_install_root(
        &self,
        scope: SourceScope,
        temporary: bool,
    ) -> Result<String, PackageManagerError> {
        if temporary {
            return self.get_temporary_dir("npm", None, None);
        }
        if scope == SourceScope::Project {
            self.assert_project_trusted_for_scope(scope)?;
            return Ok(join(&[
                &self.cwd,
                kiana_core::config::CONFIG_DIR_NAME,
                "npm",
            ]));
        }
        Ok(join(&[&self.agent_dir, "npm"]))
    }

    /// `getGitInstallRoot(scope)`。
    ///
    /// `temporary` 返回 `None`（临时 git 安装不走“安装根”）；`project` 先过 trust 检查，
    /// 返回 `<cwd>/.kiana/git`；`user` 返回 `<agent_dir>/git`。返回值只做词法拼接，不创建
    /// 目录。
    pub(crate) fn get_git_install_root(
        &self,
        scope: SourceScope,
    ) -> Result<Option<String>, PackageManagerError> {
        if scope == SourceScope::Temporary {
            return Ok(None);
        }
        if scope == SourceScope::Project {
            self.assert_project_trusted_for_scope(scope)?;
            return Ok(Some(join(&[
                &self.cwd,
                kiana_core::config::CONFIG_DIR_NAME,
                "git",
            ])));
        }
        Ok(Some(join(&[&self.agent_dir, "git"])))
    }

    /// `getManagedNpmInstallPath(source, scope)`。
    ///
    /// 布局为 `<npm 根>/node_modules/<包名>`。`scope` 决定 npm 根：`temporary` 用
    /// `get_temporary_dir("npm")`，`project` 先过 trust 检查再用 `<cwd>/.kiana/npm`，其余
    /// 用 `<agent_dir>/npm`。只做词法拼接，不创建目录，也不检查包是否真的安装。
    pub(crate) fn get_managed_npm_install_path(
        &self,
        source: &NpmSource,
        scope: SourceScope,
    ) -> Result<String, PackageManagerError> {
        if scope == SourceScope::Temporary {
            return Ok(join(&[
                &self.get_temporary_dir("npm", None, None)?,
                "node_modules",
                &source.name,
            ]));
        }
        if scope == SourceScope::Project {
            self.assert_project_trusted_for_scope(scope)?;
            return Ok(join(&[
                &self.cwd,
                kiana_core::config::CONFIG_DIR_NAME,
                "npm",
                "node_modules",
                &source.name,
            ]));
        }
        Ok(join(&[
            &self.agent_dir,
            "npm",
            "node_modules",
            &source.name,
        ]))
    }

    /// `getNpmInstallPath(source, scope)`：user 作用域下 managed 路径不存在时回退到遗留全局安装。
    ///
    /// 回退只发生在 `scope == User`：先按 `get_managed_npm_install_path` 得到 managed
    /// 路径，若它存在（[`path_exists`] 会跟随符号链接）就直接返回；否则查询
    /// 遗留的全局 npm/pnpm 安装，只有回退路径也存在才使用它，两者都不存在时仍返回 managed
    /// 路径。遗留查询可能执行 `npm root -g` / `npm list -g` 子进程（见 `npm` 子模块），
    /// 因此本方法不是纯路径计算。
    pub(crate) fn get_npm_install_path(
        &self,
        source: &NpmSource,
        scope: SourceScope,
    ) -> Result<String, PackageManagerError> {
        let managed_path = self.get_managed_npm_install_path(source, scope)?;
        if scope != SourceScope::User || path_exists(&managed_path) {
            return Ok(managed_path);
        }
        let legacy_path = self.get_legacy_global_npm_install_path(source);
        match legacy_path {
            Some(path) if path_exists(&path) => Ok(path),
            _ => Ok(managed_path),
        }
    }

    /// `getGitInstallPath(source, scope)`。
    ///
    /// `temporary` 走 `get_temporary_dir`：prefix 为 `git-<host>`、suffix 为
    /// `source.path`、ref 参与 hash，因此同一仓库的不同 ref 有独立 checkout。
    /// 其它作用域先取 `get_git_install_root`，再用 `resolve_managed_path(root, [host, path])`
    /// 拼出路径；`host`/`path` 中的 `..` 会被越界检查拒绝。
    ///
    /// `get_git_install_root` 返回 `None` 时返回 `Missing git install root`。当前三个作用域
    /// 中只有 `Temporary` 会让根为 `None`，而它在前面已提前返回，所以该分支目前不可达，
    /// 属于防御性代码。
    pub(crate) fn get_git_install_path(
        &self,
        source: &kiana_core::git::GitSource,
        scope: SourceScope,
    ) -> Result<String, PackageManagerError> {
        if scope == SourceScope::Temporary {
            // 把 ref 纳入 hash，使每个固定 ref 有自己的 checkout。
            return self.get_temporary_dir(
                &format!("git-{}", source.host),
                Some(&source.path),
                source.r#ref.as_deref(),
            );
        }
        let install_root = self.get_git_install_root(scope)?;
        let Some(install_root) = install_root else {
            return Err(err("Missing git install root"));
        };
        self.resolve_managed_path(&install_root, &[&source.host, &source.path])
    }

    /// `getTemporaryDir(prefix, suffix?, ref?)`。
    ///
    /// 布局为 `<agent_dir>/tmp/extensions/<prefix>/<hash8>[/<suffix>]`，其中 `hash8` 是
    /// `sha256("<prefix>-<suffix>@<ref>")` 的前 8 个十六进制字符（`ref` 为 `None` 时没有
    /// `@` 部分）。两段都经过 `resolve_managed_path`，因此 `suffix` 里的 `..` 无法逃出
    /// `<prefix>` 目录；`suffix` 为空串时空段被 [`path_resolve`] 跳过，结果就是
    /// `<prefix>/<hash8>`。
    ///
    /// 副作用：会先确保 `<agent_dir>/tmp/extensions` 存在（见
    /// [`get_extension_temp_folder`]），也就是说即使是查询临时路径也会创建目录。
    pub(crate) fn get_temporary_dir(
        &self,
        prefix: &str,
        suffix: Option<&str>,
        r#ref: Option<&str>,
    ) -> Result<String, PackageManagerError> {
        let temp_folder = get_extension_temp_folder(&self.agent_dir)?;
        let root = self.resolve_managed_path(&temp_folder, &[prefix])?;
        let input = format!(
            "{prefix}-{}{}",
            suffix.unwrap_or(""),
            match r#ref {
                Some(value) => format!("@{value}"),
                None => String::new(),
            }
        );
        let hash = kiana_ai::model_catalog::sha256_hex(&input);
        let hash = &hash[..hash.len().min(8)];
        self.resolve_managed_path(&root, &[hash, suffix.unwrap_or("")])
    }

    /// `resolveManagedPath(root, ...parts)`：拒绝逃出安装根的路径。
    ///
    /// # 检查流程
    ///
    /// 1. `root` 先经 [`path_resolve`] 归一化；所有 `parts` 拼在根后面再整体归一化一次。
    /// 2. 结果必须等于根，或以 `根 + 分隔符` 开头；否则返回
    ///    `Refusing to use path outside package install root: <resolved>`。
    ///
    /// # 边界
    ///
    /// - `parts` 为空时返回归一化后的根。
    /// - 这是**词法**检查：`path_resolve` 不访问文件系统、不解析符号链接；根内的符号链接
    ///   指向根外不会被发现，需要真实路径约束时调用方要另行 canonicalize。
    /// - 前缀比较带分隔符，避免 `/root-other` 被误判为 `/root` 的子路径。
    /// - 检查只看归一化后的字符串，不要求路径存在，也不创建目录。
    pub(crate) fn resolve_managed_path(
        &self,
        root: &str,
        parts: &[&str],
    ) -> Result<String, PackageManagerError> {
        let resolved_root = path_resolve(&[root]);
        let mut all: Vec<&str> = Vec::with_capacity(parts.len() + 1);
        all.push(&resolved_root);
        all.extend_from_slice(parts);
        let resolved_path = path_resolve(&all);
        if resolved_path != resolved_root
            && !resolved_path.starts_with(&format!("{resolved_root}{}", sep()))
        {
            return Err(err(format!(
                "Refusing to use path outside package install root: {resolved_path}"
            )));
        }
        Ok(resolved_path)
    }

    /// `resolvePath(input)`：以 `cwd` 为基准、`HOME || homedir()` 展开 `~`、trim。
    ///
    /// 选项为 `trim: true`、`home_dir` 取 `exec::get_home_dir()`（`HOME` 优先），其余用
    /// `PathInputOptions` 默认值（因此 `~` 会展开）。不检查结果是否存在。
    pub(crate) fn resolve_path(&self, input: &str) -> String {
        self.resolve_path_from_base(input, &self.cwd)
    }

    /// `resolvePathFromBase(input, baseDir)`。
    ///
    /// 与 `resolve_path` 相同的选项，但基准目录由调用方给出（local 源在 project 作用域
    /// 用 `<cwd>/.kiana`，user 用 `agent_dir`）。`base_dir` 为空或相对时，底层
    /// `kiana_core::paths::resolve_path` 会以进程 cwd 作为最终基准；本方法不校验这一点。
    pub(crate) fn resolve_path_from_base(&self, input: &str, base_dir: &str) -> String {
        let options = kiana_core::paths::PathInputOptions {
            trim: true,
            home_dir: Some(std::path::PathBuf::from(super::exec::get_home_dir())),
            ..kiana_core::paths::PathInputOptions::default()
        };
        kiana_core::paths::resolve_path(input, base_dir, &options)
    }

    /// `ensureNpmProject(installRoot)`。
    ///
    /// 步骤固定：
    /// 1. 根目录不存在时 `create_dir_all`（与 `get_extension_temp_folder` 不同，这里不设置
    ///    权限）；
    /// 2. `mark_path_ignored_by_cloud_sync`：在 macOS/Linux 上尝试给目录设置云同步忽略
    ///    属性，失败被静默忽略（底层不检查子进程结果）；
    /// 3. 确保目录内有 `.gitignore`（见 [`DefaultPackageManager::ensure_git_ignore`]）；
    /// 4. 没有 `package.json` 时写入固定内容：两空格缩进、键顺序为 `name` 再 `private`、
    ///    **没有结尾换行**；已存在则完全不读也不改。
    ///
    /// 失败来自 mkdir、写 `.gitignore` 与写 `package.json`，错误文本均用
    /// [`format_fs_error`] 生成，`syscall` 为 `mkdir` / `open`。
    pub(crate) fn ensure_npm_project(&self, install_root: &str) -> Result<(), PackageManagerError> {
        if !path_exists(install_root) {
            std::fs::create_dir_all(install_root)
                .map_err(|error| err(format_fs_error(&error, "mkdir", install_root)))?;
        }
        kiana_core::paths::mark_path_ignored_by_cloud_sync(install_root);
        self.ensure_git_ignore(install_root)?;
        let package_json_path = join(&[install_root, "package.json"]);
        if !path_exists(&package_json_path) {
            // JSON.stringify({ name: "kiana-extensions", private: true }, null, 2)，无结尾换行。
            const CONTENT: &str = "{\n  \"name\": \"kiana-extensions\",\n  \"private\": true\n}";
            std::fs::write(&package_json_path, CONTENT)
                .map_err(|error| err(format_fs_error(&error, "open", &package_json_path)))?;
        }
        Ok(())
    }

    /// `ensureGitIgnore(dir)`。
    ///
    /// 目录不存在时先 `create_dir_all`；`.gitignore` 不存在时写入逐字内容
    /// `"*\n!.gitignore\n"`（忽略目录内一切，但保留该文件本身），已存在则不改动。写入失败
    /// 的错误文本由 [`format_fs_error`] 生成，`syscall` 为 `open`。
    pub(crate) fn ensure_git_ignore(&self, dir: &str) -> Result<(), PackageManagerError> {
        if !path_exists(dir) {
            std::fs::create_dir_all(dir)
                .map_err(|error| err(format_fs_error(&error, "mkdir", dir)))?;
        }
        let ignore_path = join(&[dir, ".gitignore"]);
        if !path_exists(&ignore_path) {
            std::fs::write(&ignore_path, "*\n!.gitignore\n")
                .map_err(|error| err(format_fs_error(&error, "open", &ignore_path)))?;
        }
        Ok(())
    }

    /// 供 `npm.rs` 使用的全局 npm 根缓存。
    ///
    /// 返回内部 `Mutex` 的引用，由 `npm::get_global_npm_root` 读写；锁必须经 `npm::lock`
    /// 获取（互斥量中毒时取回内部值），本方法本身不加锁。
    pub(crate) fn npm_root_cache(&self) -> &Mutex<NpmRootCache> {
        &self.global_npm_root
    }
}

/// `existsSync(path)`：跟随符号链接（断链返回 `false`，与 Node 一致）。
///
/// 实现是 `Path::exists()`：任何导致元数据读取失败的原因（目标不存在、链接断开、权限
/// 不足、父路径不是目录等）都折叠成 `false`，不返回错误；调用方无法区分“不存在”和
/// “无法判断”。
pub(crate) fn path_exists(path: &str) -> bool {
    std::path::Path::new(path).exists()
}

// 测试只覆盖纯函数与不依赖真实 npm 的路径方法； fixture 一律用 `tempfile::tempdir`。
// 词法函数（`join` / `relative` / `dirname` / `basename`）的用例取值与 Node `lib/path.js`
// 的实际行为一致（待核实：是否与某组官方样例逐字对应，仓库内未找到出处），作为
// “移植是否走样”的回归基线；`fs_error_messages_use_platform_errno_names`
// 锁定 `format_fs_error` 的 errno → 文案映射。需要真实子进程的 npm 交互在 `npm.rs`
// 自己的测试模块里。
#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(windows))]
    #[test]
    fn join_and_relative_match_node() {
        assert_eq!(join(&["/a", "b", "c"]), "/a/b/c");
        assert_eq!(join(&["/a/", "b/"]), "/a/b/");
        assert_eq!(join(&["/a", "..", "b"]), "/b");
        assert_eq!(relative("/a/b", "/a/c"), "../c");
        assert_eq!(relative("/a/b", "/a/b"), "");
        assert_eq!(relative("/a/b", "/a/b/c"), "c");
        assert_eq!(relative("/a", "/a/b"), "b");
        assert_eq!(relative("/a/b", "/a"), "..");
    }

    #[cfg(not(windows))]
    #[test]
    fn dirname_and_basename_match_node() {
        assert_eq!(dirname("/a/b/c"), "/a/b");
        assert_eq!(dirname("/a"), "/");
        assert_eq!(dirname("a"), ".");
        assert_eq!(dirname("/"), "/");
        assert_eq!(basename("/a/b/c.ts"), "c.ts");
        assert_eq!(basename("/a/b/"), "b");
        assert_eq!(basename("/"), "");
        assert_eq!(basename("a"), "a");
    }

    /// Windows 语义的 dirname/basename 在任意平台都跑（纯字符串算法）。
    #[test]
    fn win32_dirname_and_basename_match_node() {
        let dirname_cases = [
            ("C:\\a", "C:\\"),
            ("C:\\", "C:\\"),
            ("C:", "C:"),
            ("C:a", "C:"),
            ("C:a\\b", "C:a"),
            ("C:\\a\\b", "C:\\a"),
            ("C:\\a\\b\\c", "C:\\a\\b"),
            ("\\\\srv\\share\\a", "\\\\srv\\share\\"),
            ("\\\\srv\\share", "\\\\srv\\share"),
            ("\\\\?\\C:\\a", "\\\\?\\C:\\"),
            ("C:/Users/x", "C:/Users"),
            ("/a/b", "/a"),
            ("a/b/", "a"),
            ("C:/", "C:/"),
            ("\\\\srv", "\\"),
            ("\\", "\\"),
        ];
        for (path, expected) in dirname_cases {
            assert_eq!(win32_dirname(path), expected, "dirname({path:?})");
        }
        let basename_cases = [
            ("C:\\", ""),
            ("C:", ""),
            ("C:a", "a"),
            ("C:/Users/x/file", "file"),
            ("a/b/", "b"),
            ("\\\\srv\\share", "share"),
            ("\\\\srv\\share\\", "share"),
            ("C:a/b", "b"),
            ("a:", ""),
            ("\\\\?\\C:\\a", "a"),
            ("\\", ""),
            ("\\\\srv", "srv"),
        ];
        for (path, expected) in basename_cases {
            assert_eq!(win32_basename(path), expected, "basename({path:?})");
        }
    }

    /// Windows 语义的 relative：大小写不敏感、不同盘符/UNC 返回 `to` 原文。
    #[test]
    fn win32_relative_matches_node() {
        let cases = [
            ("C:\\A", "C:\\a\\b", "b"),
            ("C:\\a", "C:\\A", ""),
            ("C:\\proj", "D:\\libs\\pkg", "D:\\libs\\pkg"),
            ("C:\\", "C:\\a", "a"),
            ("\\\\srv\\share\\", "C:\\a", "C:\\a"),
            ("C:\\a", "C:\\a\\b", "b"),
            ("C:\\a\\", "C:\\a\\b", "b"),
            ("\\\\srv\\share\\", "\\\\srv\\share\\x", "x"),
            ("\\\\srv\\share\\", "\\\\srv\\other\\x", "..\\other\\x"),
        ];
        for (from, to, expected) in cases {
            assert_eq!(
                win32_relative_resolved(from, to),
                expected,
                "relative({from:?}, {to:?})"
            );
        }
    }

    /// Windows 语义的 normalize：裸盘符、UNC 根、设备根与 CVE-2024-36139 前缀。
    #[test]
    fn win32_normalize_matches_node() {
        let cases = [
            ("C:", "C:."),
            ("C:\\", "C:\\"),
            ("C:\\a", "C:\\a"),
            ("C:\\a\\..\\..", "C:\\"),
            ("C:a\\..", "C:."),
            ("C:..", "C:.."),
            ("foo:/bar", ".\\foo:\\bar"),
            ("packages:", ".\\packages:"),
            ("//server//share/dir", "\\\\server\\share\\dir"),
            ("///srv/share/f", "\\srv\\share\\f"),
            ("\\\\srv\\share", "\\\\srv\\share\\"),
            ("\\\\srv\\share\\a\\..", "\\\\srv\\share\\"),
            ("\\\\?\\C:", "\\\\?\\C:"),
            ("\\\\?\\C:\\a\\..", "\\\\?\\C:"),
            ("\\", "\\"),
            ("\\\\", "\\"),
        ];
        for (path, expected) in cases {
            assert_eq!(win32_normalize(path), expected, "normalize({path:?})");
        }
    }

    fn test_manager(root: &std::path::Path) -> super::super::DefaultPackageManager {
        super::super::DefaultPackageManager::new(super::super::PackageManagerOptions {
            cwd: root.to_string_lossy().into_owned(),
            agent_dir: root.join("agent").to_string_lossy().into_owned(),
            settings_manager: kiana_config::settings::SettingsManager::in_memory(
                kiana_config::settings::Settings::default(),
            ),
            builtin_extensions: vec![],
        })
    }

    #[test]
    fn get_extension_temp_folder_creates_the_directory_with_0700() {
        let dir = tempfile::tempdir().expect("tempdir");
        let agent_dir = dir.path().join("agent");
        let temp_folder = get_extension_temp_folder(&agent_dir.to_string_lossy()).expect("temp");
        assert_eq!(
            temp_folder,
            join(&[&agent_dir.to_string_lossy(), "tmp", "extensions"])
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&temp_folder)
                .expect("temp folder")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o700);
        }
    }

    #[test]
    fn temporary_npm_install_path_lives_under_agent_temp_folder_with_0700() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manager = test_manager(dir.path());
        let source = super::super::parse::parse_source("npm:left-pad");
        let super::super::parse::ParsedSource::Npm(npm) = source else {
            panic!("npm source");
        };
        let install_path = manager
            .get_npm_install_path(&npm, SourceScope::Temporary)
            .expect("temporary path");
        assert!(install_path.ends_with("node_modules/left-pad"));
        let temp_root = super::join(&[manager.agent_dir(), "tmp", "extensions"]);
        assert!(!relative(&temp_root, &install_path).starts_with(".."));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&temp_root)
                .expect("temp root")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o700);
        }
    }

    #[test]
    fn git_install_paths_refuse_to_escape_the_install_root() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manager = test_manager(dir.path());
        let source = kiana_core::git::GitSource {
            source_type: "git".to_string(),
            repo: "git@evil.example:../../victim/repo".to_string(),
            host: "evil.example".to_string(),
            path: "../../victim/repo".to_string(),
            r#ref: None,
            pinned: false,
        };
        for scope in [
            SourceScope::User,
            SourceScope::Project,
            SourceScope::Temporary,
        ] {
            let error = manager
                .get_git_install_path(&source, scope)
                .expect_err("traversal must be rejected");
            assert!(
                error.message().contains("outside package install root"),
                "scope {scope:?}: {}",
                error.message()
            );
        }
    }

    #[test]
    fn fs_error_messages_use_platform_errno_names() {
        #[cfg(target_os = "macos")]
        let eloop = 62;
        #[cfg(not(target_os = "macos"))]
        let eloop = 40;
        let error = std::io::Error::from_raw_os_error(eloop);
        assert_eq!(
            format_fs_error(&error, "open", "/x"),
            "ELOOP: too many symbolic links encountered, open '/x'"
        );
        let error = std::io::Error::from_raw_os_error(2);
        assert_eq!(
            format_fs_error(&error, "mkdir", "/x"),
            "ENOENT: no such file or directory, mkdir '/x'"
        );
    }

    #[cfg(unix)]
    #[test]
    fn extension_temp_folder_applies_0700_to_every_created_level() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let agent_dir = dir.path().join("nested/deep/agent");
        let folder = get_extension_temp_folder(&agent_dir.to_string_lossy()).expect("temp folder");
        assert_eq!(
            folder,
            agent_dir
                .join("tmp/extensions")
                .to_string_lossy()
                .into_owned()
        );
        for level in [
            agent_dir.clone(),
            agent_dir.join("tmp"),
            agent_dir.join("tmp/extensions"),
        ] {
            let mode = std::fs::metadata(&level)
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o700, "mode of {}", level.to_string_lossy());
        }
    }

    #[test]
    fn js_string_cmp_orders_by_utf16_units() {
        // U+1F600 is a surrogate pair whose first unit (0xD83D) is smaller than U+FFFD
        // (0xFFFD), so in UTF-16 code-unit order the emoji sorts first.
        assert_eq!(
            js_string_cmp("\u{FFFD}", "\u{1F600}"),
            std::cmp::Ordering::Greater
        );
        assert_eq!(js_string_cmp("a", "b"), std::cmp::Ordering::Less);
        assert_eq!(js_string_cmp("a", "a"), std::cmp::Ordering::Equal);
    }

    #[cfg(windows)]
    #[test]
    fn windows_drive_absolute_paths_are_not_resolved_against_cwd() {
        assert!(is_absolute("C:\\a"));
        assert!(is_absolute("C:/a"));
        assert!(!is_absolute("C:"));
        assert!(!is_absolute("C:a"));
        assert_eq!(normalize("C:\\a\\.."), "C:\\");
        assert_eq!(normalize("C:\\..\\a"), "C:\\a");
        assert_eq!(
            path_resolve_with_cwd(&["C:\\root", "sub"], "D:\\cwd"),
            "C:\\root\\sub"
        );
        assert_eq!(
            path_resolve_with_cwd(&["C:\\root", "..", "sub"], "D:\\cwd"),
            "C:\\sub"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_unc_roots_are_pinned() {
        assert_eq!(normalize("\\\\server\\share"), "\\\\server\\share\\");
        assert_eq!(normalize("\\\\server\\share\\.."), "\\\\server\\share\\");
        assert_eq!(
            normalize("\\\\server\\share\\a\\..\\..\\x"),
            "\\\\server\\share\\x"
        );
        assert_eq!(
            path_resolve_with_cwd(&["\\\\server\\share\\a", "..", "..", "x"], "C:\\cwd"),
            "\\\\server\\share\\x"
        );
        assert_eq!(
            path_resolve_with_cwd(&["\\\\server\\share"], "C:\\cwd"),
            "\\\\server\\share\\"
        );
        // 重复分隔符按 Node 折叠，但仍识别为 UNC 根（`//server//share` 是合法 UNC）。
        assert_eq!(normalize("//server//share/dir"), "\\\\server\\share\\dir");
        assert_eq!(normalize("\\\\server\\\\share"), "\\\\server\\share\\");
        // 只有 server 或三个前导分隔符都不是 UNC，按 rooted path 处理。
        assert_eq!(normalize("\\\\server"), "\\server");
        assert_eq!(normalize("///srv/share/f"), "\\srv\\share\\f");
    }
}
