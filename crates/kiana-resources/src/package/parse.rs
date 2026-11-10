//! `parseSource` / `parseNpmSpec` / npm semver / identity / dedupe。
//!
//! TS: `package-manager.ts` 的 `parseSource` / `parseNpmSpec` / `getPackageIdentity` /
//! `dedupePackages` / `isExactNpmVersion` / `getNpmVersionRange`。
//!
//! git URL 解析委托给 `kiana_core::git::parse_git_url`（`utils/git.ts` 的唯一实现），本模块
//! 不重复实现。
//!
//! # 为什么需要这个模块
//!
//! 包源是用户在 settings 里手写的字符串，同一个包可以写成多种形式
//! （`https://github.com/u/r`、`git:git@github.com:u/r`、带 `.git` 或 `@ref` 的变体）。
//! 安装、更新、settings 增删和 resolve 主流程必须对“这是不是同一个包”得出相同答案，
//! 否则会重复安装、重复注册扩展，或在删除时找不到已有条目。因此源字符串的类型判定、
//! npm spec 拆分与 identity 归一化集中在本模块，其他模块只消费结果。
//!
//! # 主要流程
//!
//! 1. `parse_source` 按固定顺序判定类型：`npm:` 前缀 → local → git → local 兜底。
//! 2. npm 源在构造时一并算出 `name` / `version` / `range` / `pinned`，调用点不再重复解析。
//! 3. `NpmRange` 把 node-semver 的区间语法翻译成 `semver` crate 的 `VersionReq`，
//!    `satisfies` 直接委托给 `VersionReq::matches`。
//! 4. `get_package_identity` 把三种源归一化为 `npm:<name>` / `git:<host>/<path>` /
//!    `local:<绝对路径>`，`dedupe_packages` 按这个 key 合并条目。
//!
//! # 被谁调用
//!
//! - `package/mod.rs`：`install` / `remove` / `update` / `try_get_installed_path` /
//!   `check_package_update` / `update_configured_sources` 用 `parse_source` 判定源类型；
//!   `installed_npm_matches_configured_version` 用 `NpmRange::satisfies` 与 `valid_version`；
//!   `resolve_inner` 与 `check_for_available_updates` 收集 project/user 条目后调用
//!   `dedupe_packages`。
//! - `package/resolve.rs`：解析包源、查找 `autoload:false` 的增量基底时调用 `parse_source` /
//!   `get_package_identity` / `get_package_source_string`。
//! - `package/npm.rs`：`get_latest_npm_version` 在配置了合法区间时用 `max_satisfying`，
//!   否则用 `highest_version`。
//!
//! 本模块本身是 `pub`，但其中函数与类型都是 `pub(crate)`，外部 crate 无法使用这些实现
//! 细节。
//!
//! # 约束与失败模式
//!
//! - 判定顺序是 npm → local → git → local 兜底。`is_local_path` 把没有已知 scheme 前缀
//!   （`npm:`/`git:`/`github:`/`http(s):`/`ssh:`/`builtin:`）的串直接定为 local，因此
//!   `./x`、`../x`、绝对路径、裸 `github.com/u/r` 和裸 `git@host:path` 都不会进入 git 分支。
//!   `parse_git_url` 在没有 `git:` 前缀时只接受带 `http(s)://`/`ssh://`/`git://` 的 URL
//!   （见 `kiana_core::git` 的模块文档），所以两个判定不会对同一串给出相反结果；
//!   最后的兜底让 `parse_source` 成为不抛错的纯函数。
//! - local 源只保存原始字符串，不 trim、不转绝对路径；同一字符串在不同 scope 下
//!   identity 不同，因为 base dir 不同（user 用 `agent_dir`，project 用 `<cwd>/.kiana`，
//!   temporary 用 `cwd`，见 `kiana-resources/src/package/paths.rs`）。
//! - 纯解析函数（`parse_source`、`parse_npm_spec`、`NpmRange::parse`、`max_satisfying`）
//!   不做 IO。会返回错误的入口只有 `get_package_source_string`（对象形式缺少 `source`）、
//!   `highest_version`（非法版本号）、`get_package_identity` 与 `dedupe_packages`
//!   （local + project 作用域在项目未受信时由 `get_base_dir_for_scope` 报错；
//!   dedupe 还会透传缺 `source` 的错误）。
//! - npm semver 行为以 `tests/fixtures/package/semver_golden.json` 为准：测试逐条断言
//!   validRange 是否合法、satisfies 与 valid 的结果。改动翻译逻辑必须保证该 fixture 仍通过。
//! - `dedupe_packages` 依赖输入顺序：调用方按 project 在前、user 在后传入
//!   （`resolve_inner` / `check_for_available_updates`）。

use std::sync::LazyLock;

use kiana_config::settings::schema::PackageSource;
use regex::Regex;

use super::exec::js_trim;
use super::types::SourceScope;
use super::{DefaultPackageManager, PackageManagerError, err};

/// `NpmSource`。TS: `package-manager.ts#NpmSource`。
///
/// `parse_source` 在构造时就拆好 spec 并预先算出 `range` / `pinned`，因为安装路径、
/// 更新检查和 settings 匹配都要用这些派生值，集中计算能保证它们与同一个 spec 一致。
/// `version` 为 `None`（没有 `@...` 部分）时 `range` 也是 `None`；`latest` 这类 tag 不是
/// 合法区间，同样得到 `range: None`、`pinned: false`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NpmSource {
    /// `npm:` 之后 trim 过的 spec（不包含前缀）。
    pub spec: String,
    /// 包名。
    pub name: String,
    /// 版本/标签部分（`@` 之后）。
    pub version: Option<String>,
    /// `semver.validRange(version)` 的等价物。
    ///
    /// `None` 表示没有版本部分或版本不是合法区间。注意 `installed_npm_matches_configured_version`
    /// 把 `None` 当作“没有版本约束”，已安装版本直接视为匹配。
    pub range: Option<NpmRange>,
    /// `semver.valid(version) !== null`。
    ///
    /// 只有完整三段版本（可带 prerelease/build、允许小写 `v` 前缀）才为 `true`；
    /// 区间、tag、两位版本都是 `false`。更新检查据此跳过被钉死的 npm 源。
    pub pinned: bool,
}

/// `LocalSource`。TS: `package-manager.ts#LocalSource`。
///
/// 只保存用户写的原始字符串：这里不 trim、不展开 `~`、不转绝对路径。这些动作由
/// `resolve_path` / `resolve_path_from_base` 在需要具体路径时完成，并以调用方给出的
/// scope 基准目录为准。因此 `parse_source` 保持纯函数，同一个字符串在不同 scope 下
/// identity 也可能不同。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LocalSource {
    /// 原始路径字符串。
    pub path: String,
}

/// `ParsedSource`。TS: `package-manager.ts#ParsedSource`。
///
/// 三个变体的判定顺序见 `parse_source`：先看 `npm:` 前缀；`is_local_path` 为真直接
/// local；否则尝试 git，失败再回退 local。local 不是“解析失败”的变体，而是所有无法
/// 识别为 npm/git 的字符串的正常归宿。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ParsedSource {
    /// `npm:` 源。
    Npm(NpmSource),
    /// git 源。
    Git(kiana_core::git::GitSource),
    /// 本地路径。
    Local(LocalSource),
}

/// 带作用域的配置条目。TS: `Array<{ pkg: PackageSource; scope: SourceScope }>`。
///
/// `scope` 只影响 local 源的 identity（npm/git 与作用域无关）。调用方组装列表时约定
/// project 条目在前、user 条目在后，`dedupe_packages` 的顺序语义依赖这一点。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ConfiguredEntry {
    /// settings 里的包源（字符串或对象）。
    pub pkg: PackageSource,
    /// 作用域。
    pub scope: SourceScope,
}

/// `getPackageSourceString(pkg)`：字符串形式或对象里的 `source`。
///
/// TS 对缺失 `source` 的对象会在 `parseSource(undefined)` 里抛 TypeError；这里返回同等
/// 语义的错误，由 resolve/update 等 `Result` 路径透传。
pub(crate) fn get_package_source_string(pkg: &PackageSource) -> Result<&str, PackageManagerError> {
    match pkg {
        PackageSource::Text(source) => Ok(source),
        PackageSource::Object(object) => object.source.as_deref().ok_or_else(|| {
            super::err("Cannot read properties of undefined (reading 'startsWith')")
        }),
    }
}

/// `parseNpmSpec(spec)`：`@scope/pkg@1.2.3` → `{ name: "@scope/pkg", version: "1.2.3" }`。
///
/// 名字段 `@?[^@]+(?:/[^@]+)?` 允许一个前导 `@`（scope）和一个 `/子名`；版本段从名字
/// 之后的 `@` 开始。版本段用显式字符类复刻 JS `.` 不匹配 `\r`/`\n`/U+2028/U+2029 的行为。
/// 匹配失败（例如含多个 `@` 的畸形串）时返回整个 spec 作为包名、版本为 `None`，不报错。
static NPM_SPEC_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(@?[^@]+(?:/[^@]+)?)(?:@([^\r\n\u{2028}\u{2029}]+))?$")
        .expect("valid npm spec regex")
});

/// 把 npm spec 拆成包名与版本（`parseNpmSpec`）。
///
/// 返回 `(name, version)`；没有 `@version` 时版本为 `None`。调用方（`parse_source`）
/// 已先去掉 `npm:` 前缀并对剩余文本做 JS `trim()`。这是纯字符串操作，不会失败。
pub(crate) fn parse_npm_spec(spec: &str) -> (String, Option<String>) {
    match NPM_SPEC_RE.captures(spec) {
        Some(captures) => {
            let name = captures
                .get(1)
                .map(|m| m.as_str().to_string())
                .unwrap_or_else(|| spec.to_string());
            let version = captures.get(2).map(|m| m.as_str().to_string());
            (name, version)
        }
        None => (spec.to_string(), None),
    }
}

/// `parseSource(source)`。
///
/// 判定顺序固定，不能调换：
/// 1. `npm:` 前缀：去掉前缀后按 JS `trim()` 规范化，再拆 spec、预算 `range` 与 `pinned`。
/// 2. `kiana_core::paths::is_local_path` 为真：直接当 local，不尝试 git。
/// 3. `kiana_core::git::parse_git_url`：处理带 `git:` 前缀或受支持 scheme 的 URL。
/// 4. 兜底仍是 local，保留原始字符串（未 trim、未绝对化）。
///
/// 任何输入都有返回值，不会失败；local 判定与兜底的区别只影响是否调用 git 解析器。
pub(crate) fn parse_source(source: &str) -> ParsedSource {
    if let Some(rest) = source.strip_prefix("npm:") {
        // JS 的 String#trim 比 Rust 的 trim 多认识 U+FEFF 等字符，必须走 js_trim。
        let spec = js_trim(rest).to_string();
        let (name, version) = parse_npm_spec(&spec);
        let range = get_npm_version_range(version.as_deref());
        let pinned = is_exact_npm_version(version.as_deref());
        return ParsedSource::Npm(NpmSource {
            spec,
            name,
            version,
            range,
            pinned,
        });
    }

    // 没有已知 scheme 前缀的串直接当 local：覆盖相对/绝对路径、裸 `github.com/u/r`，
    // 以及没有 `git:` 前缀的 `git@host:path`。测试
    // `parse_source_types_from_docs_examples` / `never_parses_dot_relative_paths_as_git`
    // 锁定这些边界；提前返回也避免把纯路径送进 git 解析器。
    if kiana_core::paths::is_local_path(source) {
        return ParsedSource::Local(LocalSource {
            path: source.to_string(),
        });
    }

    if let Some(git) = kiana_core::git::parse_git_url(source) {
        return ParsedSource::Git(git);
    }

    // 带 scheme 但 git 解析器不认识的串（或没有 scheme 的串）不是错误：原串按路径返回，
    // 后续由 resolve 阶段按文件系统处理。`kiana_core::git` 的模块文档也把“返回 None 时
    // 当作本地路径”列为调用约定。
    ParsedSource::Local(LocalSource {
        path: source.to_string(),
    })
}

// ---------------------------------------------------------------------------
// npm semver（node-semver 的 valid / validRange / satisfies / maxSatisfying 子集）
// ---------------------------------------------------------------------------

/// `semver.valid(v) !== null`。
///
/// 只有完整三段版本才为 `true`；区间（`^1.2.3`）、tag（`latest`）、两位版本都是 `false`。
pub(crate) fn is_exact_npm_version(version: Option<&str>) -> bool {
    version.and_then(valid_version).is_some()
}

/// `getNpmVersionRange(v)`：`v ? (validRange(v) ?? undefined) : undefined`。
///
/// 返回 `None` 表示没有版本部分或版本不是合法区间；调用方的更新检查此时会走
/// “取最高版本”的分支（`get_latest_npm_version` 的 `range: None`）。
pub(crate) fn get_npm_version_range(version: Option<&str>) -> Option<NpmRange> {
    version.and_then(NpmRange::parse)
}

/// 解析一个 semver 版本（node 的 `valid`：trim、允许前导 `v`）。
///
/// 只接受完整三段版本；区间、tag、前导零（`01.2.3`）都返回 `None`。`semver` crate 的
/// 解析错误在这里被丢弃，调用方只需要 `Option`/布尔结果。
pub(crate) fn valid_version(input: &str) -> Option<semver::Version> {
    let trimmed = js_trim(input);
    let trimmed = trimmed.strip_prefix('v').unwrap_or(trimmed);
    semver::Version::parse(trimmed).ok()
}

/// node-semver 的版本区间（`||` 连接的多个 AND 组）。
///
/// node 的区间语法包含 `||` OR 分支、空格分隔的 AND 组、连字符区间以及
/// x-range/`~`/`^`/`~>` 简写；`semver` crate 的 `VersionReq` 只理解自己的运算符和
/// `,` 连接的 AND。因此构造时先把每个 OR 分支翻译成一条 `VersionReq` 文本，交给
/// `VersionReq::parse` 校验并保存；`satisfies` 就是对 `requirements` 求 `any`。
///
/// 不变量：`requirements` 非空。空输入和空的 OR 分支都翻译为 `VersionReq::STAR`（`*`），
/// 即匹配任意版本。prerelease 的匹配规则由 `semver` crate 提供，具体边界由
/// `tests/fixtures/package/semver_golden.json` 锁定。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NpmRange {
    /// 每个元素对应输入里一个 `||` 分支（已翻译成 `VersionReq` 语法）。
    ///
    /// 不变量：至少一个元素。空输入或空的 OR 分支都会放入 `VersionReq::STAR`，因此
    /// `satisfies` 对列表求 `any` 时不会退化成“什么都不匹配”。
    requirements: Vec<semver::VersionReq>,
}

impl NpmRange {
    /// 解析 `validRange` 接受的区间；非法返回 `None`（对应 `validRange === null`）。
    ///
    /// 先把整串按 `||` 切开（node 的 OR 没有优先级），每个分支独立翻译；任一分支非法
    /// 就让整条 range 返回 `None`。空串返回只含 `*` 的区间，与 node 的
    /// `validRange("") === "*"` 一致。
    pub(crate) fn parse(input: &str) -> Option<Self> {
        let text = js_trim(input);
        if text.is_empty() {
            return Some(Self {
                requirements: vec![semver::VersionReq::STAR],
            });
        }
        let mut requirements = Vec::new();
        for alternative in text.split("||") {
            let alternative = js_trim(alternative);
            // node 的 `parseRange` 先在每个 OR 分支上做 BUILDSTRIPRE（全局删除
            // `+<build>` 片段），因此 `+1.2.3` / `1.2.3+build` 都退化为 `*` / `1.2.3`。
            let stripped = strip_build_metadata(alternative);
            let alternative = js_trim(&stripped);
            // node：空的 AND 组等价于 `*`，而 `* || X` 整体就是 `*`。
            if alternative.is_empty() {
                requirements.push(semver::VersionReq::STAR);
                continue;
            }
            requirements.push(parse_conjunction(alternative)?);
        }
        Some(Self { requirements })
    }

    /// `semver.satisfies(version, range)`。
    ///
    /// 任一 OR 分支匹配即满足。`version` 必须是已解析的 `semver::Version`；
    /// prerelease 只在比较器允许时匹配，该规则来自 `semver` crate 的
    /// `VersionReq::matches`，本函数不做额外判断。
    pub(crate) fn satisfies(&self, version: &semver::Version) -> bool {
        self.requirements
            .iter()
            .any(|requirement| requirement.matches(version))
    }
}

/// node-semver 的通配符分量：`x`、`X`、`*`（大小写敏感）。
fn is_wildcard_component(component: &str) -> bool {
    component == "x" || component == "X" || component == "*"
}

/// 去掉小写 `v` 前缀。
fn strip_version_prefix(value: &str) -> &str {
    // node-semver 的 `v?` 只接受小写 `v`（`semver.valid("V1.2.3") === null`）。
    value.strip_prefix('v').unwrap_or(value)
}

/// 截断 `+` 之后的 build 元数据。
///
/// 只负责截断；调用 `PartialVersion::parse` 前已先校验 build 片段合法，因此这里不再
/// 区分 `1.2.3+` 与 `1.2.3+abc`。
fn strip_build(value: &str) -> &str {
    match value.find('+') {
        Some(index) => &value[..index],
        None => value,
    }
}

/// node `BUILDSTRIPRE`：全局删除 `+<valid build>` 片段（`parseRange` 的第一步）。
///
/// 删除是全局的，`+build` 可以出现在区间任意位置；只匹配 node 的 BUILD 语法
/// （`[0-9A-Za-z-]+` 点分段）。不合法或不完整的 `+` 序列会保留下来，交给后续解析
/// 判为非法，而不是在这里静默吞掉。
fn strip_build_metadata(text: &str) -> String {
    static BUILD_RE: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r"\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*").expect("valid build regex")
    });
    BUILD_RE.replace_all(text, "").into_owned()
}

/// 解析一个 AND 组（空白分隔的比较器，可能包含 `a - b` 连字符区间）。
///
/// 输入是一个 OR 分支（已 trim、已删 build 元数据）。先识别连字符区间：`A - B` 本身
/// 含空格，必须在按空白切分之前处理，否则会被拆成三个 token。否则按空白切成比较器；
/// `>= 1.2.3` 这种“运算符与版本之间带空白”的写法会合并成一个比较器。
///
/// 返回 `None` 表示该分支非法，调用方据此让整条 range 返回 `None`。输出文本使用
/// `semver::VersionReq` 语法：单个比较器由 `translate_comparator` 生成，多个之间用
/// `, `（`VersionReq` 的 AND 分隔符）连接。
fn parse_conjunction(text: &str) -> Option<semver::VersionReq> {
    // 连字符区间：`A - B`。
    if let Some((from, to)) = split_hyphen_range(text) {
        let lower = lower_bound(from).ok()?;
        let upper = upper_bound(to).ok()?;
        let mut parts: Vec<String> = Vec::new();
        if let Some(lower) = lower {
            parts.push(lower);
        }
        if let Some(upper) = upper {
            parts.push(upper);
        }
        if parts.is_empty() {
            return Some(semver::VersionReq::STAR);
        }
        return semver::VersionReq::parse(&parts.join(", ")).ok();
    }

    let tokens: Vec<&str> = text.split_whitespace().collect();
    let mut comparators: Vec<String> = Vec::new();
    let mut index = 0usize;
    while index < tokens.len() {
        let token = tokens[index];
        // `> 1.2.3`：比较器与版本号之间允许空白（node 的正则允许）。
        if is_operator_only(token) && index + 1 < tokens.len() {
            comparators.push(translate_comparator(&format!(
                "{token}{}",
                tokens[index + 1]
            ))?);
            index += 2;
            continue;
        }
        // 任一比较器非法就整组失败：`?` 直接返回 None，不会只丢掉坏 token。
        comparators.push(translate_comparator(token)?);
        index += 1;
    }
    let joined = comparators.join(", ");
    if joined.is_empty() {
        // 调用方已过滤空分支，这里保留空输入 = `*` 的兜底（与 node 的规则一致）。
        return Some(semver::VersionReq::STAR);
    }
    semver::VersionReq::parse(&joined).ok()
}

/// 该 token 是否只有比较运算符、需要与下一个 token 合并（如 `>=` + `1.2.3`）。
fn is_operator_only(token: &str) -> bool {
    matches!(token, ">" | "<" | ">=" | "<=" | "=" | "~" | "^" | "~>")
}

/// 连字符区间：node 的 HYPHENRANGE 要求 `-` 两侧都是空白。
///
/// 只把两侧都是空白的 `-` 当分隔符，因此 `1.2.3-alpha` 里的连字符不会被误拆。
/// 返回左右两段（已 trim）；任一侧为空或找不到分隔符时返回 `None`。函数只做切分，
/// 不校验版本语法，非法版本由 `lower_bound` / `upper_bound` 报错。
fn split_hyphen_range(text: &str) -> Option<(&str, &str)> {
    let mut previous: Option<char> = None;
    for (index, character) in text.char_indices() {
        if character == '-' {
            let previous_is_space = previous.is_some_and(char::is_whitespace);
            let next_is_space = text[index + 1..]
                .chars()
                .next()
                .is_some_and(char::is_whitespace);
            if previous_is_space && next_is_space {
                let from = js_trim(&text[..index]);
                let to = js_trim(&text[index + 1..]);
                if from.is_empty() || to.is_empty() {
                    return None;
                }
                return Some((from, to));
            }
        }
        previous = Some(character);
    }
    None
}

/// node-semver 的偏版本：`M` / `M.m` / `M.m.p[-pre][+build]`（通配符解析为 `None`）。
///
/// 这是比较器和连字符端点共用的中间表示：缺失的分量记为 `None`，通配符分量
/// （`x`/`X`/`*`）也归一化为 `None`，后续按 node 的规则补 0 或抬高上界。约束：
/// - `prerelease` 只有三个数字分量齐全时才接受（`1.2-alpha` 非法）；
/// - 数字分量不允许前导零（`01.2.3` 非法）；
/// - 通配符只能作为后缀（`1.x.3` 非法）。
#[derive(Debug, Clone, PartialEq, Eq)]
struct PartialVersion {
    /// 主版本；`None` 表示缺失或通配符（`x`/`X`/`*`）。
    major: Option<u64>,
    /// 次版本；`None` 表示缺失或通配符。
    minor: Option<u64>,
    /// 补丁版本；`None` 表示缺失或通配符。
    patch: Option<u64>,
    /// prerelease 标识串（不含前导 `-`）；只有三个数字分量齐全时才可能为 `Some`。
    prerelease: Option<String>,
}

impl PartialVersion {
    /// 解析一个偏版本；非法返回 `None`。调用方据此让整条 range 判为 `null`。
    fn parse(value: &str) -> Option<Self> {
        let value = strip_version_prefix(js_trim(value));
        if value.is_empty() {
            return None;
        }
        // `+` 之后必须是合法 build；`1.2.3+` 不是合法 range（node 返回 null）。
        if let Some(index) = value.find('+') {
            let build = &value[index + 1..];
            if !is_valid_build(build) {
                return None;
            }
        }
        let value = strip_build(value);
        let (core, prerelease) = match value.find('-') {
            Some(index) => (&value[..index], Some(value[index + 1..].to_string())),
            None => (value, None),
        };
        if prerelease.as_ref().is_some_and(|pre| pre.is_empty()) {
            return None;
        }
        let parts: Vec<&str> = core.split('.').collect();
        // node 只接受最多三个数字分量；多余的点段非法。
        if parts.is_empty() || parts.len() > 3 {
            return None;
        }
        let mut numbers = [None, None, None];
        let mut wildcard_seen = false;
        for (index, part) in parts.iter().enumerate() {
            if is_wildcard_component(part) {
                wildcard_seen = true;
                continue;
            }
            // 通配符之后不能再出现数字：`1.x.3` 非法。
            if wildcard_seen {
                return None;
            }
            numbers[index] = Some(parse_numeric(part)?);
        }
        // node 只在完整版本上接受 prerelease（`1.2-alpha` 不是合法 range）。
        if prerelease.is_some() && numbers.iter().any(Option::is_none) {
            return None;
        }
        Some(Self {
            major: numbers[0],
            minor: numbers[1],
            patch: numbers[2],
            prerelease,
        })
    }
}

/// node 的 BUILD：`[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*`。
///
/// 传入的是 `+` 之后的片段；空片段、空点段或含其它字符都判非法。
fn is_valid_build(build: &str) -> bool {
    !build.is_empty()
        && build.split('.').all(|identifier| {
            !identifier.is_empty()
                && identifier
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
}

/// node 的数字分量：只允许 ASCII 数字；多位数不能以 `0` 开头。
fn parse_numeric(part: &str) -> Option<u64> {
    if part.len() > 1 && part.starts_with('0') {
        return None;
    }
    if part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    part.parse().ok()
}

/// 连字符区间的下界：`A` 按 `>=` 的偏版本补齐规则。
///
/// 缺失的分量补 0（`1` → `>=1.0.0`，`1.2` → `>=1.2.0`）；通配符（`x`）让下界消失。
/// prerelease 只有完整版本才有，直接拼在补全后的版本后面。返回的文本是
/// `semver::VersionReq` 的单个比较器。
///
/// 外层 `None` 表示无法解析（整条 range 非法），内层 `None` 表示下界为空（`x - ...`）。
fn lower_bound(value: &str) -> Result<Option<String>, ()> {
    let parsed = PartialVersion::parse(value).ok_or(())?;
    let Some(major) = parsed.major else {
        return Ok(None);
    };
    let minor = parsed.minor.unwrap_or(0);
    let patch = parsed.patch.unwrap_or(0);
    let mut text = format!(">={major}.{minor}.{patch}");
    if let (Some(_), Some(prerelease)) = (parsed.patch, &parsed.prerelease) {
        text.push('-');
        text.push_str(prerelease);
    }
    Ok(Some(text))
}

/// 连字符区间的上界：node 的 `hyphenReplace`（偏版本抬高到下个 major/minor 的 `-0`）。
///
/// 规则：`M` → `<{M+1}.0.0-0`，`M.m` → `<M.{m+1}.0-0`（`-0` 哨兵排除下一版本的
/// prerelease，例如 `1 - 2` 覆盖到 `2.x` 但不含 `3.0.0-0`），完整版本 → `<=M.m.p[-pre]`
/// （包含）。通配符（`x`）让上界消失。
///
/// 外层 `None` 表示无法解析，内层 `None` 表示上界为空（`... - x`）。
fn upper_bound(value: &str) -> Result<Option<String>, ()> {
    let parsed = PartialVersion::parse(value).ok_or(())?;
    let Some(major) = parsed.major else {
        return Ok(None);
    };
    match (parsed.minor, parsed.patch) {
        (None, _) => Ok(Some(format!("<{}.0.0-0", major + 1))),
        (Some(minor), None) => Ok(Some(format!("<{major}.{}.0-0", minor + 1))),
        (Some(minor), Some(patch)) => {
            if let Some(prerelease) = &parsed.prerelease {
                Ok(Some(format!("<={major}.{minor}.{patch}-{prerelease}")))
            } else {
                Ok(Some(format!("<={major}.{minor}.{patch}")))
            }
        }
    }
}

/// 把一个 node 比较器翻译成 `semver::VersionReq` 能接受的比较器文本。
///
/// 运算符按最长匹配优先（`>=` 先于 `>`），与 node 的解析一致。返回值可能含逗号：
/// 无运算符的 x-range 会展开成上下界两个比较器（如 `1.x` → `>=1.0.0, <2.0.0-0`），
/// 由调用方统一用 `, ` 连接。无法识别的 token 返回 `None`，最终让整条 range 判为非法。
///
/// 各分支复刻 node 的 `replaceXRange` / `replaceTilde` / `replaceCaret` 规则：偏版本补 0、
/// 严格比较抬高到下个分量、上界用 `-0` 哨兵排除下一版本的 prerelease。
///
/// 分工上，`~` / `^` 分支只补全缺失分量，随后把 `~1.2` / `^1.2.3` 原样交给
/// `semver::VersionReq` 自身的 tilde/caret 语义解释；x-range、`>=`、`>`、`<`、`<=`
/// 分支则显式展开成上下界文本。
fn translate_comparator(token: &str) -> Option<String> {
    let operators = [">=", "<=", "~>", ">", "<", "=", "~", "^"];
    let mut operator = "";
    let mut rest = token;
    for candidate in operators {
        if let Some(stripped) = token.strip_prefix(candidate) {
            operator = candidate;
            rest = stripped;
            break;
        }
    }
    let rest = strip_version_prefix(js_trim(rest));
    if rest.is_empty() {
        return None;
    }

    // `*` / `x` 单独出现。
    if is_wildcard_component(rest) {
        return match operator {
            "" | "=" | "~" | "^" | "~>" | ">=" | "<=" => Some("*".to_string()),
            // node：`>*` / `<*` → `<0.0.0-0`（匹配不到任何版本）。
            _ => Some("<0.0.0-0".to_string()),
        };
    }

    match operator {
        // `~>` 与 `~` 同义（node-semver：`~>1.2` = `>=1.2.0 <1.3.0-0`）。
        "~>" | "~" => {
            let parsed = PartialVersion::parse(rest)?;
            let major = parsed.major?;
            let Some(minor) = parsed.minor else {
                return Some(format!("~{major}"));
            };
            match (parsed.patch, &parsed.prerelease) {
                (Some(patch), Some(prerelease)) => {
                    Some(format!("~{major}.{minor}.{patch}-{prerelease}"))
                }
                (Some(patch), None) => Some(format!("~{major}.{minor}.{patch}")),
                (None, _) => Some(format!("~{major}.{minor}")),
            }
        }
        "^" => {
            let parsed = PartialVersion::parse(rest)?;
            let Some(major) = parsed.major else {
                return Some("*".to_string());
            };
            let Some(minor) = parsed.minor else {
                return Some(format!("^{major}"));
            };
            match (parsed.patch, &parsed.prerelease) {
                (Some(patch), Some(prerelease)) => {
                    Some(format!("^{major}.{minor}.{patch}-{prerelease}"))
                }
                (Some(patch), None) => Some(format!("^{major}.{minor}.{patch}")),
                (None, _) => Some(format!("^{major}.{minor}")),
            }
        }
        "" | "=" => xrange(rest),
        ">=" => {
            let parsed = PartialVersion::parse(rest)?;
            let Some(major) = parsed.major else {
                return Some("*".to_string());
            };
            let minor = parsed.minor.unwrap_or(0);
            let patch = parsed.patch.unwrap_or(0);
            match (parsed.patch, &parsed.prerelease) {
                (Some(_), Some(prerelease)) => {
                    Some(format!(">={major}.{minor}.{patch}-{prerelease}"))
                }
                _ => Some(format!(">={major}.{minor}.{patch}")),
            }
        }
        ">" => {
            let parsed = PartialVersion::parse(rest)?;
            let Some(major) = parsed.major else {
                return Some("<0.0.0-0".to_string());
            };
            // 偏版本的严格大于要抬高到下个分量再取 `>=`：`>1.2` = `>=1.3.0`，
            // `>1` = `>=2.0.0`；只有完整版本才保留 `>` 本身。
            match (parsed.minor, parsed.patch) {
                (None, _) => Some(format!(">={}.0.0", major + 1)),
                (Some(minor), None) => Some(format!(">={major}.{}.0", minor + 1)),
                (Some(minor), Some(patch)) => match &parsed.prerelease {
                    Some(prerelease) => Some(format!(">{major}.{minor}.{patch}-{prerelease}")),
                    None => Some(format!(">{major}.{minor}.{patch}")),
                },
            }
        }
        "<" => {
            let parsed = PartialVersion::parse(rest)?;
            let Some(major) = parsed.major else {
                return Some("<0.0.0-0".to_string());
            };
            match (parsed.minor, parsed.patch) {
                (None, _) => Some(format!("<{major}.0.0-0")),
                (Some(minor), None) => Some(format!("<{major}.{minor}.0-0")),
                (Some(minor), Some(patch)) => match &parsed.prerelease {
                    Some(prerelease) => Some(format!("<{major}.{minor}.{patch}-{prerelease}")),
                    None => Some(format!("<{major}.{minor}.{patch}")),
                },
            }
        }
        "<=" => {
            let parsed = PartialVersion::parse(rest)?;
            let Some(major) = parsed.major else {
                return Some("*".to_string());
            };
            match (parsed.minor, parsed.patch) {
                (None, _) => Some(format!("<{}.0.0-0", major + 1)),
                (Some(minor), None) => Some(format!("<{major}.{}.0-0", minor + 1)),
                (Some(minor), Some(patch)) => match &parsed.prerelease {
                    Some(prerelease) => Some(format!("<={major}.{minor}.{patch}-{prerelease}")),
                    None => Some(format!("<={major}.{minor}.{patch}")),
                },
            }
        }
        _ => None,
    }
}

/// 无运算符的版本：完整版本是精确匹配，偏版本/X-range 展开为上下界。
///
/// 完整版本 → `=M.m.p[-pre]`（用 `=` 而不是裸版本号，方便与其他比较器用 `, ` 拼接）；
/// 偏版本 → `>=下界, <上界-0`；通配符版本（`x`/`X`/`*`）→ `*`。返回的字符串可能含
/// 逗号，说明它已经是一组 AND 比较器。
fn xrange(value: &str) -> Option<String> {
    let parsed = PartialVersion::parse(value)?;
    let Some(major) = parsed.major else {
        return Some("*".to_string());
    };
    match (parsed.minor, parsed.patch) {
        (None, _) => Some(format!(">={major}.0.0, <{}.0.0-0", major + 1)),
        (Some(minor), None) => Some(format!(">={major}.{minor}.0, <{major}.{}.0-0", minor + 1)),
        (Some(minor), Some(patch)) => match &parsed.prerelease {
            Some(prerelease) => Some(format!("={major}.{minor}.{patch}-{prerelease}")),
            None => Some(format!("={major}.{minor}.{patch}")),
        },
    }
}

/// `maxSatisfying(versions, range)`：返回满足区间的最高版本。
///
/// node 的 `maxSatisfying` 返回数组里的**原始字符串**（不做标准化），非法版本被跳过。
///
/// 比较用 semver 优先级；`v1.2.3` 与 `1.2.3`、`1.2.3+build` 与 `1.2.3` 视为相等，
/// 只有严格更大才替换，因此并列时**先出现**的原始串被保留。空数组或全部不满足时返回
/// `None`。调用方是 `get_latest_npm_version`（配置了合法区间时）。
pub(crate) fn max_satisfying(versions: &[String], range: &NpmRange) -> Option<String> {
    let mut best: Option<(semver::Version, String)> = None;
    for raw in versions {
        let Some(version) = valid_version(raw) else {
            continue;
        };
        if !range.satisfies(&version) {
            continue;
        }
        if best.as_ref().is_none_or(|(current, _)| version > *current) {
            best = Some((version, raw.clone()));
        }
    }
    best.map(|(_, raw)| raw)
}

/// `[...versions].sort(rcompare)[0]`：按 semver 优先级降序，返回原始字符串。
///
/// node 的 `rcompare` 对非法版本抛 `Invalid Version: <v>`（`getLatestNpmVersion` /
/// `get_latest_npm_version` 的调用方会把这个错误折叠成“无更新”），因此这里返回 `Err`
/// 而不是跳过。
///
/// 按输入顺序扫描，遇到**第一个**非法版本就返回错误，错误文本使用该原始串；空切片返回
/// `Ok(None)`。
pub(crate) fn highest_version(versions: &[String]) -> Result<Option<String>, PackageManagerError> {
    let mut best: Option<(semver::Version, String)> = None;
    for raw in versions {
        let version = valid_version(raw).ok_or_else(|| err(format!("Invalid Version: {raw}")))?;
        if best.as_ref().is_none_or(|(current, _)| version > *current) {
            best = Some((version, raw.clone()));
        }
    }
    Ok(best.map(|(_, raw)| raw))
}

impl DefaultPackageManager {
    /// `parseSource(source)`（实例方法，与 TS 的私有方法对应）。
    pub(crate) fn parse_source(&self, source: &str) -> ParsedSource {
        parse_source(source)
    }

    /// `getPackageIdentity(source, scope?)`：把三种源归一化成去重使用的 key。
    ///
    /// - npm → `npm:<name>`：版本、标签、区间都不参与 identity，所以 `npm:foo@1` 与
    ///   `npm:foo@2` 被视为同一个包；
    /// - git → `git:<host>/<path>`：协议、`.git` 后缀与 `@ref` 已在 `parse_git_url` 里被
    ///   归一化掉（测试 `git_identity_normalizes_protocols_and_refs` 锁定）；
    /// - local → `local:<绝对路径>`：`Some(scope)` 用 `get_base_dir_for_scope` 解析，
    ///   project 作用域会先做 trust 检查，未受信时把错误原样返回；`None` 走 `resolve_path`，
    ///   以 `cwd` 为基准。
    ///
    /// 只有 local + `Some(scope)` 这一条路径可能失败；npm/git 分支不会返回错误。
    pub(crate) fn get_package_identity(
        &self,
        source: &str,
        scope: Option<SourceScope>,
    ) -> Result<String, PackageManagerError> {
        let parsed = self.parse_source(source);
        match parsed {
            ParsedSource::Npm(npm) => Ok(format!("npm:{}", npm.name)),
            ParsedSource::Git(git) => Ok(format!("git:{}/{}", git.host, git.path)),
            ParsedSource::Local(local) => match scope {
                Some(scope) => {
                    let base_dir = self.get_base_dir_for_scope(scope)?;
                    Ok(format!(
                        "local:{}",
                        self.resolve_path_from_base(&local.path, &base_dir)
                    ))
                }
                None => Ok(format!("local:{}", self.resolve_path(&local.path))),
            },
        }
    }

    /// `dedupePackages(packages)`：按 identity 去重，project 覆盖 user。
    ///
    /// 规则与 `spec/ca-resources.md` §2.10.13 一致：
    /// - 首次出现的 identity 直接追加，`seen` 记录它在 `result` 中的下标；
    /// - 旧条目是 project、新条目是 user，且旧项目条目是 `autoload:false` 的对象时，把
    ///   user 条目**追加**到末尾作为增量基底，两条都保留（项目条目在前）；
    /// - 其余情况只要新条目是 project 就替换旧条目；user 与 user 重复则丢弃后来的。
    ///
    /// 覆盖规则依赖输入顺序：调用方必须按 project 在前、user 在后传入
    /// （`resolve_inner` / `check_for_available_updates` 已这样组装），顺序反过来时
    /// “project 覆盖 user”不会触发。
    ///
    /// 错误来自 `get_package_source_string`（对象缺少 `source`）与 `get_package_identity`
    /// （local + project 作用域未受信），并直接向上返回。
    pub(crate) fn dedupe_packages(
        &self,
        packages: &[ConfiguredEntry],
    ) -> Result<Vec<ConfiguredEntry>, PackageManagerError> {
        let mut result: Vec<ConfiguredEntry> = Vec::new();
        let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        // `seen` 只记录每个 identity 首次出现的位置；增量分支追加新条目时不会更新它，
        // 因此后续同 identity 的条目仍然与首个 project 条目比较。
        for entry in packages {
            let identity = self
                .get_package_identity(get_package_source_string(&entry.pkg)?, Some(entry.scope))?;
            match seen.get(&identity).copied() {
                None => {
                    seen.insert(identity, result.len());
                    result.push(entry.clone());
                }
                Some(index) => {
                    let existing = &result[index];
                    if existing.scope == SourceScope::Project && entry.scope == SourceScope::User {
                        if let PackageSource::Object(object) = &existing.pkg
                            && object.autoload == Some(false)
                        {
                            result.push(entry.clone());
                        }
                    } else if entry.scope == SourceScope::Project {
                        result[index] = entry.clone();
                    }
                }
            }
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manager() -> DefaultPackageManager {
        let dir = tempfile::tempdir().expect("tempdir");
        let settings = kiana_config::settings::Settings::default();
        DefaultPackageManager::new(super::super::PackageManagerOptions {
            cwd: dir.path().to_string_lossy().into_owned(),
            agent_dir: dir.path().join("agent").to_string_lossy().into_owned(),
            settings_manager: kiana_config::settings::SettingsManager::in_memory(settings),
            builtin_extensions: vec![],
        })
    }

    #[test]
    fn parse_npm_spec_matches_the_regex() {
        assert_eq!(
            parse_npm_spec("@scope/pkg@1.2.3"),
            ("@scope/pkg".to_string(), Some("1.2.3".to_string()))
        );
        assert_eq!(
            parse_npm_spec("pkg@latest"),
            ("pkg".to_string(), Some("latest".to_string()))
        );
        assert_eq!(parse_npm_spec("pkg"), ("pkg".to_string(), None));
        assert_eq!(
            parse_npm_spec("@a@b"),
            ("@a".to_string(), Some("b".to_string()))
        );
        assert_eq!(parse_npm_spec(""), ("".to_string(), None));
    }

    #[test]
    fn parse_source_types_from_docs_examples() {
        let npm = parse_source("npm:@scope/pkg@1.2.3");
        assert!(matches!(npm, ParsedSource::Npm(ref source) if source.pinned));

        let npm_range = parse_source("npm:@scope/pkg@^1.2.3");
        assert!(matches!(npm_range, ParsedSource::Npm(ref source) if !source.pinned));

        let npm_plain = parse_source("npm:pkg");
        assert!(matches!(npm_plain, ParsedSource::Npm(ref source) if !source.pinned));

        assert!(matches!(
            parse_source("git:github.com/user/repo@v1"),
            ParsedSource::Git(_)
        ));
        assert!(matches!(
            parse_source("https://github.com/user/repo@v1"),
            ParsedSource::Git(_)
        ));
        assert!(matches!(
            parse_source("git:git@github.com:user/repo@v1"),
            ParsedSource::Git(_)
        ));
        assert!(matches!(
            parse_source("ssh://git@github.com/user/repo@v1"),
            ParsedSource::Git(_)
        ));

        assert!(matches!(
            parse_source("/absolute/path/to/package"),
            ParsedSource::Local(_)
        ));
        assert!(matches!(
            parse_source("./relative/path/to/package"),
            ParsedSource::Local(_)
        ));
        assert!(matches!(
            parse_source("../relative/path/to/package"),
            ParsedSource::Local(_)
        ));
        assert!(matches!(
            parse_source("github.com/user/repo"),
            ParsedSource::Local(_)
        ));
        assert!(matches!(
            parse_source("git@github.com:user/repo"),
            ParsedSource::Local(_)
        ));
    }

    #[test]
    fn never_parses_dot_relative_paths_as_git() {
        match parse_source("./packages/agent-timers") {
            ParsedSource::Local(local) => assert_eq!(local.path, "./packages/agent-timers"),
            other => panic!("unexpected {other:?}"),
        }
        match parse_source("../packages/agent-timers") {
            ParsedSource::Local(local) => assert_eq!(local.path, "../packages/agent-timers"),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn git_identity_normalizes_protocols_and_refs() {
        let manager = manager();
        for url in [
            "https://github.com/user/repo",
            "https://github.com/user/repo@v1.0.0",
            "git:github.com/user/repo",
            "https://github.com/user/repo.git",
            "ssh://git@github.com/user/repo",
            "git:git@github.com:user/repo.git",
        ] {
            assert_eq!(
                manager.get_package_identity(url, None).expect("identity"),
                "git:github.com/user/repo",
                "url {url}"
            );
        }
        assert_eq!(
            manager
                .get_package_identity("git:git@github.com:user/repo2", None)
                .expect("identity"),
            "git:github.com/user/repo2"
        );
    }

    #[test]
    fn npm_semver_matches_node_reference() {
        // valid（`semver.valid` 只接受小写 `v` 前缀，并 trim 空白）。
        assert!(valid_version("1.2.3").is_some());
        assert!(valid_version("v1.2.3").is_some());
        assert!(valid_version(" 1.2.3 ").is_some());
        assert!(valid_version("=1.2.3").is_none());
        assert!(valid_version("V1.2.3").is_none());
        assert!(valid_version("1.2").is_none());
        assert!(valid_version("latest").is_none());
        assert!(valid_version("").is_none());

        // validRange
        assert!(NpmRange::parse("latest").is_none());
        assert!(NpmRange::parse("1.2.3").is_some());
        assert!(NpmRange::parse("^1||^2").is_some());

        // satisfies
        let satisfies = |version: &str, range: &str| {
            let version = valid_version(version).expect("version");
            NpmRange::parse(range)
                .unwrap_or_else(|| panic!("range {range}"))
                .satisfies(&version)
        };
        assert!(!satisfies("1.5.0", "1.2.3"));
        assert!(satisfies("1.5.0", "^1.2.3"));
        assert!(!satisfies("2.0.0", "1.x"));
        assert!(satisfies("1.9.0", "1.x"));
        assert!(satisfies("2.1.0", "^1||^2"));
        assert!(!satisfies("3.0.0", "^1||^2"));
        assert!(satisfies("1.2.9", "~1.2.3"));
        assert!(satisfies("1.2.9", "~>1.2.3"));
        assert!(!satisfies("1.3.0", "~>1.2.3"));
        assert!(satisfies("1.2.9", "1.2.3 - 2.3.4"));
        assert!(satisfies("1.9.0", "1"));
        assert!(!satisfies("2.0.0", "1"));

        // 连字符区间 + 偏版本上界：`1.2.3 - 2` = `>=1.2.3 <3.0.0-0`（不是 `<=2.0.0`）。
        assert!(satisfies("2.1.0", "1.2.3 - 2"));
        assert!(satisfies("1.4.5", "1.2 - 1.4"));
        assert!(satisfies("2.3.9", "1.2.3 - 2.3"));
        assert!(!satisfies("3.0.0", "1.2.3 - 2"));
        assert!(!satisfies("1.5.0", "1.2 - 1.4"));
        assert!(!satisfies("2.4.0", "1.2.3 - 2.3"));

        // 带 prerelease 的比较器与连字符下界。
        assert!(!satisfies("1.0.0", ">=1.2.3-alpha.1"));
        assert!(!satisfies("1.2.4-beta", ">=1.2.3-beta"));
        assert!(satisfies("1.2.3-beta.2", ">=1.2.3-beta"));
        assert!(!satisfies("3.0.0", "1.2.3-beta.1 - 2"));
        assert!(satisfies("1.2.3-beta.2", "1.2.3-beta.1 - 2"));

        // maxSatisfying / 排序
        let versions = vec![
            "1.0.0".to_string(),
            "1.2.0".to_string(),
            "2.0.0".to_string(),
        ];
        assert_eq!(
            max_satisfying(&versions, &NpmRange::parse("^1.0.0").expect("range")),
            Some("1.2.0".to_string())
        );
        assert_eq!(
            highest_version(&versions).expect("highest"),
            Some("2.0.0".to_string())
        );
    }

    #[test]
    fn npm_semver_matches_node_golden_table() {
        let golden: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/package/semver_golden.json"
        ))
        .expect("golden json");
        for entry in golden["ranges"].as_array().expect("ranges") {
            let range_text = entry["range"].as_str().expect("range text");
            let expected_valid = entry["valid"].as_bool().expect("valid");
            let parsed = NpmRange::parse(range_text);
            assert_eq!(
                parsed.is_some(),
                expected_valid,
                "validRange({range_text:?})"
            );
            let Some(parsed) = parsed else {
                continue;
            };
            for (version_text, expected) in entry["satisfies"].as_object().expect("satisfies") {
                let version = valid_version(version_text)
                    .unwrap_or_else(|| panic!("version {version_text:?}"));
                assert_eq!(
                    parsed.satisfies(&version),
                    expected.as_bool().expect("expected bool"),
                    "satisfies({version_text:?}, {range_text:?})"
                );
            }
        }
        for (version_text, expected) in golden["validVersions"].as_object().expect("validVersions")
        {
            assert_eq!(
                valid_version(version_text).is_some(),
                expected.is_string(),
                "valid({version_text:?})"
            );
        }
    }

    #[test]
    fn highest_version_throws_like_rcompare_on_invalid_entries() {
        // node: [...versions].sort(rcompare)[0] 对非法版本抛 "Invalid Version: <v>"。
        let versions = vec!["1.0.0".to_string(), "not-a-version".to_string()];
        let error = highest_version(&versions).expect_err("invalid version");
        assert_eq!(error.message(), "Invalid Version: not-a-version");
        // maxSatisfying 则跳过非法版本（Range.test 返回 false）。
        assert_eq!(
            max_satisfying(&versions, &NpmRange::parse("^1.0.0").expect("range")),
            Some("1.0.0".to_string())
        );
    }

    #[test]
    fn dedupe_keeps_project_and_handles_autoload_delta() {
        let manager = manager();
        let project = ConfiguredEntry {
            pkg: PackageSource::Text("npm:foo".into()),
            scope: SourceScope::Project,
        };
        let user = ConfiguredEntry {
            pkg: PackageSource::Text("npm:foo".into()),
            scope: SourceScope::User,
        };
        let deduped = manager
            .dedupe_packages(&[project.clone(), user.clone()])
            .expect("dedupe");
        assert_eq!(deduped.len(), 1);
        assert_eq!(deduped[0].scope, SourceScope::Project);

        let object = kiana_config::settings::schema::PackageSourceObject {
            source: Some("npm:foo".into()),
            autoload: Some(false),
            ..Default::default()
        };
        let delta = ConfiguredEntry {
            pkg: PackageSource::Object(object),
            scope: SourceScope::Project,
        };
        let deduped = manager.dedupe_packages(&[delta, user]).expect("dedupe");
        assert_eq!(deduped.len(), 2);
        assert_eq!(deduped[0].scope, SourceScope::Project);
        assert_eq!(deduped[1].scope, SourceScope::User);
    }
}
