//! utils/{changelog,version-check,windows-self-update,wsl,deprecation,kiana-user-agent}.ts。
//!
//! 本文件合并 6 个 TS 工具模块（§5 的合并映射，不新建 `version_check.rs`/`wsl.rs` 等）：
//!
//! | 小节 | TS 源 |
//! |---|---|
//! | changelog | `utils/changelog.ts`（196 行） |
//! | version-check | `utils/version-check.ts`（109 行） |
//! | kiana-user-agent | `utils/kiana-user-agent.ts`（6 行） |
//! | wsl | `utils/wsl.ts`（15 行） |
//! | deprecation | `utils/deprecation.ts`（14 行） |
//! | windows-self-update | `utils/windows-self-update.ts`（84 行） |
//!
//! 各小节的公开项、语义与已知差异见各自文档。
//!
//! # 为什么合并成一个文件
//! 这 6 个 TS 模块都只有几行到一百多行（最长 196 行），且彼此不共享数据结构；
//! `ARCHITECTURE.md` §5 的合并映射把它们归入同一个文件，因此这里不新建
//! `version_check.rs`/`wsl.rs` 等模块。文件内用 `// ===` 分隔条划分小节，阅读时先定位
//! 小节，不要把整份文件当成一个统一的功能。
//!
//! # 各小节的职责边界
//! - changelog：文件读取（[`parse_changelog`]）加纯函数（解析、链接改写、版本比较），
//!   自身不发起网络请求。
//! - version-check：本文件中唯一发起网络请求的小节，经
//!   [`crate::management_http::fetch_with_retry`] 访问由 `KIANA_API_BASE_URL` 派生的端点。
//! - kiana-user-agent：转发给 `kiana_ai::utils::identity::format_user_agent`，不自带格式逻辑。
//! - wsl：平台探测，只读环境变量与 `/proc/version`。
//! - deprecation：进程级去重后调用 `tracing::warn!`。
//! - windows-self-update：只做本地区目录清理；隔离原生依赖的入口在 Rust 侧是 no-op。
//!
//! # 一次典型调用
//! 1. `parse_changelog(crate::config::get_changelog_path())` 得到按文件顺序排列的
//!    [`ChangelogEntry`]；
//! 2. 展示“新增了什么”时先用 [`get_new_entries`] 过滤出严格新于上次版本的条目；
//! 3. 展示前逐条调用 [`normalize_changelog_links`]，把包内相对链接改写成锁定 tag 的
//!    GitHub 源码链接。
//!
//! 版本检查是独立流程：[`check_for_new_kiana_version`] → [`get_latest_kiana_release`] →
//! `fetch_with_retry`；前者吞掉所有错误并返回 `None`，因此可安全地放在启动时的
//! fire-and-forget 任务里（`kiana-interactive/src/interactive_mode.rs` 即如此使用）。
//!
//! # 注意事项
//! - `LATEST_VERSION_URL_OVERRIDE` 是**进程级**的 `static Mutex<Option<String>>`：保存测试
//!   覆盖的端点 URL，测试必须串行访问（`tests/version_check.rs` 用一把全局锁），业务代码
//!   不应把它当作可并行的数据源。
//! - `EMITTED_DEPRECATION_WARNINGS` 是**进程级**的
//!   `static LazyLock<Mutex<HashSet<String>>>`：保存已告警消息的全文，测试同样必须串行访问
//!   （`tests/deprecation.rs` 用一把全局锁），否则用例之间会互相抑制。
//! - 本文件里的互斥锁都用 `unwrap_or_else(|poisoned| poisoned.into_inner())` 取回数据：
//!   持锁线程 panic 后后续调用继续工作，而不是级联 panic。这一策略本身不保证被保护数据的
//!   业务一致性。
//! - 多个函数逐字复刻 JS 语义（`kiana_json::js_trim`、`kiana_json::parse_js_number`、
//!   `kiana_json::js_parse::parse`）；换成 Rust 标准库的 `trim`/`parse`/`serde_json` 会改变
//!   空白集合、数字接受度与错误文案，进而破坏与 TS 的对齐。
//! - 改本文件会影响 `kiana-interactive`（`/changelog` 与启动提示）、`kiana-coding-agent`
//!   （自更新与隔离区清理）以及 `kiana-core` 内部的 `clipboard`/`image_process`（WSL 探测）：
//!   这些调用方只依赖公开函数签名，但依赖返回文本/`Option` 的语义。

use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::management_http::{FetchError, FetchRetryOptions, fetch_with_retry};

// ===========================================================================
// changelog
// ===========================================================================

/// `GITHUB_REPO`（TS 逐字）。
const GITHUB_REPO: &str = "earendil-works/kiana";

/// `CHANGELOG_LINK_BASE_PATH`（TS 逐字）。
const CHANGELOG_LINK_BASE_PATH: &str = "packages/coding-agent";

/// ECMAScript `\s`（`WhiteSpace` + `LineTerminator`）的字符类**内容**。
///
/// 与 Rust regex 的 `\s` 不同：JS 含 U+FEFF，不含 U+0085；这里逐字复刻 JS 集合，
/// 供行内链接正则与版本标题正则使用。
const JS_WHITESPACE_CHARS: &str = r"\t\n\x{0B}\f\r \x{00A0}\x{1680}\x{2000}-\x{200A}\x{2028}\x{2029}\x{202F}\x{205F}\x{3000}\x{FEFF}";

/// `LEGACY_REPO_RE = /^https:\/\/github\.com\/(?:badlogic|earendil-works)\/kiana-mono(?=\/|$)/`。
///
/// Rust regex 不支持 lookahead；把 `(?=\/|$)` 改写成捕获 `(\/|$)`，替换时把捕获原样保留。
static LEGACY_REPO_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^https://github\.com/(?:badlogic|earendil-works)/kiana-mono(/|$)")
        .expect("legacy repo regex is valid")
});

/// `URL_SCHEME_RE = /^[a-z][a-z0-9+.-]*:/i`（ASCII 大小写不敏感，与 JS 的 `/i` 对齐）。
static URL_SCHEME_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i-u)^[a-z][a-z0-9+.-]*:").expect("url scheme regex is valid"));

/// `INLINE_MARKDOWN_LINK_RE = /(!?\[[^\]\n]+\]\()([^\s)]+)((?:\s+[^)]*)?\))/g`。
static INLINE_MARKDOWN_LINK_RE: LazyLock<Regex> = LazyLock::new(|| {
    let pattern = format!(
        r"(!?\[[^\]\n]+\]\()([^{ws})]+)((?:[{ws}]+[^)]*)?\))",
        ws = JS_WHITESPACE_CHARS
    );
    Regex::new(&pattern).expect("inline markdown link regex is valid")
});

/// `##\s+\[?(\d+)\.(\d+)\.(\d+)\]?`（`\d` 在 JS 里是 ASCII 数字，这里用 `[0-9]` 防止
/// Rust regex 的 Unicode `\d` 误配阿拉伯-印度数字）。
static CHANGELOG_VERSION_RE: LazyLock<Regex> = LazyLock::new(|| {
    let pattern = format!(
        r"##[{ws}]+\[?([0-9]+)\.([0-9]+)\.([0-9]+)\]?",
        ws = JS_WHITESPACE_CHARS
    );
    Regex::new(&pattern).expect("changelog version regex is valid")
});

/// CHANGELOG.md 的一条版本条目（TS: `changelog.ts#ChangelogEntry`）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangelogEntry {
    /// 主版本号。
    pub major: i64,
    /// 次版本号。
    pub minor: i64,
    /// 修订号。
    pub patch: i64,
    /// 条目正文（**包含**版本标题行本身；`join("\n").trim()` 之后的结果）。
    pub content: String,
}

/// [`normalize_changelog_links`] 的版本参数：`string | ChangelogEntry`。
#[derive(Debug, Clone, Copy)]
pub enum ChangelogVersion<'a> {
    /// 版本字符串（如 `"0.79.0"`）；缺 `v` 前缀时补上。
    Str(&'a str),
    /// 版本条目；用 `major.minor.patch`。
    Entry(&'a ChangelogEntry),
}

impl<'a> From<&'a str> for ChangelogVersion<'a> {
    fn from(version: &'a str) -> Self {
        Self::Str(version)
    }
}

impl<'a> From<&'a ChangelogEntry> for ChangelogVersion<'a> {
    fn from(entry: &'a ChangelogEntry) -> Self {
        Self::Entry(entry)
    }
}

impl<'a> From<&'a String> for ChangelogVersion<'a> {
    fn from(version: &'a String) -> Self {
        Self::Str(version)
    }
}

fn entry_version(entry: &ChangelogEntry) -> String {
    format!("{}.{}.{}", entry.major, entry.minor, entry.patch)
}

fn normalize_tag(version: &ChangelogVersion<'_>) -> String {
    let version_string = match version {
        ChangelogVersion::Str(value) => (*value).to_string(),
        ChangelogVersion::Entry(entry) => entry_version(entry),
    };
    if version_string.starts_with('v') {
        version_string
    } else {
        format!("v{version_string}")
    }
}

/// `normalizeChangelogLinks(markdown, version)`：把包内相对链接改写成锁定到 tag 的 GitHub
/// 源码链接。
///
/// TS: `changelog.ts#normalizeChangelogLinks`。`version` 接受 `&str` 或 `&ChangelogEntry`
/// （TS 的 `string | ChangelogEntry`）。
pub fn normalize_changelog_links<'a>(
    markdown: &str,
    version: impl Into<ChangelogVersion<'a>>,
) -> String {
    // 阶段 1：把 `string | ChangelogEntry` 统一成带 `v` 前缀的 tag（`0.79.0` → `v0.79.0`）。
    let tag = normalize_tag(&version.into());
    // 阶段 2：单遍替换所有行内链接/图片。只匹配 `[text](target ...)` 形式：引用式链接
    // （`[text][ref]`）与裸 URL 不匹配。捕获组 1 是 `[text](`（含可选的 `!` 图片前缀），
    // 2 是链接目标（`[^\s)]+`，遇到空白或 `)` 截止），3 是可选的前导空白 + 标题部分 + 收尾
    // `)`（空白本身属于组 3；无标题时组 3 就是 `)`），三段都逐字拼回。
    INLINE_MARKDOWN_LINK_RE
        .replace_all(markdown, |captures: &regex::Captures<'_>| {
            let prefix = captures.get(1).map(|m| m.as_str()).unwrap_or("");
            let target = captures.get(2).map(|m| m.as_str()).unwrap_or("");
            let suffix = captures.get(3).map(|m| m.as_str()).unwrap_or("");
            let normalized = normalize_changelog_link_target(target, &tag);
            format!("{prefix}{normalized}{suffix}")
        })
        .into_owned()
}

fn normalize_changelog_link_target(target: &str, tag: &str) -> String {
    // 阶段 1：把旧仓库 URL（`badlogic|earendil-works/kiana-mono`）的**前缀**替换成新仓库。
    // `target.replace(LEGACY_REPO_RE, "https://github.com/earendil-works/kiana")`：只替换匹配到的
    // 前缀，其余路径原样保留（`(?=\/|$)` 的 lookahead 改写成捕获 `$1`）。
    let legacy_replacement = format!("https://github.com/{GITHUB_REPO}${{1}}");
    let mut canonical = LEGACY_REPO_RE
        .replace(target, legacy_replacement.as_str())
        .into_owned();
    let repo_url = format!("https://github.com/{GITHUB_REPO}");

    // 阶段 2：已指向本仓库、但用浮动分支 `main`/`master` 的 `blob`/`tree` 链接，把分支名
    // 换成目标 tag。其它分支名与其它仓库的链接不在此处改写。
    for route in ["blob", "tree"] {
        for branch in ["main", "master"] {
            let floating_ref_prefix = format!("{repo_url}/{route}/{branch}/");
            if let Some(rest) = canonical.strip_prefix(&floating_ref_prefix) {
                canonical = format!("{repo_url}/{route}/{tag}/{rest}");
            }
        }
    }

    // 阶段 3：锚点（`#...`）、协议相对 URL（`//host/...`）与带 scheme 的绝对 URL 不是包内
    // 相对路径，原样返回。注意已改写过的链接会落在这里，因此对同一段文本重复调用是幂等的。
    if canonical.starts_with('#')
        || canonical.starts_with("//")
        || URL_SCHEME_RE.is_match(&canonical)
    {
        return canonical;
    }

    // 阶段 4：先拆出 `?query` / `#fragment`，只把 path 部分当作仓库内路径解析；query 与
    // fragment 稍后原样拼回，不参与 percent-encoding。
    let local = split_local_target(&canonical);
    if local.path_part.is_empty() {
        return canonical;
    }

    let Some(repository_path) = resolve_repository_path(local.path_part) else {
        return canonical;
    };

    // 阶段 5：GitHub 用 `/blob/` 表示文件、`/tree/` 表示目录，靠原路径末尾 `/` 或 basename
    // 是否含 `.` 来猜（无扩展名的文件如 `LICENSE` 会被当成目录）。
    let route = if is_directory_target(local.path_part, &repository_path) {
        "tree"
    } else {
        "blob"
    };
    // 阶段 6：仅有 path 段做 `encodeURI` 转义；由于 `%` 本身也在转义集合里，已经是
    // `%20` 的输入会变成 `%2520`（与 JS `encodeURI` 的行为一致，测试锁定）。
    format!(
        "{repo_url}/{route}/{tag}/{}{}{}",
        encode_uri(&repository_path),
        local.query,
        local.fragment
    )
}

/// `splitLocalTarget` 的返回值：`fragment` 含 `#`，`query` 含 `?`。
struct LocalTarget<'a> {
    fragment: &'a str,
    path_part: &'a str,
    query: &'a str,
}

/// `splitLocalTarget(target)`：`#` 先于 `?` 切分（fragment 内不再看 `?`）。
fn split_local_target(target: &str) -> LocalTarget<'_> {
    // 先切 `#`：`docs/x.md#a?b` 里的 `?b` 属于 fragment，不再当作 query。
    let hash_index = target.find('#');
    let before_hash = hash_index.map_or(target, |index| &target[..index]);
    let fragment = hash_index.map_or("", |index| &target[index..]);
    let query_index = before_hash.find('?');

    match query_index {
        Some(index) => LocalTarget {
            fragment,
            path_part: &before_hash[..index],
            query: &before_hash[index..],
        },
        None => LocalTarget {
            fragment,
            path_part: before_hash,
            query: "",
        },
    }
}

/// `normalizePathPart(value)`：`\` → `/`。
fn normalize_path_part(value: &str) -> String {
    value.replace('\\', "/")
}

/// `resolveRepositoryPath(targetPath)`：POSIX normalize 后拒绝越出仓库根的路径。
fn resolve_repository_path(target_path: &str) -> Option<String> {
    // 反斜杠先归一成 `/`：链接测试表里覆盖了 `a\b\c.md` 这类 Windows 风格分隔符
    // （`matches_reference_link_rewriting_table` 锁定）。
    let normalized_target = normalize_path_part(target_path);
    // 以 `/` 开头的是“仓库根相对”（如 `/packages/agent/README.md`）；否则相对包目录
    // `packages/coding-agent` 解析。
    let joined = if let Some(absolute) = normalized_target.strip_prefix('/') {
        posix_normalize(absolute.trim_start_matches('/'))
    } else {
        posix_normalize(&format!("{CHANGELOG_LINK_BASE_PATH}/{normalized_target}"))
    };

    // 结果为空路径（`.`）或折叠后仍以 `..` 开头时放弃改写，调用方保留原始 target：
    // 前者没有可指向的文件，后者会爬出仓库根。
    if joined == "." || joined == ".." || joined.starts_with("../") {
        return None;
    }

    Some(joined)
}

/// `path.posix.normalize`（Node 语义：合并 `//`、解析 `.`/`..`、保留尾随 `/`、空串 → `.`）。
fn posix_normalize(input: &str) -> String {
    // 空串在 Node 的 `path.posix.normalize` 里返回 `"."`，调用方据此判断“路径为空”。
    if input.is_empty() {
        return ".".to_string();
    }
    let absolute = input.starts_with('/');
    let trailing_slash = input.ends_with('/');
    let mut stack: Vec<&str> = Vec::new();
    // 单遍栈式折叠：`.` 与空段丢弃，`..` 弹栈；相对路径在栈底保留多余的 `..`
    // （Node 也保留；测试 `[x](../../../README.md)` 因此保持原样）。保留的 `..` 正是
    // `resolve_repository_path` 事后要拒绝 `../` 前缀的原因。
    for part in input.split('/') {
        match part {
            "" | "." => {}
            ".." => match stack.last() {
                Some(top) if *top != ".." => {
                    stack.pop();
                }
                _ => {
                    if !absolute {
                        stack.push("..");
                    }
                }
            },
            other => stack.push(other),
        }
    }
    let mut normalized = String::new();
    if absolute {
        normalized.push('/');
    }
    normalized.push_str(&stack.join("/"));
    if normalized.is_empty() {
        normalized.push('.');
    }
    if trailing_slash && normalized != "/" && !normalized.ends_with('/') {
        normalized.push('/');
    }
    normalized
}

/// `isDirectoryTarget(originalPath, repositoryPath)`：原始路径以 `/` 结尾，或 basename 不含 `.`。
///
/// 这是 TS 的启发式判断，用于在 GitHub 的 `/tree/`（目录）与 `/blob/`（文件）之间二选一。
/// `original_path` 是去掉 query/fragment 后的链接目标；`repository_path` 是已归一化、
/// 可能带尾随 `/` 的仓库路径。已知误判：无扩展名的文件（如 `LICENSE`、`Makefile`）basename
/// 不含 `.`，会被改写成 `/tree/` 链接。
fn is_directory_target(original_path: &str, repository_path: &str) -> bool {
    if original_path.ends_with('/') {
        return true;
    }
    let basename = repository_path
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("");
    !basename.contains('.')
}

/// `encodeURI` 转义的 ASCII 字符集合（ECMAScript `encodeURI`）。
///
/// `percent-encoding` 对非 ASCII 字节一律编码，因此只需列出 `encodeURI` 会转义的 ASCII 字符：
/// 控制字符、空格、`"`、`<`、`>`、`` ` ``、`%`、`{`、`}`、`|`、`\`、`^`、`[`、`]`。
const ENCODE_URI_ESCAPED: &percent_encoding::AsciiSet = &percent_encoding::CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'<')
    .add(b'>')
    .add(b'`')
    .add(b'%')
    .add(b'{')
    .add(b'}')
    .add(b'|')
    .add(b'\\')
    .add(b'^')
    .add(b'[')
    .add(b']');

fn encode_uri(value: &str) -> String {
    percent_encoding::utf8_percent_encode(value, ENCODE_URI_ESCAPED).to_string()
}

/// `parseChangelog(changelogPath)`：扫描 `##` 标题行收集条目。
///
/// - 文件不存在 → 空数组；
/// - 标题行必须匹配 `##\s+\[?(\d+)\.(\d+)\.(\d+)\]?`，否则重置当前条目；
/// - `content` 是**包含标题行**的 `join("\n").trim()`（`spec/ca-cli.md` 附录 A.5）；
/// - 读取失败 → `tracing::error!("Warning: Could not parse changelog: …")` 后返回空数组。
///
/// 与 Node 的差异：`readFileSync(path, "utf-8")` 对非法 UTF-8 用 U+FFFD 替换而不是抛错，
/// 这里同样用 `String::from_utf8_lossy`；IO 错误文案按 Node 的 errno 格式复刻（`node_read_error_message`）。
pub fn parse_changelog(changelog_path: &Path) -> Vec<ChangelogEntry> {
    // 阶段 1：TS `existsSync` 快路径——文件不存在时直接返回空数组，不读文件也不记日志。
    // 目录也算“存在”，随后 `fs::read` 会给出 EISDIR 并走下面的错误分支。
    if !changelog_path.exists() {
        return Vec::new();
    }

    // 阶段 2：整文件读入。Node 的 `readFileSync(path, "utf-8")` 把非法字节解码为 U+FFFD，
    // 不抛错，因此这里必须用 `from_utf8_lossy` 而不是 `read_to_string`。
    let bytes = match std::fs::read(changelog_path) {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::error!(
                "Warning: Could not parse changelog: Error: {}",
                node_read_error_message(&error, changelog_path)
            );
            return Vec::new();
        }
    };
    let content = String::from_utf8_lossy(&bytes);

    // 阶段 3：两状态扫描。`current_version` 为 `None` 时还没遇到合法版本标题，所有行
    // （文件头 `# Changelog`、导言等）都丢弃；为 `Some` 时 `current_lines` 收集从标题行
    // 开始的原始行。
    //
    // 分隔条件是字面量 `"## "`，比版本正则更严格：`##\t1.0.0` 不算新段，若前面已有版本
    // 会被当作上一段的正文（`spec/ca-cli.md` 记录的 TS 行为）。
    let mut entries: Vec<ChangelogEntry> = Vec::new();
    let mut current_lines: Vec<&str> = Vec::new();
    let mut current_version: Option<(i64, i64, i64)> = None;

    for line in content.split('\n') {
        if line.starts_with("## ") {
            // 遇到新标题：先 flush 上一段（`!current_lines.is_empty()` 与 TS 的
            // `currentVersion && currentLines.length > 0` 对应），再判断新标题是否合法。
            if let Some(version) = current_version
                && !current_lines.is_empty()
            {
                entries.push(make_entry(version, &current_lines));
            }

            match CHANGELOG_VERSION_RE.captures(line) {
                Some(captures) => {
                    current_version = Some((
                        parse_changelog_number(&captures[1]),
                        parse_changelog_number(&captures[2]),
                        parse_changelog_number(&captures[3]),
                    ));
                    current_lines = vec![line];
                }
                None => {
                    // 非版本标题（如 `## Not a version`）：上一段已经 flush，这里清空状态，
                    // 该小节自己的正文不会被并入任何条目。
                    current_version = None;
                    current_lines.clear();
                }
            }
        } else if current_version.is_some() {
            // 标题行之后的普通行（含空行）原样收集；最终 content 的首尾空白由 `make_entry`
            // 的 `js_trim` 去掉。条目是否生成只取决于 `current_lines` 是否非空：标题行在
            // 分支之前已放进 `current_lines`，所以内容不会为空。
            current_lines.push(line);
        }
    }

    // 阶段 4：文件结束时 flush 最后一段——没有后续 `## ` 行来触发它。
    if let Some(version) = current_version
        && !current_lines.is_empty()
    {
        entries.push(make_entry(version, &current_lines));
    }

    entries
}

/// Node `fs.readFileSync` 的 IO 错误文案（`<CODE>: <description>, <syscall>`）。
///
/// `existsSync` 已经挡掉不存在的情况，实际可达的是目录（EISDIR）；权限/竞态错误按 Node 的
/// `open '<path>'` 形式补全。映射不到的错误退回 `std::io::Error` 的 `Display`。
fn node_read_error_message(error: &std::io::Error, path: &Path) -> String {
    match error.kind() {
        std::io::ErrorKind::IsADirectory => {
            "EISDIR: illegal operation on a directory, read".to_string()
        }
        std::io::ErrorKind::PermissionDenied => {
            format!("EACCES: permission denied, open '{}'", path.display())
        }
        std::io::ErrorKind::NotFound => {
            format!(
                "ENOENT: no such file or directory, open '{}'",
                path.display()
            )
        }
        _ => error.to_string(),
    }
}

fn parse_changelog_number(text: &str) -> i64 {
    // JS 的 parseInt 对超长数字串给出近似大整数；这里饱和到 i64::MAX（实际 CHANGELOG 不会出现）。
    text.parse::<i64>().unwrap_or(i64::MAX)
}

fn make_entry(version: (i64, i64, i64), lines: &[&str]) -> ChangelogEntry {
    ChangelogEntry {
        major: version.0,
        minor: version.1,
        patch: version.2,
        // `trim()` 用 JS 的空白集合（含 BOM 与行终止符），不是 Rust 的 `str::trim`。
        content: kiana_json::js_trim(&lines.join("\n")).to_string(),
    }
}

/// `compareVersions(v1, v2)`：依次比较 major/minor/patch，返回差值（TS 返回的是**差值**，
/// 不是规范化的 -1/0/1）。
///
/// 只比较数字三元组，没有 semver 的 prerelease/build 语义；需要 semver 规则时用
/// [`compare_package_versions`]。差值用 `saturating_sub` 计算：正常版本号下是精确差值，
/// 极端 `i64` 输入会在边界饱和（此时仍能给出正确的大小关系）。
pub fn compare_versions(v1: &ChangelogEntry, v2: &ChangelogEntry) -> i64 {
    if v1.major != v2.major {
        return v1.major.saturating_sub(v2.major);
    }
    if v1.minor != v2.minor {
        return v1.minor.saturating_sub(v2.minor);
    }
    v1.patch.saturating_sub(v2.patch)
}

/// `getNewEntries(entries, lastVersion)`：返回严格新于 `lastVersion` 的条目。
///
/// `lastVersion.split(".").map(Number)` 的语义按 JS 复刻：非数字/空段 → 0（`|| 0`），
/// 允许小数与 `Infinity`；比较用 f64（TS 的 number）。
///
/// 输出保持输入顺序、逐条 clone，不排序也不去重；`lastVersion` 多于 3 段时多余段被忽略
/// （`get_new_entries` 测试覆盖）。空字符串等价于 `0.0.0`。
pub fn get_new_entries(entries: &[ChangelogEntry], last_version: &str) -> Vec<ChangelogEntry> {
    // 每段先按 JS `Number()` 解析，再用 `|| 0` 把 NaN 与 ±0 归零；负数是真值，会保留。
    let parts: Vec<f64> = last_version
        .split('.')
        .map(|part| {
            let value = kiana_json::parse_js_number(part);
            if value.is_nan() || value == 0.0 {
                0.0
            } else {
                value
            }
        })
        .collect();
    let last = [
        parts.first().copied().unwrap_or(0.0),
        parts.get(1).copied().unwrap_or(0.0),
        parts.get(2).copied().unwrap_or(0.0),
    ];

    entries
        .iter()
        .filter(|entry| compare_entry_with_last(entry, last) > 0.0)
        .cloned()
        .collect()
}

fn compare_entry_with_last(entry: &ChangelogEntry, last: [f64; 3]) -> f64 {
    let major = entry.major as f64;
    if major != last[0] {
        return major - last[0];
    }
    let minor = entry.minor as f64;
    if minor != last[1] {
        return minor - last[1];
    }
    entry.patch as f64 - last[2]
}

// TS 的 `export { getChangelogPath } from "../config.ts"`。
/// 重新导出 [`crate::config::get_changelog_path`]，让下游可以沿用
/// `kiana_core::changelog::get_changelog_path` 这一 TS 对应的导入路径。
pub use crate::config::get_changelog_path;

// ===========================================================================
// version-check
// ===========================================================================

/// `DEFAULT_VERSION_CHECK_TIMEOUT_MS`。
///
/// 10 秒是**总预算**（一次调用内的所有重试共享），同时约束请求与响应体读取；
/// [`VersionCheckOptions::timeout_ms`] 为 `None` 时使用它。
const DEFAULT_VERSION_CHECK_TIMEOUT_MS: u64 = 10_000;

/// 测试用的 URL 覆盖（`setLatestVersionUrlForTests`；TS 用 `vi.stubGlobal("fetch")`）。
///
/// 进程级状态：`Some` 时优先于 `KIANA_API_BASE_URL` 派生的 URL。测试必须串行修改
/// （`tests/version_check.rs` 用全局锁），否则会互相看到对方的覆盖值。中毒的锁用
/// `into_inner` 取回，避免一个测试 panic 后其余测试全部失败。
static LATEST_VERSION_URL_OVERRIDE: Mutex<Option<String>> = Mutex::new(None);

/// 仅供测试：覆盖版本检查端点（`None` 恢复由 `KIANA_API_BASE_URL` 派生）。
///
/// TS 的测试替换全局 `fetch`；Rust 侧没有全局 fetch，改为覆盖 URL 以指向 wiremock。
#[doc(hidden)]
pub fn set_latest_version_url_for_tests(url: Option<String>) {
    let mut guard = LATEST_VERSION_URL_OVERRIDE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *guard = url;
}

/// 版本检查端点：测试覆盖优先，否则是 `KIANA_API_BASE_URL/api/latest-version`。
///
/// 每次调用都重新读取覆盖与环境变量，不做缓存，因此测试可以在同一进程内切换端点。
/// 返回 `None` 表示两者都未配置：调用方把它当作“没有可用信息”，而不是错误。
fn latest_version_url() -> Option<String> {
    LATEST_VERSION_URL_OVERRIDE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
        .or_else(|| crate::config::get_api_url("api/latest-version"))
}

/// 是否配置了版本检查端点（测试覆盖或 `KIANA_API_BASE_URL`）。
pub fn is_version_check_configured() -> bool {
    latest_version_url().is_some()
}

/// `getLatestKianaRelease` 的返回值（TS: `version-check.ts#LatestKianaRelease`）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LatestKianaRelease {
    /// 最新版本（trim 后）。
    pub version: String,
    /// 新包名（可选；非空字符串才出现）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package_name: Option<String>,
    /// 升级说明（可选；非空字符串才出现）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// `getLatestKianaRelease` 的选项（TS: `{ timeoutMs?, retry? }`）。
///
/// 两者都是可选字段：`timeout_ms = None` 用 10 秒总预算，`Some(0)` 表示**不设本函数的
/// 总预算**；请求仍走共享 client，它自带的 10s 连接超时与 `read_timeout`（默认 300s）
/// 不受 `Some(0)` 影响。`retry = Some(true)` 才开启 2 次额外尝试（自动检查用默认值，
/// 即不重试）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VersionCheckOptions {
    /// 总预算（毫秒）；缺省 10_000。
    /// `Some(0)` 表示不设预算（与 `fetch_with_retry` 的 `timeout_ms = 0` 语义一致）。
    pub timeout_ms: Option<u64>,
    /// `true` 时 `maxRetries = 2`，否则 `0`。
    pub retry: Option<bool>,
}

/// `formatVersionCheckError(error)`：把 `error.cause` 里的 errno 细节拼到根消息后面。
///
/// - cause 链上有 `code` 字符串 → `{root} ({code1}, {code2})`（去重、保持顺序）；
/// - 否则第一个有非空 message 的 cause → `{root} (cause: {message})`；
/// - 否则 `{root}`。
///
/// 非 `Transport` 变体（`Timeout`/`Aborted`）没有 cause，直接返回其 `Display`
/// （测试断言 `The operation was aborted due to timeout` 等文案）。
///
/// `reqwest` 错误链的 `code` 由 [`FetchError::from_reqwest`](crate::management_http::FetchError)
/// 从最深的 `std::io::Error` 反推（`ECONNREFUSED`/`ENOTFOUND`/`ETIMEDOUT` 等），因此连接被拒
/// 会格式化出 `fetch failed (ECONNREFUSED)`（稳定错误文案，改动需同步测试）。
pub fn format_version_check_error(error: &FetchError) -> String {
    let root_message = error.to_string();
    let causes: &[crate::management_http::FetchErrorCause] = match error {
        FetchError::Transport {
            cause: Some(cause), ..
        } => cause,
        _ => &[],
    };

    let mut codes: Vec<&str> = Vec::new();
    for cause in causes {
        if let Some(code) = &cause.code
            && !codes.contains(&code.as_str())
        {
            codes.push(code);
        }
    }
    if !codes.is_empty() {
        return format!("{root_message} ({})", codes.join(", "));
    }

    if let Some(message) = causes
        .iter()
        .map(|cause| cause.message.as_str())
        .find(|message| !message.is_empty())
    {
        return format!("{root_message} (cause: {message})");
    }

    root_message
}

/// node-semver 的 `MAX_LENGTH`（`SemVer` 构造函数在正则匹配前检查 `version.length`）。
const MAX_PACKAGE_VERSION_LENGTH: usize = 256;

/// node-semver 的 `Number.MAX_SAFE_INTEGER`（数字段与 prerelease 数值化的阈值）。
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// `comparePackageVersions(left, right)`：两边都是合法 semver 时返回 -1/0/1，否则 `None`。
///
/// 用 `semver` crate 复刻 node-semver 的 `valid()`：先 `trim()`，长度不超过
/// `MAX_PACKAGE_VERSION_LENGTH`（`valid()` 对超长输入返回 `null`），再接受**单个小写前导
/// `v`**（`valid("v1.2.3") === "1.2.3"`，`"=1.2.3"` 与 `"V1.2.3"` 无效）。
///
/// 比较规则同样复刻 node-semver 的 `compare`：数字段精确比较；prerelease 的数字标识符按
/// JS `Number`（f64）比较，因此大于 2^53 的标识符会丢精度（`9007199254740993` 与
/// `9007199254740992` 相等），与 `semver` crate 的精确比较不同；build metadata 忽略。
///
/// 与 [`compare_versions`] 的分工：那个只比较 CHANGELOG 条目的数字三元组、返回差值；
/// 这个做完整 semver 校验，任一侧非法就返回 `None`，合法时返回 -1/0/1。
pub fn compare_package_versions(left_version: &str, right_version: &str) -> Option<i32> {
    let left = parse_valid_package_version(left_version)?;
    let right = parse_valid_package_version(right_version)?;
    Some(node_semver_compare(&left, &right))
}

fn parse_valid_package_version(version: &str) -> Option<semver::Version> {
    let trimmed = kiana_json::js_trim(version);
    // `version.length > MAX_LENGTH`（JS 的 UTF-16 长度）在正则匹配前抛出 → `valid()` 为 null。
    if trimmed.encode_utf16().count() > MAX_PACKAGE_VERSION_LENGTH {
        return None;
    }
    let trimmed = trimmed.strip_prefix('v').unwrap_or(trimmed);
    let parsed = semver::Version::parse(trimmed).ok()?;
    // node-semver 拒绝大于 `Number.MAX_SAFE_INTEGER` 的数字段（`+m[1] > MAX_SAFE_INTEGER`
    // 时抛错）；`semver` crate 用 u64，能表示到 u64::MAX，需要显式收窄。
    if parsed.major > MAX_SAFE_INTEGER
        || parsed.minor > MAX_SAFE_INTEGER
        || parsed.patch > MAX_SAFE_INTEGER
    {
        return None;
    }
    Some(parsed)
}

/// node-semver `SemVer#compare`：`compareMain || comparePre`（忽略 build）。
fn node_semver_compare(left: &semver::Version, right: &semver::Version) -> i32 {
    let main = left
        .major
        .cmp(&right.major)
        .then_with(|| left.minor.cmp(&right.minor))
        .then_with(|| left.patch.cmp(&right.patch));
    let ordering = match main {
        std::cmp::Ordering::Equal => compare_prerelease(left.pre.as_str(), right.pre.as_str()),
        main => main,
    };
    match ordering {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    }
}

/// node-semver `SemVer#comparePre`：无 prerelease 的版本更大；逐段比较，
/// 短的一方更小（`a === b` 时继续）。
fn compare_prerelease(left: &str, right: &str) -> std::cmp::Ordering {
    if left.is_empty() && right.is_empty() {
        return std::cmp::Ordering::Equal;
    }
    if left.is_empty() {
        return std::cmp::Ordering::Greater;
    }
    if right.is_empty() {
        return std::cmp::Ordering::Less;
    }
    let mut left_identifiers = left.split('.');
    let mut right_identifiers = right.split('.');
    loop {
        match (left_identifiers.next(), right_identifiers.next()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (Some(_), None) => return std::cmp::Ordering::Greater,
            (None, Some(_)) => return std::cmp::Ordering::Less,
            (Some(left), Some(right)) => {
                if left == right {
                    continue;
                }
                return compare_identifiers(left, right);
            }
        }
    }
}

/// node-semver `compareIdentifiers`：数字标识符数值比较（`+a` → f64，丢失 2^53 以上精度），
/// 数字标识符小于字母数字标识符，其余按字符串（ASCII）比较。
fn compare_identifiers(left: &str, right: &str) -> std::cmp::Ordering {
    let left_numeric = is_numeric_identifier(left);
    let right_numeric = is_numeric_identifier(right);
    if left_numeric && right_numeric {
        // `+a === +b ? 0 : +a < +b ? -1 : 1`；f64 溢出与 JS 一样是 Infinity。
        let left_number = left.parse::<f64>().unwrap_or(f64::NAN);
        let right_number = right.parse::<f64>().unwrap_or(f64::NAN);
        if left_number == right_number {
            std::cmp::Ordering::Equal
        } else if left_number < right_number {
            std::cmp::Ordering::Less
        } else {
            std::cmp::Ordering::Greater
        }
    } else if left_numeric {
        std::cmp::Ordering::Less
    } else if right_numeric {
        std::cmp::Ordering::Greater
    } else {
        left.cmp(right)
    }
}

/// `/^[0-9]+$/`。
fn is_numeric_identifier(identifier: &str) -> bool {
    !identifier.is_empty() && identifier.bytes().all(|byte| byte.is_ascii_digit())
}

/// `isNewerPackageVersion(candidate, current)`：可比较时用 semver，否则退回 trim 后字符串不等。
///
/// 退避分支的后果：任一侧不是合法 semver 时，只要两边 trim 后文本不同就返回 `true`
/// （例如 `current = "dev"`），这不是大小比较而是“文本有差异”；调用方需要严格判断时
/// 应先用 [`compare_package_versions`] 检查合法性。
pub fn is_newer_package_version(candidate_version: &str, current_version: &str) -> bool {
    match compare_package_versions(candidate_version, current_version) {
        Some(comparison) => comparison > 0,
        None => kiana_json::js_trim(candidate_version) != kiana_json::js_trim(current_version),
    }
}

/// `getLatestKianaRelease(currentVersion, options)`。
///
/// 返回 `Ok(None)` 表示“没有可用信息”（离线、`!response.ok`、JSON 缺字段）；
/// 传输失败（`fetch failed`）、响应体读取失败（`terminated`）与 JSON 解析失败
/// （`JSON.parse` 的 V8 `SyntaxError` 文案，经 [`kiana_json::js_parse::parse`]）返回 `Err`
/// （TS 抛错）。
///
/// **与规划稿公开面的差异**：规划稿把返回类型写成 `Option<LatestKianaRelease>`；为保留 TS 的
/// 抛错语义（`spec/ca-cli.md` 的自更新流程要求 `catch` 后调用
/// [`format_version_check_error`]），这里返回 `Result<Option<…>, FetchError>`。
///
/// `current_version` 只用于构造 `User-Agent`，不参与“是否有新版本”的判断；调用方
/// （如 `kiana-coding-agent` 的自更新）自行用 [`is_newer_package_version`] 比较。
pub async fn get_latest_kiana_release(
    current_version: &str,
    options: &VersionCheckOptions,
) -> Result<Option<LatestKianaRelease>, FetchError> {
    // `if (process.env.KIANA_OFFLINE) return undefined;` —— **任意非空值**（含 "0"）都禁用。
    if env_is_truthy("KIANA_OFFLINE") {
        return Ok(None);
    }

    // 未配置端点不是错误：返回 `Ok(None)`，调用方用 [`is_version_check_configured`]
    // 区分“没配置”与“配置了但查不到”。
    let Some(url) = latest_version_url() else {
        return Ok(None);
    };
    let user_agent = get_kiana_user_agent(current_version);
    let client = crate::http_dispatcher::shared_http_client();
    // 请求构造器：`fetch_with_retry` 每次尝试前都会调用它。`reqwest` 的 `RequestBuilder`
    // 是一次性的，所以闭包内重新 `get` 并重新附 header，不能复用同一个 builder。
    let request = {
        let client = Arc::clone(&client);
        let url = url.clone();
        move || {
            client
                .get(&url)
                .header("User-Agent", user_agent.clone())
                .header("accept", "application/json")
        }
    };
    // 总预算：`Some(0)` 表示不设总预算（与 `fetch_with_retry` 对 `timeout_ms = 0` 的解释一致），
    // 此时不计算 deadline，body 读取也不加 `timeout_at`；共享 client 自身的超时配置不受影响
    // （连接超时 10s、默认 `read_timeout` 300s）。
    let timeout_ms = options
        .timeout_ms
        .unwrap_or(DEFAULT_VERSION_CHECK_TIMEOUT_MS);
    let deadline = if timeout_ms > 0 {
        tokio::time::Instant::now().checked_add(Duration::from_millis(timeout_ms))
    } else {
        None
    };
    // 只有显式 `retry: Some(true)` 才给 2 次额外尝试；自动检查（默认选项）不重试。
    let response = fetch_with_retry(
        request,
        FetchRetryOptions {
            max_retries: Some(if options.retry.unwrap_or(false) { 2 } else { 0 }),
            timeout_ms: Some(timeout_ms),
            ..FetchRetryOptions::default()
        },
    )
    .await?;

    // HTTP 非 2xx 按“没有可用信息”处理（TS 的 `!response.ok`），不当作异常；测试用 404 锁定。
    if !response.status().is_success() {
        return Ok(None);
    }

    // TS 的 `AbortSignal.timeout` 覆盖 body 读取；`fetch_with_retry` 把同一 deadline 附到了
    // 请求上（`RequestBuilder::timeout`），这里再用 `timeout_at` 双保险。body 读取失败时：
    // - 总预算到期 → TS 的 AbortError（`The operation was aborted due to timeout`）；
    // - 传输截断 → `TypeError("terminated", { cause })`（`response.json()`）。
    let body = match deadline {
        Some(deadline) => match tokio::time::timeout_at(deadline, response.text()).await {
            Ok(Ok(body)) => body,
            Ok(Err(error)) if error.is_timeout() => return Err(FetchError::Timeout),
            Ok(Err(error)) => return Err(crate::management_http::terminated_error(&error)),
            Err(_) => return Err(FetchError::Timeout),
        },
        None => response.text().await.map_err(|error| {
            if error.is_timeout() {
                FetchError::Timeout
            } else {
                crate::management_http::terminated_error(&error)
            }
        })?,
    };

    // `JSON.parse` 的 V8 语义（错误文案与接受度，见 `ARCHITECTURE.md` §4.3）。
    let data: serde_json::Value = kiana_json::js_parse::parse(&body)
        .map_err(|error| FetchError::transport(error.message, None))?;
    // `data.version` 在 `null` 上抛 TypeError（其它原始值只得到 undefined）。
    if data.is_null() {
        return Err(FetchError::transport(
            "Cannot read properties of null (reading 'version')",
            None,
        ));
    }

    // 只接受字符串类型的 `version`；缺失、数字、`null` 字段与 trim 后为空都返回 `Ok(None)`
    // （测试覆盖 `{version: 42}`、空串、纯空白等）。
    let Some(version) = data.get("version").and_then(|value| value.as_str()) else {
        return Ok(None);
    };
    let version = kiana_json::js_trim(version);
    if version.is_empty() {
        return Ok(None);
    }

    // `packageName`/`note` 只在字符串且 trim 后非空时保留，否则为 `None`。
    Ok(Some(LatestKianaRelease {
        version: version.to_string(),
        package_name: trimmed_nonempty_string(data.get("packageName")),
        note: trimmed_nonempty_string(data.get("note")),
    }))
}

fn trimmed_nonempty_string(value: Option<&serde_json::Value>) -> Option<String> {
    let text = value?.as_str()?;
    let trimmed = kiana_json::js_trim(text);
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// `getLatestKianaVersion(currentVersion, options)`：只取版本字符串。
pub async fn get_latest_kiana_version(
    current_version: &str,
    options: &VersionCheckOptions,
) -> Result<Option<String>, FetchError> {
    Ok(get_latest_kiana_release(current_version, options)
        .await?
        .map(|release| release.version))
}

/// `checkForNewKianaVersion(currentVersion)`：`KIANA_SKIP_VERSION_CHECK` 或任何异常 → `None`。
///
/// 失败模式全部归并为 `None`：跳过开关非空、离线、未配置端点、传输/JSON 解析错误、
/// 以及“版本不比当前新”都无法由返回值区分。使用默认选项（不重试），测试断言这种调用
/// 最多只发一次请求，适合启动时的 fire-and-forget 任务。
pub async fn check_for_new_kiana_version(current_version: &str) -> Option<LatestKianaRelease> {
    if env_is_truthy("KIANA_SKIP_VERSION_CHECK") {
        return None;
    }
    match get_latest_kiana_release(current_version, &VersionCheckOptions::default()).await {
        Ok(Some(release)) if is_newer_package_version(&release.version, current_version) => {
            Some(release)
        }
        _ => None,
    }
}

/// JS 真值判定：环境变量存在且**非空**即真（`process.env.X` 是字符串，空串为假）。
fn env_is_truthy(name: &str) -> bool {
    std::env::var_os(name)
        .map(|value| !value.is_empty())
        .unwrap_or(false)
}

// ===========================================================================
// kiana-user-agent
// ===========================================================================

/// `getKianaUserAgent(version)`：kiana 自己的 `User-Agent`（与 kiana-ai 发给 provider 的 detailed 形式一致）。
///
/// 逐字委托给 `kiana_ai::utils::identity::format_user_agent`：
/// `kiana/<version> (<platform>; <bun|node>/<runtime version>; <arch>)`；宿主运行时探测不到时
/// 是 `kiana/<version> (browser)`（Rust 进程没有 `process.versions`，见 kiana-ai 的实现说明）。
///
/// `version` 原样嵌入，不做校验也不补 `v` 前缀；调用方应传应用自身版本号
/// （`kiana_core::config::VERSION`，`kiana-coding-agent`、`kiana-config` 等调用方共用）。
pub fn get_kiana_user_agent(version: &str) -> String {
    kiana_ai::utils::identity::format_user_agent(Some(&kiana_ai::ClientIdentity {
        user_agent: Some(kiana_ai::UserAgentMode::Detailed(
            kiana_ai::UserAgentModeDetailed::Detailed,
        )),
        attribution: None,
        version: Some(version.to_string()),
    }))
}

// ===========================================================================
// wsl
// ===========================================================================

/// `(?i)microsoft|wsl`（`/proc/version` 检测）。
static WSL_RELEASE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i-u)microsoft|wsl").expect("wsl release regex is valid"));

/// `isWSL(env = process.env)`：Windows Subsystem for Linux 检测（Windows 可执行文件可经
/// interop 访问）。
///
/// 1. `WSL_DISTRO_NAME` 或 `WSLENV` **非空** → `true`；
/// 2. 否则读 `/proc/version` 并匹配 `(?i)microsoft|wsl`；
/// 3. 读取失败 → `false`。
///
/// `env: Some(&[(OsString, OsString)])` 用于测试注入；`None` 时读真实进程环境。
/// 注入模式下只读给定的快照，不会回退到真实环境；变量名按 `OsStr` 精确比较，值必须非空
/// （`""` 视为未设置）。
/// 这是全 crate 唯一的 WSL 实现（`clipboard.rs`/`image_process.rs` 也调用它）。
pub fn is_wsl(env: Option<&[(OsString, OsString)]>) -> bool {
    let has_env = |key: &str| match env {
        Some(entries) => entries
            .iter()
            .any(|(name, value)| name.as_os_str() == key && !value.is_empty()),
        None => std::env::var_os(key)
            .map(|value| !value.is_empty())
            .unwrap_or(false),
    };

    if has_env("WSL_DISTRO_NAME") || has_env("WSLENV") {
        return true;
    }

    match std::fs::read_to_string("/proc/version") {
        Ok(release) => WSL_RELEASE_RE.is_match(&release),
        Err(_) => false,
    }
}

// ===========================================================================
// deprecation
// ===========================================================================

/// `const emittedDeprecationWarnings = new Set<string>()`。
///
/// 进程级去重集合，键是消息全文。测试需要串行调用 [`clear_deprecation_warnings_for_tests`]
/// 与 [`warn_deprecation`]（`tests/deprecation.rs` 用全局锁），否则用例之间会互相抑制。
static EMITTED_DEPRECATION_WARNINGS: LazyLock<Mutex<HashSet<String>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

/// `warnDeprecation(message)`：同一条消息只警告一次。
///
/// TS 用 `console.warn(chalk.yellow(...))`；Rust 按 `ARCHITECTURE.md` §4.4 走 `tracing::warn!`，
/// 消息文本固定为 `Deprecation warning: {message}`，是本模块的稳定契约（改动需同步测试）；
/// 颜色由 TUI subscriber 决定。
///
/// 消息按完整字符串精确比较，不做大小写或前缀归一。锁只覆盖“查重 + 插入”这一段，
/// `tracing::warn!` 在锁外执行：并发调用同一消息时只有一个线程会打印，不同消息之间
/// 的打印顺序不保证。
pub fn warn_deprecation(message: &str) {
    // 作用域块保证锁在 `tracing::warn!` 之前释放；只把首次出现记入集合。
    {
        let mut emitted = EMITTED_DEPRECATION_WARNINGS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if emitted.contains(message) {
            return;
        }
        emitted.insert(message.to_string());
    }
    tracing::warn!("Deprecation warning: {message}");
}

/// `clearDeprecationWarningsForTests()`：清空“已警告”集合（仅测试用）。
///
/// 生产代码不应调用：清空后同一条弃用告警会再次打印。
pub fn clear_deprecation_warnings_for_tests() {
    let mut emitted = EMITTED_DEPRECATION_WARNINGS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    emitted.clear();
}

// ===========================================================================
// windows-self-update
// ===========================================================================

/// `QUARANTINE_DIR_NAME`。
const QUARANTINE_DIR_NAME: &str = ".kiana-native-quarantine";

/// `getQuarantineRoot(packageDir)`：向上找名为 `node_modules` 的目录（大小写不敏感）。
///
/// 从解析后的 `package_dir` **自身**开始逐级向上，第一个 basename（Unicode 小写后）等于
/// `node_modules` 的目录即命中，返回其下的 `.kiana-native-quarantine`；一直走到文件系统根
/// 仍未命中则返回 `None`。比较用 `to_string_lossy()`，非 UTF-8 文件名会先被替换成 U+FFFD。
///
/// TS 的 `resolve(packageDir)` 是 Node `path.resolve` 的**词法**解析：相对路径按 cwd
/// 转绝对，并消掉 `.`/`..`（不访问文件系统）。`std::path::absolute` 不会处理 `..`，
/// 会在 `node_modules/../y` 这类输入上找到不同的祖先，因此这里复用 `paths::resolve_path`
/// （Node `path.resolve` 的逐字移植，关闭与解析无关的 `~` 展开）。
fn get_quarantine_root(package_dir: &Path) -> Option<PathBuf> {
    let mut current = resolve_package_dir(package_dir);
    loop {
        if let Some(name) = current.file_name()
            && name.to_string_lossy().to_lowercase() == "node_modules"
        {
            return Some(current.join(QUARANTINE_DIR_NAME));
        }
        let parent = current.parent()?;
        if parent == current {
            return None;
        }
        current = parent.to_path_buf();
    }
}

/// `path.resolve(packageDir)`（Node 词法语义；`~` 不展开）。
///
/// 当前目录取不到时退回 `"."`（此时相对路径的解析结果以 `.` 为基准，而不是报错）。
/// 解析过程只做字符串处理，不访问文件系统。
fn resolve_package_dir(package_dir: &Path) -> PathBuf {
    let cwd = std::env::current_dir()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|_| ".".to_string());
    let options = crate::paths::PathInputOptions {
        expand_tilde: false,
        ..crate::paths::PathInputOptions::default()
    };
    PathBuf::from(crate::paths::resolve_path(
        &package_dir.to_string_lossy(),
        &cwd,
        &options,
    ))
}

/// `cleanupWindowsSelfUpdateQuarantine(packageDir)`：删除 `<node_modules>/.kiana-native-quarantine`。
///
/// TS 用 `rmSync(root, {recursive:true, force:true})` 并忽略异常。`rmSync` 以 `recursive`
/// 删除时也接受同名的普通文件/符号链接，因此这里先看元数据：目录 → `remove_dir_all`，
/// 其它（普通文件、符号链接）→ `remove_file`。错误一律忽略（不存在也算成功）。全平台
/// 可调用（TS 在 `main` 里仅 win32 调用）。
///
/// 相对路径的 `package_dir` 先按当前工作目录解析；`symlink_metadata` 失败（不存在、
/// 无权限等）时静默返回，不区分原因。
pub fn cleanup_windows_self_update_quarantine(package_dir: &Path) {
    let Some(quarantine_root) = get_quarantine_root(package_dir) else {
        return;
    };
    match std::fs::symlink_metadata(&quarantine_root) {
        Ok(metadata) if metadata.is_dir() => {
            let _ = std::fs::remove_dir_all(&quarantine_root);
        }
        Ok(_) => {
            // 符号链接用 `remove_file` 删除链接本身（`fs.rmSync` 也不会跟随链接）。
            let _ = std::fs::remove_file(&quarantine_root);
        }
        Err(_) => {}
    }
}

/// `quarantineWindowsNativeDependencies(packageDir)`：把“已加载的原生共享对象”搬进隔离区。
///
/// **Rust 侧全平台 no-op**：TS 的数据源是 `process.report.getReport().sharedObjects`
/// （Node 的 native addon 列表）；Rust 进程没有 JS native addon，`loaded_files` 恒为空，
/// TS 在该情况下也会提前返回。`ARCHITECTURE.md` §8 第 26 条要求 Windows 分支照常编译，
/// 因此保留入口与文档，不做任何文件操作（也不引入 `windows-sys` 枚举进程模块）。
pub fn quarantine_windows_native_dependencies(_package_dir: &Path) {
    // 与 TS 的 `if (loadedFiles.length === 0) return;` 等价。
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn formats_node_read_errors() {
        // Node 26：`EISDIR: illegal operation on a directory, read`
        let error = std::io::Error::from_raw_os_error(21);
        assert_eq!(
            node_read_error_message(&error, Path::new("/tmp/CHANGELOG.md")),
            "EISDIR: illegal operation on a directory, read"
        );

        let error = std::io::Error::from_raw_os_error(13);
        assert_eq!(
            node_read_error_message(&error, Path::new("/tmp/CHANGELOG.md")),
            "EACCES: permission denied, open '/tmp/CHANGELOG.md'"
        );

        let error = std::io::Error::from_raw_os_error(2);
        assert_eq!(
            node_read_error_message(&error, Path::new("/tmp/CHANGELOG.md")),
            "ENOENT: no such file or directory, open '/tmp/CHANGELOG.md'"
        );
    }
}
