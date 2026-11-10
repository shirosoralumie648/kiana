//! `models-entry.test.ts` 的 Rust 移植。
//!
//! TS 的子进程测试验证「轻量 models 入口不加载 TypeBox/目录/SDK」。Rust 是单一 crate，
//! 所有模块在编译期链接，因此那部分不可表达；这里保留其行为断言：
//! `createModels()` 初始为空、`createProvider` 构建的 provider 能完成一次简单请求。

mod models_support;

use std::sync::Arc;

use async_trait::async_trait;
use kiana_ai::event_stream::AssistantMessageEventStream;
use kiana_ai::models::{
    CreateModelsOptions, CreateProviderOptions, ModelsSimpleStreamOptions, ProviderApiImpl,
    ProviderStreams, create_models, create_provider,
};
use kiana_ai::types::{
    AssistantContentBlock, AssistantMessageEvent, Context, DoneReason, Model, SimpleStreamOptions,
    StreamOptions, TranscriptContext,
};

use models_support::{chat, done_message, test_model};

struct EntryStreams;

impl EntryStreams {
    fn respond(model: &Model) -> AssistantMessageEventStream {
        let stream = AssistantMessageEventStream::new();
        let message = done_message(model, "OK");
        let _ = stream.push(AssistantMessageEvent::Done {
            reason: DoneReason::Stop,
            message: Box::new(message.clone()),
        });
        stream.end(message);
        stream
    }
}

#[async_trait]
impl ProviderStreams for EntryStreams {
    fn stream(
        &self,
        model: &Model,
        _context: &TranscriptContext,
        _options: StreamOptions,
    ) -> AssistantMessageEventStream {
        Self::respond(model)
    }
    fn stream_simple(
        &self,
        model: &Model,
        _context: &TranscriptContext,
        _options: SimpleStreamOptions,
    ) -> AssistantMessageEventStream {
        Self::respond(model)
    }
}

#[tokio::test]
async fn runs_a_completion_through_create_models_and_create_provider() {
    let models = create_models(CreateModelsOptions::default());
    assert!(models.get_models(None).is_empty());
    assert!(models.get_providers().is_empty());

    let model = test_model("faux", "model");
    let provider = create_provider(CreateProviderOptions {
        id: "faux".into(),
        auth: models_support::ambient_auth(),
        models: vec![chat(model.clone())],
        api: Some(ProviderApiImpl::Single(Arc::new(EntryStreams))),
        ..Default::default()
    })
    .unwrap();
    models.set_provider(provider);

    let response = models
        .complete_simple(
            &model,
            &Context::default(),
            ModelsSimpleStreamOptions::default(),
        )
        .await;
    assert_eq!(response.stop_reason, kiana_ai::types::StopReason::Stop);
    let text = match &response.content[0] {
        AssistantContentBlock::Text(text) => text.text.clone(),
        other => panic!("expected text content, got {other:?}"),
    };
    assert_eq!(text, "OK");
}
