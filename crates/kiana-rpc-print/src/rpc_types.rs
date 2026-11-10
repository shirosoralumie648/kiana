//! modes/rpc/rpc-types.ts：RPC 协议的 wire 模型（纯类型，无 I/O）。
//!
//! TS: `packages/coding-agent/src/modes/rpc/rpc-types.ts`（spec §2.5）。
//!
//! # 这个文件解决什么问题
//!
//! RPC 模式（[`crate::rpc_mode`]）与客户端（[`crate::rpc_client`]）之间靠 stdin/stdout 上的
//! JSON 行通信。这些 JSON 行的字段名、可选性、键序甚至“键是否存在”都属于当前稳定契约，改动需同步测试
//! （`ARCHITECTURE.md` 要求快照里的 JSON 文本逐字节相同），所以这里把每条 wire 形状
//! 建模成带 `serde` 属性的 enum/struct，而不是随手用 `serde_json::Value` 拼。没有这个文件，
//! 客户端解析 `get_state` 的返回、服务端组装 `response` 记录时就要各自手写取/拼字段的代码，
//! 键序与“`null` vs 缺键”的细节会散落各处且无法被测试锁定。
//!
//! # 在 crate 中的位置与使用方式
//!
//! - 服务端（[`crate::rpc_mode`]）写 `success` / `error` 记录用 [`RpcResponse`]，写扩展 UI 请求
//!   用 [`RpcExtensionUIRequest`]；但命令分发本身操作的是原始 [`Value`]，连
//!   `extension_ui_response` 也是按 `type`/`id` 原样 resolve 回 oneshot（见下“设计约束”最后
//!   一条）。
//! - 客户端（[`crate::rpc_client`]）把子进程 stdout 上的响应对象经 `response_from_value`
//!   转成 [`RpcResponse`]，把 `get_state` 的 `data` 反序列化成 [`RpcSessionState`]。
//! - 再导出面：[`crate::rpc_mode`] 与 [`crate::index`] 各再导出其中 5 个类型（`RpcCommand`、
//!   `RpcExtensionUIRequest`、`RpcExtensionUIResponse`、`RpcResponse`、`RpcSessionState`）；
//!   crate 根的 `pub use rpc_types::*` 暴露本文件的全部公开项，`kiana-coding-agent` 的
//!   `sdk_exports` 再把那 5 个名字暴露给 SDK 用户，因此本文件的类型是公开契约的一部分。
//!
//! 本文件不做任何 I/O。写出时不要直接 `serde_json::to_string`：stdout 记录须经
//! [`crate::json_event::serialize_wire_line`] / [`crate::json_event::serialize_event_line`]，
//! 需要跨进程发往子进程 stdin 的 JSON 行用 [`crate::jsonl::serialize_json_line`]（见 crate 根
//! 的写出纪律）。这层统一负责 JS 的数字格式与字符串转义，绕过它就不再逐字节对齐 TS。
//!
//! 导出符号 7 个：`RpcCommand`（33 个 `type` 分支）、`RpcSlashCommand`、`RpcSessionState`、
//! `RpcResponse`、`RpcExtensionUIRequest`（9 种 method）、`RpcExtensionUIResponse`（3 种）、
//! `RpcCommandType`。
//!
//! ## 设计约束
//!
//! - **不使用 `#[serde(flatten)]`**：serde 的 flatten 用中间 `Content` 重排键，破坏 wire 键序。
//!   [`RpcExtensionUIRequest`] 因此用“所有 method 字段的超集结构 + 单一插入顺序”表达
//!   §2.5.5 的 9 种形状（每个 method 的键序都恰好是声明序的子序列）。
//!   另外本节所有“声明序即输出序”的说法都依赖工作区启用的 `serde_json` 的
//!   `preserve_order` 特性（根 `Cargo.toml`）；没有它，`Value`/`Map` 会把键按字典序重排。
//! - `id` 在 [`RpcCommand`] 的每个分支里都是**最后一个**字段并跳过 `None`（TS 的 `id?: string`
//!   在 `JSON.stringify` 时被省略）。
//! - [`RpcResponse`] 的 `id`/`command`/`data` 用 [`Value`]：显式 `null`、非字符串 id/command、
//!   `data: null` 与 `data: undefined` 的区别都必须原样保留（`Option<Value>` 的 `None` 表示
//!   键缺失，`Some(Value::Null)` 表示键存在且为 `null`）。`error: ""` 也必须保留。
//! - 服务端本身解析原始 [`Value`]（`handleInputLine`），不会反序列化成这些类型；`RpcSessionState`
//!   与 `RpcExtensionUIResponse` 的写出同理（服务端手工构造 `Map` 以控制键序）。这些 `derive`
//!   服务于公开契约、客户端实现与测试。因此本文件的 `Deserialize` 实现不做字段校验——缺失
//!   必需字段在 TS 里表现为运行期 `undefined`（随后在业务代码里抛 TypeError），Rust 侧则在
//!   `serde` 层报错；实际的服务端分发路径不经过这里（spec §2.5.1）。

use serde::{Deserialize, Serialize};
use serde_json::Value;

use kiana_agent_core::QueueMode;
use kiana_agent_core::ThinkingLevel;
use kiana_ai::types::{ImageContent, Model};
use kiana_ext_api::events::StreamingBehavior;

/// `RpcSlashCommand`：可直接通过 `prompt` 触发的命令。
///
/// TS: `rpc-types.ts#RpcSlashCommand`。与 `core/slash-commands.ts#SlashCommandInfo` 结构相同，
/// 按 `ARCHITECTURE.md` §3.5 直接复用 `kiana-core` 的唯一定义（不复制第二份）。
///
/// 它是 `get_commands` 响应 `data.commands[]` 的元素类型。被复用的类型本身带
/// `rename_all = "camelCase"`，字段顺序为 `name`、`description`、`source`、`sourceInfo`
/// （`description` 为 `None` 时省略键）。不过服务端并不序列化这个别名：`rpc_mode.rs` 的
/// `slash_command_value` 手工构造 `Map` 得到同样的键序，因为 `source` 在那里是运行时字符串。
/// 改动 `kiana-core` 的定义会直接改变本别名的 wire 形状，属跨 crate 的冻结契约。
pub use kiana_core::slash_commands::SlashCommandInfo as RpcSlashCommand;

/// `RpcCommandType`：33 个命令 `type` 字面量。
///
/// TS: `RpcCommand["type"]`（spec 附录 B，按字典序）。
///
/// 与 [`RpcCommand`] 的变体一一对应（变体名 = `snake_case` 后的 `type` 字面量）。它单独存在
/// 是因为 TS 侧该类型是字面量联合的提取（`RpcCommand["type"]`），而 Rust 无法从内部标签枚举
/// 直接提取枚举；`as_str` 用手工 `match` 给出 wire 文本，`Display` 转发它。服务端分发不用
/// 这个枚举（直接比较字符串），因此新增命令时要同时改三处：本枚举、`as_str`、`RpcCommand`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RpcCommandType {
    /// `"abort"`。
    Abort,
    /// `"abort_bash"`。
    AbortBash,
    /// `"abort_retry"`。
    AbortRetry,
    /// `"bash"`。
    Bash,
    /// `"clear_queue"`。
    ClearQueue,
    /// `"clone"`。
    Clone,
    /// `"compact"`。
    Compact,
    /// `"cycle_model"`。
    CycleModel,
    /// `"cycle_thinking_level"`。
    CycleThinkingLevel,
    /// `"export_html"`。
    ExportHtml,
    /// `"follow_up"`。
    FollowUp,
    /// `"fork"`。
    Fork,
    /// `"get_available_models"`。
    GetAvailableModels,
    /// `"get_available_thinking_levels"`。
    GetAvailableThinkingLevels,
    /// `"get_commands"`。
    GetCommands,
    /// `"get_entries"`。
    GetEntries,
    /// `"get_fork_messages"`。
    GetForkMessages,
    /// `"get_last_assistant_text"`。
    GetLastAssistantText,
    /// `"get_messages"`。
    GetMessages,
    /// `"get_session_stats"`。
    GetSessionStats,
    /// `"get_state"`。
    GetState,
    /// `"get_tree"`。
    GetTree,
    /// `"new_session"`。
    NewSession,
    /// `"prompt"`。
    Prompt,
    /// `"set_auto_compaction"`。
    SetAutoCompaction,
    /// `"set_auto_retry"`。
    SetAutoRetry,
    /// `"set_follow_up_mode"`。
    SetFollowUpMode,
    /// `"set_model"`。
    SetModel,
    /// `"set_session_name"`。
    SetSessionName,
    /// `"set_steering_mode"`。
    SetSteeringMode,
    /// `"set_thinking_level"`。
    SetThinkingLevel,
    /// `"steer"`。
    Steer,
    /// `"switch_session"`。
    SwitchSession,
}

impl RpcCommandType {
    /// 全部 33 个命令类型（顺序同 spec 附录 B 的字典序）。
    ///
    /// 目前唯一的仓库内消费者是 `command_types_cover_33_literals` 测试（断言长度 33 且字面量
    /// 互不重复）；真实的分发逻辑在 `rpc_mode.rs` 里直接比较 `type` 字符串。新增命令时必须
    /// 同步改这里、`as_str` 与 `RpcCommand` 分支，否则测试会失败。
    pub const ALL: [RpcCommandType; 33] = [
        RpcCommandType::Abort,
        RpcCommandType::AbortBash,
        RpcCommandType::AbortRetry,
        RpcCommandType::Bash,
        RpcCommandType::ClearQueue,
        RpcCommandType::Clone,
        RpcCommandType::Compact,
        RpcCommandType::CycleModel,
        RpcCommandType::CycleThinkingLevel,
        RpcCommandType::ExportHtml,
        RpcCommandType::FollowUp,
        RpcCommandType::Fork,
        RpcCommandType::GetAvailableModels,
        RpcCommandType::GetAvailableThinkingLevels,
        RpcCommandType::GetCommands,
        RpcCommandType::GetEntries,
        RpcCommandType::GetForkMessages,
        RpcCommandType::GetLastAssistantText,
        RpcCommandType::GetMessages,
        RpcCommandType::GetSessionStats,
        RpcCommandType::GetState,
        RpcCommandType::GetTree,
        RpcCommandType::NewSession,
        RpcCommandType::Prompt,
        RpcCommandType::SetAutoCompaction,
        RpcCommandType::SetAutoRetry,
        RpcCommandType::SetFollowUpMode,
        RpcCommandType::SetModel,
        RpcCommandType::SetSessionName,
        RpcCommandType::SetSteeringMode,
        RpcCommandType::SetThinkingLevel,
        RpcCommandType::Steer,
        RpcCommandType::SwitchSession,
    ];

    /// wire 上的 `type` 字面量。
    pub fn as_str(self) -> &'static str {
        match self {
            RpcCommandType::Abort => "abort",
            RpcCommandType::AbortBash => "abort_bash",
            RpcCommandType::AbortRetry => "abort_retry",
            RpcCommandType::Bash => "bash",
            RpcCommandType::ClearQueue => "clear_queue",
            RpcCommandType::Clone => "clone",
            RpcCommandType::Compact => "compact",
            RpcCommandType::CycleModel => "cycle_model",
            RpcCommandType::CycleThinkingLevel => "cycle_thinking_level",
            RpcCommandType::ExportHtml => "export_html",
            RpcCommandType::FollowUp => "follow_up",
            RpcCommandType::Fork => "fork",
            RpcCommandType::GetAvailableModels => "get_available_models",
            RpcCommandType::GetAvailableThinkingLevels => "get_available_thinking_levels",
            RpcCommandType::GetCommands => "get_commands",
            RpcCommandType::GetEntries => "get_entries",
            RpcCommandType::GetForkMessages => "get_fork_messages",
            RpcCommandType::GetLastAssistantText => "get_last_assistant_text",
            RpcCommandType::GetMessages => "get_messages",
            RpcCommandType::GetSessionStats => "get_session_stats",
            RpcCommandType::GetState => "get_state",
            RpcCommandType::GetTree => "get_tree",
            RpcCommandType::NewSession => "new_session",
            RpcCommandType::Prompt => "prompt",
            RpcCommandType::SetAutoCompaction => "set_auto_compaction",
            RpcCommandType::SetAutoRetry => "set_auto_retry",
            RpcCommandType::SetFollowUpMode => "set_follow_up_mode",
            RpcCommandType::SetModel => "set_model",
            RpcCommandType::SetSessionName => "set_session_name",
            RpcCommandType::SetSteeringMode => "set_steering_mode",
            RpcCommandType::SetThinkingLevel => "set_thinking_level",
            RpcCommandType::Steer => "steer",
            RpcCommandType::SwitchSession => "switch_session",
        }
    }
}

impl std::fmt::Display for RpcCommandType {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// `RpcCommand`：stdin 上收到的命令（33 个分支）。
///
/// TS: `rpc-types.ts#RpcCommand`。每个分支的可选 `id` 是**最后**一个字段并在序列化时省略
/// （TS 的 `id?: string`）。
///
/// 三条 `serde` 属性各管一层，改任何一条都会改变 wire：
/// - `tag = "type"`：内部标签（internally tagged），`type` 键**最先**写出，且反序列化时
///   按该键选变体；缺失或未知的 `type` 直接报错，不会落到某个默认分支。
/// - `rename_all = "snake_case"`：作用于**变体名**，即 `GetState` -> `"get_state"`。
/// - `rename_all_fields = "camelCase"`：作用于**各变体的字段名**，即
///   `streaming_behavior` -> `"streamingBehavior"`。两者互不影响，不要合并成一条。
///
/// 服务端收到的一行原始 JSON 并不会反序列化成这个 enum（`rpc_mode.rs` 直接读 [`Value`] 的
/// `type` 字段来分发），所以这里的变体顺序不影响服务端行为，只影响 Rust 调用方构造命令时
/// 的 wire 键序。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum RpcCommand {
    /// `prompt`。
    Prompt {
        /// 提示文本。
        message: String,
        /// 图片附件（原样透传，不校验）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        images: Option<Vec<ImageContent>>,
        /// 流式期间的投递方式。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        streaming_behavior: Option<StreamingBehavior>,
        /// 请求 id。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    /// `steer`。
    Steer {
        /// 提示文本。
        message: String,
        /// 图片附件。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        images: Option<Vec<ImageContent>>,
        /// 请求 id。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    /// `follow_up`。
    FollowUp {
        /// 提示文本。
        message: String,
        /// 图片附件。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        images: Option<Vec<ImageContent>>,
        /// 请求 id。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    /// `abort`。
    Abort {
        /// 请求 id。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    /// `clear_queue`。
    ClearQueue {
        /// 请求 id。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    /// `new_session`。
    NewSession {
        /// 父会话路径。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_session: Option<String>,
        /// 请求 id。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    /// `get_state`。
    GetState {
        /// 请求 id。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    /// `set_model`。
    SetModel {
        /// provider id。
        provider: String,
        /// 模型 id。
        model_id: String,
        /// 请求 id。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    /// `cycle_model`。
    CycleModel {
        /// 请求 id。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    /// `get_available_models`。
    GetAvailableModels {
        /// 请求 id。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    /// `set_thinking_level`。
    SetThinkingLevel {
        /// 目标级别（本类型不校验取值）。
        ///
        /// 与 [`RpcSessionState::thinking_level`] 是同一类型：`kiana_agent_core` 重导出的
        /// `kiana_ai::types::ModelThinkingLevel`（含 `"off"`，`rename_all = "lowercase"`）。
        /// 注意只有走本 enum 的 `Deserialize` 才会拒绝非法字面量；服务端分发路径不反序列化
        /// 本类型，而是用 `rpc_mode.rs` 的 `parse_thinking_level` 把未知/缺失值落到 `off`。
        level: ThinkingLevel,
        /// 请求 id。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    /// `cycle_thinking_level`。
    CycleThinkingLevel {
        /// 请求 id。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    /// `get_available_thinking_levels`。
    GetAvailableThinkingLevels {
        /// 请求 id。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    /// `set_steering_mode`。
    SetSteeringMode {
        /// `"all" | "one-at-a-time"`。
        mode: QueueMode,
        /// 请求 id。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    /// `set_follow_up_mode`。
    SetFollowUpMode {
        /// `"all" | "one-at-a-time"`。
        mode: QueueMode,
        /// 请求 id。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    /// `compact`。
    Compact {
        /// 自定义压缩指令。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        custom_instructions: Option<String>,
        /// 请求 id。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    /// `set_auto_compaction`。
    SetAutoCompaction {
        /// 是否启用。
        enabled: bool,
        /// 请求 id。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    /// `set_auto_retry`。
    SetAutoRetry {
        /// 是否启用。
        enabled: bool,
        /// 请求 id。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    /// `abort_retry`。
    AbortRetry {
        /// 请求 id。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    /// `bash`。
    Bash {
        /// 要执行的命令。
        command: String,
        /// 是否从上下文排除本次执行。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exclude_from_context: Option<bool>,
        /// 请求 id。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    /// `abort_bash`。
    AbortBash {
        /// 请求 id。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    /// `get_session_stats`。
    GetSessionStats {
        /// 请求 id。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    /// `export_html`。
    ExportHtml {
        /// 输出路径。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output_path: Option<String>,
        /// 请求 id。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    /// `switch_session`。
    SwitchSession {
        /// 会话文件路径。
        session_path: String,
        /// 请求 id。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    /// `fork`。
    Fork {
        /// 分叉点条目 id。
        entry_id: String,
        /// 请求 id。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    /// `clone`。
    Clone {
        /// 请求 id。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    /// `get_fork_messages`。
    GetForkMessages {
        /// 请求 id。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    /// `get_entries`。
    GetEntries {
        /// 起始条目 id。服务端按严格相等在条目列表中查找；找不到或不是字符串时整体报错
        /// `Entry not found: <js_string>`（`rpc_mode.rs` 的 `get_entries` 分支）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        since: Option<String>,
        /// 请求 id。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    /// `get_tree`。
    GetTree {
        /// 请求 id。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    /// `get_last_assistant_text`。
    GetLastAssistantText {
        /// 请求 id。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    /// `set_session_name`。
    SetSessionName {
        /// 新名字（服务端会 `trim` 后判空）。
        name: String,
        /// 请求 id。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    /// `get_messages`。
    GetMessages {
        /// 请求 id。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    /// `get_commands`。
    GetCommands {
        /// 请求 id。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
}

/// `RpcResponse`：stdout 上的响应记录。
///
/// TS: `rpc-types.ts#RpcResponse`（33 个成功分支 + 1 个通用错误分支）。所有可选键在
/// `None` 时省略；`Some(Value::Null)` 保留显式 `null`。反序列化同样保留“键存在且为
/// `null`”与“键缺失”的区别（`deserialize_present_json`，见本文件末尾的私有辅助函数）：
/// 客户端拿到的 `data: null` 与 `undefined` 在 TS 里语义不同（`getData` 返回 `null` vs
/// `undefined`）；`rpc_client.rs` 的 `optional_data` 把“无 `data` 键”与 `data: null` 都归为
/// `None`，而 `get_data` 仍保留二者区别。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcResponse {
    /// 请求 id（客户端原样带回；解析错误记录没有 id）。
    ///
    /// 用 [`Value`] 而非 `String`：TS 客户端发来的 `id` 可以是任意 JSON 值，
    /// 服务端只负责原样回带，不校验类型。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_present_json"
    )]
    pub id: Option<Value>,
    /// 固定为 `"response"`。
    #[serde(rename = "type")]
    pub kind: RpcResponseType,
    /// 命令名；解析错误为 `"parse"`，未知命令为未知的 `type` 字符串。同样不限于字符串：
    /// 顶层非对象时 TS 的 `command.type` 是 `undefined`，回带的 `command` 键被省略。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_present_json"
    )]
    pub command: Option<Value>,
    /// 是否成功。
    pub success: bool,
    /// 成功时的数据（`None` = 无 `data` 键；`Some(Null)` = `"data":null`）。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_present_json"
    )]
    pub data: Option<Value>,
    /// 失败时的错误文本（`Some("")` 也保留）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// 已序列化的一整行 JSON（含结尾 `\n`）。
    ///
    /// 用于 `get_tree`/`get_entries`/`get_messages` 这类可能包含任意深嵌套会话数据的
    /// 响应：这些值在 64 MiB 栈的线程上构造、重排并序列化，主线程只拿到一行浅字符串，
    /// 避免深 `Value` 的递归 `Drop` 打爆默认栈。`handle_input_line` 检测到该字段时直接
    /// 写出，不再走 `serialize_wire_line`。
    #[serde(skip)]
    pub raw_line: Option<String>,
}

impl RpcResponse {
    /// 成功响应。`data` 为 `None` 时不写 `data` 键（TS 的 `data: undefined`），
    /// `Some(Value::Null)` 写出 `"data":null`——两者在 wire 上不同（见 [`RpcResponse`] 文档）。
    pub fn success(id: Option<Value>, command: Option<Value>, data: Option<Value>) -> Self {
        Self {
            id,
            kind: RpcResponseType::Response,
            command,
            success: true,
            data,
            error: None,
            raw_line: None,
        }
    }

    /// 成功响应，但数据已经由大栈线程序列化成完整的一行 JSON（含 `\n`）。
    ///
    /// `raw_line` 里的文本必须已经包含 `id`/`type`/`command`/`success`/`data` 全部字段；
    /// 写出路径会原样发送，不再做键序重排或 `JSON.stringify`。
    pub fn success_raw_line(line: String) -> Self {
        Self {
            id: None,
            kind: RpcResponseType::Response,
            command: None,
            success: true,
            data: None,
            error: None,
            raw_line: Some(line),
        }
    }

    /// 失败响应。`error` 接受空串并原样保留——客户端的 `get_data` 直接用该文本构造错误，
    /// TS 侧 `Error.message` 也可以是空串，不能把它当成“无错误”而丢弃。
    /// 服务端实际走的是 `rpc_mode.rs` 里直接构造本结构体的 `error_response`，参数来源有两类：
    /// JSON 解析失败为 `(None, Some("parse"))`，命令处理返回 `Err` 时为原始 `id`/`type`
    /// （顶层非对象时二者可能都是 `None`）。
    pub fn error(id: Option<Value>, command: Option<Value>, error: impl Into<String>) -> Self {
        Self {
            id,
            kind: RpcResponseType::Response,
            command,
            success: false,
            data: None,
            error: Some(error.into()),
            raw_line: None,
        }
    }
}

/// `RpcResponse.type` 的固定字面量。
///
/// 只有一个变体，却仍写成 `enum`：`Serialize`/`Deserialize` 需要一个只接受 `"response"`
/// 的类型来占住 `type` 键（TS 字面量 `"response"`），`#[serde(rename = "response")]` 也把反
/// 序列化限制在该字面量上。`derive(Default)` + `#[default]` 只提供
/// `RpcResponseType::default() == RpcResponseType::Response`；仓库内可见的构造点——例如
/// `rpc_mode.rs` 的 `error_response`、`rpc_client.rs` 的 `response_from_value`，以及本文件
/// `RpcResponse::success`/`RpcResponse::error`——都显式写
/// `RpcResponseType::Response`。不要把它泛化成多变体枚举——wire 上只有一种响应记录。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum RpcResponseType {
    /// `"response"`。
    #[default]
    #[serde(rename = "response")]
    Response,
}

/// `RpcSessionState`：`get_state` 的 `data`。
///
/// TS: `rpc-types.ts#RpcSessionState`，字段顺序 = 声明序 = 构造字面量序；
/// `model` / `sessionFile` / `sessionName` 为 `undefined` 时键缺失。
///
/// 注意分工：服务端（`rpc_mode.rs` 的 `session_state_value`）手工构造 `serde_json::Map`
/// 以精确控制键序，并处理 `Deserialize` 无法表达的特例：`steeringMode` / `followUpMode`
/// 在 setter 被显式设为 `undefined` 时**省略键**，其余情况写会话 getter 的值。
/// 它也**不经过本结构体**；客户端（`rpc_client.rs` 的 `get_state`）才用 [`Deserialize`] 把
/// `data` 还原成 [`RpcSessionState`]。因此修改字段顺序时两处都要核对。
///
/// 失败模式：反序列化要求全部非 `Option` 字段都存在且类型正确，否则客户端的 `get_state`
/// 返回 `Invalid response for get_state: ...`（`rpc_client.rs` 的 `deserialize_data` 包装了
/// `serde_json` 的错误）。服务端给自己的响应不会触发该路径，但手写/伪造的响应会。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcSessionState {
    /// 当前模型。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<Model>,
    /// thinking level。
    ///
    /// 这里的 [`ThinkingLevel`] 是 `kiana_agent_core` 的重导出，实际指向
    /// `kiana_ai::types::ModelThinkingLevel`（含 `"off"`，序列化为小写），与
    /// `set_thinking_level` 的 `level` 同一类型；不要与不含 `off` 的
    /// `kiana_ai::types::ThinkingLevel` 混淆。
    pub thinking_level: ThinkingLevel,
    /// 是否正在流式输出。
    pub is_streaming: bool,
    /// 是否正在压缩。
    pub is_compacting: bool,
    /// steering 队列模式。
    pub steering_mode: QueueMode,
    /// follow-up 队列模式。
    pub follow_up_mode: QueueMode,
    /// 当前会话文件。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_file: Option<String>,
    /// 会话 id。
    pub session_id: String,
    /// 会话名。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_name: Option<String>,
    /// 是否启用自动压缩。
    pub auto_compaction_enabled: bool,
    /// 消息数量。
    pub message_count: u64,
    /// 待处理消息数量。
    pub pending_message_count: u64,
}

/// `RpcExtensionUIRequest.method`：9 种 UI 请求。
///
/// TS 源：`rpc-types.ts#RpcExtensionUIRequest`；字面量拼写是当前稳定契约
/// （`setStatus` / `setWidget` / `setTitle` / `set_editor_text`），改动需同步测试。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RpcExtensionUIMethod {
    /// `"select"`。
    #[serde(rename = "select")]
    Select,
    /// `"confirm"`。
    #[serde(rename = "confirm")]
    Confirm,
    /// `"input"`。
    #[serde(rename = "input")]
    Input,
    /// `"editor"`。
    #[serde(rename = "editor")]
    Editor,
    /// `"notify"`。
    #[serde(rename = "notify")]
    Notify,
    /// `"setStatus"`。
    #[serde(rename = "setStatus")]
    SetStatus,
    /// `"setWidget"`。
    #[serde(rename = "setWidget")]
    SetWidget,
    /// `"setTitle"`。
    #[serde(rename = "setTitle")]
    SetTitle,
    /// `"set_editor_text"`。
    #[serde(rename = "set_editor_text")]
    SetEditorText,
}

impl RpcExtensionUIMethod {
    /// wire 上的 `method` 字面量。
    pub fn as_str(self) -> &'static str {
        match self {
            RpcExtensionUIMethod::Select => "select",
            RpcExtensionUIMethod::Confirm => "confirm",
            RpcExtensionUIMethod::Input => "input",
            RpcExtensionUIMethod::Editor => "editor",
            RpcExtensionUIMethod::Notify => "notify",
            RpcExtensionUIMethod::SetStatus => "setStatus",
            RpcExtensionUIMethod::SetWidget => "setWidget",
            RpcExtensionUIMethod::SetTitle => "setTitle",
            RpcExtensionUIMethod::SetEditorText => "set_editor_text",
        }
    }
}

impl std::fmt::Display for RpcExtensionUIMethod {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// `notify` 的 `notifyType`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RpcNotifyType {
    /// `"info"`。
    Info,
    /// `"warning"`。
    Warning,
    /// `"error"`。
    Error,
}

/// `setWidget` 的 `widgetPlacement`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RpcWidgetPlacement {
    /// `"aboveEditor"`。
    AboveEditor,
    /// `"belowEditor"`。
    BelowEditor,
}

/// `RpcExtensionUIRequest.type` 的固定字面量。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum RpcExtensionUIRequestType {
    /// `"extension_ui_request"`。
    #[default]
    #[serde(rename = "extension_ui_request")]
    Request,
}

/// `RpcExtensionUIRequest`：stdout 上的扩展 UI 请求。
///
/// TS: `rpc-types.ts#RpcExtensionUIRequest`（9 种 method 的联合）。Rust 侧用一个“字段超集 +
/// 固定顺序”的 struct：`type`、`id`、`method` 之后按 §2.5.5 的顺序排列，未用到的字段为
/// `None` 并被省略。这样每个 method 的输出键序都恰好是文档顺序：
///
/// - `select`：`title`、`options`、`timeout?`
/// - `confirm`：`title`、`message`、`timeout?`
/// - `input`：`title`、`placeholder?`、`timeout?`
/// - `editor`：`title`、`prefill?`
/// - `notify`：`message`、`notifyType?`
/// - `setStatus`：`statusKey`、`statusText`
/// - `setWidget`：`widgetKey`、`widgetLines`、`widgetPlacement?`
/// - `setTitle`：`title`
/// - `set_editor_text`：`text`
///
/// 约束与失败模式：结构体本身**不校验 method 与字段的搭配**。用 `new` 手工填字段时若填错
/// （例如 `Select` 却没填 `options`），序列化不会报错，只会产出一条客户端无法正确解析的
/// 请求；正常路径应使用下面的构造函数。服务端写这条记录走 `rpc_mode.rs` 的
/// `RpcUiContext::send` -> `write_record` -> [`crate::json_event::serialize_wire_line`]。
///
/// `timeout: Some(0)` 也会写出该键（TS 只用 truthy 判定是否设置定时器，但请求对象里仍带 0）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcExtensionUIRequest {
    /// 固定为 `"extension_ui_request"`。
    #[serde(rename = "type")]
    pub kind: RpcExtensionUIRequestType,
    /// 请求 id。
    pub id: String,
    /// 请求方法。
    pub method: RpcExtensionUIMethod,
    /// `select`/`confirm`/`input`/`editor`/`setTitle` 的标题。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// `select` 的选项。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<String>>,
    /// `confirm`/`notify` 的消息正文。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// `input` 的占位文本。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    /// `select`/`confirm`/`input` 的超时毫秒数。
    ///
    /// 用 `f64` 对齐 TS 的 `number`（可以是小数）。整数值按 JS 数字规则写成无小数点形式
    /// （`extension_ui_request_key_order_per_method` 测试锁定 `"timeout":10000`）；这个格式
    /// 由 wire 层的 `kiana_json::js_num::format_js_number` 决定，不是 `serde_json` 的默认
    /// 数字格式，所以写出必须经 [`crate::json_event::serialize_wire_line`]。不要改成整数类型，
    /// 否则小数超时会被截断。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<f64>,
    /// `editor` 的预填文本。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefill: Option<String>,
    /// `notify` 的类别。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notify_type: Option<RpcNotifyType>,
    /// `setStatus` 的状态键。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_key: Option<String>,
    /// `setStatus` 的状态文本（`undefined` -> 键缺失）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_text: Option<String>,
    /// `setWidget` 的部件键。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub widget_key: Option<String>,
    /// `setWidget` 的部件行（`undefined` -> 键缺失）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub widget_lines: Option<Vec<String>>,
    /// `setWidget` 的位置。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub widget_placement: Option<RpcWidgetPlacement>,
    /// `setTitle` / `set_editor_text` 的文本。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

impl RpcExtensionUIRequest {
    /// 只有 `type`/`id`/`method` 的空请求；其余字段用公开字段赋值。
    pub fn new(id: impl Into<String>, method: RpcExtensionUIMethod) -> Self {
        Self {
            kind: RpcExtensionUIRequestType::Request,
            id: id.into(),
            method,
            title: None,
            options: None,
            message: None,
            placeholder: None,
            timeout: None,
            prefill: None,
            notify_type: None,
            status_key: None,
            status_text: None,
            widget_key: None,
            widget_lines: None,
            widget_placement: None,
            text: None,
        }
    }

    /// `select` 请求。
    pub fn select(
        id: impl Into<String>,
        title: impl Into<String>,
        options: Vec<String>,
        timeout: Option<f64>,
    ) -> Self {
        Self {
            title: Some(title.into()),
            options: Some(options),
            timeout,
            ..Self::new(id, RpcExtensionUIMethod::Select)
        }
    }

    /// `confirm` 请求。
    pub fn confirm(
        id: impl Into<String>,
        title: impl Into<String>,
        message: impl Into<String>,
        timeout: Option<f64>,
    ) -> Self {
        Self {
            title: Some(title.into()),
            message: Some(message.into()),
            timeout,
            ..Self::new(id, RpcExtensionUIMethod::Confirm)
        }
    }

    /// `input` 请求。
    pub fn input(
        id: impl Into<String>,
        title: impl Into<String>,
        placeholder: Option<String>,
        timeout: Option<f64>,
    ) -> Self {
        Self {
            title: Some(title.into()),
            placeholder,
            timeout,
            ..Self::new(id, RpcExtensionUIMethod::Input)
        }
    }

    /// `editor` 请求（无 timeout）。
    pub fn editor(
        id: impl Into<String>,
        title: impl Into<String>,
        prefill: Option<String>,
    ) -> Self {
        Self {
            title: Some(title.into()),
            prefill,
            ..Self::new(id, RpcExtensionUIMethod::Editor)
        }
    }

    /// `notify` 请求。
    pub fn notify(
        id: impl Into<String>,
        message: impl Into<String>,
        notify_type: Option<RpcNotifyType>,
    ) -> Self {
        Self {
            message: Some(message.into()),
            notify_type,
            ..Self::new(id, RpcExtensionUIMethod::Notify)
        }
    }

    /// `setStatus` 请求。
    pub fn set_status(
        id: impl Into<String>,
        status_key: impl Into<String>,
        status_text: Option<String>,
    ) -> Self {
        Self {
            status_key: Some(status_key.into()),
            status_text,
            ..Self::new(id, RpcExtensionUIMethod::SetStatus)
        }
    }

    /// `setWidget` 请求。
    pub fn set_widget(
        id: impl Into<String>,
        widget_key: impl Into<String>,
        widget_lines: Option<Vec<String>>,
        widget_placement: Option<RpcWidgetPlacement>,
    ) -> Self {
        Self {
            widget_key: Some(widget_key.into()),
            widget_lines,
            widget_placement,
            ..Self::new(id, RpcExtensionUIMethod::SetWidget)
        }
    }

    /// `setTitle` 请求。
    pub fn set_title(id: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            title: Some(title.into()),
            ..Self::new(id, RpcExtensionUIMethod::SetTitle)
        }
    }

    /// `set_editor_text` 请求。
    pub fn set_editor_text(id: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            text: Some(text.into()),
            ..Self::new(id, RpcExtensionUIMethod::SetEditorText)
        }
    }
}

/// 反序列化“键存在”的 JSON 值：显式 `null` -> `Some(Value::Null)`，键缺失 -> `None`。
///
/// 标准 `Option<Value>` 会把 `null` 也变成 `None`，但 TS 的 `"value" in response` /
/// `"cancelled" in response` 只判断键是否存在（spec §2.5.6 的 `parseResponse`）。
/// 与 `#[serde(default, skip_serializing_if = ...)]` 组合后，这个区别在序列化与反序列化
/// 两个方向上都得以保留（`response_deserialization_preserves_explicit_null_presence` 测试
/// 锁定往返行为）。注意：这也意味着 `"id": "x"` 之外任何 JSON 值（数字、对象）都会被接受，
/// 与 TS 的“不校验类型”一致。
fn deserialize_present_json<'de, D>(deserializer: D) -> Result<Option<Value>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Value::deserialize(deserializer).map(Some)
}

/// `RpcExtensionUIResponse.type` 的固定字面量。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum RpcExtensionUIResponseType {
    /// `"extension_ui_response"`。
    #[default]
    #[serde(rename = "extension_ui_response")]
    Response,
}

/// `RpcExtensionUIResponse`：stdin 上回给服务端的 UI 响应。
///
/// TS: `rpc-types.ts#RpcExtensionUIResponse`（3 种）。服务端把整个对象原样 `resolve`，
/// 由 `parseResponse` 按 `in` 判定字段是否存在；因此三个载荷字段都用 `Option<Value>` +
/// “存在性”反序列化。
///
/// 注意：服务端（`rpc_mode.rs` 的 `handle_input_line`）实际按 `type`/`id` 把**原始
/// [`Value`]** 原样 resolve 给 pending 的 oneshot 通道，不经过本类型的 `Deserialize`；
/// 本类型（与 `RpcCommand` 一样）服务于公开契约、客户端侧与测试，序列化路径上的键由
/// `skip_serializing_if` 省略规则决定。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcExtensionUIResponse {
    /// 固定为 `"extension_ui_response"`。
    #[serde(rename = "type")]
    pub kind: RpcExtensionUIResponseType,
    /// 对应请求的 id。
    pub id: String,
    /// `select`/`input`/`editor` 的返回值。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_present_json"
    )]
    pub value: Option<Value>,
    /// `confirm` 的返回值。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_present_json"
    )]
    pub confirmed: Option<Value>,
    /// 是否取消。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_present_json"
    )]
    pub cancelled: Option<Value>,
}

impl RpcExtensionUIResponse {
    /// 构造一条响应；三个载荷字段为 `None` 时不写键。
    ///
    /// 两个参数层次不要混淆：`None` -> 键缺失（TS 的 `undefined`），`Some(Value::Null)` ->
    /// 写出 `null`。`parseResponse`（spec §2.5.6）对二者的处理并不一致：
    /// `"value" in r ? r.value : undefined` 会把显式 `null` 原样返回、键缺失才得到
    /// `undefined`；而 `"cancelled" in r && r.cancelled` 只看真值，`null` 与缺键同为假。
    /// 构造时请按 TS 侧实际要表达的值选择。
    pub fn new(
        id: impl Into<String>,
        value: Option<Value>,
        confirmed: Option<Value>,
        cancelled: Option<Value>,
    ) -> Self {
        Self {
            kind: RpcExtensionUIResponseType::Response,
            id: id.into(),
            value,
            confirmed,
            cancelled,
        }
    }
}

#[cfg(test)]
mod tests {
    // 测试目标：锁定 wire 字节串。序列化断言一律经 `serialize_wire_line`（与生产路径一致），
    // 不用 `serde_json::to_string`；凡断言 wire 形状的用例都锁定**完整输出行**（含键序与
    // 可选键的省略），任何 serde 属性的改动（rename、skip、字段顺序）都会被检出。
    // 个别用例（如 `command_types_cover_33_literals` 只查字面量表、
    // `response_deserialization_preserves_explicit_null_presence` 也断言反序列化后的字段值）
    // 不在此列。
    use super::*;
    use kiana_agent_core::QueueMode;
    use serde_json::json;

    fn line(value: &impl Serialize) -> String {
        crate::json_event::serialize_wire_line(value).expect("serializable")
    }

    #[test]
    fn command_types_cover_33_literals() {
        let literals: Vec<&str> = RpcCommandType::ALL.iter().map(|k| k.as_str()).collect();
        assert_eq!(literals.len(), 33);
        let mut sorted = literals.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), 33);
    }

    #[test]
    fn command_serializes_id_last_and_omits_none() {
        let command = RpcCommand::SetModel {
            provider: "anthropic".into(),
            model_id: "claude".into(),
            id: Some("req_1".into()),
        };
        assert_eq!(
            line(&command),
            "{\"type\":\"set_model\",\"provider\":\"anthropic\",\"modelId\":\"claude\",\"id\":\"req_1\"}\n"
        );

        let command = RpcCommand::GetState { id: None };
        assert_eq!(line(&command), "{\"type\":\"get_state\"}\n");
    }

    #[test]
    fn command_field_renames_match_ts() {
        let command = RpcCommand::Prompt {
            message: "hi".into(),
            images: None,
            streaming_behavior: Some(StreamingBehavior::FollowUp),
            id: None,
        };
        assert_eq!(
            line(&command),
            "{\"type\":\"prompt\",\"message\":\"hi\",\"streamingBehavior\":\"followUp\"}\n"
        );

        let command = RpcCommand::NewSession {
            parent_session: Some("/tmp/s.jsonl".into()),
            id: None,
        };
        assert_eq!(
            line(&command),
            "{\"type\":\"new_session\",\"parentSession\":\"/tmp/s.jsonl\"}\n"
        );

        let command = RpcCommand::Bash {
            command: "ls".into(),
            exclude_from_context: Some(true),
            id: None,
        };
        assert_eq!(
            line(&command),
            "{\"type\":\"bash\",\"command\":\"ls\",\"excludeFromContext\":true}\n"
        );

        let command = RpcCommand::SetSteeringMode {
            mode: QueueMode::OneAtATime,
            id: None,
        };
        assert_eq!(
            line(&command),
            "{\"type\":\"set_steering_mode\",\"mode\":\"one-at-a-time\"}\n"
        );
    }

    #[test]
    fn response_preserves_null_and_empty_error() {
        let response = RpcResponse::success(
            Some(json!("req-1")),
            Some(json!("cycle_model")),
            Some(Value::Null),
        );
        assert_eq!(
            line(&response),
            "{\"id\":\"req-1\",\"type\":\"response\",\"command\":\"cycle_model\",\"success\":true,\"data\":null}\n"
        );

        let response = RpcResponse::error(None, Some(json!("parse")), "");
        assert_eq!(
            line(&response),
            "{\"type\":\"response\",\"command\":\"parse\",\"success\":false,\"error\":\"\"}\n"
        );
    }

    #[test]
    fn extension_ui_request_key_order_per_method() {
        assert_eq!(
            line(&RpcExtensionUIRequest::select(
                "id-1",
                "Pick",
                vec!["a".into(), "b".into()],
                Some(10000.0)
            )),
            "{\"type\":\"extension_ui_request\",\"id\":\"id-1\",\"method\":\"select\",\"title\":\"Pick\",\"options\":[\"a\",\"b\"],\"timeout\":10000}\n"
        );
        assert_eq!(
            line(&RpcExtensionUIRequest::set_status(
                "id-2",
                "my-ext",
                Some("Turn 3".into())
            )),
            "{\"type\":\"extension_ui_request\",\"id\":\"id-2\",\"method\":\"setStatus\",\"statusKey\":\"my-ext\",\"statusText\":\"Turn 3\"}\n"
        );
        assert_eq!(
            line(&RpcExtensionUIRequest::set_widget(
                "id-3",
                "my-ext",
                None,
                Some(RpcWidgetPlacement::AboveEditor)
            )),
            "{\"type\":\"extension_ui_request\",\"id\":\"id-3\",\"method\":\"setWidget\",\"widgetKey\":\"my-ext\",\"widgetPlacement\":\"aboveEditor\"}\n"
        );
        assert_eq!(
            line(&RpcExtensionUIRequest::editor("id-4", "Edit", None)),
            "{\"type\":\"extension_ui_request\",\"id\":\"id-4\",\"method\":\"editor\",\"title\":\"Edit\"}\n"
        );
    }

    #[test]
    fn response_deserialization_preserves_explicit_null_presence() {
        let response: RpcResponse = serde_json::from_value(json!({
            "id": null,
            "type": "response",
            "command": "cycle_model",
            "success": true,
            "data": null
        }))
        .expect("response");
        assert_eq!(response.id, Some(Value::Null));
        assert_eq!(response.command, Some(json!("cycle_model")));
        assert_eq!(response.data, Some(Value::Null));
        // 往返后仍是显式 null，而不是丢键。
        assert_eq!(
            line(&response),
            "{\"id\":null,\"type\":\"response\",\"command\":\"cycle_model\",\"success\":true,\"data\":null}\n"
        );

        // 键缺失仍是 None（omit）。
        let response: RpcResponse =
            serde_json::from_value(json!({ "type": "response", "success": true }))
                .expect("response");
        assert_eq!(response.id, None);
        assert_eq!(response.command, None);
        assert_eq!(response.data, None);
        assert_eq!(
            line(&response),
            "{\"type\":\"response\",\"success\":true}\n"
        );
    }

    #[test]
    fn session_state_model_keeps_the_catalog_key_order() {
        let raw = kiana_ai::providers::catalog::get_model("anthropic", "claude-fable-5")
            .expect("catalog entry");
        let model: Model = serde_json::from_value(raw.clone()).expect("model");
        let state = RpcSessionState {
            model: Some(model),
            thinking_level: ThinkingLevel::High,
            is_streaming: false,
            is_compacting: false,
            steering_mode: QueueMode::All,
            follow_up_mode: QueueMode::OneAtATime,
            session_file: None,
            session_id: "s1".into(),
            session_name: None,
            auto_compaction_enabled: true,
            message_count: 2,
            pending_message_count: 0,
        };
        let line = line(&state);
        assert!(
            line.starts_with(
                "{\"model\":{\"id\":\"claude-fable-5\",\"name\":\"Claude Fable 5\",\"api\":\"anthropic-messages\",\"provider\":\"anthropic\",\"baseUrl\":\"https://api.anthropic.com\",\"reasoning\":true,\"input\":[\"text\",\"image\"],\"cost\":{"
            ),
            "unexpected model key order: {line}"
        );
        assert!(line.contains("\"thinkingLevel\":\"high\",\"isStreaming\":false"));
    }

    #[test]
    fn extension_ui_response_presence_semantics() {
        let response: RpcExtensionUIResponse = serde_json::from_value(json!({
            "type": "extension_ui_response",
            "id": "uuid-1",
            "value": null,
            "cancelled": true
        }))
        .expect("response");
        assert_eq!(response.value, Some(Value::Null));
        assert_eq!(response.confirmed, None);
        assert_eq!(response.cancelled, Some(json!(true)));
    }
}
