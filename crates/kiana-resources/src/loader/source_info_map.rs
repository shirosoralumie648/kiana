//! `find_source_info_for_path` / `get_default_source_info_for_path` / `merge_paths`
//! 与两个路径解析入口。
//!
//! TS: `packages/coding-agent/src/core/resource-loader.ts` 的同名私有方法。
//!
//! 顺序敏感（`spec/ca-resources.md` §8.4 第 9 条）：[`find_source_info_for_path`] 先按插入序
//! 遍历 `extendResources` 注入的 extra source infos，再精确查 metadata，最后按插入序做前缀
//! 匹配。`IndexMap` 的插入序就是 TS `Map` 的插入序。
//!
//! # 为什么单独成模块
//! 四条加载链（技能、prompt、主题、扩展）的 `SourceInfo` 归属判定都经
//! [`find_source_info_for_path`] 集中在这里；主题与扩展链的路径解析也用本模块入口，
//! 技能与 prompt 链的路径解析则由 skills 模块的等价辅助函数承担。TS 在这几处混用了
//! `node:path#resolve` 与
//! `utils/paths.ts#resolvePath` 两套语义，本模块把它们拆成不同入口，调用方必须按 TS 的
//! 原位置选择，混用会改变 `~` 与 `file://` 的处理结果。
//!
//! # 三个解析入口的区别
//! - `resolve_from_process_cwd`：`node:path#resolve` 语义，不展开 `~`、不识别 `file://`；
//! - `resolve_path_from_process_cwd`：`utils/paths.ts#resolvePath` 的默认选项；
//! - `resolve_resource_path` / `resolve_extension_load_path`：带显式 `cwd`，合成路径原样
//!   返回；前者 trim 输入，后者不 trim 但归一 Unicode 空格。
//!
//! # 归属判定的优先级
//! [`find_source_info_for_path`] 的顺序是外部契约：扩展注入的来源 → metadata 精确匹配 →
//! metadata 插入序前缀匹配。改动顺序会改变资源最终报告的 `source` / `scope`。

use std::collections::HashSet;
use std::path::Path;

use indexmap::IndexMap;
use kiana_core::paths::{PathInputOptions, canonicalize_path, resolve_path};
use kiana_core::source_info::{
    PathMetadata, SourceInfo, SourceOrigin, SourceScope, create_source_info,
    get_synthetic_path_source, is_synthetic_path,
};

/// 进程当前目录（`process.cwd()`）。
///
/// `current_dir()` 失败时回退到 `"."`；此时后续解析结果可能不再是绝对路径，但纯词法解析
/// 仍能继续，不会 panic。
pub(crate) fn process_cwd() -> String {
    std::env::current_dir()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|_| ".".to_string())
}

/// `resolve(p)`：**node:path** 语义，以 `process.cwd()` 为基准。
///
/// TS 的 `findSourceInfoForPath` / `getDefaultSourceInfoForPath` / `isUnderPath` 导入的是
/// `node:path` 的 `resolve`，它把 `~` 与 `file://` 当普通路径分量
/// （`resolve("~/x")` = `<cwd>/~/x`，`resolve("file:///x")` = `<cwd>/file:/x`）。
/// 不能直接用 [`kiana_core::paths::resolve_path`]（TS 的 `utils/paths.ts#resolvePath`）：
/// 后者会展开 `~` 并转换 `file://` URL。
pub(crate) fn resolve_from_process_cwd(path: &str) -> String {
    // `normalize_path` 会把开头的 `file://` 转成文件系统路径；node:path 则把 `file:`
    // 当作普通目录名。改写成不触发该分支的等价相对形式 `file:/<rest>`（node 忽略空段，
    // 多余的 `/` 与 `..` 归一化结果相同），再交给词法 `resolve`。
    let literal = match path.strip_prefix("file://") {
        Some(rest) => format!("file:/{}", rest.trim_start_matches('/')),
        None => path.to_string(),
    };
    resolve_path(
        &literal,
        &process_cwd(),
        &PathInputOptions {
            expand_tilde: false,
            ..Default::default()
        },
    )
}

/// `resolvePath(p)`（TS `utils/paths.ts`，**默认选项**、以 `process.cwd()` 为基准）。
///
/// 与 [`resolve_from_process_cwd`] 的区别：展开 `~`、转换 `file://` URL。
/// 用于 TS 里调用 `resolvePath` 而非 `resolve` 的位置（系统提示源路径）。
pub(crate) fn resolve_path_from_process_cwd(path: &str) -> String {
    resolve_path(path, &process_cwd(), &PathInputOptions::default())
}

/// `join(a, b)`（node:path 的字符串形态）。
///
/// 非 UTF-8 路径段会被有损替换为 U+FFFD。与 Rust 语义有关的注意事项：`b` 是绝对路径时
/// `Path::join` 会**丢弃** `a`；本文件与 `loader` 的调用点传入的 `b` 都是单段名字
/// （`AGENTS.md`、`themes`、`package.json` 等），不会触发该分支。
pub(crate) fn join_path(a: &str, b: &str) -> String {
    Path::new(a).join(b).to_string_lossy().into_owned()
}

/// `basename(p)`。
///
/// 与 Rust `Path::file_name` 一致：路径以 `..`、根或空串结尾时返回空字符串；尾随分隔符
/// 会被忽略（`"/a/b/"` → `"b"`）。调用方用它取候选文件名，正常输入不会落到空串。
pub(crate) fn basename(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// `dirname(p)`；根目录返回自身（与 `path.dirname("/") === "/"` 一致）。
///
/// 根目录由 `parent()` 返回 `None` 触发回退，得到自身；单段相对路径（如 `"a"`）的
/// `parent()` 是空路径，因此返回空字符串。`load_project_context_files` 的向上循环依赖
/// “根目录返回自身”这一回退来判断终止。
pub(crate) fn dirname(path: &str) -> String {
    Path::new(path)
        .parent()
        .map(|parent| parent.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

/// `resolveResourcePath(p)`：合成路径原样返回，否则 `resolvePath(p, cwd, { trim: true })`。
/// TS: `resource-loader.ts#resolveResourcePath`。
///
/// `trim` 按 JS `String.prototype.trim()` 去掉输入首尾空白，不改变路径内部内容。`loader`
/// 的主题/扩展路径、`ResourceExtensionPath` 的 `path` 与 `base_dir` 都经此解析。
pub fn resolve_resource_path(cwd: &str, path: &str) -> String {
    if is_synthetic_path(path) {
        return path.to_string();
    }
    resolve_path(
        path,
        cwd,
        &PathInputOptions {
            trim: true,
            ..Default::default()
        },
    )
}

/// `resolveExtensionLoadPath(p)`：合成路径原样返回，否则
/// `resolvePath(p, cwd, { normalizeUnicodeSpaces: true })`（**不 trim**）。
/// TS: `resource-loader.ts#resolveExtensionLoadPath`。
///
/// 与 [`resolve_resource_path`] 的差别只有两点：不 trim 输入（首尾空白原样保留，测试
/// `resolve_helpers_leave_synthetic_paths_untouched` 锁定），并把 Unicode 空格变体归一为
/// 普通空格。用于扩展加载路径作为 `loadFinalExtensionSet` 的去重键。
pub fn resolve_extension_load_path(cwd: &str, path: &str) -> String {
    if is_synthetic_path(path) {
        return path.to_string();
    }
    resolve_path(
        path,
        cwd,
        &PathInputOptions {
            normalize_unicode_spaces: true,
            ..Default::default()
        },
    )
}

/// `isUnderPath(target, root)`：相等或 `target` 以 `root + sep` 开头（**字符串**比较）。
/// TS: `resource-loader.ts#isUnderPath`。
///
/// `root` 会先经 `resolve_from_process_cwd`（node:path 语义）规范化；`target` 按传入值
/// 参与比较、不再规范化，因此调用方应传入已解析的路径（`get_default_source_info_for_path`
/// 传的就是解析结果）。比较区分大小写，不解析符号链接，也不要求路径真实存在。
pub fn is_under_path(target: &str, root: &str) -> bool {
    let normalized_root = resolve_from_process_cwd(root);
    if target == normalized_root {
        return true;
    }
    let sep = std::path::MAIN_SEPARATOR;
    let prefix = if normalized_root.ends_with(sep) {
        normalized_root
    } else {
        format!("{normalized_root}{sep}")
    };
    target.starts_with(&prefix)
}

/// `findSourceInfoForPath(resourcePath, extraSourceInfos?, metadataByPath?)`。
/// TS: `resource-loader.ts#findSourceInfoForPath`。
///
/// - 空路径 → `None`。
/// - 合成路径 → [`get_default_source_info_for_path`]（结果只取决于合成前缀，不会 stat）。
/// - `extraSourceInfos`：按插入序，`resolve` 后相等或以 `sourcePath + sep` 开头 → 命中，
///   返回 `{ ...sourceInfo, path: resourcePath }`。
/// - `metadataByPath`：先精确查（规范化路径，再原文路径），再按插入序前缀匹配。
/// - 都未命中 → `None`。
///
/// `cwd` / `agent_dir` 只用于合成路径分支的默认推断；其余分支完全不读取它们（保留参数
/// 是为了与 TS 签名一致）。非合成分支自身不产生 IO 错误，返回值里的 `Err` 只可能来自
/// 合成路径分支的默认推断（它会 `stat`）。
///
/// 扩展来源命中时返回的是克隆后把 `path` 换成**原始** `resource_path`（未规范化）的
/// `SourceInfo`，其余字段沿用注入值；metadata 命中则由 `create_source_info` 原样构造。
/// 前缀匹配按 `IndexMap` 插入序取第一个命中，插入顺序就是嵌套路径的归属优先级。
pub fn find_source_info_for_path(
    cwd: &str,
    agent_dir: &str,
    resource_path: &str,
    extra_source_infos: Option<&IndexMap<String, SourceInfo>>,
    metadata_by_path: Option<&IndexMap<String, PathMetadata>>,
) -> std::io::Result<Option<SourceInfo>> {
    if resource_path.is_empty() {
        return Ok(None);
    }
    if is_synthetic_path(resource_path) {
        return get_default_source_info_for_path(cwd, agent_dir, resource_path).map(Some);
    }

    let normalized_resource_path = resolve_from_process_cwd(resource_path);
    if let Some(extra_source_infos) = extra_source_infos {
        for (source_path, source_info) in extra_source_infos {
            let normalized_source_path = resolve_from_process_cwd(source_path);
            if normalized_resource_path == normalized_source_path
                || is_under_normalized(&normalized_resource_path, &normalized_source_path)
            {
                return Ok(Some(SourceInfo {
                    path: resource_path.to_string(),
                    ..source_info.clone()
                }));
            }
        }
    }

    if let Some(metadata_by_path) = metadata_by_path {
        let exact = metadata_by_path
            .get(&normalized_resource_path)
            .or_else(|| metadata_by_path.get(resource_path));
        if let Some(metadata) = exact {
            return Ok(Some(create_source_info(resource_path, metadata)));
        }
        for (source_path, metadata) in metadata_by_path {
            let normalized_source_path = resolve_from_process_cwd(source_path);
            if normalized_resource_path == normalized_source_path
                || is_under_normalized(&normalized_resource_path, &normalized_source_path)
            {
                return Ok(Some(create_source_info(resource_path, metadata)));
            }
        }
    }

    Ok(None)
}

/// `normalizedResourcePath.startsWith(`${normalizedSourcePath}${sep}`)`
/// （`===` 由调用方单独判断，两者分开写与 TS 的 `||` 结构一致）。
///
/// TS **无条件**拼接 `${normalizedSourcePath}${sep}`：源路径是文件系统根（`/` 或 `C:\\`）时
/// 前缀是 `//`，任何资源都不匹配。这里不能特判结尾分隔符（`is_under_path` 才是另一种语义）。
///
/// 两个参数都不再规范化，调用方必须传入 `resolve_from_process_cwd` 的结果；比较是纯字符串
/// 操作，不访问文件系统。测试
/// `source_info_prefix_matches_concatenate_the_separator_unconditionally` 锁定根路径行为。
fn is_under_normalized(target: &str, normalized_root: &str) -> bool {
    let sep = std::path::MAIN_SEPARATOR;
    target.starts_with(&format!("{normalized_root}{sep}"))
}

/// Node `fs` 错误的 `error.message`（`ENOENT: no such file or directory, stat '<path>'`）。
///
/// TS 的 `loadThemes` / `loadContextFileFromDir` / `resolvePromptInput` 把 Node 的
/// `error.message`（主题诊断）或 `Error.toString()`（console 警告，见
/// [`node_error_to_string`]）拼进用户可见文本。这里复刻 libuv 的 errno 名称与描述；
/// 未知错误退回 [`std::io::Error`] 的 Display。
///
/// `IsADirectory` 是特例：Node 的 `readFileSync` 对目录是打开成功、读取失败，因此输出固定
/// 为 `"<code>: <description>, read"`，**忽略** `syscall` 参数且不带路径。其余错误输出
/// `"<code>: <description>, <syscall> '<path>'"`。错误分类先看 `ErrorKind`，覆盖不到的
/// errno 再按 `raw_os_error()` 的数值判断（`ELOOP` 的编号在 macOS 与其它平台不同，代码里
/// 按 `cfg(target_os)` 区分）。
pub(crate) fn node_fs_error_message(error: &std::io::Error, syscall: &str, path: &str) -> String {
    let Some((code, description)) = node_error_code_and_description(error) else {
        return error.to_string();
    };
    if error.kind() == std::io::ErrorKind::IsADirectory {
        // `readFileSync` 对目录是打开成功、读取失败：Node 用 "read" 动作且不带路径。
        return format!("{code}: {description}, read");
    }
    format!("{code}: {description}, {syscall} '{path}'")
}

/// Node `Error.prototype.toString()`（`Error: <message>`），对应 TS 模板里的 `${error}`。
pub(crate) fn node_error_to_string(error: &std::io::Error, syscall: &str, path: &str) -> String {
    format!("Error: {}", node_fs_error_message(error, syscall, path))
}

/// libuv 的 `uv_strerror()` errno 名与描述。
///
/// 返回 `None` 表示该错误既不在 `ErrorKind` 覆盖范围内、也不在下面的常见 errno 表里；
/// 调用方据此退回 `std::io::Error` 的 Display，而不是编造一个 errno 文案。表里也重复列了
/// `EISDIR`（21）等常见 errno，用于 `ErrorKind` 未覆盖时的兜底。
fn node_error_code_and_description(error: &std::io::Error) -> Option<(&'static str, &'static str)> {
    use std::io::ErrorKind;
    let mapped = match error.kind() {
        ErrorKind::NotFound => ("ENOENT", "no such file or directory"),
        ErrorKind::PermissionDenied => ("EACCES", "permission denied"),
        ErrorKind::IsADirectory => ("EISDIR", "illegal operation on a directory"),
        ErrorKind::NotADirectory => ("ENOTDIR", "not a directory"),
        _ => {
            // 常见但 `ErrorKind` 未覆盖的 errno（Linux 编号；macOS 的 ELOOP 不同）。
            #[cfg(target_os = "macos")]
            const ELOOP: i32 = 62;
            #[cfg(not(target_os = "macos"))]
            const ELOOP: i32 = 40;
            match error.raw_os_error()? {
                1 => ("EPERM", "operation not permitted"),
                17 => ("EEXIST", "file already exists"),
                18 => ("EXDEV", "cross-device link not permitted"),
                21 => ("EISDIR", "illegal operation on a directory"),
                22 => ("EINVAL", "invalid argument"),
                28 => ("ENOSPC", "no space left on device"),
                30 => ("EROFS", "read-only file system"),
                value if value == ELOOP => ("ELOOP", "too many symbolic links encountered"),
                _ => return None,
            }
        }
    };
    Some(mapped)
}

/// `getDefaultSourceInfoForPath(filePath)`。TS: `resource-loader.ts#getDefaultSourceInfoForPath`。
///
/// 1. 合成路径 → `{path, source: 前缀, scope: "temporary", origin: "top-level"}`。
/// 2. `agentDir/{skills,prompts,themes,extensions}` 之下 → `scope: "user"`，`baseDir: root`。
/// 3. `cwd/.kiana/{skills,prompts,themes,extensions}` 之下 → `scope: "project"`，`baseDir: root`。
/// 4. 否则 `scope: "temporary"`，`baseDir` 为文件所在目录（对目录取自身）。
///    第 4 步的 `stat` 失败会返回错误（TS 里 `statSync` 未捕获，`reload()` 会 reject）。
///
/// 判定顺序固定：`agent_dir/{skills,prompts,themes,extensions}` 先于
/// `cwd/.kiana/{skills,prompts,themes,extensions}`（`CONFIG_DIR_NAME` 即 `.kiana`），每组内
/// 顺序为 skills → prompts → themes → extensions。命中时 `source` 固定为 `"local"`，
/// `base_dir` 是命中的根目录本身（不是资源文件所在目录）。
///
/// 第 4 步先 `stat` 判断类型：目录的 `base_dir` 是它自身，文件的 `base_dir` 是其父目录
/// （`resolve_path("..")` 纯词法求得）。`stat` 失败的错误文本被包装成 Node 风格的文件错误
/// 形式 `... stat '<path>'`，这是 `ResourceLoaderError::Io` 的 Display 稳定契约（改动需同步测试）。
pub fn get_default_source_info_for_path(
    cwd: &str,
    agent_dir: &str,
    file_path: &str,
) -> std::io::Result<SourceInfo> {
    if let Some(source) = get_synthetic_path_source(file_path) {
        return Ok(SourceInfo {
            path: file_path.to_string(),
            source,
            scope: SourceScope::Temporary,
            origin: SourceOrigin::TopLevel,
            base_dir: None,
        });
    }

    let normalized_path = resolve_from_process_cwd(file_path);
    let agent_roots = [
        join_path(agent_dir, "skills"),
        join_path(agent_dir, "prompts"),
        join_path(agent_dir, "themes"),
        join_path(agent_dir, "extensions"),
    ];
    let config_dir = join_path(cwd, kiana_core::config::CONFIG_DIR_NAME);
    let project_roots = [
        join_path(&config_dir, "skills"),
        join_path(&config_dir, "prompts"),
        join_path(&config_dir, "themes"),
        join_path(&config_dir, "extensions"),
    ];

    for root in &agent_roots {
        if is_under_path(&normalized_path, root.as_str()) {
            return Ok(SourceInfo {
                path: file_path.to_string(),
                source: "local".to_string(),
                scope: SourceScope::User,
                origin: SourceOrigin::TopLevel,
                base_dir: Some(root.clone()),
            });
        }
    }

    for root in project_roots {
        if is_under_path(&normalized_path, root.as_str()) {
            return Ok(SourceInfo {
                path: file_path.to_string(),
                source: "local".to_string(),
                scope: SourceScope::Project,
                origin: SourceOrigin::TopLevel,
                base_dir: Some(root.clone()),
            });
        }
    }

    let is_dir = std::fs::metadata(&normalized_path)
        .map_err(|error| {
            // `stat` 失败未被捕获：`reload()` 以底层 `error.message` 拒绝
            // （`ENOENT: no such file or directory, stat '<path>'`）。把同一文本装进
            // `io::Error`，使 `ResourceLoaderError::Io` 的 Display 呈现 Node 风格的文件错误
            // 文本（稳定契约，改动需同步测试）。
            std::io::Error::new(
                error.kind(),
                node_fs_error_message(&error, "stat", &normalized_path),
            )
        })?
        .is_dir();
    let base_dir = if is_dir {
        normalized_path
    } else {
        resolve_path("..", &normalized_path, &PathInputOptions::default())
    };
    Ok(SourceInfo {
        path: file_path.to_string(),
        source: "local".to_string(),
        scope: SourceScope::Temporary,
        origin: SourceOrigin::TopLevel,
        base_dir: Some(base_dir),
    })
}

/// `mergePaths(primary, additional)`：解析 + 规范化去重，**保留首次出现**。
/// TS: `resource-loader.ts#mergePaths`。
///
/// `canonicalizePath` 失败时回退原串（`kiana_core::paths::canonicalize_path`）。
///
/// 去重键是 `canonicalize_path(resolved)`（符号链接展开 + 词法规范化；失败时回退到
/// `resolved` 本身），但输出保留**第一次**出现时的 `resolved` 字符串，不输出 canonical
/// 形式。`primary` 在前、`additional` 在后，因此同一路径的写法以 primary 为准。
pub fn merge_paths(cwd: &str, primary: &[String], additional: &[String]) -> Vec<String> {
    let mut merged = Vec::with_capacity(primary.len() + additional.len());
    let mut seen: HashSet<String> = HashSet::new();
    for path in primary.iter().chain(additional) {
        let resolved = resolve_resource_path(cwd, path);
        let canonical_path = canonicalize_path(&resolved);
        if !seen.insert(canonical_path) {
            continue;
        }
        merged.push(resolved);
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_paths_keeps_first_occurrence_and_resolves_duplicates() {
        let cwd = std::env::temp_dir().join("kiana-merge-paths-test");
        std::fs::create_dir_all(&cwd).expect("create cwd");
        let cwd = cwd.to_string_lossy().into_owned();
        let file = format!("{cwd}/x.md");
        std::fs::write(&file, "").expect("write file");
        let merged = merge_paths(
            &cwd,
            &[file.clone(), format!("{cwd}/missing.md")],
            &[format!("{cwd}/./x.md"), "builtin:mcp".to_string()],
        );
        assert_eq!(
            merged,
            vec![file, format!("{cwd}/missing.md"), "builtin:mcp".to_string()]
        );
        std::fs::remove_dir_all(&cwd).ok();
    }

    #[test]
    fn resolve_helpers_leave_synthetic_paths_untouched() {
        let cwd = "/tmp";
        assert_eq!(resolve_resource_path(cwd, "builtin:read"), "builtin:read");
        assert_eq!(resolve_resource_path(cwd, "<inline:1>"), "<inline:1>");
        assert_eq!(
            resolve_extension_load_path(cwd, "builtin:read"),
            "builtin:read"
        );
        assert_eq!(
            resolve_extension_load_path(cwd, "  relative.ts  "),
            "/tmp/  relative.ts  "
        );
    }

    #[test]
    fn find_source_info_prefers_extra_sources_then_exact_metadata_then_prefixes() {
        let cwd = std::env::temp_dir().join("kiana-find-source-test");
        std::fs::create_dir_all(&cwd).expect("create cwd");
        let cwd = cwd.to_string_lossy().into_owned();
        let agent_dir = format!("{cwd}/agent");
        std::fs::create_dir_all(&agent_dir).expect("create agent");

        let mut extra = IndexMap::new();
        extra.insert(
            format!("{cwd}/extra"),
            SourceInfo {
                path: format!("{cwd}/extra"),
                source: "extra".to_string(),
                scope: SourceScope::Temporary,
                origin: SourceOrigin::TopLevel,
                base_dir: None,
            },
        );
        let mut metadata = IndexMap::new();
        metadata.insert(
            format!("{cwd}/extra/nested"),
            PathMetadata {
                source: "nested".to_string(),
                scope: SourceScope::Temporary,
                origin: SourceOrigin::TopLevel,
                base_dir: None,
                package_root: None,
            },
        );
        metadata.insert(
            format!("{cwd}/extra/nested/deeper"),
            PathMetadata {
                source: "deeper".to_string(),
                scope: SourceScope::Temporary,
                origin: SourceOrigin::TopLevel,
                base_dir: None,
                package_root: None,
            },
        );

        let info = find_source_info_for_path(
            &cwd,
            &agent_dir,
            &format!("{cwd}/extra/nested/deeper/SKILL.md"),
            Some(&extra),
            Some(&metadata),
        )
        .expect("find")
        .expect("info");
        assert_eq!(info.source, "extra");
        assert_eq!(info.path, format!("{cwd}/extra/nested/deeper/SKILL.md"));

        let info = find_source_info_for_path(
            &cwd,
            &agent_dir,
            &format!("{cwd}/extra/nested/deeper/SKILL.md"),
            None,
            Some(&metadata),
        )
        .expect("find")
        .expect("info");
        assert_eq!(info.source, "nested");

        assert_eq!(
            find_source_info_for_path(&cwd, &agent_dir, &format!("{cwd}/elsewhere"), None, None)
                .expect("find"),
            None
        );
        std::fs::remove_dir_all(&cwd).ok();
    }

    #[test]
    fn default_source_info_maps_agent_and_project_roots_and_falls_back_to_stat() {
        let root = std::env::temp_dir().join("kiana-default-source-test");
        let agent_dir = root.join("agent");
        let cwd = root.join("project");
        std::fs::create_dir_all(agent_dir.join("skills")).expect("agent skills");
        std::fs::create_dir_all(cwd.join(".kiana/skills")).expect("project skills");
        let cwd_str = cwd.to_string_lossy().into_owned();
        let agent_str = agent_dir.to_string_lossy().into_owned();

        let info = get_default_source_info_for_path(
            &cwd_str,
            &agent_str,
            &format!("{agent_str}/skills/a/SKILL.md"),
        )
        .expect("agent info");
        assert_eq!(info.scope, SourceScope::User);
        assert_eq!(
            info.base_dir.as_deref(),
            Some(format!("{agent_str}/skills").as_str())
        );

        let info = get_default_source_info_for_path(
            &cwd_str,
            &agent_str,
            &format!("{cwd_str}/.kiana/skills/a/SKILL.md"),
        )
        .expect("project info");
        assert_eq!(info.scope, SourceScope::Project);

        std::fs::write(root.join("loose.md"), "").expect("loose file");
        let info = get_default_source_info_for_path(
            &cwd_str,
            &agent_str,
            &root.join("loose.md").to_string_lossy(),
        )
        .expect("fallback info");
        assert_eq!(info.scope, SourceScope::Temporary);
        assert_eq!(
            info.base_dir.as_deref(),
            Some(root.to_string_lossy().as_ref())
        );

        assert!(
            get_default_source_info_for_path(
                &cwd_str,
                &agent_str,
                &root.join("missing.md").to_string_lossy(),
            )
            .is_err()
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn is_under_path_uses_a_string_separator_boundary() {
        assert!(is_under_path("/a/b", "/a/b"));
        assert!(is_under_path("/a/b/c", "/a/b"));
        assert!(!is_under_path("/a/bc", "/a/b"));
    }

    #[test]
    fn source_info_prefix_matches_concatenate_the_separator_unconditionally() {
        // TS：`resourcePath.startsWith(`${sourcePath}${sep}`)`，源路径为根时前缀是 `//`。
        let cwd = std::env::temp_dir().join("kiana-root-prefix-test");
        std::fs::create_dir_all(&cwd).expect("create cwd");
        let cwd = cwd.to_string_lossy().into_owned();
        let agent_dir = format!("{cwd}/agent");
        std::fs::create_dir_all(&agent_dir).expect("create agent");

        let mut extra = IndexMap::new();
        extra.insert(
            std::path::MAIN_SEPARATOR.to_string(),
            SourceInfo {
                path: std::path::MAIN_SEPARATOR.to_string(),
                source: "root".to_string(),
                scope: SourceScope::Temporary,
                origin: SourceOrigin::TopLevel,
                base_dir: None,
            },
        );
        let mut metadata = IndexMap::new();
        metadata.insert(
            std::path::MAIN_SEPARATOR.to_string(),
            PathMetadata {
                source: "root".to_string(),
                scope: SourceScope::Temporary,
                origin: SourceOrigin::TopLevel,
                base_dir: None,
                package_root: None,
            },
        );

        let target = format!("{cwd}/loose/SKILL.md");
        assert_eq!(
            find_source_info_for_path(&cwd, &agent_dir, &target, Some(&extra), None).expect("find"),
            None,
            "a `/` extra source must not prefix-match every absolute path"
        );
        assert_eq!(
            find_source_info_for_path(&cwd, &agent_dir, &target, None, Some(&metadata))
                .expect("find"),
            None,
            "a `/` metadata entry must not prefix-match every absolute path"
        );
        std::fs::remove_dir_all(&cwd).ok();
    }

    #[test]
    fn node_fs_error_messages_match_node_error_and_tostring() {
        let denied = std::io::Error::from_raw_os_error(13);
        assert_eq!(
            node_fs_error_message(&denied, "stat", "/x"),
            "EACCES: permission denied, stat '/x'"
        );
        assert_eq!(
            node_error_to_string(&denied, "open", "/x"),
            "Error: EACCES: permission denied, open '/x'"
        );
        let missing = std::io::Error::from_raw_os_error(2);
        assert_eq!(
            node_fs_error_message(&missing, "scandir", "/x"),
            "ENOENT: no such file or directory, scandir '/x'"
        );
        let not_dir = std::io::Error::from_raw_os_error(20);
        assert_eq!(
            node_fs_error_message(&not_dir, "stat", "/x"),
            "ENOTDIR: not a directory, stat '/x'"
        );
        let is_dir = std::io::Error::from_raw_os_error(21);
        assert_eq!(
            node_fs_error_message(&is_dir, "open", "/x"),
            "EISDIR: illegal operation on a directory, read"
        );
    }

    #[test]
    fn resolve_from_process_cwd_matches_node_path_resolve() {
        // node:path `resolve` 不展开 `~`、不转换 `file://`。期望值由 node 26 生成。
        let cwd = process_cwd();
        assert_eq!(resolve_from_process_cwd("~/x"), format!("{cwd}/~/x"));
        assert_eq!(
            resolve_from_process_cwd("file:///x"),
            format!("{cwd}/file:/x")
        );
        assert_eq!(
            resolve_from_process_cwd("file:////x"),
            format!("{cwd}/file:/x")
        );
        assert_eq!(resolve_from_process_cwd("file://"), format!("{cwd}/file:"));
        assert_eq!(
            resolve_from_process_cwd("file://host/x"),
            format!("{cwd}/file:/host/x")
        );
        assert_eq!(
            resolve_from_process_cwd("a/../file:///x"),
            format!("{cwd}/file:/x")
        );
        assert_eq!(
            resolve_from_process_cwd("file:///a/../b"),
            format!("{cwd}/file:/b")
        );
        assert_eq!(resolve_from_process_cwd("file:///../x"), format!("{cwd}/x"));
        assert_eq!(resolve_from_process_cwd("/a//b/../c"), "/a/c");
    }

    #[test]
    fn resolve_path_from_process_cwd_keeps_utils_paths_semantics() {
        // TS 的系统提示源路径用 `utils/paths.ts#resolvePath`：展开 `~`、转换 `file://`。
        assert_eq!(resolve_path_from_process_cwd("file:///x"), "/x");
        let home = dirs::home_dir().expect("home directory");
        assert_eq!(
            resolve_path_from_process_cwd("~/x"),
            home.join("x").to_string_lossy().into_owned()
        );
    }

    #[test]
    fn default_source_info_stat_error_message_matches_node_stat_sync() {
        let missing = std::env::temp_dir()
            .join(format!("kiana-missing-source-{}", std::process::id()))
            .join("SKILL.md");
        let missing = missing.to_string_lossy().into_owned();
        let cwd = std::env::temp_dir().to_string_lossy().into_owned();
        let error = get_default_source_info_for_path(&cwd, &cwd, &missing)
            .expect_err("statSync must throw for a missing path");
        assert_eq!(
            error.to_string(),
            format!("ENOENT: no such file or directory, stat '{missing}'")
        );
    }
}
