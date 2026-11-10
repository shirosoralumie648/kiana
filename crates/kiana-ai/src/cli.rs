//! `cli.ts`（124 行，bin `kiana-ai`）：OAuth 登录开发 CLI。
//!
//! 行为逐字对照 `spec/ai-core.md` §2.1.13 与 §3.1：
//! - `AUTH_FILE = "auth.json"`（**当前工作目录**，不是 home）；写入
//!   `JSON.stringify(auth, null, 2)`（2 空格缩进、无尾随换行）；
//! - `PROVIDERS = builtinProviders().filter(p => p.auth.oauth !== undefined)`（9 个）；
//! - `answerPrompt`：`select` 打印 `\n<message>` 与 `  <n>. <label>`，提示
//!   `Enter number (1-<n>): `；其它提示为 `<message>[ (<placeholder>)]: `；
//! - `login`：`Unknown provider: <id>`；notify 的 4 个分支逐字输出；成功后打印
//!   `\nCredentials saved to auth.json`；
//! - `main`：无命令/`help`/`--help`/`-h` 打印用法与 provider 列表；`list` 逐行
//!   `<id padEnd(20)> <name>`；`login [provider]` 无参数时交互选择；其它命令
//!   `Unknown command: <cmd>`；
//! - 顶层错误：`Error: <message>`（stderr）+ 退出码 1。
//!
//! ## Rust 侧差异
//!
//! - TS 用 `node:readline` 的 `rl.question`（事件循环上的异步等待）。Rust 用
//!   `ReadlineInterface` + [`tokio::task::spawn_blocking`] 读 stdin，避免阻塞 OAuth
//!   流程（回调服务器、设备码轮询）依赖的 runtime worker；提示符仍与 TS 一样先写 stdout
//!   再等待输入。
//! - `Number.parseInt(text, 10)` 用 `parse_int_js` 复刻（前导空白、可选正负号、读到第一个
//!   非数字为止；无数字为 `None` 即 JS 的 `NaN`）。
//! - 文件系统错误（例如无写权限）用 `std::io::Error` 的文本；Node 的错误文本形如
//!   `ENOENT: …`，两者只在极端的 I/O 失败下可观察。
//!
//! `ReadlineInterface` 复刻 `createInterface` / `rl.question` / `rl.close` 的可观察语义
//! （全部来自 Node 26 实测）：
//! - 接口第一次提问时输入流进入 flowing：此后再到达的**整行**如果没有 pending
//!   question，就被 readline 直接丢弃（不会排队给下一个问题，也不会跨 `createInterface`
//!   保留在接口自己的缓冲里）；
//! - 输入流 EOF 时接口 close。close 之后再 `rl.question` 同步抛 `ERR_USE_AFTER_CLOSE`
//!   （`readline was closed`）→ 顶层打印 `Error: readline was closed` 并以 1 退出；
//! - 问题正在等待时 EOF：`rl.question` 的 promise 永不 resolve，没有其它 handle 时事件
//!   循环耗尽、进程以 0 退出。Rust 用 [`CliOutcome::Eof`] 表达同一可观察结果（提示照常
//!   打印，随后以 0 退出，不写 `auth.json`）。TS 里挂起的定时器/回调服务器在 EOF 下可能
//!   让进程继续存活（例如设备码轮询），这里不模拟那种无限挂起；
//! - 在**已经结束**的流上新建的接口收不到 `'close'` 事件，它的第一个问题同样永不
//!   resolve（而不是抛错）。
//!
//! # 在系统中的位置
//!
//! 生产入口只有 `src/bin/kiana-ai.rs`：该 `[[bin]]` 的 `main` 直接调用 [`main`]；参数
//! 解析、输出文本与退出码全部在本文件，bin 不添加任何逻辑。`tests/cli_login.rs` 以子进程
//! 黑盒验证本模块的输出文本、交互与退出码。
//!
//! 主要依赖这些 crate 内代码（非穷举）：`auth::types` 的 `AuthInteraction` / `AuthPrompt` /
//! `AuthPromptRequest` / `AuthEvent` / `LoginOptions` / `OAuthAuth` / `Credential` /
//! `OAuthCredential` / `AuthError`，`providers::all::builtin_providers`（provider 表），
//! `models::Provider`（公开签名里的 `Arc<dyn Provider>`），`utils::abort::AbortScope`
//! （登录流程的取消信号），以及 `error::AiError`（`From<AiError> for CliError` 的投影）。
//! `auth.json` 的序列化用 `kiana_json`（`js_object::stringify_pretty` 复刻
//! `JSON.stringify` 的缩进与键序，`js_num::format_js_number` 复刻 JS 数字文本）。
//!
//! # 执行流程
//!
//! 1. [`main`] 建 tokio 运行时并阻塞执行 [`run`]（参数已去掉程序名）。
//! 2. [`run`] 按第一个参数分派：无参数 / `help` / `--help` / `-h` 打印用法；`list`
//!    列出 OAuth provider；`login [provider]` 缺参数时先交互选择，再校验 provider 存在。
//! 3. [`login`] 从 [`oauth_providers`] 找到 provider、取出其 `OAuthAuth`，创建
//!    `CliInteraction`（readline + 永不主动 abort 的 `AbortScope`），调用
//!    `OAuthAuth::login`。登录流程通过 `prompt` 读 stdin、通过 `notify` 写 stdout。
//! 4. 登录成功后 `credential_entry` 把凭证序列化成条目，[`load_auth`] 读入现有文件、
//!    插入或覆盖该 provider 的键，[`save_auth`] 整体重写，最后打印
//!    `\nCredentials saved to auth.json`。
//!
//! # readline 复刻的状态机
//!
//! `ReadlineInterface` 的可观察状态由共享状态里的两个原子量（`started`/`closed`）与
//! 创建时快照的全局 EOF 标志（`eof_at_creation`）共同决定：
//!
//! | 状态 | 条件 | 一次 `question` 的结果 |
//! |---|---|---|
//! | 未提问 | `!started && !closed` | 打印提示符，开始读一行 |
//! | 已提问 | `started && !closed` | 先排空并丢弃多余整行；若排空时见到 EOF 则转为已关闭，否则打印提示符并读一行 |
//! | 已关闭 | `closed` | 不打印提示符，返回 `readline was closed` |
//! | 创建时已 EOF（优先级最低，阶段 1/2 之后才检查） | `eof_at_creation && !closed`（且未在阶段 2 的排空中发现 EOF） | 打印提示符后返回 `Eof`（问题永不 resolve） |
//!
//! 转换关系：
//!
//! - 未提问 → 已提问：第一次 `question`；
//! - 已提问 → 已关闭：显式 `close()`、问题等待期间读到 EOF、或拿到回答后在同一次排空
//!   中发现 EOF。三种情形都会让下一次 `question` 报告 `readline was closed`；
//!   `tests/cli_login.rs` 的 `login_provider_prompt_after_stdin_eof_reports_readline_was_closed`
//!   只锁定这个可观察结果（最终报错、第二个提示符不打印），无法区分 `closed` 是在回答后的
//!   排空里置位，还是在下次提问的阶段 2 里置位——在测试的管道输入上后者同样返回 `Closed`；
//! - 创建时已 EOF 是创建时的属性（全局 `STDIN_EOF` 只置位、不清除），每次 `question` 都会
//!   在打印提示符后检查它，而不是只在第一次生效：命中时返回 `Eof` 而不是转成已关闭。但在
//!   管道/文件这类 EOF 持续可读的输入上，第二次提问会先被阶段 2 的排空发现 EOF 并转成已
//!   关闭，因而通常观察不到第二次 `Eof`；TTY 上 Ctrl-D 不是持久的 EOF（阶段 2 的 poll
//!   通常不再报告可读），于是仍会走到 `eof_at_creation` 再次返回 `Eof`。真实调用方在第一
//!   次 `Eof` 后就结束流程，走不到第二次；
//! - 已关闭是终态，`closed` 不会清回 false。
//!
//! 问题等待中的 EOF 不让问题抛错：它返回 `Eof`，由 `answer_prompt` 记录到
//! `CliInteraction::eof`，[`login`] 再改写为 [`CliOutcome::Eof`]，进程以 0 退出且不写
//! `auth.json`。这是本模块最容易误读的语义。
//!
//! # 边界与不变量
//!
//! - [`AUTH_FILE`] 是相对路径，读写都相对**进程当前工作目录**，不是 home 或
//!   `KIANA_CODING_AGENT_DIR`；本 CLI 也不使用 `CredentialStore`、不做 token 刷新
//!   （spec/ai-auth.md §A.8），只做一次性登录。
//! - `auth.json` 无文件锁、无 chmod、无 BOM 处理，直接覆盖写；登录完成之后才读文件，
//!   因此并发登录的写入可能互相覆盖（TS 行为相同）。
//! - 输出文本是契约（spec/ai-core.md §3.1 逐字规定）：改动提示符、用法或 `Error: …`
//!   的字符串会破坏 `tests/cli_login.rs` 的断言。
//! - stdin 是进程级资源，本模块不做 TTY 检测、没有行编辑；阻塞读放在 `spawn_blocking`
//!   上，避免占住 tokio worker。同一时刻只应有一个接口在读（本模块的调用顺序保证）。
//! - `notify` 是同步打印，签名没有错误通道；stdout 写入失败（例如管道提前关闭）时
//!   `println!` 会 panic，登录流程无法把它转成 [`CliError`]。用法/列表/提示符的打印
//!   同理。
//! - 单次提示的取消信号（`AuthPromptRequest::signal`）被忽略；整个流程的取消只由
//!   `CliInteraction::signal` 表达，而 CLI 从不主动 abort。
//!
//! # 阅读顺序
//!
//! [`AUTH_FILE`] 与读写函数 → [`run`]（命令分派）→ [`login`] / `login_with_oauth`
//! （写文件流程）→ `ReadlineInterface`（`new`/`ask`/`drain_available`）→
//! `credential_entry`（键序）→ [`main`]（退出码）。

use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{Map, Value};

use crate::auth::types::{
    AuthError, AuthEvent, AuthInteraction, AuthPrompt, AuthPromptRequest, Credential, LoginOptions,
    OAuthCredential,
};
use crate::models::Provider;
use crate::providers::all::builtin_providers;
use crate::utils::abort::AbortScope;

/// `AUTH_FILE = "auth.json"`（当前工作目录）。
///
/// 它是**相对路径**：`std::fs` 在打开文件时按进程当前工作目录解析，所以同一进程内
/// `chdir` 会改变 [`load_auth`] / [`save_auth`] 实际读写的文件。CLI 进程自己不 `chdir`；
/// 集成测试为每个用例建独立临时目录，并以该目录作为子进程的 `current_dir` 启动
/// （见 `tests/cli_login.rs`）。
///
/// 路径按 spec 固定为当前工作目录下的 `auth.json`（不是 home 下的配置目录），因此这个
/// 开发用 CLI 只影响“从哪个目录启动它”。
pub const AUTH_FILE: &str = "auth.json";

/// CLI 错误（TS 的普通 `Error`）。
///
/// 只保留 `message`：顶层 [`main`] 打印 `Error: <message>`，所以 [`AuthError`] 的
/// `name`/`code`/`details`、[`crate::error::AiError`] 的分类信息与 `std::io::Error` 的
/// `kind` 在这里都被丢弃、不再可观察。这是刻意的投影——TS 的顶层 `catch` 也只读
/// `error.message`（spec/ai-core.md §3.1）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliError {
    /// `error.message`。
    pub message: String,
}

impl CliError {
    /// `new Error(message)`。
    pub fn new(message: impl Into<String>) -> Self {
        CliError {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CliError {}

// 三个 `From` 实现都只搬运 message：CLI 的顶层输出只消费这一字段（见 `CliError` 的
// 文档），`AuthError` 的 name/code/details 与 `AiError` 的分类信息在这里被有意丢弃。
impl From<AuthError> for CliError {
    fn from(error: AuthError) -> Self {
        CliError::new(error.message)
    }
}

impl From<std::io::Error> for CliError {
    fn from(error: std::io::Error) -> Self {
        CliError::new(error.to_string())
    }
}

impl From<crate::error::AiError> for CliError {
    fn from(error: crate::error::AiError) -> Self {
        CliError::new(error.message)
    }
}

/// CLI 的结束方式。
///
/// TS 的 `main()` 只有两种可观察结局：正常返回（退出码 0）或抛错（`Error: …` + 退出码 1）。
/// [`CliOutcome::Eof`] 覆盖第三种情况：stdin 在提示处 EOF，`rl.question` 永不 resolve，
/// 事件循环耗尽后进程以 0 退出（不打印错误、不写 `auth.json`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CliOutcome {
    /// 命令执行完毕（`help`/`list`/登录成功）。
    Done,
    /// stdin 在提示处 EOF：什么都不做，进程以 0 退出。
    Eof,
}

/// `PROVIDERS = builtinProviders().filter(p => p.auth.oauth !== undefined)`。
///
/// 顺序即 `builtinProviders()` 的顺序（42 个内置 provider 的固定顺序）。
///
/// 每次调用都会**重新构造**全部 42 个 provider 再过滤，返回其中 `auth().oauth` 非空的
/// 9 个（anthropic、github-copilot、kimi-coding、meta、openai、openai-codex、openrouter、
/// radius、xai）。顺序是可见契约：`run` 的编号菜单与 `list` 的输出都依赖它，本文件的
/// 单元测试 `oauth_provider_list_matches_the_nine_builtin_oauth_providers` 锁定这 9 个
/// id 的顺序。
///
/// 返回 `Arc<dyn Provider>` 而不是引用，是因为 provider 表是临时构造的，调用方需要持有
/// 它们；`Provider` 的 `id()`/`name()`/`auth()` 都只读。
///
/// TS: `cli.ts#PROVIDERS`
pub fn oauth_providers() -> Vec<Arc<dyn Provider>> {
    builtin_providers()
        .into_iter()
        .filter(|provider| provider.auth().oauth.is_some())
        .collect()
}

/// `padEnd(20)`（JS 按 UTF-16 码元计数；内置 id/name 均为 ASCII）。
///
/// 长度不足 20 时补空格，达到或超过 20 时原样返回（`padEnd` 不截断）。
fn pad_end_20(text: &str) -> String {
    let length = text.encode_utf16().count();
    if length >= 20 {
        text.to_string()
    } else {
        format!("{text}{}", " ".repeat(20 - length))
    }
}

/// `loadAuth()` 对指定路径的实现。
///
/// TS: `JSON.parse(readFileSync(AUTH_FILE, "utf-8"))`，**不做任何形状校验**；只有
/// `existsSync` 失败或 `JSON.parse` 抛错时回落到 `{}`。因此文件里已有的 `api_key`
/// 条目、`oauth` 条目里的未知字段都会原样保留。顶层值可能是数组或标量（`JSON.parse`
/// 的合法结果），由 [`login`] 复刻 TS 的赋值语义。
///
/// 返回 `serde_json::Value` 而不是 `Map` 正是为了保留这种“任意顶层值”：本函数永不报错，
/// 文件不存在、无读权限、非 UTF-8、JSON 语法错误统统得到空对象。调用方必须自己判断
/// 顶层形状。读取失败与“空文件”在这里不可区分。
///
/// TS: `cli.ts#loadAuth`
pub fn load_auth_from(path: &Path) -> Value {
    let Ok(content) = std::fs::read_to_string(path) else {
        return Value::Object(Map::new());
    };
    serde_json::from_str(&content).unwrap_or_else(|_| Value::Object(Map::new()))
}

/// `loadAuth()`：读取当前工作目录的 [`AUTH_FILE`]。
///
/// TS: `cli.ts#loadAuth`
pub fn load_auth() -> Value {
    load_auth_from(Path::new(AUTH_FILE))
}

/// `saveAuth(auth)` 对指定路径的实现：`JSON.stringify(auth, null, 2)`（2 空格缩进、
/// 无尾随换行、JS 键序）。
///
/// 语义与 `fs.writeFileSync` 对齐：**整体截断重写**，不创建父目录、不排他加锁、不
/// 保留/设置权限位（已有文件的权限不变，新文件按 umask 创建），也不做原子替换——写到
/// 一半崩溃会留下截断的 JSON。失败（无权限、目录不存在、磁盘满……）转成
/// [`CliError`]，文案来自 `std::io::Error::to_string()`。
///
/// TS: `cli.ts#saveAuth`
pub fn save_auth_to(path: &Path, auth: &Value) -> Result<(), CliError> {
    std::fs::write(path, kiana_json::js_object::stringify_pretty(auth, 2))?;
    Ok(())
}

/// `saveAuth(auth)`：写入当前工作目录的 [`AUTH_FILE`]。
///
/// TS: `cli.ts#saveAuth`
pub fn save_auth(auth: &Value) -> Result<(), CliError> {
    save_auth_to(Path::new(AUTH_FILE), auth)
}

/// `Number.parseInt(text, 10)`：`None` 表示 `NaN`。
///
/// JS 语义：跳过前导空白，可选的 `+`/`-`，然后取尽可能长的十进制数字序列；没有数字即
/// `NaN`（后续的 `- 1` 仍是 `NaN`）。超出 `i64` 的范围在实际输入里不可能出现。
///
/// 两个调用点（`answer_prompt` 的 `select` 与 `run` 的交互选择）都用 `- 1` 把 1-based
/// 编号转成 0-based 下标；`None` 经 `map` 自然传播，最终落到 `Invalid selection` 或
/// `Unknown provider: `。
fn parse_int_js(text: &str) -> Option<i64> {
    let trimmed = text.trim_start();
    let bytes = trimmed.as_bytes();
    let (sign, rest) = match bytes.first() {
        Some(b'-') => (-1i64, &trimmed[1..]),
        Some(b'+') => (1i64, &trimmed[1..]),
        _ => (1i64, trimmed),
    };
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse::<i64>().ok().map(|value| sign * value)
}

/// stdin 的全局 EOF 状态：Node 的输入流只会发一次 `'end'`；在它之后新建的 readline
/// 接口收不到 `'close'`，其问题永不 resolve。
///
/// 只置位、不清除。注意只有“问题等待期间读到 EOF”这条路径会置位（`ask` 的读取循环）；
/// 其余路径（`drain_available` 排空时发现 EOF）只关闭当前接口，不更新这个全局量。之后
/// 新建的接口在**管道/文件**这类已经结束的输入上，会在自己的第一次读取中立刻看到 EOF，
/// 可观察结果相同（提示照常打印、问题挂起）；而在 TTY 上 Ctrl-D 只让当前 `read` 返回 0，
/// 之后的 `read` 会重新阻塞，新接口的第一次提问会正常等待用户输入——TS 里已结束的流上
/// 新建接口的 question 则永不 resolve，这一点两者并不相同。集成测试为每个用例启动独立
/// 子进程，因为全局量无法重置。
static STDIN_EOF: AtomicBool = AtomicBool::new(false);

/// 一次性从 stdin 读取的字节数。
///
/// 必须 >= `std::io::stdin()` 内部 `BufReader` 的容量（8192），这样 [`Read::read`] 会绕开
/// 标准库的行缓冲直接读 fd 0 —— [`poll_stdin`] 看到的可读状态才与 readline 的实际输入
/// 一致（否则数据可能滞留在标准库的缓冲区里，poll 报告“不可读”）。
const STDIN_READ_BUFFER: usize = 8192;

/// 读取/排空一次 stdin 的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReadOutcome {
    /// 读到了字节，或当前没有立即可读的数据。
    Data,
    /// stdin 已 EOF（或读取失败；Node 的 readline 在 error 后同样 close）。
    Eof,
}

/// [`poll_stdin`] 的结果。
enum PollStatus {
    /// 有数据或挂断：随后的 read 会给出结果。
    Ready,
    /// 当前没有立即可读的数据。
    Timeout,
}

/// 单次 readline 接口的共享状态（`Arc` 跨 `spawn_blocking` 线程）。
///
/// 锁协议：`pending` 是唯一需要互斥的数据；`closed`/`started` 用原子量，这样
/// `close()` 与“判断是否已关闭”都不必去等一个可能仍阻塞在 stdin 上的读取线程。
/// `pending` 的临界区只做内存扫描/追加，不在持锁期间做 IO。
struct ReadlineShared {
    /// `rl.close()`，或本接口在打开期间观察到 EOF。用原子量：关闭不应等待可能仍阻塞在
    /// stdin 上的读取线程。
    closed: AtomicBool,
    /// 是否已经问过至少一个问题（Node 的 `question` 会 `resume()` 输入流；此后到达且
    /// 没有 pending question 的行会被丢弃）。
    started: AtomicBool,
    /// readline 自己的行缓冲（Node 的 `kLine_buffer`：按接口隔离、`close()` 时丢弃）。
    ///
    /// 中毒（持锁线程 panic）时读取路径统一用 `unwrap_or_else(|error| error.into_inner())`
    /// 取回内部数据继续，而不是再次 panic：这里存的只是字节串，部分更新不会破坏内存
    /// 安全；换成 `unwrap` 会让一次 panic 把之后所有提问都变成崩溃。`close()` 是例外：
    /// 它用 `try_lock()`，中毒时跳过清空，同样不会 panic。
    pending: Mutex<Vec<u8>>,
}

/// `rl.question` 的可观察结果。
#[derive(Debug, Clone, PartialEq, Eq)]
enum QuestionOutcome {
    /// 读到一行回答（已去掉行终止符）。
    Line(String),
    /// 等待回答时 stdin EOF：TS 的 promise 永不 resolve（没有其它 handle 时进程以 0 退出）。
    Eof,
    /// 接口已 close：TS 的 `rl.question` 抛 `ERR_USE_AFTER_CLOSE`（`readline was closed`）。
    Closed,
}

/// `node:readline` 的 `createInterface({ input: process.stdin })` + `rl.question` /
/// `rl.close` 的最小复刻。语义见模块文档。
///
/// `Clone` 只复制 `Arc`，所以 `question` 能把接口移进阻塞线程而调用方仍保留句柄；同一
/// 接口的所有方法都取 `&self`。它不拥有 stdin（进程级资源），也不做终端行编辑。
#[derive(Clone)]
struct ReadlineInterface {
    shared: Arc<ReadlineShared>,
    /// 创建时 stdin 是否已经 EOF（新接口挂在已结束的流上，收不到 `'close'`）。
    eof_at_creation: bool,
}

impl ReadlineInterface {
    /// `createInterface({ input: process.stdin, output: process.stdout })`。
    fn new() -> Self {
        ReadlineInterface {
            shared: Arc::new(ReadlineShared {
                closed: AtomicBool::new(false),
                started: AtomicBool::new(false),
                pending: Mutex::new(Vec::new()),
            }),
            eof_at_creation: STDIN_EOF.load(Ordering::SeqCst),
        }
    }

    /// `rl.close()`。
    fn close(&self) {
        self.shared.closed.store(true, Ordering::SeqCst);
        // Node 的 readline 在 close 时丢弃接口自己缓冲的未完成行。
        if let Ok(mut pending) = self.shared.pending.try_lock() {
            pending.clear();
        }
    }

    /// `rl.question(question, callback)`：先打印提示符，再等待一行输入。
    ///
    /// `ask` 会在 `read` 上阻塞（stdin 无数据时可能一直阻塞），因此放进
    /// `spawn_blocking`，不占 tokio worker。`JoinError` 表示阻塞任务 panic 或运行时已
    /// 关闭，转成 [`AuthError`] 交给登录流程按普通错误传播。
    ///
    /// 注意：丢弃返回的 future **不会**终止已经开始的阻塞读取线程——Tokio 无法取消正在
    /// 运行的 blocking 任务，线程会继续持有 stdin 直到读到数据、EOF 或进程退出。CLI 不会
    /// abort `interaction.signal()`，但浏览器回调胜出时 provider 流程会放弃仍在等待的
    /// prompt future（`openai_chatgpt.rs` 的阶段 5 `tokio::select!`；
    /// `callback_server.rs::wait_for_callback_or_manual_input`，anthropic / openai-codex /
    /// openrouter 都走后者），此时读取线程就会继续阻塞在 stdin 上。
    async fn question(&self, question: String) -> Result<QuestionOutcome, AuthError> {
        let interface = self.clone();
        tokio::task::spawn_blocking(move || interface.ask(&question))
            .await
            .map_err(|error| AuthError::new(error.to_string()))
    }

    // 复刻 Node readline 处理一次 question 的事件顺序。阶段划分对应可观察差异：
    // 1) 已 close 时不打印提示符直接失败；2) 上一次提问已让流进入 flowing，先丢弃缓冲里
    // 无人等待的整行并观察 EOF；3) 打印提示符后才开始读；4) 等待中的 EOF 让问题挂起而
    // 不是抛错。多数阶段在 tests/cli_login.rs 有回归用例。
    fn ask(&self, question: &str) -> QuestionOutcome {
        // 阶段 1：已关闭的接口连提示符都不打印（TS 的同步 throw）。
        if self.shared.closed.load(Ordering::SeqCst) {
            // Node 在 close 后同步抛 ERR_USE_AFTER_CLOSE（连提示符都不打印）。
            return QuestionOutcome::Closed;
        }
        // 阶段 2：第一次提问会让输入流进入 flowing；此后没有 pending question 时到达的
        // 整行会被丢弃。这里先把它们排掉，同时观察 EOF——若已 EOF，接口随即 close，
        // 本次提问按“已关闭”失败。
        //
        // flowing 之后、没有 pending question 时到达的行被 readline 丢弃；EOF 也会在这时
        // 被观察为接口 close（下一次 question 抛错）。
        if self.shared.started.load(Ordering::SeqCst) && self.drain_available() == ReadOutcome::Eof
        {
            self.shared.closed.store(true, Ordering::SeqCst);
            return QuestionOutcome::Closed;
        }
        // 阶段 3：先打印提示符再读。`print!` 不带换行，必须手动 flush；否则输出被重定向
        // 到管道时提示符会滞留在标准库缓冲里，程序没显示提示就开始等输入。
        print!("{question}");
        let _ = std::io::stdout().flush();
        // 从这一刻起流被视为 flowing（Node 的 `rl.question` 内部 `resume()`）。
        self.shared.started.store(true, Ordering::SeqCst);
        if self.eof_at_creation {
            // 流在接口创建之前就结束了：新接口收不到 'close'，问题永不 resolve。
            return QuestionOutcome::Eof;
        }
        // 阶段 4：循环到缓冲里出现完整一行或 stdin EOF。`take_line` 只消费接口自己的
        // 缓冲，`read_chunk` 才真正读 stdin（可能阻塞）。
        loop {
            if let Some(line) = self.take_line() {
                // Node 的 ondata 在一个 chunk 内同步处理所有行：回答之后、同一 chunk 里
                // 剩下的行没有 pending question，被丢弃；EOF 也在这里确定。
                //
                // 这一步对应 Node 在“回答所在 chunk 之后紧跟 EOF”时的接口 close：这里与
                // Node 一样在观察到 EOF 后立即 close。在管道/文件这类 EOF 持续可读的输入
                // 上，即使不在此处置位，下一次提问的阶段 2 也会在同一个 EOF 上排空到
                // `Eof` 并返回 `readline was closed`（TTY 上 Ctrl-D 只对当前 read 有效，
                // 下一次提问可能重新阻塞），所以 tests/cli_login.rs 的
                // login_provider_prompt_after_stdin_eof_reports_readline_was_closed 只能
                // 锁定“下一次提问报错”这个可观察结果，无法区分 `closed` 在哪一步置位。
                if self.drain_available() == ReadOutcome::Eof {
                    self.shared.closed.store(true, Ordering::SeqCst);
                }
                return QuestionOutcome::Line(line);
            }
            match self.read_chunk() {
                ReadOutcome::Data => {}
                ReadOutcome::Eof => {
                    // 问题正在等待时 EOF：接口 close，但 promise 永不 resolve。
                    self.shared.closed.store(true, Ordering::SeqCst);
                    // 置位进程级 STDIN_EOF，让之后新建的接口进入“创建时已 EOF”状态。
                    STDIN_EOF.store(true, Ordering::SeqCst);
                    return QuestionOutcome::Eof;
                }
            }
        }
    }

    /// 从接口缓冲取出一整行（`\r?\n` 终止；与 Node 的非 terminal readline 一致）。
    ///
    /// 返回的字符串已去掉 `\n` 与紧邻的 `\r`。非 UTF-8 字节经 `from_utf8_lossy` 替换为
    /// U+FFFD；stdin 正常是 UTF-8 文本，这一步只在异常输入下可观察。
    fn take_line(&self) -> Option<String> {
        let mut pending = self
            .shared
            .pending
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let position = pending.iter().position(|byte| *byte == b'\n')?;
        let mut line: Vec<u8> = pending.drain(..=position).collect();
        line.pop();
        let line = String::from_utf8_lossy(&line).into_owned();
        Some(match line.strip_suffix('\r') {
            Some(line) => line.to_string(),
            None => line,
        })
    }

    /// 丢弃缓冲里所有完整的行（保留未完成的行，即 Node 的 `kLine_buffer`）。
    ///
    /// 末尾没有 `\n` 的尾部字节必须保留：它可能是下一行的一部分，丢掉会让下一次读取从
    /// 一个残缺的行开始。
    fn drop_complete_lines(&self) {
        let mut pending = self
            .shared
            .pending
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(last) = pending.iter().rposition(|byte| *byte == b'\n') {
            pending.drain(..=last);
        }
    }

    /// 读一批 stdin 字节到接口缓冲。
    ///
    /// 用 8192 字节的栈缓冲直接读 stdin（见 `STDIN_READ_BUFFER`）。`Ok(0)`（EOF）与任何
    /// `Err` 都归为 `Eof`：Node 的 readline 在流 error 后同样 close，二者都会让后续提问
    /// 失败或挂起。若真有读取错误，错误文本不会出现在任何输出里——这正是 TS 的行为。
    fn read_chunk(&self) -> ReadOutcome {
        let mut buffer = [0u8; STDIN_READ_BUFFER];
        match std::io::stdin().lock().read(&mut buffer) {
            Ok(0) | Err(_) => ReadOutcome::Eof,
            Ok(count) => {
                self.shared
                    .pending
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .extend_from_slice(&buffer[..count]);
                ReadOutcome::Data
            }
        }
    }

    /// 排空当前**立即可读**的输入并丢弃其中的整行；EOF 时返回 [`ReadOutcome::Eof`]。
    ///
    /// 算法：先丢掉接口缓冲里已有的完整行；再循环用 `poll_stdin`（timeout=0）检查内核
    /// 里是否还有数据，有就读一块并再丢整行，直到 poll 报告不可读。返回值 `Data` 同时
    /// 表示“读到了数据”和“当前没有更多数据”，调用方只关心是否 EOF。
    ///
    /// 它不做阻塞等待（poll 立即返回），所以不会卡住 runtime；也正因为如此，它看不到
    /// “马上会到达”的数据，那部分留给下一次 `read_chunk`。
    fn drain_available(&self) -> ReadOutcome {
        self.drop_complete_lines();
        loop {
            if let PollStatus::Timeout = poll_stdin() {
                return ReadOutcome::Data;
            }
            match self.read_chunk() {
                ReadOutcome::Data => self.drop_complete_lines(),
                ReadOutcome::Eof => return ReadOutcome::Eof,
            }
        }
    }
}

/// `poll(fd 0, timeout=0)`：当前是否有立即可读的数据（含挂断）。
///
/// `timeout = 0` 表示立即返回，只用于“现在有没有数据”的判断，不承担等待。poll 自身
/// 失败（例如被信号中断）与超时都返回 [`PollStatus::Timeout`]，让随后的 `read_chunk`
/// 给出真实结果，避免在 poll 层重复实现错误分类。
fn poll_stdin() -> PollStatus {
    let mut descriptor = libc::pollfd {
        fd: libc::STDIN_FILENO,
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: `poll` 只读写这一个 `pollfd` 结构；fd 0 是标准输入，本模块不会关闭它。
    // 即使它已无效，poll 也只在 `revents` 里报告 POLLNVAL，不会解引用任何用户指针。
    let ready = unsafe { libc::poll(&mut descriptor, 1, 0) };
    if ready > 0 {
        // > 0 表示至少一个 fd 有事件：POLLIN（数据/EOF）、POLLHUP（挂断）、POLLERR
        // （错误）或 POLLNVAL（fd 无效）。四种情况都让随后的 `read` 给出确定结果
        // （数据 / 0 / 错误）。
        PollStatus::Ready
    } else {
        // 0 = 超时；< 0 = poll 自身失败（EINTR、EINVAL 等）。都按“当前不可读”处理，
        // 由随后的 read 给出真实的 EOF/错误。
        PollStatus::Timeout
    }
}

/// `rl.question` 在接口 close 后抛出的 `ERR_USE_AFTER_CLOSE` 的 `message`（Node 26）。
fn closed_error() -> AuthError {
    AuthError::new("readline was closed")
}

/// `answerPrompt(rl, authPrompt)`。
///
/// EOF（问题等待中）时置位 [`CliInteraction`] 的 EOF 标记并返回错误；[`login`] 据此以
/// [`CliOutcome::Eof`] 结束（TS 的 prompt 永不 resolve）。接口已 close 时返回
/// `readline was closed`（TS 的同步 throw），由顶层打印 `Error: …` 并以 1 退出。
///
/// 返回用户输入的原始行；`select` 返回选中项的 **id**（不是展示用的 label）。
/// `text` / `secret` / `manual_code` 三种提示的读取路径完全相同：`secret` 不做任何
/// 掩码（终端自己回显），`placeholder` 为空串时按“无占位符”处理（JS 里 `""` 为假，
/// 与 TS 的三元表达式一致）。
///
/// TS: `cli.ts#answerPrompt`
async fn answer_prompt(
    auth_prompt: &AuthPrompt,
    interaction: &CliInteraction,
) -> Result<String, AuthError> {
    match auth_prompt {
        AuthPrompt::Select { message, options } => {
            // 编号从 1 开始；下面把用户输入减 1 后当作 0-based 下标。
            println!("\n{message}");
            for (index, option) in options.iter().enumerate() {
                println!("  {}. {}", index + 1, option.label);
            }
            let question = format!("Enter number (1-{}): ", options.len());
            let line = match interaction.readline.question(question).await? {
                QuestionOutcome::Line(line) => line,
                QuestionOutcome::Eof => return Err(interaction.eof_error()),
                QuestionOutcome::Closed => return Err(closed_error()),
            };
            // `parse_int_js` 的 `None`（非数字）与越界/负数下标都落到 `Invalid selection`；
            // `usize::try_from` 同时挡掉 `-1` 之类的负值（TS 里 `options[-2]` 为 undefined）。
            let choice = parse_int_js(&line).map(|value| value - 1);
            let selected = choice.and_then(|index| {
                usize::try_from(index)
                    .ok()
                    .and_then(|index| options.get(index))
            });
            match selected {
                Some(selected) => Ok(selected.id.clone()),
                None => Err(AuthError::new("Invalid selection")),
            }
        }
        AuthPrompt::Text {
            message,
            placeholder,
        }
        | AuthPrompt::Secret {
            message,
            placeholder,
        }
        | AuthPrompt::ManualCode {
            message,
            placeholder,
        } => {
            // 三种文本提示共用一个读取路径；占位符为空串时整段 ` (…)` 不出现，
            // 提示符固定以 `: ` 结尾。
            let suffix = placeholder
                .as_deref()
                .filter(|placeholder| !placeholder.is_empty())
                .map(|placeholder| format!(" ({placeholder})"))
                .unwrap_or_default();
            match interaction
                .readline
                .question(format!("{message}{suffix}: "))
                .await?
            {
                QuestionOutcome::Line(line) => Ok(line),
                QuestionOutcome::Eof => Err(interaction.eof_error()),
                QuestionOutcome::Closed => Err(closed_error()),
            }
        }
    }
}

/// 登录期间的回调（`{ signal, prompt, notify }`）。
///
/// 它是 `AuthInteraction` 的 CLI 实现：`prompt` 走 readline，`notify` 直接打印，
/// `signal` 是一个永不主动 abort 的 [`AbortScope`]。每个 [`login`] 创建一个，结束时由
/// 调用方统一 `close()`。
struct CliInteraction {
    signal: AbortScope,
    /// `login()` 里 `createInterface` 得到的 readline 接口（`finally { rl.close() }`）。
    readline: ReadlineInterface,
    /// stdin 是否在某个提示处到达 EOF。
    eof: AtomicBool,
}

impl CliInteraction {
    fn new() -> Self {
        // TS: `new AbortController().signal`（不主动中止）。
        CliInteraction {
            signal: AbortScope::new(),
            readline: ReadlineInterface::new(),
            eof: AtomicBool::new(false),
        }
    }

    /// `rl.close()`（TS 的 `finally`）。
    fn close(&self) {
        self.readline.close();
    }

    /// 记录 EOF。返回的错误只是让挂起的登录流程退出；[`login`] 会把它改写为
    /// [`CliOutcome::Eof`]（TS 的 `rl.question` 永不 resolve，不会抛错）。
    fn eof_error(&self) -> AuthError {
        self.eof.store(true, Ordering::SeqCst);
        AuthError::new("Login cancelled")
    }

    /// 是否观察到 EOF。
    fn eof(&self) -> bool {
        self.eof.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl AuthInteraction for CliInteraction {
    fn signal(&self) -> &AbortScope {
        &self.signal
    }

    async fn prompt(&self, request: AuthPromptRequest) -> Result<String, AuthError> {
        // CLI 只使用提示的显示字段；`request.signal`（单次提示的取消）被忽略——
        // spec §2.1.13 的 `answerPrompt` 同样只接收 rl 与提示内容。整个流程的取消由
        // interaction 的 signal 表达。
        answer_prompt(&request.prompt, self).await
    }

    fn notify(&self, event: &AuthEvent) {
        // 输出文本是契约的一部分（spec/ai-core.md §3.1）：auth_url 与 device_code 都先打印
        // 一个空行 + `Open this URL in your browser:`；instructions 为空串时不打印；
        // device_code 的 openBrowserOnConfirm/copyCodeOnConfirm/interval/expires 在本 CLI
        // 中不使用：本 crate 没有打开浏览器的代码（无 xdg-open / webbrowser 之类调用），各
        // OAuth 实现也都传 `open_browser_on_confirm: None`（spec/ai-auth.md §3.4：本单元
        // 从不设置它）；provider 只负责设备码轮询，用户需要自行打开 URL。
        match event {
            AuthEvent::AuthUrl { url, instructions } => {
                println!("\nOpen this URL in your browser:\n{url}");
                if let Some(instructions) = instructions
                    && !instructions.is_empty()
                {
                    println!("{instructions}");
                }
            }
            AuthEvent::DeviceCode {
                user_code,
                verification_uri,
                ..
            } => {
                println!("\nOpen this URL in your browser:\n{verification_uri}");
                println!("Enter code: {user_code}");
            }
            AuthEvent::Info { message, .. } | AuthEvent::Progress { message } => {
                println!("{message}");
            }
        }
    }
}

/// 登录得到的凭证在 `auth.json` 中的条目。
///
/// 形状即 [`Credential::OAuth`] 的 JSON（`type` + `access`/`refresh`/`expires` + 未知字段，
/// 其中 `expires` 按 JS number 写出），但键序复刻各 OAuth 实现返回的对象字面量：
/// anthropic / meta / github-copilot 以 `refresh` 开头，其余 6 个以 `access` 开头，
/// `type` 始终在最前，未知字段（`enterpriseUrl`、`availableModelIds`、`clientId`、
/// `scopes`、`accountId`、`scope`…）保持 `OAuthCredential::extra` 的顺序。
///
/// TS 的 `JSON.stringify` 保留对象字面量的插入顺序（spec/ai-auth.md §7.15 第 4 条要求
/// `auth.json` 字节一致）；Rust 的 [`OAuthCredential`] 是结构体、字段顺序固定，所以在
/// 写出前按 provider 重排。
///
/// 实现步骤：先用 `Credential::OAuth` 的 serde 表示拿到
/// `type`+`access`+`refresh`+`expires`+未知字段；再用 `shift_remove` 取出四个已知键
/// （`shift_remove` 保持剩余键的原有顺序）；接着按 provider 决定两个 token 的先后；
/// 最后把剩余未知字段按原序追加到 `expires` 之后。provider id 只要不在三个特例里，
/// 一律走 `access` 在前的分支。
///
/// TS: `cli.ts#login`（`auth[providerId] = credential`）+ `auth/oauth/*.ts` 的返回字面量
fn credential_entry(provider_id: &str, credential: &OAuthCredential) -> Result<Value, CliError> {
    let value = serde_json::to_value(Credential::OAuth(credential.clone()))
        .map_err(|error| CliError::new(error.to_string()))?;
    let Value::Object(mut fields) = value else {
        // `Credential` 的序列化一定是对象；保持原样以防未来形状变化。
        return Ok(value);
    };
    let type_value = fields.shift_remove("type");
    let access = fields.shift_remove("access");
    let refresh = fields.shift_remove("refresh");
    let expires = fields.shift_remove("expires");

    let mut entry = Map::new();
    if let Some(type_value) = type_value {
        entry.insert("type".into(), type_value);
    }
    // TS 登录返回值：anthropic / meta / github-copilot 是
    // `{ type, refresh, access, expires, ... }`，其余是 `{ type, access, refresh, expires, ... }`。
    if matches!(provider_id, "anthropic" | "github-copilot" | "meta") {
        if let Some(refresh) = refresh {
            entry.insert("refresh".into(), refresh);
        }
        if let Some(access) = access {
            entry.insert("access".into(), access);
        }
    } else {
        if let Some(access) = access {
            entry.insert("access".into(), access);
        }
        if let Some(refresh) = refresh {
            entry.insert("refresh".into(), refresh);
        }
    }
    if let Some(expires) = expires {
        entry.insert("expires".into(), expires);
    }
    for (key, value) in fields {
        entry.insert(key, value);
    }
    Ok(Value::Object(entry))
}

/// `auth[providerId] = credential` 在 JS 严格模式（`cli.ts` 是 ESM）下对 null / 原始值的
/// `TypeError` 文案。`JSON.parse` 的顶层值只可能是对象、数组、字符串、数字、布尔或 null；
/// 对象与数组由 [`login`] 分别处理。
///
/// V8 的措辞随接收者类型变化：`null` 用 `Cannot set properties of … (setting '…')`，
/// 原始值用 `Cannot create property '…' on <type> '…'`。数字经 `format_js_number`
/// 输出，所以 `5.0` 写成 `5`（与 JS 一致）；数组/对象走到这里是编程错误，用
/// `unreachable!` 兜底。
fn js_assignment_error(provider_id: &str, value: &Value) -> String {
    match value {
        Value::Null => format!("Cannot set properties of null (setting '{provider_id}')"),
        Value::String(text) => {
            format!("Cannot create property '{provider_id}' on string '{text}'")
        }
        Value::Number(number) => format!(
            "Cannot create property '{provider_id}' on number '{}'",
            kiana_json::js_num::format_js_number(number.as_f64().unwrap_or(f64::NAN))
        ),
        Value::Bool(flag) => {
            format!("Cannot create property '{provider_id}' on boolean '{flag}'")
        }
        Value::Object(_) | Value::Array(_) => {
            unreachable!("login handles objects and arrays before calling this")
        }
    }
}

/// `login(providerId)`：执行 provider 的 OAuth 登录并写 `auth.json`。
///
/// # 参数与返回
///
/// `provider_id` 必须是 [`oauth_providers`] 中某个 provider 的 `id`（大小写敏感、精确
/// 匹配）。返回 [`CliOutcome::Done`] 表示登录成功且 `auth.json` 已重写；
/// [`CliOutcome::Eof`] 表示 stdin 在提示处 EOF——此时不写文件，进程应以 0 退出（见
/// [`CliOutcome`]）。
///
/// # 错误
///
/// - `Unknown provider: <id>`：id 不在 OAuth provider 表中；
/// - OAuth 流程自身的错误（网络、状态校验、token 交换……）经 `From<AuthError>` 变成
///   [`CliError`]，只保留 message；
/// - `auth.json` 的写错误（[`save_auth`] → `From<std::io::Error>`）；读错误被
///   [`load_auth_from`] 吞掉并回退为空对象，不会到达这里。
///
/// # 副作用
///
/// 交互读写 stdin/stdout，并**整体重写**当前工作目录的 `auth.json`（保留其它 provider
/// 的条目；顶层是数组时静默丢弃新凭证，但文件仍会被 [`save_auth`] 按 2 空格缩进重写，
/// 见 `login_with_oauth`）。readline 接口无论成功失败都会关闭（TS 的 `finally`）。
///
/// TS: `cli.ts#login`
pub async fn login(provider_id: &str) -> Result<CliOutcome, CliError> {
    let providers = oauth_providers();
    let provider = providers
        .iter()
        .find(|provider| provider.id() == provider_id)
        .ok_or_else(|| CliError::new(format!("Unknown provider: {provider_id}")))?;
    // `oauth_providers` 的过滤保证 `oauth` 一定存在；这里用 expect 表达不变式：若它
    // 失败，说明 `filter` 与 `Provider::auth()` 的实现不一致，属于编程错误而不是用户错误。
    let oauth = provider
        .auth()
        .oauth
        .as_ref()
        .expect("oauth_providers only contains providers with oauth auth")
        .clone();
    let interaction = CliInteraction::new();
    let outcome = login_with_oauth(oauth.as_ref(), &interaction, provider_id).await;
    // TS: `finally { rl.close(); }`——成功、登录失败、写文件失败都会关闭接口。
    interaction.close();
    outcome
}

/// [`login`] 的 `try { ... }` 部分（`rl.close()` 由调用方在 finally 位置执行）。
///
/// 流程：构造 `LoginOptions` → 运行 `OAuthAuth::login`（期间通过 `interaction` 与用户
/// 交互）→ 若 EOF 标记已置位则直接返回 [`CliOutcome::Eof`]（TS 的 promise 永不 resolve，
/// 不抛错、不写文件）→ 用 `credential_entry` 序列化条目 → [`load_auth`] → 合并 →
/// [`save_auth`] → 打印成功。
///
/// 合并语义逐条对齐 TS 的 `auth[providerId] = credential`：对象插入/覆盖；数组上的非
/// 索引属性不会被 `JSON.stringify` 写出，于是静默丢弃新凭证；标量（null/字符串/数字/
/// 布尔）在严格模式下抛 `TypeError`，文案由 `js_assignment_error` 复刻。
async fn login_with_oauth(
    oauth: &dyn crate::auth::types::OAuthAuth,
    interaction: &CliInteraction,
    provider_id: &str,
) -> Result<CliOutcome, CliError> {
    let options = LoginOptions {
        // TS: `{ getDeviceId: randomUUID }`（node:crypto 的 v4 UUID）。
        get_device_id: Some(Arc::new(|| uuid::Uuid::new_v4().to_string())),
        // TS 只传 getDeviceId，不传 agent name；`None` 让各流程使用自己的内置默认值
        // （例如 Codex originator 的 `kiana`、ChatGPT 的 `AGENT_NAME_HINT`）。
        agent_name: None,
    };
    let credential = match oauth.login(interaction, &options).await {
        Ok(credential) => credential,
        Err(error) => {
            // stdin 在提示处 EOF：TS 的 `rl.question` 永不 resolve，进程以 0 退出。
            if interaction.eof() {
                return Ok(CliOutcome::Eof);
            }
            return Err(error.into());
        }
    };
    let entry = credential_entry(provider_id, &credential)?;
    // 读取发生在登录**之后**：交互期间别的进程写入的条目会被这次读取看到，不会被旧内容
    // 覆盖。真正的并发窗口是 `load_auth()` 与 `save_auth()` 之间——这里没有文件锁，另一个
    // 进程若在这两步之间写入，其条目会被本次 `save_auth` 整体覆盖。TS 同样是先 login 再
    // loadAuth。
    let mut auth = load_auth();
    match &mut auth {
        Value::Object(map) => {
            map.insert(provider_id.to_string(), entry);
        }
        // TS: 数组上的非索引属性不会被 `JSON.stringify` 写出——文件保持原样
        // （新凭证丢失），但仍然打印保存成功。
        //
        // 严格说“原样”指内容语义：随后 `save_auth` 仍会按 2 空格缩进重新序列化整个
        // 数组，所以原文件若是紧凑格式，字节会变但条目不变。
        Value::Array(_) => {}
        other => return Err(CliError::new(js_assignment_error(provider_id, other))),
    }
    save_auth(&auth)?;
    println!("\nCredentials saved to {AUTH_FILE}");
    Ok(CliOutcome::Done)
}

/// 打印 `help` 文本（单次 `console.log`）。
///
/// provider 行先拼成一个字符串，再与固定模板放进**一次** `println!`，对应 TS 的单次
/// `console.log`。模板（含 `login`/`list` 两行的对齐与 `Providers:` 后的两空格缩进）
/// 逐字来自 spec/ai-core.md §3.1，不能顺手重排。
fn print_usage(providers: &[Arc<dyn Provider>]) {
    let provider_list = providers
        .iter()
        .map(|provider| format!("  {} {}", pad_end_20(provider.id()), provider.name()))
        .collect::<Vec<_>>()
        .join("\n");
    println!(
        "Usage: npx @earendil-works/kiana-ai <command> [provider]\n\nCommands:\n  login [provider]  Login to an OAuth provider\n  list              List available providers\n\nProviders:\n{provider_list}"
    );
}

/// `main()`：解析 `process.argv.slice(2)` 并执行命令。
///
/// `args` 不含程序名（[`main`] 传的是 `argv[1..]`）；本函数不写 stderr、不退出进程，
/// 只返回 [`CliOutcome`] 或错误——`Error:` 前缀与退出码由 [`main`] 负责。第一个参数为
/// 空串时与缺省行为相同（JS 里 `""` 为假）。`login` 分支在真正登录前按
/// [`oauth_providers`] 校验一次 id（[`login`] 内部还会再查一次，对应 TS 的两处校验）。
///
/// TS: `cli.ts#main`
pub async fn run(args: &[String]) -> Result<CliOutcome, CliError> {
    let providers = oauth_providers();
    // TS 的 `if (!command ...)`：空字符串在 JS 里为假，与无参数走同一分支。
    let command = args
        .first()
        .filter(|value| !value.is_empty())
        .map(String::as_str);
    match command {
        None | Some("help") | Some("--help") | Some("-h") => {
            // 用法与 provider 列表都来自同一个 `providers`，所以 help 和 list 的集合
            // 永远一致。
            print_usage(&providers);
            Ok(CliOutcome::Done)
        }
        Some("list") => {
            // 每行无前导空格（usage 里的 provider 行才有两空格）；id 列宽 20 由
            // `pad_end_20` 按 UTF-16 码元补齐。
            for provider in &providers {
                println!("{} {}", pad_end_20(provider.id()), provider.name());
            }
            Ok(CliOutcome::Done)
        }
        Some("login") => {
            // TS: `let providerId = args[1]; if (!providerId) { ...交互选择... }`
            // 空字符串在 JS 里为假，因此同样进入交互选择。
            // `args[2..]` 被忽略（TS 同样只看 args[0] 与 args[1]）。
            let mut provider_id = args.get(1).filter(|value| !value.is_empty()).cloned();
            if provider_id.is_none() {
                // TS: `const rl = createInterface(...)` 创建于打印列表之前，
                // `finally { rl.close(); }` 关闭（输入缓冲随接口丢弃）。
                let readline = ReadlineInterface::new();
                // 交互选择拆进一个独立 async 块，块结束后统一关闭接口（TS 的
                // try/finally）。`selected` 的三层 Option 含义：
                // `Ok(None)` = stdin EOF；`Ok(Some(None))` = 输入非法；`Ok(Some(Some(id)))`
                // = 选中了 id。
                let selected: Result<Option<Option<String>>, CliError> = async {
                    for (index, provider) in providers.iter().enumerate() {
                        println!("  {}. {}", index + 1, provider.name());
                    }
                    let question = format!("Enter number (1-{}): ", providers.len());
                    match readline.question(question).await? {
                        QuestionOutcome::Line(line) => {
                            let selected =
                                parse_int_js(&line)
                                    .map(|value| value - 1)
                                    .and_then(|index| {
                                        usize::try_from(index)
                                            .ok()
                                            .and_then(|index| providers.get(index))
                                    });
                            // `Some(None)` = 越界/非数字（TS 的 `providerId` 保持 undefined）。
                            Ok(Some(selected.map(|provider| provider.id().to_string())))
                        }
                        // TS: `rl.question` 在 EOF 上永不 resolve；事件循环耗尽后进程以 0 退出。
                        QuestionOutcome::Eof => Ok(None),
                        QuestionOutcome::Closed => Err(CliError::new("readline was closed")),
                    }
                }
                .await;
                // 与 TS 的 finally 位置一致：成功、EOF、`readline was closed` 都先关闭
                // 接口。`close()` 丢弃接口缓冲，残留行不会流到 `login` 新建的接口。
                readline.close();
                match selected? {
                    Some(selected) => provider_id = selected,
                    None => return Ok(CliOutcome::Eof),
                }
            }
            let Some(provider_id) = provider_id else {
                // TS: `Unknown provider: ${providerId ?? ""}`
                return Err(CliError::new("Unknown provider: "));
            };
            // 先校验再进入登录：未知 id 不会进入 OAuth 流程（不创建登录接口、不触网）。
            // `login` 内部还会再查一次（同一文本），重复是对 TS 两处校验的逐字复刻。
            if !providers
                .iter()
                .any(|provider| provider.id() == provider_id)
            {
                return Err(CliError::new(format!("Unknown provider: {provider_id}")));
            }
            login(&provider_id).await
        }
        // 任何其它首参数（包括只差大小写的 `Login`）都是未知命令。
        Some(other) => Err(CliError::new(format!("Unknown command: {other}"))),
    }
}

/// CLI 入口：`process.argv.slice(2)` + 顶层 `main().catch(...)`。
///
/// 错误时把 `Error: <message>` 写到 stderr 并以 1 退出（与 TS 的
/// `console.error("Error:", error.message); process.exit(1)` 一致）。
///
/// 运行时用**多线程** tokio 并打开全部驱动：OAuth 流程需要定时器（设备码轮询的 sleep）
/// 与 TCP/HTTP（回调服务器、token 交换），stdin 读取则走 `spawn_blocking`。运行时是在
/// 函数里显式构建的（不是 `#[tokio::main]`），因此构建失败也能按同一 `Error: …` 格式
/// 输出并以 1 退出。
///
/// 退出码：正常路径只有两种——`run` 成功（[`CliOutcome::Done`] 与 [`CliOutcome::Eof`]）
/// → 0；任何错误 → 1。`Eof` 也按 0 处理是 TS 的可观察行为：`rl.question` 的 promise
/// 永不 resolve，事件循环耗尽后 Node 以 0 退出（见 [`CliOutcome`]）。成功后函数直接
/// 返回，由进程正常结束获得退出码 0。
///
/// TS: `cli.ts` 的顶层调用
pub fn main() {
    // 与 `process.argv.slice(2)` 对齐：跳过 argv[0]（可执行文件路径）。
    let args: Vec<String> = std::env::args().skip(1).collect();
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        // 运行时都建不起来时没有任何交互可做，直接按顶层错误格式退出。
        Err(error) => {
            eprintln!("Error: {error}");
            std::process::exit(1);
        }
    };
    match runtime.block_on(run(&args)) {
        // `Done` 与 `Eof` 都以 0 退出（EOF 见 [`CliOutcome`]）。
        Ok(_) => {}
        Err(error) => {
            eprintln!("Error: {}", error.message);
            std::process::exit(1);
        }
    }
}

// 本模块只测可离线验证的纯逻辑：`parse_int_js`、`pad_end_20`、provider 顺序、
// `auth.json` 的读写往返、`credential_entry` 的键序、V8 错误文案。CLI 的端到端行为
// （stdin 交互、readline 语义、退出码）由 `tests/cli_login.rs` 以子进程方式覆盖——
// 不要在这里新增读写进程 stdin/fd 0 的用例：全局 `STDIN_EOF` 与 fd 0 会与并行测试冲突。
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_matches_number_parse_int() {
        assert_eq!(parse_int_js("1"), Some(1));
        assert_eq!(parse_int_js(" 2 "), Some(2));
        assert_eq!(parse_int_js("2.9"), Some(2));
        assert_eq!(parse_int_js("+3"), Some(3));
        assert_eq!(parse_int_js("-1"), Some(-1));
        assert_eq!(parse_int_js("abc"), None);
        assert_eq!(parse_int_js(""), None);
        assert_eq!(parse_int_js("   "), None);
        assert_eq!(parse_int_js("-"), None);
        // JS: Number.parseInt("1e3", 10) === 1
        assert_eq!(parse_int_js("1e3"), Some(1));
    }

    #[test]
    fn pad_end_pads_ids_to_twenty_columns() {
        assert_eq!(pad_end_20("openai"), "openai              ");
        assert_eq!(pad_end_20("openai").len(), 20);
        assert_eq!(
            pad_end_20("a-very-long-provider-id"),
            "a-very-long-provider-id"
        );
    }

    #[test]
    fn oauth_provider_list_matches_the_nine_builtin_oauth_providers() {
        let ids: Vec<String> = oauth_providers()
            .iter()
            .map(|provider| provider.id().to_string())
            .collect();
        // TS `builtinProviders()` 的过滤结果顺序（cli.ts 的编号列表依赖它）。
        assert_eq!(
            ids,
            [
                "anthropic",
                "github-copilot",
                "kimi-coding",
                "meta",
                "openai",
                "openai-codex",
                "openrouter",
                "radius",
                "xai",
            ]
        );
    }

    /// 每个用例一个唯一的临时文件（不依赖 cwd，测试可并行）。
    fn temp_path(name: &str) -> std::path::PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        std::env::temp_dir().join(format!(
            "kiana-ai-cli-unit-{name}-{}-{unique}.json",
            std::process::id()
        ))
    }

    fn pretty(value: &Value) -> String {
        kiana_json::js_object::stringify_pretty(value, 2)
    }

    #[test]
    fn load_auth_returns_empty_for_missing_or_invalid_files() {
        let missing = temp_path("missing");
        assert_eq!(load_auth_from(&missing), Value::Object(Map::new()));

        let invalid = temp_path("invalid");
        std::fs::write(&invalid, "not json").expect("write invalid auth.json");
        assert_eq!(load_auth_from(&invalid), Value::Object(Map::new()));
        let _ = std::fs::remove_file(&invalid);
    }

    /// 回归（critical）：`loadAuth` 不做形状校验，已有的 `api_key` 条目必须保留。
    #[test]
    fn load_auth_preserves_api_key_entries_and_unknown_fields() {
        let path = temp_path("preserve");
        let text = r#"{"openai":{"type":"api_key","key":"sk-x"},"anthropic":{"type":"oauth","access":"acc","refresh":"ref","expires":1234567890123,"accountId":"acct"}}"#;
        std::fs::write(&path, text).expect("write auth.json");

        let auth = load_auth_from(&path);
        let map = auth.as_object().expect("object");
        assert_eq!(map.len(), 2);
        assert_eq!(map["openai"]["type"], "api_key");
        assert_eq!(map["openai"]["key"], "sk-x");
        assert_eq!(map["anthropic"]["accountId"], "acct");
        let _ = std::fs::remove_file(&path);
    }

    /// 回归（critical）：登录写入 `type: "oauth"` 且保留文件中已有条目；键序是当前稳定契约，改动需同步测试。
    #[test]
    fn save_auth_writes_type_tag_and_preserves_other_credentials() {
        let path = temp_path("save");
        let text = r#"{"openai":{"type":"api_key","key":"sk-x"},"anthropic":{"type":"oauth","access":"acc","refresh":"ref","expires":1234567890123}}"#;
        std::fs::write(&path, text).expect("write auth.json");

        let mut auth = load_auth_from(&path);
        let credential = OAuthCredential::new("acc-token", "ref-token", 1234567890123.0);
        let entry = credential_entry("anthropic", &credential).expect("credential entry");
        auth.as_object_mut()
            .expect("object")
            .insert("anthropic".into(), entry);
        // 新 provider 追加到末尾（TS `auth[providerId] = credential`）。
        let entry = credential_entry("meta", &credential).expect("credential entry");
        auth.as_object_mut()
            .expect("object")
            .insert("meta".into(), entry);
        save_auth_to(&path, &auth).expect("save auth.json");

        // JS: JSON.stringify({ openai, anthropic, meta }, null, 2)
        let expected = "{\n  \"openai\": {\n    \"type\": \"api_key\",\n    \"key\": \"sk-x\"\n  },\n  \"anthropic\": {\n    \"type\": \"oauth\",\n    \"refresh\": \"ref-token\",\n    \"access\": \"acc-token\",\n    \"expires\": 1234567890123\n  },\n  \"meta\": {\n    \"type\": \"oauth\",\n    \"refresh\": \"ref-token\",\n    \"access\": \"acc-token\",\n    \"expires\": 1234567890123\n  }\n}";
        assert_eq!(std::fs::read_to_string(&path).expect("read"), expected);
        let _ = std::fs::remove_file(&path);
    }

    /// 回归（minor）：`Credential` 序列化补上 `type` 后，仍按 TS 对象字面量的键序写出。
    #[test]
    fn credential_entry_matches_ts_literal_key_order() {
        let credential = OAuthCredential::new("acc", "ref", 1234567890123.0);

        // anthropic / meta / github-copilot: { type, refresh, access, expires }
        for provider in ["anthropic", "meta", "github-copilot"] {
            assert_eq!(
                pretty(&credential_entry(provider, &credential).expect("entry")),
                "{\n  \"type\": \"oauth\",\n  \"refresh\": \"ref\",\n  \"access\": \"acc\",\n  \"expires\": 1234567890123\n}",
                "{provider}"
            );
        }
        // 其余 6 个: { type, access, refresh, expires }
        for provider in [
            "kimi-coding",
            "openai",
            "openai-codex",
            "openrouter",
            "radius",
            "xai",
        ] {
            assert_eq!(
                pretty(&credential_entry(provider, &credential).expect("entry")),
                "{\n  \"type\": \"oauth\",\n  \"access\": \"acc\",\n  \"refresh\": \"ref\",\n  \"expires\": 1234567890123\n}",
                "{provider}"
            );
        }
    }

    /// 未知字段按 `OAuthCredential::extra` 的顺序跟在 `expires` 之后
    /// （github-copilot 的 `enterpriseUrl`、`availableModelIds`；openai 的 `clientId`、`scopes`）。
    #[test]
    fn credential_entry_keeps_extra_field_order() {
        let mut copilot = OAuthCredential::new("acc", "ref", 1.0);
        copilot.extra.insert(
            "enterpriseUrl".into(),
            Value::String("company.ghe.com".into()),
        );
        copilot.extra.insert(
            "availableModelIds".into(),
            Value::Array(vec![Value::String("gpt-5.4".into())]),
        );
        assert_eq!(
            pretty(&credential_entry("github-copilot", &copilot).expect("entry")),
            "{\n  \"type\": \"oauth\",\n  \"refresh\": \"ref\",\n  \"access\": \"acc\",\n  \"expires\": 1,\n  \"enterpriseUrl\": \"company.ghe.com\",\n  \"availableModelIds\": [\n    \"gpt-5.4\"\n  ]\n}"
        );

        let mut openai = OAuthCredential::new("acc", "ref", 1.0);
        openai
            .extra
            .insert("clientId".into(), Value::String("cid".into()));
        openai.extra.insert(
            "scopes".into(),
            Value::Array(vec![Value::String("openid".into())]),
        );
        assert_eq!(
            pretty(&credential_entry("openai", &openai).expect("entry")),
            "{\n  \"type\": \"oauth\",\n  \"access\": \"acc\",\n  \"refresh\": \"ref\",\n  \"expires\": 1,\n  \"clientId\": \"cid\",\n  \"scopes\": [\n    \"openid\"\n  ]\n}"
        );
    }

    #[test]
    fn js_assignment_error_matches_v8_messages() {
        assert_eq!(
            js_assignment_error("anthropic", &Value::Null),
            "Cannot set properties of null (setting 'anthropic')"
        );
        assert_eq!(
            js_assignment_error("anthropic", &Value::String("x".into())),
            "Cannot create property 'anthropic' on string 'x'"
        );
        assert_eq!(
            js_assignment_error("anthropic", &serde_json::json!(5)),
            "Cannot create property 'anthropic' on number '5'"
        );
        assert_eq!(
            js_assignment_error("anthropic", &serde_json::json!(true)),
            "Cannot create property 'anthropic' on boolean 'true'"
        );
    }
}
