//! `src/native-module-path.ts`（31 行）的等价物：原生模块候选路径。
//!
//! Rust 没有运行期 `.node` 加载（后端在编译期选定，见父模块文档），但这个函数的**可观察
//! 契约**仍要保留，因为 `native-module-path.test.ts` 的 2 个 it 是 tui-core 的验收项：
//!
//! 1. 候选顺序：包安装位置 → `moduleDir/..` → `moduleDir` → 可执行文件目录；
//! 2. 去重（保持首次出现顺序），`resolvePackage` 抛异常时跳过第一项。
//!
//! TS 的 `resolvePackage` 是 `createRequire().resolve`；Rust 侧对应
//! `Option<&dyn Fn(&str) -> Option<PathBuf>>`，`None` 或返回 `None` 都表示“没有已安装的包”。
//!
//! # 设计原因与边界
//!
//! Rust 的后端在编译期选定（见父模块文档），这条搜索链在仓库内没有真实调用方（只有测试
//! 与再导出）。保留它是为了让 `native-module-path.test.ts` 的路径推导、规范化与去重仍能在
//! Rust 侧验证（`tests/native_module_path.rs`）；删掉它会让对应的验收项失去被测对象。
//!
//! # 注意事项
//!
//! - 返回的候选**已经**按 `node:path.join` 规范化并去重，调用方不要再假设里面含 `..` 或重复项。
//! - `module_dir` 与 `exec_path` 由调用方注入；`module_dir` 为 `None` 时回退到可执行文件所在
//!   目录，`exec_path` 为 `None` 时才读取 `std::env::current_exe()`。`current_exe()` 失败会
//!   退化为空路径（候选变成相对路径），本函数不报错。
//! - `resolve_package` 是同步闭包，返回 `None` 表示“没有已安装的包”（TS 的 `resolvePackage`
//!   抛异常）；此时只跳过第一项，其余候选照常返回。

use std::path::{Path, PathBuf};

use crate::native::ClipboardValue;

/// `TUI_PACKAGE_NAME`。
pub const TUI_PACKAGE_NAME: &str = "@earendil-works/kiana-tui";

/// `resolvePackage`：解析 `@earendil-works/kiana-tui` 的入口文件；`None` 表示“没有已安装的包”。
pub type ResolvePackage<'a> = &'a dyn Fn(&str) -> Option<PathBuf>;

/// `NativeModuleCandidateOptions`。
#[derive(Default, Clone)]
pub struct NativeModuleCandidateOptions<'a> {
    /// TS `moduleUrl` → 这里直接给模块目录（`dirname(fileURLToPath(moduleUrl))`）。
    pub module_dir: Option<PathBuf>,
    /// TS `execPath`（默认 `process.execPath`）→ Rust 侧由调用方传入 `current_exe()`。
    pub exec_path: Option<PathBuf>,
    /// TS `resolvePackage`。
    pub resolve_package: Option<ResolvePackage<'a>>,
}

/// `getNativeModuleCandidates(nativePath, options)`。
///
/// 返回的每一项都按 `node:path.join` 的规则做了词法规范化（`a/../b` → `b`），并像
/// `Array.from(new Set(...))` 一样按规范化后的路径去重（保持首次出现顺序）。
///
/// 候选顺序即解析优先级（测试逐项断言）：
///
/// 1. `resolve(TUI_PACKAGE_NAME)` 返回的入口文件所在目录的 `..` 下的 `native_path`；
///    解析失败时整项跳过；
/// 2. `module_dir/../native_path`；
/// 3. `module_dir/native_path`；
/// 4. 可执行文件目录下的 `native_path`。
///
/// 后三项总会生成，但最后统一去重，返回列表可能少于三项；例如可执行文件目录恰好是
/// `module_dir` 的父目录时第 2、4 项相同，`module_dir` 等于可执行文件目录时第 3、4 项相同。
pub fn get_native_module_candidates(
    native_path: &Path,
    options: &NativeModuleCandidateOptions<'_>,
) -> Vec<PathBuf> {
    let module_dir: PathBuf = options
        .module_dir
        .clone()
        .unwrap_or_else(default_module_dir);
    let mut candidates: Vec<PathBuf> = Vec::new();

    // 1. 已安装的 TUI 包：`join(dirname(packageEntry), "..", nativePath)`。
    let resolved = match options.resolve_package {
        Some(resolve) => resolve(TUI_PACKAGE_NAME),
        None => None,
    };
    if let Some(entry) = resolved {
        let dir = entry.parent().unwrap_or(entry.as_path());
        candidates.push(node_join(&[dir, Path::new(".."), native_path]));
    }

    // 2. `moduleDir/..`、3. `moduleDir`、4. 可执行文件目录。
    candidates.push(node_join(&[&module_dir, Path::new(".."), native_path]));
    candidates.push(node_join(&[&module_dir, native_path]));
    let exec_dir = options
        .exec_path
        .clone()
        .or_else(|| std::env::current_exe().ok())
        .map(|exe| match exe.parent() {
            Some(parent) => parent.to_path_buf(),
            // `dirname("/kiana")` = `"/"`（Node 不会返回 undefined）。
            None => exe,
        })
        .unwrap_or_default();
    candidates.push(node_join(&[&exec_dir, native_path]));

    dedupe_preserving_order(candidates)
}

/// TS 的 `moduleDir` 默认值是模块文件所在目录（`dirname(fileURLToPath(import.meta.url))`）。
///
/// Rust 侧模块被静态链接进可执行文件，“模块目录”的运行期等价物就是可执行文件所在目录
/// （不是可执行文件本身的路径）。
fn default_module_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .unwrap_or_default()
}

/// `node:path.join(...parts)`：用路径分隔符拼接后做词法规范化。
///
/// 与 Rust 的 [`Path::join`] 不同：后面的绝对路径片段**不会**替换前面的前缀（Node 的 `join`
/// 只是拼接），`..` 按 Node `normalize` 的规则消解（根之上的 `..` 丢弃，相对路径的 `..` 保留）。
///
/// 拼接用 `OsString` 完成，不要求路径是合法 UTF-8；分隔符固定为 `MAIN_SEPARATOR`。
fn node_join(parts: &[&Path]) -> PathBuf {
    let mut joined = std::ffi::OsString::new();
    for part in parts {
        if !joined.is_empty() {
            joined.push(std::path::MAIN_SEPARATOR.to_string());
        }
        joined.push(part.as_os_str());
    }
    normalize_lexically(Path::new(&joined))
}

/// 与 `node:path.normalize` 等价的词法规范化。
///
/// 规则：
///
/// - `.`（`CurDir`）直接丢弃；
/// - `..`（`ParentDir`）在上一项是普通目录名时消掉该目录名；
/// - `..` 不能越过根或盘符前缀，位于它们之上时丢弃；相对路径开头的 `..` 保留；
/// - 只做词法处理：不访问文件系统，也不解析符号链接。
fn normalize_lexically(path: &Path) -> PathBuf {
    use std::path::Component;

    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => out.push(prefix.as_os_str()),
            Component::RootDir => out.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => match out.components().next_back() {
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                // `..` 不能越过根；相对路径开头的 `..` 保留。
                Some(Component::RootDir | Component::Prefix(_)) => {}
                Some(Component::ParentDir) | None => out.push(component.as_os_str()),
                Some(Component::CurDir) => {}
            },
            Component::Normal(name) => out.push(name),
        }
    }
    out
}

/// `Array.from(new Set(candidates))`。
fn dedupe_preserving_order(items: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::with_capacity(items.len());
    for item in items {
        if !out.contains(&item) {
            out.push(item);
        }
    }
    out
}

/// `getNativePlatformHelper()` 的静态视图：Rust 构建里是否编译进了原生后端。
///
/// TS 用它决定要不要尝试 `require`；Rust 用它给 `terminal.ts` 的 `enableVirtualTerminalInput`
/// 与 `native-platform.test.ts` 的 skip 条件一个确定的答案。
///
/// 这是 `cfg!` 的编译期常量表达式，不读取运行期状态：Linux 上它只表示“x11 后端被编译进来”，
/// 不代表当前 `DISPLAY` 可用（那是 [`crate::native::get_native_clipboard`] 的判断）。
pub fn native_backend_compiled() -> bool {
    cfg!(all(target_os = "linux", feature = "x11"))
        || cfg!(all(target_os = "macos", feature = "darwin"))
        || cfg!(all(target_os = "windows", feature = "win32"))
}

/// 未实现的后端统一返回 `Unavailable`（`undefined`），而不是伪造数据。
pub fn unavailable<T>() -> ClipboardValue<T> {
    ClipboardValue::Unavailable
}
