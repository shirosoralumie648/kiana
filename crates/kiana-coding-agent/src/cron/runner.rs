//! cron scheduler 与 `AgentSession` 的接线层。
//!
//! [`CronScheduler`] 只负责“何时该派发”，不持有会话；本模块把
//! [`DispatchRequest`] 变成一次可执行的 turn 请求：
//!
//! 1. `SessionJobDispatcher` 校验 job，解析受管脚本并在 `job.workdir` 下执行；
//! 2. 通过 [`CronTurnRunner`] 交给宿主（通常是 LocalSet 上持有 `AgentSession` 的
//!    任务）执行 prompt；
//! 3. [`run_cron_loop`] 提供“启动先 tick 一次、之后每 30s tick”的循环。
//!
//! # 为什么用 trait/通道而不是直接持有 `AgentSession`
//!
//! `AgentSession` 是 `!Send`（扩展运行时基于线程本地的 rquickjs），而
//! [`JobDispatcher`] 要求 `Send + Sync`（scheduler 的 tick future 会跨进 tokio
//! 任务）。因此生产用法是：
//!
//! ```text
//! let (runner, mut receiver) = cron::runner::cron_turn_channel();
//! // LocalSet 上的宿主任务：
//! tokio::task::spawn_local(async move {
//!     while let Some(request) = receiver.recv().await {
//!         let result = run_cron_turn(&session, request).await;
//!         // request.reply 由 ChannelTurnRunner 内部持有；见 cron_turn_channel。
//!     }
//! });
//! let dispatcher = SessionJobDispatcher::new(Arc::new(runner), Some(script_dir));
//! ```
//!
//! `run_cron_turn` 的宿主实现需要应用 `skills`/`model`/`context_from`（这些不是
//! [`PromptOptions`](kiana_session::agent_session::PromptOptions) 的字段）：
//! 先切换模型、激活技能/加载上下文摘要，再 `prompt(request.prompt, ...)`。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::{mpsc, oneshot};

use super::scheduler::{CronConfig, CronScheduler, DispatchRequest, JobDispatcher, TickReport};
use super::store::{CronError, CronJob, CronStore, resolve_script_path};
use super::time::now_secs;

/// 默认 tick 间隔（秒）。
pub const DEFAULT_TICK_INTERVAL_SECS: u64 = 30;
/// 脚本输出的默认字节上限。
pub const DEFAULT_MAX_SCRIPT_OUTPUT_BYTES: usize = 64 * 1024;
/// 脚本默认超时（秒）。
pub const DEFAULT_SCRIPT_TIMEOUT_SECS: u64 = 300;

/// 一次 cron turn 请求（`AgentSession` 宿主需要的全部字段）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CronTurnRequest {
    /// job id。
    pub job_id: String,
    /// 要提交给模型的 prompt。
    pub prompt: String,
    /// 需要预加载/激活的技能名。
    pub skills: Vec<String>,
    /// 目标模型（`provider/model` 或模型 id；`None` 表示会话当前模型）。
    pub model: Option<String>,
    /// 需要作为上下文加载的会话 id 列表。
    pub context_from: Vec<String>,
    /// 执行目录（job.workdir，缺省为会话 cwd）。
    pub workdir: PathBuf,
    /// 受管脚本输出（已截断）；`None` 表示没有脚本或脚本无误。
    pub script_output: Option<String>,
    /// execution id（用于 delivery/日志关联）。
    pub execution_id: String,
    /// 计划槽位（Unix 秒）。
    pub slot: i64,
}

/// 宿主执行一次 cron turn 的契约。
// `async_trait` 展开的 Future 方法自带 `#[must_use]`，clippy 看不到宏内属性。
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait CronTurnRunner: Send + Sync {
    /// 执行；`Err` 会写进 execution 并标记 `failed`。
    async fn run_turn(&self, request: CronTurnRequest) -> Result<(), String>;
}

/// 通过通道把请求交给 LocalSet 宿主的 runner。
///
/// 生产用法见模块文档：宿主持有 `mpsc::Receiver<CronTurnRequest>`，执行后通过
/// [`CronTurnRequest`] 之外的 oneshot 回复。为保持 `CronTurnRequest` 可序列化/简单，
/// 回复通道单独放在 [`CronTurnEnvelope`] 里。
pub struct ChannelTurnRunner {
    sender: mpsc::Sender<CronTurnEnvelope>,
}

/// 通道里的一条请求（携带回复）。
pub struct CronTurnEnvelope {
    /// 请求。
    pub request: CronTurnRequest,
    /// 执行结果回执。
    pub reply: oneshot::Sender<Result<(), String>>,
}

impl ChannelTurnRunner {
    /// 新建发送端。
    pub fn new(sender: mpsc::Sender<CronTurnEnvelope>) -> Self {
        Self { sender }
    }
}

/// 创建 runner/worker 通道。
pub fn cron_turn_channel() -> (Arc<ChannelTurnRunner>, mpsc::Receiver<CronTurnEnvelope>) {
    let (sender, receiver) = mpsc::channel(8);
    (Arc::new(ChannelTurnRunner::new(sender)), receiver)
}

/// LocalSet 上的生产 worker：串行执行 cron turn。
///
/// `AgentSession` 是 `!Send`，因此只能在持有会话的 `LocalSet` 上运行；`ChannelTurnRunner`
/// 的 async trait 把请求跨进 tokio 任务，worker 再把结果用 oneshot 回执。每个请求依次：
///
/// 1. 会话正在流式时拒绝（不打断用户 turn）；
/// 2. 按 `request.model`（`provider/model` 或模型 id）切换模型；
/// 3. 激活 `request.skills`；
/// 4. 把 `request.context_from` 指向的会话文件内容作为 `cron.context-from` 自定义消息注入；
/// 5. 把受管脚本输出附到 prompt，并发出一次 `prompt`。
pub async fn run_cron_worker(
    session: kiana_session::agent_session::AgentSession,
    receiver: mpsc::Receiver<CronTurnEnvelope>,
) {
    run_cron_worker_with(session, receiver).await
}

/// 与 [`run_cron_worker`] 相同，但对任意 [`CronTurnHost`] 工作（测试替身）。
pub async fn run_cron_worker_with<H: CronTurnHost>(
    host: H,
    mut receiver: mpsc::Receiver<CronTurnEnvelope>,
) {
    while let Some(envelope) = receiver.recv().await {
        let result = host.execute_cron_turn(&envelope.request).await;
        let _ = envelope.reply.send(result);
    }
}

/// LocalSet 宿主执行一次 cron turn 的契约。
///
/// `AgentSession` 是 `!Send`（扩展运行时是线程本地的），因此这里用
/// `async_trait(?Send)`；调用方必须在 LocalSet 上 `spawn_local`。
// `async_trait` 展开的 Future 方法自带 `#[must_use]`，clippy 看不到宏内属性。
#[allow(clippy::double_must_use)]
#[async_trait(?Send)]
pub trait CronTurnHost {
    /// 执行；`Err` 会写进 execution 并标记 `failed`。
    async fn execute_cron_turn(&self, request: &CronTurnRequest) -> Result<(), String>;
}

#[async_trait(?Send)]
impl CronTurnHost for kiana_session::agent_session::AgentSession {
    async fn execute_cron_turn(&self, request: &CronTurnRequest) -> Result<(), String> {
        execute_cron_turn(self, request).await
    }
}

/// 执行一次 cron turn（worker 的主体，测试可直接调用）。
pub async fn execute_cron_turn(
    session: &kiana_session::agent_session::AgentSession,
    request: &CronTurnRequest,
) -> Result<(), String> {
    if session.is_streaming() {
        return Err("session is busy with another run; cron turn deferred".to_string());
    }
    // 校验 context_from 等所有可能提前返回的输入，再修改共享会话状态。
    for source in &request.context_from {
        let path = Path::new(source);
        if !path.is_file() {
            return Err(format!(
                "cron context_from {source:?} is not a readable session file"
            ));
        }
        let entries =
            kiana_session_core::session_manager::load_entries_from_file(&path.to_string_lossy())
                .map_err(|error| format!("cannot read cron context {source:?}: {error}"))?;
        let context = render_context_entries(&entries, 64 * 1024);
        if context.is_empty() {
            continue;
        }
        session
            .send_custom_message(
                kiana_session_core::messages::CustomMessage {
                    role: kiana_session_core::messages::CustomRole::Custom,
                    custom_type: "cron.context-from".to_string(),
                    content: serde_json::Value::String(context),
                    display: false,
                    details: Some(serde_json::json!({ "source": source })),
                    timestamp: now_millis() as f64,
                },
                Some(false),
                None,
            )
            .await
            .map_err(|error| format!("cannot inject cron context from {source:?}: {error}"))?;
    }
    let mut prompt = request.prompt.clone();
    if !request.workdir.as_os_str().is_empty() {
        prompt.push_str(&format!(
            "\n\n<cron-workdir>\n{}\n</cron-workdir>",
            request.workdir.display()
        ));
    }
    if let Some(output) = &request.script_output
        && !output.trim().is_empty()
    {
        prompt.push_str("\n\n<managed-script-output>\n");
        prompt.push_str(output);
        prompt.push_str("\n</managed-script-output>");
    }

    // cron 与交互式回合共享同一个 `AgentSession`（`session.clone()` 是 Arc 句柄）。
    // 模型/技能覆盖必须在本回合结束后恢复，否则会泄漏到用户的下一次输入。
    let previous_model = session.model();
    let previous_thinking = session.thinking_level();
    let previous_skills: std::collections::HashSet<String> = session
        .active_skill_activations()
        .into_iter()
        .map(|activation| activation.skill_name)
        .collect();
    let mut activated: Vec<String> = Vec::new();
    let mut model_changed = false;
    let outcome = async {
        if let Some(name) = &request.model {
            let model = resolve_cron_model(session, name)?;
            let same = previous_model.as_ref().is_some_and(|previous| {
                previous.provider == model.provider && previous.id == model.id
            });
            if !same {
                session
                    .set_model(
                        model,
                        kiana_session::agent_session::ModelMutationOptions::default(),
                    )
                    .await
                    .map_err(|error| format!("cannot switch cron model: {error}"))?;
                model_changed = true;
            }
        }
        for skill in &request.skills {
            session
                .activate_skill(skill)
                .map_err(|error| format!("cannot activate cron skill {skill:?}: {error}"))?;
            if !previous_skills.contains(skill) {
                activated.push(skill.clone());
            }
        }
        session
            .prompt(
                prompt,
                kiana_session::agent_session::PromptOptions {
                    source: Some(kiana_ext_api::events::InputSource::Extension),
                    ..Default::default()
                },
            )
            .await
            .map_err(|error| format!("cron turn failed: {error}"))
    }
    .await;

    for skill in activated {
        session.deactivate_skill(&skill);
    }
    if model_changed && let Some(previous) = previous_model {
        let _ = session
            .set_model(
                previous,
                kiana_session::agent_session::ModelMutationOptions::default(),
            )
            .await;
        // `set_model` 会按模型重新推导 thinking；恢复会话原来的显式级别。
        session.set_thinking_level(
            previous_thinking,
            kiana_session::agent_session::ModelMutationOptions::default(),
        );
    }
    outcome
}

fn resolve_cron_model(
    session: &kiana_session::agent_session::AgentSession,
    name: &str,
) -> Result<kiana_ai::types::Model, String> {
    let models = session.model_runtime().get_available_snapshot();
    models
        .into_iter()
        .find(|model| model.id == name || format!("{}/{}", model.provider, model.id) == name)
        .ok_or_else(|| {
            format!("unknown cron model {name:?}; available models: {}", {
                session
                    .model_runtime()
                    .get_available_snapshot()
                    .iter()
                    .map(|model| format!("{}/{}", model.provider, model.id))
                    .collect::<Vec<_>>()
                    .join(", ")
            })
        })
}

fn render_context_entries(
    entries: &[kiana_session_core::session_manager::FileEntry],
    max_bytes: usize,
) -> String {
    let mut rendered = String::new();
    // 最近 50 条，保持时间顺序；超过上限时保留最新尾部。
    for entry in entries.iter().rev().take(50).rev() {
        let Ok(json) = serde_json::to_string(entry) else {
            continue;
        };
        rendered.push_str(&json);
        rendered.push('\n');
        if rendered.len() > max_bytes {
            let mut cut = max_bytes;
            while cut > 0 && !rendered.is_char_boundary(cut) {
                cut -= 1;
            }
            rendered.truncate(cut);
            rendered.push_str("\n[context truncated]\n");
            break;
        }
    }
    rendered
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[async_trait]
impl CronTurnRunner for ChannelTurnRunner {
    async fn run_turn(&self, request: CronTurnRequest) -> Result<(), String> {
        let (reply, receiver) = oneshot::channel();
        self.sender
            .send(CronTurnEnvelope { request, reply })
            .await
            .map_err(|_| "cron turn worker is not running".to_string())?;
        receiver
            .await
            .map_err(|_| "cron turn worker dropped the reply".to_string())?
    }
}

/// 生产 dispatcher：校验 job、执行受管脚本，然后交给 [`CronTurnRunner`]。
pub struct SessionJobDispatcher {
    runner: Arc<dyn CronTurnRunner>,
    managed_script_dir: Option<PathBuf>,
    script_timeout_secs: u64,
    max_script_output_bytes: usize,
}

impl std::fmt::Debug for SessionJobDispatcher {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionJobDispatcher")
            .field("managed_script_dir", &self.managed_script_dir)
            .field("script_timeout_secs", &self.script_timeout_secs)
            .finish_non_exhaustive()
    }
}

impl SessionJobDispatcher {
    /// 新建。`managed_script_dir` 必须与 `CronConfig::managed_script_dir` 一致。
    pub fn new(runner: Arc<dyn CronTurnRunner>, managed_script_dir: Option<PathBuf>) -> Self {
        Self {
            runner,
            managed_script_dir,
            script_timeout_secs: DEFAULT_SCRIPT_TIMEOUT_SECS,
            max_script_output_bytes: DEFAULT_MAX_SCRIPT_OUTPUT_BYTES,
        }
    }

    /// 覆盖脚本超时。
    pub fn with_script_timeout_secs(mut self, seconds: u64) -> Self {
        self.script_timeout_secs = seconds.max(1);
        self
    }

    /// 覆盖脚本输出上限。
    pub fn with_max_script_output_bytes(mut self, bytes: usize) -> Self {
        self.max_script_output_bytes = bytes.max(1);
        self
    }

    async fn run_script(&self, job: &CronJob, workdir: &Path) -> Result<Option<String>, String> {
        let Some(script) = job.script.as_deref() else {
            return Ok(None);
        };
        let Some(directory) = self.managed_script_dir.as_deref() else {
            return Err("script is set but no managed script directory is configured".to_string());
        };
        let path = resolve_script_path(directory, script).map_err(|error| error.to_string())?;
        let output = run_script_with_cap(
            &path,
            workdir,
            self.script_timeout_secs,
            self.max_script_output_bytes,
        )
        .await?;
        if output.status_success {
            Ok(Some(output.text))
        } else {
            Err(format!(
                "managed script {} failed with {}: {}",
                path.display(),
                output
                    .exit_code
                    .map(|code| code.to_string())
                    .unwrap_or_else(|| "signal".to_string()),
                output.text
            ))
        }
    }
}

#[async_trait]
impl JobDispatcher for SessionJobDispatcher {
    async fn dispatch(&self, request: DispatchRequest) -> Result<(), String> {
        let job = &request.job;
        job.validate(self.managed_script_dir.as_deref())
            .map_err(|error| error.to_string())?;
        let workdir = match &job.workdir {
            Some(workdir) => PathBuf::from(workdir),
            None => std::env::current_dir()
                .map_err(|error| format!("cannot resolve current directory: {error}"))?,
        };
        if !workdir.is_dir() {
            return Err(format!(
                "cron workdir {} does not exist or is not a directory",
                workdir.display()
            ));
        }
        let script_output = self.run_script(job, &workdir).await?;
        self.runner
            .run_turn(CronTurnRequest {
                job_id: job.id.clone(),
                prompt: job.prompt.clone(),
                skills: job.skills.clone(),
                model: job.model.clone(),
                context_from: job.context_from.clone(),
                workdir,
                script_output,
                execution_id: request.execution_id,
                slot: request.slot,
            })
            .await
    }
}

#[derive(Debug)]
struct ScriptOutput {
    status_success: bool,
    exit_code: Option<i32>,
    text: String,
}

async fn drain_capped<R>(reader: &mut Option<R>, max_bytes: usize) -> Vec<u8>
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;
    let mut kept = Vec::new();
    let mut chunk = [0u8; 8192];
    if let Some(reader) = reader {
        loop {
            match reader.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    if kept.len() < max_bytes {
                        let take = (max_bytes - kept.len()).min(read);
                        kept.extend_from_slice(&chunk[..take]);
                    }
                }
            }
        }
    }
    kept
}

/// 在 `workdir` 下执行受管脚本。
///
/// 沙箱语义与 `bash` 工具一致：`KIANA_SANDBOX_POLICY` 配置了策略时，脚本通过
/// [`kiana_sandbox::SandboxManager`] 启动（bubblewrap / Landlock，失败即不执行）；
/// 未配置沙箱时退化为宿主机 `sh`。cron 是无人值守执行，不存在交互式审批通道，
/// 因此这里不经过权限引擎的 Ask 流程——脚本本身必须位于 0700 的受管目录
/// （`resolve_script_path` 已拒绝绝对路径、`..` 与符号链接逃逸）。
async fn run_script_with_cap(
    script: &Path,
    workdir: &Path,
    timeout_secs: u64,
    max_bytes: usize,
) -> Result<ScriptOutput, String> {
    use tokio::time::timeout;

    let mut child = spawn_managed_script(script, workdir)
        .map_err(|error| format!("cannot start managed script: {error}"))?;

    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    // 持续排空管道（超过上限后只丢弃，不再写入 buffer），否则脚本写满 pipe 会阻塞在
    // 写端。**read 与 wait 必须在同一个 timeout 内**：脚本关闭 stdout/stderr 后继续运行
    // 时排空会立即 EOF，若 wait 在超时之外，一个静默长跑脚本会永久挂住 tick 循环。
    let wait_and_read = async {
        let (out, err) = tokio::join!(
            drain_capped(&mut stdout, max_bytes),
            drain_capped(&mut stderr, max_bytes),
        );
        let status = child.wait().await;
        (out, err, status)
    };
    let (out, err, status) = match timeout(Duration::from_secs(timeout_secs), wait_and_read).await {
        Ok((out, err, status)) => (out, err, status),
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err(format!(
                "managed script {} timed out after {timeout_secs}s",
                script.display()
            ));
        }
    };
    let status = status.map_err(|error| format!("cannot wait for managed script: {error}"))?;
    let mut text = String::from_utf8_lossy(&out).into_owned();
    let stderr_text = String::from_utf8_lossy(&err);
    if !stderr_text.trim().is_empty() {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&stderr_text);
    }
    if text.len() > max_bytes {
        let mut cut = max_bytes;
        while cut > 0 && !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
        text.push_str("\n[script output truncated]");
    }
    Ok(ScriptOutput {
        status_success: status.success(),
        exit_code: status.code(),
        text,
    })
}

/// 按 `KIANA_SANDBOX_POLICY` 启动受管脚本。
///
/// 与 `bash` 工具共用 [`kiana_sandbox::SandboxManager::from_env`]：策略要求沙箱时走
/// bubblewrap/Landlock，后端不可用直接报错（脚本不执行，fail-closed）；未配置策略时
/// 才是宿主 `sh`。
fn spawn_managed_script(script: &Path, workdir: &Path) -> Result<tokio::process::Child, String> {
    use kiana_sandbox::{SpawnOptions, StdioMode};
    use std::process::Stdio;

    let script_arg = script.to_string_lossy().into_owned();
    let options = SpawnOptions {
        cwd: Some(workdir.to_path_buf()),
        stdin: StdioMode::Ignore,
        stdout: StdioMode::Pipe,
        stderr: StdioMode::Pipe,
        ..Default::default()
    };
    if let Some(manager) = kiana_sandbox::SandboxManager::from_env(workdir) {
        return manager
            .spawn("sh", &[script_arg], &options)
            .map_err(|error| error.to_string());
    }
    tokio::process::Command::new("sh")
        .arg(script)
        .current_dir(workdir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| error.to_string())
}

/// 启动 cron 调度循环：立即 tick 一次，之后每 `interval` tick 一次，直到
/// `shutdown` 变为 `true` 或发送端被丢弃。
///
/// `on_report` 在每次 tick 后收到 `TickReport`/错误；调用方负责写日志（本 crate 的
/// stdout 纪律要求用户可见输出走 `Output`）。
pub async fn run_cron_loop<S, F>(
    scheduler: Arc<CronScheduler<S>>,
    interval: Duration,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
    mut on_report: F,
) where
    S: CronStore + 'static,
    F: FnMut(Result<TickReport, CronError>),
{
    let interval = interval.max(Duration::from_millis(1));
    loop {
        // `changed()` 只在“之后再次变化”时返回；启动时已经是 `true` 的 receiver 不能靠
        // 它退出（否则循环会一直 tick）。
        if *shutdown.borrow() {
            break;
        }
        let report = scheduler.tick(now_secs()).await;
        on_report(report);
        tokio::select! {
            _ = tokio::time::sleep(interval) => {}
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
            }
        }
    }
}

/// 构造生产 scheduler（`FileCronStore` + `SessionJobDispatcher`）。
pub fn build_cron_scheduler(
    cron_root: impl AsRef<Path>,
    dispatcher: Arc<dyn JobDispatcher>,
) -> Result<CronScheduler<super::store::FileCronStore>, CronError> {
    let root = cron_root.as_ref();
    let scripts = root.join("scripts");
    std::fs::create_dir_all(&scripts).map_err(|source| super::store::CronStoreError::Io {
        path: scripts.clone(),
        source,
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&scripts, std::fs::Permissions::from_mode(0o700));
    }
    let store = super::store::FileCronStore::new(root)?;
    Ok(CronScheduler::new(
        Arc::new(store),
        super::store::ProcessFingerprint::current(),
        CronConfig {
            managed_script_dir: Some(scripts),
            ..Default::default()
        },
        dispatcher,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cron::ScheduleSpec;
    use crate::cron::store::{CronStore, ExecutionStatus, FileCronStore};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Default)]
    struct RecordingRunner {
        requests: Mutex<Vec<CronTurnRequest>>,
    }

    #[async_trait]
    impl CronTurnRunner for RecordingRunner {
        async fn run_turn(&self, request: CronTurnRequest) -> Result<(), String> {
            self.requests.lock().expect("requests").push(request);
            Ok(())
        }
    }

    fn job_due_now(store: &FileCronStore, now: i64) -> CronJob {
        let mut job = CronJob::new("daily", "review open PRs", ScheduleSpec::every("60s"));
        job.grace_minutes = 5;
        job.skills = vec!["review".to_string()];
        job.model = Some("fake/model".to_string());
        job.context_from = vec!["session-1".to_string()];
        job.workdir = Some(std::env::temp_dir().to_string_lossy().into_owned());
        job.initialize_schedule(now).expect("initialize");
        job.next_run_at = Some(now);
        store.save_job(&job).expect("save job");
        job
    }

    #[tokio::test]
    async fn dispatcher_executes_managed_script_and_forwards_turn_fields() {
        let temp = tempfile::tempdir().expect("tempdir");
        let scripts = temp.path().join("scripts");
        std::fs::create_dir_all(&scripts).expect("scripts dir");
        let script = scripts.join("run.sh");
        std::fs::write(&script, "echo script-ok\n").expect("script");
        let store = FileCronStore::new(temp.path()).expect("store");
        let now = now_secs();
        let mut job = job_due_now(&store, now);
        job.script = Some("run.sh".to_string());
        job.schedule = ScheduleSpec::every("60s");
        store.save_job(&job).expect("save job");

        let runner = Arc::new(RecordingRunner::default());
        let dispatcher = SessionJobDispatcher::new(runner.clone(), Some(scripts));
        let scheduler = CronScheduler::new(
            Arc::new(FileCronStore::new(temp.path()).expect("store")),
            crate::cron::store::ProcessFingerprint::current(),
            CronConfig {
                managed_script_dir: Some(temp.path().join("scripts")),
                ..Default::default()
            },
            Arc::new(dispatcher),
        );
        let report = scheduler.tick(now + 1).await.expect("tick");
        assert_eq!(report.dispatched, vec!["daily".to_string()]);
        let requests = runner.requests.lock().expect("requests").clone();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].prompt, "review open PRs");
        assert_eq!(requests[0].skills, vec!["review".to_string()]);
        assert_eq!(requests[0].model.as_deref(), Some("fake/model"));
        assert_eq!(requests[0].context_from, vec!["session-1".to_string()]);
        assert!(
            requests[0]
                .script_output
                .as_deref()
                .is_some_and(|output| output.contains("script-ok")),
            "{:?}",
            requests[0].script_output
        );
        let executions = store.list_executions().expect("executions");
        assert!(executions.iter().any(|execution| {
            execution.job_id == "daily" && execution.status == ExecutionStatus::Completed
        }));
    }

    #[tokio::test]
    async fn failed_script_marks_execution_failed_and_does_not_run_a_turn() {
        let temp = tempfile::tempdir().expect("tempdir");
        let scripts = temp.path().join("scripts");
        std::fs::create_dir_all(&scripts).expect("scripts dir");
        std::fs::write(scripts.join("bad.sh"), "echo boom >&2\nexit 7\n").expect("script");
        let store = FileCronStore::new(temp.path()).expect("store");
        let now = now_secs();
        let mut job = job_due_now(&store, now);
        job.script = Some("bad.sh".to_string());
        store.save_job(&job).expect("save job");

        let runner = Arc::new(RecordingRunner::default());
        let dispatcher = SessionJobDispatcher::new(runner.clone(), Some(scripts));
        let scheduler = CronScheduler::new(
            Arc::new(FileCronStore::new(temp.path()).expect("store")),
            crate::cron::store::ProcessFingerprint::current(),
            CronConfig {
                managed_script_dir: Some(temp.path().join("scripts")),
                ..Default::default()
            },
            Arc::new(dispatcher),
        );
        let report = scheduler.tick(now + 1).await.expect("tick");
        assert_eq!(report.failed, vec!["daily".to_string()]);
        assert!(runner.requests.lock().expect("requests").is_empty());
        let executions = store.list_executions().expect("executions");
        assert!(executions.iter().any(|execution| {
            execution.job_id == "daily" && execution.status == ExecutionStatus::Failed
        }));
    }

    #[tokio::test]
    async fn cron_loop_ticks_immediately_and_stops_on_shutdown() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = Arc::new(FileCronStore::new(temp.path()).expect("store"));
        let runner = Arc::new(RecordingRunner::default());
        let dispatcher = SessionJobDispatcher::new(runner, Some(temp.path().join("scripts")));
        let scheduler = Arc::new(CronScheduler::new(
            store,
            crate::cron::store::ProcessFingerprint::current(),
            CronConfig::default(),
            Arc::new(dispatcher),
        ));
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let ticks = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&ticks);
        let loop_future = run_cron_loop(
            scheduler,
            Duration::from_millis(5),
            shutdown_rx,
            move |_report| {
                counter.fetch_add(1, Ordering::SeqCst);
            },
        );
        // 立即 tick 一次后由 shutdown 结束。
        let handle = tokio::spawn(loop_future);
        tokio::time::sleep(Duration::from_millis(5)).await;
        shutdown_tx.send(true).expect("shutdown");
        handle.await.expect("loop");
        assert!(ticks.load(Ordering::SeqCst) >= 1);
    }

    #[tokio::test]
    async fn cron_loop_exits_when_shutdown_is_already_set() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = Arc::new(FileCronStore::new(temp.path()).expect("store"));
        let runner = Arc::new(RecordingRunner::default());
        let dispatcher = SessionJobDispatcher::new(runner, Some(temp.path().join("scripts")));
        let scheduler = Arc::new(CronScheduler::new(
            store,
            crate::cron::store::ProcessFingerprint::current(),
            CronConfig::default(),
            Arc::new(dispatcher),
        ));
        let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(true);
        let ticks = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&ticks);
        tokio::time::timeout(
            Duration::from_secs(1),
            run_cron_loop(
                scheduler,
                Duration::from_millis(5),
                shutdown_rx,
                move |_report| {
                    counter.fetch_add(1, Ordering::SeqCst);
                },
            ),
        )
        .await
        .expect("loop must exit when shutdown is already set");
        assert_eq!(ticks.load(Ordering::SeqCst), 0);
    }

    #[derive(Default, Clone)]
    struct RecordingHost {
        requests: Arc<Mutex<Vec<CronTurnRequest>>>,
    }

    #[async_trait(?Send)]
    impl CronTurnHost for RecordingHost {
        async fn execute_cron_turn(&self, request: &CronTurnRequest) -> Result<(), String> {
            self.requests
                .lock()
                .expect("requests")
                .push(request.clone());
            Ok(())
        }
    }

    /// worker 真正消费通道并回执：宿主拿到请求，`ChannelTurnRunner` 得到 Ok。
    #[tokio::test]
    async fn cron_worker_consumes_channel_requests_and_replies() {
        let (runner, receiver) = cron_turn_channel();
        let host = RecordingHost::default();
        let worker = run_cron_worker_with(host.clone(), receiver);
        let client = async move {
            let request = CronTurnRequest {
                job_id: "daily".to_string(),
                prompt: "do it".to_string(),
                skills: Vec::new(),
                model: None,
                context_from: Vec::new(),
                workdir: std::env::temp_dir(),
                script_output: None,
                execution_id: "exec-1".to_string(),
                slot: 1,
            };
            runner.run_turn(request).await.expect("worker replies");
            // worker 在 sender 全部 drop 后退出。
            drop(runner);
        };
        tokio::join!(worker, client);
        assert_eq!(host.requests.lock().expect("requests").len(), 1);
    }

    /// 脚本关闭 stdout/stderr 后静默长跑：read 会立即 EOF，wait 必须在同一个 timeout
    /// 内被杀死，否则 tick 循环永久挂起，`timeout_secs` 形同虚设。
    #[tokio::test]
    async fn silent_long_running_script_is_killed_by_the_timeout() {
        let temp = tempfile::tempdir().expect("tempdir");
        let script = temp.path().join("silent.sh");
        // 关闭两个管道后 sleep；没有超时的实现会永远等 wait。
        std::fs::write(&script, "exec 1>&- 2>&-\nsleep 30\n").expect("script");
        let workdir = temp.path().to_path_buf();
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            run_script_with_cap(&script, &workdir, 1, 4096),
        )
        .await
        .expect("timeout must not hang the caller");
        let error = result.expect_err("silent long-running script must time out");
        assert!(error.contains("timed out"), "{error}");
    }
}
