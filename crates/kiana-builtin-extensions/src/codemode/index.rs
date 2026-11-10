//! extensions/codemode/index.ts：把 `codemode` 注册为扩展。
//!
//! CLI 把它作为内置扩展加载；SDK 用户把 `create_codemode_extension()` 加入扩展工厂。
//! `codemode` 注册时不激活：用 `--tools`、`defaultTools` 设置或 `setActiveTools()` 激活；
//! 当 MCP 工具只能从脚本访问时，MCP 扩展会自动激活它（`mcp/index.rs` 的
//! `ensure_discovery_active`，TS `ensureDiscoveryActive`）。
//!
//! # 职责与加载流程
//!
//! 对外的函数入口只有 [`create_codemode_extension`]：它返回一个 [`ExtensionFactory`]（`Arc`
//! 包装的异步闭包）。扩展 loader 在加载阶段调用该闭包，闭包内依次：
//!
//! 1. 分配 token，把 `Arc<dyn ExtensionApi>` 登记到当前线程的 `API_REGISTRY`；
//! 2. 构造 `codemode` 工具的 `ToolDefinition`，把「读设置、读工具表、持久化 `store()`」
//!    全部实现为回调（优先级见 [`create_codemode_extension`]）；
//! 3. 把 `default_active` 设为 `false`，再调用 `register_tool`。
//!
//! 在 loader 里，工厂执行时通常尚未 `bind_core`，`RuntimeShared.actions` 还没有绑定；
//! `register_tool` 这类注册方法可用，但 `get_settings` / `append_entry` / `get_all_tools`
//! 这类运行期动作会触发 "Extension runtime not initialized. Action methods cannot be called
//! during extension loading." 的 panic（loader 把它记成加载错误）。该 panic 取决于动作是否
//! 已经绑定，而不是扩展的 loading 状态——若调用方在加载前已绑定 runtime 动作，加载期调用
//! 可以成功。因此这里刻意把每次读取推迟到工具被激活/执行之后；回调只捕获 token 与选项，
//! 不在工厂体内读设置。
//!
//! # 线程模型
//!
//! [`kiana_ext_api::types::ExtensionApi`] 是 `!Send`（kiana-extensions 在单线程 `LocalSet` 上把它
//! 实现成 rquickjs 句柄的持有者），而 `ToolDefinition` 的 `prepare_loadout` / `reaches_tools`
//! / `execute` 回调都要求 `Send + Sync`。本模块用两种方式桥接：
//!
//! 1. **同步读**（settings、工具 namespace/guideline）：工厂在 `LocalSet` 线程上把
//!    `Arc<dyn ExtensionApi>` 登记到线程本地表，回调按 token 查找。TS 的 `prepare_loadout`
//!    也在会话的同一线程上运行；从其它线程调用时查不到 API，此时 `get_mode` 回退到
//!    `CodemodeExtensionOptions::mode` 或 `SETTINGS_DEFAULTS.codemode.mode`，
//!    `get_inline_budget` 回退到选项值或 `None`，工具 namespace 为 `None`、guideline 为空表。
//! 2. **异步写**（`appendEntry`）：工厂 `spawn_local` 一个转发任务持有 API，回调通过
//!    `tokio::sync::mpsc` 发送请求并等待完成，从而保持 TS 的写入顺序。
//!
//! 线程本地表与 TS 的扩展闭包一样在扩展生命周期内保持 API 句柄存活（不额外释放）；更准确
//! 地说，表只追加、从不删除，条目会一直留到线程退出。token 由进程级 `AtomicU64` 单调分配，
//! 因此不同扩展实例之间不会串线。

use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use kiana_config::settings::defaults::SETTINGS_DEFAULTS;
use kiana_config::settings::{CodemodeMode, Settings};
use kiana_ext_api::types::{ExtensionApi, ExtensionFactory};
use tokio::sync::{mpsc, oneshot};

use crate::codemode::tool::{
    AppendEntryFn, CodemodeStoreEntryData, CodemodeToolOptions, GetInlineBudgetFn, GetModeFn,
    GetToolGuidelinesFn, GetToolNamespaceFn, create_codemode_tool_definition,
};

thread_local! {
    /// 工厂注册的扩展 API，按 token 查找。只保存本线程（`LocalSet`）的句柄。
    static API_REGISTRY: RefCell<Vec<(u64, Arc<dyn ExtensionApi>)>> = const { RefCell::new(Vec::new()) };
}

/// 下一个注册 token。
static NEXT_API_TOKEN: AtomicU64 = AtomicU64::new(1);

fn register_api(token: u64, api: Arc<dyn ExtensionApi>) {
    API_REGISTRY.with(|registry| registry.borrow_mut().push((token, api)));
}

/// 在本线程的表里查找 API 并执行 `f`；不在注册线程上时返回 `None`。
///
/// 表只追加，查找是从后往前的线性扫描；token 唯一，所以扫描顺序不影响命中结果。
/// `f` 在持有 `RefCell` 不可变借用期间执行：如果 `f` 里再次调用 [`register_api`]，
/// 会因借用冲突 panic，因此注册只能发生在工厂体内、查找之外。
fn with_api<R>(token: u64, f: impl FnOnce(&Arc<dyn ExtensionApi>) -> R) -> Option<R> {
    API_REGISTRY.with(|registry| {
        let registry = registry.borrow();
        registry
            .iter()
            .rev()
            .find(|(registered, _)| *registered == token)
            .map(|(_, api)| f(api))
    })
}

/// `readMode(kiana)`：`codemode.mode === "only"` 之外一律用默认值（`on`）。
///
/// 设置里 `codemode` 与 `mode` 都是 `Option`：未配置时取 [`SETTINGS_DEFAULTS`] 的 `mode`
/// （`On`）。`CodemodeMode` 只有 `On` / `Only` 两态，因此这段 match 等价于
/// 「只有显式配置 `Only` 才得到 `Only`」。
fn read_mode(settings: &Settings) -> CodemodeMode {
    match settings
        .codemode
        .as_ref()
        .and_then(|codemode| codemode.mode)
    {
        Some(CodemodeMode::Only) => CodemodeMode::Only,
        _ => SETTINGS_DEFAULTS.codemode.mode,
    }
}

/// `readInlineBudget(kiana)`：有限且 `>= 0` 的数字，否则 `None`。
///
/// 返回 `None` 表示「未设置/无效」，消费方（`tool.rs` 的 loadout 描述）会改用
/// `DEFAULT_CODEMODE_INLINE_BUDGET`。负值、NaN 与 ±∞ 被当作无效配置静默忽略，
/// 不会让工具注册或执行失败。
fn read_inline_budget(settings: &Settings) -> Option<f64> {
    settings
        .codemode
        .as_ref()
        .and_then(|codemode| codemode.inline_budget)
        .filter(|budget| budget.is_finite() && *budget >= 0.0)
}

/// `createCodemodeExtension` 的选项。
///
/// 三个字段都是「覆盖」：`None` 表示沿用设置或内置默认值（`Default` 即全部不覆盖）。
/// 选项在创建工厂时按值捕获；工厂每次执行会 clone 一份，因此创建后修改原结构不会影响
/// 已生成的扩展，多个 loader 之间也不共享可变状态。
#[derive(Debug, Clone, Default)]
pub struct CodemodeExtensionOptions {
    /// 覆盖 `codemode.mode` 设置。
    pub mode: Option<CodemodeMode>,
    /// 覆盖 `codemode.inlineBudget` 设置。
    pub inline_budget: Option<f64>,
    /// 把模型目录与分类器暴露给脚本。默认 `true`。
    pub models: Option<bool>,
}

/// `appendEntry` 的转发请求。
enum EntryRequest {
    Append {
        custom_type: String,
        data: CodemodeStoreEntryData,
        reply: oneshot::Sender<()>,
    },
}

/// 启动 `appendEntry` 的同线程转发任务（必须在 `LocalSet` 上调用）。
///
/// [`AppendEntryFn`] 的回调可能在任何线程被轮询，而 `ExtensionApi` 只在创建它的
/// `LocalSet` 线程上可用。桥接因此分两步：
///
/// 1. 无界 `mpsc` 通道 + 一个 `spawn_local` 任务持有 `Arc<dyn ExtensionApi>`；
/// 2. 返回的闭包把 `(custom_type, data)` 发进通道，并等待 `oneshot` 应答。
///
/// 等待应答让调用方在返回的 future 完成时知道写入已经执行，与同步
/// `appendEntry` 的语义对齐：通道正常时，工具结果返回前条目已追加。并发执行的脚本之间没有额外
/// 排序保证，顺序取决于各自请求到达通道的先后。
///
/// 失败路径：通道关闭（`LocalSet` 销毁或任务结束）时 `send` 返回错误，闭包直接完成，
/// 写入被静默丢弃（`ExtensionApi::append_entry` 没有错误返回）；若任务内的
/// `append_entry` panic，任务随之结束，之后的写入同样静默丢弃。
fn start_append_entry_bridge(kiana: Arc<dyn ExtensionApi>) -> AppendEntryFn {
    let (sender, mut receiver) = mpsc::unbounded_channel();
    tokio::task::spawn_local(async move {
        while let Some(EntryRequest::Append {
            custom_type,
            data,
            reply,
        }) = receiver.recv().await
        {
            let value = serde_json::to_value(&data).unwrap_or(serde_json::Value::Null);
            kiana.append_entry(&custom_type, Some(value));
            let _ = reply.send(());
        }
    });
    Arc::new(move |custom_type, data| {
        let sender = sender.clone();
        Box::pin(async move {
            let (reply, receiver) = oneshot::channel();
            if sender
                .send(EntryRequest::Append {
                    custom_type,
                    data,
                    reply,
                })
                .is_ok()
            {
                let _ = receiver.await;
            }
        })
    })
}

/// `createCodemodeExtension(options = {})`。
///
/// 返回扩展工厂。loader 调用工厂时注册 `codemode` 工具，但**不激活**它：`default_active`
/// 被显式设为 `false`，需要 `--tools`、`defaultTools` 或 `setActiveTools()` 才会声明给
/// 模型；MCP 扩展在「MCP 工具只能经脚本触达」时会自动激活它。
///
/// # 回调的取值优先级
///
/// - `mode`：`options.mode` > 实时 `get_settings().codemode.mode`（仅 `Only` 有意义）>
///   默认 `On`。
/// - `inline_budget`：`options.inline_budget` > 实时设置（有限且非负）> 未设置
///   （消费方使用默认预算）。
/// - 工具 namespace / guideline：实时 `get_all_tools()`；不在注册线程上时分别退化为
///   `None` 与空表。
///
/// # 约束
///
/// 必须在 `LocalSet` 上调用（内部用 `spawn_local` 启动 `appendEntry` 转发任务），在普通
/// 线程调用会 panic。每次工厂执行都会分配新的注册 token，因此同一进程可以加载多个
/// `codemode` 实例。
///
/// 工具执行时的 `store()` 写入经无界通道转发到该 `LocalSet` 线程；通道正常时，调用方会
/// 等到写入完成才返回工具结果。
pub fn create_codemode_extension(options: CodemodeExtensionOptions) -> ExtensionFactory {
    Arc::new(move |kiana: Arc<dyn ExtensionApi>| {
        let options = options.clone();
        Box::pin(async move {
            let token = NEXT_API_TOKEN.fetch_add(1, Ordering::SeqCst);
            // TS 在 `getMode`/`getInlineBudget` 里惰性读取 `kiana.getSettings()`；工厂执行时仍处于
            // 扩展加载阶段，kiana-extensions 禁止任何运行时动作（早读会 panic 并被记成加载错误）。
            register_api(token, Arc::clone(&kiana));

            let mode_override = options.mode;
            let get_mode: GetModeFn = Arc::new(move || {
                if let Some(mode) = mode_override {
                    return mode;
                }
                with_api(token, |api| read_mode(&api.get_settings()))
                    .unwrap_or(SETTINGS_DEFAULTS.codemode.mode)
            });
            let budget_override = options.inline_budget;
            let get_inline_budget: GetInlineBudgetFn = Arc::new(move || {
                if let Some(budget) = budget_override {
                    return Some(budget);
                }
                with_api(token, |api| read_inline_budget(&api.get_settings())).unwrap_or(None)
            });
            let get_tool_namespace: GetToolNamespaceFn = Arc::new(move |name: &str| {
                with_api(token, |api| {
                    api.get_all_tools()
                        .into_iter()
                        .find(|tool| tool.name == name)
                        .and_then(|tool| tool.namespace)
                })
                .flatten()
            });
            let get_tool_guidelines: GetToolGuidelinesFn = Arc::new(move || {
                with_api(token, |api| {
                    api.get_all_tools()
                        .into_iter()
                        .map(|tool| (tool.name, tool.prompt_guidelines.unwrap_or_default()))
                        .collect()
                })
                .unwrap_or_default()
            });

            let mut definition = create_codemode_tool_definition(CodemodeToolOptions {
                get_tool_namespace: Some(get_tool_namespace),
                get_tool_guidelines: Some(get_tool_guidelines),
                models: Some(options.models.unwrap_or(true)),
                append_entry: Some(start_append_entry_bridge(Arc::clone(&kiana))),
                get_mode: Some(get_mode),
                get_inline_budget: Some(get_inline_budget),
            });
            // 不默认激活：只有 `--tools` / `defaultTools` / `setActiveTools()` 才把 `codemode`
            // 声明给模型；MCP 扩展在工具只能经脚本触达时会主动激活（`mcp/index.rs`）。
            definition.default_active = Some(false);
            kiana.register_tool(definition);
        })
    })
}
