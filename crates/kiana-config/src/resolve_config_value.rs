//! core/resolve-config-value.ts（`$ENV` / `${ENV}` / `!command` / `$$` / `$!` 模板）。
//!
//! TS 源：`packages/coding-agent/src/core/resolve-config-value.ts`（287 行）。
//! 规范：`spec/ca-config-models.md` §2.11（状态机逐字复刻）、§3.6（错误文案）、
//! §8.3 第 9 条（模板/缓存/Windows stdin）。
//!
//! ## 行为要点
//!
//! - **模板**：`$NAME`、`${NAME}`（`NAME` 必须是 `[A-Za-z_][A-Za-z0-9_]*`）、`$$` → 字面 `$`、
//!   `$!` → 字面 `!`；裸 `$`、`${` 未闭合、`${非法名字}` 都按字面保留。相邻字面量合并。
//! - **命令**：值是 `!` 开头的字符串时执行其余部分；stdout 按 JS `String.prototype.trim()`
//!   的空白集合裁剪，空输出 → `undefined`。失败（非零退出、超时 10 000 ms、spawn 失败、
//!   stdout 超过 Node 默认 `maxBuffer` 1 MiB）→ `undefined`。成功与失败都进进程级缓存，
//!   `clear_config_value_cache()` 清空。
//! - **环境变量不缓存**：每次调用都重新读取（`env` 映射优先、`process.env` 兜底；空串视为缺失，
//!   与 TS 的 `||` 一致）。
//! - **Windows**：先试 `getShellConfig()` 解析出的 bash；`commandTransport === "stdin"` 时把命令
//!   写入 stdin（`bash -s`），否则作为 `-c` 的参数。启动失败（ENOENT）才回落到默认 shell
//!   （`%ComSpec% || cmd.exe`：cmd.exe 走 `['/d','/s','/c', '"cmd"']` + `windowsVerbatimArguments`，
//!   其它 ComSpec 走 `['-c', cmd]`）。
//!
//! ## 与 TS 的已知偏差
//!
//! - 环境映射的 Rust 类型是 [`ConfigEnv`]（`IndexMap<String, String>`，对应 JS 的
//!   `Record<string, string>` 且保留插入顺序）。
//! - Node 的 `spawnSync`/`execSync` 以 `utf-8` 解码；Rust 侧用 lossy UTF-8 解码
//!   （非法字节 → U+FFFD），与 Node 的 `Buffer#toString("utf8")` 一致。`maxBuffer` 按解码后的
//!   UTF-8 字节数判断（含 U+FFFD 替换的非法字节），对合法 UTF-8 输出与 Node 的原始字节计数一致。
//! - `maxBuffer` 由本文件实现：`kiana_core::child_process::spawn_process_sync` 没有该选项，直接
//!   使用会返回无界 stdout（`!yes` 还会耗尽内存）。这里的本地 `spawnSync` 等价物在累积输出
//!   超过 1 MiB 时 kill 子进程并返回 ENOBUFS，与 Node 的 `spawnSync`/`execSync` 一致。
//! - Windows 的 `cmd.exe` 默认 shell 分支只用 `raw_arg` 原样拼接参数；该分支只在 Windows 下
//!   编译与运行（ARCHITECTURE §8 第 26 条）。纯参数构造通过
//!   [`windows_default_shell_invocation`] 在任意平台上测试。
//!
//! ## 这个模块解决什么问题
//!
//! provider 配置（`models.json`）与扩展配置中的 API key、header 值允许三种写法：
//!
//! ```text
//! 字面量          my-api-key
//! 环境变量模板    $MY_KEY / ${MY_KEY} / prefix-${MY_KEY}
//! shell 命令      !security find-generic-password -s my-key -w
//! ```
//!
//! 本模块负责判定一个字符串属于哪种写法并求值，调用方只使用它的公开函数。主要的
//! 调用方例如：`models::composer` 用它解析 provider 的 API key 与 header，
//! `kiana-builtin-extensions` 的 MCP 运行时用 [`resolve_config_value_or_throw`] 解析
//! 同类配置值；`crate::auth::storage` 在读取 `auth.json` 凭据时也调用
//! [`resolve_config_value`] 解析 API key。解析规则（转义、空串环境变量的回退、
//! 命令 stdout 的 `trim` 空白集合）
//! 都按 spec §2.11 与 TS 逐字对齐，偏差集中记录在上一节。
//!
//! ## 执行流程
//!
//! 1. `parse_config_value_reference`：首字符是 `!` → 命令；否则把字符串扫描成
//!    `TemplatePart` 序列（由字面量与环境变量引用按出现顺序组成；相邻字面量已合并，
//!    环境变量引用可以相邻，如 `"$FOO$BAR"` 产生两个相邻的 env part）。
//! 2. 命令：`execute_command` 先查进程级缓存；未命中才执行 shell——Windows 上先试
//!    configured shell（shell 配置解析失败或 spawn 返回 ENOENT 时 `executed == false`），
//!    再回落到默认 shell；非 Windows 直接用 `/bin/sh -c`。stdout 按 JS `trim()` 裁剪，
//!    空输出视为 `None`。
//! 3. 模板：`resolve_template` 顺序拼接；任一环境变量取不到非空值，整个结果就是
//!    `None`，不会做部分替换。
//! 4. `*_or_throw` 系列把 `None` 映射成 [`ConfigValueError`]，`Display` 文案逐字等于
//!    TS 的 `Error.message`（spec §3.6）。
//!
//! ## 注意事项
//!
//! - 命令缓存（只被 [`resolve_config_value`] 路径使用）是进程级全局状态
//!   （`static Mutex<HashMap<...>>`），成功与失败都缓存，只有
//!   [`clear_config_value_cache`] 能清空；模板中的环境变量值从不缓存。
//! - 模板中的环境变量每次调用都重新读取：`env` 映射优先，未命中或值为空串时回退
//!   到进程环境（对应 TS 的 `||`）。
//! - 命令解析是同步阻塞的：内部用 `std::process::Command` 加轮询（循环里 sleep 2 ms），
//!   10 s 超时后 kill，子进程退出后最多再等 2 s 收集 stdout；没有取消接口。
//! - 缓存读取与写入是两次独立加锁，没有 single-flight：同一命令的并发首次调用可能
//!   各自启动一个子进程。
//! - 文本解码统一是 lossy UTF-8（见上文“与 TS 的已知偏差”）。
//!
//! ## 阅读顺序
//!
//! `parse_config_value_template` → `resolve_template` → `spawn_sync_command`
//! （超时 / maxBuffer / 退出状态的状态机）→ `execute_command_uncached_with` →
//! 公开查询函数 → [`resolve_headers`] / [`resolve_headers_or_throw`]。

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, OnceLock};

use indexmap::IndexMap;
use kiana_core::child_process::SpawnSyncResult;
use kiana_core::shell::{
    CommandTransport, Platform, ShellConfig, ShellConfigError, get_shell_config,
};

/// JS `Record<string, string>`（含顺序）的环境变量视图。
///
/// TS: `env?: Record<string, string>`。`Some(map)` 时优先读 map，再回退到 `process.env`；
/// `None` 时只读 `process.env`。
///
/// 回退发生在两层：映射未命中、或命中但值为空串，都会继续查进程环境；只有两层都取不到
/// 非空值时才视为“未定义”。本模块只读该映射，不会修改调用方传入的内容。
pub type ConfigEnv = IndexMap<String, String>;

/// 命令解析超时（毫秒）。TS: `timeout: 10000`（`executeWithConfiguredShell` 与 `execSync`）。
const COMMAND_TIMEOUT_MS: u64 = 10_000;

/// Node `spawnSync`/`execSync` 的默认 `maxBuffer`（字节，`MAX_BUFFER = 1024 * 1024`）。
///
/// stdout 超过该值 → `error.code === "ENOBUFS"`，TS 的 catch/错误分支返回 `undefined`。
/// 边界由 node 26.12.1 实测确认：恰好 1 048 576 字节成功，1 048 577 字节失败。
const MAX_BUFFER_BYTES: usize = 1024 * 1024;

/// 模板片段。
///
/// TS: `type TemplatePart = { type: "literal"; value: string } | { type: "env"; name: string }`。
#[derive(Debug, Clone, PartialEq, Eq)]
enum TemplatePart {
    /// 字面量（相邻字面量已合并）。
    Literal { value: String },
    /// 环境变量引用。
    Env { name: String },
}

/// 配置值引用。
///
/// TS: `type ConfigValueReference = { type: "command"; config: string } | { type: "template"; parts: TemplatePart[] }`。
#[derive(Debug, Clone, PartialEq, Eq)]
enum ConfigValueReference {
    /// `!command`，`config` 保留开头的 `!`。
    Command { config: String },
    /// 模板。
    Template { parts: Vec<TemplatePart> },
}

/// 进程级命令结果缓存：键是**含 `!` 的完整 config**，成功（`Some`）与失败（`None`）都缓存。
fn command_result_cache() -> &'static Mutex<HashMap<String, Option<String>>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<String>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 取全局缓存锁。线程 panic 导致锁中毒时取回内部值继续使用，而不是 panic。
fn cache_guard() -> MutexGuard<'static, HashMap<String, Option<String>>> {
    command_result_cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 清空配置值命令缓存。TS: `clearConfigValueCache()`（导出的测试入口）。
///
/// 缓存是进程级共享状态，清空会影响同进程内的所有调用方。它只清命令结果；模板中的
/// 环境变量从不缓存，无需清理。加锁在中毒时取回内部值继续使用，因此本函数不会失败。
pub fn clear_config_value_cache() {
    cache_guard().clear();
}

/// 追加一个字面量片段；空串忽略，最后一段已是字面量时拼接。
///
/// TS: `appendLiteral(parts, value)`。
fn append_literal(parts: &mut Vec<TemplatePart>, value: &str) {
    if value.is_empty() {
        return;
    }
    if let Some(TemplatePart::Literal { value: previous }) = parts.last_mut() {
        previous.push_str(value);
        return;
    }
    parts.push(TemplatePart::Literal {
        value: value.to_string(),
    });
}

/// `ENV_VAR_NAME_RE = /^[A-Za-z_][A-Za-z0-9_]*$/` 的等价物。
fn is_env_var_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    let Some(first) = bytes.first() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || *first == b'_') {
        return false;
    }
    bytes[1..]
        .iter()
        .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
}

/// `ENV_VAR_NAME_PREFIX_RE = /^[A-Za-z_][A-Za-z0-9_]*/` 的等价物：返回匹配到的前缀。
fn env_var_name_prefix(text: &str) -> Option<&str> {
    let bytes = text.as_bytes();
    let first = *bytes.first()?;
    if !(first.is_ascii_alphabetic() || first == b'_') {
        return None;
    }
    let mut end = 1;
    while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
        end += 1;
    }
    Some(&text[..end])
}

/// `parseConfigValueTemplate(config)` 的逐字符状态机。
///
/// 所有下标都是 UTF-8 字节下标；因为只搜索/切分 ASCII（`$`、`{`、`}`）位置，UTF-8 与
/// JS 的 UTF-16 索引在这些边界上等价。
fn parse_config_value_template(config: &str) -> Vec<TemplatePart> {
    let mut parts: Vec<TemplatePart> = Vec::new();
    let mut index = 0usize;
    let bytes = config.as_bytes();

    while index < config.len() {
        let Some(relative) = config[index..].find('$') else {
            append_literal(&mut parts, &config[index..]);
            break;
        };
        let dollar_index = index + relative;
        append_literal(&mut parts, &config[index..dollar_index]);

        match bytes.get(dollar_index + 1).copied() {
            Some(b'$') | Some(b'!') => {
                // `$$` / `$!`：转义成字面量，并把转义后的字符当作普通字面量合并。
                append_literal(&mut parts, &config[dollar_index + 1..dollar_index + 2]);
                index = dollar_index + 2;
            }
            Some(b'{') => {
                // `${...}` 以**第一个** `}` 结束，不处理嵌套；名字仍须匹配
                // `[A-Za-z_][A-Za-z0-9_]*`，否则整段（含花括号）原样作为字面量。
                let Some(end_relative) = config[dollar_index + 2..].find('}') else {
                    // 未闭合的 `${`：只把 `$` 字面化，`{` 留给下一轮按普通字面量追加。
                    append_literal(&mut parts, "$");
                    index = dollar_index + 1;
                    continue;
                };
                let end_index = dollar_index + 2 + end_relative;
                let name = &config[dollar_index + 2..end_index];
                if is_env_var_name(name) {
                    parts.push(TemplatePart::Env {
                        name: name.to_string(),
                    });
                } else {
                    // 非法名字：`${...}` 整段按字面量保留（含花括号）。
                    append_literal(&mut parts, &config[dollar_index..=end_index]);
                }
                index = end_index + 1;
            }
            _ => {
                // `$` 后既不是转义也不是 `{`：按 `$NAME` 尝试读取名字前缀；失败
                // （裸 `$`、`$1`、`$变量` 等）时只把 `$` 当字面量，从下一个字符继续扫描。
                if let Some(name) = env_var_name_prefix(&config[dollar_index + 1..]) {
                    parts.push(TemplatePart::Env {
                        name: name.to_string(),
                    });
                    index = dollar_index + 1 + name.len();
                    continue;
                }
                append_literal(&mut parts, "$");
                index = dollar_index + 1;
            }
        }
    }

    parts
}

/// `parseConfigValueReference(config)`。
fn parse_config_value_reference(config: &str) -> ConfigValueReference {
    if config.starts_with('!') {
        return ConfigValueReference::Command {
            config: config.to_string(),
        };
    }
    ConfigValueReference::Template {
        parts: parse_config_value_template(config),
    }
}

/// `resolveEnvConfigValue(name, env)`：`env?.[name] || process.env[name] || undefined`
/// （空串视为缺失）。
fn resolve_env_config_value(name: &str, env: Option<&ConfigEnv>) -> Option<String> {
    env.and_then(|map| map.get(name))
        .filter(|value| !value.is_empty())
        .cloned()
        .or_else(|| std::env::var(name).ok().filter(|value| !value.is_empty()))
}

/// `getTemplateEnvVarNames(parts)`：按出现顺序去重。
fn template_env_var_names(parts: &[TemplatePart]) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for part in parts {
        if let TemplatePart::Env { name } = part
            && !names.contains(name)
        {
            names.push(name.clone());
        }
    }
    names
}

/// `resolveTemplate(parts, env)`：顺序拼接；任一环境变量缺失 → 整个 `None`。
///
/// 空配置串（零个 part）得到 `Some("")` 而不是 `None`；字面量部分本身不会失败。
fn resolve_template(parts: &[TemplatePart], env: Option<&ConfigEnv>) -> Option<String> {
    let mut resolved = String::new();
    for part in parts {
        match part {
            TemplatePart::Literal { value } => resolved.push_str(value),
            TemplatePart::Env { name } => {
                let value = resolve_env_config_value(name, env)?;
                resolved.push_str(&value);
            }
        }
    }
    Some(resolved)
}

/// JS `String.prototype.trim()` 的空白集合。
///
/// ES 的 `TrimString` 去掉 WhiteSpace（`\t \v \f space NBSP ZWNBSP` + Unicode `Zs`）与
/// LineTerminator（U+000A、U+000D、U+2028、U+2029）。注意 U+0085（NEL）**不是** JS 空白，
/// 而 Rust 的 `char::is_whitespace` 认为它是，因此不能直接用 `str::trim`。
fn is_js_trim_char(character: char) -> bool {
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

/// `String.prototype.trim()` 的等价物。
fn js_trim(text: &str) -> &str {
    text.trim_matches(is_js_trim_char)
}

/// `(stdout ?? "").trim() || undefined`。
fn command_result(stdout: &str) -> Option<String> {
    let value = js_trim(stdout);
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

/// `executeWithConfiguredShell(command)` 的返回值。
///
/// `executed` 是 Windows 回落默认 shell 的唯一开关：只有“shell 配置无法解析”或
/// “spawn 返回 ENOENT”才是 `false`；非零退出、超时、ENOBUFS 都算已执行
/// （`value: None`），不会再尝试默认 shell。
struct ShellExecution {
    /// spawn 是否真正执行了（只有 shell 配置抛错或 ENOENT 时是 `false`）。
    executed: bool,
    /// 裁剪后的 stdout（空 → `None`）。
    value: Option<String>,
}

/// `spawnSync(shell, args, options)` 的选项子集。
struct SpawnRequest<'a> {
    /// `input`：写入子进程 stdin 的内容；`None` → stdin 为 `"ignore"`。
    stdin_input: Option<&'a str>,
    /// `windowsHide`。
    windows_hide: bool,
    /// `windowsVerbatimArguments`：参数已按 Node 的 cmd.exe 规则拼好，不能再让 std 转义。
    windows_verbatim_arguments: bool,
}

/// Node `spawnSync` 的等价物：stdout `pipe`、stderr `ignore`、可选的 stdin `input`、
/// `timeout` 与默认 `maxBuffer`（1 MiB）。
///
/// `kiana_core::child_process::spawn_process_sync` 没有 `maxBuffer`：它会把任意长度的 stdout
/// 全部累积下来。Node 的 `spawnSync`/`execSync` 默认在 stdout 超过 1 MiB 时以 ENOBUFS
/// 终止子进程（TS 的错误分支 → `undefined`），所以这里在累积超限时立即 kill，
/// 既保证结果一致，也避免 `!yes` 这类无界输出耗尽内存。
///
/// 返回值的 `stderr` 恒为空字符串（子进程 stderr 被丢弃，不参与 maxBuffer 计数）；
/// ENOBUFS 时 `status` 强制为 `None`，具体原因放在 `error` 里。
fn spawn_sync_command(
    shell: &str,
    args: &[String],
    request: &SpawnRequest<'_>,
    timeout_ms: u64,
) -> SpawnSyncResult {
    let mut command = std::process::Command::new(shell);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        if request.windows_verbatim_arguments {
            for arg in args {
                command.raw_arg(arg);
            }
        } else {
            command.args(args);
        }
        if request.windows_hide {
            // CREATE_NO_WINDOW（Node 的 `windowsHide: true`）。
            command.creation_flags(0x0800_0000);
        }
    }
    #[cfg(not(windows))]
    {
        let _ = request.windows_verbatim_arguments;
        let _ = request.windows_hide;
        command.args(args);
    }
    command
        .stdin(match request.stdin_input {
            Some(_) => std::process::Stdio::piped(),
            None => std::process::Stdio::null(),
        })
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());

    // spawn 失败（ENOENT、权限等）不 panic：把 io::Error 放进结果，由调用方决定是
    // 回落到默认 shell（仅 Windows 的 ENOENT）还是直接当作失败。
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return SpawnSyncResult {
                status: None,
                stdout: String::new(),
                stderr: String::new(),
                error: Some(error),
            };
        }
    };

    // stdin 写入放到独立线程：命令可能超过管道缓冲，主线程不能先阻塞在写操作上。
    let mut stdin = child.stdin.take();
    let input = request.stdin_input.map(|input| input.as_bytes().to_vec());
    let writer = std::thread::spawn(move || {
        use std::io::Write;
        if let (Some(mut stdin), Some(input)) = (stdin.take(), input) {
            let _ = stdin.write_all(&input);
        }
    });

    // stdout 在独立线程里按块读走，主线程负责累积并在超过 maxBuffer 后 kill。
    let mut stdout = child.stdout.take();
    let (stdout_sender, stdout_receiver) = std::sync::mpsc::channel::<Vec<u8>>();
    let reader = std::thread::spawn(move || {
        use std::io::Read;
        let mut buffer = [0u8; 64 * 1024];
        if let Some(mut stdout) = stdout.take() {
            loop {
                match stdout.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(read) => {
                        if stdout_sender.send(buffer[..read].to_vec()).is_err() {
                            break;
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
        }
    });

    // 超时从 spawn 成功之后开始计，不包含构造 Command 与启动进程的时间。
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    let mut stdout_bytes: Vec<u8> = Vec::new();
    let mut reader_done = false;
    let mut exit_status: Option<std::process::ExitStatus> = None;
    let mut exit_grace_deadline: Option<std::time::Instant> = None;
    let mut wait_error: Option<std::io::Error> = None;
    let mut timed_out = false;
    let mut overflowed = false;

    // 单线程轮询状态机，每轮依次做五件事：
    // ① 非阻塞地排空 stdout channel，累积并检查 maxBuffer（超限立即记 overflowed）；
    // ② 若 overflowed 且子进程尚未退出且未超时，则 kill 子进程（ENOBUFS 分支每轮都会
    //    重发 kill，直到观察到退出或超时）；
    // ③ 用 try_wait 观察退出状态；
    // ④ 若已退出，最多再等 2 s 让读线程收完 stdout（孙进程可能仍持有管道，不能无限等）；
    // ⑤ 检查 deadline，超时就 kill（只发一次）。
    // 循环的出口只有：已退出且 stdout 收完 / 退出宽限到期，或 try_wait 本身报错。
    // 注意只有 deadline 分支的 kill 只发一次；ENOBUFS 分支每轮重发。若子进程不响应
    // kill 且 stdout 始终不关闭，循环会持续轮询。
    loop {
        // 非阻塞地取走已读到的输出块。
        loop {
            match stdout_receiver.try_recv() {
                Ok(chunk) => {
                    if !overflowed {
                        stdout_bytes.extend_from_slice(&chunk);
                        if stdout_bytes.len() > MAX_BUFFER_BYTES {
                            overflowed = true;
                            stdout_bytes.truncate(MAX_BUFFER_BYTES + 1);
                        }
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    reader_done = true;
                    break;
                }
            }
        }

        if overflowed && exit_status.is_none() && !timed_out {
            // Node: maxBuffer 超限 → 销毁 stdio、kill 子进程、`error.code = "ENOBUFS"`。
            let _ = child.kill();
        }

        if exit_status.is_none() {
            match child.try_wait() {
                Ok(Some(status)) => exit_status = Some(status),
                Ok(None) => {}
                Err(error) => {
                    wait_error = Some(error);
                    break;
                }
            }
        }

        if exit_status.is_some() {
            // 子进程已退出：最多再等 2 s（孙进程可能仍持有 stdout，不能无限等）。
            let grace = *exit_grace_deadline.get_or_insert_with(|| {
                std::time::Instant::now() + std::time::Duration::from_secs(2)
            });
            if reader_done || std::time::Instant::now() >= grace {
                break;
            }
        }

        if !timed_out && std::time::Instant::now() >= deadline {
            timed_out = true;
            // Node 的 `spawnSync` 超时后先发 killSignal（默认 SIGTERM），再等待退出。
            let _ = child.kill();
        }

        std::thread::sleep(std::time::Duration::from_millis(2));
    }

    // try_wait 报错会提前跳出循环，此时进程可能尚未退出：若之前已超时并 kill 过，
    // 再阻塞 wait 一次做回收，避免留下僵尸进程；kill 是否成功不影响最终结果。
    if timed_out && exit_status.is_none() {
        let _ = child.wait();
    }
    let _ = writer.join();
    // reader_done 才是真的读到 EOF；否则读线程可能仍阻塞在孙进程持有的 stdout 上，
    // 不能 join（否则本函数会越过退出宽限无限等待），让它随管道关闭自行结束。
    if reader_done {
        let _ = reader.join();
    }

    let error = if overflowed {
        Some(std::io::Error::other("ENOBUFS"))
    } else if timed_out {
        Some(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "ETIMEDOUT",
        ))
    } else {
        wait_error
    };

    SpawnSyncResult {
        // ENOBUFS/超时与 Node 一致：`status` 为 null，错误见 `error`。
        status: if overflowed {
            None
        } else {
            exit_status.and_then(|status| status.code())
        },
        stdout: String::from_utf8_lossy(&stdout_bytes).into_owned(),
        stderr: String::new(),
        error,
    }
}

/// `executeWithConfiguredShell(command)`。
fn execute_with_configured_shell(
    command: &str,
    shell_config: Result<ShellConfig, ShellConfigError>,
) -> ShellExecution {
    let Ok(shell_config) = shell_config else {
        // `getShellConfig()` 抛错：`catch` 分支 → `executed: false`。
        return ShellExecution {
            executed: false,
            value: None,
        };
    };

    let command_from_stdin = shell_config.command_transport == Some(CommandTransport::Stdin);
    let (args, stdin_input) = if command_from_stdin {
        (shell_config.args.clone(), Some(command))
    } else {
        let mut args = shell_config.args.clone();
        args.push(command.to_string());
        (args, None)
    };
    // TS: `spawnSync(..., { input, timeout: 10000, windowsHide: true })`。
    let result = spawn_sync_command(
        &shell_config.shell,
        &args,
        &SpawnRequest {
            stdin_input,
            windows_hide: true,
            windows_verbatim_arguments: false,
        },
        COMMAND_TIMEOUT_MS,
    );

    if let Some(error) = result.error {
        // `result.error.code === "ENOENT"` → 让 Windows 调用方回落到默认 shell。
        // 其它 spawn 错误视为“已尝试”（executed = true），避免在同一环境下再用默认
        // shell 重跑一遍。
        if error.kind() == std::io::ErrorKind::NotFound {
            return ShellExecution {
                executed: false,
                value: None,
            };
        }
        return ShellExecution {
            executed: true,
            value: None,
        };
    }

    if result.status != Some(0) {
        return ShellExecution {
            executed: true,
            value: None,
        };
    }

    ShellExecution {
        executed: true,
        value: command_result(&result.stdout),
    }
}

/// `execSync(command, {...})` 解析出的默认 shell 调用。
struct DefaultShellInvocation {
    /// shell 可执行文件。
    shell: String,
    /// 完整参数（含 command）。
    args: Vec<String>,
    /// `windowsVerbatimArguments`（Node 的 cmd.exe 分支为 `true`）。
    windows_verbatim_arguments: bool,
}

/// 默认 shell 的调用方式。
///
/// Unix：`execSync` 的默认 shell 是 `/bin/sh -c <command>`。
///
/// 这里固定该路径，不读取 `$SHELL`。
#[cfg(unix)]
fn default_shell_invocation(command: &str) -> DefaultShellInvocation {
    DefaultShellInvocation {
        shell: "/bin/sh".to_string(),
        args: vec!["-c".to_string(), command.to_string()],
        windows_verbatim_arguments: false,
    }
}

/// 默认 shell 的调用方式。
///
/// Windows：`process.env.ComSpec || "cmd.exe"`（见 [`windows_default_shell_invocation`]）。
#[cfg(windows)]
fn default_shell_invocation(command: &str) -> DefaultShellInvocation {
    let comspec = std::env::var("ComSpec")
        .or_else(|_| std::env::var("comspec"))
        .ok();
    let (shell, args, windows_verbatim_arguments) =
        windows_default_shell_invocation(comspec.as_deref(), command);
    DefaultShellInvocation {
        shell,
        args,
        windows_verbatim_arguments,
    }
}

/// Node `normalizeSpawnArguments()` 的 shell 分支（win32）：
/// `comspec || "cmd.exe"`；文件名匹配 `/^(?:.*\\)?cmd(?:\.exe)?$/i` 时参数为
/// `['/d','/s','/c', '"command"']` 且 `windowsVerbatimArguments = true`，否则 `['-c', command]`。
///
/// **test hook, not public API**：Windows 分支只保证编译，但纯参数构造在任意平台都能断言。
#[doc(hidden)]
pub fn windows_default_shell_invocation(
    comspec: Option<&str>,
    command: &str,
) -> (String, Vec<String>, bool) {
    let shell = comspec.unwrap_or("cmd.exe").to_string();
    if is_cmd_comspec(&shell) {
        (
            shell,
            vec![
                "/d".to_string(),
                "/s".to_string(),
                "/c".to_string(),
                format!("\"{command}\""),
            ],
            true,
        )
    } else {
        (shell, vec!["-c".to_string(), command.to_string()], false)
    }
}

/// `/^(?:.*\\)?cmd(?:\.exe)?$/i` 的等价物（只认反斜杠路径分隔符，与 Node 一致）。
fn is_cmd_comspec(file: &str) -> bool {
    let base = file.rsplit_once('\\').map_or(file, |(_, base)| base);
    let base = base.to_ascii_lowercase();
    base == "cmd" || base == "cmd.exe"
}

/// `executeWithDefaultShell(command)`：任何失败（spawn 错误、非零退出、超时、ENOBUFS）→ `None`。
fn execute_with_default_shell(command: &str) -> Option<String> {
    let invocation = default_shell_invocation(command);
    // TS: `execSync` 的默认选项不含 `windowsHide`（与 `executeWithConfiguredShell` 不同）。
    let result = spawn_sync_command(
        &invocation.shell,
        &invocation.args,
        &SpawnRequest {
            stdin_input: None,
            windows_hide: false,
            windows_verbatim_arguments: invocation.windows_verbatim_arguments,
        },
        COMMAND_TIMEOUT_MS,
    );
    if result.error.is_some() || result.status != Some(0) {
        return None;
    }
    command_result(&result.stdout)
}

/// `executeCommandUncached(commandConfig)`。
fn execute_command_uncached_with(
    command_config: &str,
    platform: Platform,
    shell_config: Option<Result<ShellConfig, ShellConfigError>>,
) -> Option<String> {
    // TS: `commandConfig.slice(1)`（`!` 是 ASCII，字节切片安全）。
    let command = command_config.strip_prefix('!').unwrap_or(command_config);

    if matches!(platform, Platform::Win32) {
        // Win32：先按配置的 shell 执行；只有根本没执行（shell 配置解析失败或 ENOENT）
        // 才回落到默认 shell，普通失败（非零退出、超时、ENOBUFS）不再重试。
        let configured_result = execute_with_configured_shell(
            command,
            shell_config.unwrap_or_else(|| get_shell_config(None)),
        );
        if configured_result.executed {
            configured_result.value
        } else {
            execute_with_default_shell(command)
        }
    } else {
        // 非 Windows 完全忽略 `shell_config`：Node 的 `execSync` 默认用 `/bin/sh -c`。
        execute_with_default_shell(command)
    }
}

/// `executeCommandUncached(commandConfig)`（真实平台）。
fn execute_command_uncached(command_config: &str) -> Option<String> {
    execute_command_uncached_with(command_config, Platform::current(), None)
}

/// `executeCommand(commandConfig)`：进程级缓存（成功与失败都缓存）。
///
/// 键是**含开头 `!` 的完整配置串**：`"!echo hi"` 与 `"! echo hi"` 是两个不同的键，
/// 不做任何规范化。
/// 失败结果以 `None` 缓存，因此失败命令不会在缓存清空之前被重复执行。
/// 查询缓存与写入缓存是两次独立加锁，没有 single-flight：并发的首次调用可能各自
/// 执行一次命令，后写入的结果覆盖先写入的。
fn execute_command(command_config: &str) -> Option<String> {
    let cached = cache_guard().get(command_config).cloned();
    if let Some(value) = cached {
        return value;
    }

    let result = execute_command_uncached(command_config);
    cache_guard().insert(command_config.to_string(), result.clone());
    result
}

/// `getConfigValueEnvVarName(config)`：模板且恰好一个 part 且是 env → 名字；否则 `None`。
///
/// 因此 `"$FOO"`、`"${FOO}"` 返回 `Some("FOO")`；`"$FOO$BAR"`、带字面量的
/// `"x$FOO"`、命令以及纯字面量都返回 `None`。
pub fn get_config_value_env_var_name(config: &str) -> Option<String> {
    match parse_config_value_reference(config) {
        ConfigValueReference::Command { .. } => None,
        ConfigValueReference::Template { parts } => match parts.as_slice() {
            [TemplatePart::Env { name }] => Some(name.clone()),
            _ => None,
        },
    }
}

/// `getConfigValueEnvVarNames(config)`：模板 → 按出现顺序去重的 env 名字；命令 → 空表。
///
/// 转义（`$$`、`$!`）与非法 `${...}` 都是字面量，不计入名字；重复名字只保留第一次
/// 出现的位置。
pub fn get_config_value_env_var_names(config: &str) -> Vec<String> {
    match parse_config_value_reference(config) {
        ConfigValueReference::Command { .. } => Vec::new(),
        ConfigValueReference::Template { parts } => template_env_var_names(&parts),
    }
}

/// `getMissingConfigValueEnvVarNames(config, env)`：过滤掉已解析到（非空）的名字。
///
/// 返回顺序与 [`get_config_value_env_var_names`] 一致。`env` 中命中空串会回退到进程
/// 环境，两层都取不到非空值才算缺失。
pub fn get_missing_config_value_env_var_names(
    config: &str,
    env: Option<&ConfigEnv>,
) -> Vec<String> {
    get_config_value_env_var_names(config)
        .into_iter()
        .filter(|name| resolve_env_config_value(name, env).is_none())
        .collect()
}

/// `isCommandConfigValue(config)`。
///
/// 只检查首字符是否为 `!`（与 `parse_config_value_reference` 的唯一判据一致），不校验
/// 命令内容或可执行性。`"$!x"` 以 `$` 开头，是模板转义，不是命令。
pub fn is_command_config_value(config: &str) -> bool {
    matches!(
        parse_config_value_reference(config),
        ConfigValueReference::Command { .. }
    )
}

/// `isConfigValueConfigured(config, env)`：缺失列表为空（命令恒为 `true`）。
///
/// 命令恒为 `true`（即使执行会失败或超时）；纯字面量也为 `true`；只有模板缺少至少
/// 一个非空环境变量时为 `false`。
pub fn is_config_value_configured(config: &str, env: Option<&ConfigEnv>) -> bool {
    get_missing_config_value_env_var_names(config, env).is_empty()
}

/// `resolveConfigValue(config, env)`：命令走缓存，模板逐段解析。
///
/// - 命令：经 `execute_command` 使用进程级缓存，命中时不启动子进程；stdout 已按 JS
///   `trim()` 裁剪，空输出 → `None`。
/// - 模板：每次调用都重新读取环境变量（不缓存）；任一变量缺失 → `None`。
pub fn resolve_config_value(config: &str, env: Option<&ConfigEnv>) -> Option<String> {
    match parse_config_value_reference(config) {
        ConfigValueReference::Command { config } => execute_command(&config),
        ConfigValueReference::Template { parts } => resolve_template(&parts, env),
    }
}

/// `resolveConfigValueUncached(config, env)`：命令每次都执行（不进缓存）。
///
/// 命令分支既不读也不写缓存，每次调用都会启动子进程；模板分支本来就不缓存，行为与
/// [`resolve_config_value`] 相同。
pub fn resolve_config_value_uncached(config: &str, env: Option<&ConfigEnv>) -> Option<String> {
    match parse_config_value_reference(config) {
        ConfigValueReference::Command { config } => execute_command_uncached(&config),
        ConfigValueReference::Template { parts } => resolve_template(&parts, env),
    }
}

/// 注入平台与 shell 配置的 `resolveConfigValueUncached`。**test hook, not public API**。
///
/// 对应 TS 测试里 `vi.spyOn(shellModule, "getShellConfig")` + 改写 `process.platform` 的组合；
/// 生产代码用 [`resolve_config_value_uncached`]。
///
/// `shell_config` 只在 `platform == Platform::Win32` 且值是命令时参与：Win32 先尝试它，
/// 它是 `Err` 或 spawn 返回 ENOENT 时回落默认 shell；其它平台始终走默认 shell，该参数
/// 被忽略。模板分支与平台无关，只做环境变量替换。
#[doc(hidden)]
pub fn resolve_config_value_uncached_with_shell(
    config: &str,
    env: Option<&ConfigEnv>,
    platform: Platform,
    shell_config: Result<ShellConfig, ShellConfigError>,
) -> Option<String> {
    match parse_config_value_reference(config) {
        ConfigValueReference::Command { config } => {
            execute_command_uncached_with(&config, platform, Some(shell_config))
        }
        ConfigValueReference::Template { parts } => resolve_template(&parts, env),
    }
}

/// `resolveConfigValueOrThrow` 的错误。`Display` 逐字等于 TS 的 `Error.message`（§3.6）。
///
/// 错误只描述“缺了什么”（命令本身或环境变量名），不携带底层 IO、退出码或超时原因：
/// TS 侧这些原因同样被折叠成 `undefined`，不会进入错误文案。`source()` 因此为 `None`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigValueError {
    /// `Failed to resolve ${description} from shell command: ${command}`（`command` 不含 `!`）。
    ShellCommand {
        /// 描述前缀（如 `API key for provider "x"`）。
        description: String,
        /// 去掉开头 `!` 的命令本身。
        command: String,
    },
    /// `Failed to resolve ${description} from environment variable: ${name}`。
    EnvironmentVariable {
        /// 描述前缀。
        description: String,
        /// 缺失的环境变量名。
        name: String,
    },
    /// `Failed to resolve ${description} from environment variables: ${name1}, ${name2}`。
    EnvironmentVariables {
        /// 描述前缀。
        description: String,
        /// 缺失的环境变量名（按出现顺序）。
        names: Vec<String>,
    },
    /// 兜底：`Failed to resolve ${description}`。
    Unresolved {
        /// 描述前缀。
        description: String,
    },
}

impl std::fmt::Display for ConfigValueError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigValueError::ShellCommand {
                description,
                command,
            } => write!(
                formatter,
                "Failed to resolve {description} from shell command: {command}"
            ),
            ConfigValueError::EnvironmentVariable { description, name } => write!(
                formatter,
                "Failed to resolve {description} from environment variable: {name}"
            ),
            ConfigValueError::EnvironmentVariables { description, names } => write!(
                formatter,
                "Failed to resolve {description} from environment variables: {}",
                names.join(", ")
            ),
            ConfigValueError::Unresolved { description } => {
                write!(formatter, "Failed to resolve {description}")
            }
        }
    }
}

impl std::error::Error for ConfigValueError {}

/// `resolveConfigValueOrThrow(config, description, env)`。
///
/// 值已解析 → `Ok`；命令 → [`ConfigValueError::ShellCommand`]；模板按缺失的环境变量数量
/// 给出单数/复数文案；兜底 [`ConfigValueError::Unresolved`]。
///
/// 注意它调用的是 [`resolve_config_value_uncached`]（跳过缓存）：命令每次都会真的
/// 执行，即使 [`resolve_config_value`] 已缓存过同一命令的结果。可解析为空串的值
/// （例如空配置串）返回 `Ok("")`，不当作失败。
pub fn resolve_config_value_or_throw(
    config: &str,
    description: &str,
    env: Option<&ConfigEnv>,
) -> Result<String, ConfigValueError> {
    if let Some(resolved) = resolve_config_value_uncached(config, env) {
        return Ok(resolved);
    }

    match parse_config_value_reference(config) {
        ConfigValueReference::Command {
            config: command_config,
        } => Err(ConfigValueError::ShellCommand {
            description: description.to_string(),
            command: command_config
                .strip_prefix('!')
                .unwrap_or(&command_config)
                .to_string(),
        }),
        ConfigValueReference::Template { .. } => {
            let missing = get_missing_config_value_env_var_names(config, env);
            if missing.len() == 1 {
                Err(ConfigValueError::EnvironmentVariable {
                    description: description.to_string(),
                    name: missing[0].clone(),
                })
            } else if missing.len() > 1 {
                Err(ConfigValueError::EnvironmentVariables {
                    description: description.to_string(),
                    names: missing,
                })
            } else {
                // 兜底：解析失败但重新统计的缺失变量为空（例如两次读取之间进程环境
                // 恰好发生变化），仍给出 `Failed to resolve ${description}`。
                Err(ConfigValueError::Unresolved {
                    description: description.to_string(),
                })
            }
        }
    }
}

/// [`resolve_config_value_or_throw`] 的 Rust 风格别名。
///
/// 行为完全等价（一行转发）的别名。
pub fn resolve_config_value_or_error(
    config: &str,
    description: &str,
    env: Option<&ConfigEnv>,
) -> Result<String, ConfigValueError> {
    resolve_config_value_or_throw(config, description, env)
}

/// `resolveHeaders(headers, env)`：逐项解析，**只保留非空值**；结果为空 → `None`。
///
/// 输入 `None` → `None`。
///
/// 每个值都经 [`resolve_config_value`]，命令结果会复用进程级缓存。解析失败（环境变量
/// 缺失、命令失败/超时）或解析结果为空串时，该 header 被**静默丢弃**，不会报错；
/// 输出保持输入的键顺序（`IndexMap`）。输入空映射或全部条目被丢弃 → `None`。
pub fn resolve_headers(
    headers: Option<&IndexMap<String, String>>,
    env: Option<&ConfigEnv>,
) -> Option<IndexMap<String, String>> {
    let headers = headers?;
    let mut resolved = IndexMap::new();
    for (key, value) in headers {
        if let Some(resolved_value) = resolve_config_value(value, env)
            && !resolved_value.is_empty()
        {
            resolved.insert(key.clone(), resolved_value);
        }
    }
    if resolved.is_empty() {
        None
    } else {
        Some(resolved)
    }
}

/// `resolveHeadersOrThrow(headers, description, env)`。
///
/// 输入 `None` → `Ok(None)`；逐项错误描述是 `${description} header "${key}"`；
/// 结果为空对象 → `Ok(None)`（空串值会保留，因为 `OrThrow` 要么抛错要么返回非 undefined）。
///
/// 每个 header 用 [`resolve_config_value_or_throw`] 解析，因此命令**不走缓存**，每次调用
/// 都会执行。这里的“保留空串”指解析恰好得到 `Some("")` 的情形（如字面量 `""`）；命令
/// 失败或环境变量缺失仍会返回错误。输出保持输入的键顺序（`IndexMap`）。
pub fn resolve_headers_or_throw(
    headers: Option<&IndexMap<String, String>>,
    description: &str,
    env: Option<&ConfigEnv>,
) -> Result<Option<IndexMap<String, String>>, ConfigValueError> {
    let Some(headers) = headers else {
        return Ok(None);
    };
    let mut resolved = IndexMap::new();
    for (key, value) in headers {
        let resolved_value =
            resolve_config_value_or_throw(value, &format!("{description} header \"{key}\""), env)?;
        resolved.insert(key.clone(), resolved_value);
    }
    if resolved.is_empty() {
        Ok(None)
    } else {
        Ok(Some(resolved))
    }
}
