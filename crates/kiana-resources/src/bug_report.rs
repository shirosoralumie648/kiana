//! core/bug-report.ts + bug-report-upload.ts。
//!
//! 逐字移植 `packages/coding-agent/src/core/bug-report.ts`（375 行）与
//! `bug-report-upload.ts`（37 行）。JSON 键序与数值格式依赖 `serde_json` 的
//! `preserve_order` 以及 [`kiana_json::js_object::stringify_pretty`]（等价 `JSON.stringify(v, null, 2)`）。
//!
//! # 关于 `compaction/**` 的刻意重复
//!
//! `core/bug-report.ts` 从 `core/compaction/**` 引入 `estimateTokens` / `serializeConversation` /
//! `completeSummarization` / `getSummarizationFailure`，但 `kiana-compaction` **不是** `kiana-resources`
//! 的允许依赖（`ca-resources.md` §2.2，且 `kiana-ext-api` 的传递边不能用）。因此本文件用私有本地
//! 辅助函数复刻 `kiana-compaction` 的这些公开语义（`select_messages` / `estimate_tokens_local` /
//! `serialize_conversation` / `complete_summarization` / `summarization_failure`），并在
//! [`generate_bug_report_summary`] 中通过 `kiana_agent_core` 的 `StreamFn` + `kiana_ai` 驱动模型。
//! 这是本单元**唯一**的刻意重复；`bug-report.test.ts` 不覆盖 LLM 路径。
//!
//! `collectEnvironment` 里 `os.release()` / `os.version()` / `process.platform` 等 Node
//! 运行期值在 Rust 侧按平台映射（见 [`collect_environment`]）；`createdAt` 用 UTC ISO-8601、
//! 毫秒恒 3 位。
//!
//! # 模块边界与调用方
//!
//! 本文件只负责**收集与序列化**，不直接读写会话文件、也不驱动交互：
//! `kiana-interactive` 的 `/bug` 命令负责提问、选择上传或导出；
//! `kiana-session` 的 `AgentSession::summarize_for_bug_report` 准备鉴权后调用
//! [`generate_bug_report_summary`]。会话自定义条目 [`BUG_REPORT_CUSTOM_ENTRY_TYPE`]
//! 由调用方在归档/上传成功后写入。
//!
//! 输入是会话条目（`ReadonlySessionManager`）、模型与 `ModelRuntime`、扩展列表和两份
//! 设置；输出是 `report.json` / `diagnostics.json` 的 `Value`（键序即输出顺序）、
//! 内存中的文件列表、ZIP 归档或一次 HTTP 上传。
//!
//! # 一次报告的执行流程
//!
//! 1. 需要摘要时：[`generate_bug_report_summary`] 按上下文窗口的 60% 挑选最近消息、
//!    序列化对话并请求模型；
//! 2. [`collect_bug_report_metadata`] 汇总环境、会话元信息、模型、扩展与设置；
//! 3. [`collect_bug_report_diagnostics`] 提取失败/被中止的 assistant turn 与崩溃记录；
//! 4. [`bug_report_files`] 按固定顺序生成文件；导出走 [`write_bug_report_archive`]，
//!    上传走 [`upload_bug_report`]。
//!
//! # 输出契约（修改前先看测试）
//!
//! - **键序**：工作区的 `serde_json` 启用了 `preserve_order`，`Map` 的插入顺序就是
//!   JSON 文本顺序；它与 TS 对象字面量一致，`tests/bug_report.rs` 逐键锁定。
//! - **字符串长度**：token 估算按 UTF-16 code unit（`String.prototype.length` 的语义），
//!   不是 Rust 的字节数或 `char` 数。
//! - **脱敏**：键名命中敏感词时整个值替换为 [`REDACTED`]，字符串值再经过 [`redact_url`]；
//!   各收集函数在构造输出时完成这些步骤。
//!
//! # 边界与失败模式
//!
//! - `collect_*` / `describe_*` / `redact_*` 与 [`bug_report_files`] 都返回 `Value` 或
//!   普通结构体，没有 `Result`；其中 [`collect_environment`] 会读环境变量、在非 Windows
//!   平台读 `/proc/sys/kernel/*` 并可能启动 `uname`（Windows 改走 `RtlGetVersion` 与
//!   注册表），失败时退化为空串或 `null`，其余函数只做内存计算；枚举序列化失败时
//!   退化为 `null`；
//! - 对外写出的两个入口返回可失败结果：[`write_bug_report_archive`] 用 `std::io::Error`
//!   报告写盘失败（不创建父目录，见 `kiana_core::zip` 的模块文档），
//!   [`upload_bug_report`] 用 [`BugReportUploadError`] 报告解析/网络/协议失败；
//! - [`generate_bug_report_summary`] 把模型的取消、错误停止、toolCall、空文本映射成
//!   [`BugReportSummaryError`]，其 `Display` 就是 TS 的 `error.message`，调用方直接展示；
//! - 上传函数自身不实现重试；网络层失败统一折叠成 `fetch failed`，取消优先于已经就绪的
//!   响应（`select!` 带 `biased`）。
//!
//! # 阅读顺序
//!
//! [`redact_url`] → [`collect_environment`] → [`collect_bug_report_metadata`] →
//! [`collect_bug_report_diagnostics`] → [`bug_report_files`] →
//! [`generate_bug_report_summary`] → [`upload_bug_report`]。

use std::collections::HashSet;
use std::path::Path;

use kiana_agent_core::types::{AgentMessage, StreamFn, ThinkingLevel};
use kiana_ai::compat::complete_simple;
use kiana_ai::types::{
    AssistantContentBlock, AssistantMessage, CacheRetention, Context, JsonObject, Message, Model,
    SimpleStreamOptions, StreamOptions, TextContent, TextKind, UserContent, UserContentBlock,
    UserMessage, UserRole,
};
use kiana_ai::utils::abort::AbortScope;
use kiana_ai::utils::error_body::safe_json_stringify;
use kiana_ai::utils::retry::{RetryPolicy, retry_assistant_call};
use kiana_ai::utils::text::{content_text_assistant, content_text_user, content_text_user_content};
use kiana_config::models::runtime::ModelRuntime;
use kiana_config::settings::Settings;
use kiana_core::zip::{ZipEntry, write_zip_archive};
use kiana_ext_api::types::Extension;
use kiana_session_core::messages::convert_to_llm;
use kiana_session_core::session_manager::{ReadonlySessionManager, SessionEntry};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value};

use crate::crash_log::CrashRecord;

/// 会话自定义条目类型。TS: `bug-report.ts#BUG_REPORT_CUSTOM_ENTRY_TYPE`。
///
/// 调用方用 `append_custom_entry` 把 [`BugReportSessionEntryData`] 写进会话时，以该字符串
/// 作为条目类型；读取方按同一字符串识别条目。值移植时从 `pi.bug-report` 改为
/// `kiana.bug-report`，与既有会话文件的兼容性取决于这一常量。
pub const BUG_REPORT_CUSTOM_ENTRY_TYPE: &str = "kiana.bug-report";
/// schema 版本。TS: `bug-report.ts#BUG_REPORT_SCHEMA_VERSION`。
///
/// 同时写入 `report.json`（[`collect_bug_report_metadata`]）与 `diagnostics.json`
/// （[`collect_bug_report_diagnostics`]）的 `schemaVersion` 字段。
pub const BUG_REPORT_SCHEMA_VERSION: u64 = 1;
/// 脱敏占位符。TS: `bug-report.ts#REDACTED`。
///
/// 作为查询参数值写回 URL 时会被 percent-encode 成 `%3Credacted%3E`（见 [`redact_url`]
/// 与测试 `removes_url_credentials_and_secret_query_parameters`）。
pub const REDACTED: &str = "<redacted>";

/// URL 查询参数敏感键的正则（逐字）。TS: `bug-report.ts#SENSITIVE_KEY`。
///
/// 这里手写匹配：`(?:^|[-_])(api[-_]?key|secret|token|password|passwd|credential|authorization|cookie)(?:$|[-_])`，
/// 不区分大小写。
///
/// 数组保存其中**固定**的分支；`api[-_]?key` 的可选分隔符无法用简单的词表表达，由
/// `matches_sensitive_word_at` 单独处理。末尾的空串只是占位，匹配循环会跳过它。
const SENSITIVE_WORDS: [&str; 8] = [
    "secret",
    "token",
    "password",
    "passwd",
    "credential",
    "authorization",
    "cookie",
    "",
];

/// `key` 是否是敏感键（camelCase → snake，再按 `SENSITIVE_KEY` 匹配）。
///
/// TS: `bug-report.ts#isSensitiveKey`。
///
/// 判定规则（全部大小写不敏感）：
/// - 先在 `[a-z0-9]` 与大写字母之间插入 `_`（`apiKey` → `api_Key`）；
/// - 只在字符串开头或 `-`/`_` 之后尝试起匹配；
/// - 词后必须是结尾、`-` 或 `_`。
///
/// 因此 `token`、`accessToken`、`api-key`、`api_key` 命中，而 `tokenizer`、`monkey`、
/// `mysecrets`、`compactionTokens` 不命中（测试
/// `sensitive_key_matches_camel_case_and_separator_boundaries` 锁定两组样例）。
pub fn is_sensitive_key(key: &str) -> bool {
    // `key.replace(/([a-z0-9])([A-Z])/g, "$1_$2")`
    let mut normalized = String::with_capacity(key.len() + 4);
    let mut previous: Option<char> = None;
    for character in key.chars() {
        if character.is_ascii_uppercase()
            && previous
                .is_some_and(|previous| previous.is_ascii_lowercase() || previous.is_ascii_digit())
        {
            normalized.push('_');
        }
        normalized.push(character);
        previous = Some(character);
    }

    let characters: Vec<char> = normalized.chars().collect();
    for index in 0..characters.len() {
        if index > 0 && !matches!(characters[index - 1], '-' | '_') {
            continue;
        }
        if matches_sensitive_word_at(&characters, index) {
            return true;
        }
    }
    false
}

/// 在 `characters[index..]` 匹配一个敏感词，并要求其后是结尾或 `-`/`_`。
fn matches_sensitive_word_at(characters: &[char], index: usize) -> bool {
    // `api[-_]?key`。
    if starts_with_ignore_case(characters, index, "api") {
        let mut next = index + 3;
        if characters.get(next).is_some_and(|c| matches!(c, '-' | '_')) {
            next += 1;
        }
        if starts_with_ignore_case(characters, next, "key") {
            let after = next + 3;
            if is_key_boundary(characters, after) {
                return true;
            }
        }
    }
    for word in SENSITIVE_WORDS {
        if word.is_empty() {
            continue;
        }
        if starts_with_ignore_case(characters, index, word)
            && is_key_boundary(characters, index + word.len())
        {
            return true;
        }
    }
    false
}

fn starts_with_ignore_case(characters: &[char], index: usize, word: &str) -> bool {
    let mut position = index;
    for expected in word.chars() {
        match characters.get(position) {
            Some(actual) if actual.eq_ignore_ascii_case(&expected) => position += 1,
            _ => return false,
        }
    }
    true
}

fn is_key_boundary(characters: &[char], index: usize) -> bool {
    match characters.get(index) {
        None => true,
        Some(character) => matches!(character, '-' | '_'),
    }
}

/// WHATWG 的 special scheme 集合（小写）。
fn is_special_scheme(scheme: &str) -> bool {
    matches!(scheme, "http" | "https" | "ws" | "wss" | "ftp" | "file")
}

/// 原始文本里 `scheme://` 之后的 authority 段（到 `/`、`?`、`#` 或结尾）。
fn raw_authority(value: &str) -> Option<&str> {
    let colon = value.find(':')?;
    let rest = value.get(colon + 1..)?;
    let rest = rest.strip_prefix("//")?;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    rest.get(..end)
}

/// `new URL(value)` 会拒绝、但 `url` crate 会接受的输入。
///
/// WHATWG URL 的 authority 解析：scheme 不是 special scheme 时 host 可以为空，但
/// authority 里出现过 `@`（带 userinfo）时空 host 必须使解析失败
/// （`ssh://:@?token=1` → `Invalid URL`）。`url` crate 丢弃空 userinfo 后照常解析，
/// 于是 TS 的 `catch` 返回原串，而 Rust 会把它重写成 `ssh://?token=...`。
fn whatwg_rejects_empty_host_with_userinfo(value: &str, url: &url::Url) -> bool {
    !is_special_scheme(url.scheme())
        && url.has_authority()
        && url.host_str().is_none_or(str::is_empty)
        && raw_authority(value).is_some_and(|authority| authority.contains('@'))
}

/// `new URL(value)` 会拒绝、但 `url` crate 会接受的另一种输入。
///
/// 非 special scheme 不把 `\` 当分隔符：它留在 authority 里，落在 host 段就是
/// forbidden host code point，落在 port 段就是非法端口，整个 URL 解析失败
/// （`ssh://host:2222\a\b?token=1`）。userinfo 里的 `\` 合法（会被百分号编码），
/// 因此只看最后一个 `@` 之后的部分。
fn whatwg_rejects_non_special_authority_backslash(value: &str, scheme: &str) -> bool {
    if is_special_scheme(scheme) {
        return false;
    }
    let Some(authority) = raw_authority(value) else {
        return false;
    };
    let host_and_port = match authority.rfind('@') {
        Some(index) => &authority[index + 1..],
        None => authority,
    };
    host_and_port.contains('\\')
}

/// 脱敏 URL：去掉 userinfo，并把敏感查询参数替换为 `<redacted>`。
///
/// TS: `bug-report.ts#redactUrl`。未发生变化时**返回原始字符串**（不重新序列化）。
///
/// 处理顺序：
/// 1. 嵌套协议前缀（`git:https://…`）先剥离外层 scheme，只递归脱敏内层 URL；
/// 2. 用 `url` crate 解析；对 `new URL()` 会拒绝、而 `url` crate 会接受的两类输入
///    （空 host 且带 userinfo、非 special scheme 的 authority 含 `\`）原样返回；
/// 3. 清除 userinfo；查询串里存在敏感键时重建：敏感键只保留**第一次**出现并写成
///    `<redacted>`（同名参数被丢弃），非敏感参数保持相对顺序；
/// 4. `file:` URL 按原始文本还原 host/path 后自行序列化（`whatwg_file_parts`），
///    其余 scheme 用 `url` crate 的序列化结果。
///
/// 注意：只有发生修改才会重新序列化，因此未命中敏感信息的 URL（包括大小写、默认端口
/// 写法）保持输入原样。查询串一旦重建，所有参数都会经过一次 form-urlencoded 编解码，
/// 与 TS 的 `URLSearchParams.set` 行为一致。
pub fn redact_url(value: &str) -> String {
    // 嵌套形式：`foo:https://user:pass@host`（外层不是 URL 的协议前缀）。
    // TS 的嵌套正则用 `.*` 匹配内层，而 JS 的 `.` 不跨行终止符；因此含 `\n`、`\r`、
    // U+2028/U+2029 时这里不按嵌套处理，走下面的普通解析路径。
    //
    // 找到第一个真正的 `scheme://`，前提是它前面的所有内容都是 `scheme:` 链。
    // 这样 `a:b:https://…`、`a:a:a:…:https://…` 都能剥掉全部外层前缀（迭代扫描，
    // 嵌套层数不会造成递归）。
    //
    // 与 `spec/ca-resources.md` §2.13.2 的有意差异：spec 的单层正则会把这些多级前缀
    // 当成 WHATWG 不透明 URL 原样返回（凭据不脱敏）。bug report 会上传原始字符串，
    // 这里选择多脱敏一层；测试 `multi_level_scheme_prefixes_are_redacted` 锁定该行为。
    if let Some(start) = nested_url_start(value) {
        let mut output = String::with_capacity(value.len());
        output.push_str(&value[..start]);
        output.push_str(&redact_url_flat(&value[start..]));
        return output;
    }
    redact_url_flat(value)
}

/// `value` 里真正的 URL 起点（多级 `scheme:` 前缀之后）。
///
/// 返回 `None`：没有 `://`、前缀不全是合法 scheme、或 `://` 前没有任何前缀（本身就是
/// 顶层 URL）。含 JS 正则 `.` 不跨过的行终止符时也不按嵌套处理。
fn nested_url_start(value: &str) -> Option<usize> {
    if value.contains(['\n', '\r', '\u{2028}', '\u{2029}']) {
        return None;
    }
    let mut search_from = 0usize;
    while let Some(offset) = value[search_from..].find("://") {
        let separator = search_from + offset;
        let bytes = value.as_bytes();
        let mut start = separator;
        while start > 0
            && (bytes[start - 1].is_ascii_alphanumeric()
                || matches!(bytes[start - 1], b'+' | b'-' | b'.'))
        {
            start -= 1;
        }
        if start < separator && bytes[start].is_ascii_alphabetic() {
            let prefix = value[..start].strip_suffix(':')?;
            let schemes_valid = !prefix.is_empty()
                && prefix.split(':').all(|scheme| {
                    let mut chars = scheme.chars();
                    chars
                        .next()
                        .is_some_and(|first| first.is_ascii_alphabetic())
                        && chars.all(|character| {
                            character.is_ascii_alphanumeric()
                                || matches!(character, '+' | '-' | '.')
                        })
                });
            if schemes_valid {
                return Some(start);
            }
        }
        search_from = separator + 3;
    }
    None
}

/// 单层 URL 的脱敏（不含嵌套协议前缀）。
fn redact_url_flat(value: &str) -> String {
    let Ok(mut url) = url::Url::parse(value) else {
        return value.to_string();
    };
    if whatwg_rejects_empty_host_with_userinfo(value, &url) {
        return value.to_string();
    }
    if whatwg_rejects_non_special_authority_backslash(value, url.scheme()) {
        return value.to_string();
    }
    let mut changed = false;
    if !url.username().is_empty() || url.password().is_some() {
        let _ = url.set_username("");
        let _ = url.set_password(None);
        changed = true;
    }

    let pairs: Vec<(String, String)> = url.query_pairs().into_owned().collect();
    if pairs.iter().any(|(key, _)| is_sensitive_key(key)) {
        // TS 用 `searchParams.set`：同名敏感键折叠成第一次出现位置上的一个
        // `<redacted>`，其余同名参数被删除。`emitted` 复现该行为。
        let mut emitted: HashSet<String> = HashSet::new();
        let mut rebuilt: Vec<(String, String)> = Vec::with_capacity(pairs.len());
        for (key, value) in pairs {
            if is_sensitive_key(&key) {
                if emitted.insert(key.clone()) {
                    rebuilt.push((key, REDACTED.to_string()));
                }
            } else {
                rebuilt.push((key, value));
            }
        }
        url.set_query(None);
        {
            let mut query = url.query_pairs_mut();
            for (key, value) in &rebuilt {
                query.append_pair(key, value);
            }
        }
        changed = true;
    }

    if changed {
        // `url` crate 的 file URL 序列化与 WHATWG 有多处差异（host 丢弃、盘符 `|`、
        // 前导空段），这里按原始文本还原 host/path 后自行拼接。
        if url.scheme() == "file"
            && let Some(parts) = whatwg_file_parts(value)
        {
            let mut serialized = String::with_capacity(value.len());
            serialized.push_str("file://");
            serialized.push_str(&parts.host);
            serialized.push_str(&parts.path);
            if let Some(query) = url.query() {
                serialized.push('?');
                serialized.push_str(query);
            }
            if let Some(fragment) = url.fragment() {
                serialized.push('#');
                serialized.push_str(fragment);
            }
            return serialized;
        }
        url.to_string()
    } else {
        value.to_string()
    }
}

/// WHATWG `file:` URL 的 host 与 path（用于重新序列化）。
struct WhatwgFileParts {
    /// 规范化后的 host；空串表示「无 host」。
    host: String,
    /// WHATWG 路径（保留前导空段、盘符已规范化）。
    path: String,
}

/// 是否是 `[A-Za-z]:` / `[A-Za-z]|` 形式的 Windows 盘符段。
fn is_windows_drive_letter_text(segment: &str) -> bool {
    let bytes = segment.as_bytes();
    bytes.len() == 2 && bytes[0].is_ascii_alphabetic() && matches!(bytes[1], b':' | b'|')
}

/// 从原始文本还原 WHATWG `file:` URL 的 host 与 path。
///
/// `url` crate 的 file 解析与 WHATWG 不同：
/// - 路径首段是盘符时会丢掉 host（WHATWG 保留，如 `file://host/C:/x`）；
/// - 丢失路径开头的空段（`file://[::1]//a` → `/a`）；
/// - 不把盘符里的 `|` 归一为 `:`（`file:///C|/x` → `/C:/x`）。
///
/// 这里把 host 交给 http 解析器（special scheme 的 host 解析与 file 相同）做
/// percent-decode / IDNA / IPv4 归一，把路径交给 https 解析器（同样是 special
/// scheme，路径状态机与 file 一致且保留前导空段）。无法可靠还原时返回 `None`，
/// 调用方回退到 `url` crate 的结果。
fn whatwg_file_parts(value: &str) -> Option<WhatwgFileParts> {
    // WHATWG：解析前去掉所有 ASCII tab/newline，并去掉首尾的 C0 控制符与空格。
    let cleaned: String = value
        .chars()
        .filter(|character| !matches!(character, '\t' | '\n' | '\r'))
        .collect();
    let cleaned = cleaned.trim_matches(|character: char| character <= '\u{20}');
    let colon = cleaned.find(':')?;
    if !cleaned[..colon].eq_ignore_ascii_case("file") {
        return None;
    }
    let rest = &cleaned[colon + 1..];

    let (host, path_text) = if rest.starts_with("//") || rest.starts_with("\\\\") {
        // `file://\\host` / `file:////a`：文件 slash 状态把 `\` 与 `/` 都当分隔符。
        let after_separators = &rest[2..];
        let host_end = after_separators
            .find(['/', '\\', '?', '#'])
            .unwrap_or(after_separators.len());
        let host_raw = &after_separators[..host_end];
        let remaining = &after_separators[host_end..];
        if is_windows_drive_letter_text(host_raw) {
            // `file://C:/x`：盘符不是 host，WHATWG 从盘符开始重新解析路径。
            (String::new(), format!("/{host_raw}{remaining}"))
        } else if host_raw.is_empty() {
            (String::new(), remaining.to_string())
        } else {
            (normalize_file_host(host_raw)?, remaining.to_string())
        }
    } else {
        // `file:a` / `file:/a`：没有 authority。
        (String::new(), rest.to_string())
    };

    let path_end = path_text.find(['?', '#']).unwrap_or(path_text.len());
    let raw_path = path_text[..path_end].replace('\\', "/");
    // 先按 WHATWG 的 path state 去掉 `.` / `..` 段。`url` crate 的 https 解析在
    // `..` 前一段形如盘符（如 `/C:/C:/..`）时不会回退（它保留了 file scheme 的
    // “不要弹出 Windows 盘符”规则），这里自己处理以保证与 V8 一致。
    let raw_path = remove_file_dot_segments(&raw_path);
    let candidate = format!("https://x{raw_path}");
    let parsed = url::Url::parse(&candidate).ok()?;
    let mut path = parsed.path().to_string();
    normalize_file_drive_letter(&mut path);
    Some(WhatwgFileParts { host, path })
}

/// WHATWG path state 的 `.` / `..` 处理（special scheme，路径以 `/` 开头）。
///
/// 单点段是 `.` 或 `%2e`（大小写不敏感）；双点段还接受 `.` / `..` 任一侧写成
/// percent-encoded 的组合（逐项见 `is_double_dot` 的匹配列表）。比较针对原始段文本，
/// 发生在后续交给 `url` crate 解析（会做 percent-decode）之前。
///
/// 与 `url` crate 的差异：`..` 前一段是 Windows 盘符时 WHATWG 仍会回退（除了路径
/// 只有该盘符这一项时），而 `url` crate 的 `pop_path` 对 file scheme 有额外保护。
fn remove_file_dot_segments(raw_path: &str) -> String {
    fn is_single_dot(segment: &str) -> bool {
        matches!(segment, "." | "%2e" | "%2E")
    }
    fn is_double_dot(segment: &str) -> bool {
        matches!(
            segment,
            ".." | "%2e%2e" | "%2e%2E" | "%2E%2e" | "%2E%2E" | "%2e." | "%2E." | ".%2e" | ".%2E"
        )
    }

    let normalized = if raw_path.starts_with('/') {
        raw_path.to_string()
    } else {
        format!("/{raw_path}")
    };
    let parts: Vec<&str> = normalized.split('/').skip(1).collect();
    let last_index = parts.len().saturating_sub(1);
    let mut segments: Vec<&str> = Vec::new();
    for (index, part) in parts.iter().enumerate() {
        let at_end = index == last_index;
        if is_single_dot(part) {
            // 单点段被丢弃；位于末尾时 WHATWG 追加一个空段（保留尾随 `/`）。
            if at_end {
                segments.push("");
            }
        } else if is_double_dot(part) {
            // file scheme：路径只有一项且是盘符时不会回退。WHATWG 在 append 时就把
            // 首段的 `|` 规范化成 `:`，所以这里按盘符形式（`:` 或 `|`）判断。
            let protected = segments.len() == 1 && is_windows_drive_letter_text(segments[0]);
            if !protected {
                segments.pop();
            }
            if at_end {
                segments.push("");
            }
        } else {
            segments.push(part);
        }
    }
    format!("/{}", segments.join("/"))
}

/// WHATWG file host 解析（percent-decode + IDNA + IPv4 归一）。空串表示无 host。
///
/// file URL 没有 userinfo/port：`@`、`:` 出现在 host 里即解析失败；IPv6 必须整体
/// 是 `[...]`。返回 `None` 表示无法还原（调用方回退）。
fn normalize_file_host(host: &str) -> Option<String> {
    if host.starts_with('[') {
        if !host.ends_with(']') {
            return None;
        }
    } else if host.contains([':', '@', '[', ']']) {
        return None;
    }
    let parsed = url::Url::parse(&format!("http://{host}/")).ok()?;
    if !parsed.username().is_empty() || parsed.password().is_some() || parsed.port().is_some() {
        return None;
    }
    let normalized = parsed.host_str()?;
    if normalized.is_empty() {
        return None;
    }
    // WHATWG：file host 为 "localhost"（大小写不敏感）时归一为空 host。
    if normalized.eq_ignore_ascii_case("localhost") {
        return Some(String::new());
    }
    Some(normalized.to_string())
}

/// WHATWG 只把**路径首段**的 Windows 盘符里的 `|` 归一为 `:`。
fn normalize_file_drive_letter(path: &mut String) {
    let Some(rest) = path.strip_prefix('/') else {
        return;
    };
    let end = rest.find('/').unwrap_or(rest.len());
    let segment = &rest[..end];
    if is_windows_drive_letter_text(segment) && segment.as_bytes()[1] == b'|' {
        let mut normalized = String::with_capacity(path.len());
        normalized.push('/');
        normalized.push_str(&rest[..1]);
        normalized.push(':');
        normalized.push_str(&rest[2..]);
        *path = normalized;
    }
}

/// 复制 JSON 值并移除可能包含凭据的内容（JSON 往返语义）。
///
/// TS: `bug-report.ts#redactJsonValue`。键名敏感时值变 `<redacted>`（**先于**字符串 URL
/// 处理）；所有字符串值经过 [`redact_url`]。
///
/// 递归规则见 `redact_value_at_key`：敏感键的**整棵子树**被替换（值为 `null` 时保留
/// `null`），数组元素以下标字符串作为键（下标不会命中敏感词）。最后的
/// `stringify` → `parse` 往返复现 `JSON.parse(JSON.stringify(...))`：数字按 JS 规则
/// 归一化（`-0` → `0` 等），对象按 JS 键序重排（整数索引键升序在前）。
pub fn redact_json_value(value: &Value) -> Value {
    let redacted = redact_value_at_key("", value);
    // TS 是 `JSON.parse(JSON.stringify(...))`：数字按 JS 规则归一化（`-0` → `0` 等）。
    match kiana_json::js_object::stringify(&redacted)
        .parse::<Value>()
        .ok()
    {
        Some(normalized) => normalized,
        None => redacted,
    }
}

/// TS `JSON.stringify` 的 replacer 语义：每一层的键都参与敏感词判断；键敏感且值不是
/// `null` 时整棵子树被替换。数组元素用下标字符串作键（下标不命中敏感词）。
fn redact_value_at_key(key: &str, value: &Value) -> Value {
    if !value.is_null() && is_sensitive_key(key) {
        return Value::String(REDACTED.to_string());
    }
    match value {
        Value::String(text) => Value::String(redact_url(text)),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .enumerate()
                .map(|(index, item)| redact_value_at_key(&index.to_string(), item))
                .collect(),
        ),
        Value::Object(map) => {
            let mut redacted = Map::new();
            for (child_key, child) in map {
                redacted.insert(child_key.clone(), redact_value_at_key(child_key, child));
            }
            Value::Object(redacted)
        }
        other => other.clone(),
    }
}

/// TS: `bug-report.ts#redactSettings`。
///
/// 先序列化成 JSON 对象，再剔除 `trackingId` / `deviceId`（TS 用解构丢弃，这两个字段
/// 完全不出现在报告里），剩余字段交给 [`redact_json_value`]。`shift_remove` 保证其余
/// 键保持原顺序。序列化失败时退化为 `Value::Null`（防御分支）。
fn redact_settings(settings: &Settings) -> Value {
    let mut value = serde_json::to_value(settings).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut value {
        map.shift_remove("trackingId");
        map.shift_remove("deviceId");
    }
    redact_json_value(&value)
}

/// `process.platform` 的等价物。
///
/// 输出使用 Node 的命名（`darwin`/`win32`/…），让报告字段与 TS 版本在同一个平台上
/// 取值一致；`std::env::consts::OS` 未列出的值原样返回。
fn node_platform() -> &'static str {
    match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        "freebsd" => "freebsd",
        "openbsd" => "openbsd",
        "netbsd" => "netbsd",
        "android" => "android",
        "ios" => "darwin",
        other => other,
    }
}

/// `process.arch` 的等价物。
///
/// 输出使用 Node 的命名（`x64`/`arm64`/`ia32`/…）；未列出的架构原样返回。
fn node_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        "x86" => "ia32",
        "arm" => "arm",
        "powerpc" => "ppc",
        "powerpc64" => "ppc64",
        "s390x" => "s390x",
        "riscv64" => "riscv64",
        "loongarch64" => "loong64",
        other => other,
    }
}

/// 去掉行终止符（`\n`，以及其前的 `\r`）。`os.release()` / `os.version()` 返回 utsname
/// 原值：内核 version 末尾的空格必须保留（不能按 JS `trim` 处理）。
#[cfg(not(windows))]
fn strip_line_terminator(text: &str) -> &str {
    let text = text.strip_suffix('\n').unwrap_or(text);
    text.strip_suffix('\r').unwrap_or(text)
}

/// `os.release()`：优先 `/proc/sys/kernel/osrelease`，否则 `uname -r`。
#[cfg(not(windows))]
fn os_release() -> String {
    if let Ok(content) = std::fs::read_to_string("/proc/sys/kernel/osrelease") {
        return strip_line_terminator(&content).to_string();
    }
    uname_field("-r")
}

/// `os.version()`：优先 `/proc/sys/kernel/version`，否则 `uname -v`。
#[cfg(not(windows))]
fn os_version() -> String {
    if let Ok(content) = std::fs::read_to_string("/proc/sys/kernel/version") {
        return strip_line_terminator(&content).to_string();
    }
    uname_field("-v")
}

/// `os.release()` / `os.version()` 的兜底：执行 `uname -r` / `uname -v`。
///
/// 只有退出码为 `Some(0)` 时采用 stdout；进程启动失败或非零退出都返回空串（stderr
/// 被丢弃）。调用方把结果原样写进报告，不会因此报错。
#[cfg(not(windows))]
fn uname_field(flag: &str) -> String {
    let options = kiana_core::child_process::SpawnSyncOptions {
        stdout: kiana_core::child_process::StdioMode::Pipe,
        stderr: kiana_core::child_process::StdioMode::Ignore,
        ..Default::default()
    };
    match kiana_core::child_process::spawn_process_sync("uname", &[flag.to_string()], &options) {
        result if result.status == Some(0) => strip_line_terminator(&result.stdout).to_string(),
        _ => String::new(),
    }
}

/// Windows 上 `os.release()` / `os.version()` 的等价物。
///
/// Node 走 libuv 的 `uv_os_uname`（`src/win/util.c`）：
/// - `release` 是 `RtlGetVersion` 的 `major.minor.build`（Windows 10/11 形如
///   `10.0.22631`），与 `uname -r` 完全无关；
/// - `version` 是注册表 `HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion`
///   的 `ProductName`（64 位视图），Windows 11（build >= 22000）把前缀
///   `"Windows 10"` 改成 `"Windows 11"`；若 `RtlGetVersion` 的
///   `szCSDVersion` 非空，再以空格追加。
#[cfg(windows)]
mod windows_os {
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{
        HKEY_LOCAL_MACHINE, KEY_QUERY_VALUE, KEY_WOW64_64KEY, RRF_RT_REG_SZ, RegCloseKey,
        RegGetValueW, RegOpenKeyExW,
    };
    use windows_sys::Win32::System::SystemInformation::OSVERSIONINFOW;

    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn RtlGetVersion(version_info: *mut OSVERSIONINFOW) -> i32;
    }

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// 调用 `RtlGetVersion` 填充版本结构体。
    ///
    /// 调用前必须先写入 `dwOSVersionInfoSize`（由该 API 校验结构体版本）；返回值被
    /// 忽略，函数总是返回结构体当前内容（失败时不会额外报错，调用方看到的是未写入的值）。
    fn os_version_info() -> OSVERSIONINFOW {
        let mut info = OSVERSIONINFOW::default();
        info.dwOSVersionInfoSize = std::mem::size_of::<OSVERSIONINFOW>() as u32;
        // SAFETY: `RtlGetVersion` 只会写入这个按引用传入的结构体；size 字段已初始化。
        let _status = unsafe { RtlGetVersion(&mut info) };
        info
    }

    /// 从注册表 64 位视图读取 `ProductName`；打开键或读取失败返回 `None`。
    ///
    /// 成功时只取缓冲区里第一个 NUL 之前的 UTF-16 码元；随后按 libuv 的 Windows 11
    /// workaround，用 `info` 的 major/build 判断是否需要把 `"Windows 10"` 改成
    /// `"Windows 11"`。
    fn product_name(info: &OSVERSIONINFOW) -> Option<String> {
        let subkey = wide(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion");
        let mut key = std::ptr::null_mut();
        // SAFETY: 字符串以 NUL 结尾且在整个调用期间有效；`key` 是有效的出参。
        let status = unsafe {
            RegOpenKeyExW(
                HKEY_LOCAL_MACHINE,
                subkey.as_ptr(),
                0,
                KEY_QUERY_VALUE | KEY_WOW64_64KEY,
                &mut key,
            )
        };
        if status != ERROR_SUCCESS {
            return None;
        }
        let value_name = wide("ProductName");
        let mut buffer = [0u16; 256];
        let mut size = std::mem::size_of_val(&buffer) as u32;
        // SAFETY: `key` 已打开；`buffer` 在 `size` 字节范围内有效。
        let status = unsafe {
            RegGetValueW(
                key,
                std::ptr::null(),
                value_name.as_ptr(),
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                buffer.as_mut_ptr().cast(),
                &mut size,
            )
        };
        // SAFETY: `key` 由上面的 RegOpenKeyExW 打开，此后不再使用。
        unsafe {
            RegCloseKey(key);
        }
        if status != ERROR_SUCCESS {
            return None;
        }
        let length = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
        let mut product = String::from_utf16_lossy(&buffer[..length]);
        // libuv 的 Windows 11 workaround：ProductName 仍是 "Windows 10 *"。
        if info.dwMajorVersion == 10
            && info.dwBuildNumber >= 22000
            && product.starts_with("Windows 10")
        {
            product.replace_range(9..10, "1");
        }
        Some(product)
    }

    /// `szCSDVersion`（Service Pack 文本）中第一个 NUL 之前的副本；为空时返回 `None`。
    fn csd_version(info: &OSVERSIONINFOW) -> Option<String> {
        let length = info
            .szCSDVersion
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(info.szCSDVersion.len());
        if length == 0 {
            None
        } else {
            Some(String::from_utf16_lossy(&info.szCSDVersion[..length]))
        }
    }

    pub(super) fn os_release() -> String {
        let info = os_version_info();
        format!(
            "{}.{}.{}",
            info.dwMajorVersion, info.dwMinorVersion, info.dwBuildNumber
        )
    }

    pub(super) fn os_version() -> String {
        let info = os_version_info();
        let mut version = product_name(&info).unwrap_or_default();
        if let Some(csd) = csd_version(&info) {
            if !version.is_empty() {
                version.push(' ');
            }
            version.push_str(&csd);
        }
        version
    }
}

#[cfg(windows)]
fn os_release() -> String {
    windows_os::os_release()
}

#[cfg(windows)]
fn os_version() -> String {
    windows_os::os_version()
}

/// `process.env[name] || null`。
fn env_non_empty(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

/// 收集 Bug 报告的环境信息。TS: `bug-report.ts#collectEnvironment`。
///
/// 返回对象的键序（即 JSON 输出顺序，测试锁定）：
/// `version`、`userAgent`、`runtime`、`platform`、`arch`、`osRelease`、`osVersion`、
/// `shell`、`terminal`、`kianaEnvironmentVariables`。
///
/// - `runtime`：`bun/<ver>` 或 `node/<ver>`；探测不到时用 `node/unknown`；
/// - `platform` / `arch`：Node 命名（见 `node_platform` / `node_arch`）；
/// - `shell`：`SHELL` 的最后一个路径段（`\` 与 `/` 都算分隔符），空值 → `null`；
/// - `terminal`：`term`/`program`/`programVersion`/`colorterm` 为字符串或 `null`，
///   `tmux`/`ssh`/`ci` 由对应环境变量是否非空得到布尔值；
/// - `osRelease` / `osVersion`：Unix 读 `/proc/sys/kernel/osrelease` 与
///   `/proc/sys/kernel/version`，失败时退回 `uname -r` / `uname -v`；Windows 走
///   `RtlGetVersion` 与注册表 `ProductName`；
/// - `kianaEnvironmentVariables`：只收集 `KIANA_` 前缀的变量**名**（不含值），按 UTF-16
///   码元升序；无法转成 UTF-8 的名字被跳过。
///
/// 本函数不做 URL 脱敏：这里没有 URL，且变量值不会被收集。
pub fn collect_environment() -> Value {
    let runtime = kiana_ai::utils::identity::runtime_description()
        .unwrap_or_else(|| "node/unknown".to_string());

    let shell = env_non_empty("SHELL").and_then(|shell| {
        shell
            .split(['\\', '/'])
            .next_back()
            .filter(|segment| !segment.is_empty())
            .map(str::to_string)
    });

    let mut kiana_environment_variables: Vec<String> = std::env::vars_os()
        .filter_map(|(name, _)| name.to_str().map(str::to_string))
        .filter(|name| name.starts_with("KIANA_"))
        .collect();
    // JS 的 `Array.prototype.sort()` 默认按 UTF-16 码元比较；Rust 的 `str` 排序是字节序。
    kiana_environment_variables
        .sort_by(|left, right| left.encode_utf16().cmp(right.encode_utf16()));

    let mut terminal = Map::new();
    terminal.insert("term".to_string(), option_string(env_non_empty("TERM")));
    terminal.insert(
        "program".to_string(),
        option_string(env_non_empty("TERM_PROGRAM")),
    );
    terminal.insert(
        "programVersion".to_string(),
        option_string(env_non_empty("TERM_PROGRAM_VERSION")),
    );
    terminal.insert(
        "colorterm".to_string(),
        option_string(env_non_empty("COLORTERM")),
    );
    terminal.insert(
        "tmux".to_string(),
        Value::Bool(env_non_empty("TMUX").is_some()),
    );
    terminal.insert(
        "ssh".to_string(),
        Value::Bool(
            env_non_empty("SSH_CONNECTION").is_some()
                || env_non_empty("SSH_CLIENT").is_some()
                || env_non_empty("SSH_TTY").is_some(),
        ),
    );
    terminal.insert("ci".to_string(), Value::Bool(env_non_empty("CI").is_some()));

    // 插入顺序就是 report.json 里的键序；测试逐键锁定这 10 个键。
    let mut environment = Map::new();
    environment.insert(
        "version".to_string(),
        Value::String(kiana_core::config::VERSION.to_string()),
    );
    environment.insert(
        "userAgent".to_string(),
        Value::String(kiana_core::changelog::get_kiana_user_agent(
            kiana_core::config::VERSION,
        )),
    );
    environment.insert("runtime".to_string(), Value::String(runtime));
    environment.insert(
        "platform".to_string(),
        Value::String(node_platform().to_string()),
    );
    environment.insert("arch".to_string(), Value::String(node_arch().to_string()));
    environment.insert("osRelease".to_string(), Value::String(os_release()));
    environment.insert("osVersion".to_string(), Value::String(os_version()));
    environment.insert("shell".to_string(), option_string(shell));
    environment.insert("terminal".to_string(), Value::Object(terminal));
    environment.insert(
        "kianaEnvironmentVariables".to_string(),
        Value::Array(
            kiana_environment_variables
                .into_iter()
                .map(Value::String)
                .collect(),
        ),
    );
    Value::Object(environment)
}

fn option_string(value: Option<String>) -> Value {
    match value {
        Some(value) => Value::String(value),
        None => Value::Null,
    }
}

/// `describeModel(model)`。TS: `bug-report.ts#describeModel`。
///
/// 输出键序：`provider`、`id`、`name`、`api`、`baseUrl`、`reasoning`、`input`、
/// `contextWindow`、`maxTokens`、`samplingParams`、`compat`、`thinkingLevelMap`、
/// `headerNames`（测试锁定）。
///
/// 脱敏与裁剪：
/// - `baseUrl` 经过 [`redact_url`]；
/// - `samplingParams`、`compat` 经过 [`redact_json_value`]；
/// - `headerNames` 只列出 header 名（排序后），从不包含 header 值；
/// - `reasoning` / `contextWindow` / `maxTokens` 为 `None` 时省略对应键；
/// - `samplingParams` / `compat` / `thinkingLevelMap` 为 `None` 时写成 `null`；
///   `thinkingLevelMap` 按原值序列化（TS 未对它脱敏）。
pub fn describe_model(model: &Model) -> Value {
    let mut object = Map::new();
    object.insert(
        "provider".to_string(),
        Value::String(model.provider.clone()),
    );
    object.insert("id".to_string(), Value::String(model.id.clone()));
    object.insert("name".to_string(), Value::String(model.name.clone()));
    object.insert("api".to_string(), Value::String(model.api.clone()));
    object.insert(
        "baseUrl".to_string(),
        Value::String(redact_url(&model.base_url)),
    );
    if let Some(reasoning) = model.reasoning {
        object.insert("reasoning".to_string(), Value::Bool(reasoning));
    }
    object.insert(
        "input".to_string(),
        serde_json::to_value(&model.input).unwrap_or(Value::Null),
    );
    if let Some(context_window) = model.context_window {
        object.insert(
            "contextWindow".to_string(),
            Value::Number(Number::from(context_window)),
        );
    }
    if let Some(max_tokens) = model.max_tokens {
        object.insert(
            "maxTokens".to_string(),
            Value::Number(Number::from(max_tokens)),
        );
    }
    object.insert(
        "samplingParams".to_string(),
        match &model.sampling_params {
            Some(params) => redact_json_value(&serde_json::to_value(params).unwrap_or(Value::Null)),
            None => Value::Null,
        },
    );
    object.insert(
        "compat".to_string(),
        match &model.compat {
            Some(compat) => redact_json_value(compat),
            None => Value::Null,
        },
    );
    object.insert(
        "thinkingLevelMap".to_string(),
        match &model.thinking_level_map {
            Some(map) => serde_json::to_value(map).unwrap_or(Value::Null),
            None => Value::Null,
        },
    );
    // JS 的 `Object.keys(...).sort()`：按 UTF-16 码元比较，不是 Rust 的字节序。
    let mut header_names: Vec<String> = model
        .headers
        .as_ref()
        .map(|headers| headers.keys().cloned().collect())
        .unwrap_or_default();
    header_names.sort_by(|left, right| left.encode_utf16().cmp(right.encode_utf16()));
    object.insert(
        "headerNames".to_string(),
        Value::Array(header_names.into_iter().map(Value::String).collect()),
    );
    Value::Object(object)
}

/// `describeProvider(modelRuntime, provider)`。TS: `bug-report.ts#describeProvider`。
///
/// 输出键序：`id`、`name`、`baseUrl`、`headerNames`、`authTypes`、`authStatus`、
/// `usingOAuth`、`registeredByExtension`。
///
/// - `baseUrl`：`None` 或空串 → `null`，否则 [`redact_url`]；
/// - `headerNames`：只含 header 名（排序后），不含值；
/// - `authTypes`：按 `api_key`、`oauth` 的固定顺序，仅追加已配置的方式；
/// - `authStatus` / `usingOAuth` 查询自 `ModelRuntime`；`registeredByExtension` 表示该
///   provider 是否出现在扩展注册的 id 列表中。
pub fn describe_provider(
    model_runtime: &ModelRuntime,
    provider: &dyn kiana_ai::models::Provider,
) -> Value {
    let mut auth_types: Vec<Value> = Vec::new();
    let auth = provider.auth();
    if auth.api_key.is_some() {
        auth_types.push(Value::String("api_key".to_string()));
    }
    if auth.oauth.is_some() {
        auth_types.push(Value::String("oauth".to_string()));
    }

    let mut header_names: Vec<String> = provider
        .headers()
        .map(|headers| headers.keys().cloned().collect())
        .unwrap_or_default();
    header_names.sort_by(|left, right| left.encode_utf16().cmp(right.encode_utf16()));

    let mut object = Map::new();
    object.insert("id".to_string(), Value::String(provider.id().to_string()));
    object.insert(
        "name".to_string(),
        Value::String(provider.name().to_string()),
    );
    object.insert(
        "baseUrl".to_string(),
        match provider.base_url().filter(|base_url| !base_url.is_empty()) {
            Some(base_url) => Value::String(redact_url(base_url)),
            None => Value::Null,
        },
    );
    object.insert(
        "headerNames".to_string(),
        Value::Array(header_names.into_iter().map(Value::String).collect()),
    );
    object.insert("authTypes".to_string(), Value::Array(auth_types));
    object.insert(
        "authStatus".to_string(),
        serde_json::to_value(model_runtime.get_provider_auth_status(provider.id()))
            .unwrap_or(Value::Null),
    );
    object.insert(
        "usingOAuth".to_string(),
        Value::Bool(model_runtime.is_using_oauth(provider.id())),
    );
    object.insert(
        "registeredByExtension".to_string(),
        Value::Bool(
            model_runtime
                .get_registered_provider_ids()
                .iter()
                .any(|id| id == provider.id()),
        ),
    );
    Value::Object(object)
}

/// `describeExtension(extension)`。TS: `bug-report.ts#describeExtension`。
///
/// 输出键序：`path`、`source`、`scope`、`origin`、`hidden`。`source` 经过
/// [`redact_url`]（扩展来源可能是带凭据的 git URL）；`hidden` 仅在 `Some(true)` 时为
/// `true`（`None` 与 `Some(false)` 都为 `false`）；`scope` / `origin` 直接序列化枚举，
/// 序列化失败时写 `null`。
pub fn describe_extension(extension: &Extension) -> Value {
    let mut object = Map::new();
    object.insert("path".to_string(), Value::String(extension.path.clone()));
    object.insert(
        "source".to_string(),
        Value::String(redact_url(&extension.source_info.source)),
    );
    object.insert(
        "scope".to_string(),
        serde_json::to_value(extension.source_info.scope).unwrap_or(Value::Null),
    );
    object.insert(
        "origin".to_string(),
        serde_json::to_value(extension.source_info.origin).unwrap_or(Value::Null),
    );
    object.insert(
        "hidden".to_string(),
        Value::Bool(extension.hidden == Some(true)),
    );
    Value::Object(object)
}

/// 扩展加载错误（`extensionErrors` 的元素）。TS: `{ path: string; error: string }`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionError {
    /// 扩展路径。
    pub path: String,
    /// 错误文本。
    pub error: String,
}

/// `collectBugReportMetadata` 的选项。字段与 TS 一一对应。
pub struct CollectBugReportMetadataOptions<'a> {
    /// 预生成的报告 id（缺省用 `uuidv7()`）。
    pub id: Option<String>,
    /// 用户描述。
    pub hint: Option<String>,
    /// 会话 id。
    pub session_id: String,
    /// 工作目录。
    pub cwd: String,
    /// 是否包含会话记录。
    pub include_session: bool,
    /// 是否包含摘要。
    pub include_summary: bool,
    /// 消息数量。
    pub message_count: u64,
    /// 当前模型。
    pub model: Option<&'a Model>,
    /// 模型运行期。
    pub model_runtime: &'a ModelRuntime,
    /// thinking level。
    pub thinking_level: ThinkingLevel,
    /// 已加载扩展。
    pub extensions: &'a [Extension],
    /// 扩展加载错误。
    pub extension_errors: &'a [ExtensionError],
    /// 全局设置。
    pub global_settings: &'a Settings,
    /// 项目设置。
    pub project_settings: &'a Settings,
}

/// Bug 报告元数据（TS 的对象字面量；键序即 JSON 序列化顺序）。
pub type BugReportMetadata = Value;

/// 收集 Bug 报告元数据。TS: `bug-report.ts#collectBugReportMetadata`。
///
/// 步骤：
/// 1. `hint` 按 JS 的 `trim()` 去掉空白，空串规整为 `null`；
/// 2. 用 `model.provider` 从 `model_runtime` 查 provider：没有模型时 `model` 与
///    `provider` 都是 `null`；模型存在但 provider 未注册时只有 `provider` 为 `null`，
///    `model` 仍照常输出（测试 `collect_metadata_matches_the_ts_key_order_and_conditional_cwd`
///    覆盖 `model: None` 的分支）；
/// 3. `id` 缺省用 `uuidv7()`（失败时退化为空串），`createdAt` 用 [`iso_now_millis`]；
/// 4. 按 TS 对象字面量的顺序插入键；`session.cwd` 只在 `include_session` 为真时出现；
///    两份设置都经 `redact_settings`。
///
/// 返回 `Value::Object`，键序即 `report.json` 的键序（测试锁定）。本函数不包含会话正文、
/// 诊断或摘要文本——这些由调用方放进 [`BugReportBundle`]。
pub fn collect_bug_report_metadata(
    options: &CollectBugReportMetadataOptions<'_>,
) -> BugReportMetadata {
    let hint = options
        .hint
        .as_deref()
        .map(kiana_json::js_string::js_trim)
        .filter(|hint| !hint.is_empty())
        .map(str::to_string);

    // provider 仅用于 `describe_provider`：模型存在但 provider 未注册时这里是 `None`。
    let provider = options
        .model
        .and_then(|model| options.model_runtime.get_provider(&model.provider));

    let id = options.id.clone().unwrap_or_else(default_bug_report_id);

    // 插入顺序 = report.json 的键序；字段顺序与 TS 对象字面量一致。
    let mut object = Map::new();
    object.insert(
        "schemaVersion".to_string(),
        Value::Number(Number::from(BUG_REPORT_SCHEMA_VERSION)),
    );
    object.insert("id".to_string(), Value::String(id));
    object.insert("createdAt".to_string(), Value::String(iso_now_millis()));
    object.insert("hint".to_string(), option_string(hint));
    object.insert("environment".to_string(), collect_environment());

    let mut session = Map::new();
    session.insert("id".to_string(), Value::String(options.session_id.clone()));
    session.insert("included".to_string(), Value::Bool(options.include_session));
    session.insert(
        "summaryIncluded".to_string(),
        Value::Bool(options.include_summary),
    );
    session.insert(
        "messageCount".to_string(),
        Value::Number(Number::from(options.message_count)),
    );
    if options.include_session {
        session.insert("cwd".to_string(), Value::String(options.cwd.clone()));
    }
    object.insert("session".to_string(), Value::Object(session));

    object.insert(
        "model".to_string(),
        match options.model {
            Some(model) => describe_model(model),
            None => Value::Null,
        },
    );
    object.insert(
        "provider".to_string(),
        match provider {
            Some(provider) => describe_provider(options.model_runtime, provider.as_ref()),
            None => Value::Null,
        },
    );
    object.insert(
        "thinkingLevel".to_string(),
        serde_json::to_value(options.thinking_level).unwrap_or(Value::Null),
    );
    object.insert(
        "extensions".to_string(),
        Value::Array(options.extensions.iter().map(describe_extension).collect()),
    );
    object.insert(
        "extensionErrors".to_string(),
        Value::Array(
            options
                .extension_errors
                .iter()
                .map(|error| {
                    let mut item = Map::new();
                    item.insert("path".to_string(), Value::String(error.path.clone()));
                    item.insert("error".to_string(), Value::String(error.error.clone()));
                    Value::Object(item)
                })
                .collect(),
        ),
    );

    let mut settings = Map::new();
    settings.insert(
        "global".to_string(),
        redact_settings(options.global_settings),
    );
    settings.insert(
        "project".to_string(),
        redact_settings(options.project_settings),
    );
    object.insert("settings".to_string(), Value::Object(settings));

    Value::Object(object)
}

/// `uuidv7()`（生成失败时退化为空串；TS 会抛错，但此路径不可达）。
fn default_bug_report_id() -> String {
    kiana_ai::utils::uuid::uuidv7(None).unwrap_or_default()
}

/// `new Date().toISOString()`：UTC、毫秒恒 3 位。
pub fn iso_now_millis() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Bug 报告诊断（TS 的对象字面量）。TS: `bug-report.ts#collectBugReportDiagnostics`。
pub type BugReportDiagnostics = Value;

/// 收集失败 assistant turn（不含会话正文）。TS: `bug-report.ts#collectBugReportDiagnostics`。
///
/// 遍历 `session_manager.get_entries()`：
/// - 只处理 `Message` 条目中的**标准** assistant 消息（自定义 assistant 消息没有类型化
///   视图，会被跳过）；
/// - `assistantMessageCount` 统计所有**标准** assistant turn（成功的也计入），`assistant`
///   数组只收录“有问题”的那些：`diagnostics` 非空，或 `stop_reason` 为
///   `error`/`aborted`，或 `error_message` 非空；
/// - `crashes` 逐条复制并去掉 `notified` 字段，其余键序保持不变。
///
/// 输出键序：`schemaVersion`、`sessionId`、`entryCount`、`assistantMessageCount`、
/// `assistant`、`crashes`。条目的 `timestamp` 原样取自会话里的字符串，不再格式化；
/// 条目没有时间戳时写成空串。
///
/// `assistant` 每个元素的键序：`entryId`、`timestamp`、`provider`、`model`、`api`、
/// `stopReason`、`rawStopReason`（仅 `Some` 时）、`errorMessage`（仅 `Some` 时）、
/// `diagnostics`（序列化失败时为空数组）。`rawStopReason` / `errorMessage` 即使为空串
/// 也会写出。
pub fn collect_bug_report_diagnostics(
    session_manager: &dyn ReadonlySessionManager,
    crashes: &[CrashRecord],
) -> BugReportDiagnostics {
    let entries = session_manager.get_entries();
    let mut assistant: Vec<Value> = Vec::new();
    let mut assistant_message_count: u64 = 0;

    for entry in &entries {
        let SessionEntry::Message(message_entry) = entry else {
            continue;
        };
        let Some(message) = message_entry.message.as_assistant() else {
            continue;
        };
        // 计数发生在过滤之前：成功的 turn 也计入 assistantMessageCount，只是不会进入
        // assistant 数组（测试用 2 条消息、1 条失败来锁定这一点）。
        assistant_message_count += 1;

        let diagnostics = message.diagnostics.clone().unwrap_or_default();
        if diagnostics.is_empty()
            && message.stop_reason != kiana_ai::types::StopReason::Error
            && message.stop_reason != kiana_ai::types::StopReason::Aborted
            && message
                .error_message
                .as_deref()
                .unwrap_or_default()
                .is_empty()
        {
            continue;
        }

        let mut item = Map::new();
        item.insert("entryId".to_string(), Value::String(entry.id().to_string()));
        item.insert(
            "timestamp".to_string(),
            Value::String(entry.timestamp().unwrap_or_default().to_string()),
        );
        item.insert(
            "provider".to_string(),
            Value::String(message.provider.clone()),
        );
        item.insert("model".to_string(), Value::String(message.model.clone()));
        item.insert("api".to_string(), Value::String(message.api.clone()));
        item.insert(
            "stopReason".to_string(),
            serde_json::to_value(message.stop_reason).unwrap_or(Value::Null),
        );
        if let Some(raw_stop_reason) = &message.raw_stop_reason {
            item.insert(
                "rawStopReason".to_string(),
                Value::String(raw_stop_reason.clone()),
            );
        }
        if let Some(error_message) = &message.error_message {
            item.insert(
                "errorMessage".to_string(),
                Value::String(error_message.clone()),
            );
        }
        item.insert(
            "diagnostics".to_string(),
            serde_json::to_value(&diagnostics).unwrap_or(Value::Array(Vec::new())),
        );
        assistant.push(Value::Object(item));
    }

    let mut object = Map::new();
    object.insert(
        "schemaVersion".to_string(),
        Value::Number(Number::from(BUG_REPORT_SCHEMA_VERSION)),
    );
    object.insert(
        "sessionId".to_string(),
        Value::String(session_manager.get_session_id().to_string()),
    );
    object.insert(
        "entryCount".to_string(),
        Value::Number(Number::from(entries.len() as u64)),
    );
    object.insert(
        "assistantMessageCount".to_string(),
        Value::Number(Number::from(assistant_message_count)),
    );
    object.insert("assistant".to_string(), Value::Array(assistant));
    object.insert(
        "crashes".to_string(),
        Value::Array(
            crashes
                .iter()
                .map(|crash| {
                    let mut record = crash.as_map().clone();
                    record.shift_remove("notified");
                    Value::Object(record)
                })
                .collect(),
        ),
    );
    Value::Object(object)
}

/// Bug 报告包。TS: `bug-report.ts#BugReportBundle`。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BugReportBundle {
    /// `report.json` 的内容。
    pub metadata: BugReportMetadata,
    /// `diagnostics.json` 的内容。
    pub diagnostics: BugReportDiagnostics,
    /// `session.jsonl` 原文。
    pub session_jsonl: Option<String>,
    /// `summary.md` 文本。
    pub summary: Option<String>,
}

/// 写入会话的自定义条目数据（`BUG_REPORT_CUSTOM_ENTRY_TYPE` 条目）。
/// TS: `bug-report.ts#BugReportSessionEntryData`。
///
/// 字段顺序是序列化契约（serde 按声明顺序输出，测试逐字锁定）：`id`、`createdAt`、
/// `hint`、`sessionIncluded`、`summaryIncluded`、`delivery`、`path`。`path` 只在
/// `delivery === "zip"` 时出现（可选字段）。
///
/// 序列化由 serde 属性锁定：`rename_all = "camelCase"` 产生 `createdAt` /
/// `sessionIncluded` / `summaryIncluded`；`hint` 为 `None` 时写出 `null`；`path` 带
/// `skip_serializing_if`，只在 `Some` 时出现（“仅 zip 携带 path” 是调用方约定，类型本身
/// 不强制）；`delivery` 序列化为 `"zip"` / `"upload"`。测试
/// `session_entry_data_serializes_in_ts_field_order` 逐字锁定输出。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BugReportSessionEntryData {
    /// 报告 id。
    pub id: String,
    /// 创建时间（`toISOString()`）。
    pub created_at: String,
    /// 用户填写的描述（可为 `null`）。
    pub hint: Option<String>,
    /// 是否附带会话记录。
    pub session_included: bool,
    /// 是否附带总结。
    pub summary_included: bool,
    /// 交付方式。
    pub delivery: BugReportDelivery,
    /// ZIP 路径（仅 `delivery === "zip"`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// `BugReportSessionEntryData.delivery`。
///
/// `rename_all = "lowercase"` 决定会话条目里的字面量：`"zip"` / `"upload"`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BugReportDelivery {
    /// `"zip"`。
    Zip,
    /// `"upload"`。
    Upload,
}

/// 上传/打包共用的文件。TS: `bug-report.ts#BugReportFile`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BugReportFile {
    /// 文件名（同时也是 multipart 字段名）。
    pub name: String,
    /// MIME 类型。
    pub content_type: String,
    /// 文本内容。
    pub data: String,
}

/// 打包/上传共用的文件列表（顺序固定）。TS: `bug-report.ts#bugReportFiles`。
///
/// 顺序：`report.json`、`diagnostics.json`（两者都是 `JSON.stringify(v, null, 2)` 的
/// 2 空格缩进加末尾换行），随后是可选的两个文件：
/// - `session.jsonl`：`Some` 时原样写入（空串也会产生文件）；
/// - `summary.md`：缺末尾换行时补一个。
///
/// 前两个的 MIME 是 `application/json`，会话是 `application/x-ndjson`，摘要是
/// `text/markdown`；`BugReportFile::name` 同时用作 multipart 字段名与文件名。
pub fn bug_report_files(bundle: &BugReportBundle) -> Vec<BugReportFile> {
    let mut files = vec![
        BugReportFile {
            name: "report.json".to_string(),
            content_type: "application/json".to_string(),
            data: format!(
                "{}\n",
                kiana_json::js_object::stringify_pretty(&bundle.metadata, 2)
            ),
        },
        BugReportFile {
            name: "diagnostics.json".to_string(),
            content_type: "application/json".to_string(),
            data: format!(
                "{}\n",
                kiana_json::js_object::stringify_pretty(&bundle.diagnostics, 2)
            ),
        },
    ];
    if let Some(session_jsonl) = &bundle.session_jsonl {
        files.push(BugReportFile {
            name: "session.jsonl".to_string(),
            content_type: "application/x-ndjson".to_string(),
            data: session_jsonl.clone(),
        });
    }
    if let Some(summary) = &bundle.summary {
        files.push(BugReportFile {
            name: "summary.md".to_string(),
            content_type: "text/markdown".to_string(),
            data: if summary.ends_with('\n') {
                summary.clone()
            } else {
                format!("{summary}\n")
            },
        });
    }
    files
}

/// 写出 ZIP 包（不创建父目录）。TS: `bug-report.ts#writeBugReportArchive`。
///
/// 归档先在内存中完整组装，再一次性 `std::fs::write`：父目录不存在时返回
/// `io::ErrorKind::NotFound`；目标已存在时被截断覆盖。条目顺序与内容由
/// [`bug_report_files`] 决定，ZIP 的编码细节见 `kiana_core::zip` 的模块文档。
pub fn write_bug_report_archive(bundle: &BugReportBundle, file_path: &Path) -> std::io::Result<()> {
    let entries: Vec<ZipEntry> = bug_report_files(bundle)
        .into_iter()
        .map(|file| ZipEntry {
            name: file.name,
            data: file.data.into_bytes(),
        })
        .collect();
    write_zip_archive(file_path, &entries)
}

/// `kiana-bug-report-${id}.zip`。TS: `bug-report.ts#bugReportArchiveFileName`。
pub fn bug_report_archive_file_name(id: &str) -> String {
    format!("kiana-bug-report-{id}.zip")
}

/// 摘要 system prompt（逐字）。TS: `bug-report.ts#BUG_SUMMARY_SYSTEM_PROMPT`。
pub const BUG_SUMMARY_SYSTEM_PROMPT: &str = "You are helping a user file a bug report about kiana, the coding agent they are talking to. You will be shown the conversation transcript. Write a report for the kiana developers describing what the user was doing and what went wrong.

Do NOT continue the conversation. Do NOT respond to any questions in the conversation. ONLY output the report.";

/// 摘要指令（逐字）。TS: `bug-report.ts#BUG_SUMMARY_INSTRUCTIONS`。
pub const BUG_SUMMARY_INSTRUCTIONS: &str = "Write the bug report in Markdown with these sections:

## What the user was doing
One short paragraph.

## What went wrong
Concrete description of the failure: wrong output, errors, hangs, tool failures, unexpected behavior. Quote error messages and tool output verbatim where they exist.

## Steps to reproduce
Numbered list, as specific as the transcript allows.

## Relevant details
Tool calls involved, files touched, model behavior, anything else that helps a developer reproduce or locate the problem.

Do not include file contents, secrets, or credentials from the transcript; refer to files by path only. Keep the report factual and concise.";

/// 从最新消息向前挑选消息，直到超出 token 预算（至少选 1 条）。
///
/// TS: `bug-report.ts#selectMessages`。
///
/// 从后往前累加 `estimate_tokens_local` 的结果：已选至少 1 条、且加入下一条会超过
/// `token_budget` 时停止。因此最新一条即使单独超预算也会被选中。预算或某条消息的估算
/// 为 `NaN` 时，`tokens + next > token_budget` 恒为 false，会一直选到最旧一条。
/// 返回按原顺序（旧 → 新）排列的克隆。
pub fn select_messages(messages: &[AgentMessage], token_budget: f64) -> Vec<AgentMessage> {
    let mut selected: Vec<AgentMessage> = Vec::new();
    let mut tokens = 0.0f64;
    for message in messages.iter().rev() {
        let next = estimate_tokens_local(message);
        if !selected.is_empty() && tokens + next > token_budget {
            break;
        }
        selected.push(message.clone());
        tokens += next;
    }
    selected.reverse();
    selected
}

/// `Math.ceil(utf16_length / 4)`。
fn ceil_div4(chars: f64) -> f64 {
    (chars / 4.0).ceil()
}

fn utf16_len(text: &str) -> f64 {
    text.encode_utf16().count() as f64
}

/// JSON.stringify 后的字符数（UTF-16）。
fn json_stringify_len(value: &Value) -> f64 {
    utf16_len(&safe_json_stringify(value))
}

/// 一个图片块估算的字符数。TS: `compaction/compaction.ts#ESTIMATED_IMAGE_CHARS`。
const ESTIMATED_IMAGE_CHARS: f64 = 4800.0;

/// JS 真值判断。
fn js_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(number) => number.as_f64().is_some_and(|number| number != 0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

/// JS 读 `.length` 再按 `ToNumber` 转换（`undefined` → `NaN`）。
fn js_length_number(value: Option<&Value>) -> f64 {
    match value {
        None | Some(Value::Null) => f64::NAN,
        Some(Value::String(text)) => utf16_len(text),
        Some(Value::Array(items)) => items.len() as f64,
        Some(Value::Object(object)) => match object.get("length") {
            Some(length) => js_number_value(length),
            None => f64::NAN,
        },
        Some(Value::Number(_)) | Some(Value::Bool(_)) => f64::NAN,
    }
}

/// JS `String(value)` / `Array.prototype.join(",")`（`kiana-compaction` 的同一辅助）。
fn js_value_to_string(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => "null".to_string(),
        Value::Bool(value) => value.to_string(),
        Value::Number(_) => safe_json_stringify(value),
        Value::Array(items) => items
            .iter()
            .map(|item| match item {
                Value::Null => String::new(),
                other => js_value_to_string(other),
            })
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(_) => "[object Object]".to_string(),
    }
}

/// JS `Number(value)`；无法转换时 `NaN`。
fn js_number_value(value: &Value) -> f64 {
    match value {
        Value::Null => 0.0,
        Value::Bool(true) => 1.0,
        Value::Bool(false) => 0.0,
        Value::Number(number) => number.as_f64().unwrap_or(f64::NAN),
        Value::String(text) => kiana_json::parse_js_number(text),
        Value::Array(_) => kiana_json::parse_js_number(&js_value_to_string(value)),
        Value::Object(_) => f64::NAN,
    }
}

/// TS `estimateTextAndImageContentChars(content)` 的数值版本（保留 `NaN`）。
///
/// 字符串 → UTF-16 长度；数组 → `text` 块加 `text.length`、`image` 块加
/// [`ESTIMATED_IMAGE_CHARS`]；其余形态在 TS 里抛 `TypeError`，这里用 `NaN` 近似。
fn json_content_chars_f64(content: Option<&Value>) -> f64 {
    match content {
        Some(Value::String(text)) => utf16_len(text),
        Some(Value::Array(blocks)) => {
            let mut chars = 0.0;
            for block in blocks {
                match block {
                    Value::Object(object) => {
                        let kind = object
                            .get("type")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        if kind == "text" {
                            if let Some(text) = object.get("text")
                                && js_truthy(text)
                            {
                                chars += js_length_number(Some(text));
                            }
                        } else if kind == "image" {
                            chars += ESTIMATED_IMAGE_CHARS;
                        }
                    }
                    Value::Null => return f64::NAN,
                    _ => {}
                }
            }
            chars
        }
        _ => f64::NAN,
    }
}

/// `system.sections` 的字符数（`Object.values` + 真值项）。
fn add_sections_chars(chars: &mut f64, sections: Option<&Value>) {
    match sections {
        Some(Value::Object(sections)) => {
            for section in sections.values() {
                add_section_chars(chars, section);
            }
        }
        Some(Value::Array(items)) => {
            for section in items {
                add_section_chars(chars, section);
            }
        }
        Some(Value::String(text)) => *chars += utf16_len(text),
        _ => {}
    }
}

/// `if (section) chars += section.length`。
fn add_section_chars(chars: &mut f64, section: &Value) {
    if js_truthy(section) {
        *chars += js_length_number(Some(section));
    }
}

/// typed user 内容块的字符数（文本按 UTF-16、图片固定 4800）。
fn user_blocks_chars(blocks: &[UserContentBlock]) -> f64 {
    blocks
        .iter()
        .map(|block| match block {
            UserContentBlock::Text(text) => utf16_len(&text.text),
            UserContentBlock::Image(_) => ESTIMATED_IMAGE_CHARS,
        })
        .sum()
}

/// typed `string | (Text|Image)[]` 内容的字符数。
fn user_content_chars(content: &UserContent) -> f64 {
    match content {
        UserContent::Text(text) => utf16_len(text),
        UserContent::Blocks(blocks) => user_blocks_chars(blocks),
    }
}

/// typed system 消息的字符数（内容 + sections + `JSON.stringify(toolsAdded)`）。
fn system_message_chars(system: &kiana_ai::types::SystemMessage) -> f64 {
    let mut chars = match &system.content {
        kiana_ai::types::SystemContent::Text(text) => utf16_len(text),
        kiana_ai::types::SystemContent::Blocks(blocks) => {
            blocks.iter().map(|block| utf16_len(&block.text)).sum()
        }
    };
    if let Some(sections) = &system.sections {
        for section in sections.values().flatten() {
            chars += utf16_len(section);
        }
    }
    if let Some(tools) = &system.tools_added {
        chars += json_stringify_len(&serde_json::to_value(tools).unwrap_or(Value::Null));
    }
    chars
}

/// 本地 `estimateTokens`：与 `kiana-compaction` 的公开语义一致（含 `NaN` 传播）。
///
/// TS: `compaction/compaction.ts#estimateTokens`。
///
/// 返回 `f64` 而不是整数，是为了保留 JS 的 `NaN`：`select_messages` 的预算比较在
/// `NaN` 下为 false，从而继续向更旧的消息选。TS 在畸形自定义消息上读 `.length` 会抛
/// `TypeError`，这里用 `NaN` 近似（不复制崩溃）；与 `kiana-compaction` 的
/// `estimate_tokens_f64` 逐分支一致。
fn estimate_tokens_local(message: &AgentMessage) -> f64 {
    // 角色分派：system / user / assistant / custom|toolResult / bashExecution /
    // branchSummary|compactionSummary；其余角色（含缺 `role` 的自定义消息）计 0。
    match message.role() {
        "system" => match message {
            AgentMessage::Standard(Message::System(system)) => {
                ceil_div4(system_message_chars(system))
            }
            AgentMessage::Custom(object) => {
                let mut chars = json_content_chars_f64(object.get("content"));
                add_sections_chars(&mut chars, object.get("sections"));
                if let Some(tools_added) = object.get("toolsAdded")
                    && js_truthy(tools_added)
                {
                    chars += json_stringify_len(tools_added);
                }
                ceil_div4(chars)
            }
            _ => 0.0,
        },
        "user" => match message {
            AgentMessage::Standard(Message::User(user)) => {
                ceil_div4(user_content_chars(&user.content))
            }
            AgentMessage::Custom(object) => {
                ceil_div4(json_content_chars_f64(object.get("content")))
            }
            _ => 0.0,
        },
        "assistant" => match message {
            AgentMessage::Standard(Message::Assistant(assistant)) => {
                let mut chars = 0.0;
                for block in &assistant.content {
                    match block {
                        AssistantContentBlock::Text(text) => chars += utf16_len(&text.text),
                        AssistantContentBlock::Thinking(thinking) => {
                            chars += utf16_len(&thinking.thinking)
                        }
                        AssistantContentBlock::ToolCall(call) => {
                            chars += utf16_len(&call.name);
                            chars += json_stringify_len(&Value::Object(
                                call.arguments
                                    .iter()
                                    .map(|(key, value)| (key.clone(), value.clone()))
                                    .collect(),
                            ));
                        }
                    }
                }
                ceil_div4(chars)
            }
            AgentMessage::Custom(object) => {
                // `for (const block of assistant.content)`：字符串逐字符迭代（无 `.type`，计 0）；
                // 缺失/null/数字等抛 `TypeError`（这里用 `NaN` 近似）。
                let blocks = match object.get("content") {
                    Some(Value::Array(blocks)) => blocks,
                    Some(Value::String(_)) => return 0.0,
                    _ => return f64::NAN,
                };
                let mut chars = 0.0;
                for block in blocks {
                    let Value::Object(block) = block else {
                        if matches!(block, Value::Null) {
                            return f64::NAN;
                        }
                        continue;
                    };
                    match block.get("type").and_then(Value::as_str) {
                        Some("text") => chars += js_length_number(block.get("text")),
                        Some("thinking") => chars += js_length_number(block.get("thinking")),
                        Some("toolCall") => {
                            chars += js_length_number(block.get("name"));
                            match block.get("arguments") {
                                Some(arguments) => {
                                    chars += json_stringify_len(arguments);
                                }
                                None => return f64::NAN,
                            }
                        }
                        _ => {}
                    }
                }
                ceil_div4(chars)
            }
            _ => 0.0,
        },
        "custom" | "toolResult" => match message {
            AgentMessage::Standard(Message::ToolResult(result)) => {
                ceil_div4(user_blocks_chars(&result.content))
            }
            AgentMessage::Custom(object) => {
                ceil_div4(json_content_chars_f64(object.get("content")))
            }
            _ => 0.0,
        },
        "bashExecution" => match message {
            AgentMessage::Custom(object) => ceil_div4(
                js_length_number(object.get("command")) + js_length_number(object.get("output")),
            ),
            _ => 0.0,
        },
        "branchSummary" | "compactionSummary" => match message {
            AgentMessage::Custom(object) => ceil_div4(js_length_number(object.get("summary"))),
            _ => 0.0,
        },
        _ => 0.0,
    }
}

/// 摘要中工具结果的最大字符数。TS: `compaction/utils.ts#TOOL_RESULT_MAX_CHARS`。
const TOOL_RESULT_MAX_CHARS: usize = 2000;

/// `truncateForSummary`（UTF-16 码元截断，保留代理对完整性 —— 与 `kiana-compaction` 相同的
/// 文档化偏差）。
///
/// 超出时才截断，并追加 `\n\n[... N more characters truncated]`。`N` 取「原长度 - 上限」
/// 的 UTF-16 码元差；若截断点落在代理对中间，实际输出的码元数会比上限少 1，但标记里的
/// N 仍按原始差值计算。
fn truncate_for_summary(text: &str, max_chars: usize) -> String {
    let length = text.encode_utf16().count();
    if length <= max_chars {
        return text.to_string();
    }
    let truncated_chars = length - max_chars;
    let mut out = String::new();
    let mut units = 0;
    for character in text.chars() {
        let char_units = character.len_utf16();
        if units + char_units > max_chars {
            break;
        }
        units += char_units;
        out.push(character);
    }
    format!("{out}\n\n[... {truncated_chars} more characters truncated]")
}

/// `Object.entries(arguments)` 的 JS 键序（整数索引键升序在前）。
///
/// JS 枚举普通对象时先按数值升序访问数组索引键，再按插入序访问其余键；Rust 的
/// `IndexMap` 只有插入序，因此这里显式重排。
fn js_object_ordered_keys(keys: impl Iterator<Item = String>) -> Vec<String> {
    let mut indices: Vec<(u64, String)> = Vec::new();
    let mut others: Vec<String> = Vec::new();
    for key in keys {
        match js_array_index(&key) {
            Some(index) => indices.push((index, key)),
            None => others.push(key),
        }
    }
    indices.sort_by_key(|(index, _)| *index);
    indices
        .into_iter()
        .map(|(_, key)| key)
        .chain(others)
        .collect()
}

/// JS 规范数组索引：`"0"` … `"4294967294"`，无前导零；其余形式返回 `None`。
///
/// 上界是 `2^32 - 2`（`value < u32::MAX`），与 JS 的数组索引定义一致；长度超过 10 个
/// 字符或含非数字字符的键按普通字符串键处理。
fn js_array_index(key: &str) -> Option<u64> {
    if key.is_empty() || key.len() > 10 || (key.len() > 1 && key.starts_with('0')) {
        return None;
    }
    if !key.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let value: u64 = key.parse().ok()?;
    (value < u64::from(u32::MAX)).then_some(value)
}

/// 把工具参数渲染成 `k=<JSON.stringify(v)>` 并以 `", "` 连接。
///
/// 键按 `Object.entries` 的 JS 顺序输出；参数为空时返回空串。
fn render_tool_arguments(arguments: &JsonObject) -> String {
    let mut pairs: Vec<String> = Vec::new();
    for key in js_object_ordered_keys(arguments.keys().cloned()) {
        if let Some(value) = arguments.get(&key) {
            pairs.push(format!("{key}={}", safe_json_stringify(value)));
        }
    }
    pairs.join(", ")
}

/// 把 LLM 消息序列化为摘要用的文本。
///
/// TS: `compaction/utils.ts#serializeConversation`（与 `kiana-compaction` 的公开函数一致）。
///
/// 输入应先用 `convert_to_llm` 把 `AgentMessage` 转成 `Message`。每条消息产出 0..n 段，
/// 段间用空行（`"\n\n"`）连接：
/// - `[User]: …`：内容为空时整条省略；
/// - `[Assistant]: …`：仅当内容里至少有一个 text 块；多个文本块用 `"\n"` 拼接；
/// - `[Assistant tool calls]: …`：`name(k=<JSON>)` 形式，多个调用用 `"; "` 连接，
///   参数按 JS 对象键序渲染；
/// - `[Tool result]: …`：超过 2000 个 UTF-16 码元时截断并追加省略标记；
/// - system 消息不渲染；只有 thinking、既无 text 也无 toolCall 的 assistant 消息不产段。
pub fn serialize_conversation(messages: &[Message]) -> String {
    let mut parts: Vec<String> = Vec::new();
    for message in messages {
        match message {
            Message::User(user) => {
                let content = content_text_user_content(&user.content, "");
                if !content.is_empty() {
                    parts.push(format!("[User]: {content}"));
                }
            }
            Message::Assistant(assistant) => {
                let mut tool_calls: Vec<String> = Vec::new();
                for block in &assistant.content {
                    if let AssistantContentBlock::ToolCall(call) = block {
                        tool_calls.push(format!(
                            "{}({})",
                            call.name,
                            render_tool_arguments(&call.arguments)
                        ));
                    }
                }
                if assistant
                    .content
                    .iter()
                    .any(|block| matches!(block, AssistantContentBlock::Text(_)))
                {
                    parts.push(format!(
                        "[Assistant]: {}",
                        content_text_assistant(&assistant.content, "\n")
                    ));
                }
                if !tool_calls.is_empty() {
                    parts.push(format!("[Assistant tool calls]: {}", tool_calls.join("; ")));
                }
            }
            Message::ToolResult(result) => {
                let content = content_text_user(&result.content, "");
                if !content.is_empty() {
                    parts.push(format!(
                        "[Tool result]: {}",
                        truncate_for_summary(&content, TOOL_RESULT_MAX_CHARS)
                    ));
                }
            }
            Message::System(_) => {}
        }
    }
    parts.join("\n\n")
}

/// 摘要生成错误。`Display` 逐字等于 TS 的 `error.message`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BugReportSummaryError {
    /// 错误文本。
    pub message: String,
}

impl std::fmt::Display for BugReportSummaryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for BugReportSummaryError {}

impl BugReportSummaryError {
    fn new(message: impl Into<String>) -> Self {
        BugReportSummaryError {
            message: message.into(),
        }
    }
}

/// `generateBugReportSummary` 的选项。TS: `bug-report.ts#GenerateBugReportSummaryOptions`。
///
/// `stream_fn` 为 `None` 时走 `kiana_ai::compat::complete_simple`；`retry` 为 `None`
/// 时不重试；`signal` 为 `None` 时无法通过令牌取消请求。`thinking_level` 为 `None`
/// 表示不设置推理档位（调用方把 `off` 映射为 `None`）。
pub struct GenerateBugReportSummaryOptions<'a> {
    /// 会话消息。
    pub messages: &'a [AgentMessage],
    /// 用户描述。
    pub hint: Option<String>,
    /// 模型。
    pub model: &'a Model,
    /// API key。
    pub api_key: Option<String>,
    /// 请求头。
    pub headers: Option<kiana_ai::types::ProviderHeaders>,
    /// provider 环境变量。
    pub env: Option<kiana_ai::types::ProviderEnv>,
    /// 取消信号。
    pub signal: Option<tokio_util::sync::CancellationToken>,
    /// thinking level。
    pub thinking_level: Option<kiana_ai::types::ThinkingLevel>,
    /// 可选的会话 stream 函数。
    pub stream_fn: Option<StreamFn>,
    /// 重试策略。
    pub retry: Option<RetryPolicy>,
    /// routing session id。
    pub session_id: Option<String>,
}

/// 让会话模型撰写 Bug 报告。
///
/// TS: `bug-report.ts#generateBugReportSummary`。错误文案逐字：
/// - `Bug report summary was cancelled`
/// - `${label} failed: …`
/// - `Bug report summary attempted to call a tool`
/// - `Bug report summary was empty`
///
/// 步骤：
/// 1. 预算 = `floor(context_window * 0.6)`；`context_window` 缺失或 <= 0 时按 128000
///    计算。用 [`select_messages`] 取最近的消息，有裁剪时在 prompt 开头加说明；
/// 2. 用 [`serialize_conversation`] 渲染 `<conversation>`，`hint`（JS trim 后非空）放进
///    `<user-report>`，最后追加 [`BUG_SUMMARY_INSTRUCTIONS`]；system prompt 是
///    [`BUG_SUMMARY_SYSTEM_PROMPT`]；
/// 3. 输出上限为 `min(4096, model.max_tokens)`（缺失或 0 时用 4096）；`reasoning` 只在
///    模型声明支持推理且传入了 thinking level 时设置；
/// 4. 经 `complete_summarization_local`（`complete_simple` 或注入的 `stream_fn`，带重试
///    与取消）请求模型；
/// 5. 错误检查顺序：aborted → Error/Length → 出现 toolCall → 文本为空；成功时返回
///    JS `trim()` 后的文本。
pub async fn generate_bug_report_summary(
    options: GenerateBugReportSummaryOptions<'_>,
) -> Result<String, BugReportSummaryError> {
    let model = options.model;
    let context_window = match model.context_window {
        Some(window) if window > 0 => window,
        _ => 128_000,
    };
    // 预算按上下文窗口的 60% 估算（字符数 / 4 的启发式，不是 provider 的真实 tokenizer）。
    let token_budget = (context_window as f64 * 0.6).floor();
    let messages = select_messages(options.messages, token_budget);
    let hint = options
        .hint
        .as_deref()
        .map(kiana_json::js_string::js_trim)
        .filter(|hint| !hint.is_empty());

    // prompt 各段顺序固定（测试锁定）：可选的行数提示、<conversation>、可选 <user-report>、指令。
    let mut parts: Vec<String> = Vec::new();
    if messages.len() < options.messages.len() {
        parts.push(format!(
            "Note: only the last {} of {} messages are shown.",
            messages.len(),
            options.messages.len()
        ));
    }
    let llm_messages = convert_to_llm(&messages);
    parts.push(format!(
        "<conversation>\n{}\n</conversation>",
        serialize_conversation(&llm_messages)
    ));
    if let Some(hint) = hint {
        parts.push(format!("<user-report>\n{hint}\n</user-report>"));
    }
    parts.push(BUG_SUMMARY_INSTRUCTIONS.to_string());
    let prompt = parts.join("\n\n");

    // TS 的 `Math.min(4096, maxTokens > 0 ? maxTokens : Infinity)`。
    let max_tokens = match model.max_tokens {
        Some(max_tokens) if max_tokens > 0 => max_tokens.min(4096),
        _ => 4096,
    };
    let request_options = SimpleStreamOptions {
        base: StreamOptions {
            max_tokens: Some(max_tokens),
            signal: options.signal.clone(),
            api_key: options.api_key.clone(),
            headers: options.headers.clone(),
            env: options.env.clone(),
            session_id: options.session_id.clone(),
            ..Default::default()
        },
        reasoning: match (model.reasoning, options.thinking_level) {
            (Some(true), Some(level)) => Some(level),
            _ => None,
        },
        ..Default::default()
    };

    // normalize_context 把 systemPrompt 提升为消息流第 0 条的 system 消息，测试据此断言
    // `messages[0].role == "system"`、`messages[1]` 是本次的 user prompt。
    let context = kiana_ai::utils::transcript::normalize_context(&Context {
        system_prompt: Some(BUG_SUMMARY_SYSTEM_PROMPT.to_string()),
        messages: vec![Message::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Blocks(vec![UserContentBlock::Text(TextContent {
                kind_text: TextKind::Text,
                text: prompt,
                text_signature: None,
            })]),
            timestamp: chrono::Utc::now().timestamp_millis(),
        })],
        tools: None,
    });

    let response = complete_summarization_local(
        model,
        context,
        request_options,
        options.stream_fn.clone(),
        options.retry.clone(),
    )
    .await?;

    // 错误优先级固定：取消 → Error/Length → toolCall → 空文本。取消优先，避免被后续错误掩盖。
    if response.stop_reason == kiana_ai::types::StopReason::Aborted {
        return Err(BugReportSummaryError::new(
            "Bug report summary was cancelled",
        ));
    }
    if let Some(failure) = summarization_failure_local(&response, "Bug report summary") {
        return Err(BugReportSummaryError::new(failure));
    }
    if response
        .content
        .iter()
        .any(|block| matches!(block, AssistantContentBlock::ToolCall(_)))
    {
        return Err(BugReportSummaryError::new(
            "Bug report summary attempted to call a tool",
        ));
    }
    let text = kiana_json::js_string::js_trim(&content_text_assistant(&response.content, "\n"))
        .to_string();
    if text.is_empty() {
        return Err(BugReportSummaryError::new("Bug report summary was empty"));
    }
    Ok(text)
}

/// `getSummarizationFailure(response, label)`：`Error` → `${label} failed: ${msg || "Unknown error"}`，
/// `Length` → `${label} failed: generation hit the token cap and the summary is incomplete`。
fn summarization_failure_local(response: &AssistantMessage, label: &str) -> Option<String> {
    match response.stop_reason {
        kiana_ai::types::StopReason::Error => {
            let message = response
                .error_message
                .as_deref()
                .filter(|message| !message.is_empty())
                .unwrap_or("Unknown error");
            Some(format!("{label} failed: {message}"))
        }
        kiana_ai::types::StopReason::Length => Some(format!(
            "{label} failed: generation hit the token cap and the summary is incomplete"
        )),
        _ => None,
    }
}

/// 手工复制 `SimpleStreamOptions`（该类型未实现 `Clone`）。
///
/// `retry_assistant_call` 的 `produce` 是 `FnMut`，每次重试都可能调用一次，因此每次都要
/// 交出独立的 options 副本。
fn clone_simple_stream_options(options: &SimpleStreamOptions) -> SimpleStreamOptions {
    // 字段清单集中在 `kiana_ai::types::SimpleStreamOptions::duplicate`，避免多处拷贝漂移。
    options.duplicate()
}

/// `completeSummarization` 的本地复刻（见模块头部的刻意重复说明）。
///
/// - 关闭 prompt cache（`CacheRetention::None`），与 `kiana-compaction` 的一次性摘要一致；
/// - 补齐 `session_id`：缺失时现生成 `uuidv7`，失败映射成 `BugReportSummaryError`；
/// - `stream_fn` 的失败无法用 `AssistantMessage` 表达，因此写进旁路 `hard_error` 并返回
///   一条 `Aborted` 哨兵消息；中止是终态，重试循环不会再试；
/// - 重试结束后旁路里有错误时优先返回它（硬失败优先于哨兵响应）。
async fn complete_summarization_local(
    model: &Model,
    context: kiana_ai::types::TranscriptContext,
    options: SimpleStreamOptions,
    stream_fn: Option<StreamFn>,
    retry: Option<RetryPolicy>,
) -> Result<AssistantMessage, BugReportSummaryError> {
    let mut request_options = options;
    request_options.base.cache_retention = Some(CacheRetention::None);
    request_options.base.session_id = Some(match request_options.base.session_id.clone() {
        Some(session_id) => session_id,
        None => kiana_ai::utils::uuid::uuidv7(None)
            .map_err(|error| BugReportSummaryError::new(error.to_string()))?,
    });

    let abort_scope = request_options.base.signal.clone().map(AbortScope::from);
    // 旁路错误槽：produce 只能返回 AssistantMessage（重试循环的固定签名），流层错误先写
    // 这里，再用 Aborted 哨兵让循环停下。
    let hard_error: std::sync::Arc<std::sync::Mutex<Option<String>>> =
        std::sync::Arc::new(std::sync::Mutex::new(None));

    let produce_options = clone_simple_stream_options(&request_options);
    let produce_model = model.clone();
    let produce_context = context.clone();
    let produce_stream_fn = stream_fn.clone();
    let produce_hard_error = std::sync::Arc::clone(&hard_error);
    let produce = move || {
        let options = clone_simple_stream_options(&produce_options);
        let model = produce_model.clone();
        let context = produce_context.clone();
        let stream_fn = produce_stream_fn.clone();
        let hard_error = std::sync::Arc::clone(&produce_hard_error);
        async move {
            match stream_fn {
                Some(stream_fn) => match stream_fn(model, context, Some(options)).await {
                    Ok(stream) => match stream.result().await {
                        Ok(message) => message,
                        Err(error) => hard_error_sentinel(&hard_error, error.to_string()),
                    },
                    Err(error) => hard_error_sentinel(&hard_error, error.to_string()),
                },
                None => complete_simple(&model, context.as_context(), options).await,
            }
        }
    };

    let response = retry_assistant_call(produce, retry.as_ref(), abort_scope.as_ref(), None).await;
    let hard = hard_error
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take();
    if let Some(message) = hard {
        return Err(BugReportSummaryError::new(message));
    }
    Ok(response)
}

/// 记录流层硬错误并返回 `Aborted` 哨兵。
///
/// `retry_assistant_call` 把中止视为终态、不会重试；调用方在循环结束后读取旁路，
/// 决定是否返回 `Err`。锁中毒时取回内部值继续使用，不把 panic 传播给调用方。
fn hard_error_sentinel(
    hard_error: &std::sync::Mutex<Option<String>>,
    message: String,
) -> AssistantMessage {
    *hard_error
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(message);
    AssistantMessage {
        stop_reason: kiana_ai::types::StopReason::Aborted,
        ..AssistantMessage::default()
    }
}

/// 上传选项。TS: `bug-report-upload.ts#UploadBugReportOptions`。
#[derive(Debug, Clone, Default)]
pub struct UploadBugReportOptions {
    /// Radius 账户 token（存在时发 `Authorization: Bearer`）。
    pub token: Option<String>,
    /// 取消信号。
    pub signal: Option<tokio_util::sync::CancellationToken>,
    /// 网关地址覆盖（缺省 [`kiana_config::defaults::get_radius_gateway_url`]）。
    pub gateway_url: Option<String>,
}

/// 上传成功的结果。TS: `{ id: string }`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadedBugReport {
    /// 服务端分配的 id。
    pub id: String,
}

/// 上传错误。`Display` 逐字等于 TS 的 `error.message`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BugReportUploadError {
    /// 错误文本。
    pub message: String,
}

impl std::fmt::Display for BugReportUploadError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for BugReportUploadError {}

/// 以 multipart/form-data 上传报告（匿名或归属 Radius 账户）。
///
/// TS: `bug-report-upload.ts#uploadBugReport`。multipart 每个 part 的字段名与文件名都是
/// `file.name`，顺序同 [`bug_report_files`]；请求头
/// `Content-Type: multipart/form-data; boundary=…` 由本函数自行生成并设置（TS 里由
/// `FormData` 自动完成）。
///
/// 步骤：
/// 1. 解析 gateway（缺省 `get_radius_gateway_url()`），用绝对路径拼 `/v1/bug-reports`
///    （会替换 gateway 自带的路径）；解析失败统一报 `Invalid URL`；
/// 2. 手工拼 multipart 请求体：每个 part 的 `name` 与 `filename` 都是
///    [`BugReportFile::name`]，`Content-Type` 取文件类型，boundary 为
///    `----kianaBugReport<uuidv7>`（生成失败时退化为 `----kianaBugReportboundary`）；
/// 3. `token` 非空时加 `Authorization: Bearer …`；请求走进程级共享 HTTP client；
/// 4. 有 `signal` 时用 `select!`（`biased`）等待取消：取消优先，报
///    `This operation was aborted`；传输失败报 `fetch failed`；
/// 5. 成功要求三者同时成立：2xx、JSON 的 `ok` 是布尔 `true`、`bug_report.id` 是字符串；
///    否则按 TS 的 `description || error || statusText || status` 规则生成
///    `Bug report upload failed: …`。`statusText` 用状态码的标准短语
///    （`canonical_reason`），不是服务端原始 reason phrase。
pub async fn upload_bug_report(
    bundle: &BugReportBundle,
    options: UploadBugReportOptions,
) -> Result<UploadedBugReport, BugReportUploadError> {
    let gateway = options
        .gateway_url
        .clone()
        .unwrap_or_else(kiana_config::defaults::get_radius_gateway_url);
    let base = url::Url::parse(&gateway).map_err(|_| BugReportUploadError {
        message: "Invalid URL".to_string(),
    })?;
    // 绝对路径：gateway 自带的路径前缀会被丢弃（等价 `new URL("/v1/bug-reports", gateway)`）。
    let endpoint = base
        .join("/v1/bug-reports")
        .map_err(|_| BugReportUploadError {
            message: "Invalid URL".to_string(),
        })?;

    let boundary = format!(
        "----kianaBugReport{}",
        kiana_ai::utils::uuid::uuidv7(None).unwrap_or_else(|_| "boundary".to_string())
    );
    // 手工构造 multipart：part 之间用 CRLF 分隔，结尾是 `--boundary--`；字段名与文件名
    // 都是 `file.name`。
    let mut body: Vec<u8> = Vec::new();
    for file in bug_report_files(bundle) {
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        body.extend_from_slice(
            format!(
                "Content-Disposition: form-data; name=\"{0}\"; filename=\"{0}\"\r\n",
                file.name
            )
            .as_bytes(),
        );
        body.extend_from_slice(format!("Content-Type: {}\r\n\r\n", file.content_type).as_bytes());
        body.extend_from_slice(file.data.as_bytes());
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());

    let client = kiana_core::http_dispatcher::shared_http_client();
    let mut request = client
        .post(endpoint)
        .header(
            reqwest::header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(body);
    if let Some(token) = options.token.as_deref().filter(|token| !token.is_empty()) {
        request = request.header(reqwest::header::AUTHORIZATION, format!("Bearer {token}"));
    }

    // 取消优先：`biased` 让已经取消的 signal 先于已经就绪的响应返回。整个
    // “发送请求 + 读完响应体”都在 select 内，否则服务端发完响应头后挂住 body 时取消
    // 无法生效（参考实现的 AbortSignal 覆盖 body 消费）。
    let send_and_read = async {
        let response = request.send().await.map_err(|_| BugReportUploadError {
            // undici 的 fetch 传输失败统一是 `TypeError: fetch failed`。
            message: "fetch failed".to_string(),
        })?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        Ok::<_, BugReportUploadError>((status, text))
    };
    let (status, text) = match &options.signal {
        Some(signal) => tokio::select! {
            biased;
            _ = signal.cancelled() => {
                return Err(BugReportUploadError {
                    message: "This operation was aborted".to_string(),
                });
            }
            result = send_and_read => result?,
        },
        None => send_and_read.await?,
    };

    let status_text = status.canonical_reason().unwrap_or_default().to_string();
    let status_code = status.as_u16();
    let ok = status.is_success();
    // 读取响应体失败被当作空串：`json` 随后为 `None`，错误文案回落到 `statusText` /
    // 状态码，因此传输中途断开不会单独报错。
    let json: Option<Value> = serde_json::from_str(&text).ok();

    // 成功条件与 TS 相同：HTTP 2xx、`ok` 严格为布尔 true、`bug_report.id` 为字符串，
    // 三者缺一不可（`"ok": 1` 或数字 id 都算失败，见测试）。
    if ok
        && let Some(json) = &json
        && json.get("ok").and_then(Value::as_bool) == Some(true)
        && let Some(id) = json
            .get("bug_report")
            .and_then(|report| report.get("id"))
            .and_then(Value::as_str)
    {
        return Ok(UploadedBugReport { id: id.to_string() });
    }

    // TS: `json && !json.ok ? json.description || json.error : undefined` —— `ok` 按 JS 真值
    // 判断（`1`/`"x"` 也算成功），`description`/`error` 可以是任意 JSON 值。
    let detail = json.as_ref().and_then(|json| {
        if kiana_json::js_string::is_truthy(json.get("ok")) {
            return None;
        }
        let description = json.get("description");
        if kiana_json::js_string::is_truthy(description) {
            description
        } else {
            json.get("error")
        }
    });
    // TS: `${detail || response.statusText || response.status}` —— `${}` 是 `String(value)`。
    let reason = match detail.filter(|detail| kiana_json::js_string::is_truthy(Some(detail))) {
        Some(detail) => kiana_json::js_string::js_string(Some(detail)),
        None if !status_text.is_empty() => status_text,
        None => status_code.to_string(),
    };
    Err(BugReportUploadError {
        message: format!("Bug report upload failed: {reason}"),
    })
}

#[cfg(test)]
mod redact_url_tests {
    use super::redact_url;

    #[test]
    fn nested_scheme_prefix_is_redacted() {
        assert_eq!(
            redact_url("git:https://user:pass@example.com/x"),
            "git:https://example.com/x"
        );
    }

    #[test]
    fn long_scheme_prefix_chains_do_not_overflow() {
        let mut value = String::new();
        for _ in 0..10_000 {
            value.push_str("a:");
        }
        value.push_str("https://user:pass@example.com/x");
        let redacted = redact_url(&value);
        assert!(redacted.starts_with("a:a:"));
        assert!(redacted.ends_with("https://example.com/x"));
        assert!(!redacted.contains("user:pass"), "{redacted}");
    }

    #[test]
    fn multi_level_scheme_prefixes_are_redacted() {
        assert_eq!(
            redact_url("a:b:https://user:pass@example.com/x"),
            "a:b:https://example.com/x"
        );
    }
}
