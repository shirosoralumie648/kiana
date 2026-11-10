//! core/bash-executor.ts。
//!
//! 带流式与取消能力的 bash 执行器，被 `AgentSession.executeBash()`（交互/RPC）与需要 bash
//! 的模式共用。
//!
//! # 与 bash 工具的区别（必须保留）
//!
//! 这里对**每一块**输出都做 `stripAnsi` + `sanitizeBinaryOutput` + 删除 `\r`；bash 工具
//! （`OutputAccumulator`）保留原始字节，ANSI 一直到 TUI/HTML 渲染时才解释。
//!
//! # 流式细节
//!
//! - 解码用 `Utf8StreamDecoder`（跨块保留不完整的多字节序列）；
//! - `pending_ansi + 解码文本` 再交给 `splitIncompleteAnsiSuffix`，只处理 `complete` 部分
//!   （顺序不能反：先切分再解码会让被块边界切开的转义序列泄漏成可见字符）；
//! - 滚动缓冲比较的 `outputBytes` 是**文本的 UTF-16 code unit 数**（`text.length`），
//!   而阈值是 `DEFAULT_MAX_BYTES * 2`（字节数）——这一量纲混用是当前稳定行为，改动需同步测试；
//! - 累计输入字节 `totalBytes > DEFAULT_MAX_BYTES` 时开始写临时文件（写入的是**已清理**的
//!   文本，不是原始字节；与 `OutputAccumulator` 相反）。
//!
//! # 调用方
//!
//! 主要调用方是 `kiana-session` 的 `AgentSession::execute_bash`：它把 `on_chunk` 包装成
//! 「用户回调 + `BashExecutionUpdate` 事件」再传进来，并在执行结束后用返回值记录 bash
//! 结果；`kiana-interactive` 直接使用 [`BashChunkCallback`] 类型做 UI 流式渲染。
//! `tests/bash_executor.rs` 用脚本化的 [`BashOperations`] 驱动同一入口。
//!
//! # 执行流程
//!
//! 1. 构造 `BashExecState`（`Mutex` 保护）与 `on_data` 闭包；闭包把每个块交给 `feed`，
//!    I/O 错误只记录在 `state.error`（回调签名不能返回错误）；
//! 2. 调用 [`BashOperations::exec`]，原样交出命令、cwd、`on_data` 与取消 token；
//!    本模块**不轮询**取消，取消是否生效取决于实现；
//! 3. `exec` 返回后先决定错误处理，再 `flush` 解码器尾部（未完成的多字节序列变成
//!    U+FFFD，残留的 ANSI 前缀也在这里输出）；
//! 4. 取 `state.error`（有则返回 `Err`），拼接滚动缓冲并 `truncate_tail`；
//! 5. 返回 [`BashResult`]；取消时 `exit_code` 置 `None`。
//!
//! # 失败模式与注意事项
//!
//! - **错误优先级**：输出处理（feed/flush/临时文件）的 I/O 错误一旦记录，最终一定返回
//!   `Err`，即使 token 已取消；取消只会吞掉 `exec` 自己返回的**普通**错误。
//!   携带安全结论的错误（`|unconfirmed`、沙箱 `Unconfirmed:`/`BackendUnavailable:`/
//!   `Interrupted:` 与拒绝码）即使 token 已取消也原样上抛，避免调用方自动重试一条
//!   可能已经执行过的命令；
//! - 第一个错误胜出：`on_data` 看到 `state.error` 已设置就直接丢弃后续块；
//! - `on_chunk` 在持有互斥锁时同步调用，回调应尽快返回；回调 panic 本模块不捕获，
//!   若之后仍会用到同一把锁，`lock()` 会取回内部状态而不是再次 panic；
//! - **滚动缓冲不是完整日志**：UTF-16 单元数超过 `DEFAULT_MAX_BYTES * 2` 时会整块丢弃
//!   最旧的输出（至少保留一块）。UTF-16 单元数不超过其来源的原始字节数，所以发生丢弃时
//!   累计原始输入必然已超过阈值、临时文件已经创建——完整内容只能从 `full_output_path` 读；
//! - `full_output_path` 只表示临时文件被创建过，与 `truncated` 不互为充要：`truncated`
//!   为真时一定会创建（结束前会按需补建）。反方向要分情况：跨过原始字节阈值后至少还有
//!   一块清理后非空的文本时，临时文件已创建（此时 `truncated` 可为假）；若跨过阈值的块
//!   清理后为空且此后没有非空块，`append_text` 在阈值判断之前就因空文本提前返回，
//!   临时文件不会被创建，路径仍为 `None`。

use std::io::{BufWriter, Write as _};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use kiana_core::ansi::{split_incomplete_ansi_suffix, strip_ansi};
use kiana_core::output_files::create_output_file_stream;
use kiana_core::shell::sanitize_binary_output;
pub use kiana_ext_api::tool_result::{BashExecOptions, BashExecResult, BashOperations, BashResult};
use tokio_util::sync::CancellationToken;

use crate::output_accumulator::Utf8StreamDecoder;
use crate::truncate::{DEFAULT_MAX_BYTES, TruncationOptions, truncate_tail, utf16_len};

/// 流式输出回调（已清理）。TS: `onChunk?: (chunk: string) => void`。
pub type BashChunkCallback = Arc<dyn Fn(&str) + Send + Sync>;

/// 执行器透传的错误（TS 里是任意 throw）。
pub type BashExecError = Box<dyn std::error::Error + Send + Sync>;

/// `BashExecutorOptions`。
///
/// TS: `core/bash-executor.ts#BashExecutorOptions`。`onChunk` 收到的块已经过清理。
#[derive(Clone, Default)]
pub struct BashExecutorOptions {
    /// 流式输出回调（已清理）。
    ///
    /// 回调在 [`BashOperations`] 调用 `on_data` 的线程上同步执行（可能是任意线程），
    /// 因此类型要求 `Send + Sync`；同一时刻只有一块在回调中（执行器内部持锁调用）。
    pub on_chunk: Option<BashChunkCallback>,
    /// 取消信号（TS 的 `AbortSignal`）。执行器只把它透传给 [`BashOperations`]，
    /// 并在结束时用最终状态决定 `cancelled`/`exit_code`。
    pub signal: Option<CancellationToken>,
}

impl std::fmt::Debug for BashExecutorOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BashExecutorOptions")
            .field("on_chunk", &self.on_chunk.is_some())
            .field("signal", &self.signal)
            .finish()
    }
}

/// 执行状态。`onData` 回调是 `Fn`，用互斥量做内部可变性。
struct BashExecState {
    /// 滚动缓冲：返回值与临时文件重放都从这里取；超过上限时会丢弃最旧的块。
    output_chunks: Vec<String>,
    /// `text.length` 之和（UTF-16 code unit，不是字节）。
    output_bytes: usize,
    /// 已创建的临时文件路径；`finish_temp_file` 之后仍保留，用于 `full_output_path`。
    temp_file_path: Option<PathBuf>,
    /// 临时文件写入器；`finish_temp_file` 时 flush 并丢弃。
    temp_file: Option<BufWriter<std::fs::File>>,
    /// 累计输入原始字节数。
    total_bytes: usize,
    /// 上一块末尾未完成的 ANSI 转义序列。
    pending_ansi: String,
    decoder: Utf8StreamDecoder,
    /// 第一个输出处理错误；一旦记录，`on_data` 会丢弃后续所有块。
    error: Option<std::io::Error>,
}

impl BashExecState {
    fn new() -> Self {
        Self {
            output_chunks: Vec::new(),
            output_bytes: 0,
            temp_file_path: None,
            temp_file: None,
            total_bytes: 0,
            pending_ansi: String::new(),
            decoder: Utf8StreamDecoder::new(),
            error: None,
        }
    }

    fn ensure_temp_file(&mut self) -> std::io::Result<()> {
        // 幂等：临时文件只在首次调用时创建，之后直接返回。
        if self.temp_file_path.is_some() {
            return Ok(());
        }
        let (path, file) = create_output_file_stream("kiana-bash", ".log")?;
        // 用同步 BufWriter 写临时文件：on_data 回调是同步的，这里无法 await。
        // 如果 tokio 句柄已无法换回 std 文件，就丢弃它并按路径以只写方式重新打开；
        // 此时文件尚未写入，重新打开不会截断任何内容。
        let std_file = match file.try_into_std() {
            Ok(file) => file,
            Err(file) => {
                drop(file);
                std::fs::OpenOptions::new().write(true).open(&path)?
            }
        };
        let mut writer = BufWriter::new(std_file);
        // 把已经进入滚动缓冲的清理后文本重放到文件开头，保证文件从第一块开始连续。
        // 调用点都在写入当前块之前，因此文件内容的顺序与 output_chunks 一致。
        // 若重放写入失败，函数返回 Err 且不记录 path：已创建的文件会留在临时目录，
        // 本模块不清理它。
        for chunk in &self.output_chunks {
            writer.write_all(chunk.as_bytes())?;
        }
        self.temp_file_path = Some(path);
        self.temp_file = Some(writer);
        Ok(())
    }

    fn finish_temp_file(&mut self) {
        if let Some(mut writer) = self.temp_file.take() {
            // 只 flush 不 close；flush 的错误被忽略（函数没有错误通道），
            // 因此返回成功不能保证临时文件内容完整。
            let _ = writer.flush();
        }
    }

    fn append_text(
        &mut self,
        raw_text: &str,
        on_chunk: Option<&BashChunkCallback>,
    ) -> std::io::Result<()> {
        // 清理：去 ANSI、替换二进制垃圾、归一换行。
        let text = sanitize_binary_output(&strip_ansi(raw_text)).replace('\r', "");
        if text.is_empty() {
            return Ok(());
        }

        // 超过阈值后开始写临时文件。判断用的是累计**原始输入**字节，且发生在写入本块
        // 之前；空块已在上面提前返回，因此不会触发建文件。
        if self.total_bytes > DEFAULT_MAX_BYTES {
            self.ensure_temp_file()?;
        }

        if let Some(writer) = self.temp_file.as_mut() {
            writer.write_all(text.as_bytes())?;
        }

        // 滚动缓冲。
        // 按「UTF-16 code unit」计数，阈值却是字节数（当前稳定行为，改动需同步测试）。
        // `len() > 1` 保证至少保留一块：单个超大块不会被丢弃。
        self.output_bytes += utf16_len(&text);
        self.output_chunks.push(text.clone());
        let max_output_bytes = DEFAULT_MAX_BYTES * 2;
        while self.output_bytes > max_output_bytes && self.output_chunks.len() > 1 {
            let removed = self.output_chunks.remove(0);
            self.output_bytes -= utf16_len(&removed);
        }
        // 这里若真的丢弃过块，累计原始输入必然已超过 DEFAULT_MAX_BYTES
        // （UTF-16 单元数 ≤ 原始字节数），临时文件已经创建，完整内容仍可从
        // full_output_path 取回；滚动缓冲本身不再是完整日志。

        if let Some(on_chunk) = on_chunk {
            on_chunk(&text);
        }
        Ok(())
    }

    fn feed(&mut self, data: &[u8], on_chunk: Option<&BashChunkCallback>) -> std::io::Result<()> {
        // total_bytes 在解码前累加：它衡量喂进来的原始字节，而不是清理后的文本。
        self.total_bytes += data.len();
        let decoded = self.decoder.decode(data, false);
        let combined = format!("{}{}", self.pending_ansi, decoded);
        let split = split_incomplete_ansi_suffix(&combined);
        self.pending_ansi = split.pending;
        self.append_text(&split.complete, on_chunk)
    }

    fn flush(&mut self, on_chunk: Option<&BashChunkCallback>) -> std::io::Result<()> {
        // 流结束时把解码器未完成的字节（解码为 U+FFFD）与残留 ANSI 前缀一起输出；
        // 之后流就结束了，这里清空 pending_ansi 以免重复输出。
        let rest = format!("{}{}", self.pending_ansi, self.decoder.decode(&[], true));
        self.pending_ansi.clear();
        self.append_text(&rest, on_chunk)
    }
}

fn lock(state: &Mutex<BashExecState>) -> MutexGuard<'_, BashExecState> {
    // 中毒恢复：临界区内只有内存操作，panic 主要来自调用方传入的 on_chunk；
    // 让锁永久不可用会让之后到达的所有输出块丢失，所以取回内部状态继续。
    state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// `BashOperations::exec` 的错误是否携带**不能因取消而丢弃**的安全结论。
///
/// 两条协议来源：
/// - 本地 shell 的中止错误在 relay 已经启动过命令后带 `|unconfirmed` 后缀
///   （命令可能已经执行，调用方不得自动重试）；
/// - 沙箱状态/拒绝错误使用固定前缀：`Unconfirmed:` / `BackendUnavailable:` /
///   `Interrupted:` 与 `READ_ONLY:` / `OUTSIDE_WORKSPACE:` / `PROTECTED_PATH:` 等。
///
/// 其他自定义 `BashOperations` 只要按同一文本协议返回错误即可被识别；普通错误
/// （例如 `aborted`/`timeout:…`）保持 TS 的“取消后返回已产生输出”的语义。
fn exec_error_carries_safety_conclusion(error: &(dyn std::error::Error + Send + Sync)) -> bool {
    let message = error.to_string();
    if message.contains("|unconfirmed") {
        return true;
    }
    const SAFETY_PREFIXES: &[&str] = &[
        // ExecutionStatus::tool_message()（沙箱状态）。
        "Unconfirmed:",
        "BackendUnavailable:",
        "Interrupted:",
        // SandboxDenial::message()（执行策略拒绝）。
        "INVALID_CONFIG:",
        "UNKNOWN_TOOL:",
        "TOOL_INPUT_MISMATCH:",
        "NO_WRITE_PATHS:",
        "PATH_UNRESOLVED:",
        "READ_ONLY:",
        "OUTSIDE_WORKSPACE:",
        "PROTECTED_PATH:",
    ];
    SAFETY_PREFIXES
        .iter()
        .any(|prefix| message.starts_with(prefix))
}

/// 使用自定义 [`BashOperations`] 执行 bash 命令（远端执行、容器、测试 stub 等）。
///
/// TS: `executeBashWithOperations(command, cwd, operations, options?)`。
/// 取消时仍返回已产生的输出；未取消的失败把错误透传（TS 的 `catch` + rethrow）。
///
/// 例外：`exec` 的错误携带安全结论时（`|unconfirmed`、沙箱状态/拒绝前缀，见
/// [`exec_error_carries_safety_conclusion`]），即使取消也会返回 `Err`——此时
/// 命令可能已经执行，不能把它降级成普通的 `cancelled` 结果。
///
/// `cwd` 原样交给 [`BashOperations::exec`]，本模块不校验目录是否存在；`timeout` 与 `env`
/// 固定传 `None`（本入口没有这两个参数）。`options` 缺省表示无回调、无取消。
///
/// 返回值：`output` 是清理后的文本（去 ANSI、去二进制控制字符、去 `\r`），超过默认截断
/// 上限（2000 行 / 50 KiB，即 `TruncationOptions::default()`）时只保留尾部。`exit_code` 为
/// `None` 有两种来源——运行被取消，或 `operations.exec` 返回的退出码本身就是 `None`
/// （TS 的 `null`）——用 [`BashResult::cancelled`] 区分。`execution_status` 从
/// [`BashExecResult::execution_status`] 原样复制；取消只清空 `exit_code`，不改写状态。
///
/// 错误：未取消时 `exec` 的错误原样返回；临时文件创建/写入的 `io::Error` 也以
/// [`BashExecError`] 返回（输出解码本身不会失败：`Utf8StreamDecoder` 把非法字节替换为
/// U+FFFD）。错误路径不会返回部分 [`BashResult`]。
pub async fn execute_bash_with_operations(
    command: &str,
    cwd: &str,
    operations: &dyn BashOperations,
    options: Option<BashExecutorOptions>,
) -> Result<BashResult, BashExecError> {
    let signal = options.as_ref().and_then(|options| options.signal.clone());
    let on_chunk = options
        .as_ref()
        .and_then(|options| options.on_chunk.clone());
    let state = Mutex::new(BashExecState::new());

    // 整个执行期间共享一份状态：on_data 可能由 operations 在任意线程同步调用，
    // 所以用 Mutex；锁只在同步的 feed 期间持有，不跨 await。
    let on_data = |data: &[u8]| {
        let mut state = lock(&state);
        if state.error.is_some() {
            // 第一个错误胜出：后续块不再处理，也不再调用 on_chunk。
            return;
        }
        if let Err(error) = state.feed(data, on_chunk.as_ref()) {
            state.error = Some(error);
        }
    };

    let exec_result = operations
        .exec(
            command,
            cwd,
            BashExecOptions {
                on_data: &on_data,
                signal: signal.clone(),
                timeout: None,
                env: None,
            },
        )
        .await;

    let (exit_code, execution_status) = match exec_result {
        Ok(result) => (result.exit_code, result.execution_status),
        Err(error) => {
            let cancelled = signal.as_ref().is_some_and(CancellationToken::is_cancelled);
            // 取消只允许吞掉“中止本来就是预期结果”的普通错误。命令**可能已经执行**
            // 的 `|unconfirmed`，以及沙箱拒绝/后端不可用等安全结论，不能因为取消而
            // 丢失——否则调用方只看到 `cancelled`，可能自动重试一条已经产生副作用
            // 的命令，或把沙箱拒绝当成用户取消。
            if !cancelled || exec_error_carries_safety_conclusion(error.as_ref()) {
                // 错误路径不返回部分 BashResult（与未取消失败一致）；先收尾临时文件。
                lock(&state).finish_temp_file();
                return Err(error);
            }
            (None, None)
        }
    };

    {
        let mut state = lock(&state);
        // 成功与「取消后吞掉错误」两条路径都会走到这里：补输出解码器尾部与残留 ANSI。
        // 只有已经记录过错误时才跳过（避免在错误结果上再叠加一次失败）。
        if state.error.is_none()
            && let Err(error) = state.flush(on_chunk.as_ref())
        {
            state.error = Some(error);
        }
    }

    let mut state = lock(&state);
    // 输出处理错误优先于「取消也返回输出」：一旦这里返回 Err，调用方拿不到 BashResult。
    if let Some(error) = state.error.take() {
        state.finish_temp_file();
        return Err(Box::new(error));
    }

    // 滚动缓冲的拼接结果：可能已经丢弃过最旧的块（见 append_text 的注释）。
    let full_output = state.output_chunks.concat();
    let truncation_result = truncate_tail(&full_output, &TruncationOptions::default());
    if truncation_result.truncated
        && let Err(error) = state.ensure_temp_file()
    {
        // 只有按行数截断、且此前从未超原始字节阈值时才需要在这里补建文件。
        state.finish_temp_file();
        return Err(Box::new(error));
    }
    state.finish_temp_file();
    // cancelled 取执行结束时的 token 状态；取消时丢弃退出码。
    let cancelled = signal.as_ref().is_some_and(CancellationToken::is_cancelled);

    Ok(BashResult {
        // output 始终来自滚动缓冲；截断时用 truncate_tail 的尾部结果。
        output: if truncation_result.truncated {
            truncation_result.content
        } else {
            full_output
        },
        exit_code: if cancelled { None } else { exit_code },
        cancelled,
        truncated: truncation_result.truncated,
        full_output_path: state
            .temp_file_path
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned()),
        execution_status,
    })
}
