//! core/crash-log.ts。
//!
//! 逐字移植 `packages/coding-agent/src/core/crash-log.ts`（170 行）。
//!
//! 记录以 [`CrashRecord`]（保序 JSON 对象）表示：`readCrashLog` 只校验 `timestamp` 与 `message`
//! 是字符串，其余字段原样保留、原样回写（`takeUnnotifiedCrash` 只加 `notified: true`）。
//! 时间戳用 `Date.toISOString()` 等价格式（UTC、毫秒恒 3 位）。
//!
//! # 这个模块解决什么问题
//!
//! 交互进程（`kiana-interactive`）在未捕获异常或致命错误时必须立即退出，用户来不及描述现场。
//! 这里把崩溃摘要追加到 agent 目录下的 `crashes.json`，下次启动时由 `InteractiveMode::run`
//! 调用 [`take_unnotified_crash`] 提示用户运行 `/bug`；`/bug` 的 diagnostics 收集会带上该文件
//! （`kiana_resources::bug_report` 中的 `collect_bug_report_diagnostics`），报告写入会话成功
//! 且诊断里确有崩溃记录时，由 `kiana-interactive` 调用 [`clear_crash_log`] 删除。没有这个文件，
//! 进程重启后就找不到上次崩溃的摘要（stderr 上的输出已随终端滚走）。
//!
//! # 文件格式与不变量
//!
//! - 本模块写出的文件顶层是 JSON 数组，最多 [`MAX_CRASH_RECORDS`] 条，按写入顺序从旧到新排列
//!   （读取时不做截断，外部手工写入的更多条目也会被读入并原样写回）；
//! - 每条是对象，`timestamp` / `message` 必须是字符串，其余键由写入方决定；
//! - `timestamp` 由私有函数 `iso_now_millis` 生成，是 UTC ISO-8601、毫秒恒 3 位、`Z` 结尾；
//! - 写文件用 `kiana_json::js_object::stringify_pretty`（2 空格缩进）并补一个结尾换行；
//! - 读文件用 `kiana_json::js_parse` 加 `normalize_numbers`，复现 `JSON.parse` 的 f64 数字语义。
//!   直接 `serde_json::from_str` 会把大整数保留成 u64，读入再写出就与 TS 不一致；
//! - 读取不剥离 BOM：`JSON.parse` 遇 BOM 抛错，这里同样退回空数组。
//!
//! # 一次典型的写入 / 读取流程
//!
//! 1. [`record_crash`]：读出现有数组（失败视为空）→ 追加新记录 → 只保留末尾
//!    [`MAX_CRASH_RECORDS`] 条 → 整文件重写；写失败返回 `None`。
//! 2. [`take_unnotified_crash`]：从后往前找第一条 `notified` 为假、时间戳可解析且在
//!    [`MAX_AGE_MS`] 内的记录；找到后把**所有**未通知记录标成 `notified: true` 并写回，
//!    返回值仍是标记前的副本。
//! 3. [`clear_crash_log`]：删除文件，错误忽略。
//!
//! # 失败模式
//!
//! 全部静默：文件缺失/不可读/JSON 非法/顶层不是数组时 [`read_crash_log`] 返回空；
//! [`record_crash`] 写失败返回 `None`（调用方据此决定是否显示上报提示）；
//! [`take_unnotified_crash`] 忽略写回失败（下次启动会重复提示，无害）；
//! [`clear_crash_log`] 忽略删除错误。这些函数本身不会因文件内容而 panic。
//!
//! # 与 TS 对齐的细节
//!
//! `notified` 按 JS 真值判断（见私有函数 `js_truthy`），因此 `0`、`""`、`null` 都算未通知，
//! 数字 `1` 算已通知；`Date.parse` 只实现 ECMA-262 Date Time String Format，V8 的 legacy
//! 格式（如 `2026/01/01`、`Jan 1, 2026`）会解析失败，从而被当作过期记录跳过。

use std::collections::HashSet;
use std::path::Path;

use kiana_core::config::VERSION;
use kiana_core::source_info::{SourceInfo, is_synthetic_path};
use serde_json::{Map, Value};

/// 崩溃记录的最大保留条数。TS: `crash-log.ts#MAX_CRASH_RECORDS`。
pub const MAX_CRASH_RECORDS: usize = 5;

/// 崩溃记录的有效期（7 天，毫秒）。TS: `crash-log.ts#MAX_AGE`。
pub const MAX_AGE_MS: i64 = 7 * 24 * 60 * 60 * 1000;

/// 崩溃类型。TS: `crash-log.ts#CrashRecord.kind`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrashKind {
    /// `"uncaught_exception"`。
    UncaughtException,
    /// `"fatal_error"`。
    FatalError,
}

impl CrashKind {
    /// 与 TS 相同的字面量。
    pub fn as_str(self) -> &'static str {
        match self {
            CrashKind::UncaughtException => "uncaught_exception",
            CrashKind::FatalError => "fatal_error",
        }
    }
}

/// 一条崩溃记录：保序 JSON 对象（TS 的 `CrashRecord`）。
///
/// TS 的 `readCrashLog` 不校验除 `timestamp`/`message` 之外的字段，因此这里用原样键值表，
/// 保证读入再写出不会增加或重排字段。
///
/// `#[serde(transparent)]` 让 `serde` 直接把这个 map 当成 JSON 对象序列化，不额外包一层；
/// 但真正写文件时走的是 `kiana_json` 的 JS 兼容 stringify，键序与数字格式按 TS 规则输出。
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct CrashRecord(Map<String, Value>);

impl CrashRecord {
    /// 由键值表构造。
    pub fn from_map(map: Map<String, Value>) -> Self {
        CrashRecord(map)
    }

    /// 底层键值表。
    pub fn as_map(&self) -> &Map<String, Value> {
        &self.0
    }

    /// `timestamp`（只读）。
    pub fn timestamp(&self) -> Option<&str> {
        self.0.get("timestamp").and_then(Value::as_str)
    }

    /// `message`（只读）。
    pub fn message(&self) -> Option<&str> {
        self.0.get("message").and_then(Value::as_str)
    }

    /// `kind`（只读；未校验）。
    pub fn kind(&self) -> Option<&str> {
        self.0.get("kind").and_then(Value::as_str)
    }

    /// JS 真值判断（`!record.notified` / `record.notified ? ...`）。
    pub fn notified(&self) -> bool {
        self.0.get("notified").is_some_and(js_truthy)
    }

    /// 复制并标记 `notified: true`。TS: `{...record, notified: true}`。
    pub fn mark_notified(&self) -> CrashRecord {
        let mut map = self.0.clone();
        map.insert("notified".to_string(), Value::Bool(true));
        CrashRecord(map)
    }
}

/// `join(getAgentDir(), "crashes.json")`；`agent_dir` 缺省用 [`kiana_core::config::get_agent_dir`]。
///
/// 返回 `String` 而不是 `PathBuf`，因为本模块其余 API 都以 `&str` 接收路径。非 UTF-8 的
/// agent 目录经 `to_string_lossy` 转换，非法字节会被替换字符取代。
pub fn crash_log_path(agent_dir: Option<&str>) -> String {
    let agent_dir = match agent_dir {
        Some(dir) => dir.to_string(),
        None => kiana_core::config::get_agent_dir()
            .to_string_lossy()
            .into_owned(),
    };
    std::path::Path::new(&agent_dir)
        .join("crashes.json")
        .to_string_lossy()
        .into_owned()
}

/// 读取崩溃日志；文件缺失、解析失败或顶层不是数组时返回空表。
///
/// TS: `crash-log.ts#readCrashLog`。**不剥离 BOM**（`JSON.parse` 遇 BOM 抛错 → `[]`）。
///
/// 数组中只有“元素是对象且 `timestamp`/`message` 都是字符串”的记录会被保留，其余元素
/// 静默丢弃（不报错、不影响其它记录）。保留下来的对象保留全部原有键，包括未知键和
/// `notified`，这样后续写回不会丢失字段。
pub fn read_crash_log(path: &str) -> Vec<CrashRecord> {
    let Ok(bytes) = std::fs::read(path) else {
        return Vec::new();
    };
    // Node 的 `readFileSync(path, "utf8")` 对非法 UTF-8 使用替换字符。
    let text = String::from_utf8_lossy(&bytes);
    // `JSON.parse`：数字是 f64（大整数按 JS 舍入、`-0` 保留符号）。`serde_json` 会原样
    // 保留 u64 的十进制位，因此这里走 `kiana_json` 的 V8 兼容解析并归一化数字。
    let Ok(value) = kiana_json::js_parse::parse(&text).map(kiana_json::js_parse::normalize_numbers)
    else {
        return Vec::new();
    };
    let Value::Array(records) = value else {
        return Vec::new();
    };
    records
        .into_iter()
        .filter_map(|record| match record {
            Value::Object(map)
                if map.get("timestamp").is_some_and(Value::is_string)
                    && map.get("message").is_some_and(Value::is_string) =>
            {
                Some(CrashRecord(map))
            }
            _ => None,
        })
        .collect()
}

/// 写入崩溃日志：创建父目录，2 空格缩进 + 结尾换行。
///
/// TS: `crash-log.ts#writeCrashLog`。
///
/// 整文件覆盖写（`std::fs::write`，没有临时文件 + rename）。若写到一半失败，磁盘上可能
/// 留下不完整的 JSON，下一次 [`read_crash_log`] 会解析失败并退回空数组。
fn write_crash_log(records: &[CrashRecord], path: &str) -> std::io::Result<()> {
    if let Some(parent) = Path::new(path).parent() {
        std::fs::create_dir_all(parent)?;
    }
    let value = Value::Array(
        records
            .iter()
            .map(|record| Value::Object(record.0.clone()))
            .collect(),
    );
    std::fs::write(
        path,
        format!("{}\n", kiana_json::js_object::stringify_pretty(&value, 2)),
    )
}

/// 扩展 stack 归属所需的最小元数据。TS: `crash-log.ts#ExtensionStackMetadata`。
///
/// 三个字段都参与 [`find_extension_stack_matches`] 的判定：`resolved_path` 用于精确路径或
/// `index.*` 入口匹配，`source_info.base_dir` 用于包根目录匹配，`source_info.origin` 与
/// `source_info.source` 共同决定“单文件包”判定以及最终返回的展示标签。
#[derive(Debug, Clone, PartialEq)]
pub struct ExtensionStackMetadata {
    /// 显示路径（`Extension.path`）。
    pub path: String,
    /// 解析后的真实路径（`Extension.resolvedPath`）。
    pub resolved_path: String,
    /// 来源信息。
    pub source_info: SourceInfo,
}

impl From<&kiana_ext_api::types::Extension> for ExtensionStackMetadata {
    fn from(extension: &kiana_ext_api::types::Extension) -> Self {
        ExtensionStackMetadata {
            path: extension.path.clone(),
            resolved_path: extension.resolved_path.clone(),
            source_info: extension.source_info.clone(),
        }
    }
}

/// `value.replace(/\\/g, "/").replace(/\/+$/u, "")`。
fn normalize_stack_path(value: &str) -> String {
    value.replace('\\', "/").trim_end_matches('/').to_string()
}

/// JS `/\\s/u`（crash log 的边界字符判定）。与 [`char::is_whitespace`] 不同，包含 U+FEFF。
fn js_is_whitespace(character: char) -> bool {
    matches!(
        character,
        '\u{0009}'
            | '\u{000A}'
            | '\u{000B}'
            | '\u{000C}'
            | '\u{000D}'
            | '\u{0020}'
            | '\u{00A0}'
            | '\u{1680}'
            | '\u{2000}'
            ..='\u{200A}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202F}'
                | '\u{205F}'
                | '\u{3000}'
                | '\u{FEFF}'
    )
}

/// `stack.includes(target)` 或带边界字符的 `indexOf`。
///
/// TS: `crash-log.ts#stackContainsPath`。边界字符集合：结尾、`:`、`)`、任意 JS 空白。
///
/// 目标为空串或以 `<` 开头的合成路径（如 `<builtin:...>`）时直接返回 `false`：这类路径
/// 不会出现在 Node 的 stack 里。`include_descendants` 为真时只检查 `target/` 是否作为
/// 子串出现（用于包根/目录匹配）；为假时逐个出现位置检查后继字符，避免 `/a/b` 误匹配
/// `/a/bc`。目标形如 `X:/...`（ASCII 盘符）时整串按小写比较，其余路径区分大小写。
fn stack_contains_path(stack: &str, target_path: &str, include_descendants: bool) -> bool {
    let target = normalize_stack_path(target_path);
    if target.is_empty() || is_synthetic_path(&target) {
        return false;
    }
    // `/^[a-z]:\//iu`：ASCII 盘符（大小写不敏感）。
    let bytes = target.as_bytes();
    let case_insensitive =
        bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'/';
    let haystack = if case_insensitive {
        stack.to_lowercase()
    } else {
        stack.to_string()
    };
    let needle = if case_insensitive {
        target.to_lowercase()
    } else {
        target
    };
    if include_descendants {
        return haystack.contains(&format!("{needle}/"));
    }

    let mut search_from = 0usize;
    while let Some(index) = haystack[search_from..].find(&needle) {
        let index = search_from + index;
        let next = haystack[index + needle.len()..].chars().next();
        match next {
            None => return true,
            Some(character)
                if character == ':' || character == ')' || js_is_whitespace(character) =>
            {
                return true;
            }
            Some(_) => {}
        }
        search_from = index + needle.len();
    }
    false
}

/// 逐行 `decodeURI`：保留保留字符的转义，遇到非法转义/非法 UTF-8 时保留原行。
///
/// `decodeURI` 不解码保留字符 `;,/?:@&=+$#`，因此这些 `%XX` 原样写回（大小写不变）。
/// 只要行内有一个非法 `%` 转义（缺两位十六进制），或解码后的字节不是合法 UTF-8，
/// 整行就原样返回；逐行解码保证错误消息里的坏转义不会影响其它栈帧。
fn decode_uri(line: &str) -> String {
    const RESERVED: &[u8] = b";,/?:@&=+$#";
    let bytes = line.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let valid = index + 2 < bytes.len()
                && bytes[index + 1].is_ascii_hexdigit()
                && bytes[index + 2].is_ascii_hexdigit();
            if !valid {
                return line.to_string();
            }
            let high = (bytes[index + 1] as char).to_digit(16).unwrap_or(0) as u8;
            let low = (bytes[index + 2] as char).to_digit(16).unwrap_or(0) as u8;
            let decoded = (high << 4) | low;
            if RESERVED.contains(&decoded) {
                // decodeURI 不解码保留字符：保留原始 `%XX`（大小写不变）。
                out.extend_from_slice(&bytes[index..index + 3]);
            } else {
                out.push(decoded);
            }
            index += 3;
            continue;
        }
        out.push(bytes[index]);
        index += 1;
    }
    match String::from_utf8(out) {
        Ok(decoded) => decoded,
        Err(_) => line.to_string(),
    }
}

/// 找出 stack 中归属于已加载扩展的包/文件。
///
/// TS: `crash-log.ts#findExtensionStackMatches`。
///
/// 返回去重后的**展示标签**，顺序与 `extensions` 的遍历顺序一致（同一标签只保留首次命中）：
/// 来源为 `package` 且 `source` 非空时用 `source`（如 `npm:foo`），否则用 `path`。
/// `stack` 为 `None` 或没有命中时返回空表。
///
/// 处理分三步：
///
/// 1. 丢弃首行（通常是错误消息，可能含扩展路径，不能作为归属证据），只保留形如
///    `    at ...` 的栈帧；逐帧做 `decodeURI`，再把反斜杠统一成 `/`，使 Windows 路径可比；
/// 2. 为每个扩展选匹配目标：普通包用 `base_dir`（要求 stack 出现其子路径）；`index.*`
///    入口用父目录（同样允许子路径）；其余情况用 `resolved_path` 本身（要求后缀边界）。
///    若包来源的 `source` 本身就是 `.js`/`.ts` 文件（且不是 `npm:`/`git:`/URL 标签），
///    视为单文件包，不用 `base_dir` 匹配，避免把同目录的兄弟扩展一起算进去；
/// 3. 用标签集合去重后返回。
pub fn find_extension_stack_matches(
    stack: Option<&str>,
    extensions: &[ExtensionStackMetadata],
) -> Vec<String> {
    let Some(stack) = stack else {
        return Vec::new();
    };
    // 首行是错误消息（`Error: ...`），可能恰好包含扩展路径，必须排除；其余行只认
    // `^\s+at\s` 形式的栈帧。逐帧解码，坏转义只影响该帧，不会牵连其它行。
    let normalized_stack = stack
        .split('\n')
        .skip(1)
        .filter(|line| is_stack_frame(line))
        .map(decode_uri)
        .collect::<Vec<_>>()
        .join("\n")
        .replace('\\', "/");
    let mut matches: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    for extension in extensions {
        let resolved_path = normalize_stack_path(&extension.resolved_path);
        let source = &extension.source_info.source;
        // `source` 是文件路径（如 `/plugins/a.ts`）而非包标签时，base_dir 可能是整个
        // 插件目录，用它做“子路径”匹配会把同目录的兄弟扩展误判为崩溃来源。
        let single_file_package = extension.source_info.origin
            == kiana_core::source_info::SourceOrigin::Package
            && !(source.starts_with("npm:")
                || source.starts_with("git:")
                || source.starts_with("http://")
                || source.starts_with("https://")
                || source.starts_with("ssh://"))
            && matches_script_extension(source);
        // 只有普通包（非单文件包）且 base_dir 非空时才按包根目录匹配。
        let package_root = if extension.source_info.origin
            == kiana_core::source_info::SourceOrigin::Package
            && !single_file_package
        {
            extension
                .source_info
                .base_dir
                .as_deref()
                .filter(|base_dir| !base_dir.is_empty())
        } else {
            None
        };
        let slash_index = resolved_path.rfind('/');
        let directory_entry = matches_index_entry(&resolved_path);
        // 优先级：包根（匹配任意子路径）→ `index.*` 的父目录（匹配任意子路径）→
        // 精确文件路径（要求边界，防止前缀相同的文件名误报）。
        let matched = if let Some(package_root) = package_root {
            stack_contains_path(&normalized_stack, package_root, true)
        } else if directory_entry {
            match slash_index {
                Some(slash_index) => {
                    stack_contains_path(&normalized_stack, &resolved_path[..slash_index], true)
                }
                None => false,
            }
        } else {
            stack_contains_path(&normalized_stack, &resolved_path, false)
        };
        if !matched {
            continue;
        }

        let label = if extension.source_info.origin
            == kiana_core::source_info::SourceOrigin::Package
            && !source.is_empty()
        {
            source.clone()
        } else {
            extension.path.clone()
        };
        if seen.insert(label.clone()) {
            matches.push(label);
        }
    }
    matches
}

/// `/\.[cm]?[jt]s$/u`。
fn matches_script_extension(value: &str) -> bool {
    let Some(rest) = value.strip_suffix('s') else {
        return false;
    };
    let Some(rest) = rest.strip_suffix(['j', 't']) else {
        return false;
    };
    let rest = rest.strip_suffix(['c', 'm']).unwrap_or(rest);
    rest.ends_with('.')
}

/// `/\/index\.[cm]?[jt]s$/u`。
fn matches_index_entry(value: &str) -> bool {
    let Some(index) = value.rfind("/index.") else {
        return false;
    };
    matches_js_extension(&value[index + "/index.".len()..])
}

/// `[cm]?[jt]s`（完整扩展名）。
fn matches_js_extension(extension: &str) -> bool {
    matches!(extension, "js" | "ts" | "cjs" | "cts" | "mjs" | "mts")
}

/// `/^\s+at\s/u`：至少一个 JS 空白 + `at` + 至少一个 JS 空白。
fn is_stack_frame(line: &str) -> bool {
    let mut characters = line.chars();
    let mut spaces = 0usize;
    while characters.clone().next().is_some_and(js_is_whitespace) {
        characters.next();
        spaces += 1;
    }
    if spaces == 0 {
        return false;
    }
    if !characters.as_str().starts_with("at") {
        return false;
    }
    let mut after = characters.as_str()[2..].chars();
    after.next().is_some_and(js_is_whitespace)
}

/// [`record_crash`] 的输入参数，对应 TS 里传给 `recordCrash` 的 `Error` 与会话上下文。
///
/// `message` 为空串时回退 `error_name`（TS 的 `error.message || error.name`）。
pub struct CrashInput<'a> {
    /// `"uncaught_exception"` / `"fatal_error"`。
    pub kind: CrashKind,
    /// 错误消息（`Error.message`）。
    pub message: &'a str,
    /// 错误名（`Error.name`；非 Error 时为 `None`）。
    pub error_name: Option<&'a str>,
    /// 错误栈（`Error.stack`）。
    pub stack: Option<&'a str>,
    /// 会话文件。
    pub session_file: Option<&'a str>,
    /// 工作目录。
    pub cwd: &'a str,
}

/// 记录一次崩溃；失败时返回 `None`。TS: `crash-log.ts#recordCrash`。
///
/// 记录字段按 TS 顺序写入：`timestamp`、`version`、`kind`、`message`、`stack`、
/// `sessionFile`、`cwd`。`stack` / `sessionFile` 为 `None` 时写 JSON `null` 而不是省略该键，
/// 这样下游读到的字段集合固定。
///
/// 读出现有日志后追加，`saturating_sub` 保证只保留最后 [`MAX_CRASH_RECORDS`] 条（不足时
/// 不会下溢）。返回 `Some` 仅表示整文件重写成功；IO 错误类型不对外区分，调用方只把它当
/// 布尔值用。注意读失败会被当成空日志，因此损坏的旧文件会被本次写入直接覆盖。
pub fn record_crash(crash: CrashInput<'_>, path: &str) -> Option<CrashRecord> {
    let mut record: Map<String, Value> = Map::new();
    record.insert("timestamp".to_string(), Value::String(iso_now_millis()));
    record.insert("version".to_string(), Value::String(VERSION.to_string()));
    record.insert(
        "kind".to_string(),
        Value::String(crash.kind.as_str().to_string()),
    );
    let message = if crash.message.is_empty() {
        crash.error_name.unwrap_or_default()
    } else {
        crash.message
    };
    record.insert("message".to_string(), Value::String(message.to_string()));
    record.insert(
        "stack".to_string(),
        match crash.stack {
            Some(stack) => Value::String(stack.to_string()),
            None => Value::Null,
        },
    );
    record.insert(
        "sessionFile".to_string(),
        match crash.session_file {
            Some(session_file) => Value::String(session_file.to_string()),
            None => Value::Null,
        },
    );
    record.insert("cwd".to_string(), Value::String(crash.cwd.to_string()));
    let record = CrashRecord(record);

    let mut records = read_crash_log(path);
    records.push(record.clone());
    let start = records.len().saturating_sub(MAX_CRASH_RECORDS);
    let records = records[start..].to_vec();
    match write_crash_log(&records, path) {
        Ok(()) => Some(record),
        Err(_) => None,
    }
}

/// 返回最新的一条未通知崩溃，并把**所有**未通知记录标记为已通知。
///
/// TS: `crash-log.ts#takeUnnotifiedCrash`。`Date.parse` 失败（`NaN`）的记录被忽略。
///
/// 查找从数组尾部向前进行（最新优先），跳过 `notified` 为真、时间戳不可解析、
/// 或距 `now` 超过 [`MAX_AGE_MS`] 的记录；`now_ms` 为 `None` 时取当前 UTC 时间。
/// 一旦找到，**即使后续写回失败**也会返回该记录；返回值是标记前的副本，因此调用方看到
/// 的 `notified` 仍是原值（可能缺失或为假）。写回失败被忽略，代价只是下次启动重复提示。
pub fn take_unnotified_crash(path: &str, now_ms: Option<i64>) -> Option<CrashRecord> {
    let now = now_ms.unwrap_or_else(|| chrono::Utc::now().timestamp_millis());
    let records = read_crash_log(path);
    let crash = records
        .iter()
        .rev()
        .find(|record| {
            if record.notified() {
                return false;
            }
            match parse_js_date_ms(record.timestamp().unwrap_or_default()) {
                Some(timestamp) => now.saturating_sub(timestamp) <= MAX_AGE_MS,
                None => false,
            }
        })
        .cloned();
    let crash = crash?;
    let marked: Vec<CrashRecord> = records
        .iter()
        .map(|record| {
            if record.notified() {
                record.clone()
            } else {
                record.mark_notified()
            }
        })
        .collect();
    // 写回失败可以忽略：再次展示提示是无害的。
    let _ = write_crash_log(&marked, path);
    Some(crash)
}

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

/// `Date.parse(value)`（毫秒）；无法解析时 `None`（JS 的 `NaN`）。
///
/// 支持 ECMA-262 的 Date Time String Format（含仅日期的 UTC 形式、无时区的本地时间、
/// 空格分隔符与 `±HH:mm` / `±HHmm` 偏移）。V8 的其它 legacy 格式（如 `2026/01/01`、
/// `Jan 1, 2026`）不在支持范围内（与 `kiana-compaction` 的实现保持一致的已知偏差）。
fn parse_js_date_ms(value: &str) -> Option<i64> {
    parse_iso_date_ms(value)
}

/// 解析 ECMA-262 Date Time String Format。
///
/// - `YYYY`、`YYYY-MM`、`YYYY-MM-DD`：UTC 午夜（仅日期形式不带时区）；
/// - `YYYY-MM-DD[T ]HH:mm[:ss[.fff…]][Z|±HH:mm|±HHmm]`：无偏移时按本地时间；
/// - 支持 `±YYYYYY` 扩展年份、`24:00` 与大小写 `T`/`Z`。
///
/// 解析按阶段推进：年份 → 月/日 → 时间 → 越界归一化 → 时区。任何一段格式不符都返回
/// `None`（等价 JS 的 `NaN`）。输入不做 trim：前后空白会导致失败，与 V8 一致。
fn parse_iso_date_ms(value: &str) -> Option<i64> {
    use chrono::{NaiveDate, TimeZone};

    let bytes = value.as_bytes();
    // 含非 ASCII 字符（或为空）的输入不可能是 ISO 形式。
    if bytes.is_empty() || !value.is_ascii() {
        return None;
    }

    fn digits(bytes: &[u8], start: usize, length: usize) -> Option<u32> {
        if start + length > bytes.len() {
            return None;
        }
        let slice = &bytes[start..start + length];
        if !slice.iter().all(u8::is_ascii_digit) {
            return None;
        }
        Some(
            slice
                .iter()
                .fold(0u32, |acc, byte| acc * 10 + u32::from(byte - b'0')),
        )
    }

    // 阶段 1：年份。普通形式固定 4 位；带符号时是 6 位扩展年份，符号位占 1 字节。
    let (year, mut index): (i32, usize) = if matches!(bytes.first(), Some(b'+') | Some(b'-')) {
        let sign = if bytes[0] == b'-' { -1 } else { 1 };
        let magnitude = digits(bytes, 1, 6)?;
        (sign * magnitude as i32, 7)
    } else {
        (digits(bytes, 0, 4)? as i32, 4)
    };

    // 阶段 2：月/日。只给年或年-月也合法，此时 month/day 保持缺省值 1。
    let mut month = 1u32;
    let mut day = 1u32;
    let mut date_complete = false;
    if index < bytes.len() && bytes[index] == b'-' {
        month = digits(bytes, index + 1, 2)?;
        index += 3;
        if index < bytes.len() {
            if bytes[index] != b'-' {
                return None;
            }
            day = digits(bytes, index + 1, 2)?;
            index += 3;
            date_complete = true;
        }
    }

    // 语法允许 01-12 月、01-31 日；`MakeDay` 会把 2 月 30 日这类日溢出顺延到下个月
    // （V8 对 "2026-02-30" 返回 3 月 2 日），但月份本身越界仍是 invalid。
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let date = NaiveDate::from_ymd_opt(year, month, 1)?
        .checked_add_days(chrono::Days::new(u64::from(day - 1)))?;

    if index == bytes.len() {
        // 仅日期形式按 UTC 解析。`YYYY-MM` / `YYYY` 同样如此（V8 行为）。
        return date
            .and_hms_opt(0, 0, 0)
            .map(|naive| naive.and_utc().timestamp_millis());
    }

    // 日期时间形式必须带完整的 `-DD`。
    if !date_complete {
        return None;
    }

    // 阶段 3：时间。小时与分钟必填，秒与小数秒可选；`T`/`t` 之后或一个及以上空格之后。
    let separator = bytes[index];
    if separator == b'T' || separator == b't' {
        index += 1;
    } else if separator == b' ' {
        while index < bytes.len() && bytes[index] == b' ' {
            index += 1;
        }
    } else {
        return None;
    }

    let hour = digits(bytes, index, 2)?;
    if bytes.get(index + 2) != Some(&b':') {
        return None;
    }
    let minute = digits(bytes, index + 3, 2)?;
    index += 5;

    let mut second = 0u32;
    let mut millisecond = 0u32;
    if index < bytes.len() && bytes[index] == b':' {
        second = digits(bytes, index + 1, 2)?;
        index += 3;
        if index < bytes.len() && bytes[index] == b'.' {
            let fraction_start = index + 1;
            let mut fraction_end = fraction_start;
            while fraction_end < bytes.len() && bytes[fraction_end].is_ascii_digit() {
                fraction_end += 1;
            }
            if fraction_end == fraction_start {
                return None;
            }
            // V8 截断到毫秒：小数秒不足 3 位时在末尾补 0，且绝不越过小数段读取
            // 后面的时区字符（`...00.1Z` 是 100ms，不是 52x ms）。
            for position in 0..3 {
                let digit = if fraction_start + position < fraction_end {
                    u32::from(bytes[fraction_start + position] - b'0')
                } else {
                    0
                };
                millisecond = millisecond * 10 + digit;
            }
            index = fraction_end;
        }
    }

    // 24:00（及 24:00:00[.000]）顺延到次日；其余越界时间无效。
    let mut date = date;
    let mut hour = hour;
    if hour == 24 {
        if minute != 0 || second != 0 || millisecond != 0 {
            return None;
        }
        date = date.succ_opt()?;
        hour = 0;
    } else if hour > 23 || minute > 59 || second > 59 {
        return None;
    }

    let naive = date.and_hms_milli_opt(hour, minute, second, millisecond)?;

    // 阶段 4：时区后缀。空 → 本地时间；`Z`/`z` → UTC；`±HH:mm` / `±HHmm` → 固定偏移。
    let rest = &bytes[index..];
    if rest.is_empty() {
        // 无时区：本地时间。夏令时歧义取较早的时刻。
        return chrono::Local
            .from_local_datetime(&naive)
            .earliest()
            .map(|date| date.timestamp_millis());
    }

    if rest == b"Z" || rest == b"z" {
        return Some(naive.and_utc().timestamp_millis());
    }

    if rest[0] != b'+' && rest[0] != b'-' {
        return None;
    }
    let sign = if rest[0] == b'-' { -1 } else { 1 };
    let offset_hours = digits(rest, 1, 2)?;
    let offset_minutes = if rest.len() == 6 && rest[3] == b':' {
        digits(rest, 4, 2)?
    } else if rest.len() == 5 && rest[3].is_ascii_digit() {
        digits(rest, 3, 2)?
    } else {
        return None;
    };
    if offset_hours > 23 || offset_minutes > 59 {
        return None;
    }
    let offset_seconds = sign * (offset_hours as i32 * 3600 + offset_minutes as i32 * 60);
    let offset = chrono::FixedOffset::east_opt(offset_seconds)?;
    offset
        .from_local_datetime(&naive)
        .single()
        .map(|date| date.timestamp_millis())
}

/// `new Date().toISOString()`。
fn iso_now_millis() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// 删除崩溃日志；异常忽略。TS: `crash-log.ts#clearCrashLog`。
///
/// 文件不存在、权限不足或路径是目录时都不会报告错误，调用方也无法区分是否真的删除了。
/// `kiana-interactive` 的 `/bug` 记录路径在调用后直接返回成功，因此清理失败不会改变
/// 报告本身的结果。
pub fn clear_crash_log(path: &str) {
    let _ = std::fs::remove_file(path);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn date_parse_accepts_iso_date_only_as_utc() {
        // V8: Date.parse("2026-01-01") === 1767225600000
        assert_eq!(parse_js_date_ms("2026-01-01"), Some(1_767_225_600_000));
        assert_eq!(parse_js_date_ms("2026-01"), Some(1_767_225_600_000));
        assert_eq!(parse_js_date_ms("2026"), Some(1_767_225_600_000));
    }

    #[test]
    fn date_parse_normalizes_day_overflow_like_make_day() {
        // V8: Date.parse("2026-02-30") === 1772409600000（2026-03-02T00:00:00Z）
        assert_eq!(parse_js_date_ms("2026-02-30"), Some(1_772_409_600_000));
        assert_eq!(
            parse_js_date_ms("2026-02-30T12:00:00Z"),
            Some(1_772_452_800_000)
        );
    }

    #[test]
    fn date_parse_matches_iso_datetime_forms() {
        assert_eq!(
            parse_js_date_ms("2026-01-01T00:00:00Z"),
            Some(1_767_225_600_000)
        );
        assert_eq!(
            parse_js_date_ms("2026-01-01t00:00:00z"),
            Some(1_767_225_600_000)
        );
        assert_eq!(
            parse_js_date_ms("2026-01-01 00:00:00Z"),
            Some(1_767_225_600_000)
        );
        assert_eq!(
            parse_js_date_ms("2026-01-01T00:00:00+08:00"),
            Some(1_767_196_800_000)
        );
        assert_eq!(
            parse_js_date_ms("2026-01-01T00:00:00+0800"),
            Some(1_767_196_800_000)
        );
        assert_eq!(
            parse_js_date_ms("2026-01-01T00:00:00.123456Z"),
            Some(1_767_225_600_123)
        );
        assert_eq!(
            parse_js_date_ms("2026-01-01T24:00:00Z"),
            Some(1_767_312_000_000)
        );
        assert_eq!(
            parse_js_date_ms("+002026-01-01T00:00:00Z"),
            Some(1_767_225_600_000)
        );
        assert_eq!(
            parse_js_date_ms("-002026-01-01T00:00:00Z"),
            Some(-126_101_577_600_000)
        );
    }

    #[test]
    fn date_parse_pads_short_fractional_seconds() {
        // V8: Date.parse("2026-01-01T00:00:00.1Z") === 1767225600100
        assert_eq!(
            parse_js_date_ms("2026-01-01T00:00:00.1Z"),
            Some(1_767_225_600_100)
        );
        assert_eq!(
            parse_js_date_ms("2026-01-01T00:00:00.12Z"),
            Some(1_767_225_600_120)
        );
        assert_eq!(
            parse_js_date_ms("2026-01-01T00:00:00.1+05:00"),
            Some(1_767_207_600_100)
        );
        assert_eq!(
            parse_js_date_ms("2026-01-01T00:00:00.12-05:00"),
            Some(1_767_243_600_120)
        );
        assert_eq!(
            parse_js_date_ms("2026-01-01T00:00:00.9999Z"),
            Some(1_767_225_600_999)
        );
        assert_eq!(
            parse_js_date_ms("2026-01-01T00:00:00.0001Z"),
            Some(1_767_225_600_000)
        );
        // 无时区的短小数秒按本地时间解析。
        use chrono::TimeZone;
        let expected = chrono::Local
            .from_local_datetime(
                &chrono::NaiveDate::from_ymd_opt(2026, 1, 1)
                    .unwrap()
                    .and_hms_milli_opt(0, 0, 0, 100)
                    .unwrap(),
            )
            .earliest()
            .unwrap()
            .timestamp_millis();
        assert_eq!(parse_js_date_ms("2026-01-01T00:00:00.1"), Some(expected));
    }

    #[test]
    fn date_parse_rejects_forms_v8_rejects() {
        assert_eq!(parse_js_date_ms(" 2026-01-01T00:00:00Z"), None);
        assert_eq!(parse_js_date_ms("2026-01-01T00:00:00Z "), None);
        assert_eq!(parse_js_date_ms("2026-01-01T00:00:00+08"), None);
        assert_eq!(parse_js_date_ms("2026-13-01"), None);
        assert_eq!(parse_js_date_ms("2026-01-00"), None);
        assert_eq!(parse_js_date_ms("2026-00-01"), None);
        assert_eq!(parse_js_date_ms("2026-01-32"), None);
        assert_eq!(parse_js_date_ms("2026-01-01T00:00:00,000Z"), None);
        assert_eq!(parse_js_date_ms("2026-01-01T24:00:01Z"), None);
    }

    #[test]
    fn date_parse_without_offset_uses_local_time() {
        use chrono::TimeZone;

        let expected = chrono::Local
            .from_local_datetime(
                &chrono::NaiveDate::from_ymd_opt(2026, 1, 1)
                    .unwrap()
                    .and_hms_opt(0, 0, 0)
                    .unwrap(),
            )
            .earliest()
            .unwrap()
            .timestamp_millis();
        assert_eq!(parse_js_date_ms("2026-01-01T00:00:00"), Some(expected));
        assert_eq!(parse_js_date_ms("2026-01-01 00:00"), Some(expected));
    }
}
