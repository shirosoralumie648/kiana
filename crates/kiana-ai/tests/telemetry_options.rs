//! `telemetry-options.test.ts` 的 Rust 移植。
//!
//! `buildBaseOptions`（`api/simple-options.ts`）属于 API 适配器单元；Rust 的
//! [`SimpleStreamOptions`] 直接携带 `telemetry`，因此这里断言类型面与 provider/Models 分发。
//! `registerImagesApiProvider` / 全局 `generateImages`（images registry 单元已合入）与
//! `Models.generateImages` 两条分派路径都覆盖。

mod models_support;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use kiana_ai::auth::types::{
    ApiKeyAuth, ApiKeyAuthInput, AuthError, AuthResult, ModelAuth, ProviderAuth,
};
use kiana_ai::event_stream::AssistantMessageEventStream;
use kiana_ai::models::{
    CreateModelsOptions, CreateProviderOptions, ModelsDeferredCancelOptions,
    ModelsDeferredFetchOptions, ModelsImagesOptions, ModelsSimpleStreamOptions,
    ModelsStreamOptions, ProviderApiImpl, ProviderImages, ProviderStreams, create_models,
    create_provider,
};
use kiana_ai::types::{
    AssistantImages, DeferredFetchOptions, DeferredHandle, ImagesContext, Model,
    SimpleStreamOptions, StreamOptions, TranscriptContext,
};
use kiana_telemetry::TelemetrySpan;

use models_support::{chat, done_message, test_model};

fn recorder() -> Arc<Mutex<Vec<Arc<dyn TelemetrySpan>>>> {
    Arc::new(Mutex::new(Vec::new()))
}

fn noop_telemetry() -> Arc<dyn TelemetrySpan> {
    kiana_telemetry::noop_telemetry_context()
}

struct RecordingStreams {
    observed: Arc<Mutex<Vec<Arc<dyn TelemetrySpan>>>>,
}

impl RecordingStreams {
    fn record(&self, telemetry: Option<Arc<dyn TelemetrySpan>>) {
        if let Some(telemetry) = telemetry {
            self.observed
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(telemetry);
        }
    }
    fn response(&self, model: &Model) -> AssistantMessageEventStream {
        let stream = AssistantMessageEventStream::new();
        let message = done_message(model, "ok");
        let _ = stream.push(kiana_ai::types::AssistantMessageEvent::Done {
            reason: kiana_ai::types::DoneReason::Stop,
            message: Box::new(message.clone()),
        });
        stream.end(message);
        stream
    }
}

#[async_trait]
impl ProviderStreams for RecordingStreams {
    fn stream(
        &self,
        model: &Model,
        _context: &TranscriptContext,
        options: StreamOptions,
    ) -> AssistantMessageEventStream {
        self.record(options.telemetry.clone());
        self.response(model)
    }
    fn stream_simple(
        &self,
        model: &Model,
        _context: &TranscriptContext,
        options: SimpleStreamOptions,
    ) -> AssistantMessageEventStream {
        self.record(options.base.telemetry.clone());
        self.response(model)
    }
    fn has_fetch_deferred(&self) -> bool {
        true
    }
    fn fetch_deferred(
        &self,
        model: &Model,
        _handle: &DeferredHandle,
        options: DeferredFetchOptions,
    ) -> Option<AssistantMessageEventStream> {
        self.record(options.base.telemetry.clone());
        Some(self.response(model))
    }
    fn has_cancel_deferred(&self) -> bool {
        true
    }
    async fn cancel_deferred(
        &self,
        _model: &Model,
        _handle: &DeferredHandle,
        options: kiana_ai::types::DeferredCancelOptions,
    ) -> Option<Result<(), kiana_ai::error::AiError>> {
        self.record(options.telemetry.clone());
        Some(Ok(()))
    }
}

struct RecordingImages {
    observed: Arc<Mutex<Vec<Arc<dyn TelemetrySpan>>>>,
}

#[async_trait]
impl ProviderImages for RecordingImages {
    async fn generate_images(
        &self,
        model: &Model,
        _context: &ImagesContext,
        options: Option<&StreamOptions>,
    ) -> AssistantImages {
        self.record(options.and_then(|options| options.telemetry.clone()));
        AssistantImages {
            api: model.api.clone(),
            provider: model.provider.clone(),
            model: model.id.clone(),
            output: vec![],
            response_id: None,
            usage: None,
            stop_reason: kiana_ai::types::ImagesStopReason::Stop,
            error_message: None,
            timestamp: 0,
        }
    }
}

impl RecordingImages {
    fn record(&self, telemetry: Option<Arc<dyn TelemetrySpan>>) {
        if let Some(telemetry) = telemetry {
            self.observed
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(telemetry);
        }
    }
}

struct TestAuth;

#[async_trait]
impl ApiKeyAuth for TestAuth {
    fn name(&self) -> &str {
        "Test"
    }
    async fn resolve(&self, _input: ApiKeyAuthInput<'_>) -> Result<Option<AuthResult>, AuthError> {
        Ok(Some(AuthResult::new(ModelAuth::default())))
    }
}

fn auth() -> ProviderAuth {
    ProviderAuth {
        api_key: Some(Arc::new(TestAuth)),
        oauth: None,
    }
}

#[test]
fn is_inherited_by_every_request_option_surface() {
    let telemetry = noop_telemetry();
    let options = StreamOptions {
        telemetry: Some(Arc::clone(&telemetry)),
        ..Default::default()
    };
    assert!(Arc::ptr_eq(options.telemetry.as_ref().unwrap(), &telemetry));
    let simple = SimpleStreamOptions {
        base: StreamOptions {
            telemetry: Some(Arc::clone(&telemetry)),
            ..Default::default()
        },
        ..Default::default()
    };
    assert!(Arc::ptr_eq(
        simple.base.telemetry.as_ref().unwrap(),
        &telemetry
    ));
    // TS 的编译期断言 `telemetryContext` 已移除（无兼容别名）；Rust 结构体没有该字段。
}

#[tokio::test]
async fn survives_provider_and_models_stream_deferred_dispatch() {
    let observed = recorder();
    let model = test_model("telemetry-provider", "model");
    let provider = create_provider(CreateProviderOptions {
        id: model.provider.clone(),
        auth: auth(),
        models: vec![chat(model.clone())],
        api: Some(ProviderApiImpl::Single(Arc::new(RecordingStreams {
            observed: Arc::clone(&observed),
        }))),
        ..Default::default()
    })
    .unwrap();
    let handle = DeferredHandle {
        provider: model.provider.clone(),
        model_id: model.id.clone(),
        api: model.api.clone(),
        id: "response".into(),
        ..Default::default()
    };
    let telemetry = noop_telemetry();
    let context = TranscriptContext::from_normalized(kiana_ai::types::Context::default());

    provider
        .stream(
            &model,
            &context,
            StreamOptions {
                telemetry: Some(Arc::clone(&telemetry)),
                ..Default::default()
            },
        )
        .result()
        .await
        .unwrap();
    provider
        .stream_simple(
            &model,
            &context,
            SimpleStreamOptions {
                base: StreamOptions {
                    telemetry: Some(Arc::clone(&telemetry)),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .result()
        .await
        .unwrap();
    provider
        .fetch_deferred(
            &model,
            &handle,
            DeferredFetchOptions {
                base: StreamOptions {
                    telemetry: Some(Arc::clone(&telemetry)),
                    ..Default::default()
                },
                wait: None,
            },
        )
        .unwrap()
        .result()
        .await
        .unwrap();
    provider
        .cancel_deferred(
            &model,
            &handle,
            StreamOptions {
                telemetry: Some(Arc::clone(&telemetry)),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let models = create_models(CreateModelsOptions::default());
    models.set_provider(provider);
    models
        .stream(
            &model,
            &kiana_ai::types::Context::default(),
            ModelsStreamOptions {
                base: StreamOptions {
                    telemetry: Some(Arc::clone(&telemetry)),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .result()
        .await
        .unwrap();
    models
        .stream_simple(
            &model,
            &kiana_ai::types::Context::default(),
            ModelsSimpleStreamOptions {
                base: SimpleStreamOptions {
                    base: StreamOptions {
                        telemetry: Some(Arc::clone(&telemetry)),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .result()
        .await
        .unwrap();
    models
        .fetch_deferred(
            &model,
            &handle,
            ModelsDeferredFetchOptions {
                base: DeferredFetchOptions {
                    base: StreamOptions {
                        telemetry: Some(Arc::clone(&telemetry)),
                        ..Default::default()
                    },
                    wait: None,
                },
                ..Default::default()
            },
        )
        .await;
    models
        .cancel_deferred(
            &model,
            &handle,
            ModelsDeferredCancelOptions {
                base: StreamOptions {
                    telemetry: Some(Arc::clone(&telemetry)),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let observed = observed.lock().unwrap_or_else(|e| e.into_inner());
    assert_eq!(observed.len(), 8);
    assert!(observed.iter().all(|value| Arc::ptr_eq(value, &telemetry)));
}

#[tokio::test]
async fn survives_models_image_dispatch() {
    let observed = recorder();
    let mut image_model = test_model("telemetry-image-provider", "image-model");
    image_model.api = "telemetry-test-images".into();
    image_model.model_type = Some(kiana_ai::types::ModelType::Image);
    image_model.output = Some(vec!["image".into()]);
    let mut images = indexmap::IndexMap::new();
    images.insert(
        image_model.api.clone(),
        Arc::new(RecordingImages {
            observed: Arc::clone(&observed),
        }) as Arc<dyn ProviderImages>,
    );
    let models = create_models(CreateModelsOptions::default());
    models.set_provider(
        create_provider(CreateProviderOptions {
            id: image_model.provider.clone(),
            auth: auth(),
            models: vec![kiana_ai::types::AnyModel::Image(image_model.clone())],
            images: Some(images),
            classifiers: None,
            ..Default::default()
        })
        .unwrap(),
    );
    let telemetry = noop_telemetry();
    let result = models
        .generate_images(
            &image_model,
            &ImagesContext { input: vec![] },
            ModelsImagesOptions {
                base: StreamOptions {
                    telemetry: Some(Arc::clone(&telemetry)),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .await;
    assert_eq!(result.stop_reason, kiana_ai::types::ImagesStopReason::Stop);
    let observed = observed.lock().unwrap_or_else(|e| e.into_inner());
    assert_eq!(observed.len(), 1);
    assert!(Arc::ptr_eq(&observed[0], &telemetry));
}

#[tokio::test]
async fn survives_direct_image_dispatch() {
    let observed = recorder();
    let mut image_model = test_model("telemetry-image-provider", "image-model");
    image_model.api = "telemetry-direct-images".into();
    image_model.model_type = Some(kiana_ai::types::ModelType::Image);
    image_model.output = Some(vec!["image".into()]);
    kiana_ai::image_models::register_images_api_provider(
        kiana_ai::image_models::ImagesApiProvider::from_provider(
            image_model.api.clone(),
            Arc::new(RecordingImages {
                observed: Arc::clone(&observed),
            }) as Arc<dyn ProviderImages>,
        ),
        None,
    );

    let telemetry = noop_telemetry();
    let result = kiana_ai::image_models::generate_images(
        &image_model,
        &ImagesContext { input: vec![] },
        Some(&StreamOptions {
            telemetry: Some(Arc::clone(&telemetry)),
            ..Default::default()
        }),
    )
    .await
    .expect("global generateImages dispatches through the registry");

    assert_eq!(result.stop_reason, kiana_ai::types::ImagesStopReason::Stop);
    let observed = observed.lock().unwrap_or_else(|e| e.into_inner());
    assert_eq!(observed.len(), 1);
    assert!(Arc::ptr_eq(&observed[0], &telemetry));
}
