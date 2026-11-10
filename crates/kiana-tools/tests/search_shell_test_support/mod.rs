//! shell / search 工具测试的共享上下文。
//!
//! 提供最小 [`ExtensionToolContextApi`]（只覆写 cwd / model / sessionManager /
//! thinkingLevel，其余委托给 [`kiana_tools::definition::missing_tool_context`]）、直接执行
//! [`ToolDefinition`] 的便捷函数，以及把假的 `rg`/`fd` 放进 `KIANA_CODING_AGENT_DIR/bin`
//! 的环境守卫（`ensure_tool` 优先读取该目录）。
//!
//! 环境变量是进程级的：所有会改 env 的测试必须通过 [`with_fake_tool`] 串行化。

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use kiana_agent_core::{
    AgentTool, AgentToolCallOutcome, AgentToolError, AgentToolResult, ThinkingLevel,
};
use kiana_ai::types::{JsonValue, Model, TextContent, TextKind, UserContentBlock};
use kiana_config::models::registry::ModelRegistry;
use kiana_config::models::resolver::ScopedModel;
use kiana_ext_api::BoxFuture;
use kiana_ext_api::tool_types::ToolDefinition;
use kiana_ext_api::types::{
    CompactOptions, ContextUsage, ExecuteToolOptions, ExtensionContextApi, ExtensionMode,
    ExtensionToolContextApi,
};
use kiana_ext_api::ui::ExtensionUiContext;
use kiana_session_core::session_manager::{ReadonlySessionManager, SessionManager};
use tokio_util::sync::CancellationToken;

fn fallback() -> &'static Arc<dyn ExtensionToolContextApi> {
    static MISSING: OnceLock<Arc<dyn ExtensionToolContextApi>> = OnceLock::new();
    MISSING.get_or_init(kiana_tools::definition::missing_tool_context)
}

/// 测试上下文。
pub struct TestContext {
    /// `ctx.cwd`。
    pub cwd: PathBuf,
    /// `ctx.model`。
    pub model: Option<Model>,
    /// `ctx.sessionManager`（`None` 时回落到占位上下文的管理器）。
    pub session_manager: Option<SessionManager>,
    /// `ctx.thinkingLevel`。
    pub thinking_level: Option<ThinkingLevel>,
}

impl TestContext {
    /// 只设置 cwd 的上下文。
    ///
    /// 返回 `Arc<dyn ExtensionToolContextApi>` 而不是 `Self`：工具的 `execute` 需要 trait 对象，
    /// 测试不必再手动 `Arc::new(...) as Arc<dyn ...>`。
    #[allow(clippy::new_ret_no_self)]
    pub fn new(cwd: impl Into<PathBuf>) -> Arc<dyn ExtensionToolContextApi> {
        Arc::new(Self {
            cwd: cwd.into(),
            model: None,
            session_manager: None,
            thinking_level: None,
        })
    }

    /// 带 model 的上下文。
    pub fn with_model(cwd: impl Into<PathBuf>, model: Model) -> Arc<dyn ExtensionToolContextApi> {
        Arc::new(Self {
            cwd: cwd.into(),
            model: Some(model),
            session_manager: None,
            thinking_level: None,
        })
    }

    /// 带会话元数据的上下文（KIANA_* 注入测试）。
    pub fn with_session(
        cwd: impl Into<PathBuf>,
        model: Option<Model>,
        session_manager: SessionManager,
        thinking_level: Option<ThinkingLevel>,
    ) -> Arc<dyn ExtensionToolContextApi> {
        Arc::new(Self {
            cwd: cwd.into(),
            model,
            session_manager: Some(session_manager),
            thinking_level,
        })
    }
}

impl ExtensionContextApi for TestContext {
    fn ui(&self) -> &dyn ExtensionUiContext {
        fallback().ui()
    }

    fn mode(&self) -> ExtensionMode {
        ExtensionMode::Tui
    }

    fn has_ui(&self) -> bool {
        false
    }

    fn cwd(&self) -> &Path {
        &self.cwd
    }

    fn session_manager(&self) -> &dyn ReadonlySessionManager {
        match &self.session_manager {
            Some(manager) => manager,
            None => fallback().session_manager(),
        }
    }

    fn model_registry(&self) -> &ModelRegistry {
        fallback().model_registry()
    }

    fn model(&self) -> Option<Model> {
        self.model.clone()
    }

    fn scoped_models(&self) -> Vec<ScopedModel> {
        Vec::new()
    }

    fn thinking_level(&self) -> Option<ThinkingLevel> {
        self.thinking_level
    }

    fn is_idle(&self) -> bool {
        true
    }

    fn is_project_trusted(&self) -> bool {
        false
    }

    fn signal(&self) -> Option<CancellationToken> {
        None
    }

    fn abort(&self) {}

    fn has_pending_messages(&self) -> bool {
        false
    }

    fn shutdown(&self) {}

    fn get_context_usage(&self) -> Option<ContextUsage> {
        None
    }

    fn compact(&self, _options: Option<CompactOptions>) {}

    fn get_system_prompt(&self) -> String {
        String::new()
    }
}

impl ExtensionToolContextApi for TestContext {
    fn tools(&self) -> Vec<AgentTool> {
        Vec::new()
    }

    fn execute_tool<'a>(
        &'a self,
        name: &'a str,
        args: JsonValue,
        options: Option<ExecuteToolOptions>,
    ) -> BoxFuture<'a, AgentToolCallOutcome> {
        fallback().execute_tool(name, args, options)
    }
}

/// 执行一次工具调用。
pub async fn execute(
    definition: &ToolDefinition,
    params: JsonValue,
    ctx: &Arc<dyn ExtensionToolContextApi>,
) -> Result<AgentToolResult<JsonValue>, AgentToolError> {
    definition
        .execute
        .execute("test-call", params, None, None, Arc::clone(ctx))
        .await
}

/// 带中止信号执行一次工具调用。
pub async fn execute_with_signal(
    definition: &ToolDefinition,
    params: JsonValue,
    signal: &CancellationToken,
    ctx: &Arc<dyn ExtensionToolContextApi>,
) -> Result<AgentToolResult<JsonValue>, AgentToolError> {
    definition
        .execute
        .execute("test-call", params, Some(signal), None, Arc::clone(ctx))
        .await
}

/// 带 `onUpdate` 执行一次工具调用。
pub async fn execute_with_update(
    definition: &ToolDefinition,
    params: JsonValue,
    on_update: Option<kiana_agent_core::AgentToolUpdateCallback>,
    ctx: &Arc<dyn ExtensionToolContextApi>,
) -> Result<AgentToolResult<JsonValue>, AgentToolError> {
    definition
        .execute
        .execute("test-call", params, None, on_update, Arc::clone(ctx))
        .await
}

/// 结果里的文本块（等价 TS 的 `getTextOutput`）。
pub fn text_output(result: &AgentToolResult<JsonValue>) -> String {
    result
        .content
        .iter()
        .filter_map(|block| match block {
            UserContentBlock::Text(text) => Some(text.text.clone()),
            UserContentBlock::Image(_) => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 一个 text 内容块（构造 renderer 测试的结果）。
pub fn text_block(text: &str) -> UserContentBlock {
    UserContentBlock::Text(TextContent {
        kind_text: TextKind::Text,
        text: text.to_string(),
        text_signature: None,
    })
}

/// 带 provider/id 的测试模型。
pub fn test_model(provider: &str, id: &str) -> Model {
    Model {
        id: id.to_string(),
        provider: provider.to_string(),
        ..Model::default()
    }
}

/// 读取 TS 参考生成的黄金文件。
pub fn golden() -> JsonValue {
    serde_json::from_str(include_str!(
        "../fixtures/search-shell/behavior.golden.json"
    ))
    .expect("valid golden fixture")
}

/// 环境变量守卫（进程级；调用方必须持有 [`ENV_LOCK`]）。
struct EnvGuard {
    saved: Vec<(&'static str, Option<std::ffi::OsString>)>,
}

impl EnvGuard {
    fn set(key: &'static str, value: Option<&std::ffi::OsStr>) -> Self {
        let previous = std::env::var_os(key);
        // SAFETY: 调用方持有 `ENV_LOCK`，本测试二进制内对这些变量的读写已串行化。
        unsafe {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
        Self {
            saved: vec![(key, previous)],
        }
    }

    fn push(&mut self, key: &'static str, value: &str) {
        let previous = std::env::var_os(key);
        // SAFETY: 同上（调用方持有 `ENV_LOCK`）。
        unsafe { std::env::set_var(key, value) };
        self.saved.push((key, previous));
    }

    /// 把 `dir` 前置到 `PATH`。
    ///
    /// `kiana_core::tools_manager` 的 `TOOLS_DIR` 是首次使用时捕获的 `LazyLock`（与 TS 的模块加载
    /// 期捕获一致），后续测试换 `KIANA_CODING_AGENT_DIR` 不会换 bin 目录；因此假工具必须同时
    /// 出现在 `PATH` 上，让 `commandExists()` 回退能找到它。
    fn prepend_path(&mut self, dir: &Path) {
        let previous = std::env::var_os("PATH");
        let mut value = dir.as_os_str().to_os_string();
        value.push(if cfg!(windows) { ";" } else { ":" });
        if let Some(previous) = &previous {
            value.push(previous);
        }
        // SAFETY: 同上（调用方持有 `ENV_LOCK`）。
        unsafe { std::env::set_var("PATH", value) };
        self.saved.push(("PATH", previous));
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, previous) in self.saved.drain(..) {
            // SAFETY: 同上。
            unsafe {
                match &previous {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }
}

/// 环境变量互斥量：所有伪造 `rg`/`fd` 的测试都通过 [`with_fake_tool`] 串行执行。
pub static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// 把名为 `tool` 的假可执行脚本放进新的 `KIANA_CODING_AGENT_DIR/bin`，设置 `KIANA_OFFLINE=1`
/// 与额外环境变量后运行 `run`，结束后恢复原环境。
pub async fn with_fake_tool<F, Fut, T>(
    tool: &str,
    script: &str,
    extra_env: &[(&'static str, String)],
    run: F,
) -> T
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = T>,
{
    let _guard = ENV_LOCK.lock().await;
    let dir = tempfile::TempDir::new().expect("temp agent dir");
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).expect("bin dir");
    let path = bin.join(tool);
    std::fs::write(&path, script).expect("write fake tool");
    set_executable(&path);

    let mut env = EnvGuard::set("KIANA_CODING_AGENT_DIR", Some(dir.path().as_os_str()));
    env.prepend_path(&bin);
    env.push("KIANA_OFFLINE", "1");
    for (key, value) in extra_env {
        env.push(key, value);
    }
    run().await
}

/// 给脚本加可执行位（Unix；其它平台依赖扩展名解析）。
fn set_executable(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(path).expect("metadata").permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions).expect("chmod");
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}
