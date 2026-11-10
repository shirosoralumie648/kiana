//! 编译期契约：上下文 trait 的成员集合被完整实现钉住。
//!
//! `spec/ca-extensions.md` §8.8 A3 要求 `ExtensionContext` 的 **18 个成员**、以及
//! `ExtensionToolContext`（+2）、`ExtensionCommandContext`（+6）、`ReplacedSessionContext`（+2）
//! 与 TS 一一对应。这里给出一个覆盖全部成员的最小 mock：向任一 trait 增加**必填**成员都会让
//! 本文件无法编译，因此成员集合（而不只是对象安全）被编译器钉住。
//!
//! mock 的方法体全部是 `panic!`/`unreachable!`：本文件只做类型检查，不执行任何方法。行为级
//! 契约（no-op 默认值、对话框返回值）在 `kiana-extensions` 与 `tests/ui_contract.rs`。
//!
//! 成员清单（TS: `types.ts#ExtensionContext`）：
//! `ui`、`mode`、`hasUI`、`cwd`、`sessionManager`、`modelRegistry`、`model`、`scopedModels`、
//! `thinkingLevel`、`isIdle`、`isProjectTrusted`、`signal`、`abort`、`hasPendingMessages`、
//! `shutdown`、`getContextUsage`、`compact`、`getSystemPrompt`（18 个）。

use std::path::Path;

use kiana_agent_core::{AgentTool, AgentToolCallOutcome, ThinkingLevel};
use kiana_ai::types::{JsonValue, Model, UserContent};
use kiana_config::models::registry::ModelRegistry;
use kiana_config::models::resolver::ScopedModel;
use kiana_ext_api::BoxFuture;
use kiana_ext_api::LocalBoxFuture;
use kiana_ext_api::system_prompt::BuildSystemPromptOptions;
use kiana_ext_api::types::{
    CompactOptions, ContextUsage, ExecuteToolOptions, ExtensionCommandContextApi,
    ExtensionContextApi, ExtensionMode, ExtensionToolContextApi, ForkOptions, NavigateTreeOptions,
    NewSessionOptions, NewSessionResult, ReplacedSessionContextApi, SendMessageOptions,
    SendUserMessageOptions, SwitchSessionOptions,
};
use kiana_ext_api::ui::ExtensionUiContext;
use kiana_session_core::session_manager::ReadonlySessionManager;
use tokio_util::sync::CancellationToken;

/// 覆盖全部上下文成员的 mock（只用于编译期检查）。
struct MockContext;

impl ExtensionContextApi for MockContext {
    fn ui(&self) -> &dyn ExtensionUiContext {
        panic!("compile-time contract only")
    }

    fn mode(&self) -> ExtensionMode {
        panic!("compile-time contract only")
    }

    fn has_ui(&self) -> bool {
        panic!("compile-time contract only")
    }

    fn cwd(&self) -> &Path {
        Path::new("/")
    }

    fn session_manager(&self) -> &dyn ReadonlySessionManager {
        panic!("compile-time contract only")
    }

    fn model_registry(&self) -> &ModelRegistry {
        panic!("compile-time contract only")
    }

    fn model(&self) -> Option<Model> {
        panic!("compile-time contract only")
    }

    fn scoped_models(&self) -> Vec<ScopedModel> {
        panic!("compile-time contract only")
    }

    fn thinking_level(&self) -> Option<ThinkingLevel> {
        panic!("compile-time contract only")
    }

    fn is_idle(&self) -> bool {
        panic!("compile-time contract only")
    }

    fn is_project_trusted(&self) -> bool {
        panic!("compile-time contract only")
    }

    fn signal(&self) -> Option<CancellationToken> {
        panic!("compile-time contract only")
    }

    fn abort(&self) {
        panic!("compile-time contract only")
    }

    fn has_pending_messages(&self) -> bool {
        panic!("compile-time contract only")
    }

    fn shutdown(&self) {
        panic!("compile-time contract only")
    }

    fn get_context_usage(&self) -> Option<ContextUsage> {
        panic!("compile-time contract only")
    }

    fn compact(&self, _options: Option<CompactOptions>) {
        panic!("compile-time contract only")
    }

    fn get_system_prompt(&self) -> String {
        panic!("compile-time contract only")
    }
}

impl ExtensionToolContextApi for MockContext {
    fn tools(&self) -> Vec<AgentTool> {
        panic!("compile-time contract only")
    }

    fn execute_tool<'a>(
        &'a self,
        _name: &'a str,
        _args: JsonValue,
        _options: Option<ExecuteToolOptions>,
    ) -> BoxFuture<'a, AgentToolCallOutcome> {
        panic!("compile-time contract only")
    }
}

impl ExtensionCommandContextApi for MockContext {
    fn get_system_prompt_options(&self) -> BuildSystemPromptOptions {
        panic!("compile-time contract only")
    }

    fn wait_for_idle(&self) -> LocalBoxFuture<'static, ()> {
        panic!("compile-time contract only")
    }

    fn new_session(
        &self,
        _options: Option<NewSessionOptions>,
    ) -> LocalBoxFuture<'static, NewSessionResult> {
        panic!("compile-time contract only")
    }

    fn fork<'a>(
        &'a self,
        _entry_id: &'a str,
        _options: Option<ForkOptions>,
    ) -> LocalBoxFuture<'a, NewSessionResult> {
        panic!("compile-time contract only")
    }

    fn navigate_tree<'a>(
        &'a self,
        _target_id: &'a str,
        _options: Option<NavigateTreeOptions>,
    ) -> LocalBoxFuture<'a, NewSessionResult> {
        panic!("compile-time contract only")
    }

    fn switch_session<'a>(
        &'a self,
        _session_path: &'a str,
        _options: Option<SwitchSessionOptions>,
    ) -> LocalBoxFuture<'a, NewSessionResult> {
        panic!("compile-time contract only")
    }

    fn reload(&self) -> LocalBoxFuture<'static, ()> {
        panic!("compile-time contract only")
    }
}

impl ReplacedSessionContextApi for MockContext {
    fn send_message(&self, _message: SendMessageOptions) -> LocalBoxFuture<'static, ()> {
        panic!("compile-time contract only")
    }

    fn send_user_message(
        &self,
        _content: UserContent,
        _options: Option<SendUserMessageOptions>,
    ) -> LocalBoxFuture<'static, ()> {
        panic!("compile-time contract only")
    }
}

#[test]
fn context_traits_are_fully_implementable_with_eighteen_plus_members() {
    fn take_context(_: &dyn ExtensionContextApi) {}
    fn take_tool(_: &dyn ExtensionToolContextApi) {}
    fn take_command(_: &dyn ExtensionCommandContextApi) {}
    fn take_replaced(_: &dyn ReplacedSessionContextApi) {}

    take_context(&MockContext);
    take_tool(&MockContext);
    take_command(&MockContext);
    take_replaced(&MockContext);
}
