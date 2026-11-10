//! `node:path.win32` 的函数级移植（与 [`super::posix`] 同为唯一实现，见 `ARCHITECTURE.md` §4.9 / §8 第 23 条）。
//!
//! 逐函数对齐 Node 的 `lib/path.js`（Node 26）：盘符（`C:`）、UNC（`\\server\share`）、设备路径
//! （`\\.\`、`\\?\`）、盘符相对路径（`C:foo`）、保留设备名（`CON`/`COM1`/…，含 CVE-2024-36139 的
//! `.\` 前缀规则）、大小写保留（比较用 lowercase）、`/` 与 `\` 都接受为分隔符。
//!
//! **运行平台**：`resolve` 的 cwd 回落与 Node 的 `isWindows` 分支一致（Linux 上把 cwd 的 `/` 换成 `\`），
//! 用 `cfg!(windows)` 表达。
//!
//! # 这个文件解决什么问题
//!
//! Rust 的 `std::path` 面向宿主平台，不提供 JS `path.win32` 的纯字符串语义：`normalize` 的 `..`
//! 折叠、盘符相对路径（`C:foo`）、尾随分隔符保留、`//` 折叠，以及用 `.\` 前缀阻止结果被 Windows
//! 当作绝对路径。这里按 Node 的算法逐函数移植，目标是同一输入得到逐字节相同的字符串；已知
//! 例外是 [`relative_with_cwd`] 在截取落在代理对中间时用 U+FFFD 替代 Node 的孤立代理项
//! （Rust 的 `String` 无法表示，详见该函数文档）。
//!
//! # 在系统中的位置
//!
//! 调用方：`env::node` 的平台分派（`cfg!(windows)` 时选 win32）、`env::node_watch` 的相对路径
//! 计算，以及 `kiana-env-client` 的 `PathFlavor::Win32`（远端 daemon 是 Windows 时）。黄金测试
//! `path/mod.rs` 的 `golden` 用 `tests/fixtures/path_corpus.json`（Node 26 生成）逐例对拍。
//!
//! # 主要流程：根匹配
//!
//! `resolve` 与 `normalize` 的根匹配结构相近（`normalize` 多一个保留设备名分支，见下）：
//! 先确定 `device`（盘符 `C:` 或 UNC `\\server\share`）、根长度 `root_end` 与 `is_absolute`，
//! 再把剩余尾部交给 `posix::normalize_string`（分隔符换成 `\`）折叠 `.`/`..`。根的形式按
//! 优先级是：
//! 1. 单字符分隔符（`resolve` 视为根；`normalize` 直接返回 `\`）；
//! 2. 前两个字节都是分隔符：`\\server\share` 是 UNC 设备；`\\.\`/`\\?\` 是设备命名空间，
//!    根固定占 4 字节，`normalize` 还会把紧随其后的保留设备名（如 `\\?\CON:`）整段并入设备；
//! 3. `[A-Za-z]:`：盘符；第三个字节是分隔符才是绝对路径，否则是盘符相对（`C:foo`）；
//! 4. 非分隔符开头且冒号前是保留设备名（**仅 `normalize`**，如 `CON:`、`COM1:`）：名字连同
//!    冒号作为 `device`，根长为冒号 + 1；`resolve` 没有这一分支，同样输入按相对尾部处理；
//! 5. 其余：没有设备，根长为 0。
//!
//! `dirname` 只对 UNC/盘符两种前缀做简化扫描（不构造 `device` 字符串），`basename` 只跳过
//! 两字节盘符；两者都不折叠 `..`。
//!
//! # 重要约束
//!
//! - **输出由语料锁定，不是"大致兼容"**：语料覆盖尾随 `\`、`.\` 前缀、`CON:` 等
//!   保留设备名的输出，改动任何分支前先核对 corpus。已知例外：`relative` 在代理对中间截断时
//!   用 U+FFFD 顶替 Node 的孤立代理项（Rust 的 `String` 无法表示），见 [`relative_with_cwd`]。
//! - 输入按 UTF-8 字节扫描。大部分判定只比较 `\`、`/`、`:` 与 ASCII 字母，切片点落在
//!   分隔符或字符串两端；`basename` 的后缀比较逐字节进行，`relative` 则按 UTF-16 码元比较、
//!   最后用 `utf16_slice` 换算回字节下标。
//! - 除默认包装 `resolve_string` / `relative` 读取一次进程 cwd、以及解析盘符相对路径时
//!   查询一次同名环境变量（`=C:`）外，全部是纯字符串运算：不检查路径是否存在、不解析符号
//!   链接。
//! - `resolve_string_with_cwd` 与 `relative_with_cwd` 要求 `cwd` 非空：`cwd` 为空时，相对
//!   参数、无盘符的根相对参数（如 `\a`）、以及 `=C:` 未设置时的盘符相对参数（如 `C:a`）都会
//!   让根匹配索引 `path.as_bytes()[0]` 而 panic；带设备的绝对路径（`C:\a`、UNC）会在扫描中
//!   提前 `break`，`parts` 为空时走开头的快路径直接返回，这两类不受影响。默认包装
//!   `resolve_string` / `relative` 用 `win32_cwd()`（失败回退 `\`），直接调用 `*_with_cwd`
//!   的代码需自行保证。

use std::path::PathBuf;

use super::posix::normalize_string;

/// `path.win32.sep`。
pub const SEP: char = '\\';

const CHAR_FORWARD_SLASH: u8 = b'/';
const CHAR_BACKWARD_SLASH: u8 = b'\\';
const CHAR_COLON: u8 = b':';

/// Node 的 `isPathSeparator`：Windows 上 `/` 与 `\` 都算分隔符（非 ASCII 字节不会命中）。
fn is_path_separator(code: u8) -> bool {
    code == CHAR_FORWARD_SLASH || code == CHAR_BACKWARD_SLASH
}

/// Node 的 `isWindowsDeviceRoot`：盘符只认 ASCII 字母，`é:`、`_:` 都不是设备根。
fn is_windows_device_root(code: u8) -> bool {
    code.is_ascii_uppercase() || code.is_ascii_lowercase()
}

/// Node 的 `WINDOWS_RESERVED_NAMES`。
const WINDOWS_RESERVED_NAMES: &[&str] = &[
    "CON",
    "PRN",
    "AUX",
    "NUL",
    "COM1",
    "COM2",
    "COM3",
    "COM4",
    "COM5",
    "COM6",
    "COM7",
    "COM8",
    "COM9",
    "LPT1",
    "LPT2",
    "LPT3",
    "LPT4",
    "LPT5",
    "LPT6",
    "LPT7",
    "LPT8",
    "LPT9",
    "COM\u{b9}",
    "COM\u{b2}",
    "COM\u{b3}",
    "LPT\u{b9}",
    "LPT\u{b2}",
    "LPT\u{b3}",
];

/// `String.prototype.slice(0, end)` 的等价物（`end` 为负时从末尾计数）。
///
/// 按字节取前缀后再退到 UTF-8 字符边界，保证返回的 `&str` 合法；`end` 超出长度时夹紧。
/// `is_windows_reserved_name` 依赖负 `end` 的 JS `slice(0, -1)` 语义。
fn slice_to(path: &str, end: isize) -> &str {
    let len = path.len() as isize;
    let end = if end < 0 {
        (len + end).max(0)
    } else {
        end.min(len)
    } as usize;
    let mut end = end.min(path.len());
    while end > 0 && !path.is_char_boundary(end) {
        end -= 1;
    }
    &path[..end]
}

/// Node 的 `isWindowsReservedName(path, colonIndex)`。
///
/// 取 `path[..colon_index]` 大写后与保留名表比较（大小写不敏感）。注意 `colon_index < 0` 时
/// `slice_to` 按 JS 负索引语义从末尾计数：例如 `CONx` 会截成 `CON` 而命中，这不是笔误，
/// 改前先对照 Node 的输出。
fn is_windows_reserved_name(path: &str, colon_index: isize) -> bool {
    let device_part = slice_to(path, colon_index).to_uppercase();
    WINDOWS_RESERVED_NAMES.contains(&device_part.as_str())
}

/// 进程当前目录；取不到时回退到根 `\`。
///
/// Node 的 `process.cwd()` 在这种情况会抛错，Rust 侧没有异常通道，而错误模型不允许用 panic
/// 表达环境失败（与 `posix` 侧回退 `/` 同理），因此返回一个仍然绝对、归一化结果确定的值。
fn win32_cwd() -> String {
    match std::env::current_dir() {
        Ok(path) => path.to_string_lossy().into_owned(),
        Err(_) => "\\".to_owned(),
    }
}

/// `path.win32.resolve(...parts)`；返回 [`PathBuf`] 以匹配骨架冻结的签名。
///
/// 从右往左扫描 `parts`：一旦某段被判定为绝对，它左边的段就不再贡献尾部；带设备的绝对段
/// （`C:\...`、UNC）还会结束扫描（若扫描已处于绝对状态，该段只贡献设备、其尾部被丢弃）。
/// 无设备的绝对段（如 `\foo`）不会结束扫描，盘符由左侧参数或 cwd 提供；只有所有参数都
/// 不是绝对段时才整体以 cwd 为根（盘符相对段 `C:a` 也会用 cwd 或环境变量 `=C:` 补全）。
/// 结果已归一化（`.`/`..` 与分隔符都已折叠）。`PathBuf` 只是包装，**不会**按 Rust 的路径
/// 规则再做处理；除经 `resolve_string` 取一次进程 cwd 外不访问文件系统（因此不解析符号
/// 链接、不检查存在性）。
///
/// 更精确的语义见 [`resolve_string_with_cwd`]。
pub fn resolve(parts: &[&str]) -> PathBuf {
    PathBuf::from(resolve_string(parts))
}

/// [`resolve`] 的字符串形式（供 [`relative`] 复用）。
///
/// 使用进程 cwd 与编译目标平台的 `cfg!(windows)`；cwd 取不到时由 `win32_cwd` 回退 `\`，
/// 不会 panic。
pub fn resolve_string(parts: &[&str]) -> String {
    resolve_string_with_cwd(parts, &win32_cwd(), cfg!(windows))
}

/// [`resolve_string`]，但 cwd 与 `isWindows` 由调用方给出（等价于 Node 里覆盖 `process.cwd` /
/// 平台；黄金测试用它固定与平台无关的期望值）。
///
/// # 参数
///
/// - `cwd`：没有带设备的绝对段时的根回退——完全相对路径整体接在 `cwd` 下，根相对/盘符
///   相对路径用 `cwd` 补盘符或补全；对应 Node 的 `process.cwd()`；必须非空。
/// - `is_windows`：只影响快路径里是否把 cwd 的 `/` 换成 `\`（Node 的 `isWindows`），**不会**
///   切换成 posix 语义；黄金测试传 `false` 以复现 Linux 上生成的期望值。
///
/// # 结果
///
/// 绝对路径拼成 `device\tail`；`device` 为空但 `is_absolute` 为真时（如根相对路径 `\foo`
/// 配上没有盘符的 cwd）输出 `\tail`。只有相对结果才拼成 `device+tail`；盘符相对参数
/// （`C:a`）会继续用 cwd 补全，例如 `=C:` 未设置、cwd 为 `/cwd` 时 `C:a` 得到 `C:\cwd\a`，
/// 而不是 `C:a`（corpus 中同一分支的 `["C:a", "b/c"]` 结果为 `C:\cwd\a\b\c`，单参数结果由
/// 该分支推得）。结果为空时返回 `.`。所有段最后统一折叠 `.`/`..`。
///
/// # Panics
///
/// `cwd` 为空时，相对路径（如 `["foo"]`）、无盘符的根相对路径（如 `["\a"]`）、以及
/// `=C:` 环境变量未设置时的盘符相对路径（如 `["C:a"]`）都会在根匹配处索引空字符串而 panic；
/// 带设备的绝对路径（`C:\a`、UNC）会提前 `break`，`parts` 为空时走开头的快路径直接返回，
/// 这两类不受影响。直接调用本函数的代码必须保证 `cwd` 非空。
pub fn resolve_string_with_cwd(parts: &[&str], cwd: &str, is_windows: bool) -> String {
    let mut resolved_device = String::new();
    let mut resolved_tail = String::new();
    let mut resolved_absolute = false;

    // 从右往左扫描：一旦某段被判定为绝对，它左边的段就不再贡献尾部；带设备的绝对段会结束
    // 扫描（扫描已在绝对状态时只贡献设备）。无设备的绝对段（`\foo`）不结束扫描，盘符由
    // 左侧参数或 cwd 提供；空参数按 Node 的语义跳过。
    let mut i = parts.len() as isize - 1;
    while i >= -1 {
        let mut path: String;
        if i >= 0 {
            let arg = parts[i as usize];
            if arg.is_empty() {
                i -= 1;
                continue;
            }
            path = arg.to_owned();
        } else if resolved_device.is_empty() {
            path = cwd.to_owned();
            // 快路径（与 Node 相同）：没有参数、或只有一个空/`.` 参数，且 cwd 本身以分隔符
            // 开头时，结果就是 cwd，不必再走根匹配。非 Windows 宿主上把 `/` 换成 `\`。
            let fast = parts.is_empty()
                || (parts.len() == 1
                    && (parts[0].is_empty() || parts[0] == ".")
                    && path
                        .as_bytes()
                        .first()
                        .is_some_and(|code| is_path_separator(*code)));
            if fast {
                if !is_windows {
                    path = path.replace('/', "\\");
                }
                return path;
            }
        } else {
            // Windows 的盘符相对 cwd：`=C:` 环境变量，否则进程 cwd；不匹配该盘符时退回盘符根。
            // 这里的前导 `=` 是环境变量名的一部分（不是比较运算）；取不到时统一走 `_` 分支
            // 用调用方传入的 `cwd`。
            path = match std::env::var(format!("={resolved_device}")) {
                Ok(value) if !value.is_empty() => value,
                _ => cwd.to_owned(),
            };
            let first_two = path.get(0..2).unwrap_or("");
            let third = path.as_bytes().get(2).copied();
            if first_two.to_lowercase() != resolved_device.to_lowercase()
                && third == Some(CHAR_BACKWARD_SLASH)
            {
                path = format!("{resolved_device}\\");
            }
        }

        let len = path.len();
        let mut root_end = 0usize;
        let mut device: Option<String> = None;
        let mut is_absolute = false;
        let code = path.as_bytes()[0];

        // 尝试匹配根：确定根占多少字节（root_end）、设备名（device）与是否绝对（is_absolute）。
        // 边界条件与 Node 一致，改动会影响所有 resolve 的结果。
        if len == 1 {
            // 单字符：只有分隔符算根，其它字符是纯相对路径。
            if is_path_separator(code) {
                root_end = 1;
                is_absolute = true;
            }
        } else if is_path_separator(code) {
            // 可能是 UNC 根；不是 UNC 时就是根相对路径（`\foo`），下面把 root_end 设为 1。
            is_absolute = true;
            if is_path_separator(path.as_bytes()[1]) {
                let mut j = 2usize;
                let mut last = j;
                // 第一段：server（至少一个非分隔字符）。
                while j < len && !is_path_separator(path.as_bytes()[j]) {
                    j += 1;
                }
                if j < len && j != last {
                    let first_part = path[last..j].to_owned();
                    last = j;
                    // 跳过 server 与 share 之间的分隔符。
                    while j < len && is_path_separator(path.as_bytes()[j]) {
                        j += 1;
                    }
                    if j < len && j != last {
                        last = j;
                        // 第二段：share（至少一个非分隔字符）。
                        while j < len && !is_path_separator(path.as_bytes()[j]) {
                            j += 1;
                        }
                        if j == len || j != last {
                            if first_part != "." && first_part != "?" {
                                device = Some(format!("\\\\{first_part}\\{}", &path[last..j]));
                                root_end = j;
                            } else {
                                // `\\.\` / `\\?\` 设备命名空间：设备名只取 `\\` + firstPart，
                                // 根固定到第 4 字节（含其后的分隔符）。
                                device = Some(format!("\\\\{first_part}"));
                                root_end = 4;
                            }
                        }
                    }
                }
            } else {
                root_end = 1;
            }
        } else if is_windows_device_root(code) && path.as_bytes().get(1) == Some(&CHAR_COLON) {
            // 盘符：`C:` 是设备；第三个字节是分隔符时才是绝对路径，否则是盘符相对（`C:foo`）。
            device = Some(path[0..2].to_owned());
            root_end = 2;
            if len > 2 && is_path_separator(path.as_bytes()[2]) {
                is_absolute = true;
                root_end = 3;
            }
        }

        if let Some(device) = device {
            if !resolved_device.is_empty() {
                if device.to_lowercase() != resolved_device.to_lowercase() {
                    // 指向另一个设备，不适用。
                    // 比较不区分大小写；这一段被丢弃，但更左边的参数仍可能属于同一设备。
                    i -= 1;
                    continue;
                }
            } else {
                resolved_device = device;
            }
        }

        // 把当前段（去掉根）压到已解析尾部的左边。`resolved_tail` 始终以 `\` 连接，最后统一
        // 归一化；一旦拿到"带设备的绝对路径"，左边的参数不可能再影响结果，直接结束扫描。
        // 注意"绝对但没有设备"（根相对 `\foo`）不会提前结束：cwd 仍可能补上盘符。
        if resolved_absolute {
            if !resolved_device.is_empty() {
                break;
            }
        } else {
            resolved_tail = format!("{}\\{resolved_tail}", &path[root_end.min(len)..]);
            resolved_absolute = is_absolute;
            if is_absolute && !resolved_device.is_empty() {
                break;
            }
        }
        i -= 1;
    }

    // 只有结果仍是相对路径时才允许 `..` 越过起点；绝对路径的 `..` 不能越过根。
    let resolved_tail =
        normalize_string(&resolved_tail, !resolved_absolute, '\\', is_path_separator);

    // 绝对值一定有前导 `\`（即使设备为空）；相对值直接拼设备前缀（`C:` + `foo` = `C:foo`），
    // 全空才回退 `.`。
    if resolved_absolute {
        format!("{resolved_device}\\{resolved_tail}")
    } else {
        let result = format!("{resolved_device}{resolved_tail}");
        if result.is_empty() {
            ".".to_owned()
        } else {
            result
        }
    }
}

/// `path.win32.normalize(path)`。
///
/// 根匹配与 [`resolve`] 同源，另有下面三条规则需要注意：
/// - 保留输入末尾的分隔符（`a/` → `a\`）；
/// - 非绝对且含 `:` 的路径在可能被解释成设备/绝对路径时加 `.\` 前缀（CVE-2024-36139）；
/// - 只有 UNC 根时补一个尾随 `\`（`\\server\share` → `\\server\share\`）。
///
/// 空串返回 `.`；单字符 `/` 归一成 `\`，其余单字符原样返回。纯字符串运算，不访问文件系统。
pub fn normalize(path: &str) -> String {
    let len = path.len();
    if len == 0 {
        return ".".to_owned();
    }
    let mut root_end = 0usize;
    let mut device: Option<String> = None;
    let mut is_absolute = false;
    let code = path.as_bytes()[0];

    // 单字符提前返回：`/` 归一成 `\`，其余（包括 `C`、`.`、`\`）原样返回。
    if len == 1 {
        return if code == CHAR_FORWARD_SLASH {
            "\\".to_owned()
        } else {
            path.to_owned()
        };
    }
    if is_path_separator(code) {
        // 以分隔符开头：可能是 UNC/设备命名空间，也可能只是根相对路径。
        is_absolute = true;
        if is_path_separator(path.as_bytes()[1]) {
            let mut j = 2usize;
            let mut last = j;
            while j < len && !is_path_separator(path.as_bytes()[j]) {
                j += 1;
            }
            if j < len && j != last {
                let first_part = path[last..j].to_owned();
                last = j;
                while j < len && is_path_separator(path.as_bytes()[j]) {
                    j += 1;
                }
                if j < len && j != last {
                    last = j;
                    while j < len && !is_path_separator(path.as_bytes()[j]) {
                        j += 1;
                    }
                    if j == len || j != last {
                        if first_part == "." || first_part == "?" {
                            // 设备命名空间（`\\.\`、`\\?\`）：根固定 4 字节。若第 4 字节到
                            // 第一个冒号之间（含冒号，如 `CON:`）是保留设备名，就把它整体
                            // 并入 device，并让 root_end 覆盖到冒号之后（否则 `CON:` 会留在
                            // tail，设备只到 `\\?`）。
                            device = Some(format!("\\\\{first_part}"));
                            root_end = 4;
                            let colon_index =
                                path.find(':').map(|value| value as isize).unwrap_or(-1);
                            let possible_device = slice_range(path, 4, colon_index + 1);
                            if is_windows_reserved_name(
                                possible_device,
                                possible_device.len() as isize - 1,
                            ) {
                                device = Some(format!("\\\\?\\{possible_device}"));
                                root_end = 4 + possible_device.len();
                            }
                        } else if j == len {
                            // 只有 UNC 根：返回其归一化形式。
                            return format!("\\\\{first_part}\\{}\\", &path[last..]);
                        } else {
                            device = Some(format!("\\\\{first_part}\\{}", &path[last..j]));
                            root_end = j;
                        }
                    }
                }
            }
        } else {
            root_end = 1;
        }
    } else {
        // 非分隔符开头：先看是不是 `C:` 盘符（冒号在第 2 字节且首字节是 ASCII 字母），
        // 否则看冒号前是不是保留设备名（`CON:`、`COM1:`）；两者都要当作 device 处理。
        let colon_index = path.find(':').map(|value| value as isize).unwrap_or(-1);
        if colon_index > 0 {
            if is_windows_device_root(code) && colon_index == 1 {
                device = Some(path[0..2].to_owned());
                root_end = 2;
                if len > 2 && is_path_separator(path.as_bytes()[2]) {
                    is_absolute = true;
                    root_end = 3;
                }
            } else if is_windows_reserved_name(path, colon_index) {
                device = Some(path[..colon_index as usize + 1].to_owned());
                root_end = colon_index as usize + 1;
            }
        }
    }

    // 根之后的尾部折叠 `.`/`..`；相对路径允许越过起点（`..\a` 保留前导 `..`）。
    let mut tail = if root_end < len {
        normalize_string(&path[root_end..], !is_absolute, '\\', is_path_separator)
    } else {
        String::new()
    };
    // 只剩根且不是绝对路径时补 `.`（`C:` → `C:.`），这是 corpus 锁定的输出。
    if tail.is_empty() && !is_absolute {
        tail = ".".to_owned();
    }
    // normalize 会保留输入末尾的分隔符（`a/` → `a\`），但尾部为空时不补。
    if !tail.is_empty() && is_path_separator(path.as_bytes()[len - 1]) {
        tail.push('\\');
    }
    if !is_absolute && device.is_none() && path.contains(':') {
        // 原始路径不是绝对路径且无法解析到具体设备时，确保 tail 不会被 Windows 当作绝对路径
        // （CVE-2024-36139）。判定条件：tail 以 `X:` 开头，或某个冒号在路径末尾/后面紧跟
        // 分隔符——这类形态会被 Win32 API 解释成设备或盘符绝对路径；统一加 `.\` 固定为相对。
        if tail.len() >= 2
            && is_windows_device_root(tail.as_bytes()[0])
            && tail.as_bytes()[1] == CHAR_COLON
        {
            return format!(".\\{tail}");
        }
        let mut index = path.find(':').map(|value| value as isize);
        while let Some(current) = index {
            let next_is_separator = path
                .as_bytes()
                .get(current as usize + 1)
                .is_some_and(|code| is_path_separator(*code));
            if current as usize == len - 1 || next_is_separator {
                return format!(".\\{tail}");
            }
            index = path[current as usize + 1..]
                .find(':')
                .map(|value| value as isize + current + 1);
        }
    }
    // 保留设备名（`CON:`、`COM1:` 等）：加 `.\` 前缀，避免 Windows 把它当成设备而不是
    // 普通文件名。有冒号的命中已在前面把名字（含冒号）设为 device，这里原样拼回；无冒号的
    // 命中来自 `is_windows_reserved_name` 的负索引截断（`CONx` 截成 `CON`），此时 device
    // 为 None，用 `unwrap_or_default()` 拼出空串（这类输入本身并不是保留设备名）。
    let colon_index = path.find(':').map(|value| value as isize).unwrap_or(-1);
    if is_windows_reserved_name(path, colon_index) {
        return format!(".\\{}{tail}", device.unwrap_or_default());
    }
    match device {
        None => {
            if is_absolute {
                format!("\\{tail}")
            } else {
                tail
            }
        }
        Some(device) => {
            if is_absolute {
                format!("{device}\\{tail}")
            } else {
                format!("{device}{tail}")
            }
        }
    }
}

/// `String.prototype.slice(start, end)`（非负 start、可能为负的 end；越界夹紧）。
///
/// 只有 `normalize` 的设备命名空间检查用它（`start = 4`、`end = 第一个冒号 + 1`）。起点不在
/// 字符边界时返回空串，终点回退到字符边界，避免切出非法 UTF-8。
fn slice_range(path: &str, start: usize, end: isize) -> &str {
    let len = path.len();
    if start > len || !path.is_char_boundary(start) {
        return "";
    }
    let end = if end < 0 {
        (len as isize + end).max(0) as usize
    } else {
        (end as usize).min(len)
    };
    if end <= start {
        return "";
    }
    let mut end = end;
    while end > start && !path.is_char_boundary(end) {
        end -= 1;
    }
    &path[start..end]
}

/// `path.win32.isAbsolute(path)`：`\`、`/`、`C:\`、`C:/`、`\\server`。
///
/// 只看前三个字节，不访问文件系统：`C:`/`C:a` 是盘符相对路径，返回 `false`；根相对 `\a` 与
/// UNC 前缀 `\\server` 返回 `true`（不论路径是否存在）。
pub fn is_absolute(path: &str) -> bool {
    let len = path.len();
    if len == 0 {
        return false;
    }
    let code = path.as_bytes()[0];
    is_path_separator(code)
        || (len > 2
            && is_windows_device_root(code)
            && path.as_bytes()[1] == CHAR_COLON
            && is_path_separator(path.as_bytes()[2]))
}

/// `path.win32.join(...parts)`。
///
/// 空 `parts` 或全为空串时返回 `.`；空串参数被忽略。非空段先用 `\` 拼起来，然后：
/// 1. 若开头有多个分隔符且首段不是 UNC（`//server`），只留一个 `\`，否则 `normalize` 会把
///    普通拼接误当成 UNC 根；
/// 2. 任一分段的冒号前缀是保留设备名（`CON:` 等）时**跳过**归一化，只把 `/` 换成 `\` 后
///    返回；
/// 3. 其余情况交给 [`normalize`] 折叠 `.`/`..`。
///
/// 不访问文件系统；也不做绝对化——中间出现绝对段不会清空前面的段（那是 [`resolve`] 的职责）。
pub fn join(parts: &[&str]) -> String {
    if parts.is_empty() {
        return ".".to_owned();
    }
    let non_empty: Vec<&str> = parts
        .iter()
        .copied()
        .filter(|part| !part.is_empty())
        .collect();
    if non_empty.is_empty() {
        return ".".to_owned();
    }
    let first_part = non_empty[0];
    let mut joined = non_empty.join("\\");

    // 统计开头连续的分隔符。首段若形如 `//server`（第三个字节不是分隔符）就是真正的 UNC，
    // 原样保留；否则拼接后要把多余的前导分隔符折成一个，避免 normalize 误判成 UNC。
    let mut needs_replace = true;
    let mut slash_count = 0usize;
    if is_path_separator(first_part.as_bytes()[0]) {
        slash_count += 1;
        let first_len = first_part.len();
        if first_len > 1 && is_path_separator(first_part.as_bytes()[1]) {
            slash_count += 1;
            if first_len > 2 {
                if is_path_separator(first_part.as_bytes()[2]) {
                    slash_count += 1;
                } else {
                    // 第一部分本身就是 UNC 路径。
                    needs_replace = false;
                }
            }
        }
    }
    if needs_replace {
        while slash_count < joined.len() && is_path_separator(joined.as_bytes()[slash_count]) {
            slash_count += 1;
        }
        if slash_count >= 2 {
            joined = format!("\\{}", &joined[slash_count..]);
        }
    }

    // 设备保留名在场时跳过归一化。
    // 把 joined 按单个 `\` 切成段（连续分隔符跳过），逐段检查冒号前的名字。
    let mut parts_vec: Vec<&str> = Vec::new();
    let joined_bytes = joined.as_bytes();
    let mut part_start: Option<usize> = None;
    let mut index = 0usize;
    while index < joined_bytes.len() {
        if joined_bytes[index] == CHAR_BACKWARD_SLASH {
            if let Some(start) = part_start {
                parts_vec.push(&joined[start..index]);
            }
            part_start = None;
            while index + 1 < joined_bytes.len() && joined_bytes[index + 1] == CHAR_BACKWARD_SLASH {
                index += 1;
            }
        } else if part_start.is_none() {
            part_start = Some(index);
        }
        index += 1;
    }
    if let Some(start) = part_start {
        parts_vec.push(&joined[start..]);
    }

    let has_reserved = parts_vec.iter().any(|part| {
        part.find(':')
            .map(|colon| is_windows_reserved_name(part, colon as isize))
            .unwrap_or(false)
    });
    // 任一段命中保留设备名就不能交给 normalize：normalize 会把 `CON:` 当设备并输出
    // `.\CON:.`，而 join 的语义是普通拼接（corpus 锁定 `join("CON:", "b") == "CON:\b"`）。
    if has_reserved {
        return joined.replace('/', "\\");
    }
    normalize(&joined)
}

/// `path.win32.relative(from, to)`。
///
/// 使用进程 cwd 与编译目标平台（`cfg!(windows)`）；cwd 取不到时由 `win32_cwd` 回退 `\`。
pub fn relative(from: &str, to: &str) -> String {
    relative_with_cwd(from, to, &win32_cwd(), cfg!(windows))
}

/// [`relative`]，但 cwd 与 `isWindows` 由调用方给出（黄金测试用）。
///
/// 两侧先各自 [`resolve_string_with_cwd`] 解析后比较。默认包装的 `cwd` 来自 `win32_cwd()`
/// （绝对路径），因此相对参数通常解析成绝对路径，`cwd` 参与比较；直接传相对 `cwd` 时两侧
/// 结果可能仍是相对路径（例如 `cwd` 为 `foo` 时 `a`、`b` 分别解析为 `foo\a`、`foo\b`），
/// 比较结果不再有保证。`from`/`to` 本身是带设备的绝对路径时，该侧的 resolve 会提前
/// `break`，不依赖 `cwd`。
///
/// 约束：`cwd` 应为绝对路径；某一侧本身是带设备的绝对路径时，该侧不依赖 `cwd`（另一侧仍
/// 按上述规则参与解析）。解析后相等（包括仅大小写不同）返回空串。两侧没有任何公共分隔符
/// （典型是不同盘符）时返回 `to` 的解析结果，例如 `C:\a` 对 `D:\a` 得到 `D:\a`。
///
/// 比较分两条路径：
/// - 常规路径：按 UTF-16 码元逐位比较（Windows 路径不区分大小写），记住最后一个公共 `\`
///   的位置，前面用 `..` 逐级回退、后面接 `to` 的原始段；`i == 2` 的设备根特判依赖码元下标。
/// - `to_lowercase()` 改变 UTF-16 长度时：退回按 `\` 切段、逐段比较的简化实现。
///
/// 返回值保留 `to` 一侧的原始大小写。截取落在代理对中间时 `utf16_slice` 会用 U+FFFD 顶替
/// （Node 产生孤立代理项，Rust 的 `String` 无法表示），这是表示能力导致的差异。
///
/// # Panics
///
/// `from == to` 在解析前直接返回空串，`cwd` 为空也不会 panic。除此之外，`cwd` 为空时，
/// 相对路径、无盘符的根相对 `\a`、以及 `=C:` 未设置时的盘符相对 `C:a`，只要出现在 `from`
/// 或 `to` 任一侧，就会让该侧的 [`resolve_string_with_cwd`] panic（判定条件与该函数相同）；
/// 带设备的绝对路径（`C:\a`、UNC）那一侧会提前 `break`，不受影响。
pub fn relative_with_cwd(from: &str, to: &str, cwd: &str, is_windows: bool) -> String {
    if from == to {
        return String::new();
    }
    // 先解析成绝对、归一化后的形式；后续比较都基于它们，但返回时用 `to_orig` 的原文以保留
    // 大小写。
    let from_orig = resolve_string_with_cwd(&[from], cwd, is_windows);
    let to_orig = resolve_string_with_cwd(&[to], cwd, is_windows);
    if from_orig == to_orig {
        return String::new();
    }
    let from_lower = from_orig.to_lowercase();
    let to_lower = to_orig.to_lowercase();
    if from_lower == to_lower {
        return String::new();
    }

    // Node 的 `String#length` 是 UTF-16 码元数；astral 字符占两个码元。
    // 这里对应 Node 的 `fromOrig.length !== fromOrig.toLowerCase().length`：小写化改变了
    // UTF-16 长度时，码元下标无法与原文一一对应，只能走下面的分段回退实现。
    if from_orig.encode_utf16().count() != from_lower.encode_utf16().count()
        || to_orig.encode_utf16().count() != to_lower.encode_utf16().count()
    {
        // 回退实现：按 `\` 切段、逐段做大小写不敏感比较；resolved 路径若以分隔符结尾
        // （UNC 根）会多出一个空段，先去掉。
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
        while i < length {
            if from_split[i].to_lowercase() != to_split[i].to_lowercase() {
                break;
            }
            i += 1;
        }
        // 第一段就不同（典型是不同盘符）：没有公共根，直接返回 `to` 的解析结果。
        if i == 0 {
            return to_orig;
        } else if i == length {
            // 一方是另一方的前缀：`to` 更长就返回剩余段；`from` 更长就按剩余层数输出 `..`。
            if to_len > length {
                return to_split[i..].join("\\");
            }
            if from_len > length {
                return format!("{}{}", "..\\".repeat(from_len - 1 - i), "..");
            }
            return String::new();
        }
        return format!(
            "{}{}",
            "..\\".repeat(from_len - i),
            to_split[i..].join("\\")
        );
    }

    // 该分支在 UTF-16 码元上工作（`i === 2` 的“设备根”判断依赖码元计数），
    // 因此比较与下标都使用 u16 序列；写回 `to_orig` 时再换算成字节下标。
    let from_units: Vec<u16> = from_lower.encode_utf16().collect();
    let to_units: Vec<u16> = to_lower.encode_utf16().collect();
    let backslash = CHAR_BACKWARD_SLASH as u16;

    // 去掉前导反斜杠。
    let mut from_start = 0usize;
    while from_start < from_units.len() && from_units[from_start] == backslash {
        from_start += 1;
    }
    // 去掉尾随反斜杠（只对 UNC 路径适用）。
    let mut from_end = from_units.len();
    while from_end > from_start + 1 && from_units[from_end - 1] == backslash {
        from_end -= 1;
    }
    let from_len = from_end - from_start;

    let mut to_start = 0usize;
    while to_start < to_units.len() && to_units[to_start] == backslash {
        to_start += 1;
    }
    let mut to_end = to_units.len();
    while to_end > to_start + 1 && to_units[to_end - 1] == backslash {
        to_end -= 1;
    }
    let to_len = to_end - to_start;

    let length = from_len.min(to_len);
    let mut last_common_sep: isize = -1;
    let mut i = 0usize;
    while i < length {
        let from_code = from_units[from_start + i];
        if from_code != to_units[to_start + i] {
            break;
        } else if from_code == backslash {
            last_common_sep = i as isize;
        }
        i += 1;
    }

    if i != length {
        // 在第一个公共分隔符之前就不匹配：返回原始 `to`。
        if last_common_sep == -1 {
            return to_orig;
        }
    } else {
        if to_len > length {
            if to_units.get(to_start + i) == Some(&backslash) {
                // `from` 是 `to` 的精确基路径。
                return utf16_slice(&to_orig, to_start + i + 1, to_units.len());
            }
            if i == 2 {
                // `from` 是设备根。
                return utf16_slice(&to_orig, to_start + i, to_units.len());
            }
        }
        if from_len > length {
            if from_units.get(from_start + i) == Some(&backslash) {
                last_common_sep = i as isize;
            } else if i == 2 {
                last_common_sep = 3;
            }
        }
        if last_common_sep == -1 {
            last_common_sep = 0;
        }
    }

    // 每遇到 from 的一个剩余分隔符就输出一级 `..`；`index == from_end` 让最后一段（没有
    // 尾随分隔符）也算一级。
    let mut out = String::new();
    let mut index = (from_start as isize + last_common_sep + 1) as usize;
    while index <= from_end {
        if index == from_end || from_units[index] == backslash {
            if out.is_empty() {
                out.push_str("..");
            } else {
                out.push_str("\\..");
            }
        }
        index += 1;
    }

    // 公共分隔符之后的 `to` 原文保留大小写；out 为空时跳过 `to` 开头的一个 `\`，避免
    // 结果以分隔符开头。
    let to_start = (to_start as isize + last_common_sep) as usize;
    if !out.is_empty() {
        return format!("{out}{}", utf16_slice(&to_orig, to_start, to_end));
    }
    let mut to_start = to_start;
    if to_units.get(to_start) == Some(&backslash) {
        to_start += 1;
    }
    utf16_slice(&to_orig, to_start, to_end)
}

/// UTF-16 码元下标对应的字节下标；若该下标切断了代理对则返回 `None`。
///
/// `unit_index` 恰好等于码元总数时返回 `s.len()`，超出时返回 `None`。
fn byte_offset_for_utf16_index(s: &str, unit_index: usize) -> Option<usize> {
    let mut units = 0usize;
    for (byte_index, ch) in s.char_indices() {
        if units == unit_index {
            return Some(byte_index);
        }
        units += ch.len_utf16();
        if units > unit_index {
            return None;
        }
    }
    (units == unit_index).then_some(s.len())
}

/// 按 UTF-16 码元区间截取字符串。切到代理对中间时用 U+FFFD 顶替
/// （Node 会产出孤立代理项，Rust 的 `String` 无法表示）。
///
/// 两个下标都能换算成字节下标且顺序合法时直接按字节切片后复制（不经过 UTF-16 转换）；
/// 否则退回 `from_utf16_lossy`（会把半个代理对替换成 U+FFFD）。只有 `relative` 的码元下标
/// 回写会用到这个函数。
fn utf16_slice(s: &str, start: usize, end: usize) -> String {
    if let (Some(b0), Some(b1)) = (
        byte_offset_for_utf16_index(s, start),
        byte_offset_for_utf16_index(s, end),
    ) && b0 <= b1
    {
        return s[b0..b1].to_owned();
    }
    let units: Vec<u16> = s.encode_utf16().collect();
    let start = start.min(units.len());
    let end = end.min(units.len()).max(start);
    String::from_utf16_lossy(&units[start..end])
}

/// `path.win32.dirname(path)`。
///
/// 返回最后一个分隔符之前的部分，忽略尾随分隔符；没有父目录时返回 `.`。盘符与 UNC 前缀会被
/// 当成根：`C:`/`C:\` 原样保留，`\\server\share`（只有共享名）整体返回，带剩余部分的 UNC
/// 取到 `share` 后的分隔符（`\\server\share\a` → `\\server\share\`）。根相对的 `\foo` /
/// `/foo` 则返回那个分隔符本身。
///
/// 不折叠 `.`/`..`（`dirname("a/../b")` 是 `a/..`）。空串返回 `.`；单字符分隔符原样返回
/// （`/` 保持 `/`，不会被改写成 `\`）。
pub fn dirname(path: &str) -> String {
    let len = path.len();
    if len == 0 {
        return ".".to_owned();
    }
    let mut root_end: isize = -1;
    let mut offset = 0usize;
    let code = path.as_bytes()[0];

    // 单字符：分隔符原样返回，其它字符没有父目录。
    if len == 1 {
        return if is_path_separator(code) {
            path.to_owned()
        } else {
            ".".to_owned()
        };
    }

    if is_path_separator(code) {
        root_end = 1;
        offset = 1;
        if is_path_separator(path.as_bytes()[1]) {
            // UNC 根 `\\server\share`：server 与 share 都要非空；只有共享名时整体返回，
            // 带剩余部分时根包含 share 后的分隔符。
            let mut j = 2usize;
            let mut last = j;
            while j < len && !is_path_separator(path.as_bytes()[j]) {
                j += 1;
            }
            if j < len && j != last {
                last = j;
                while j < len && is_path_separator(path.as_bytes()[j]) {
                    j += 1;
                }
                if j < len && j != last {
                    last = j;
                    while j < len && !is_path_separator(path.as_bytes()[j]) {
                        j += 1;
                    }
                    if j == len {
                        // 只有 UNC 根。
                        return path.to_owned();
                    }
                    if j != last {
                        // 带剩余部分的 UNC 根：偏移 1 把 UNC 根后的分隔符也算作根。
                        root_end = j as isize + 1;
                        offset = j + 1;
                    }
                }
            }
        }
    } else if is_windows_device_root(code) && path.as_bytes()[1] == CHAR_COLON {
        // 盘符根 `C:` 或 `C:\`：offset 跳过盘符，使扫描不会把盘符后的分隔符当成可截断点。
        root_end = if len > 2 && is_path_separator(path.as_bytes()[2]) {
            3
        } else {
            2
        };
        offset = root_end as usize;
    }

    // 从末尾向前找最后一个"前面不是分隔符"的分隔符，即最后一段的起点；连续分隔符被跳过。
    let mut end: isize = -1;
    let mut matched_slash = true;
    let mut i = len as isize - 1;
    while i >= offset as isize {
        if is_path_separator(path.as_bytes()[i as usize]) {
            if !matched_slash {
                end = i;
                break;
            }
        } else {
            matched_slash = false;
        }
        i -= 1;
    }

    // 没找到可截断的分隔符：有根就返回根（`C:`、`C:\`、`\\server\share\`），否则 `.`。
    if end == -1 {
        if root_end == -1 {
            return ".".to_owned();
        }
        end = root_end;
    }
    path[..end as usize].to_owned()
}

/// `path.win32.basename(path, ext?)`。
///
/// 返回最后一段（忽略尾随分隔符）；盘符前缀会被跳过，因此 `basename("C:")` 是空串。
/// `ext` 非空且其 UTF-16 码元数不超过路径时，从尾部逐字节比较并剥离一次：
/// 比较区分大小写（`a.TXT` + `.txt` 不剥离），只影响结尾（`a.html.html` + `.html` →
/// `a.html`）。空路径返回空串；不带可剥离后缀时，只有分隔符/盘符的路径也返回空串。
pub fn basename(path: &str, ext: Option<&str>) -> String {
    let mut start = 0usize;
    let mut end: isize = -1;
    let mut matched_slash = true;

    // 跳过盘符，避免把其后的分隔符当成多余尾随分隔符。
    if path.len() >= 2
        && is_windows_device_root(path.as_bytes()[0])
        && path.as_bytes()[1] == CHAR_COLON
    {
        start = 2;
    }

    // 对应 Node 的 `ext && suffix.length <= path.length`；这里按 UTF-16 码元长度筛选，
    // 与 JS 的 `length` 对齐。
    if let Some(suffix) = ext
        && !suffix.is_empty()
        && suffix.encode_utf16().count() <= path.encode_utf16().count()
    {
        // 整个路径恰好等于后缀：返回空串。
        if suffix == path {
            return String::new();
        }
        let mut ext_idx = suffix.len() as isize - 1;
        let mut first_non_slash_end: isize = -1;
        let mut i = path.len() as isize - 1;
        // 从尾部逐字节回退后缀；一旦不匹配就停止剥离。`first_non_slash_end` 记录最后一段
        // 的末尾（跳过尾随分隔符），作为放弃剥离时的 `end`。
        while i >= start as isize {
            let code = path.as_bytes()[i as usize];
            if is_path_separator(code) {
                if !matched_slash {
                    start = i as usize + 1;
                    break;
                }
            } else {
                if first_non_slash_end == -1 {
                    matched_slash = false;
                    first_non_slash_end = i + 1;
                }
                if ext_idx >= 0 {
                    if code == suffix.as_bytes()[ext_idx as usize] {
                        ext_idx -= 1;
                        if ext_idx == -1 {
                            end = i;
                        }
                    } else {
                        ext_idx = -1;
                        end = first_non_slash_end;
                    }
                }
            }
            i -= 1;
        }
        // 最后一段恰好等于后缀（如 `C:.html` + `.html`）：退回完整最后一段，而不是空串。
        if start as isize == end {
            end = first_non_slash_end;
        } else if end == -1 {
            end = path.len() as isize;
        }
        return path[start..end as usize].to_owned();
    }
    // 没有可剥离的后缀：从末尾跳过连续分隔符，定位最后一段的起点。
    let mut i = path.len() as isize - 1;
    while i >= start as isize {
        if is_path_separator(path.as_bytes()[i as usize]) {
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
    // 整个路径没有非分隔字符（如 `/`、`C:\`）：basename 为空。
    if end == -1 {
        return String::new();
    }
    path[start..end as usize].to_owned()
}

#[cfg(test)]
mod tests {
    //! 主要公开函数的代表性样例：空串、根、UNC、`..`、尾随分隔符与后缀剥离。
    //! `resolve` 系列（`resolve`、`resolve_string`、`resolve_string_with_cwd`）没有这里的
    //! 样例，只由 `path/mod.rs` 的 `golden` 测试（`tests/fixtures/path_corpus.json`）覆盖；
    //! 其余函数与 Node 的完整对拍也在该测试中。

    use super::*;

    #[test]
    fn normalize_examples() {
        assert_eq!(normalize(""), ".");
        assert_eq!(normalize("\\"), "\\");
        assert_eq!(normalize("/"), "\\");
        assert_eq!(normalize("a//b"), "a\\b");
        assert_eq!(normalize("C:\\a\\..\\b"), "C:\\b");
        assert_eq!(normalize("C:"), "C:.");
        assert_eq!(normalize("C:\\"), "C:\\");
        assert_eq!(normalize("C:a"), "C:a");
        assert_eq!(normalize("//server/share"), "\\\\server\\share\\");
        assert_eq!(normalize("//server/share/"), "\\\\server\\share\\");
        assert_eq!(normalize("//server/share/a"), "\\\\server\\share\\a");
        assert_eq!(normalize("\\\\server\\share\\a"), "\\\\server\\share\\a");
    }

    #[test]
    fn is_absolute_examples() {
        assert!(is_absolute("C:\\a"));
        assert!(is_absolute("C:/a"));
        assert!(is_absolute("\\\\server"));
        assert!(is_absolute("\\a"));
        assert!(!is_absolute("C:"));
        assert!(!is_absolute("C:a"));
        assert!(!is_absolute("a"));
        assert!(!is_absolute(""));
    }

    #[test]
    fn dirname_examples() {
        assert_eq!(dirname(""), ".");
        assert_eq!(dirname("/"), "/");
        assert_eq!(dirname("\\"), "\\");
        assert_eq!(dirname("C:\\a\\b"), "C:\\a");
        assert_eq!(dirname("C:\\a"), "C:\\");
        assert_eq!(dirname("C:"), "C:");
        assert_eq!(dirname("C:a"), "C:");
        assert_eq!(dirname("a"), ".");
        assert_eq!(dirname("C:\\a\\b\\"), "C:\\a");
    }

    #[test]
    fn basename_examples() {
        assert_eq!(basename("C:\\a\\b", None), "b");
        assert_eq!(basename("C:\\a\\b\\", None), "b");
        assert_eq!(basename("C:", None), "");
        assert_eq!(basename("C:\\", None), "");
        assert_eq!(basename("a.html", Some(".html")), "a");
        assert_eq!(basename("C:\\a.html", Some(".html")), "a");
        assert_eq!(basename("a.html", Some(".htm")), "a.html");
    }

    #[test]
    fn join_examples() {
        assert_eq!(join(&[]), ".");
        assert_eq!(join(&["a", "b"]), "a\\b");
        assert_eq!(join(&["a", "", "b"]), "a\\b");
        assert_eq!(join(&["//server", "share"]), "\\\\server\\share\\");
        assert_eq!(join(&["C:\\a", "..", "b"]), "C:\\b");
    }

    #[test]
    fn relative_examples() {
        assert_eq!(relative("C:\\a\\b", "C:\\a\\c"), "..\\c");
        assert_eq!(relative("C:\\a", "C:\\a"), "");
        assert_eq!(relative("C:\\a\\b", "C:\\a\\b"), "");
        assert_eq!(relative("C:\\", "C:\\a"), "a");
        assert_eq!(relative("C:\\a", "C:\\"), "..");
    }
}
