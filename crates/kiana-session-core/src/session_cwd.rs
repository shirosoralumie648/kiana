//! 持久化会话的 cwd 校验。
//!
//! TS: `packages/coding-agent/src/core/session-cwd.ts`。
//!
//! # 为什么需要它
//!
//! 会话文件的 header 记录了创建时的 cwd。恢复会话时那个目录可能已经被删除或移动；本模块
//! 把这种情况转成可诊断的 issue / 错误，让调用方在创建运行期之前决定是否回退到当前 cwd。
//! 它只**检测并格式化**，不修改 cwd、也不做任何文件系统写操作。
//!
//! # 规则
//!
//! [`crate::session_cwd::get_missing_session_cwd_issue`] 在以下任一条件下返回 `None`
//! （不报告问题）：
//!
//! - 会话没有关联文件（内存态会话）——`getSessionFile()` 为 `None`；
//! - 会话记录的 cwd 是空串；
//! - `Path::exists` 认为该路径存在。
//!
//! 注意这里只判断“路径存在”，不检查它是不是目录；检测与真正使用工作目录之间路径仍可能
//! 发生变化（删除/重命名），本模块不解决这种竞态。
//!
//! # 调用方
//!
//! - `kiana-coding-agent` 启动时用
//!   [`crate::session_cwd::get_missing_session_cwd_issue`] 决定是否提示用户选择新的 cwd；
//! - `kiana-session` 的运行时在 resume / 切换会话前用
//!   [`crate::session_cwd::assert_session_cwd_exists`] 把问题变成 `MissingSessionCwdError`；
//! - `kiana-interactive` 用 [`crate::session_cwd::format_missing_session_cwd_prompt`]
//!   渲染选择提示。
//!
//! # 类型说明
//!
//! [`crate::session_cwd::SessionCwdIssue::session_file`] 是 `Option<String>` 以对应 TS 的
//! `sessionFile?: string`；本模块的
//! [`crate::session_cwd::get_missing_session_cwd_issue`] 在无文件时提前返回，
//! 因此它产生的 issue 总是 `Some`。格式化函数仍兼容 `None`（外部构造的 issue）。

use crate::session_manager::ReadonlySessionManager;

/// 持久化会话记录的 cwd 不存在时的诊断信息。
///
/// 字段都会被展示给用户：`session_cwd` 是失效路径，`fallback_cwd` 是建议改用的当前目录。
/// TS: `session-cwd.ts#SessionCwdIssue`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionCwdIssue {
    /// 会话文件路径（内存态会话没有）。
    pub session_file: Option<String>,
    /// 会话记录里的 cwd。
    pub session_cwd: String,
    /// 回退使用的当前 cwd。
    pub fallback_cwd: String,
}

/// 提供 cwd 与 session 文件的只读来源。
///
/// TS 是结构化类型（duck typing），Rust 用这个窄 trait 对应：任何能提供这两个方法的类型
/// 都可以参与检测，不必实现完整的会话管理器。
///
/// TS: `session-cwd.ts#SessionCwdSource`
pub trait SessionCwdSource {
    /// `getCwd()`。
    fn get_cwd(&self) -> &str;
    /// `getSessionFile()`。
    fn get_session_file(&self) -> Option<&str>;
}

/// 让所有 `ReadonlySessionManager` 自动满足 `SessionCwdSource`。
///
/// 覆盖 `?Sized` 目标，因此 `&dyn ReadonlySessionManager` 也能直接传入本模块的函数。
impl<T: ReadonlySessionManager + ?Sized> SessionCwdSource for T {
    fn get_cwd(&self) -> &str {
        ReadonlySessionManager::get_cwd(self)
    }
    fn get_session_file(&self) -> Option<&str> {
        ReadonlySessionManager::get_session_file(self)
    }
}

/// 检测持久化会话的 cwd 是否缺失，返回诊断信息。
///
/// 输入：`session_manager` 提供会话文件路径与记录里的 cwd；`fallback_cwd` 是当前进程/运行
/// 时使用的 cwd，仅用于填充 issue，不参与判断。
///
/// 返回：无需提示（内存态会话、空 cwd、路径存在）时为 `None`，否则为
/// `Some(SessionCwdIssue)`。
///
/// 副作用：只调用 `Path::exists`（一次元数据查询），不修改任何状态。
///
/// TS: `session-cwd.ts#getMissingSessionCwdIssue`
pub fn get_missing_session_cwd_issue(
    session_manager: &(impl SessionCwdSource + ?Sized),
    fallback_cwd: &str,
) -> Option<SessionCwdIssue> {
    // 无 session 文件 = 内存态会话：没有可校验的持久化 cwd，直接放行。
    let session_file = session_manager.get_session_file()?.to_string();
    let session_cwd = session_manager.get_cwd();
    // 空 cwd 视为“未记录”；非空则要求路径存在。
    if session_cwd.is_empty() || std::path::Path::new(session_cwd).exists() {
        return None;
    }
    Some(SessionCwdIssue {
        session_file: Some(session_file),
        session_cwd: session_cwd.to_string(),
        fallback_cwd: fallback_cwd.to_string(),
    })
}

/// 把 issue 格式化成用户可见的错误消息（`MissingSessionCwdError` 的 `message`）。
///
/// 有会话文件且非空时，第二行插入 `\nSession file: <path>`；否则省略该行。文本与 TS 逐字
/// 一致（调用方按纯文本展示，不做额外本地化）。
/// TS: `session-cwd.ts#formatMissingSessionCwdError`
pub fn format_missing_session_cwd_error(issue: &SessionCwdIssue) -> String {
    let session_file = match &issue.session_file {
        Some(session_file) if !session_file.is_empty() => {
            format!("\nSession file: {session_file}")
        }
        _ => String::new(),
    };
    format!(
        "Stored session working directory does not exist: {}{session_file}\nCurrent working directory: {}",
        issue.session_cwd, issue.fallback_cwd
    )
}

/// 把 issue 格式化成交互式选择提示（询问用户是否改用当前 cwd）。
///
/// 只拼接文本，不读取输入；选择逻辑在 `kiana-interactive`。
/// TS: `session-cwd.ts#formatMissingSessionCwdPrompt`
pub fn format_missing_session_cwd_prompt(issue: &SessionCwdIssue) -> String {
    format!(
        "cwd from session file does not exist\n{}\n\ncontinue in current cwd\n{}",
        issue.session_cwd, issue.fallback_cwd
    )
}

/// `MissingSessionCwdError`（`name` 为 `"MissingSessionCwdError"`）。
///
/// `Display` 直接复用 [`format_missing_session_cwd_error`]；`Error` 实现没有 `source`。
/// TS: `session-cwd.ts#MissingSessionCwdError`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingSessionCwdError {
    /// 诊断信息。
    pub issue: SessionCwdIssue,
}

impl std::fmt::Display for MissingSessionCwdError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&format_missing_session_cwd_error(&self.issue))
    }
}

impl std::error::Error for MissingSessionCwdError {}

impl MissingSessionCwdError {
    /// `error.name` 的字面量（TS 构造函数里写 `this.name = "MissingSessionCwdError"`）。
    /// TS: session-cwd.ts#name
    pub fn name(&self) -> &'static str {
        "MissingSessionCwdError"
    }
}

/// `assertSessionCwdExists(sessionManager, fallbackCwd)`：cwd 缺失时返回错误。
///
/// 判定完全委托给 [`get_missing_session_cwd_issue`]；成功时返回 `Ok(())`，不返回诊断信息。
/// TS: `session-cwd.ts#assertSessionCwdExists`
pub fn assert_session_cwd_exists(
    session_manager: &(impl SessionCwdSource + ?Sized),
    fallback_cwd: &str,
) -> Result<(), MissingSessionCwdError> {
    match get_missing_session_cwd_issue(session_manager, fallback_cwd) {
        Some(issue) => Err(MissingSessionCwdError { issue }),
        None => Ok(()),
    }
}
