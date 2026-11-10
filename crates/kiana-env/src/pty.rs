//! `pty.rs`：伪终端里的程序，供交互式终端使用。与 `exec` 不同，输出是终端的原始字节，且程序读输入：
//! `ptyWrite`、`ptyResize`、`ptyForeground` 按 `pty` 请求的 id 寻址运行中的请求。
//!
//! TS 源：packages/env/daemon/src/pty.rs（原 daemon 本身就是 Rust；逐字移植）。
//! `pty`: a program in a pseudo-terminal, for interactive terminals. Unlike `exec`, output is the terminal's raw
//! bytes, and the program reads input: `ptyWrite`, `ptyResize` and `ptyForeground` address the running `pty` request by
//! its id.
//!
//! # 职责与边界
//! 本模块只处理"交互式终端"这一种执行形态：把程序放进新的伪终端（pseudo-terminal，简称 pty）运行，
//! 把终端的原始输出字节以 `event { kind: "output" }` 事件发回客户端，并支持运行期间写入击键、
//! 调整窗口大小、查询前台进程组。非交互命令（UTF-8 解码 stdout/stderr、超时、把完整输出落盘
//! 的 spill）由 `exec` 模块负责，两者不要混淆：`exec` 的输出去解码、按流区分；pty 的输出是原样字节。
//!
//! # 在系统中的位置
//! 调用关系（见 `main.rs`）：
//! - `pty::run` 由 `Server::dispatch` 在收到 `pty` 请求时调用，运行在独立请求线程上
//!   （`main.rs` 对 `exec`/`watch`/`pty` 统一使用 16 MiB 栈）；
//! - `ptyWrite` / `ptyResize` / `ptyForeground` 由读取帧的线程直接调用 `write` / `resize` /
//!   `foreground`，不经过工作线程池，从而保证同一终端的击键按到达顺序生效；
//! - 运行中的进程组 pid 登记在 `Groups` 里，连接结束（`main::kill_all`）时会被统一杀掉；
//! - 输出统一交给 `Output`：`started` 事件走 control 队列，输出字节走 bulk 队列。
//!
//! # 执行流程（Unix）
//! 1. `PtyRequest::from_json` 校验请求：`cwd` 必填，`cols`/`rows` 必须在 1..=4096；
//!    `argv` 缺省或为 `null` 表示启动用户的登录 shell。
//! 2. `run` 先检查取消标志与工作目录，再 `openpty` 得到 master/slave 两个描述符；子进程以 slave 作为
//!    stdin/stdout/stderr，并在 `pre_exec` 里 `setsid` + `TIOCSCTTY`，成为新会话首进程、终端的前台进程组。
//! 3. 把 `Terminal`（输入发送端、master、pid）登记进 `Terminals`，先发 `started` 事件，再启动三个辅助线程：
//!    master 读取线程、master 写出线程、`child.wait()` 线程。
//! 4. `run` 自身进入事件循环，把读取线程送来的原始字节作为 `output` 事件转给 `Output`，
//!    直到程序退出且输出结束，或退出后 100 ms 内没有新输出（`EXIT_GRACE`）。
//! 5. 清理：置 `stop`、注销终端与进程组、等读取线程结束，然后按取消/退出状态返回结果。
//!
//! # 关键约束
//! - `Terminals` 以 `pty` 请求的 id 为键；请求结束时才移除，移除后 `ptyWrite` 等返回 `EBADF`。
//! - `target` 在 JSON 里是数字，而协议规定请求 id 是 `u32`，所以查表前按 `u32` 截断。
//! - 输出是终端原始字节，不做 UTF-8 解码（与 `exec` 不同），客户端自行决定如何呈现。
//! - `MAX_CELLS` 分别限制 `cols` 和 `rows` 的取值，不是两者的乘积。
//!
//! # 失败模式
//! - 请求参数不合法：`EINVAL`（缺 `cwd`、尺寸越界、`argv` 形状错误）。
//! - 启动失败（程序不存在、`openpty`/`spawn` 失败）：`spawn_error`。
//! - 取消：`aborted`；`{ mode: "kill" }` 只杀进程组、不标记取消，最终以被杀进程的退出码返回。
//! - 终端已结束仍被写入/改尺寸/查前台：`EBADF`（改尺寸可能是 ioctl 错误）。
//! - 非 Unix 平台：`run` 返回 `ENOTSUP`，`resize` 退化为空操作、`foreground` 返回 `null`。
//!
//! 这些错误由调用方（`main.rs` 的 `dispatch` 或帧读取分支）转成 ERROR 帧；本模块不自动重试：
//! 参数错误需要修正请求，`EBADF` 表示终端已消失、需要重新发起 `pty` 请求，`ENOTSUP` 在平台实现前无法恢复。
//!
//! # 阅读顺序
//! `PtyRequest::from_json` → `run`（状态机与线程协作）→ `mod unix`（系统调用细节）。

use crate::errors::Failure;
use crate::exec::{Control, Groups};
#[cfg(unix)]
use crate::exec::{Message, kill_group};
#[cfg(unix)]
use crate::frame::{EVENT, Frame};
use crate::output::Output;
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

/// After the program exits, output still arriving is collected until this much silence.
#[cfg(unix)]
const EXIT_GRACE: std::time::Duration = std::time::Duration::from_millis(100);
/// Largest terminal size accepted, in cells.
///
/// 对 `cols` 与 `rows` 分别生效（各自 1..=4096），不是两者乘积的上限。
const MAX_CELLS: u64 = 4096;

/// What `ptyWrite`, `ptyResize` and `ptyForeground` reach: the terminal of one running `pty` request.
///
/// 由 `run` 在 `openpty` 成功后创建并放入 `Terminals`，请求结束时移除。三个终端操作都先按 `target`
/// 查到它再执行动作。字段随平台不同：`input` 在所有平台都有，`master`/`pid` 仅 Unix。
pub struct Terminal {
    /// 输入字节队列的发送端：`ptyWrite` 把 payload 放进队列，由 `run` 的写出线程按到达顺序写入 master。
    /// 接收端由写出线程持有；写出线程退出后接收端被丢弃，此后的 `send` 失败即表示"终端已结束"。
    input: Sender<Vec<u8>>,
    /// 终端 master 描述符：读取线程从这里读程序输出，`resize`/`foreground` 对它做 ioctl。
    #[cfg(unix)]
    master: Arc<std::fs::File>,
    /// The program's process id, which is also its process group's (it leads the terminal's session).
    #[cfg(unix)]
    pid: u32,
}

/// Running terminals by the id of their `pty` request.
///
/// 键是发起该终端的 `pty` 请求 id。值用 `Arc` 共享，读取帧的线程与请求线程会并发访问；
/// 锁只保护这张表的查找/插入/删除，不覆盖任何阻塞式 IO，因此慢速写终端不会卡住其他请求。
pub type Terminals = Arc<Mutex<HashMap<u32, Arc<Terminal>>>>;

/// 一次 `pty` 请求解析后的参数。
///
/// 字段语义：
/// - `argv`：`None` 表示不指定程序，`run` 会以登录 shell 启动（见 `unix::command`）；
/// - `cwd`：必填，`run` 在启动前检查其存在；
/// - `env`：请求携带的环境变量，值保留原始 JSON 类型，非字符串值在真正构建子进程环境时被忽略；
/// - `inherit_env`：`inheritEnv` 缺省或不是布尔值时按 `true` 处理；
/// - `cols`/`rows`：终端尺寸，已在此处校验为 1..=4096。
///
/// 非 Unix 平台没有对应实现，`run` 完全不读取这些字段，因此允许 dead_code。
#[cfg_attr(not(unix), allow(dead_code))]
pub struct PtyRequest {
    /// 要运行的程序；`None` 表示用户的登录 shell。
    argv: Option<Vec<String>>,
    /// 程序的工作目录（必填）。
    cwd: String,
    /// 请求指定的环境变量；非字符串值保留在这里，构建环境时被筛选掉。
    env: Map<String, Value>,
    /// 是否继承守护进程自身环境（对应协议字段 `inheritEnv`，默认 `true`）。
    inherit_env: bool,
    /// 终端列数，1..=4096 个字符单元。
    cols: u16,
    /// 终端行数，1..=4096 个字符单元。
    rows: u16,
}

/// 解析并校验一个终端维度（`cols` 或 `rows`）：只接受 1..=4096 的 JSON 整数。
///
/// 错误消息里带上字段名（`key`），调用方不需要自己拼接；小数、负数、字符串、布尔、null
/// 与缺失一律视为同一类错误 `EINVAL`。
fn size(json: &Value, key: &str) -> Result<u16, Failure> {
    match json[key].as_u64() {
        Some(value) if (1..=MAX_CELLS).contains(&value) => Ok(value as u16),
        _ => Err(Failure::new(
            "EINVAL",
            format!("{key} must be between 1 and {MAX_CELLS}"),
        )),
    }
}

impl PtyRequest {
    /// 把 `pty` 请求的 JSON 解析成 `PtyRequest`，并按固定顺序校验（`argv` → `cwd` → 尺寸）。
    ///
    /// - `argv` 缺失或为 `null` → `None`；否则必须是非空字符串数组，任何非字符串元素、空数组
    ///   或非数组值都会得到 `EINVAL("argv must be a non-empty array of strings")`；
    /// - `cwd` 必须是字符串，否则 `EINVAL("pty needs cwd")`；该校验先于尺寸校验，
    ///   所以同时缺 `cwd` 且尺寸非法时报告前者（测试固定了这一行为）；
    /// - `cols`/`rows` 必需，缺失也按错误处理，因为协议要求终端尺寸；
    /// - `env` 缺失或不是对象时为空表；`inheritEnv` 非布尔时回到默认 `true`。
    pub fn from_json(json: &Value) -> Result<PtyRequest, Failure> {
        let argv = match &json["argv"] {
            Value::Null => None,
            value => Some(
                value
                    .as_array()
                    .and_then(|items| {
                        items
                            .iter()
                            .map(|item| item.as_str().map(str::to_string))
                            .collect()
                    })
                    .filter(|argv: &Vec<String>| !argv.is_empty())
                    .ok_or_else(|| {
                        Failure::new("EINVAL", "argv must be a non-empty array of strings")
                    })?,
            ),
        };
        Ok(PtyRequest {
            argv,
            cwd: json["cwd"]
                .as_str()
                .ok_or_else(|| Failure::new("EINVAL", "pty needs cwd"))?
                .to_string(),
            env: json["env"].as_object().cloned().unwrap_or_default(),
            inherit_env: json["inheritEnv"].as_bool().unwrap_or(true),
            cols: size(json, "cols")?,
            rows: size(json, "rows")?,
        })
    }
}

/// 按 `target` 查找运行中的终端。
///
/// `target` 缺失或不是 `u64` → `EINVAL`；查不到 → `EBADF("no running terminal with this id")`。
/// 协议规定请求 id 是 `u32`，所以这里截断后查表（测试覆盖了 `2^32 + 7` 命中 `7` 的情况）。
/// 返回 `Arc` 克隆而不是借用，使调用方可以在不持有 `Terminals` 锁的情况下完成后续 IO。
fn find(terminals: &Terminals, json: &Value) -> Result<Arc<Terminal>, Failure> {
    let target = json["target"]
        .as_u64()
        .ok_or_else(|| Failure::new("EINVAL", "missing field target"))?;
    terminals
        .lock()
        .unwrap()
        .get(&(target as u32))
        .cloned()
        .ok_or_else(|| Failure::new("EBADF", "no running terminal with this id"))
}

/// Queue input for the terminal; it is written in the order it arrives, without holding up other requests.
///
/// 只把 payload 放进通道就返回，真正的写入在 `run` 的写出线程里完成，因此读取帧的线程不会被
/// "暂时不读输入"的程序阻塞。发送失败说明写出线程已退出（终端结束或写入出错），返回 `EBADF`。
/// 注意 `std::sync::mpsc::channel` 无界：程序长期不读输入时，数据会在通道里排队等待写出，
/// 期间 `ptyWrite` 仍会成功返回，不会施加反压。
pub fn write(terminals: &Terminals, json: &Value, payload: Vec<u8>) -> Result<Value, Failure> {
    let terminal = find(terminals, json)?;
    terminal
        .input
        .send(payload)
        .map_err(|_| Failure::new("EBADF", "the terminal has ended"))?;
    Ok(json!({}))
}

/// Change the terminal's size; the program receives `SIGWINCH`.
///
/// 先查找目标（找不到就是 `EBADF`，早于尺寸校验，测试锁定），再校验 `cols`/`rows`，最后发 ioctl：
/// 非法尺寸不会到达终端。Unix 上 `TIOCSWINSZ` 失败会映射成 `Failure::io`（syscall 记为 `ioctl`、
/// path 记为 `pty`）；非 Unix 平台什么都不做，直接返回 `{}`。
pub fn resize(terminals: &Terminals, json: &Value) -> Result<Value, Failure> {
    let terminal = find(terminals, json)?;
    let (cols, rows) = (size(json, "cols")?, size(json, "rows")?);
    #[cfg(unix)]
    unix::set_size(&terminal.master, cols, rows)
        .map_err(|error| Failure::io(&error, "ioctl", "pty"))?;
    #[cfg(not(unix))]
    let _ = (terminal, cols, rows);
    Ok(json!({}))
}

/// The process group in the terminal's foreground when it is not the program's own: a job a shell started, such as an
/// editor. `{ foreground: null }` while the program itself is in front (a shell at its prompt).
///
/// 返回值：程序自身在前台，或 `tcgetpgrp` 报告非正数时，为 `{ foreground: null }`；否则为
/// `{ foreground: { pid, name } }`，其中 `pid` 是前台进程组 id（会话首进程的 pid 也是它的进程组 id），
/// `name` 是该组首进程的名字，取不到时为 `null`。终端已结束时 `find` 返回 `EBADF`；
/// `tcgetpgrp` 失败映射为 syscall 错误。非 Unix 平台总是返回 `null`。
pub fn foreground(terminals: &Terminals, json: &Value) -> Result<Value, Failure> {
    let terminal = find(terminals, json)?;
    #[cfg(unix)]
    {
        let group = unix::foreground(&terminal.master)
            .map_err(|error| Failure::io(&error, "tcgetpgrp", "pty"))?;
        if group <= 0 || group as u32 == terminal.pid {
            return Ok(json!({ "foreground": null }));
        }
        let name = unix::process_name(group);
        Ok(json!({ "foreground": { "pid": group, "name": name } }))
    }
    #[cfg(not(unix))]
    {
        let _ = terminal;
        Ok(json!({ "foreground": null }))
    }
}

/// 非 Unix 平台的占位实现：伪终端依赖 `openpty`、会话控制与终端 ioctl，尚未移植到 Windows，
/// 因此直接返回 `ENOTSUP`（协议里明确记录了这一限制）。
#[cfg(not(unix))]
pub fn run(
    _id: u32,
    _request: PtyRequest,
    _output: &Arc<Output>,
    _control: &Control,
    _groups: &Groups,
    _terminals: &Terminals,
) -> Result<Value, Failure> {
    Err(Failure::new(
        "ENOTSUP",
        "Terminals are not supported on this system yet",
    ))
}

/// Run the program in a new terminal, sending a `started` event and then its output, until it exits.
///
/// 本函数是 pty 请求的核心状态机，运行在请求线程上。读取线程与等待退出的线程通过
/// `std::sync::mpsc`（多生产者、单消费者）消息通道把 `Data`/`Eof`（读取线程）与
/// `Exit`/`WaitFailed`（等待线程）送回本循环，`Control` 也可向同一条通道投递
/// `Cancel`/`Kill`；写出线程走另一条输入通道，只从其中取待写入字节写入 master，
/// 不向主循环回送消息。详见下方"状态与消息"。
///
/// # 返回值
/// 正常结束返回 `{ exitCode }`：`sys::exit_code` 会把"被信号杀死"映射为 `128 + signal`。
///
/// # 错误
/// - 取消（收到 `Cancel`）→ `Failure("aborted")`；
/// - `child.wait()` 失败 → `spawn_error`；循环结束却从未收到退出消息 → `spawn_error`；
/// - 启动阶段失败（`cwd` 不存在、`openpty`/`Command::spawn` 失败）→ `spawn_error`。
///
/// # 状态与消息（状态机）
/// 循环由三个变量决定何时结束：`status`（`None` → `Some(Ok(ExitStatus))`/`Some(Err)`）、
/// `ended`（读取线程报告 `Eof`）、`idle_until`（退出后等待残余输出的截止时间）。
/// 消息类型由 `exec::Message` 定义。消息处理：
/// - `Data`：把终端字节作为 bulk `output` 事件发出；进程已退出时把截止时间再延后 `EXIT_GRACE`；
/// - `Eof`：终端输出结束；
/// - `Exit` / `WaitFailed`：记录退出状态并开始 `EXIT_GRACE` 倒计时（`WaitFailed` 立即到期）；
/// - `Cancel`：置 `aborted` 并杀掉进程组；
/// - `Kill`：只杀进程组，不置 `aborted`；
/// - `recv_timeout` 超时：回到循环顶部重新检查终止条件与截止时间。
/// 结束条件：`status` 已记录且 `Eof` 已到；或收尾期限已过（后台任务可能继续占用终端，
/// 不能无限等）；或所有发送端消失导致通道断开。
///
/// # 并发与清理
/// - 三个辅助线程：读取线程（master → 本循环）、写出线程（输入通道 → master）、等待线程
///   （`child.wait`）。读取线程在 `stop` 置位（`poll` 每 100 ms 检查一次）或上层消息通道
///   关闭后退出；写出线程在写入数据时看到 `stop` 会停止，但空闲阻塞在输入通道上时不会被
///   `stop` 唤醒，通常要等本函数移除 `Terminal`、关闭输入通道后才退出（见清理步骤）；
///   等待线程在程序结束、`child.wait()` 返回后退出。本函数只 `join` 读取线程，不 join
///   另外两个线程（它们持有各自的资源副本，退出即释放）。
/// - 读取线程每送出一块数据后调用 `Output::wait_for_room`，在未发送的 bulk 输出超过上限时等待，
///   效果等同"管道写满"的反压（backpressure：消费端变慢时让生产端等待；这里最多再多一块数据），
///   不会在本进程内丢弃输出。
/// - 结束时先置 `stop`，再从 `Terminals`/`Groups` 注销；注销之后到达的 `ptyWrite` 查不到终端，返回 `EBADF`。
///
/// # 注意事项
/// - 取消可能先于进程启动到达：入口会检查 `control.aborted` 并直接返回；若是 kill 模式，
///   则先照常启动，随后检查 `control.killed` 并立即杀组，`killed` 标志保证重复 kill 只执行一次。
/// - `groups` 记录 pid，供连接断开时 `main::kill_all` 统一清理。
#[cfg(unix)]
pub fn run(
    id: u32,
    request: PtyRequest,
    output: &Arc<Output>,
    control: &Control,
    groups: &Groups,
    terminals: &Terminals,
) -> Result<Value, Failure> {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{self, RecvTimeoutError};
    use std::thread;
    use std::time::{Duration, Instant};

    // 读取线程与等待线程往这个通道发消息，`Control` 里的克隆让取消操作也能投递 `Cancel`/`Kill`；
    // 写出线程不经过这里，它从下面独立的 `inputs` 通道取待写入字节。
    let (sender, receiver) = mpsc::channel();
    control.attach(sender.clone());
    if control.aborted.load(Ordering::SeqCst) {
        return Err(Failure::new("aborted", "aborted"));
    }
    // 与 exec 一致：先确认工作目录存在，失败归为 spawn_error（pty 协议没有超时参数）。
    if !std::path::Path::new(&request.cwd).exists() {
        return Err(Failure::new(
            "spawn_error",
            format!("Working directory does not exist: {}", request.cwd),
        ));
    }
    // 启动子进程：`spawn` 内部完成 openpty、会话/控制终端设置以及 master 的非阻塞设置。
    let (master, mut child) =
        unix::spawn(&request).map_err(|error| Failure::new("spawn_error", error.to_string()))?;
    let master = Arc::new(master);
    let pid = child.id();
    // 登记进程组：连接断开时 main::kill_all 会据此杀掉仍在运行的终端程序。
    groups.lock().unwrap().insert(pid);
    let (input, inputs) = mpsc::channel::<Vec<u8>>();
    // 先登记终端再发 started：客户端收到 started 后立刻发来的 ptyWrite 必须能查到它。
    terminals.lock().unwrap().insert(
        id,
        Arc::new(Terminal {
            input,
            master: master.clone(),
            pid,
        }),
    );
    // started 走 control 队列，先于任何 bulk 输出，保证客户端先拿到 pid 再看到输出。
    output.control(Frame::new(
        EVENT,
        id,
        json!({ "kind": "started", "pid": pid }),
    ));

    let stop = Arc::new(AtomicBool::new(false));
    // 读取线程：master 上的原始字节 → 事件循环。每次送出后 `wait_for_room` 形成反压；
    // 若 `send` 失败（本函数已退出），`on_data` 返回 false，读取线程随之结束。
    let reader = {
        let master = master.clone();
        let sender = sender.clone();
        let stop = stop.clone();
        let output = output.clone();
        thread::spawn(move || {
            unix::read(&master, &stop, |bytes| {
                // `Data`/`Eof` 的流索引对 pty 没有意义（只有一根终端流），这里固定传 0。
                let delivered = sender.send(Message::Data(0, bytes.to_vec())).is_ok();
                // Like a full pipe: a program that writes faster than the link waits.
                output.wait_for_room(&stop);
                delivered
            });
            let _ = sender.send(Message::Eof(0));
        })
    };
    // 写出线程：输入通道按到达顺序交付，逐块写入 master；写入途中看到 `stop` 置位会停止，
    // 但空闲阻塞在 `for bytes in inputs` 时不会被唤醒，要等 `run` 移除 `Terminal` 关闭输入通道才退出。
    {
        let master = master.clone();
        let stop = stop.clone();
        thread::spawn(move || {
            for bytes in inputs {
                if !unix::write_all(&master, &bytes, &stop) {
                    return;
                }
            }
        });
    }
    // 等待线程：`child.wait()` 会阻塞，必须放到独立线程，主循环才能同时接收输出。
    let waiter = sender.clone();
    thread::spawn(move || {
        let _ = waiter.send(match child.wait() {
            Ok(status) => Message::Exit(status),
            Err(error) => Message::WaitFailed(error.to_string()),
        });
    });
    // 本函数持有的发送端到此结束；`Control` 与辅助线程里的克隆仍让通道保持连通。
    drop(sender);

    let mut status = None;
    let mut ended = false;
    let mut idle_until: Option<Instant> = None;
    let mut aborted = false;
    let mut killed = false;
    // 幂等杀组：`Cancel` 与 `Kill` 可能先后到达，`killed` 保证只杀一次。
    let kill = |killed: &mut bool| {
        if !*killed {
            *killed = true;
            kill_group(pid);
        }
    };
    // kill 模式可能在子进程启动前就已请求，这里补一次杀组。
    if control.killed.load(Ordering::SeqCst) {
        kill(&mut killed);
    }
    loop {
        // 先检查终止条件，再决定本次等待多久。每条消息都会更新状态或产生副作用（`Data`
        // 发送 bulk 输出事件，`Cancel`/`Kill` 通过 `kill` 闭包向进程组发信号）；是否需要结束
        // 由循环顶部依据 `status`/`ended`/`idle_until` 判断。
        if status.is_some() && ended {
            break;
        }
        // 没有截止时间时用 1 小时作为"长等待"，实际会被通道消息提前唤醒。
        let wait = match idle_until {
            Some(idle) => {
                let now = Instant::now();
                if now >= idle {
                    // A background job may keep the terminal open after the program exited.
                    break;
                }
                idle - now
            }
            None => Duration::from_secs(3600),
        };
        match receiver.recv_timeout(wait) {
            Ok(Message::Data(_, bytes)) => {
                // 终端字节原样作为 bulk 输出事件；退出后收到的残余输出会延长收尾期限。
                output.bulk(
                    Frame::with_payload(EVENT, id, json!({ "kind": "output" }), bytes),
                    None,
                );
                if status.is_some() {
                    idle_until = Some(Instant::now() + EXIT_GRACE);
                }
            }
            Ok(Message::Eof(_)) => ended = true,
            Ok(Message::Exit(exit)) => {
                status = Some(Ok(exit));
                idle_until = Some(Instant::now() + EXIT_GRACE);
            }
            Ok(Message::WaitFailed(error)) => {
                status = Some(Err(error));
                idle_until = Some(Instant::now());
            }
            Ok(Message::Cancel) => {
                // abort：先记下取消，再杀组；返回时 aborted 优先于退出码。
                aborted = true;
                kill(&mut killed);
            }
            Ok(Message::Kill) => kill(&mut killed),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    // 收尾：`stop` 让 poll/写入中的线程尽快退出；移除终端会丢弃输入发送端，写出线程随之结束。
    stop.store(true, Ordering::SeqCst);
    // Ending the input channel ends the writer thread.
    terminals.lock().unwrap().remove(&id);
    groups.lock().unwrap().remove(&pid);
    let _ = reader.join();
    // 取消优先：即便进程随后被杀并产生退出状态，也统一报告 aborted。
    if aborted {
        return Err(Failure::new("aborted", "aborted"));
    }
    // 退出码交给平台层转换（Unix 上被信号杀死记为 128 + signal）。
    match status {
        Some(Ok(status)) => Ok(json!({ "exitCode": crate::sys::exit_code(status) })),
        Some(Err(error)) => Err(Failure::new("spawn_error", error)),
        None => Err(Failure::new(
            "spawn_error",
            "process did not report its exit",
        )),
    }
}

#[cfg(unix)]
mod unix {
    //! Unix 平台层：直接封装 `openpty`、`setsid`、`TIOCSCTTY`、`TIOCSWINSZ`、`tcgetpgrp`、`poll`
    //! 等 libc 调用。
    //!
    //! 这里的 `unsafe` 都是对 libc 的 FFI。每个调用点的 SAFETY 注释说明它依赖的指针/描述符
    //! 有效性与所有权；调用方必须保证传入的 `File` 在调用期间存活（`Terminal` 用 `Arc` 持有
    //! master，引用不会超过调用期）。
    //!
    //! 读写都依赖 master 上设置的 `O_NONBLOCK`：系统调用返回 `WouldBlock` 时用 `poll` 等待，
    //! 从而能在等待间隙检查停止标志，而不是永久阻塞。
    use super::PtyRequest;
    use std::fs::File;
    use std::io::{self, Read, Write};
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::process::CommandExt;
    use std::process::{Child, Command, Stdio};
    use std::sync::atomic::{AtomicBool, Ordering};

    use serde_json::Value;

    // libc 失败时返回 -1 并设置 errno，其余取值原样返回；调用点统一用 `?` 传播。
    fn check(result: libc::c_int) -> io::Result<libc::c_int> {
        if result < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(result)
        }
    }

    // 内核的 `winsize` 只使用行列；像素尺寸字段置 0 表示未知。
    fn winsize(cols: u16, rows: u16) -> libc::winsize {
        libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        }
    }

    /// 改变内核记录的终端尺寸；内核随后向终端的前台进程组发送 `SIGWINCH`。
    pub fn set_size(master: &File, cols: u16, rows: u16) -> io::Result<()> {
        let size = winsize(cols, rows);
        // SAFETY: TIOCSWINSZ reads one winsize from a valid pointer.
        check(unsafe { libc::ioctl(master.as_raw_fd(), libc::TIOCSWINSZ as _, &size) })?;
        Ok(())
    }

    /// The terminal's foreground process group.
    pub fn foreground(master: &File) -> io::Result<libc::pid_t> {
        // SAFETY: tcgetpgrp on a descriptor the terminal owns.
        let group = unsafe { libc::tcgetpgrp(master.as_raw_fd()) };
        if group < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(group)
        }
    }

    /// A process's name, if it still runs and the system tells.
    ///
    /// macOS：`proc_name` 最多写 `buffer.len()` 字节并返回写入长度；返回非正数表示取不到。
    #[cfg(target_vendor = "apple")]
    pub fn process_name(pid: libc::pid_t) -> Option<String> {
        let mut buffer = [0u8; 256];
        // SAFETY: proc_name writes at most `buffer.len()` bytes and returns how many.
        let length =
            unsafe { libc::proc_name(pid, buffer.as_mut_ptr().cast(), buffer.len() as u32) };
        (length > 0).then(|| String::from_utf8_lossy(&buffer[..length as usize]).into_owned())
    }

    /// Linux/Android：读 `/proc/<pid>/comm` 并去掉行尾换行；进程已退出或文件不可读时返回 `None`。
    #[cfg(any(target_os = "linux", target_os = "android"))]
    pub fn process_name(pid: libc::pid_t) -> Option<String> {
        let name = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
        Some(name.trim_end().to_string()).filter(|name| !name.is_empty())
    }

    /// 其它 Unix 没有可移植的取进程名方式，返回 `None`（对应 `ptyForeground` 的 `name: null`）。
    #[cfg(not(any(target_vendor = "apple", target_os = "linux", target_os = "android")))]
    pub fn process_name(_pid: libc::pid_t) -> Option<String> {
        None
    }

    /// 给描述符加上 `FD_CLOEXEC`，避免 fork/exec 后子进程额外继承 master/slave。
    /// 子进程的 stdin/stdout/stderr 由 `Stdio` 复制并显式传入，不受此标志影响。
    fn cloexec(fd: libc::c_int) -> io::Result<()> {
        // SAFETY: fcntl on a descriptor this function's caller owns.
        unsafe {
            let flags = check(libc::fcntl(fd, libc::F_GETFD))?;
            check(libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC))?;
        }
        Ok(())
    }

    /// The program to run: `argv`, or the user's shell as a login shell (`-` before its name, as `login` does).
    ///
    /// 无 `argv` 时的选择顺序：请求 `env.SHELL` → 守护进程自身的 `SHELL` → `/bin/sh`；
    /// 前两者还要求非空且路径存在（`sys::path_exists`）。`arg0` 前加 `-` 是登录 shell 的约定，
    /// shell 据此按登录会话读取配置文件。
    fn command(request: &PtyRequest) -> Command {
        match &request.argv {
            Some(argv) => {
                let mut command = Command::new(&argv[0]);
                command.args(&argv[1..]);
                command
            }
            None => {
                let shell = request
                    .env
                    .get("SHELL")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .or_else(|| std::env::var("SHELL").ok())
                    .filter(|shell| !shell.is_empty() && crate::sys::path_exists(shell))
                    .unwrap_or_else(|| "/bin/sh".into());
                let name = shell.rsplit('/').next().unwrap_or("sh").to_string();
                let mut command = Command::new(&shell);
                command.arg0(format!("-{name}"));
                command
            }
        }
    }

    /// 创建伪终端对并启动程序，返回父进程持有的 master 与 `Child`。
    ///
    /// 阶段（与函数内编号注释对应）：
    /// 1. `openpty` 创建 master/slave，初始尺寸直接带入；
    /// 2. 裸描述符收进 `OwnedFd`，由 Rust 负责关闭；
    /// 3. master/slave 设 `FD_CLOEXEC`；
    /// 4. 子进程 stdin/stdout/stderr 都指向 slave 的副本；
    /// 5. 应用工作目录与环境变量；
    /// 6. `pre_exec` 中 `setsid` + `TIOCSCTTY`，建立会话与控制终端；
    /// 7. `spawn` 执行程序，随后父进程关闭自己的 slave 副本；
    /// 8. master 设为非阻塞，供 `poll` 驱动的读写循环使用。
    ///
    /// 错误：任一步系统调用失败都以 `io::Error` 返回，由 `run` 统一映射成 `spawn_error`。
    pub fn spawn(request: &PtyRequest) -> io::Result<(File, Child)> {
        // 1) 创建伪终端对，并带入初始尺寸。
        let mut master: libc::c_int = -1;
        let mut slave: libc::c_int = -1;
        let mut size = winsize(request.cols, request.rows);
        // SAFETY: openpty writes two descriptors; the name and termios are not requested.
        check(unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &raw mut size,
            )
        })?;
        // 2) 把裸描述符收进 OwnedFd：从此归还给 Rust 管理，提前返回也不会泄漏。
        // SAFETY: openpty returned two new descriptors this function now owns.
        let (master, slave) =
            unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
        // 3) 防止 exec 后被子进程额外继承。
        cloexec(master.as_raw_fd())?;
        cloexec(slave.as_raw_fd())?;
        let slave = File::from(slave);
        let mut command = command(request);
        // 4) 三个标准流都是 slave 的 dup；父进程保留 slave，等 spawn 后关闭。
        command
            .current_dir(&request.cwd)
            .stdin(Stdio::from(slave.try_clone()?))
            .stdout(Stdio::from(slave.try_clone()?))
            .stderr(Stdio::from(slave.try_clone()?));
        // 5) `inheritEnv: false` 先清空环境；非字符串值在这里被忽略。
        if !request.inherit_env {
            command.env_clear();
        }
        for (key, value) in &request.env {
            if let Some(value) = value.as_str() {
                command.env(key, value);
            }
        }
        // 6) 以下闭包在 fork 之后、exec 之前运行，只能调用 async-signal-safe 的函数。
        // SAFETY: only async-signal-safe calls between fork and exec.
        unsafe {
            command.pre_exec(|| {
                // A new session whose controlling terminal is the pty (stdin), so job control and SIGHUP work; default
                // signal handling and an empty mask, as for `exec`.
                //
                // `setsid` 使子进程成为新会话首进程，同时成为新进程组组长（pid == pgid）。
                if libc::setsid() < 0 {
                    return Err(io::Error::last_os_error());
                }
                // 让 pty（fd 0）成为控制终端，作业控制与挂断信号才有作用。
                if libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(io::Error::last_os_error());
                }
                // 恢复默认信号处理：SIGKILL/SIGSTOP 不可设置，跳过。
                for signal in 1..32 {
                    if signal != libc::SIGKILL && signal != libc::SIGSTOP {
                        libc::signal(signal, libc::SIG_DFL);
                    }
                }
                // 清空从守护进程继承的信号屏蔽字。
                let mut mask: libc::sigset_t = std::mem::zeroed();
                libc::sigemptyset(&mut mask);
                libc::sigprocmask(libc::SIG_SETMASK, &mask, std::ptr::null_mut());
                Ok(())
            });
        }
        // 7) 执行程序；失败时以 io::Error 返回。
        let child = command.spawn()?;
        // Only the program holds the terminal now, so reads end once it and its descendants close it.
        drop(slave);
        let master = File::from(master);
        // 8) 非阻塞 master：读写返回 WouldBlock 时由 poll 等待，等待间隙可检查停止标志。
        // SAFETY: fcntl on the descriptor owned above; reads and writes poll instead of blocking.
        unsafe {
            let flags = check(libc::fcntl(master.as_raw_fd(), libc::F_GETFL))?;
            check(libc::fcntl(
                master.as_raw_fd(),
                libc::F_SETFL,
                flags | libc::O_NONBLOCK,
            ))?;
        }
        Ok((master, child))
    }

    /// 等待 `fd` 就绪，最多 100 ms，返回是否已就绪。
    ///
    /// `poll` 被信号中断（`EINTR`）时返回 `Ok(false)`，让调用方回到循环顶部检查停止标志；
    /// 其它错误原样返回。固定 100 ms 超时保证阻塞等待也能定期观察 `stop`。
    fn poll(fd: libc::c_int, events: libc::c_short) -> io::Result<bool> {
        let mut poll = libc::pollfd {
            fd,
            events,
            revents: 0,
        };
        // SAFETY: one valid pollfd.
        let ready = unsafe { libc::poll(&mut poll, 1, 100) };
        if ready < 0 {
            let error = io::Error::last_os_error();
            return if error.kind() == io::ErrorKind::Interrupted {
                Ok(false)
            } else {
                Err(error)
            };
        }
        Ok(ready > 0)
    }

    /// Read until the terminal closes (Linux reports `EIO` once no program holds it), `on_data` returns false, or
    /// `stop` is set.
    ///
    /// 每轮先 `poll` 可读（超时就回去检查 `stop`），再读最多 64 KiB。`on_data` 返回 `false`
    /// （上层通道已关闭）或终端关闭（`Ok(0)` / `EIO`）时结束；`WouldBlock`/`EINTR` 则继续尝试。
    /// 本函数不向外传播错误：读取线程的职责只是把字节送走，读失败等同于终端结束。
    pub fn read(master: &File, stop: &AtomicBool, mut on_data: impl FnMut(&[u8]) -> bool) {
        let mut buffer = vec![0u8; 64 * 1024];
        while !stop.load(Ordering::SeqCst) {
            match poll(master.as_raw_fd(), libc::POLLIN) {
                // poll 超时：回到循环顶部检查 stop。
                Ok(false) => continue,
                Ok(true) => {}
                Err(_) => return,
            }
            match (&*master).read(&mut buffer) {
                // 所有持有 slave 的进程都关闭了终端。
                Ok(0) => return,
                Ok(read) => {
                    if !on_data(&buffer[..read]) {
                        return;
                    }
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                // Linux 在最后一个 slave 关闭后返回 EIO；其它 IO 错误同样按结束处理。
                Err(_) => return,
            }
        }
    }

    /// Write all of `bytes`, waiting while the program does not read its input; false once the terminal is gone.
    ///
    /// master 是非阻塞的：一次 `write` 可能只写一部分，剩余部分继续写；`WouldBlock` 时用
    /// `poll(POLLOUT)` 等最多 100 ms。`stop` 置位、写 0 字节或真实 IO 错误时返回 `false`。
    pub fn write_all(master: &File, mut bytes: &[u8], stop: &AtomicBool) -> bool {
        while !bytes.is_empty() {
            if stop.load(Ordering::SeqCst) {
                return false;
            }
            match (&*master).write(bytes) {
                // 写 0 字节说明终端已关闭。
                Ok(0) => return false,
                Ok(written) => bytes = &bytes[written..],
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    // 程序暂时不读输入：等可写；poll 出错说明终端已不可用。
                    if poll(master.as_raw_fd(), libc::POLLOUT).is_err() {
                        return false;
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => return false,
            }
        }
        true
    }
}

// 单元测试覆盖可脱离真实终端验证的部分：请求解析与默认值、尺寸边界、`target` 截断、
// 输入通道与 EBADF 语义、尺寸校验先于 ioctl，以及非 Unix 平台的降级行为。
// 测试用 `/dev/null` 充当 master，使 ioctl 必然失败，从而能验证错误映射；真实 pty 的
// 端到端行为不在这里（`run` 会真正 fork 进程）。
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    /// A terminal whose master is `/dev/null` on unix: the ioctl calls fail without a real pseudo-terminal.
    #[cfg_attr(not(unix), allow(unused_variables))]
    fn terminal(input: Sender<Vec<u8>>, pid: u32) -> Terminal {
        Terminal {
            input,
            #[cfg(unix)]
            master: Arc::new(std::fs::File::open("/dev/null").expect("open /dev/null")),
            #[cfg(unix)]
            pid,
        }
    }

    fn registered(id: u32, input: Sender<Vec<u8>>) -> Terminals {
        Arc::new(Mutex::new(HashMap::from([(
            id,
            Arc::new(terminal(input, 0)),
        )])))
    }

    fn failed(result: Result<Value, Failure>) -> Failure {
        match result {
            Ok(value) => panic!("expected a failure, got {value}"),
            Err(failure) => failure,
        }
    }

    #[test]
    fn sizes_are_checked_against_the_terminal_bounds() {
        // Both ends of the accepted range, and the cell count reported as a number.
        assert_eq!(size(&json!({ "cols": 1 }), "cols").unwrap(), 1);
        assert_eq!(size(&json!({ "cols": 4096 }), "cols").unwrap(), 4096);
        for bad in [
            json!({ "cols": 0 }),
            json!({ "cols": 4097 }),
            json!({ "cols": 80.5 }),
            json!({ "cols": -1 }),
            json!({ "cols": "80" }),
            json!({ "cols": true }),
            json!({ "cols": null }),
            json!({}),
        ] {
            let failure = failed(size(&bad, "cols").map(|_| json!(null)));
            assert_eq!(failure.code, "EINVAL", "{bad}");
            assert_eq!(failure.message, "cols must be between 1 and 4096");
            assert_eq!(
                failure.to_json(),
                json!({
                    "code": "EINVAL",
                    "message": "cols must be between 1 and 4096",
                })
            );
        }
        // The key is named in the message, so `rows` reports itself.
        assert_eq!(
            failed(size(&json!({ "rows": 0 }), "rows").map(|_| json!(null))).message,
            "rows must be between 1 and 4096"
        );
    }

    #[test]
    fn a_request_defaults_to_the_users_shell_and_inherited_environment() {
        let request =
            PtyRequest::from_json(&json!({ "cwd": "/tmp", "cols": 80, "rows": 24 })).unwrap();
        assert!(
            request.argv.is_none(),
            "no argv means the user's login shell"
        );
        assert_eq!(request.cwd, "/tmp");
        assert!(request.env.is_empty());
        assert!(
            request.inherit_env,
            "the environment is inherited unless asked not to"
        );
        assert_eq!((request.cols, request.rows), (80, 24));
    }

    #[test]
    fn a_request_takes_argv_and_keeps_non_string_environment_values() {
        let request = PtyRequest::from_json(&json!({
            "argv": ["/bin/sh", "-c", "echo hi"],
            "cwd": "/tmp",
            "env": { "TERM": "xterm-256color", "NUMBER": 2, "FLAG": true },
            "inheritEnv": false,
            "cols": 100,
            "rows": 30,
        }))
        .unwrap();
        assert_eq!(request.argv.unwrap(), ["/bin/sh", "-c", "echo hi"]);
        assert_eq!(request.env["TERM"], json!("xterm-256color"));
        // Non-string values stay in the map; the spawn filters them out when it builds the environment.
        assert_eq!(request.env["NUMBER"], json!(2));
        assert_eq!(request.env["FLAG"], json!(true));
        assert!(!request.inherit_env);
        assert_eq!((request.cols, request.rows), (100, 30));
        // A non-boolean `inheritEnv` falls back to the default.
        let request = PtyRequest::from_json(&json!({
            "cwd": "/tmp", "cols": 80, "rows": 24, "inheritEnv": "no"
        }))
        .unwrap();
        assert!(request.inherit_env);
    }

    #[test]
    fn argv_must_be_a_non_empty_array_of_strings() {
        for argv in [
            json!([]),
            json!("sh"),
            json!(["sh", 5]),
            json!({ "0": "sh" }),
            json!(true),
        ] {
            let failure = failed(
                PtyRequest::from_json(
                    &json!({ "argv": argv, "cwd": "/tmp", "cols": 80, "rows": 24 }),
                )
                .map(|_| json!(null)),
            );
            assert_eq!(failure.code, "EINVAL");
            assert_eq!(failure.message, "argv must be a non-empty array of strings");
        }
    }

    #[test]
    fn a_request_needs_a_working_directory() {
        for missing in [
            json!({ "cols": 80, "rows": 24 }),
            json!({ "cwd": 5, "cols": 80, "rows": 24 }),
        ] {
            let failure = failed(PtyRequest::from_json(&missing).map(|_| json!(null)));
            assert_eq!(failure.code, "EINVAL");
            assert_eq!(failure.message, "pty needs cwd");
        }
        // The working directory is checked before the terminal size.
        let failure =
            failed(PtyRequest::from_json(&json!({ "cols": 0, "rows": 0 })).map(|_| json!(null)));
        assert_eq!(failure.message, "pty needs cwd");
    }

    #[test]
    fn a_registered_terminal_is_found_and_a_write_reaches_its_channel() {
        let (input, receiver) = mpsc::channel();
        let terminals = registered(7, input);
        let found = find(&terminals, &json!({ "target": 7 })).unwrap();
        assert!(
            Arc::ptr_eq(&found, &terminals.lock().unwrap()[&7]),
            "the map's terminal, not a copy"
        );
        assert_eq!(
            write(&terminals, &json!({ "target": 7 }), b"hi".to_vec()).unwrap(),
            json!({})
        );
        assert_eq!(receiver.try_recv().unwrap(), b"hi");
    }

    #[test]
    fn writing_to_a_terminal_whose_program_ended_reports_ebadf() {
        let (input, receiver) = mpsc::channel();
        let terminals = registered(1, input);
        drop(receiver);
        let failure = failed(write(&terminals, &json!({ "target": 1 }), b"x".to_vec()));
        assert_eq!(failure.code, "EBADF");
        assert_eq!(failure.message, "the terminal has ended");
    }

    #[test]
    fn console_operations_reject_missing_and_unknown_targets() {
        let (input, _receiver) = mpsc::channel();
        let terminals = registered(1, input);
        for json in [
            json!({}),
            json!({ "target": null }),
            json!({ "target": "1" }),
            json!({ "target": -1 }),
        ] {
            let failure = failed(write(&terminals, &json, Vec::new()));
            assert_eq!(failure.code, "EINVAL", "{json}");
            assert_eq!(failure.message, "missing field target");
        }
        let failure = failed(write(&terminals, &json!({ "target": 999 }), Vec::new()));
        assert_eq!(failure.code, "EBADF");
        assert_eq!(failure.message, "no running terminal with this id");
        // `resize` and `foreground` look the target up the same way, before they validate anything else.
        let failure = failed(resize(&terminals, &json!({})));
        assert_eq!(failure.message, "missing field target");
        let failure = failed(resize(&terminals, &json!({ "target": 999 })));
        assert_eq!(failure.message, "no running terminal with this id");
        let failure = failed(foreground(&terminals, &json!({ "target": 999 })));
        assert_eq!(failure.code, "EBADF");
        assert_eq!(failure.message, "no running terminal with this id");
    }

    #[test]
    fn the_target_is_truncated_to_the_wide_of_a_terminal_id() {
        // `target` arrives as a JSON number (u64) and addresses a map keyed by u32, as the protocol specifies.
        let (input, receiver) = mpsc::channel();
        let terminals = registered(7, input);
        let target = (1u64 << 32) + 7;
        assert_eq!(
            write(&terminals, &json!({ "target": target }), b"x".to_vec()).unwrap(),
            json!({})
        );
        assert_eq!(receiver.try_recv().unwrap(), b"x");
    }

    #[cfg(unix)]
    #[test]
    fn resize_reports_ioctl_on_pty_and_foreground_reports_tcgetpgrp() {
        let (input, _receiver) = mpsc::channel();
        let terminals = registered(1, input);
        // A master that is not a terminal (here `/dev/null`): the ioctl fails with the fixed syscall and path names.
        let failure = failed(resize(
            &terminals,
            &json!({ "target": 1, "cols": 80, "rows": 24 }),
        ));
        let json = failure.to_json();
        assert_eq!(json["syscall"], "ioctl");
        assert_eq!(json["path"], "pty");
        assert!(
            failure.message.ends_with(", ioctl 'pty'"),
            "{}",
            failure.message
        );
        assert_eq!(json["message"], failure.message);
        let failure = failed(foreground(&terminals, &json!({ "target": 1 })));
        let json = failure.to_json();
        assert_eq!(json["syscall"], "tcgetpgrp");
        assert_eq!(json["path"], "pty");
        assert!(
            failure.message.ends_with(", tcgetpgrp 'pty'"),
            "{}",
            failure.message
        );
        // The size is validated before the ioctl, so a bad size never reaches the terminal.
        let failure = failed(resize(
            &terminals,
            &json!({ "target": 1, "cols": 0, "rows": 24 }),
        ));
        assert_eq!(failure.code, "EINVAL");
        assert_eq!(failure.message, "cols must be between 1 and 4096");
    }

    #[cfg(not(unix))]
    #[test]
    fn resize_and_foreground_do_nothing_without_a_terminal_system() {
        let (input, _receiver) = mpsc::channel();
        let terminals = registered(1, input);
        assert_eq!(
            resize(&terminals, &json!({ "target": 1, "cols": 80, "rows": 24 })).unwrap(),
            json!({})
        );
        assert_eq!(
            foreground(&terminals, &json!({ "target": 1 })).unwrap(),
            json!({ "foreground": null })
        );
    }

    #[cfg(not(unix))]
    #[test]
    fn terminals_are_not_supported_on_this_system_yet() {
        let request =
            PtyRequest::from_json(&json!({ "cwd": "/", "cols": 80, "rows": 24 })).unwrap();
        let output = Arc::new(Output::default());
        let failure = failed(run(
            1,
            request,
            &output,
            &Control::default(),
            &Groups::default(),
            &Terminals::default(),
        ));
        assert_eq!(failure.code, "ENOTSUP");
        assert_eq!(
            failure.message,
            "Terminals are not supported on this system yet"
        );
    }

    #[test]
    fn the_terminal_constants_match_the_spec() {
        assert_eq!(MAX_CELLS, 4096);
        #[cfg(unix)]
        assert_eq!(EXIT_GRACE, std::time::Duration::from_millis(100));
    }
}
