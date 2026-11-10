//! IDE 启动接线：发现 lockfile → 建立 MCP Streamable HTTP 连接 → initialize。
//!
//! [`crate::ide::lockfile`] 只做发现/清理，[`crate::ide::protocol`] 只做协议；
//! 本模块把两者接成应用启动时可以调用的一次动作：
//!
//! ```no_run
//! # async fn run(cwd: &std::path::Path) -> Result<(), kiana_coding_agent::ide::IdeError> {
//! # let context = kiana_coding_agent::ide::IdeContext::default();
//! if let Some(connection) = kiana_coding_agent::ide::connect_for_workspace(cwd).await? {
//!     connection.client.send_context_update(&context).await?;
//! }
//! # Ok(())
//! # }
//! ```
//!
//! 失败语义：没有 IDE 是 `Ok(None)`；找到但连不上/initialize 失败是 `Err`，调用方
//! 应记录后继续（IDE 是增强能力，不是会话启动依赖）。`transport: "ws"` 目前未实现，
//! 明确返回 `Err` 而不是静默连 HTTP。

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use serde_json::Value;

use crate::ide::lockfile::{DiscoveredIde, cleanup_stale_locks, discover_ides};
use crate::ide::protocol::{
    DiffBroker, DiffOutcome, DiffRequest, HttpIdeTransport, IdeError, McpIdeClient,
};
use crate::web::ReqwestWebTransport;

/// 进程级 diff broker：`openDiff` 的等待端与 SSE 通知接收端共用同一份登记表。
static DIFF_BROKER: OnceLock<DiffBroker> = OnceLock::new();

/// 启动时发现并初始化的 IDE 连接（没有 IDE 时保持 `None`）。
static CONNECTION: OnceLock<Option<IdeConnection>> = OnceLock::new();

/// 进程级 diff broker。
pub fn global_diff_broker() -> &'static DiffBroker {
    DIFF_BROKER.get_or_init(DiffBroker::new)
}

/// 记录启动时建立的连接（只写一次；重复调用忽略）。
pub fn install_connection(connection: Option<IdeConnection>) {
    let _ = CONNECTION.set(connection);
}

/// 读取启动时建立的连接。
pub fn global_connection() -> Option<&'static IdeConnection> {
    CONNECTION.get().and_then(Option::as_ref)
}

/// 把 MCP 服务器通知路由到 [`global_diff_broker`]；返回是否有等待者被结算。
pub fn route_ide_notification(method: &str, params: &Value) -> bool {
    match method {
        "ide/diffAccepted" => {
            let Some(id) = params.get("id").and_then(Value::as_str) else {
                return false;
            };
            let content = params
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or_default();
            global_diff_broker().resolve_accepted(id, content)
        }
        "ide/diffRejected" => {
            let Some(id) = params.get("id").and_then(Value::as_str) else {
                return false;
            };
            global_diff_broker().resolve_rejected(id)
        }
        _ => false,
    }
}

/// `~/.kiana/ide`（`KIANA_IDE_DIR` 可覆盖）。
pub fn agent_ide_dir() -> PathBuf {
    if let Ok(directory) = std::env::var("KIANA_IDE_DIR")
        && !directory.is_empty()
    {
        return PathBuf::from(directory);
    }
    kiana_core::config::get_agent_dir()
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("ide")
}

/// 一个已初始化的 IDE 连接。
pub struct IdeConnection {
    /// 发现结果（lockfile 路径、端口、workspace）。
    pub ide: DiscoveredIde,
    /// 已 initialize 的 MCP 客户端。
    pub client: McpIdeClient,
    /// HTTP 传输（`openDiff` / `contextUpdate` 共用）。
    pub transport: Arc<HttpIdeTransport>,
}

impl IdeConnection {
    /// 在 IDE 里打开 diff 并等待用户接受/拒绝。
    ///
    /// 通知由 [`route_ide_notification`]（挂在传输的 SSE 接收端）结算；`read_only`
    /// 请求立即返回 [`IdeError::ReadOnlyDiff`]，不会发送任何内容。
    pub async fn open_diff(&self, request: DiffRequest) -> Result<DiffOutcome, IdeError> {
        global_diff_broker()
            .open_diff(self.transport.as_ref(), request)
            .await
    }
}

/// 使用默认 agent 目录发现并连接 IDE。
pub async fn connect_for_workspace(
    cwd: impl AsRef<Path>,
) -> Result<Option<IdeConnection>, IdeError> {
    connect_in(&agent_ide_dir(), cwd.as_ref()).await
}

/// 指定 lockfile 目录（测试/嵌入方）。
pub async fn connect_in(ide_dir: &Path, cwd: &Path) -> Result<Option<IdeConnection>, IdeError> {
    cleanup_stale_locks(ide_dir, cwd);
    let Some(ide) = discover_ides(ide_dir, cwd).into_iter().next() else {
        return Ok(None);
    };
    if ide
        .lock
        .transport
        .as_deref()
        .is_some_and(|transport| transport.eq_ignore_ascii_case("ws"))
    {
        return Err(IdeError::Transport(
            "IDE lockfile requests WebSocket transport, which is not implemented; use HTTP transport or no IDE"
                .to_string(),
        ));
    }
    let transport = Arc::new(
        HttpIdeTransport::new(
            Arc::new(ReqwestWebTransport::new()),
            ide.port,
            ide.lock.auth_token.clone(),
        )
        // 同一连接上收到的 `ide/diffAccepted` / `ide/diffRejected` 直接结算 broker。
        .with_notification_sink(Arc::new(|method, params| {
            let _ = route_ide_notification(method, params);
        })),
    );
    let client = McpIdeClient::new(transport.clone());
    client.initialize().await?;
    Ok(Some(IdeConnection {
        ide,
        client,
        transport,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ide::lockfile::IdeLockfile;

    #[tokio::test]
    async fn ide_diff_notifications_resolve_the_global_broker() {
        let accepted_id = "test-diff-accepted";
        let receiver = global_diff_broker().register(accepted_id);
        assert!(route_ide_notification(
            "ide/diffAccepted",
            &serde_json::json!({"id": accepted_id, "content": "final content"})
        ));
        assert_eq!(
            receiver.await.expect("outcome"),
            DiffOutcome::Accepted {
                content: "final content".to_string()
            }
        );

        let rejected_id = "test-diff-rejected";
        let receiver = global_diff_broker().register(rejected_id);
        assert!(route_ide_notification(
            "ide/diffRejected",
            &serde_json::json!({"id": rejected_id})
        ));
        assert_eq!(receiver.await.expect("outcome"), DiffOutcome::Rejected);

        // 未知 id / 无关通知不 panic、不消费任何等待者。
        assert!(!route_ide_notification(
            "ide/diffRejected",
            &serde_json::json!({"id": "no-such-diff"})
        ));
        assert!(!route_ide_notification(
            "other/notification",
            &serde_json::json!({})
        ));
    }

    #[tokio::test]
    async fn empty_directory_means_no_ide() {
        let temp = tempfile::tempdir().expect("tempdir");
        let connection = connect_in(temp.path(), Path::new("/repo"))
            .await
            .expect("no error");
        assert!(connection.is_none());
    }

    #[tokio::test]
    async fn dead_pid_lockfile_is_cleaned_instead_of_connected() {
        let temp = tempfile::tempdir().expect("tempdir");
        let lock = IdeLockfile {
            workspace_path: Some("/repo".to_string()),
            port: Some(9),
            pid: Some(u32::MAX),
            ppid: None,
            transport: Some("http".to_string()),
            ..Default::default()
        };
        std::fs::write(
            temp.path().join("9.lock"),
            serde_json::to_vec(&lock).expect("json"),
        )
        .expect("write lock");
        let connection = connect_in(temp.path(), Path::new("/repo"))
            .await
            .expect("no error");
        assert!(connection.is_none());
        assert!(!temp.path().join("9.lock").exists());
    }
}
