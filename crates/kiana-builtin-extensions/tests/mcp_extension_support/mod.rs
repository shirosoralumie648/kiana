//! `mcp/index.ts` 的测试设施：记录注册行为的 [`FakeExtensionApi`]。
//!
//! 照抄 `crates/kiana-ext-api/tests/public_surface.rs` 的 `MockExtensionApi`（33 个成员），
//! 并记录 `register_tool`/`register_command`/`register_tool_renderer`/`on` 的调用，
//! 让测试能直接拿到扩展注册的处理器与 tool renderer resolver。

#![allow(dead_code)]

use std::cell::RefCell;
use std::sync::Arc;

use kiana_agent_core::ThinkingLevel;
use kiana_ai::types::{JsonValue, Model, UserContent};
use kiana_config::settings::Settings;
use kiana_core::event_bus::{EventBus, Unsubscribe};
use kiana_core::mcp_servers::{McpServerConfig, RegisteredMcpServer};
use kiana_core::slash_commands::SlashCommandInfo;
use kiana_ext_api::events::{ExtensionEventKind, ExtensionHandler};
use kiana_ext_api::types::{
    EntryRenderer, FlagValue, MarkdownTransformer, MessageRenderer, ProviderRegistration,
    RegisterCommandOptions, RegisterFlagOptions, RegisterShortcutOptions, SendMessageOptions,
    SendUserMessageOptions,
};
use kiana_ext_api::{
    ExecOptions, ExecResult, ExtensionApi, LocalBoxFuture, ToolDefinition, ToolInfo,
    ToolRendererResolver,
};

/// 记录注册内容的假 `ExtensionApi`。
#[derive(Default)]
pub struct FakeExtensionApi {
    /// `register_tool` 收到的定义（按注册顺序）。
    pub tools: RefCell<Vec<ToolDefinition>>,
    /// `register_command` 收到的 `(name, options)`。
    pub commands: RefCell<Vec<(String, RegisterCommandOptions)>>,
    /// `register_tool_renderer` 收到的 resolver。
    pub tool_renderers: RefCell<Vec<ToolRendererResolver>>,
    /// `on` 订阅的 `(kind, handler)`。
    pub handlers: RefCell<Vec<(ExtensionEventKind, ExtensionHandler)>>,
    /// 当前 active 工具。
    pub active_tools: RefCell<Vec<String>>,
    /// `get_all_tools` 的返回值。
    pub all_tools: RefCell<Vec<ToolInfo>>,
    /// `get_mcp_servers` 的返回值。
    pub mcp_servers: RefCell<Vec<RegisteredMcpServer>>,
    /// `register_provider` 的调用次数。
    pub providers: RefCell<usize>,
    /// `get_settings` 的返回值。
    pub settings: Settings,
}

impl FakeExtensionApi {
    /// 空记录器。
    pub fn new() -> Self {
        FakeExtensionApi::default()
    }

    /// 注册的命令名。
    pub fn command_names(&self) -> Vec<String> {
        self.commands
            .borrow()
            .iter()
            .map(|(name, _)| name.clone())
            .collect()
    }

    /// 注册的工具名。
    pub fn tool_names(&self) -> Vec<String> {
        self.tools
            .borrow()
            .iter()
            .map(|tool| tool.name.clone())
            .collect()
    }
}

/// 把拿到的 `ExtensionHandler` 按事件名找回来（测试用）。
impl FakeExtensionApi {
    pub fn handler(&self, kind: ExtensionEventKind) -> Option<ExtensionHandler> {
        self.handlers
            .borrow()
            .iter()
            .rev()
            .find(|(registered, _)| *registered == kind)
            .map(|(_, handler)| handler.clone())
    }
}

impl ExtensionApi for FakeExtensionApi {
    fn on(&self, kind: ExtensionEventKind, handler: ExtensionHandler) -> Unsubscribe {
        self.handlers.borrow_mut().push((kind, handler));
        Box::new(|| {})
    }
    fn register_tool(&self, tool: ToolDefinition) {
        self.tools.borrow_mut().push(tool);
    }
    fn register_command(&self, name: &str, options: RegisterCommandOptions) {
        self.commands.borrow_mut().push((name.to_string(), options));
    }
    fn register_shortcut(&self, _shortcut: &str, _options: RegisterShortcutOptions) {}
    fn register_flag(&self, _name: &str, _options: RegisterFlagOptions) {}
    fn get_flag(&self, _name: &str) -> Option<FlagValue> {
        None
    }
    fn register_message_renderer(&self, _custom_type: &str, _renderer: MessageRenderer) {}
    fn register_markdown_transformer(&self, _transformer: MarkdownTransformer) {}
    fn register_entry_renderer(&self, _custom_type: &str, _renderer: EntryRenderer) {}
    fn register_tool_renderer(&self, resolver: ToolRendererResolver) {
        self.tool_renderers.borrow_mut().push(resolver);
    }
    fn send_message(&self, _options: SendMessageOptions) {}
    fn send_user_message(&self, _content: UserContent, _options: Option<SendUserMessageOptions>) {}
    fn append_entry(&self, _custom_type: &str, _data: Option<JsonValue>) {}
    fn set_session_name(&self, _name: &str) {}
    fn get_session_name(&self) -> Option<String> {
        None
    }
    fn set_label(&self, _entry_id: &str, _label: Option<&str>) {}
    fn exec<'a>(
        &'a self,
        _command: &'a str,
        _args: &'a [String],
        _options: Option<ExecOptions>,
    ) -> LocalBoxFuture<'a, ExecResult> {
        Box::pin(async {
            ExecResult {
                stdout: String::new(),
                stderr: String::new(),
                code: 0,
                killed: false,
            }
        })
    }
    fn get_active_tools(&self) -> Vec<String> {
        self.active_tools.borrow().clone()
    }
    fn get_all_tools(&self) -> Vec<ToolInfo> {
        self.all_tools.borrow().clone()
    }
    fn get_settings(&self) -> Settings {
        self.settings.clone()
    }
    fn set_active_tools(&self, tool_names: Vec<String>) {
        *self.active_tools.borrow_mut() = tool_names;
    }
    fn get_commands(&self) -> Vec<SlashCommandInfo> {
        Vec::new()
    }
    fn set_model(&self, _model: Model) -> LocalBoxFuture<'static, bool> {
        Box::pin(async { false })
    }
    fn get_thinking_level(&self) -> ThinkingLevel {
        ThinkingLevel::default()
    }
    fn set_thinking_level(&self, _level: ThinkingLevel) {}
    fn register_provider(&self, _registration: ProviderRegistration) {
        *self.providers.borrow_mut() += 1;
    }
    fn unregister_provider(&self, _name: &str) {}
    fn register_mcp_server(&self, _name: &str, _config: McpServerConfig) {}
    fn unregister_mcp_server(&self, _name: &str) {}
    fn get_mcp_servers(&self) -> Vec<RegisteredMcpServer> {
        self.mcp_servers.borrow().clone()
    }
    fn register_virtual_model(&self, _model: kiana_ext_api::ExtensionVirtualModel) {}
    fn unregister_virtual_model(&self, _provider: &str, _id: &str) {}
    fn events(&self) -> Arc<dyn EventBus> {
        kiana_core::event_bus::create_event_bus()
    }
}
