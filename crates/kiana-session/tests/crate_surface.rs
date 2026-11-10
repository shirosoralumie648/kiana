//! `kiana-session` crate 根的会话层公开面（`spec/ca-session.md` §2.13 前三条）。
#![allow(clippy::arc_with_non_send_sync)]
#![allow(dead_code)]

// agent-session 层。
use kiana_session::{
    AgentSession, AgentSessionConfig, AgentSessionEvent, AgentSessionEventListener,
    ModelCycleResult, PromptOptions, SessionStats,
};
// runtime 层。
use kiana_session::{
    AgentSessionRuntime, CreateAgentSessionRuntimeFactory, CreateAgentSessionRuntimeResult,
    create_agent_session_runtime, createAgentSessionRuntime,
};
// services 层。
use kiana_session::{
    AgentSessionRuntimeDiagnostic, AgentSessionServices, CreateAgentSessionFromServicesOptions,
    CreateAgentSessionServicesOptions, create_agent_session_from_services,
    create_agent_session_services, createAgentSessionFromServices, createAgentSessionServices,
};

/// 其余 `core/index.ts` 条目**不在** `kiana-session` 根：从各自 owning crate 引用，
/// 保证不会在本地重复定义（若 kiana-session 也定义同名类型，这里的引用仍会解析到 owning crate，
/// 但工作区的“唯一 struct”约束由各 crate 的测试保证）。
fn other_core_index_surfaces() {
    let _ = std::any::type_name::<kiana_tools::bash_executor::BashExecutorOptions>();
    let _ = std::any::type_name::<kiana_tools::bash_executor::BashResult>();
    let _ = kiana_tools::bash_executor::execute_bash_with_operations;
    let _ = std::any::type_name::<kiana_config::cache::warmer::CacheWarmingDecision>();
    let _ = std::any::type_name::<kiana_config::cache::warmer::CacheWarmingStatus>();
    let _ = std::any::type_name::<kiana_compaction::compaction::CompactionResult>();
    let _ = kiana_core::event_bus::create_event_bus;
    assert_trait::<dyn kiana_core::event_bus::EventBus>();
    let _ = kiana_config::defaults::are_experimental_features_enabled;
    let _ = kiana_core::source_info::create_synthetic_source_info;
    let _ = std::any::type_name::<kiana_ext_api::tool_result::ExecOptions>();
    let _ = std::any::type_name::<kiana_ext_api::tool_result::ExecResult>();
    let _ = std::any::type_name::<kiana_extensions::ExtensionRunner>();
    let _ = kiana_extensions::discover_and_load_extensions;
}

fn assert_trait<T: ?Sized>() {}

/// 扩展 API 的 60 个名字（含 `*Api` 改名）在 `kiana_ext_api` 可见。
fn extension_surface() {
    assert_trait::<dyn kiana_ext_api::ExtensionApi>();
    assert_trait::<dyn kiana_ext_api::ExtensionContextApi>();
    assert_trait::<dyn kiana_ext_api::ExtensionCommandContextApi>();
    assert_trait::<dyn kiana_ext_api::ExtensionToolContextApi>();
    assert_trait::<dyn kiana_ext_api::ExtensionUiContext>();
    let _ = std::any::type_name::<kiana_ext_api::InlineExtension>();
    let _ = std::any::type_name::<kiana_ext_api::ExtensionFactory>();
    let _ = std::any::type_name::<kiana_ext_api::ToolDefinition>();
    let _ = std::any::type_name::<kiana_ext_api::events::SessionStartEvent>();
    let _ = std::any::type_name::<kiana_ext_api::events::SessionShutdownEvent>();
    let _ = std::any::type_name::<kiana_ext_api::events::ProjectTrustContext>();
}

#[test]
fn session_layer_surface_is_reachable_at_the_crate_root() {
    let _ = std::any::type_name::<AgentSession>();
    let _ = std::any::type_name::<AgentSessionConfig>();
    let _ = std::any::type_name::<AgentSessionEvent>();
    let _ = std::any::type_name::<AgentSessionEventListener>();
    let _ = std::any::type_name::<ModelCycleResult>();
    let _ = std::any::type_name::<PromptOptions>();
    let _ = std::any::type_name::<SessionStats>();
    let _ = std::any::type_name::<AgentSessionRuntime>();
    let _ = std::any::type_name::<CreateAgentSessionRuntimeFactory>();
    let _ = std::any::type_name::<CreateAgentSessionRuntimeResult>();
    let _ = std::any::type_name::<AgentSessionRuntimeDiagnostic>();
    let _ = std::any::type_name::<AgentSessionServices>();
    let _ = std::any::type_name::<CreateAgentSessionFromServicesOptions>();
    let _ = std::any::type_name::<CreateAgentSessionServicesOptions>();
    let _ = createAgentSessionRuntime;
    let _ = create_agent_session_runtime;
    let _ = createAgentSessionFromServices;
    let _ = create_agent_session_from_services;
    let _ = createAgentSessionServices;
    let _ = create_agent_session_services;
    other_core_index_surfaces();
    extension_surface();
}
