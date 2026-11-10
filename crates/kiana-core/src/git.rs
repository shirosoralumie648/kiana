//! utils/git.ts。
//!
//! TS: `packages/coding-agent/src/utils/git.ts`（226 行）。
//!
//! `hosted-git-info` 不能直接移植，这里按其 9.0.3 的 `fromUrl` / `parseUrl` / `hosts`
//! 逐条复刻（`spec/ca-cli.md` §2.21.36 / §8.2 第 12 条）：
//!
//! - 支持 `github.com`、`gitlab.com`、`bitbucket.org`、`gist.github.com`、`git.sr.ht`
//!   以及 `github:` / `gitlab:` / `bitbucket:` / `gist:` / `sourcehut:` 简写；
//! - `correctProtocol` / `correctUrl`（scp-like `git@host:path` 的修补）逐字移植；
//! - 每个 host 的 `extract`（含 `.git` 后缀、`tree` 路径、`#committish`）逐字移植；
//! - `info.user`/`info.project`/`info.committish` 的拼接与 `null` → `"null"` 的怪异行为保留
//!   （例如 `https://gist.github.com/abc123` 的 `path` 是 `"null/abc123"`；
//!   `https://github.com/user/repo/tree` 的 `ref` 是字符串 `"undefined"`）。
//!
//! ## 门禁
//!
//! 无 `git:` 前缀时只接受 `^(https?|ssh|git)://`（大小写不敏感）；有前缀时接受所有历史简写。
//! `buildGitSource` 的安全检查（`decodeURIComponent` 失败、NUL、反斜杠、前导 `/`、
//! `..` 路径段）全部保留（安全边界不可放宽）。
//!
//! ## 已知偏差
//!
//! - JS 的 `decodeURIComponent` 作用于 UTF-16 字符串；Rust 用「按字符编码为 UTF-8、
//!   解 `%XX`、整体校验 UTF-8」的等价实现（合法 UTF-8 输入下结果相同）。
//! - `URL` 解析用 `url` crate（同一 WHATWG 规范）。
//!
//! ## 这个模块为什么存在
//!
//! 用户可以在包配置里写 git 源（`git:...`、`https://...`、scp 形式等）。下游
//! `kiana-resources` 的 `parse_source` 先用 `paths::is_local_path` 排除本地路径，
//! 再调用 [`parse_git_url`]；返回 `None` 时会把原字符串当作本地路径继续处理。
//! `kiana-interactive` 只读取结果的 `host`/`path`/`ref`，用于紧凑来源标签与
//! 自动补全的来源标记。
//! 因此本模块的“接受/拒绝”边界会直接影响包安装的目标路径与 `git clone` 命令。
//!
//! ## 解析流程
//!
//! 1. `js_trim` 去掉 JS 意义上的空白；有 `git:` 前缀则去掉后再 trim 一次。
//! 2. 无 `git:` 前缀时用 `PROTOCOL_PREFIX_RE` 做门禁，只放行 `http(s)://`、`ssh://`、
//!    `git://`；有前缀时接受所有历史简写。
//! 3. `split_ref` 从路径中拆出 `@<ref>`（三种输入形态见 `split_ref` 的注释）。
//! 4. 先尝试 hosted 候选：`repo#ref`（若有 ref）与原串。命中域名表时用对应
//!    `extract` 得到 user/project/committish；命中 `github:`/`gitlab:` 等简写
//!    scheme 时直接从 pathname 拆 user/project，不调用 `extract`。若 ref 已拆出且
//!    `project` 含 `@`，跳过该候选（避免把 ref 误当成 project 的一部分）。
//! 5. 再尝试补 `https://` 的两个候选，规则同上。
//! 6. 最后 `parse_generic_git_url` 处理通用 scp/带 scheme/裸 `host/path` 形式。
//! 7. 每个成功出口都要经过 `build_git_source`：路径至少 2 段；host/路径不得含 NUL
//!    或反斜杠，路径不得以 `/` 开头，任一段不得为 `..`，host 另不许含 `/`
//!    （原串与解码后的串各检查一次）。
//!
//! ## 注意事项
//!
//! - 纯函数：不访问网络、文件系统或环境变量，相同输入总是相同结果。
//! - `host` 在命中 host 表时是规范域名（`www.` 会被去掉），通用 URL 则是解析出的
//!   hostname；两者都不一定是输入里原样写的主机名。
//! - `repo` 可能保留 `#committish`（语料 `https://github.com/user/repo#v1.0.0`），
//!   而 `@<ref>` 形式会被 `split_ref` 从 repo 中剥离。
//! - hosted 候选中的 percent 解码失败只会跳过该候选（解码错误被吞掉，
//!   该候选视为不匹配）；`parse_git_url` 仍会尝试后续候选与通用解析。
//!   通用解析不解码 `repo` 与 `ref`，因此可能返回其中保留非法 `%` 序列的
//!   `GitSource`，而不是一律拒绝。
//! - `tests/fixtures/git_url_corpus.json` 的 123 条输入/输出由 TS 实现生成，
//!   其中包含 `"null"`、`"undefined"` 等反直觉但必须保留的值，不要“顺手修正”。

use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};
use url::Url;

/// TS: `export type GitSource`。
///
/// 这是 [`parse_git_url`] 的返回值，也是下游包管理器与 UI 共享的 git 源描述。
/// 序列化后的字段名是当前稳定契约（`type`/`repo`/`host`/`path`/`ref`/`pinned`），
/// 黄金语料 `tests/fixtures/git_url_corpus.json` 按这个形状逐字比较。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitSource {
    /// 恒为 `"git"`（TS 的 `type: "git"`）。
    #[serde(rename = "type")]
    pub source_type: String,
    /// 克隆 URL（总是可 `git clone`，不含 ref 后缀）。
    ///
    /// 注意 `#committish` 形式会保留在 `repo` 里（语料
    /// `https://github.com/user/repo#v1.0.0`）；只有 `@<ref>` 会被 `split_ref` 剥掉。
    pub repo: String,
    /// Git host 域名（如 `"github.com"`）。
    ///
    /// 命中 host 表时取规范 domain（`www.` 前缀会先去掉）；不在 host 表里的通用 URL
    /// 则取解析出的 hostname。它不一定是输入串里原样写的主机名。
    pub host: String,
    /// 仓库路径（如 `"user/repo"`）。
    ///
    /// 来源随解析分支不同：
    /// - 命中域名表：`extract` 得到的 user 与 project 用 `/` 拼接；
    /// - 命中 `github:`/`gitlab:` 等简写 scheme：直接从 pathname 拆出 user 与
    ///   project 再用 `/` 拼接，不经过 `extract`；
    /// - 通用分支：scp-like 形式的 `SCP_LIKE_RE` 捕获、带 scheme 的 URL path
    ///   （去前导 `/`），或裸 `host/path` 的 path 部分。
    ///
    /// user 缺失时会出现字面量 `"null"`（如 gist 单段 URL 的 `"null/abc123"`）。
    /// `build_git_source` 保证它至少包含 2 个 `/` 分隔段。
    pub path: String,
    /// 分支/标签/提交（如果指定）。
    ///
    /// 可能来自 `@<ref>`、`#<committish>` 或 GitHub 的 `/tree/<ref>` 路径；GitHub
    /// `/tree`（没有第 4 段）会得到字面量 `"undefined"`，见模块文档的已知怪癖。
    #[serde(rename = "ref", default, skip_serializing_if = "Option::is_none")]
    pub r#ref: Option<String>,
    /// ref 是否被显式指定（指定后包不会被自动更新）。
    ///
    /// 由 `build_git_source` 计算为“`ref` 存在且非空”；`kiana-resources` 的更新
    /// 检查与临时目录刷新据此跳过自动更新。
    pub pinned: bool,
}

/// scp-like `git@host:path` —— 对应正则 `/^git@([^:]+):(.+)$/`：
/// `[^:]` 允许换行（JS 的否定字符类包含行终止符），而 `(.+)$` 不允许
/// `\r`/`\n`/U+2028/U+2029（JS 的 `.` 排除行终止符），`$` 只匹配输入末尾。
static SCP_LIKE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^git@([^:]+):([^\r\n\u{2028}\u{2029}]+)$").expect("valid scp-like regex")
});

/// `^(https?|ssh|git)://`（大小写不敏感）。
static PROTOCOL_PREFIX_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(?:https?|ssh|git)://").expect("valid protocol prefix regex")
});

/// JS 正则 `\s`、`String.prototype.trim()` 与 `Number.parseInt` 的 TrimString 共用的
/// 空白字符集合（含 BOM `U+FEFF`）。与 Rust 的 `char::is_whitespace` 不同，这里必须
/// 逐字对齐 JS，否则同一输入在 TS 与 Rust 下会 trim 出不同结果。
fn is_js_whitespace(c: char) -> bool {
    matches!(
        c,
        ' ' | '\t' | '\n' | '\u{000B}' | '\u{000C}' | '\r' | '\u{00A0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200A}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202F}'
                | '\u{205F}'
                | '\u{3000}'
                | '\u{FEFF}'
    )
}

/// JS `String.prototype.trim()` 的等价物：去掉两端所有 `is_js_whitespace` 字符。
fn js_trim(value: &str) -> &str {
    value.trim_matches(is_js_whitespace)
}

/// JS `String.prototype.split(sep, limit)` 的语义（超出的尾部被丢弃）。
///
/// 用于复刻 hosted-git-info 的 `pathname.split('/', N)`：第 N 段只保留到下一个 `/`
/// 为止，其余尾部丢弃（例：`"a/b/c".split('/', 2)` → `["a", "b"]`，不是
/// `["a", "b/c"]`）。
fn js_split_limited(value: &str, limit: usize) -> Vec<&str> {
    if limit == 0 {
        return Vec::new();
    }
    let mut parts = Vec::new();
    let mut start = 0usize;
    while parts.len() < limit {
        if parts.len() + 1 == limit {
            let end = value[start..]
                .find('/')
                .map(|offset| start + offset)
                .unwrap_or(value.len());
            parts.push(&value[start..end]);
            break;
        }
        match value[start..].find('/') {
            Some(offset) => {
                parts.push(&value[start..start + offset]);
                start += offset + 1;
            }
            None => {
                parts.push(&value[start..]);
                break;
            }
        }
    }
    parts
}

/// JS `decodeURIComponent(value)`；失败（非法 `%` 序列或非法 UTF-8）返回 `None`。
///
/// 与 JS 一样，`%00` 会解码成 NUL，非 ASCII 字符按原 UTF-8 字节保留；任何一个 `%`
/// 缺少两位十六进制、或解码结果不是合法 UTF-8，都返回 `None`（对应 TS 里抛出的
/// `URIError`）。
///
/// `%00` 解出的 NUL 只有在它进入 host/path 时才会被 `has_unsafe_git_install_part`
/// 拒绝；作为 committish/ref 时不受该检查约束，可能原样进入 `GitSource.ref`
/// （TS 行为相同），本模块不对 ref 做安全检查。
fn decode_uri_component(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut index = 0usize;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte == b'%' {
            if index + 2 >= bytes.len() {
                return None;
            }
            let high = hex_value(bytes[index + 1])?;
            let low = hex_value(bytes[index + 2])?;
            out.push((high << 4) | low);
            index += 3;
        } else if byte < 0x80 {
            out.push(byte);
            index += 1;
        } else {
            // 非 ASCII 字符按原 UTF-8 字节拷贝（`&str` 保证边界完整）。
            let width = utf8_width(byte);
            out.extend_from_slice(&bytes[index..index + width]);
            index += width;
        }
    }
    String::from_utf8(out).ok()
}

/// 单个十六进制字符的数值；`0-9`/`a-f`/`A-F` 之外的字符返回 `None`。
fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// 由 UTF-8 首字节推断该字符的字节宽度（1–4）。
///
/// 前提：`first` 是 `&str` 内部某个字符的首字节（调用方保证索引落在 char 边界），
/// 所以不需要校验后续字节；这里不处理孤立/非法 UTF-8 字节。
fn utf8_width(first: u8) -> usize {
    if first < 0x80 {
        1
    } else if first < 0xE0 {
        2
    } else if first < 0xF0 {
        3
    } else {
        4
    }
}

/// TS: `hosted-git-info` 的 host 表。
///
/// 只保留解析所需的三个维度：规范域名、允许的协议前缀、`extract` 规则。
/// 生成各类 URL 的模板（`httpstemplate`/`tarballtemplate` 等）本模块不需要，未移植。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HostKind {
    Github,
    Bitbucket,
    Gitlab,
    Gist,
    Sourcehut,
}

impl HostKind {
    /// 该 host 的规范域名，也是 `GitSource.host` 的取值。
    fn domain(self) -> &'static str {
        match self {
            HostKind::Github => "github.com",
            HostKind::Bitbucket => "bitbucket.org",
            HostKind::Gitlab => "gitlab.com",
            HostKind::Gist => "gist.github.com",
            HostKind::Sourcehut => "git.sr.ht",
        }
    }

    /// 该 host 允许的 URL scheme（含尾随冒号），对应 hosted-git-info 的 `protocols`。
    /// 只有按域名识别的分支会检查这个列表；`github:` 等简写分支不受限制。
    fn protocols(self) -> &'static [&'static str] {
        match self {
            HostKind::Github => &["git:", "http:", "git+ssh:", "git+https:", "ssh:", "https:"],
            HostKind::Bitbucket => &["git+ssh:", "git+https:", "ssh:", "https:"],
            HostKind::Gitlab => &["git+ssh:", "git+https:", "ssh:", "https:"],
            HostKind::Gist => &["git:", "git+ssh:", "git+https:", "ssh:", "https:"],
            HostKind::Sourcehut => &["git+ssh:", "https:"],
        }
    }

    /// 简写 scheme（`github:` 等）→ host；其他 scheme 返回 `None`。
    fn from_shortcut(protocol: &str) -> Option<HostKind> {
        match protocol {
            "github:" => Some(HostKind::Github),
            "bitbucket:" => Some(HostKind::Bitbucket),
            "gitlab:" => Some(HostKind::Gitlab),
            "gist:" => Some(HostKind::Gist),
            "sourcehut:" => Some(HostKind::Sourcehut),
            _ => None,
        }
    }

    /// 主机名 → host；调用方已去掉 `www.` 前缀，这里要求完全匹配。
    fn from_domain(hostname: &str) -> Option<HostKind> {
        match hostname {
            "github.com" => Some(HostKind::Github),
            "bitbucket.org" => Some(HostKind::Bitbucket),
            "gitlab.com" => Some(HostKind::Gitlab),
            "gist.github.com" => Some(HostKind::Gist),
            "git.sr.ht" => Some(HostKind::Sourcehut),
            _ => None,
        }
    }
}

/// `hosted-git-info` 的 protocols 表（`correctProtocol` 用；值为 `(defaultRepresentation, auth)`，
/// 只有键集影响本模块行为）。
fn is_known_protocol(protocol: &str) -> bool {
    matches!(
        protocol,
        "git+ssh:"
            | "ssh:"
            | "git+https:"
            | "git:"
            | "http:"
            | "https:"
            | "git+http:"
            | "github:"
            | "bitbucket:"
            | "gitlab:"
            | "gist:"
            | "sourcehut:"
    )
}

/// `hosted-git-info` 的 `isGitHubShorthand`.
///
/// 判据与 TS 逐条对应，识别的是 `user/repo` 这种 GitHub 简写：有 `/` 且第一个 `/`
/// 不在开头；不以 `/` 结尾（若有 `#`，则要求 `#` 前一个字符不是 `/`）；不以 `.`
/// 开头；`@`、`:`、第二个 `/` 与空白都只能出现在第一个 `#` 之后（`#` 后是
/// committish，可以包含这些字符）。
fn is_github_shorthand(arg: &str) -> bool {
    let first_hash = arg.find('#');
    let first_slash = arg.find('/');
    let second_slash =
        first_slash.and_then(|index| arg[index + 1..].find('/').map(|offset| index + 1 + offset));
    let first_colon = arg.find(':');
    let first_space = arg.find(is_js_whitespace);
    let first_at = arg.find('@');

    let space_only_after_hash = match (first_space, first_hash) {
        (None, _) => true,
        (Some(space), Some(hash)) => space > hash,
        (Some(_), None) => false,
    };
    let at_only_after_hash = match (first_at, first_hash) {
        (None, _) => true,
        (Some(at), Some(hash)) => at > hash,
        (Some(_), None) => false,
    };
    let colon_only_after_hash = match (first_colon, first_hash) {
        (None, _) => true,
        (Some(colon), Some(hash)) => colon > hash,
        (Some(_), None) => false,
    };
    let second_slash_only_after_hash = match (second_slash, first_hash) {
        (None, _) => true,
        (Some(slash), Some(hash)) => slash > hash,
        (Some(_), None) => false,
    };
    let has_slash = matches!(first_slash, Some(index) if index > 0);
    let does_not_end_with_slash = match first_hash {
        Some(hash) if hash > 0 => arg.as_bytes()[hash - 1] != b'/',
        Some(_) => true, // arg[-1] === undefined → true
        None => !arg.ends_with('/'),
    };
    let does_not_start_with_dot = !arg.starts_with('.');

    space_only_after_hash
        && has_slash
        && does_not_end_with_slash
        && does_not_start_with_dot
        && at_only_after_hash
        && colon_only_after_hash
        && second_slash_only_after_hash
}

/// JS `String.prototype.lastIndexOf(ch, fromIndex)`，`fromIndex` 是 `#` 的位置（找不到则搜全串）。
///
/// 只搜索第一个 `#` 之前的部分（`#` 本身不参与）：`#` 后是 committish，其中的
/// `@`/`:` 不应影响 URL 修补。找不到 `#` 时按“搜索全串”处理（相当于传入无限大的 `fromIndex`）。
fn last_index_of_before(value: &str, needle: char, before: char) -> Option<usize> {
    let end = value.find(before).unwrap_or(value.len());
    value[..end].rfind(needle)
}

/// `hosted-git-info` 的 `correctProtocol`.
///
/// 分支与 TS 一一对应：
/// - 已是已知协议（`git:`、`ssh:`、`github:` 等）或形如 `<scheme>://` → 原样返回；
/// - 存在 `@` 且它在第一个 `:` 之后（没有 `:` 时视为 -1，同样命中）→ 假定是
///   `<foo>:<bar>@<baz>`，补 `git+ssh://`；
/// - `@` 在 `:` 之前（`git@host:path`）→ 原样返回；
/// - 没有 `@`：在第一个 `:` 后插入 `//`；没有 `:` 则在最前面补 `//`。
fn correct_protocol(arg: &str) -> String {
    let first_colon = arg.find(':');
    let protocol = match first_colon {
        Some(index) => &arg[..=index],
        None => "",
    };
    if is_known_protocol(protocol) {
        return arg.to_string();
    }
    if let Some(index) = first_colon
        && arg[index..].starts_with("://")
    {
        return arg.to_string();
    }

    match arg.find('@') {
        Some(at) if Some(at) > first_colon => format!("git+ssh://{arg}"),
        Some(_) => arg.to_string(),
        None => match first_colon {
            Some(index) => format!("{}//{}", &arg[..=index], &arg[index + 1..]),
            None => format!("//{arg}"),
        },
    }
}

/// `hosted-git-info` 的 `correctUrl`：把 scp-like URL 修补成 `new URL()` 可解析的形式。
///
/// 规则：找到第一个 `#` 之前最后一个 `@` 与最后一个 `:`；若 `:` 在 `@` 之后
/// （或没有 `@`），把该 `:` 替换为 `/`。之后若已无 `:` 且整串不含 `//`，整体加
/// `git+ssh://` 前缀。两处查找都以第一个 `#` 为界，避免 committish 里的 `@`/`:`
/// （如 `#semver:^1.0.0`）干扰。
fn correct_url(value: &str) -> String {
    let mut url = value.to_string();
    let first_at = last_index_of_before(&url, '@', '#');
    let last_colon = last_index_of_before(&url, ':', '#');

    let at_position = first_at.map(|index| index as i64).unwrap_or(-1);
    let colon_position = last_colon.map(|index| index as i64).unwrap_or(-1);
    if colon_position > at_position {
        let colon = last_colon.expect("colon position is set when it is greater");
        url = format!("{}/{}", &url[..colon], &url[colon + 1..]);
    }

    if last_index_of_before(&url, ':', '#').is_none() && !url.contains("//") {
        url = format!("git+ssh://{url}");
    }
    url
}

/// `hosted-git-info` 的 `parseUrl`。
///
/// 先按 `correct_protocol` 的结果直接解析；失败再用 `correct_url` 修补后重试；
/// 两次都失败返回 `None`。使用 `url` crate 的 WHATWG 解析（与 TS 的 `new URL()`
/// 同一规范），因此 host/path 的归一化行为一致。
fn parse_hosted_url(giturl: &str) -> Option<Url> {
    let with_protocol = correct_protocol(giturl);
    if let Ok(parsed) = Url::parse(&with_protocol) {
        return Some(parsed);
    }
    Url::parse(&correct_url(&with_protocol)).ok()
}

/// `hosted-git-info` 解析出的字段（本模块只消费 `domain`/`user`/`project`/`committish`）。
///
/// `user` 为 `None` 对应 TS 的 null（gist 单段 URL、简写分支 pathname 中没有 `/`
/// 等情形），拼接 `GitSource.path` 时得到字面量 `"null"`；`committish` 的
/// `None`/空串语义见 `hosted_from_url`：TS 的空串是 falsy，会让 `parse_git_url`
/// 回退到 `split.ref`。
#[derive(Debug, Clone, PartialEq, Eq)]
struct HostedInfo {
    domain: &'static str,
    user: Option<String>,
    project: String,
    committish: Option<String>,
}

/// TS `url.hash.slice(1)` 的等价物：无 `#` 与空 fragment 都返回 `""`。
fn hash_fragment(parsed: &Url) -> String {
    parsed.fragment().unwrap_or("").to_string()
}

/// 各 host 的 `extract` 返回值（字段仍是 percent-encoded）。
///
/// 解码统一在 `hosted_from_url` 里做，任一字段解码失败都会让该候选返回 `None`。
/// `user` 为 `None` 对应 TS 的 null（gist 单段 URL 会把 user 置 null，拼接结果中的
/// 字面量 `"null"` 即来源于此）；`committish` 的 `None`/`Some("")` 对应 TS 的
/// undefined/空串。
struct Extract {
    user: Option<String>,
    project: String,
    committish: Option<String>,
}

/// GitHub 的 `extract`：`path` 按 `split('/', 5)` 分为
/// `["", user, project, type, committish]`。
///
/// `type` 必须是空或 `"tree"`（`blob` 等网页路径返回 `None`）；`type` 为空/缺失时
/// committish 取 URL 的 `#fragment`。`project` 的 `.git` 后缀会剥掉；user 或
/// project 为空返回 `None`。`/tree` 且没有第 4 段时 committish 是 `None`，后续会
/// 变成字面量 `"undefined"`（见模块文档）。
fn extract_github(parsed: &Url) -> Option<Extract> {
    let segments = js_split_limited(parsed.path(), 5);
    let user = segments.get(1).map(|value| value.to_string());
    let mut project = segments.get(2).map(|value| value.to_string());
    let kind = segments.get(3).copied();
    let mut committish = segments.get(4).map(|value| value.to_string());

    // JS: `if (type && type !== 'tree') return;` / `if (!type) committish = hash` —— 空串是 falsy。
    if let Some(kind) = kind
        && !kind.is_empty()
        && kind != "tree"
    {
        return None;
    }
    if kind.map(str::is_empty).unwrap_or(true) {
        committish = Some(hash_fragment(parsed));
    }

    if let Some(value) = project.as_mut()
        && !value.is_empty()
        && value.ends_with(".git")
    {
        value.truncate(value.len() - 4);
    }

    let user = user.filter(|value| !value.is_empty())?;
    let project = project.filter(|value| !value.is_empty())?;
    Some(Extract {
        user: Some(user),
        project,
        committish,
    })
}

/// Bitbucket 的 `extract`：`path` 按 `split('/', 4)` 分为
/// `["", user, project, aux]`。
///
/// `aux == "get"` 是下载页 URL，返回 `None`；`.git` 后缀剥掉；committish 来自
/// `#fragment`。user 或 project 为空返回 `None`。
fn extract_bitbucket(parsed: &Url) -> Option<Extract> {
    let segments = js_split_limited(parsed.path(), 4);
    let user = segments.get(1).map(|value| value.to_string());
    let mut project = segments.get(2).map(|value| value.to_string());
    let aux = segments.get(3).copied();

    if aux == Some("get") {
        return None;
    }
    if let Some(value) = project.as_mut()
        && !value.is_empty()
        && value.ends_with(".git")
    {
        value.truncate(value.len() - 4);
    }
    let user = user.filter(|value| !value.is_empty())?;
    let project = project.filter(|value| !value.is_empty())?;
    Some(Extract {
        user: Some(user),
        project,
        committish: Some(hash_fragment(parsed)),
    })
}

/// GitLab 的 `extract`：GitLab 允许 group/subgroup 嵌套，最后一个 `/` 段是
/// project，其余各段用 `/` 连接成 user。
///
/// 含 `/-/`（Web UI 路径）或 `/archive.tar.gz`（归档下载）的 URL 返回 `None`，
/// 它们不是仓库根地址。`project` 的 `.git` 后缀会剥掉；user 或 project 为空
/// 返回 `None`；committish 来自 `#fragment`。
fn extract_gitlab(parsed: &Url) -> Option<Extract> {
    let path = parsed.path().strip_prefix('/').unwrap_or(parsed.path());
    if path.contains("/-/") || path.contains("/archive.tar.gz") {
        return None;
    }

    let mut segments: Vec<&str> = path.split('/').collect();
    let mut project = segments.pop().unwrap_or("").to_string();
    if project.ends_with(".git") {
        project.truncate(project.len() - 4);
    }
    let user = segments.join("/");
    if user.is_empty() || project.is_empty() {
        return None;
    }
    Some(Extract {
        user: Some(user),
        project,
        committish: Some(hash_fragment(parsed)),
    })
}

/// Gist 的 `extract`：`path` 按 `split('/', 4)` 拆成
/// `["", user, project, aux]`。
///
/// `aux == "raw"` 的下载页返回 `None`。gist URL 可以只有一段
/// （`gist.github.com/abc123`）：此时把该段当作 project、user 置 `None`，于是最终
/// `GitSource.path` 是字面量 `"null/abc123"`（黄金语料锁定）。`.git` 后缀会剥掉。
fn extract_gist(parsed: &Url) -> Option<Extract> {
    let segments = js_split_limited(parsed.path(), 4);
    let mut user = segments.get(1).map(|value| value.to_string());
    let mut project = segments.get(2).map(|value| value.to_string());
    let aux = segments.get(3).copied();

    if aux == Some("raw") {
        return None;
    }

    let project_missing = project
        .as_deref()
        .map(|value| value.is_empty())
        .unwrap_or(true);
    if project_missing {
        if user
            .as_deref()
            .map(|value| value.is_empty())
            .unwrap_or(true)
        {
            return None;
        }
        project = user.take();
    }

    let mut project = project?;
    if !project.is_empty() && project.ends_with(".git") {
        project.truncate(project.len() - 4);
    }
    Some(Extract {
        user,
        project,
        committish: Some(hash_fragment(parsed)),
    })
}

/// Sourcehut 的 `extract`：`path` 按 `split('/', 4)` 拆成
/// `["", user, project, aux]`。
///
/// `aux == "archive"` 是 tarball 下载页，返回 `None`；`.git` 后缀剥掉；user 或
/// project 为空返回 `None`；committish 来自 `#fragment`。
fn extract_sourcehut(parsed: &Url) -> Option<Extract> {
    let segments = js_split_limited(parsed.path(), 4);
    let user = segments.get(1).map(|value| value.to_string());
    let mut project = segments.get(2).map(|value| value.to_string());
    let aux = segments.get(3).copied();

    if aux == Some("archive") {
        return None;
    }
    if let Some(value) = project.as_mut()
        && !value.is_empty()
        && value.ends_with(".git")
    {
        value.truncate(value.len() - 4);
    }
    let user = user.filter(|value| !value.is_empty())?;
    let project = project.filter(|value| !value.is_empty())?;
    Some(Extract {
        user: Some(user),
        project,
        committish: Some(hash_fragment(parsed)),
    })
}

/// `hosted-git-info` 的 `fromUrl`（只保留 `domain`/`user`/`project`/`committish`）。
///
/// 流程：
/// 1. 空串直接 `None`；GitHub 简写（`user/repo`）补 `github:` 前缀；
/// 2. 解析 URL（先 `correct_protocol`，失败再 `correct_url`）；
/// 3. 先按 scheme 查简写 host，再按去掉 `www.` 的 hostname 查域名表；
/// 4. 简写分支直接从 pathname 拆 user/project（忽略 auth 段）；域名分支要求 scheme
///    在该 host 的 `protocols` 列表里，再调用 host 的 `extract`；
/// 5. 所有 percent 解码失败都返回 `None`（解码错误被吞掉，不向调用方抛出）。
///
/// `committish` 的映射：extract 给出 `Some("")` → `None`（TS 空串是 falsy，
/// `parse_git_url` 会回退到 `split.ref`）；`None` → `Some("undefined")`
/// （TS `decodeURIComponent(undefined)` 得到字符串 `"undefined"`）；非空 → 解码后的值。
fn hosted_from_url(giturl: &str) -> Option<HostedInfo> {
    if giturl.is_empty() {
        return None;
    }

    let corrected = if is_github_shorthand(giturl) {
        format!("github:{giturl}")
    } else {
        giturl.to_string()
    };
    let parsed = parse_hosted_url(&corrected)?;

    let protocol = format!("{}:", parsed.scheme());
    // 简写 host 由 scheme 决定，优先于按 hostname 查表。
    let shortcut = HostKind::from_shortcut(&protocol);
    let hostname = parsed.host_str().unwrap_or("");
    let hostname = hostname.strip_prefix("www.").unwrap_or(hostname);
    let kind = shortcut.or_else(|| HostKind::from_domain(hostname))?;

    if let Some(kind) = shortcut {
        let pathname = parsed.path();
        let pathname = pathname.strip_prefix('/').unwrap_or(pathname);
        let pathname = match pathname.find('@') {
            Some(index) => &pathname[index + 1..],
            None => pathname,
        };
        let last_slash = pathname.rfind('/');
        let user = match last_slash {
            Some(index) => {
                let decoded = decode_uri_component(&pathname[..index])?;
                if decoded.is_empty() {
                    None
                } else {
                    Some(decoded)
                }
            }
            None => None,
        };
        let project_part = match last_slash {
            Some(index) => &pathname[index + 1..],
            None => pathname,
        };
        let mut project = decode_uri_component(project_part)?;
        if project.ends_with(".git") {
            project.truncate(project.len() - 4);
        }
        let committish = match parsed.fragment() {
            Some(fragment) if !fragment.is_empty() => Some(decode_uri_component(fragment)?),
            _ => None,
        };
        return Some(HostedInfo {
            domain: kind.domain(),
            user,
            project,
            committish,
        });
    }

    // 只有域名分支检查协议白名单；简写分支（上面的 `if let`）不做这个检查。
    if !kind.protocols().contains(&protocol.as_str()) {
        return None;
    }

    let segments = match kind {
        HostKind::Github => extract_github(&parsed)?,
        HostKind::Bitbucket => extract_bitbucket(&parsed)?,
        HostKind::Gitlab => extract_gitlab(&parsed)?,
        HostKind::Gist => extract_gist(&parsed)?,
        HostKind::Sourcehut => extract_sourcehut(&parsed)?,
    };

    // 统一在这里做 percent 解码；任一处失败都会让整条 URL 解析失败（返回 None）。
    let user = match &segments.user {
        Some(value) if !value.is_empty() => Some(decode_uri_component(value)?),
        Some(_) => Some(String::new()),
        None => None,
    };
    let project = decode_uri_component(&segments.project)?;
    // JS 里 `decodeURIComponent(undefined)` 得到字符串 `"undefined"`
    // （`https://github.com/user/repo/tree` 的 `ref` 就是它），必须保留。
    let committish = match segments.committish.as_deref() {
        Some(value) if !value.is_empty() => Some(decode_uri_component(value)?),
        Some(_) => None,
        None => Some("undefined".to_string()),
    };

    Some(HostedInfo {
        domain: kind.domain(),
        user,
        project,
        committish,
    })
}

/// TS: `splitRef(url)` 的返回值。
///
/// `repo` 是去掉 `@ref` 后的字符串（可能仍带 query/fragment）；`ref` 只在路径中
/// `@` 的两侧都非空时才拆分出来，否则整体原样保留。
#[derive(Debug, Clone, PartialEq, Eq)]
struct SplitRef {
    repo: String,
    r#ref: Option<String>,
}

/// TS: `splitRef(url)`。
///
/// 三种输入形态：
/// 1. scp-like `git@host:path`：用 `SCP_LIKE_RE` 捕获 host 与 path，path 中第一个
///    `@` 拆分；成功时 repo 重建为 `git@{host}:{repo_path}`（不含 ref）。
/// 2. 含 `://`：`Url::parse` 后取 pathname（去前导 `/`），首个 `@` 拆分；成功时用
///    `set_path` 改回 `/{repo_path}` 再序列化，并去掉尾随 `/`（TS 同款）。
///    query/fragment 不在 pathname 里，会原样保留在 repo 中。
/// 3. 裸 `host/path`：path 中首个 `@` 拆分，repo 重建为 `{host}/{repo_path}`。
/// 任一形态下只要 `@` 左侧或右侧为空、或 `Url::parse` 失败，就原样返回且
/// `ref` 为 `None`。
fn split_ref(url: &str) -> SplitRef {
    // 形态 1：scp-like `git@host:path[@ref]`。
    if let Some(captures) = SCP_LIKE_RE.captures(url) {
        let host = captures.get(1).map(|m| m.as_str()).unwrap_or("");
        let path_with_maybe_ref = captures.get(2).map(|m| m.as_str()).unwrap_or("");
        if let Some(separator) = path_with_maybe_ref.find('@') {
            let repo_path = &path_with_maybe_ref[..separator];
            let r#ref = &path_with_maybe_ref[separator + 1..];
            if !repo_path.is_empty() && !r#ref.is_empty() {
                return SplitRef {
                    repo: format!("git@{host}:{repo_path}"),
                    r#ref: Some(r#ref.to_string()),
                };
            }
        }
        return SplitRef {
            repo: url.to_string(),
            r#ref: None,
        };
    }

    // 形态 2：带 `://` 的 URL；只有 pathname（不含 query/fragment）参与 `@` 拆分。
    if url.contains("://") {
        if let Ok(mut parsed) = Url::parse(url) {
            let path_with_maybe_ref = parsed.path().trim_start_matches('/').to_string();
            if let Some(separator) = path_with_maybe_ref.find('@') {
                let repo_path = path_with_maybe_ref[..separator].to_string();
                let r#ref = path_with_maybe_ref[separator + 1..].to_string();
                if !repo_path.is_empty() && !r#ref.is_empty() {
                    parsed.set_path(&format!("/{repo_path}"));
                    let repo = parsed.to_string();
                    let repo = repo.strip_suffix('/').unwrap_or(&repo).to_string();
                    return SplitRef {
                        repo,
                        r#ref: Some(r#ref),
                    };
                }
            }
        }
        return SplitRef {
            repo: url.to_string(),
            r#ref: None,
        };
    }

    // 形态 3：裸 `host/path[@ref]`（例如 `git:github.com/user/repo@v1`）。
    if let Some(slash) = url.find('/') {
        let host = &url[..slash];
        let path_with_maybe_ref = &url[slash + 1..];
        if let Some(separator) = path_with_maybe_ref.find('@') {
            let repo_path = &path_with_maybe_ref[..separator];
            let r#ref = &path_with_maybe_ref[separator + 1..];
            if !repo_path.is_empty() && !r#ref.is_empty() {
                return SplitRef {
                    repo: format!("{host}/{repo_path}"),
                    r#ref: Some(r#ref.to_string()),
                };
            }
        }
    }

    SplitRef {
        repo: url.to_string(),
        r#ref: None,
    }
}

/// TS: `decodeForValidation(value)` —— 解码失败返回 `None`。
///
/// 与 `decode_uri_component` 相同；单独命名是为了与 TS 的调用点一一对应。
fn decode_for_validation(value: &str) -> Option<String> {
    decode_uri_component(value)
}

/// TS: `hasUnsafeGitInstallPart(value, allowSlash)`.
///
/// 安装路径的安全门禁，对原串和解码后的串各查一遍（防止 `%2e%2e`、`%00`、`%5c`
/// 绕过）：
/// - 含 NUL 或反斜杠 → 不安全；
/// - 以 `/` 开头 → 不安全；
/// - `allow_slash == false`（host）时含 `/` → 不安全；
/// - 任一段恰为 `..` → 不安全；
/// - 解码失败（非法 `%` 序列或非法 UTF-8）→ 直接视为不安全。
///
/// 测试 `tests/git_ssh_url.rs::should_reject_unsafe_git_install_path_inputs` 覆盖了
/// `..%2F..%2F`、尾部单个 `%`、绝对路径、反斜杠与 NUL。
fn has_unsafe_git_install_part(value: &str, allow_slash: bool) -> bool {
    let Some(decoded) = decode_for_validation(value) else {
        return true;
    };
    for candidate in [value, decoded.as_str()] {
        if candidate.contains('\0') || candidate.contains('\\') || candidate.starts_with('/') {
            return true;
        }
        if !allow_slash && candidate.contains('/') {
            return true;
        }
        if candidate.split('/').any(|segment| segment == "..") {
            return true;
        }
    }
    false
}

/// TS: `buildGitSource({ repo, host, path, ref })`.
///
/// 所有解析分支的唯一构造出口与安全检查点。返回 `None` 的条件：
/// - `path` 以 `/` 开头；
/// - 去掉一个尾随 `.git` 与所有前导 `/` 后，host 为空、path 为空，或 `/` 分段数
///   小于 2（只要求至少两段，不要求各段非空，`user/`、`user//repo` 也能通过）；
/// - host 或 path 未通过 `has_unsafe_git_install_part`（host 不允许含 `/`）。
///
/// `pinned` 在这里由 `ref` 是否存在且非空计算，调用方不再自行推导。
///
/// 注意 `parse_git_url` 的两个候选循环是 `return build_git_source(...)`：一旦 host
/// 命中但这里校验失败，不会再尝试该循环的后续候选或通用解析（与 TS 相同）。
fn build_git_source(
    repo: String,
    host: String,
    path: String,
    r#ref: Option<String>,
) -> Option<GitSource> {
    if path.starts_with('/') {
        return None;
    }
    let normalized_path = path.strip_suffix(".git").unwrap_or(&path);
    let normalized_path = normalized_path.trim_start_matches('/');

    // 安装路径至少要有两段（不要求各段非空）；host 本身不允许含 `/`。
    if host.is_empty() || normalized_path.is_empty() || normalized_path.split('/').count() < 2 {
        return None;
    }
    if has_unsafe_git_install_part(&host, false)
        || has_unsafe_git_install_part(normalized_path, true)
    {
        return None;
    }

    let pinned = r#ref.as_deref().is_some_and(|value| !value.is_empty());
    Some(GitSource {
        source_type: "git".to_string(),
        repo,
        host,
        path: normalized_path.to_string(),
        r#ref,
        pinned,
    })
}

/// TS: `parseGenericGitUrl(url)`。
///
/// `split_ref` 之后按 repo 的形态分三支：
/// - scp-like `git@host:path`：host/path 来自 `SCP_LIKE_RE` 的捕获；
/// - 以 `https://`、`http://`、`ssh://`、`git://` 开头：`Url::parse` 取 hostname 与
///   pathname（去前导 `/`）；
/// - 裸 `host/path`：host 必须含 `.` 或等于 `localhost`，否则返回 `None`；repo 补
///   `https://` 前缀。
///
/// 三支最终都交给 `build_git_source` 做统一校验。
fn parse_generic_git_url(url: &str) -> Option<GitSource> {
    let split = split_ref(url);
    let mut repo = split.repo;
    let r#ref = split.r#ref;

    let (host, path) = if let Some(captures) = SCP_LIKE_RE.captures(&repo) {
        (
            captures
                .get(1)
                .map(|m| m.as_str())
                .unwrap_or("")
                .to_string(),
            captures
                .get(2)
                .map(|m| m.as_str())
                .unwrap_or("")
                .to_string(),
        )
    } else if repo.starts_with("https://")
        || repo.starts_with("http://")
        || repo.starts_with("ssh://")
        || repo.starts_with("git://")
    {
        let parsed = Url::parse(&repo).ok()?;
        (
            parsed.host_str().unwrap_or("").to_string(),
            parsed.path().trim_start_matches('/').to_string(),
        )
    } else {
        let slash = repo.find('/')?;
        let host = repo[..slash].to_string();
        let path = repo[slash + 1..].to_string();
        if !host.contains('.') && host != "localhost" {
            return None;
        }
        repo = format!("https://{repo}");
        (host, path)
    };

    build_git_source(repo, host, path, r#ref)
}

/// TS: `parseGitUrl(source)` —— 解析 git 源为 [`GitSource`]；不支持/不安全时返回 `None`。
///
/// 规则（逐字）：
/// - 有 `git:` 前缀时接受所有历史简写；无前缀时必须以 `https?://`、`ssh://`、`git://`
///   开头（大小写不敏感）；
/// - 先试 `hosted-git-info` 的 host（含 `#committish` 与 `@ref`），再试 `https://` 前缀，
///   最后走通用 git URL 解析。
///
/// # 参数与返回
///
/// - `source`：配置中的原始源字符串；首尾的 JS 空白（含 `U+FEFF`）会被去掉。
/// - `Some(GitSource)`：受支持的 git 源；host/path 已通过安全检查，但 ref 未做任何
///   校验，不能保证可 checkout/可安装，需要调用方自行校验。`path` 已去掉 `.git`
///   后缀（任何以 `/` 开头的 `path` 都会被直接拒绝），`pinned` 由 ref 是否非空决定。
/// - `None`：不是受支持的 git 源，或 URL 解码/安全检查失败。它只表示语法上不接受，
///   不是网络错误；`kiana-resources` 的 `parse_source` 会把原字符串当成本地路径继续
///   处理。
///
/// # 副作用与边界
///
/// 纯函数，无网络/文件系统访问，也不校验仓库是否真实存在。解析出的 host 或路径
/// 若含 `..` 段、NUL、反斜杠或以 `/` 开头，都会被 `has_unsafe_git_install_part`
/// 拒绝。
///
/// # 示例（`text`，非 doctest）
///
/// ```text
/// parse_git_url("git:git@github.com:user/repo@v1")
///   -> GitSource { source_type: "git", repo: "git@github.com:user/repo",
///                   host: "github.com", path: "user/repo",
///                   ref: Some("v1"), pinned: true }
/// parse_git_url("git@github.com:user/repo") -> None   // 无 git: 前缀，门禁拒绝
/// ```
pub fn parse_git_url(source: &str) -> Option<GitSource> {
    let trimmed = js_trim(source);
    let has_git_prefix = trimmed.starts_with("git:");
    let url = if has_git_prefix {
        js_trim(&trimmed[4..])
    } else {
        trimmed
    };

    // 门禁：没有 `git:` 前缀时，只接受带受支持 scheme 的绝对 URL。
    if !has_git_prefix && !PROTOCOL_PREFIX_RE.is_match(url) {
        return None;
    }

    let split = split_ref(url);

    // 候选 1：把拆出的 ref 拼回 `#committish`，让 hosted-git-info 能识别
    // `@ref` 形式；候选 2：原串（覆盖 `#committish` 已在 URL 里的情况）。
    let mut hosted_candidates: Vec<String> = Vec::new();
    if let Some(r#ref) = &split.r#ref {
        hosted_candidates.push(format!("{}#{}", split.repo, r#ref));
    }
    hosted_candidates.push(url.to_string());

    for candidate in &hosted_candidates {
        let Some(info) = hosted_from_url(candidate) else {
            continue;
        };
        // ref 已单独拆出时，project 里残留的 `@` 说明这个候选把 ref 误当成了
        // project 的一部分，跳过它。
        if split.r#ref.is_some() && info.project.contains('@') {
            continue;
        }
        let repo = if use_https_prefix(&split.repo) {
            format!("https://{}", split.repo)
        } else {
            split.repo.clone()
        };
        let path = format!(
            "{}/{}",
            info.user.as_deref().unwrap_or("null"),
            info.project
        );
        let r#ref = info.committish.or_else(|| split.r#ref.clone());
        return build_git_source(repo, info.domain.to_string(), path, r#ref);
    }

    // 第二组候选：原串没有 scheme 时，补 `https://` 后再交给 hosted-git-info；
    // 这一步覆盖 `git:github.com/user/repo` 这类历史简写。
    let mut https_candidates: Vec<String> = Vec::new();
    if let Some(r#ref) = &split.r#ref {
        https_candidates.push(format!("https://{}#{}", split.repo, r#ref));
    }
    https_candidates.push(format!("https://{url}"));

    for candidate in &https_candidates {
        let Some(info) = hosted_from_url(candidate) else {
            continue;
        };
        if split.r#ref.is_some() && info.project.contains('@') {
            continue;
        }
        let path = format!(
            "{}/{}",
            info.user.as_deref().unwrap_or("null"),
            info.project
        );
        let r#ref = info.committish.or_else(|| split.r#ref.clone());
        return build_git_source(
            format!("https://{}", split.repo),
            info.domain.to_string(),
            path,
            r#ref,
        );
    }

    // 兜底：不在 host 表里的通用 git URL（scp-like / 带 scheme / 裸 host/path）。
    parse_generic_git_url(url)
}

/// repo 没有显式 scheme、也不是 scp-like `git@...` 时返回 `true`，调用方据此补
/// `https://` 前缀。列表之外的形式（如其他 user 的 scp 形式）同样返回 `true`，
/// 这是逐字移植 TS 判定的结果。
fn use_https_prefix(repo: &str) -> bool {
    !repo.starts_with("http://")
        && !repo.starts_with("https://")
        && !repo.starts_with("ssh://")
        && !repo.starts_with("git://")
        && !repo.starts_with("git@")
}
