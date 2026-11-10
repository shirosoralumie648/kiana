//! `main.ts`（1001 行）：启动编排、会话选择、模式分发、迁移调用。
//!
//! TS 源：`packages/coding-agent/src/main.ts`。规范：`spec/ca-cli.md` §2.4。
//!
//! # 退出策略
//!
//! TS 的 `main()` 在多处直接 `process.exit(code)` 或设置 `process.exitCode`。Rust 侧把
//! 这些路径统一成 [`CliExit`] 返回值：内部辅助函数返回 `Result<_, CliExit>`，
//! [`main`] 把结果折算成 `i32` 退出码；只有两个二进制入口（`src/main.rs` 与
//! `src/bin/kiana-rpc.rs`）真正调用 `std::process::exit`。这样 `create_session_manager`
//! 等辅助函数可以在测试进程内直接调用并断言退出码。
//!
//! # 输出纪律
//!
//! 所有用户可见字节都经 [`Output`]（`console.log` → [`Output::write_line_stdout`]，
//! `console.error` → [`Output::write_line_stderr`]）。stdout 被接管后普通输出会自动转写
//! stderr，与 TS 替换 `process.stdout.write` 的行为一致。
//!
//! # 一次启动的执行流程
//!
//! [`main`] 保留 TS `main()` 的线性流程，`run_main` 内的编号注释对应
//! `spec/ca-cli.md` §2.4.7 的步骤。整体可以分成五段：
//!
//! 1. **早期短路**：`auth` 子命令、`package`/`config` 命令、`mcp` 子命令都在解析普通
//!    参数之前直接返回，因此这些路径不会创建会话，也不会被后续参数校验影响。
//! 2. **参数与模式**：`parse_args` 的诊断先打印（存在 error 即退出 1），随后处理
//!    `--version` / `--export`，再按 `--mode`、`--print` 以及 stdin/stdout 是否为 TTY
//!    决定 `AppMode`（交互 / 打印 / JSON / RPC）。
//! 3. **启动准备**：迁移、启动设置与诊断、首次设置、主题覆盖、sessionDir 解析、会话
//!    选择（[`create_session_manager`]）。
//! 4. **运行期构建**：`create_runtime` 工厂负责项目信任、资源加载、model scope 与会话
//!    参数；它由 `create_agent_session_runtime` 调用，并会在后续会话替换
//!    （`switch_session` / `new_session` / `fork` / `import_from_jsonl`）时被再次调用。
//! 5. **分发**：`--help` / `--list-models` 先返回；其余读取管道 stdin、初始化主题，最后
//!    进入 RPC / Interactive / Print|Json 之一。
//!
//! # 设计原因与约束
//!
//! - **退出码是返回值而不是 `process::exit`**：`run_main` 统一用 [`CliResult`] 传播
//!   [`CliExit`]，这样 `create_session_manager` 等辅助函数能在测试进程内调用并断言退出码
//!   （见 `tests/session_id_readonly.rs`）；本文件自身不调用 `std::process::exit`，进程
//!   终止由两个二进制入口负责。
//! - **运行期上下文放进 `create_runtime` 闭包**：该工厂在初次创建与会话替换时都会被调用，
//!   因此每次调用都按值 clone `Args`/`Output`/路径列表，而信任缓存、trust store 与启动
//!   设置管理器通过 `Arc`/内部共享在多次调用之间保持一致（见 `CreateRuntimeCaptures`）。
//! - **`Output` 贯穿全程**：所有用户可见文本走同一通道，print/RPC 接管 stdout 后普通文本
//!   自动改道 stderr，协议字节独占 fd 1。
//!
//! # 注意事项
//!
//! - offline 模式会写进程环境变量（`KIANA_OFFLINE`、`KIANA_SKIP_VERSION_CHECK`）。这是
//!   `std::env::set_var` 的 `unsafe` 调用，调用方需保证没有其它线程并发读写环境。
//! - `main` 必须在单线程 `tokio::task::LocalSet` 上运行：会话与扩展运行时是 `!Send` 的，
//!   两个二进制入口都如此。工厂闭包上的 `Arc` 只是冻结的工厂类型要求，不代表可跨线程。
//! - 本文件只做编排；会话、模型、资源加载的语义分别由 `kiana-session`、
//!   `kiana-config`、`kiana-resources` 实现。

use std::collections::HashMap;
use std::io::{IsTerminal, Read};
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use kiana_agent_core::budget::BudgetConfig;
use kiana_agent_core::loop_guard::{LoopGuard, LoopGuardConfig};
use std::time::Duration;

use kiana_ai::auth::types::AuthOperationOptions;
use kiana_ai::models::ModelsRefreshOptions;
use kiana_ai::types::{Model, ModelType};
use kiana_ai::utils::abort::AbortScope;
use kiana_config::auth::storage::{AuthStorage, ReadOnlyAuthStorage};
use kiana_config::auth_guidance::format_no_models_available_message;
use kiana_config::models::resolver::{
    ResolveCliModelOptions, resolve_cli_model, resolve_model_scope_with_diagnostics,
};
use kiana_config::models::runtime::{CreateModelRuntimeOptions, ModelRuntime};
use kiana_config::settings::diagnostics::{
    DiagnosticKind, collect_settings_diagnostics, deduplicate_diagnostics,
};
use kiana_config::settings::{Settings, SettingsManager, SettingsManagerCreateOptions};
use kiana_config::trust::manager::{ProjectTrustStore, has_trust_requiring_project_resources};
use kiana_config::trust::project_trust::{
    AppMode, ProjectTrustContext as ConfigProjectTrustContext, ResolveProjectTrustedOptions,
    resolve_project_trusted,
};
use kiana_core::config::{APP_NAME, ENV_SESSION_DIR, VERSION, get_agent_dir, get_package_dir};
use kiana_core::http_dispatcher::{
    DEFAULT_HTTP_IDLE_TIMEOUT_MS, apply_http_proxy_settings, configure_http_dispatcher,
};
use kiana_core::output_guard;
use kiana_core::timings::{TimingLabel, print_timings, reset_timings, time};
use kiana_ext_api::types::{ExtensionFlag, InlineExtension};
use kiana_interactive::{InteractiveMode, InteractiveModeOptions};
use kiana_permissions::{
    PermissionEngine, PermissionFlags, PermissionMode, RuleDecision, parse_cli_rule,
};
use kiana_resources::loader::{DefaultResourceLoaderOptions, ResourceLoaderReloadOptions};
use kiana_rpc_print::{
    PrintModeOptions, PrintOutputMode, global_raw_stdout, run_print_mode, run_rpc_mode,
};
use kiana_sandbox::{SandboxManager, SandboxPolicy};
use kiana_session::agent_session::ScopedModel as SessionScopedModel;
use kiana_session::agent_session_runtime::{
    AgentSessionRuntimeError, CreateAgentSessionRuntimeFactory,
    CreateAgentSessionRuntimeFactoryOptions, CreateAgentSessionRuntimeResult,
    create_agent_session_runtime,
};
use kiana_session::agent_session_services::{
    AgentSessionRuntimeDiagnostic, CreateAgentSessionFromServicesOptions,
    CreateAgentSessionServicesOptions, create_agent_session_from_services,
    create_agent_session_services,
};
use kiana_session::sdk::NoTools;
use kiana_session_core::session_cwd::{
    MissingSessionCwdError, format_missing_session_cwd_prompt, get_missing_session_cwd_issue,
};
use kiana_session_core::session_manager::{NewSessionOptions, SessionManager};
use kiana_theme::{init_theme, set_theme_json_validator, stop_theme_watcher, validate_theme_json};
use kiana_tui::set_capability_overrides;

use crate::cron::{
    SessionJobDispatcher, build_cron_scheduler, cron_turn_channel, run_cron_loop, run_cron_worker,
};

use crate::cli::args::{
    Args, DiagnosticKind as ArgsDiagnosticKind, ListModelsArg, Mode, normalize_session_name,
    parse_args, print_help,
};
use crate::cli::auth_check::{
    AuthCheckOptions, AuthCheckReason, AuthCheckResult, AuthCheckStatus, check_provider_auth,
    create_auth_check_model_runtime, get_provider_credential,
};
use crate::cli::auth_command::{
    AuthCommand, AuthCommandError, AuthCommandKind, get_auth_command_name, get_auth_command_usage,
    is_auth_command_help, parse_auth_command, print_auth_command_help, validate_auth_command_args,
};
use crate::cli::credential_print::resolve_credential_for_print;
use crate::cli::file_processor::{ProcessFileOptions, process_file_arguments};
use crate::cli::initial_message::{InitialMessageInput, build_initial_message};
use crate::cli::list_models::list_models;
use crate::cli::project_trust::{
    ProjectTrustAdapter, ProjectTrustEmitterAdapter, ProjectTrustOptions,
    create_project_trust_context_views,
};
use crate::cli::session_picker::select_session;
use crate::cli::setup::{CliExit, CliResult, Output, Style};
use crate::cli::startup_ui::{
    StartupSelectorOption, should_run_first_time_setup_default, show_first_time_setup,
    show_startup_selector,
};
use crate::migrations::{run_migrations, show_deprecation_warnings};
use crate::package_manager_cli::{
    PackageCommandRuntimeOptions, cleanup_managed_install, handle_config_command,
    handle_package_command,
};

/// `EXTENSION_LOAD_FAILURE_HINT`。
const EXTENSION_LOAD_FAILURE_HINT: &str = "Hint: Start without extensions using \"kiana -ne\".";

/// [`main`] 的选项（TS `MainOptions`）。
///
/// `None` 与 `Some(MainOptions::default())` 等价：两者都只使用内置扩展。注入的工厂按
/// `extension_factories` 的顺序追加在内置扩展之后，并会被复制进每次 runtime 创建流程，
/// 因此同一个工厂可能在初次创建和后续会话替换中被调用多次。
#[derive(Default)]
pub struct MainOptions {
    /// 外部注入的扩展工厂（追加在内置扩展之后）。
    ///
    /// 列表会在每个 runtime 创建流程中 clone 一份；工厂自身需要能安全地重复调用。
    pub extension_factories: Vec<InlineExtension>,
}

/// `main(args, options)`：启动编排。
///
/// 返回值是进程退出码（TS 的 `process.exitCode` 或 `process.exit` 的等价物）。
///
/// # 参数与返回值
///
/// `args` 不含程序名（对应 TS 的 `process.argv.slice(2)`）；`options` 只能追加扩展工厂。
/// 返回 0/1/2 等退出码，具体值由子命令与运行模式决定。本函数**不**调用
/// `std::process::exit`，也不返回 `Result`：错误路径由本文件或辅助函数打印文案后折算成
/// 非零退出码（例如失败的子命令、参数校验、会话打开错误都打印 `Error: ...`）。
///
/// # 副作用
///
/// - offline 模式（`--offline` 或真值 `KIANA_OFFLINE`）下写进程环境变量
///   `KIANA_OFFLINE=1`、`KIANA_SKIP_VERSION_CHECK=1`；
/// - 非交互模式会接管 stdout（普通文本改道 stderr）；Print/Json 的正常分发分支在返回前
///   恢复，`--help`/`--list-models` 等提前返回的路径不会恢复接管状态；
/// - 会启动 tokio 任务（多处 15 秒超时、RPC 的 catalog 后台刷新），因此必须运行在
///   tokio 运行时上。
///
/// # 调用约束
///
/// 会话与扩展运行时是 `!Send` 的，两个二进制入口都在单线程
/// `tokio::task::LocalSet` 上调用本函数；嵌入方必须保持同样的线程模型。二进制入口在
/// 退出前还会 `await` 一次 `Output::process().flush()`；若嵌入方自行结束进程，应保持
/// 同样的冲刷纪律（见 `kiana_core::output_guard` 的模块文档：raw stdout 队列不冲刷
/// 就可能丢失未写出的字节）。
pub async fn main(args: Vec<String>, options: Option<MainOptions>) -> i32 {
    match run_main(args, options).await {
        Ok(code) => code,
        Err(exit) => exit.code,
    }
}

async fn run_main(args: Vec<String>, options: Option<MainOptions>) -> CliResult<i32> {
    let output = Output::process();

    // 下面的编号与 `spec/ca-cli.md` §2.4.7 的步骤一一对应。前半段（1-16）处理无需会话
    // 即可返回的子命令与参数错误，后半段进入会话选择、runtime 构建与模式分发。任何一步
    // 需要非零退出时都返回 `Err(CliExit)` 或 `Ok(code)`，由 [`main`] 折算成 i32。

    // `main` 可以在同一进程内多次调用（SDK/测试）；每次启动重置 `promptConfirm` 的状态。
    PROMPT_CONSUMED_STDIN.store(false, Ordering::SeqCst);

    // 1. `resetTimings()` / `setThemeJsonValidator(validateThemeJson)`。
    reset_timings(TimingLabel::Main);
    set_theme_json_validator(Box::new(validate_theme_json));

    // 2. `extensionFactories = [...builtInExtensions, ...options?.extensionFactories]`。
    let mut extension_factories = kiana_builtin_extensions::builtin_extensions();
    // 第 12 周：WebSearch/WebFetch 工具（默认不激活；--tools web_search,web_fetch 启用）。
    extension_factories.push(crate::web::web_tools_extension());
    if let Some(options) = options {
        extension_factories.extend(options.extension_factories);
    }

    // 3. 离线模式：原始 argv 的 `--offline` 或真值环境变量。
    let offline_mode = args.iter().any(|arg| arg == "--offline")
        || is_truthy_env_flag(std::env::var("KIANA_OFFLINE").ok().as_deref());
    if offline_mode {
        // SAFETY: 单线程启动阶段（二进制入口在创建运行时之后才可能有其它线程；这里是
        // `main` 的开头）。Rust 2024 要求 `set_var` 显式 unsafe。
        unsafe {
            std::env::set_var("KIANA_OFFLINE", "1");
            std::env::set_var("KIANA_SKIP_VERSION_CHECK", "1");
        }
    }

    // 4. auth 子命令短路。
    if let Some(code) = run_auth_command(&args, &output).await? {
        return Ok(code);
    }

    // 5/6. Windows 自更新隔离清理 + managed 安装清理。
    if cfg!(target_os = "windows") {
        kiana_core::changelog::cleanup_windows_self_update_quarantine(&get_package_dir());
    }
    cleanup_managed_install();

    // 7. cwd / agentDir / bootstrap settings。
    //
    // 此时还无法判断项目是否受信，因此 `project_trusted: Some(false)` 让这个实例只读取
    // 全局设置（未受信时项目作用域按空设置处理）。本文件只把它用于读取全局 `httpProxy`；
    // 真正影响扩展与资源加载的 SettingsManager 会在 `create_runtime_factory` 里按实际
    // cwd 与信任结果重建。
    let cwd = process_cwd();
    let agent_dir = get_agent_dir().to_string_lossy().into_owned();
    let bootstrap_settings_manager = SettingsManager::create_with_options(
        &cwd,
        &agent_dir,
        SettingsManagerCreateOptions {
            project_trusted: Some(false),
        },
    );
    apply_http_proxy_settings(
        bootstrap_settings_manager
            .get_global_settings()
            .http_proxy
            .as_deref(),
    );
    let _ = configure_http_dispatcher(DEFAULT_HTTP_IDLE_TIMEOUT_MS);

    // 8. package 命令。
    let package_outcome = handle_package_command(
        &args,
        PackageCommandRuntimeOptions {
            cwd: cwd.clone(),
            agent_dir: agent_dir.clone(),
            extension_factories: clone_inline_extensions(&extension_factories),
            output: output.clone(),
        },
    )
    .await;
    if package_outcome.handled {
        // TS 在 win32 上让成功的 `kiana update` 自然 drain；Rust 侧同样只是返回 0。
        return Ok(package_outcome.exit_code);
    }

    // 9. config 命令。
    let config_outcome = handle_config_command(
        &args,
        PackageCommandRuntimeOptions {
            cwd: cwd.clone(),
            agent_dir: agent_dir.clone(),
            extension_factories: clone_inline_extensions(&extension_factories),
            output: output.clone(),
        },
    )
    .await;
    if config_outcome.handled {
        return Ok(config_outcome.exit_code);
    }

    // 10. `kiana mcp ...`。
    if args.first().map(String::as_str) == Some("mcp") {
        let log_output = output.clone();
        let error_output = output.clone();
        let options = kiana_builtin_extensions::McpCommandOptions {
            log: Some(Arc::new(move |message: &str| {
                log_output.write_line_stdout(message);
            })),
            error: Some(Arc::new(move |message: &str| {
                error_output.write_line_stderr(message);
            })),
            ..kiana_builtin_extensions::McpCommandOptions::new(&cwd, &agent_dir)
        };
        let code = kiana_builtin_extensions::run_mcp_command(args[1..].to_vec(), options).await;
        return Ok(code);
    }

    // 11. 参数解析与诊断。
    let mut parsed = parse_args(&args);
    if !parsed.diagnostics.is_empty() {
        let style = Style::detect();
        for diagnostic in &parsed.diagnostics {
            let formatted = format!(
                "{}: {}",
                if diagnostic.kind == ArgsDiagnosticKind::Error {
                    "Error"
                } else {
                    "Warning"
                },
                diagnostic.message
            );
            output.write_line_stderr(&match diagnostic.kind {
                ArgsDiagnosticKind::Error => style.red(&formatted),
                ArgsDiagnosticKind::Warning => style.yellow(&formatted),
            });
        }
        if parsed
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.kind == ArgsDiagnosticKind::Error)
        {
            return Ok(1);
        }
    }
    time("parseArgs", TimingLabel::Main);

    // 12. `--version`。
    if parsed.version {
        output.write_line_stdout(VERSION);
        return Ok(0);
    }

    // 13. `--export`。
    if let Some(export_path) = js_truthy(parsed.export.as_deref()).map(str::to_string) {
        let output_path = parsed.messages.first().cloned();
        match kiana_export::export_from_file(
            &export_path,
            Some(kiana_export::ExportOptions {
                output_path,
                ..Default::default()
            }),
        ) {
            Ok(result) => {
                output.write_line_stdout(&format!("Exported to: {result}"));
                return Ok(0);
            }
            Err(error) => {
                output.write_line_stderr(&Style::detect().red(&format!("Error: {error}")));
                return Ok(1);
            }
        }
    }

    // 14. 模式判定与 stdout 接管。
    //
    // `--help`/`--list-models`（且未显式指定 mode、未加 `--print`）保留真实 stdout：
    // 它们输出的是用户主动请求的元数据。其余非交互模式接管 stdout，普通文本被改道
    // stderr，print/RPC 的协议字节因此独占 fd 1。
    let mut app_mode = resolve_app_mode(
        &parsed,
        std::io::stdin().is_terminal(),
        std::io::stdout().is_terminal(),
    );
    let should_take_over_stdout =
        app_mode != AppMode::Interactive && !is_plain_runtime_metadata_command(&parsed);
    if should_take_over_stdout {
        output_guard::take_over_stdout();
    }

    // 15. RPC 模式拒绝 `@file`。
    if parsed.mode == Some(Mode::Rpc) && !parsed.file_args.is_empty() {
        output.write_line_stderr(
            &Style::detect().red("Error: @file arguments are not supported in RPC mode"),
        );
        return Ok(1);
    }

    // 16. flag 冲突校验。
    validate_fork_flags(&parsed, &output)?;
    validate_session_id_flags(&parsed, &output)?;

    // 17. 迁移。
    //
    // 迁移在设置与会话创建之前运行，可能重命名或改写旧文件。返回的两部分结果分别在
    // 后面使用：`migrated_auth_providers` 交给 InteractiveMode 做启动告警，
    // `deprecation_warnings` 由本函数在交互模式启动前展示。
    let migrations = run_migrations(Path::new(&cwd));
    time("runMigrations", TimingLabel::Main);

    // 18. 启动设置与诊断。
    //
    // 这里用默认构造（默认信任项目）创建启动期实例：它服务于首次设置、
    // 主题、sessionDir 与设置诊断。会话运行期会按项目信任结果重建另一个实例。
    let startup_settings_manager = SettingsManager::create(&cwd, &agent_dir);
    let startup_settings_diagnostics =
        settings_diagnostics_to_runtime(collect_settings_diagnostics(&startup_settings_manager));

    // 19. 首次设置。
    if app_mode == AppMode::Interactive
        && !parsed.help
        && parsed.list_models.is_none()
        && should_run_first_time_setup_default()
    {
        show_first_time_setup(&startup_settings_manager).await;
        time("firstTimeSetup", TimingLabel::Main);
    }

    // 20. `--use-theme` 覆盖。
    if app_mode == AppMode::Interactive
        && let Some(theme) = parsed.use_theme.clone()
    {
        startup_settings_manager.apply_overrides(Settings {
            theme: Some(theme),
            ..Default::default()
        });
    }

    // 21. sessionDir 优先级：`--session-dir` > 环境变量 > 设置。
    //
    // TS: `const envSessionDir = process.env[ENV_SESSION_DIR]; ... (envSessionDir ? ... : undefined)`
    // ——导出的空串是假值，必须按“未设置”处理并继续回退到 `getSessionDir()`；
    // 否则 `Some("")` 会短路后续回退，并让 listAll 走 `listAllIn("")`。
    let env_session_dir = std::env::var(ENV_SESSION_DIR)
        .ok()
        .filter(|dir| !dir.is_empty());
    let session_dir = parsed
        .session_dir
        .as_deref()
        .filter(|dir| !dir.is_empty())
        .map(|dir| kiana_core::paths::normalize_path(dir, &Default::default()))
        .or_else(|| {
            env_session_dir
                .as_deref()
                .map(kiana_core::config::expand_tilde_path)
        })
        .or_else(|| startup_settings_manager.get_session_dir());

    // 22. 会话选择。
    let mut session_manager = create_session_manager(
        &parsed,
        &cwd,
        session_dir.as_deref(),
        &startup_settings_manager,
        &output,
    )
    .await?;

    // 23. stored cwd 缺失。
    if let Some(issue) = get_missing_session_cwd_issue(&session_manager, &cwd) {
        if app_mode == AppMode::Interactive {
            let selected_cwd =
                prompt_for_missing_session_cwd(&issue, &startup_settings_manager).await;
            let Some(selected_cwd) = selected_cwd else {
                return Ok(0);
            };
            let session_file = issue.session_file.clone().unwrap_or_default();
            session_manager =
                SessionManager::open(&session_file, session_dir.as_deref(), Some(&selected_cwd))
                    .map_err(|error| report_session_error(&output, &error))?;
        } else {
            output.write_line_stderr(
                &Style::detect().red(&MissingSessionCwdError { issue }.to_string()),
            );
            return Ok(1);
        }
    }

    // 24. `--name`。
    if let Some(raw_name) = parsed.name.clone() {
        let Some(name) = normalize_session_name(&raw_name) else {
            output.write_line_stderr(
                &Style::detect().red("Error: --name requires a non-empty value"),
            );
            return Ok(1);
        };
        if let Err(error) = session_manager.append_session_info(&name) {
            return Err(report_session_error(&output, &error));
        }
    }
    time("createSessionManager", TimingLabel::Main);

    // 26. 信任存储与运行期状态。
    //
    // `auto_trust_on_reload_cwd` 是交给 InteractiveMode 的“reload 后可自动持久化信任”的
    // 候选 cwd：只有用户没有显式覆盖、且当前项目没有需要信任的资源时才给值；为 None 时
    // reload 后不会自动写入信任（是否询问由交互模式自身的信任流程决定）。
    // `trust_prompt_mode` 把 help/list-models 压成 Print：这两条路径没有 TUI，信任解析
    // 阶段因此不会弹选择器（`has_ui` 随之为 false）。
    let trust_store = ProjectTrustStore::new(&agent_dir);
    let session_cwd = session_manager.get_cwd().to_string();
    let auto_trust_on_reload_cwd = (parsed.project_trust_override.is_none()
        && !has_trust_requiring_project_resources(&session_cwd))
    .then(|| session_cwd.clone());
    let trust_prompt_mode = if parsed.help || parsed.list_models.is_some() {
        AppMode::Print
    } else {
        app_mode
    };
    let project_trust_by_cwd: Arc<Mutex<HashMap<String, bool>>> =
        Arc::new(Mutex::new(HashMap::new()));

    // 27. CLI 资源路径。
    let resolved_extension_paths = resolve_cli_paths(&cwd, parsed.extensions.as_deref());
    let resolved_skill_paths = resolve_cli_paths(&cwd, parsed.skills.as_deref());
    let resolved_prompt_template_paths =
        resolve_cli_paths(&cwd, parsed.prompt_templates.as_deref());
    let resolved_theme_paths = resolve_cli_paths(&cwd, parsed.themes.as_deref());

    // 28. `createRuntime` 工厂。
    let create_runtime: CreateAgentSessionRuntimeFactory = {
        let parsed = parsed.clone();
        let project_trust_by_cwd = Arc::clone(&project_trust_by_cwd);
        let trust_store = trust_store.clone();
        let startup_settings_manager = startup_settings_manager.clone();
        let extension_factories = clone_inline_extensions(&extension_factories);
        let output = output.clone();
        // 工厂闭包捕获扩展工厂（`Arc<dyn Fn(...) -> LocalBoxFuture>`，没有 Send/Sync 超trait）；
        // 与 TS 一样只在同一个 LocalSet 线程上调用，`Arc` 只是 `CreateAgentSessionRuntimeFactory`
        // 的冻结形状（同 kiana-resources DefaultResourceLoader 的 allow）。
        #[allow(
            clippy::arc_with_non_send_sync,
            reason = "factory runs exclusively on the single-threaded LocalSet"
        )]
        Arc::new(
            move |factory_options: CreateAgentSessionRuntimeFactoryOptions| {
                let parsed = parsed.clone();
                let project_trust_by_cwd = Arc::clone(&project_trust_by_cwd);
                let trust_store = trust_store.clone();
                let startup_settings_manager = startup_settings_manager.clone();
                let extension_factories = clone_inline_extensions(&extension_factories);
                let output = output.clone();
                let resolved_extension_paths = resolved_extension_paths.clone();
                let resolved_skill_paths = resolved_skill_paths.clone();
                let resolved_prompt_template_paths = resolved_prompt_template_paths.clone();
                let resolved_theme_paths = resolved_theme_paths.clone();
                Box::pin(async move {
                    create_runtime_factory(
                        factory_options,
                        CreateRuntimeCaptures {
                            parsed,
                            project_trust_by_cwd,
                            trust_store,
                            startup_settings_manager,
                            extension_factories,
                            output,
                            resolved_extension_paths,
                            resolved_skill_paths,
                            resolved_prompt_template_paths,
                            resolved_theme_paths,
                            app_mode,
                            trust_prompt_mode,
                        },
                    )
                    .await
                })
            },
        )
    };
    time("createRuntime", TimingLabel::Main);

    // 工厂内部的错误（信任存储读写、资源加载、model scope 解析等）会以
    // `AgentSessionRuntimeError` 回到这里：打印 `Error: ...` 并以 1 退出，等价 TS 中
    // `main()` 的 promise 被拒绝、Node 按未捕获异常退出 1。
    let runtime = create_agent_session_runtime(
        create_runtime,
        kiana_session::agent_session_runtime::CreateAgentSessionRuntimeOptions {
            cwd: session_manager.get_cwd().to_string(),
            agent_dir: agent_dir.clone(),
            session_manager,
            session_start_event: None,
        },
    )
    .await
    .map_err(|error| {
        output.write_line_stderr(&Style::detect().red(&format!("Error: {error}")));
        CliExit::new(1)
    })?;
    time("createAgentSessionRuntime", TimingLabel::Main);

    // 29. 运行期服务与 HTTP 配置。
    let services = runtime.services();
    let session = runtime.session();
    // 命令行权限覆盖由 `create_runtime_factory` 在**每次**创建会话时应用
    // （这里拿到的 session 已经带上了 CLI 覆盖）。
    let model_fallback_message = runtime.model_fallback_message();
    let settings_manager = services.settings_manager.clone();
    let model_runtime = services.model_runtime.clone();
    let resource_loader = Arc::clone(&services.resource_loader);
    set_capability_overrides(settings_manager.get_terminal_capability_overrides());
    apply_http_proxy_settings(settings_manager.get_global_settings().http_proxy.as_deref());
    // TS 的 `configureHttpDispatcher(settingsManager.getHttpIdleTimeoutMs())` 不在 try/catch
    // 内：非法设置值会抛出 `Invalid httpIdleTimeoutMs setting: ...`，`main()` 的 promise
    // 拒绝，Node 以未捕获异常退出 1（不打印 help）。Rust 侧同样中止启动。
    let http_idle_timeout_ms = match settings_manager.get_http_idle_timeout_ms() {
        Ok(timeout_ms) => timeout_ms,
        Err(error) => {
            output.write_line_stderr(&Style::detect().red(&format!("Error: {error}")));
            return Err(CliExit::new(1));
        }
    };
    let _ = configure_http_dispatcher(http_idle_timeout_ms as i64);

    // 30. 合并诊断。
    //
    // 顺序固定为“启动设置诊断 → runtime 诊断”，再去重。注意 `has_runtime_errors` 只看
    // runtime 的原始诊断（不含设置诊断）：设置错误会随 `startup_diagnostics` 展示，但
    // 不会在这里终止启动，这与 TS 的 `runtime.diagnostics.some(...)` 一致。
    let runtime_diagnostics = runtime.diagnostics();
    let mut combined_diagnostics = startup_settings_diagnostics;
    combined_diagnostics.extend(runtime_diagnostics.iter().cloned());
    let startup_diagnostics = deduplicate_diagnostics(&combined_diagnostics);
    let has_runtime_errors = runtime_diagnostics
        .iter()
        .any(|diagnostic| diagnostic.kind == DiagnosticKind::Error);

    // 31. `--help`。
    if parsed.help {
        report_diagnostics(&startup_diagnostics, &output);
        let extension_flags: Vec<ExtensionFlag> = resource_loader
            .get_extensions()
            .extensions
            .iter()
            .flat_map(|extension| extension.flags.values().cloned())
            .collect();
        print_help(Some(&extension_flags));
        return Ok(0);
    }

    // 32. `--list-models`。
    if let Some(list_models_arg) = parsed.list_models.clone() {
        report_diagnostics(&startup_diagnostics, &output);
        let search_pattern = match &list_models_arg {
            ListModelsArg::Search(pattern) => Some(pattern.as_str()),
            ListModelsArg::True => None,
        };
        if let Err(error) = list_models(
            &model_runtime,
            search_pattern,
            Some(abort_scope_with_timeout(15_000)),
        )
        .await
        {
            output.write_line_stderr(&Style::detect().red(&format!("Error: {error}")));
            return Ok(1);
        }
        return Ok(0);
    }

    // 33. 读取管道 stdin（RPC 除外）。
    let mut stdin_content = None;
    if app_mode != AppMode::Rpc {
        match read_piped_stdin().await {
            // TS: `promptConfirm` 的 `rl.close()` 已经关闭 stdin；之后调用 `readPipedStdin`
            // 时 `end` 不会再触发，promise 永不 resolve，Node 事件循环耗尽后静默退出 0。
            PipedStdin::ClosedByPrompt => return Ok(0),
            PipedStdin::Content(content) => {
                stdin_content = content;
                if stdin_content.is_some() && app_mode == AppMode::Interactive {
                    app_mode = AppMode::Print;
                }
            }
        }
    }
    time("readPipedStdin", TimingLabel::Main);

    // 34. 初始消息。
    let initial = prepare_initial_message(&mut parsed, stdin_content).await?;
    time("prepareInitialMessage", TimingLabel::Main);

    // 35. 主题。
    init_theme(
        settings_manager.get_theme().as_deref(),
        app_mode == AppMode::Interactive,
    );
    time("initTheme", TimingLabel::Main);

    // 36. 交互模式弃用警告。
    if app_mode == AppMode::Interactive && !migrations.deprecation_warnings.is_empty() {
        show_deprecation_warnings(&migrations.deprecation_warnings).await;
    }

    // 37/38. 报告诊断。
    time("resolveModelScope", TimingLabel::Main);
    if app_mode != AppMode::Interactive || has_runtime_errors {
        report_diagnostics(&startup_diagnostics, &output);
    }

    // 39. 运行期错误。
    if has_runtime_errors {
        if runtime_diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("Failed to load extension"))
        {
            output.write_line_stderr(&Style::detect().yellow(EXTENSION_LOAD_FAILURE_HINT));
        }
        return Ok(1);
    }
    time("createAgentSession", TimingLabel::Main);

    // 40. 非交互模式必须有模型。
    if app_mode != AppMode::Interactive && session.model().is_none() {
        output.write_line_stderr(&Style::detect().red(&format_no_models_available_message()));
        return Ok(1);
    }

    // 41. 启动基准仅支持交互模式。
    let startup_benchmark =
        is_truthy_env_flag(std::env::var("KIANA_STARTUP_BENCHMARK").ok().as_deref());
    if startup_benchmark && app_mode != AppMode::Interactive {
        output.write_line_stderr(
            &Style::detect().red("Error: KIANA_STARTUP_BENCHMARK only supports interactive mode"),
        );
        return Ok(1);
    }

    // 42. RPC 后台刷新 model catalog。
    //
    // 不阻塞启动，失败被忽略；`AbortScope` 在 15 秒后中止请求。只有 RPC 且非离线模式
    // 会启动这个刷新任务。
    if !offline_mode && app_mode == AppMode::Rpc {
        let model_runtime = model_runtime.clone();
        let signal = abort_scope_with_timeout(15_000);
        tokio::spawn(async move {
            let _ = model_runtime
                .refresh(ModelsRefreshOptions {
                    signal: Some(signal),
                    ..Default::default()
                })
                .await;
        });
    }

    // 43. 分发先前：消费 InterruptedPrompt 的 recovery continuation。
    //
    // 用户在 prompt 之后、回答之前杀掉进程时，会话尾部留下一条没有回答的 user entry。
    // 启动路径在这里按 entry id 去重地重放一次（不新增 canonical user entry）；RPC
    // 模式由客户端驱动请求，不在此处重放。失败只告警，不阻止启动。
    if app_mode != AppMode::Rpc {
        match session.resume_recovery_continuation().await {
            Ok(true) => {
                output.write_line_stderr("Replayed the interrupted prompt from the previous run.");
            }
            Ok(false) => {}
            Err(error) => {
                output.write_line_stderr(&format!(
                    "Warning: failed to replay the interrupted prompt: {error}"
                ));
            }
        }
    }

    // 43. 无人值守 cron（opt-in：`KIANA_CRON=1` 或 settings `cron.enabled`）。
    //
    // run_main 在这里创建 cron 目录/调度循环，并在会话所在的 LocalSet 上启动 worker：
    // scheduler 只负责“何时派发”，worker 用同一份 AgentSession 执行 prompt。sender
    // 在 run_main 结束时 drop，循环随之退出。
    let _cron_shutdown = if app_mode != AppMode::Rpc {
        match cron_startup_config(&settings_manager) {
            Some((root, interval)) => {
                match start_cron_background(session.clone(), root.clone(), interval, output.clone())
                    .await
                {
                    Ok(shutdown) => {
                        output.write_line_stderr(&format!(
                            "Cron scheduler started ({}).",
                            root.display()
                        ));
                        Some(shutdown)
                    }
                    Err(error) => {
                        output.write_line_stderr(&format!(
                            "Warning: failed to start cron scheduler: {error}"
                        ));
                        None
                    }
                }
            }
            None => None,
        }
    } else {
        None
    };

    // 43. IDE 接线：发现 lockfile → MCP initialize → 推送初始 contextUpdate。
    //
    // IDE 是增强能力：没有 IDE 是 `Ok(None)`；连接失败只告警，不阻止会话启动。
    // `KIANA_IDE=0/false/off` 显式关闭；离线模式跳过（连接是 loopback HTTP）。
    if !offline_mode && ide_startup_enabled() {
        match crate::ide::connect_for_workspace(&session_cwd).await {
            Ok(Some(connection)) => {
                let context = crate::ide::IdeContext {
                    trust: settings_manager.is_project_trusted(),
                    ..Default::default()
                };
                if let Err(error) = connection.client.send_context_update(&context).await {
                    output.write_line_stderr(&format!(
                        "Warning: failed to send IDE context update: {error}"
                    ));
                }
                crate::ide::install_connection(Some(connection));
            }
            Ok(None) => {}
            Err(error) => {
                output.write_line_stderr(&format!("Warning: failed to connect to IDE: {error}"));
            }
        }
    }

    // 43. daemon 模式（`--serve`）：把会话事件暴露成 SSE/WebSocket/REST。
    //
    // 放在模式分发之前：daemon 不进入交互/打印，也不接管 stdout；cron 已在上面启动，
    // 因此无人值守任务与事件服务可以同时工作。
    if parsed.serve {
        print_timings();
        return run_daemon_mode(session, &parsed, output).await;
    }

    // 43. 分发。
    //
    // 三条分支的退出清理不同：Interactive 不接管 stdout；Print/Json 分支在返回前停止
    // 主题 watcher 并恢复 stdout；RPC 把协议读写与退出码交给 `run_rpc_mode`，不在此处
    // 恢复接管状态。
    match app_mode {
        AppMode::Rpc => {
            print_timings();
            let exit_code = run_rpc_mode(runtime, global_raw_stdout()).await;
            Ok(exit_code)
        }
        AppMode::Interactive => {
            let mut interactive_mode = InteractiveMode::new(
                runtime,
                InteractiveModeOptions {
                    migrated_providers: Some(migrations.migrated_auth_providers.clone()),
                    startup_diagnostics: Some(startup_diagnostics),
                    model_fallback_message,
                    auto_trust_on_reload_cwd,
                    initial_message: initial.initial_message,
                    initial_images: initial.initial_images,
                    initial_messages: Some(parsed.messages.clone()),
                    verbose: Some(parsed.verbose),
                    tui_mode: parsed.tui_mode,
                    initial_theme_setting: parsed.use_theme.clone(),
                    terminal: None,
                },
            );
            if startup_benchmark {
                interactive_mode.init().await;
                time("interactiveMode.init", TimingLabel::Main);
                // 给 TUI 的 stdin 处理器一点时间消费终端查询应答后再恢复终端。
                tokio::time::sleep(Duration::from_millis(150)).await;
                interactive_mode.stop();
                stop_theme_watcher();
                print_timings();
                output.flush().await;
                flush_stdio();
                return Ok(0);
            }
            print_timings();
            interactive_mode.run().await;
            Ok(0)
        }
        AppMode::Print | AppMode::Json => {
            print_timings();
            let exit_code = run_print_mode(
                runtime,
                PrintModeOptions {
                    mode: to_print_output_mode(app_mode),
                    messages: parsed.messages.clone(),
                    initial_message: initial.initial_message,
                    initial_images: initial.initial_images,
                },
                global_raw_stdout(),
            )
            .await;
            stop_theme_watcher();
            output_guard::restore_stdout();
            Ok(exit_code)
        }
    }
}

// ============================================================================
// 内部函数
// ============================================================================

/// `promptConfirm` 是否已通过 `readline` 消费并关闭了管道 stdin。
///
/// TS 的 `promptConfirm` 在回答后调用 `rl.close()`：readline 暂停 `process.stdin`，
/// 并把首个数据块中行终止符之后的字节留在内部缓冲区里丢弃。之后 `readPipedStdin`
/// 挂 `data`/`end` 监听器并 `resume()`：
/// - 流尚未读到 EOF（管道里还有数据，或写端仍然打开）→ 读取剩余内容后 resolve，
///   `main` 继续启动流程；
/// - EOF 在监听器挂上之前就已经发生 → `end` 不会再次触发，promise 永不 resolve，
///   Node 事件循环耗尽后进程静默退出 0。
///
/// 标记只在 stdin 不是 TTY 时置位；[`read_piped_stdin`] 消费后清零。
static PROMPT_CONSUMED_STDIN: AtomicBool = AtomicBool::new(false);

/// Node 从管道读取的数据块大小（`libuv` 的 64 KiB 读缓冲）。
const READLINE_CHUNK_BYTES: usize = 65_536;

/// [`read_piped_stdin`] 的结果。
enum PipedStdin {
    /// 读到（可能为空）的 stdin 内容：`None` 等价 TS 的 `undefined`。
    Content(Option<String>),
    /// stdin 在 [`prompt_confirm`] 之后已经 EOF：TS 的 `readPipedStdin` 永不 resolve。
    ClosedByPrompt,
}

/// `readPipedStdin()`：TTY → `None`；否则读尽 stdin 并 `trim()`，空串 → `None`。
async fn read_piped_stdin() -> PipedStdin {
    if std::io::stdin().is_terminal() {
        return PipedStdin::Content(None);
    }
    // `promptConfirm` 的 readline 已经消费过一行：只有流还没走到 EOF 时，
    // `readPipedStdin` 才能等到 `end`（见 [`PROMPT_CONSUMED_STDIN`]）。
    let prompt_consumed = PROMPT_CONSUMED_STDIN.swap(false, Ordering::SeqCst);
    let read = tokio::task::spawn_blocking(move || {
        if prompt_consumed && stdin_reached_eof() {
            return None;
        }
        let mut buffer = Vec::new();
        let _ = std::io::stdin().read_to_end(&mut buffer);
        Some(String::from_utf8_lossy(&buffer).into_owned())
    })
    .await;
    let data = match read {
        Ok(Some(data)) => data,
        // EOF 早于监听器：TS 的 promise 永不 resolve，调用方按进程退出语义返回 0。
        Ok(None) => return PipedStdin::ClosedByPrompt,
        // 阻塞任务 panic 时保持旧的“空 stdin”行为。
        Err(_) => String::new(),
    };
    let trimmed = kiana_tui::text::trim_js(&data);
    PipedStdin::Content((!trimmed.is_empty()).then(|| trimmed.to_string()))
}

/// 管道 stdin 是否已经到达 EOF 且没有待读数据。
///
/// 零超时 `poll(2)`：`POLLIN` 说明还有可读数据（即使写端已关闭也要继续读）；
/// 只有 `POLLHUP`/`POLLERR`/`POLLNVAL` 而没有 `POLLIN` 才等价于 Node 已经发出
/// `end`。`poll` 返回 0（写端仍打开、暂无数据）不算 EOF —— TS 此时也会挂上监听器
/// 继续等待后续数据或迟到的 EOF。
#[cfg(unix)]
fn stdin_reached_eof() -> bool {
    use std::os::fd::AsRawFd;

    fd_reached_eof(std::io::stdin().as_raw_fd())
}

/// [`stdin_reached_eof`] 的 fd 版本（便于用管道/文件做单元测试）。
///
/// 普通文件（`kiana ... < file`）的 `poll(2)` 总是报告可读，EOF 只能通过文件偏移
/// 判断：当前偏移已到文件末尾时，Node 的流在之后的事件循环轮次里必然已经发出
/// `end`；仍有剩余字节时则继续读取。
#[cfg(unix)]
fn fd_reached_eof(fd: std::os::fd::RawFd) -> bool {
    // SAFETY: `stat` 只在调用成功后读取。
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: `fd` 有效，`stat` 指向可写内存。
    if unsafe { libc::fstat(fd, &mut stat) } == 0 && stat.st_mode & libc::S_IFMT == libc::S_IFREG {
        // SAFETY: `SEEK_CUR` 只查询当前偏移，不移动文件指针。
        let offset = unsafe { libc::lseek(fd, 0, libc::SEEK_CUR) };
        return offset >= 0 && offset >= stat.st_size;
    }

    let mut descriptor = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: `descriptor` 是调用期间有效的单个 `pollfd`。
    let ready = unsafe { libc::poll(&mut descriptor, 1, 0) };
    ready > 0
        && descriptor.revents & libc::POLLIN == 0
        && descriptor.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0
}

/// 非 Unix 平台没有零超时 `poll(2)`：不预判 EOF，总是继续读取 stdin。
#[cfg(not(unix))]
fn stdin_reached_eof() -> bool {
    false
}

/// `reportDiagnostics(diagnostics)`。
fn report_diagnostics(diagnostics: &[AgentSessionRuntimeDiagnostic], output: &Output) {
    let style = Style::detect();
    for diagnostic in diagnostics {
        let color = match diagnostic.kind {
            DiagnosticKind::Error => style.red(&format!("Error: {}", diagnostic.message)),
            DiagnosticKind::Warning => style.yellow(&format!("Warning: {}", diagnostic.message)),
            DiagnosticKind::Info => style.dim(&diagnostic.message),
        };
        output.write_line_stderr(&color);
    }
}

/// `isTruthyEnvFlag(value)`：非空且 `1` / `true` / `yes`（大小写不敏感）。
fn is_truthy_env_flag(value: Option<&str>) -> bool {
    let Some(value) = value else {
        return false;
    };
    if value.is_empty() {
        return false;
    }
    value == "1" || value.to_lowercase() == "true" || value.to_lowercase() == "yes"
}

/// `resolveAppMode(parsed, stdinIsTTY, stdoutIsTTY)`。
///
/// 优先级：`--mode rpc` > `--mode json` > `--print` 或任一标准流不是 TTY > 交互。
/// TTY 判定由调用方传入，便于测试；stdin 或 stdout 任一被重定向都会退化为非交互的
/// 打印模式（stdin 重定向还可能带初始消息，见第 33 步）。
fn resolve_app_mode(parsed: &Args, stdin_is_tty: bool, stdout_is_tty: bool) -> AppMode {
    match parsed.mode {
        Some(Mode::Rpc) => return AppMode::Rpc,
        Some(Mode::Json) => return AppMode::Json,
        _ => {}
    }
    if parsed.print || !stdin_is_tty || !stdout_is_tty {
        return AppMode::Print;
    }
    AppMode::Interactive
}

/// `toPrintOutputMode(appMode)`。
fn to_print_output_mode(app_mode: AppMode) -> PrintOutputMode {
    if app_mode == AppMode::Json {
        PrintOutputMode::Json
    } else {
        PrintOutputMode::Text
    }
}

/// `isPlainRuntimeMetadataCommand(parsed)`。
///
/// 仅当 `--help` 或 `--list-models` 生效、且没有 `--print`/显式 `--mode` 时为真：
/// 这两类输出属于用户请求的元数据，不应触发 stdout 接管。
fn is_plain_runtime_metadata_command(parsed: &Args) -> bool {
    !parsed.print && parsed.mode.is_none() && (parsed.help || parsed.list_models.is_some())
}

/// `runAuthCommand(args)`：`Some(code)` 表示已处理。
async fn run_auth_command(args: &[String], output: &Output) -> CliResult<Option<i32>> {
    let style = Style::detect();
    if is_auth_command_help(args) {
        print_auth_command_help(output);
        return Ok(Some(0));
    }

    let command = match parse_auth_command(args) {
        Ok(Some(command)) => command,
        Ok(None) => return Ok(None),
        Err(error) => {
            output.write_line_stderr(&style.red(&format!("Error: {error}")));
            return Ok(Some(1));
        }
    };

    let parsed = parse_args(&command.args);
    if let Some(option) = parsed.unknown_flags.keys().next() {
        output.write_line_stderr(&style.red(&format!(
            "Unknown option --{option} for \"{}\".",
            get_auth_command_name(command.kind)
        )));
        output.write_line_stderr(&style.dim(&format!(
            "Use \"{APP_NAME} --help\" or \"{}\".",
            get_auth_command_usage(command.kind)
        )));
        return Ok(Some(1));
    }

    match execute_auth_command(&command, parsed, output).await {
        Ok(code) => Ok(Some(code)),
        Err(error) => {
            output.write_line_stderr(&style.red(&format!("Error: {error}")));
            let code = if command.kind == AuthCommandKind::Check {
                2
            } else {
                1
            };
            Ok(Some(code))
        }
    }
}

/// `runAuthCommand` 的 try 块；错误由调用方按 TS 的 catch 处理。
async fn execute_auth_command(
    command: &AuthCommand,
    parsed: Args,
    output: &Output,
) -> Result<i32, AuthCommandError> {
    if !parsed.diagnostics.is_empty() {
        return Err(AuthCommandError::new(
            parsed
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.message.as_str())
                .collect::<Vec<_>>()
                .join("\n"),
        ));
    }

    let kind = command.kind;
    // 两条分支共用本函数：非 `check` 子命令解析并打印凭据；`check` 需要构造凭据存储、
    // 做一次 auth 检查，并把结果映射成退出码 0/1/2（ready/not_ready/invalid）。
    if kind != AuthCommandKind::Check {
        let signal = abort_scope_with_timeout(15_000);
        let model_runtime = ModelRuntime::create(CreateModelRuntimeOptions {
            allow_model_network: false,
            signal: Some(signal.clone()),
            ..Default::default()
        })
        .await
        // TS 的外层 catch：非 `AuthCommandError` 一律折叠成固定文案
        // `Failed to resolve credential`（`ModelRuntime.create` 的失败属于这一类）。
        .map_err(|_| AuthCommandError::new("Failed to resolve credential"))?;
        let credential = resolve_credential_for_print(
            &parsed,
            &model_runtime,
            kind,
            command.min_expiry_ms,
            Some(signal),
        )
        .await?;
        output.write_stdout(&format!("{credential}\n"));
        return Ok(0);
    }

    // `check`
    let requested_auth = validate_auth_command_args(&parsed, kind)?;
    // TS 内层 catch 的 provider 来源：`requestedAuth.provider ?? requestedAuth.model!`。
    let requested_provider = requested_auth
        .provider
        .clone()
        .or_else(|| requested_auth.model.clone())
        .unwrap_or_default();
    let credentials: Arc<dyn kiana_ai::auth::types::CredentialStore> = if command.no_refresh {
        Arc::new(ReadOnlyAuthStorage::with_default_path())
    } else {
        Arc::new(AuthStorage::create_default())
    };

    let mut result: AuthCheckResult;
    let mut credential: Option<String> = None;
    // `check` 的失败折叠规则：model runtime 创建失败、`checkProviderAuth` 失败、
    // `getProviderCredential` 失败都视为 invalid（退出码 2）；provider 不存在
    // （`provider_not_found`）与缺少凭据（`credentials_not_configured`，含空串凭据）是
    // not_ready（退出码 1）。
    match create_auth_check_model_runtime(Arc::clone(&credentials)).await {
        Ok(model_runtime) => {
            match check_provider_auth(
                &parsed,
                &model_runtime,
                AuthCheckOptions {
                    refresh: !command.no_refresh,
                },
            )
            .await
            {
                Ok(found) => {
                    result = found;
                    if command.credentials && result.status == AuthCheckStatus::Ready {
                        match get_provider_credential(
                            &result.provider,
                            &model_runtime,
                            credentials.as_ref(),
                            AuthCheckOptions {
                                refresh: !command.no_refresh,
                            },
                        )
                        .await
                        {
                            Ok(found_credential) => match found_credential {
                                Some(found_credential) => {
                                    // TS 用 `if (!credential)` 判断：空串是假值，状态改为
                                    // `not_ready`，但变量本身仍是 `""`（非 `--json` 输出打印
                                    // 空行；JSON 的 `credential ? {...} : {}` 省略字段）。
                                    if found_credential.is_empty() {
                                        result = AuthCheckResult {
                                            status: AuthCheckStatus::NotReady,
                                            provider: result.provider,
                                            reason: Some(AuthCheckReason::CredentialNotAvailable),
                                            auth_type: None,
                                        };
                                    }
                                    credential = Some(found_credential);
                                }
                                None => {
                                    result = AuthCheckResult {
                                        status: AuthCheckStatus::NotReady,
                                        provider: result.provider,
                                        reason: Some(AuthCheckReason::CredentialNotAvailable),
                                        auth_type: None,
                                    };
                                }
                            },
                            Err(_) => {
                                // TS 的内层 try/catch 把 getProviderCredential 的**任何**异常
                                // 映射为 `{status:"invalid", reason:"invalid_state"}`（退出码 2），
                                // 只有返回空凭据才是 not_ready。
                                result = invalid_auth_result(requested_provider.clone());
                            }
                        }
                    }
                }
                Err(_) => {
                    result = invalid_auth_result(requested_provider.clone());
                }
            }
        }
        Err(_) => {
            result = invalid_auth_result(requested_provider.clone());
        }
    }

    let text = if command.json {
        crate::cli::auth_check::AuthCheckJson::from_result(
            &result,
            credential
                .as_deref()
                .filter(|credential| !credential.is_empty()),
        )
        .to_json()
    } else {
        credential
            .clone()
            .unwrap_or_else(|| result.status.as_str().to_string())
    };
    output.write_stdout(&format!("{text}\n"));
    Ok(match result.status {
        AuthCheckStatus::Ready => 0,
        AuthCheckStatus::NotReady => 1,
        AuthCheckStatus::Invalid => 2,
    })
}

/// `{status:"invalid", provider, reason:"invalid_state"}`。
fn invalid_auth_result(provider: String) -> AuthCheckResult {
    AuthCheckResult {
        status: AuthCheckStatus::Invalid,
        provider,
        reason: Some(AuthCheckReason::InvalidState),
        auth_type: None,
    }
}

/// `prepareInitialMessage(parsed, stdinContent)`。
async fn prepare_initial_message(
    parsed: &mut Args,
    stdin_content: Option<String>,
) -> CliResult<crate::cli::initial_message::InitialMessageResult> {
    if parsed.file_args.is_empty() {
        return Ok(build_initial_message(
            parsed,
            InitialMessageInput {
                file_text: None,
                file_images: None,
                stdin_content,
            },
        ));
    }
    let processed = process_file_arguments(
        &parsed.file_args,
        ProcessFileOptions {
            auto_resize_images: Some(false),
        },
    )
    .await?;
    Ok(build_initial_message(
        parsed,
        InitialMessageInput {
            file_text: Some(processed.text),
            file_images: Some(processed.images),
            stdin_content,
        },
    ))
}

/// `ResolvedSession`。
///
/// `resolveSessionPath` 的返回：前三种都带可直接打开的 `path`，区别只在来源与调用方
/// 是否需要提示跨项目 fork；`NotFound` 携带原始参数用于错误文案。
#[derive(Debug, Clone, PartialEq, Eq)]
enum ResolvedSession {
    /// 直接文件路径。
    Path { path: String },
    /// 当前项目中的会话。
    Local { path: String },
    /// 其它项目中的会话。
    Global { path: String, cwd: String },
    /// 未找到。
    NotFound { arg: String },
}

/// `findLocalSessionByExactId(sessionId, cwd, sessionDir?)`。
///
/// 精确 id 查找不走会话列表（`SessionManager::find_by_id` 只扫描目录中的 header），
/// 因此不会加载消息体；找不到返回 `Ok(None)`，只有目录解析/创建失败才返回错误。
fn find_local_session_by_exact_id(
    session_id: &str,
    cwd: &str,
    session_dir: Option<&str>,
) -> CliResult<Option<ResolvedSession>> {
    match SessionManager::find_by_id(cwd, session_id, session_dir) {
        Ok(Some(path)) => Ok(Some(ResolvedSession::Local { path })),
        Ok(None) => Ok(None),
        Err(error) => Err(report_session_error(&Output::process(), &error)),
    }
}

/// `resolveSessionPath(sessionArg, cwd, sessionDir?)`。
///
/// 按 TS 的固定顺序解析 `--session`/`--fork` 的参数：
/// 1. 含路径分隔符（`/` 或 `\`）或以 `.jsonl` 结尾 → 视为文件路径，相对 `cwd` 解析；
///    这里不检查文件是否存在，是否存在/能否新建由后续的 `SessionManager::open` 或
///    `SessionManager::fork_from` 决定。
/// 2. 当前项目内精确 id 命中 → `Local`。
/// 3. 当前项目会话列表中第一个 id 前缀匹配 → `Local`（先后取决于
///    `SessionManager::list` 的返回顺序）。
/// 4. 全局会话列表先找精确 id、再找前缀 → `Global`（带该会话的 cwd）。
/// 5. 都不命中 → `NotFound`。
///
/// 空串 `session_dir` 按“未设置”处理，全局列表回退到默认目录（与 TS 的真值语义一致；
/// 见下方注释）。列表读取失败会打印 `Error: ...` 并返回退出码 1。
async fn resolve_session_path(
    session_arg: &str,
    cwd: &str,
    session_dir: Option<&str>,
) -> CliResult<ResolvedSession> {
    if session_arg.contains('/') || session_arg.contains('\\') || session_arg.ends_with(".jsonl") {
        return Ok(ResolvedSession::Path {
            path: kiana_core::paths::resolve_path(
                session_arg,
                cwd,
                &kiana_core::paths::PathInputOptions::default(),
            ),
        });
    }

    if let Some(exact) = find_local_session_by_exact_id(session_arg, cwd, session_dir)? {
        return Ok(exact);
    }

    let local_sessions = SessionManager::list(cwd, session_dir, None, None)
        .await
        .map_err(|error| report_list_error(&Output::process(), &error))?;
    if let Some(local_match) = local_sessions
        .iter()
        .find(|session| session.id.starts_with(session_arg))
    {
        return Ok(ResolvedSession::Local {
            path: local_match.path.clone(),
        });
    }

    // TS: `SessionManager.listAll(sessionDir)` 里 `customSessionDir` 用真值判断，
    // 空串是假值 → 回退到默认的全局 sessions 目录（而不是扫描 `""`）。
    let all_sessions = match session_dir.filter(|dir| !dir.is_empty()) {
        Some(dir) => SessionManager::list_all_in(dir, None, None).await,
        None => SessionManager::list_all(None, None).await,
    }
    .map_err(|error| report_list_error(&Output::process(), &error))?;
    let global_match = all_sessions
        .iter()
        .find(|session| session.id == session_arg)
        .or_else(|| {
            all_sessions
                .iter()
                .find(|session| session.id.starts_with(session_arg))
        });
    if let Some(global_match) = global_match {
        return Ok(ResolvedSession::Global {
            path: global_match.path.clone(),
            cwd: global_match.cwd.clone(),
        });
    }

    Ok(ResolvedSession::NotFound {
        arg: session_arg.to_string(),
    })
}

/// `promptConfirm(message)`：读一行 stdin，`y`/`yes` 为真。
///
/// TS 用 `readline.createInterface().question()`：stdin 关闭（EOF）时回调**永远不会**
/// 被调用，Node 的事件循环自然耗尽并以 0 退出。因此 Rust 侧用 [`Option`] 表达“没有
/// 答案”，调用方不得再打印 `Aborted.`。
async fn prompt_confirm(message: &str, output: &Output) -> Option<bool> {
    output.write_stdout(&format!("{message} [y/N] "));
    let stdin_is_terminal = std::io::stdin().is_terminal();
    let line = tokio::task::spawn_blocking(read_readline_line)
        .await
        .unwrap_or_default()?;
    // TS 的 `rl.close()` 暂停了管道 stdin；随后 `readPipedStdin` 只在 EOF 尚未
    // 发生时才可能 resolve（TTY 上 readline 只是暂停，不置位）。
    if !stdin_is_terminal {
        PROMPT_CONSUMED_STDIN.store(true, Ordering::SeqCst);
    }
    let answer = line.to_lowercase();
    Some(answer == "y" || answer == "yes")
}

/// Node `readline` 的“一行”语义。
///
/// `rl.question()` 的回调只在收到行终止符时触发：`\n`、`\r`、`\r\n` 都结束一行；
/// 若流在终止符之前 EOF，readline 丢弃缓冲区里的残留字节，回调**永远不会**被调用
/// （调用方据此静默退出 0）。返回的行不含终止符。
///
/// 读取按 Node 管道的数据块大小（64 KiB）进行：readline 会把同一个数据块里行终止符
/// 之后的字节存入内部缓冲，`rl.close()` 时一并丢弃；之后的 [`read_piped_stdin`]
/// 只能看到尚未从管道读出的剩余数据。逐字节读取会把同一块里的剩余内容留给
/// `readPipedStdin`，与 TS 不符。
fn read_readline_line() -> Option<String> {
    let mut stdin = std::io::stdin().lock();
    let mut bytes: Vec<u8> = Vec::new();
    let mut chunk = vec![0u8; READLINE_CHUNK_BYTES];
    loop {
        let read = match stdin.read(&mut chunk) {
            Ok(0) | Err(_) => return None,
            Ok(read) => read,
        };
        let slice = &chunk[..read];
        if let Some(position) = slice
            .iter()
            .position(|byte| *byte == b'\n' || *byte == b'\r')
        {
            bytes.extend_from_slice(&slice[..position]);
            // 终止符之后的同一块数据随 `rl.close()` 丢弃。
            return Some(String::from_utf8_lossy(&bytes).into_owned());
        }
        bytes.extend_from_slice(slice);
    }
}

/// `validateForkFlags(parsed)`。
fn validate_fork_flags(parsed: &Args, output: &Output) -> CliResult<()> {
    // TS: `if (!parsed.fork) return;` —— 空串是假值，等价于未提供。
    if js_truthy(parsed.fork.as_deref()).is_none() {
        return Ok(());
    }
    let conflicting = [
        // TS 用真值判断：`parsed.session ? "--session" : undefined`，空串不算提供。
        js_truthy(parsed.session.as_deref()).map(|_| "--session"),
        parsed.r#continue.then_some("--continue"),
        parsed.resume.then_some("--resume"),
        parsed.no_session.then_some("--no-session"),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    if conflicting.is_empty() {
        return Ok(());
    }
    output.write_line_stderr(&Style::detect().red(&format!(
        "Error: --fork cannot be combined with {}",
        conflicting.join(", ")
    )));
    Err(CliExit::new(1))
}

/// `validateSessionIdFlags(parsed)`。
fn validate_session_id_flags(parsed: &Args, output: &Output) -> CliResult<()> {
    let Some(session_id) = parsed.session_id.as_deref() else {
        return Ok(());
    };
    let conflicting = [
        // TS 用真值判断：`parsed.session ? "--session" : undefined`，空串不算提供。
        js_truthy(parsed.session.as_deref()).map(|_| "--session"),
        parsed.r#continue.then_some("--continue"),
        parsed.resume.then_some("--resume"),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    if !conflicting.is_empty() {
        output.write_line_stderr(&Style::detect().red(&format!(
            "Error: --session-id cannot be combined with {}",
            conflicting.join(", ")
        )));
        return Err(CliExit::new(1));
    }
    match kiana_session_core::session_manager::assert_valid_session_id(session_id) {
        Ok(()) => Ok(()),
        Err(error) => {
            output.write_line_stderr(&Style::detect().red(&format!("Error: {error}")));
            Err(CliExit::new(1))
        }
    }
}

/// `openSessionOrExit(path, sessionDir?)`。
fn open_session_or_exit(
    path: &str,
    session_dir: Option<&str>,
    output: &Output,
) -> CliResult<SessionManager> {
    SessionManager::open(path, session_dir, None)
        .map_err(|error| report_session_error(output, &error))
}

/// `forkSessionOrExit(sourcePath, cwd, sessionDir?, sessionId?)`。
fn fork_session_or_exit(
    source_path: &str,
    cwd: &str,
    session_dir: Option<&str>,
    session_id: Option<String>,
    output: &Output,
) -> CliResult<SessionManager> {
    SessionManager::fork_from(
        source_path,
        cwd,
        session_dir,
        Some(NewSessionOptions {
            id: session_id,
            parent_session: None,
        }),
    )
    .map_err(|error| report_session_error(output, &error))
}

/// `createSessionManager(parsed, cwd, sessionDir, settingsManager)`。
///
/// 分支顺序固定为“先命中先返回”：
/// 1. `--no-session` / `--help` / `--list-models`：返回内存会话，不写盘
///    （`get_session_file()` 恒为 `None`）；`--session-id` 仍作为新会话 id，非法 id 会
///    报错。
/// 2. `--fork`：若同时给了 `--session-id` 且当前项目已存在该 id，直接报错退出 1；
///    否则解析源会话并 fork（源不存在 → 报错退出 1）。
/// 3. `--session`：`Path`/`Local` 直接打开；`Global` 先打印会话所在项目并提示是否
///    fork 到当前目录，拒绝或 stdin EOF 都以 0 返回。
/// 4. `--resume`：启动会话选择器；未选中时打印 `No session selected` 并以 0 返回。
/// 5. `--continue`：打开最近的会话。
/// 6. `--session-id`：当前项目已有同 id → 打开；否则打印 Warning 后新建。
/// 7. 其余情况新建会话。
///
/// `session_dir` 会原样传给会话查找/打开/新建入口（内存会话除外）。`Err(CliExit)`
/// 对应 TS 的 `process.exit(code)`：错误文案已写入 `output`，测试可直接断言退出码。
pub async fn create_session_manager(
    parsed: &Args,
    cwd: &str,
    session_dir: Option<&str>,
    settings_manager: &SettingsManager,
    output: &Output,
) -> CliResult<SessionManager> {
    // `settings_manager` 只被 `--resume` 选择器使用。
    //
    // help/list-models 也需要一个可工作的 SessionManager，但绝不能持久化：否则会得到一个
    // 持久化的 `SessionManager`（`get_session_file()` 为 `Some`，文件本身仍要等到首条
    // user/assistant 消息才真正写盘，见 `kiana-session-core` 的 `persist_last_entry`）。
    // in-memory 会话的 `get_session_file()` 恒为 `None`，`tests/session_id_readonly.rs`
    // 对 help 路径断言 `get_session_file().is_none()`、对进程级路径断言磁盘上无该会话文件。
    if parsed.no_session || parsed.help || parsed.list_models.is_some() {
        return SessionManager::in_memory(
            cwd,
            parsed.session_id.clone().map(|id| NewSessionOptions {
                id: Some(id),
                parent_session: None,
            }),
            None,
        )
        .map_err(|error| report_session_error(output, &error));
    }

    if let Some(fork_arg) = js_truthy(parsed.fork.as_deref()).map(str::to_string) {
        if let Some(session_id) = js_truthy(parsed.session_id.as_deref()).map(str::to_string)
            && find_local_session_by_exact_id(&session_id, cwd, session_dir)?.is_some()
        {
            // 只在当前项目内查重：fork 出来的新会话必须能用这个 id 新建，因此已存在就
            // 直接失败，而不是像普通 `--session-id` 分支那样复用旧会话。
            output.write_line_stderr(
                &Style::detect().red(&format!("Session already exists with id '{session_id}'")),
            );
            return Err(CliExit::new(1));
        }

        let resolved = resolve_session_path(&fork_arg, cwd, session_dir).await?;
        return match resolved {
            ResolvedSession::Path { path }
            | ResolvedSession::Local { path }
            | ResolvedSession::Global { path, .. } => {
                fork_session_or_exit(&path, cwd, session_dir, parsed.session_id.clone(), output)
            }
            ResolvedSession::NotFound { arg } => {
                output.write_line_stderr(
                    &Style::detect().red(&format!("No session found matching '{arg}'")),
                );
                Err(CliExit::new(1))
            }
        };
    }

    if let Some(session_arg) = js_truthy(parsed.session.as_deref()).map(str::to_string) {
        let resolved = resolve_session_path(&session_arg, cwd, session_dir).await?;
        return match resolved {
            ResolvedSession::Path { path } | ResolvedSession::Local { path } => {
                open_session_or_exit(&path, session_dir, output)
            }
            ResolvedSession::Global {
                path,
                cwd: other_cwd,
            } => {
                output.write_line_stdout(
                    &Style::detect()
                        .yellow(&format!("Session found in different project: {other_cwd}")),
                );
                match prompt_confirm("Fork this session into current directory?", output).await {
                    Some(true) => {}
                    Some(false) => {
                        output.write_line_stdout(&Style::detect().dim("Aborted."));
                        return Err(CliExit::new(0));
                    }
                    // `None`（EOF）时 TS 的 readline 回调不触发：不打印 `Aborted.`，直接以 0 退出。
                    None => return Err(CliExit::new(0)),
                }
                fork_session_or_exit(&path, cwd, session_dir, None, output)
            }
            ResolvedSession::NotFound { arg } => {
                output.write_line_stderr(
                    &Style::detect().red(&format!("No session found matching '{arg}'")),
                );
                Err(CliExit::new(1))
            }
        };
    }

    if parsed.resume {
        let current_cwd = cwd.to_string();
        let current_session_dir = session_dir.map(str::to_string);
        let current_loader_cwd = current_cwd.clone();
        let current_loader_dir = current_session_dir.clone();
        let current_sessions_loader: kiana_interactive::components::session_selector::SessionsLoader =
            Arc::new(move |on_progress, signal| {
                let cwd = current_loader_cwd.clone();
                let session_dir = current_loader_dir.clone();
                Box::pin(async move {
                    SessionManager::list(&cwd, session_dir.as_deref(), on_progress, signal.as_ref())
                        .await
                })
            });
        let all_loader_dir = current_session_dir.clone();
        let all_sessions_loader: kiana_interactive::components::session_selector::SessionsLoader =
            Arc::new(move |on_progress, signal| {
                let session_dir = all_loader_dir.clone();
                Box::pin(async move {
                    // 同 `resolveSessionPath`：空 sessionDir 是 JS 假值，`listAll` 回退到
                    // 默认的全局 sessions 目录。
                    match session_dir.filter(|dir| !dir.is_empty()) {
                        Some(dir) => {
                            SessionManager::list_all_in(&dir, on_progress, signal.as_ref()).await
                        }
                        None => SessionManager::list_all(on_progress, signal.as_ref()).await,
                    }
                })
            });
        let selected = select_session(
            current_sessions_loader,
            all_sessions_loader,
            settings_manager,
        )
        .await;
        // 先停止主题 watcher，再把选择器的结果/错误向上传递（对应 TS 的 `finally`）；
        // 这样无论选中、取消还是读取列表失败，都不会遗留监听。
        stop_theme_watcher();
        let selected = selected?;
        let Some(selected_path) = selected else {
            output.write_line_stdout(&Style::detect().dim("No session selected"));
            return Err(CliExit::new(0));
        };
        return open_session_or_exit(&selected_path, session_dir, output);
    }

    if parsed.r#continue {
        return SessionManager::continue_recent(cwd, session_dir)
            .map_err(|error| report_session_error(output, &error));
    }

    if let Some(session_id) = js_truthy(parsed.session_id.as_deref()).map(str::to_string) {
        if let Some(existing) = find_local_session_by_exact_id(&session_id, cwd, session_dir)? {
            let ResolvedSession::Local { path } = existing else {
                unreachable!("find_local_session_by_exact_id only returns Local");
            };
            return open_session_or_exit(&path, session_dir, output);
        }
        output.write_line_stderr(&Style::detect().yellow(&format!(
            "Warning: No project session found with id '{session_id}'; creating a new session with that id."
        )));
    }

    SessionManager::create(
        cwd,
        session_dir,
        Some(NewSessionOptions {
            id: parsed.session_id.clone(),
            parent_session: None,
        }),
    )
    .map_err(|error| report_session_error(output, &error))
}

/// `buildSessionOptions` 的产物。
struct BuiltSessionOptions {
    model: Option<Model>,
    thinking_level: Option<kiana_ai::types::ModelThinkingLevel>,
    scoped_models: Vec<SessionScopedModel>,
    tools: Option<Vec<String>>,
    exclude_tools: Option<Vec<String>>,
    no_tools: Option<NoTools>,
    cli_thinking_from_model: bool,
    /// CLI 显式预算（`--max-turns` 等）；`None` 时交给 settings。
    budget_config: Option<BudgetConfig>,
    /// CLI 显式 loop guard（`--no-loop-guard` / `--loop-guard-stop`）；`None` 时交给
    /// settings（缺省启用保守默认值）。
    loop_guard: Option<kiana_agent_core::loop_guard::LoopGuardHandle>,
    diagnostics: Vec<AgentSessionRuntimeDiagnostic>,
}

/// `buildSessionOptions(parsed, scopedModels, hasExistingSession, modelRuntime, settingsManager)`。
///
/// 决定本次会话的模型、思考级别与工具过滤。模型来源的优先级：
/// 1. `--model`（可带 `--provider`）交给 `resolve_cli_model`，其 warning/error 直接作为
///    诊断返回；
/// 2. 没有 CLI 模型、model scope 非空、且**不是**续接已有会话时，用设置里的默认模型
///    （必须落在 scope 内）或 scope 的第一项；
/// 3. 显式 `--thinking` 最后覆盖以上任何来源的思考级别。
///
/// `cli_thinking_from_model` 记录思考级别是否由模型解析带出，调用方据此决定是否在创建
/// 会话后再应用一次；`has_existing_session` 为真时跳过第 2 步，即不用 scope/默认模型
/// 覆盖已有会话的模型选择。
fn build_session_options(
    parsed: &Args,
    scoped_models: &[kiana_config::models::resolver::ScopedModel],
    has_existing_session: bool,
    model_runtime: &ModelRuntime,
    settings_manager: &SettingsManager,
) -> BuiltSessionOptions {
    let mut model: Option<Model> = None;
    let mut thinking_level = None;
    let mut diagnostics = Vec::new();
    let mut cli_thinking_from_model = false;

    if js_truthy(parsed.provider.as_deref()).is_some()
        && js_truthy(parsed.model.as_deref()).is_none()
    {
        let provider = parsed.provider.clone().unwrap_or_default();
        diagnostics.push(AgentSessionRuntimeDiagnostic::new(
            DiagnosticKind::Error,
            format!(
                "--provider requires --model (for example: --provider {provider} --model <pattern>)"
            ),
        ));
    }

    if let Some(cli_model) = js_truthy(parsed.model.as_deref()).map(str::to_string) {
        let resolved = resolve_cli_model(&ResolveCliModelOptions {
            cli_provider: parsed.provider.clone(),
            cli_model: Some(cli_model),
            cli_thinking: parsed.thinking,
            inline_api_key: js_truthy(parsed.api_key.as_deref()).is_some(),
            model_runtime,
        });
        if let Some(warning) = resolved.warning {
            diagnostics.push(AgentSessionRuntimeDiagnostic::new(
                DiagnosticKind::Warning,
                warning,
            ));
        }
        if let Some(error) = resolved.error {
            diagnostics.push(AgentSessionRuntimeDiagnostic::new(
                DiagnosticKind::Error,
                error,
            ));
        }
        if let Some(resolved_model) = resolved.model {
            model = Some(resolved_model);
            if parsed.thinking.is_none()
                && let Some(resolved_thinking) = resolved.thinking_level
            {
                thinking_level = Some(resolved_thinking);
                cli_thinking_from_model = true;
            }
        }
    }

    if model.is_none() && !scoped_models.is_empty() && !has_existing_session {
        let saved_provider = settings_manager.get_default_provider();
        let saved_model_id = settings_manager.get_default_model();
        let saved_model = match (saved_provider.as_deref(), saved_model_id.as_deref()) {
            (Some(provider), Some(model_id)) => model_runtime.get_model(provider, model_id),
            _ => None,
        };
        let saved_in_scope = saved_model.as_ref().and_then(|saved| {
            scoped_models
                .iter()
                .find(|scoped| same_model(&scoped.model, saved))
        });
        if let Some(scoped) = saved_in_scope {
            model = Some(scoped.model.clone());
            if parsed.thinking.is_none()
                && let Some(scoped_thinking) = scoped.thinking_level
            {
                thinking_level = Some(scoped_thinking);
            }
        } else {
            let first = &scoped_models[0];
            model = Some(first.model.clone());
            if parsed.thinking.is_none()
                && let Some(first_thinking) = first.thinking_level
            {
                thinking_level = Some(first_thinking);
            }
        }
    }

    if let Some(cli_thinking) = parsed.thinking {
        thinking_level = Some(cli_thinking);
    }

    let session_scoped_models = if scoped_models.is_empty() {
        Vec::new()
    } else {
        scoped_models
            .iter()
            .map(|scoped| SessionScopedModel {
                model: scoped.model.clone(),
                thinking_level: scoped.thinking_level,
            })
            .collect()
    };

    let no_tools = if parsed.no_tools {
        Some(NoTools::All)
    } else if parsed.no_builtin_tools {
        Some(NoTools::Builtin)
    } else {
        None
    };

    // CLI 预算/loop guard 覆盖：只在显式给了相关 flag 时构造，否则保持 `None`，
    // 由 `create_agent_session_from_services` 读取 settings。
    let mut budget_config = BudgetConfig::new();
    budget_config.max_turns = parsed.max_turns;
    budget_config.max_tokens = parsed.max_tokens;
    budget_config.max_cost_usd = parsed.max_cost_usd;
    budget_config.max_wall_clock_ms = parsed.max_wall_clock_ms;
    let budget_config = budget_config.is_enabled().then_some(budget_config);

    let loop_guard = if parsed.no_loop_guard {
        Some(Arc::new(Mutex::new(LoopGuard::new(LoopGuardConfig {
            enabled: false,
            ..Default::default()
        })))
            as kiana_agent_core::loop_guard::LoopGuardHandle)
    } else if parsed.loop_guard_stop {
        Some(Arc::new(Mutex::new(LoopGuard::new(LoopGuardConfig {
            stop_on_detection: true,
            ..Default::default()
        })))
            as kiana_agent_core::loop_guard::LoopGuardHandle)
    } else {
        None
    };

    BuiltSessionOptions {
        model,
        thinking_level,
        scoped_models: session_scoped_models,
        tools: parsed.tools.clone(),
        exclude_tools: parsed.exclude_tools.clone(),
        no_tools,
        cli_thinking_from_model,
        budget_config,
        loop_guard,
        diagnostics,
    }
}

/// `modelsAreEqual`（type + id + provider）。
fn same_model(left: &Model, right: &Model) -> bool {
    model_type(left) == model_type(right) && left.id == right.id && left.provider == right.provider
}

/// `getModelType(model)`：`model.type ?? "chat"`。
fn model_type(model: &Model) -> ModelType {
    model.model_type.unwrap_or(ModelType::Chat)
}

/// `resolveCliPaths(cwd, paths?)`。
///
/// 对每一项，`kiana_core::paths::is_local_path` 为真时相对 `cwd` 解析为绝对路径；
/// 否则原样保留（带 `npm:`/`git:`/`github:`/`http(s):`/`ssh:`/`builtin:` 前缀的源）。
/// 未提供时返回空列表。
fn resolve_cli_paths(cwd: &str, paths: Option<&[String]>) -> Vec<String> {
    paths
        .unwrap_or_default()
        .iter()
        .map(|value| {
            if kiana_core::paths::is_local_path(value) {
                kiana_core::paths::resolve_path(
                    value,
                    cwd,
                    &kiana_core::paths::PathInputOptions::default(),
                )
            } else {
                value.clone()
            }
        })
        .collect()
}

/// `promptForMissingSessionCwd(issue, settingsManager)`。
///
/// 返回选中的 cwd：`Continue` 给出 `issue.fallback_cwd`；`Cancel` 与选择器返回 `None`
/// 都折叠成 `None`，调用方在交互模式下据此以 0 退出。
async fn prompt_for_missing_session_cwd(
    issue: &kiana_session_core::session_cwd::SessionCwdIssue,
    settings_manager: &SettingsManager,
) -> Option<String> {
    show_startup_selector(
        settings_manager,
        &format_missing_session_cwd_prompt(issue),
        vec![
            StartupSelectorOption {
                label: "Continue".to_string(),
                value: Some(issue.fallback_cwd.clone()),
            },
            StartupSelectorOption {
                label: "Cancel".to_string(),
                value: None,
            },
        ],
    )
    .await
    .flatten()
}

/// `createRuntime` 的捕获状态。
///
/// runtime 创建工厂可能被调用多次（初次创建与每次会话替换），闭包每次调用都从这里按值
/// 复制一份输入：`Args`/`Output`/四组路径列表相互独立，而 `project_trust_by_cwd`、
/// `trust_store`、`startup_settings_manager` 通过 `Arc`/内部共享在多次调用之间保持一致。
struct CreateRuntimeCaptures {
    parsed: Args,
    project_trust_by_cwd: Arc<Mutex<HashMap<String, bool>>>,
    trust_store: ProjectTrustStore,
    startup_settings_manager: SettingsManager,
    extension_factories: Vec<InlineExtension>,
    output: Output,
    resolved_extension_paths: Vec<String>,
    resolved_skill_paths: Vec<String>,
    resolved_prompt_template_paths: Vec<String>,
    resolved_theme_paths: Vec<String>,
    app_mode: AppMode,
    trust_prompt_mode: AppMode,
}

/// TS `createRuntime` 函数体。
async fn create_runtime_factory(
    options: CreateAgentSessionRuntimeFactoryOptions,
    captures: CreateRuntimeCaptures,
) -> Result<CreateAgentSessionRuntimeResult, AgentSessionRuntimeError> {
    let CreateAgentSessionRuntimeFactoryOptions {
        cwd,
        agent_dir,
        session_manager,
        session_start_event,
        project_trust_context,
    } = options;
    // 阶段 1：项目信任。`is_initial_runtime` 区分首次创建与会话替换
    // （`session_start_event.is_none()`）；两个共享槽位分别收集
    // 信任回调产生的 Warning 诊断与无法通过回调签名返回的错误（见下方说明）。
    let is_initial_runtime = session_start_event.is_none();

    let project_trust_diagnostics: Arc<Mutex<Vec<AgentSessionRuntimeDiagnostic>>> =
        Arc::new(Mutex::new(Vec::new()));
    // TS 的 `resolveProjectTrust` 回调里 `trustStore.get/set` 抛出的异常会让 `reload()`
    // 拒绝，进而 `createAgentSessionServices`/`createRuntime` 拒绝（main 未捕获 → 退出 1）。
    // kiana-resources 的回调签名返回 `bool`（所有单元共用），因此这里把第一个错误存进槽位，
    // 等 reload 返回后立刻以 `createRuntime` 错误的形式抛出；不计入 diagnostics。
    let project_trust_error: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

    // 只有“没有 CLI 覆盖、没有缓存结论、且项目含需要信任的资源”时，才把信任决定推迟到
    // 资源加载阶段（可能弹选择器）；其余情况在此直接得出结论，优先级为
    // 缓存 > 显式覆盖 >（无需信任的资源 ⇒ 受信，否则读 trust store）。
    let cached_project_trust = lock_map(&captures.project_trust_by_cwd).get(&cwd).copied();
    let has_trust_requiring_resources = has_trust_requiring_project_resources(&cwd);
    let should_resolve_project_trust = captures.parsed.project_trust_override.is_none()
        && cached_project_trust.is_none()
        && has_trust_requiring_resources;

    let project_trusted = if should_resolve_project_trust {
        false
    } else if let Some(cached) = cached_project_trust {
        cached
    } else if let Some(override_value) = captures.parsed.project_trust_override {
        override_value
    } else {
        !has_trust_requiring_resources
            || captures
                .trust_store
                .get(&cwd)
                .map_err(|error| AgentSessionRuntimeError::Other(error.to_string()))?
                .unwrap_or(false)
    };

    let runtime_settings_manager = SettingsManager::create_with_options(
        &cwd,
        &agent_dir,
        SettingsManagerCreateOptions {
            project_trusted: Some(project_trusted),
        },
    );
    // 进程级默认 MCP 沙箱必须在真正加载扩展之前安装，但必须用**信任结论已知**的
    // manager：启动期 `SettingsManager::create` 默认信任项目，会把未受信仓库里的
    // `permissions.mode: bypass` 应用到所有 stdio MCP 子进程。这里 `project_trusted`
    // 为 false 时项目层被忽略，且 managed/企业层照常生效（fail-closed）。
    install_default_mcp_sandbox_for_permissions(&captures.parsed, &runtime_settings_manager, &cwd);
    // 需要询问的情况下 `project_trusted` 为 `false`：本次加载不应用项目设置。确认结果
    // 随后写入 `project_trust_by_cwd`；后续对同一 cwd 的工厂调用可以用缓存值重建实例。

    // 阶段 2：只有需要解析信任时才安装回调。回调在资源加载过程中被调用，结论写入共享
    // 缓存，并可能产生 Warning 诊断。回调签名不能返回 `Result`，失败只能记进
    // `project_trust_error`，由本函数在 `create_agent_session_services` 返回后优先转成
    // createRuntime 错误（对应 TS 在 reload 中途抛异常）。
    let resource_loader_reload_options = if should_resolve_project_trust {
        let trust_cwd = cwd.clone();
        let trust_store = captures.trust_store.clone();
        let startup_settings_manager = captures.startup_settings_manager.clone();
        let project_trust_by_cwd = Arc::clone(&captures.project_trust_by_cwd);
        let trust_diagnostics = Arc::clone(&project_trust_diagnostics);
        let trust_error = Arc::clone(&project_trust_error);
        let trust_override = captures.parsed.project_trust_override;
        let default_project_trust = Some(startup_settings_manager.get_default_project_trust());
        let app_mode = captures.app_mode;
        let trust_prompt_mode = captures.trust_prompt_mode;
        let output = captures.output.clone();
        let supplied_context = project_trust_context.clone();
        Some(ResourceLoaderReloadOptions {
            resolve_project_trust: Some(Box::new(move |extensions_result| {
                let trust_cwd = trust_cwd.clone();
                let trust_store = trust_store.clone();
                let startup_settings_manager = startup_settings_manager.clone();
                let project_trust_by_cwd = Arc::clone(&project_trust_by_cwd);
                let trust_diagnostics = Arc::clone(&trust_diagnostics);
                let trust_error = Arc::clone(&trust_error);
                let output = output.clone();
                let supplied_context = supplied_context.clone();
                Box::pin(async move {
                    let views = create_project_trust_context_views(ProjectTrustOptions {
                        cwd: trust_cwd.clone(),
                        mode: if is_initial_runtime {
                            trust_prompt_mode
                        } else {
                            app_mode
                        },
                        settings_manager: startup_settings_manager,
                        has_ui: is_initial_runtime && trust_prompt_mode == AppMode::Interactive,
                        output,
                    });
                    let (extension_context, config_context) = match supplied_context {
                        Some(context) => {
                            let config_context = ConfigProjectTrustContext {
                                has_ui: context.has_ui,
                                ui: Some(Arc::new(ProjectTrustAdapter::new(context.ui.clone()))),
                            };
                            (context, config_context)
                        }
                        None => (views.extension, views.config),
                    };
                    let emitter: Arc<dyn kiana_config::trust::project_trust::ProjectTrustEmitter> =
                        Arc::new(ProjectTrustEmitterAdapter::new(
                            extensions_result,
                            extension_context,
                        ));
                    let diagnostics = Arc::clone(&trust_diagnostics);
                    let resolved = resolve_project_trusted(ResolveProjectTrustedOptions {
                        cwd: trust_cwd.clone(),
                        trust_store,
                        trust_override,
                        default_project_trust,
                        project_trust_context: config_context,
                        emit_project_trust: Some(emitter),
                        on_extension_error: Some(Arc::new(move |message: &str| {
                            lock_diagnostics(&diagnostics).push(
                                AgentSessionRuntimeDiagnostic::new(
                                    DiagnosticKind::Warning,
                                    message.to_string(),
                                ),
                            );
                        })),
                    })
                    .await;
                    let trusted = match resolved {
                        Ok(trusted) => trusted,
                        Err(error) => {
                            // TS 在这里抛出；记录首个错误，由 `create_runtime_factory`
                            // 在 reload 返回后转成 createRuntime 的拒绝（不写 `Aborted`
                            // 类诊断，也不缓存该 cwd 的信任结果）。
                            let mut slot = lock_option(&trust_error);
                            if slot.is_none() {
                                *slot = Some(error.to_string());
                            }
                            return false;
                        }
                    };
                    lock_map(&project_trust_by_cwd).insert(trust_cwd, trusted);
                    trusted
                })
            })),
        })
    } else {
        None
    };

    // 阶段 3：创建会话服务（设置、模型运行时、资源加载器）。CLI 的扩展/技能/提示词/
    // 主题路径在这一步并入资源加载；`--no-mcp` 通过禁用内置 `"mcp"` 扩展实现。
    let services = create_agent_session_services(CreateAgentSessionServicesOptions {
        cwd: cwd.clone(),
        agent_dir: Some(agent_dir.clone()),
        settings_manager: Some(runtime_settings_manager),
        model_runtime: None,
        model_runtime_signal: Some(abort_scope_with_timeout(15_000)),
        extension_flag_values: Some(captures.parsed.unknown_flags.clone()),
        resource_loader_options: Some(DefaultResourceLoaderOptions {
            additional_extension_paths: captures.resolved_extension_paths,
            additional_skill_paths: captures.resolved_skill_paths,
            additional_prompt_template_paths: captures.resolved_prompt_template_paths,
            additional_theme_paths: captures.resolved_theme_paths,
            no_extensions: captures.parsed.no_extensions,
            disabled_builtin_extensions: if captures.parsed.no_mcp {
                vec!["mcp".to_string()]
            } else {
                Vec::new()
            },
            no_skills: captures.parsed.no_skills,
            no_prompt_templates: captures.parsed.no_prompt_templates,
            no_themes: captures.parsed.no_themes,
            no_context_files: captures.parsed.no_context_files,
            system_prompt: captures.parsed.system_prompt.clone(),
            append_system_prompt: captures.parsed.append_system_prompt.clone(),
            extension_factories: captures.extension_factories,
            ..Default::default()
        }),
        resource_loader_reload_options,
    })
    .await;
    // 项目信任解析抛错优先于 reload 自身的失败（TS 在 reload 中途抛出，后续步骤不再执行）。
    if let Some(message) = lock_option(&project_trust_error).take() {
        return Err(AgentSessionRuntimeError::Other(message));
    }
    let services = services.map_err(|error| AgentSessionRuntimeError::Other(error.to_string()))?;

    let settings_manager = services.settings_manager.clone();
    let model_runtime = services.model_runtime.clone();
    let resource_loader = Arc::clone(&services.resource_loader);

    // 阶段 4：汇总诊断。顺序为信任回调 Warning → 服务诊断 → 设置诊断 → 扩展加载
    // error/warning；`main` 会把这份列表与启动设置诊断合并去重后展示。
    let mut diagnostics: Vec<AgentSessionRuntimeDiagnostic> =
        lock_diagnostics(&project_trust_diagnostics).clone();
    diagnostics.extend(services.diagnostics.iter().cloned());
    diagnostics.extend(settings_diagnostics_to_runtime(
        collect_settings_diagnostics(&settings_manager),
    ));
    // 配置分层（System/Managed/Enterprise/User/Project/Local）的加载与严格诊断。
    // 与 `Core::new` 的权限/执行策略接线共用同一套层组装逻辑；这里只负责让启动时
    // 就能看到 managed 文件错误与未知字段，而不是等第一次权限判断才暴露。
    diagnostics.extend(
        kiana_session::agent_session::session_config_diagnostics(&settings_manager, &cwd)
            .into_iter()
            .map(|message| AgentSessionRuntimeDiagnostic::new(DiagnosticKind::Warning, message)),
    );
    let extensions = resource_loader.get_extensions();
    for error in &extensions.errors {
        diagnostics.push(AgentSessionRuntimeDiagnostic::new(
            DiagnosticKind::Error,
            format!(
                "Failed to load extension \"{}\": {}",
                error.path, error.error
            ),
        ));
    }
    if let Some(warnings) = &extensions.warnings {
        for warning in warnings {
            diagnostics.push(AgentSessionRuntimeDiagnostic::new(
                DiagnosticKind::Warning,
                format!(
                    "Extension package \"{}\": {}",
                    warning.path, warning.warning
                ),
            ));
        }
    }

    // 阶段 5：解析 model scope。`--models` 优先于设置里的 `enabledModels`；没有模式时
    // 不做 scope 解析，`scoped_models` 保持为空。scope 解析的 warning 直接写 stderr。
    let model_patterns = captures
        .parsed
        .models
        .clone()
        .or_else(|| settings_manager.get_enabled_models());
    let scoped_models = match model_patterns {
        Some(patterns) if !patterns.is_empty() => {
            let resolved = resolve_model_scope_with_diagnostics(
                &patterns,
                &model_runtime,
                AuthOperationOptions {
                    signal: Some(abort_scope_with_timeout(15_000)),
                },
            )
            .await
            .map_err(|error| AgentSessionRuntimeError::Other(error.to_string()))?;
            let style = Style::detect();
            for diagnostic in &resolved.diagnostics {
                captures
                    .output
                    .write_line_stderr(&style.yellow(&format!("Warning: {}", diagnostic.message)));
            }
            resolved.scoped_models
        }
        _ => Vec::new(),
    };

    // 阶段 6：决定会话的模型/思考级别/工具过滤（规则见 `build_session_options`），并把
    // 它产生的诊断并入列表。`has_existing_session` 以“会话上下文是否已有消息”判断。
    let has_existing_session = !session_manager.build_session_context().messages.is_empty();
    let built = build_session_options(
        &captures.parsed,
        &scoped_models,
        has_existing_session,
        &model_runtime,
        &settings_manager,
    );
    diagnostics.extend(built.diagnostics);

    // 阶段 7：`--api-key` 只能设置到已经选出的模型上；没有模型时只产生一条错误诊断，
    // 不会设置任何密钥。
    if let Some(api_key) = js_truthy(captures.parsed.api_key.as_deref()).map(str::to_string) {
        match built.model.as_ref() {
            None => diagnostics.push(AgentSessionRuntimeDiagnostic::new(
                DiagnosticKind::Error,
                "--api-key requires a model to be specified via --model, --provider/--model, or --models",
            )),
            Some(model) => {
                model_runtime
                    .set_runtime_api_key(
                        &model.provider,
                        &api_key,
                        AuthOperationOptions::default(),
                    )
                    .await
                    .map_err(|error| AgentSessionRuntimeError::Other(error.to_string()))?;
            }
        }
    }

    // 阶段 8：创建会话本身。`services` 按值交给 `create_agent_session_from_services`，
    // 因此先 clone 一份用于返回结果；会话创建失败同样以 `AgentSessionRuntimeError` 上抛。
    let services_for_result = services.clone();
    let created = create_agent_session_from_services(CreateAgentSessionFromServicesOptions {
        services,
        session_manager,
        session_start_event,
        model: built.model,
        thinking_level: built.thinking_level,
        scoped_models: built.scoped_models,
        tools: built.tools,
        exclude_tools: built.exclude_tools,
        no_tools: built.no_tools,
        custom_tools: Vec::new(),
        loop_guard: built.loop_guard,
        budget_config: built.budget_config,
    })
    .await
    .map_err(|error| AgentSessionRuntimeError::Other(error.to_string()))?;

    // 显式 `--thinking` 或由 `--model` 解析出的级别在会话创建后再应用一次；
    // `persist: None` 表示不把它写回设置的默认值。只在会话确实有模型时执行。
    let cli_thinking_override = captures.parsed.thinking.is_some() || built.cli_thinking_from_model;
    if created.session.model().is_some() && cli_thinking_override {
        created.session.set_thinking_level(
            created.session.thinking_level(),
            kiana_session::agent_session::ModelMutationOptions { persist: None },
        );
    }

    // 命令行权限覆盖：`/new`、`/fork`、会话切换/导入都会经过这里重建会话，
    // 必须在工厂内应用，否则首次会话之外的会话会丢失 `--permission-mode` /
    // `--allow` / `--deny` / `--add-dir`。
    apply_cli_permission_overrides(&created.session, &captures.parsed);

    Ok(CreateAgentSessionRuntimeResult {
        session: created.session,
        extensions_result: created.extensions_result,
        model_fallback_message: created.model_fallback_message,
        services: services_for_result,
        diagnostics,
    })
}

/// 把 CLI 权限覆盖叠加到刚创建的会话上。
///
/// settings/env 显式关闭权限时，单独的 `--allow`/`--deny`/`--add-dir` 不能把引擎
/// 重新建出来（只有 `--permission-mode` 可以），否则 `permissions.off` 会被无提示地
/// 改回 Default 模式。
fn apply_cli_permission_overrides(
    session: &kiana_session::agent_session::AgentSession,
    args: &Args,
) {
    if let Some(mode) = args.permission_mode {
        session.set_permission_mode(mode);
    }
    // `--add-dir` 改变的是沙箱可写根，与权限层是否启用无关：先应用再判断 rules。
    // 一次批量应用，避免每个目录都重建全部工具定义。
    if !args.add_dirs.is_empty() {
        session.add_permission_directories(args.add_dirs.iter().map(PathBuf::from));
    }
    if session.permissions_disabled() && args.permission_mode.is_none() {
        return;
    }
    let rules = args
        .permission_allow
        .iter()
        .filter_map(|spec| parse_cli_rule(spec, RuleDecision::Allow))
        .chain(
            args.permission_deny
                .iter()
                .filter_map(|spec| parse_cli_rule(spec, RuleDecision::Deny)),
        )
        .collect::<Vec<_>>();
    if !rules.is_empty() {
        session.add_permission_rules(rules);
    }
}

/// 权限模式 → MCP stdio 子进程的沙箱策略。
///
/// - `read-only`：`SandboxPolicy::read_only()`；
/// - `bypass`：`SandboxPolicy::danger_full_access()`（权限层已放行，沙箱不再额外限制）；
/// - `default` / `accept-edits`：工作区可写，`--add-dir` 追加为可写根。
fn mcp_sandbox_policy_for_mode(
    mode: PermissionMode,
    cwd: &Path,
    add_dirs: &[PathBuf],
) -> SandboxPolicy {
    match mode {
        PermissionMode::ReadOnly => SandboxPolicy::read_only(),
        PermissionMode::BypassPermissions => SandboxPolicy::danger_full_access(),
        PermissionMode::Default | PermissionMode::AcceptEdits => {
            let mut roots = Vec::with_capacity(add_dirs.len() + 1);
            roots.push(cwd.to_path_buf());
            for dir in add_dirs {
                if !roots.contains(dir) {
                    roots.push(dir.clone());
                }
            }
            SandboxPolicy::workspace_write_with_roots(roots)
        }
    }
}

/// 按有效权限模式解析进程级默认 MCP 沙箱策略；`None` 表示不安装。
///
/// 用户显式设置的 `KIANA_SANDBOX_POLICY`（trim 后非空）优先于权限模式；`KIANA_SANDBOX`
/// 显式关闭沙箱（`none` 及同义拼写）时同样不安装，保持 `SandboxManager::from_env` 的
/// 既有语义。两个 env 值由调用方传入，便于单测覆盖而不修改进程环境。
fn resolve_mcp_sandbox_policy(
    mode: PermissionMode,
    cwd: &Path,
    add_dirs: &[PathBuf],
    sandbox_policy_env: Option<&str>,
    sandbox_env: Option<&str>,
) -> Option<SandboxPolicy> {
    if sandbox_policy_env.is_some_and(|value| !value.trim().is_empty()) {
        return None;
    }
    let disabled = sandbox_env.is_some_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "none" | "off" | "disabled" | "false"
        )
    });
    if disabled {
        return None;
    }
    Some(mcp_sandbox_policy_for_mode(mode, cwd, add_dirs))
}

/// 把 CLI 权限参数转换成权限引擎的覆盖项（与 `apply_cli_permission_overrides` 同一来源）。
fn permission_flags_from_args(args: &Args) -> PermissionFlags {
    PermissionFlags {
        mode: args.permission_mode,
        allow: args.permission_allow.clone(),
        deny: args.permission_deny.clone(),
        add_dirs: args.add_dirs.iter().map(PathBuf::from).collect(),
    }
}

/// 在创建会话运行时之前安装进程级默认 MCP 沙箱。
///
/// MCP 内置扩展用 `McpExtensionOptions::default()` 构造，拿不到会话级 manager；这里按
/// 启动期有效权限模式（settings 的 `permissions` + CLI flags，复用
/// `PermissionEngine::from_settings_and_flags`）安装默认值，供
/// `create_default_transport` 在显式 manager 之后、`KIANA_SANDBOX_POLICY` 之前使用。
///
/// `OnceLock` 只接受第一次安装：同一进程多次 `run_main` 时沿用第一次的 manager。
fn install_default_mcp_sandbox_for_permissions(
    args: &Args,
    settings_manager: &SettingsManager,
    cwd: &str,
) {
    if kiana_builtin_extensions::default_mcp_sandbox().is_some() {
        return;
    }
    let flags = permission_flags_from_args(args);
    let permission_settings = settings_manager.get_settings().raw("permissions");
    let mode = PermissionEngine::from_settings_and_flags(cwd, permission_settings.as_ref(), &flags)
        .map(|engine| engine.mode())
        // 权限层显式关闭（settings/env 写 `off`）时仍按 fail-closed 的 default 模式安装。
        .unwrap_or_default();
    let sandbox_policy_env =
        std::env::var_os("KIANA_SANDBOX_POLICY").map(|value| value.to_string_lossy().into_owned());
    let sandbox_env =
        std::env::var_os("KIANA_SANDBOX").map(|value| value.to_string_lossy().into_owned());
    let Some(policy) = resolve_mcp_sandbox_policy(
        mode,
        Path::new(cwd),
        &flags.add_dirs,
        sandbox_policy_env.as_deref(),
        sandbox_env.as_deref(),
    ) else {
        return;
    };
    kiana_builtin_extensions::install_default_mcp_sandbox(Arc::new(SandboxManager::new(
        policy, cwd,
    )));
}

// ============================================================================
// 小工具
// ============================================================================

/// 进程当前目录；读取失败时退回 `"."`。
fn process_cwd() -> String {
    std::env::current_dir()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|_| ".".to_string())
}

/// `InlineExtension` 的逐字段克隆。
///
/// `kiana-ext-api` 的 [`InlineExtension`] 没有实现 `Clone`（它的工厂字段是 `Arc`，克隆是
/// 安全的）。TS 的 `extensionFactories` 数组在每次 `createRuntime` 里被展开复用，Rust 侧
/// 用本函数达到同样效果，不修改冻结的 `kiana-ext-api` 类型。
fn clone_inline_extension(extension: &InlineExtension) -> InlineExtension {
    match extension {
        InlineExtension::Factory(factory) => InlineExtension::Factory(Arc::clone(factory)),
        InlineExtension::Named {
            name,
            factory,
            hidden,
            replaceable,
            builtin,
        } => InlineExtension::Named {
            name: name.clone(),
            factory: Arc::clone(factory),
            hidden: *hidden,
            replaceable: *replaceable,
            builtin: *builtin,
        },
    }
}

/// [`clone_inline_extension`] 的列表版本。
fn clone_inline_extensions(extensions: &[InlineExtension]) -> Vec<InlineExtension> {
    extensions.iter().map(clone_inline_extension).collect()
}

/// `AgentSessionRuntimeDiagnostic` 列表的转换（settings → runtime）。
fn settings_diagnostics_to_runtime(
    diagnostics: Vec<kiana_config::settings::diagnostics::SettingsDiagnostic>,
) -> Vec<AgentSessionRuntimeDiagnostic> {
    diagnostics
        .into_iter()
        .map(|diagnostic| AgentSessionRuntimeDiagnostic::new(diagnostic.kind, diagnostic.message))
        .collect()
}

/// JS 真值语义：`undefined` 与空串都视为“未提供”。
fn js_truthy(value: Option<&str>) -> Option<&str> {
    value.filter(|value| !value.is_empty())
}

/// 取互斥锁；中毒（持锁线程 panic）时取回内部值继续。
///
/// 三个 `lock_*` 辅助函数共用这一策略。它们只保护启动期的内存缓存与诊断收集，每次都是
/// 单次 get/insert/push，没有需要整体回滚的复合不变量；TS 也没有“中毒”概念，因此让启动
/// 因一次无关 panic 直接失败并不合适。
fn lock_map<T>(mutex: &Mutex<HashMap<String, T>>) -> std::sync::MutexGuard<'_, HashMap<String, T>> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 同 [`lock_map`] 的中毒处理策略。
fn lock_diagnostics(
    mutex: &Mutex<Vec<AgentSessionRuntimeDiagnostic>>,
) -> std::sync::MutexGuard<'_, Vec<AgentSessionRuntimeDiagnostic>> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 同 [`lock_map`] 的中毒处理策略。
fn lock_option<T>(mutex: &Mutex<Option<T>>) -> std::sync::MutexGuard<'_, Option<T>> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 新建带超时的 [`AbortScope`]：到期后在 tokio 上 abort。
///
/// 立即返回 scope，计时在独立 tokio 任务里进行，因此调用方必须处于 tokio 运行时中。
/// 计时任务持有 scope 的一个 clone，不会因调用方提前 abort 或丢弃 scope 而提前结束；
/// 它最多活到 `timeout_ms` 到期。
fn abort_scope_with_timeout(timeout_ms: u64) -> AbortScope {
    let scope = AbortScope::new();
    let timer = scope.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(timeout_ms)).await;
        timer.abort();
    });
    scope
}

/// `Error: {message}` 并返回退出码 1。
fn report_session_error(
    output: &Output,
    error: &kiana_session_core::session_manager::SessionManagerError,
) -> CliExit {
    output.write_line_stderr(&Style::detect().red(&format!("Error: {error}")));
    CliExit::new(1)
}

/// `Error: {message}` 并返回退出码 1（会话列表错误）。
fn report_list_error(
    output: &Output,
    error: &kiana_session_core::session_manager::SessionListError,
) -> CliExit {
    output.write_line_stderr(&Style::detect().red(&format!("Error: {error}")));
    CliExit::new(1)
}

/// 尽力冲刷进程的 stdout/stderr（TS 的 `writableLength` 等待在 Rust 没有对应物）。
fn flush_stdio() {
    use std::io::Write;
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
}

/// cron 启动配置：环境变量 `KIANA_CRON` 或 settings `cron`。
///
/// - settings 显式配置了 `cron` 时**以 settings 为准**（`false` / `enabled: false`
///   即使 `KIANA_CRON=1` 也保持关闭）；
/// - settings 未配置时，`KIANA_CRON=1/true/yes/on` 启用，默认目录
///   `<agent_dir>/cron`、30s tick；
/// - settings `cron: true` 启用；`cron: { "dir": "...", "intervalSecs": 30 }` 可覆盖
///   目录与间隔（`enabled` 缺省为 true）。
fn cron_startup_config(settings_manager: &SettingsManager) -> Option<(PathBuf, Duration)> {
    // 真值集合与文档一致：`0`/`false`/`off` 等非空字符串必须表示关闭，不能沿用
    // JS 字符串真值（任何非空串都为真）。
    let env_enabled = std::env::var("KIANA_CRON").ok().is_some_and(|value| {
        let value = value.trim().to_ascii_lowercase();
        matches!(value.as_str(), "1" | "true" | "yes" | "on")
    });
    let value = settings_manager.get_settings().raw("cron");
    let (enabled, root, interval_secs) = match value.as_ref() {
        Some(serde_json::Value::Bool(enabled)) => (*enabled, None, None),
        Some(serde_json::Value::Object(object)) => {
            let enabled = object
                .get("enabled")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(true);
            let root = object
                .get("dir")
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.is_empty())
                .map(PathBuf::from);
            let interval = object
                .get("intervalSecs")
                .and_then(serde_json::Value::as_u64)
                .filter(|value| *value > 0);
            (enabled, root, interval)
        }
        // settings 没有显式配置 cron：只有环境变量能开启。
        _ => (env_enabled, None, None),
    };
    if !enabled {
        return None;
    }
    let root = root.unwrap_or_else(|| get_agent_dir().join("cron"));
    let interval = Duration::from_secs(interval_secs.unwrap_or(30));
    Some((root, interval))
}

/// 启动 cron loop + LocalSet worker；返回 shutdown sender（drop 即停止）。
async fn start_cron_background(
    session: kiana_session::agent_session::AgentSession,
    root: PathBuf,
    interval: Duration,
    output: Output,
) -> Result<tokio::sync::watch::Sender<bool>, String> {
    let (runner, receiver) = cron_turn_channel();
    let scripts = root.join("scripts");
    let runner: Arc<dyn crate::cron::CronTurnRunner> = runner;
    let dispatcher = Arc::new(SessionJobDispatcher::new(runner, Some(scripts.clone())));
    let scheduler =
        Arc::new(build_cron_scheduler(&root, dispatcher).map_err(|error| error.to_string())?);
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    tokio::task::spawn_local(run_cron_worker(session, receiver));
    tokio::task::spawn_local(run_cron_loop(
        scheduler,
        interval,
        shutdown_rx,
        move |report| match report {
            Ok(report)
                if !report.dispatched.is_empty()
                    || !report.failed.is_empty()
                    || !report.recovered.is_empty() =>
            {
                output.write_line_stderr(&format!(
                    "cron tick: dispatched {:?}, failed {:?}, recovered {:?}",
                    report.dispatched, report.failed, report.recovered
                ));
            }
            Ok(_) => {}
            Err(error) => {
                output.write_line_stderr(&format!("cron tick failed: {error}"));
            }
        },
    ));
    Ok(shutdown_tx)
}

/// `--serve`：把 `AgentSessionEvent` 暴露成 daemon 事件服务。
///
/// - 每条事件只序列化一次；事件类型取 serde 变体名（`agent_end`、`message_update` 等），
///   通道由 [`kiana_rpc_print::daemon::channel_for_event_type`] 决定（流式 `.delta` /
///   `.update` 为 volatile，其余 durable）。
/// - 始终要求 bearer token（`--serve-token` 或自动生成）；`/healthz` 例外。
/// - Ctrl-C 后中止监听任务并返回 0。
async fn run_daemon_mode(
    session: kiana_session::agent_session::AgentSession,
    parsed: &Args,
    output: Output,
) -> CliResult<i32> {
    use kiana_rpc_print::daemon::{DaemonConfig, bind_daemon, channel_for_event_type};
    use kiana_rpc_print::event_feed::EventFeed;

    let feed = EventFeed::new();
    let feed_for_events = feed.clone();
    let _unsubscribe = session.subscribe(std::sync::Arc::new(move |event| {
        let Ok(value) = serde_json::to_value(event) else {
            return;
        };
        let Some(event_type) = value
            .as_object()
            .and_then(|object| object.keys().next())
            .cloned()
        else {
            return;
        };
        let channel = channel_for_event_type(&event_type);
        let _ = feed_for_events.publish(channel, &event_type, &value);
    }));

    let token = parsed
        .serve_token
        .clone()
        .filter(|token| !token.is_empty())
        .unwrap_or_else(generate_daemon_token);
    let bind_addr = match parsed.serve_addr.as_deref() {
        Some(value) => match value.parse::<std::net::SocketAddr>() {
            Ok(address) => address,
            Err(error) => {
                output
                    .write_line_stderr(&format!("Error: invalid --serve-addr {value:?}: {error}"));
                return Ok(1);
            }
        },
        None => std::net::SocketAddr::from(([127, 0, 0, 1], 0)),
    };
    let config = DaemonConfig {
        bind_addr,
        auth_token: Some(token.clone()),
        ..Default::default()
    };
    let (address, handle) = match bind_daemon(feed, config).await {
        Ok(bound) => bound,
        Err(error) => {
            output.write_line_stderr(&format!(
                "Error: failed to start daemon on {bind_addr}: {error}"
            ));
            return Ok(1);
        }
    };
    output.write_line_stderr(&format!("Daemon listening on http://{address}"));
    output.write_line_stderr(&format!("Bearer token: {token}"));
    output.write_line_stderr(
        "Endpoints: GET /events (SSE or WebSocket), GET /events/latest?limit=N, GET /healthz",
    );

    match tokio::signal::ctrl_c().await {
        Ok(()) => output.write_line_stderr("Daemon shutting down."),
        Err(error) => {
            output.write_line_stderr(&format!("Warning: failed to listen for Ctrl-C: {error}"))
        }
    }
    handle.abort();
    Ok(0)
}

/// IDE 启动接线是否启用（`KIANA_IDE=0/false/no/off` 关闭，默认启用）。
fn ide_startup_enabled() -> bool {
    match std::env::var("KIANA_IDE") {
        Ok(value) => {
            let value = value.trim().to_ascii_lowercase();
            !matches!(value.as_str(), "0" | "false" | "no" | "off")
        }
        Err(_) => true,
    }
}

/// 生成 daemon token：优先读 `/dev/urandom`，失败时退回时间 + pid + 进程内计数器。
fn generate_daemon_token() -> String {
    use std::io::Read;

    let mut bytes = [0u8; 32];
    if std::fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .is_ok()
    {
        return bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    }
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let counter = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    format!("{nanos:032x}{:08x}{counter:016x}", std::process::id())
}

#[cfg(test)]
mod tests {
    //! 本文件内联的单测：锁定 TS 真值语义（空串等价于未提供）、模式判定优先级、
    //! print 输出模式映射，以及管道 stdin 的 EOF 探测（含普通文件重定向）。
    //! 会话选择与退出码等进程级行为在 `tests/session_id_readonly.rs` 等集成测试中覆盖。

    use super::*;

    fn args_with(overrides: impl FnOnce(&mut Args)) -> Args {
        let mut args = Args::default();
        overrides(&mut args);
        args
    }

    #[test]
    fn truthy_env_flag_matches_ts() {
        assert!(!is_truthy_env_flag(None));
        assert!(!is_truthy_env_flag(Some("")));
        assert!(is_truthy_env_flag(Some("1")));
        assert!(is_truthy_env_flag(Some("true")));
        assert!(is_truthy_env_flag(Some("TRUE")));
        assert!(is_truthy_env_flag(Some("Yes")));
        assert!(!is_truthy_env_flag(Some("0")));
        assert!(!is_truthy_env_flag(Some("no")));
    }

    #[test]
    fn resolve_app_mode_matches_ts_precedence() {
        let rpc = args_with(|args| args.mode = Some(Mode::Rpc));
        assert_eq!(resolve_app_mode(&rpc, true, true), AppMode::Rpc);

        let json = args_with(|args| args.mode = Some(Mode::Json));
        assert_eq!(resolve_app_mode(&json, true, true), AppMode::Json);

        let text = args_with(|args| args.mode = Some(Mode::Text));
        assert_eq!(resolve_app_mode(&text, true, true), AppMode::Interactive);
        assert_eq!(resolve_app_mode(&text, false, true), AppMode::Print);
        assert_eq!(resolve_app_mode(&text, true, false), AppMode::Print);

        let printed = args_with(|args| args.print = true);
        assert_eq!(resolve_app_mode(&printed, true, true), AppMode::Print);
    }

    #[test]
    fn plain_runtime_metadata_command_requires_no_mode_and_no_print() {
        assert!(is_plain_runtime_metadata_command(&args_with(|args| {
            args.help = true;
        })));
        assert!(is_plain_runtime_metadata_command(&args_with(|args| {
            args.list_models = Some(ListModelsArg::True);
        })));
        assert!(!is_plain_runtime_metadata_command(&args_with(|args| {
            args.help = true;
            args.print = true;
        })));
        assert!(!is_plain_runtime_metadata_command(&args_with(|args| {
            args.help = true;
            args.mode = Some(Mode::Json);
        })));
        assert!(!is_plain_runtime_metadata_command(&args_with(|_| {})));
    }

    #[test]
    fn print_output_mode_maps_json_only() {
        assert_eq!(to_print_output_mode(AppMode::Json), PrintOutputMode::Json);
        assert_eq!(to_print_output_mode(AppMode::Print), PrintOutputMode::Text);
        assert_eq!(to_print_output_mode(AppMode::Rpc), PrintOutputMode::Text);
    }

    /// TS: `if (!parsed.fork) return;` —— 显式空 `--fork ''` 不参与冲突校验。
    #[test]
    fn empty_fork_value_is_treated_as_absent() {
        let output = Output::capture();
        let parsed = args_with(|args| {
            args.fork = Some(String::new());
            args.session = Some("x".to_string());
        });
        assert_eq!(validate_fork_flags(&parsed, &output), Ok(()));
        assert_eq!(output.captured_stderr(), "");

        let parsed = args_with(|args| {
            args.fork = Some("source".to_string());
            args.session = Some("x".to_string());
        });
        let error = validate_fork_flags(&parsed, &output).expect_err("conflict must be rejected");
        assert_eq!(error.code, 1);
        assert!(
            output
                .captured_stderr()
                .contains("Error: --fork cannot be combined with --session")
        );
    }
    /// TS: `parsed.session ? "--session" : undefined` —— 显式空 `--session ''` 是假值，
    /// 不参与 `--fork` / `--session-id` 的冲突校验（main.ts:299-336）。
    #[test]
    fn empty_session_value_is_falsy_for_conflict_validation() {
        let output = Output::capture();
        let parsed = args_with(|args| {
            args.fork = Some("source".to_string());
            args.session = Some(String::new());
        });
        assert_eq!(validate_fork_flags(&parsed, &output), Ok(()));
        assert_eq!(output.captured_stderr(), "");

        let parsed = args_with(|args| {
            args.session_id = Some("abc".to_string());
            args.session = Some(String::new());
        });
        assert_eq!(validate_session_id_flags(&parsed, &output), Ok(()));
        assert_eq!(output.captured_stderr(), "");

        // 非空串仍然是冲突。
        let parsed = args_with(|args| {
            args.session_id = Some("abc".to_string());
            args.session = Some("x".to_string());
        });
        let error =
            validate_session_id_flags(&parsed, &output).expect_err("conflict must be rejected");
        assert_eq!(error.code, 1);
        assert!(
            output
                .captured_stderr()
                .contains("Error: --session-id cannot be combined with --session")
        );
    }

    /// TS 的 `promptConfirm` 在回答后 `rl.close()` 暂停管道 stdin；`readPipedStdin` 只有在
    /// 流尚未 EOF 时才能等到 `end`。EOF 判定用零超时 `poll(2)`：
    /// - 写端仍打开、暂无数据 → 不是 EOF（之后的数据/EOF 仍会到达）；
    /// - 写端已关闭但还有未读数据 → 不是 EOF（剩余数据仍要读）；
    /// - 写端已关闭且没有未读数据 → EOF（`end` 不会再触发）。
    #[cfg(unix)]
    #[test]
    fn stdin_eof_detection_matches_node_stream_end_semantics() {
        let mut fds = [0; 2];
        // SAFETY: `fds` 指向容量为 2 的有效数组。
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        let (read_fd, write_fd) = (fds[0], fds[1]);

        // 写端打开、无数据：不是 EOF。
        assert!(!fd_reached_eof(read_fd));

        // 有未读数据（写端已关闭）：不是 EOF。
        let payload = b"hello\n";
        // SAFETY: 缓冲区与 fd 均有效。
        assert_eq!(
            unsafe { libc::write(write_fd, payload.as_ptr().cast(), payload.len()) },
            payload.len() as isize
        );
        // SAFETY: 关闭后不再使用该 fd。
        unsafe { libc::close(write_fd) };
        assert!(!fd_reached_eof(read_fd));

        // 读空所有数据后：EOF。
        let mut buffer = [0u8; 8];
        // SAFETY: 缓冲区与 fd 均有效。
        let read = unsafe { libc::read(read_fd, buffer.as_mut_ptr().cast(), buffer.len()) };
        assert_eq!(read, payload.len() as isize);
        assert!(fd_reached_eof(read_fd));

        // SAFETY: 关闭后不再使用该 fd。
        unsafe { libc::close(read_fd) };
    }

    /// 重定向自普通文件时 `poll(2)` 永远报告可读，EOF 必须用文件偏移判断。
    #[cfg(unix)]
    #[test]
    fn stdin_eof_detection_handles_redirected_regular_files() {
        use std::os::fd::AsRawFd;

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("input.txt");
        std::fs::write(&path, b"y\nhello\n").expect("write input");
        let mut file = std::fs::File::open(&path).expect("open input");

        let mut answer = [0u8; 2];
        file.read_exact(&mut answer).expect("read answer");
        assert!(
            !fd_reached_eof(file.as_raw_fd()),
            "remaining bytes mean the stream is not at EOF"
        );

        let mut rest = Vec::new();
        file.read_to_end(&mut rest).expect("read rest");
        assert_eq!(rest, b"hello\n");
        assert!(
            fd_reached_eof(file.as_raw_fd()),
            "reading a regular file to its end is EOF"
        );
    }

    /// cron 相关测试会改 `KIANA_CRON`（进程级环境变量），必须串行。
    static CRON_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// `cron` 设置（settings.json 里的未知键）能开启 cron 后台并读取 dir/interval。
    #[test]
    fn cron_settings_object_enables_startup_with_dir_and_interval() {
        let _guard = CRON_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut settings = Settings::default();
        settings.extra.insert(
            "cron".to_string(),
            serde_json::json!({
                "enabled": true,
                "dir": "/tmp/kiana-cron-test",
                "intervalSecs": 7,
            }),
        );
        let manager = SettingsManager::in_memory(settings);
        let (root, interval) = cron_startup_config(&manager).expect("cron enabled via settings");
        assert_eq!(root, PathBuf::from("/tmp/kiana-cron-test"));
        assert_eq!(interval, Duration::from_secs(7));
    }

    fn with_cron_env(value: Option<&str>, settings: Settings) -> Option<(PathBuf, Duration)> {
        match value {
            Some(value) => unsafe { std::env::set_var("KIANA_CRON", value) },
            None => unsafe { std::env::remove_var("KIANA_CRON") },
        }
        let result = cron_startup_config(&SettingsManager::in_memory(settings));
        unsafe { std::env::remove_var("KIANA_CRON") };
        result
    }

    /// 显式 `cron: false` 必须压过环境变量：事故后禁用 cron 的配置不能被 shell 里
    /// 残留的 `KIANA_CRON=1` 悄悄重新打开。
    #[test]
    fn explicit_settings_disable_overrides_the_environment() {
        let _guard = CRON_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut settings = Settings::default();
        settings
            .extra
            .insert("cron".to_string(), serde_json::Value::Bool(false));
        assert!(with_cron_env(Some("1"), settings).is_none());

        let mut settings = Settings::default();
        settings
            .extra
            .insert("cron".to_string(), serde_json::json!({ "enabled": false }));
        assert!(with_cron_env(Some("1"), settings).is_none());
    }

    /// settings 没有 cron 键时，环境变量仍然可以开启；显式 true 也可开启。
    #[test]
    fn environment_enables_cron_only_when_settings_say_nothing() {
        let _guard = CRON_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert!(with_cron_env(Some("on"), Settings::default()).is_some());
        assert!(with_cron_env(Some("0"), Settings::default()).is_none());

        let mut settings = Settings::default();
        settings
            .extra
            .insert("cron".to_string(), serde_json::Value::Bool(true));
        assert!(with_cron_env(None, settings).is_some());
    }

    // ------------------------------------------------------------------
    // MCP stdio 沙箱：权限模式 → SandboxPolicy
    // ------------------------------------------------------------------

    #[test]
    fn read_only_permission_mode_maps_to_read_only_sandbox() {
        assert_eq!(
            mcp_sandbox_policy_for_mode(
                PermissionMode::ReadOnly,
                Path::new("/workspace/project"),
                &[],
            ),
            SandboxPolicy::read_only()
        );
    }

    #[test]
    fn default_and_accept_edits_map_to_workspace_write_with_add_dirs() {
        let cwd = Path::new("/workspace/project");
        let add_dirs = [
            PathBuf::from("/workspace/shared"),
            PathBuf::from("/workspace/other"),
        ];
        for mode in [PermissionMode::Default, PermissionMode::AcceptEdits] {
            let policy = mcp_sandbox_policy_for_mode(mode, cwd, &add_dirs);
            assert_eq!(
                policy,
                SandboxPolicy::workspace_write_with_roots(vec![
                    cwd.to_path_buf(),
                    add_dirs[0].clone(),
                    add_dirs[1].clone(),
                ])
            );
            assert_eq!(policy.writable_roots()[0], cwd);
            assert!(policy.writable_roots().contains(&add_dirs[0]));
        }
    }

    #[test]
    fn bypass_permission_mode_maps_to_danger_full_access() {
        assert_eq!(
            mcp_sandbox_policy_for_mode(
                PermissionMode::BypassPermissions,
                Path::new("/workspace/project"),
                &[PathBuf::from("/workspace/shared")],
            ),
            SandboxPolicy::danger_full_access()
        );
    }

    #[test]
    fn explicit_sandbox_policy_env_wins_over_permission_mode() {
        let cwd = Path::new("/workspace/project");
        // 非空值：不安装，交给 SandboxManager::from_env。
        assert_eq!(
            resolve_mcp_sandbox_policy(
                PermissionMode::ReadOnly,
                cwd,
                &[],
                Some("workspace-write"),
                None
            ),
            None
        );
        // 空串/空白等价于未设置（kiana-sandbox 的既有语义）。
        assert_eq!(
            resolve_mcp_sandbox_policy(PermissionMode::ReadOnly, cwd, &[], Some("   "), None),
            Some(SandboxPolicy::read_only())
        );
    }

    #[test]
    fn sandbox_none_disables_the_process_default() {
        for value in ["none", "off", "disabled", "false", "NONE"] {
            assert_eq!(
                resolve_mcp_sandbox_policy(
                    PermissionMode::Default,
                    Path::new("/workspace/project"),
                    &[PathBuf::from("/workspace/shared")],
                    None,
                    Some(value),
                ),
                None,
                "KIANA_SANDBOX={value} must not install a manager"
            );
        }
    }

    /// 未受信仓库里的 `permissions.mode` 不能影响进程级 MCP 沙箱：`project_trusted`
    /// 为 false 时 manager 必须忽略项目层（受信时才生效）。
    #[test]
    fn untrusted_project_permissions_do_not_loosen_mcp_sandbox() {
        let dir = tempfile::tempdir().expect("tempdir");
        let project = dir.path().join(".kiana");
        std::fs::create_dir_all(&project).expect("mkdir");
        std::fs::write(
            project.join("settings.json"),
            r#"{"permissions":{"mode":"bypass"}}"#,
        )
        .expect("write settings");
        let cwd = dir.path().to_string_lossy();
        let agent_dir = dir.path().join("agent").to_string_lossy().into_owned();
        let mode_for = |trusted: bool| {
            let manager = SettingsManager::create_with_options(
                cwd.as_ref(),
                &agent_dir,
                SettingsManagerCreateOptions {
                    project_trusted: Some(trusted),
                },
            );
            let raw = manager.get_settings().raw("permissions");
            PermissionEngine::from_settings_and_flags(
                cwd.as_ref(),
                raw.as_ref(),
                &PermissionFlags::default(),
            )
            .map(|engine| engine.mode())
            .unwrap_or_default()
        };
        assert_eq!(mode_for(false), PermissionMode::Default);
        assert_eq!(mode_for(true), PermissionMode::BypassPermissions);
        assert_eq!(
            mcp_sandbox_policy_for_mode(mode_for(false), dir.path(), &[]),
            SandboxPolicy::workspace_write_with_roots(vec![dir.path().to_path_buf()])
        );
    }

    /// 有效模式来自 settings + CLI flags：`PermissionEngine::from_settings_and_flags`
    /// 产出后才会进入沙箱策略映射（不复制权限规则）。
    #[test]
    fn settings_and_cli_flags_produce_the_effective_permission_mode() {
        let cwd = "/workspace/project";
        let settings = serde_json::json!({"mode": "read-only"});
        let flags = PermissionFlags::default();
        let engine = PermissionEngine::from_settings_and_flags(cwd, Some(&settings), &flags)
            .expect("settings permissions always create an engine");
        assert_eq!(engine.mode(), PermissionMode::ReadOnly);
        assert_eq!(
            resolve_mcp_sandbox_policy(engine.mode(), Path::new(cwd), &[], None, None),
            Some(SandboxPolicy::read_only())
        );

        // CLI `--permission-mode bypass` 覆盖 settings 的 read-only。
        let flags = PermissionFlags {
            mode: Some(PermissionMode::BypassPermissions),
            ..PermissionFlags::default()
        };
        let engine = PermissionEngine::from_settings_and_flags(cwd, Some(&settings), &flags)
            .expect("settings permissions always create an engine");
        assert_eq!(engine.mode(), PermissionMode::BypassPermissions);
    }
}
