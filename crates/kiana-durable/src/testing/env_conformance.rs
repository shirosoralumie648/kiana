//! `testing/env-conformance.ts`：与 runner 无关的 `ExecutionEnv` 一致性用例。
//!
//! 用例名与顺序逐字来自 TS；`timeoutMs` 只在 watch 用例上出现（30 000）。
//!
//! # 为什么存在
//!
//! [`ExecutionEnv`] 是文件系统加 shell 的抽象（`crate::env`）。本地 Node、远程 env-client
//! 等实现必须表现出相同的可观察行为；这些用例就是这份行为的可执行清单。本模块只构造用例，
//! 不依赖任何测试框架：环境与断言都由调用方注入。
//!
//! # 调用方与执行模型
//!
//! - [`crate::testing::runner::register_env_conformance`] 把用例数组注册到
//!   `StorageConformanceRunner`（Vitest/Jest 形状的 `describe`/`it`）。
//! - 测试直接调用 [`create_env_conformance`] 得到数组并逐个执行，例如
//!   `crates/kiana-durable/tests/env_node_conformance.rs` 与 `crates/kiana-env-client/tests/`
//!   下的套件。
//! - 每个用例的 `run` 通过 `EnvConformanceOptions::with_env` 申请环境。provider 必须恰好
//!   调用并等待传入的回调一次；只调用而不 await，回调返回的用例 future 会被丢弃，用例体
//!   静默不执行而 `run` 仍以 `()` 结束。它还必须交出 `cwd` 为全新、空、可写目录的环境。
//!
//! # 用例的组织
//!
//! 返回值是一个有序的 `Vec<EnvConformanceCase>`：22 个无条件用例覆盖二进制读取器、目录
//! 读取器、watch 与 exec；`list_files` 打开时追加 4 个 listFiles 用例，`symlinks` 打开时
//! 追加 2 个符号链接用例，二者同时打开再追加 1 个“listFiles 不跟随符号链接”的用例。
//! 用例名与顺序由 `crates/kiana-durable/tests/storage_conformance.rs` 的 `ENV_CASE_NAMES`
//! 锁定，改名或调整顺序会让该测试失败。
//!
//! # 失败模式
//!
//! 用例体的返回类型是 `()`，没有错误通道：`ok_file`/`ok_exec` 像 TS 的 `getOrThrow` 一样
//! 用 panic 展开失败；断言如何报告失败由注入的 `StorageConformanceAssertions` 实现决定
//! （仓库里的测试用 `PanicAssertions`，它 panic）。`timeout_ms` 只是元数据，本模块不设超时，
//! 由执行方决定如何应用。watch 用例每次 `expectChange` 最多轮询等待 3 秒，超时会 panic 并
//! 带出这一步收到的全部变化。
//!
//! # 注意事项
//!
//! - 用例假定 shell 能执行 POSIX 脚本（`printf`、`while`、`exit`、`ln -s`），默认
//!   `["sh", "-c"]`；不能创建符号链接的环境应传 `symlinks: Some(false)`，没有 listFiles
//!   能力的实现应保持 `list_files` 的默认值 false。
//! - 长度断言按字节计算：`encoded_len` 是 UTF-8 字节数，`newline_count` 数的是 `\n` 字节。
//! - 环境不由本模块创建或清理；创建与回收都发生在调用方的 provider 里。

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::FutureExt;
use kiana_chord::context::{
    AbortSignal, BACKGROUND_CONTEXT, Context, ContextRef, with_abort_signal,
};
use serde_json::{Value, json};

use crate::env::decode::{decode_whole, range_decoder};
use crate::env::{
    DirPage, ExecutionEnv, ExecutionError, ExecutionErrorCode, FileContent, FileError,
    FileErrorCode, FileInfo, FileKind, FileListing, ListFilesOptions, OpenBinaryReaderOptions,
    ShellCommand, ShellExecOptions, ShellExecResult, ShellOutputInfo, ShellOutputStream,
    ShellOutputWindow, WatchChange, WatchExclude, WatchMode, WatchTarget,
};
use crate::testing::types::{
    CaseFuture, EnvConformanceCase, EnvConformanceOptions, EnvUse, StorageConformanceAssertions,
};

/// 所有用例传给环境实现的共享上下文：后台上下文，不带取消信号。
fn context() -> &'static dyn Context {
    &**BACKGROUND_CONTEXT
}

/// `with_abort_signal` 需要的拥有型上下文；与 [`context`] 指向同一个后台上下文。
fn background() -> ContextRef {
    ContextRef::clone(&*BACKGROUND_CONTEXT)
}

/// 已中止的上下文：把 abort 过的信号挂到后台上下文上，用来断言操作返回 `aborted` 错误码。
fn aborted_context() -> ContextRef {
    let signal = AbortSignal::new();
    signal.abort_without_reason();
    with_abort_signal(signal, background())
}

/// `getOrThrow`：失败时以错误消息 panic。
///
/// 用例的返回类型是 `()`，没有错误返回值；`Result` 只能在这里解包，失败即 panic。
fn ok_file<T>(result: Result<T, FileError>) -> T {
    match result {
        Ok(value) => value,
        Err(error) => panic!("{error}"),
    }
}

/// `ok_file` 的 exec 版本：把 `ExecutionError` 转成 panic。
fn ok_exec(result: Result<ShellExecResult, ExecutionError>) -> ShellExecResult {
    match result {
        Ok(value) => value,
        Err(error) => panic!("{error}"),
    }
}

/// `FileErrorCode` 对应的 TS 字符串（如 `"not_found"`）；断言用它和 TS 用例逐字对齐。
fn file_error_code(code: FileErrorCode) -> &'static str {
    match code {
        FileErrorCode::Aborted => "aborted",
        FileErrorCode::NotFound => "not_found",
        FileErrorCode::PermissionDenied => "permission_denied",
        FileErrorCode::NotDirectory => "not_directory",
        FileErrorCode::IsDirectory => "is_directory",
        FileErrorCode::Invalid => "invalid",
        FileErrorCode::NotSupported => "not_supported",
        FileErrorCode::Unknown => "unknown",
    }
}

/// `ExecutionErrorCode` 对应的 TS 字符串（如 `"timeout"`）；断言用它和 TS 用例逐字对齐。
fn execution_error_code(code: ExecutionErrorCode) -> &'static str {
    match code {
        ExecutionErrorCode::Aborted => "aborted",
        ExecutionErrorCode::Timeout => "timeout",
        ExecutionErrorCode::ShellUnavailable => "shell_unavailable",
        ExecutionErrorCode::SpawnError => "spawn_error",
        ExecutionErrorCode::CallbackError => "callback_error",
        ExecutionErrorCode::Unknown => "unknown",
    }
}

/// TS `errorCode(result)`：成功没有值（JSON null，对应 TS 的 undefined），失败给出错误码字符串。
fn file_code<T>(result: &Result<T, FileError>) -> Value {
    match result {
        Ok(_) => Value::Null,
        Err(error) => json!(file_error_code(error.code)),
    }
}

/// `file_code` 的 exec 版本。
fn exec_code(result: &Result<ShellExecResult, ExecutionError>) -> Value {
    match result {
        Ok(_) => Value::Null,
        Err(error) => json!(execution_error_code(error.code)),
    }
}

/// `listFiles` 的 `<kind letter> <path>` 行所用的字母（`f`/`d`/`l`）。
fn file_kind_letter(kind: FileKind) -> &'static str {
    match kind {
        FileKind::File => "f",
        FileKind::Directory => "d",
        FileKind::Symlink => "l",
    }
}

/// TS `FileInfo["kind"]`：`"file"`/`"directory"`/`"symlink"`。与 [`file_kind_letter`] 不同，
/// 这是 `FileInfo` 结构本身的判别值。
fn file_kind_name(kind: FileKind) -> &'static str {
    match kind {
        FileKind::File => "file",
        FileKind::Directory => "directory",
        FileKind::Symlink => "symlink",
    }
}

/// `FileInfo` 的 camelCase JSON 视图。字段名与 TS 的 `FileInfo` 一致，供部分/深层
/// 相等断言使用。
fn file_info_json(info: &FileInfo) -> Value {
    json!({
        "name": info.name,
        "path": info.path,
        "kind": file_kind_name(info.kind),
        "size": info.size,
        "mtimeMs": info.mtime_ms,
    })
}

/// `DirPage` 的 JSON 视图（`entries` 与 `done`）。
fn page_json(page: &DirPage) -> Value {
    json!({
        "entries": page.entries.iter().map(file_info_json).collect::<Vec<_>>(),
        "done": page.done,
    })
}

/// `listFiles` 结果的 JSON 视图：每个条目写成 `<kind letter> <path>`，另带 `truncated`。
fn listing_json(listing: &FileListing) -> Value {
    json!({
        "entries": listing
            .entries
            .iter()
            .map(|entry| format!("{} {}", file_kind_letter(entry.kind), entry.path))
            .collect::<Vec<_>>(),
        "truncated": listing.truncated,
    })
}

fn assert_ok(assertions: &dyn StorageConformanceAssertions, value: bool, message: &str) {
    assertions.ok(value, Some(message));
}

fn assert_strict(assertions: &dyn StorageConformanceAssertions, actual: Value, expected: Value) {
    assertions.strict_equal(&actual, &expected);
}

fn assert_deep(assertions: &dyn StorageConformanceAssertions, actual: Value, expected: Value) {
    assertions.deep_equal(&actual, &expected);
}

fn assert_partial(assertions: &dyn StorageConformanceAssertions, actual: Value, expected: Value) {
    assertions.partial_deep_equal(&actual, &expected);
}

/// `new TextDecoder("utf-8", { ignoreBOM: from > 0 }).decode(bytes.subarray(from, to))`。
///
/// 区间先钳到 `[0, bytes.len()]`，对应 `subarray` 对越界下标的截断。`from > 0` 时按
/// `ignoreBOM: true` 流式解码，保留切片里的 U+FEFF；`from == 0` 时用 [`decode_whole`]，
/// 与 TS 的 `ignoreBOM: false` 一样丢弃文件开头的 BOM。`scan_lines` 的选区文本断言依赖
/// 这个区别。
fn range_text(bytes: &[u8], from: usize, to: usize) -> String {
    let from = from.min(bytes.len());
    let to = to.clamp(from, bytes.len());
    let slice = &bytes[from..to];
    if from > 0 {
        let mut decoder = range_decoder();
        let mut text = decoder.decode(Some(slice));
        text.push_str(&decoder.decode(None));
        text
    } else {
        decode_whole(slice)
    }
}

/// `TextEncoder().encode(text).length`：Rust 的 `str::len()` 就是 UTF-8 字节数。
fn encoded_len(text: &str) -> usize {
    text.len()
}

/// `text.split("\n").length - 1`：等价于统计 `\n` 的个数。
fn newline_count(text: &str) -> usize {
    text.matches('\n').count()
}

/// 以 `max_entries` 为页大小读完整个目录，返回所有页与是否到达结尾。
///
/// 最多读 1000 页：实现若一直不置 `done`，这里返回 `done = false` 而不是无限循环，随后由
/// 调用用例的断言失败。到达结尾与页数耗尽两条路径都会 `close` 读取器。
async fn read_all(
    env: &dyn ExecutionEnv,
    path: &str,
    max_entries: usize,
) -> (Vec<Vec<FileInfo>>, bool) {
    let reader = ok_file(env.open_dir_reader(path, context()).await);
    let mut pages = Vec::new();
    for _ in 0..1000 {
        let next = ok_file(reader.next(max_entries, context()).await);
        pages.push(next.entries);
        if next.done {
            reader.close(context()).await;
            return (pages, true);
        }
    }
    reader.close(context()).await;
    (pages, false)
}

/// TS 的 `if (env.listFiles === undefined) throw new Error("The environment does not implement listFiles")`。
///
/// Rust 的 [`ExecutionEnv`] 把 `list_files` 定义成带默认实现的 trait 方法，不存在“成员缺失”
/// 这种状态；默认实现固定返回 `not_supported` + `"listFiles is not supported by this environment"`。
/// 这里按该默认实现的特征识别“未实现”，让失败消息保持当前稳定契约（ARCHITECTURE §4.9 冻结了
/// `env/*` 的签名，不能给 trait 增加能力查询方法）。
///
/// 识别条件同时看错误码与固定消息：实现若返回别种文本的 `not_supported`，会按普通错误原样
/// panic（消息不同），不会误报为“未实现”。文件底部的单元测试锁定了这两个分支。
fn list_files_or_panic<T>(result: Result<T, FileError>) -> T {
    match result {
        Ok(value) => value,
        Err(error)
            if error.code == FileErrorCode::NotSupported
                && error.message == "listFiles is not supported by this environment" =>
        {
            panic!("The environment does not implement listFiles")
        }
        Err(error) => panic!("{error}"),
    }
}

/// 一份 `<kind letter> <path>` 形式的列表（`f`/`d`/`l`）与截断标志。
///
/// 通过 `list_files_or_panic` 保持 TS 对“环境未实现 listFiles”的报错文本。
async fn listed(env: &dyn ExecutionEnv, path: &str, options: Option<&ListFilesOptions>) -> Value {
    let listing = list_files_or_panic(env.list_files(path, options, context()).await);
    listing_json(&listing)
}

/// 取出列表 JSON 的 `entries` 数组；缺失时当空数组，供只需要 `entries` 数组（不检查
/// `truncated`）的断言使用。数组保持 listing 中的顺序，因此既可用于成员检查，也可用于
/// 依赖顺序的整表 `assert_deep` 比较。
fn entries_of(listing: &Value) -> Vec<Value> {
    listing
        .get("entries")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// `WatchChange` 的 JSON 视图，用于失败消息与“关闭后没有变化”的断言。
///
/// `Paths` → `{"paths": [...]}`，`Overflow` → `{"overflow": true}`，`Error` 见下方注释。
fn change_json(change: &WatchChange) -> Value {
    match change {
        WatchChange::Paths(paths) => json!({ "paths": paths }),
        WatchChange::Overflow => json!({ "overflow": true }),
        // `JSON.stringify(new Error(...))` 是 `{}`（message 等属性不可枚举）。
        WatchChange::Error(_) => json!({}),
    }
}

/// 一次变化是否报告了 `path`：overflow，或某条上报路径是它的祖先。
///
/// `path` 由调用方通过 `absolute` 得到；这里是纯字符串前缀比较，不做路径规范化（`..`、
/// 结尾斜杠等都不会被处理）。`Overflow` 视为覆盖任意路径，`Error` 不覆盖。
fn covers(change: &WatchChange, path: &str) -> bool {
    match change {
        WatchChange::Overflow => true,
        WatchChange::Error(_) => false,
        WatchChange::Paths(reported) => reported.iter().any(|reported| {
            path == reported
                || path.starts_with(&format!("{reported}/"))
                || path.starts_with(&format!("{reported}\\"))
        }),
    }
}

/// `expectChange` 与 `absolute` 的辅助面。
///
/// `changes` 与 watch 回调共享同一个 `Arc<Mutex<Vec<_>>>`；回调只负责把变化 push 进来，
/// 用例体通过 `expect_change` 轮询查看新增部分。
struct WatchSession<'a> {
    env: &'a dyn ExecutionEnv,
    changes: Arc<Mutex<Vec<WatchChange>>>,
}

impl<'a> WatchSession<'a> {
    async fn absolute(&self, path: &str) -> String {
        ok_file(self.env.absolute_path(path, context()).await)
    }

    /// 先 `absolute(path)` 再记录起点，执行 `change` 后最多等三秒。
    ///
    /// 只检查起点之后追加的变化，避免上一步的残留变化让本次期望提前通过。`change` 执行完
    /// 后每 20 ms 轮询一次；超时的 panic 消息里带出起点之后的全部变化 JSON。
    async fn expect_change<F>(&self, path: &str, change: F)
    where
        F: Future<Output = ()>,
    {
        let target = self.absolute(path).await;
        let from = self.changes.lock().unwrap().len();
        change.await;
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            {
                let changes = self.changes.lock().unwrap();
                if changes[from..].iter().any(|entry| covers(entry, &target)) {
                    return;
                }
            }
            if Instant::now() > deadline {
                let changes = self.changes.lock().unwrap();
                let tail = changes[from..].iter().map(change_json).collect::<Vec<_>>();
                panic!("No change reported {target}; got {}", Value::Array(tail));
            }
            // 轮询而不是等待通知：WatchCallback 是同步 `Fn`，没有可 await 的通知通道。
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

/// `watching` 的用例体类型：装箱后借用调用方 `env` 的 future。
///
/// `Send` 与外层用例 future 的要求一致（`EnvConformanceCase::run` 返回的 future 必须 `Send`）。
type WatchRun<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

/// 注册 watcher，运行 `run`，最后关闭 watcher。
///
/// 变化回调把 `WatchChange` push 进共享的 `Mutex<Vec<_>>`（回调类型要求 `Send + Sync`，
/// 实现可能从其他线程调用）。`run` 用 `catch_unwind` 包住：panic 被捕获后先执行 `close`，
/// 再用 `resume_unwind` 把原始载荷重新抛出——TS 用 `try/finally`，Rust 的 panic 不会自动
/// 执行后续语句。`AssertUnwindSafe` 表示接受共享状态在 panic 后可能不完整（此时用例即将
/// 失败）。
async fn watching<'a, F>(env: &'a dyn ExecutionEnv, targets: &[WatchTarget], run: F)
where
    F: FnOnce(WatchSession<'a>) -> WatchRun<'a>,
{
    let changes: Arc<Mutex<Vec<WatchChange>>> = Arc::new(Mutex::new(Vec::new()));
    let callback_changes = changes.clone();
    let watcher = ok_file(
        env.watch(
            targets,
            Arc::new(move |change: WatchChange| {
                callback_changes.lock().unwrap().push(change);
            }),
            context(),
        )
        .await,
    );
    let session = WatchSession { env, changes };
    let outcome = std::panic::AssertUnwindSafe(run(session))
        .catch_unwind()
        .await;
    watcher.close(context()).await;
    if let Err(payload) = outcome {
        std::panic::resume_unwind(payload);
    }
}

/// 一次 `exec` 的结果，以及按流拼接后的回调输出。
struct ExecCollected {
    result: Result<ShellExecResult, ExecutionError>,
    stdout: String,
    stderr: String,
}

/// 执行命令并通过 `on_output` 收集 stdout/stderr。
///
/// 只设置 `cwd` 与 `on_output`，`window`/`spill` 用默认值（都不启用），因此回调文本拼起来
/// 就是完整输出。两个流分别追加；本辅助函数不保留跨流的相对顺序。
async fn exec_collect(
    env: &dyn ExecutionEnv,
    command: ShellCommand,
    cwd: Option<&str>,
) -> ExecCollected {
    let output: Arc<Mutex<(String, String)>> = Arc::new(Mutex::new((String::new(), String::new())));
    let captured = output.clone();
    let options = ShellExecOptions {
        cwd: cwd.map(str::to_owned),
        on_output: Some(Arc::new(
            move |text: &str, _context: &dyn Context, info: &ShellOutputInfo| {
                let mut output = captured.lock().unwrap();
                match info.stream {
                    ShellOutputStream::Stdout => output.0.push_str(text),
                    ShellOutputStream::Stderr => output.1.push_str(text),
                }
            },
        )),
        ..ShellExecOptions::default()
    };
    let result = env.exec(command, Some(&options), context()).await;
    let output = output.lock().unwrap().clone();
    ExecCollected {
        result,
        stdout: output.0,
        stderr: output.1,
    }
}

/// 构造 `Argv([shell..., script])`，等价 TS 的 `[...shell, script]`（即 `sh -c <script>`）。
///
/// 需要给脚本传位置参数时，调用方继续往 argv 末尾 push（它们会成为 `$0`、`$1`……）。
fn shell_command(shell: &[String], script: &str) -> ShellCommand {
    let mut argv = shell.to_vec();
    argv.push(script.to_owned());
    ShellCommand::Argv(argv)
}

/// 把一个 `Fn(env, assert) -> Future` 包装成可重复调用的 [`EnvConformanceCase`]。
///
/// 每次调用 `run` 都重新通过 `options.with_env` 申请环境；provider 的契约（恰好调用并等待
/// 传入的 `use` 回调一次）由调用方保证——只调用不 await 会让用例 future 被丢弃、用例体静默
/// 不执行。`assertions`、`with_env` 与测试函数都克隆进 `'static` future。这里不应用
/// `timeout_ms`——它只是给执行方的元数据。
fn conformance_case<F, Fut>(
    options: &EnvConformanceOptions,
    name: &str,
    timeout_ms: Option<u64>,
    test: F,
) -> EnvConformanceCase
where
    F: Fn(Arc<dyn ExecutionEnv>, Arc<dyn StorageConformanceAssertions>) -> Fut
        + Send
        + Sync
        + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let assertions = options.assertions.clone();
    let with_env = options.with_env.clone();
    let test = Arc::new(test);
    EnvConformanceCase {
        name: name.to_owned(),
        timeout_ms,
        run: Arc::new(move || {
            let with_env = with_env.clone();
            let assertions = assertions.clone();
            let test = test.clone();
            Box::pin(async move {
                let use_assertions = assertions.clone();
                let use_env: EnvUse = Arc::new(move |env: Arc<dyn ExecutionEnv>| {
                    let test = test.clone();
                    let assertions = use_assertions.clone();
                    let future: CaseFuture = Box::pin(async move { test(env, assertions).await });
                    future
                });
                with_env(use_env).await;
            })
        }),
    }
}

/// 需要一个 shell 命令的用例：把 `shell`（默认 `["sh", "-c"]`）通过 Arc 传进用例体。
///
/// 只负责多传一个参数；其余包装逻辑与 `conformance_case` 相同。
fn shell_conformance_case<F, Fut>(
    options: &EnvConformanceOptions,
    name: &str,
    wait_ms: Option<u64>,
    shell: Arc<Vec<String>>,
    test: F,
) -> EnvConformanceCase
where
    F: Fn(Arc<Vec<String>>, Arc<dyn ExecutionEnv>, Arc<dyn StorageConformanceAssertions>) -> Fut
        + Send
        + Sync
        + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    conformance_case(options, name, wait_ms, move |env, assert| {
        let shell = shell.clone();
        test(shell, env, assert)
    })
}

/// `writeAll`：依次写入空文件（父目录由环境的 `writeFile` 负责创建）。
///
/// 内容无关紧要，用例只关心路径布局；第一个文件失败就 panic，后续文件不再写。
async fn write_all(env: &dyn ExecutionEnv, files: &[&str]) {
    for file in files {
        ok_file(
            env.write_file(file, FileContent::Text(String::new()), context())
                .await,
        );
    }
}

/// `createEnvConformance(options)`：构造全部用例，不执行任何一项。
///
/// 输入 `options` 提供断言集合、环境 provider（每个用例调用一次，交出 `cwd` 为全新空可写
/// 目录的环境），以及三个开关：`shell`（默认 `["sh", "-c"]`）、`symlinks`（默认 true）、
/// `list_files`（默认 false，因为它是可选能力；`FileSystem::list_files` 是带 `not_supported`
/// 默认实现的可选能力，打开开关前需确认环境实现了它，否则相关 listFiles 用例会失败）。
///
/// 返回的用例顺序固定：22 个核心用例之后，按开关依次追加 listFiles、符号链接用例。
/// `timeout_ms` 只在 watch 用例上是 `Some(30_000)`，其余为 `None`；本函数不应用超时。
/// 每个 `run` 都可重复调用，每次都会重新走 `with_env` 申请环境。
pub fn create_env_conformance(options: &EnvConformanceOptions) -> Vec<EnvConformanceCase> {
    // TS 的默认值：shell = ["sh", "-c"]、symlinks = true、listFiles = false。
    let shell = Arc::new(
        options
            .shell
            .clone()
            .unwrap_or_else(|| vec!["sh".to_owned(), "-c".to_owned()]),
    );
    let symlinks = options.symlinks.unwrap_or(true);
    let list_files = options.list_files.unwrap_or(false);

    // 22 个无条件用例，顺序即 TS 中 `it` 的声明顺序；测试用 ENV_CASE_NAMES 锁定。
    let mut cases: Vec<EnvConformanceCase> = vec![
        conformance_case(
            options,
            "binary reader reads byte ranges of the opened file",
            None,
            |env, assert| async move {
                let assert = assert.as_ref();
                ok_file(
                    env.write_file(
                        "data.txt",
                        FileContent::Text("hello world".to_owned()),
                        context(),
                    )
                    .await,
                );
                let reader = ok_file(env.open_binary_reader("data.txt", None, context()).await);
                let info = ok_file(reader.info(context()).await);
                assert_partial(
                    assert,
                    file_info_json(&info),
                    json!({
                        "name": "data.txt",
                        "kind": "file",
                        "size": 11,
                    }),
                );
                assert_strict(
                    assert,
                    json!(decode_whole(&ok_file(reader.read(0, 5, context()).await))),
                    json!("hello"),
                );
                assert_strict(
                    assert,
                    json!(decode_whole(&ok_file(reader.read(6, 100, context()).await))),
                    json!("world"),
                );
                assert_strict(
                    assert,
                    json!(ok_file(reader.read(11, 4, context()).await).len()),
                    json!(0),
                );
                assert_strict(
                    assert,
                    json!(ok_file(reader.read(50, 1, context()).await).len()),
                    json!(0),
                );
                assert_strict(
                    assert,
                    json!(ok_file(reader.read(3, 0, context()).await).len()),
                    json!(0),
                );
                // TS 还断言 `read(-1, 1)` 与 `read(0, 1.5)` 返回 `invalid`；Rust 的
                // `read(offset: u64, length: usize)`（`env/*` 在 ARCHITECTURE §4.9 中冻结）在类型上
                // 排除了负偏移与非整数长度，这两种输入无法构造，故无对应断言。
                // 中止上下文则是 `aborted`（`read(0, 1, abortedContext())`）。
                assert_strict(
                    assert,
                    file_code(&reader.read(0, 1, aborted_context().as_ref()).await),
                    json!("aborted"),
                );
                // 关闭是幂等的（TS 也连续 close 两次）；关闭后 read/info 都是 invalid。
                reader.close(context()).await;
                reader.close(context()).await;
                assert_strict(
                    assert,
                    file_code(&reader.read(0, 1, context()).await),
                    json!("invalid"),
                );
                assert_strict(
                    assert,
                    file_code(&reader.info(context()).await),
                    json!("invalid"),
                );
            },
        ),
        conformance_case(
            options,
            "binary reader scans lines like decoding the whole file",
            None,
            |env, assert| async move {
                let assert = assert.as_ref();
                // 一个字节序标记、换行前的非法序列、一个空行、后面的 U+FEFF，以及没有结尾换行。
                let bytes: Vec<u8> = vec![
                    0xef, 0xbb, 0xbf, 0x61, 0x0a, 0xe2, 0x82, 0x0a, 0x0a, 0xef, 0xbb, 0xbf, 0x62,
                    0x0a, 0xc3, 0xa9,
                ];
                ok_file(
                    env.write_file("lines.txt", FileContent::Bytes(bytes.clone()), context())
                        .await,
                );
                let lines: Vec<String> = decode_whole(&bytes)
                    .split('\n')
                    .map(str::to_owned)
                    .collect();
                let reader = ok_file(env.open_binary_reader("lines.txt", None, context()).await);
                // 覆盖整文件、从中间行开始、跨行、越界结束行与末行无换行等选区。
                let ranges: [(usize, Option<usize>); 6] = [
                    (0, None),
                    (0, Some(1)),
                    (1, Some(3)),
                    (2, Some(3)),
                    (3, None),
                    (4, Some(9)),
                ];
                for (start_line, end_line) in ranges {
                    let scan = ok_file(
                        reader
                            .scan_lines(
                                crate::env::LineRange {
                                    start_line: start_line as u64,
                                    end_line: end_line.map(|value| value as u64),
                                },
                                context(),
                            )
                            .await,
                    );
                    // TS 的 `lines.slice(startLine, endLine)` 会对越界的结束行截断。
                    let end = end_line.unwrap_or(lines.len()).min(lines.len());
                    let selected = &lines[start_line..end];
                    let selected_text = selected.join("\n");
                    assert_strict(assert, json!(scan.newlines), json!(lines.len() - 1));
                    assert_strict(
                        assert,
                        json!(range_text(&bytes, scan.start, scan.end)),
                        json!(selected_text),
                    );
                    assert_strict(
                        assert,
                        json!(scan.selected_bytes),
                        json!(encoded_len(&selected_text)),
                    );
                    assert_strict(
                        assert,
                        json!(range_text(&bytes, scan.start, scan.first_line_end)),
                        json!(lines[start_line]),
                    );
                    assert_strict(
                        assert,
                        json!(scan.first_line_bytes),
                        json!(encoded_len(&lines[start_line])),
                    );
                }
                // 起点超过行数时返回文件末尾的空区间，而不是错误。
                let scan = ok_file(
                    reader
                        .scan_lines(
                            crate::env::LineRange {
                                start_line: 9,
                                end_line: None,
                            },
                            context(),
                        )
                        .await,
                );
                assert_partial(
                    assert,
                    json!({
                        "start": scan.start,
                        "end": scan.end,
                        "selectedBytes": scan.selected_bytes,
                    }),
                    json!({
                        "start": bytes.len(),
                        "end": bytes.len(),
                        "selectedBytes": 0,
                    }),
                );
                // 起止相同的空选区是 invalid。
                assert_strict(
                    assert,
                    file_code(
                        &reader
                            .scan_lines(
                                crate::env::LineRange {
                                    start_line: 2,
                                    end_line: Some(2),
                                },
                                context(),
                            )
                            .await,
                    ),
                    json!("invalid"),
                );
                reader.close(context()).await;
            },
        ),
        conformance_case(
            options,
            "binary reader keeps reading the file it opened after a rename",
            None,
            |env, assert| async move {
                let assert = assert.as_ref();
                ok_file(
                    env.write_file("a.txt", FileContent::Text("one".to_owned()), context())
                        .await,
                );
                let reader = ok_file(env.open_binary_reader("a.txt", None, context()).await);
                ok_file(env.rename_file("a.txt", "b.txt", context()).await);
                ok_file(
                    env.write_file("a.txt", FileContent::Text("two".to_owned()), context())
                        .await,
                );
                assert_strict(
                    assert,
                    json!(decode_whole(&ok_file(reader.read(0, 10, context()).await))),
                    json!("one"),
                );
                reader.close(context()).await;
            },
        ),
        conformance_case(
            options,
            "binary reader refuses directories, missing files and aborted opens",
            None,
            |env, assert| async move {
                let assert = assert.as_ref();
                ok_file(env.create_dir("dir", None, context()).await);
                ok_file(
                    env.write_file("file.txt", FileContent::Text("x".to_owned()), context())
                        .await,
                );
                assert_strict(
                    assert,
                    file_code(&env.open_binary_reader("dir", None, context()).await),
                    json!("is_directory"),
                );
                assert_strict(
                    assert,
                    file_code(&env.open_binary_reader("missing.txt", None, context()).await),
                    json!("not_found"),
                );
                assert_strict(
                    assert,
                    file_code(
                        &env.open_binary_reader("file.txt", None, aborted_context().as_ref())
                            .await,
                    ),
                    json!("aborted"),
                );
            },
        ),
        conformance_case(
            options,
            "directory reader pages every entry exactly once",
            None,
            |env, assert| async move {
                let assert = assert.as_ref();
                let names = ["a.txt", "b.txt", "c.txt", "d.txt", "e.txt"];
                for name in names {
                    ok_file(
                        env.write_file(name, FileContent::Text(name.to_owned()), context())
                            .await,
                    );
                }
                ok_file(env.create_dir("sub", None, context()).await);
                let (pages, done) = read_all(env.as_ref(), ".", 2).await;
                assert_ok(assert, done, "directory reader reached the end");
                for page in &pages {
                    assert_ok(assert, page.len() <= 2, "page within maxEntries");
                }
                // 目录读取器只承诺文件系统顺序；排序后比较，只检查条目不重不漏。
                let entries: Vec<FileInfo> = pages.into_iter().flatten().collect();
                let mut sorted: Vec<String> =
                    entries.iter().map(|entry| entry.name.clone()).collect();
                sorted.sort();
                let mut expected: Vec<String> =
                    names.iter().map(|name| (*name).to_owned()).collect();
                expected.push("sub".to_owned());
                expected.sort();
                assert_deep(assert, json!(sorted), json!(expected));
                assert_strict(
                    assert,
                    json!(
                        entries
                            .iter()
                            .find(|entry| entry.name == "sub")
                            .map(|entry| file_kind_name(entry.kind))
                    ),
                    json!("directory"),
                );
                assert_partial(
                    assert,
                    entries
                        .iter()
                        .find(|entry| entry.name == "a.txt")
                        .map(file_info_json)
                        .unwrap_or(Value::Null),
                    json!({ "kind": "file", "size": 5 }),
                );
            },
        ),
        conformance_case(
            options,
            "directory reader reports the end and refuses use after close",
            None,
            |env, assert| async move {
                let assert = assert.as_ref();
                ok_file(env.create_dir("empty", None, context()).await);
                // 空目录上重复 next 都得到 done；close 幂等，关闭后 next 变 invalid。
                let reader = ok_file(env.open_dir_reader("empty", context()).await);
                assert_deep(
                    assert,
                    page_json(&ok_file(reader.next(10, context()).await)),
                    json!({ "entries": [], "done": true }),
                );
                assert_deep(
                    assert,
                    page_json(&ok_file(reader.next(10, context()).await)),
                    json!({ "entries": [], "done": true }),
                );
                assert_strict(
                    assert,
                    file_code(&reader.next(0, context()).await),
                    json!("invalid"),
                );
                assert_strict(
                    assert,
                    file_code(&reader.next(1, aborted_context().as_ref()).await),
                    json!("aborted"),
                );
                reader.close(context()).await;
                reader.close(context()).await;
                assert_strict(
                    assert,
                    file_code(&reader.next(1, context()).await),
                    json!("invalid"),
                );
            },
        ),
        conformance_case(
            options,
            "directory reader refuses missing paths and files",
            None,
            |env, assert| async move {
                let assert = assert.as_ref();
                ok_file(
                    env.write_file("file.txt", FileContent::Text("x".to_owned()), context())
                        .await,
                );
                assert_strict(
                    assert,
                    file_code(&env.open_dir_reader("missing", context()).await),
                    json!("not_found"),
                );
                assert_strict(
                    assert,
                    file_code(&env.open_dir_reader("file.txt", context()).await),
                    json!("not_directory"),
                );
                assert_strict(
                    assert,
                    file_code(&env.open_dir_reader(".", aborted_context().as_ref()).await),
                    json!("aborted"),
                );
            },
        ),
        conformance_case(
            options,
            "directory reader skips entries removed during enumeration",
            None,
            |env, assert| async move {
                let assert = assert.as_ref();
                ok_file(env.create_dir("dir", None, context()).await);
                for name in ["x", "y", "z"] {
                    ok_file(
                        env.write_file(
                            &format!("dir/{name}"),
                            FileContent::Text(name.to_owned()),
                            context(),
                        )
                        .await,
                    );
                }
                let reader = ok_file(env.open_dir_reader("dir", context()).await);
                for name in ["x", "y", "z"] {
                    ok_file(env.remove(&format!("dir/{name}"), None, context()).await);
                }
                let mut entries: Vec<FileInfo> = Vec::new();
                for _ in 0..10 {
                    let next = ok_file(reader.next(10, context()).await);
                    entries.extend(next.entries);
                    if next.done {
                        break;
                    }
                }
                assert_deep(
                    assert,
                    json!(entries.iter().map(file_info_json).collect::<Vec<_>>()),
                    json!([]),
                );
                reader.close(context()).await;
            },
        ),
        // 以下 watch 用例统一带 30 000 ms 超时：每步最多轮询等待 3 秒，而测试框架的默认
        // 超时通常更短（TS 的 watchCase 同样显式传 30 000）。
        conformance_case(
            options,
            "watch reports a missing file's creation, changes, replacement and removal",
            Some(30_000),
            |env, _assert| async move {
                let targets = vec![WatchTarget {
                    path: "AGENTS.md".to_owned(),
                    recursive: None,
                    exclude: None,
                }];
                watching(env.as_ref(), &targets, |session| {
                    Box::pin(async move {
                        let env = session.env;
                        session
                            .expect_change("AGENTS.md", async {
                                ok_file(
                                    env.write_file(
                                        "AGENTS.md",
                                        FileContent::Text("one".to_owned()),
                                        context(),
                                    )
                                    .await,
                                );
                            })
                            .await;
                        session
                            .expect_change("AGENTS.md", async {
                                ok_file(
                                    env.write_file(
                                        "AGENTS.md",
                                        FileContent::Text("two!".to_owned()),
                                        context(),
                                    )
                                    .await,
                                );
                            })
                            .await;
                        // 编辑器通过把新文件 rename 到旧路径来替换文件。
                        session
                            .expect_change("AGENTS.md", async {
                                ok_file(
                                    env.write_file(
                                        "AGENTS.md.tmp",
                                        FileContent::Text("three".to_owned()),
                                        context(),
                                    )
                                    .await,
                                );
                                ok_file(
                                    env.rename_file("AGENTS.md.tmp", "AGENTS.md", context())
                                        .await,
                                );
                            })
                            .await;
                        session
                            .expect_change("AGENTS.md", async {
                                ok_file(
                                    env.write_file(
                                        "AGENTS.md",
                                        FileContent::Text("four".to_owned()),
                                        context(),
                                    )
                                    .await,
                                );
                            })
                            .await;
                        session
                            .expect_change("AGENTS.md", async {
                                ok_file(env.remove("AGENTS.md", None, context()).await);
                            })
                            .await;
                    })
                })
                .await;
            },
        ),
        conformance_case(
            options,
            "watch reports a missing target whose ancestors are created",
            Some(30_000),
            |env, _assert| async move {
                let targets = vec![WatchTarget {
                    path: "a/b/c/AGENTS.md".to_owned(),
                    recursive: None,
                    exclude: None,
                }];
                watching(env.as_ref(), &targets, |session| {
                    Box::pin(async move {
                        let env = session.env;
                        session
                            .expect_change("a/b/c/AGENTS.md", async {
                                ok_file(
                                    env.write_file(
                                        "a/b/c/AGENTS.md",
                                        FileContent::Text("x".to_owned()),
                                        context(),
                                    )
                                    .await,
                                );
                            })
                            .await;
                    })
                })
                .await;
            },
        ),
        conformance_case(
            options,
            "watch follows directories created together with their contents",
            Some(30_000),
            |env, _assert| async move {
                ok_file(env.create_dir("skills", None, context()).await);
                let targets = vec![WatchTarget {
                    path: "skills".to_owned(),
                    recursive: Some(true),
                    exclude: None,
                }];
                watching(env.as_ref(), &targets, |session| {
                    Box::pin(async move {
                        let env = session.env;
                        // 在任何新目录的 watcher 存在之前写入。
                        session
                            .expect_change("skills/a/b/SKILL.md", async {
                                ok_file(
                                    env.write_file(
                                        "skills/a/b/SKILL.md",
                                        FileContent::Text("one".to_owned()),
                                        context(),
                                    )
                                    .await,
                                );
                            })
                            .await;
                        session
                            .expect_change("skills/a/b/SKILL.md", async {
                                ok_file(
                                    env.write_file(
                                        "skills/a/b/SKILL.md",
                                        FileContent::Text("two!".to_owned()),
                                        context(),
                                    )
                                    .await,
                                );
                            })
                            .await;
                        session
                            .expect_change("skills/a/b/c/SKILL.md", async {
                                ok_file(
                                    env.write_file(
                                        "skills/a/b/c/SKILL.md",
                                        FileContent::Text("deeper".to_owned()),
                                        context(),
                                    )
                                    .await,
                                );
                            })
                            .await;
                    })
                })
                .await;
            },
        ),
        conformance_case(
            options,
            "watch keeps watching a path whose parent is renamed and recreated",
            Some(30_000),
            |env, _assert| async move {
                ok_file(
                    env.write_file(
                        "proj/.kiana/skills/x.md",
                        FileContent::Text("x".to_owned()),
                        context(),
                    )
                    .await,
                );
                let targets = vec![WatchTarget {
                    path: "proj/.kiana/skills".to_owned(),
                    recursive: Some(true),
                    exclude: None,
                }];
                watching(env.as_ref(), &targets, |session| {
                    Box::pin(async move {
                        let env = session.env;
                        session
                            .expect_change("proj/.kiana/skills", async {
                                ok_file(
                                    env.rename_file("proj/.kiana", "proj/old", context()).await,
                                );
                            })
                            .await;
                        session
                            .expect_change("proj/.kiana/skills/y.md", async {
                                ok_file(
                                    env.write_file(
                                        "proj/.kiana/skills/y.md",
                                        FileContent::Text("y".to_owned()),
                                        context(),
                                    )
                                    .await,
                                );
                            })
                            .await;
                        session
                            .expect_change("proj/.kiana/skills/y.md", async {
                                ok_file(
                                    env.write_file(
                                        "proj/.kiana/skills/y.md",
                                        FileContent::Text("yy".to_owned()),
                                        context(),
                                    )
                                    .await,
                                );
                            })
                            .await;
                    })
                })
                .await;
            },
        ),
        conformance_case(
            options,
            "watch skips excluded entries and reports a rename out of them",
            Some(30_000),
            |env, assert| async move {
                let assert = assert.clone();
                ok_file(env.create_dir("skills", None, context()).await);
                let targets = vec![WatchTarget {
                    path: "skills".to_owned(),
                    recursive: Some(true),
                    exclude: Some(WatchExclude {
                        hidden: Some(true),
                        names: Some(vec!["node_modules".to_owned()]),
                    }),
                }];
                watching(env.as_ref(), &targets, |session| {
                    Box::pin(async move {
                        let env = session.env;
                        // 先写被排除的路径；它们在后续变化里不应出现，断言在最后统一检查。
                        ok_file(
                            env.write_file(
                                "skills/node_modules/dep/SKILL.md",
                                FileContent::Text("dep".to_owned()),
                                context(),
                            )
                            .await,
                        );
                        ok_file(
                            env.write_file(
                                "skills/.SKILL.md.tmp",
                                FileContent::Text("draft".to_owned()),
                                context(),
                            )
                            .await,
                        );
                        session
                            .expect_change("skills/SKILL.md", async {
                                ok_file(
                                    env.rename_file(
                                        "skills/.SKILL.md.tmp",
                                        "skills/SKILL.md",
                                        context(),
                                    )
                                    .await,
                                );
                            })
                            .await;
                        let hidden = [
                            session.absolute("skills/node_modules").await,
                            session.absolute("skills/.SKILL.md.tmp").await,
                        ];
                        let changes = session.changes.lock().unwrap();
                        for change in changes.iter() {
                            let paths = match change {
                                WatchChange::Paths(paths) => paths,
                                WatchChange::Overflow | WatchChange::Error(_) => continue,
                            };
                            for path in paths {
                                assert_ok(
                                    assert.as_ref(),
                                    !hidden.iter().any(|excluded| {
                                        path == excluded || path.starts_with(excluded)
                                    }),
                                    &format!("excluded {path}"),
                                );
                            }
                        }
                    })
                })
                .await;
            },
        ),
        conformance_case(
            options,
            "watch keeps recursive coverage where a non-recursive target overlaps",
            Some(30_000),
            |env, _assert| async move {
                ok_file(
                    env.write_file(
                        "skills/a/one.md",
                        FileContent::Text("one".to_owned()),
                        context(),
                    )
                    .await,
                );
                let targets = vec![
                    WatchTarget {
                        path: "skills".to_owned(),
                        recursive: None,
                        exclude: None,
                    },
                    WatchTarget {
                        path: "skills".to_owned(),
                        recursive: Some(true),
                        exclude: None,
                    },
                ];
                watching(env.as_ref(), &targets, |session| {
                    Box::pin(async move {
                        let env = session.env;
                        session
                            .expect_change("skills/a/two.md", async {
                                ok_file(
                                    env.write_file(
                                        "skills/a/two.md",
                                        FileContent::Text("two".to_owned()),
                                        context(),
                                    )
                                    .await,
                                );
                            })
                            .await;
                    })
                })
                .await;
            },
        ),
        conformance_case(
            options,
            "watch follows a directory replaced at the same path",
            Some(30_000),
            |env, _assert| async move {
                ok_file(
                    env.write_file(
                        "skills/a/x.md",
                        FileContent::Text("x".to_owned()),
                        context(),
                    )
                    .await,
                );
                let targets = vec![WatchTarget {
                    path: "skills".to_owned(),
                    recursive: Some(true),
                    exclude: None,
                }];
                watching(env.as_ref(), &targets, |session| {
                    Box::pin(async move {
                        let env = session.env;
                        session
                            .expect_change("skills/a", async {
                                ok_file(env.rename_file("skills/a", "skills-old", context()).await);
                                ok_file(env.create_dir("skills/a", None, context()).await);
                            })
                            .await;
                        session
                            .expect_change("skills/a/y.md", async {
                                ok_file(
                                    env.write_file(
                                        "skills/a/y.md",
                                        FileContent::Text("y".to_owned()),
                                        context(),
                                    )
                                    .await,
                                );
                            })
                            .await;
                        session
                            .expect_change("skills/a/y.md", async {
                                ok_file(
                                    env.write_file(
                                        "skills/a/y.md",
                                        FileContent::Text("yy".to_owned()),
                                        context(),
                                    )
                                    .await,
                                );
                            })
                            .await;
                    })
                })
                .await;
            },
        ),
        conformance_case(
            options,
            "watch stops reporting once closed",
            Some(30_000),
            |env, assert| async move {
                let assert = assert.as_ref();
                let changes: Arc<Mutex<Vec<WatchChange>>> = Arc::new(Mutex::new(Vec::new()));
                let callback_changes = changes.clone();
                let watcher = ok_file(
                    env.watch(
                        &[WatchTarget {
                            path: "file.txt".to_owned(),
                            recursive: None,
                            exclude: None,
                        }],
                        Arc::new(move |change: WatchChange| {
                            callback_changes.lock().unwrap().push(change);
                        }),
                        context(),
                    )
                    .await,
                );
                assert_ok(
                    assert,
                    matches!(watcher.mode(), WatchMode::Native | WatchMode::Polling),
                    "watcher reports its mode",
                );
                // close 幂等，且 FileWatcher 的契约是返回后不再有 on_change 调用；关闭后的
                // 写入不应产生任何变化。等待 300 ms 让可能的异步事件有机会到达。
                watcher.close(context()).await;
                watcher.close(context()).await;
                ok_file(
                    env.write_file("file.txt", FileContent::Text("x".to_owned()), context())
                        .await,
                );
                tokio::time::sleep(Duration::from_millis(300)).await;
                let changes = changes.lock().unwrap();
                assert_deep(
                    assert,
                    json!(changes.iter().map(change_json).collect::<Vec<_>>()),
                    json!([]),
                );
            },
        ),
        shell_conformance_case(
            options,
            "argv exec passes arguments to the program without shell parsing",
            None,
            shell.clone(),
            |shell, env, assert| async move {
                let assert = assert.as_ref();
                // 位置参数经 `sh -c` 的 `$0`/`$1`/`$2` 原样传入；hostile 里的命令替换、反引号
                // 与通配符都不得被求值，否则会创建 pwned 文件。
                let hostile = "it's $(touch pwned) `touch pwned` *; touch pwned";
                let command = shell_command(&shell, r#"printf "%s|%s" "$1" "$2""#);
                let ShellCommand::Argv(mut argv) = command else {
                    unreachable!()
                };
                argv.push("argv0".to_owned());
                argv.push(hostile.to_owned());
                argv.push("a b".to_owned());
                let collected = exec_collect(env.as_ref(), ShellCommand::Argv(argv), None).await;
                assert_strict(assert, json!(ok_exec(collected.result).exit_code), json!(0));
                assert_strict(
                    assert,
                    json!(collected.stdout),
                    json!(format!("{hostile}|a b")),
                );
                assert_strict(
                    assert,
                    json!(ok_file(env.exists("pwned", context()).await)),
                    json!(false),
                );
            },
        ),
        shell_conformance_case(
            options,
            "exec reports the stream of every chunk in both forms",
            None,
            shell.clone(),
            |shell, env, assert| async move {
                let assert = assert.as_ref();
                // Argv 与 Shell 两种命令形式必须产生同样的 stdout/stderr 内容与分流。
                // `exec_collect` 只按流拼接文本、不保留分块边界，所以这里观察不到分块。
                let script = "printf out; printf err >&2; printf more";
                let argv = exec_collect(env.as_ref(), shell_command(&shell, script), None).await;
                assert_strict(assert, json!(ok_exec(argv.result).exit_code), json!(0));
                assert_strict(assert, json!(argv.stdout), json!("outmore"));
                assert_strict(assert, json!(argv.stderr), json!("err"));
                let string =
                    exec_collect(env.as_ref(), ShellCommand::Shell(script.to_owned()), None).await;
                assert_strict(assert, json!(ok_exec(string.result).exit_code), json!(0));
                assert_strict(assert, json!(string.stdout), json!("outmore"));
                assert_strict(assert, json!(string.stderr), json!("err"));
            },
        ),
        shell_conformance_case(
            options,
            "argv exec honors cwd and exit codes",
            None,
            shell.clone(),
            |shell, env, assert| async move {
                let assert = assert.as_ref();
                ok_file(env.create_dir("sub", None, context()).await);
                let made = exec_collect(
                    env.as_ref(),
                    shell_command(&shell, "printf x > made.txt; exit 3"),
                    Some("sub"),
                )
                .await;
                assert_strict(assert, json!(ok_exec(made.result).exit_code), json!(3));
                assert_strict(
                    assert,
                    json!(ok_file(env.read_text_file("sub/made.txt", context()).await)),
                    json!("x"),
                );
            },
        ),
        conformance_case(
            options,
            "argv exec reports missing programs and empty argv as spawn errors",
            None,
            |env, assert| async move {
                let assert = assert.as_ref();
                assert_strict(
                    assert,
                    exec_code(
                        &env.exec(
                            ShellCommand::Argv(vec![
                                "kiana-durable-conformance-missing-program".to_owned(),
                            ]),
                            None,
                            context(),
                        )
                        .await,
                    ),
                    json!("spawn_error"),
                );
                assert_strict(
                    assert,
                    exec_code(
                        &env.exec(ShellCommand::Argv(Vec::new()), None, context())
                            .await,
                    ),
                    json!("spawn_error"),
                );
            },
        ),
        shell_conformance_case(
            options,
            "windowed exec keeps the exact tail and counts what it skips",
            None,
            shell.clone(),
            |shell, env, assert| async move {
                let assert = assert.clone();
                let lines = 2000usize;
                let window = ShellOutputWindow {
                    max_bytes: 200,
                    max_lines: 5,
                    min_interval_ms: 0.0,
                    bytes_per_second: 1_000_000_000.0,
                };
                // skipped 与 delivered 分开累加：两者相加应等于完整输出。
                struct WindowState {
                    bytes: usize,
                    newlines: usize,
                    tail: String,
                }
                let state: Arc<Mutex<WindowState>> = Arc::new(Mutex::new(WindowState {
                    bytes: 0,
                    newlines: 0,
                    tail: String::new(),
                }));
                let captured = state.clone();
                let callback_assert = assert.clone();
                let options = ShellExecOptions {
                    window: Some(window),
                    on_output: Some(Arc::new(
                        move |text: &str, _context: &dyn Context, info: &ShellOutputInfo| {
                            let mut state = captured.lock().unwrap();
                            if let Some(skipped) = info.skipped {
                                state.bytes += skipped.bytes;
                                state.newlines += skipped.newlines;
                                let after = encoded_len(text);
                                let after_newlines = newline_count(text);
                                assert_ok(
                                    callback_assert.as_ref(),
                                    after > window.max_bytes || after_newlines > window.max_lines,
                                    "a skip is followed by more than the window",
                                );
                                // 出现 skipped 说明本轮之前有输出被丢弃；用例要求此时当前块
                                // 本身就超出窗口，旧 tail 因此作废。
                                state.tail.clear();
                            }
                            state.bytes += encoded_len(text);
                            state.newlines += newline_count(text);
                            state.tail.push_str(text);
                        },
                    )),
                    ..ShellExecOptions::default()
                };
                let command = shell_command(
                    &shell,
                    &format!("i=0; while [ $i -lt {lines} ]; do echo line-$i; i=$((i+1)); done"),
                );
                let result = env.exec(command, Some(&options), context()).await;
                assert_strict(assert.as_ref(), json!(ok_exec(result).exit_code), json!(0));
                let expected: String = (0..lines).map(|index| format!("line-{index}\n")).collect();
                let state = state.lock().unwrap();
                assert_strict(assert.as_ref(), json!(state.bytes), json!(expected.len()));
                assert_strict(assert.as_ref(), json!(state.newlines), json!(lines));
                let tail: String = (lines - window.max_lines..lines)
                    .map(|index| format!("line-{index}\n"))
                    .collect();
                assert_ok(
                    assert.as_ref(),
                    state.tail.ends_with(&tail),
                    "the delivered output ends with the tail",
                );
            },
        ),
        shell_conformance_case(
            options,
            "argv exec distinguishes timeout from abort",
            None,
            shell.clone(),
            |shell, env, assert| async move {
                let assert = assert.as_ref();
                // 0.1 秒超时与 100 ms 后中止是两个独立信号，错误码必须区分 timeout/aborted。
                let timed_out = env
                    .exec(
                        shell_command(&shell, "sleep 2"),
                        Some(&ShellExecOptions {
                            timeout: Some(0.1),
                            ..ShellExecOptions::default()
                        }),
                        context(),
                    )
                    .await;
                assert_strict(assert, exec_code(&timed_out), json!("timeout"));
                let signal = AbortSignal::new();
                let aborting = signal.clone();
                let abort_task = tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    aborting.abort_without_reason();
                });
                let context = with_abort_signal(signal, background());
                let running = env
                    .exec(shell_command(&shell, "sleep 2"), None, context.as_ref())
                    .await;
                abort_task.await.unwrap();
                assert_strict(assert, exec_code(&running), json!("aborted"));
            },
        ),
    ];

    // listFiles 是可选能力：trait 的默认实现返回 not_supported，不打开开关就不追加这些用例。
    if list_files {
        cases.push(conformance_case(
            options,
            "listFiles lists files and directories depth first in name order",
            None,
            |env, assert| async move {
                let assert = assert.as_ref();
                // FileListing 的契约是深度优先、父目录先于条目、兄弟按名字字节序；期望值
                // 直接写死该顺序。
                write_all(
                    env.as_ref(),
                    &["b.txt", "a/z.txt", "a/b/c.txt", ".hidden/x", "A.txt"],
                )
                .await;
                ok_file(env.create_dir("empty", None, context()).await);
                assert_deep(
                    assert,
                    listed(env.as_ref(), ".", None).await,
                    json!({
                        "entries": [
                            "d .hidden",
                            "f .hidden/x",
                            "f A.txt",
                            "d a",
                            "d a/b",
                            "f a/b/c.txt",
                            "f a/z.txt",
                            "f b.txt",
                            "d empty",
                        ],
                        "truncated": false,
                    }),
                );
                let hidden_off = ListFilesOptions {
                    hidden: Some(false),
                    ..ListFilesOptions::default()
                };
                assert_deep(
                    assert,
                    json!(entries_of(
                        &listed(env.as_ref(), ".", Some(&hidden_off)).await
                    )),
                    json!([
                        "f A.txt",
                        "d a",
                        "d a/b",
                        "f a/b/c.txt",
                        "f a/z.txt",
                        "f b.txt",
                        "d empty",
                    ]),
                );
                let expected =
                    json!({ "entries": ["d b", "f b/c.txt", "f z.txt"], "truncated": false });
                assert_deep(
                    assert,
                    listed(env.as_ref(), "a", None).await,
                    expected.clone(),
                );
                // 相对路径与绝对路径应得到同一份列表。
                let absolute = ok_file(env.absolute_path("a", context()).await);
                assert_deep(
                    assert,
                    listed(env.as_ref(), &absolute, None).await,
                    expected,
                );
                assert_deep(
                    assert,
                    listed(env.as_ref(), "empty", None).await,
                    json!({ "entries": [], "truncated": false }),
                );
            },
        ));
        cases.push(conformance_case(
            options,
            "listFiles skips what git ignores in work trees, .git, and excluded names",
            None,
            |env, assert| async move {
                let assert = assert.as_ref();
                write_all(
                    env.as_ref(),
                    &[
                        "keep.txt",
                        "out/x.js",
                        "sub/keep.txt",
                        "sub/drop.log",
                        "node_modules/m/i.js",
                    ],
                )
                .await;
                ok_file(
                    env.write_file(
                        ".gitignore",
                        FileContent::Text("out/\n*.log\n".to_owned()),
                        context(),
                    )
                    .await,
                );
                ok_file(
                    env.write_file(
                        "sub/.gitignore",
                        FileContent::Text("!keep.txt\n".to_owned()),
                        context(),
                    )
                    .await,
                );
                // 还不是工作树：`.gitignore` 文件不生效。
                assert_ok(
                    assert,
                    entries_of(&listed(env.as_ref(), ".", None).await)
                        .iter()
                        .any(|entry| entry.as_str() == Some("f out/x.js")),
                    "out/x.js listed before .git exists",
                );
                ok_file(
                    env.write_file(
                        ".git/HEAD",
                        FileContent::Text("ref: refs/heads/main\n".to_owned()),
                        context(),
                    )
                    .await,
                );
                ok_file(
                    env.write_file(
                        ".git/info/exclude",
                        FileContent::Text("keep.txt\n".to_owned()),
                        context(),
                    )
                    .await,
                );
                let exclude = ListFilesOptions {
                    exclude: Some(vec!["node_modules".to_owned()]),
                    ..ListFilesOptions::default()
                };
                assert_deep(
                    assert,
                    json!(entries_of(&listed(env.as_ref(), ".", Some(&exclude)).await)),
                    json!([
                        "f .gitignore",
                        "d sub",
                        "f sub/.gitignore",
                        "f sub/keep.txt",
                    ]),
                );
                // 工作树内的目录会应用父目录的规则。
                assert_deep(
                    assert,
                    json!(entries_of(&listed(env.as_ref(), "sub", None).await)),
                    json!(["f .gitignore", "f keep.txt"]),
                );
                let no_gitignore = ListFilesOptions {
                    gitignore: Some(false),
                    ..ListFilesOptions::default()
                };
                let all = entries_of(&listed(env.as_ref(), ".", Some(&no_gitignore)).await);
                assert_ok(
                    assert,
                    all.iter().any(|entry| entry.as_str() == Some("f out/x.js"))
                        && all
                            .iter()
                            .any(|entry| entry.as_str() == Some("f node_modules/m/i.js"))
                        && all.iter().any(|entry| entry.as_str() == Some("f keep.txt")),
                    "all ignored files listed",
                );
                assert_ok(
                    assert,
                    !all.iter().any(|entry| {
                        entry
                            .as_str()
                            .is_some_and(|entry| entry.contains(".git/") || entry == "d .git")
                    }),
                    ".git is always skipped",
                );
            },
        ));
        cases.push(conformance_case(
            options,
            "listFiles stops at its bounds and reports truncated",
            None,
            |env, assert| async move {
                let assert = assert.as_ref();
                write_all(env.as_ref(), &["a", "b", "c"]).await;
                let max_two = ListFilesOptions {
                    max_entries: Some(2),
                    ..ListFilesOptions::default()
                };
                assert_deep(
                    assert,
                    listed(env.as_ref(), ".", Some(&max_two)).await,
                    json!({ "entries": ["f a", "f b"], "truncated": true }),
                );
                let max_three = ListFilesOptions {
                    max_entries: Some(3),
                    ..ListFilesOptions::default()
                };
                assert_deep(
                    assert,
                    listed(env.as_ref(), ".", Some(&max_three)).await,
                    json!({ "entries": ["f a", "f b", "f c"], "truncated": false }),
                );
                // 每个条目计入它的路径加两个字节。
                let max_bytes = ListFilesOptions {
                    max_bytes: Some(6),
                    ..ListFilesOptions::default()
                };
                assert_deep(
                    assert,
                    listed(env.as_ref(), ".", Some(&max_bytes)).await,
                    json!({ "entries": ["f a", "f b"], "truncated": true }),
                );
                let max_zero = ListFilesOptions {
                    max_entries: Some(0),
                    ..ListFilesOptions::default()
                };
                assert_deep(
                    assert,
                    listed(env.as_ref(), ".", Some(&max_zero)).await,
                    json!({ "entries": [], "truncated": true }),
                );
                // TS 还断言 `maxEntries: -1` 返回 `invalid`；Rust 的 `max_entries: usize`
                // （`ListFilesOptions` 属 ARCHITECTURE §4.9 冻结的 `env/*`）在类型上排除了负值，
                // 无法构造该输入，故无对应断言。
                // timeoutMs 只接受整数毫秒；1.5 这类非整数按 TS 用例应报 invalid。
                let bad_timeout = ListFilesOptions {
                    timeout_ms: Some(1.5),
                    ..ListFilesOptions::default()
                };
                assert_strict(
                    assert,
                    file_code(&env.list_files(".", Some(&bad_timeout), context()).await),
                    json!("invalid"),
                );
            },
        ));
        cases.push(conformance_case(
            options,
            "listFiles fails on missing directories, files, and aborts",
            None,
            |env, assert| async move {
                let assert = assert.as_ref();
                ok_file(
                    env.write_file("file.txt", FileContent::Text(String::new()), context())
                        .await,
                );
                assert_strict(
                    assert,
                    file_code(&env.list_files("missing", None, context()).await),
                    json!("not_found"),
                );
                assert_strict(
                    assert,
                    file_code(&env.list_files("file.txt", None, context()).await),
                    json!("not_directory"),
                );
                assert_strict(
                    assert,
                    file_code(&env.list_files(".", None, aborted_context().as_ref()).await),
                    json!("aborted"),
                );
            },
        ));
    }

    // 这个用例既要用 `ln -s` 造链接，又要用 listFiles 观察链接本身；两个开关都打开才追加。
    if symlinks && list_files {
        cases.push(shell_conformance_case(
            options,
            "listFiles lists symbolic links without following them",
            None,
            shell.clone(),
            |shell, env, assert| async move {
                let assert = assert.as_ref();
                ok_file(
                    env.write_file("dir/inner.txt", FileContent::Text(String::new()), context())
                        .await,
                );
                // 一条悬空链接、一条指回父目录的循环链接：都必须按 `l` 列出，不能跟随。
                let linked = env
                    .exec(
                        shell_command(
                            &shell,
                            "ln -s dir link && ln -s missing dangling && ln -s .. dir/loop",
                        ),
                        None,
                        context(),
                    )
                    .await;
                assert_strict(assert, json!(ok_exec(linked).exit_code), json!(0));
                assert_deep(
                    assert,
                    json!(entries_of(&listed(env.as_ref(), ".", None).await)),
                    json!([
                        "l dangling",
                        "d dir",
                        "f dir/inner.txt",
                        "l dir/loop",
                        "l link",
                    ]),
                );
                // 被列出的目录本身可以是链接。
                assert_deep(
                    assert,
                    json!(entries_of(&listed(env.as_ref(), "link", None).await)),
                    json!(["f inner.txt", "l loop"]),
                );
            },
        ));
    }

    // 其余符号链接用例只依赖 shell 的 `ln -s`。
    if symlinks {
        cases.push(shell_conformance_case(
            options,
            "binary reader follows symlinks unless noFollow refuses the final one",
            None,
            shell.clone(),
            |shell, env, assert| async move {
                let assert = assert.as_ref();
                ok_file(
                    env.write_file(
                        "target.txt",
                        FileContent::Text("target".to_owned()),
                        context(),
                    )
                    .await,
                );
                ok_file(env.create_dir("sub", None, context()).await);
                ok_file(
                    env.write_file(
                        "sub/inner.txt",
                        FileContent::Text("inner".to_owned()),
                        context(),
                    )
                    .await,
                );
                let linked = env
                    .exec(
                        shell_command(&shell, "ln -s target.txt link.txt && ln -s sub dirlink"),
                        None,
                        context(),
                    )
                    .await;
                assert_strict(assert, json!(ok_exec(linked).exit_code), json!(0));

                let followed = ok_file(env.open_binary_reader("link.txt", None, context()).await);
                assert_strict(
                    assert,
                    json!(decode_whole(&ok_file(
                        followed.read(0, 10, context()).await
                    ))),
                    json!("target"),
                );
                followed.close(context()).await;

                let no_follow = OpenBinaryReaderOptions {
                    no_follow: Some(true),
                };
                assert_strict(
                    assert,
                    file_code(
                        &env.open_binary_reader("link.txt", Some(&no_follow), context())
                            .await,
                    ),
                    json!("invalid"),
                );

                // 只拒绝最后一段；前面通过符号链接解析的目录仍然有效。
                let inner = ok_file(
                    env.open_binary_reader("dirlink/inner.txt", Some(&no_follow), context())
                        .await,
                );
                assert_strict(
                    assert,
                    json!(decode_whole(&ok_file(inner.read(0, 10, context()).await))),
                    json!("inner"),
                );
                inner.close(context()).await;
            },
        ));
        cases.push(shell_conformance_case(
            options,
            "watch reports changes to the file a watched symbolic link points to",
            Some(30_000),
            shell.clone(),
            |shell, env, assert| async move {
                let _ = &assert;
                ok_file(
                    env.write_file(
                        "data/real.md",
                        FileContent::Text("one".to_owned()),
                        context(),
                    )
                    .await,
                );
                ok_file(env.create_dir("config", None, context()).await);
                let linked = env
                    .exec(
                        shell_command(&shell, "ln -s ../data/real.md config/AGENTS.md"),
                        None,
                        context(),
                    )
                    .await;
                assert_strict(assert.as_ref(), json!(ok_exec(linked).exit_code), json!(0));
                // 监视路径是链接本身，实际写入的是它指向的 data/real.md；变化要报到链接路径。
                let targets = vec![WatchTarget {
                    path: "config/AGENTS.md".to_owned(),
                    recursive: None,
                    exclude: None,
                }];
                watching(env.as_ref(), &targets, |session| {
                    Box::pin(async move {
                        let env = session.env;
                        session
                            .expect_change("config/AGENTS.md", async {
                                ok_file(
                                    env.write_file(
                                        "data/real.md",
                                        FileContent::Text("two!".to_owned()),
                                        context(),
                                    )
                                    .await,
                                );
                            })
                            .await;
                    })
                })
                .await;
            },
        ));
    }

    cases
}

// 锁定能力探测约定：默认实现的 not_supported 消息映射为 TS 的 missing-member 错误，
// 其余错误按自身消息原样转成 panic（`FileError` 的 `Display` 只输出 `message`），
// 不映射为“未实现 listFiles”。
#[cfg(test)]
mod tests {
    use std::panic::AssertUnwindSafe;

    use super::*;

    fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
        payload
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| {
                payload
                    .downcast_ref::<&str>()
                    .map(|text| (*text).to_owned())
            })
            .expect("panic message")
    }

    #[test]
    fn list_files_guard_uses_the_typescript_message_for_unimplemented_environments() {
        let error = FileError::new(
            FileErrorCode::NotSupported,
            "listFiles is not supported by this environment",
        );
        let panic =
            std::panic::catch_unwind(AssertUnwindSafe(|| list_files_or_panic::<()>(Err(error))))
                .expect_err("the guard must panic");
        assert_eq!(
            panic_message(panic),
            "The environment does not implement listFiles"
        );
    }

    #[test]
    fn list_files_guard_keeps_other_errors_verbatim() {
        let error = FileError::new(FileErrorCode::Invalid, "bad maxEntries");
        let panic =
            std::panic::catch_unwind(AssertUnwindSafe(|| list_files_or_panic::<()>(Err(error))))
                .expect_err("the guard must panic");
        assert_eq!(panic_message(panic), "bad maxEntries");
    }
}
