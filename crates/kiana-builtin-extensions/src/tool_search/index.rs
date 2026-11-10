//! extensions/tool-search/index.ts：把 `tool_search` 注册为扩展。
//!
//! CLI 把它作为内置扩展加载；SDK 用户把 `create_tool_search_extension()` 加入扩展工厂。
//! `tool_search` 默认**不激活**：用 `--tools`、`defaultTools` 设置或 `setActiveTools()` 激活。
//!
//! # 线程桥
//!
//! [`kiana_ext_api::types::ExtensionApi`] 是 `!Send`（kiana-extensions 在单线程 `LocalSet` 上把它
//! 实现成 rquickjs 句柄的持有者），而 `ToolDefinition.execute` 要求 `Send + Sync`。工厂在
//! `LocalSet` 上运行，因此这里 `spawn_local` 一个转发任务持有 API 句柄；工具定义只捕获
//! `tokio::sync::mpsc` 的发送端（`Send + Sync`）。语义与 TS 直接调用 `kiana.getAllTools()` 相同，
//! 只是多一次同线程的消息往返。
//!
//! # 设计原因：请求/回复协议
//!
//! `ToolDefinition::execute` 要求执行器 `Send + Sync`（见上节），而把
//! `Arc<dyn ExtensionApi>` 直接捕获进执行器无法满足该约束（`!Send` 表示值不能跨线程移动；
//! `LocalSet` 是 tokio 提供的单线程任务上下文）。这三个调用又都需要等待完成或结果，
//! 不能只“发出去”就不管；因此这里用 `tokio::sync::mpsc::UnboundedSender` 传请求、每个请求
//! 自带一个 `oneshot` 送回结果：发送端是纯数据（`Send + Sync`），真正调用 API 的任务留在
//! `LocalSet` 上。
//!
//! # 注册与执行流程
//!
//! 1. 工厂被调用时，`start_extension_api_tools` 创建无界通道，并在当前 `LocalSet` 上
//!    `spawn_local` 一个转发任务；任务持有 `Arc<dyn ExtensionApi>`。
//! 2. 工厂用这个 `ToolSearchTools` 实现调用
//!    [`crate::tool_search::tool::create_tool_search_tool_definition`] 构造工具定义，随后
//!    把 `default_active` 覆盖成 `Some(false)`，再调用 `register_tool`。
//! 3. 模型触发 `tool_search.execute` 后，`ExtensionApiTools` 为每次
//!    `getActiveTools` / `getAllTools` / `setActiveTools` 调用发送一条 `ToolsRequest`，
//!    并等待对应的 oneshot 回复。
//! 4. 转发任务串行处理请求：先调用同步的 API 方法，再把结果写回 oneshot。
//!
//! # 顺序与失败模式
//!
//! - 通道无界，发送不会等待；所有请求由**同一个**任务按发送顺序串行执行，因此单次
//!   `getActiveTools` 看到的是该请求被处理时的会话状态。两次独立调用之间没有事务性：
//!   搜索流程先读 active、再写回整份列表，期间其它来源的激活会被这份快照覆盖；
//!   `setActiveTools` 是整体替换，见 [`crate::tool_search::tool::ToolSearchTools`] 的契约。
//! - 若转发任务已退出（通道关闭）或 oneshot 回复被丢弃，`request` 返回 `None`：两个查询
//!   方法退化成空 `Vec`，`set_active_tools` 静默忽略。`ToolSearchTools` 没有错误通道，
//!   调用方只会看到“没有匹配工具”，不会看到错误原因。
//! - `kiana-extensions` 的 `ExtensionApi` 实现在扩展 entry 失效后会让这些同步方法 panic
//!   （loader 的 `ensure_active`）；panic 会结束转发任务，使后续请求走上面的退化路径。
//! - `spawn_local` 要求当前线程存在 `LocalSet` 上下文。扩展工厂按 `ExtensionFactory` 的
//!   类型文档约定在 `LocalSet` 上轮询；`tests/builtin_registry.rs` 显式创建 `LocalSet`
//!   后调用四个内置工厂。

use std::sync::Arc;

use kiana_ext_api::types::{ExtensionApi, ExtensionFactory, ToolInfo};
use tokio::sync::{mpsc, oneshot};

use crate::tool_search::tool::{
    ToolSearchToolOptions, ToolSearchTools, create_tool_search_tool_definition,
};

/// 转发任务收到的一次调用。
///
/// 每个请求都自带一个 `oneshot::Sender` 作为回复通道；无界通道只承载请求，因此并发请求
/// 不会把回复串到一起。请求被处理前若发送方离开，转发任务写回复时会失败，该失败被忽略。
enum ToolsRequest {
    /// `getAllTools()`。
    All {
        reply: oneshot::Sender<Vec<ToolInfo>>,
    },
    /// `getActiveTools()`。
    Active { reply: oneshot::Sender<Vec<String>> },
    /// `setActiveTools(names)`。
    SetActive {
        names: Vec<String>,
        reply: oneshot::Sender<()>,
    },
}

/// [`ToolSearchTools`] 的转发实现：只持有请求通道的发送端，不直接接触 `ExtensionApi`。
///
/// 它因此是 `Send + Sync`，可以安全地放进 `ToolDefinition::execute` 的 `Arc` 里；API 调用
/// 全部由 `start_extension_api_tools` 创建的同线程任务完成。
struct ExtensionApiTools {
    sender: mpsc::UnboundedSender<ToolsRequest>,
}

impl ExtensionApiTools {
    /// 发送一条请求并等待回复。
    ///
    /// 返回 `None` 有两种来源：发送失败（转发任务已退出、通道关闭）与回复被丢弃（任务在处理
    /// 该请求时结束或 panic）。调用方据此退化为空结果或忽略写入——trait 没有错误通道。
    async fn request<T>(
        &self,
        build: impl FnOnce(oneshot::Sender<T>) -> ToolsRequest,
    ) -> Option<T> {
        let (reply, receiver) = oneshot::channel();
        self.sender.send(build(reply)).ok()?;
        receiver.await.ok()
    }
}

#[async_trait::async_trait]
#[allow(clippy::double_must_use)] // 同 trait 定义。
impl ToolSearchTools for ExtensionApiTools {
    async fn get_all_tools(&self) -> Vec<ToolInfo> {
        self.request(|reply| ToolsRequest::All { reply })
            .await
            .unwrap_or_default()
    }

    async fn get_active_tools(&self) -> Vec<String> {
        self.request(|reply| ToolsRequest::Active { reply })
            .await
            .unwrap_or_default()
    }

    async fn set_active_tools(&self, tool_names: Vec<String>) {
        let _ = self
            .request(|reply| ToolsRequest::SetActive {
                names: tool_names,
                reply,
            })
            .await;
    }
}

/// 启动同线程转发任务；必须在 `LocalSet` 上调用（扩展工厂的既定运行方式）。
///
/// 任务持有 `kiana` 的 `Arc`，只要还有工具定义持有发送端就不会释放；所有发送端被丢弃后
/// `recv` 返回 `None`，任务结束。每次调用本函数都会新建独立的通道与任务（不共享状态）。
///
/// 注意：同步 API 方法可能 panic（扩展 entry 失效时 `kiana-extensions` 的 `ensure_active`
/// 会 panic）；panic 结束该任务，后续请求的 `request` 返回 `None`。
fn start_extension_api_tools(kiana: Arc<dyn ExtensionApi>) -> Arc<dyn ToolSearchTools> {
    let (sender, mut receiver) = mpsc::unbounded_channel();
    tokio::task::spawn_local(async move {
        // 发送端全部丢弃后循环自然结束。
        while let Some(request) = receiver.recv().await {
            match request {
                ToolsRequest::All { reply } => {
                    // 执行方可能已经离开（oneshot 被丢弃），忽略发送失败。
                    let _ = reply.send(kiana.get_all_tools());
                }
                ToolsRequest::Active { reply } => {
                    let _ = reply.send(kiana.get_active_tools());
                }
                ToolsRequest::SetActive { names, reply } => {
                    kiana.set_active_tools(names);
                    let _ = reply.send(());
                }
            }
        }
    });
    Arc::new(ExtensionApiTools { sender })
}

/// `createToolSearchExtension()`。
///
/// 返回一个 [`ExtensionFactory`]：每次调用工厂都会
/// 新建独立的无界通道与转发任务，并注册一个名为 `tool_search` 的
/// [`ToolDefinition`](kiana_ext_api::tool_types::ToolDefinition)。注册前把
/// `default_active` 覆盖为 `Some(false)`，因此工具默认不进入 active 集合，需要
/// `--tools` / `defaultTools` / `setActiveTools()` 才会声明给模型。
///
/// 工厂返回的 future 在 `kiana-extensions` 的单线程 `LocalSet` 上轮询。
///
/// 调用方：`crate::index::builtin_extensions` 把它作为 `tool-search` 内置项；
/// `kiana-coding-agent` 的 `sdk_exports` 以 `createToolSearchExtension` 重导出给 SDK 用户。
/// `tests/builtin_registry.rs` 在 `LocalSet` 上用假 API 调用它，断言注册了 `tool_search`。
pub fn create_tool_search_extension() -> ExtensionFactory {
    Arc::new(move |kiana: Arc<dyn ExtensionApi>| {
        Box::pin(async move {
            let tools = start_extension_api_tools(Arc::clone(&kiana));
            let mut definition =
                create_tool_search_tool_definition(ToolSearchToolOptions { tools: Some(tools) });
            definition.default_active = Some(false);
            kiana.register_tool(definition);
        })
    })
}
