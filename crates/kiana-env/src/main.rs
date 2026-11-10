//! 对应 packages/env/daemon（原生 Rust 二进制 kiana-env）：远端执行守护进程。
//!
//! 以 `kiana-env serve --token <hex>` 启动（通常经 `ssh`），在 stdin/stdout 上按 docs/protocol.md 的帧协议
//! 为 Kiana Durable 宿主提供本机的执行环境。
//!
//! TS 源：packages/env/daemon/src/main.rs（原 daemon 本身就是 Rust；逐字移植，签名不变）。
//!
//! 模块树（每文件对上上游一个文件，`sys` 为平台层）：
//! - `frame` / `errors` / `output`：线协议、错误 JSON、stdout 单一写线程；
//! - `fs` / `list`：文件操作与 `listFiles`；
//! - `exec` / `pty` / `watch`：命令、伪终端、文件监视；
//! - `decode` / `scan` / `window`：WHATWG 解码、行扫描、输出窗口；
//! - `sys`：平台层（unix / windows）。
//!
//! # 这个文件解决什么问题
//! `kiana-env` 是运行在远端机器上的执行守护进程（daemon：长期运行、由客户端按需拉起的后台
//! 进程）。它把本机的文件读写、目录列举、命令执行、伪终端和文件监视能力，封装成 stdin/stdout
//! 上的一条帧协议，供 Kiana Durable 宿主跨 `ssh` 调用。本文件是二进制入口：进程启动、握手、
//! 读帧、分发请求、并发调度、取消与断线清理都在这里；具体操作实现在各子模块。
//!
//! # 在系统中的位置
//! 上游：Kiana Durable 宿主 / kiana-env 客户端通过 `ssh` 启动本进程并写 stdin。
//! 下游：`frame`（线格式）、`output`（stdout 唯一写线程）、`exec`/`pty`/`watch`（长任务）、
//! `fs`/`list`/`scan`（文件操作）、`sys`（平台差异）。
//! 对应 TS：packages/env/daemon（原 daemon 本身就是 Rust，本文件逐字移植、签名不变）。
//!
//! # 进程启动与握手（对应 docs/protocol.md 的 Sync 一节）
//! 1. `main` 解析参数：`serve --token <hex>` 启动服务，`--version` 打印版本，其余打印 usage
//!    并以退出码 2 结束。
//! 2. `serve` 先向 stdout 打印一行 `KIANA-ENV <token>` 并 flush。SSH 登录脚本可能在它之前
//!    往 stdout 打印内容，客户端丢弃这一行之前的字节，因此它是“帧从这里开始”的同步标记；
//!    token 只是从命令行原样回显，本进程不拿它做鉴权。
//! 3. 握手之后 stdout 只允许 `Output` 写线程写帧；诊断信息一律走 stderr。
//!
//! # 一次请求的执行流程
//! 1. `serve_frames` 在唯一的读线程里循环 `frame::read_frame`，只处理 REQUEST 与 CANCEL，
//!    其余帧类型读取后忽略。
//! 2. 除 `ptyWrite`/`ptyResize`/`ptyForeground` 外，每个 REQUEST 先登记一个 `Control`
//!    （取消状态），再按操作类型调度：
//!    - `exec`/`watch`/`pty`：各开一个 16 MiB 栈的专用线程，因为它们可能长期运行；
//!    - `ptyWrite`/`ptyResize`/`ptyForeground`：不登记取消状态，就在读线程内按到达顺序
//!      处理，避免两次写互换；
//!    - 其余操作：投递到 16 个 worker 线程的共享队列；`writeChunk` 额外按句柄串行化。
//! 3. `Server::dispatch` 返回 `(JSON, payload)` 或 `Failure`，`respond` 把它包装成
//!    RESULT/ERROR 帧，从 `controls` 注销该请求，再决定走 control 队列还是 bulk 队列。
//! 4. `Output` 的写线程是 stdout 的唯一写者：control 帧（小回复、非 exec/pty 的错误、
//!    ping、watch 事件）优先于 bulk 帧（exec/pty 输出与其终局帧、大于 64 KiB 的回复）
//!    出队，因此排队中的 ping 和小回复不会排在大块数据后面。
//!
//! # 并发模型与生命周期
//! - 共享状态集中在 `Server`，用 `Mutex`/原子量保护，并以 `Arc` 在多个线程间共享。
//! - 请求 id 由客户端分配，`respond` 按 id 注销取消状态。同一 id 重复登记会覆盖旧控制块，
//!   随后先完成的一方会按 id 注销另一方的登记，因此客户端必须保证同一连接内 id 唯一。
//! - 心跳线程每 5 秒发一个 ping；超过 30 秒没从 stdin 读到任何字节，就杀掉所有命令进程组
//!   并调用 `process::exit(0)`。
//! - stdin 结束或帧格式错误时 `serve_frames` 返回：先 `kill_all` 杀掉所有命令进程组，再关闭
//!   `Output`、join 写线程，最后按结果返回退出码（干净 EOF 为 0，I/O/协议错误为 1）。
//!
//! # 边界与失败模式
//! - 单帧最大 16 MiB（见 `frame::MAX_FRAME`）。读帧错误会结束整个连接，而不是只回一个错误；
//!   JSON 解析失败时帧结构仍然完整，`dispatch` 收到 `Value::Null` 并回 `EINVAL`。
//! - 每个连接最多 4096 个句柄，超出回 `EMFILE`；句柄只在当前 daemon 进程内有效，连接结束
//!   即失效，新 daemon 不认识旧 id。
//! - `exec`/`pty` 的终局帧走 bulk 队列，以保证排在命令自己的输出事件之后；`watch` 的事件
//!   走 control 队列，其终局帧（空对象或错误）也走 control 队列。
//! - 断线清理靠 stdin EOF 或 30 秒静默检测；代码中没有信号处理逻辑。
//!
//! # 阅读顺序
//! 先读 `main` → `serve` → `serve_frames` → `Server::dispatch`，再按操作跳转到
//! `exec`/`pty`/`watch`/`fs`/`list`/`scan`；线格式细节见 `frame`，输出排队规则见 `output`，
//! 错误 JSON 见 `errors`。
mod decode;
mod errors;
mod exec;
mod frame;
mod fs;
mod list;
mod output;
mod pty;
mod scan;
mod sys;
mod watch;
mod window;

use errors::Failure;
use exec::Control;
use frame::{CANCEL, ERROR, Frame, PING, REQUEST, RESULT};
use output::Output;
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::env;
use std::fs::{File, ReadDir};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// 线协议版本，`hello` 回复原样上报；协议文档把当前版本定为 1。
const PROTOCOL: u64 = 1;
/// Operations added after protocol 1's first release; clients check `hello.features` before using them.
///
/// `listFiles` 是协议 1 发布后追加的操作：老 daemon 的 `hello` 不含 `features` 字段，也不认识
/// 这个 op。客户端必须先看 `features` 再决定是否调用，daemon 侧不强制校验协商结果。
const FEATURES: &[&str] = &["listFiles"];
/// The npm package version this daemon ships with (build.rs).
///
/// `build.rs` 把 `KIANA_ENV_VERSION`（发布脚本从 `packages/env/package.json` 注入）编进来；
/// 未注入时回退到移植时记录的 npm 版本字面量，**不**回退 `CARGO_PKG_VERSION`。因此
/// `kiana-env --version` 与 `hello.version` 报告的是 npm 包版本，而不是 workspace 的 crate 版本。
const VERSION: &str = env!("KIANA_ENV_VERSION");
/// 心跳线程发送 `PING` 的间隔（5 秒）。客户端也按同样的节奏发 ping；daemon 不解析收到的
/// `PING` 帧，但读到的字节会刷新 `Seen` 记录的活跃时间。
const PING_INTERVAL: Duration = Duration::from_secs(5);
/// 静默上限（毫秒）：超过这段时间没有从 stdin 收到任何字节，就认为客户端已失联，
/// 杀掉所有命令进程组并退出（见 `serve` 中的心跳线程）。
const SILENCE_LIMIT_MS: u64 = 30_000;
/// `scanLines` 每次定位读取的块大小（64 KiB）。它同时是扫描的内存上界和取消检查的粒度：
/// 每读完一块才检查一次 `Control::aborted`。
const SCAN_CHUNK: usize = 64 * 1024;
/// Threads for file operations; commands run on their own threads.
/// 固定 16 个 worker 处理文件操作；`exec`/`pty`/`watch` 不经过这个池，而是各开专用线程
/// （调度规则见 `serve_frames` 与 docs/protocol.md 的 Scheduling 一节）。
const WORKERS: usize = 16;
/// Open handles per connection, beyond which opening fails with `EMFILE`.
/// 上限按连接（即每个 daemon 进程）计算；`Server::insert` 在插入前检查，达到上限时返回
/// `EMFILE: too many open files`。
const MAX_HANDLES: usize = 4096;
/// Replies with larger payloads queue behind command output instead of going first.
/// 阈值只看 payload 字节数（不含 JSON）。`exec`/`pty` 的终局帧不受大小影响，总是走 bulk
/// 队列，以保证排在它们自己的输出事件之后（见 `respond`）。
const CONTROL_PAYLOAD: usize = 64 * 1024;

/// 一个已打开文件的句柄。
///
/// 句柄由 `Arc<FileHandle>` 共享：`close` 只是从 `Server.handles` 删除映射，已经复制出
/// `Arc` 的请求可以继续使用该文件，直到最后一个引用释放才真正关闭。
struct FileHandle {
    /// 打开的文件。读取和写入都作用于同一个文件描述符（共享文件偏移），
    /// 因此需要固定位置的读由 `fs::pread` 完成。
    file: File,
    /// 打开时的路径，只用于构造 Node 风格的错误消息（`syscall 'path'`）。
    path: String,
    /// A failed chunk write poisons a write handle, so later chunks cannot leave a gap.
    /// 一旦置位就不会复位：后续 `writeChunk` 直接回 `EBADF`，避免失败块之后的块继续写入，
    /// 在文件中留下一个客户端无法察觉的缺口。用 `SeqCst` 让其他 worker 线程看到置位。
    failed: AtomicBool,
    /// Chunk writes run one at a time in arrival order, though the client sends several at once.
    /// 只串行 `writeChunk`；`pread`/`fstat`/`close` 等操作不经过这个队列，可能与写并发发生。
    serial: Mutex<Serial>,
}

/// 一个文件句柄上的待写任务队列，由 `run_in_order` 驱动。
///
/// `jobs` 按请求到达读线程的顺序排队；`running` 表示是否已经有一个 drain 任务在 worker
/// 池里。drain 任务在队列清空后把 `running` 置回 `false` 并退出，因此不需要每个 chunk
/// 都占用一个池任务。
///
/// 不变量（在锁内成立）：`running == false` 时 `jobs` 必为空。因此新任务看到
/// `running == false` 才需要自己投递 drain 任务，其余情况只需入队。
#[derive(Default)]
struct Serial {
    jobs: VecDeque<Job>,
    running: bool,
}

/// 一个已打开目录的游标。
///
/// `ReadDir` 每次 `next()` 都会推进内部位置，所以分页读取必须串行；`Mutex` 由
/// `Handle::Dir` 提供。`path` 用于拼接条目路径和构造错误消息。
struct DirHandle {
    entries: ReadDir,
    path: String,
}

/// `handles` 映射中保存的两类句柄。
///
/// 文件句柄可以并发读（`Arc<FileHandle>` 之间共享 `&File`）；目录句柄必须独占推进游标
/// （`Arc<Mutex<DirHandle>>`）。操作与句柄类型不匹配时返回 `EBADF`。
enum Handle {
    File(Arc<FileHandle>),
    Dir(Arc<Mutex<DirHandle>>),
}

/// 投递给 worker 池的一次性任务：请求只执行一次，并且必须能跨线程移动。
type Job = Box<dyn FnOnce() + Send>;

/// 守护进程的共享状态，用 `Arc` 在读线程、worker 池、长任务线程和心跳线程之间共享。
///
/// 锁的范围（本文件中的获取方式，彼此不嵌套）：
/// 1. `handles` 只保护 id 到句柄的映射，取出 `Arc` 后立即释放；
/// 2. 文件句柄的 `serial` 只保护该文件的 chunk 写队列；
/// 3. 目录的 `Mutex<DirHandle>` 只在一次 `readdir` 分页期间持有。
/// `controls`、`groups`、`terminals` 各自独立，持有其中任何一个时都不会再去拿句柄锁。
struct Server {
    /// stdout 的唯一写队列；所有回复和事件都经它排队，不能直接写 stdout。
    output: Arc<Output>,
    /// 远端临时目录；`exec` 的 spill 文件在它下面创建 `tmp-*` 目录。
    tmpdir: String,
    /// 当前连接打开的句柄；`close` 或连接结束即失效，句柄不跨连接复用。
    handles: Mutex<HashMap<u64, Handle>>,
    /// 下一个句柄 id，从 1 开始。`fetch_add` 保证唯一性，`Relaxed` 足够，
    /// 因为 id 只需要唯一，不需要与其他内存操作建立顺序。
    next_handle: AtomicU64,
    /// 运行中请求的取消状态，键是请求 id。任务开始前登记，`respond` 发送终局帧前注销。
    controls: Mutex<HashMap<u32, Arc<Control>>>,
    /// 本进程启动且尚未结束的命令进程组；断线或静默超时时由 `kill_all` 统一杀掉。
    groups: exec::Groups,
    /// 运行中的伪终端，键是 `pty` 请求 id，供 `ptyWrite`/`ptyResize`/`ptyForeground` 寻址。
    terminals: pty::Terminals,
}

/// 当前 Unix epoch 的墙钟毫秒时间戳。
///
/// 用于 `Seen` 的活跃时间和心跳的静默判断。`SystemTime` 不是单调时钟：系统时间被向前
/// 调整可能让静默判断提前触发；向回调整时 `saturating_sub` 避免下溢。时间早于 epoch
/// 时 `duration_since` 失败，返回 0。
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

/// 取必填的字符串字段。
///
/// 字段缺失、`json` 不是对象，或值不是字符串，都返回 `EINVAL`，消息为
/// `missing field <key>`，让客户端能直接定位坏请求。
fn field<'a>(json: &'a Value, key: &str) -> Result<&'a str, Failure> {
    json[key]
        .as_str()
        .ok_or_else(|| Failure::new("EINVAL", format!("missing field {key}")))
}

/// 取必填的非负整数字段（`u64`）。
///
/// 负数、小数、字符串、缺失都返回 `EINVAL`，消息为 `missing or invalid field <key>`。
/// 这是有意的严格解析：例如句柄 id 不接受 `"3"` 这样的字符串。
fn number(json: &Value, key: &str) -> Result<u64, Failure> {
    json[key]
        .as_u64()
        .ok_or_else(|| Failure::new("EINVAL", format!("missing or invalid field {key}")))
}

/// 取可选布尔标志；字段缺失或不是布尔值一律视为 `false`，不报错。
///
/// 只有 JSON 的 `true` 才算真：任何非布尔值（包括字符串 `"yes"`）都按假处理
/// （测试 `request_helpers_report_the_field_names` 锁定）。依据是 `Value::as_bool()`
/// 只对 JSON 布尔值返回 `Some`，其他 JSON 类型一律返回 `None`。
fn flag(json: &Value, key: &str) -> bool {
    json[key].as_bool().unwrap_or(false)
}

impl Server {
    /// 从请求 JSON 的 `handle` 字段取出文件句柄。
    ///
    /// 错误：
    /// - `handle` 缺失或不是 `u64`：`EINVAL`（由 `number` 产生）；
    /// - id 不存在：`EBADF` "unknown handle"；
    /// - id 对应目录句柄：`EBADF` "not a file handle"。
    ///
    /// 返回的 `Arc` 让调用方脱离 `handles` 锁继续操作；即使随后 `close` 删除了映射，
    /// 文件也会活到最后一个 `Arc` 释放。
    fn file(&self, json: &Value) -> Result<Arc<FileHandle>, Failure> {
        match self.handles.lock().unwrap().get(&number(json, "handle")?) {
            Some(Handle::File(file)) => Ok(file.clone()),
            Some(Handle::Dir(_)) => Err(Failure::new("EBADF", "not a file handle")),
            None => Err(Failure::new("EBADF", "unknown handle")),
        }
    }

    /// 把一个句柄登记到当前连接的表里，返回新 id。
    ///
    /// - 达到 `MAX_HANDLES`（4096）后不再插入，返回 `EMFILE`；检查与插入在同一把锁内，
    ///   并发登记不会超过上限。
    /// - id 由 `next_handle` 单调递增分配，不复用，旧 id 不会指向后来的新资源。
    fn insert(&self, handle: Handle) -> Result<u64, Failure> {
        let mut handles = self.handles.lock().unwrap();
        if handles.len() >= MAX_HANDLES {
            return Err(Failure::new("EMFILE", "EMFILE: too many open files"));
        }
        let id = self.next_handle.fetch_add(1, Ordering::Relaxed);
        handles.insert(id, handle);
        Ok(id)
    }

    /// 用 `FileHandle` 包装打开的文件并登记；`path` 只用于后续错误消息。
    fn insert_file(&self, file: File, path: &str) -> Result<u64, Failure> {
        self.insert(Handle::File(Arc::new(FileHandle {
            file,
            path: path.to_string(),
            failed: AtomicBool::new(false),
            serial: Mutex::new(Serial::default()),
        })))
    }

    /// 执行一个请求，返回 `(JSON, payload)`；错误以 `Failure` 返回，由 `respond` 转成 ERROR 帧。
    ///
    /// 输入：`id` 是请求 id（只有 `exec`/`watch`/`pty` 用它标记事件帧），`json` 是请求 JSON，
    /// `payload` 是帧尾原始字节（`write` 的文件内容或 `writeChunk` 的数据；`ptyWrite` 在
    /// `serve_frames` 里直接处理，不会进到这里），`control` 是该请求的取消状态。
    /// `json` 为 `Null` 表示帧里的 JSON 解析失败（`frame::read_frame` 的选择），统一回
    /// `EINVAL "invalid JSON"`；未知 `op` 回 `EINVAL "unknown operation ..."`。
    ///
    /// 本函数是阻塞的：`dispatch` 返回时操作才结束。`exec`/`pty`/`watch` 在返回前已经把
    /// 过程中产生的事件投递到 `Output`，`respond` 再决定终局帧的排队方式。
    fn dispatch(
        &self,
        id: u32,
        json: &Value,
        payload: Vec<u8>,
        control: &Control,
    ) -> Result<(Value, Vec<u8>), Failure> {
        // 解析失败时 frame 层给的是 Null；客户端也可能显式发 null，这里一并按坏 JSON 处理。
        if json.is_null() {
            return Err(Failure::new("EINVAL", "invalid JSON"));
        }
        // 缺少 op 或 op 不是字符串时得到空串，最终落到下面的未知操作分支。
        let op = json["op"].as_str().unwrap_or_default();
        // 大多数操作的回复只有 JSON，没有 payload；用这个闭包统一补空 Vec。
        let plain = |value: Result<Value, Failure>| value.map(|value| (value, Vec::new()));
        match op {
            // `hello` 是协议的第一个请求：客户端据此得知协议版本、平台、路径分隔符、
            // 远端 home/tmpdir/cwd 与可用特性，再决定后续路径解析和能否使用 `listFiles`。
            "hello" => plain(Ok(json!({
                "protocol": PROTOCOL,
                "version": VERSION,
                "os": env::consts::OS,
                "arch": env::consts::ARCH,
                "home": sys::home(),
                "separator": sys::PATH_SEPARATOR,
                "tmpdir": self.tmpdir,
                "cwd": sys::cwd(),
                "driveCwds": sys::drive_cwds(),
                "pid": std::process::id(),
                "features": FEATURES,
            }))),
            "lstat" => plain(fs::lstat(field(json, "path")?)),
            "realpath" => plain(fs::realpath(field(json, "path")?)),
            // 语义等同 Node 的 writeFile/appendFile：append=false 时截断后重写，true 时追加。
            // parents 缺省为 true（自动创建父目录），因此不能用 `flag`（它默认 false）。
            "write" => {
                let path = field(json, "path")?;
                let parents = json["parents"].as_bool().unwrap_or(true);
                let file = fs::write(path, flag(json, "append"), parents, &payload)?;
                // 不 keep 时 file 在此被丢弃并关闭，回复空对象。
                if !flag(json, "keep") {
                    return plain(Ok(json!({})));
                }
                // keep=true 时把已写完的文件留给 writeChunk。若句柄表已满，insert_file 返回
                // EMFILE，但内容已经写入（写不可回滚），客户端需同时处理“写成功但没拿到句柄”。
                plain(Ok(json!({ "handle": self.insert_file(file, path)? })))
            }
            // 追加到 write 返回的句柄。failed 是粘性毒化标记：某次写失败后，后续块不再尝试，
            // 直接返回 EBADF，避免失败块之后的内容被写入而形成难以发现的缺口。
            "writeChunk" => {
                let handle = self.file(json)?;
                if handle.failed.load(Ordering::SeqCst) {
                    return Err(Failure::new("EBADF", "an earlier write failed"));
                }
                (&handle.file).write_all(&payload).map_err(|error| {
                    handle.failed.store(true, Ordering::SeqCst);
                    Failure::io(&error, "write", &handle.path)
                })?;
                plain(Ok(json!({})))
            }
            "truncate" => plain(fs::truncate(field(json, "path")?, number(json, "size")?)),
            "fsync" => plain(fs::fsync(field(json, "path")?)),
            "rename" => plain(fs::rename(field(json, "path")?, field(json, "to")?)),
            "mkdir" => plain(fs::mkdir(field(json, "path")?, flag(json, "recursive"))),
            "rm" => plain(fs::rm(
                field(json, "path")?,
                flag(json, "recursive"),
                flag(json, "force"),
            )),
            "mkdtemp" => plain(fs::mkdtemp(field(json, "path")?)),
            "open" => {
                let path = field(json, "path")?;
                if json["mode"].as_str() == Some("read") {
                    // Node's `open(path, "r")`: any kind of file, blocking; reads report what the file is.
                    // mode "read" 打开任何类型的文件，读操作可能阻塞（例如 FIFO 等到写端），
                    // 因此这个分支会占住一个 worker 线程。即使 fstat 失败也仍返回 handle，
                    // 并附 statError，让客户端看到“文件已打开但无法取元数据”。
                    let file = sys::open(path, sys::OpenMode::Read)
                        .map_err(|error| Failure::io(&error, "open", path))?;
                    let stat = match file.metadata() {
                        Ok(metadata) => json!({ "stat": fs::info(path, &metadata) }),
                        Err(error) => {
                            json!({ "statError": Failure::io(&error, "fstat", path).to_json() })
                        }
                    };
                    let mut result = stat;
                    result["handle"] = json!(self.insert_file(file, path)?);
                    return plain(Ok(result));
                }
                // 默认分支是 openBinaryReader：非阻塞打开、只接受普通文件，返回 info 供客户端
                // 判断文件种类；noFollow 拒绝最终路径是符号链接的情况。
                let (file, info) = fs::open_reader(path, flag(json, "noFollow"))?;
                plain(Ok(
                    json!({ "handle": self.insert_file(file, path)?, "info": info }),
                ))
            }
            "pread" => {
                let handle = self.file(json)?;
                // 限制单次响应 payload，保证加上 JSON 帧头后不超过 MAX_FRAME；客户端需分批读。
                let length = (number(json, "length")? as usize).min(frame::MAX_PAYLOAD);
                // offset 缺失或不是 u64 时，从文件当前偏移读一次（会推进共享偏移）；
                // 给出 offset 时用定位读，不改变文件偏移。
                let bytes = match json["offset"].as_u64() {
                    Some(offset) => fs::pread(&handle.file, &handle.path, offset, length)?,
                    None => fs::read(&handle.file, &handle.path, length)?,
                };
                Ok((json!({}), bytes))
            }
            "fstat" => {
                let handle = self.file(json)?;
                let metadata = handle
                    .file
                    .metadata()
                    .map_err(|error| Failure::io(&error, "fstat", &handle.path))?;
                plain(Ok(fs::info(&handle.path, &metadata)))
            }
            // 扫描在块边界检查取消，取消时返回 aborted 而不是部分结果。
            "scanLines" => {
                let handle = self.file(json)?;
                plain(scan_lines(&handle.file, &handle.path, json, control))
            }
            // 目录句柄持有系统迭代器；条目由后续 readdir 分页取出。
            "opendir" => {
                let path = field(json, "path")?;
                let entries =
                    sys::read_dir(path).map_err(|error| Failure::io(&error, "opendir", path))?;
                let handle = self.insert(Handle::Dir(Arc::new(Mutex::new(DirHandle {
                    entries,
                    path: path.to_string(),
                }))))?;
                plain(Ok(json!({ "handle": handle })))
            }
            "readdir" => plain(self.read_dir(json)),
            // features 只是客户端侧的协商；daemon 收到该 op 就执行。取消在 list 内部按条目检查。
            "listFiles" => list::run(json, &|| control.aborted.load(Ordering::SeqCst)),
            // 未知句柄也当成功（测试 `close_of_an_unknown_handle_succeeds` 锁定）。删除映射后，
            // 正在进行中的读仍持有 Arc，可以继续到本次操作结束。
            "close" => {
                self.handles
                    .lock()
                    .unwrap()
                    .remove(&number(json, "handle")?);
                plain(Ok(json!({})))
            }
            // 三个长任务：在调用线程上阻塞到请求结束，过程中产生的事件直接投递到 output；
            // `id` 用于给这些事件帧标记所属请求。
            "exec" => plain(exec::run(
                id,
                exec::ExecRequest::from_json(json)?,
                &self.tmpdir,
                &self.output,
                control,
                &self.groups,
            )),
            "watch" => plain(watch::run(id, json, &self.output, control)),
            "pty" => plain(pty::run(
                id,
                pty::PtyRequest::from_json(json)?,
                &self.output,
                control,
                &self.groups,
                &self.terminals,
            )),
            // 未知 op（含空 op）返回 EINVAL，消息带上 op 名称供客户端诊断。
            _ => Err(Failure::new("EINVAL", format!("unknown operation {op}"))),
        }
    }

    /// 从目录句柄读取最多 `max` 项，返回 `{ entries, done }`。
    ///
    /// `max` 至少按 1 处理。`done` 只有在本页内真的读到迭代器末尾才为 `true`；若恰好取满
    /// `max` 项而没试到 `None`，`done` 仍为 `false`，客户端需要再取一页。
    ///
    /// 锁的范围：先用 `handles` 锁取出 `Arc` 后立刻释放，再锁该目录自己的 `Mutex`，
    /// 因此一个慢目录不会阻塞其他句柄的请求；同一目录的并发分页会串行化，防止两个请求
    /// 交错消费同一个 `ReadDir` 游标。单个条目的 `lstat` 失败会作为该条目的 `error`
    /// 字段返回（见 `fs::dir_entry`），只有目录迭代器本身出错才让整页失败。
    fn read_dir(&self, json: &Value) -> Result<Value, Failure> {
        // max=0 会被抬到 1；这样循环至少尝试一次 next()，能正确报告空目录的 done。
        let max = number(json, "max")?.max(1) as usize;
        // Lock only this directory, so other requests are not held up by a slow listing.
        let dir = match self.handles.lock().unwrap().get(&number(json, "handle")?) {
            Some(Handle::Dir(dir)) => dir.clone(),
            _ => return Err(Failure::new("EBADF", "not a directory handle")),
        };
        let mut dir = dir.lock().unwrap();
        let mut entries = Vec::new();
        let mut done = false;
        while entries.len() < max {
            match dir.entries.next() {
                None => {
                    done = true;
                    break;
                }
                Some(Err(error)) => return Err(Failure::io(&error, "readdir", &dir.path)),
                Some(Ok(entry)) => entries.push(fs::dir_entry(&dir.path, &entry.file_name())),
            }
        }
        Ok(json!({ "entries": entries, "done": done }))
    }
}

/// 扫描文件的行区间 `[startLine, endLine)`，返回 `scan::LineScanner` 的统计 JSON。
///
/// - `startLine` 必填 `u64`；`endLine` 可省略，给出时必须大于 `startLine`，否则回
///   `EINVAL "Invalid line range"`。非 `u64` 的 `endLine` 视为未给出（测试
///   `events_carry_the_request_id_and_scan_lines_rejects_empty_ranges` 锁定该行为）。
/// - 按 `SCAN_CHUNK`（64 KiB）分块做定位读，内存占用与文件大小无关。
/// - 每块之间检查取消标志；被取消返回 `aborted`，不会返回部分统计。
/// - `endLine` 超过文件实际行数时，`LineScanner::finish` 把结束位置收在文件末尾。
///
/// 错误：范围非法为 `EINVAL`，读取失败为 Node 风格的 I/O 错误（`syscall` 为 `read`），
/// 均通过 `Failure` 返回给 `respond`。
fn scan_lines(file: &File, path: &str, json: &Value, control: &Control) -> Result<Value, Failure> {
    // 先校验范围再碰文件：非法请求不产生 I/O。endLine 只有是 u64 时才参与比较。
    let start_line = number(json, "startLine")?;
    let end_line = json["endLine"].as_u64();
    if end_line.is_some_and(|end| end <= start_line) {
        return Err(Failure::new("EINVAL", "Invalid line range"));
    }
    let mut scanner = scan::LineScanner::new(start_line, end_line);
    // 复用同一块缓冲区；文件位置由 position 显式推进，不依赖文件描述符的共享偏移。
    let mut buffer = vec![0u8; SCAN_CHUNK];
    let mut position = 0u64;
    loop {
        // 取消只在块边界检查：正在进行的单次 read_at 要等它返回后才生效，最坏多读一块。
        if control.aborted.load(Ordering::SeqCst) {
            return Err(Failure::new("aborted", "aborted"));
        }
        let read = sys::read_at(file, &mut buffer, position)
            .map_err(|error| Failure::io(&error, "read", path))?;
        if read == 0 {
            // EOF 才收尾；finish 会补上末行结束位置和解码器的尾部字节。
            return Ok(scanner.finish());
        }
        scanner.push(&buffer[..read]);
        position += read as u64;
    }
}

/// 杀掉本进程记录的所有命令进程组，用于连接结束或静默超时。
///
/// `exec::kill_group` 在 Unix 上向进程组发 `SIGKILL`（组不存在时退化为杀单个进程），
/// 在 Windows 上调用 `taskkill /F /T`。这里只负责发信号，不等待进程退出；各命令线程在
/// 收尾时会自行从 `groups` 移除自己的 pid，因此遍历后不修改集合。
fn kill_all(groups: &exec::Groups) {
    for pid in groups.lock().unwrap().iter() {
        exec::kill_group(*pid);
    }
}

/// Stdin that records when bytes last arrived, so a large frame arriving slowly counts as a live client.
///
/// 心跳线程无法直接知道读线程何时收到客户端数据，因此每次 `read` 返回正数字节数时更新
/// 共享的 `last_seen`（Unix 毫秒）。一个大帧分多次到达时每一段都会刷新，所以“慢但持续”
/// 的传输不会被误判为失联。
struct Seen<R> {
    /// 被包装的输入源，实际是 `io::stdin().lock()`。
    inner: R,
    /// 最近一次读到字节的墙钟毫秒时间，与心跳线程共享。
    last_seen: Arc<AtomicU64>,
}

impl<R: Read> Read for Seen<R> {
    /// 转发一次读，并在读到字节时刷新活跃时间；EOF（0 字节）和错误不刷新。
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let read = self.inner.read(buffer)?;
        if read > 0 {
            self.last_seen.store(now_ms(), Ordering::Relaxed);
        }
        Ok(read)
    }
}

/// 启动固定 `WORKERS` 个文件操作 worker，返回任务发送端。
///
/// 所有 worker 通过 `Mutex<Receiver<Job>>` 竞争取任务：同一时刻只有一个 worker 持锁阻塞在
/// `recv`，取到任务后释放锁再执行，因此最多可以有 `WORKERS` 个任务同时运行。
/// 发送端被丢弃后（`serve_frames` 结束），`recv` 返回 `Err`，空闲 worker 退出；已入队的
/// 任务仍会先被处理，但 `serve` 不 join worker，进程结束时它们可能还在运行。
fn start_workers() -> Sender<Job> {
    let (jobs, queue) = mpsc::channel::<Job>();
    let queue = Arc::new(Mutex::new(queue));
    for _ in 0..WORKERS {
        let queue: Arc<Mutex<Receiver<Job>>> = queue.clone();
        thread::spawn(move || {
            loop {
                let job = queue.lock().unwrap().recv();
                match job {
                    Ok(job) => job(),
                    Err(_) => return,
                }
            }
        });
    }
    jobs
}

/// 执行一个已登记的请求、注销它的取消状态，并发送终局帧（RESULT 或 ERROR）。
///
/// 排队规则：
/// - `exec`/`pty` 的终局帧必须排在它们已经产生的 bulk 输出事件之后，所以走 `Output::bulk`；
///   错误帧也一样，保证客户端先看到输出再看到结束。
/// - `watch` 的事件本身走 control 队列，终局帧是空对象或错误，留在 control 队列即可保持先后。
/// - 其他请求的回复通常走 control 队列；payload 大于 `CONTROL_PAYLOAD`（64 KiB）时改走
///   bulk，避免一个大文件读取结果把 ping 和小回复堵在它后面。
///
/// `dispatch` 执行期间控制块一直可查，因此这段时间到达的 CANCEL 能作用于该请求（是否中止
/// 取决于操作本身是否检查取消标志）；注销发生在排队之前，注销后到达的 CANCEL 找不到目标，
/// 被静默忽略。
fn respond(server: &Server, id: u32, request: Frame, control: Arc<Control>) {
    let is_exec = matches!(request.json["op"].as_str(), Some("exec" | "pty"));
    let reply = match server.dispatch(id, &request.json, request.payload, &control) {
        Ok((json, payload)) => Frame::with_payload(RESULT, id, json, payload),
        Err(failure) => Frame::new(ERROR, id, failure.to_json()),
    };
    // 先注销再排队：从这一刻起该 id 的 CANCEL 不再能找到控制块。
    server.controls.lock().unwrap().remove(&id);
    // A command's result follows its output; large payloads do not hold up pings and small replies.
    if is_exec || reply.payload.len() > CONTROL_PAYLOAD {
        server.output.bulk(reply, None);
    } else {
        server.output.control(reply);
    }
}

/// Queue `job` behind the handle's earlier chunk writes; one pool job drains the queue.
///
/// 保证同一文件句柄上的 `writeChunk` 按请求到达读线程的顺序执行，且同一时刻只有一个写；
/// 不同文件句柄互不影响。若队列原本空闲，这里才向 worker 池投递一个 drain 任务；drain
/// 任务会把队列取空，最后在锁内把 `running` 置回 `false` 再退出，因此不会丢唤醒，也不会
/// 每个 chunk 都占用一个池任务。
///
/// 只覆盖 `writeChunk`：`pread`、`fstat`、`close` 等不经过这个队列，可以与写并发发生。
fn run_in_order(jobs: &Sender<Job>, handle: Arc<FileHandle>, job: Job) {
    let mut serial = handle.serial.lock().unwrap();
    serial.jobs.push_back(job);
    if serial.running {
        return;
    }
    serial.running = true;
    // 先释放队列锁再投递：通道是无界的（mpsc::channel），send 不会阻塞；先释放可以避免
    // worker 拿到 drain 任务后先等这把锁。
    drop(serial);
    let _ = jobs.send(Box::new(move || {
        loop {
            // 取任务与“队列空则复位 running”必须在同一把锁内完成，否则新任务可能看到
            // running=true 而无人再取，造成永久挂起。
            let next = {
                let mut serial = handle.serial.lock().unwrap();
                let next = serial.jobs.pop_front();
                if next.is_none() {
                    serial.running = false;
                }
                next
            };
            match next {
                Some(job) => job(),
                None => return,
            }
        }
    }));
}

/// 读帧主循环：唯一读线程按到达顺序分发请求，直到 stdin 干净结束（返回 `Ok`）或帧错误
/// （返回 `Err`）。
///
/// 调度规则（对应 docs/protocol.md 的 Scheduling 一节）：
/// - `ptyWrite`/`ptyResize`/`ptyForeground` 在读线程内同步处理：必须按到达顺序生效，
///   丢进多线程池可能让两次写互换；它们不登记 `Control`，因此不可取消。
/// - `exec`/`watch`/`pty` 各开一个 16 MiB 栈的专用线程：任务可能长期运行，而 `watch`
///   会递归扫描目录树；它们不能占用文件操作 worker。
/// - 其余操作交给 `WORKERS` 个 worker；`writeChunk` 额外按句柄串行（`run_in_order`）。
///
/// 对上面未单独处理的 REQUEST，取消状态在调度前登记，保证紧跟 REQUEST 的 CANCEL 不会丢；
/// 专用线程创建失败时会注销登记并向客户端回 `spawn_error`。`read_frame` 在长度前缀未读满时
/// 返回 `None`，因此客户端只写了 1–3 字节长度头就关闭连接，也会被当作干净结束。
fn serve_frames(server: &Arc<Server>, input: &mut impl Read) -> io::Result<()> {
    // 文件操作 worker 池的生命周期与本次连接相同；返回时 jobs 被丢弃，空闲 worker 退出。
    let jobs = start_workers();
    while let Some(request) = frame::read_frame(input)? {
        match request.kind {
            REQUEST
                if matches!(
                    request.json["op"].as_str(),
                    Some("ptyWrite" | "ptyResize" | "ptyForeground")
                ) =>
            {
                // Answered here, in arrival order: through the worker pool, two writes could swap.
                // 回复走 control 队列；写操作本身只把数据排入终端输入队列，不会长期阻塞。
                let reply = match request.json["op"].as_str() {
                    Some("ptyWrite") => {
                        pty::write(&server.terminals, &request.json, request.payload)
                    }
                    Some("ptyResize") => pty::resize(&server.terminals, &request.json),
                    _ => pty::foreground(&server.terminals, &request.json),
                };
                server.output.control(match reply {
                    Ok(json) => Frame::new(RESULT, request.id, json),
                    Err(failure) => Frame::new(ERROR, request.id, failure.to_json()),
                });
            }
            REQUEST => {
                let id = request.id;
                // Registered before the request runs, so a cancel right behind it is not lost.
                // 同一 id 重复登记会覆盖旧控制块，随后按 id 注销会互相干扰；客户端应保证 id 唯一。
                let control = Arc::new(Control::default());
                server.controls.lock().unwrap().insert(id, control.clone());
                // Commands and watchers last; they get their own threads (watchers recurse through trees).
                // 三者统一使用 16 MiB 栈；现有说明指出 watch 会递归遍历目录树。spawn 失败时
                // 操作尚未开始，注销控制块并回一个 ERROR，避免客户端永远等这个 id。
                if matches!(request.json["op"].as_str(), Some("exec" | "watch" | "pty")) {
                    let worker = server.clone();
                    let spawned = thread::Builder::new()
                        .stack_size(16 * 1024 * 1024)
                        .spawn(move || respond(&worker, id, request, control));
                    if let Err(error) = spawned {
                        server.controls.lock().unwrap().remove(&id);
                        let failure = Failure::new("spawn_error", error.to_string());
                        server
                            .output
                            .bulk(Frame::new(ERROR, id, failure.to_json()), None);
                    }
                } else {
                    let worker = server.clone();
                    // 提前解析 writeChunk 的句柄：有效时把任务放进该句柄的串行队列；句柄无效
                    // 或解析失败时为 None，直接进并发池，由 dispatch 返回对应的 EBADF/EINVAL。
                    let serial = match request.json["op"].as_str() {
                        Some("writeChunk") => server.file(&request.json).ok(),
                        _ => None,
                    };
                    let job: Job = Box::new(move || respond(&worker, id, request, control));
                    match serial {
                        Some(handle) => run_in_order(&jobs, handle, job),
                        None => {
                            let _ = jobs.send(job);
                        }
                    }
                }
            }
            CANCEL => {
                // `{ mode: "kill" }` 只对 exec/pty 有 cleanup（清理）语义：杀掉进程组，但请求仍按
                // 被杀进程的退出状态正常结算，不产生 `aborted`。其余取消只置 abort 标志，最终结局由
                // 具体操作决定：exec/pty/scanLines/listFiles 在各自的检查点看到该标志后以 error
                // `"aborted"` 结束；watch 停止并返回成功结果 `{}`（见 watch.rs 的循环）；不检查该标志
                // 的短操作照常完成。scanLines/listFiles/watch 只读 `aborted`，所以 kill 模式对它们没有
                // 可观察效果。找不到对应 id 的 CANCEL 被忽略。
                let control = server.controls.lock().unwrap().get(&request.id).cloned();
                if let Some(control) = control {
                    control.cancel(request.json["mode"].as_str() == Some("kill"));
                }
            }
            // RESULT/ERROR/EVENT/PING 以及对端发来的未知类型都不需要处理；PING 的字节
            // 已经在读帧时刷新了 Seen 的活跃时间。
            _ => {}
        }
    }
    Ok(())
}

/// 运行一个连接的服务循环，函数返回即连接结束。
///
/// 启动顺序：
/// 1. 向 stdout 打印握手行 `KIANA-ENV <token>` 并 flush；这是客户端同步点，之后 stdout 专用于帧。
/// 2. 建 `Output` 与唯一写线程（1 MiB `BufWriter`）；所有帧只经它写 stdout。
/// 3. 建共享 `Server`，启动心跳线程（每 5 秒 ping，静默超时则杀进程组并退出）。
/// 4. 用 1 MiB `BufReader` 包住 stdin，并用 `Seen` 记录活跃时间。
/// 5. 进入 `serve_frames`；无论它因 EOF 还是错误返回，都先 `kill_all`、关闭输出、join 写线程，
///    再把结果交给 `main` 决定退出码。
///
/// 注意：心跳线程从不 join，可能在任何时刻调用 `process::exit(0)`；这是协议约定的
/// “断线即停”语义，不依赖客户端发送关闭指令。
fn serve(token: &str) -> io::Result<()> {
    {
        // 握手行必须先于任何帧出现在 stdout。锁在块内，flush 后立即释放，随后 stdout 交给
        // Output 写线程独占；SSH 启动脚本在这之前打印的内容由客户端负责丢弃。
        let mut stdout = io::stdout().lock();
        writeln!(stdout, "KIANA-ENV {token}")?;
        stdout.flush()?;
    }
    let output = Arc::new(Output::default());
    let writer = {
        // stdout 的唯一写线程：用 1 MiB BufWriter 承载帧；队列优先级与关闭逻辑由 Output 负责。
        let output = output.clone();
        thread::spawn(move || {
            output.run(&mut BufWriter::with_capacity(1 << 20, io::stdout().lock()))
        })
    };
    let server = Arc::new(Server {
        output: output.clone(),
        tmpdir: sys::tmpdir(),
        handles: Mutex::new(HashMap::new()),
        next_handle: AtomicU64::new(1),
        controls: Mutex::new(HashMap::new()),
        groups: Arc::new(Mutex::new(Default::default())),
        terminals: Arc::new(Mutex::new(HashMap::new())),
    });
    let last_seen = Arc::new(AtomicU64::new(now_ms()));
    {
        // 心跳线程：每 5 秒投递一个 id=0 的 PING（让客户端知道 daemon 活着），并检查 stdin
        // 最近一次读到字节的时间。静默超过 30 秒说明客户端已失联（例如手机断网），
        // 杀掉所有命令进程组后直接退出，避免远端留下长时间运行的任务。检查每 5 秒一次，
        // 所以实际退出通常在静默 30 秒后的下一次检查（约 35 秒内，线程调度可能更久）。
        let output = output.clone();
        let last_seen = last_seen.clone();
        let groups = server.groups.clone();
        thread::spawn(move || {
            loop {
                thread::sleep(PING_INTERVAL);
                output.control(Frame::new(PING, 0, json!({})));
                // A client that went silent (a phone that lost its network) leaves nothing running.
                if now_ms().saturating_sub(last_seen.load(Ordering::Relaxed)) > SILENCE_LIMIT_MS {
                    kill_all(&groups);
                    std::process::exit(0);
                }
            }
        });
    }
    // stdin 用 1 MiB 缓冲；Seen 在每次读到字节时刷新 last_seen，供心跳线程判断。
    let mut input = BufReader::with_capacity(
        1 << 20,
        Seen {
            inner: io::stdin().lock(),
            last_seen,
        },
    );
    let result = serve_frames(&server, &mut input);
    // However input ended, the client is gone: stop everything it started.
    // 清理顺序：先杀命令进程组，再关闭输出队列，最后 join 写线程；写线程会排空已入队帧，
    // 但 stdout 写失败时 Output 会自行关闭并提前返回。
    kill_all(&server.groups);
    output.close();
    let _ = writer.join();
    result
}

/// 进程入口：解析 `serve --token <hex>` / `--version`，并决定退出码。
///
/// 退出码：
/// - 0：`--version`；`serve` 正常结束（例如 stdin EOF）；或心跳线程在静默超时后直接
///   `process::exit(0)`（`tests/cli.rs` 覆盖前两种，`tests/protocol.rs` 的
///   `exits_after_thirty_seconds_without_stdin_bytes` 覆盖静默退出）；
/// - 1：`serve` 因 I/O 或帧错误返回（`tests/protocol.rs` 的帧长度错误用例覆盖）；
/// - 2：缺少子命令、`serve` 缺少 token，或未知子命令（usage 打到 stderr，`tests/cli.rs` 覆盖）。
///
/// 参数解析刻意宽松：`--token` 可以出现在 `serve` 之后的任意位置，取它后面的那个参数；
/// 多余参数被忽略；`--version` 只认 `args[1]`，其后的参数也忽略。
fn main() {
    let args: Vec<String> = env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("serve") => {
            // 取第一个 "--token" 后紧跟的参数；不校验 token 格式，缺失（包括位于末尾）即 usage。
            let token = args
                .iter()
                .position(|arg| arg == "--token")
                .and_then(|index| args.get(index + 1));
            let Some(token) = token else {
                eprintln!("usage: kiana-env serve --token <hex>");
                std::process::exit(2);
            };
            // serve 只在 I/O/帧错误时返回 Err；干净 EOF 返回 Ok。
            if let Err(error) = serve(token) {
                eprintln!("kiana-env: {error}");
                std::process::exit(1);
            }
            // Request threads may still be blocked (a FIFO open); nothing is left to answer them.
            // 连接已结束，但仍可能有请求线程阻塞在系统调用里（例如等待写端的 FIFO open）；
            // 它们即使解除阻塞也没有客户端可回复。这里用 process::exit 立即终止整个进程，
            // 不再执行进程内其他对象（包括这些线程所持资源）的析构。
            std::process::exit(0);
        }
        Some("--version") => println!("kiana-env {VERSION}"),
        _ => {
            eprintln!("usage: kiana-env serve --token <hex>");
            std::process::exit(2);
        }
    }
}

#[cfg(test)]
mod main_tests {
    use super::*;

    #[test]
    fn request_helpers_report_the_field_names() {
        let json = json!({ "path": "/p", "handle": 3, "flag": true });
        assert_eq!(field(&json, "path").unwrap(), "/p");
        let failure = field(&json, "to").unwrap_err().to_json();
        assert_eq!(failure["code"], json!("EINVAL"));
        assert_eq!(failure["message"], json!("missing field to"));
        assert_eq!(number(&json, "handle").unwrap(), 3);
        let failure = number(&json, "handle2").unwrap_err().to_json();
        assert_eq!(
            failure["message"],
            json!("missing or invalid field handle2")
        );
        // Negative, fractional and string numbers are not u64.
        for value in [json!(-1), json!(1.5), json!("3")] {
            let failure = number(&json!({ "n": value }), "n").unwrap_err().to_json();
            assert_eq!(failure["message"], json!("missing or invalid field n"));
        }
        assert!(flag(&json, "flag"));
        assert!(!flag(&json, "missing"), "a missing flag is false");
        assert!(!flag(&json!({ "flag": "yes" }), "flag"));
    }

    #[test]
    fn now_ms_is_wall_clock_milliseconds() {
        let before = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let now = now_ms();
        let after = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        assert!(before <= now && now <= after);
    }

    #[test]
    fn serving_constants_match_the_documented_values() {
        assert_eq!(PROTOCOL, 1);
        assert_eq!(FEATURES, ["listFiles"]);
        assert_eq!(PING_INTERVAL, Duration::from_secs(5));
        assert_eq!(SILENCE_LIMIT_MS, 30_000);
        assert_eq!(SCAN_CHUNK, 64 * 1024);
        assert_eq!(WORKERS, 16);
        assert_eq!(MAX_HANDLES, 4096);
        assert_eq!(CONTROL_PAYLOAD, 64 * 1024);
    }

    /// A `Server` with no writer thread: handle bookkeeping needs only the maps.
    fn server(tmpdir: &str) -> Server {
        Server {
            output: Arc::new(Output::default()),
            tmpdir: tmpdir.to_string(),
            handles: Mutex::new(HashMap::new()),
            next_handle: AtomicU64::new(1),
            controls: Mutex::new(HashMap::new()),
            groups: Arc::new(Mutex::new(Default::default())),
            terminals: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    #[test]
    fn handles_start_at_one_and_file_handles_report_a_kind_mismatch() {
        let dir = std::env::temp_dir().join(format!("kiana-env-handles-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a");
        std::fs::write(&path, b"x").unwrap();
        let server = server(&dir.to_string_lossy());
        let first = server
            .insert_file(File::open(&path).unwrap(), &path.to_string_lossy())
            .unwrap();
        assert_eq!(first, 1, "handle ids start at one");
        let second = server
            .insert_file(File::open(&path).unwrap(), &path.to_string_lossy())
            .unwrap();
        assert_eq!(second, 2);
        // `file` finds a file handle, and rejects a directory handle with "not a file handle".
        let file = server.file(&json!({ "handle": first })).unwrap();
        assert_eq!(file.path, path.to_string_lossy());
        let dir_handle = server
            .insert(Handle::Dir(Arc::new(Mutex::new(DirHandle {
                entries: std::fs::read_dir(&dir).unwrap(),
                path: dir.to_string_lossy().into_owned(),
            }))))
            .unwrap();
        let failure = server
            .file(&json!({ "handle": dir_handle }))
            .err()
            .unwrap()
            .to_json();
        assert_eq!(failure["code"], json!("EBADF"));
        assert_eq!(failure["message"], json!("not a file handle"));
        let failure = server
            .file(&json!({ "handle": 99 }))
            .err()
            .unwrap()
            .to_json();
        assert_eq!(failure["code"], json!("EBADF"));
        assert_eq!(failure["message"], json!("unknown handle"));
        let failure = server.file(&json!({})).err().unwrap().to_json();
        assert_eq!(failure["message"], json!("missing or invalid field handle"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_handle_limit_is_4096_with_libuv_wording() {
        let server = server("/tmp");
        let path = "/dev/null";
        for index in 1..=MAX_HANDLES as u64 {
            let id = server.insert_file(File::open(path).unwrap(), path).unwrap();
            assert_eq!(id, index);
        }
        let failure = server
            .insert_file(File::open(path).unwrap(), path)
            .err()
            .unwrap()
            .to_json();
        assert_eq!(failure["code"], json!("EMFILE"));
        assert_eq!(failure["message"], json!("EMFILE: too many open files"));
    }

    #[test]
    fn events_carry_the_request_id_and_scan_lines_rejects_empty_ranges() {
        // `scan_lines` validates the range before reading, so a scratch file is enough.
        let dir = std::env::temp_dir().join(format!("kiana-env-scan-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a");
        std::fs::write(&path, b"one\ntwo\n").unwrap();
        let file = File::open(&path).unwrap();
        let control = Control::default();
        let failure = scan_lines(
            &file,
            "a",
            &json!({ "startLine": 2, "endLine": 2 }),
            &control,
        )
        .unwrap_err()
        .to_json();
        assert_eq!(failure["code"], json!("EINVAL"));
        assert_eq!(failure["message"], json!("Invalid line range"));
        let failure = scan_lines(&file, "a", &json!({}), &control)
            .unwrap_err()
            .to_json();
        assert_eq!(
            failure["message"],
            json!("missing or invalid field startLine")
        );
        // Non-u64 `endLine` counts as absent, so no range check happens.
        let value = scan_lines(
            &file,
            "a",
            &json!({ "startLine": 0, "endLine": "x" }),
            &control,
        )
        .unwrap();
        assert_eq!(value["newlines"], json!(2));
        assert_eq!(value["end"], json!(8));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_cancelled_control_stops_the_scan() {
        let dir =
            std::env::temp_dir().join(format!("kiana-env-scan-cancel-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a");
        std::fs::write(&path, b"one\ntwo\n").unwrap();
        let file = File::open(&path).unwrap();
        let control = Control::default();
        control.cancel(false);
        let failure = scan_lines(&file, "a", &json!({ "startLine": 0 }), &control)
            .unwrap_err()
            .to_json();
        assert_eq!(failure["code"], json!("aborted"));
        assert_eq!(failure["message"], json!("aborted"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_version_comes_from_the_build_script() {
        // build.rs sets KIANA_ENV_VERSION from the environment or falls back to the npm package version
        // (see crates/kiana-env/build.rs), as the reference build.rs reads packages/env/package.json.
        assert_eq!(VERSION, env!("KIANA_ENV_VERSION"));
        assert!(!VERSION.is_empty());
    }
}
