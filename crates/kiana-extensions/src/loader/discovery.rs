//! `resolveExtensionEntries` / `discoverExtensionsInDir`（loader.ts 的发现规则）。
//!
//! TS 源：`packages/coding-agent/src/core/extensions/loader.ts`（`isExtensionFile`、
//! `resolveExtensionEntries`、`discoverExtensionsInDir`）。规范：`spec/ca-extensions.md`
//! §2.2.8、§4.1–§4.2。
//!
//! # 逐条对齐的语义
//!
//! - **一层发现**：`extensions/` 下的直接 `.ts`/`.js` 文件与「可解析入口」的子目录/符号链接；
//!   不递归。
//! - 子目录入口顺序：`package.json` 的 `kiana.extensions`（只保留存在的条目）→ `index.ts` →
//!   `index.js` → 无。
//! - `package.json` 条目用 `path.resolve(dir, entry)` 解析，**不做 `~` 展开**（`~entry.ts`
//!   是目录内的普通文件名）。
//! - `readdir` 出错（权限、非目录）或迭代中途的任何 IO 错误 → 空列表（TS 的 `catch { return [] }`，
//!   已收集的结果一并丢弃）。
//! - **不排序**：顺序是文件系统顺序；扩展加载顺序（以及 handler 顺序）依赖它，调用方只能自行
//!   排序比较。
//!
//! # 在调用链中的位置
//!
//! 生产调用方只有 `loader::discover_and_load_extensions`（`loader/mod.rs`）：它先用
//! [`discover_extensions_in_dir`] 扫描 `<cwd>/.kiana/extensions` 与 `<agentDir>/extensions`，
//! 再对每个显式配置目录先试 [`resolve_extension_entries`]、没有入口时才退化为目录扫描。
//! 本模块只产出路径字符串，不读扩展源码、不加载模块；去重与最终加载顺序由调用方决定
//! （`discover_and_load_extensions` 按解析后的绝对路径去重，并保留首次出现的顺序）。
//!
//! # 边界与失败模式
//!
//! - `package.json` 读不到、JSON 非法、`kiana` 不是对象，或 `kiana.extensions` 不是全字符串
//!   数组时，`read_kiana_manifest` 返回 `None` / 缺字段；本模块把它当作“没有声明”，继续看
//!   `index.ts` / `index.js`，原始错误信息被丢弃。
//! - 存在性判断用 `Path::exists()`（不要求目标是普通文件）：目录或符号链接的目标存在即算命中。
//! - 符号链接只看链接自身的名字与类型（`DirEntry::file_type` 不跟随目标）：名字以 `.ts`/`.js`
//!   结尾的链接直接按文件收集，不检查目标是否是文件、是否存在。
//! - 真实目录名以 `.ts`/`.js` 结尾时不会进入文件分支（该分支要求 `isFile` 或符号链接），而是
//!   按子目录入口解析。

use std::path::{Path, PathBuf};

use kiana_core::kiana_manifest::read_kiana_manifest;
use kiana_core::paths::{PathInputOptions, resolve_path};

/// 文件名是否是扩展入口（`name.endsWith(".ts") || name.endsWith(".js")`）。
///
/// 大小写敏感；`.mjs`/`.cjs`/`.tsx` 都不算（TS 逐字如此）。
/// TS: `loader.ts#isExtensionFile`。
pub fn is_extension_file(name: &str) -> bool {
    name.ends_with(".ts") || name.ends_with(".js")
}

/// 析构用：不做 `~` 展开的 `path.resolve(base, input)`。
///
/// `kiana_core::paths::resolve_path` 的默认 `expand_tilde` 是 true（`normalizePath` 的 TS
/// 缺省），而 `path.resolve` 不展开；这里显式关掉全部规范化选项。
fn path_resolve(base: &Path, input: &str) -> String {
    let base = base.to_string_lossy();
    resolve_path(
        input,
        &base,
        &PathInputOptions {
            trim: false,
            expand_tilde: false,
            home_dir: None,
            strip_at_prefix: false,
            normalize_unicode_spaces: false,
        },
    )
}

/// 解析一个目录的扩展入口。
///
/// 顺序（`spec/ca-extensions.md` §2.2.8）：
/// 1. `<dir>/package.json` 存在且 `kiana.extensions` 非空：返回其中**存在**的条目
///    （`path.resolve(dir, entry)`，无 `~` 展开）；至少一条存在时即返回，否则继续。
/// 2. `<dir>/index.ts`。
/// 3. `<dir>/index.js`。
/// 4. `None`。
///
/// 返回的路径已按 `path.resolve` 规则归一化：`package.json` 条目经内部 `path_resolve`
/// （绝对条目或绝对 `dir` 得到绝对路径，否则保持相对）；`index.*` 是 `dir.join(...)`。
/// 入口存在性只看 `Path::exists()`，因此名为 `index.ts` 的**目录**也会被当成入口返回。
///
/// `package.json` 的读取/校验失败与“声明的条目全部不存在”都无法从返回值区分：两者都表现为
/// 继续尝试 `index.*`。确实没有任何入口时返回 `None`（不是空 `Vec`）。
///
/// TS: `loader.ts#resolveExtensionEntries`。
pub fn resolve_extension_entries(dir: &Path) -> Option<Vec<String>> {
    let package_json_path = dir.join("package.json");
    if package_json_path.exists()
        && let Some(manifest) = read_kiana_manifest(&package_json_path)
        && let Some(declared) = manifest.extensions
        && !declared.is_empty()
    {
        let mut entries: Vec<String> = Vec::new();
        for entry in &declared {
            let resolved = path_resolve(dir, entry);
            if Path::new(&resolved).exists() {
                entries.push(resolved);
            }
        }
        if !entries.is_empty() {
            return Some(entries);
        }
    }

    let index_ts = dir.join("index.ts");
    if index_ts.exists() {
        return Some(vec![index_ts.to_string_lossy().into_owned()]);
    }
    let index_js = dir.join("index.js");
    if index_js.exists() {
        return Some(vec![index_js.to_string_lossy().into_owned()]);
    }
    None
}

/// 在目录中发现扩展入口（**只一层**）。
///
/// 规则（`spec/ca-extensions.md` §2.2.8）：
/// 1. 直接文件：`.ts`/`.js`（`isFile` **或** `isSymbolicLink`）→ 收集。
/// 2. 子目录或符号链接 → [`resolve_extension_entries`]；有入口则展开。
///
/// 起始目录不存在或任何 IO 错误 → `[]`（丢弃已收集结果，与 TS 的 `catch { return [] }` 一致）。
/// **不排序**，顺序即 `readdir` 顺序。
///
/// 名字不是 `.ts`/`.js` 的普通文件被静默忽略；名字不以 `.ts`/`.js` 结尾的符号链接会像子目录
/// 一样尝试入口解析（链接目标设成扩展目录也能被发现）。同一目录内不去重，入口展开的结果
/// 直接追加。
///
/// TS: `loader.ts#discoverExtensionsInDir`。
pub fn discover_extensions_in_dir(dir: &Path) -> Vec<String> {
    if !dir.exists() {
        return Vec::new();
    }

    let read_dir = match std::fs::read_dir(dir) {
        Ok(read_dir) => read_dir,
        Err(_) => return Vec::new(),
    };

    let mut discovered: Vec<String> = Vec::new();
    for entry in read_dir {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => return Vec::new(),
        };
        let entry_path: PathBuf = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        // `file_type()` 不跟随符号链接：链接在这里是 `is_symlink() == true`，`is_file()` /
        // `is_dir()` 为 false，所以下面两个分支都用 `is_symlink` 显式补上链接的情况。
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(_) => return Vec::new(),
        };
        let is_symlink = file_type.is_symlink();

        // 1. 直接文件：`*.ts` / `*.js`（符号链接按名字判定，不跟随目标类型）。
        if (file_type.is_file() || is_symlink) && is_extension_file(&name) {
            discovered.push(entry_path.to_string_lossy().into_owned());
            continue;
        }

        // 2/3. 子目录与符号链接：解析 manifest / index 入口。
        if (file_type.is_dir() || is_symlink)
            && let Some(entries) = resolve_extension_entries(&entry_path)
        {
            discovered.extend(entries);
        }
    }

    discovered
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_file_names_are_ts_or_js_only() {
        for name in ["a.ts", "a.js", ".ts", ".js", "x.y.ts"] {
            assert!(is_extension_file(name), "{name}");
        }
        for name in ["a.mjs", "a.cjs", "a.tsx", "a.TS", "ts", ""] {
            assert!(!is_extension_file(name), "{name}");
        }
    }

    #[test]
    fn resolve_entries_does_not_expand_tilde() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("~entry.ts"), "").expect("write");
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"kiana":{"extensions":["~entry.ts","~/missing.ts"]}}"#,
        )
        .expect("write");
        let entries = resolve_extension_entries(dir.path()).expect("entries");
        assert_eq!(entries.len(), 1);
        assert_eq!(
            Path::new(&entries[0]),
            dir.path().join("~entry.ts").as_path()
        );
    }

    #[test]
    fn index_prefers_ts_and_falls_back_to_js() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(resolve_extension_entries(dir.path()), None);
        std::fs::write(dir.path().join("index.js"), "").expect("write");
        assert!(resolve_extension_entries(dir.path()).expect("js")[0].ends_with("index.js"));
        std::fs::write(dir.path().join("index.ts"), "").expect("write");
        assert!(resolve_extension_entries(dir.path()).expect("ts")[0].ends_with("index.ts"));
    }

    #[test]
    fn discovery_returns_empty_for_missing_directory() {
        let missing = Path::new("/definitely/not/a/real/kiana-extension-dir");
        assert!(discover_extensions_in_dir(missing).is_empty());
    }
}
