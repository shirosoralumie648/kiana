//! modes/interactive/session-share.ts。
//!
//! 通过 Radius artifact 或私有 GitHub gist 分享当前会话。
//!
//! # 这个文件负责什么
//!
//! 交互模式 `/share` 命令的完整编排：把当前会话分支导出成 JSONL 或 HTML，上传到
//! Radius（组织内 artifact），或退回私有 GitHub gist，最后把可点击的链接显示在状态栏。
//! 它不定义会话格式，也不自己实现导出：JSONL 序列化在
//! `kiana_session_core::session_export`，HTML 导出在 `AgentSession::export_to_html`，
//! 预览链接拼装在 `kiana_core::config::get_share_viewer_url`。
//!
//! # 调用关系
//!
//! - 唯一生产调用方是 `interactive_mode/commands/export_import_share.rs` 的
//!   `handle_share_command`：它构造 [`SessionShareContext`] 后 `await` [`share_session`]。
//!   `Err` 表示 TS 中未被捕获的 rejection，调用方会走崩溃路径而不是普通错误提示。
//!   另有集成测试 `tests/session_share.rs` 直接调用 [`share_session`] 驱动各分支。
//! - `tests/export_jsonl_share.rs` 直接调用 [`export_session_for_share`]，锁定
//!   “分享导出不改变既有条目 id / parentId 链”这一契约。
//!
//! # 执行流程（[`share_session`]）
//!
//! 1. 建临时目录并导出 JSONL；导出失败只显示错误，随后正常返回。
//! 2. 读设置的 `sessionShareDestination`：`gist` 直接走 GitHub；`auto` 先试 Radius；
//!    `radius` 在认证缺失时直接报错，**不**回退 gist。
//! 3. gist 分支先执行 `gh auth status`，再导出 HTML 并调用 `gh gist create`。
//! 4. 无论走哪条分支，加载器都会被恢复、临时目录随 `TempDir` 的 Drop 递归删除。
//!
//! # 重要约束
//!
//! - 用户可见的错误文案要逐字复刻 Node/TypeScript 的 `error.message`，所以本文件
//!   自带一套 Node 错误格式化函数（`node_*`），不直接使用 `io::Error` 的 Display。
//! - 上传与子进程等待都可以被用户取消；取消只提示 “Share cancelled”，不再显示
//!   后续错误。
//! - 用户可见文案与分支流程是稳定契约，保真优先于惯用写法；改文案或分支前必须同步
//!   测试（历史出处见 `session-share.ts`）。

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use kiana_agent_core::types::AgentTool;
use kiana_ai::auth::types::{AuthError, AuthResult};
use kiana_ai::providers::radius_config::DEFAULT_RADIUS_GATEWAY;
use kiana_ai::types::ProviderHeaders;
use kiana_config::defaults::RADIUS_PROVIDER_ID;
use kiana_config::models::runtime::ModelRuntimeAuthOverrides;
use kiana_core::config::get_share_viewer_url;
use kiana_session::agent_session::{AgentSession, ExportThemeOptions};
use kiana_session_core::session_export::{TrailingEntries, export_session_to_jsonl};
use kiana_session_core::session_manager::SessionManagerError;
use kiana_tui::terminal_image::hyperlink;
use kiana_tui::text::is_js_whitespace;
use kiana_tui::tui::{Container, Tui};
use kiana_ui_kit::bordered_loader::BorderedLoader;
use serde_json::{Map, Value};

use crate::tui_renderer::EditorSlot;

/// TS `shareSession` 的上下文。
///
/// 把“要分享的会话”和“如何操作界面”拆开，本文件的流程只通过这里的字段与 UI 交互。
/// `editor_container` 与 `editor` 是同一套编辑器槽位：加载期间会被 loader 替换，
/// 结束时必须恢复（见 `show_loader`/`restore_editor` 与 `EditorSlot`）。
/// 两个回调由调用方注入，类型只有 `&str -> ()`，不返回错误。
pub struct SessionShareContext {
    /// 会话。
    pub session: AgentSession,
    /// TUI 句柄。
    pub ui: Rc<RefCell<dyn Tui>>,
    /// `editorContainer`。
    pub editor_container: Rc<RefCell<Container>>,
    /// 当前编辑器（overlay 结束后恢复）。
    pub editor: kiana_tui::layout_node::SharedComponent,
    /// `showStatus(message)`。
    pub show_status: Rc<dyn Fn(&str)>,
    /// `showError(message)`。
    pub show_error: Rc<dyn Fn(&str)>,
}

impl SessionShareContext {
    /// 交互模式的编辑器槽位（overlay 的挂载/恢复）。
    fn editor_slot(&self) -> EditorSlot {
        EditorSlot::new(
            Rc::clone(&self.ui),
            Rc::clone(&self.editor_container),
            Rc::clone(&self.editor),
        )
    }
}

/// `error instanceof Error ? error.message : "Unknown error"` 的本地等价物。
pub(crate) fn error_message(message: impl std::fmt::Display) -> String {
    message.to_string()
}
/// 把工具定义投影成分享数据里的 `{ name, description, parameters }`。
///
/// `parameters` 是 JSON Schema，原样克隆；对象只含这三个键，不携带工具的其他运行时
/// 字段。`tests/export_jsonl_share.rs` 逐项断言三者的值。
pub(crate) fn tool_json(tool: &AgentTool) -> Value {
    let mut object = Map::new();
    object.insert("name".to_string(), Value::String(tool.name.clone()));
    object.insert(
        "description".to_string(),
        Value::String(tool.description.clone()),
    );
    object.insert("parameters".to_string(), tool.parameters.clone());
    Value::Object(object)
}

/// 用当前会话状态构造 trailing `kiana.share` 条目。
///
/// TS 在导出回调里读取 `session.state`；导出是同步的，状态不会在中途变化，因此 Rust 在
/// 导出前取快照（回调要求 `Send + Sync`，无法捕获 `&AgentSession`）。
pub(crate) fn share_trailing_entry(
    system_prompt: &str,
    tools: &[AgentTool],
    parent_id: Option<&str>,
    timestamp: &str,
) -> Value {
    let mut data = Map::new();
    data.insert(
        "systemPrompt".to_string(),
        Value::String(system_prompt.to_string()),
    );
    data.insert(
        "tools".to_string(),
        Value::Array(tools.iter().map(tool_json).collect()),
    );

    let mut entry = Map::new();
    entry.insert("type".to_string(), Value::String("custom".to_string()));
    entry.insert(
        "customType".to_string(),
        Value::String("kiana.share".to_string()),
    );
    // TS `crypto.randomUUID().slice(0, 8)`：UUIDv4 的前 8 个十六进制字符。UUIDv7 的前
    // 8 字符是毫秒时间戳高位（同一 65.5 s 窗口内完全相同），不能用作短随机 id。
    entry.insert(
        "id".to_string(),
        Value::String(uuid::Uuid::new_v4().to_string().chars().take(8).collect()),
    );
    entry.insert(
        "parentId".to_string(),
        parent_id.map_or(Value::Null, |parent_id| {
            Value::String(parent_id.to_string())
        }),
    );
    entry.insert(
        "timestamp".to_string(),
        Value::String(timestamp.to_string()),
    );
    entry.insert("data".to_string(), Value::Object(data));
    Value::Object(entry)
}

/// `createShareTrailingEntries(session, parentId, timestamp)`。
///
/// trailing `kiana.share` 条目携带系统提示与工具 schema，供 session viewer 使用。
///
/// 返回恰好一个元素：`session.state()` 同步取快照后交给 `share_trailing_entry`；
/// `parent_id` 与 `timestamp` 直接写进条目，导出器会传入当前 leaf 与 header 时间。
/// 本函数只构造 JSON，不写盘、不校验参数。
pub fn create_share_trailing_entries(
    session: &AgentSession,
    parent_id: Option<&str>,
    timestamp: &str,
) -> Vec<Value> {
    let state = session.state();
    vec![share_trailing_entry(
        &state.system_prompt,
        &state.tools,
        parent_id,
        timestamp,
    )]
}

/// `exportSessionForShare(filePath, session)`：把当前分支连同展示元数据导出为 JSONL。
///
/// # 输入与副作用
///
/// - `file_path`：目标文件。调用底层导出器时经 `to_string_lossy` 转成 `String`，
///   因此非 UTF-8 路径中的非法字节会变成替换字符 U+FFFD；父目录不存在时会被递归创建，
///   已存在的文件会被覆盖。
/// - `session`：`state()` 在调用线程同步取快照，之后即使会话继续变化也不影响本次导出。
///   快照被移动进 `Send + Sync` 的 trailing 闭包（这是不能直接捕获 `&AgentSession` 的原因）。
///
/// # 失败
///
/// 返回 `Err(SessionManagerError)`，实际来自底层写文件/建目录的 IO 失败
/// （`SessionManagerError::Io`）。调用方用 `session_export_error` 把它转成 Node 风格文案。
pub fn export_session_for_share(
    file_path: &std::path::Path,
    session: &AgentSession,
) -> Result<(), SessionManagerError> {
    let state = session.state();
    let system_prompt = state.system_prompt;
    let tools = state.tools;
    let trailing: TrailingEntries = std::sync::Arc::new(move |parent_id, timestamp| {
        vec![share_trailing_entry(
            &system_prompt,
            &tools,
            parent_id,
            timestamp,
        )]
    });
    let path = file_path.to_string_lossy().into_owned();
    session.with_session_manager_mut(|manager| {
        export_session_to_jsonl(manager, Some(&path), Some(&trailing)).map(|_| ())
    })
}

/// `getAuthCredential(auth)`：优先 `apiKey`，否则取 `Authorization: Bearer <token>`。
///
/// 返回 `None` 表示没有非空 `apiKey`，也没有能解析出 token 的 `Authorization` 头；
/// `try_share_via_radius` 据此把 Radius 视为未配置并继续走 gist。
pub(crate) fn get_auth_credential(auth: Option<&AuthResult>) -> Option<String> {
    let auth = auth?;
    // TS `if (auth?.auth.apiKey) return auth.auth.apiKey;`：空串在 JS 里是假值，继续查
    // `Authorization` 头（否则会发出 `Authorization: Bearer `）。
    if let Some(api_key) = &auth.auth.api_key
        && !api_key.is_empty()
    {
        return Some(api_key.clone());
    }
    let authorization = bearer_header(auth.auth.headers.as_ref())?;
    parse_bearer(authorization)
}

/// 大小写不敏感地取 `authorization` 头。
///
/// 用 `to_lowercase()` 逐个头名比较，取**第一个**匹配项；若该头存在但值为 `None`，
/// 直接返回 `None`，不会继续查找后面的同名头。返回值借用自 headers，不做拷贝。
fn bearer_header(headers: Option<&ProviderHeaders>) -> Option<&str> {
    headers?
        .iter()
        .find(|(name, _)| name.to_lowercase() == "authorization")
        .and_then(|(_, value)| value.as_deref())
}

/// `/^Bearer\s+(.+)$/iu` 的捕获组。
///
/// Rust 的 `regex` 与 ECMAScript 有两处差异，因此手工匹配：
/// * `.` 在 JS 里排除全部行终止符（`\n`、`\r`、U+2028、U+2029），Rust 只排除 `\n`；
/// * `\s` 的集合不同：JS 包含 U+FEFF、不包含 U+0085（见 [`is_js_whitespace`]）。
///
/// 贪婪回溯也要复刻：`\s+` 先吃满空白，`(.+)` 至少还要匹配一个字符；若整段剩余都是
/// 空白，正则回溯后捕获的是**最后一个**空白字符（例如 `"Bearer  "` → `" "`）。
fn parse_bearer(value: &str) -> Option<String> {
    const PREFIX: &str = "Bearer";
    if !value.get(..PREFIX.len())?.eq_ignore_ascii_case(PREFIX) {
        return None;
    }
    let rest = &value[PREFIX.len()..];
    match rest
        .char_indices()
        .find(|(_, character)| !is_js_whitespace(*character))
    {
        // `\s+` 必须至少消费一个字符。
        Some((0, _)) => None,
        Some((index, _)) => {
            let captured = &rest[index..];
            // `(.+)` 不能跨越行终止符，而 `$`（无 `m` 标志）只在输入末尾匹配。
            (!captured.chars().any(is_js_line_terminator)).then(|| captured.to_string())
        }
        // 剩余全是空白：`\s+` 至少要保留一个字符给 `(.+)`。回溯只会退回最后一个字符
        // （`"Bearer  "` → `" "`）；只有一个空白字符时无法同时满足两个 `+`。
        None => {
            rest.chars().nth(1)?;
            let last = rest.chars().last()?;
            (!is_js_line_terminator(last)).then(|| last.to_string())
        }
    }
}

/// JS `.` 不匹配的四个行终止符。
fn is_js_line_terminator(character: char) -> bool {
    matches!(character, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

/// Node/libuv 的 `(code, description)`（`uv_err_name` / `uv_strerror` 的子集）。
///
/// 只有 Linux 会先把 `raw_os_error()` 的 errno 映射为 Node 的固定字符串；拿不到 errno
/// 或 errno 不在表中时，退回 `io::ErrorKind` 的通用映射。两者都不匹配返回 `None`，
/// 调用方再回退到 `io::Error` 自身的 Display。映射表是文案保真用的**子集**，不是全部 errno。
fn node_error_code_and_description(error: &std::io::Error) -> Option<(&'static str, &'static str)> {
    #[cfg(target_os = "linux")]
    if let Some(raw) = error.raw_os_error() {
        let mapped = match raw {
            1 => Some(("EPERM", "operation not permitted")),
            2 => Some(("ENOENT", "no such file or directory")),
            5 => Some(("EIO", "i/o error")),
            13 => Some(("EACCES", "permission denied")),
            16 => Some(("EBUSY", "resource busy or locked")),
            17 => Some(("EEXIST", "file already exists")),
            18 => Some(("EXDEV", "cross-device link not permitted")),
            20 => Some(("ENOTDIR", "not a directory")),
            21 => Some(("EISDIR", "illegal operation on a directory")),
            22 => Some(("EINVAL", "invalid argument")),
            23 => Some(("ENFILE", "file table overflow")),
            24 => Some(("EMFILE", "too many open files")),
            26 => Some(("ETXTBSY", "text file is busy")),
            27 => Some(("EFBIG", "file too large")),
            28 => Some(("ENOSPC", "no space left on device")),
            30 => Some(("EROFS", "read-only file system")),
            31 => Some(("EMLINK", "too many links")),
            32 => Some(("EPIPE", "broken pipe")),
            36 => Some(("ENAMETOOLONG", "name too long")),
            40 => Some(("ELOOP", "too many symbolic links encountered")),
            122 => Some(("EDQUOT", "disk quota exceeded")),
            _ => None,
        };
        if let Some(mapped) = mapped {
            return Some(mapped);
        }
    }
    use std::io::ErrorKind;
    match error.kind() {
        ErrorKind::NotFound => Some(("ENOENT", "no such file or directory")),
        ErrorKind::PermissionDenied => Some(("EACCES", "permission denied")),
        ErrorKind::IsADirectory => Some(("EISDIR", "illegal operation on a directory")),
        ErrorKind::NotADirectory => Some(("ENOTDIR", "not a directory")),
        ErrorKind::AlreadyExists => Some(("EEXIST", "file already exists")),
        ErrorKind::StorageFull => Some(("ENOSPC", "no space left on device")),
        ErrorKind::ReadOnlyFilesystem => Some(("EROFS", "read-only file system")),
        ErrorKind::FileTooLarge => Some(("EFBIG", "file too large")),
        _ => None,
    }
}

/// Node `fs.writeFileSync`（open → write）失败时的 `error.message`：
/// `<CODE>: <description>, open '<path>'`，写入阶段的错误（`ENOSPC`/`EDQUOT`/`EFBIG`）
/// 只写 `<CODE>: <description>, write`。
///
/// TS 的 `writeZipArchive`/`writeFileSync` 直接抛出 Node 的 fs 异常，`reportBug` 与
/// `shareSession` 只显示 `error.message`；Rust `io::Error` 的 Display 缺 code/路径。
///
/// “哪个阶段失败”只按 errno code 判断：`ENOSPC`/`EDQUOT`/`EFBIG` 视为 `write`，其余视为
/// `open '<path>'`。拿不到可映射的 code 时退回 `io::Error` 自身的 Display；路径用
/// `Path::display` 渲染，非 UTF-8 字节同样会变成替换字符。
pub(crate) fn node_write_file_error(error: &std::io::Error, path: &Path) -> String {
    let Some((code, description)) = node_error_code_and_description(error) else {
        return error_message(error);
    };
    match code {
        "ENOSPC" | "EDQUOT" | "EFBIG" => format!("{code}: {description}, write"),
        _ => format!("{code}: {description}, open '{}'", path.display()),
    }
}

/// Node `fs.readFileSync` 失败时的 `error.message`：打开阶段是 `open '<path>'`，
/// 读阶段（目录）是 `EISDIR: illegal operation on a directory, read`（无路径）。
fn node_read_file_error(error: &std::io::Error, path: &Path) -> String {
    let Some((code, description)) = node_error_code_and_description(error) else {
        return error_message(error);
    };
    if code == "EISDIR" {
        format!("{code}: {description}, read")
    } else {
        format!("{code}: {description}, open '{}'", path.display())
    }
}

/// Node `fs.mkdirSync` 失败时的 `error.message`：`<CODE>: <description>, mkdir '<path>'`。
pub(crate) fn node_mkdir_error(error: &std::io::Error, path: &Path) -> String {
    let Some((code, description)) = node_error_code_and_description(error) else {
        return error_message(error);
    };
    format!("{code}: {description}, mkdir '{}'", path.display())
}

/// Node `fs.mkdtempSync` 失败时的 `error.message`（模板路径以 `XXXXXX` 结尾）。
fn node_mkdtemp_error(error: &std::io::Error, template: &Path) -> String {
    let Some((code, description)) = node_error_code_and_description(error) else {
        return error_message(error);
    };
    format!("{code}: {description}, mkdtemp '{}'", template.display())
}

/// Node `process.cwd()` 在 cwd 被删除时的 `error.message`（Node 26 / libuv `uv_cwd`）。
///
/// 能映射 code 时用 `io::Error::new(kind, 新文案)` 重建，保留原 `ErrorKind`；映射不到时
/// 原样返回传入的 error。
pub(crate) fn node_cwd_error(error: std::io::Error) -> std::io::Error {
    let Some((code, description)) = node_error_code_and_description(&error) else {
        return error;
    };
    std::io::Error::new(
        error.kind(),
        format!(
            "{code}: process.cwd failed with error {description}, the current working directory was likely removed without changing the working directory, uv_cwd"
        ),
    )
}

/// Node `child_process.spawn` 启动失败时的 `error.message`：`spawn <command> <CODE>`。
fn node_spawn_error(command: &str, error: &std::io::Error) -> String {
    let code = node_error_code_and_description(error).map_or("UNKNOWN", |(code, _)| code);
    format!("spawn {command} {code}")
}

/// `SessionManagerError::Display` 的 Node 版：`Io` 变体是 `writeFileSync` 抛出的 fs 异常
/// （`ENOENT: no such file or directory, open '<path>'`），其余变体逐字透传。
pub(crate) fn session_export_error(error: &SessionManagerError, path: &Path) -> String {
    match error {
        SessionManagerError::Io(io) => node_write_file_error(io, path),
        other => error_message(other),
    }
}

/// `shareSession(context)`：先试 Radius，再回退到私有 gist。
///
/// 返回 `Err` 对应 TS 里 `shareSession` 的 rejection：`modelRuntime.getAuth("radius")`
/// 的拒绝（该 await 位于所有 try/catch 之外）与 `fs.mkdtempSync` 的失败（位于
/// try/finally 之外，没有 shareSession 生成的文案）。调用方的 `await` 收到 rejection，
/// 分享中止，绝不回退到 gist。
///
/// # 行为
///
/// - 临时目录创建失败是最先发生的 `Err`；之后 JSONL 导出失败只 `show_error` 并返回
///   `Ok(())`。
/// - `destination` 为 `Radius` 且没有可用凭据时，显示登录提示后返回 `Ok(())`，不落到
///   gist；只有 `Auto` 才会继续尝试 gist。
/// - `gh` 未安装或未登录都归一成同一条 “not logged in” 提示（原因见下方 `gh auth status`
///   处的注释）。
/// - 成功时状态栏显示链接：Radius 用响应里的 `canonical_url`，gist 由 viewer URL 与
///   gist URL 组成。
///
/// # 并发与生命周期
///
/// 在调用方的异步任务里运行；内部用 `tokio::select!` 与 loader 的取消令牌竞速。
/// 临时目录由 `TempDir` 在所有返回路径上自动删除，本函数不启动后台任务。
pub async fn share_session(context: SessionShareContext) -> Result<(), AuthError> {
    let temp_dir = match tempfile::Builder::new().prefix("kiana-share-").tempdir() {
        Ok(temp_dir) => temp_dir,
        Err(error) => {
            // TS: `fs.mkdtempSync(path.join(os.tmpdir(), "kiana-share-"))` 在 try 之外抛出。
            let template = std::env::temp_dir().join("kiana-share-XXXXXX");
            return Err(AuthError::new(node_mkdtemp_error(&error, &template)));
        }
    };
    let jsonl_file = temp_dir.path().join("session.jsonl");
    let html_file = temp_dir.path().join("session.html");

    if let Err(error) = export_session_for_share(&jsonl_file, &context.session) {
        (context.show_error)(&format!(
            "Failed to export session: {}",
            session_export_error(&error, &jsonl_file)
        ));
        return Ok(());
    }

    let destination = context
        .session
        .settings_manager()
        .get_session_share_destination();
    if destination != kiana_config::settings::schema::SessionShareDestination::Gist {
        match try_share_via_radius(&jsonl_file, &context).await {
            Ok(true) => return Ok(()),
            Ok(false) => {}
            Err(error) => return Err(error),
        }
        if destination == kiana_config::settings::schema::SessionShareDestination::Radius {
            (context.show_error)(
                "Radius authentication is not configured. Run '/login radius' first.",
            );
            return Ok(());
        }
    }

    // TS 用 `spawnSync("gh", ["auth", "status"])` 并检查 `authResult.status !== 0`。
    // Node 的 `spawnSync` 对 ENOENT/EACCES **不抛异常**（返回 `{status: null, error}`），
    // 所以二进制缺失时走的是 `status !== 0` 分支而不是 `catch`：两条路径的文案相同。
    // 这里把 spawn 失败同样映射到该分支（§4.4 的"抛异常 → not installed"描述与运行时
    // 不符，以 TS 源码的实际行为为准）。
    let authenticated = std::process::Command::new("gh")
        .args(["auth", "status"])
        .output()
        .is_ok_and(|output| output.status.success());
    if !authenticated {
        (context.show_error)("GitHub CLI is not logged in. Run 'gh auth login' first.");
        return Ok(());
    }

    let theme_name = kiana_theme::current_theme_name();
    if let Err(error) = context.session.export_to_html(
        Some(html_file.to_string_lossy().into_owned()),
        Some(ExportThemeOptions { theme_name }),
    ) {
        (context.show_error)(&format!(
            "Failed to export session: {}",
            error_message(error)
        ));
        return Ok(());
    }
    share_via_gist(&html_file, &context).await;
    // `temp_dir` 在作用域结束时递归删除（TS `finally` 的 `rmSync`）。
    Ok(())
}

/// 创建并挂载加载器；返回共享句柄与取消令牌。
///
/// 成对契约：调用方必须在**每一个**出口调用 `restore_editor`，否则编辑器槽位会一直被
/// loader 占着。这里不注册 `onAbort`，取消处理完全由调用方对令牌做 `tokio::select!`；
/// 返回的令牌来自默认（可取消）的 `BorderedLoader`。
fn show_loader(
    context: &SessionShareContext,
    message: &str,
) -> (
    Rc<RefCell<BorderedLoader>>,
    tokio_util::sync::CancellationToken,
) {
    let loader = crate::tui_renderer::create_bordered_loader(&context.ui, message);
    let signal = loader.borrow().signal();
    let slot = context.editor_slot();
    slot.show(Box::new(crate::tui_renderer::SharedLoader::new(Rc::clone(
        &loader,
    ))));
    (loader, signal)
}

/// `loader.dispose(); editorContainer.clear(); addChild(editor); setFocus(editor)`。
///
/// 先释放 loader（`dispose` 只停止动画，不取消令牌），再恢复槽位。与 `bug-report.ts`
/// 不同，`session-share.ts` 的对应收尾没有 `requestRender()`，`EditorSlot::restore` 也
/// 不请求渲染。
fn restore_editor(slot: &EditorSlot, loader: &Rc<RefCell<BorderedLoader>>) {
    loader.borrow_mut().dispose();
    slot.restore();
}

/// `tryShareViaRadius(tmpFile, context)`：返回 `Ok(true)` 表示已处理（成功、失败或取消）。
/// `getAuth` 的拒绝以 `Err` 冒泡（TS 中它没有被 try/catch 捕获）。
///
/// # 阶段
///
/// 1. provider 不存在 → `Ok(false)`，交给 gist。
/// 2. 取凭据（要求 OAuth 至少还有 5 分钟有效期）；解析不出 token → `Ok(false)`。
/// 3. 挂 loader，读 JSONL、拼 URL、POST；发送请求与读取响应体都与取消令牌竞速。
/// 4. 成功后显示链接；其余失败显示错误并返回 `Ok(true)`（已处理，不再走 gist）。
///
/// # 取消
///
/// 每个 await 点取消后都会先恢复编辑器再提示 “Share cancelled”；已发出的请求不再等待
/// 响应或读取 body。
async fn try_share_via_radius(
    jsonl_file: &std::path::Path,
    context: &SessionShareContext,
) -> Result<bool, AuthError> {
    let model_runtime = context.session.model_runtime();
    if model_runtime.get_provider(RADIUS_PROVIDER_ID).is_none() {
        return Ok(false);
    }
    let auth = model_runtime
        .get_auth_for_provider(
            RADIUS_PROVIDER_ID,
            ModelRuntimeAuthOverrides {
                min_oauth_validity_ms: Some(5 * 60_000),
                ..Default::default()
            },
        )
        .await?;
    let token = get_auth_credential(auth.as_ref());
    let Some(token) = token else {
        return Ok(false);
    };

    let (loader, signal) = show_loader(context, "Uploading to Radius...");
    let slot = context.editor_slot();

    let body = match std::fs::read(jsonl_file) {
        Ok(body) => body,
        Err(error) => {
            restore_editor(&slot, &loader);
            (context.show_error)(&format!(
                "Failed to upload Radius artifact: {}",
                node_read_file_error(&error, jsonl_file)
            ));
            return Ok(true);
        }
    };

    let url = match radius_artifacts_url() {
        Ok(url) => url,
        Err(error) => {
            restore_editor(&slot, &loader);
            (context.show_error)(&format!(
                "Failed to upload Radius artifact: {}",
                error_message(error)
            ));
            return Ok(true);
        }
    };

    let client = kiana_core::http_dispatcher::shared_http_client();
    let request = client
        .post(url)
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/x-ndjson")
        .header("Content-Length", body.len())
        .body(body);

    let response = tokio::select! {
        _ = signal.cancelled() => None,
        response = request.send() => Some(response),
    };
    let Some(response) = response else {
        // TS `onAbort`：恢复编辑器并提示取消。
        restore_editor(&slot, &loader);
        (context.show_status)("Share cancelled");
        return Ok(true);
    };
    let response = match response {
        Ok(response) => response,
        Err(error) => {
            // TS 的 catch 先看 `loader.signal.aborted`：已取消时 onAbort 已经恢复编辑器并
            // 显示 "Share cancelled"，错误不再显示。
            restore_editor(&slot, &loader);
            if signal.is_cancelled() {
                (context.show_status)("Share cancelled");
            } else {
                (context.show_error)(&format!(
                    "Failed to upload Radius artifact: {}",
                    error_message(error)
                ));
            }
            return Ok(true);
        }
    };
    if signal.is_cancelled() {
        restore_editor(&slot, &loader);
        (context.show_status)("Share cancelled");
        return Ok(true);
    }

    let status = response.status();
    let status_text = status.canonical_reason().unwrap_or_default().to_string();
    let status_code = status.as_u16();
    // TS `await response.json().catch(() => null)` 同样受 AbortSignal 控制：body 读取期间
    // 取消时 fetch 立即 reject，onAbort 恢复编辑器。这里与取消令牌竞速。
    let json: Option<Value> = tokio::select! {
        _ = signal.cancelled() => {
            restore_editor(&slot, &loader);
            (context.show_status)("Share cancelled");
            return Ok(true);
        }
        text = response.text() => match text {
            Ok(text) => serde_json::from_str(&text).ok(),
            Err(_) => None,
        },
    };
    if signal.is_cancelled() {
        restore_editor(&slot, &loader);
        (context.show_status)("Share cancelled");
        return Ok(true);
    }
    restore_editor(&slot, &loader);

    // TS 只要求 `response.ok && json?.artifact`（任意真值）；随后直接读
    // `artifact.canonical_url` 并插值（缺失 → `"undefined"`）。
    let Some(share_url) = radius_share_url(status.is_success(), json.as_ref()) else {
        let detail = radius_upload_error_detail(json.as_ref(), &status_text, status_code);
        (context.show_error)(&format!("Failed to upload Radius artifact: {detail}"));
        return Ok(true);
    };
    (context.show_status)(&format!("Share URL: {}", hyperlink(&share_url, &share_url)));
    Ok(true)
}

/// TS `if (!response.ok || !json?.artifact) { … } const shareUrl = json.artifact.canonical_url;`
///
/// `artifact` 只需是真值；`canonical_url` 缺失/非字符串时按 `String(value)` 插值
/// （`undefined`、`null`、`42` …）。
fn radius_share_url(status_ok: bool, json: Option<&Value>) -> Option<String> {
    if !status_ok {
        return None;
    }
    let artifact = json.and_then(|json| json.get("artifact"));
    if !kiana_json::js_string::is_truthy(artifact) {
        return None;
    }
    Some(kiana_json::js_string::js_string(
        artifact.and_then(|artifact| artifact.get("canonical_url")),
    ))
}

/// TS `` `${json?.error || response.statusText || response.status}` ``。
///
/// `||` 按 JS 真值判断，`${}` 是 `String(value)`（对象 → `"[object Object]"`、数组按 `","`
/// 连接、数字按 JS `Number::toString`）。`status_text` 是 `Response.statusText`：
/// 本进程的 fetch 被 `configureHttpDispatcher`（`allowH2: false`）固定为 HTTP/1.1，Rust 的
/// 共享 client 同样 `http1_only`；reqwest/hyper 不暴露 reason phrase，标准短语用
/// `canonical_reason()` 复刻，自定义短语无法观察（TS 会原样输出）。
fn radius_upload_error_detail(json: Option<&Value>, status_text: &str, status_code: u16) -> String {
    let error = json.and_then(|json| json.get("error"));
    if kiana_json::js_string::is_truthy(error) {
        return kiana_json::js_string::js_string(error);
    }
    if !status_text.is_empty() {
        return status_text.to_string();
    }
    status_code.to_string()
}

/// `new URL("/v1/artifacts", DEFAULT_RADIUS_GATEWAY)` + `visibility=organization` +
/// `title=Kiana session`。
fn radius_artifacts_url() -> Result<url::Url, url::ParseError> {
    let base = url::Url::parse(DEFAULT_RADIUS_GATEWAY)?;
    let mut url = base.join("/v1/artifacts")?;
    url.query_pairs_mut()
        .append_pair("visibility", "organization")
        .append_pair("title", "Kiana session");
    Ok(url)
}

/// `shareViaGist(tmpFile, context)`。
///
/// 以 `gh gist create --public=false <html>` 创建私有 gist；把 `stdout.trim()` 当作 URL，
/// 取其最后一个 `/` 之后的部分作为 gist id，再用 `get_share_viewer_url` 拼预览地址。
/// 函数无返回值：成功、失败与取消全部经 `show_status`/`show_error` 汇报（唯一例外：
/// 取消与 spawn/等待失败竞速时只恢复编辑器，不再提示——两个失败分支都以
/// `!signal.is_cancelled()` 为前提）。
///
/// 取消通过 `kill_on_drop(true)` 终止子进程。`gh` 启动失败、等待进程失败、非零退出
/// （stderr 为空时回退 “Unknown error”）各有独立文案分支，见函数体。
async fn share_via_gist(html_file: &std::path::Path, context: &SessionShareContext) {
    let (loader, signal) = show_loader(context, "Creating gist...");
    let slot = context.editor_slot();

    let mut command = tokio::process::Command::new("gh");
    command
        .args([
            "gist",
            "create",
            "--public=false",
            &html_file.to_string_lossy(),
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        // TS `proc?.kill()`（onAbort）：select 取消时 drop 子进程会杀掉它。
        .kill_on_drop(true);
    let child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            restore_editor(&slot, &loader);
            if !signal.is_cancelled() {
                // TS 的 `catch` 打印 `error.message`；Node 的启动失败文案是
                // `spawn gh ENOENT`（不是 Rust 的 `No such file or directory (os error 2)`）。
                (context.show_error)(&format!(
                    "Failed to create gist: {}",
                    node_spawn_error("gh", &error)
                ));
            }
            return;
        }
    };

    let output = tokio::select! {
        _ = signal.cancelled() => None,
        output = child.wait_with_output() => Some(output),
    };
    let Some(output) = output else {
        restore_editor(&slot, &loader);
        (context.show_status)("Share cancelled");
        return;
    };
    let output = match output {
        Ok(output) => output,
        Err(error) => {
            restore_editor(&slot, &loader);
            if !signal.is_cancelled() {
                (context.show_error)(&format!("Failed to create gist: {}", error_message(error)));
            }
            return;
        }
    };
    if signal.is_cancelled() {
        restore_editor(&slot, &loader);
        (context.show_status)("Share cancelled");
        return;
    }
    restore_editor(&slot, &loader);

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() {
        // TS `result.stderr?.trim() || "Unknown error"`：JS 的 trim（含 U+FEFF、不含 U+0085）。
        let detail = kiana_tui::text::trim_js(&stderr);
        let detail = if detail.is_empty() {
            "Unknown error".to_string()
        } else {
            detail.to_string()
        };
        (context.show_error)(&format!("Failed to create gist: {detail}"));
        return;
    }

    let gist_url = kiana_tui::text::trim_js(&stdout).to_string();
    let gist_id = gist_url.rsplit('/').next().unwrap_or("");
    if gist_id.is_empty() {
        (context.show_error)("Failed to parse gist ID from gh output");
        return;
    }
    let status = match get_share_viewer_url(gist_id) {
        Some(preview_url) => format!(
            "Share URL: {}\nGist: {}",
            hyperlink(&preview_url, &preview_url),
            hyperlink(&gist_url, &gist_url)
        ),
        None => format!(
            "Gist: {}\nShare viewer URL is not configured (set KIANA_SHARE_VIEWER_URL or KIANA_API_BASE_URL).",
            hyperlink(&gist_url, &gist_url)
        ),
    };
    (context.show_status)(&status);
}

#[cfg(test)]
mod tests {
    //! 无会话 fixture 的纯函数回归：Bearer 解析、Node fs 文案、Radius 响应判读与
    //! `kiana.share` 条目结构。需要真实 `SessionManager` 的导出行为在
    //! `tests/export_jsonl_share.rs`。
    use super::*;
    use kiana_ai::auth::types::ModelAuth;
    use serde_json::json;

    fn auth_with(api_key: Option<&str>, authorization: Option<&str>) -> AuthResult {
        let mut headers = ProviderHeaders::new();
        if let Some(value) = authorization {
            headers.insert("Authorization".to_string(), Some(value.to_string()));
        }
        AuthResult::new(ModelAuth {
            api_key: api_key.map(str::to_string),
            headers: (!headers.is_empty()).then_some(headers),
            base_url: None,
        })
    }

    #[test]
    fn treats_an_empty_api_key_as_absent_and_falls_back_to_the_bearer_header() {
        let auth = auth_with(Some(""), Some("Bearer header-token"));
        assert_eq!(
            get_auth_credential(Some(&auth)),
            Some("header-token".to_string())
        );
        assert_eq!(get_auth_credential(Some(&auth_with(Some(""), None))), None);
        assert_eq!(
            get_auth_credential(Some(&auth_with(Some("key"), None))),
            Some("key".to_string())
        );
        assert_eq!(
            get_auth_credential(Some(&auth_with(None, Some("bogus")))),
            None
        );
        assert_eq!(get_auth_credential(None), None);
    }

    #[test]
    fn formats_the_radius_upload_error_like_a_js_template_literal() {
        assert_eq!(
            radius_upload_error_detail(Some(&json!({ "error": "boom" })), "Unauthorized", 401),
            "boom"
        );
        // 空串 / null 是假值（`||`），继续回退到 statusText。
        assert_eq!(
            radius_upload_error_detail(Some(&json!({ "error": "" })), "Unauthorized", 401),
            "Unauthorized"
        );
        assert_eq!(
            radius_upload_error_detail(Some(&json!({ "error": null })), "Unauthorized", 401),
            "Unauthorized"
        );
        // 非字符串按 `String(value)` 插值。
        assert_eq!(
            radius_upload_error_detail(Some(&json!({ "error": { "code": 1 } })), "", 401),
            "[object Object]"
        );
        assert_eq!(
            radius_upload_error_detail(Some(&json!({ "error": 42 })), "", 401),
            "42"
        );
        assert_eq!(
            radius_upload_error_detail(Some(&json!({ "error": [1, "a", null] })), "", 401),
            "1,a,"
        );
        // 没有 statusText 时回退数字状态码。
        assert_eq!(radius_upload_error_detail(None, "", 401), "401");
        assert_eq!(radius_upload_error_detail(Some(&json!({})), "", 401), "401");
    }

    #[test]
    fn parses_bearer_headers_with_ecmascript_semantics() {
        // 报文来自 Node 26 的 `/^Bearer\s+(.+)$/iu.exec` 实测。
        for (header, expected) in [
            ("Bearer abc", Some("abc")),
            ("bearer\tabc", Some("abc")),
            ("BEARER abc", Some("abc")),
            ("Bearer abc def", Some("abc def")),
            ("Bearer abc\r", None),
            ("Bearer abc\n", None),
            ("Bearer abc\u{2028}", None),
            ("Bearer\u{feff}abc", Some("abc")),
            ("Bearer\u{85}abc", None),
            ("Bearer  ", Some(" ")),
            ("Bearer\u{3000}", None),
            ("Bearer\u{3000}\u{3000}", Some("\u{3000}")),
            ("Bearer\n ", Some(" ")),
            ("Bearer \n", None),
            ("Bearer  \n", None),
            ("Bearer\t\nx", Some("x")),
            ("Bearer", None),
            ("Bearer ", None),
            ("Bearer\n", None),
            ("BearerX abc", None),
            ("bogus", None),
        ] {
            assert_eq!(
                parse_bearer(header),
                expected.map(str::to_string),
                "{header:?}"
            );
        }
    }

    #[test]
    fn formats_node_fs_write_errors_with_open_or_write_syscalls() {
        let path = Path::new("/tmp/kiana-bug-report-abc.zip");
        assert_eq!(
            node_write_file_error(&std::io::Error::from_raw_os_error(2), path),
            "ENOENT: no such file or directory, open '/tmp/kiana-bug-report-abc.zip'"
        );
        assert_eq!(
            node_write_file_error(&std::io::Error::from_raw_os_error(13), path),
            "EACCES: permission denied, open '/tmp/kiana-bug-report-abc.zip'"
        );
        assert_eq!(
            node_write_file_error(&std::io::Error::from_raw_os_error(21), path),
            "EISDIR: illegal operation on a directory, open '/tmp/kiana-bug-report-abc.zip'"
        );
        assert_eq!(
            node_write_file_error(&std::io::Error::from_raw_os_error(28), path),
            "ENOSPC: no space left on device, write"
        );
        assert_eq!(
            node_write_file_error(&std::io::Error::from_raw_os_error(27), path),
            "EFBIG: file too large, write"
        );
        assert_eq!(
            node_write_file_error(&std::io::Error::from_raw_os_error(122), path),
            "EDQUOT: disk quota exceeded, write"
        );
        assert_eq!(
            node_mkdtemp_error(
                &std::io::Error::from_raw_os_error(2),
                Path::new("/nonexistent/kiana-share-XXXXXX")
            ),
            "ENOENT: no such file or directory, mkdtemp '/nonexistent/kiana-share-XXXXXX'"
        );
        assert_eq!(
            node_read_file_error(&std::io::Error::from_raw_os_error(21), Path::new("/tmp")),
            "EISDIR: illegal operation on a directory, read"
        );
        assert_eq!(
            node_read_file_error(&std::io::Error::from_raw_os_error(2), path),
            "ENOENT: no such file or directory, open '/tmp/kiana-bug-report-abc.zip'"
        );
        assert_eq!(
            node_spawn_error("gh", &std::io::Error::from_raw_os_error(2)),
            "spawn gh ENOENT"
        );
        assert_eq!(
            node_cwd_error(std::io::Error::from_raw_os_error(2)).to_string(),
            "ENOENT: process.cwd failed with error no such file or directory, the current working directory was likely removed without changing the working directory, uv_cwd"
        );
        assert_eq!(
            session_export_error(
                &SessionManagerError::EntryNotFound {
                    id: "entry-1".to_string()
                },
                path
            ),
            "Entry entry-1 not found"
        );
        assert_eq!(
            node_spawn_error("gh", &std::io::Error::from_raw_os_error(13)),
            "spawn gh EACCES"
        );
    }

    #[test]
    fn accepts_any_truthy_radius_artifact_like_the_ts_flow() {
        assert_eq!(
            radius_share_url(
                true,
                Some(&json!({ "artifact": { "canonical_url": "https://r/x" } }))
            ),
            Some("https://r/x".to_string())
        );
        // `artifact` 只要真值即可；缺失 canonical_url → `String(undefined)`。
        assert_eq!(
            radius_share_url(true, Some(&json!({ "artifact": {} }))),
            Some("undefined".to_string())
        );
        assert_eq!(
            radius_share_url(true, Some(&json!({ "artifact": "not-an-object" }))),
            Some("undefined".to_string())
        );
        assert_eq!(
            radius_share_url(true, Some(&json!({ "artifact": { "canonical_url": 42 } }))),
            Some("42".to_string())
        );
        assert_eq!(
            radius_share_url(
                true,
                Some(&json!({ "artifact": { "canonical_url": null } }))
            ),
            Some("null".to_string())
        );
        // 假值 artifact / 缺失 artifact / 非 2xx 都是失败。
        assert_eq!(
            radius_share_url(true, Some(&json!({ "artifact": null }))),
            None
        );
        assert_eq!(
            radius_share_url(true, Some(&json!({ "error": "boom" }))),
            None
        );
        assert_eq!(radius_share_url(true, None), None);
        assert_eq!(
            radius_share_url(
                false,
                Some(&json!({ "artifact": { "canonical_url": "https://r/x" } }))
            ),
            None
        );
    }

    /// TS `crypto.randomUUID().slice(0, 8)`：每个条目的短 id 必须来自随机 UUIDv4。
    ///
    /// 回归：UUIDv7 的前 8 个字符是 48 位毫秒时间戳的高 32 位，同一 65.5 s 窗口内完全
    /// 相同（同一会话的两次分享会得到重复 `id`）；且生成失败时退化成空串。
    #[test]
    fn share_entry_ids_are_eight_random_lowercase_hex_characters() {
        let ids: Vec<String> = (0..3)
            .map(|_| {
                let entry = share_trailing_entry("", &[], None, "2026-01-01T00:00:00.000Z");
                entry
                    .get("id")
                    .and_then(Value::as_str)
                    .expect("share id")
                    .to_string()
            })
            .collect();
        for id in &ids {
            assert_eq!(id.len(), 8, "{id:?}");
            assert!(
                id.chars()
                    .all(|character| character.is_ascii_digit() || ('a'..='f').contains(&character)),
                "{id:?} must be lowercase hex"
            );
        }
        assert_ne!(ids[0], ids[1], "share ids must be random per entry");
        assert_ne!(ids[1], ids[2], "share ids must be random per entry");
    }

    /// TS 的 `fs.writeFileSync` 抛出 Node fs 异常，`error.message` 含 errno code；Rust
    /// `io::Error` 的 Display 既没有 code 也没有路径，必须按 Node 的形状重建。
    ///
    /// 回归：旧实现用 `format!("{}: {}", error.kind(), error)`，用户会看到
    /// `No space left on device (os error 28): No space left on device (os error 28)`。
    #[cfg(target_os = "linux")]
    #[test]
    fn write_errors_use_the_node_fs_message_shape() {
        let path = Path::new("/tmp/kiana-share/session.jsonl");
        assert_eq!(
            node_write_file_error(&std::io::Error::from_raw_os_error(28), path),
            "ENOSPC: no space left on device, write"
        );
        assert_eq!(
            node_write_file_error(&std::io::Error::from_raw_os_error(2), path),
            "ENOENT: no such file or directory, open '/tmp/kiana-share/session.jsonl'"
        );
        assert_eq!(
            node_write_file_error(&std::io::Error::from_raw_os_error(13), path),
            "EACCES: permission denied, open '/tmp/kiana-share/session.jsonl'"
        );
    }
}
