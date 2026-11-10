//! `cli/setup.ts`（13 行）：`setupCli()`——进程标题、`KIANA_CODING_AGENT`/`AI_AGENT` 标记、
//! HTTP dispatcher、警告抑制。
//!
//! 本文件同时承载 crate 级共享基础设施（spec/ca-cli.md §8.1）：
//!
//! - [`Output`]：stdout/stderr 输出抽象。所有用户可见字节都必须经过它，模块内禁止
//!   `println!`/`eprintln!`；stdout 通道遵循 [`kiana_core::output_guard`] 的接管语义。
//! - [`Style`]：chalk v6 的 16 色样式（`red`/`yellow`/`green`/`cyan`/`dim`/`bold`/
//!   `italic`/`underline`/`strikethrough`），颜色级别每进程只检测一次。
//! - [`CliExit`] / [`CliResult`]：库函数用返回值表达 `process.exit(code)`，由二进制入口
//!   决定何时真正退出。
//!
//! TS: `packages/coding-agent/src/cli/setup.ts#setupCli`、`utils/ansi`（chalk 级别）。
//!
//! # 在启动链上的位置
//!
//! `kiana`（`src/main.rs`）在创建 tokio 运行时之前调用 [`setup_cli`]，随后进入
//! `app::main`；`kiana-rpc`（`src/bin/kiana-rpc.rs`）不调用它（否则进程标题会从
//! `kiana-rpc` 变成 `kiana`），而是自行设置环境标记与 dispatcher，只复用
//! [`install_fatal_startup_panic_hook`]。
//!
//! 其余模块把这里当作共享基础设施：`cli/args`、`cli/file_processor`、`cli/project_trust`、
//! `app`、`migrations`、`package_manager_cli` 等分别导入 [`Output`] / [`Style`] / [`CliExit`]
//! 等类型，迁移与包管理命令还用 [`node_fs_error_message`] 生成 Node 风格的 fs 错误文案。
//!
//! # 失败模式
//!
//! - 普通 stdout 直写失败时回退到 `kiana_core::output_guard::write_stdout`；该模块的写线程
//!   遇到不可恢复错误会 `exit(1)`。stderr 的写错误被忽略。
//! - 捕获模式的 `write_stdout` / `write_stderr` 只写内存缓冲，不接触真实 fd，因此不会因管道
//!   关闭而失败（`write_raw_stdout` 例外，它始终走真实 stdout）。
//! - [`FatalStartupError`] 只有在 [`install_fatal_startup_panic_hook`] 安装之后才会变成
//!   `Error: <message>` + 退出码 1；未安装时按普通 panic 展开，测试可以用 `catch_unwind`
//!   读取 payload。
//!
//! # 阅读顺序
//!
//! [`Output`] → [`Style`]（以及私有的 `chalk_color_enabled` 判定链）→ [`node_error_parts`] /
//! [`node_fs_error_message`] → [`FatalStartupError`] → [`setup_cli`] → 文件末尾的测试。

use std::io::Write;
use std::sync::{Arc, Mutex, Once, OnceLock};

use kiana_core::http_dispatcher::{DEFAULT_HTTP_IDLE_TIMEOUT_MS, configure_http_dispatcher};

/// CLI 库层的退出请求：对应 TS 里直接调用 `process.exit(code)` 的路径。
///
/// 库函数返回 `Err(CliExit { code })`，由二进制入口在打印完消息后调用
/// `std::process::exit`；这样测试与嵌入调用方都能观察到退出码而不终止进程。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CliExit {
    /// 进程退出码。
    pub code: i32,
}

impl CliExit {
    /// 以给定退出码构造。
    pub fn new(code: i32) -> Self {
        Self { code }
    }
}

/// 库层函数的返回类型；`Err` 表示调用方应以其退出码结束进程。
pub type CliResult<T> = Result<T, CliExit>;

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------

/// stdout/stderr 输出通道（`console.log` / `console.error` / `process.stdout.write`）。
///
/// - 普通 stdout 输出同步写 fd 1（未接管时）或 fd 2（接管时，与 TS 替换
///   `process.stdout.write` 的行为一致）；同步写入保证进程退出前字节已经落盘。
/// - stderr 输出始终写 fd 2。
/// - [`Output::capture`] 构造一个把两个通道收集到内存的实现，供单元测试断言逐字输出；
///   它不影响进程的真实 stdout/stderr。
/// - `Clone` 是浅拷贝：进程模式下克隆没有额外状态；捕获模式下克隆共享同一个
///   `Arc<Mutex<…>>`，通过任一克隆写入的文本都能从原实例的 [`Output::captured_stdout`] /
///   [`Output::captured_stderr`] 读到（测试把 `Output` 传入被测代码，再从原实例断言）。
/// - 捕获优先于接管：即使进程已 `take_over_stdout`，捕获实例仍把 stdout 文本收进内存，
///   不写真实 fd。
///
/// `console.log(x)` 等价于 [`Output::write_line_stdout`]（追加一个 `"\n"`）；
/// `console.log()` 等价于 `write_line_stdout("")`（只写一个换行）。
#[derive(Clone)]
pub struct Output {
    capture: Option<Arc<Mutex<CaptureBuffers>>>,
}

/// 捕获模式下两个通道的累积缓冲；按写入顺序拼接为 UTF-8 文本。
#[derive(Default)]
struct CaptureBuffers {
    stdout: String,
    stderr: String,
}

// 手写 `Debug`：只报告是否处于捕获模式，不打印已捕获的 CLI 输出。
impl std::fmt::Debug for Output {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Output")
            .field("capture", &self.capture.is_some())
            .finish()
    }
}

impl Output {
    /// 进程输出：stdout 走接管感知通道，stderr 直写。
    ///
    /// 不缓存接管状态：每次写入都查询 `output_guard::is_stdout_taken_over()`，因此之后
    /// `take_over_stdout` / `restore_stdout` 对已创建的实例立即生效。
    pub fn process() -> Self {
        Self { capture: None }
    }

    /// 测试用的内存输出；通过 [`Output::captured_stdout`] / [`Output::captured_stderr`] 读取。
    ///
    /// 写入不接触真实 fd，两个通道分别累积、互不混入。
    pub fn capture() -> Self {
        Self {
            capture: Some(Arc::new(Mutex::new(CaptureBuffers::default()))),
        }
    }

    /// 是否处于捕获模式。
    pub fn is_capture(&self) -> bool {
        self.capture.is_some()
    }

    /// 当前捕获到的 stdout 文本（进程模式返回空串）。
    pub fn captured_stdout(&self) -> String {
        self.capture
            .as_ref()
            .map(|capture| lock_capture(capture).stdout.clone())
            .unwrap_or_default()
    }

    /// 当前捕获到的 stderr 文本（进程模式返回空串）。
    pub fn captured_stderr(&self) -> String {
        self.capture
            .as_ref()
            .map(|capture| lock_capture(capture).stderr.clone())
            .unwrap_or_default()
    }

    /// `process.stdout.write(text)`（接管时写 stderr）。
    ///
    /// 与 TS 的差异：TS 的 `process.stdout.write` 由 Node 的流负责缓冲，并在**正常退出时**
    /// 排空；Rust 没有进程级退出钩子，`kiana_core::output_guard::write_stdout` 的队列是
    /// “发后不管”，进程若在 `flush()` 之前返回就会丢掉输出（help / list-models / 迁移提示
    /// 都是这样输出的）。因此这里直接同步写目标 fd 并 flush：`console.log` 的字节在调用
    /// 返回时已经离开进程。接管状态下仍写 stderr（与 TS 替换 `process.stdout.write` 一致）。
    ///
    /// 协议字节（[`Output::write_raw_stdout`]）仍走 `output_guard` 的串行队列；未接管时的
    /// 普通输出与它不再共享队列，但两种通道分别用于交互/CLI 文本与 print/RPC 协议，不会在
    /// 同一模式内混用。
    pub fn write_stdout(&self, text: &str) {
        match &self.capture {
            Some(capture) => lock_capture(capture).stdout.push_str(text),
            None => {
                if kiana_core::output_guard::is_stdout_taken_over() {
                    let stderr = std::io::stderr();
                    let mut handle = stderr.lock();
                    let _ = handle.write_all(text.as_bytes());
                    let _ = handle.flush();
                    return;
                }
                let stdout = std::io::stdout();
                let mut handle = stdout.lock();
                if handle
                    .write_all(text.as_bytes())
                    .and_then(|()| handle.flush())
                    .is_err()
                {
                    // 保持 `output_guard` 的错误语义（写入失败按 TS 的 raw 链 exit(1)）。
                    // 回退重新排入的是整段 `text`，不补偿直写可能已经写出的部分字节。
                    kiana_core::output_guard::write_stdout(text);
                }
            }
        }
    }

    /// `console.error(text)`：写入 stderr。
    ///
    /// 不受 stdout 接管影响：接管只改变普通 stdout 的去向；进程模式始终写 fd 2，捕获模式
    /// 写入 `stderr` 缓冲。
    pub fn write_stderr(&self, text: &str) {
        match &self.capture {
            Some(capture) => lock_capture(capture).stderr.push_str(text),
            None => {
                let stderr = std::io::stderr();
                let mut handle = stderr.lock();
                // 与 Node 的 console.error 一样，写入错误不改变控制流。
                let _ = handle.write_all(text.as_bytes());
                let _ = handle.flush();
            }
        }
    }

    /// `console.log(text)`：追加一个换行（单次写入，与 Node 的一次 `write` 一致）。
    pub fn write_line_stdout(&self, text: &str) {
        self.write_stdout(&format!("{text}\n"));
    }

    /// `console.error(text)`（带换行）。
    pub fn write_line_stderr(&self, text: &str) {
        self.write_stderr(&format!("{text}\n"));
    }

    /// `writeRawStdout(text)`：协议字节，始终写真实 stdout。
    ///
    /// 不受捕获模式与接管状态影响：捕获实例也写进程级 raw 队列、最终落到 fd 1。
    /// 空串被丢弃（不产生写入任务）。函数不等待写出完成——“发后不管”，退出前必须
    /// `await` [`Output::flush`]。
    pub fn write_raw_stdout(&self, text: &str) {
        kiana_core::output_guard::write_raw_stdout(text);
    }

    /// 等待原始 stdout 队列排空（print/RPC 退出前必须 await）。
    ///
    /// 冲刷的是进程级队列，与 `self` 是否捕获无关。等待没有超时：只要仍有新写入追加，
    /// 就会继续等待（见 `kiana_core::output_guard` 的背压循环）。普通 stdout 输出已经同步
    /// 写出，不需要调用它。
    pub async fn flush(&self) {
        kiana_core::output_guard::flush_raw_stdout().await;
    }
}

/// 取捕获缓冲锁。锁被毒化（持锁线程 panic）时取回内部数据而不是再次 panic，让断言仍能
/// 读到已经写入的内容。
fn lock_capture(capture: &Mutex<CaptureBuffers>) -> std::sync::MutexGuard<'_, CaptureBuffers> {
    capture
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

// ---------------------------------------------------------------------------
// Style（chalk v6）
// ---------------------------------------------------------------------------

/// chalk v6 的样式集合。
///
/// 颜色级别由 [`Style::detect`] 每进程检测一次（与 chalk 在模块加载时固定 level 一致）。
/// 未启用颜色时所有方法都返回原文。嵌套语义与 chalk 相同：`style.bold(&style.yellow(x))`
/// 产生 `\x1b[1m\x1b[33m…\x1b[39m\x1b[22m`（内层先关闭）。
///
/// `Style` 的全部状态就是这个布尔值，并实现了 `Copy`；是否带颜色完全由构造方式决定
/// （[`Style::detect`]、[`Style::plain`]、[`Style::with_enabled`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Style {
    enabled: bool,
}

/// 进程级颜色开关缓存：第一次 [`Style::detect`] 时求值，之后所有 `Style` 共用；运行期再改
/// 环境变量或重定向 stdout 都不会更新它。
static COLOR_ENABLED: OnceLock<bool> = OnceLock::new();

impl Style {
    /// 按当前进程的 TTY/环境变量/argv 检测颜色（首次调用后固定）。
    ///
    /// 结果缓存在私有的 `COLOR_ENABLED`（`OnceLock`）里；此后再改 `FORCE_COLOR` / `TERM`
    /// 或把 stdout 重定向都不会影响本进程。需要确定性输出时用 [`Style::plain`]。
    pub fn detect() -> Self {
        Self {
            enabled: *COLOR_ENABLED.get_or_init(chalk_color_enabled),
        }
    }

    /// 无颜色（测试与确定性输出）。
    pub fn plain() -> Self {
        Self { enabled: false }
    }

    /// 显式启用/禁用。
    pub fn with_enabled(enabled: bool) -> Self {
        Self { enabled }
    }

    /// 是否启用颜色。
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    fn wrap(&self, text: &str, open: &str, close: &str) -> String {
        if self.enabled {
            format!("\u{1b}[{open}m{text}\u{1b}[{close}m")
        } else {
            text.to_string()
        }
    }

    /// `chalk.red`（31/39）。
    pub fn red(&self, text: &str) -> String {
        self.wrap(text, "31", "39")
    }

    /// `chalk.green`（32/39）。
    pub fn green(&self, text: &str) -> String {
        self.wrap(text, "32", "39")
    }

    /// `chalk.yellow`（33/39）。
    pub fn yellow(&self, text: &str) -> String {
        self.wrap(text, "33", "39")
    }

    /// `chalk.cyan`（36/39）。
    pub fn cyan(&self, text: &str) -> String {
        self.wrap(text, "36", "39")
    }

    /// `chalk.dim`（2/22）。
    pub fn dim(&self, text: &str) -> String {
        self.wrap(text, "2", "22")
    }

    /// `chalk.bold`（1/22）。
    pub fn bold(&self, text: &str) -> String {
        self.wrap(text, "1", "22")
    }

    /// `chalk.italic`（3/23）。
    pub fn italic(&self, text: &str) -> String {
        self.wrap(text, "3", "23")
    }

    /// `chalk.underline`（4/24）。
    pub fn underline(&self, text: &str) -> String {
        self.wrap(text, "4", "24")
    }

    /// `chalk.strikethrough`（9/29）。
    pub fn strikethrough(&self, text: &str) -> String {
        self.wrap(text, "9", "29")
    }
}

// ---------------------------------------------------------------------------
// chalk v6 颜色级别（vendored supports-color 的 stdout 分支）
// ---------------------------------------------------------------------------

/// chalk 6 的颜色使能判定（`node_modules/chalk/source/vendor/supports-color` 的 `stdout`
/// 分支，取 `level > 0`）。
///
/// 与 `kiana-config` 的 resolver 中已验证的实现相同：
/// - chalk 的默认实例用 **stdout** 的检测结果决定所有样式的 level（即使 `console.error`
///   写 stderr）；
/// - chalk 6 的 vendored `supports-color` **不读取** `NO_COLOR`（只有 `FORCE_COLOR` 与
///   `--color` 等 argv 标志）。
fn chalk_color_enabled() -> bool {
    // 阶段 1：argv 里的显式颜色标志。`has_flag` 只做完整 token 相等匹配，并且忽略 `--`
    // 终止符之后的参数（与 supports-color 的 hasFlag 一致）。
    let arguments = argv_strings(std::env::args_os());
    let has_flag = |flag: &str| -> bool {
        let prefix = if flag.starts_with('-') {
            ""
        } else if flag.len() == 1 {
            "-"
        } else {
            "--"
        };
        let needle = format!("{prefix}{flag}");
        let position = arguments.iter().position(|argument| *argument == needle);
        let terminator = arguments.iter().position(|argument| argument == "--");
        match position {
            Some(position) => terminator.is_none_or(|terminator| position < terminator),
            None => false,
        }
    };
    let mut flag_force_color: Option<u8> = None;
    // `--no-color` 家族优先于 `--color` 家族；两者都不存在时保持 `None`。
    if has_flag("no-color")
        || has_flag("no-colors")
        || has_flag("color=false")
        || has_flag("color=never")
    {
        flag_force_color = Some(0);
    } else if has_flag("color")
        || has_flag("colors")
        || has_flag("color=true")
        || has_flag("color=always")
    {
        flag_force_color = Some(1);
    }

    // 阶段 2：`FORCE_COLOR` 覆盖 argv 的结论（supports-color 的 envForceColor）。
    // `parse_force_color_level` 对无法识别的值返回 `None`，此时保留 argv 的决定。
    let force_color_value = || -> Option<u8> {
        let value = std::env::var_os("FORCE_COLOR")?;
        parse_force_color_level(&value.to_string_lossy())
    };
    let numeric_force_color = std::env::var_os("FORCE_COLOR").is_some_and(|value| {
        let value = value.to_string_lossy();
        !value.is_empty() && value.chars().all(|character| character.is_ascii_digit())
    });
    if let Some(level) = force_color_value() {
        flag_force_color = Some(level);
    }
    let force_color = flag_force_color;
    // 阶段 3：显式关闭（`--no-color` / `--color=false`，或 `FORCE_COLOR=false/0`）立即返回，
    // 跳过终端探测。
    if force_color == Some(0) {
        return false;
    }
    // `--color=16m` / `--color=256` 等显式能力标志直接开启，不受 stdout 是否为 TTY 限制
    // （能走到这里说明 `force_color` 不是 0）。
    if has_flag("color=16m") || has_flag("color=full") || has_flag("color=truecolor") {
        return true;
    }
    if has_flag("color=256") {
        return true;
    }
    // 数字形式的 FORCE_COLOR 指定精确 level；`true` / 空串只表示“启用”，level 继续检测。
    // 纯数字时在此直接开启（0 已在上面的 `Some(0)` 分支返回），同样不看 TTY。
    if force_color.is_some() && numeric_force_color {
        return force_color != Some(0);
    }
    let env_var_present = |name: &str| std::env::var_os(name).is_some();
    let env_var_equals =
        |name: &str, expected: &str| std::env::var_os(name).is_some_and(|value| value == expected);
    // 阶段 4：Azure DevOps 流水线（`TF_BUILD` + `AGENT_NAME`）即使没有 TTY 也支持颜色，
    // 因此判定放在 TTY 检查之前。
    if env_var_present("TF_BUILD") && env_var_present("AGENT_NAME") {
        return true;
    }
    use std::io::IsTerminal;
    // 阶段 5：stdout 不是终端、且没有解析出强制颜色值（没有 `--color`，或 `FORCE_COLOR`
    // 缺失/无法识别）时关闭。检测的是 **stdout**，所以 stderr 文案的颜色也跟随 stdout 是否
    // 被重定向（chalk 默认实例的语义）。
    if !std::io::stdout().is_terminal() && force_color.is_none() {
        return false;
    }
    let min = force_color.unwrap_or(0);
    // 阶段 6：终端能力探测，顺序与 supports-color 一致：
    // TERM=dumb → Windows → CI 服务 → TeamCity → COLORTERM / TERM / TERM_PROGRAM。
    if env_var_equals("TERM", "dumb") {
        return min > 0;
    }
    if cfg!(windows) {
        // Windows 终端按支持颜色处理（supports-color 的 win32 分支）。
        return true;
    }
    if env_var_present("CI") {
        // 已知的 CI 服务直接开启；其它 CI 只有 min > 0（显式要求颜色）时开启。
        if ["GITHUB_ACTIONS", "GITEA_ACTIONS", "CIRCLECI"]
            .iter()
            .any(|name| env_var_present(name))
        {
            return true;
        }
        if ["TRAVIS", "APPVEYOR", "GITLAB_CI", "BUILDKITE", "DRONE"]
            .iter()
            .any(|name| env_var_present(name))
            || env_var_equals("CI_NAME", "codeship")
        {
            return true;
        }
        return min > 0;
    }
    // TeamCity 按版本判断：9.1+ 或两位以上版本号支持颜色。
    if let Some(version) = std::env::var_os("TEAMCITY_VERSION") {
        let version = version.to_string_lossy().into_owned();
        return teamcity_supports_color(&version);
    }
    if env_var_equals("COLORTERM", "truecolor") {
        return true;
    }
    let term = std::env::var_os("TERM").map(|value| value.to_string_lossy().into_owned());
    if let Some(term) = term.as_deref()
        && matches!(term, "xterm-kitty" | "xterm-ghostty" | "wezterm")
    {
        return true;
    }
    if let Some(term_program) = std::env::var_os("TERM_PROGRAM") {
        let term_program = term_program.to_string_lossy();
        if term_program == "iTerm.app" || term_program == "Apple_Terminal" {
            return true;
        }
    }
    if let Some(term) = term.as_deref()
        && (term_has_256_color(term) || term_has_basic_color(term))
    {
        return true;
    }
    if env_var_present("COLORTERM") {
        return true;
    }
    // 没有识别出终端能力时，只有显式要求颜色（`--color` 或 `FORCE_COLOR=true`，即
    // `min > 0`）才开启。
    min > 0
}

/// `process.argv` 的等价物：Node 对 argv 做 UTF-8 有损解码。
///
/// 这里用 `args_os()` 而不是 `args()`：后者遇到非 UTF-8 参数会 panic，Node 不会。
fn argv_strings(arguments: impl IntoIterator<Item = std::ffi::OsString>) -> Vec<String> {
    arguments
        .into_iter()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect()
}

/// `/^(?:9\.0*[1-9]\d*\.|\d{2,}\.)/`：TeamCity 9.1+ 支持颜色。
///
/// `9.` 前缀要求第一个非零数字后紧跟 `.`（`9.01.2` 通过，`9.0.1` 不通过）；其它版本要求
/// 至少两位数字后跟 `.`（`10.` 通过，`9.` 不通过）。
fn teamcity_supports_color(version: &str) -> bool {
    if let Some(rest) = version.strip_prefix("9.") {
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        if digits > 0 {
            let number: String = rest.chars().take(digits).collect();
            let significant = number.trim_start_matches('0');
            return !significant.is_empty() && rest.as_bytes().get(digits) == Some(&b'.');
        }
        return false;
    }
    let leading_digits = version.bytes().take_while(u8::is_ascii_digit).count();
    leading_digits >= 2 && version.as_bytes().get(leading_digits) == Some(&b'.')
}

/// `/-256(?:color)?$/i`（先转小写再匹配，等价于正则的 `i`）。
fn term_has_256_color(term: &str) -> bool {
    let lower = term.to_lowercase();
    lower.ends_with("-256") || lower.ends_with("-256color")
}

/// `/^screen|^xterm|^vt100|^vt220|^rxvt|color|ansi|cygwin|linux/i`（同上，先转小写）。
fn term_has_basic_color(term: &str) -> bool {
    let lower = term.to_lowercase();
    lower.starts_with("screen")
        || lower.starts_with("xterm")
        || lower.starts_with("vt100")
        || lower.starts_with("vt220")
        || lower.starts_with("rxvt")
        || lower.contains("color")
        || lower.contains("ansi")
        || lower.contains("cygwin")
        || lower.contains("linux")
}

/// supports-color 的 `envForceColor()`：`FORCE_COLOR` → level。
///
/// - `"false"` → 0，`"true"` / 空串 → 1；其它非数字字符串 → `None`（表示“未指定”，
///   调用方保留 argv 的判定）。
/// - 纯数字逐位饱和累加并封顶 3，因此超长数字串返回 `Some(3)` 而不是溢出。
fn parse_force_color_level(value: &str) -> Option<u8> {
    if value == "false" {
        return Some(0);
    }
    if value == "true" || value.is_empty() {
        return Some(1);
    }
    if !value.chars().all(|character| character.is_ascii_digit()) {
        return None;
    }
    let mut level: u8 = 0;
    for byte in value.bytes() {
        level = level.saturating_mul(10).saturating_add(byte - b'0').min(3);
    }
    Some(level)
}

// ---------------------------------------------------------------------------
// Node `fs` 错误文案
// ---------------------------------------------------------------------------

/// Linux 的 errno → libuv 的 `(code, description)`（Node 26 实测）。
///
/// 只看 `ErrorKind` 会把 EPERM 误报成 EACCES、把 ELOOP/EBUSY 等归入 UNKNOWN，
/// 因此优先按原始 errno 查表。数值在 Linux 上固定；其它平台退回 `ErrorKind`。
///
/// 表未覆盖的 errno 继续走 `ErrorKind` 映射或 UNKNOWN 回退。
#[cfg(target_os = "linux")]
const UNIX_ERRNO_PARTS: &[(i32, &str, &str)] = &[
    (1, "EPERM", "operation not permitted"),
    (2, "ENOENT", "no such file or directory"),
    (5, "EIO", "i/o error"),
    (12, "ENOMEM", "not enough memory"),
    (13, "EACCES", "permission denied"),
    (16, "EBUSY", "resource busy or locked"),
    (17, "EEXIST", "file already exists"),
    (18, "EXDEV", "cross-device link not permitted"),
    (20, "ENOTDIR", "not a directory"),
    (21, "EISDIR", "illegal operation on a directory"),
    (22, "EINVAL", "invalid argument"),
    (23, "ENFILE", "too many open files in system"),
    (24, "EMFILE", "too many open files"),
    (28, "ENOSPC", "no space left on device"),
    (30, "EROFS", "read-only file system"),
    (36, "ENAMETOOLONG", "name too long"),
    (39, "ENOTEMPTY", "directory not empty"),
    (40, "ELOOP", "too many symbolic links encountered"),
];

/// 底层 `io::Error` → Node/libuv 的 `(code, description)`。
///
/// 返回值只包含 `<code>: <description>` 两段；`<syscall> [path] [-> dest]` 由
/// [`node_fs_error_message`] 或调用方拼接。`code` 是 libuv 的错误码（如 `"EPERM"`、
/// `"UNKNOWN"`），`description` 是 libuv 风格的小写文案。
///
/// 解析顺序：
/// 1. Linux 上优先用 `raw_os_error()` 查私有的 `UNIX_ERRNO_PARTS`：`ErrorKind` 无法区分
///    `EPERM` 与 `EACCES`（都映射到 `PermissionDenied`），只按 kind 会把 EPERM 误报成
///    EACCES；
/// 2. 没有原始 errno、或表里没有该 errno 时，按 `ErrorKind` 映射常见的 Node 错误码；
/// 3. 仍未命中时返回 `("UNKNOWN", 文案)`：文案取自 `Display`，去掉 `" (os error N)"` 后缀
///    并把首字母小写。
///
/// 非 Linux 平台没有 errno 表（整段被 `cfg` 编译掉），直接走第 2、3 步。
///
/// 调用方：`cli/file_processor` 用它拼 `open`/`read` 阶段的读取错误；迁移与
/// `package_manager_cli` 通过 [`node_fs_error_message`] 间接使用。
pub fn node_error_parts(error: &std::io::Error) -> (&'static str, String) {
    #[cfg(target_os = "linux")]
    if let Some(raw) = error.raw_os_error()
        && let Some((_, code, message)) = UNIX_ERRNO_PARTS.iter().find(|(errno, ..)| *errno == raw)
    {
        return (code, (*message).to_string());
    }
    use std::io::ErrorKind;
    let known = match error.kind() {
        ErrorKind::NotFound => Some(("ENOENT", "no such file or directory")),
        ErrorKind::PermissionDenied => Some(("EACCES", "permission denied")),
        ErrorKind::AlreadyExists => Some(("EEXIST", "file already exists")),
        ErrorKind::IsADirectory => Some(("EISDIR", "illegal operation on a directory")),
        ErrorKind::NotADirectory => Some(("ENOTDIR", "not a directory")),
        ErrorKind::DirectoryNotEmpty => Some(("ENOTEMPTY", "directory not empty")),
        ErrorKind::ReadOnlyFilesystem => Some(("EROFS", "read-only file system")),
        ErrorKind::StorageFull => Some(("ENOSPC", "no space left on device")),
        ErrorKind::CrossesDevices => Some(("EXDEV", "cross-device link not permitted")),
        ErrorKind::InvalidInput => Some(("EINVAL", "invalid argument")),
        _ => None,
    };
    if let Some((code, message)) = known {
        return (code, message.to_string());
    }
    // libuv 文案小写开头；Rust 的 strerror 大写开头且带 " (os error N)"。
    let display = error.to_string();
    let message = display
        .rsplit_once(" (os error ")
        .map_or(display.as_str(), |(message, _)| message);
    let mut characters = message.chars();
    let message = match characters.next() {
        Some(first) => first.to_lowercase().collect::<String>() + characters.as_str(),
        None => String::new(),
    };
    ("UNKNOWN", message)
}

/// Node 风格的消息：`<code>: <description>, <syscall> [path] [-> dest]`。
///
/// `path`/`dest` 用 `'…'` 包裹；`read`/`write` 阶段 libuv 通常不带路径，由调用方传 `None`。
/// `dest` 只用于 `rename` 这类有两个路径的调用（第二个路径出现在 ` -> ` 之后）。
///
/// 路径以原样字符串拼入消息，不做转义；调用方负责给出 libuv 的 syscall 名（`open`、
/// `write`、`rename`、`mkdir`、`mkdtemp` 等），本函数不做推断。
pub fn node_fs_error_message(
    error: &std::io::Error,
    syscall: &str,
    path: Option<&std::path::Path>,
    dest: Option<&std::path::Path>,
) -> String {
    let (code, description) = node_error_parts(error);
    let mut message = format!("{code}: {description}, {syscall}");
    if let Some(path) = path {
        message.push_str(&format!(" '{}'", path.display()));
    }
    if let Some(dest) = dest {
        message.push_str(&format!(" -> '{}'", dest.display()));
    }
    message
}

// ---------------------------------------------------------------------------
// 启动期致命错误
// ---------------------------------------------------------------------------

/// 启动期无法继续的错误（TS 里是逃出 `runMigrations` 的未捕获异常）。
///
/// Rust 侧用 panic 传播（[`run_migrations`](crate::migrations::run_migrations) 的签名已冻结），
/// 由 [`install_fatal_startup_panic_hook`] 在二进制入口把这类 panic 映射成 Node 的行为：
/// 打印 `Error: <message>` 到 stderr 并以退出码 1 结束。没有安装钩子时（单元测试）它仍可被
/// `catch_unwind` 捕获并检查消息。
///
/// `message` 是已经拼好的 Node `Error.message`（通常来自 [`node_fs_error_message`]，或启动
/// UI 的错误文本）；钩子只加 `Error: ` 前缀，不再改写内容。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FatalStartupError {
    /// Node `Error.message`（例如 `EACCES: permission denied, open '/x'`）。
    pub message: String,
}

/// 把 [`FatalStartupError`] panic 映射为 Node 的未捕获异常行为：stderr + 退出码 1。
///
/// TS 的 `runMigrations` 让 `mkdirSync`/`writeFileSync` 的同步错误直接逃逸，Node 打印
/// `Error: <message>`（含 stack，Rust 无法复刻）并以 1 退出；默认的 Rust panic 会以 101
/// 退出并打印 `thread 'main' panicked at …`，两者在可观察行为上不同。只拦截本类型，
/// 其它 panic 交给之前的钩子。
///
/// 进程级且只安装一次：内部的 `Once` 保证后续调用是 no-op。安装时 `take_hook` 保存之前的
/// 钩子，非 [`FatalStartupError`] 的 panic 仍交给它处理；默认钩子会打印 `thread … panicked
/// at …`，主线程上的 panic 最终以 101 退出。
///
/// 匹配到本类型时在钩子里直接 `std::process::exit(1)`，panic **不会展开**，因此同一进程里
/// 即使包了 `catch_unwind` 也看不到 payload（本文件的测试用子进程验证退出码）。钩子在 panic
/// 所在线程执行，`Output::process().write_line_stderr` 是同步写并 flush 的，字节在 `exit`
/// 前已经写出。
///
/// 调用点：`setup_cli`、`kiana-rpc` 入口，以及本文件与 `startup_ui` 的子进程测试。
pub fn install_fatal_startup_panic_hook() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if let Some(fatal) = info.payload().downcast_ref::<FatalStartupError>() {
                Output::process().write_line_stderr(&format!("Error: {}", fatal.message));
                std::process::exit(1);
            }
            previous(info);
        }));
    });
}

// ---------------------------------------------------------------------------
// setupCli
// ---------------------------------------------------------------------------

/// `setupCli()`：进程标题、环境标记、警告抑制、HTTP dispatcher。
///
/// TS 的四步顺序（标题 → 环境变量 → 警告抑制 → dispatcher）保持不变；Rust 额外安装启动期
/// 致命错误的 panic 钩子。实际执行顺序：
/// 1. `process.title = APP_NAME`（Linux 上尽力写 `/proc/self/comm`；其它平台 no-op）；
/// 2. 安装 panic 钩子（[`install_fatal_startup_panic_hook`]）——TS 没有对应物，提前到这里是
///    因为随后的迁移与启动 UI 都可能以 [`FatalStartupError`] 中止；
/// 3. `KIANA_CODING_AGENT=true`、`AI_AGENT=kiana`；
/// 4. `process.emitWarning = noop`（Rust 没有全局 warning 钩子，这里是 no-op）；
/// 5. `configureHttpDispatcher()`（无参默认 idle timeout）。
///
/// 调用契约：必须在创建任何线程/tokio 运行时之前调用一次（下面的 `set_var` 依赖单线程，
/// 见函数内的 SAFETY 注释）；`src/main.rs` 在构建运行时之前调用。`configure_http_dispatcher`
/// 的返回值被丢弃：这里传的是合法常量；即使失败，后续 `shared_http_client()` 仍会在槽位为空
/// 时按默认配置惰性构造 client。
pub fn setup_cli() {
    set_process_title(kiana_core::config::APP_NAME);

    // 必须在迁移与启动 UI 运行之前安装（它们会以 `FatalStartupError` 中止启动）。
    install_fatal_startup_panic_hook();

    // SAFETY: setup_cli 由二进制入口 `src/main.rs` 在创建 tokio 运行时与任何线程之前调用；
    // `src/bin/kiana-rpc.rs` 不调用 setup_cli（见 `setup_cli` 的文档），它在自己的入口
    // 同样在创建任何线程之前设置这两个变量。因此这里修改进程环境不存在数据竞争。
    // Rust 2024 把 `set_var` 标记为 unsafe 正是因为多线程环境下的竞争。
    unsafe {
        std::env::set_var("KIANA_CODING_AGENT", "true");
        std::env::set_var("AI_AGENT", "kiana");
    }

    // TS: `process.emitWarning = (() => {}) as typeof process.emitWarning;`
    // Rust 没有进程级 warning 钩子（tracing 由 subscriber 控制），这里保持 no-op。

    // 在 provider SDK 发请求前配置 HTTP dispatcher；全局/项目配置稍后由
    // SettingsManager 应用（app 会在设置加载后再次调用）。
    // 返回值忽略：常量合法；失败时槽位保持不变，首次 `shared_http_client()` 会惰性构造
    // 默认 client。
    let _ = configure_http_dispatcher(DEFAULT_HTTP_IDLE_TIMEOUT_MS);
}

/// `process.title = title` 的 Linux 近似：写 `/proc/self/comm`（15 字节上限，失败静默）。
///
/// 注意：`src/bin/kiana-rpc.rs` 有一份同样的本地实现（标题为 `kiana-rpc`），修改这里不会
/// 影响它。
#[cfg(target_os = "linux")]
fn set_process_title(title: &str) {
    let _ = std::fs::write("/proc/self/comm", title);
}

/// 非 Linux 平台没有可移植的进程标题设置；保持 no-op（与 TS 的可观察行为无关）。
#[cfg(not(target_os = "linux"))]
fn set_process_title(_title: &str) {}

#[cfg(test)]
mod tests {
    //! 测试策略：
    //!
    //! - 需要真实 fd 行为（同步写 stdout、panic 钩子 `exit(1)`）的用例用**子进程**运行：
    //!   父进程以环境变量区分角色，子进程 `std::process::exit` / 钩子退出后由父进程检查
    //!   stdout/stderr 与退出码。安装钩子后 panic 会直接结束进程，无法在同一进程内观察，
    //!   也会影响其它测试。
    //! - `node_fs_error_messages_match_node_libuv_text` 在 Linux 上用 `from_raw_os_error`
    //!   固定 errno → libuv 文案的映射（重点是 EPERM 与 EACCES 的区分）。

    use super::*;

    /// 子进程分支：同步写一行后立刻 `process::exit`；父进程断言捕获到了字节。
    ///
    /// 回归：`Output::process()` 曾经只是把文本排入 `output_guard` 的后台写线程（发后不管），
    /// 进程退出时输出会丢失。TS 的 `console.log` 由 Node 在正常退出时排空，因此 Rust 侧
    /// 必须同步写。
    #[test]
    fn writes_stdout_synchronously_before_process_exit() {
        const CHILD_ENV: &str = "KIANA_OUTPUT_SYNC_CHILD";
        if std::env::var_os(CHILD_ENV).is_none() {
            let executable = std::env::current_exe().expect("test executable");
            let output = std::process::Command::new(executable)
                .args([
                    "--exact",
                    "cli::setup::tests::writes_stdout_synchronously_before_process_exit",
                ])
                .env(CHILD_ENV, "1")
                .output()
                .expect("run child test");
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                stdout.contains("kiana-output-sync-marker"),
                "stdout was lost before exit: {stdout:?} / {:?}",
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        Output::process().write_line_stdout("kiana-output-sync-marker");
        std::process::exit(0);
    }

    /// 子进程分支：安装钩子后抛出 [`FatalStartupError`]；父进程断言 Node 风格的
    /// stderr 文案与退出码 1（而不是 Rust panic 的 101）。
    #[test]
    fn fatal_startup_error_exits_with_status_one() {
        const CHILD_ENV: &str = "KIANA_FATAL_STARTUP_CHILD";
        if std::env::var_os(CHILD_ENV).is_none() {
            let executable = std::env::current_exe().expect("test executable");
            let output = std::process::Command::new(executable)
                .args([
                    "--exact",
                    "cli::setup::tests::fatal_startup_error_exits_with_status_one",
                ])
                .env(CHILD_ENV, "1")
                .output()
                .expect("run child test");
            assert_eq!(output.status.code(), Some(1));
            assert_eq!(
                String::from_utf8_lossy(&output.stderr),
                "Error: EACCES: permission denied, open '/x'\n"
            );
            return;
        }
        install_fatal_startup_panic_hook();
        std::panic::panic_any(FatalStartupError {
            message: "EACCES: permission denied, open '/x'".to_string(),
        });
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn node_fs_error_messages_match_node_libuv_text() {
        // EPERM(1) 与 EACCES(13) 都是 ErrorKind::PermissionDenied，必须按 errno 区分。
        let eperm = std::io::Error::from_raw_os_error(1);
        assert_eq!(
            node_fs_error_message(
                &eperm,
                "rename",
                Some(std::path::Path::new("/a")),
                Some(std::path::Path::new("/b"))
            ),
            "EPERM: operation not permitted, rename '/a' -> '/b'"
        );
        let eacces = std::io::Error::from_raw_os_error(13);
        assert_eq!(
            node_fs_error_message(&eacces, "open", Some(std::path::Path::new("/a")), None),
            "EACCES: permission denied, open '/a'"
        );
        let exdev = std::io::Error::from_raw_os_error(18);
        assert_eq!(
            node_fs_error_message(&exdev, "rename", Some(std::path::Path::new("/a")), None),
            "EXDEV: cross-device link not permitted, rename '/a'"
        );
        let eisdir = std::io::Error::from_raw_os_error(21);
        assert_eq!(
            node_fs_error_message(&eisdir, "read", None, None),
            "EISDIR: illegal operation on a directory, read"
        );
    }
}
