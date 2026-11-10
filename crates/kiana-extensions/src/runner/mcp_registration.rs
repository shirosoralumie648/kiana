//! `registerMcpServer` 的校验、所有权与命名空间冲突检查。
//!
//! TS: `core/extensions/loader.ts` 的扩展 API 方法
//! `registerMcpServer(name, config)`（`spec/ca-extensions.md` §4.4、§8.6 第 14 条）。
//!
//! 本模块把 TS 方法体里的“校验 → 所有权 → 命名空间 → 注册”四步抽成纯 Rust 函数，
//! 供 loader-facade（`js/api.js` 的宿主原语）调用；变更监听与 `mcp_servers_change`
//! 的派发仍由 runner-core 接线。
//!
//! # 调用链
//!
//! loader 的 JS 桥把 `kiana.registerMcpServer(name, config)` 转到
//! `loader::ExtensionEntry::register_mcp_server_raw`：后者先调用 [`prepare_mcp_server`]
//! 做全部检查，再把返回值交给被 `apply_runtime_change` 包住的
//! [`McpServerRegistry::register`]。加载期（`LoadState::Loading`）只把提交闭包压入**当前
//! 扩展工厂**的 pending 队列，工厂返回后 `commit` 才真正写注册表；运行期则立即写入。
//! 两种时序的差异由 `tests/mcp_registration.rs` 锁定。
//!
//! # 检查顺序与失败模式
//!
//! 三个检查严格按固定顺序执行（校验 → 所有权 → 命名空间），先失败先返回；因此非法配置
//! 即使名字已被占用，也报校验错误。错误 `Display` 文案为当前稳定契约，改动需同步测试。
//! 所有检查都在写入之前完成，失败时注册表不变、变更监听器也不会被触发。
//!
//! 检查（[`prepare_mcp_server`]）与写入（[`McpServerRegistry::register`]）是两个独立调用，
//! 本模块不持有一把跨越两步的锁，因此“检查-写入”并非原子；当前调用方（loader）在同一个
//! 引擎线程上完成这两步，依赖单线程模型（设计文档 §2.0）而非锁来避免并发插入。
//!
//! # 命名空间
//!
//! [`mcp_namespace`] 把名字里的 `-` 换成 `_` 并加上 `mcp__` 前缀，因此 `work-files` 与
//! `work_files` 共享同一个命名空间，不能同时注册。检查只比较**名字不同**的条目，同名
//! 重注册不受影响；多个冲突时按注册表顺序（插入顺序）取第一个作为错误里的 `other`。

use kiana_core::mcp_servers::{
    McpServerRegistry, RegisteredMcpServer, mcp_namespace, validate_mcp_server_config,
};
use serde_json::Value;

/// `registerMcpServer` 抛出的错误。`Display` 逐字等于 TS `Error.message`。
///
/// TS: `loader.ts#registerMcpServer` 的三个 `throw new Error(...)`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpRegistrationError {
    /// 配置未通过 `validateMcpServerConfig`。
    ///
    /// 文案：`Invalid MCP server registered by extension "<path>": <validated>`。
    Invalid {
        /// 注册它的扩展路径。
        extension_path: String,
        /// `validateMcpServerConfig` 返回的错误串。
        message: String,
    },
    /// 同名 server 已被另一个扩展注册。
    ///
    /// 文案：`MCP server "<name>" is already registered by extension "<owner>"`。
    AlreadyRegistered {
        /// server 名。
        name: String,
        /// 现有条目的扩展路径。
        owner: String,
    },
    /// `mcp__` 命名空间（把 `-` 换成 `_` 后）与另一个 server 相同。
    ///
    /// 文案：`MCP server "<name>" conflicts with registered server "<other>"`。
    NamespaceConflict {
        /// server 名。
        name: String,
        /// 冲突的现有 server 名。
        other: String,
    },
}

impl std::fmt::Display for McpRegistrationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            McpRegistrationError::Invalid {
                extension_path,
                message,
            } => write!(
                formatter,
                "Invalid MCP server registered by extension \"{extension_path}\": {message}"
            ),
            McpRegistrationError::AlreadyRegistered { name, owner } => write!(
                formatter,
                "MCP server \"{name}\" is already registered by extension \"{owner}\""
            ),
            McpRegistrationError::NamespaceConflict { name, other } => write!(
                formatter,
                "MCP server \"{name}\" conflicts with registered server \"{other}\""
            ),
        }
    }
}

impl std::error::Error for McpRegistrationError {}

/// 执行 `registerMcpServer` 在**注册前**的全部检查，返回可提交的注册项。
///
/// TS 源：`loader.ts#registerMcpServer`。步骤与顺序如下：
/// 1. `validate_mcp_server_config(name, config)`；失败时消息统一加
///    `Invalid MCP server registered by extension "<path>": ` 前缀。
/// 2. 同名 server 已存在且属于**另一个**扩展时拒绝（同一扩展可重注册/替换）。
/// 3. 现有 server 名与 `name` 不同但 [`mcp_namespace`] 相同时拒绝（`-`/`_` 共享命名空间）。
/// 4. 返回的注册项持有 `config` 的**独立副本**（TS 的 `structuredClone(validated)`；
///    [`McpServerConfig`] 只含 `String`/`IndexMap`/`Value`，`Clone` 即深拷贝）。
///
/// TS: `loader.ts#registerMcpServer` 中 `applyRuntimeChange(() => runtime.mcpServers.register(server))`
/// 之前的三步。扩展加载期间的注册在 TS 里只推迟 `register` 本身（`applyRuntimeChange` 的
/// pending 队列），检查仍立即对当前注册表求值；loader-facade 在“加载期延后提交”时用本函数，
/// 并在 commit 时把返回值交给 [`McpServerRegistry::register`]。
///
/// 提交边界必须是**单个扩展工厂**：TS 的 `initializeExtension` 在工厂返回后立刻
/// `load.commit()`，而 `loadExtensionsInternal` 顺序加载各扩展（`loader.ts:626-631`、
/// `694-701`），因此后一个扩展的检查能看到前一个扩展已提交的 server（同名会被拒绝），
/// 只有**同一个工厂内**的 pending 项彼此不可见。不要让多个扩展共用一条全局 pending 队列。
///
/// `config` 在这里就完成校验与深拷贝（TS 的 `structuredClone(validated)` 同样发生在
/// API 调用时，而不是提交时）。
///
/// 返回值是一个**尚未写入注册表**的 [`RegisteredMcpServer`]；本函数没有副作用。检查基于
/// 调用时刻的注册表快照，提交时不会重新检查——同一工厂内的多条 pending 项因此可以共存
/// （见 [`register_mcp_server`] 对两种调用方式的说明）。
///
/// [`McpServerConfig`]: kiana_core::mcp_servers::McpServerConfig
pub fn prepare_mcp_server(
    registry: &McpServerRegistry,
    extension_path: &str,
    name: &str,
    config: &Value,
) -> Result<RegisteredMcpServer, McpRegistrationError> {
    let validated = validate_mcp_server_config(name, config).map_err(|message| {
        McpRegistrationError::Invalid {
            extension_path: extension_path.to_string(),
            message,
        }
    })?;

    if let Some(owner) = registry.get(name).map(|server| server.extension_path)
        && owner != extension_path
    {
        return Err(McpRegistrationError::AlreadyRegistered {
            name: name.to_string(),
            owner,
        });
    }

    // 只在 `-`/`_` 上不同的名字会共享 `mcp__` 命名空间。
    let namespace = mcp_namespace(name);
    if let Some(clash) = registry
        .list()
        .into_iter()
        .find(|server| server.name != name && mcp_namespace(&server.name) == namespace)
    {
        return Err(McpRegistrationError::NamespaceConflict {
            name: name.to_string(),
            other: clash.name,
        });
    }

    Ok(RegisteredMcpServer {
        name: name.to_string(),
        config: validated,
        extension_path: extension_path.to_string(),
    })
}

/// 校验并立即注册一个扩展 MCP server（`prepare_mcp_server` + `registry.register`）。
///
/// TS: `loader.ts#registerMcpServer` 的**非加载期**路径（`applyRuntimeChange` 在
/// `state === "active"` 时直接执行 `register`）。检查与提交之间没有 pending 窗口：
/// 本函数写入的 server 对后续任何调用（包括同一扩展）立即可见。
///
/// 成功时 `McpServerRegistry::register` 会在写完注册表后同步调用变更监听器（每次注册
/// 一次，即使同名替换）；失败时直接返回错误，注册表不变、监听器不触发
/// （`tests/mcp_registration.rs` 锁定）。
///
/// 因此 loader-facade **不要**用本函数模拟工厂内的注册：TS 在加载期把整个 `register`
/// 延后到工厂返回，同一工厂先注册 `my-server` 再注册 `my_server` 时两项都只在 pending
/// 队列里，彼此不可见因而都能通过检查；若提前落库，第二项会被命名空间检查拒绝。
/// 工厂内应改用 [`prepare_mcp_server`]，并在工厂返回时按序提交。
pub fn register_mcp_server(
    registry: &McpServerRegistry,
    extension_path: &str,
    name: &str,
    config: &Value,
) -> Result<(), McpRegistrationError> {
    let server = prepare_mcp_server(registry, extension_path, name, config)?;
    registry.register(server);
    Ok(())
}
