//! 扩展加载警告：宿主包依赖、安装失败文案、被替换的内置扩展。
//!
//! TS: `packages/coding-agent/src/core/resource-loader.ts` 的
//! `HOST_PROVIDED_EXTENSION_PACKAGES` / `collectExtensionPackageWarnings` /
//! `describePackageInstallFailure` / `formatPackageInstallWarning` /
//! `collectPackageInstallWarnings` / `mergeExtensionWarnings` / `isBuiltinExtension` /
//! `omitReplacedExtensions`。
//!
//! 两条「解析失败」语义在此**刻意不同**（`spec/ca-resources.md` §8.4 第 10 条）：
//! [`collect_extension_package_warnings`] 用 `JSON.parse` 等价物，坏 JSON **返回错误**；
//! 包管理的 `read_kiana_manifest` 对同一文件返回 `None`。不要统一。
//!
//! # 负责的边界
//! 只生成用户可见的警告/错误文本、合并与过滤扩展集合，不做日志输出、不持久化。返回的
//! `LoadWarning` / `LoadExtensionsResult` 会原样进入公开 getter，因此文案与顺序都是可观察
//! 契约。
//!
//! # 在 reload 中的位置
//! - `collect_package_install_warnings`：第 7 步（准备阶段）汇总安装失败；
//! - `collect_extension_package_warnings`：第 15 步检查扩展包的 `package.json`；
//! - `merge_extension_warnings`：第 17 步把上述两类警告并入扩展结果；
//! - `omit_replaced_extensions`：最终扩展集合加载之后，丢弃被同名扩展占位的 replaceable 项；
//! - `is_builtin_extension`：`DefaultResourceLoader::new` 分流内置与普通内联工厂。
//!
//! # 注意事项
//! - 文案逐字对齐 TS（含空格与标点）；改动会破坏本文件与 `tests/resource_loader.rs`
//!   中的单测断言。
//! - [`collect_extension_package_warnings`] 用 `std::fs::read` 同步读清单，在异步 reload
//!   中是一段阻塞 IO；它返回的 `Err` 会让整个 reload 提前失败（TS 的未捕获抛出）。

use std::fmt;
use std::path::Path;

use indexmap::IndexMap;
use kiana_core::config::APP_NAME;
use kiana_core::source_info::{PathMetadata, SourceScope};
use kiana_core::text::strip_bom;
use kiana_ext_api::types::{Extension, InlineExtension, LoadExtensionsResult, LoadWarning};

use crate::loader::source_info_map::node_fs_error_message;
use crate::package::PackageInstallFailure;

/// 必须声明在 `peerDependencies`（范围 `"*"`）里的宿主内置包（**逐字 10 项**）。
/// TS: `resource-loader.ts#HOST_PROVIDED_EXTENSION_PACKAGES`。
pub const HOST_PROVIDED_EXTENSION_PACKAGES: [&str; 10] = [
    "@earendil-works/kiana-agent-core",
    "@earendil-works/kiana-ai",
    "@earendil-works/kiana-coding-agent",
    "@earendil-works/kiana-tui",
    "@mariozechner/kiana-agent-core",
    "@mariozechner/kiana-ai",
    "@mariozechner/kiana-coding-agent",
    "@mariozechner/kiana-tui",
    "@sinclair/typebox",
    "typebox",
];

/// 扩展包的 `package.json` 无法读取或解析。
///
/// TS 的 `collectExtensionPackageWarnings` 里 `readFileSync` / `JSON.parse` 抛出的错误
/// 都未被捕获，会一路冒泡让 `reload()` reject（`resource-loader.test.ts` 的
/// `should fail when an extension package manifest cannot be parsed`）。`Display` 逐字等于
/// TS 的 `error.message`（读取失败是 Node 的 `ENOENT/EACCES/EISDIR ...` 文本，
/// 解析失败是 `SyntaxError` 文本）。
///
/// `Display` 只输出 `message`，**不含** `path`；`path` 供调用方定位。该类型被
/// `ResourceLoaderError::Manifest` 包装，reload 直接把这里的 `message` 作为错误文本。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestParseError {
    /// 出错的 `package.json` 路径。
    pub path: String,
    /// `error.message`。
    pub message: String,
}

impl fmt::Display for ManifestParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ManifestParseError {}

/// 读取并解析扩展包清单。
///
/// 读取失败时 `error.message` 与 Node `readFileSync` 一致
/// （`EACCES: permission denied, open '<path>'` / `EISDIR: illegal operation on a directory, read`），
/// 因为 TS 里该错误未被捕获，会原样成为 `reload()` 的 rejection。
///
/// 解码顺序：`std::fs::read` 原始字节 → UTF-8 有损解码 → 去 BOM → `kiana_json::parse`。
/// 后者是 `JSON.parse` 的等价实现（错误文本为 V8 模板），因此 `null`、数组、字符串都能被
/// 接受，合法性判断交给调用方。读取失败与解析失败都包成 [`ManifestParseError`]，`path`
/// 固定为传入路径。
fn read_manifest(path: &str) -> Result<serde_json::Value, ManifestParseError> {
    let error = |message: String| ManifestParseError {
        path: path.to_string(),
        message,
    };
    let bytes =
        std::fs::read(path).map_err(|err| error(node_fs_error_message(&err, "open", path)))?;
    let content = String::from_utf8_lossy(&bytes);
    let text = strip_bom(&content);
    kiana_json::parse(&text).map_err(|err| error(err.message))
}

/// 收集「宿主包被写进 `dependencies`」的警告。TS:
/// `resource-loader.ts#collectExtensionPackageWarnings`。
///
/// 1. 取这些扩展路径的 `metadata.packageRoot`（去重、保插入序）。
/// 2. 对每个根读 `<root>/package.json`；`dependencies` 不是普通对象则跳过。
/// 3. `dependencies` 的键里命中 [`HOST_PROVIDED_EXTENSION_PACKAGES`] 的名字排序后用 `", "`
///    连接，push 一条逐字警告。
///
/// 清单不存在时跳过；存在但读/解析失败时返回 [`ManifestParseError`]（TS 的未捕获抛出）。
pub fn collect_extension_package_warnings(
    extension_paths: &[String],
    metadata_by_path: &IndexMap<String, PathMetadata>,
) -> Result<Vec<LoadWarning>, ManifestParseError> {
    let mut roots: Vec<String> = Vec::new();
    let mut seen_roots: std::collections::HashSet<String> = std::collections::HashSet::new();
    for extension_path in extension_paths {
        let Some(package_root) = metadata_by_path
            .get(extension_path)
            .and_then(|metadata| metadata.package_root.as_ref())
        else {
            continue;
        };
        if seen_roots.insert(package_root.clone()) {
            roots.push(package_root.clone());
        }
    }

    let mut warnings: Vec<LoadWarning> = Vec::new();
    for package_root in roots {
        let package_json_path = Path::new(&package_root)
            .join("package.json")
            .to_string_lossy()
            .into_owned();
        if !Path::new(&package_json_path).exists() {
            continue;
        }
        let manifest = read_manifest(&package_json_path)?;
        // `JSON.parse("null")` 得到 `null`；TS 读取 `manifest.dependencies` 会抛 TypeError
        // （未被捕获，reload() reject）。数组/字符串/数字上读属性只得到 undefined，跳过。
        if manifest.is_null() {
            return Err(ManifestParseError {
                path: package_json_path,
                message: "Cannot read properties of null (reading 'dependencies')".to_string(),
            });
        }
        // `typeof dependencies === "object" && dependencies !== null && !Array.isArray(...)`：
        // 只有普通对象才继续；数组/字符串/数字/缺失都跳过。
        let Some(dependencies) = manifest
            .get("dependencies")
            .and_then(|value| value.as_object())
        else {
            continue;
        };
        let mut host_dependencies: Vec<&str> = dependencies
            .keys()
            .filter(|name| HOST_PROVIDED_EXTENSION_PACKAGES.contains(&name.as_str()))
            .map(String::as_str)
            .collect();
        // `Array.prototype.sort()` 默认按 UTF-16 code unit 排序；名单里的名字都是 ASCII。
        host_dependencies.sort_unstable();
        if host_dependencies.is_empty() {
            continue;
        }
        warnings.push(LoadWarning {
            path: package_json_path,
            warning: format!(
                "Host-provided extension packages must be declared in peerDependencies with a \"*\" range, not dependencies: {}. Installed copies can bypass the extension loader and create duplicate runtime modules.",
                host_dependencies.join(", ")
            ),
        });
    }
    Ok(warnings)
}

/// 描述一个安装失败被跳过的包，并给出移除方式。TS:
/// `resource-loader.ts#describePackageInstallFailure`。
pub fn describe_package_install_failure(failure: &PackageInstallFailure) -> String {
    let hint = if failure.scope == SourceScope::Temporary {
        "Check the source and try again.".to_string()
    } else {
        let local = if failure.scope == SourceScope::Project {
            " --local"
        } else {
            ""
        };
        format!(
            "Run \"{APP_NAME} remove {}{local}\" to remove it from settings.json.",
            failure.source
        )
    };
    format!("failed to install ({}). {hint}", failure.message)
}

/// 启动横幅的单行警告。TS: `resource-loader.ts#formatPackageInstallWarning`。
///
/// 与 [`collect_package_install_warnings`] 的差别：这里**带** `Extension package "…":` 前缀。
pub fn format_package_install_warning(failure: &PackageInstallFailure) -> String {
    format!(
        "Extension package \"{}\": {}",
        failure.source,
        describe_package_install_failure(failure)
    )
}

/// 把若干安装失败列表拍平成警告（`path` 是**源字符串**，文案**不带**包名前缀）。
/// TS: `resource-loader.ts#collectPackageInstallWarnings`（TS 的变参语义）。
pub fn collect_package_install_warnings(
    failures: &[Option<&[PackageInstallFailure]>],
) -> Vec<LoadWarning> {
    let mut warnings = Vec::new();
    for entries in failures {
        for failure in entries.unwrap_or(&[]) {
            warnings.push(LoadWarning {
                path: failure.source.clone(),
                warning: describe_package_install_failure(failure),
            });
        }
    }
    warnings
}

/// 按 `path` 去重合并警告：**新值覆盖旧值**，顺序保留首次出现的位置。
/// TS: `resourceLoader.ts#mergeExtensionWarnings`（`new Map([...old, ...new]).values()`）。
pub fn merge_extension_warnings(result: &mut LoadExtensionsResult, warnings: Vec<LoadWarning>) {
    let mut merged: IndexMap<String, LoadWarning> = IndexMap::new();
    for warning in result.warnings.take().unwrap_or_default() {
        merged.insert(warning.path.clone(), warning);
    }
    for warning in warnings {
        merged.insert(warning.path.clone(), warning);
    }
    result.warnings = Some(merged.into_values().collect());
}

/// 是否是带 `factory` 的具名内置扩展（`builtin: true`）。TS:
/// `resource-loader.ts#isBuiltinExtension`（类型守卫 `Exclude<InlineExtension, ExtensionFactory>`）。
pub fn is_builtin_extension(input: &InlineExtension) -> bool {
    matches!(
        input,
        InlineExtension::Named {
            builtin: Some(true),
            ..
        }
    )
}

/// 扩展注册的工具/命令/flag 名（先 tools、再 commands、再 flags，各自保持 Map 顺序）。
fn extension_names(extension: &Extension) -> Vec<String> {
    let mut names = Vec::with_capacity(
        extension.tools.len() + extension.commands.len() + extension.flags.len(),
    );
    names.extend(extension.tools.keys().map(|name| format!("tool:{name}")));
    names.extend(
        extension
            .commands
            .keys()
            .map(|name| format!("command:{name}")),
    );
    names.extend(extension.flags.keys().map(|name| format!("flag:{name}")));
    names
}

/// 略过与其它扩展注册同名 tool/command/flag 的 `replaceable` 扩展。
/// TS: `resource-loader.ts#omitReplacedExtensions`。
///
/// - `taken` 只收集**非 replaceable** 扩展的名字；同名重复时后写覆盖前写（`new Map` 语义），
///   但保留首次插入的位置。
/// - 对每个 replaceable 扩展，找第一个被占用的名字；没有则保留。
/// - 被占用的若是内置扩展（`builtin:` 前缀），追加一条逐字警告（`path` 是**内置扩展**的路径）。
/// - 其余情况静默丢弃。
pub fn omit_replaced_extensions(
    extensions: Vec<Extension>,
    warnings: &mut Vec<LoadWarning>,
) -> Vec<Extension> {
    let mut taken: IndexMap<String, String> = IndexMap::new();
    for extension in &extensions {
        if extension.replaceable.unwrap_or(false) {
            continue;
        }
        for name in extension_names(extension) {
            taken.insert(name, extension.path.clone());
        }
    }

    let mut kept = Vec::with_capacity(extensions.len());
    for extension in extensions {
        if !extension.replaceable.unwrap_or(false) {
            kept.push(extension);
            continue;
        }
        let replacement = extension_names(&extension)
            .into_iter()
            .find_map(|name| taken.get(&name).map(|owner| (name, owner.clone())));
        let Some((name, owner_path)) = replacement else {
            kept.push(extension);
            continue;
        };
        if extension
            .path
            .starts_with(kiana_core::source_info::BUILTIN_PATH_PREFIX)
        {
            let builtin_name =
                &extension.path[kiana_core::source_info::BUILTIN_PATH_PREFIX.len()..];
            // TS 用 `replacement.name.split(":", 2)`：只保留前两段，第二个冒号之后被丢弃。
            let (kind, raw_name) = split_colon_two(&name);
            let raw_name = raw_name.unwrap_or("undefined");
            let registered_name = match kind {
                "command" => format!("/{raw_name}"),
                "flag" => format!("--{raw_name}"),
                _ => raw_name.to_string(),
            };
            warnings.push(LoadWarning {
                path: extension.path.clone(),
                warning: format!(
                    "Extension {} registers {} `{}`, so built-in extension `{}` was not loaded. To use `{}`, run `kiana config` and make sure it is enabled under Built-in extensions, then disable or remove the existing extension. We recommend only having one or the other loaded at a time.",
                    owner_path, kind, registered_name, builtin_name, builtin_name
                ),
            });
        }
        // 被别的扩展占用了名字：无论是否内置都丢弃。
    }
    kept
}

/// `value.split(":", 2)` 的 JS 语义：最多两段；第二段在**第二个**冒号处截断，
/// 没有冒号时第二段是 `undefined`（Rust 的 `split_once` 会把剩余部分全留在第二段）。
fn split_colon_two(value: &str) -> (&str, Option<&str>) {
    match value.split_once(':') {
        Some((head, rest)) => match rest.split_once(':') {
            Some((second, _)) => (head, Some(second)),
            None => (head, Some(rest)),
        },
        None => (value, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package::PackageInstallFailure;
    use kiana_core::source_info::{
        SourceOrigin, SourceScope, SyntheticSourceInfoOptions, create_synthetic_source_info,
    };
    use kiana_ext_api::types::{Extension, ExtensionFlag, RegisteredCommand};

    fn extension(path: &str, replaceable: bool, commands: &[&str], flags: &[&str]) -> Extension {
        let source_info = create_synthetic_source_info(
            path,
            SyntheticSourceInfoOptions {
                source: "test".to_string(),
                ..Default::default()
            },
        );
        let mut command_map = IndexMap::new();
        for name in commands {
            command_map.insert(
                (*name).to_string(),
                RegisteredCommand {
                    name: (*name).to_string(),
                    source_info: source_info.clone(),
                    description: Some(format!("{name} description")),
                    get_argument_completions: None,
                    handler: std::sync::Arc::new(|_, _| Box::pin(async {})),
                },
            );
        }
        let mut flag_map = IndexMap::new();
        for name in flags {
            flag_map.insert((*name).to_string(), ExtensionFlag::default());
        }
        Extension {
            path: path.to_string(),
            resolved_path: path.to_string(),
            hidden: None,
            replaceable: Some(replaceable),
            source_info,
            handlers: IndexMap::new(),
            tools: IndexMap::new(),
            message_renderers: IndexMap::new(),
            tool_renderers: None,
            markdown_transformer: None,
            entry_renderers: None,
            commands: command_map,
            flags: flag_map,
            shortcuts: IndexMap::new(),
        }
    }

    #[test]
    fn host_provided_packages_are_the_ten_verbatim_names() {
        assert_eq!(HOST_PROVIDED_EXTENSION_PACKAGES.len(), 10);
        assert!(HOST_PROVIDED_EXTENSION_PACKAGES.contains(&"@sinclair/typebox"));
        assert!(HOST_PROVIDED_EXTENSION_PACKAGES.contains(&"typebox"));
    }

    #[test]
    fn install_failure_hints_match_the_three_scope_forms() {
        let failure = |scope| PackageInstallFailure {
            source: "npm:broken-package".to_string(),
            scope,
            message: "boom".to_string(),
        };
        assert_eq!(
            describe_package_install_failure(&failure(SourceScope::Temporary)),
            "failed to install (boom). Check the source and try again."
        );
        assert_eq!(
            describe_package_install_failure(&failure(SourceScope::Project)),
            "failed to install (boom). Run \"kiana remove npm:broken-package --local\" to remove it from settings.json."
        );
        assert_eq!(
            describe_package_install_failure(&failure(SourceScope::User)),
            "failed to install (boom). Run \"kiana remove npm:broken-package\" to remove it from settings.json."
        );
        assert_eq!(
            format_package_install_warning(&failure(SourceScope::User)),
            "Extension package \"npm:broken-package\": failed to install (boom). Run \"kiana remove npm:broken-package\" to remove it from settings.json."
        );
    }

    #[test]
    fn collect_package_install_warnings_uses_the_source_and_no_package_prefix() {
        let failures = vec![PackageInstallFailure {
            source: "npm:broken-package".to_string(),
            scope: SourceScope::User,
            message: "boom".to_string(),
        }];
        let warnings = collect_package_install_warnings(&[Some(failures.as_slice()), None]);
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].path, "npm:broken-package");
        assert_eq!(
            warnings[0].warning,
            "failed to install (boom). Run \"kiana remove npm:broken-package\" to remove it from settings.json."
        );
    }

    #[test]
    fn merge_extension_warnings_new_value_wins_but_keeps_first_position() {
        let mut result = LoadExtensionsResult {
            extensions: Vec::new(),
            errors: Vec::new(),
            warnings: Some(vec![
                LoadWarning {
                    path: "a".to_string(),
                    warning: "old-a".to_string(),
                },
                LoadWarning {
                    path: "b".to_string(),
                    warning: "old-b".to_string(),
                },
            ]),
            runtime: kiana_extensions::create_extension_runtime(),
        };
        merge_extension_warnings(
            &mut result,
            vec![
                LoadWarning {
                    path: "b".to_string(),
                    warning: "new-b".to_string(),
                },
                LoadWarning {
                    path: "c".to_string(),
                    warning: "new-c".to_string(),
                },
            ],
        );
        let warnings = result.warnings.expect("warnings");
        assert_eq!(
            warnings
                .iter()
                .map(|warning| (warning.path.as_str(), warning.warning.as_str()))
                .collect::<Vec<_>>(),
            vec![("a", "old-a"), ("b", "new-b"), ("c", "new-c")]
        );
    }

    #[test]
    fn omit_replaced_extensions_drops_replaceable_and_warns_for_builtins() {
        let mut warnings = Vec::new();
        let kept = omit_replaced_extensions(
            vec![
                extension("/ext/other-mcp.ts", false, &["mcp"], &["verbose"]),
                extension("builtin:mcp", true, &["mcp"], &[]),
                extension("builtin:llama", true, &["llama"], &[]),
                extension("builtin:verbose", true, &[], &["verbose"]),
            ],
            &mut warnings,
        );
        assert_eq!(
            kept.iter()
                .map(|extension| extension.path.as_str())
                .collect::<Vec<_>>(),
            vec!["/ext/other-mcp.ts", "builtin:llama"]
        );
        assert_eq!(warnings.len(), 2);
        assert_eq!(warnings[0].path, "builtin:mcp");
        assert_eq!(
            warnings[0].warning,
            "Extension /ext/other-mcp.ts registers command `/mcp`, so built-in extension `mcp` was not loaded. To use `mcp`, run `kiana config` and make sure it is enabled under Built-in extensions, then disable or remove the existing extension. We recommend only having one or the other loaded at a time."
        );
        assert_eq!(warnings[1].path, "builtin:verbose");
        assert_eq!(
            warnings[1].warning,
            "Extension /ext/other-mcp.ts registers flag `--verbose`, so built-in extension `verbose` was not loaded. To use `verbose`, run `kiana config` and make sure it is enabled under Built-in extensions, then disable or remove the existing extension. We recommend only having one or the other loaded at a time."
        );
    }

    #[test]
    fn split_colon_two_truncates_after_the_second_colon_like_js() {
        assert_eq!(
            split_colon_two("command:deploy"),
            ("command", Some("deploy"))
        );
        assert_eq!(split_colon_two("command:foo:bar"), ("command", Some("foo")));
        assert_eq!(split_colon_two("command:"), ("command", Some("")));
        assert_eq!(split_colon_two("command"), ("command", None));
    }

    #[test]
    fn omit_replaced_extensions_truncates_colon_names_in_warnings() {
        let mut warnings = Vec::new();
        let kept = omit_replaced_extensions(
            vec![
                extension("/ext/other.ts", false, &["foo:bar:baz"], &[]),
                extension("builtin:demo", true, &["foo:bar:baz"], &[]),
            ],
            &mut warnings,
        );
        assert_eq!(kept.len(), 1);
        assert_eq!(
            warnings[0].warning,
            "Extension /ext/other.ts registers command `/foo`, so built-in extension `demo` was not loaded. To use `demo`, run `kiana config` and make sure it is enabled under Built-in extensions, then disable or remove the existing extension. We recommend only having one or the other loaded at a time."
        );
    }

    #[test]
    fn null_package_manifest_is_a_type_error_not_a_skip() {
        let dir = std::env::temp_dir().join(format!("kiana-null-manifest-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create dir");
        let package_json = dir.join("package.json");
        std::fs::write(&package_json, "null").expect("write manifest");
        let package_root = dir.to_string_lossy().into_owned();
        let extension_path = format!("{package_root}/extensions/e.ts");
        let mut metadata = IndexMap::new();
        metadata.insert(
            extension_path.clone(),
            PathMetadata {
                source: "package".to_string(),
                scope: SourceScope::Temporary,
                origin: SourceOrigin::Package,
                base_dir: None,
                package_root: Some(package_root.clone()),
            },
        );
        let error = collect_extension_package_warnings(&[extension_path], &metadata)
            .expect_err("must throw");
        assert_eq!(
            error.message,
            "Cannot read properties of null (reading 'dependencies')"
        );
        assert_eq!(error.path, package_json.to_string_lossy());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn is_builtin_extension_requires_the_named_builtin_form() {
        assert!(is_builtin_extension(&InlineExtension::Named {
            name: "mcp".to_string(),
            factory: std::sync::Arc::new(|_| Box::pin(async {})),
            hidden: None,
            replaceable: None,
            builtin: Some(true),
        }));
        assert!(!is_builtin_extension(&InlineExtension::Named {
            name: "mcp".to_string(),
            factory: std::sync::Arc::new(|_| Box::pin(async {})),
            hidden: None,
            replaceable: None,
            builtin: None,
        }));
        assert!(!is_builtin_extension(&InlineExtension::Factory(
            std::sync::Arc::new(|_| Box::pin(async {}))
        )));
    }

    #[test]
    fn collect_extension_package_warnings_uses_node_read_error_messages() {
        // TS 的 `readFileSync` 未被捕获：目录形式的 package.json 得到
        // `EISDIR: illegal operation on a directory, read`（node 26）。
        let temp = tempfile::tempdir().expect("tempdir");
        let package_root = temp.path().join("dir-package");
        std::fs::create_dir_all(package_root.join("package.json")).expect("package.json dir");
        let package_root = package_root.to_string_lossy().into_owned();
        let extension_path = format!("{package_root}/extensions/entry.ts");

        let mut metadata_by_path = IndexMap::new();
        metadata_by_path.insert(
            extension_path.clone(),
            PathMetadata {
                source: "package".to_string(),
                scope: SourceScope::Temporary,
                origin: SourceOrigin::Package,
                base_dir: None,
                package_root: Some(package_root),
            },
        );

        let error = collect_extension_package_warnings(&[extension_path], &metadata_by_path)
            .expect_err("readFileSync must throw for a directory");
        assert_eq!(
            error.message,
            "EISDIR: illegal operation on a directory, read"
        );
    }
}
