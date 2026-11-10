//! `extensions/mcp/log.ts`：MCP 服务器 `notifications/message` 的日志行与 `mcp.log` 追加。
//!
//! TS: `packages/coding-agent/src/extensions/mcp/log.ts`。
//!
//! 服务器用 `notifications/message` 发出的消息追加到 agent 目录的 `mcp.log`。多个 kiana 进程可能写
//! 同一个文件，因此每条消息都是一次同步追加。文件超过 `MAX_LOG_BYTES` 后轮转到 `mcp.log.1`。
//!
//! # 为什么需要
//!
//! MCP 服务器可以主动上报日志（协议字段 `level`/`logger`/`data`）；`notifications/message`
//! 由 runtime 转给 [`McpServerLog`] 追加到 agent 目录的 `mcp.log`
//! （`McpExtensionOptions::log_path` 可覆盖），正文里的换行保留并缩进 4 个空格。
//! 文件过大时先把旧内容整体
//! 改名为 `mcp.log.1`，后续消息写入新的 `mcp.log`（只保留一代备份）。
//!
//! # 在系统中的位置
//!
//! [`McpServerLog`] 由 `runtime.rs` 的 `McpServerConnectionOptions::log` 接收：连接建立时
//! runtime 注册 `notifications/message` 监听器，并在回调里调用 [`McpServerLog::write`]。
//! `index.rs` 惰性创建一个实例、克隆给所有连接（`server_log_for`）；`cli.rs` 的每个临时
//! 连接各自 `new` 一个实例。克隆共享内部的 `size` 缓存。
//!
//! # 写入流程（[`McpServerLog::write`]）
//!
//! 1. 在锁外取当前时间生成一行（[`format_mcp_log_message`]）；
//! 2. 锁住大小缓存；首次写入时创建父目录并 `stat` 文件得到初始大小；
//! 3. 缓存大小**严格大于** `MAX_LOG_BYTES` 时，重新 `stat` 磁盘，仍超限才把文件重命名为
//!    `mcp.log.1`（失败时丢弃本次日志）；随后把缓存重置为磁盘上的实际大小；
//! 4. 以 append 模式打开文件并 `write_all`；成功则把行字节数累加进缓存。
//!
//! # 失败模式与注意事项
//!
//! - 日志是 best effort：目录创建、重命名、打开、写入的错误全部被吞掉，本次调用直接返回，
//!   不 panic、不影响工具调用；
//! - 重命名失败时整行被丢弃，且缓存大小不更新（与 TS 的 try/catch 一致，测试锁定）；
//! - 缓存只在初始化、轮转、本实例成功写入后更新；其它进程写入的字节不计入，所以跨进程的
//!   5 MiB 阈值是近似的。轮转前的第二次 `stat` 只是避免对已经被别处轮转（变小）的文件
//!   重复重命名；
//! - 同一实例的克隆之间，`Mutex` 在整个写入（含文件 IO）期间持有，因此进程内写入串行；
//!   不同实例（例如 `cli.rs` 的每个连接）各有各的锁，只靠 append 打开方式保证不覆盖已有
//!   内容；
//! - 格式化函数本身不转义服务器名与 `level`；它们若含换行，日志行会被拆成多行（代码没有
//!   清洗）。

use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

/// `5 * 1024 * 1024`。TS: `log.ts#MAX_LOG_BYTES`。
const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;

/// `formatData(data)`：字符串原样；否则 `JSON.stringify(data) ?? String(data)`。
///
/// JSON 值里没有 `undefined`；键缺失由 `None` 表示，输出 `String(undefined)` == `"undefined"`。
fn format_data(data: Option<&Value>) -> String {
    match data {
        None => "undefined".to_string(),
        Some(Value::String(text)) => text.clone(),
        Some(value) => kiana_json::js_object::stringify(value),
    }
}

/// 把 `/\r?\n/g` 替换成 `"\n    "`（续行缩进恰好 4 个空格）。
fn indent_continuation_lines(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if ch == '\n' {
            if out.ends_with('\r') {
                out.pop();
            }
            out.push_str("\n    ");
        } else {
            out.push(ch);
        }
    }
    out
}

/// [`format_mcp_log_message`] 的当前时间（`Date.now()`）。
fn now_ms() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => i64::try_from(duration.as_millis()).unwrap_or(i64::MAX),
        Err(error) => -i64::try_from(error.duration().as_millis()).unwrap_or(i64::MAX),
    }
}

/// `Date.prototype.toISOString()`：`YYYY-MM-DDTHH:mm:ss.sssZ`（UTC，毫秒 3 位）。
///
/// 用民用历算法（Howard Hinnant 的 `civil_from_days`）而不是 `chrono`，这样年份 0..
/// 9999 之外也按 JS 的 `±YYYYYY` 规则展开。`Date` 的有效范围（±8.64e15 ms）与 `i64` 的
/// 毫秒数相容；超出该范围的输入在 TS 里会抛 `RangeError`，这里仍格式化（不可达路径）。
fn iso_string(timestamp_ms: i64) -> String {
    const MS_PER_DAY: i64 = 86_400_000;
    let days = timestamp_ms.div_euclid(MS_PER_DAY);
    let within = timestamp_ms.rem_euclid(MS_PER_DAY);
    let (year, month, day) = civil_from_days(days);
    let hour = within / 3_600_000;
    let minute = (within / 60_000) % 60;
    let second = (within / 1000) % 60;
    let millisecond = within % 1000;
    let year_text = if (0..=9999).contains(&year) {
        format!("{year:04}")
    } else if year < 0 {
        format!("-{:06}", -year)
    } else {
        format!("+{year:06}")
    };
    format!("{year_text}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millisecond:03}Z")
}

/// 1970-01-01 起的天数 → (年, 月, 日)。Howard Hinnant 的 `civil_from_days`。
///
/// 输入可为负（1970 年之前）；era 的除法对负值先减去 146096（一个 era 减一天）再除，
/// 从而得到向下取整。
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = (shifted - era * 146_097) as u64; // [0, 146096]
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365; // [0, 399]
    let year = year_of_era as i64 + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100); // [0, 365]
    let month_prime = (5 * day_of_year + 2) / 153; // [0, 11]
    let day = (day_of_year - (153 * month_prime + 2) / 5 + 1) as u32; // [1, 31]
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    } as u32;
    (year + i64::from(month <= 2), month, day)
}

/// 把 `server` 的一条 `notifications/message` 格式化成一行日志；续行缩进 4 个空格。
/// TS: `log.ts#formatMcpLogMessage`。
///
/// `params` 接受 `Option<&Value>`（kiana-mcp 的通知监听器就是 `Option<&Value>`）：缺失的
/// `params` 在 TS 里是 `undefined`，包成 `{ data: undefined }` 后输出 `undefined`；显式 JSON
/// `null` 仍输出 `null`。
///
/// # 格式
///
/// `{ISO 时间} [{server}] {level}{logger} {data}\n`：
///
/// - `level` 取 `params.level` 的字符串值，缺失或非字符串时为 `info`；
/// - `logger` 只在是非空字符串时输出 ` {logger}:`；
/// - `data` 取记录里的 `data` 键；`params` 本身不是对象时整个 `params` 作为 data；
/// - data 是字符串时原样输出，否则按 JS `JSON.stringify`；缺省的 data 输出 `undefined`。
///
/// 服务器名、`level` 与文本都按原样拼接，不做转义（文本只做续行缩进）。
pub fn format_mcp_log_message<'a>(
    server: &str,
    params: impl Into<Option<&'a Value>>,
    now_ms: i64,
) -> String {
    let params = params.into();
    let message = params.and_then(Value::as_object);
    let level = message
        .and_then(|message| message.get("level"))
        .and_then(Value::as_str)
        .unwrap_or("info");
    let logger = match message.and_then(|message| message.get("logger")) {
        Some(Value::String(logger)) if !logger.is_empty() => format!(" {logger}:"),
        _ => String::new(),
    };
    // `isRecord(params) ? params : { data: params }`；非记录时 data 就是 params 本身。
    let data = match message {
        Some(message) => message.get("data"),
        None => params,
    };
    let text = indent_continuation_lines(&format_data(data));
    format!("{} [{server}] {level}{logger} {text}\n", iso_string(now_ms))
}

/// 把服务器日志消息追加到一个文件。写错误被忽略：日志是 best effort，不能弄坏工具调用。
/// TS: `log.ts#McpServerLog`。
///
/// `Clone` 共享同一大小缓存：TS 的扩展只创建一个实例给所有连接使用（`getServerLog` 的
/// `serverLog ??=`），多个实例各自缓存会让轮转阈值失准。
///
/// # 共享与并发
///
/// 克隆得到相同的 `path` 并共享内部大小缓存。同一实例（含克隆）的写入在进程内串行；
/// 不同实例之间没有共享锁，只能依赖 append 打开方式保证追加不覆盖已有内容。
/// `index.rs` 用一个共享实例，`cli.rs` 的每个临时连接各建一个。
#[derive(Debug, Clone)]
pub struct McpServerLog {
    /// 日志文件路径（`<agentDir>/mcp.log`）。
    pub path: PathBuf,
    /// 缓存的文件大小；`None` 表示尚未初始化（同时决定是否创建目录）。
    size: Arc<Mutex<Option<u64>>>,
}

impl McpServerLog {
    /// `new McpServerLog(path)`。
    pub fn new(path: impl Into<PathBuf>) -> Self {
        McpServerLog {
            path: path.into(),
            size: Arc::new(Mutex::new(None)),
        }
    }

    /// `write(server, params)`：格式化并追加一行，必要时轮转。
    ///
    /// 同步 IO，且在内部 `Mutex` 保护下执行，因此调用方（`runtime.rs` 的通知回调）会一直
    /// 等到写盘结束。所有错误都被忽略（签名无返回值）：日志失败不能影响工具调用。
    pub fn write<'a>(&self, server: &str, params: impl Into<Option<&'a Value>>) {
        let line = format_mcp_log_message(server, params, now_ms());
        let mut size = self
            .size
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if size.is_none() {
            if let Some(parent) = self.path.parent()
                && !parent.as_os_str().is_empty()
                // TS 的 `mkdirSync` 抛错时整个 write 被 catch 吞掉，这一行不落地、`size`
                // 保持未初始化（下次 write 会重试）。
                && fs::create_dir_all(parent).is_err()
            {
                return;
            }
            *size = Some(current_size(&self.path));
        }
        if size.is_some_and(|size| size > MAX_LOG_BYTES) {
            // 其它进程可能已经轮转过；重命名前再检查磁盘上的真实大小。
            if current_size(&self.path) > MAX_LOG_BYTES {
                let mut rotated: OsString = self.path.clone().into_os_string();
                rotated.push(".1");
                // TS 的 `renameSync` 在 try 块里：抛错（例如 `.1` 是非空目录）时这一行也被
                // 丢掉，且缓存的 size 不更新。
                if fs::rename(&self.path, Path::new(&rotated)).is_err() {
                    return;
                }
            }
            *size = Some(current_size(&self.path));
        }
        // append 打开：本实例与其它进程都追加到文件尾，不覆盖已有内容。
        let Ok(mut file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        else {
            return;
        };
        // 只有整行写成功才累加缓存；失败时缓存不更新（若已部分写入，缓存会少计那些字节）。
        if file.write_all(line.as_bytes()).is_ok()
            && let Some(size) = size.as_mut()
        {
            *size += line.len() as u64;
        }
    }
}

/// `currentSize()`：`statSync(path).size`，异常返回 0。
fn current_size(path: &Path) -> u64 {
    fs::metadata(path)
        .map(|metadata| metadata.len())
        .unwrap_or(0)
}

// 测试锁定日志行格式（默认 level、CRLF、JS 年份展开）与轮转/克隆共享缓存行为；
// `tests/mcp_tools.rs` 另用 TS 生成的 golden 断言同一格式化输出。
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn formats_the_documented_example() {
        // 2024-01-02T03:04:05.678Z == 1704164645678 ms。
        let line = format_mcp_log_message(
            "x",
            &json!({"level": "error", "logger": "db", "data": "a\nb"}),
            1_704_164_645_678,
        );
        assert_eq!(line, "2024-01-02T03:04:05.678Z [x] error db: a\n    b\n");
    }

    #[test]
    fn defaults_level_and_wraps_non_records() {
        assert_eq!(
            format_mcp_log_message("s", &json!("hello"), 0),
            "1970-01-01T00:00:00.000Z [s] info hello\n"
        );
        assert_eq!(
            format_mcp_log_message("s", &Value::Null, 0),
            "1970-01-01T00:00:00.000Z [s] info null\n"
        );
        assert_eq!(
            format_mcp_log_message("s", &json!({}), 0),
            "1970-01-01T00:00:00.000Z [s] info undefined\n"
        );
        assert_eq!(
            format_mcp_log_message("s", &json!({"data": {"b": 1, "a": [2]}}), 0),
            "1970-01-01T00:00:00.000Z [s] info {\"b\":1,\"a\":[2]}\n"
        );
        assert_eq!(
            format_mcp_log_message("s", &json!({"data": 1e21}), 0),
            "1970-01-01T00:00:00.000Z [s] info 1e+21\n"
        );
        assert_eq!(
            format_mcp_log_message("s", &json!({"level": 5, "logger": "", "data": "x"}), 0),
            "1970-01-01T00:00:00.000Z [s] info x\n"
        );
    }

    #[test]
    fn normalizes_crlf_and_keeps_lone_carriage_returns() {
        assert_eq!(
            format_mcp_log_message("s", &json!({"data": "a\r\nb\nc\rd"}), 0),
            "1970-01-01T00:00:00.000Z [s] info a\n    b\n    c\rd\n"
        );
    }

    #[test]
    fn rotates_past_five_mib_before_appending() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("nested").join("mcp.log");
        fs::create_dir_all(path.parent().expect("parent")).expect("create dir");
        let oversized = vec![b'x'; MAX_LOG_BYTES as usize + 1];
        fs::write(&path, &oversized).expect("write oversized log");

        let log = McpServerLog::new(&path);
        log.write("s", &json!({"data": "after"}));

        let rotated = directory.path().join("nested").join("mcp.log.1");
        assert!(rotated.exists(), "the oversized log is rotated to .1");
        assert_eq!(
            fs::metadata(&rotated).expect("rotated metadata").len(),
            MAX_LOG_BYTES + 1
        );
        let text = fs::read_to_string(&path).expect("new log");
        assert!(text.ends_with(" [s] info after\n"), "{text}");
        assert!(!text.starts_with('x'));
    }

    #[test]
    fn does_not_rotate_at_exactly_five_mib() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("mcp.log");
        fs::write(&path, vec![b'x'; MAX_LOG_BYTES as usize]).expect("write log");
        let log = McpServerLog::new(&path);
        log.write("s", &json!({"data": "kept"}));
        assert!(!directory.path().join("mcp.log.1").exists());
        let line = format_mcp_log_message("s", &json!({"data": "kept"}), 0);
        assert_eq!(
            fs::metadata(&path).expect("metadata").len(),
            MAX_LOG_BYTES + line.len() as u64
        );
    }

    /// 评审回归：TS 的扩展只创建一个 `McpServerLog` 给所有连接（`getServerLog` 的
    /// `serverLog ??=`）。克隆必须共享大小缓存，否则一个连接写入的大日志不会让另一个
    /// 实例轮转，文件会超过 TS 的 5 MiB 上限。
    #[test]
    fn clones_share_the_cached_size() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("mcp.log");
        let log = McpServerLog::new(&path);
        log.write("s", &json!({"data": "warm"}));

        let clone = log.clone();
        clone.write(
            "s",
            &json!({"data": "x".repeat(MAX_LOG_BYTES as usize + 1)}),
        );
        log.write("s", &json!({"data": "after"}));

        assert!(
            directory.path().join("mcp.log.1").exists(),
            "the size written through the clone triggers rotation in the original"
        );
    }

    #[test]
    fn expands_years_outside_four_digits_like_js() {
        assert_eq!(iso_string(253_402_300_799_999), "9999-12-31T23:59:59.999Z");
        assert_eq!(
            iso_string(253_402_300_800_000),
            "+010000-01-01T00:00:00.000Z"
        );
        assert_eq!(iso_string(-62_167_219_200_000), "0000-01-01T00:00:00.000Z");
        assert_eq!(
            iso_string(-62_167_219_200_001),
            "-000001-12-31T23:59:59.999Z"
        );
    }

    /// 评审回归：`renameSync` 抛错时 TS 的 try/catch 丢掉整次写入（不追加、不更新缓存大小）。
    #[test]
    fn drops_the_line_when_rotation_fails() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("mcp.log");
        let oversized = vec![b'x'; MAX_LOG_BYTES as usize + 1];
        fs::write(&path, &oversized).expect("write oversized log");
        // A non-empty directory at `.1` makes `rename` fail.
        let rotated = directory.path().join("mcp.log.1");
        fs::create_dir(&rotated).expect("create rotated dir");
        fs::write(rotated.join("inner"), "x").expect("write inner");

        let log = McpServerLog::new(&path);
        log.write("s", &json!({"data": "after"}));
        log.write("s", &json!({"data": "after2"}));

        assert_eq!(
            fs::metadata(&path).expect("metadata").len(),
            MAX_LOG_BYTES + 1,
            "the oversized log is not appended to"
        );
        assert!(rotated.is_dir());
    }
}
