//! `packages/env/test/differential.test.ts` 的移植：`RemoteExecutionEnv` 与 `NodeExecutionEnv`
//! 的结果逐项对比。
//!
//! 与 TS 的差异：
//! - 结果先由本测试序列化成与 `JSON.stringify(normalize(...))` 相同的 JSON 形状，再比较
//!   （TS 用 `JSON.stringify` 的 replacer；Rust 没有，故逐操作构造同样的值）。
//! - `Uint8Array` → `{ bytes: hex }`、`mtimeMs` 的值 → `"mtime"`、失败的 `Result` →
//!   `{ ok: false, code }`、根目录（`realpath` 与原文）→ `"<root>"`。
//! - `G2.4` 的 `gives the same read and bash tool results`：`ToolExecutionApi` 的签名里出现
//!   `kiana_ai::models::Models`，因此 `kiana-ai` 是本 crate 的 **dev-dependency**（`[dependencies]`
//!   没有增加任何东西）；测试替身只实现 read/bash 用到的部分，其余方法与 kiana-durable 的同名
//!   测试替身一样 `unreachable!`。
//! - 另有回归用例 `list_dir_skips_unsupported_entries_like_node`：TS 的 FIFO 用例没有调用
//!   `listDir`，没有覆盖“跳过不支持条目”的规则。
//! - `listDir` 的两侧结果在比较前按名字排序：TS 依赖 libuv 的 `scandir`（POSIX 上按字节排序），
//!   而 kiana-durable 的 `NodeExecutionEnv::list_dir` 目前保留文件系统的顺序（这是 kiana-durable 的
//!   缺口，不在本单元范围内）。`RemoteExecutionEnv` 自己的排序由
//!   `remote_list_dir_sorts_names_by_bytes_on_posix` 单独验证。
//! - `normalize` 在 Windows 上剥掉 `std::fs::canonicalize` 的 `\\?\` 扩展长度前缀，对应
//!   `realpathSync.native` 的普通拼写（differential.test.ts:53-64）。
//!
//! TS: packages/env/test/differential.test.ts

mod common;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use kiana_chord::context::Context;
use kiana_durable::env::node::{NodeEnvOptions, NodeExecutionEnv};
use kiana_durable::env::{
    CreateDirOptions, DirPage, ExecutionEnv, FileContent, FileError, FileErrorCode, FileInfo,
    FileKind, FileSystem, LineRange, LineScan, OpenBinaryReaderOptions, ReadTextLinesOptions,
    RemoveOptions, ShellCommand, ShellOutputSkip, ShellOutputWindow, TextLine,
};
use kiana_durable::harness::types::{
    Agent, AnyTask, ConversationHandle, NestedToolExecutionResult, NestedToolOptions,
    RegistrySnapshot, RetainedOutput, SettledTask, ToolDiagnostic, ToolExecutionApi, ToolOutput,
    ToolRegistration,
};
use kiana_durable::tools::bash::create_bash_tool;
use kiana_durable::tools::read::{ReadToolOptions, create_read_tool};
use kiana_durable::{
    BoxError, CommitOptions, ConversationId, DocTarget, DocumentObserver, DocumentReader,
    DocumentWatch, EntryId, JsonObject, JsonValue, SessionChange, TaskId, TaskOptions, TaskRecord,
    id_from_number,
};
use serde_json::{Value, json as json_value};

use kiana_env_client::{Connection, RemoteExecutionEnv, RemoteExecutionEnvOptions};

// ---------------------------------------------------------------------------
// 随机数（mulberry32 变体，逐位照抄 TS）
// ---------------------------------------------------------------------------

struct Random {
    state: u32,
}

impl Random {
    fn new(seed: u32) -> Self {
        Random { state: seed }
    }

    fn next(&mut self) -> f64 {
        self.state = self.state.wrapping_add(0x6d2b_79f5);
        let mut t = self.state;
        t = (t ^ (t >> 15)).wrapping_mul(t | 1);
        t ^= t.wrapping_add((t ^ (t >> 7)).wrapping_mul(t | 61));
        f64::from(t ^ (t >> 14)) / 4_294_967_296.0
    }

    fn pick(&mut self, length: usize) -> usize {
        ((self.next() * length as f64).floor() as usize).min(length.saturating_sub(1))
    }

    fn flag(&mut self) -> bool {
        self.next() < 0.5
    }
}

// ---------------------------------------------------------------------------
// 夹具
// ---------------------------------------------------------------------------

const NAMES: [&str; 8] = [
    "a.txt",
    "b",
    "dir",
    "dir/c.txt",
    "dir/sub",
    "dir/sub/d.bin",
    "missing",
    "a.txt/x",
];

const CONTENT_TEXTS: [&str; 4] = ["", "hello\n", "\u{feff}bom\r\nline", "x"];
const CONTENT_BYTES: [&[u8]; 2] = [&[0xe2, 0x82, 0x0a, 0xff, 0xef, 0xbb, 0xbf], &[]];

/// `CONTENTS` 的 6 个元素：4 个文本（第 4 个是 `"x"×70_000`）与 2 个字节数组（第 2 个是
/// `"é"×300_001` 的 UTF-8）。
#[derive(Clone)]
enum Content {
    LongX,
    ManyE,
    Text(&'static str),
    Bytes(&'static [u8]),
}

impl Content {
    fn at(index: usize) -> Content {
        match index {
            0 => Content::Text(CONTENT_TEXTS[0]),
            1 => Content::Text(CONTENT_TEXTS[1]),
            2 => Content::Text(CONTENT_TEXTS[2]),
            3 => Content::Bytes(CONTENT_BYTES[0]),
            4 => Content::LongX,
            5 => Content::ManyE,
            _ => Content::Bytes(CONTENT_BYTES[1]),
        }
    }

    fn file_content(&self) -> FileContent {
        match self {
            Content::LongX => FileContent::Text("x".repeat(70_000)),
            Content::ManyE => FileContent::Text("é".repeat(300_001)),
            Content::Text(text) => FileContent::Text((*text).to_owned()),
            Content::Bytes(bytes) => FileContent::Bytes(bytes.to_vec()),
        }
    }

    fn label(&self) -> String {
        match self {
            Content::LongX => string_arg(&"x".repeat(70_000)),
            Content::ManyE => string_arg(&"é".repeat(300_001)),
            Content::Text(text) => string_arg(text),
            Content::Bytes(_) => "<bytes>".to_owned(),
        }
    }
}

fn utf16_length(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

/// `pick`：字符串超过 20 个 UTF-16 码元时写成 `<n chars>`，否则 `JSON.stringify`。
fn string_arg(text: &str) -> String {
    if utf16_length(text) > 20 {
        format!("<{} chars>", utf16_length(text))
    } else {
        serde_json::to_string(text).expect("a string serializes")
    }
}

fn name_arg(name: &str) -> String {
    string_arg(name)
}

fn number_arg(value: u64) -> String {
    value.to_string()
}

fn bool_arg(value: bool) -> String {
    if value {
        "true".to_owned()
    } else {
        "false".to_owned()
    }
}

// ---------------------------------------------------------------------------
// 归一化：与 TS 的 `normalize` 相同
// ---------------------------------------------------------------------------

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

fn failure(error: FileError) -> Value {
    json_value!({"ok": false, "code": file_error_code(error.code)})
}

fn success(value: Value) -> Value {
    json_value!({"ok": true, "value": value})
}

fn success_unit() -> Value {
    json_value!({"ok": true})
}

fn kind_name(kind: FileKind) -> &'static str {
    match kind {
        FileKind::File => "file",
        FileKind::Directory => "directory",
        FileKind::Symlink => "symlink",
    }
}

fn info_json(info: &FileInfo) -> Value {
    json_value!({
        "name": info.name,
        "path": info.path,
        "kind": kind_name(info.kind),
        "size": info.size,
        "mtimeMs": "mtime",
    })
}

fn infos_json(infos: &[FileInfo]) -> Value {
    Value::Array(infos.iter().map(info_json).collect())
}

/// `listDir` 结果的比较键：按名字的字节排序。
///
/// TS 里两侧的顺序都由 libuv 的 `scandir` 决定（POSIX 上按字节排序）；Rust 的
/// `NodeExecutionEnv::list_dir` 目前保留文件系统的顺序（这是 kiana-durable 的缺口，不在本单元
/// 的范围内，已报告），因此这里把两侧都排序后再比较。`RemoteExecutionEnv` 自己的排序由
/// `remote_list_dir_sorts_names_by_bytes_on_posix` 单独验证。
fn sorted_infos_json(infos: &[FileInfo]) -> Value {
    let mut infos: Vec<&FileInfo> = infos.iter().collect();
    infos.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));
    Value::Array(infos.into_iter().map(info_json).collect())
}

fn bytes_json(bytes: &[u8]) -> Value {
    json_value!({"bytes": hex::encode(bytes)})
}

fn text_line_json(line: &Option<TextLine>) -> Value {
    match line {
        Some(line) => json_value!({"text": line.text, "terminated": line.terminated}),
        None => Value::Null,
    }
}

fn scan_json(scan: LineScan) -> Value {
    json_value!({
        "newlines": scan.newlines,
        "start": scan.start,
        "end": scan.end,
        "firstLineEnd": scan.first_line_end,
        "lastLineStart": scan.last_line_start,
        "selectedBytes": scan.selected_bytes,
        "firstLineBytes": scan.first_line_bytes,
    })
}

fn page_json(page: &DirPage) -> Value {
    json_value!({"entries": infos_json(&page.entries), "done": page.done})
}

/// 剥掉 Windows 的扩展长度前缀：`std::fs::canonicalize` 返回 `\\?\C:\…` / `\\?\UNC\…`，而
/// Node 的 `realpathSync.native` 返回普通拼写（diff 里两侧的根才会对上）。其它平台原样返回。
fn strip_extended_length_prefix(path: &str) -> String {
    if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{rest}");
    }
    if let Some(rest) = path.strip_prefix(r"\\?\") {
        return rest.to_owned();
    }
    path.to_owned()
}

/// 把结果里的根目录替换为 `<root>`：TS 对 JSON 文本做同样的替换。
fn normalize(value: Value, root: &str) -> Value {
    let text = serde_json::to_string(&value).expect("the result serializes");
    let canonical = std::fs::canonicalize(root)
        .map(|path| strip_extended_length_prefix(&path.to_string_lossy()))
        .unwrap_or_else(|_| root.to_owned());
    let escaped_canonical = json_escape(&canonical);
    let escaped_root = json_escape(root);
    let text = text
        .replace(&escaped_canonical, "<root>")
        .replace(&escaped_root, "<root>");
    serde_json::from_str(&text).expect("the normalized result parses")
}

fn json_escape(text: &str) -> String {
    let quoted = serde_json::to_string(text).expect("a string serializes");
    quoted[1..quoted.len() - 1].to_owned()
}

// ---------------------------------------------------------------------------
// 随机操作
// ---------------------------------------------------------------------------

enum Operation {
    WriteFile {
        name: String,
        content: Content,
    },
    AppendFile {
        name: String,
        content: Content,
    },
    ReadTextFile {
        name: String,
    },
    ReadBinaryFile {
        name: String,
    },
    ReadTextLines {
        name: String,
        max_lines: usize,
    },
    FileInfo {
        name: String,
    },
    Exists {
        name: String,
    },
    ListDir {
        name: String,
    },
    CreateDir {
        name: String,
        recursive: bool,
    },
    Remove {
        name: String,
        recursive: bool,
        force: bool,
    },
    RenameFile {
        source: String,
        destination: String,
    },
    TruncateFile {
        name: String,
        size: u64,
    },
    FlushFile {
        name: String,
    },
    CanonicalPath {
        name: String,
    },
    OpenBinaryReader {
        name: String,
        no_follow: bool,
        offset: u64,
        length: usize,
    },
}

impl Operation {
    fn label(&self) -> String {
        let (name, args): (&str, Vec<String>) = match self {
            Operation::WriteFile { name, content } => {
                ("writeFile", vec![name_arg(name), content.label()])
            }
            Operation::AppendFile { name, content } => {
                ("appendFile", vec![name_arg(name), content.label()])
            }
            Operation::ReadTextFile { name } => ("readTextFile", vec![name_arg(name)]),
            Operation::ReadBinaryFile { name } => ("readBinaryFile", vec![name_arg(name)]),
            Operation::ReadTextLines { name, max_lines } => (
                "readTextLines",
                vec![name_arg(name), format!("{{\"maxLines\":{max_lines}}}")],
            ),
            Operation::FileInfo { name } => ("fileInfo", vec![name_arg(name)]),
            Operation::Exists { name } => ("exists", vec![name_arg(name)]),
            Operation::ListDir { name } => ("listDir", vec![name_arg(name)]),
            Operation::CreateDir { name, recursive } => (
                "createDir",
                vec![
                    name_arg(name),
                    format!("{{\"recursive\":{}}}", bool_arg(*recursive)),
                ],
            ),
            Operation::Remove {
                name,
                recursive,
                force,
            } => (
                "remove",
                vec![
                    name_arg(name),
                    format!(
                        "{{\"recursive\":{},\"force\":{}}}",
                        bool_arg(*recursive),
                        bool_arg(*force)
                    ),
                ],
            ),
            Operation::RenameFile {
                source,
                destination,
            } => ("renameFile", vec![name_arg(source), name_arg(destination)]),
            Operation::TruncateFile { name, size } => {
                ("truncateFile", vec![name_arg(name), number_arg(*size)])
            }
            Operation::FlushFile { name } => ("flushFile", vec![name_arg(name)]),
            Operation::CanonicalPath { name } => ("canonicalPath", vec![name_arg(name)]),
            Operation::OpenBinaryReader {
                name,
                no_follow,
                offset,
                length,
            } => (
                "openBinaryReader",
                vec![
                    name_arg(name),
                    format!("{{\"noFollow\":{}}}", bool_arg(*no_follow)),
                    number_arg(*offset),
                    number_arg(*length as u64),
                ],
            ),
        };
        format!("{name}({})", args.join(", "))
    }

    async fn run(&self, env: &dyn ExecutionEnv, context: &dyn Context) -> Value {
        match self {
            Operation::WriteFile { name, content } => {
                match env.write_file(name, content.file_content(), context).await {
                    Ok(()) => success_unit(),
                    Err(error) => failure(error),
                }
            }
            Operation::AppendFile { name, content } => {
                match env.append_file(name, content.file_content(), context).await {
                    Ok(()) => success_unit(),
                    Err(error) => failure(error),
                }
            }
            Operation::ReadTextFile { name } => match env.read_text_file(name, context).await {
                Ok(text) => success(Value::String(text)),
                Err(error) => failure(error),
            },
            Operation::ReadBinaryFile { name } => match env.read_binary_file(name, context).await {
                Ok(bytes) => success(bytes_json(&bytes)),
                Err(error) => failure(error),
            },
            Operation::ReadTextLines { name, max_lines } => {
                let options = ReadTextLinesOptions {
                    max_lines: Some(*max_lines),
                };
                match env.read_text_lines(name, Some(&options), context).await {
                    Ok(lines) => {
                        success(Value::Array(lines.into_iter().map(Value::String).collect()))
                    }
                    Err(error) => failure(error),
                }
            }
            Operation::FileInfo { name } => match env.file_info(name, context).await {
                Ok(info) => success(info_json(&info)),
                Err(error) => failure(error),
            },
            Operation::Exists { name } => match env.exists(name, context).await {
                Ok(exists) => success(Value::Bool(exists)),
                Err(error) => failure(error),
            },
            Operation::ListDir { name } => match env.list_dir(name, context).await {
                Ok(infos) => success(sorted_infos_json(&infos)),
                Err(error) => failure(error),
            },
            Operation::CreateDir { name, recursive } => {
                let options = CreateDirOptions {
                    recursive: Some(*recursive),
                };
                match env.create_dir(name, Some(&options), context).await {
                    Ok(()) => success_unit(),
                    Err(error) => failure(error),
                }
            }
            Operation::Remove {
                name,
                recursive,
                force,
            } => {
                let options = RemoveOptions {
                    recursive: Some(*recursive),
                    force: Some(*force),
                };
                match env.remove(name, Some(&options), context).await {
                    Ok(()) => success_unit(),
                    Err(error) => failure(error),
                }
            }
            Operation::RenameFile {
                source,
                destination,
            } => match env.rename_file(source, destination, context).await {
                Ok(()) => success_unit(),
                Err(error) => failure(error),
            },
            Operation::TruncateFile { name, size } => {
                match env.truncate_file(name, *size, context).await {
                    Ok(()) => success_unit(),
                    Err(error) => failure(error),
                }
            }
            Operation::FlushFile { name } => match env.flush_file(name, context).await {
                Ok(()) => success_unit(),
                Err(error) => failure(error),
            },
            Operation::CanonicalPath { name } => match env.canonical_path(name, context).await {
                Ok(path) => success(Value::String(path)),
                Err(error) => failure(error),
            },
            Operation::OpenBinaryReader {
                name,
                no_follow,
                offset,
                length,
            } => {
                let options = OpenBinaryReaderOptions {
                    no_follow: Some(*no_follow),
                };
                match env.open_binary_reader(name, Some(&options), context).await {
                    Err(error) => failure(error),
                    Ok(reader) => {
                        let read = reader.read(*offset, *length, context).await;
                        let scan = reader
                            .scan_lines(
                                LineRange {
                                    start_line: 0,
                                    end_line: None,
                                },
                                context,
                            )
                            .await;
                        reader.close(context).await;
                        Value::Array(vec![
                            match read {
                                Ok(bytes) => success(bytes_json(&bytes)),
                                Err(error) => failure(error),
                            },
                            match scan {
                                Ok(scan) => success(scan_json(scan)),
                                Err(error) => failure(error),
                            },
                        ])
                    }
                }
            }
        }
    }
}

fn random_operation(next: &mut Random) -> Operation {
    let choice = next.pick(15);
    match choice {
        0 => Operation::WriteFile {
            name: name_value(next),
            content: Content::at(next.pick(6)),
        },
        1 => Operation::AppendFile {
            name: name_value(next),
            content: Content::at(next.pick(6)),
        },
        2 => Operation::ReadTextFile {
            name: name_value(next),
        },
        3 => Operation::ReadBinaryFile {
            name: name_value(next),
        },
        4 => Operation::ReadTextLines {
            name: name_value(next),
            max_lines: next.pick(4),
        },
        5 => Operation::FileInfo {
            name: name_value(next),
        },
        6 => Operation::Exists {
            name: name_value(next),
        },
        7 => Operation::ListDir {
            name: name_value(next),
        },
        8 => Operation::CreateDir {
            name: name_value(next),
            recursive: next.flag(),
        },
        9 => Operation::Remove {
            name: name_value(next),
            recursive: next.flag(),
            force: next.flag(),
        },
        10 => Operation::RenameFile {
            source: name_value(next),
            destination: name_value(next),
        },
        11 => Operation::TruncateFile {
            name: name_value(next),
            size: next.pick(10) as u64,
        },
        12 => Operation::FlushFile {
            name: name_value(next),
        },
        13 => Operation::CanonicalPath {
            name: name_value(next),
        },
        _ => Operation::OpenBinaryReader {
            name: name_value(next),
            no_follow: next.flag(),
            offset: next.pick(8) as u64,
            length: next.pick(8),
        },
    }
}

fn name_value(next: &mut Random) -> String {
    NAMES[next.pick(NAMES.len())].to_owned()
}

// ---------------------------------------------------------------------------
// 一对环境
// ---------------------------------------------------------------------------

struct Pair {
    local: NodeExecutionEnv,
    remote: RemoteExecutionEnv,
    local_root: String,
    remote_root: String,
    _local_dir: tempfile::TempDir,
    _remote_dir: tempfile::TempDir,
}

fn pair(connection: &Arc<Connection>) -> Pair {
    let local_dir = tempfile::Builder::new()
        .prefix("kiana-env-local-")
        .tempdir()
        .expect("create a temporary directory");
    let remote_dir = tempfile::Builder::new()
        .prefix("kiana-env-remote-")
        .tempdir()
        .expect("create a temporary directory");
    let local_root = local_dir.path().to_string_lossy().into_owned();
    let remote_root = remote_dir.path().to_string_lossy().into_owned();
    let local = NodeExecutionEnv::new(NodeEnvOptions::new(&local_root));
    let remote = RemoteExecutionEnv::new(RemoteExecutionEnvOptions {
        connection: Arc::clone(connection),
        id: "kiana-env:test".to_owned(),
        cwd: remote_root.clone(),
        shell_path: None,
        shell_env: None,
        watch: None,
    });
    Pair {
        local,
        remote,
        local_root,
        remote_root,
        _local_dir: local_dir,
        _remote_dir: remote_dir,
    }
}

/// `gives the same results for random file operation sequences`：60 个种子 ×25 步。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gives_the_same_results_for_random_file_operation_sequences() {
    common::titled(
        "gives the same results for random file operation sequences",
        async {
            let connection = Arc::new(Connection::new(vec![common::daemon_path()]));
            let run = async {
                for seed in 1..=60u32 {
                    let mut next = Random::new(seed);
                    let pair = pair(&connection);
                    for step in 0..25 {
                        let operation = random_operation(&mut next);
                        let expected = normalize(
                            operation.run(&pair.local, common::context()).await,
                            &pair.local_root,
                        );
                        let actual = normalize(
                            operation.run(&pair.remote, common::context()).await,
                            &pair.remote_root,
                        );
                        assert_eq!(
                            actual,
                            expected,
                            "seed {seed} step {step} {}",
                            operation.label()
                        );
                    }
                }
            };
            match tokio::time::timeout(Duration::from_secs(120), run).await {
                Ok(()) => {}
                Err(_) => {
                    panic!("gives the same results for random file operation sequences timed out")
                }
            }
            connection.close();
        },
    )
    .await;
}

/// `shell_result`：`mkfifo` 的退出码。
async fn make_fifo(env: &dyn ExecutionEnv, fifo: &str) {
    let result = env
        .exec(
            ShellCommand::Argv(vec!["mkfifo".to_owned(), fifo.to_owned()]),
            None,
            common::context(),
        )
        .await;
    match result {
        Ok(result) if result.exit_code == 0 => {}
        Ok(result) => panic!("mkfifo failed: exit {}", result.exit_code),
        Err(error) => panic!("mkfifo failed: {}", error.message),
    }
}

// The writer is intentional: TS spawns it without waiting either, and it exits once the FIFO's
// reader closes.
#[allow(clippy::zombie_processes)]
fn spawn_writer(text: &str, fifo: &str) {
    std::process::Command::new("sh")
        .arg("-c")
        .arg("printf '%s' \"$1\" > \"$2\"")
        .arg("sh")
        .arg(text)
        .arg(fifo)
        .spawn()
        .expect("the FIFO writer starts");
}

/// `reads devices, FIFOs and directories like Node`（非 Windows）。
#[cfg(not(windows))]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reads_devices_fifos_and_directories_like_node() {
    common::titled("reads devices, FIFOs and directories like Node", async {
        let connection = Arc::new(Connection::new(vec![common::daemon_path()]));
        let pair = pair(&connection);
        async fn results(env: &dyn ExecutionEnv, root: String) -> Value {
            let fifo = format!("{root}/fifo");
            make_fifo(env, &fifo).await;
            match env.create_dir("dir", None, common::context()).await {
                Ok(()) => {}
                Err(error) => panic!("createDir failed: {}", error.message),
            }
            spawn_writer("fifo text\n", &fifo);
            let fifo_text = env.read_text_file("fifo", common::context()).await;
            spawn_writer("fifo bytes", &fifo);
            let fifo_bytes = env.read_binary_file("fifo", common::context()).await;
            spawn_writer("x", &fifo);
            let fifo_lines = env.read_text_lines("fifo", None, common::context()).await;
            let dir_reader = env.open_text_line_reader("dir", common::context()).await;
            let (dir_reader_opened, dir_line) = match dir_reader {
                Ok(reader) => {
                    let line = reader.read_line(common::context()).await;
                    reader.close(common::context()).await;
                    match line {
                        Ok(line) => (true, success(text_line_json(&line))),
                        Err(error) => (true, failure(error)),
                    }
                }
                Err(_) => (false, Value::Null),
            };
            let mut json = serde_json::Map::new();
            json.insert(
                "nullText".to_owned(),
                match env.read_text_file("/dev/null", common::context()).await {
                    Ok(text) => success(Value::String(text)),
                    Err(error) => failure(error),
                },
            );
            json.insert(
                "nullBytes".to_owned(),
                match env.read_binary_file("/dev/null", common::context()).await {
                    Ok(bytes) => success(bytes_json(&bytes)),
                    Err(error) => failure(error),
                },
            );
            json.insert(
                "nullLines".to_owned(),
                match env
                    .read_text_lines("/dev/null", None, common::context())
                    .await
                {
                    Ok(lines) => {
                        success(Value::Array(lines.into_iter().map(Value::String).collect()))
                    }
                    Err(error) => failure(error),
                },
            );
            json.insert(
                "fifoText".to_owned(),
                match fifo_text {
                    Ok(text) => success(Value::String(text)),
                    Err(error) => failure(error),
                },
            );
            json.insert(
                "fifoBytes".to_owned(),
                match fifo_bytes {
                    Ok(bytes) => success(bytes_json(&bytes)),
                    Err(error) => failure(error),
                },
            );
            json.insert(
                "fifoLines".to_owned(),
                match fifo_lines {
                    Ok(lines) => {
                        success(Value::Array(lines.into_iter().map(Value::String).collect()))
                    }
                    Err(error) => failure(error),
                },
            );
            json.insert("dirReaderOpened".to_owned(), Value::Bool(dir_reader_opened));
            json.insert("dirLine".to_owned(), dir_line);
            json.insert(
                "dirText".to_owned(),
                match env.read_text_file("dir", common::context()).await {
                    Ok(text) => success(Value::String(text)),
                    Err(error) => failure(error),
                },
            );
            json.insert(
                "dirBytes".to_owned(),
                match env.read_binary_file("dir", common::context()).await {
                    Ok(bytes) => success(bytes_json(&bytes)),
                    Err(error) => failure(error),
                },
            );
            Value::Object(json)
        }
        let expected = normalize(
            results(&pair.local, pair.local_root.clone()).await,
            &pair.local_root,
        );
        let actual = normalize(
            results(&pair.remote, pair.remote_root.clone()).await,
            &pair.remote_root,
        );
        assert_eq!(
            actual, expected,
            "reads devices, FIFOs and directories like Node"
        );
        connection.close();
    })
    .await;
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn write_raw_name(root: &str, name: &[u8]) {
    use std::os::unix::ffi::OsStrExt;
    let mut path = root.as_bytes().to_vec();
    path.push(b'/');
    path.extend_from_slice(name);
    let path = std::path::Path::new(std::ffi::OsStr::from_bytes(&path));
    std::fs::write(path, "x").expect("write a raw name");
}

/// `lists names that are not UTF-8 like Node`（linux/android）。
#[cfg(any(target_os = "linux", target_os = "android"))]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lists_names_that_are_not_utf8_like_node() {
    common::titled("lists names that are not UTF-8 like Node", async {
        let connection = Arc::new(Connection::new(vec![common::daemon_path()]));
        let pair = pair(&connection);
        for root in [&pair.local_root, &pair.remote_root] {
            for name in [
                &[0x62u8][..],
                &[0x61, 0xff],
                &[0xc3, 0xa9],
                &[0x61, 0xfe, 0x7a],
                &[0x7a],
            ] {
                write_raw_name(root, name);
            }
        }
        async fn results(env: &dyn ExecutionEnv) -> Value {
            let reader = env.open_dir_reader(".", common::context()).await;
            let mut pages: Vec<Value> = Vec::new();
            if let Ok(reader) = reader {
                let mut done = false;
                while !done {
                    let page = reader.next(2, common::context()).await;
                    match &page {
                        Ok(page) => {
                            pages.push(success(page_json(page)));
                            done = page.done;
                        }
                        Err(error) => {
                            pages.push(failure(error.clone()));
                            done = true;
                        }
                    }
                }
                reader.close(common::context()).await;
            }
            let list = match env.list_dir(".", common::context()).await {
                Ok(infos) => success(sorted_infos_json(&infos)),
                Err(error) => failure(error),
            };
            json_value!({"list": list, "pages": pages})
        }
        let expected = normalize(results(&pair.local).await, &pair.local_root);
        let actual = normalize(results(&pair.remote).await, &pair.remote_root);
        assert_eq!(actual, expected, "lists names that are not UTF-8 like Node");
        connection.close();
    })
    .await;
}

// ---------------------------------------------------------------------------
// `gives the same read and bash tool results`
// ---------------------------------------------------------------------------

/// `differential.test.ts` 的 `api(env, output)`：只实现 read/bash 用到的部分。
///
/// TS 用对象字面量 + `as unknown as ToolExecutionApi`；Rust 必须实现整个 trait，未用到的
/// 方法（registry/commit/memo/任务/嵌套工具）与 kiana-durable 自己的同名测试替身一样
/// `unreachable!`。
struct ToolApi {
    env: Option<Arc<dyn ExecutionEnv>>,
    output: std::sync::Mutex<String>,
}

impl ToolApi {
    fn new(env: Arc<dyn ExecutionEnv>) -> Arc<Self> {
        Arc::new(ToolApi {
            env: Some(env),
            output: std::sync::Mutex::new(String::new()),
        })
    }

    fn streamed(&self) -> String {
        self.output
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

#[async_trait]
impl DocumentReader for ToolApi {
    async fn snapshot(
        &self,
        _target: &DocTarget,
        _context: &dyn Context,
    ) -> Result<Option<JsonObject>, BoxError> {
        unreachable!("coding tools do not read documents")
    }

    async fn snapshot_as_of(
        &self,
        _target: &DocTarget,
        _at: EntryId,
        _context: &dyn Context,
    ) -> Result<Option<JsonObject>, BoxError> {
        unreachable!("coding tools do not read documents")
    }
}

#[async_trait]
impl DocumentObserver for ToolApi {
    async fn watch_doc(
        &self,
        _target: &DocTarget,
        _context: &dyn Context,
    ) -> Result<Option<DocumentWatch>, BoxError> {
        unreachable!("coding tools do not watch documents")
    }
}

#[async_trait]
impl ToolExecutionApi for ToolApi {
    fn task_id(&self) -> TaskId {
        id_from_number(1)
    }

    fn conversation_id(&self) -> ConversationId {
        id_from_number(1)
    }

    fn call_id(&self) -> &str {
        "call"
    }

    fn registry(&self) -> Arc<dyn RegistrySnapshot> {
        unreachable!("coding tools do not read the registry")
    }

    async fn agent(&self, _context: &dyn Context) -> Result<Arc<Agent>, BoxError> {
        // 没有模型：`read` 视作接受图片的模型（TS 测试的 api 对象同样没有 models）。
        Ok(Arc::new(Agent {
            model: None,
            thinking_level: Default::default(),
            extensions: Vec::new(),
            tools: Vec::new(),
            callable: Vec::new(),
            deferred: Vec::new(),
            sections: Vec::new(),
            instructions: None,
            cwd: None,
        }))
    }

    fn models(&self) -> Arc<dyn kiana_ai::models::Models> {
        unreachable!("the fake agent has no model")
    }

    fn env(&self) -> Option<Arc<dyn ExecutionEnv>> {
        self.env.clone()
    }

    fn now(&self) -> i64 {
        0
    }

    fn output(&self, chunk: ToolOutput, _skipped: Option<ShellOutputSkip>) {
        let text = match chunk {
            ToolOutput::Text(text) => text,
            ToolOutput::Bytes(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        };
        self.output
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push_str(&text);
    }

    fn output_window(&self) -> Option<ShellOutputWindow> {
        None
    }

    fn retained_output(&self) -> RetainedOutput {
        RetainedOutput {
            text: self.streamed(),
            truncated: false,
        }
    }

    fn diagnostic(&self, _diagnostic: ToolDiagnostic) {}

    async fn details(&self, _value: JsonValue, _context: &dyn Context) -> Result<(), BoxError> {
        Ok(())
    }

    async fn commit(
        &self,
        _change: &dyn SessionChange,
        _context: &dyn Context,
        _options: CommitOptions,
    ) -> Result<Box<dyn std::any::Any + Send>, BoxError> {
        unreachable!("coding tools do not commit")
    }

    async fn memo(
        &self,
        _name: &str,
        _context: &dyn Context,
    ) -> Result<Option<JsonValue>, BoxError> {
        unreachable!("coding tools do not use memos")
    }

    async fn memo_put(
        &self,
        _name: &str,
        _candidate: JsonValue,
        _context: &dyn Context,
    ) -> Result<JsonValue, BoxError> {
        unreachable!("coding tools do not use memos")
    }

    async fn create_task(
        &self,
        _task: &AnyTask,
        _input: JsonValue,
        _options: TaskOptions,
        _context: &dyn Context,
    ) -> Result<TaskId, BoxError> {
        unreachable!("coding tools do not create tasks")
    }

    async fn get_task(
        &self,
        _id: TaskId,
        _context: &dyn Context,
    ) -> Result<Option<TaskRecord>, BoxError> {
        unreachable!("coding tools do not read tasks")
    }

    async fn wait_for_task(
        &self,
        _id: TaskId,
        _context: &dyn Context,
    ) -> Result<SettledTask, BoxError> {
        unreachable!("coding tools do not wait for tasks")
    }

    async fn conversation(
        &self,
        _id: ConversationId,
        _context: &dyn Context,
    ) -> Result<Option<Arc<dyn ConversationHandle>>, BoxError> {
        unreachable!("coding tools do not open conversations")
    }

    fn execute_tool(
        self: Arc<Self>,
        _name: &str,
        _args: JsonObject,
        _context: &dyn Context,
        _options: NestedToolOptions,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<NestedToolExecutionResult, BoxError>>
                + Send
                + 'static,
        >,
    > {
        unreachable!("coding tools do not nest tools")
    }
}

/// `run(env, tool, args)`：`{ result, output }` 或 `{ error, output }`。
async fn run_tool(env: &Arc<dyn ExecutionEnv>, tool: &ToolRegistration, args: Value) -> Value {
    let api = ToolApi::new(Arc::clone(env));
    let execute = tool.execute.as_ref().expect("the tool has an execute");
    let context = kiana_chord::context::BACKGROUND_CONTEXT.clone();
    match execute(args, api.clone(), context).await {
        Ok(result) => json_value!({
            "result": serde_json::to_value(&result).expect("the result serializes"),
            "output": api.streamed(),
        }),
        Err(error) => json_value!({
            "error": error.to_string(),
            "output": api.streamed(),
        }),
    }
}

/// `gives the same read and bash tool results`：`createReadTool()`/`createBashTool()` 在
/// `NodeExecutionEnv` 与 `RemoteExecutionEnv` 上的结果（`result` 的完整 JSON 与流式输出）相同。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gives_the_same_read_and_bash_tool_results() {
    common::titled("gives the same read and bash tool results", async {
        let connection = Arc::new(Connection::new(vec![common::daemon_path()]));
        let local_dir = tempfile::Builder::new()
            .prefix("kiana-env-local-")
            .tempdir()
            .expect("create a temporary directory");
        let remote_dir = tempfile::Builder::new()
            .prefix("kiana-env-remote-")
            .tempdir()
            .expect("create a temporary directory");
        let local_root = local_dir.path().to_string_lossy().into_owned();
        let remote_root = remote_dir.path().to_string_lossy().into_owned();
        let local: Arc<dyn ExecutionEnv> =
            Arc::new(NodeExecutionEnv::new(NodeEnvOptions::new(&local_root)));
        let remote: Arc<dyn ExecutionEnv> =
            Arc::new(RemoteExecutionEnv::new(RemoteExecutionEnvOptions {
                connection: Arc::clone(&connection),
                id: "kiana-env:test".to_owned(),
                cwd: remote_root.clone(),
                shell_path: None,
                shell_env: None,
                watch: None,
            }));
        let context = kiana_chord::context::BACKGROUND_CONTEXT.clone();
        let files: [(&str, FileContent); 5] = [
            (
                "text.txt",
                FileContent::Text("one\ntwo\n\nthree".to_owned()),
            ),
            ("big.txt", FileContent::Text("line\n".repeat(3000))),
            (
                "long.txt",
                FileContent::Text(format!("{}\nend", "é".repeat(40_000))),
            ),
            ("bom.txt", FileContent::Text("\u{feff}hello".to_owned())),
            (
                "bad.txt",
                FileContent::Bytes(vec![0x61, 0xe2, 0x82, 0x0a, 0x62]),
            ),
        ];
        for (path, content) in files {
            local
                .write_file(path, content.clone(), context.as_ref())
                .await
                .expect("write to the local environment");
            remote
                .write_file(path, content, context.as_ref())
                .await
                .expect("write to the remote environment");
        }

        #[derive(Clone, Copy, PartialEq)]
        enum Tool {
            Read,
            Bash,
        }
        let cases: [(Tool, Value); 11] = [
            (Tool::Read, json_value!({ "path": "text.txt" })),
            (
                Tool::Read,
                json_value!({ "path": "text.txt", "offset": 2, "limit": 1 }),
            ),
            (Tool::Read, json_value!({ "path": "big.txt" })),
            (Tool::Read, json_value!({ "path": "long.txt" })),
            (Tool::Read, json_value!({ "path": "bom.txt" })),
            (Tool::Read, json_value!({ "path": "bad.txt" })),
            (Tool::Read, json_value!({ "path": "missing.txt" })),
            (
                Tool::Bash,
                json_value!({ "command": "cat text.txt; exit 0" }),
            ),
            (
                Tool::Bash,
                json_value!({ "command": "echo err >&2; exit 0" }),
            ),
            (Tool::Bash, json_value!({ "command": "exit 3" })),
            (Tool::Bash, json_value!({ "command": "printf 'a\\nb'; ls" })),
        ];
        for (tool_kind, args) in cases {
            let make = || match tool_kind {
                Tool::Read => create_read_tool(ReadToolOptions::default()),
                Tool::Bash => create_bash_tool(None),
            };
            let expected = normalize(run_tool(&local, &make(), args.clone()).await, &local_root);
            // 一个 bash 用例返回结果（非零退出是错误结果）：两边都抛异常会掩盖这个区别。
            if tool_kind == Tool::Bash {
                assert!(expected.get("result").is_some(), "{}", args);
            }
            let actual = normalize(run_tool(&remote, &make(), args.clone()).await, &remote_root);
            assert_eq!(actual, expected, "{args}");
        }
        connection.close();
    })
    .await;
}

/// 回归（review finding）：`listDir` 跳过不支持的类型（FIFO、socket、设备），而不是让整个列表
/// 失败。TS 的 `listDir` 对每个条目做 `if (info.ok) infos.push(info.value)`（remote-env.ts:717-724），
/// `NodeExecutionEnv::list_dir` 也跳过它们；这里直接对比两者。
#[cfg(not(windows))]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_dir_skips_unsupported_entries_like_node() {
    common::titled("listDir skips unsupported entries like Node", async {
        let connection = Arc::new(Connection::new(vec![common::daemon_path()]));
        let pair = pair(&connection);
        for root in [&pair.local_root, &pair.remote_root] {
            std::fs::write(format!("{root}/a.txt"), "a").expect("write a.txt");
            std::fs::create_dir(format!("{root}/sub")).expect("create sub");
            let status = std::process::Command::new("mkfifo")
                .arg(format!("{root}/fifo"))
                .status()
                .expect("mkfifo starts");
            assert!(status.success(), "mkfifo creates the FIFO");
        }
        async fn listing(env: &dyn ExecutionEnv) -> Value {
            match env.list_dir(".", common::context()).await {
                Ok(infos) => success(sorted_infos_json(&infos)),
                Err(error) => failure(error),
            }
        }
        let expected = normalize(listing(&pair.local).await, &pair.local_root);
        let actual = normalize(listing(&pair.remote).await, &pair.remote_root);
        assert_eq!(
            actual, expected,
            "listDir skips unsupported entries like Node"
        );
        match pair.remote.list_dir(".", common::context()).await {
            Ok(infos) => {
                let names: Vec<&str> = infos.iter().map(|info| info.name.as_str()).collect();
                assert_eq!(names, ["a.txt", "sub"], "the FIFO is skipped");
            }
            Err(error) => panic!("listDir must skip the FIFO: {}", error.message),
        }
        connection.close();
    })
    .await;
}

/// 回归（review finding）：POSIX 上 `listDir` 按名字的**原始字节**排序（remote-env.ts:706-710，
/// libuv 的 `scandir` 顺序）。kiana-durable 的 `NodeExecutionEnv::list_dir` 不排序，不能用来对照，
/// 所以直接断言 `RemoteExecutionEnv` 的顺序。
#[cfg(not(windows))]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_list_dir_sorts_names_by_bytes_on_posix() {
    common::titled("listDir sorts names by their bytes on POSIX", async {
        let connection = Arc::new(Connection::new(vec![common::daemon_path()]));
        let pair = pair(&connection);
        for name in ["b", "a", "c"] {
            std::fs::write(format!("{}/{}", pair.remote_root, name), "x").expect("write a file");
        }
        let infos = pair
            .remote
            .list_dir(".", common::context())
            .await
            .expect("listDir");
        let names: Vec<&str> = infos.iter().map(|info| info.name.as_str()).collect();
        assert_eq!(names, ["a", "b", "c"]);
        connection.close();
    })
    .await;
}

/// 回归（review finding）：Windows 的 `\\?\` 扩展长度前缀要剥掉，才能与
/// `realpathSync.native` 的拼写对上（differential.test.ts:53-64）。
#[test]
fn extended_length_prefixes_are_stripped_like_realpath_native() {
    assert_eq!(
        strip_extended_length_prefix(r"\\?\C:\work\file"),
        r"C:\work\file"
    );
    assert_eq!(
        strip_extended_length_prefix(r"\\?\UNC\server\share\file"),
        r"\\server\share\file"
    );
    assert_eq!(
        strip_extended_length_prefix("/tmp/kiana-env"),
        "/tmp/kiana-env"
    );
}
