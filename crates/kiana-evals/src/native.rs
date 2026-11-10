//! 原生 task suite：隔离目录、确定性 mock 模型、沙箱内测试执行、断点续跑与 replay。
//!
//! # 任务布局
//!
//! ```text
//! suites/<suite>/<task>/
//!   meta.json
//!   solution/...   # 每次 trial 开始前重置进 workspace
//!   tests/...      # 测试文件（同时是运行测试命令时的工作区内容）
//!   examples/...   # 给模型的示例文件
//! ```
//!
//! `meta.json`：
//!
//! ```json
//! {
//!   "id": "write-42",
//!   "language": "shell",
//!   "testCommand": ["sh", "-c", "grep -qx 42 answer.txt"],
//!   "timeoutMs": 60000,
//!   "maxAttempts": 2,
//!   "instructions": "Write 42 into answer.txt.",
//!   "files": [
//!     {"path": "solution/answer.txt", "role": "solution"},
//!     {"path": "tests/check.sh", "role": "test"},
//!     {"path": "examples/example.txt", "role": "example"}
//!   ],
//!   "mock": {"path": "answer.txt", "initial": "41", "repair": "42", "finalText": "done"}
//! }
//! ```
//!
//! `files` 可省略（按 `solution/`、`tests/`、`examples/` 目录自动发现）；角色决定复制到
//! workspace 时是否剥掉角色目录前缀。
//!
//! # 一次 trial
//!
//! 1. 删除并重建 `artifacts/<suite>/workspaces/<task>/trial-<n>/workspace`；
//! 2. 从任务目录复制 solution/test/example 文件（solution 因此每次都被重置）；
//! 3. 用命名 mock 轨迹驱动 `kiana-agent-core` 的完整工具循环（`write_file` / `read_file`
//!    / `bash`）；
//! 4. 通过 [`CommandRunner`] 执行测试命令（CLI 的默认实现是 [`SandboxCommandRunner`]，
//!    所有命令都在 `kiana-sandbox` 的 workspace-write 策略下运行）；
//! 5. 失败时把测试输出追加到下一次 prompt（`Test failed ... Fix the solution`），再次尝试；
//! 6. 候选结果逐条写入 `results.json`，进程中断后只跳过 `passed/failed/error` 的完整记录。
//!
//! # 结果
//!
//! 每个任务在 `artifacts/<suite>/tasks/<task>/results.json` 写自己的记录，suite 级
//! `artifacts/<suite>/results.json` 汇总 pass@1 / pass@k / well-formed% / cost。记录包含
//! `pass`、`cost`、`durationMs`、`tokens`、`malformed`、`contextExhausted`。
//!
//! # 断点与 replay
//!
//! `fingerprint` 是 suite 文件内容 + meta + 模型配置的 sha256。fingerprint 相同才复用旧记录；
//! `dry_run` 在 fingerprint 相同时直接返回旧结果（零模型调用），不同时拒绝运行。`replay`
//! 只读取既有 results.json，不接触模型与沙箱。

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use kiana_agent_core::agent_loop::{AgentEventSink, AgentLoopError, run_agent_loop};
use kiana_agent_core::budget::{BudgetConfig, BudgetController};
use kiana_agent_core::loop_guard::{LoopGuard, LoopGuardConfig};
use kiana_agent_core::types::{
    AgentContext, AgentEvent, AgentLoopConfig, AgentMessage, AgentTool, AgentToolExecute,
    AgentToolResult, AgentToolUpdateCallback, BoxFuture, StreamFn,
};
use kiana_ai::models::ProviderStreams;
use kiana_ai::providers::mock::{
    MockLlm, MockLlmAdmin, MockModelOptions, MockResponse, MockStep, MockTrajectory,
};
use kiana_ai::types::{
    AssistantMessage, ModelCost, StopReason, TextContent, TextKind, UserContent, UserContentBlock,
    UserMessage, UserRole,
};
use kiana_sandbox::{SandboxManager, SandboxPolicy, SpawnOptions, StdioMode};

// ---------------------------------------------------------------------------
// 错误
// ---------------------------------------------------------------------------

/// 原生 suite 错误。
#[derive(Debug, thiserror::Error)]
pub enum NativeError {
    /// 参数/配置错误。
    #[error("{0}")]
    Message(String),
    /// 未显式启用沙箱。
    #[error("native task suites require the sandbox; pass --sandbox")]
    SandboxRequired,
    /// 沙箱后端不可用。
    #[error("native task suites require a working sandbox backend: {0}")]
    SandboxUnavailable(String),
    /// suite 不存在。
    #[error("suite `{0}` not found under {1}")]
    SuiteNotFound(String, String),
    /// 任务级错误。
    #[error("task `{0}`: {1}")]
    Task(String, String),
    /// 文件 I/O。
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// JSON 解析/写出。
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

impl NativeError {
    fn message(message: impl Into<String>) -> Self {
        NativeError::Message(message.into())
    }

    fn task(id: &str, message: impl Into<String>) -> Self {
        NativeError::Task(id.to_string(), message.into())
    }
}

// ---------------------------------------------------------------------------
// 运行选项
// ---------------------------------------------------------------------------

/// 原生 suite 运行选项。
#[derive(Debug, Clone)]
pub struct NativeRunOptions {
    /// suite 名。
    pub suite: String,
    /// suite 根目录（其下是 `<suite>/<task>/meta.json`）。
    pub suite_root: PathBuf,
    /// 每个任务的 trial 数。
    pub trials: u32,
    /// 产物根目录。
    pub artifacts_dir: PathBuf,
    /// 是否要求沙箱；CLI 必须显式传 `--sandbox`。
    pub sandbox: bool,
    /// replay 指定的 results.json；只读，不运行。
    pub replay: Option<PathBuf>,
    /// dry-run：fingerprint 未变时零模型调用直接返回。
    pub dry_run: bool,
    /// 只跑某个任务。
    pub task_filter: Option<String>,
    /// 每次尝试的最大模型轮数。
    pub max_attempts: u32,
    /// 单条命令超时。
    pub command_timeout: Duration,
    /// mock 模型费用表。
    pub model_cost: ModelCost,
}

impl Default for NativeRunOptions {
    fn default() -> Self {
        NativeRunOptions {
            suite: "smoke".to_string(),
            suite_root: default_suite_root(),
            trials: 1,
            artifacts_dir: PathBuf::from(".eval-native"),
            sandbox: false,
            replay: None,
            dry_run: false,
            task_filter: None,
            max_attempts: 2,
            command_timeout: Duration::from_secs(120),
            model_cost: ModelCost {
                input: 3.0,
                output: 15.0,
                ..Default::default()
            },
        }
    }
}

/// 默认 suite 根目录：优先 `KIANA_EVAL_SUITE_ROOT`，否则仓库内 `crates/kiana-evals/suites`。
pub fn default_suite_root() -> PathBuf {
    std::env::var_os("KIANA_EVAL_SUITE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("crates/kiana-evals/suites"))
}

// ---------------------------------------------------------------------------
// suite / meta
// ---------------------------------------------------------------------------

/// 文件角色。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileRole {
    /// 测试文件。
    Test,
    /// 示例文件。
    Example,
    /// 参考解/初始解（每次 trial 前重置）。
    Solution,
}

impl FileRole {
    fn directory(self) -> &'static str {
        match self {
            FileRole::Test => "tests",
            FileRole::Example => "examples",
            FileRole::Solution => "solution",
        }
    }
}

/// `meta.json` 里显式声明的文件。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskFile {
    /// 相对任务根目录的路径。
    pub path: String,
    /// 角色。
    pub role: FileRole,
}

/// 测试命令：字符串或 argv 数组。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TestCommandSpec {
    /// 交给 `sh -c` 的字符串。
    Shell(String),
    /// 直接 argv。
    Argv(Vec<String>),
}

/// 确定性 mock agent 的脚本。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MockTaskSpec {
    /// 写入/修复的目标路径（相对 workspace）。
    pub path: String,
    /// 第一次尝试写入的内容。
    pub initial: String,
    /// 收到失败反馈后写入的内容。
    pub repair: String,
    /// 写完后返回的最终文本。
    #[serde(default = "default_final_text")]
    pub final_text: String,
}

fn default_final_text() -> String {
    "done".to_string()
}

/// `meta.json`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskMeta {
    /// 任务 id；缺省用目录名。
    #[serde(default)]
    pub id: Option<String>,
    /// 语言标签。
    #[serde(default)]
    pub language: Option<String>,
    /// 测试命令；缺省按语言选默认值。
    #[serde(default)]
    pub test_command: Option<TestCommandSpec>,
    /// 单条命令超时。
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    /// 最大尝试次数（默认取运行选项）。
    #[serde(default)]
    pub max_attempts: Option<u32>,
    /// 任务说明。
    #[serde(default)]
    pub instructions: Option<String>,
    /// 显式文件清单；缺省按角色目录发现。
    #[serde(default)]
    pub files: Option<Vec<TaskFile>>,
    /// 确定性 mock 脚本。
    #[serde(default)]
    pub mock: Option<MockTaskSpec>,
}

/// 一个任务的完整规格。
#[derive(Debug, Clone)]
pub struct TaskSpec {
    /// 任务 id。
    pub id: String,
    /// 任务根目录。
    pub root: PathBuf,
    /// meta。
    pub meta: TaskMeta,
    /// 文件清单。
    pub files: Vec<TaskFile>,
    /// 解析后的测试命令 argv。
    pub test_command: Vec<String>,
    /// 任务级超时。
    pub timeout: Duration,
    /// 任务级最大尝试次数。
    pub max_attempts: u32,
}

/// 一个 suite。
#[derive(Debug, Clone)]
pub struct SuiteSpec {
    /// suite 名。
    pub name: String,
    /// suite 根目录。
    pub root: PathBuf,
    /// 任务（按 id 排序）。
    pub tasks: Vec<TaskSpec>,
}

impl SuiteSpec {
    /// 发现 suite。
    pub fn discover(suite_root: &Path, suite: &str) -> Result<Self, NativeError> {
        let root = suite_root.join(suite);
        if !root.is_dir() {
            return Err(NativeError::SuiteNotFound(
                suite.to_string(),
                suite_root.display().to_string(),
            ));
        }
        let mut tasks = Vec::new();
        let mut entries: Vec<PathBuf> = std::fs::read_dir(&root)?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .collect();
        entries.sort();
        for task_root in entries {
            let meta_path = task_root.join("meta.json");
            if !meta_path.is_file() {
                continue;
            }
            let meta: TaskMeta = serde_json::from_str(&std::fs::read_to_string(&meta_path)?)?;
            let id = meta
                .id
                .clone()
                .or_else(|| {
                    task_root
                        .file_name()
                        .and_then(|name| name.to_str())
                        .map(str::to_string)
                })
                .ok_or_else(|| NativeError::message("task directory has no name"))?;
            let files = collect_files(&task_root, &meta, &id)?;
            let test_command = resolve_test_command(&meta, &id)?;
            let timeout = Duration::from_millis(meta.timeout_ms.unwrap_or(120_000).max(1));
            let max_attempts = meta.max_attempts.unwrap_or(2).max(1);
            tasks.push(TaskSpec {
                id,
                root: task_root,
                meta,
                files,
                test_command,
                timeout,
                max_attempts,
            });
        }
        tasks.sort_by(|left, right| left.id.cmp(&right.id));
        let mut seen = std::collections::BTreeSet::new();
        for task in &tasks {
            if !seen.insert(task.id.clone()) {
                return Err(NativeError::message(format!(
                    "duplicate task id `{}` in suite `{suite}`",
                    task.id
                )));
            }
        }
        if tasks.is_empty() {
            return Err(NativeError::message(format!(
                "suite `{suite}` contains no tasks (missing meta.json?)"
            )));
        }
        Ok(SuiteSpec {
            name: suite.to_string(),
            root,
            tasks,
        })
    }

    /// suite 内容指纹：所有任务文件 + meta + 模型配置。
    pub fn fingerprint(&self, model_cost: &ModelCost) -> Result<String, NativeError> {
        let mut hasher = Sha256::new();
        hasher.update(self.name.as_bytes());
        for task in &self.tasks {
            hasher.update(task.id.as_bytes());
            hasher.update(serde_json::to_vec(&task.meta)?);
            let mut files = Vec::new();
            collect_all_files(&task.root, &task.root, &mut files)?;
            files.sort_by(|left, right| left.0.cmp(&right.0));
            for (relative, bytes) in files {
                hasher.update(relative.as_bytes());
                hasher.update((bytes.len() as u64).to_le_bytes());
                hasher.update(&bytes);
            }
        }
        hasher.update(model_cost.input.to_bits().to_le_bytes());
        hasher.update(model_cost.output.to_bits().to_le_bytes());
        hasher.update(model_cost.cache_read.to_bits().to_le_bytes());
        hasher.update(model_cost.cache_write.to_bits().to_le_bytes());
        Ok(hex::encode(hasher.finalize()))
    }
}

fn collect_files(
    task_root: &Path,
    meta: &TaskMeta,
    id: &str,
) -> Result<Vec<TaskFile>, NativeError> {
    if let Some(files) = &meta.files {
        let mut collected = Vec::new();
        for file in files {
            validate_relative_path(&file.path, id)?;
            if !task_root.join(&file.path).is_file() {
                return Err(NativeError::task(
                    id,
                    format!("file `{}` does not exist", file.path),
                ));
            }
            collected.push(file.clone());
        }
        if !collected.iter().any(|file| file.role == FileRole::Solution) {
            return Err(NativeError::task(id, "task has no solution file"));
        }
        return Ok(collected);
    }
    let mut collected = Vec::new();
    for role in [FileRole::Solution, FileRole::Test, FileRole::Example] {
        let directory = task_root.join(role.directory());
        if !directory.is_dir() {
            continue;
        }
        let mut files = Vec::new();
        collect_all_files(&directory, task_root, &mut files)?;
        for (relative, _) in files {
            collected.push(TaskFile {
                path: relative,
                role,
            });
        }
    }
    if !collected.iter().any(|file| file.role == FileRole::Solution) {
        return Err(NativeError::task(id, "task has no solution file"));
    }
    collected.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(collected)
}

fn resolve_test_command(meta: &TaskMeta, id: &str) -> Result<Vec<String>, NativeError> {
    if let Some(spec) = &meta.test_command {
        return match spec {
            TestCommandSpec::Shell(command) => {
                Ok(vec!["sh".to_string(), "-c".to_string(), command.clone()])
            }
            TestCommandSpec::Argv(argv) if !argv.is_empty() => Ok(argv.clone()),
            TestCommandSpec::Argv(_) => Err(NativeError::task(id, "testCommand is empty")),
        };
    }
    match meta.language.as_deref().unwrap_or("shell") {
        "python" | "py" => Ok(vec![
            "python3".to_string(),
            "-m".to_string(),
            "pytest".to_string(),
            "-q".to_string(),
        ]),
        "rust" => Ok(vec![
            "cargo".to_string(),
            "test".to_string(),
            "--quiet".to_string(),
        ]),
        language => Err(NativeError::task(
            id,
            format!("language `{language}` needs an explicit testCommand"),
        )),
    }
}

fn collect_all_files(
    directory: &Path,
    base: &Path,
    output: &mut Vec<(String, Vec<u8>)>,
) -> Result<(), NativeError> {
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_all_files(&path, base, output)?;
        } else if path.is_file() {
            let relative = path
                .strip_prefix(base)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            output.push((relative, std::fs::read(&path)?));
        }
    }
    Ok(())
}

fn validate_relative_path(path: &str, id: &str) -> Result<(), NativeError> {
    let parsed = Path::new(path);
    if parsed.is_absolute()
        || parsed
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::RootDir))
    {
        return Err(NativeError::task(id, format!("unsafe file path `{path}`")));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 命令执行
// ---------------------------------------------------------------------------

/// 一条命令的结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandOutput {
    /// argv。
    pub command: Vec<String>,
    /// 退出码（被信号杀死时为 None）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// 是否超时。
    pub timed_out: bool,
    /// stdout（已截断到 64 KiB）。
    pub stdout: String,
    /// stderr（已截断到 64 KiB）。
    pub stderr: String,
    /// 耗时。
    pub duration_ms: u64,
}

impl CommandOutput {
    /// 是否成功。
    pub fn success(&self) -> bool {
        !self.timed_out && self.exit_code == Some(0)
    }
}

/// 命令执行器；CLI 用 [`SandboxCommandRunner`]，测试可注入假实现。
#[async_trait]
pub trait CommandRunner: Send + Sync {
    /// 在 `cwd` 里执行 `command`。
    async fn run(
        &self,
        command: &[String],
        cwd: &Path,
        timeout: Duration,
    ) -> Result<CommandOutput, NativeError>;
}

/// 所有命令都走 `kiana-sandbox` 的执行器。
#[derive(Debug, Clone)]
pub struct SandboxCommandRunner {
    probe_type: String,
}

impl SandboxCommandRunner {
    /// 探测当前平台可用的沙箱后端；不可用时返回 [`NativeError::SandboxUnavailable`]。
    pub fn probe() -> Result<Self, NativeError> {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let manager = SandboxManager::new(SandboxPolicy::workspace_write(cwd.clone()), cwd.clone());
        if !manager.sandbox_type().is_sandboxed() {
            return Err(NativeError::SandboxUnavailable(format!(
                "sandbox backend selected `{}`; set KIANA_SANDBOX=bwrap/landlock or install bwrap",
                manager.sandbox_type().as_str()
            )));
        }
        Ok(SandboxCommandRunner {
            probe_type: manager.sandbox_type().as_str().to_string(),
        })
    }

    /// 探测到的后端名。
    pub fn backend(&self) -> &str {
        &self.probe_type
    }
}

#[async_trait]
impl CommandRunner for SandboxCommandRunner {
    async fn run(
        &self,
        command: &[String],
        cwd: &Path,
        timeout: Duration,
    ) -> Result<CommandOutput, NativeError> {
        if command.is_empty() {
            return Err(NativeError::message("refusing to run an empty command"));
        }
        std::fs::create_dir_all(cwd)?;
        let manager = SandboxManager::new(SandboxPolicy::workspace_write(cwd), cwd);
        if !manager.sandbox_type().is_sandboxed() {
            return Err(NativeError::SandboxUnavailable(format!(
                "sandbox backend selected `{}`",
                manager.sandbox_type().as_str()
            )));
        }
        let started = Instant::now();
        let mut child = manager
            .spawn(
                &command[0],
                &command[1..],
                &SpawnOptions {
                    cwd: Some(cwd.to_path_buf()),
                    stdin: StdioMode::Ignore,
                    stdout: StdioMode::Pipe,
                    stderr: StdioMode::Pipe,
                    detached: false,
                    windows_hide: true,
                    env: Some(sandbox_env(cwd)),
                    ..SpawnOptions::default()
                },
            )
            .map_err(|error| NativeError::SandboxUnavailable(error.to_string()))?;

        let stdout_pipe = child.stdout.take();
        let stderr_pipe = child.stderr.take();
        let read_stdout = tokio::spawn(async move {
            let mut buffer = Vec::new();
            if let Some(mut pipe) = stdout_pipe {
                use tokio::io::AsyncReadExt;
                let _ = pipe.read_to_end(&mut buffer).await;
            }
            buffer
        });
        let read_stderr = tokio::spawn(async move {
            let mut buffer = Vec::new();
            if let Some(mut pipe) = stderr_pipe {
                use tokio::io::AsyncReadExt;
                let _ = pipe.read_to_end(&mut buffer).await;
            }
            buffer
        });
        let (timed_out, status) = match tokio::time::timeout(timeout, child.wait()).await {
            Ok(status) => (false, status.ok()),
            Err(_) => {
                let _ = child.kill().await;
                (true, child.wait().await.ok())
            }
        };
        let stdout = read_stdout.await.unwrap_or_default();
        let stderr = read_stderr.await.unwrap_or_default();
        let duration = started.elapsed();
        Ok(CommandOutput {
            command: command.to_vec(),
            exit_code: status.and_then(|status| status.code()),
            timed_out,
            stdout: truncate_output(String::from_utf8_lossy(&stdout).into_owned()),
            stderr: truncate_output(String::from_utf8_lossy(&stderr).into_owned()),
            duration_ms: duration.as_millis() as u64,
        })
    }
}

fn sandbox_env(cwd: &Path) -> Vec<(OsString, OsString)> {
    let path = std::env::var_os("PATH").unwrap_or_else(|| OsString::from("/usr/bin:/bin"));
    vec![
        (OsString::from("PATH"), path),
        (OsString::from("HOME"), cwd.as_os_str().to_os_string()),
        (OsString::from("TMPDIR"), cwd.join(".tmp").into_os_string()),
        (OsString::from("LANG"), OsString::from("C.UTF-8")),
        (OsString::from("LC_ALL"), OsString::from("C.UTF-8")),
    ]
}

fn truncate_output(mut text: String) -> String {
    const LIMIT: usize = 64 * 1024;
    if text.len() <= LIMIT {
        return text;
    }
    let mut split = LIMIT;
    while split > 0 && !text.is_char_boundary(split) {
        split -= 1;
    }
    text.truncate(split);
    text.push_str("\n… (truncated)");
    text
}

// ---------------------------------------------------------------------------
// 结果
// ---------------------------------------------------------------------------

/// trial 状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrialStatus {
    /// 正在运行（进程中断时保留该状态，下次会重跑）。
    Running,
    /// 通过。
    Passed,
    /// 测试失败。
    Failed,
    /// 运行错误。
    Error,
}

impl TrialStatus {
    /// 是否是完整记录（断点续跑只跳过这些）。
    pub fn is_complete(self) -> bool {
        matches!(
            self,
            TrialStatus::Passed | TrialStatus::Failed | TrialStatus::Error
        )
    }
}

/// 一次尝试的记录。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttemptResult {
    /// 1-based 尝试序号。
    pub attempt: u32,
    /// 本轮 prompt 的末尾预览。
    pub prompt_preview: String,
    /// assistant 消息数。
    pub assistant_turns: u32,
    /// 调用的工具名（按调用顺序）。
    pub tools: Vec<String>,
    /// 测试命令结果（agent loop 硬失败时可能没有）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test: Option<CommandOutput>,
    /// token 合计。
    pub tokens: u64,
    /// 费用。
    pub cost: f64,
    /// 价格未知。
    pub cost_unknown: bool,
    /// 格式错误次数。
    pub malformed: u32,
    /// 是否上下文耗尽。
    pub context_exhausted: bool,
    /// 耗时。
    pub duration_ms: u64,
    /// 失败原因。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// 一个 task × trial 的结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrialResult {
    /// 任务 id。
    pub task: String,
    /// 1-based trial 序号。
    pub trial: u32,
    /// 状态。
    pub status: TrialStatus,
    /// 是否通过。
    pub pass: bool,
    /// 尝试记录。
    pub attempts: Vec<AttemptResult>,
    /// 总耗时。
    pub duration_ms: u64,
    /// token 合计。
    pub tokens: u64,
    /// cost 合计。
    pub cost: f64,
    /// 价格未知。
    pub cost_unknown: bool,
    /// 格式错误总数。
    pub malformed: u32,
    /// 是否上下文耗尽。
    pub context_exhausted: bool,
    /// 错误文本。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// workspace 路径。
    pub workspace: String,
}

/// suite 汇总。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeSummary {
    /// 任务数。
    pub tasks: usize,
    /// trial 数。
    pub trials: u32,
    /// pass@1（通过 trial / 总 trial）。
    pub pass_at_1: f64,
    /// pass@k（按任务：前 k 个 trial 至少一个通过）。
    pub pass_at_k: f64,
    /// `k`（等于 trials）。
    pub k: u32,
    /// 格式正确率（0-100）。
    pub well_formed_percent: f64,
    /// cost 合计。
    pub total_cost: f64,
    /// 是否有未知价格。
    pub cost_unknown: bool,
    /// 是否全部通过。
    pub all_passed: bool,
}

/// suite 结果文件。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeSuiteResults {
    /// suite 名。
    pub suite: String,
    /// 内容指纹。
    pub fingerprint: String,
    /// 模型 id。
    pub model: String,
    /// 计划 trial 数。
    pub trials: u32,
    /// 开始时间（RFC3339）。
    pub started_at: String,
    /// 结束时间。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    /// 全部记录。
    pub records: Vec<TrialResult>,
    /// 汇总。
    pub summary: NativeSummary,
    /// mock provider 收到的请求总数。
    pub model_calls: u64,
}

/// 每个任务的 results.json。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskResults {
    /// suite 名。
    pub suite: String,
    /// 任务 id。
    pub task: String,
    /// 指纹。
    pub fingerprint: String,
    /// 记录。
    pub records: Vec<TrialResult>,
}

impl NativeSuiteResults {
    fn new(suite: &str, fingerprint: &str, trials: u32) -> Self {
        NativeSuiteResults {
            suite: suite.to_string(),
            fingerprint: fingerprint.to_string(),
            model: "mock".to_string(),
            trials,
            started_at: chrono::Utc::now().to_rfc3339(),
            finished_at: None,
            records: Vec::new(),
            summary: NativeSummary {
                tasks: 0,
                trials,
                pass_at_1: 0.0,
                pass_at_k: 0.0,
                k: trials,
                well_formed_percent: 0.0,
                total_cost: 0.0,
                cost_unknown: false,
                all_passed: false,
            },
            model_calls: 0,
        }
    }

    /// 重新计算 summary。
    pub fn recompute_summary(&mut self) {
        let mut task_ids: Vec<String> = self
            .records
            .iter()
            .map(|record| record.task.clone())
            .collect();
        task_ids.sort();
        task_ids.dedup();
        let completed: Vec<&TrialResult> = self
            .records
            .iter()
            .filter(|record| record.status.is_complete())
            .collect();
        let total_trials = completed.len().max(1);
        let passed_trials = completed.iter().filter(|record| record.pass).count();
        let pass_at_1 = passed_trials as f64 / total_trials as f64;
        let mut pass_at_k_sum = 0.0;
        for task in &task_ids {
            let mut trials: Vec<&TrialResult> = completed
                .iter()
                .copied()
                .filter(|record| &record.task == task)
                .collect();
            trials.sort_by_key(|record| record.trial);
            let any = trials
                .iter()
                .take(self.trials.max(1) as usize)
                .any(|record| record.pass);
            if any {
                pass_at_k_sum += 1.0;
            }
        }
        let pass_at_k = if task_ids.is_empty() {
            0.0
        } else {
            pass_at_k_sum / task_ids.len() as f64
        };
        let attempts: u64 = completed
            .iter()
            .map(|record| record.attempts.len() as u64)
            .sum();
        let malformed: u64 = completed.iter().map(|record| record.malformed as u64).sum();
        let well_formed_percent = if attempts == 0 {
            100.0
        } else {
            (attempts.saturating_sub(malformed)) as f64 / attempts as f64 * 100.0
        };
        self.summary = NativeSummary {
            tasks: task_ids.len(),
            trials: self.trials,
            pass_at_1,
            pass_at_k,
            k: self.trials,
            well_formed_percent,
            total_cost: completed.iter().map(|record| record.cost).sum(),
            cost_unknown: completed.iter().any(|record| record.cost_unknown),
            all_passed: !task_ids.is_empty()
                && task_ids.iter().all(|task| {
                    completed
                        .iter()
                        .filter(|record| &record.task == task)
                        .any(|record| record.pass)
                }),
        };
    }

    fn record_for(&mut self, task: &str, trial: u32) -> Option<&mut TrialResult> {
        self.records
            .iter_mut()
            .find(|record| record.task == task && record.trial == trial)
    }
}

// ---------------------------------------------------------------------------
// runner
// ---------------------------------------------------------------------------

/// 原生 suite 运行器。
pub struct NativeRunner {
    options: NativeRunOptions,
    admin: MockLlmAdmin,
    command_runner: Arc<dyn CommandRunner>,
}

impl NativeRunner {
    /// 注入命令执行器（测试）。
    pub fn with_command_runner(
        options: NativeRunOptions,
        command_runner: Arc<dyn CommandRunner>,
    ) -> Self {
        NativeRunner {
            options,
            admin: MockLlmAdmin::new(),
            command_runner,
        }
    }

    /// 使用沙箱执行器（CLI）。
    pub fn with_sandbox(options: NativeRunOptions) -> Result<Self, NativeError> {
        if !options.sandbox {
            return Err(NativeError::SandboxRequired);
        }
        let runner = SandboxCommandRunner::probe()?;
        Ok(NativeRunner::with_command_runner(options, Arc::new(runner)))
    }

    /// 执行或 replay。
    pub async fn run(&mut self) -> Result<NativeSuiteResults, NativeError> {
        if let Some(replay) = self.options.replay.clone() {
            let contents = std::fs::read_to_string(&replay)?;
            let mut results: NativeSuiteResults = serde_json::from_str(&contents)?;
            results.recompute_summary();
            return Ok(results);
        }
        let suite = SuiteSpec::discover(&self.options.suite_root, &self.options.suite)?;
        let fingerprint = suite.fingerprint(&self.options.model_cost)?;
        let results_path = self.suite_results_path();
        let existing = read_results(&results_path)?;

        if self.options.dry_run {
            return match existing {
                Some(results) if results.fingerprint == fingerprint => Ok(results),
                Some(_) => Err(NativeError::message(
                    "dry-run fingerprint changed; refusing to call the model",
                )),
                None => Err(NativeError::message(
                    "dry-run found no previous results for this suite",
                )),
            };
        }
        if !self.options.sandbox {
            return Err(NativeError::SandboxRequired);
        }

        let mut results = match existing {
            Some(existing) if existing.fingerprint == fingerprint => existing,
            _ => NativeSuiteResults::new(&suite.name, &fingerprint, self.options.trials),
        };
        results.fingerprint = fingerprint.clone();
        results.trials = self.options.trials;
        results.model = format!("mock:{}", suite.name);
        results.finished_at = None;

        let mut model_calls = 0u64;
        for task in &suite.tasks {
            if let Some(filter) = &self.options.task_filter
                && filter != &task.id
            {
                continue;
            }
            let provider = self.register_task(task)?;
            for trial in 1..=self.options.trials {
                let completed = results
                    .record_for(&task.id, trial)
                    .is_some_and(|record| record.status.is_complete());
                if completed {
                    continue;
                }
                // 先落一条 running 记录：进程中断时它不会被当成完成。
                results
                    .records
                    .retain(|record| !(record.task == task.id && record.trial == trial));
                results.records.push(TrialResult {
                    task: task.id.clone(),
                    trial,
                    status: TrialStatus::Running,
                    pass: false,
                    attempts: Vec::new(),
                    duration_ms: 0,
                    tokens: 0,
                    cost: 0.0,
                    cost_unknown: false,
                    malformed: 0,
                    context_exhausted: false,
                    error: None,
                    workspace: self.workspace_path(&task.id, trial).display().to_string(),
                });
                results.recompute_summary();
                write_results(&results_path, &results)?;
                write_task_results(&self.task_results_path(&task.id), &results, &task.id)?;

                let (record, trial_calls) = self
                    .run_trial(task, &provider, trial)
                    .await
                    .map_err(|error| NativeError::task(&task.id, error.to_string()))?;
                model_calls += trial_calls;
                if let Some(slot) = results.record_for(&task.id, trial) {
                    *slot = record;
                }
                results.model_calls = model_calls;
                results.recompute_summary();
                write_results(&results_path, &results)?;
                write_task_results(&self.task_results_path(&task.id), &results, &task.id)?;
            }
        }
        results.model_calls = model_calls;
        results.finished_at = Some(chrono::Utc::now().to_rfc3339());
        results.recompute_summary();
        write_results(&results_path, &results)?;
        Ok(results)
    }

    /// results.json 路径。
    pub fn suite_results_path(&self) -> PathBuf {
        self.options
            .artifacts_dir
            .join(&self.options.suite)
            .join("results.json")
    }

    fn task_results_path(&self, task: &str) -> PathBuf {
        self.options
            .artifacts_dir
            .join(&self.options.suite)
            .join("tasks")
            .join(task)
            .join("results.json")
    }

    fn workspace_path(&self, task: &str, trial: u32) -> PathBuf {
        self.options
            .artifacts_dir
            .join(&self.options.suite)
            .join("workspaces")
            .join(task)
            .join(format!("trial-{trial}"))
            .join("workspace")
    }

    fn register_task(
        &self,
        task: &TaskSpec,
    ) -> Result<kiana_ai::providers::mock::MockProviderHandle, NativeError> {
        let spec = task.meta.mock.clone().ok_or_else(|| {
            NativeError::task(
                &task.id,
                "task has no deterministic `mock` trajectory; add meta.mock",
            )
        })?;
        let step = MockStep::dynamic(move |request| {
            if !request.changed_files.is_empty() {
                return MockResponse::text(spec.final_text.clone());
            }
            let content = if request.prompt_text.contains("Test failed") {
                spec.repair.clone()
            } else {
                spec.initial.clone()
            };
            mock_write_call(&spec.path, &content)
        });
        // 每次尝试会 reset，单个尝试最多 2-3 个请求；32 份克隆足够覆盖长轨迹。
        let trajectory = MockTrajectory::from_steps(vec![step; 32]);
        self.admin.register(&task.id, trajectory);
        self.admin
            .provider(
                &task.id,
                MockModelOptions {
                    id: Some(format!("mock-{}", task.id)),
                    cost: Some(self.options.model_cost.clone()),
                    ..Default::default()
                },
            )
            .map_err(|error| NativeError::task(&task.id, error.to_string()))
    }

    async fn run_trial(
        &self,
        task: &TaskSpec,
        provider: &kiana_ai::providers::mock::MockProviderHandle,
        trial: u32,
    ) -> Result<(TrialResult, u64), NativeError> {
        let workspace = self.workspace_path(&task.id, trial);
        if workspace.exists() {
            std::fs::remove_dir_all(&workspace)?;
        }
        std::fs::create_dir_all(&workspace)?;
        reset_workspace(task, &workspace)?;

        let started = Instant::now();
        let max_attempts = task.max_attempts.max(self.options.max_attempts);
        let mut attempts: Vec<AttemptResult> = Vec::new();
        let mut feedback = String::new();
        let mut passed = false;
        let mut error: Option<String> = None;
        let mut model_calls = 0u64;

        for attempt in 1..=max_attempts {
            // 每次尝试都从轨迹起点开始；请求记录仍保留在 attempts 里。
            let _ = self.admin.reset(&task.id);
            let prompt = build_prompt(task, &workspace, &feedback);
            let attempt_started = Instant::now();
            let agent = self
                .run_agent_attempt(task, provider, &workspace, &prompt)
                .await;
            model_calls += self
                .admin
                .requests(&task.id)
                .map(|requests| requests.len() as u64)
                .unwrap_or(0);
            let mut attempt_result = match agent {
                Ok(result) => result,
                Err(failure) => {
                    attempts.push(AttemptResult {
                        attempt,
                        prompt_preview: tail_preview(&prompt, 1200),
                        assistant_turns: 0,
                        tools: Vec::new(),
                        test: None,
                        tokens: 0,
                        cost: 0.0,
                        cost_unknown: false,
                        malformed: 0,
                        context_exhausted: matches!(failure, AgentFailure::ContextExhausted),
                        duration_ms: attempt_started.elapsed().as_millis() as u64,
                        error: Some(failure.to_string()),
                    });
                    error = Some(failure.to_string());
                    break;
                }
            };
            attempt_result.attempt = attempt;
            attempt_result.duration_ms = attempt_started.elapsed().as_millis() as u64;

            let test = self
                .command_runner
                .run(&task.test_command, &workspace, task.timeout)
                .await?;
            let success = test.success();
            attempt_result.test = Some(test.clone());
            attempts.push(attempt_result);
            if success {
                passed = true;
                break;
            }
            let stdout = test.stdout.trim();
            let stderr = test.stderr.trim();
            feedback = format!(
                "Test failed (attempt {attempt}/{max_attempts}).\n\
                 Exit code: {:?}\n\
                 STDOUT:\n{}\n\
                 STDERR:\n{}\n\n\
                 Fix the solution and try again. The workspace is at `{}`.",
                test.exit_code,
                truncate_for_prompt(stdout),
                truncate_for_prompt(stderr),
                workspace.display()
            );
        }

        let duration_ms = started.elapsed().as_millis() as u64;
        let tokens = attempts.iter().map(|attempt| attempt.tokens).sum();
        let cost: f64 = attempts.iter().map(|attempt| attempt.cost).sum();
        let cost_unknown = attempts.iter().any(|attempt| attempt.cost_unknown);
        let malformed: u32 = attempts.iter().map(|attempt| attempt.malformed).sum();
        let context_exhausted = attempts.iter().any(|attempt| attempt.context_exhausted);
        let status = if passed {
            TrialStatus::Passed
        } else if error.is_some() {
            TrialStatus::Error
        } else {
            TrialStatus::Failed
        };
        Ok((
            TrialResult {
                task: task.id.clone(),
                trial,
                status,
                pass: passed,
                attempts,
                duration_ms,
                tokens,
                cost,
                cost_unknown,
                malformed,
                context_exhausted,
                error,
                workspace: workspace.display().to_string(),
            },
            model_calls,
        ))
    }

    async fn run_agent_attempt(
        &self,
        task: &TaskSpec,
        provider: &kiana_ai::providers::mock::MockProviderHandle,
        workspace: &Path,
        prompt: &str,
    ) -> Result<AttemptResult, AgentFailure> {
        let tools = native_tools(workspace, self.command_runner.clone(), task.timeout);
        let context = AgentContext {
            messages: Vec::new(),
            tools: Some(tools),
        };
        let budget = Arc::new(Mutex::new(BudgetController::new(
            BudgetConfig::new().with_max_turns(8),
            &provider.model,
        )));
        let guard = Arc::new(Mutex::new(LoopGuard::new(LoopGuardConfig::default())));
        let config = AgentLoopConfig {
            model: provider.model.clone(),
            budget: Some(budget),
            loop_guard: Some(guard),
            ..Default::default()
        };
        let core: MockLlm = provider.core.clone();
        let stream_fn: StreamFn = Arc::new(move |model, context, options| {
            let core = core.clone();
            let stream = core.stream_simple(&model, &context, options.unwrap_or_default());
            Box::pin(async move { Ok(stream) })
        });
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink_events = events.clone();
        let emit: AgentEventSink = Arc::new(move |event| {
            sink_events
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(event);
            Box::pin(async {})
        });
        let result = run_agent_loop(
            vec![AgentMessage::Standard(kiana_ai::types::Message::User(
                UserMessage {
                    role: UserRole::User,
                    content: UserContent::Text(prompt.to_string()),
                    timestamp: now_unix_ms(),
                },
            ))],
            context,
            config,
            emit,
            None,
            Some(stream_fn),
        )
        .await;

        let events = events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        if let Err(error) = &result {
            if matches!(error, AgentLoopError::LoopDetected(_)) {
                return Err(AgentFailure::Loop(error.to_string()));
            }
            let message = error.to_string();
            if is_context_error_text(&message) {
                return Err(AgentFailure::ContextExhausted);
            }
            return Err(AgentFailure::Error(message));
        }

        let mut assistant_turns = 0u32;
        let mut tokens = 0u64;
        let mut cost = 0.0f64;
        let mut malformed = 0u32;
        let mut context_exhausted = false;
        let mut tool_names = Vec::new();
        for event in &events {
            if let AgentEvent::ToolExecutionStart { tool_name, .. } = event {
                tool_names.push(tool_name.clone());
            }
            let AgentEvent::MessageEnd {
                message: AgentMessage::Standard(kiana_ai::types::Message::Assistant(message)),
            } = event
            else {
                continue;
            };
            assistant_turns += 1;
            tokens += if message.usage.total_tokens > 0 {
                message.usage.total_tokens
            } else {
                message.usage.input
                    + message.usage.output
                    + message.usage.cache_read
                    + message.usage.cache_write
            };
            cost += message.usage.cost.total;
            if is_malformed(message) {
                malformed += 1;
            }
            if is_context_exhausted(message) {
                context_exhausted = true;
            }
        }
        if !result.is_ok() {
            return Err(AgentFailure::Error("agent loop failed".to_string()));
        }
        Ok(AttemptResult {
            attempt: 0,
            prompt_preview: tail_preview(prompt, 1200),
            assistant_turns,
            tools: tool_names,
            test: None,
            tokens,
            cost,
            cost_unknown: false,
            malformed,
            context_exhausted,
            duration_ms: 0,
            error: None,
        })
    }
}

/// agent attempt 的失败分类。
#[derive(Debug, Clone, PartialEq, Eq)]
enum AgentFailure {
    ContextExhausted,
    Loop(String),
    Error(String),
}

impl std::fmt::Display for AgentFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentFailure::ContextExhausted => formatter.write_str("context exhausted"),
            AgentFailure::Loop(message) => write!(formatter, "loop detected: {message}"),
            AgentFailure::Error(message) => formatter.write_str(message),
        }
    }
}

fn native_tools(
    workspace: &Path,
    runner: Arc<dyn CommandRunner>,
    timeout: Duration,
) -> Vec<AgentTool> {
    let workspace = Arc::new(workspace.to_path_buf());
    let write_workspace = workspace.clone();
    let read_workspace = workspace.clone();
    let bash_workspace = workspace.clone();
    vec![
        make_tool(
            "write_file",
            "Write a UTF-8 file inside the task workspace.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "content": {"type": "string"}
                },
                "required": ["path", "content"]
            }),
            Arc::new(move |params| {
                let path = params
                    .get("path")
                    .and_then(|value| value.as_str())
                    .unwrap_or("");
                let content = params
                    .get("content")
                    .and_then(|value| value.as_str())
                    .unwrap_or("");
                match safe_join(&write_workspace, path) {
                    Ok(target) => {
                        if let Some(parent) = target.parent()
                            && let Err(error) = std::fs::create_dir_all(parent)
                        {
                            return error_result(&format!("failed to create directory: {error}"));
                        }
                        match std::fs::write(&target, content) {
                            Ok(()) => {
                                text_result(&format!("wrote {} bytes to {path}", content.len()))
                            }
                            Err(error) => error_result(&format!("failed to write {path}: {error}")),
                        }
                    }
                    Err(message) => error_result(&message),
                }
            }),
        ),
        make_tool(
            "read_file",
            "Read a UTF-8 file from the task workspace.",
            serde_json::json!({
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"]
            }),
            Arc::new(move |params| {
                let path = params
                    .get("path")
                    .and_then(|value| value.as_str())
                    .unwrap_or("");
                match safe_join(&read_workspace, path) {
                    Ok(target) => match std::fs::read_to_string(&target) {
                        Ok(content) => text_result(&content),
                        Err(error) => error_result(&format!("failed to read {path}: {error}")),
                    },
                    Err(message) => error_result(&message),
                }
            }),
        ),
        AgentTool {
            name: "bash".to_string(),
            label: "bash".to_string(),
            description: "Run a shell command in the task workspace (sandboxed).".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {"command": {"type": "string"}},
                "required": ["command"]
            }),
            constrained_sampling: None,
            output_schema: None,
            prepare_arguments: None,
            execution_mode: None,
            replay: None,
            execute: Arc::new(BashExec {
                runner,
                workspace: bash_workspace,
                timeout,
            }),
        },
    ]
}

/// `bash` 工具：异步等待命令执行器（不阻塞 agent loop 的任务）。
struct BashExec {
    runner: Arc<dyn CommandRunner>,
    workspace: Arc<PathBuf>,
    timeout: Duration,
}

impl AgentToolExecute for BashExec {
    fn execute<'a>(
        &'a self,
        _tool_call_id: &'a str,
        params: serde_json::Value,
        _signal: Option<&'a tokio_util::sync::CancellationToken>,
        _on_update: Option<AgentToolUpdateCallback>,
    ) -> BoxFuture<
        'a,
        Result<AgentToolResult<serde_json::Value>, kiana_agent_core::types::AgentToolError>,
    > {
        Box::pin(async move {
            let command = params
                .get("command")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            let argv = vec!["sh".to_string(), "-c".to_string(), command.to_string()];
            match self.runner.run(&argv, &self.workspace, self.timeout).await {
                Ok(output) => {
                    let mut text = output.stdout.clone();
                    if !output.stderr.is_empty() {
                        text.push_str("\nSTDERR:\n");
                        text.push_str(&output.stderr);
                    }
                    let mut result = text_result(&text);
                    if !output.success() {
                        result.is_error = Some(true);
                    }
                    Ok(result)
                }
                Err(error) => Ok(error_result(&error.to_string())),
            }
        })
    }
}

fn make_tool(
    name: &str,
    description: &str,
    schema: serde_json::Value,
    execute: Arc<dyn Fn(serde_json::Value) -> AgentToolResult<serde_json::Value> + Send + Sync>,
) -> AgentTool {
    struct Exec {
        execute: Arc<dyn Fn(serde_json::Value) -> AgentToolResult<serde_json::Value> + Send + Sync>,
    }
    impl AgentToolExecute for Exec {
        fn execute<'a>(
            &'a self,
            _tool_call_id: &'a str,
            params: serde_json::Value,
            _signal: Option<&'a tokio_util::sync::CancellationToken>,
            _on_update: Option<AgentToolUpdateCallback>,
        ) -> BoxFuture<
            'a,
            Result<AgentToolResult<serde_json::Value>, kiana_agent_core::types::AgentToolError>,
        > {
            let result = (self.execute)(params);
            Box::pin(async move { Ok(result) })
        }
    }
    AgentTool {
        name: name.to_string(),
        label: name.to_string(),
        description: description.to_string(),
        parameters: schema,
        constrained_sampling: None,
        output_schema: None,
        prepare_arguments: None,
        execution_mode: None,
        replay: None,
        execute: Arc::new(Exec { execute }),
    }
}

fn text_result(text: &str) -> AgentToolResult<serde_json::Value> {
    AgentToolResult {
        content: vec![UserContentBlock::Text(TextContent {
            kind_text: TextKind::Text,
            text: text.to_string(),
            text_signature: None,
        })],
        details: serde_json::Value::Null,
        structured_content: None,
        usage: None,
        is_error: None,
        terminate: None,
    }
}

fn error_result(message: &str) -> AgentToolResult<serde_json::Value> {
    let mut result = text_result(message);
    result.is_error = Some(true);
    result
}

fn safe_join(workspace: &Path, path: &str) -> Result<PathBuf, String> {
    let parsed = Path::new(path);
    if parsed.is_absolute()
        || parsed
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::RootDir))
    {
        return Err(format!("unsafe path `{path}`"));
    }
    Ok(workspace.join(parsed))
}

fn mock_write_call(path: &str, content: &str) -> MockResponse {
    MockResponse::tool_call(
        "write_file",
        [
            (
                "path".to_string(),
                serde_json::Value::String(path.to_string()),
            ),
            (
                "content".to_string(),
                serde_json::Value::String(content.to_string()),
            ),
        ]
        .into_iter()
        .collect(),
    )
}

fn build_prompt(task: &TaskSpec, workspace: &Path, feedback: &str) -> String {
    let mut prompt = String::new();
    prompt.push_str(&format!("# Evaluation task: {}\n\n", task.id));
    if let Some(instructions) = &task.meta.instructions {
        prompt.push_str(instructions);
        prompt.push_str("\n\n");
    }
    prompt.push_str(
        "You are working inside an isolated workspace. Use `write_file` to create or edit \
         files, `read_file` to inspect them, and `bash` for shell commands. Do not use paths \
         outside the workspace. When you are done, reply with a short final message; the \
         test command is run automatically.\n\n",
    );
    let mut examples = Vec::new();
    for file in &task.files {
        if file.role == FileRole::Example {
            let destination = role_relative_path(file);
            examples.push(destination);
        }
    }
    if !examples.is_empty() {
        prompt.push_str("Example files in the workspace: ");
        prompt.push_str(&examples.join(", "));
        prompt.push_str("\n\n");
    }
    prompt.push_str(&format!("Workspace: {}\n", workspace.display()));
    if !feedback.is_empty() {
        prompt.push_str("\n---\n\n");
        prompt.push_str(feedback);
        prompt.push('\n');
    }
    prompt
}

fn reset_workspace(task: &TaskSpec, workspace: &Path) -> Result<(), NativeError> {
    for file in &task.files {
        let source = task.root.join(&file.path);
        let relative = role_relative_path(file);
        let destination = workspace.join(&relative);
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(&source, &destination).map_err(|error| {
            NativeError::task(&task.id, format!("failed to copy `{}`: {error}", file.path))
        })?;
    }
    Ok(())
}

fn role_relative_path(file: &TaskFile) -> String {
    let directory = file.role.directory();
    file.path
        .strip_prefix(&format!("{directory}/"))
        .unwrap_or(&file.path)
        .to_string()
}

fn is_malformed(message: &AssistantMessage) -> bool {
    if message.stop_reason == StopReason::Pending {
        return true;
    }
    message.content.iter().any(|block| {
        matches!(
            block,
            kiana_ai::types::AssistantContentBlock::ToolCall(call)
                if call.arguments_error.is_some()
        )
    })
}

fn is_context_exhausted(message: &AssistantMessage) -> bool {
    message
        .error_message
        .as_deref()
        .is_some_and(is_context_error_text)
}

fn is_context_error_text(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    (lower.contains("context") || lower.contains("prompt"))
        && (lower.contains("exceed")
            || lower.contains("overflow")
            || lower.contains("too long")
            || lower.contains("maximum")
            || lower.contains("limit"))
}

fn tail_preview(text: &str, limit: usize) -> String {
    let chars = text.chars().count();
    if chars <= limit {
        return text.to_string();
    }
    let tail: String = text.chars().skip(chars - limit).collect();
    format!("…{tail}")
}

fn truncate_for_prompt(text: &str) -> String {
    const LIMIT: usize = 4_000;
    if text.chars().count() <= LIMIT {
        return text.to_string();
    }
    let kept: String = text.chars().take(LIMIT).collect();
    format!("{kept}\n… (truncated)")
}

fn now_unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// 结果读写
// ---------------------------------------------------------------------------

fn read_results(path: &Path) -> Result<Option<NativeSuiteResults>, NativeError> {
    if !path.is_file() {
        return Ok(None);
    }
    let contents = std::fs::read_to_string(path)?;
    Ok(Some(serde_json::from_str(&contents)?))
}

fn write_results(path: &Path, results: &NativeSuiteResults) -> Result<(), NativeError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, serde_json::to_string_pretty(results)?)?;
    std::fs::rename(temporary, path)?;
    Ok(())
}

fn write_task_results(
    path: &Path,
    results: &NativeSuiteResults,
    task: &str,
) -> Result<(), NativeError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let task_results = TaskResults {
        suite: results.suite.clone(),
        task: task.to_string(),
        fingerprint: results.fingerprint.clone(),
        records: results
            .records
            .iter()
            .filter(|record| record.task == task)
            .cloned()
            .collect(),
    };
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, serde_json::to_string_pretty(&task_results)?)?;
    std::fs::rename(temporary, path)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

/// replay / dry-run 的占位执行器：这些路径不会真正运行命令。
struct NoopRunner;

#[async_trait]
impl CommandRunner for NoopRunner {
    async fn run(
        &self,
        _command: &[String],
        _cwd: &Path,
        _timeout: Duration,
    ) -> Result<CommandOutput, NativeError> {
        Err(NativeError::message(
            "command runner is not available in replay/dry-run mode",
        ))
    }
}

/// `kiana-evals run ...` 的入口。
pub async fn run_cli(args: &[String]) -> Result<i32, NativeError> {
    let mut options = NativeRunOptions::default();
    let mut index = 0;
    while index < args.len() {
        let argument = &args[index];
        let mut next = |name: &str| -> Result<String, NativeError> {
            index += 1;
            args.get(index)
                .cloned()
                .ok_or_else(|| NativeError::message(format!("Missing value for {name}.")))
        };
        match argument.as_str() {
            "--suite" => options.suite = next("--suite")?,
            "--trials" => {
                let value = next("--trials")?;
                options.trials = value
                    .parse::<u32>()
                    .map_err(|_| NativeError::message(format!("Invalid --trials value: {value}")))?
                    .max(1);
            }
            "--suite-root" => options.suite_root = PathBuf::from(next("--suite-root")?),
            "--artifacts" => options.artifacts_dir = PathBuf::from(next("--artifacts")?),
            "--task" => options.task_filter = Some(next("--task")?),
            "--max-attempts" => {
                let value = next("--max-attempts")?;
                options.max_attempts = value.parse::<u32>().map_err(|_| {
                    NativeError::message(format!("Invalid --max-attempts value: {value}"))
                })?;
            }
            "--timeout-ms" => {
                let value = next("--timeout-ms")?;
                let millis = value.parse::<u64>().map_err(|_| {
                    NativeError::message(format!("Invalid --timeout-ms value: {value}"))
                })?;
                options.command_timeout = Duration::from_millis(millis.max(1));
            }
            "--replay" => options.replay = Some(PathBuf::from(next("--replay")?)),
            "--sandbox" => options.sandbox = true,
            "--dry-run" => options.dry_run = true,
            other => {
                return Err(NativeError::message(format!(
                    "Unknown argument: {other}. Usage: kiana-evals run --suite <name> [--trials N] [--sandbox] [--suite-root DIR] [--artifacts DIR] [--task ID] [--max-attempts N] [--timeout-ms N] [--dry-run] [--replay FILE]"
                )));
            }
        }
        index += 1;
    }

    // replay / dry-run 不需要沙箱与模型；只有真正跑任务时才探测沙箱后端。
    let mut runner = if options.replay.is_some() || options.dry_run {
        NativeRunner::with_command_runner(options, Arc::new(NoopRunner))
    } else {
        NativeRunner::with_sandbox(options)?
    };
    let results = runner.run().await?;
    print_summary(&results);
    Ok(if results.summary.all_passed { 0 } else { 1 })
}

/// 打印汇总（CLI 与测试共用）。
pub fn print_summary(results: &NativeSuiteResults) {
    println!(
        "suite {}: {} task(s) × {} trial(s)",
        results.suite, results.summary.tasks, results.summary.trials
    );
    println!(
        "pass@1 {:.1}%  pass@{k} {:.1}%  well-formed {:.1}%  cost ${:.6}{}",
        results.summary.pass_at_1 * 100.0,
        results.summary.pass_at_k * 100.0,
        results.summary.well_formed_percent,
        results.summary.total_cost,
        if results.summary.cost_unknown {
            " (cost_unknown)"
        } else {
            ""
        },
        k = results.summary.k
    );
}

/// 生成结果文件的 JSON 文本（测试/文档用）。
pub fn results_json(results: &NativeSuiteResults) -> Result<String, NativeError> {
    Ok(serde_json::to_string_pretty(results)?)
}

/// replay 入口（CLI 的 `--replay` 路径也走 [`NativeRunner::run`]）。
pub fn replay_results(path: &Path) -> Result<NativeSuiteResults, NativeError> {
    let contents = std::fs::read_to_string(path)?;
    let mut results: NativeSuiteResults = serde_json::from_str(&contents)?;
    results.recompute_summary();
    Ok(results)
}
