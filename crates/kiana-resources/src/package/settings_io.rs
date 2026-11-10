//! settings 里的 source 增删与规范化。
//!
//! TS: `package-manager.ts` 的 `addSourceToSettings` / `removeSourceFromSettings` /
//! `getSourceMatchKeyForInput` / `getSourceMatchKeyForSettings` /
//! `normalizePackageSourceForSettings` / `packageSourcesMatch` /
//! `findSuggestedConfiguredSource` / `buildNoMatchingPackageMessage`，
//! 以及读取 settings 数组时的 JS 宽松回退（`packages ?? []`、字符串按码点展开）。
//!
//! # 为什么需要这个模块
//! settings 文件是包源唯一的持久化位置：`install` / `remove` 只改磁盘，真正“记住”一个源
//! 的是这里的写回逻辑。本模块同时实现 TS 的 identity 匹配规则（npm 用包名、git 用
//! `host/path`、local 用解析后的绝对路径）和本地路径“相对 settings 基准目录”的规范化，
//! 使同一路径在不同工作目录下写出稳定、可再次匹配的形式。
//!
//! # 边界与调用方
//! - 读取：`super::resolve_inner`、`check_for_available_updates`、`update` 与
//!   `list_configured_packages` 通过 `settings_packages` / `settings_string_list` 取
//!   宽松视图；`super::resolve` 也用它读取 `extensions` 覆盖规则。
//! - 写入：`PackageManager` trait 的 `add_source_to_settings` /
//!   `remove_source_from_settings` 以及 `install_and_persist` / `remove_and_persist`
//!   最终落到本文件的 `try_*` 与 `persist_packages`。
//! - 本模块不安装、不删除任何包文件，只读写 settings。
//!
//! # 读写不是跨调用的原子操作
//! 每次调用都先取一份 settings 值拷贝（`get_global_settings()` / `get_project_settings()`），
//! 在内存里算出完整的新数组后整组写回；本模块不提供“读-改-写”跨调用的锁或版本检查。
//! 因此并发写入者之间可能互相覆盖，调用方不应并行修改同一作用域的 packages。
//!
//! # 兼容契约
//! 错误文案（`entries is not iterable`、`<值> is not iterable (...)`、
//! `Cannot read properties of undefined (reading 'startsWith')`）是外部按文本断言的稳定契约，
//! 不能改写（改动需同步测试）。local 源的“输入 identity 以 cwd 为基准、settings identity
//! 以作用域基准目录为基准”的不对称是既定行为，不要合并成同一种解析。

use kiana_config::settings::schema::{PackageSource, Settings};
use serde_json::Value;

use super::exec::js_trim;
use super::parse::{ParsedSource, get_package_source_string};
use super::paths::{join, relative};
use super::types::{InstallOptions, SourceScope};
use super::{DefaultPackageManager, PackageManagerError, err};

/// 非法 settings 值在 TS 里触发的 TypeError 文案（`parseSource(undefined)`）。
///
/// 本文件在两个位置复用它：`settings_string_list` 的数组含非字符串元素时，以及
/// `settings_packages` 的元素无法反序列化成 [`PackageSource`] 时（例如数字/布尔）。
/// 另外 `super::parse::get_package_source_string` 对缺少 `source` 的对象也返回同一条
/// 文案——那里对应 TS 的 `parseSource(undefined)`，是这条文案的原始出处。
pub(crate) fn invalid_settings_value_error() -> PackageManagerError {
    err("Cannot read properties of undefined (reading 'startsWith')")
}

/// `settings[key]` 的字符串数组视图（带 TS 的宽松回退）。
///
/// TS 的 `getGlobalSettings()` 直接返回 `JSON.parse` 的原始对象：
/// - 数组 → 逐个使用元素（非字符串元素会让后续 `startsWith` 抛 TypeError）；
/// - 字符串 → `[...value]`（按码点展开为字符）；
/// - 其它 → 空数组（`?? []` 只对 `null`/`undefined` 生效）。
///
/// 读取顺序是“先类型化字段、后原始 JSON”：`settings.extensions` 等字段已成功解析时直接
/// 返回其克隆，否则（键缺失、显式 `null`、类型不符被放进 `extra`）回退到 `settings.raw`。
/// 返回值是独立的 `Vec<String>`，本函数不修改 settings。
///
/// 失败模式：原始数组含非字符串元素时返回 [`invalid_settings_value_error`]；原始值是
/// 数字/布尔/对象等不可迭代的标量时返回 `entries is not iterable`。显式 `null` 与键缺失
/// 都是空列表（对应 TS 只兜 `null`/`undefined` 的 `?? []`）。
pub(crate) fn settings_string_list(
    settings: &Settings,
    key: &str,
) -> Result<Vec<String>, PackageManagerError> {
    let typed = match key {
        "extensions" => settings.extensions.as_ref(),
        "skills" => settings.skills.as_ref(),
        "prompts" => settings.prompts.as_ref(),
        "themes" => settings.themes.as_ref(),
        _ => None,
    };
    if let Some(values) = typed {
        return Ok(values.clone());
    }
    match settings.raw(key) {
        Some(Value::Array(items)) => {
            let mut values = Vec::with_capacity(items.len());
            for item in items {
                match item.as_str() {
                    Some(text) => values.push(text.to_string()),
                    None => return Err(invalid_settings_value_error()),
                }
            }
            Ok(values)
        }
        Some(Value::String(text)) => Ok(text
            .chars()
            .map(|character| character.to_string())
            .collect()),
        Some(Value::Null) | None => Ok(Vec::new()),
        // TS: `(globalSettings[resourceType] ?? [])` 之后调用 `resolveLocalEntries(entries)`
        // → `splitPatterns(entries)` → `for (const entry of entries)`。数字/布尔/对象没有
        // 迭代器，V8 抛 `TypeError: entries is not iterable`（文案用变量名）。
        Some(_) => Err(err("entries is not iterable")),
    }
}

/// V8 在 `for (const pkg of globalSettings.packages ?? [])` 上的 TypeError 文案。
///
/// 可迭代表达式是 `??` 时，V8 用「值描述 + is not iterable (cannot read property
/// Symbol(Symbol.iterator))」：数字 `number 42`、布尔 `boolean true`、对象 `object`。
///
/// 值描述复用 `kiana_json::js_string`，这样数字/布尔的文本与其它 JS 字符串化路径一致；
/// 对象不再展开内容，固定为 `object`。`null`/数组/字符串不会走到这里。
fn packages_not_iterable_error(value: &Value) -> PackageManagerError {
    match value {
        Value::Bool(_) | Value::Number(_) => {
            let kind = if matches!(value, Value::Bool(_)) {
                "boolean"
            } else {
                "number"
            };
            err(format!(
                "{kind} {} is not iterable (cannot read property Symbol(Symbol.iterator))",
                kiana_json::js_string::js_string(Some(value))
            ))
        }
        _ => err("object is not iterable (cannot read property Symbol(Symbol.iterator))"),
    }
}

/// `settings.packages ?? []` 的宽松视图。
///
/// 与 [`settings_string_list`] 相同的“类型化字段优先、原始 JSON 回退”策略：
/// - `settings.packages` 已成功解析时直接克隆；
/// - 原始数组逐元素反序列化为 [`PackageSource`]，任一元素失败就整体返回
///   [`invalid_settings_value_error`]（不跳过坏元素，也不返回部分结果）；
/// - 原始字符串按码点展开，每个字符变成一个 `PackageSource::Text`（对应 JS 的
///   `[...value]`，是 `?? []` 之外的历史宽松行为）；
/// - `null`/缺失为空列表；其它标量返回 [`packages_not_iterable_error`]。
///
/// 返回顺序与 settings 中一致；本函数不排序、不去重。
pub(crate) fn settings_packages(
    settings: &Settings,
) -> Result<Vec<PackageSource>, PackageManagerError> {
    if let Some(values) = &settings.packages {
        return Ok(values.clone());
    }
    match settings.raw("packages") {
        Some(Value::Array(items)) => {
            let mut packages = Vec::with_capacity(items.len());
            for item in items {
                match serde_json::from_value::<PackageSource>(item.clone()) {
                    Ok(package) => packages.push(package),
                    Err(_) => return Err(invalid_settings_value_error()),
                }
            }
            Ok(packages)
        }
        Some(Value::String(text)) => Ok(text
            .chars()
            .map(|character| PackageSource::Text(character.to_string()))
            .collect()),
        Some(Value::Null) | None => Ok(Vec::new()),
        // TS: `for (const pkg of projectSettings.packages ?? [])` 直接展开；数字/布尔/对象
        // 没有迭代器，抛 `TypeError: <值描述> is not iterable (...)`。
        Some(other) => Err(packages_not_iterable_error(&other)),
    }
}

impl DefaultPackageManager {
    /// `addSourceToSettings(source, options?)` 的 `Result` 版本。
    ///
    /// 执行顺序：
    /// 1. `options.local` 选作用域（project / user）并读取对应 settings 快照；
    /// 2. `settings_packages` 取当前列表（含 TS 的宽松回退）；
    /// 3. `normalize_package_source_for_settings` 规范化待写入源；
    /// 4. 用 `package_sources_match` 找**第一个** identity 相同的条目。
    ///
    /// 第 4 步的三种结局：
    /// - 命中且现有源字符串等于规范化结果 → `Ok(false)`，不写 settings；
    /// - 命中但字符串不同 → 只替换该条目的源（对象形式保留 `skills` / `extensions` 等
    ///   过滤字段），整组写回，返回 `Ok(true)`；
    /// - 未命中 → 追加一个 `PackageSource::Text` 条目，整组写回，返回 `Ok(true)`。
    ///
    /// 注意规范化只改写 local 源；npm/git 源原样返回，因此带空白的输入会被原样存储，
    /// 但匹配时仍按解析后的 identity 比较。project 作用域的 trust 检查最多有三处，错误
    /// 文本分两类：第 3 步取基准目录与第 4 步对 local 条目计算 settings identity（经
    /// `get_source_match_key_for_settings` → `get_base_dir_for_scope`）都报
    /// `Project is not trusted; refusing to access project package storage`；写回
    /// `set_project_packages` 时报 `Project is not trusted; refusing to write project settings`。
    /// 现有条目全是 npm/git 时第 4 步不触达 project 基准目录，此时只有两处。
    ///
    /// # Errors
    /// settings 形状非法、project 未受信、settings 保存失败时返回 [`PackageManagerError`]，
    /// 消息即 TS 的 `error.message`。
    pub fn try_add_source_to_settings(
        &self,
        source: &str,
        options: InstallOptions,
    ) -> Result<bool, PackageManagerError> {
        let scope = if options.local {
            SourceScope::Project
        } else {
            SourceScope::User
        };
        let current_settings = if scope == SourceScope::Project {
            self.settings_manager.get_project_settings()
        } else {
            self.settings_manager.get_global_settings()
        };
        let current_packages = settings_packages(&current_settings)?;
        let normalized_source = self.normalize_package_source_for_settings(source, scope)?;
        let mut match_index = None;
        for (index, existing) in current_packages.iter().enumerate() {
            if self.package_sources_match(existing, source, scope)? {
                match_index = Some(index);
                break;
            }
        }
        if let Some(match_index) = match_index {
            let existing = &current_packages[match_index];
            if get_package_source_string(existing)? == normalized_source {
                return Ok(false);
            }
            let mut next_packages = current_packages.clone();
            next_packages[match_index] = match existing {
                PackageSource::Text(_) => PackageSource::Text(normalized_source),
                PackageSource::Object(object) => {
                    let mut object = object.clone();
                    object.source = Some(normalized_source);
                    PackageSource::Object(object)
                }
            };
            return self.persist_packages(scope, next_packages);
        }
        let mut next_packages = current_packages;
        next_packages.push(PackageSource::Text(normalized_source));
        self.persist_packages(scope, next_packages)
    }

    /// `addSourceToSettings(source, options?)`（frozen trait 签名：错误映射为 `false`）。
    ///
    /// 这是 [`PackageManager`] trait 的冻结签名；所有错误（包括 project 未受信与 settings
    /// 形状非法）都被折叠成 `false`，与“源已存在、无需写入”的 `false` 无法区分。需要
    /// 区分时调用 [`Self::try_add_source_to_settings`]。
    ///
    /// [`PackageManager`]: super::PackageManager
    pub(crate) fn add_source_to_settings(&self, source: &str, options: InstallOptions) -> bool {
        self.try_add_source_to_settings(source, options)
            .unwrap_or(false)
    }

    /// `removeSourceFromSettings(source, options?)` 的 `Result` 版本。
    ///
    /// 逐条计算 identity，保留所有**不匹配**的条目；因此删除会作用于当前 settings 里
    /// 全部 identity 相同的条目，而不是只删第一个。没有删除任何条目时返回 `Ok(false)`
    /// 且完全不写 settings；否则整组写回并返回 `Ok(true)`。
    ///
    /// 匹配同样使用 `package_sources_match`：输入 local 路径以 manager 的 `cwd`
    /// 为基准解析，settings 中的 local 路径以作用域基准目录解析，所以能匹配同一目录的
    /// 不同写法（例如末尾带 `/`，测试 `should_remove_local_package_entries_using_equivalent_path_forms`）。
    /// project 作用域下，只要列表里有 local 条目，计算 identity 前会做 trust 检查；
    /// 列表为空则不会触达 project 存储，直接返回 `false`。
    ///
    /// # Errors
    /// settings 形状非法、project 未受信（见上）、settings 保存失败时返回错误。
    pub fn try_remove_source_from_settings(
        &self,
        source: &str,
        options: InstallOptions,
    ) -> Result<bool, PackageManagerError> {
        let scope = if options.local {
            SourceScope::Project
        } else {
            SourceScope::User
        };
        let current_settings = if scope == SourceScope::Project {
            self.settings_manager.get_project_settings()
        } else {
            self.settings_manager.get_global_settings()
        };
        let current_packages = settings_packages(&current_settings)?;
        let mut next_packages: Vec<PackageSource> = Vec::with_capacity(current_packages.len());
        for existing in &current_packages {
            if !self.package_sources_match(existing, source, scope)? {
                next_packages.push(existing.clone());
            }
        }
        if next_packages.len() == current_packages.len() {
            return Ok(false);
        }
        self.persist_packages(scope, next_packages)
    }

    /// `removeSourceFromSettings(source, options?)`（frozen trait 签名）。
    ///
    /// 与添加版本对称：错误被折叠成 `false`，与“没有匹配条目”不可区分；需要错误时用
    /// [`Self::try_remove_source_from_settings`]。该 `false` 会被 CLI 当成
    /// `No matching package found` 处理（见 `PackageManager` trait 文档）。
    pub(crate) fn remove_source_from_settings(
        &self,
        source: &str,
        options: InstallOptions,
    ) -> bool {
        self.try_remove_source_from_settings(source, options)
            .unwrap_or(false)
    }

    /// 持久化 packages 数组；返回是否写入成功（等价 TS 的 `persist()`）。
    ///
    /// project 作用域走 `set_project_packages`，它会在未受信时返回
    /// `Project is not trusted; refusing to write project settings`，这里只取
    /// `error.message()` 转成 [`PackageManagerError`]；user 作用域的 `set_packages` 签名
    /// 不返回错误，因此恒为 `Ok(true)`。调用时传入的是**完整**新数组，不是增量。
    fn persist_packages(
        &self,
        scope: SourceScope,
        packages: Vec<PackageSource>,
    ) -> Result<bool, PackageManagerError> {
        if scope == SourceScope::Project {
            self.settings_manager
                .set_project_packages(packages)
                .map_err(|error| err(error.message()))?;
        } else {
            self.settings_manager.set_packages(packages);
        }
        Ok(true)
    }

    /// `getSourceMatchKeyForInput(source)`。
    ///
    /// 用户输入源的 identity：npm 只用包名（忽略版本/标签），git 只用 `host/path`
    /// （忽略 ref），local 用 [`Self::resolve_path`] 的结果（以 manager 的 `cwd` 为基准，
    /// 会 trim 并展开 `~`）。identity 不含作用域。
    pub(crate) fn get_source_match_key_for_input(
        &self,
        source: &str,
    ) -> Result<String, PackageManagerError> {
        let parsed = self.parse_source(source);
        Ok(match parsed {
            ParsedSource::Npm(npm) => format!("npm:{}", npm.name),
            ParsedSource::Git(git) => format!("git:{}/{}", git.host, git.path),
            ParsedSource::Local(local) => format!("local:{}", self.resolve_path(&local.path)),
        })
    }

    /// `getSourceMatchKeyForSettings(source, scope)`。
    ///
    /// 与 [`Self::get_source_match_key_for_input`] 的唯一差别在 local：这里以作用域基准
    /// 目录解析（user 为 `agent_dir`，project 为 `<cwd>/.kiana`）。settings 里的 local 源
    /// 正是由 [`Self::normalize_package_source_for_settings`] 写成相对该基准目录的形式，
    /// 因此重新按基准解析即可还原成与输入相同的绝对路径。npm/git 的 key 与作用域无关。
    ///
    /// project 作用域调用 [`Self::get_base_dir_for_scope`] 时会先做 trust 检查，未受信
    /// 返回错误（这发生在本函数内部，调用方无法绕开）。
    pub(crate) fn get_source_match_key_for_settings(
        &self,
        source: &str,
        scope: SourceScope,
    ) -> Result<String, PackageManagerError> {
        let parsed = self.parse_source(source);
        Ok(match parsed {
            ParsedSource::Npm(npm) => format!("npm:{}", npm.name),
            ParsedSource::Git(git) => format!("git:{}/{}", git.host, git.path),
            ParsedSource::Local(local) => {
                let base_dir = self.get_base_dir_for_scope(scope)?;
                format!(
                    "local:{}",
                    self.resolve_path_from_base(&local.path, &base_dir)
                )
            }
        })
    }

    /// `packageSourcesMatch(existing, inputSource, scope)`。
    ///
    /// 左侧取 settings 条目的 identity（按作用域基准目录解析 local），右侧取输入源的
    /// identity（按 manager 的 `cwd` 解析 local），相等即视为同一个包。因此版本/ref
    /// 不同但包名或 host/path 相同的条目会互相匹配——添加时的“更新 ref”行为依赖这一点。
    /// 现有条目是缺少 `source` 的对象时返回错误，而不是当作不匹配。
    pub(crate) fn package_sources_match(
        &self,
        existing: &PackageSource,
        input_source: &str,
        scope: SourceScope,
    ) -> Result<bool, PackageManagerError> {
        let left =
            self.get_source_match_key_for_settings(get_package_source_string(existing)?, scope)?;
        let right = self.get_source_match_key_for_input(input_source)?;
        Ok(left == right)
    }

    /// `normalizePackageSourceForSettings(source, scope)`：本地路径相对 settings 基准目录。
    ///
    /// 只有 local 源会被改写：先用 [`Self::resolve_path`]（以 `cwd` 为基准）解析成绝对
    /// 路径，再用 `path.relative` 表示成相对作用域基准目录的形式；结果为空串（源就是
    /// 基准目录本身）时写成 `"."`。npm/git 源原样返回——不 trim、不改写版本/ref。
    ///
    /// project 作用域在取基准目录时先做 trust 检查，未受信返回
    /// `Project is not trusted; refusing to access project package storage`。
    pub(crate) fn normalize_package_source_for_settings(
        &self,
        source: &str,
        scope: SourceScope,
    ) -> Result<String, PackageManagerError> {
        let parsed = self.parse_source(source);
        let ParsedSource::Local(local) = parsed else {
            return Ok(source.to_string());
        };
        let base_dir = self.get_base_dir_for_scope(scope)?;
        let resolved = self.resolve_path(&local.path);
        let rel = relative(&base_dir, &resolved);
        Ok(if rel.is_empty() { ".".to_string() } else { rel })
    }

    /// `findSuggestedConfiguredSource(source, configuredPackages)`。
    ///
    /// 只用于生成“你是不是想输入已配置的某个源”的提示，不修改任何状态。比较前用 JS
    /// `trim()`（比 Rust 的 `trim` 多认识 U+FEFF 等字符）处理输入，然后按配置顺序扫描：
    /// - npm 源：输入等于包名或原始 spec（`npm:foo@1.2.3` 的 `foo@1.2.3`）即命中；
    /// - git 源：输入等于 `host/path` 或 `host/path@ref` 即命中；
    /// - local 源不参与提示。
    ///
    /// 取不出源字符串的条目（对象缺 `source`）被静默跳过，不会让提示失败。返回第一个
    /// 命中条目的原始源字符串（保留 `npm:` / `git:` 前缀），没有命中返回 `None`。
    pub(crate) fn find_suggested_configured_source(
        &self,
        source: &str,
        configured_packages: &[PackageSource],
    ) -> Option<String> {
        let trimmed_source = js_trim(source);
        for pkg in configured_packages {
            let Ok(source_str) = get_package_source_string(pkg) else {
                continue;
            };
            let parsed = self.parse_source(source_str);
            match parsed {
                ParsedSource::Npm(npm) => {
                    if trimmed_source == npm.name || trimmed_source == npm.spec {
                        return Some(source_str.to_string());
                    }
                }
                ParsedSource::Git(git) => {
                    let shorthand = format!("{}/{}", git.host, git.path);
                    let shorthand_with_ref =
                        git.r#ref.as_ref().map(|r#ref| format!("{shorthand}@{ref}"));
                    if trimmed_source == shorthand
                        || shorthand_with_ref.as_deref() == Some(trimmed_source)
                    {
                        return Some(source_str.to_string());
                    }
                }
                ParsedSource::Local(_) => {}
            }
        }
        None
    }

    /// `buildNoMatchingPackageMessage(source, configuredPackages)`。
    ///
    /// 有提示时文案为 `No matching package found for <source>. Did you mean <suggestion>?`，
    /// 否则为 `No matching package found for <source>`（标点与大小写是稳定契约，改动需同步测试）。
    /// `update(source)` 在没有任何 identity 匹配的条目时用它构造错误返回给调用方。
    pub(crate) fn build_no_matching_package_message(
        &self,
        source: &str,
        configured_packages: &[PackageSource],
    ) -> String {
        match self.find_suggested_configured_source(source, configured_packages) {
            Some(suggestion) => {
                format!("No matching package found for {source}. Did you mean {suggestion}?")
            }
            None => format!("No matching package found for {source}"),
        }
    }

    /// `join` 的转发（settings 规范化之外的小工具，测试用）。
    #[allow(dead_code)]
    pub(crate) fn join_for_tests(&self, parts: &[&str]) -> String {
        join(parts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manager(settings: Settings) -> DefaultPackageManager {
        let dir = tempfile::tempdir().expect("tempdir");
        DefaultPackageManager::new(super::super::PackageManagerOptions {
            cwd: dir.path().to_string_lossy().into_owned(),
            agent_dir: dir.path().join("agent").to_string_lossy().into_owned(),
            settings_manager: kiana_config::settings::SettingsManager::in_memory(settings),
            builtin_extensions: vec![],
        })
    }

    #[test]
    fn settings_string_list_falls_back_to_raw_values() {
        let mut settings = Settings::default();
        settings
            .extra
            .insert("extensions".to_string(), serde_json::json!("ab"));
        assert_eq!(
            settings_string_list(&settings, "extensions").expect("list"),
            vec!["a", "b"]
        );

        let settings = Settings {
            extensions: Some(vec!["x".to_string()]),
            ..Settings::default()
        };
        assert_eq!(
            settings_string_list(&settings, "extensions").expect("list"),
            vec!["x"]
        );
        assert!(
            settings_string_list(&settings, "skills")
                .expect("list")
                .is_empty()
        );

        let mut malformed = Settings::default();
        malformed
            .extra
            .insert("extensions".to_string(), serde_json::json!(["ok", 42]));
        assert!(settings_string_list(&malformed, "extensions").is_err());
    }

    #[test]
    fn settings_packages_falls_back_to_raw_values() {
        let mut settings = Settings::default();
        settings.extra.insert(
            "packages".to_string(),
            serde_json::json!(["npm:a", {"source": "npm:b", "skills": []}]),
        );
        let packages = settings_packages(&settings).expect("packages");
        assert_eq!(packages.len(), 2);
        assert_eq!(
            get_package_source_string(&packages[1]).expect("source"),
            "npm:b"
        );

        let mut malformed = Settings::default();
        malformed
            .extra
            .insert("packages".to_string(), serde_json::json!(["npm:a", 42]));
        assert!(settings_packages(&malformed).is_err());

        let mut missing_source = Settings::default();
        missing_source
            .extra
            .insert("packages".to_string(), serde_json::json!([{"skills": []}]));
        let packages = settings_packages(&missing_source).expect("packages");
        assert!(get_package_source_string(&packages[0]).is_err());
    }

    /// `for (const pkg of settings.packages ?? [])` / `for (const entry of entries)`：
    /// 数字/布尔/对象不可迭代，V8 抛 TypeError（文案见 `packages_not_iterable_error`）。
    #[test]
    fn non_iterable_settings_values_report_v8_type_errors() {
        let mut packages = Settings::default();
        packages
            .extra
            .insert("packages".to_string(), serde_json::json!(42));
        assert_eq!(
            settings_packages(&packages).expect_err("number").message(),
            "number 42 is not iterable (cannot read property Symbol(Symbol.iterator))"
        );

        let mut object_packages = Settings::default();
        object_packages
            .extra
            .insert("packages".to_string(), serde_json::json!({"a": 1}));
        assert_eq!(
            settings_packages(&object_packages)
                .expect_err("object")
                .message(),
            "object is not iterable (cannot read property Symbol(Symbol.iterator))"
        );

        let mut boolean_packages = Settings::default();
        boolean_packages
            .extra
            .insert("packages".to_string(), serde_json::json!(true));
        assert_eq!(
            settings_packages(&boolean_packages)
                .expect_err("boolean")
                .message(),
            "boolean true is not iterable (cannot read property Symbol(Symbol.iterator))"
        );

        let mut extensions = Settings::default();
        extensions
            .extra
            .insert("extensions".to_string(), serde_json::json!(42));
        assert_eq!(
            settings_string_list(&extensions, "extensions")
                .expect_err("number")
                .message(),
            "entries is not iterable"
        );

        // `?? []`：null / 缺失仍然是空列表。
        let mut null_packages = Settings::default();
        null_packages
            .extra
            .insert("packages".to_string(), serde_json::Value::Null);
        assert!(
            settings_packages(&null_packages)
                .expect("null packages")
                .is_empty()
        );
    }

    #[test]
    fn suggestions_cover_npm_and_git_shorthands() {
        let manager = manager(Settings::default());
        let configured = vec![
            PackageSource::Text("npm:example".into()),
            PackageSource::Text("git:github.com/example/repo@v1".into()),
        ];
        assert_eq!(
            manager.find_suggested_configured_source("example", &configured),
            Some("npm:example".to_string())
        );
        assert_eq!(
            manager.find_suggested_configured_source("github.com/example/repo", &configured),
            Some("git:github.com/example/repo@v1".to_string())
        );
        assert_eq!(
            manager.build_no_matching_package_message("example", &configured),
            "No matching package found for example. Did you mean npm:example?"
        );
        assert_eq!(
            manager.build_no_matching_package_message("nope", &configured),
            "No matching package found for nope"
        );
    }
}
