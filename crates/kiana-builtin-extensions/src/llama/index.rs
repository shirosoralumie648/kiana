//! extensions/llama/index.ts。
//!
//! `llama_extension` 注册原生 `llama.cpp` provider 与 `/llama` 命令：非 TUI 模式只发
//! `"/llama is available in interactive mode"` 警告；TUI 里进入模型管理器（加载/卸载/下载/
//! Hugging Face 搜索），动作之后重读目录，连接错误交给 Retry/Close 循环。
//!
//! 逐字文案（spec §3.1/§3.4）：`Manage llama.cpp router models`、
//! `Configure llama.cpp with /login llama.cpp`、`{n} model is loaded` / `{n} models are loaded`、
//! `Unload all and load` / `Keep loaded and load` / `Cancel`、`Restoring previously loaded models`、
//! `Loaded {id}` / `Load started for {id}`、`Unloaded {id}`、`Downloaded {model}`、
//! `{id} is {status}`、`Model catalog refresh timed out.`。
//!
//! # 在系统中的位置与生命周期
//!
//! [`llama_extension`] 由 `crate::index::builtin_extensions`（`src/index.rs`）包装成内建扩展
//! 工厂 `llama.cpp`，资源 loader 在扩展加载时调用工厂。它向宿主注册两件东西：
//! 一个原生 provider（`ProviderRegistration::Native`）和 `/llama` 命令。provider 的模型
//! 目录不是静态表，而是本模块每次读取服务器后通过 `set_catalog` 写入的。
//!
//! # 一次 /llama 调用的执行流程
//!
//! 1. `configured_client` 从 model registry 解析 `llama.cpp` 的鉴权：没有结果就提示
//!    `Configure llama.cpp with /login llama.cpp` 并结束；`LLAMA_BASE_URL`（非空）优先于
//!    `auth.base_url`，经 `normalize_llama_server_url` 归一后构造客户端。
//! 2. `show_llama_ui` 打开自定义 TUI；`read_catalog` 读取服务器目录，失败时弹
//!    Retry/Close 对话框（选 Close 则整个命令返回）。
//! 3. `run_manager_loop` 展示模型列表并按动作分派：下载、卸载（先确认）、加载
//!    （必要时先卸载其它模型，取消/失败时恢复）、或对不可操作状态给出 warning。
//! 4. 每个动作结束后重新读取目录（Close 除外）；非连接类动作错误再用 error 通知。
//!
//! # 错误处理与约束
//!
//! - 目录刷新有 15 s 的 `AbortSignal.timeout` 等价物（`timeout_scope`）；中止时用
//!   `Model catalog refresh timed out.`，provider 自身的刷新错误原样返回（`sync_catalog`）。
//! - `read_catalog` 对**任何**错误都重试；文案只是经 `connection_error_message` 归一：
//!   连接类错误显示 `Could not connect to the server.`，其余显示原始 `message`。
//! - 动作错误不会中断管理器：先刷新目录，再决定是否 notify；连接类错误不重复通知
//!   （与 Retry/Close 对话框职责重叠，TS 用 `!isConnectionError` 过滤）。
//! - 命令处理器是 `LocalBoxFuture`，运行在扩展运行时的单线程 `LocalSet` 上
//!   （`ARCHITECTURE.md` §4.2）；Hugging Face 搜索回调是 `Rc<dyn Fn ...>`（非 `Send`），
//!   不能跨线程。

// `AiError` 是 `kiana-ai` 的共享错误契约（128 字节）；本模块的公开签名由 TS 的
// `Promise`/`throw` 语义直接映射而来，不装箱（与 `kiana-config`/`kiana-ai` 的同类模块一致）。
#![allow(clippy::result_large_err)]

use std::sync::Arc;
use std::time::Duration;

use kiana_ai::error::AiError;
use kiana_ai::models::ModelsRefreshOptions;
use kiana_ai::utils::abort::{AbortReason, AbortScope};
use kiana_ext_api::types::{
    ExtensionApi, ExtensionCommandContextApi, ExtensionMode, ProviderRegistration,
    RegisterCommandOptions,
};
use kiana_ext_api::ui::NotifyKind;

use super::client::{
    ListOptions, LlamaClient, LlamaModelInfo, format_bytes, normalize_llama_server_url,
};
use super::huggingface::{HuggingFaceClient, HuggingFaceGated, find_huggingface_token};
use super::provider::{LLAMA_PROVIDER_ID, LlamaProviderController, create_llama_provider};
use super::ui::{
    ConnectionOutcome, LlamaManagerAction, LlamaUi, RunProgressOutcome, RunWithProgressOptions,
    SearchModelsFn, run_with_progress, show_llama_ui,
};

/// `modelIsLoaded(model)`（sleeping 视为已加载，选中会唤醒）。
///
/// 本模块把 `"loaded"` 与 `"sleeping"` 都当作“已加载”：列表里选中它们会走卸载分支。
/// 其它状态（`unloaded`、`loading`、下载中或服务端新增的状态）不在这里命中。
fn model_is_loaded(model: &LlamaModelInfo) -> bool {
    model.status.value == "loaded" || model.status.value == "sleeping"
}

/// `isConnectionError(error)`。
///
/// 把 `name` 与 `message` 用空格拼起来转小写，再做子串匹配（`fetch failed` / `timeout` /
/// `network`）。因此名字或正文任一命中都算连接错误；这是启发式判断，不看
/// `AiErrorKind`，也不区分错误来自 llama.cpp 还是 Hugging Face 请求。
fn is_connection_error(error: &AiError) -> bool {
    let message = format!("{} {}", error.name, error.message).to_lowercase();
    message.contains("fetch failed") || message.contains("timeout") || message.contains("network")
}

/// `connectionErrorMessage(error)`。
///
/// 连接类错误统一显示 `Could not connect to the server.`；其余错误原样返回
/// `error.message`。
fn connection_error_message(error: &AiError) -> String {
    if is_connection_error(error) {
        "Could not connect to the server.".to_string()
    } else {
        error.message.clone()
    }
}

/// `parseHuggingFaceModel(value)`：只在第一个 `/` 之后找 `:`。
///
/// 这样 `owner/repo:Q4_K_M` 才拆成 `("owner/repo", Some("Q4_K_M"))`；`a:b/c` 里的冒号在
/// 第一个 `/` 之前，不参与拆分。没有 `/` 时从头开始找冒号（对应 TS 的
/// `indexOf("/") + 1` 在 `indexOf` 返回 -1 时从 0 开始）。量化部分是原样切片（可能为空
/// 字符串），由调用方按 JS 假值语义过滤。
fn parse_huggingface_model(value: &str) -> (String, Option<String>) {
    let start = value.find('/').map_or(0, |index| index + 1);
    match value[start..].find(':') {
        Some(relative) => {
            let colon = start + relative;
            (
                value[..colon].to_string(),
                Some(value[colon + 1..].to_string()),
            )
        }
        None => (value.to_string(), None),
    }
}

/// `AbortSignal.timeout(ms)` 的等价物（`TimeoutError` 文案与 Node 一致）。
///
/// 起一个后台任务在 `ms` 后调用 `abort_with`，返回的 scope 由调用方继续使用。定时任务
/// 不会被提前取消：即使操作先完成，它仍会到期执行；此时没有等待者，只写入 reason。
/// 返回的 scope 与其克隆共享同一个取消状态，因此同一个 scope 可以同时交给目录请求与
/// model registry 刷新（`sync_catalog` 正是这样共享 15 s 预算的）。
fn timeout_scope(milliseconds: u64) -> AbortScope {
    let scope = AbortScope::new();
    let inner = scope.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(milliseconds)).await;
        inner.abort_with(AbortReason {
            name: "TimeoutError".to_string(),
            message: "The operation was aborted due to timeout".to_string(),
        });
    });
    scope
}

/// `configuredClient(ctx)`。
///
/// `Ok(None)` 表示“尚未配置”：已经用 warning 提示用户执行
/// `/login llama.cpp`，调用方直接结束命令。`Err` 表示 registry 解析失败（命令失败、
/// 配置值无法求值等），错误分类统一改成 `AiErrorKind::Other`，但保留原 `name`/`message`。
///
/// URL 取值顺序与 TS 的真值判断一致：`env.LLAMA_BASE_URL` 非空时用它，否则用
/// `auth.base_url`，再否则用空串；随后 `normalize_llama_server_url` 可能因非法 URL 或
/// 非 http(s) 协议返回 `Err`。api-key 原样交给 `LlamaClient`（空字符串不会发送，
/// 见 `llama/client.rs` 的 `authorization_header`）。
async fn configured_client(
    ctx: &dyn ExtensionCommandContextApi,
) -> Result<Option<LlamaClient>, AiError> {
    let result = ctx
        .model_registry()
        .get_provider_auth(LLAMA_PROVIDER_ID)
        .await
        .map_err(|error| {
            AiError::new(kiana_ai::error::AiErrorKind::Other, error.message).with_name(error.name)
        })?;
    let Some(result) = result else {
        ctx.ui().notify(
            "Configure llama.cpp with /login llama.cpp",
            Some(NotifyKind::Warning),
        );
        return Ok(None);
    };
    let configured_url = result
        .env
        .as_ref()
        .and_then(|env| env.get("LLAMA_BASE_URL"));
    let raw = match configured_url {
        Some(url) if !url.is_empty() => url.as_str(),
        _ => result.auth.base_url.as_deref().unwrap_or(""),
    };
    let server_url = normalize_llama_server_url(raw)?;
    Ok(Some(LlamaClient::new(&server_url, result.auth.api_key)?))
}

/// `syncCatalog(ctx, client, catalog?)`。
///
/// 先确定 `current`：调用方传入的目录（下载流程已拿到最终目录时用）或向服务器
/// `GET /models` 拉取的新目录。把 `current` 写进 provider（`routerAutoload = false`，
/// 与 TS 一样不启用 router 自动加载），再让 model registry 只刷新 `llama.cpp` 并强制
/// 允许联网——`/llama` 已经联系过配置的服务器，所以即使进程处于 offline 模式也保持联网。
///
/// 返回的始终是 `current`，不是 registry 合成后的快照；provider 的目录还可能被刷新过程
/// （`refresh_models` 交给 publish 的 `update` 闭包）再次覆盖。
///
/// 失败路径：
/// - `client.list` 的传输/解析错误、registry 刷新本身的 `Err` 都直接返回；
/// - `result.aborted`（同一个 15 s scope 到期）→ `AiErrorKind::Aborted` +
///   `Model catalog refresh timed out.`；
/// - 该 provider 的刷新错误（`errors[LLAMA_PROVIDER_ID]`）原样返回。
///
/// 15 s 的 `timeout_scope` 在目录请求前创建，并被目录请求与 registry 刷新共享，因此两者
/// 合计只有一份 15 s 预算（与 TS 的 `const signal = AbortSignal.timeout(15_000)` 一致）。
async fn sync_catalog(
    ctx: &dyn ExtensionCommandContextApi,
    provider: &LlamaProviderController,
    client: &LlamaClient,
    catalog: Option<Vec<LlamaModelInfo>>,
) -> Result<Vec<LlamaModelInfo>, AiError> {
    let signal = timeout_scope(15_000);
    let current = match catalog {
        Some(catalog) => catalog,
        None => {
            client
                .list(ListOptions {
                    reload: false,
                    signal: Some(&signal),
                })
                .await?
        }
    };
    // `/llama` 已联系过配置的服务器，因此即使在 KIANA_OFFLINE 下也保持联网刷新。
    // 先写 provider：即使下面的刷新被跳过或失败，provider 也已经是本次读取的目录。
    provider.set_catalog(&current, &client.server_url, false);
    let result = ctx
        .model_registry()
        .refresh(ModelsRefreshOptions {
            allow_network: Some(true),
            providers: Some(vec![LLAMA_PROVIDER_ID.to_string()]),
            force: None,
            signal: Some(signal),
        })
        .await?;
    if result.aborted {
        return Err(AiError::new(
            kiana_ai::error::AiErrorKind::Aborted,
            "Model catalog refresh timed out.",
        ));
    }
    if let Some(error) = result.errors.get(LLAMA_PROVIDER_ID) {
        return Err(error.clone());
    }
    Ok(current)
}

/// `readCatalog`：连接错误时无限重试，用户选 Close 才返回 `None`。
///
/// 对**所有** `sync_catalog` 错误都弹 Retry/Close 对话框，不只连接类错误：界面文案由
/// `connection_error_message` 归一（连接类 → `Could not connect to the server.`，其余保留
/// 原 message）。选 Retry 会重新执行完整流程（重新 list + registry 刷新）；`None` 表示
/// 用户关闭，调用方应直接结束命令、不再读目录。
async fn read_catalog(
    ctx: &dyn ExtensionCommandContextApi,
    provider: &LlamaProviderController,
    client: &LlamaClient,
    ui: &dyn LlamaUi,
) -> Option<Vec<LlamaModelInfo>> {
    loop {
        match sync_catalog(ctx, provider, client, None).await {
            Ok(catalog) => return Some(catalog),
            Err(error) => {
                if ui
                    .connection_error(&client.server_url, &connection_error_message(&error))
                    .await
                    == ConnectionOutcome::Close
                {
                    return None;
                }
            }
        }
    }
}

/// `restoreLoaded`：恢复先前被卸载的模型并重读目录。
///
/// 逐个 `load_and_wait`（等待每个模型真正加载完成，进度回调忽略）后刷新目录；任一步
/// 失败返回 `Err`。调用方在“用户取消加载”和“加载失败”两条路径上用它，但待遇不同：
/// 取消路径用 `?` 传播恢复错误；失败路径忽略恢复错误以保留原始加载错误（见 `load_model`）。
async fn restore_loaded(
    ctx: &dyn ExtensionCommandContextApi,
    provider: &LlamaProviderController,
    client: &LlamaClient,
    loaded: &[LlamaModelInfo],
) -> Result<(), AiError> {
    ctx.ui().notify("Restoring previously loaded models", None);
    for model in loaded {
        client.load_and_wait(&model.id, |_| {}, None).await?;
    }
    sync_catalog(ctx, provider, client, None).await?;
    Ok(())
}

/// `loadModel(ctx, ui, client, catalog, target)`。
///
/// 阶段：
/// 1. 从 `catalog` 找出除 `target` 外处于 loaded/sleeping 的模型（目标自身即使是
///    sleeping 也不算冲突）；若有，询问 `Unload all and load` / `Keep loaded and load` /
///    `Cancel`。对话框取消（`None`）或选 `Cancel` 都直接返回 `Ok(())`，不做任何改动。
/// 2. 若选 `Unload all and load`，先逐个 `unload_and_wait` 卸载这些模型。
/// 3. 用 `run_with_progress` 加载目标：进度来自 `load_and_wait`，取消动作是 `unload`
///    （对应 TS 的 `cancel: () => client.unload(target.id)`）。
/// 4. 用户取消 → 若第 2 步卸载过其它模型，用 `restore_loaded` 恢复；正常完成 → 重新读
///    目录，按目标最终状态通知 `Loaded {id}`（已 loaded）或 `Load started for {id}`。
///
/// 失败：`run_with_progress` 及其后的步骤返回 `Err` 时，若“曾卸载其它模型”，会先尝试
/// 恢复（恢复错误被吞掉，保留原始错误）再返回。注意第 2 步 `unload_and_wait` 自身的
/// 错误直接返回、不会恢复已卸载的模型——这段在 `outcome` 块之外，与 TS 的
/// `try` 起始位置一致。
async fn load_model(
    ctx: &dyn ExtensionCommandContextApi,
    ui: &dyn LlamaUi,
    provider: &LlamaProviderController,
    client: &LlamaClient,
    catalog: &[LlamaModelInfo],
    target: &LlamaModelInfo,
) -> Result<(), AiError> {
    let loaded: Vec<LlamaModelInfo> = catalog
        .iter()
        .filter(|model| model.id != target.id && model_is_loaded(model))
        .cloned()
        .collect();
    let mut replace = false;
    if !loaded.is_empty() {
        let title = format!(
            "{} model{} loaded",
            loaded.len(),
            if loaded.len() == 1 { " is" } else { "s are" }
        );
        let choice = ui
            .select(
                &title,
                &[
                    "Unload all and load".to_string(),
                    "Keep loaded and load".to_string(),
                    "Cancel".to_string(),
                ],
            )
            .await;
        let Some(choice) = choice else {
            return Ok(());
        };
        if choice == "Cancel" {
            return Ok(());
        }
        replace = choice == "Unload all and load";
    }

    if replace {
        for model in &loaded {
            client.unload_and_wait(&model.id, None).await?;
        }
    }

    let outcome: Result<(), AiError> = async {
        let run_client = client.clone();
        let run_model = target.id.clone();
        let cancel_client = client.clone();
        let cancel_model = target.id.clone();
        let result = run_with_progress(
            ui,
            RunWithProgressOptions {
                title: "Loading model".to_string(),
                model: target.id.clone(),
                initial_message: "Starting…".to_string(),
                cancel_title: "Stop loading?".to_string(),
                cancel_message: target.id.clone(),
                run: Box::new(move |signal, update| {
                    Box::pin(async move {
                        run_client
                            .load_and_wait(
                                &run_model,
                                move |progress| update(progress),
                                Some(&signal),
                            )
                            .await
                    })
                }),
                cancel: Box::new(move || {
                    Box::pin(async move { cancel_client.unload(&cancel_model, None).await })
                }),
            },
        )
        .await?;
        if matches!(result, RunProgressOutcome::Cancelled) {
            if replace {
                restore_loaded(ctx, provider, client, &loaded).await?;
            }
            return Ok(());
        }
        let refreshed = sync_catalog(ctx, provider, client, None).await?;
        let loaded_model = refreshed.iter().find(|model| model.id == target.id);
        let message = if loaded_model.is_some_and(|model| model.status.value == "loaded") {
            format!("Loaded {}", target.id)
        } else {
            format!("Load started for {}", target.id)
        };
        ctx.ui().notify(&message, None);
        Ok(())
    }
    .await;

    if let Err(error) = outcome {
        if replace {
            // 保留原始加载错误。
            let _ = restore_loaded(ctx, provider, client, &loaded).await;
        }
        return Err(error);
    }
    Ok(())
}

/// `unloadModel(ctx, ui, client, model)`。
///
/// `confirm("Unload model?", id)` 返回否（或对话框取消）时直接 `Ok(())`，不算错误；
/// 否则 `unload_and_wait` → 刷新目录 → 通知 `Unloaded {id}`。卸载/刷新错误向上传播，
/// 由 `run_manager_loop` 决定是否展示。
async fn unload_model(
    ctx: &dyn ExtensionCommandContextApi,
    ui: &dyn LlamaUi,
    provider: &LlamaProviderController,
    client: &LlamaClient,
    model: &LlamaModelInfo,
) -> Result<(), AiError> {
    if !ui.confirm("Unload model?", &model.id).await {
        return Ok(());
    }
    client.unload_and_wait(&model.id, None).await?;
    sync_catalog(ctx, provider, client, None).await?;
    ctx.ui().notify(&format!("Unloaded {}", model.id), None);
    Ok(())
}

/// `downloadModel(ctx, ui, client)`。
///
/// 流程：用 `find_huggingface_token(None)`（读真实进程环境）构造 `HuggingFaceClient` →
/// 富搜索选择 `owner/repo[:quant]` → 解析量化 → 取模型详情 → gated 时提示访问条款 →
/// 未指定量化且详情有候选时让用户选择（详情为空则直接用 `details.id`，不带量化后缀）→
/// `run_with_progress` 调 `download_and_wait` → 成功后把其返回的目录（`reload=true`
/// 重读的结果）交给 `sync_catalog`，再通知 `Downloaded {model}`。
///
/// JS 假值语义：搜索返回空字符串、量化选择返回空字符串、解析出的空量化名都按“取消”
/// 处理（对应的 `filter(|...| !...is_empty())`）；`owner/repo:` 因此不会变成
/// “下载一个叫空字符串的量化的模型”。
///
/// 取消路径只返回 `Ok(())`，不刷新目录；`run_with_progress` 的取消动作是 `unload(model)`
/// （与 TS 的 `cancel: () => client.unload(model)` 一致）。`hugging_face.details` 与下载的
/// 错误向上抛，由 `run_manager_loop` 决定展示方式；Hugging Face **搜索** 的错误不经过
/// 这里——搜索页自己把 `AiError.message` 显示为状态文案（见 `ui.rs` 的 `run_search`）。
async fn download_model(
    ctx: &dyn ExtensionCommandContextApi,
    ui: &dyn LlamaUi,
    provider: &LlamaProviderController,
    client: &LlamaClient,
) -> Result<(), AiError> {
    let hugging_face = HuggingFaceClient::new(find_huggingface_token(None).await, None);
    let search_client = hugging_face.clone();
    let search: SearchModelsFn = std::rc::Rc::new(move |query, signal| {
        let client = search_client.clone();
        Box::pin(async move { client.search(&query, Some(&signal)).await })
    });
    // TS: `if (!selected) return;`——空字符串是假值。
    let Some(selected) = ui
        .search_models(search)
        .await
        .filter(|selected| !selected.is_empty())
    else {
        return Ok(());
    };
    let (repository, mut quantization) = parse_huggingface_model(&selected);
    // TS 的 `if (!quantization)`：`"owner/repo:"` 解析出的空串按“未指定”处理——
    // 若有量化列表就提示选择，最终下载 `details.id`（而不是 `"owner/repo:"`）。
    quantization = quantization.filter(|quantization| !quantization.is_empty());
    ui.show_status("Loading model details", &repository);
    let details = hugging_face.details(&repository, None).await?;
    if details.gated.is_gated() {
        let approval = if details.gated == HuggingFaceGated::Manual {
            "Manual approval is required"
        } else {
            "Accept the access terms"
        };
        let choice = ui
            .select(
                &format!(
                    "Hugging Face access required\n{}\n\n{} at:\nhttps://huggingface.co/{}\n\nThe llama.cpp server needs HF_TOKEN with access.",
                    details.id, approval, details.id
                ),
                &["Continue".to_string(), "Back".to_string()],
            )
            .await;
        if choice.as_deref() != Some("Continue") {
            return Ok(());
        }
    }
    if quantization.is_none() && !details.quantizations.is_empty() {
        let options: Vec<String> = details
            .quantizations
            .iter()
            .map(|entry| {
                let mut parts: Vec<String> = Vec::new();
                if let Some(size) = entry.size {
                    parts.push(format_bytes(size));
                }
                if entry.name == "Q4_K_M" {
                    parts.push("recommended".to_string());
                }
                let detail = parts.join(" · ");
                if detail.is_empty() {
                    entry.name.clone()
                } else {
                    format!("{} · {}", entry.name, detail)
                }
            })
            .collect();
        // TS: `if (!choice) return;`——空字符串是假值。
        let Some(choice) = ui
            .select(&format!("Select quantization\n{}", details.id), &options)
            .await
            .filter(|choice| !choice.is_empty())
        else {
            return Ok(());
        };
        // TS: `if (!quantization) return;`——空量化名同样按假值处理。
        // `options` 与 `details.quantizations` 同序且名字唯一，按选中文本反查下标即可映射回量化项。
        quantization = options
            .iter()
            .position(|option| option == &choice)
            .and_then(|index| details.quantizations.get(index))
            .map(|entry| entry.name.clone())
            .filter(|quantization| !quantization.is_empty());
        if quantization.is_none() {
            return Ok(());
        }
    }
    let model = match &quantization {
        Some(quantization) => format!("{}:{quantization}", details.id),
        None => details.id.clone(),
    };
    let run_client = client.clone();
    let run_model = model.clone();
    let cancel_client = client.clone();
    let cancel_model = model.clone();
    let result = run_with_progress(
        ui,
        RunWithProgressOptions {
            title: "Downloading model".to_string(),
            model: model.clone(),
            initial_message: "Starting…".to_string(),
            cancel_title: "Stop download?".to_string(),
            cancel_message: model.clone(),
            run: Box::new(move |signal, update| {
                Box::pin(async move {
                    run_client
                        .download_and_wait(
                            &run_model,
                            move |progress| update(progress),
                            Some(&signal),
                        )
                        .await
                })
            }),
            cancel: Box::new(move || {
                Box::pin(async move { cancel_client.unload(&cancel_model, None).await })
            }),
        },
    )
    .await?;
    match result {
        RunProgressOutcome::Cancelled => Ok(()),
        RunProgressOutcome::Completed(models) => {
            sync_catalog(ctx, provider, client, Some(models)).await?;
            ctx.ui().notify(&format!("Downloaded {model}"), None);
            Ok(())
        }
    }
}

/// `/llama` 管理器主循环（TS handler 里的 `while (true)` 部分）。
///
/// 每轮：`ui.show_models` → 按动作分派 → `read_catalog`（用户 Close 则结束）→ 若动作
/// 出错且不是连接类错误，用 error 通知。动作错误不会立刻返回：先刷新目录（这一轮可能
/// 弹出 Retry/Close），再用刷新后的目录继续菜单。
///
/// 分派规则：loaded/sleeping → 卸载；`unloaded` → 加载；其它状态（`loading`、
/// `downloading`、服务端新增值）只提示 `{id} is {status}` 的 warning，不进错误路径。
///
/// 抽成独立函数只为回归测试 `close` 的提前返回：TS
/// `if (action.type === "close") return;` 会结束 run，让 `showLlamaUi` 的 `done()` 关闭自定义
/// UI；若把 Close 当作普通动作处理，会重读目录（网络请求）并再次打开同一个菜单。
async fn run_manager_loop(
    ctx: &dyn ExtensionCommandContextApi,
    provider: &LlamaProviderController,
    client: &LlamaClient,
    ui: &dyn LlamaUi,
    mut catalog: Vec<LlamaModelInfo>,
) -> Result<(), AiError> {
    loop {
        let action = ui.show_models(&client.server_url, &catalog).await;
        // TS: `if (action.type === "close") return;`。
        if matches!(action, LlamaManagerAction::Close) {
            return Ok(());
        }
        let mut action_error: Option<AiError> = None;
        let outcome: Result<(), AiError> = match action {
            // 已在上方返回；保留分支只为穷尽匹配。
            LlamaManagerAction::Close => Ok(()),
            LlamaManagerAction::Download => download_model(ctx, ui, provider, client).await,
            LlamaManagerAction::Model(model) if model_is_loaded(&model) => {
                unload_model(ctx, ui, provider, client, &model).await
            }
            LlamaManagerAction::Model(model) if model.status.value == "unloaded" => {
                load_model(ctx, ui, provider, client, &catalog, &model).await
            }
            LlamaManagerAction::Model(model) => {
                ctx.ui().notify(
                    &format!("{} is {}", model.id, model.status.value),
                    Some(NotifyKind::Warning),
                );
                Ok(())
            }
        };
        if let Err(error) = outcome {
            action_error = Some(error);
        }
        // 动作失败也不跳过刷新：TS 在 catch 之后同样执行 readCatalog，刷新本身失败时
        // 还能让用户选择 Retry/Close。
        let Some(refreshed) = read_catalog(ctx, provider, client, ui).await else {
            return Ok(());
        };
        catalog = refreshed;
        // 连接类错误与 read_catalog 的 Retry/Close 对话框职责重叠，不再单独 notify
        // （TS 同样用 `!isConnectionError(actionError)` 过滤）。
        if let Some(error) = &action_error
            && !is_connection_error(error)
        {
            ctx.ui().notify(&error.message, Some(NotifyKind::Error));
        }
    }
}

/// `llamaExtension(kiana)`（TS 默认导出）。
///
/// 注册两件事：
/// - 原生 provider（`ProviderRegistration::Native`）：`create_llama_provider()` 返回的
///   controller 与注册出去的 provider 共享同一份目录状态，命令侧刷新后 provider 立即生效；
/// - `/llama` 命令：仅 TUI 模式执行管理流程；非 TUI 模式只发 warning
///   `/llama is available in interactive mode`。
///
/// 命令处理器自己不上报“进入 UI 之后”的错误：`show_llama_ui` 会在 `run` future 返回
/// `Err` 时统一 notify `error.message` 并关闭视图（见 `ui.rs` 的 `show_llama_ui`）。
/// 初始化阶段（读鉴权、归一 URL、打开 UI 前）的错误则在这里直接 notify。
pub fn llama_extension(kiana: Arc<dyn ExtensionApi>) {
    let controller = create_llama_provider();
    kiana.register_provider(ProviderRegistration::Native(Arc::clone(
        &controller.provider,
    )));

    // `controller` 被移入命令处理器；此前 `controller.provider` 的 `Arc` 已克隆注册给宿主，
    // 两者共享 `Arc<Mutex<LlamaCatalogState>>`，因此处理器里的 `set_catalog` 会立即改变
    // provider 返回的模型。
    let handler_controller = controller;
    kiana.register_command(
        "llama",
        RegisterCommandOptions {
            description: Some("Manage llama.cpp router models".to_string()),
            get_argument_completions: None,
            handler: Arc::new(
                move |_args: String, ctx: Arc<dyn ExtensionCommandContextApi>| {
                    let provider = handler_controller.clone();
                    Box::pin(async move {
                        if ctx.mode() != ExtensionMode::Tui {
                            ctx.ui().notify(
                                "/llama is available in interactive mode",
                                Some(NotifyKind::Warning),
                            );
                            return;
                        }
                        let client = match configured_client(ctx.as_ref()).await {
                            Ok(Some(client)) => client,
                            Ok(None) => return,
                            Err(error) => {
                                ctx.ui().notify(&error.message, Some(NotifyKind::Error));
                                return;
                            }
                        };
                        let run_ctx = Arc::clone(&ctx);
                        show_llama_ui(run_ctx, move |ui| async move {
                            let Some(catalog) =
                                read_catalog(ctx.as_ref(), &provider, &client, &ui).await
                            else {
                                return Ok(());
                            };
                            run_manager_loop(ctx.as_ref(), &provider, &client, &ui, catalog).await
                        })
                        .await;
                    })
                },
            ),
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hugging_face_model_quantization() {
        assert_eq!(
            parse_huggingface_model("owner/repo:Q4_K_M"),
            ("owner/repo".to_string(), Some("Q4_K_M".to_string()))
        );
        assert_eq!(
            parse_huggingface_model("owner/repo"),
            ("owner/repo".to_string(), None)
        );
        // TS 解析出空串（随后按假值处理：有量化列表就提示选择，否则下载 details.id）。
        assert_eq!(
            parse_huggingface_model("owner/repo:"),
            ("owner/repo".to_string(), Some(String::new()))
        );
        // 只在第一个 `/` 之后找 `:`。
        assert_eq!(
            parse_huggingface_model("a:b/c"),
            ("a:b/c".to_string(), None)
        );
        assert_eq!(
            parse_huggingface_model("repo:Q4"),
            ("repo".to_string(), Some("Q4".to_string()))
        );
    }

    /// `close` 回归测试的上下文：任何被调用都视为失败。
    ///
    /// 修复前 `close` 被当作普通动作，循环会继续 `readCatalog`（先发 HTTP，再访问
    /// `model_registry`），因此测试用会失败的 [`kiana_ai::http::HttpClient`] 让流程先走到
    /// `ui.connection_error`，再用计数断言“close 之后不应再有任何目录读取/UI 交互”。
    struct PanicContext;

    impl kiana_ext_api::types::ExtensionContextApi for PanicContext {
        fn ui(&self) -> &dyn kiana_ext_api::ui::ExtensionUiContext {
            panic!("close action must not touch ctx.ui()")
        }
        fn mode(&self) -> ExtensionMode {
            ExtensionMode::Tui
        }
        fn has_ui(&self) -> bool {
            true
        }
        fn cwd(&self) -> &std::path::Path {
            std::path::Path::new("/")
        }
        fn session_manager(
            &self,
        ) -> &dyn kiana_session_core::session_manager::ReadonlySessionManager {
            panic!("close action must not touch the session manager")
        }
        fn model_registry(&self) -> &kiana_config::models::registry::ModelRegistry {
            panic!("close action must not refresh the model catalog")
        }
        fn model(&self) -> Option<kiana_ai::types::Model> {
            None
        }
        fn scoped_models(&self) -> Vec<kiana_config::models::resolver::ScopedModel> {
            Vec::new()
        }
        fn thinking_level(&self) -> Option<kiana_agent_core::ThinkingLevel> {
            None
        }
        fn is_idle(&self) -> bool {
            true
        }
        fn is_project_trusted(&self) -> bool {
            true
        }
        fn signal(&self) -> Option<tokio_util::sync::CancellationToken> {
            None
        }
        fn abort(&self) {
            panic!("close action must not abort")
        }
        fn has_pending_messages(&self) -> bool {
            false
        }
        fn shutdown(&self) {
            panic!("close action must not shut down")
        }
        fn get_context_usage(&self) -> Option<kiana_ext_api::types::ContextUsage> {
            None
        }
        fn compact(&self, _options: Option<kiana_ext_api::types::CompactOptions>) {
            panic!("close action must not compact")
        }
        fn get_system_prompt(&self) -> String {
            String::new()
        }
    }

    impl ExtensionCommandContextApi for PanicContext {
        fn get_system_prompt_options(
            &self,
        ) -> kiana_ext_api::system_prompt::BuildSystemPromptOptions {
            panic!("close action must not read system prompt options")
        }
        fn wait_for_idle(&self) -> kiana_ext_api::LocalBoxFuture<'static, ()> {
            Box::pin(async {})
        }
        fn new_session(
            &self,
            _options: Option<kiana_ext_api::types::NewSessionOptions>,
        ) -> kiana_ext_api::LocalBoxFuture<'static, kiana_ext_api::types::NewSessionResult>
        {
            panic!("close action must not start a new session")
        }
        fn fork<'a>(
            &'a self,
            _entry_id: &'a str,
            _options: Option<kiana_ext_api::types::ForkOptions>,
        ) -> kiana_ext_api::LocalBoxFuture<'a, kiana_ext_api::types::NewSessionResult> {
            panic!("close action must not fork")
        }
        fn navigate_tree<'a>(
            &'a self,
            _target_id: &'a str,
            _options: Option<kiana_ext_api::types::NavigateTreeOptions>,
        ) -> kiana_ext_api::LocalBoxFuture<'a, kiana_ext_api::types::NewSessionResult> {
            panic!("close action must not navigate")
        }
        fn switch_session<'a>(
            &'a self,
            _session_path: &'a str,
            _options: Option<kiana_ext_api::types::SwitchSessionOptions>,
        ) -> kiana_ext_api::LocalBoxFuture<'a, kiana_ext_api::types::NewSessionResult> {
            panic!("close action must not switch sessions")
        }
        fn reload(&self) -> kiana_ext_api::LocalBoxFuture<'static, ()> {
            panic!("close action must not reload")
        }
    }

    /// 总是失败（`fetch failed`）的 HTTP 客户端：让错误路径不依赖网络。
    struct FailingHttpClient;

    impl kiana_ai::http::HttpClient for FailingHttpClient {
        fn execute<'a>(
            &'a self,
            _request: kiana_ai::http::HttpRequest,
            _signal: Option<&tokio_util::sync::CancellationToken>,
        ) -> kiana_ai::http::BoxFuture<
            'a,
            Result<kiana_ai::http::HttpResponse, kiana_ai::http::HttpError>,
        > {
            Box::pin(async { Err(kiana_ai::http::HttpError::new("fetch failed")) })
        }
    }

    /// 第一次 `showModels` 就返回 `Close` 的 UI；其余交互都是失败。
    #[derive(Default)]
    struct CloseUi {
        show_models_calls: std::cell::Cell<usize>,
        connection_error_calls: std::cell::Cell<usize>,
    }

    impl LlamaUi for CloseUi {
        fn show_models<'a>(
            &'a self,
            _server_url: &'a str,
            _models: &'a [LlamaModelInfo],
        ) -> kiana_ext_api::LocalBoxFuture<'a, LlamaManagerAction> {
            self.show_models_calls.set(self.show_models_calls.get() + 1);
            Box::pin(async { LlamaManagerAction::Close })
        }
        fn select<'a>(
            &'a self,
            _title: &'a str,
            _options: &'a [String],
        ) -> kiana_ext_api::LocalBoxFuture<'a, Option<String>> {
            panic!("select must not be called after close")
        }
        fn confirm<'a>(
            &'a self,
            _title: &'a str,
            _message: &'a str,
        ) -> kiana_ext_api::LocalBoxFuture<'a, bool> {
            panic!("confirm must not be called after close")
        }
        fn connection_error<'a>(
            &'a self,
            _server_url: &'a str,
            _message: &'a str,
        ) -> kiana_ext_api::LocalBoxFuture<'a, ConnectionOutcome> {
            self.connection_error_calls
                .set(self.connection_error_calls.get() + 1);
            Box::pin(async { ConnectionOutcome::Close })
        }
        fn search_models<'a>(
            &'a self,
            _search: SearchModelsFn,
        ) -> kiana_ext_api::LocalBoxFuture<'a, Option<String>> {
            panic!("search must not be called after close")
        }
        fn show_status(&self, _title: &str, _message: &str) {
            panic!("status must not be called after close")
        }
        fn progress<'a>(
            &'a self,
            _state: &'a super::super::ui::ProgressState,
        ) -> kiana_ext_api::LocalBoxFuture<'a, ()> {
            panic!("progress must not be called after close")
        }
        fn update_progress(&self, _state: &super::super::ui::ProgressState) {
            panic!("progress update must not be called after close")
        }
    }

    #[tokio::test]
    async fn close_action_exits_the_manager_without_reloading_the_catalog() {
        let provider = create_llama_provider();
        let client = LlamaClient::with_http_client(
            "http://127.0.0.1:8080",
            None,
            std::sync::Arc::new(FailingHttpClient),
        )
        .expect("client");
        let ui = CloseUi::default();
        let result = run_manager_loop(
            &PanicContext,
            &provider,
            &client,
            &ui,
            vec![LlamaModelInfo {
                id: "test-model".to_string(),
                status: super::super::client::LlamaModelStatusInfo {
                    value: "unloaded".to_string(),
                    ..Default::default()
                },
                ..Default::default()
            }],
        )
        .await;
        assert!(result.is_ok());
        assert_eq!(ui.show_models_calls.get(), 1);
        assert_eq!(ui.connection_error_calls.get(), 0);
    }

    #[test]
    fn connection_errors_match_typescript() {
        let error = AiError::new(kiana_ai::error::AiErrorKind::Transport, "fetch failed");
        assert!(is_connection_error(&error));
        assert_eq!(
            connection_error_message(&error),
            "Could not connect to the server."
        );
        let error = AiError::new(
            kiana_ai::error::AiErrorKind::Other,
            "The operation was aborted due to timeout",
        )
        .with_name("TimeoutError");
        assert!(is_connection_error(&error));
        let error = AiError::new(kiana_ai::error::AiErrorKind::Other, "boom");
        assert!(!is_connection_error(&error));
        assert_eq!(connection_error_message(&error), "boom");
    }
}
