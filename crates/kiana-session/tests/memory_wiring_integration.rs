//! 第 10 周记忆接线的会话级集成测试。
//!
//! 覆盖：
//! 1. 启用记忆后，load-time `MEMORY.md` 索引进入快照（`memory_prompt_index`）；
//! 2. 每轮召回注入 0/1 条 `<memory-context>` user 消息；
//! 3. 召回消息只参与当前请求，不写入会话文件；
//! 4. 未启用时不会创建 `.kiana/memory`。
#![allow(clippy::too_many_arguments)]

mod common;

use common::*;
use kiana_ai::providers::faux::{FauxResponseStep, faux_assistant_message};
use kiana_config::settings::schema::Settings;
use kiana_memory::{
    MemoryConfig, MemoryEntryDraft, MemoryScope, MemoryService, MemoryStore, MemoryType,
    ScopePaths, WriteBatch, WriteSource,
};
use kiana_session::agent_session::PromptOptions;
use kiana_session::memory_wiring::MemoryWiringOptions;
use serde_json::json;

fn bypass_settings() -> Settings {
    let mut settings = Settings::default();
    settings
        .extra
        .insert("permissions".to_string(), json!({"mode": "bypass"}));
    settings
}

fn seed_memory(root: &std::path::Path) {
    let store = MemoryStore::open(
        ScopePaths::at_root(MemoryScope::Project, root),
        MemoryConfig::default(),
    )
    .expect("open memory store");
    let service = MemoryService::trusted(store);
    service
        .apply(
            WriteBatch::new().add(MemoryEntryDraft::new(
                "deploy-rule",
                "never deploy on fridays",
                MemoryType::User,
                "Rule: never deploy on Fridays.\nWhy: weekend coverage is thin.\nHow to apply: block Friday deploys.",
            )),
            WriteSource::Tool,
        )
        .expect("seed memory");
}

#[tokio::test]
async fn memory_index_and_recall_are_wired_into_the_session() {
    run(async {
        let memory_dir = tempfile::tempdir().expect("memory dir");
        seed_memory(memory_dir.path());

        let harness = create_harness(HarnessOptions {
            settings: Some(bypass_settings()),
            memory: Some(MemoryWiringOptions {
                enabled: true,
                auto_extract: false,
                root: Some(memory_dir.path().to_path_buf()),
                user_scope: false,
                user_root: None,
                session_dir: None,
            }),
            ..Default::default()
        })
        .await;

        assert!(harness.session.memory_enabled());
        assert!(
            harness
                .session
                .get_active_tool_names()
                .iter()
                .any(|name| name == "memory"),
            "memory tool should be active: {:?}",
            harness.session.get_active_tool_names()
        );
        let index = harness
            .session
            .memory_prompt_index()
            .expect("load-time index");
        assert!(index.contains("deploy-rule"), "index: {index}");

        harness.set_responses(vec![FauxResponseStep::Message(faux_assistant_message(
            "acknowledged",
            Default::default(),
        ))]);
        harness
            .session
            .prompt(
                "should we deploy on friday?".to_string(),
                PromptOptions::default(),
            )
            .await
            .expect("prompt");

        let system_prompt = harness.agent.state().system_prompt;
        assert!(
            system_prompt.contains("<memory>"),
            "system prompt missing memory section: {system_prompt}"
        );
        assert!(
            system_prompt.contains("deploy-rule"),
            "system prompt missing memory index: {system_prompt}"
        );

        let user_texts = get_user_texts(&harness);
        let memory_messages: Vec<&String> = user_texts
            .iter()
            .filter(|text| text.contains("<memory-context>"))
            .collect();
        assert_eq!(memory_messages.len(), 1, "user texts: {user_texts:?}");
        assert!(memory_messages[0].contains("deploy-rule"));

        // 召回消息是 per-turn 参考数据，不能写进会话文件。
        let entries = serde_json::to_string(&harness.session_entries()).expect("entries json");
        assert!(
            !entries.contains("memory-context"),
            "memory context leaked into the persisted transcript"
        );

        harness.cleanup();
    })
    .await;
}

#[tokio::test]
async fn memory_is_disabled_by_default_and_creates_no_directory() {
    run(async {
        let harness = create_harness(HarnessOptions::default()).await;
        assert!(!harness.session.memory_enabled());
        assert!(harness.session.memory_prompt_index().is_none());
        assert!(!harness.temp_dir.path().join(".kiana/memory").exists());
        harness.cleanup();
    })
    .await;
}
