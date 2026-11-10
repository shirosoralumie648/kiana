//! `virtual_models` 适配层：断言 kiana-config 只是 `kiana-session-core::virtual_models` 的再导出
//! （同一常量、同一类型、同一实现函数），没有重复状态或第二份实现。

use kiana_ai::types::{Model, ModelInputModality, ModelThinkingLevel};
use kiana_config::models::virtual_models::{
    THINKING_LEVELS, VIRTUAL_MODEL_API, VIRTUAL_MODEL_STATE_ENTRY, VirtualModelDefinition,
    VirtualModelStateData, create_virtual_model, find_latest_response, get_virtual_model_state,
    is_virtual_model, unrouted_stream, with_virtual_models,
};
use kiana_session_core::virtual_models as core;

#[test]
fn constants_are_the_same_values() {
    assert_eq!(VIRTUAL_MODEL_API, "kiana-virtual");
    assert_eq!(VIRTUAL_MODEL_STATE_ENTRY, "kiana.virtual-model-state");
    assert_eq!(VIRTUAL_MODEL_API, core::VIRTUAL_MODEL_API);
    assert_eq!(VIRTUAL_MODEL_STATE_ENTRY, core::VIRTUAL_MODEL_STATE_ENTRY);
    assert_eq!(THINKING_LEVELS, core::THINKING_LEVELS);
}

#[test]
fn functions_are_the_same_items() {
    let create: fn(&VirtualModelDefinition) -> Model = create_virtual_model;
    let create_core: fn(&core::VirtualModelDefinition) -> Model = core::create_virtual_model;
    let _ = (create, create_core);

    let is_virtual: fn(&Model) -> bool = is_virtual_model;
    let _ = is_virtual;
    let latest: fn(&[kiana_agent_core::AgentMessage]) -> Option<kiana_ai::types::AssistantMessage> =
        find_latest_response;
    let _ = latest;
    let state: fn(
        &[kiana_session_core::session_manager::SessionEntry],
        &str,
        &str,
    ) -> Option<kiana_ai::types::JsonValue> = get_virtual_model_state;
    let _ = state;
    let unroutable: fn(&Model) -> kiana_ai::event_stream::AssistantMessageEventStream =
        unrouted_stream;
    let _ = unroutable;
}

#[test]
fn create_virtual_model_matches_the_core_implementation() {
    let definition = VirtualModelDefinition {
        provider: "demo".to_string(),
        id: "virtual".to_string(),
        name: "Virtual".to_string(),
        thinking_levels: Some(vec![ModelThinkingLevel::Off, ModelThinkingLevel::Low]),
        context_window: Some(1000),
        max_tokens: Some(100),
        input: Some(vec![ModelInputModality::Text, ModelInputModality::Image]),
        route: None,
    };
    let model = create_virtual_model(&definition);
    let core_model = core::create_virtual_model(&core::VirtualModelDefinition {
        provider: definition.provider.clone(),
        id: definition.id.clone(),
        name: definition.name.clone(),
        thinking_levels: definition.thinking_levels.clone(),
        context_window: definition.context_window,
        max_tokens: definition.max_tokens,
        input: definition.input.clone(),
        route: None,
    });
    assert_eq!(model, core_model);
    assert_eq!(model.api, VIRTUAL_MODEL_API);
    assert!(is_virtual_model(&model));
    assert_eq!(
        model
            .thinking_level_map
            .as_ref()
            .unwrap()
            .get(&ModelThinkingLevel::Low),
        Some(&Some("low".to_string()))
    );
    assert_eq!(
        model
            .thinking_level_map
            .as_ref()
            .unwrap()
            .get(&ModelThinkingLevel::High),
        Some(&None)
    );
}

#[test]
fn with_virtual_models_wraps_the_core_provider() {
    let definition = VirtualModelDefinition {
        provider: "demo".to_string(),
        id: "virtual".to_string(),
        name: "Virtual".to_string(),
        thinking_levels: None,
        context_window: None,
        max_tokens: None,
        input: None,
        route: None,
    };
    let virtual_model = create_virtual_model(&definition);
    let provider = with_virtual_models("demo", None, vec![virtual_model.clone()]);
    assert_eq!(provider.id(), "demo");
    assert_eq!(provider.get_models(), vec![virtual_model]);
    assert!(provider.auth().api_key.is_some());
}

#[test]
fn state_data_and_empty_queries() {
    let data = VirtualModelStateData {
        provider: "demo".to_string(),
        model_id: "virtual".to_string(),
        state: Some(serde_json::json!({"x": 1})),
    };
    let json = serde_json::to_value(&data).unwrap();
    assert_eq!(
        json,
        serde_json::json!({"provider": "demo", "modelId": "virtual", "state": {"x": 1}})
    );
    assert!(find_latest_response(&[]).is_none());
    assert!(get_virtual_model_state(&[], "demo", "virtual").is_none());
}
