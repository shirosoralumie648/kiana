//! `node:path.posix` 的函数级移植（`kiana-durable` 是唯一实现位置，见 `ARCHITECTURE.md` §4.9 / §8 第 23 条）。
//!
//! 逐函数对齐 Node 的 `lib/path.js`（Node 26）：`resolve`（右到左扫描、绝对路径截断、`..` 归一化）、
//! `normalize`（`//` 折叠、尾随分隔符保留规则）、`join`（空串参数忽略、绝对参数按普通段拼接）、
//! `dirname`/`basename`（尾随斜杠、`.`/`..`、根目录、可选后缀）、`relative`（大小写敏感、空结果 `""`）。
//!
//! 实现细节：Node 的算法在 UTF-16 码元上按字节式扫描，但所有判定只看 `'/'` 与 `'.'` 这两个 ASCII
//! 字符、所有切片都落在分隔符或字符串两端，因此用 UTF-8 字节实现与 Node 完全等价（非 ASCII 的每个
//! UTF-8 字节都不等于 `'/'`/`'.'`，`lastSegmentLength` 与 `res.length` 使用同一套单位）。
//!
//! # 在系统中的位置
//!
//! 调用方：`crate::env::node` 的 `platform_*` 包装（按本机平台分派）、`crate::env::node_watch`
//! 的 dirname/relative，以及 `kiana-env-client::remote_env` 在 `PathFlavor::Posix` 时的分派。
//! `normalize_string` 另被 `win32` 复用（见 `win32` 模块文档）。
//!
//! # 设计原因
//!
//! 不能用 `std::path` 代替：它按宿主平台识别分隔符（Windows 上反斜杠也是分隔符），而 `path.posix`
//! 只认 `/`，反斜杠不会被识别为分隔符（例如 [`normalize`] 对全反斜杠输入原样保留，而 [`dirname`]、
//! [`relative`] 等仍按各自规则处理，并非所有函数都原样返回）；`std::path` 也没有 Node 的 `//` 折叠、
//! 尾随分隔符保留这些纯字符串规则。下游把这些结果当作 watch 键与错误字段逐字节使用，所以按
//! `lib/path.js` 逐函数移植，而不是把 `std::path` 的输出转成字符串。
//!
//! # 执行流程
//!
//! 1. [`resolve_string_with_cwd`] 从右向左扫描参数：一旦遇到绝对参数就丢弃它左边的所有参数；
//!    没有绝对参数时以 cwd 为根。随后交给 `normalize_string` 折叠 `.`/`..`，最后补回根 `/`。
//! 2. [`normalize`] 直接调用 `normalize_string`；[`join`] 先忽略空串参数并按顺序拼接，再交给
//!    [`normalize`]（因此只是间接经过 `normalize_string`）。
//! 3. [`relative_with_cwd`] 先对两侧各做一次 resolve，再按字节比较公共前缀，最后用 `..` 补回差异。
//! 4. [`dirname`]/[`basename`] 只做一次反向字节扫描，不折叠 `..`。
//!
//! 除默认包装读取一次进程 cwd 外，全部是纯字符串运算（不检查路径是否存在、不解析符号链接）。
//!
//! # 注意事项
//!
//! - **扫描按 UTF-8 字节、以 ASCII 判定**：所有判定只看 `/`、`.` 等 ASCII 字节，所有切片点都
//!   落在分隔符或字符串两端，因此按字节扫描与按 UTF-16 码元扫描等价。新增分支时不要引入按
//!   `char` 计数、或把字节长度当成字符长度（UTF-16 码元）的比较。
//! - `normalize_string` 是与 `win32` 共用的核心（`pub(crate)`）：它只认调用方传入的 `separator`
//!   与 `is_path_separator`，不处理盘符、UNC 等根前缀（这些由 `win32` 在调用前切掉）。
//! - [`relative_with_cwd`] 假定两次 resolve 的结果都以 `/` 开头（它从下标 1 开始比较，跳过根
//!   分隔符）。直接传非绝对 cwd 且参数也是相对路径时，结果不再可靠。

use std::path::PathBuf;

/// `path.posix.sep`。
///
/// 固定为 `/`、与宿主平台无关；不要与 `std::path::MAIN_SEPARATOR`（随目标平台变化）混用。
pub const SEP: char = '/';

const CHAR_FORWARD_SLASH: u8 = b'/';
const CHAR_DOT: u8 = b'.';

/// `isPathSeparator` 的 posix 版本：只有 `/` 是分隔符，反斜杠是普通字节。
fn is_posix_path_separator(code: u8) -> bool {
    code == CHAR_FORWARD_SLASH
}

/// Node 的 `normalizeString(path, allowAboveRoot, separator, isPathSeparator)`（逐字移植）。
///
/// 输入可以带前导分隔符（posix 调用者传整串，开头的 `/` 被当作空段跳过）；win32 调用者则先
/// 切掉盘符/UNC 根，只传尾部。输出是折叠掉空段、`.` 与 `..` 后的相对形式：不带前导/尾随
/// 分隔符，空结果用 `""` 表示，由调用方决定补 `/`、反斜杠还是 `.`。
///
/// # 参数
///
/// - `allow_above_root`：为 `false`（绝对路径）时，越过根的 `..` 被丢弃；为 `true` 时保留前导
///   `..`（`../a`）。
/// - `separator`：拼接结果时写入的分隔符（posix 为 `/`，win32 为反斜杠）。
/// - `is_path_separator`：判断输入字节是否为分隔符（win32 同时接受 `/` 与反斜杠）；输出中统一
///   写成 `separator`。
///
/// # 算法
///
/// 单遍字节扫描，维护三个状态：
/// - `last_slash`：上一个分隔符的字节下标（初始 `-1`）；
/// - `dots`：当前段是否全由 `.` 组成——`0`/`1`/`2`/`3`… 表示点数，`-1` 表示已出现非点号字节；
/// - `last_segment_length`：结果中最后一段的字节长度，遇到 `..` 时靠它把上一段截掉。
///
/// 每遇到一个分隔符（字符串末尾被当作隐式分隔符；若最后一个字节本身已是分隔符则直接结束）：
/// 1. 空段（前一个字节也是分隔符）或 `"."` 段：丢弃；
/// 2. `".."` 段：若结果末尾不是 `..`，先弹出最后一段（`res.len() > 2` 时按 `last_segment_length`
///    截断，否则清空）；结果里已经是 `..` 时不能再弹，只在 `allow_above_root` 为真时再追加一个
///    `..`；
/// 3. 普通段：追加 `separator`（结果非空时）与该段原文。
///
/// 所有切片端点都落在分隔符或字符串末尾，UTF-8 多字节序列不会被截断；这与 Node 在 UTF-16
/// 码元上扫描的结果等价，因为判定只看 `/`、反斜杠、`.` 这些 ASCII 字节。
pub(crate) fn normalize_string(
    path: &str,
    allow_above_root: bool,
    separator: char,
    is_path_separator: impl Fn(u8) -> bool,
) -> String {
    let bytes = path.as_bytes();
    let mut res = String::new();
    let mut last_segment_length: usize = 0;
    let mut last_slash: isize = -1;
    let mut dots: isize = 0;
    let mut code: u8 = 0;

    // 多迭代一次（i == bytes.len()）：把“字符串结束”当作隐式分隔符来提交最后一段；
    // 若最后一个字节本身就是分隔符，则直接 break，避免把这个空段再处理一遍。
    for i in 0..=bytes.len() {
        if i < bytes.len() {
            code = bytes[i];
        } else if is_path_separator(code) {
            break;
        } else {
            code = CHAR_FORWARD_SLASH;
        }

        if is_path_separator(code) {
            // 三类段：空段与 "." 丢弃；".." 弹出上一段；其余原样追加。
            if last_slash == i as isize - 1 || dots == 1 {
                // NOOP
            } else if dots == 2 {
                let res_bytes = res.as_bytes();
                if res_bytes.len() < 2
                    || last_segment_length != 2
                    || res_bytes[res_bytes.len() - 1] != CHAR_DOT
                    || res_bytes[res_bytes.len() - 2] != CHAR_DOT
                {
                    if res_bytes.len() > 2 {
                        // 结果里还有可弹出的段：按 last_segment_length 找到最后一段的起始分隔符
                        // 并截断，再重算截断后最后一段的长度（后续的 `..` 还要用它）。
                        let last_slash_index =
                            res_bytes.len() as isize - last_segment_length as isize - 1;
                        if last_slash_index == -1 {
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
                        res.clear();
                        last_segment_length = 0;
                        last_slash = i as isize;
                        dots = 0;
                        continue;
                    }
                }
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
                if !res.is_empty() {
                    res.push(separator);
                    res.push_str(&path[(last_slash + 1) as usize..i]);
                } else {
                    res.push_str(&path[(last_slash + 1) as usize..i]);
                }
                last_segment_length = (i as isize - last_slash - 1) as usize;
            }
            last_slash = i as isize;
            dots = 0;
        } else if code == CHAR_DOT && dots != -1 {
            // 只有当前段到目前为止全由 "." 组成时才继续计数；一旦出现其它字节，`dots`
            // 保持 -1，直到下一个分隔符把它重置。
            dots += 1;
        } else {
            dots = -1;
        }
    }
    res
}

/// `process.cwd()` 的字符串形式；取不到时回退 `/`（见下方注释）。
///
/// 每次调用都重新读取进程当前目录，所以默认包装 [`resolve`]/[`relative`] 的结果取决于调用时刻
/// 的 cwd；需要确定性的结果时用 `*_with_cwd` 变体（黄金测试就是这样固定 `/cwd` 的）。
fn posix_cwd() -> String {
    match std::env::current_dir() {
        Ok(path) => path.to_string_lossy().into_owned(),
        // Node 的 `process.cwd()` 在 cwd 不可用时抛错；Rust 侧没有异常通道，退回根目录（绝对、
        // 归一化结果仍然确定）。`ARCHITECTURE.md` §8 第 5 条的错误模型不允许 panic。
        Err(_) => "/".to_owned(),
    }
}

/// `path.posix.resolve(...parts)`；返回 [`PathBuf`] 以匹配骨架冻结的签名。
///
/// 语义见 [`resolve_string`]：从右向左找第一个绝对参数，它左边的参数全部丢弃；没有绝对参数时
/// 以进程 cwd 为根。结果已归一化（`.`/`..`、重复、尾随分隔符都折叠），不检查路径是否存在。
/// `PathBuf` 只是包装，不会再按 Rust 的路径规则处理。
pub fn resolve(parts: &[&str]) -> PathBuf {
    PathBuf::from(resolve_string(parts))
}

/// [`resolve`] 的字符串形式（供 [`relative`] 与 win32 实现复用）。
///
/// cwd 取自 `posix_cwd()`（每次调用读取进程当前目录，失败回退 `/`）。需要与机器无关的确定性
/// 结果时用 [`resolve_string_with_cwd`]。
pub fn resolve_string(parts: &[&str]) -> String {
    resolve_string_with_cwd(parts, &posix_cwd())
}

/// [`resolve_string`]，但 cwd 由调用方给出（等价于 Node 里覆盖 `process.cwd`；黄金测试用它固定
/// 与平台无关的期望值）。
///
/// # 算法（对应 Node 的 `path.posix.resolve`）
///
/// 1. 快路径：没有参数、或只有一个空串/`"."`，且 `cwd` 以 `/` 开头时，直接返回 `cwd`
///    （不再重新归一化）。
/// 2. 从最后一个参数向前拼接，直到遇到以 `/` 开头的参数；它左边的参数全部作废。
/// 3. 仍不是绝对路径时，在最前面补上 `cwd` 与一个 `/`。
/// 4. 交给 `normalize_string` 折叠 `.`/`..`：绝对路径不允许越过根，相对路径保留前导 `..`。
/// 5. 绝对结果补回 `normalize_string` 去掉的前导 `/`；相对结果为空时返回 `"."`。
///
/// # 约束
///
/// `cwd` 被当作普通字符串使用：函数既不验证它是绝对路径，也不读取真实 cwd（只有
/// [`resolve_string`] 才读）。若 `cwd` 不以 `/` 开头且所有参数都是相对路径，返回值也会是相对
/// 路径；调用方若依赖“resolve 的结果是绝对路径”这一前提，需要自行保证 `cwd` 绝对。
pub fn resolve_string_with_cwd(parts: &[&str], cwd: &str) -> String {
    if (parts.is_empty() || (parts.len() == 1 && (parts[0].is_empty() || parts[0] == ".")))
        && cwd.as_bytes().first() == Some(&CHAR_FORWARD_SLASH)
    {
        return cwd.to_owned();
    }
    let mut resolved_path = String::new();
    let mut resolved_absolute = false;

    let mut index = parts.len() as isize - 1;
    // 从右往左拼：一旦当前参数是绝对路径，右边的结果已经完整，左边的参数不再影响结果
    // （Node 的 resolve 语义），因此循环以 resolved_absolute 为终止条件。
    while index >= 0 && !resolved_absolute {
        let path = parts[index as usize];
        if path.is_empty() {
            index -= 1;
            continue;
        }
        resolved_path = format!("{path}/{resolved_path}");
        resolved_absolute = path.as_bytes().first() == Some(&CHAR_FORWARD_SLASH);
        index -= 1;
    }

    if !resolved_absolute {
        // 扫到最左仍没有绝对参数：用 cwd 兜底；cwd 自身可能是绝对路径，所以再判一次。
        resolved_path = format!("{cwd}/{resolved_path}");
        resolved_absolute = cwd.as_bytes().first() == Some(&CHAR_FORWARD_SLASH);
    }

    let resolved_path = normalize_string(
        &resolved_path,
        !resolved_absolute,
        '/',
        is_posix_path_separator,
    );

    if resolved_absolute {
        format!("/{resolved_path}")
    } else if !resolved_path.is_empty() {
        resolved_path
    } else {
        ".".to_owned()
    }
}

/// `path.posix.normalize(path)`。
///
/// 规则：
/// - 空串返回 `"."`；
/// - 连续分隔符折叠成一个 `/`，`.` 段丢弃，`..` 弹出上一段（绝对路径在根处停止）；
/// - 保留输入的尾随分隔符（`"a/"` → `"a/"`，`"./"` → `"./"`）；
/// - 归一化后为空时，按“是否绝对 / 是否有尾随分隔符”返回 `"/"`、`"./"` 或 `"."`。
///
/// 纯字符串运算，不访问文件系统，也不解析符号链接。
pub fn normalize(path: &str) -> String {
    if path.is_empty() {
        return ".".to_owned();
    }
    let is_absolute = path.as_bytes()[0] == CHAR_FORWARD_SLASH;
    let trailing_separator = path.as_bytes()[path.len() - 1] == CHAR_FORWARD_SLASH;

    let mut path = normalize_string(path, !is_absolute, '/', is_posix_path_separator);

    if path.is_empty() {
        if is_absolute {
            return "/".to_owned();
        }
        return if trailing_separator {
            "./".to_owned()
        } else {
            ".".to_owned()
        };
    }
    if trailing_separator {
        path.push('/');
    }
    if is_absolute {
        format!("/{path}")
    } else {
        path
    }
}

/// `path.posix.isAbsolute(path)`。
///
/// 只看第一个字节是不是 `/`：`C:\a`、`\\server\share` 在 posix 语义下都是相对路径，
/// `//a` 则是绝对路径（前导双斜杠不算特殊根）。
pub fn is_absolute(path: &str) -> bool {
    path.starts_with('/')
}

/// `path.posix.join(...parts)`。
///
/// 忽略空串参数；一个非空参数都没有时返回 `"."`。非空参数按顺序拼接（后面的绝对路径**不会**
/// 像 `resolve` 那样截断前面的部分：`join(&["a", "/b"]) == "a/b"`），最后用 [`normalize`]
/// 折叠 `.`/`..` 与重复分隔符。
///
/// 不读取 cwd，结果不保证是绝对路径。
pub fn join(parts: &[&str]) -> String {
    if parts.is_empty() {
        return ".".to_owned();
    }
    let mut joined: Option<String> = None;
    for part in parts {
        // 空串参数被忽略：既不产生分隔符也不产生段（Node 的 join 语义）。
        if !part.is_empty() {
            match joined.as_mut() {
                Some(joined) => {
                    joined.push('/');
                    joined.push_str(part);
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

/// `path.posix.relative(from, to)`。
///
/// 两侧先各自用进程 cwd resolve 后再比较；`from == to` 的字符串快路径在 cwd 参与 resolve 之前
/// 返回（`relative` 仍会先把 `posix_cwd()` 求值为实参），因此同一字符串的结果与 cwd 无关。
/// 结果为空串表示同一路径。
///
/// 大小写敏感（posix 语义）；不访问文件系统，路径不存在也不报错。
pub fn relative(from: &str, to: &str) -> String {
    relative_with_cwd(from, to, &posix_cwd())
}

/// [`relative`]，但 cwd 由调用方给出（黄金测试用）。
///
/// # 算法
///
/// 1. `from == to` 直接返回 `""`；否则两侧各自 resolve（含归一化），再次相等也返回 `""`。
/// 2. 从下标 1（根 `/` 之后）逐字节比较，记住最后一个公共 `/` 的位置 `last_common_sep`。
/// 3. 公共前缀之后，`from` 每剩一段就输出一个 `..`，再接上 `to`（已 resolve 的字符串）从
///    `last_common_sep` 起的剩余部分。
/// 4. 前缀比较正好用完较短一方时有两个特例：`from` 是 `to` 的精确基路径（`to` 的下一个字节是
///    `/`）时直接返回 `to` 的剩余段；`from` 是根（`i == 0`）时返回去掉前导 `/` 的 `to`。
///
/// # 约束
///
/// 假定两次 resolve 都得到以 `/` 开头的绝对路径：`from_start`/`to_start` 固定为 1，用来跳过
/// 根分隔符。若 `cwd` 不是绝对路径、参数又是相对路径，resolve 会返回相对路径，前缀比较从第 2
/// 个字节开始，结果不再可靠。`env::node_watch` 调用时传 `cwd = ""`，它依赖快照键本身是绝对
/// 路径（该模块文档说明快照键保持 `resolve_path` 的输出形式）。
pub fn relative_with_cwd(from: &str, to: &str, cwd: &str) -> String {
    if from == to {
        return String::new();
    }
    let from = resolve_string_with_cwd(&[from], cwd);
    let to = resolve_string_with_cwd(&[to], cwd);
    if from == to {
        return String::new();
    }

    let from_start = 1usize;
    let from_end = from.len();
    let from_len = from_end - from_start;
    let to_start = 1usize;
    let to_len = to.len().saturating_sub(to_start);

    let length = from_len.min(to_len);
    let mut last_common_sep: isize = -1;
    let mut i = 0usize;
    // 下标 0 是两条绝对路径共同的根 `/`，内容从 1 开始比；last_common_sep 记录最后一个公共
    // `/`，决定要回退几级以及 `to` 从哪一段开始保留。
    while i < length {
        let from_code = from.as_bytes()[from_start + i];
        if from_code != to.as_bytes()[to_start + i] {
            break;
        } else if from_code == CHAR_FORWARD_SLASH {
            last_common_sep = i as isize;
        }
        i += 1;
    }
    if i == length {
        // 公共前缀覆盖了较短的一整条路径：先处理“from 是 to 的精确基路径”和“from 是根”两个
        // 特例；都不命中时沿用循环里记录的最后一个公共分隔符（没有则为 -1，表示只共享根）。
        if to_len > length {
            if to.as_bytes().get(to_start + i) == Some(&CHAR_FORWARD_SLASH) {
                // `from` 是 `to` 的精确基路径。
                return to[to_start + i + 1..].to_owned();
            }
            if i == 0 {
                // `from` 是根。
                return to[to_start + i..].to_owned();
            }
        } else if from_len > length {
            if from.as_bytes().get(from_start + i) == Some(&CHAR_FORWARD_SLASH) {
                last_common_sep = i as isize;
            } else if i == 0 {
                last_common_sep = 0;
            }
        }
    }
    let mut out = String::new();
    let mut index = (from_start as isize + last_common_sep + 1) as usize;
    while index <= from_end {
        if index == from_end || from.as_bytes()[index] == CHAR_FORWARD_SLASH {
            if out.is_empty() {
                out.push_str("..");
            } else {
                out.push_str("/..");
            }
        }
        index += 1;
    }
    format!(
        "{out}{}",
        &to[(to_start as isize + last_common_sep) as usize..]
    )
}

/// `path.posix.dirname(path)`。
///
/// 从末尾向前扫描，找到目录部分的最后一个分隔符并截断；不折叠 `..`、不解析符号链接。
/// 没有可用的分隔符时返回 `"."`（绝对路径返回 `"/"`）：`"a/b/"` → `"a"`，`"a/"` → `"."`。
/// 根的特例：`"/"` 与 `"//"` 都返回 `"/"`（单元测试锁定）。
pub fn dirname(path: &str) -> String {
    if path.is_empty() {
        return ".".to_owned();
    }
    let has_root = path.as_bytes()[0] == CHAR_FORWARD_SLASH;
    let mut end: isize = -1;
    let mut matched_slash = true;
    let mut i = path.len() as isize - 1;
    while i >= 1 {
        if path.as_bytes()[i as usize] == CHAR_FORWARD_SLASH {
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
            "/".to_owned()
        } else {
            ".".to_owned()
        };
    }
    if has_root && end == 1 {
        // end == 1 表示分隔符紧跟在根之后（如 `//a`）：Node 的 dirname 在这里保留双斜杠，
        // 返回 `//` 而不是 `/`。
        return "//".to_owned();
    }
    path[..end as usize].to_owned()
}

/// `path.posix.basename(path, ext?)`。
///
/// 反向扫描，返回最后一个分隔符之后的部分；空串与 `"/"` 返回 `""`，`"a/"` 返回 `"a"`。
/// 给出 `ext` 时会去掉**区分大小写**的后缀，但只在 `ext` 非空、按 UTF-16 码元数不超过 `path`
/// （即不长于路径）、且确实匹配到路径末尾时生效；后缀恰好覆盖整个文件名段时不剥离
/// （`basename("/a", Some("a"))` 仍是 `"a"`）。
///
/// 长度检查用 UTF-16 码元（对齐 JS 的 `string.length`），实际比较用 UTF-8 字节；对合法 UTF-8
/// 输入两者等价。
pub fn basename(path: &str, ext: Option<&str>) -> String {
    let mut start = 0usize;
    let mut end: isize = -1;
    let mut matched_slash = true;
    if let Some(suffix) = ext
        && !suffix.is_empty()
        && suffix.encode_utf16().count() <= path.encode_utf16().count()
    {
        if suffix == path {
            return String::new();
        }
        let mut ext_idx = suffix.len() as isize - 1;
        let mut first_non_slash_end: isize = -1;
        let mut i = path.len() as isize - 1;
        while i >= 0 {
            let code = path.as_bytes()[i as usize];
            if code == CHAR_FORWARD_SLASH {
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
        // 后缀恰好覆盖整个文件名段（start == end）时不剥离，恢复完整文件名：Node 的同一特例。
        if start as isize == end {
            end = first_non_slash_end;
        } else if end == -1 {
            end = path.len() as isize;
        }
        return path[start..end as usize].to_owned();
    }
    let mut i = path.len() as isize - 1;
    while i >= 0 {
        if path.as_bytes()[i as usize] == CHAR_FORWARD_SLASH {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_examples() {
        assert_eq!(normalize(""), ".");
        assert_eq!(normalize("/"), "/");
        assert_eq!(normalize("//"), "/");
        assert_eq!(normalize("a//b"), "a/b");
        assert_eq!(normalize("a/../b"), "b");
        assert_eq!(normalize("../a"), "../a");
        assert_eq!(normalize("/../a"), "/a");
        assert_eq!(normalize("a/"), "a/");
        assert_eq!(normalize("./"), "./");
        assert_eq!(normalize("a/.."), ".");
        assert_eq!(normalize("a/../"), "./");
        assert_eq!(normalize("/a/.."), "/");
        assert_eq!(normalize("a/b/../../c"), "c");
        assert_eq!(normalize("a/b/../../../c"), "../c");
    }

    #[test]
    fn join_examples() {
        assert_eq!(join(&[]), ".");
        assert_eq!(join(&["", ""]), ".");
        assert_eq!(join(&["a", "b"]), "a/b");
        assert_eq!(join(&["a", "", "b"]), "a/b");
        assert_eq!(join(&["a", "/b"]), "a/b");
        assert_eq!(join(&["/a", "b"]), "/a/b");
        assert_eq!(join(&["a", "..", "b"]), "b");
    }

    #[test]
    fn dirname_examples() {
        assert_eq!(dirname(""), ".");
        assert_eq!(dirname("/"), "/");
        assert_eq!(dirname("//"), "/");
        assert_eq!(dirname("/a"), "/");
        assert_eq!(dirname("/a/b"), "/a");
        assert_eq!(dirname("a"), ".");
        assert_eq!(dirname("a/"), ".");
        assert_eq!(dirname("a/b/"), "a");
        assert_eq!(dirname("./a"), ".");
    }

    #[test]
    fn basename_examples() {
        assert_eq!(basename("", None), "");
        assert_eq!(basename("/", None), "");
        assert_eq!(basename("/a/b", None), "b");
        assert_eq!(basename("/a/b/", None), "b");
        assert_eq!(basename("a", None), "a");
        assert_eq!(basename("a.html", Some(".html")), "a");
        assert_eq!(basename("a.html", Some("html")), "a.");
        assert_eq!(basename("a.html", Some(".htm")), "a.html");
        assert_eq!(basename("/a/b.html", Some(".html")), "b");
        assert_eq!(basename("a.html", Some("")), "a.html");
        assert_eq!(basename("/a", Some("a")), "a");
    }

    #[test]
    fn is_absolute_examples() {
        assert!(is_absolute("/a"));
        assert!(!is_absolute("a"));
        assert!(!is_absolute(""));
    }
}
