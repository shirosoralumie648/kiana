//! utils/output-files.ts。
//!
//! TS: `packages/coding-agent/src/utils/output-files.ts`（34 行）。
//!
//! ```ts
//! const OUTPUT_FILE_MODE = 0o600;
//! function createOutputFilePath(prefix, extension) {
//!   return join(tmpdir(), `${prefix}-${randomBytes(8).toString("hex")}${extension}`);
//! }
//! export async function writeOutputFile(prefix, extension, data): Promise<string>
//! export function createOutputFileStream(prefix, extension): { path; stream }
//! ```
//!
//! - 路径：`<tmpdir>/<prefix>-<16 位小写 hex><extension>`（`randomBytes(8)`）；
//! - 权限：`0o600`（`OUTPUT_FILE_MODE`），`flag: "wx"` —— **绝不跟随**别人放在该路径上的符号链接；
//! - `createWriteStream` 在 Rust 侧是 `tokio::fs::File`。
//!
//! - 临时目录：复刻 Node `os.tmpdir()`（而不是 `std::env::temp_dir()`）：
//!   - Unix：`TMPDIR || TMP || TEMP || "/tmp"`，路径长度 > 1 时去掉**一个**结尾 `/`；
//!   - Windows（Node 26 的实现）：`TEMP || TMP || (SystemRoot || windir) + "\\temp"`
//!     （不读 `TMPDIR`；两者都没有时是字面量 `"undefined\\temp"`），长度 > 1 且结尾
//!     是 `\`、前一个字符不是 `:` 时去掉一个结尾 `\`（所以 `C:\` 保留）。
//! - 文件路径按 Node `path.join(tmpdir, name)` 构造：POSIX 上逐字移植 `path.posix.join`
//!   （`normalize` 折叠重复 `/`、解析 `.`/`..`），返回规范化后的路径文本
//!   （例如 `TMPDIR="//"` → `/name`）。Windows 的 `path.win32` 规范化（UNC/设备名/
//!   保留名/CVE-2024-36139 等）未移植，仅做 `PathBuf::join`，见下文偏差。
//!
//! 偏差：
//!
//! - Windows 上 Node 忽略 `mode`（使用默认 ACL）；`#[cfg(unix)]` 之外不加 `mode`，行为一致。
//! - `crypto.randomBytes` 失败在 TS 抛异常；这里映射为 `io::ErrorKind::Other`。
//! - Windows 的 `path.win32.join` 规范化（重复分隔符、`.`/`..`、UNC、设备名、保留名）
//!   无法在本机验证，未逐字移植：Windows 分支直接 `PathBuf::join`，仅在临时目录含重复
//!   分隔符时返回的**文本**与 Node 不同（指向同一文件）。POSIX 分支行为一致。
//!
//! ## 调用方
//!
//! - `kiana-tools/src/output_accumulator.rs`：用 [`create_output_file_stream`] 落盘完整命令
//!   输出（`.log`），随后把 tokio `File` 转回 `std::fs::File` 做同步 `BufWriter` 写入；
//! - `kiana-builtin-extensions` 的 MCP `save_to_temp_file` 与 codemode 执行器：用
//!   [`write_output_file`] 保存工具结果全文/二进制。
//!
//! ## 约束与失败模式
//!
//! - 文件名随机部分来自 `getrandom`，调用方不能假设可复现；`create_new` 一旦发现同名文件
//!   就返回错误，本模块不重试、不覆盖（16 位 hex 碰撞概率极低，但确实是失败路径）。
//! - 只创建文件，不创建目录：临时目录不存在或不可写时错误原样返回。
//! - 返回的 `io::Error` 不携带尝试过的路径；需要 Node `error.message` 形状的调用方要自行
//!   补路径（见 `kiana-builtin-extensions/src/mcp/tools.rs` 的 `save_to_temp_file`）。
//! - 本模块不清理文件：临时文件的生命周期由调用方或会话负责。

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

/// TS: `OUTPUT_FILE_MODE = 0o600` —— 输出可能含私有数据，只允许本人读。
#[cfg(unix)]
const OUTPUT_FILE_MODE: u32 = 0o600;

/// TS: `os.tmpdir()` —— Node 的临时目录解析规则（不是 `std::env::temp_dir()`）。
fn tmpdir() -> PathBuf {
    tmpdir_with(|key| std::env::var_os(key), cfg!(windows))
}

/// 可注入环境变量查询与平台的 `os.tmpdir()`，便于在任意平台上单元测试两个分支。
fn tmpdir_with(mut get: impl FnMut(&str) -> Option<OsString>, windows: bool) -> PathBuf {
    let value = if windows {
        // Node 26 lib/os.js：
        //   TEMP || TMP || (SystemRoot || windir) + '\\temp'
        // `undefined + '\\temp'` 在 JS 里是字符串 `"undefined\\temp"`。
        get_non_empty(&mut get, "TEMP")
            .or_else(|| get_non_empty(&mut get, "TMP"))
            .unwrap_or_else(|| {
                let root = get_non_empty(&mut get, "SystemRoot")
                    .or_else(|| get_non_empty(&mut get, "windir"))
                    .unwrap_or_else(|| OsString::from("undefined"));
                let mut text = root;
                text.push("\\temp");
                text
            })
    } else {
        // Unix：TMPDIR || TMP || TEMP || '/tmp'。
        get_non_empty(&mut get, "TMPDIR")
            .or_else(|| get_non_empty(&mut get, "TMP"))
            .or_else(|| get_non_empty(&mut get, "TEMP"))
            .unwrap_or_else(|| OsString::from("/tmp"))
    };
    normalize_tmpdir(value, windows)
}

/// `process.env.NAME || ...`：空字符串视为未设置。
fn get_non_empty(mut get: impl FnMut(&str) -> Option<OsString>, key: &str) -> Option<OsString> {
    get(key).filter(|value| !value.is_empty())
}

/// Node 只移除一个结尾分隔符：Unix 在长度 > 1 时去掉结尾 `/`；
/// Windows 还要求前一个字符不是 `:`（`C:\` 保留）。
fn normalize_tmpdir(value: OsString, windows: bool) -> PathBuf {
    let (separator, keep_drive_root) = if windows { ('\\', true) } else { ('/', false) };

    let Some(mut text) = value.to_str().map(str::to_owned) else {
        // 非 UTF-8 路径：Node 会把它当字符串（替换非法字节）；这里保留原样。
        return PathBuf::from(value);
    };
    let len = text.encode_utf16().count();
    if len > 1 && text.ends_with(separator) {
        let before_ok = if keep_drive_root {
            // 排除 `C:\`：倒数第二个 code unit 是 ':' 时保留。
            text.encode_utf16().nth(len - 2) != Some(u16::from(b':'))
        } else {
            true
        };
        if before_ok {
            text.pop();
        }
    }
    PathBuf::from(text)
}

/// TS: `createOutputFilePath(prefix, extension)` ——
/// `path.join(tmpdir(), \`${prefix}-${randomBytes(8).toString("hex")}${extension}\`)`。
///
/// 生成 8 字节随机数并格式化为 16 位小写 hex；`getrandom` 失败时映射为
/// `io::Error::other`（TS 的 `crypto.randomBytes` 抛异常）。这里只计算路径，不创建文件；
/// 路径文本的拼法见 `join_output_path`。
fn create_output_file_path(prefix: &str, extension: &str) -> io::Result<PathBuf> {
    let mut bytes = [0u8; 8];
    getrandom::fill(&mut bytes).map_err(|error| io::Error::other(error.to_string()))?;

    let mut hex = String::with_capacity(16);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }

    let file_name = format!("{prefix}-{hex}{extension}");
    Ok(join_output_path(&tmpdir(), &file_name))
}

/// Node `path.join(dir, name)`：POSIX 逐字移植；Windows 退化为 `PathBuf::join`（见模块偏差）。
fn join_output_path(dir: &Path, name: &str) -> PathBuf {
    #[cfg(windows)]
    {
        dir.join(name)
    }
    #[cfg(not(windows))]
    {
        match dir.to_str() {
            Some(dir) if !name.is_empty() => PathBuf::from(posix_join(dir, name)),
            _ => dir.join(name),
        }
    }
}

/// `path.posix.join(dir, name)`：空参数被丢弃，其余用 `/` 连接后 `normalize`。
#[cfg(any(not(windows), test))]
fn posix_join(dir: &str, name: &str) -> String {
    let mut parts: Vec<&str> = Vec::with_capacity(2);
    if !dir.is_empty() {
        parts.push(dir);
    }
    if !name.is_empty() {
        parts.push(name);
    }
    if parts.is_empty() {
        return ".".to_string();
    }
    normalize_posix(&parts.join("/"))
}

/// 逐字移植 Node `path.posix.normalize`（lib/path.js）。
#[cfg(any(not(windows), test))]
fn normalize_posix(path: &str) -> String {
    if path.is_empty() {
        return ".".to_string();
    }
    let is_absolute = path.starts_with('/');
    let trailing_separator = path.ends_with('/');
    let chars: Vec<char> = path.chars().collect();
    let mut normalized: String = normalize_string(&chars, !is_absolute, '/', |c| c == '/')
        .into_iter()
        .collect();
    if normalized.is_empty() {
        if is_absolute {
            return "/".to_string();
        }
        return if trailing_separator {
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

/// 逐字移植 Node `normalizeString`（`res` 按字符保存，分隔符/点号都是 ASCII，
/// 与 JS 的 UTF-16 索引等价）。
#[cfg(any(not(windows), test))]
fn normalize_string(
    path: &[char],
    allow_above_root: bool,
    separator: char,
    is_path_separator: fn(char) -> bool,
) -> Vec<char> {
    // 状态：`dots` 记录当前段已经积累了连续点号的数量（0、1、2、3……；遇到点号就无条件
    // 递增），`-1` 表示段内出现过非点号字符；后续判定只区分 1、2 与其它值（≥3 按普通段处理）。
    // `last_segment_length` 是最后写入 `res` 的段长度，处理 `..` 回退时用它定位上一个
    // 分隔符；`last_slash` 是上一个分隔符在**输入**中的下标，用于切出普通段。
    let mut res: Vec<char> = Vec::new();
    let mut last_segment_length: isize = 0;
    let mut last_slash: isize = -1;
    let mut dots: isize = 0;
    let mut code = '\0';
    let mut i: isize = 0;
    // 输入耗尽后再注入一个虚拟分隔符（`else { code = '/' }`），让最后一段与中间段走同一个
    // 分隔符分支；下一轮发现 code 已是分隔符就 `break`。
    loop {
        if (i as usize) < path.len() {
            code = path[i as usize];
        } else if is_path_separator(code) {
            break;
        } else {
            code = '/';
        }

        if is_path_separator(code) {
            if last_slash == i - 1 || dots == 1 {
                // NOOP
            } else if dots == 2 {
                if res.len() < 2
                    || last_segment_length != 2
                    || res[res.len() - 1] != '.'
                    || res[res.len() - 2] != '.'
                {
                    if res.len() > 2 {
                        let last_slash_index = res.len() as isize - last_segment_length - 1;
                        if last_slash_index == -1 {
                            res.clear();
                            last_segment_length = 0;
                        } else {
                            res.truncate(last_slash_index as usize);
                            let previous = res.iter().rposition(|&c| c == separator);
                            last_segment_length = match previous {
                                Some(index) => res.len() as isize - 1 - index as isize,
                                None => res.len() as isize,
                            };
                        }
                        last_slash = i;
                        dots = 0;
                        i += 1;
                        continue;
                    } else if !res.is_empty() {
                        res.clear();
                        last_segment_length = 0;
                        last_slash = i;
                        dots = 0;
                        i += 1;
                        continue;
                    }
                }
                if allow_above_root {
                    if res.is_empty() {
                        res.push('.');
                        res.push('.');
                    } else {
                        res.push(separator);
                        res.push('.');
                        res.push('.');
                    }
                    last_segment_length = 2;
                }
            } else {
                if !res.is_empty() {
                    res.push(separator);
                }
                let start = (last_slash + 1) as usize;
                let end = i as usize;
                res.extend_from_slice(&path[start..end]);
                last_segment_length = i - last_slash - 1;
            }
            last_slash = i;
            dots = 0;
        } else if code == '.' && dots != -1 {
            dots += 1;
        } else {
            dots = -1;
        }
        i += 1;
    }
    res
}

/// 以 `wx`（`create_new`）+ `0o600` 打开一个新文件。
///
/// `create_new` 在路径已存在（包括是符号链接）时失败，因此不会出现“跟随链接写穿”或覆盖
/// 已有文件的情况。`mode` 只在 Unix 的**创建**时刻生效，Windows 分支不设置它（Node 在
/// Windows 上同样忽略 `mode`，见模块文档的偏差说明）。
fn open_new(path: &Path) -> io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(OUTPUT_FILE_MODE);
    }
    options.open(path)
}

/// TS: `writeOutputFile(prefix, extension, data)` —— 写入一个新的输出文件并返回其路径。
///
/// 调用前提：`prefix` 必须是普通文件名前缀，不得包含路径分隔符（`/`；Windows 上还有 `\`）
/// 或 `..` 路径段。代码没有校验它：`prefix` 直接进入 `format!("{prefix}-{hex}{extension}")`，
/// 再交给 `join_output_path` / `posix_join` 按 `path.join` 语义规范化，因此 `"../x"` 这类
/// 前缀会被当作目录/回退，目标可能落到临时目录之外（要在代码层保证需另行校验）。
/// `extension` 原样拼接（调用方自行带前导点，如 `.json`）。文件以 `create_new` + `0o600`
/// （Unix）创建，因此不会覆盖已有文件，也不会跟随别人预先放在目标路径上的符号链接。
///
/// 数据通过 `write_all` 一次写入，不调用 `sync_all`：返回时数据已交给操作系统的文件缓冲，
/// 但本函数不承诺落盘。错误来源：随机数生成、创建/打开文件、写入。
pub fn write_output_file(prefix: &str, extension: &str, data: &[u8]) -> io::Result<PathBuf> {
    let path = create_output_file_path(prefix, extension)?;
    let mut file = open_new(&path)?;
    use std::io::Write as _;
    file.write_all(data)?;
    Ok(path)
}

/// TS: `createOutputFileStream(prefix, extension)` —— 打开一个新的输出文件用于流式写入。
///
/// 返回 `(路径, tokio::fs::File)`；文件在返回前已同步创建并打开，异步句柄是同一个文件描述
/// 符的包装（`tokio::fs::File::from_std`），调用方可以直接 `write_all` / `flush`。失败语义
/// 与 [`write_output_file`] 相同。
///
/// 文件在创建后即使没有任何写入也会以 0 字节留在临时目录里；清理责任在调用方。
pub fn create_output_file_stream(
    prefix: &str,
    extension: &str,
) -> io::Result<(PathBuf, tokio::fs::File)> {
    let path = create_output_file_path(prefix, extension)?;
    let file = open_new(&path)?;
    Ok((path, tokio::fs::File::from_std(file)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lookup<'a>(values: &'a [(&'a str, &'a str)]) -> impl FnMut(&str) -> Option<OsString> + 'a {
        move |key| {
            values
                .iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| OsString::from(*value))
        }
    }

    #[test]
    fn tmpdir_follows_node_precedence_and_skips_empty_values() {
        assert_eq!(
            tmpdir_with(lookup(&[("TMPDIR", "/first"), ("TMP", "/second")]), false),
            PathBuf::from("/first")
        );
        // 空字符串在 Node 里是 falsy —— 相当于未设置。
        assert_eq!(
            tmpdir_with(lookup(&[("TMPDIR", ""), ("TMP", "/second")]), false),
            PathBuf::from("/second")
        );
        assert_eq!(
            tmpdir_with(lookup(&[("TMP", "/second"), ("TEMP", "/third")]), false),
            PathBuf::from("/second")
        );
        assert_eq!(tmpdir_with(lookup(&[]), false), PathBuf::from("/tmp"));
    }

    #[test]
    fn windows_tmpdir_matches_node_26() {
        // TEMP > TMP，且不读 TMPDIR。
        assert_eq!(
            tmpdir_with(
                lookup(&[("TMPDIR", "/ignored"), ("TEMP", "/temp"), ("TMP", "/tmp")]),
                true
            ),
            PathBuf::from("/temp")
        );
        assert_eq!(
            tmpdir_with(lookup(&[("TMP", "C:\\tmp"), ("TEMP", "")]), true),
            PathBuf::from("C:\\tmp")
        );
        // SystemRoot 在 TMP 之后、windir 之前。
        assert_eq!(
            tmpdir_with(
                lookup(&[("SystemRoot", "C:\\Windows"), ("windir", "D:\\Win")]),
                true
            ),
            PathBuf::from("C:\\Windows\\temp")
        );
        assert_eq!(
            tmpdir_with(lookup(&[("windir", "D:\\Win")]), true),
            PathBuf::from("D:\\Win\\temp")
        );
        // 两者都缺失时 JS 的 `undefined + '\\temp'`。
        assert_eq!(
            tmpdir_with(lookup(&[]), true),
            PathBuf::from("undefined\\temp")
        );
        // 结尾反斜杠：只去掉一个，且 `C:\` 保留。
        assert_eq!(
            tmpdir_with(lookup(&[("TEMP", "C:\\Temp\\")]), true),
            PathBuf::from("C:\\Temp")
        );
        assert_eq!(
            tmpdir_with(lookup(&[("TEMP", "C:\\")]), true),
            PathBuf::from("C:\\")
        );
        assert_eq!(
            tmpdir_with(lookup(&[("TEMP", "C:\\Temp\\\\")]), true),
            PathBuf::from("C:\\Temp\\")
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn tmpdir_strips_exactly_one_trailing_separator() {
        assert_eq!(
            tmpdir_with(lookup(&[("TMPDIR", "/a/b/")]), false),
            PathBuf::from("/a/b")
        );
        // 只去掉一个：`/a/b//` → `/a/b/`（Node 的 slice(0, -1)）。
        assert_eq!(
            tmpdir_with(lookup(&[("TMPDIR", "/a/b//")]), false),
            PathBuf::from("/a/b/")
        );
        // 长度 1 的根路径保持原样。
        assert_eq!(
            tmpdir_with(lookup(&[("TMPDIR", "/")]), false),
            PathBuf::from("/")
        );
        assert_eq!(
            tmpdir_with(lookup(&[("TMPDIR", "//")]), false),
            PathBuf::from("/")
        );
        // 相对路径照样使用。
        assert_eq!(
            tmpdir_with(lookup(&[("TMPDIR", "relative/dir/")]), false),
            PathBuf::from("relative/dir")
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn output_path_uses_node_path_join_normalization() {
        // Node: path.join("//", "x") === "/x"；path.join("/a//b/./c/../d/", "x") === "/a/b/d/x"。
        assert_eq!(posix_join("//", "x-abc.txt"), "/x-abc.txt");
        assert_eq!(posix_join("/a//b/./c/../d/", "x.txt"), "/a/b/d/x.txt");
        assert_eq!(posix_join("relative//dir/", "x.txt"), "relative/dir/x.txt");
        assert_eq!(posix_join("/a/b/..", "x.txt"), "/a/x.txt");
        assert_eq!(posix_join("a/../..", "x.txt"), "../x.txt");
        assert_eq!(posix_join("/", "x.txt"), "/x.txt");
        assert_eq!(posix_join("/a/b/", "x.txt"), "/a/b/x.txt");
        // 与 Node 26 的实测输出逐一对照。
        assert_eq!(normalize_posix("///x"), "/x");
        assert_eq!(normalize_posix(""), ".");
        assert_eq!(normalize_posix("/"), "/");
        assert_eq!(normalize_posix("a/.."), ".");
        assert_eq!(normalize_posix("../a"), "../a");
        assert_eq!(normalize_posix("a/./b"), "a/b");
    }

    #[test]
    fn created_files_live_in_the_node_tmpdir() {
        let path = write_output_file("kiana-unit-tmpdir", ".txt", b"x").expect("write");
        assert_eq!(path.parent(), Some(tmpdir().as_path()));
        std::fs::remove_file(&path).expect("cleanup");
    }
}
