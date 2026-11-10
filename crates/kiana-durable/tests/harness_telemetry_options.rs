//! 移植自 `packages/durable/test/harness-telemetry-options.test.ts`。
//!
//! TS 用例的核心是“`HarnessOptions.telemetry` 接受一个完整适配器（带可选的 propagation /
//! metrics 能力），缺省则不设”。Rust 里字段类型是 `Option<Arc<dyn TelemetryContext>>`，
//! 传入只有 metrics 的对象在编译期就不成立（原文件的 `@ts-expect-error` 用例），因此这里
//! 只断言运行期可表达的部分。

use std::sync::Arc;

use kiana_durable::harness::types::{
    HarnessOptions, ModelsProvider, RegistryReader, RegistrySnapshot,
};
use kiana_telemetry::{
    InMemoryTelemetryContext, TelemetryContext, noop_telemetry_context_as_context,
};

struct EmptySnapshot;

impl RegistrySnapshot for EmptySnapshot {
    fn installed(&self) -> Vec<Arc<dyn kiana_durable::harness::types::Extension>> {
        Vec::new()
    }
    fn extension(&self, _name: &str) -> Option<Arc<dyn kiana_durable::harness::types::Extension>> {
        None
    }
    fn tools(&self) -> Vec<kiana_durable::harness::types::RegistryTool> {
        Vec::new()
    }
    fn sections(&self) -> Vec<kiana_durable::harness::types::RegistrySection> {
        Vec::new()
    }
    fn tasks(&self) -> Vec<kiana_durable::harness::types::AnyTask> {
        Vec::new()
    }
    fn task(&self, _name: &str) -> Option<kiana_durable::harness::types::AnyTask> {
        None
    }
}

struct EmptyRegistry;

impl RegistryReader for EmptyRegistry {
    fn snapshot(&self) -> Arc<dyn RegistrySnapshot> {
        Arc::new(EmptySnapshot)
    }
    fn subscribe(&self, _listener: Arc<dyn Fn() + Send + Sync>) -> kiana_durable::Unsubscribe {
        Box::new(|| {})
    }
}

fn models() -> ModelsProvider {
    ModelsProvider::Dynamic(Arc::new(|| -> Arc<dyn kiana_ai::models::Models> {
        panic!("the telemetry test never resolves models")
    }))
}

fn options(telemetry: Option<Arc<dyn TelemetryContext>>) -> HarnessOptions {
    HarnessOptions {
        telemetry,
        models: models(),
        identity: None,
        registry: Arc::new(EmptyRegistry),
        settings: None,
        env: None,
        conversation_created: None,
        now: None,
        set_timer: None,
        on_wake: None,
        on_report: None,
        read_only: None,
    }
}

#[test]
fn accepts_one_adapter_with_optional_propagation_and_metrics_capabilities() {
    let adapter: Arc<dyn TelemetryContext> = Arc::new(InMemoryTelemetryContext::new());
    let full = options(Some(Arc::clone(&adapter)));
    let context_only = options(Some(noop_telemetry_context_as_context()));
    let omitted = options(None);

    // `Option<Arc<dyn TelemetryContext>>` 是字段的精确类型（对应 TS 的
    // `telemetry?: TelemetryContext`）。
    let field: &Option<Arc<dyn TelemetryContext>> = &full.telemetry;
    assert!(
        field
            .as_ref()
            .is_some_and(|value| Arc::ptr_eq(value, &adapter))
    );
    assert!(context_only.telemetry.is_some());
    assert!(omitted.telemetry.is_none());

    // capabilities 在 trait 上可选：propagation()/metrics() 都是 `Option`。
    let _ = full
        .telemetry
        .as_ref()
        .and_then(|context| context.propagation());
    let _ = full
        .telemetry
        .as_ref()
        .and_then(|context| context.metrics());

    let filtered = full.telemetry.as_ref().unwrap();
    assert!(Arc::ptr_eq(filtered, &adapter));
}
